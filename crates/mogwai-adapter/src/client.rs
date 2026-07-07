use std::{
    collections::HashMap,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use mogwai_protocol::{
    ClientHavoc, ClientMessage, ConnHavoc, HavocLatency, HavocSpec, InstrumentDef, MarketRegime,
    ServerClock, ServerMessage, SimClock, Symbol, TradeTick,
};
use nautilus_common::{
    clients::{DataClient, ExecutionClient},
    live::{get_data_event_sender, get_runtime, try_get_exec_event_sender},
    messages::{
        DataEvent,
        data::{
            BarsResponse, DataResponse, InstrumentResponse, InstrumentsResponse, QuotesResponse,
            RequestBars, RequestInstrument, RequestInstruments, RequestQuotes, RequestTrades,
            SubscribeBars, SubscribeInstrument, SubscribeInstruments, SubscribeQuotes,
            SubscribeTrades, TradesResponse, UnsubscribeBars, UnsubscribeInstrument,
            UnsubscribeInstruments, UnsubscribeQuotes, UnsubscribeTrades,
        },
        execution::{
            CancelOrder, GenerateFillReports, GenerateOrderStatusReports,
            GeneratePositionStatusReports, ModifyOrder, SubmitOrder,
        },
    },
};
use nautilus_core::{Params, UUID4, UnixNanos, time::get_atomic_clock_realtime};
use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
use nautilus_model::{
    accounts::AccountAny,
    data::{Bar, BarType, Data, bar::get_bar_interval_ns},
    enums::{
        BarAggregation, LiquiditySide, OmsType, OrderSide, OrderStatus, OrderType,
        PositionSideSpecified,
    },
    events::{
        AccountState as NautilusAccountState, OrderAccepted, OrderCancelRejected, OrderCanceled,
        OrderEventAny, OrderFilled, OrderModifyRejected, OrderRejected, OrderSubmitted,
        OrderUpdated,
    },
    identifiers::{AccountId, ClientId, ClientOrderId, InstrumentId, TradeId, Venue, VenueOrderId},
    orders::Order,
    reports::{ExecutionMassStatus, FillReport, OrderStatusReport, PositionStatusReport},
    types::{AccountBalance, MarginBalance, currency::Currency},
};
use nautilus_network::http::HttpClient;
use rand::{RngExt, SeedableRng, rngs::StdRng};
use rust_decimal::Decimal;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::{
    MOGWAI_VENUE, MogwaiDataClientConfig, MogwaiExecClientConfig,
    clock::fetch_clock,
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
const ACCOUNT_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);
const ACCOUNT_REGISTRATION_POLL: Duration = Duration::from_millis(10);

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
        let handler_filter = Arc::clone(&havoc_filter);
        let disconnect_filter = Arc::clone(&havoc_filter);
        let handler_sink = sink.clone();
        let handler_instruments = Arc::clone(&instruments);
        let handler_subs = Arc::clone(&subs);
        let handler_bars = Arc::clone(&bars);
        let disconnect_sink = sink;
        let disconnect_instruments = Arc::clone(&instruments);
        let disconnect_subs = Arc::clone(&subs);
        let disconnect_bars = Arc::clone(&bars);
        let connect_subs = Arc::clone(&subs);
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
                    let sink = handler_sink.clone();
                    let instruments = Arc::clone(&handler_instruments);
                    let subs = Arc::clone(&handler_subs);
                    let bars = Arc::clone(&handler_bars);
                    async move {
                        let mut filter = handler_filter.lock().await;
                        dispatch_havoc(&mut filter, server_msg, sim, |msg| {
                            handle_market_message(msg, &sink, &instruments, &subs, &bars, sim)
                        })
                        .await;
                    }
                },
                move || {
                    let disconnect_filter = Arc::clone(&disconnect_filter);
                    let sink = disconnect_sink.clone();
                    let instruments = Arc::clone(&disconnect_instruments);
                    let subs = Arc::clone(&disconnect_subs);
                    let bars = Arc::clone(&disconnect_bars);
                    async move {
                        let mut filter = disconnect_filter.lock().await;
                        flush_havoc(&mut filter, sim, |msg| {
                            handle_market_message(msg, &sink, &instruments, &subs, &bars, sim)
                        })
                        .await;
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
        track_task(&self.task_handles, get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos(sim);
                for def in defs {
                    if let Some(instrument) = instrument_any_or_warn(&def, ts_init) {
                        drop(sink.send(DataEvent::Instrument(instrument)));
                    }
                }
            }
        }));
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
        track_task(&self.task_handles, get_runtime().spawn(async move {
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
        }));
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
                Some(0) => (true, bars.remove(&cmd.bar_type).and_then(|state| state.active)),
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
            // Closing live in-progress windows on a clock timer (the general
            // AD19 fix, and the stop()-teardown flush) is a larger feature,
            // flagged rather than built here.
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
        track_task(&self.task_handles, get_runtime().spawn(async move {
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
        track_task(&self.task_handles, get_runtime().spawn(async move {
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
        }));
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
        track_task(&self.task_handles, get_runtime().spawn(async move {
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
        }));
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

/// Drains the havoc-mangled expansion of one inbound `msg` and routes each
/// resulting wire message through `handle`, sleeping the per-message delay
/// first. Generic over the per-message sink so the market path (which forwards
/// to `handle_market_message`, async) and the exec path (which forwards to
/// `handle_exec_message`, wrapped in an async block) share one control flow.
/// `flush_havoc` is the same loop over `filter.flush()` for the disconnect
/// teardown that emits any divergence-held events.
async fn dispatch_havoc<F, Fut>(
    filter: &mut HavocFilter,
    msg: ServerMessage,
    sim: SimClock,
    mut handle: F,
) where
    F: FnMut(ServerMessage) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for (msg, delay) in filter.apply(msg) {
        sleep_havoc_delay(sim, delay).await;
        handle(msg).await;
    }
}

async fn flush_havoc<F, Fut>(filter: &mut HavocFilter, sim: SimClock, mut handle: F)
where
    F: FnMut(ServerMessage) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    for (msg, delay) in filter.flush() {
        sleep_havoc_delay(sim, delay).await;
        handle(msg).await;
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
            for trade in trades {
                let (sink, instruments, subs, bars) =
                    (&ctx.sink, &ctx.instruments, &ctx.subs, &ctx.bars);
                dispatch_havoc(
                    &mut ctx.havoc_filter,
                    ServerMessage::Trade(trade),
                    ctx.sim,
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
fn lock_recover<'a, T>(mutex: &'a Mutex<T>, label: &str) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        tracing::warn!(lock = label, "recovering from poisoned mutex");
        poisoned.into_inner()
    })
}

/// Records a spawned task's handle so `stop()`/`disconnect()` can abort it
/// (AD17/AE19: a `request_*` fetch or an HTTP order dispatch spawned just before
/// disconnect otherwise keeps issuing HTTP requests, racing the HttpQuota, and
/// sends into a possibly-dropped sink after the client stopped). Already-finished
/// handles are pruned on every insert so the vec is bounded by the count of
/// in-flight tasks rather than growing once per short-lived request over the
/// client's lifetime. Shared behind the `Arc<Mutex<..>>` so the `&self` request
/// handlers can track their task alongside the `&mut self` connect path.
fn track_task(handles: &Arc<Mutex<Vec<JoinHandle<()>>>>, handle: JoinHandle<()>) {
    let mut handles = lock_recover(handles, "task handles");
    handles.retain(|h| !h.is_finished());
    handles.push(handle);
}

/// Aborts and clears every tracked task handle. Shared by both clients' `stop()`.
fn abort_tasks(handles: &Arc<Mutex<Vec<JoinHandle<()>>>>) {
    for handle in lock_recover(handles, "task handles").drain(..) {
        handle.abort();
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

fn instrument_def(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    symbol: &str,
) -> Option<InstrumentDef> {
    lock_recover(instruments, "instrument").get(symbol).cloned()
}

/// Warns exactly once per symbol that a streamed frame arrived with no seeded
/// `InstrumentDef`, so the symbol's data is being black-holed. The instruments
/// map is seeded once at connect; a symbol subscribed but absent from that seed
/// (a server config change or a later-added instrument) otherwise streams into
/// nothing with zero diagnostics. The drain (`emit_trade`, the quote arm) has
/// no HTTP handle to re-seed, so it can only surface the miss - the poll path,
/// which does have async/HTTP context, self-heals via `ensure_instrument`. The
/// dedup keeps a per-trade warn from flooding the log for a genuinely-missing
/// symbol; it is process-global, which is acceptable for a diagnostic that only
/// wants to fire on the transition into the black-holed state.
fn warn_missing_instrument_once(symbol: &str) {
    static WARNED: std::sync::OnceLock<Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    let mut warned = lock_recover(
        WARNED.get_or_init(|| Mutex::new(std::collections::HashSet::new())),
        "missing-instrument warn set",
    );
    if warned.insert(symbol.to_string()) {
        tracing::warn!(
            %symbol,
            "no instrument def for a streamed symbol; its data is black-holed \
             until the instrument is seeded (server config change or later-added \
             instrument)"
        );
    }
}

async fn seed_instruments(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
) -> anyhow::Result<()> {
    let defs = fetch_instruments(http, quota, base).await?;
    cache_instruments(instruments, defs);
    Ok(())
}

/// Push every seeded instrument into the Nautilus cache on connect.
///
/// Seeding only fills the adapter's local `InstrumentDef` map, which the
/// adapter consults for price/precision conversion. Nautilus's own cache is a
/// separate store fed exclusively by `DataEvent::Instrument`, and broadarrow's
/// executor refuses to process a bar whose instrument is absent from that cache
/// (a desync guard against advancing the shadow with no real order). A forward
/// run that subscribes to bars but never to the instrument itself would
/// therefore have the cache stay empty and every bar refused. Emitting the
/// seeded defs here populates the cache the instant the data client connects,
/// independent of whatever the strategy later subscribes to.
fn emit_seeded_instruments(
    sink: &UnboundedSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    sim: SimClock,
) {
    let ts_init = now_unix_nanos(sim);
    let defs: Vec<InstrumentDef> = lock_recover(instruments, "instrument")
        .values()
        .cloned()
        .collect();
    for def in defs {
        if let Some(instrument) = instrument_any_or_warn(&def, ts_init) {
            drop(sink.send(DataEvent::Instrument(instrument)));
        }
    }
}

/// Converts a seeded/fetched `InstrumentDef` into a nautilus instrument,
/// warning loudly on failure instead of swallowing it. `convert::instrument_any`
/// errors when the def carries a base/quote currency unknown to nautilus (an
/// exotic pair), and a silent drop here cascades: the instrument never reaches
/// the Nautilus cache, so broadarrow's executor refuses EVERY bar for the
/// symbol with no log line pointing at the cause - the exact failure
/// `emit_seeded_instruments` exists to prevent. Naming the symbol and the error
/// at every swallow site turns that into a diagnosable log.
fn instrument_any_or_warn(
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> Option<nautilus_model::instruments::InstrumentAny> {
    convert::instrument_any(def, ts_init)
        .map_err(|err| {
            tracing::warn!(
                symbol = %def.symbol,
                error = %err,
                "dropping instrument: unrepresentable (currency unknown to nautilus?); \
                 broadarrow will refuse every bar for this symbol"
            );
        })
        .ok()
}

async fn ensure_instrument(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    symbol: &str,
) -> anyhow::Result<InstrumentDef> {
    if let Some(def) = instrument_def(instruments, symbol) {
        return Ok(def);
    }
    seed_instruments(http, quota, base, instruments).await?;
    instrument_def(instruments, symbol).with_context(|| format!("unknown instrument {symbol}"))
}

fn cache_instruments(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    defs: Vec<InstrumentDef>,
) {
    let mut cache = lock_recover(instruments, "instrument");
    for def in defs {
        cache.insert(def.symbol.clone(), def);
    }
}

async fn fetch_instruments(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
) -> anyhow::Result<Vec<InstrumentDef>> {
    quota.wait().await;
    let response = http
        .get(
            join_url(base, "instruments"),
            None,
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await
        .context("fetch instruments")?;
    ensure!(
        response.status.is_success(),
        "fetch instruments returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode instruments")
}

/// Distinguishes a 404 (older server without GET /account, the only
/// warn-and-continue case) from every other pull failure (decode, 5xx, timeout,
/// transport), which must fail connect() rather than silently recreate the
/// first-fill `account not found in cache` this fix exists to eliminate.
#[derive(Debug)]
enum FetchAccountError {
    NotFound,
    Other(anyhow::Error),
}

impl std::fmt::Display for FetchAccountError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "fetch account returned 404"),
            Self::Other(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for FetchAccountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::NotFound => None,
            Self::Other(err) => err.source(),
        }
    }
}

impl From<anyhow::Error> for FetchAccountError {
    fn from(err: anyhow::Error) -> Self {
        Self::Other(err)
    }
}

async fn fetch_account(
    http: &HttpClient,
    quota: &HttpQuota,
    base: &str,
) -> Result<mogwai_protocol::AccountState, FetchAccountError> {
    quota.wait().await;
    let response = http
        .get(
            join_url(base, "account"),
            None,
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await
        .context("fetch account")?;
    if response.status.as_u16() == 404 {
        return Err(FetchAccountError::NotFound);
    }
    if !response.status.is_success() {
        return Err(FetchAccountError::Other(anyhow::anyhow!(
            "fetch account returned {}",
            response.status.as_u16()
        )));
    }
    serde_json::from_slice(&response.body)
        .context("decode account")
        .map_err(FetchAccountError::Other)
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

async fn post_order(
    http: &HttpClient,
    quota: &HttpQuota,
    url: &str,
    msg: &ClientMessage,
    timeout_secs: u64,
) -> anyhow::Result<Vec<ServerMessage>> {
    let body = serde_json::to_vec(msg).context("encode order")?;
    let mut headers = HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    quota.wait().await;
    let response = http
        .post(
            url.to_string(),
            None,
            Some(headers),
            Some(body),
            Some(timeout_secs),
            None,
        )
        .await
        .context("post order")?;
    ensure!(
        response.status.is_success(),
        "post order returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode order events")
}

async fn ship_server_havoc(
    http: &HttpClient,
    http_base: &str,
    spec: &HavocSpec,
    sim: SimClock,
) -> anyhow::Result<()> {
    let url = join_url(http_base, "control/divergence");
    for divergence in &spec.server {
        let body = serde_json::to_vec(divergence).context("encode divergence")?;
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let response = http
            .post(
                url.clone(),
                None,
                Some(headers),
                Some(body),
                Some(request_timeout_secs(&None, sim)),
                None,
            )
            .await
            .context("post divergence")?;
        ensure!(
            response.status.is_success(),
            "post divergence returned {}",
            response.status.as_u16()
        );
    }
    Ok(())
}

struct HavocFilter {
    latency: Option<HavocLatency>,
    drop_prob: f64,
    duplicate_prob: f64,
    reorder_prob: f64,
    rng: StdRng,
    held: Option<ServerMessage>,
}

impl HavocFilter {
    fn from_client(client: &ClientHavoc) -> Self {
        Self {
            latency: client.latency,
            drop_prob: client.drop_prob,
            duplicate_prob: client.duplicate_prob,
            reorder_prob: client.reorder_prob,
            rng: client
                .seed
                .map_or_else(|| StdRng::from_rng(&mut rand::rng()), StdRng::seed_from_u64),
            held: None,
        }
    }

    fn apply(&mut self, msg: ServerMessage) -> Vec<(ServerMessage, Duration)> {
        let mut candidates = Vec::new();
        if let Some(held) = self.held.take() {
            candidates.push(msg);
            candidates.push(held);
        } else if self.draw(self.reorder_prob) {
            self.held = Some(msg);
            return Vec::new();
        } else {
            candidates.push(msg);
        }
        self.emit_candidates(candidates)
    }

    fn flush(&mut self) -> Vec<(ServerMessage, Duration)> {
        let Some(held) = self.held.take() else {
            return Vec::new();
        };
        self.emit_candidates(vec![held])
    }

    fn emit_candidates(
        &mut self,
        candidates: Vec<ServerMessage>,
    ) -> Vec<(ServerMessage, Duration)> {
        let mut out = Vec::new();
        for msg in candidates {
            if self.draw(self.drop_prob) {
                continue;
            }
            let delay = self.delay_for(&msg);
            out.push((msg.clone(), delay));
            if self.draw(self.duplicate_prob) {
                out.push((msg, delay));
            }
        }
        out
    }

    fn delay_for(&self, msg: &ServerMessage) -> Duration {
        let category = msg.category();
        let baseline = mogwai_protocol::BASELINE_LATENCY.delay_for(category);
        let armed = self
            .latency
            .map_or(Duration::ZERO, |latency| latency.delay_for(category));
        baseline + armed
    }

    fn draw(&mut self, probability: f64) -> bool {
        probability > 0.0 && self.rng.random::<f64>() < probability
    }
}

fn client_havoc(spec: &Option<HavocSpec>) -> ClientHavoc {
    spec.as_ref()
        .map_or_else(ClientHavoc::default, |spec| spec.client.clone())
}

fn data_regime(spec: &Option<HavocSpec>) -> Option<MarketRegime> {
    spec.as_ref().and_then(|spec| spec.data)
}

fn conn_havoc(spec: &Option<HavocSpec>) -> ConnHavoc {
    spec.as_ref()
        .map_or_else(ConnHavoc::default, |spec| spec.conn)
}

/// Wall floor for the scaled HTTP request timeout, in seconds. Unlike the other
/// scaled durations (whose floor is the ~1ms tokio granularity), this one guards
/// a REAL local-IO round trip whose wall cost does NOT compress with `speed`:
/// dividing a sim-seconds timeout by a high `speed` would otherwise yield a
/// sub-second wall budget that the actual HTTP round trip blows, spuriously
/// timing out every order. Clamping UP to one wall second keeps the request
/// survivable; the consequence is that `request_timeout_secs` is the tightest
/// contributor to the usable-speed ceiling. Documented in `reference/clock.md`.
const MIN_WALL_REQUEST_TIMEOUT_SECS: u64 = 1;

fn request_timeout_secs(spec: &Option<HavocSpec>, sim: SimClock) -> u64 {
    let configured = conn_havoc(spec).request_timeout_secs;
    let sim_secs = if configured == 0 {
        mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS
    } else {
        configured
    };
    let wall_ns = sim.wall_span(sim_secs.saturating_mul(1_000_000_000));
    wall_ns
        .div_ceil(1_000_000_000)
        .max(MIN_WALL_REQUEST_TIMEOUT_SECS)
}

fn client_havoc_for_dispatch(spec: &Option<HavocSpec>, counter: u64) -> ClientHavoc {
    let mut client = client_havoc(spec);
    if let Some(seed) = client.seed.as_mut() {
        *seed ^= counter;
    }
    client
}

async fn sleep_havoc_delay(sim: SimClock, delay: Duration) {
    if !delay.is_zero() {
        tokio::time::sleep(sim.wall_duration(duration_to_nanos(delay))).await;
    }
}

/// Wall backoff between clock-fetch retries. Small and fixed: this runs inline
/// in connect(), so it must not stall boot for long, but a couple of retries
/// ride out a transient blip before the identity fallback commits the whole
/// connection to the wrong time axis.
const CLOCK_FETCH_RETRY_DELAY: Duration = Duration::from_millis(200);
const CLOCK_FETCH_MAX_ATTEMPTS: u32 = 3;

async fn fetch_clock_or_identity(http: &HttpClient, http_base: &str) -> ServerClock {
    // Retry before committing to identity (AD16): the identity fallback silently
    // puts EVERY ts_init, havoc sleep, quota interval, backoff and timeout on the
    // wrong axis for the life of the connection if the server actually runs at
    // speed != 1, and nothing re-fetches the clock later. A couple of quick
    // retries ride out a transient fetch blip; only a persistent failure falls
    // back, and then loudly.
    let mut last_err = None;
    for attempt in 0..CLOCK_FETCH_MAX_ATTEMPTS {
        match fetch_clock(http, http_base).await {
            Ok(clock) => {
                if attempt > 0 {
                    tracing::info!(attempt, "clock fetch recovered after retry");
                }
                return clock;
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    attempt,
                    "clock fetch failed; retrying before the identity fallback"
                );
                last_err = Some(err);
                if attempt + 1 < CLOCK_FETCH_MAX_ATTEMPTS {
                    tokio::time::sleep(CLOCK_FETCH_RETRY_DELAY).await;
                }
            }
        }
    }
    tracing::error!(
        err = ?last_err,
        attempts = CLOCK_FETCH_MAX_ATTEMPTS,
        "clock fetch failed after retries; falling back to the identity mogwai \
         clock - if the server runs at speed != 1, every ts_init, havoc sleep, \
         quota interval, backoff and timeout will be scaled on the wrong axis for \
         the life of this connection"
    );
    // No reachable server: identity map and an UNKNOWN tape floor. A
    // `data_origin_ns` of 0 is the "floor unknown" sentinel the warmup
    // guard checks - it skips the pre-flight refusal and lets the server's
    // own 422 stand if a later request is off-tape.
    ServerClock {
        sim: SimClock::identity(),
        server_now_ns: 0,
        data_origin_ns: 0,
        backfill_horizon_ns: 0,
    }
}

fn duration_to_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// Resolves the effective row limit sent to the server's bounded `/trades`
/// scan. A missing limit defaults to the ceiling, and any requested limit is
/// clamped to it so neither the response body nor the materialized nautilus
/// response `Vec` can grow unbounded over a multi-GB dump.
fn capped_limit(limit: Option<std::num::NonZeroUsize>) -> usize {
    limit
        .map_or(
            mogwai_protocol::MAX_HISTORY_LIMIT,
            std::num::NonZeroUsize::get,
        )
        .min(mogwai_protocol::MAX_HISTORY_LIMIT)
}

pub(crate) fn join_url(base: &str, path: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), path)
}

fn symbol_from_instrument(instrument_id: InstrumentId) -> String {
    instrument_id.symbol.to_string()
}

fn start_ts_param(params: &Option<Params>) -> Option<u64> {
    params.as_ref().and_then(|p| p.get_u64("start_ts"))
}

fn date_to_unix_nanos(date: Option<chrono::DateTime<chrono::Utc>>) -> Option<UnixNanos> {
    date.and_then(|dt| dt.timestamp_nanos_opt())
        .and_then(|ns| u64::try_from(ns).ok())
        .map(UnixNanos::from)
}

/// Refuse an off-tape warmup BEFORE spending a round trip on it. A `start`
/// below the published `data_origin` can never be served (the tape begins at the
/// origin), so a fetch would come back an empty `200` the warmup cannot tell
/// from "no trades happened" - or, post-Landing-2, a server `422`. Failing here,
/// naming both the requested start and the floor, turns that into a loud,
/// surfaced error at the request boundary instead of a silent doomed fetch.
///
/// `data_origin == 0` is the "floor unknown" sentinel (the `/clock` fetch failed
/// and the client fell back to identity): the check is skipped so the server's
/// own refusal stays authoritative. A `None` start means "from origin" and is
/// always on-tape.
fn ensure_on_tape(start: Option<UnixNanos>, data_origin: u64) -> anyhow::Result<()> {
    if let Some(start) = start
        && data_origin != 0
        && start.as_u64() < data_origin
    {
        anyhow::bail!(
            "requested start {} precedes data_origin_ns {}; the mogwai tape cannot serve before its origin",
            start.as_u64(),
            data_origin
        );
    }
    Ok(())
}

fn now_unix_nanos(sim: SimClock) -> UnixNanos {
    // Thin typed wrapper over the shared wall read plus the fetched simulated
    // clock. The underlying reader keeps the saturating contract; the affine
    // map then places adapter-side `ts_init` on the same axis as the server.
    UnixNanos::from(sim.sim_ns(mogwai_protocol::now_unix_nanos()))
}

async fn wait_connected(connected: &Arc<AtomicBool>, ws_url: &str) -> anyhow::Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        if connected.load(Ordering::Relaxed) {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    anyhow::bail!("connect websocket {ws_url} timed out")
}

#[derive(Debug)]
pub struct MogwaiExecutionClient {
    core: ExecutionClientCore,
    config: MogwaiExecClientConfig,
    emitter: ExecutionEventEmitter,
    connected: Arc<AtomicBool>,
    http: HttpClient,
    http_quota: HttpQuota,
    sim: SimClock,
    ws_cmd: Option<UnboundedSender<ExecWsCommand>>,
    state: Arc<Mutex<ExecState>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    http_dispatch_counter: Arc<AtomicU64>,
    /// Handles for the WS reader and every spawned HTTP order dispatch. Shared
    /// behind an `Arc<Mutex<..>>` so the `&self` `dispatch_order` can record its
    /// spawned POST task; `stop()` aborts the lot so a slow POST cannot emit exec
    /// events (its emitter still holds a live sender clone) after the client
    /// stopped (AE19).
    task_handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl MogwaiExecutionClient {
    /// Creates a new disconnected mogwai execution client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(core: ExecutionClientCore, config: MogwaiExecClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        let emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            config.trader_id,
            config.account_id,
            config.account_type,
            None,
        );
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
            core,
            http_quota: HttpQuota::from_conn(&conn_havoc(&config.havoc), SimClock::identity()),
            config,
            emitter,
            connected: Arc::new(AtomicBool::new(false)),
            http,
            sim: SimClock::identity(),
            ws_cmd: None,
            state: Arc::new(Mutex::new(ExecState::default())),
            instruments: Arc::new(Mutex::new(HashMap::new())),
            http_dispatch_counter: Arc::new(AtomicU64::new(0)),
            task_handles: Arc::new(Mutex::new(Vec::new())),
        })
    }

    #[cfg(test)]
    fn is_started(&self) -> bool {
        self.core.is_started()
    }

    #[cfg(test)]
    fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }

    fn send_ws(&self, cmd: ExecWsCommand) -> anyhow::Result<()> {
        let tx = self
            .ws_cmd
            .as_ref()
            .context("mogwai execution client is not connected")?;
        tx.send(cmd).context("send execution websocket command")
    }

    fn exec_context(&self) -> ExecContext {
        ExecContext {
            emitter: self.emitter.clone(),
            state: Arc::clone(&self.state),
            instruments: Arc::clone(&self.instruments),
            trader_id: self.core.trader_id,
            account_id: self.core.account_id,
            account_type: self.config.account_type,
            sim: self.sim,
        }
    }

    fn dispatch_order(&self, cmd: ExecWsCommand) -> anyhow::Result<()> {
        if self.config.transport_profile.orders_over_http() {
            let msg = exec_command_to_client_message(cmd.clone());
            let http = self.http.clone();
            let http_quota = self.http_quota.clone();
            let url = join_url(&self.config.http_base_url(), "orders");
            let ctx = self.exec_context();
            let counter = self.http_dispatch_counter.fetch_add(1, Ordering::Relaxed);
            let client_havoc = client_havoc_for_dispatch(&self.config.havoc, counter);
            let timeout_secs = request_timeout_secs(&self.config.havoc, self.sim);
            track_task(&self.task_handles, get_runtime().spawn(async move {
                let mut filter = HavocFilter::from_client(&client_havoc);
                match post_order(&http, &http_quota, &url, &msg, timeout_secs).await {
                    Ok(events) => {
                        for event in events {
                            dispatch_havoc(&mut filter, event, ctx.sim, |msg| async {
                                handle_exec_message(msg, &ctx);
                            })
                            .await;
                        }
                        flush_havoc(&mut filter, ctx.sim, |msg| async {
                            handle_exec_message(msg, &ctx);
                        })
                        .await;
                    }
                    Err(err) => synthesize_transport_reject(&cmd, &err, &ctx),
                }
            }));
            Ok(())
        } else if let Err(err) = self.send_ws(cmd.clone()) {
            // The WS command channel is gone (reconnect exhausted, or the client
            // was stopped), so the command never reached the venue. Nautilus only
            // LOGS an Err from cancel_order/modify_order (no event), so without
            // this a cancel/modify would sit forever in PendingCancel/PendingUpdate
            // (and a submit in Submitted) with no reject to restore it - unlike the
            // HTTP path, which already synthesizes the matching reject on transport
            // failure. Synthesize it here too and report success: the reject event
            // is the signal, not the return value (matching the HTTP path, whose
            // spawn returns Ok and surfaces the failure only as the event) (AE9).
            let ctx = self.exec_context();
            synthesize_transport_reject(&cmd, &err, &ctx);
            Ok(())
        } else {
            Ok(())
        }
    }

    /// Block until the runner has drained the forwarded AccountState and the
    /// cache holds the account row, or the timeout elapses. The forward only
    /// queues an event; this proves the row is present before connect() returns
    /// so the first order is not worked against an unknown account.
    async fn await_account_registered(&self) -> anyhow::Result<()> {
        let deadline = tokio::time::Instant::now() + ACCOUNT_REGISTRATION_TIMEOUT;
        loop {
            if self
                .core
                .cache()
                .account_owned(&self.core.account_id)
                .is_some()
            {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "account {} not registered after {:?}",
                    self.core.account_id,
                    ACCOUNT_REGISTRATION_TIMEOUT
                );
            }
            tokio::time::sleep(ACCOUNT_REGISTRATION_POLL).await;
        }
    }
}

