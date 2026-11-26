// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Submit, amend and cancel against the aggregate hold fold for a margin-equity
//! account.
//!
//! Argv: `[resting orders] [rounds]`, defaulting to 50 and 2,000; the canonical
//! decision cell is 50 resting orders. The timed wall goes to stdout as
//! `elapsed_ns`, the work sizes to stderr.

use std::{collections::HashMap, hint::black_box, time::Instant};

use mogwai_engine::{
    Engine, EngineConfig, MarginBasis, MarginBreachAction, MarginPolicy, MarketReading,
};
use mogwai_protocol::{
    AccountId, Command, InstrumentClass, InstrumentDef, OrderType, Side, SubmitOrder, TimeInForce,
};
use rust_decimal::Decimal;

fn limit(id: String, price: i64) -> SubmitOrder {
    SubmitOrder {
        client_order_id: id,
        symbol: "AAPL".into(),
        position_id: None,
        side: Side::Sell,
        order_type: OrderType::Limit,
        quantity: Decimal::ONE,
        price: Some(Decimal::from(price)),
        trigger_price: None,
        trail_offset: None,
        limit_offset: None,
        activation_price: None,
        reduce_only: false,
        post_only: false,
        time_in_force: TimeInForce::Gtc,
        expire_time: None,
        link: None,
    }
}

pub(crate) fn run(resting: usize, rounds: usize) {
    assert!(resting >= 2, "the mix needs at least two resting orders");

    let instrument = InstrumentDef {
        symbol: "AAPL".into(),
        class: InstrumentClass::Equity {
            currency: "USD".into(),
            multiplier: Decimal::ONE,
            lot_size: Decimal::ONE,
            borrowable: Some(Decimal::from(1_000_000)),
            settlement_ns: 0,
        },
        price_precision: 2,
        size_precision: 0,
        price_increment: Decimal::new(1, 2),
        size_increment: Decimal::ONE,
    };
    let mut engine = Engine::build(EngineConfig {
        account_id: AccountId::parse("BENCH-001").expect("account id"),
        instruments: vec![instrument],
        balances: HashMap::from([("USD".to_owned(), Decimal::from(10_000_000))]),
        fill_seed: 7,
    });
    engine.set_margin_policy(
        "AAPL".into(),
        MarginPolicy {
            initial_per_contract: Decimal::new(5, 1),
            maintenance_per_contract: Decimal::new(25, 2),
            breach_action: MarginBreachAction::Refuse,
            basis: MarginBasis::Notional,
        },
    );
    let reading = MarketReading::flat(Decimal::from(100), 1, 0);
    for index in 0..resting {
        black_box(engine.process_with_market(
            Command::SubmitOrder(limit(format!("BASE-{index}"), 200 + index as i64)),
            1,
            Some(reading.clone()),
        ));
    }

    let started = Instant::now();
    for round in 0..rounds {
        let id = format!("MIX-{round}");
        black_box(engine.process_with_market(
            Command::SubmitOrder(limit(id.clone(), 1_000 + round as i64)),
            2,
            Some(reading.clone()),
        ));
        black_box(engine.process_with_market(
            Command::ModifyOrder {
                client_order_id: id.clone(),
                quantity: Some(Decimal::from(2)),
                price: Some(Decimal::from(1_001 + round as i64)),
                trigger_price: None,
            },
            3,
            Some(reading.clone()),
        ));
        black_box(engine.process(
            Command::CancelOrder {
                client_order_id: id,
            },
            4,
        ));
    }
    let elapsed = started.elapsed();
    println!("elapsed_ns={}", elapsed.as_nanos());
    eprintln!("refolds_performed={}", rounds * 4);
    eprintln!("orders_resting={resting}");
    eprintln!("commands={}", rounds * 3);
}
