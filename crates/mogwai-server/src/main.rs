//! mogwai fake-broker server.
//!
//! Hosts the native JSON-over-WS gateway (`/ws`) that the broadarrow adapter
//! connects to, plus an out-of-band control plane (`/control/divergence`) for
//! arming deterministic divergences from tests. The exchange logic lives in
//! [`mogwai_engine`]; market data is replayed from the Kraken dump by
//! [`mogwai_data`]; this binary owns sockets, the clock and replay pacing.

use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
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
use mogwai_data::{
    Identity, KrakenCsvSource, MergeSource, Permutation, TickEvent, TickRuleAggressor, TickSource,
};
use mogwai_engine::Engine;
use mogwai_protocol::{ClientMessage, ServerMessage, control::Divergence};
use tokio::sync::{Mutex, mpsc};

/// Replay/runtime configuration, sourced from the environment at startup.
#[derive(Clone)]
struct Config {
    /// Directory of `<SYMBOL>.csv` Kraken pair files.
    data_dir: PathBuf,
    /// Replay speed multiplier. `0.0` ⇒ unthrottled (stream as fast as the client
    /// drains); otherwise inter-tick wall delay = (tick gap) / speed.
    speed: f64,
    /// Maximum wall-clock sleep between two ticks under paced replay, in
    /// milliseconds. `0` disables the cap.
    gap_cap_ms: u64,
    /// When set, infer each trade's aggressor side from the tick rule as ticks
    /// replay; otherwise ticks keep the dump's `NoAggressor` (the default).
    infer_aggressor: bool,
}

impl Config {
    fn from_env() -> Self {
        let data_dir = std::env::var("MOGWAI_DATA_DIR")
            .unwrap_or_else(|_| "/media/folk/Banan/Kraken_Trading_History".into())
            .into();
        let speed = std::env::var("MOGWAI_REPLAY_SPEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0.0);
        let gap_cap_ms = std::env::var("MOGWAI_GAP_CAP_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        let infer_aggressor = std::env::var("MOGWAI_INFER_AGGRESSOR")
            .is_ok_and(|v| matches!(v.trim(), "1" | "true" | "TRUE" | "yes"));
        Self {
            data_dir,
            speed,
            gap_cap_ms,
            infer_aggressor,
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
fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos() as u64
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "mogwai_server=info".into()),
        )
        .init();

    let cfg = Config::from_env();
    tracing::info!(data_dir = %cfg.data_dir.display(), speed = cfg.speed, "config");

    let state = AppState {
        engine: Arc::new(Mutex::new(Engine::new())),
        cfg,
        delay_ms: Arc::new(AtomicU64::new(0)),
        dark_until_ns: Arc::new(AtomicU64::new(0)),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
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
    match div {
        Divergence::DelayAcks { ms } => {
            state.delay_ms.store(ms, Ordering::Relaxed);
        }
        Divergence::GoDark { ms } => {
            let until = now_ns().saturating_add(ms.saturating_mul(1_000_000));
            state.dark_until_ns.store(until, Ordering::Relaxed);
        }
        engine_div => state.engine.lock().await.arm(engine_div),
    }
    StatusCode::ACCEPTED
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
    let mut replays: HashMap<Vec<String>, Arc<AtomicBool>> = HashMap::new();
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
            let payload = serde_json::to_string(&msg).expect("serialize ServerMessage");
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
            ClientMessage::Subscribe { symbols, start_ts } => {
                let key = sub_key(&symbols);
                if let Some(flag) = replays.remove(&key) {
                    flag.store(true, Ordering::Relaxed);
                }
                let cancel = Arc::new(AtomicBool::new(false));
                replays.insert(key, Arc::clone(&cancel));
                spawn_replay(symbols, start_ts, state.cfg.clone(), tx.clone(), cancel);
            }
            ClientMessage::Unsubscribe { symbols } => {
                let key = sub_key(&symbols);
                if let Some(flag) = replays.remove(&key) {
                    flag.store(true, Ordering::Relaxed);
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

    for flag in replays.values() {
        flag.store(true, Ordering::Relaxed);
    }
    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!(%e, "writer task did not shut down cleanly");
    }
}

fn sub_key(symbols: &[String]) -> Vec<String> {
    let mut key = symbols.to_vec();
    key.sort();
    key.dedup();
    key
}

fn is_execution_event(msg: &ServerMessage) -> bool {
    !matches!(msg, ServerMessage::Trade(_) | ServerMessage::Quote(_))
}

/// Stream historical trades for `symbols` as market data into `tx`.
///
/// CSV reads are blocking, so the replay runs on a dedicated OS thread and uses
/// [`mpsc::Sender::blocking_send`] - which also applies backpressure, pacing the
/// reader to however fast the client drains.
fn spawn_replay(
    symbols: Vec<String>,
    start_ts: Option<u64>,
    cfg: Config,
    tx: mpsc::Sender<ServerMessage>,
    cancel: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut sources: Vec<Box<dyn TickSource>> = Vec::new();
        for sym in &symbols {
            let path = cfg.data_dir.join(format!("{sym}.csv"));
            match KrakenCsvSource::open(&path) {
                Ok(s) => sources.push(Box::new(s)),
                Err(e) => tracing::warn!(symbol = %sym, path = %path.display(), %e, "no data file"),
            }
        }
        if sources.is_empty() {
            return;
        }
        tracing::info!(?symbols, ?start_ts, "replay started");

        let mut merged = MergeSource::starting_at(sources, start_ts);
        // Aggressor inference runs over the merged, time-ordered stream so each
        // symbol's tick rule sees its own trades in replay order. Opt-in: the
        // default is the dump's verbatim NoAggressor.
        let mut perm: Box<dyn Permutation> = if cfg.infer_aggressor {
            Box::new(TickRuleAggressor::new())
        } else {
            Box::new(Identity)
        };
        let mut prev_ts: Option<u64> = None;
        while let Some(tick) = merged.next_tick() {
            let tick = perm.apply(tick);
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if cfg.speed > 0.0 {
                if let Some(prev) = prev_ts {
                    let gap_ns = tick.ts_event().saturating_sub(prev);
                    let mut wait_ms = (gap_ns as f64 / cfg.speed) as u64 / 1_000_000;
                    if cfg.gap_cap_ms > 0 {
                        wait_ms = wait_ms.min(cfg.gap_cap_ms);
                    }
                    if wait_ms > 0 {
                        std::thread::sleep(Duration::from_millis(wait_ms));
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
            if tx.blocking_send(msg).is_err() {
                break; // client gone
            }
        }
        tracing::info!(?symbols, "replay finished");
    });
}
