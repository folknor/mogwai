// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Exact population variance, matching `statistics.pvariance` bit for bit.
//!
//! Why this exists. `check_cadence_feasible.py:187` calls
//! `statistics.pvariance(gaps)`, which evaluates the variance as an exact
//! rational over its binary64 inputs and rounds once at the end. The obvious
//! port - sum the squared deviations from the rounded mean with `py_fsum` and
//! divide - is not merely a last-bit difference from that. It is
//! ill-conditioned: when the series is clustered, the true variance is a
//! difference of quantities that agree in almost every bit, so the rounding of
//! each individual square dominates the answer. On three nearly-equal gaps it
//! comes out wrong by a factor of three. Three separate ULP ceilings were
//! claimed for that approach and all three were refuted, because the error has
//! no bound to find.
//!
//! So this module computes the same rational CPython does, exactly, and rounds
//! once:
//!
//! ```text
//! variance = (n * sum(x_i^2) - (sum x_i)^2) / n^2
//! ```
//!
//! That identity was verified against `statistics.pvariance` over 2,005 cases
//! including the pathological ones before a line of this was written; it is not
//! an inference from reading the module.
//!
//! How it is exact without rationals. Every finite binary64 is an integer times
//! a power of two, `x = m * 2^e`. Writing `s` for the smallest `e` in the
//! sample, both sums become integers against one shared scale:
//!
//! ```text
//! sum x_i        = A * 2^s        A = sum m_i * 2^(e_i - s)
//! sum x_i^2      = B * 2^(2s)     B = sum m_i^2 * 2^(2(e_i - s))
//! n*Q - S^2      = (n*B - A^2) * 2^(2s)
//! ```
//!
//! `A`, `B` and `n*B - A^2` are exact integers, so the only rounding in the
//! whole computation is the final division by `n^2`, done once, to nearest,
//! ties to even. No floating-point arithmetic participates before that point.
//!
//! The arbitrary-precision natural below is deliberately minimal - addition,
//! subtraction, schoolbook multiplication, a shift, and division by a
//! single-limb divisor. That is the entire set this identity needs, and a
//! dependency would have brought a general-purpose bignum for one function.

/// A minimal arbitrary-precision natural, little-endian limbs, always
/// normalized so the most significant limb is nonzero (except for zero, which
/// is the empty limb vector).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Nat {
    limbs: Vec<u64>,
}

impl Nat {
    const fn zero() -> Self {
        Self { limbs: Vec::new() }
    }

    fn from_u64(value: u64) -> Self {
        let mut out = Self { limbs: vec![value] };
        out.normalize();
        out
    }

    fn normalize(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
    }

    fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    /// Position of the most significant set bit, one-based. Zero for zero.
    fn bit_len(&self) -> usize {
        match self.limbs.last() {
            None => 0,
            Some(top) => (self.limbs.len() - 1) * 64 + (64 - top.leading_zeros() as usize),
        }
    }

    /// The bit at `index`, counting from zero at the least significant end.
    fn bit(&self, index: usize) -> bool {
        let limb = index / 64;
        self.limbs
            .get(limb)
            .is_some_and(|word| (word >> (index % 64)) & 1 == 1)
    }

    /// True when any bit strictly below `index` is set - the sticky flag a
    /// round-to-nearest decision needs.
    fn any_bits_below(&self, index: usize) -> bool {
        let full = index / 64;
        if self.limbs.iter().take(full).any(|word| *word != 0) {
            return true;
        }
        let rest = index % 64;
        rest != 0
            && self
                .limbs
                .get(full)
                .is_some_and(|word| word & ((1u64 << rest) - 1) != 0)
    }

    fn cmp_nat(&self, other: &Self) -> std::cmp::Ordering {
        if self.limbs.len() != other.limbs.len() {
            return self.limbs.len().cmp(&other.limbs.len());
        }
        for (a, b) in self.limbs.iter().rev().zip(other.limbs.iter().rev()) {
            if a != b {
                return a.cmp(b);
            }
        }
        std::cmp::Ordering::Equal
    }

