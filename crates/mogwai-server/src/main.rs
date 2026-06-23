//! mogwai fake-broker server.
//!
//! Hosts the native JSON-over-WS gateway (`/ws`) that the broadarrow adapter
//! connects to, plus an out-of-band control plane (`/control/divergence`) for
//! arming deterministic divergences from tests. The exchange logic lives in
//! [`mogwai_engine`]; market data is synthesized from the committed fingerprint
//! by [`mogwai_data`]; this binary owns sockets, the clock and replay pacing.

mod source;

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use axum::{
    Json, Router,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::{SinkExt, StreamExt};
use mogwai_data::TickEvent;
use mogwai_engine::Engine;
use mogwai_protocol::{
    ClientMessage, InstrumentDef, MAX_HISTORY_LIMIT, MarketRegime, QuoteTick, ServerMessage,
    TradeTick, control::Divergence, validate_divergence, validate_market_regime,
};
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

/// Replay/runtime configuration, sourced from the environment at startup.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct Config {
    /// Replay speed multiplier. `0.0` means unthrottled (stream as fast as the client
    /// drains); otherwise inter-tick wall delay = (tick gap) / speed.
    speed: f64,
    /// Maximum wall-clock sleep between two ticks under paced replay, in
    /// milliseconds. `0` disables the cap.
    gap_cap_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            speed: 0.0,
            gap_cap_ms: 1000,
        }
    }
}

impl Config {
    /// Load run config from a TOML file. The path is the `--config <path>`
    /// argument when passed, otherwise `mogwai.toml` in the working directory.
    /// A missing file yields built-in defaults so the server still starts with
    /// no config present; a malformed file is a hard error rather than a silent
    /// fallback. Replaces the former MOGWAI_REPLAY_SPEED and MOGWAI_GAP_CAP_MS
    /// environment variables - run knobs belong in explicit input, not ambient
    /// environment.
    fn load() -> anyhow::Result<Self> {
        let mut path = std::path::PathBuf::from("mogwai.toml");
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            if arg == "--config"
                && let Some(p) = args.next()
            {
                path = std::path::PathBuf::from(p);
            }
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
}

#[derive(Clone)]
struct AppState {
    engine: Arc<Mutex<Engine>>,
    cfg: Config,
    /// Execution-event delay in milliseconds, shared by all live writers.
    delay_ms: Arc<AtomicU64>,
    /// Wall-clock unix-ns instant before which writers drop all outbound frames.
    dark_until_ns: Arc<AtomicU64>,
}

/// Nanoseconds since the Unix epoch - the server's clock, fed into the engine.
///
/// Thin local alias over [`mogwai_protocol::now_unix_nanos`], the shared
/// saturating clock reader the adapter also uses: a backward clock step
/// (NTP/leap) that puts `now` before the epoch saturates to 0 rather than
/// panicking every order path and divergence arm, and the `u128` nanosecond
/// count is clamped to `u64::MAX` rather than silently truncated. Kept as a
/// local name so the call sites below stay unchanged.
fn now_ns() -> u64 {
    mogwai_protocol::now_unix_nanos()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mogwai_server=info".into()),
        )
        .init();

    let cfg = Config::load()?;
    tracing::info!(speed = cfg.speed, gap_cap_ms = cfg.gap_cap_ms, "config");

