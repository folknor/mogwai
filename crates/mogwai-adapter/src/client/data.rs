// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `MogwaiDataClient`: the `DataClient` half of the adapter. Owns the
//! subscription table, the poll/WS transport choice, the live bar
//! aggregator, and the request handlers that page the venue's bounded
//! `/trades` scan. Plumbing shared with the execution half (the havoc
//! dispatch pipeline, the instrument cache, clock/url glue) lives in
//! `super::shared`.

use std::{
    collections::HashMap,
    num::NonZeroU64,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use mogwai_data::{BarAcc, fold_trade};
use mogwai_protocol::{InstrumentDef, SimClock, Symbol, TradeTick, VenueMessage};
use nautilus_common::{
    clients::DataClient,
    live::{get_data_event_sender, get_runtime},
    messages::{
        DataEvent,
        data::{
            BarsResponse, DataResponse, InstrumentResponse, InstrumentsResponse, QuotesResponse,
            RequestBars, RequestInstrument, RequestInstruments, RequestQuotes, RequestTrades,
            SubscribeBars, SubscribeInstrument, SubscribeInstruments, SubscribeQuotes,
            SubscribeTrades, TradesResponse, UnsubscribeBars, UnsubscribeInstrument,
            UnsubscribeInstruments, UnsubscribeQuotes, UnsubscribeTrades,
        },
    },
};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarType, Data, bar::get_bar_interval_ns},
    enums::BarAggregation,
    identifiers::{ClientId, InstrumentId, Venue},
};
use nautilus_network::http::HttpClient;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::{
    MOGWAI_VENUE, MogwaiDataClientConfig,
    client::shared::{
        HavocDelivery, HavocFilter, abort_tasks, cache_instruments, capped_limit, conn_havoc,
        date_to_unix_nanos, emit_seeded_instruments, enqueue_havoc, ensure_instrument,
        ensure_on_river, fetch_clock_or_identity, fetch_instruments, flush_havoc_into_pump,
        inbound_havoc, instrument_any_or_warn, instrument_def, join_url, lock_recover,
        now_unix_nanos, run_identity_check, seed_instruments, spawn_latency_pump,
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
    sink: Option<UnboundedSender<DataEvent>>,
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
    /// after the client stopped (AD17).
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MogwaiDataClient {
    /// Creates a new disconnected mogwai data client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(client_id: ClientId, config: MogwaiDataClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let http = HttpClient::new(
            HashMap::new(),
            Vec::new(),
            Vec::new(),
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .context("create HTTP client")?;
        Ok(Self {
            client_id,
            http_quota: HttpQuota::from_conn(&conn_havoc(&config.havoc), SimClock::identity()),
            config,
            connected: Arc::new(AtomicBool::new(false)),
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
        })
    }

    /// Retires the current transport generation's connectivity flag by
    /// REPLACING the shared cell, not by storing `false` into it.
    ///
    /// `abort_tasks` is not synchronous: cancellation is delivered at the
    /// aborted task's next await point, so a reader caught between
    /// `connect_async(..).await` returning and its first select can still run
    /// `connected.store(true)` AFTER the caller has stored `false`. That
    /// leaves a dead client reporting itself connected, and a subsequent
    /// `wait_connected` returning success for a socket that never opened.
    /// Swapping the `Arc` means the retired reader writes to a cell nobody
    /// reads: the drop timing of the old generation stops being a
    /// correctness property.
    fn retire_connected_flag(&mut self) {
        self.connected = Arc::new(AtomicBool::new(false));
    }

    fn subscribe_symbol(&self, symbol: Symbol, kind: SubKind) -> anyhow::Result<()> {
        // The subscription symbol is derived from the nautilus `instrument_id`,
        // while `config.symbol` is what the socket named on its upgrade - two
        // sources of truth for one fact. Unreconciled, a host subscribing ES on
        // a socket bound to MNQ would receive MNQ ticks relabelled ES by
        // nautilus, silently, with no frame and no log. So they must agree
        // case-exactly, matching the venue's own comparison. An absent
        // `config.symbol` takes the venue default and applies no check, which
        // is the pre-carrier behaviour unchanged.
        if let Some(bound) = self.config.symbol.as_deref() {
            ensure!(
                symbol.as_ref() == bound,
                "subscription symbol {symbol} does not match the symbol this connection is bound to ({bound})"
            );
        }
        // The subscription is satisfied LOCALLY. Nautilus still calls
        // subscribe/unsubscribe and this client must still implement them, but
        // the venue serves one run's one tape and pushes it unbidden, so there
        // is no frame to send: this table only gates which arriving ticks are
        // forwarded to the message bus.
        //
        // THE SEEDED INSTRUMENT SET IS NOT AN ADMISSION LIST. The venue resolves
        // any wire-legal symbol and registers it on BIND, so a symbol absent
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
        // THERE IS NO RESUME CURSOR, and a `start_ts` request parameter is
        // therefore not honoured on the subscribe path at all. There used to be
        // one - a per-symbol `SubState.start_ts` seeded here and advanced on
        // every delivered trade - written in three places and read in none,
        // because the reattach hook this client passes to `run_ws_connection`
        // is `Vec::new` and the historical request paths take their start from
        // `request.start`. Reintroducing a cursor means reintroducing a READER
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
        let cached = self
            .subs
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?
            .get(symbol)
            .and_then(|state| state.cached_quote.clone());
        after_enable();
        if let Some(quote) = cached
            && let Some(def) = instrument_def(&self.instruments, symbol)
        {
            emit_quote(&quote, &self.sink()?, &def, self.sim);
        }
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    /// The mirror of `subscribe_symbol`, and equally local: dropping the last
    /// subscriber of every kind stops forwarding ticks. A row that has cached
    /// a quote remains resident so a later quote subscription can replay the
    /// current book; an as-yet uncached row is retired. Nothing is sent to the
    /// venue, which keeps pushing the run's one tape either way.
    fn unsubscribe_symbol(&mut self, symbol: Symbol, kind: SubKind) -> anyhow::Result<()> {
        let mut subs = self
            .subs
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?;
        if let Some(state) = subs.get_mut(&symbol) {
            state.decrement(kind);
            if state.total() == 0 && state.cached_quote.is_none() {
                subs.remove(&symbol);
            }
        }
        Ok(())
    }

    fn sink(&self) -> anyhow::Result<UnboundedSender<DataEvent>> {
        self.sink
            .as_ref()
            .cloned()
            .context("data event sink not initialized")
    }

    /// Flush every completed-but-withheld bar window on teardown (AD19). A time
    /// window whose `close_ts` has already passed but that never got a later
    /// trade to cross its boundary is a GENUINELY COMPLETE bar that the lazy
    /// emit-on-next-trade rule would otherwise discard when the subscription
    /// state is torn down (`stop`, and through it `reset`/`dispose`) - the same
    /// discard `unsubscribe_bars` already guards for a single removed bar type,
    /// generalized to the whole table so a shutdown or a reconnect-driven
    /// `reset` does not silently drop the newest complete bar of every live bar
    /// feed. An IN-PROGRESS window (close_ts still in the future) is left
    /// untouched: shipping it would inject a future-stamped, incomplete bar a
    /// consumer could not tell from a real one. Each flushed window is cleared
    /// so a second teardown call cannot double-emit, and the send is
    /// best-effort: if the egress receiver is already gone the bar simply drops.
    fn flush_completed_bars(&mut self) {
        let Ok(sink) = self.sink() else {
            return;
        };
        let now = now_unix_nanos(self.sim).as_u64();
        let Ok(mut bars) = self.bars.lock() else {
            return;
        };
        for (bar_type, state) in bars.iter_mut() {
            let Some(active) = &state.active else {
                continue;
            };
            if active.close_ts > now {
                continue;
            }
            let symbol = symbol_from_instrument(bar_type.instrument_id());
            let Some(def) = instrument_def(&self.instruments, &symbol) else {
                continue;
            };
            match acc_to_bar(*bar_type, active, &def, self.sim) {
                Ok(bar) => drop(sink.send(DataEvent::Data(Data::Bar(bar)))),
                Err(err) => tracing::warn!(
                    %bar_type,
                    error = %err,
                    "dropping unrepresentable bar on teardown flush"
                ),
            }
            state.active = None;
        }
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
        abort_tasks(&self.task_handles);
        self.retire_connected_flag();
        // Emit any completed-but-withheld bar windows before the drain tasks are
        // gone and `reset` clears the table (AD19). Done after abort so no drain
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
        abort_tasks(&self.task_handles);
        self.retire_connected_flag();
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
        // head-of-line-blocked pings/commands - AD4). It enqueues each filtered
        // message, arrival-anchored, into a latency pump that owns the sink and
        // paces delivery off-loop. Spawn and track the pump before the reader so
        // stop() aborts it alongside the connection task.
        let (deliver_tx, deliver_rx) = unbounded_channel::<HavocDelivery>();
        // The delivery barrier. The venue attaches this socket to the live tape
        // at upgrade, so trade and quote frames can arrive before the post-bind
        // reseed below has read `/instruments` - and `instrument_def` BLACK-HOLES
        // a frame whose def is missing. The reader still enqueues; the pump holds
        // its FIRST delivery until the reseed says go, so nothing reaches a
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
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: inbound_havoc.seed,
                    connected,
                    sim,
                    label: "data",
                    identity,
                },
                None::<tokio::sync::mpsc::UnboundedReceiver<std::convert::Infallible>>,
                |never: &std::convert::Infallible| match *never {},
                // The venue pushes the one run's tape unbidden, so a reattach
                // has no subscribe frames to replay: subscription state is
                // satisfied locally in this client and never reaches the wire.
                Vec::new,
                move |venue_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let handler_deliver = handler_deliver.clone();
                    async move {
                        let mut filter = handler_filter.lock().await;
                        enqueue_havoc(&mut filter, venue_msg, sim, &handler_deliver);
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
                // The data socket sends no commands at all - its command
                // receiver is `Infallible` - so nothing can go undelivered on
                // it. The callback is unreachable rather than merely unused.
                |never: std::convert::Infallible| match never {},
            )
            .await;
        });

        track_task(&self.task_handles, reader_handle);
        // A timed-out connect must not orphan the reader task: it is already in
        // task_handles and would keep looping/reconnecting on the shared
        // `connected` flag, so a retry would spawn a second reader racing the
        // first. Abort the task and clear the stale handle and ws_cmd before
        // propagating, leaving the client cleanly disconnected for retry.
        if let Err(err) = wait_connected(&self.connected, &ws_url).await {
            abort_tasks(&self.task_handles);
            self.retire_connected_flag();
            return Err(err);
        }
        // The post-bind reseed. Binding is what REGISTERS an unconfigured symbol
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
                            drop(sink.send(DataEvent::Instrument(instrument)));
                        }
                    }
                }
            }),
        );
        Ok(())
    }

    fn subscribe_instrument(&mut self, cmd: SubscribeInstrument) -> anyhow::Result<()> {
        let symbol = symbol_from_instrument(cmd.instrument_id);
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
                        if def.symbol == symbol
                            && let Some(instrument) = instrument_any_or_warn(&def, ts_init)
                        {
                            drop(sink.send(DataEvent::Instrument(instrument)));
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

    fn subscribe_quotes(&mut self, cmd: SubscribeQuotes) -> anyhow::Result<()> {
        let symbol = symbol_from_instrument(cmd.instrument_id);
        self.subscribe_quotes_inner(&symbol, || {})
    }

    fn subscribe_trades(&mut self, cmd: SubscribeTrades) -> anyhow::Result<()> {
        let symbol = symbol_from_instrument(cmd.instrument_id);
        self.subscribe_symbol(symbol, SubKind::Trades)
    }

    fn subscribe_bars(&mut self, cmd: SubscribeBars) -> anyhow::Result<()> {
        ensure!(
            cmd.bar_type.spec().is_time_aggregated(),
            "mogwai only supports time based external bars"
        );
        ensure!(
            !is_calendar_anchored(cmd.bar_type.spec().aggregation),
            "mogwai does not support Week/Month/Year bars: they need calendar \
             anchoring this adapter's epoch-anchored aggregation cannot produce; \
             use Day or finer"
        );
        let symbol = symbol_from_instrument(cmd.bar_type.instrument_id());
        {
            // `lock_recover`, matching the rollback below and every other bar
            // lock in this client. The two halves of one increment-then-roll-
            // back pair must not disagree about poisoning: failing the
            // increment on a poisoned guard while the rollback recovers it
            // would make the poisoned path take a branch no test covers.
            let mut bars = lock_recover(&self.bars, "bar");
            bars.entry(cmd.bar_type).or_default().refs += 1;
        }
        // ROLL THE REF BACK WHEN THE SYMBOL SUBSCRIPTION REFUSES (AD27). The
        // per-`BarType` ref and the per-symbol `SubState.bars` count are the
        // two halves of one subscription and must move together - that is the
        // whole of AD10's rule in `unsubscribe_bars`, stated there from the
        // release side. `subscribe_symbol` can refuse (the bound-symbol check),
        // and a ref left standing over a refusal is the same cross-counter
        // desync arriving from the subscribe side: a later `unsubscribe_bars`
        // for that bar type finds `refs > 0`, so it matches, and it spends a
        // symbol-count decrement belonging to a DIFFERENT bar type's live
        // subscription - which, at zero, darkens the surviving feed.
        if let Err(err) = self.subscribe_symbol(symbol, SubKind::Bars) {
            let mut bars = lock_recover(&self.bars, "bar");
            if let Some(state) = bars.get_mut(&cmd.bar_type) {
                state.refs = state.refs.saturating_sub(1);
                if state.refs == 0 {
                    bars.remove(&cmd.bar_type);
                }
            }
            return Err(err);
        }
        Ok(())
    }

    fn unsubscribe_quotes(&mut self, cmd: &UnsubscribeQuotes) -> anyhow::Result<()> {
        self.unsubscribe_symbol(symbol_from_instrument(cmd.instrument_id), SubKind::Quotes)
    }

    fn unsubscribe_trades(&mut self, cmd: &UnsubscribeTrades) -> anyhow::Result<()> {
        self.unsubscribe_symbol(symbol_from_instrument(cmd.instrument_id), SubKind::Trades)
    }

    fn unsubscribe_bars(&mut self, cmd: &UnsubscribeBars) -> anyhow::Result<()> {
        let symbol = symbol_from_instrument(cmd.bar_type.instrument_id());
        // Decrement the per-symbol bars count ONLY when this bar type actually had
        // a live subscription to release (AD10). The per-BarType refs and the
        // per-symbol SubState.bars count are incremented together on subscribe, so
        // they must be decremented together. An unmatched unsubscribe_bars (a bar
        // type never subscribed, or a double-unsubscribe interleaved by nautilus
        // command replay) that still decremented the symbol count would steal a
        // decrement belonging to a DIFFERENT bar type's live subscription and, if
        // that dropped the symbol total to 0, fire a wire Unsubscribe that darkens
        // the surviving feed. Saturating arithmetic prevents underflow, not this
        // cross-type theft - so gate the symbol decrement on a real match.
        let (matched, to_flush) = {
            let mut bars = self
                .bars
                .lock()
                .map_err(|_| anyhow::anyhow!("bar mutex poisoned"))?;
            let refs_after = match bars.get_mut(&cmd.bar_type) {
                Some(state) if state.refs > 0 => {
                    state.refs -= 1;
                    Some(state.refs)
                }
                _ => None,
            };
            match refs_after {
                // On the last release, take the removed bar type's active window
                // out with it so a completed-but-withheld bar can be flushed
                // below rather than silently discarded (AD19).
                Some(0) => (
                    true,
                    bars.remove(&cmd.bar_type).and_then(|state| state.active),
                ),
                Some(_) => (true, None),
                None => (false, None),
            }
        };
        if matched {
            // Flush the removed bar type's active window IF it already closed
            // (close_ts <= sim-now) but was withheld only for lack of a later
            // trade to cross its boundary - the AD19 discard-on-unsubscribe case.
            // A genuinely in-progress window (close_ts still in the future) is
            // dropped, not emitted: shipping it would inject a future-stamped,
            // incomplete bar a consumer could not tell from a real completed one.
            // The teardown twin of this flush lives in `flush_completed_bars`
            // (called from `stop`). Closing a live in-progress window ON TIME on
            // a clock timer is a separate feature, deliberately not built - see
            // `flush_completed_bars` and the AD19 note in havoc.md.
            if let Some(active) = to_flush {
                let now = now_unix_nanos(self.sim).as_u64();
                if active.close_ts <= now
                    && let Some(def) = instrument_def(&self.instruments, &symbol)
                {
                    match acc_to_bar(cmd.bar_type, &active, &def, self.sim) {
                        Ok(bar) => {
                            if let Ok(sink) = self.sink() {
                                drop(sink.send(DataEvent::Data(Data::Bar(bar))));
                            }
                        }
                        Err(err) => tracing::warn!(
                            bar_type = %cmd.bar_type,
                            error = %err,
                            "dropping unrepresentable bar on unsubscribe flush"
                        ),
                    }
                }
            }
            self.unsubscribe_symbol(symbol, SubKind::Bars)?;
        } else {
            tracing::warn!(
                bar_type = %cmd.bar_type,
                "ignoring unsubscribe_bars with no matching subscription; \
                 not touching the symbol's shared bars count (AD10)"
            );
        }
        Ok(())
    }

    fn request_trades(&self, request: RequestTrades) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        let end = date_to_unix_nanos(request.end);
        // Refuse an off-river window at the boundary, loudly - but ANSWER it.
        // Returning the error to nautilus is not a refusal the requester ever
        // sees: `DataEngine::execute` log::error!s a synchronous client error
        // and emits no correlated response, so `?` here leaves the request
        // outstanding forever and the consumer burns its whole timeout on what
        // looks like a hung venue. Log the named diagnostic and answer empty.
        if let Err(err) = ensure_on_river(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_trades: refusing an off-river window; answering with an empty trade response so the request resolves");
            drop(sink.send(DataEvent::Response(DataResponse::Trades(
                TradesResponse::new(
                    request.request_id,
                    client_id,
                    request.instrument_id,
                    Vec::new(),
                    start,
                    end,
                    now_unix_nanos(sim),
                    request.params,
                ),
            ))));
            return Ok(());
        }
        track_task(&self.task_handles, get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            // Page the window rather than issuing one MAX_HISTORY_LIMIT-capped
            // request: a window with more than 1000 trades used to return only
            // its oldest 1000, with the response still claiming the full range.
            // `request.limit` counts TRADES here, so it becomes the pagination
            // ceiling.
            let max_trades = request.limit.map(std::num::NonZeroUsize::get);
            // Always-yields block, for the reason spelled out in `request_bars`:
            // a failure arm that returned from the task left the nautilus
            // request unresolved, which the consumer cannot tell from a hang.
            let data = 'trades: {
                let def = match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
                {
                    Ok(def) => def,
                    Err(err) => {
                        tracing::error!(%symbol, error = %err, "request_trades: instrument lookup failed; answering with an empty trade response so the request resolves");
                        break 'trades Vec::new();
                    }
                };
                let (mut trades, truncated) = match fetch_trades_windowed(
                    &http,
                    &http_quota,
                    &base,
                    TradeFetch {
                        symbol: &symbol,
                        start,
                        end,
                        limit: None,
                    },
                    |out| max_trades.is_some_and(|m| out.len() >= m),
                )
                .await
                {
                    Ok(result) => result,
                    Err(err) => {
                        // Surface the failure instead of the old silent `if let Ok`
                        // drop: a venue 400 (an off-river window, or a symbol the
                        // run does not serve) or any fetch error must be visible,
                        // not mistaken for "no trades in the window".
                        tracing::error!(%symbol, error = %err, "request_trades: trade fetch failed (the venue may have refused an off-river window); answering with an empty trade response so the request resolves");
                        break 'trades Vec::new();
                    }
                };
                if let Some(m) = max_trades {
                    // Paging overshoots the trade ceiling by up to one page; trim to
                    // the requested count (from the oldest edge) so the response
                    // honors the limit exactly.
                    trades.truncate(m);
                }
                if truncated {
                    tracing::warn!(
                        %symbol,
                        trades = trades.len(),
                        "request_trades: history window truncated before its end (trade limit reached or same-ts wedge); the requested history may not splice contiguously into live"
                    );
                }
                trades
                    .iter()
                    .filter_map(|t| {
                        convert::trade_tick(t, request.instrument_id, &def, now_unix_nanos(sim))
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
            };
            let response = TradesResponse::new(
                request.request_id,
                client_id,
                request.instrument_id,
                data,
                start,
                end,
                now_unix_nanos(sim),
                request.params,
            );
            drop(sink.send(DataEvent::Response(DataResponse::Trades(response))));
        }));
        Ok(())
    }

    fn request_quotes(&self, request: RequestQuotes) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        let end = date_to_unix_nanos(request.end);
        if let Err(err) = ensure_on_river(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_quotes: refusing an off-river window; answering empty");
            drop(sink.send(DataEvent::Response(DataResponse::Quotes(
                QuotesResponse::new(
                    request.request_id,
                    client_id,
                    request.instrument_id,
                    Vec::new(),
                    start,
                    end,
                    now_unix_nanos(sim),
                    request.params,
                ),
            ))));
            return Ok(());
        }
        track_task(&self.task_handles, get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            let data = 'quotes: {
                let def = match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await {
                    Ok(def) => def,
                    Err(err) => {
                        tracing::error!(%symbol, error = %err, "request_quotes: instrument lookup failed; answering empty");
                        break 'quotes Vec::new();
                    }
                };
                let limit = request.limit.map(std::num::NonZeroUsize::get);
                let (quotes, truncated) = match fetch_quotes_windowed(
                    &http,
                    &http_quota,
                    &base,
                    QuoteFetch { symbol: &symbol, start, end, limit: request.limit },
                ).await {
                    Ok(quotes) => quotes,
                    Err(err) => {
                        tracing::error!(%symbol, error = %err, "request_quotes: history fetch failed; answering empty");
                        break 'quotes Vec::new();
                    }
                };
                if truncated {
                    tracing::warn!(%symbol, "quote history truncated (limit reached or same-ts wedge)");
                }
                quotes.into_iter().take(limit.unwrap_or(usize::MAX)).filter_map(|quote| {
                    convert::quote_tick(
                        &quote,
                        request.instrument_id,
                        &def,
                        now_unix_nanos(sim),
                    ).map_err(|err| tracing::warn!(%symbol, error = %err, "dropping historical quote: unrepresentable tick")).ok()
                }).collect()
            };
            let response = QuotesResponse::new(
                request.request_id,
                client_id,
                request.instrument_id,
                data,
                start,
                end,
                now_unix_nanos(sim),
                request.params,
            );
            drop(sink.send(DataEvent::Response(DataResponse::Quotes(response))));
        }));
        Ok(())
    }

    fn request_bars(&self, request: RequestBars) -> anyhow::Result<()> {
        ensure!(
            request.bar_type.spec().is_time_aggregated(),
            "mogwai only supports time based external bars"
        );
        ensure!(
            !is_calendar_anchored(request.bar_type.spec().aggregation),
            "mogwai does not support Week/Month/Year bars: they need calendar \
             anchoring this adapter's epoch-anchored aggregation cannot produce; \
             use Day or finer"
        );
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        let end = date_to_unix_nanos(request.end);
        // Refuse an off-river history window at the boundary, naming the floor -
        // but ANSWER it, for the reason spelled out in `request_trades`: a
        // synchronous `Err` is logged by the data engine and never turned into
        // a response, so `?` here would leave the history request unresolved.
        if let Err(err) = ensure_on_river(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_bars: refusing an off-river history window; answering with an empty bar response so the request resolves");
            drop(
                sink.send(DataEvent::Response(DataResponse::Bars(BarsResponse::new(
                    request.request_id,
                    client_id,
                    request.bar_type,
                    Vec::new(),
                    start,
                    end,
                    now_unix_nanos(sim),
                    request.params,
                )))),
            );
            return Ok(());
        }
        track_task(&self.task_handles, get_runtime().spawn(async move {
            let instrument_id = request.bar_type.instrument_id();
            let symbol = symbol_from_instrument(instrument_id);
            // Page the window, translating nautilus's BAR-count limit into a
            // bar-span pagination target: the old single request applied a
            // BAR-count limit as a TRADE-page limit, so a history request for
            // N bars fetched at most N trades (~N/5 bars on the fitted tape)
            // covering only the oldest edge, under-delivering or timing out
            // the history request.
            let bar_limit = request.limit.map(std::num::NonZeroUsize::get);
            let interval = get_bar_interval_ns(&request.bar_type).as_u64();
            // Every exit from this block yields bars, so the response below is
            // ALWAYS sent. A failure arm used to `return` straight out of the
            // task, which left the nautilus request unresolved forever: from the
            // consumer that is indistinguishable from a hang, and it burns the
            // whole history-request timeout before dying with nothing but a
            // line in the worker log to show for it. An empty response is a
            // truthful answer that at least RESOLVES; the error detail rides
            // the log.
            let bars: Vec<Bar> = 'bars: {
                let def = match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
                {
                    Ok(def) => def,
                    Err(err) => {
                        tracing::error!(%symbol, error = %err, "request_bars: instrument lookup failed; answering with an empty bar response so the request resolves");
                        break 'bars Vec::new();
                    }
                };
                let (trades, truncated) = match fetch_trades_windowed(
                    &http,
                    &http_quota,
                    &base,
                    TradeFetch {
                        symbol: &symbol,
                        start,
                        end,
                        limit: None,
                    },
                    |out| bar_span_reached(out, interval, bar_limit),
                )
                .await
                {
                    Ok(result) => result,
                    Err(err) => {
                        tracing::error!(%symbol, error = %err, "request_bars: trade fetch failed (the venue may have refused an off-river window); answering with an empty bar response so the request resolves");
                        break 'bars Vec::new();
                    }
                };
                if truncated {
                    tracing::warn!(
                        %symbol,
                        "request_bars: history window truncated before its end (bar limit reached or same-ts wedge); the requested history may not splice contiguously into live"
                    );
                }
                let mut bars = aggregate_bars(&request.bar_type, &trades, &def, sim, end);
                if let Some(m) = bar_limit {
                    // Paging spans at least `bar_limit` intervals, so it may produce
                    // a few extra bars; trim to the requested count (oldest edge,
                    // from the window start) so the response honors the bar limit.
                    bars.truncate(m);
                }
                // An on-river window that under-delivers is a real, reachable
                // state, not an error: mogwai's fitted arrival process is
                // heavy-tailed, and a measured sweep of the default 24h-horizon
                // tape found stretches of 15+ SIMULATED HOURS running at 3-10
                // trades per hour (see reference/architecture.md, "Tape arrival
                // droughts"). Bars exist only for intervals that CONTAIN a
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
                        bar_type = %request.bar_type,
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
            let response = BarsResponse::new(
                request.request_id,
                client_id,
                request.bar_type,
                bars,
                start,
                end,
                now_unix_nanos(sim),
                request.params,
            );
            drop(sink.send(DataEvent::Response(DataResponse::Bars(response))));
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
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                // Same always-answer rule as `request_bars`/`request_trades`:
                // the old `if let Ok` swallowed a failed fetch entirely, and
                // the nautilus request then never resolved. Answer with an
                // empty set and put the reason in the log.
                let ts_init = now_unix_nanos(sim);
                let data = match fetch_instruments(&http, &http_quota, &base).await {
                    Ok(defs) => {
                        cache_instruments(&instruments, defs.clone());
                        defs.iter()
                            .filter_map(|def| instrument_any_or_warn(def, ts_init))
                            .collect()
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "request_instruments: instrument fetch failed; answering with an empty instrument response so the request resolves");
                        Vec::new()
                    }
                };
                let response = InstrumentsResponse::new(
                    request.request_id,
                    client_id,
                    request.venue.unwrap_or(*MOGWAI_VENUE),
                    data,
                    date_to_unix_nanos(request.start),
                    date_to_unix_nanos(request.end),
                    ts_init,
                    request.params,
                );
                drop(sink.send(DataEvent::Response(DataResponse::Instruments(response))));
            }),
        );
        Ok(())
    }

    fn request_instrument(&self, request: RequestInstrument) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let http_quota = self.http_quota.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                let symbol = symbol_from_instrument(request.instrument_id);
                // The one generator that CANNOT answer on failure:
                // `InstrumentResponse` carries exactly one `InstrumentAny` and
                // has no empty form, so there is no truthful response to send
                // when the instrument cannot be resolved - unlike the bars,
                // trades and instruments generators, which all answer empty
                // rather than leaving the request unresolved. Log loudly at
                // both failure points so the unresolved request is at least
                // diagnosable.
                match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await {
                    Ok(def) => {
                        let ts_init = now_unix_nanos(sim);
                        if let Some(data) = instrument_any_or_warn(&def, ts_init) {
                            let response = InstrumentResponse::new(
                                request.request_id,
                                client_id,
                                request.instrument_id,
                                data,
                                date_to_unix_nanos(request.start),
                                date_to_unix_nanos(request.end),
                                ts_init,
                                request.params,
                            );
                            drop(sink.send(DataEvent::Response(DataResponse::Instrument(
                                Box::new(response),
                            ))));
                        } else {
                            tracing::error!(
                                %symbol,
                                "request_instrument: the venue's definition is unrepresentable as a nautilus instrument; the request cannot be answered and will not resolve"
                            );
                        }
                    }
                    Err(err) => tracing::error!(
                        %symbol,
                        error = %err,
                        "request_instrument: instrument lookup failed; the request cannot be answered and will not resolve"
                    ),
                }
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
}

