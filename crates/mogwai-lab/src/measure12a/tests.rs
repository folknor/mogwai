// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Unit tests for the unified block engine. The heavy corpus-and-walk parity
//! gates live in `crates/mogwai-cli/tests/parity12a.rs` (they need preset and
//! profile resolution from `mogwai-server`, which must never land under this
//! crate); everything provable on a crafted stream is proved here.

use rust_decimal::Decimal;

use super::generated::GeneratedAcc;
use super::*;
use mogwai_protocol::{AggressorSide, QuoteTick, TradeTick};

/// 2026-07-06T22:00Z, the July 7 session open (offset -300).
const OPEN_NS: u64 = 1_783_375_200_000_000_000;
/// 2026-07-07T21:00Z, the July 7 session close.
const CLOSE_NS: u64 = 1_783_458_000_000_000_000;
/// 2026-07-06T23:00Z, the fixture hour.
const H23_NS: u64 = OPEN_NS + 3_600_000_000_000;
const OFFSET: i32 = -300;

fn tick() -> Decimal {
    Decimal::new(25, 2) // 0.25, the MNQ tick
}

fn px(level: i64) -> Decimal {
    Decimal::from(23_000) + tick() * Decimal::from(level)
}

fn quote(ts: u64, level: i64) -> QuoteTick {
    QuoteTick {
        symbol: "MNQ".to_string(),
        bid_px: px(level - 1),
        ask_px: px(level + 1),
        bid_sz: Decimal::ONE,
        ask_sz: Decimal::ONE,
        ts_event: ts,
    }
}

fn trade(ts: u64, level: i64) -> TradeTick {
    TradeTick {
        symbol: "MNQ".to_string(),
        price: px(level),
        size: Decimal::ONE,
        aggressor: AggressorSide::Buyer,
        ts_event: ts,
    }
}

#[expect(clippy::too_many_arguments, reason = "a test fixture constructor")]
fn vt(
    innovation_std: f64,
    sigma2_candidate: f64,
    sigma2_realized: f64,
    base_return: f64,
    realized_return: f64,
    mid: f64,
    sigma_cap_hit: bool,
    feedback_clamp_hit: bool,
) -> mogwai_data::VolTrace {
    mogwai_data::VolTrace {
        innovation_raw: innovation_std * std::f64::consts::SQRT_2,
        innovation_std,
        sigma2_candidate,
        sigma2_realized,
        sigma_cap_hit,
        garch_scale: sigma2_realized.sqrt(),
        base_return_unclipped: base_return,
        base_return,
        feedback_clamp_hit,
        session_vol_mult: 1.0,
        regime_vol_mult: 1.0,
        pre_realized_return: 0.0,
        realized_return,
        realized_clamp_hit: false,
        mid_before: mid,
        mid_after: mid * (1.0 + realized_return),
    }
}

struct Fixture {
    acc: GeneratedAcc,
}

impl Fixture {
    fn new() -> Self {
        Self {
            acc: GeneratedAcc::new(9, OPEN_NS, CLOSE_NS, OFFSET, tick()),
        }
    }

    fn parent(&mut self, ts: u64, level: i64, trace: mogwai_data::VolTrace, trade_levels: &[i64]) {
        self.acc
            .push_quote(&quote(ts, level), Some(trace))
            .expect("quote");
        for (i, &tl) in trade_levels.iter().enumerate() {
            self.acc
                .push_trade(&trade(ts + i as u64, tl))
                .expect("trade");
        }
    }
}

fn quiet() -> mogwai_data::VolTrace {
    vt(0.5, 1.0e-8, 1.0e-8, 1.0e-4, 1.0e-4, 23_000.0, false, false)
}

// -- The arithmetic conventions ---------------------------------------------

