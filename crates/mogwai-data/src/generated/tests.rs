// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

// Compiling out the five heavy walks (see the note below) strands the helpers
// only they reach - `measure`, `measure_session_curves`,
// `windowed_latent_returns` and the two result structs. They stay compiled in
// that shape, which is exactly what keeps the instrumented sweep a real build
// check of this file rather than a hole in it; they are simply unreachable, so
// the lint is silenced there and only there. The default shape still reports
// dead code in this file, so a helper whose last real caller is deleted is
// still caught - which is the decay a blanket allow would otherwise hide.
#![cfg_attr(feature = "hotpath", allow(dead_code))]

use mogwai_protocol::{AggressorSide, MarketRegime, decimal_to_f64};

use crate::{TickEvent, TickFault, TickSource};

use super::arrival::cadence_base_mean_s;
use super::checkpoint::MAX_CHECKPOINTS;
use super::consts::{
    ARRIVAL_MEAN_CAL, ARRIVAL_QUIET_ACTIVE_RATIO, ARRIVAL_STATE_PERSISTENCE,
    FEEDBACK_RETURN_CEILING, GARCH_ARCH, GARCH_GARCH, GARCH_SIGMA_CAP, MAX_SESSION_GAP_NS,
    NS_PER_HOUR, REALIZED_RETURN_CEILING, STUDENT_T_DF, STUDENT_T_UNIT_SCALE,
    TRADE_BOUNCE_HALF_WIDTH_TICKS, VOL_SCALAR,
};
use super::numeric::decimal_from_f64;
use super::session::{utc_hour, utc_hour_dow};
use super::source::{integral_lot, repeat_is_compatible};
use super::*;
use rust_decimal::Decimal;
use std::collections::HashSet;

// The heavy-walk cfg note. Five tests in this file carry
// `#[cfg(not(feature = "hotpath"))]` and point back here; this is the one place
// the reasoning lives.
//
// This crate's lib test binary is built and run twice by the full gate: once in
// the default shape, and once with `hotpath-alloc` on, so that a feature nothing
// compiles cannot rot unnoticed. That second sweep exists to prove the
// annotations, the optional dependency and the harness examples still build - a
// compile-time property, satisfied by compiling.
//
// Re-executing a multi-second statistical walk in it proves nothing further,
// because `crates/mogwai-data/src` carries no `hotpath` annotation at all: the
// only `cfg(feature = "hotpath")` in this crate is in
// `examples/arrival_walk_bench.rs`. The two shapes differ in the dependency
// graph and in that example, not in one byte of behaviour these tests can
// observe - measured, `session_modulation_reproduces_curves` runs 7.65 s in the
// default shape and 7.67 s in the instrumented one.
//
// So the gate paid about 28 s of dev-profile compute per run for a re-run that
// cannot say anything the first run did not. The five walks opt out of the
// instrumented shape; the other ~170 tests in this binary still run in both,
// which is what keeps the second sweep a real sweep rather than a build check.
// Opting out is the mechanism rather than a runner-side filter, deliberately:
// the gate certifies complete coverage, so a filtered-out test is an orphaned
// pair and an error, while a test that does not exist in a build shape is no
// pair at all.
//
// The line is drawn at roughly two seconds, measured one test per process in
// dev by `scripts/adapter_test_walls.py`, which is generic over libtest
// binaries. Below that the doubling is not worth a `cfg` a reader has to come
// here to understand.
//
// If this crate ever does grow an annotation in `src`, this stays sound: an
// annotation wraps a call in a timer, it does not move a value, and the
// remaining tests still compile and run it in both shapes.

fn next_trade(source: &mut GeneratedSource) -> mogwai_protocol::TradeTick {
    loop {
        match source.next_tick().expect("infinite generated tape") {
            TickEvent::Trade(trade) => return trade,
            TickEvent::Quote(_) => {}
        }
    }
}

#[test]
fn clones_share_immutable_config_and_emitted_symbol_storage() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut source = GeneratedSource::new(scalars, 4242, 1_000, &fp, None);
    let mut clone = source.clone();
    assert!(source.shares_immutable_config_with(&clone));

    let left = next_trade(&mut source);
    let right = next_trade(&mut clone);
    assert!(std::sync::Arc::ptr_eq(&left.symbol, &right.symbol));
}

/// An integrated source whose origin sits at `origin_ns`, for building arrival
/// refusals. `next_parent` computes `limit = from_ns + MAX_SESSION_GAP_NS`
/// saturating, so an origin at or near `u64::MAX` leaves the budget walk with
/// no exposure to spend and it refuses `NoOpenExposure` in O(1) - the budget
/// walk runs at most one iteration and advances no cell, which is what makes
/// this cheap enough to be the crate's refusal fixture.
fn refusing_source_at(origin_ns: u64) -> GeneratedSource {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.arrival = Some(ArrivalConfig::LogOuCox {
        sigma_y: 0.6,
        tau_s: 60.0,
    });
    GeneratedSource::new(scalars, 91, origin_ns, &fp, None)
}

/// The refusal fixture at the very last representable instant: `cursor < limit`
/// is false on entry, so `next_parent` refuses without executing its loop once.
/// Use this wherever the refusal is all that matters. A consumer whose own guard
/// compares the source clock against a forward target cannot use it - nothing is
/// after `u64::MAX` - and wants `refusing_source_at` a little below the top
/// instead; see the `extend_toward` test.
fn refusing_source() -> GeneratedSource {
    refusing_source_at(u64::MAX)
}

#[test]
fn a_refused_parent_is_reported_rather_than_summarized_as_a_phantom() {
    let mut source = refusing_source();
    assert!(source.fault().is_none(), "the fixture starts clean");
    let refusal = source
        .advance_parent()
        .expect_err("the kernel refuses at the end of representable time");
    assert!(matches!(
        refusal,
        TickFault::Arrival(ArrivalRefusal::NoOpenExposure { .. })
    ));
    // The fault is latched, not merely returned: a caller that only consults
    // `fault()` must see the same verdict.
    assert!(matches!(
        source.fault(),
        Some(TickFault::Arrival(ArrivalRefusal::NoOpenExposure { .. }))
    ));
    // And it is terminal - a second call must not draw again.
    assert!(source.advance_parent().is_err());
}

#[test]
fn seek_to_terminates_on_a_refusal_instead_of_spinning_on_a_stale_summary() {
    // The refusal leaves `burst` untouched, so an infallible `advance_parent`
    // handed back the previous parent's timestamp with zero children - a
    // `parent_end` of 0, below any forward target - and `seek_to` adopted the
    // faulted clone and looped forever without ever reaching `next_tick`, the
    // one place the fault is read. A regression therefore hangs rather than
    // asserting, so the seek runs on its own thread against a deadline.
    //
    // The detached thread leaks on regression and that is deliberate: a spinning
    // `seek_to` cannot be cancelled, so the choice is a busy core for the rest
    // of a failing sweep or no test at all. It costs nothing when green - the
    // refusing fixture makes `seek_to` return on its first iteration, so the
    // thread is finished before `recv_timeout` is even entered. Do not
    // re-litigate this into a synchronous call; that reintroduces the hang.
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let mut source = refusing_source();
        let tick = source.seek_to(1_000);
        tx.send((tick.is_none(), source.fault()))
            .expect("the seek result is received");
    });
    let (ended, fault) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("seek_to must terminate on a refusal");
    handle.join().expect("the seek thread joins");
    assert!(ended, "a refused walk yields no tick");
    assert!(
        matches!(
            fault,
            Some(TickFault::Arrival(ArrivalRefusal::NoOpenExposure { .. }))
        ),
        "the refusal must survive on the seeked source, not just end the loop"
    );
}

#[test]
fn extend_toward_credits_no_walk_for_a_refused_parent() {
    // The frontier rule, at the site it was written for: `extend_toward` may
    // only credit `walked`/`since_snapshot` and snapshot the lead over a parent
    // the same expression drew successfully. An infallible `advance_parent`
    // handed it a phantom - the empty burst's `parent_ts_ns` of 0 with zero
    // children - whose `parent_end` of 0 is below any target, so it adopted the
    // faulted clone, credited one tick for a parent that was never drawn, took
    // snapshots of a faulted source, and burned the entire `max_extend` budget
    // before reporting "unreachable" rather than "faulted".
    //
    // The origin sits just below `u64::MAX` rather than at it: the loop guard is
    // `self.lead.clock_ns() < target`, which no lead at the top instant can
    // satisfy, so the fixture used by the two tests above would skip the loop
    // entirely and pass for the wrong reason. A few nanoseconds of headroom keep
    // the guard satisfiable while `limit = from_ns + MAX_SESSION_GAP_NS` still
    // saturates, so the refusal is still O(1).
    let origin = u64::MAX - 1_000;
    let source = refusing_source_at(origin);
    assert!(source.fault().is_none(), "the fixture starts clean");
    assert_eq!(source.clock_ns(), origin);

    let max_extend = 10_000;
    let mut index = CheckpointIndex::new(source, 1, max_extend);
    assert_eq!(index.checkpoint_count(), 1, "only the pinned origin");

    let walked = index.extend_toward(u64::MAX);

    assert_eq!(
        walked, 0,
        "a refused parent is no walk: crediting it is the frontier defect"
    );
    assert!(
        matches!(
            index.fault(),
            Some(TickFault::Arrival(ArrivalRefusal::NoOpenExposure { .. }))
        ),
        "the faulted lead must be adopted, so the index can name the refusal \
         instead of reporting an exhausted budget"
    );
    assert_eq!(
        index.checkpoint_count(),
        1,
        "no snapshot may be taken of a source that drew nothing"
    );
}

#[test]
fn compact_parent_advancement_matches_wire_frames_and_continuation() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut compact = GeneratedSource::new(scalars.clone(), 4242, 1_000, &fp, None);
    let mut wire = GeneratedSource::new(scalars, 4242, 1_000, &fp, None);

    for _ in 0..512 {
        let summary = compact.advance_parent().expect("compact parent");
        let quote = wire.next_tick().expect("parent quote");
        assert!(matches!(quote, TickEvent::Quote(_)));
        assert_eq!(quote.ts_event(), summary.parent_ts_ns);
        for child in 0..summary.child_count {
            let trade = wire.next_tick().expect("parent child");
            assert!(matches!(trade, TickEvent::Trade(_)));
            assert_eq!(
                trade.ts_event(),
                summary.parent_ts_ns + u64::from(child) * summary.child_stride_ns
            );
        }
    }

    for _ in 0..1_000 {
        assert_eq!(
            format!("{:?}", compact.next_tick()),
            format!("{:?}", wire.next_tick())
        );
    }
}

#[test]
fn compact_seek_is_wire_identical_and_preserves_continuation() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut compact = GeneratedSource::new(scalars.clone(), 4242, 1_000, &fp, None);
    let mut wire = GeneratedSource::new(scalars, 4242, 1_000, &fp, None);
    let target = 60_000_000_000;

    let compact_head = compact.seek_to(target).expect("infinite compact tape");
    let wire_head = loop {
        let tick = wire.next_tick().expect("infinite wire tape");
        if tick.ts_event() >= target {
            break tick;
        }
    };
    assert_eq!(format!("{compact_head:?}"), format!("{wire_head:?}"));
    for _ in 0..1_000 {
        assert_eq!(
            format!("{:?}", compact.next_tick()),
            format!("{:?}", wire.next_tick())
        );
    }
}

/// A surge moves the canonical tape, so a history read placed inside the surge
/// window must replay the surged prints rather than the clean ones. That is
/// what keeps one river one realization: the subscribed feed, `/trades`, the
/// volatility reading and the trigger scans are all this one walk.
///
/// The read goes through `try_source_before` deliberately. Comparing two
/// clones of the lead proves only that the lead is surged, which was never in
/// doubt; what has to agree is the chain, because that is what a history
/// request resumes from.
///
/// The surge is installed at construction and the clean river is a separate
/// source. It used to be armed mid-walk on a shared index, with the clean
/// comparison taken from a frontier clone - which was the mutation the fork
/// removed. Two rivers is now the honest fixture, because that is what a
/// passenger carrying an arm and a passenger without one actually board.
#[test]
fn a_surged_river_reads_the_same_tape_canonically_and_through_history() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    // Derived from the tape, not guessed: the window has to open somewhere the
    // walk below actually reaches, and far enough in that the two rivers share
    // a real prefix first. A hardcoded instant either sits past the end of the
    // walk - in which case the surge never opens and the test passes against a
    // no-op - or sits at tick zero, where "they agree before the window" says
    // nothing.
    let opens_at = {
        let mut probe = GeneratedSource::new(scalars.clone(), 4242, 1_000, &fp, None);
        let mut ts = 0;
        for _ in 0..2_000 {
            ts = probe.next_tick().expect("infinite tape").ts_event();
        }
        ts
    };
    let mut index = CheckpointIndex::new(
        GeneratedSource::new(scalars.clone(), 4242, 1_000, &fp, None)
            .with_surge(opens_at, 600_000, 10.0, 4.0),
        8_192,
        1_000_000,
    );
    let mut clean = GeneratedSource::new(scalars, 4242, 1_000, &fp, None);

    // Walked in lockstep from the origin. Advancing one side first would make
    // the two disagree from misalignment rather than from the surge, which is a
    // divergence assertion that passes against a no-op window.
    let mut canonical = Vec::with_capacity(10_000);
    let mut first_mismatch = None;
    for _ in 0..10_000 {
        let tick = index.next_tick().expect("infinite tape");
        let rendered = format!("{tick:?}");
        // Both sides unwrapped. Comparing a `TickEvent` against an
        // `Option<TickEvent>` renders `Trade(..)` against `Some(Trade(..))`,
        // which can never be equal - so the divergence guard would report a
        // mismatch on the first tick whatever the water did, and the
        // assertion that the fixture exercises the surge could not fail.
        let clean_tick = clean.next_tick().expect("infinite tape");
        if rendered != format!("{clean_tick:?}") && first_mismatch.is_none() {
            first_mismatch = Some(tick.ts_event());
        }
        canonical.push((tick.ts_event(), rendered));
    }
    let parted_at = first_mismatch.expect("the fixture must exercise the surge");
    assert!(
        parted_at >= opens_at,
        "the two rivers must agree until the window opens at {opens_at}, but they \
         parted at {parted_at}"
    );

    // A history read landing well inside the surge window, resumed off the
    // chain exactly as `build_history_source` does it.
    // A probe whose `ts_event` is unique in the sequence. A parent's quote and
    // its first child legitimately share an instant, so seeking to a shared one
    // lands on whichever of the pair comes first and the comparison below would
    // be about tie order rather than about which tape was replayed.
    let probe = (5_000..canonical.len() - 200)
        .find(|&i| canonical[i].0 != canonical[i - 1].0 && canonical[i].0 != canonical[i + 1].0)
        .expect("an unshared instant in the surged window");
    let target = canonical[probe].0;
    let mut resumed = index.try_source_before(target).expect("a reached target");
    assert!(resumed.clock_ns() < target, "resume is strictly before");
    let mut tick = resumed.next_tick().expect("infinite tape");
    while tick.ts_event() < target {
        tick = resumed.next_tick().expect("infinite tape");
    }
    for expected in &canonical[probe..probe + 200] {
        assert_eq!(
            (tick.ts_event(), format!("{tick:?}")),
            *expected,
            "a history read inside the surge window replayed a different tape"
        );
        tick = resumed.next_tick().expect("infinite tape");
    }
}

/// A positioned source resumes after the tick its checkpoint last consumed, so
/// a reader hunting the last print at or before a target can find an empty
/// residual even though the print exists - the checkpoint ate it. That is not a
/// hypothetical: the sweep's futures marks and its settlement prices are read
/// through exactly this lookup, and a missing settlement mark used to retire the
/// instant silently. The walk-back is the recovery.
#[test]
fn a_checkpoint_that_consumed_the_last_print_still_answers_for_it() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    // A trade whose successor prints strictly later than one nanosecond after
    // it, so a target one nanosecond past it is answered by that trade and by
    // nothing in the residual. The end of a parent's child run is the ordinary
    // case; searching for it rather than hardcoding an instant keeps the
    // fixture honest if the cadence moves.
    let mut probe = GeneratedSource::new(scalars.clone(), 4242, 1_000, &fp, None);
    let mut previous: Option<(u64, Decimal)> = None;
    let (trade_ts, trade_px) = loop {
        let tick = probe.next_tick().expect("infinite tape");
        if let Some((ts, px)) = previous
            && tick.ts_event() > ts + 1
        {
            break (ts, px);
        }
        previous = match tick {
            TickEvent::Trade(trade) => Some((trade.ts_event, trade.price)),
            TickEvent::Quote(_) => None,
        };
    };
    let target = trade_ts + 1;

    // k = 1 puts a checkpoint on every tick, so the strict partition lands
    // exactly on the trade and the residual is guaranteed to be empty. The
    // venue's K of 8192 hits the same shape whenever a checkpoint or a
    // `FlowSurge` control boundary happens to land there.
    let origin = GeneratedSource::new(scalars, 4242, 1_000, &fp, None);
    let mut index = CheckpointIndex::new(origin, 1, 10_000_000);

    let (mut resumed, exhausted) = index
        .try_source_before_target(target, 0)
        .expect("a reachable target");
    assert!(
        !exhausted,
        "the partition point must not be the origin here"
    );
    assert_eq!(
        last_trade_through(&mut resumed, target),
        None,
        "the fixture must exercise a residual the checkpoint already ate"
    );

    let (mut walked_back, _) = index
        .try_source_before_target(target, 1)
        .expect("a reachable target");
    assert_eq!(
        last_trade_through(&mut walked_back, target),
        Some(trade_px),
        "walking back one snapshot must recover the consumed print"
    );
}

fn last_trade_through(source: &mut GeneratedSource, target: u64) -> Option<Decimal> {
    let mut last = None;
    for _ in 0..10_000 {
        let Some(tick) = source.next_tick() else {
            break;
        };
        let TickEvent::Trade(trade) = tick else {
            continue;
        };
        if trade.ts_event > target {
            break;
        }
        last = Some(trade.price);
    }
    last
}

/// The fence is gone and nothing replaced it, because a surge installed before
/// the first draw leaves nothing to fence.
///
/// The old hazard: arming mutated a generator that had already walked, so a
/// snapshot taken before the arm would replay the span after it unsurged. The
/// walk-back had to stop at a pinned control boundary, coarsening had to keep
/// that boundary, and a reader whose print the boundary had eaten needed a
/// separate recovery to avoid refusing forever. All three existed to contain
/// one mutation.
///
/// What has to be true now is stronger and simpler: retreating across the surge
/// window - all the way to the origin, past however many snapshots - must
/// replay the same ticks as resuming just before the target. This is the test
/// that would have failed under the old model without a fence, so it is the
/// one that shows the fence is unnecessary rather than merely absent.
#[test]
fn a_walk_back_across_a_surge_window_stays_on_one_realization() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let opens_at = 5_000_000_000;
    // A dense chain, so a deep retreat really does cross many snapshots and the
    // window rather than landing next door to the target.
    let mut index = CheckpointIndex::new(
        GeneratedSource::new(scalars, 4242, 1_000, &fp, None)
            .with_surge(opens_at, 600_000, 10.0, 4.0),
        64,
        10_000_000,
    );
    index.extend_toward(opens_at.saturating_mul(2));

    // A target well past the window's opening, so every resume below has to
    // replay across it.
    let target = index.frontier_ns();
    let near = {
        let (mut source, _) = index
            .try_source_before_target(target, 0)
            .expect("a reached target");
        drain_from(&mut source, target, 64)
    };
    let (mut far_source, at_origin) = index
        .try_source_before_target(target, usize::MAX)
        .expect("a reached target");
    assert!(
        at_origin,
        "an unbounded retreat must reach the origin, which is the only floor left"
    );
    let far = drain_from(&mut far_source, target, 64);

    assert_eq!(
        near, far,
        "a retreat across the surge window replayed a different tape"
    );
}

/// Coarsening has no exception any more, and this is what that means: a chain
/// carrying a surge coarsens exactly like a clean one. Under the old model two
/// snapshots were pinned against the every-other rule - the origin and the arm
/// boundary - and dropping the boundary would have reopened the realization
/// fork. There is no boundary to keep now.
#[test]
fn coarsening_keeps_no_exception_on_a_surged_chain() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut index = CheckpointIndex::new(
        GeneratedSource::new(scalars, 11, 1_000, &fp, None).with_surge(1_000, 60_000, 8.0, 3.0),
        1,
        10_000_000,
    );

    // Past MAX_CHECKPOINTS at k = 1, so coarsen must have run at least once.
    for _ in 0..MAX_CHECKPOINTS + 500 {
        index.next_tick().expect("infinite tape");
    }
    assert!(index.checkpoint_count() <= MAX_CHECKPOINTS);

    // And the chain still answers consistently after coarsening, which is the
    // property the pin used to buy.
    let target = index.frontier_ns();
    let (mut resumed, _) = index
        .try_source_before_target(target, 0)
        .expect("a reached target");
    let (mut from_origin, at_origin) = index
        .try_source_before_target(target, usize::MAX)
        .expect("a reached target");
    assert!(at_origin);
    assert_eq!(
        drain_from(&mut resumed, target, 32),
        drain_from(&mut from_origin, target, 32),
        "a coarsened surged chain must replay one tape from every resume point"
    );
}

/// Seek `source` to the first tick at or after `target`, then take `count`
/// ticks in their debug form - enough to compare two replays as sequences
/// rather than as single ticks, since a divergence can begin anywhere.
fn drain_from(source: &mut GeneratedSource, target: u64, count: usize) -> Vec<String> {
    let mut tick = source.next_tick().expect("infinite tape");
    while tick.ts_event() < target {
        tick = source.next_tick().expect("infinite tape");
    }
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        out.push(format!("{tick:?}"));
        tick = source.next_tick().expect("infinite tape");
    }
    out
}

const DRAW: usize = 2_000_000;
// Parent events, not prints. The day-of-week assertions below need at least a
// full week of simulated tape and want several, so this is denominated in sim
// span rather than in ticks: at the committed 0.171 s mean event gap, 15M
// events is roughly 30 simulated days. `measure_session_curves` drops each
// event's children rather than emitting them, which is what makes a four-week
// span affordable - see the note there on what that costs.
const SESSION_DRAW: usize = 15_000_000;
// The clean tape's duration ACF is gated by the committed cross-pair band
// (`cadence.targets.duration_acf_lag1` / `_lag5`, measured on microsecond-
// stamped Binance parent events). This absolute tolerance is used at one site
// only - the drought test, which walks a 50,000-gap sample far too small to
// inherit that band and only needs to see that thinning the tape stretches the
// gaps without destroying their serial dependence. 0.14 is a principled
// absolute choice rather than a fitted one: it gives margin for sampling wobble
// on a short draw while still catching a flattened ACF, which collapses the lag
// toward zero, an order of magnitude past 0.14 from the anchor.
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
// (history seek caps, checkpoint spacing, the venue's backfill horizon), and
// it is what keeps ARRIVAL_MEAN_CAL honest.
const DWELL_P999_SLACK: f64 = 2.0;
const EMPTY_HOUR_FRAC_SLACK: f64 = 0.01;
const EMPTY_HOUR_RUN_SLACK_H: f64 = 2.0;
const MEAN_GAP_REL_TOL: f64 = 0.10;
const INTENSITY_SHARE_ABS_TOL: f64 = 0.006;
const SESSION_START_TS: u64 = 1_700_438_400_000_000_000;

#[test]
fn fingerprint_parses() {
    let fp = Fingerprint::from_repo_json();
    assert_eq!(fp.session_profile.intensity_hour.len(), 24);
    assert_eq!(fp.session_profile.dow_weight.len(), 7);
    assert_eq!(fp.golden_targets.abs_return_acf_anchor.len(), 50);
    assert_eq!(fp.golden_targets.dwell.era_start_ts, 1_546_300_800);
    assert_eq!(fp.empirical_ranges.price_decimals.min, 1.0);
    assert_eq!(fp.empirical_ranges.price_decimals.max, 7.0);
}

#[test]
fn scalars_validate() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::from_fingerprint_medians("ETHUSD", &fp);
    assert!(scalars.validate().is_ok());
    let mut bad = scalars.clone();
    bad.mean_event_duration_s = fp.empirical_ranges.mean_event_duration_s.max + 1.0;
    assert!(bad.validate().is_ok());
    assert_eq!(
        bad.empirical_diagnostics(&fp, Decimal::ONE)[0].field,
        "mean_event_duration_s"
    );
}

