// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The bounce walks' per-parent main-stream draws (tape protocol 37): a fixed
//! number of raw `u64` words per parent, in a fixed slot order, each turned
//! into its variate by a pure function of its own words.
//!
//! Before 37 a parent drew a variable amount - the ziggurat normal and the
//! gamma behind the chi-squared reject, the child count took one uniform or
//! two, the drift coins were drawn only in the high regime, the repeat coin
//! only for a single-child low-regime parent, and the level-step gaps varied
//! with the sweep - so where parent `k`'s draws began was known only by
//! running parents `0` to `k - 1`, and the main stream's consumption could
//! not be batched. Now every slot is drawn whether its branch is taken or
//! not, the level steps have a stream of their own ([`super::children::
//! StepStream`]), and every transform here is free of rejection.
//!
//! The words are drawn up front; their results are applied in the walk's
//! causal order, which is unchanged: the gap reads the opening clock and the
//! current arrival state, the state flip lands after `last_quiet` is
//! recorded, the latent mid reads the printing clock, the regime moves before
//! the side flip reads its probability, the drift reads the new regime, the
//! count reads `last_quiet`, the repeat reads the new regime.
//!
//! The laws are the old laws in the continuous limit, not on the finite grid:
//! the exponential gap loses the old atom at zero and is capped near 36.7;
//! a coin's probability is quantized to `2^-53` instead of the integer
//! threshold's `2^-64`; the t(4) innovation's numerator and denominator are
//! both discretized by the 52-bit uniform grid, so its support is bounded.
//! None of that is visible to the statistical gates, which are what judge
//! it.
//!
//! The cascade's own fixed-word samplers (tape protocol 39) live here too:
//! the integer-degree t ([`student_t_integer`]) and the Poisson count by
//! inversion of one uniform ([`poisson_inverse`]). Which words the cascade
//! hands them, and from which domain, is the cascade module's business.

use super::children::{half_open_unit, normal_quantile, open_unit};
use super::consts::{ARRIVAL_WEIBULL_SHAPE, CHILD_CAP, STUDENT_T_DF};
use super::dynamics::SweepShape;

// The closed forms below are specific to these two constants: an exponential
// gap is a unit Weibull only at shape one, and chi-squared is `-2 ln` of a
// product of two uniforms only at four degrees of freedom.
const _: () = assert!(ARRIVAL_WEIBULL_SHAPE == 1.0);
const _: () = assert!(STUDENT_T_DF == 4.0);

/// The legacy walk's slots, in draw order: `LegacyDraws::draw`'s order, and
/// what the conformance vectors and the stride pins check it against.
#[cfg(test)]
pub(super) const LEGACY_SLOTS: [&str; 12] = [
    "gap",
    "state_flip",
    "innovation_normal",
    "innovation_chi_a",
    "innovation_chi_b",
    "regime",
    "side_flip",
    "drift_direction",
    "drift",
    "count_branch",
    "count_geometric",
    "repeat",
];

/// The integrated walk's slots, in draw order: its clock and child count
/// are the arrival kernel's, on the cadence stream, and it has no repeat.
#[cfg(test)]
pub(super) const INTEGRATED_SLOTS: [&str; 7] = [
    "innovation_normal",
    "innovation_chi_a",
    "innovation_chi_b",
    "regime",
    "side_flip",
    "drift_direction",
    "drift",
];

/// One parent's raw legacy words.
#[derive(Clone, Copy)]
pub(super) struct LegacyDraws {
    pub(super) gap: u64,
    pub(super) state_flip: u64,
    pub(super) innovation: [u64; 3],
    pub(super) bounce: BounceDraws,
    pub(super) count_branch: u64,
    pub(super) count_geometric: u64,
    pub(super) repeat: u64,
}

/// The four bounce words both walks draw.
#[derive(Clone, Copy)]
pub(super) struct BounceDraws {
    pub(super) regime: u64,
    pub(super) side_flip: u64,
    pub(super) drift_direction: u64,
    pub(super) drift: u64,
}

