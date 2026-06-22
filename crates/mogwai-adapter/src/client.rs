use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, ensure};
use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use mogwai_protocol::{ClientMessage, InstrumentDef, ServerMessage, Symbol};
use nautilus_common::{
    clients::{DataClient, ExecutionClient},
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
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    accounts::AccountAny,
    data::{Bar, BarType, Data, bar::get_bar_interval_ns},
    enums::OmsType,
    identifiers::{AccountId, ClientId, InstrumentId, Venue},
    types::{AccountBalance, MarginBalance},
};
use nautilus_network::http::HttpClient;
use rust_decimal::Decimal;
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
}

impl MogwaiExecutionClient {
    /// Creates a new disconnected mogwai execution client.
    ///
    /// # Errors
    ///
    /// Returns an error if the supplied config is invalid.
    pub fn new(core: ExecutionClientCore, config: MogwaiExecClientConfig) -> anyhow::Result<Self> {
        config.validate()?;
        Ok(Self { core, config })
    }

    #[cfg(test)]
    fn is_started(&self) -> bool {
        self.core.is_started()
    }

    #[cfg(test)]
    fn is_stopped(&self) -> bool {
        self.core.is_stopped()
    }
}

#[async_trait(?Send)]
impl ExecutionClient for MogwaiExecutionClient {
    fn is_connected(&self) -> bool {
        self.core.is_connected()
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
        _balances: Vec<AccountBalance>,
        _margins: Vec<MarginBalance>,
        _reported: bool,
        _ts_event: UnixNanos,
    ) -> anyhow::Result<()> {
        // The trait requires this surface before account snapshot emission is
        // implemented. Later handlers will consume mogwai AccountState messages
        // and emit real nautilus account events here.
        Ok(())
    }

    fn start(&mut self) -> anyhow::Result<()> {
        if self.core.is_started() {
            return Ok(());
        }

        self.core.set_started();
        Ok(())
    }

    fn stop(&mut self) -> anyhow::Result<()> {
        if self.core.is_stopped() {
            return Ok(());
        }

        self.core.set_stopped();
        self.core.set_disconnected();
        Ok(())
    }
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

    use nautilus_common::{cache::Cache, clients::ExecutionClient};
    use nautilus_live::ExecutionClientCore;
    use nautilus_model::{
        enums::OmsType,
        identifiers::{ClientId, TraderId},
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
}