// `ARRIVAL_MEAN_CAL` corrects the shipped sampling scheme's realized-mean
// inflation, and it must reach that scheme and nothing else. The integrated
// arrival frame draws from an exact time change, which has no such inflation
// to correct, so a correction that leaked into it would be a uniform
// 1/ARRIVAL_MEAN_CAL rate excess on every integrated river - the 2026-08-09
// calibration amendment rests on the frame staying bare. Until this test the
// separation was held by a doc comment and nothing else.
//
// Both expectations are derived from the constant and from the functions
// themselves, so a second copy of the same mistake cannot satisfy them, and
// the comparison is on bits because the claim is exact identity rather than
// closeness: an inherited correction reproduces the same product this test
// forms, bit for bit.
//
// This gates the bare side only. The corrected side is composed inline in
// `GeneratedSource::new` and lands in a private field, so no observable of it
// is reachable from here without replaying a whole generation; a source-side
// accessor for the composed active mean is what would let this test assert
// the other half.
#[test]
fn the_arrival_mean_calibration_stays_off_the_integrated_frame() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let declared = scalars.mean_event_duration_s;
    assert!(declared > 0.0, "the anchor declares a positive mean gap");
    // A calibration of one is unobservable in a product, which would leave
    // every assertion below true whether or not the frame inherited it.
    assert!(
        (ARRIVAL_MEAN_CAL - 1.0).abs() > 1e-6,
        "the calibration must differ from one for this gate to see anything"
    );
    let corrected = ARRIVAL_MEAN_CAL * declared;

    // The one definition both integrated call sites read: the generator's
    // `begin_integrated_event` and `CadenceWalk::assemble`.
    let bare = cadence_base_mean_s(&scalars);
    assert_eq!(
        bare.to_bits(),
        declared.to_bits(),
        "the integrated mean is the declared mean, uncorrected"
    );
    assert_ne!(
        bare.to_bits(),
        corrected.to_bits(),
        "the integrated frame inherited the shipped path's calibration"
    );

    // And the assembly the generator actually constructs, which is what the
    // kernel is driven with - a bare definition wired to a corrected assembly
    // would be the same defect one call away.
    let parts = CadenceWalk::assemble(
        &scalars,
        &fp.session_profile,
        None,
        1.0,
        42,
        SESSION_START_TS,
    );
    assert_eq!(
        parts.base_mean_s.to_bits(),
        declared.to_bits(),
        "the assembled cadence mean is the declared mean, uncorrected"
    );
    assert_ne!(
        parts.base_mean_s.to_bits(),
        corrected.to_bits(),
        "the assembled cadence mean inherited the shipped path's calibration"
    );

    // The corrected half, which nothing reached until `active_mean_s` existed.
    // With `quiet_share` at zero the two arrival states coincide and the
    // composition in `GeneratedSource::new` reduces to the calibration times
    // the declared mean exactly, so this is an identity rather than a
    // tolerance: the calibration reaching the shipped sampler is asserted here
    // as directly as its absence from the integrated frame is asserted above.
    // A calibration silently dropped from the shipped path - the mirror image
    // of the leak this gate was written for - now fails a bit comparison
    // instead of passing unnoticed.
    let mut shipped = scalars.clone();
    shipped.arrival = Some(ArrivalConfig::EventMarkov {
        quiet_share: 0.0,
        switch_rate: 1.0 - ARRIVAL_STATE_PERSISTENCE,
        rate_ratio: ARRIVAL_QUIET_ACTIVE_RATIO,
    });
    let source = GeneratedSource::new(shipped, 42, SESSION_START_TS, &fp, None);
    assert_eq!(
        source.active_mean_s().to_bits(),
        corrected.to_bits(),
        "the shipped sampler's active mean is the calibrated mean, bit for bit"
    );
}

#[test]
fn mean_notional_is_derived_reporting_not_sampler_input() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::from_fingerprint_medians("BTCUSDT", &fp);
    assert_near(
        "derived crypto mean notional",
        scalars.mean_trade_notional(Decimal::ONE),
        fp.cadence.targets.mean_trade_notional.anchor,
        1e-9,
    );

    let mut mnq = scalars;
    mnq.start_price = Decimal::from(21_000);
    mnq.latent_size_median = Decimal::ONE;
    assert_near(
        "derived MNQ mean notional",
        mnq.mean_trade_notional(Decimal::from(2)),
        21_000.0 * 2.0 * (mnq.size_log_sigma.powi(2) / 2.0).exp(),
        1e-9,
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

    // A legitimate closed-session profile sums below 1.0 on intensity (a
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
    assert!(good.validate().is_ok());

    // (d) an omitted symbol serde-defaults to "" and cross-contaminates
    // every symbol-keyed consumer.
    let mut no_symbol = good.clone();
    no_symbol.symbol = String::new();
    assert_eq!(no_symbol.validate(), Err(ScalarError::field("symbol")));

    // (a) modal_tick 1e-7 is in range and price_decimals 1 is in range, but
    // together round_dp(1) silently coarsens the grid to 0.1.
    let mut fine_tick = good.clone();
    fine_tick.modal_tick = Decimal::new(1, 7);
    fine_tick.price_decimals = 1;
    assert_eq!(fine_tick.validate(), Err(ScalarError::field("modal_tick")));

    // (b) start_price outside the [modal_tick, MID_CEILING] clamp band: a
    // value above the ceiling instantly collapses the mid, one below a tick
    // instantly jumps up.
    let mut high_start = good.clone();
    high_start.start_price = Decimal::from(5_000_000_000_i64);
    assert_eq!(
        high_start.validate(),
        Err(ScalarError::field("start_price"))
    );
    let mut low_start = good.clone();
    low_start.start_price = good.modal_tick / Decimal::from(2);
    assert_eq!(low_start.validate(), Err(ScalarError::field("start_price")));

    // (c) vol_scalar above the sigma cap is silently pinned at the cap on
    // the first tick and does nothing in the base regime.
    let mut hot_vol = good.clone();
    hot_vol.vol_scalar = GARCH_SIGMA_CAP * 10.0;
    assert_eq!(hot_vol.validate(), Err(ScalarError::field("vol_scalar")));

    // Equality used to pass even though GARCH then initialized on its cap and
    // clipped every upward shock. The mechanism now reserves headroom.
    let mut capped_vol = good.clone();
    capped_vol.vol_scalar = GARCH_SIGMA_CAP;
    assert_eq!(capped_vol.validate(), Err(ScalarError::field("vol_scalar")));

    // A truthful coarse grid is not rejected merely because its tick return
    // exceeds the module-level clamp. That ratio predicts sticky latent-mid
    // traversal; it is a diagnostic, not an admission failure or a reason to
    // change stressed-tail clipping per instrument.
    let mut unreachable_tick = good.clone();
    unreachable_tick.start_price = Decimal::from(44_000);
    unreachable_tick.modal_tick = Decimal::ONE;
    unreachable_tick.price_decimals = 0;
    assert!(unreachable_tick.validate().is_ok());

    // The fingerprint-median construction (a fine 4-decimal grid) still
    // passes: its modal_tick scale (4) equals price_decimals (4).
    let medians = GeneratorScalars::from_fingerprint_medians("ETHUSD", &fp);
    assert!(medians.validate().is_ok());

    // Whole-number quote grids are structurally representable. Kraken's
    // observed minimum of one decimal place was never a mechanism bound.
    let mut whole_number = good;
    whole_number.modal_tick = Decimal::ONE;
    whole_number.price_decimals = 0;
    whole_number.start_price = Decimal::from(100_000);
    assert!(whole_number.validate().is_ok());
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
    bad_scalars.mean_event_duration_s = f64::NAN;
    assert_eq!(
        GeneratedSource::try_new(bad_scalars, 42, 0, &fp, None).err(),
        Some(GeneratedSourceError::Scalar(ScalarError::field(
            "mean_event_duration_s"
        )))
    );

    // A bad session profile on the explicit-session path is surfaced too: an
    // all-ones intensity curve sums to 24 and is rejected as a whole-array
    // normalization violation.
    let mut bad_session = fp.session_profile.clone();
    bad_session.intensity_hour = [1.0; 24];
    assert_eq!(
        GeneratedSource::try_new_with_session_profile(
            scalars,
            42,
            0,
            &fp,
            &bad_session,
            None,
            SizeGrid::spot(),
            None,
        )
        .err(),
        Some(GeneratedSourceError::Session(SessionProfileError {
            field: "intensity_hour",
            index: usize::MAX,
        }))
    );

    let empty_calendar = SessionCalendar {
        utc_offset_minutes: 0,
        open_windows: Vec::new(),
        settlement_minute_of_day: None,
    };
    assert_eq!(
        GeneratedSource::try_new_with_session_profile(
            GeneratorScalars::xbtusd_anchor(&fp),
            42,
            0,
            &fp,
            &fp.session_profile,
            None,
            SizeGrid::spot(),
            Some(empty_calendar),
        )
        .err(),
        Some(GeneratedSourceError::Calendar(CalendarError(
            "open_windows"
        )))
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
        assert!(tick.ts_event() >= prior);
        prior = tick.ts_event();
    }
}

#[test]
fn on_grid_prices_and_native_aggressor() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    for _ in 0..10_000 {
        let trade = next_trade(&mut src);
        assert_eq!((trade.price / scalars.modal_tick).fract(), Decimal::ZERO);
        assert!(matches!(
            trade.aggressor,
            AggressorSide::Buyer | AggressorSide::Seller
        ));
    }
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
        let trade = next_trade(&mut src);
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
// exact ticks a from-origin run produces - the byte-identical guarantee the
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
            let t = next_trade(&mut reference);
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
    let mut resumed = index.source_before(target);
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
    let mut tick = next_trade(&mut resumed);
    while tick.ts_event < target {
        tick = next_trade(&mut resumed);
    }
    for expected in &ref_ticks[3_000..3_200] {
        assert_eq!(
            (tick.ts_event, tick.price, tick.size, tick.aggressor),
            *expected,
            "resumed tail diverged"
        );
        tick = next_trade(&mut resumed);
    }
}

// Regression for the exact-ts collision the strictly-before partition in
// `source_before` exists for: snapshots land on every K-th tick's
// exact ts_event, and pollers pass an emitted tick's exact ts_event as the
// seek target, so a seek target can equal a checkpoint's clock_ns. Under
// the old `<=` partition the index handed back the checkpoint that had
// already consumed the boundary tick, and the residual seek silently
// skipped to the next tick - one tick dropped versus the from-origin path.
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

    // The 3K-th tick (0-based index 3K-1) is exactly where extend_toward
    // pushes its third interior snapshot, so that snapshot's clock_ns
    // equals this tick's ts_event - the forced collision. The from-origin
    // seek semantics (first tick with ts_event >= target) return this
    // boundary tick itself; the checkpointed path must return the
    // identical tick and stay byte-identical afterwards.
    let boundary = 3 * k - 1;
    let target = ref_ticks[boundary].0;

    let origin = GeneratedSource::new(scalars, seed, origin_ts, &fp, None);
    let mut index = CheckpointIndex::new(origin, k, 100_000);
    let mut resumed = index.source_before(target);
    // Origin plus exactly three interior snapshots: extend_toward stops the
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
// `coarsen` fires, then assert the snapshot count stayed capped and that
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
    let _ = index.source_before(ref_ticks[n - 1].0);
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
        let mut resumed = index.source_before(probe_target);
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
// is what keeps a bogus or far-future `start` (which the venue does not
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
    // walk stops at the bound instead of running forever. A second call can
    // make another bounded chunk, which lets a legitimate boot release its
    // external lock between chunks without weakening this per-call ceiling.
    let unreachable_target = origin_ts + 315_360_000_000_000_000;
    assert_eq!(index.extend_toward(unreachable_target), cap);
    let first_frontier = index.frontier_ns();
    assert_eq!(index.extend_toward(unreachable_target), cap);
    assert!(index.frontier_ns() > first_frontier);
    assert!(index.try_source_before(unreachable_target).is_none());
    // Three bounded calls were made, so at most `3 * cap / K` interior
    // snapshots were taken beyond the origin.
    assert!(
        index.checkpoint_count() <= 1 + 3 * cap / 64 + 1,
        "each bounded walk took at most cap/K interior checkpoints"
    );
}

#[test]
fn clean_regime_is_byte_identical() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars, 42, 1_000, &fp, None);
    let _protocol_six_expected = [
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00016711, aggressor: Buyer, ts_event: 1651680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.002, aggressor: Buyer, ts_event: 1652680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.001, aggressor: Buyer, ts_event: 1653680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00118992, aggressor: Buyer, ts_event: 1654680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00374134, aggressor: Buyer, ts_event: 2085712 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00981338, aggressor: Buyer, ts_event: 2390006 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00632398, aggressor: Buyer, ts_event: 4430072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00219303, aggressor: Buyer, ts_event: 4431072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00064674, aggressor: Buyer, ts_event: 4432072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.009, aggressor: Buyer, ts_event: 4433072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.003, aggressor: Buyer, ts_event: 4434072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00072433, aggressor: Buyer, ts_event: 4435072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00089376, aggressor: Buyer, ts_event: 4436072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.01757230, aggressor: Buyer, ts_event: 4437072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00234238, aggressor: Buyer, ts_event: 4438072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00259664, aggressor: Buyer, ts_event: 4439072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00129664, aggressor: Buyer, ts_event: 4440072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00929062, aggressor: Buyer, ts_event: 4441072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00177211, aggressor: Buyer, ts_event: 4442072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00172264, aggressor: Buyer, ts_event: 4443072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00494222, aggressor: Buyer, ts_event: 4444072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00076565, aggressor: Buyer, ts_event: 4445072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.002, aggressor: Buyer, ts_event: 4446072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.01535550, aggressor: Buyer, ts_event: 4447072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00048139, aggressor: Buyer, ts_event: 4448072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00199492, aggressor: Buyer, ts_event: 4449072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.004, aggressor: Buyer, ts_event: 4450072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00216568, aggressor: Buyer, ts_event: 4451072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.007, aggressor: Buyer, ts_event: 4452072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00078736, aggressor: Buyer, ts_event: 4453072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00070413, aggressor: Buyer, ts_event: 4454072 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.3, size: 0.00191262, aggressor: Buyer, ts_event: 4455072 }))"#,
    ];

    let expected = [
        r#"Some(Quote(QuoteTick { symbol: "XBTUSD", bid_px: 60000.1, ask_px: 60000.2, bid_sz: 0.00000001, ask_sz: 0.00000001, ts_event: 1651680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00016711, aggressor: Buyer, ts_event: 1651680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.002, aggressor: Buyer, ts_event: 1652680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.001, aggressor: Buyer, ts_event: 1653680 }))"#,
        r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.2, size: 0.00118992, aggressor: Buyer, ts_event: 1654680 }))"#,
        r#"Some(Quote(QuoteTick { symbol: "XBTUSD", bid_px: 60000.1, ask_px: 60000.2, bid_sz: 0.00000001, ask_sz: 0.00000001, ts_event: 2085712 }))"#,
    ];
    let actual: Vec<_> = (0..expected.len())
        .map(|_| format!("{:?}", src.next_tick()))
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn vol_storm_lifts_realized_output_without_touching_the_base_process() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let regime = Some(MarketRegime::VolStorm { vol_mult: 100.0 });
    let mut clean = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut lifted = GeneratedSource::new(scalars, 42, 0, &fp, regime);

    let clean_rms = rms(&latent_returns(&mut clean, 50_000));
    let lifted_returns = latent_returns(&mut lifted, 50_000);
    let lifted_rms = rms(&lifted_returns);
    let lifted_max = lifted_returns
        .iter()
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);

    // A storm amplifies output.
    assert!(
        lifted_rms > clean_rms * 50.0,
        "clean_rms={clean_rms} lifted_rms={lifted_rms}"
    );
    // ...and meets the absolute ceiling, which no regime scales. Before the
    // rails were separated this ceiling was the feedback rail times vol_mult,
    // so a maximum-strength storm's permitted single-event move rode on
    // whatever the base rail happened to be.
    assert!(
        lifted_max <= REALIZED_RETURN_CEILING * (1.0 + 1e-12),
        "a storm must not exceed the absolute realized ceiling: lifted_max={lifted_max}"
    );
    // ...and the base process is untouched. The storm reaches the mid only
    // through vol_mult, never through the GARCH state or feedback rails, so
    // after an equal number of updates the recursion state is bit-identical to
    // an unarmed run - which is what makes a divergence an envelope rather than
    // something that changes the tape after it ends.
    assert_eq!(
        clean.vol.sigma2.to_bits(),
        lifted.vol.sigma2.to_bits(),
        "an armed storm must not perturb the GARCH state"
    );
    assert_eq!(
        clean.vol.prev_return.to_bits(),
        lifted.vol.prev_return.to_bits(),
        "an armed storm must not perturb the feedback return"
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
// behavior: thin_factor multiplies only the realized gap, leaving the fitted
// state transitions intact. A constant multiplier leaves realized gap ACF
// invariant, which is what makes that invariant testable here.
#[test]
fn liquidity_drought_imitates_dying_symbol() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    const THIN_FACTOR: f64 = 1000.0;
    let mut src = GeneratedSource::new(
        scalars.clone(),
        42,
        0,
        &fp,
        Some(MarketRegime::LiquidityDrought {
            thin_factor: THIN_FACTOR,
        }),
    );
    let gaps = durations(&mut src, 50_000);
    let mean_gap = mean(&gaps);
    let max_gap = gaps.iter().copied().fold(0.0_f64, f64::max);
    // What this test is about is the ratio the drought applies to the parent
    // gap: `thin_factor` multiplies the clean gap, so the realized mean must
    // land near `thin_factor * mean_event_duration_s`.
    //
    // The window is an ensemble, not a taste. It used to be 0.5x to 2x, on the
    // stated ground that a 50,000-gap draw is noisy - and it is not: the ratio
    // is a systematic quantity, measured 0.8898 to 0.9739 over the run seeds 0
    // through 7 and 42, an 8.4 percent spread with no seed anywhere near the
    // old edges.
    //
    // Why it sits below one: event-weighted sampling, and one site only. A
    // draft of this comment blamed the envelope dividing the arrival rate as
    // well as multiplying the gap - that second site is `low_intensity_gap_ns`
    // and it is gated on `arr_mult < LOW_INTENSITY_ARR_MULT`, which is 0.01,
    // while the committed calendar-free `session_profile` bottoms out at
    // `min intensity_hour * 24 * min dow_weight * 7`, about 0.584. It is
    // unreachable here, so the only site in play is the open-market
    // `duration_s = duration_s / arr_mult * arrival_thin`. Its mean is
    // sampled per event, and busy hours emit more events, so the sample
    // over-represents large `arr_mult` and the mean of `1 / arr_mult` lands
    // under one.
    //
    // The catch set, measured by scaling `thin_factor` while holding
    // `expected_gap` at the unscaled value - which is exactly a production
    // multiplier off by that factor. Because the low-intensity branch is
    // unreachable, the gap really is proportional to `arrival_thin` draw by
    // draw; the linearity is still only statistical, because a scaled gap
    // advances `clock_ns` and so changes which hours the later gaps sample.
    // That feedback is where the 8.4 percent ensemble spread comes from. At the
    // old window: caught below 0.55x and above 2.25x, so a multiplier off by
    // 50 percent upward passed (1.5x reads 1.351, inside 2). At this one:
    // caught below ~0.83x and above ~1.27x, with the nine-seed ensemble still
    // clearing both edges by about 18 percent. A tape change that legitimately
    // moves the envelope re-blesses this number, which is the bump it already
    // owes.
    let expected_gap = THIN_FACTOR * scalars.mean_event_duration_s;
    let gap_ratio = mean_gap / expected_gap;
    assert!(
        (0.75..=1.15).contains(&gap_ratio),
        "mean_gap={mean_gap} expected~{expected_gap} ratio={gap_ratio}"
    );
    assert_near(
        "drought_duration_acf_lag1",
        acf(&gaps, 1),
        fp.cadence.targets.duration_acf_lag1.anchor,
        DURATION_ACF_ABS_TOL,
    );
    assert!(
        max_gap >= 5.0 * mean_gap,
        "max_gap={max_gap} mean_gap={mean_gap}"
    );
}

// A 1M-tick walk: see the heavy-walk cfg note at the top of this file.
#[test]
#[cfg(not(feature = "hotpath"))]
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

    let (in_window, out_window) = windowed_latent_returns(&mut src, 1_000_000, 14, 16);
    let in_rms = rms(&in_window);
    let out_rms = rms(&out_window);
    assert!(in_rms >= out_rms * 2.0, "in_rms={in_rms} out_rms={out_rms}");
}

// Companion to vol_storm_lifts_realized_rms for the other vol regime.
// Pre-fix, SessionEdgeSpike left the realized clamp pinned at 1.0, so a
// large extra_vol_mult saturated every in-window return against
// MAX_ABS_RETURN and the realized spike stopped tracking the requested
// amplification (the existing localizes test uses 6.0, below where the pin
// binds). Post-fix the realized clamp is lifted in-window by
// (1.0 + extra_vol_mult), so a near-ceiling extra_vol_mult both breaks the
// old MAX_ABS_RETURN ceiling and scales the in-window RMS with the knob.
//
// Two 1M-tick walks: see the heavy-walk cfg note at the top of this file.
#[test]
#[cfg(not(feature = "hotpath"))]
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

    let (big_in, _) = windowed_latent_returns(&mut big_src, 1_000_000, start_hour, end_hour);
    let (small_in, _) = windowed_latent_returns(&mut small_src, 1_000_000, start_hour, end_hour);
    let big_in_rms = rms(&big_in);
    let small_in_rms = rms(&small_in);
    let big_in_max = big_in
        .iter()
        .map(|value| value.abs())
        .fold(0.0_f64, f64::max);

    // An edge spike obeys the same absolute ceiling as every other regime; it
    // no longer carries a ceiling lift of its own.
    assert!(
        big_in_max <= REALIZED_RETURN_CEILING * (1.0 + 1e-12),
        "an edge spike must not exceed the absolute realized ceiling: big_in_max={big_in_max}"
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
    for _ in 0..100_000 {
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
// at construction (fail closed, with a tracing warning) and draws no RNG,
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
        SizeGrid::spot(),
        None,
    );
    let first = next_trade(&mut src).ts_event;
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
        SizeGrid::spot(),
        None,
    );
    assert_eq!(next_trade(&mut twin).ts_event, first);
    let mut prior = first;
    for _ in 0..5_000 {
        let tick = next_trade(&mut src);
        let twin_tick = next_trade(&mut twin);
        assert_eq!(format!("{tick:?}"), format!("{twin_tick:?}"));
        assert!(tick.ts_event > prior, "clock stalled at {prior}");
        prior = tick.ts_event;
    }
}

// The degenerate extreme: every hour closed so hard the budget can never
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
        SizeGrid::spot(),
        None,
    );
    let mut prior = SESSION_START_TS;
    for _ in 0..300 {
        let ts = next_trade(&mut src).ts_event;
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

fn mnq_size_grid() -> SizeGrid {
    SizeGrid {
        multiplier: Decimal::from(2),
        integral: true,
        min_size: Decimal::ONE,
    }
}

fn cme_calendar() -> SessionCalendar {
    let mut open_windows = Vec::new();
    for day in 0..5 {
        let session_start = if day == 0 { 1_080 } else { day * 1_440 + 1_080 };
        let next_day = (day + 1) * 1_440;
        open_windows.push(WeeklyWindow {
            start_minute: session_start,
            end_minute: next_day + 975,
        });
        open_windows.push(WeeklyWindow {
            start_minute: next_day + 990,
            end_minute: next_day + 1_020,
        });
    }
    SessionCalendar {
        utc_offset_minutes: 0,
        open_windows,
        settlement_minute_of_day: Some(960),
    }
}

const DAY_NS: u64 = 86_400_000_000_000;
const MINUTE_NS: u64 = 60_000_000_000;
const FIRST_SUNDAY_NS: u64 = 3 * DAY_NS;

fn calendar_source(start: u64) -> GeneratedSource {
    let fp = Fingerprint::from_repo_json();
    GeneratedSource::new_with_session_profile(
        GeneratorScalars::xbtusd_anchor(&fp),
        42,
        start,
        &fp,
        &fp.session_profile,
        None,
        SizeGrid::spot(),
        Some(cme_calendar()),
    )
}

#[test]
fn a_calendar_weekend_emits_no_tick_and_reopens_exactly_on_the_boundary() {
    let friday_close = FIRST_SUNDAY_NS + (5 * 1_440 + 1_020) * MINUTE_NS;
    let sunday_reopen = FIRST_SUNDAY_NS + 7 * DAY_NS + 1_080 * MINUTE_NS;
    let mut source = calendar_source(friday_close - 1);
    let mut timestamps = Vec::new();
    for _ in 0..1_000 {
        let ts = source.next_tick().unwrap().ts_event();
        timestamps.push(ts);
        if ts >= sunday_reopen {
            break;
        }
    }
    assert!(
        timestamps
            .iter()
            .all(|ts| *ts < friday_close || *ts >= sunday_reopen)
    );
    assert_eq!(
        timestamps.into_iter().find(|ts| *ts >= sunday_reopen),
        Some(sunday_reopen)
    );
}

#[test]
fn reopen_gap_inside_calendar_closure_is_consumed_at_the_jump() {
    let friday_close = FIRST_SUNDAY_NS + (5 * 1_440 + 1_020) * MINUTE_NS;
    let sunday_reopen = FIRST_SUNDAY_NS + 7 * DAY_NS + 1_080 * MINUTE_NS;
    let halt_ns = 60 * MINUTE_NS;
    let fp = Fingerprint::from_repo_json();
    let mut source = GeneratedSource::new_with_session_profile(
        GeneratorScalars::xbtusd_anchor(&fp),
        42,
        friday_close - 1,
        &fp,
        &fp.session_profile,
        Some(MarketRegime::ReopenGap {
            at_ts: friday_close + MINUTE_NS,
            halt_secs: halt_ns / 1_000_000_000,
            gap_frac: 0.05,
        }),
        SizeGrid::spot(),
        Some(cme_calendar()),
    );

    assert_eq!(next_trade(&mut source).ts_event, sunday_reopen + halt_ns);
}

// The other end of the same family. Children step the clock past their parent,
// so an arm whose instant falls inside a burst was never covered by any tested
// span: the burst's own event tested up to the parent, and the next event
// started from the last child, already past the arm. The crossing frontier is
// its own field precisely so those spans are contiguous, and this places an arm
// one nanosecond after a multi-child parent - the middle of the old hole.
#[test]
fn a_reopen_gap_armed_inside_a_burst_still_fires() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let halt_ns = 86_400_000_000_000;

    let mut clean = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut at_ts = None;
    for _ in 0..512 {
        let parent = clean.advance_parent().expect("clean parent");
        if parent.child_count >= 2 {
            at_ts = Some(parent.parent_ts_ns + 1);
            break;
        }
    }
    let at_ts = at_ts.expect("a multi-child parent inside 512 events");

    let mut src = GeneratedSource::new(
        scalars,
        42,
        0,
        &fp,
        Some(MarketRegime::ReopenGap {
            at_ts,
            halt_secs: halt_ns / 1_000_000_000,
            gap_frac: 0.05,
        }),
    );
    let mut prior_ts = 0;
    let mut halts = 0;
    for _ in 0..100_000 {
        let tick = src.next_tick().expect("unbounded generated source");
        if tick.ts_event().saturating_sub(prior_ts) >= halt_ns {
            halts += 1;
        }
        prior_ts = tick.ts_event();
    }
    assert_eq!(halts, 1, "an arm inside a burst must still halt once");
}

