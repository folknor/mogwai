// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end guard for reconciliation reports. Nautilus' Rust trait defaults
//! silently degrade to empty reports or `None`; these socket-backed tests pin
//! every mogwai venue-truth surface and their mass-status composition.

mod common;

use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};

use common::{
    StubState, account_json, bound_stub, connected_exec_client, instrument_id, next_exec_event,
    position_json, venue_fill_row, venue_order_row, venue_stop_order_row,
};
use mogwai_protocol::WireOrderStatus;
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{
        ExecutionEvent,
        execution::{
            ExecutionReport, GenerateFillReports, GenerateOrderStatusReport,
            GenerateOrderStatusReports, GeneratePositionStatusReports, QueryOrder,
        },
    },
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_model::{
    enums::{LiquiditySide, OrderStatus, OrderType, PositionSideSpecified, TriggerType},
    identifiers::{
        ClientId, ClientOrderId, PositionId, StrategyId, TradeId, TraderId, VenueOrderId,
    },
    types::{Price, Quantity},
};
use rust_decimal::Decimal;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

struct Fixture {
    state: Arc<StubState>,
    client: mogwai_adapter::MogwaiExecutionClient,
    sink_rx: UnboundedReceiver<ExecutionEvent>,
}

async fn fixture() -> Fixture {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    *state.account_body.lock().expect("account body mutex") = Some(account_json(
        "MOGWAI-001",
        &format!("[{}]", position_json("1", "100.00")),
        12,
    ));
    state
        .venue_orders
        .lock()
        .expect("venue orders mutex")
        .push(venue_order_row(
            "O-1",
            "V-1",
            WireOrderStatus::PartiallyFilled,
            Decimal::ONE,
            11,
        ));
    state
        .venue_fills
        .lock()
        .expect("venue fills mutex")
        .push(venue_fill_row("O-1", "V-1", "T-1", Decimal::ONE, 11));
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel();
    replace_exec_event_sender(sink_tx);
    let client = connected_exec_client(
        base_url,
        Rc::new(RefCell::new(Cache::default())),
        &mut sink_rx,
    )
    .await;
    Fixture {
        state,
        client,
        sink_rx,
    }
}

fn order_reports_cmd() -> GenerateOrderStatusReports {
    GenerateOrderStatusReports::new(
        UUID4::new(),
        UnixNanos::from(20),
        true,
        None,
        None,
        None,
        None,
        None,
    )
}