#[derive(Debug, Default)]
struct SubState {
    trades: usize,
    quotes: usize,
    bars: usize,
    cached_quote: Option<mogwai_protocol::QuoteTick>,
}

impl SubState {
    fn total(&self) -> usize {
        self.trades + self.quotes + self.bars
    }

    fn increment(&mut self, kind: SubKind) {
        match kind {
            SubKind::Trades => self.trades += 1,
            SubKind::Quotes => self.quotes += 1,
            SubKind::Bars => self.bars += 1,
        }
    }

    fn decrement(&mut self, kind: SubKind) {
        match kind {
            SubKind::Trades => self.trades = self.trades.saturating_sub(1),
            SubKind::Quotes => self.quotes = self.quotes.saturating_sub(1),
            SubKind::Bars => self.bars = self.bars.saturating_sub(1),
        }
    }
}

#[derive(Debug, Default)]
struct BarSubState {
    refs: usize,
    active: Option<BarAcc>,
}

#[allow(clippy::too_many_arguments)]
async fn handle_market_message(
    msg: VenueMessage,
    sink: &UnboundedSender<DataEvent>,
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
        VenueMessage::Heartbeat { .. } => {
            tracing::trace!("ignoring venue heartbeat on data path");
        }
        VenueMessage::ProtocolError { reason, .. } => {
            // ProtocolError is now narrowed to WHOLE-FRAME faults the venue
            // could not attribute to any entry (a Subscribe refused at the
            // validation boundary, an unsupported carrier). Per-entry subscribe
            // outcomes arrive as SubscriptionIssues, handled below.
            // Swallowing this here left the feed silent with
            // no downstream signal, indistinguishable from a quiet market.
            // Surface the venue's reason VERBATIM and do not guess at causes:
            // an earlier version of this line enumerated three candidates, and
            // when a venue restart rewound sim-now under a surviving client's
            // cursor the enumeration named only wrong ones - at ~11 WARN/s it
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
            // ERROR, not warn, and the level is a RULING rather than taste.
            // `FeedLagged` is the venue declaring that this client's tape has
            // a hole: bars aggregate wrong, a `PollCursor` resumes past the
            // missing span, and nothing downstream can tell the difference
            // between a quiet market and a dropped one. The standing
            // preference is an assert, a type or a guard over a verification,
            // and none of the three is available HERE: nautilus's `DataEvent`
            // carries no gap or degradation variant (`Response`, `Data`,
            // `Instrument`, `FundingRate`, `InstrumentStatus`, `OptionGreeks`
            // only), the client is handed to the host as a `dyn DataClient`
            // with no downcast, and fabricating an `InstrumentStatus` would
            // report a venue halt that did not happen. Refusing - tearing the
            // socket down - would turn a recoverable gap into a total outage
            // and invent a policy the venue did not ask for. So the loudest
            // honest channel is the log, at the level a host alerts on. The
            // real fix is a declared feed-gap event upstream; see the
            // cross-repo entry in `notes/todo.md`.
            //
            // THE BOUNDARIES ARE THE ACTIONABLE PART for whoever reads this
            // log: they delimit the affected span, so a bar folded across it or
            // a cursor advanced through it can be identified rather than
            // guessed at. `episode` and `skipped_total` separate a client that
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
            // ITS OWN RECORD, and not folded into the arm above. The run may
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
    sink: &UnboundedSender<DataEvent>,
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

fn retain_quote(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    quote: &mogwai_protocol::QuoteTick,
) -> bool {
    let mut states = lock_recover(subs, "subscriptions");
    // A pre-subscription BBO must survive until the local subscription is
    // activated, but an arbitrary wire symbol must not grow this map forever.
    // One run serves one configured instrument, so more than this many orphan
    // symbols is malformed input and is dropped without allocating a row.
    //
    // The bound counts ORPHANS ONLY - rows no subscription refers to - so a
    // client with many live subscriptions cannot crowd itself out of the
    // pre-subscription cache. Nothing here evicts: an existing row is always
    // updated, so a symbol that once got a cache row can never lose it to a
    // flood of junk symbols arriving after it.
    const MAX_ORPHAN_QUOTE_SYMBOLS: usize = 64;
    let orphans = states.values().filter(|state| state.total() == 0).count();
    if !states.contains_key(&quote.symbol) && orphans >= MAX_ORPHAN_QUOTE_SYMBOLS {
        tracing::warn!(symbol = %quote.symbol, "dropping quote: orphan quote cache is full");
        return false;
    }
    let state = states
        .entry(std::sync::Arc::clone(&quote.symbol))
        .or_default();
    state.cached_quote = Some(quote.clone());
    state.quotes > 0
}

fn emit_quote(
    quote: &mogwai_protocol::QuoteTick,
    sink: &UnboundedSender<DataEvent>,
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
    sink: &UnboundedSender<DataEvent>,
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
    sink: &UnboundedSender<DataEvent>,
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
            if let Some(bar) = update_bar_state(*bar_type, state, trade, def, sim) {
                ready.push(bar);
            }
        }
    }
    for bar in ready {
        drop(sink.send(DataEvent::Data(Data::Bar(bar))));
    }
}

