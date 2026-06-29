//! mogwai fake-broker server.
//!
//! Hosts the native JSON-over-WS gateway (`/ws`) that the broadarrow adapter
//! connects to, plus an out-of-band control plane (`/control/divergence`) for
//! arming deterministic divergences from tests. The exchange logic lives in
//! [`mogwai_engine`]; market data is synthesized from the committed fingerprint
//! by [`mogwai_data`]; this binary owns sockets, the clock and replay pacing.

mod man;
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
use clap::{Args, Parser, Subcommand};
use futures_util::{SinkExt, StreamExt};
use mogwai_data::TickEvent;
use mogwai_engine::Engine;
use mogwai_protocol::{
    ClientMessage, InstrumentDef, MAX_HISTORY_LIMIT, MarketRegime, QuoteTick, ServerMessage,
    SimClock, TradeTick, control::Divergence, validate_divergence, validate_market_regime,
};
use serde::Deserialize;
use tokio::sync::{Mutex, mpsc};

/// Replay/runtime configuration, loaded from a TOML config file at startup
/// (see `load`); never from ambient environment variables.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct Config {
    /// Simulated start instant. `0` keeps the identity wall-time clock.
    sim_epoch_ns: u64,
    /// Replay speed multiplier. `0.0` means unthrottled (stream as fast as the client
    /// drains). `1.0` is the default and paces to real wall-clock gaps; otherwise
    /// inter-tick wall delay = (tick gap) / speed.
    speed: f64,
    /// Maximum wall-clock sleep between two ticks under paced replay, in
    /// milliseconds. `0` disables the cap.
    gap_cap_ms: u64,
    /// Optional server-originated heartbeat cadence in milliseconds. `0`
    /// disables it. When enabled, each websocket session receives liveness
    /// frames that survive `StallData` but not `GoDark`.
    server_heartbeat_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sim_epoch_ns: 0,
            // Honest-by-default: wall-clock pace the generator's inter-arrival
            // gaps so a no-config server serves a realistic live feed, matching
            // the committed mogwai.toml. 0.0 remains available as an explicit
            // firehose for fast local iteration. Until the coherent simulated
            // clock lands this is the 1x baseline; afterwards it is the 1x point
            // of the acceleration axis.
            speed: 1.0,
            gap_cap_ms: 1000,
            server_heartbeat_ms: 0,
        }
    }
}

