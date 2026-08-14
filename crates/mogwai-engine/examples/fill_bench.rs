// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Criterion benchmarks for the engine's fill path: the immediate submit, both
//! banded submit branches, and the batch scan application. Run as
//! the `fill_bench` target with `-- --bench`; `criterion_main` parses `--bench`
//! flag itself, which is what lets these live in an example target instead of a
//! `cargo bench` harness.
//!
//! `criterion_group!` expands to `pub fn` items in a crate where nothing is
//! reachable from outside, so the crate-level allow overrides the workspace's
//! denied `unreachable_pub`.
#![allow(unreachable_pub)]

use std::collections::HashMap;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use mogwai_engine::{BreachAction, Engine, EngineConfig, MarginPolicy, MarketReading, ScanResult};
use mogwai_protocol::Hit;
use mogwai_protocol::{
    AccountId, ClientMessage, InstrumentClass, InstrumentDef, OrderType, Side, SubmitOrder,
    TimeInForce, WireAssetClass, default_instruments,
};
use rust_decimal::Decimal;

/// Every benchmark builds its engine in the setup closure. The engine is
/// stateful in ways that grow without bound across iterations - accepted client
/// order ids are retained for duplicate detection, closed orders and fills are
/// retained as history, and every resting submit leaves another order in
/// `open` - so reusing one engine would price a monotonically growing
/// structure and report the average of a ramp as a latency.
fn engine(fill_seed: u64) -> Engine {
    Engine::build(EngineConfig {
        account_id: AccountId::parse("BENCH").expect("static account"),
        instruments: default_instruments(),
        balances: HashMap::new(),
        fill_seed,
    })
}

fn order(id: String) -> SubmitOrder {
    SubmitOrder {
        client_order_id: id,
        symbol: "BTCUSDT".into(),
        position_id: None,
        side: Side::Buy,
        order_type: OrderType::Limit,
        quantity: Decimal::ONE,
        price: Some(Decimal::from(100)),
        trigger_price: None,
        reduce_only: false,
        post_only: false,
        time_in_force: TimeInForce::Gtc,
    }
}

/// One engine holding `size` resting limits, plus the scan results a sweep pass
/// would hand back for them. `fill` decides whether the tape triggered every one
/// (the worst case: `size` fills plus a snapshot) or none of them (the common
/// pass).
fn scans(size: usize, fill: bool) -> (Engine, Vec<ScanResult>) {
    let mut engine = engine(1);
    for index in 0..size {
        let _ = engine.process(ClientMessage::SubmitOrder(order(format!("b{index}"))), 1);
    }
    let results = engine
        .pending_scans()
        .iter()
        .map(|scan| ScanResult {
            client_order_id: scan.client_order_id.clone(),
            from_ns: scan.from_ns,
            revision: scan.revision,
            hit: fill.then_some(Hit {
                ts_ns: 2,
                px: scan.px,
            }),
            scanned_to_ns: 2,
        })
        .collect();
    (engine, results)
}