#[test]
fn the_log_mid_is_ln_of_the_nano_unit_price_sum_halved() {
    // THE convention, pinned against an explicit hand-computed vector: the
    // price sum is formed in EXACT integer 1e-9 units, halved in f64, and
    // only then logged. Logging a mid expressed in POINTS instead shifts the
    // value by ln(1e9), and every difference-based statistic downstream by a
    // term that does not cancel across segments - the 3.55e-15 divergence
    // class this port exists to kill.
    let seg = SessionSegment {
        session_start_ns: OPEN_NS,
        session_end_ns: CLOSE_NS,
        segment_origin_ns: OPEN_NS,
        segment_end_ns: CLOSE_NS,
        trade_day: 0,
        segment: "overnight",
    };
    let mut acc = SessionAcc::new("2026-07-07".to_string(), &seg, OFFSET);
    // bid 22_999.75, ask 23_000.25 in 1e-9 units.
    let bid = 22_999_750_000_000i64;
    let ask = 23_000_250_000_000i64;
    acc.push_parent(0, OPEN_NS + 1, bid, ask, true)
        .expect("parent");
    let got = acc.segments[0].as_ref().expect("segment").mid_log[0];
    let want = (((bid + ask) as f64) / 2.0).ln();
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "the log-mid must be bit-exact"
    );
    // And it is NOT the log of the mid in points.
    let in_points = (23_000.0f64).ln();
    assert!((got - in_points - (1.0e9f64).ln()).abs() < 1e-9);
    // The half-tick quote-mid divides the SUM by the FULL tick.
    assert_eq!(
        acc.quote_min[&((OPEN_NS + 1) / NS_PER_MIN)],
        (184_000, 184_000)
    );
}

#[test]
fn off_grid_prices_refuse_rather_than_flooring() {
    let seg = SessionSegment {
        session_start_ns: OPEN_NS,
        session_end_ns: CLOSE_NS,
        segment_origin_ns: OPEN_NS,
        segment_end_ns: CLOSE_NS,
        trade_day: 0,
        segment: "overnight",
    };
    let mut acc = SessionAcc::new("2026-07-07".to_string(), &seg, OFFSET);
    // A one-nano-unit-off quote sum: the Python floor-divides and silently
    // loses the remainder; the port refuses.
    let err = acc.push_parent(0, OPEN_NS + 1, 22_999_750_000_000, 23_000_250_000_001, true);
    assert!(
        matches!(err, Err(crate::error::LabError::Refusal(_))),
        "{err:?}"
    );
    // A trade range off the tick grid refuses out of block 1.
    let mut acc2 = SessionAcc::new("2026-07-07".to_string(), &seg, OFFSET);
    acc2.push_print(OPEN_NS + 1, 23_000_000_000_000);
    acc2.push_print(OPEN_NS + 2, 23_000_000_000_001);
    assert!(matches!(
        acc2.close(Scope::Observed),
        Err(crate::error::LabError::Refusal(_))
    ));
}

#[test]
fn the_window_schedule_excludes_hour_crossing_by_endpoint_attribution() {
    // A window ending EXACTLY on the hour boundary crosses and is excluded -
    // the endpoint-hour rule, matching the fixed-horizon convention.
    let origin = 3_600_000_000_000u64 - 120_000_000_000; // 00:58:00
    let end = origin + 300_000_000_000;
    let sched = window_schedule(origin, end, 60_000_000_000);
    let hours: Vec<Option<u64>> = sched.iter().map(|&(_, _, h)| h).collect();
    // 00:58->00:59 in hour 0; 00:59->01:00 ends exactly ON the boundary and
    // therefore CROSSES (excluded); the rest attribute to hour 1.
    assert_eq!(hours, vec![Some(0), None, Some(1), Some(1), Some(1)]);
    // Only windows STRICTLY contained in the segment are scheduled.
    assert_eq!(window_schedule(0, 90_000_000_000, 60_000_000_000).len(), 1);
}

