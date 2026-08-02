// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use mogwai_protocol::{AggressorSide, MarketRegime, decimal_to_f64};

use crate::{TickEvent, TickSource};

use super::checkpoint::MAX_CHECKPOINTS;
use super::consts::{GARCH_SIGMA_CAP, MAX_ABS_RETURN, MAX_SESSION_GAP_NS, NS_PER_HOUR};
use super::numeric::WEIBULL_MEAN_SHAPE_060;
use super::session::{utc_hour, utc_hour_dow};
use super::*;
use rust_decimal::Decimal;
use std::collections::HashSet;

const DRAW: usize = 2_000_000;
const SESSION_DRAW: usize = 5_000_000;
// Unlike the return/abs-return/dispersion targets, duration_acf has no
// committed cross-pair min/median/max band in the fingerprint - only the
// anchor lag vector (duration_acf_anchor). With no band to inherit, 0.14 is
// a principled absolute choice rather than a fitted one: the seeded duration
// ACF lands within a few hundredths of the anchor at lags 1 and 5, so 0.14
// gives margin for seed-to-seed sampling wobble while staying tight enough
// that a flattened ACF (the failure mode the after-the-recursion session
// envelope is designed to prevent) - which collapses these lags toward zero,
// an order of magnitude past 0.14 from the anchor - is still caught.
const DURATION_ACF_ABS_TOL: f64 = 0.14;
// Dwell slack. All three bounds are one-sided: the failure mode this gate
// exists to catch is silence, and the lower side is already policed by the
// duration ACF band and (weakly) the dispersion floor. The slack covers seed
// wobble on a 2M-tick draw plus the residual population mismatch - the draw
// runs a few months of simulated time under the session envelope, while the
// era-windowed anchor spans years. The p999 bound is additionally scaled by
// the cadence ratio (the tape's declared mean gap over the windowed anchor
// mean) because the default profile is seeded from the cross-pair median
// cadence and so runs ~2.5x slower than the anchor by construction - the
// handicap is made explicit rather than absorbed into the slack. The two
// empty-hour statistics take additive slack instead: they are fractions and
// counts of whole hours whose anchor values are near zero, so a multiplicative
// bound would be meaningless.
// MEAN_GAP_REL_TOL is the only two-sided band here. It guards the tape's
// declared cadence, which every tick-count budget outside this crate prices
// (history seek caps, checkpoint spacing, the server's backfill horizon), and
// it is what keeps ACD_RELAX_MEAN_CAL honest.
const DWELL_P999_SLACK: f64 = 2.0;
const EMPTY_HOUR_FRAC_SLACK: f64 = 0.01;
const EMPTY_HOUR_RUN_SLACK_H: f64 = 2.0;
const MEAN_GAP_REL_TOL: f64 = 0.10;
const INTENSITY_SHARE_ABS_TOL: f64 = 0.006;
const DOW_SHARE_ABS_TOL: f64 = 0.01;
const SESSION_START_TS: u64 = 1_700_438_400_000_000_000;

#[test]
fn fingerprint_parses() {
    let fp = Fingerprint::from_repo_json();
    assert_eq!(fp.session_profile.intensity_hour.len(), 24);
    assert_eq!(fp.session_profile.dow_weight.len(), 7);
    assert_eq!(fp.golden_targets.abs_return_acf_anchor.len(), 50);
    assert_eq!(fp.golden_targets.dwell.era_start_ts, 1_546_300_800);
    assert_eq!(fp.scalar_ranges.price_decimals.min, 1.0);
    assert_eq!(fp.scalar_ranges.price_decimals.max, 7.0);
}

#[test]
fn scalars_validate() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::from_fingerprint_medians("ETHUSD", &fp);
    assert!(scalars.validate(&fp).is_ok());
    let mut bad = scalars.clone();
    bad.mean_duration_s = fp.scalar_ranges.mean_duration_s.max + 1.0;
    assert_eq!(
        bad.validate(&fp),
        Err(ScalarError {
            field: "mean_duration_s"
        })
    );
}

#[test]
fn session_profile_rejects_non_normalized_curves() {
    let fp = Fingerprint::from_repo_json();
    // The committed fingerprint profile normalizes exactly (sums 1/1/24)
    // and must keep passing.
    assert!(fp.session_profile.validate().is_ok());

    // A plausible "no modulation" attempt: all-ones intensity sums to 24, a
    // 24x arrival multiplier that silently compresses the validated
    // mean_duration into milliseconds. Rejected as a whole-array violation.
    let mut all_ones = fp.session_profile.clone();
    all_ones.intensity_hour = [1.0; 24];
    assert_eq!(
        all_ones.validate(),
        Err(SessionProfileError {
            field: "intensity_hour",
            index: usize::MAX,
        })
    );

    // All-ones dow (sum 7) is the same pathology on the day axis.
    let mut all_ones_dow = fp.session_profile.clone();
    all_ones_dow.dow_weight = [1.0; 7];
    assert_eq!(
        all_ones_dow.validate(),
        Err(SessionProfileError {
            field: "dow_weight",
            index: usize::MAX,
        })
    );

    // An un-normalized vol curve silently rescales overall volatility even
    // though vol_scalar validated. vol_hour is held to a symmetric band, so
    // both an inflated (sum 48) and a deflated (sum 12) curve are rejected.
    let mut hot_vol = fp.session_profile.clone();
    hot_vol.vol_hour = [2.0; 24];
    assert_eq!(
        hot_vol.validate(),
        Err(SessionProfileError {
            field: "vol_hour",
            index: usize::MAX,
        })
    );
    let mut cold_vol = fp.session_profile.clone();
    cold_vol.vol_hour = [0.5; 24];
    assert_eq!(
        cold_vol.validate(),
        Err(SessionProfileError {
            field: "vol_hour",
            index: usize::MAX,
        })
    );

    // A legitimate closed-session profile sums BELOW 1.0 on intensity (a
    // closed hour carries ~0 mass) and must stay legal - the near-zero
    // mechanism and the fully-closed cap machinery both depend on the
    // one-sided bound admitting sub-1 intensity/dow sums.
    let mut closed = fp.session_profile.clone();
    closed.intensity_hour = [1e-9; 24];
    assert!(
        closed.validate().is_ok(),
        "a closed-session profile with sub-1 intensity sum must validate"
    );
}