/// True for the time-aggregated bar aggregations mogwai refuses (AD11). Week,
/// Month, and Year are calendar-anchored in nautilus (`get_time_bar_start`
/// anchors weeks to Monday and months/years to the calendar), but
/// `get_bar_interval_ns` returns a fixed 7-day/30-day/365-day proxy - nautilus's
/// own comment calls it a proxy "for comparing bar lengths", not a calendar
/// interval. The adapter's `((ts / interval) + 1) * interval` aggregation would
/// therefore produce epoch-anchored 30-day blocks instead of calendar months,
/// and epoch-anchored (Thursday) weeks instead of Monday-anchored ones. Day and
/// finer are correctly UTC-aligned, so only these three are refused - like the
/// tick/volume aggregations `is_time_aggregated` already refuses. Refusing is the
/// chosen resolution over building calendar anchoring (heavy, and unlikely bar
/// specs for this venue).
fn is_calendar_anchored(aggregation: BarAggregation) -> bool {
    matches!(
        aggregation,
        BarAggregation::Week | BarAggregation::Month | BarAggregation::Year
    )
}

// The `expect` below is on a genuine invariant (every admitted bar aggregation
// has a positive interval; tick, volume and calendar-anchored aggregations are
// refused upstream, which is what `mogwai_data::bars` takes a `NonZeroU64`
// interval to encode), not a fallible path this function's
// `Option<Bar>` return is meant to surface, so `clippy::unwrap_in_result`'s
// default suggestion (propagate it as the returned `None`) does not apply
// here.
#[allow(clippy::unwrap_in_result)]
fn update_bar_state(
    bar_type: BarType,
    state: &mut BarSubState,
    trade: &mogwai_protocol::TradeTick,
    def: &InstrumentDef,
    sim: SimClock,
) -> Option<Bar> {
    let interval_ns = get_bar_interval_ns(&bar_type).as_u64();
    let interval =
        NonZeroU64::new(interval_ns).expect("admitted bar aggregations have a positive interval");
    // The window has already rotated inside `fold_trade` by the time this
    // returns, so the "one bad bar doesn't wedge aggregation" property is
    // structural: the rotation no longer depends on the conversion below
    // succeeding. A hostile open/high/low/close/volume that overflows
    // nautilus Price/Quantity just drops this one bar with a warning.
    let closed = fold_trade(
        &mut state.active,
        trade.price,
        trade.size,
        trade.ts_event,
        interval,
    )?;
    match acc_to_bar(bar_type, &closed, def, sim) {
        Ok(bar) => Some(bar),
        Err(err) => {
            tracing::warn!(%bar_type, error = %err, "dropping unrepresentable bar");
            None
        }
    }
}