#[test]
fn wall_boundaries_follow_the_protocol_11_pending_rule() {
    // Establishment (the first boundary with an as-of emits nothing),
    // segment-local as-of, and the PENDING rule: an endpoint exactly ON the
    // boundary updates first, so it is the as-of of its own boundary.
    let mut seg = SegmentAcc::new(0, 0, 10_000_000_000);
    seg.mid_ts = vec![1_000_000_000, 2_000_000_000, 4_000_000_000];
    seg.mid_log = vec![1.0, 2.0, 4.0];
    let rows = wall_boundaries(&seg, 2_000_000_000);
    let summary: Vec<(u64, Option<f64>, bool, Option<f64>)> =
        rows.iter().map(|&(b, a, e, _h, r)| (b, a, e, r)).collect();
    assert_eq!(
        summary,
        vec![
            // b = 2 s: the endpoint AT 2 s is included (ts <= b) - the
            // pending rule - so the as-of is 2.0, not 1.0. This boundary
            // ESTABLISHES the chain and emits nothing.
            (2_000_000_000, Some(2.0), false, None),
            (4_000_000_000, Some(4.0), true, Some(2.0)),
            (6_000_000_000, Some(4.0), true, Some(0.0)),
            (8_000_000_000, Some(4.0), true, Some(0.0)),
        ]
    );
    // With no endpoint before the first boundary the chain never establishes.
    let mut bare = SegmentAcc::new(0, 0, 6_000_000_000);
    bare.mid_ts = vec![5_000_000_000];
    bare.mid_log = vec![1.0];
    let rows = wall_boundaries(&bare, 2_000_000_000);
    assert_eq!(rows.iter().filter(|r| r.2).count(), 0);
    assert_eq!(rows[0].1, None);
}

#[test]
fn block4_omits_a_nonpositive_scale_with_one_refusal_per_session_hour() {
    // A flat mid drives the trailing one-max-trimmed scale to zero once the
    // 1000-return history fills: the residuals are OMITTED (Amendment F) and
    // recorded, never a hard error, and the hour gets exactly ONE record.
    let mut fx = Fixture::new();
    let flat = vt(0.0, 1.0e-8, 1.0e-8, 0.0, 0.0, 23_000.0, false, false);
    for k in 0..1500u64 {
        fx.parent(H23_NS + k * 200_000_000, 0, flat, &[0]);
    }
    let value = fx.acc.finish().expect("finish");
    let s = &value["per_session"][0];
    assert_eq!(s["block4"]["23"]["residual_count"], 0);
    assert!(
        s["block4"]["23"]["warmup_excluded"]
            .as_u64()
            .expect("count")
            >= 1000
    );
    let refusals = s["refusals"].as_array().expect("refusals");
    assert_eq!(refusals.len(), 1);
    assert!(
        refusals[0]["cell"]
            .as_str()
            .expect("cell")
            .contains("standardizer")
    );
    // The generated scope carries the SEED; the observed scope does not.
    assert_eq!(refusals[0]["scope"], "seed 9 session 2026-07-07");
}

#[test]
fn the_refusal_scope_distinguishes_the_two_sides() {
    assert_eq!(
        Scope::Observed.session_refusal_scope("2026-07-07"),
        "observed session 2026-07-07"
    );
    assert_eq!(
        Scope::Generated { seed: 3 }.session_refusal_scope("2026-07-07"),
        "seed 3 session 2026-07-07"
    );
    assert!(Scope::Observed.wants_permutations());
    assert!(!Scope::Generated { seed: 3 }.wants_permutations());
}

// -- The generated side, end to end -----------------------------------------

