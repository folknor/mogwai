// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `MogwaiDataClient`: the `DataClient` half of the adapter. Owns the
//! subscription table, the websocket reader, the live bar aggregator, and
//! the request handlers that page a window of history off that same socket.
//! There is one transport and no choice to make: the polling carrier and its
//! timestamp cursor are retired, and history now travels as `QueryHistory`
//! frames answered by paged `HistoryPage` replies. Plumbing shared with the
//! execution half (the havoc
//! dispatch pipeline, the instrument cache, clock/url glue) lives in
//! `super::shared`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use mogwai_protocol::{InstrumentDef, SimClock, Symbol, TradeTick, VenueMessage};
use nautilus_common::{
    clients::{DataClient, execute_bar_subscriptions, execute_instrument_subscriptions},
    live::{get_data_event_sender, get_runtime, sender::EventSender},
    messages::{
        DataEvent,
        data::{
            BarsResponse, DataRequestFailed, DataResponse, InstrumentResponse, InstrumentsResponse,
            QuotesResponse, RequestBars, RequestInstrument, RequestInstruments, RequestQuotes,
            RequestTrades, SubscribeBars, SubscribeFundingRates, SubscribeInstrument,
            SubscribeInstruments, SubscribeQuotes, SubscribeTrades, TradesResponse,
            UnsubscribeBars, UnsubscribeFundingRates, UnsubscribeInstrument,
            UnsubscribeInstruments, UnsubscribeQuotes, UnsubscribeTrades,
        },
    },
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarType, Data},
    identifiers::{ClientId, InstrumentId, Venue},
};
use nautilus_network::http::HttpClient;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::{
    MOGWAI_VENUE, MogwaiDataClientConfig,
    bars::{BarAggregator, aggregate_bars},
    client::shared::{
        HavocDelivery, HavocFilter, abort_tasks, cache_instruments, conn_havoc, date_to_unix_nanos,
        emit_seeded_instruments, enqueue_havoc, ensure_instrument, ensure_on_river,
        fetch_clock_or_identity, fetch_instruments, flush_havoc_into_pump, inbound_havoc,
        instrument_any_or_warn, instrument_def, lock_recover, now_unix_nanos, request_timeout_secs,
        run_identity_check, schedule_reorder_flush, seed_instruments, spawn_latency_pump,
        symbol_from_instrument, track_task, wait_connected, warn_missing_instrument_once,
    },
    convert,
    lifecycle::{HttpQuota, WsConnectionConfig, run_ws_connection},
};

#[derive(Debug)]
pub struct MogwaiDataClient {
    client_id: ClientId,
    config: MogwaiDataClientConfig,
    connected: Arc<AtomicBool>,
    connected_notify: Arc<tokio::sync::Notify>,
    sink: Option<EventSender<DataEvent>>,
    http: HttpClient,
    http_quota: HttpQuota,
    sim: SimClock,
    /// Earliest `ts_event` the venue can serve. `None` means the clock fetch
    /// failed; `Some(0)` is the real fixed tape floor.
    data_origin_ns: Option<u64>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    missing_instrument_warnings: Arc<Mutex<std::collections::HashSet<String>>>,
    subs: Arc<Mutex<HashMap<Symbol, SubState>>>,
    quote_delivery: Arc<Mutex<()>>,
    bars: Arc<Mutex<HashMap<BarType, BarSubState>>>,
    /// Handles for every task this client spawns (the WS reader and each
    /// short-lived `request_*`/`subscribe_instrument*` fetch). Shared
    /// behind an `Arc<Mutex<..>>` so the `&self` request handlers can record
    /// their handle too, not just the `&mut self` connect path; `stop()` aborts
    /// the lot so a fetch spawned just before disconnect cannot keep issuing
    /// HTTP requests (and racing the HttpQuota) or send into a dropped sink
    /// after the client stopped.
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
    /// Outbound command channel for this leg.
    ///
    /// The data socket used to send nothing at all - its command receiver was
    /// `Infallible` - because subscriptions are satisfied locally and the venue
    /// pushes the tape unbidden. History changed that: a page is a correlated
    /// request-response, and it is carried here rather than over HTTP because
    /// this connection is the only thing that knows which river this client is
    /// reading. A symbol does not: after the river fork one label names several,
    /// so an HTTP poll would backfill whichever the label resolved to.
    ws_cmd: Option<UnboundedSender<mogwai_protocol::Command>>,
    /// In-flight history waiters, correlation id to reply sender.
    pending_history: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<HistoryReply>>>>,
    /// Monotonic source of correlation ids for this client's own requests.
    next_request: Arc<std::sync::atomic::AtomicU64>,
    /// Every nautilus data request whose answer a spawned task still owes.
    ///
    /// The id in this set is the right to resolve the request, and exactly one
    /// side takes it: the task, through `resolve_request`, or `stop()` and
    /// `connect()`, through `fail_outstanding_requests`, which answer it as
    /// failed because they abort the task that would have. Both remove the id
    /// and send under this one lock, so the loser finds nothing and sends
    /// nothing, and a teardown that returns has made its send attempt for every
    /// request it found. That holds only for unique outstanding request ids,
    /// which nautilus's engine also assumes, since it keys its own inflight
    /// table by them.
    outstanding_requests: Arc<Mutex<std::collections::HashSet<nautilus_core::UUID4>>>,
}

/// What the reader hands back to a waiting history request: a page, or the
/// venue's correlated refusal.
///
/// A refusal is carried rather than turned into an empty page, because an empty
/// page and a quiet market are indistinguishable to a consumer folding bars -
/// which is the whole reason the venue refuses explicitly.
#[derive(Debug)]
enum HistoryReply {
    Page {
        rows: Vec<mogwai_protocol::HistoryRow>,
        continuation: Option<String>,
        complete: bool,
    },
    Rejected {
        reason: String,
    },
}

impl MogwaiDataClient {
    /// Creates a new disconnected mogwai data client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(client_id: ClientId, config: MogwaiDataClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let http = HttpClient::builder()
            .timeout_secs(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS)
            .build()
            .context("create HTTP client")?;
        Ok(Self {
            client_id,
            http_quota: HttpQuota::from_conn(&conn_havoc(&config.havoc), SimClock::identity()),
            config,
            connected: Arc::new(AtomicBool::new(false)),
            connected_notify: Arc::new(tokio::sync::Notify::new()),
            sink: None,
            http,
            sim: SimClock::identity(),
            data_origin_ns: None,
            instruments: Arc::new(Mutex::new(HashMap::new())),
            missing_instrument_warnings: Arc::new(Mutex::new(std::collections::HashSet::new())),
            subs: Arc::new(Mutex::new(HashMap::new())),
            quote_delivery: Arc::new(Mutex::new(())),
            bars: Arc::new(Mutex::new(HashMap::new())),
            task_handles: Arc::new(Mutex::new(Vec::new())),
            ws_cmd: None,
            pending_history: Arc::new(Mutex::new(HashMap::new())),
            next_request: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            outstanding_requests: Arc::new(Mutex::new(std::collections::HashSet::new())),
        })
    }

    /// Claim the right to answer `request_id` for a task about to be spawned.
    /// Called after every synchronous precondition has passed, so a request
    /// refused with `Err` (which the engine answers itself) is never claimed.
    fn register_request(&self, request_id: nautilus_core::UUID4) {
        lock_recover(&self.outstanding_requests, "outstanding requests").insert(request_id);
    }

    /// Answer every request a spawned task still owes, as failed, because the
    /// caller is about to abort those tasks or already has. Without this a
    /// request in flight at `stop()` or a reconnect was never answered at all:
    /// its task died mid-page and nautilus kept the request pending until the
    /// consumer's own timeout. No payload is carried - nothing was retrieved
    /// that could be trusted as complete.
    fn fail_outstanding_requests(&self, reason: &str) {
        let mut outstanding = lock_recover(&self.outstanding_requests, "outstanding requests");
        let Ok(sink) = self.sink() else {
            // No sink means no request could have been registered: every
            // `request_*` takes the sink before it registers.
            outstanding.clear();
            return;
        };
        for request_id in outstanding.drain() {
            tracing::error!(%request_id, reason, "data request failed");
            drop(sink.send(DataEvent::RequestFailed(DataRequestFailed {
                request_id,
                reason: reason.to_owned(),
                response: None,
            })));
        }
    }

    /// Retires the current transport generation's connectivity flag by
    /// replacing the shared cell outright, not by storing `false` into it.
    ///
    /// `abort_tasks` is not synchronous: cancellation is delivered at the
    /// aborted task's next await point, so a reader caught between
    /// `connect_async(..).await` returning and its first select can still run
    /// `connected.store(true)` after the caller has stored `false`. That
    /// leaves a dead client reporting itself connected, and a subsequent
    /// `wait_connected` returning success for a socket that never opened.
    /// Swapping the `Arc` means the retired reader writes to a cell nobody
    /// reads: the drop timing of the old generation stops being a
    /// correctness property.
    fn retire_connected_flag(&mut self) {
        self.connected = Arc::new(AtomicBool::new(false));
        self.connected_notify = Arc::new(tokio::sync::Notify::new());
    }

    fn subscribe_symbol(&self, symbol: Symbol, kind: SubKind) -> anyhow::Result<()> {
        ensure_bound_symbol(self.config.symbol.as_deref(), &symbol, "subscription")?;
        // The subscription is satisfied entirely locally. Nautilus still calls
        // subscribe/unsubscribe and this client must still implement them, but
        // the venue serves one run's one tape and pushes it unbidden, so there
        // is no frame to send: this table only gates which arriving ticks are
        // forwarded to the message bus.
        //
        // The seeded instrument set is never an admission list. The venue resolves
        // any wire-legal symbol and registers it on bind, so a symbol absent
        // from the seed is routinely one this run will serve; refusing on the
        // seed refused exactly the passengers piece 13 exists to support. The
        // bound-symbol check above stays, because a subscription for a symbol
        // this connection did not bind genuinely can never be delivered.
        {
            let mut subs = self
                .subs
                .lock()
                .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?;
            subs.entry(symbol).or_default().increment(kind);
        }

        // Nothing is sent to the venue. The subscription is satisfied entirely
        // by this local table: the WS reader forwards an arriving tick when the
        // matching kind's count is non-zero, and the venue pushes the run's one
        // tape whether or not anybody asked.
        //
        // There is no resume cursor at all, and a `start_ts` request parameter is
        // therefore not honoured on the subscribe path at all. There used to be
        // one - a per-symbol `SubState.start_ts` seeded here and advanced on
        // every delivered trade - written in three places and read in none,
        // because the reattach hook this client passes to `run_ws_connection`
        // is `Vec::new` and the historical request paths take their start from
        // `request.start`. Reintroducing a cursor means reintroducing a reader
        // in the same change; until then, maintaining one would be a mutex
        // acquisition per tick on the hot path for a value nobody consumes.
        Ok(())
    }

    fn subscribe_quotes_inner(
        &self,
        symbol: &str,
        after_enable: impl FnOnce(),
    ) -> anyhow::Result<()> {
        let _delivery = self
            .quote_delivery
            .lock()
            .map_err(|_| anyhow::anyhow!("quote delivery mutex poisoned"))?;
        self.subscribe_symbol(symbol.into(), SubKind::Quotes)?;
        // Nothing after the increment may fail: the member is accepted the
        // moment its counter moves, and an `Err` from here on would report it
        // refused while it holds a count the engine will never release. So
        // the relock recovers rather than refusing, and the cached-book
        // replay is best effort.
        let cached = lock_recover(&self.subs, "subscriptions")
            .get(symbol)
            .and_then(|state| state.cached_quote.clone());
        after_enable();
        if let Some(quote) = cached
            && let Some(def) = instrument_def(&self.instruments, symbol)
        {
            match self.sink() {
                Ok(sink) => emit_quote(&quote, &sink, &def, self.sim),
                Err(err) => tracing::warn!(
                    %symbol,
                    error = %err,
                    "quote subscription accepted, but the cached book could not be replayed"
                ),
            }
        }
        Ok(())
    }

    fn subscribe_funding_inner(&self, symbol: &str) -> anyhow::Result<()> {
        let _delivery = self
            .quote_delivery
            .lock()
            .map_err(|_| anyhow::anyhow!("market delivery mutex poisoned"))?;
        self.subscribe_symbol(symbol.into(), SubKind::Funding)?;
        // Infallible past the increment, for the reason given in
        // `subscribe_quotes_inner`.
        let cached = lock_recover(&self.subs, "subscriptions")
            .get(symbol)
            .and_then(|state| state.cached_funding.clone());
        if let Some(cached) = cached
            && let Some(def) = instrument_def(&self.instruments, symbol)
        {
            match self.sink() {
                Ok(sink) => emit_funding(&cached, &sink, &def, self.sim),
                Err(err) => tracing::warn!(
                    %symbol,
                    error = %err,
                    "funding subscription accepted, but the standing rate could not be replayed"
                ),
            }
        }
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    /// The mirror of `subscribe_symbol`, and equally local: dropping the last
    /// subscriber of every kind stops forwarding ticks. A row that has cached
    /// a quote or a funding rate remains resident so a later subscription of
    /// that kind can replay the current book or the standing rate; a row that
    /// has cached neither is retired. Nothing is sent to the venue, which keeps
    /// pushing the run's one tape either way.
    ///
    /// Infallible, and the lock recovers rather than refusing, because a
    /// release is the second half of `unsubscribe_bar`'s coupled release: that
    /// member has already dropped its bar ref when it gets here, so a refusal
    /// would report the member unreleased while half of it was gone, and a
    /// retry could never find the ref again to finish the job.
    fn unsubscribe_symbol(&mut self, symbol: Symbol, kind: SubKind) {
        let mut subs = lock_recover(&self.subs, "subscriptions");
        if let Some(state) = subs.get_mut(&symbol) {
            state.decrement(kind);
            if state.total() == 0 && state.cached_quote.is_none() && state.cached_funding.is_none()
            {
                subs.remove(&symbol);
            }
        }
    }

    fn sink(&self) -> anyhow::Result<EventSender<DataEvent>> {
        self.sink
            .as_ref()
            .cloned()
            .context("data event sink not initialized")
    }

    /// Gather what a paged history session needs off `&self`.
    ///
    /// Fails when this client is not connected, which is the honest answer
    /// rather than an inconvenience: a page reads the river this socket
    /// boarded, so with no socket there is no river to read and no label that
    /// could stand in for one.
    fn history_session(&self) -> anyhow::Result<HistorySession> {
        let cmd = self
            .ws_cmd
            .clone()
            .context("mogwai data client is not connected")?;
        Ok(HistorySession {
            cmd,
            pending: Arc::clone(&self.pending_history),
            next_request: Arc::clone(&self.next_request),
            timeout_secs: request_timeout_secs(&self.config.havoc, self.sim),
        })
    }

    /// Flush every completed-but-withheld bar window on teardown. A time
    /// window whose `close_ts` has already passed but that never got a later
    /// trade to cross its boundary is a genuinely complete bar that the lazy
    /// emit-on-next-trade rule would otherwise discard when the subscription
    /// state is torn down (`stop`, and through it `reset`/`dispose`) - the same
    /// discard `unsubscribe_bars` already guards for a single removed bar type,
    /// generalized to the whole table so a shutdown or a reconnect-driven
    /// `reset` does not silently drop the newest complete bar of every live bar
    /// feed. A still-in-progress window (close_ts still in the future) is left
    /// untouched: shipping it would inject a future-stamped, incomplete bar a
    /// consumer could not tell from a real one. Each flushed window is cleared
    /// so a second teardown call cannot double-emit, and the send is
    /// best-effort: if the egress receiver is already gone the bar simply drops.
    fn flush_completed_bars(&mut self) {
        let Ok(sink) = self.sink() else {
            return;
        };
        let now = now_unix_nanos(self.sim);
        let Ok(mut bars) = self.bars.lock() else {
            return;
        };
        for (bar_type, state) in bars.iter_mut() {
            let symbol = symbol_from_instrument(bar_type.instrument_id());
            let Some(def) = instrument_def(&self.instruments, &symbol) else {
                continue;
            };
            match state.window.take_closed(&def, now) {
                None => {}
                Some(Ok(bar)) => drop(sink.send(DataEvent::Data(Data::Bar(bar)))),
                Some(Err(err)) => tracing::warn!(
                    %bar_type,
                    error = %err,
                    "dropping unrepresentable bar on teardown flush"
                ),
            }
        }
    }

    /// One member of a `subscribe_bars` batch, atomic on its own: either both
    /// counters move or neither does. The batch contract it serves is stated
    /// over `subscribe_quotes` in the trait impl.
    fn subscribe_bar(&self, bar_type: BarType) -> anyhow::Result<()> {
        let window = BarAggregator::new(bar_type)?;
        let symbol = symbol_from_instrument(bar_type.instrument_id());
        {
            // `lock_recover`, matching the rollback below and every other bar
            // lock in this client. The two halves of one increment-then-roll-
            // back pair must not disagree about poisoning: failing the
            // increment on a poisoned guard while the rollback recovers it
            // would make the poisoned path take a branch no test covers.
            let mut bars = lock_recover(&self.bars, "bar");
            bars.entry(bar_type)
                .or_insert(BarSubState { refs: 0, window })
                .refs += 1;
        }
        // Roll the ref back when the symbol subscription refuses. The
        // per-`BarType` ref and the per-symbol `SubState.bars` count are the
        // two halves of one subscription and must move together - that is the
        // whole of the release rule in `unsubscribe_bar`, stated there from the
        // release side. `subscribe_symbol` can refuse (the bound-symbol check,
        // or a poisoned subscription table), and a ref left standing over a
        // refusal is the same cross-counter desync arriving from the subscribe
        // side: a later unsubscribe for that bar type finds `refs > 0`, so it
        // matches, and it spends a symbol-count decrement belonging to a
        // different bar type's live subscription - which, at zero, darkens the
        // surviving feed. In a batch the rollback is what lets the members
        // around a refused one stand: only this member's own ref is undone.
        if let Err(err) = self.subscribe_symbol(symbol, SubKind::Bars) {
            let mut bars = lock_recover(&self.bars, "bar");
            if let Some(state) = bars.get_mut(&bar_type) {
                state.refs = state.refs.saturating_sub(1);
                if state.refs == 0 {
                    bars.remove(&bar_type);
                }
            }
            return Err(err);
        }
        Ok(())
    }

    /// One member of an `unsubscribe_bars` batch.
    fn unsubscribe_bar(&mut self, bar_type: BarType) -> anyhow::Result<()> {
        let symbol = symbol_from_instrument(bar_type.instrument_id());
        // Decrement the per-symbol bars count only when this bar type actually had
        // a live subscription to release. The per-BarType refs and the
        // per-symbol SubState.bars count are incremented together on subscribe, so
        // they must be decremented together. An unmatched release (a bar type
        // never subscribed, a member of a batch whose subscribe was refused, or a
        // double-unsubscribe interleaved by nautilus command replay) that still
        // decremented the symbol count would steal a
        // decrement belonging to a different bar type's live subscription. No
        // frame goes to the venue either way - subscriptions are satisfied
        // entirely by this local table - and that is exactly what makes the
        // theft dangerous: if it drops the symbol's bars count to 0, the WS
        // reader stops forwarding that symbol's ticks to the bar path while the
        // venue keeps pushing them, so the surviving bar type goes dark with
        // nothing on the wire to show for it. Saturating arithmetic prevents
        // underflow, not this cross-type theft - so gate the symbol decrement
        // on a real match.
        let (matched, to_flush) = {
            let mut bars = self
                .bars
                .lock()
                .map_err(|_| anyhow::anyhow!("bar mutex poisoned"))?;
            let refs_after = match bars.get_mut(&bar_type) {
                Some(state) if state.refs > 0 => {
                    state.refs -= 1;
                    Some(state.refs)
                }
                _ => None,
            };
            match refs_after {
                // On the last release, take the removed bar type's active window
                // out with it so a completed-but-withheld bar can be flushed
                // below rather than silently discarded.
                Some(0) => (true, bars.remove(&bar_type).map(|state| state.window)),
                Some(_) => (true, None),
                None => (false, None),
            }
        };
        if matched {
            // Flush the removed bar type's active window only if it already closed
            // (close_ts <= sim-now) but was withheld only for lack of a later
            // trade to cross its boundary - the discard-on-unsubscribe case.
            // A genuinely in-progress window (close_ts still in the future) is
            // dropped, not emitted: shipping it would inject a future-stamped,
            // incomplete bar a consumer could not tell from a real completed one.
            // The teardown twin of this flush lives in `flush_completed_bars`
            // (called from `stop`). Closing a live in-progress window on time on
            // a clock timer is a separate feature, deliberately not built - see
            // `flush_completed_bars` and the withheld-bar note in havoc.md.
            if let Some(mut window) = to_flush
                && let Some(def) = instrument_def(&self.instruments, &symbol)
                && let Some(flushed) = window.take_closed(&def, now_unix_nanos(self.sim))
            {
                match flushed {
                    Ok(bar) => {
                        if let Ok(sink) = self.sink() {
                            drop(sink.send(DataEvent::Data(Data::Bar(bar))));
                        }
                    }
                    Err(err) => tracing::warn!(
                        %bar_type,
                        error = %err,
                        "dropping unrepresentable bar on unsubscribe flush"
                    ),
                }
            }
            self.unsubscribe_symbol(symbol, SubKind::Bars);
        } else {
            // Reported as released, not refused: the engine is told this bar
            // type is no longer held, which is true - it never was.
            tracing::warn!(
                %bar_type,
                "ignoring unsubscribe_bars with no matching subscription; \
                 not touching the symbol's shared bars count"
            );
        }
        Ok(())
    }
}