#[async_trait(?Send)]
impl ExecutionClient for MogwaiExecutionClient {
    fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    fn client_id(&self) -> ClientId {
        self.core.client_id
    }

    fn account_id(&self) -> AccountId {
        self.core.account_id
    }

    fn venue(&self) -> Venue {
        *MOGWAI_VENUE
    }

    fn oms_type(&self) -> OmsType {
        self.core.oms_type
    }

    fn get_account(&self) -> Option<AccountAny> {
        self.core.cache().account_owned(&self.core.account_id)
    }

    fn generate_account_state(
        &self,
        balances: Vec<AccountBalance>,
        margins: Vec<MarginBalance>,
        reported: bool,
        ts_event: UnixNanos,
    ) -> anyhow::Result<()> {
        self.emitter.send_account_state(NautilusAccountState::new(
            self.core.account_id,
            self.config.account_type,
            balances,
            margins,
            reported,
            UUID4::new(),
            ts_event,
            now_unix_nanos(self.sim),
            None,
        ));
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        if let Some(sender) = try_get_exec_event_sender() {
            self.emitter.set_sender(sender);
        }
        self.core.set_started();
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self.core.is_stopped() {
            return Ok(());
        }

        self.connected.store(false, Ordering::Relaxed);
        self.ws_cmd = None;
        abort_tasks(&self.task_handles);
        self.core.set_stopped();
        self.core.set_disconnected();
        Ok(())
    }

    fn reset(&mut self) -> anyhow::Result<()> {
        // Mirror MogwaiDataClient::reset: stop first (abort tasks, drop the WS
        // command channel), then clear the reconciliation mirror. Without this
        // the default no-op `reset` leaves ExecState.orders/fills/positions
        // populated across a stop/start, so a prior session's orders leak into
        // the next session's status/fill/position reports.
        self.stop()?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        *state = ExecState::default();
        Ok(())
    }