#[test]
fn scalars_reject_coverage_holes() {
    let fp = Fingerprint::from_repo_json();
    let good = GeneratorScalars::xbtusd_anchor(&fp);
    assert!(good.validate(&fp).is_ok());

    // (d) an omitted symbol serde-defaults to "" and cross-contaminates
    // every symbol-keyed consumer.
    let mut no_symbol = good.clone();
    no_symbol.symbol = String::new();
    assert_eq!(
        no_symbol.validate(&fp),
        Err(ScalarError { field: "symbol" })
    );

    // (a) modal_tick 1e-7 is in range and price_decimals 1 is in range, but
    // together round_dp(1) silently coarsens the grid to 0.1.
    let mut fine_tick = good.clone();
    fine_tick.modal_tick = Decimal::new(1, 7);
    fine_tick.price_decimals = 1;
    assert_eq!(
        fine_tick.validate(&fp),
        Err(ScalarError {
            field: "modal_tick"
        })
    );

    // (b) start_price outside the [modal_tick, MID_CEILING] clamp band: a
    // value above the ceiling instantly collapses the mid, one below a tick
    // instantly jumps up.
    let mut high_start = good.clone();
    high_start.start_price = Decimal::from(5_000_000_000_i64);
    assert_eq!(
        high_start.validate(&fp),
        Err(ScalarError {
            field: "start_price"
        })
    );
    let mut low_start = good.clone();
    low_start.start_price = good.modal_tick / Decimal::from(2);
    assert_eq!(
        low_start.validate(&fp),
        Err(ScalarError {
            field: "start_price"
        })
    );

    // (c) vol_scalar above the sigma cap is silently pinned at the cap on
    // the first tick and does nothing in the base regime.
    let mut hot_vol = good.clone();
    hot_vol.vol_scalar = GARCH_SIGMA_CAP * 10.0;
    assert_eq!(
        hot_vol.validate(&fp),
        Err(ScalarError {
            field: "vol_scalar"
        })
    );

    // The fingerprint-median construction (a fine 4-decimal grid) still
    // passes: its modal_tick scale (4) equals price_decimals (4).
    let medians = GeneratorScalars::from_fingerprint_medians("ETHUSD", &fp);
    assert!(medians.validate(&fp).is_ok());
}

#[test]
fn try_new_accepts_valid_input_and_surfaces_bad_input() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    // Valid input builds a source (the fingerprint-anchored scalars pass).
    assert!(GeneratedSource::try_new(scalars.clone(), 42, 0, &fp, None).is_ok());

    // Bad scalars come back as an Err instead of panicking the way the
    // infallible `new` would. `GeneratedSource` is not `PartialEq`, so drop
    // the Ok half with `.err()` before comparing the error.
    let mut bad_scalars = scalars.clone();
    bad_scalars.mean_duration_s = fp.scalar_ranges.mean_duration_s.max + 1.0;
    assert_eq!(
        GeneratedSource::try_new(bad_scalars, 42, 0, &fp, None).err(),
        Some(GeneratedSourceError::Scalar(ScalarError {
            field: "mean_duration_s"
        }))
    );

    // A bad session profile on the explicit-session path is surfaced too: an
    // all-ones intensity curve sums to 24 and is rejected as a whole-array
    // normalization violation.
    let mut bad_session = fp.session_profile.clone();
    bad_session.intensity_hour = [1.0; 24];
    assert_eq!(
        GeneratedSource::try_new_with_session_profile(scalars, 42, 0, &fp, &bad_session, None)
            .err(),
        Some(GeneratedSourceError::Session(SessionProfileError {
            field: "intensity_hour",
            index: usize::MAX,
        }))
    );
}

#[test]
fn determinism() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut a = GeneratedSource::new(scalars.clone(), 42, 1_000, &fp, None);
    let mut b = GeneratedSource::new(scalars.clone(), 42, 1_000, &fp, None);
    let mut c = GeneratedSource::new(scalars, 43, 1_000, &fp, None);
    for _ in 0..1_000 {
        let ta = a.next_tick();
        let tb = b.next_tick();
        let tc = c.next_tick();
        assert_eq!(format!("{ta:?}"), format!("{tb:?}"));
        assert_ne!(format!("{ta:?}"), format!("{tc:?}"));
    }
}

#[test]
fn monotonic_clock() {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 42, 0, &fp, None);
    let mut prior = 0;
    for _ in 0..10_000 {
        let tick = src.next_tick().expect("unbounded generated source");
        assert!(tick.ts_event() > prior);
        prior = tick.ts_event();
    }
}

#[test]
fn on_grid_prices_and_native_aggressor() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    for _ in 0..10_000 {
        let TickEvent::Trade(trade) = src.next_tick().expect("trade") else {
            unreachable!("generated source emits trades")
        };
        assert_eq!((trade.price / scalars.modal_tick).fract(), Decimal::ZERO);
        assert!(matches!(
            trade.aggressor,
            AggressorSide::Buyer | AggressorSide::Seller
        ));
    }
}

#[test]
fn weibull_mean_matches_known_constant() {
    // Pin the construction-time constant the ACD clock divides by. The true
    // value of gamma(1 + 1/0.6) = gamma(2.6666...) is ~1.5045754867; the
    // hard-coded WEIBULL_MEAN_SHAPE_060 literal is the f64 the former Lanczos
    // series produced for that argument (~1.5e-8 from the true value, the
    // series' inherent approximation error). A 1e-7 tolerance catches a typo in
    // the literal without asserting an exact f64 bit pattern (the byte-exact
    // guard is the golden test, clean_regime_is_byte_identical).
    assert!(
        (WEIBULL_MEAN_SHAPE_060 - 1.504_575_486_7).abs() < 1e-7,
        "WEIBULL_MEAN_SHAPE_060={WEIBULL_MEAN_SHAPE_060}"
    );
}

#[test]
fn fine_grid_prices_stay_on_grid() {
    // C.4 coverage: the realism/anchor on-grid checks only run xbtusd_anchor
    // (tick 0.1, 1 decimal). The from_fingerprint_medians path pins the
    // fingerprint medians - modal_tick 0.0001, price_decimals 4 - a 4-decimal
    // fine grid where next_price accumulates f64 error in
    // `price_ticks * tick_f64` before round_dp(4) snaps it back. Exercise that
    // multi-decimal path and assert the same on-grid invariant the anchor test
    // asserts: every emitted price divides evenly by the modal tick.
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::from_fingerprint_medians("ETHUSD", &fp);
    // Guard the precondition this test is about: a genuinely fine, multi-decimal
    // grid (so a future fingerprint edit that coarsened the median tick would
    // not silently turn this back into a 1-decimal duplicate of the anchor test).
    assert_eq!(scalars.price_decimals, 4);
    assert_eq!(scalars.modal_tick, Decimal::new(1, 4));
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    for _ in 0..10_000 {
        let TickEvent::Trade(trade) = src.next_tick().expect("trade") else {
            unreachable!("generated source emits trades")
        };
        assert_eq!(
            (trade.price / scalars.modal_tick).fract(),
            Decimal::ZERO,
            "off-grid price {} for tick {}",
            trade.price,
            scalars.modal_tick
        );
        assert!(matches!(
            trade.aggressor,
            AggressorSide::Buyer | AggressorSide::Seller
        ));
    }
}