/// The Brick G gate test, carried over from the `mogwai-cli` twin: the
/// accumulator's serialized blocks against an INDEPENDENT recompute over the
/// same crafted parent stream.
#[test]
fn measure12a_matches_independent_recompute() {
    let mut fx = Fixture::new();
    let minute = |m: u64| H23_NS + m * NS_PER_MIN;
    // Minute 0: the extreme - a calm opener, then a 12-sigma parent with two
    // clamp flags printing an 80-tick range.
    fx.parent(minute(0) + 10_000_000_000, 0, quiet(), &[0]);
    fx.parent(
        minute(0) + 20_000_000_000,
        40,
        vt(12.0, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, true, true),
        &[40, -40],
    );
    // Minutes 5..45: forty quiet one-parent minutes (the control candidate
    // pool past the top-32 exclusion).
    for m in 5..45 {
        fx.parent(minute(m) + 30_000_000_000, 0, quiet(), &[0]);
    }
    let value = fx.acc.finish().expect("finish");

    let sessions = value["per_session"].as_array().expect("array");
    assert_eq!(sessions.len(), 1);
    let s = &sessions[0];
    assert_eq!(s["session_date"], "2026-07-07");
    // Spec 5.1 is observed-side only: the generated side emits an EMPTY array.
    assert_eq!(s["permutations"], serde_json::json!([]));
    assert_eq!(s["segments"][0]["open_ns"].as_u64(), Some(OPEN_NS));
    assert_eq!(s["segments"][0]["segment_index"], 0);
    assert_eq!(s["segments"][1]["segment_index"], 1);

    // Block 1, independently recomputed: 41 populated minutes, one with n=2.
    let hist = s["block1_hist"].as_array().expect("hist");
    let total_minutes: u64 = hist
        .iter()
        .map(|r| r["count"].as_u64().expect("count"))
        .sum();
    assert_eq!(total_minutes, 41);
    let extreme_row = hist
        .iter()
        .find(|r| r["n"] == 2)
        .expect("the two-parent extreme minute");
    assert_eq!(extreme_row["trade_range_ticks"], 80);
    assert_eq!(extreme_row["quote_range_half_ticks"], 80);
    assert_eq!(extreme_row["hour"], 23);
    assert_eq!(extreme_row["since_open_bin"], "1800+");
    assert_eq!(extreme_row["until_close_bin"], "1800+");
    let quiet_row = hist.iter().find(|r| r["n"] == 1).expect("quiet minutes");
    assert_eq!(quiet_row["count"], 40);
    assert_eq!(quiet_row["trade_range_ticks"], 0);
    assert_eq!(quiet_row["quote_range_half_ticks"], 0);

    // Block 2 hour 23 w=60: the hour holds 60 origin-aligned windows minus
    // the hour-crossing last one = 59 scheduled; 42 parents fall in 41
    // distinct windows (the extreme minute's two parents share one).
    let b2 = &s["block2"]["23"]["60"];
    assert_eq!(b2["scheduled_windows"], 59);
    assert_eq!(b2["zero_windows"], 59 - 41);
    assert_eq!(b2["count_hist"]["1"], 40);
    assert_eq!(b2["count_hist"]["2"], 1);
    assert_eq!(b2["count_p99"], 2);

    // Block 3 hour 23 h=60, independently recomputed with a naive as-of walk
    // over the same (ts, log-mid) series built the canonical way.
    let logmid = |level: i64| -> f64 {
        let sum_nanos = (px(level - 1) + px(level + 1)) * Decimal::new(1_000_000_000, 0);
        (f64::try_from(sum_nanos).expect("exact") / 2.0).ln()
    };
    let mids: Vec<(u64, f64)> = {
        let mut v = vec![
            (minute(0) + 10_000_000_000, logmid(0)),
            (minute(0) + 20_000_000_000, logmid(40)),
        ];
        for m in 5..45 {
            v.push((minute(m) + 30_000_000_000, logmid(0)));
        }
        v
    };
    let naive = |h_ns: u64| -> (u64, f64) {
        let mut prev: Option<f64> = None;
        let mut count = 0u64;
        let mut sum_abs = 0.0;
        let mut max_abs = 0.0f64;
        let mut b = OPEN_NS + h_ns;
        while b < OPEN_NS + 80_100_000_000_000 {
            let asof = mids.iter().rev().find(|&&(ts, _)| ts <= b).map(|&(_, m)| m);
            if let Some(cur) = asof {
                if let Some(p) = prev
                    && (b / NS_PER_HOUR) % 24 == ((b - h_ns) / NS_PER_HOUR) % 24
                    && (b / NS_PER_HOUR) % 24 == 23
                {
                    let a = (cur - p).abs();
                    count += 1;
                    sum_abs += a;
                    if a > max_abs {
                        max_abs = a;
                    }
                }
                prev = Some(cur);
            }
            b += h_ns;
        }
        (count, (sum_abs - max_abs) / (count - 1) as f64)
    };
    let (n60, robust60) = naive(60_000_000_000);
    let b3 = &s["block3"]["cells"]["23"]["60"];
    assert_eq!(b3["return_count"].as_u64(), Some(n60));
    let got = b3["robust_scale"].as_f64().expect("robust");
    assert!((got - robust60).abs() <= 1e-15_f64.max(robust60 * 1e-12));

    // Block 4: 41 adjacent-mid returns, every one below the 1000-return
    // history floor - all warmup, no residuals.
    assert_eq!(s["block4"]["23"]["warmup_excluded"], 41);
    assert_eq!(s["block4"]["23"]["residual_count"], 0);
    assert_eq!(s["block4"]["all"]["warmup_excluded"], 41);
    assert_eq!(s["refusals"], serde_json::json!([]));

    // Forensic: the extreme minute, its trace-grounded fields, and the
    // control chosen by the frozen tie-break.
    let records = value["forensic"]["records"].as_array().expect("records");
    assert_eq!(records.len(), 2);
    let extreme = &records[0];
    assert_eq!(extreme["kind"], "extreme_range");
    assert_eq!(extreme["minute_start_ns"].as_u64(), Some(minute(0)));
    assert_eq!(extreme["parent_count"], 2);
    assert_eq!(extreme["trade_count"], 3);
    assert_eq!(extreme["traced_parents"], 2);
    assert_eq!(extreme["largest_innovation_std"], 12.0);
    assert_eq!(extreme["innovation_exceed_8"], 1);
    assert_eq!(extreme["innovation_exceed_16"], 0);
    assert_eq!(extreme["initiation"], true);
    // Two simultaneous clamp flags on one parent contribute two.
    assert_eq!(extreme["clamp_hits"], 2);
    assert_eq!(extreme["max_signed_run"], 2);
    assert!(extreme["sigma_start"].is_null());
    assert!(extreme["sigma_escalation"].is_null());
    let expected_share = generated::ARCH_12A * 3.0e-4_f64.powi(2) / 1.0e-8;
    let got_share = extreme["arch_share_next"].as_f64().expect("share");
    assert!((got_share - expected_share).abs() < 1e-12);
    let refusals = value["forensic"]["refusals"].as_array().expect("refusals");
    assert_eq!(refusals.len(), 1);
    assert!(
        refusals[0]["cell"]
            .as_str()
            .expect("cell")
            .contains("sigma_start"),
        "{refusals:?}"
    );
    // Control: same segment-hour, at or below the median range, outside the
    // top-32 by range; the tie among the nine eligible quiet minutes resolves
    // by the frozen tuple_mix rank.
    let control = &records[1];
    assert_eq!(control["kind"], "control");
    assert_eq!(
        control["matched_extreme_minute_start"].as_u64(),
        Some(minute(0))
    );
    let expected_control = (36..45)
        .map(minute)
        .min_by_key(|&m| {
            let base = crate::subcontract::CONTROL_TIE_BASE_SEED as u64;
            (crate::kernel::tuple_mix(base, &[9, minute(0), m]), m)
        })
        .expect("nine candidates");
    assert_eq!(control["minute_start_ns"].as_u64(), Some(expected_control));
}

