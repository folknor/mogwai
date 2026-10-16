// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The two halves of a placed sweep's children that tape protocol 36 pulled
//! apart: the level steps, which feed back into the walk and stay on the main
//! stream as geometric gaps ([`LevelSteps`]), and the sizes, which never feed
//! back and, for a river's fixed size law, are a pure function of the seed
//! and a child's global index ([`SizeStream`]).
//!
//! The split is what lets a parent be skipped at the cost of its feedback
//! draws alone. A skipped parent - a checkpoint extension, `advance_parent`,
//! a seek dropping a parent before its target - reserves its children's
//! indices and never draws a size; a parent a reader will see has its sizes
//! filled from the size stream positioned at its first child. Both give the
//! same sizes, because a child's size draw does not depend on how its parent
//! was read.

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;

use mogwai_protocol::seeds::splitmix64;

/// The size stream's domain tag: the stream is
/// `ChaCha12Rng::seed_from_u64(splitmix64(seed ^ SIZE_STREAM_TAG))`.
const SIZE_STREAM_TAG: u64 = 0x7369_7A65_7331_3336;

/// 32-bit words each child reads from the size stream: two `u64`, the size
/// uniform and the snap uniform, always both, so child `i` starts at word
/// `4 * i` whatever the snap probability.
const WORDS_PER_CHILD: u128 = 4;

/// A uniform strictly inside `(0, 1)` from one raw `u64`: the top 52 bits
/// plus a half, scaled by 2^-52. The largest input lands on `1 - 2^-53`,
/// which is exactly representable, so neither endpoint is reachable - unlike
/// a 53-bit midpoint, whose largest value rounds to exactly one.
pub(super) fn open_unit(raw: u64) -> f64 {
    ((raw >> 12) as f64 + 0.5) * (1.0 / 4_503_599_627_370_496.0)
}

/// A uniform in `[0, 1)` from one raw `u64`: the top 53 bits scaled by 2^-53.
fn half_open_unit(raw: u64) -> f64 {
    (raw >> 11) as f64 * (1.0 / 9_007_199_254_740_992.0)
}

/// The level steps of one placed sweep, drawn as geometric gaps over its
/// eligible children.
///
/// Child zero prints at the parent's opening level and never steps, so a
/// sweep of `count` children has `count - 1` trials, each stepping with the
/// same probability `p` - the probability is fixed for the parent (the bounce
/// regime changes once per parent, before the sweep, and the cascade's is
/// constant). Independent trials are equivalent in law to geometric gaps
/// between successes, so the stream draws one uniform per step plus one
/// terminal draw whose gap overshoots the remaining trials, instead of one
/// coin per child. A step landing on the last trial ends the draws without a
/// terminal one; no trials, `p <= 0` and `p >= 1` draw nothing at all. The
/// finite law differs from integer-threshold coins at the level of uniform
/// quantization, which the statistical gates, not an identity, judge.
pub(super) struct LevelSteps {
    p: f64,
    /// `ln(1 - p)`, through `ln_1p` so a small `p` does not round `1 - p`
    /// to one and destroy the denominator.
    log_q: f64,
    trials: u32,
    /// The first trial no draw has decided yet.
    cursor: u32,
    /// The trial the next step lands on, if any remains.
    next: Option<u32>,
}

impl LevelSteps {
    /// The steps of a sweep of `count` children at probability `p`, with the
    /// first gap already drawn.
    pub(super) fn new(p: f64, count: u32, rng: &mut ChaCha12Rng) -> Self {
        let mut steps = Self {
            p,
            log_q: (-p).ln_1p(),
            trials: count.saturating_sub(1),
            cursor: 0,
            next: None,
        };
        steps.advance(rng);
        steps
    }

    fn advance(&mut self, rng: &mut ChaCha12Rng) {
        self.next = None;
        if self.cursor >= self.trials || self.p <= 0.0 {
            self.cursor = self.trials;
            return;
        }
        if self.p >= 1.0 {
            self.next = Some(self.cursor);
            self.cursor += 1;
            return;
        }
        let gap = (open_unit(rng.random::<u64>()).ln() / self.log_q).floor();
        let remaining = self.trials - self.cursor;
        if gap >= f64::from(remaining) {
            self.cursor = self.trials;
            return;
        }
        let at = self.cursor + gap as u32;
        self.next = Some(at);
        self.cursor = at + 1;
    }

    /// Whether child `child` steps a level, drawing the following gap when
    /// it does. Asked once per child, in order.
    pub(super) fn steps_at(&mut self, child: u32, rng: &mut ChaCha12Rng) -> bool {
        if child == 0 || self.next != Some(child - 1) {
            return false;
        }
        self.advance(rng);
        true
    }
}

/// One child's size draw, before quantization.
#[derive(Clone, Copy)]
pub(super) struct SizeDraw {
    pub(super) base: f64,
    pub(super) lot_snap: bool,
}

