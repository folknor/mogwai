// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The discrete book: spread state, effective depletion and minimal
//! projection against the latent anchor. The mechanism, its consensus
//! record and the fit behind every constant live in
//! `notes/book-dynamics-spec.md`; this module is the state and the pure
//! transitions, deliberately free of the emission plan and the wire so the
//! walk logic is testable on its own.
//!
//! Prices are integer ticks, quantities are integer units of the size
//! increment. The deeper book is the declared ladder both this walk and
//! the venue's `cross_book` derive from the published touch with the same
//! multiply-and-floor arithmetic - the frozen-ladder rule: constructed
//! once per parent from the pre-trade snapshot, walked once, never rebuilt
//! mid-parent. Only the two touch quantities are stochastic state; the
//! post-trade touch is the surviving level's exact residual, and the next
//! deeper ladder derives from the final published touch (the old deeper
//! quantities are discarded - retaining them would need published state
//! the wire does not carry).
//!
//! Per parent, the causal order the spec fixes:
//!
//! 1. the external diffusion, jump and gap move the anchor; projection
//!    re-centres the book only if its mid left the admissible band;
//! 2. the parent size is the displayed touch with the size-match
//!    probability (a taker sizes the order to what it can see - the real
//!    eq arm of the trichotomy is 0.46 pooled), else the independent draw;
//! 3. the quantity walks the frozen ladder;
//! 4. effective depletion by the size trichotomy decides whether the touch
//!    recedes by the next observation, with the un-struck side following a
//!    witnessed depletion in with its own probability;
//! 5. the impact (permanent plus decaying transient) moves the anchor,
//!    then the spread relaxes toward a drawn target state - skipped for a
//!    witnessed depletion, whose widened book is the very move the witness
//!    measures;
//! 6. one more projection reconciles the book with the moved anchor,
//!    crediting the mechanical movement already made.
//!
//! The two rng streams are domain-separated derivations of the realization
//! seed, never drawn from the main stream (which would couple the cascade
//! realization to their creation): the book stream owns every mechanical
//! draw, the reporting stream owns the record splits, so reporting
//! parameters cannot move a price.

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Deserialize;

use super::fingerprint::ScalarError;
use mogwai_protocol::seeds::splitmix64;

/// Stream tag for the book mechanics rng: `xor`ed into the realization
/// seed and mixed, the same derivation the cadence stream uses.
const BOOK_STREAM_TAG: u64 = 0x626F_6F6B_6479_6E31;
/// Stream tag for the reporting rng.
const REPORTING_STREAM_TAG: u64 = 0x7265_706F_7274_3161;

/// Ceiling on the spread the relaxation and widening may reach. The real
/// year's 4+ share is under seven percent pooled; past this the book is a
/// misconfiguration, not a market.
const MAX_SPREAD_TICKS: i64 = 8;

/// The dead zone, in ticks, inside which a narrowing quote's side is
/// chosen symmetrically rather than toward the anchor.
const NARROW_DEAD_ZONE_TICKS: f64 = 0.25;

/// One phase row of the book knobs. The boundaries are preset data beside
/// the envelope, not a calendar frame: a boundary is a law change only,
/// and the persistent book state crosses it untouched.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookPhaseKnobs {
    /// Minute of session this row takes effect, inclusive. Rows are
    /// sorted and the first must start at zero.
    pub start_minute: u32,
    /// Mean of the replenishment law: the geometric a fresh touch
    /// quantity is drawn from. Fitted so the simulated pre-trade touch
    /// distribution matches the observed one - the observed distribution
    /// itself is size-biased by survival and must not be drawn from.
    pub replenish_mean: f64,
    /// How the replenishment mean thickens with the spread the quote
    /// joins: mean = replenish_mean * touch_by_spread^(spread - 1).
    pub touch_by_spread: f64,
    /// The stationary spread target shares: one tick, two ticks, with the
    /// remainder at three. The relaxation aims at a fresh draw from this
    /// law each parent.
    pub target_one: f64,
    pub target_two: f64,
    /// Probability per parent that a side below the drawn target pulls
    /// its touch (cancellation widening).
    pub p_widen: f64,
    /// Per-step probability of the narrowing loop: a book wider than the
    /// drawn target quotes inside on the side that brings the mid toward
    /// the anchor.
    pub p_narrow: f64,
    /// Probability the un-struck side follows a witnessed depletion in.
    pub p_follow: f64,
    /// Effective depletion probability per trichotomy arm: the touch is
    /// observed receded at the next parent given the parent's size was
    /// under, at, or over the displayed touch.
    pub p_dep_lt: f64,
    pub p_dep_eq: f64,
    pub p_dep_gt: f64,
    /// Probability the parent's size is exactly the displayed touch.
    pub p_size_match: f64,
    /// Probability a single-level execution of at least two units is
    /// reported as two records (the reporting split; supplies the
    /// single-level multi-print mass, conditional share 0.907).
    pub p_split: f64,
    /// The anchor impact: permanent plus a transient decaying per parent.
    /// The residual impact channel the fit protocol admitted after the
    /// emergent route undershot lags 1 and 10 with lag 100 on target.
    pub impact_permanent_ticks: f64,
    pub impact_transient_ticks: f64,
    pub impact_transient_decay: f64,
    /// The projection band's slack beyond the half-spread, in ticks.
    pub slack_ticks: f64,
}