#[async_trait(?Send)]
impl DataClient for MogwaiDataClient {
    fn client_id(&self) -> ClientId {
        self.client_id
    }

    fn venue(&self) -> Option<Venue> {
        Some(*MOGWAI_VENUE)
    }

    fn start(&mut self) -> anyhow::Result<()> {
        self.sink = Some(get_data_event_sender());
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        // Close the command generation before cancelling its receiver. A new
        // request must fail synchronously instead of entering a dead queue.
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        // The aborted tasks include every `request_*` still collecting, and an
        // aborted task answers nothing, so the requests it owed are answered
        // here.
        self.fail_outstanding_requests(
            "the mogwai data client stopped before the request completed",
        );
        self.retire_connected_flag();
        // Dropping every sender wakes history collectors through their existing
        // abandoned-session arm instead of making them wait out the request
        // timeout after this transport generation is gone.
        lock_recover(&self.pending_history, "pending history").clear();
        // Emit any completed-but-withheld bar windows before the drain tasks are
        // gone and `reset` clears the table. Done after abort so no drain
        // task races a fresh trade into the same window; the shared bar mutex
        // keeps the flush idempotent regardless.
        self.flush_completed_bars();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        self.stop()?;
        self.subs
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?
            .clear();
        self.bars
            .lock()
            .map_err(|_| anyhow::anyhow!("bar mutex poisoned"))?
            .clear();
        Ok(())
    }

    fn dispose(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn is_disconnected(&self) -> bool {
        !self.is_connected()
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        let sink = self.sink()?;
        // A client owns exactly one transport generation. Retire every task
        // and sender from the previous generation before doing any async setup,
        // so a second connect cannot leave two readers sharing `connected`.
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        // Before the first await, as `stop()` does it and for the same reason.
        self.fail_outstanding_requests(
            "the mogwai data client reconnected before the request completed",
        );
        self.retire_connected_flag();
        lock_recover(&self.pending_history, "pending history").clear();
        let http_base_url = self.config.http_base_url();
        let (venue, floor_known) = fetch_clock_or_identity(&self.http, &http_base_url).await;
        let sim = venue.sim;
        self.sim = sim;
        self.data_origin_ns = floor_known.then_some(venue.data_origin_ns);
        let conn = conn_havoc(&self.config.havoc);
        self.http_quota = HttpQuota::from_conn(&conn, sim);
        seed_instruments(
            &self.http,
            &self.http_quota,
            &http_base_url,
            &self.instruments,
        )
        .await?;
        // Populate the worker's Nautilus cache before any subscription so the
        // executor's instrument-presence guard is satisfied the moment bars
        // start flowing (see `emit_seeded_instruments`). `sink` is still owned
        // here; both transport branches move/clone it afterwards.
        emit_seeded_instruments(&sink, &self.instruments, sim);
        // Venue-side divergences are execution-owned. The data client accepts
        // the same config object but only applies its inbound transport half.
        let inbound_havoc = inbound_havoc(&self.config.havoc);

        // `ws_url` already carries the `/ws` path.
        let ws_url = self.config.ws_url();

        let connected = Arc::clone(&self.connected);
        // Minted per connection: a reconnect gets a fresh channel, and the
        // waiters of the old one are failed by the undelivered hook rather than
        // left pointing at a socket that is gone.
        let (cmd_tx, cmd_rx) = unbounded_channel::<mogwai_protocol::Command>();
        self.ws_cmd = Some(cmd_tx);
        let undelivered_history = Arc::clone(&self.pending_history);
        let reply_history = Arc::clone(&self.pending_history);
        let instruments = Arc::clone(&self.instruments);
        let missing_instrument_warnings = Arc::clone(&self.missing_instrument_warnings);
        let subs = Arc::clone(&self.subs);
        let quote_delivery = Arc::clone(&self.quote_delivery);
        let bars = Arc::clone(&self.bars);
        let havoc_filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_inbound(
            &inbound_havoc,
        )));
        // The market-data drain no longer sleeps the per-message havoc latency
        // inline in the reader loop (which capped throughput at ~33 msg/s and
        // head-of-line-blocked pings/commands). It enqueues each filtered
        // message, arrival-anchored, into a latency pump that owns the sink and
        // paces delivery off-loop. Spawn and track the pump before the reader so
        // stop() aborts it alongside the connection task.
        let (deliver_tx, deliver_rx) = unbounded_channel::<HavocDelivery>();
        // The delivery barrier. The venue attaches this socket to the live tape
        // at upgrade, so trade and quote frames can arrive before the post-bind
        // reseed below has read `/instruments` - and `instrument_def` black-holes
        // a frame whose def is missing. The reader still enqueues; the pump holds
        // its first delivery until the reseed says go, so nothing reaches a
        // handler before the def it needs is resident. Holding rather than
        // dropping is the point: those frames are real tape.
        let (delivery_ready, pump_ready) = tokio::sync::watch::channel(false);
        let reseed_sink = sink.clone();
        let pump_handle = spawn_latency_pump(deliver_rx, move |msg| {
            let sink = sink.clone();
            let instruments = Arc::clone(&instruments);
            let missing_instrument_warnings = Arc::clone(&missing_instrument_warnings);
            let subs = Arc::clone(&subs);
            let quote_delivery = Arc::clone(&quote_delivery);
            let bars = Arc::clone(&bars);
            let mut pump_ready = pump_ready.clone();
            async move {
                // An `Err` means connect dropped the sender - the barrier will
                // never open, so deliver rather than wedge the pump.
                drop(pump_ready.wait_for(|open| *open).await);
                handle_market_message(
                    msg,
                    &sink,
                    &instruments,
                    &missing_instrument_warnings,
                    &subs,
                    &quote_delivery,
                    &bars,
                    sim,
                )
                .await;
            }
        });
        track_task(&self.task_handles, pump_handle);

