// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `MogwaiDataClient`: the `DataClient` half of the adapter. Owns the
//! subscription table, the poll/WS transport choice, the live bar
//! aggregator, and the request handlers that page the server's bounded
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
use mogwai_protocol::{ClientMessage, InstrumentDef, ServerMessage, SimClock, Symbol, TradeTick};
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
use nautilus_core::{Params, UnixNanos};
use nautilus_model::{
    data::{Bar, BarType, Data, bar::get_bar_interval_ns},
    enums::BarAggregation,
    identifiers::{ClientId, Venue},
};
use nautilus_network::http::HttpClient;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::{
    MOGWAI_VENUE, MogwaiDataClientConfig,
    client::shared::{
        HavocDelivery, HavocFilter, abort_tasks, cache_instruments, capped_limit, client_havoc,
        conn_havoc, date_to_unix_nanos, emit_seeded_instruments, enqueue_havoc, ensure_instrument,
        ensure_on_tape, fetch_clock_or_identity, fetch_instruments, flush_havoc_into_pump,
        instrument_any_or_warn, instrument_def, join_url, lock_recover, now_unix_nanos,
        seed_instruments, spawn_latency_pump, symbol_from_instrument, track_task, wait_connected,
        warn_missing_instrument_once,
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
    ws_cmd: Option<UnboundedSender<WsCommand>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: Arc<Mutex<HashMap<Symbol, SubState>>>,
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
            ws_cmd: None,
            instruments: Arc::new(Mutex::new(HashMap::new())),
            subs: Arc::new(Mutex::new(HashMap::new())),
            bars: Arc::new(Mutex::new(HashMap::new())),
            task_handles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn subscribe_symbol(
        &mut self,
        symbol: Symbol,
        kind: SubKind,
        start_ts: Option<u64>,
    ) -> anyhow::Result<()> {
        // The subscription is satisfied LOCALLY. Nautilus still calls
        // subscribe/unsubscribe and this client must still implement them, but
        // the venue serves one run's one tape and pushes it unbidden, so there
        // is no frame to send: this table only gates which arriving ticks are
        // forwarded to the message bus.
        // The venue serves exactly one instrument and pushes its tape unbidden,
        // so a subscribe for any OTHER symbol can never be satisfied. Refuse it
        // here, loudly and locally, rather than recording a subscription that
        // will silently never deliver: silence is precisely the misbinding
        // defect this lifecycle exists to remove. An empty instrument cache
        // means the client has not connected yet and has nothing to check
        // against, so it defers rather than guessing.
        {
            let instruments = self
                .instruments
                .lock()
                .map_err(|_| anyhow::anyhow!("instrument mutex poisoned"))?;
            if !instruments.is_empty() && !instruments.contains_key(&symbol) {
                let served: Vec<&str> = instruments.keys().map(String::as_str).collect();
                tracing::error!(
                    %symbol,
                    served = ?served,
                    "refusing a subscription for an instrument this run does not serve"
                );
                anyhow::bail!(
                    "this venue run serves {served:?}, not {symbol}; \
                     one run is one instrument and cannot be asked for another"
                );
            }
        }
        {
            let mut subs = self
                .subs
                .lock()
                .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?;
            let state = subs.entry(symbol).or_default();
            // The 0->1 transition. It no longer emits anything - there is no
            // wire subscribe - but it is still what seeds the shared resume
            // cursor, and only the FIRST subscriber may.
            let first = state.total() == 0;
            state.increment(kind);
            // `start_ts` is this symbol's single shared resume cursor, advanced
            // forward-only by `advance_sub_start_ts` on every delivered tick so a
            // reconnect resumes after the last delivered tick instead of replaying.
            // Only the FIRST subscriber (the 0->1 transition, `emit`) seeds it. The
            // old `min(existing, new)` on a later subscriber pulled the cursor
            // BACKWARD to an earlier requested start (AD7); since a live second
            // subscribe never actually sends a Subscribe on the WS path (that fires
            // only on 0->1) and the poll path ignores the update, the earlier start
            // was never delivered as backfill - it only corrupted the resume cursor,
            // so the NEXT reconnect replayed an already-delivered window. A later
            // subscriber must not move the shared cursor at all: not backward (the
            // replay), and not forward either (that would skip data the first
            // subscriber still wants). The shared feed already serves every
            // subscriber from the cursor forward.
            if first {
                state.start_ts = start_ts;
            }
        }

        // Nothing is sent to the venue. The subscription is satisfied entirely
        // by this local table: the WS reader forwards an arriving tick when the
        // matching kind's count is non-zero, and the venue pushes the run's one
        // tape whether or not anybody asked. The seeded `start_ts` survives as
        // the resume cursor the historical request paths read.
        Ok(())
    }

    #[allow(clippy::needless_pass_by_value)]
    /// The mirror of `subscribe_symbol`, and equally local: dropping the last
    /// subscriber of every kind retires the symbol's row, which stops the WS
    /// reader forwarding its ticks. Nothing is sent to the venue, which keeps
    /// pushing the run's one tape either way.
    fn unsubscribe_symbol(&mut self, symbol: Symbol, kind: SubKind) -> anyhow::Result<()> {
        let mut subs = self
            .subs
            .lock()
            .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?;
        if let Some(state) = subs.get_mut(&symbol) {
            state.decrement(kind);
            if state.total() == 0 {
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
        self.connected.store(false, Ordering::Relaxed);
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
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
        let http_base_url = self.config.http_base_url();
        let (server, floor_known) = fetch_clock_or_identity(&self.http, &http_base_url).await;
        let sim = server.sim;
        self.sim = sim;
        self.data_origin_ns = floor_known.then_some(server.data_origin_ns);
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
        // Server-side divergences are execution-owned. The data client accepts
        // the same config object but only applies its client-side transport half.
        let client_havoc = client_havoc(&self.config.havoc);

        // `ws_url` already carries the `/ws` path and the account query; do not
        // join a path onto it (see its comment).
        let ws_url = self.config.ws_url();
        let (cmd_tx, cmd_rx) = unbounded_channel::<WsCommand>();
        self.ws_cmd = Some(cmd_tx);

        let connected = Arc::clone(&self.connected);
        let instruments = Arc::clone(&self.instruments);
        let subs = Arc::clone(&self.subs);
        let bars = Arc::clone(&self.bars);
        let havoc_filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_client(
            &client_havoc,
        )));
        // The market-data drain no longer sleeps the per-message havoc latency
        // inline in the reader loop (which capped throughput at ~33 msg/s and
        // head-of-line-blocked pings/commands - AD4). It enqueues each filtered
        // message, arrival-anchored, into a latency pump that owns the sink and
        // paces delivery off-loop. Spawn and track the pump before the reader so
        // stop() aborts it alongside the connection task.
        let (deliver_tx, deliver_rx) = unbounded_channel::<HavocDelivery>();
        let pump_handle = spawn_latency_pump(deliver_rx, move |msg| {
            let sink = sink.clone();
            let instruments = Arc::clone(&instruments);
            let subs = Arc::clone(&subs);
            let bars = Arc::clone(&bars);
            async move {
                handle_market_message(msg, &sink, &instruments, &subs, &bars, sim).await;
            }
        });
        track_task(&self.task_handles, pump_handle);

        let handler_filter = Arc::clone(&havoc_filter);
        let handler_deliver = deliver_tx.clone();
        let disconnect_filter = Arc::clone(&havoc_filter);
        let disconnect_deliver = deliver_tx;
        let task_ws_url = ws_url.clone();
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: client_havoc.seed,
                    connected,
                    sim,
                    label: "data",
                },
                cmd_rx,
                ws_command_to_client_message,
                // The venue pushes the one run's tape unbidden, so a reattach
                // has no subscribe frames to replay: subscription state is
                // satisfied locally in this client and never reaches the wire.
                Vec::new,
                move |server_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let handler_deliver = handler_deliver.clone();
                    async move {
                        let mut filter = handler_filter.lock().await;
                        enqueue_havoc(&mut filter, server_msg, sim, &handler_deliver);
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
            if let Some(handle) = lock_recover(&self.task_handles, "task handles").pop() {
                handle.abort();
            }
            self.ws_cmd = None;
            return Err(err);
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
        self.subscribe_symbol(symbol, SubKind::Quotes, start_ts_param(&cmd.params))
    }

    fn subscribe_trades(&mut self, cmd: SubscribeTrades) -> anyhow::Result<()> {
        let symbol = symbol_from_instrument(cmd.instrument_id);
        self.subscribe_symbol(symbol, SubKind::Trades, start_ts_param(&cmd.params))
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
            let mut bars = self
                .bars
                .lock()
                .map_err(|_| anyhow::anyhow!("bar mutex poisoned"))?;
            bars.entry(cmd.bar_type).or_default().refs += 1;
        }
        self.subscribe_symbol(symbol, SubKind::Bars, start_ts_param(&cmd.params))
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
        // Refuse an off-tape window at the boundary, loudly - but ANSWER it.
        // Returning the error to nautilus is not a refusal the requester ever
        // sees: `DataEngine::execute` log::error!s a synchronous client error
        // and emits no correlated response, so `?` here leaves the request
        // outstanding forever and the consumer burns its whole timeout on what
        // looks like a hung venue. Log the named diagnostic and answer empty.
        if let Err(err) = ensure_on_tape(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_trades: refusing an off-tape window; answering with an empty trade response so the request resolves");
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
                        // drop: a server 422 (off-tape) or any fetch error must be
                        // visible, not mistaken for "no trades in the window".
                        tracing::error!(%symbol, error = %err, "request_trades: trade fetch failed (the server may have refused an off-tape window); answering with an empty trade response so the request resolves");
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
                        "request_trades: window truncated before its end (trade limit reached or same-ts wedge); the warmup/live splice may not be contiguous"
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
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        track_task(
            &self.task_handles,
            get_runtime().spawn(async move {
                let response = QuotesResponse::new(
                    request.request_id,
                    client_id,
                    request.instrument_id,
                    Vec::new(),
                    date_to_unix_nanos(request.start),
                    date_to_unix_nanos(request.end),
                    now_unix_nanos(sim),
                    request.params,
                );
                drop(sink.send(DataEvent::Response(DataResponse::Quotes(response))));
            }),
        );
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
        // Refuse an off-tape warmup window at the boundary, naming the floor -
        // but ANSWER it, for the reason spelled out in `request_trades`: a
        // synchronous `Err` is logged by the data engine and never turned into
        // a response, so `?` here would hang the warmup rather than refuse it.
        if let Err(err) = ensure_on_tape(start, self.data_origin_ns) {
            tracing::error!(error = %err, "request_bars: refusing an off-tape warmup window; answering with an empty bar response so the request resolves");
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
            // BAR-count limit as a TRADE-page limit, so a warmup for N bars
            // fetched at most N trades (~N/5 bars on the fitted tape) covering
            // only the oldest edge, under-delivering or timing out the warmup.
            let bar_limit = request.limit.map(std::num::NonZeroUsize::get);
            let interval = get_bar_interval_ns(&request.bar_type).as_u64();
            // Every exit from this block yields bars, so the response below is
            // ALWAYS sent. A failure arm used to `return` straight out of the
            // task, which left the nautilus request unresolved forever: from the
            // consumer that is indistinguishable from a hang, and it burns the
            // whole warmup timeout before dying with nothing but a line in the
            // worker log to show for it. An empty response is a truthful answer
            // that at least RESOLVES; the error detail rides the log.
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
                        tracing::error!(%symbol, error = %err, "request_bars: trade fetch failed (the server may have refused an off-tape window); answering with an empty bar response so the request resolves");
                        break 'bars Vec::new();
                    }
                };
                if truncated {
                    tracing::warn!(
                        %symbol,
                        "request_bars: window truncated before its end (bar limit reached or same-ts wedge); the warmup may not splice contiguously into live"
                    );
                }
                let mut bars = aggregate_bars(&request.bar_type, &trades, &def, sim, end);
                if let Some(m) = bar_limit {
                    // Paging spans at least `bar_limit` intervals, so it may produce
                    // a few extra bars; trim to the requested count (oldest edge,
                    // from the window start) so the response honors the bar limit.
                    bars.truncate(m);
                }
                // An on-tape window that under-delivers is a real, reachable
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
                        "request_bars: the window is on-tape but produced fewer bars than requested; \
                         mogwai's synthetic tape has multi-hour arrival droughts, so a short warmup \
                         window can legitimately be sparse or empty - widen the window, lower the bar \
                         interval, or let the venue run further past its epoch before starting the warmup"
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
    start_ts: Option<u64>,
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

#[derive(Debug, Clone, PartialEq)]
enum WsCommand {}

/// Maps an outbound `WsCommand` to the mogwai wire `ClientMessage` the writer
/// task serializes. Kept as a named function so the subscribe-variant wiring is
/// unit-testable without a live socket.
#[allow(clippy::needless_pass_by_value)]
fn ws_command_to_client_message(cmd: WsCommand) -> ClientMessage {
    match cmd {}
}
async fn handle_market_message(
    msg: ServerMessage,
    sink: &UnboundedSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
    sim: SimClock,
) {
    match msg {
        ServerMessage::Trade(trade) => {
            emit_trade(&trade, sink, instruments, subs, bars, sim);
        }
        ServerMessage::Quote(quote) => {
            let Some(def) = instrument_def(instruments, &quote.symbol) else {
                warn_missing_instrument_once(&quote.symbol);
                return;
            };
            let state = sub_state(subs, &quote.symbol);
            if state.as_ref().is_some_and(|s| s.quotes > 0) {
                let id = convert::instrument_id(&def);
                match convert::quote_tick(&quote, id, &def, now_unix_nanos(sim)) {
                    Ok(tick) => drop(sink.send(DataEvent::Data(Data::Quote(tick)))),
                    Err(err) => tracing::warn!(
                        symbol = %quote.symbol,
                        error = %err,
                        "dropping quote: unrepresentable tick"
                    ),
                }
            }
        }
        ServerMessage::Heartbeat { .. } => {
            tracing::trace!("ignoring server heartbeat on data path");
        }
        ServerMessage::ProtocolError { reason, .. } => {
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
        ServerMessage::RunComplete {
            sim_now_ns,
            elapsed_ns,
        } => {
            // The lifecycle owns the terminal transition and suppresses its
            // reconnect.  Keep an explicit completion record on the data leg
            // so a finished run is never mistaken for a quiet failed feed.
            tracing::info!(sim_now_ns, elapsed_ns, "venue run completed on data socket");
        }
        _ => {}
    }
}

fn emit_trade(
    trade: &TradeTick,
    sink: &UnboundedSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
    sim: SimClock,
) {
    let Some(def) = instrument_def(instruments, &trade.symbol) else {
        warn_missing_instrument_once(&trade.symbol);
        return;
    };
    // Advance this subscription's resume cursor to just past the delivered tick.
    // On the WS path a reconnect re-issues `Subscribe { start_ts }`; pinning
    // start_ts at the original subscription instant made the server replay the
    // whole history on every reconnect (the connection-lifecycle havoc surface
    // floods duplicate ticks). Advancing to `ts_event + 1` (exclusive of the
    // delivered ts) mirrors the polling path's PollCursor so a reconnect resumes
    // instead of replaying. The first subscribe still uses the originally
    // requested instant because this only moves the cursor forward.
    advance_sub_start_ts(subs, &trade.symbol, trade.ts_event);
    let state = sub_state(subs, &trade.symbol);
    let id = convert::instrument_id(&def);
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
        emit_live_bars(trade, &def, sink, bars, sim);
    }
}

fn emit_live_bars(
    trade: &mogwai_protocol::TradeTick,
    def: &InstrumentDef,
    sink: &UnboundedSender<DataEvent>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
    sim: SimClock,
) {
    let mut ready = Vec::new();
    {
        let mut bars = lock_recover(bars, "bar");
        for (bar_type, state) in bars.iter_mut() {
            if bar_type.instrument_id() != convert::instrument_id(def) || state.refs == 0 {
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
    // silently dropped (the always-stale/missing last bar of every warmup). If
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

/// Advances a subscription's `start_ts` resume cursor to just past `ts_event`
/// (exclusive) so a WS reconnect resumes after the last delivered tick rather
/// than replaying from the original subscription instant. Only ever moves the
/// cursor forward; same-ns ticks already delivered before the reconnect are not
/// re-requested. Saturates at `u64::MAX` to never wrap.
fn advance_sub_start_ts(subs: &Arc<Mutex<HashMap<Symbol, SubState>>>, symbol: &str, ts_event: u64) {
    let next = ts_event.saturating_add(1);
    let mut subs = lock_recover(subs, "subscription");
    if let Some(state) = subs.get_mut(symbol) {
        state.start_ts = Some(state.start_ts.map_or(next, |existing| existing.max(next)));
    }
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
    quotes: usize,
    bars: usize,
}
impl From<&SubState> for SubStateSnapshot {
    fn from(state: &SubState) -> Self {
        Self {
            trades: state.trades,
            quotes: state.quotes,
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
/// exists so a server that kept answering full pages without advancing the
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
        let next = page.last().map(|trade| trade.ts_event.saturating_add(1));
        out.extend(page);
        if stop(&out) {
            return Ok((out, true));
        }
        if !full {
            return Ok((out, false));
        }
        let Some(next) = next else {
            return Ok((out, false));
        };
        start = Some(UnixNanos::from(next));
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
fn start_ts_param(params: &Option<Params>) -> Option<u64> {
    params
        .as_ref()
        .and_then(|params| params.get_u64("start_ts"))
}
