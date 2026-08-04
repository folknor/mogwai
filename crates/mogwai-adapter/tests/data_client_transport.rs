// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end transport test for the mogwai `DataClient`.
//!
//! Exercises the public adapter surface against the shared self-contained stub
//! (`tests/common`) that speaks the mogwai HTTP (`/instruments`, `/clock`,
//! `/trades`) and WebSocket (`/ws`) protocol on a single bound port. It
//! installs its own egress sink via the nautilus runner thread-local, then
//! asserts that:
//!
//! - the venue's unbidden tape reaches the sink as a `DataEvent::Data(
//!   Data::Trade)` with every field round-tripped, once the local subscription
//!   is recorded,
//! - a `request_trades` fetch returns a matching `DataResponse::Trades` with the
//!   two distinct trades in order,
//! - a failed or off-tape history fetch still RESOLVES the nautilus request
//!   rather than hanging it, and
//! - a subscribe for an instrument this run does not serve is refused locally
//!   and loudly.
//!
//! The `fixture(transport_profile)` parameterization is gone with
//! `TransportProfile` itself: there is one carrier, so there is nothing to
//! parameterize over, and the polling case that only existed under
//! `HttpPolling` was deleted with the profile rather than ported.
//!
//! Marked `#[ignore]` because it binds a real TCP listener, which the sandbox
//! may refuse; `brokkr check --gate` and the focused runner both include it.

mod common;

use std::{sync::Arc, time::Duration};

use common::{
    StubState, bound_stub, instrument_id, next_data_event, next_non_instrument_data_event,
};
use mogwai_adapter::{MogwaiDataClient, MogwaiDataClientConfig};
use nautilus_common::{
    clients::DataClient,
    live::runner::replace_data_event_sender,
    messages::{
        DataEvent,
        data::{DataResponse, SubscribeQuotes, SubscribeTrades},
    },
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    data::Data,
    enums::AggressorSide,
    identifiers::{AccountId, ClientId},
    types::{Price, Quantity},
};
use tokio::sync::mpsc::unbounded_channel;

const TRADES_JSON: &str = r#"[{"symbol":"BTCUSDT","price":"100.00","size":"1","aggressor":"Buyer","ts_event":10},{"symbol":"BTCUSDT","price":"101.00","size":"2","aggressor":"Seller","ts_event":20}]"#;

fn bar_type() -> nautilus_model::data::BarType {
    nautilus_model::data::BarType::new(
        instrument_id(),
        nautilus_model::data::BarSpecification::new(
            1,
            nautilus_model::enums::BarAggregation::Minute,
            nautilus_model::enums::PriceType::Last,
        ),
        nautilus_model::enums::AggregationSource::External,
    )
}

