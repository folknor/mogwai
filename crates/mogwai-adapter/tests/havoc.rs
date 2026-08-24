// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Ignored havoc tests for the mogwai adapter.
//!
//! These bind a real TCP listener and exercise behavior that unit tests cannot
//! reach: venue-side divergence shipping, adapter-inbound corruption,
//! connection-lifecycle havoc, and the end-to-end divergence surfaces the suite
//! claims (partial fills, duplicate fills, dropped account updates, blackouts).
//! They share the stub harness in `tests/common`.
//!
//! Run a focused case in a socket-capable environment with e.g.
//! `test -p mogwai-adapter ships_venue_havoc --debug` in the focused runner.

mod common;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use common::{
    StubState, bound_stub, cached_order, cached_stop_market, instrument_id, next_exec_event,
    next_non_instrument_data_event, submit_command, trade_json,
};
use mogwai_adapter::{
    MOGWAI_VENUE, MogwaiDataClient, MogwaiDataClientConfig, MogwaiExecClientConfig,
    MogwaiExecutionClient,
};
use mogwai_protocol::{
    ConnHavoc, EventKind, HavocLatency, HavocSpec, InboundHavoc, control::Divergence,
};
use nautilus_common::{
    cache::Cache,
    clients::{DataClient, ExecutionClient},
    live::runner::{replace_data_event_sender, replace_exec_event_sender},
    messages::{DataEvent, ExecutionEvent, data::SubscribeTrades, execution::SubmitOrder},
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    data::Data,
    enums::{AccountType, OmsType},
    events::OrderEventAny,
    identifiers::{AccountId, ClientId, StrategyId, TraderId},
    orders::Order,
    types::Quantity,
};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

async fn subscribed_data_client(
    state: Arc<StubState>,
    havoc: Option<HavocSpec>,
) -> UnboundedReceiver<DataEvent> {
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc,
        expected_run_seed: None,
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    subscribe(&mut client);
    // The subscription is now recorded, so the tape may flow. Released here rather
    // than waited out by the stub, because the subscription never crosses the
    // wire and nothing on the venue side can see it (`common::PushGate`).
    state.push_gate.open();
    sink_rx
}

fn subscribe(client: &mut MogwaiDataClient) {
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
}