// Landing 4: resuming from a checkpoint and replaying the residual yields the
// EXACT ticks a from-origin run produces - the byte-identical guarantee the
// checkpointed seek rests on. Drives the resume path directly (the golden
// sequence only exercises the from-origin path).
#[test]
fn checkpoint_resume_is_byte_identical() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let origin_ts = 1_000u64;
    let seed = 42u64;

    // Reference: a straight from-origin run, captured as every economically
    // meaningful field (ts, price, size, aggressor) so the "byte-identical"
    // name is honest - a resume that reproduced ts+price but drifted on size
    // or side would previously have slipped through.
    let mut reference = GeneratedSource::new(scalars.clone(), seed, origin_ts, &fp, None);
    let ref_ticks: Vec<(u64, Decimal, Decimal, AggressorSide)> = (0..5_000)
        .map(|_| {
            let TickEvent::Trade(t) = reference.next_tick().expect("trade") else {
                unreachable!("generated source emits trades")
            };
            (t.ts_event, t.price, t.size, t.aggressor)
        })
        .collect();

    // A small spacing so a 5k-tick run holds many checkpoints, and a target
    // deep enough that the resume restores from a non-origin checkpoint.
    let target = ref_ticks[3_000].0;
    let origin = GeneratedSource::new(scalars.clone(), seed, origin_ts, &fp, None);
    // A generous extension cap so the 3000-tick target is reached in one
    // call; the cap's runaway behavior is pinned separately below.
    let mut index = CheckpointIndex::new(origin, 128, 100_000);
    let mut resumed = index.source_at_or_before(target);
    assert!(
        index.checkpoint_count() > 1,
        "a 3000-tick seek at K=128 must have taken interior checkpoints"
    );
    assert!(
        resumed.clock_ns() <= target,
        "resume starts at or before the target"
    );

    // Drain the residual up to the target tick, then compare the tail to the
    // reference: same ts, price, size AND aggressor, tick for tick.
    let mut tick = resumed.next_tick().expect("trade");
    while tick.ts_event() < target {
        tick = resumed.next_tick().expect("trade");
    }
    for expected in &ref_ticks[3_000..3_200] {
        let TickEvent::Trade(t) = tick else {
            unreachable!("generated source emits trades")
        };
        assert_eq!(
            (t.ts_event, t.price, t.size, t.aggressor),
            *expected,
            "resumed tail diverged"
        );
        tick = resumed.next_tick().expect("trade");
    }
}

// Regression for the exact-ts collision the strictly-before partition in
// `source_at_or_before` exists for: snapshots land on every K-th tick's
// exact ts_event, and pollers pass an emitted tick's exact ts_event as the
// seek target, so a seek target CAN equal a checkpoint's clock_ns. Under
// the old `<=` partition the index handed back the checkpoint that had
// already consumed the boundary tick, and the residual seek silently
// skipped to the NEXT tick - one tick dropped versus the from-origin path.
#[test]
fn checkpoint_resume_at_exact_boundary_ts_returns_boundary_tick() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let origin_ts = 1_000u64;
    let seed = 42u64;
    let k = 128usize;

    // From-origin reference, captured as full debug strings so the
    // comparison pins every field of every tick, not just (ts, price).
    let mut reference = GeneratedSource::new(scalars.clone(), seed, origin_ts, &fp, None);
    let ref_ticks: Vec<(u64, String)> = (0..600)
        .map(|_| {
            let tick = reference.next_tick().expect("trade");
            (tick.ts_event(), format!("{tick:?}"))
        })
        .collect();

    // The 3K-th tick (0-based index 3K-1) is exactly where extend_to
    // pushes its third interior snapshot, so that snapshot's clock_ns
    // EQUALS this tick's ts_event - the forced collision. The from-origin
    // seek semantics (first tick with ts_event >= target) return this
    // boundary tick itself; the checkpointed path must return the
    // identical tick and stay byte-identical afterwards.
    let boundary = 3 * k - 1;
    let target = ref_ticks[boundary].0;

    let origin = GeneratedSource::new(scalars, seed, origin_ts, &fp, None);
    let mut index = CheckpointIndex::new(origin, k, 100_000);
    let mut resumed = index.source_at_or_before(target);
    // Origin plus exactly three interior snapshots: extend_to stops the
    // moment the lead reaches the target, right after snapshotting it -
    // proof the collision this test is about actually occurred.
    assert_eq!(index.checkpoint_count(), 4);
    assert!(
        resumed.clock_ns() < target,
        "an exact-ts target must resume strictly before the boundary tick \
         (resumed={} target={target})",
        resumed.clock_ns()
    );

    let mut tick = resumed.next_tick().expect("trade");
    while tick.ts_event() < target {
        tick = resumed.next_tick().expect("trade");
    }
    for expected in &ref_ticks[boundary..boundary + 100] {
        assert_eq!(
            (tick.ts_event(), format!("{tick:?}")),
            *expected,
            "checkpoint resume diverged from the from-origin tape"
        );
        tick = resumed.next_tick().expect("trade");
    }
}

// Coarsening bounds the index's memory (its unbounded per-`k`-ticks growth
// was the S14/D7 finding) without breaking the byte-identity guarantee.
// Drive the index past `MAX_CHECKPOINTS` at k=1 (a snapshot every tick) so
// `coarsen` fires, then assert the snapshot count stayed capped AND that
// resumes off the coarsened grid still reproduce the from-origin tape.
#[test]
fn checkpoint_index_coarsens_to_bound_memory_and_stays_byte_identical() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let origin_ts = 1_000u64;
    let seed = 42u64;

    // Enough ticks that a k=1 index would hold more than MAX_CHECKPOINTS
    // snapshots, so at least one coarsening pass must run.
    let n = MAX_CHECKPOINTS + 500;
    let mut reference = GeneratedSource::new(scalars.clone(), seed, origin_ts, &fp, None);
    let ref_ticks: Vec<(u64, String)> = (0..n)
        .map(|_| {
            let tick = reference.next_tick().expect("trade");
            (tick.ts_event(), format!("{tick:?}"))
        })
        .collect();

    let origin = GeneratedSource::new(scalars, seed, origin_ts, &fp, None);
    let mut index = CheckpointIndex::new(origin, 1, 10_000_000);
    let _ = index.source_at_or_before(ref_ticks[n - 1].0);
    // Without coarsening a k=1 walk of `n > MAX_CHECKPOINTS` ticks would hold
    // n+1 snapshots; the cap proves coarsen() ran and holds the ceiling.
    assert!(
        index.checkpoint_count() <= MAX_CHECKPOINTS,
        "coarsening must hold the snapshot count at or below the cap, got {}",
        index.checkpoint_count()
    );

    // Correctness survives coarsening: resume to several interior targets -
    // including exact emitted-tick ts_events (the D1 boundary case) - off the
    // now-sparse grid and assert byte-identical tails.
    for &probe in &[n / 4, n / 2, (3 * n) / 4, n - 100] {
        let probe_target = ref_ticks[probe].0;
        let mut resumed = index.source_at_or_before(probe_target);
        assert!(
            resumed.clock_ns() < probe_target,
            "resume starts strictly before the target after coarsening"
        );
        let mut tick = resumed.next_tick().expect("trade");
        while tick.ts_event() < probe_target {
            tick = resumed.next_tick().expect("trade");
        }
        for expected in &ref_ticks[probe..probe + 50] {
            assert_eq!(
                (tick.ts_event(), format!("{tick:?}")),
                *expected,
                "coarsened resume diverged from the from-origin tape at probe {probe}"
            );
            tick = resumed.next_tick().expect("trade");
        }
    }
}