        let handler_filter = Arc::clone(&havoc_filter);
        let handler_deliver = deliver_tx.clone();
        let disconnect_filter = Arc::clone(&havoc_filter);
        let disconnect_deliver = deliver_tx;
        let task_ws_url = ws_url.clone();
        let identity = run_identity_check(
            self.http.clone(),
            self.http_quota.clone(),
            http_base_url.clone(),
            self.config.expected_run_seed,
            "data",
        );
        let dial_timeout = std::time::Duration::from_secs(self.config.dial_timeout_secs);
        let connected_notify = Arc::clone(&self.connected_notify);
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: inbound_havoc.seed,
                    connected,
                    connected_notify,
                    sim,
                    label: "data",
                    identity,
                    dial_timeout,
                    // The data leg holds no witness for its sink. Its events go
                    // to the runner's data channel, which it never obtains a
                    // sender for - the execution leg is the only one that keeps
                    // one, so it is the only one that can observe the loss.
                    sink_dead: None,
                },
                Some(cmd_rx),
                // The client's command type is itself the wire command here: this leg
                // sends only history requests, which need no per-connection
                // rewriting the way a resubscribe would.
                mogwai_protocol::Command::clone,
                // The venue pushes the one run's tape unbidden, so a reattach
                // has no subscribe frames to replay: subscription state is
                // satisfied locally in this client and never reaches the wire.
                Vec::new,
                move |venue_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let handler_deliver = handler_deliver.clone();
                    let reply_history = Arc::clone(&reply_history);
                    async move {
                        // Resolved right here rather than through the market pump. A
                        // page is a correlated reply, not channel data: sending
                        // it through the pump would hold it behind the delivery
                        // barrier that exists to keep tape frames from
                        // outrunning their instrument defs, and would subject a
                        // request-response to the inbound data latency meant for
                        // a pushed feed.
                        let Some(venue_msg) = route_history(venue_msg, &reply_history) else {
                            return;
                        };
                        let mut filter = handler_filter.lock().await;
                        enqueue_havoc(&mut filter, venue_msg, sim, &handler_deliver);
                        if let Some(token) = filter.held_token() {
                            schedule_reorder_flush(
                                Arc::clone(&handler_filter),
                                token,
                                sim,
                                handler_deliver.clone(),
                            );
                        }
                    }
                },
                move || {
                    let disconnect_filter = Arc::clone(&disconnect_filter);
                    let disconnect_deliver = disconnect_deliver.clone();
                    async move {
                        let mut filter = disconnect_filter.lock().await;
                        flush_havoc_into_pump(&mut filter, sim, &disconnect_deliver);
                    }
                },
                // A history request that never reached the venue fails its
                // waiter here rather than being left to time out. A consumer
                // cannot tell a lost command from a slow one, and a backfill
                // that hangs is indistinguishable from a quiet market - which
                // is the failure the whole correlated shape exists to avoid.
                {
                    let undelivered = Arc::clone(&undelivered_history);
                    move |cmd: mogwai_protocol::Command| {
                        let mogwai_protocol::Command::QueryHistory { request_id, .. } = cmd else {
                            return;
                        };
                        if let Ok(mut pending) = undelivered.lock()
                            && let Some(waiter) = pending.remove(&request_id)
                        {
                            drop(waiter.send(HistoryReply::Rejected {
                                reason:
                                    "history request was never delivered to the venue".to_owned(),
                            }));
                        }
                    }
                },
            )
            .await;
        });

        track_task(&self.task_handles, reader_handle);
        // A timed-out connect must not orphan the reader task: it is already in
        // task_handles and would keep looping/reconnecting on the shared
        // `connected` flag, so a retry would spawn a second reader racing the
        // first. Abort the task and clear the stale handle and ws_cmd before
        // propagating, leaving the client cleanly disconnected for retry.
        if let Err(err) = wait_connected(
            &self.connected,
            &self.connected_notify,
            &ws_url,
            dial_timeout,
        )
        .await
        {
            abort_tasks(&self.task_handles);
            self.retire_connected_flag();
            self.ws_cmd = None;
            lock_recover(&self.pending_history, "pending history").clear();
            return Err(err);
        }
        // The post-bind reseed. Binding is what registers an unconfigured symbol
        // venue-side, so the pre-dial seed cannot have carried its def; only a
        // read after the socket is up can. `cache_instruments` overwrites by key
        // and re-emitting an unchanged def is idempotent at the nautilus cache,
        // so this costs one HTTP round trip and changes nothing for a run whose
        // symbols were all configured.
        if let Err(err) = seed_instruments(
            &self.http,
            &self.http_quota,
            &http_base_url,
            &self.instruments,
        )
        .await
        {
            // Same teardown as a timed-out connect: leave nothing running that a
            // retry would race, and never leave the barrier shut on a live pump.
            abort_tasks(&self.task_handles);
            self.retire_connected_flag();
            self.ws_cmd = None;
            lock_recover(&self.pending_history, "pending history").clear();
            return Err(err);
        }
        emit_seeded_instruments(&reseed_sink, &self.instruments, sim);
        if delivery_ready.send(true).is_err() {
            // No receiver: the pump task is already gone, so there is nothing
            // held behind the barrier to release.
            tracing::debug!("released the delivery barrier with no pump listening");
        }
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn subscribe_instruments(&mut self, _cmd: SubscribeInstruments) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let sim = self.sim;
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                    cache_instruments(&instruments, defs.clone());
                    let ts_init = now_unix_nanos(sim);
                    for def in defs {
                        if let Some(instrument) = instrument_any_or_warn(&def, ts_init) {
                            drop(
                                sink.send(DataEvent::Data(Data::Instrument(Box::new(instrument)))),
                            );
                        }
                    }
                }
            }),
        );
        Ok(())
    }

    /// One fetch answers the whole batch: the venue serves its instrument set
    /// as one document, so a batch of N ids is one HTTP round trip that emits
    /// the requested subset, not N round trips. Acceptance is the dispatch of
    /// that fetch, so every member is accepted together; a member the venue
    /// does not define simply emits nothing, as a single-id subscription for an
    /// unknown symbol always did.
    fn subscribe_instrument(&mut self, cmd: SubscribeInstrument) -> anyhow::Result<()> {
        let symbols: std::collections::HashSet<Symbol> = cmd
            .instrument_ids
            .iter()
            .map(|id| symbol_from_instrument(*id))
            .collect();
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let sim = self.sim;
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                    cache_instruments(&instruments, defs.clone());
                    let ts_init = now_unix_nanos(sim);
                    for def in defs {
                        if symbols.contains(&def.symbol)
                            && let Some(instrument) = instrument_any_or_warn(&def, ts_init)
                        {
                            drop(
                                sink.send(DataEvent::Data(Data::Instrument(Box::new(instrument)))),
                            );
                        }
                    }
                }
            }),
        );
        Ok(())
    }

    fn unsubscribe_instruments(&mut self, _cmd: &UnsubscribeInstruments) -> anyhow::Result<()> {
        Ok(())
    }

    fn unsubscribe_instrument(&mut self, _cmd: &UnsubscribeInstrument) -> anyhow::Result<()> {
        Ok(())
    }

    // The batch contract, one rule for quotes, trades, funding and bars, in
    // both directions: each member is applied on its own and is atomic on its
    // own, a refused member does not stop the members after it, and nothing an
    // earlier member did is rolled back. A failure returns nautilus's
    // `SubscriptionError` naming exactly the members that were applied.
    //
    // Per member rather than all-or-nothing because that is the contract the
    // engine is built to: the `DataClient` trait asks for best effort without
    // rolling back accepted members, and `DataClientAdapter::subscribe_batch`
    // records precisely the ids a `SubscriptionError` lists as active and no
    // others. That listing is what keeps the engine's own subscription set and
    // this client's counters describing the same subscriptions. A loop that
    // stopped at the first refusal and returned a bare error would tell the
    // engine nothing was accepted while the members before the refusal held
    // counts here; the engine would never release them, so those feeds could
    // not be turned off, and a re-subscribe would count them twice.
    //
    // Member atomicity is the other half, and it is what the bar member's
    // rollback in `subscribe_bar` provides: its two counters move together or
    // not at all, so a refusal midway through a batch leaves no stray ref that
    // a later unsubscribe could spend against a surviving feed. Subscriptions
    // are local table entries, so there is no venue-side state to unwind.
    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |id| {
            self.subscribe_quotes_inner(&symbol_from_instrument(id), || {})
        })
    }

    fn subscribe_funding_rates(&mut self, cmd: SubscribeFundingRates) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |id| {
            self.subscribe_funding_inner(&symbol_from_instrument(id))
        })
    }

    fn subscribe_trades(&mut self, cmd: SubscribeTrades) -> anyhow::Result<()> {
        execute_instrument_subscriptions(&cmd.instrument_ids, |id| {
            self.subscribe_symbol(symbol_from_instrument(id), SubKind::Trades)
        })
    }

    fn subscribe_bars(&mut self, cmd: SubscribeBars) -> anyhow::Result<()> {
        execute_bar_subscriptions(&cmd.bar_types, |bar_type| self.subscribe_bar(bar_type))
    }

    // An instrument-keyed release cannot fail (see `unsubscribe_symbol`), so
    // every member is released and the batch is accepted whole.
    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        for &id in &cmd.instrument_ids {
            self.unsubscribe_symbol(symbol_from_instrument(id), SubKind::Quotes);
        }
        Ok(())
    }

    fn unsubscribe_funding_rates(&mut self, cmd: &UnsubscribeFundingRates) -> anyhow::Result<()> {
        for &id in &cmd.instrument_ids {
            self.unsubscribe_symbol(symbol_from_instrument(id), SubKind::Funding);
        }
        Ok(())
    }

    fn unsubscribe_trades(&mut self, cmd: &UnsubscribeTrades) -> anyhow::Result<()> {
        for &id in &cmd.instrument_ids {
            self.unsubscribe_symbol(symbol_from_instrument(id), SubKind::Trades);
        }
        Ok(())
    }

    fn unsubscribe_bars(&mut self, cmd: &UnsubscribeBars) -> anyhow::Result<()> {
        execute_bar_subscriptions(&cmd.bar_types, |bar_type| self.unsubscribe_bar(bar_type))
    }

    // How a history request is answered, one rule for trades, quotes and bars.
    //
    // Every request resolves, exactly once, and the same holds for the two
    // instrument requests. A success goes out as a payload response; a failure
    // to retrieve goes out as nautilus's `DataRequestFailed`, naming the
    // reason. The failure channel is the point: an empty success is
    // indistinguishable from a quiet window, and this river has multi-hour
    // droughts.
    //
    // A failure carries a payload only when the history was actually
    // retrieved for at least one member, and then the payload is exactly what
    // those members got, which may be no rows - a truthful quiet window for
    // them. A failure that retrieved nothing (an off-river window, a refused
    // or dropped page session, no member resolvable, a teardown) carries
    // none. That is deliberate: nautilus's `DataActor` runs the typed hook
    // (`on_historical_bars` and kin) before it hands the envelope with its
    // failure reason to `on_data_response`, so an empty payload on a failure
    // would show a strategy a quiet window first and the failure second.
    //
    // A synchronous `Err` from these handlers resolves too (the engine answers
    // it with a terminal failure envelope), so `?` on a precondition is an
    // honest refusal. The engine's reason for one is only its own context
    // line, though, which is why the off-river refusal answers in band with
    // the floor named. Once a task is spawned the request is registered as
    // outstanding and answered through `resolve_request`, and `stop()` and
    // `connect()` answer whatever is still outstanding when they abort it.
    //
    // A success goes out as `DataEvent::Response`, the payload-only reply,
    // rather than as a completed `DataResponseEnvelope`, and that is a choice
    // about catalog write-back. Nautilus writes a response into a host's
    // catalog only on an envelope that certifies coverage, and a mogwai window
    // is one run's realization under an instrument id that every run and
    // every seed shares: certified, it would let a later run read this run's
    // water back as history. What a host already stored is read before any
    // request reaches this client, so isolating runs that share a catalog is
    // the host's to do.
    //
    // Rows that fail conversion to nautilus types are dropped with a warning
    // in `historical_trades`/`historical_quotes` rather than failing the
    // request; that is a representation refusal, not a retrieval failure.
    //
    // A request may name several instruments. This socket reads one river, so
    // the window is paged once and its rows are partitioned by label: a member
    // gets only the rows its own symbol labels, and a member this connection
    // is not bound to is a member failure rather than an empty answer - the
    // same bound-symbol rule the subscribe path applies. Without the partition,
    // every member would be handed the whole river relabelled as itself.
    fn request_trades(&self, request: RequestTrades) -> anyhow::Result<()> {
        ensure!(
            !request.instrument_ids.is_empty(),
            "request_trades: the request names no instrument"
        );
        let sink = self.sink()?;
        let session = self.history_session()?;
        let bound = self.config.symbol.clone();
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        // Pinned once, before any page goes out. The venue answers history as of
        // the run clock when each request is admitted, so a paged window with no
        // stated end would be cut against a later present on every page and the
        // logical window would grow as the pages were fetched. Naming the end up
        // front makes one logical request one fixed window; the venue clamps it
        // against its own snapshot regardless, so a pin slightly ahead converges
        // rather than reaching past the run present.
        //
        // Only when the clock is authoritative. `data_origin_ns` is `Some`
        // exactly when `/clock` was actually fetched; when it was not, `sim` is
        // an identity projection standing in for an axis this client never read,
        // and pinning from it would cut every window against a wall-clock instant
        // that has nothing to do with the run - silently returning less than the
        // caller asked for. Leaving the end absent hands the choice to the venue,
        // which is the only party that knows its own present.
        let end = date_to_unix_nanos(request.end)
            .or_else(|| self.data_origin_ns.is_some().then(|| now_unix_nanos(sim)));
        // Refuse an off-river window at the boundary, loudly, and answer it as
        // a failure that retrieved nothing, per the rule above.
        if let Err(err) = ensure_on_river(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_trades: refusing an off-river window");
            drop(sink.send(request_event(
                request.request_id,
                None,
                &[format!("{err:#}")],
            )));
            return Ok(());
        }
        let outstanding = Arc::clone(&self.outstanding_requests);
        self.register_request(request.request_id);
        track_task(&self.task_handles, get_runtime().spawn(async move {
            // Page the window rather than issuing one MAX_HISTORY_LIMIT-capped
            // request: a window with more than 1000 trades used to return only
            // its oldest 1000, with the response still claiming the full range.
            // `request.limit` counts trades here, so it becomes the pagination
            // ceiling. The engine refuses a limit on a multi-instrument request,
            // so a ceiling only ever bounds a single member's rows.
            let max_trades = request.limit.map(std::num::NonZeroUsize::get);
            let mut failures = Vec::new();
            let members = resolve_history_members(
                &request.instrument_ids,
                bound.as_deref(),
                &http,
                &http_quota,
                &base,
                &instruments,
                &mut failures,
            )
            .await;
            let mut data = Vec::new();
            let mut retrieved = false;
            if !members.is_empty() {
                // Over this client's OWN socket, which is what makes the rows
                // this passenger's river rather than whatever the label
                // resolves to. The symbol is still used to find the instrument
                // def - that is instrument identity, which a label does name -
                // and to partition the rows, but it selects no water here.
                match session
                    .collect(mogwai_protocol::HistoryKind::Trades, start, end, max_trades)
                    .await
                {
                    Ok((rows, truncated)) => {
                        let trades: Vec<mogwai_protocol::TradeTick> = rows
                            .into_iter()
                            .filter_map(|row| match row {
                                mogwai_protocol::HistoryRow::Trade(trade) => Some(trade),
                                mogwai_protocol::HistoryRow::Quote(_) => None,
                            })
                            .collect();
                        if truncated {
                            tracing::warn!(
                                trades = trades.len(),
                                "request_trades: history window truncated before its end at the caller's own trade limit; the requested history may not splice contiguously into live"
                            );
                        }
                        for (id, def) in &members {
                            data.extend(historical_trades(&trades, *id, def));
                        }
                        retrieved = true;
                    }
                    // Surfaced rather than silently emptied: a refusal, a
                    // timeout or a dropped socket must be visible, not mistaken
                    // for "no trades in the window".
                    Err(err) => failures.push(format!("history failed: {err:#}")),
                }
            }
            data.sort_by_key(|trade| trade.ts_init);
            let response = retrieved.then(|| {
                DataResponse::Trades(TradesResponse::new(
                    request.request_id,
                    client_id,
                    request.instrument_ids,
                    data,
                    start,
                    end,
                    now_unix_nanos(sim),
                    request.params,
                ))
            });
            resolve_request(&outstanding, &sink, request.request_id, response, &failures);
        }));
        Ok(())
    }

    fn request_quotes(&self, request: RequestQuotes) -> anyhow::Result<()> {
        ensure!(
            !request.instrument_ids.is_empty(),
            "request_quotes: the request names no instrument"
        );
        let sink = self.sink()?;
        let session = self.history_session()?;
        let bound = self.config.symbol.clone();
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        // One request owns one fixed window. See `request_trades` for why an
        // absent caller end is pinned only when this client read the run clock.
        let end = date_to_unix_nanos(request.end)
            .or_else(|| self.data_origin_ns.is_some().then(|| now_unix_nanos(sim)));
        if let Err(err) = ensure_on_river(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_quotes: refusing an off-river window");
            drop(sink.send(request_event(
                request.request_id,
                None,
                &[format!("{err:#}")],
            )));
            return Ok(());
        }
        let outstanding = Arc::clone(&self.outstanding_requests);
        self.register_request(request.request_id);
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                let mut failures = Vec::new();
                let members = resolve_history_members(
                    &request.instrument_ids,
                    bound.as_deref(),
                    &http,
                    &http_quota,
                    &base,
                    &instruments,
                    &mut failures,
                )
                .await;
                let mut data = Vec::new();
                let mut retrieved = false;
                if !members.is_empty() {
                    // `collect` already trims to the row ceiling, and quote rows
                    // are a subset of the rows, so no second cut is needed here.
                    let limit = request.limit.map(std::num::NonZeroUsize::get);
                    match session
                        .collect(mogwai_protocol::HistoryKind::Quotes, start, end, limit)
                        .await
                    {
                        Ok((rows, truncated)) => {
                            let quotes: Vec<mogwai_protocol::QuoteTick> = rows
                                .into_iter()
                                .filter_map(|row| match row {
                                    mogwai_protocol::HistoryRow::Quote(quote) => Some(quote),
                                    mogwai_protocol::HistoryRow::Trade(_) => None,
                                })
                                .collect();
                            if truncated {
                                tracing::warn!("quote history truncated at the caller's own limit");
                            }
                            for (id, def) in &members {
                                data.extend(historical_quotes(&quotes, *id, def));
                            }
                            retrieved = true;
                        }
                        Err(err) => failures.push(format!("history failed: {err:#}")),
                    }
                }
                data.sort_by_key(|quote| quote.ts_init);
                let response = retrieved.then(|| {
                    DataResponse::Quotes(QuotesResponse::new(
                        request.request_id,
                        client_id,
                        request.instrument_ids,
                        data,
                        start,
                        end,
                        now_unix_nanos(sim),
                        request.params,
                    ))
                });
                resolve_request(&outstanding, &sink, request.request_id, response, &failures);
            }),
        );
        Ok(())
    }

    fn request_bars(&self, request: RequestBars) -> anyhow::Result<()> {
        // One bar type per request. The engine's request validation already
        // refuses a longer list for bars, so this only restates it for a host
        // that drives the client directly; the synchronous `Err` resolves the
        // request as failed either way.
        let bar_type = request.single_bar_type()?;
        let interval = crate::bars::admit(&bar_type)?.get();
        let sink = self.sink()?;
        let session = self.history_session()?;
        let bound = self.config.symbol.clone();
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        // Pinned before the first page, and only from an authoritative clock, for
        // the reasons spelled out in `request_trades`.
        let end = date_to_unix_nanos(request.end)
            .or_else(|| self.data_origin_ns.is_some().then(|| now_unix_nanos(sim)));
        // Refuse an off-river history window at the boundary, naming the floor,
        // and answer it as a failure that retrieved nothing, by the rule stated
        // over `request_trades`.
        if let Err(err) = ensure_on_river(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_bars: refusing an off-river history window");
            drop(sink.send(request_event(
                request.request_id,
                None,
                &[format!("{err:#}")],
            )));
            return Ok(());
        }
        let outstanding = Arc::clone(&self.outstanding_requests);
        self.register_request(request.request_id);
        track_task(&self.task_handles, get_runtime().spawn(async move {
            let instrument_id = bar_type.instrument_id();
            let symbol = symbol_from_instrument(instrument_id);
            let mut failures = Vec::new();
            // Page the window, translating nautilus's bar-count limit into a
            // bar-span pagination target: the old single request applied a
            // bar-count limit as a trade-page limit, so a history request for
            // N bars fetched at most N trades (~N/5 bars on the fitted tape)
            // covering only the oldest edge, under-delivering or timing out
            // the history request.
            let bar_limit = request.limit.map(std::num::NonZeroUsize::get);
            // Every exit from this block yields bars, so the result below is
            // always sent. A failure arm used to `return` straight out of the
            // task, which left the nautilus request unresolved forever: from the
            // consumer that is indistinguishable from a hang, and it burns the
            // whole history-request timeout before dying with nothing but a
            // line in the worker log to show for it. A failure arm now records
            // its reason and breaks out, and the result goes out as a
            // `DataRequestFailed` carrying that reason and no payload: one
            // bar type is one member, so a failure here retrieved nothing.
            let bars: Vec<Bar> = 'bars: {
                let members = resolve_history_members(
                    &[instrument_id],
                    bound.as_deref(),
                    &http,
                    &http_quota,
                    &base,
                    &instruments,
                    &mut failures,
                )
                .await;
                let Some((_, def)) = members.into_iter().next() else {
                    break 'bars Vec::new();
                };
                // Bars are folded from trades, so this asks for the trade
                // stream and stops when the span it has covers the bars the
                // caller asked for. Taking the caller's BAR limit as a row
                // limit would be the old defect: a request for N bars would
                // fetch N trades and fold roughly a fifth of the bars wanted.
                let (rows, truncated) = match session
                    .collect_until(
                        mogwai_protocol::HistoryKind::Trades,
                        start,
                        end,
                        |rows| bar_span_reached(rows, interval, bar_limit),
                    )
                    .await
                {
                    Ok(result) => result,
                    Err(err) => {
                        failures.push(format!("history failed: {err:#}"));
                        break 'bars Vec::new();
                    }
                };
                // Only rows this bar type's own label names, for the partition
                // reason stated over `request_trades`.
                let trades: Vec<mogwai_protocol::TradeTick> = rows
                    .into_iter()
                    .filter_map(|row| match row {
                        mogwai_protocol::HistoryRow::Trade(trade) if trade.symbol == def.symbol => {
                            Some(trade)
                        }
                        mogwai_protocol::HistoryRow::Trade(_)
                        | mogwai_protocol::HistoryRow::Quote(_) => None,
                    })
                    .collect();
                if truncated {
                    tracing::warn!(
                        %symbol,
                        "request_bars: history window truncated before its end at the caller's own bar limit; the requested history may not splice contiguously into live"
                    );
                }
                let mut bars = match aggregate_bars(&bar_type, &trades, &def, end) {
                    Ok(bars) => bars,
                    Err(err) => {
                        failures.push(format!("bar type {bar_type} refused: {err:#}"));
                        break 'bars Vec::new();
                    }
                };
                if let Some(m) = bar_limit {
                    // Paging spans at least `bar_limit` intervals, so it may produce
                    // a few extra bars; trim to the requested count (oldest edge,
                    // from the window start) so the response honors the bar limit.
                    bars.truncate(m);
                }
                // An on-river window that under-delivers is a real, reachable
                // state, not an error: mogwai's fitted arrival process is
                // heavy-tailed, and a measured sweep of the default 24h-horizon
                // tape found stretches of 15+ simulated hours running at 3-10
                // trades per hour (see reference/architecture-generator.md, "Tape arrival
                // droughts"). Bars exist only for intervals that contain a
                // trade, so inside a drought a request for N bars typically
                // comes back with a handful - non-empty, so nothing downstream
                // objects, and the strategy silently warms from a fraction of
                // its configured history. That short case is the dangerous one
                // precisely because it does not stop anything, so it warns on
                // the same footing as the empty case. The venue side is where
                // the trade count and the drought context actually live.
                let short = bar_limit.is_some_and(|m| bars.len() < m);
                if bars.is_empty() || short {
                    tracing::warn!(
                        %symbol,
                        %bar_type,
                        ?start,
                        ?end,
                        requested = ?bar_limit,
                        produced = bars.len(),
                        trades = trades.len(),
                        "request_bars: the window is on-river but produced fewer bars than requested; \
                         mogwai's synthetic river has multi-hour arrival droughts, so a short history \
                         window can legitimately be sparse or empty - widen the window, lower the bar \
                         interval, or let the venue run further past its epoch before starting the history request"
                    );
                }
                bars
            };
            let response = failures.is_empty().then(|| {
                DataResponse::Bars(BarsResponse::new(
                    request.request_id,
                    client_id,
                    bar_type,
                    bars,
                    start,
                    end,
                    now_unix_nanos(sim),
                    request.params,
                ))
            });
            resolve_request(&outstanding, &sink, request.request_id, response, &failures);
        }));
        Ok(())
    }

    fn request_instruments(&self, request: RequestInstruments) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let outstanding = Arc::clone(&self.outstanding_requests);
        self.register_request(request.request_id);
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                // Same always-answer rule as the history requests: the old
                // `if let Ok` swallowed a failed fetch entirely, and the
                // nautilus request then never resolved. A failed fetch now
                // resolves as a failure with no payload. A definition nautilus
                // cannot represent (a `forex` symbol, which `convert` refuses
                // by name) is left out of the listing with a warning and is not
                // a failure: this request asks for what the venue can publish,
                // and a standing refusal would otherwise fail every listing of
                // a venue that serves one such symbol.
                let ts_init = now_unix_nanos(sim);
                let response = match fetch_instruments(&http, &http_quota, &base).await {
                    Ok(defs) => {
                        cache_instruments(&instruments, defs.clone());
                        let data = defs
                            .iter()
                            .filter_map(|def| instrument_any_or_warn(def, ts_init))
                            .collect();
                        Ok(DataResponse::Instruments(InstrumentsResponse::new(
                            request.request_id,
                            client_id,
                            request.venue.unwrap_or(*MOGWAI_VENUE),
                            data,
                            date_to_unix_nanos(request.start),
                            date_to_unix_nanos(request.end),
                            ts_init,
                            request.params,
                        )))
                    }
                    Err(err) => Err(format!("instrument fetch failed: {err:#}")),
                };
                match response {
                    Ok(response) => {
                        resolve_request(
                            &outstanding,
                            &sink,
                            request.request_id,
                            Some(response),
                            &[],
                        );
                    }
                    Err(reason) => {
                        resolve_request(&outstanding, &sink, request.request_id, None, &[reason]);
                    }
                }
            }),
        );
        Ok(())
    }

    /// Several ids answer as one `InstrumentsResponse` of those that resolved;
    /// one id answers as an `InstrumentResponse`, the upstream shape. Any id
    /// that fails - including one whose definition nautilus cannot represent,
    /// since this request names it - resolves the request as failed, carrying
    /// the instruments that did resolve when at least one did, and no payload
    /// when none did. `InstrumentResponse` holds exactly one instrument and has
    /// no empty form, so a failed single-id request used to be unanswerable
    /// and stayed unresolved; the failure channel is what lets it resolve now.
    /// Instrument identity is not bound to the socket's river, so no
    /// bound-symbol rule applies here.
    fn request_instrument(&self, request: RequestInstrument) -> anyhow::Result<()> {
        ensure!(
            !request.instrument_ids.is_empty(),
            "request_instrument: the request names no instrument"
        );
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let outstanding = Arc::clone(&self.outstanding_requests);
        self.register_request(request.request_id);
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                let ts_init = now_unix_nanos(sim);
                let mut failures = Vec::new();
                let mut data = Vec::new();
                for &id in &request.instrument_ids {
                    let symbol = symbol_from_instrument(id);
                    match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
                    {
                        Ok(def) => match instrument_any_or_warn(&def, ts_init) {
                            Some(instrument) => data.push(instrument),
                            None => failures.push(format!(
                                "{id}: the venue's definition is unrepresentable as a nautilus instrument"
                            )),
                        },
                        Err(err) => failures.push(format!("{id}: {err:#}")),
                    }
                }
                let start = date_to_unix_nanos(request.start);
                let end = date_to_unix_nanos(request.end);
                let response = if let [single] = request.instrument_ids.as_slice() {
                    data.pop().map(|instrument| {
                        DataResponse::Instrument(Box::new(InstrumentResponse::new(
                            request.request_id,
                            client_id,
                            *single,
                            instrument,
                            start,
                            end,
                            ts_init,
                            request.params.clone(),
                        )))
                    })
                } else {
                    (!data.is_empty()).then(|| {
                        DataResponse::Instruments(InstrumentsResponse::new(
                            request.request_id,
                            client_id,
                            *MOGWAI_VENUE,
                            data,
                            start,
                            end,
                            ts_init,
                            request.params.clone(),
                        ))
                    })
                };
                resolve_request(&outstanding, &sink, request.request_id, response, &failures);
            }),
        );
        Ok(())
    }
}
#[derive(Clone, Copy)]
enum SubKind {
    Trades,
    Quotes,
    Bars,
    Funding,
}