#[test]
fn the_maintenance_halt_is_expressible_at_sub_hour_resolution() {
    let monday = FIRST_SUNDAY_NS + DAY_NS;
    let close = monday + 975 * MINUTE_NS;
    let reopen = monday + 990 * MINUTE_NS;
    let mut source = calendar_source(close - MINUTE_NS);
    let ticks: Vec<_> = (0..10_000)
        .map(|_| source.next_tick().unwrap().ts_event())
        .take_while(|ts| *ts <= reopen)
        .collect();
    assert!(ticks.iter().all(|ts| *ts < close || *ts >= reopen));
}

#[test]
fn a_surged_river_cannot_shorten_a_closure_jump() {
    let friday_close = FIRST_SUNDAY_NS + (5 * 1_440 + 1_020) * MINUTE_NS;
    let sunday_reopen = FIRST_SUNDAY_NS + 7 * DAY_NS + 1_080 * MINUTE_NS;
    let mut source = calendar_source(friday_close - 1).with_surge(
        friday_close - 1,
        4 * 24 * 60 * 60 * 1_000,
        100.0,
        1.0,
    );
    for _ in 0..1_000 {
        let ts = source.next_tick().unwrap().ts_event();
        assert!(ts < friday_close || ts >= sunday_reopen);
        if ts >= sunday_reopen {
            return;
        }
    }
    panic!("source never reached the reopen");
}

#[test]
fn checkpoint_resume_across_a_closure_is_byte_identical() {
    let friday_close = FIRST_SUNDAY_NS + (5 * 1_440 + 1_020) * MINUTE_NS;
    let sunday_reopen = FIRST_SUNDAY_NS + 7 * DAY_NS + 1_080 * MINUTE_NS;
    let target = friday_close + DAY_NS;
    let mut reference = calendar_source(friday_close - 1);
    let mut expected = Vec::new();
    while expected.len() < 200
        || expected
            .last()
            .is_none_or(|tick: &(u64, Decimal, Decimal, AggressorSide)| tick.0 < sunday_reopen)
    {
        let tick = next_trade(&mut reference);
        expected.push((tick.ts_event, tick.price, tick.size, tick.aggressor));
    }
    let origin = calendar_source(friday_close - 1);
    let mut index = CheckpointIndex::new(origin, 8, 100_000);
    let mut resumed = index.source_before(target);
    let start = expected.iter().position(|tick| tick.0 >= target).unwrap();
    for expected_tick in expected.iter().skip(start).take(100) {
        let tick = next_trade(&mut resumed);
        assert_eq!(
            (tick.ts_event, tick.price, tick.size, tick.aggressor),
            *expected_tick
        );
    }
}

#[test]
fn assert_calendar_dwell_excludes_closed_hours() {
    let calendar = cme_calendar();
    let mut source = calendar_source(FIRST_SUNDAY_NS + 4 * DAY_NS);
    let timestamps: Vec<_> = (0..50_000)
        .map(|_| source.next_tick().unwrap().ts_event())
        .collect();
    assert!(
        timestamps
            .iter()
            .all(|timestamp| calendar.is_open(*timestamp))
    );
    let open_gaps: Vec<_> = timestamps
        .windows(2)
        .filter(|pair| calendar.is_open(pair[0].saturating_add(MINUTE_NS)))
        .map(|pair| pair[1] - pair[0])
        .collect();
    assert!(open_gaps.iter().all(|gap| *gap < 60 * MINUTE_NS));
}

#[test]
fn contract_sizes_are_whole_numbers_and_never_zero() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.start_price = Decimal::from(21_000);
    scalars.latent_size_median = Decimal::from(2);
    scalars.top_sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
    let mut source =
        GeneratedSource::try_new_with_size_grid(scalars, 73, 0, &fp, None, mnq_size_grid())
            .expect("MNQ grid is valid");
    for _ in 0..100_000 {
        let tick = next_trade(&mut source);
        assert!(tick.size >= Decimal::ONE);
        assert_eq!(tick.size.fract(), Decimal::ZERO);
    }
}

#[test]
fn contract_size_sampler_consumes_the_declared_latent_median_directly() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.start_price = Decimal::from(21_000);
    scalars.latent_size_median = Decimal::new(25, 1);
    scalars.top_sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
    let source = GeneratedSource::try_new_with_size_grid(scalars, 1, 0, &fp, None, mnq_size_grid())
        .expect("MNQ grid is valid");
    assert_near(
        "contract latent size median",
        source.size_median,
        2.5,
        1e-12,
    );
}

#[test]
fn a_lot_snap_on_an_integral_grid_never_produces_a_fractional_lot() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.latent_size_median = Decimal::ONE;
    scalars.top_sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
    let mut source =
        GeneratedSource::try_new_with_size_grid(scalars, 9, 0, &fp, None, mnq_size_grid())
            .expect("integral lot grid is valid");
    for _ in 0..10_000 {
        assert_eq!(source.next_size().fract(), Decimal::ZERO);
    }
}

#[test]
fn integral_lot_tracks_latent_median_decades() {
    assert_eq!(integral_lot(0.001), 1.0);
    assert_eq!(integral_lot(1.0), 1.0);
    assert_eq!(integral_lot(9.999), 1.0);
    assert_eq!(integral_lot(10.0), 10.0);
    assert_eq!(integral_lot(25.0), 10.0);
    assert_eq!(integral_lot(100.0), 100.0);
}

#[test]
fn size_grid_separates_unit_mismatch_from_thin_truthful_sizes() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.latent_size_median = Decimal::new(1337, 6);
    assert_eq!(
        scalars.validate_size_grid(Decimal::ONE),
        Err(ScalarError::field("latent_size_median"))
    );

    scalars.latent_size_median = Decimal::new(5, 1);
    assert!(scalars.validate_size_grid(Decimal::ONE).is_ok());
    assert!(scalars.size_diagnostics(Decimal::ONE, true).is_empty());

    // A coherent but extremely bottom-heavy latent law is admitted and warned,
    // rather than conflated with the unit mismatch above.
    scalars.latent_size_median = Decimal::new(5, 2);
    assert!(scalars.validate_size_grid(Decimal::ONE).is_ok());
    assert_eq!(
        scalars.size_diagnostics(Decimal::ONE, true)[0].code,
        "quantized-size-variation-below-one-percent"
    );
}

#[test]
fn tick_traversal_uses_grid_return_over_unconditional_sigma() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.start_price = Decimal::from(21_000);
    scalars.modal_tick = Decimal::new(25, 2);
    scalars.price_decimals = 2;
    let traversal = scalars.tick_traversal();
    assert_near(
        "MNQ tick return",
        traversal.tick_return,
        1.190_469_105e-5,
        1e-12,
    );
    // Was 11.90 when `vol_scalar` was 1e-6. The GARCH repair re-solved it to
    // 1.2e-5, so an MNQ-shaped grid now sits at roughly one sigma per tick
    // rather than twelve. That is a large change in how the tape meets its
    // grid, and it is the quantity `zero_change_frac` is governed by - which is
    // exactly why traversal is reported by the realism gate rather than left
    // implicit.
    assert_near(
        "MNQ sigma units",
        traversal.sigma_units,
        0.992_057_587,
        1e-9,
    );
    assert_near(
        "MNQ random-walk updates",
        traversal.expected_updates_per_tick,
        traversal.sigma_units.powi(2),
        f64::EPSILON,
    );
}

#[test]
fn the_integral_floor_lifts_the_realized_mean_above_the_notional_target() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.start_price = Decimal::from(21_000);
    let target = 20_000.0 / (21_000.0 * 2.0);
    scalars.latent_size_median =
        decimal_from_f64(target / (scalars.size_log_sigma.powi(2) / 2.0).exp());
    scalars.top_sizes = TopOfBookSizes::uncalibrated(Decimal::ONE);
    let mut source =
        GeneratedSource::try_new_with_size_grid(scalars, 81, 0, &fp, None, mnq_size_grid())
            .expect("MNQ grid is valid");
    let draws = 100_000;
    let realized = (0..draws)
        .map(|_| decimal_to_f64(source.next_size()))
        .sum::<f64>()
        / f64::from(draws);
    // `realized > target` alone cannot fail: the floor is one contract and the
    // target here is 0.476 of one, so any grid passes it, including a broken
    // one that returns the floor for every draw. What is pinned instead is the
    // measured ratio, so a later change to the rounding rule has to re-bless
    // the number rather than move it silently. Measured, because this comment
    // used to claim a truncating grid "reads 1.00, all mass on the floor",
    // which is not what happens: substituting `RoundingStrategy::ToZero` for
    // the shipped `MidpointAwayFromZero` in `materialize_size` reads 2.2642
    // and fails this assertion by 0.012 past its window. The floor is doing
    // most of the lifting either way - what separates the two is the mass
    // between one and two contracts, not the mass at the floor - so the
    // window's bite against a rounding change is thin and is stated rather
    // than assumed.
    let ratio = realized / target;
    assert_near("integral floor lift", ratio, 2.326, 0.05);
}

#[test]
fn spot_draws_are_bit_identical_across_the_size_grid_change() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut default_path = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut gridded =
        GeneratedSource::try_new_with_size_grid(scalars, 42, 0, &fp, None, SizeGrid::spot())
            .expect("spot grid is valid");
    let default_draws: Vec<_> = (0..100_000).map(|_| default_path.next_size()).collect();
    let gridded_draws: Vec<_> = (0..100_000).map(|_| gridded.next_size()).collect();
    // The two constructors agreeing proves only that `new` delegates with
    // `SizeGrid::spot()`, which is a tautology - both run the same code. The
    // claim the landing actually makes is that the spot draws are the ones the
    // pre-grid generator produced, and only a golden can carry it. These are
    // the first twelve draws of seed 42 on the committed fingerprint, and they
    // are the pre-grid values: the spot arm of `next_size` is character
    // identical to the pre-grid expression, while the direct native-unit
    // latent median is the same calibrated value. Bit identity is provable
    // rather than merely asserted, and this golden keeps it provable across
    // the next edit to either arm.
    const PRE_GRID_HEAD: [&str; 12] = [
        "0.00289574",
        "0.00361598",
        "0.00124550",
        "0.00334147",
        "0.001",
        "0.02185139",
        "0.002",
        "0.00272721",
        "0.00085280",
        "0.00118992",
        "0.00643059",
        "0.003",
    ];
    assert_eq!(default_draws, gridded_draws);
    let head: Vec<String> = default_draws
        .iter()
        .take(PRE_GRID_HEAD.len())
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(head, PRE_GRID_HEAD, "the spot grid moved the spot draws");
}

// A 2M-parent-event walk: see the heavy-walk cfg note at the top of this file.
#[test]
#[cfg(not(feature = "hotpath"))]
fn realism() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let measured = measure(&mut src, &scalars, DRAW);
    assert_in_range(
        "duration_dispersion_cv2",
        measured.duration_dispersion_cv2,
        &fp.cadence.targets.duration_dispersion_cv2.range,
    );
    assert_in_range(
        "duration_acf_lag1",
        measured.duration_acf_lag1,
        &fp.cadence.targets.duration_acf_lag1.range,
    );
    assert_in_range(
        "duration_acf_lag5",
        measured.duration_acf_lag5,
        &fp.cadence.targets.duration_acf_lag5.range,
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
        &fp.empirical_ranges.size_round_frac,
    );
    assert!(measured.size_cv > 0.5, "size_cv={}", measured.size_cv);
    assert_eq!(measured.off_grid_prices, 0);
    assert_eq!(measured.neutral_aggressors, 0);
    let density = &fp.cadence.targets.per_second_counts;
    assert!(
        (measured.fills_per_second_mean / density.mean - 1.0).abs() <= 0.10,
        "fills_per_second_mean={}",
        measured.fills_per_second_mean
    );
    assert!(
        (density.median.saturating_sub(1)..=density.median + 1)
            .contains(&measured.fills_per_second_median),
        "fills_per_second_median={}",
        measured.fills_per_second_median
    );
    assert!(
        (density.p95 / 2..=density.p95 * 2).contains(&measured.fills_per_second_p95),
        "fills_per_second_p95={}",
        measured.fills_per_second_p95
    );
    assert!(
        (measured.zero_second_frac - density.zero_frac).abs() <= 0.05,
        "zero_second_frac={}",
        measured.zero_second_frac
    );
    assert!(
        measured.truncation_frac < 1e-5,
        "truncation_frac={}",
        measured.truncation_frac
    );
    assert_dwell_is_bounded(&measured, &scalars, &fp);
}

#[test]
fn sweep_structure() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut total_children = 0_u64;
    let mut singles = 0_u64;
    let mut total_levels = 0_u64;
    let mut prior_ts = 0_u64;
    for _ in 0..5_000 {
        assert_eq!(src.burst.remaining, 0);
        let updates_before = src.latent_updates;
        let quote = src.next_tick().expect("quote");
        assert!(matches!(quote, TickEvent::Quote(_)));
        let first = next_trade(&mut src);
        assert_eq!(src.latent_updates, updates_before + 1);
        let parent_ts = first.ts_event;
        let side = first.aggressor;
        let mut previous_price = first.price;
        let mut children = 1_u64;
        let mut levels = 1_u64;
        assert!(parent_ts > prior_ts);
        while src.burst.remaining > 0 {
            let child = next_trade(&mut src);
            assert_eq!(child.aggressor, side);
            assert_eq!(
                child.ts_event,
                parent_ts + children * super::consts::INTRA_EVENT_STEP_NS
            );
            match side {
                AggressorSide::Buyer => assert!(child.price >= previous_price),
                AggressorSide::Seller => assert!(child.price <= previous_price),
                AggressorSide::NoAggressor => unreachable!(),
            }
            if child.price != previous_price {
                levels += 1;
            }
            previous_price = child.price;
            children += 1;
        }
        assert_eq!(src.latent_updates, updates_before + 1);
        total_children += children;
        total_levels += levels;
        singles += u64::from(children == 1);
        prior_ts = parent_ts + (children - 1) * super::consts::INTRA_EVENT_STEP_NS;
    }
    let events = 5_000.0;
    assert!((total_children as f64 / events / scalars.children_mean - 1.0).abs() < 0.10);
    assert!((singles as f64 / events - scalars.children_single_frac).abs() < 0.05);
    assert!((total_levels as f64 / events / scalars.levels_mean - 1.0).abs() < 0.15);
}

// A snapshot taken mid-sweep must restore mid-sweep. `SweepBurst` lives in
// generator state exactly so this holds, and the failure mode is invisible
// outside the seek path: a burst that did not survive a snapshot would only
// show up as a seek returning a tape that re-opened its parent. Asserted
// through the real `CheckpointIndex`, not through `Clone`, because `Clone` is
// the substrate the index is built on and would pass no matter what the index
// did with it.
#[test]
fn checkpoint_resume_mid_sweep_is_byte_identical() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let origin_ts = 1_000u64;
    let seed = 42u64;
    let mut reference = GeneratedSource::new(scalars.clone(), seed, origin_ts, &fp, None);
    let mut ticks = Vec::with_capacity(4_000);
    let mut mid_sweep = Vec::with_capacity(4_000);
    for _ in 0..4_000 {
        let opens_event = reference.burst.remaining == 0;
        let t = next_trade(&mut reference);
        // Mid-sweep means: this child did not open its event AND its event is
        // not finished, so a snapshot at this instant carries `remaining > 0`.
        mid_sweep.push(!opens_event && reference.burst.remaining > 0);
        ticks.push((t.ts_event, t.price, t.size, t.aggressor));
    }
    let at = (1_000..3_000)
        .find(|i| mid_sweep[*i])
        .expect("a 4000-tick draw at ~8.5 children per event holds a mid-sweep child");
    let target = ticks[at].0;

    let origin = GeneratedSource::new(scalars, seed, origin_ts, &fp, None);
    let mut index = CheckpointIndex::new(origin, 7, 100_000);
    let mut resumed = index.source_before(target);
    assert!(index.checkpoint_count() > 1);
    let mut tick = next_trade(&mut resumed);
    while tick.ts_event < target {
        tick = next_trade(&mut resumed);
    }
    for expected in &ticks[at..at + 200] {
        assert_eq!(
            (tick.ts_event, tick.price, tick.size, tick.aggressor),
            *expected,
            "resumed tail diverged after a mid-sweep restore"
        );
        tick = next_trade(&mut resumed);
    }
}

// A halt lands between events, never inside one: the `ReopenGap` crossing check
// sits in `begin_event`, so a sweep is an atomic exchange action. The failure
// this pins is a burst whose children straddle the halt, which is a state no
// venue produces.
#[test]
fn a_reopen_gap_never_splits_a_sweep() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let at_ts = 400_000_000_000;
    let halt_ns = 86_400_000_000_000;
    let mut src = GeneratedSource::new(
        scalars,
        42,
        0,
        &fp,
        Some(MarketRegime::ReopenGap {
            at_ts,
            halt_secs: halt_ns / 1_000_000_000,
            gap_frac: 0.05,
        }),
    );
    let mut crossings = 0;
    let mut prior_ts = 0;
    let mut event_start_ts = 0;
    for _ in 0..200_000 {
        let opens_event = src.burst.remaining == 0;
        let tick = src.next_tick().expect("unbounded generated source");
        if opens_event {
            event_start_ts = tick.ts_event();
        }
        if tick.ts_event().saturating_sub(prior_ts) >= halt_ns {
            crossings += 1;
            assert!(
                opens_event,
                "the halt landed inside a sweep that opened at {event_start_ts}"
            );
        }
        prior_ts = tick.ts_event();
    }
    assert_eq!(crossings, 1, "crossings={crossings}");
}

// The two state-conditional sweep multipliers must reproduce the declared
// unconditional child mean exactly, or the mean the fingerprint states and the
// mean the tape realizes are two different numbers. The active multiplier is
// therefore not a free constant - it is determined by the quiet share and the
// quiet multiplier, and this pins the committed literal to that identity.
#[test]
fn arrival_children_mults_preserve_the_declared_mean() {
    use super::consts::{
        ARRIVAL_ACTIVE_CHILDREN_MULT, ARRIVAL_QUIET_CHILDREN_MULT, ARRIVAL_QUIET_FRACTION,
    };
    let blended = ARRIVAL_QUIET_FRACTION * ARRIVAL_QUIET_CHILDREN_MULT
        + (1.0 - ARRIVAL_QUIET_FRACTION) * ARRIVAL_ACTIVE_CHILDREN_MULT;
    assert!((blended - 1.0).abs() < 1e-12, "blended={blended}");
    let derived = (1.0 - ARRIVAL_QUIET_FRACTION * ARRIVAL_QUIET_CHILDREN_MULT)
        / (1.0 - ARRIVAL_QUIET_FRACTION);
    assert!(
        (derived - ARRIVAL_ACTIVE_CHILDREN_MULT).abs() < 1e-12,
        "derived={derived}"
    );
}

// The only dwell gate, and it is a multi-seed one. It replaced a pair: this
// test, which was `#[ignore]`d and in the runner's `skip` list on the claim
// that it "outlives the 20-second per-test hang watchdog by design", and
// `run_seeded_tape_dwell_is_bounded`, which walked seed 42 alone at the full
// `DRAW` and ran on every lane. The claim was false - the eight arms are
// `DRAW / 8` apiece, the same 2M parent events in total as `realism`, measured
// at 6.54 s - so the repo's only evidence that the dwell band holds across
// seeds was the one test nothing ever ran, while the one that did run could
// only ever say "seed 42 passes".
//
// Seed 42 stays in the loop, not dropped with the test that carried it: it is the
// default run seed (`RunSeeds::from_run_seed(42)`), i.e. the tape a bare
// `mogwai serve` actually produces, so a sweep of the first eight integers that
// excluded it would have traded the shipped realization for eight unshipped
// ones. The arms are spelled out below and there are eight of them:
// 0, 1, 2, 3, 4, 5, 6 and 42. Seed 7, which the pre-round sweep of the first
// eight integers covered, is deliberately dropped - the arm count is what
// multiplies `DRAW / 8` back to the two million parent events the wall-clock
// argument above rests on, so 42 displaces 7 rather than joining it. The list
// is written out rather than expressed as a range plus a chain because every
// prose statement of the old form was read as nine arms.
//
// What the shorter per-seed draw costs, measured rather than argued, seed by
// seed against each bound:
//   - `mean_gap_s`, the only two-sided band: 0.1743-0.1785 across the eight
//     `DRAW / 8` arms against a declared 0.17104 and a +/-10% window, versus
//     0.17426 on the old full-draw seed-42 arm. The short arms sit further from
//     the declared mean, so the band's bite did not soften.
//   - `gap_p999_s`: 2.92-3.03 short, 3.18 full, bound 10.65. A defect must
//     stretch the tail 3.55x rather than 3.35x to be caught - the one place the
//     shorter draw is measurably weaker, 6%, bought with seven extra
//     realizations.
//   - `empty_hour_frac` and `max_empty_hour_run_h` came out at exactly 0 on
//     every arm at both draws. That is not vacuity: they are one-sided bounds
//     against silence, and a tape at a 0.17 s mean gap has no empty hour to
//     find. Bite-checked at the short draw by injecting a 30,000x
//     `LiquidityDrought` into this helper - `empty_hour_frac` fails at 0.765
//     against its 0.0105 bound.
//     One assertion is genuinely weaker at the short draw and it is
//     `max_empty_hour_run_h`. Its bound is the anchor plus 2 h, i.e. 7 h, and a
//     `DRAW / 8` arm spans only ~12 h (250k events at 0.174 s), so it can fire
//     only on a tape that is dead for eight of its eleven complete hour
//     buckets, where the full draw spans ~4 days and would catch an eight-hour
//     blackout anywhere in it. It costs the test nothing, because
//     `empty_hour_frac` is asserted first and dominates it at both draws: one
//     empty hour in eleven is 0.09, already eight times the bound, so every
//     blackout `max_empty_hour_run_h` would have named is caught one line
//     earlier by name. What is lost is the two-sidedness of the statement, not
//     the coverage: at ~11 complete hour buckets the resolution of
//     `empty_hour_frac` is 1/11 = 0.09 against a 0.0105 bound, so the assertion
//     has degenerated to "no empty hour at all". It was already effectively
//     that at the full draw - ~96 buckets is a resolution of 0.0104 against
//     0.0105 - so nothing is given up here, but read it as the binary guard it
//     is rather than as a measurement of a fraction.
//
// An eight-arm 2M-parent-event walk: see the heavy-walk cfg note at the top of
// this file. Run it alone with the workspace runner AGENTS.md names:
//   test -p mogwai-data dwell_is_bounded_across_run_seeds --debug
// That takes about 6.5 s per sweep and needs no `--timeout` raise - the old
// hint here asked for `--timeout 280` on a walk that never needed it. The
// runner runs every sweep of the profile, and for this crate it reports
// skip with a reason for the `instrumented` sweep, which is the `cfg` above
// doing its job rather than a silent zero-match pass.
#[test]
#[cfg(not(feature = "hotpath"))]
fn dwell_is_bounded_across_run_seeds() {
    for run_seed in [0, 1, 2, 3, 4, 5, 6, 42] {
        assert_run_seed_dwell_is_bounded_with_draw(run_seed, DRAW / 8);
    }
}

