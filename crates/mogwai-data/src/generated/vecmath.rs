// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Batched float transforms for the tape's contiguous, feedback-free draws:
//! the normal quantile of a run of raw words, and the size law's `exp`.
//!
//! Each runs four lanes in AVX2 where the running CPU has it, chosen at run
//! time as the Philox fill is, and the scalar code elsewhere and on every
//! remainder. The two paths evaluate the same expression tree with separately
//! rounded IEEE operations - no FMA, no reassociation, no reciprocal
//! approximation - so a value is bit-identical whichever path computed it,
//! and never depends on batch length, lane or position.
//!
//! The quantile's vector path computes only AS 241's central branch, about
//! 85 percent of inputs, and hands the tail lanes to
//! [`normal_quantile`] one by one, so it returns exactly what the scalar
//! quantile returns. [`tape_exp`] is this module's own: it does not match
//! `libm` bit for bit, and is the size law's `exp` since tape protocol 41.
//! [`tape_ln`] is likewise its own, the bounce walks' random-input
//! logarithm since tape protocol 42, four at a time through [`tape_ln4`].

use super::children::{normal_quantile, open_unit};

/// fdlibm's `e_log` polynomial coefficients, by their published bits.
const LG: [f64; 7] = [
    f64::from_bits(0x3FE5_5555_5555_5593),
    f64::from_bits(0x3FD9_9999_9997_FA04),
    f64::from_bits(0x3FD2_4924_9422_9359),
    f64::from_bits(0x3FCC_71C5_1D8E_78AF),
    f64::from_bits(0x3FC7_4664_96CB_03DE),
    f64::from_bits(0x3FC3_9A09_D078_C69F),
    f64::from_bits(0x3FC2_F112_DF3E_5244),
];
/// The mantissa field and the offset that carries a mantissa at or past
/// `sqrt(2)` into the exponent: fdlibm's `0x95f64` in the high word.
const MANTISSA: u64 = (1 << 52) - 1;
const SQRT2_CARRY: u64 = 0x95F64 << 32;
const ONE_EXPONENT: u64 = 0x3FF << 52;

/// `ln x` for a positive normal `x`, refusing anything else: fdlibm's
/// `e_log` reduction `x = 2^k m`, `m` in `[sqrt(2) / 2, sqrt(2))`,
/// `s = f / (2 + f)` with `f = m - 1`, its even and odd polynomial halves,
/// and its reconstruction, applied uniformly, without any of its special
/// branches, so the accuracy is this tree's, pinned by the tests rather than
/// inherited. `tape_ln(1.0)` is exactly zero, and every input below one
/// gives a negative value.
pub(super) fn tape_ln(x: f64) -> f64 {
    assert!(x.is_normal() && x > 0.0, "tape_ln outside its domain: {x}");
    let bits = x.to_bits();
    let mantissa = bits & MANTISSA;
    let carry = (mantissa + SQRT2_CARRY) & (1 << 52);
    let k = (bits >> 52) as i64 - 1023 + (carry >> 52) as i64;
    let m = f64::from_bits(mantissa | (carry ^ ONE_EXPONENT));
    let f = m - 1.0;
    let s = f / (2.0 + f);
    let z = s * s;
    let w = z * z;
    let t1 = w * (LG[1] + w * (LG[3] + w * LG[5]));
    let t2 = z * (LG[0] + w * (LG[2] + w * (LG[4] + w * LG[6])));
    let r = t2 + t1;
    let hfsq = 0.5 * f * f;
    let dk = k as f64;
    dk * LN2_HI - ((hfsq - (s * (hfsq + r) + dk * LN2_LO)) - f)
}