// The runaway backstop: a target far beyond what `max_extend` permits in one
// call leaves the lead short rather than spinning the never-ending walk. This
// is what keeps a bogus or far-future `start` (which the server does not
// refuse - only `start < data_origin` is) from hanging the shared index under
// its mutex.
#[test]
fn checkpoint_extension_is_capped() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let origin_ts = 1_000u64;
    let cap = 500usize;

    let origin = GeneratedSource::new(scalars.clone(), 7u64, origin_ts, &fp, None);
    let mut index = CheckpointIndex::new(origin, 64, cap);

    // A target a decade of nanoseconds away is unreachable within the cap; the
    // walk stops at the bound instead of running forever.
    let unreachable_target = origin_ts + 315_360_000_000_000_000;
    let positioned = index.source_at_or_before(unreachable_target);
    assert!(
        positioned.clock_ns() < unreachable_target,
        "a target past the extension cap must not be reached in one call"
    );
    // At most `cap` ticks were walked, so at most `cap / K` interior snapshots
    // were taken beyond the origin.
    assert!(
        index.checkpoint_count() <= 1 + cap / 64 + 1,
        "the bounded walk took at most cap/K interior checkpoints"
    );
}

#[test]
fn clean_regime_is_byte_identical() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars, 42, 1_000, &fp, None);
    let expected = [
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.12498350, aggressor: Buyer, ts_event: 2375459195 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.10200766, aggressor: Buyer, ts_event: 17708346295 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.1, aggressor: Buyer, ts_event: 77929073872 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.08673040, aggressor: Buyer, ts_event: 86444530598 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.01819669, aggressor: Buyer, ts_event: 86561710566 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.09715509, aggressor: Buyer, ts_event: 86567076056 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.08202765, aggressor: Buyer, ts_event: 86876361485 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.08319530, aggressor: Buyer, ts_event: 98295035162 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.09242684, aggressor: Buyer, ts_event: 120993574375 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.09712421, aggressor: Buyer, ts_event: 121247548592 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.02064065, aggressor: Buyer, ts_event: 129024888135 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.29393977, aggressor: Buyer, ts_event: 130034976086 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.57435314, aggressor: Buyer, ts_event: 145146021841 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.18559909, aggressor: Buyer, ts_event: 154759534192 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.01540320, aggressor: Buyer, ts_event: 173302106626 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.07153911, aggressor: Buyer, ts_event: 190057400305 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.9, aggressor: Buyer, ts_event: 219161426338 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.1, aggressor: Buyer, ts_event: 263734424400 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.07602495, aggressor: Buyer, ts_event: 288536671002 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.22289688, aggressor: Buyer, ts_event: 294224262441 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.31333162, aggressor: Buyer, ts_event: 300725147459 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.47843512, aggressor: Buyer, ts_event: 321664742379 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.3, aggressor: Buyer, ts_event: 326769050219 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.17390790, aggressor: Buyer, ts_event: 364425972535 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.17095568, aggressor: Buyer, ts_event: 365453225597 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.1, aggressor: Buyer, ts_event: 385123311756 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.19798447, aggressor: Buyer, ts_event: 388789456068 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.76061526, aggressor: Buyer, ts_event: 424394953397 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.05957004, aggressor: Buyer, ts_event: 482437887016 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.06491765, aggressor: Buyer, ts_event: 493347162617 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.12519947, aggressor: Buyer, ts_event: 496530290227 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.32332866, aggressor: Buyer, ts_event: 496562406191 }))"#,
    ];

    for expected_tick in expected {
        assert_eq!(format!("{:?}", src.next_tick()), expected_tick);
    }
}

#[test]
fn vol_storm_lifts_realized_rms() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let regime = Some(MarketRegime::VolStorm { vol_mult: 500.0 });
    let mut clean = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut lifted = GeneratedSource::new(scalars.clone(), 42, 0, &fp, regime);
    let mut pinned = GeneratedSource::new_with_clamp_override(scalars, 42, 0, &fp, regime, 1.0);

    let clean_rms = rms(&latent_returns(&mut clean, 50_000));
    let lifted_rms = rms(&latent_returns(&mut lifted, 50_000));
    let pinned_returns = latent_returns(&mut pinned, 50_000);
    let pinned_rms = rms(&pinned_returns);
    let pinned_max = pinned_returns
        .iter()
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);

    assert!(
        lifted_rms > clean_rms * 50.0,
        "clean_rms={clean_rms} lifted_rms={lifted_rms}"
    );
    assert!(
        pinned_max <= MAX_ABS_RETURN * 1.01,
        "pinned_max={pinned_max}"
    );
    assert!(
        lifted_rms > pinned_rms * 5.0,
        "lifted_rms={lifted_rms} pinned_rms={pinned_rms}"
    );
}

#[test]
fn liquidity_drought_stretches_durations() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut clean = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut drought = GeneratedSource::new(
        scalars,
        42,
        0,
        &fp,
        Some(MarketRegime::LiquidityDrought { thin_factor: 5.0 }),
    );

    let clean_mean = mean(&durations(&mut clean, 20_000));
    let drought_mean = mean(&durations(&mut drought, 20_000));
    assert!(
        drought_mean >= clean_mean * 4.0,
        "clean_mean={clean_mean} drought_mean={drought_mean}"
    );
}

// The dying-symbol scenario the default tape's dwell bound evicts from ambient
// behavior: thin_factor multiplies only the REALIZED gap, while the ACD
// feedback and the wall-time relaxation both read the un-modulated draw, so a
// thinned tape carries the fitted clustering stretched intact rather than
// relaxing itself back to density. A constant multiplier leaves the realized
// gap ACF invariant, which is what makes that invariant testable here.
#[test]
fn liquidity_drought_imitates_dying_symbol() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(
        scalars,
        42,
        0,
        &fp,
        Some(MarketRegime::LiquidityDrought {
            thin_factor: 1000.0,
        }),
    );
    let gaps = durations(&mut src, 50_000);
    let mean_gap = mean(&gaps);
    let max_gap = gaps.iter().copied().fold(0.0_f64, f64::max);
    assert!(
        (3600.0..=14_400.0).contains(&mean_gap),
        "mean_gap={mean_gap}"
    );
    assert_near(
        "drought_duration_acf_lag1",
        acf(&gaps, 1),
        fp.golden_targets.duration_acf_anchor[0],
        DURATION_ACF_ABS_TOL,
    );
    assert!(
        max_gap >= 5.0 * mean_gap,
        "max_gap={max_gap} mean_gap={mean_gap}"
    );
}

#[test]
fn session_edge_spike_localizes() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(
        scalars,
        42,
        SESSION_START_TS,
        &fp,
        Some(MarketRegime::SessionEdgeSpike {
            start_hour: 14,
            end_hour: 16,
            extra_vol_mult: 6.0,
        }),
    );

    let (in_window, out_window) = windowed_latent_returns(&mut src, 250_000, 14, 16);
    let in_rms = rms(&in_window);
    let out_rms = rms(&out_window);
    assert!(in_rms >= out_rms * 2.0, "in_rms={in_rms} out_rms={out_rms}");
}