/// No two trades on one river share a `ts_event`, and a consumer's correctness
/// rests on it even though nothing here said so until this test.
///
/// The history endpoints page by row count and the cursor that walks them is
/// timestamp-only, so a page cut in the middle of an instant cannot be advanced
/// past without either losing rows or repeating them. `mogwai-adapter` handles
/// that honestly - it keeps the complete prefix and re-requests from the
/// trailing group's own timestamp - but the case where the whole page shares one
/// timestamp has no complete prefix and no safe advance, so it can only stop and
/// report a short history. A bar-folding consumer cannot distinguish that from a
/// window that legitimately ended, which is how it reaches a strategy: as a
/// silently short warmup rather than an error. broadarrow filed exactly that
/// hazard on 2026-08-18.
///
/// It is unreachable, and this test is what keeps it unreachable. Children are
/// stamped `parent + emitted * INTRA_EVENT_STEP_NS` at a stride of 1 us, and the
/// arrival kernel floors a parent's advance at the same stride, so the trade
/// stream is strictly increasing until the u64 nanosecond epoch is exhausted
/// (~580 years). Measured over a real walk before this landed: 1.1 M BTCUSDT
/// trades, zero ties, smallest gap 27 ns.
///
/// So the assertion is deliberately strict rather than non-decreasing. A change
/// that made ties possible - collapsing the child stride, stamping a burst at
/// one instant - would be a legitimate-looking generator change that breaks a
/// consumer nowhere near it, and this is the only thing that would say so. The
/// quote stream ties with its first child by design and that is fine: trades
/// and quotes are separate pages with separate cutoffs, so a tie across them
/// can never cut either. Quote against quote is gated separately, by
/// `a_river_never_prints_two_quotes_at_one_instant`.
///
/// What depends on this, named so a future generator change can see the cost:
/// socket history paginates by an inclusive per-kind cutoff, and a consumer
/// splices its buffered live rows onto a completed history by dropping those at
/// or below that cutoff. Two trades at one instant across a page boundary
/// therefore lose a row or duplicate one, with neither side able to tell which.
/// A generator change that permits trade ties owes the wire a splice identity
/// stronger than a timestamp, in the same landing, on top of the tape-version
/// bump it already owes.
#[test]
fn a_river_never_prints_two_trades_at_one_instant() {
    let fp = Fingerprint::from_repo_json();
    let def = mogwai_protocol::default_instruments()
        .into_iter()
        .find(|def| def.symbol.as_ref() == "BTCUSDT")
        .expect("the default instrument set carries BTCUSDT");
    let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, &fp);
    scalars.modal_tick = def.price_increment;
    scalars.price_decimals = u32::from(def.price_precision);
    let seed = mogwai_protocol::RunSeeds::from_run_seed(42).tape_for("BTCUSDT");
    let mut src = GeneratedSource::new(scalars, seed, 0, &fp, None);

    let mut prior: Option<u64> = None;
    let mut trades = 0_u32;
    // Enough to cross many parent bursts at the fitted child counts, without
    // making this one of the multi-second walks the gate already carries.
    while trades < 200_000 {
        let Some(tick) = src.next_tick() else { break };
        let TickEvent::Trade(trade) = tick else {
            continue;
        };
        if let Some(prior) = prior {
            assert!(
                trade.ts_event > prior,
                "two trades share instant {prior}: a history page can be cut inside it, and a \
                 consumer paging by a timestamp cursor then reports a silently short window"
            );
        }
        prior = Some(trade.ts_event);
        trades += 1;
    }
    assert_eq!(
        trades, 200_000,
        "the walk produced the trades it was asked for"
    );
}

#[cfg(not(feature = "hotpath"))]
fn assert_run_seed_dwell_is_bounded_with_draw(run_seed: u64, draw: usize) {
    let fp = Fingerprint::from_repo_json();
    let def = mogwai_protocol::default_instruments()
        .into_iter()
        .find(|def| def.symbol.as_ref() == "BTCUSDT")
        .expect("the default instrument set carries BTCUSDT");
    let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, &fp);
    scalars.modal_tick = def.price_increment;
    scalars.price_decimals = u32::from(def.price_precision);
    let seed = mogwai_protocol::RunSeeds::from_run_seed(run_seed).tape_for("BTCUSDT");
    let mut src = GeneratedSource::new(scalars.clone(), seed, 0, &fp, None);
    let measured = measure(&mut src, &scalars, draw);
    assert_dwell_is_bounded(&measured, &scalars, &fp);
}

fn assert_dwell_is_bounded(measured: &Measured, scalars: &GeneratorScalars, fp: &Fingerprint) {
    let dwell = &fp.golden_targets.dwell;
    let cadence = scalars.mean_event_duration_s / dwell.mean_s.anchor;
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
        (measured.mean_gap_s - scalars.mean_event_duration_s).abs()
            <= MEAN_GAP_REL_TOL * scalars.mean_event_duration_s,
        "mean_gap_s={} declared_mean_s={}",
        measured.mean_gap_s,
        scalars.mean_event_duration_s
    );
}

// Note: the persistent arrival state can carry a long quiet gap across a UTC
// day boundary. Hour shares remain tightly sampled, while the seven-point day
// curve is therefore checked for shape rather than exact occupancy.
//
// The `#[ignore]` carries a reason now, and the reason is cost. It was the one
// bare `#[ignore]` in the workspace, which made it unclassifiable: nothing can
// tell a cost ignore from an environment one or a parked one, and this file's
// neighbours are all three.
//
// What the attribute buys is exclusion from the fast lane, which does not set
// `include_ignored`; the full gate does set it, so this is a real gate there
// and runs on every one. It is deliberately not in the runner's `skip` list.
//
// Cutting `SESSION_DRAW` was considered and refused. At the committed 0.171 s
// mean event gap, 15M parent events is about 30 simulated days, and the seven
// `dow_weight` assertions below need several whole weeks to separate a weekend
// from a weekday; halving the draw halves the span to two weeks, which does not
// fail sooner, it passes on less evidence. The harness already throws every
// child away (`src.burst.remaining = 0`), so what is left is the arrival draw
// itself at about 500 ns per event in dev - there is no fat to take.
//
// The watchdog headroom is 2.6x, measured, not the ~1.6x a static estimate
// suggested: 7.51 s one test per process in dev, and the runner config records
// the same walk at 7.622 s serial against 7.768 s at eight threads, i.e. this
// walk barely notices the parallel lane. The 20 s per-test kill is not close.
//
// A 15M-parent-event walk: see the heavy-walk cfg note at the top of this file.
#[test]
#[cfg(not(feature = "hotpath"))]
#[ignore = "walks 15M parent events, about 30 simulated days, in 7.5 s; kept out of the fast lane, run by the full gate"]
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
        measured.vol_hour[14] > 1.5,
        "vol_hour[14]={}",
        measured.vol_hour[14]
    );
    assert!(
        measured.vol_hour[1] < 1.0,
        "vol_hour[1]={}",
        measured.vol_hour[1]
    );
    let vol_corr = pearson(&measured.vol_hour, &fp.session_profile.vol_hour);
    // SESSION_VOL_CORR_MIN of the protocol-11 spec (Brick T), inclusive.
    // The measured curve is now a median absolute latent return per hour -
    // the robust estimator the earlier follow-up here demanded before any
    // vol_hour refit could be attempted. The RMS it replaces had infinite
    // sampling variance under the standardized t(4) innovation (infinite
    // kurtosis), which is what had forced this threshold down to 0.85; a
    // median's variance is finite regardless of tail weight, and under the
    // pure scale family the session multiplier induces, the median ratio
    // estimates the same curve the fingerprint stores. The threshold is
    // frozen in the spec and is not lowered from an observed result.
    assert!(vol_corr >= 0.90, "vol_corr={vol_corr}");

    let dow_corr = pearson(&measured.dow_weight, &fp.session_profile.dow_weight);
    assert!(dow_corr > 0.9, "dow_corr={dow_corr}");
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
    // The `expect` is conditional because under `hotpath` the whole struct is
    // unreachable (its two callers are the compiled-out heavy walks), so rustc
    // reports the struct and this field-level expectation goes unfulfilled -
    // which is itself an error. It still bites in the default shape, which is
    // the shape that gets to say a field has no reader.
    #[cfg_attr(
        not(feature = "hotpath"),
        expect(
            dead_code,
            reason = "Sample maxima are recorded for diagnostic context but intentionally never gated."
        )
    )]
    max_gap_s: f64,
    gap_p999_s: f64,
    empty_hour_frac: f64,
    max_empty_hour_run_h: f64,
    duration_dispersion_cv2: f64,
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
    fills_per_second_mean: f64,
    fills_per_second_median: u32,
    fills_per_second_p95: u32,
    zero_second_frac: f64,
    truncation_frac: f64,
}

/// `draw` counts parent events, not prints: at the committed
/// `children_mean` a 2M-event draw emits roughly 17M raw fills.
///
/// The per-event series (parent timestamps, event prices, the gaps and returns
/// derived from them) are retained in full - 2M `f64` is 16 MB apiece and the
/// dwell block's p999 needs a sortable duration vector. The per-print series
/// are not: 17M timestamps plus 17M `Decimal` sizes would be most of a
/// gigabyte, so sizes are folded into running moments and timestamps into the
/// two structures that actually read them - the occupied-hour set and the
/// per-second count vector, both O(simulated seconds) rather than O(prints).
fn measure(src: &mut GeneratedSource, scalars: &GeneratorScalars, draw: usize) -> Measured {
    let median_size = decimal_to_f64(scalars.latent_size_median);
    let mut parent_timestamps = Vec::with_capacity(draw);
    let mut prices = Vec::with_capacity(draw);
    let mut size_n = 0_u64;
    let mut size_sum = 0.0_f64;
    let mut size_sumsq = 0.0_f64;
    let mut round_lots = 0_u64;
    let mut occupied_hours: HashSet<u64> = HashSet::new();
    let mut first_ts = None;
    let mut last_ts = 0_u64;
    let mut counts: Vec<u32> = Vec::new();
    let mut off_grid_prices = 0;
    let mut neutral_aggressors = 0;
    while prices.len() < draw {
        let starts_event = src.burst.remaining == 0;
        let trade = next_trade(src);
        if (trade.price / scalars.modal_tick).fract() != Decimal::ZERO {
            off_grid_prices += 1;
        }
        if trade.aggressor == AggressorSide::NoAggressor {
            neutral_aggressors += 1;
        }
        let first = *first_ts.get_or_insert(trade.ts_event);
        last_ts = trade.ts_event;
        occupied_hours.insert(trade.ts_event / NS_PER_HOUR);
        let second = (trade.ts_event / 1_000_000_000 - first / 1_000_000_000) as usize;
        if second >= counts.len() {
            counts.resize(second + 1, 0);
        }
        counts[second] += 1;
        if starts_event {
            parent_timestamps.push(trade.ts_event);
        }
        if src.burst.remaining == 0 {
            prices.push(decimal_to_f64(trade.price));
        }
        let size = decimal_to_f64(trade.size);
        size_n += 1;
        size_sum += size;
        size_sumsq += size * size;
        round_lots += u64::from(is_round_lot(trade.size, median_size));
    }

    let mut durations = Vec::with_capacity(draw - 1);
    let mut returns = Vec::with_capacity(draw - 1);
    let mut zero_changes = 0;
    for i in 1..draw {
        durations.push((parent_timestamps[i] - parent_timestamps[i - 1]) as f64 / 1_000_000_000.0);
        returns.push((prices[i] / prices[i - 1]).ln());
        if prices[i] == prices[i - 1] {
            zero_changes += 1;
        }
    }
    let abs_returns: Vec<f64> = returns.iter().map(|r| r.abs()).collect();

    let mut sorted_durations = durations.clone();
    sorted_durations.sort_by(f64::total_cmp);
    let p999_index = (999 * sorted_durations.len()).div_ceil(1000) - 1;
    let (empty_hour_frac, max_empty_hour_run_h) = empty_hour_stats_over(
        first_ts.expect("a non-empty draw"),
        last_ts,
        &occupied_hours,
    );
    let span = counts.len();
    counts.sort_unstable();
    let size_mean = size_sum / size_n as f64;
    let size_var = (size_sumsq / size_n as f64 - size_mean * size_mean).max(0.0);
    let zero_seconds = counts.partition_point(|count| *count == 0);
    Measured {
        mean_gap_s: mean(&durations),
        max_gap_s: durations.iter().copied().fold(0.0_f64, f64::max),
        gap_p999_s: sorted_durations[p999_index],
        empty_hour_frac,
        max_empty_hour_run_h,
        duration_dispersion_cv2: variance(&durations) / mean(&durations).powi(2),
        duration_acf_lag1: acf(&durations, 1),
        duration_acf_lag5: acf(&durations, 5),
        return_acf_lag1: acf(&returns, 1),
        abs_return_acf_lag1: acf(&abs_returns, 1),
        abs_return_acf_lag10: acf(&abs_returns, 10),
        abs_return_acf_lag50: acf(&abs_returns, 50),
        zero_change_frac: zero_changes as f64 / returns.len() as f64,
        round_lot_frac: round_lots as f64 / size_n as f64,
        size_cv: size_var.sqrt() / size_mean,
        off_grid_prices,
        neutral_aggressors,
        fills_per_second_mean: size_n as f64 / span as f64,
        fills_per_second_median: counts[(span - 1) / 2],
        fills_per_second_p95: counts[(95 * span).div_ceil(100) - 1],
        zero_second_frac: zero_seconds as f64 / span as f64,
        truncation_frac: src.shape.truncated as f64 / src.shape.drawn as f64,
    }
}

/// Slice-shaped front for the slice test below. `measure` streams and calls
/// `empty_hour_stats_over` directly, because holding 17M print timestamps only
/// to bucket them is the allocation the streaming rewrite exists to remove.
fn empty_hour_stats(timestamps_ns: &[u64]) -> (f64, f64) {
    let (Some(&first), Some(&last)) = (timestamps_ns.first(), timestamps_ns.last()) else {
        return (0.0, 0.0);
    };
    let seen: HashSet<u64> = timestamps_ns.iter().map(|ts| ts / NS_PER_HOUR).collect();
    empty_hour_stats_over(first, last, &seen)
}

fn empty_hour_stats_over(first: u64, last: u64, seen: &HashSet<u64>) -> (f64, f64) {
    // Population: every whole UTC hour bucket lying fully inside [first, last],
    // matching `mogwai_lab::characterize::dwell_stats` bucket for bucket so the
    // realism gate compares like with like. The two are held equal by
    // `analysis/dwell_conformance.json`, which both sides run - see
    // `empty_hour_stats_match_the_shared_conformance_fixture` below. A span
    // holding no complete bucket defines both statistics as 0, which keeps the
    // helper total.
    let first_complete = first.div_ceil(NS_PER_HOUR);
    let after_last_complete = last / NS_PER_HOUR;
    if first_complete >= after_last_complete {
        return (0.0, 0.0);
    }
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

/// The shared dwell fixture, replacing the hand-built cases that used to sit
/// here. This helper measures the synthetic tape and
/// `mogwai_lab::characterize::dwell_stats` measures the corpus, and a realism
/// gate compares one against the other - so if the two hour-bucket conventions
/// ever drift, the gate silently compares two different quantities and still
/// passes. A hand-built fixture on one side cannot catch that, because it
/// pins this implementation against itself.
///
/// `analysis/dwell_conformance.json` is run by both sides instead. The two
/// implementations stay separate deliberately: this one streams nanoseconds
/// and the lab one takes seconds with an era clamp, and collapsing them would
/// put a `mogwai-data` dev-dependency on `mogwai-lab`, which is the wrong
/// direction - `mogwai-lab` depends on `mogwai-data`.
#[test]
fn empty_hour_stats_match_the_shared_conformance_fixture() {
    #[derive(serde::Deserialize)]
    struct Expect {
        empty_hour_frac: f64,
        max_empty_hour_run_h: f64,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        first_ts_s: f64,
        last_ts_s: f64,
        occupied_hours: Vec<u64>,
        expect: Expect,
    }
    #[derive(serde::Deserialize)]
    struct Epoch {
        seconds: u64,
    }
    #[derive(serde::Deserialize)]
    struct Spec {
        version: u32,
        tolerance: f64,
        epoch: Epoch,
        cases: Vec<Case>,
    }

    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../analysis/dwell_conformance.json"
    ));
    let spec: Spec = serde_json::from_str(raw).expect("conformance fixture parses");
    assert_eq!(spec.version, 1, "fixture version changed; re-read it");

    // The fixture states seconds after an epoch; this side works in absolute
    // nanoseconds, so the epoch is what makes one set of cases drive both.
    let epoch_ns = spec.epoch.seconds * 1_000_000_000;
    let epoch_hour = epoch_ns / NS_PER_HOUR;
    assert_eq!(
        epoch_hour * NS_PER_HOUR,
        epoch_ns,
        "the fixture's epoch must land on an hour boundary for the offsets to be hour offsets"
    );

    for case in &spec.cases {
        let seen: HashSet<u64> = case
            .occupied_hours
            .iter()
            .map(|hour| epoch_hour + hour)
            .collect();
        let to_ns = |offset_s: f64| epoch_ns + (offset_s * 1_000_000_000.0) as u64;
        let (frac, run) =
            empty_hour_stats_over(to_ns(case.first_ts_s), to_ns(case.last_ts_s), &seen);
        assert!(
            (frac - case.expect.empty_hour_frac).abs() <= spec.tolerance,
            "{}: empty_hour_frac {frac} != {}",
            case.name,
            case.expect.empty_hour_frac
        );
        assert!(
            (run - case.expect.max_empty_hour_run_h).abs() <= spec.tolerance,
            "{}: max_empty_hour_run_h {run} != {}",
            case.name,
            case.expect.max_empty_hour_run_h
        );
    }
}

/// The slice-shaped front is what `measure` does NOT use, so it gets its own
/// check that it buckets and brackets a timestamp slice the same way.
#[test]
fn empty_hour_stats_bucket_a_timestamp_slice() {
    let h = NS_PER_HOUR;
    // Buckets 1 and 2 are complete, bucket 2 is empty.
    assert_eq!(empty_hour_stats(&[h / 2, h + 1, 4 * h - 1]), (0.5, 1.0));
    // No complete bucket inside the span at all.
    assert_eq!(empty_hour_stats(&[h / 2, h + 1, 2 * h - 1]), (0.0, 0.0));
    // An empty slice is total, like the streaming path on a pair with no print.
    assert_eq!(empty_hour_stats(&[]), (0.0, 0.0));
}

