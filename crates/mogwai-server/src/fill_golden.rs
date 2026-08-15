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
//! The instrument is the SHIPPED BTCUSDT PRESET, resolved through the same
//! `config::profile_for_symbol` path a run boots with, rather than the retired
//! median-derived built-in default. That preset declares no generator or
//! session table, so its scalars are the fingerprint medians the retired
//! bundle forced and the committed artifact did not move when the default
//! bundle changed - which is the evidence behind that claim.
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
//!
//! # Why the committed artifact was last re-blessed
//!
//! The tape root became per-river at `TAPE_PROTOCOL_VERSION` 17. A river's
//! generator seed is now derived from the run seed AND the requested symbol
//! label rather than from the run seed alone, so this harness - which renders
//! through a real `Rivers` at `GOLDEN_RUN_SEED` on BTCUSDT - walks a different
//! tape than it did at 16. Nothing about the band, the predicate, the frontier
//! or the scenario moved: the fill stream is still run-level and its vectors in
//! `mogwai-protocol`'s `derived_streams_differ_and_are_stable` are unchanged,
//! which is the evidence that only the TAPE under the harness moved.
//!
//! The artifact therefore moved in NO predicted direction, and it must not be
//! read as one - it is a fresh draw, not the same draw under a repair. Six
//! cells go from 5 filled to 4 - the 1, 30 and 100 tick rungs in both
//! scenarios - which at 5 samples per cell is one order apiece and
//! well inside the 20-percentage-point resolution this coverage buys. What was
//! checked before accepting it is the structure and the properties, all of them
//! machine-asserted in `assert_shape` before any write: same schema, same
//! origin, sweep, horizon, order count and stride, same ten cells at the same
//! offsets, both sides still participating at the nearest rung, a majority of
//! the nearest rung still filling, and the pathwise `unbanded >= banded`
//! ordering intact at every rung. The banded half also remains byte-identical
//! to the unbanded half, for the resolution reason set out below.
//!
//! # Why the committed artifact was re-blessed before that
//!
//! The GARCH recursion was repaired. Its innovation is now standardized to unit
//! variance, which is what `GarchVol::new`'s `a0` derivation always assumed; the
//! raw Student-t(4) it was fed instead has variance 2, so the true second-moment
//! condition was `a1 * E[z^2] + b1` = 1.115 and the process had no stationary
//! variance. It stayed bounded only by its own rails, ran 8.17x hotter than
//! `vol_scalar` claimed, and sat pinned at the variance cap 12.96 percent of the
//! time. `a1`, `b1` and `vol_scalar` were re-solved against the corrected
//! condition and the rails re-derived from a measured tail.
//!
//! The artifact moved in ONE direction, and it is the direction that repair
//! predicts: cells that previously CENSORED an order - never filled it inside
//! the horizon - now fill it, so `censored` drops to zero in four cells and the
//! latency vectors gain a long entry apiece. A stationary tape at the corrected
//! scale traverses more ticks per unit time, so a resting limit that used to
//! outlive the horizon now gets reached. Fill timing did not regress; the tape
//! stopped being artificially sluggish.
//!
//! This did NOT re-open the band calibration below, and that is measured rather
//! than assumed. `fill_band_vol_mult` is selected by `fills::vol_probe` against
//! the tape's realized volatility, and the repair moved that volatility by
//! roughly 1.3x in RMS. Re-run against protocol 6 the probe reads `0.001` at
//! median 0 and p90 1, `0.002` at median 1 and p90 3, `0.005` at median 4 and
//! p90 8, and `0.010` at median 9 and p90 16 - so `0.005` is still the smallest
//! multiplier satisfying the 3-to-100-tick median rule and the default stands.
//! Only the p90 moved, from 7 to 8, which is why the durable comments quoting it
//! were updated with this landing.
//!
//! # Why the committed artifact was re-blessed before both of those
//!
//! The banded scenario runs at `Config::default().fill_band_vol_mult`, and that
//! default moved from `0.5` to `0.005` when the fill band was re-calibrated for
//! the raw-fill cadence. The raw-fill tape carries ~15,700 returns in the
//! estimator's 300 s window where the print-layer tape carried ~32, so at `0.5`
//! the implied band ran a median 439 ticks against the 200-tick
//! `fill_band_max_ticks` clamp: every banded trigger was drawn uniformly across
//! the whole clamp range, and the tape had stopped deciding fills. `0.005` is
//! what `fills::vol_probe`'s PROCEED rule selects on the current fingerprint
//! (median 4 ticks, p90 7). The artifact therefore moved because the SCENARIO
//! moved, not because fill timing regressed - the banded cells are now measured
//! under a band that tracks volatility instead of one pinned to its ceiling.
//!
//! READ THE RE-BLESSED ARTIFACT BEFORE TRUSTING ITS BANDED HALF. At `0.005` the
//! five banded cells came out BYTE-IDENTICAL to the five unbanded ones - same
//! fill counts, same latency vectors, same pass counts. That is not a bug in the
//! harness and it does not violate the `unbanded >= banded` property asserted
//! below (equality satisfies it), but it does mean the banded half currently
//! certifies only that the band PIPELINE runs, not that the band BITES.
//!
//! The cause is resolution, not calibration. Latency here is quantized to
//! `SWEEP_INTERVAL_NS`, one second, and one second of raw-fill tape carries
//! roughly fifty prints travelling far more than the 0-to-4 ticks (about 0.1
//! basis points on a 37,000 tape) that a `0.005` band displaces a trigger by. The
//! band moves the trigger; the tape crosses the difference inside the same sweep
//! pass, so the recorded latency does not move. A wider band showed up here only
//! because it was clamp-saturated at 200 ticks, which is precisely the state the
//! re-calibration removed.
//!
//! So the two knobs that would restore discrimination are a finer
//! `SWEEP_INTERVAL_NS` and a tighter offset ladder, both of which cost runtime
//! and neither of which is taken here. `notes/todo.md` carries it as owed work.
//!
//! # Why the coverage is smaller than the print-layer harness's
//!
//! Four dimensions shrank when the tape moved to the raw-fill cadence: warmup
//! 24 h to 1 h, horizon 20 min to 1 min, 40 orders per offset to 5, and the
//! acceptance stride 3 s to 1 s. Only the first three reduce work; the stride
//! shrank so the smaller population still spans a meaningful share of the
//! shorter horizon.
//!
//! It is a RUNTIME trade, and it is safe because the thing being reduced is
//! SIMULATED TIME, while what the artifact needs is PRINTS. The raw-fill tape
//! carries roughly 8.5x the prints per unit of sim time that the print layer
//! did, so one sim minute of horizon now contains more tape than the old
//! twenty-minute horizon did, and one sim hour of warmup fills the estimator's
//! 300 s window many times over - the reason 24 h was needed was that the
//! print-layer window was print-starved, which the probe's 0-of-128 cold-window
//! refusal census says is no longer the case. The cost that did NOT shrink with
//! sim time is the per-order `read_market` walk and the checkpoint restore each
//! one pays, which is why the ORDER COUNT came down as well.
//!
//! What the coverage still establishes is unchanged in kind: five offset rungs
//! from 1 to 100 ticks spanning saturation to near-total censoring, both sides
//! participating at the nearest rung, and the pathwise `unbanded >= banded`
//! ordering at every rung - all asserted in `assert_shape` before any comparison
//! or write. What it establishes LESS of is tail resolution: 5 samples per cell
//! resolves a fill fraction to 20 percentage points, so a cell's censoring count
//! is a coarse reading and the artifact's value is byte-exact REGRESSION
//! detection - any change to the fingerprint, generator, predicate, seeding or
//! frontier arithmetic still moves it - rather than an estimate of the fill
//! distribution's shape. `fills::vol_probe` is where distributional questions
//! get answered.

