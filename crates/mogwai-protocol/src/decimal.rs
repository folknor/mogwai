// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use rust_decimal::Decimal;

/// Saturating `Decimal` -> `f64`. `Decimal`'s max magnitude (~7.9e28) sits
/// nowhere near `f64::MAX` (~1.8e308), so `to_f64()` cannot actually fail for
/// any value the type can hold - the `unwrap_or(0.0)` fallback is defensive
/// completeness, not a live safety net today. It is kept anyway because
/// `0.0` is the worst possible sentinel for a price or quantity on the hot
/// fill/balance path (a huge magnitude would silently read as zero rather
/// than surface as an error), so if `Decimal`'s range or `f64`'s ever changed
/// underneath this, this is the one place that assumption needs revisiting.
/// The data crate carries a private reader with this exact contract; the
/// adapter's `convert.rs` already calls this helper directly rather than the
/// panicking `.to_f64().expect(...)` a pathological wire `Decimal` would
/// otherwise take the runtime down with.
#[must_use]
pub fn decimal_to_f64(d: Decimal) -> f64 {
    use rust_decimal::prelude::ToPrimitive;
    d.to_f64().unwrap_or(0.0)
}

/// Saturating `f64` -> `Decimal`: clamps to `Decimal::MAX` / `Decimal::MIN` for
/// out-of-range finite inputs and maps any non-finite input (NaN, +/-inf) to
/// `Decimal::ZERO`. Mirrors the data crate's private writer so the two can be
/// unified, and gives the adapter a total conversion in place of a panicking one.
#[must_use]
pub fn decimal_from_f64(x: f64) -> Decimal {
    use rust_decimal::prelude::FromPrimitive;
    if !x.is_finite() {
        return Decimal::ZERO;
    }
    Decimal::from_f64(x).unwrap_or(if x > 0.0 { Decimal::MAX } else { Decimal::MIN })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_f64_round_trips_and_saturates() {
        // A representable value round-trips through both helpers.
        let d = Decimal::new(12_345, 2);
        assert!((decimal_to_f64(d) - 123.45).abs() < 1e-9);
        assert_eq!(decimal_from_f64(123.45), d);

        // Non-finite inputs collapse to zero rather than panicking.
        assert_eq!(decimal_from_f64(f64::NAN), Decimal::ZERO);
        assert_eq!(decimal_from_f64(f64::INFINITY), Decimal::ZERO);
        assert_eq!(decimal_from_f64(f64::NEG_INFINITY), Decimal::ZERO);

        // Magnitudes past Decimal's range saturate to the signed bound.
        assert_eq!(decimal_from_f64(1e40), Decimal::MAX);
        assert_eq!(decimal_from_f64(-1e40), Decimal::MIN);
    }
}
