// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Ignored havoc tests for the mogwai adapter.
//!
//! These bind a real TCP listener and exercise behavior that unit tests cannot
//! reach: server-side divergence shipping, client-side inbound corruption,
//! connection-lifecycle havoc, and the end-to-end divergence surfaces the suite
//! claims (partial fills, duplicate fills, dropped account updates, blackouts).
//! They share the stub harness in `tests/common`.
//!
//! Run a focused case in a socket-capable environment with e.g.
//! `brokkr test -p mogwai-adapter ships_server_havoc --debug`.

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
use mogwai_protocol::{ClientHavoc, ConnHavoc, HavocLatency, HavocSpec, control::Divergence};
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
    let base_url = bound_stub(state).await;
    let (sink_tx, sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        havoc,
        expected_run_seed: None,
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    subscribe(&mut client);
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

fn data_havoc(client: ClientHavoc) -> HavocSpec {
    HavocSpec {
        client,
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
    havoc: Option<HavocSpec>,
) -> (MogwaiDataClient, UnboundedReceiver<DataEvent>) {
    let (sink_tx, sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        // Stated, not defaulted: the default is a placeholder the validator
        // refuses, so no socket binds an account nobody chose.
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        havoc,
        expected_run_seed: None,
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    subscribe(&mut client);
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
async fn assert_only_instrument_prologue(rx: &mut UnboundedReceiver<DataEvent>, window: Duration) {
    let deadline = Instant::now() + window;
    loop {
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
        let value = count.load(Ordering::Relaxed);
        if value >= target || Instant::now() >= deadline {
            return value;
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
/// supplied client havoc. The order shape is what differs: only a conditional
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
async fn ships_server_havoc() {
    let state = Arc::new(StubState::default());
    let base_url = bound_stub(Arc::clone(&state)).await;
    // Engine-side single-shots only: this spec rides HTTP transport profiles
    // below, and validate() now refuses a server temporal window (GoDark,
    // DelayAcks, StallData under polling) the chosen carrier cannot deliver.
    let havoc = HavocSpec {
        client: ClientHavoc::default(),
        server: vec![
            Divergence::RejectNextSubmit {
                reason: "nope".into(),
            },
            Divergence::DropNextAccountUpdate,
        ],
        data: None,
        conn: ConnHavoc::default(),
    };

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
            .filter_map(|body| serde_json::from_str(body).ok())
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
        havoc: Some(havoc),
        expected_run_seed: None,
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

    // Connect + subscribe FIRST, then start the clock, so the lower bound
    // measures only the latency filter's contribution and not the unbounded
    // connect/subscribe/handshake setup. The reader task that owns the latency
    // sleep does not see the trade until the stub pushes it after the Subscribe;
    // the stub pushes immediately on Subscribe, so the only material delay
    // between this instant and delivery is the composed latency.
    let mut rx = subscribed_data_client(state, Some(havoc)).await;
    let start = Instant::now();
    let _trade = next_trade(&mut rx).await;

    let elapsed = start.elapsed();
    assert!(
        elapsed >= delay,
        "inbound trade arrived {elapsed:?} after subscribe, before the composed {delay:?} delay"
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
    let havoc = data_havoc(ClientHavoc {
        drop_prob: 1.0,
        seed: Some(1),
        ..ClientHavoc::default()
    });

    let mut rx = subscribed_data_client(state, Some(havoc)).await;

    // Panics if any of the five dropped trades reaches the sink; only the
    // connect-time instrument prologue may arrive.
    assert_only_instrument_prologue(&mut rx, Duration::from_millis(400)).await;
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
    let havoc = data_havoc(ClientHavoc {
        duplicate_prob: 1.0,
        seed: Some(1),
        ..ClientHavoc::default()
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
    let havoc = data_havoc(ClientHavoc {
        reorder_prob: 1.0,
        seed: Some(1),
        ..ClientHavoc::default()
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

/// A client bound to a run refuses an address serving a DIFFERENT run, and says
/// so in one named line rather than dying as a generic connect failure.
///
/// The exposure this closes: the venue's port is ephemeral and is freed BEFORE
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
        havoc: None,
        // Bound to a run this stub is not serving.
        expected_run_seed: Some(7),
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the data-event sink");
    // Refused, so the client never reports connected and `connect` gives up
    // waiting. The DISTINCT signal is the named `venue identity mismatch` error
    // line the loop logs before returning; at this API the outcome is simply
    // that the client never comes up, which is the correct end state either way.
    drop(client.connect().await);

    assert!(
        client.is_disconnected(),
        "a client bound to run 7 must not stay connected to run 4242"
    );
    // The refusal is terminal, so the loop stops dialing rather than walking the
    // reconnect ladder against an address it has already judged.
    let handshakes = state.ws_handshakes.load(Ordering::Relaxed);
    assert!(
        handshakes <= 2,
        "a refused identity must not be retried, saw {handshakes} handshakes"
    );
}

/// An identity check the venue cannot answer is NOT a mismatch. A probe fails
/// for the same transport reasons a socket does, and refusing on that would turn
/// a blip into a dead client - so a venue with no `/health` is used, not judged.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn an_unanswerable_identity_probe_does_not_refuse() {
    let state = Arc::new(StubState::default());
    // Serving the run this client expects, so the only question is reachability.
    state.run_seed.store(7, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        havoc: None,
        expected_run_seed: Some(7),
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs the data-event sink");
    client.connect().await.expect("connect is spawned");

    tokio::time::sleep(Duration::from_millis(600)).await;
    assert!(
        !client.is_disconnected(),
        "a matching run must be used, not refused"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_reconnect_respects_max_attempts() {
    // The stub refuses the WebSocket upgrade on every dial. With a three-attempt
    // cap the client must dial at least three times (one initial plus two
    // retries) and then give up disconnected - and never dial a fifth. The exact
    // contract is "disconnected after the cap"; pin the count with a robust
    // lower+upper bound rather than a brittle exact-count under scheduler delay,
    // and assert `is_disconnected`.
    let state = Arc::new(StubState::default());
    state.refuse_ws.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, _sink_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(sink_tx);

    let config = MogwaiDataClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url,
        havoc: Some(conn_havoc(ConnHavoc {
            reconnect_max_attempts: Some(3),
            reconnect_delay_initial_ms: 30,
            reconnect_delay_max_ms: 30,
            ..ConnHavoc::default()
        })),
        expected_run_seed: None,
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    // `connect` resolves the sink first, so the client must be started or it
    // errors out before ever dialing the WS (no handshake would reach the stub).
    client.start().expect("start grabs the data-event sink");
    // The dials all fail, so the loop exhausts the cap and the connect helper's
    // readiness wait never succeeds; bound the await rather than block on it.
    drop(tokio::time::timeout(Duration::from_secs(2), client.connect()).await);

    // Give any erroneous extra dial time to land before pinning the count.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let handshakes = state.ws_handshakes.load(Ordering::Relaxed);
    assert!(
        (3..=4).contains(&handshakes),
        "a three-attempt cap must dial three times (initial plus two retries), \
         tolerating one racing extra at most; saw {handshakes}"
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
    // The interval is 50ms; over a ~600ms window we expect several, so we assert
    // the pacing produced more than one rather than a single accidental probe.
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

    let pings = wait_for_at_least(&state.ws_pings, 3, Duration::from_secs(2)).await;
    assert!(
        pings >= 3,
        "heartbeat did not pace several Pings in the window (pings={pings})"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_handles_inbound_server_ping() {
    // The stub sends an unsolicited WS Ping after the subscribe. The client's
    // reader must answer it with a Pong (lifecycle Ping -> Pong path); the stub
    // counts the Pong. This proves the inbound control-frame reply path that the
    // heartbeat test (client-initiated Ping) never reaches.
    let state = Arc::new(StubState::default());
    state.ws_server_pings.store(1, Ordering::Relaxed);
    // Keep the socket lively with one trade so the reader loop runs.
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, _rx) = connect_data_client(base_url, None).await;

    let pongs = wait_for_at_least(&state.ws_pongs, 1, Duration::from_secs(2)).await;
    assert!(
        pongs >= 1,
        "client did not Pong the server's inbound Ping (pongs={pongs})"
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
        next_exec_event(&mut rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    match next_exec_event(&mut rx, timeout).await {
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
    // duplicate wire event is STILL forwarded downstream - that is the intended
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
        next_exec_event(&mut rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    let first = match next_exec_event(&mut rx, timeout).await {
        ExecutionEvent::Order(OrderEventAny::Filled(fill)) => fill,
        other => panic!("expected first OrderFilled, got {other:?}"),
    };
    let second = match next_exec_event(&mut rx, timeout).await {
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
    // NO AccountState. The adapter must forward the fill and emit no Account
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
        next_exec_event(&mut rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, timeout).await,
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
async fn divergence_go_dark_suppresses_stream_during_window() {
    // `GoDark { ms }` blacks the venue out for a window. The stub models the
    // blackout on the data WS leg: after the Subscribe it holds (suppresses) all
    // application frames for `dark_ms`, then resumes. The client must see NO
    // trade during the dark window, and the trade only after it lifts.
    let dark_ms = 300usize;
    let state = Arc::new(StubState::default());
    state.dark_ms.store(dark_ms, Ordering::Relaxed);
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(10, "100.00"));

    let start = Instant::now();
    let mut rx = subscribed_data_client(state, None).await;

    // Within the dark window no stream frame arrives - the instrument
    // prologue does (connect() emits it to the sink directly, not over the
    // suppressed WS leg), so tolerate exactly that.
    assert_only_instrument_prologue(&mut rx, Duration::from_millis(150)).await;

    // After the window lifts, the held trade is delivered.
    let trade = next_trade(&mut rx).await;
    assert_eq!(trade.ts_event, UnixNanos::from(10));
    assert!(
        start.elapsed() >= Duration::from_millis(dark_ms as u64),
        "the trade must not arrive before the dark window elapsed"
    );
}

/// Havoc reaches the order a trigger produces, and reaches the trigger itself.
///
/// `OrderTriggered` is a new wire variant, and the one thing that decides
/// whether the ack-holding arms (`DelayAcks` on the venue's writer, the
/// latency filter on this end) touch it at all is
/// `ServerMessage::category` - both ends consult that one classifier, which is
/// exactly why a misfiled variant would be invisible in a single-ended test. A
/// trigger filed as `Data` would slip past every execution hold while the fill
/// behind it was held, delivering the two out of order to a strategy.
///
/// The gate is two-sided on purpose. The exec buckets are set to a delay the
/// trigger and the fill must BOTH clear, and the data bucket to one an order
/// of magnitude larger that they must NOT pay: passing only the first half
/// would also pass if every frame were delayed, and passing only the second
/// would also pass if havoc reached nothing.
///
/// It drives the client-side filter rather than a venue-armed `DelayAcks`
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
        client: ClientHavoc {
            latency: Some(HavocLatency {
                base_nanos: 0,
                exec_event_nanos: 400_000_000,
                fill_nanos: 400_000_000,
                // An order of magnitude beyond the exec buckets: a trigger or a
                // fill misfiled as market data would pay THIS instead, and the
                // upper bound below would catch it.
                data_nanos: 4_000_000_000,
            }),
            ..ClientHavoc::default()
        },
        ..HavocSpec::default()
    };

    let start = Instant::now();
    let (_client, mut rx) = submit_stop_exec_client(state, havoc).await;

    // Submitted is emitted locally by the client, never over the wire, so it
    // pays no inbound latency. The clock starts before connect, which only ever
    // ADDS setup time to the measurement - the lower bound stays honest and the
    // upper bound is loose enough to absorb it.
    assert!(matches!(
        next_exec_event(&mut rx, Duration::from_secs(6)).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut rx, Duration::from_secs(6)).await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    match next_exec_event(&mut rx, Duration::from_secs(6)).await {
        ExecutionEvent::Order(OrderEventAny::Triggered(_)) => {}
        other => panic!("expected the held OrderTriggered, got {other:?}"),
    }
    let triggered_at = start.elapsed();
    match next_exec_event(&mut rx, Duration::from_secs(6)).await {
        ExecutionEvent::Order(OrderEventAny::Filled(_)) => {}
        other => panic!("expected the held fill behind the trigger, got {other:?}"),
    }
    let filled_at = start.elapsed();

    assert!(
        triggered_at >= held,
        "OrderTriggered arrived in {triggered_at:?}, before the {held:?} execution hold: \
         it is not classified as execution traffic"
    );
    assert!(
        filled_at >= held,
        "the fill behind the trigger arrived in {filled_at:?}, before the {held:?} hold"
    );
    assert!(
        triggered_at < Duration::from_secs(3) && filled_at < Duration::from_secs(3),
        "trigger {triggered_at:?} / fill {filled_at:?} paid the four-second DATA bucket: \
         one of them is misfiled as market data"
    );
}