fn fill_reports_cmd() -> GenerateFillReports {
    GenerateFillReports::new(
        UUID4::new(),
        UnixNanos::from(20),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

fn position_reports_cmd() -> GeneratePositionStatusReports {
    GeneratePositionStatusReports::new(
        UUID4::new(),
        UnixNanos::from(20),
        None,
        None,
        None,
        None,
        None,
    )
}

fn assert_mass_status(mass: &nautilus_model::reports::ExecutionMassStatus) {
    assert_eq!(mass.client_id, ClientId::from("MOGWAI-EXEC"));
    assert_eq!(mass.account_id.to_string(), "MOGWAI-001");
    let orders = mass.order_reports();
    let order = orders.get(&VenueOrderId::from("V-1")).expect(
        "empty order reports: venue truth degraded and startup reconciliation would adopt nothing",
    );
    assert_eq!(order.order_status, OrderStatus::PartiallyFilled);
    assert_eq!(order.client_order_id, Some(ClientOrderId::from("O-1")));
    let fills = mass.fill_reports();
    let fill = fills.get(&VenueOrderId::from("V-1")).and_then(|fills| fills.first())
        .expect("empty fill reports: venue truth degraded and startup reconciliation would adopt nothing");
    assert_eq!(fill.trade_id, TradeId::from("T-1"));
    assert_eq!(fill.last_qty, Quantity::from("1"));
    assert_eq!(fill.last_px, Price::from("100.00"));
    let positions = mass.position_reports();
    let position = positions.get(&instrument_id()).and_then(|reports| reports.first())
        .expect("empty position reports: the account route degraded and startup reconciliation would adopt nothing");
    assert_eq!(position.position_side, PositionSideSpecified::Long);
    assert_eq!(position.quantity, Quantity::from("1"));
    assert_eq!(position.instrument_id, instrument_id());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn mass_status_pairs_an_open_orders_fill_outside_the_lookback() {
    let fixture = fixture().await;
    // An older fill on the SAME open order, ahead of the fixture's own in the
    // venue's ordering. A zero-minute lookback puts both outside the window.
    fixture
        .state
        .venue_fills
        .lock()
        .expect("venue fills mutex")
        .insert(0, venue_fill_row("O-1", "V-1", "T-0", Decimal::ONE, 5));
    let mass = fixture
        .client
        .generate_mass_status(Some(0))
        .await
        .expect("mass status")
        .expect("mass status is served");
    assert!(
        mass.order_reports()
            .contains_key(&VenueOrderId::from("V-1")),
        "the open order must be reported"
    );
    let all_fills = mass.fill_reports();
    let fills = all_fills
        .get(&VenueOrderId::from("V-1"))
        .expect("an open order's fills are reported");
    // Both halves of the finding: the fills survive the lookback at all, and
    // they arrive in the venue's own order, so pairing them yields the venue's
    // average price rather than a reversed one.
    assert_eq!(
        fills
            .iter()
            .map(|report| report.trade_id.to_string())
            .collect::<Vec<_>>(),
        vec!["T-0".to_string(), "T-1".to_string()]
    );
    assert!(
        fills
            .iter()
            .all(|report| report.last_px.as_decimal() == Decimal::from(100)),
        "fill prices must reach the host; the order report carries no avg_px"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn connecting_twice_replaces_the_execution_socket() {
    let mut fixture = fixture().await;
    fixture.client.connect().await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while fixture.state.active_ws.load(Ordering::Relaxed) != 1 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "old execution socket remained active"
        );
        tokio::task::yield_now().await;
    }
    assert_eq!(fixture.state.ws_hits.load(Ordering::Relaxed), 2);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn reconnect_after_stop_can_be_stopped_again() {
    let mut fixture = fixture().await;
    fixture.client.stop().unwrap();
    fixture.client.connect().await.unwrap();
    fixture.client.stop().unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while fixture.state.active_ws.load(Ordering::Relaxed) != 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "stop after reconnect left the execution socket active"
        );
        tokio::task::yield_now().await;
    }
}

/// The `..._over_ws` twin of this test was deleted: identical fixture,
/// identical assertions, one strictly weaker - residue of the deleted
/// `TransportProfile` parameterization, back when the two names meant two
/// carriers. This is the stronger of the pair, so it is the one that survived.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn mass_status_reports_all_three_sets_over_the_single_ws_transport() {
    let fixture = fixture().await;
    let mass = fixture
        .client
        .generate_mass_status(None)
        .await
        .expect("mass status generates")
        .expect("mass status is Some, not the trait default");
    assert_mass_status(&mass);
    assert!(fixture.state.order_queries.load(Ordering::Relaxed) >= 1);
    assert!(fixture.state.fill_queries.load(Ordering::Relaxed) >= 1);
    assert!(fixture.state.ws_hits.load(Ordering::Relaxed) >= 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn order_status_reports_are_non_empty_end_to_end() {
    let fixture = fixture().await;
    let reports = fixture
        .client
        .generate_order_status_reports(&order_reports_cmd())
        .await
        .expect("order reports");
    assert_eq!(
        reports.len(),
        1,
        "empty order reports silently disable reconciliation"
    );
    assert_eq!(reports[0].client_order_id, Some(ClientOrderId::from("O-1")));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn fill_reports_are_non_empty_end_to_end() {
    let fixture = fixture().await;
    let reports = fixture
        .client
        .generate_fill_reports(fill_reports_cmd())
        .await
        .expect("fill reports");
    assert_eq!(
        reports.len(),
        1,
        "empty fill reports silently disable reconciliation"
    );
    assert_eq!(reports[0].venue_order_id, VenueOrderId::from("V-1"));
    assert_eq!(reports[0].trade_id, TradeId::from("T-1"));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_maker_fill_reports_maker_liquidity_side() {
    let fixture = fixture().await;
    fixture.state.venue_fills.lock().unwrap()[0].liquidity_side =
        mogwai_protocol::LiquiditySide::Maker;
    let reports = fixture
        .client
        .generate_fill_reports(fill_reports_cmd())
        .await
        .unwrap();
    assert_eq!(reports[0].liquidity_side, LiquiditySide::Maker);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_venue_assigned_position_id_reaches_the_nautilus_fill_report() {
    let fixture = fixture().await;
    fixture.state.venue_fills.lock().unwrap()[0].position_id = Some("MNQ-7".into());
    let reports = fixture
        .client
        .generate_fill_reports(fill_reports_cmd())
        .await
        .unwrap();
    assert_eq!(
        reports[0].venue_position_id,
        Some(PositionId::from("MNQ-7"))
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_venue_assigned_position_id_reaches_the_order_status_report() {
    let fixture = fixture().await;
    fixture.state.venue_orders.lock().unwrap()[0].position_id = Some("BTCUSDT-7".into());
    let reports = fixture
        .client
        .generate_order_status_reports(&order_reports_cmd())
        .await
        .unwrap();
    assert_eq!(
        reports[0].venue_position_id,
        Some(PositionId::from("BTCUSDT-7"))
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_futures_commission_books_in_the_settlement_currency() {
    let fixture = fixture().await;
    {
        let mut fills = fixture.state.venue_fills.lock().unwrap();
        fills[0].commission = Decimal::new(125, 2);
        fills[0].commission_currency = "USD".into();
    }
    let reports = fixture
        .client
        .generate_fill_reports(fill_reports_cmd())
        .await
        .unwrap();
    assert_eq!(reports[0].commission.currency.code.as_str(), "USD");
    assert_eq!(reports[0].commission.as_decimal(), Decimal::new(125, 2));
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn position_status_reports_are_non_empty_end_to_end() {
    let fixture = fixture().await;
    let reports = fixture
        .client
        .generate_position_status_reports(&position_reports_cmd())
        .await
        .expect("position reports");
    assert_eq!(
        reports.len(),
        1,
        "empty reports would mask the legacy-account-route fallback"
    );
    assert_eq!(reports[0].position_side, PositionSideSpecified::Long);
    assert_eq!(reports[0].instrument_id, instrument_id());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn singular_order_status_report_resolves_a_targeted_order() {
    let fixture = fixture().await;
    let command = |id| {
        GenerateOrderStatusReport::new(
            UUID4::new(),
            UnixNanos::from(20),
            None,
            Some(ClientOrderId::from(id)),
            None,
            None,
            None,
        )
    };
    assert_eq!(
        fixture
            .client
            .generate_order_status_report(&command("O-1"))
            .await
            .expect("report")
            .expect("O-1 exists")
            .venue_order_id,
        VenueOrderId::from("V-1")
    );
    assert!(
        fixture
            .client
            .generate_order_status_report(&command("O-404"))
            .await
            .expect("unknown query")
            .is_none()
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn query_order_emits_an_order_status_report() {
    let mut fixture = fixture().await;
    fixture
        .client
        .query_order(QueryOrder::new(
            TraderId::from("MOGWAI-001"),
            Some(ClientId::from("MOGWAI-EXEC")),
            StrategyId::from("S-001"),
            instrument_id(),
            ClientOrderId::from("O-1"),
            None,
            UUID4::new(),
            UnixNanos::from(20),
            None,
            None,
        ))
        .expect("query order");
    match next_exec_event(&mut fixture.sink_rx, Duration::from_secs(2)).await {
        ExecutionEvent::Report(ExecutionReport::Order(report)) => {
            assert_eq!(report.client_order_id, Some(ClientOrderId::from("O-1")));
            assert_eq!(report.venue_order_id, VenueOrderId::from("V-1"));
        }
        other => panic!("expected an order status report, got {other:?}"),
    }
}

/// Reconciliation of a CONDITIONAL, over both order-status carriers.
///
/// The existing guard above proves venue truth reaches nautilus at all. It
/// cannot see the hole the conditional surface opened: `OrderStatusReport`'s
/// constructor leaves price, trigger price, trigger type and `ts_triggered` at
/// `None`, so a report built without setting them describes a stop the venue is
/// resting as an order with no limit and nothing to wait for. Startup
/// reconciliation would then adopt a protective leg it cannot match against the
/// strategy's own, silently. Both an UNTRIGGERED and a TRIGGERED row are pinned
/// because they differ in exactly the two fields (`order_status`,
/// `ts_triggered`) the new status ladder introduced.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_stop_report_carries_its_trigger_price_and_status() {
    let state = Arc::new(StubState::default());
    state.serve_account.store(true, Ordering::Relaxed);
    {
        let mut rows = state.venue_orders.lock().expect("venue orders mutex");
        rows.push(venue_stop_order_row(
            "O-STOP",
            "V-9",
            mogwai_protocol::OrderType::StopMarket,
            WireOrderStatus::Accepted,
            None,
            Decimal::from(95),
            None,
            11,
        ));
        rows.push(venue_stop_order_row(
            "O-STOPLIMIT",
            "V-7",
            mogwai_protocol::OrderType::StopLimit,
            WireOrderStatus::Triggered,
            Some(Decimal::from(94)),
            Decimal::from(95),
            Some(12),
            12,
        ));
    }
    let base_url = bound_stub(Arc::clone(&state)).await;
    let (sink_tx, mut sink_rx) = unbounded_channel();
    replace_exec_event_sender(sink_tx);
    let client = connected_exec_client(
        base_url,
        Rc::new(RefCell::new(Cache::default())),
        &mut sink_rx,
    )
    .await;

    // Carrier one: the bulk sweep startup reconciliation runs. `open_only` is
    // true, which is also what proves `WireOrderStatus::Triggered.is_open()` -
    // a triggered stop-limit that fell out here would vanish from open-order
    // reconciliation for the whole window between its trigger and its fill.
    let reports = client
        .generate_order_status_reports(&order_reports_cmd())
        .await
        .expect("order reports");
    assert_eq!(
        reports.len(),
        2,
        "a triggered conditional must survive the open-order partition"
    );

    let untriggered = reports
        .iter()
        .find(|report| report.client_order_id == Some(ClientOrderId::from("O-STOP")))
        .expect("the untriggered stop-market is reported");
    assert_eq!(untriggered.order_status, OrderStatus::Accepted);
    assert_eq!(untriggered.order_type, OrderType::StopMarket);
    assert_eq!(untriggered.trigger_price, Some(Price::from("95.00")));
    assert_eq!(
        untriggered.trigger_type,
        Some(TriggerType::LastPrice),
        "the venue has one trigger reference and the report must say which"
    );
    assert_eq!(
        untriggered.price, None,
        "a stop-market carries no price the venue ever consults"
    );
    assert_eq!(untriggered.ts_triggered, None);
    assert!(untriggered.reduce_only);

    let triggered = reports
        .iter()
        .find(|report| report.client_order_id == Some(ClientOrderId::from("O-STOPLIMIT")))
        .expect("the triggered stop-limit is reported");
    assert_eq!(triggered.order_status, OrderStatus::Triggered);
    assert_eq!(triggered.order_type, OrderType::StopLimit);
    assert_eq!(
        triggered.price,
        Some(Price::from("94.00")),
        "a stop-limit report without its limit price is unreconcilable"
    );
    assert_eq!(triggered.trigger_price, Some(Price::from("95.00")));
    assert_eq!(triggered.ts_triggered, Some(UnixNanos::from(12)));

    // Carrier two: the singular, targeted query the execution manager's
    // in-flight probe uses. It builds its report through the same converter, so
    // a divergence between the two would be a converter that lost a field on
    // one path only.
    let targeted = client
        .generate_order_status_report(&GenerateOrderStatusReport::new(
            UUID4::new(),
            UnixNanos::from(20),
            None,
            Some(ClientOrderId::from("O-STOPLIMIT")),
            None,
            None,
            None,
        ))
        .await
        .expect("targeted report")
        .expect("the venue knows the stop-limit");
    assert_eq!(targeted.order_status, OrderStatus::Triggered);
    assert_eq!(targeted.trigger_price, Some(Price::from("95.00")));
    assert_eq!(targeted.price, Some(Price::from("94.00")));
    assert_eq!(targeted.ts_triggered, Some(UnixNanos::from(12)));
    assert_eq!(targeted.trigger_type, Some(TriggerType::LastPrice));
}