#[test]
fn initiation_survives_a_minute_straddling_burst() {
    // A burst whose later children cross the minute boundary must not resolve
    // the minute's initiation before its own parent is attributed back to the
    // first-child minute: minute closure is PARENT-driven, not trade-driven.
    let mut fx = Fixture::new();
    let minute = |m: u64| H23_NS + m * NS_PER_MIN;
    fx.parent(minute(0) + 10_000_000_000, 0, quiet(), &[0]);
    fx.acc
        .push_quote(
            &quote(minute(0) + 58_000_000_000, 40),
            Some(vt(
                12.0, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, false, false,
            )),
        )
        .expect("quote");
    fx.acc
        .push_trade(&trade(minute(0) + 58_000_000_000, 40))
        .expect("trade");
    fx.acc
        .push_trade(&trade(minute(1) + 2_000_000_000, 40))
        .expect("straddling trade");
    for m in 5..45 {
        fx.parent(minute(m) + 30_000_000_000, 0, quiet(), &[0]);
    }
    let value = fx.acc.finish().expect("finish");
    let extreme = &value["forensic"]["records"][0];
    assert_eq!(extreme["kind"], "extreme_range");
    assert_eq!(extreme["minute_start_ns"].as_u64(), Some(minute(0)));
    assert_eq!(
        extreme["initiation"], true,
        "the straddling burst's own breakpoint must decide initiation"
    );
}

