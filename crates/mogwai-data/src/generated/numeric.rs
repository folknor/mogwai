//! Small numeric helpers shared across the generator: fingerprint-range
//! membership checks, the saturating f64-to-Decimal conversion the walk uses
//! for prices/sizes, round-lot snapping, and the one derived constant
//! (`WEIBULL_MEAN_SHAPE_060`) the ACD clock needs but `rand_distr` does not
//! expose.

use rust_decimal::{Decimal, prelude::FromPrimitive};

use super::fingerprint::{MinMedianMax, ScalarError};

pub(super) fn validate_f64(
    field: &'static str,
    value: f64,
    range: &MinMedianMax,
) -> Result<(), ScalarError> {
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

pub(super) fn round_lot_size(base: f64) -> Decimal {
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
pub(super) const WEIBULL_MEAN_SHAPE_060: f64 = 1.504_575_488_251_555_6;
