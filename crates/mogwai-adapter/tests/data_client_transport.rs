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
//! may refuse; the full gate and the focused runner both include it.

mod common;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use common::{
    StubState, assert_push_gate_opened, bound_stub, instrument_id, next_data_event,
    next_non_instrument_data_event,
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
        // Stated rather than defaulted so the label these tests assert on is
        // written down here. It is BOTH a nautilus-side label and the ledger
        // named on `/ws?account=` - the two are deliberately one field, and the
        // upgrade has carried the account since the shared-venue landing.
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
    // The subscription is recorded, so the stub's tape push is released. The
    // harness used to sleep 100 ms before pushing and hope this line had run;
    // see `common::PushGate`.
    state.push_gate.open();

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
            assert_eq!(trade.aggressor_side, AggressorSide::Buy);
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
            assert_eq!(first.aggressor_side, AggressorSide::Buy);
            assert_eq!(first.ts_event, UnixNanos::from(10));
            let second = &resp.data[1];
            assert_eq!(second.price, Price::from("101.00"));
            assert_eq!(second.size, Quantity::from("2"));
            assert_eq!(second.aggressor_side, AggressorSide::Sell);
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
async fn connecting_twice_replaces_the_data_socket() {
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    let mut client = data_client(base_url);
    client.start().unwrap();
    client.connect().await.unwrap();
    client.connect().await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while state.active_ws.load(std::sync::atomic::Ordering::Relaxed) != 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "old data socket remained active"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(state.ws_hits.load(std::sync::atomic::Ordering::Relaxed), 2);
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

    // THE QUOTE MUST BE ON THE WIRE BEFORE THE SUBSCRIBE, which is the whole
    // property: a host that subscribes late still gets the book, because the
    // client caches the pre-subscription BBO. This used to be a `sleep(50ms)`
    // that was racing the harness's own 100 ms pre-push delay and therefore
    // ALWAYS too short - the test passed only because the cache does not care
    // when the quote lands, so the ordering it claims to set up was never
    // established. Now the push is released explicitly and waited for: the stub
    // stamps `ws_first_frame_at` strictly before the send, so observing it set
    // establishes the wire half of the ordering.
    //
    // WHAT IT STILL CANNOT ESTABLISH is the client half - whether the reader had
    // filed the quote in the pre-subscription cache before the call below. The
    // client exposes no observable for that, and a quote delivered LIVE to a
    // subscription that beat it satisfies the same assertion. That gap predates
    // this change and the sleep did not close it either; it is recorded rather
    // than papered over.
    state.push_gate.open();
    let deadline = Instant::now() + Duration::from_secs(2);
    while state
        .ws_first_frame_at
        .lock()
        .expect("ws first frame instant mutex")
        .is_none()
    {
        // The gate is checked before this wait reports its own timeout: a leg
        // that gave up waiting for it stamps no first-frame instant either.
        assert_push_gate_opened();
        assert!(
            Instant::now() < deadline,
            "the stub never put the cached quote on the wire"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

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
    // Subscribed BEFORE connect, so there is no ordering to arrange - the gate
    // is opened here only because the leg has frames to push and would
    // otherwise wait for a release that never comes.
    state.push_gate.open();
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
            symbol: "BTCUSDT".into(),
            price: rust_decimal::Decimal::new(10_000, 2),
            size: rust_decimal::Decimal::ONE,
            aggressor: mogwai_protocol::AggressorSide::Buyer,
            ts_event: ts,
        });
    }
    first.push(mogwai_protocol::TradeTick {
        symbol: "BTCUSDT".into(),
        price: rust_decimal::Decimal::new(10_100, 2),
        size: rust_decimal::Decimal::ONE,
        aggressor: mogwai_protocol::AggressorSide::Seller,
        ts_event: mogwai_protocol::MAX_HISTORY_LIMIT as u64 + 1,
    });
    *state.trades_tape.lock().expect("trades tape mutex") = Some(first);
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
    let delivered = loop {
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            break resp.data.clone();
        }
    };
    assert_eq!(delivered.len(), mogwai_protocol::MAX_HISTORY_LIMIT + 1);
    for pair in delivered.windows(2) {
        assert!(pair[0].ts_event < pair[1].ts_event);
    }
    // READ AFTER THE LOOP, NOT INSIDE IT, and the move is about making the
    // ordering argument visible rather than about fixing a race. `trades_hits`
    // is incremented by the stub's `/trades` handler on another task, and this
    // assertion is sound only because the response cannot exist until both
    // fetches have been served - so the count is settled by the time the
    // response is in hand. Asserting it from inside the loop stated that
    // dependency nowhere and read as a cross-task counter poked at an arbitrary
    // moment, which is the shape that IS a race everywhere else in these files.
    assert_eq!(
        state.trades_hits.load(std::sync::atomic::Ordering::Relaxed),
        2,
        "the seam is crossed in exactly two pages: one full page and its remainder"
    );
}