fn aggregate_bars(
    bar_type: &BarType,
    trades: &[mogwai_protocol::TradeTick],
    def: &InstrumentDef,
    sim: SimClock,
    end: Option<UnixNanos>,
) -> Vec<Bar> {
    let mut state = BarSubState::default();
    let mut out = Vec::new();
    for trade in trades {
        if let Some(bar) = update_bar_state(*bar_type, &mut state, trade, def, sim) {
            out.push(bar);
        }
    }
    // Flush the trailing window ONLY when the request's `end` proves it fully
    // elapsed. A window's bar is otherwise emitted lazily, when a LATER trade
    // crosses its `close_ts` - but a historical request over a window that has
    // already passed gets no such trade, so the newest COMPLETE window would be
    // silently dropped (the always-stale or missing last bar of every history
    // request). If
    // `end >= acc.close_ts` the window closed within the requested range and
    // must be emitted; a genuinely-partial trailing window (`end` inside it, or
    // an unknown `end`) is still dropped, matching the live path.
    if let (Some(acc), Some(end)) = (&state.active, end)
        && end.as_u64() >= acc.close_ts
    {
        match acc_to_bar(*bar_type, acc, def, sim) {
            Ok(bar) => out.push(bar),
            Err(err) => {
                tracing::warn!(%bar_type, error = %err, "dropping unrepresentable trailing bar");
            }
        }
    }
    out
}