#[derive(Debug, Clone)]
struct CachedFunding {
    rate: rust_decimal::Decimal,
    interval_ns: u64,
    next_funding_ns: u64,
    ts_event: u64,
}

#[derive(Debug, Default)]
struct SubState {
    trades: usize,
    quotes: usize,
    bars: usize,
    funding: usize,
    cached_quote: Option<mogwai_protocol::QuoteTick>,
    cached_funding: Option<CachedFunding>,
}

impl SubState {
    fn total(&self) -> usize {
        self.trades + self.quotes + self.bars + self.funding
    }

    fn increment(&mut self, kind: SubKind) {
        match kind {
            SubKind::Trades => self.trades += 1,
            SubKind::Quotes => self.quotes += 1,
            SubKind::Bars => self.bars += 1,
            SubKind::Funding => self.funding += 1,
        }
    }

    fn decrement(&mut self, kind: SubKind) {
        match kind {
            SubKind::Trades => self.trades = self.trades.saturating_sub(1),
            SubKind::Quotes => self.quotes = self.quotes.saturating_sub(1),
            SubKind::Bars => self.bars = self.bars.saturating_sub(1),
            SubKind::Funding => self.funding = self.funding.saturating_sub(1),
        }
    }
}

#[derive(Debug)]
struct BarSubState {
    refs: usize,
    window: BarAggregator,
}