use std::collections::HashMap;
use std::path::PathBuf;

use mogwai_data::TickEvent;
use mogwai_engine::{Engine, EngineConfig, ScanResult};
use mogwai_protocol::{
    AccountId, ClientMessage, OrderType, RunSeeds, ServerMessage, Side, SubmitOrder, TimeInForce,
};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::{fills, source::InstrumentProfiles};

const SYMBOL: &str = "BTCUSDT";
const GOLDEN_RUN_SEED: u64 = 42;
const GOLDEN_WARMUP_NS: u64 = 3_600_000_000_000;
const ORIGIN: u64 = crate::source::TAPE_ORIGIN_NS + GOLDEN_WARMUP_NS;
const SWEEP_INTERVAL_NS: u64 = 1_000_000_000;
const HORIZON_NS: u64 = 60_000_000_000;
const ORDERS_PER_OFFSET: usize = 5;
const ACCEPT_STRIDE_NS: u64 = 1_000_000_000;
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

/// The banded scenario's multiplier, READ from the shipped default rather than
/// written out here. Two copies of a calibration constant is how a golden ends
/// up certifying a band nothing runs: this way moving the default moves the
/// artifact, the test fails, and the re-bless is forced rather than forgotten.
fn shipped_mult() -> f64 {
    crate::config::Config::default().fill_band_vol_mult
}

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

