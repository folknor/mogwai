// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Shared test-support harness for the ignored adapter integration tests.
//!
//! All three adapter test binaries (`adapter_smoke`, `data_client_transport`,
//! `havoc`) drive the public adapter path against a self-contained HTTP +
//! WebSocket stub that speaks just enough of the mogwai protocol. The stub used
//! to be copy-pasted across the three files with subtle divergence (only the
//! havoc copy parsed `Content-Length`, `respond_json` had two different
//! signatures, `serve_ws` was fixed-frame in two and data-driven in one). This
//! module is the single, most-capable variant; each test still drives its own
//! scenario by mutating the shared [`StubState`] - the one thing that
//! legitimately differs between tests.
//!
//! The WS leg matches on a parsed [`ClientMessage`] rather than a type-name
//! substring, so a wire-format change (a field rename, an envelope change) makes
//! the stub stop recognising the client's `Subscribe` / `SubmitOrder` and the
//! test fails loudly instead of passing against a broken protocol.

// A shared test-support module: every item is `pub` so the three test binaries
// can use it, but nothing is reachable outside the crate, so both lints fire
// here by construction rather than flagging real issues.
#![allow(dead_code, unreachable_pub)]

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use mogwai_adapter::MOGWAI_VENUE;
use mogwai_protocol::ClientMessage;
use nautilus_common::{
    cache::Cache,
    clock::TestClock,
    factories::OrderFactory,
    messages::{DataEvent, ExecutionEvent},
};
use nautilus_model::{
    enums::{OrderSide, TimeInForce},
    identifiers::{ClientId, ClientOrderId, InstrumentId, StrategyId, Symbol, TraderId},
    types::{Price, Quantity},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::UnboundedReceiver,
};
use tokio_tungstenite::tungstenite::Message;

/// The canonical single-instrument `/instruments` seed both ends agree on.
pub const INSTRUMENTS_JSON: &str = r#"[{"symbol":"BTCUSDT","base":"BTC","quote":"USDT","price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;

/// Test scenario state shared between the stub and the test body. A test mutates
/// the relevant fields before connecting, then reads the counters/recorded
/// bodies afterwards. Defaults model a clean, honest venue.
#[derive(Default)]
pub struct StubState {
    /// Number of `POST /control/divergence` requests served.
    pub control_hits: AtomicUsize,
    /// Raw bodies of each `/control/divergence` POST (for round-trip asserts).
    pub control_bodies: Mutex<Vec<String>>,
    /// Trade frames the WS leg pushes after a client `Subscribe`.
    pub ws_trades: Mutex<Vec<String>>,
    /// Execution frames the WS leg pushes after a client `SubmitOrder`.
    pub ws_exec_frames: Mutex<Vec<String>>,
    /// WS upgrade attempts (handshakes the stub started serving). The
    /// idle-reconnect and max-attempts tests count (re)connections with this.
    pub ws_handshakes: AtomicUsize,
    /// WS `Ping` frames received from the client (heartbeat probes).
    pub ws_pings: AtomicUsize,
    /// `Pong` frames the client returned in reply to a server `Ping`.
    pub ws_pongs: AtomicUsize,
    /// `ServerMessage` JSON the stub `Ping`s the client with, after subscribe,
    /// to exercise the inbound `Ping` -> client `Pong` reply path.
    pub ws_server_pings: AtomicUsize,
    /// JSON body of each HTTP `GET /trades` response. Defaults to `[]`.
    pub trades_body: Mutex<Option<String>>,
    /// When true, `GET /account` returns an empty account snapshot. Defaults to
    /// false so older-server compatibility remains the default stub behavior.
    pub serve_account: AtomicBool,
    /// Timestamps of each HTTP request to `/orders` and `/trades`. The quota
    /// tests assert the gaps between consecutive entries.
    pub http_request_times: Mutex<Vec<Instant>>,
    /// Count of `GET /trades` requests served (polling-repeat assertion).
    pub trades_hits: AtomicUsize,
    /// Count of WS `/ws` upgrades (the polling profile must never open one).
    pub ws_hits: AtomicUsize,
    /// When true, `serve_ws` drops the connection before completing the upgrade,
    /// modelling a venue that refuses the socket. The handshake is still counted
    /// so the attempt-cap test can pin the count.
    pub refuse_ws: AtomicBool,
    /// When true, the order POST handler accepts the request but never replies,
    /// so the client's per-request timeout must turn the order into a reject.
    pub hang_orders: AtomicBool,
    /// When set, the WS leg suppresses application frames sent within this many
    /// ms of the subscribe instant, modelling a `GoDark` blackout window.
    pub dark_ms: AtomicUsize,
    /// When true, the WS data leg closes the connection (returns, dropping the
    /// socket) right after pushing all `ws_trades` on the FIRST subscribe,
    /// modelling a clean stream end. The peer close makes the client's reader
    /// loop exit normally and run its on-disconnect flush callback - the only
    /// path that releases a message the reorder filter is holding at stream end
    /// (the flush never runs on a client-side `stop()`, which merely aborts the
    /// reader task: that is finding A.5). On any later (reconnected) subscribe
    /// the leg serves nothing, so the held trade is not replayed.
    pub close_after_trades: AtomicBool,
    /// Internal: set once the close-after-trades leg has served its one batch.
    served_once: AtomicBool,
}

