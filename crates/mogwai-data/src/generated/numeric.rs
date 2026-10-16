// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Small numeric helpers shared across the generator: the saturating
//! f64-to-Decimal conversion, and the two conversions from the walk's float
//! coordinates onto a river's integer record scale - [`PriceGrid`] for tick
//! counts and [`SizeQuantizer`] for size draws.

use rust_decimal::{Decimal, prelude::FromPrimitive};

// Saturating f64 -> Decimal conversion: no internal generator draw can panic
// here. NaN maps to zero (no sign to preserve); +/-inf and out-of-range finite
// magnitudes saturate to Decimal::MAX / Decimal::MIN by sign.
//
// Deliberately not unified with `mogwai_protocol::decimal_from_f64`, which
// zeroes +/-inf instead of saturating (it has a test pinning that behaviour).
// The reverse direction (`decimal_to_f64`) is byte-identical in both crates and
// is imported from the protocol helper instead of redefined here.
pub(super) fn decimal_from_f64(value: f64) -> Decimal {
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

/// The ceiling on every quantity mantissa a generated record carries:
/// configured top-of-book sizes, the minimum size, every size draw and every
/// discrete-book quantity. Build refuses configured quantities above it, and
/// draws saturate to it, so no record quantity exceeds it.
///
/// It bounds one record, not a sum: a reader accumulating quantities over an
/// unbounded number of records does so in `u128` or checked arithmetic. At
/// 10^18 a `u128` sum holds more than 10^20 records at the ceiling.
pub const SIZE_UNITS_CEILING: u64 = 1_000_000_000_000_000_000;

/// 2^53: every non-negative integer below it is exactly a float, and the cast
/// to `i64` of an integral float below it is exact. Compared as a float
/// literal rather than derived from `i64::MAX as f64`, which rounds up to
/// 2^63 and would admit a value the saturating cast silently clamps.
const EXACT_INT_LIMIT: f64 = 9_007_199_254_740_992.0;

/// A river's tick as a mantissa at its price scale, and the checked
/// conversion of a tick count onto it.
///
/// The walk prices on an integer tick grid held as `f64`; the record is the
/// tick count times the tick's own mantissa, pure integer arithmetic, exact
/// for any tick the scale can represent (0.01, 0.25, 5). The conversion is the
/// real guard on the record's range: the latent mid's ceiling proves nothing
/// about an emitted level once width, displacement, drift and a sweep's
/// traversal are added, so every emitted price passes through here, and a
/// coordinate that is not a positive integer inside the exact range, or whose
/// mantissa overflows, is refused rather than clamped.
#[derive(Debug, Clone, Copy)]
pub(super) struct PriceGrid {
    pub(super) tick_mantissa: i64,
}

impl PriceGrid {
    pub(super) fn mantissa(self, ticks: f64) -> Option<i64> {
        // A range test is false for NaN, so the guard refuses it with the rest.
        if !((1.0..EXACT_INT_LIMIT).contains(&ticks) && ticks.fract() == 0.0) {
            return None;
        }
        (ticks as i64).checked_mul(self.tick_mantissa)
    }

    pub(super) fn mantissa_of_ticks(self, ticks: i64) -> Option<i64> {
        if ticks < 1 {
            return None;
        }
        ticks.checked_mul(self.tick_mantissa)
    }
}

/// A river's size draws onto its integer quantity scale.
///
/// Built once per source from the size grid, the scale and the latent median,
/// so a draw costs a multiply, a round and a compare.
#[derive(Debug, Clone, Copy)]
pub(super) struct SizeQuantizer {
    /// `10^size_dp`, the factor from a size to its mantissa.
    unit_scale: f64,
    integral: bool,
    min_units: u64,
    /// The round lot as a size, never smaller than one unit.
    lot_value: f64,
    /// The round lot in units: at least one, saturated at the ceiling.
    lot_units: u64,
}

impl SizeQuantizer {
    /// `lot_exponent` is `floor(log10(median))`, the decade the round lot
    /// tracks. An integral grid's lot is at least one whole contract, so its
    /// exponent floors at zero.
    pub(super) fn new(size_dp: u32, integral: bool, min_units: u64, lot_exponent: i32) -> Self {
        let lot_exponent = if integral {
            lot_exponent.max(0)
        } else {
            lot_exponent
        };
        // The lot in units, built without constructing a power that cannot
        // be represented: below one unit the lot is one unit, and a lot above
        // the ceiling is the ceiling.
        let unit_exponent = lot_exponent.saturating_add(size_dp as i32);
        let (lot_value, lot_units) = if unit_exponent < 0 {
            (10_f64.powi(-(size_dp as i32)), 1)
        } else {
            let units = u32::try_from(unit_exponent)
                .ok()
                .and_then(|exponent| 10_u64.checked_pow(exponent))
                .map_or(SIZE_UNITS_CEILING, |units| units.min(SIZE_UNITS_CEILING));
            (10_f64.powi(lot_exponent), units)
        };
        Self {
            unit_scale: 10_f64.powi(size_dp as i32),
            integral,
            min_units,
            lot_value,
            lot_units,
        }
    }

    /// One size draw onto the grid.
    ///
    /// An ordinary spot draw rounds half to even at the scale, as the decimal
    /// rounding it replaced did; an integral draw rounds half away from zero,
    /// which is what the integral grid has always done (truncation biases every
    /// draw down and banker's rounding thins the odd sizes on a 0.5-heavy
    /// grid). A snapped draw takes the lot count rounded half away from zero -
    /// the float `round` the snap has always used - and on a fractional grid
    /// floors at one lot, so a snapped draw well below half a lot is never
    /// zero. A non-finite or over-range intermediate saturates to the ceiling
    /// before the minimum is applied, and the ceiling is above the minimum by
    /// construction, so a numerical failure can never land below it.
    pub(super) fn quantize(self, base: f64, lot_snap: bool) -> u64 {
        let ceiling = SIZE_UNITS_CEILING as f64;
        let units = if lot_snap {
            let lots = base / self.lot_value;
            if lots.is_finite() {
                let lots = lots.round();
                let lots = if self.integral { lots } else { lots.max(1.0) };
                if lots >= ceiling {
                    SIZE_UNITS_CEILING
                } else {
                    (lots as u64)
                        .checked_mul(self.lot_units)
                        .map_or(SIZE_UNITS_CEILING, |units| units.min(SIZE_UNITS_CEILING))
                }
            } else {
                SIZE_UNITS_CEILING
            }
        } else {
            let scaled = base * self.unit_scale;
            if !scaled.is_finite() || scaled >= ceiling {
                SIZE_UNITS_CEILING
            } else if self.integral {
                scaled.round() as u64
            } else {
                scaled.round_ties_even() as u64
            }
        };
        units.max(self.min_units)
    }
}

/// The round-lot decade a size snaps to: `floor(log10(median))`. Decade-
/// relative to the median rather than absolute, because at the raw-fill size
/// scale (a 0.0027 BTC median) an absolute sub-unit lot turned every snapped
/// draw into 37 times the median. `is_round_lot` in the test module is the
/// same predicate, so the generator and the gate cannot disagree about what a
/// round lot is.
pub(super) fn lot_exponent(median: f64) -> i32 {
    let exponent = median.log10().floor();
    if exponent.is_finite() {
        exponent.clamp(f64::from(i32::MIN / 2), f64::from(i32::MAX / 2)) as i32
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_price_grid_refuses_what_it_cannot_carry_exactly() {
        let grid = PriceGrid { tick_mantissa: 25 };
        assert_eq!(grid.mantissa(4.0), Some(100));
        for bad in [
            0.0,
            -1.0,
            0.5,
            f64::NAN,
            f64::INFINITY,
            EXACT_INT_LIMIT,
            i64::MAX as f64,
        ] {
            assert_eq!(grid.mantissa(bad), None, "{bad}");
        }
        let wide = PriceGrid {
            tick_mantissa: 1_000_000_000_000,
        };
        assert_eq!(
            wide.mantissa(EXACT_INT_LIMIT - 1.0),
            None,
            "mantissa overflow"
        );
        assert_eq!(grid.mantissa_of_ticks(0), None);
        assert_eq!(grid.mantissa_of_ticks(i64::MAX), None);
    }

    #[test]
    fn spot_draws_round_half_even_and_snaps_floor_at_one_lot() {
        // 8 dp, a 0.0027 median: the lot is 0.001, 100_000 units.
        let quantizer = SizeQuantizer::new(8, false, 1, lot_exponent(0.0027));
        assert_eq!(quantizer.quantize(0.002_7, false), 270_000);
        assert_eq!(
            quantizer.quantize(0.000_000_005, false),
            1,
            "the minimum floors it"
        );
        assert_eq!(quantizer.quantize(0.002_7, true), 300_000);
        assert_eq!(
            quantizer.quantize(0.000_01, true),
            100_000,
            "never below one lot"
        );
        assert_eq!(quantizer.quantize(f64::INFINITY, false), SIZE_UNITS_CEILING);
        assert_eq!(quantizer.quantize(f64::MAX, true), SIZE_UNITS_CEILING);
        assert_eq!(quantizer.quantize(1e300, false), SIZE_UNITS_CEILING);
    }

    #[test]
    fn integral_draws_round_half_away_and_floor_at_the_minimum() {
        let quantizer = SizeQuantizer::new(0, true, 1, lot_exponent(40.0));
        assert_eq!(quantizer.quantize(2.5, false), 3);
        assert_eq!(quantizer.quantize(0.2, false), 1);
        assert_eq!(quantizer.quantize(44.0, true), 40);
        assert_eq!(
            quantizer.quantize(3.0, true),
            1,
            "a zero-lot snap takes the minimum"
        );
        // A sub-contract median still snaps to whole contracts.
        let small = SizeQuantizer::new(0, true, 1, lot_exponent(0.3));
        assert_eq!(small.quantize(2.4, true), 2);
    }

    #[test]
    fn a_lot_below_one_unit_or_above_the_ceiling_is_clamped_not_constructed() {
        let tiny = SizeQuantizer::new(8, false, 1, -12);
        assert_eq!(tiny.lot_units, 1);
        assert_eq!(tiny.quantize(0.000_000_034, true), 3);
        let huge = SizeQuantizer::new(18, false, 1, 30);
        assert_eq!(huge.lot_units, SIZE_UNITS_CEILING);
        assert_eq!(huge.quantize(1e31, true), SIZE_UNITS_CEILING);
    }
}
