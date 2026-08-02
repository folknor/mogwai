// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! A deterministic, end-to-end fill-timing distribution for the fill band,
//! pinned byte-exactly by a committed artifact.
//!
//! It lives in the bin crate because this is the only place that can see
//! `fills::scan_triggers`, `Engine::apply_scans` and `InstrumentProfiles`
//! at once - precisely the seam being certified. The harness reproduces
//! `sweeper.rs`'s three-phase pass synchronously (pending scans, one walk per
//! symbol, apply) rather than sharing code with it: the async loop, the teardown
//! race and the per-connection delivery are not what a fill distribution
//! measures.
//! It does NOT reimplement the walk or the gate - both are the shipped
//! functions, which is the whole point. A golden computed by a hand-rolled tape
//! loop would certify nothing but itself.
//!
//! `GeneratedSource` is deterministic given the committed fingerprint, the fixed
//! origin and the fixed order population, so every value in the artifact is an
//! integer and the comparison is exact: any change to the fingerprint, the
//! generator, the predicate, the seeding rule or the frontier arithmetic moves
//! this file, and any change that does not move it did not change fill timing.
//!
//! Regeneration is an explicit operator act with no env var and no bless switch:
//! delete the artifact, run this test (it WRITES the file and then FAILS by
//! design), inspect the diff, run again. Absence is not a pass - a golden that
//! was dropped from a checkout or never committed would otherwise be green
//! forever with the guard silently absent.
//!
//! Orders unfilled at the horizon are CENSORED - counted, and excluded from the
//! sample rather than folded in at the horizon value, which would make the
//! artifact move whenever the horizon moved.
//!
//! Out of scope, deliberately: the tranche-redraw path. A remainder needs an
//! armed `PartialFillNext`; this harness arms no divergence, so every admitted
//! order fills completely and leaves the book.
//!
//! Where the band comes from: nothing hands this harness a `MarketReading`,
//! because it never goes through the HTTP submit path, so it takes one itself
//! with `fills::read_market` at each order's OWN acceptance instant, under the
//! scenario's multiplier and the default 200-tick clamp. That is per-order
//! rather than one reading per scenario - a deliberate departure from the
//! spec's anchor-instant reuse, and it costs one extra tape walk per acceptance
//! (measured at roughly 0.2 ms each). It buys exactly what the shipped submit
//! path does: each order is banded by the regime it actually arrived in, so the
//! artifact moves when the estimator moves. Its cost is bounded by the same
//! `SWEEP_DRAIN_BUDGET` the sweep pays.
//!
//! The two scenarios are the degenerate `0.0` band and the shipped multiplier.
//! Retired rather than migrated: the old `penetration_ticks = 3` cell covered
//! ACCUMULATION of penetrations across sweep boundaries, and this model has no
//! counter to accumulate - one print through the trigger fills.

use std::collections::HashMap;
use std::path::PathBuf;

use mogwai_data::TickEvent;
use mogwai_engine::{Engine, EngineConfig, ScanResult};
use mogwai_protocol::{
    AccountId, ClientMessage, OrderType, RunSeeds, ServerMessage, Side, SubmitOrder, TimeInForce,
    default_instruments,
};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::{fills, source::InstrumentProfiles};

const SYMBOL: &str = "BTCUSDT";
const GOLDEN_RUN_SEED: u64 = 42;
const GOLDEN_WARMUP_NS: u64 = 86_400_000_000_000;
const ORIGIN: u64 = crate::source::TAPE_ORIGIN_NS + GOLDEN_WARMUP_NS;
const SWEEP_INTERVAL_NS: u64 = 1_000_000_000;
const HORIZON_NS: u64 = 1_200_000_000_000;
const ORDERS_PER_OFFSET: usize = 40;
const ACCEPT_STRIDE_NS: u64 = 3_000_000_000;
/// Limit placement in price increments away from the market at acceptance,
/// always >= 1 so nothing is marketable on arrival and every fill's timing is a
/// function of the tape rather than of the acceptance seed.
///
/// Half-decade spacing, not decades. BTCUSDT's increment is 0.01 against a
/// fitted tape near 37,000, so one tick is well under a basis point and 100
/// ticks is roughly 2.7. A full decade ladder to 10,000 ticks put the top two
/// rungs 27 and 270 basis points out, which this tape does not travel inside a
/// twenty-minute horizon: both cells censored completely and pinned an empty
/// sample. 1 to 100 spans saturation to near-total censoring, so every cell
/// carries a distribution that moves when fill timing moves.
const OFFSETS: [u32; 5] = [1, 3, 10, 30, 100];
/// The shipped `fill_band_max_ticks` default, so the artifact is produced under
/// the clamp a real venue runs.
const MAX_TICKS: u32 = 200;

