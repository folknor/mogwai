//! End-to-end transport test for the mogwai `DataClient`.
//!
//! Exercises the public adapter surface against a self-contained stub that
//! speaks the mogwai HTTP (`/instruments`, `/trades`) and WebSocket (`/ws`)
//! protocol on a single bound port. It installs its own egress sink via the
//! nautilus runner thread-local, then asserts that:
//!
//! - a live `subscribe_trades` drives the stub-pushed `ServerMessage::Trade`
//!   into the sink as a `DataEvent::Data(Data::Trade)`, and
//! - a `request_trades` fetch returns a matching `DataResponse::Trades`.
//!
//! This is the spec's brick-2/brick-3/brick-4 integration gate. It is marked
//! `#[ignore]` because it binds a real TCP listener, which the CI sandbox may
//! refuse; run it explicitly in a socket-capable environment with
//! `brokkr test -p mogwai-adapter data_client --debug`.

use std::time::Duration;

use mogwai_adapter::{MogwaiDataClient, MogwaiDataClientConfig};
use nautilus_common::{
    clients::DataClient,
    live::runner::replace_data_event_sender,
    messages::{
        DataEvent,
        data::{DataResponse, SubscribeTrades},
    },
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    data::Data,
    identifiers::{ClientId, InstrumentId, Symbol, Venue},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::mpsc::unbounded_channel,
};
use tokio_tungstenite::tungstenite::Message;

const INSTRUMENTS_JSON: &str = r#"[{"symbol":"BTCUSDT","base":"BTC","quote":"USDT","price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;
const TRADES_JSON: &str = r#"[{"symbol":"BTCUSDT","price":"100.00","size":"1","aggressor":"Buyer","ts_event":10},{"symbol":"BTCUSDT","price":"101.00","size":"2","aggressor":"Seller","ts_event":20}]"#;

/// Serves one HTTP request or one WS upgrade per connection, on `listener`.
async fn run_stub(listener: TcpListener) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            return;
        };

        // Peek the request head so we can route by path.
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
        } else if path.starts_with("/trades") {
            respond_json(&mut stream, TRADES_JSON).await;
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

/// Completes a tungstenite server handshake against an already-read request
/// head, then pushes one `ServerMessage::Trade` after the client subscribes.
async fn serve_ws(stream: TcpStream, head: String) {
    use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

    // Re-implement the minimal server handshake by hand because the request
    // head was already consumed above.
    let key = head
        .lines()
        .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
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
    // Wait for the client's Subscribe, then push a trade frame.
    while let Some(Ok(msg)) = ws.next().await {
        if let Message::Text(text) = msg
            && text.contains("Subscribe")
        {
            let trade = r#"{"type":"Trade","symbol":"BTCUSDT","price":"100.00","size":"1","aggressor":"Buyer","ts_event":10}"#;
            drop(ws.send(Message::Text(trade.to_string().into())).await);
            break;
        }
    }
    // Keep the socket open briefly so the drain task can read.
    tokio::time::sleep(Duration::from_millis(200)).await;
}

fn instrument_id() -> InstrumentId {
    InstrumentId::new(Symbol::from("BTCUSDT"), Venue::from("MOGWAI"))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn subscribe_and_request_drive_data_events() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(run_stub(listener));

    // Install our own egress sink on this thread; start() grabs it.
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url: format!("ws://127.0.0.1:{port}"),
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");

    client.start().expect("start grabs the sink");
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

    // The stub pushes a trade after the Subscribe; it must reach the sink.
    let event = tokio::time::timeout(Duration::from_secs(2), sink_rx.recv())
        .await
        .expect("a data event arrives")
        .expect("sink open");
    assert!(
        matches!(event, DataEvent::Data(Data::Trade(t)) if t.instrument_id == instrument_id()),
        "expected a trade data event"
    );

    // request_trades drives an HTTP fetch returning a Trades response.
    use nautilus_common::messages::data::RequestTrades;
    client
        .request_trades(RequestTrades::new(
            instrument_id(),
            None,
            None,
            None,
            Some(ClientId::from("MOGWAI-DATA")),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("request trades");

    loop {
        let event = tokio::time::timeout(Duration::from_secs(2), sink_rx.recv())
            .await
            .expect("a response arrives")
            .expect("sink open");
        if let DataEvent::Response(DataResponse::Trades(resp)) = event {
            assert_eq!(resp.data.len(), 2, "stub returns two trades");
            break;
        }
    }
}