fn durations(src: &mut GeneratedSource, draw: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(draw.saturating_sub(1));
    let mut prior_ts = None;
    while prior_ts.is_none() || out.len() < draw.saturating_sub(1) {
        let starts_event = src.burst.remaining == 0;
        let tick = src.next_tick().expect("unbounded generated source");
        if !starts_event {
            continue;
        }
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
    while out.len() < draw {
        let starts_event = src.burst.remaining == 0;
        let _tick = src.next_tick().expect("unbounded generated source");
        if !starts_event {
            continue;
        }
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
    while in_window.len() + out_window.len() < draw {
        let starts_event = src.burst.remaining == 0;
        let _tick = src.next_tick().expect("unbounded generated source");
        if !starts_event {
            continue;
        }
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
    let mut abs_ret_hour: [Vec<f64>; 24] = std::array::from_fn(|_| Vec::new());
    let mut dow_count = [0_u64; 7];
    let mut prev_price: Option<f64> = None;

    let mut events = 0;
    while events < draw {
        let TickEvent::Trade(trade) = src.next_tick().expect("unbounded generated source") else {
            continue;
        };
        let starts_event = src.burst.emitted == 1;
        if !starts_event {
            continue;
        }
        // Drop the rest of the sweep instead of emitting it, so a four-week
        // span costs 15M ticks rather than ~130M. Safe for this test and no
        // other: no child draw touches the arrival clock, the GARCH state or
        // the session curves, which are the only things measured here. It is
        // not the shipped tape's realization - the skipped children would have
        // consumed level and size draws, so every later draw lands at a
        // different position in the RNG stream. That makes this a statistically
        // equivalent path, not a byte-identical one, which is all a curve-shape
        // assertion needs.
        src.burst.remaining = 0;
        events += 1;
        let (hour, dow) = utc_hour_dow(trade.ts_event);
        hour_count[hour] += 1;
        dow_count[dow] += 1;

        // Session volatility modulates the one latent update made for each
        // parent event. Child sweep geometry is execution microstructure, so
        // measuring the final child would mix level count into this curve.
        let price = src.vol.mid;
        if let Some(prev) = prev_price {
            // Absolute latent return, for the median scale below. Latent
            // mids are continuous, so no tick-grid zero atom threatens the
            // median here (unlike observed quote mids, where the protocol-11
            // estimator must include the zero mass differently).
            abs_ret_hour[hour].push((price / prev).ln().abs());
        }
        prev_price = Some(price);
    }

    let total_hour = hour_count.iter().sum::<u64>() as f64;
    let total_dow = dow_count.iter().sum::<u64>() as f64;
    let mut intensity_hour = [0.0; 24];
    let mut med_hour = [0.0; 24];
    let mut populated_hours = 0;
    for h in 0..24 {
        intensity_hour[h] = hour_count[h] as f64 / total_hour;
        if !abs_ret_hour[h].is_empty() {
            // Median absolute return: the robust per-hour scale the recorded
            // follow-up demanded. Under a pure scale family (session
            // modulation multiplies every return by vol_mult) the median
            // ratio equals the RMS ratio it replaces, without the infinite
            // sampling variance a t(4) RMS carries.
            let mid = abs_ret_hour[h].len() / 2;
            let (_, m, _) = abs_ret_hour[h]
                .select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).expect("finite return"));
            med_hour[h] = *m;
            populated_hours += 1;
        }
    }

    let med_mean =
        med_hour.iter().filter(|value| **value > 0.0).sum::<f64>() / populated_hours as f64;
    let mut vol_hour = [0.0; 24];
    for h in 0..24 {
        vol_hour[h] = med_hour[h] / med_mean;
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

// The standardized-innovation counterfactual and the (a1, b1, vol_scalar)
// sweep that chose the landed parameters lived here. They were decision tools,
// the decision is made, and their full output is preserved in the history at
// commits ae42015 and cac0ff1. Two lessons from them are worth keeping in
// front of a reader rather than in a log:
//
//   - A pilot at 120k events put the shipped arm outside three committed
//     ranges purely on sample size. ACF estimators need the full draw before
//     any arm can be compared against a fitted band.
//   - The first parameter grid put its optimum at two boundaries and so never
//     bracketed; its ranking turned out to be almost pure amplitude ordering.
//     A sweep that does not bracket reads exactly like a result.

// ---------------------------------------------------------------------------
// Rail sizing for the standardized candidate.
//
// With standardized t(4) and persistence 0.999 the second moment is
// stationary, but the fourth is not: t(4) has infinite kurtosis, so sigma2 has
// infinite variance and its running maximum grows without bound in the horizon.
// "Zero hits in two million updates" therefore cannot mean "this rail never
// binds" - it means the horizon was short. A rail is a policy: choose a clean
// calibration horizon, choose how many hits are tolerable within it, and read
// the rail off the measured tail.
//
// Rails are reported as scale-free ratios - sigma rail over vol_scalar, return
// rail over return RMS - so the policy survives a later change to vol_scalar,
// and so the replacement for the `vol_scalar < 0.9 * cap` headroom rule falls
// out as 1/ratio rather than inheriting 0.9 by inertia.
// ---------------------------------------------------------------------------

/// `STUDENT_T_UNIT_SCALE` is written as `SQRT_2` because that is what
/// `sqrt(df / (df - 2))` equals at the committed four degrees of freedom. The
/// coupling is to `STUDENT_T_DF`, not to the number two, so a change there must
/// fail here rather than silently de-standardize the innovation and put the
/// recursion back above its stationarity condition.
#[test]
fn student_t_unit_scale_matches_df() {
    let expected = (STUDENT_T_DF / (STUDENT_T_DF - 2.0)).sqrt();
    assert!(
        (STUDENT_T_UNIT_SCALE - expected).abs() < 1e-15,
        "STUDENT_T_UNIT_SCALE {STUDENT_T_UNIT_SCALE} does not match sqrt(df/(df-2)) {expected} \
         at STUDENT_T_DF {STUDENT_T_DF}"
    );
}

/// Test-only GARCH parameterization. `GarchVol` reads only `a0`, `a1`, `b1`
/// and its running `sigma2`, so writing those four expresses any
/// `(a1, b1, vol_scalar)` triple. Retained after the parameter sweep was
/// removed because the rail and envelope harnesses below still need to state
/// which process they are measuring.
#[derive(Debug, Clone, Copy)]
struct GarchOverride {
    a1: f64,
    b1: f64,
    vol_scalar: f64,
}

/// Bounded high-tail collector: keeps the largest `keep` values seen without
/// retaining the whole sample, which at sixteen million draws would be hundreds
/// of megabytes for statistics that only read the extreme upper tail.
struct TopK {
    keep: usize,
    values: Vec<f64>,
    threshold: f64,
    seen: u64,
}

impl TopK {
    fn new(keep: usize) -> Self {
        Self {
            keep,
            values: Vec::with_capacity(keep * 2),
            threshold: f64::NEG_INFINITY,
            seen: 0,
        }
    }
    fn push(&mut self, value: f64) {
        self.seen += 1;
        if value <= self.threshold {
            return;
        }
        self.values.push(value);
        if self.values.len() >= self.keep * 2 {
            self.compact();
        }
    }
    fn compact(&mut self) {
        self.values.sort_unstable_by(|a, b| b.total_cmp(a));
        self.values.truncate(self.keep);
        self.threshold = *self.values.last().unwrap_or(&f64::NEG_INFINITY);
    }
    /// The value exceeded by exactly `rank` samples, i.e. the rail that would
    /// have been hit `rank` times over the whole sample.
    fn value_exceeded_by(&mut self, rank: usize) -> Option<f64> {
        self.compact();
        if rank >= self.values.len() {
            return None;
        }
        Some(self.values[rank])
    }
    fn max(&mut self) -> f64 {
        self.compact();
        self.values.first().copied().unwrap_or(f64::NAN)
    }
}

/// Drive the standardized candidate's recursion with the rails effectively
/// removed, so the measured tail is the process rather than the fence.
fn measure_uncapped_tail(over: GarchOverride, seeds: u64, horizon: u64) -> (TopK, TopK, f64, u64) {
    use rand::SeedableRng;
    use rand_distr::{ChiSquared, Normal};

    // The rails are no longer openable - `step` reads the shipped constants and
    // takes no multiplier, which is the decoupling. That is fine and in fact
    // stronger: the shipped rails were sized to sit above this process's clean
    // tail, so the assertion inside the loop is the sizing claim, checked on
    // every step rather than on a top-k maximum. Callers must not restate it
    // afterwards as `sigma_tail.max() < GARCH_SIGMA_CAP`: `step` writes
    // `sigma2 = candidate.min(cap)`, so a maximum taken over the capped value
    // can never exceed the cap and such an assertion is dead by construction.
    // Both callers say so where the temptation is.
    //
    // The sigma tail is therefore collected from `sigma2_candidate`, the value
    // before the cap, which is what "uncapped tail" in this function's name
    // means. The in-loop guard makes the two equal today; collecting the
    // capped one would silently start under-reporting the tail the moment it
    // stopped being equal.
    let unit = STUDENT_T_UNIT_SCALE;
    let mut sigma_tail = TopK::new(65_536);
    let mut return_tail = TopK::new(65_536);
    let mut sumsq = 0.0_f64;
    let mut total = 0_u64;

    for seed in 0..seeds {
        let mut vol = super::dynamics::GarchVol::new(21_000.0, over.vol_scalar);
        let uncond = over.vol_scalar.powi(2);
        vol.a0 = uncond * (1.0 - over.a1 - over.b1);
        vol.a1 = over.a1;
        vol.b1 = over.b1;
        vol.sigma2 = uncond;
        let mut rng = rand_chacha::ChaCha12Rng::seed_from_u64(seed);
        let normal = Normal::new(0.0, 1.0).expect("valid normal");
        let chi_squared = ChiSquared::new(STUDENT_T_DF).expect("valid chi-squared");
        for _ in 0..horizon {
            let z = super::dynamics::draw_student_t(&mut rng, &normal, &chi_squared) / unit;
            let step = vol.step(z);
            assert!(
                !step.hit_variance_cap() && !step.hit_feedback_clamp(),
                "the sizing run must be unrailed: a shipped rail bound the clean \
                 process, so it does not sit above the clean tail and this \
                 measurement is no longer of the unrailed process"
            );
            sigma_tail.push(step.sigma2_candidate.sqrt());
            return_tail.push(step.unclipped_return.abs());
            sumsq += step.unclipped_return * step.unclipped_return;
            total += 1;
        }
    }
    let rms = (sumsq / total as f64).sqrt();
    (sigma_tail, return_tail, rms, total)
}

// This is the repository's rail-sizing claim and it runs. It was `#[ignore]`d
// and in the gate profile's `skip` list under a cost heading asserting that
// every entry there outlives the 20-second per-test hang watchdog. Measured on
// 2026-08-18, one test per process in dev, it takes 0.43 s - so the durable
// numbers in `consts.rs` (a sigma reaching 57.2x its unconditional scale, a
// largest clean return of 3.33e-3, an RMS of 1.2393e-5, all over 16M updates)
// rested on a test nothing had run since it was written. Neither the
// `#[ignore]` nor the skip entry survives that measurement.
//
// It is also pointed at the shipped constants rather than at a written-down
// "stage-1 winner" triple that happened to equal them. A candidate frozen in a
// test is a second opinion about the generator: re-solve `GARCH_ARCH`,
// `GARCH_GARCH` or `VOL_SCALAR` and this would have gone on measuring the old
// process, green, while `consts.rs` still cited it.
#[test]
fn shipped_garch_rails_sit_above_the_clean_tail() {
    let candidate = GarchOverride {
        a1: GARCH_ARCH,
        b1: GARCH_GARCH,
        vol_scalar: VOL_SCALAR,
    };
    const SEEDS: u64 = 8;
    const HORIZON: u64 = 2_000_000;

    let (mut sigma_tail, mut return_tail, rms, total) =
        measure_uncapped_tail(candidate, SEEDS, HORIZON);

    println!();
    println!(
        "shipped standardized process a1 {:.2} b1 {:.3} vol_scalar {:.2e}",
        candidate.a1, candidate.b1, candidate.vol_scalar
    );
    println!("{SEEDS} seeds x {HORIZON} updates = {total} unrailed latent updates");
    println!("unclipped return RMS {rms:.4e}");
    println!();
    println!(
        "A rail placed at the value exceeded by K samples is hit K times across the whole run,"
    );
    println!("i.e. K/{SEEDS} times per {HORIZON}-update clean calibration horizon.");
    println!();
    println!(
        "{:>10} {:>14} {:>12} | {:>14} {:>12}",
        "hits/run", "sigma rail", "/ vol_scalar", "return rail", "/ RMS"
    );
    for rank in [0_usize, 1, 8, 80, 800, 8_000] {
        let sigma = sigma_tail.value_exceeded_by(rank);
        let ret = return_tail.value_exceeded_by(rank);
        let (Some(sigma), Some(ret)) = (sigma, ret) else {
            continue;
        };
        println!(
            "{:>10} {:>14.4e} {:>12.1} | {:>14.4e} {:>12.1}",
            rank,
            sigma,
            sigma / candidate.vol_scalar,
            ret,
            ret / rms
        );
    }
    println!();
    println!(
        "observed maxima: sigma {:.4e} ({:.1}x vol_scalar), |return| {:.4e} ({:.1}x RMS)",
        sigma_tail.max(),
        sigma_tail.max() / candidate.vol_scalar,
        return_tail.max(),
        return_tail.max() / rms
    );
    println!();
    println!("shipped rails for comparison:");
    println!(
        "  GARCH_SIGMA_CAP {:.4e} = {:.1}x this candidate's vol_scalar",
        GARCH_SIGMA_CAP,
        GARCH_SIGMA_CAP / candidate.vol_scalar
    );
    println!(
        "  FEEDBACK_RETURN_CEILING {:.4e} = {:.1}x this candidate's return RMS",
        FEEDBACK_RETURN_CEILING,
        FEEDBACK_RETURN_CEILING / rms
    );
    println!();
    println!("headroom is DIAGNOSED, not gated: a universal vol_scalar-over-cap ratio");
    println!("would repeat the scalar_ranges mistake in another dimension.");

    // The pinned invariant, post-repair: both shipped rails sit above the clean
    // process's measured excursions, so neither participates in ordinary
    // operation. Pre-repair this read the other way round - the rails sat below
    // the operating range, which is why the recursion spent 12.96 percent of
    // its life saturated and why the repair could not land without re-deriving
    // them.
    //
    // "Above the measured tail" is not "never hit". Standardized t(4) still has
    // infinite kurtosis, so the running maximum grows with the horizon; the
    // honest claim is that no participation was observed over this 16M sample.
    //
    // Both halves of that invariant are enforced in the loop, not here, and
    // the pair of top-k restatements that used to stand at this point is gone
    // because neither could fail. `measure_uncapped_tail` asserts
    // `!hit_variance_cap() && !hit_feedback_clamp()` on every step, so a rail
    // sized below the clean tail aborts the run at the first offending update
    // and no later comparison against a maximum is ever reached. The sigma one
    // was dead twice over: `step` writes `sigma2 = candidate.min(cap)`, so a
    // maximum taken over the capped value cannot exceed the cap even with the
    // guard removed. Two assertions that read as the headline claim and could
    // not report it is exactly the defect this round is named for, so the
    // claim is stated once, on the guard, where it bites per-step rather than
    // per-maximum.
    //
    // What survives here are the claims the guard does not make.
    assert!(
        sigma_tail.max() / candidate.vol_scalar > 1.0,
        "sigma excursions should exceed the unconditional scale"
    );
}

// ---------------------------------------------------------------------------
// Realized-return envelope under regime scaling.
//
// Three concepts are currently overloaded onto two constants:
//
//   1. the GARCH-state safety rail (caps sigma2, so it changes the recursion);
//   2. the clean feedback-return rail (caps what re-enters the recursion);
//   3. the absolute realized-return ceiling, after session and regime scaling.
//
// Today `clamp_mult` multiplies 1 and 2 as well as 3, so `VolStorm` does not
// merely amplify output - it raises the GARCH state ceiling and changes every
// subsequent recursion step. That contradicts the principle `SessionEdgeSpike`
// already follows, of not contaminating feedback state.
//
// Under the decoupled design the storm is an output envelope: rails 1 and 2
// stay at their clean values, so the base process is identical armed or not,
// and `base_return * session_mult * regime_mult` is bounded by a separately
// named realized ceiling. This measures what that ceiling has to be, because
// multiplying the clean rail by 100 is not a derivation.
// ---------------------------------------------------------------------------

// The same treatment, on the same grounds, as the rail sizing above: `#[ignore]`d and
// skipped under a watchdog claim, measured at 0.20 s. It runs, and it reads the
// shipped constants rather than the design-era candidate and the design-era
// "proposed" rails - those literals were what the sweep was choosing between,
// and leaving them written down here after the choice landed turned the
// instrument into a record of a decision instead of a check on it.
#[test]
fn realized_return_envelope_under_regime_scaling() {
    let candidate = GarchOverride {
        a1: GARCH_ARCH,
        b1: GARCH_GARCH,
        vol_scalar: VOL_SCALAR,
    };
    const SEEDS: u64 = 8;
    const HORIZON: u64 = 1_000_000;
    // The clean rails, as shipped. Both sit above the observed clean tail, so
    // they do not participate in ordinary calibration - which is what lets the
    // base distribution below be treated as the unrailed one.
    const CLEAN_SIGMA_RAIL: f64 = GARCH_SIGMA_CAP;
    const CLEAN_FEEDBACK_RAIL: f64 = FEEDBACK_RETURN_CEILING;
    // Peak of the committed session vol curve; the realized path multiplies by
    // this as well as by the regime, and ignoring it would under-size the
    // ceiling by a factor of nearly 2.5.
    let session_peak = Fingerprint::from_repo_json()
        .session_profile
        .vol_hour
        .iter()
        .copied()
        .fold(f64::MIN, f64::max);

    let (mut sigma_tail, mut return_tail, rms, total) =
        measure_uncapped_tail(candidate, SEEDS, HORIZON);
    let base_max = return_tail.max();
    println!();
    println!("clean base process over {total} updates: RMS {rms:.4e}, max |return| {base_max:.4e}");
    println!(
        "shipped clean rails: sigma {CLEAN_SIGMA_RAIL:.1e} (max seen {:.4e}), feedback {CLEAN_FEEDBACK_RAIL:.1e} (max seen {base_max:.4e})",
        sigma_tail.max()
    );
    println!("session vol peak {session_peak:.3}");
    println!();
    println!("Realized = base * session * regime. Under the DECOUPLED design the base");
    println!("distribution is regime-independent, so the envelope scales analytically.");
    println!();
    println!(
        "{:>8} | {:>12} {:>12} {:>12} | {:>10} {:>10}",
        "vol_mult", "realized RMS", "p99.99", "max", "max move", "today max"
    );
    let p9999 = return_tail
        .value_exceeded_by((total / 10_000) as usize)
        .expect("tail has p99.99");
    for mult in [1.0_f64, 2.0, 5.0, 10.0, 100.0] {
        let scale = mult * session_peak;
        let realized_max = base_max * scale;
        // What the shipped constant allows at this multiplier today, for
        // comparison: before the rails were separated, the realized ceiling was
        // the feedback rail multiplied by the regime, so a storm's permitted
        // move scaled with whatever that rail happened to be. It was 2e-5.
        const PRE_REPAIR_RAIL: f64 = 2e-5;
        let today = PRE_REPAIR_RAIL * mult;
        println!(
            "{:>8.0} | {:>12.4e} {:>12.4e} {:>12.4e} | {:>9.2}% {:>9.2}%",
            mult,
            rms * scale,
            p9999 * scale,
            realized_max,
            100.0 * (realized_max.exp() - 1.0),
            100.0 * (today.exp() - 1.0)
        );
    }
    println!();
    println!("Candidate absolute realized ceilings, and the single-event move each permits:");
    for ceiling in [4e-3_f64, 1e-2, 2.5e-2, 5e-2, 1e-1] {
        // Occupancy is measured at the worst case: max regime and peak session.
        let scale = 100.0 * session_peak;
        let rank_exceeding = (0..return_tail.values.len())
            .take_while(|i| return_tail.values[*i] * scale > ceiling)
            .count();
        // The tail collector retains a bounded number of values, so a count
        // that reaches its capacity is censored - a lower bound, not a count.
        // Reporting it as a count would understate how hard a low ceiling binds.
        let censored = rank_exceeding >= return_tail.keep;
        println!(
            "  ceiling {:.1e} -> {:>6.2}% max move; exceeded {}{} times in {} at vol_mult 100 x session peak",
            ceiling,
            100.0 * (ceiling.exp() - 1.0),
            if censored { ">=" } else { "" },
            rank_exceeding,
            total
        );
    }
    println!();
    println!(
        "Pre-repair the max-strength storm ceiling was {:.1e} = {:.2}%; it is now the",
        2e-5_f64 * 100.0,
        100.0 * ((2e-5_f64 * 100.0).exp() - 1.0)
    );
    println!(
        "absolute REALIZED_RETURN_CEILING {:.1e} = {:.2}%, chosen as policy from this table.",
        REALIZED_RETURN_CEILING,
        100.0 * (REALIZED_RETURN_CEILING.exp() - 1.0)
    );

    // The two rail restatements that stood here are gone for the reason given
    // at the end of `shipped_garch_rails_sit_above_the_clean_tail`: both clean
    // rails are enforced per-step by `measure_uncapped_tail`'s own guard, which
    // aborts before any maximum is compared, and the sigma one could not fail
    // even without it. `CLEAN_SIGMA_RAIL` and `CLEAN_FEEDBACK_RAIL` stay named
    // above because the printed table is written in terms of them.
    //
    // The claim this instrument owns, and the only one the rail sizing above
    // does not already carry: `REALIZED_RETURN_CEILING` is a product-policy
    // decision that must be inert in clean operation. `consts.rs` states it as "the
    // unbounded realized maximum at `vol_mult` 1 is 0.82 percent" against a
    // ceiling permitting 5.13 percent. `vol_mult` 1 at the session vol peak is
    // the worst clean case, so the ceiling shaping nothing there is exactly
    // that sentence, asserted.
    let clean_worst_case = base_max * session_peak;
    assert!(
        clean_worst_case < REALIZED_RETURN_CEILING,
        "the realized ceiling must not shape clean operation: worst clean \
         realized return {clean_worst_case} against ceiling \
         {REALIZED_RETURN_CEILING}"
    );
}

// ---------------------------------------------------------------------------
// Synthetic spread decomposition.
//
// "The spread" is not one number in this model, and treating it as one is how
// an earlier analysis reached three mutually inconsistent figures without
// noticing. This reports each one separately and names it.
//
// The output schema here is deliberately the contract that the real-data
// experiments must match, so a Roll estimate from a venue archive can be
// compared against the same quantity rather than against whichever number was
// convenient.
//
// Two things it must never do: report a Roll spread when the covariance is
// non-negative (the estimator is undefined there, and zero is not the answer),
// and compute at the print layer, where sweep children walk monotonically and
// flip the covariance sign outright.
// ---------------------------------------------------------------------------

/// Trailing realized-volatility horizon, in parent events, for the observable
/// stratification axis. Fixed here, before any result was examined, which is
/// the point: a horizon chosen after seeing the strata is a fitted parameter
/// wearing a methodology's clothes.
const TRAILING_VOL_EVENTS: usize = 64;

/// Global quantile boundaries for the observable axis. Computed once over the
/// whole analysis corpus and reused unchanged across every sampling convention,
/// so conventions stay comparable to each other.
const VOL_STRATUM_QUANTILES: [f64; 3] = [0.25, 0.75, 0.95];
const VOL_STRATUM_NAMES: [&str; 4] = ["calm", "middle", "stressed", "extreme"];

/// Minimum covariance pairs before a cell reports an estimate. Below this the
/// cell fails closed: an unstable number printed without comment is worse than
/// a hole, because it will be read.
const MIN_COVARIANCE_PAIRS: usize = 500;

/// One Roll estimate plus the covariance behind it, so an unavailable result
/// still says why.
#[derive(Debug, Clone, Copy)]
struct RollEstimate {
    covariance: f64,
    /// `None` when the covariance is non-negative. Roll is then unavailable,
    /// which is a different statement from a spread of zero and must never be
    /// coerced into one.
    ticks: Option<f64>,
}

/// Three sampling estimators of the same underlying quantity, differing only in
/// which prints they sample, plus the same-mid separation and mid-relative
/// displacement measured at two of those sampling points.
///
/// The protocol-7 schema keeps the published-book and latent-mid referents
/// separate. Effective spread is measured against the emitted BBO midpoint;
/// latent-relative displacement remains alongside as a model diagnostic.
///
/// On this synthetic tape the estimators happen to order as
/// `roll_first_child < same-mid separation < roll_last_child`. That ordering is
/// recorded, not asserted: it is not a general bound. In real data either
/// estimator can land on either side of the quoted spread depending on side
/// persistence, latent movement, burst grouping and within-burst book changes.
/// Promoting an observed relationship to an invariant is how a schema stops
/// being able to report a surprise.
/// One of the two parent populations section 2.7 separates, with the sample
/// size beside the statistic: a cohort of forty parents and one of four hundred
/// thousand carry the same field and must not read as equally supported.
#[derive(Debug, Clone, Copy)]
struct Cohort {
    parents: usize,
    effective_spread_ticks: f64,
}

#[derive(Debug, Clone)]
struct SpreadDecomposition {
    /// Exact configured BBO width, in integer ticks.
    configured_quoted_width_ticks: f64,
    same_mid_separation_ticks: Vec<(f64, u32)>,
    /// Samples each burst's first print. Excludes the sweep's own level walk.
    roll_first_child: RollEstimate,
    /// Samples each burst's last print. Includes the walk, and is what the
    /// realism gate's event series uses.
    roll_last_child: RollEstimate,
    /// Samples every print. Retained precisely because it fails: recording an
    /// unavailable estimator documents why ordinary print-layer Roll cannot be
    /// used here, where dropping it would leave the question open forever.
    roll_all_prints: RollEstimate,
    /// Realized opposite-side print separation around one published midpoint.
    same_mid_separation_at_first_child: f64,
    same_mid_separation_at_last_child: f64,
    /// Effective spread against the published midpoint, followed by the same
    /// formula against the unobservable latent midpoint.
    effective_spread_at_first_child: f64,
    effective_spread_at_last_child: f64,
    latent_mid_relative_displacement_at_first_child: f64,
    latent_mid_relative_displacement_at_last_child: f64,
    opposite_side_separation: Vec<(i64, f64)>,
    zero_change_frac: f64,
    accepted_repeat_frac: f64,
    /// Effective spread split by section 2.7's two mechanisms rather than
    /// pooled. `None` where the sample carried no parent of that kind, which is
    /// a fact about the sample and is reported as one rather than as a zero.
    repeated_parents: Option<Cohort>,
    fresh_parents: Option<Cohort>,
    conditional_return_scale: f64,
    tick_return: f64,
    tick_traversal_sigma_units: f64,
}

/// Trailing realized volatility per event, from parent-event log returns over
/// `TRAILING_VOL_EVENTS`. Trailing only - element `i` uses returns strictly
/// before `i`, so no observation can see its own future. Leading elements
/// before the horizon fills are `None` and are excluded rather than
/// back-filled.
fn trailing_vol(prices: &[f64]) -> Vec<Option<f64>> {
    let returns: Vec<f64> = prices.windows(2).map(|w| (w[1] / w[0]).ln()).collect();
    let mut out = vec![None; prices.len()];
    for i in 0..prices.len() {
        if i < TRAILING_VOL_EVENTS {
            continue;
        }
        let window = &returns[i - TRAILING_VOL_EVENTS..i];
        let sumsq = window.iter().map(|r| r * r).sum::<f64>();
        out[i] = Some((sumsq / window.len() as f64).sqrt());
    }
    out
}

/// Global stratum boundaries from the whole corpus, so every sampling
/// convention is cut at the same places.
fn vol_boundaries(vols: &[Option<f64>]) -> Vec<f64> {
    let mut present: Vec<f64> = vols.iter().filter_map(|v| *v).collect();
    present.sort_by(f64::total_cmp);
    VOL_STRATUM_QUANTILES
        .iter()
        .map(|q| present[((present.len() - 1) as f64 * q) as usize])
        .collect()
}

fn stratum_of(vol: f64, boundaries: &[f64]) -> usize {
    boundaries.iter().filter(|b| vol >= **b).count()
}

/// Volatility per change for a parent-event series: change `k` runs from parent
/// price `k` to `k + 1`, and the value known before it is the volatility at
/// price `k`.
fn parent_change_vols(vols: &[Option<f64>]) -> Vec<Option<f64>> {
    vols[..vols.len().saturating_sub(1)].to_vec()
}

/// Volatility per change for the all-print series, expressed in the shared
/// parent-event currency.
///
/// The horizon and the stratum boundaries are both denominated in parent
/// events, so the print series must borrow that scale rather than compute its
/// own: a trailing window measured in prints has a different cadence and a
/// different return distribution, and cutting it at parent-derived boundaries
/// compares two things that were never the same quantity. That mismatch is what
/// left the extreme all-print cell with 34 pairs - a scale artifact, not
/// genuine scarcity.
///
/// A change entering parent event `j` takes the trailing parent volatility
/// available after event `j - 1`. Changes between children of event `j` take
/// that same pre-event value, since nothing new is observable within a burst.
fn print_change_vols(parent_of_print: &[usize], vols: &[Option<f64>]) -> Vec<Option<f64>> {
    (0..parent_of_print.len().saturating_sub(1))
        .map(|k| {
            let entering = parent_of_print[k + 1];
            if entering == 0 {
                None
            } else {
                vols.get(entering - 1).copied().flatten()
            }
        })
        .collect()
}

/// Roll over a price series restricted to one stratum. A covariance pair spans
/// two consecutive changes, hence three prices, and contributes
/// `dP_t * dP_{t-1}`.
///
/// No-lookahead rule, and it is load-bearing. The pair belongs to the later of
/// its two changes, `dP_t`, so a boundary cannot claim a pair that straddles
/// it - but the volatility that assigns it must be computed from information
/// available before `dP_t`. It may include `dP_{t-1}`, matching what a
/// conditional volatility process would know.
///
/// An earlier version indexed `vols` at the pair's third price, whose trailing
/// window includes `dP_t` itself. That stratifies on one of the two terms being
/// multiplied, and mechanically amplifies exactly the relationship the matrix
/// exists to measure. Returns the estimate and the pair count behind it.
fn roll_in_stratum(
    prices: &[f64],
    change_vols: &[Option<f64>],
    boundaries: &[f64],
    stratum: usize,
    tick: f64,
    min_pairs: usize,
) -> (RollEstimate, usize) {
    let changes: Vec<f64> = prices.windows(2).map(|w| w[1] - w[0]).collect();
    let mut pairs: Vec<(f64, f64)> = Vec::new();
    for i in 0..changes.len().saturating_sub(1) {
        // `change_vols` carries one volatility per change, already the value
        // known before that change. Taking it per price instead forced index
        // arithmetic that differed between conventions and silently mixed a
        // print-denominated horizon with parent-denominated boundaries.
        let Some(vol) = change_vols.get(i + 1).copied().flatten() else {
            continue;
        };
        if stratum_of(vol, boundaries) == stratum {
            pairs.push((changes[i], changes[i + 1]));
        }
    }
    if pairs.len() < min_pairs {
        return (
            RollEstimate {
                covariance: f64::NAN,
                ticks: None,
            },
            pairs.len(),
        );
    }
    let mean_a = pairs.iter().map(|p| p.0).sum::<f64>() / pairs.len() as f64;
    let mean_b = pairs.iter().map(|p| p.1).sum::<f64>() / pairs.len() as f64;
    let covariance = pairs
        .iter()
        .map(|(a, b)| (a - mean_a) * (b - mean_b))
        .sum::<f64>()
        / pairs.len() as f64;
    let ticks = if covariance < 0.0 {
        Some(2.0 * (-covariance).sqrt() / tick)
    } else {
        None
    };
    (RollEstimate { covariance, ticks }, pairs.len())
}

/// Pins the information set of `trailing_vol`, which the whole no-lookahead
/// argument rests on and which is easy to break by one index without any test
/// noticing.
///
/// For price index `i` the window is `returns[i - H .. i]`, and `returns[j]`
/// ends at price `j + 1`. So the volatility at price `i` includes the return
/// arriving at `i` and excludes the return leaving it. Distinct magnitudes make
/// the two cases separable rather than merely plausible.
#[test]
fn trailing_vol_includes_the_arriving_return_and_excludes_the_leaving_one() {
    let horizon = TRAILING_VOL_EVENTS;
    // Flat except for one large step, placed so that it is the return arriving
    // at the probe index. A flat series has zero volatility, so any non-zero
    // reading can only come from that step.
    let mut prices = vec![100.0_f64; horizon + 3];
    let probe = horizon;
    for p in prices.iter_mut().skip(probe) {
        *p = 110.0;
    }
    let vols = trailing_vol(&prices);
    let at_probe = vols[probe].expect("horizon has filled by the probe index");
    assert!(
        at_probe > 0.0,
        "volatility at price i must INCLUDE the return arriving at i, got {at_probe}"
    );

    // Same step, but now it leaves the probe index: everything up to the probe
    // is flat, so the probe must read exactly zero.
    let mut prices = vec![100.0_f64; horizon + 3];
    for p in prices.iter_mut().skip(probe + 1) {
        *p = 110.0;
    }
    let vols = trailing_vol(&prices);
    let at_probe = vols[probe].expect("horizon has filled by the probe index");
    assert_eq!(
        at_probe, 0.0,
        "volatility at price i must EXCLUDE the return leaving i"
    );
}

/// The Rust half of the shared conformance fixture.
///
/// `analysis/roll_estimator.py` runs the same file. The two implementations are
/// deliberately separate - Rust tests generator truth, Python handles archive
/// analysis - so without this, the claim that both corpora are measured by the
/// same estimator would rest on two implementations happening to agree. Here it
/// is a tested contract instead.
///
/// The fixture is data, not code, so neither language can quietly redefine the
/// rules while still passing its own tests.
#[test]
fn stratified_roll_matches_the_shared_conformance_fixture() {
    #[derive(serde::Deserialize)]
    struct Expect {
        status: String,
        pairs: usize,
        covariance: Option<f64>,
        roll_ticks: Option<f64>,
    }
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        tick: f64,
        prices: Vec<f64>,
        change_vol: Vec<Option<f64>>,
        boundaries: Vec<f64>,
        stratum: usize,
        min_pairs: usize,
        expect: Expect,
    }
    #[derive(serde::Deserialize)]
    struct Spec {
        version: u32,
        tolerance: f64,
        cases: Vec<Case>,
    }

    let raw = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../analysis/spread_conformance.json"
    ));
    let spec: Spec = serde_json::from_str(raw).expect("conformance fixture parses");
    assert_eq!(spec.version, 1, "fixture version changed; re-read it");

    for case in &spec.cases {
        let (est, pairs) = roll_in_stratum(
            &case.prices,
            &case.change_vol,
            &case.boundaries,
            case.stratum,
            case.tick,
            case.min_pairs,
        );
        let status = if pairs < case.min_pairs {
            "fail_closed"
        } else if est.ticks.is_some() {
            "matched"
        } else {
            "unavailable"
        };
        assert_eq!(status, case.expect.status, "{}: status", case.name);
        assert_eq!(pairs, case.expect.pairs, "{}: pair count", case.name);
        match case.expect.covariance {
            // fail_closed reports no covariance at all, which the Rust side
            // signals with NaN; the fixture says `null`.
            None => assert!(
                est.covariance.is_nan(),
                "{}: covariance should be absent, got {}",
                case.name,
                est.covariance
            ),
            Some(want) => assert!(
                (est.covariance - want).abs() <= spec.tolerance,
                "{}: covariance {} != {want}",
                case.name,
                est.covariance
            ),
        }
        match (est.ticks, case.expect.roll_ticks) {
            (None, None) => {}
            (Some(got), Some(want)) => assert!(
                (got - want).abs() <= spec.tolerance,
                "{}: roll_ticks {got} != {want}",
                case.name
            ),
            (got, want) => panic!("{}: roll_ticks {got:?} != {want:?}", case.name),
        }
    }
}

