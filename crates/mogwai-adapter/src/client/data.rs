//! `MogwaiDataClient`: the `DataClient` half of the adapter. Owns the
//! subscription table, the poll/WS transport choice, the live bar
//! aggregator, and the request handlers that page the server's bounded
//! `/trades` scan. Plumbing shared with the execution half (the havoc
//! dispatch pipeline, the instrument cache, clock/url glue) lives in
//! `super::shared`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use mogwai_protocol::{
    ClientMessage, InstrumentDef, MarketRegime, ServerMessage, SimClock, Symbol, TradeTick,
};
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
use rust_decimal::Decimal;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::{
    MOGWAI_VENUE, MogwaiDataClientConfig,
    client::shared::{
        HavocDelivery, HavocFilter, abort_tasks, cache_instruments, capped_limit, client_havoc,
        conn_havoc, data_regime, date_to_unix_nanos, drain_havoc_anchored, emit_seeded_instruments,
        enqueue_havoc, ensure_instrument, ensure_on_tape, fetch_clock_or_identity,
        fetch_instruments, flush_havoc, flush_havoc_into_pump, instrument_any_or_warn,
        instrument_def, join_url, lock_recover, now_unix_nanos, seed_instruments,
        spawn_latency_pump, symbol_from_instrument, track_task, wait_connected,
        warn_missing_instrument_once,
    },
    convert,
    lifecycle::{HttpQuota, WsConnectionConfig, run_ws_connection},
};

/// Cadence of the `HttpPolling` data path's `/trades` pull. This stays a WALL
/// duration and is deliberately NOT scaled by the sim clock: the endpoint it
/// polls is the `ORIGIN_TS`-anchored history seek path, which the coherent-clock
/// work leaves OFF the accelerated axis (see `reference/clock.md` and the
/// coherent-clock spec). Scaling it would only spin the poll loop faster under
/// acceleration without producing on-axis data. The accelerated vehicle is the
/// WS push path, which deadline-paces server-side.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Consecutive poll-failure count at which the wedged-feed warning re-fires and
/// the AD6 self-heal (clock re-fetch + cursor reset) runs - onset (count 1),
/// then every this-many failures (~10s at the 250ms POLL_INTERVAL).
const POLL_FAILURE_ALERT_EVERY: u64 = 40;

#[derive(Debug)]
pub struct MogwaiDataClient {
    client_id: ClientId,
    config: MogwaiDataClientConfig,
    connected: Arc<AtomicBool>,
    sink: Option<UnboundedSender<DataEvent>>,
    http: HttpClient,
    http_quota: HttpQuota,
    sim: SimClock,
    /// Earliest `ts_event` the venue can serve, learned from `/clock` at connect.
    /// `0` means unknown (clock fetch failed); the warmup guard skips its
    /// pre-flight refusal in that case and defers to the server's own 422.
    data_origin_ns: u64,
    ws_cmd: Option<UnboundedSender<WsCommand>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: Arc<Mutex<HashMap<BarType, BarSubState>>>,
    poll_cursor: Arc<Mutex<HashMap<Symbol, PollCursor>>>,
    /// Handles for every task this client spawns (the WS reader, the poll loop,
    /// and each short-lived `request_*`/`subscribe_instrument*` fetch). Shared
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
            data_origin_ns: 0,
            ws_cmd: None,
            instruments: Arc::new(Mutex::new(HashMap::new())),
            subs: Arc::new(Mutex::new(HashMap::new())),
            bars: Arc::new(Mutex::new(HashMap::new())),
            poll_cursor: Arc::new(Mutex::new(HashMap::new())),
            task_handles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    fn subscribe_symbol(
        &mut self,
        symbol: Symbol,
        kind: SubKind,
        start_ts: Option<u64>,
    ) -> anyhow::Result<()> {
        // A live subscribe sends `start_ts = None` and lets the server seek the
        // shared tape to sim-now. The old default anchored a fresh subscribe at
        // `sim_epoch_ns` (the tape origin), which under acceleration made the
        // server replay the whole backfill at once - the catch-up dump. "Live
        // means from now" is now the server's job; the adapter only forwards an
        // explicit caller-supplied start (a resume cursor) unchanged.
        let (emit, active_start_ts) = {
            let mut subs = self
                .subs
                .lock()
                .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?;
            let state = subs.entry(symbol.clone()).or_default();
            let emit = state.total() == 0;
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
            if emit {
                state.start_ts = start_ts;
            }
            (emit, state.start_ts)
        };

        // Gate the direct Subscribe send on a live connection. A subscribe that
        // lands during a reconnect backoff (`connected == false`) must NOT queue a
        // Subscribe: the queued command survives the reconnect in the unbounded
        // command channel and would be sent AGAIN after `on_connect`
        // (`subscribe_commands`) already rebuilt the full subscription state from
        // the table, double-subscribing the symbol and restarting the server-side
        // replay - the resubscribe duplicate-window bug (AD5). While disconnected,
        // `on_connect` is the sole post-reconnect subscribe source, and it
        // reconstructs this subscription from the table the increment above just
        // updated, so nothing is lost by skipping the send here.
        if emit
            && !self.config.transport_profile.data_by_polling()
            && self.connected.load(Ordering::Relaxed)
        {
            self.send_ws(WsCommand::Subscribe {
                symbols: vec![symbol],
                start_ts: active_start_ts,
                regime: data_regime(&self.config.havoc),
            })?;
        }
        Ok(())
    }

    fn unsubscribe_symbol(&mut self, symbol: Symbol, kind: SubKind) -> anyhow::Result<()> {
        let mut emit = false;
        {
            let mut subs = self
                .subs
                .lock()
                .map_err(|_| anyhow::anyhow!("subscription mutex poisoned"))?;
            if let Some(state) = subs.get_mut(&symbol) {
                state.decrement(kind);
                emit = state.total() == 0;
            }
            if emit {
                subs.remove(&symbol);
            }
        }

        if emit {
            self.poll_cursor
                .lock()
                .map_err(|_| anyhow::anyhow!("poll cursor mutex poisoned"))?
                .remove(&symbol);
        }

        // Gate the Unsubscribe send on a live connection, mirroring the subscribe
        // path (AD5). While disconnected the symbol was already removed from the
        // table above, so `on_connect` will simply not re-subscribe it on the next
        // reconnect - there is nothing to unsubscribe on a dead socket, and queuing
        // a command that outlives the reconnect only risks racing a fresh
        // subscribe's rebuilt state.
        if emit
            && !self.config.transport_profile.data_by_polling()
            && self.connected.load(Ordering::Relaxed)
        {
            self.send_ws(WsCommand::Unsubscribe {
                symbols: vec![symbol],
            })?;
        }
        Ok(())
    }