// Companion to vol_storm_lifts_realized_rms for the OTHER vol regime.
// Pre-fix, SessionEdgeSpike left the realized clamp pinned at 1.0, so a
// large extra_vol_mult saturated every in-window return against
// MAX_ABS_RETURN and the realized spike stopped tracking the requested
// amplification (the existing localizes test uses 6.0, below where the pin
// binds). Post-fix the realized clamp is lifted in-window by
// (1.0 + extra_vol_mult), so a near-ceiling extra_vol_mult both breaks the
// old MAX_ABS_RETURN ceiling and scales the in-window RMS with the knob.
#[test]
fn session_edge_spike_lifts_realized_clamp() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let (start_hour, end_hour) = (14u8, 16u8);
    let big = 90.0;
    let small = 6.0;
    let spike = |extra_vol_mult| MarketRegime::SessionEdgeSpike {
        start_hour,
        end_hour,
        extra_vol_mult,
    };

    let mut big_src =
        GeneratedSource::new(scalars.clone(), 42, SESSION_START_TS, &fp, Some(spike(big)));
    let mut small_src = GeneratedSource::new(
        scalars.clone(),
        42,
        SESSION_START_TS,
        &fp,
        Some(spike(small)),
    );

    let (big_in, _) = windowed_latent_returns(&mut big_src, 250_000, start_hour, end_hour);
    let (small_in, _) = windowed_latent_returns(&mut small_src, 250_000, start_hour, end_hour);
    let big_in_rms = rms(&big_in);
    let small_in_rms = rms(&small_in);
    let big_in_max = big_in
        .iter()
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);

    // Decisive: the old pinned clamp forced |return| <= MAX_ABS_RETURN
    // in-window, which is impossible to exceed. The lift breaks that ceiling.
    assert!(
        big_in_max > MAX_ABS_RETURN,
        "in-window return never exceeded the old ceiling: big_in_max={big_in_max}"
    );
    // The realized spike tracks the amplification instead of flattening:
    // 90 vs 6 is ~13x more amplification, so the RMS grows well past the ~1x
    // ratio a saturated ceiling would produce.
    assert!(
        big_in_rms > small_in_rms * 3.0,
        "big_in_rms={big_in_rms} small_in_rms={small_in_rms}"
    );

    // The in-window lift stays deterministic (it draws no extra RNG): two
    // fresh sources reproduce the stream tick for tick.
    let mut a = GeneratedSource::new(scalars.clone(), 42, SESSION_START_TS, &fp, Some(spike(big)));
    let mut b = GeneratedSource::new(scalars, 42, SESSION_START_TS, &fp, Some(spike(big)));
    for _ in 0..2_000 {
        assert_eq!(
            format!("{:?}", a.next_tick()),
            format!("{:?}", b.next_tick())
        );
    }
}

#[test]
fn reopen_gap_halts_and_gaps() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let at_ts = 400_000_000_000;
    let halt_ns = 86_400_000_000_000;
    let gap_frac = 0.05;
    let mut src = GeneratedSource::new(
        scalars,
        42,
        0,
        &fp,
        Some(MarketRegime::ReopenGap {
            at_ts,
            halt_secs: halt_ns / 1_000_000_000,
            gap_frac,
        }),
    );

    let mut prior_ts = 0;
    let mut prior_mid = src.vol.mid;
    let mut gap_return = None;
    let mut straddling_gap = None;
    let mut large_gaps = 0;
    for _ in 0..10_000 {
        let _tick = src.next_tick().expect("unbounded generated source");
        let dt = src.clock_ns - prior_ts;
        if dt >= halt_ns {
            large_gaps += 1;
        }
        if prior_ts < at_ts && at_ts <= src.clock_ns {
            straddling_gap = Some(dt);
            gap_return = Some((src.vol.mid / prior_mid).ln());
        }
        prior_ts = src.clock_ns;
        prior_mid = src.vol.mid;
    }

    assert_eq!(large_gaps, 1, "large_gaps={large_gaps}");
    assert!(
        straddling_gap.expect("gap straddles at_ts") >= halt_ns,
        "straddling_gap={straddling_gap:?}"
    );
    let gap_return = gap_return.expect("gap return measured");
    assert!(
        (gap_return - gap_frac).abs() <= 0.001,
        "gap_return={gap_return} gap_frac={gap_frac}"
    );
}

// A ReopenGap whose at_ts is at or before the tape anchor has already
// elapsed: the crossing condition (old_clock < at_ts) can never hold, so
// pre-fix the halt sat armed forever, silently inert. The fix consumes it
// at construction (fail closed, with a stderr warning) and draws no RNG,
// so the resulting stream must be byte-identical to a regime-free run - in
// particular no halt-sized gap and no mid jump can ever appear.
#[test]
fn reopen_gap_at_or_before_anchor_is_consumed_and_matches_clean() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let start_ts = 1_000_000u64;
    // The anchor itself (gaps cover (old, new], so an instant exactly at
    // the anchor is already past), one tick before it, and the earliest
    // instant the protocol validator admits.
    for at_ts in [start_ts, start_ts - 1, 1] {
        let regime = Some(MarketRegime::ReopenGap {
            at_ts,
            halt_secs: 86_400,
            gap_frac: 0.05,
        });
        let mut clean = GeneratedSource::new(scalars.clone(), 42, start_ts, &fp, None);
        let mut gapped = GeneratedSource::new(scalars.clone(), 42, start_ts, &fp, regime);
        for _ in 0..5_000 {
            assert_eq!(
                format!("{:?}", clean.next_tick()),
                format!("{:?}", gapped.next_tick()),
                "an elapsed ReopenGap (at_ts={at_ts}) must leave the stream untouched"
            );
        }
    }
}

// Trading hours are expressed as near-zero hour shares, not a separate
// code path. Pre-fix, the arrival multiplier was sampled once at the
// instant a gap opened and the whole draw was divided by it: a 1e-12 share
// stretched the first draw by ~4e10, saturating the ns cast at u64::MAX
// and pinning the clock there forever. Post-fix a gap opening below the
// closed-window gate treats the draw as a budget integrated hour by hour,
// so the closed hour consumes almost none of it and the tape resumes
// roughly when the next open hour begins.
#[test]
fn near_zero_hour_share_reopens_at_the_next_open_hour() {
    // SESSION_START_TS is exactly hour 0 of a Monday; re-pin that here
    // since the placement of the closed hour depends on it.
    assert_eq!(utc_hour_dow(SESSION_START_TS), (0, 1));
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut intensity_hour = [(1.0 - 1e-12) / 23.0; 24];
    intensity_hour[0] = 1e-12;
    let profile = SessionProfile {
        intensity_hour,
        vol_hour: [1.0; 24],
        dow_weight: [1.0 / 7.0; 7],
    };
    profile
        .validate()
        .expect("near-zero shares pass validation");

    let mut src = GeneratedSource::new_with_session_profile(
        scalars.clone(),
        42,
        SESSION_START_TS,
        &fp,
        &profile,
        None,
    );
    let first = src.next_tick().expect("trade").ts_event();
    // The first tick must survive the closed hour 0 (not print inside it)
    // and land shortly after hour 1 opens - not months out, and nowhere
    // near u64 saturation. The upper bound leaves room for the draw's own
    // few seconds of open-hour budget plus heavy-tail slack.
    assert!(
        first >= SESSION_START_TS + NS_PER_HOUR,
        "first tick printed inside the closed hour: first={first}"
    );
    assert!(
        first < SESSION_START_TS + 3 * NS_PER_HOUR,
        "first tick overshot the reopen: first={first}"
    );

    // Determinism holds on the integrating path (it draws no RNG), and
    // strict monotonicity survives the repeated daily close.
    let mut twin = GeneratedSource::new_with_session_profile(
        scalars,
        42,
        SESSION_START_TS,
        &fp,
        &profile,
        None,
    );
    assert_eq!(twin.next_tick().expect("trade").ts_event(), first);
    let mut prior = first;
    for _ in 0..5_000 {
        let tick = src.next_tick().expect("trade");
        let twin_tick = twin.next_tick().expect("trade");
        assert_eq!(format!("{tick:?}"), format!("{twin_tick:?}"));
        assert!(tick.ts_event() > prior, "clock stalled at {prior}");
        prior = tick.ts_event();
    }
}