/// The placed sweeps' size decoration: a dedicated stream addressed by a
/// child's global index, so a size is a pure function of the seed and the
/// index. A size is `exp(mu + sigma * z)`, `z` the normal quantile of an open
/// uniform - the lognormal law the rejection sampler drew, with its tails
/// truncated where the 52-bit uniform grid ends, near 8.3 standard
/// deviations - and the round-lot snap is a uniform below the snap fraction,
/// so a fraction of zero never snaps and one always does.
#[derive(Clone)]
pub(super) struct SizeStream {
    rng: ChaCha12Rng,
    mu: f64,
    sigma: f64,
    snap_frac: f64,
}

impl SizeStream {
    pub(super) fn new(seed: u64, median: f64, sigma: f64, snap_frac: f64) -> Self {
        Self {
            rng: ChaCha12Rng::seed_from_u64(splitmix64(seed ^ SIZE_STREAM_TAG)),
            mu: median.ln(),
            sigma,
            snap_frac,
        }
    }

    /// Position the stream at child `first`; the following `draw` calls
    /// return that child's size and its successors' in order. A stream
    /// already there is left alone: repositioning regenerates the cipher's
    /// whole buffer, which for a reader decorating consecutive parents would
    /// cost a buffer per parent rather than per few dozen children.
    pub(super) fn seek(&mut self, first: u64) {
        let word = u128::from(first) * WORDS_PER_CHILD;
        if self.rng.get_word_pos() != word {
            self.rng.set_word_pos(word);
        }
    }

    /// The next child's size draw.
    pub(super) fn draw(&mut self) -> SizeDraw {
        let z = normal_quantile(open_unit(self.rng.random::<u64>()));
        let base = (self.mu + self.sigma * z).exp().max(f64::MIN_POSITIVE);
        let lot_snap = half_open_unit(self.rng.random::<u64>()) < self.snap_frac;
        SizeDraw { base, lot_snap }
    }
}