/// Binds an ephemeral loopback listener, spawns the stub against it, and returns
/// the `ws://` base URL the client config consumes.
pub async fn bound_stub(state: Arc<StubState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(run_stub(listener, state));
    format!("ws://127.0.0.1:{port}")
}

/// Accepts connections forever, dispatching each to [`handle_connection`].
pub async fn run_stub(listener: TcpListener, state: Arc<StubState>) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };
        let state = Arc::clone(&state);
        tokio::spawn(async move {
            handle_connection(&mut stream, state).await;
        });
    }
}

async fn handle_connection(stream: &mut TcpStream, state: Arc<StubState>) {
    let Some((head, body)) = read_request(stream).await else {
        return;
    };
    let path = head.split_whitespace().nth(1).unwrap_or("/");

    if path.starts_with("/ws") {
        serve_ws(stream, head, state).await;
    } else if path.starts_with("/account") {
        if state.serve_account.load(Ordering::Relaxed) {
            respond_json(
                stream,
                "200 OK",
                r#"{"balances":[],"positions":[],"ts_event":0}"#,
            )
            .await;
        } else {
            respond_json(stream, "404 Not Found", "").await;
        }
    } else if path.starts_with("/instruments") {
        respond_json(stream, "200 OK", INSTRUMENTS_JSON).await;
    } else if path.starts_with("/trades") {
        state.trades_hits.fetch_add(1, Ordering::Relaxed);
        state
            .http_request_times
            .lock()
            .expect("http request times mutex")
            .push(Instant::now());
        let body = state
            .trades_body
            .lock()
            .expect("trades body mutex")
            .clone()
            .unwrap_or_else(|| "[]".to_string());
        respond_json(stream, "200 OK", &body).await;
    } else if path.starts_with("/orders") {
        state
            .http_request_times
            .lock()
            .expect("http request times mutex")
            .push(Instant::now());
        if state.hang_orders.load(Ordering::Relaxed) {
            // Accept the POST but never respond: the client's per-request
            // timeout must elapse and surface the order as a reject. Hold the
            // socket so the request does not fail with a connection reset.
            tokio::time::sleep(Duration::from_secs(30)).await;
            return;
        }
        respond_json(stream, "200 OK", "[]").await;
    } else if path.starts_with("/control/divergence") {
        state.control_hits.fetch_add(1, Ordering::Relaxed);
        state
            .control_bodies
            .lock()
            .expect("control bodies mutex")
            .push(String::from_utf8_lossy(&body).to_string());
        respond_json(stream, "202 Accepted", "").await;
    } else {
        respond_json(stream, "200 OK", "[]").await;
    }
}

