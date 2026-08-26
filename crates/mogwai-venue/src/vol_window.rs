// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! One boat's resident trailing window of prints, kept by the tape thread so an
//! acceptance-time market reading does not have to regenerate the tape.
//!
//! Why this exists. A cache miss in `fills::MarketReadingCache` called
//! `fills::read_market`, which opens a history source at a checkpoint and
//! replays the full `VOL_WINDOW_NS` (300 s) of tape - measured at 9.8 ms on the
//! submit path against a 5 ms budget, with the window walk, not the restore, as
//! the cost that remained after the checkpoint stride repair. The prints that
//! walk regenerates are prints this boat's tape thread produced moments ago and
//! threw away. Keeping them resident turns the miss into a fold over ~15,000
//! floats, tens of microseconds, with the walk kept as the fallback authority.
//!
//! The identity discipline, which is the load-bearing part. The walk and this
//! window are one quantity computed from two holders of the same tape, which is
//! the two-implementations trap the workspace's standing rules name. Three
//! things keep them one implementation rather than two:
//!
//! - The arithmetic is `mogwai_data::vol_reading_from_trades`, the single
//!   shared fold both the walk and this reader call. Neither side owns a copy
//!   of the estimator's math.
//! - The proof rules mirror the walk's exactly. An instant is covered only
//!   when an event at a strictly later instant has been pulled (the walk's
//!   `reached_ns` rule), or the source ended (`close`). A read whose upper
//!   bound is not proven, or whose lower bound reaches past what the window
//!   retains, answers "fall back" rather than guessing - the same refusal
//!   shape as a walk that stopped short.
//! - Where the walk could refuse on a spent budget, the window refuses first:
//!   its event count over the retained span is an overcount of the window's,
//!   so any read the window serves is one the walk would have completed. Every
//!   divergence route therefore collapses back to the walk.
//!
//! No lookahead, and why. Trades are folded in when the tape thread pulls them
//! from the cursor, before the pacing sleep, because pulling the event at `T`
//! is what proves every instant before `T` complete - the coverage that lets a
//! read during a quiet gap be answered instead of walked. A pulled-but-unpaced
//! trade at `T` can never be served: proving `to_ns` requires a strictly later
//! pulled event, so a read can only reach trades at instants strictly before
//! the newest pull - all of which the thread has already published - and a
//! command's own boat-clock instant is in any case behind `T`, whose pacing
//! sleep has not elapsed.
//!
//! Retention is size-capped, not time-based, and the choice is about quiet
//! gaps: a time-based eviction keyed to the newest pulled trade would evict
//! the entire still-wanted window the moment a print hours ahead is pulled
//! during a session gap. The cap holds several dense windows' worth of prints,
//! so eviction only happens on water dense enough to refill the window, and a
//! quiet boat retains its history across the gap. Whatever is evicted moves
//! `covered_from_ns` forward, so a read wanting evicted water falls back to
//! the walk rather than reading a hole.

use std::{collections::VecDeque, sync::Mutex};

use mogwai_data::{BookState, VolReading};
use rust_decimal::Decimal;

/// Prints retained. At the raw fill cadence a 300 s window is ~15,000 prints,
/// so this holds roughly four windows of the densest tape - enough that a read
/// at a sweep-interval bucket behind the frontier always finds its window on
/// dense water, while a quiet boat's handful of prints is never evicted at all.
const MAX_TRADES: usize = 65_536;

/// One retained print plus the cumulative event count at its pull, kept so a
/// read can bound how many events the equivalent walk would have drained.
#[derive(Debug, Clone, Copy)]
struct WindowTrade {
    ts_ns: u64,
    px: Decimal,
    cum_events: u64,
}

#[derive(Debug, Clone, Copy)]
struct WindowQuote {
    book: BookState,
    cum_events: u64,
}