    let state = AppState {
        engine: Arc::new(Mutex::new(Engine::new())),
        cfg,
        delay_ms: Arc::new(AtomicU64::new(0)),
        dark_until_ns: Arc::new(AtomicU64::new(0)),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/instruments", get(instruments))
        .route("/trades", get(trades))
        .route("/quotes", get(quotes))
        .route("/orders", post(submit_order_http))
        .route("/ws", get(ws_upgrade))
        .route("/control/divergence", post(arm_divergence))
        .with_state(state);

    let addr = "127.0.0.1:8787";
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "mogwai listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Control plane: arm a divergence to fire on its next trigger.
async fn arm_divergence(
    State(state): State<AppState>,
    Json(div): Json<Divergence>,
) -> impl IntoResponse {
    tracing::info!(?div, "arming divergence");
    // Validate at the arming boundary so an out-of-range knob (e.g. a
    // `PartialFillNext.fraction` outside `(0, 1]`) is rejected before it is
    // stored into server state or armed on the engine, rather than surfacing
    // as a degenerate fill downstream. Mirrors the `validate_market_regime`
    // gate on the subscription/history paths.
    if let Err(err) = validate_divergence(&div) {
        tracing::warn!(?div, err, "rejecting out-of-range divergence");
        return (StatusCode::BAD_REQUEST, err.to_string());
    }
    match div {
        Divergence::DelayAcks { ms } => {
            state.delay_ms.store(ms, Ordering::Relaxed);
        }
        Divergence::GoDark { ms } => {
            let until = now_ns().saturating_add(ms.saturating_mul(1_000_000));
            state.dark_until_ns.store(until, Ordering::Relaxed);
        }
        // Server-ownership contract (pins B.4 / E.11): `DelayAcks` and `GoDark`
        // are temporal, connection-scoped divergences with no synchronous
        // engine-side trigger. The server owns them - they are applied here via
        // `delay_ms` / `dark_until_ns` and must NEVER reach `engine.arm()`, which
        // silently drops both variants (`Engine::arm`). The two arms above
        // intercept them before this catch-all, so `engine_div` can only be one
        // of the four engine-side variants. The assert makes a future refactor
        // that forwards a whole `HavocSpec.server` vec straight to `engine.arm()`
        // fail loudly rather than silently losing the blackout/delay knobs.
        engine_div => {
            debug_assert!(
                !matches!(
                    engine_div,
                    Divergence::DelayAcks { .. } | Divergence::GoDark { .. }
                ),
                "DelayAcks/GoDark are server-owned and must not be forwarded to engine.arm()",
            );
            state.engine.lock().await.arm(engine_div);
        }
    }
    (StatusCode::ACCEPTED, String::new())
}

async fn instruments(State(state): State<AppState>) -> Json<Vec<InstrumentDef>> {
    Json(state.engine.lock().await.instrument_defs())
}

/// HTTP order-entry surface for profiles that do not use `/ws` for orders.
async fn submit_order_http(
    State(state): State<AppState>,
    Json(msg): Json<ClientMessage>,
) -> Result<Json<Vec<ServerMessage>>, StatusCode> {
    match msg {
        ClientMessage::Subscribe { .. } | ClientMessage::Unsubscribe { .. } => {
            Err(StatusCode::BAD_REQUEST)
        }
        order_cmd => {
            let events = state.engine.lock().await.process(order_cmd, now_ns());
            Ok(Json(events))
        }
    }
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    symbol: String,
    start: Option<u64>,
    end: Option<u64>,
    limit: Option<usize>,
    #[serde(default)]
    regime: Option<String>,
}

async fn trades(
    State(_state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<HistoryQuery>,
) -> Result<Json<Vec<TradeTick>>, StatusCode> {
    let limit = normalize_limit(query.limit);
    let regime = parse_history_regime(query.regime.as_deref());
    // Synthesizing up to `MAX_HISTORY_LIMIT` ticks is pure CPU work against the
    // generator (the source never blocks on IO), and `next_tick` never returns
    // `None`, so a no-`end` request always grinds out the full `limit`. Run it on
    // a blocking thread rather than inline on the tokio worker, so a burst of
    // `/trades` requests cannot stall the async runtime's worker pool.
    let HistoryQuery {
        symbol, start, end, ..
    } = query;
    let ticks = match tokio::task::spawn_blocking(move || {
        bounded_trades(&symbol, start, end, limit, regime)
    })
    .await
    {
        Ok(ticks) => ticks,
        Err(e) => {
            tracing::error!(%e, "history synthesis task failed");
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    Ok(Json(ticks))
}

async fn quotes(
    axum::extract::Query(_query): axum::extract::Query<HistoryQuery>,
) -> Json<Vec<QuoteTick>> {
    // Mogwai's generated history is trades-only, so a bounded historical quote
    // fetch is empty by construction. If synthesized top-of-book is wired in,
    // this route grows the same seek-and-bound scan as `trades`.
    Json(Vec::new())
}

/// The single owner of `/trades` page-size policy: a missing `limit` requests a
/// full page, and any requested size is clamped to `MAX_HISTORY_LIMIT`. Folding
/// the `unwrap_or` + `.min()` here (rather than splitting the clamp into the
/// handler and an empty-vec guard into `bounded_trades`) keeps the page-size
/// contract in one place. A normalized `0` still flows through to the early
/// return in `bounded_trades`, which the synthesis loop relies on (the
/// `out.len() >= limit` break would otherwise yield one tick for `limit == 0`).
fn normalize_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(MAX_HISTORY_LIMIT).min(MAX_HISTORY_LIMIT)
}

fn bounded_trades(
    symbol: &str,
    start: Option<u64>,
    end: Option<u64>,
    limit: usize,
    regime: Option<MarketRegime>,
) -> Vec<TradeTick> {
    if limit == 0 {
        return Vec::new();
    }

    let mut merged = source::build_history_source(symbol, start, regime);
    let mut out = Vec::new();

    while let Some(tick) = merged.next_tick() {
        if let Some(end) = end
            && tick.ts_event() > end
        {
            break;
        }

        let TickEvent::Trade(trade) = tick else {
            continue;
        };
        out.push(trade);
        if out.len() >= limit {
            break;
        }
    }

    out
}

fn parse_history_regime(raw: Option<&str>) -> Option<MarketRegime> {
    let raw = raw?;
    let Ok(regime) = serde_json::from_str::<MarketRegime>(raw) else {
        tracing::warn!(raw, "dropping malformed market regime");
        return None;
    };
    validate_regime_or_clean(Some(regime))
}

fn validate_regime_or_clean(regime: Option<MarketRegime>) -> Option<MarketRegime> {
    let regime = regime?;
    match validate_market_regime(&regime) {
        Ok(()) => Some(regime),
        Err(err) => {
            tracing::warn!(?regime, err, "dropping out-of-range market regime");
            None
        }
    }
}

async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

/// One client session.
///
/// The socket is split so order events and replayed market data can be written
/// concurrently: every outbound [`ServerMessage`] funnels through one mpsc
/// channel drained by a single writer task. Order commands are processed inline
/// against the engine; `Subscribe` spawns a replay feeding the same channel.
async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::channel::<ServerMessage>(1024);
    // One replay stream PER SYMBOL, not per sorted symbol-set. Keying per set let
    // overlapping subscriptions (`[A,B]` then `[B,C]`) spawn two independent
    // replays both emitting `B` from independent generators/clocks, so the client
    // saw duplicated, interleaved, out-of-order-per-symbol `B` trades - breaking
    // the ascending-`ts_event` ordering the adapter's `PollCursor` relies on
    // (E.5). With a per-symbol map a given symbol is fed by exactly one stream:
    // re-subscribing a symbol already in flight quiesces (cancels + joins) the old
    // stream before the replacement emits, so no stale tick interleaves at the
    // seam (E.6); the handles are tracked and reaped so threads cannot pile up
    // under connect/subscribe/disconnect churn (E.7).
    let mut replays: HashMap<String, Replay> = HashMap::new();
    let delay_ms = Arc::clone(&state.delay_ms);
    let dark_until_ns = Arc::clone(&state.dark_until_ns);

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if is_execution_event(&msg) {
                let delay = delay_ms.load(Ordering::Relaxed);
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
            if now_ns() < dark_until_ns.load(Ordering::Relaxed) {
                continue;
            }
            // Skip an un-serializable frame rather than panicking the writer
            // task: this runs in a detached `tokio::spawn`, so an `expect` here
            // would silently tear down the whole connection's outbound stream.
            let payload = match serde_json::to_string(&msg) {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!(%e, "dropping un-serializable ServerMessage");
                    continue;
                }
            };
            if sink.send(Message::Text(payload.into())).await.is_err() {
                break; // client gone
            }
        }
    });