fn probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

impl BookPhaseKnobs {
    fn validate(&self) -> Result<(), ScalarError> {
        if !self.replenish_mean.is_finite() || self.replenish_mean < 1.0 {
            return Err(ScalarError::detailed("book", "replenish_mean"));
        }
        if !self.touch_by_spread.is_finite() || !(0.1..=10.0).contains(&self.touch_by_spread) {
            return Err(ScalarError::detailed("book", "touch_by_spread"));
        }
        if !probability(self.target_one)
            || !probability(self.target_two)
            || self.target_one + self.target_two > 1.0
        {
            return Err(ScalarError::detailed("book", "spread target shares"));
        }
        for (name, value) in [
            ("p_widen", self.p_widen),
            ("p_narrow", self.p_narrow),
            ("p_follow", self.p_follow),
            ("p_dep_lt", self.p_dep_lt),
            ("p_dep_eq", self.p_dep_eq),
            ("p_dep_gt", self.p_dep_gt),
            ("p_size_match", self.p_size_match),
            ("p_split", self.p_split),
        ] {
            if !probability(value) {
                return Err(ScalarError::detailed("book", name));
            }
        }
        // The narrowing loop is geometric in p_narrow; a certain step
        // would spin forever on a wide book.
        if self.p_narrow >= 1.0 {
            return Err(ScalarError::detailed("book", "p_narrow"));
        }
        if !self.impact_permanent_ticks.is_finite()
            || !(0.0..=10.0).contains(&self.impact_permanent_ticks)
            || !self.impact_transient_ticks.is_finite()
            || !(0.0..=10.0).contains(&self.impact_transient_ticks)
            || !self.impact_transient_decay.is_finite()
            || !(0.0..1.0).contains(&self.impact_transient_decay)
        {
            return Err(ScalarError::detailed("book", "impact"));
        }
        if !self.slack_ticks.is_finite() || !(0.0..=4.0).contains(&self.slack_ticks) {
            return Err(ScalarError::detailed("book", "slack_ticks"));
        }
        Ok(())
    }
}

/// The `[instrument.generator.book]` table: the per-phase knob rows.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookDynamicsConfig {
    pub phases: Vec<BookPhaseKnobs>,
}

impl BookDynamicsConfig {
    pub fn validate(&self) -> Result<(), ScalarError> {
        if self.phases.is_empty() {
            return Err(ScalarError::detailed("book", "at least one phase row"));
        }
        if self.phases[0].start_minute != 0 {
            return Err(ScalarError::detailed(
                "book",
                "the first phase row must start at minute zero",
            ));
        }
        for pair in self.phases.windows(2) {
            if pair[1].start_minute <= pair[0].start_minute {
                return Err(ScalarError::detailed(
                    "book",
                    "phase rows must be sorted by strictly increasing start_minute",
                ));
            }
        }
        for row in &self.phases {
            row.validate()?;
        }
        Ok(())
    }

    /// The knob row in effect at a minute of session: the last row whose
    /// start is at or before it. Total because validation pins row zero
    /// to minute zero.
    #[must_use]
    pub fn knobs_at(&self, minute_of_session: u32) -> &BookPhaseKnobs {
        self.phases
            .iter()
            .rev()
            .find(|row| row.start_minute <= minute_of_session)
            .expect("validation pins the first row to minute zero")
    }
}

/// How a parent's size compared with the displayed touch it struck.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trichotomy {
    Under,
    Exact,
    Over,
}

/// One executed level of the frozen-ladder walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelFill {
    pub price_ticks: i64,
    pub units: u64,
}

