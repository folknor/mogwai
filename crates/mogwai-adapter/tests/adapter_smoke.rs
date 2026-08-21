// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Ignored execution transport smoke test for the mogwai execution client.
//!
//! It uses the shared self-contained HTTP and WebSocket stub (`tests/common`)
//! that speaks enough of the mogwai protocol to verify the public adapter path:
//! start, connect, submit, drain execution events - and that the fill maps to
//! the right order/qty/price/venue, not merely the right event variant.

mod common;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

use common::{
    StubState, assert_owns_a_fresh_exec_sink, bound_stub, cached_order, cached_stop_limit,
    cached_stop_market, cached_trailing_stop, connected_exec_client, instrument_id,
    next_exec_event, submit_command,
};
use mogwai_adapter::{
    MOGWAI_VENUE, MogwaiDataClient, MogwaiDataClientConfig, MogwaiExecClientConfig,
    MogwaiExecutionClient,
};
use nautilus_common::{
    cache::Cache,
    clients::{DataClient, ExecutionClient},
    live::runner::{replace_data_event_sender, replace_exec_event_sender},
    messages::{
        DataEvent, ExecutionEvent,
        execution::{ModifyOrder, SubmitOrder},
    },
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::{ContingencyType, OmsType, OrderStatus, TriggerType},
    events::OrderEventAny,
    identifiers::{
        AccountId, ClientId, ClientOrderId, InstrumentId, OrderListId, PositionId, StrategyId,
        Symbol, TraderId, VenueOrderId,
    },
    orders::Order,
    types::{Price, Quantity},
};
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn connect_seeds_initial_account_state() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let client = connected_exec_client(base_url, Rc::clone(&cache), &mut sink_rx).await;
    assert!(client.get_account().is_some(), "account registered");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_cash_configured_client_still_connects_to_a_futures_run() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);
    let client = connected_exec_client(
        base_url,
        Rc::new(RefCell::new(Cache::default())),
        &mut sink_rx,
    )
    .await;
    assert!(client.is_connected());
}

