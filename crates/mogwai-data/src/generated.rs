use mogwai_protocol::{AggressorSide, MarketRegime, TradeTick, decimal_to_f64};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{ChiSquared, Distribution, LogNormal, Normal, Weibull};
use rust_decimal::{Decimal, prelude::FromPrimitive};
use serde::Deserialize;

use crate::{TickEvent, TickSource};

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
const ACD_PERSISTENCE: f64 = 0.9935;
const ACD_FEEDBACK_SHARE: f64 = 0.08;
const ACD_WEIBULL_SHAPE: f64 = 0.60;
// GARCH persistence gives clustered latent volatility without letting the
// continuous mid-price drown out tick-grid flat runs.
const GARCH_ARCH: f64 = 0.06;
const GARCH_GARCH: f64 = 0.935;
const STUDENT_T_DF: f64 = 4.0;
// The high/low bounce regime jointly controls negative lag-1 return ACF
// (target -0.197 .. -0.057), zero_change_frac (0.336 .. 0.751) and the
// absolute-return ACF decay (lag1 0.152 .. 0.307, lag10 .. 0.156, lag50 ..
// 0.123). The regime's MEAN ACTIVE LENGTH (1 / BOUNCE_HIGH_TO_LOW_PROB ~ 33
// trades) sets the abs-return ACF timescale: too long flattens the lag1->lag10
// decay above the lag10 ceiling, too short starves the lag50 floor, so these
// two transition probabilities are pinned together with the flip contrast.
const BOUNCE_LOW_FLIP_PROB: f64 = 0.02;
const BOUNCE_HIGH_FLIP_PROB: f64 = 0.58;
const BOUNCE_LOW_TO_HIGH_PROB: f64 = 0.004;
const BOUNCE_HIGH_TO_LOW_PROB: f64 = 0.030;
const HALF_SPREAD_TICKS: f64 = 0.5;
// High-regime drift adds same-direction on-grid movement so volatility clusters
// are not only alternating bid-ask moves.
const HIGH_REGIME_DRIFT_PROB: f64 = 0.65;
const HOT_DRIFT_PROB: f64 = 0.90;
const DRIFT_DIR_FLIP_PROB: f64 = 0.015;
// Size calibration controls heavy-tailed sizes and the round-lot mass gate.
const SIZE_LOG_SIGMA: f64 = 1.15;
const SIZE_DECIMALS: u32 = 8;
const MAX_ABS_RETURN: f64 = 0.000_02;
const GARCH_SIGMA_CAP: f64 = 0.000_001;
// Hard ceiling `next_latent_mid` clamps the latent mid to (the modal tick is
// the matching floor). Hoisted so the two clamp sites and `start_price`
// validation cannot drift: a `start_price` above this is silently collapsed on
// the very first tick (a start_price of 5e9 becomes an ~80 percent crash).
const MID_CEILING: f64 = 1_000_000_000.0;
// Session-share sum invariants the modulator relies on. `intensity_hour` and
// `dow_weight` are per-period FRACTIONS the modulator re-centers on 1.0 by
// multiplying by the period count (24, 7), so a well-formed curve sums to
// ~1.0. `vol_hour` is a per-mean RMS ratio used raw, so its mean is 1.0 and it
// sums to ~24. The committed fingerprint hits these exactly (1.0 / 1.0 / 24.0).
const SESSION_SHARE_SUM: f64 = 1.0;
const VOL_HOUR_SUM: f64 = 24.0;
const SESSION_SUM_TOL: f64 = 0.02;
// Shared price/size axes both generator constructors pin identically. Hoisted to
// module consts so from_fingerprint_medians and xbtusd_anchor cannot drift apart:
// the start price the GARCH mid walks from, the lognormal size median, and the
// per-tick volatility scalar feeding GarchVol's unconditional variance. The
// per-pair fields that legitimately differ (modal_tick, price_decimals) stay in
// the constructors.
const START_PRICE_USD: i64 = 60_000;
// 0.1 expressed as Decimal::new(mantissa, scale).
const TYPICAL_SIZE_MANTISSA: i64 = 1;
const TYPICAL_SIZE_SCALE: u32 = 1;
const VOL_SCALAR: f64 = 0.000_000_05;
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
const SESSION_CLOSED_ARR_MULT: f64 = 0.01;
// Hard per-gap ceiling (366 days in ns) for BOTH paths. On the open path it is
// unreachable for any multiplier above the gate paired with the validated
// thin_factor <= 1000 and realistic duration draws - it exists so no f64->u64
// cast can ever land on u64::MAX again. On the closed path it bounds the
// hour walk when a profile closes EVERY hour so hard the budget can never be
// spent: the gap caps at ~a year and the clock keeps strictly advancing
// instead of freezing.
const MAX_SESSION_GAP_NS: u64 = 31_622_400_000_000_000;
const NS_PER_HOUR: u64 = 3_600_000_000_000;

#[derive(Debug, Clone, Deserialize)]
pub struct Fingerprint {
    pub golden_targets: GoldenTargets,
    pub session_profile: SessionProfile,
    pub scalar_ranges: ScalarRanges,
}

