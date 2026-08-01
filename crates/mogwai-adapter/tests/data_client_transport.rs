// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end transport test for the mogwai `DataClient`.
//!
//! Exercises the public adapter surface against the shared self-contained stub
//! (`tests/common`) that speaks the mogwai HTTP (`/instruments`, `/trades`) and
//! WebSocket (`/ws`) protocol on a single bound port. It installs its own egress
//! sink via the nautilus runner thread-local, then asserts that:
//!
//! - a live `subscribe_trades` drives the stub-pushed `ServerMessage::Trade`
//!   into the sink as a `DataEvent::Data(Data::Trade)` with the right
//!   price/size/aggressor/ts,
//! - a `request_trades` fetch returns a matching `DataResponse::Trades` with the
//!   two distinct trades in order, and
//! - under the `HttpPolling` profile, a `subscribe_trades` drives a polled
//!   `GET /trades` row into the sink, the poll actually repeats, and NO `/ws`
//!   socket is ever opened.
//!
//! This is the transport-profile integration gate (the polling case is the
//! behavior neither the engine tests nor the smoke path reach). It is marked
//! `#[ignore]` because it binds a real TCP listener, which the CI sandbox may
//! refuse; run it explicitly in a socket-capable environment with
//! `brokkr test -p mogwai-adapter subscribe_and_request_drive_data_events --debug`.

mod common;

use std::{sync::Arc, time::Duration};

use common::{
    StubState, bound_stub, instrument_id, next_data_event, next_non_instrument_data_event,
};
use mogwai_adapter::{MogwaiDataClient, MogwaiDataClientConfig};
use mogwai_protocol::TransportProfile;
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
    enums::AggressorSide,
    identifiers::ClientId,
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

    let config = MogwaiDataClientConfig {
        base_url,
        ..MogwaiDataClientConfig::default()
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

    // The stub pushes a trade after the Subscribe; it must reach the sink with
    // every field round-tripped (a wrong scaling, flipped aggressor or dropped
    // ts would still be a Trade variant but is caught here). The connect-time
    // instrument prologue is skipped (see next_non_instrument_data_event).
    let timeout = Duration::from_secs(2);
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
async fn http_polling_subscribe_fetches_trades_without_ws() {
    use std::sync::atomic::Ordering;

    let state = Arc::new(StubState::default());
    // A single trade in the polled body; the polling profile fetches it over
    // `GET /trades` rather than the WS leg.
    *state.trades_body.lock().expect("trades body mutex") = Some(
        r#"[{"symbol":"BTCUSDT","price":"100.00","size":"1","aggressor":"Buyer","ts_event":10}]"#
            .to_string(),
    );
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url,
        transport_profile: TransportProfile::HttpPolling,
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");

    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect starts polling");
    // Anchor the poll cursor BELOW the stub's fixed trade: a subscribe without
    // a start anchors at sim-now (wall now, against this stub's identity
    // clock), and the cursor's rewind guard would then skip the ts_event=10
    // body forever - the stub serves it verbatim regardless of the requested
    // window, unlike the real server.
    let params: nautilus_core::Params =
        serde_json::from_value(serde_json::json!({ "start_ts": 5 })).expect("params decode");
    client
        .subscribe_trades(SubscribeTrades::new(
            instrument_id(),
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            Some(params),
        ))
        .expect("subscribe trades");

    // Skip the connect-time instrument prologue and assert the polled trade.
    let timeout = Duration::from_secs(2);
    match next_non_instrument_data_event(&mut sink_rx, timeout).await {
        DataEvent::Data(Data::Trade(trade)) => {
            assert_eq!(trade.instrument_id, instrument_id());
            assert_eq!(trade.price, Price::from("100.00"));
            assert_eq!(trade.ts_event, UnixNanos::from(10));
        }
        other => panic!("expected a polled trade data event, got {other:?}"),
    }

    // Repeated polling is the polling profile's defining behavior - a one-shot
    // poll that fired once and stopped would still deliver the first row above.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if state.trades_hits.load(Ordering::Relaxed) >= 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "polling did not repeat a second /trades GET"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    assert_eq!(
        state.ws_hits.load(Ordering::Relaxed),
        0,
        "polling must not open /ws"
    );

    client.disconnect().await.expect("disconnect stops polling");
}

/// A failed history fetch must still RESOLVE the nautilus request.
///
/// The failure arms of `request_trades`/`request_bars` used to log and return
/// straight out of the spawned task, so no `DataResponse` was ever emitted and
/// the request hung forever. From the consumer that is indistinguishable from a
/// dead venue: broadarrow burns its entire warmup timeout and dies
/// `WarmupHandoffFailed` with nothing to go on but a line in the worker log.
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

    let config = MogwaiDataClientConfig {
        base_url,
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens transports");

    let timeout = Duration::from_secs(2);

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
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn off_tape_window_still_answers_the_request() {
    use nautilus_common::messages::data::{RequestBars, RequestTrades};

    const ORIGIN: u64 = 2_000_000_000_000_000_000;

    let state = Arc::new(StubState::default());
    // Publish a real clock envelope: without one the client cannot decode a
    // floor, falls back to "unknown" (0), and the off-tape guard never fires.
    *state.clock_body.lock().expect("clock body mutex") = Some(format!(
        r#"{{"sim":{{"sim_epoch_ns":0,"wall_anchor_ns":0,"speed":1.0}},"server_now_ns":{},"data_origin_ns":{ORIGIN},"backfill_horizon_ns":86400000000000}}"#,
        ORIGIN + 86_400_000_000_000
    ));
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        base_url,
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens transports");

    // One nanosecond below the floor: as off-tape as it gets.
    let off_tape = chrono::DateTime::from_timestamp_nanos((ORIGIN - 1) as i64);
    let timeout = Duration::from_secs(2);

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
