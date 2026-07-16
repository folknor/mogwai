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
    time::Duration,
};

use common::{StubState, bound_stub, cached_order, instrument_id, next_exec_event};
use mogwai_adapter::{MOGWAI_VENUE, MogwaiExecClientConfig, MogwaiExecutionClient};
use mogwai_protocol::TransportProfile;
use nautilus_common::{
    cache::Cache,
    clients::ExecutionClient,
    live::runner::replace_exec_event_sender,
    messages::{ExecutionEvent, execution::SubmitOrder},
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_live::ExecutionClientCore;
use nautilus_model::{
    enums::OmsType,
    events::OrderEventAny,
    identifiers::{ClientId, StrategyId, TraderId, VenueOrderId},
    orders::Order,
    types::{Price, Quantity},
};
use tokio::sync::mpsc::unbounded_channel;

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn connect_seeds_initial_account_state() {
    for transport_profile in [TransportProfile::WsStreaming, TransportProfile::HttpOrders] {
        let state = Arc::new(StubState::default());
        state.serve_account.store(true, Ordering::Relaxed);
        let base_url = bound_stub(Arc::clone(&state)).await;

        let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
        replace_exec_event_sender(sink_tx);

        let cache = Rc::new(RefCell::new(Cache::default()));
        let config = MogwaiExecClientConfig {
            base_url,
            transport_profile,
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
            Rc::clone(&cache),
        );
        let mut client = MogwaiExecutionClient::new(core, config).expect("client builds");

        client.start().expect("start grabs sink");

        let account_id = client.account_id();
        let drain_account = async {
            match next_exec_event(&mut sink_rx, Duration::from_secs(2)).await {
                ExecutionEvent::Account(account) => {
                    assert_eq!(account.account_id, account_id);
                    cache
                        .borrow_mut()
                        .add_account(account.into())
                        .expect("cache account");
                }
                other => panic!("expected initial AccountState, got {other:?}"),
            }
        };
        let (connect, ()) = tokio::join!(client.connect(), drain_account);
        connect.expect("connect seeds account");
        assert!(
            client.get_account().is_some(),
            "account registered for {transport_profile:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "binds a real TCP listener; run in a socket-capable environment"]
async fn adapter_submit_drives_live_exec_events() {
    // The exec WS leg replies to the client's `SubmitOrder` with accept, fill
    // and account frames. The fill names order `O-1`, venue id `V-1`, qty `1`,
    // price `100.00`; the account snapshot carries a `9900` USDT balance.
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
        frames.push(
            r#"{"type":"AccountState","balances":[{"currency":"USDT","total":"9900","free":"9900","locked":"0"}],"positions":[{"symbol":"BTCUSDT","quantity":"1","avg_px":"100.00"}],"ts_event":12}"#
                .to_string(),
        );
    }
    let base_url = bound_stub(Arc::clone(&state)).await;

    let (sink_tx, mut sink_rx) = unbounded_channel::<ExecutionEvent>();
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

    let timeout = Duration::from_secs(2);
    assert!(matches!(
        next_exec_event(&mut sink_rx, timeout).await,
        ExecutionEvent::Order(OrderEventAny::Submitted(_))
    ));

    match next_exec_event(&mut sink_rx, timeout).await {
        ExecutionEvent::Order(OrderEventAny::Accepted(accepted)) => {
            assert_eq!(accepted.client_order_id, order.client_order_id());
            assert_eq!(accepted.venue_order_id, VenueOrderId::from("V-1"));
        }
        other => panic!("expected an OrderAccepted, got {other:?}"),
    }

    // The fill must map onto the right order, venue id, instrument, qty, price -
    // a mis-mapped fill (wrong order, wrong scale, dropped venue id) would still
    // be a `Filled` variant but is caught here.
    match next_exec_event(&mut sink_rx, timeout).await {
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

    match next_exec_event(&mut sink_rx, timeout).await {
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