    async fn connect(&mut self) -> anyhow::Result<()> {
        let http_base_url = self.config.http_base_url();
        // The execution client rides only the affine map; the tape boundary in
        // the envelope is the data client's concern.
        let sim = fetch_clock_or_identity(&self.http, &http_base_url)
            .await
            .sim;
        self.sim = sim;
        let conn = conn_havoc(&self.config.havoc);
        self.http_quota = HttpQuota::from_conn(&conn, sim);
        seed_instruments(
            &self.http,
            &self.http_quota,
            &http_base_url,
            &self.instruments,
        )
        .await?;
        if let Some(havoc) = &self.config.havoc {
            ship_server_havoc(&self.http, &http_base_url, havoc, sim).await?;
        }
        // Seed the bridge's account row before any order is worked. Pull the
        // venue's current snapshot and forward it through the same exec dispatch
        // an inbound AccountState frame uses, so the cache row exists before the
        // first order's events arrive instead of erroring `account not found in
        // cache`. Instruments are already seeded above, so any positions in the
        // snapshot resolve their defs.
        //
        // Forwarding alone is necessary but not sufficient: handle_account_state
        // only sends an ExecutionEvent::Account onto the exec channel, which the
        // runner drains and applies to the cache asynchronously - possibly after
        // connect() returns and the first order is worked. So we block on
        // await_account_registered (mirroring every canonical adapter's
        // await_account_registered) until the cache row is proven present.
        //
        // This pull bypasses the HavocFilter that WS-pushed AccountState frames
        // pass through, by design: the snapshot is a point-in-time query, not a
        // tape frame, so a DropNextAccountUpdate divergence does not suppress it.
        //
        // Failure policy: a 404 means a server predating GET /account; warn and
        // fall back to the legacy reactive path (the account seeds off the first
        // fill, as before this fix). Any OTHER failure against a server that does
        // publish the route is fatal - warn-and-continue there would silently
        // recreate the exact first-fill cache-miss this fix exists to eliminate.
        match fetch_account(&self.http, &self.http_quota, &http_base_url).await {
            Ok(state) => {
                handle_exec_message(ServerMessage::AccountState(state), &self.exec_context());
                self.await_account_registered().await?;
            }
            Err(FetchAccountError::NotFound) => {
                tracing::warn!("server predates GET /account; account will seed on first fill");
            }
            Err(err) => {
                return Err(anyhow::Error::new(err).context("initial account snapshot"));
            }
        }
        let client_havoc = client_havoc(&self.config.havoc);

        if self.config.transport_profile.orders_over_http() {
            self.connected.store(true, Ordering::Relaxed);
            self.core.set_connected();
            return Ok(());
        }

        let ws_url = join_url(&self.config.ws_url(), "ws");
        let (cmd_tx, cmd_rx) = unbounded_channel::<ExecWsCommand>();
        self.ws_cmd = Some(cmd_tx);

        let connected = Arc::clone(&self.connected);
        let ctx = self.exec_context();
        let havoc_filter = Arc::new(tokio::sync::Mutex::new(HavocFilter::from_client(
            &client_havoc,
        )));
        let handler_filter = Arc::clone(&havoc_filter);
        let disconnect_filter = Arc::clone(&havoc_filter);
        let handler_ctx = ctx.clone();
        let disconnect_ctx = ctx;
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
                exec_command_to_client_message,
                Vec::new,
                move |server_msg| {
                    let handler_filter = Arc::clone(&handler_filter);
                    let ctx = handler_ctx.clone();
                    async move {
                        let mut filter = handler_filter.lock().await;
                        dispatch_havoc(&mut filter, server_msg, sim, |msg| async {
                            handle_exec_message(msg, &ctx);
                        })
                        .await;
                    }
                },
                move || {
                    let disconnect_filter = Arc::clone(&disconnect_filter);
                    let ctx = disconnect_ctx.clone();
                    async move {
                        let mut filter = disconnect_filter.lock().await;
                        flush_havoc(&mut filter, sim, |msg| async {
                            handle_exec_message(msg, &ctx);
                        })
                        .await;
                    }
                },
            )
            .await;
        });

        track_task(&self.task_handles, reader_handle);
        // See MogwaiDataClient::connect: a timed-out connect must abort the
        // just-spawned reader and clear the stale handle/ws_cmd so a retry does
        // not orphan the first task racing on the shared `connected` flag.
        if let Err(err) = wait_connected(&self.connected, &ws_url).await {
            if let Some(handle) = lock_recover(&self.task_handles, "task handles").pop() {
                handle.abort();
            }
            self.ws_cmd = None;
            return Err(err);
        }
        self.core.set_connected();
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;

        // Build the wire order FIRST (AE8). An unsupported side/type/TIF errors
        // out of convert::wire_* here; emitting OrderSubmitted before this - as
        // the code used to - queued a Submitted event that nautilus then had to
        // apply to an order it had already denied (Initialized -> Denied) on the
        // very same failed conversion, producing a stray event, a scary
        // invalid-transition log, and (on a later send failure) a permanently
        // Submitted mirror stray that fed the unbounded ExecState growth.
        // Converting first means a conversion failure returns before any event is
        // emitted or any mirror record exists.
        let wire = mogwai_protocol::SubmitOrder {
            client_order_id: cmd.client_order_id.to_string(),
            symbol: symbol_from_instrument(cmd.instrument_id),
            side: convert::wire_side(cmd.order_init.order_side)?,
            order_type: convert::wire_order_type(cmd.order_init.order_type)?,
            quantity: cmd.order_init.quantity.as_decimal(),
            price: cmd.order_init.price.map(|p| p.as_decimal()),
            time_in_force: convert::wire_time_in_force(cmd.order_init.time_in_force)?,
        };

        let ts_init = now_unix_nanos(self.sim);
        let submitted = OrderSubmitted::new(
            self.core.trader_id,
            order.strategy_id(),
            order.instrument_id(),
            order.client_order_id(),
            self.core.account_id,
            UUID4::new(),
            ts_init,
            ts_init,
        );

        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
            state.orders.insert(
                cmd.client_order_id,
                OrderRecord {
                    strategy_id: cmd.strategy_id,
                    instrument_id: cmd.instrument_id,
                    order_side: cmd.order_init.order_side,
                    order_type: cmd.order_init.order_type,
                    time_in_force: cmd.order_init.time_in_force,
                    status: OrderStatus::Submitted,
                    quantity: cmd.order_init.quantity.as_decimal(),
                    price: cmd.order_init.price.map(|p| p.as_decimal()),
                    filled_qty: Decimal::ZERO,
                    avg_px: None,
                    venue_order_id: None,
                    ts_accepted: cmd.ts_init,
                    ts_last: cmd.ts_init,
                    seen_trades: std::collections::HashSet::new(),
                },
            );
            state.prune();
        }

        // Emit Submitted only now that conversion has succeeded and the mirror
        // record exists. The dispatch below may still fail at transport (WS
        // channel gone, or an HTTP POST error), in which case dispatch_order
        // synthesizes the matching OrderRejected - a valid Submitted -> Rejected
        // transition - so the order still reaches a terminal state.
        self.emitter
            .send_order_event(OrderEventAny::Submitted(submitted));

        self.dispatch_order(ExecWsCommand::Submit(wire))
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        self.dispatch_order(ExecWsCommand::Modify {
            client_order_id: cmd.client_order_id.to_string(),
            price: cmd.price.map(|p| p.as_decimal()),
            quantity: cmd.quantity.map(|q| q.as_decimal()),
        })
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        self.dispatch_order(ExecWsCommand::Cancel {
            client_order_id: cmd.client_order_id.to_string(),
        })
    }

    async fn generate_order_status_reports(
        &self,
        cmd: &GenerateOrderStatusReports,
    ) -> anyhow::Result<Vec<OrderStatusReport>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        let reports = state
            .orders
            .iter()
            .filter(|(_, record)| !cmd.open_only || record.status.is_open())
            .filter(|(_, record)| {
                cmd.instrument_id
                    .is_none_or(|id| id == record.instrument_id)
            })
            .filter(|(_, record)| {
                // An open order requested under open_only is always included,
                // regardless of when it last had activity: a real venue
                // mass-status returns every resting order, and reconciliation
                // passes a lookback-bounded `start`, so filtering a long-quiet
                // open order by `ts_last` used to hide it - and the manager then
                // infers it canceled-at-venue (AE10). The time filter still
                // applies to closed/historical records (open_only false).
                (cmd.open_only && record.status.is_open())
                    || in_time_range(record.ts_last, cmd.start, cmd.end)
            })
            .filter_map(|(client_order_id, record)| {
                // A Submitted order with no venue ack yet carries no
                // venue_order_id. `VenueOrderId::from("")` routes through the
                // panicking `VenueOrderId::new`, which nautilus's own
                // `#[should_panic]` empty-string test confirms rejects the
                // empty string - so a `None` cannot be papered over with a
                // placeholder here. There is also nothing venue-side to
                // reconcile yet for an order the venue has not acknowledged,
                // so the record is dropped from this report set (with a
                // warning) rather than risk a bogus/duplicate venue id
                // corrupting the reconciliation manager's venue-order-id
                // index.
                let venue_order_id = record.venue_order_id.or_else(|| {
                    tracing::warn!(
                        order = %client_order_id,
                        status = ?record.status,
                        "omitting order status report: no venue order id yet (unacked submit)"
                    );
                    None
                })?;
                // A record whose mirrored quantity/filled_qty cannot represent
                // as a nautilus Quantity (hostile magnitude/precision off the
                // wire) is dropped from the report set with a warning rather
                // than panicking the report generator.
                let quantity = record
                    .quantity_for_report(&self.instruments)
                    .map_err(|err| {
                        tracing::warn!(
                            order = %client_order_id,
                            error = %err,
                            "dropping order status report: unrepresentable quantity"
                        );
                    })
                    .ok()?;
                let filled = record
                    .filled_quantity_for_report(&self.instruments)
                    .map_err(|err| {
                        tracing::warn!(
                            order = %client_order_id,
                            error = %err,
                            "dropping order status report: unrepresentable filled quantity"
                        );
                    })
                    .ok()?;
                Some(OrderStatusReport::new(
                    self.core.account_id,
                    record.instrument_id,
                    Some(*client_order_id),
                    venue_order_id,
                    record.order_side,
                    record.order_type,
                    record.time_in_force,
                    record.status,
                    quantity,
                    filled,
                    record.ts_accepted,
                    record.ts_last,
                    now_unix_nanos(self.sim),
                    None,
                ))
            })
            .collect();
        Ok(reports)
    }

    async fn generate_fill_reports(
        &self,
        cmd: GenerateFillReports,
    ) -> anyhow::Result<Vec<FillReport>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        let reports = state
            .fills
            .iter()
            .filter(|fill| cmd.instrument_id.is_none_or(|id| id == fill.instrument_id))
            .filter(|fill| {
                cmd.venue_order_id
                    .is_none_or(|id| id == fill.venue_order_id)
            })
            .filter(|fill| in_time_range(fill.ts_event, cmd.start, cmd.end))
            .filter_map(|fill| {
                // A commission that cannot represent as nautilus Money drops
                // just this fill report with a warning; the rest still report.
                let commission = convert::money(fill.commission, fill.quote_currency)
                    .map_err(|err| {
                        tracing::warn!(
                            trade = %fill.trade_id,
                            error = %err,
                            "dropping fill report: unrepresentable commission"
                        );
                    })
                    .ok()?;
                Some(FillReport::new(
                    self.core.account_id,
                    fill.instrument_id,
                    fill.venue_order_id,
                    fill.trade_id,
                    fill.order_side,
                    fill.last_qty,
                    fill.last_px,
                    commission,
                    LiquiditySide::Taker,
                    Some(fill.client_order_id),
                    None,
                    fill.ts_event,
                    now_unix_nanos(self.sim),
                    None,
                ))
            })
            .collect();
        Ok(reports)
    }

    async fn generate_position_status_reports(
        &self,
        cmd: &GeneratePositionStatusReports,
    ) -> anyhow::Result<Vec<PositionStatusReport>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("execution state mutex poisoned"))?;
        let reports = state
            .positions
            .values()
            .filter(|position| {
                cmd.instrument_id
                    .is_none_or(|id| id == position.instrument_id)
            })
            .filter(|position| {
                // Every position the mirror holds is a current open (nonzero)
                // venue position - the account-snapshot apply drops flat
                // instruments - so a lookback-bounded `start` must not hide a
                // long-quiet resting position (reconciliation would otherwise
                // have to re-adopt it as EXTERNAL mid-run) (AE10). Include any
                // open position regardless of last-activity time; the time filter
                // still applies to a defensive flat entry.
                !position.quantity.is_zero()
                    || in_time_range(position.ts_last, cmd.start, cmd.end)
            })
            .filter_map(|position| {
                let def = instrument_def(&self.instruments, &position.symbol)?;
                let quantity = convert::quantity(position.quantity.abs(), def.size_precision)
                    .map_err(|err| {
                        tracing::warn!(
                            symbol = %position.symbol,
                            error = %err,
                            "dropping position report: unrepresentable quantity"
                        );
                    })
                    .ok()?;
                Some(PositionStatusReport::new(
                    self.core.account_id,
                    position.instrument_id,
                    position_side(position.quantity),
                    quantity,
                    position.ts_last,
                    now_unix_nanos(self.sim),
                    None,
                    None,
                    Some(position.avg_px),
                ))
            })
            .collect();
        Ok(reports)
    }

    /// Composes the three report generators into the mass status the live
    /// node's startup reconciliation consumes. The trait default returns
    /// `Ok(None)`, which the node logs as "no mass status available (likely
    /// adapter error)" and then reconciles NOTHING - a worker restarted while
    /// holding an open mogwai position would boot flat and only discover the
    /// venue net via the periodic position poll, mid-run, as a late EXTERNAL
    /// adoption. Following the canonical adapter shape (e.g. kraken spot):
    /// open orders, their fills, and current positions, bounded by the
    /// caller's lookback.
    ///
    /// `lookback_mins` maps to the same `start` bound the three generators
    /// already apply (each filters records by `ts_last`/`ts_event` within
    /// `[start, end]`), computed against sim-now because every mirrored
    /// timestamp lives on the venue's sim axis. `None` means unbounded.
    async fn generate_mass_status(
        &self,
        lookback_mins: Option<u64>,
    ) -> anyhow::Result<Option<ExecutionMassStatus>> {
        let ts_init = now_unix_nanos(self.sim);
        let start = lookback_mins.map(|mins| {
            UnixNanos::from(
                ts_init
                    .as_u64()
                    .saturating_sub(mins.saturating_mul(60 * 1_000_000_000)),
            )
        });
        let order_reports = self
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                ts_init,
                true,
                None,
                start,
                None,
                None,
                None,
            ))
            .await?;
        let fill_reports = self
            .generate_fill_reports(GenerateFillReports::new(
                UUID4::new(),
                ts_init,
                None,
                None,
                start,
                None,
                None,
                None,
            ))
            .await?;
        let position_reports = self
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                ts_init,
                None,
                start,
                None,
                None,
                None,
            ))
            .await?;

        let mut mass_status = ExecutionMassStatus::new(
            self.core.client_id,
            self.core.account_id,
            *MOGWAI_VENUE,
            ts_init,
            None,
        );
        mass_status.add_order_reports(order_reports);
        mass_status.add_fill_reports(fill_reports);
        mass_status.add_position_reports(position_reports);

        Ok(Some(mass_status))
    }
}

#[derive(Debug, Clone)]
enum ExecWsCommand {
    Submit(mogwai_protocol::SubmitOrder),
    Cancel {
        client_order_id: String,
    },
    Modify {
        client_order_id: String,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
    },
}

fn exec_command_to_client_message(cmd: ExecWsCommand) -> ClientMessage {
    match cmd {
        ExecWsCommand::Submit(order) => ClientMessage::SubmitOrder(order),
        ExecWsCommand::Cancel { client_order_id } => ClientMessage::CancelOrder { client_order_id },
        ExecWsCommand::Modify {
            client_order_id,
            price,
            quantity,
        } => ClientMessage::ModifyOrder {
            client_order_id,
            price,
            quantity,
        },
    }
}

