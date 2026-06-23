//! Ignored havoc tests for the mogwai adapter.
//!
//! These bind a real TCP listener and exercise behavior that unit tests cannot
//! reach: server-side divergence shipping and client-side inbound corruption.

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use mogwai_adapter::{
    MOGWAI_VENUE, MogwaiDataClient, MogwaiDataClientConfig, MogwaiExecClientConfig,
    MogwaiExecutionClient,
};
use mogwai_protocol::{
    ClientHavoc, ConnHavoc, HavocLatency, HavocSpec, TransportProfile, control::Divergence,
};
use nautilus_common::{
    cache::Cache,
    clients::{DataClient, ExecutionClient},
    clock::TestClock,
    factories::OrderFactory,
    live::runner::{replace_data_event_sender, replace_exec_event_sender},
    messages::{DataEvent, ExecutionEvent, data::SubscribeTrades, execution::SubmitOrder},
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    data::Data,
    enums::{AccountType, OmsType, OrderSide, TimeInForce},
    events::OrderEventAny,
    identifiers::{ClientId, ClientOrderId, InstrumentId, StrategyId, Symbol, TraderId, Venue},
    orders::Order,
    types::{Price, Quantity},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::{UnboundedReceiver, unbounded_channel},
};
use tokio_tungstenite::tungstenite::Message;

const INSTRUMENTS_JSON: &str = r#"[{"symbol":"BTCUSDT","base":"BTC","quote":"USDT","price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;

#[derive(Default)]
struct StubState {
    control_hits: AtomicUsize,
    control_bodies: Mutex<Vec<String>>,
    ws_trades: Mutex<Vec<String>>,
    /// WS upgrade attempts (handshakes the stub started serving). Used by the
    /// idle-reconnect and max-attempts tests to count (re)connections.
    ws_handshakes: AtomicUsize,
    /// WS `Ping` frames received from the client (heartbeat probes).
    ws_pings: AtomicUsize,
    /// Timestamps of each HTTP request to `/orders` and `/trades`. The quota
    /// tests assert the gaps between consecutive entries.
    http_request_times: Mutex<Vec<Instant>>,
    /// When true, `serve_ws` drops the connection before completing the
    /// WebSocket upgrade, modelling a venue that refuses the socket. The
    /// handshake is still counted so the attempt-cap test can pin the count.
    refuse_ws: AtomicBool,
    /// When true, the order POST handler accepts the request but never replies,
    /// so the client's per-request timeout must turn the order into a reject.
    hang_orders: AtomicBool,
}

async fn run_stub(listener: TcpListener, state: Arc<StubState>) {
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
    } else if path.starts_with("/instruments") {
        respond_json(stream, "200 OK", INSTRUMENTS_JSON).await;
    } else if path.starts_with("/trades") {
        state
            .http_request_times
            .lock()
            .expect("http request times mutex")
            .push(Instant::now());
        respond_json(stream, "200 OK", "[]").await;
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

async fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
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

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim())
        })
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

async fn respond_json(stream: &mut TcpStream, status: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    drop(stream.write_all(response.as_bytes()).await);
    drop(stream.flush().await);
}

async fn serve_ws(stream: &mut TcpStream, head: String, state: Arc<StubState>) {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    state.ws_handshakes.fetch_add(1, Ordering::Relaxed);

    // Model a venue that refuses the socket: the TCP dial succeeded (counted
    // above) but the WebSocket upgrade never completes, so the client treats
    // the dial as failed and backs off into its reconnect loop.
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
            Message::Text(text) if text.contains("Subscribe") => {
                let trades = state.ws_trades.lock().expect("ws trades mutex").clone();
                for trade in trades {
                    drop(ws.send(Message::Text(trade.into())).await);
                }
            }
            _ => {}
        }
    }
}

async fn bound_stub(state: Arc<StubState>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(run_stub(listener, state));
    format!("ws://127.0.0.1:{port}")
}

fn instrument_id() -> InstrumentId {
    InstrumentId::new(Symbol::from("BTCUSDT"), Venue::from("MOGWAI"))
}

fn trade_json(ts_event: u64, price: &str) -> String {
    format!(
        r#"{{"type":"Trade","symbol":"BTCUSDT","price":"{price}","size":"1","aggressor":"Buyer","ts_event":{ts_event}}}"#
    )
}

