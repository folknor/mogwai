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
//! The cascade's draws are untouched: its degrees of freedom are configurable
//! and its sampler is its own.

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
