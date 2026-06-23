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

use common::{StubState, bound_stub, cached_order, instrument_id, next_exec_event, trade_json};
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
    live::runner::{replace_data_event_sender, replace_exec_event_sender},
    messages::{DataEvent, ExecutionEvent, data::SubscribeTrades, execution::SubmitOrder},
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    data::Data,
    enums::{AccountType, OmsType},
    events::OrderEventAny,
    identifiers::{ClientId, StrategyId, TraderId},
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
        base_url,
        havoc,
        ..MogwaiDataClientConfig::default()
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
        base_url,
        havoc,
        ..MogwaiDataClientConfig::default()
    };
    let mut client =
        MogwaiDataClient::new(ClientId::from("MOGWAI-DATA"), config).expect("client builds");
    client.start().expect("start grabs sink");
    client.connect().await.expect("connect opens transports");
    subscribe(&mut client);
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
                .any(|d| matches!(d, Divergence::GoDark { ms } if *ms == 25)),
            "GoDark did not round-trip its ms"
        );
    }

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

    let result = tokio::time::timeout(Duration::from_millis(400), rx.recv()).await;
    assert!(
        result.is_err(),
        "none of the five dropped trades may reach the sink"
    );
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

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_idle_timeout_triggers_reconnect() {
    // Silent server: the socket opens but never sends an application frame, so
    // the idle clock must fire and force a reconnect. After the reconnect the
    // stub serves a trade, so we can assert the client recovered into a usable
    // subscription rather than merely counting handshakes (a reconnect storm
    // also satisfies >= 2 handshakes).
    let state = Arc::new(StubState::default());
    state
        .ws_trades
        .lock()
        .expect("ws trades mutex")
        .push(trade_json(99, "100.00"));
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (_client, mut rx) = connect_data_client(
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
    // The re-subscribe after reconnect must drive the trade through: recovery,
    // not just a flap.
    let trade = next_trade(&mut rx).await;
    assert_eq!(
        trade.ts_event,
        UnixNanos::from(99),
        "client did not re-subscribe and recover after the idle reconnect"
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

    // Let the poll loop issue several /trades GETs through the quota gate. Bound
    // the wait so a silently stalled poll fails clearly instead of hanging.
    let deadline = Instant::now() + Duration::from_secs(5);
    let times = loop {
        let times = state
            .http_request_times
            .lock()
            .expect("http request times mutex")
            .clone();
        if times.len() >= 3 {
            break times;
        }
        assert!(
            Instant::now() < deadline,
            "polling stalled before issuing three quota-gated /trades GETs"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    // 2 requests/sec means a 500ms minimum interval. The gap must be AT LEAST
    // 500ms - tolerance belongs on the slow side (scheduler jitter lengthens
    // gaps), never below the true interval, or a throttle shipping at 460ms
    // (faster than 2/sec) would pass.
    for pair in times.windows(2) {
        let gap = pair[1].duration_since(pair[0]);
        assert!(
            gap >= Duration::from_millis(500),
            "data HTTP requests spaced only {gap:?} apart under a 2/sec (>=500ms) quota"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn conn_http_request_timeout_rejects_order() {
    // The order POST is accepted but never answered. With a short per-request
    // timeout the dispatch must elapse and surface the order as a reject via the
    // existing reject path, rather than hanging on the default 30s timeout. The
    // reject must correlate to the timed-out order (right client_order_id), not
    // some other order.
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

    let timeout = Duration::from_secs(3);
    // The submitted event arrives first, then the timeout-driven reject.
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    match next_exec_event(&mut sink_rx, timeout).await {
        ExecutionEvent::Order(OrderEventAny::Rejected(reject)) => {
            assert_eq!(
                reject.client_order_id,
                order.client_order_id(),
                "the timeout reject must name the order that timed out"
            );
        }
        other => panic!("a request that timed out must surface as an order reject, got {other:?}"),
    }
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
            r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"0.4","last_px":"100.00","leaves_qty":"0.6","commission":"0","ts_event":11}"#
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
    // divergence broadarrow's classify layer must see. So two identical Filled
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
        let fill = r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"1","last_px":"100.00","leaves_qty":"0","commission":"0","ts_event":11}"#;
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
            r#"{"type":"OrderFilled","client_order_id":"O-1","venue_order_id":"V-1","trade_id":"T-1","symbol":"BTCUSDT","side":"Buy","last_qty":"1","last_px":"100.00","leaves_qty":"0","commission":"0","ts_event":11}"#
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

    // Within the dark window nothing arrives.
    let during = tokio::time::timeout(Duration::from_millis(150), rx.recv()).await;
    assert!(
        during.is_err(),
        "the stream must be suppressed during the GoDark window"
    );

    // After the window lifts, the held trade is delivered.
    let trade = next_trade(&mut rx).await;
    assert_eq!(trade.ts_event, UnixNanos::from(10));
    assert!(
        start.elapsed() >= Duration::from_millis(dark_ms as u64),
        "the trade must not arrive before the dark window elapsed"
    );
}