fn run_scenario(band_vol_mult: f64, profiles: &crate::source::Rivers) -> Vec<Cell> {
    let def = profiles
        .profiles()
        .get(SYMBOL)
        .expect("resolved BTCUSDT profile exists")
        .def
        .clone();
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
        instruments: vec![def.clone()],
        balances: HashMap::new(),
        fill_seed: RunSeeds::from_run_seed(GOLDEN_RUN_SEED).fill,
    });
    let increment = def.price_increment;
    let mut meta = HashMap::new();
    let mut samples: HashMap<u32, Vec<(u64, u64, Side)>> = HashMap::new();
    for ts in (ORIGIN..=ORIGIN + HORIZON_NS).step_by(SWEEP_INTERVAL_NS as usize) {
        let i = ((ts - ORIGIN) / ACCEPT_STRIDE_NS) as usize;
        if i < total {
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
                    let crate::source::History::Source(mut tape) =
                        profiles.history_source(SYMBOL, Some(ORIGIN)).ok()?
                    else {
                        return None;
                    };
                    // The tape's first PRINT, not its first FRAME. Protocol 7
                    // opens every parent burst with a quote, so reading one tick
                    // and giving up on a non-trade returned None for the
                    // ordinary case - turning this fallback into a panic at the
                    // exact moment it was needed. Nothing caught it because
                    // `read_last` currently answers at every scenario instant,
                    // so the arm is only reachable when it is load-bearing.
                    loop {
                        if let TickEvent::Trade(trade) = tape.next_tick()? {
                            return Some(trade.price);
                        }
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
                position_id: None,
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
                censored: ORDERS_PER_OFFSET.saturating_sub(filled),
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
    let profiles = std::sync::Arc::new(InstrumentProfiles::from_profiles(vec![
        crate::config::profile_for_symbol("BTCUSDT")
            .expect("BTCUSDT preset must resolve for the fill golden"),
    ]));
    let profiles = crate::source::Rivers::new(
        crate::source::TapeIdentity {
            seeds: RunSeeds::from_run_seed(GOLDEN_RUN_SEED),
            regime: None,
        },
        profiles,
    );
    let golden = Golden {
        schema: 2,
        symbol: SYMBOL,
        data_origin_ns: ORIGIN,
        sweep_interval_ns: SWEEP_INTERVAL_NS,
        horizon_ns: HORIZON_NS,
        orders_per_offset: ORDERS_PER_OFFSET,
        accept_stride_ns: ACCEPT_STRIDE_NS,
        cells: [0.0, shipped_mult()]
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
        let expected_mult = if index < OFFSETS.len() {
            0.0
        } else {
            shipped_mult()
        };
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
