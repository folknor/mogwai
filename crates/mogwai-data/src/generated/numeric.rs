// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Small numeric helpers shared across the generator: the saturating
//! f64-to-Decimal conversion the walk uses for prices/sizes and round-lot
//! snapping.

use rust_decimal::{Decimal, prelude::FromPrimitive};

// Saturating f64 -> Decimal conversion: no internal generator draw can panic
// here. The pinned size/price distributions keep draws far inside Decimal
// range in practice, but a sufficiently extreme heavy-tail sample (or a NaN)
// would make `Decimal::from_f64` return None, so we clamp to the nearest
// representable value instead of unwrapping. NaN maps to zero (no sign to
// preserve); +/-inf and out-of-range finite magnitudes saturate to
// Decimal::MAX / Decimal::MIN by sign.
//
// Deliberately not unified with `mogwai_protocol::decimal_from_f64`, which
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

/// `decimal_from_f64(value).round_dp(dp)`, byte for byte, without the
/// conversion's bit-by-bit decimal expansion wherever the answer is already
/// decided in binary.
///
/// `from_f64` keeps at most 16 significant digits, so the decimal it
/// produces sits within a relative 1e-13 of `value` even where the 28-digit
/// scale cap truncates it harder (the `y >= 1` floor bounds that case), and
/// `value * 10^dp` is within a relative 1.2e-16 of its exact product. When `y
/// = value * 10^dp` sits further than a relative 1e-11 from both an integer
/// and a half-integer, the exact decimal times `10^dp` lies strictly inside
/// the same open interval: it is not an integer, so the decimal carries more
/// than `dp` places and `round_dp` rescales to exactly `dp`; it is no tie, so
/// banker's rounding goes to the nearer neighbour, which is `y`'s. Everything
/// else - near ties, near-integers whose trailing zeros `from_f64` strips,
/// and values outside the range where `y` is an exact integer carrier - takes
/// the slow path unchanged. `fast_round_dp_matches_the_decimal_conversion`
/// pins the equivalence differentially.
pub(super) fn decimal_from_f64_round_dp(value: f64, dp: u32) -> Decimal {
    fast_round_dp(value, dp).unwrap_or_else(|| decimal_from_f64(value).round_dp(dp))
}

/// The decided-in-binary half of [`decimal_from_f64_round_dp`]; `None` sends
/// the value down the exact conversion.
fn fast_round_dp(value: f64, dp: u32) -> Option<Decimal> {
    const POW10: [f64; 16] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15,
    ];
    let y = value * POW10.get(dp as usize)?;
    if !(1.0..1e15).contains(&y) {
        return None;
    }
    let floor = y.floor();
    let frac = y - floor;
    let guard = y * 1e-11;
    if frac <= guard || frac >= 1.0 - guard || (frac - 0.5).abs() <= guard {
        return None;
    }
    let mantissa = if frac > 0.5 { floor + 1.0 } else { floor };
    Some(Decimal::new(mantissa as i64, dp))
}

/// A direct-mapped memo of `decimal_from_f64(ticks * tick).round_dp(decimals)`
/// for one source's fixed tick and price precision.
///
/// The walk prices on an integer tick grid that clusters around the mid, so
/// the same few hundred levels are converted over and over; the conversion is
/// a pure function of the tick count, which makes a hit byte-identical to a
/// miss by construction. Keyed on the f64's bits, so a lookup can only hit
/// the exact input that filled it; the empty sentinel is a NaN bit pattern,
/// which no finite input can share, and non-finite inputs bypass the memo.
#[derive(Clone)]
pub(super) struct PriceMemo {
    tick: f64,
    decimals: u32,
    keys: [u64; PRICE_MEMO_SLOTS],
    values: [Decimal; PRICE_MEMO_SLOTS],
}

const PRICE_MEMO_SLOTS: usize = 32;
const PRICE_MEMO_EMPTY: u64 = u64::MAX;

impl PriceMemo {
    pub(super) fn new(tick: f64, decimals: u32) -> Self {
        Self {
            tick,
            decimals,
            keys: [PRICE_MEMO_EMPTY; PRICE_MEMO_SLOTS],
            values: [Decimal::ZERO; PRICE_MEMO_SLOTS],
        }
    }

    pub(super) fn price(&mut self, ticks: f64) -> Decimal {
        if !ticks.is_finite() {
            return self.compute(ticks);
        }
        let key = ticks.to_bits();
        let slot = (ticks as i64 as usize) % PRICE_MEMO_SLOTS;
        if self.keys[slot] == key {
            return self.values[slot];
        }
        let value = self.compute(ticks);
        self.keys[slot] = key;
        self.values[slot] = value;
        value
    }

    fn compute(&self, ticks: f64) -> Decimal {
        decimal_from_f64(ticks * self.tick).round_dp(self.decimals)
    }
}