    fn add_assign(&mut self, other: &Self) {
        if self.limbs.len() < other.limbs.len() {
            self.limbs.resize(other.limbs.len(), 0);
        }
        let mut carry = 0u64;
        for (index, slot) in self.limbs.iter_mut().enumerate() {
            let addend = other.limbs.get(index).copied().unwrap_or(0);
            let (sum, overflow_one) = slot.overflowing_add(addend);
            let (sum, overflow_two) = sum.overflowing_add(carry);
            *slot = sum;
            carry = u64::from(overflow_one) + u64::from(overflow_two);
        }
        if carry != 0 {
            self.limbs.push(carry);
        }
    }

    /// `self -= other`, which requires `self >= other`.
    fn sub_assign(&mut self, other: &Self) {
        debug_assert!(self.cmp_nat(other) != std::cmp::Ordering::Less);
        let mut borrow = 0u64;
        for index in 0..self.limbs.len() {
            let subtrahend = other.limbs.get(index).copied().unwrap_or(0);
            let (difference, borrow_one) = self.limbs[index].overflowing_sub(subtrahend);
            let (difference, borrow_two) = difference.overflowing_sub(borrow);
            self.limbs[index] = difference;
            borrow = u64::from(borrow_one) + u64::from(borrow_two);
        }
        debug_assert_eq!(borrow, 0, "sub_assign underflowed");
        self.normalize();
    }

    fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let mut out = vec![0u64; self.limbs.len() + other.limbs.len()];
        for (i, a) in self.limbs.iter().enumerate() {
            let mut carry = 0u128;
            for (j, b) in other.limbs.iter().enumerate() {
                let slot = &mut out[i + j];
                let wide = u128::from(*a) * u128::from(*b) + u128::from(*slot) + carry;
                *slot = wide as u64;
                carry = wide >> 64;
            }
            let mut index = i + other.limbs.len();
            while carry != 0 {
                let wide = u128::from(out[index]) + carry;
                out[index] = wide as u64;
                carry = wide >> 64;
                index += 1;
            }
        }
        let mut result = Self { limbs: out };
        result.normalize();
        result
    }

    fn mul_small(&self, factor: u64) -> Self {
        if self.is_zero() || factor == 0 {
            return Self::zero();
        }
        let mut out = Vec::with_capacity(self.limbs.len() + 1);
        let mut carry = 0u128;
        for limb in &self.limbs {
            let wide = u128::from(*limb) * u128::from(factor) + carry;
            out.push(wide as u64);
            carry = wide >> 64;
        }
        if carry != 0 {
            out.push(carry as u64);
        }
        let mut result = Self { limbs: out };
        result.normalize();
        result
    }

    fn shl_bits(&self, shift: usize) -> Self {
        if self.is_zero() {
            return Self::zero();
        }
        let whole = shift / 64;
        let part = shift % 64;
        let mut out = vec![0u64; whole];
        if part == 0 {
            out.extend_from_slice(&self.limbs);
        } else {
            let mut carry = 0u64;
            for limb in &self.limbs {
                out.push((limb << part) | carry);
                carry = limb >> (64 - part);
            }
            if carry != 0 {
                out.push(carry);
            }
        }
        let mut result = Self { limbs: out };
        result.normalize();
        result
    }

    /// Divides by a single-limb divisor, returning the quotient and remainder.
    fn div_rem_small(&self, divisor: u64) -> (Self, u64) {
        assert!(divisor != 0, "division by zero");
        let mut quotient = vec![0u64; self.limbs.len()];
        let mut remainder = 0u128;
        for index in (0..self.limbs.len()).rev() {
            let current = (remainder << 64) | u128::from(self.limbs[index]);
            quotient[index] = (current / u128::from(divisor)) as u64;
            remainder = current % u128::from(divisor);
        }
        let mut result = Self { limbs: quotient };
        result.normalize();
        (result, remainder as u64)
    }
}

/// Splits a finite binary64 into `(sign_is_negative, mantissa, exponent)` such
/// that the value is exactly `+/- mantissa * 2^exponent`.
fn decompose(value: f64) -> (bool, u64, i32) {
    let bits = value.to_bits();
    let negative = bits >> 63 == 1;
    let exponent_field = ((bits >> 52) & 0x7FF) as i32;
    let fraction = bits & 0x000F_FFFF_FFFF_FFFF;
    if exponent_field == 0 {
        // Subnormal (and zero): no implicit leading one, fixed exponent.
        (negative, fraction, -1074)
    } else {
        (negative, fraction | (1 << 52), exponent_field - 1075)
    }
}