#[derive(Serialize)]
struct Golden {
    schema: u32,
    symbol: &'static str,
    data_origin_ns: u64,
    sweep_interval_ns: u64,
    horizon_ns: u64,
    orders_per_offset: usize,
    accept_stride_ns: u64,
    cells: Vec<Cell>,
}

#[derive(Serialize)]
struct Cell {
    band_vol_mult: f64,
    offset_ticks: u32,
    samples: usize,
    filled: usize,
    censored: usize,
    buy_filled: usize,
    sell_filled: usize,
    latency_ns: Vec<u64>,
    passes: Vec<u64>,
}

struct OrderMeta {
    offset_ticks: u32,
    side: Side,
    accept_ns: u64,
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/fill_distribution.json")
}

fn run_scenario(band_vol_mult: f64, profiles: &InstrumentProfiles) -> Vec<Cell> {
    let total = OFFSETS.len() * ORDERS_PER_OFFSET;
    // The last acceptance must leave every order at least half the horizon in
    // which to fill, so a later edit to the population, the stride or the
    // horizon cannot silently push acceptances past the end and censor a whole
    // tail as if the tape had done it.
    assert!(
        (total as u64 - 1) * ACCEPT_STRIDE_NS * 2 <= HORIZON_NS,
        "the acceptance schedule must end inside half the horizon"
    );
    let mut engine = Engine::build(EngineConfig {
        account_id: AccountId::parse("GOLDEN").expect("static account"),
        instruments: default_instruments(),
        balances: HashMap::new(),
        fill_seed: RunSeeds::from_run_seed(GOLDEN_RUN_SEED).fill,
    });
    let increment = default_instruments()
        .into_iter()
        .find(|instrument| instrument.symbol == SYMBOL)
        .expect("BTCUSDT exists")
        .price_increment;
    let mut meta = HashMap::new();
    let mut samples: HashMap<u32, Vec<(u64, u64, Side)>> = HashMap::new();
    for ts in (ORIGIN..=ORIGIN + HORIZON_NS).step_by(SWEEP_INTERVAL_NS as usize) {
        let i = ((ts - ORIGIN) / ACCEPT_STRIDE_NS) as usize;
        if i < total && (ts - ORIGIN).is_multiple_of(ACCEPT_STRIDE_NS) {
            let offset_ticks = OFFSETS[i % OFFSETS.len()];
            let side = if i.is_multiple_of(2) {
                Side::Buy
            } else {
                Side::Sell
            };
            // The anchor the harness prices its own limit off. `None` only at
            // the very origin, where no print exists at or before `ts` yet, so
            // the first order of each scenario anchors on the tape's first
            // print instead. That is a look-ahead the venue itself would refuse,
            // but it only positions the harness's price: the limit is placed
            // `offset_ticks` away from the very reading it is judged against, so
            // no order is ever marketable on arrival either way.
            let market = fills::read_last(SYMBOL, ts, profiles)
                .or_else(|| {
                    let mut tape =
                        crate::source::build_history_source(SYMBOL, Some(ORIGIN), profiles)?;
                    match tape.next_tick()? {
                        TickEvent::Trade(trade) => Some(trade.price),
                        TickEvent::Quote(_) => None,
                    }
                })
                .expect("clean tape has an initial price");
            let offset = increment * Decimal::from(offset_ticks);
            let price = match side {
                Side::Buy => market - offset,
                Side::Sell => market + offset,
            };
            let id = format!("g{band_vol_mult}-{i}");
            meta.insert(
                id.clone(),
                OrderMeta {
                    offset_ticks,
                    side,
                    accept_ns: ts,
                },
            );
            let order = SubmitOrder {
                client_order_id: id,
                symbol: SYMBOL.into(),
                side,
                order_type: OrderType::Limit,
                quantity: Decimal::ONE,
                price: Some(price),
                trigger_price: None,
                reduce_only: false,
                post_only: false,
                time_in_force: TimeInForce::Gtc,
            };
            // Exactly what `http::market_reading` hands the engine on the real
            // path, refusals included: a refused reading is passed on as `None`
            // rather than papered over with a synthetic zero band.
            let reading = fills::read_market(SYMBOL, ts, profiles, band_vol_mult, MAX_TICKS);
            let submitted =
                engine.process_with_market(ClientMessage::SubmitOrder(order), ts, reading);
            record_fills(&submitted, &meta, &mut samples);
        }
        let scans = engine.pending_scans();
        if scans.is_empty() {
            continue;
        }
        let walk = fills::scan_triggers(SYMBOL, &scans, ts, profiles)
            .expect("scenario starts on reachable clean tape");
        let results = scans
            .iter()
            .zip(walk.hits)
            .map(|(scan, hit)| ScanResult {
                client_order_id: scan.client_order_id.clone(),
                from_ns: scan.from_ns,
                revision: scan.revision,
                hit,
                scanned_to_ns: walk.reached_ns,
            })
            .collect::<Vec<_>>();
        let (events, _) = engine.apply_scans(&results, ts);
        record_fills(&events, &meta, &mut samples);
    }
    OFFSETS
        .into_iter()
        .map(|offset_ticks| {
            let mut values = samples.remove(&offset_ticks).unwrap_or_default();
            values.sort_by_key(|(latency, _, _)| *latency);
            let filled = values.len();
            Cell {
                band_vol_mult,
                offset_ticks,
                samples: ORDERS_PER_OFFSET,
                filled,
                censored: ORDERS_PER_OFFSET - filled,
                buy_filled: values
                    .iter()
                    .filter(|(_, _, side)| *side == Side::Buy)
                    .count(),
                sell_filled: values
                    .iter()
                    .filter(|(_, _, side)| *side == Side::Sell)
                    .count(),
                latency_ns: values.iter().map(|(latency, _, _)| *latency).collect(),
                passes: values.iter().map(|(_, passes, _)| *passes).collect(),
            }
        })
        .collect()
}