/// Maps a transport-level failure (the HTTP POST for the command never got a
/// venue reply) onto the `ServerMessage` shape `handle_exec_message` already
/// knows how to turn into a nautilus event. Only valid for `Submit`/`Modify`:
/// a failed `Cancel` is not a full order rejection (the order is still live,
/// or its fate is simply unknown) and is handled by `emit_cancel_rejected`
/// before the call site ever reaches this function - see the comment at the
/// `dispatch_order` call site.
fn reject_for(cmd: &ExecWsCommand, err: &anyhow::Error, sim: SimClock) -> ServerMessage {
    let reason = err.to_string();
    let ts_event = now_unix_nanos(sim).as_u64();
    match cmd {
        ExecWsCommand::Submit(order) => ServerMessage::OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason,
            ts_event,
        },
        ExecWsCommand::Modify {
            client_order_id, ..
        } => ServerMessage::OrderModifyRejected {
            client_order_id: client_order_id.clone(),
            venue_order_id: None,
            reason,
            ts_event,
        },
        ExecWsCommand::Cancel { client_order_id } => unreachable!(
            "cancel transport failures are reported via emit_cancel_rejected, \
             not reject_for (client_order_id={client_order_id})"
        ),
    }
}

/// Synthesizes the nautilus reject for a command whose transport failed before
/// the venue ever saw it - shared by the HTTP POST error path and the WS
/// send-failure path (AE9). A failed `Cancel` is reported as a `CancelRejected`
/// (the order is still live, or its fate is simply unknown, not dead), leaving
/// the mirrored status untouched; a failed `Submit`/`Modify` is reported as the
/// matching `OrderRejected`/`OrderModifyRejected` so the order reaches a terminal
/// state instead of wedging in `Submitted`/`PendingUpdate`. Both bypass the
/// per-dispatch `HavocFilter` by design: the failure is purely local and never
/// traveled the wire, so there is nothing for the venue-havoc pipeline to model,
/// and routing a terminal reject through a `drop_prob` draw could discard it
/// entirely, leaving nautilus and the mirror stuck forever.
fn synthesize_transport_reject(cmd: &ExecWsCommand, err: &anyhow::Error, ctx: &ExecContext) {
    if let ExecWsCommand::Cancel { client_order_id } = cmd {
        if let Some(client_order_id) = wire_client_order_id(client_order_id) {
            emit_cancel_rejected(
                client_order_id,
                None,
                err.to_string(),
                now_unix_nanos(ctx.sim),
                ctx,
            );
        }
    } else {
        handle_exec_message(reject_for(cmd, err, ctx.sim), ctx);
    }
}

/// Reports a rejected `Cancel` as a nautilus `OrderCancelRejected` without
/// touching the mirrored order's status. Serves both origins:
///
/// - a `Cancel` that failed at TRANSPORT (the HTTP POST never reached the
///   venue): `ts_event` is sim-now and `wire_venue_order_id` is `None`, and
/// - a venue-originated `ServerMessage::OrderCancelRejected` (the engine could
///   not honor the cancel): `ts_event` is the venue's stamp and the wire may
///   name the venue id.
///
/// Unlike a submit rejection, a failed cancel does not mean the order is dead:
/// nautilus's own order FSM restores the pre-cancel status on `CancelRejected`
/// (see `orders/mod.rs`'s `(PendingCancel, CancelRejected) => PendingCancel`
/// transition), so the mirror must likewise leave `record.status` untouched -
/// the order is still whatever it was (Accepted, PartiallyFilled, ...) before
/// the cancel was attempted. The emitted venue id prefers the wire value and
/// falls back to the mirror's, so a wire `None` on a known order still carries
/// the id the adapter already holds.
fn emit_cancel_rejected(
    client_order_id: ClientOrderId,
    wire_venue_order_id: Option<VenueOrderId>,
    reason: String,
    ts_event: UnixNanos,
    ctx: &ExecContext,
) {
    let Some(record) = order_record(&ctx.state, client_order_id) else {
        // Same limitation as OrderRejected/OrderModifyRejected (A.11): the
        // mirror lacks the order, so we have no real strategy_id/
        // instrument_id, and a placeholder would be silently dropped by
        // nautilus `Order.apply` strategy-id validation. Make the drop
        // visible rather than silent.
        tracing::warn!(
            order = %client_order_id,
            reason = %reason,
            "cancel rejected for an order the mirror does not know; \
             reject not surfaced to nautilus (A.11)"
        );
        return;
    };
    let event = OrderCancelRejected::new(
        ctx.trader_id,
        record.strategy_id,
        record.instrument_id,
        client_order_id,
        reason.into(),
        UUID4::new(),
        ts_event,
        now_unix_nanos(ctx.sim),
        false,
        wire_venue_order_id.or(record.venue_order_id),
        Some(ctx.account_id),
    );
    ctx.emitter
        .send_order_event(OrderEventAny::CancelRejected(event));
}

#[derive(Clone)]
struct ExecContext {
    emitter: ExecutionEventEmitter,
    state: Arc<Mutex<ExecState>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    trader_id: nautilus_model::identifiers::TraderId,
    account_id: AccountId,
    account_type: nautilus_model::enums::AccountType,
    sim: SimClock,
}

#[derive(Debug, Default)]
struct ExecState {
    orders: HashMap<ClientOrderId, OrderRecord>,
    fills: Vec<FillRecord>,
    positions: HashMap<Symbol, PositionRecord>,
    /// `ts_event` of the last account snapshot applied to the position mirror.
    /// Snapshots carry the venue's COMPLETE non-zero position set and are
    /// applied destructively (absent symbols are dropped as flat), which is
    /// only sound if each applied snapshot is at least as new as the last:
    /// reorder/duplicate havoc delivering an OLDER snapshot would resurrect a
    /// just-closed position (the phantom-EXTERNAL desync class the drop rule
    /// exists to kill), erase a just-opened one, and move ts_last backward.
    /// `handle_account_state` skips any snapshot below this watermark.
    account_ts_last: UnixNanos,
}

/// Cap on retained terminal order records. Open orders are never pruned (they
/// are live reconciliation truth); only closed records beyond this many are
/// dropped, oldest-by-`ts_last` first, so a report over the retained window
/// keeps every record it needs while a long forward run cannot accumulate
/// terminal orders without bound (AE6).
const MAX_TERMINAL_ORDERS: usize = 10_000;

/// Cap on the append-only `fills` Vec, pruned oldest-first past this bound.
/// Fill reports are lookback-bounded, so the oldest fills beyond this many are
/// never needed by a report within the retained window (AE6).
const MAX_FILLS: usize = 10_000;

impl ExecState {
    /// Bounds the mirror's memory: an unpruned `orders` map (terminal records
    /// and permanently-Submitted strays live forever) and an append-only `fills`
    /// Vec otherwise grow linearly over a long forward run, and every report
    /// generation scans them all. Prunes the oldest terminal orders and the
    /// oldest fills past their caps; open orders are always retained. Called
    /// after each mirror mutation that can grow the maps (a submit insert, a fill
    /// push), and does real work only when a cap is exceeded.
    fn prune(&mut self) {
        if self.fills.len() > MAX_FILLS {
            // `fills` is appended in arrival order (ascending ts on the clean
            // path), so draining the front drops the oldest.
            let excess = self.fills.len() - MAX_FILLS;
            self.fills.drain(0..excess);
        }
        // Cheap length gate before the O(n) terminal scan: the terminal count
        // cannot exceed the cap unless the whole map does, so this keeps prune
        // O(1) on the hot submit/fill paths until the mirror genuinely grows
        // large.
        if self.orders.len() <= MAX_TERMINAL_ORDERS {
            return;
        }
        let terminal = self
            .orders
            .values()
            .filter(|record| record.status.is_closed())
            .count();
        if terminal > MAX_TERMINAL_ORDERS {
            let excess = terminal - MAX_TERMINAL_ORDERS;
            let mut terminal_ids: Vec<(ClientOrderId, UnixNanos)> = self
                .orders
                .iter()
                .filter(|(_, record)| record.status.is_closed())
                .map(|(id, record)| (*id, record.ts_last))
                .collect();
            terminal_ids.sort_by_key(|(_, ts_last)| *ts_last);
            for (id, _) in terminal_ids.into_iter().take(excess) {
                self.orders.remove(&id);
            }
        }
    }
}

#[derive(Debug, Clone)]
struct OrderRecord {
    strategy_id: nautilus_model::identifiers::StrategyId,
    instrument_id: InstrumentId,
    order_side: OrderSide,
    order_type: OrderType,
    time_in_force: nautilus_model::enums::TimeInForce,
    status: OrderStatus,
    quantity: Decimal,
    price: Option<Decimal>,
    filled_qty: Decimal,
    avg_px: Option<Decimal>,
    venue_order_id: Option<VenueOrderId>,
    ts_accepted: UnixNanos,
    ts_last: UnixNanos,
    /// `trade_id`s already applied to this order's reconciliation mirror. The
    /// duplicate-fill divergence (`DuplicateNextFill`) and client-side
    /// `duplicate_prob` deliberately deliver the same `OrderFilled` twice; the
    /// duplicate wire event is forwarded downstream (the intended divergence),
    /// but it must not double-apply to the mirror, so the second sighting of a
    /// `trade_id` skips the filled_qty/avg_px/fills mutation.
    seen_trades: std::collections::HashSet<TradeId>,
}

impl OrderRecord {
    fn quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> anyhow::Result<nautilus_model::types::Quantity> {
        let precision = report_size_precision(instruments, self.instrument_id)?;
        convert::quantity(self.quantity, precision)
    }

    fn filled_quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> anyhow::Result<nautilus_model::types::Quantity> {
        let precision = report_size_precision(instruments, self.instrument_id)?;
        convert::quantity(self.filled_qty, precision)
    }
}

/// The instrument's real `size_precision`, or an error on a cache miss.
///
/// A guessed default (this used to fall back to a bare `8`) can silently
/// misrepresent a report's quantity precision against the real instrument -
/// exactly the class of hostile/unrepresentable-data problem `convert.rs`
/// otherwise takes care to surface rather than paper over. Both call sites
/// already thread the `anyhow::Result` through a `filter_map` that warns and
/// drops the one report on error, so surfacing the miss here costs nothing.
fn report_size_precision(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    instrument_id: InstrumentId,
) -> anyhow::Result<u8> {
    instrument_def(instruments, &symbol_from_instrument(instrument_id))
        .map(|def| def.size_precision)
        .with_context(|| format!("no instrument def cached for {instrument_id}"))
}

#[derive(Debug, Clone)]
struct FillRecord {
    client_order_id: ClientOrderId,
    instrument_id: InstrumentId,
    venue_order_id: VenueOrderId,
    trade_id: TradeId,
    order_side: OrderSide,
    last_qty: nautilus_model::types::Quantity,
    last_px: nautilus_model::types::Price,
    commission: Decimal,
    quote_currency: Currency,
    ts_event: UnixNanos,
}

#[derive(Debug, Clone)]
struct PositionRecord {
    symbol: Symbol,
    instrument_id: InstrumentId,
    quantity: Decimal,
    avg_px: Decimal,
    ts_last: UnixNanos,
}