#[derive(Debug)]
struct Inner {
    trades: VecDeque<WindowTrade>,
    quotes: VecDeque<WindowQuote>,
    /// Every instant at or before this is fully folded in: set to `ts - 1` on
    /// each pull (the walk's proof rule - the pulled event proves everything
    /// strictly before it) and to the final instant on `close`.
    frontier_ns: u64,
    /// The window holds every print strictly after this instant. Starts at the
    /// boat's origin - warmup water before it was never seen here - and moves
    /// forward on eviction.
    covered_from_ns: u64,
    /// Events (trades and quotes) folded in total, and the count as of the
    /// covered boundary. Their difference bounds from above what a walk of any
    /// retained window would drain, which is what keeps the budget refusal
    /// conservative.
    events_seen: u64,
    covered_cum: u64,
    last_event_ts: Option<u64>,
}

/// The channel between one boat's tape thread and the submit path's market
/// reading. Same ownership shape as `crate::extremes::PriceExtremes`: created
/// per boat, written by that boat's tape thread, read under a brief lock.
#[derive(Debug)]
pub(crate) struct VolWindow {
    inner: Mutex<Inner>,
}

impl VolWindow {
    /// A window for a boat whose cursor starts at `origin_ns`. Nothing before
    /// the origin is covered: the first `VOL_WINDOW_NS` of readings after
    /// boarding reach into warmup water this thread never saw, and fall back
    /// to the walk exactly as every reading did before this type existed.
    pub(crate) fn starting_at(origin_ns: u64) -> Self {
        Self {
            inner: Mutex::new(Inner {
                trades: VecDeque::new(),
                quotes: VecDeque::new(),
                frontier_ns: origin_ns,
                covered_from_ns: origin_ns,
                events_seen: 0,
                covered_cum: 0,
                last_event_ts: None,
            }),
        }
    }

    /// Fold one pulled event in. Called by the tape thread for every tick, at
    /// pull time; `px` carries the price when the event is a trade and `None`
    /// for a quote, which still advances the frontier and the event count.
    pub(crate) fn fold(&self, ts_ns: u64, px: Option<Decimal>, book: Option<BookState>) {
        let mut inner = self.locked();
        inner.events_seen += 1;
        inner.frontier_ns = inner.frontier_ns.max(ts_ns.saturating_sub(1));
        inner.last_event_ts = Some(ts_ns);
        if let Some(px) = px {
            let cum_events = inner.events_seen;
            inner.trades.push_back(WindowTrade {
                ts_ns,
                px,
                cum_events,
            });
            while inner.trades.len() > MAX_TRADES {
                let evicted = inner.trades.pop_front().expect("len checked above");
                inner.covered_from_ns = inner.covered_from_ns.max(evicted.ts_ns);
                inner.covered_cum = inner.covered_cum.max(evicted.cum_events);
            }
        }
        if let Some(book) = book {
            let cum_events = inner.events_seen;
            inner.quotes.push_back(WindowQuote { book, cum_events });
            while inner.quotes.len() > MAX_TRADES {
                let evicted = inner.quotes.pop_front().expect("len checked above");
                inner.covered_from_ns = inner.covered_from_ns.max(evicted.book.ts_ns);
                inner.covered_cum = inner.covered_cum.max(evicted.cum_events);
            }
        }
    }

    /// The source ended, so its final instant carries nothing further and is
    /// itself fully covered - the walk's source-end rule.
    pub(crate) fn close(&self) {
        let mut inner = self.locked();
        if let Some(last) = inner.last_event_ts {
            inner.frontier_ns = inner.frontier_ns.max(last);
        }
    }

