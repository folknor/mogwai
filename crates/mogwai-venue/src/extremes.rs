// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The high and low one river reached between two sweep passes, recorded by the
//! tape thread as it prints.
//!
//! Why this exists. Peak equity ratchets tick by tick at a real venue, and so
//! does a trailing stop: a spike lasting 200 milliseconds spends drawdown budget
//! and drags a trail behind it. Both were evaluated on the fill sweeper's mark
//! instead - one price per pass - so a spike that opened and closed between two
//! passes never happened as far as the account was concerned, and the account
//! kept room it should have lost.
//!
//! Why it is not per-tick evaluation. The obvious closure - run the policy in
//! the tape thread - needs the equity inputs published out of the engine and
//! puts a comparison on the hottest path in the venue. It is also unnecessary,
//! because of what the quantities actually are:
//!
//! - A trailing stop's trigger is a monotone function of its own river's tape: a
//!   sell trail tracks the running high, a buy trail the running low. The
//!   maximum over the ticks in a span is the span's high, so ratcheting once
//!   against the high is bit-for-bit what ratcheting per tick would have
//!   produced. This holds per symbol however many rivers the account rides.
//! - Equity is linear in each price it depends on, so an account holding one
//!   marked symbol attains its extreme over a span at that symbol's price
//!   extreme. Long accounts peak at the high and trough at the low; short
//!   accounts the other way round.
//!
//! So two prices per span carry the whole tick-resolution answer for an account
//! on one river, and the tape thread's only job is to remember them.
//!
//! The bound, since an account rides as many rivers as its passengers have
//! boarded. The sweeper judges a policy once per due boat, and the span it holds
//! covers that boat's river alone; every other symbol the account holds is
//! valued at its last mark. So an account on several rivers has its cross-river
//! equity evaluated at mark cadence, and its peak-equity ratchet sees a partial
//! reconstruction of the interval ordered by whichever boat came due first. That
//! is a stated bound of the model, not a defect this type is failing to cover.
//!
//! The instants are carried, and they are what keeps the answer honest rather
//! than merely harsh. A span that spiked and then collapsed is a different run
//! from one that collapsed and then spiked: in the first the ratchet raises the
//! floor before the fall tests it, in the second it does not. Replaying the two
//! extremes in time order reproduces the ordering a per-tick evaluation would
//! have seen; applying them favourable-first would invent breaches that never
//! happened, which is the opposite error to the one being fixed.
//!
//! The writer never blocks on a reader. The tape thread keeps its running
//! extremes in its own stack and publishes only when one moves, which after the
//! first few ticks of a span is rare. The reader opens a fresh span by bumping
//! an epoch; the writer notices on its next tick with one relaxed load.

use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use rust_decimal::Decimal;

/// One river's price extremes over one span, with the instant each was reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PriceSpan {
    pub(crate) high_px: Decimal,
    pub(crate) high_ns: u64,
    pub(crate) low_px: Decimal,
    pub(crate) low_ns: u64,
}

impl PriceSpan {
    fn of(px: Decimal, ts_ns: u64) -> Self {
        Self {
            high_px: px,
            high_ns: ts_ns,
            low_px: px,
            low_ns: ts_ns,
        }
    }

    /// Fold one print in. Returns whether either extreme moved, which is what
    /// decides whether the writer publishes.
    fn fold(&mut self, px: Decimal, ts_ns: u64) -> bool {
        let mut moved = false;
        if px > self.high_px {
            self.high_px = px;
            self.high_ns = ts_ns;
            moved = true;
        }
        if px < self.low_px {
            self.low_px = px;
            self.low_ns = ts_ns;
            moved = true;
        }
        moved
    }

    /// The two extremes in the order the tape reached them, deduplicated when a
    /// span held one price (or one print).
    ///
    /// This is the sequence a per-tick evaluation would have seen, reduced to
    /// the readings that can change anything: every price between two extremes
    /// ratchets nothing the extremes do not and breaches nothing the adverse
    /// extreme does not.
    pub(crate) fn in_time_order(&self) -> Vec<(Decimal, u64)> {
        if self.high_px == self.low_px {
            return vec![(self.high_px, self.high_ns)];
        }
        if self.high_ns <= self.low_ns {
            vec![(self.high_px, self.high_ns), (self.low_px, self.low_ns)]
        } else {
            vec![(self.low_px, self.low_ns), (self.high_px, self.high_ns)]
        }
    }
}