fn handle_exec_message(msg: ServerMessage, ctx: &ExecContext) {
    match msg {
        ServerMessage::OrderAccepted {
            client_order_id,
            venue_order_id,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else {
                return;
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Terminal-state guard: reorder/duplicate havoc can deliver this
                // Accepted AFTER the fill or cancel that ended the order (the
                // engine emits Accepted and Filled adjacently for immediate
                // fills - exactly the pair a reorder transposes). Nautilus's own
                // order FSM has no terminal-to-Accepted arm, and this mirror is
                // the reconciliation truth source, so it must be at least as
                // strict: never regress a terminal record. The wire event is
                // still forwarded below (the intended divergence, matching the
                // duplicate-fill discipline); only the mirror mutation is
                // skipped.
                let stale = record.status.is_closed();
                if !stale {
                    record.status = OrderStatus::Accepted;
                    record.venue_order_id = Some(venue_order_id);
                    record.ts_accepted = UnixNanos::from(ts_event);
                    // Forward-only, matching the fill handler (F11): a non-terminal
                    // Accepted reordered behind an event that already advanced the
                    // record must not walk ts_last backward and perturb the
                    // in_time_range report filtering.
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                // Same A.11 limitation as the reject arms: the mirror lacks the
                // order (e.g. an event arriving after reset() cleared it), so
                // there is no real strategy_id/instrument_id to emit with. Make
                // the drop visible instead of silent.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order accepted for an order the mirror does not know; \
                     event not surfaced to nautilus (A.11)"
                );
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "accepted event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderAccepted::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                venue_order_id,
                ctx.account_id,
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
            );
            ctx.emitter.send_order_event(OrderEventAny::Accepted(event));
        }
        ServerMessage::OrderRejected {
            client_order_id,
            reason,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
                record.status = OrderStatus::Rejected;
                record.ts_last = UnixNanos::from(ts_event);
                record.clone()
            }) else {
                // The local mirror does not know this order, so we lack the
                // real strategy_id/instrument_id the emit requires. We cannot
                // synthesize them: nautilus `Order.apply` hard-validates the
                // event's strategy_id against the cached order and silently
                // drops the event on mismatch, so a placeholder would guarantee
                // the drop rather than surface the reject. Surfacing it
                // correctly (e.g. resolving the order from the nautilus cache,
                // which ExecContext does not hold) is a design change tracked
                // as bug-hunt A.11; for now make the drop visible instead of
                // silent.
                tracing::warn!(
                    order = %client_order_id,
                    reason = %reason,
                    "order rejected for an order the mirror does not know; \
                     reject not surfaced to nautilus (A.11)"
                );
                return;
            };
            let event = OrderRejected::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                ctx.account_id,
                reason.into(),
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                false,
            );
            ctx.emitter.send_order_event(OrderEventAny::Rejected(event));
        }
        ServerMessage::OrderCanceled {
            client_order_id,
            venue_order_id,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else {
                return;
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Terminal-state guard, same as the OrderAccepted arm: a
                // Canceled transposed behind the fill that actually ended the
                // order must not overwrite Filled (or a duplicate re-cancel an
                // already-terminal record). Forward the wire event, skip the
                // mirror regression.
                let stale = record.status.is_closed();
                if !stale {
                    record.status = OrderStatus::Canceled;
                    record.venue_order_id = Some(venue_order_id);
                    // Forward-only, matching the fill handler (F11).
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                // Same A.11 limitation as the reject arms: no real strategy_id/
                // instrument_id to emit with. Make the drop visible.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order canceled for an order the mirror does not know; \
                     event not surfaced to nautilus (A.11)"
                );
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "canceled event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderCanceled::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                Some(venue_order_id),
                Some(ctx.account_id),
            );
            ctx.emitter.send_order_event(OrderEventAny::Canceled(event));
        }
        ServerMessage::OrderUpdated {
            client_order_id,
            venue_order_id,
            quantity,
            price,
            leaves_qty,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(venue_order_id) = wire_venue_order_id(&venue_order_id) else {
                return;
            };
            let Some(known) = order_record(&ctx.state, client_order_id) else {
                // Same A.11 limitation as the reject arms: no real strategy_id/
                // instrument_id to emit with. Make the drop visible.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = %venue_order_id,
                    "order update for an order the mirror does not know; \
                     event not surfaced to nautilus (A.11)"
                );
                return;
            };
            // Resolve the instrument before touching the mirror so a missing def
            // does not leave the mirror amended with no matching event emitted.
            let Some(def) = instrument_def(
                &ctx.instruments,
                &symbol_from_instrument(known.instrument_id),
            ) else {
                tracing::warn!(
                    order = %client_order_id,
                    instrument = %known.instrument_id,
                    "dropping order update: no instrument def (cache not seeded?)"
                );
                return;
            };
            // Convert the amend's quantity/price before touching the mirror,
            // same as the missing-def guard above: a hostile amend value that
            // cannot represent as nautilus Price/Quantity must not leave the
            // mirror amended with no event emitted, and must not panic the
            // exec task. Drop the amend with a warning instead.
            let updated_quantity = match convert::quantity(quantity, def.size_precision) {
                Ok(quantity) => quantity,
                Err(err) => {
                    tracing::warn!(
                        order = %client_order_id,
                        error = %err,
                        "dropping order update: unrepresentable quantity"
                    );
                    return;
                }
            };
            let updated_price = match price
                .map(|p| convert::price(p, def.price_precision))
                .transpose()
            {
                Ok(price) => price,
                Err(err) => {
                    tracing::warn!(
                        order = %client_order_id,
                        error = %err,
                        "dropping order update: unrepresentable price"
                    );
                    return;
                }
            };
            let Some((record, stale)) = with_order_record(&ctx.state, client_order_id, |record| {
                // Terminal-state guard, same as the OrderAccepted arm: an amend
                // ack reordered behind the order's terminal event must not
                // recompute a non-terminal status from leaves_qty (or rewrite
                // quantity/price on a record the venue has already closed).
                // Forward the wire event, skip the mirror mutation.
                let stale = record.status.is_closed();
                if !stale {
                    record.venue_order_id = Some(venue_order_id);
                    record.quantity = quantity;
                    record.price = price;
                    // The venue's `leaves_qty` is authoritative for the remaining
                    // size after the amend. Reconcile `filled_qty` so the mirror
                    // invariant `quantity - filled_qty == leaves_qty` holds even
                    // after a downsizing amend (which the bare `quantity` overwrite
                    // used to leave stale, drifting from the venue). Clamp at zero
                    // so a `leaves_qty` exceeding the new total cannot push
                    // `filled_qty` negative.
                    record.filled_qty = (quantity - leaves_qty).max(Decimal::ZERO);
                    // An amend never reverses an in-progress fill: an order with a
                    // non-zero filled_qty stays PARTIALLY_FILLED (or flips to FILLED
                    // when the amend leaves nothing outstanding) so the mirror does
                    // not report Accepted alongside a non-zero filled_qty.
                    record.status = if record.filled_qty.is_zero() {
                        OrderStatus::Accepted
                    } else if leaves_qty.is_zero() {
                        OrderStatus::Filled
                    } else {
                        OrderStatus::PartiallyFilled
                    };
                    // Forward-only, matching the fill handler (F11): a non-terminal
                    // amend ack reordered behind a later event must not walk
                    // ts_last backward.
                    record.ts_last = record.ts_last.max(UnixNanos::from(ts_event));
                }
                (record.clone(), stale)
            }) else {
                return;
            };
            if stale {
                tracing::warn!(
                    order = %client_order_id,
                    status = ?record.status,
                    "update event for a terminal mirror record; keeping the \
                     terminal status (reordered or duplicated event)"
                );
            }
            let event = OrderUpdated::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                updated_quantity,
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                Some(venue_order_id),
                Some(ctx.account_id),
                updated_price,
                None,
                None,
                false,
            );
            ctx.emitter.send_order_event(OrderEventAny::Updated(event));
        }
        ServerMessage::OrderModifyRejected {
            client_order_id,
            venue_order_id,
            reason,
            ts_event,
        } => {
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            let Some(record) = order_record(&ctx.state, client_order_id) else {
                // Same limitation as OrderRejected (A.11): the mirror lacks the
                // order, so we have no real strategy_id/instrument_id, and a
                // placeholder would be silently dropped by nautilus
                // `Order.apply` strategy-id validation. The `venue_order_id:
                // None` case (protocol's explicit "id unknown to venue") is
                // exactly when surfacing this matters most, but doing so
                // correctly needs a non-mirror order source (a design change).
                // Make the drop visible rather than silent.
                tracing::warn!(
                    order = %client_order_id,
                    venue_order_id = ?venue_order_id,
                    reason = %reason,
                    "modify rejected for an order the mirror does not know; \
                     reject not surfaced to nautilus (A.11)"
                );
                return;
            };
            let event = OrderModifyRejected::new(
                ctx.trader_id,
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                reason.into(),
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(ctx.sim),
                false,
                // Prefer the wire id and fall back to the mirror's, matching
                // emit_cancel_rejected: the engine omits the venue id for an
                // order that has gone terminal even though the id is known, so
                // a known order's modify-reject would otherwise reach nautilus
                // with no venue id where the equivalent cancel-reject carries
                // one. The fallback also covers any future wire omission.
                venue_order_id
                    .as_deref()
                    .and_then(wire_venue_order_id)
                    .or(record.venue_order_id),
                Some(ctx.account_id),
            );
            ctx.emitter
                .send_order_event(OrderEventAny::ModifyRejected(event));
        }
        ServerMessage::OrderCancelRejected {
            client_order_id,
            venue_order_id,
            reason,
            ts_event,
        } => {
            // A venue-originated cancel rejection. emit_cancel_rejected leaves
            // the mirror's status untouched (nautilus restores the pre-cancel
            // status) and handles the unknown-order (A.11) drop, exactly as the
            // transport-failure path does - the only differences are the wire
            // timestamp and the possibly-named venue id, both passed through.
            let Some(client_order_id) = wire_client_order_id(&client_order_id) else {
                return;
            };
            emit_cancel_rejected(
                client_order_id,
                venue_order_id.as_deref().and_then(wire_venue_order_id),
                reason,
                UnixNanos::from(ts_event),
                ctx,
            );
        }
        ServerMessage::OrderFilled(fill) => handle_order_filled(&fill, ctx),
        ServerMessage::AccountState(state) => handle_account_state(state, ctx),
        ServerMessage::Heartbeat { .. } => {
            tracing::trace!("ignoring server heartbeat on execution path");
        }
        ServerMessage::ProtocolError { reason, .. } => {
            // Untargeted (no client_order_id to attribute it to, unlike
            // OrderRejected), so there is no nautilus order event to raise -
            // just make the venue-side decode failure visible in the adapter's
            // own logs.
            tracing::warn!(%reason, "venue reported a protocol error");
        }
        ServerMessage::Trade(_) | ServerMessage::Quote(_) => {}
    }
}

fn handle_order_filled(fill: &mogwai_protocol::OrderFilled, ctx: &ExecContext) {
    let Some(client_order_id) = wire_client_order_id(&fill.client_order_id) else {
        return;
    };
    let Some(def) = instrument_def(&ctx.instruments, &fill.symbol) else {
        // A missing instrument def here is a legitimate miss (the instrument
        // cache has not been seeded yet), not a divergence: dropping a real
        // fill silently strands the order in Submitted/Accepted forever, so
        // surface it. (The DropNextAccountUpdate divergence drops account
        // snapshots, not fills, so warning here does not mask it.)
        tracing::warn!(
            symbol = %fill.symbol,
            order = %client_order_id,
            trade = %fill.trade_id,
            "dropping order fill: no instrument def (cache not seeded?)"
        );
        return;
    };
    let Ok(quote_currency) = Currency::from_str(&def.quote) else {
        tracing::warn!(
            symbol = %fill.symbol,
            quote = %def.quote,
            order = %client_order_id,
            "dropping order fill: unknown quote currency"
        );
        return;
    };
    let Some(venue_order_id) = wire_venue_order_id(&fill.venue_order_id) else {
        return;
    };
    // Nautilus caps a `TradeId` at 36 non-empty ASCII chars and the panicking
    // constructors (`TradeId::new`, the `From` impls) assert past that. The
    // engine's ids are short, but this is a wire value: a server bug or havoc
    // corruption sending an over-long/empty/non-ASCII id must drop the fill
    // with a warning rather than panic the unsupervised exec task (the same
    // discipline convert.rs applies to the data-side synthetic trade ids).
    let trade_id = match TradeId::new_checked(&fill.trade_id) {
        Ok(trade_id) => trade_id,
        Err(err) => {
            tracing::warn!(
                order = %client_order_id,
                trade = %fill.trade_id,
                error = %err,
                "dropping order fill: unrepresentable trade id"
            );
            return;
        }
    };
    // Convert the wire price/qty before mutating the mirror so a hostile fill
    // value that overflows nautilus Price/Quantity drops the whole fill with a
    // warning rather than panicking the exec task or leaving the mirror
    // advanced with no event emitted.
    let last_qty = match convert::quantity(fill.last_qty, def.size_precision) {
        Ok(last_qty) => last_qty,
        Err(err) => {
            tracing::warn!(
                order = %client_order_id,
                trade = %trade_id,
                error = %err,
                "dropping order fill: unrepresentable last_qty"
            );
            return;
        }
    };
    let last_px = match convert::price(fill.last_px, def.price_precision) {
        Ok(last_px) => last_px,
        Err(err) => {
            tracing::warn!(
                order = %client_order_id,
                trade = %trade_id,
                error = %err,
                "dropping order fill: unrepresentable last_px"
            );
            return;
        }
    };
    // A repeated `trade_id` for this order is a duplicate fill: the duplicate
    // OrderFilled wire event is still forwarded below (the intended divergence),
    // but the reconciliation mirror (filled_qty/avg_px/fills) must apply each
    // economic fill exactly once, so the second sighting skips the mutation.
    let Some((record, is_duplicate)) = with_order_record(&ctx.state, client_order_id, |record| {
        let is_duplicate = !record.seen_trades.insert(trade_id);
        if !is_duplicate {
            // Terminal-state guard (see the OrderAccepted arm): a partial fill
            // transposed behind the cancel (or the final fill) that ended the
            // order must still book its economics - money moved at the venue,
            // so filled_qty/avg_px/the fill record are real - but must not
            // regress the terminal status back to PartiallyFilled and re-open
            // a closed order in the reconciliation mirror.
            if !record.status.is_closed() {
                record.status = if fill.leaves_qty.is_zero() {
                    OrderStatus::Filled
                } else {
                    OrderStatus::PartiallyFilled
                };
            }
            record.venue_order_id = Some(venue_order_id);
            let previous_notional = record
                .avg_px
                .unwrap_or(Decimal::ZERO)
                .checked_mul(record.filled_qty)
                .unwrap_or(Decimal::ZERO);
            record.filled_qty += fill.last_qty;
            let total_notional = previous_notional
                .checked_add(
                    fill.last_px
                        .checked_mul(fill.last_qty)
                        .unwrap_or(Decimal::ZERO),
                )
                .unwrap_or(previous_notional);
            if !record.filled_qty.is_zero() {
                record.avg_px = total_notional.checked_div(record.filled_qty);
            }
            // A reordered fill carries an OLDER ts_event than the event that
            // already advanced the record; only ever move ts_last forward so
            // the mirror's in_time_range filtering does not walk backward.
            record.ts_last = record.ts_last.max(UnixNanos::from(fill.ts_event));
        }
        (record.clone(), is_duplicate)
    }) else {
        // The worst silent drop on this path: money moved at the venue and no
        // nautilus event can be built (same A.11 limitation as the reject
        // arms - the mirror lacks the order, so there is no real strategy_id/
        // instrument_id to emit with). Make it loud.
        tracing::warn!(
            order = %client_order_id,
            trade = %trade_id,
            venue_order_id = %venue_order_id,
            "order fill for an order the mirror does not know; \
             fill not surfaced to nautilus (A.11)"
        );
        return;
    };

    if !is_duplicate {
        let mut state = lock_recover(&ctx.state, "execution state");
        state.fills.push(FillRecord {
            client_order_id,
            instrument_id: record.instrument_id,
            venue_order_id,
            trade_id,
            order_side: record.order_side,
            last_qty,
            last_px,
            commission: fill.commission,
            quote_currency,
            ts_event: UnixNanos::from(fill.ts_event),
        });
        state.prune();
    }

    let commission = if fill.commission.is_zero() {
        None
    } else {
        // The mirror is already advanced and the fill event must still emit; a
        // commission that cannot represent as nautilus Money degrades to no
        // reported commission (with a warning) rather than panicking or
        // dropping the whole fill.
        match convert::money(fill.commission, quote_currency) {
            Ok(money) => Some(money),
            Err(err) => {
                tracing::warn!(
                    order = %client_order_id,
                    trade = %trade_id,
                    error = %err,
                    "reporting fill without commission: unrepresentable amount"
                );
                None
            }
        }
    };
    // mogwai does not report a liquidity side on the wire. The engine fills
    // immediately against replayed history, which is taker-equivalent, so every
    // fill is reported as Taker. This is a deliberate, lossy mapping: the wire
    // carries no maker/taker flag to preserve.
    let event = OrderFilled::new(
        ctx.trader_id,
        record.strategy_id,
        record.instrument_id,
        client_order_id,
        venue_order_id,
        ctx.account_id,
        trade_id,
        record.order_side,
        record.order_type,
        last_qty,
        last_px,
        quote_currency,
        LiquiditySide::Taker,
        UUID4::new(),
        UnixNanos::from(fill.ts_event),
        now_unix_nanos(ctx.sim),
        false,
        None,
        commission,
    );
    ctx.emitter.send_order_event(OrderEventAny::Filled(event));
}

fn handle_account_state(state: mogwai_protocol::AccountState, ctx: &ExecContext) {
    let ts_event = UnixNanos::from(state.ts_event);
    let balances = state
        .balances
        .iter()
        .filter_map(|balance| {
            // Warn on a balance whose currency string nautilus cannot represent,
            // matching every other unrepresentable-amount drop in this closure
            // (AE18): a bare `.ok()?` swallowed the whole currency's balance with
            // no diagnostic, so an account snapshot silently under-reported.
            let currency = match Currency::from_str(&balance.currency) {
                Ok(currency) => currency,
                Err(err) => {
                    tracing::warn!(
                        currency = %balance.currency,
                        error = %err,
                        "dropping account balance: currency unknown to nautilus"
                    );
                    return None;
                }
            };
            // A hostile balance amount that cannot represent as nautilus Money
            // drops just this currency's balance with a warning rather than
            // panicking the exec task that books the account snapshot.
            let convert_amount = |amount, label| {
                convert::money(amount, currency)
                    .map_err(|err| {
                        tracing::warn!(
                            currency = %balance.currency,
                            field = label,
                            error = %err,
                            "dropping account balance: unrepresentable amount"
                        );
                    })
                    .ok()
            };
            let total = convert_amount(balance.total, "total")?;
            let locked = convert_amount(balance.locked, "locked")?;
            let free = convert_amount(balance.free, "free")?;
            // `AccountBalance::new` hard-asserts `locked + free == total` and
            // panics otherwise. A havoc'd/messy AccountState can carry
            // inconsistent amounts, and even a self-consistent wire decimal
            // snapshot can fail the fixed-point check after each amount is
            // independently rounded to currency precision above. Route
            // through new_checked and drop just this currency's balance
            // (matching convert_amount's own drop-with-warning discipline)
            // rather than panicking the unsupervised exec task.
            match AccountBalance::new_checked(total, locked, free) {
                Ok(balance) => Some(balance),
                Err(err) => {
                    tracing::warn!(
                        currency = %balance.currency,
                        error = %err,
                        "dropping account balance: locked + free != total"
                    );
                    None
                }
            }
        })
        .collect();

    {
        let mut mirror = lock_recover(&ctx.state, "execution state");
        // Snapshots must apply in venue order, not arrival order: the retain
        // below treats each snapshot as the newest truth, so an OLDER snapshot
        // delivered late by reorder/duplicate havoc would resurrect a closed
        // position or erase an open one, persistently (nothing corrects it
        // until the next fill-driven snapshot, which may be never). Skip any
        // snapshot below the applied watermark - and skip forwarding it to
        // nautilus too, since nautilus applies account states in arrival order
        // with no staleness guard of its own. Equal-ts duplicates pass; they
        // re-apply idempotently. The check and the apply share one lock so
        // concurrent HTTP dispatch drains cannot interleave between them.
        if ts_event < mirror.account_ts_last {
            tracing::warn!(
                ts_event = ts_event.as_u64(),
                last_applied = mirror.account_ts_last.as_u64(),
                "dropping stale account snapshot: older than the last applied one"
            );
            return;
        }
        mirror.account_ts_last = ts_event;
        // The engine reports a COMPLETE set of its non-zero positions in every
        // snapshot, and signals a flat instrument by OMITTING it: a position
        // closed to zero is removed from the engine's map, never reported as a
        // zero-qty entry. So the snapshot is authoritative - any symbol the
        // mirror still holds that is absent here has gone flat at the venue and
        // must be dropped. Without this drop the insert-only mirror keeps the
        // stale non-zero PositionRecord from the entry after the close, and
        // generate_position_status_reports then hands broadarrow a phantom
        // venue net it can only adopt as an EXTERNAL position - desyncing
        // reconciliation (slices no longer sum to net) and halting the account.
        let present: std::collections::HashSet<Symbol> =
            state.positions.iter().map(|p| p.symbol.clone()).collect();
        mirror
            .positions
            .retain(|symbol, _| present.contains(symbol));
        for position in state.positions {
            let Some(def) = instrument_def(&ctx.instruments, &position.symbol) else {
                // A legitimate missing def (instrument cache not yet seeded)
                // silently drops a real position from the account snapshot,
                // leaving the mirror inconsistent. The DropNextAccountUpdate
                // divergence drops the whole snapshot upstream, not individual
                // positions here, so warning does not mask it.
                tracing::warn!(
                    symbol = %position.symbol,
                    "dropping account position: no instrument def (cache not seeded?)"
                );
                continue;
            };
            mirror.positions.insert(
                position.symbol.clone(),
                PositionRecord {
                    symbol: position.symbol,
                    instrument_id: convert::instrument_id(&def),
                    quantity: position.quantity,
                    avg_px: position.avg_px,
                    ts_last: ts_event,
                },
            );
        }
    }
    ctx.emitter.send_account_state(NautilusAccountState::new(
        ctx.account_id,
        ctx.account_type,
        balances,
        Vec::new(),
        true,
        UUID4::new(),
        ts_event,
        now_unix_nanos(ctx.sim),
        None,
    ));
}