impl LegacyDraws {
    /// The parent's twelve words, in slot order.
    pub(super) fn draw(mut next: impl FnMut() -> u64) -> Self {
        let gap = next();
        let state_flip = next();
        let innovation = [next(), next(), next()];
        let bounce = BounceDraws::draw(&mut next);
        Self {
            gap,
            state_flip,
            innovation,
            bounce,
            count_branch: next(),
            count_geometric: next(),
            repeat: next(),
        }
    }
}

impl BounceDraws {
    pub(super) fn draw(mut next: impl FnMut() -> u64) -> Self {
        Self {
            regime: next(),
            side_flip: next(),
            drift_direction: next(),
            drift: next(),
        }
    }
}

/// A unit exponential: `-ln` of an open uniform.
pub(super) fn unit_exponential(raw: u64) -> f64 {
    -open_unit(raw).ln()
}

/// A coin landing with probability `p`: a half-open uniform below it. Zero
/// never lands and one always does.
pub(super) fn coin(raw: u64, p: f64) -> bool {
    half_open_unit(raw) < p
}

/// A raw, unstandardized t(4) variate: a normal over the root of an
/// independent chi-squared(4) over four, the chi-squared being
/// `-2 ln(u1 u2)`. The expression is fixed - `ln(u1 u2)` and
/// `ln(u1) + ln(u2)` need not agree in the last bit - so a later batched
/// transform must evaluate exactly this.
pub(super) fn student_t4(words: [u64; 3]) -> f64 {
    let normal = normal_quantile(open_unit(words[0]));
    let chi_over_df = -0.5 * (open_unit(words[1]) * open_unit(words[2])).ln();
    normal / chi_over_df.sqrt()
}

/// A raw, unstandardized t variate with an integer `nu` degrees of freedom
/// (tape protocol 39, the cascade): a normal quantile over the root of an
/// independent chi-squared(`nu`) over `nu`. The chi-squared is `-2` times
/// the sum of the logs of `nu / 2` open uniforms - each `-2 ln u` is a
/// chi-squared with two degrees - plus, for an odd `nu`, the square of one
/// more normal quantile. Summing logarithms rather than multiplying uniforms
/// keeps a large `nu` from underflowing the product. `chi_words` holds
/// exactly `nu / 2` words and `odd_word` is present exactly when `nu` is odd.
pub(super) fn student_t_integer(numerator: u64, chi_words: &[u64], odd_word: Option<u64>) -> f64 {
    let mut chi = -2.0 * chi_words.iter().map(|w| open_unit(*w).ln()).sum::<f64>();
    let mut nu = 2 * chi_words.len();
    if let Some(word) = odd_word {
        let z = normal_quantile(open_unit(word));
        chi += z * z;
        nu += 1;
    }
    normal_quantile(open_unit(numerator)) / (chi.max(f64::MIN_POSITIVE) / nu as f64).sqrt()
}

/// The Poisson draws' ceiling: `poisson_inverse` returns
/// `min(Poisson(lambda), POISSON_CEILING)`, the law the rejection sampler it
/// replaced had through its clamp.
pub(super) const POISSON_CEILING: f64 = 1_000_000.0;

/// `POISSON_CEILING + 12 sqrt(POISSON_CEILING)`. At or above this rate the
/// capped law is the ceiling itself to within its lower tail, and the
/// Chernoff bound `P(X <= c) <= exp(-lambda + c + c ln(lambda / c))` puts
/// that tail below `1e-31` here and lower for any larger rate. So returning
/// the ceiling changes the ideal capped law by less than `1e-30` in total
/// variation - a bound on this shortcut against the exact law, not on the
/// helper's floating-point error elsewhere.
const POISSON_SATURATED: f64 = 1_012_000.0;

/// Below this rate the search runs up from zero; at or above it, outward
/// from the mode. The two represent the same law; their floating-point
/// partitions of the unit interval differ, which no seeded outcome is owed.
const POISSON_SMALL: f64 = 30.0;

