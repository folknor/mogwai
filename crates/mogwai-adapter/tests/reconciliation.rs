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
    position_json, venue_fill_row, venue_order_row,
};
use mogwai_protocol::{TransportProfile, WireOrderStatus};
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
    enums::{OrderStatus, PositionSideSpecified},
    identifiers::{ClientId, ClientOrderId, StrategyId, TradeId, TraderId, VenueOrderId},
    types::{Price, Quantity},
};
use rust_decimal::Decimal;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

struct Fixture {
    state: Arc<StubState>,
    client: mogwai_adapter::MogwaiExecutionClient,
    sink_rx: UnboundedReceiver<ExecutionEvent>,
}

async fn fixture(transport_profile: TransportProfile) -> Fixture {
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
        transport_profile,
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
async fn mass_status_reports_all_three_sets_over_ws() {
    let fixture = fixture(TransportProfile::WsStreaming).await;
    let mass = fixture
        .client
        .generate_mass_status(None)
        .await
        .expect("mass status generates")
        .expect("mass status is Some, not the trait default");
    assert_mass_status(&mass);
    assert!(fixture.state.order_queries.load(Ordering::Relaxed) >= 1);
    assert!(fixture.state.fill_queries.load(Ordering::Relaxed) >= 1);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn mass_status_reports_all_three_sets_over_http() {
    let fixture = fixture(TransportProfile::HttpOrders).await;
    let mass = fixture
        .client
        .generate_mass_status(None)
        .await
        .expect("mass status generates")
        .expect("mass status is Some, not the trait default");
    assert_mass_status(&mass);
    assert!(fixture.state.order_queries.load(Ordering::Relaxed) >= 1);
    assert!(fixture.state.fill_queries.load(Ordering::Relaxed) >= 1);
    assert_eq!(fixture.state.ws_hits.load(Ordering::Relaxed), 0);
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn order_status_reports_are_non_empty_end_to_end() {
    let fixture = fixture(TransportProfile::WsStreaming).await;
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
    let fixture = fixture(TransportProfile::WsStreaming).await;
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
async fn position_status_reports_are_non_empty_end_to_end() {
    let fixture = fixture(TransportProfile::WsStreaming).await;
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
    let fixture = fixture(TransportProfile::WsStreaming).await;
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
    let mut fixture = fixture(TransportProfile::WsStreaming).await;
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
