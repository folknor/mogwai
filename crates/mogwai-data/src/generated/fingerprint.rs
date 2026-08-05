// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The committed fingerprint's config schema: the `Deserialize` types parsed
//! straight from `analysis/fingerprint.json`, plus the validation that keeps
//! caller-supplied [`GeneratorScalars`] and [`SessionProfile`] inputs. Hard
//! validation below is derived from the mechanism. Corpus ranges are retained
//! separately as diagnostics: what eight crypto pairs happened to do must not
//! decide whether a futures contract is allowed to exist.

use mogwai_protocol::decimal_to_f64;
use rust_decimal::Decimal;
use serde::Deserialize;

use super::consts::{
    GARCH_SIGMA_CAP, LATENT_SIZE_MIN_RATIO, SESSION_SHARE_SUM, SESSION_SUM_TOL,
    SIGMA_RAIL_HEADROOM_RATIO, SIZE_DECIMALS, SIZE_LOG_SIGMA, START_PRICE_USD, VOL_HOUR_SUM,
    VOL_SCALAR,
};
use super::numeric::decimal_from_f64;

#[derive(Debug, Clone, Deserialize)]
pub struct Fingerprint {
    pub cadence: Cadence,
    pub golden_targets: GoldenTargets,
    pub session_profile: SessionProfile,
    pub empirical_ranges: EmpiricalRanges,
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
    pub duration_dispersion_cv2: AnchorRange,
    pub dwell: DwellTargets,
    pub return_acf_lag1: AnchorRange,
    pub abs_return_acf: AbsReturnAcf,
    pub zero_change_frac: AnchorRange,
    pub return_acf_anchor: Vec<f64>,
    pub abs_return_acf_anchor: Vec<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Cadence {
    pub targets: CadenceTargets,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CadenceTargets {
    pub mean_event_duration_s: AnchorRange,
    pub children_mean: AnchorRange,
    pub children_single_frac: AnchorRange,
    pub levels_mean: AnchorRange,
    pub mean_trade_notional: AnchorRange,
    pub duration_dispersion_cv2: AnchorRange,
    pub duration_acf_lag1: AnchorRange,
    pub duration_acf_lag5: AnchorRange,
    pub per_second_counts: PerSecondCounts,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PerSecondCounts {
    pub mean: f64,
    pub median: u32,
    pub p95: u32,
    pub zero_frac: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DwellTargets {
    pub era_start_ts: u64,
    pub mean_s: AnchorRange,
    pub max_gap_s: AnchorRange,
    pub gap_p999_s: AnchorRange,
    pub empty_hour_frac: AnchorRange,
    pub max_empty_hour_run_h: AnchorRange,
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
    /// truth for diagnostic membership and realism tests, so those two cannot
    /// drift on the inclusive-vs-exclusive convention.
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
    /// Calendar-free validation, kept as the bare name so every existing caller
    /// and every operator profile without a calendar behaves exactly as before.
    pub fn validate(&self) -> Result<(), SessionProfileError> {
        self.validate_for(false)
    }

    /// Validation conditional on whether a `SessionCalendar` owns closure.
    ///
    /// With a calendar the modulator normalizes the composite over open minutes,
    /// so any positive scale is equivalent and the sum contracts below are not
    /// merely unnecessary - they are harmful. A fitted profile's factors are
    /// relative effects conditional on being open and satisfy no particular sum;
    /// forcing one would distort them, and it would make Saturday's
    /// unidentifiable placeholder - zero exposure, so 0/0, so a declared 1.0 -
    /// push error into the six days that ARE identifiable.
    ///
    /// The bug the sum guard exists to catch also disappears under that
    /// normalization rather than going unguarded: an all-ones profile no longer
    /// yields a 168x arrival multiplier, it yields exactly 1.0.
    pub fn validate_for(&self, calendar_owns_closure: bool) -> Result<(), SessionProfileError> {
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
        // compressing the validated `mean_event_duration_s` from seconds to
        // milliseconds; an un-normalized vol curve silently rescales overall
        // volatility even though `vol_scalar` validated. The on-grid and
        // positivity invariants still hold, so the golden stream would never
        // reveal either bug - only a sum check does.
        //
        // The bound is ONE-SIDED for the two distributions and two-sided for
        // vol: a sum ABOVE 1.0 is the compression pathology and is rejected,
        // but a sum BELOW 1.0 is a legitimately thin profile - and at the
        // degenerate extreme the all-but-silent one that `low_intensity_gap_ns`'
        // cap machinery exists to survive - so a low sum stays legal. That is a
        // NUMERICAL allowance, not a closure idiom: a calendar-free profile
        // whose author means "shut" has no way to say so, which is exactly why
        // closure belongs to `SessionCalendar`. `vol_hour` has no such use, so
        // both an
        // inflated and a deflated curve are misconfigurations and get a
        // symmetric band. The sentinel `index` of `usize::MAX` marks a
        // whole-array (sum) violation rather than a single bad element.
        //
        // ALL OF THE ABOVE IS CONDITIONAL ON THERE BEING NO CALENDAR. When one
        // owns closure the modulator divides the composite by its own
        // exposure-weighted mean, so scale carries no meaning to constrain and
        // the "no modulation" pathology this guard was built for cannot occur.
        if calendar_owns_closure {
            return Ok(());
        }
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
pub struct EmpiricalRanges {
    /// Human-readable identity of the corpus behind every range below. These
    /// bands are observations, not universal units or mechanism limits.
    pub corpus: String,
    pub modal_tick: MinMedianMax,
    pub price_decimals: MinMedianMax,
    pub mean_event_duration_s: MinMedianMax,
    pub children_mean: MinMedianMax,
    pub children_single_frac: MinMedianMax,
    pub levels_mean: MinMedianMax,
    pub mean_trade_notional: MinMedianMax,
    pub size_round_frac: MinMedianMax,
}

fn default_size_log_sigma() -> f64 {
    SIZE_LOG_SIGMA
}

#[derive(Debug, Clone, Deserialize)]
pub struct GeneratorScalars {
    #[serde(default)]
    pub symbol: String,
    pub modal_tick: Decimal,
    pub price_decimals: u32,
    pub mean_event_duration_s: f64,
    pub children_mean: f64,
    pub children_single_frac: f64,
    pub levels_mean: f64,
    pub size_round_frac: f64,
    pub start_price: Decimal,
    /// Median of the continuous lognormal before minimum-size flooring, grid
    /// quantization, or optional round-lot snapping, in this instrument's
    /// native size unit. Calling this merely `size_median` is a trap: many
    /// different latent medians collapse onto an observed one-contract median.
    pub latent_size_median: Decimal,
    /// Log-scale sigma of the latent size lognormal. The module-level
    /// `SIZE_LOG_SIGMA = 1.15` is this field's DEFAULT, not a refit: an
    /// instrument that omits it draws exactly the shared crypto shape,
    /// byte for byte, while a fitted instrument states its own tail (the
    /// generator successor spec 3.2 - July MNQ's p99 measured 8 contracts
    /// against 14 generated at the shared sigma).
    #[serde(default = "default_size_log_sigma")]
    pub size_log_sigma: f64,
    pub vol_scalar: f64,
    #[serde(default)]
    pub quoted_width: super::quote::QuotedWidth,
    #[serde(default)]
    pub top_sizes: super::quote::TopOfBookSizes,
    #[serde(default)]
    pub trade_displacement_ticks: super::quote::TradeDisplacement,
}

impl GeneratorScalars {
    #[must_use]
    pub fn from_fingerprint_medians(symbol: &str, fp: &Fingerprint) -> Self {
        Self {
            symbol: symbol.to_string(),
            modal_tick: decimal_from_f64(fp.empirical_ranges.modal_tick.median),
            price_decimals: fp.empirical_ranges.price_decimals.median.round() as u32,
            mean_event_duration_s: fp.cadence.targets.mean_event_duration_s.anchor,
            children_mean: fp.cadence.targets.children_mean.anchor,
            children_single_frac: fp.cadence.targets.children_single_frac.anchor,
            levels_mean: fp.cadence.targets.levels_mean.anchor,
            size_round_frac: fp.empirical_ranges.size_round_frac.median,
            start_price: Decimal::from(START_PRICE_USD),
            // Preserve the calibrated crypto stream while removing the proxy
            // from the public configuration schema. The old field was exactly
            // MEAN trade notional because it divided by exp(sigma^2 / 2), even
            // though its `typical_` name suggested a representative center.
            latent_size_median: decimal_from_f64(
                fp.cadence.targets.mean_trade_notional.anchor
                    / START_PRICE_USD as f64
                    / (SIZE_LOG_SIGMA.powi(2) / 2.0).exp(),
            ),
            size_log_sigma: SIZE_LOG_SIGMA,
            vol_scalar: VOL_SCALAR,
            quoted_width: super::quote::QuotedWidth::default(),
            top_sizes: super::quote::TopOfBookSizes::uncalibrated(Decimal::new(1, SIZE_DECIMALS)),
            trade_displacement_ticks: super::quote::TradeDisplacement::default(),
        }
    }

    #[must_use]
    pub fn xbtusd_anchor(fp: &Fingerprint) -> Self {
        Self {
            symbol: "XBTUSD".to_string(),
            modal_tick: Decimal::new(1, 1),
            price_decimals: 1,
            mean_event_duration_s: fp.cadence.targets.mean_event_duration_s.anchor,
            children_mean: fp.cadence.targets.children_mean.anchor,
            children_single_frac: fp.cadence.targets.children_single_frac.anchor,
            levels_mean: fp.cadence.targets.levels_mean.anchor,
            size_round_frac: fp.empirical_ranges.size_round_frac.median,
            start_price: Decimal::from(START_PRICE_USD),
            latent_size_median: decimal_from_f64(
                fp.cadence.targets.mean_trade_notional.anchor
                    / START_PRICE_USD as f64
                    / (SIZE_LOG_SIGMA.powi(2) / 2.0).exp(),
            ),
            size_log_sigma: SIZE_LOG_SIGMA,
            vol_scalar: VOL_SCALAR,
            quoted_width: super::quote::QuotedWidth::default(),
            top_sizes: super::quote::TopOfBookSizes::uncalibrated(Decimal::new(1, SIZE_DECIMALS)),
            trade_displacement_ticks: super::quote::TradeDisplacement::default(),
        }
    }

    pub fn validate(&self) -> Result<(), ScalarError> {
        // A `symbol` omitted from config serde-defaults to "". Every trade this
        // source emits would then carry an empty symbol, keying
        // `TickRuleAggressor` per-symbol state and any symbol-keyed consumer on
        // the same empty string and cross-contaminating instruments. Reject it.
        if self.symbol.is_empty() {
            return Err(ScalarError { field: "symbol" });
        }
        for (field, provenance) in [
            ("quoted_width", self.quoted_width.provenance()),
            ("top_sizes", &self.top_sizes.provenance),
            (
                "trade_displacement_ticks",
                self.trade_displacement_ticks.provenance(),
            ),
        ] {
            if matches!(
                provenance,
                super::quote::CalibrationProvenance::Fitted { corpus } if corpus.trim().is_empty()
            ) {
                return Err(ScalarError { field });
            }
        }
        if self.modal_tick <= Decimal::ZERO {
            return Err(ScalarError {
                field: "modal_tick",
            });
        }
        // `modal_tick` must be representable at `price_decimals`. `next_price`
        // snaps every quote with `round_dp(price_decimals)`; a tick carrying
        // more decimal places than that is silently coarsened to the
        // 10^-price_decimals grid, so the configured modal_tick stops being the
        // real tick (modal_tick 1e-7 with price_decimals 1 collapses every
        // price onto a 0.1 grid). The on-grid invariant still holds against the
        // COARSER grid, so no test or runtime check catches it - only this does.
        // The server enforces the same relationship on its instrument defs via
        // `on_increment`; this is the generator-layer twin.
        // rust_decimal carries at most 28 fractional digits. Zero is a valid
        // whole-number grid (YM/MYM are the motivating examples); values above
        // 28 claim precision the numeric representation cannot provide.
        if self.price_decimals > 28 || self.modal_tick.normalize().scale() > self.price_decimals {
            return Err(ScalarError {
                field: "modal_tick",
            });
        }
        if !strictly_positive_finite(self.mean_event_duration_s) {
            return Err(ScalarError {
                field: "mean_event_duration_s",
            });
        }
        if !strictly_positive_finite(self.children_mean) {
            return Err(ScalarError {
                field: "children_mean",
            });
        }
        if !self.children_single_frac.is_finite() {
            return Err(ScalarError {
                field: "children_single_frac",
            });
        }
        if !strictly_positive_finite(self.levels_mean) {
            return Err(ScalarError {
                field: "levels_mean",
            });
        }
        if self.children_mean <= 1.0 {
            return Err(ScalarError {
                field: "children_mean",
            });
        }
        if !(0.0..1.0).contains(&self.children_single_frac) {
            return Err(ScalarError {
                field: "children_single_frac",
            });
        }
        // Floor-branch feasibility (the generator successor spec, 3.1). An
        // instrument whose base quiet-state mean sits at or below one child
        // takes the floor-aware conditioning, whose active-state solve must
        // be expressible by the one-plus-geometric mixture: the solved
        // active single fraction has to sit in [1/active_mean, 1].
        // Evaluated at children_mult = 1; a named test proves feasibility
        // is monotone across the whole surge range [1, 100], so this
        // construction-time check covers every runtime state.
        if self.children_mean * super::consts::ARRIVAL_QUIET_CHILDREN_MULT <= 1.0 {
            let quiet_share = super::consts::ARRIVAL_QUIET_FRACTION;
            let quiet_eff = self.children_mean * super::consts::ARRIVAL_QUIET_CHILDREN_MULT;
            let quiet_mean = quiet_eff.max(1.0);
            let quiet_single = if quiet_eff <= 1.0 {
                1.0
            } else {
                self.children_single_frac.max(1.0 / quiet_eff)
            };
            let active_mean = (self.children_mean - quiet_share * quiet_mean) / (1.0 - quiet_share);
            let active_single =
                (self.children_single_frac - quiet_share * quiet_single) / (1.0 - quiet_share);
            if active_mean <= 1.0 || !(1.0 / active_mean..=1.0).contains(&active_single) {
                return Err(ScalarError {
                    field: "children_single_frac (floor-branch active solve infeasible)",
                });
            }
        }
        if !(1.0..=self.children_mean).contains(&self.levels_mean) {
            return Err(ScalarError {
                field: "levels_mean",
            });
        }
        if !self.size_round_frac.is_finite() || !(0.0..=1.0).contains(&self.size_round_frac) {
            return Err(ScalarError {
                field: "size_round_frac",
            });
        }
        // The successor spec 3.2 bound: wide enough for any plausible tail,
        // tight enough that a unit mix-up (a variance where a sigma belongs)
        // cannot slip through as configuration.
        if !self.size_log_sigma.is_finite() || !(0.1..=3.0).contains(&self.size_log_sigma) {
            return Err(ScalarError {
                field: "size_log_sigma",
            });
        }
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
        if self.latent_size_median <= Decimal::ZERO {
            return Err(ScalarError {
                field: "latent_size_median",
            });
        }
        if self.latent_size_median.round_dp(SIZE_DECIMALS) <= Decimal::ZERO {
            return Err(ScalarError {
                field: "latent_size_median",
            });
        }
        if !strictly_positive_finite(self.vol_scalar) {
            return Err(ScalarError {
                field: "vol_scalar",
            });
        }
        // Deliberately no upper coupling to `quoted_width`: the displayed BBO
        // is only the top level, and an aggressive parent may print beyond the
        // touch when it consumes a thin book. Width and displacement are
        // independently calibrated observables; requiring displacement to fit
        // inside half the width would collapse those seams back into one.
        if !self.trade_displacement_ticks.ticks().is_finite()
            || self.trade_displacement_ticks.ticks() < 0.0
        {
            return Err(ScalarError {
                field: "trade_displacement_ticks",
            });
        }
        // `vol_scalar` is the unconditional sigma, and `GarchVol` initializes
        // `sigma2` to its square. At or above the state rail the process is born
        // clipped and the knob's documented meaning stops holding on the very
        // first tick, so this much IS a mechanism bound.
        //
        // What is NOT enforced here is a HEADROOM ratio. The previous rule
        // demanded ten percent clearance under the rail, which reads as prudence
        // and behaves as a universal scale gate: it would deny a legitimately
        // higher-volatility instrument for no reason but that one corpus sat
        // lower - the `scalar_ranges` mistake in another dimension. Insufficient
        // headroom is diagnosed instead, and shipped presets are held to zero
        // diagnostics.
        if self.vol_scalar >= GARCH_SIGMA_CAP {
            return Err(ScalarError {
                field: "vol_scalar",
            });
        }
        Ok(())
    }

    /// Warns when the state rail sits close enough to `vol_scalar` to take part
    /// in ordinary operation rather than bounding pathology. The threshold is
    /// measured, not chosen: the clean process was observed reaching 57.2x its
    /// unconditional scale over sixteen million updates, so a rail below
    /// [`SIGMA_RAIL_HEADROOM_RATIO`] would clip the process's normal excursions.
    #[must_use]
    pub fn rail_diagnostics(&self) -> Vec<ScalarDiagnostic> {
        if GARCH_SIGMA_CAP / self.vol_scalar < SIGMA_RAIL_HEADROOM_RATIO {
            vec![ScalarDiagnostic {
                code: "sigma-rail-headroom-below-measured-excursion",
                field: "vol_scalar",
                corpus: "mechanism-derived".to_string(),
            }]
        } else {
            Vec::new()
        }
    }

    /// Grid-dependent mechanism validation belongs beside scalar validation,
    /// not in a crypto-fitted range. A latent center two orders of magnitude
    /// below the smallest tradable quantity is the numerical signature of the
    /// unit mismatch which used to turn MNQ into a constant-one-contract tape.
    pub fn validate_size_grid(&self, min_size: Decimal) -> Result<(), ScalarError> {
        if min_size <= Decimal::ZERO
            || decimal_to_f64(self.latent_size_median)
                < decimal_to_f64(min_size) * LATENT_SIZE_MIN_RATIO
        {
            return Err(ScalarError {
                field: "latent_size_median",
            });
        }
        Ok(())
    }

    /// Closed-form, dimensionless view of how the latent mid meets the price
    /// grid. `expected_updates_per_tick` is the random-walk order estimate
    /// `(tick_return / sigma)^2`, not a promise about printed prices: bounce,
    /// sweep children, tick phase, recentering, and explicit event repetition
    /// all remain downstream. This is the principled input for a future
    /// derivation of `zero_change_frac`; the old corpus constant is not.
    #[must_use]
    pub fn tick_traversal(&self) -> TickTraversal {
        self.tick_traversal_at(1.0)
            .expect("the neutral volatility multiplier is positive and finite")
    }

    /// The same traversal estimate under a composed session/regime volatility
    /// multiplier. Callers inspecting a particular hour or armed regime must
    /// use this form; the zero-argument helper is intentionally the neutral
    /// baseline rather than an implied all-hours claim.
    #[must_use]
    pub fn tick_traversal_at(&self, vol_multiplier: f64) -> Option<TickTraversal> {
        if !strictly_positive_finite(vol_multiplier) {
            return None;
        }
        let tick_return =
            (1.0 + decimal_to_f64(self.modal_tick) / decimal_to_f64(self.start_price)).ln();
        let effective_sigma = self.vol_scalar * vol_multiplier;
        let sigma_units = tick_return / effective_sigma;
        Some(TickTraversal {
            tick_return,
            effective_sigma,
            sigma_units,
            expected_updates_per_tick: sigma_units.powi(2),
        })
    }

    /// Grid-aware behavioral warnings. Unlike `validate_size_grid`, this does
    /// not call a thin but coherent contract invalid. The 1 percent tail rule
    /// warns that quantization will hide almost all size variation; the hard
    /// one-hundredth ratio check above is reserved for contradictory units.
    /// This deliberately models the final integral rounding and floor only.
    /// It does not fold in the optional `size_round_frac` lot-snap branch,
    /// whose decade-sized lots matter once the latent median reaches ten; the
    /// separately pinned `integral_lot` transitions keep that limitation
    /// visible rather than letting this probability masquerade as the whole
    /// output distribution.
    #[must_use]
    pub fn size_diagnostics(&self, min_size: Decimal, integral: bool) -> Vec<ScalarDiagnostic> {
        if !integral {
            return Vec::new();
        }
        let median = decimal_to_f64(self.latent_size_median);
        let threshold = decimal_to_f64(min_size) + 0.5;
        let z = (threshold / median).ln() / self.size_log_sigma;
        // Phi^-1(0.99). P(X >= threshold) < 0.01 exactly when z exceeds it.
        if z > 2.326_347_874_040_840_8 {
            vec![ScalarDiagnostic {
                code: "quantized-size-variation-below-one-percent",
                field: "latent_size_median",
                corpus: "mechanism-derived".to_string(),
            }]
        } else {
            Vec::new()
        }
    }

    /// Corpus comparisons are intentionally advisory. Operators may choose an
    /// instrument outside Kraken's observed envelope; shipped presets must
    /// accept every diagnostic explicitly in provenance, with stale
    /// acceptances rejected by the server's preset-validation test.
    #[must_use]
    pub fn empirical_diagnostics(
        &self,
        fp: &Fingerprint,
        multiplier: Decimal,
    ) -> Vec<ScalarDiagnostic> {
        let observed = &fp.empirical_ranges;
        let mut diagnostics = [
            (
                "modal_tick",
                decimal_to_f64(self.modal_tick),
                &observed.modal_tick,
            ),
            (
                "price_decimals",
                f64::from(self.price_decimals),
                &observed.price_decimals,
            ),
            (
                "mean_event_duration_s",
                self.mean_event_duration_s,
                &observed.mean_event_duration_s,
            ),
            ("children_mean", self.children_mean, &observed.children_mean),
            (
                "children_single_frac",
                self.children_single_frac,
                &observed.children_single_frac,
            ),
            ("levels_mean", self.levels_mean, &observed.levels_mean),
            (
                "size_round_frac",
                self.size_round_frac,
                &observed.size_round_frac,
            ),
        ]
        .into_iter()
        .filter(|(_, value, range)| !range.contains(*value))
        .map(|(field, _, _)| ScalarDiagnostic {
            code: "outside-empirical-corpus-range",
            field,
            corpus: observed.corpus.clone(),
        })
        .collect::<Vec<_>>();
        if !observed
            .mean_trade_notional
            .contains(self.mean_trade_notional(multiplier))
        {
            diagnostics.push(ScalarDiagnostic {
                code: "derived-mean-notional-outside-empirical-corpus-range",
                field: "latent_size_median",
                corpus: observed.corpus.clone(),
            });
        }
        diagnostics
    }

    /// Arithmetic mean notional implied by the latent lognormal at the
    /// configured reference price. Notional remains useful for comparing a
    /// preset with the corpus, but it is derived reporting data now, never the
    /// sampler input. The exp term is explicit because this exact correction
    /// was hidden behind the old and misleading `typical_notional` field.
    #[must_use]
    pub fn mean_trade_notional(&self, multiplier: Decimal) -> f64 {
        decimal_to_f64(self.latent_size_median)
            * (self.size_log_sigma.powi(2) / 2.0).exp()
            * decimal_to_f64(self.start_price)
            * decimal_to_f64(multiplier)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalarDiagnostic {
    pub code: &'static str,
    pub field: &'static str,
    pub corpus: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TickTraversal {
    pub tick_return: f64,
    pub effective_sigma: f64,
    pub sigma_units: f64,
    pub expected_updates_per_tick: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScalarError {
    pub field: &'static str,
}

/// Why a [`super::GeneratedSource`] construction can fail: either the scalar
/// config violates a mechanism constraint or the session profile is invalid.
/// Returned by the fallible `GeneratedSource::try_new` /
/// `GeneratedSource::try_new_with_session_profile` so a caller holding config
/// that has not been pre-validated can surface the error instead of tripping
/// the panic inside the infallible `GeneratedSource::new` family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratedSourceError {
    Scalar(ScalarError),
    Session(SessionProfileError),
}