/// Exact population variance of `values`, matching
/// `statistics.pvariance(values)` bit for bit.
///
/// Returns `0.0` for a single value, as the identity does. An empty slice has
/// no population variance; CPython raises `StatisticsError` and this returns
/// `f64::NAN`, since the sole caller cannot produce one.
///
/// # Panics
/// If any value is not finite. The gap series this serves is built from
/// finite differences, and a non-finite input would make the exact identity
/// meaningless rather than merely inaccurate.
#[must_use]
pub fn population_variance(values: &[f64]) -> f64 {
    let count = values.len();
    if count == 0 {
        return f64::NAN;
    }
    assert!(
        values.iter().all(|v| v.is_finite()),
        "exact population variance requires finite inputs"
    );
    if count == 1 {
        return 0.0;
    }

    let scale = values
        .iter()
        .map(|v| decompose(*v).2)
        .min()
        .expect("nonempty");

    // Sum of the values, and sum of their squares, both against `scale`.
    // Positive and negative contributions accumulate separately so no signed
    // big integer is needed: only the magnitude of the total is ever squared.
    let mut positive = Nat::zero();
    let mut negative = Nat::zero();
    let mut squares = Nat::zero();
    for value in values {
        let (is_negative, mantissa, exponent) = decompose(*value);
        let shift = usize::try_from(exponent - scale).expect("scale is the minimum exponent");
        let term = Nat::from_u64(mantissa).shl_bits(shift);
        if is_negative {
            negative.add_assign(&term);
        } else {
            positive.add_assign(&term);
        }
        let square = Nat::from_u64(mantissa).mul(&Nat::from_u64(mantissa));
        squares.add_assign(&square.shl_bits(shift * 2));
    }

    let mut total = if positive.cmp_nat(&negative) == std::cmp::Ordering::Less {
        let mut swapped = negative.clone();
        swapped.sub_assign(&positive);
        swapped
    } else {
        let mut kept = positive.clone();
        kept.sub_assign(&negative);
        kept
    };
    let total_squared = total.mul(&total);
    total = Nat::zero();
    debug_assert!(total.is_zero());

    let count_u64 = u64::try_from(count).expect("sample count fits a u64");
    let mut numerator = squares.mul_small(count_u64);
    debug_assert!(
        numerator.cmp_nat(&total_squared) != std::cmp::Ordering::Less,
        "n * sum of squares is at least the square of the sum, by Cauchy-Schwarz"
    );
    numerator.sub_assign(&total_squared);
    if numerator.is_zero() {
        return 0.0;
    }

    // The exact value is now `numerator * 2^(2 * scale) / count^2`. Divide once,
    // to nearest, ties to even.
    let divisor = count_u64
        .checked_mul(count_u64)
        .expect("count squared fits a u64 for any realistic sample");
    let exponent = 2 * i64::from(scale);

    // Shift up so the quotient carries plenty of bits above whatever position
    // the rounding lands on, with anything below the last one captured by the
    // division remainder as sticky.
    let target = 64 + usize::try_from(64 - divisor.leading_zeros()).expect("bit count");
    let shift = target.saturating_sub(numerator.bit_len());
    let (quotient, remainder) = numerator.shl_bits(shift).div_rem_small(divisor);
    let sticky_from_division = remainder != 0;
    debug_assert!(!quotient.is_zero(), "a nonzero numerator keeps a quotient");

    // `quotient * 2^quotient_exponent` is the exact value, give or take the
    // remainder already folded into the sticky flag.
    let quotient_exponent = exponent - i64::try_from(shift).expect("shift fits");
    let leading =
        i64::try_from(quotient.bit_len() - 1).expect("bit length fits") + quotient_exponent;

    // Where to round, and this is the whole subtlety. A normal result keeps 53
    // significant bits, so its least significant bit sits at `leading - 52`. A
    // subnormal result has no such freedom: every subnormal is an integer
    // multiple of 2^-1074, so the rounding position is pinned there and the
    // result keeps fewer than 53 bits.
    //
    // Rounding to 53 bits first and scaling afterwards - which is what this
    // function used to do - therefore rounds twice for a subnormal result, once
    // to 53 bits and again on the way down into the subnormal range. That is a
    // real defect and not a theoretical one: five specific finite inputs made it
    // return one ULP below `statistics.pvariance`, and the 820-case sweep missed
    // it because none of its families produce a nonzero subnormal variance. Its
    // 39 zero results exercise underflow to zero, which is a different class
    // from correct rounding within the subnormal range.
    const MIN_SUBNORMAL_EXPONENT: i64 = -1074;
    let round_position = (leading - 52).max(MIN_SUBNORMAL_EXPONENT);

    let drop = round_position - quotient_exponent;
    let mut mantissa: u64 = if drop <= 0 {
        // The quotient is already finer than the target precision. Only
        // reachable for absurdly small samples; shift up rather than lose bits.
        let up = usize::try_from(-drop).expect("shift fits");
        let widened = quotient.shl_bits(up);
        debug_assert!(widened.bit_len() <= 64);
        let mut acc = 0u64;
        for index in (0..widened.bit_len()).rev() {
            acc = (acc << 1) | u64::from(widened.bit(index));
        }
        acc
    } else {
        let drop = usize::try_from(drop).expect("drop fits");
        let bits = quotient.bit_len();
        let mut acc = 0u64;
        if drop < bits {
            for index in (drop..bits).rev() {
                acc = (acc << 1) | u64::from(quotient.bit(index));
            }
        }
        let round_bit = drop >= 1 && quotient.bit(drop - 1);
        let sticky = sticky_from_division || (drop >= 2 && quotient.any_bits_below(drop - 1));
        if round_bit && (sticky || acc & 1 == 1) {
            acc += 1;
        }
        acc
    };

    if mantissa == 0 {
        // Rounded to nothing: the exact value is below half the smallest
        // subnormal, which is what CPython returns zero for as well.
        return 0.0;
    }

    // Assemble the bit pattern directly rather than scaling by a power of two,
    // so the single rounding above is the only one.
    if round_position == MIN_SUBNORMAL_EXPONENT {
        // This branch covers more than the subnormals, and the extra coverage is
        // correct rather than accidental. `round_position` is
        // `max(leading - 52, -1074)`, so it pins to the floor for every result
        // whose leading bit sits at or below 2^-1022 - which is every subnormal
        // and the whole lowest normal binade, from 2^-1022 up to just under
        // 2^-1021.
        //
        // Direct assembly handles all of it in one expression, because binary64
        // encodes both ranges as the same integer multiple of 2^-1074:
        // `mantissa` below 2^52 is a subnormal bit pattern, exactly 2^52 is the
        // smallest normal, and above that it is the lowest normal binade with
        // its exponent field already reading 1. The carry out of the subnormal
        // range therefore needs no special case either.
        //
        // The assertion here originally read `mantissa <= 1 << 52`, which was
        // narrower than the branch and panicked in debug builds throughout the
        // lowest normal binade - on values `from_bits` was constructing
        // correctly. Release builds were unaffected, which is precisely why the
        // bound has to be right rather than merely absent.
        debug_assert!(
            mantissa < 1 << 53,
            "the branch spans the subnormals and the lowest normal binade"
        );
        return f64::from_bits(mantissa);
    }

    let mut result_exponent = leading;
    if mantissa == 1 << 53 {
        // Rounding carried into the next binade.
        mantissa >>= 1;
        result_exponent += 1;
    }
    debug_assert!((1 << 52..1 << 53).contains(&mantissa));
    let field = result_exponent + 1023;
    assert!(
        field <= 2046,
        "population variance overflowed binary64; the inputs are near the representable maximum"
    );
    debug_assert!(field >= 1, "the subnormal branch above covers field < 1");
    #[expect(
        clippy::cast_sign_loss,
        reason = "field is checked to be in the normal exponent range"
    )]
    let bits = ((field as u64) << 52) | (mantissa - (1 << 52));
    f64::from_bits(bits)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The case that motivated the whole module: three nearly-equal gaps, where
    /// the `py_fsum`-over-squared-deviations approach is wrong by a factor of
    /// three. CPython 3.14.6 gives `0x1.c71c71c71c71cp-109`.
    #[test]
    fn the_factor_of_three_case_is_now_exact() {
        let gaps = [
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBE),
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBE),
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBF),
        ];
        assert_eq!(
            population_variance(&gaps).to_bits(),
            0x392C_71C7_1C71_C71C,
            "must equal statistics.pvariance exactly, not approximately"
        );
    }

    /// The original three-gap discriminator. CPython gives
    /// `0.14509134298012094`.
    #[test]
    fn the_three_gap_case_is_now_exact() {
        let gaps = [0.154_148_210_468, 1.076_405_188_57, 0.737_720_944_656];
        assert_eq!(
            population_variance(&gaps).to_bits(),
            0.145_091_342_980_120_94_f64.to_bits()
        );
    }

    /// The double-rounding case. Five finite inputs whose exact variance is a
    /// nonzero subnormal, which is the one output class the generated sweep did
    /// not reach: its zero results exercise underflow to zero, a different
    /// thing from correct rounding inside the subnormal range.
    ///
    /// The first implementation rounded the quotient to 53 bits and then scaled
    /// it down by powers of two, so a subnormal result got rounded twice and
    /// landed one ULP low. CPython 3.14.6 gives `0x00058bda4738f5c3`; the
    /// two-step version gave `0x00058bda4738f5c2`.
    #[test]
    fn a_nonzero_subnormal_result_rounds_only_once() {
        let values = [
            f64::from_bits(0x2236_016C_7435_BC70),
            f64::from_bits(0x2236_016C_7435_3B90),
            f64::from_bits(0x2236_016C_7437_8AAE),
            f64::from_bits(0x2236_016C_7434_8566),
            f64::from_bits(0x2236_016C_7434_32DF),
        ];
        let got = population_variance(&values);
        assert!(
            got > 0.0 && got < f64::MIN_POSITIVE,
            "the case only bites if the result really is a nonzero subnormal: {got:?}"
        );
        assert_eq!(got.to_bits(), 0x0005_8BDA_4738_F5C3);
        assert_ne!(
            got.to_bits(),
            0x0005_8BDA_4738_F5C2,
            "that is the double-rounded value; seeing it again means the rounding \
             position stopped tracking the subnormal floor"
        );
    }

    /// The boundaries either side of the subnormal range, where the rounding
    /// position changes rule. The largest subnormal and the smallest normal are
    /// adjacent representable values, so a variance landing between them has to
    /// pick the right one rather than falling off either branch.
    #[test]
    fn the_subnormal_and_normal_boundary_is_handled() {
        // Smallest positive subnormal as a variance: two values 2*sqrt(that)
        // apart is awkward to construct, so drive it from the identity instead
        // and simply assert the branch produces representable, correctly
        // classified output across the boundary.
        let min_subnormal = f64::from_bits(1);
        let min_normal = f64::MIN_POSITIVE;

        let near_zero = population_variance(&[0.0, min_subnormal]);
        assert_eq!(
            near_zero.to_bits(),
            0,
            "underflows to zero, as CPython does"
        );

        // A pair whose exact variance is exactly the smallest normal: variance
        // of [0, x] is x^2/4, so x = 2 * sqrt(min_normal) gives exactly
        // min_normal when both are exactly representable.
        let root = f64::from_bits(0x2000_0000_0000_0000); // 2^-511, exact
        let variance = population_variance(&[0.0, root * 2.0]);
        assert_eq!(
            variance.to_bits(),
            min_normal.to_bits(),
            "x^2/4 for x = 2^-510 is exactly 2^-1022, the smallest normal"
        );
    }

    /// Inside the lowest normal binade, which the boundary case above does not
    /// reach and which is a separate branch condition in disguise.
    ///
    /// `round_position` pins to the subnormal floor for every result whose
    /// leading bit sits at or below `2^-1022`, so the direct-assembly branch
    /// spans the subnormals AND all of `[2^-1022, 2^-1021)`. Landing exactly ON
    /// the join leaves `mantissa` at exactly `2^52` - which is why the test
    /// above passed against an assertion that was too narrow by a whole binade.
    /// Anything strictly inside has `mantissa > 2^52` and used to panic in debug
    /// builds only, while release computed the correct value throughout. A
    /// wrong bound is worse than no bound for exactly that reason: it fails in
    /// the configuration that is supposed to be the stricter one.
    ///
    /// `statistics.pvariance([0.0, 0x1.4p-510])` is `0x1.9p-1022`, bit pattern
    /// `0x0019000000000000`. The `x^2/4` identity gives the expectation without
    /// a CPython round trip: `(1.25 * 2^-510)^2 / 4` is `1.5625 * 2^-1022`.
    #[test]
    fn the_lowest_normal_binade_is_assembled_not_asserted_away() {
        let got = population_variance(&[0.0, f64::from_bits(0x2014_0000_0000_0000)]);
        assert_eq!(got.to_bits(), 0x0019_0000_0000_0000);
        assert!(
            got > f64::MIN_POSITIVE && got < f64::MIN_POSITIVE * 2.0,
            "the case only bites STRICTLY inside the lowest normal binade: {got:?}"
        );
    }

    #[test]
    fn a_single_value_has_zero_variance() {
        assert_eq!(population_variance(&[3.5]), 0.0);
    }

    #[test]
    fn identical_values_have_exactly_zero_variance() {
        assert_eq!(population_variance(&[2.5, 2.5, 2.5, 2.5]), 0.0);
    }

    #[test]
    fn an_empty_sample_has_no_variance() {
        assert!(population_variance(&[]).is_nan());
    }

    /// A wide dynamic range, where the terms span 18 decades and a naive
    /// accumulation loses the small ones entirely. CPython 3.14.6:
    /// `statistics.pvariance([1e-9, 1.0, 1e9])` is `2.22222222e+17`, bit
    /// pattern `0x4388ABEF77DC1060`.
    #[test]
    fn a_wide_dynamic_range_is_exact() {
        assert_eq!(
            population_variance(&[1e-9, 1.0, 1e9]).to_bits(),
            0x4388_ABEF_77DC_1060
        );
    }

    /// Subnormal inputs exercise the `exponent_field == 0` branch of the
    /// decomposition, where there is no implicit leading one.
    ///
    /// Both of these underflow to exactly zero, and CPython agrees: the true
    /// variance of `[0, 5e-324]` is `2^-2150`, far below the smallest
    /// representable subnormal, so the single final rounding takes it to zero.
    /// That is the right answer rather than a lost one, and it is worth pinning
    /// because an implementation that scaled through an intermediate could
    /// return a denormal here instead.
    #[test]
    fn subnormal_inputs_decompose_correctly() {
        let tiny = f64::from_bits(1);
        assert_eq!(population_variance(&[tiny, tiny]).to_bits(), 0);
        assert_eq!(population_variance(&[0.0, tiny]).to_bits(), 0);
    }

    #[test]
    fn negative_values_are_handled_through_the_magnitude() {
        // Variance is translation-invariant, and these two samples differ by a
        // shift of 2.0, so their exact variances must agree bit for bit.
        let shifted = population_variance(&[1.0, 3.0, 5.0]);
        let centred = population_variance(&[-1.0, 1.0, 3.0]);
        assert_eq!(shifted.to_bits(), centred.to_bits());
        // And the value itself, because agreement alone is the one shape in this
        // module that a broken implementation can satisfy for free: both sides
        // returning zero, or both NaN, agree perfectly. 8/3 exactly, which the
        // nearest double renders as 0x4005555555555555.
        assert_eq!(
            shifted.to_bits(),
            (8.0_f64 / 3.0).to_bits(),
            "the shared value must be the variance, not merely a shared answer"
        );
    }

    #[test]
    fn the_nat_helpers_round_trip() {
        let a = Nat::from_u64(u64::MAX);
        let b = a.mul(&a);
        assert_eq!(b.bit_len(), 128);
        let (quotient, remainder) = b.div_rem_small(u64::MAX);
        assert_eq!(remainder, 0);
        assert_eq!(quotient, a);

        let shifted = Nat::from_u64(1).shl_bits(200);
        assert_eq!(shifted.bit_len(), 201);
        assert!(shifted.bit(200));
        assert!(!shifted.bit(199));
        assert!(!shifted.any_bits_below(200));
    }
}
