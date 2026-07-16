// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The committed fingerprint's config schema: the `Deserialize` types parsed
//! straight from `analysis/fingerprint.json`, plus the validation that keeps
//! caller-supplied [`GeneratorScalars`] and [`SessionProfile`] inputs inside
//! the bands the generator's calibration assumes.

use mogwai_protocol::decimal_to_f64;
use rust_decimal::Decimal;
use serde::Deserialize;

use super::consts::{
    GARCH_SIGMA_CAP, SESSION_SHARE_SUM, SESSION_SUM_TOL, START_PRICE_USD, TYPICAL_SIZE_MANTISSA,
    TYPICAL_SIZE_SCALE, VOL_HOUR_SUM, VOL_SCALAR,
};
use super::numeric::{decimal_from_f64, validate_f64};

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

/// Why a [`super::GeneratedSource`] construction can fail: either the scalar
/// config or the session profile is outside the fingerprint's validated
/// ranges. Returned by the fallible `GeneratedSource::try_new` /
/// `GeneratedSource::try_new_with_session_profile` so a caller holding config
/// that has not been pre-validated can surface the error instead of tripping
/// the panic inside the infallible `GeneratedSource::new` family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedSourceError {
    Scalar(ScalarError),
    Session(SessionProfileError),
}