/// Everything one paged history session needs, gathered off `&self` so the
/// spawned task owns it.
#[derive(Clone)]
struct HistorySession {
    cmd: UnboundedSender<mogwai_protocol::Command>,
    pending: Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<HistoryReply>>>>,
    next_request: Arc<std::sync::atomic::AtomicU64>,
    timeout_secs: u64,
}

impl HistorySession {
    /// Page a whole window off the socket, or fail saying why.
    ///
    /// Pulls: the next page is asked for only after this one is in hand, which
    /// is what bounds how much of a window the venue has resident for a reader
    /// that has stopped reading.
    ///
    /// `max_rows` bounds what the caller wanted; the venue bounds each page
    /// independently. Reaching the caller's ceiling stops the session with rows
    /// in hand rather than failing it, and the caller is told the window was
    /// truncated - the same contract the HTTP pagination had.
    async fn collect(
        &self,
        kind: mogwai_protocol::HistoryKind,
        start: Option<UnixNanos>,
        end: Option<UnixNanos>,
        max_rows: Option<usize>,
    ) -> anyhow::Result<(Vec<mogwai_protocol::HistoryRow>, bool)> {
        self.collect_until(kind, start, end, move |rows| {
            max_rows.is_some_and(|max| rows.len() >= max)
        })
        .await
        .map(|(mut rows, truncated)| {
            // Paging overshoots a row ceiling by up to one page, so the trim
            // happens here rather than in the loop: the caller asked for a
            // count and gets exactly that, from the oldest edge.
            if let Some(max) = max_rows {
                rows.truncate(max);
            }
            (rows, truncated)
        })
    }

    /// The same session, stopping on a caller's own predicate rather than on a
    /// row count - which is what a bar request needs, since it wants a span of
    /// trades rather than a number of them.
    async fn collect_until(
        &self,
        kind: mogwai_protocol::HistoryKind,
        start: Option<UnixNanos>,
        end: Option<UnixNanos>,
        enough: impl Fn(&[mogwai_protocol::HistoryRow]) -> bool + Send,
    ) -> anyhow::Result<(Vec<mogwai_protocol::HistoryRow>, bool)> {
        let start = start.map(|ts| ts.as_u64());
        let end = end.map(|ts| ts.as_u64());
        let mut out: Vec<mogwai_protocol::HistoryRow> = Vec::new();
        let mut continuation: Option<String> = None;
        loop {
            let request_id = format!(
                "mogwai-history-{}",
                self.next_request
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            );
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            // Registered before the command goes out, so a reply racing back
            // faster than this task resumes still finds its slot; removed again
            // on a send failure so a dead socket does not leak the entry.
            self.pending
                .lock()
                .map_err(|_| anyhow::anyhow!("history waiter mutex poisoned"))?
                .insert(request_id.clone(), reply_tx);
            let sent = self.cmd.send(mogwai_protocol::Command::QueryHistory {
                request_id: request_id.clone(),
                kind,
                start,
                end,
                continuation: continuation.clone(),
            });
            if sent.is_err() {
                if let Ok(mut pending) = self.pending.lock() {
                    pending.remove(&request_id);
                }
                anyhow::bail!("history request not sent: the data socket is gone");
            }

            let timeout = std::time::Duration::from_secs(self.timeout_secs.max(1));
            let reply = match tokio::time::timeout(timeout, reply_rx).await {
                Ok(Ok(reply)) => reply,
                Ok(Err(_)) => anyhow::bail!("history request abandoned: the data client stopped"),
                Err(_) => {
                    if let Ok(mut pending) = self.pending.lock() {
                        pending.remove(&request_id);
                    }
                    anyhow::bail!(
                        "history request {request_id} timed out after {}s",
                        self.timeout_secs.max(1)
                    )
                }
            };
            let (rows, next, complete) = match reply {
                HistoryReply::Page {
                    rows,
                    continuation,
                    complete,
                } => (rows, continuation, complete),
                // Propagated rather than folded into an empty result: the
                // caller decides what to tell nautilus, and it must not be able
                // to mistake this for a window that held nothing.
                HistoryReply::Rejected { reason } => {
                    anyhow::bail!("venue refused a history page: {reason}")
                }
            };
            out.extend(rows);
            // The caller's own ceiling, checked before the venue's completion:
            // stopping here means the window was cut short of its end, which
            // the caller is told about rather than left to infer.
            if enough(&out) {
                return Ok((out, true));
            }
            if complete {
                return Ok((out, false));
            }
            let Some(next) = next else {
                anyhow::bail!("an incomplete history page carried no continuation")
            };
            continuation = Some(next);
        }
    }
}

/// Refuse a symbol this connection was not bound to.
///
/// The symbol is derived from the nautilus `instrument_id`, while
/// `config.symbol` is what the socket named on its upgrade - two sources of
/// truth for one fact. Unreconciled, a host subscribing ES on a socket bound to
/// MNQ would receive MNQ ticks relabelled ES by nautilus, silently, with no
/// frame and no log, and a history request for ES would be answered with MNQ's
/// river. So they must agree case-exactly, matching the venue's own
/// comparison. An absent `config.symbol` takes the venue default and applies no
/// check, which is the pre-carrier behaviour unchanged.
fn ensure_bound_symbol(bound: Option<&str>, symbol: &str, what: &str) -> anyhow::Result<()> {
    if let Some(bound) = bound {
        ensure!(
            symbol == bound,
            "{what} symbol {symbol} does not match the symbol this connection is bound to ({bound})"
        );
    }
    Ok(())
}

/// Resolve the members of a history request to the instrument defs whose rows
/// they will be given, recording a reason for each member that cannot be
/// served: one this connection is not bound to, or one whose def cannot be
/// found. The survivors keep the request's order.
async fn resolve_history_members(
    ids: &[InstrumentId],
    bound: Option<&str>,
    http: &HttpClient,
    http_quota: &HttpQuota,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    failures: &mut Vec<String>,
) -> Vec<(InstrumentId, InstrumentDef)> {
    let mut members = Vec::with_capacity(ids.len());
    for &id in ids {
        let symbol = symbol_from_instrument(id);
        if let Err(err) = ensure_bound_symbol(bound, &symbol, "history") {
            failures.push(format!("{id}: {err:#}"));
            continue;
        }
        match ensure_instrument(http, http_quota, base, instruments, &symbol).await {
            Ok(def) => members.push((id, def)),
            Err(err) => failures.push(format!("{id}: instrument lookup failed: {err:#}")),
        }
    }
    members
}

/// The one event that answers a request: a payload response when nothing
/// failed, otherwise a `DataRequestFailed` naming every failure and carrying
/// whatever payload the caller judged retrieved (see the rule over
/// `request_trades`).
///
/// No failures and no payload is not a state any caller reaches, but it is
/// still answered, as a failure, because a request left unresolved is one the
/// consumer cannot tell from a hung venue.
fn request_event(
    request_id: nautilus_core::UUID4,
    response: Option<DataResponse>,
    failures: &[String],
) -> DataEvent {
    match (failures.is_empty(), response) {
        (true, Some(response)) => DataEvent::Response(response),
        (true, None) => DataEvent::RequestFailed(DataRequestFailed {
            request_id,
            reason: "the request produced no response".to_owned(),
            response: None,
        }),
        (false, response) => {
            let reason = failures.join("; ");
            tracing::error!(%request_id, %reason, "data request failed");
            DataEvent::RequestFailed(DataRequestFailed {
                request_id,
                reason,
                response,
            })
        }
    }
}

/// Answer a registered request from its own task, unless a teardown already
/// answered it. The removal and the send happen under the registry lock, the
/// one `MogwaiDataClient::fail_outstanding_requests` drains under, so exactly
/// one of the two sends; holding a std mutex across the send is sound because
/// it is a synchronous unbounded push with no await and no inline receiver.
/// "Sends" is the whole guarantee: with the sink's receiver gone, nothing can
/// deliver the answer.
fn resolve_request(
    outstanding: &Mutex<std::collections::HashSet<nautilus_core::UUID4>>,
    sink: &EventSender<DataEvent>,
    request_id: nautilus_core::UUID4,
    response: Option<DataResponse>,
    failures: &[String],
) {
    let event = request_event(request_id, response, failures);
    let mut outstanding = lock_recover(outstanding, "outstanding requests");
    if outstanding.remove(&request_id) {
        drop(sink.send(event));
    }
}

/// Hand a history frame to whichever request is waiting on it, or give the
/// message back so the market path can have it.
///
/// An uncorrelated page is dropped with a warning rather than delivered
/// anywhere: it answers a request that has already timed out or been abandoned,
/// and there is no consumer left to give it to.
fn route_history(
    msg: VenueMessage,
    pending: &Arc<Mutex<HashMap<String, tokio::sync::oneshot::Sender<HistoryReply>>>>,
) -> Option<VenueMessage> {
    let (request_id, reply) = match msg {
        VenueMessage::HistoryPage {
            request_id,
            rows,
            continuation,
            complete,
            ..
        } => (
            request_id,
            HistoryReply::Page {
                rows,
                continuation,
                complete,
            },
        ),
        VenueMessage::HistoryRejected {
            request_id, reason, ..
        } => (request_id, HistoryReply::Rejected { reason }),
        other => return Some(other),
    };
    let waiter = pending
        .lock()
        .ok()
        .and_then(|mut pending| pending.remove(&request_id));
    match waiter {
        Some(waiter) => drop(waiter.send(reply)),
        None => tracing::warn!(
            %request_id,
            "history reply arrived for no waiting request; the requester timed out or went away"
        ),
    }
    None
}