/// The outcome of one parent's walk against the frozen ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkOutcome {
    /// Requested quantity, in units.
    pub requested: u64,
    /// Executed quantity: the sum of the fills. Reporting records sum to
    /// exactly this.
    pub executed: u64,
    /// Unexecuted remainder after the declared ladder was exhausted. The
    /// walk never appends an execution level to finish the parent.
    pub unexecuted: u64,
    /// Fully exhausted levels.
    pub levels_exhausted: u16,
    /// Residual units at the surviving level the walk stopped inside, or
    /// zero when it exhausted the whole declared ladder.
    pub residual: u64,
    /// The per-level fills, in walk order.
    pub fills: Vec<LevelFill>,
    /// The trichotomy against the pre-trade touch.
    pub trichotomy: Trichotomy,
}

/// A published snapshot: the four values the wire carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookSnapshot {
    pub bid_ticks: i64,
    pub ask_ticks: i64,
    pub bid_units: u64,
    pub ask_units: u64,
}

/// The persistent discrete book. `Clone` end to end, so the checkpoint
/// chain and the seek carry it unchanged.
#[derive(Debug, Clone)]
pub struct DiscreteBook {
    bid_ticks: i64,
    ask_ticks: i64,
    bid_units: u64,
    ask_units: u64,
    /// The transient impact register: the decayed sum of past signs.
    register: f64,
    /// Whether the last settled parent's depletion was witnessed - the
    /// same-parent relaxation skip.
    witnessed: bool,
    rng: ChaCha12Rng,
    reporting_rng: ChaCha12Rng,
}

/// A struck side, as a direction on the tick grid: `+1` is the ask (a
/// buyer strikes it), `-1` the bid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Struck {
    Ask,
    Bid,
}

impl Struck {
    fn direction(self) -> i64 {
        match self {
            Self::Ask => 1,
            Self::Bid => -1,
        }
    }

    fn sign(self) -> f64 {
        match self {
            Self::Ask => 1.0,
            Self::Bid => -1.0,
        }
    }
}

/// The venue ladder arithmetic, one definition: the quantity behind a
/// level of `units`, `floor(units * growth)` with a floor of one unit -
/// exactly `cross_book`'s `floor_to_increment(size * growth,
/// increment).max(increment)` expressed in units of the increment.
#[must_use]
pub fn ladder_next_units(units: u64, growth: Decimal) -> u64 {
    (Decimal::from(units) * growth)
        .floor()
        .to_u64()
        .unwrap_or(u64::MAX)
        .max(1)
}