fn data_havoc(inbound: InboundHavoc) -> HavocSpec {
    HavocSpec {
        inbound,
        ..HavocSpec::default()
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
    state: &StubState,
    havoc: Option<HavocSpec>,
) -> (MogwaiDataClient, UnboundedReceiver<DataEvent>) {
    let (sink_tx, sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        // Stated, not defaulted: the default is a placeholder the validator
        // refuses, so no socket binds an account nobody chose.
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc,
        expected_run_seed: None,
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    subscribe(&mut client);
    // See `subscribed_data_client`: the local subscription is now recorded, so
    // the stub's tape push is released.
    state.push_gate.open();
    (client, sink_rx)
}

async fn next_trade(rx: &mut UnboundedReceiver<DataEvent>) -> nautilus_model::data::TradeTick {
    // Skips the connect-time instrument prologue (`emit_seeded_instruments`
    // publishes the seeded defs to the sink before any trade), like the
    // transport tests do.
    match next_non_instrument_data_event(rx, Duration::from_secs(2)).await {
        DataEvent::Data(Data::Trade(trade)) => trade,
        other => panic!("expected trade event, got {other:?}"),
    }
}

/// Asserts nothing except the connect-time instrument prologue reaches the
/// data sink within `window`. The prologue is emitted by `connect()` straight
/// to the sink - it neither passes the `HavocFilter` nor rides the WS stream -
/// so "the havoc/stub suppressed everything" tests must tolerate it while
/// still failing on any trade, bar, or quote that leaks through.
///
/// The window does not open until the stub has spoken, and that is the
/// difference between "the client suppressed the tape" and "the tape was never
/// sent". A window timed from the caller's own line races the stub's push task;
/// expiring before the first frame reached the wire is a pass that observed
/// nothing, which is this arc's signature defect wearing a negative
/// assertion's costume. `ws_first_frame_at` is stamped strictly before the
/// first send, so waiting for it makes the silence afterwards evidence. The
/// wait is bounded and fails outright rather than falling through, because a stub that
/// never pushed is the case the window cannot judge.
async fn assert_only_instrument_prologue(
    state: &StubState,
    rx: &mut UnboundedReceiver<DataEvent>,
    window: Duration,
) {
    let sent_by = Instant::now() + Duration::from_secs(2);
    while state
        .ws_first_frame_at
        .lock()
        .expect("ws first frame instant mutex")
        .is_none()
    {
        common::assert_push_gate_opened();
        assert!(
            Instant::now() < sent_by,
            "the stub never put a frame on the wire, so a silent sink says nothing \
             about what the client suppressed"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let deadline = Instant::now() + window;
    loop {
        // See `wait_for_at_least`: an absence observed because the tape was
        // never released is not evidence that anything was suppressed.
        common::assert_push_gate_opened();
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            // Elapsed silent, or sink closed: nothing (more) can leak through.
            Err(_) | Ok(None) => return,
            Ok(Some(DataEvent::Instrument(_))) => continue,
            Ok(Some(other)) => panic!("expected no data events within the window, got {other:?}"),
        }
    }
}

/// Waits up to `timeout` for `count` to reach at least `target`, polling on a
/// short interval. Returns the final observed value.
async fn wait_for_at_least(count: &AtomicUsize, target: usize, timeout: Duration) -> usize {
    let deadline = Instant::now() + timeout;
    loop {
        // The gate speaks first, on every poll helper in this file: a leg that
        // gave up waiting for `push_gate` sent no tape, and a counter that then
        // fails to move - or an absence that then passes - is that omission's
        // symptom rather than a finding about the client. Free unless a stall
        // was actually recorded on this thread.
        common::assert_push_gate_opened();
        let value = count.load(Ordering::Relaxed);
        if value >= target || Instant::now() >= deadline {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits up to `timeout` for the stub to have recorded at least one `/ws`
/// upgrade, returning the request lines it holds at that point (empty if the
/// deadline passed without one). The stub records from its own handler task, so
/// a caller that reads the list directly is asking a question the answer to has
/// not necessarily been written yet.
async fn wait_for_ws_request(state: &StubState, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        common::assert_push_gate_opened();
        let requests = state.ws_requests.lock().expect("ws request mutex").clone();
        if !requests.is_empty() || Instant::now() >= deadline {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Builds, starts, connects and submits an order against a stub on the exec WS
/// leg, returning the live exec client and its sink. The stub replies to the
/// `SubmitOrder` with whatever frames the caller seeded into `ws_exec_frames`.
async fn submit_exec_client(
    state: Arc<StubState>,
) -> (MogwaiExecutionClient, UnboundedReceiver<ExecutionEvent>) {
    let base_url = bound_stub(state).await;
    let (sink_tx, sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_order(&cache);
    let config = MogwaiExecClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
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
    (client, sink_rx)
}

/// As `submit_exec_client`, but submits a nautilus `StopMarketOrder` under the
/// supplied inbound havoc. The order shape is what differs: only a conditional
/// produces an `OrderTriggered` for havoc to reach.
async fn submit_stop_exec_client(
    state: Arc<StubState>,
    havoc: HavocSpec,
) -> (MogwaiExecutionClient, UnboundedReceiver<ExecutionEvent>) {
    let base_url = bound_stub(state).await;
    let (sink_tx, sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_stop_market(&cache);
    let config = MogwaiExecClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc: Some(havoc),
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
        .submit_order(submit_command(&order, order.init_event().clone()))
        .expect("a stop-market is no longer refused at conversion");
    (client, sink_rx)
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn ships_venue_havoc() {
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    // Engine-side single-shots only: this spec rides HTTP transport profiles
    // below, and validate() now refuses a venue temporal window (GoDark,
    // DelayAcks, StallData under polling) the chosen carrier cannot deliver.
    let havoc = HavocSpec {
        inbound: InboundHavoc::default(),
        venue: vec![
            Divergence::RejectNextSubmit {
                reason: "nope".into(),
            },
            Divergence::DropNextAccountUpdate,
        ],
        data: None,
        conn: ConnHavoc::default(),
    };

    // An exec client with no event sink is deaf - nautilus's emitter drops
    // every order event with a log line and no error - so `connect()` refuses
    // one outright (AE20). This test cares only about the HTTP control leg,
    // but it still has to be a client that could hear an answer; hold the
    // receiver alive for the duration rather than dropping it.
    let (sink_tx, _sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let exec_config = MogwaiExecClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url: base_url.clone(),
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
    // The control bodies must round-trip the actual payload values, not merely
    // contain the type-name: a serialization bug shipping an empty reason or the
    // wrong duration would pass a substring-only check.
    {
        let bodies = state.control_bodies.lock().expect("control bodies mutex");
        let reject: Vec<Divergence> = bodies
            .iter()
            .filter_map(|body| {
                let mut request: serde_json::Value = serde_json::from_str(body).ok()?;
                let kind = request.get_mut("kind")?.take();
                let mut args = request.get_mut("args")?.take().as_object()?.clone();
                args.insert("type".into(), kind);
                serde_json::from_value(serde_json::Value::Object(args)).ok()
            })
            .collect();
        assert!(
            reject.iter().any(|d| matches!(
                d,
                Divergence::RejectNextSubmit { reason } if reason == "nope"
            )),
            "RejectNextSubmit did not round-trip its reason"
        );
        assert!(
            reject
                .iter()
                .any(|d| matches!(d, Divergence::DropNextAccountUpdate)),
            "DropNextAccountUpdate did not round-trip"
        );
    }

    let data_config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc: Some(havoc),
        expected_run_seed: None,
        ..Default::default()
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

    // Drained to a deadline, not read on the next line. The count is written by
    // the stub's handler tasks, so an immediate read cannot distinguish "the
    // data client shipped nothing" from "the data client's POST has not landed
    // yet" - a regression shipping divergences asynchronously would pass. The
    // assertion is that the count stays at 2 for a window, which is the same
    // discipline `an_order_list_reaches_the_wire_as_linked_legs` uses.
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        assert_eq!(
            state.control_hits.load(Ordering::Relaxed),
            2,
            "the data client must never ship divergences; only the exec leg arms the venue"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
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
    let armed = HavocLatency {
        base_nanos: 20_000_000,
        data_nanos: 30_000_000,
        ..HavocLatency::default()
    };
    // The bound is the composed delay, derived rather than written down. It read
    // 50 ms - the armed half alone - which under-stated the contract by the
    // always-on 30 ms baseline and left the bite margin at ~20 ms: zeroing the
    // armed latency, the injection that proves this test alive, still delivered
    // at ~31.7 ms and cleared 50. Stating the sum the client actually owes moves
    // the discriminator to 50 ms for no wall cost at all, and deriving it from
    // `BASELINE_LATENCY` means a change to either half cannot leave a stale
    // literal here. It cannot flake low: the pump sleeps until a deadline
    // anchored at arrival, and `ws_first_frame_at` is stamped strictly before
    // the send, so the measured interval is >= the composed delay by
    // construction.
    let delay = mogwai_protocol::BASELINE_LATENCY.delay_for(EventKind::Data)
        + armed.delay_for(EventKind::Data);
    let havoc = data_havoc(InboundHavoc {
        latency: Some(armed),
        ..InboundHavoc::default()
    });

    // The clock starts at the stub's send, not at `connect()`. Measuring from
    // the return of connect/subscribe charges the client for everything the stub
    // does between the upgrade and the push, and the harness does a good deal
    // there - so with `HavocLatency` zeroed the assertion still passed, satisfied
    // entirely by stub time. `ws_first_frame_at` is the instant the trade went on
    // the wire, and the interval from there to delivery is the latency filter's
    // contribution and nothing else, whatever the harness grows in front of it.
    let mut rx = subscribed_data_client(Arc::clone(&state), Some(havoc)).await;
    let _trade = next_trade(&mut rx).await;
    let arrived = Instant::now();

    let sent = state
        .ws_first_frame_at
        .lock()
        .expect("ws first frame instant mutex")
        .expect("the stub recorded when it put the trade on the wire");
    let elapsed = arrived.duration_since(sent);
    assert!(
        elapsed >= delay,
        "inbound trade arrived {elapsed:?} after the stub sent it, before the composed {delay:?} delay"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "inbound trade was delayed far beyond the composed latency"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_drop_prob_one_drops_all() {
    // Push N trades and assert zero arrive. A single trade + assert-nothing
    // cannot distinguish "drop applied" from "never delivered"; pushing several
    // and observing none reach the sink pins the drop to the filter.
    let state = Arc::new(StubState::default());
    {
        let mut trades = state.ws_trades.lock().expect("ws trades mutex");
        for ts in [10u64, 20, 30, 40, 50] {
            trades.push(trade_json(ts, "100.00"));
        }
    }
    let havoc = data_havoc(InboundHavoc {
        drop_prob: 1.0,
        seed: Some(1),
        ..InboundHavoc::default()
    });

    let mut rx = subscribed_data_client(Arc::clone(&state), Some(havoc)).await;

    // Panics if any of the five dropped trades reaches the sink; only the
    // connect-time instrument prologue may arrive. The helper establishes that
    // the stub sent before it starts scoring the silence.
    assert_only_instrument_prologue(&state, &mut rx, Duration::from_millis(400)).await;
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_duplicate_prob_one_doubles() {
    // One trade in, with duplicate_prob 1.0: exactly two identical trades out,
    // then nothing. Asserting "exactly two" (with a tail drain) rules out a
    // triple-emit; matching every field rules out two independently-synthesized
    // trades masquerading as a duplicate.
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let havoc = data_havoc(InboundHavoc {
        duplicate_prob: 1.0,
        seed: Some(1),
        ..InboundHavoc::default()
    });

    let mut rx = subscribed_data_client(state, Some(havoc)).await;

    let first = next_trade(&mut rx).await;
    let second = next_trade(&mut rx).await;
    assert_eq!(first.ts_event, second.ts_event);
    assert_eq!(first.price, second.price);
    assert_eq!(first.size, second.size);
    assert_eq!(first.aggressor_side, second.aggressor_side);
    let tail = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
    assert!(tail.is_err(), "duplicate must emit exactly two, not more");
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
    // Feed an ODD count (three trades). The reorder filter holds one message and
    // releases it on the next; with three frames the third is left dangling and
    // must be released by `HavocFilter::flush`. A two-trade feed always pairs and
    // never exercises the flush/held-message path - if flush dropped the dangling
    // held message, this catches it.
    //
    // The flush only runs from the reader loop's on-disconnect callback, which
    // fires when the socket closes (peer close / EOF) - NOT during a live
    // mid-stream read, and NOT on a client-side `stop()` (that just aborts the
    // reader task: finding A.5, the held message starves until a peer close). So
    // the stub closes the socket after pushing the three trades, modelling a
    // clean stream end; the close drives the flush that releases the third.
    let state = Arc::new(StubState::default());
    state.close_after_trades.store(true, Ordering::Relaxed);
    {
        let mut trades = state.ws_trades.lock().expect("ws trades mutex");
        trades.push(trade_json(10, "100.00"));
        trades.push(trade_json(20, "101.00"));
        trades.push(trade_json(30, "102.00"));
    }
    let havoc = data_havoc(InboundHavoc {
        reorder_prob: 1.0,
        seed: Some(1),
        ..InboundHavoc::default()
    });

    let mut rx = subscribed_data_client(state, Some(havoc)).await;

    // First two are transposed mid-stream; the third is the dangling held
    // message, released by the flush the stub's close triggers.
    let first = next_trade(&mut rx).await;
    let second = next_trade(&mut rx).await;
    let third = next_trade(&mut rx).await;
    assert_eq!(first.ts_event, UnixNanos::from(20));
    assert_eq!(second.ts_event, UnixNanos::from(10));
    assert_eq!(
        third.ts_event,
        UnixNanos::from(30),
        "the odd dangling trade must be released by the flush-on-close path, not lost"
    );
}

/// The `close_after_trades` stub serves its batch once, and the reconnect that
/// its own close provokes gets a live, silent socket rather than the batch
/// again.
///
/// This pins the harness, not the client, and it is here because the switch is
/// armed here. The close is what makes the client re-dial, so a leg that
/// re-reads `ws_trades` on every upgrade re-serves and re-closes for as long as
/// the test runs - a stub spinning underneath a green test, which surfaces later
/// as an unrelated-looking flake in whatever else shares the box. The reorder
/// test above cannot see it: it stops reading after three trades.
///
/// The handshake count is not decoration. Without a re-dial there is nothing to
/// replay and the trade assertion below would pass for free, so the reconnect is
/// established first and the silence asserted after it.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_close_after_trades_leg_does_not_replay_its_batch_on_the_reconnect() {
    let state = Arc::new(StubState::default());
    state.close_after_trades.store(true, Ordering::Relaxed);
    {
        let mut trades = state.ws_trades.lock().expect("ws trades mutex");
        trades.push(trade_json(10, "100.00"));
        trades.push(trade_json(20, "101.00"));
    }
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, mut rx) = connect_data_client(base_url, &state, None).await;

    let first = next_trade(&mut rx).await;
    let second = next_trade(&mut rx).await;
    assert_eq!(first.ts_event, UnixNanos::from(10));
    assert_eq!(second.ts_event, UnixNanos::from(20));

    let handshakes = wait_for_at_least(&state.ws_handshakes, 2, Duration::from_secs(3)).await;
    assert!(
        handshakes >= 2,
        "the stub's close must drive the client back for a second upgrade, or this \
         test cannot observe a replay at all (saw {handshakes})"
    );
    // A negative assertion with a bounded window, opened only after the re-dial
    // above is a fact.
    let replayed = tokio::time::timeout(Duration::from_millis(400), rx.recv()).await;
    assert!(
        replayed.is_err(),
        "the reconnected leg must serve nothing; the batch was replayed as {replayed:?}"
    );
}

/// A client bound to a run refuses an address serving a different run, and says
/// so in one named line rather than dying as a generic connect failure.
///
/// The exposure this closes: the venue's port is ephemeral and is freed before
/// the process exits - it stops accepting, then drains for up to the shutdown
/// grace - so an address can be reused while a consumer watching for child exit
/// still sees a live child. A client that only knows where to dial cannot tell
/// its own run from whatever answers there next.
///
/// Terminal on purpose: reconnecting is for a venue that went away and came
/// back. A different run at the same address did not come back, and re-dialling
/// would only find it again.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_venue_serving_another_run_is_refused_terminally() {
    let state = Arc::new(StubState::default());
    state.run_seed.store(4242, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc: None,
        // Bound to a run this stub is not serving.
        expected_run_seed: Some(7),
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the data-event sink");
    // Refused, so the client never reports connected and `connect` spends its
    // whole readiness bound waiting for something that cannot arrive. The
    // distinct signal is the named `venue identity mismatch` error line the loop
    // logs before returning; at this API the outcome is simply that the client
    // never comes up, which is the correct end state either way.
    //
    // Bounded, because `wait_connected`'s five seconds are on this test's
    // passing path, not its failing one. Readiness never arrives here, so the
    // internal deadline was the runtime and this one test was 5.0 s of a 15.6 s
    // serial sweep - the same shape `conn_reconnect_respects_max_attempts` was
    // repaired for. Everything the refusal needs is loopback and complete in
    // single-digit milliseconds: one dial, one `/health` probe, one classify.
    //
    // The bound is coupled to the stub's `/clock` default, and that coupling is
    // invisible from here, so it is written down. `connect()` does the clock
    // fetch, then seeds instruments, then spawns the pump, and only then spawns
    // the reader that issues the `/health` probe the poll below waits on. When
    // the stub's default clock body was the undecodable catch-all, that prefix
    // alone was ~400 ms of retry ladder and this bound had ~100 ms of margin;
    // the default now serves a decodable envelope, which is the only reason the
    // headroom is two orders of magnitude. Arming `fail_clock` in this fixture,
    // or lengthening the ladder, puts the prefix back over the bound - and this
    // test would then fail on `health_hits` never reaching one, reporting a
    // refusal defect that is not there. Raise the bound with the ladder, or
    // derive it from `CLOCK_FETCH_MAX_ATTEMPTS`.
    //
    // Cancelling `connect()` mid-flight skips its own cleanup - `abort_tasks`
    // and `retire_connected_flag` never run, so the pump, the reader and the
    // `delivery_ready` sender are dropped rather than retired. That is the
    // guard-scope shape AGENTS.md flags, and terminality is what makes it safe
    // here: a venue-identity mismatch is terminal, so the lifecycle loop returns
    // and nothing restarts to race the assertions below. On any fixture whose
    // failure is retryable this cancellation would leave a live reconnect loop
    // running under the assertions.
    drop(tokio::time::timeout(Duration::from_millis(500), client.connect()).await);

    // The probe was asked, polled rather than assumed: with the connect bounded
    // above, "not refused yet" and "never probed" are otherwise the same
    // silence, and an assertion on the flag alone would then be reading a
    // client that had not got round to deciding.
    let probes = wait_for_at_least(&state.health_hits, 1, Duration::from_secs(2)).await;
    assert!(
        probes >= 1,
        "the client must have probed /health to judge the run at all; saw {probes}"
    );

    // This `is_disconnected()` is live, and it is the only assertion here that
    // catches the defect - which is the opposite of what it looks like, so it is
    // written down rather than left to be re-derived. The flag is `!connected`,
    // it starts false, and `lifecycle` stores false on every failed dial, so on
    // a fixture that refuses the upgrade it reads "disconnected" from t=0 and
    // any assertion on it is vacuous - that is why it was removed twice from
    // `conn_reconnect_respects_max_attempts`, and a test-hunt report proposed
    // removing it here on the same reasoning. It does not transfer. This stub
    // serves a perfectly good websocket; the refusal happens after the dial
    // succeeds, in `verify_run_identity`, and the very next statement on the
    // non-refusing path is `connected.store(true)`. So a client that stops
    // refusing reports connected here, and the flag discriminates.
    //
    // Bite-checked as a text edit: making `IdentityOutcome::Mismatch` return
    // `Ok(())` fails this assertion by its own message. The `handshakes` bound
    // below does not move - one dial either way - so deleting this would have
    // left the whole refusal unpinned.
    //
    // Held as a window, not snapshotted, because the gate above is `health_hits`
    // and the stub increments that when it serves `/health` - before the client
    // has received the response, classified it and reached the store. A single
    // read the instant the counter moves therefore has a real if narrow window
    // in which a NON-refusing client is still false, which would pass this
    // vacuously on the very bite-check that is supposed to catch it. Watching
    // for a quarter second closes the window: a client that stops refusing
    // stores true within microseconds of the classify.
    let watch_until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < watch_until {
        assert!(
            client.is_disconnected(),
            "a client bound to run 7 must not stay connected to run 4242"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The refusal is terminal, so the loop stops dialing rather than walking the
    // reconnect ladder against an address it has already judged.
    let handshakes = state.ws_handshakes.load(Ordering::Relaxed);
    assert!(
        handshakes <= 2,
        "a refused identity must not be retried, saw {handshakes} handshakes"
    );
}

/// The port-reuse question, answered - and the answer is narrower than the
/// question assumed. An external QA pass showed the adapter dials a dead
/// venue's address and that a stranger holding the reused port accepts the
/// connection, but their stranger was a bare TCP listener that accepted and
/// closed, so nothing past the dial was ever demonstrated.
///
/// The stranger here speaks the wire. With no `expected_run_seed` at all the client
/// dials blind and establishes a live passenger against a venue serving an
/// entirely different run, and nothing notices. That is the cost of the blind
/// default, now measured rather than assumed: not a dial that fails fast, but a
/// live client consuming a stranger's market data as though it were its own
/// venue's - which silently corrupts a forward run rather than failing it.
///
/// The disclosure half is now live, and this test is where it was reserved to
/// be asserted. The original framing said the account id could not be "stamped
/// onto the stranger's state" because the adapter disclosed none - its `/ws`
/// query carried only an optional `symbol` and the id was a nautilus-side
/// label. That stopped being true when the adapter started naming its ledger,
/// which it must, or every worker attached to a shared venue trades one book.
///
/// So the exposure of a blind dial is now what the client reveals as well as
/// what it consumes: dialling a stranger opens an account there under this
/// run's id, and trades it. That does not change the remedy - `expected_run_seed`
/// is what makes a blind dial impossible, and it is set by `for_run`, which is
/// the constructor a launched or attached venue always has the record for - but
/// it does raise the cost of not setting it, and this test is the measurement
/// rather than the assumption.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn dialing_blind_establishes_a_full_passenger_with_a_stranger() {
    let state = Arc::new(StubState::default());
    // A stranger serving some other run. A client that checked would refuse it.
    state.run_seed.store(4242, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-042"),
        base_url,
        symbol: None,
        callsign: None,
        havoc: None,
        // The undecided default: no identity to check against.
        expected_run_seed: None,
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the data-event sink");
    client.connect().await.expect("connect is spawned");

    // The upgrade record is written by the stub's handler task, so it is polled
    // for rather than read on the next line: silence from a background recorder
    // means "nothing recorded yet", and a fixed sleep only guesses how long yet
    // lasts. On the passing path the poll returns on its first look, because
    // `serve_ws` pushes the request line before it writes the upgrade bytes and
    // `connect` cannot have returned connected before that; the two seconds are
    // a failure deadline for the paths where it did not happen.
    let requests = wait_for_ws_request(&state, Duration::from_secs(2)).await;
    assert!(
        !requests.is_empty(),
        "the stranger completed at least one upgrade"
    );
    // A live passenger, not an upgrade that completes and then dies. This is a
    // window, not a snapshot, and that is what the deleted `sleep(600ms)` was
    // buying: `ws_requests` is pushed at the top of `serve_ws`, before the
    // upgrade bytes are even written, and `connect` returns as soon as the
    // client reports connected - so a stranger that completes the handshake and
    // immediately drops the socket satisfies both of the checks above. The
    // passenger has to be observed surviving, which is the stub's own
    // `active_ws` (decremented by the handler's drop guard) plus the client not
    // having fallen back into its reconnect loop.
    let held = Instant::now() + Duration::from_millis(200);
    while Instant::now() < held {
        assert_eq!(
            state.active_ws.load(Ordering::Relaxed),
            1,
            "the stranger's socket must stay up, not close behind the upgrade"
        );
        assert!(
            !client.is_disconnected(),
            "dialing blind establishes a passenger against a stranger"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The disclosure half, pinned as the positive claim it now is: the upgrade names
    // this run's ledger, so a blind dial opens that account on a stranger's
    // venue. Asserted on the configured id rather than on the substring
    // `account`, so a future query key that merely contains the word cannot
    // stand in for the disclosure this is measuring.
    assert!(
        requests
            .iter()
            .all(|line| line.contains("account=MOGWAI-042")),
        "every upgrade names the configured ledger: {requests:?}"
    );
}

/// An identity check the venue cannot answer is never a mismatch. A probe fails
/// for the same transport reasons a socket does, and refusing on that would turn
/// a blip into a dead client - so a venue that cannot answer is used, not judged.
///
/// The fixture has to exclude the answerable case, and it did not: the stub
/// served a perfectly good `/health` naming the run the client expected, which
/// is the plain matching-identity path. Turning `verify_run_identity` into a
/// hard refusal on `Unreachable` left this test green, and left the whole
/// refused-to-refuse behaviour with no coverage anywhere - the pure-unit
/// classifier test pins the sorting, not what the connection loop does with it.
/// So the venue here answers `500`: probed, unresolvable, and used regardless.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn an_unanswerable_identity_probe_does_not_refuse() {
    let state = Arc::new(StubState::default());
    // The run this client expects - set so that a stub which ever starts
    // answering again would answer favourably, and this test would then be
    // pinning the wrong branch loudly rather than passing quietly.
    state.run_seed.store(7, Ordering::Relaxed);
    // ...but it does not answer. The probe is made and cannot be resolved.
    state.fail_health.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc: None,
        expected_run_seed: Some(7),
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the data-event sink");
    // The connect result is dropped rather than unwrapped, so a client that
    // wrongly refuses this venue fails on the property below instead of on a
    // generic "connect timed out" from the helper - the refusal path never
    // reports connected, and its message would name the socket, not the check.
    drop(client.connect().await);

    // The probe was asked before anything is concluded from the client not
    // having refused: a client that skipped the check entirely would satisfy
    // the assertion below for free, which is the vacuity this test carried.
    //
    // Polled, not slept for. `health_hits` is written by the stub's handler
    // task, so an empty count means "not recorded yet" as readily as "never
    // asked"; the two-second wait is the failure deadline, not the success
    // path, and a client that really skips the probe now fails in two seconds
    // rather than being waited out for a fixed six hundred milliseconds.
    let probes = wait_for_at_least(&state.health_hits, 1, Duration::from_secs(2)).await;
    assert!(
        probes >= 1,
        "the client must have probed /health at all; saw {probes} requests"
    );
    assert!(
        !client.is_disconnected(),
        "an unanswerable probe must be used, not refused"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_reconnect_respects_max_attempts() {
    // The stub refuses the WebSocket upgrade on every dial. With a three-attempt
    // cap the client must dial exactly three times - one initial plus two
    // retries - and then stop.
    //
    // "Stopped" is asserted as the absence of a fourth dial, and deliberately
    // not as `is_disconnected()`. That flag is `!connected`, it starts false,
    // and `lifecycle` stores `false` on every failed dial - so it reads
    // "disconnected" from the first instant of the test, long before the cap is
    // reached, and any assertion built on it passes whatever the client does.
    // Two earlier shapes of this test asserted it anyway, the second inside a
    // poll loop whose condition was therefore false on entry, making the loop
    // body and its named message unreachable in every run. The only observable
    // that distinguishes "gave up" from "still trying" is the dial counter.
    let state = Arc::new(StubState::default());
    state.refuse_ws.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        havoc: Some(conn_havoc(ConnHavoc {
            reconnect_max_attempts: Some(3),
            reconnect_delay_initial_ms: 30,
            reconnect_delay_max_ms: 30,
            ..ConnHavoc::default()
        })),
        expected_run_seed: None,
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    // `connect` resolves the sink first, so the client must be started or it
    // errors out before ever dialing the WS (no handshake would reach the stub).
    client.start().expect("start grabs the data-event sink");
    // The dials all fail, so the loop exhausts the cap and the connect helper's
    // readiness wait never succeeds; bound the await rather than block on it.
    // The bound is a failure deadline, not a budget this test spends - it used
    // to be two seconds, and since readiness never arrives the test paid every
    // one of them on the passing path. The ladder here is three loopback dials
    // and two 30 ms backoffs, so a client that is going to give up has given up
    // an order of magnitude inside this.
    drop(tokio::time::timeout(Duration::from_secs(1), client.connect()).await);

    // The lower bound is polled, not inherited from the timeout above. The
    // ladder in front of the third dial is a `/clock` fetch with its own retry
    // sleeps, an instrument seed, three loopback dials and two 30 ms backoffs;
    // if that overruns the connect bound then the negative window below opens
    // early and fails on `saw 1` or `saw 2` - a wall-clock flake wearing the
    // costume of the defect. Waiting for the count makes the bound a failure
    // deadline on both sides.
    let dials = wait_for_at_least(&state.ws_handshakes, 3, Duration::from_secs(2)).await;
    assert!(
        dials >= 3,
        "a three-attempt cap dials three times: initial plus two retries; saw {dials}"
    );
    // The window is a negative assertion and stays: the property is that no
    // fourth dial ever lands, and the only way to observe an event that must
    // not happen is to watch for a while. What changed is the count it holds.
    //
    // The old `(3..=4)` tolerance made the window vacuous against the defect it
    // was written for - a fourth dial arriving at 310 ms passed, and one
    // arriving at 10 ms passed too, so the wait bought nothing. Three is exact
    // rather than tolerant because the ladder is: `exhausted(0)` is false, the
    // dial fails, `backoff_or_exhausted` bumps to 1 and 2 and returns true at
    // 3, and every dial is counted by the stub before it drops the socket the
    // client is waiting on. There is no scheduling order in which a fourth is
    // legal, so tolerating one only hid it.
    let window = Instant::now() + Duration::from_millis(300);
    while Instant::now() < window {
        let handshakes = state.ws_handshakes.load(Ordering::Relaxed);
        assert_eq!(
            handshakes, 3,
            "a three-attempt cap dials exactly three times (initial plus two \
             retries) and never again; saw {handshakes}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_heartbeat_pings_when_enabled() {
    // A live but silent socket: with the heartbeat enabled the client must emit
    // WS Ping frames the stub observes, even though no application data flows.
    // The interval is 50ms; over a ~600ms window we expect several, so we assert
    // the pacing produced more than one rather than a single accidental probe.
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, _rx) = connect_data_client(
        base_url,
        &state,
        Some(conn_havoc(ConnHavoc {
            heartbeat_interval_ms: 50,
            ..ConnHavoc::default()
        })),
    )
    .await;

    let pings = wait_for_at_least(&state.ws_pings, 3, Duration::from_secs(2)).await;
    assert!(
        pings >= 3,
        "heartbeat did not pace several Pings in the window (pings={pings})"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_handles_inbound_venue_ping() {
    // The stub sends an unsolicited WS Ping after the subscribe. The client's
    // reader must answer it with a Pong (lifecycle Ping -> Pong path); the stub
    // counts the Pong. This proves the inbound control-frame reply path that the
    // heartbeat test (client-initiated Ping) never reaches.
    let state = Arc::new(StubState::default());
    state.ws_venue_pings.store(1, Ordering::Relaxed);
    // Keep the socket lively with one trade so the reader loop runs.
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, _rx) = connect_data_client(base_url, &state, None).await;

    let pongs = wait_for_at_least(&state.ws_pongs, 1, Duration::from_secs(2)).await;
    assert!(
        pongs >= 1,
        "client did not Pong the venue's inbound Ping (pongs={pongs})"
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
    let (_client, mut rx) = connect_data_client(base_url, &state, None).await;

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

// --- F.13: end-to-end divergence behavioral tests ------------------------------

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn divergence_partial_fill_reports_partial_qty() {
    // A `PartialFillNext` divergence has the venue fill only part of the order.
    // The stub models the resulting wire frame: an `OrderFilled` moving 0.4 of a
    // qty-1 order with leaves_qty 0.6. The adapter must report a fill of exactly
    // 0.4 (the partial), not the full quantity.
    let state = Arc::new(StubState::default());
    {
        let mut frames = state.ws_exec_frames.lock().expect("ws exec frames mutex");
        frames.push(
            r#"{"type":"OrderAccepted","client_order_id":"O-1","venue_order_id":"V-1","ts_event":10}"#
                .to_string(),
        );
        frames.push(
            r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"0.4","last_px":"100.00","leaves_qty":"0.6","commission":"0","commission_currency":"USDT","liquidity_side":"taker","ts_event":11}"#
                .to_string(),
        );
    }
    let (_client, mut rx) = submit_exec_client(state).await;

    let timeout = Duration::from_secs(3);
    assert!(matches!(
        next_exec_event(&mut rx, timeout, "the local OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout, "the venue's OrderAccepted").await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    match next_exec_event(&mut rx, timeout, "the PARTIAL OrderFilled").await {
        ExecutionEvent::Order(OrderEventAny::Filled(fill)) => {
            assert_eq!(
                fill.last_qty,
                Quantity::from("0.4"),
                "a partial fill must report the partial last_qty, not the full order"
            );
        }
        other => panic!("expected a partial OrderFilled, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn divergence_duplicate_fill_forwards_both_wire_events() {
    // A `DuplicateNextFill` divergence emits the same OrderFilled (same
    // trade_id) twice. Per the A.1 contract, the adapter's reconciliation mirror
    // dedups the fill by trade_id (so filled_qty is not double-counted), but the
    // duplicate wire event is still forwarded downstream regardless - that is the intended
    // divergence a host's classify layer must see. So two identical Filled
    // events must reach the sink. (The mirror's internal dedup is not observable
    // from the egress sink; it is covered by the client.rs unit tests. This test
    // pins the wire-forwarding half of A.1.)
    let state = Arc::new(StubState::default());
    {
        let mut frames = state.ws_exec_frames.lock().expect("ws exec frames mutex");
        frames.push(
            r#"{"type":"OrderAccepted","client_order_id":"O-1","venue_order_id":"V-1","ts_event":10}"#
                .to_string(),
        );
        let fill = r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"1","last_px":"100.00","leaves_qty":"0","commission":"0","commission_currency":"USDT","liquidity_side":"taker","ts_event":11}"#;
        frames.push(fill.to_string());
        frames.push(fill.to_string());
    }
    let (_client, mut rx) = submit_exec_client(state).await;

    let timeout = Duration::from_secs(3);
    assert!(matches!(
        next_exec_event(&mut rx, timeout, "the local OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout, "the venue's OrderAccepted").await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    let first = match next_exec_event(&mut rx, timeout, "the FIRST of the duplicated fills").await {
        ExecutionEvent::Order(OrderEventAny::Filled(fill)) => fill,
        other => panic!("expected first OrderFilled, got {other:?}"),
    };
    let second = match next_exec_event(&mut rx, timeout, "the DUPLICATE fill behind it").await {
        ExecutionEvent::Order(OrderEventAny::Filled(fill)) => fill,
        other => panic!("expected the duplicate OrderFilled to be forwarded, got {other:?}"),
    };
    assert_eq!(
        first.trade_id, second.trade_id,
        "both forwarded fills are the same economic trade"
    );
    assert_eq!(first.last_qty, second.last_qty);
    assert_eq!(first.last_px, second.last_px);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn divergence_dropped_account_update_leaves_fill_without_snapshot() {
    // `DropNextAccountUpdate` swallows the account-state snapshot that would
    // normally follow a fill. The stub models the result: it pushes the fill but
    // no AccountState at all. The adapter must forward the fill and emit no Account
    // event - the deliberate account drift the divergence exists to inject.
    let state = Arc::new(StubState::default());
    {
        let mut frames = state.ws_exec_frames.lock().expect("ws exec frames mutex");
        frames.push(
            r#"{"type":"OrderAccepted","client_order_id":"O-1","venue_order_id":"V-1","ts_event":10}"#
                .to_string(),
        );
        frames.push(
            r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"1","last_px":"100.00","leaves_qty":"0","commission":"0","commission_currency":"USDT","liquidity_side":"taker","ts_event":11}"#
                .to_string(),
        );
        // Deliberately no AccountState frame.
    }
    let (_client, mut rx) = submit_exec_client(state).await;

    let timeout = Duration::from_secs(3);
    assert!(matches!(
        next_exec_event(&mut rx, timeout, "the local OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout, "the venue's OrderAccepted").await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    assert!(matches!(
        next_exec_event(
            &mut rx,
            timeout,
            "the fill whose account update was dropped"
        )
        .await,
        ExecutionEvent::Order(OrderEventAny::Filled(_))
    ));
    // No account snapshot follows the dropped update.
    let tail = tokio::time::timeout(Duration::from_millis(400), rx.recv()).await;
    assert!(
        tail.is_err(),
        "a dropped account update must leave no AccountState event after the fill"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn divergence_go_dark_within_the_idle_timeout_is_ridden_out() {
    // `GoDark { ms }` blacks the venue out for a window; the stub models it on
    // the data WS leg by emitting no application frame for `dark_ms`.
    //
    // What the client decides about a blackout is the idle clock, and that is
    // the production surface this pins. A blackout shorter than
    // `idle_timeout_ms` must be ridden out: the socket stays up, nothing is
    // re-dialled, and the frame held behind the blackout is delivered when it
    // lifts. Its twin below pins the other side of the same decision.
    //
    // Without the idle timeout set this test asserted only that the stub slept,
    // which no production edit could falsify - the blackout is the venue's
    // behaviour, and the venue here is the harness.
    let dark_ms = 300usize;
    let state = Arc::new(StubState::default());
    state.dark_ms.store(dark_ms, Ordering::Relaxed);
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;

    let start = Instant::now();
    // Comfortably beyond the blackout plus the harness's own pre-push delay, so
    // a green run means the idle clock tolerated the silence rather than that
    // the two budgets happened not to overlap.
    let (client, mut rx) = connect_data_client(
        base_url,
        &state,
        Some(conn_havoc(ConnHavoc {
            idle_timeout_ms: 1_500,
            ..ConnHavoc::default()
        })),
    )
    .await;

    // Within the dark window no stream frame arrives - the instrument
    // prologue does (connect() emits it to the sink directly, not over the
    // suppressed WS leg), so tolerate exactly that.
    //
    // The window ends when the stub speaks, not at a fixed duration. A fixed
    // window starts when `connect()` returns, which races the harness's own
    // pre-push and blackout delays: on a slow debug build a long enough connect
    // pushes the frame inside the window and fails the test for nothing.
    // `ws_first_frame_at` is stamped strictly before the send, so observing it
    // set is a sound place to stop looking.
    while state
        .ws_first_frame_at
        .lock()
        .expect("ws first frame instant mutex")
        .is_none()
    {
        while let Ok(event) = rx.try_recv() {
            assert!(
                matches!(event, DataEvent::Instrument(_)),
                "no stream data may reach the sink before the blackout lifts; got {event:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // After the window lifts, the held trade is delivered.
    let trade = next_trade(&mut rx).await;
    assert_eq!(trade.ts_event, UnixNanos::from(10));
    assert!(
        start.elapsed() >= Duration::from_millis(dark_ms as u64),
        "the trade must not arrive before the dark window elapsed"
    );
    assert!(
        !client.is_disconnected(),
        "a blackout inside the idle timeout must not take the client down"
    );
    assert_eq!(
        state.ws_handshakes.load(Ordering::Relaxed),
        1,
        "a blackout inside the idle timeout must not cost a re-dial"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn divergence_go_dark_past_the_idle_timeout_is_read_as_a_dead_socket() {
    // The other side of the blackout decision: a `GoDark` window longer than
    // `idle_timeout_ms` is indistinguishable from a dead socket, and the client
    // must say so by dropping the connection and re-dialling. That is the cost
    // of the divergence a host has to know about, and it is the arm that
    // actually exercises `WsAction::Idle` - deleting that arm's `break` changed
    // nothing anywhere in this crate before this test existed.
    //
    // Ping and Pong deliberately never reset the idle clock (see `ConnHavoc`),
    // so no heartbeat is armed here: the silence under test is application
    // silence, and a heartbeat would neither rescue nor hasten it.
    //
    // The fixture seeds a trade, and that is what makes `dark_ms` load-bearing.
    // With an empty tape the socket is application-silent forever whatever
    // `dark_ms` says, so deleting the blackout would not have moved the verdict
    // and the test would have pinned "a permanently dead venue is re-dialled" -
    // the trivial case the blackout was supposed to exclude. With a trade
    // seeded, the venue would have spoken as soon as the push gate opened -
    // which is the instant the subscription is recorded, well inside the idle
    // window - and only the blackout keeps it quiet past the idle timeout, so
    // the assertions below separate the two. The straddle used to be stated
    // against the harness's fixed 100 ms pre-push sleep; with the gate the
    // pre-push interval is no longer a wall duration at all, which makes the
    // separation wider rather than narrower.
    let state = Arc::new(StubState::default());
    state.dark_ms.store(600, Ordering::Relaxed);
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(11, "101.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (_client, mut rx) = connect_data_client(
        base_url,
        &state,
        Some(conn_havoc(ConnHavoc {
            // Shorter than the blackout, and long enough that the push the gate
            // releases lands inside it: without the blackout the seeded trade
            // arrives inside the idle window and no socket is ever declared
            // dead.
            idle_timeout_ms: 250,
            // A flat, short backoff: the property is that a second dial happens
            // at all, and the default one-second ladder would only make the
            // window longer for no extra evidence.
            reconnect_delay_initial_ms: 20,
            reconnect_delay_max_ms: 20,
            ..ConnHavoc::default()
        })),
    )
    .await;

    let handshakes = wait_for_at_least(&state.ws_handshakes, 2, Duration::from_secs(3)).await;
    assert!(
        handshakes >= 2,
        "a blackout past the idle timeout must be declared dead and re-dialled; \
         saw {handshakes} handshakes"
    );
    // ...and it was re-dialled because of the blackout, not after being served.
    // A venue that got its frame out before the idle clock expired has not gone
    // dark at all, so nothing but the connect-time prologue may have arrived by
    // the time the second dial happens.
    while let Ok(event) = rx.try_recv() {
        assert!(
            matches!(event, DataEvent::Instrument(_)),
            "the blackout, not a served-then-idle socket, must be what cost the \
             re-dial; got {event:?} before the second handshake"
        );
    }

    // The half a blackout test owes and the deleted one never had: the venue
    // comes back. Lifting `dark_ms` leaves the client's own reconnect loop to
    // find the next socket serving normally, and the held trade is delivered.
    state.dark_ms.store(0, Ordering::Relaxed);
    let trade = next_trade(&mut rx).await;
    assert_eq!(
        trade.ts_event,
        UnixNanos::from(11),
        "the re-dialled socket must deliver the tape once the blackout lifts"
    );
}

/// Havoc reaches the order a trigger produces, and reaches the trigger itself.
///
/// `OrderTriggered` is a new wire variant, and the one thing that decides
/// whether the ack-holding arms (`DelayAcks` on the venue's writer, the
/// latency filter on this end) touch it at all is
/// `VenueMessage::category` - both ends consult that one classifier, which is
/// exactly why a misfiled variant would be invisible in a single-ended test. A
/// trigger filed as `Data` would slip past every execution hold while the fill
/// behind it was held, delivering the two out of order to a strategy.
///
/// The gate is two-sided on purpose. The exec buckets are set to a delay the
/// trigger and the fill must both clear, and the data bucket to one an order
/// of magnitude larger that they must never pay: passing only the first half
/// would also pass if every frame were delayed, and passing only the second
/// would also pass if havoc reached nothing.
///
/// It drives the inbound filter rather than a venue-armed `DelayAcks`
/// because the venue here is the test stub, which has no writer windows to
/// arm - and the classification under test is shared, so the client-side
/// bucket is the same decision observed from the reachable end.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn havoc_reaches_the_order_a_trigger_produces() {
    let held = Duration::from_millis(400);
    let state = Arc::new(StubState::default());
    {
        let mut frames = state.ws_exec_frames.lock().expect("ws exec frames mutex");
        frames.push(
            r#"{"type":"OrderAccepted","client_order_id":"O-STOP","venue_order_id":"V-9","ts_event":10}"#
                .to_string(),
        );
        frames.push(
            r#"{"type":"OrderTriggered","client_order_id":"O-STOP","venue_order_id":"V-9","ts_event":11}"#
                .to_string(),
        );
        frames.push(
            r#"{"type":"OrderFilled","client_order_id":"O-STOP","venue_order_id":"V-9","trade_id":"T-9","symbol":"BTCUSDT","side":"Sell","last_qty":"1","last_px":"94.97","leaves_qty":"0","commission":"0","commission_currency":"USDT","liquidity_side":"taker","ts_event":12}"#
                .to_string(),
        );
    }
    let havoc = HavocSpec {
        inbound: InboundHavoc {
            latency: Some(HavocLatency {
                base_nanos: 0,
                exec_event_nanos: 400_000_000,
                fill_nanos: 400_000_000,
                // An order of magnitude beyond the exec buckets: a trigger or a
                // fill misfiled as market data would pay this instead, and the
                // upper bound below would catch it.
                data_nanos: 4_000_000_000,
            }),
            ..InboundHavoc::default()
        },
        ..HavocSpec::default()
    };

    let state_probe = Arc::clone(&state);
    let (_client, mut rx) = submit_stop_exec_client(state, havoc).await;

    // Submitted is emitted locally by the client, never over the wire, so it
    // pays no inbound latency.
    assert!(matches!(
        next_exec_event(&mut rx, Duration::from_secs(6), "the local OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, Duration::from_secs(6), "the held OrderAccepted").await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    match next_exec_event(&mut rx, Duration::from_secs(6), "the held OrderTriggered").await {
        ExecutionEvent::Order(OrderEventAny::Triggered(_)) => {}
        other => panic!("expected the held OrderTriggered, got {other:?}"),
    }
    let triggered = Instant::now();
    match next_exec_event(
        &mut rx,
        Duration::from_secs(6),
        "the held fill behind the trigger",
    )
    .await
    {
        ExecutionEvent::Order(OrderEventAny::Filled(_)) => {}
        other => panic!("expected the held fill behind the trigger, got {other:?}"),
    }
    let filled = Instant::now();

    // The clock starts at the stub's send. It used to start before `connect()`,
    // on the reasoning that setup time only ever adds and so leaves the lower
    // bound honest. That reasoning was wrong here, and measurably so: connect
    // does not return until `await_account_registered` sees the seeded snapshot,
    // which arrives through this very pump and therefore pays the armed 400 ms
    // exec delay inside connect. Setup measured 416.7-418.7 ms over 40 runs -
    // past the 400 ms hold on its own, so the lower bound below passed in every
    // run before a single execution frame was classified. `ws_first_exec_frame_at`
    // is the instant the stub put the OrderAccepted on the wire; the interval
    // from there is the filter's contribution and nothing else.
    let sent = state_probe
        .ws_first_exec_frame_at
        .lock()
        .expect("ws first exec frame instant mutex")
        .expect("the stub recorded when it put the exec frames on the wire");
    let triggered_at = triggered.duration_since(sent);
    let filled_at = filled.duration_since(sent);

    assert!(
        triggered_at >= held,
        "OrderTriggered arrived {triggered_at:?} after the stub sent it, before the \
         {held:?} execution hold: it is not classified as execution traffic"
    );
    assert!(
        filled_at >= held,
        "the fill behind the trigger arrived {filled_at:?} after the stub sent it, \
         before the {held:?} hold"
    );
    // Anchored at the send, the honest arrival is the 30 ms baseline plus the
    // 400 ms exec hold - measured at ~475 ms end to end before the re-anchor.
    // The defect is the 4,030 ms data bucket, so two seconds sits ~1.5 s above
    // the honest value and ~2 s below the defect, where the old three seconds
    // was measuring from a clock that had already spent 418 ms.
    assert!(
        triggered_at < Duration::from_secs(2) && filled_at < Duration::from_secs(2),
        "trigger {triggered_at:?} / fill {filled_at:?} paid the four-second DATA bucket: \
         one of them is misfiled as market data"
    );
}

/// The venue's second-cadence refusal is conditional, so the client must still
/// be dialling when it lifts.
///
/// `docs/accounts.md`: the rule holds while any of the account's passengers is
/// riding that river and lifts once the last leaves. The incumbent need not be
/// ours - any process naming this account id may hold that river - so nothing
/// this client does clears it and nothing it can observe proves it never will.
/// A round-4 fix pass read the refusal as permanent and disabled reconnect on
/// it, which loses the run for good the moment the incumbent disconnects.
///
/// The stub answers the first two upgrades with the venue's real 400, marker
/// and all, then serves the third. This exercises the transition rather than
/// the classifier: what is asserted is that the connection the client finally
/// gets is a working one, which is only reachable through the retry ladder.
#[tokio::test]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_cadence_conflict_is_retried_until_the_incumbent_leaves() {
    let state = Arc::new(StubState::default());
    state.cadence_refusals.store(2, Ordering::Relaxed);
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(11, "101.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);
    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        symbol: None,
        callsign: None,
        // A flat, short backoff. The property is that the ladder survives the
        // refusals at all; the default one-second ladder would only make the
        // test slower for no extra evidence. The attempt cap stays at its
        // default, well above the two refusals, so an exhausted cap cannot be
        // mistaken for the terminal behaviour this test forbids.
        havoc: Some(conn_havoc(ConnHavoc {
            reconnect_delay_initial_ms: 20,
            reconnect_delay_max_ms: 20,
            ..ConnHavoc::default()
        })),
        expected_run_seed: None,
        ..Default::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");

    // `connect()` returns once a transport is up, so it is the assertion. It is
    // bounded rather than awaited: a client that treats the refusal as terminal
    // abandons the connection task and readiness never arrives, and this must
    // fail by name rather than by hanging out the runner's per-test budget. The
    // ladder here is three loopback dials and two 20 ms backoffs, so five
    // seconds is a failure deadline and not a budget the passing path spends.
    tokio::time::timeout(Duration::from_secs(5), client.connect())
        .await
        .expect(
            "the client stopped dialling on a cadence refusal: it is conditional and lifts \
             when the incumbent passenger leaves",
        )
        .expect("connect opens transports once the conflict clears");
    subscribe(&mut client);
    state.push_gate.open();

    // It got there by re-dialling, rather than by the stub having served the
    // first upgrade after all.
    let handshakes = state.ws_handshakes.load(Ordering::Relaxed);
    assert!(
        handshakes >= 3,
        "two refusals then a served upgrade is three dials; saw {handshakes}"
    );

    // And the connection it finally got is a real one, not merely an accepted
    // socket: the seeded trade crosses it.
    let trade = next_trade(&mut rx).await;
    assert_eq!(
        trade.price,
        nautilus_model::types::Price::from("101.00"),
        "the post-conflict connection must carry the tape like any other"
    );
}