/// `min(Poisson(lambda), POISSON_CEILING)` by inversion of one uniform in
/// `[0, 1)`: the count is the first outcome, in a fixed enumeration order,
/// whose cumulative probability exceeds `u`. Any fixed order is an exact
/// sampler; below `POISSON_SMALL` the order is `0, 1, 2, ...`, above it the
/// mode `m` then `m - 1, m + 1, m - 2, ...`, so the search is about
/// `sqrt(lambda)` long rather than `lambda`. Zero for a rate that is not
/// finite and positive.
///
/// A tail is retired once its next probability falls below
/// `f64::MIN_POSITIVE`, so the search never crawls through subnormals; if
/// every tail is retired with the cumulative still at or below `u` - the
/// floating-point deficit of the starting probability, the recurrences and
/// the sum - the count is the mode. That deficit is one small atom on the
/// mode, the residual rule's only meaning on either path.
pub(super) fn poisson_inverse(u: f64, lambda: f64) -> u32 {
    if !lambda.is_finite() || lambda <= 0.0 {
        return 0;
    }
    if lambda >= POISSON_SATURATED {
        return POISSON_CEILING as u32;
    }
    let mode = lambda.floor() as u64;
    let count = if lambda < POISSON_SMALL {
        poisson_from_zero(u, lambda, mode)
    } else {
        poisson_from_mode(u, lambda, mode)
    };
    count.min(POISSON_CEILING as u64) as u32
}

/// A Neumaier-compensated running sum, so a long search's cumulative
/// probability does not drift by the rounding of every addition.
struct CompensatedSum {
    sum: f64,
    carry: f64,
}

impl CompensatedSum {
    fn new() -> Self {
        Self {
            sum: 0.0,
            carry: 0.0,
        }
    }

    fn add(&mut self, value: f64) -> f64 {
        let next = self.sum + value;
        self.carry += if self.sum.abs() >= value.abs() {
            (self.sum - next) + value
        } else {
            (value - next) + self.sum
        };
        self.sum = next;
        self.sum + self.carry
    }
}

fn poisson_from_zero(u: f64, lambda: f64, mode: u64) -> u64 {
    let mut cumulative = CompensatedSum::new();
    let mut p = (-lambda).exp();
    let mut k = 0_u64;
    loop {
        if cumulative.add(p) > u {
            return k;
        }
        k += 1;
        p *= lambda / k as f64;
        if p < f64::MIN_POSITIVE {
            return mode;
        }
    }
}

fn poisson_from_mode(u: f64, lambda: f64, mode: u64) -> u64 {
    let mut cumulative = CompensatedSum::new();
    let at_mode = poisson_pmf_loader(mode, lambda);
    if cumulative.add(at_mode) > u {
        return mode;
    }
    let (mut low, mut p_low) = (mode, at_mode);
    let (mut high, mut p_high) = (mode, at_mode);
    loop {
        if p_low > 0.0 {
            if low == 0 {
                p_low = 0.0;
            } else {
                p_low *= low as f64 / lambda;
                low -= 1;
                if p_low < f64::MIN_POSITIVE {
                    p_low = 0.0;
                } else if cumulative.add(p_low) > u {
                    return low;
                }
            }
        }
        if p_high > 0.0 {
            high += 1;
            p_high *= lambda / high as f64;
            if p_high < f64::MIN_POSITIVE {
                p_high = 0.0;
            } else if cumulative.add(p_high) > u {
                return high;
            }
        }
        if p_low == 0.0 && p_high == 0.0 {
            return mode;
        }
    }
}

/// The Poisson probability of `x >= 30` at rate `lambda`, in Loader's
/// saddle-point form `exp(-stirlerr(x) - bd0(x, lambda)) / sqrt(2 pi x)`,
/// which never subtracts two large log-gammas.
fn poisson_pmf_loader(x: u64, lambda: f64) -> f64 {
    let x = x as f64;
    (-stirling_error(x) - deviance_term(x, lambda)).exp() / (std::f64::consts::TAU * x).sqrt()
}

/// `ln(x!) - (x + 1/2) ln x + x - ln sqrt(2 pi)` for `x >= 30`, by the
/// Stirling series through the `x^-9` term, accurate far below a part in
/// `1e16` there.
fn stirling_error(x: f64) -> f64 {
    const S0: f64 = 1.0 / 12.0;
    const S1: f64 = 1.0 / 360.0;
    const S2: f64 = 1.0 / 1260.0;
    const S3: f64 = 1.0 / 1680.0;
    const S4: f64 = 1.0 / 1188.0;
    let xx = x * x;
    (S0 - (S1 - (S2 - (S3 - S4 / xx) / xx) / xx) / xx) / x
}