impl Fingerprint {
    #[must_use]
    pub fn from_repo_json() -> Self {
        let bytes = include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../analysis/fingerprint.json"
        ));
        let fingerprint: Self = serde_json::from_str(bytes).expect("committed fingerprint parses");
        fingerprint
            .session_profile
            .validate()
            .expect("committed fingerprint session profile is valid");
        fingerprint
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GoldenTargets {
    pub duration_dispersion_index: AnchorRange,
    pub return_acf_lag1: AnchorRange,
    pub abs_return_acf: AbsReturnAcf,
    pub zero_change_frac: AnchorRange,
    pub duration_acf_anchor: Vec<f64>,
    pub return_acf_anchor: Vec<f64>,
    pub abs_return_acf_anchor: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AbsReturnAcf {
    pub lag1: AnchorRange,
    pub lag10: AnchorRange,
    pub lag50: AnchorRange,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AnchorRange {
    pub anchor: f64,
    pub range: MinMedianMax,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MinMedianMax {
    pub min: f64,
    pub median: f64,
    pub max: f64,
}

impl MinMedianMax {
    /// Inclusive range-membership over the fitted band. The single source of
    /// truth for "is this scalar inside the fingerprint range" - `validate_f64`
    /// and the realism tests both route through it so the two cannot drift on
    /// the inclusive-vs-exclusive convention.
    #[must_use]
    pub fn contains(&self, v: f64) -> bool {
        (self.min..=self.max).contains(&v)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionProfile {
    pub intensity_hour: [f64; 24],
    pub vol_hour: [f64; 24],
    pub dow_weight: [f64; 7],
}

impl SessionProfile {
    pub fn validate(&self) -> Result<(), SessionProfileError> {
        for (index, share) in self.intensity_hour.iter().enumerate() {
            if !strictly_positive_finite(*share) {
                return Err(SessionProfileError {
                    field: "intensity_hour",
                    index,
                });
            }
        }
        for (index, mult) in self.vol_hour.iter().enumerate() {
            if !strictly_positive_finite(*mult) {
                return Err(SessionProfileError {
                    field: "vol_hour",
                    index,
                });
            }
        }
        for (index, share) in self.dow_weight.iter().enumerate() {
            if !strictly_positive_finite(*share) {
                return Err(SessionProfileError {
                    field: "dow_weight",
                    index,
                });
            }
        }
        // Normalization guard. Per-element positivity is not enough: the
        // modulator assumes `intensity_hour` / `dow_weight` are fractions
        // summing to ~1 (it multiplies by 24 / 7 to center each multiplier on
        // 1.0) and `vol_hour` is a per-mean ratio summing to ~24. A plausible
        // "no modulation" config of all-ones intensity passes every per-element
        // check yet yields a 24x (168x with dow) arrival multiplier, silently
        // compressing the validated `mean_duration_s` from seconds to
        // milliseconds; an un-normalized vol curve silently rescales overall
        // volatility even though `vol_scalar` validated. The on-grid and
        // positivity invariants still hold, so the golden stream would never
        // reveal either bug - only a sum check does.
        //
        // The bound is ONE-SIDED for the two distributions and two-sided for
        // vol: a sum ABOVE 1.0 is the compression pathology and is rejected,
        // but a sum BELOW 1.0 is the deliberate near-zero-share encoding of
        // closed sessions (a closed hour carries ~0 mass) and, at the
        // degenerate extreme, the every-hour-closed profile that
        // `closed_window_gap_ns`' cap machinery exists to survive - so a low
        // sum stays legal. `vol_hour` has no closed-hour use (a closed hour
        // prints no trades, so its vol value is irrelevant), so both an
        // inflated and a deflated curve are misconfigurations and get a
        // symmetric band. The sentinel `index` of `usize::MAX` marks a
        // whole-array (sum) violation rather than a single bad element.
        let intensity_sum: f64 = self.intensity_hour.iter().sum();
        if intensity_sum > SESSION_SHARE_SUM * (1.0 + SESSION_SUM_TOL) {
            return Err(SessionProfileError {
                field: "intensity_hour",
                index: usize::MAX,
            });
        }
        let dow_sum: f64 = self.dow_weight.iter().sum();
        if dow_sum > SESSION_SHARE_SUM * (1.0 + SESSION_SUM_TOL) {
            return Err(SessionProfileError {
                field: "dow_weight",
                index: usize::MAX,
            });
        }
        let vol_sum: f64 = self.vol_hour.iter().sum();
        if vol_sum < VOL_HOUR_SUM * (1.0 - SESSION_SUM_TOL)
            || vol_sum > VOL_HOUR_SUM * (1.0 + SESSION_SUM_TOL)
        {
            return Err(SessionProfileError {
                field: "vol_hour",
                index: usize::MAX,
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionProfileError {
    pub field: &'static str,
    /// Index of the offending element within `field`, or `usize::MAX` to denote
    /// a whole-array normalization (sum) violation with no single bad element.
    pub index: usize,
}

fn strictly_positive_finite(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

// Civil UTC fields the session profile is keyed on. Derived purely from the
// nanosecond wall clock with no chrono dependency: the session curves only need
// hour-of-day and day-of-week, both of which fall out of integer division on the
// unix-epoch second. Day-of-week uses the (days_since_epoch + 4) % 7 convention
// that puts Sun=0 (1970-01-01 was a Thursday), matching the fingerprint.
fn utc_hour_dow(clock_ns: u64) -> (usize, usize) {
    let secs = clock_ns / 1_000_000_000;
    let days = secs / 86_400;
    let hour = ((secs % 86_400) / 3_600) as usize;
    let dow = ((days + 4) % 7) as usize;
    (hour, dow)
}

// Hour-of-day only, for the sites that key on the hour and discard the
// day-of-week. Wraps utc_hour_dow so the civil-time derivation lives in exactly
// one place.
fn utc_hour(clock_ns: u64) -> usize {
    utc_hour_dow(clock_ns).0
}

// Precomputed session multipliers. Built once from the fingerprint's
// SessionProfile so the per-tick hot path is two array indexes and a multiply,
// not a re-normalization. The arrival multiplier centers each share on 1.0 by
// dividing out the uniform share (24 hours, 7 days); the vol multiplier is the
// fingerprint's per-mean ratio used as-is.
#[derive(Clone)]
struct SessionModulator {
    // intensity_hour[h] * 24.0: arrival-rate multiplier from the hour share,
    // centered on 1.0 (uniform hour share is 1/24).
    arr_hour: [f64; 24],
    // dow_weight[d] * 7.0: arrival-rate multiplier from the day share, centered
    // on 1.0 (uniform day share is 1/7). Sun=0 .. Sat=6.
    arr_dow: [f64; 7],
    // vol_hour[h]: per-mean per-trade RMS-return multiplier, used directly.
    vol_hour: [f64; 24],
}

impl SessionModulator {
    fn new(profile: &SessionProfile) -> Self {
        let mut arr_hour = [0.0; 24];
        for (h, mult) in arr_hour.iter_mut().enumerate() {
            *mult = profile.intensity_hour[h] * 24.0;
        }
        let mut arr_dow = [0.0; 7];
        for (d, mult) in arr_dow.iter_mut().enumerate() {
            *mult = profile.dow_weight[d] * 7.0;
        }
        Self {
            arr_hour,
            arr_dow,
            vol_hour: profile.vol_hour,
        }
    }

    // Arrival-rate multiplier at this wall-clock instant: hour-of-day times
    // day-of-week, both centered on 1.0. A duration is divided by this so a
    // high-activity instant produces shorter inter-arrivals.
    fn arrival_mult(&self, clock_ns: u64) -> f64 {
        let (hour, dow) = utc_hour_dow(clock_ns);
        self.arr_hour[hour] * self.arr_dow[dow]
    }

    // Volatility multiplier at this wall-clock instant: the fingerprint's
    // per-mean hour ratio. A formed return is multiplied by this, scaling the
    // innovation standard deviation rather than the variance.
    fn vol_mult(&self, clock_ns: u64) -> f64 {
        self.vol_hour[utc_hour(clock_ns)]
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScalarRanges {
    pub modal_tick: MinMedianMax,
    pub price_decimals: MinMedianMax,
    pub mean_duration_s: MinMedianMax,
    pub size_round_frac: MinMedianMax,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorScalars {
    #[serde(default)]
    pub symbol: String,
    pub modal_tick: Decimal,
    pub price_decimals: u32,
    pub mean_duration_s: f64,
    pub size_round_frac: f64,
    pub start_price: Decimal,
    pub typical_size: Decimal,
    pub vol_scalar: f64,
}

impl GeneratorScalars {
    #[must_use]
    pub fn from_fingerprint_medians(symbol: &str, fp: &Fingerprint) -> Self {
        Self {
            symbol: symbol.to_string(),
            modal_tick: decimal_from_f64(fp.scalar_ranges.modal_tick.median),
            price_decimals: fp.scalar_ranges.price_decimals.median.round() as u32,
            mean_duration_s: fp.scalar_ranges.mean_duration_s.median,
            size_round_frac: fp.scalar_ranges.size_round_frac.median,
            start_price: Decimal::from(START_PRICE_USD),
            typical_size: Decimal::new(TYPICAL_SIZE_MANTISSA, TYPICAL_SIZE_SCALE),
            vol_scalar: VOL_SCALAR,
        }
    }

    #[must_use]
    pub fn xbtusd_anchor(fp: &Fingerprint) -> Self {
        Self {
            symbol: "XBTUSD".to_string(),
            modal_tick: Decimal::new(1, 1),
            price_decimals: 1,
            mean_duration_s: fp.scalar_ranges.mean_duration_s.median,
            size_round_frac: fp.scalar_ranges.size_round_frac.median,
            start_price: Decimal::from(START_PRICE_USD),
            typical_size: Decimal::new(TYPICAL_SIZE_MANTISSA, TYPICAL_SIZE_SCALE),
            vol_scalar: VOL_SCALAR,
        }
    }

    pub fn validate(&self, fp: &Fingerprint) -> Result<(), ScalarError> {
        // A `symbol` omitted from config serde-defaults to "". Every trade this
        // source emits would then carry an empty symbol, keying
        // `TickRuleAggressor` per-symbol state and any symbol-keyed consumer on
        // the same empty string and cross-contaminating instruments. Reject it.
        if self.symbol.is_empty() {
            return Err(ScalarError { field: "symbol" });
        }
        validate_f64(
            "modal_tick",
            decimal_to_f64(self.modal_tick),
            &fp.scalar_ranges.modal_tick,
        )?;
        validate_f64(
            "price_decimals",
            f64::from(self.price_decimals),
            &fp.scalar_ranges.price_decimals,
        )?;
        // `modal_tick` must be representable at `price_decimals`. `next_price`
        // snaps every quote with `round_dp(price_decimals)`; a tick carrying
        // more decimal places than that is silently coarsened to the
        // 10^-price_decimals grid, so the configured modal_tick stops being the
        // real tick (modal_tick 1e-7 with price_decimals 1 collapses every
        // price onto a 0.1 grid). The on-grid invariant still holds against the
        // COARSER grid, so no test or runtime check catches it - only this does.
        // The server enforces the same relationship on its instrument defs via
        // `on_increment`; this is the generator-layer twin.
        if self.modal_tick.normalize().scale() > self.price_decimals {
            return Err(ScalarError {
                field: "modal_tick",
            });
        }
        validate_f64(
            "mean_duration_s",
            self.mean_duration_s,
            &fp.scalar_ranges.mean_duration_s,
        )?;
        validate_f64(
            "size_round_frac",
            self.size_round_frac,
            &fp.scalar_ranges.size_round_frac,
        )?;
        // `start_price` seeds `vol.mid`, which `next_latent_mid` clamps to
        // [modal_tick, MID_CEILING] on the very first tick. A start_price above
        // the ceiling instantly collapses the mid (an ~80 percent crash for
        // 5e9); one below a single tick instantly jumps up. Both are silent, so
        // keep start_price inside the clamp band (this subsumes the old
        // strictly-positive check, since modal_tick is already validated > 0).
        if self.start_price < self.modal_tick || self.start_price > Decimal::from(1_000_000_000_i64)
        {
            return Err(ScalarError {
                field: "start_price",
            });
        }
        if self.typical_size <= Decimal::ZERO {
            return Err(ScalarError {
                field: "typical_size",
            });
        }
        if !strictly_positive_finite(self.vol_scalar) {
            return Err(ScalarError {
                field: "vol_scalar",
            });
        }
        // `vol_scalar` feeds GarchVol's unconditional variance (vol_scalar^2),
        // which `next_latent_mid` caps at (GARCH_SIGMA_CAP * clamp_mult)^2. In
        // the base (no-regime) path clamp_mult is 1.0, so any vol_scalar above
        // GARCH_SIGMA_CAP is pinned at the cap on the first tick and the knob's
        // documented per-tick-volatility meaning silently stops holding. Reject
        // a value the base regime cannot honor rather than accept a dead knob (a
        // VolStorm can lift the cap, but that is a transient per-subscription
        // overlay, not the construction-time scalar's contract).
        if self.vol_scalar > GARCH_SIGMA_CAP {
            return Err(ScalarError {
                field: "vol_scalar",
            });
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalarError {
    pub field: &'static str,
}

/// Why a [`GeneratedSource`] construction can fail: either the scalar config or
/// the session profile is outside the fingerprint's validated ranges. Returned
/// by the fallible [`GeneratedSource::try_new`] /
/// [`GeneratedSource::try_new_with_session_profile`] constructors so a caller
/// holding config that has not been pre-validated can surface the error instead
/// of tripping the panic inside the infallible [`GeneratedSource::new`] family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedSourceError {
    Scalar(ScalarError),
    Session(SessionProfileError),
}

/// `Clone` is the substrate of the checkpointed seek (`CheckpointIndex`): the
/// generator is a path-dependent walk whose entire future is a pure function of
/// its current state, so a clone taken at tick N, advanced, reproduces ticks
/// N+1, N+2, ... byte-for-byte. Every field is `Clone`, including the
/// `ChaCha12Rng` (rand's `StdRng` is the same cipher but dropped `Clone`, which is
/// why the generator holds `ChaCha12Rng` directly).
#[derive(Clone)]
pub struct GeneratedSource {
    scalars: GeneratorScalars,
    rng: ChaCha12Rng,
    clock_ns: u64,
    acd: AcdClock,
    vol: GarchVol,
    session: SessionModulator,
    bounce: BounceState,
    duration_dist: Weibull<f64>,
    normal: Normal<f64>,
    chi_squared: ChiSquared<f64>,
    size_dist: LogNormal<f64>,
    tick_f64: f64,
    regime: RegimeState,
}

impl GeneratedSource {
    #[must_use]
    pub fn new(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
    ) -> Self {
        Self::new_with_session_profile(scalars, seed, start_ts, fp, &fp.session_profile, regime)
    }

    #[must_use]
    pub fn new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
    ) -> Self {
        Self::with_clamp_override(scalars, seed, start_ts, fp, session, regime, None)
    }

    /// Fallible twin of [`GeneratedSource::new`]. Both `scalars` and the
    /// fingerprint's session profile `Deserialize` straight from user config, so
    /// a caller holding un-pre-validated input should route through this and
    /// surface a [`GeneratedSourceError`] rather than let the infallible `new`
    /// turn a config typo into a process panic. `new` is `try_new(..).expect(..)`.
    pub fn try_new(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_with_clamp_override(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            None,
        )
    }

    /// Fallible twin of [`GeneratedSource::new_with_session_profile`] - same
    /// rationale as [`GeneratedSource::try_new`], but for the explicit-session
    /// path where the profile is also untrusted config.
    pub fn try_new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_with_clamp_override(scalars, seed, start_ts, fp, session, regime, None)
    }

    /// Infallible wrapper: panics if either input is outside the fingerprint
    /// ranges. Callers building from the committed fingerprint (valid by
    /// construction) use this via `new` / `new_with_session_profile`; callers
    /// with untrusted config use the `try_*` twins above instead.
    fn with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
    ) -> Self {
        Self::try_with_clamp_override(scalars, seed, start_ts, fp, session, regime, clamp_override)
            .expect("generated source inputs are inside fingerprint ranges")
    }

    // The only fallible inputs are `scalars`/`session`, guarded by the two
    // `?`-propagated `validate` calls at the top. The distribution constructors
    // below (`LogNormal`/`Weibull`/`Normal`/`ChiSquared`) take compile-time
    // constants that are always valid params, so their `expect`s cannot fire -
    // `unwrap_in_result` is silenced here because there is no meaningful error
    // variant to map them onto, not because a failure is being swallowed.
    #[expect(
        clippy::unwrap_in_result,
        reason = "distribution params are constant and valid; only the validated scalars/session can fail"
    )]
    fn try_with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
    ) -> Result<Self, GeneratedSourceError> {
        scalars
            .validate(fp)
            .map_err(GeneratedSourceError::Scalar)?;
        session.validate().map_err(GeneratedSourceError::Session)?;
        let mean_duration_s = scalars.mean_duration_s;
        let alpha = ACD_PERSISTENCE * ACD_FEEDBACK_SHARE;
        let beta = ACD_PERSISTENCE - alpha;
        let omega = mean_duration_s * (1.0 - ACD_PERSISTENCE);
        let vol = GarchVol::new(decimal_to_f64(scalars.start_price), scalars.vol_scalar);
        let size_median = decimal_to_f64(scalars.typical_size).max(f64::MIN_POSITIVE);
        let size_dist = LogNormal::new(size_median.ln(), SIZE_LOG_SIGMA).expect("valid lognormal");
        // Built before the struct literal because it borrows `scalars.symbol`,
        // which the literal moves; `start_ts` is the tape anchor RegimeState
        // needs to fail-close an already-elapsed ReopenGap.
        let regime = RegimeState::new(regime, clamp_override, start_ts, &scalars.symbol);
        Ok(Self {
            tick_f64: decimal_to_f64(scalars.modal_tick),
            scalars,
            rng: ChaCha12Rng::seed_from_u64(seed),
            clock_ns: start_ts,
            acd: AcdClock {
                omega,
                alpha,
                beta,
                psi: mean_duration_s,
                prev_duration_s: mean_duration_s,
                eps_mean: WEIBULL_MEAN_SHAPE_060,
            },
            vol,
            session: SessionModulator::new(session),
            bounce: BounceState {
                // Every realization opens on the Buyer side regardless of seed:
                // `prev_side` seeds to Buyer and the low regime flips it only at
                // BOUNCE_LOW_FLIP_PROB (0.02), so a fresh stream prints a long
                // Buyer run before the first flip. This seed-independent opening
                // bias is deliberate and left as-is: it is a start-of-stream
                // transient (a real bounce process is equally free to open either
                // way), it is fully deterministic, and re-seeding the side would
                // consume an extra RNG draw and break the committed fingerprint's
                // byte-identical golden stream for zero fidelity gain.
                prev_side: AggressorSide::Buyer,
                high_regime: false,
                drift_ticks: 0,
                drift_dir: 1,
                drift_hot: false,
                half_spread_ticks: HALF_SPREAD_TICKS,
            },
            duration_dist: Weibull::new(1.0, ACD_WEIBULL_SHAPE).expect("valid weibull"),
            normal: Normal::new(0.0, 1.0).expect("valid normal"),
            chi_squared: ChiSquared::new(STUDENT_T_DF).expect("valid chi-squared"),
            size_dist,
            regime,
        })
    }

    /// The simulated instant the generator has reached: the `ts_event` of the
    /// last emitted tick, i.e. the clock the NEXT `next_tick` advances from. A
    /// fresh source sits at its `start_ts`. `CheckpointIndex` uses this to place
    /// snapshots and to binary-search them against a seek target.
    #[must_use]
    pub fn clock_ns(&self) -> u64 {
        self.clock_ns
    }

    #[cfg(test)]
    fn new_with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
        clamp_mult: f64,
    ) -> Self {
        Self::with_clamp_override(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            Some(clamp_mult),
        )
    }

    fn next_duration_ns(&mut self) -> u64 {
        let raw_eps = self.duration_dist.sample(&mut self.rng);
        let eps = (raw_eps / self.acd.eps_mean).max(f64::MIN_POSITIVE);
        self.acd.psi = self.acd.omega
            + self.acd.alpha * self.acd.prev_duration_s
            + self.acd.beta * self.acd.psi;
        let duration_s = (self.acd.psi * eps).max(0.000_000_001);
        // ACD feedback sees the un-modulated duration so clustering dynamics are
        // unchanged; the session envelope only stretches or compresses the
        // realized gap.
        self.acd.prev_duration_s = duration_s;
        let arr_mult = self.session.arrival_mult(self.clock_ns);
        if arr_mult >= SESSION_CLOSED_ARR_MULT {
            // Open-market path: the multiplier sampled at the instant the gap
            // opens stretches the whole draw - the original math, unchanged
            // bit for bit so the committed fingerprint's golden stream stays
            // byte-identical (its multipliers never go near the gate). The
            // trailing `.min` is a pure safety rail: for any multiplier above
            // the gate and the validated thin_factor range it is far out of
            // reach of realistic draws, so it never alters an open-market gap;
            // it only guarantees the cast can never saturate to u64::MAX.
            let duration_s =
                ((duration_s / arr_mult) * self.regime.arrival_thin).max(0.000_000_001);
            return (duration_s * 1_000_000_000.0)
                .round()
                .max(1.0)
                .min(MAX_SESSION_GAP_NS as f64) as u64;
        }
        self.closed_window_gap_ns(duration_s)
    }

    /// Wall-clock gap for a duration draw whose gap OPENS inside a closed
    /// session window (arrival multiplier below `SESSION_CLOSED_ARR_MULT`).
    ///
    /// The open-market path in `next_duration_ns` samples the arrival
    /// multiplier once and stretches the entire draw by `1/mult` - fine while
    /// the multiplier is O(1), catastrophic when a share is near zero: the
    /// stretched gap wildly overshoots the closed window (share 1e-6 turns a
    /// ~7 s draw into ~80 days) and an extreme share saturates the f64->u64
    /// cast at u64::MAX, pinning the clock there so every later tick carries
    /// the same `ts_event` - breaking the strict monotonicity `monotonic_clock`
    /// pins and the ordering `MergeSource` and `seek_to` rely on.
    ///
    /// Here the draw is instead treated as a BUDGET of un-modulated seconds
    /// and converted to wall time by integrating the piecewise-constant
    /// session intensity hour by hour: each wall hour consumes
    /// `hour_seconds * rate` of budget, so a closed hour consumes almost
    /// nothing and the budget is spent almost entirely in the first open
    /// hour - the tape resumes roughly when the market reopens, which is the
    /// trading-hours semantics the near-zero-share mechanism promises.
    /// Day-of-week transitions land on hour boundaries, so stepping hours
    /// re-samples both curves. Deterministic: no RNG is consumed; the walk is
    /// a pure function of `clock_ns`, the profile and the draw, so same seed +
    /// anchor still yields the same stream.
    ///
    /// Residual limitations, stated honestly:
    /// - only gaps that OPEN below the gate take this path. A gap opening in
    ///   an open hour still crosses a later closed window at its open-hour
    ///   rate (a tick can print inside the closed window). That artifact
    ///   predates this path and is left in place deliberately: fixing it would
    ///   change every boundary-crossing gap and break the committed
    ///   fingerprint's byte-identical golden stream.
    /// - a profile whose EVERY hour is effectively closed can never spend the
    ///   budget; the walk caps at `MAX_SESSION_GAP_NS` per gap, so the clock
    ///   advances strictly (one tick per ~year) instead of freezing. Reaching
    ///   u64::MAX at all now requires actually simulating the ~580-year u64
    ///   nanosecond epoch - an inherent representation limit, no longer a
    ///   session artifact.
    fn closed_window_gap_ns(&self, duration_s: f64) -> u64 {
        let mut budget_s = duration_s;
        let mut pos_ns = self.clock_ns;
        let mut gap_ns: u64 = 0;
        while gap_ns < MAX_SESSION_GAP_NS {
            // Effective arrival rate over this hour segment: session shares are
            // validated strictly positive finite and thin_factor is validated
            // in [1, 1000], so the rate is positive - unless the product
            // underflows to 0.0, in which case `budget_s / rate` is +inf, the
            // residual branch below never fires, and the walk runs to the cap
            // (the venue is closed harder than f64 can express).
            let rate = self.session.arrival_mult(pos_ns) / self.regime.arrival_thin;
            let to_boundary_ns = NS_PER_HOUR - (pos_ns % NS_PER_HOUR);
            let need_ns = (budget_s / rate) * 1_000_000_000.0;
            if need_ns <= to_boundary_ns as f64 {
                // The budget runs out inside this hour: spend the residual and
                // stop. `need_ns` is at most an hour in ns here, so the cast is
                // exact; the floor keeps the clock strictly advancing.
                let residual_ns = (need_ns.round() as u64).max(1);
                return gap_ns
                    .saturating_add(residual_ns)
                    .clamp(1, MAX_SESSION_GAP_NS);
            }
            budget_s -= (to_boundary_ns as f64 / 1_000_000_000.0) * rate;
            pos_ns = pos_ns.saturating_add(to_boundary_ns);
            gap_ns = gap_ns.saturating_add(to_boundary_ns);
        }
        MAX_SESSION_GAP_NS
    }

    fn next_latent_mid(&mut self) -> f64 {
        let normal = self.normal.sample(&mut self.rng);
        // Guard against a chi-squared draw that underflows to exactly 0.0: an
        // unguarded 0.0 denominator makes `student_t` `0.0/0.0 = NaN` when
        // `normal` also happens to be 0.0, and `f64::clamp` propagates NaN
        // through `base_return` into `mid`, poisoning the walk for the rest of
        // the session. Astronomically unlikely from a continuous distribution,
        // but cheap to close off (matches the `f64::MIN_POSITIVE` floors used
        // elsewhere in this file for the same reason).
        let chi = self
            .chi_squared
            .sample(&mut self.rng)
            .max(f64::MIN_POSITIVE);
        let student_t = normal / (chi / STUDENT_T_DF).sqrt();
        self.vol.sigma2 = self.vol.a0
            + self.vol.a1 * self.vol.prev_return.powi(2)
            + self.vol.b1 * self.vol.sigma2;
        let sigma_cap = (GARCH_SIGMA_CAP * self.regime.clamp_mult).powi(2);
        self.vol.sigma2 = self.vol.sigma2.min(sigma_cap);
        // FEEDBACK clamp: `base_return` (which feeds `prev_return`) and the
        // sigma2 cap above use the regime's BASE clamp lift (vol_mult for a
        // storm, the test override, or 1.0). A SessionEdgeSpike deliberately
        // does NOT lift this - keeping the GARCH recursion state (sigma2,
        // prev_return) byte-identical to a clean run is what lets the spike
        // leave zero trace outside its hour window.
        let feedback_cap = MAX_ABS_RETURN * self.regime.clamp_mult;
        let base_return = (self.vol.sigma2.sqrt() * student_t).clamp(-feedback_cap, feedback_cap);
        // GARCH feedback sees the un-modulated return so volatility clustering
        // is unchanged; the session envelope scales the realized RMS on top,
        // then the hard clamp still bounds the mid update.
        self.vol.prev_return = base_return;
        // Vol composition convention (see also RegimeState::vol_mult): the
        // session envelope and the regime envelope COMPOSE MULTIPLICATIVELY here
        // (session 1.0 = no session bias, regime 1.0 = no regime bias, so the
        // product is the combined RMS scale). Inside the regime envelope a
        // SessionEdgeSpike instead composes ADDITIVELY onto the storm baseline
        // (vol_mult + edge_mult). The two conventions are intentional and rely on
        // both neutral values being 1.0; do NOT restructure either into the other
        // (a future regime that set both vol_mult and an edge spike would want the
        // add re-examined - today the match is exclusive so only one is non-unit).
        let vol_mult = self.session.vol_mult(self.clock_ns) * self.regime.vol_mult(self.clock_ns);
        // REALIZED clamp: the composed return that actually moves the mid uses
        // the WINDOWED clamp. For VolStorm and the clean/drought/reopen regimes
        // this equals `feedback_cap` bit for bit (edge_extra is 0), so their
        // streams are unchanged. A SessionEdgeSpike lifts it only INSIDE its
        // hour window, by exactly the same (1.0 + extra_vol_mult) that amplifies
        // vol_mult there - so a large extra_vol_mult no longer saturates the
        // realized spike against MAX_ABS_RETURN the way it did when this clamp
        // was pinned at 1.0. Outside the window `realized_clamp_mult` returns the
        // base clamp, so every out-of-window return stays byte-identical.
        let realized_cap = MAX_ABS_RETURN * self.regime.realized_clamp_mult(self.clock_ns);
        let return_n = (base_return * vol_mult).clamp(-realized_cap, realized_cap);
        self.vol.mid = (self.vol.mid * return_n.exp())
            .max(self.tick_f64)
            .min(MID_CEILING);
        self.vol.mid
    }

    fn next_price(&mut self, mid: f64) -> (Decimal, AggressorSide) {
        let side = self.bounce.next_side(&mut self.rng);
        self.bounce.next_drift(&mut self.rng);
        let mid_ticks = mid / self.tick_f64 + self.bounce.drift_ticks as f64;
        let price_ticks = match side {
            AggressorSide::Buyer => (mid_ticks + self.bounce.half_spread_ticks).ceil(),
            AggressorSide::Seller => (mid_ticks - self.bounce.half_spread_ticks).floor(),
            // Invariant-protected, not a runtime check: `side` is the return of
            // `BounceState::next_side` directly above, whose every branch yields
            // Buyer or Seller (its flip match collapses NoAggressor into Buyer).
            // The generator never produces a neutral aggressor - that side exists
            // only for the CSV/tick-rule lineage - so this arm is dead by
            // construction. It stays as a guard so a future edit to next_side that
            // started emitting NoAggressor would fail loudly here rather than
            // silently quoting a mid-priced trade.
            AggressorSide::NoAggressor => unreachable!("bounce only emits buyer or seller"),
        };
        // `mid` is floored at one tick (see next_latent_mid), but drift_ticks
        // is an unbounded accumulated random walk with no such floor: a long
        // enough same-direction high-regime streak can push mid_ticks (and
        // hence price_ticks) to zero or negative, quoting a zero/negative
        // price. Clamp the quoted tick count the same way mid itself is
        // clamped, so the drifted quote can never undercut one tick.
        let price_ticks = price_ticks.max(1.0);
        let price = decimal_from_f64(price_ticks * self.tick_f64);
        (price.round_dp(self.scalars.price_decimals), side)
    }

    fn next_size(&mut self) -> Decimal {
        let base = self.size_dist.sample(&mut self.rng).max(f64::MIN_POSITIVE);
        let size = if self.rng.random_bool(self.scalars.size_round_frac) {
            round_lot_size(base)
        } else {
            decimal_from_f64(base).round_dp(SIZE_DECIMALS)
        };
        size.max(Decimal::new(1, SIZE_DECIMALS))
    }
}

impl TickSource for GeneratedSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        let dt_ns = self.next_duration_ns();
        // Order is load-bearing: next_duration_ns reads the arrival multiplier at
        // the START of the gap (the clock has not advanced yet), then the clock
        // steps, then next_latent_mid reads the volatility multiplier at the
        // instant the trade PRINTS. A duration belongs to the session window it
        // opens in; a trade's volatility belongs to the window it prints in. Do
        // not reorder these three lines to "tidy" them - it silently shifts which
        // session window each tick is attributed to.
        let old_clock_ns = self.clock_ns;
        self.clock_ns = self.clock_ns.saturating_add(dt_ns);
        if let Some(reopen) = self.regime.take_reopen_crossed(old_clock_ns, self.clock_ns) {
            self.clock_ns = self.clock_ns.saturating_add(reopen.halt_ns);
            self.vol.mid = (self.vol.mid * reopen.gap_frac.exp())
                .max(self.tick_f64)
                .min(MID_CEILING);
        }
        let mid = self.next_latent_mid();
        let (price, aggressor) = self.next_price(mid);
        let size = self.next_size();
        Some(TickEvent::Trade(TradeTick {
            symbol: self.scalars.symbol.clone(),
            price,
            size,
            aggressor,
            ts_event: self.clock_ns,
        }))
    }
}

/// Turns the from-origin seek from O(distance) into O(K): instead of re-walking
/// the path-dependent generator from the tape origin to a target, snapshot the
/// walk (a `GeneratedSource` clone) every `K` ticks and, to reach a target,
/// resume from the latest snapshot at or before it and replay only the residual
/// (< K ticks).
///
/// This lifts the accelerated uptime ceiling Landing 1 priced: a from-origin
/// seek to sim-now grows with session length and eventually blows the backstop
/// cap, but a resume-and-replay is flat in `K` no matter how far the session has
/// run. The index is shared per realization (one symbol's clean tape) and
/// extended lazily and monotonically, so the O(span) walk to the frontier is
/// paid once across all of that realization's seeks, never per request.
///
/// The realization is preserved byte-for-byte: a snapshot is the exact walk
/// state, so resuming it and replaying yields the same ticks a from-origin run
/// would (the golden sequence is unchanged, and `checkpoint_resume_is_byte_identical`
/// pins the resume path directly).
/// Hard ceiling on the number of snapshots one `CheckpointIndex` retains. Once
/// `extend_to` would push past this, `coarsen` halves the count and doubles the
/// spacing, so the index's memory is bounded by `MAX_CHECKPOINTS` generator
/// clones regardless of how long an accelerated session runs - closing the
/// unbounded per-`k`-ticks growth. 4096 keeps coarsening rare (the first only
/// after `4096 * k` ticks, ~34M ticks at the server's K = 8192) so the residual
/// drain stays at the base `k` for any realistic run, while capping worst-case
/// memory at a few tens of MB.
const MAX_CHECKPOINTS: usize = 4096;

pub struct CheckpointIndex {
    /// A generator advanced to the frontier; cloned to extend the chain and to
    /// hand out positioned sources. Carries the immutable config every snapshot
    /// shares.
    lead: GeneratedSource,
    /// Snapshots in ascending `clock_ns`; `[0]` is the origin (pre-first-tick).
    checkpoints: Vec<GeneratedSource>,
    /// Ticks the lead has advanced since the last snapshot was taken.
    since_snapshot: usize,
    /// Snapshot spacing in ticks.
    k: usize,
    /// Runaway backstop: the most ticks `extend_to` will walk the lead in a
    /// single call. The server refuses a `start` below `data_origin`, but
    /// nothing rejects an absurd `start` *above* the live frontier (a bogus or
    /// far-future window), and `GeneratedSource::next_tick` never ends - so an
    /// uncapped `extend_to` would spin the path-dependent walk indefinitely
    /// while holding the shared index mutex. A target past this bound leaves the
    /// frontier short; the caller's own `BoundedSeek` then caps too and the seek
    /// yields an empty page instead of hanging. Sized to the same budget as the
    /// from-origin cap, so every legitimate target (warmup, live sim-now, a
    /// poll's modest per-step delta) sits far inside it.
    max_extend: usize,
}

impl CheckpointIndex {
    /// Build an index over the realization `origin` heads. `origin` must be a
    /// fresh source at the tape origin (no ticks drawn yet); its pre-first-tick
    /// state becomes checkpoint 0. `max_extend` bounds the per-call walk (see the
    /// field doc) - pass the caller's from-origin seek cap.
    #[must_use]
    pub fn new(origin: GeneratedSource, k: usize, max_extend: usize) -> Self {
        assert!(k > 0, "checkpoint spacing must be positive");
        assert!(max_extend > 0, "extension cap must be positive");
        Self {
            checkpoints: vec![origin.clone()],
            lead: origin,
            since_snapshot: 0,
            k,
            max_extend,
        }
    }

    /// Extend the snapshot chain until it covers `target`, advancing the lead and
    /// snapshotting every `k` ticks. Monotonic: a later, further target only does
    /// the new delta, so the from-origin walk is paid once across all seeks. The
    /// walk is bounded by `max_extend` per call (the runaway backstop); a target
    /// beyond that leaves the lead short and the caller's seek caps the rest.
    fn extend_to(&mut self, target: u64) {
        let mut walked = 0usize;
        while self.lead.clock_ns() < target {
            if walked >= self.max_extend {
                break;
            }
            if self.lead.next_tick().is_none() {
                break;
            }
            walked += 1;
            self.since_snapshot += 1;
            if self.since_snapshot >= self.k {
                self.checkpoints.push(self.lead.clone());
                self.since_snapshot = 0;
                if self.checkpoints.len() > MAX_CHECKPOINTS {
                    self.coarsen();
                }
            }
        }
    }

    /// Halve the snapshot count once it exceeds `MAX_CHECKPOINTS` by dropping
    /// every other checkpoint and doubling the spacing `k`. This is what makes
    /// the index's memory a HARD ceiling (`MAX_CHECKPOINTS` generator clones)
    /// over any session length, rather than a clone per `k` ticks growing
    /// without bound.
    ///
    /// It is correctness-preserving: every retained checkpoint is still the
    /// EXACT walk state at its `clock_ns`, so resuming from the coarser grid and
    /// replaying reproduces the identical tape - dropping an intermediate
    /// snapshot only lengthens the residual drain (`source_at_or_before` now
    /// resumes up to the new, larger `k` ticks before the target), it never
    /// changes which ticks are emitted. The origin (index 0) is always retained
    /// as the pre-first-tick fallback. The residual drain stays bounded by the
    /// caller's `BoundedSeek`; `k` grows only logarithmically in session length
    /// (a doubling costs `MAX_CHECKPOINTS * k` more ticks), so it never
    /// realistically approaches that cap.
    fn coarsen(&mut self) {
        let mut idx = 0usize;
        self.checkpoints.retain(|_| {
            let keep = idx.is_multiple_of(2);
            idx += 1;
            keep
        });
        self.k = self.k.saturating_mul(2);
    }

    /// A fresh generator positioned at the latest checkpoint strictly before
    /// `target` (or the origin when nothing is). The caller drains it forward to
    /// the exact target (< K ticks) via the normal seek; the returned source is
    /// an independent clone, so the shared index is untouched by that replay.
    pub fn source_at_or_before(&mut self, target: u64) -> GeneratedSource {
        self.extend_to(target);
        // Strictly-before partition (`<`, not `<=`): a checkpoint's `clock_ns`
        // is the `ts_event` of the last tick it has ALREADY consumed, so a
        // checkpoint whose `clock_ns` EQUALS the target has the boundary tick
        // behind it. Resuming there and seeking to `target` (the trait-default
        // seek returns the first tick with `ts_event >= target`) would skip
        // that boundary tick, while a from-origin seek returns it - the two
        // paths the byte-identical guarantee promises are one tape would
        // disagree by exactly one tick. The collision is not hypothetical:
        // snapshots land on every K-th tick's exact `ts_event`, and pollers
        // legitimately pass an emitted tick's exact `ts_event` as the seek
        // target, so under `<=` one tick per ~K such seeks would vanish. With
        // `<` the resume point sits strictly before the target and the
        // residual replay re-emits the boundary tick itself
        // (`checkpoint_resume_at_exact_boundary_ts_returns_boundary_tick` pins
        // this). When `target` is at or before the origin's clock the
        // partition point is 0 and the `saturating_sub` keeps us on the
        // origin, which has emitted nothing and is therefore always a safe
        // resume point.
        let idx = self
            .checkpoints
            .partition_point(|c| c.clock_ns() < target)
            .saturating_sub(1);
        self.checkpoints[idx].clone()
    }

    /// Number of snapshots held (origin included). For tests and the measurement.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }
}

#[derive(Clone)]
struct AcdClock {
    omega: f64,
    alpha: f64,
    beta: f64,
    psi: f64,
    prev_duration_s: f64,
    eps_mean: f64,
}

#[derive(Debug, Clone)]
struct RegimeState {
    arrival_thin: f64,
    vol_mult: f64,
    clamp_mult: f64,
    edge: Option<EdgeSpike>,
    reopen: Option<Reopen>,
}

impl RegimeState {
    fn new(
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
        start_ts: u64,
        symbol: &str,
    ) -> Self {
        let mut state = Self {
            arrival_thin: 1.0,
            vol_mult: 1.0,
            clamp_mult: 1.0,
            edge: None,
            reopen: None,
        };

        // LOCKSTEP with mogwai_protocol::validate_market_regime: these arms
        // destructure the same MarketRegime variants that validator range-checks,
        // one crate away, with no compiler link forcing the two to agree. A new
        // variant or a renamed field must be mirrored in BOTH - add the arm here
        // and the matching guard there. Notably SessionEdgeSpike's
        // `start_hour < end_hour` invariant is enforced only in the validator and
        // trusted at runtime by EdgeSpike's window comparison below.
        match regime {
            Some(MarketRegime::VolStorm { vol_mult }) => {
                state.vol_mult = vol_mult;
                // Default the clamp lift to the storm multiplier; the test-only
                // clamp_override below uniformly replaces it when present (it is
                // how the pricing instrument pins the clamp to disprove the bare
                // multiply), so there is no need to fold the override in here.
                state.clamp_mult = vol_mult;
            }
            Some(MarketRegime::LiquidityDrought { thin_factor }) => {
                state.arrival_thin = thin_factor;
            }
            Some(MarketRegime::SessionEdgeSpike {
                start_hour,
                end_hour,
                extra_vol_mult,
            }) => {
                // `start_hour < end_hour` is enforced by
                // mogwai_protocol::validate_market_regime, not here - this
                // lockstep match trusts a validated regime reached it. A
                // regime that bypasses the validator (a test fixture, or a
                // future construction path) with an inverted window would
                // silently produce a zero-width spike in vol_mult below; catch
                // that in debug builds instead of staying silent.
                debug_assert!(
                    start_hour < end_hour,
                    "SessionEdgeSpike window must satisfy start_hour < end_hour \
                     (start_hour={start_hour}, end_hour={end_hour})"
                );
                state.edge = Some(EdgeSpike {
                    start_hour,
                    end_hour,
                    extra_vol_mult,
                });
            }
            Some(MarketRegime::ReopenGap {
                at_ts,
                halt_secs,
                gap_frac,
            }) => {
                // Crossing detection (`take_reopen_crossed`) fires when a
                // tick's gap straddles the instant:
                // `old_clock < at_ts && at_ts <= new_clock`. The very first
                // gap opens at the tape anchor (`start_ts`), so an `at_ts` at
                // or before the anchor can NEVER satisfy the first conjunct -
                // the halt would sit armed forever, silently inert.
                // `validate_market_regime` rejects `at_ts == 0` for exactly
                // that failure mode, but any other already-elapsed instant
                // reproduces it one layer down, so treat it the way the server
                // treats out-of-band divergences: consume it at construction
                // (fail closed) and say so, rather than fabricate a halt at
                // whatever tick the anchor happens to make first. No RNG is
                // drawn either way, so an elapsed ReopenGap leaves a stream
                // byte-identical to no regime at all
                // (`reopen_gap_at_or_before_anchor_is_consumed_and_matches_clean`
                // pins this). No tracing dep in this crate, so stderr is the
                // visible channel - same convention as KrakenCsvSource.
                if at_ts <= start_ts {
                    eprintln!(
                        "GeneratedSource({symbol}): ReopenGap at_ts {at_ts} is at or before \
                         the tape anchor {start_ts}; the halt has already elapsed and is \
                         dropped (it would never fire)"
                    );
                } else {
                    state.reopen = Some(Reopen {
                        at_ts,
                        halt_ns: halt_secs.saturating_mul(1_000_000_000),
                        gap_frac,
                    });
                }
            }
            None => {}
        }

        if let Some(clamp_mult) = clamp_override {
            state.clamp_mult = clamp_mult;
        }

        state
    }

    // `extra_vol_mult` inside a SessionEdgeSpike's UTC hour window, 0.0 outside
    // it (and 0.0 when no edge spike is armed). Shared by `vol_mult` and
    // `realized_clamp_mult` so the RMS amplification and the clamp lift track
    // the exact same window, by construction, and cannot drift apart.
    fn edge_extra(&self, clock_ns: u64) -> f64 {
        self.edge.as_ref().map_or(0.0, |edge| {
            let hour = utc_hour(clock_ns);
            if usize::from(edge.start_hour) <= hour && hour < usize::from(edge.end_hour) {
                edge.extra_vol_mult
            } else {
                0.0
            }
        })
    }

    fn vol_mult(&self, clock_ns: u64) -> f64 {
        // ADDITIVE within the regime envelope: the neutral value is 1.0 (set in
        // `new`), and a SessionEdgeSpike layers its extra_vol_mult on top by
        // addition (out of window edge_extra is 0.0, leaving the baseline). The
        // RESULT is then composed MULTIPLICATIVELY with the session envelope in
        // next_latent_mid - see the convention note there. The mix is deliberate
        // and load-bearing on both neutral values being 1.0.
        self.vol_mult + self.edge_extra(clock_ns)
    }

    // Clamp multiplier for the REALIZED return (the one that moves the mid), as
    // opposed to the GARCH feedback which uses `self.clamp_mult` raw. Mirrors
    // `vol_mult`'s additive structure so the realized clamp is lifted exactly
    // where and by exactly how much the realized RMS is amplified: the base
    // clamp (vol_mult for a storm, the test override, or 1.0) plus a
    // SessionEdgeSpike's in-window `extra_vol_mult`. Keeping this SEPARATE from
    // the feedback clamp is what lets the edge spike lift its in-window ceiling
    // while leaving the recursion state - and therefore every out-of-window
    // tick - byte-identical to a clean run. For VolStorm and the
    // clean/drought/reopen regimes `edge_extra` is 0.0, so this equals
    // `self.clamp_mult` and their streams are unchanged.
    fn realized_clamp_mult(&self, clock_ns: u64) -> f64 {
        self.clamp_mult + self.edge_extra(clock_ns)
    }

    fn take_reopen_crossed(&mut self, old_clock_ns: u64, new_clock_ns: u64) -> Option<Reopen> {
        if self
            .reopen
            .as_ref()
            .is_some_and(|reopen| old_clock_ns < reopen.at_ts && reopen.at_ts <= new_clock_ns)
        {
            self.reopen.take()
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
struct EdgeSpike {
    start_hour: u8,
    end_hour: u8,
    extra_vol_mult: f64,
}

#[derive(Debug, Clone, Copy)]
struct Reopen {
    at_ts: u64,
    halt_ns: u64,
    gap_frac: f64,
}

#[derive(Clone)]
struct GarchVol {
    a0: f64,
    a1: f64,
    b1: f64,
    sigma2: f64,
    prev_return: f64,
    mid: f64,
}

impl GarchVol {
    fn new(mid: f64, vol_scalar: f64) -> Self {
        let unconditional_var = vol_scalar.powi(2);
        let persistence = GARCH_ARCH + GARCH_GARCH;
        Self {
            a0: unconditional_var * (1.0 - persistence),
            a1: GARCH_ARCH,
            b1: GARCH_GARCH,
            sigma2: unconditional_var,
            prev_return: 0.0,
            mid,
        }
    }
}

#[derive(Clone)]
struct BounceState {
    prev_side: AggressorSide,
    high_regime: bool,
    /// Accumulated same-direction on-grid drift, in ticks, applied on top of the
    /// latent mid in `next_price`. This is an UNBOUNDED, never-reset random walk
    /// (it only advances inside the high regime), so over a long run the printed
    /// price = mid + drift can wander arbitrarily far from `start_price` even
    /// though the mid itself is clamped to [tick, MID_CEILING]. Left unbounded
    /// deliberately: the diffusion is slow, the downside is fenced by the
    /// `price_ticks.max(1.0)` floor in `next_price` (a quote can never undercut
    /// one tick), and the only untethered direction is upward and cosmetic.
    /// Bounding or mean-reverting the drift would change the on-grid walk and
    /// break the committed fingerprint's byte-identical golden stream, so the
    /// long-run price un-tethering is documented rather than fixed.
    drift_ticks: i64,
    drift_dir: i64,
    drift_hot: bool,
    half_spread_ticks: f64,
}

impl BounceState {
    fn next_drift(&mut self, rng: &mut ChaCha12Rng) {
        if !self.high_regime {
            return;
        }
        if rng.random_bool(DRIFT_DIR_FLIP_PROB) {
            self.drift_dir *= -1;
        }
        let p_drift = if self.drift_hot {
            HOT_DRIFT_PROB
        } else {
            HIGH_REGIME_DRIFT_PROB
        };
        if rng.random_bool(p_drift) {
            self.drift_ticks += self.drift_dir;
            self.drift_hot = true;
        } else {
            self.drift_hot = false;
        }
    }

    fn next_side(&mut self, rng: &mut ChaCha12Rng) -> AggressorSide {
        if self.high_regime {
            if rng.random_bool(BOUNCE_HIGH_TO_LOW_PROB) {
                self.high_regime = false;
            }
        } else if rng.random_bool(BOUNCE_LOW_TO_HIGH_PROB) {
            self.high_regime = true;
        }
        let p_flip = if self.high_regime {
            BOUNCE_HIGH_FLIP_PROB
        } else {
            BOUNCE_LOW_FLIP_PROB
        };
        if rng.random_bool(p_flip) {
            self.prev_side = match self.prev_side {
                AggressorSide::Buyer => AggressorSide::Seller,
                AggressorSide::Seller | AggressorSide::NoAggressor => AggressorSide::Buyer,
            };
        }
        self.prev_side
    }
}

fn validate_f64(field: &'static str, value: f64, range: &MinMedianMax) -> Result<(), ScalarError> {
    if range.contains(value) {
        Ok(())
    } else {
        Err(ScalarError { field })
    }
}

// Saturating f64 -> Decimal conversion: no internal generator draw can panic
// here. The pinned size/price distributions keep draws far inside Decimal
// range in practice, but a sufficiently extreme heavy-tail sample (or a NaN)
// would make `Decimal::from_f64` return None, so we clamp to the nearest
// representable value instead of unwrapping. NaN maps to zero (no sign to
// preserve); +/-inf and out-of-range finite magnitudes saturate to
// Decimal::MAX / Decimal::MIN by sign.
//
// Deliberately NOT unified with `mogwai_protocol::decimal_from_f64`, which
// zeroes +/-inf instead of saturating (it has a test pinning that behaviour).
// For a generated price/size, clamping an overflowed draw to a huge-but-valid
// magnitude is a safer failure mode than collapsing it to zero, so this stays
// local. The reverse direction (`decimal_to_f64`) is byte-identical in both
// crates and is imported from the protocol helper instead of redefined here.
fn decimal_from_f64(value: f64) -> Decimal {
    if let Some(decimal) = Decimal::from_f64(value) {
        return decimal;
    }
    if value.is_nan() {
        Decimal::ZERO
    } else if value.is_sign_positive() {
        Decimal::MAX
    } else {
        Decimal::MIN
    }
}

fn round_lot_size(base: f64) -> Decimal {
    if base >= 1.0 {
        decimal_from_f64(base.round().max(1.0))
    } else {
        decimal_from_f64((base * 10.0).round().max(1.0) / 10.0).round_dp(1)
    }
}

// Mean of a Weibull(scale = 1, shape = 0.60): gamma(1 + 1/0.60) = gamma(2.6666...).
// The ACD clock divides each duration innovation by this so the latent process
// targets a unit mean; the sole consumer is the construction-time `eps_mean` below.
// This was computed by a Lanczos approximation (g = 7, n = 9) of the gamma function,
// but gamma was only ever called with this one argument, so the series has been
// replaced by its result as a literal. The literal is the shortest decimal that
// round-trips to the exact f64 the series produced (bits 0x3ff812bdbf467568,
// identical in debug and release - no FP-contraction divergence), so it reproduces
// the byte-identical golden stream (`clean_regime_is_byte_identical`); the tolerance
// test below guards the magnitude against a typo. `rand_distr 0.4`'s Weibull exposes
// no `mean()` accessor, which is why the mean lives here as a constant.
const WEIBULL_MEAN_SHAPE_060: f64 = 1.504_575_488_251_555_6;

#[cfg(test)]
mod tests {
    use super::*;

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
    const INTENSITY_SHARE_ABS_TOL: f64 = 0.006;
    const DOW_SHARE_ABS_TOL: f64 = 0.01;
    const SESSION_START_TS: u64 = 1_700_438_400_000_000_000;

    #[test]
    fn fingerprint_parses() {
        let fp = Fingerprint::from_repo_json();
        assert_eq!(fp.session_profile.intensity_hour.len(), 24);
        assert_eq!(fp.session_profile.dow_weight.len(), 7);
        assert_eq!(fp.golden_targets.abs_return_acf_anchor.len(), 50);
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
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.12498350, aggressor: Buyer, ts_event: 1932367546 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.10200766, aggressor: Buyer, ts_event: 14404949050 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.1, aggressor: Buyer, ts_event: 63395677414 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.08673040, aggressor: Buyer, ts_event: 70348510297 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.01819669, aggressor: Buyer, ts_event: 70444232828 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.09715509, aggressor: Buyer, ts_event: 70448615756 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.08202765, aggressor: Buyer, ts_event: 70701257715 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.08319530, aggressor: Buyer, ts_event: 80028550942 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.09242684, aggressor: Buyer, ts_event: 98575731579 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.09712421, aggressor: Buyer, ts_event: 98783459004 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.02064065, aggressor: Buyer, ts_event: 105144479491 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.29393977, aggressor: Buyer, ts_event: 105970801177 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.57435314, aggressor: Buyer, ts_event: 118332552074 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.18559909, aggressor: Buyer, ts_event: 126200221174 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.01540320, aggressor: Buyer, ts_event: 141379233446 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.07153911, aggressor: Buyer, ts_event: 155104805884 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.9, aggressor: Buyer, ts_event: 178963716674 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.1, aggressor: Buyer, ts_event: 215566810133 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.07602495, aggressor: Buyer, ts_event: 236005084772 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.22289688, aggressor: Buyer, ts_event: 240701378904 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.31333162, aggressor: Buyer, ts_event: 246071332070 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.47843512, aggressor: Buyer, ts_event: 263375527056 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.3, aggressor: Buyer, ts_event: 267600259492 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.17390790, aggressor: Buyer, ts_event: 298777679331 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.17095568, aggressor: Buyer, ts_event: 299630805098 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.1, aggressor: Buyer, ts_event: 315966784358 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.19798447, aggressor: Buyer, ts_event: 319016030880 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.76061526, aggressor: Buyer, ts_event: 348635836996 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.05957004, aggressor: Buyer, ts_event: 397060998713 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.06491765, aggressor: Buyer, ts_event: 406213679341 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.12519947, aggressor: Buyer, ts_event: 408886805844 }))"#,
            r#"Some(Trade(TradeTick { symbol: "XBTUSD", price: 60000.1, size: 0.32332866, aggressor: Buyer, ts_event: 408913781393 }))"#,
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
        let mut a =
            GeneratedSource::new(scalars.clone(), 42, SESSION_START_TS, &fp, Some(spike(big)));
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
            let TickEvent::Trade(trade) = src.next_tick().expect("unbounded generated source")
            else {
                unreachable!("generated source emits trades")
            };
            if (trade.price / scalars.modal_tick).fract() != Decimal::ZERO {
                off_grid_prices += 1;
            }
            if trade.aggressor == AggressorSide::NoAggressor {
                neutral_aggressors += 1;
            }
            timestamps.push(trade.ts_event as f64 / 1_000_000_000.0);
            prices.push(decimal_to_f64(trade.price));
            sizes.push(trade.size);
        }

        let mut durations = Vec::with_capacity(draw - 1);
        let mut returns = Vec::with_capacity(draw - 1);
        let mut zero_changes = 0;
        for i in 1..draw {
            durations.push(timestamps[i] - timestamps[i - 1]);
            returns.push((prices[i] / prices[i - 1]).ln());
            if prices[i] == prices[i - 1] {
                zero_changes += 1;
            }
        }
        let abs_returns: Vec<f64> = returns.iter().map(|r| r.abs()).collect();
        let sizes_f64: Vec<f64> = sizes.iter().copied().map(decimal_to_f64).collect();
        let round_lots = sizes.iter().filter(|size| is_round_lot(**size)).count();

        Measured {
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
            let TickEvent::Trade(trade) = src.next_tick().expect("unbounded generated source")
            else {
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
}