impl Config {
    /// Load run config from a TOML file. `path` is the parsed `--config <path>`
    /// argument when passed, otherwise `mogwai.toml` in the working directory.
    /// A missing file yields built-in defaults so the server still starts with
    /// no config present; a malformed file is a hard error rather than a silent
    /// fallback. Replaces the former MOGWAI_REPLAY_SPEED and MOGWAI_GAP_CAP_MS
    /// environment variables - run knobs belong in explicit input, not ambient
    /// environment.
    fn load(path: Option<std::path::PathBuf>) -> anyhow::Result<Self> {
        let path = path.unwrap_or_else(|| std::path::PathBuf::from("mogwai.toml"));
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
    /// Run clock shared with adapters over `/clock`.
    sim: SimClock,
    /// Execution-event delay in milliseconds, shared by all live writers.
    delay_ms: Arc<AtomicU64>,
    /// Sim-time unix-ns instant before which writers drop all outbound frames.
    dark_until_ns: Arc<AtomicU64>,
    /// Sim-time unix-ns instant before which writers drop market-data frames.
    stall_until_ns: Arc<AtomicU64>,
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

fn sim_now_ns(sim: SimClock) -> u64 {
    sim.sim_ns(now_ns())
}

fn sim_duration_from_millis(ms: u64) -> u64 {
    ms.saturating_mul(1_000_000)
}

/// Convert a millisecond window into an absolute sim-time unix-ns deadline.
fn window_until_ns(now: u64, ms: u64) -> u64 {
    now.saturating_add(ms.saturating_mul(1_000_000))
}

fn build_sim_clock(cfg: &Config, wall_anchor_ns: u64) -> anyhow::Result<SimClock> {
    if !cfg.speed.is_finite() {
        anyhow::bail!("speed must be finite");
    }
    if cfg.sim_epoch_ns == 0 {
        if cfg.speed != 1.0 && cfg.speed != 0.0 {
            anyhow::bail!("sim_epoch_ns must be set when speed is neither 0.0 nor 1.0");
        }
        return Ok(SimClock::identity());
    }
    if cfg.speed <= 0.0 {
        anyhow::bail!("speed must be > 0.0 when sim_epoch_ns is set");
    }
    Ok(SimClock {
        sim_epoch_ns: cfg.sim_epoch_ns,
        wall_anchor_ns,
        speed: cfg.speed,
    })
}

/// Version banner: semver plus the short git hash (with a `-dirty` suffix for an
/// unclean tree) and the UTC build time, stamped at compile time by `build.rs`.
/// e.g. `0.1.0 (abc123def 2026-06-24 12:34:56 UTC)`. Fed to clap's `--version`.
const LONG_VERSION: &str = env!("MOGWAI_LONG_VERSION");

/// The `mogwai` command line. Two explicit verbs: `serve` runs the gateway,
/// `man` renders the bundled reference docs. There is deliberately no default
/// verb - a bare `mogwai` prints help rather than silently binding a socket.
/// `--help`, `--version`/`-V` and the argument grammar are clap-provided; the
/// server's run knobs live in `mogwai.toml`, not in flags or the environment.
#[derive(Parser)]
#[command(
    name = "mogwai",
    version = LONG_VERSION,
    about = "Fake broker/exchange that drives broadarrow's live trading path",
    arg_required_else_help = true
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// The two modes: serve the gateway, or read the bundled docs.
#[derive(Subcommand)]
enum Command {
    /// Run the gateway server (binds the sockets and replays market data).
    Serve(ServeArgs),
    /// Render a bundled reference doc, or list the topics when none is given.
    Man {
        /// Reference topic to display. Omit to list the available topics.
        #[arg(value_name = "TOPIC")]
        topic: Option<man::ManTopic>,
    },
}

/// `serve` arguments: where to read run config and what address to bind.
#[derive(Args)]
struct ServeArgs {
    /// Load run config from this TOML file. Defaults to `mogwai.toml` in the
    /// working directory; a missing file falls back to built-in defaults, a
    /// malformed one is a hard error.
    #[arg(long, value_name = "PATH")]
    config: Option<std::path::PathBuf>,
    /// Address to bind the gateway to, as `host:port`. The adapter's default
    /// server URL targets `8787`, so a non-default port also needs the adapter
    /// pointed at it.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:8787")]
    addr: std::net::SocketAddr,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let serve = match Cli::parse().command {
        Command::Man { topic } => {
            man::run(topic);
            return Ok(());
        }
        Command::Serve(args) => args,
    };

    // RUST_LOG is the one deliberate exception to the no-ambient-environment
    // rule that governs run knobs (those live in mogwai.toml): log level is the
    // universally-expected env var and is not a run knob. Falls back to
    // mogwai_server=info when RUST_LOG is unset or unparseable.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mogwai_server=info".into()),
        )
        .init();

    let cfg = Config::load(serve.config)?;
    let sim = build_sim_clock(&cfg, now_ns())?;
    tracing::info!(
        sim_epoch_ns = cfg.sim_epoch_ns,
        wall_anchor_ns = sim.wall_anchor_ns,
        speed = cfg.speed,
        gap_cap_ms = cfg.gap_cap_ms,
        server_heartbeat_ms = cfg.server_heartbeat_ms,
        "config"
    );
    if !sim.is_identity() && cfg.gap_cap_ms != 0 {
        tracing::info!(
            gap_cap_ms = cfg.gap_cap_ms,
            "gap_cap_ms is ignored under simulated deadline pacing"
        );
    }

    let state = AppState {
        engine: Arc::new(Mutex::new(Engine::new())),
        cfg,
        sim,
        delay_ms: Arc::new(AtomicU64::new(0)),
        dark_until_ns: Arc::new(AtomicU64::new(0)),
        stall_until_ns: Arc::new(AtomicU64::new(0)),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/instruments", get(instruments))
        .route("/trades", get(trades))
        .route("/quotes", get(quotes))
        .route("/clock", get(clock))
        .route("/orders", post(submit_order_http))
        .route("/ws", get(ws_upgrade))
        .route("/control/divergence", post(arm_divergence))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(serve.addr).await?;
    tracing::info!(addr = %serve.addr, "mogwai listening");
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
            state.dark_until_ns.store(
                window_until_ns(sim_now_ns(state.sim), ms),
                Ordering::Relaxed,
            );
        }
        Divergence::StallData { ms } => {
            state.stall_until_ns.store(
                window_until_ns(sim_now_ns(state.sim), ms),
                Ordering::Relaxed,
            );
        }
        Divergence::ClearDivergences => {
            // Lift both server-owned temporal windows process-wide. `0` is the
            // cleared sentinel: `delay_ms == 0` skips the writer's delay sleep,
            // and `now_ns() < 0` is never true so the dark and data-stall
            // guards are off. There is no backlog to replay because gated
            // frames are dropped.
            state.delay_ms.store(0, Ordering::Relaxed);
            state.dark_until_ns.store(0, Ordering::Relaxed);
            state.stall_until_ns.store(0, Ordering::Relaxed);
        }
        // Server-ownership contract (pins B.4 / E.11): `DelayAcks`, `GoDark`,
        // `StallData`, and `ClearDivergences` are server-owned controls with no
        // synchronous engine-side trigger. The server owns them and must NEVER
        // forward them to `engine.arm()`.
        // The arms above intercept them before this catch-all, so `engine_div`
        // can only be one of the four engine-side variants. The assert makes a
        // future refactor that forwards a whole `HavocSpec.server` vec straight
        // to `engine.arm()` fail loudly rather than silently losing these knobs.
        engine_div => {
            debug_assert!(
                !matches!(
                    engine_div,
                    Divergence::DelayAcks { .. }
                        | Divergence::GoDark { .. }
                        | Divergence::StallData { .. }
                        | Divergence::ClearDivergences
                ),
                "DelayAcks/GoDark/StallData/ClearDivergences are server-owned and must not be forwarded to engine.arm()",
            );
            state.engine.lock().await.arm(engine_div);
        }
    }
    (StatusCode::ACCEPTED, String::new())
}