/// `x ln(x / lambda) + lambda - x`, by its series when `x` is near `lambda`,
/// where the direct form cancels.
fn deviance_term(x: f64, lambda: f64) -> f64 {
    if (x - lambda).abs() < 0.1 * (x + lambda) {
        let mut v = (x - lambda) / (x + lambda);
        let mut sum = (x - lambda) * v;
        let mut term = 2.0 * x * v;
        v *= v;
        for j in 1..1000 {
            term *= v;
            let next = sum + term / f64::from(2 * j + 1);
            if next == sum {
                return next;
            }
            sum = next;
        }
        sum
    } else {
        x * (x / lambda).ln() + lambda - x
    }
}

/// The child-count mixture from its two words: below `q` one child, else
/// the geometric on `1, 2, ...` with mean `m`, capped at `CHILD_CAP` with
/// the truncation counted. `SweepShape::next_count` draws the same mixture
/// from the main stream for the paths that still do.
pub(super) fn mixture_count(shape: &mut SweepShape, branch: u64, geometric: u64) -> u32 {
    shape.drawn += 1;
    if half_open_unit(branch) < shape.q {
        return 1;
    }
    let raw = 1.0 + open_unit(geometric).ln() / shape.ln_continue();
    let count = raw.floor().max(1.0) as u64;
    if count > u64::from(CHILD_CAP) {
        shape.truncated += 1;
        CHILD_CAP
    } else {
        count as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The capped Poisson law at `lambda`, computed independently of the
    /// sampler: each probability from `-lambda + k ln(lambda) - ln(k!)`, the
    /// factorial's log summed term by term, with everything at or past the
    /// ceiling folded onto it. Only the outcomes with mass worth checking
    /// are kept.
    fn reference_law(lambda: f64) -> std::collections::BTreeMap<u64, f64> {
        let ceiling = POISSON_CEILING as u64;
        let mut law = std::collections::BTreeMap::new();
        let mut ln_factorial = 0.0;
        let mut tail = 0.0;
        let last = (lambda + 40.0 * lambda.sqrt() + 40.0) as u64;
        for k in 0..=last {
            if k > 0 {
                ln_factorial += (k as f64).ln();
            }
            let p = (-lambda + k as f64 * lambda.ln() - ln_factorial).exp();
            if k >= ceiling {
                tail += p;
            } else if p > 1e-15 {
                law.insert(k, p);
            }
        }
        if tail > 0.0 {
            law.insert(ceiling, tail);
        }
        law
    }

    /// The sampler partitions the unit interval into one piece per outcome,
    /// each as long as that outcome's probability, in its own enumeration
    /// order. Over an even grid of `n` uniforms, an outcome below the ceiling
    /// is therefore hit `n * p(k)` times to within the grid's resolution - a
    /// check on the whole law at once, both search orders and both sides of
    /// the threshold between them. The ceiling is different: under the
    /// mode-outward order it is every outcome at or past it, many separate
    /// pieces whose grid errors add, so it is held to a bound scaled to
    /// that rather than to one piece's resolution.
    #[test]
    fn poisson_inversion_partitions_the_unit_interval_by_the_law() {
        let cases = [
            (0.2, 1 << 20),
            (3.0, 1 << 20),
            (POISSON_SMALL.next_down(), 1 << 20),
            (POISSON_SMALL, 1 << 20),
            (POISSON_SMALL.next_up(), 1 << 20),
            (250.5, 1 << 20),
            (POISSON_CEILING, 1 << 16),
        ];
        for (lambda, n) in cases {
            let mut hits = std::collections::BTreeMap::<u64, u64>::new();
            for i in 0..n {
                let u = i as f64 / n as f64;
                *hits
                    .entry(u64::from(poisson_inverse(u, lambda)))
                    .or_default() += 1;
            }
            let law = reference_law(lambda);
            let ceiling = POISSON_CEILING as u64;
            if let Some(p) = law.get(&ceiling) {
                // Every piece past the ceiling is at most one grid point off,
                // and they number about `40 sqrt(lambda)` with mass; the
                // reference's own summed log-factorials add their part.
                let expected = p * n as f64;
                let count = hits.get(&ceiling).copied().unwrap_or(0) as f64;
                assert!(
                    (count - expected).abs() <= 40.0 + 1e-3 * expected,
                    "lambda {lambda} ceiling: {count} hits against {expected}"
                );
            }
            for (k, count) in hits.iter().filter(|(k, _)| **k != ceiling) {
                let expected = law.get(k).copied().unwrap_or(0.0) * n as f64;
                assert!(
                    (*count as f64 - expected).abs() <= 2.0 + 1e-4 * expected,
                    "lambda {lambda} outcome {k}: {count} hits against {expected}"
                );
            }
            for (k, p) in law.iter().filter(|(k, _)| **k != ceiling) {
                let expected = p * n as f64;
                if expected >= 4.0 {
                    let count = hits.get(k).copied().unwrap_or(0) as f64;
                    assert!(
                        (count - expected).abs() <= 2.0 + 1e-4 * expected,
                        "lambda {lambda} outcome {k}: {count} hits against {expected}"
                    );
                }
            }
        }
    }

    /// The edges: no rate, a bad rate, the saturated shortcut, and the
    /// extreme uniforms on both search paths, which must all terminate.
    #[test]
    fn poisson_inversion_handles_its_edges() {
        for lambda in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert_eq!(poisson_inverse(0.5, lambda), 0, "{lambda}");
        }
        let ceiling = POISSON_CEILING as u32;
        assert_eq!(poisson_inverse(0.0, POISSON_SATURATED), ceiling);
        assert_eq!(poisson_inverse(0.0, 1e300), ceiling);
        // The strict comparison: a uniform of zero takes the first outcome
        // in each order, zero from below and the mode from above.
        assert_eq!(poisson_inverse(0.0, 3.0), 0);
        assert_eq!(poisson_inverse(0.0, 250.5), 250);
        // A uniform past every computed mass falls to the residual rule,
        // which is the mode on both paths.
        assert_eq!(poisson_from_zero(1.5, 3.0, 3), 3);
        assert_eq!(poisson_from_mode(1.5, 250.5, 250), 250);
        let top = 1.0 - f64::EPSILON / 2.0;
        for lambda in [0.2, 29.0, 30.0, 250.5, 1e5, POISSON_SATURATED.next_down()] {
            for u in [0.0, top] {
                let k = poisson_inverse(u, lambda);
                assert!(
                    f64::from(k) <= lambda + 60.0 * lambda.sqrt() + 60.0,
                    "lambda {lambda} u {u}: {k}"
                );
            }
        }
    }

    /// The integer-degree t is symmetric with the variance `nu / (nu - 2)`,
    /// for an even and an odd `nu` - the odd one reading its extra normal.
    #[test]
    fn integer_t_has_its_variance() {
        let mut rng = crate::TapeRng::new(17, 0);
        for nu in [9_usize, 10] {
            let draws = 2_000_000;
            let (mut sum_sq, mut positive) = (0.0, 0_u32);
            for _ in 0..draws {
                let numerator = rng.next_word();
                let chi: Vec<u64> = (0..nu / 2).map(|_| rng.next_word()).collect();
                let odd = (nu % 2 == 1).then(|| rng.next_word());
                let t = student_t_integer(numerator, &chi, odd);
                sum_sq += t * t;
                positive += u32::from(t > 0.0);
            }
            let variance = sum_sq / f64::from(draws);
            let expected = nu as f64 / (nu as f64 - 2.0);
            assert!(
                (variance - expected).abs() < 0.015,
                "nu {nu}: variance {variance} against {expected}"
            );
            let share = f64::from(positive) / f64::from(draws);
            assert!(
                (share - 0.5).abs() < 0.002,
                "nu {nu}: positive share {share}"
            );
        }
    }
}