/// Roll's estimator over a price series, in ticks. `2 * sqrt(-cov)` where the
/// covariance is of consecutive price changes - not of returns, and not a
/// normalized autocorrelation, both of which drop the scale the estimate needs.
fn roll_estimate(prices: &[f64], tick: f64) -> RollEstimate {
    let changes: Vec<f64> = prices.windows(2).map(|w| w[1] - w[0]).collect();
    let n = changes.len() - 1;
    let mean = changes.iter().sum::<f64>() / changes.len() as f64;
    let covariance = (0..n)
        .map(|i| (changes[i] - mean) * (changes[i + 1] - mean))
        .sum::<f64>()
        / n as f64;
    // A non-negative covariance means the bounce signature the estimator
    // assumes is absent. It has no value here - not a zero.
    let ticks = if covariance < 0.0 {
        Some(2.0 * (-covariance).sqrt() / tick)
    } else {
        None
    };
    RollEstimate { covariance, ticks }
}

/// Mean same-mid print separation and mean mid-relative displacement, in ticks,
/// at one sampling point.
///
/// Neither return is a quoted spread, and the earlier naming of this function
/// and its locals after `quoted` and `effective` spreads was wrong: the
/// generator constructs no `QuoteTick`, so no bid and no ask exist to be quoted.
/// The `counterfactual_buy`/`counterfactual_sell` locals below are exactly that:
/// where a buy and a sell would print against this event's latent mid. Their
/// difference is a property of the print series, not a book width.
///
/// The second return is `2 * sign * (price - mid)` against the latent mid. It
/// shares its formula with the effective spread the real-data experiment
/// computes, but not its referent: the real one is measured against an as-of
/// joined book top that a venue actually published, and the latent mid is
/// unobservable. Comparing the two compares a model internal against a market
/// observable, which is legitimate only while it is stated.
///
/// Negative displacements are not clamped. Their real-data counterpart, a
/// negative effective spread, is evidence of a stale quote, sequencing ambiguity
/// or price improvement, and clamping would erase exactly the diagnostic that
/// says the join is wrong.
fn separation_and_displacement_at(
    prices: &[f64],
    mids: &[f64],
    sides: &[AggressorSide],
    tick: f64,
    configured_displacement_ticks: f64,
) -> (f64, f64) {
    let mut separation = 0.0;
    let mut displacement = 0.0;
    for i in 0..prices.len() {
        let counterfactual_buy = (mids[i] / tick + configured_displacement_ticks).round();
        let counterfactual_sell = (mids[i] / tick - configured_displacement_ticks).round();
        separation += counterfactual_buy - counterfactual_sell;
        let sign = match sides[i] {
            AggressorSide::Buyer => 1.0,
            AggressorSide::Seller => -1.0,
            AggressorSide::NoAggressor => 0.0,
        };
        displacement += 2.0 * sign * (prices[i] - mids[i]) / tick;
    }
    let n = prices.len() as f64;
    (separation / n, displacement / n)
}

fn decompose_spread(scalars: &GeneratorScalars, events: usize) -> SpreadDecomposition {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let tick = decimal_to_f64(scalars.modal_tick);

    // The burst grouping rule, and it is part of the estimator contract rather than
    // an implementation detail. Here it is exact: `burst.remaining` is the
    // generator's own structure, so first and last child are known without
    // inference. Real data has no such field and must approximate the grouping
    // from equal timestamp plus aggressor side - which makes timestamp
    // resolution part of the contract too, because a millisecond archive merges
    // events that a microsecond tape separates. Any comparison against this
    // schema has to state which grouping rule it used.
    let mut first_prices: Vec<f64> = Vec::with_capacity(events);
    let mut last_prices: Vec<f64> = Vec::with_capacity(events);
    let mut first_mids: Vec<f64> = Vec::with_capacity(events);
    let mut first_latent_mids: Vec<f64> = Vec::with_capacity(events);
    let mut sides: Vec<AggressorSide> = Vec::with_capacity(events);
    let mut print_prices: Vec<f64> = Vec::new();
    let mut base_returns: Vec<f64> = Vec::with_capacity(events);
    let mut current_quote_mid = None;
    let mut prior_quote_mid = None;
    let mut prior_last_price = None;
    let mut accepted_repeats = 0_u64;
    // Per parent, aligned with `first_prices`: whether this event repeated the
    // previous price against a frozen book rather than pricing off a fresh one.
    // Section 2.7 requires the two populations reported apart.
    let mut repeated: Vec<bool> = Vec::with_capacity(events);

    while last_prices.len() < events {
        let event = src.next_tick().expect("infinite tape");
        let TickEvent::Trade(trade) = event else {
            if let TickEvent::Quote(quote) = event {
                current_quote_mid =
                    Some((decimal_to_f64(quote.bid_px) + decimal_to_f64(quote.ask_px)) / 2.0);
            }
            continue;
        };
        let starts_event = src.burst.emitted == 1;
        let price = decimal_to_f64(trade.price);
        print_prices.push(price);
        if starts_event {
            let is_repeat = prior_quote_mid == current_quote_mid && prior_last_price == Some(price);
            accepted_repeats += u64::from(is_repeat);
            repeated.push(is_repeat);
            first_prices.push(price);
            // The latent mid this event priced against, read directly rather
            // than inferred from the prints it produced.
            first_mids.push(current_quote_mid.expect("a quote precedes every parent"));
            first_latent_mids.push(src.vol.mid);
            base_returns.push(src.vol.prev_return);
        }
        if src.burst.remaining == 0 {
            last_prices.push(price);
            sides.push(trade.aggressor);
            prior_last_price = Some(price);
            prior_quote_mid = current_quote_mid;
        }
    }
    first_prices.truncate(last_prices.len());
    first_mids.truncate(last_prices.len());
    first_latent_mids.truncate(last_prices.len());
    repeated.truncate(last_prices.len());

    let roll_first_child = roll_estimate(&first_prices, tick);
    let roll_last_child = roll_estimate(&last_prices, tick);
    let roll_all_prints = roll_estimate(&print_prices, tick);
    let configured_displacement_ticks = scalars.trade_displacement_ticks.ticks();
    let (separation_first, displacement_first) = separation_and_displacement_at(
        &first_prices,
        &first_mids,
        &sides,
        tick,
        configured_displacement_ticks,
    );
    let (separation_last, displacement_last) = separation_and_displacement_at(
        &last_prices,
        &first_mids,
        &sides,
        tick,
        configured_displacement_ticks,
    );
    // The two populations of section 2.7, measured apart. A repeat parent
    // reprints the previous price against a frozen book, so its displacement
    // from that book's midpoint is a different quantity from a fresh parent's
    // displacement from the book just published. Pooling them reports a blend of
    // two mechanisms in the one table meant to be compared against the real-data
    // matrix, where no such blending is possible to undo after the fact.
    let cohort = |want_repeat: bool| -> Option<Cohort> {
        let mut prices = Vec::new();
        let mut mids = Vec::new();
        let mut cohort_sides = Vec::new();
        for index in 0..first_prices.len() {
            if repeated[index] == want_repeat {
                prices.push(first_prices[index]);
                mids.push(first_mids[index]);
                cohort_sides.push(sides[index]);
            }
        }
        if prices.is_empty() {
            return None;
        }
        let (_, effective_spread_ticks) = separation_and_displacement_at(
            &prices,
            &mids,
            &cohort_sides,
            tick,
            configured_displacement_ticks,
        );
        Some(Cohort {
            parents: prices.len(),
            effective_spread_ticks,
        })
    };
    let repeated_parents = cohort(true);
    let fresh_parents = cohort(false);

    let (_, latent_displacement_first) = separation_and_displacement_at(
        &first_prices,
        &first_latent_mids,
        &sides,
        tick,
        configured_displacement_ticks,
    );
    let (_, latent_displacement_last) = separation_and_displacement_at(
        &last_prices,
        &first_latent_mids,
        &sides,
        tick,
        configured_displacement_ticks,
    );

    let mut separation: std::collections::BTreeMap<i64, u64> = std::collections::BTreeMap::new();
    let mut opposite = 0_u64;
    for i in 1..last_prices.len() {
        if sides[i] != sides[i - 1] {
            let ticks = ((last_prices[i] - last_prices[i - 1]).abs() / tick).round() as i64;
            *separation.entry(ticks).or_default() += 1;
            opposite += 1;
        }
    }
    let parent_prices = last_prices;

    // Counterfactual same-mid separation: how far apart a buy print and a sell
    // print would land at the same latent mid, as a function of grid phase. Not
    // an observable market statistic - a mechanism diagnostic - because no two
    // prints share a mid. `ask`/`bid` name the two sides of the counterfactual,
    // not a book: the generator publishes none.
    let mut phase_hist: Vec<(f64, u32)> = Vec::new();
    for step in 0..8_u32 {
        let phase = f64::from(step) / 8.0;
        let width = f64::from(scalars.quoted_width.ticks().get());
        let bid = (10.0 + phase - width / 2.0).round();
        let book_mid = bid + width / 2.0;
        let ask = (book_mid + scalars.trade_displacement_ticks.ticks()).round();
        let bid_print = (book_mid - scalars.trade_displacement_ticks.ticks()).round();
        phase_hist.push((phase, (ask - bid_print) as u32));
    }

    let zero_change = parent_prices
        .windows(2)
        .filter(|w| (w[1] - w[0]).abs() < f64::EPSILON)
        .count() as f64
        / (parent_prices.len() - 1) as f64;
    let scale =
        (base_returns.iter().map(|r| r * r).sum::<f64>() / base_returns.len() as f64).sqrt();
    let tick_return = (1.0 + tick / decimal_to_f64(scalars.start_price)).ln();

    SpreadDecomposition {
        configured_quoted_width_ticks: f64::from(scalars.quoted_width.ticks().get()),
        same_mid_separation_ticks: phase_hist,
        roll_first_child,
        roll_last_child,
        roll_all_prints,
        same_mid_separation_at_first_child: separation_first,
        same_mid_separation_at_last_child: separation_last,
        effective_spread_at_first_child: displacement_first,
        effective_spread_at_last_child: displacement_last,
        latent_mid_relative_displacement_at_first_child: latent_displacement_first,
        latent_mid_relative_displacement_at_last_child: latent_displacement_last,
        opposite_side_separation: separation
            .into_iter()
            .map(|(ticks, count)| (ticks, count as f64 / opposite as f64))
            .collect(),
        zero_change_frac: zero_change,
        accepted_repeat_frac: accepted_repeats as f64 / parent_prices.len() as f64,
        repeated_parents,
        fresh_parents,
        conditional_return_scale: scale,
        tick_return,
        tick_traversal_sigma_units: tick_return / scale,
    }
}

/// The XBTUSD anchor re-gridded onto MNQ's quarter-point tick and price level.
///
/// Shared by every diagnostic that reports on both grids, so they measure the
/// same second grid by construction rather than by two copies of these five
/// assignments agreeing. It is not a preset: the arrival, size and session
/// scalars stay crypto's, because the point is to vary the grid alone.
fn mnq_shaped_grid(fp: &Fingerprint) -> GeneratorScalars {
    let mut index = GeneratorScalars::xbtusd_anchor(fp);
    index.symbol = "MNQ-SHAPED".to_string();
    index.modal_tick = Decimal::new(25, 2);
    index.price_decimals = 2;
    index.start_price = Decimal::from(21_000);
    index.latent_size_median = Decimal::ONE;
    index
}

// Measurement instrument. Its name is in the gate profile's `skip` list in
// the workspace runner config, and has to be: the gate sets `include_ignored`
// deliberately - that is how the socket-backed suites get covered - so
// `#[ignore]` does not keep this out of it. Every instrument of this shape must
// be added to that list as well. Nothing detects the omission; the symptom is
// the full gate dying on the 20-second per-test hang watchdog, blaming a test
// that was never meant to run there.
//
// Run it deliberately by name, raising the watchdog for that one run:
//   test -p mogwai-data synthetic_spread_decomposition_at_protocol_seven --timeout 280
// `--timeout` applies to the focused runner only, takes 1 to 280 seconds, and
// is refused when the name matches more than one test. 280 is a hard cap. See
// AGENTS.md for the runner these arguments belong to.
#[test]
#[ignore = "spread decomposition report; run deliberately with an extended timeout"]
fn synthetic_spread_decomposition_at_protocol_seven() {
    let fp = Fingerprint::from_repo_json();
    // Two grids, because every quantity here is grid-dependent and reporting
    // one would invite the same over-generalization the report already made.
    let crypto = GeneratorScalars::xbtusd_anchor(&fp);
    let index = mnq_shaped_grid(&fp);

    for (label, scalars) in [("XBTUSD anchor", &crypto), ("MNQ-shaped grid", &index)] {
        let d = decompose_spread(scalars, 400_000);
        println!();
        println!("=== {label}");
        println!(
            "  TAPE_PROTOCOL_VERSION            {}",
            crate::TAPE_PROTOCOL_VERSION
        );
        println!(
            "  configured quoted width          {:.2} ticks",
            d.configured_quoted_width_ticks
        );
        print!("  same-mid grid separation         ");
        for (phase, sep) in &d.same_mid_separation_ticks {
            print!("f={phase:.3}:{sep}t ");
        }
        println!();
        for (name, est) in [
            ("roll_first_child", d.roll_first_child),
            ("roll_last_child", d.roll_last_child),
            ("roll_all_prints", d.roll_all_prints),
        ] {
            match est.ticks {
                Some(ticks) => println!(
                    "  {name:<32} {ticks:>9.3} ticks  (cov {:.4e})",
                    est.covariance
                ),
                None => println!(
                    "  {name:<32} UNAVAILABLE        (cov {:.4e} >= 0)",
                    est.covariance
                ),
            }
        }
        println!(
            "  same_mid_sep_at_first_child      {:>9.3} ticks",
            d.same_mid_separation_at_first_child
        );
        println!(
            "  same_mid_sep_at_last_child       {:>9.3} ticks",
            d.same_mid_separation_at_last_child
        );
        println!(
            "  effective_spread_first_child     {:>9.3} ticks  (vs PUBLISHED book mid)",
            d.effective_spread_at_first_child
        );
        println!(
            "  effective_spread_last_child      {:>9.3} ticks  (vs PUBLISHED book mid)",
            d.effective_spread_at_last_child
        );
        println!(
            "  latent_mid_relative_first_child  {:>9.3} ticks  (unobservable)",
            d.latent_mid_relative_displacement_at_first_child
        );
        println!(
            "  latent_mid_relative_last_child   {:>9.3} ticks  (unobservable)",
            d.latent_mid_relative_displacement_at_last_child
        );
        print!("  opposite-side parent separation  ");
        for (ticks, share) in d.opposite_side_separation.iter().take(7) {
            print!("{ticks}t:{:.1}% ", 100.0 * share);
        }
        println!();
        println!(
            "  zero_change_frac                 {:.4}",
            d.zero_change_frac
        );
        println!(
            "  accepted_repeat_frac             {:.4}",
            d.accepted_repeat_frac
        );
        // Reported apart, never pooled and never summarized into one number: a
        // repeat parent prices against a frozen book and a fresh one against the
        // book just published, and the whole point of separating them is that
        // the gap between these two rows is readable.
        for (label, cohort) in [("repeated", d.repeated_parents), ("fresh", d.fresh_parents)] {
            match cohort {
                Some(cohort) => println!(
                    "  effective_spread_{label:<8} parents {:>9} {:>9.3} ticks",
                    cohort.parents, cohort.effective_spread_ticks
                ),
                None => println!(
                    "  effective_spread_{label:<8} parents UNAVAILABLE: none in the sample"
                ),
            }
        }
        println!(
            "  conditional return scale         {:.4e}",
            d.conditional_return_scale
        );
        println!("  tick_return                      {:.4e}", d.tick_return);
        println!(
            "  tick_return / return scale       {:.3}",
            d.tick_traversal_sigma_units
        );
    }

    // The invariant this report exists to keep honest: Roll is unavailable at
    // the print layer, on every grid. Sweep children walk monotonically in one
    // direction, so consecutive child price changes are positively correlated
    // and the estimator's sign assumption fails outright. A harness that
    // computed it there would silently return a number from a square root of a
    // negative quantity, or worse, coerce it to zero.
    // The stratified matrix, on the grid where both parent estimators return a
    // value. Quote age is a single column here and is named rather than set to
    // zero: this generator emits no quote at all, so there is no age to report,
    // and calling that "0 ms" would imply an observed transport timestamp that
    // does not exist and invite comparison against real zero-age joins, which
    // are a different thing.
    // Both grids, because print dependence changes materially with
    // tick_return / return_scale. Judging a real BTCUSDT result only against an
    // MNQ-shaped synthetic tape compares two different regimes and calls the
    // difference a finding.
    for (grid_label, grid) in [("MNQ-shaped", &index), ("BTCUSDT-shaped", &crypto)] {
        stratified_matrix(grid_label, grid);
    }

    for scalars in [&crypto, &index] {
        let d = decompose_spread(scalars, 400_000);
        assert!(
            d.roll_all_prints.ticks.is_none(),
            "roll_all_prints must be UNAVAILABLE on every grid: cov={}",
            d.roll_all_prints.covariance
        );
        assert!(d.accepted_repeat_frac > 0.0);
    }
    // Nothing else is asserted. The ordering between the first-child and
    // last-child estimators, and their position relative to the same-mid
    // print separation,
    // are grid-dependent observations rather than invariants: in real data
    // either can land on either side of quoted spread depending on side
    // persistence, latent movement, burst grouping and within-burst book
    // changes. Pinning an observed relationship is how a schema loses the
    // ability to report a surprise.
}

/// Calendar-aware normalization must not move a calendar-free tape by one ulp.
///
/// Without a calendar every minute is exposed, and the schema's sum-to-one
/// contract makes the week-mean of `intensity_hour * 24 * dow_weight * 7`
/// exactly 1.0. So the normalizer is not an approximation of that value, it is
/// that value itself, and the modulator returns a literal `1.0` rather than recomputing
/// it - a floating-point sum could land a few ulps away and perturb every gap in
/// every existing operator profile, which is not something a protocol migration
/// may do silently to configurations it was not aiming at.
///
/// The three shipped crypto presets and the fingerprint anchor carry no
/// calendar, so this is the gate that says the MNQ session work left them alone.
/// `clean_regime_is_byte_identical` would catch a regression here too, but it
/// would report it as a golden mismatch rather than as what it is.
#[test]
fn calendar_free_profiles_are_untouched_by_calendar_aware_normalization() {
    let fp = Fingerprint::from_repo_json();
    for scalars in [
        GeneratorScalars::xbtusd_anchor(&fp),
        GeneratorScalars::from_fingerprint_medians("BTCUSDT", &fp),
        mnq_shaped_grid(&fp),
    ] {
        let symbol = scalars.symbol.clone();
        let mut source = GeneratedSource::new(scalars.clone(), 42, 1_000, &fp, None);
        // The multiplier the legacy arithmetic produces, reproduced here from
        // the profile alone so the assertion does not just restate the code.
        for hour in 0..24_u64 {
            for day in 0..7_u64 {
                let clock_ns = (day * 24 + hour) * 3_600_000_000_000;
                // The epoch fell on a Thursday, so the day index is not `day`.
                // Re-derived here rather than borrowed from `utc_hour_dow`, so
                // the test pins the convention instead of restating it.
                let dow = ((clock_ns / 86_400_000_000_000 + 4) % 7) as usize;
                // Associated exactly as the modulator associates it - the claim
                // is bit equality, and a different grouping of the same three
                // multiplications can differ in the last place.
                let expected = (fp.session_profile.intensity_hour[hour as usize] * 24.0)
                    * (fp.session_profile.dow_weight[dow] * 7.0);
                assert_eq!(
                    source.arrival_mult_for_test(clock_ns),
                    expected,
                    "{symbol} hour {hour} dow {dow} arrival multiplier moved"
                );
                assert_eq!(
                    source.vol_mult_for_test(clock_ns),
                    fp.session_profile.vol_hour[hour as usize],
                    "{symbol} hour {hour} volatility multiplier moved"
                );
            }
        }
        // And the stream itself, not only the multipliers.
        let mut twin = GeneratedSource::new(scalars, 42, 1_000, &fp, None);
        for _ in 0..2_000 {
            assert_eq!(
                format!("{:?}", source.next_tick()),
                format!("{:?}", twin.next_tick())
            );
        }
    }
}

#[test]
fn a_quote_precedes_every_parent_burst() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut src = GeneratedSource::new(scalars, 7, 0, &fp, None);
    let mut need_quote = true;
    for _ in 0..20_000 {
        match src.next_tick().expect("infinite tape") {
            TickEvent::Quote(_) => {
                assert!(need_quote, "only one quote may govern a parent burst");
                need_quote = false;
            }
            TickEvent::Trade(_) => {
                assert!(
                    !need_quote,
                    "a parent trade arrived without a governing quote"
                );
                if src.burst.remaining == 0 {
                    need_quote = true;
                }
            }
        }
    }
}

#[test]
fn published_books_govern_parent_trades_and_stay_on_grid() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let tick = scalars.modal_tick;
    let mut src = GeneratedSource::new(scalars, 19, 0, &fp, None);
    let mut previous_quote: Option<mogwai_protocol::QuoteTick> = None;
    let mut previous_last_price = None;
    for _ in 0..20_000 {
        let TickEvent::Quote(quote) = src.next_tick().expect("parent quote") else {
            panic!("each parent begins with a quote");
        };
        assert_eq!((quote.bid_px / tick).fract(), Decimal::ZERO);
        assert_eq!((quote.ask_px / tick).fract(), Decimal::ZERO);
        assert_eq!((quote.ask_px - quote.bid_px) / tick, Decimal::ONE);
        let trade = next_trade(&mut src);
        let repeated = previous_quote.as_ref().is_some_and(|prior| {
            prior.bid_px == quote.bid_px
                && prior.ask_px == quote.ask_px
                && previous_last_price == Some(trade.price)
        });
        match trade.aggressor {
            AggressorSide::Buyer => {
                if !repeated {
                    assert_eq!(trade.price, quote.ask_px);
                }
                assert!(trade.price * Decimal::from(2) >= quote.bid_px + quote.ask_px);
            }
            AggressorSide::Seller => {
                if !repeated {
                    assert_eq!(trade.price, quote.bid_px);
                }
                assert!(trade.price * Decimal::from(2) <= quote.bid_px + quote.ask_px);
            }
            AggressorSide::NoAggressor => panic!("generated parents have a native side"),
        }
        let mut last_price = trade.price;
        while src.burst.remaining > 0 {
            last_price = next_trade(&mut src).price;
        }
        previous_quote = Some(quote);
        previous_last_price = Some(last_price);
    }
}