// The degenerate extreme: EVERY hour closed so hard the budget can never
// be spent. The hour walk must cap each gap at MAX_SESSION_GAP_NS and keep
// the clock strictly advancing - one tick per ~year - instead of the
// pre-fix u64::MAX saturation that pinned the clock forever. 300 capped
// gaps cover ~300 years of sim time while staying far from the u64
// nanosecond ceiling (~584 years).
#[test]
fn fully_closed_profile_caps_each_gap_and_never_freezes_the_clock() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let profile = SessionProfile {
        intensity_hour: [1e-30; 24],
        vol_hour: [1.0; 24],
        dow_weight: [1.0 / 7.0; 7],
    };
    profile.validate().expect("tiny shares pass validation");
    let mut src = GeneratedSource::new_with_session_profile(
        scalars,
        42,
        SESSION_START_TS,
        &fp,
        &profile,
        None,
    );
    let mut prior = SESSION_START_TS;
    for _ in 0..300 {
        let ts = src.next_tick().expect("trade").ts_event();
        assert!(ts > prior, "clock froze: ts={ts} prior={prior}");
        assert!(ts < u64::MAX, "clock saturated at the u64 ceiling");
        assert!(
            ts - prior <= MAX_SESSION_GAP_NS,
            "gap {} exceeds the per-gap cap",
            ts - prior
        );
        prior = ts;
    }
}

#[test]
fn realism() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let measured = measure(&mut src, &scalars, DRAW);
    assert_in_range(
        "duration_dispersion_index",
        measured.duration_dispersion_index,
        &fp.golden_targets.duration_dispersion_index.range,
    );
    assert_near(
        "duration_acf_lag1",
        measured.duration_acf_lag1,
        fp.golden_targets.duration_acf_anchor[0],
        DURATION_ACF_ABS_TOL,
    );
    assert_near(
        "duration_acf_lag5",
        measured.duration_acf_lag5,
        fp.golden_targets.duration_acf_anchor[4],
        DURATION_ACF_ABS_TOL,
    );
    assert_in_range(
        "return_acf_lag1",
        measured.return_acf_lag1,
        &fp.golden_targets.return_acf_lag1.range,
    );
    assert_in_range(
        "abs_return_acf_lag1",
        measured.abs_return_acf_lag1,
        &fp.golden_targets.abs_return_acf.lag1.range,
    );
    assert_in_range(
        "abs_return_acf_lag10",
        measured.abs_return_acf_lag10,
        &fp.golden_targets.abs_return_acf.lag10.range,
    );
    assert_in_range(
        "abs_return_acf_lag50",
        measured.abs_return_acf_lag50,
        &fp.golden_targets.abs_return_acf.lag50.range,
    );
    assert_in_range(
        "zero_change_frac",
        measured.zero_change_frac,
        &fp.golden_targets.zero_change_frac.range,
    );
    assert_in_range(
        "round_lot_frac",
        measured.round_lot_frac,
        &fp.scalar_ranges.size_round_frac,
    );
    assert!(measured.size_cv > 0.5, "size_cv={}", measured.size_cv);
    assert_eq!(measured.off_grid_prices, 0);
    assert_eq!(measured.neutral_aggressors, 0);
    assert_dwell_is_bounded(&measured, &scalars, &fp);
}

// The dwell bound asserted against the tape broadarrow actually consumes. The
// realism gate runs at seed 42, but the server keys each symbol's walk on an
// FNV-1a-64 hash of the symbol, so the served BTCUSDT walk is a different
// realization from every other committed test - and until this test existed,
// nothing asserted anything about it at all.
#[test]
fn default_symbol_tape_dwell_is_bounded() {
    let fp = Fingerprint::from_repo_json();
    // Reconstructs mogwai-server's `default_profile`: fingerprint medians
    // overlaid with the default instrument's tick size and precision. The
    // instrument def comes from mogwai-protocol so it cannot drift; the seed
    // fold is duplicated from `seed_for` in mogwai-server's source.rs, which
    // stays the source of truth (mogwai-data cannot depend on the server).
    let def = mogwai_protocol::default_instruments()
        .into_iter()
        .find(|def| def.symbol == "BTCUSDT")
        .expect("the default instrument set carries BTCUSDT");
    let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, &fp);
    scalars.modal_tick = def.price_increment;
    scalars.price_decimals = u32::from(def.price_precision);
    let mut seed = 0xcbf2_9ce4_8422_2325_u64;
    for byte in def.symbol.bytes() {
        seed = (seed ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut src = GeneratedSource::new(scalars.clone(), seed, 0, &fp, None);
    let measured = measure(&mut src, &scalars, DRAW);
    assert_dwell_is_bounded(&measured, &scalars, &fp);
}

fn assert_dwell_is_bounded(measured: &Measured, scalars: &GeneratorScalars, fp: &Fingerprint) {
    let dwell = &fp.golden_targets.dwell;
    let cadence = scalars.mean_duration_s / dwell.mean_s.anchor;
    assert!(
        measured.gap_p999_s <= DWELL_P999_SLACK * cadence * dwell.gap_p999_s.anchor,
        "gap_p999_s={} bound={}",
        measured.gap_p999_s,
        DWELL_P999_SLACK * cadence * dwell.gap_p999_s.anchor
    );
    assert!(
        measured.empty_hour_frac <= dwell.empty_hour_frac.anchor + EMPTY_HOUR_FRAC_SLACK,
        "empty_hour_frac={} bound={}",
        measured.empty_hour_frac,
        dwell.empty_hour_frac.anchor + EMPTY_HOUR_FRAC_SLACK
    );
    assert!(
        measured.max_empty_hour_run_h <= dwell.max_empty_hour_run_h.anchor + EMPTY_HOUR_RUN_SLACK_H,
        "max_empty_hour_run_h={} bound={}",
        measured.max_empty_hour_run_h,
        dwell.max_empty_hour_run_h.anchor + EMPTY_HOUR_RUN_SLACK_H
    );
    assert!(
        (measured.mean_gap_s - scalars.mean_duration_s).abs()
            <= MEAN_GAP_REL_TOL * scalars.mean_duration_s,
        "mean_gap_s={} declared_mean_s={}",
        measured.mean_gap_s,
        scalars.mean_duration_s
    );
}

// NOTE: this test asserts the RAW fingerprint shares (intensity_hour,
// dow_weight) directly against measured occupancy fractions. That only holds
// because SessionModulator centers each arrival multiplier on 1.0 (the share
// times 24 or 7), so occupancy converges back to the underlying share. If the
// centering convention in SessionModulator::new ever changes, these
// assertions break in a non-obvious way - and because the test is #[ignore]d
// (it draws 5M ticks), `brokkr check` will not catch the regression. Re-derive
// the expected curves alongside any centering change.
#[test]
#[ignore]
fn session_modulation_reproduces_curves() {
    assert_eq!(utc_hour_dow(0), (0, 4));
    assert_eq!(utc_hour_dow(1_700_000_000_000_000_000), (22, 2));
    assert_eq!(utc_hour_dow(SESSION_START_TS), (0, 1));

    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars, 42, SESSION_START_TS, &fp, None);
    let measured = measure_session_curves(&mut src, SESSION_DRAW);

    for h in 0..24 {
        assert_near(
            "intensity_hour",
            measured.intensity_hour[h],
            fp.session_profile.intensity_hour[h],
            INTENSITY_SHARE_ABS_TOL,
        );
    }
    let max_intensity_hour = argmax(&measured.intensity_hour);
    let min_intensity_hour = argmin(&measured.intensity_hour);
    assert!(
        (14..=16).contains(&max_intensity_hour),
        "max_intensity_hour={max_intensity_hour}"
    );
    assert!(
        (3..=6).contains(&min_intensity_hour),
        "min_intensity_hour={min_intensity_hour}"
    );

    let max_vol_hour = argmax(&measured.vol_hour);
    assert_eq!(max_vol_hour, 14);
    assert!(
        measured.vol_hour[14] > 1.8,
        "vol_hour[14]={}",
        measured.vol_hour[14]
    );
    assert!(
        measured.vol_hour[1] < 1.0,
        "vol_hour[1]={}",
        measured.vol_hour[1]
    );
    let vol_corr = pearson(&measured.vol_hour, &fp.session_profile.vol_hour);
    assert!(vol_corr > 0.9, "vol_corr={vol_corr}");

    for d in 0..7 {
        assert_near(
            "dow_weight",
            measured.dow_weight[d],
            fp.session_profile.dow_weight[d],
            DOW_SHARE_ABS_TOL,
        );
    }
    for weekday in 1..=5 {
        assert!(
            measured.dow_weight[0] < measured.dow_weight[weekday],
            "sun={} weekday{}={}",
            measured.dow_weight[0],
            weekday,
            measured.dow_weight[weekday]
        );
        assert!(
            measured.dow_weight[6] < measured.dow_weight[weekday],
            "sat={} weekday{}={}",
            measured.dow_weight[6],
            weekday,
            measured.dow_weight[weekday]
        );
    }
}