#[test]
fn a_shared_control_refuses_once_and_a_new_largest_clears_the_share() {
    // Both extremes select the SAME sole eligible control: the control emits
    // one record per extreme, but a refused cell on it carries exactly ONE
    // logical RefusalRec.
    let mut fx = Fixture::new();
    let minute = |m: u64| H23_NS + m * NS_PER_MIN;
    for m in 0..29 {
        fx.parent(minute(m) + 30_000_000_000, 0, quiet(), &[0]);
    }
    fx.parent(
        minute(31) + 10_000_000_000,
        0,
        vt(9.0, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, false, false),
        &[50, -50],
    );
    for k in 0..3 {
        fx.parent(
            minute(31) + 20_000_000_000 + k * 5_000_000_000,
            0,
            quiet(),
            &[0],
        );
    }
    fx.parent(minute(32) + 10_000_000_000, 0, quiet(), &[45, -45]);
    fx.parent(
        minute(33) + 10_000_000_000,
        0,
        vt(0.5, 1.0e-8, 0.0, 0.0, 0.0, 23_000.0, false, false),
        &[0, 1],
    );
    fx.parent(minute(36) + 10_000_000_000, 0, quiet(), &[0]);
    let value = fx.acc.finish().expect("finish");
    let records = value["forensic"]["records"].as_array().expect("records");
    assert_eq!(records.len(), 4, "{records:?}");
    assert_eq!(records[0]["kind"], "extreme_range");
    assert_eq!(records[0]["minute_start_ns"].as_u64(), Some(minute(31)));
    assert_eq!(records[1]["kind"], "control");
    assert_eq!(
        records[1]["matched_extreme_minute_start"].as_u64(),
        Some(minute(31))
    );
    assert_eq!(records[1]["minute_start_ns"].as_u64(), Some(minute(36)));
    assert_eq!(records[2]["kind"], "extreme_sqrt");
    assert_eq!(records[2]["minute_start_ns"].as_u64(), Some(minute(32)));
    assert_eq!(records[3]["kind"], "control");
    assert_eq!(
        records[3]["matched_extreme_minute_start"].as_u64(),
        Some(minute(32))
    );
    assert_eq!(records[3]["minute_start_ns"].as_u64(), Some(minute(36)));
    assert!(records[1]["sigma_escalation"].is_null());
    assert!(records[3]["sigma_escalation"].is_null());
    let refusals = value["forensic"]["refusals"].as_array().expect("refusals");
    let escalations: Vec<_> = refusals
        .iter()
        .filter(|r| {
            r["cell"]
                .as_str()
                .expect("cell")
                .contains("sigma_escalation")
        })
        .collect();
    assert_eq!(escalations.len(), 1, "{refusals:?}");
}