#[test]
fn every_trade_has_a_governing_quote_at_or_before_it() {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 31, 0, &fp, None);
    let mut governing_ts = None;
    for _ in 0..100_000 {
        match src.next_tick().unwrap() {
            TickEvent::Quote(quote) => governing_ts = Some(quote.ts_event),
            TickEvent::Trade(trade) => assert!(
                governing_ts.is_some_and(|quote_ts| quote_ts <= trade.ts_event),
                "trade {} had no governing quote",
                trade.ts_event
            ),
        }
    }
}

/// A river never prints two quotes at one instant either, and this is strict
/// for the same reason the trade gate is.
///
/// It used to assert non-decreasing, which permits a tie - and a tie on the
/// quote stream is exactly what a socket history page cannot survive. History
/// paginates by an inclusive per-kind cutoff and the consumer splices live rows
/// by dropping those at or below it, so two quotes sharing an instant across a
/// page boundary either lose one row or duplicate it, with no way for either
/// side to tell which happened. The trade stream has carried that guarantee for
/// a while; the quote stream was merely believed to.
///
/// Cross-kind ties stay legal and harmless: a quote and its first child trade
/// share an instant by design, and the two kinds are separate streams with
/// separate cutoffs, so a tie across them can never cut either.
///
/// Walked over every shipped preset rather than one river, because the property
/// is a claim about what this venue serves and a single-river witness would not
/// notice a preset whose cadence collapses the quote stride.
#[test]
fn a_river_never_prints_two_quotes_at_one_instant() {
    let fp = Fingerprint::from_repo_json();
    for def in mogwai_protocol::default_instruments() {
        let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, &fp);
        scalars.modal_tick = def.price_increment;
        scalars.price_decimals = u32::from(def.price_precision);
        let seed = mogwai_protocol::RunSeeds::from_run_seed(32).tape_for(&def.symbol);
        let mut src = GeneratedSource::new(scalars, seed, 0, &fp, None);
        let mut prior: Option<u64> = None;
        let mut quotes = 0;
        while quotes < 20_000 {
            let Some(tick) = src.next_tick() else { break };
            if let TickEvent::Quote(quote) = tick {
                if let Some(prior) = prior {
                    assert!(
                        quote.ts_event > prior,
                        "{} printed two quotes at {}",
                        def.symbol,
                        quote.ts_event
                    );
                }
                prior = Some(quote.ts_event);
                quotes += 1;
            }
        }
        assert_eq!(quotes, 20_000, "{} must produce a full sample", def.symbol);
    }
}

#[test]
fn non_repeat_parents_print_at_the_touch_at_unit_width() {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 33, 0, &fp, None);
    let mut previous_last = None;
    let mut previous_book = None;
    for _ in 0..20_000 {
        let TickEvent::Quote(quote) = src.next_tick().unwrap() else {
            panic!()
        };
        let parent = next_trade(&mut src);
        let repeated = previous_last == Some(parent.price)
            && previous_book == Some((quote.bid_px, quote.ask_px));
        if !repeated {
            assert_eq!(
                parent.price,
                match parent.aggressor {
                    AggressorSide::Buyer => quote.ask_px,
                    AggressorSide::Seller => quote.bid_px,
                    AggressorSide::NoAggressor => panic!(),
                }
            );
        }
        let mut last = parent.price;
        while src.burst.remaining > 0 {
            last = next_trade(&mut src).price;
        }
        previous_last = Some(last);
        previous_book = Some((quote.bid_px, quote.ask_px));
    }
}

#[test]
fn a_repeat_parent_republishes_the_frozen_book() {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 34, 0, &fp, None);
    let mut previous_last = None;
    let mut previous_quote: Option<mogwai_protocol::QuoteTick> = None;
    for _ in 0..200_000 {
        let TickEvent::Quote(quote) = src.next_tick().unwrap() else {
            panic!()
        };
        let parent = next_trade(&mut src);
        if previous_last == Some(parent.price)
            && previous_quote.as_ref().is_some_and(|prior| {
                prior.bid_px == quote.bid_px
                    && prior.ask_px == quote.ask_px
                    && prior.bid_sz == quote.bid_sz
                    && prior.ask_sz == quote.ask_sz
            })
        {
            assert!(previous_quote.as_ref().unwrap().ts_event < quote.ts_event);
            return;
        }
        let mut last = parent.price;
        while src.burst.remaining > 0 {
            last = next_trade(&mut src).price;
        }
        previous_last = Some(last);
        previous_quote = Some(quote);
    }
    panic!("no accepted repeat observed");
}

#[test]
fn an_incompatible_repeat_is_rejected() {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 35, 0, &fp, None);
    while src.rejected_repeat_draws == 0 {
        src.next_tick().unwrap();
        assert!(
            src.repeat_draws < 1_000_000,
            "no incompatible repeat was exercised"
        );
    }
    assert!(src.rejected_repeat_draws <= src.repeat_draws);
}

#[test]
fn repeat_compatibility_uses_the_frozen_governing_book() {
    let frozen = PublishedBook {
        bid_ticks: 100.0,
        ask_ticks: 101.0,
        bid_size: Decimal::ONE,
        ask_size: Decimal::ONE,
    };
    let fresh = PublishedBook {
        bid_ticks: 105.0,
        ask_ticks: 106.0,
        bid_size: Decimal::ONE,
        ask_size: Decimal::ONE,
    };
    let previous_sweep_last = 104.0;
    assert!(repeat_is_compatible(
        previous_sweep_last,
        &fresh,
        AggressorSide::Seller
    ));
    assert!(!repeat_is_compatible(
        previous_sweep_last,
        &frozen,
        AggressorSide::Seller
    ));
}

#[test]
fn configured_uncalibrated_top_sizes_are_published_unchanged() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.top_sizes = TopOfBookSizes::uncalibrated(Decimal::TEN);
    let mut source = GeneratedSource::new(scalars, 91, 0, &fp, None);
    let TickEvent::Quote(quote) = source.next_tick().unwrap() else {
        panic!()
    };
    assert_eq!(quote.bid_sz, Decimal::TEN);
    assert_eq!(quote.ask_sz, Decimal::TEN);
}

#[test]
fn no_parent_print_crosses_the_wrong_side_of_its_governing_book() {
    let fp = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 36, 0, &fp, None);
    for _ in 0..20_000 {
        let TickEvent::Quote(quote) = src.next_tick().unwrap() else {
            panic!()
        };
        let parent = next_trade(&mut src);
        let twice_price = parent.price * Decimal::from(2);
        match parent.aggressor {
            AggressorSide::Buyer => assert!(twice_price >= quote.bid_px + quote.ask_px),
            AggressorSide::Seller => assert!(twice_price <= quote.bid_px + quote.ask_px),
            AggressorSide::NoAggressor => panic!(),
        }
        while src.burst.remaining > 0 {
            let _ = next_trade(&mut src);
        }
    }
}

#[test]
fn published_book_prices_are_on_the_tick_grid() {
    let fp = Fingerprint::from_repo_json();
    for scalars in [GeneratorScalars::xbtusd_anchor(&fp), mnq_shaped_grid(&fp)] {
        let tick = scalars.modal_tick;
        let mut src = GeneratedSource::new(scalars, 37, 0, &fp, None);
        let mut quotes = 0;
        while quotes < 20_000 {
            if let TickEvent::Quote(quote) = src.next_tick().unwrap() {
                assert_eq!((quote.bid_px / tick).fract(), Decimal::ZERO);
                assert_eq!((quote.ask_px / tick).fract(), Decimal::ZERO);
                quotes += 1;
            }
        }
    }
}

#[test]
fn realized_displacement_changes_only_at_the_grid_boundaries() {
    let book = PublishedBook {
        bid_ticks: 100.0,
        ask_ticks: 101.0,
        bid_size: Decimal::ONE,
        ask_size: Decimal::ONE,
    };
    let mid = book_mid_ticks(&book);
    for displacement in [0.01, 0.25, 0.49, 0.5] {
        assert_eq!((mid + displacement).round(), 101.0);
        assert_eq!((mid - displacement).round(), 100.0);
    }
    assert_eq!((mid + 0.51).round(), 101.0);
    assert_eq!((mid + 1.5).round(), 102.0);
    assert_eq!((mid - 1.5).round(), 99.0);
}

#[test]
fn width_and_displacement_are_independently_settable() {
    let fp = Fingerprint::from_repo_json();
    let narrow = GeneratorScalars::xbtusd_anchor(&fp);
    let mut wide = narrow.clone();
    wide.quoted_width = QuotedWidth::uncalibrated(std::num::NonZeroU32::new(3).unwrap());
    let mut displaced = narrow.clone();
    displaced.trade_displacement_ticks = TradeDisplacement::uncalibrated(1.5);
    let mut narrow_source = GeneratedSource::new(narrow.clone(), 4, 0, &fp, None);
    let mut wide_source = GeneratedSource::new(wide, 4, 0, &fp, None);
    let mut displaced_source = GeneratedSource::new(displaced, 4, 0, &fp, None);
    let TickEvent::Quote(narrow_quote) = narrow_source.next_tick().unwrap() else {
        panic!()
    };
    let TickEvent::Quote(wide_quote) = wide_source.next_tick().unwrap() else {
        panic!()
    };
    let TickEvent::Quote(displaced_quote) = displaced_source.next_tick().unwrap() else {
        panic!()
    };
    assert_eq!(
        wide_quote.ask_px - wide_quote.bid_px,
        narrow.modal_tick * Decimal::from(3)
    );
    assert_eq!(
        displaced_quote.ask_px - displaced_quote.bid_px,
        narrow_quote.ask_px - narrow_quote.bid_px
    );
    let baseline = next_trade(&mut narrow_source);
    let moved = next_trade(&mut displaced_source);
    assert_ne!(baseline.price, moved.price);
}

#[test]
fn quote_construction_consumes_no_randomness() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut control = GeneratedSource::new(scalars.clone(), 55, 0, &fp, None);
    let mut probed = GeneratedSource::new(scalars, 55, 0, &fp, None);
    for _ in 0..1_000 {
        let _ = probed.diagnostic_place_book();
    }
    for _ in 0..10_000 {
        assert_eq!(
            format!("{:?}", control.next_tick()),
            format!("{:?}", probed.next_tick())
        );
    }
}

#[test]
fn a_resumed_source_reproduces_quotes_identically() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut source = GeneratedSource::new(scalars, 88, 0, &fp, None);
    for _ in 0..1_337 {
        source.next_tick();
    }
    let mut resumed = source.clone();
    for _ in 0..10_000 {
        assert_eq!(
            format!("{:?}", source.next_tick()),
            format!("{:?}", resumed.next_tick())
        );
    }
}

#[test]
fn the_realized_repeat_rate_and_zero_change_frac_are_recorded() {
    let fp = Fingerprint::from_repo_json();
    for scalars in [GeneratorScalars::xbtusd_anchor(&fp), mnq_shaped_grid(&fp)] {
        let measured = decompose_spread(&scalars, 100_000);
        assert!(
            fp.golden_targets
                .zero_change_frac
                .range
                .contains(measured.zero_change_frac),
            "{} zero_change_frac={} left the committed band",
            scalars.symbol,
            measured.zero_change_frac
        );
        assert!(measured.accepted_repeat_frac > 0.0);
    }
}

/// The trade displacement is frozen, in every bounce regime and on every grid.
///
/// The companion diagnosis to the one above, and the reason item A of the
/// blocker is worth stating separately: the storage seam is already per-instance
/// (`BounceState::trade_bounce_ticks`, read fresh by `next_price` on every
/// event), so making the displacement respond to volatility is a mutation rather
/// than a restructuring. What is missing is the response, not the slot.
///
/// The run is long enough to cross into the high bounce regime many times over
/// (its stationary share is 0.3125, with a mean run of about 33 events), so a
/// regime-conditional displacement would be caught here.
#[test]
fn the_trade_displacement_never_varies() {
    let fp = Fingerprint::from_repo_json();
    let crypto = GeneratorScalars::xbtusd_anchor(&fp);
    let index = mnq_shaped_grid(&fp);

    for scalars in [crypto, index] {
        let symbol = scalars.symbol.clone();
        let mut src = GeneratedSource::new(scalars, 11, 0, &fp, None);
        let mut saw_high_regime = false;
        for _ in 0..20_000 {
            src.next_tick().expect("infinite tape");
            saw_high_regime |= src.bounce.high_regime;
            assert!(
                (src.bounce.trade_bounce_ticks - TRADE_BOUNCE_HALF_WIDTH_TICKS).abs()
                    < f64::EPSILON,
                "{symbol}: displacement moved to {} from {TRADE_BOUNCE_HALF_WIDTH_TICKS}",
                src.bounce.trade_bounce_ticks
            );
        }
        // Without this the assertion above could pass on a tape that never left
        // the low regime, which would prove nothing about regime dependence.
        assert!(saw_high_regime, "{symbol}: never entered the high regime");
    }
}

fn stratified_matrix(grid_label: &str, index: &GeneratorScalars) {
    let fp2 = Fingerprint::from_repo_json();
    let mut src = GeneratedSource::new(index.clone(), 42, 0, &fp2, None);
    let tick = decimal_to_f64(index.modal_tick);
    let mut first: Vec<f64> = Vec::new();
    let mut last: Vec<f64> = Vec::new();
    let mut prints: Vec<f64> = Vec::new();
    // Which parent event each print belongs to, so the all-print series can be
    // stratified in the shared parent-event currency rather than in its own.
    let mut parent_of_print: Vec<usize> = Vec::new();
    let mut parent_index = 0_usize;
    while last.len() < 400_000 {
        let TickEvent::Trade(t) = src.next_tick().expect("infinite tape") else {
            continue;
        };
        let starts = src.burst.emitted == 1;
        let p = decimal_to_f64(t.price);
        prints.push(p);
        parent_of_print.push(parent_index);
        if starts {
            first.push(p);
        }
        if src.burst.remaining == 0 {
            last.push(p);
            parent_index += 1;
        }
    }
    first.truncate(last.len());

    // Boundaries are computed once, from the last-child series, and reused for
    // every convention so the conventions stay comparable to one another.
    let vols = trailing_vol(&last);
    let boundaries = vol_boundaries(&vols);
    println!();
    println!("=== stratified matrix: {grid_label} grid");
    println!("  quote-age dimension: contemporaneous_model_quote");
    println!("  observable axis: trailing realized vol over {TRAILING_VOL_EVENTS} parent events,");
    println!("  parent-event returns, horizon fixed before results were examined");
    print!("  global quantile boundaries:");
    for (q, b) in VOL_STRATUM_QUANTILES.iter().zip(&boundaries) {
        print!(" p{:.0}={b:.4e}", q * 100.0);
    }
    println!();
    println!(
        "  cells fail closed below {MIN_COVARIANCE_PAIRS} covariance pairs; a pair belongs to the \
         LATER of its"
    );
    println!(
        "  two changes, stratified by volatility observable strictly BEFORE that change - no \
         lookahead"
    );
    println!();
    println!(
        "  {:<18} {:<10} {:>12} {:>10} {:>14}",
        "convention", "stratum", "roll ticks", "pairs", "covariance"
    );
    // Every convention is cut at the same boundaries, in the same parent-event
    // currency. The print series borrows the parent volatility rather than
    // computing its own, which is the correction: a print-denominated horizon
    // cut at parent-denominated boundaries is not the same quantity.
    let parent_vols = parent_change_vols(&vols);
    let print_vols = print_change_vols(&parent_of_print, &vols);
    for (name, series, change_vols) in [
        ("roll_first_child", &first, &parent_vols),
        ("roll_last_child", &last, &parent_vols),
        ("roll_all_prints", &prints, &print_vols),
    ] {
        for (s, label) in VOL_STRATUM_NAMES.iter().enumerate() {
            let (est, pairs) = roll_in_stratum(
                series,
                change_vols,
                &boundaries,
                s,
                tick,
                MIN_COVARIANCE_PAIRS,
            );
            match est.ticks {
                Some(t) => println!(
                    "  {name:<18} {label:<10} {t:>12.3} {pairs:>10} {:>14.4e}",
                    est.covariance
                ),
                None if pairs < MIN_COVARIANCE_PAIRS => println!(
                    "  {name:<18} {label:<10} {:>12} {pairs:>10} {:>14}",
                    "FAIL-CLOSED", "sparse"
                ),
                None => println!(
                    "  {name:<18} {label:<10} {:>12} {pairs:>10} {:>14.4e}",
                    "UNAVAILABLE", est.covariance
                ),
            }
        }
    }
}