#[derive(Default)]
struct Measured {
    mean_gap_s: f64,
    #[expect(
        dead_code,
        reason = "Sample maxima are recorded for diagnostic context but intentionally never gated."
    )]
    max_gap_s: f64,
    gap_p999_s: f64,
    empty_hour_frac: f64,
    max_empty_hour_run_h: f64,
    duration_dispersion_index: f64,
    duration_acf_lag1: f64,
    duration_acf_lag5: f64,
    return_acf_lag1: f64,
    abs_return_acf_lag1: f64,
    abs_return_acf_lag10: f64,
    abs_return_acf_lag50: f64,
    zero_change_frac: f64,
    round_lot_frac: f64,
    size_cv: f64,
    off_grid_prices: usize,
    neutral_aggressors: usize,
}

fn measure(src: &mut GeneratedSource, scalars: &GeneratorScalars, draw: usize) -> Measured {
    let mut timestamps = Vec::with_capacity(draw);
    let mut prices = Vec::with_capacity(draw);
    let mut sizes = Vec::with_capacity(draw);
    let mut off_grid_prices = 0;
    let mut neutral_aggressors = 0;
    for _ in 0..draw {
        let TickEvent::Trade(trade) = src.next_tick().expect("unbounded generated source") else {
            unreachable!("generated source emits trades")
        };
        if (trade.price / scalars.modal_tick).fract() != Decimal::ZERO {
            off_grid_prices += 1;
        }
        if trade.aggressor == AggressorSide::NoAggressor {
            neutral_aggressors += 1;
        }
        timestamps.push(trade.ts_event);
        prices.push(decimal_to_f64(trade.price));
        sizes.push(trade.size);
    }

    let mut durations = Vec::with_capacity(draw - 1);
    let mut returns = Vec::with_capacity(draw - 1);
    let mut zero_changes = 0;
    for i in 1..draw {
        durations.push((timestamps[i] - timestamps[i - 1]) as f64 / 1_000_000_000.0);
        returns.push((prices[i] / prices[i - 1]).ln());
        if prices[i] == prices[i - 1] {
            zero_changes += 1;
        }
    }
    let abs_returns: Vec<f64> = returns.iter().map(|r| r.abs()).collect();
    let sizes_f64: Vec<f64> = sizes.iter().copied().map(decimal_to_f64).collect();
    let round_lots = sizes.iter().filter(|size| is_round_lot(**size)).count();

    let mut sorted_durations = durations.clone();
    sorted_durations.sort_by(f64::total_cmp);
    let p999_index = (999 * sorted_durations.len()).div_ceil(1000) - 1;
    let (empty_hour_frac, max_empty_hour_run_h) = empty_hour_stats(&timestamps);
    Measured {
        mean_gap_s: mean(&durations),
        max_gap_s: durations.iter().copied().fold(0.0_f64, f64::max),
        gap_p999_s: sorted_durations[p999_index],
        empty_hour_frac,
        max_empty_hour_run_h,
        duration_dispersion_index: variance(&durations) / mean(&durations),
        duration_acf_lag1: acf(&durations, 1),
        duration_acf_lag5: acf(&durations, 5),
        return_acf_lag1: acf(&returns, 1),
        abs_return_acf_lag1: acf(&abs_returns, 1),
        abs_return_acf_lag10: acf(&abs_returns, 10),
        abs_return_acf_lag50: acf(&abs_returns, 50),
        zero_change_frac: zero_changes as f64 / returns.len() as f64,
        round_lot_frac: round_lots as f64 / sizes.len() as f64,
        size_cv: variance(&sizes_f64).sqrt() / mean(&sizes_f64),
        off_grid_prices,
        neutral_aggressors,
    }
}

fn empty_hour_stats(timestamps_ns: &[u64]) -> (f64, f64) {
    let Some(&first) = timestamps_ns.first() else {
        return (0.0, 0.0);
    };
    let Some(&last) = timestamps_ns.last() else {
        return (0.0, 0.0);
    };
    // Population: every whole UTC hour bucket lying fully inside [first, last],
    // matching `dwell_stats` in analysis/characterize.py bucket for bucket so
    // the gate compares like with like. A span holding no complete bucket
    // defines both statistics as 0, which keeps the helper total.
    let first_complete = first.div_ceil(NS_PER_HOUR);
    let after_last_complete = last / NS_PER_HOUR;
    if first_complete >= after_last_complete {
        return (0.0, 0.0);
    }
    let seen: HashSet<u64> = timestamps_ns.iter().map(|ts| ts / NS_PER_HOUR).collect();
    let total = after_last_complete - first_complete;
    let mut empty = 0_u64;
    let mut run = 0_u64;
    let mut max_run = 0_u64;
    for hour in first_complete..after_last_complete {
        if seen.contains(&hour) {
            run = 0;
        } else {
            empty += 1;
            run += 1;
            max_run = max_run.max(run);
        }
    }
    (empty as f64 / total as f64, max_run as f64)
}