    /// The reading over `(from_ns, to_ns]`, or `None` when this window cannot
    /// prove it would match the walk - an unproven upper bound, a lower bound
    /// past what is retained, or an event count the walk's budget might refuse.
    /// The outer `None` means "ask the walk"; the inner `Option` is the
    /// authoritative answer, including the estimator's own refusals (sample
    /// floor, zero span), which are the walk's answers too.
    pub(crate) fn read(
        &self,
        from_ns: u64,
        to_ns: u64,
        budget: usize,
    ) -> Option<Option<VolReading>> {
        let window: Vec<(u64, Decimal)> = {
            let inner = self.locked();
            if to_ns > inner.frontier_ns || from_ns < inner.covered_from_ns {
                return None;
            }
            if inner.events_seen.saturating_sub(inner.covered_cum) > budget as u64 {
                return None;
            }
            let lo = inner.trades.partition_point(|t| t.ts_ns <= from_ns);
            let hi = inner.trades.partition_point(|t| t.ts_ns <= to_ns);
            inner
                .trades
                .range(lo..hi)
                .map(|t| (t.ts_ns, t.px))
                .collect()
        };
        Some(mogwai_data::vol_reading_from_trades(&window))
    }

    pub(crate) fn book_at(&self, ts_ns: u64, budget: usize) -> Option<Option<BookState>> {
        let inner = self.locked();
        if ts_ns > inner.frontier_ns
            || ts_ns < inner.covered_from_ns
            || inner.events_seen.saturating_sub(inner.covered_cum) > budget as u64
        {
            return None;
        }
        let hi = inner
            .quotes
            .partition_point(|quote| quote.book.ts_ns <= ts_ns);
        let index = hi.checked_sub(1)?;
        Some(inner.quotes.get(index).map(|quote| quote.book))
    }

