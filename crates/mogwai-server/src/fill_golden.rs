// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! A deterministic, end-to-end fill-timing distribution for the penetration
//! gate, pinned byte-exactly by a committed artifact.
//!
//! It lives in the bin crate because this is the only place that can see
//! `fills::count_penetrations`, `Engine::apply_scans` and `InstrumentProfiles`
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
//! Out of scope, deliberately: the window-restart path. The engine restarts a
//! penetration window only when a fill leaves a remainder, which needs an armed
//! `PartialFillNext`; this harness arms no divergence, so every admitted order
//! fills completely and leaves the book. `penetration_ticks = 3` covers
//! multi-pass penetration ACCUMULATION across sweep boundaries, not a restart.

use std::collections::HashMap;
use std::path::PathBuf;

use mogwai_data::TickEvent;
use mogwai_engine::{Engine, EngineConfig, ScanResult};
use mogwai_protocol::{
    AccountId, ClientMessage, OrderType, ServerMessage, Side, SubmitOrder, TimeInForce,
    default_instruments,
};
use rust_decimal::Decimal;
use serde::Serialize;

use crate::{fills, source::InstrumentProfiles};

const SYMBOL: &str = "BTCUSDT";
const ORIGIN: u64 = 1_700_438_400_000_000_000;
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
    penetration_ticks: u32,
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

fn run_scenario(penetration_ticks: u32, profiles: &InstrumentProfiles) -> Vec<Cell> {
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
        penetration_ticks,
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
            // The same acceptance-time reading `http.rs` gives the real path,
            // so the seed is the shipped one. It is `None` only at the very
            // origin, where no print exists at or before `ts` yet; the first
            // order of each scenario therefore anchors on the tape's first
            // print instead. That is a look-ahead the venue itself would refuse,
            // but here it only anchors the harness's own limit price: the limit
            // is placed `offset_ticks` away from the very reading it is judged
            // against, so no order is ever seeded on arrival either way.
            let market = fills::last_trade_at_or_before(SYMBOL, ts, profiles, ORIGIN)
                .or_else(|| {
                    let mut tape = crate::source::build_history_source(
                        SYMBOL,
                        Some(ORIGIN),
                        profiles,
                        ORIGIN,
                    )?;
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
            let id = format!("g{penetration_ticks}-{i}");
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
                time_in_force: TimeInForce::Gtc,
            };
            let submitted =
                engine.process_with_market(ClientMessage::SubmitOrder(order), ts, Some(market));
            record_fills(&submitted, &meta, &mut samples);
        }
        let scans = engine.pending_scans();
        if scans.is_empty() {
            continue;
        }
        let walk = fills::count_penetrations(SYMBOL, &scans, ts, profiles, ORIGIN)
            .expect("scenario starts on reachable clean tape");
        let results = scans
            .iter()
            .zip(walk.counted)
            .map(|(scan, counted)| ScanResult {
                client_order_id: scan.client_order_id.clone(),
                from_ns: scan.from_ns,
                revision: scan.revision,
                counted,
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
                penetration_ticks,
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
        schema: 1,
        symbol: SYMBOL,
        data_origin_ns: ORIGIN,
        sweep_interval_ns: SWEEP_INTERVAL_NS,
        horizon_ns: HORIZON_NS,
        orders_per_offset: ORDERS_PER_OFFSET,
        accept_stride_ns: ACCEPT_STRIDE_NS,
        cells: [1, 3]
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
        let expected_ticks = if index < OFFSETS.len() { 1 } else { 3 };
        assert_eq!(cell["penetration_ticks"], expected_ticks);
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
            "cell {index} (penetration_ticks={}, offset_ticks={})",
            new_cell["penetration_ticks"], new_cell["offset_ticks"]
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
