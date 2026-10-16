// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The two halves of a placed sweep's children that tape protocol 36 pulled
//! apart: the level steps, which feed back into the walk and are drawn as
//! geometric gaps ([`LevelSteps`]), and the sizes, which never feed back and,
//! for a river's fixed size law, are a pure function of the seed and a
//! child's global index ([`SizeStream`]). Since protocol 37 the bounce walks'
//! gaps are words addressed by child index ([`StepStream`]); the cascade's
//! are still drawn from its main stream.
//!
//! The split is what lets a parent be skipped at the cost of its feedback
//! draws alone. A skipped parent - a checkpoint extension, `advance_parent`,
//! a seek dropping a parent before its target - and a parent read through an
//! unsized view reserve their children's indices and never draw a size; a
//! parent a record reader or a sized view sees has its sizes filled from the
//! size stream positioned at its first child. Both give the same sizes,
//! because a child's size draw does not depend on how its parent was read.
//!
//! A placed sweep's level steps are drained whole at resolution
//! ([`LevelSteps::drain`]) and the sweep resolved from them without laying a
//! child out ([`resolve_sweep`]).

use crate::TapeRng;

/// The size stream's [`TapeRng`] domain.
const SIZE_STREAM_TAG: u64 = 0x7369_7A65_7331_3336;

/// Positions each child reads from the size stream: the size uniform and the
/// snap uniform, always both, so child `i` starts at position `2 * i`
/// whatever the snap probability.
const POSITIONS_PER_CHILD: u64 = 2;

/// A uniform strictly inside `(0, 1)` from one raw `u64`: the top 52 bits
/// plus a half, scaled by 2^-52. The largest input lands on `1 - 2^-53`,
/// which is exactly representable, so neither endpoint is reachable - unlike
/// a 53-bit midpoint, whose largest value rounds to exactly one.
pub(super) fn open_unit(raw: u64) -> f64 {
    ((raw >> 12) as f64 + 0.5) * (1.0 / 4_503_599_627_370_496.0)
}

/// A uniform in `[0, 1)` from one raw `u64`: the top 53 bits scaled by 2^-53.
pub(super) fn half_open_unit(raw: u64) -> f64 {
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
    /// first gap already drawn. `word` yields one raw `u64` per call and is
    /// called only for a gap: the cascade passes its main stream, the bounce
    /// walks their counter-based step words.
    pub(super) fn new(p: f64, count: u32, word: &mut impl FnMut() -> u64) -> Self {
        let mut steps = Self {
            p,
            log_q: (-p).ln_1p(),
            trials: count.saturating_sub(1),
            cursor: 0,
            next: None,
        };
        steps.advance(word);
        steps
    }

    fn advance(&mut self, word: &mut impl FnMut() -> u64) {
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
        let gap = (open_unit(word()).ln() / self.log_q).floor();
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
    /// it does. Asked once per child, in order. The per-child walk the
    /// reference resolver keeps; production drains instead.
    #[cfg(test)]
    pub(super) fn steps_at(&mut self, child: u32, word: &mut impl FnMut() -> u64) -> bool {
        if child == 0 || self.next != Some(child - 1) {
            return false;
        }
        self.advance(word);
        true
    }

    /// Every child that steps, in order, appended to `out`: trial `t` lands
    /// on child `t + 1`. Draws exactly the gaps that asking `steps_at` of
    /// every child in turn draws, in the same order - the gap `new` already
    /// drew is recorded before the next is drawn, and a step on the last
    /// trial still advances, which then draws nothing.
    pub(super) fn drain(&mut self, word: &mut impl FnMut() -> u64, out: &mut Vec<u32>) {
        while let Some(trial) = self.next {
            out.push(trial + 1);
            self.advance(word);
        }
    }
}

/// How a placed sweep resolved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SweepOutcome {
    /// Every child's price is representable. `open_ticks` is the opening
    /// level after its floor, `final_ticks` the last child's level, and
    /// `last_ts` the last child's instant.
    Placed {
        open_ticks: f64,
        final_ticks: f64,
        last_ts: u64,
    },
    /// The first child whose price is not representable, at `fault_ts`.
    /// `last_child_ticks` is that child's level when it is the sweep's last
    /// child: the per-child walk updates the end-of-sweep state on the last
    /// child before it checks that child's price, so a refusal there has
    /// already moved it.
    Refused {
        fault_ts: u64,
        last_child_ticks: Option<f64>,
    },
}