#[allow(clippy::too_many_arguments)]
async fn handle_market_message(
    msg: VenueMessage,
    sink: &EventSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    missing_instrument_warnings: &Arc<Mutex<std::collections::HashSet<String>>>,
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    quote_delivery: &Arc<Mutex<()>>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
    sim: SimClock,
) {
    match msg {
        VenueMessage::Trade(trade) => {
            emit_trade(
                &trade,
                sink,
                instruments,
                missing_instrument_warnings,
                subs,
                bars,
                sim,
            );
        }
        VenueMessage::Quote(quote) => {
            handle_quote_message(
                &quote,
                sink,
                instruments,
                missing_instrument_warnings,
                subs,
                quote_delivery,
                sim,
            );
        }
        VenueMessage::FundingRate {
            symbol,
            rate,
            interval_ns,
            next_funding_ns,
            ts_event,
        } => {
            let cached = CachedFunding {
                rate,
                interval_ns,
                next_funding_ns,
                ts_event,
            };
            // The same guard the quote path takes, and the same one for the
            // same reason: a rate replayed by a subscribe must not be overtaken
            // by a rate arriving live while that replay is in flight.
            let _delivery = lock_recover(quote_delivery, "market delivery");
            if retain_funding(subs, &symbol, cached.clone()) {
                let Some(def) = instrument_def(instruments, &symbol) else {
                    warn_missing_instrument_once(missing_instrument_warnings, &symbol);
                    return;
                };
                emit_funding(&cached, sink, &def, sim);
            }
        }
        VenueMessage::Heartbeat { .. } => {
            tracing::trace!("ignoring venue heartbeat on data path");
        }
        VenueMessage::ProtocolError { reason, .. } => {
            // ProtocolError is now narrowed to whole-frame faults the venue
            // could not attribute to any entry (a Subscribe refused at the
            // validation boundary, an unsupported carrier). Per-entry subscribe
            // outcomes arrive as SubscriptionIssues, handled below.
            // Swallowing this here left the feed silent with
            // no downstream signal, indistinguishable from a quiet market.
            // Surface the venue's reason verbatim and do not guess at causes:
            // an earlier version of this line enumerated three candidates, and
            // when a venue restart rewound sim-now under a surviving client's
            // cursor the enumeration named only wrong ones - at ~11 `warn`/s it
            // was the loudest thing in the log, pointing every operator at a
            // phantom subscription bug instead of the venue bounce.
            tracing::warn!(
                %reason,
                "venue reported a protocol error on the data path; the reason is the venue's own diagnosis"
            );
        }
        VenueMessage::FeedLagged {
            episode,
            skipped,
            skipped_total,
            after_ts_event,
            resumed_ts_event,
        } => {
            // `error`, not `warn`, and the level is a ruling rather than taste.
            // `FeedLagged` is the venue declaring that this client's tape has
            // a hole: the live bar aggregator folds across it and closes bars
            // over trades it never saw, downstream state built from the pushed
            // stream is short by exactly the skipped rows, and nothing
            // downstream can tell the difference
            // between a quiet market and a dropped one. The standing
            // preference is an assert, a type or a guard over a verification,
            // and none of the three is available here: nautilus's `DataEvent`
            // carries no gap or degradation variant (`Data`, `Response`,
            // `RequestFailed` and `ResponseEnvelope`, the last three scoped to
            // a correlated request, plus two `defi`-gated pool variants), the
            // client is handed to the host as a `dyn DataClient`
            // with no downcast, and fabricating an `InstrumentStatus` would
            // report a venue halt that did not happen. Refusing - tearing the
            // socket down - would turn a recoverable gap into a total outage
            // and invent a policy the venue did not ask for. So the loudest
            // honest channel is the log, at the level a host alerts on. The
            // real fix is a declared feed-gap event upstream: a `DataEvent`
            // variant or a `DataClient` health callback the engine surfaces, so
            // a gap reaches the host as an event rather than as a log line.
            // Nothing in this repository can supply it.
            //
            // The boundaries are the actionable part for whoever reads this
            // log: they delimit the affected span, so a bar folded across it,
            // or any downstream state accumulated through it, can be identified
            // rather than guessed at. A history request over the same span is
            // the recovery available: it re-synthesizes this passenger's own
            // river from the generator rather than replaying the pushed stream,
            // so a span already behind the passenger's present comes back whole. `episode` and `skipped_total` separate a client that
            // fell behind once from one that cannot keep up at all - the second
            // is a sizing problem, not an incident.
            tracing::error!(
                episode,
                skipped,
                skipped_total,
                after_ts_event,
                resumed_ts_event,
                "venue declared a gap in this client's market view; downstream aggregation is wrong between the two boundaries"
            );
        }
        VenueMessage::RunComplete {
            sim_now_ns,
            elapsed_ns,
        } => {
            // The lifecycle owns the terminal transition and suppresses its
            // reconnect.  Keep an explicit completion record on the data leg
            // so a finished run is never mistaken for a quiet failed feed.
            tracing::info!(sim_now_ns, elapsed_ns, "venue run completed on data socket");
        }
        VenueMessage::PassengerDurationComplete {
            sim_now_ns,
            elapsed_ns,
            declared_duration_ns,
        } => {
            // Its own record, and deliberately not folded into the arm above. The run may
            // still be going for everyone else, so logging this as a completed
            // run would tell an operator the venue had finished when only this
            // socket had. Both spans are reported because they can differ: the
            // deadline is what was asked for, the elapsed span is what this
            // passenger actually observed on its boat clock.
            tracing::info!(
                sim_now_ns,
                elapsed_ns,
                declared_duration_ns,
                "this data socket's own declared duration elapsed; the run continues"
            );
        }
        _ => {}
    }
}

fn handle_quote_message(
    quote: &mogwai_protocol::QuoteTick,
    sink: &EventSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    missing_instrument_warnings: &Arc<Mutex<std::collections::HashSet<String>>>,
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    quote_delivery: &Arc<Mutex<()>>,
    sim: SimClock,
) {
    let _delivery = lock_recover(quote_delivery, "quote delivery");
    if !retain_quote(subs, quote) {
        return;
    }
    let Some(def) = instrument_def(instruments, &quote.symbol) else {
        warn_missing_instrument_once(missing_instrument_warnings, &quote.symbol);
        return;
    };
    emit_quote(quote, sink, &def, sim);
}

/// Cache one market fact against its symbol's row, and answer whether that
/// symbol has a live subscription of the kind that fact feeds.
///
/// One helper rather than one per fact, and the sharing is the point: a
/// pre-subscription BBO or funding rate must survive until the local
/// subscription is activated, but an arbitrary wire symbol must not grow this
/// map forever. One run serves one configured instrument, so more than this
/// many orphan symbols is malformed input and is dropped without allocating a
/// row. Both caches live in the same map, so they must be bounded by one count
/// over one predicate - a second bound of its own would let either path push
/// the shared orphan set past what the other expects.
///
/// The bound counts only orphans - rows no subscription of any kind refers to -
/// so a client with many live subscriptions cannot crowd itself out of the
/// pre-subscription cache. Nothing here evicts: an existing row is always
/// updated, so a symbol that once got a cache row can never lose it to a flood
/// of junk symbols arriving after it.
fn retain_market_fact(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    symbol: &Symbol,
    what: &'static str,
    write: impl FnOnce(&mut SubState) -> bool,
) -> bool {
    const MAX_ORPHAN_MARKET_SYMBOLS: usize = 64;
    let mut states = lock_recover(subs, "subscriptions");
    let orphans = states.values().filter(|state| state.total() == 0).count();
    if !states.contains_key(symbol) && orphans >= MAX_ORPHAN_MARKET_SYMBOLS {
        tracing::warn!(symbol = %symbol, "dropping {what}: orphan market cache is full");
        return false;
    }
    write(states.entry(std::sync::Arc::clone(symbol)).or_default())
}

fn retain_quote(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    quote: &mogwai_protocol::QuoteTick,
) -> bool {
    retain_market_fact(subs, &quote.symbol, "quote", |state| {
        state.cached_quote = Some(quote.clone());
        state.quotes > 0
    })
}

fn retain_funding(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    symbol: &Symbol,
    funding: CachedFunding,
) -> bool {
    retain_market_fact(subs, symbol, "funding rate", |state| {
        state.cached_funding = Some(funding);
        state.funding > 0
    })
}

fn emit_funding(
    funding: &CachedFunding,
    sink: &EventSender<DataEvent>,
    def: &InstrumentDef,
    sim: SimClock,
) {
    let id = match convert::instrument_id(def) {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(symbol = %def.symbol, error = %err, "dropping funding rate: unrepresentable instrument symbol");
            return;
        }
    };
    let update = convert::funding_rate_update(
        id,
        funding.rate,
        funding.interval_ns,
        funding.next_funding_ns,
        funding.ts_event,
        now_unix_nanos(sim),
    );
    drop(sink.send(DataEvent::Data(Data::FundingRate(update))));
}

fn emit_quote(
    quote: &mogwai_protocol::QuoteTick,
    sink: &EventSender<DataEvent>,
    def: &InstrumentDef,
    sim: SimClock,
) {
    let id = match convert::instrument_id(def) {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(
                symbol = %quote.symbol,
                error = %err,
                "dropping quote: unrepresentable instrument symbol"
            );
            return;
        }
    };
    match convert::quote_tick(quote, id, def, now_unix_nanos(sim)) {
        Ok(tick) => drop(sink.send(DataEvent::Data(Data::Quote(tick)))),
        Err(err) => tracing::warn!(
            symbol = %quote.symbol,
            error = %err,
            "dropping quote: unrepresentable tick"
        ),
    }
}

fn emit_trade(
    trade: &TradeTick,
    sink: &EventSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    missing_instrument_warnings: &Arc<Mutex<std::collections::HashSet<String>>>,
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
    sim: SimClock,
) {
    let Some(def) = instrument_def(instruments, &trade.symbol) else {
        warn_missing_instrument_once(missing_instrument_warnings, &trade.symbol);
        return;
    };
    let state = sub_state(subs, &trade.symbol);
    let id = match convert::instrument_id(&def) {
        Ok(id) => id,
        Err(err) => {
            tracing::warn!(
                symbol = %trade.symbol,
                error = %err,
                "dropping trade: unrepresentable instrument symbol"
            );
            return;
        }
    };
    if state.as_ref().is_some_and(|s| s.trades > 0) {
        match convert::trade_tick(trade, id, &def, now_unix_nanos(sim)) {
            Ok(tick) => drop(sink.send(DataEvent::Data(Data::Trade(tick)))),
            Err(err) => tracing::warn!(
                symbol = %trade.symbol,
                ts_event = trade.ts_event,
                error = %err,
                "dropping trade: unrepresentable tick"
            ),
        }
    }
    if state.as_ref().is_some_and(|s| s.bars > 0) {
        emit_live_bars(trade, id, &def, sink, bars, sim);
    }
}

fn emit_live_bars(
    trade: &mogwai_protocol::TradeTick,
    id: InstrumentId,
    def: &InstrumentDef,
    sink: &EventSender<DataEvent>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
    sim: SimClock,
) {
    let mut ready = Vec::new();
    {
        let mut bars = lock_recover(bars, "bar");
        for (bar_type, state) in bars.iter_mut() {
            if bar_type.instrument_id() != id || state.refs == 0 {
                continue;
            }
            if let Some(bar) = state.window.push(trade, def, now_unix_nanos(sim)) {
                ready.push(bar);
            }
        }
    }
    for bar in ready {
        drop(sink.send(DataEvent::Data(Data::Bar(bar))));
    }
}

/// Converts a page of historical wire trades for a `TradesResponse`, stamping
/// each tick `ts_init = ts_event`. History must never stamp the conversion
/// clock: the response's `end` is pinned before the asynchronous paging runs,
/// and nautilus's `DataResponse::trim_to_bounds` trims vector payloads on
/// `ts_init` - so a conversion-time stamp lands past `end` and every history
/// request comes back empty at the data engine.
///
/// Only rows labelled with the def's own symbol are converted. A history page
/// carries the socket's whole river, and stamping every row with the requested
/// `instrument_id` regardless of its label would hand one member another
/// label's water under its own name.
fn historical_trades(
    trades: &[mogwai_protocol::TradeTick],
    instrument_id: InstrumentId,
    def: &InstrumentDef,
) -> Vec<nautilus_model::data::TradeTick> {
    trades
        .iter()
        .filter(|t| t.symbol == def.symbol)
        .filter_map(|t| {
            convert::trade_tick(t, instrument_id, def, UnixNanos::from(t.ts_event))
                .map_err(|err| {
                    tracing::warn!(
                        symbol = %t.symbol,
                        ts_event = t.ts_event,
                        error = %err,
                        "dropping historical trade: unrepresentable tick"
                    );
                })
                .ok()
        })
        .collect()
}

/// The quote twin of `historical_trades`, for the same reasons.
fn historical_quotes(
    quotes: &[mogwai_protocol::QuoteTick],
    instrument_id: InstrumentId,
    def: &InstrumentDef,
) -> Vec<nautilus_model::data::QuoteTick> {
    quotes
        .iter()
        .filter(|quote| quote.symbol == def.symbol)
        .filter_map(|quote| {
            convert::quote_tick(quote, instrument_id, def, UnixNanos::from(quote.ts_event))
                .map_err(|err| {
                    tracing::warn!(
                        symbol = %quote.symbol,
                        ts_event = quote.ts_event,
                        error = %err,
                        "dropping historical quote: unrepresentable tick"
                    );
                })
                .ok()
        })
        .collect()
}

fn sub_state(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    symbol: &str,
) -> Option<SubStateSnapshot> {
    lock_recover(subs, "subscription")
        .get(symbol)
        .map(SubStateSnapshot::from)
}
struct SubStateSnapshot {
    trades: usize,
    bars: usize,
}
impl From<&SubState> for SubStateSnapshot {
    fn from(state: &SubState) -> Self {
        Self {
            trades: state.trades,
            bars: state.bars,
        }
    }
}
// The HTTP history carrier is gone from this client entirely, and with it the whole
// timestamp-cursor apparatus that made it survivable: the paged trade and quote
// fetchers, the per-request trade ceiling and page cap, and
// `final_ts_group_start`, which existed because a timestamp-only cursor cannot
// prove a full page's trailing group complete and so had to cut before it. That
// cut had a failure mode with no answer - a page that was one whole timestamp
// could not advance at all, and returned a short history a bar-folding consumer
// could not tell from a quiet window.
//
// None of it has an equivalent here. The venue issues an opaque continuation,
// so the position is its own bookkeeping rather than a timestamp this side
// reconstructs, and there is no trailing group for a consumer to reason about.
/// Whether the rows in hand already span the bar intervals the caller asked
/// for.
///
/// Reads the history rows directly rather than a converted trade slice: the
/// predicate runs once per page against everything collected so far, and
/// converting the whole accumulation on each call would make the session
/// quadratic in its own length for a question that only needs two timestamps.
fn bar_span_reached(
    rows: &[mogwai_protocol::HistoryRow],
    interval: u64,
    limit: Option<usize>,
) -> bool {
    match (rows.first(), rows.last(), limit) {
        (Some(first), Some(last), Some(limit)) if interval > 0 => {
            usize::try_from(
                (last.ts_event() / interval).saturating_sub(first.ts_event() / interval),
            )
            .unwrap_or(usize::MAX)
                >= limit
        }
        _ => false,
    }
}
#[cfg(test)]
mod quote_cache_tests {
    use super::*;
    use nautilus_core::UUID4;
    use rust_decimal::Decimal;

