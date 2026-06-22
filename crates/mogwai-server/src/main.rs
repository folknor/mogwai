//! mogwai fake-broker server.
//!
//! Hosts the native JSON-over-WS gateway (`/ws`) that the broadarrow adapter
//! connects to, plus an out-of-band control plane (`/control/divergence`) for
//! arming deterministic divergences from tests. The exchange logic lives in
//! [`mogwai_engine`]; market data is replayed from the Kraken dump by
//! [`mogwai_data`]; this binary owns sockets, the clock and replay pacing.

use std::{
    path::PathBuf,
    sync::Arc,
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
use mogwai_data::{KrakenCsvSource, MergeSource, TickEvent, TickSource};
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
        Self { data_dir, speed }
    }
}

#[derive(Clone)]
struct AppState {
    engine: Arc<Mutex<Engine>>,
    cfg: Config,
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
    state.engine.lock().await.arm(div);
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

    let writer = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
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
            ClientMessage::Subscribe { symbols } => {
                spawn_replay(symbols, state.cfg.clone(), tx.clone());
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

    drop(tx);
    if let Err(e) = writer.await {
        tracing::warn!(%e, "writer task did not shut down cleanly");
    }
}

/// Stream historical trades for `symbols` as market data into `tx`.
///
/// CSV reads are blocking, so the replay runs on a dedicated OS thread and uses
/// [`mpsc::Sender::blocking_send`] - which also applies backpressure, pacing the
/// reader to however fast the client drains.
fn spawn_replay(symbols: Vec<String>, cfg: Config, tx: mpsc::Sender<ServerMessage>) {
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
        tracing::info!(?symbols, "replay started");

        let mut merged = MergeSource::new(sources);
        let mut prev_ts: Option<u64> = None;
        while let Some(tick) = merged.next_tick() {
            if cfg.speed > 0.0 {
                if let Some(prev) = prev_ts {
                    let gap_ns = tick.ts_event().saturating_sub(prev);
                    let wait = (gap_ns as f64 / cfg.speed) as u64;
                    if wait > 0 {
                        std::thread::sleep(Duration::from_nanos(wait));
                    }
                }
                prev_ts = Some(tick.ts_event());
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