/// The standard normal quantile, Wichura's algorithm AS 241 (PPND16), accurate
/// to about 1e-16 relative over the open unit interval.
#[expect(
    clippy::excessive_precision,
    reason = "the published AS 241 coefficients, kept as printed so they can be checked against the paper"
)]
pub(super) fn normal_quantile(p: f64) -> f64 {
    let q = p - 0.5;
    if q.abs() <= 0.425 {
        let r = 0.180_625 - q * q;
        return q
            * (((((((2.509_080_928_730_122_7e3 * r + 3.343_057_558_358_812_8e4) * r
                + 6.726_577_092_700_870_1e4)
                * r
                + 4.592_195_393_154_987_1e4)
                * r
                + 1.373_169_376_550_946_1e4)
                * r
                + 1.971_590_950_306_551_4e3)
                * r
                + 1.331_416_678_917_843_8e2)
                * r
                + 3.387_132_872_796_366_6)
            / (((((((5.226_495_278_852_854_6e3 * r + 2.872_908_573_572_194_3e4) * r
                + 3.930_789_580_009_271_1e4)
                * r
                + 2.121_379_430_158_659_6e4)
                * r
                + 5.394_196_021_424_751_1e3)
                * r
                + 6.871_870_074_920_579_1e2)
                * r
                + 4.231_333_070_160_091_1e1)
                * r
                + 1.0);
    }
    let tail = if q < 0.0 { p } else { 1.0 - p };
    let mut r = (-tail.ln()).sqrt();
    let magnitude = if r <= 5.0 {
        r -= 1.6;
        (((((((7.745_450_142_783_414e-4 * r + 2.272_384_498_926_918_5e-2) * r
            + 2.417_807_251_774_506_1e-1)
            * r
            + 1.270_458_252_452_368_4)
            * r
            + 3.647_848_324_763_204_6)
            * r
            + 5.769_497_221_460_691_4)
            * r
            + 4.630_337_846_156_545_3)
            * r
            + 1.423_437_110_749_683_6)
            / (((((((1.050_750_071_644_416_8e-9 * r + 5.475_938_084_995_344_9e-4) * r
                + 1.519_866_656_361_645_7e-2)
                * r
                + 1.481_039_764_274_800_7e-1)
                * r
                + 6.897_673_349_851_000_0e-1)
                * r
                + 1.676_384_830_183_803_8)
                * r
                + 2.053_191_626_637_758_8)
                * r
                + 1.0)
    } else {
        r -= 5.0;
        (((((((2.010_334_399_292_288_1e-7 * r + 2.711_555_568_743_487_6e-5) * r
            + 1.242_660_947_388_078_4e-3)
            * r
            + 2.653_218_952_657_612_3e-2)
            * r
            + 2.965_605_718_285_048_9e-1)
            * r
            + 1.784_826_539_917_291_3)
            * r
            + 5.463_784_911_164_114_4)
            * r
            + 6.657_904_643_501_103_8)
            / (((((((2.044_263_103_389_939_8e-15 * r + 1.421_511_758_316_445_9e-7) * r
                + 1.846_318_317_510_054_7e-5)
                * r
                + 7.868_691_311_456_132_6e-4)
                * r
                + 1.487_536_129_085_061_5e-2)
                * r
                + 1.369_298_809_227_358_1e-1)
                * r
                + 5.998_322_065_558_879_4e-1)
                * r
                + 1.0)
    };
    if q < 0.0 { -magnitude } else { magnitude }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_unit_never_reaches_either_endpoint() {
        assert!(open_unit(0) > 0.0);
        assert!(open_unit(u64::MAX) < 1.0);
        assert_eq!(half_open_unit(0), 0.0);
        assert!(half_open_unit(u64::MAX) < 1.0);
    }

    #[test]
    fn the_normal_quantile_matches_reference_points() {
        // Reference values to 15 significant digits.
        for (p, z) in [
            (0.5, 0.0),
            (0.975, 1.959_963_984_540_054),
            (0.841_344_746_068_542_9, 1.0),
            (0.001, -3.090_232_306_167_813_5),
            (1e-10, -6.361_340_902_404_056),
        ] {
            let got = normal_quantile(p);
            assert!(
                (got - z).abs() <= 1e-13 * z.abs().max(1.0),
                "quantile({p}) = {got}, expected {z}"
            );
            // `1 - p` itself rounds in the far tail, so the mirror is loose.
            assert!((normal_quantile(1.0 - p) + z).abs() <= 1e-6 * z.abs().max(1.0));
        }
        // The open uniform's extremes stay finite.
        assert!(normal_quantile(open_unit(0)).is_finite());
        assert!(normal_quantile(open_unit(u64::MAX)).is_finite());
    }

    #[test]
    fn level_steps_draw_nothing_at_the_degenerate_ends() {
        for (p, count, steps) in [(0.0, 9, 0), (1.0, 9, 8), (0.3, 1, 0), (0.3, 0, 0)] {
            let mut rng = ChaCha12Rng::seed_from_u64(7);
            let untouched = rng.clone();
            let mut level = LevelSteps::new(p, count, &mut rng);
            let taken = (0..count).filter(|&c| level.steps_at(c, &mut rng)).count();
            assert_eq!(taken, steps, "p {p} count {count}");
            assert_eq!(rng.get_word_pos(), untouched.get_word_pos(), "p {p}");
        }
    }

    /// Draw accounting: one uniform per step plus one terminal overshoot,
    /// except that a step on the last trial ends the draws.
    #[test]
    fn level_steps_draw_once_per_step_plus_the_terminal_gap() {
        for seed in 0..2_000 {
            let mut rng = ChaCha12Rng::seed_from_u64(seed);
            let start = rng.get_word_pos();
            let count = 1 + (seed % 12) as u32;
            let mut level = LevelSteps::new(0.37, count, &mut rng);
            let stepped: Vec<u32> = (0..count)
                .filter(|&c| level.steps_at(c, &mut rng))
                .collect();
            let last_stepped = stepped.last() == Some(&(count - 1));
            let draws = if count == 1 {
                0
            } else {
                stepped.len() as u128 + u128::from(!last_stepped)
            };
            assert_eq!(rng.get_word_pos() - start, 2 * draws, "seed {seed}");
            assert!(stepped.iter().all(|&c| c > 0 && c < count));
        }
    }

    /// The step frequency per eligible trial is `p`, the law the coins had.
    #[test]
    fn level_steps_land_at_the_trial_probability() {
        let mut rng = ChaCha12Rng::seed_from_u64(11);
        for p in [0.05, 0.37, 0.9] {
            let (mut trials, mut steps) = (0_u64, 0_u64);
            for _ in 0..20_000 {
                let mut level = LevelSteps::new(p, 9, &mut rng);
                steps += (0..9).filter(|&c| level.steps_at(c, &mut rng)).count() as u64;
                trials += 8;
            }
            let rate = steps as f64 / trials as f64;
            let se = (p * (1.0 - p) / trials as f64).sqrt();
            assert!((rate - p).abs() < 5.0 * se, "p {p} rate {rate}");
        }
    }

    /// A child's size depends on its index alone: drawn in sequence, or by
    /// seeking straight to it, it is the same draw.
    #[test]
    fn a_child_size_is_a_function_of_its_index() {
        let mut sequential = SizeStream::new(42, 0.0027, 1.1, 0.2);
        sequential.seek(0);
        let run: Vec<(u64, bool)> = (0..64)
            .map(|_| {
                let draw = sequential.draw();
                (draw.base.to_bits(), draw.lot_snap)
            })
            .collect();
        let mut direct = SizeStream::new(42, 0.0027, 1.1, 0.2);
        for index in [63_u64, 0, 17, 40] {
            direct.seek(index);
            let draw = direct.draw();
            assert_eq!(
                (draw.base.to_bits(), draw.lot_snap),
                run[index as usize],
                "child {index}"
            );
        }
    }
}