fn futures_book(size: usize) -> (Engine, Vec<(mogwai_protocol::Symbol, Decimal)>) {
    let instruments: Vec<_> = (0..size)
        .map(|index| InstrumentDef {
            symbol: format!("F{index}").into(),
            class: InstrumentClass::Future {
                underlying: format!("U{index}"),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
        })
        .collect();
    let mut engine = Engine::build(EngineConfig {
        account_id: AccountId::parse("BENCH").expect("static account"),
        instruments,
        balances: HashMap::from([("USD".into(), Decimal::from(1_000_000))]),
        fill_seed: 1,
    });
    let policy = MarginPolicy {
        initial_per_contract: Decimal::from(2000),
        maintenance_per_contract: Decimal::from(1800),
        breach_action: BreachAction::Refuse,
    };
    for index in 0..size {
        let symbol: mogwai_protocol::Symbol = format!("F{index}").into();
        engine.set_margin_policy(std::sync::Arc::clone(&symbol), policy);
        let submit = SubmitOrder {
            client_order_id: format!("OPEN-{index}"),
            symbol: std::sync::Arc::clone(&symbol),
            position_id: None,
            side: Side::Buy,
            order_type: OrderType::Market,
            quantity: Decimal::ONE,
            price: Some(Decimal::from(21_000)),
            trigger_price: None,
            reduce_only: false,
            post_only: false,
            time_in_force: TimeInForce::Gtc,
        };
        let _ = engine.process_with_market(
            ClientMessage::SubmitOrder(submit),
            1,
            Some(MarketReading {
                last_px: Decimal::from(21_000),
                ts_ns: 1,
                band_ticks: 0,
            }),
        );
    }
    let marks = (0..size)
        .map(|index| (format!("F{index}").into(), Decimal::from(21_001)))
        .collect();
    (engine, marks)
}

fn benches(c: &mut Criterion) {
    // The client order id and the `SubmitOrder` are built in the setup closure
    // too, so neither id formatting nor `Decimal` parsing lands in the timed
    // region.
    //
    // Every routine hands its engine back as part of the output. `iter_batched`
    // builds inputs before the timer starts and drops OUTPUTS after it stops,
    // but an input consumed by the routine is dropped inside the timed region -
    // and tearing down an engine carrying an instrument table, an id set and a
    // fill history costs more than the submit being measured, with allocator
    // noise on top.
    c.bench_function("submit_immediate", |b| {
        b.iter_batched(
            || (engine(0), order("full".into())),
            |(mut engine, order)| {
                let out = engine.process(ClientMessage::SubmitOrder(order), 1);
                (engine, out)
            },
            BatchSize::SmallInput,
        );
    });
    c.bench_function("submit_banded_rest", |b| {
        b.iter_batched(
            || (engine(1), order("rest".into())),
            |(mut engine, order)| {
                let out = engine.process_with_market(ClientMessage::SubmitOrder(order), 1, None);
                (engine, out)
            },
            BatchSize::SmallInput,
        );
    });
    // `Engine::process` passes no reading, so a bench built only on it never
    // reaches the marketable-on-arrival branch a submit takes on the real path,
    // where `http.rs` and the socket handler both supply one. 99 is
    // strictly through the buy limit at 100 at a zero band, so the order is
    // marketable on arrival and fills synchronously.
    c.bench_function("submit_banded_marketable", |b| {
        b.iter_batched(
            || (engine(1), order("seed".into())),
            |(mut engine, order)| {
                let out = engine.process_with_market(
                    ClientMessage::SubmitOrder(order),
                    1,
                    Some(mogwai_engine::MarketReading {
                        last_px: Decimal::from(99),
                        ts_ns: 0,
                        band_ticks: 0,
                    }),
                );
                (engine, out)
            },
            BatchSize::SmallInput,
        );
    });
    // Two sizes per shape, not redundancy: 50 and 200 differ by 4x, so a
    // linear term reads as roughly 4x and a quadratic one as roughly 16x. This
    // is the regression witness for keyed result lookup and O(1) book removal.
    for (name, size, fill) in [
        ("apply_scans_50", 50, false),
        ("apply_scans_200", 200, false),
        ("apply_scans_50_all_fill", 50, true),
        ("apply_scans_200_all_fill", 200, true),
    ] {
        c.bench_function(name, |b| {
            b.iter_batched(
                || scans(size, fill),
                |(mut engine, results)| {
                    let out = engine.apply_scans(&results, 2);
                    (engine, results, out)
                },
                BatchSize::SmallInput,
            );
        });
    }
    for (name, size) in [("mark_pass_1_future", 1), ("mark_pass_4_futures", 4)] {
        c.bench_function(name, |b| {
            b.iter_batched(
                || futures_book(size),
                |(mut engine, marks)| {
                    let out = engine.mark(&marks, 2);
                    (engine, marks, out)
                },
                BatchSize::SmallInput,
            );
        });
    }
}

criterion_group!(fill_benches, benches);
criterion_main!(fill_benches);