/// Converts a server-sent `client_order_id` string into a nautilus
/// `ClientOrderId`, dropping the event with a warning instead of panicking.
/// `ClientOrderId::from` routes through the panicking `new`, which nautilus's
/// `check_valid_string_ascii` rejects on an empty, whitespace-only, or
/// non-ASCII string (there is no length cap, unlike `TradeId`). These are wire
/// values, so a server bug or havoc corruption sending a malformed id must not
/// panic the unsupervised exec task - the same drop-and-warn discipline the
/// fill's `trade_id` already gets.
fn wire_client_order_id(raw: &str) -> Option<ClientOrderId> {
    ClientOrderId::new_checked(raw)
        .map_err(|err| {
            tracing::warn!(
                order = %raw,
                error = %err,
                "dropping exec event: unrepresentable client order id"
            );
        })
        .ok()
}

/// Converts a server-sent `venue_order_id` string into a nautilus
/// `VenueOrderId`, dropping it with a warning instead of panicking. Same
/// rationale as `wire_client_order_id`: `VenueOrderId::from` panics on empty,
/// whitespace-only, or non-ASCII wire strings.
fn wire_venue_order_id(raw: &str) -> Option<VenueOrderId> {
    VenueOrderId::new_checked(raw)
        .map_err(|err| {
            tracing::warn!(
                venue_order_id = %raw,
                error = %err,
                "dropping exec event: unrepresentable venue order id"
            );
        })
        .ok()
}

fn with_order_record<T>(
    state: &Arc<Mutex<ExecState>>,
    client_order_id: ClientOrderId,
    f: impl FnOnce(&mut OrderRecord) -> T,
) -> Option<T> {
    lock_recover(state, "execution state")
        .orders
        .get_mut(&client_order_id)
        .map(f)
}

fn order_record(
    state: &Arc<Mutex<ExecState>>,
    client_order_id: ClientOrderId,
) -> Option<OrderRecord> {
    lock_recover(state, "execution state")
        .orders
        .get(&client_order_id)
        .cloned()
}

fn position_side(quantity: Decimal) -> PositionSideSpecified {
    if quantity.is_sign_positive() && !quantity.is_zero() {
        PositionSideSpecified::Long
    } else if quantity.is_sign_negative() {
        PositionSideSpecified::Short
    } else {
        PositionSideSpecified::Flat
    }
}

fn in_time_range(ts: UnixNanos, start: Option<UnixNanos>, end: Option<UnixNanos>) -> bool {
    start.is_none_or(|start| ts >= start) && end.is_none_or(|end| ts <= end)
}

#[cfg(test)]
mod data_client_tests {
    use std::num::NonZeroUsize;

    use mogwai_protocol::{AggressorSide, MarketRegime, TransportProfile};
    use nautilus_core::{Params, UUID4};
    use nautilus_model::{
        data::BarSpecification,
        enums::{AggregationSource, BarAggregation, PriceType},
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
        match rx.try_recv().expect("the completed withheld window is flushed") {
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
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use mogwai_protocol::{Side, TransportProfile};
    use nautilus_common::{cache::Cache, clients::ExecutionClient, messages::ExecutionEvent};
    use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
    use nautilus_model::{
        enums::TimeInForce,
        identifiers::{ClientId, StrategyId, TraderId},
    };

    use super::*;

    fn execution_client() -> MogwaiExecutionClient {
        execution_client_with_config(MogwaiExecClientConfig::default())
    }

    fn execution_client_with_config(config: MogwaiExecClientConfig) -> MogwaiExecutionClient {
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            TraderId::from("MOGWAI-001"),
            ClientId::from("MOGWAI-TEST"),
            *MOGWAI_VENUE,
            config.oms_type,
            config.account_id,
            config.account_type,
            None,
            cache,
        );

        MogwaiExecutionClient::new(core, config).expect("valid execution client")
    }

    fn instrument_id() -> InstrumentId {
        InstrumentId::new(
            nautilus_model::identifiers::Symbol::from("BTCUSDT"),
            *MOGWAI_VENUE,
        )
    }

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

    fn instruments_map() -> Arc<Mutex<HashMap<Symbol, InstrumentDef>>> {
        Arc::new(Mutex::new(HashMap::from([("BTCUSDT".to_string(), def())])))
    }

    fn seed_order(state: &Arc<Mutex<ExecState>>) {
        state.lock().expect("execution state mutex").orders.insert(
            ClientOrderId::from("O-1"),
            OrderRecord {
                strategy_id: StrategyId::from("S-001"),
                instrument_id: instrument_id(),
                order_side: OrderSide::Buy,
                order_type: OrderType::Limit,
                time_in_force: TimeInForce::Gtc,
                status: OrderStatus::Submitted,
                quantity: Decimal::new(1, 0),
                price: Some(Decimal::new(10_000, 2)),
                filled_qty: Decimal::ZERO,
                avg_px: None,
                venue_order_id: None,
                ts_accepted: UnixNanos::from(1),
                ts_last: UnixNanos::from(1),
                seen_trades: std::collections::HashSet::new(),
            },
        );
    }

    fn exec_context() -> (
        ExecContext,
        tokio::sync::mpsc::UnboundedReceiver<ExecutionEvent>,
    ) {
        let (tx, rx) = unbounded_channel();
        let config = MogwaiExecClientConfig::default();
        let mut emitter = ExecutionEventEmitter::new(
            get_atomic_clock_realtime(),
            config.trader_id,
            config.account_id,
            config.account_type,
            None,
        );
        emitter.set_sender(tx);
        let state = Arc::new(Mutex::new(ExecState::default()));
        seed_order(&state);
        (
            ExecContext {
                emitter,
                state,
                instruments: instruments_map(),
                trader_id: config.trader_id,
                account_id: config.account_id,
                account_type: config.account_type,
                sim: SimClock::identity(),
            },
            rx,
        )
    }

    #[test]
    fn mogwai_exec_client_start_stop_are_idempotent() {
        let mut client = execution_client();

        assert!(client.is_stopped());
        assert!(!client.is_started());

        client.start().expect("first start succeeds");
        client.start().expect("second start succeeds");
        assert!(client.is_started());
        assert!(!client.is_stopped());

        client.stop().expect("first stop succeeds");
        client.stop().expect("second stop succeeds");
        assert!(client.is_stopped());
        assert!(!client.is_started());
        assert!(!client.is_connected());
    }

    #[test]
    fn reset_clears_exec_state_so_orders_do_not_leak_across_sessions() {
        let mut client = execution_client();
        seed_order(&client.state);
        client
            .state
            .lock()
            .expect("execution state mutex")
            .fills
            .push(FillRecord {
                client_order_id: ClientOrderId::from("O-1"),
                instrument_id: instrument_id(),
                venue_order_id: VenueOrderId::from("V-1"),
                trade_id: TradeId::from("T-1"),
                order_side: OrderSide::Buy,
                last_qty: nautilus_model::types::Quantity::new(1.0, 8),
                last_px: nautilus_model::types::Price::new(100.0, 2),
                commission: Decimal::ZERO,
                quote_currency: Currency::from_str("USDT").expect("usdt"),
                ts_event: UnixNanos::from(1),
            });

        client.reset().expect("reset succeeds");

        let state = client.state.lock().expect("execution state mutex");
        assert!(state.orders.is_empty(), "orders cleared on reset");
        assert!(state.fills.is_empty(), "fills cleared on reset");
        assert!(state.positions.is_empty(), "positions cleared on reset");
    }

    #[test]
    fn submit_modify_cancel_emit_wire_commands() {
        let submit = ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
            client_order_id: "O-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: mogwai_protocol::OrderType::Limit,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            time_in_force: mogwai_protocol::TimeInForce::Gtc,
        });

