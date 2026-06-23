use std::{
    collections::HashMap,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{ClientMessage, InstrumentDef, ServerMessage, Side, Symbol};
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
    events::{OrderAccepted, OrderCanceled, OrderEventAny, OrderFilled, OrderUpdated},
    identifiers::{AccountId, ClientId, ClientOrderId, InstrumentId, TradeId, Venue, VenueOrderId},
    reports::{FillReport, OrderStatusReport, PositionStatusReport},
    types::{AccountBalance, MarginBalance, Money, currency::Currency},
};
use nautilus_network::http::HttpClient;
use rust_decimal::{Decimal, prelude::ToPrimitive};
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::{MOGWAI_VENUE, MogwaiDataClientConfig, MogwaiExecClientConfig, convert};

const HISTORY_LIMIT_CAP: usize = 10_000;

#[derive(Debug)]
#[allow(dead_code)]
pub struct MogwaiDataClient {
    client_id: ClientId,
    config: MogwaiDataClientConfig,
    connected: Arc<AtomicBool>,
    sink: Option<UnboundedSender<DataEvent>>,
    http: HttpClient,
    ws_cmd: Option<UnboundedSender<WsCommand>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: Arc<Mutex<HashMap<BarType, BarSubState>>>,
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
        let http = HttpClient::new(HashMap::new(), Vec::new(), Vec::new(), None, Some(30), None)
            .context("create HTTP client")?;
        Ok(Self {
            client_id,
            config,
            connected: Arc::new(AtomicBool::new(false)),
            sink: None,
            http,
            ws_cmd: None,
            instruments: Arc::new(Mutex::new(HashMap::new())),
            subs: Arc::new(Mutex::new(HashMap::new())),
            bars: Arc::new(Mutex::new(HashMap::new())),
            task_handles: Vec::new(),
        })
    }

    fn subscribe_symbol(
        &mut self,
        symbol: Symbol,
        kind: SubKind,
        start_ts: Option<u64>,
    ) -> anyhow::Result<()> {
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

        if emit {
            self.send_ws(WsCommand::Subscribe {
                symbols: vec![symbol],
                start_ts: active_start_ts,
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
        seed_instruments(&self.http, &http_base_url, &self.instruments).await?;

        let ws_url = join_url(&self.config.ws_url(), "ws");
        let (ws, _) = connect_async(&ws_url)
            .await
            .with_context(|| format!("connect websocket {ws_url}"))?;
        let (mut writer, mut reader) = ws.split();
        let (cmd_tx, mut cmd_rx) = unbounded_channel::<WsCommand>();
        self.ws_cmd = Some(cmd_tx);

        let connected = Arc::clone(&self.connected);
        let writer_connected = Arc::clone(&self.connected);
        let writer_handle = tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                let msg = ws_command_to_client_message(cmd);
                let Ok(payload) = serde_json::to_string(&msg) else {
                    continue;
                };
                if writer.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            writer_connected.store(false, Ordering::Relaxed);
        });

        let instruments = Arc::clone(&self.instruments);
        let subs = Arc::clone(&self.subs);
        let bars = Arc::clone(&self.bars);
        let reader_handle = tokio::spawn(async move {
            while let Some(Ok(msg)) = reader.next().await {
                let Message::Text(text) = msg else {
                    continue;
                };
                let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) else {
                    continue;
                };
                handle_market_message(server_msg, &sink, &instruments, &subs, &bars).await;
            }
            connected.store(false, Ordering::Relaxed);
        });

        self.task_handles.push(writer_handle);
        self.task_handles.push(reader_handle);
        self.connected.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn subscribe_instruments(&mut self, _cmd: SubscribeInstruments) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        drop(get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos();
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
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        drop(get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos();
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
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        drop(get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            let start = date_to_unix_nanos(request.start);
            let end = date_to_unix_nanos(request.end);
            if let Ok(def) = ensure_instrument(&http, &base, &instruments, &symbol).await
                && let Ok(trades) =
                    fetch_trades(&http, &base, &symbol, start, end, request.limit).await
            {
                let data = trades
                    .iter()
                    .map(|t| convert::trade_tick(t, request.instrument_id, &def, now_unix_nanos()))
                    .collect();
                let response = TradesResponse::new(
                    request.request_id,
                    client_id,
                    request.instrument_id,
                    data,
                    start,
                    end,
                    now_unix_nanos(),
                    request.params,
                );
                drop(sink.send(DataEvent::Response(DataResponse::Trades(response))));
            }
        }));
        Ok(())
    }

    fn request_quotes(&self, request: RequestQuotes) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let client_id = request.client_id.unwrap_or(self.client_id);
        drop(get_runtime().spawn(async move {
            let response = QuotesResponse::new(
                request.request_id,
                client_id,
                request.instrument_id,
                Vec::new(),
                date_to_unix_nanos(request.start),
                date_to_unix_nanos(request.end),
                now_unix_nanos(),
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
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        drop(get_runtime().spawn(async move {
            let instrument_id = request.bar_type.instrument_id();
            let symbol = symbol_from_instrument(instrument_id);
            let start = date_to_unix_nanos(request.start);
            let end = date_to_unix_nanos(request.end);
            if let Ok(def) = ensure_instrument(&http, &base, &instruments, &symbol).await
                && let Ok(trades) =
                    fetch_trades(&http, &base, &symbol, start, end, request.limit).await
            {
                let bars = aggregate_bars(&request.bar_type, &trades, &def);
                let response = BarsResponse::new(
                    request.request_id,
                    client_id,
                    request.bar_type,
                    bars,
                    start,
                    end,
                    now_unix_nanos(),
                    request.params,
                );
                drop(sink.send(DataEvent::Response(DataResponse::Bars(response))));
            }
        }));
        Ok(())
    }

    fn request_instruments(&self, request: RequestInstruments) -> anyhow::Result<()> {
        let sink = self.sink()?;
        let http = self.http.clone();
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        drop(get_runtime().spawn(async move {
            if let Ok(defs) = fetch_instruments(&http, &base).await {
                cache_instruments(&instruments, defs.clone());
                let ts_init = now_unix_nanos();
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
        let base = self.config.http_base_url();
        let instruments = Arc::clone(&self.instruments);
        let client_id = request.client_id.unwrap_or(self.client_id);
        drop(get_runtime().spawn(async move {
            let symbol = symbol_from_instrument(request.instrument_id);
            if let Ok(def) = ensure_instrument(&http, &base, &instruments, &symbol).await {
                let ts_init = now_unix_nanos();
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

#[derive(Debug)]
struct ActiveBar {
    open: Decimal,
    high: Decimal,
    low: Decimal,
    close: Decimal,
    volume: Decimal,
    close_ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WsCommand {
    Subscribe {
        symbols: Vec<Symbol>,
        start_ts: Option<u64>,
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
        WsCommand::Subscribe { symbols, start_ts } => {
            ClientMessage::Subscribe { symbols, start_ts }
        }
        WsCommand::Unsubscribe { symbols } => ClientMessage::Unsubscribe { symbols },
    }
}

async fn handle_market_message(
    msg: ServerMessage,
    sink: &UnboundedSender<DataEvent>,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
) {
    match msg {
        ServerMessage::Trade(trade) => {
            let Some(def) = instrument_def(instruments, &trade.symbol) else {
                return;
            };
            let state = sub_state(subs, &trade.symbol);
            let id = convert::instrument_id(&def);
            if state.as_ref().is_some_and(|s| s.trades > 0) {
                let tick = convert::trade_tick(&trade, id, &def, now_unix_nanos());
                drop(sink.send(DataEvent::Data(Data::Trade(tick))));
            }
            if state.as_ref().is_some_and(|s| s.bars > 0) {
                emit_live_bars(&trade, &def, sink, bars);
            }
        }
        ServerMessage::Quote(quote) => {
            let Some(def) = instrument_def(instruments, &quote.symbol) else {
                return;
            };
            let state = sub_state(subs, &quote.symbol);
            if state.as_ref().is_some_and(|s| s.quotes > 0) {
                let id = convert::instrument_id(&def);
                let tick = convert::quote_tick(&quote, id, &def, now_unix_nanos());
                drop(sink.send(DataEvent::Data(Data::Quote(tick))));
            }
        }
        _ => {}
    }
}

fn emit_live_bars(
    trade: &mogwai_protocol::TradeTick,
    def: &InstrumentDef,
    sink: &UnboundedSender<DataEvent>,
    bars: &Arc<Mutex<HashMap<BarType, BarSubState>>>,
) {
    let mut ready = Vec::new();
    {
        let mut bars = bars.lock().expect("bar mutex poisoned");
        for (bar_type, state) in bars.iter_mut() {
            if bar_type.instrument_id() != convert::instrument_id(def) || state.refs == 0 {
                continue;
            }
            if let Some(bar) = update_bar_state(*bar_type, state, trade, def) {
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
) -> Option<Bar> {
    let interval = get_bar_interval_ns(&bar_type).as_u64();
    let close_ts = ((trade.ts_event / interval) + 1) * interval;
    if let Some(active) = &mut state.active {
        if trade.ts_event >= active.close_ts {
            let bar = active_to_bar(bar_type, active, def);
            state.active = Some(new_active_bar(trade, close_ts));
            Some(bar)
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
) -> Vec<Bar> {
    let mut state = BarSubState::default();
    let mut out = Vec::new();
    for trade in trades {
        if let Some(bar) = update_bar_state(*bar_type, &mut state, trade, def) {
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

fn active_to_bar(bar_type: BarType, active: &ActiveBar, def: &InstrumentDef) -> Bar {
    Bar::new(
        bar_type,
        convert::price(active.open, def.price_precision),
        convert::price(active.high, def.price_precision),
        convert::price(active.low, def.price_precision),
        convert::price(active.close, def.price_precision),
        convert::quantity(active.volume, def.size_precision),
        UnixNanos::from(active.close_ts),
        now_unix_nanos(),
    )
}

fn sub_state(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    symbol: &str,
) -> Option<SubStateSnapshot> {
    subs.lock()
        .expect("subscription mutex poisoned")
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
    instruments
        .lock()
        .expect("instrument mutex poisoned")
        .get(symbol)
        .cloned()
}

async fn seed_instruments(
    http: &HttpClient,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
) -> anyhow::Result<()> {
    let defs = fetch_instruments(http, base).await?;
    cache_instruments(instruments, defs);
    Ok(())
}

async fn ensure_instrument(
    http: &HttpClient,
    base: &str,
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    symbol: &str,
) -> anyhow::Result<InstrumentDef> {
    if let Some(def) = instrument_def(instruments, symbol) {
        return Ok(def);
    }
    seed_instruments(http, base, instruments).await?;
    instrument_def(instruments, symbol).with_context(|| format!("unknown instrument {symbol}"))
}

fn cache_instruments(
    instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    defs: Vec<InstrumentDef>,
) {
    let mut cache = instruments.lock().expect("instrument mutex poisoned");
    for def in defs {
        cache.insert(def.symbol.clone(), def);
    }
}

async fn fetch_instruments(http: &HttpClient, base: &str) -> anyhow::Result<Vec<InstrumentDef>> {
    let response = http
        .get(join_url(base, "instruments"), None, None, Some(30), None)
        .await
        .context("fetch instruments")?;
    ensure!(
        response.status.is_success(),
        "fetch instruments returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode instruments")
}

async fn fetch_trades(
    http: &HttpClient,
    base: &str,
    symbol: &str,
    start: Option<UnixNanos>,
    end: Option<UnixNanos>,
    limit: Option<std::num::NonZeroUsize>,
) -> anyhow::Result<Vec<mogwai_protocol::TradeTick>> {
    let mut params = HashMap::new();
    params.insert("symbol".to_string(), vec![symbol.to_string()]);
    if let Some(start) = start {
        params.insert("start".to_string(), vec![start.as_u64().to_string()]);
    }
    if let Some(end) = end {
        params.insert("end".to_string(), vec![end.as_u64().to_string()]);
    }
    params.insert("limit".to_string(), vec![capped_limit(limit).to_string()]);
    let response = http
        .get(
            join_url(base, "trades"),
            Some(&params),
            None,
            Some(30),
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

/// Resolves the effective row limit sent to the server's bounded `/trades`
/// scan. A missing limit defaults to the ceiling, and any requested limit is
/// clamped to it so neither the response body nor the materialized nautilus
/// response `Vec` can grow unbounded over a multi-GB dump.
fn capped_limit(limit: Option<std::num::NonZeroUsize>) -> usize {
    limit
        .map_or(HISTORY_LIMIT_CAP, std::num::NonZeroUsize::get)
        .min(HISTORY_LIMIT_CAP)
}

fn join_url(base: &str, path: &str) -> String {
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

fn now_unix_nanos() -> UnixNanos {
    UnixNanos::from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before epoch")
            .as_nanos() as u64,
    )
}

#[derive(Debug)]
#[allow(dead_code)]
pub struct MogwaiExecutionClient {
    core: ExecutionClientCore,
    config: MogwaiExecClientConfig,
    emitter: ExecutionEventEmitter,
    connected: Arc<AtomicBool>,
    http: HttpClient,
    ws_cmd: Option<UnboundedSender<ExecWsCommand>>,
    state: Arc<Mutex<ExecState>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
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
        let http = HttpClient::new(HashMap::new(), Vec::new(), Vec::new(), None, Some(30), None)
            .context("create HTTP client")?;
        Ok(Self {
            core,
            config,
            emitter,
            connected: Arc::new(AtomicBool::new(false)),
            http,
            ws_cmd: None,
            state: Arc::new(Mutex::new(ExecState::default())),
            instruments: Arc::new(Mutex::new(HashMap::new())),
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
        self.emitter
            .emit_account_state(balances, margins, reported, ts_event);
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

    async fn connect(&mut self) -> anyhow::Result<()> {
        let http_base_url = self.config.http_base_url();
        seed_instruments(&self.http, &http_base_url, &self.instruments).await?;

        let ws_url = join_url(&self.config.ws_url(), "ws");
        let (ws, _) = connect_async(&ws_url)
            .await
            .with_context(|| format!("connect websocket {ws_url}"))?;
        let (mut writer, mut reader) = ws.split();
        let (cmd_tx, mut cmd_rx) = unbounded_channel::<ExecWsCommand>();
        self.ws_cmd = Some(cmd_tx);

        let connected = Arc::clone(&self.connected);
        let writer_connected = Arc::clone(&self.connected);
        let writer_handle = tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                let msg = exec_command_to_client_message(cmd);
                let Ok(payload) = serde_json::to_string(&msg) else {
                    continue;
                };
                if writer.send(Message::Text(payload.into())).await.is_err() {
                    break;
                }
            }
            writer_connected.store(false, Ordering::Relaxed);
        });

        let ctx = ExecContext {
            emitter: self.emitter.clone(),
            state: Arc::clone(&self.state),
            instruments: Arc::clone(&self.instruments),
            trader_id: self.core.trader_id,
            account_id: self.core.account_id,
        };
        let reader_handle = tokio::spawn(async move {
            while let Some(Ok(msg)) = reader.next().await {
                let Message::Text(text) = msg else {
                    continue;
                };
                let Ok(server_msg) = serde_json::from_str::<ServerMessage>(&text) else {
                    continue;
                };
                handle_exec_message(server_msg, &ctx);
            }
            connected.store(false, Ordering::Relaxed);
        });

        self.task_handles.push(writer_handle);
        self.task_handles.push(reader_handle);
        self.connected.store(true, Ordering::Relaxed);
        self.core.set_connected();
        Ok(())
    }

    async fn disconnect(&mut self) -> anyhow::Result<()> {
        self.stop()
    }

    fn submit_order(&self, cmd: SubmitOrder) -> anyhow::Result<()> {
        let order = self.core.get_order(&cmd.client_order_id)?;
        self.emitter.emit_order_submitted(&order);

        let wire = mogwai_protocol::SubmitOrder {
            client_order_id: cmd.client_order_id.to_string(),
            symbol: symbol_from_instrument(cmd.instrument_id),
            side: wire_side(cmd.order_init.order_side)?,
            order_type: wire_order_type(cmd.order_init.order_type)?,
            quantity: cmd.order_init.quantity.as_decimal(),
            price: cmd.order_init.price.map(|p| p.as_decimal()),
            time_in_force: wire_time_in_force(cmd.order_init.time_in_force)?,
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
            },
        );
        drop(state);

        self.send_ws(ExecWsCommand::Submit(wire))
    }

    fn modify_order(&self, cmd: ModifyOrder) -> anyhow::Result<()> {
        self.send_ws(ExecWsCommand::Modify {
            client_order_id: cmd.client_order_id.to_string(),
            price: cmd.price.map(|p| p.as_decimal()),
            quantity: cmd.quantity.map(|q| q.as_decimal()),
        })
    }

    fn cancel_order(&self, cmd: CancelOrder) -> anyhow::Result<()> {
        self.send_ws(ExecWsCommand::Cancel {
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
            .map(|(client_order_id, record)| {
                OrderStatusReport::new(
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
                    record.quantity_for_report(&self.instruments),
                    record.filled_quantity_for_report(&self.instruments),
                    record.ts_accepted,
                    record.ts_last,
                    now_unix_nanos(),
                    None,
                )
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
            .map(|fill| {
                FillReport::new(
                    self.core.account_id,
                    fill.instrument_id,
                    fill.venue_order_id,
                    fill.trade_id,
                    fill.order_side,
                    fill.last_qty,
                    fill.last_px,
                    Money::new(
                        fill.commission.to_f64().expect("decimal fits f64"),
                        fill.quote_currency,
                    ),
                    LiquiditySide::Taker,
                    Some(fill.client_order_id),
                    None,
                    fill.ts_event,
                    now_unix_nanos(),
                    None,
                )
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
                Some(PositionStatusReport::new(
                    self.core.account_id,
                    position.instrument_id,
                    position_side(position.quantity),
                    convert::quantity(position.quantity.abs(), def.size_precision),
                    position.ts_last,
                    now_unix_nanos(),
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

#[derive(Clone)]
struct ExecContext {
    emitter: ExecutionEventEmitter,
    state: Arc<Mutex<ExecState>>,
    instruments: Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    trader_id: nautilus_model::identifiers::TraderId,
    account_id: AccountId,
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
}

impl OrderRecord {
    fn quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> nautilus_model::types::Quantity {
        let precision = instrument_def(instruments, &symbol_from_instrument(self.instrument_id))
            .map_or(8, |def| def.size_precision);
        convert::quantity(self.quantity, precision)
    }

    fn filled_quantity_for_report(
        &self,
        instruments: &Arc<Mutex<HashMap<Symbol, InstrumentDef>>>,
    ) -> nautilus_model::types::Quantity {
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
                now_unix_nanos(),
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
                return;
            };
            ctx.emitter.emit_order_rejected_event(
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                &reason,
                UnixNanos::from(ts_event),
                false,
            );
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
                now_unix_nanos(),
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
            ts_event,
            ..
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
            let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
                // An amend never reverses an in-progress fill: a partially filled
                // order that is re-priced or re-sized stays PARTIALLY_FILLED so the
                // mirror does not report Accepted alongside a non-zero filled_qty.
                if record.status != OrderStatus::PartiallyFilled {
                    record.status = OrderStatus::Accepted;
                }
                record.venue_order_id = Some(venue_order_id);
                record.quantity = quantity;
                record.price = price;
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
                convert::quantity(quantity, def.size_precision),
                UUID4::new(),
                UnixNanos::from(ts_event),
                now_unix_nanos(),
                false,
                Some(venue_order_id),
                Some(ctx.account_id),
                price.map(|p| convert::price(p, def.price_precision)),
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
                return;
            };
            ctx.emitter.emit_order_modify_rejected_event(
                record.strategy_id,
                record.instrument_id,
                client_order_id,
                venue_order_id.map(VenueOrderId::from),
                &reason,
                UnixNanos::from(ts_event),
            );
        }
        ServerMessage::OrderFilled(fill) => handle_order_filled(fill, ctx),
        ServerMessage::AccountState(state) => handle_account_state(state, ctx),
        ServerMessage::Trade(_) | ServerMessage::Quote(_) => {}
    }
}

fn handle_order_filled(fill: mogwai_protocol::OrderFilled, ctx: &ExecContext) {
    let client_order_id = ClientOrderId::from(fill.client_order_id);
    let Some(def) = instrument_def(&ctx.instruments, &fill.symbol) else {
        return;
    };
    let Ok(quote_currency) = Currency::from_str(&def.quote) else {
        return;
    };
    let venue_order_id = VenueOrderId::from(fill.venue_order_id);
    let trade_id = TradeId::from(fill.trade_id);
    let last_qty = convert::quantity(fill.last_qty, def.size_precision);
    let last_px = convert::price(fill.last_px, def.price_precision);
    let Some(record) = with_order_record(&ctx.state, client_order_id, |record| {
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
        record.clone()
    }) else {
        return;
    };

    {
        let mut state = ctx.state.lock().expect("execution state mutex poisoned");
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
        Some(Money::new(
            fill.commission.to_f64().expect("decimal fits f64"),
            quote_currency,
        ))
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
        now_unix_nanos(),
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
            Some(AccountBalance::new(
                Money::new(balance.total.to_f64().expect("decimal fits f64"), currency),
                Money::new(balance.locked.to_f64().expect("decimal fits f64"), currency),
                Money::new(balance.free.to_f64().expect("decimal fits f64"), currency),
            ))
        })
        .collect();

    {
        let mut mirror = ctx.state.lock().expect("execution state mutex poisoned");
        for position in state.positions {
            let Some(def) = instrument_def(&ctx.instruments, &position.symbol) else {
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
    ctx.emitter
        .emit_account_state(balances, Vec::new(), true, ts_event);
}

fn with_order_record<T>(
    state: &Arc<Mutex<ExecState>>,
    client_order_id: ClientOrderId,
    f: impl FnOnce(&mut OrderRecord) -> T,
) -> Option<T> {
    state
        .lock()
        .expect("execution state mutex poisoned")
        .orders
        .get_mut(&client_order_id)
        .map(f)
}

fn order_record(
    state: &Arc<Mutex<ExecState>>,
    client_order_id: ClientOrderId,
) -> Option<OrderRecord> {
    state
        .lock()
        .expect("execution state mutex poisoned")
        .orders
        .get(&client_order_id)
        .cloned()
}

fn wire_side(side: OrderSide) -> anyhow::Result<Side> {
    match side {
        OrderSide::Buy => Ok(Side::Buy),
        OrderSide::Sell => Ok(Side::Sell),
        other => anyhow::bail!("unsupported order side {other:?}"),
    }
}

fn wire_order_type(order_type: OrderType) -> anyhow::Result<mogwai_protocol::OrderType> {
    match order_type {
        OrderType::Market => Ok(mogwai_protocol::OrderType::Market),
        OrderType::Limit => Ok(mogwai_protocol::OrderType::Limit),
        other => anyhow::bail!("unsupported order type {other:?}"),
    }
}

fn wire_time_in_force(
    tif: nautilus_model::enums::TimeInForce,
) -> anyhow::Result<mogwai_protocol::TimeInForce> {
    match tif {
        nautilus_model::enums::TimeInForce::Gtc => Ok(mogwai_protocol::TimeInForce::Gtc),
        nautilus_model::enums::TimeInForce::Ioc => Ok(mogwai_protocol::TimeInForce::Ioc),
        nautilus_model::enums::TimeInForce::Fok => Ok(mogwai_protocol::TimeInForce::Fok),
        other => anyhow::bail!("unsupported time in force {other:?}"),
    }
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

    use mogwai_protocol::AggressorSide;
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

        let bars = aggregate_bars(&bar_type, &trades, &def);

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
        assert!(update_bar_state(bar_type, &mut state, &trade(0, 10_000, 1), &def).is_none());
        assert!(
            update_bar_state(bar_type, &mut state, &trade(interval - 1, 11_000, 1), &def).is_none()
        );

        // A trade past the close boundary flushes the completed window.
        let bar = update_bar_state(bar_type, &mut state, &trade(interval, 12_000, 1), &def)
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
        assert_eq!(capped_limit(None), HISTORY_LIMIT_CAP);
        assert_eq!(
            capped_limit(NonZeroUsize::new(HISTORY_LIMIT_CAP * 100)),
            HISTORY_LIMIT_CAP
        );
        assert_eq!(capped_limit(NonZeroUsize::new(5)), 5);
    }
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, rc::Rc};

    use nautilus_common::{cache::Cache, clients::ExecutionClient, messages::ExecutionEvent};
    use nautilus_live::{ExecutionClientCore, ExecutionEventEmitter};
    use nautilus_model::{
        enums::{OmsType, TimeInForce},
        identifiers::{ClientId, StrategyId, TraderId},
    };

    use super::*;

    fn execution_client() -> MogwaiExecutionClient {
        let config = MogwaiExecClientConfig::default();
        let cache = Rc::new(RefCell::new(Cache::default()));
        let core = ExecutionClientCore::new(
            TraderId::from("MOGWAI-001"),
            ClientId::from("MOGWAI-TEST"),
            *MOGWAI_VENUE,
            OmsType::Netting,
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