    /// The last print at or before `ts_ns`, or `None` where this window is not
    /// entitled to answer.
    ///
    /// The same coverage refusal [`VolWindow::read`] and [`VolWindow::book_at`]
    /// apply, and for the same reason rather than for symmetry. Past the fold
    /// frontier the window has not seen the instant yet, so the newest print it
    /// holds is from before it and would be served as though it were the last
    /// one - which is not a look-ahead but is a stale answer presented as a
    /// current one. Before `covered_from_ns` the eviction has thrown away the
    /// prints that would have been the answer, so the oldest survivor is served
    /// instead of the print that really preceded `ts_ns`. Both are silently
    /// wrong rather than absent, which is the shape this window refuses.
    pub(crate) fn last_trade_at(&self, ts_ns: u64) -> Option<Decimal> {
        let inner = self.locked();
        if ts_ns > inner.frontier_ns || ts_ns < inner.covered_from_ns {
            return None;
        }
        let hi = inner.trades.partition_point(|trade| trade.ts_ns <= ts_ns);
        hi.checked_sub(1)
            .and_then(|index| inner.trades.get(index).map(|trade| trade.px))
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_data::{MemorySource, TickEvent, vol_reading};
    use mogwai_protocol::{AggressorSide, TradeTick};

    fn trade(ts_event: u64, price: i64) -> TickEvent {
        TickEvent::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::from(price),
            size: Decimal::ONE,
            aggressor: AggressorSide::NoAggressor,
            ts_event,
        })
    }
    fn quote(ts_event: u64, bid: i64, ask: i64) -> TickEvent {
        TickEvent::Quote(mogwai_protocol::QuoteTick {
            symbol: "BTCUSDT".into(),
            bid_px: Decimal::from(bid),
            ask_px: Decimal::from(ask),
            bid_sz: Decimal::ONE,
            ask_sz: Decimal::ONE,
            ts_event,
        })
    }

    fn fold_all(window: &VolWindow, ticks: &[TickEvent]) {
        for tick in ticks {
            match tick {
                TickEvent::Trade(t) => window.fold(t.ts_event, Some(t.price), None),
                TickEvent::Quote(q) => window.fold(
                    q.ts_event,
                    None,
                    Some(BookState {
                        bid_px: q.bid_px,
                        ask_px: q.ask_px,
                        bid_sz: q.bid_sz,
                        ask_sz: q.ask_sz,
                        ts_ns: q.ts_event,
                    }),
                ),
            }
        }
    }

    /// The cross-implementation pin: the window and the walk are one quantity,
    /// and this holds them bit-equal on one stream - prices interleaved with
    /// quotes, instants shared between a quote and its print, exactly the shape
    /// protocol 7 emits. Every field is compared, the floats by bit pattern.
    #[test]
    fn a_window_read_is_bit_identical_to_the_walk() {
        let mut ticks = Vec::new();
        for i in 1..=40_u64 {
            ticks.push(quote(i * 10, 99, 101));
            ticks.push(trade(i * 10, 100 + (i as i64 % 7)));
        }
        let window = VolWindow::starting_at(0);
        fold_all(&window, &ticks);
        for (from_ns, to_ns) in [(0, 250), (35, 250), (100, 390), (0, 390)] {
            let mut source = MemorySource::new(ticks.clone());
            let walked = vol_reading(&mut source, from_ns, to_ns, 10_000);
            let resident = window
                .read(from_ns, to_ns, 10_000)
                .expect("the window covers this span");
            match (walked, resident) {
                (Some(a), Some(b)) => {
                    assert_eq!(a.last_px, b.last_px);
                    assert_eq!(a.last_ts_ns, b.last_ts_ns);
                    assert_eq!(a.samples, b.samples);
                    assert_eq!(a.span_ns, b.span_ns);
                    assert_eq!(a.rms_return.to_bits(), b.rms_return.to_bits());
                    assert_eq!(a.horizon_return.to_bits(), b.horizon_return.to_bits());
                }
                (None, None) => {}
                (walked, resident) => {
                    panic!("walk {walked:?} and window {resident:?} disagree on refusal");
                }
            }
        }
    }

    #[test]
    fn an_unproven_upper_bound_falls_back_rather_than_answering() {
        let window = VolWindow::starting_at(0);
        fold_all(
            &window,
            &(1..=20).map(|i| trade(i * 10, 100)).collect::<Vec<_>>(),
        );
        // The newest pull is at 200, so 199 is proven and 200 is not: more
        // events could share that instant, exactly the walk's refusal.
        assert!(window.read(0, 199, 10_000).is_some());
        assert!(window.read(0, 200, 10_000).is_none());
    }

    #[test]
    fn close_proves_the_final_instant() {
        let window = VolWindow::starting_at(0);
        fold_all(
            &window,
            &(1..=20).map(|i| trade(i * 10, 100)).collect::<Vec<_>>(),
        );
        assert!(window.read(0, 200, 10_000).is_none());
        window.close();
        assert!(window.read(0, 200, 10_000).is_some());
    }

    #[test]
    fn a_window_reaching_before_the_origin_falls_back() {
        // The boat boarded at 1_000; warmup water before that was never folded
        // here, so a span touching it must go to the walk however much the
        // ring holds.
        let window = VolWindow::starting_at(1_000);
        fold_all(
            &window,
            &(1..=20)
                .map(|i| trade(1_000 + i * 10, 100))
                .collect::<Vec<_>>(),
        );
        assert!(window.read(999, 1_190, 10_000).is_none());
        assert!(window.read(1_000, 1_190, 10_000).is_some());
    }

    #[test]
    fn eviction_moves_the_covered_boundary_and_evicted_spans_fall_back() {
        let window = VolWindow::starting_at(0);
        let count = MAX_TRADES as u64 + 100;
        for i in 1..=count {
            window.fold(i, Some(Decimal::from(100)), None);
        }
        // The first 100 prints are gone, so the boundary sits at print 100 and
        // a span opening before it is refused while one opening at it reads.
        assert!(window.read(99, count - 1, usize::MAX).is_none());
        assert!(window.read(100, count - 1, usize::MAX).is_some());
    }

    #[test]
    fn a_span_the_walk_might_refuse_on_budget_falls_back() {
        let window = VolWindow::starting_at(0);
        fold_all(
            &window,
            &(1..=20).map(|i| trade(i * 10, 100)).collect::<Vec<_>>(),
        );
        assert!(window.read(0, 150, 10).is_none());
        assert!(window.read(0, 150, 1_000).is_some());
    }
}
