use mogwai_protocol::{AggressorSide, TradeTick};
use rand::{Rng, SeedableRng, rngs::StdRng};
use rand_distr::{ChiSquared, Distribution, LogNormal, Normal, Weibull};
use rust_decimal::{Decimal, prelude::FromPrimitive, prelude::ToPrimitive};
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
        // Fail loud on a non-positive session share. arrival_mult divides the
        // drawn duration by arr_hour[h] * arr_dow[d], which are these shares
        // scaled by 24 and 7; a zero or negative entry would make the divisor
        // zero or negative, yielding infinite/negative durations that saturate
        // to u64::MAX. Rather than a silent clamp masking a broken fingerprint,
        // panic here - consistent with the parse-on-malformed contract above -
        // so a bad fingerprint is caught at load, not in the hot path.
        for (h, share) in fingerprint
            .session_profile
            .intensity_hour
            .iter()
            .enumerate()
        {
            assert!(
                *share > 0.0,
                "fingerprint session_profile.intensity_hour[{h}] must be strictly positive, got {share}"
            );
        }
        for (d, share) in fingerprint.session_profile.dow_weight.iter().enumerate() {
            assert!(
                *share > 0.0,
                "fingerprint session_profile.dow_weight[{d}] must be strictly positive, got {share}"
            );
        }
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