fn is_round_lot(size: Decimal, median: f64) -> bool {
    let lot = 10.0_f64.powf(median.log10().floor());
    let units = decimal_to_f64(size) / lot;
    (units - units.round()).abs() < 1e-6
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

// ---------------------------------------------------------------------------
// GARCH second-moment instrumentation
//
// The recursion is a textbook GARCH(1,1), and `GarchVol::new` sets
// `a0 = vol_scalar^2 * (1 - a1 - b1)` so `vol_scalar^2` is the unconditional
// variance. That derivation assumes a unit-variance innovation, and the
// second-moment condition it needs is `a1 * E[z^2] + b1 < 1`.
//
// As shipped, that condition holds. `next_latent_mid` divides the raw
// Student-t(df) by `STUDENT_T_UNIT_SCALE` before the innovation reaches the
// recursion, so `E[z^2] = 1` and the condition is `a1 + b1` = 0.999. The first
// arm below is that process. Its label used to say `"COUNTERFACTUAL"` while the
// raw arm said `"AS SHIPPED"`, exactly inverted, for two repairs after the
// standardization landed - which is the shape this file keeps finding
// elsewhere: a legible, quotable report describing a world that stopped
// existing.
//
// The second arm is the same shipped parameters with the standardization
// switched off, and it is kept because it is why the standardization is
// load-bearing rather than cosmetic. Unstandardized, the innovation is a raw
// Student-t(4) of variance `df / (df - 2)` = 2, so the condition reads
// `a1 * 2 + b1` = 1.019 at today's `a1` and `b1` - no finite stationary
// variance, bounded only because `sigma2` is capped and the feedback return
// clamped.
//
// It is not a reconstruction of the pre-standardization era and must not be
// labelled as one. That era ran `a1` 0.12 and `b1` 0.875, giving 1.115, at a
// `vol_scalar` of 1e-6; `run_garch_harness` builds `GarchVol::new`, which reads
// the shipped `GARCH_ARCH` and `GARCH_GARCH` and offers no override, so no arm
// reachable from here can be that process. An earlier repair drove this arm at
// the era's 1e-6 and captioned it accordingly, which printed a triple that
// never shipped in any era under a historical banner - the same defect, one
// layer down. Reconstructing the era needs an `a1`/`b1` override hook, and
// nothing here asks a question that would justify adding one.
//
// This harness measures what the recursion does, driving the shipped
// `GarchVol::step` and `draw_student_t` rather than re-deriving them. Prices,
// bounce, sweeps and the session envelope are bypassed on purpose: reading this
// off emitted prices does not work, because the almost-sure two-tick bounce is
// larger than the latent return it would have to reveal.
//
// What it does not cover, so nobody reads it as covering it: the harness makes
// its own standardization decision, so de-standardizing production would leave
// every assertion here green. That property belongs to
// `trace_consumes_no_draws_and_leaves_the_tape_byte_identical`, which asserts
// `innovation_raw / innovation_std == STUDENT_T_UNIT_SCALE` against the shipped
// walk. What this file owns is the consequence of the choice - stationarity,
// rail occupancy and whether `VOL_SCALAR` means what it says.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
struct GarchReport {
    updates: u64,
    candidate_over_cap: u64,
    at_cap: u64,
    longest_cap_run: u64,
    feedback_clamped: u64,
    first_cap_hit: Option<u64>,
    mean_scale: f64,
    /// `E[sigma2]`, the quantity `GarchVol::new` intends to equal
    /// `vol_scalar^2`. Reported alongside the mean scale because the two are
    /// not interchangeable: sigma2 is heavily right-skewed at this
    /// persistence, so `E[sqrt(sigma2)]` sits well below `sqrt(E[sigma2])` and
    /// comparing the former against `vol_scalar` understates the fit.
    mean_sigma2: f64,
    mean_z2: f64,
    scale_p50: f64,
    scale_p99: f64,
    /// Median of `GarchStep::unclipped_conditional_sd`, taken from the shipped
    /// method rather than recomputed here: the point of the exercise is that
    /// the scale and the conditional SD are different numbers.
    unclipped_sd_p50: f64,
}

impl GarchReport {
    fn frac(count: u64, total: u64) -> f64 {
        if total == 0 {
            0.0
        } else {
            count as f64 / total as f64
        }
    }
    fn cap_occupancy(&self) -> f64 {
        Self::frac(self.at_cap, self.updates)
    }
    fn clamp_occupancy(&self) -> f64 {
        Self::frac(self.feedback_clamped, self.updates)
    }
    /// `a1 * E[z^2] + b1`, estimated from realized draws rather than assumed.
    fn effective_persistence(&self) -> f64 {
        super::consts::GARCH_ARCH * self.mean_z2 + super::consts::GARCH_GARCH
    }
}

/// Drive the shipped recursion for `total` updates after `burn_in`. When
/// `standardize` is set the innovation is divided by `STUDENT_T_UNIT_SCALE`,
/// which is what `next_latent_mid` does; clearing it drives the same shipped
/// `a1` and `b1` with an unstandardized innovation. `a1` and `b1` are not
/// overridable here - `GarchVol::new` reads them from `consts.rs` - so the
/// cleared arm is today's parameters without the standardization, never a
/// reconstruction of the pre-standardization era. The divisor is the shipped
/// constant rather than a local `sqrt(df / (df - 2))` so a change to either is
/// felt here.
fn run_garch_harness(vol_scalar: f64, burn_in: u64, total: u64, standardize: bool) -> GarchReport {
    use rand::SeedableRng;
    use rand_distr::{ChiSquared, Normal};

    let mut vol = super::dynamics::GarchVol::new(21_000.0, vol_scalar);
    let mut rng = rand_chacha::ChaCha12Rng::seed_from_u64(42);
    let normal = Normal::new(0.0, 1.0).expect("valid normal");
    let chi_squared = ChiSquared::new(super::consts::STUDENT_T_DF).expect("valid chi-squared");
    let unit = super::consts::STUDENT_T_UNIT_SCALE;

    let mut report = GarchReport::default();
    let mut scales: Vec<f64> = Vec::with_capacity(total as usize);
    let mut conditional_sds: Vec<f64> = Vec::with_capacity(total as usize);
    let mut run = 0_u64;
    for i in 0..(burn_in + total) {
        let raw = super::dynamics::draw_student_t(&mut rng, &normal, &chi_squared);
        let z = if standardize { raw / unit } else { raw };
        let step = vol.step(z);
        if step.hit_variance_cap() && report.first_cap_hit.is_none() {
            report.first_cap_hit = Some(i);
        }
        if i < burn_in {
            continue;
        }
        report.updates += 1;
        report.mean_z2 += z * z;
        report.mean_scale += step.garch_scale;
        report.mean_sigma2 += step.sigma2;
        if step.hit_variance_cap() {
            report.candidate_over_cap += 1;
        }
        if step.hit_feedback_clamp() {
            report.feedback_clamped += 1;
        }
        // "At the cap" is the state that matters for dynamic range: pinned
        // there, the recursion cannot express an upward cluster at all.
        if step.sigma2 >= step.sigma_cap * (1.0 - 1e-12) {
            report.at_cap += 1;
            run += 1;
            report.longest_cap_run = report.longest_cap_run.max(run);
        } else {
            run = 0;
        }
        scales.push(step.garch_scale);
        conditional_sds.push(step.unclipped_conditional_sd());
    }
    let n = report.updates as f64;
    report.mean_z2 /= n;
    report.mean_scale /= n;
    report.mean_sigma2 /= n;
    scales.sort_by(|a, b| a.partial_cmp(b).expect("finite scales"));
    conditional_sds.sort_by(|a, b| a.partial_cmp(b).expect("finite conditional sds"));
    report.scale_p50 = scales[scales.len() / 2];
    report.scale_p99 = scales[scales.len() * 99 / 100];
    report.unclipped_sd_p50 = conditional_sds[conditional_sds.len() / 2];
    report
}

fn print_garch_report(label: &str, vol_scalar: f64, report: &GarchReport) {
    let unit = STUDENT_T_UNIT_SCALE;
    println!("--- {label}");
    println!("  updates                        {}", report.updates);
    println!("  empirical E[z^2]               {:.4}", report.mean_z2);
    println!(
        "  effective persistence a1E+b1   {:.4}  (stationary iff < 1)",
        report.effective_persistence()
    );
    println!("  vol_scalar (intended sigma)    {vol_scalar:.4e}");
    println!("  mean garch_scale sqrt(sigma2)  {:.4e}", report.mean_scale);
    println!("  garch_scale p50                {:.4e}", report.scale_p50);
    println!("  garch_scale p99                {:.4e}", report.scale_p99);
    println!(
        "  unclipped conditional SD p50   {:.4e}  (scale * {unit:.4})",
        report.unclipped_sd_p50
    );
    println!(
        "  sqrt(E[sigma2])                {:.4e}",
        report.mean_sigma2.sqrt()
    );
    println!(
        "  sqrt(E[sigma2]) / vol_scalar   {:.2}x  (1.00x is the intended fit)",
        report.mean_sigma2.sqrt() / vol_scalar
    );
    println!(
        "  sigma2 cap                     {:.4e}",
        GARCH_SIGMA_CAP.powi(2)
    );
    println!(
        "  candidate over cap             {:.2}%",
        100.0 * GarchReport::frac(report.candidate_over_cap, report.updates)
    );
    println!(
        "  cap occupancy                  {:.2}%",
        100.0 * report.cap_occupancy()
    );
    println!(
        "  longest consecutive cap run    {}",
        report.longest_cap_run
    );
    println!(
        "  feedback clamp occupancy       {:.2}%",
        100.0 * report.clamp_occupancy()
    );
    println!(
        "  first cap hit at update        {:?}",
        report.first_cap_hit
    );
}

/// Reports the recursion's realized second-moment behaviour and gates the
/// shipped triple `(GARCH_ARCH, GARCH_GARCH, VOL_SCALAR)` on it, so a future
/// GARCH repair has to re-bless this deliberately rather than quietly changing
/// what the generator's volatility means.
///
/// The unstandardized arm carries no gate on the venue. It is here for the
/// contrast in the printed report and for the one claim that is still live
/// about it, which is stated on its own assertion below.
#[test]
fn garch_second_moment_instrumentation() {
    const BURN_IN: u64 = 100_000;
    const UPDATES: u64 = 1_000_000;

    // Both arms run at `VOL_SCALAR`; see the block comment above for why the
    // second one cannot be captioned as the pre-standardization era. Its two
    // assertions are scale-free, so the `vol_scalar` it is driven at reaches
    // only the printed report - which is exactly why an inaccurate one here was
    // worth removing rather than tolerating.
    let shipped = run_garch_harness(VOL_SCALAR, BURN_IN, UPDATES, true);
    let unstandardized = run_garch_harness(VOL_SCALAR, BURN_IN, UPDATES, false);
    print_garch_report("standardized t(4), AS SHIPPED", VOL_SCALAR, &shipped);
    print_garch_report(
        "raw t(4), TODAY's parameters WITHOUT the standardization",
        VOL_SCALAR,
        &unstandardized,
    );

    assert!(
        (shipped.mean_z2 - 1.0).abs() < 0.25,
        "the shipped innovation must reach the recursion with unit variance, got {}",
        shipped.mean_z2
    );
    assert!(
        shipped.effective_persistence() < 1.0,
        "the shipped recursion must be stationary in second moment: \
         GARCH_ARCH {GARCH_ARCH} * E[z^2] + GARCH_GARCH {GARCH_GARCH} = {}",
        shipped.effective_persistence()
    );
    // The rails are not part of the process at these parameters, and that is
    // the claim most easily lost by a re-solve: a non-stationary recursion is
    // bounded by its rails rather than by its own dynamics, which is what the
    // unstandardized arm below shows at 17.19 percent cap occupancy and 1.46
    // percent feedback clamping, at these same shipped `a1` and `b1`.
    // Occupancy is asserted at zero rather than "small", because at the
    // shipped scale the cap sits 83x above the unconditional scale and nothing
    // in a clean million updates comes near it. The wider-horizon version of
    // this claim, with the measured tail behind it, is
    // `shipped_garch_rails_sit_above_the_clean_tail`.
    assert_eq!(
        (shipped.at_cap, shipped.feedback_clamped),
        (0, 0),
        "no shipped update may be shaped by a rail: {} at the variance cap and \
         {} feedback clamps in {} clean updates",
        shipped.at_cap,
        shipped.feedback_clamped,
        shipped.updates
    );
    // `VOL_SCALAR` means what it says - and the check has to be written around
    // a twentyfold amplification, which is the whole reason this is three lines rather
    // than one.
    //
    // `GarchVol::new` intends `E[sigma2] = VOL_SCALAR^2`, setting
    // `a0 = VOL_SCALAR^2 * (1 - a1 - b1)`. The stationary variance of the
    // recursion is `a0 / (1 - a1 * E[z^2] - b1)`, so the intent is only
    // realized when `E[z^2]` is exactly 1. At the shipped `a1 + b1` of 0.999
    // the denominator is 0.001 and its derivative in `E[z^2]` is `-a1` = -0.02,
    // a twentyfold amplification: `E[z^2]` of 0.99 gives a ratio of 0.913, and
    // 1.01 gives 1.118. So a naive `sqrt(E[sigma2]) / VOL_SCALAR` within 10
    // percent is not a 10 percent band on anything - it is a band of under
    // plus or minus 1 percent on `E[z^2]`, tighter than the unit-variance
    // assertion above by 25x, and it would reject values that assertion
    // deliberately accepts. It cannot be defended statistically either: `z` is
    // a standardized t(4), `z^2` has infinite variance at df 4, so the sample
    // mean has no CLT error bar, and the persistence-0.999 recursion relaxes
    // over ~1000 steps, leaving of order 1000 effective blocks in a million.
    // A `rand_distr` bump or a burn-in change could flip that red with nothing
    // about the generator having moved. A previous draft of this repair asserted
    // exactly that, commented as a 10 percent window; the measured 0.957 sat
    // 43 percent of the way to its lower failure edge.
    //
    // Dividing the amplification out makes it worse, which was worth measuring
    // before writing a number down. Comparing the realized `sqrt(E[sigma2])`
    // against the stationary sd implied by the measured `E[z^2]` - the
    // amplification-corrected form, which is the obvious repair - was tried
    // over three harness seeds, 42, 7 and 1:
    //
    //     seed   E[z^2]   raw ratio   corrected ratio
    //       42   1.0026      0.9571            0.9319
    //        7   0.9982      0.9904            1.0085
    //        1   1.0008      0.9530            0.9455
    //
    // The correction widens the spread, 3.7 points to 7.7, because it feeds
    // `E[z^2]`'s own sampling noise through the same 20x amplification into the
    // target it is comparing against. The residual is not the amplification: it
    // is that `E[sigma2]` is a mean over a persistence-0.999 recursion, whose
    // relaxation time is of order 1000 steps, so a million updates carry of
    // order a thousand effective observations of a heavy-tailed quantity. On
    // that evidence 0.9319 would have sat 68 percent of the way to the 10
    // percent edge - a gate one unlucky seed from red.
    //
    // So the window is wide - half to double - and what it catches was
    // established by biting it rather than by taste. The ratio is close to an
    // identity across the whole stationary interior, because `a0` is defined
    // from `a1` and `b1` to make it one: moving `b1` within that interior
    // barely moves it, so no band of any width gates `a1` and `b1` here, and
    // pretending otherwise was half of what the 10 percent version was doing.
    // What it does catch, measured:
    //
    //   - a broken `a0` derivation. Multiplying `GarchVol::new`'s `a0` by 9
    //     gives 2.87x and fails. That is the actual claim - `VOL_SCALAR^2` is
    //     the unconditional variance - and it is the one nothing else here
    //     asserts.
    //   - loss of unit variance in the innovation. The unstandardized arm below
    //     misses by 80x at these very parameters, and the pre-repair process
    //     missed by 968x. Two orders of magnitude of margin.
    //
    // And the failure mode the tight version would have added, also measured:
    // driving `b1` to 0.9799, still stationary and still green everywhere else,
    // takes the ratio to 0.685 and the amplification-corrected form to 0.474,
    // both downward, opposite to the direction stationary theory predicts,
    // because near the pole a million updates no longer sample the stationary
    // mean at all. A 10 percent window on either form is a red gate for a
    // parameter move that broke nothing.
    //
    // Both ratios are printed so a drift within the band is still visible to a
    // reader, which is the part a tight assertion was pretending to do.
    let stationary_var = VOL_SCALAR.powi(2) * (1.0 - GARCH_ARCH - GARCH_GARCH)
        / (1.0 - GARCH_ARCH * shipped.mean_z2 - GARCH_GARCH);
    let realized_ratio = shipped.mean_sigma2.sqrt() / VOL_SCALAR;
    println!(
        "sqrt(E[sigma2]) / VOL_SCALAR {realized_ratio:.4}x; against the stationary \
         sd at the measured E[z^2] {:.4}x",
        shipped.mean_sigma2.sqrt() / stationary_var.sqrt()
    );
    assert!(
        stationary_var.is_finite() && stationary_var > 0.0,
        "the measured E[z^2] {} puts the recursion past its stationarity pole",
        shipped.mean_z2
    );
    assert!(
        (0.5..2.0).contains(&realized_ratio),
        "sqrt(E[sigma2]) must track VOL_SCALAR to within a factor of two - see \
         the comment above for why the band is this wide and what it is sized \
         to catch - got {realized_ratio}x"
    );

    assert!(
        (unstandardized.mean_z2 - 2.0).abs() < 0.5,
        "an unstandardized t(4) innovation has variance df/(df-2) = 2, got {}",
        unstandardized.mean_z2
    );
    // The one live claim this arm carries: at today's a1 and b1 the
    // unstandardized condition is still above one, so the standardization is
    // what buys stationarity rather than slack in the parameters. If a future
    // re-solve turns this red it has not regressed - it has made the
    // standardization optional, which is a deliberate re-read of both arms.
    assert!(
        unstandardized.effective_persistence() > 1.0,
        "without the standardization today's parameters would still be \
         non-stationary; got {} - see the comment above this assertion before \
         changing it",
        unstandardized.effective_persistence()
    );
}

// ---------------------------------------------------------------------------
// Brick C of the retired protocol-10 successor spec: floor-aware child
// conditioning. July MNQ measured children_mean 1.1711 with a 0.9049
// single fraction; the legacy identity broke below the one-child floor and
// generated 1.44/0.74 at ANY configured near-one mean.
// ---------------------------------------------------------------------------

fn mnq_july_scalars(fp: &Fingerprint) -> GeneratorScalars {
    let mut scalars = GeneratorScalars::xbtusd_anchor(fp);
    scalars.children_mean = 1.171_112_721_155_989_7;
    scalars.children_single_frac = 0.904_898_398_286_822_2;
    scalars.levels_mean = 1.121_551_351_424_383_1;
    scalars
}

fn realized_children(source: &mut GeneratedSource, parents: u64) -> (f64, f64) {
    let mut rows = 0u64;
    let mut singles = 0u64;
    for _ in 0..parents {
        let summary = source.advance_parent().expect("parent draw");
        rows += u64::from(summary.child_count);
        singles += u64::from(summary.child_count == 1);
    }
    (
        rows as f64 / parents as f64,
        singles as f64 / parents as f64,
    )
}

#[test]
fn the_floor_branch_reproduces_a_near_single_child_tape() {
    let fp = Fingerprint::from_repo_json();
    let scalars = mnq_july_scalars(&fp);
    let mut source = GeneratedSource::new(scalars, 42, 0, &fp, None);
    let (mean, single) = realized_children(&mut source, 200_000);
    assert!(
        (mean - 1.171_112_7).abs() < 0.02,
        "floor-branch children_mean should track the configured target, \
         got {mean}"
    );
    assert!(
        (single - 0.904_898_4).abs() < 0.01,
        "floor-branch single fraction should track the configured target, \
         got {single}"
    );
}

#[test]
fn the_base_mean_boundary_reproduces_the_target_on_both_sides() {
    // children_mean 5.0 sits exactly on the floor boundary (quiet mult
    // 0.20); 5.05 sits just above on the legacy branch. Both must
    // reproduce their configured unconditional mean - the branch decides
    // the arithmetic, never the target.
    let fp = Fingerprint::from_repo_json();
    for (mean_cfg, single_cfg, single_preserved) in [(5.0, 0.55, true), (5.05, 0.55, false)] {
        let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
        scalars.children_mean = mean_cfg;
        scalars.children_single_frac = single_cfg;
        scalars.levels_mean = 2.0;
        let mut source = GeneratedSource::new(scalars, 7, 0, &fp, None);
        let (mean, single) = realized_children(&mut source, 120_000);
        assert!(
            (mean - mean_cfg).abs() / mean_cfg < 0.05,
            "children_mean {mean_cfg} at the branch boundary generated \
             {mean}"
        );
        if single_preserved {
            // The floor side preserves the configured single fraction by
            // construction. The legacy side just above the boundary keeps
            // the old identity byte-for-byte, whose single fraction misses
            // near the floor - that miss is itself the preserved behavior, so it
            // is deliberately not asserted there.
            assert!(
                (single - single_cfg).abs() < 0.02,
                "floor-side single fraction should track {single_cfg}, \
                 got {single}"
            );
        }
    }
}

#[test]
fn floor_branch_feasibility_is_monotone_under_surge() {
    // The construction-time feasibility check runs at children_mult = 1;
    // this proves the solve stays feasible across the whole FlowSurge
    // range [1, 100], so no runtime refusal is needed. Independent
    // reimplementation of the 3.1 arithmetic, deliberately: two
    // implementations of one rule police each other.
    let (children_mean, single_frac): (f64, f64) =
        (1.171_112_721_155_989_7, 0.904_898_398_286_822_2);
    let quiet_share = 0.35_f64;
    let quiet_mult = 0.20_f64;
    let mut mult = 1.0_f64;
    while mult <= 100.0 {
        let effective = children_mean * mult;
        let quiet_eff = effective * quiet_mult;
        let quiet_mean = quiet_eff.max(1.0);
        let quiet_single = if quiet_eff <= 1.0 {
            1.0
        } else {
            single_frac.max(1.0 / quiet_eff)
        };
        let active_mean = (effective - quiet_share * quiet_mean) / (1.0 - quiet_share);
        let active_single = (single_frac - quiet_share * quiet_single) / (1.0 - quiet_share);
        assert!(
            active_mean > 1.0 && active_single >= 1.0 / active_mean && active_single <= 1.0,
            "infeasible at children_mult {mult}: active mean {active_mean}, \
             active single {active_single}"
        );
        mult += 0.5;
    }
}

#[test]
fn a_surge_crossing_the_floor_boundary_follows_the_branch() {
    // children_mult 6 pushes the quiet effective mean across one
    // (1.1711 * 6 * 0.2 = 1.405) - the floor-aware branch's internal
    // transition. The surge scales the unconditional target, and the
    // solve must preserve it: 1.1711 * 6 = 7.027, where the broken legacy
    // identity would not.
    let fp = Fingerprint::from_repo_json();
    let scalars = mnq_july_scalars(&fp);
    let mut source =
        GeneratedSource::new(scalars, 11, 0, &fp, None).with_surge(0, 86_400_000, 1.0, 6.0);
    let (mean, single) = realized_children(&mut source, 120_000);
    let target = 1.171_112_721_155_989_7 * 6.0;
    assert!(
        (mean - target).abs() / target < 0.10,
        "surged floor-branch children_mean should track {target}, got {mean}"
    );
    // The surge scales the mean target; the single-fraction target is
    // unconditional and the solve preserves it across the internal
    // quiet_eff transition.
    assert!(
        (single - 0.904_898_4).abs() < 0.01,
        "surged floor-branch single fraction should stay at the configured \
         0.9049, got {single}"
    );
}

// ---------------------------------------------------------------------------
// Brick S: the target-local size sigma (successor spec 3.2).
// ---------------------------------------------------------------------------

#[test]
fn size_log_sigma_reaches_the_draw() {
    let fp = Fingerprint::from_repo_json();
    let narrow = GeneratorScalars::xbtusd_anchor(&fp);
    let mut wide = narrow.clone();
    wide.size_log_sigma = 2.0;
    let mut narrow_source = GeneratedSource::new(narrow, 42, 0, &fp, None);
    let mut wide_source = GeneratedSource::new(wide, 42, 0, &fp, None);
    let differs =
        (0..256).any(|_| next_trade(&mut narrow_source).size != next_trade(&mut wide_source).size);
    assert!(differs, "a changed sigma never reached the size draw");
}

#[test]
fn the_default_sigma_is_byte_identical() {
    // An explicit 1.15 and the serde default are the same configuration:
    // the shared shape value became the default, not a refit.
    let fp = Fingerprint::from_repo_json();
    let default_scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut explicit = default_scalars.clone();
    explicit.size_log_sigma = 1.15;
    let mut a = GeneratedSource::new(default_scalars, 42, 0, &fp, None);
    let mut b = GeneratedSource::new(explicit, 42, 0, &fp, None);
    for _ in 0..512 {
        assert_eq!(
            format!("{:?}", a.next_tick()),
            format!("{:?}", b.next_tick())
        );
    }
}

#[test]
fn an_absent_arrival_seam_keeps_the_legacy_tape_byte_identical() {
    // Brick K / B1 regression pin.  The protocol-12b fields are deliberately
    // optional: omitting them must leave the shipped main-RNG path untouched.
    let fp = Fingerprint::from_repo_json();
    let legacy_scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut explicit_absence = legacy_scalars.clone();
    explicit_absence.arrival = None;
    let mut legacy = GeneratedSource::new(legacy_scalars, 7, 0, &fp, None);
    let mut seamless = GeneratedSource::new(explicit_absence, 7, 0, &fp, None);
    for _ in 0..10_000 {
        assert_eq!(
            format!("{:?}", legacy.next_tick()),
            format!("{:?}", seamless.next_tick()),
            "an omitted arrival seam moved a legacy tape byte"
        );
    }
}

#[test]
fn a_sigma_outside_its_band_refuses() {
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.size_log_sigma = 5.0;
    assert_eq!(
        scalars
            .validate()
            .expect_err("unit mix-up must refuse")
            .field,
        "size_log_sigma"
    );
}

// ---------------------------------------------------------------------------
// Brick T: the observation-only volatility trace (successor spec 3.3).
// ---------------------------------------------------------------------------

#[test]
fn trace_consumes_no_draws_and_leaves_the_tape_byte_identical() {
    let fp = Fingerprint::from_repo_json();
    let scalars = GeneratorScalars::xbtusd_anchor(&fp);
    let mut traced = GeneratedSource::new(scalars.clone(), 42, 0, &fp, None);
    let mut plain = GeneratedSource::new(scalars, 42, 0, &fp, None);
    traced.enable_vol_trace();
    let mut records = 0;
    for _ in 0..512 {
        assert_eq!(
            format!("{:?}", traced.next_tick()),
            format!("{:?}", plain.next_tick()),
            "the trace must observe, never perturb"
        );
        if let Some(trace) = traced.take_vol_trace() {
            records += 1;
            assert!(trace.sigma2_realized > 0.0);
            assert!(trace.mid_before > 0.0 && trace.mid_after > 0.0);
            assert!(
                (trace.innovation_raw / trace.innovation_std - super::consts::STUDENT_T_UNIT_SCALE)
                    .abs()
                    < 1e-12,
                "raw and standardized innovations must share one draw"
            );
        }
    }
    assert!(
        records > 0,
        "an enabled trace that records nothing is no trace"
    );
    assert!(
        plain.take_vol_trace().is_none(),
        "a source never enabled must carry no trace"
    );
}

#[test]
fn an_infeasible_active_solve_refuses_by_name() {
    // A near-one mean with a LOW single fraction leaves the active state
    // owing a negative single share - unrepresentable, and exactly the
    // silent-miss class the floor branch exists to end.
    let fp = Fingerprint::from_repo_json();
    let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
    scalars.children_mean = 1.05;
    scalars.children_single_frac = 0.10;
    scalars.levels_mean = 1.02;
    let err = scalars
        .validate()
        .expect_err("infeasible solve must refuse");
    // `field` stays a bare config-field name a consumer can match on, and the
    // discriminating prose lives in `detail`. Both halves are asserted: the
    // refusal is useless to an operator without the detail, and unmatchable if
    // the detail ever leaks back into the field.
    assert_eq!(
        err,
        ScalarError::detailed(
            "children_single_frac",
            "floor-branch active solve infeasible"
        )
    );
    assert_eq!(
        err.to_string(),
        "children_single_frac (floor-branch active solve infeasible)"
    );
}

/// Nothing else in the tree checks that a refusal's `field` is renderable as a
/// config path, and `children_single_frac (floor-branch ...)` shipped for as
/// long as it did because of that. Every refusal `validate`,
/// `validate_size_grid` and `try_new` can produce is walked here.
///
/// Each case asserts its own refusal, by name. An earlier draft guarded the
/// walk with a `refusals.len() >= 12` floor, which is the defect this arc keeps
/// finding: three mutations could quietly stop refusing, or start refusing on a
/// different field, and the floor still held. A named expectation per case
/// fails on both.
#[test]
fn every_scalar_refusal_names_a_bare_config_field() {
    fn bare(err: &ScalarError) -> bool {
        !err.field.is_empty()
            && err
                .field
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    }

    let fp = Fingerprint::from_repo_json();
    let good = GeneratorScalars::xbtusd_anchor(&fp);
    let empty_corpus = super::quote::CalibrationProvenance::Fitted {
        corpus: "   ".to_string(),
    };

    // (mutation, the field the refusal must name). Ordered as `validate` tests
    // them, so a reordered check that shadows a later branch shows up as the
    // wrong name rather than as a still-passing count.
    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, Box<dyn Fn(&mut GeneratorScalars)>)> = vec![
        (
            "symbol",
            Box::new(|s: &mut GeneratorScalars| s.symbol = String::new()),
        ),
        (
            "arrival",
            Box::new(|s: &mut GeneratorScalars| {
                s.arrival = Some(ArrivalConfig::LogOuCox {
                    sigma_y: -1.0,
                    tau_s: 60.0,
                });
            }),
        ),
        // The three provenance slots: a `Fitted` calibration whose corpus is
        // blank is a fit that names no data, and each slot must refuse under
        // its OWN name rather than under whichever the loop happens to reach.
        ("quoted_width", {
            let p = empty_corpus.clone();
            Box::new(move |s: &mut GeneratorScalars| {
                s.quoted_width = QuotedWidth::new(std::num::NonZeroU32::new(2).unwrap(), p.clone());
            })
        }),
        ("top_sizes", {
            let p = empty_corpus.clone();
            Box::new(move |s: &mut GeneratorScalars| s.top_sizes.provenance = p.clone())
        }),
        ("trade_displacement_ticks", {
            let p = empty_corpus.clone();
            Box::new(move |s: &mut GeneratorScalars| {
                s.trade_displacement_ticks = TradeDisplacement::new(0.5, p.clone());
            })
        }),
        (
            "modal_tick",
            Box::new(|s: &mut GeneratorScalars| s.modal_tick = Decimal::ZERO),
        ),
        (
            "modal_tick",
            Box::new(|s: &mut GeneratorScalars| {
                s.modal_tick = Decimal::new(1, 7);
                s.price_decimals = 1;
            }),
        ),
        (
            "mean_event_duration_s",
            Box::new(|s: &mut GeneratorScalars| s.mean_event_duration_s = f64::NAN),
        ),
        (
            "children_mean",
            Box::new(|s: &mut GeneratorScalars| s.children_mean = 0.0),
        ),
        (
            "children_single_frac",
            Box::new(|s: &mut GeneratorScalars| s.children_single_frac = 1.5),
        ),
        // The one detailed refusal: the floor-branch solve. It must still name
        // a bare field, with the prose in `detail`.
        (
            "children_single_frac",
            Box::new(|s: &mut GeneratorScalars| {
                s.children_mean = 1.05;
                s.children_single_frac = 0.10;
                s.levels_mean = 1.02;
            }),
        ),
        (
            "levels_mean",
            Box::new(|s: &mut GeneratorScalars| s.levels_mean = 0.5),
        ),
        (
            "size_round_frac",
            Box::new(|s: &mut GeneratorScalars| s.size_round_frac = 2.0),
        ),
        (
            "size_log_sigma",
            Box::new(|s: &mut GeneratorScalars| s.size_log_sigma = 9.0),
        ),
        (
            "start_price",
            Box::new(|s: &mut GeneratorScalars| s.start_price = Decimal::from(5_000_000_000_i64)),
        ),
        (
            "latent_size_median",
            Box::new(|s: &mut GeneratorScalars| s.latent_size_median = Decimal::ZERO),
        ),
        (
            "vol_scalar",
            Box::new(|s: &mut GeneratorScalars| s.vol_scalar = GARCH_SIGMA_CAP),
        ),
        (
            "trade_displacement_ticks",
            Box::new(|s: &mut GeneratorScalars| {
                s.trade_displacement_ticks = TradeDisplacement::uncalibrated(-1.0);
            }),
        ),
    ];

    for (index, (expected, mutate)) in cases.iter().enumerate() {
        let mut scalars = good.clone();
        mutate(&mut scalars);
        let err = scalars
            .validate()
            .expect_err(&format!("case {index} ({expected}) stopped refusing"));
        assert_eq!(
            err.field, *expected,
            "case {index} refused under the wrong field: {err:?}"
        );
        assert!(
            bare(&err),
            "case {index} field is not a bare identifier: {err:?}"
        );
    }

    // `validate_size_grid` is a separate entry point with a refusal of its own:
    // a latent center far under the smallest tradable quantity.
    let err = good
        .validate_size_grid(Decimal::ONE)
        .expect_err("a sub-grid latent median must refuse");
    assert_eq!(err.field, "latent_size_median");
    assert!(bare(&err), "{err:?}");

    // And `try_new` produces one refusal neither validator can: the top-of-book
    // sizes against the instrument's own grid. It is operator-visible, so it is
    // held to the same bare-field rule.
    let mut off_grid = good.clone();
    off_grid.top_sizes.bid = Decimal::ZERO;
    let Some(GeneratedSourceError::Scalar(err)) =
        GeneratedSource::try_new(off_grid, 42, 0, &fp, None).err()
    else {
        panic!("a sub-min top-of-book size must refuse as a scalar error");
    };
    assert_eq!(err.field, "top_sizes");
    assert!(bare(&err), "{err:?}");
}
