// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Shared trade-to-OHLCV time-bar aggregation core. Nautilus-free; lifted out
//! of `mogwai-adapter` so its second consumer (the `mogwai gen` CLI in
//! `mogwai-venue`) folds bars through the same implementation instead of a
//! hand-rolled copy.
//!
//! Caller contract, in full: the bar interval is nonzero by its type
//! (`NonZeroU64`), so the division cannot trap; the window-close arithmetic
//! saturates, so a pathological interval yields a never-closing window rather
//! than wrapping; and trade timestamps are expected to be nondecreasing across a
//! fold sequence for a given accumulator. The last one is an expectation with a
//! defined failure mode rather than an enforced invariant - see `fold_trade`,
//! which documents exactly what an out-of-order trade does and why the adapter
//! can legitimately feed it one.

use core::num::NonZeroU64;

use rust_decimal::Decimal;

/// In-progress OHLCV accumulator for one time-bar window. Pure `Decimal`; the
/// window it belongs to closes at `close_ts` (exclusive upper bound,
/// epoch-anchored). `count` is the number of trades folded into the window so
/// far. Fields are public: both consumers read them (the adapter to convert to
/// a nautilus `Bar`, the CLI to format a CSV row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BarAcc {
    pub open: Decimal,
    pub high: Decimal,
    pub low: Decimal,
    pub close: Decimal,
    pub volume: Decimal,
    pub count: u64,
    pub close_ts: u64,
}

/// The epoch-anchored close of the window a trade at `ts` falls in, for a bar
/// `interval`: `((ts / interval) + 1) * interval`. This is the exact anchoring
/// the adapter used (matching nautilus `get_bar_interval_ns` for Day-and-finer
/// aggregations; calendar anchoring for Week/Month/Year is out of scope and
/// stays refused adapter-side). Total by construction: `interval` is nonzero by
/// type, and the arithmetic saturates, so a huge interval yields `u64::MAX`
/// (a never-closing window) rather than wrapping.
#[must_use]
pub fn window_close_ns(ts: u64, interval: NonZeroU64) -> u64 {
    let iv = interval.get();
    (ts / iv).saturating_add(1).saturating_mul(iv)
}

/// Fold one trade into the running window `state`. Returns the just-closed
/// window's accumulator when the trade rotates into a new window; `None` when it
/// extends the current window or opens the first one. This reproduces the
/// adapter's `update_bar_state` rotate semantics exactly: open on the first
/// trade (all prices = trade price, volume = size, count = 1), fold with
/// `high.max` / `low.min` / `close =` / `volume +=` / `count += 1`, and rotate
/// on `ts >= active.close_ts` returning the old window unchanged.
///
/// Ordering: `ts` is expected to be nondecreasing across calls for a given
/// `state`, but an out-of-order trade is a defined, non-fatal outcome rather
/// than a checked error or a panic. A trade whose `ts` is below the active
/// window's `close_ts` folds into that (later) window even if it belongs to an
/// earlier one; a trade at or above `close_ts` rotates. Nothing wedges, no bar
/// is lost, and the distortion is confined to the window that swallowed it.
///
/// Do not "fix" this by buffering or rejecting. One consumer violates the
/// expectation on purpose: the adapter's live path runs trades through the
/// consumer `HavocFilter` before aggregating, so an armed `reorder_prob` hands
/// this function out-of-order timestamps by design. Bars are fabricated
/// adapter-side from the trade feed, so a reordered feed producing a reshaped
/// bar is the honest consequence a real consumer-side aggregator would also
/// suffer; buffering here would silently repair a divergence the operator armed.
/// The CLI's `GeneratedSource` is monotone, so it never exercises this path.
///
/// `#[must_use]`: ignoring the return silently drops a closed bar.
#[must_use]
pub fn fold_trade(
    state: &mut Option<BarAcc>,
    price: Decimal,
    size: Decimal,
    ts: u64,
    interval: NonZeroU64,
) -> Option<BarAcc> {
    let close_ts = window_close_ns(ts, interval);
    match state {
        Some(active) if ts >= active.close_ts => {
            let closed = active.clone();
            *state = Some(BarAcc::open(price, size, close_ts));
            Some(closed)
        }
        Some(active) => {
            active.high = active.high.max(price);
            active.low = active.low.min(price);
            active.close = price;
            active.volume += size;
            active.count += 1;
            None
        }
        None => {
            *state = Some(BarAcc::open(price, size, close_ts));
            None
        }
    }
}

