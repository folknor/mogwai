// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Criterion benchmarks for the engine's fill path: the ungated submit, both
//! gated submit branches, and the batch scan application. Run as
//! `brokkr run fill_bench -- --bench`; `criterion_main` parses the `--bench`
//! flag itself, which is what lets these live in an example target instead of a
//! `cargo bench` harness.
//!
//! `criterion_group!` expands to `pub fn` items in a crate where nothing is
//! reachable from outside, so the crate-level allow overrides the workspace's
//! denied `unreachable_pub`.
#![allow(unreachable_pub)]

use std::collections::HashMap;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use mogwai_engine::{Engine, EngineConfig, ScanResult};
use mogwai_protocol::{
    AccountId, ClientMessage, OrderType, Side, SubmitOrder, TimeInForce, default_instruments,
};
use rust_decimal::Decimal;

/// Every benchmark builds its engine in the setup closure. The engine is
/// stateful in ways that grow without bound across iterations - accepted client
/// order ids are retained for duplicate detection, closed orders and fills are
/// retained as history, and under the gate every submit leaves another resting
/// order in `open` - so reusing one engine would price a monotonically growing
/// structure and report the average of a ramp as a latency.
fn engine(penetration_ticks: u32) -> Engine {
    Engine::build(EngineConfig {
        account_id: AccountId::parse("BENCH").expect("static account"),
        instruments: default_instruments(),
        balances: HashMap::new(),
        penetration_ticks,
    })
}

fn order(id: String) -> SubmitOrder {
    SubmitOrder {
        client_order_id: id,
        symbol: "BTCUSDT".into(),
        side: Side::Buy,
        order_type: OrderType::Limit,
        quantity: Decimal::ONE,
        price: Some(Decimal::from(100)),
        time_in_force: TimeInForce::Gtc,
    }
}

/// One gated engine holding `size` resting limits, plus the scan results a
/// sweep pass would hand back for them. `fill` decides whether every result
/// crosses its threshold (the worst case: `size` fills plus a snapshot) or none
/// does (the common pass).
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
            counted: u32::from(fill),
            scanned_to_ns: 2,
        })
        .collect();
    (engine, results)
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
    c.bench_function("submit_full_fill", |b| {
        b.iter_batched(
            || (engine(0), order("full".into())),
            |(mut engine, order)| {
                let out = engine.process(ClientMessage::SubmitOrder(order), 1);
                (engine, out)
            },
            BatchSize::SmallInput,
        );
    });
    c.bench_function("submit_gated_rest", |b| {
        b.iter_batched(
            || (engine(1), order("rest".into())),
            |(mut engine, order)| {
                let out = engine.process_with_market(ClientMessage::SubmitOrder(order), 1, None);
                (engine, out)
            },
            BatchSize::SmallInput,
        );
    });
    // `Engine::process` passes no market price, so a bench built only on it
    // never reaches the seed branch every gated submit takes on the real path,
    // where `http.rs` and the socket handler both supply a reading. 99 is
    // strictly through the buy limit at 100, so `seeded == 1` and the submit
    // fills synchronously.
    c.bench_function("submit_gated_seeded", |b| {
        b.iter_batched(
            || (engine(1), order("seed".into())),
            |(mut engine, order)| {
                let out = engine.process_with_market(
                    ClientMessage::SubmitOrder(order),
                    1,
                    Some(Decimal::from(99)),
                );
                (engine, out)
            },
            BatchSize::SmallInput,
        );
    });
    // Two sizes per shape, not redundancy: `apply_scans` scans `open` linearly
    // per result, so a full batch is quadratic in the resting count. 50 and 200
    // differ by 4x, so a linear term reads as roughly 4x and a quadratic one as
    // roughly 16x.
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
}

criterion_group!(fill_benches, benches);
criterion_main!(fill_benches);