impl DiscreteBook {
    /// A book opened around the anchor at a two-tick spread with
    /// replenishment-law touch quantities, its streams derived from the
    /// realization seed under stable domain tags.
    #[must_use]
    pub fn new(seed: u64, anchor_ticks: f64, knobs: &BookPhaseKnobs) -> Self {
        let mut rng = ChaCha12Rng::seed_from_u64(splitmix64(seed ^ BOOK_STREAM_TAG));
        let reporting_rng = ChaCha12Rng::seed_from_u64(splitmix64(seed ^ REPORTING_STREAM_TAG));
        let bid = (anchor_ticks - 1.0).round().max(1.0) as i64;
        let bid_units = replenish(&mut rng, knobs, 2);
        let ask_units = replenish(&mut rng, knobs, 2);
        Self {
            bid_ticks: bid,
            ask_ticks: bid + 2,
            bid_units,
            ask_units,
            register: 0.0,
            witnessed: false,
            rng,
            reporting_rng,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> BookSnapshot {
        BookSnapshot {
            bid_ticks: self.bid_ticks,
            ask_ticks: self.ask_ticks,
            bid_units: self.bid_units,
            ask_units: self.ask_units,
        }
    }

    fn spread(&self) -> i64 {
        self.ask_ticks - self.bid_ticks
    }

    fn mid(&self) -> f64 {
        (self.bid_ticks + self.ask_ticks) as f64 / 2.0
    }

    fn touch_units(&self, struck: Struck) -> u64 {
        match struck {
            Struck::Ask => self.ask_units,
            Struck::Bid => self.bid_units,
        }
    }

    fn set_touch_units(&mut self, struck: Struck, units: u64) {
        match struck {
            Struck::Ask => self.ask_units = units,
            Struck::Bid => self.bid_units = units,
        }
    }

    /// Step 1 and step 6: minimal projection against the anchor. The book
    /// moves only when its mid left the band (half spread plus slack),
    /// and then by the smallest whole-tick translation that restores it -
    /// never a snap to the anchor, whose discontinuity manufactures the
    /// multi-tick moves the mechanism exists to remove. Spread preserved;
    /// the bid floors at one tick, where symmetry cannot hold and is not
    /// claimed.
    pub fn project(&mut self, anchor_ticks: f64, knobs: &BookPhaseKnobs) {
        let band = self.spread() as f64 / 2.0 + knobs.slack_ticks;
        let shift = anchor_ticks - self.mid();
        if shift.abs() <= band {
            return;
        }
        let move_ticks = (shift.abs() - band).ceil() as i64 * shift.signum() as i64;
        let spread = self.spread();
        self.bid_ticks = (self.bid_ticks + move_ticks).max(1);
        self.ask_ticks = self.bid_ticks + spread;
    }

    /// Step 2: the parent's effective size. With the size-match
    /// probability it is exactly the displayed touch; otherwise the
    /// independent draw the caller supplies from the declared size law.
    pub fn effective_size(
        &mut self,
        struck: Struck,
        drawn_units: u64,
        knobs: &BookPhaseKnobs,
    ) -> u64 {
        let touch = self.touch_units(struck);
        if touch > 0 && self.rng.random_bool(knobs.p_size_match) {
            touch
        } else {
            drawn_units.max(1)
        }
    }

    /// Step 3: the frozen-ladder walk. The ladder is constructed here,
    /// once, from the pre-trade snapshot - level zero the displayed
    /// touch, each deeper level `ladder_next_units` of the one before -
    /// and consumed exactly once. Consumption never mutates the book;
    /// `settle` forms the post-trade book from the outcome, which is what
    /// keeps a mid-parent checkpoint coherent and the venue's view of the
    /// same ladder identical.
    #[must_use]
    pub fn execute(
        &self,
        struck: Struck,
        requested_units: u64,
        growth: Decimal,
        depth_levels: u16,
    ) -> WalkOutcome {
        let touch_units = self.touch_units(struck);
        let touch_price = match struck {
            Struck::Ask => self.ask_ticks,
            Struck::Bid => self.bid_ticks,
        };
        let trichotomy = match requested_units.cmp(&touch_units) {
            std::cmp::Ordering::Less => Trichotomy::Under,
            std::cmp::Ordering::Equal => Trichotomy::Exact,
            std::cmp::Ordering::Greater => Trichotomy::Over,
        };
        let mut fills = Vec::new();
        let mut remaining = requested_units;
        let mut level_units = touch_units;
        let mut exhausted: u16 = 0;
        let mut residual = 0_u64;
        for level in 0..depth_levels {
            let price = touch_price + i64::from(level) * struck.direction();
            let take = remaining.min(level_units);
            if take > 0 {
                fills.push(LevelFill {
                    price_ticks: price,
                    units: take,
                });
                remaining -= take;
            }
            if take == level_units {
                exhausted += 1;
                // Exhausting a level exposes the level behind it with its
                // declared quantity - including a walk that stopped
                // exactly on the boundary, whose surviving touch is that
                // exposed level. A fully exhausted ladder leaves zero,
                // and the post-trade replenishment creates the next
                // touch; the walk never appends an execution level to
                // finish the parent.
                residual = if level + 1 < depth_levels {
                    ladder_next_units(level_units, growth)
                } else {
                    0
                };
            } else {
                residual = level_units - take;
            }
            if remaining == 0 {
                break;
            }
            level_units = residual;
        }
        let executed = requested_units - remaining;
        if remaining > 0 {
            residual = 0;
        }
        WalkOutcome {
            requested: requested_units,
            executed,
            unexecuted: remaining,
            levels_exhausted: exhausted,
            residual,
            fills,
            trichotomy,
        }
    }

    /// Step 4: effective depletion, the follow-in, and the post-trade
    /// touch. Returns whether the depletion was witnessed, which the
    /// relaxation reads.
    pub fn settle(&mut self, struck: Struck, walk: &WalkOutcome, knobs: &BookPhaseKnobs) -> bool {
        let p = match walk.trichotomy {
            Trichotomy::Under => knobs.p_dep_lt,
            Trichotomy::Exact => knobs.p_dep_eq,
            Trichotomy::Over => knobs.p_dep_gt,
        };
        let witnessed = self.rng.random_bool(p);
        self.witnessed = witnessed;
        if witnessed {
            let recede = i64::from(walk.levels_exhausted.max(1));
            match struck {
                Struck::Ask => self.ask_ticks += recede,
                Struck::Bid => self.bid_ticks -= recede,
            }
            self.bid_ticks = self.bid_ticks.max(1);
            self.ask_ticks = self.ask_ticks.max(self.bid_ticks + 1);
            // The exposed touch is the surviving level's exact residual
            // where the walk reached it; a receded touch the walk never
            // consumed into, or an exhausted ladder, takes a
            // replenishment draw. A zero residual is never a published
            // occupied touch.
            let units = if walk.levels_exhausted > 0 && walk.residual > 0 {
                walk.residual
            } else {
                let spread = self.spread();
                replenish(&mut self.rng, knobs, spread)
            };
            self.set_touch_units(struck, units);
            if self.rng.random_bool(knobs.p_follow) {
                match struck {
                    Struck::Ask => {
                        self.bid_ticks = (self.bid_ticks + recede).min(self.ask_ticks - 1)
                    }
                    Struck::Bid => {
                        self.ask_ticks = (self.ask_ticks - recede).max(self.bid_ticks + 1)
                    }
                }
                let spread = self.spread();
                let follow_units = replenish(&mut self.rng, knobs, spread);
                match struck {
                    Struck::Ask => self.bid_units = follow_units,
                    Struck::Bid => self.ask_units = follow_units,
                }
            }
        } else if walk.levels_exhausted > 0 {
            // Replenished before observed: the touch price stands and its
            // quantity is a fresh draw.
            let spread = self.spread();
            let units = replenish(&mut self.rng, knobs, spread);
            self.set_touch_units(struck, units);
        } else if walk.executed > 0 {
            // Partial consumption of a standing touch leaves the exact
            // residual.
            self.set_touch_units(struck, walk.residual);
        }
        witnessed
    }

    /// Step 5's anchor half: the impact move in ticks, permanent plus the
    /// transient register's step. The caller applies it to the latent
    /// anchor - never to the book, which follows only through projection.
    pub fn anchor_impact_ticks(&mut self, struck: Struck, knobs: &BookPhaseKnobs) -> f64 {
        let sign = struck.sign();
        let previous = self.register;
        self.register = knobs.impact_transient_decay * previous + sign;
        knobs.impact_permanent_ticks * sign
            + knobs.impact_transient_ticks
                * (sign - (1.0 - knobs.impact_transient_decay) * previous)
    }

    /// Step 5's book half: the spread relaxes toward a drawn target
    /// state. Skipped entirely for a witnessed depletion - narrowing the
    /// widened book back in the same parent would cancel the very move
    /// the witness measures. Narrowing quotes inside on the side that
    /// brings the mid toward the anchor when the displacement is
    /// meaningful, symmetrically otherwise; widening is a quote pull at
    /// any spread below the drawn target.
    pub fn relax(&mut self, anchor_ticks: f64, growth: Decimal, knobs: &BookPhaseKnobs) {
        let u: f64 = self.rng.random();
        let target = if u < knobs.target_one {
            1
        } else if u < knobs.target_one + knobs.target_two {
            2
        } else {
            3
        };
        if !self.witnessed {
            while self.spread() > target && self.rng.random_bool(knobs.p_narrow) {
                let displacement = anchor_ticks - self.mid();
                let toward_ask = if displacement < -NARROW_DEAD_ZONE_TICKS {
                    true
                } else if displacement > NARROW_DEAD_ZONE_TICKS {
                    false
                } else {
                    self.rng.random_bool(0.5)
                };
                let spread = self.spread() - 1;
                if toward_ask {
                    self.ask_ticks -= 1;
                    self.ask_units = replenish(&mut self.rng, knobs, spread);
                } else {
                    self.bid_ticks += 1;
                    self.bid_units = replenish(&mut self.rng, knobs, spread);
                }
            }
        }
        if self.spread() < target
            && self.spread() < MAX_SPREAD_TICKS
            && self.rng.random_bool(knobs.p_widen)
        {
            // The pulled touch exposes the declared ladder level behind
            // it, per the shared arithmetic.
            if self.rng.random_bool(0.5) {
                self.ask_ticks += 1;
                self.ask_units = ladder_next_units(self.ask_units, growth);
            } else {
                self.bid_ticks = (self.bid_ticks - 1).max(1);
                self.bid_units = ladder_next_units(self.bid_units, growth);
            }
        }
        self.witnessed = false;
    }

    /// The reporting split: a single-level execution of at least two
    /// units may be reported as two records at the same price, total
    /// quantity preserved. Reads only the reporting stream, so reporting
    /// parameters leave every book draw - and therefore every price -
    /// untouched.
    #[must_use]
    pub fn split_reports(&mut self, walk: &WalkOutcome, knobs: &BookPhaseKnobs) -> Vec<LevelFill> {
        if walk.fills.len() == 1
            && walk.fills[0].units >= 2
            && self.reporting_rng.random_bool(knobs.p_split)
        {
            let fill = walk.fills[0];
            let cut = self.reporting_rng.random_range(1..fill.units);
            return vec![
                LevelFill {
                    price_ticks: fill.price_ticks,
                    units: cut,
                },
                LevelFill {
                    price_ticks: fill.price_ticks,
                    units: fill.units - cut,
                },
            ];
        }
        walk.fills.clone()
    }
}

/// A replenishment draw: geometric on one and up, mean thickened by the
/// spread the quote joins. This is the law behind a fresh queue, fitted so
/// the simulated pre-trade touch distribution matches the observed one -
/// which is size-biased by survival and must not be drawn from directly.
fn replenish(rng: &mut ChaCha12Rng, knobs: &BookPhaseKnobs, spread: i64) -> u64 {
    let exponent = (spread.max(1) - 1) as f64;
    let mean = (knobs.replenish_mean * knobs.touch_by_spread.powf(exponent)).max(1.0);
    let p = 1.0 / mean;
    let u: f64 = rng.random();
    // Inverse transform of the geometric on {1, 2, ...}: the smallest k
    // with 1 - (1 - p)^k >= u.
    let k = ((1.0 - u).ln() / (1.0 - p).ln()).ceil();
    if k.is_finite() && k >= 1.0 {
        (k as u64).min(1_000_000)
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn knobs() -> BookPhaseKnobs {
        BookPhaseKnobs {
            start_minute: 0,
            replenish_mean: 2.2,
            touch_by_spread: 2.0,
            target_one: 0.5,
            target_two: 0.4,
            p_widen: 0.28,
            p_narrow: 0.7,
            p_follow: 0.6,
            p_dep_lt: 0.19,
            p_dep_eq: 0.45,
            p_dep_gt: 0.6,
            p_size_match: 0.42,
            p_split: 0.05,
            impact_permanent_ticks: 0.34,
            impact_transient_ticks: 0.32,
            impact_transient_decay: 0.9,
            slack_ticks: 0.5,
        }
    }

    fn config() -> BookDynamicsConfig {
        let mut evening = knobs();
        evening.start_minute = 900;
        evening.replenish_mean = 1.05;
        BookDynamicsConfig {
            phases: vec![knobs(), evening],
        }
    }

    #[test]
    fn validation_refuses_the_malformed_rows() {
        assert!(config().validate().is_ok());
        let mut bad = config();
        bad.phases.clear();
        assert!(bad.validate().is_err(), "no rows");
        let mut bad = config();
        bad.phases[0].start_minute = 5;
        assert!(bad.validate().is_err(), "first row off zero");
        let mut bad = config();
        bad.phases[1].start_minute = 0;
        assert!(bad.validate().is_err(), "unsorted rows");
        let mut bad = config();
        bad.phases[0].target_one = 0.7;
        bad.phases[0].target_two = 0.5;
        assert!(bad.validate().is_err(), "target shares past one");
        let mut bad = config();
        bad.phases[0].p_narrow = 1.0;
        assert!(bad.validate().is_err(), "certain narrowing spins");
        let mut bad = config();
        bad.phases[0].replenish_mean = 0.5;
        assert!(bad.validate().is_err(), "sub-unit replenishment mean");
    }

    #[test]
    fn the_knob_row_is_the_last_at_or_before_the_minute() {
        let config = config();
        assert_eq!(config.knobs_at(0).start_minute, 0);
        assert_eq!(config.knobs_at(899).start_minute, 0);
        assert_eq!(config.knobs_at(900).start_minute, 900);
        assert_eq!(config.knobs_at(1_400).start_minute, 900);
    }

    #[test]
    fn projection_moves_minimally_and_only_outside_the_band() {
        let knobs = knobs();
        let mut book = DiscreteBook::new(7, 1000.0, &knobs);
        let before = book.snapshot();
        // Inside the band (spread 2, slack 0.5, band 1.5): no move.
        book.project(book.mid() + 1.4, &knobs);
        assert_eq!(book.snapshot(), before);
        // Outside by 0.5: the smallest whole-tick translation, one tick.
        let mid = book.mid();
        book.project(mid + 2.0, &knobs);
        assert_eq!(book.snapshot().bid_ticks, before.bid_ticks + 1);
        assert_eq!(book.spread(), 2, "projection preserves the spread");
        // Symmetric downward.
        let mid = book.mid();
        book.project(mid - 2.0, &knobs);
        assert_eq!(book.snapshot().bid_ticks, before.bid_ticks);
    }

    #[test]
    fn the_ladder_arithmetic_is_the_venue_rule() {
        // floor(units * growth) with a floor of one unit, the exact
        // `cross_book` recurrence in units of the size increment.
        assert_eq!(ladder_next_units(2, Decimal::TWO), 4);
        assert_eq!(ladder_next_units(3, Decimal::new(15, 1)), 4, "floor of 4.5");
        assert_eq!(ladder_next_units(1, Decimal::ONE), 1);
        assert_eq!(ladder_next_units(0, Decimal::TWO), 1, "the one-unit floor");
    }

    #[test]
    fn the_walk_conserves_quantity_and_freezes_the_ladder() {
        let knobs = knobs();
        let mut book = DiscreteBook::new(11, 1000.0, &knobs);
        book.ask_units = 2;
        let walk = book.execute(Struck::Ask, 5, Decimal::TWO, 8);
        // Ladder 2, 4, 8...: 2 at the touch, 3 at the next level.
        assert_eq!(walk.executed, 5);
        assert_eq!(walk.unexecuted, 0);
        assert_eq!(walk.levels_exhausted, 1);
        assert_eq!(walk.residual, 1, "4 less 3 survives at the second level");
        assert_eq!(walk.fills.len(), 2);
        assert_eq!(walk.fills[0].units + walk.fills[1].units, walk.executed);
        assert_eq!(walk.fills[1].price_ticks, walk.fills[0].price_ticks + 1);
        assert_eq!(walk.trichotomy, Trichotomy::Over);
        // The walk mutated nothing: the book still displays the frozen
        // pre-trade touch until settle forms the post-trade book.
        assert_eq!(book.snapshot().ask_units, 2);
    }

    #[test]
    fn an_exhausted_ladder_partially_fills_and_never_extends() {
        let knobs = knobs();
        let mut book = DiscreteBook::new(13, 1000.0, &knobs);
        book.ask_units = 1;
        // Two declared levels, quantities 1 and 2: a request of 9 fills 3.
        let walk = book.execute(Struck::Ask, 9, Decimal::TWO, 2);
        assert_eq!(walk.executed, 3);
        assert_eq!(walk.unexecuted, 6);
        assert_eq!(walk.levels_exhausted, 2);
        assert_eq!(walk.residual, 0, "no exposed residual to promote");
        assert_eq!(walk.fills.len(), 2);
    }

    #[test]
    fn settle_never_publishes_a_zero_touch_and_the_follow_preserves_order() {
        let knobs = knobs();
        for seed in 0..64_u64 {
            let mut book = DiscreteBook::new(seed, 1000.0, &knobs);
            book.ask_units = 2;
            let walk = book.execute(Struck::Ask, 2, Decimal::TWO, 8);
            book.settle(Struck::Ask, &walk, &knobs);
            let snap = book.snapshot();
            assert!(snap.ask_units > 0, "seed {seed}: zero published touch");
            assert!(snap.bid_units > 0, "seed {seed}");
            assert!(snap.ask_ticks > snap.bid_ticks, "seed {seed}: crossed book");
        }
    }

    #[test]
    fn a_partial_touch_consumption_leaves_the_exact_residual() {
        let knobs = {
            let mut k = knobs();
            k.p_dep_lt = 0.0;
            k
        };
        let mut book = DiscreteBook::new(17, 1000.0, &knobs);
        book.ask_units = 5;
        let walk = book.execute(Struck::Ask, 2, Decimal::TWO, 8);
        assert_eq!(walk.trichotomy, Trichotomy::Under);
        let witnessed = book.settle(Struck::Ask, &walk, &knobs);
        assert!(!witnessed);
        assert_eq!(book.snapshot().ask_units, 3, "5 less 2, exactly");
    }

    #[test]
    fn the_reporting_split_conserves_quantity_and_price() {
        let mut knobs = knobs();
        knobs.p_split = 1.0;
        let mut book = DiscreteBook::new(19, 1000.0, &knobs);
        book.ask_units = 4;
        let walk = book.execute(Struck::Ask, 3, Decimal::TWO, 8);
        assert_eq!(walk.fills.len(), 1);
        let reports = book.split_reports(&walk, &knobs);
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].units + reports[1].units, walk.executed);
        assert_eq!(reports[0].price_ticks, reports[1].price_ticks);
        assert!(reports[0].units >= 1 && reports[1].units >= 1);
        // A one-unit fill cannot split.
        book.ask_units = 1;
        let walk = book.execute(Struck::Ask, 1, Decimal::TWO, 8);
        let reports = book.split_reports(&walk, &knobs);
        assert_eq!(reports.len(), 1);
    }

    #[test]
    fn the_reporting_stream_never_moves_a_book_draw() {
        // Two books, same seed; one takes a reporting draw between
        // parents, and every subsequent mechanical outcome is identical.
        let knobs = {
            let mut k = knobs();
            k.p_split = 1.0;
            k
        };
        let mut a = DiscreteBook::new(23, 1000.0, &knobs);
        let mut b = a.clone();
        let walk = b.execute(Struck::Ask, 3, Decimal::TWO, 8);
        let reports = b.split_reports(&walk, &knobs);
        assert_eq!(reports.len(), 2, "the reporting draw was actually taken");
        for struck in [Struck::Ask, Struck::Bid, Struck::Ask] {
            let size_a = a.effective_size(struck, 2, &knobs);
            let size_b = b.effective_size(struck, 2, &knobs);
            assert_eq!(size_a, size_b);
            let walk_a = a.execute(struck, size_a, Decimal::TWO, 8);
            let walk_b = b.execute(struck, size_b, Decimal::TWO, 8);
            assert_eq!(walk_a, walk_b);
            assert_eq!(
                a.settle(struck, &walk_a, &knobs),
                b.settle(struck, &walk_b, &knobs)
            );
            a.relax(a.mid(), Decimal::TWO, &knobs);
            b.relax(b.mid(), Decimal::TWO, &knobs);
            assert_eq!(a.snapshot(), b.snapshot());
        }
    }

    #[test]
    fn the_anchor_impact_is_the_propagator_form() {
        let knobs = knobs();
        let mut book = DiscreteBook::new(29, 1000.0, &knobs);
        // First parent: register empty, the move is the full kick.
        let first = book.anchor_impact_ticks(Struck::Ask, &knobs);
        let kick = knobs.impact_permanent_ticks + knobs.impact_transient_ticks;
        assert!((first - kick).abs() < 1e-12);
        // A long same-side run converges the per-parent move toward the
        // permanent component: the transient register saturates at
        // 1 / (1 - decay) and its decay cancels the fresh transient kick.
        let mut last = first;
        for _ in 0..400 {
            last = book.anchor_impact_ticks(Struck::Ask, &knobs);
        }
        assert!(
            (last - knobs.impact_permanent_ticks).abs() < 0.01,
            "converged move {last} against permanent {}",
            knobs.impact_permanent_ticks
        );
    }

    #[test]
    fn relaxation_respects_the_witness_skip() {
        let mut knobs = knobs();
        knobs.p_narrow = 0.99;
        knobs.target_one = 1.0;
        knobs.target_two = 0.0;
        knobs.p_widen = 0.0;
        for seed in 0..32_u64 {
            let mut book = DiscreteBook::new(seed, 1000.0, &knobs);
            book.witnessed = true;
            let spread_before = book.spread();
            book.relax(book.mid(), Decimal::TWO, &knobs);
            assert_eq!(
                book.spread(),
                spread_before,
                "seed {seed}: a witnessed parent narrowed its own widening"
            );
            // The skip is one parent only.
            book.relax(book.mid(), Decimal::TWO, &knobs);
            assert_eq!(book.spread(), 1, "seed {seed}: the next parent relaxes");
        }
    }

    #[test]
    fn the_size_match_takes_the_displayed_touch() {
        let mut knobs = knobs();
        knobs.p_size_match = 1.0;
        let mut book = DiscreteBook::new(31, 1000.0, &knobs);
        book.ask_units = 7;
        assert_eq!(book.effective_size(Struck::Ask, 2, &knobs), 7);
        knobs.p_size_match = 0.0;
        assert_eq!(book.effective_size(Struck::Ask, 2, &knobs), 2);
    }
}