    while let Some(Ok(msg)) = stream.next().await {
        let Message::Text(text) = msg else { continue };

        let client_msg: ClientMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(%e, %text, "undecodable client message");
                continue;
            }
        };

        match client_msg {
            ClientMessage::Subscribe {
                symbols,
                start_ts,
                regime,
            } => {
                let regime = validate_regime_or_clean(regime);
                for symbol in dedup_symbols(symbols) {
                    // Quiesce any in-flight stream for this symbol BEFORE the
                    // replacement emits: cancel the old thread and join it (off
                    // the async worker) so it cannot land one last tick into the
                    // shared channel after the new generator starts. Without the
                    // join the old thread - blocked in a send or mid-`next_tick`
                    // - could deliver an out-of-order/duplicate tick at the seam
                    // (E.6).
                    if let Some(old) = replays.remove(&symbol) {
                        quiesce_replay(old).await;
                    }
                    let cancel = Arc::new(AtomicBool::new(false));
                    let handle = spawn_replay(
                        symbol.clone(),
                        start_ts,
                        regime,
                        &state.cfg,
                        tx.clone(),
                        Arc::clone(&cancel),
                    );
                    replays.insert(symbol, Replay { cancel, handle });
                }
            }
            ClientMessage::Unsubscribe { symbols } => {
                for symbol in dedup_symbols(symbols) {
                    if let Some(old) = replays.remove(&symbol) {
                        quiesce_replay(old).await;
                    }
                }
            }
            order_cmd => {
                let events = state.engine.lock().await.process(order_cmd, now_ns());
                for ev in events {
                    if tx.send(ev).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    // Cancel every replay first so the threads stop generating, then join them
    // (the writer task is still draining `rx`, so a thread blocked in a send
    // unblocks and observes the cancel promptly). Reaping the handles here means
    // a disconnect leaves no detached replay thread parked in `next_tick`/send
    // (E.7). Only after the threads are joined do we drop the last `tx` and let
    // the writer task finish.
    for replay in replays.values() {
        replay.cancel.store(true, Ordering::Relaxed);
    }
    for (_, replay) in replays.drain() {
        quiesce_replay(replay).await;
    }
    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!(%e, "writer task did not shut down cleanly");
    }
}

/// A live per-symbol replay stream: the cancel flag the handler raises to stop
/// it, plus the OS-thread handle so the stream can be joined (reaped) rather than
/// detached and left to linger.
struct Replay {
    cancel: Arc<AtomicBool>,
    handle: std::thread::JoinHandle<()>,
}

/// Maximum wall time a replay thread parks while the outbound channel is full
/// before it re-checks its cancel flag. Bounds how long a cancelled stream can
/// stay parked in backpressure, so a quiesce/join completes promptly (E.7).
const REPLAY_SEND_POLL: Duration = Duration::from_millis(5);

/// Cancel a replay and join its thread, off the async worker.
///
/// The join can block briefly (until the thread observes the cancel between
/// generated ticks or within one [`REPLAY_SEND_POLL`] of a full-channel park),
/// so it runs on a blocking thread rather than stalling the tokio worker driving
/// this connection. Returning only once the thread has exited is what guarantees
/// quiescence: callers rely on it so a replaced stream cannot interleave a stale
/// tick after its successor begins (E.6), and so disconnect reaps every thread.
async fn quiesce_replay(replay: Replay) {
    replay.cancel.store(true, Ordering::Relaxed);
    if let Err(e) = tokio::task::spawn_blocking(move || replay.handle.join()).await {
        tracing::warn!(%e, "replay join task panicked");
    }
}

/// Sort + dedup the requested symbols so a single subscription naming a symbol
/// twice does not spawn (then immediately quiesce) two streams for it.
fn dedup_symbols(mut symbols: Vec<String>) -> Vec<String> {
    symbols.sort();
    symbols.dedup();
    symbols
}

fn is_execution_event(msg: &ServerMessage) -> bool {
    !matches!(msg, ServerMessage::Trade(_) | ServerMessage::Quote(_))
}

/// Stream generated trades for a single `symbol` as market data into `tx`,
/// returning the joinable thread handle so the caller can reap it.
///
/// The replay runs on a dedicated OS thread. It applies backpressure by retrying
/// a `try_send` whenever the channel is full, sleeping at most
/// [`REPLAY_SEND_POLL`] between attempts and re-checking `cancel` each time, so a
/// stream blocked behind a slow/stalled client still observes cancellation
/// promptly instead of parking indefinitely in a plain `blocking_send` (E.7).
fn spawn_replay(
    symbol: String,
    start_ts: Option<u64>,
    regime: Option<MarketRegime>,
    cfg: &Config,
    tx: mpsc::Sender<ServerMessage>,
    cancel: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let speed = cfg.speed;
    let gap_cap_ms = cfg.gap_cap_ms;
    std::thread::spawn(move || {
        let symbols = [symbol.clone()];
        let Some(mut merged) = source::build_live_source(&symbols, start_ts, regime) else {
            return;
        };
        tracing::info!(%symbol, ?start_ts, ?regime, "replay started");

        let mut prev_ts: Option<u64> = None;
        while let Some(tick) = merged.next_tick() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if speed > 0.0 {
                if let Some(prev) = prev_ts {
                    let gap_ns = tick.ts_event().saturating_sub(prev);
                    // Pace at nanosecond resolution: integer-dividing the scaled
                    // gap down to whole milliseconds collapses any sub-ms
                    // inter-tick gap to a zero-delay send, so micros-apart bursts
                    // (which the generator emits) wouldn't be paced at all and the
                    // realized timeline would not track original_timeline / speed.
                    let mut wait_ns = (gap_ns as f64 / speed) as u64;
                    if gap_cap_ms > 0 {
                        wait_ns = wait_ns.min(gap_cap_ms.saturating_mul(1_000_000));
                    }
                    if wait_ns > 0 {
                        std::thread::sleep(Duration::from_nanos(wait_ns));
                    }
                }
                prev_ts = Some(tick.ts_event());
            }

            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let msg = match tick {
                TickEvent::Trade(t) => ServerMessage::Trade(t),
                TickEvent::Quote(q) => ServerMessage::Quote(q),
            };
            if send_cancellable(&tx, msg, &cancel).is_err() {
                break; // client gone or stream cancelled
            }
        }
        tracing::info!(%symbol, "replay finished");
    })
}