async fn subscribed_data_client(
    state: Arc<StubState>,
    havoc: Option<HavocSpec>,
) -> UnboundedReceiver<DataEvent> {
    let base_url = bound_stub(state).await;
    let (sink_tx, sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url,
        havoc,
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    client
        .subscribe_trades(SubscribeTrades::new(
            instrument_id(),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("subscribe trades");
    sink_rx
}

fn data_havoc(client: ClientHavoc) -> HavocSpec {
    HavocSpec {
        client,
        server: Vec::new(),
        data: None,
        conn: ConnHavoc::default(),
    }
}

fn conn_havoc(conn: ConnHavoc) -> HavocSpec {
    HavocSpec {
        conn,
        ..HavocSpec::default()
    }
}

/// Builds, starts, connects and subscribes a `WsStreaming` data client against
/// the given stub, returning the live client (kept alive by the caller so its
/// reconnect task is not detached) and its event sink. The stub may serve no
/// trades, in which case the socket is application-silent.
async fn connect_data_client(
    base_url: String,
    havoc: Option<HavocSpec>,
) -> (MogwaiDataClient, UnboundedReceiver<DataEvent>) {
    let (sink_tx, sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url,
        havoc,
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    client
        .subscribe_trades(SubscribeTrades::new(
            instrument_id(),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("subscribe trades");
    (client, sink_rx)
}

async fn next_trade(rx: &mut UnboundedReceiver<DataEvent>) -> nautilus_model::data::TradeTick {
    let event = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("data event arrives")
        .expect("sink open");
    match event {
        DataEvent::Data(Data::Trade(trade)) => trade,
        other => panic!("expected trade event, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn ships_server_havoc() {
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    let havoc = HavocSpec {
        client: ClientHavoc::default(),
        server: vec![
            Divergence::RejectNextSubmit {
                reason: "nope".into(),
            },
            Divergence::GoDark { ms: 25 },
        ],
        data: None,
        conn: ConnHavoc::default(),
    };

    let cache = Rc::new(RefCell::new(Cache::default()));
    let exec_config = MogwaiExecClientConfig {
        base_url: base_url.clone(),
        transport_profile: TransportProfile::HttpOrders,
        havoc: Some(havoc.clone()),
        ..MogwaiExecClientConfig::default()
    };
    let core = ExecutionClientCore::new(
        TraderId::from("MOGWAI-001"),
        ClientId::from("MOGWAI-EXEC"),
        *MOGWAI_VENUE,
        OmsType::Netting,
        exec_config.account_id,
        AccountType::Cash,
        None,
        cache,
    );
    let mut exec_client = MogwaiExecutionClient::new(core, exec_config).expect("client builds");
    exec_client.start().expect("start exec client");
    exec_client
        .connect()
        .await
        .expect("exec connect ships havoc");

    assert_eq!(state.control_hits.load(Ordering::Relaxed), 2);
    let (saw_reject, saw_dark) = {
        let bodies = state.control_bodies.lock().expect("control bodies mutex");
        (
            bodies.iter().any(|body| body.contains("RejectNextSubmit")),
            bodies.iter().any(|body| body.contains("GoDark")),
        )
    };
    assert!(saw_reject);
    assert!(saw_dark);

    let data_config = MogwaiDataClientConfig {
        base_url,
        transport_profile: TransportProfile::HttpPolling,
        havoc: Some(havoc),
    };
    let mut data_client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), data_config).expect("client builds");
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    data_client.start().expect("start data client");
    data_client
        .connect()
        .await
        .expect("data connect does not ship");

    assert_eq!(state.control_hits.load(Ordering::Relaxed), 2);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_latency_delays_inbound_event() {
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let delay = Duration::from_millis(50);
    let havoc = data_havoc(ClientHavoc {
        latency: Some(HavocLatency {
            base_nanos: 20_000_000,
            data_nanos: 30_000_000,
            ..HavocLatency::default()
        }),
        ..ClientHavoc::default()
    });

    // The clock starts before connect: the reader task that owns the latency
    // sleep is not spawned until inside `connect`, so its timer cannot begin
    // before this instant and the lower bound stays sound. The generous upper
    // bound (the spec asks for one) catches a runaway delay without coupling to
    // the unbounded connect/subscribe handshake time folded into the interval.
    let start = Instant::now();
    let mut rx = subscribed_data_client(state, Some(havoc)).await;
    let _trade = next_trade(&mut rx).await;

    let elapsed = start.elapsed();
    assert!(
        elapsed >= delay,
        "inbound trade arrived before the composed delay"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "inbound trade was delayed far beyond the composed latency"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_drop_prob_one_drops_all() {
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let havoc = data_havoc(ClientHavoc {
        drop_prob: 1.0,
        seed: Some(1),
        ..ClientHavoc::default()
    });

    let mut rx = subscribed_data_client(state, Some(havoc)).await;

    let result = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
    assert!(result.is_err(), "dropped trade must not reach the sink");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_duplicate_prob_one_doubles() {
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let havoc = data_havoc(ClientHavoc {
        duplicate_prob: 1.0,
        seed: Some(1),
        ..ClientHavoc::default()
    });

    let mut rx = subscribed_data_client(state, Some(havoc)).await;

    let first = next_trade(&mut rx).await;
    let second = next_trade(&mut rx).await;
    assert_eq!(first.ts_event, second.ts_event);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_clean_is_passthrough() {
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));

    let mut rx = subscribed_data_client(state, None).await;

    let trade = next_trade(&mut rx).await;
    assert_eq!(trade.ts_event, UnixNanos::from(10));
    let result = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
    assert!(result.is_err(), "clean path emits exactly one trade");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_reorder_swaps_adjacent() {
    let state = Arc::new(StubState::default());
    {
        let mut trades = state.ws_trades.lock().expect("ws trades mutex");
        trades.push(trade_json(10, "100.00"));
        trades.push(trade_json(20, "101.00"));
    }
    let havoc = data_havoc(ClientHavoc {
        reorder_prob: 1.0,
        seed: Some(1),
        ..ClientHavoc::default()
    });

    let mut rx = subscribed_data_client(state, Some(havoc)).await;

    let first = next_trade(&mut rx).await;
    let second = next_trade(&mut rx).await;
    assert_eq!(first.ts_event, UnixNanos::from(20));
    assert_eq!(second.ts_event, UnixNanos::from(10));
}

/// Waits up to `timeout` for `count` to reach at least `target`, polling on a
/// short interval. Returns the final observed value.
async fn wait_for_at_least(count: &AtomicUsize, target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        let value = count.load(Ordering::Relaxed);
        if value >= target || Instant::now() >= deadline {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_idle_timeout_triggers_reconnect() {
    // Silent server: the socket opens but never sends an application frame, so
    // the idle clock must fire and force a reconnect. The stub accepts each
    // fresh dial, so the handshake count climbs past one.
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, _rx) = connect_data_client(
        base_url,
        Some(conn_havoc(ConnHavoc {
            idle_timeout_ms: 100,
            reconnect_delay_initial_ms: 50,
            reconnect_delay_max_ms: 50,
            ..ConnHavoc::default()
        })),
    )
    .await;

    let handshakes = wait_for_at_least(&state.ws_handshakes, 2, Duration::from_secs(2)).await;
    assert!(
        handshakes >= 2,
        "idle timeout did not trigger a reconnect (handshakes={handshakes})"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_reconnect_respects_max_attempts() {
    // The stub refuses the WebSocket upgrade on every dial. With a three-attempt
    // cap the client must dial exactly three times (one initial plus two
    // retries) and then give up disconnected - never a fourth.
    let state = Arc::new(StubState::default());
    state.refuse_ws.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url,
        havoc: Some(conn_havoc(ConnHavoc {
            reconnect_max_attempts: Some(3),
            reconnect_delay_initial_ms: 30,
            reconnect_delay_max_ms: 30,
            ..ConnHavoc::default()
        })),
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    // The dials all fail, so the loop exhausts the cap and the connect helper's
    // readiness wait never succeeds; bound the await rather than block on it.
    drop(tokio::time::timeout(Duration::from_secs(2), client.connect()).await);

    // Give any erroneous fourth dial time to land before pinning the count.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let handshakes = state.ws_handshakes.load(Ordering::Relaxed);
    assert_eq!(
        handshakes, 3,
        "max-attempts cap admitted the wrong number of dials"
    );
    assert!(
        client.is_disconnected(),
        "client must end disconnected once attempts are exhausted"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_heartbeat_pings_when_enabled() {
    // A live but silent socket: with the heartbeat enabled the client must emit
    // WS Ping frames the stub observes, even though no application data flows.
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, _rx) = connect_data_client(
        base_url,
        Some(conn_havoc(ConnHavoc {
            heartbeat_interval_ms: 50,
            ..ConnHavoc::default()
        })),
    )
    .await;

    let pings = wait_for_at_least(&state.ws_pings, 1, Duration::from_secs(2)).await;
    assert!(
        pings >= 1,
        "heartbeat did not send a Ping within the window (pings={pings})"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_clean_default_is_single_connection() {
    // The honest default transport against a stub that serves one trade and
    // holds the socket open: exactly one message, exactly one connection - no
    // spurious reconnect from the default reconnect-on-EOF loop.
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, mut rx) = connect_data_client(base_url, None).await;

    let trade = next_trade(&mut rx).await;
    assert_eq!(trade.ts_event, UnixNanos::from(10));
    let result = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
    assert!(result.is_err(), "clean default emits exactly one trade");
    assert_eq!(
        state.ws_handshakes.load(Ordering::Relaxed),
        1,
        "clean default must not reconnect a held-open socket"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_http_quota_spaces_data_requests() {
    // The data client polls trades over HTTP; the quota gate must space those
    // GETs at least 1/n seconds apart. The quota is per-client (Section 3.2),
    // so this exercises the data client's own throttle.
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url,
        transport_profile: TransportProfile::HttpPolling,
        havoc: Some(conn_havoc(ConnHavoc {
            max_requests_per_second: Some(2),
            ..ConnHavoc::default()
        })),
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    client
        .subscribe_trades(SubscribeTrades::new(
            instrument_id(),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("subscribe trades");

    // Let the poll loop issue several /trades GETs through the quota gate.
    let times = loop {
        let times = state
            .http_request_times
            .lock()
            .expect("http request times mutex")
            .clone();
        if times.len() >= 3 {
            break times;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    for pair in times.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(
            gap >= Duration::from_millis(450),
            "data HTTP requests spaced only {gap:?} apart under a 2/sec quota"
        );
    }
}

fn cached_order(cache: &Rc<RefCell<Cache>>) -> nautilus_model::orders::OrderAny {
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

async fn next_exec_event(rx: &mut UnboundedReceiver<ExecutionEvent>) -> ExecutionEvent {
    tokio::time::timeout(Duration::from_secs(3), rx.recv())
        .await
        .expect("execution event arrives")
        .expect("sink open")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_http_request_timeout_rejects_order() {
    // The order POST is accepted but never answered. With a short per-request
    // timeout the dispatch must elapse and surface the order as a reject via the
    // existing reject path, rather than hanging on the default 30s timeout.
    let state = Arc::new(StubState::default());
    state.hang_orders.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_order(&cache);
    let config = MogwaiExecClientConfig {
        base_url,
        transport_profile: TransportProfile::HttpOrders,
        havoc: Some(conn_havoc(ConnHavoc {
            request_timeout_secs: 1,
            ..ConnHavoc::default()
        })),
        ..MogwaiExecClientConfig::default()
    };
    let core = ExecutionClientCore::new(
        config.trader_id,
        ClientId::from("MOGWAI-EXEC"),
        *MOGWAI_VENUE,
        OmsType::Netting,
        config.account_id,
        config.account_type,
        None,
        cache,
    );
    let mut client = MogwaiExecutionClient::new(core, config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    client
        .submit_order(SubmitOrder::new(
            TraderId::from("MOGWAI-001"),
            Some(ClientId::from("MOGWAI-EXEC")),
            StrategyId::from("S-001"),
            instrument_id(),
            order.client_order_id(),
            order.init_event().clone(),
            None,
            None,
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("submit order");

    // The submitted event arrives first, then the timeout-driven reject.
    assert!(matches!(
        next_exec_event(&mut sink_rx).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(
        matches!(
            next_exec_event(&mut sink_rx).await,
            ExecutionEvent::Order(OrderEventAny::Rejected(_))
        ),
        "a request that timed out must surface as an order reject"
    );
}