#[derive(Debug, Clone, Deserialize)]
pub struct SessionProfile {
    pub intensity_hour: [f64; 24],
    pub vol_hour: [f64; 24],
    pub dow_weight: [f64; 7],
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

// Precomputed session multipliers. Built once from the fingerprint's
// SessionProfile so the per-tick hot path is two array indexes and a multiply,
// not a re-normalization. The arrival multiplier centers each share on 1.0 by
// dividing out the uniform share (24 hours, 7 days); the vol multiplier is the
// fingerprint's per-mean ratio used as-is.
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
        let (hour, _dow) = utc_hour_dow(clock_ns);
        self.vol_hour[hour]
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ScalarRanges {
    pub modal_tick: MinMedianMax,
    pub price_decimals: MinMedianMax,
    pub mean_duration_s: MinMedianMax,
    pub size_round_frac: MinMedianMax,
}

#[derive(Debug, Clone)]
pub struct GeneratorScalars {
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
            start_price: Decimal::from(60_000),
            typical_size: Decimal::new(1, 1),
            vol_scalar: 0.000_000_05,
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
            start_price: Decimal::from(60_000),
            typical_size: Decimal::new(1, 1),
            vol_scalar: 0.000_000_05,
        }
    }

    pub fn validate(&self, fp: &Fingerprint) -> Result<(), ScalarError> {
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
        validate_f64(
            "mean_duration_s",
            self.mean_duration_s,
            &fp.scalar_ranges.mean_duration_s,
        )?;
        validate_f64(
            "size_round_frac",
            self.size_round_frac,
            &fp.scalar_ranges.size_round_frac,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalarError {
    pub field: &'static str,
}

pub struct GeneratedSource {
    scalars: GeneratorScalars,
    rng: StdRng,
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
}

impl GeneratedSource {
    #[must_use]
    pub fn new(scalars: GeneratorScalars, seed: u64, start_ts: u64, fp: &Fingerprint) -> Self {
        scalars
            .validate(fp)
            .expect("generated source scalars are inside fingerprint ranges");
        let mean_duration_s = scalars.mean_duration_s;
        let alpha = ACD_PERSISTENCE * ACD_FEEDBACK_SHARE;
        let beta = ACD_PERSISTENCE - alpha;
        let omega = mean_duration_s * (1.0 - ACD_PERSISTENCE);
        let vol = GarchVol::new(decimal_to_f64(scalars.start_price), scalars.vol_scalar);
        let size_median = decimal_to_f64(scalars.typical_size).max(f64::MIN_POSITIVE);
        let size_dist = LogNormal::new(size_median.ln(), SIZE_LOG_SIGMA).expect("valid lognormal");
        Self {
            tick_f64: decimal_to_f64(scalars.modal_tick),
            scalars,
            rng: StdRng::seed_from_u64(seed),
            clock_ns: start_ts,
            acd: AcdClock {
                omega,
                alpha,
                beta,
                psi: mean_duration_s,
                prev_duration_s: mean_duration_s,
                eps_mean: weibull_mean(ACD_WEIBULL_SHAPE),
            },
            vol,
            session: SessionModulator::new(&fp.session_profile),
            bounce: BounceState {
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
        }
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
        let duration_s = (duration_s / arr_mult).max(0.000_000_001);
        (duration_s * 1_000_000_000.0).round().max(1.0) as u64
    }

    fn next_latent_mid(&mut self) -> f64 {
        let normal = self.normal.sample(&mut self.rng);
        let chi = self.chi_squared.sample(&mut self.rng);
        let student_t = normal / (chi / STUDENT_T_DF).sqrt();
        self.vol.sigma2 = self.vol.a0
            + self.vol.a1 * self.vol.prev_return.powi(2)
            + self.vol.b1 * self.vol.sigma2;
        self.vol.sigma2 = self.vol.sigma2.min(GARCH_SIGMA_CAP.powi(2));
        let base_return =
            (self.vol.sigma2.sqrt() * student_t).clamp(-MAX_ABS_RETURN, MAX_ABS_RETURN);
        // GARCH feedback sees the un-modulated return so volatility clustering
        // is unchanged; the session envelope scales the realized RMS on top,
        // then the hard clamp still bounds the mid update.
        self.vol.prev_return = base_return;
        let vol_mult = self.session.vol_mult(self.clock_ns);
        let return_n = (base_return * vol_mult).clamp(-MAX_ABS_RETURN, MAX_ABS_RETURN);
        self.vol.mid = (self.vol.mid * return_n.exp())
            .max(self.tick_f64)
            .min(1_000_000_000.0);
        self.vol.mid
    }

    fn next_price(&mut self, mid: f64) -> (Decimal, AggressorSide) {
        let side = self.bounce.next_side(&mut self.rng);
        self.bounce.next_drift(&mut self.rng);
        let mid_ticks = mid / self.tick_f64 + self.bounce.drift_ticks as f64;
        let price_ticks = match side {
            AggressorSide::Buyer => (mid_ticks + self.bounce.half_spread_ticks).ceil(),
            AggressorSide::Seller => (mid_ticks - self.bounce.half_spread_ticks).floor(),
            AggressorSide::NoAggressor => unreachable!("bounce only emits buyer or seller"),
        };
        let price = decimal_from_f64(price_ticks * self.tick_f64);
        (price.round_dp(self.scalars.price_decimals), side)
    }

    fn next_size(&mut self) -> Decimal {
        let base = self.size_dist.sample(&mut self.rng).max(f64::MIN_POSITIVE);
        let size = if self.rng.gen_bool(self.scalars.size_round_frac) {
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
        self.clock_ns = self.clock_ns.saturating_add(dt_ns);
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

struct AcdClock {
    omega: f64,
    alpha: f64,
    beta: f64,
    psi: f64,
    prev_duration_s: f64,
    eps_mean: f64,
}

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

struct BounceState {
    prev_side: AggressorSide,
    high_regime: bool,
    drift_ticks: i64,
    drift_dir: i64,
    drift_hot: bool,
    half_spread_ticks: f64,
}

impl BounceState {
    fn next_drift(&mut self, rng: &mut StdRng) {
        if !self.high_regime {
            return;
        }
        if rng.gen_bool(DRIFT_DIR_FLIP_PROB) {
            self.drift_dir *= -1;
        }
        let p_drift = if self.drift_hot {
            HOT_DRIFT_PROB
        } else {
            HIGH_REGIME_DRIFT_PROB
        };
        if rng.gen_bool(p_drift) {
            self.drift_ticks += self.drift_dir;
            self.drift_hot = true;
        } else {
            self.drift_hot = false;
        }
    }

    fn next_side(&mut self, rng: &mut StdRng) -> AggressorSide {
        if self.high_regime {
            if rng.gen_bool(BOUNCE_HIGH_TO_LOW_PROB) {
                self.high_regime = false;
            }
        } else if rng.gen_bool(BOUNCE_LOW_TO_HIGH_PROB) {
            self.high_regime = true;
        }
        let p_flip = if self.high_regime {
            BOUNCE_HIGH_FLIP_PROB
        } else {
            BOUNCE_LOW_FLIP_PROB
        };
        if rng.gen_bool(p_flip) {
            self.prev_side = match self.prev_side {
                AggressorSide::Buyer => AggressorSide::Seller,
                AggressorSide::Seller | AggressorSide::NoAggressor => AggressorSide::Buyer,
            };
        }
        self.prev_side
    }
}

fn validate_f64(field: &'static str, value: f64, range: &MinMedianMax) -> Result<(), ScalarError> {
    if (range.min..=range.max).contains(&value) {
        Ok(())
    } else {
        Err(ScalarError { field })
    }
}

// Saturating conversions: no internal generator draw can panic here. The
// pinned size/price distributions keep draws far inside Decimal range in
// practice, but a sufficiently extreme heavy-tail sample (or a NaN) would
// make `Decimal::from_f64` return None, so we clamp to the nearest
// representable value instead of unwrapping. NaN maps to zero (no sign to
// preserve); +/-inf and out-of-range finite magnitudes saturate to
// Decimal::MAX / Decimal::MIN by sign.
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

fn decimal_to_f64(value: Decimal) -> f64 {
    value.to_f64().unwrap_or(0.0)
}

fn round_lot_size(base: f64) -> Decimal {
    if base >= 1.0 {
        decimal_from_f64(base.round().max(1.0))
    } else {
        decimal_from_f64((base * 10.0).round().max(1.0) / 10.0).round_dp(1)
    }
}

fn weibull_mean(shape: f64) -> f64 {
    gamma(1.0 + 1.0 / shape)
}

fn gamma(z: f64) -> f64 {
    const COEFFS: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        0.000_009_984_369_578_019_572,
        0.000_000_150_563_273_514_931_16,
    ];
    if z < 0.5 {
        std::f64::consts::PI / ((std::f64::consts::PI * z).sin() * gamma(1.0 - z))
    } else {
        let z = z - 1.0;
        let mut x = COEFFS[0];
        for (i, coeff) in COEFFS.iter().enumerate().skip(1) {
            x += coeff / (z + i as f64);
        }
        let t = z + 7.5;
        (2.0 * std::f64::consts::PI).sqrt() * t.powf(z + 0.5) * (-t).exp() * x
    }
}

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
    fn determinism() {
        let fp = Fingerprint::from_repo_json();
        let scalars = GeneratorScalars::xbtusd_anchor(&fp);
        let mut a = GeneratedSource::new(scalars.clone(), 42, 1_000, &fp);
        let mut b = GeneratedSource::new(scalars.clone(), 42, 1_000, &fp);
        let mut c = GeneratedSource::new(scalars, 43, 1_000, &fp);
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
        let mut src = GeneratedSource::new(GeneratorScalars::xbtusd_anchor(&fp), 42, 0, &fp);
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
        let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp);
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
    fn realism() {
        let fp = Fingerprint::from_repo_json();
        let scalars = GeneratorScalars::xbtusd_anchor(&fp);
        let mut src = GeneratedSource::new(scalars.clone(), 42, 0, &fp);
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

    #[test]
    #[ignore]
    fn session_modulation_reproduces_curves() {
        assert_eq!(utc_hour_dow(0), (0, 4));
        assert_eq!(utc_hour_dow(1_700_000_000_000_000_000), (22, 2));
        assert_eq!(utc_hour_dow(SESSION_START_TS), (0, 1));

        let fp = Fingerprint::from_repo_json();
        let scalars = GeneratorScalars::xbtusd_anchor(&fp);
        let mut src = GeneratedSource::new(scalars, 42, SESSION_START_TS, &fp);
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
            (range.min..=range.max).contains(&value),
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