/// A venue whose `/clock` CANNOT BE DECODED is RETRIED and then served anyway:
/// the fallback is a fallback, never a refusal.
///
/// THIS BRANCH HAD NO DELIBERATE COVERAGE AND ENORMOUS ACCIDENTAL COVERAGE.
/// The stub's default `/clock` body used to be the catch-all `[]`, so all 57
/// connecting tests in these four binaries traversed
/// `fetch_clock_or_identity`'s retry-then-fall-back path - three attempts, two
/// 200 ms wall sleeps, INLINE IN `connect()` - and not one of them asserted
/// anything about it. That was ~400 ms per test and ~24 s of a ~40 s serial
/// sweep, the crate's largest single cost. The default now serves a real
/// envelope; this test is what keeps the fallback covered, once, on purpose,
/// and it is the only place `fail_clock` should be armed.
///
/// WHAT IS ASSERTED IS THE LADDER AND THE OUTCOME, because those are the two
/// things visible from outside. `clock_hits` pins that the client RETRIED
/// rather than committing to the identity axis on the first failure - the whole
/// reason the retry exists is that the fallback silently puts every `ts_init`,
/// havoc sleep, quota interval and backoff on the wrong time axis for the life
/// of the connection, so giving up immediately on a transient blip is the
/// defect. The connect then SUCCEEDING is the other half: an unreadable clock
/// is a fallback, never a refusal, or a venue whose clock route blipped once
/// would be unattachable.
///
/// THE UNKNOWN FLOOR IS NOT PINNED HERE, AND CANNOT BE PINNED FROM ONE TEST.
/// The fallback sets `data_origin_ns = None`; the success path against a stub
/// serving `IDENTITY_CLOCK_JSON` sets `Some(0)`; and `ensure_on_tape` only
/// bails when `start < data_origin`, where `start` is a `u64`. Nothing can
/// precede zero, so `Some(0)` and `None` are OBSERVATIONALLY IDENTICAL and a
/// window assertion on this fixture passes whatever `floor_known` is. So this
/// test asserts only what it CAN discriminate - the ladder and the non-refusal.
/// The floor contrast needs a NONZERO known floor to have a false branch at
/// all, and that is `off_tape_window_still_answers_the_request`'s job.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn an_undecodable_clock_is_retried_then_falls_back_without_refusing() {
    use nautilus_common::messages::data::RequestTrades;

    let state = Arc::new(StubState::default());
    state
        .fail_clock
        .store(true, std::sync::atomic::Ordering::Relaxed);
    // A tape whose rows sit far above zero, matching the sibling's fixture so
    // the two differ in exactly one input: whether the clock decodes.
    *state.trades_tape.lock().expect("trades tape mutex") =
        Some(vec![tape_trade(2_000_000_000_000_000_000, 10_000)]);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    // The connect completes DESPITE the unreadable clock: the fallback is the
    // point, and a client that refused here would be unable to attach to any
    // venue whose clock route blipped.
    client
        .connect()
        .await
        .expect("an unreadable clock is a fallback, not a refusal");

    // The connect is over, so the ladder is settled and this needs no poll.
    assert_eq!(
        state.clock_hits.load(std::sync::atomic::Ordering::Relaxed),
        3,
        "an unreadable clock is retried before the identity fallback commits \
         the connection to the wrong time axis"
    );

    client
        .request_trades(RequestTrades::new(
            instrument_id(),
            Some(jiff::Timestamp::from_nanosecond(1).expect("in range")),
            None,
            None,
            Some(ClientId::from("MOGWAI-DATA")),
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .expect("the request is accepted");

    let timeout = Duration::from_secs(5);
    let delivered = loop {
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, timeout).await
        {
            break resp.data.clone();
        }
    };
    // The connection is USABLE after the fallback - it reaches the wire and
    // serves rows. This is the "not a refusal" half restated at the request
    // level, not a claim about the floor.
    assert_eq!(delivered.len(), 1);
    assert_eq!(
        delivered[0].ts_event,
        UnixNanos::from(2_000_000_000_000_000_000)
    );
    assert!(
        state.trades_hits.load(std::sync::atomic::Ordering::Relaxed) >= 1,
        "the request must have reached /trades, not been answered empty at the boundary"
    );
}

