// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Folding wire trades into nautilus bars.
//!
//! This is the one implementation both of the data client's bar feeds run
//! through: `BarAggregator::push` is the live subscription path and
//! `aggregate_bars` the history response path. It is public so a consumer can
//! witness what the adapter actually stamps - chiefly that a bar's `ts_event`
//! is the end of its interval, never its open - by driving the same code the
//! client drives rather than reading it. It is not offered as a general bar
//! engine: it admits exactly the bar types the data client admits, and folds
//! exactly as the client folds.

use std::num::NonZeroU64;

use anyhow::ensure;
use mogwai_data::{BarAcc, fold_trade};
use mogwai_protocol::{InstrumentDef, TradeTick};
use nautilus_core::UnixNanos;
use nautilus_model::{
    data::{Bar, BarType, bar::get_bar_interval_ns},
    enums::BarAggregation,
};

use crate::convert;

/// Refuses a bar type this adapter cannot fold, and returns its interval.
///
/// Tick and volume bars are refused because the fold is by time. Week, Month
/// and Year are refused because nautilus anchors them to the calendar
/// (`get_time_bar_start` anchors weeks to Monday and months and years to the
/// calendar) while `get_bar_interval_ns` returns a fixed 7-day, 30-day or
/// 365-day proxy - nautilus's own comment calls it a proxy "for comparing bar
/// lengths", not a calendar interval - so the epoch-anchored fold would
/// produce 30-day blocks instead of months and Thursday-anchored weeks. Day and
/// finer are correctly UTC-aligned. Refusing is the chosen resolution over
/// building calendar anchoring, which is heavy and an unlikely bar spec for
/// this venue.
///
/// # Errors
///
/// Returns an error naming the refused aggregation.
pub fn admit(bar_type: &BarType) -> anyhow::Result<NonZeroU64> {
    ensure!(
        bar_type.spec().is_time_aggregated(),
        "mogwai only supports time based external bars"
    );
    ensure!(
        !matches!(
            bar_type.spec().aggregation,
            BarAggregation::Week | BarAggregation::Month | BarAggregation::Year
        ),
        "mogwai does not support Week/Month/Year bars: they need calendar \
         anchoring this adapter's epoch-anchored aggregation cannot produce; \
         use Day or finer"
    );
    NonZeroU64::new(get_bar_interval_ns(bar_type).as_u64())
        .ok_or_else(|| anyhow::anyhow!("bar type {bar_type} has a zero interval"))
}

/// One bar type's open window, folding trades as they arrive.
#[derive(Debug)]
pub struct BarAggregator {
    bar_type: BarType,
    interval: NonZeroU64,
    active: Option<BarAcc>,
}

impl BarAggregator {
    /// # Errors
    ///
    /// Returns an error when `admit` refuses the bar type.
    pub fn new(bar_type: BarType) -> anyhow::Result<Self> {
        Ok(Self {
            interval: admit(&bar_type)?,
            bar_type,
            active: None,
        })
    }

    /// Folds one trade, returning the bar for the window it closed, if any.
    ///
    /// A window's bar is emitted lazily, when a later trade crosses its end:
    /// nothing closes a window on a timer. `ts_init` is the caller's receipt
    /// stamp, which the live path takes from the clock at conversion.
    pub fn push(
        &mut self,
        trade: &TradeTick,
        def: &InstrumentDef,
        ts_init: UnixNanos,
    ) -> Option<Bar> {
        self.fold(trade, def, Some(ts_init))
    }

    /// `None` for `ts_init` is the history fold, where the closed window's own
    /// close is the bar's `ts_init` - see `acc_to_bar` for why history must not
    /// stamp the conversion clock.
    fn fold(
        &mut self,
        trade: &TradeTick,
        def: &InstrumentDef,
        ts_init: Option<UnixNanos>,
    ) -> Option<Bar> {
        // The window has already rotated inside `fold_trade` by the time this
        // returns, so the "one bad bar doesn't wedge aggregation" property is
        // structural: the rotation no longer depends on the conversion below
        // succeeding. A hostile open/high/low/close/volume that overflows
        // nautilus Price/Quantity just drops this one bar with a warning.
        let closed = fold_trade(
            &mut self.active,
            trade.price,
            trade.size,
            trade.ts_event,
            self.interval,
        )?;
        let ts_init = ts_init.unwrap_or_else(|| UnixNanos::from(closed.close_ts));
        match acc_to_bar(self.bar_type, &closed, def, ts_init) {
            Ok(bar) => Some(bar),
            Err(err) => {
                tracing::warn!(bar_type = %self.bar_type, error = %err, "dropping unrepresentable bar");
                None
            }
        }
    }

    /// Takes the open window if it has already ended by `now`, as the bar the
    /// lazy emit rule withheld for lack of a later trade. A window still in
    /// progress is left in place: shipping it would inject a future-stamped,
    /// incomplete bar a consumer could not tell from a real one. A taken window
    /// is cleared even when its conversion fails, so a second call cannot emit
    /// it twice.
    pub(crate) fn take_closed(
        &mut self,
        def: &InstrumentDef,
        now: UnixNanos,
    ) -> Option<anyhow::Result<Bar>> {
        if self.active.as_ref()?.close_ts > now.as_u64() {
            return None;
        }
        let active = self.active.take()?;
        Some(acc_to_bar(self.bar_type, &active, def, now))
    }
}

