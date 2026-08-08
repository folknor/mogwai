// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Exact population variance, matching `statistics.pvariance` bit for bit.
//!
//! WHY THIS EXISTS. `check_cadence_feasible.py:187` calls
//! `statistics.pvariance(gaps)`, which evaluates the variance as an EXACT
//! RATIONAL over its binary64 inputs and rounds once at the end. The obvious
//! port - sum the squared deviations from the rounded mean with `py_fsum` and
//! divide - is not merely a last-bit difference from that. It is
//! ill-conditioned: when the series is clustered, the true variance is a
//! difference of quantities that agree in almost every bit, so the rounding of
//! each individual square dominates the answer. On three nearly-equal gaps it
//! comes out WRONG BY A FACTOR OF THREE. Three separate ULP ceilings were
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
//! HOW IT IS EXACT WITHOUT RATIONALS. Every finite binary64 is an integer times
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

/// Builds `mantissa * 2^exponent` as a binary64 without any rounding, for a
/// mantissa of at most 53 significant bits.
///
/// Multiplication by a power of two is exact, so constructing the scale
/// directly from its bit pattern and multiplying keeps the single rounding in
/// [`population_variance`] where it belongs. `f64::powi` is deliberately NOT
/// used: its precision is documented as varying by platform and Rust version,
/// which would make the result of an "exact" routine platform-dependent.
fn scale_by_power_of_two(mantissa: u64, exponent: i32) -> f64 {
    debug_assert!(mantissa < (1 << 53));
    let value = mantissa as f64;
    if mantissa == 0 {
        return 0.0;
    }
    // Split the scaling so neither step overflows or flushes to zero for any
    // exponent the identity can produce.
    let mut result = value;
    let mut remaining = exponent;
    while remaining != 0 {
        let step = remaining.clamp(-512, 512);
        let field = step + 1023;
        debug_assert!((1..=2046).contains(&field));
        result *= f64::from_bits((field as u64) << 52);
        remaining -= step;
    }
    result
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

    // Shift up so the quotient carries at least 55 bits: 53 of significand plus
    // a rounding bit, with anything below captured by the remainder as sticky.
    let target = 55 + usize::try_from(64 - divisor.leading_zeros()).expect("bit count");
    let shift = target.saturating_sub(numerator.bit_len());
    let (quotient, remainder) = numerator.shl_bits(shift).div_rem_small(divisor);
    let sticky_from_division = remainder != 0;

    let quotient_bits = quotient.bit_len();
    assert!(
        quotient_bits > 53,
        "the shift must leave more than a significand's worth of quotient"
    );
    let drop = quotient_bits - 53;
    let (mut mantissa, mut carried) = {
        let mut acc = 0u64;
        for index in (drop..quotient_bits).rev() {
            acc = (acc << 1) | u64::from(quotient.bit(index));
        }
        (acc, false)
    };
    let round_bit = quotient.bit(drop - 1);
    let sticky = sticky_from_division || (drop >= 2 && quotient.any_bits_below(drop - 1));
    if round_bit && (sticky || mantissa & 1 == 1) {
        mantissa += 1;
        if mantissa == 1 << 53 {
            mantissa >>= 1;
            carried = true;
        }
    }

    let mut result_exponent = exponent - i64::try_from(shift).expect("shift fits") + drop as i64;
    if carried {
        result_exponent += 1;
    }
    let clamped = i32::try_from(result_exponent).expect("exponent fits an i32");
    scale_by_power_of_two(mantissa, clamped)
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
    /// That is the RIGHT answer rather than a lost one, and it is worth pinning
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
