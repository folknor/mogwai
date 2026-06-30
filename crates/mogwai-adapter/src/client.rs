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
    enums::{LiquiditySide, OmsType, OrderSide, OrderStatus, OrderType, PositionSideSpecified},
    events::{
        AccountState as NautilusAccountState, OrderAccepted, OrderCanceled, OrderEventAny,
        OrderFilled, OrderModifyRejected, OrderRejected, OrderSubmitted, OrderUpdated,
    },
    identifiers::{AccountId, ClientId, ClientOrderId, InstrumentId, TradeId, Venue, VenueOrderId},
    orders::Order,
    reports::{FillReport, OrderStatusReport, PositionStatusReport},
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
const ACCOUNT_REGISTRATION_TIMEOUT: Duration = Duration::from_secs(5);
const ACCOUNT_REGISTRATION_POLL: Duration = Duration::from_millis(10);

#[derive(Debug)]
#[allow(dead_code)]
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
    task_handles: Vec<JoinHandle<()>>,
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
            task_handles: Vec::new(),
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
            match (state.start_ts, start_ts) {
                (Some(existing), Some(new)) => state.start_ts = Some(existing.min(new)),
                (None, Some(new)) => state.start_ts = Some(new),
                _ => {}
            }
            (emit, state.start_ts)
        };

        if emit && !self.config.transport_profile.data_by_polling() {
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

        if emit && !self.config.transport_profile.data_by_polling() {
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
        for handle in &self.task_handles {
            handle.abort();
        }
        self.task_handles.clear();
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
            self.task_handles.push(poll_handle);
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

        self.task_handles.push(reader_handle);
        // A timed-out connect must not orphan the reader task: it is already in
        // task_handles and would keep looping/reconnecting on the shared
        // `connected` flag, so a retry would spawn a second reader racing the
        // first. Abort the task and clear the stale handle and ws_cmd before
        // propagating, leaving the client cleanly disconnected for retry.
        if let Err(err) = wait_connected(&self.connected, &ws_url).await {
            if let Some(handle) = self.task_handles.pop() {
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
        drop(get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos(sim);
                for def in defs {
                    if let Ok(instrument) = convert::instrument_any(&def, ts_init) {
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
        drop(get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos(sim);
                for def in defs {
                    if def.symbol == symbol
                        && let Ok(instrument) = convert::instrument_any(&def, ts_init)
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
        {
            let mut bars = self
                .bars
                .lock()
                .map_err(|_| anyhow::anyhow!("bar mutex poisoned"))?;
            if let Some(state) = bars.get_mut(&cmd.bar_type) {
                state.refs = state.refs.saturating_sub(1);
                if state.refs == 0 {
                    bars.remove(&cmd.bar_type);
                }
            }
        }
        self.unsubscribe_symbol(symbol, SubKind::Bars)
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
        drop(get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            let def = match ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
            {
                Ok(def) => def,
                Err(err) => {
                    tracing::error!(%symbol, error = %err, "request_trades: instrument lookup failed");
                    return;
                }
            };
            let trades = match fetch_trades(
                &http,
                &http_quota,
                &base,
                TradeFetch {
                    symbol: &symbol,
                    start,
                    end,
                    limit: request.limit,
                    regime,
                },
            )
            .await
            {
                Ok(trades) => trades,
                Err(err) => {
                    // Surface the failure instead of the old silent `if let Ok`
                    // drop: a server 422 (off-tape) or any fetch error must be
                    // visible, not mistaken for "no trades in the window".
                    tracing::error!(%symbol, error = %err, "request_trades: trade fetch failed; the server may have refused an off-tape window");
                    return;
                }
            };
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
        drop(get_runtime().spawn(async move {
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
        drop(get_runtime().spawn(async move {
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
            let trades = match fetch_trades(
                &http,
                &http_quota,
                &base,
                TradeFetch {
                    symbol: &symbol,
                    start,
                    end,
                    limit: request.limit,
                    regime,
                },
            )
            .await
            {
                Ok(trades) => trades,
                Err(err) => {
                    tracing::error!(%symbol, error = %err, "request_bars: trade fetch failed; the server may have refused an off-tape window");
                    return;
                }
            };
            let bars = aggregate_bars(&request.bar_type, &trades, &def, sim);
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
        drop(get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &http_quota, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos(sim);
                let data = defs
                    .iter()
                    .filter_map(|def| convert::instrument_any(def, ts_init).ok())
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
        drop(get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            if let Ok(def) =
                ensure_instrument(&http, &http_quota, &base, &instruments, &symbol).await
            {
                let ts_init = now_unix_nanos(sim);
                if let Ok(data) = convert::instrument_any(&def, ts_init) {
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
    while ctx.connected.load(Ordering::Relaxed) {
        let symbols = poll_symbols(&ctx.subs);
        for (symbol, start_ts) in symbols {
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
            let Ok(batch) = fetch_trades(
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
            else {
                continue;
            };
            let trades = {
                let mut cursors = lock_recover(&ctx.cursor, "poll cursor");
                let entry = cursors
                    .entry(symbol.clone())
                    .or_insert_with(|| PollCursor::new(poll_anchor));
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
) -> Vec<Bar> {
    let mut state = BarSubState::default();
    let mut out = Vec::new();
    for trade in trades {
        if let Some(bar) = update_bar_state(*bar_type, &mut state, trade, def, sim) {
            out.push(bar);
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
        if let Ok(instrument) = convert::instrument_any(&def, ts_init) {
            drop(sink.send(DataEvent::Instrument(instrument)));
        }
    }
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

async fn fetch_clock_or_identity(http: &HttpClient, http_base: &str) -> ServerClock {
    match fetch_clock(http, http_base).await {
        Ok(clock) => clock,
        Err(err) => {
            tracing::warn!(%err, "falling back to identity mogwai clock");
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
#[allow(dead_code)]
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
    task_handles: Vec<JoinHandle<()>>,
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
            task_handles: Vec::new(),
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
            drop(get_runtime().spawn(async move {
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
                    Err(err) => {
                        dispatch_havoc(
                            &mut filter,
                            reject_for(&cmd, &err, ctx.sim),
                            ctx.sim,
                            |msg| async {
                                handle_exec_message(msg, &ctx);
                            },
                        )
                        .await;
                        flush_havoc(&mut filter, ctx.sim, |msg| async {
                            handle_exec_message(msg, &ctx);
                        })
                        .await;
                    }
                }
            }));
            Ok(())
        } else {
            self.send_ws(cmd)
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
        for handle in &self.task_handles {
            handle.abort();
        }
        self.task_handles.clear();
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

        self.task_handles.push(reader_handle);
        // See MogwaiDataClient::connect: a timed-out connect must abort the
        // just-spawned reader and clear the stale handle/ws_cmd so a retry does
        // not orphan the first task racing on the shared `connected` flag.
        if let Err(err) = wait_connected(&self.connected, &ws_url).await {
            if let Some(handle) = self.task_handles.pop() {
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
        self.emitter
            .send_order_event(OrderEventAny::Submitted(submitted));

        let wire = mogwai_protocol::SubmitOrder {
            client_order_id: cmd.client_order_id.to_string(),
            symbol: symbol_from_instrument(cmd.instrument_id),
            side: convert::wire_side(cmd.order_init.order_side)?,
            order_type: convert::wire_order_type(cmd.order_init.order_type)?,
            quantity: cmd.order_init.quantity.as_decimal(),
            price: cmd.order_init.price.map(|p| p.as_decimal()),
            time_in_force: convert::wire_time_in_force(cmd.order_init.time_in_force)?,
        };

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
        drop(state);

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
            .filter(|(_, record)| in_time_range(record.ts_last, cmd.start, cmd.end))
            .filter_map(|(client_order_id, record)| {
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
                    record
                        .venue_order_id
                        .unwrap_or_else(|| VenueOrderId::from("")),
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
            .filter(|position| in_time_range(position.ts_last, cmd.start, cmd.end))
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

fn reject_for(cmd: &ExecWsCommand, err: &anyhow::Error, sim: SimClock) -> ServerMessage {
    let reason = err.to_string();
    let ts_event = now_unix_nanos(sim).as_u64();
    match cmd {
        ExecWsCommand::Submit(order) => ServerMessage::OrderRejected {
            client_order_id: order.client_order_id.clone(),
            reason,
            ts_event,
        },
        ExecWsCommand::Cancel { client_order_id } => ServerMessage::OrderRejected {
            client_order_id: client_order_id.clone(),
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
    }
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
        let precision = instrument_def(instruments, &symbol_from_instrument(self.instrument_id))
            .map_or(8, |def| def.size_precision);
        convert::quantity(self.quantity, precision)
    }

    fn filled_quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> anyhow::Result<nautilus_model::types::Quantity> {
        let precision = instrument_def(instruments, &symbol_from_instrument(self.instrument_id))
            .map_or(8, |def| def.size_precision);
        convert::quantity(self.filled_qty, precision)
    }
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
            let client_order_id = ClientOrderId::from(client_order_id);
            let venue_order_id = VenueOrderId::from(venue_order_id);
            let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
                record.status = OrderStatus::Accepted;
                record.venue_order_id = Some(venue_order_id);
                record.ts_accepted = UnixNanos::from(ts_event);
                record.ts_last = UnixNanos::from(ts_event);
                record.clone()
            }) else {
                return;
            };
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
            let client_order_id = ClientOrderId::from(client_order_id);
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
            let client_order_id = ClientOrderId::from(client_order_id);
            let venue_order_id = VenueOrderId::from(venue_order_id);
            let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
                record.status = OrderStatus::Canceled;
                record.venue_order_id = Some(venue_order_id);
                record.ts_last = UnixNanos::from(ts_event);
                record.clone()
            }) else {
                return;
            };
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
            let client_order_id = ClientOrderId::from(client_order_id);
            let venue_order_id = VenueOrderId::from(venue_order_id);
            // Resolve the instrument before touching the mirror so a missing def
            // does not leave the mirror amended with no matching event emitted.
            let Some(def) = order_record(&ctx.state, client_order_id).and_then(|record| {
                instrument_def(
                    &ctx.instruments,
                    &symbol_from_instrument(record.instrument_id),
                )
            }) else {
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
            let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
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
                record.ts_last = UnixNanos::from(ts_event);
                record.clone()
            }) else {
                return;
            };
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
            let client_order_id = ClientOrderId::from(client_order_id);
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
                venue_order_id.map(VenueOrderId::from),
                Some(ctx.account_id),
            );
            ctx.emitter
                .send_order_event(OrderEventAny::ModifyRejected(event));
        }
        ServerMessage::OrderFilled(fill) => handle_order_filled(fill, ctx),
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

fn handle_order_filled(fill: mogwai_protocol::OrderFilled, ctx: &ExecContext) {
    let client_order_id = ClientOrderId::from(fill.client_order_id);
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
    let venue_order_id = VenueOrderId::from(fill.venue_order_id);
    let trade_id = TradeId::from(fill.trade_id);
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
            record.status = if fill.leaves_qty.is_zero() {
                OrderStatus::Filled
            } else {
                OrderStatus::PartiallyFilled
            };
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
            record.ts_last = UnixNanos::from(fill.ts_event);
        }
        (record.clone(), is_duplicate)
    }) else {
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
            let currency = Currency::from_str(&balance.currency).ok()?;
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
            Some(AccountBalance::new(
                convert_amount(balance.total, "total")?,
                convert_amount(balance.locked, "locked")?,
                convert_amount(balance.free, "free")?,
            ))
        })
        .collect();

    {
        let mut mirror = lock_recover(&ctx.state, "execution state");
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

        let bars = aggregate_bars(&bar_type, &trades, &def, SimClock::identity());

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
}