fn tape_trade(ts_event: u64, price_cents: i64) -> mogwai_protocol::TradeTick {
    mogwai_protocol::TradeTick {
        symbol: "BTCUSDT".into(),
        price: rust_decimal::Decimal::new(price_cents, 2),
        size: rust_decimal::Decimal::ONE,
        aggressor: mogwai_protocol::AggressorSide::Buyer,
        ts_event,
    }
}

/// Drives one `request_trades` against a cursor-honouring tape and returns the
/// delivered rows.
///
/// THE COUNTERS MEAN "SINCE THIS CALL BEGAN", and that is the strongest thing
/// the reset can buy. This binds a NEW stub on a NEW port while taking a
/// `&Arc<StubState>` that a caller may already have used, so `trades_hits` and
/// `trades_starts` would otherwise accumulate across calls and a second call's
/// caller would assert a paging count that includes the first call's fetches.
/// Every caller happens to call it once today; that is a property of the call
/// sites, not of this helper, and it is exactly the kind of latency that turns
/// into a wrong verdict the first time someone adds a second call.
///
/// IT IS NOT ISOLATION. The previous call's stub keeps its accept loop running
/// on the SAME `Arc<StubState>` - nothing shuts it down - so a client still
/// alive from an earlier call could in principle bump these counters after the
/// reset. No caller holds one today. If one ever does, the reset stops being
/// enough and the stubs need shutting down, not the counters rescoping.
async fn request_whole_tape(
    state: &Arc<StubState>,
    tape: Vec<mogwai_protocol::TradeTick>,
) -> Vec<nautilus_model::data::TradeTick> {
    use nautilus_common::messages::data::RequestTrades;

    state
        .trades_hits
        .store(0, std::sync::atomic::Ordering::Relaxed);
    state
        .trades_starts
        .lock()
        .expect("trades starts mutex")
        .clear();
    *state.trades_tape.lock().expect("trades tape mutex") = Some(tape);
    let base_url = bound_stub(Arc::clone(state)).await;
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
    loop {
        if let DataEvent::Response(DataResponse::Trades(resp)) =
            next_data_event(&mut sink_rx, Duration::from_secs(10)).await
        {
            return resp.data.clone();
        }
    }
}

/// A full page CUT INSIDE a timestamp group is the ordinary shape of the
/// timestamp-only cursor's hazard, and the one a page-wide "every row shares a
/// timestamp" check misses entirely. The cursor may only advance onto a
/// timestamp whose rows have all been seen, so the trailing group is dropped
/// and re-requested from its own instant. Advancing past the page's last row
/// instead loses every undisclosed sibling of that row.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn trade_history_keeps_the_rest_of_a_split_timestamp_group() {
    let limit = mogwai_protocol::MAX_HISTORY_LIMIT;
    let mut tape = (0..limit - 1)
        .map(|i| tape_trade(7, 10_000 + i as i64))
        .collect::<Vec<_>>();
    // Three rows share the second timestamp, so a full first page carries
    // exactly one of them and the other two are only reachable by re-asking
    // at that instant.
    tape.push(tape_trade(9, 1));
    tape.push(tape_trade(9, 2));
    tape.push(tape_trade(9, 3));

    let state = Arc::new(StubState::default());
    let delivered = request_whole_tape(&state, tape).await;
    assert_eq!(delivered.len(), limit + 2);
    let at_nine = delivered
        .iter()
        .filter(|trade| trade.ts_event.as_u64() == 9)
        .count();
    assert_eq!(at_nine, 3, "no row of the split group may be skipped");
    assert_eq!(
        *state.trades_starts.lock().expect("trades starts mutex"),
        vec![None, Some(9)],
        "the second page re-asks at the group's own instant, not past it"
    );
}