/// [`tape_ln`] of four values at once, in AVX2 lanes where the CPU has
/// them: the same bits, lane by lane, as four scalar calls.
pub(super) fn tape_ln4(xs: [f64; 4]) -> [f64; 4] {
    #[cfg(target_arch = "x86_64")]
    if vector_path() {
        // SAFETY: `vector_path` checked the running CPU for AVX2, the only
        // feature the called function enables.
        return unsafe { tape_ln4_avx2(xs) };
    }
    xs.map(tape_ln)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn tape_ln4_avx2(xs: [f64; 4]) -> [f64; 4] {
    use std::arch::x86_64::{
        _mm256_add_epi64, _mm256_add_pd, _mm256_and_si256, _mm256_castpd_si256,
        _mm256_castsi256_pd, _mm256_div_pd, _mm256_loadu_pd, _mm256_mul_pd, _mm256_or_si256,
        _mm256_set1_epi64x, _mm256_set1_pd, _mm256_srli_epi64, _mm256_storeu_pd, _mm256_sub_epi64,
        _mm256_sub_pd, _mm256_xor_si256,
    };
    for x in xs {
        assert!(x.is_normal() && x > 0.0, "tape_ln outside its domain: {x}");
    }
    let c = |v: f64| _mm256_set1_pd(v);
    // SAFETY: `xs` is four `f64`s; the unaligned load needs no alignment.
    let x = unsafe { _mm256_loadu_pd(xs.as_ptr()) };
    let bits = _mm256_castpd_si256(x);
    let mantissa = _mm256_and_si256(bits, _mm256_set1_epi64x(MANTISSA as i64));
    let carry = _mm256_and_si256(
        _mm256_add_epi64(mantissa, _mm256_set1_epi64x(SQRT2_CARRY as i64)),
        _mm256_set1_epi64x(1 << 52),
    );
    // `k` as a double without a 64-bit integer conversion: an exact small
    // integer added into the mantissa of `1.5 * 2^52`, less that value.
    let k = _mm256_sub_epi64(
        _mm256_add_epi64(
            _mm256_srli_epi64::<52>(bits),
            _mm256_srli_epi64::<52>(carry),
        ),
        _mm256_set1_epi64x(1023),
    );
    let dk = _mm256_sub_pd(
        _mm256_castsi256_pd(_mm256_add_epi64(
            k,
            _mm256_set1_epi64x(SHIFT.to_bits() as i64),
        )),
        c(SHIFT),
    );
    let m = _mm256_castsi256_pd(_mm256_or_si256(
        mantissa,
        _mm256_xor_si256(carry, _mm256_set1_epi64x(ONE_EXPONENT as i64)),
    ));
    let f = _mm256_sub_pd(m, c(1.0));
    let s = _mm256_div_pd(f, _mm256_add_pd(c(2.0), f));
    let z = _mm256_mul_pd(s, s);
    let w = _mm256_mul_pd(z, z);
    let mul = |a, b| _mm256_mul_pd(a, b);
    let add = |a, b| _mm256_add_pd(a, b);
    let t1 = mul(w, add(c(LG[1]), mul(w, add(c(LG[3]), mul(w, c(LG[5]))))));
    let t2 = mul(
        z,
        add(
            c(LG[0]),
            mul(w, add(c(LG[2]), mul(w, add(c(LG[4]), mul(w, c(LG[6])))))),
        ),
    );
    let r = add(t2, t1);
    let hfsq = mul(mul(c(0.5), f), f);
    let inner = _mm256_sub_pd(hfsq, add(mul(s, add(hfsq, r)), mul(dk, c(LN2_LO))));
    let result = _mm256_sub_pd(mul(dk, c(LN2_HI)), _mm256_sub_pd(inner, f));
    let mut out = [0.0; 4];
    // SAFETY: `out` is four `f64`s.
    unsafe { _mm256_storeu_pd(out.as_mut_ptr(), result) };
    out
}

/// AS 241's central numerator, highest power first, as printed in the
/// scalar [`normal_quantile`].
#[expect(
    clippy::excessive_precision,
    reason = "the published AS 241 coefficients, kept as printed so they can be checked against the paper"
)]
const CENTRAL_NUM: [f64; 8] = [
    2.509_080_928_730_122_7e3,
    3.343_057_558_358_812_8e4,
    6.726_577_092_700_870_1e4,
    4.592_195_393_154_987_1e4,
    1.373_169_376_550_946_1e4,
    1.971_590_950_306_551_4e3,
    1.331_416_678_917_843_8e2,
    3.387_132_872_796_366_6,
];