async fn instruments(State(state): State<AppState>) -> Json<Vec<InstrumentDef>> {
    Json(state.engine.lock().await.instrument_defs())
}

async fn clock(State(state): State<AppState>) -> Json<SimClock> {
    Json(state.sim)
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
            let events = state
                .engine
                .lock()
                .await
                .process(order_cmd, sim_now_ns(state.sim));
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
    let stall_until_ns = Arc::clone(&state.stall_until_ns);
    let sim = state.sim;

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let is_heartbeat = matches!(msg, ServerMessage::Heartbeat { .. });
            if !is_heartbeat && is_execution_event(&msg) {
                let delay = delay_ms.load(Ordering::Relaxed);
                if delay > 0 {
                    tokio::time::sleep(sim.wall_duration(sim_duration_from_millis(delay))).await;
                }
            }
            let now = sim_now_ns(sim);
            if now < dark_until_ns.load(Ordering::Relaxed) {
                continue;
            }
            if msg.is_market_data() && now < stall_until_ns.load(Ordering::Relaxed) {
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

    let heartbeat = if state.cfg.server_heartbeat_ms > 0 {
        Some(spawn_heartbeat(
            state.cfg.server_heartbeat_ms,
            state.sim,
            tx.clone(),
        ))
    } else {
        None
    };

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
                        state.sim,
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
                let events = state
                    .engine
                    .lock()
                    .await
                    .process(order_cmd, sim_now_ns(state.sim));
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
    if let Some(handle) = heartbeat {
        handle.abort();
        if let Err(e) = handle.await
            && !e.is_cancelled()
        {
            tracing::warn!(%e, "heartbeat task did not shut down cleanly");
        }
    }
    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!(%e, "writer task did not shut down cleanly");
    }
}

