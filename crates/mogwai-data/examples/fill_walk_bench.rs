// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Criterion benchmarks for the shared trigger walk and the fixed per-pass
//! cost the sweeper pays around it. Run as `brokkr run fill_walk_bench --
//! --bench`; `criterion_main` parses the `--bench` flag itself, which is what
//! lets these live in an example target instead of a `cargo bench` harness.
//!
//! `criterion_group!` expands to `pub fn` items in a crate where nothing is
//! reachable from outside, so the crate-level allow overrides the workspace's
//! denied `unreachable_pub`.
#![allow(unreachable_pub)]

use std::sync::Mutex;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use mogwai_data::{
    CheckpointIndex, Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, TickSource,
    TriggerScan, scan_triggers,
};
use mogwai_protocol::{ScanKind, Side};
use rust_decimal::Decimal;

const ORIGIN: u64 = 1_700_438_400_000_000_000;
const SPAN_NS: u64 = 1_000_000_000;
/// The sweeper's own per-pass drain budget (`fills::SWEEP_DRAIN_BUDGET`).
const BUDGET: usize = 5_000_000;
/// Mirrors the server's checkpoint spacing (`source::CHECKPOINT_K`), so the
/// positioning benchmark restores from the same grid and pays the same residual
/// drain the sweeper does.
const CHECKPOINT_K: usize = 262_144;
/// This BENCH's own walk length. It has no server counterpart: the server's
/// per-request seek budget (`MAX_HISTORY_SEEK_TICKS`) died with the lazy
/// history path, because a declared warmup is materialized eagerly and a
/// request below the floor is refused by name rather than served short. What
/// remains here is simply how far this benchmark walks.
const BENCH_SEEK_TICKS: usize = 190_000;

fn source() -> GeneratedSource {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::from_fingerprint_medians("BTCUSDT", &fp);
    scalars.modal_tick = Decimal::new(1, 2);
    scalars.price_decimals = 2;
    GeneratedSource::new(scalars, 42, ORIGIN, &fp, None)
}

/// Far-from-market buy limits: no print is ever strictly through them, so every
/// walk drains its whole span instead of returning early on a satisfied scan.
fn scans(count: usize) -> Vec<TriggerScan> {
    (0..count)
        .map(|_| TriggerScan {
            side: Side::Buy,
            px: Decimal::ONE,
            kind: ScanKind::FillThrough,
            from_ns: ORIGIN,
        })
        .collect()
}

/// The field-identical stand-in for `mogwai_engine::PendingScan`, which this
/// crate cannot name and must not start depending on. The mapping benchmark
/// prices the allocation and the per-field copy the server wrapper pays each
/// pass, and nothing else.
type ScanTuple = (Side, Decimal, u64);

fn benches(c: &mut Criterion) {
    for (name, count) in [
        ("walk_one_pass_1_scan", 1),
        ("walk_one_pass_50_scans", 50),
        ("walk_one_pass_500_scans", 500),
    ] {
        let scans = scans(count);
        // A `TickSource` is consumed as it drains, so the source is cloned per
        // iteration: reusing one would leave every iteration after the first
        // already past `to_ns`, breaking on its first pull and reporting the
        // cost of one `next_tick` as the cost of a pass.
        //
        // The source is handed back as part of the output: `iter_batched` drops
        // outputs after stopping the timer but drops a consumed input inside the
        // timed region, and a `GeneratedSource` teardown is not free.
        c.bench_function(name, |b| {
            b.iter_batched(
                source,
                |mut source| {
                    let walk = scan_triggers(&mut source, &scans, ORIGIN + SPAN_NS, BUDGET);
                    (source, walk)
                },
                BatchSize::SmallInput,
            );
        });
    }

    let mapping_input: Vec<ScanTuple> = scans(50)
        .into_iter()
        .map(|scan| (scan.side, scan.px, scan.from_ns))
        .collect();
    c.bench_function("scan_mapping_50", |b| {
        b.iter_batched(
            || mapping_input.clone(),
            |input| {
                input
                    .iter()
                    .map(|&(side, px, from_ns)| TriggerScan {
                        side,
                        px,
                        kind: ScanKind::FillThrough,
                        from_ns,
                    })
                    .collect::<Vec<_>>()
            },
            BatchSize::SmallInput,
        );
    });

    // The sweeper's fixed per-pass cost is positioning, not walking, and the two
    // scale with different things - the seek distance versus print density and
    // scan count - so they are timed apart. Shaped like
    // `fills::scan_triggers`: a checkpoint restore out of a long-lived
    // process-wide index, taken under a lock, then the residual drain through
    // `MergeSource::starting_at` behind a `Box<dyn TickSource>`.
    let index = Mutex::new(CheckpointIndex::new(
        source(),
        CHECKPOINT_K,
        BENCH_SEEK_TICKS,
    ));
    let target = ORIGIN + SPAN_NS;
    // Prime the index so the timed region measures a steady-state restore rather
    // than the one-off from-origin extension, which is what the server pays after
    // its first pass on a symbol.
    let _ = index.lock().expect("index").source_at_or_before(target);
    c.bench_function("source_positioning", |b| {
        b.iter_batched(
            || (),
            // Batched with a unit setup so the positioned source is an OUTPUT
            // and its teardown falls outside the timed region, as in the walk
            // benchmarks above.
            |()| {
                let restored = index.lock().expect("index").source_at_or_before(target);
                let boxed: Box<dyn TickSource> = Box::new(restored);
                MergeSource::starting_at(vec![boxed], Some(target))
            },
            BatchSize::SmallInput,
        );
    });
}

criterion_group!(fill_walk_benches, benches);
criterion_main!(fill_walk_benches);