/// Folds a run of historical trades into the bars they close, stamping each
/// bar's `ts_init` with its own close.
///
/// The trailing window is flushed only when `end` proves it fully elapsed. A
/// window's bar is otherwise emitted lazily, when a later trade crosses its
/// `close_ts` - but a historical request over a window that has already passed
/// gets no such trade, so the newest complete window would be silently dropped
/// (the always-stale or missing last bar of every history request). If
/// `end >= close_ts` the window closed within the requested range and is
/// emitted; a genuinely partial trailing window (`end` inside it, or an unknown
/// `end`) is dropped, matching the live path.
///
/// # Errors
///
/// Returns an error when `admit` refuses the bar type.
pub fn aggregate_bars(
    bar_type: &BarType,
    trades: &[TradeTick],
    def: &InstrumentDef,
    end: Option<UnixNanos>,
) -> anyhow::Result<Vec<Bar>> {
    let mut window = BarAggregator::new(*bar_type)?;
    let mut out: Vec<Bar> = trades
        .iter()
        .filter_map(|trade| window.fold(trade, def, None))
        .collect();
    if let (Some(acc), Some(end)) = (&window.active, end)
        && end.as_u64() >= acc.close_ts
    {
        match acc_to_bar(*bar_type, acc, def, UnixNanos::from(acc.close_ts)) {
            Ok(bar) => out.push(bar),
            Err(err) => {
                tracing::warn!(%bar_type, error = %err, "dropping unrepresentable trailing bar");
            }
        }
    }
    Ok(out)
}

/// `ts_init` is the caller's decision because the two producers mean different
/// things by it: the live subscription path stamps receipt time (the clock at
/// conversion), while a history response stamps the bar's own close - a
/// conversion-time stamp there lands past the response's pinned `end` and
/// nautilus's trim-on-`ts_init` empties the response.
fn acc_to_bar(
    bar_type: BarType,
    acc: &BarAcc,
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> anyhow::Result<Bar> {
    Ok(Bar::new(
        bar_type,
        convert::price(acc.open, def.price_precision)?,
        convert::price(acc.high, def.price_precision)?,
        convert::price(acc.low, def.price_precision)?,
        convert::price(acc.close, def.price_precision)?,
        convert::quantity(acc.volume, def.size_precision)?,
        UnixNanos::from(acc.close_ts),
        ts_init,
    ))
}

#[cfg(test)]
mod tests {
    use rust_decimal::Decimal;

    use super::*;

    const MINUTE_NS: u64 = 60_000_000_000;
    const OPEN_NS: u64 = 1_700_000_040 * 1_000_000_000;

    fn trade(def: &InstrumentDef, ts_event: u64) -> TradeTick {
        TradeTick {
            symbol: std::sync::Arc::clone(&def.symbol),
            price: Decimal::from(100),
            size: Decimal::ONE,
            aggressor: mogwai_protocol::AggressorSide::Buyer,
            ts_event,
        }
    }

    fn minute_bars(def: &InstrumentDef) -> BarType {
        BarType::from(format!("{}.MOGWAI-1-MINUTE-LAST-EXTERNAL", def.symbol).as_str())
    }

    /// Both feeds stamp a bar's `ts_event` at the end of its interval, not its
    /// open and not the last instant inside it. The trade sits mid-window so
    /// an open-stamp and an end-stamp cannot coincide with the trade's own time.
    #[test]
    fn both_feeds_stamp_the_interval_end() {
        let def = mogwai_protocol::default_instruments().remove(0);
        let bar_type = minute_bars(&def);
        let first = trade(&def, OPEN_NS + MINUTE_NS / 2);

        let history = aggregate_bars(
            &bar_type,
            std::slice::from_ref(&first),
            &def,
            Some(UnixNanos::from(OPEN_NS + MINUTE_NS)),
        )
        .expect("a minute bar is admitted");
        assert_eq!(history.len(), 1, "the elapsed window flushes on `end`");
        assert_eq!(history[0].ts_event.as_u64(), OPEN_NS + MINUTE_NS);
        assert_eq!(history[0].ts_init, history[0].ts_event);

        let receipt = UnixNanos::from(OPEN_NS + 5 * MINUTE_NS);
        let mut live = BarAggregator::new(bar_type).expect("a minute bar is admitted");
        assert!(
            live.push(&first, &def, receipt).is_none(),
            "the window is still open"
        );
        let bar = live
            .push(&trade(&def, OPEN_NS + MINUTE_NS), &def, receipt)
            .expect("a trade at the boundary closes the window");
        assert_eq!(bar.ts_event.as_u64(), OPEN_NS + MINUTE_NS);
        assert_eq!(
            bar.ts_init, receipt,
            "the live path stamps receipt, not close"
        );
    }

    #[test]
    fn calendar_and_tick_bars_are_refused() {
        let def = mogwai_protocol::default_instruments().remove(0);
        for spec in ["1-WEEK", "1-MONTH", "100-TICK"] {
            let bar_type =
                BarType::from(format!("{}.MOGWAI-{spec}-LAST-EXTERNAL", def.symbol).as_str());
            assert!(
                BarAggregator::new(bar_type).is_err(),
                "{spec} must be refused"
            );
            assert!(aggregate_bars(&bar_type, &[], &def, None).is_err());
        }
    }
}