/// Resolve a placed sweep of `count` children (at least one) opening at
/// `open` on `side`, stepping at the children in `steps` (strictly
/// increasing, inside `1..count`), without laying out a child.
///
/// A sweep's levels are monotone from the opening to the final one - a
/// buyer's rise one tick per step, a seller's fall and floor at one tick -
/// and the grid accepts a contiguous range of whole ticks, so the two
/// endpoints decide every child. Where either is refused, the per-child
/// float walk is replayed to find the first refused child; the closed form
/// is not trusted there, because near `2^53` repeated unit steps and one
/// step of `k` need not agree.
pub(super) fn resolve_sweep(
    open: f64,
    side: mogwai_protocol::AggressorSide,
    count: u32,
    parent_ts: u64,
    steps: &[u32],
    grid: super::numeric::PriceGrid,
) -> SweepOutcome {
    use mogwai_protocol::AggressorSide;
    debug_assert!(count >= 1);
    let stride = super::consts::INTRA_EVENT_STEP_NS;
    let child_ts = |child: u32| parent_ts.saturating_add(u64::from(child).saturating_mul(stride));
    let open_ticks = open.max(1.0);
    if grid.mantissa(open_ticks).is_some() {
        let k = steps.len() as f64;
        let final_ticks = match side {
            AggressorSide::Buyer => open_ticks + k,
            AggressorSide::Seller => (open_ticks - k).max(1.0),
            AggressorSide::NoAggressor => unreachable!("a sweep's side is native"),
        };
        if grid.mantissa(final_ticks).is_some() {
            return SweepOutcome::Placed {
                open_ticks,
                final_ticks,
                last_ts: child_ts(count - 1),
            };
        }
    }
    let mut ticks = open;
    let mut next_step = 0_usize;
    for child in 0..count {
        if steps.get(next_step) == Some(&child) {
            next_step += 1;
            match side {
                AggressorSide::Buyer => ticks += 1.0,
                AggressorSide::Seller => ticks -= 1.0,
                AggressorSide::NoAggressor => unreachable!("a sweep's side is native"),
            }
        }
        ticks = ticks.max(1.0);
        if grid.mantissa(ticks).is_none() {
            return SweepOutcome::Refused {
                fault_ts: child_ts(child),
                last_child_ticks: (child + 1 == count).then_some(ticks),
            };
        }
    }
    unreachable!("a refused endpoint is a refused child of a monotone sweep")
}

/// The step stream's [`TapeRng`] domain.
const STEP_STREAM_TAG: u64 = 0x7374_6570_7331_3337;

/// The bounce walks' level-step gaps, addressed by child index (tape
/// protocol 37): the `j`-th gap of a sweep whose first child is `c0` is the
/// river's step-domain output at position `c0 + j`. A sweep of `n` children
/// has `n - 1` trials and draws at most one gap per trial, so its positions
/// stay inside the `[c0, c0 + n)` range its children reserve, and no two
/// parents of one trajectory share a position. Only the gaps read are
/// computed, at most a block apiece.
///
/// The gaps still feed back - the final level re-centres the drift and is the
/// repeat branch's price - so every parent drains them in turn; what the
/// stream buys is a main stream that takes the same number of draws from
/// every parent.
#[derive(Clone)]
pub(super) struct StepStream {
    rng: TapeRng,
}

impl StepStream {
    pub(super) fn new(seed: u64) -> Self {
        Self {
            rng: TapeRng::new(seed, STEP_STREAM_TAG),
        }
    }

    /// The words of the sweep whose first child is `first`, in order.
    pub(super) fn from(&self, first: u64) -> impl FnMut() -> u64 + use<> {
        let mut rng = self.rng.clone();
        rng.seek(first);
        move || rng.next_word()
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
    rng: TapeRng,
    mu: f64,
    sigma: f64,
    snap_frac: f64,
}

impl SizeStream {
    pub(super) fn new(seed: u64, median: f64, sigma: f64, snap_frac: f64) -> Self {
        Self {
            rng: TapeRng::new(seed, SIZE_STREAM_TAG),
            mu: median.ln(),
            sigma,
            snap_frac,
        }
    }

    /// Position the stream at child `first`; the following `draw` calls
    /// return that child's size and its successors' in order. Refuses a child
    /// index whose positions the stream cannot address, rather than wrapping
    /// onto another child's draw.
    pub(super) fn seek(&mut self, first: u64) {
        let position = first
            .checked_mul(POSITIONS_PER_CHILD)
            .expect("a child index past the size stream's position space");
        self.rng.seek(position);
    }

    /// The next child's size draw.
    pub(super) fn draw(&mut self) -> SizeDraw {
        let z = normal_quantile(open_unit(self.rng.next_word()));
        let base = (self.mu + self.sigma * z).exp().max(f64::MIN_POSITIVE);
        let lot_snap = half_open_unit(self.rng.next_word()) < self.snap_frac;
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
            let mut rng = TapeRng::new(7, 0);
            let untouched = rng.clone();
            let taken = {
                let mut word = || rng.next_word();
                let mut level = LevelSteps::new(p, count, &mut word);
                (0..count).filter(|&c| level.steps_at(c, &mut word)).count()
            };
            assert_eq!(taken, steps, "p {p} count {count}");
            assert_eq!(rng.position(), untouched.position(), "p {p}");
        }
    }