/// THE TWO LEGS OF ONE HOST DISCLOSE ONE SESSION, and it is this process's.
///
/// This is the property `process_session_id`'s whole design rests on, and until
/// this test it had no end-to-end coverage at all: `MogwaiDataClientConfig` and
/// `MogwaiExecClientConfig` both default `session` to it, `ws_url` appends it,
/// and `StubState::ws_requests` records the request line precisely so a
/// disclosure is assertable - but every socket assertion in these four binaries
/// read that list for `account=` and nothing read it for `session=`. Making
/// `default_session` return `None` left every one of them green.
///
/// What breaks without it is not this test's stub, which seats everyone: the
/// VENUE seats one client per ledger and evicts a second, so a nautilus host -
/// which holds two sockets on one account by construction - would disconnect
/// its own data leg the instant its execution leg dialled. That failure is
/// invisible from either client's own side, which is why the assertion has to
/// be on what crossed the wire.
///
/// THREE THINGS, and each is separately losable. The parameter is PRESENT on
/// both upgrades; the two values are EQUAL, which is the anti-eviction property
/// itself; and the value is wire-legal by the venue's own rule, because the
/// URL is built by concatenation and a session needing percent-encoding would
/// fail as an unreadable 400 from inside the reconnect loop.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn both_legs_disclose_one_process_session_on_the_upgrade() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    // The exec leg, built exactly as every other test in this binary builds one,
    // so what is asserted here is the DEFAULT a host gets rather than a fixture.
    let (exec_tx, mut exec_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(exec_tx);
    let _exec = connected_exec_client(
        base_url.clone(),
        Rc::new(RefCell::new(Cache::default())),
        &mut exec_rx,
    )
    .await;

    // ...and the data leg of the same host, on the same ledger. `session` is
    // left at its default on purpose: stating it would test the fixture.
    let (data_tx, _data_rx) = unbounded_channel::<DataEvent>();
    replace_data_event_sender(data_tx);
    let mut data = MogwaiDataClient::new(
        ClientId::from("MOGWAI-DATA"),
        MogwaiDataClientConfig {
            account_id: AccountId::from("MOGWAI-001"),
            base_url,
            ..MogwaiDataClientConfig::default()
        },
    )
    .expect("data client builds");
    data.start().expect("start grabs the data-event sink");
    data.connect().await.expect("the data leg connects");

    // POLLED, NOT READ ON THE NEXT LINE: `ws_requests` is written by the stub's
    // per-connection handler tasks, so an empty or short list means "not
    // recorded yet" exactly as readily as "never sent". The two seconds are a
    // failure deadline, not a wait this test spends on its passing path.
    let deadline = Instant::now() + Duration::from_secs(2);
    let requests = loop {
        let seen = state.ws_requests.lock().expect("ws request mutex").clone();
        if seen.len() >= 2 || Instant::now() >= deadline {
            break seen;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(
        requests.len() >= 2,
        "both legs must have upgraded, or there is no pair to compare: {requests:?}"
    );

    let sessions: Vec<String> = requests
        .iter()
        .map(|line| {
            session_query_value(line)
                .unwrap_or_else(|| panic!("every upgrade must disclose a session: {line}"))
        })
        .collect();
    let first = &sessions[0];
    assert!(
        sessions.iter().all(|session| session == first),
        "the two legs of one process are ONE client of one ledger; differing \
         sessions make the venue evict one with the other: {sessions:?}"
    );
    // Wire-legal by the venue's rule, not by a local charset guess, so the two
    // ends cannot drift on what a URL may carry.
    mogwai_protocol::validate_session_id(first).unwrap_or_else(|reason| {
        panic!("the minted session {first:?} is not wire-legal: {reason}")
    });
    // ...and it is THIS PROCESS's, which is what makes a restarted worker get a
    // fresh identity and reclaim its ledger from the sockets of the dead one. A
    // constant compiled into the adapter would satisfy everything above.
    let expected_prefix = format!("mogwai-{pid}-", pid = std::process::id());
    assert!(
        first.starts_with(&expected_prefix),
        "the session must be minted from this process's identity ({expected_prefix}...), got {first:?}"
    );
}

/// Reads `session=` out of a recorded `/ws` request line. Deliberately exact on
/// the key rather than a substring search for the word: a future query
/// parameter merely CONTAINING it must not stand in for the disclosure.
fn session_query_value(request_line: &str) -> Option<String> {
    request_line
        .split_whitespace()
        .nth(1)?
        .split_once('?')?
        .1
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(name, _)| *name == "session")
        .map(|(_, value)| value.to_owned())
        .filter(|value| !value.is_empty())
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_submitted_position_id_reaches_the_wire() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    state.ws_exec_frames.lock().unwrap().push(
        r#"{"type":"OrderAccepted","client_order_id":"O-1","venue_order_id":"V-1","ts_event":10}"#
            .into(),
    );
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);
    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_order(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
    client
        .submit_order(SubmitOrder::new(
            TraderId::from("MOGWAI-001"),
            Some(ClientId::from("MOGWAI-EXEC")),
            StrategyId::from("S-001"),
            instrument_id(),
            order.client_order_id(),
            order.init_event().clone(),
            None,
            Some(PositionId::from("BTCUSDT-7")),
            None,
            UUID4::new(),
            UnixNanos::default(),
            None,
        ))
        .unwrap();
    // DRAINED TO A DEADLINE, the same discipline as
    // `an_order_list_reaches_the_wire_as_linked_legs`, which asks the same
    // question of the same list. This used to drain two execution events and
    // then read `ws_client_messages` once, which was SAFE - the accept can only
    // exist because the submit crossed - but safe by an inference about the
    // stub's reply rather than by observing the thing asserted on, and the two
    // tests then answered "did this field reach the wire" two different ways.
    // The inference also breaks silently for any future fixture that acks
    // without echoing, at which point the read races the writer task.
    let deadline = Instant::now() + Duration::from_secs(2);
    let submit = loop {
        let found = state
            .ws_client_messages
            .lock()
            .expect("ws client messages mutex")
            .iter()
            .find(|message| message.contains("SubmitOrder"))
            .cloned();
        if let Some(submit) = found {
            break submit;
        }
        assert!(
            Instant::now() < deadline,
            "the client never put a SubmitOrder on the wire: {:?}",
            state
                .ws_client_messages
                .lock()
                .expect("ws client messages mutex")
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(submit.contains(r#""position_id":"BTCUSDT-7""#), "{submit}");
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn adapter_submit_drives_live_exec_events() {
    // The exec WS leg replies to the client's `SubmitOrder` with accept, fill
    // and account frames. The fill names order `O-1`, venue id `V-1`, qty `1`,
    // price `100.00`; the account snapshot carries a `9900` USDT balance.
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
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
        frames.push(
            r#"{"type":"AccountState","account_id":"MOGWAI-001","balances":[{"currency":"USDT","total":"9900","free":"9900","locked":"0"}],"positions":[{"symbol":"BTCUSDT","quantity":"1","avg_px":"100.00"}],"ts_event":12}"#
                .to_string(),
        );
    }
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_order(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
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

    let timeout = Duration::from_secs(2);
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout, "the local OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));

    match next_exec_event(&mut sink_rx, timeout, "the venue's OrderAccepted").await {
        ExecutionEvent::Order(OrderEventAny::Accepted(accepted)) => {
            assert_eq!(accepted.client_order_id, order.client_order_id());
            assert_eq!(accepted.venue_order_id, VenueOrderId::from("V-1"));
        }
        other => panic!("expected an OrderAccepted, got {other:?}"),
    }

    // The fill must map onto the right order, venue id, instrument, qty, price -
    // a mis-mapped fill (wrong order, wrong scale, dropped venue id) would still
    // be a `Filled` variant but is caught here.
    match next_exec_event(&mut sink_rx, timeout, "the venue's OrderFilled").await {
        ExecutionEvent::Order(OrderEventAny::Filled(fill)) => {
            assert_eq!(fill.client_order_id, order.client_order_id());
            assert_eq!(fill.venue_order_id, VenueOrderId::from("V-1"));
            assert_eq!(fill.instrument_id, instrument_id());
            assert_eq!(fill.instrument_id.venue, *MOGWAI_VENUE);
            assert_eq!(fill.last_qty, Quantity::from("1"));
            assert_eq!(fill.last_px, Price::from("100.00"));
        }
        other => panic!("expected an OrderFilled, got {other:?}"),
    }

    match next_exec_event(&mut sink_rx, timeout, "the pushed AccountState").await {
        ExecutionEvent::Account(account) => {
            let usdt = account
                .balances
                .iter()
                .find(|b| b.currency.code.as_str() == "USDT")
                .expect("account carries a USDT balance");
            assert_eq!(usdt.total.as_decimal(), rust_decimal::Decimal::from(9900));
        }
        other => panic!("expected an AccountState, got {other:?}"),
    }
}

/// A venue that labels its ledger differently from this client is SERVED, not
/// refused and not silently ignored.
///
/// Both halves of that used to fail. The connect path asserted equality and
/// killed the client; the pushed path dropped every mismatched snapshot, so a
/// client would take its fills while its balances quietly stopped moving. Both
/// were per-account-slot invariants that outlived the slots, and THE SCOPE THAT
/// SURVIVES IS THE CONNECTION rather than the venue: a venue does seat several
/// ledgers, keyed by account plus session, but a socket names exactly one on its
/// `/ws?account=` upgrade, so the account this connection carries is the only
/// one it can ever see and a label cannot mean it belongs elsewhere. The full
/// argument, and what would have to change first, is in
/// `reference/architecture.md`.
///
/// The label here is deliberately nothing like the configured `MOGWAI-001`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn an_account_labelled_differently_is_still_served() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
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
        frames.push(
            r#"{"type":"AccountState","account_id":"SANDBOX-042","balances":[{"currency":"USDT","total":"9900","free":"9900","locked":"0"}],"positions":[{"symbol":"BTCUSDT","quantity":"1","avg_px":"100.00"}],"ts_event":12}"#
                .to_string(),
        );
    }
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_order(&cache);
    // Connecting at all is the first half: this pulls GET /account, which used
    // to be a fatal equality check.
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
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

    // DRAINED TO A DEADLINE RATHER THAN COUNTED, because `next_exec_event`
    // PANICS on timeout: a regression that drops the differently-labelled
    // snapshot produces fewer than four events, so the loop died on the panic
    // "execution event arrives" and the written message below was unreachable -
    // the failure named the harness instead of the property. Running out of
    // events is now the loop's normal exit and the assertion does the talking.
    //
    // SELECTED BY BALANCE, and the LABEL IS ASSERTED. `9900` is the seeded
    // snapshot's fingerprint against the connect-time snapshot's `10000`, so the
    // right event is picked rather than assumed to be first. The id it carries
    // must be the CONFIGURED `MOGWAI-001`, not the wire's `SANDBOX-042`: the
    // venue's label is a label and `note_account_label` says so out loud while
    // the client keeps its own id. Asserting the wire id here would pin the
    // opposite of the design; asserting nothing, as this test did, would let a
    // regression propagate the venue's label into every downstream event.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut saw_account = false;
    let mut totals: Vec<String> = Vec::new();
    while !saw_account {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let Ok(Some(event)) = tokio::time::timeout(remaining, sink_rx.recv()).await else {
            break;
        };
        if let ExecutionEvent::Account(account) = event {
            let usdt = account
                .balances
                .iter()
                .find(|b| b.currency.code.as_str() == "USDT")
                .expect("account carries a USDT balance");
            if usdt.total.as_decimal() != rust_decimal::Decimal::from(9900) {
                totals.push(usdt.total.to_string());
                continue;
            }
            assert_eq!(
                account.account_id,
                AccountId::from("MOGWAI-001"),
                "the venue's label is a label: a served snapshot must carry the \
                 CONFIGURED account id, whatever the wire called the ledger"
            );
            saw_account = true;
        }
    }
    assert!(
        saw_account,
        "a differently-labelled account snapshot must reach the sink, not be \
         dropped; saw USDT totals {totals:?}"
    );
}

/// The one test that proves the conditional refusal is actually gone end to
/// end: a real nautilus `StopMarketOrder` through the real `ExecutionClient`,
/// over a real socket, producing `Submitted -> Accepted -> Triggered -> Filled`
/// on the emitter. Before this landing the submit failed at `wire_order_type`
/// and no frame ever left the client.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn adapter_submits_a_stop_market_and_sees_triggered_then_filled() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
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
        // Adverse for a sell: the triggering print was 95.00 and the fill lands
        // below it, which is what the band does to a triggered stop-market.
        frames.push(
            r#"{"type":"OrderFilled","client_order_id":"O-STOP","venue_order_id":"V-9","trade_id":"T-9","symbol":"BTCUSDT","side":"Sell","last_qty":"1","last_px":"94.97","leaves_qty":"0","commission":"0","commission_currency":"USDT","liquidity_side":"taker","ts_event":12}"#
                .to_string(),
        );
    }
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_stop_market(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
    client
        .submit_order(submit_command(&order, order.init_event().clone()))
        .expect("a stop-market is no longer refused at conversion");

    let timeout = Duration::from_secs(2);
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout, "the stop-market's OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    match next_exec_event(&mut sink_rx, timeout, "the stop-market's OrderAccepted").await {
        ExecutionEvent::Order(OrderEventAny::Accepted(accepted)) => {
            assert_eq!(accepted.client_order_id, order.client_order_id());
            assert_eq!(accepted.venue_order_id, VenueOrderId::from("V-9"));
        }
        other => panic!("expected an OrderAccepted, got {other:?}"),
    }
    match next_exec_event(&mut sink_rx, timeout, "the stop-market's OrderTriggered").await {
        ExecutionEvent::Order(OrderEventAny::Triggered(triggered)) => {
            assert_eq!(triggered.client_order_id, order.client_order_id());
            assert_eq!(triggered.venue_order_id, Some(VenueOrderId::from("V-9")));
            assert_eq!(triggered.instrument_id, instrument_id());
        }
        other => panic!("expected an OrderTriggered between accept and fill, got {other:?}"),
    }
    match next_exec_event(&mut sink_rx, timeout, "the fill the trigger produced").await {
        ExecutionEvent::Order(OrderEventAny::Filled(fill)) => {
            assert_eq!(fill.client_order_id, order.client_order_id());
            assert_eq!(fill.instrument_id.venue, *MOGWAI_VENUE);
            assert_eq!(fill.last_qty, Quantity::from("1"));
            assert_eq!(fill.last_px, Price::from("94.97"));
        }
        other => panic!("expected the fill the trigger produced, got {other:?}"),
    }
    assert_eq!(
        client.mirrored_order_status(order.client_order_id()),
        Some(OrderStatus::Filled)
    );

    // The trigger price, the reduce-only flag and the type must have CROSSED
    // THE WIRE, not merely been accepted by the client: a submit that dropped
    // them would still produce the event sequence above against this stub.
    let sent = state
        .ws_client_messages
        .lock()
        .expect("ws client messages mutex")
        .clone();
    let submit = sent
        .iter()
        .find(|text| text.contains("SubmitOrder"))
        .expect("the client sent a SubmitOrder");
    assert!(submit.contains(r#""order_type":"StopMarket""#), "{submit}");
    assert!(submit.contains(r#""trigger_price":"95.00""#), "{submit}");
    assert!(submit.contains(r#""reduce_only":true"#), "{submit}");
}

/// An ORDER LIST reaching the wire as ONE `SubmitOrderGroup`, its legs in the
/// list's own order and each carrying its own rule.
///
/// The venue models a linkage rather than a list object, so what has to arrive
/// is each leg's rule: the entry naming what it triggers, the exit naming the
/// entry as its parent. What must NOT arrive is two separate submits - and this
/// test used to assert exactly that, because the adapter used to send them.
/// Per-leg dispatch lets the entry FILL before the exit is admitted, at which
/// point the entry's rule adjusts a sibling that is not on the book and the
/// exit arrives at full size. One frame is what makes the admission atomic, and
/// the venue now refuses a linked bare submit outright.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn an_order_list_reaches_the_wire_as_linked_legs() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let entry = cached_order(&cache);
    let exit = cached_stop_market(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;

    let list_id = OrderListId::from("OL-1");
    let mut entry_init = entry.init_event().clone();
    entry_init.order_list_id = Some(list_id);
    entry_init.contingency_type = Some(ContingencyType::Oto);
    entry_init.linked_order_ids = Some(vec![exit.client_order_id()]);
    let mut exit_init = exit.init_event().clone();
    exit_init.order_list_id = Some(list_id);
    exit_init.parent_order_id = Some(entry.client_order_id());

    let list = nautilus_model::orders::OrderList::new(
        list_id,
        instrument_id(),
        StrategyId::from("S-001"),
        vec![entry.client_order_id(), exit.client_order_id()],
        UnixNanos::default(),
    );
    let cmd = nautilus_common::messages::execution::SubmitOrderList::new(
        TraderId::from("MOGWAI-001"),
        Some(ClientId::from("MOGWAI-EXEC")),
        StrategyId::from("S-001"),
        list,
        vec![entry_init, exit_init],
        None,
        None,
        None,
        UUID4::new(),
        UnixNanos::default(),
        None,
    );
    client.submit_order_list(cmd).expect("the list is served");

    // Dispatch is a channel hop to the websocket task, so the assertion drains
    // to a DEADLINE rather than reading the stub once: reading immediately would
    // race the flush, and reading `the next` message would be the same mistake
    // in the other direction.
    let deadline = Instant::now() + Duration::from_secs(2);
    let submits = loop {
        let seen: Vec<String> = state
            .ws_client_messages
            .lock()
            .expect("ws client messages mutex")
            .iter()
            .filter(|text| text.contains("SubmitOrder"))
            .cloned()
            .collect();
        if !seen.is_empty() || Instant::now() >= deadline {
            break seen;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        submits.len(),
        1,
        "the list is ONE frame, not one per leg: {submits:?}"
    );
    let group = &submits[0];
    assert!(
        group.contains(r#""type":"SubmitOrderGroup""#),
        "and it is the group frame: {group}"
    );
    assert!(
        group.contains(r#""contingency":"Oto""#) && group.contains(r#""order_list_id":"OL-1""#),
        "the entry carries its rule and its list: {group}"
    );
    assert!(
        group.contains(r#""parent_order_id":"O-1""#),
        "the exit names the entry as its parent: {group}"
    );
    // ORDER MATTERS and is the list's own: a child must follow the parent it
    // names, which is the order nautilus's `OrderList` already puts them in.
    assert!(
        group.find(r#""client_order_id":"O-1""#) < group.find(r#""client_order_id":"O-STOP""#),
        "the legs keep the list's order: {group}"
    );
}

/// The refusal that REMAINS on a trailing stop, now that the TYPE itself is
/// served: an offset stated in anything but PRICE. The venue trails by an
/// absolute price distance, and converting ticks or basis points needs a
/// reference price the two ends would have to agree on independently - the exact
/// silent disagreement that leaves a stop somewhere neither side intended.
///
/// Per the AE8 ordering the convert-first block exists for, it must fail before
/// any `OrderSubmitted` is emitted.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_trailing_offset_the_venue_cannot_read_is_refused_by_name() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_trailing_stop(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
    let err = client
        .submit_order(submit_command(&order, order.init_event().clone()))
        .expect_err("a PriceTier offset is unreadable here")
        .to_string();
    assert!(
        err.contains("trailing offset type"),
        "the refusal names the shape it refused: {err}"
    );
    assert!(
        err.contains("absolute price distance"),
        "the refusal names what the venue CAN read: {err}"
    );
    assert!(
        err.contains("state the offset in price"),
        "the refusal names what to do instead: {err}"
    );
    assert_no_exec_event(&mut sink_rx).await;
    assert_eq!(
        client.mirrored_order_status(order.client_order_id()),
        None,
        "a refused submit leaves no mirror record to leak"
    );
}

/// The order-init shapes the venue cannot honor, each refused BEFORE any
/// `OrderSubmitted`. Table-driven because the three are one rule - the venue
/// refuses what it cannot serve rather than serving something else quietly -
/// and because each is built by mutating a legal init, which is the only way
/// to produce shapes nautilus' own factory will not assemble.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn unsupported_init_shapes_are_refused_before_submitted() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_stop_market(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
    let legal = order.init_event().clone();

    let mark_price = {
        let mut init = legal.clone();
        init.trigger_type = Some(TriggerType::MarkPrice);
        init
    };
    let foreign_instrument = {
        let mut init = legal.clone();
        init.trigger_instrument_id =
            Some(InstrumentId::new(Symbol::from("ETHUSDT"), *MOGWAI_VENUE));
        init
    };
    // A linkage with no list to key it by. The four nautilus fields are
    // independent, so this shape assembles - and means nothing, since the venue
    // keys a linkage by `order_list_id`. Refused HERE rather than at the venue:
    // a venue-side refusal of something the host could have caught reads to a
    // strategy author as the venue being broken.
    let unkeyed_linkage = {
        let mut init = legal.clone();
        init.contingency_type = Some(ContingencyType::Oco);
        init.linked_order_ids = Some(vec![ClientOrderId::from("O-SIBLING")]);
        init
    };

    for (label, init, needles) in [
        (
            "a mark-price trigger",
            mark_price,
            ["unsupported", "trigger"],
        ),
        (
            "a foreign trigger instrument",
            foreign_instrument,
            ["unsupported", "tape"],
        ),
        (
            "an unkeyed linkage",
            unkeyed_linkage,
            ["must name its order list", "order_list_id"],
        ),
    ] {
        let err = client
            .submit_order(submit_command(&order, init))
            .expect_err(label)
            .to_string();
        for needle in needles {
            assert!(
                err.contains(needle),
                "{label}: the refusal names the ruling ({needle}): {err}"
            );
        }
        assert_no_exec_event(&mut sink_rx).await;
    }
    assert!(
        state
            .ws_client_messages
            .lock()
            .expect("ws client messages mutex")
            .iter()
            .all(|text| !text.contains("SubmitOrder")),
        "a refused init must not reach the venue at all"
    );
}

/// The mirror regression section 3.5 names: an amend ack recomputes status from
/// fill progress, and with no `Triggered` case a quantity or price amend on a
/// TRIGGERED stop-limit walks the mirror back to `Accepted` and desyncs it from
/// the engine for the rest of the run. Nautilus' own FSM keeps
/// `(Triggered, Updated) => Triggered`. The amend ack must also carry the new
/// trigger price, or the amend is unverifiable.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_trigger_amend_on_a_triggered_stop_limit_keeps_it_triggered() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    {
        let mut frames = state.ws_exec_frames.lock().expect("ws exec frames mutex");
        frames.push(
            r#"{"type":"OrderAccepted","client_order_id":"O-STOPLIMIT","venue_order_id":"V-7","ts_event":10}"#
                .to_string(),
        );
        frames.push(
            r#"{"type":"OrderTriggered","client_order_id":"O-STOPLIMIT","venue_order_id":"V-7","ts_event":11}"#
                .to_string(),
        );
    }
    state
        .ws_modify_frames
        .lock()
        .expect("ws modify frames mutex")
        .push(
            r#"{"type":"OrderUpdated","client_order_id":"O-STOPLIMIT","venue_order_id":"V-7","quantity":"1","price":"93.00","trigger_price":"96.00","leaves_qty":"1","ts_event":12}"#
                .to_string(),
        );
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let order = cached_stop_limit(&cache);
    let client = connected_exec_client(base_url, cache, &mut sink_rx).await;
    client
        .submit_order(submit_command(&order, order.init_event().clone()))
        .expect("a stop-limit is no longer refused at conversion");

    let timeout = Duration::from_secs(2);
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout, "the stop-limit's OrderSubmitted").await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout, "the stop-limit's OrderAccepted").await,
        ExecutionEvent::Order(OrderEventAny::Accepted(_))
    ));
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout, "the stop-limit's OrderTriggered").await,
        ExecutionEvent::Order(OrderEventAny::Triggered(_))
    ));
    assert_eq!(
        client.mirrored_order_status(order.client_order_id()),
        Some(OrderStatus::Triggered)
    );

    client
        .modify_order(ModifyOrder::new(
            TraderId::from("MOGWAI-001"),
            Some(ClientId::from("MOGWAI-EXEC")),
            StrategyId::from("S-001"),
            instrument_id(),
            order.client_order_id(),
            Some(VenueOrderId::from("V-7")),
            None,
            Some(Price::from("93.00")),
            Some(Price::from("96.00")),
            UUID4::new(),
            UnixNanos::default(),
            None,
            None,
        ))
        .expect("amend dispatches");

    match next_exec_event(
        &mut sink_rx,
        timeout,
        "the amend ack (OrderUpdated) for the triggered stop-limit",
    )
    .await
    {
        ExecutionEvent::Order(OrderEventAny::Updated(updated)) => {
            assert_eq!(
                updated.trigger_price,
                Some(Price::from("96.00")),
                "the ack forwards the venue's new trigger price into nautilus"
            );
            assert_eq!(updated.price, Some(Price::from("93.00")));
        }
        other => panic!("expected an OrderUpdated, got {other:?}"),
    }
    assert_eq!(
        client.mirrored_order_status(order.client_order_id()),
        Some(OrderStatus::Triggered),
        "the amend walked a triggered order back to Accepted"
    );

    let sent = state
        .ws_client_messages
        .lock()
        .expect("ws client messages mutex")
        .clone();
    let modify = sent
        .iter()
        .find(|text| text.contains("ModifyOrder"))
        .expect("the client sent a ModifyOrder");
    assert!(modify.contains(r#""trigger_price":"96.00""#), "{modify}");
}

/// Builds a started-but-unconnected execution client over `cache`, with no
/// socket anywhere: both regressions below are refusals that fire before any
/// transport is touched, which is exactly why they are cheap enough to run in
/// the default sweep rather than behind `#[ignore]`.
fn unconnected_exec_client(cache: Rc<RefCell<Cache>>) -> MogwaiExecutionClient {
    let config = MogwaiExecClientConfig {
        account_id: AccountId::from("MOGWAI-001"),
        base_url: "ws://127.0.0.1:1".into(),
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
    MogwaiExecutionClient::new(core, config).expect("client builds")
}

/// AE20. A CONNECTION WITH NO EVENT SINK IS DEAF, AND MUST REFUSE.
///
/// Nautilus's `ExecutionEventEmitter` is `Clone` and owns its sender BY VALUE,
/// so the WS pump's context freezes the sender state at connect time and
/// `send_order_event` on a sender-less emitter only logs. A client that
/// connected without one would report success and then drop every accept,
/// fill, cancel and reject the venue pushed, for the whole run, silently.
///
/// This test installs NO sender - `replace_exec_event_sender` is a
/// `thread_local!` and libtest gives each test its own thread (in EVERY lane,
/// `--test-threads=1` included - see `common`'s header), so the runner slot
/// this test's client reads is genuinely empty. That premise is asserted
/// rather than assumed: were it ever to break, this test would otherwise fail
/// on the error-text assertion below and read as a regression in the guard it
/// is pinning.
#[tokio::test(flavor = "current_thread")]
async fn connect_refuses_a_client_with_no_execution_event_sink() {
    assert_owns_a_fresh_exec_sink();
    let cache = Rc::new(RefCell::new(Cache::default()));
    let mut client = unconnected_exec_client(cache);
    // start() cannot find a sender on this thread and says so; it is connect()
    // that must refuse rather than proceed deaf.
    client.start().expect("start is not the gate");
    let error = client
        .connect()
        .await
        .expect_err("a deaf connection must be refused, not reported connected");
    assert!(
        error
            .to_string()
            .contains("execution event sender not initialized"),
        "{error}"
    );
}

/// A LIST THAT CANNOT RESOLVE EVERY LEG ANNOUNCES NO LEG.
///
/// `submit_order_list` announces each leg and then dispatches ONE group frame.
/// Announcing inside the fallible loop meant a leg the cache could not resolve
/// returned after the earlier legs had already been emitted as `OrderSubmitted`
/// and mirrored, and before any frame went out - half a bracket announced, none
/// dispatched, and no reject to end it. The resolve pass now runs to completion
/// before the first announcement.
#[tokio::test(flavor = "current_thread")]
async fn a_list_whose_second_leg_is_unresolvable_announces_neither() {
    assert_owns_a_fresh_exec_sink();
    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
    replace_exec_event_sender(sink_tx);

    let cache = Rc::new(RefCell::new(Cache::default()));
    let entry = cached_order(&cache);
    // Built against a THROWAWAY cache: a well-formed second leg that the
    // client's own cache has never heard of, which is what `get_order` fails on.
    let orphan_cache = Rc::new(RefCell::new(Cache::default()));
    let exit = cached_stop_market(&orphan_cache);

    let mut client = unconnected_exec_client(Rc::clone(&cache));
    // START IT. Without this the emitter holds no sender and every emitted
    // event is swallowed by nautilus's warn - which would leave
    // `assert_no_exec_event` below passing against the defect it exists to
    // catch, with only the mirror assertion doing any work.
    client.start().expect("start installs this thread's sink");

    let list_id = OrderListId::from("OL-1");
    let mut entry_init = entry.init_event().clone();
    entry_init.order_list_id = Some(list_id);
    entry_init.contingency_type = Some(ContingencyType::Oto);
    entry_init.linked_order_ids = Some(vec![exit.client_order_id()]);
    let mut exit_init = exit.init_event().clone();
    exit_init.order_list_id = Some(list_id);
    exit_init.parent_order_id = Some(entry.client_order_id());

    let list = nautilus_model::orders::OrderList::new(
        list_id,
        instrument_id(),
        StrategyId::from("S-001"),
        vec![entry.client_order_id(), exit.client_order_id()],
        UnixNanos::default(),
    );
    let cmd = nautilus_common::messages::execution::SubmitOrderList::new(
        TraderId::from("MOGWAI-001"),
        Some(ClientId::from("MOGWAI-EXEC")),
        StrategyId::from("S-001"),
        list,
        vec![entry_init, exit_init],
        None,
        None,
        None,
        UUID4::new(),
        UnixNanos::default(),
        None,
    );
    let error = client
        .submit_order_list(cmd)
        .expect_err("a leg the cache cannot resolve fails the list");

    assert_no_exec_event(&mut sink_rx).await;
    assert_eq!(
        client.mirrored_order_status(entry.client_order_id()),
        None,
        "and no leg was mirrored either: {error}"
    );
}

/// Asserts nothing reaches the execution sink inside a short window. A refusal
/// that emitted `OrderSubmitted` first would queue an event nautilus then has
/// to apply to an order it has already denied, which is the AE8 stray.
async fn assert_no_exec_event(rx: &mut tokio::sync::mpsc::UnboundedReceiver<ExecutionEvent>) {
    let stray = tokio::time::timeout(Duration::from_millis(250), rx.recv()).await;
    assert!(
        stray.is_err(),
        "a refused submit emitted an execution event: {stray:?}"
    );
}