#[test]
fn a_new_largest_innovation_nulls_the_stale_share() {
    // Within one minute: parent A (inn 9) resolves a share when parent B
    // arrives; B (inn 12) becomes the largest and never gains a successor, so
    // arch_share_next must be NULL while the minute max keeps A's share.
    let mut fx = Fixture::new();
    let minute = |m: u64| H23_NS + m * NS_PER_MIN;
    let big = |inn: f64| vt(inn, 9.0e-8, 9.0e-8, 3.0e-4, 3.0e-4, 23_000.0, false, false);
    fx.parent(minute(0) + 10_000_000_000, 40, big(9.0), &[40, -40]);
    fx.parent(minute(0) + 20_000_000_000, 0, big(12.0), &[0]);
    let value = fx.acc.finish().expect("finish");
    let extreme = &value["forensic"]["records"][0];
    assert_eq!(extreme["largest_innovation_std"], 12.0);
    assert!(
        extreme["arch_share_next"].is_null(),
        "the stale share of the superseded largest leaked: {extreme}"
    );
    assert!(extreme["arch_share_minute_max"].as_f64().is_some());
}

#[test]
fn only_complete_sessions_are_emitted_on_the_generated_side() {
    // A session whose close lies past the measured window end never emits -
    // the Q1 all-session rule. The engine still accumulates it.
    let mut acc = GeneratedAcc::new(1, OPEN_NS, CLOSE_NS - NS_PER_MIN, OFFSET, tick());
    acc.push_quote(&quote(H23_NS, 0), Some(quiet()))
        .expect("quote");
    acc.push_trade(&trade(H23_NS, 0)).expect("trade");
    let value = acc.finish().expect("finish");
    assert_eq!(value["per_session"].as_array().expect("array").len(), 0);
}

// -- The permutation records (observed side only) ---------------------------

#[test]
fn permutations_preserve_the_variant_invariants_and_are_seed_stable() {
    // A crafted single-hour cell: the sign variant must leave every
    // magnitude in place, the magnitude variant every sign, and both must
    // reproduce bit-for-bit across two runs of the same session.
    let seg = SessionSegment {
        session_start_ns: OPEN_NS,
        session_end_ns: CLOSE_NS,
        segment_origin_ns: OPEN_NS,
        segment_end_ns: CLOSE_NS,
        trade_day: 0,
        segment: "overnight",
    };
    let build = || {
        let mut acc = SessionAcc::new("2026-07-07".to_string(), &seg, OFFSET);
        for k in 0..400u64 {
            // Alternating up/down moves so signs and magnitudes both vary.
            let level = i64::from(k % 7 == 0) * 3 - i64::from(k % 5 == 0);
            let bid = 23_000_000_000_000i64 + level * 250_000_000;
            acc.push_parent(0, H23_NS + k * 1_000_000_000, bid, bid + 500_000_000, true)
                .expect("parent");
        }
        acc
    };
    let a = build().close(Scope::Observed).expect("close");
    let b = build().close(Scope::Observed).expect("close");
    assert_eq!(
        crate::kernel::typed_canon(&a),
        crate::kernel::typed_canon(&b),
        "the permutation stream must be a pure function of the session"
    );
    let perms = a["permutations"].as_array().expect("permutations");
    assert!(!perms.is_empty());
    // Both variants, 16 replicates each, per (segment, hour) cell.
    let variants: std::collections::BTreeSet<&str> =
        perms.iter().filter_map(|p| p["variant"].as_str()).collect();
    assert_eq!(variants, ["magnitude", "sign"].into_iter().collect());
    let reps: std::collections::BTreeSet<u64> = perms
        .iter()
        .filter_map(|p| p["replicate"].as_u64())
        .collect();
    assert_eq!(reps.len(), 16);
    assert_eq!(perms.len() % 32, 0);
    // The generated side emits none of this.
    let g = build().close(Scope::Generated { seed: 1 }).expect("close");
    assert_eq!(g["permutations"], serde_json::json!([]));
    // ... and everything else is identical: 5.1 is the ONLY block that
    // differs between the two scopes on the same input.
    for key in ["block1_hist", "block2", "block3", "block4", "segments"] {
        assert_eq!(
            crate::kernel::typed_canon(&a[key]),
            crate::kernel::typed_canon(&g[key]),
            "{key}"
        );
    }
}

// -- Cross-implementation agreement ------------------------------------------