fn data_client(base_url: String) -> MogwaiDataClient {
    let config = MogwaiDataClientConfig {
        // Stated, not defaulted: `account_id` defaults to a placeholder the
        // validator refuses, so a data socket can never bind an account nobody
        // chose. It is a nautilus-side label with no venue meaning now - the
        // venue has one ledger and ignores it - but the loud refusal still
        // earns its place.
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        ..MogwaiDataClientConfig::default()
    };
    MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds")
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn subscribe_and_request_drive_data_events() {
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(r#"{"type":"Trade","symbol":"BTCUSDT","price":"100.00","size":"1","aggressor":"Buyer","ts_event":10}"#.to_string());
    *state.trades_body.lock().expect("trades body mutex") = Some(TRADES_JSON.to_string());
    let base_url = bound_stub(Arc::clone(&state)).await;

    // Install our own egress sink on this thread; start() grabs it.
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");

    // The subscribe is satisfied LOCALLY: no frame reaches the venue, which
    // pushes its one run's tape whether or not anybody asked. What the call
    // still does is gate forwarding, so the tape below only reaches the sink
    // because of it.
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

    // The stub pushes a trade unbidden; it must reach the sink with every field
    // round-tripped (a wrong scaling, flipped aggressor or dropped ts would
    // still be a Trade variant but is caught here). The connect-time instrument
    // prologue is skipped (see next_non_instrument_data_event).
    let timeout = Duration::from_secs(5);
    match next_non_instrument_data_event(&mut sink_rx, timeout).await {
        DataEvent::Data(Data::Trade(trade)) => {
            assert_eq!(trade.instrument_id, instrument_id());
            assert_eq!(trade.price, Price::from("100.00"));
            assert_eq!(trade.size, Quantity::from("1"));
            assert_eq!(trade.aggressor_side, AggressorSide::Buyer);
            assert_eq!(trade.ts_event, UnixNanos::from(10));
        }
        other => panic!("expected a trade data event, got {other:?}"),
    }

    // request_trades drives an HTTP fetch returning the two distinct trades, in
    // order. Asserting only `len == 2` would pass two copies of one wrong trade.
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
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            assert_eq!(resp.data.len(), 2, "stub returns two trades");
            let first = &resp.data[0];
            assert_eq!(first.price, Price::from("100.00"));
            assert_eq!(first.size, Quantity::from("1"));
            assert_eq!(first.aggressor_side, AggressorSide::Buyer);
            assert_eq!(first.ts_event, UnixNanos::from(10));
            let second = &resp.data[1];
            assert_eq!(second.price, Price::from("101.00"));
            assert_eq!(second.size, Quantity::from("2"));
            assert_eq!(second.aggressor_side, AggressorSide::Seller);
            assert_eq!(second.ts_event, UnixNanos::from(20));
            assert!(
                first.ts_event < second.ts_event,
                "trades must arrive in ascending ts order"
            );
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_host_subscribing_quotes_after_connect_receives_the_book_immediately() {
    let state = Arc::new(StubState::default());
    state.ws_trades.lock().expect("ws frames mutex").push(
        r#"{"type":"Quote","symbol":"BTCUSDT","bid_px":"99.00","ask_px":"100.00","bid_sz":"2","ask_sz":"3","ts_event":9}"#.to_string(),
    );
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");
    tokio::time::sleep(Duration::from_millis(50)).await;

    client
        .subscribe_quotes(SubscribeQuotes::new(
            instrument_id(),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("subscribe quotes");

    match next_non_instrument_data_event(&mut sink_rx, Duration::from_secs(5)).await {
        DataEvent::Data(Data::Quote(quote)) => {
            assert_eq!(quote.instrument_id, instrument_id());
            assert_eq!(quote.bid_price, Price::from("99.00"));
            assert_eq!(quote.ask_price, Price::from("100.00"));
            assert_eq!(quote.bid_size, Quantity::from("2"));
            assert_eq!(quote.ask_size, Quantity::from("3"));
            assert_eq!(quote.ts_event, UnixNanos::from(9));
        }
        other => panic!("expected an immediate cached quote, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn an_unrepresentable_quote_is_dropped_not_panicked() {
    let state = Arc::new(StubState::default());
    {
        let mut frames = state.ws_trades.lock().expect("ws frames mutex");
        frames.push(
            r#"{"type":"Quote","symbol":"BTCUSDT","bid_px":"100000000000000000000","ask_px":"100000000000000000001","bid_sz":"2","ask_sz":"3","ts_event":8}"#.to_string(),
        );
        frames.push(
            r#"{"type":"Quote","symbol":"BTCUSDT","bid_px":"99.00","ask_px":"100.00","bid_sz":"2","ask_sz":"3","ts_event":9}"#.to_string(),
        );
    }
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client
        .subscribe_quotes(SubscribeQuotes::new(
            instrument_id(),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("subscribe quotes before connect");
    client
        .connect()
        .await
        .expect("connect survives invalid quote");
    match next_non_instrument_data_event(&mut sink_rx, Duration::from_secs(5)).await {
        DataEvent::Data(Data::Quote(quote)) => assert_eq!(quote.ts_event, UnixNanos::from(9)),
        other => panic!("the task did not survive to the valid quote: {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn request_quotes_uses_the_live_history_route() {
    use nautilus_common::messages::data::RequestQuotes;

    let state = Arc::new(StubState::default());
    *state.quotes_body.lock().expect("quotes body mutex") = Some(
        r#"[{"symbol":"BTCUSDT","bid_px":"99.00","ask_px":"100.00","bid_sz":"2","ask_sz":"3","ts_event":9}]"#.to_string(),
    );
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");
    client
        .request_quotes(RequestQuotes::new(
            instrument_id(),
            None,
            None,
            None,
            Some(ClientId::from("MOGWAI-DATA")),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("request quotes");
    loop {
        if let DataEvent::Response(DataResponse::Quotes(response)) =
            next_data_event(&mut sink_rx, Duration::from_secs(5)).await
        {
            assert_eq!(response.data.len(), 1);
            assert_eq!(response.data[0].bid_price, Price::from("99.00"));
            assert_eq!(response.data[0].ask_price, Price::from("100.00"));
            break;
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn trade_history_pages_without_duplicates_at_the_seam() {
    use nautilus_common::messages::data::RequestTrades;

    let state = Arc::new(StubState::default());
    let mut first = Vec::with_capacity(mogwai_protocol::MAX_HISTORY_LIMIT);
    for ts in 1..=mogwai_protocol::MAX_HISTORY_LIMIT as u64 {
        first.push(mogwai_protocol::TradeTick {
            symbol: "BTCUSDT".to_string(),
            price: rust_decimal::Decimal::new(10_000, 2),
            size: rust_decimal::Decimal::ONE,
            aggressor: mogwai_protocol::AggressorSide::Buyer,
            ts_event: ts,
        });
    }
    let second = vec![mogwai_protocol::TradeTick {
        symbol: "BTCUSDT".to_string(),
        price: rust_decimal::Decimal::new(10_100, 2),
        size: rust_decimal::Decimal::ONE,
        aggressor: mogwai_protocol::AggressorSide::Seller,
        ts_event: mogwai_protocol::MAX_HISTORY_LIMIT as u64 + 1,
    }];
    {
        let mut pages = state.trades_pages.lock().expect("trades pages mutex");
        pages.push_back(serde_json::to_string(&first).expect("first page json"));
        pages.push_back(serde_json::to_string(&second).expect("second page json"));
    }
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");
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

    let timeout = Duration::from_secs(10);
    loop {
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            assert_eq!(resp.data.len(), mogwai_protocol::MAX_HISTORY_LIMIT + 1);
            for pair in resp.data.windows(2) {
                assert!(pair[0].ts_event < pair[1].ts_event);
            }
            assert_eq!(
                state.trades_hits.load(std::sync::atomic::Ordering::Relaxed),
                2
            );
            break;
        }
    }
}

/// One run is one instrument, so a subscribe for any other symbol can never be
/// served. It is refused LOCALLY and loudly rather than recorded as a
/// subscription that silently never delivers - silence here would be the
/// misbinding defect this lifecycle exists to remove, in a new place.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_subscribe_for_another_instrument_is_refused_locally() {
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client
        .connect()
        .await
        .expect("connect seeds the instrument");

    let other = nautilus_model::identifiers::InstrumentId::new(
        nautilus_model::identifiers::Symbol::from("ETHUSDT"),
        *mogwai_adapter::MOGWAI_VENUE,
    );
    let err = client
        .subscribe_trades(SubscribeTrades::new(
            other,
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect_err("a symbol this run does not serve must be refused");
    let message = err.to_string();
    assert!(message.contains("ETHUSDT"), "{message}");
    assert!(message.contains("BTCUSDT"), "{message}");

    // The run's own instrument is still accepted, so the refusal is a check and
    // not a blanket failure.
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
        .expect("the run's own instrument subscribes");
}

/// A failed history fetch must still RESOLVE the nautilus request.
///
/// The failure arms of `request_trades`/`request_bars` used to log and return
/// straight out of the spawned task, so no `DataResponse` was ever emitted and
/// the request hung forever. From the consumer that is indistinguishable from a
/// dead venue: the host burns its entire warmup timeout and fails the handoff
/// with nothing to go on but a line in the worker log.
/// An empty response is the truthful answer that at least completes the
/// exchange.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn failed_history_fetch_still_answers_the_request() {
    use std::sync::atomic::Ordering;

    use nautilus_common::messages::data::{RequestBars, RequestTrades};

    let state = Arc::new(StubState::default());
    state.fail_trades.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");

    let timeout = Duration::from_secs(5);

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
        .expect("request trades dispatches");
    loop {
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            assert!(
                resp.data.is_empty(),
                "a refused fetch answers empty, not with invented trades"
            );
            break;
        }
    }

    let bar_type = bar_type();
    client
        .request_bars(RequestBars::new(
            bar_type,
            None,
            None,
            None,
            Some(ClientId::from("MOGWAI-DATA")),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("request bars dispatches");
    loop {
        if let DataEvent::Response(DataResponse::Bars(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            assert!(
                resp.data.is_empty(),
                "a refused fetch answers empty, not with invented bars"
            );
            break;
        }
    }
}

/// An OFF-TAPE window must answer too, for the same reason.
///
/// The adapter refuses a `start` below the venue's published `data_origin_ns`
/// at the request boundary. Returning that refusal to nautilus does not reach
/// the requester: `DataEngine::execute` logs a synchronous client error and
/// emits no correlated response, so the loud refusal read downstream as a hang.
///
/// The clock envelope below is the literal `/clock` text, and it carries
/// `warmup_ns` rather than the former `backfill_horizon_ns`: the rename is a
/// WIRE change, so this text and `clock_snapshot_round_trips` in
/// `mogwai-protocol` move together or one of them fails.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn off_tape_window_still_answers_the_request() {
    use nautilus_common::messages::data::{RequestBars, RequestTrades};

    const ORIGIN: u64 = 2_000_000_000_000_000_000;

    let state = Arc::new(StubState::default());
    // Publish a real clock envelope: without one the client cannot decode a
    // floor, falls back to "unknown" (0), and the off-tape guard never fires.
    *state.clock_body.lock().expect("clock body mutex") = Some(format!(
        r#"{{"sim":{{"sim_epoch_ns":0,"wall_anchor_ns":0,"speed":1.0}},"server_now_ns":{},"data_origin_ns":{ORIGIN},"warmup_ns":86400000000000}}"#,
        ORIGIN + 86_400_000_000_000
    ));
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");

    // One nanosecond below the floor: as off-tape as it gets.
    let off_tape = chrono::DateTime::from_timestamp_nanos((ORIGIN - 1) as i64);
    let timeout = Duration::from_secs(5);

    client
        .request_trades(RequestTrades::new(
            instrument_id(),
            Some(off_tape),
            None,
            None,
            Some(ClientId::from("MOGWAI-DATA")),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("an off-tape request must not error out of the handler");
    loop {
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            assert!(resp.data.is_empty(), "an off-tape window answers empty");
            break;
        }
    }

    client
        .request_bars(RequestBars::new(
            bar_type(),
            Some(off_tape),
            None,
            None,
            Some(ClientId::from("MOGWAI-DATA")),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("an off-tape request must not error out of the handler");
    loop {
        if let DataEvent::Response(DataResponse::Bars(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            assert!(resp.data.is_empty(), "an off-tape window answers empty");
            break;
        }
    }

    // Proves the empty responses above came from the off-tape GUARD and not
    // merely from a stub that happens to serve no trades: a refused window is
    // never fetched at all. Without this the test passes vacuously whenever the
    // clock envelope fails to decode and the floor reads as unknown.
    assert_eq!(
        state.trades_hits.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "an off-tape window must be refused before any /trades fetch"
    );
}