/// The round-lot scale a spot size snaps to: decade-relative to the derived
/// median rather than absolute. Computed once per source, since the median
/// is fixed for its life.
///
/// The old rule snapped to whole units above 1.0 and to 0.1 below it. At the
/// raw-fill size scale (a 0.0027 BTC median) that turned every round-lot draw
/// into exactly 0.1 - 37x the median - on the ~24% of trades `size_round_frac`
/// selects. `lot = 10^floor(log10(median))` reproduces the old sub-unit
/// behaviour at a 0.1 median and tracks the median anywhere else. `is_round_lot`
/// in the test module is the same predicate, deliberately, so the generator and
/// the gate cannot disagree about what a round lot is.
pub(super) fn spot_round_lot(median: f64) -> f64 {
    10.0_f64.powf(median.log10().floor())
}

/// Snap a size draw to the round-lot grid `lot` (see [`spot_round_lot`]).
///
/// The floor at one lot keeps a snapped size strictly positive: `round` can
/// legitimately return zero for a draw well below half a lot.
pub(super) fn round_lot_size(base: f64, lot: f64) -> Decimal {
    decimal_from_f64((lot * (base / lot).round()).max(lot))
}

#[cfg(test)]
mod tests {
    use rand::{RngExt, SeedableRng};
    use rand_chacha::ChaCha12Rng;

    use super::*;

    fn slow(value: f64, dp: u32) -> Decimal {
        decimal_from_f64(value).round_dp(dp)
    }

    // Equal as wire text, which is what the tape carries: value and scale
    // both, so a fast path that kept a trailing zero `from_f64` strips fails
    // here even though the two compare equal as numbers. Returns whether the
    // fast path answered.
    fn assert_same(value: f64, dp: u32) -> bool {
        let fast = fast_round_dp(value, dp);
        let expected = slow(value, dp).to_string();
        if let Some(fast) = fast {
            assert_eq!(
                fast.to_string(),
                expected,
                "fast path diverged at {value:e} ({:#x}) dp {dp}",
                value.to_bits()
            );
        }
        assert_eq!(
            decimal_from_f64_round_dp(value, dp).to_string(),
            expected,
            "{value:e} dp {dp}"
        );
        fast.is_some()
    }

    /// The guarded fast path agrees with `from_f64` plus `round_dp` across
    /// thirty decades around the shipped size law, and turns away the
    /// adversarial inputs its guard exists for: exact grid points, exact
    /// half-points and their float neighbours, where a stripped trailing zero
    /// or a banker's tie decides the text. The hit counts are asserted too, so
    /// a guard that rejected everything - correct and useless - cannot pass.
    #[test]
    fn fast_round_dp_matches_the_decimal_conversion() {
        let mut rng = ChaCha12Rng::seed_from_u64(0x5EED_D0D0);
        let mut hits = 0_u32;
        let mut total = 0_u32;
        for _ in 0..60_000 {
            let value = 10_f64.powf(rng.random_range(-15.0..15.0));
            for dp in [0, 2, 4, 8, 10, 15] {
                hits += u32::from(assert_same(value, dp));
                total += 1;
            }
        }
        // About a third of these land with `y` in range at all; nearly all of
        // those are decided.
        assert!(hits > total / 4, "fast path fired {hits} of {total} times");

        let mut edge_hits = 0_u32;
        for k in 1_u64..20_000 {
            let k = k * 7_919;
            for dp in [2_u32, 8] {
                let scale = 10_f64.powi(dp as i32);
                for base in [k as f64 / scale, (k as f64 + 0.5) / scale] {
                    for value in [base.next_down(), base, base.next_up()] {
                        edge_hits += u32::from(assert_same(value, dp));
                    }
                }
            }
        }
        assert_eq!(edge_hits, 0, "the guard let a near-tie through");
    }

    /// A memo hit returns what a miss computes, through slot collisions that
    /// evict and refill, and at non-finite inputs that bypass it.
    #[test]
    fn price_memo_returns_the_conversion_through_collisions() {
        let tick = 0.01;
        let mut memo = PriceMemo::new(tick, 2);
        let mut rng = ChaCha12Rng::seed_from_u64(0x0BAD_F00D);
        let mut ticks = 9_500_000.0_f64;
        for _ in 0..50_000 {
            ticks += f64::from(rng.random_range(-40_i32..=40));
            assert_eq!(
                memo.price(ticks).to_string(),
                slow(ticks * tick, 2).to_string(),
                "ticks {ticks}"
            );
        }
        for ticks in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, 1.0] {
            assert_eq!(
                memo.price(ticks).to_string(),
                slow(ticks * tick, 2).to_string(),
                "ticks {ticks}"
            );
        }
    }
}