    /// Draw accounting: one uniform per step plus one terminal overshoot,
    /// except that a step on the last trial ends the draws.
    #[test]
    fn level_steps_draw_once_per_step_plus_the_terminal_gap() {
        for seed in 0..2_000 {
            let mut rng = TapeRng::new(seed, 0);
            let start = rng.position();
            let count = 1 + (seed % 12) as u32;
            let stepped: Vec<u32> = {
                let mut word = || rng.next_word();
                let mut level = LevelSteps::new(0.37, count, &mut word);
                (0..count)
                    .filter(|&c| level.steps_at(c, &mut word))
                    .collect()
            };
            let last_stepped = stepped.last() == Some(&(count - 1));
            let draws = if count == 1 {
                0
            } else {
                stepped.len() as u64 + u64::from(!last_stepped)
            };
            assert_eq!(rng.position() - start, draws, "seed {seed}");
            assert!(stepped.iter().all(|&c| c > 0 && c < count));
        }
    }

    /// Draining is asking `steps_at` of every child in turn: the same steps,
    /// and the stream left at the same position with the same continuation.
    /// Over random seeds, and over the forced cases a seed reaches only by
    /// luck: no trials, a certain step and an impossible one, a step on the
    /// last trial (which ends the draws) and a terminal overshoot.
    #[test]
    fn draining_steps_is_asking_every_child() {
        let walk = |p: f64, count: u32, seed: u64, drain: bool| {
            let mut rng = TapeRng::new(seed, 0);
            let stepped: Vec<u32> = {
                let mut word = || rng.next_word();
                let mut steps = LevelSteps::new(p, count, &mut word);
                if drain {
                    let mut out = Vec::new();
                    steps.drain(&mut word, &mut out);
                    out
                } else {
                    (0..count)
                        .filter(|&c| steps.steps_at(c, &mut word))
                        .collect()
                }
            };
            (stepped, rng.position(), rng.next_word())
        };
        let mut last_trial = false;
        let mut overshoot = false;
        for seed in 0..4_000_u64 {
            for (p, count) in [
                (0.37, 1 + (seed % 14) as u32),
                (0.9, 2 + (seed % 5) as u32),
                (0.02, 9),
                (0.0, 9),
                (1.0, 9),
                (0.5, 0),
                (0.5, 1),
            ] {
                let drained = walk(p, count, seed, true);
                assert_eq!(drained, walk(p, count, seed, false), "p {p} count {count}");
                last_trial |= count > 1 && drained.0.last() == Some(&(count - 1));
                overshoot |=
                    count > 1 && p > 0.0 && p < 1.0 && drained.0.last() != Some(&(count - 1));
            }
        }
        assert!(last_trial && overshoot, "both endings were exercised");
    }

    /// The per-child float walk `resolve_sweep` replaces: the reference its
    /// closed form is held to.
    fn per_child(
        open: f64,
        side: mogwai_protocol::AggressorSide,
        count: u32,
        parent_ts: u64,
        steps: &[u32],
        grid: super::super::numeric::PriceGrid,
    ) -> (Vec<(u64, i64)>, SweepOutcome) {
        let mut ticks = open;
        let mut printed = Vec::new();
        for child in 0..count {
            if steps.contains(&child) {
                ticks += match side {
                    mogwai_protocol::AggressorSide::Buyer => 1.0,
                    _ => -1.0,
                };
            }
            ticks = ticks.max(1.0);
            let ts = parent_ts.saturating_add(
                u64::from(child).saturating_mul(super::super::consts::INTRA_EVENT_STEP_NS),
            );
            match grid.mantissa(ticks) {
                Some(px) => printed.push((ts, px)),
                None => {
                    return (
                        printed,
                        SweepOutcome::Refused {
                            fault_ts: ts,
                            last_child_ticks: (child + 1 == count).then_some(ticks),
                        },
                    );
                }
            }
        }
        (
            printed,
            SweepOutcome::Placed {
                open_ticks: open.max(1.0),
                final_ticks: ticks,
                last_ts: parent_ts.saturating_add(
                    u64::from(count - 1).saturating_mul(super::super::consts::INTRA_EVENT_STEP_NS),
                ),
            },
        )
    }