        assert!(matches!(
            exec_command_to_client_message(submit),
            ClientMessage::SubmitOrder(order)
                if order.client_order_id == "O-1"
                    && order.symbol == "BTCUSDT"
                    && order.side == Side::Buy
        ));
        assert!(matches!(
            exec_command_to_client_message(ExecWsCommand::Modify {
                client_order_id: "O-1".into(),
                price: Some(Decimal::new(12_000, 2)),
                quantity: Some(Decimal::new(2, 0)),
            }),
            ClientMessage::ModifyOrder {
                client_order_id,
                price: Some(price),
                quantity: Some(quantity),
            } if client_order_id == "O-1"
                && price == Decimal::new(12_000, 2)
                && quantity == Decimal::new(2, 0)
        ));
        assert!(matches!(
            exec_command_to_client_message(ExecWsCommand::Cancel {
                client_order_id: "O-1".into(),
            }),
            ClientMessage::CancelOrder { client_order_id } if client_order_id == "O-1"
        ));
    }

    #[tokio::test]
    async fn http_orders_dispatch_does_not_require_ws_channel() {
        let client = execution_client_with_config(MogwaiExecClientConfig {
            transport_profile: TransportProfile::HttpOrders,
            ..MogwaiExecClientConfig::default()
        });

        client
            .dispatch_order(ExecWsCommand::Cancel {
                client_order_id: "O-1".into(),
            })
            .expect("HTTP dispatch accepts command without websocket");
        assert!(client.ws_cmd.is_none());
    }

    #[test]
    fn account_state_buckets_into_exec_latency_not_data() {
        // Distinct knobs so a misbucket is observable: base 5ns, exec +20,
        // fill +30, data +40. The always-on 30ms baseline adds on top of those
        // armed values. The bug this pins had `AccountState` riding the data
        // knob; it is an account/execution event and must take exec latency.
        // Both ends key off `ServerMessage::category`, so this asserts the
        // adapter side of that shared classification.
        let havoc = ClientHavoc {
            latency: Some(HavocLatency {
                base_nanos: 5,
                exec_event_nanos: 20,
                fill_nanos: 30,
                data_nanos: 40,
            }),
            ..ClientHavoc::default()
        };
        let filter = HavocFilter::from_client(&havoc);

        let account = ServerMessage::AccountState(mogwai_protocol::AccountState {
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&account),
            Duration::from_nanos(30_000_025),
            "AccountState takes baseline + armed base + exec latency, not data"
        );

        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(1, 0),
            size: Decimal::new(1, 0),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&trade),
            Duration::from_nanos(30_000_045),
            "trades still take baseline + armed base + data latency"
        );

        let fill = ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
            client_order_id: "O-1".into(),
            venue_order_id: "V-1".into(),
            trade_id: "T-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            last_qty: Decimal::new(1, 0),
            last_px: Decimal::new(10_000, 2),
            leaves_qty: Decimal::ZERO,
            commission: Decimal::ZERO,
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&fill),
            Duration::from_nanos(30_000_035),
            "fills still take baseline + armed base + fill latency"
        );
    }

    #[test]
    fn no_armed_latency_still_carries_baseline() {
        // `ClientHavoc` with no latency covers both no-havoc and an explicit
        // client object without a latency key. It must still carry the honest
        // baseline, and armed latency must add on top rather than replace it.
        let filter = HavocFilter::from_client(&ClientHavoc::default());
        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::new(1, 0),
            size: Decimal::new(1, 0),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert_eq!(
            filter.delay_for(&trade),
            mogwai_protocol::BASELINE_LATENCY.delay_for(mogwai_protocol::EventKind::Data)
        );

        let armed = HavocFilter::from_client(&ClientHavoc {
            latency: Some(HavocLatency {
                base_nanos: 7,
                ..Default::default()
            }),
            ..ClientHavoc::default()
        });
        assert_eq!(
            armed.delay_for(&trade),
            mogwai_protocol::BASELINE_LATENCY.delay_for(mogwai_protocol::EventKind::Data)
                + Duration::from_nanos(7)
        );
    }

    #[test]
    fn draw_at_intermediate_probability_is_seeded_and_mixed() {
        // F.6: every other havoc test pins the probability to 1.0 or 0.0, so the
        // `> 0.0` short-circuit (false) or the always-true `r#gen::<f64>() < 1.0`
        // arm (true) decides every draw and the seeded RNG is never consulted.
        // A regression in the seeding (wrong seed plumbed in, `from_entropy`
        // taken instead) or the draw arithmetic (`<=` vs `<`) would slip through
        // silently. This pins a mid-range probability against a fixed seed and
        // asserts the exact fire/no-fire sequence over several draws, so it is
        // sensitive to both the seed and the comparison and is robust to nothing
        // but a real behavior change.
        //
        // The expected sequence below is the literal output of `StdRng`
        // seed_from_u64(SEED) drawing `gen::<f64> < 0.5` ten times. It is a
        // genuine mix of true and false (asserted), so a draw that ignored the
        // RNG and returned a constant could not reproduce it. The same SEED on a
        // second filter must reproduce the run bit for bit (determinism); a
        // different seed must diverge somewhere (the seed is actually consulted).
        const SEED: u64 = 0xC0FF_EE12_3456_789A;
        const PROBABILITY: f64 = 0.5;

        let havoc = ClientHavoc {
            seed: Some(SEED),
            ..ClientHavoc::default()
        };

        let mut filter = HavocFilter::from_client(&havoc);
        let actual: Vec<bool> = (0..10).map(|_| filter.draw(PROBABILITY)).collect();

        let expected = [
            true, true, true, false, true, true, false, false, true, true,
        ];
        assert_eq!(
            actual, expected,
            "seeded draw sequence drifted: seeding or draw arithmetic changed"
        );

        // Guard against the sequence degenerating to all-one-way, which would
        // make the test pass even if the draw ignored its argument or its RNG.
        assert!(
            expected.iter().any(|&b| b) && expected.iter().any(|&b| !b),
            "the pinned sequence must mix fire and no-fire to exercise the draw"
        );

        // Same seed reproduces the run exactly (determinism), so the outcome is a
        // function of the seed and not of process entropy.
        let mut twin = HavocFilter::from_client(&havoc);
        let twin_seq: Vec<bool> = (0..10).map(|_| twin.draw(PROBABILITY)).collect();
        assert_eq!(twin_seq, expected, "identical seed must replay identically");

        // A different seed must change the sequence somewhere; if it did not, the
        // seed would not be reaching the RNG at all.
        let other = ClientHavoc {
            seed: Some(SEED ^ 1),
            ..ClientHavoc::default()
        };
        let mut other_filter = HavocFilter::from_client(&other);
        let other_seq: Vec<bool> = (0..10).map(|_| other_filter.draw(PROBABILITY)).collect();
        assert_ne!(
            other_seq, expected,
            "a different seed must produce a different draw sequence"
        );
    }

    #[test]
    fn per_dispatch_seed_decorrelates_yet_stays_reproducible() {
        // F.6 follow-up: `client_havoc_for_dispatch` XORs the configured client
        // seed with the per-dispatch counter (`*seed ^= counter`) so each
        // dispatched order draws a distinct havoc stream instead of every order
        // replaying one. A regression dropping the XOR would silently collapse
        // all dispatches onto the same sequence; that bug is invisible to the
        // single-stream `draw` test above. This pins a configured seed plus a
        // mid-range probability, builds the filter the way `dispatch_order`
        // does (derive a per-dispatch ClientHavoc, then HavocFilter::from_client),
        // and asserts adjacent counters DECORRELATE while each counter stays
        // reproducible under the same configured seed.
        const SEED: u64 = 0xC0FF_EE12_3456_789A;
        const PROBABILITY: f64 = 0.5;

        let spec = Some(HavocSpec {
            client: ClientHavoc {
                seed: Some(SEED),
                drop_prob: PROBABILITY,
                ..ClientHavoc::default()
            },
            ..HavocSpec::default()
        });

        // The seam under test: a per-dispatch ClientHavoc whose seed is the
        // configured seed XORed with the dispatch counter.
        let draw_dispatch = |counter: u64| -> Vec<bool> {
            let client = client_havoc_for_dispatch(&spec, counter);
            let mut filter = HavocFilter::from_client(&client);
            (0..10).map(|_| filter.draw(PROBABILITY)).collect()
        };

        // Confirm the XOR actually moved the seed off the configured value for a
        // non-zero counter (counter 0 leaves it untouched, which is intended).
        assert_eq!(
            client_havoc_for_dispatch(&spec, 0).seed,
            Some(SEED),
            "counter 0 must leave the configured seed untouched"
        );
        assert_eq!(
            client_havoc_for_dispatch(&spec, 1).seed,
            Some(SEED ^ 1),
            "counter 1 must XOR the configured seed with the counter"
        );

        let first = draw_dispatch(0);
        let second = draw_dispatch(1);

        // Decorrelation: two successive dispatches must draw different sequences.
        // If the XOR were dropped, both would replay the configured seed and
        // these would be identical.
        assert_ne!(
            first, second,
            "successive dispatches must draw decorrelated havoc streams; \
             the per-dispatch seed XOR was dropped if these match"
        );

        // Each must mix fire and no-fire so the comparison exercises the draw
        // rather than passing on a degenerate all-one-way run.
        assert!(
            first.iter().any(|&b| b) && first.iter().any(|&b| !b),
            "dispatch 0 must mix fire and no-fire"
        );
        assert!(
            second.iter().any(|&b| b) && second.iter().any(|&b| !b),
            "dispatch 1 must mix fire and no-fire"
        );

        // Individual reproducibility: the SAME configured seed must replay each
        // per-dispatch stream bit for bit, so the decorrelation is deterministic
        // and not process entropy. Re-deriving from the same `spec` reproduces.
        assert_eq!(
            draw_dispatch(0),
            first,
            "same configured seed must replay dispatch 0 identically"
        );
        assert_eq!(
            draw_dispatch(1),
            second,
            "same configured seed must replay dispatch 1 identically"
        );
    }

    #[test]
    fn accepted_then_filled_then_account_drive_exec_events() {
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
            &ctx,
        );
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: vec![mogwai_protocol::Balance {
                    currency: "USDT".into(),
                    total: Decimal::new(10_000, 0),
                    free: Decimal::new(9_900, 0),
                    locked: Decimal::new(100, 0),
                }],
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );

        assert!(matches!(
            rx.try_recv().expect("accepted event"),
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            rx.try_recv().expect("filled event"),
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        assert!(matches!(
            rx.try_recv().expect("account event"),
            ExecutionEvent::Account(_)
        ));
    }

    #[test]
    fn http_response_events_use_same_exec_drain() {
        let (ctx, mut rx) = exec_context();
        let events = vec![
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
        ];

        for event in events {
            handle_exec_message(event, &ctx);
        }

        assert!(matches!(
            rx.try_recv().expect("accepted event"),
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        assert!(matches!(
            rx.try_recv().expect("filled event"),
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
    }

    #[test]
    fn http_post_failure_rejects_submitted_order() {
        let (ctx, mut rx) = exec_context();
        let cmd = ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
            client_order_id: "O-1".into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: mogwai_protocol::OrderType::Limit,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            time_in_force: mogwai_protocol::TimeInForce::Gtc,
        });
        let err = anyhow::anyhow!("connection refused");

        handle_exec_message(reject_for(&cmd, &err, SimClock::identity()), &ctx);

        match rx.try_recv().expect("rejected event") {
            ExecutionEvent::Order(OrderEventAny::Rejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
                assert_eq!(event.reason.as_str(), "connection refused");
            }
            other => panic!("expected rejected event, got {other:?}"),
        }
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(record.status, OrderStatus::Rejected);
    }

    #[test]
    fn http_cancel_failure_reports_cancel_rejected_without_touching_status() {
        // A cancel failing at transport must not be reported as a full order
        // rejection (bug-hunt A.3): the order never heard back from the
        // venue, so it is still whatever it was before the cancel attempt.
        let (ctx, mut rx) = exec_context();
        let err = anyhow::anyhow!("connection refused");

        emit_cancel_rejected(
            ClientOrderId::from("O-1"),
            None,
            err.to_string(),
            now_unix_nanos(ctx.sim),
            &ctx,
        );

        match rx.try_recv().expect("cancel rejected event") {
            ExecutionEvent::Order(OrderEventAny::CancelRejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
                assert_eq!(event.reason.as_str(), "connection refused");
            }
            other => panic!("expected cancel rejected event, got {other:?}"),
        }
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a failed cancel must leave the mirrored order status untouched"
        );
    }

    #[test]
    fn wire_cancel_rejected_surfaces_event_and_honors_wire_venue_id() {
        // A venue-originated ServerMessage::OrderCancelRejected (the engine
        // could not honor a cancel: target unknown or already terminal) must
        // surface as a nautilus CancelRejected, leave the mirrored order's
        // status untouched (like the transport-failure path), and carry the
        // venue id named on the wire even though the seeded record holds None.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderCancelRejected {
                client_order_id: "O-1".into(),
                venue_order_id: Some("V-1".into()),
                reason: "order already terminal (filled or canceled)".into(),
                ts_event: 42,
            },
            &ctx,
        );

        match rx.try_recv().expect("cancel rejected event") {
            ExecutionEvent::Order(OrderEventAny::CancelRejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
                assert_eq!(
                    event.reason.as_str(),
                    "order already terminal (filled or canceled)"
                );
                assert_eq!(event.venue_order_id, Some(VenueOrderId::from("V-1")));
                assert_eq!(event.ts_event, UnixNanos::from(42));
            }
            other => panic!("expected cancel rejected event, got {other:?}"),
        }
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a rejected cancel must leave the mirrored order status untouched"
        );
    }

    #[test]
    fn emitter_bypass_events_stamp_ts_init_on_sim_axis() {
        let (mut ctx, mut rx) = exec_context();
        let wall = mogwai_protocol::now_unix_nanos();
        ctx.sim = SimClock {
            sim_epoch_ns: 1_900_000_000_000_000_000,
            wall_anchor_ns: wall.saturating_sub(1),
            speed: 1.0,
        };

        handle_exec_message(
            ServerMessage::OrderRejected {
                client_order_id: "O-1".into(),
                reason: "no".into(),
                ts_event: 10,
            },
            &ctx,
        );
        match rx.try_recv().expect("rejected event") {
            ExecutionEvent::Order(OrderEventAny::Rejected(event)) => {
                assert!(event.ts_init.as_u64() >= ctx.sim.sim_epoch_ns);
            }
            other => panic!("expected rejected event, got {other:?}"),
        }

        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 12,
            }),
            &ctx,
        );
        match rx.try_recv().expect("account event") {
            ExecutionEvent::Account(event) => {
                assert!(event.ts_init.as_u64() >= ctx.sim.sim_epoch_ns);
            }
            other => panic!("expected account event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn reports_reconstruct_from_mirror() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
            &ctx,
        );
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );

        let orders = client
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                false,
                Some(instrument_id()),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("order reports");
        let fills = client
            .generate_fill_reports(GenerateFillReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(instrument_id()),
                Some(VenueOrderId::from("V-1")),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("fill reports");
        let positions = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(instrument_id()),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");

        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].order_status, OrderStatus::Filled);
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].trade_id, TradeId::from("T-1"));
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].position_side, PositionSideSpecified::Long);
    }

    #[tokio::test]
    async fn closed_position_drops_from_mirror_so_reconciliation_reports_none() {
        // Regression: the engine omits a flat instrument from its account
        // snapshot (it removes a position closed to zero rather than reporting
        // zero qty). The insert-only mirror used to keep the stale entry
        // PositionRecord, so generate_position_status_reports handed broadarrow
        // a phantom venue net it adopted as an EXTERNAL position -> attribution
        // desync -> halted account. The close snapshot must clear the mirror.
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };

        // Entry snapshot carries the open long.
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );

        // Close snapshot: the engine has dropped the now-flat position, so the
        // snapshot lists none. The mirror must follow and drop BTCUSDT too.
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 13,
            }),
            &ctx,
        );

        let positions = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(20),
                Some(instrument_id()),
                None,
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");

        assert!(
            positions.is_empty(),
            "a flattened position must not survive as a phantom reconciliation report"
        );
    }

    fn wire_fill(
        trade_id: &str,
        leaves_qty: Decimal,
        ts_event: u64,
    ) -> mogwai_protocol::OrderFilled {
        mogwai_protocol::OrderFilled {
            client_order_id: "O-1".into(),
            venue_order_id: "V-1".into(),
            trade_id: trade_id.into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            last_qty: Decimal::new(1, 0),
            last_px: Decimal::new(10_000, 2),
            leaves_qty,
            commission: Decimal::ZERO,
            ts_event,
        }
    }

    #[test]
    fn reordered_terminal_events_cannot_regress_the_mirror() {
        // Reorder havoc transposes the engine's adjacent Accepted+Filled pair
        // (immediate fills), so the mirror sees Filled first and the Accepted
        // arrives late. The old unconditional overwrite regressed the mirror
        // to Accepted FOREVER (nothing later corrects a terminal order), and
        // generate_order_status_reports(open_only) then reported a phantom
        // open order with full filled_qty. The mirror must be at least as
        // strict as nautilus's own FSM, which has no terminal-to-Accepted arm.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill("T-1", Decimal::ZERO, 11)),
            &ctx,
        );
        let _ = rx.try_recv().expect("accepted");
        let _ = rx.try_recv().expect("filled");

        // The reordered duplicate Accepted lands after the fill: the wire
        // event is still forwarded (nautilus's FSM refuses it on its own),
        // but the mirror keeps its terminal status and timestamps.
        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        assert!(matches!(
            rx.try_recv().expect("late accepted still forwarded"),
            ExecutionEvent::Order(OrderEventAny::Accepted(_))
        ));
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Filled,
            "a late Accepted must not regress a Filled mirror record"
        );
        assert_eq!(record.ts_last, UnixNanos::from(11));

        // A Canceled transposed behind the fill likewise must not overwrite.
        handle_exec_message(
            ServerMessage::OrderCanceled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 12,
            },
            &ctx,
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Filled,
            "a late Canceled must not overwrite a Filled mirror record"
        );

        // And an amend ack reordered behind the terminal event must not
        // recompute a non-terminal status from leaves_qty.
        handle_exec_message(
            ServerMessage::OrderUpdated {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                quantity: Decimal::new(2, 0),
                price: Some(Decimal::new(10_000, 2)),
                leaves_qty: Decimal::new(2, 0),
                ts_event: 9,
            },
            &ctx,
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Filled,
            "a late Updated must not regress a Filled mirror record"
        );
        assert_eq!(
            record.quantity,
            Decimal::new(1, 0),
            "a late amend must not rewrite a terminal record's quantity"
        );
    }

    #[test]
    fn late_fill_after_cancel_books_economics_without_reopening() {
        // The engine's partial-fill-then-cancel pair (e.g. IOC remainder)
        // transposed by reorder havoc: Canceled first, then the partial fill.
        // Money moved at the venue, so the fill's economics must book into
        // the mirror - but the terminal Canceled status must survive, and
        // ts_last must not walk backward to the fill's older stamp.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        handle_exec_message(
            ServerMessage::OrderCanceled {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 12,
            },
            &ctx,
        );
        let _ = rx.try_recv().expect("accepted");
        let _ = rx.try_recv().expect("canceled");

        // The partial fill (leaves > 0) arrives late.
        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill("T-1", Decimal::new(1, 0), 11)),
            &ctx,
        );
        assert!(matches!(
            rx.try_recv().expect("late fill still forwarded"),
            ExecutionEvent::Order(OrderEventAny::Filled(_))
        ));
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Canceled,
            "a late partial fill must not re-open a Canceled mirror record"
        );
        assert_eq!(
            record.filled_qty,
            Decimal::new(1, 0),
            "the late fill's economics must still book"
        );
        assert_eq!(
            record.ts_last,
            UnixNanos::from(12),
            "ts_last must not walk backward to the reordered fill's stamp"
        );
        let state = ctx.state.lock().expect("execution state mutex");
        assert_eq!(state.fills.len(), 1, "the fill record must still be kept");
    }

    #[test]
    fn stale_account_snapshot_is_dropped_entirely() {
        // Two adjacent AccountStates transposed by reorder havoc: the close
        // snapshot (newer, position gone) arrives first, then the entry
        // snapshot (older, position open). Applying the older one in arrival
        // order resurrected the closed position - the phantom-EXTERNAL class -
        // and moved the mirror's watermark backward. The stale snapshot must
        // be skipped wholesale: no mirror mutation, no account event forwarded
        // (nautilus has no staleness guard of its own).
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: Vec::new(),
                ts_event: 13,
            }),
            &ctx,
        );
        assert!(matches!(
            rx.try_recv().expect("fresh snapshot forwards"),
            ExecutionEvent::Account(_)
        ));

        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );
        assert!(
            rx.try_recv().is_err(),
            "a stale snapshot must not forward an account event"
        );
        let state = ctx.state.lock().expect("execution state mutex");
        assert!(
            state.positions.is_empty(),
            "a stale snapshot must not resurrect a closed position"
        );
        assert_eq!(
            state.account_ts_last,
            UnixNanos::from(13),
            "the applied watermark must not move backward"
        );
    }

    #[test]
    fn modify_reject_falls_back_to_mirror_venue_id() {
        // The engine omits the venue id on a reject for an order that has
        // gone terminal even though the id is known. emit_cancel_rejected
        // already falls back to the mirror's id; the modify path must match,
        // so a known order's modify-reject carries the id the adapter holds.
        let (ctx, mut rx) = exec_context();

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        let _ = rx.try_recv().expect("accepted");

        handle_exec_message(
            ServerMessage::OrderModifyRejected {
                client_order_id: "O-1".into(),
                venue_order_id: None,
                reason: "order already terminal (filled or canceled)".into(),
                ts_event: 42,
            },
            &ctx,
        );
        match rx.try_recv().expect("modify rejected event") {
            ExecutionEvent::Order(OrderEventAny::ModifyRejected(event)) => {
                assert_eq!(
                    event.venue_order_id,
                    Some(VenueOrderId::from("V-1")),
                    "a wire None must fall back to the mirror's venue id"
                );
            }
            other => panic!("expected modify rejected event, got {other:?}"),
        }
    }

    #[test]
    fn hostile_fill_trade_id_drops_fill_without_panicking() {
        // Nautilus caps TradeId at 36 non-empty ASCII chars and the panicking
        // From impl asserts past that. A server-sent or havoc-corrupted id
        // must drop the fill with a warning instead of panicking the
        // unsupervised exec task; the mirror stays untouched.
        let (ctx, mut rx) = exec_context();
        let long_id = "T-".repeat(30);

        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill(&long_id, Decimal::ZERO, 11)),
            &ctx,
        );

        assert!(
            rx.try_recv().is_err(),
            "an unrepresentable trade id must not emit a fill event"
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.filled_qty,
            Decimal::ZERO,
            "mirror must stay unadvanced"
        );
        assert_eq!(record.status, OrderStatus::Submitted);
        let state = ctx.state.lock().expect("execution state mutex");
        assert!(state.fills.is_empty(), "no fill record for a dropped fill");
    }

    #[test]
    fn hostile_wire_order_ids_drop_exec_events_without_panicking() {
        // F7: VenueOrderId::from / ClientOrderId::from panic on empty,
        // whitespace-only, or non-ASCII wire strings (no length cap, unlike
        // TradeId). A server bug or havoc corruption sending such an id must drop
        // the event with a warning, not panic the unsupervised exec task, and
        // must leave the mirror untouched.
        let (ctx, mut rx) = exec_context();

        // Empty venue id on an Accepted: the panicking From would abort here.
        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: String::new(),
                ts_event: 10,
            },
            &ctx,
        );
        assert!(
            rx.try_recv().is_err(),
            "an empty venue id must not emit an accepted event"
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a dropped accepted must leave the mirror untouched"
        );

        // Empty client id on a fill: guarded at the very top of the fill drain.
        handle_exec_message(
            ServerMessage::OrderFilled(mogwai_protocol::OrderFilled {
                client_order_id: String::new(),
                venue_order_id: "V-1".into(),
                trade_id: "T-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                last_qty: Decimal::new(1, 0),
                last_px: Decimal::new(10_000, 2),
                leaves_qty: Decimal::ZERO,
                commission: Decimal::ZERO,
                ts_event: 11,
            }),
            &ctx,
        );
        assert!(
            rx.try_recv().is_err(),
            "an empty client id must not emit a fill event"
        );
        let record = order_record(&ctx.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.filled_qty,
            Decimal::ZERO,
            "a dropped fill must leave the mirror unadvanced"
        );
    }

    #[tokio::test]
    async fn generate_mass_status_composes_the_three_report_sets() {
        // The trait default returns Ok(None), which the live node's startup
        // reconciliation logs as "no mass status available" and reconciles
        // NOTHING. The implementation must compose the three report
        // generators: open orders, their fills, current positions.
        let mut client = execution_client();
        client.instruments = instruments_map();
        seed_order(&client.state);
        let (ctx, _rx) = exec_context();
        let ctx = ExecContext {
            emitter: ctx.emitter,
            state: Arc::clone(&client.state),
            instruments: Arc::clone(&client.instruments),
            trader_id: ctx.trader_id,
            account_id: ctx.account_id,
            account_type: ctx.account_type,
            sim: ctx.sim,
        };

        handle_exec_message(
            ServerMessage::OrderAccepted {
                client_order_id: "O-1".into(),
                venue_order_id: "V-1".into(),
                ts_event: 10,
            },
            &ctx,
        );
        // A partial fill keeps the order open, so it passes the open_only
        // filter the canonical mass-status shape applies to order reports.
        handle_exec_message(
            ServerMessage::OrderFilled(wire_fill("T-1", Decimal::new(1, 0), 11)),
            &ctx,
        );
        handle_exec_message(
            ServerMessage::AccountState(mogwai_protocol::AccountState {
                balances: Vec::new(),
                positions: vec![mogwai_protocol::Position {
                    symbol: "BTCUSDT".into(),
                    quantity: Decimal::new(1, 0),
                    avg_px: Decimal::new(10_000, 2),
                }],
                ts_event: 12,
            }),
            &ctx,
        );

        let mass = client
            .generate_mass_status(None)
            .await
            .expect("mass status generates")
            .expect("mass status is Some, not the trait's None default");

        assert_eq!(mass.client_id, client.core.client_id);
        assert_eq!(mass.account_id, client.core.account_id);
        assert_eq!(mass.venue, *MOGWAI_VENUE);
        let orders = mass.order_reports();
        assert_eq!(orders.len(), 1, "the open order must report");
        let order = orders
            .get(&VenueOrderId::from("V-1"))
            .expect("keyed by venue order id");
        assert_eq!(order.order_status, OrderStatus::PartiallyFilled);
        let fills = mass.fill_reports();
        assert_eq!(
            fills.get(&VenueOrderId::from("V-1")).map(Vec::len),
            Some(1),
            "the order's fill must report under its venue id"
        );
        assert_eq!(mass.position_reports().len(), 1, "the position must report");

        // The lookback bounds the FILL reports (still time-filtered by ts_event),
        // but open orders and open positions now report regardless of
        // last-activity time (AE10): a real venue mass-status returns every
        // resting order/position, so a long-quiet open item stamped near the
        // epoch must NOT vanish under a short lookback (which reconciliation
        // would otherwise read as canceled/closed at venue). Only closed and
        // historical records honor the lookback.
        let bounded = client
            .generate_mass_status(Some(1))
            .await
            .expect("bounded mass status generates")
            .expect("bounded mass status is Some");
        assert_eq!(
            bounded.order_reports().len(),
            1,
            "an open order still reports under a short lookback (AE10)"
        );
        assert_eq!(
            bounded.position_reports().len(),
            1,
            "an open position still reports under a short lookback (AE10)"
        );
        assert!(
            bounded.fill_reports().is_empty(),
            "fills remain time-filtered by the lookback"
        );
    }

    #[tokio::test]
    async fn http_post_failure_reject_bypasses_client_drop_havoc() {
        // A synthesized POST-failure reject models a purely local transport
        // failure - it never traveled the wire - so it must NOT pass through
        // the per-dispatch HavocFilter: with drop_prob = 1.0 the filter would
        // discard the terminal event and wedge the order in Submitted forever
        // (the cancel branch already bypasses for the same reason).
        let mut client = execution_client_with_config(MogwaiExecClientConfig {
            transport_profile: TransportProfile::HttpOrders,
            // Loopback port 1: nothing listens there, so the POST fails fast
            // with a transport error instead of waiting out a timeout.
            base_url: "ws://127.0.0.1:1".to_string(),
            havoc: Some(HavocSpec {
                client: ClientHavoc {
                    drop_prob: 1.0,
                    ..ClientHavoc::default()
                },
                ..HavocSpec::default()
            }),
            ..MogwaiExecClientConfig::default()
        });
        let (tx, mut rx) = unbounded_channel();
        client.emitter.set_sender(tx);
        seed_order(&client.state);

        client
            .dispatch_order(ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
                client_order_id: "O-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: mogwai_protocol::OrderType::Limit,
                quantity: Decimal::new(1, 0),
                price: Some(Decimal::new(10_000, 2)),
                time_in_force: mogwai_protocol::TimeInForce::Gtc,
            }))
            .expect("HTTP dispatch spawns");

        let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("the synthesized reject must arrive despite drop_prob = 1.0")
            .expect("event channel stays open");
        assert!(matches!(
            event,
            ExecutionEvent::Order(OrderEventAny::Rejected(_))
        ));
        let record = order_record(&client.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(record.status, OrderStatus::Rejected);
    }

    fn order_at(status: OrderStatus, ts: UnixNanos) -> OrderRecord {
        OrderRecord {
            strategy_id: StrategyId::from("S-001"),
            instrument_id: instrument_id(),
            order_side: OrderSide::Buy,
            order_type: OrderType::Limit,
            time_in_force: TimeInForce::Gtc,
            status,
            quantity: Decimal::new(1, 0),
            price: Some(Decimal::new(10_000, 2)),
            filled_qty: Decimal::ZERO,
            avg_px: None,
            venue_order_id: Some(VenueOrderId::from("V-1")),
            ts_accepted: ts,
            ts_last: ts,
            seen_trades: std::collections::HashSet::new(),
        }
    }

    // AE9: on the WS path, a cancel whose send fails (channel gone: reconnect
    // exhausted or the client stopped) must synthesize a CancelRejected so the
    // order does not sit forever in PendingCancel with no reject to restore it -
    // matching the HTTP path. The mirrored status is left untouched.
    #[test]
    fn ws_cancel_send_failure_synthesizes_cancel_rejected() {
        let mut client = execution_client(); // default = WsStreaming
        let (tx, mut rx) = unbounded_channel();
        client.emitter.set_sender(tx);
        seed_order(&client.state);
        assert!(client.ws_cmd.is_none(), "no live WS channel");

        client
            .dispatch_order(ExecWsCommand::Cancel {
                client_order_id: "O-1".into(),
            })
            .expect("dispatch returns Ok after synthesizing the reject");

        match rx.try_recv().expect("cancel rejected event") {
            ExecutionEvent::Order(OrderEventAny::CancelRejected(event)) => {
                assert_eq!(event.client_order_id, ClientOrderId::from("O-1"));
            }
            other => panic!("expected cancel rejected, got {other:?}"),
        }
        let record = order_record(&client.state, ClientOrderId::from("O-1")).expect("order record");
        assert_eq!(
            record.status,
            OrderStatus::Submitted,
            "a failed WS cancel must leave the mirrored status untouched"
        );
    }

    // AE9: a failed WS modify synthesizes a ModifyRejected (status untouched),
    // and a failed WS submit synthesizes an OrderRejected (Submitted -> Rejected).
    #[test]
    fn ws_modify_and_submit_send_failures_synthesize_rejects() {
        let mut client = execution_client();
        let (tx, mut rx) = unbounded_channel();
        client.emitter.set_sender(tx);
        seed_order(&client.state);

        client
            .dispatch_order(ExecWsCommand::Modify {
                client_order_id: "O-1".into(),
                price: Some(Decimal::new(12_000, 2)),
                quantity: None,
            })
            .expect("modify dispatch returns Ok after synthesizing the reject");
        assert!(matches!(
            rx.try_recv().expect("modify rejected event"),
            ExecutionEvent::Order(OrderEventAny::ModifyRejected(_))
        ));
        assert_eq!(
            order_record(&client.state, ClientOrderId::from("O-1"))
                .expect("order record")
                .status,
            OrderStatus::Submitted,
            "a failed modify leaves the order live"
        );

        client
            .dispatch_order(ExecWsCommand::Submit(mogwai_protocol::SubmitOrder {
                client_order_id: "O-1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: mogwai_protocol::OrderType::Limit,
                quantity: Decimal::new(1, 0),
                price: Some(Decimal::new(10_000, 2)),
                time_in_force: mogwai_protocol::TimeInForce::Gtc,
            }))
            .expect("submit dispatch returns Ok after synthesizing the reject");
        assert!(matches!(
            rx.try_recv().expect("rejected event"),
            ExecutionEvent::Order(OrderEventAny::Rejected(_))
        ));
        assert_eq!(
            order_record(&client.state, ClientOrderId::from("O-1"))
                .expect("order record")
                .status,
            OrderStatus::Rejected,
            "a failed submit reaches a terminal Rejected state"
        );
    }

    // AE10: an open order under open_only reports regardless of last-activity
    // time (a resting order older than the reconciliation lookback must not be
    // hidden and then inferred canceled-at-venue); the time filter still applies
    // to closed records and whenever open_only is false.
    #[tokio::test]
    async fn open_only_keeps_long_quiet_open_order_but_time_filters_the_rest() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        {
            let mut state = client.state.lock().expect("state");
            state
                .orders
                .insert(ClientOrderId::from("O-OPEN"), order_at(OrderStatus::Accepted, UnixNanos::from(1)));
            state
                .orders
                .insert(ClientOrderId::from("O-CLOSED"), order_at(OrderStatus::Canceled, UnixNanos::from(1)));
        }

        let start = Some(UnixNanos::from(1_000_000));
        let open = client
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                UnixNanos::from(2_000_000),
                true,
                None,
                start,
                None,
                None,
                None,
            ))
            .await
            .expect("open-only reports");
        assert_eq!(
            open.len(),
            1,
            "the long-quiet open order must still report under open_only (AE10)"
        );
        assert_eq!(open[0].order_status, OrderStatus::Accepted);

        let all = client
            .generate_order_status_reports(&GenerateOrderStatusReports::new(
                UUID4::new(),
                UnixNanos::from(2_000_000),
                false,
                None,
                start,
                None,
                None,
                None,
            ))
            .await
            .expect("all reports");
        assert!(
            all.is_empty(),
            "with open_only false the lookback still filters long-quiet records"
        );
    }

    // AE10 (positions): every mirrored position is a current open venue position
    // (flat ones are dropped on snapshot apply), so a lookback-bounded start must
    // not hide a long-quiet resting position.
    #[tokio::test]
    async fn position_report_keeps_long_quiet_open_position() {
        let mut client = execution_client();
        client.instruments = instruments_map();
        client.state.lock().expect("state").positions.insert(
            "BTCUSDT".to_string(),
            PositionRecord {
                symbol: "BTCUSDT".to_string(),
                instrument_id: instrument_id(),
                quantity: Decimal::new(1, 0),
                avg_px: Decimal::new(10_000, 2),
                ts_last: UnixNanos::from(1),
            },
        );

        let reports = client
            .generate_position_status_reports(&GeneratePositionStatusReports::new(
                UUID4::new(),
                UnixNanos::from(2_000_000),
                None,
                Some(UnixNanos::from(1_000_000)),
                None,
                None,
                None,
            ))
            .await
            .expect("position reports");
        assert_eq!(
            reports.len(),
            1,
            "a long-quiet open position must still report (AE10)"
        );
    }

    // AE6: ExecState.prune bounds the append-only fills Vec and terminal-order
    // records past their caps (oldest-first) while never dropping an open order.
    #[test]
    fn exec_state_prune_bounds_fills_and_terminal_orders_keeping_open() {
        let mut state = ExecState::default();
        for i in 0..(MAX_FILLS as u64 + 3) {
            state.fills.push(FillRecord {
                client_order_id: ClientOrderId::from("O-1"),
                instrument_id: instrument_id(),
                venue_order_id: VenueOrderId::from("V-1"),
                trade_id: TradeId::from("T-1"),
                order_side: OrderSide::Buy,
                last_qty: nautilus_model::types::Quantity::new(1.0, 8),
                last_px: nautilus_model::types::Price::new(100.0, 2),
                commission: Decimal::ZERO,
                quote_currency: Currency::from_str("USDT").expect("usdt"),
                ts_event: UnixNanos::from(i),
            });
        }
        for i in 0..(MAX_TERMINAL_ORDERS as u64 + 2) {
            state.orders.insert(
                ClientOrderId::from(format!("C-{i}").as_str()),
                order_at(OrderStatus::Canceled, UnixNanos::from(i)),
            );
        }
        state
            .orders
            .insert(ClientOrderId::from("OPEN"), order_at(OrderStatus::Accepted, UnixNanos::from(5)));

        state.prune();

        assert_eq!(state.fills.len(), MAX_FILLS, "fills capped at MAX_FILLS");
        assert_eq!(
            state.fills.first().map(|fill| fill.ts_event),
            Some(UnixNanos::from(3)),
            "the oldest 3 fills were dropped"
        );
        let terminal = state
            .orders
            .values()
            .filter(|record| record.status.is_closed())
            .count();
        assert_eq!(
            terminal, MAX_TERMINAL_ORDERS,
            "terminal orders capped at MAX_TERMINAL_ORDERS"
        );
        assert!(
            state.orders.contains_key(&ClientOrderId::from("OPEN")),
            "an open order is never pruned"
        );
    }
}