/// Feed per-session server liveness frames into the same channel the writer
/// gates and serializes, keeping socket writes single-owned and ordered.
fn spawn_heartbeat(
    interval_ms: u64,
    sim: SimClock,
    tx: mpsc::Sender<ServerMessage>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval =
            tokio::time::interval(sim.wall_duration(sim_duration_from_millis(interval_ms)));
        loop {
            interval.tick().await;
            if tx
                .send(ServerMessage::Heartbeat {
                    ts_event: sim_now_ns(sim),
                })
                .await
                .is_err()
            {
                break;
            }
        }
    })
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
    // Delegates to the protocol's shared classifier (`ServerMessage::category`)
    // so the server's `DelayAcks` delay path and the adapter's inbound latency
    // bucketing decide exec-vs-data from one source of truth. Execution traffic
    // is everything but market data - notably `AccountState`, an account event
    // that reports balances and positions moved by fills, which both ends now
    // agree rides the execution path.
    msg.category().is_execution()
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
    sim: SimClock,
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
            if !sim.is_identity() {
                let deadline = sim.wall_ns(tick.ts_event());
                let now = now_ns();
                if deadline > now {
                    std::thread::sleep(Duration::from_nanos(deadline - now));
                }
            } else if speed > 0.0 {
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
                sim_epoch_ns: 0,
                speed: 0.0,
                gap_cap_ms: 0,
                server_heartbeat_ms: 0,
            },
            sim: SimClock::identity(),
            delay_ms: Arc::new(AtomicU64::new(0)),
            dark_until_ns: Arc::new(AtomicU64::new(0)),
            stall_until_ns: Arc::new(AtomicU64::new(0)),
        }
    }

    #[test]
    fn sim_clock_config_rejects_two_knob_trap() {
        let cfg = Config {
            speed: 2.0,
            ..Config::default()
        };

        let err = build_sim_clock(&cfg, 123)
            .expect_err("data-only acceleration must be rejected")
            .to_string();

        assert!(err.contains("sim_epoch_ns must be set"));
    }

    #[test]
    fn sim_clock_config_builds_identity_and_accelerated_maps() {
        let mut cfg = Config::default();
        assert_eq!(
            build_sim_clock(&cfg, 123).expect("identity clock"),
            SimClock::identity()
        );

        cfg.speed = 0.0;
        assert_eq!(
            build_sim_clock(&cfg, 123).expect("legacy firehose keeps identity clock"),
            SimClock::identity()
        );

        cfg.sim_epoch_ns = 1_700_438_400_000_000_000;
        cfg.speed = 3_600.0;
        assert_eq!(
            build_sim_clock(&cfg, 123).expect("accelerated clock"),
            SimClock {
                sim_epoch_ns: 1_700_438_400_000_000_000,
                wall_anchor_ns: 123,
                speed: 3_600.0,
            }
        );

        cfg.speed = 0.0;
        assert!(build_sim_clock(&cfg, 123).is_err());
    }

    #[tokio::test]
    async fn clock_route_returns_stored_run_clock() {
        let mut state = state();
        state.sim = SimClock {
            sim_epoch_ns: 10,
            wall_anchor_ns: 20,
            speed: 30.0,
        };

        let Json(returned) = clock(State(state)).await;

        assert_eq!(
            returned,
            SimClock {
                sim_epoch_ns: 10,
                wall_anchor_ns: 20,
                speed: 30.0,
            }
        );
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

    #[test]
    fn account_state_is_delayed_as_an_execution_event() {
        // The `DelayAcks` writer path delays everything `is_execution_event`
        // accepts. `AccountState` is an account/execution event, so it must be
        // delayed alongside fills and order-lifecycle events - not treated as
        // market data the way trades and quotes are. This is the server side of
        // the shared `ServerMessage::category` classification; the adapter buckets
        // the same frame into exec latency from the same source of truth.
        let account = ServerMessage::AccountState(mogwai_protocol::AccountState {
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        });
        assert!(
            is_execution_event(&account),
            "AccountState rides the execution-delay path"
        );

        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: "1".parse().expect("decimal"),
            size: "1".parse().expect("decimal"),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert!(
            !is_execution_event(&trade),
            "trades are market data, not delayed by DelayAcks"
        );
    }

    #[test]
    fn stall_data_classifier_leaves_execution_and_heartbeat_alive() {
        let trade = ServerMessage::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: "1".parse().expect("decimal"),
            size: "1".parse().expect("decimal"),
            aggressor: mogwai_protocol::AggressorSide::NoAggressor,
            ts_event: 1,
        });
        assert!(trade.is_market_data());
        assert!(!is_execution_event(&trade));

        let accepted = ServerMessage::OrderAccepted {
            client_order_id: "O".into(),
            venue_order_id: "V".into(),
            ts_event: 1,
        };
        assert!(!accepted.is_market_data());
        assert!(is_execution_event(&accepted));

        let account = ServerMessage::AccountState(mogwai_protocol::AccountState {
            balances: Vec::new(),
            positions: Vec::new(),
            ts_event: 1,
        });
        assert!(!account.is_market_data());
        assert!(is_execution_event(&account));

        let heartbeat = ServerMessage::Heartbeat { ts_event: 1 };
        assert_eq!(heartbeat.category(), mogwai_protocol::EventKind::Data);
        assert!(!heartbeat.is_market_data());
        assert!(matches!(heartbeat, ServerMessage::Heartbeat { .. }));
    }

    #[test]
    fn window_until_ns_saturates_and_preserves_zero_window() {
        let now = 1_000_000_000;
        assert_eq!(window_until_ns(now, 0), now);
        assert_eq!(window_until_ns(now, 250), 1_250_000_000);
        assert_eq!(window_until_ns(now, u64::MAX), u64::MAX);
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
            sim_epoch_ns: 0,
            speed: 0.0,
            gap_cap_ms: 0,
            server_heartbeat_ms: 0,
        };
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let handle = spawn_replay(
            "KEUR".to_string(),
            None,
            None,
            &cfg,
            SimClock::identity(),
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
            sim_epoch_ns: 0,
            speed: 0.0,
            gap_cap_ms: 0,
            server_heartbeat_ms: 0,
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
            SimClock::identity(),
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

    /// The accelerated branch of `spawn_replay` deadline-paces against the sim
    /// clock instead of the legacy per-gap sleep. Anchoring the clock ~150 ms in
    /// the FUTURE makes the first tick's wall deadline `wall_ns(ts_event)` land
    /// at the anchor, so a correct deadline-pacer holds the very first tick for
    /// ~150 ms; a broken one (or the identity gap-pacer) would release it at
    /// once. The high speed collapses the generator's own inter-arrival gaps to
    /// near zero, isolating the assertion to the anchor delay alone. This is the
    /// only unit coverage of the deadline branch (the rest is the `--accelerated`
    /// smoke, which `brokkr check` does not run).
    #[tokio::test]
    async fn replay_deadline_paces_against_sim_clock() {
        const EPOCH: u64 = 1_900_000_000_000_000_000;
        let anchor = now_ns() + 150_000_000;
        let sim = SimClock {
            sim_epoch_ns: EPOCH,
            wall_anchor_ns: anchor,
            speed: 1_000.0,
        };
        // `gap_cap_ms`/`speed` are the identity-path knobs; under a non-identity
        // clock `spawn_replay` ignores them and takes the deadline branch.
        let cfg = Config {
            sim_epoch_ns: EPOCH,
            speed: 1_000.0,
            gap_cap_ms: 1_000,
            server_heartbeat_ms: 0,
        };
        let (tx, mut rx) = mpsc::channel::<ServerMessage>(8);
        let cancel = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let handle = spawn_replay(
            "KEUR".to_string(),
            Some(EPOCH),
            None,
            &cfg,
            sim,
            tx,
            Arc::clone(&cancel),
        );

        let first = rx.recv().await.expect("a tick");
        let elapsed = started.elapsed();
        let ts_event = match first {
            ServerMessage::Trade(t) => t.ts_event,
            ServerMessage::Quote(q) => q.ts_event,
            other => panic!("unexpected first frame: {other:?}"),
        };
        // Deadline-paced to the future anchor (generous lower bound for slow CI).
        assert!(
            elapsed >= Duration::from_millis(80),
            "first tick released after {elapsed:?}, expected deadline pacing to the anchor",
        );
        // The generator anchored on the sim epoch, so stamps ride the sim axis.
        assert!(
            ts_event >= EPOCH,
            "ts_event {ts_event} below sim epoch {EPOCH}"
        );

        quiesce_replay(Replay { cancel, handle }).await;
    }
}