fn record_fills(
    events: &[ServerMessage],
    meta: &HashMap<String, OrderMeta>,
    samples: &mut HashMap<u32, Vec<(u64, u64, Side)>>,
) {
    for event in events {
        let ServerMessage::OrderFilled(fill) = event else {
            continue;
        };
        let order = meta
            .get(&fill.client_order_id)
            .expect("golden fill belongs to its schedule");
        // Latency is measured against the order's own acceptance instant, not
        // against the pass instant. The two are equal by construction, because
        // `ACCEPT_STRIDE_NS` is an exact multiple of `SWEEP_INTERVAL_NS`, so
        // every acceptance lands on a pass - which is also why the elapsed pass
        // count is exactly the latency divided by the interval rather than a
        // separately tracked counter that could disagree with it.
        let latency = fill.ts_event - order.accept_ns;
        samples.entry(order.offset_ticks).or_default().push((
            latency,
            latency / SWEEP_INTERVAL_NS,
            order.side,
        ));
    }
}

fn render() -> String {
    let profiles = InstrumentProfiles::defaults();
    let golden = Golden {
        schema: 2,
        symbol: SYMBOL,
        data_origin_ns: ORIGIN,
        sweep_interval_ns: SWEEP_INTERVAL_NS,
        horizon_ns: HORIZON_NS,
        orders_per_offset: ORDERS_PER_OFFSET,
        accept_stride_ns: ACCEPT_STRIDE_NS,
        cells: [0.0, 0.5]
            .into_iter()
            .flat_map(|ticks| run_scenario(ticks, &profiles))
            .collect(),
    };
    format!(
        "{}\n",
        serde_json::to_string_pretty(&golden).expect("golden serializes")
    )
}

/// Properties of correct code, not of a particular tape, asserted against the
/// FRESHLY computed result before any comparison or write - so broken code can
/// never produce a blessable artifact. The committed file inherits all of them,
/// since the comparison below proves it equals this result.
fn assert_shape(rendered: &str) {
    let golden: serde_json::Value = serde_json::from_str(rendered).expect("rendered JSON");
    let cells = golden["cells"].as_array().expect("cells array");
    assert_eq!(cells.len(), 2 * OFFSETS.len());
    for (index, cell) in cells.iter().enumerate() {
        let expected_mult = if index < OFFSETS.len() { 0.0 } else { 0.5 };
        assert_eq!(cell["band_vol_mult"], expected_mult);
        assert_eq!(cell["offset_ticks"], OFFSETS[index % OFFSETS.len()]);
        let samples = cell["samples"].as_u64().expect("samples");
        let filled = cell["filled"].as_u64().expect("filled");
        assert_eq!(samples, ORDERS_PER_OFFSET as u64);
        assert_eq!(
            filled + cell["censored"].as_u64().expect("censored"),
            samples
        );
        assert_eq!(
            cell["latency_ns"].as_array().expect("latencies").len(),
            filled as usize
        );
        assert_eq!(
            cell["passes"].as_array().expect("passes").len(),
            filled as usize
        );
    }
    // The nearest cell of each scenario: both predicate directions must
    // participate, and a majority of the sample must fill. "At least one fill"
    // would pass for a build that filled a single order or only buys; an
    // inverted predicate or a frontier that never advances fails here.
    for index in [0, OFFSETS.len()] {
        let cell = &cells[index];
        assert!(
            cell["buy_filled"].as_u64().expect("buy fills") > 0,
            "cell {index}: no buy filled at the nearest offset"
        );
        assert!(
            cell["sell_filled"].as_u64().expect("sell fills") > 0,
            "cell {index}: no sell filled at the nearest offset"
        );
        assert!(
            cell["filled"].as_u64().expect("filled") * 2 > ORDERS_PER_OFFSET as u64,
            "cell {index}: the nearest offset filled a minority of its sample"
        );
    }
    // The one inspection property this artifact can actually assert. It is
    // implied PATHWISE rather than statistically - `u >= 0` moves a trigger
    // away from the market, so a banded order fills only on a subset of the
    // tapes that fill an unbanded one at the same price - so it holds per order
    // and needs no tolerance.
    //
    // Censoring rising with `offset_ticks` is NOT asserted, deliberately. It
    // would only be a valid test of a correct model on PAIRED cohorts - every
    // offset submitted at the same acceptance instants under the same identity
    // stem - and this harness rotates offsets through the acceptance schedule
    // instead, so different offsets are accepted at different tape instants
    // under different order identities and finite-sample noise can invert two
    // adjacent offsets even when the model is exactly right. A revert condition
    // that fires on noise is worse than no revert condition.
    for index in 0..OFFSETS.len() {
        let unbanded = cells[index]["filled"].as_u64().expect("filled");
        let banded = cells[index + OFFSETS.len()]["filled"]
            .as_u64()
            .expect("filled");
        assert!(
            unbanded >= banded,
            "a banded trigger cannot be easier than an unbanded trigger"
        );
    }
}