fn acc_to_bar(
    bar_type: BarType,
    acc: &BarAcc,
    def: &InstrumentDef,
    sim: SimClock,
) -> anyhow::Result<Bar> {
    Ok(Bar::new(
        bar_type,
        convert::price(acc.open, def.price_precision)?,
        convert::price(acc.high, def.price_precision)?,
        convert::price(acc.low, def.price_precision)?,
        convert::price(acc.close, def.price_precision)?,
        convert::quantity(acc.volume, def.size_precision)?,
        UnixNanos::from(acc.close_ts),
        now_unix_nanos(sim),
    ))
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
struct TradeFetch<'a> {
    symbol: &'a str,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
    limit: Option<std::num::NonZeroUsize>,
}
/// Request-wide ceiling, in TRADES rather than in pages, on what one paged
/// history request may accumulate. A page count would multiply badly: 256 pages
/// at 50,000 each is 12.8M `TradeTick`s and ~1.8 GB of JSON for a single
/// `request_bars`. 1,000,000 is 20 pages at the current page size, roughly seven
/// simulated hours, ~140 MB transferred and a resident vector in the low
/// hundreds of MB.
///
/// It is sized against the fact that `request_bars` aggregates from a COMPLETE
/// trade vector. Making bar aggregation incremental is the change that would let
/// this be raised; until then the vector is what the bound is about.
const MAX_TRADES_PER_REQUEST: usize = 1_000_000;
/// Loop-safety backstop only. The real bound is `MAX_TRADES_PER_REQUEST`; this
/// exists so a venue that kept answering full pages without advancing the
/// cursor cannot spin forever.
const MAX_TRADE_PAGES: usize = 64;
async fn fetch_trades(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    fetch: TradeFetch<'_>,
) -> anyhow::Result<Vec<TradeTick>> {
    let params = trade_query_params(fetch.symbol, fetch.start, fetch.end, fetch.limit)?;
    quota.wait().await;
    let response = http
        .get(
            join_url(base, "trades"),
            Some(&params),
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await?;
    ensure!(
        response.status.is_success(),
        "fetch trades returned {}",
        response.status.as_u16()
    );
    Ok(serde_json::from_slice(&response.body)?)
}
async fn fetch_trades_windowed(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    fetch: TradeFetch<'_>,
    mut stop: impl FnMut(&[TradeTick]) -> bool,
) -> anyhow::Result<(Vec<TradeTick>, bool)> {
    let mut out = Vec::new();
    let mut start = fetch.start;
    // A caller-stated limit is a request-wide ceiling too, not a per-page one:
    // it must bound the accumulation or paging would silently overrun it.
    // Today's two callers pass `None` (their real stop condition is the `stop`
    // closure), but honoring it keeps the field from becoming a lie.
    let ceiling = fetch.limit.map_or(MAX_TRADES_PER_REQUEST, |limit| {
        limit.get().min(MAX_TRADES_PER_REQUEST)
    });
    for _ in 0..MAX_TRADE_PAGES {
        let remaining = ceiling.saturating_sub(out.len());
        if remaining == 0 {
            return Ok((out, true));
        }
        let page_limit = remaining.min(mogwai_protocol::MAX_HISTORY_LIMIT);
        let page = fetch_trades(
            http,
            quota,
            base,
            TradeFetch {
                symbol: fetch.symbol,
                start,
                end: fetch.end,
                limit: std::num::NonZeroUsize::new(page_limit),
            },
        )
        .await?;
        let full = page.len() == page_limit;
        if !full {
            // A short page is the end of the window: every row at every
            // timestamp it contains has been disclosed.
            out.extend(page);
            let stopped = stop(&out);
            return Ok((out, stopped));
        }
        // A full page may have been cut inside a timestamp, so its final
        // timestamp group is not known to be complete. The cursor is
        // timestamp-only, so the only advance that cannot lose a row is one
        // onto a timestamp whose rows have all been seen: keep the complete
        // prefix, drop the trailing group, and re-request from that group's
        // own timestamp, which the endpoint's start bound includes.
        let mut page = page;
        let group_start = final_ts_group_start(&page, |trade| trade.ts_event);
        if group_start == 0 {
            // The whole page shares one timestamp, so there is no complete
            // prefix and no advance that discloses the rest. Stop visibly.
            //
            // LOUD, because "visibly" was not true where it matters. This
            // returns a SHORT history with no error, and a consumer that folds
            // bars from trades cannot tell that from a window which legitimately
            // ended: it reaches a strategy as a quiet history response, which is the
            // shape this venue has been caught in twice already (the masked
            // history refusal, and `FeedLagged` having no channel but a log).
            // The venue's own tape cannot produce this - trades on one river are
            // strictly increasing at a 1 us child stride, pinned by
            // `a_river_never_prints_two_trades_at_one_instant` in `mogwai-data` -
            // so reaching here means either that invariant broke or the caller
            // asked for a page of one row. Both are worth an operator seeing,
            // and neither is worth failing the request over: the rows are real.
            tracing::error!(
                symbol = %fetch.symbol,
                rows = out.len() + page.len(),
                page_limit,
                ts_event = page[0].ts_event,
                "history page is one whole timestamp, so paging cannot advance without losing \
                 rows; returning a SHORT history that will look like a quiet window"
            );
            out.extend(page);
            return Ok((out, true));
        }
        let group_ts = page[group_start].ts_event;
        page.truncate(group_start);
        out.extend(page);
        if stop(&out) {
            return Ok((out, true));
        }
        start = Some(UnixNanos::from(group_ts));
    }
    Ok((out, true))
}

/// Index at which the final `ts_event` group of an ascending page begins, or
/// `page.len()` for an empty page. A full page's trailing group is the only
/// part a timestamp-only cursor cannot prove complete, so this is where the
/// windowed fetchers cut before advancing.
fn final_ts_group_start<T>(page: &[T], ts_of: impl Fn(&T) -> u64) -> usize {
    let Some(last) = page.last() else {
        return 0;
    };
    let last_ts = ts_of(last);
    let mut idx = page.len();
    while idx > 0 && ts_of(&page[idx - 1]) == last_ts {
        idx -= 1;
    }
    idx
}

struct QuoteFetch<'a> {
    symbol: &'a str,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
    limit: Option<std::num::NonZeroUsize>,
}