/// The quote cursor carries the identical hazard and the identical invariant,
/// so it is pinned identically rather than argued from the shared helper.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn quote_history_keeps_the_rest_of_a_split_timestamp_group() {
    use nautilus_common::messages::data::RequestQuotes;

    let limit = mogwai_protocol::MAX_HISTORY_LIMIT;
    let quote = |ts_event: u64, bid: i64| mogwai_protocol::QuoteTick {
        symbol: "BTCUSDT".into(),
        bid_px: rust_decimal::Decimal::new(bid, 2),
        ask_px: rust_decimal::Decimal::new(bid + 100, 2),
        bid_sz: rust_decimal::Decimal::ONE,
        ask_sz: rust_decimal::Decimal::ONE,
        ts_event,
    };
    let mut tape = (0..limit - 1)
        .map(|i| quote(7, 9_900 + i as i64))
        .collect::<Vec<_>>();
    tape.push(quote(9, 1));
    tape.push(quote(9, 2));
    tape.push(quote(9, 3));

    let state = Arc::new(StubState::default());
    *state.quotes_tape.lock().expect("quotes tape mutex") = Some(tape);
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
            next_data_event(&mut sink_rx, Duration::from_secs(10)).await
        {
            assert_eq!(response.data.len(), limit + 2);
            let at_nine = response
                .data
                .iter()
                .filter(|quote| quote.ts_event.as_u64() == 9)
                .count();
            assert_eq!(at_nine, 3, "no quote of the split group may be skipped");
            assert_eq!(
                *state.quotes_starts.lock().expect("quotes starts mutex"),
                vec![None, Some(9)]
            );
            break;
        }
    }
}

/// The degenerate shape: a whole full page at one timestamp leaves no complete
/// prefix, so there is no advance that discloses the remainder. Report the
/// truncation instead of skipping the undisclosed rows.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn trade_history_does_not_advance_past_a_full_same_nanosecond_page() {
    let limit = mogwai_protocol::MAX_HISTORY_LIMIT;
    let tape = (0..limit + 5)
        .map(|i| tape_trade(7, 10_000 + i as i64))
        .collect::<Vec<_>>();

    let state = Arc::new(StubState::default());
    let delivered = request_whole_tape(&state, tape).await;
    assert_eq!(delivered.len(), limit);
    assert_eq!(
        state.trades_hits.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "a wedged cursor stops rather than paging past the timestamp"
    );
}

