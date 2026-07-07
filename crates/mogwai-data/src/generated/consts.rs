//! Tuning constants for the fingerprint-fitted synthetic generator. Grouped
//! here, separate from the types that consume them, so the calibration
//! knobs and their fingerprint-band rationale stay easy to scan as a unit.
//! Every constant here is `pub(super)`: shared across the `generated` module
//! tree (config validation, the core walk, the stochastic building blocks),
//! never part of the crate's public API.

// ACD persistence (phi = alpha + beta) and the Weibull innovation shape jointly
// set duration_dispersion_index into the fingerprint band [131.7 .. 4608.9].
// Both push the dispersion up: phi toward 1 lengthens the clustered runs and a
// shape below 1 fattens the per-event innovation tail. The pair is deliberately
// NOT pushed to the anchor extreme (4608.9): var(d)/mean(d) is dominated by the
// single largest gap when the innovation is that heavy-tailed, so chasing the
// anchor makes the statistic explode well past the band on unlucky seeds. This
// pair lands the seeded (seed 42) realism draw near the band's lower interior
// (~190) with margin to the 131.7 floor, which is the cross-pair-band gate the
// spec commits to - not anchor point-tracking.
pub(super) const ACD_PERSISTENCE: f64 = 0.9935;
pub(super) const ACD_FEEDBACK_SHARE: f64 = 0.08;
pub(super) const ACD_WEIBULL_SHAPE: f64 = 0.60;
// GARCH persistence gives clustered latent volatility without letting the
// continuous mid-price drown out tick-grid flat runs.
pub(super) const GARCH_ARCH: f64 = 0.06;
pub(super) const GARCH_GARCH: f64 = 0.935;
pub(super) const STUDENT_T_DF: f64 = 4.0;
// The high/low bounce regime jointly controls negative lag-1 return ACF
// (target -0.197 .. -0.057), zero_change_frac (0.336 .. 0.751) and the
// absolute-return ACF decay (lag1 0.152 .. 0.307, lag10 .. 0.156, lag50 ..
// 0.123). The regime's MEAN ACTIVE LENGTH (1 / BOUNCE_HIGH_TO_LOW_PROB ~ 33
// trades) sets the abs-return ACF timescale: too long flattens the lag1->lag10
// decay above the lag10 ceiling, too short starves the lag50 floor, so these
// two transition probabilities are pinned together with the flip contrast.
pub(super) const BOUNCE_LOW_FLIP_PROB: f64 = 0.02;
pub(super) const BOUNCE_HIGH_FLIP_PROB: f64 = 0.58;
pub(super) const BOUNCE_LOW_TO_HIGH_PROB: f64 = 0.004;
pub(super) const BOUNCE_HIGH_TO_LOW_PROB: f64 = 0.030;
pub(super) const HALF_SPREAD_TICKS: f64 = 0.5;
// High-regime drift adds same-direction on-grid movement so volatility clusters
// are not only alternating bid-ask moves.
pub(super) const HIGH_REGIME_DRIFT_PROB: f64 = 0.65;
pub(super) const HOT_DRIFT_PROB: f64 = 0.90;
pub(super) const DRIFT_DIR_FLIP_PROB: f64 = 0.015;
// Size calibration controls heavy-tailed sizes and the round-lot mass gate.
pub(super) const SIZE_LOG_SIGMA: f64 = 1.15;
pub(super) const SIZE_DECIMALS: u32 = 8;
pub(super) const MAX_ABS_RETURN: f64 = 0.000_02;
pub(super) const GARCH_SIGMA_CAP: f64 = 0.000_001;
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
pub(super) const TYPICAL_SIZE_MANTISSA: i64 = 1;
pub(super) const TYPICAL_SIZE_SCALE: u32 = 1;
pub(super) const VOL_SCALAR: f64 = 0.000_000_05;
// Session-gap rails. Trading hours, maintenance breaks and closed weekends are
// expressed as NEAR-ZERO hour/day shares in a custom SessionProfile - not a
// separate code path - but the arrival multiplier is sampled once, at the
// instant a gap opens, and dividing a whole duration draw by a near-zero share
// stretches it far past the closed window (share 1e-6 turns a ~7 s draw into
// ~80 days) and can push the ns cast to saturate at u64::MAX, pinning the
// clock there forever. Gaps that open BELOW this multiplier gate therefore
// take the hour-integrating path (`closed_window_gap_ns`); gaps at or above it
// keep the original once-sampled math bit for bit, which is what keeps the
// committed fingerprint's stream byte-identical - its smallest hour*day
// multiplier is ~0.58, two orders of magnitude above the gate, so the
// fingerprint profile can never cross it.
pub(super) const SESSION_CLOSED_ARR_MULT: f64 = 0.01;
// Hard per-gap ceiling (366 days in ns) for BOTH paths. On the open path it is
// unreachable for any multiplier above the gate paired with the validated
// thin_factor <= 1000 and realistic duration draws - it exists so no f64->u64
// cast can ever land on u64::MAX again. On the closed path it bounds the
// hour walk when a profile closes EVERY hour so hard the budget can never be
// spent: the gap caps at ~a year and the clock keeps strictly advancing
// instead of freezing.
pub(super) const MAX_SESSION_GAP_NS: u64 = 31_622_400_000_000_000;
pub(super) const NS_PER_HOUR: u64 = 3_600_000_000_000;