#[test]
fn the_ceil_n_over_2_median_backs_the_control_group_ranges() {
    // The forensic control's per (segment, hour) median takes the
    // ceil(n/2)-th order statistic, matching median_or_none - an even-length
    // group takes the LOWER middle.
    for (values, want) in [
        (vec![0i64, 1], 0i64),
        (vec![0, 1, 2], 1),
        (vec![0, 0, 5, 9], 0),
    ] {
        let rank = values.len().div_ceil(2);
        assert_eq!(values[rank - 1], want);
        let as_f: Vec<Option<f64>> = values.iter().map(|v| Some(*v as f64)).collect();
        assert_eq!(crate::kernel::median_or_none(&as_f), Some(want as f64));
    }
}

#[test]
fn close_reduced_agrees_with_close_on_block1_and_block2() {
    // Protocol 12b Stage A drops blocks 3 and 4 to buy budget. It may buy
    // nothing else: on the same stream the reduced close must emit the two
    // blocks the screen reads exactly as the full close does, or the
    // projection has quietly grown a second block-1 or block-2.
    let seg = SessionSegment {
        session_start_ns: OPEN_NS,
        session_end_ns: CLOSE_NS,
        segment_origin_ns: OPEN_NS,
        segment_end_ns: CLOSE_NS,
        trade_day: 0,
        segment: "overnight",
    };
    let fill = |acc: &mut SessionAcc| {
        for i in 0..400_u64 {
            let ts = H23_NS + i * 137_000_000;
            acc.push_print(ts, 0);
            if i.is_multiple_of(7) {
                acc.push_parent(0, ts, 0, 0, false).expect("parent");
            }
        }
    };
    let mut full = SessionAcc::new("2026-07-07".to_string(), &seg, OFFSET);
    let mut reduced = SessionAcc::new("2026-07-07".to_string(), &seg, OFFSET);
    fill(&mut full);
    fill(&mut reduced);
    let full = full
        .close(Scope::Generated { seed: 1 })
        .expect("full close");
    let reduced = reduced
        .close_reduced(Scope::Generated { seed: 1 })
        .expect("reduced close");
    for key in ["session_date", "block1_hist", "block2"] {
        assert_eq!(full[key], reduced[key], "{key}");
    }
    assert!(reduced.get("block3").is_none(), "block 3 is not computed");
    assert!(reduced.get("block4").is_none(), "block 4 is not computed");
}

#[test]
fn screen_accumulator_matches_the_generic_reduced_surface_exactly() {
    let seg = SessionSegment {
        session_start_ns: OPEN_NS,
        session_end_ns: CLOSE_NS,
        segment_origin_ns: OPEN_NS,
        segment_end_ns: CLOSE_NS,
        trade_day: 0,
        segment: "overnight",
    };
    let date = "2026-07-07".to_string();
    let mut generic = SessionAcc::new(date.clone(), &seg, OFFSET);
    let mut screen = ScreenSessionAcc::new(date, &seg, OFFSET);
    let parents = [
        H23_NS + 100_000_000,
        H23_NS + 800_000_000,
        H23_NS + 2_100_000_000,
        H23_NS + 61_000_000_000,
        CLOSE_NS - 20 * NS_PER_MIN + 500_000_000,
        CLOSE_NS - 19 * NS_PER_MIN + 500_000_000,
    ];
    for ts in parents {
        let resolved = session_segment_at(ts, OFFSET).expect("a parent inside a segment");
        let index = u8::from(resolved.segment_origin_ns != resolved.session_start_ns);
        generic
            .push_parent(index, ts, 0, 0, false)
            .expect("generic parent");
        screen
            .push_parent(index, ts)
            .expect("screen parent");
        generic.push_print(ts, 0);
        screen.push_print(ts);
    }
    for ts in [H23_NS + 3 * NS_PER_MIN, CLOSE_NS - 18 * NS_PER_MIN] {
        generic.push_print(ts, 0);
        screen.push_print(ts);
    }
    let generic = generic
        .close_reduced(Scope::Generated { seed: 1 })
        .expect("generic close");
    let screen = screen.close().expect("screen close");
    assert_eq!(screen, generic);
}
