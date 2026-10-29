// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! End-to-end guard for reconciliation reports. Nautilus' Rust trait defaults
//! degrade the report generators to empty reports or `None` with only a
//! "not implemented" log line, and the default mass status composes those
//! empties into an empty whole. The policy hooks default to the conservative
//! answer instead (an overfill stays rejected under
//! `allows_reconciliation_overfill`). These socket-backed tests pin every
//! mogwai venue-truth surface and their mass-status composition.

use crate::common;

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
    enums::{LiquiditySide, OrderStatus, OrderType, PositionSide, TriggerType},
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
    assert_eq!(position.position_side, PositionSide::Long);
    assert_eq!(position.quantity, Quantity::from("1"));
    assert_eq!(position.instrument_id, instrument_id());
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn mass_status_pairs_an_open_orders_fill_outside_the_lookback() {
    let fixture = fixture().await;
    // An older fill on the same open order, ahead of the fixture's own in the
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
    common::connect_with_deadline(fixture.client.connect())
        .await
        .unwrap();

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
    common::connect_with_deadline(fixture.client.connect())
        .await
        .unwrap();
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
    // Both sets came off the wire rather than out of a client-side cache: a
    // mass status assembled from local state would satisfy `assert_mass_status`
    // and leave these at zero.
    assert!(fixture.state.order_queries.load(Ordering::Relaxed) >= 1);
    assert!(fixture.state.fill_queries.load(Ordering::Relaxed) >= 1);
    // `ws_hits >= 1` used to sit here and could not fail. `fixture()` connects
    // the exec client and `expect`s it, and a connect that returns has by
    // definition completed a `/ws` upgrade the stub counted - so the value is
    // at least one before this test's first line runs, whatever the mass-status
    // path does. It is deleted rather than repaired: the two counters above are
    // the wire evidence, and each of them is written by the transport this one
    // was standing in for.
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
    assert_eq!(reports[0].position_side, PositionSide::Long);
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
    match next_exec_event(
        &mut fixture.sink_rx,
        Duration::from_secs(2),
        "the order status report query_order(O-1) emits",
    )
    .await
    {
        ExecutionEvent::Report(ExecutionReport::Order(report)) => {
            assert_eq!(report.client_order_id, Some(ClientOrderId::from("O-1")));
            assert_eq!(report.venue_order_id, VenueOrderId::from("V-1"));
        }
        other => panic!("expected an order status report, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn targeted_order_queries_refuse_a_wrong_venue_row() {
    let fixture = fixture().await;
    fixture
        .state
        .ignore_order_query_filter
        .store(true, Ordering::Relaxed);

    let singular = fixture
        .client
        .generate_order_status_report(&GenerateOrderStatusReport::new(
            UUID4::new(),
            UnixNanos::from(20),
            None,
            Some(ClientOrderId::from("O-404")),
            None,
            None,
            None,
        ))
        .await
        .expect("query completes");
    assert!(
        singular.is_none(),
        "a row for O-1 must not answer the singular query for O-404"
    );
}

/// Reconciliation of a conditional order, over both order-status carriers.
///
/// The existing guard above proves venue truth reaches nautilus at all. It
/// cannot see the hole the conditional surface opened: `OrderStatusReport`'s
/// constructor leaves price, trigger price, trigger type and `ts_triggered` at
/// `None`, so a report built without setting them describes a stop the venue is
/// resting as an order with no limit and nothing to wait for. Startup
/// reconciliation would then adopt a protective leg it cannot match against the
/// strategy's own, silently. Both an untriggered and a triggered row are pinned
/// because they differ in exactly the two fields (`order_status`,
/// `ts_triggered`) the new status ladder introduced.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn a_stop_report_carries_its_trigger_price_and_status() {
    let state = Arc::new(StubState::default());
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
    // a mismatch between the two would be a converter that lost a field on
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

/// A `GET /account` body for a margin ledger holding one BTCUSDT position and
/// posting one BTCUSDT margin row of initial 100 and maintenance 200. `locked`
/// is set above that row's sum on purpose: the venue's locked is wider than its
/// margin rows, and the extra is what a nautilus recalculation off margins
/// alone would also lose.
fn margin_account_json(ts_event: u64, locked: &str, free: &str) -> String {
    let position = common::position_json("1", "100.00");
    let body = format!(
        r#"{{"clock":"venue","account":{{"account_id":"MOGWAI-001","balances":[{{"currency":"USDT","total":"10000","free":"{free}","locked":"{locked}"}}],"positions":[{position}],"margins":[{{"symbol":"BTCUSDT","currency":"USDT","initial":"100","maintenance":"200"}}],"risk":{{"equity":"10000","peak_equity":"10000","day_open_equity":"10000"}},"ts_event":{ts_event}}},"sweep_passes":[]}}"#
    );
    if let Err(err) = serde_json::from_str::<mogwai_protocol::http::AccountSnapshot>(&body) {
        panic!("the margin account body is not an AccountSnapshot: {err}: {body}");
    }
    body
}

/// The BTCUSDT seed as a USDT-settled perpetual, the class a margin row is
/// posted for.
fn perpetual_instruments_json() -> String {
    let def = mogwai_protocol::InstrumentDef {
        symbol: "BTCUSDT".into(),
        class: mogwai_protocol::InstrumentClass::Perpetual {
            underlying: "BTC".into(),
            settlement_currency: "USDT".into(),
            multiplier: Decimal::ONE,
            asset_class: mogwai_protocol::WireAssetClass::Cryptocurrency,
            funding_interval_ns: 8 * 3_600 * 1_000_000_000,
            funding_rate: Decimal::ZERO,
            index_symbol: None,
            funding_clamp: Decimal::ZERO,
        },
        price_precision: 2,
        size_precision: 0,
        price_increment: Decimal::new(1, 2),
        size_increment: Decimal::ONE,
    };
    serde_json::to_string(&[def]).expect("the instrument seed serializes")
}

/// A BTCUSDT margin row as `(initial, maintenance)`.
type MarginRow = Option<(Decimal, Decimal)>;

/// A USDT balance as `(total, locked, free)`.
type BalanceTriple = (Decimal, Decimal, Decimal);

/// The margin account this test's cache holds, with its BTCUSDT margin row and
/// its USDT balance.
fn margin_view(
    cache: &Rc<RefCell<Cache>>,
) -> (
    nautilus_model::accounts::AccountAny,
    MarginRow,
    BalanceTriple,
) {
    let account = cache
        .borrow()
        .account(&nautilus_model::identifiers::AccountId::from("MOGWAI-001"))
        .expect("the account is cached")
        .cloned();
    let nautilus_model::accounts::AccountAny::Margin(margin) = &account else {
        panic!("the host was configured for a margin account, got {account:?}");
    };
    let row = margin
        .margin(&instrument_id())
        .map(|row| (row.initial.as_decimal(), row.maintenance.as_decimal()));
    let balance = account
        .balance(Some(nautilus_model::types::Currency::USDT()))
        .expect("the USDT balance row is cached");
    let triple = (
        balance.total.as_decimal(),
        balance.locked.as_decimal(),
        balance.free.as_decimal(),
    );
    (account, row, triple)
}

/// The startup clobber, and the adapter's repair of it, against the real
/// published nautilus portfolio.
///
/// After reconciling a mass status, a live node runs
/// `Portfolio::initialize_orders` and `initialize_positions`. On a margin
/// account those recompute margin from the instrument's rates, which `convert`
/// leaves at zero, and clear what the venue reported. The first half of this
/// test pins that upstream behaviour at the pinned release, step by step, so a
/// nautilus that stops doing it fails here and the repair can be reconsidered.
/// The second half is the repair: `generate_mass_status` forwards a fresh venue
/// snapshot, which the node applies after the initializers.
///
/// What this cannot show is the node's ordering - that nothing drains the exec
/// channel between mass status and the initializers - nor that admission and
/// enqueue stay ordered against a concurrent push. Both are argued from source
/// at `generate_mass_status` and `handle_account_state`.
#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn mass_status_repairs_the_margin_the_startup_initializers_clear() {
    use nautilus_model::{accounts::Account, instruments::Instrument};

    let state = Arc::new(StubState::default());
    *state
        .instruments_body
        .lock()
        .expect("instruments body mutex") = Some(perpetual_instruments_json());
    *state.account_body.lock().expect("account body mutex") =
        Some(margin_account_json(12, "350", "9650"));
    let base_url = bound_stub(Arc::clone(&state)).await;

    // The instrument as the host caches it, off the adapter's own conversion:
    // the data client publishes its seeded definitions on connect.
    let (data_tx, mut data_rx) = unbounded_channel();
    nautilus_common::live::runner::replace_data_event_sender(data_tx);
    let mut data_client = mogwai_adapter::MogwaiDataClient::new(
        ClientId::from("MOGWAI-DATA"),
        mogwai_adapter::MogwaiDataClientConfig {
            account_id: nautilus_model::identifiers::AccountId::from("MOGWAI-001"),
            base_url: base_url.clone(),
            ..mogwai_adapter::MogwaiDataClientConfig::default()
        },
    )
    .expect("data client builds");
    nautilus_common::clients::DataClient::start(&mut data_client).expect("data client starts");
    common::connect_with_deadline(nautilus_common::clients::DataClient::connect(
        &mut data_client,
    ))
    .await
    .expect("data client connects");
    let instrument = match common::next_data_event(&mut data_rx, Duration::from_secs(2)).await {
        nautilus_common::messages::DataEvent::Data(nautilus_model::data::Data::Instrument(
            instrument,
        )) => *instrument,
        other => panic!("expected the seeded instrument first, got {other:?}"),
    };
    assert_eq!(instrument.id(), instrument_id());
    assert!(
        instrument.margin_init().is_zero() && instrument.margin_maint().is_zero(),
        "the premise is that convert publishes no margin rate; with one, the initializers \
         compute a requirement instead of clearing, and this test no longer describes them"
    );

    let cache = Rc::new(RefCell::new(Cache::default()));
    cache
        .borrow_mut()
        .add_instrument(instrument.clone())
        .expect("cache instrument");
    let (sink_tx, mut sink_rx) = unbounded_channel();
    replace_exec_event_sender(sink_tx);
    let config = mogwai_adapter::MogwaiExecClientConfig {
        account_id: nautilus_model::identifiers::AccountId::from("MOGWAI-001"),
        account_type: nautilus_model::enums::AccountType::Margin,
        base_url,
        ..mogwai_adapter::MogwaiExecClientConfig::default()
    };
    let core = nautilus_live::ExecutionClientCore::new(
        TraderId::from(mogwai_adapter::DEFAULT_TRADER_ID),
        ClientId::from("MOGWAI-EXEC"),
        *mogwai_adapter::MOGWAI_VENUE,
        nautilus_model::enums::OmsType::Netting,
        config.account_id,
        config.account_type,
        None,
        Rc::clone(&cache),
    );
    let mut client =
        mogwai_adapter::MogwaiExecutionClient::new(core, config).expect("exec client builds");
    client.start().expect("exec client starts");
    common::connect_seeding_account(&mut client, &cache, &mut sink_rx).await;

    let (account, row, triple) = margin_view(&cache);
    assert!(
        !account.calculated_account_state(),
        "a venue-reported account is the live shape; a calculated one takes the recompute \
         path everywhere and is not what this test is about"
    );
    assert_eq!(row, Some((Decimal::from(100), Decimal::from(200))));
    assert_eq!(
        triple,
        (
            Decimal::from(10_000),
            Decimal::from(350),
            Decimal::from(9_650)
        )
    );

    // What reconciliation leaves in the cache on a restart against this venue:
    // the resting order and the open position the venue holds.
    let trader_id = TraderId::from(mogwai_adapter::DEFAULT_TRADER_ID);
    let strategy_id = StrategyId::from("S-001");
    let account_id = nautilus_model::identifiers::AccountId::from("MOGWAI-001");
    let mut factory = nautilus_common::factories::OrderFactory::new(
        trader_id,
        strategy_id,
        None,
        None,
        Rc::new(RefCell::new(nautilus_common::clock::VirtualClock::new())),
        false,
        false,
    );
    let order = factory.limit(
        instrument_id(),
        nautilus_model::enums::OrderSide::Buy,
        Quantity::from("1"),
        Price::from("99.00"),
        Some(nautilus_model::enums::TimeInForce::Gtc),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(ClientOrderId::from("O-REST")),
    );
    cache
        .borrow_mut()
        .add_order(order, None, Some(ClientId::from("MOGWAI-EXEC")), false)
        .expect("cache order");
    cache
        .borrow_mut()
        .update_order(&nautilus_model::events::OrderEventAny::Submitted(
            nautilus_model::events::OrderSubmitted::new(
                trader_id,
                strategy_id,
                instrument_id(),
                ClientOrderId::from("O-REST"),
                account_id,
                UUID4::new(),
                UnixNanos::from(5),
                UnixNanos::from(5),
            ),
        ))
        .expect("order submitted");
    cache
        .borrow_mut()
        .update_order(&nautilus_model::events::OrderEventAny::Accepted(
            nautilus_model::events::OrderAccepted::new(
                trader_id,
                strategy_id,
                instrument_id(),
                ClientOrderId::from("O-REST"),
                VenueOrderId::from("V-REST"),
                account_id,
                UUID4::new(),
                UnixNanos::from(6),
                UnixNanos::from(6),
                true,
            ),
        ))
        .expect("order accepted");
    let fill = nautilus_model::events::OrderFilled::new(
        trader_id,
        strategy_id,
        instrument_id(),
        ClientOrderId::from("O-OPEN"),
        VenueOrderId::from("V-OPEN"),
        account_id,
        TradeId::from("T-OPEN"),
        nautilus_model::enums::OrderSide::Buy,
        OrderType::Market,
        Quantity::from("1"),
        Price::from("100.00"),
        nautilus_model::types::Currency::USDT(),
        LiquiditySide::Taker,
        UUID4::new(),
        UnixNanos::from(4),
        UnixNanos::from(4),
        true,
        Some(PositionId::from("P-OPEN")),
        None,
        None,
    );
    let position = nautilus_model::position::Position::new(&instrument, fill);
    cache
        .borrow_mut()
        .add_position(&position, nautilus_model::enums::OmsType::Netting)
        .expect("cache position");
    assert_eq!(
        cache
            .borrow()
            .orders_open(None, Some(&instrument_id()), None, None, None)
            .len(),
        1,
        "initialize_orders enumerates open orders only, so the resting order must be one"
    );
    assert_eq!(
        cache
            .borrow()
            .positions_open(None, Some(&instrument_id()), None, None, None)
            .len(),
        1
    );

    // The node's startup sequence, from the portfolio's side.
    let mut portfolio = nautilus_portfolio::portfolio::Portfolio::new(
        Rc::new(RefCell::new(nautilus_common::clock::VirtualClock::new())),
        Rc::clone(&cache),
        None,
    );
    portfolio.initialize_orders();
    let (_, row, triple) = margin_view(&cache);
    assert_eq!(
        row,
        Some((Decimal::ZERO, Decimal::from(200))),
        "initialize_orders clears the venue's initial margin: a zero rate computes zero"
    );
    assert_eq!(
        triple,
        (
            Decimal::from(10_000),
            Decimal::from(200),
            Decimal::from(9_800)
        ),
        "and rewrites locked from the margins that remain, dropping the venue's 150 of \
         unmargined hold along with the 100 initial"
    );
    portfolio.initialize_positions();
    let (_, row, triple) = margin_view(&cache);
    assert_eq!(
        row, None,
        "initialize_positions clears the maintenance margin too, and the empty row goes"
    );
    assert_eq!(
        triple,
        (Decimal::from(10_000), Decimal::ZERO, Decimal::from(10_000)),
        "leaving nothing locked"
    );

    // The repair. The venue's state has moved on since connect, so a replay of
    // the connect snapshot cannot pass for the fresh pull.
    *state.account_body.lock().expect("account body mutex") =
        Some(margin_account_json(30, "360", "9640"));
    assert!(
        sink_rx.try_recv().is_err(),
        "nothing may be queued before mass status, or the repair below is not its doing"
    );
    client
        .generate_mass_status(None)
        .await
        .expect("mass status generates")
        .expect("mass status is served");
    let event = sink_rx.try_recv().expect(
        "generate_mass_status forwards a venue account snapshot before returning, so it is \
         already queued; without it the initializers' clobber stands until the next push",
    );
    let ExecutionEvent::Account(account_state) = event else {
        panic!("expected the re-seeded AccountState, got {event:?}");
    };
    assert_eq!(account_state.ts_event, UnixNanos::from(30));
    portfolio.update_account(&account_state);
    let (_, row, triple) = margin_view(&cache);
    assert_eq!(row, Some((Decimal::from(100), Decimal::from(200))));
    assert_eq!(
        triple,
        (
            Decimal::from(10_000),
            Decimal::from(360),
            Decimal::from(9_640)
        )
    );
}