/// Send `msg` into `tx`, applying backpressure without parking indefinitely.
///
/// `Sender::blocking_send` would block until the channel drains, and the replay
/// thread would only re-check `cancel` at the next loop top - so a stream behind
/// a stalled client could sit unkillable in a full channel. Instead this retries
/// `try_send`, sleeping at most [`REPLAY_SEND_POLL`] when the channel is full and
/// re-checking `cancel` between attempts. `Err(())` means the stream should stop
/// (client gone, or cancelled while parked).
fn send_cancellable(
    tx: &mpsc::Sender<ServerMessage>,
    msg: ServerMessage,
    cancel: &AtomicBool,
) -> Result<(), ()> {
    use mpsc::error::TrySendError;
    let mut msg = msg;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(());
        }
        match tx.try_send(msg) {
            Ok(()) => return Ok(()),
            Err(TrySendError::Full(returned)) => {
                msg = returned;
                std::thread::sleep(REPLAY_SEND_POLL);
            }
            Err(TrySendError::Closed(_)) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{OrderType, Side, SubmitOrder, TimeInForce};
    use std::time::Instant;

    fn state() -> AppState {
        AppState {
            engine: Arc::new(Mutex::new(Engine::new())),
            cfg: Config {
                speed: 0.0,
                gap_cap_ms: 0,
            },
            delay_ms: Arc::new(AtomicU64::new(0)),
            dark_until_ns: Arc::new(AtomicU64::new(0)),
        }
    }

    #[tokio::test]
    async fn http_orders_route_processes_order_commands() {
        let response = submit_order_http(
            State(state()),
            Json(ClientMessage::SubmitOrder(SubmitOrder {
                client_order_id: "HTTP1".into(),
                symbol: "BTCUSDT".into(),
                side: Side::Buy,
                order_type: OrderType::Limit,
                quantity: "1".parse().expect("decimal"),
                price: Some("100".parse().expect("decimal")),
                time_in_force: TimeInForce::Gtc,
            })),
        )
        .await
        .expect("order accepted");

        assert!(matches!(response.0[0], ServerMessage::OrderAccepted { .. }));
        assert!(matches!(response.0[1], ServerMessage::OrderFilled(_)));
        assert!(matches!(response.0[2], ServerMessage::AccountState(_)));
    }

    #[tokio::test]
    async fn http_orders_route_rejects_subscription_messages() {
        let err = submit_order_http(
            State(state()),
            Json(ClientMessage::Subscribe {
                symbols: vec!["BTCUSDT".into()],
                start_ts: None,
                regime: None,
            }),
        )
        .await
        .expect_err("subscribe rejected");

        assert_eq!(err, StatusCode::BAD_REQUEST);
    }

    #[test]
    fn generated_history_default_limit_is_bounded_and_fast() {
        let start = Instant::now();
        let ticks = bounded_trades("KEUR", None, None, MAX_HISTORY_LIMIT, None);
        let elapsed = start.elapsed();

        println!("default /trades synthesis elapsed: {elapsed:?}");
        assert_eq!(ticks.len(), MAX_HISTORY_LIMIT);
        assert!(ticks.len() <= 1_000);
        assert!(
            elapsed < Duration::from_millis(250),
            "default /trades synthesis took {elapsed:?}"
        );
    }

    #[test]
    fn generated_history_is_replayable_and_cursorable() {
        let first = bounded_trades("KEUR", None, None, 8, None);
        let replay = bounded_trades("KEUR", None, None, 8, None);
        assert_eq!(trade_signatures(&first), trade_signatures(&replay));

        let cursor = first.last().expect("first page has trades").ts_event;
        let second = bounded_trades("KEUR", Some(cursor), None, 8, None);

        assert_eq!(
            second.first().expect("second page has trades").ts_event,
            cursor
        );
        assert!(second.iter().skip(1).all(|trade| trade.ts_event > cursor));
        assert_ne!(trade_signatures(&first), trade_signatures(&second));
    }

    #[test]
    fn out_of_range_regime_replays_clean() {
        let clean = bounded_trades("KEUR", Some(86_401_000_000_000), None, 8, None);
        let invalid =
            parse_history_regime(Some(r#"{"type":"LiquidityDrought","thin_factor":0.5}"#));
        assert_eq!(invalid, None);
        let fallback = bounded_trades("KEUR", Some(86_401_000_000_000), None, 8, invalid);

        assert_eq!(trade_signatures(&clean), trade_signatures(&fallback));
    }

    #[test]
    fn subscribe_out_of_range_regime_is_dropped_to_clean() {
        let regime =
            validate_regime_or_clean(Some(MarketRegime::LiquidityDrought { thin_factor: 0.5 }));

        assert_eq!(regime, None);
    }

    #[test]
    fn malformed_history_regime_replays_clean() {
        let regime = parse_history_regime(Some(r#"{"type":"Bogus"}"#));

        assert_eq!(regime, None);
    }

    fn trade_signatures(trades: &[TradeTick]) -> Vec<(String, String, String, u64)> {
        trades
            .iter()
            .map(|trade| {
                (
                    trade.symbol.clone(),
                    trade.price.to_string(),
                    trade.size.to_string(),
                    trade.ts_event,
                )
            })
            .collect()
    }

    #[test]
    fn dedup_symbols_sorts_and_dedups() {
        assert_eq!(
            dedup_symbols(vec!["B".into(), "A".into(), "B".into()]),
            vec!["A".to_string(), "B".to_string()],
        );
    }

    #[test]
    fn normalize_limit_defaults_and_clamps() {
        assert_eq!(normalize_limit(None), MAX_HISTORY_LIMIT);
        assert_eq!(normalize_limit(Some(0)), 0);
        assert_eq!(normalize_limit(Some(10)), 10);
        assert_eq!(
            normalize_limit(Some(MAX_HISTORY_LIMIT + 5)),
            MAX_HISTORY_LIMIT
        );
    }

    #[test]
    fn zero_limit_yields_empty_page() {
        assert!(bounded_trades("KEUR", None, None, normalize_limit(Some(0)), None).is_empty());
    }

    /// A replay drains into the channel and is cancellable + joinable. Pins E.7:
    /// once cancelled, the thread exits promptly and the handle joins clean (no
    /// detached thread left parked).
    #[tokio::test]
    async fn replay_is_cancellable_and_joins() {
        let cfg = Config {
            speed: 0.0,
            gap_cap_ms: 0,
        };
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = spawn_replay(
            "KEUR".to_string(),
            None,
            None,
            &cfg,
            tx,
            Arc::clone(&cancel),
        );
        // First tick arrives, confirming the stream is live and feeding `KEUR`.
        let first = rx.recv().await.expect("a tick");
        assert!(matches!(
            first,
            ServerMessage::Trade(_) | ServerMessage::Quote(_)
        ));

        // Cancel + join must complete: the thread, even if parked behind the
        // bounded channel, observes the flag within one send-poll and exits.
        quiesce_replay(Replay { cancel, handle }).await;
    }

    /// Re-subscribing a symbol whose stream is parked behind a full channel still
    /// quiesces promptly. Drives the E.6 seam: the old stream is gone before the
    /// caller would spawn its replacement.
    #[tokio::test]
    async fn parked_replay_quiesces_promptly() {
        let cfg = Config {
            speed: 0.0,
            gap_cap_ms: 0,
        };
        // Capacity 1 so the generator fills the channel and parks in
        // `send_cancellable` almost immediately, never draining.
        let (tx, _rx) = mpsc::channel::<ServerMessage>(1);
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = spawn_replay(
            "KEUR".to_string(),
            None,
            None,
            &cfg,
            tx,
            Arc::clone(&cancel),
        );

        let started = Instant::now();
        quiesce_replay(Replay { cancel, handle }).await;
        // A few send-poll intervals of slack; a plain `blocking_send` would have
        // parked forever here because `_rx` never drains.
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "parked replay took {:?} to quiesce",
            started.elapsed(),
        );
    }
}