    fn quote_for(symbol: &str, ts_event: u64) -> mogwai_protocol::QuoteTick {
        mogwai_protocol::QuoteTick {
            symbol: symbol.into(),
            bid_px: Decimal::from(99),
            ask_px: Decimal::from(100),
            bid_sz: Decimal::ONE,
            ask_sz: Decimal::ONE,
            ts_event,
        }
    }

    fn client_bound_to(symbol: Option<&str>) -> MogwaiDataClient {
        let config = MogwaiDataClientConfig {
            account_id: nautilus_model::identifiers::AccountId::from("MOGWAI-001"),
            base_url: "ws://127.0.0.1:1".into(),
            symbol: symbol.map(str::to_owned),
            ..MogwaiDataClientConfig::default()
        };
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client")
    }

    fn trade_for(symbol: &str, ts_event: u64) -> mogwai_protocol::TradeTick {
        mogwai_protocol::TradeTick {
            symbol: symbol.into(),
            price: Decimal::from(100),
            size: Decimal::ONE,
            aggressor: mogwai_protocol::AggressorSide::Buyer,
            ts_event,
        }
    }

    /// The warmup-killing defect: a history response's `end` is pinned before
    /// the asynchronous paging runs, and the payload used to be stamped
    /// `ts_init = now` at conversion - strictly after that `end` - so
    /// nautilus's `DataResponse::trim_to_bounds`, which trims vector payloads
    /// on `ts_init` (not `ts_event`), removed every item and the data engine
    /// delivered an empty response to the session. These three tests run the
    /// actual nautilus trim over the actual builders, because a test that
    /// inspected the response before the trim was exactly the vacuous
    /// coverage that let the defect ship.
    #[test]
    fn historical_bars_survive_the_nautilus_trim_on_a_pinned_end() {
        let def = mogwai_protocol::default_instruments().remove(0);
        let minute = 60_000_000_000u64;
        let trades: Vec<mogwai_protocol::TradeTick> = [10, 70, 130]
            .iter()
            .map(|s| trade_for(&def.symbol, s * 1_000_000_000))
            .collect();
        let bar_type =
            BarType::from(format!("{}.MOGWAI-1-MINUTE-LAST-EXTERNAL", def.symbol).as_str());
        let end = UnixNanos::from(3 * minute);
        let bars =
            aggregate_bars(&bar_type, &trades, &def, Some(end)).expect("a minute bar is admitted");
        assert_eq!(bars.len(), 3, "three closed minute windows");
        let response = BarsResponse::new(
            nautilus_core::UUID4::new(),
            ClientId::from("MOGWAI-DATA"),
            bar_type,
            bars,
            Some(UnixNanos::from(0u64)),
            Some(end),
            // The response-time stamp the paging produces: far past `end`.
            UnixNanos::from(10 * minute),
            None,
        );
        let mut response = DataResponse::Bars(response);
        response.trim_to_bounds();
        let DataResponse::Bars(trimmed) = response else {
            unreachable!()
        };
        assert_eq!(
            trimmed.data.len(),
            3,
            "historical bars must carry ts_init within the pinned window or nautilus trims them all"
        );
        for bar in trimmed.data.iter() {
            assert_eq!(
                bar.ts_init, bar.ts_event,
                "a historical bar's ts_init is its own close"
            );
        }
    }

    #[test]
    fn historical_trades_survive_the_nautilus_trim_on_a_pinned_end() {
        let def = mogwai_protocol::default_instruments().remove(0);
        let id = InstrumentId::from(format!("{}.MOGWAI", def.symbol).as_str());
        let end = UnixNanos::from(100_000_000_000u64);
        let trades: Vec<mogwai_protocol::TradeTick> = [10u64, 40, 90]
            .iter()
            .map(|s| trade_for(&def.symbol, s * 1_000_000_000))
            .collect();
        let data = historical_trades(&trades, id, &def);
        let response = TradesResponse::new(
            nautilus_core::UUID4::new(),
            ClientId::from("MOGWAI-DATA"),
            id,
            data,
            Some(UnixNanos::from(0u64)),
            Some(end),
            UnixNanos::from(500_000_000_000u64),
            None,
        );
        let mut response = DataResponse::Trades(response);
        response.trim_to_bounds();
        let DataResponse::Trades(trimmed) = response else {
            unreachable!()
        };
        assert_eq!(
            trimmed.data.len(),
            3,
            "event-stamped ts_init keeps history inside the window"
        );
    }

    #[test]
    fn historical_quotes_survive_the_nautilus_trim_on_a_pinned_end() {
        let def = mogwai_protocol::default_instruments().remove(0);
        let id = InstrumentId::from(format!("{}.MOGWAI", def.symbol).as_str());
        let end = UnixNanos::from(100_000_000_000u64);
        let quotes: Vec<mogwai_protocol::QuoteTick> = [10u64, 40, 90]
            .iter()
            .map(|s| quote_for(&def.symbol, s * 1_000_000_000))
            .collect();
        let data = historical_quotes(&quotes, id, &def);
        let response = QuotesResponse::new(
            nautilus_core::UUID4::new(),
            ClientId::from("MOGWAI-DATA"),
            id,
            data,
            Some(UnixNanos::from(0u64)),
            Some(end),
            UnixNanos::from(500_000_000_000u64),
            None,
        );
        let mut response = DataResponse::Quotes(response);
        response.trim_to_bounds();
        let DataResponse::Quotes(trimmed) = response else {
            unreachable!()
        };
        assert_eq!(
            trimmed.data.len(),
            3,
            "event-stamped ts_init keeps history inside the window"
        );
    }

    #[test]
    fn subscribe_refuses_an_instrument_outside_the_bound_symbol() {
        let client = client_bound_to(Some("MNQ"));
        let error = client
            .subscribe_symbol(Arc::from("MES"), SubKind::Trades)
            .expect_err("subscription must match the socket river");
        assert!(error.to_string().contains("bound to (MNQ)"), "{error}");
    }

    #[test]
    fn subscribe_without_a_config_symbol_keeps_the_venue_default_behavior() {
        let client = client_bound_to(None);
        client
            .subscribe_symbol(Arc::from("MES"), SubKind::Trades)
            .expect("an absent binding applies no client-side check");
    }

    /// The bar ref and the per-symbol bars count are one subscription in
    /// two counters, and a refused `subscribe_bars` must leave both at zero.
    /// A ref surviving the refusal makes a later `unsubscribe_bars` for that
    /// bar type "match", so it spends a symbol decrement belonging to another
    /// bar type's live subscription.
    #[test]
    fn a_refused_bar_subscription_leaves_no_ref_behind() {
        let mut client = client_bound_to(Some("MNQ"));
        let cmd = SubscribeBars::new(
            BarType::from("MES.MOGWAI-1-MINUTE-LAST-EXTERNAL"),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        );
        let error = client
            .subscribe_bars(cmd)
            .expect_err("a bar type outside the bound river cannot be served");
        assert!(error.to_string().contains("bound to (MNQ)"), "{error}");
        assert!(
            lock_recover(&client.bars, "bar").is_empty(),
            "the refused subscription must leave no bar ref standing"
        );
        assert!(
            lock_recover(&client.subs, "subscriptions").is_empty(),
            "and no symbol subscription either"
        );
    }

    fn mogwai_bar(symbol: &str, minutes: u64) -> BarType {
        BarType::from(format!("{symbol}.MOGWAI-{minutes}-MINUTE-LAST-EXTERNAL").as_str())
    }

    fn mogwai_id(symbol: &str) -> InstrumentId {
        InstrumentId::from(format!("{symbol}.MOGWAI").as_str())
    }

    fn accepted_bars(error: &anyhow::Error) -> Vec<BarType> {
        error
            .downcast_ref::<nautilus_common::messages::data::SubscriptionError>()
            .unwrap_or_else(|| panic!("a partial batch must report its accepted members: {error}"))
            .bar_types_accepted
            .clone()
    }

    fn accepted_ids(error: &anyhow::Error) -> Vec<InstrumentId> {
        error
            .downcast_ref::<nautilus_common::messages::data::SubscriptionError>()
            .unwrap_or_else(|| panic!("a partial batch must report its accepted members: {error}"))
            .instrument_ids_accepted
            .clone()
    }

    /// A bar batch with a refused member in the middle.
    ///
    /// The members on either side of the refusal stand, the refused one leaves
    /// nothing behind in either counter, and the engine is told exactly which
    /// two were accepted - that listing is the only way its own subscription
    /// set comes to match this client's. Three ways to get this wrong, each
    /// caught by a different assertion: a loop that stopped at the refusal
    /// never subscribes the third member and returns a bare error; a member
    /// without its rollback leaves the refused bar type's ref standing; and a
    /// bare error, even after a complete loop, tells the engine nothing was
    /// accepted. Then the surviving feed is released past the refused member,
    /// to show the refusal cannot be spent against it.
    #[test]
    fn a_bar_batch_with_a_refused_member_keeps_the_rest_and_reports_them() {
        let mut client = client_bound_to(Some("MNQ"));
        let one = mogwai_bar("MNQ", 1);
        let refused = mogwai_bar("MES", 1);
        let five = mogwai_bar("MNQ", 5);
        let cmd = SubscribeBars::new(
            vec![one, refused, five],
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        );

        let error = client
            .subscribe_bars(cmd)
            .expect_err("a member outside the bound river fails the batch");
        assert!(error.to_string().contains("bound to (MNQ)"), "{error}");
        assert_eq!(
            accepted_bars(&error),
            vec![one, five],
            "exactly the applied members are reported, in order"
        );
        {
            let bars = lock_recover(&client.bars, "bar");
            assert!(
                !bars.contains_key(&refused),
                "the refused member must leave no bar ref standing"
            );
            assert_eq!(bars.get(&one).map(|state| state.refs), Some(1));
            assert_eq!(
                bars.get(&five).map(|state| state.refs),
                Some(1),
                "a member after the refusal is still applied"
            );
        }
        {
            let subs = lock_recover(&client.subs, "subscriptions");
            assert_eq!(
                subs.get("MNQ").map(|state| state.bars),
                Some(2),
                "the symbol count moves with exactly the two accepted refs"
            );
            assert!(!subs.contains_key("MES"));
        }

        // Release the refused member and one survivor together. The refused
        // one has nothing to release and must not spend a decrement; the
        // other survivor's feed must stay live.
        let release = UnsubscribeBars::new(
            vec![refused, one],
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        );
        client
            .unsubscribe_bars(&release)
            .expect("releasing an unheld bar type is not an error");
        assert_eq!(
            lock_recover(&client.subs, "subscriptions")
                .get("MNQ")
                .map(|state| state.bars),
            Some(1),
            "the surviving bar type keeps its feed"
        );
        let bars = lock_recover(&client.bars, "bar");
        assert!(!bars.contains_key(&one));
        assert_eq!(bars.get(&five).map(|state| state.refs), Some(1));
    }

