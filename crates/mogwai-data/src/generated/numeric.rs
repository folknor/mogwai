// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Small numeric helpers shared across the generator: fingerprint-range
//! membership checks, the saturating f64-to-Decimal conversion the walk uses
//! for prices/sizes and round-lot snapping.

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

/// Snap a size draw to the venue's round-lot grid, where the grid is DECADE
/// RELATIVE to the derived median rather than absolute.
///
/// The old rule snapped to whole units above 1.0 and to 0.1 below it. At the
/// raw-fill size scale (a 0.0027 BTC median) that turned every round-lot draw
/// into exactly 0.1 - 37x the median - on the ~24% of trades `size_round_frac`
/// selects. `lot = 10^floor(log10(median))` reproduces the old sub-unit
/// behaviour at a 0.1 median and tracks the median anywhere else. `is_round_lot`
/// in the test module is the same predicate, deliberately, so the generator and
/// the gate cannot disagree about what a round lot is.
///
/// The floor at one lot keeps a snapped size strictly positive: `round` can
/// legitimately return zero for a draw well below half a lot.
pub(super) fn round_lot_size(base: f64, median: f64) -> Decimal {
    let lot = 10.0_f64.powf(median.log10().floor());
    decimal_from_f64((lot * (base / lot).round()).max(lot))
}