#[test]
fn empty_hour_stats_use_complete_utc_buckets() {
    let h = NS_PER_HOUR;
    // Hand-built fixture for the rule `dwell_stats` implements on the Python
    // side; keep the two in step or the gate compares different populations.
    // Buckets 1 and 2 are complete, bucket 2 is empty.
    assert_eq!(empty_hour_stats(&[h / 2, h + 1, 4 * h - 1]), (0.5, 1.0));
    // No complete bucket inside the span at all.
    assert_eq!(empty_hour_stats(&[h / 2, h + 1, 2 * h - 1]), (0.0, 0.0));
    // Buckets 0 through 4 are complete (the last one ends exactly on the final
    // trade, so it counts) and only bucket 0 is occupied: a four-hour desert.
    assert_eq!(empty_hour_stats(&[0, 5 * h]), (0.8, 4.0));
}

fn durations(src: &mut GeneratedSource, draw: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(draw.saturating_sub(1));
    let mut prior_ts = None;
    for _ in 0..draw {
        let tick = src.next_tick().expect("unbounded generated source");
        if let Some(prior) = prior_ts {
            out.push((tick.ts_event() - prior) as f64 / 1_000_000_000.0);
        }
        prior_ts = Some(tick.ts_event());
    }
    out
}

fn latent_returns(src: &mut GeneratedSource, draw: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(draw.saturating_sub(1));
    let mut prior_mid = src.vol.mid;
    for _ in 0..draw {
        let _tick = src.next_tick().expect("unbounded generated source");
        out.push((src.vol.mid / prior_mid).ln());
        prior_mid = src.vol.mid;
    }
    out
}

fn windowed_latent_returns(
    src: &mut GeneratedSource,
    draw: usize,
    start_hour: u8,
    end_hour: u8,
) -> (Vec<f64>, Vec<f64>) {
    let mut in_window = Vec::new();
    let mut out_window = Vec::new();
    let mut prior_mid = src.vol.mid;
    for _ in 0..draw {
        let _tick = src.next_tick().expect("unbounded generated source");
        let ret = (src.vol.mid / prior_mid).ln();
        let hour = utc_hour(src.clock_ns);
        if usize::from(start_hour) <= hour && hour < usize::from(end_hour) {
            in_window.push(ret);
        } else {
            out_window.push(ret);
        }
        prior_mid = src.vol.mid;
    }
    (in_window, out_window)
}

fn rms(values: &[f64]) -> f64 {
    (values.iter().map(|value| value.powi(2)).sum::<f64>() / values.len() as f64).sqrt()
}

struct SessionCurves {
    intensity_hour: [f64; 24],
    vol_hour: [f64; 24],
    dow_weight: [f64; 7],
}

fn measure_session_curves(src: &mut GeneratedSource, draw: usize) -> SessionCurves {
    let mut hour_count = [0_u64; 24];
    let mut ret_count_hour = [0_u64; 24];
    let mut sumsq_ret_hour = [0.0; 24];
    let mut dow_count = [0_u64; 7];
    let mut prev_price: Option<f64> = None;

    for _ in 0..draw {
        let TickEvent::Trade(trade) = src.next_tick().expect("unbounded generated source") else {
            unreachable!("generated source emits trades")
        };
        let (hour, dow) = utc_hour_dow(trade.ts_event);
        hour_count[hour] += 1;
        dow_count[dow] += 1;

        let price = decimal_to_f64(trade.price);
        if let Some(prev) = prev_price {
            let ret = (price / prev).ln();
            sumsq_ret_hour[hour] += ret.powi(2);
            // The RMS divisor counts only return-contributing trades. The
            // very first trade of the whole draw has no predecessor and
            // contributes no squared return, so dividing sumsq by hour_count
            // (which includes it) would deflate that one hour's RMS by one
            // trade. ret_count_hour tracks exactly the trades that added a
            // squared return.
            ret_count_hour[hour] += 1;
        }
        prev_price = Some(price);
    }

    let total_hour = hour_count.iter().sum::<u64>() as f64;
    let total_dow = dow_count.iter().sum::<u64>() as f64;
    let mut intensity_hour = [0.0; 24];
    let mut rms_hour = [0.0; 24];
    let mut populated_hours = 0;
    for h in 0..24 {
        intensity_hour[h] = hour_count[h] as f64 / total_hour;
        if ret_count_hour[h] > 0 {
            rms_hour[h] = (sumsq_ret_hour[h] / ret_count_hour[h] as f64).sqrt();
            populated_hours += 1;
        }
    }

    let rms_mean =
        rms_hour.iter().filter(|value| **value > 0.0).sum::<f64>() / populated_hours as f64;
    let mut vol_hour = [0.0; 24];
    for h in 0..24 {
        vol_hour[h] = rms_hour[h] / rms_mean;
    }

    let mut dow_weight = [0.0; 7];
    for d in 0..7 {
        dow_weight[d] = dow_count[d] as f64 / total_dow;
    }

    SessionCurves {
        intensity_hour,
        vol_hour,
        dow_weight,
    }
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn variance(values: &[f64]) -> f64 {
    let mean = mean(values);
    values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / values.len() as f64
}

fn acf(values: &[f64], lag: usize) -> f64 {
    let mean = mean(values);
    let denom = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>();
    let num = values
        .iter()
        .zip(values.iter().skip(lag))
        .map(|(a, b)| (a - mean) * (b - mean))
        .sum::<f64>();
    num / denom
}

fn is_round_lot(size: Decimal) -> bool {
    let normalized = size.normalize();
    normalized.scale() <= 1
}

fn assert_in_range(label: &str, value: f64, range: &MinMedianMax) {
    assert!(
        range.contains(value),
        "{label}={value} outside [{}, {}]",
        range.min,
        range.max
    );
}

fn assert_near(label: &str, value: f64, expected: f64, tolerance: f64) {
    assert!(
        (value - expected).abs() <= tolerance,
        "{label}={value} outside {expected} +/- {tolerance}"
    );
}

fn argmax<const N: usize>(values: &[f64; N]) -> usize {
    values
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(idx, _)| idx)
        .expect("non-empty array")
}

fn argmin<const N: usize>(values: &[f64; N]) -> usize {
    values
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| a.total_cmp(b))
        .map(|(idx, _)| idx)
        .expect("non-empty array")
}

fn pearson<const N: usize>(a: &[f64; N], b: &[f64; N]) -> f64 {
    let mean_a = a.iter().sum::<f64>() / N as f64;
    let mean_b = b.iter().sum::<f64>() / N as f64;
    let mut numerator = 0.0;
    let mut denom_a = 0.0;
    let mut denom_b = 0.0;
    for i in 0..N {
        let da = a[i] - mean_a;
        let db = b[i] - mean_b;
        numerator += da * db;
        denom_a += da.powi(2);
        denom_b += db.powi(2);
    }
    numerator / (denom_a.sqrt() * denom_b.sqrt())
}
