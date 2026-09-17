// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The depth ladder: the one shared definition of what sits behind a
//! published touch, walked identically by the generator's book and the
//! venue's crossing. It lives in the protocol crate because it is a
//! statement about the published book both ends must agree on - the
//! generator consumes these quantities when it prints, and the crossing
//! fills against exactly the same quantities, so a private copy on either
//! side is a divergence waiting for a fixture that never exercises it.
//!
//! Two shapes exist and they are not interchangeable:
//!
//! - [`DepthLadder::Geometric`] is the legacy parametric ladder: level `k`
//!   sits `k` price increments beyond the touch with size
//!   `touch * growth^k`, applied by repeated multiply-and-floor on the
//!   instrument's size grid. Every non-book preset keeps this shape byte
//!   for byte.
//! - [`DepthLadder::Ratios`] is the fitted per-level profile from the
//!   mbp-10 corpus (jump-then-plateau, which no single multiplier
//!   expresses): every level anchors at the touch,
//!   `max(1, round_ties_even(touch_units * ratios[k - 1]))` units, in whole
//!   units of the size increment. The vector's length defines the display
//!   bound - `len + 1` levels counting the touch - so a bound inconsistent
//!   with the vector cannot be stated.
//!
//! The rounding is ties-to-even, declared: the fit's prototype is Python,
//! whose `round` is ties-to-even, and a touch of 1 at a ratio of 2.5 must
//! yield 2 units on both sides of the transcription. Rust's default
//! `f64::round` rounds ties away from zero and is therefore wrong here.

use rust_decimal::Decimal;
use std::sync::Arc;

/// The shape of the depth ladder behind a published touch, resolved from
/// the instrument's preset by the venue. See the module doc for the two
/// shapes and their arithmetic.
///
/// No `Default`. A ladder is either resolved from a preset or absent, and
/// an implicit one is how a zero-level ladder (which crosses nothing) or a
/// zero-growth one (which is not a ladder) reaches a crossing path looking
/// legitimate.
#[derive(Debug, Clone, PartialEq)]
pub enum DepthLadder {
    /// The legacy parametric ladder. `Decimal` growth because a level size
    /// must land on the instrument's size grid, so growth is applied by
    /// repeated multiply-and-floor rather than by a float power.
    Geometric { levels: u16, growth: Decimal },
    /// The fitted per-level ratio profile, in whole units of the size
    /// increment, every level anchored at the touch. `Arc` because the
    /// vector is resolved once per instrument and then rides in every
    /// reading and every hit.
    Ratios(Arc<[f64]>),
}

impl DepthLadder {
    /// The degenerate one-level ladder: the touch and nothing behind it.
    /// What an instrument with no calibrated depth quotes, and what the
    /// unit suites drive so their fills are the touch by arithmetic rather
    /// than by a special case in the crossing path.
    #[must_use]
    pub fn flat() -> Self {
        Self::Geometric {
            levels: 1,
            growth: Decimal::ONE,
        }
    }

    /// How many levels the ladder quotes, the touch included. For the
    /// ratio shape this is the vector's length plus one - the vector is
    /// the bound, so the two cannot disagree.
    #[must_use]
    pub fn levels(&self) -> u16 {
        match self {
            Self::Geometric { levels, .. } => *levels,
            Self::Ratios(ratios) => {
                u16::try_from(ratios.len() + 1).expect("validation bounds the ratio vector")
            }
        }
    }
}

/// The ratio ladder's level arithmetic, the one definition both the
/// generator's frozen-ladder walk and the venue's crossing call: level
/// zero is the displayed touch, and level `k` behind it carries
/// `max(1, round_ties_even(touch_units * ratios[k - 1]))` whole units of
/// the size increment.
///
/// A level past the vector is a programming error, not thin liquidity:
/// the vector defines the bound, every walk derives its level count from
/// the same vector, and an earlier draft that returned one unit for a
/// missing ratio manufactured tail liquidity out of a malformed preset.
#[must_use]
pub fn ladder_level_units(touch_units: u64, level: u16, ratios: &[f64]) -> u64 {
    if level == 0 {
        return touch_units;
    }
    let ratio = ratios[usize::from(level) - 1];
    ((touch_units as f64 * ratio).round_ties_even() as u64).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ladder_arithmetic_is_the_shared_ratio_rule() {
        let ratios = [2.0, 2.4, 3.4];
        assert_eq!(ladder_level_units(3, 0, &ratios), 3);
        assert_eq!(ladder_level_units(3, 1, &ratios), 6);
        assert_eq!(ladder_level_units(3, 2, &ratios), 7, "round of 7.2");
        assert_eq!(ladder_level_units(3, 3, &ratios), 10, "round of 10.2");
        assert_eq!(ladder_level_units(0, 1, &ratios), 1, "the one-unit floor");
    }

    #[test]
    fn rounding_is_ties_to_even_matching_the_prototype() {
        // Touch 1 at ratio 2.5: Python round(2.5) is 2, and the fitted
        // constants were derived under that convention. Rust's default
        // round would say 3.
        assert_eq!(ladder_level_units(1, 1, &[2.5]), 2);
        assert_eq!(ladder_level_units(1, 1, &[3.5]), 4, "3.5 rounds up to even");
        assert_eq!(
            ladder_level_units(3, 1, &[0.5]),
            2,
            "1.5 rounds down to even"
        );
    }

    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn a_level_past_the_vector_is_a_programming_error() {
        let _units = ladder_level_units(3, 4, &[2.0, 2.4, 3.4]);
    }

    #[test]
    fn the_vector_defines_the_level_count() {
        let ladder = DepthLadder::Ratios(Arc::from([2.0, 2.4, 2.5].as_slice()));
        assert_eq!(ladder.levels(), 4);
        assert_eq!(DepthLadder::flat().levels(), 1);
    }
}