/// The channel between one boat's tape thread and the sweeper reading it.
#[derive(Debug, Default)]
pub(crate) struct PriceExtremes {
    /// Bumped by the reader to open a fresh span. The writer compares it with
    /// the epoch its running extremes belong to and starts over when they
    /// differ, which is what makes "since the last pass" mean what it says
    /// without the reader ever touching the writer's state.
    epoch: AtomicU64,
    published: Mutex<Option<(u64, PriceSpan)>>,
}

/// The tape thread's side: its running extremes and which span they belong to.
#[derive(Debug, Default)]
pub(crate) struct SpanWriter {
    epoch: u64,
    span: Option<PriceSpan>,
}

impl PriceExtremes {
    /// Record one print. Called from the tape thread on every trade it
    /// publishes, and holding the lock only when an extreme actually moved.
    pub(crate) fn record(&self, writer: &mut SpanWriter, px: Decimal, ts_ns: u64) {
        let epoch = self.epoch.load(Ordering::Acquire);
        let moved = if writer.epoch == epoch {
            match &mut writer.span {
                Some(span) => span.fold(px, ts_ns),
                slot @ None => {
                    *slot = Some(PriceSpan::of(px, ts_ns));
                    true
                }
            }
        } else {
            writer.epoch = epoch;
            writer.span = Some(PriceSpan::of(px, ts_ns));
            true
        };
        if !moved {
            return;
        }
        let Some(span) = writer.span else {
            return;
        };
        *self
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((epoch, span));
    }

    /// Take the span since the last call and open a fresh one.
    ///
    /// `None` when the tape printed nothing this span, which is an ordinary
    /// answer on a quiet river and not a reason to fall back to a single mark:
    /// no print means no tick a per-tick evaluation would have seen either.
    ///
    /// The epoch is bumped before the take, so a print racing this call is
    /// attributed to the new span rather than silently discarded: the writer
    /// stamps what it publishes, and a stale stamp is dropped here.
    pub(crate) fn take(&self) -> Option<PriceSpan> {
        let closing = self.epoch.fetch_add(1, Ordering::AcqRel);
        let mut published = self
            .published
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match *published {
            Some((epoch, span)) if epoch == closing => {
                *published = None;
                Some(span)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(value: i64) -> Decimal {
        Decimal::from(value)
    }

    #[test]
    fn a_span_carries_the_high_and_the_low_the_tape_actually_reached() {
        let extremes = PriceExtremes::default();
        let mut writer = SpanWriter::default();
        for (value, ts) in [(100, 1), (140, 2), (90, 3), (110, 4)] {
            extremes.record(&mut writer, px(value), ts);
        }
        let span = extremes.take().expect("the tape printed");
        assert_eq!((span.high_px, span.high_ns), (px(140), 2));
        assert_eq!((span.low_px, span.low_ns), (px(90), 3));
    }

    /// The ordering that keeps this from inventing breaches: a spike before a
    /// collapse raises the floor first, a spike after one does not.
    #[test]
    fn the_extremes_replay_in_the_order_the_tape_reached_them() {
        let spike_first = PriceSpan {
            high_px: px(140),
            high_ns: 2,
            low_px: px(90),
            low_ns: 5,
        };
        assert_eq!(
            spike_first.in_time_order(),
            vec![(px(140), 2), (px(90), 5)],
            "the ratchet comes first when the spike came first"
        );
        let collapse_first = PriceSpan {
            high_px: px(140),
            high_ns: 5,
            low_px: px(90),
            low_ns: 2,
        };
        assert_eq!(
            collapse_first.in_time_order(),
            vec![(px(90), 2), (px(140), 5)],
            "and the fall is tested against the unratcheted floor when it came first"
        );
    }

    #[test]
    fn a_flat_span_reads_as_one_price() {
        let flat = PriceSpan::of(px(100), 7);
        assert_eq!(flat.in_time_order(), vec![(px(100), 7)]);
    }

    /// Taking a span opens a fresh one: the next pass must not re-ratchet
    /// against a peak the previous pass already spent.
    #[test]
    fn taking_a_span_opens_a_new_one() {
        let extremes = PriceExtremes::default();
        let mut writer = SpanWriter::default();
        extremes.record(&mut writer, px(140), 1);
        assert!(extremes.take().is_some());
        assert!(
            extremes.take().is_none(),
            "a span with no print of its own is empty rather than a repeat"
        );
        extremes.record(&mut writer, px(100), 2);
        let span = extremes.take().expect("the new span printed");
        assert_eq!(
            span.high_px,
            px(100),
            "the new span starts from its own first print, not the old high"
        );
    }
}