impl BarAcc {
    fn open(price: Decimal, size: Decimal, close_ts: u64) -> Self {
        Self {
            open: price,
            high: price,
            low: price,
            close: price,
            volume: size,
            count: 1,
            close_ts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iv(n: u64) -> NonZeroU64 {
        NonZeroU64::new(n).expect("nonzero interval")
    }

    #[test]
    fn window_close_ns_anchors_mid_window_and_on_boundary() {
        // interval 1: window close is always ts + 1.
        assert_eq!(window_close_ns(0, iv(1)), 1);
        assert_eq!(window_close_ns(5, iv(1)), 6);

        // interval 10: a ts mid-window (23) closes at the next boundary (30);
        // a ts exactly on a boundary (30) closes at the following boundary (40)
        // - `close_ts` is exclusive, so a trade at a boundary belongs to the
        // window that boundary opens, not the one it closes.
        assert_eq!(window_close_ns(23, iv(10)), 30);
        assert_eq!(window_close_ns(30, iv(10)), 40);
    }

    #[test]
    fn window_close_ns_saturates_instead_of_wrapping() {
        assert_eq!(window_close_ns(u64::MAX - 1, iv(u64::MAX - 1)), u64::MAX);
        assert_eq!(window_close_ns(u64::MAX, iv(u64::MAX)), u64::MAX);
    }

    #[test]
    fn fold_trade_opens_on_first_trade() {
        let mut state = None;
        let price = Decimal::new(10_007, 2); // 100.07
        let size = Decimal::new(333, 3); // 0.333
        let closed = fold_trade(&mut state, price, size, 5, iv(10));
        assert_eq!(closed, None);
        let active = state.expect("window opened");
        assert_eq!(active.open, price);
        assert_eq!(active.high, price);
        assert_eq!(active.low, price);
        assert_eq!(active.close, price);
        assert_eq!(active.volume, size);
        assert_eq!(active.count, 1);
        assert_eq!(active.close_ts, 10);
    }

    #[test]
    fn fold_trade_non_rotating_updates_and_returns_none() {
        let mut state = None;
        let opened = fold_trade(
            &mut state,
            Decimal::new(10_000, 2),
            Decimal::new(1, 0),
            0,
            iv(10),
        );
        assert_eq!(opened, None);
        let closed = fold_trade(
            &mut state,
            Decimal::new(11_000, 2),
            Decimal::new(2, 0),
            5,
            iv(10),
        );
        assert_eq!(closed, None);
        let active = state.expect("window still open");
        assert_eq!(active.open, Decimal::new(10_000, 2));
        assert_eq!(active.high, Decimal::new(11_000, 2));
        assert_eq!(active.low, Decimal::new(10_000, 2));
        assert_eq!(active.close, Decimal::new(11_000, 2));
        assert_eq!(active.volume, Decimal::new(3, 0));
        assert_eq!(active.count, 2);
        assert_eq!(active.close_ts, 10);
    }

    #[test]
    fn fold_trade_rotating_returns_old_window_and_starts_new_one() {
        let mut state = None;
        let opened = fold_trade(
            &mut state,
            Decimal::new(10_000, 2),
            Decimal::new(1, 0),
            0,
            iv(10),
        );
        assert_eq!(opened, None);
        let closed = fold_trade(
            &mut state,
            Decimal::new(12_000, 2),
            Decimal::new(1, 0),
            10,
            iv(10),
        )
        .expect("rotate closes the old window");
        assert_eq!(closed.open, Decimal::new(10_000, 2));
        assert_eq!(closed.close, Decimal::new(10_000, 2));
        assert_eq!(closed.count, 1);
        assert_eq!(closed.close_ts, 10);

        let active = state.expect("new window opened");
        assert_eq!(active.open, Decimal::new(12_000, 2));
        assert_eq!(active.high, Decimal::new(12_000, 2));
        assert_eq!(active.low, Decimal::new(12_000, 2));
        assert_eq!(active.close, Decimal::new(12_000, 2));
        assert_eq!(active.count, 1);
        assert_eq!(active.close_ts, 20);
    }

    #[test]
    fn fold_trade_multi_trade_sequence_at_nontrivial_precision() {
        let mut state = None;
        let interval = iv(100);

        // All three trades land in window [0, 100).
        assert_eq!(
            fold_trade(
                &mut state,
                Decimal::new(10_007, 2), // 100.07
                Decimal::new(333, 3),    // 0.333
                10,
                interval,
            ),
            None
        );
        assert_eq!(
            fold_trade(
                &mut state,
                Decimal::new(9_993, 2), // 99.93
                Decimal::new(333, 3),   // 0.333
                20,
                interval,
            ),
            None
        );
        assert_eq!(
            fold_trade(
                &mut state,
                Decimal::new(10_050, 2), // 100.50
                Decimal::new(333, 3),    // 0.333
                30,
                interval,
            ),
            None
        );

        // A trade at the boundary rotates and returns the closed window.
        let closed = fold_trade(
            &mut state,
            Decimal::new(9_950, 2), // 99.50
            Decimal::new(1, 1),     // 0.1
            100,
            interval,
        )
        .expect("boundary trade rotates the window");

        assert_eq!(closed.open, Decimal::new(10_007, 2)); // 100.07
        assert_eq!(closed.high, Decimal::new(10_050, 2)); // 100.50
        assert_eq!(closed.low, Decimal::new(9_993, 2)); // 99.93
        assert_eq!(closed.close, Decimal::new(10_050, 2)); // 100.50
        assert_eq!(closed.volume, Decimal::new(999, 3)); // 0.999
        assert_eq!(closed.count, 3);
        assert_eq!(closed.close_ts, 100);

        let active = state.expect("new window opened at the boundary trade");
        assert_eq!(active.open, Decimal::new(9_950, 2));
        assert_eq!(active.count, 1);
        assert_eq!(active.close_ts, 200);
    }

    /// Pins the defined outcome for an out-of-order trade, which the adapter
    /// feeds this function on purpose whenever consumer `reorder_prob` is armed
    /// (trades pass the `HavocFilter` before aggregation). The point is that the
    /// distortion is bounded and legible: the stale trade is absorbed by the
    /// window that was open when it arrived, nothing wedges, and no bar is lost,
    /// so nobody is tempted to "repair" it with buffering and thereby suppress
    /// an armed divergence.
    #[test]
    fn an_out_of_order_trade_folds_into_the_open_window_without_wedging() {
        let interval = iv(100);
        let mut state = None;

        // Window [100, 200) opens.
        assert_eq!(
            fold_trade(
                &mut state,
                Decimal::new(10_000, 2), // 100.00
                Decimal::new(1, 0),
                150,
                interval,
            ),
            None
        );

        // A trade belonging to the previous window [0, 100) arrives late. It is
        // below the active close_ts, so it folds into the open window instead of
        // reopening the earlier one: its price moves the high, and it is counted.
        assert_eq!(
            fold_trade(
                &mut state,
                Decimal::new(12_000, 2), // 120.00, an extreme the real window never saw
                Decimal::new(2, 0),
                50,
                interval,
            ),
            None
        );

        let active = state.clone().expect("the window is still open");
        assert_eq!(
            active.close_ts, 200,
            "the stale trade must not rewind the grid"
        );
        assert_eq!(
            active.open,
            Decimal::new(10_000, 2),
            "open stays the first trade seen"
        );
        assert_eq!(
            active.high,
            Decimal::new(12_000, 2),
            "the stale price reshapes the window"
        );
        assert_eq!(active.close, Decimal::new(12_000, 2));
        assert_eq!(active.volume, Decimal::new(3, 0));
        assert_eq!(active.count, 2);

        // Aggregation continues normally: the next in-order trade still rotates
        // on the original grid, so one reordered trade cannot desynchronize the
        // window boundaries for everything after it.
        let closed = fold_trade(
            &mut state,
            Decimal::new(10_100, 2),
            Decimal::new(1, 0),
            250,
            interval,
        )
        .expect("the next in-order trade rotates the window");
        assert_eq!(closed.close_ts, 200);
        assert_eq!(closed.count, 2);
        assert_eq!(
            state.expect("a fresh window opened").close_ts,
            300,
            "the grid is still epoch-anchored after the disturbance"
        );
    }
}