async fn fetch_quotes(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    fetch: QuoteFetch<'_>,
) -> anyhow::Result<Vec<mogwai_protocol::QuoteTick>> {
    let params = trade_query_params(fetch.symbol, fetch.start, fetch.end, fetch.limit)?;
    quota.wait().await;
    let response = http
        .get(
            join_url(base, "quotes"),
            Some(&params),
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await?;
    ensure!(
        response.status.is_success(),
        "fetch quotes returned {}",
        response.status.as_u16()
    );
    Ok(serde_json::from_slice(&response.body)?)
}

async fn fetch_quotes_windowed(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    fetch: QuoteFetch<'_>,
) -> anyhow::Result<(Vec<mogwai_protocol::QuoteTick>, bool)> {
    let ceiling = fetch.limit.map_or(MAX_TRADES_PER_REQUEST, |limit| {
        limit.get().min(MAX_TRADES_PER_REQUEST)
    });
    let mut out = Vec::new();
    let mut start = fetch.start;
    for _ in 0..MAX_TRADE_PAGES {
        let remaining = ceiling.saturating_sub(out.len());
        if remaining == 0 {
            return Ok((out, true));
        }
        let page_limit = remaining.min(mogwai_protocol::MAX_HISTORY_LIMIT);
        let page = fetch_quotes(
            http,
            quota,
            base,
            QuoteFetch {
                symbol: fetch.symbol,
                start,
                end: fetch.end,
                limit: std::num::NonZeroUsize::new(page_limit),
            },
        )
        .await?;
        let full = page.len() == page_limit;
        if !full {
            out.extend(page);
            // `false`, NOT the trade path's `stop(&out)`, and the difference is
            // deliberate rather than drift: a short page means the venue has no
            // more rows in the window, so nothing was truncated. The trade path
            // consults its stop closure here because it can also be asked to
            // stop EARLY - once enough bars are spanned - and stopping early
            // genuinely does truncate. There is no such closure on this path.
            // These two loops are near-duplicates and this line is the one
            // place they must not be made identical.
            return Ok((out, false));
        }
        // Same invariant as the trade path: only advance onto a timestamp
        // whose rows have all been seen. See `final_ts_group_start`.
        let mut page = page;
        let group_start = final_ts_group_start(&page, |quote| quote.ts_event);
        if group_start == 0 {
            out.extend(page);
            return Ok((out, true));
        }
        let group_ts = page[group_start].ts_event;
        page.truncate(group_start);
        out.extend(page);
        start = Some(UnixNanos::from(group_ts));
    }
    Ok((out, true))
}
fn bar_span_reached(trades: &[TradeTick], interval: u64, limit: Option<usize>) -> bool {
    match (trades.first(), trades.last(), limit) {
        (Some(first), Some(last), Some(limit)) if interval > 0 => {
            usize::try_from((last.ts_event / interval).saturating_sub(first.ts_event / interval))
                .unwrap_or(usize::MAX)
                >= limit
        }
        _ => false,
    }
}
fn trade_query_params(
    symbol: &str,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
    limit: Option<std::num::NonZeroUsize>,
) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let mut params = HashMap::new();
    params.insert("symbol".into(), vec![symbol.into()]);
    match (start, end) {
        (Some(start), Some(end)) => {
            params.insert("start".into(), vec![start.as_u64().to_string()]);
            params.insert("end".into(), vec![end.as_u64().to_string()]);
        }
        (Some(start), None) => {
            params.insert("start".into(), vec![start.as_u64().to_string()]);
        }
        (None, Some(end)) => {
            params.insert("end".into(), vec![end.as_u64().to_string()]);
        }
        (None, None) => {}
    }
    params.insert("limit".into(), vec![capped_limit(limit).to_string()]);
    // No `regime` parameter: the market regime is boot config on the venue
    // now, chosen by whoever launches the run, so a client cannot select one
    // per request.
    Ok(params)
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

    /// AD27. The bar ref and the per-symbol bars count are one subscription in
    /// two counters, and a refused `subscribe_bars` must leave BOTH at zero.
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

    #[test]
    fn orphan_quote_cache_is_bounded() {
        let subs = Arc::new(Mutex::new(HashMap::new()));
        for i in 0..100 {
            retain_quote(&subs, &quote_for(&format!("S{i}"), i));
        }
        assert_eq!(lock_recover(&subs, "test").len(), 64);
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