/// The SEEDED SET IS NOT AN ADMISSION LIST. The venue resolves any wire-legal
/// symbol and registers it at bind, so a label absent from the connect-time
/// seed is routinely one this run will serve; the guard that refused it here
/// refused exactly the passengers piece 13 exists to support. The bound-symbol
/// check is the surviving local gate and is pinned by
/// `subscribe_refuses_an_instrument_outside_the_bound_symbol`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_subscribe_for_an_instrument_absent_from_the_seeded_set_is_accepted() {
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
    client
        .subscribe_trades(SubscribeTrades::new(
            other,
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("the seeded set is not an admission list");

    // The initially seeded instrument remains accepted too.
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

/// THE POST-BIND RESEED, and the frame that depends on it.
///
/// A client bound to a label nobody configured cannot have its def in the
/// connect-time seed: BINDING is what registers the symbol venue-side, so only
/// a read AFTER the socket is up can carry it. Without that reseed
/// `handle_market_message` finds no def and drops every frame for the symbol,
/// silently - which is what this test bites on: delete the reseed and the trade
/// below never arrives.
///
/// The readiness barrier around it is not separately observable here: the stub
/// holds its trade until this test opens the push gate, which is after connect
/// and after the subscribe, and the pump is a task on this same current-thread
/// runtime that does not run until the test awaits anyway. What the barrier
/// buys is the case the pump DOES get scheduled mid-connect; it is cheap, and
/// the ordering it enforces is the one this test's delivery depends on.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_frame_for_an_unconfigured_symbol_survives_the_post_bind_reseed() {
    const AFTER_BIND: &str = r#"[{"symbol":"BTCUSDT","class":{"class":"spot","base":"BTC","quote":"USDT"},"price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"},{"symbol":"ETHUSDT","class":{"class":"spot","base":"ETH","quote":"USDT"},"price_precision":2,"size_precision":8,"price_increment":"0.01","size_increment":"0.00000001"}]"#;

    let state = Arc::new(StubState::default());
    *state
        .instruments_after_bind
        .lock()
        .expect("post-bind instruments mutex") = Some(AFTER_BIND.to_string());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(r#"{"type":"Trade","symbol":"ETHUSDT","price":"200.00","size":"1","aggressor":"Seller","ts_event":11}"#.to_string());
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client
        .connect()
        .await
        .expect("connect binds and reseeds behind the barrier");

    let unconfigured = nautilus_model::identifiers::InstrumentId::new(
        nautilus_model::identifiers::Symbol::from("ETHUSDT"),
        *mogwai_adapter::MOGWAI_VENUE,
    );
    client
        .subscribe_trades(SubscribeTrades::new(
            unconfigured,
            Some(ClientId::from("MOGWAI-DATA")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("an unconfigured label subscribes");
    // The subscription is recorded; release the tape (`common::PushGate`).
    state.push_gate.open();

    match next_non_instrument_data_event(&mut sink_rx, Duration::from_secs(5)).await {
        DataEvent::Data(Data::Trade(trade)) => {
            assert_eq!(trade.instrument_id, unconfigured);
            assert_eq!(trade.price, Price::from("200.00"));
            assert_eq!(trade.ts_event, UnixNanos::from(11));
        }
        other => panic!("the first frame was black-holed: {other:?}"),
    }
}

/// A failed history fetch must still RESOLVE the nautilus request.
///
/// The failure arms of `request_trades`/`request_bars` used to log and return
/// straight out of the spawned task, so no `DataResponse` was ever emitted and
/// the request hung forever. From the consumer that is indistinguishable from a
/// dead venue: the host burns its entire history timeout and fails the handoff
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
///
/// THE TAPE IS STOCKED AND `/trades` IS COUNTED, and both are load-bearing. An
/// empty response is what a venue holding NO ROWS returns too, so on the stub's
/// default empty tape `resp.data.is_empty()` passed whether or not the guard
/// fired - vacuous in exactly the way this crate spent a round sweeping for.
/// A row at the floor makes the emptiness a consequence, and `trades_hits == 0`
/// says the refusal happened at the CLIENT boundary rather than at the venue.
/// This is also the only place a KNOWN floor is discriminated from an unknown
/// one: `an_undecodable_clock_is_retried_then_falls_back_without_refusing`
/// cannot do it, because the only floor its fixture yields is zero and no
/// `u64` start precedes zero.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn off_tape_window_still_answers_the_request() {
    use nautilus_common::messages::data::{RequestBars, RequestTrades};

    const ORIGIN: u64 = 2_000_000_000_000_000_000;

    let state = Arc::new(StubState::default());
    // Publish a real clock envelope: without one the client cannot decode a
    // floor, falls back to "unknown" (0), and the off-tape guard never fires.
    *state.clock_body.lock().expect("clock body mutex") = Some(format!(
        r#"{{"sim":{{"sim_epoch_ns":0,"wall_anchor_ns":0,"speed":1.0}},"venue_now_ns":{},"data_origin_ns":{ORIGIN},"warmup_ns":86400000000000}}"#,
        ORIGIN + 86_400_000_000_000
    ));
    // Rows the venue WOULD serve if it were asked. See the note above: without
    // them an empty response proves nothing.
    *state.trades_tape.lock().expect("trades tape mutex") = Some(vec![tape_trade(ORIGIN, 10_000)]);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let mut client = data_client(base_url);
    client.start().expect("start grabs the sink");
    client.connect().await.expect("connect opens the socket");

    // One nanosecond below the floor: as off-tape as it gets.
    let off_tape =
        jiff::Timestamp::from_nanosecond(i128::from(ORIGIN - 1)).expect("the floor is in range");
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
            assert!(
                resp.data.is_empty(),
                "an off-tape window answers empty, though the tape holds a row at the floor"
            );
            break;
        }
    }
    assert_eq!(
        state.trades_hits.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the refusal is at the CLIENT boundary: /trades must never have been asked"
    );

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

/// The harness's own request reader, pinned against a SEGMENTED head.
///
/// `common::read_request` used to take a single `read` into a 4096-byte buffer
/// and return `None` when `\r\n\r\n` was not in it - dropping the connection
/// with no diagnostic, so the symptom was an unexplained connect failure
/// attributed to the adapter. Two shapes reach that branch and neither is
/// exotic: a head split across TCP segments (the terminator itself straddling
/// the boundary is the worst case, and it is what this test writes), and a head
/// larger than the buffer, which one `Authorization` header or a cookie jar
/// would produce. Loopback with today's small heads made both latent rather
/// than absent.
///
/// This test lives in a test binary rather than beside the helper because
/// `tests/common` is compiled into four of them and a pin there would run four
/// times for one property.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn the_harness_reads_a_request_head_split_across_segments() {
    use tokio::{
        io::AsyncWriteExt,
        net::{TcpListener, TcpStream},
    };

    // A head comfortably past the old 4096-byte buffer, so the oversize shape
    // and the segmented shape are both exercised by one exchange.
    let padding = "x".repeat(6000);
    let head = format!(
        "POST /control/divergence HTTP/1.1\r\nHost: stub\r\nAuthorization: {padding}\r\nContent-Length: 5\r\n\r\n"
    );
    // Split so the terminator itself straddles the boundary: the first segment
    // ends inside `\r\n\r\n`, which defeats any scan of only the newest bytes.
    let split = head.len() - 2;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let venue = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        common::read_request(&mut stream).await
    });

    let mut client = TcpStream::connect(addr).await.expect("connect");
    client
        .write_all(&head.as_bytes()[..split])
        .await
        .expect("first segment");
    client.flush().await.expect("flush first segment");
    // Let the reader observe the short segment before the rest arrives, so the
    // test really does present two reads rather than relying on the kernel to
    // split them.
    tokio::time::sleep(Duration::from_millis(20)).await;
    // NOT `expect`ed, and that is the point. A reader that gave up on the short
    // first segment has already dropped its socket, so these writes fail with
    // ECONNRESET - an error naming the write, not the property. The verdict
    // belongs to what the reader RETURNED, which is asserted below; letting the
    // write panic would report a bound rather than the check.
    drop(client.write_all(&head.as_bytes()[split..]).await);
    drop(client.write_all(b"hello").await);
    drop(client.flush().await);

    // BOUNDED for the same reason the writes above are not `expect`ed: the
    // failure this test discriminates is "the reader gave up", and a future
    // regression could express that as a BLOCK rather than a `None`. Without a
    // bound of its own the test would hang to the libtest watchdog and report
    // nothing attributable.
    let (read_head, body) = tokio::time::timeout(Duration::from_secs(5), venue)
        .await
        .expect("the request reader must finish rather than block on a segmented head")
        .expect("reader task")
        .expect("a segmented head must be read, not dropped as an unparseable request");
    assert!(
        read_head.starts_with("POST /control/divergence "),
        "the request line must survive segmentation (head={:?})",
        &read_head[..read_head.len().min(80)]
    );
    assert!(
        read_head.contains(&padding),
        "a head larger than the read buffer must be accumulated in full"
    );
    assert_eq!(
        String::from_utf8_lossy(&body),
        "hello",
        "the body must still be read once the head loop has finished"
    );
}