/// Reads one HTTP request, honouring `Content-Length` so POST bodies (the
/// `/control/divergence` divergence payloads) are read in full rather than
/// truncated at the header boundary.
pub async fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = vec![0u8; 4096];
    let n = stream.read(&mut buf).await.ok()?;
    if n == 0 {
        return None;
    }
    let mut bytes = buf[..n].to_vec();
    let header_end = find_header_end(&bytes)?;
    let head = String::from_utf8_lossy(&bytes[..header_end]).to_string();
    let content_length = content_length(&head);
    let body_start = header_end + 4;
    while bytes.len().saturating_sub(body_start) < content_length {
        let n = stream.read(&mut buf).await.ok()?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&buf[..n]);
    }
    let body_end = body_start.saturating_add(content_length).min(bytes.len());
    Some((head, bytes[body_start..body_end].to_vec()))
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Extracts the `Content-Length` header value, defaulting to `0`.
pub fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim())
        })
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Writes a one-shot `Connection: close` JSON response with the given status.
pub async fn respond_json(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    drop(stream.write_all(response.as_bytes()).await);
    drop(stream.flush().await);
}

/// Completes a tungstenite server handshake against an already-read request
/// head, then pushes the state-driven frames. Data clients send `Subscribe` and
/// receive `ws_trades`; exec clients send `SubmitOrder` and receive
/// `ws_exec_frames`. Matching is on a parsed `ClientMessage`, not a substring.
pub async fn serve_ws(stream: &mut TcpStream, head: String, state: Arc<StubState>) {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    state.ws_handshakes.fetch_add(1, Ordering::Relaxed);
    state.ws_hits.fetch_add(1, Ordering::Relaxed);

    // Model a venue that refuses the socket: the TCP dial succeeded (counted
    // above) but the WebSocket upgrade never completes, so the client treats the
    // dial as failed and backs off into its reconnect loop.
    if state.refuse_ws.load(Ordering::Relaxed) {
        return;
    }

    let key = head
        .lines()
        .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
        .map(str::trim)
        .unwrap_or_default();
    let accept = derive_accept_key(key.as_bytes());
    let upgrade = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    if stream.write_all(upgrade.as_bytes()).await.is_err() {
        return;
    }
    drop(stream.flush().await);

    let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
        stream,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;

    use futures_util::{SinkExt, StreamExt};
    while let Some(Ok(msg)) = ws.next().await {
        match msg {
            Message::Ping(_) => {
                state.ws_pings.fetch_add(1, Ordering::Relaxed);
            }
            Message::Pong(_) => {
                state.ws_pongs.fetch_add(1, Ordering::Relaxed);
            }
            Message::Text(text) => {
                match serde_json::from_str::<ClientMessage>(&text) {
                    Ok(ClientMessage::Subscribe { .. }) => {
                        // A close-after-trades leg serves its one batch only on
                        // the first subscribe; a reconnect after the close gets
                        // an empty, silent socket so the held trade is not
                        // replayed when the client re-dials.
                        if state.close_after_trades.load(Ordering::Relaxed)
                            && state.served_once.swap(true, Ordering::Relaxed)
                        {
                            return;
                        }
                        // Optionally open the blackout window first so the
                        // suppressed-during-dark frames are dropped by the stub.
                        let dark = state.dark_ms.load(Ordering::Relaxed);
                        let frames = state.ws_trades.lock().expect("ws trades mutex").clone();
                        if dark > 0 {
                            // During the dark window the stub holds (drops) every
                            // application frame, then resumes. We model the
                            // suppression by emitting nothing for `dark` ms.
                            tokio::time::sleep(Duration::from_millis(dark as u64)).await;
                        }
                        for trade in frames {
                            drop(ws.send(Message::Text(trade.into())).await);
                        }
                        // After the data frames, optionally probe the client's
                        // inbound Ping -> Pong reply path.
                        if state.ws_server_pings.load(Ordering::Relaxed) > 0 {
                            drop(ws.send(Message::Ping(Vec::new().into())).await);
                        }
                        // A clean stream end: send a Close and return, so the
                        // client's reader loop exits normally and runs its
                        // on-disconnect flush callback (releasing any message the
                        // reorder filter is holding). See `close_after_trades`.
                        if state.close_after_trades.load(Ordering::Relaxed) {
                            drop(ws.close(None).await);
                            return;
                        }
                    }
                    Ok(ClientMessage::SubmitOrder(_)) => {
                        let frames = state
                            .ws_exec_frames
                            .lock()
                            .expect("ws exec frames mutex")
                            .clone();
                        for frame in frames {
                            drop(ws.send(Message::Text(frame.into())).await);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// The single-symbol instrument id every test trades.
pub fn instrument_id() -> InstrumentId {
    InstrumentId::new(Symbol::from("BTCUSDT"), *MOGWAI_VENUE)
}

/// A `Trade` wire frame for the WS leg.
pub fn trade_json(ts_event: u64, price: &str) -> String {
    format!(
        r#"{{"type":"Trade","symbol":"BTCUSDT","price":"{price}","size":"1","aggressor":"Buyer","ts_event":{ts_event}}}"#
    )
}

/// Seeds a limit BTCUSDT buy order `O-1` into the cache and returns it, so the
/// exec client's submit path can look it up.
pub fn cached_order(cache: &Rc<RefCell<Cache>>) -> nautilus_model::orders::OrderAny {
    let trader_id = TraderId::from("MOGWAI-001");
    let strategy_id = StrategyId::from("S-001");
    let clock = Rc::new(RefCell::new(TestClock::new()));
    let mut factory = OrderFactory::new(trader_id, strategy_id, None, None, clock, false, false);
    let order = factory.limit(
        instrument_id(),
        OrderSide::Buy,
        Quantity::from("1"),
        Price::from("100.00"),
        Some(TimeInForce::Gtc),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ClientOrderId::from("O-1")),
    );
    cache
        .borrow_mut()
        .add_order(
            order.clone(),
            None,
            Some(ClientId::from("MOGWAI-EXEC")),
            false,
        )
        .expect("cache order");
    order
}

/// Awaits the next execution event or fails after `timeout`.
pub async fn next_exec_event(
    rx: &mut UnboundedReceiver<ExecutionEvent>,
    timeout: Duration,
) -> ExecutionEvent {
    tokio::time::timeout(timeout, rx.recv())
        .await
        .expect("execution event arrives")
        .expect("sink open")
}

/// Awaits the next data event or fails after `timeout`.
pub async fn next_data_event(
    rx: &mut UnboundedReceiver<DataEvent>,
    timeout: Duration,
) -> DataEvent {
    tokio::time::timeout(timeout, rx.recv())
        .await
        .expect("data event arrives")
        .expect("sink open")
}

/// Like [`next_data_event`], but discards leading `DataEvent::Instrument`
/// frames and returns the first event that is not one.
///
/// On connect the data client publishes its seeded instrument definitions to
/// the sink before any trade or bar (`emit_seeded_instruments`), so nautilus
/// caches the instrument first. That ordering is load-bearing, not incidental:
/// broadarrow's executor refuses a bar whose instrument is absent from the
/// cache. A transport test asserting the first TRADE must therefore skip past
/// this instrument prologue rather than mistake it for the trade.
pub async fn next_non_instrument_data_event(
    rx: &mut UnboundedReceiver<DataEvent>,
    timeout: Duration,
) -> DataEvent {
    loop {
        match next_data_event(rx, timeout).await {
            DataEvent::Instrument(_) => continue,
            other => return other,
        }
    }
}
