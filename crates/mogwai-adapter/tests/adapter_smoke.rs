//! Ignored execution transport smoke test for the mogwai execution client.
//!
//! It uses a self-contained HTTP and WebSocket stub that speaks enough of the
//! mogwai protocol to verify the public adapter path: start, connect, submit,
//! drain execution events.

use std::{cell::RefCell, rc::Rc, time::Duration};

use mogwai_adapter::{MOGWAI_VENUE, MogwaiExecClientConfig, MogwaiExecutionClient};
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    clock::TestClock,
    factories::OrderFactory,
    live::runner::replace_exec_event_sender,
    messages::{ExecutionEvent, execution::SubmitOrder},
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{OmsType, OrderSide, TimeInForce},
    events::OrderEventAny,
    identifiers::{ClientId, ClientOrderId, InstrumentId, StrategyId, Symbol, TraderId},
    orders::Order,
    types::{Price, Quantity},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::unbounded_channel,
};
use tokio_tungstenite::tungstenite::Message;

const INSTRUMENTS_JSON: &str = r#"[{"symbol":"BTCUSDT","base":"BTC","quote":"USDT","price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;

async fn run_stub(listener: TcpListener) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };

        let mut buf = vec![0u8; 4096];
        let n = match stream.read(&mut buf).await {
            Ok(0) | Err(_) => continue,
            Ok(n) => n,
        };
        let head = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();

        if path.starts_with("/ws") {
            tokio::spawn(serve_ws(stream, head));
        } else if path.starts_with("/instruments") {
            respond_json(&mut stream, INSTRUMENTS_JSON).await;
        } else {
            respond_json(&mut stream, "[]").await;
        }
    }
}

async fn respond_json(stream: &mut TcpStream, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    drop(stream.write_all(response.as_bytes()).await);
    drop(stream.flush().await);
}

async fn serve_ws(stream: TcpStream, head: String) {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    let key = head
        .lines()
        .find_map(|line| line.strip_prefix("Sec-WebSocket-Key: "))
        .map(str::trim)
        .unwrap_or_default();
    let accept = derive_accept_key(key.as_bytes());
    let mut stream = stream;
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
        if let Message::Text(text) = msg
            && text.contains("SubmitOrder")
        {
            let accepted = r#"{"type":"OrderAccepted","client_order_id":"O-1","venue_order_id":"V-1","ts_event":10}"#;
            let filled = r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"1","last_px":"100.00","leaves_qty":"0","commission":"0","ts_event":11}"#;
            let account = r#"{"type":"AccountState","balances":[{"currency":"USDT","total":"9900","free":"9900","locked":"0"}],"positions":[{"symbol":"BTCUSDT","quantity":"1","avg_px":"100.00"}],"ts_event":12}"#;
            drop(ws.send(Message::Text(accepted.to_string().into())).await);
            drop(ws.send(Message::Text(filled.to_string().into())).await);
            drop(ws.send(Message::Text(account.to_string().into())).await);
            break;
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
}

fn instrument_id() -> InstrumentId {
    InstrumentId::new(Symbol::from("BTCUSDT"), *MOGWAI_VENUE)
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn adapter_submit_drives_live_exec_events() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(run_stub(listener));

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_order(&cache);
    let config = MogwaiExecClientConfig {
        base_url: format!("ws://127.0.0.1:{port}"),
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

    assert!(matches!(
        next_exec_event(&mut sink_rx).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut sink_rx).await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut sink_rx).await,
        ExecutionEvent::Order(OrderEventAny::Filled(_))
    ));
    assert!(matches!(
        next_exec_event(&mut sink_rx).await,
        ExecutionEvent::Account(_)
    ));
}

async fn next_exec_event(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<ExecutionEvent>,
) -> ExecutionEvent {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("execution event arrives")
        .expect("sink open")
}