/// AS 241's central denominator, highest power first.
#[expect(
    clippy::excessive_precision,
    reason = "the published AS 241 coefficients, kept as printed so they can be checked against the paper"
)]
const CENTRAL_DEN: [f64; 8] = [
    5.226_495_278_852_854_6e3,
    2.872_908_573_572_194_3e4,
    3.930_789_580_009_271_1e4,
    2.121_379_430_158_659_6e4,
    5.394_196_021_424_751_1e3,
    6.871_870_074_920_579_1e2,
    4.231_333_070_160_091_1e1,
    1.0,
];

/// The standard normal quantile of `open_unit(word)` for every word, into
/// `out`, which must be as long as `words`. Bit-identical to calling
/// [`normal_quantile`] on each.
pub(super) fn normal_quantiles(words: &[u64], out: &mut [f64]) {
    assert_eq!(words.len(), out.len(), "one quantile per word");
    #[cfg(target_arch = "x86_64")]
    if vector_path() {
        // SAFETY: the running CPU was just checked for AVX2, the only feature
        // the called function enables.
        unsafe { normal_quantiles_avx2(words, out) };
        return;
    }
    normal_quantiles_scalar(words, out);
}

/// Whether the batches run in AVX2 lanes: on a CPU that has it, in a build
/// without debug assertions. An unoptimized build makes every intrinsic a
/// call and measured the vector path at more than twice the scalar wall,
/// and the two paths give the same bits, so the choice cannot touch a tape;
/// the tests run the vector kernels directly whatever the build.
#[cfg(target_arch = "x86_64")]
fn vector_path() -> bool {
    !cfg!(debug_assertions) && std::arch::is_x86_feature_detected!("avx2")
}