    /// `resolve_sweep` decides what the per-child walk decides, and a placed
    /// sweep's described trades are the per-child walk's prints: through the
    /// floor, at and across `2^53`, at the mantissa's overflow edge, refused
    /// on the first child, mid-sweep and on the last child, and at instants
    /// saturating `u64`.
    #[test]
    fn a_resolved_sweep_is_the_per_child_walk() {
        use mogwai_protocol::AggressorSide::{Buyer, Seller};
        let limit = 9_007_199_254_740_992.0_f64;
        let unit = super::super::numeric::PriceGrid { tick_mantissa: 1 };
        // Overflows i64 past 922 ticks.
        let wide = super::super::numeric::PriceGrid {
            tick_mantissa: 10_000_000_000_000_000,
        };
        let cases: Vec<(f64, _, u32, u64, Vec<u32>, _)> = vec![
            (5.0, Buyer, 9, 0, vec![2, 3, 7], unit),
            (3.0, Seller, 9, 0, vec![1, 2, 3, 4, 5, 8], unit),
            (1.0, Seller, 4, 0, vec![1, 2, 3], unit),
            (0.4, Buyer, 3, 0, vec![], unit),
            (f64::NAN, Seller, 2, 0, vec![1], unit),
            (limit - 3.0, Buyer, 6, 0, vec![1, 2], unit),
            (limit - 3.0, Buyer, 6, 0, vec![1, 2, 3], unit),
            (limit - 3.0, Buyer, 6, 0, vec![1, 2, 3, 4, 5], unit),
            (limit - 1.0, Buyer, 2, 0, vec![1], unit),
            (limit, Seller, 3, 0, vec![1, 2], unit),
            (limit + 2.0, Seller, 4, 0, vec![1, 2, 3], unit),
            (920.0, Buyer, 5, 0, vec![1, 2, 4], wide),
            (921.0, Buyer, 3, 0, vec![1], wide),
            (921.0, Buyer, 3, 0, vec![2], wide),
            (5.0, Buyer, 4, u64::MAX - 1_500, vec![1, 3], unit),
            (5.0, Seller, 1, u64::MAX, vec![], unit),
        ];
        let (mut placed, mut first, mut mid, mut last) = (0, 0, 0, 0);
        for (open, side, count, parent_ts, steps, grid) in cases {
            let (prints, expected) = per_child(open, side, count, parent_ts, &steps, grid);
            let got = resolve_sweep(open, side, count, parent_ts, &steps, grid);
            let label = format!("open {open} {side:?} count {count} steps {steps:?}");
            match (got, expected) {
                (
                    SweepOutcome::Placed {
                        open_ticks,
                        final_ticks,
                        last_ts,
                    },
                    SweepOutcome::Placed {
                        final_ticks: want_final,
                        last_ts: want_last,
                        ..
                    },
                ) => {
                    placed += 1;
                    assert_eq!(final_ticks.to_bits(), want_final.to_bits(), "{label}");
                    assert_eq!(last_ts, want_last, "{label}");
                    let described: Vec<(u64, i64)> = crate::SweepTrades::resolved(
                        parent_ts,
                        count,
                        side,
                        open_ticks as i64,
                        grid.tick_mantissa,
                        super::super::consts::INTRA_EVENT_STEP_NS,
                        &steps,
                    )
                    .trades()
                    .map(|trade| (trade.ts, trade.px))
                    .collect();
                    assert_eq!(described, prints, "{label}");
                }
                (got, expected) => {
                    assert_eq!(got, expected, "{label}");
                    match prints.len() {
                        0 => first += 1,
                        n if n + 1 == count as usize => last += 1,
                        _ => mid += 1,
                    }
                }
            }
        }
        assert!(placed > 0 && first > 0 && mid > 0 && last > 0);
    }

    /// The step stream is the river's step domain read by child index: its
    /// words are that domain's sequence, by position, and a parent's words do
    /// not depend on where any other parent's were read.
    #[test]
    fn step_words_are_the_step_domain_by_child_index() {
        let stream = StepStream::new(5);
        let mut sequential = TapeRng::new(5, STEP_STREAM_TAG);
        let reference: Vec<u64> = (0..64).map(|_| sequential.next_word()).collect();
        let mut from_zero = stream.from(0);
        let run: Vec<u64> = (0..64).map(|_| from_zero()).collect();
        assert_eq!(run, reference);
        for first in [0_u64, 17, 40, 63] {
            assert_eq!(
                stream.from(first)(),
                reference[first as usize],
                "child {first}"
            );
        }
    }

    /// The step frequency per eligible trial is `p`, the law the coins had,
    /// with the gaps read from the production step stream the way a walk
    /// reads it: back-to-back parents, each from its own first child.
    #[test]
    fn level_steps_land_at_the_trial_probability() {
        let stream = StepStream::new(11);
        let mut first_child = 0_u64;
        for p in [0.05, 0.37, 0.9] {
            let (mut trials, mut steps) = (0_u64, 0_u64);
            for _ in 0..20_000 {
                let mut word = stream.from(first_child);
                let mut level = LevelSteps::new(p, 9, &mut word);
                steps += (0..9).filter(|&c| level.steps_at(c, &mut word)).count() as u64;
                trials += 8;
                first_child += 9;
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