/// Name the first cell that moved and, inside it, the first field or sample
/// index that moved. A whole-file `assert_eq!` on two multi-kilobyte JSON blobs
/// is unreadable, and the point of the golden is to say WHAT changed.
fn describe_mismatch(rendered: &str, expected: &str) -> String {
    let (Ok(new), Ok(old)) = (
        serde_json::from_str::<serde_json::Value>(rendered),
        serde_json::from_str::<serde_json::Value>(expected),
    ) else {
        return "the committed golden is not parseable JSON".to_owned();
    };
    for (key, value) in new.as_object().expect("rendered object") {
        if key != "cells" && old.get(key) != Some(value) {
            return format!("config field {key}: {:?} became {value:?}", old.get(key));
        }
    }
    let new_cells = new["cells"].as_array().expect("rendered cells");
    let old_cells = old["cells"].as_array().map_or(&[][..], Vec::as_slice);
    if new_cells.len() != old_cells.len() {
        return format!("cell count {} became {}", old_cells.len(), new_cells.len());
    }
    for (index, (new_cell, old_cell)) in new_cells.iter().zip(old_cells).enumerate() {
        if new_cell == old_cell {
            continue;
        }
        let label = format!(
            "cell {index} (band_vol_mult={}, offset_ticks={})",
            new_cell["band_vol_mult"], new_cell["offset_ticks"]
        );
        for field in ["samples", "filled", "censored", "buy_filled", "sell_filled"] {
            if new_cell[field] != old_cell[field] {
                return format!(
                    "{label}: {field} {} became {}",
                    old_cell[field], new_cell[field]
                );
            }
        }
        for field in ["latency_ns", "passes"] {
            let new_values = new_cell[field].as_array().expect("sample vector");
            let old_values = old_cell[field].as_array().map_or(&[][..], Vec::as_slice);
            if let Some((at, (new_value, old_value))) = new_values
                .iter()
                .zip(old_values)
                .enumerate()
                .find(|(_, (new_value, old_value))| new_value != old_value)
            {
                return format!("{label}: {field}[{at}] {old_value} became {new_value}");
            }
        }
        return format!("{label}: differs");
    }
    "no structural difference found; the files differ only in formatting".to_owned()
}

#[test]
fn fill_distribution_matches_the_golden() {
    crate::source::set_boot_for_test(crate::source::BootTape {
        seeds: RunSeeds::from_run_seed(GOLDEN_RUN_SEED),
        regime: None,
    });
    let rendered = render();
    assert_shape(&rendered);
    let path = golden_path();
    match std::fs::read_to_string(&path) {
        Ok(expected) => assert!(
            rendered == expected,
            "golden changed: {}\ndelete {}, rerun this test, inspect the diff, then rerun",
            describe_mismatch(&rendered, &expected),
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path.parent().expect("golden parent"))
                .expect("create golden parent");
            std::fs::write(&path, rendered).expect("write golden");
            panic!(
                "wrote {} ({} cells); inspect the regenerated artifact and rerun",
                path.display(),
                2 * OFFSETS.len()
            );
        }
        Err(error) => panic!("read {}: {error}", path.display()),
    }
}