    fn send_ws(&self, cmd: WsCommand) -> anyhow::Result<()> {
        let tx = self
            .ws_cmd
            .as_ref()
            .context("mogwai data client is not connected")?;
        tx.send(cmd).context("send websocket command")
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
            match active_to_bar(*bar_type, active, &def, self.sim) {
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
        self.poll_cursor
            .lock()
            .map_err(|_| anyhow::anyhow!("poll cursor mutex poisoned"))?
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
        let server = fetch_clock_or_identity(&self.http, &http_base_url).await;
        let sim = server.sim;
        self.sim = sim;
        self.data_origin_ns = server.data_origin_ns;
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
        let regime = data_regime(&self.config.havoc);

        if self.config.transport_profile.data_by_polling() {
            let connected = Arc::clone(&self.connected);
            connected.store(true, Ordering::Relaxed);
            let http = self.http.clone();
            let http_quota = self.http_quota.clone();
            let instruments = Arc::clone(&self.instruments);
            let subs = Arc::clone(&self.subs);
            let bars = Arc::clone(&self.bars);
            let cursor = Arc::clone(&self.poll_cursor);
            let havoc_filter = HavocFilter::from_client(&client_havoc);
            let poll_handle = tokio::spawn(async move {
                poll_market_data(DataPollContext {
                    http,
                    http_quota,
                    base: http_base_url,
                    sink,
                    instruments,
                    subs,
                    bars,
                    cursor,
                    connected,
                    havoc_filter,
                    regime,
                    sim,
                })
                .await;
            });
            track_task(&self.task_handles, poll_handle);
            return Ok(());
        }

        let ws_url = join_url(&self.config.ws_url(), "ws");
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
        let connect_subs = Arc::clone(&self.subs);
        let task_ws_url = ws_url.clone();
        let reader_handle = tokio::spawn(async move {
            run_ws_connection(
                WsConnectionConfig {
                    ws_url: task_ws_url,
                    conn,
                    seed: client_havoc.seed,
                    connected,
                    sim,
                },
                cmd_rx,
                ws_command_to_client_message,
                move || subscribe_commands(&connect_subs, regime),
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
                    match active_to_bar(cmd.bar_type, &active, &def, self.sim) {
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
        let regime = data_regime(&self.config.havoc);
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        let end = date_to_unix_nanos(request.end);
        // Refuse an off-tape window at the boundary, loudly, rather than spawning
        // a doomed fetch the warmup would read as an empty page.
        ensure_on_tape(start, self.data_origin_ns)?;
        track_task(&self.task_handles, get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            let def = match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
            {
                Ok(def) => def,
                Err(err) => {
                    tracing::error!(%symbol, error = %err, "request_trades: instrument lookup failed");
                    return;
                }
            };
            // Page the window rather than issuing one MAX_HISTORY_LIMIT-capped
            // request: a window with more than 1000 trades used to return only
            // its oldest 1000, with the response still claiming the full range.
            // `request.limit` counts TRADES here, so it becomes the pagination
            // ceiling.
            let max_trades = request.limit.map(std::num::NonZeroUsize::get);
            let (mut trades, truncated) = match fetch_trades_windowed(
                &http,
                &http_quota,
                &base,
                TradeFetch {
                    symbol: &symbol,
                    start,
                    end,
                    limit: None,
                    regime,
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
                    tracing::error!(%symbol, error = %err, "request_trades: trade fetch failed; the server may have refused an off-tape window");
                    return;
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
            let data = trades
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
                .collect();
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
        let regime = data_regime(&self.config.havoc);
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        let sim = self.sim;
        let start = date_to_unix_nanos(request.start);
        let end = date_to_unix_nanos(request.end);
        // Refuse an off-tape warmup window at the boundary, naming the floor,
        // rather than spawning a fetch that returns an empty page the warmup
        // can never complete on (the #13 failure mode this spec closes).
        ensure_on_tape(start, self.data_origin_ns)?;
        track_task(&self.task_handles, get_runtime().spawn(async move {
            let instrument_id = request.bar_type.instrument_id();
            let symbol = symbol_from_instrument(instrument_id);
            let def = match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
            {
                Ok(def) => def,
                Err(err) => {
                    tracing::error!(%symbol, error = %err, "request_bars: instrument lookup failed");
                    return;
                }
            };
            // Page the window, translating nautilus's BAR-count limit into a
            // bar-span pagination target: the old single request applied a
            // BAR-count limit as a TRADE-page limit, so a warmup for N bars
            // fetched at most N trades (~N/5 bars on the fitted tape) covering
            // only the oldest edge, under-delivering or timing out the warmup.
            let bar_limit = request.limit.map(std::num::NonZeroUsize::get);
            let interval = get_bar_interval_ns(&request.bar_type).as_u64();
            let (trades, truncated) = match fetch_trades_windowed(
                &http,
                &http_quota,
                &base,
                TradeFetch {
                    symbol: &symbol,
                    start,
                    end,
                    limit: None,
                    regime,
                },
                |out| bar_span_reached(out, interval, bar_limit),
            )
            .await
            {
                Ok(result) => result,
                Err(err) => {
                    tracing::error!(%symbol, error = %err, "request_bars: trade fetch failed; the server may have refused an off-tape window");
                    return;
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
                if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                    cache_instruments(&instruments, defs.clone());
                    let ts_init = now_unix_nanos(sim);
                    let data = defs
                        .iter()
                        .filter_map(|def| instrument_any_or_warn(def, ts_init))
                        .collect();
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
                }
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
                if let Ok(def) =
                    ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
                {
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
                        drop(
                            sink.send(DataEvent::Response(DataResponse::Instrument(Box::new(
                                response,
                            )))),
                        );
                    }
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
    active: Option<ActiveBar>,
}

/// Per-symbol resume cursor for the HTTP-polling data path.
///
/// COUPLING: this dedup is correct only if the server's `/trades` scan returns
/// trades in ascending `ts_event` with a stable order among same-`ts_event`
/// trades across consecutive batches. `unseen_from_batch` resumes by skipping
/// the first `emitted_at_last_ts` trades at `last_ts` (the count already
/// delivered at that timestamp) before forwarding the rest, then re-derives
/// `last_ts`/`emitted_at_last_ts` from the batch maximum. If the server ever
/// returned descending or unstably-ordered same-ns trades, the skip would drop
/// the wrong trades (under-deliver) or fail to skip duplicates (over-deliver).
/// The server contract that backs this lives in `mogwai-server`'s bounded
/// `/trades` seek (and is asserted by its replay-cursor test); this cursor
/// depends on it but cannot enforce it here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PollCursor {
    last_ts: u64,
    emitted_at_last_ts: usize,
}

impl PollCursor {
    fn new(start_ts: Option<u64>) -> Self {
        Self {
            last_ts: start_ts.unwrap_or(0),
            emitted_at_last_ts: 0,
        }
    }

    /// Returns the trades in `batch` not already delivered, advancing the
    /// cursor. Assumes `batch` is ascending by `ts_event` (see the type-level
    /// COUPLING note).
    fn unseen_from_batch(&mut self, batch: Vec<TradeTick>) -> Vec<TradeTick> {
        let mut skipped = 0;
        let mut out = Vec::new();
        for trade in batch {
            // Defense in depth against a server that rewinds: a trade stamped
            // BEFORE the cursor's high-water mark was already delivered (or
            // belongs to a window the cursor has moved past), so re-emitting it
            // would duplicate ticks downstream. The primary trigger (the data
            // crate's exact-ts checkpoint off-by-one) is fixed server-side, so
            // this only fires if some future server bug returns descending ts;
            // skipping it keeps the cursor robust against rewinds by
            // construction rather than trusting the contract.
            if trade.ts_event < self.last_ts {
                continue;
            }
            if trade.ts_event == self.last_ts && skipped < self.emitted_at_last_ts {
                skipped += 1;
                continue;
            }
            out.push(trade);
        }

        if let Some(last_ts) = out.iter().map(|trade| trade.ts_event).max() {
            let old_last_ts = self.last_ts;
            let emitted_at_last_ts = out.iter().filter(|trade| trade.ts_event == last_ts).count();
            self.last_ts = last_ts;
            self.emitted_at_last_ts = if last_ts == old_last_ts {
                self.emitted_at_last_ts + emitted_at_last_ts
            } else {
                emitted_at_last_ts
            };
        }

        out
    }
}

#[derive(Debug)]
struct ActiveBar {
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
    close_ts: u64,
}

#[derive(Debug, Clone, PartialEq)]
enum WsCommand {
    Subscribe {
        symbols: Vec<Symbol>,
        start_ts: Option<u64>,
        regime: Option<MarketRegime>,
    },
    Unsubscribe {
        symbols: Vec<Symbol>,
    },
}

/// Maps an outbound `WsCommand` to the mogwai wire `ClientMessage` the writer
/// task serializes. Kept as a named function so the subscribe-variant wiring is
/// unit-testable without a live socket.
fn ws_command_to_client_message(cmd: WsCommand) -> ClientMessage {
    match cmd {
        WsCommand::Subscribe {
            symbols,
            start_ts,
            regime,
        } => ClientMessage::Subscribe {
            symbols,
            start_ts,
            regime,
        },
        WsCommand::Unsubscribe { symbols } => ClientMessage::Unsubscribe { symbols },
    }
}
fn subscribe_commands(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    regime: Option<MarketRegime>,
) -> Vec<WsCommand> {
    let symbols = lock_recover(subs, "subscription")
        .iter()
        .filter(|(_, state)| state.total() > 0)
        .map(|(symbol, state)| (symbol.clone(), state.start_ts))
        .collect::<Vec<_>>();
    symbols
        .into_iter()
        .map(|(symbol, start_ts)| WsCommand::Subscribe {
            symbols: vec![symbol],
            start_ts,
            regime,
        })
        .collect()
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
            // The server sends ProtocolError to diagnose an unservable subscribe
            // (unknown symbol, exhausted positioning seek, pre-origin start_ts
            // clamp) as well as a decode failure. Swallowing it here left the
            // feed silent with no downstream signal, indistinguishable from a
            // quiet market. Match the exec path and surface the reason so a dead
            // feed is visible in the adapter's own logs.
            tracing::warn!(
                %reason,
                "venue reported a protocol error on the data path; a subscribe may have been refused (unknown symbol, exhausted seek, or pre-origin start)"
            );
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
struct DataPollContext {
    http: HttpClient,
    http_quota: HttpQuota,
    base: String,
    sink: UnboundedSender<DataEvent>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: Arc<Mutex<HashMap<BarType, BarSubState>>>,
    cursor: Arc<Mutex<HashMap<Symbol, PollCursor>>>,
    connected: Arc<AtomicBool>,
    havoc_filter: HavocFilter,
    regime: Option<MarketRegime>,
    sim: SimClock,
}

async fn poll_market_data(mut ctx: DataPollContext) {
    // Per-symbol consecutive fetch-failure counters. A persistent failure means
    // a dead feed (the server-restart/off-tape-cursor class): the poll cursor
    // and clock are fetched once at connect, so a restarted server with a fresh
    // data_origin 422s every `GET /trades` forever, indistinguishable from a
    // quiet market. The old `let Ok(batch) = ... else { continue }` swallowed it
    // silently; we now warn - but rate-limited, so a wedged feed does not emit
    // ~4 warns/sec/symbol. Warn on the first failure (onset) and roughly every
    // 10s thereafter (POLL_INTERVAL is 250ms), and log a one-line recovery when
    // a previously-failing symbol fetches cleanly again.
    let mut poll_failures: HashMap<Symbol, u64> = HashMap::new();
    while ctx.connected.load(Ordering::Relaxed) {
        let symbols = poll_symbols(&ctx.subs);
        // Drop failure counters for symbols no longer subscribed (F13): the
        // counter was previously removed only on a successful fetch, so a symbol
        // unsubscribed WHILE its poll was failing left a stale entry that never
        // cleared. Retaining only currently-polled symbols each cycle bounds the
        // map by the live subscription set.
        {
            let active: std::collections::HashSet<&Symbol> =
                symbols.iter().map(|(symbol, _)| symbol).collect();
            poll_failures.retain(|symbol, _| active.contains(symbol));
        }
        for (symbol, start_ts) in symbols {
            // Self-heal a missing instrument def on the poll path: a symbol
            // streaming without a seeded def is otherwise silently black-holed
            // by `emit_trade`. `ensure_instrument` is a cheap map hit when the
            // def is already present and re-seeds only on a genuine miss; the
            // async/HTTP context the drain lacks lives here, so this is where
            // the re-seed belongs. A genuine miss still surfaces once per symbol
            // via the drain's warn.
            if instrument_def(&ctx.instruments, &symbol).is_none()
                && let Err(err) = ensure_instrument(
                    &ctx.http,
                    &ctx.http_quota,
                    &ctx.base,
                    &ctx.instruments,
                    &symbol,
                )
                .await
            {
                // Best-effort re-seed: a still-missing def falls through to the
                // drain's once-per-symbol warn below, so swallow the fetch error
                // here at debug rather than warn twice for the same cause.
                tracing::debug!(%symbol, error = %err, "poll-path instrument re-seed failed");
            }
            // A fresh subscribe carries no start (the server seeks live to
            // sim-now); the poll cursor must anchor there too. Defaulting to 0
            // would send `start=0`, which the boot-derived-origin server refuses
            // with a 422 (0 precedes data_origin) - the poll would loop forever
            // on the rejection and emit nothing. Anchor an absent start at sim-now
            // so the first poll fetches an on-tape window.
            let poll_anchor = start_ts.or_else(|| Some(now_unix_nanos(ctx.sim).as_u64()));
            let start = {
                let mut cursors = lock_recover(&ctx.cursor, "poll cursor");
                let entry = cursors
                    .entry(symbol.clone())
                    .or_insert_with(|| PollCursor::new(poll_anchor));
                UnixNanos::from(entry.last_ts)
            };
            let batch = match fetch_trades(
                &ctx.http,
                &ctx.http_quota,
                &ctx.base,
                TradeFetch {
                    symbol: &symbol,
                    start: Some(start),
                    end: None,
                    limit: None,
                    regime: ctx.regime,
                },
            )
            .await
            {
                Ok(batch) => {
                    if poll_failures.remove(&symbol).is_some() {
                        tracing::info!(%symbol, "poll fetch recovered");
                    }
                    batch
                }
                Err(err) => {
                    let count = poll_failures.entry(symbol.clone()).or_insert(0);
                    *count += 1;
                    let count = *count;
                    if count == 1 || count.is_multiple_of(POLL_FAILURE_ALERT_EVERY) {
                        tracing::warn!(
                            %symbol,
                            error = %err,
                            failures = count,
                            "poll fetch failed; the feed may be dead (server restart with a \
                             fresh data_origin, or an off-tape cursor)"
                        );
                    }
                    // Self-heal a persistently wedged feed (AD6). A run of
                    // consecutive failures is the server-restart / off-tape-cursor
                    // class: the cursor's last_ts (or the sim-now anchor computed
                    // against the pre-restart clock) now precedes the server's
                    // fresh data_origin, so every GET /trades 422s forever - and
                    // batch 3 only made that VISIBLE (the warn above), not
                    // recoverable. Re-fetch /clock to pick up the restarted
                    // server's sim mapping and origin, then reset this symbol's
                    // cursor onto the new tape (its old start floored up to the new
                    // data_origin) so the next poll requests an on-tape window.
                    // Done once per alert interval, not every failure, to avoid a
                    // fetch_clock storm while the server is genuinely down; if it
                    // is, the re-fetch falls back to identity and the next interval
                    // retries.
                    if count.is_multiple_of(POLL_FAILURE_ALERT_EVERY) {
                        let refreshed = fetch_clock_or_identity(&ctx.http, &ctx.base).await;
                        ctx.sim = refreshed.sim;
                        let requested =
                            start_ts.unwrap_or_else(|| now_unix_nanos(ctx.sim).as_u64());
                        let anchor = requested.max(refreshed.data_origin_ns);
                        lock_recover(&ctx.cursor, "poll cursor")
                            .insert(symbol.clone(), PollCursor::new(Some(anchor)));
                        tracing::warn!(
                            %symbol,
                            anchor,
                            "poll feed self-heal: re-fetched clock and reset the cursor \
                             after persistent failures"
                        );
                    }
                    continue;
                }
            };
            let trades = {
                let mut cursors = lock_recover(&ctx.cursor, "poll cursor");
                // get_mut, not entry().or_insert_with: a last unsubscribe landing
                // in the fetch window removes this cursor (and the subs entry). A
                // resurrecting or_insert_with here would leak the entry
                // (poll_symbols no longer lists the symbol, so it is never polled
                // or removed again) and let a later fresh subscribe silently
                // resume from this stale position instead of its requested
                // start/sim-now anchor. Drop the batch when the entry is gone.
                let Some(entry) = cursors.get_mut(&symbol) else {
                    continue;
                };
                entry.unseen_from_batch(batch)
            };
            // Anchor every trade in this page at one arrival instant so the
            // per-message havoc latency does not compound across the page: a
            // 1000-trade page drains in a single delay window rather than
            // page_len * delay (AD4). The anchor reads ctx.sim live, which can
            // change under the AD6 self-heal, so the poll path keeps this inline
            // shape instead of a separate pump.
            let page_arrival = Instant::now();
            for trade in trades {
                let (sink, instruments, subs, bars) =
                    (&ctx.sink, &ctx.instruments, &ctx.subs, &ctx.bars);
                drain_havoc_anchored(
                    &mut ctx.havoc_filter,
                    ServerMessage::Trade(trade),
                    ctx.sim,
                    page_arrival,
                    |msg| handle_market_message(msg, sink, instruments, subs, bars, ctx.sim),
                )
                .await;
            }
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (sink, instruments, subs, bars) = (&ctx.sink, &ctx.instruments, &ctx.subs, &ctx.bars);
    flush_havoc(&mut ctx.havoc_filter, ctx.sim, |msg| {
        handle_market_message(msg, sink, instruments, subs, bars, ctx.sim)
    })
    .await;
}
fn poll_symbols(subs: &Arc<Mutex<HashMap<Symbol, SubState>>>) -> Vec<(Symbol, Option<u64>)> {
    lock_recover(subs, "subscription")
        .iter()
        .filter(|(_, state)| state.trades > 0 || state.bars > 0)
        .map(|(symbol, state)| (symbol.clone(), state.start_ts))
        .collect()
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

fn update_bar_state(
    bar_type: BarType,
    state: &mut BarSubState,
    trade: &mogwai_protocol::TradeTick,
    def: &InstrumentDef,
    sim: SimClock,
) -> Option<Bar> {
    let interval = get_bar_interval_ns(&bar_type).as_u64();
    let close_ts = ((trade.ts_event / interval) + 1) * interval;
    if let Some(active) = &mut state.active {
        if trade.ts_event >= active.close_ts {
            // Build the closed window's bar before rotating to the new one. A
            // hostile open/high/low/close/volume that overflows nautilus
            // Price/Quantity drops just this bar with a warning rather than
            // panicking the reader/poll task; the window still rotates so a
            // single bad bar does not wedge aggregation.
            let bar = match active_to_bar(bar_type, active, def, sim) {
                Ok(bar) => Some(bar),
                Err(err) => {
                    tracing::warn!(%bar_type, error = %err, "dropping unrepresentable bar");
                    None
                }
            };
            state.active = Some(new_active_bar(trade, close_ts));
            bar
        } else {
            active.high = active.high.max(trade.price);
            active.low = active.low.min(trade.price);
            active.close = trade.price;
            active.volume += trade.size;
            None
        }
    } else {
        state.active = Some(new_active_bar(trade, close_ts));
        None
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
    // `end >= active.close_ts` the window closed within the requested range and
    // must be emitted; a genuinely-partial trailing window (`end` inside it, or
    // an unknown `end`) is still dropped, matching the live path.
    if let (Some(active), Some(end)) = (&state.active, end)
        && end.as_u64() >= active.close_ts
    {
        match active_to_bar(*bar_type, active, def, sim) {
            Ok(bar) => out.push(bar),
            Err(err) => {
                tracing::warn!(%bar_type, error = %err, "dropping unrepresentable trailing bar");
            }
        }
    }
    out
}

fn new_active_bar(trade: &mogwai_protocol::TradeTick, close_ts: u64) -> ActiveBar {
    ActiveBar {
        open: trade.price,
        high: trade.price,
        low: trade.price,
        close: trade.price,
        volume: trade.size,
        close_ts,
    }
}

fn active_to_bar(
    bar_type: BarType,
    active: &ActiveBar,
    def: &InstrumentDef,
    sim: SimClock,
) -> anyhow::Result<Bar> {
    Ok(Bar::new(
        bar_type,
        convert::price(active.open, def.price_precision)?,
        convert::price(active.high, def.price_precision)?,
        convert::price(active.low, def.price_precision)?,
        convert::price(active.close, def.price_precision)?,
        convert::quantity(active.volume, def.size_precision)?,
        UnixNanos::from(active.close_ts),
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

/// Locks a `Mutex` from a spawned reader/poll/dispatch task, recovering from a
/// poisoned lock instead of panicking. The `&mut self` subscription methods
/// propagate poison as `anyhow::Err`, but these free functions run in
/// `tokio::spawn`ed tasks with no supervisor, so a bare `.expect("... poisoned")`
/// here would cascade one upstream panic into killing the whole reader/poll
/// task (and with it the data/exec stream). Poison only signals that some other
/// holder panicked mid-mutation; the guarded map is still structurally sound, so
/// we log once at `warn` and proceed with the recovered guard, matching the
/// recoverable-error style the instance methods already use.
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
    regime: Option<MarketRegime>,
}

async fn fetch_trades(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    fetch: TradeFetch<'_>,
) -> anyhow::Result<Vec<mogwai_protocol::TradeTick>> {
    let params = trade_query_params(
        fetch.symbol,
        fetch.start,
        fetch.end,
        fetch.limit,
        fetch.regime,
    )?;
    quota.wait().await;
    let response = http
        .get(
            join_url(base, "trades"),
            Some(&params),
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await
        .context("fetch trades")?;
    ensure!(
        response.status.is_success(),
        "fetch trades returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode trades")
}

/// Pages the server's bounded `/trades` scan across `[start, end]`, following
/// the same overlap-and-skip cursor discipline the live poll path uses
/// (`PollCursor`): each page re-fetches from the last seen `ts_event` and skips
/// the already-emitted prefix at that timestamp, so trades sharing a `ts_event`
/// across a page boundary are neither dropped nor duplicated. `MAX_HISTORY_LIMIT`
/// is the per-page size - a single unpaged request silently truncated any
/// window of more than 1000 trades to its oldest 1000, under-delivering every
/// warmup (and, for `request_bars`, applying a BAR-count limit as a TRADE-page
/// limit,
/// so a warmup for N bars fetched at most N trades covering only the oldest
/// edge). `stop` decides after each page whether enough has been collected: a
/// trade ceiling for `request_trades`, a bar-span target for `request_bars`. The
/// loop also stops when a short page proves the window is exhausted at the
/// server frontier - the server clamps `end` at sim-now and refuses a start
/// beyond it, so a forward-walking cursor always reaches a short page and
/// terminates cleanly. Returns the accumulated trades and whether collection was
/// cut short before the window end (`stop` fired, or the same-ts wedge), so the
/// caller can warn that the delivered range is a truncated prefix.
async fn fetch_trades_windowed(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    fetch: TradeFetch<'_>,
    mut stop: impl FnMut(&[mogwai_protocol::TradeTick]) -> bool,
) -> anyhow::Result<(Vec<mogwai_protocol::TradeTick>, bool)> {
    let TradeFetch {
        symbol,
        start,
        end,
        regime,
        // The per-page size is always MAX_HISTORY_LIMIT (fetch_trades defaults a
        // `None` limit to the ceiling), so the caller's `limit` is ignored here -
        // pagination, not a single-request cap, bounds the result.
        limit: _,
    } = fetch;
    let mut cursor = PollCursor::new(start.map(|s| s.as_u64()));
    // The first page honors the caller's start verbatim: a `None` means "from
    // origin" and must NOT be sent as start=0 (the boot-origin server 422s it).
    // Subsequent pages re-fetch from the advancing cursor boundary.
    let mut next_start = start;
    let mut out = Vec::new();
    loop {
        let page = fetch_trades(
            http,
            quota,
            base,
            TradeFetch {
                symbol,
                start: next_start,
                end,
                limit: None,
                regime,
            },
        )
        .await?;
        let raw_len = page.len();
        let prev_last_ts = cursor.last_ts;
        out.extend(cursor.unseen_from_batch(page));
        if raw_len < mogwai_protocol::MAX_HISTORY_LIMIT {
            // Short page: the server returned everything left in the window.
            return Ok((out, false));
        }
        if cursor.last_ts == prev_last_ts {
            // A full page that did not advance the cursor: every trade in it
            // shares one `ts_event` and the ts-cursor cannot page past it (the
            // inherent >1000-trades-at-one-ns wedge). Stop rather than spin
            // forever, and report the window as truncated.
            return Ok((out, true));
        }
        if stop(&out) {
            return Ok((out, true));
        }
        next_start = Some(UnixNanos::from(cursor.last_ts));
    }
}

/// Stop predicate for `request_bars`' pagination: true once the accumulated
/// trades span at least `bar_limit` bar intervals. Translating nautilus's
/// BAR-count limit into a window span (rather than an exact bar count) bounds
/// the loop without aggregating incrementally; a sparse tape with empty windows
/// may yield slightly fewer than `bar_limit` actual bars (the tape simply did
/// not trade in those windows), which is correct - the warmup gets the bars that
/// exist. `None` bar_limit never stops early (page to the frontier).
fn bar_span_reached(
    trades: &[mogwai_protocol::TradeTick],
    interval: u64,
    bar_limit: Option<usize>,
) -> bool {
    let Some(limit) = bar_limit else {
        return false;
    };
    match (trades.first(), trades.last()) {
        (Some(first), Some(last)) if interval > 0 => {
            let spanned = (last.ts_event / interval).saturating_sub(first.ts_event / interval);
            usize::try_from(spanned).unwrap_or(usize::MAX) >= limit
        }
        _ => false,
    }
}

fn trade_query_params(
    symbol: &str,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
    limit: Option<std::num::NonZeroUsize>,
    regime: Option<MarketRegime>,
) -> anyhow::Result<HashMap<String, Vec<String>>> {
    let mut params = HashMap::new();
    params.insert("symbol".to_string(), vec![symbol.to_string()]);
    if let Some(start) = start {
        params.insert("start".to_string(), vec![start.as_u64().to_string()]);
    }
    if let Some(end) = end {
        params.insert("end".to_string(), vec![end.as_u64().to_string()]);
    }
    params.insert("limit".to_string(), vec![capped_limit(limit).to_string()]);
    if let Some(regime) = regime {
        params.insert(
            "regime".to_string(),
            vec![serde_json::to_string(&regime).context("encode market regime")?],
        );
    }
    Ok(params)
}
fn start_ts_param(params: &Option<Params>) -> Option<u64> {
    params.as_ref().and_then(|p| p.get_u64("start_ts"))
}

#[cfg(test)]
mod data_client_tests {
    use std::num::NonZeroUsize;

    use mogwai_protocol::{AggressorSide, HavocSpec, MarketRegime, TransportProfile};
    use nautilus_core::{Params, UUID4};
    use nautilus_model::{
        data::BarSpecification,
        enums::{AggregationSource, BarAggregation, PriceType},
        identifiers::InstrumentId,
    };

    use super::*;

    fn def() -> InstrumentDef {
        InstrumentDef {
            symbol: "BTCUSDT".into(),
            base: "BTC".into(),
            quote: "USDT".into(),
            price_precision: 2,
            size_precision: 8,
            price_increment: Decimal::new(1, 2),
            size_increment: Decimal::new(1, 8),
        }
    }

    fn instrument_id() -> InstrumentId {
        InstrumentId::new(
            nautilus_model::identifiers::Symbol::from("BTCUSDT"),
            *MOGWAI_VENUE,
        )
    }

    fn trade(ts_event: u64, price: i64, size: i64) -> mogwai_protocol::TradeTick {
        mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(price, 2),
            size: Decimal::new(size, 0),
            aggressor: AggressorSide::Buyer,
            ts_event,
        }
    }

    fn instruments_map() -> Arc<Mutex<HashMap<Symbol, InstrumentDef>>> {
        let map = HashMap::from([(def().symbol.clone(), def())]);
        Arc::new(Mutex::new(map))
    }

    fn subs_with(state: SubState) -> Arc<Mutex<HashMap<Symbol, SubState>>> {
        Arc::new(Mutex::new(HashMap::from([("BTCUSDT".to_string(), state)])))
    }

    fn time_bar_type(step: usize, aggregation: BarAggregation) -> BarType {
        BarType::new(
            instrument_id(),
            BarSpecification::new(step, aggregation, PriceType::Last),
            AggregationSource::External,
        )
    }

    fn start_ts_params(start_ts: u64) -> Option<Params> {
        Some(
            serde_json::from_value(serde_json::json!({ "start_ts": start_ts }))
                .expect("params decode"),
        )
    }

    fn data_client() -> MogwaiDataClient {
        MogwaiDataClient::new(
            ClientId::from("MOGWAI-DATA"),
            MogwaiDataClientConfig::default(),
        )
        .expect("valid data client")
    }

    fn polling_data_client() -> MogwaiDataClient {
        MogwaiDataClient::new(
            ClientId::from("MOGWAI-DATA"),
            MogwaiDataClientConfig {
                transport_profile: TransportProfile::HttpPolling,
                ..MogwaiDataClientConfig::default()
            },
        )
        .expect("valid polling data client")
    }

    // Drain-side seam: a parsed `ServerMessage` with a live subscription must
    // drive the matching `DataEvent::Data` into the egress sink. This is the
    // brick-2 gate's intent exercised in-process (no socket) by calling the
    // drain function the WS reader task feeds.
    #[tokio::test]
    async fn trade_frame_drives_data_event_into_sink() {
        let (tx, mut rx) = unbounded_channel();
        let instruments = instruments_map();
        let subs = subs_with(SubState {
            trades: 1,
            ..SubState::default()
        });
        let bars = Arc::new(Mutex::new(HashMap::new()));

        handle_market_message(
            ServerMessage::Trade(trade(42, 12_345, 1)),
            &tx,
            &instruments,
            &subs,
            &bars,
            SimClock::identity(),
        )
        .await;

        match rx.try_recv().expect("a data event was emitted") {
            DataEvent::Data(Data::Trade(t)) => {
                assert_eq!(t.instrument_id, instrument_id());
                assert_eq!(t.ts_event, UnixNanos::from(42));
                assert_eq!(t.price.precision, 2);
            }
            other => panic!("expected trade data event, got {other:?}"),
        }
    }

    #[test]
    fn emit_trade_shared_body_drives_data_event_into_sink() {
        let (tx, mut rx) = unbounded_channel();
        let instruments = instruments_map();
        let subs = subs_with(SubState {
            trades: 1,
            ..SubState::default()
        });
        let bars = Arc::new(Mutex::new(HashMap::new()));

        emit_trade(
            &trade(42, 12_345, 1),
            &tx,
            &instruments,
            &subs,
            &bars,
            SimClock::identity(),
        );

        match rx.try_recv().expect("a data event was emitted") {
            DataEvent::Data(Data::Trade(t)) => {
                assert_eq!(t.instrument_id, instrument_id());
                assert_eq!(t.ts_event, UnixNanos::from(42));
            }
            other => panic!("expected trade data event, got {other:?}"),
        }
    }

    // Connect-side seam: seeding fills only the adapter's local def map; the
    // worker's Nautilus cache is fed solely by `DataEvent::Instrument`. Without
    // a connect-time emit, a forward run that subscribes to bars but never to
    // the instrument leaves the cache empty and the executor refuses every bar.
    // `emit_seeded_instruments` must push each seeded def into the sink so the
    // cache is populated the instant the data client connects.
    #[test]
    fn connect_emits_seeded_instruments_into_sink() {
        let (tx, mut rx) = unbounded_channel();
        let instruments = instruments_map();

        emit_seeded_instruments(&tx, &instruments, SimClock::identity());

        match rx.try_recv().expect("an instrument event was emitted") {
            DataEvent::Instrument(nautilus_model::instruments::InstrumentAny::CurrencyPair(
                pair,
            )) => {
                assert_eq!(pair.id, instrument_id());
            }
            other => panic!("expected currency-pair instrument event, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "exactly one instrument should be emitted for a single seeded def"
        );
    }

    // Drain-side type filter: a symbol subscribed for trades only must not
    // forward quote frames it never asked for (obstacle 4 / brick 3).
    #[tokio::test]
    async fn quote_frame_is_filtered_for_trades_only_subscription() {
        let (tx, mut rx) = unbounded_channel();
        let instruments = instruments_map();
        let subs = subs_with(SubState {
            trades: 1,
            ..SubState::default()
        });
        let bars = Arc::new(Mutex::new(HashMap::new()));

        handle_market_message(
            ServerMessage::Quote(mogwai_protocol::QuoteTick {
                symbol: "BTCUSDT".into(),
                bid_px: Decimal::new(100, 0),
                ask_px: Decimal::new(101, 0),
                bid_sz: Decimal::new(1, 0),
                ask_sz: Decimal::new(1, 0),
                ts_event: 7,
            }),
            &tx,
            &instruments,
            &subs,
            &bars,
            SimClock::identity(),
        )
        .await;

        assert!(
            rx.try_recv().is_err(),
            "quote must not reach a trades-only sink"
        );
    }

    // Brick-3 wiring: each subscribe variant must emit the correct mogwai
    // `ClientMessage` on the 0->1 transition, and the refcount machinery must
    // suppress redundant sends. We drive the real handlers against a client
    // whose ws command channel we own, then map the queued `WsCommand` exactly
    // as the writer task does.
    #[test]
    fn subscribe_variants_emit_subscribe_then_refcount_suppresses() {
        let mut client = data_client();
        let (tx, mut rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        // A live subscribe emits its Subscribe only on a live connection (AD5):
        // mark the client connected so these refcount-wiring assertions exercise
        // the direct-send path rather than the disconnected defer-to-on_connect path.
        client.connected.store(true, Ordering::Relaxed);

        client
            .subscribe_trades(SubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                start_ts_params(100),
            ))
            .expect("subscribe trades");

        // 0->1 transition emits a Subscribe carrying the start_ts.
        let cmd = rx.try_recv().expect("first subscribe emits a command");
        assert!(matches!(
            ws_command_to_client_message(cmd),
            ClientMessage::Subscribe {
                symbols,
                start_ts: Some(100),
                regime: None,
            } if symbols == vec!["BTCUSDT".to_string()]
        ));

        // A second subscribe for the same symbol (quotes) only bumps the
        // refcount; no new Subscribe is sent to the server.
        client
            .subscribe_quotes(SubscribeQuotes::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("subscribe quotes");
        assert!(
            rx.try_recv().is_err(),
            "second subscribe must not re-issue a Subscribe"
        );
    }

    #[test]
    fn subscribe_command_carries_data_regime() {
        let mut client = MogwaiDataClient::new(
            ClientId::from("MOGWAI-DATA"),
            MogwaiDataClientConfig {
                havoc: Some(HavocSpec {
                    data: Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
                    ..HavocSpec::default()
                }),
                ..MogwaiDataClientConfig::default()
            },
        )
        .expect("valid data client");
        let (tx, mut rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        // A live subscribe emits its Subscribe only on a live connection (AD5):
        // mark the client connected so these refcount-wiring assertions exercise
        // the direct-send path rather than the disconnected defer-to-on_connect path.
        client.connected.store(true, Ordering::Relaxed);

        client
            .subscribe_trades(SubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("subscribe trades");

        let cmd = rx.try_recv().expect("subscribe emits a command");
        assert!(matches!(
            ws_command_to_client_message(cmd),
            ClientMessage::Subscribe {
                regime: Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
                ..
            }
        ));
    }

    #[test]
    fn polling_subscribe_updates_refs_without_ws_channel() {
        let mut client = polling_data_client();

        client
            .subscribe_trades(SubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                start_ts_params(100),
            ))
            .expect("subscribe trades without websocket");

        let subs = client.subs.lock().expect("subscription mutex");
        let state = subs.get("BTCUSDT").expect("symbol subscribed");
        assert_eq!(state.trades, 1);
        assert_eq!(state.start_ts, Some(100));
        assert!(client.ws_cmd.is_none());
    }

    // Brick-3 unsubscribe: only the 1->0 transition emits an Unsubscribe.
    #[test]
    fn unsubscribe_emits_only_on_last_release() {
        let mut client = data_client();
        let (tx, mut rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        // A live subscribe emits its Subscribe only on a live connection (AD5):
        // mark the client connected so these refcount-wiring assertions exercise
        // the direct-send path rather than the disconnected defer-to-on_connect path.
        client.connected.store(true, Ordering::Relaxed);

        client
            .subscribe_trades(SubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("subscribe trades");
        client
            .subscribe_quotes(SubscribeQuotes::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("subscribe quotes");
        let _ = rx.try_recv().expect("initial subscribe");

        client
            .unsubscribe_trades(&UnsubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("unsubscribe trades");
        assert!(
            rx.try_recv().is_err(),
            "quotes still subscribed: no Unsubscribe yet"
        );

        client
            .unsubscribe_quotes(&UnsubscribeQuotes::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("unsubscribe quotes");
        assert!(matches!(
            ws_command_to_client_message(rx.try_recv().expect("final release emits Unsubscribe")),
            ClientMessage::Unsubscribe { symbols }
                if symbols == vec!["BTCUSDT".to_string()]
        ));
    }

    // Landing 3 warmup guard: a window whose start precedes the published
    // data_origin is refused at the request boundary (a loud, surfaced error
    // naming both the start and the floor), while an on-tape window - or one with
    // an unknown floor - passes through to the fetch. This is the adapter half of
    // the #13 close: broadarrow learns "off-tape" as an error, not an empty page.
    #[test]
    fn request_bars_off_tape_window_errors_loudly() {
        const ORIGIN: u64 = 1_900_000_000_000_000_000;

        // Below the floor: refused, and the message names both numbers so the
        // operator can see exactly how far off-tape the request was.
        let err = ensure_on_tape(Some(UnixNanos::from(ORIGIN - 1)), ORIGIN)
            .expect_err("a start below data_origin is refused");
        let msg = err.to_string();
        assert!(
            msg.contains(&(ORIGIN - 1).to_string()),
            "names the start: {msg}"
        );
        assert!(msg.contains(&ORIGIN.to_string()), "names the floor: {msg}");

        // On the floor and above it: served.
        assert!(ensure_on_tape(Some(UnixNanos::from(ORIGIN)), ORIGIN).is_ok());
        assert!(ensure_on_tape(Some(UnixNanos::from(ORIGIN + 1)), ORIGIN).is_ok());
        // "From origin" (no start) is always on-tape.
        assert!(ensure_on_tape(None, ORIGIN).is_ok());
        // Unknown floor (clock fetch failed -> identity fallback): defer to the
        // server's own refusal rather than guessing.
        assert!(ensure_on_tape(Some(UnixNanos::from(1)), 0).is_ok());
    }

    // Brick-2/3 bars (request path): aggregation pins ts_event = bar close,
    // and the trailing partial window is dropped (it never reaches its close).
    #[test]
    fn request_bar_aggregation_closes_on_window_and_drops_partial() {
        let bar_type = time_bar_type(1, BarAggregation::Second);
        let interval = get_bar_interval_ns(&bar_type).as_u64();
        let def = def();

        // Two trades in the first second window, one in the second window.
        // The first window closes (emits one bar); the second is partial and
        // must be dropped.
        let trades = vec![
            trade(10, 10_000, 1), // window [0, interval)
            trade(interval - 5, 20_000, 2),
            trade(interval + 5, 30_000, 1), // window [interval, 2*interval)
        ];

        // `end` is None (unknown), so the trailing partial window is dropped.
        let bars = aggregate_bars(&bar_type, &trades, &def, SimClock::identity(), None);

        assert_eq!(bars.len(), 1, "only the closed window emits a bar");
        let bar = bars[0];
        assert_eq!(
            bar.ts_event,
            UnixNanos::from(interval),
            "bar ts_event is the window close"
        );
        assert_eq!(bar.open.as_f64(), 100.0);
        assert_eq!(bar.high.as_f64(), 200.0);
        assert_eq!(bar.low.as_f64(), 100.0);
        assert_eq!(bar.close.as_f64(), 200.0);
        assert_eq!(bar.volume.as_f64(), 3.0);
    }

    // AD2: a historical window whose `end` proves the trailing window fully
    // elapsed must flush that COMPLETE window's bar - it is the newest bar of a
    // warmup, and the lazy "emit when a later trade crosses close_ts" rule would
    // otherwise drop it (no later trade exists in an already-passed window). The
    // genuinely-partial case (end inside the window) still drops.
    #[test]
    fn request_bar_aggregation_flushes_trailing_completed_window() {
        let bar_type = time_bar_type(1, BarAggregation::Second);
        let interval = get_bar_interval_ns(&bar_type).as_u64();
        let def = def();

        // Two trades in window [0, interval), one in window [interval,
        // 2*interval). No trade crosses the second window's close, so only `end`
        // can prove it closed.
        let trades = vec![
            trade(10, 10_000, 1),
            trade(interval - 5, 20_000, 2),
            trade(interval + 5, 30_000, 3),
        ];

        // end at exactly the second window's close (2*interval): both windows
        // are complete, so both flush.
        let closed = aggregate_bars(
            &bar_type,
            &trades,
            &def,
            SimClock::identity(),
            Some(UnixNanos::from(2 * interval)),
        );
        assert_eq!(closed.len(), 2, "end proves the trailing window closed");
        assert_eq!(closed[1].ts_event, UnixNanos::from(2 * interval));
        assert_eq!(closed[1].open.as_f64(), 300.0);
        assert_eq!(closed[1].close.as_f64(), 300.0);
        assert_eq!(closed[1].volume.as_f64(), 3.0);

        // end INSIDE the trailing window: it is genuinely partial and dropped.
        let partial = aggregate_bars(
            &bar_type,
            &trades,
            &def,
            SimClock::identity(),
            Some(UnixNanos::from(interval + 6)),
        );
        assert_eq!(
            partial.len(),
            1,
            "an end inside the trailing window drops the partial"
        );
    }

    // Brick-3 bars (live path): a bar only reaches the sink when its window
    // closes; an in-progress window emits nothing.
    #[test]
    fn live_bar_state_emits_only_on_window_close() {
        let bar_type = time_bar_type(1, BarAggregation::Second);
        let interval = get_bar_interval_ns(&bar_type).as_u64();
        let def = def();
        let mut state = BarSubState {
            refs: 1,
            active: None,
        };

        // Two trades inside the first window: no bar yet.
        assert!(
            update_bar_state(
                bar_type,
                &mut state,
                &trade(0, 10_000, 1),
                &def,
                SimClock::identity(),
            )
            .is_none()
        );
        assert!(
            update_bar_state(
                bar_type,
                &mut state,
                &trade(interval - 1, 11_000, 1),
                &def,
                SimClock::identity(),
            )
            .is_none()
        );

        // A trade past the close boundary flushes the completed window.
        let bar = update_bar_state(
            bar_type,
            &mut state,
            &trade(interval, 12_000, 1),
            &def,
            SimClock::identity(),
        )
        .expect("window close flushes a bar");
        assert_eq!(bar.ts_event, UnixNanos::from(interval));
        assert_eq!(bar.open.as_f64(), 100.0);
        assert_eq!(bar.close.as_f64(), 110.0);
    }

    // Brick-4 response bound: a missing limit defaults to the ceiling and any
    // over-ceiling limit is clamped, so the materialized response Vec stays
    // bounded over a multi-GB dump.
    #[test]
    fn history_limit_is_capped_at_the_ceiling() {
        assert_eq!(capped_limit(None), mogwai_protocol::MAX_HISTORY_LIMIT);
        assert_eq!(
            capped_limit(NonZeroUsize::new(mogwai_protocol::MAX_HISTORY_LIMIT * 100)),
            mogwai_protocol::MAX_HISTORY_LIMIT
        );
        assert_eq!(capped_limit(NonZeroUsize::new(5)), 5);
    }

    #[test]
    fn trade_query_regime_round_trips_through_url_encoding() {
        let regime = MarketRegime::VolStorm { vol_mult: 10.0 };
        let params = trade_query_params("BTCUSDT", None, None, NonZeroUsize::new(5), Some(regime))
            .expect("params build");
        let pairs: Vec<(&str, &str)> = params
            .iter()
            .flat_map(|(key, values)| {
                values
                    .iter()
                    .map(move |value| (key.as_str(), value.as_str()))
            })
            .collect();
        let encoded = serde_urlencoded::to_string(pairs).expect("query encodes");
        let decoded: HashMap<String, String> =
            serde_urlencoded::from_str(&encoded).expect("query decodes");
        let decoded_regime: MarketRegime =
            serde_json::from_str(decoded.get("regime").expect("regime param"))
                .expect("regime decodes");

        assert_eq!(decoded_regime, regime);
    }

    #[test]
    fn poll_cursor_overlaps_unique_boundary_without_duplicates() {
        let mut cursor = PollCursor::new(Some(10));
        let first = cursor.unseen_from_batch(vec![trade(10, 10_000, 1), trade(20, 20_000, 1)]);
        assert_eq!(
            first.iter().map(|trade| trade.ts_event).collect::<Vec<_>>(),
            vec![10, 20]
        );
        assert_eq!(
            cursor,
            PollCursor {
                last_ts: 20,
                emitted_at_last_ts: 1,
            }
        );

        let second = cursor.unseen_from_batch(vec![trade(20, 20_000, 1), trade(30, 30_000, 1)]);
        assert_eq!(
            second
                .iter()
                .map(|trade| trade.ts_event)
                .collect::<Vec<_>>(),
            vec![30]
        );
        assert_eq!(
            cursor,
            PollCursor {
                last_ts: 30,
                emitted_at_last_ts: 1,
            }
        );
    }

    #[test]
    fn poll_cursor_handles_cap_boundary_inside_same_timestamp() {
        let mut cursor = PollCursor::new(None);
        let first = cursor.unseen_from_batch(vec![trade(10, 10_000, 1), trade(20, 20_000, 1)]);
        assert_eq!(first.len(), 2);
        assert_eq!(
            cursor,
            PollCursor {
                last_ts: 20,
                emitted_at_last_ts: 1,
            }
        );

        let second = cursor.unseen_from_batch(vec![
            trade(20, 20_000, 1),
            trade(20, 21_000, 1),
            trade(20, 22_000, 1),
            trade(30, 30_000, 1),
        ]);
        assert_eq!(
            second.iter().map(|trade| trade.price).collect::<Vec<_>>(),
            vec![
                Decimal::new(21_000, 2),
                Decimal::new(22_000, 2),
                Decimal::new(30_000, 2)
            ]
        );
        assert_eq!(
            cursor,
            PollCursor {
                last_ts: 30,
                emitted_at_last_ts: 1,
            }
        );
    }

    // AD8: defense in depth against a server that rewinds. A trade stamped below
    // the cursor's high-water mark was already delivered (or belongs to a window
    // the cursor has moved past), so it must be skipped rather than re-emitted as
    // a duplicate - the guard makes the cursor robust against a descending-ts
    // server bug by construction.
    #[test]
    fn poll_cursor_skips_rewound_trades_below_last_ts() {
        let mut cursor = PollCursor::new(Some(20));
        let first = cursor.unseen_from_batch(vec![trade(20, 20_000, 1)]);
        assert_eq!(first.len(), 1);
        assert_eq!(cursor.last_ts, 20);

        let rewound = cursor.unseen_from_batch(vec![trade(15, 15_000, 1), trade(25, 25_000, 1)]);
        assert_eq!(
            rewound.iter().map(|t| t.ts_event).collect::<Vec<_>>(),
            vec![25],
            "a trade below the cursor high-water mark must not be re-emitted"
        );
        assert_eq!(cursor.last_ts, 25);
    }

    // AD3: `request_bars`' pagination stops once the accumulated trades span at
    // least the requested number of bar intervals, translating nautilus's
    // BAR-count limit into a window target rather than the old TRADE-page cap.
    #[test]
    fn bar_span_stops_once_enough_intervals_are_covered() {
        let interval = 1_000;
        // Three bars requested; trades spanning three intervals satisfy it.
        let spanning = vec![trade(0, 1, 1), trade(3_500, 1, 1)];
        assert!(bar_span_reached(&spanning, interval, Some(3)));
        // Only two intervals spanned: keep paging.
        let short = vec![trade(0, 1, 1), trade(2_500, 1, 1)];
        assert!(!bar_span_reached(&short, interval, Some(3)));
        // No bar limit never stops early (page to the frontier).
        assert!(!bar_span_reached(&spanning, interval, None));
        // Empty accumulation never stops.
        assert!(!bar_span_reached(&[], interval, Some(1)));
    }

    // AD5: a subscribe that lands during a reconnect backoff (connected == false)
    // must NOT queue a Subscribe. The queued command would survive the reconnect
    // in the command channel and be sent AGAIN after on_connect
    // (subscribe_commands) already rebuilt the subscription from the table -
    // double-subscribing and restarting the server-side replay. While
    // disconnected, on_connect is the sole subscribe source and reconstructs the
    // subscription from the table, so nothing is lost.
    #[test]
    fn subscribe_while_disconnected_defers_to_on_connect() {
        let mut client = data_client();
        let (tx, mut rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        // connected stays false: this is a reconnect backoff window.

        client
            .subscribe_trades(SubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                start_ts_params(100),
            ))
            .expect("subscribe trades while disconnected");

        assert!(
            rx.try_recv().is_err(),
            "a subscribe while disconnected must not queue a Subscribe (AD5)"
        );
        // The subscription is recorded, so on_connect reconstructs it exactly once.
        let reconstructed = subscribe_commands(&client.subs, None);
        assert!(matches!(
            reconstructed.as_slice(),
            [
                WsCommand::Subscribe {
                    symbols,
                    start_ts: Some(100),
                    ..
                }
            ] if symbols == &vec!["BTCUSDT".to_string()]
        ));
    }

    // AD7: a later subscriber must not pull the symbol's shared resume cursor
    // (start_ts) backward. The old min(existing, new) rewound it to an earlier
    // requested start, which the next reconnect's on_connect then replayed as an
    // already-delivered window. Only the first subscriber seeds the cursor.
    #[test]
    fn later_subscriber_does_not_pull_start_ts_backward() {
        let mut client = data_client();
        let (tx, _rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        client.connected.store(true, Ordering::Relaxed);

        client
            .subscribe_trades(SubscribeTrades::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                start_ts_params(100),
            ))
            .expect("first subscribe seeds the cursor at 100");

        // A second subscriber asking for an EARLIER start (50) must not rewind it.
        client
            .subscribe_quotes(SubscribeQuotes::new(
                instrument_id(),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                start_ts_params(50),
            ))
            .expect("second subscribe with an earlier start");

        let subs = client.subs.lock().expect("subscription mutex");
        assert_eq!(
            subs.get("BTCUSDT").expect("symbol subscribed").start_ts,
            Some(100),
            "a later earlier-start subscriber must not rewind the shared cursor (AD7)"
        );
    }

    // AD10: an unsubscribe_bars for a bar type that was never subscribed must be a
    // no-op on the symbol's shared bars count, so it cannot darken a surviving
    // subscription for a DIFFERENT bar type on the same symbol.
    #[test]
    fn unmatched_unsubscribe_bars_does_not_darken_surviving_feed() {
        let mut client = data_client();
        let (tx, mut rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        client.connected.store(true, Ordering::Relaxed);

        let subscribed = time_bar_type(1, BarAggregation::Second);
        client
            .subscribe_bars(SubscribeBars::new(
                subscribed,
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("subscribe bars");
        assert!(
            matches!(
                ws_command_to_client_message(rx.try_recv().expect("0->1 subscribe")),
                ClientMessage::Subscribe { .. }
            ),
            "the live bar subscription emits its Subscribe"
        );

        // Unsubscribe a DIFFERENT bar type that was never subscribed.
        let never = time_bar_type(5, BarAggregation::Minute);
        client
            .unsubscribe_bars(&UnsubscribeBars::new(
                never,
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("unmatched unsubscribe is a no-op");

        assert!(
            rx.try_recv().is_err(),
            "an unmatched unsubscribe_bars must not fire a wire Unsubscribe (AD10)"
        );
        let subs = client.subs.lock().expect("subscription mutex");
        assert_eq!(
            subs.get("BTCUSDT").expect("symbol still subscribed").bars,
            1,
            "the surviving bar subscription's shared count must be untouched"
        );
    }

    // AD11: Week/Month/Year bars are refused (their calendar anchoring cannot be
    // produced by the adapter's epoch-anchored aggregation); Day and finer pass.
    #[test]
    fn calendar_anchored_bar_aggregations_are_refused() {
        assert!(is_calendar_anchored(BarAggregation::Week));
        assert!(is_calendar_anchored(BarAggregation::Month));
        assert!(is_calendar_anchored(BarAggregation::Year));
        assert!(!is_calendar_anchored(BarAggregation::Day));
        assert!(!is_calendar_anchored(BarAggregation::Hour));
        assert!(!is_calendar_anchored(BarAggregation::Minute));
        assert!(!is_calendar_anchored(BarAggregation::Second));

        // Wired into subscribe_bars: a Week bar is refused at the boundary.
        let mut client = data_client();
        client.connected.store(true, Ordering::Relaxed);
        let err = client
            .subscribe_bars(SubscribeBars::new(
                time_bar_type(1, BarAggregation::Week),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect_err("Week bars are refused");
        assert!(err.to_string().contains("Week/Month/Year"), "{err}");

        // A Day bar passes the gate.
        let (tx, _rx) = unbounded_channel::<WsCommand>();
        client.ws_cmd = Some(tx);
        client
            .subscribe_bars(SubscribeBars::new(
                time_bar_type(1, BarAggregation::Day),
                Some(client.client_id),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("Day bars are accepted");
    }

    // AD19 (partial): on the last unsubscribe of a bar type, a window that has
    // already closed (close_ts <= sim-now) but was withheld for lack of a later
    // trade to cross its boundary is flushed rather than silently discarded; a
    // genuinely in-progress window (close_ts still in the future) is dropped, not
    // shipped as a future-stamped partial bar.
    #[test]
    fn unsubscribe_bars_flushes_completed_window_but_not_in_progress() {
        fn active_bar(close_ts: u64) -> ActiveBar {
            ActiveBar {
                open: Decimal::new(10_000, 2),
                high: Decimal::new(10_000, 2),
                low: Decimal::new(10_000, 2),
                close: Decimal::new(10_000, 2),
                volume: Decimal::new(1, 0),
                close_ts,
            }
        }

        let mut client = data_client();
        let cid = client.client_id;
        let (tx, mut rx) = unbounded_channel();
        client.sink = Some(tx);
        client.instruments = instruments_map();

        let completed = time_bar_type(1, BarAggregation::Second);
        let in_progress = time_bar_type(5, BarAggregation::Minute);
        {
            let mut bars = client.bars.lock().expect("bars");
            bars.insert(
                completed,
                BarSubState {
                    refs: 1,
                    // close_ts 1 is far in the past under the identity clock
                    // (sim-now reads wall-clock nanos).
                    active: Some(active_bar(1)),
                },
            );
            bars.insert(
                in_progress,
                BarSubState {
                    refs: 1,
                    active: Some(active_bar(u64::MAX)),
                },
            );
        }
        {
            let mut subs = client.subs.lock().expect("subs");
            subs.insert(
                "BTCUSDT".to_string(),
                SubState {
                    bars: 2,
                    ..SubState::default()
                },
            );
        }

        client
            .unsubscribe_bars(&UnsubscribeBars::new(
                completed,
                Some(cid),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("unsubscribe the completed bar type");
        match rx
            .try_recv()
            .expect("the completed withheld window is flushed")
        {
            DataEvent::Data(Data::Bar(bar)) => assert_eq!(bar.ts_event, UnixNanos::from(1)),
            other => panic!("expected a bar, got {other:?}"),
        }

        client
            .unsubscribe_bars(&UnsubscribeBars::new(
                in_progress,
                Some(cid),
                None,
                UUID4::new(),
                UnixNanos::default(),
                None,
                None,
            ))
            .expect("unsubscribe the in-progress bar type");
        assert!(
            rx.try_recv().is_err(),
            "an in-progress window must not ship a future-stamped partial bar"
        );
    }

    // AD19 (teardown): stop() flushes every completed-but-withheld window across
    // the whole bar table - not just a single unsubscribed type - so a shutdown
    // or a reconnect-driven reset does not silently drop the newest complete bar
    // of a live feed. In-progress windows are left unshipped, and a flushed
    // window is cleared so a second teardown cannot double-emit.
    #[test]
    fn stop_flushes_completed_bar_windows_but_not_in_progress() {
        fn active_bar(close_ts: u64) -> ActiveBar {
            ActiveBar {
                open: Decimal::new(10_000, 2),
                high: Decimal::new(10_000, 2),
                low: Decimal::new(10_000, 2),
                close: Decimal::new(10_000, 2),
                volume: Decimal::new(1, 0),
                close_ts,
            }
        }

        let mut client = data_client();
        let (tx, mut rx) = unbounded_channel();
        client.sink = Some(tx);
        client.instruments = instruments_map();

        let completed = time_bar_type(1, BarAggregation::Second);
        let in_progress = time_bar_type(5, BarAggregation::Minute);
        {
            let mut bars = client.bars.lock().expect("bars");
            bars.insert(
                completed,
                BarSubState {
                    refs: 1,
                    active: Some(active_bar(1)),
                },
            );
            bars.insert(
                in_progress,
                BarSubState {
                    refs: 1,
                    active: Some(active_bar(u64::MAX)),
                },
            );
        }

        client.stop().expect("stop");

        match rx
            .try_recv()
            .expect("the completed withheld window is flushed at stop")
        {
            DataEvent::Data(Data::Bar(bar)) => assert_eq!(bar.ts_event, UnixNanos::from(1)),
            other => panic!("expected a bar, got {other:?}"),
        }
        assert!(
            rx.try_recv().is_err(),
            "the in-progress window is not shipped and nothing is double-emitted"
        );

        client.stop().expect("stop again");
        assert!(
            rx.try_recv().is_err(),
            "a second teardown must not re-emit the already-flushed window"
        );
    }
}