    /// The instrument-keyed twin, across all three kinds that share the
    /// contract. The refused member is first, so a loop that stopped at it
    /// would apply nothing at all.
    #[test]
    fn quote_trade_and_funding_batches_with_a_refused_member_keep_the_rest() {
        let mut client = client_bound_to(Some("MNQ"));
        let refused = mogwai_id("MES");
        let served = mogwai_id("MNQ");
        let client_id = Some(ClientId::from("MOGWAI-DATA"));

        let error = client
            .subscribe_quotes(SubscribeQuotes::new(
                vec![refused, served],
                client_id,
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect_err("quotes: the refused member fails the batch");
        assert_eq!(accepted_ids(&error), vec![served], "quotes");

        let error = client
            .subscribe_trades(SubscribeTrades::new(
                vec![refused, served],
                client_id,
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect_err("trades: the refused member fails the batch");
        assert_eq!(accepted_ids(&error), vec![served], "trades");

        let error = client
            .subscribe_funding_rates(SubscribeFundingRates::new(
                vec![refused, served],
                client_id,
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect_err("funding: the refused member fails the batch");
        assert_eq!(accepted_ids(&error), vec![served], "funding");

        {
            let subs = lock_recover(&client.subs, "subscriptions");
            let state = subs.get("MNQ").expect("the served member was applied");
            assert_eq!(
                (state.quotes, state.trades, state.funding),
                (1, 1, 1),
                "each served member counted once per kind"
            );
            assert!(!subs.contains_key("MES"), "the refused member left no row");
        }

        // And the release side: the whole batch releases, and the served
        // member's counts return to zero rather than being left or overspent.
        client
            .unsubscribe_trades(&UnsubscribeTrades::new(
                vec![refused, served],
                client_id,
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("releasing a batch is accepted whole");
        let subs = lock_recover(&client.subs, "subscriptions");
        let state = subs
            .get("MNQ")
            .expect("quotes and funding still hold the row");
        assert_eq!((state.quotes, state.trades, state.funding), (1, 0, 1));
    }

    /// The data client's teardown, which had none of the execution client's.
    ///
    /// `stop()` aborts the reader, so nothing will ever answer a history page
    /// that is in flight when it lands: the waiter used to park for the whole
    /// havoc-scaled request timeout - 30 s by default - and then give up,
    /// while `history_session()` kept handing out a sender for a dead
    /// generation. Both halves are asserted here, and the ordering matters as
    /// much as the clearing: `ws_cmd` is dropped before the abort, so a request
    /// racing `stop()` cannot enqueue onto a receiver that is already going
    /// away.
    #[test]
    fn stopping_the_data_client_retires_its_command_sender_and_its_history_waiters() {
        let mut client = client_bound_to(None);
        let (cmd_tx, _cmd_rx) = unbounded_channel();
        client.ws_cmd = Some(cmd_tx);
        assert!(
            client.history_session().is_ok(),
            "connected before the stop"
        );
        let (reply_tx, mut reply_rx) = tokio::sync::oneshot::channel();
        lock_recover(&client.pending_history, "test pending").insert("R-1".to_owned(), reply_tx);

        client.stop().expect("stop is infallible here");

        // `try_recv`, not a blocking wait: the defect's signature is a waiter
        // that parks, so a test that waits for it reproduces the hang instead
        // of reporting it.
        assert!(
            matches!(
                reply_rx.try_recv(),
                Err(tokio::sync::oneshot::error::TryRecvError::Closed)
            ),
            "an abandoned history waiter must fail fast rather than wait out its timeout"
        );
        assert!(
            lock_recover(&client.pending_history, "test pending").is_empty(),
            "the pending table must not carry a dead generation's waiters"
        );
        let Err(error) = client.history_session() else {
            panic!("a stopped client has no river to page");
        };
        assert!(error.to_string().contains("not connected"), "{error}");
    }

    /// A request whose task is aborted by `stop()` is answered by `stop()`.
    ///
    /// Every `request_*` task is tracked and aborted with the rest, and an
    /// aborted task sends nothing, so a request in flight at teardown used to
    /// stay pending at the engine until the consumer's own timeout - while the
    /// handlers' doc said every request resolves. The answer is a failure with
    /// no payload, and it is on the sink by the time `stop()` returns, which
    /// is why this reads with `try_recv`.
    #[test]
    fn stopping_the_data_client_answers_the_requests_its_tasks_owed() {
        let mut client = client_bound_to(None);
        let (tx, mut rx) = unbounded_channel::<DataEvent>();
        client.sink = Some(tx.into());
        let owed = UUID4::new();
        client.register_request(owed);

        client.stop().expect("stop is infallible here");

        match rx.try_recv() {
            Ok(DataEvent::RequestFailed(failure)) => {
                assert_eq!(failure.request_id, owed);
                assert!(failure.response.is_none(), "a teardown retrieved nothing");
                assert!(failure.reason.contains("stopped"), "{}", failure.reason);
            }
            other => panic!("the owed request must be answered as failed, got {other:?}"),
        }
        assert!(rx.try_recv().is_err(), "answered exactly once");
        assert!(
            lock_recover(&client.outstanding_requests, "test").is_empty(),
            "nothing is left owed"
        );
    }

    /// The other half of the ownership rule: whichever side removes the id
    /// answers, and the other sends nothing, in both orders.
    #[test]
    fn a_request_is_answered_once_whichever_side_wins() {
        let client = {
            let mut client = client_bound_to(None);
            let (tx, rx) = unbounded_channel::<DataEvent>();
            client.sink = Some(tx.into());
            (client, rx)
        };
        let (client, mut rx) = client;
        let sink = client.sink().expect("sink");

        // The task wins: it answers, and a later teardown finds nothing.
        let first = UUID4::new();
        client.register_request(first);
        resolve_request(
            &client.outstanding_requests,
            &sink,
            first,
            None,
            &["boom".to_owned()],
        );
        client.fail_outstanding_requests("teardown");
        let Ok(DataEvent::RequestFailed(failure)) = rx.try_recv() else {
            panic!("the task's own answer goes out");
        };
        assert_eq!(failure.reason, "boom");
        assert!(
            rx.try_recv().is_err(),
            "the teardown must not answer it again"
        );

        // The teardown wins: it answers, and the late task sends nothing.
        let second = UUID4::new();
        client.register_request(second);
        client.fail_outstanding_requests("teardown");
        resolve_request(
            &client.outstanding_requests,
            &sink,
            second,
            None,
            &["late".to_owned()],
        );
        let Ok(DataEvent::RequestFailed(failure)) = rx.try_recv() else {
            panic!("the teardown's answer goes out");
        };
        assert_eq!(failure.reason, "teardown");
        assert!(
            rx.try_recv().is_err(),
            "the late task must not answer it again"
        );
    }

    /// A history member outside the bound river is a member failure.
    ///
    /// The history path used to have no bound-symbol check at all, and the
    /// rows were stamped with whatever id was asked for, so on a socket bound
    /// to MNQ a request for MES came back as MNQ's river relabelled MES - a
    /// full, plausible, wrong window. The refusal comes before any lookup.
    #[tokio::test]
    async fn a_history_member_outside_the_bound_river_fails_without_a_lookup() {
        let client = client_bound_to(Some("MNQ"));
        let mut failures = Vec::new();
        let members = resolve_history_members(
            &[mogwai_id("MES")],
            client.config.symbol.as_deref(),
            &client.http,
            &client.http_quota,
            &client.config.http_base_url(),
            &client.instruments,
            &mut failures,
        )
        .await;
        assert!(members.is_empty(), "the refused member is not served");
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("bound to (MNQ)"), "{failures:?}");
    }

    /// A quote member is accepted the moment its counter moves, so nothing
    /// after the increment may refuse it. With a cached book and no sink, the
    /// replay used to `?` out after the count had moved: the member was
    /// reported refused while holding a count the engine would never release.
    #[test]
    fn a_quote_member_with_no_sink_for_its_replay_is_still_accepted() {
        let mut client = client_bound_to(None);
        // Any class will do: the replay only needs the def to be present.
        let def = InstrumentDef {
            symbol: "MNQ".into(),
            class: mogwai_protocol::InstrumentClass::Perpetual {
                underlying: "MNQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::ONE,
                asset_class: mogwai_protocol::WireAssetClass::Index,
                funding_interval_ns: 28_800_000_000_000,
                funding_rate: Decimal::ZERO,
                index_symbol: None,
                funding_clamp: Decimal::ZERO,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        };
        lock_recover(&client.instruments, "test").insert(Arc::clone(&def.symbol), def);
        assert!(!retain_quote(&client.subs, &quote_for("MNQ", 7)));
        assert!(client.sink.is_none());

        client
            .subscribe_quotes(SubscribeQuotes::new(
                mogwai_id("MNQ"),
                Some(ClientId::from("MOGWAI-DATA")),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("the member is accepted even though its replay had nowhere to go");
        assert_eq!(
            lock_recover(&client.subs, "test")
                .get("MNQ")
                .map(|state| state.quotes),
            Some(1)
        );
    }

    #[test]
    fn a_quote_cached_before_its_instrument_resolves_is_still_replayed() {
        let subs = Arc::new(Mutex::new(HashMap::new()));
        assert!(!retain_quote(&subs, &quote_for("LATE", 7)));
        let cached = lock_recover(&subs, "test")
            .get("LATE")
            .and_then(|state| state.cached_quote.clone())
            .expect("cache does not depend on an instrument definition");
        assert_eq!(cached.ts_event, 7);
    }

    #[tokio::test]
    async fn funding_frames_are_gated_cached_and_published() {
        let def = InstrumentDef {
            symbol: "BTCUSDT.P".into(),
            class: mogwai_protocol::InstrumentClass::Perpetual {
                underlying: "BTC".into(),
                settlement_currency: "USDT".into(),
                multiplier: rust_decimal::Decimal::ONE,
                asset_class: mogwai_protocol::WireAssetClass::Cryptocurrency,
                funding_interval_ns: 28_800_000_000_000,
                funding_rate: rust_decimal::Decimal::new(1, 4),
                index_symbol: None,
                funding_clamp: rust_decimal::Decimal::ZERO,
            },
            price_precision: 2,
            size_precision: 3,
            price_increment: rust_decimal::Decimal::new(1, 2),
            size_increment: rust_decimal::Decimal::new(1, 3),
        };
        let symbol = Arc::clone(&def.symbol);
        let instruments = Arc::new(Mutex::new(HashMap::from([(Arc::clone(&symbol), def)])));
        let warnings = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let subs = Arc::new(Mutex::new(HashMap::new()));
        let delivery = Arc::new(Mutex::new(()));
        let bars = Arc::new(Mutex::new(HashMap::new()));
        let (sink, mut events) = unbounded_channel();
        let sink = EventSender::from(sink);
        let frame = VenueMessage::FundingRate {
            symbol: Arc::clone(&symbol),
            rate: rust_decimal::Decimal::new(125, 6),
            interval_ns: 28_800_000_000_000,
            next_funding_ns: 57_600_000_000_000,
            ts_event: 28_800_000_000_000,
        };

        handle_market_message(
            frame.clone(),
            &sink,
            &instruments,
            &warnings,
            &subs,
            &delivery,
            &bars,
            SimClock::identity(),
        )
        .await;
        assert!(
            events.try_recv().is_err(),
            "an unsubscribed rate was forwarded"
        );
        assert!(
            lock_recover(&subs, "test")[&symbol]
                .cached_funding
                .is_some()
        );

        lock_recover(&subs, "test")
            .get_mut(&symbol)
            .unwrap()
            .increment(SubKind::Funding);
        handle_market_message(
            frame,
            &sink,
            &instruments,
            &warnings,
            &subs,
            &delivery,
            &bars,
            SimClock::identity(),
        )
        .await;
        let DataEvent::Data(Data::FundingRate(update)) = events.try_recv().unwrap() else {
            panic!("subscribed funding rate was not published");
        };
        assert_eq!(update.rate, rust_decimal::Decimal::new(125, 6));
        assert_eq!(update.interval, Some(480));
    }

    #[test]
    fn orphan_quote_cache_is_bounded() {
        let subs = Arc::new(Mutex::new(HashMap::new()));
        for i in 0..100 {
            retain_quote(&subs, &quote_for(&format!("S{i}"), i));
        }
        assert_eq!(lock_recover(&subs, "test").len(), 64);
    }

    #[test]
    fn orphan_funding_cache_is_bounded_without_freezing_existing_rows() {
        let subs = Arc::new(Mutex::new(HashMap::new()));
        let cached = |ts_event| CachedFunding {
            rate: rust_decimal::Decimal::new(ts_event as i64, 6),
            interval_ns: 60_000_000_000,
            next_funding_ns: ts_event + 60_000_000_000,
            ts_event,
        };
        for i in 0..64 {
            assert!(!retain_funding(&subs, &format!("S{i}").into(), cached(i)));
        }
        assert!(!retain_funding(&subs, &"LATE".into(), cached(65)));
        assert!(!lock_recover(&subs, "test").contains_key("LATE"));
        assert!(!retain_funding(&subs, &"S0".into(), cached(99)));
        assert_eq!(
            lock_recover(&subs, "test")["S0"]
                .cached_funding
                .as_ref()
                .unwrap()
                .ts_event,
            99
        );

        let mut states = lock_recover(&subs, "test");
        states.get_mut("S0").unwrap().increment(SubKind::Funding);
        assert_eq!(states["S0"].total(), 1);
    }

    #[test]
    fn a_live_quote_cannot_overtake_the_replayed_book() {
        let config = MogwaiDataClientConfig {
            account_id: nautilus_model::identifiers::AccountId::from("MOGWAI-001"),
            base_url: "ws://127.0.0.1:1/ws".into(),
            ..MogwaiDataClientConfig::default()
        };
        let mut client = MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).unwrap();
        let (sink_tx, mut sink_rx) = unbounded_channel();
        let sink_tx = EventSender::from(sink_tx);
        client.sink = Some(sink_tx.clone());
        let def = mogwai_protocol::default_instruments().remove(0);
        client
            .instruments
            .lock()
            .unwrap()
            .insert(std::sync::Arc::clone(&def.symbol), def);
        assert!(!retain_quote(&client.subs, &quote_for("BTCUSDT", 1)));

        let subscribed = Arc::new(std::sync::Barrier::new(2));
        let release = Arc::new(std::sync::Barrier::new(2));
        let publisher_started = Arc::new(std::sync::Barrier::new(2));
        let live_instruments = Arc::clone(&client.instruments);
        let live_warnings = Arc::clone(&client.missing_instrument_warnings);
        let live_subs = Arc::clone(&client.subs);
        let live_delivery = Arc::clone(&client.quote_delivery);
        std::thread::scope(|scope| {
            let subscribed_worker = Arc::clone(&subscribed);
            let release_worker = Arc::clone(&release);
            let subscriber = scope.spawn(|| {
                client
                    .subscribe_quotes_inner("BTCUSDT", move || {
                        subscribed_worker.wait();
                        release_worker.wait();
                    })
                    .unwrap();
            });
            subscribed.wait();
            let publisher_worker = Arc::clone(&publisher_started);
            let live = scope.spawn(move || {
                publisher_worker.wait();
                handle_quote_message(
                    &quote_for("BTCUSDT", 2),
                    &sink_tx,
                    &live_instruments,
                    &live_warnings,
                    &live_subs,
                    &live_delivery,
                    SimClock::identity(),
                );
            });
            publisher_started.wait();
            release.wait();
            subscriber.join().unwrap();
            live.join().unwrap();
        });

        let timestamps: Vec<_> = (0..2)
            .map(|_| match sink_rx.try_recv().unwrap() {
                DataEvent::Data(Data::Quote(quote)) => quote.ts_event.as_u64(),
                other => panic!("expected quote data, got {other:?}"),
            })
            .collect();
        assert_eq!(timestamps, vec![1, 2]);
    }
}
