// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Tuning constants for the fingerprint-fitted synthetic generator. Grouped
//! here, separate from the types that consume them, so the calibration
//! knobs and their fingerprint-band rationale stay easy to scan as a unit.
//! Every constant here is `pub(super)`: shared across the `generated` module
//! tree (config validation, the core walk, the stochastic building blocks),
//! never part of the crate's public API.

// Raw-trade cadence refit. The permitted ACD refit was run first over 500
// explicit (persistence, feedback, weibull_shape, relax_mean_cal) tuples and
// its best candidate still produced median 13 fills/sec against a measured 4,
// and 17% empty seconds against a measured 13.4% - a process-SHAPE miss, not a
// calibration one. Section 4.4b of `notes/spec-trade-cadence.md` permits
// replacing the family on exactly that evidence, so the ACD clock is gone and
// arrivals are drawn from a two-state (quiet/active) Markov-modulated Weibull
// clock instead. Every constant below is fitted, not derived, and the whole
// block is module-level process SHAPE - deliberately NOT per-instrument, per
// section 10's stopping rule.
//
// The chain is parameterized by its STATIONARY quiet share and a persistence,
// so the two transition probabilities in `ArrivalClock` fall out as
// `(1 - persistence) * (1 - quiet_fraction)` and `(1 - persistence) *
// quiet_fraction`, whose stationary quiet occupancy is exactly
// ARRIVAL_QUIET_FRACTION. The active and quiet state means are solved from the
// declared `mean_event_duration_s` so the UNCONDITIONAL mean gap is preserved
// whatever the ratio is set to.
/// Stationary share of parent events drawn from the quiet state.
pub(super) const ARRIVAL_QUIET_FRACTION: f64 = 0.35;
/// Probability the clock keeps its current state at the next parent event, in
/// the sense that the switch rate is `1 - this`. Higher values make the dead
/// stretches and the bursts both longer; this is the knob that buys the
/// measured empty-second mass without inflating the median.
pub(super) const ARRIVAL_STATE_PERSISTENCE: f64 = 0.90;
/// Quiet mean gap divided by active mean gap. The measured tape alternates
/// between dead seconds and violent bursts rather than being uniformly slow,
/// which is what this ratio buys and what no single-state clock could.
pub(super) const ARRIVAL_QUIET_ACTIVE_RATIO: f64 = 150.0;
/// Shape of the per-event Weibull innovation. 1.0 is exponential: the burst
/// clustering lives in the state chain, not in the marginal, so a sub-unit
/// shape would double-count it.
pub(super) const ARRIVAL_WEIBULL_SHAPE: f64 = 1.0;
/// Mean-gap calibration, the successor to `ACD_RELAX_MEAN_CAL`. Re-derived by
/// the same 10-step bisection the spec documents, driving the seed-42 realized
/// mean PARENT gap of the two-million-event realism draw onto
/// `scalars.mean_event_duration_s`; `MEAN_GAP_REL_TOL` in the test module is
/// what keeps it honest.
pub(super) const ARRIVAL_MEAN_CAL: f64 = 0.944;
/// Mean of `Weibull(scale = 1, shape = ARRIVAL_WEIBULL_SHAPE)`, which the draw
/// is divided by so the innovation carries unit mean. `gamma(1 + 1/1.0) = 1`
/// exactly at the committed shape; it stays a named constant so a future shape
/// change finds the coupling instead of silently losing the normalization.
pub(super) const ARRIVAL_WEIBULL_MEAN: f64 = 1.0;
// Sweep intensity conditional on the arrival state. The two multipliers scale
// `children_mean` per state and are chosen to preserve the DECLARED
// unconditional mean exactly:
//
//     quiet_frac * QUIET + (1 - quiet_frac) * ACTIVE == 1
//
// so `ARRIVAL_ACTIVE_CHILDREN_MULT` is fully determined by the other two, and
// `arrival_children_mults_preserve_the_declared_mean` in the test module pins
// the literal against that identity. Conditioning sweep size on the state (thin
// sweeps while quiet, fat sweeps while active) is what supplies the measured
// mean/median spread: the child mixture alone is exponential-tailed and cannot.
pub(super) const ARRIVAL_QUIET_CHILDREN_MULT: f64 = 0.20;
pub(super) const ARRIVAL_ACTIVE_CHILDREN_MULT: f64 = 1.430_769_230_769_230_8;
/// Probability that a SINGLE-child event in the low bounce regime re-prints the
/// previous event's last price instead of the freshly priced one. Fitted to the
/// `zero_change_frac` band: at the raw-fill layer most prints are children that
/// repeat their parent's level by construction, but the EVENT series the gate
/// measures (section 6.3) would otherwise almost never repeat, because every
/// parent re-prices off a fresh latent mid. A venue whose top of book does not
/// move between two small takes prints the same price twice, which is what this
/// reproduces. It costs one Bernoulli draw per event and never applies in the
/// high regime, where the price is meant to be moving.
pub(super) const EVENT_PRICE_REPEAT_PROB: f64 = 0.8;
// Level-walk intensity conditional on the bounce regime. `level_step_prob` is
// solved from the measured `levels_mean` (section 2.4) as an unconditional
// probability; these two multipliers redistribute it so a sweep in the high
// regime walks the grid hard and a sweep in the quiet regime barely walks at
// all, which is where the abs-return ACF at the event layer comes from.
// NOTE the deliberate imprecision: at the bounce chain's stationary high share
// (BOUNCE_LOW_TO_HIGH / (BOUNCE_LOW_TO_HIGH + BOUNCE_HIGH_TO_LOW) = 0.3125) the
// blended multiplier is 0.956, so realized `levels_mean` sits ~4% BELOW the
// declared scalar. `sweep_structure` asserts that agreement at a 15% tolerance
// with this shortfall in mind; tightening it means re-solving these two against
// the stationary share, which re-blesses the golden.
pub(super) const HIGH_REGIME_LEVEL_STEP_MULT: f64 = 2.4;
pub(super) const LOW_REGIME_LEVEL_STEP_MULT: f64 = 0.3;
/// Fraction of the gap between an event's LAST child price and the latent mid
/// that is carried into the next event as `BounceState::drift_ticks`. This
/// re-centres the drift at every event boundary, which is what keeps the
/// event-layer return ACF negative once a sweep can walk several ticks away
/// from the mid inside one event: without it a burst's walk would be a
/// permanent, uncorrected excursion.
pub(super) const DRIFT_RECENTER_FRAC: f64 = 0.16;
// GARCH persistence gives clustered latent volatility without letting the
// continuous mid-price drown out tick-grid flat runs.
//
// The innovation is STANDARDIZED to unit variance before it reaches the
// recursion (see `STUDENT_T_UNIT_SCALE`). That is load-bearing, not cosmetic.
// `GarchVol::new` derives `a0 = vol_scalar^2 * (1 - a1 - b1)` so that
// `vol_scalar^2` is the unconditional variance, and that derivation assumes
// `E[z^2] = 1`. A raw Student-t(4) has variance `df / (df - 2)` = 2, which made
// the true second-moment condition `a1 * E[z^2] + b1` = 1.115 - above one, so
// the recursion had NO finite stationary variance and stayed bounded only by
// its own safety rails. Measured, it ran 8.17x hotter than configured and sat
// pinned at the variance cap 12.96 percent of the time.
//
// `a1` and `b1` were re-solved against the corrected condition over a grid of
// (a1, persistence, vol_scalar) with the rails held out of reach and an
// occupancy penalty, scored on the anchor's absolute-return ACF DECAY RATIOS
// rather than on level-band membership alone. See
// `garch_second_moment_instrumentation`. Persistence is `a1 + b1` = 0.999.
pub(super) const GARCH_ARCH: f64 = 0.02;
pub(super) const GARCH_GARCH: f64 = 0.979;
pub(super) const STUDENT_T_DF: f64 = 4.0;
/// `sqrt(df / (df - 2))`, the standard deviation of a raw Student-t(df). The
/// innovation is divided by this so the GARCH recursion receives a unit-variance
/// shock, which is what `GarchVol::new`'s `a0` derivation has always assumed.
/// At the committed `STUDENT_T_DF` of 4 this is exactly `sqrt(2)`, so the
/// standard constant is used - but the COUPLING is to the degrees of freedom,
/// not to the number two. `student_t_unit_scale_matches_df` pins that, so a
/// change to `STUDENT_T_DF` fails a test here instead of silently
/// de-standardizing the innovation.
pub(super) const STUDENT_T_UNIT_SCALE: f64 = std::f64::consts::SQRT_2;
// The high/low bounce regime jointly controls negative lag-1 return ACF
// (target -0.197 .. -0.057), zero_change_frac (0.336 .. 0.751) and the
// absolute-return ACF decay (lag1 0.152 .. 0.307, lag10 .. 0.156, lag50 ..
// 0.123). The regime's MEAN ACTIVE LENGTH (1 / BOUNCE_HIGH_TO_LOW_PROB ~ 33
// trades) sets the abs-return ACF timescale: too long flattens the lag1->lag10
// decay above the lag10 ceiling, too short starves the lag50 floor, so these
// two transition probabilities are pinned together with the flip contrast.
pub(super) const BOUNCE_LOW_FLIP_PROB: f64 = 0.02;
pub(super) const BOUNCE_HIGH_FLIP_PROB: f64 = 0.25;
pub(super) const BOUNCE_LOW_TO_HIGH_PROB: f64 = 0.01;
pub(super) const BOUNCE_HIGH_TO_LOW_PROB: f64 = 0.022;
/// How far the TRADE price is displaced from the drifted latent mid, on the
/// aggressor's side, before the grid rounding in `next_price`.
///
/// This is NOT a spread, and the name it carried until 2026-08-04
/// (`HALF_SPREAD_TICKS`) asserted a mechanism the generator does not have. The
/// generator constructs no `QuoteTick` anywhere: there is no bid, no ask and no
/// top of book, and `mogwai-server`'s `/quotes` route returns empty by
/// construction. What this constant produces is the bounce amplitude of the
/// PRINT series - two consecutive opposite-sided prints at an unchanged mid land
/// `2.0 * TRADE_BOUNCE_HALF_WIDTH_TICKS` apart.
///
/// The distinction is load-bearing rather than pedantic. Against real data the
/// quoted width `ask - bid` and the effective spread
/// `2 * aggressor_sign * (trade_price - quote_mid)` are separate observables: a
/// trade may execute inside, at, or outside the displayed spread. Reading a
/// two-tick print separation as a two-tick quoted width silently assumes they
/// agree, and a venue that exists to inject execution divergence is precisely
/// the place that assumption must not be baked in. Whatever eventually fits a
/// quoted width belongs to a quote layer that does not exist yet; this value
/// belongs to the trade layer and stays there.
pub(super) const TRADE_BOUNCE_HALF_WIDTH_TICKS: f64 = 0.5;
// High-regime drift adds same-direction on-grid movement so volatility clusters
// are not only alternating bid-ask moves.
pub(super) const HIGH_REGIME_DRIFT_PROB: f64 = 0.65;
pub(super) const HOT_DRIFT_PROB: f64 = 0.90;
pub(super) const DRIFT_DIR_FLIP_PROB: f64 = 0.015;
// Size calibration controls heavy-tailed sizes and the round-lot mass gate.
pub(super) const SIZE_LOG_SIGMA: f64 = 1.15;
pub(super) const SIZE_DECIMALS: u32 = 8;
pub(super) const INTRA_EVENT_STEP_NS: u64 = 1_000;
pub(super) const CHILD_CAP: u32 = 4_096;
// THREE SEPARATE CONTRACTS, previously overloaded onto two constants that a
// regime multiplier scaled together. Keeping them apart is what makes a
// divergence an OUTPUT envelope instead of something that raises the process's
// own state ceiling and alters every later recursion step - the principle
// `SessionEdgeSpike` already follows by staying out of feedback state.
//
//   1. GARCH_SIGMA_CAP        bounds the recursion STATE. Never regime-scaled.
//   2. FEEDBACK_RETURN_CEILING bounds what re-enters the recursion. Never
//                              regime-scaled.
//   3. REALIZED_RETURN_CEILING bounds the return that actually moves the mid,
//                              AFTER session and regime scaling. Absolute:
//                              it never scales with a regime either, which is
//                              the whole point of naming it separately.
//
/// Bounds the GARCH state. Sized from a measured tail rather than chosen: over
/// eight seeds by two million clean updates the recursion's sigma reached 57.2x
/// its unconditional scale, so this sits above that at roughly 83x `VOL_SCALAR`
/// and does not participate in calibration. Not "never hit" - the standardized
/// t(4) still has infinite kurtosis, so the running maximum grows with the
/// horizon. The honest statement is NO OBSERVED PARTICIPATION over the 16M
/// calibration sample. See `standardized_candidate_rail_sizing`.
pub(super) const GARCH_SIGMA_CAP: f64 = 1e-3;
/// Bounds the clean feedback return. Same sizing run: the largest clean
/// |return| observed over 16M updates was 3.33e-3, so this sits just above it
/// and shapes nothing in ordinary operation.
pub(super) const FEEDBACK_RETURN_CEILING: f64 = 4e-3;
/// Absolute ceiling on the realized log return, applied after the session and
/// regime envelopes and NEVER scaled by either.
///
/// This is a PRODUCT POLICY, not a fitted market fact, and its provenance is
/// the maximum-strength envelope experiment
/// (`realized_return_envelope_under_regime_scaling`) rather than any MNQ
/// calibration. As a log return it permits approximately +5.13 percent up and
/// -4.88 percent down in a single event.
///
/// Chosen from the measured policy space: inert during clean operation, where
/// the unbounded realized maximum at `vol_mult` 1 is 0.82 percent; 0.024
/// percent occupancy at `vol_mult` 100 and the session vol peak, rare enough
/// not to become the storm's distribution; and far below the 126 percent move
/// an unrailed maximum storm would otherwise reach.
///
/// NOTE this bounds a SINGLE EVENT. Cumulative movement over many events is not
/// bounded by it, and a sustained storm can walk the mid arbitrarily far.
pub(super) const REALIZED_RETURN_CEILING: f64 = 5e-2;
/// A sigma rail below this multiple of `vol_scalar` would participate in
/// ordinary calibration rather than bounding pathology, because the clean
/// process was measured reaching 57.2x. Diagnosed, not enforced: a hard
/// universal ratio gate would repeat the `scalar_ranges` mistake in another
/// dimension, denying a legitimately higher-scale instrument.
pub(super) const SIGMA_RAIL_HEADROOM_RATIO: f64 = 60.0;
/// A latent size center below this fraction of the minimum tradable quantity
/// is a unit contradiction, not merely an illiquid instrument. Behavioral
/// tail mass is diagnosed separately so coherent thin contracts remain legal.
pub(super) const LATENT_SIZE_MIN_RATIO: f64 = 0.01;
// Hard ceiling `next_latent_mid` clamps the latent mid to (the modal tick is
// the matching floor). Hoisted so the two clamp sites and `start_price`
// validation cannot drift: a `start_price` above this is silently collapsed on
// the very first tick (a start_price of 5e9 becomes an ~80 percent crash).
pub(super) const MID_CEILING: f64 = 1_000_000_000.0;
// Session-share sum invariants the modulator relies on. `intensity_hour` and
// `dow_weight` are per-period FRACTIONS the modulator re-centers on 1.0 by
// multiplying by the period count (24, 7), so a well-formed curve sums to
// ~1.0. `vol_hour` is a per-mean RMS ratio used raw, so its mean is 1.0 and it
// sums to ~24. The committed fingerprint hits these exactly (1.0 / 1.0 / 24.0).
pub(super) const SESSION_SHARE_SUM: f64 = 1.0;
pub(super) const VOL_HOUR_SUM: f64 = 24.0;
pub(super) const SESSION_SUM_TOL: f64 = 0.02;
// Shared price/size axes both generator constructors pin identically. Hoisted to
// module consts so from_fingerprint_medians and xbtusd_anchor cannot drift apart:
// the start price the GARCH mid walks from, the lognormal size median, and the
// per-tick volatility scalar feeding GarchVol's unconditional variance. The
// per-pair fields that legitimately differ (modal_tick, price_decimals) stay in
// the constructors.
pub(super) const START_PRICE_USD: i64 = 60_000;
// 0.1 expressed as Decimal::new(mantissa, scale).
// Re-solved with `GARCH_ARCH` and `GARCH_GARCH` against the corrected
// second-moment condition. The previous 1e-6 was never the realized scale: the
// unstandardized innovation drove the process 8.17x hotter, so the tape's
// amplitude came from saturation rather than from this number. At 1.2e-5 the
// measured unclipped return RMS is 1.2393e-5, so the scalar now means what it
// says to within 3 percent.
pub(super) const VOL_SCALAR: f64 = 1.2e-5;
// Low-intensity gap rails. This gate is a NUMERICAL safeguard, not a closure
// mechanism. `SessionCalendar` owns hard closure - trading hours, maintenance
// breaks and shut weekends - and `SessionProfile` describes relative activity
// while the market is open. A profile factor may still be very small, because a
// genuinely thin open hour is a real thing to model, and that is the case this
// exists for.
//
// The arrival multiplier is sampled once, at the instant a gap opens, and
// dividing a whole duration draw by a near-zero factor stretches it absurdly
// (1e-6 turns a ~7 s draw into ~80 days) and can push the ns cast to saturate
// at u64::MAX, pinning the clock there forever. Gaps opening BELOW this gate
// therefore take the hour-integrating path (`low_intensity_gap_ns`); gaps at or
// above it keep the original once-sampled math bit for bit, which is what keeps
// the committed fingerprint's stream byte-identical - its smallest hour*day
// multiplier is ~0.58, two orders of magnitude above the gate, so the
// fingerprint profile can never cross it.
//
// This constant was `SESSION_CLOSED_ARR_MULT` and the path it gates was
// `closed_window_gap_ns`. Both names asserted that a small factor MEANS closed,
// which is the conflation the calendar-conditional contract removed: encoding a
// known closure as a near-zero share is no longer the intended idiom, and only
// the calendar may say a market is shut.
pub(super) const LOW_INTENSITY_ARR_MULT: f64 = 0.01;
// Hard per-gap ceiling (366 days in ns) for BOTH paths. On the open path it is
// unreachable for any multiplier above the gate paired with the validated
// thin_factor <= 1000 and realistic duration draws - it exists so no f64->u64
// cast can ever land on u64::MAX again. On the closed path it bounds the
// hour walk when a profile closes EVERY hour so hard the budget can never be
// spent: the gap caps at ~a year and the clock keeps strictly advancing
// instead of freezing.
pub(super) const MAX_SESSION_GAP_NS: u64 = 31_622_400_000_000_000;
pub(super) const NS_PER_HOUR: u64 = 3_600_000_000_000;