/// The scalar path of [`normal_quantiles`], also the reference its vector
/// path is pinned against.
pub(super) fn normal_quantiles_scalar(words: &[u64], out: &mut [f64]) {
    for (word, z) in words.iter().zip(out.iter_mut()) {
        *z = normal_quantile(open_unit(*word));
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn normal_quantiles_avx2(words: &[u64], out: &mut [f64]) {
    use std::arch::x86_64::{
        _CMP_LE_OQ, _mm256_add_pd, _mm256_andnot_pd, _mm256_cmp_pd, _mm256_div_pd, _mm256_loadu_pd,
        _mm256_movemask_pd, _mm256_mul_pd, _mm256_set1_pd, _mm256_storeu_pd, _mm256_sub_pd,
    };
    let (word_chunks, word_rest) = words.as_chunks::<4>();
    let (out_chunks, out_rest) = out.as_chunks_mut::<4>();
    let half = _mm256_set1_pd(0.5);
    let sign = _mm256_set1_pd(-0.0);
    let cut = _mm256_set1_pd(0.425);
    let r0 = _mm256_set1_pd(0.180_625);
    for (lane_words, lane_out) in word_chunks.iter().zip(out_chunks.iter_mut()) {
        // The uniform is built in scalar code: AVX2 has no exact unsigned
        // 64-bit to double conversion, and `open_unit` is exact anyway.
        let p = lane_words.map(open_unit);
        // SAFETY: `p` and `lane_out` are four `f64`s each; the unaligned
        // load and store need no alignment.
        let pv = unsafe { _mm256_loadu_pd(p.as_ptr()) };
        let q = _mm256_sub_pd(pv, half);
        let central = _mm256_cmp_pd::<_CMP_LE_OQ>(_mm256_andnot_pd(sign, q), cut);
        let r = _mm256_sub_pd(r0, _mm256_mul_pd(q, q));
        let horner = |coefficients: &[f64; 8]| {
            let mut acc = _mm256_set1_pd(coefficients[0]);
            for c in &coefficients[1..] {
                acc = _mm256_add_pd(_mm256_mul_pd(acc, r), _mm256_set1_pd(*c));
            }
            acc
        };
        let z = _mm256_div_pd(_mm256_mul_pd(q, horner(&CENTRAL_NUM)), horner(&CENTRAL_DEN));
        // SAFETY: as for the load.
        unsafe { _mm256_storeu_pd(lane_out.as_mut_ptr(), z) };
        let mask = _mm256_movemask_pd(central);
        if mask != 0b1111 {
            for (lane, value) in lane_out.iter_mut().enumerate() {
                if mask & (1 << lane) == 0 {
                    *value = normal_quantile(p[lane]);
                }
            }
        }
    }
    normal_quantiles_scalar(word_rest, out_rest);
}

/// `log2(e)`, correctly rounded.
const LOG2E: f64 = std::f64::consts::LOG2_E;
/// The round-to-nearest-even shifter: adding it to a value of magnitude
/// below `2^51` leaves the nearest integer in the low mantissa bits.
const SHIFT: f64 = 6_755_399_441_055_744.0;
/// `ln 2` split Cody-Waite style: `LN2_HI` is fdlibm's, 32 significant bits,
/// so `n * LN2_HI` is exact for every `|n| < 2^21`; `LN2_LO` carries the rest.
const LN2_HI: f64 = f64::from_bits(0x3FE6_2E42_FEE0_0000);
const LN2_LO: f64 = f64::from_bits(0x3DEA_39EF_3579_3C76);
/// The domain [`tape_exp`] accepts. Its `n` stays inside `[-1010, 1010]`, so
/// `2^n` and the result are both normal.
pub(super) const EXP_DOMAIN: std::ops::RangeInclusive<f64> = -700.0..=700.0;

/// The degree-13 Taylor coefficients `1 / k!`, highest power first. On
/// `|r| <= ln(2) / 2` the truncation is below `4e-18` relative.
const EXP_TAYLOR: [f64; 14] = {
    let mut c = [0.0; 14];
    let mut k = 0;
    let mut factorial = 1.0;
    while k < 14 {
        if k > 0 {
            factorial *= k as f64;
        }
        c[13 - k] = 1.0 / factorial;
        k += 1;
    }
    c
};

/// `e^x` for `x` in [`EXP_DOMAIN`], refusing anything else (NaN included):
/// a reduction to `x = n ln 2 + r` with `n` rounded to nearest even, a
/// degree-13 Taylor polynomial in `r`, and `2^n` built from its exponent
/// bits. Within a few ulp of the mathematical value; the same bits as
/// [`tape_exps`] gives for `x` in any lane.
pub(super) fn tape_exp(x: f64) -> f64 {
    assert!(EXP_DOMAIN.contains(&x), "tape_exp outside its domain: {x}");
    let t = x * LOG2E + SHIFT;
    let n_f = t - SHIFT;
    let n = t.to_bits() as i64 - SHIFT.to_bits() as i64;
    let r = (x - n_f * LN2_HI) - n_f * LN2_LO;
    // Written out rather than looped: the same Horner tree as the vector
    // kernel's loop, and an unoptimized build's iterator cost is gone.
    let c = &EXP_TAYLOR;
    let p = ((((((((((((c[0] * r + c[1]) * r + c[2]) * r + c[3]) * r + c[4]) * r + c[5]) * r
        + c[6])
        * r
        + c[7])
        * r
        + c[8])
        * r
        + c[9])
        * r
        + c[10])
        * r
        + c[11])
        * r
        + c[12])
        * r
        + c[13];
    p * f64::from_bits(((n + 1023) as u64) << 52)
}

/// [`tape_exp`] of every value in place.
pub(super) fn tape_exps(xs: &mut [f64]) {
    #[cfg(target_arch = "x86_64")]
    if vector_path() {
        // SAFETY: the running CPU was just checked for AVX2, the only feature
        // the called function enables.
        unsafe { tape_exps_avx2(xs) };
        return;
    }
    tape_exps_scalar(xs);
}

/// The scalar path of [`tape_exps`].
pub(super) fn tape_exps_scalar(xs: &mut [f64]) {
    for x in xs {
        *x = tape_exp(*x);
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn tape_exps_avx2(xs: &mut [f64]) {
    use std::arch::x86_64::{
        _mm256_add_epi64, _mm256_add_pd, _mm256_castpd_si256, _mm256_castsi256_pd, _mm256_loadu_pd,
        _mm256_mul_pd, _mm256_set1_epi64x, _mm256_set1_pd, _mm256_slli_epi64, _mm256_storeu_pd,
        _mm256_sub_epi64, _mm256_sub_pd,
    };
    let (chunks, rest) = xs.as_chunks_mut::<4>();
    let log2e = _mm256_set1_pd(LOG2E);
    let shift = _mm256_set1_pd(SHIFT);
    let shift_bits = _mm256_set1_epi64x(SHIFT.to_bits() as i64);
    let bias = _mm256_set1_epi64x(1023);
    let hi = _mm256_set1_pd(LN2_HI);
    let lo = _mm256_set1_pd(LN2_LO);
    for lanes in chunks {
        for x in lanes.iter() {
            assert!(EXP_DOMAIN.contains(x), "tape_exp outside its domain: {x}");
        }
        // SAFETY: `lanes` is four `f64`s; the unaligned load and store need
        // no alignment.
        let x = unsafe { _mm256_loadu_pd(lanes.as_ptr()) };
        let t = _mm256_add_pd(_mm256_mul_pd(x, log2e), shift);
        let n_f = _mm256_sub_pd(t, shift);
        let n = _mm256_sub_epi64(_mm256_castpd_si256(t), shift_bits);
        let r = _mm256_sub_pd(
            _mm256_sub_pd(x, _mm256_mul_pd(n_f, hi)),
            _mm256_mul_pd(n_f, lo),
        );
        let mut p = _mm256_set1_pd(EXP_TAYLOR[0]);
        for c in &EXP_TAYLOR[1..] {
            p = _mm256_add_pd(_mm256_mul_pd(p, r), _mm256_set1_pd(*c));
        }
        let scale = _mm256_castsi256_pd(_mm256_slli_epi64::<52>(_mm256_add_epi64(n, bias)));
        // SAFETY: as for the load.
        unsafe { _mm256_storeu_pd(lanes.as_mut_ptr(), _mm256_mul_pd(p, scale)) };
    }
    tape_exps_scalar(rest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TapeRng;

    /// The batch under test: the AVX2 kernel where the CPU has it, whatever
    /// the build, so a dev-build suite pins the path a release build runs.
    fn vector_quantiles(words: &[u64], out: &mut [f64]) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected.
            unsafe { normal_quantiles_avx2(words, out) };
            return;
        }
        normal_quantiles(words, out);
    }

    fn vector_exps(xs: &mut [f64]) {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected.
            unsafe { tape_exps_avx2(xs) };
            return;
        }
        tape_exps(xs);
    }

    /// Words whose uniforms bracket both central cutoffs, the midpoint and
    /// the grid's ends, then a seeded run, at every length and offset so
    /// every remainder placement and every lane is exercised.
    fn probe_words() -> Vec<u64> {
        let mut words = Vec::new();
        for target in [0.075_f64, 0.925, 0.5] {
            let centre = ((target * 4_503_599_627_370_496.0) as u64) << 12;
            for delta in 0..8_u64 {
                words.push(centre.wrapping_add(delta << 12));
                words.push(centre.wrapping_sub(delta << 12));
            }
        }
        words.extend([0, u64::MAX, 1 << 63, (1 << 63) - 1]);
        let mut rng = TapeRng::new(7, 0x7665_6374);
        words.extend((0..4_000).map(|_| rng.next_word()));
        words
    }

    #[test]
    fn batched_quantiles_are_the_scalar_bits_at_every_offset_and_length() {
        let words = probe_words();
        let mut reference = vec![0.0; words.len()];
        normal_quantiles_scalar(&words, &mut reference);
        for (word, z) in words.iter().zip(&reference) {
            assert_eq!(z.to_bits(), normal_quantile(open_unit(*word)).to_bits());
        }
        for start in 0..5 {
            for len in [0, 1, 2, 3, 4, 5, 7, 8, 9, 31, 32, 33, words.len() - start] {
                let slice = &words[start..start + len];
                let mut got = vec![0.0; len];
                vector_quantiles(slice, &mut got);
                for (g, r) in got.iter().zip(&reference[start..]) {
                    assert_eq!(g.to_bits(), r.to_bits(), "a batched quantile moved a bit");
                }
            }
        }
    }

    /// The inputs a reduction or a power of two can trip on: the domain's
    /// ends and zero, and the representable neighbours of every point where
    /// the rounded `x * log2(e)` changes integer and where `e^x` crosses a
    /// power of two.
    fn exp_probes() -> Vec<f64> {
        let mut xs = vec![0.0, -0.0, -700.0, 700.0];
        for k in -1010_i32..=1010 {
            let ln2k = f64::from(k) * std::f64::consts::LN_2;
            for centre in [ln2k, ln2k + 0.5 * std::f64::consts::LN_2] {
                let mut below = centre;
                let mut above = centre;
                for _ in 0..3 {
                    for x in [below, above] {
                        if EXP_DOMAIN.contains(&x) {
                            xs.push(x);
                        }
                    }
                    below = below.next_down();
                    above = above.next_up();
                }
            }
        }
        let mut rng = TapeRng::new(11, 0x0065_7870);
        xs.extend((0..20_000).map(|_| {
            let u = super::super::children::half_open_unit(rng.next_word());
            -700.0 + 1400.0 * u
        }));
        xs.extend((0..20_000).map(|_| {
            let u = super::super::children::half_open_unit(rng.next_word());
            -10.0 + 20.0 * u
        }));
        xs
    }

    fn ulps(a: f64, b: f64) -> u64 {
        (a.to_bits() as i64 - b.to_bits() as i64).unsigned_abs()
    }

    /// A differential gate, not an accuracy proof: `f64::exp`'s precision
    /// is unspecified, so this pins agreement with the platform's `exp`,
    /// while `tape_exp_meets_its_reference_values` pins the kernel against
    /// values worked to more digits than a double holds.
    #[test]
    fn tape_exp_stays_within_two_ulp_of_the_platform_exp() {
        let mut worst = 0;
        for x in exp_probes() {
            let got = tape_exp(x);
            assert!(got.is_finite() && got > 0.0, "tape_exp({x}) = {got}");
            worst = worst.max(ulps(got, x.exp()));
        }
        assert!(
            worst <= 2,
            "tape_exp drifted {worst} ulp from the platform exp"
        );
    }

    #[test]
    #[expect(
        clippy::excessive_precision,
        clippy::approx_constant,
        reason = "reference values written to more digits than a double holds, on purpose"
    )]
    fn tape_exp_meets_its_reference_values() {
        // e^x to 20 significant digits, worked independently of any f64 exp.
        let cases: [(f64, f64); 6] = [
            (0.0, 1.0),
            (1.0, 2.718_281_828_459_045_235_4),
            (-1.0, 0.367_879_441_171_442_321_6),
            (0.5, 1.648_721_270_700_128_146_8),
            (10.0, 22_026.465_794_806_716_517),
            (-10.0, 4.539_992_976_248_485_154e-5),
        ];
        for (x, want) in cases {
            let got = tape_exp(x);
            assert!(ulps(got, want) <= 2, "tape_exp({x}) = {got}, want {want}");
        }
    }

    #[test]
    fn batched_exps_are_the_scalar_bits_at_every_offset_and_length() {
        let xs = exp_probes();
        let reference: Vec<u64> = xs.iter().map(|x| tape_exp(*x).to_bits()).collect();
        for start in 0..5 {
            for len in [0, 1, 3, 4, 5, 8, 9, 33, xs.len() - start] {
                let mut got = xs[start..start + len].to_vec();
                vector_exps(&mut got);
                for (g, r) in got.iter().zip(&reference[start..]) {
                    assert_eq!(g.to_bits(), *r, "a batched exp moved a bit");
                }
            }
        }
    }

    /// The size law's transform three ways over 16M words, in chunks of
    /// [`super::super::children::SIZE_BATCH`] as decoration runs it: the
    /// tape-40 chain (scalar quantile, platform `exp`), the scalar chain on
    /// [`tape_exp`], and the batches. Run with `--release`; it prints walls
    /// and asserts nothing but agreement of the last two.
    #[test]
    #[ignore = "bench: a wall measurement, meaningful only in a release build"]
    fn size_transform_walls() {
        const N: usize = 1 << 24;
        const CHUNK: usize = super::super::children::SIZE_BATCH;
        let mut rng = TapeRng::new(3, 0x0062_656E);
        let mut words = vec![0_u64; N];
        rng.fill(&mut words);
        let (mu, sigma) = (-5.0, 1.4);
        let time = |label: &str, f: &dyn Fn(&[u64], &mut [f64])| {
            let mut out = vec![0.0; N];
            let start = std::time::Instant::now();
            for (w, o) in words.chunks(CHUNK).zip(out.chunks_mut(CHUNK)) {
                f(w, o);
            }
            let wall = start.elapsed();
            let sum: f64 = out.iter().sum();
            println!("{label}: {wall:?} for {N} sizes, sum {sum}");
            out
        };
        time("tape 40, scalar quantile and platform exp", &|w, o| {
            for (word, x) in w.iter().zip(o.iter_mut()) {
                *x = (mu + sigma * normal_quantile(open_unit(*word))).exp();
            }
        });
        let scalar = time("scalar quantile and tape_exp", &|w, o| {
            for (word, x) in w.iter().zip(o.iter_mut()) {
                *x = tape_exp(mu + sigma * normal_quantile(open_unit(*word)));
            }
        });
        let batched = time("batched", &|w, o| {
            vector_quantiles(w, o);
            for x in o.iter_mut() {
                *x = mu + sigma * *x;
            }
            vector_exps(o);
        });
        assert!(
            scalar
                .iter()
                .zip(&batched)
                .all(|(a, b)| a.to_bits() == b.to_bits())
        );
    }

    fn vector_ln4(xs: [f64; 4]) -> [f64; 4] {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            // SAFETY: AVX2 was just detected.
            return unsafe { tape_ln4_avx2(xs) };
        }
        tape_ln4(xs)
    }

    /// The inputs the logarithm can trip on: neighbours of every power of
    /// two the bounce walks reach and of the `sqrt(2)` normalization cutoff
    /// in each binade, the values just below one, the reachable minimum
    /// `2^-53` and minimum product `2^-106`, one itself, and seeded open
    /// uniforms and products of two.
    fn ln_probes() -> Vec<f64> {
        let mut xs = vec![1.0, 2.0_f64.powi(-53), 2.0_f64.powi(-106)];
        for e in -106..=1 {
            let power = 2.0_f64.powi(e);
            for centre in [power, power * std::f64::consts::SQRT_2] {
                let (mut below, mut above) = (centre, centre);
                for _ in 0..3 {
                    xs.extend([below, above]);
                    below = below.next_down();
                    above = above.next_up();
                }
            }
        }
        let mut below_one = 1.0_f64;
        for _ in 0..8 {
            below_one = below_one.next_down();
            xs.push(below_one);
        }
        let mut rng = TapeRng::new(5, 0x006C_6E00);
        for _ in 0..20_000 {
            let u = open_unit(rng.next_word());
            xs.extend([u, u * open_unit(rng.next_word())]);
        }
        xs.retain(|x| x.is_normal() && *x <= 1.0);
        xs
    }

    #[test]
    fn four_lane_logs_are_the_scalar_bits_in_every_lane() {
        let xs = ln_probes();
        for window in xs.windows(4) {
            let lanes = [window[0], window[1], window[2], window[3]];
            for rotation in 0..4 {
                let mut rotated = lanes;
                rotated.rotate_left(rotation);
                let got = vector_ln4(rotated);
                for (g, x) in got.iter().zip(rotated) {
                    assert_eq!(
                        g.to_bits(),
                        tape_ln(x).to_bits(),
                        "a vector log moved a bit at {x}"
                    );
                }
            }
        }
    }

    /// A differential gate against the platform's `ln`, plus the sign
    /// contract the t denominator rests on and monotonicity over the sorted
    /// probes.
    #[test]
    fn tape_ln_stays_within_one_ulp_and_keeps_its_sign() {
        assert_eq!(tape_ln(1.0).to_bits(), 0.0_f64.to_bits());
        let mut xs = ln_probes();
        xs.sort_by(f64::total_cmp);
        xs.dedup();
        let mut worst = 0;
        let mut previous = f64::NEG_INFINITY;
        for x in xs {
            let got = tape_ln(x);
            worst = worst.max(ulps(got, x.ln()));
            if x < 1.0 {
                assert!(got < 0.0, "tape_ln({x}) = {got} is not negative");
            }
            assert!(got >= previous, "tape_ln is not monotone at {x}");
            previous = got;
        }
        assert!(
            worst <= 1,
            "tape_ln drifted {worst} ulp from the platform ln"
        );
    }

    #[test]
    #[expect(
        clippy::excessive_precision,
        clippy::approx_constant,
        reason = "reference values written to more digits than a double holds, on purpose"
    )]
    fn tape_ln_meets_its_reference_values() {
        // ln x to 20 significant digits, worked independently of any f64 ln.
        let cases: [(f64, f64); 4] = [
            (0.5, -0.693_147_180_559_945_309_42),
            (0.1, -2.302_585_092_994_045_684_0),
            (1e-10, -23.025_850_929_940_456_840),
            (0.75, -0.287_682_072_451_780_927_44),
        ];
        for (x, want) in cases {
            let got = tape_ln(x);
            assert!(ulps(got, want) <= 1, "tape_ln({x}) = {got}, want {want}");
        }
    }

    /// The legacy packet's three random-input logs over 16M packets: tape
    /// 41's three platform `ln` calls, three scalar `tape_ln`, and one
    /// four-lane call. Run with `--release`; it prints walls.
    #[test]
    #[ignore = "bench: a wall measurement, meaningful only in a release build"]
    fn packet_log_walls() {
        const N: usize = 1 << 24;
        let mut rng = TapeRng::new(9, 0x0070_6B74);
        let mut words = vec![0_u64; 4 * N];
        rng.fill(&mut words);
        let inputs = |w: &[u64]| {
            [
                open_unit(w[0]),
                open_unit(w[1]) * open_unit(w[2]),
                open_unit(w[3]),
                1.0,
            ]
        };
        let time = |label: &str, f: &dyn Fn([f64; 4]) -> [f64; 4]| {
            let start = std::time::Instant::now();
            let mut sum = 0.0;
            for w in words.as_chunks::<4>().0 {
                let [a, b, c, _] = f(inputs(w));
                sum += a + b + c;
            }
            println!("{label}: {:?} for {N} packets, sum {sum}", start.elapsed());
        };
        time("platform ln, three calls", &|x| {
            [x[0].ln(), x[1].ln(), x[2].ln(), 0.0]
        });
        time("scalar tape_ln, three calls", &|x| {
            [tape_ln(x[0]), tape_ln(x[1]), tape_ln(x[2]), 0.0]
        });
        time("tape_ln4", &vector_ln4);
    }

    #[test]
    #[should_panic(expected = "outside its domain")]
    fn tape_ln_refuses_zero() {
        tape_ln(0.0);
    }

    #[test]
    #[should_panic(expected = "outside its domain")]
    fn tape_exp_refuses_nan() {
        tape_exp(f64::NAN);
    }

    #[test]
    #[should_panic(expected = "outside its domain")]
    fn batched_exp_refuses_past_its_domain() {
        vector_exps(&mut [0.0, 0.0, 0.0, 700.5]);
    }
}
