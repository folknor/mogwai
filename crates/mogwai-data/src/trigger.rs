// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded tape walks used by the venue's trigger-price fill model.

use mogwai_protocol::{Hit, ScanKind, Side};
use rust_decimal::{Decimal, prelude::ToPrimitive};

use crate::{TickEvent, TickSource};

/// Trailing window the realized-volatility estimator samples, ending inclusively
/// at the reading instant. A const, not a knob: it sets the estimator's
/// identity, and a venue whose fill model changes shape per deployment is not a
/// venue anyone can compare runs across.
pub const VOL_WINDOW_NS: u64 = 300_000_000_000;
/// The horizon a resting order is plausibly exposed to between arrival and the
/// flow that could consume it. The per-print RMS return carries no time
/// dimension, so it is scaled up to this before it can price a band; sitting
/// comfortably above any sweep interval the config validator permits is what
/// makes the band a claim about the span an order actually waits out.
pub const FILL_HORIZON_NS: u64 = 60_000_000_000;
/// The count of returns below which a reading is refused rather than reported -
/// a sample-size floor, not a threshold on the return values. Refusing is
/// the conservative answer: a zero band is the most permissive fill regime the
/// venue has, so answering "no evidence" with it would be backwards.
pub const MIN_VOL_SAMPLES: usize = 8;

#[derive(Debug, Clone, Copy)]
pub struct TriggerScan {
    pub side: Side,
    /// The price the predicate is applied against: a live limit's drawn band
    /// trigger, or an untriggered conditional's stop price.
    pub px: Decimal,
    /// Which predicate decides this scan - `trades_through` or
    /// `touches_trigger`. The engine classifies, this walk only evaluates.
    pub kind: ScanKind,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
}

#[derive(Debug, Clone)]
pub struct Walk {
    /// Per scan, in the input's order: the first print in
    /// `(scan.from_ns, reached_ns]` that satisfied its predicate, or `None`.
    pub hits: Vec<Option<Hit>>,
    /// Where the drain actually got to, which its budget may leave short of
    /// `to_ns`. A caller advances its frontier to exactly this, never past it.
    pub reached_ns: u64,
    /// Ticks pulled, for the cost gates.
    pub drained: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct VolReading {
    /// Last print at or before `to_ns`. Only ever produced by a walk that
    /// actually reached `to_ns`.
    pub last_px: Decimal,
    /// Instant of that last print. Carried so a stop that triggers on arrival
    /// can name a real print off the canonical tape rather than inventing one.
    pub last_ts_ns: u64,
    /// RMS of one print's return, unitless.
    pub rms_return: f64,
    /// `rms_return` scaled to `FILL_HORIZON_NS` by the observed arrival rate.
    /// This is the number the band formula multiplies.
    pub horizon_return: f64,
    /// Returns collected, one fewer than the prints seen.
    pub samples: usize,
    /// Sim-time span the samples actually covered. Reported so a calibration
    /// probe can show the scaling's inputs rather than only its output.
    pub span_ns: u64,
}

/// Walk one tape once for every scan on it, reporting which triggers a print
/// went strictly through.
///
/// One walk per tape shared by every scan, per-scan `from_ns`, early return
/// once every scan has triggered, and a total-function empty-scan branch: the
/// structural properties the fill sweep is built on, unchanged from the
/// penetration counter this replaced.
#[must_use]
pub fn scan_triggers(
    source: &mut dyn TickSource,
    scans: &[TriggerScan],
    to_ns: u64,
    budget: usize,
) -> Walk {
    let Some(earliest) = scans.iter().map(|scan| scan.from_ns).min() else {
        // The one place in either walk where `reached_ns` is asserted rather
        // than proved, and it is sound only because of what a frontier means
        // here. It is not a claim that the tape up to `to_ns` was read; it is a
        // claim that nothing between the caller's old frontier and `to_ns` can
        // still be owed a hit. With no scans there is no predicate to owe one
        // to, and this branch reads no tick, so it consumes no frontier either -
        // `drained: 0` is the proof of that, and every other exit below earns
        // its `reached_ns` from an event with a later timestamp. If a caller
        // ever derives something from `reached_ns` other than "no scan was
        // missed" - a data-completeness claim, a checkpoint, a cache key - this
        // branch stops being true and must start returning the caller's own
        // `from`.
        return Walk {
            hits: Vec::new(),
            reached_ns: to_ns,
            drained: 0,
        };
    };
    let mut hits = vec![None; scans.len()];
    // Per-print pre-filter, added when the tape went to ~50 raw fills a second:
    // a pass that used to see a handful of prints now sees thousands, and the
    // inner loop is O(scans) per print. Each of the four bounds is the extreme
    // of one (kind, side) group in the direction that group's predicate opens,
    // so "no scan in this group can hit" is exact rather than heuristic -
    // FillThrough Buy hits on `price < px`, so nothing hits once the price is
    // at or above the largest such `px`, and symmetrically for the other three.
    // A skipped print therefore cannot hide a hit, and the `all(is_some)` early
    // return is safe to skip with it because no hit was recorded.
    let mut fill_buy_max: Option<Decimal> = None;
    let mut fill_sell_min: Option<Decimal> = None;
    let mut touch_buy_min: Option<Decimal> = None;
    let mut touch_sell_max: Option<Decimal> = None;
    // `TouchedTrigger` - the touched-order family. It opens in the
    // opposite direction to the stop family and the same one as a limit: a
    // touched buy hits at or below its trigger, so its bound is the largest such
    // trigger, and symmetrically for a sell. Kept
    // as its own pair rather than folded into the `fill_*` slots, because the
    // strictness differs - `<=` against `<` - and sharing a bound would make the
    // prune drop a print that lands exactly on a touched trigger.
    let mut toward_buy_max: Option<Decimal> = None;
    let mut toward_sell_min: Option<Decimal> = None;
    for scan in scans {
        let slot = match (scan.kind, scan.side) {
            (ScanKind::FillThrough, Side::Buy) => &mut fill_buy_max,
            (ScanKind::FillThrough, Side::Sell) => &mut fill_sell_min,
            (ScanKind::StopTrigger, Side::Buy) => &mut touch_buy_min,
            (ScanKind::StopTrigger, Side::Sell) => &mut touch_sell_max,
            (ScanKind::TouchedTrigger, Side::Buy) => &mut toward_buy_max,
            (ScanKind::TouchedTrigger, Side::Sell) => &mut toward_sell_min,
        };
        // Which extreme this group's predicate opens toward: a group whose
        // predicate admits prices below its bound keeps the largest, one that
        // admits prices above keeps the smallest.
        let keeps_max = matches!(
            (scan.kind, scan.side),
            (ScanKind::FillThrough | ScanKind::TouchedTrigger, Side::Buy)
                | (ScanKind::StopTrigger, Side::Sell)
        );
        *slot = Some(match *slot {
            Some(bound) if keeps_max => bound.max(scan.px),
            Some(bound) => bound.min(scan.px),
            None => scan.px,
        });
    }
    // `reached_ns` names a timestamp this walk has drained completely, because
    // the caller advances an exclusive frontier to it and never revisits what it
    // passed. Completeness at an instant is only established by an event with a
    // later one, or by the source ending: a parent publishes its quote and its
    // first print at a single instant, and a source at coarser resolution can
    // carry many prints there, so stopping at the first event whose timestamp
    // equals `to_ns` would let the caller step over everything else sharing it -
    // a trigger crossed by a later child, silently never filled.
    let mut reached_ns = earliest;
    let mut last_ts: Option<u64> = None;
    let mut drained = 0;
    let mut proved_past = false;
    for _ in 0..budget {
        let Some(event) = source.next_tick() else {
            // The source ended, so its final instant carries nothing further and
            // is itself fully drained.
            if let Some(previous) = last_ts {
                reached_ns = reached_ns.max(previous);
            }
            proved_past = true;
            break;
        };
        drained += 1;
        if event.ts_event() > to_ns {
            reached_ns = to_ns;
            proved_past = true;
            break;
        }
        if let Some(previous) = last_ts
            && event.ts_event() != previous
        {
            reached_ns = reached_ns.max(previous);
        }
        last_ts = Some(event.ts_event());
        if let TickEvent::Trade(trade) = event {
            let any_price_can_hit = fill_buy_max.is_some_and(|px| trade.price < px)
                || fill_sell_min.is_some_and(|px| trade.price > px)
                || touch_buy_min.is_some_and(|px| trade.price >= px)
                || touch_sell_max.is_some_and(|px| trade.price <= px)
                || toward_buy_max.is_some_and(|px| trade.price <= px)
                || toward_sell_min.is_some_and(|px| trade.price >= px);
            if !any_price_can_hit {
                continue;
            }
            for (hit, scan) in hits.iter_mut().zip(scans) {
                if hit.is_none()
                    && trade.ts_event > scan.from_ns
                    && scan.kind.hit(scan.side, scan.px, trade.price)
                {
                    *hit = Some(Hit {
                        ts_ns: trade.ts_event,
                        px: trade.price,
                    });
                }
            }
            if hits.iter().all(Option::is_some) {
                // Every scan is answered, so the walk stops here - but the
                // frontier it reports is still only what it proved, which is the
                // last instant it saw past. Reporting this print's own timestamp
                // would hand the caller a frontier covering events at that
                // instant the walk never read.
                return Walk {
                    hits,
                    reached_ns,
                    drained,
                };
            }
        }
    }
    if !proved_past && let Some(last) = last_ts {
        // The budget ran out mid-stream. Every instant strictly before the last
        // one seen is drained - the source is time-ordered and was read
        // contiguously - so the frontier can advance to just short of it. That
        // instant itself may still carry events this walk never pulled.
        reached_ns = reached_ns.max(last.saturating_sub(1));
    }
    Walk {
        hits,
        reached_ns,
        drained,
    }
}

/// One walk of `(from_ns, to_ns]` producing the trailing realized volatility
/// and the last print.
///
/// `None` in every case where the reading would be untrue rather than merely
/// imprecise: the span carries no trade, it carries fewer than
/// `MIN_VOL_SAMPLES` returns, its samples span zero sim time, the walk stopped
/// short of `to_ns` (a spent `budget`, or a tape that ended), or a price failed
/// to convert to a finite `f64`. A caller never receives a partial or stale
/// reading: `last_px` is documented as the last print at or before the reading
/// instant and the submit path decides marketability against exactly that
/// number, so an older print in that field would be a lie rather than an
/// approximation.
#[must_use]
pub fn vol_reading(
    source: &mut dyn TickSource,
    from_ns: u64,
    to_ns: u64,
    budget: usize,
) -> Option<VolReading> {
    let mut prices = Vec::new();
    let mut first_ts = None;
    let mut last_ts = None;
    let mut last_px = None;
    // Same frontier rule as `scan_triggers`, and the stake here is `last_px`.
    // That field is contractually the last print at or before the reading
    // instant, and the submit path decides marketability against exactly it, so
    // stopping at the first event that merely equals `to_ns` returns whichever
    // print happened to come first at that instant - the quote's own parent when
    // the tape is protocol 7, an arbitrary one when the source is coarser.
    let mut reached_ns = from_ns;
    // Distinct from `last_ts`, which is the last print and feeds `last_ts_ns`
    // and `span_ns`. This one is the last event of any kind, and exists only to
    // decide when an instant has been drained.
    let mut last_event_ts: Option<u64> = None;
    let mut drained = 0;
    let mut proved_past = false;
    for _ in 0..budget {
        let Some(event) = source.next_tick() else {
            if let Some(previous) = last_event_ts {
                reached_ns = reached_ns.max(previous);
            }
            proved_past = true;
            break;
        };
        drained += 1;
        if event.ts_event() > to_ns {
            reached_ns = to_ns;
            proved_past = true;
            break;
        }
        if let Some(previous) = last_event_ts
            && event.ts_event() != previous
        {
            reached_ns = reached_ns.max(previous);
        }
        last_event_ts = Some(event.ts_event());
        if let TickEvent::Trade(trade) = event
            && trade.ts_event > from_ns
        {
            let price = trade.price.to_f64()?;
            if !price.is_finite() {
                return None;
            }
            first_ts.get_or_insert(trade.ts_event);
            last_ts = Some(trade.ts_event);
            last_px = Some(trade.price);
            prices.push(price);
        }
    }
    if !proved_past && let Some(last) = last_event_ts {
        reached_ns = reached_ns.max(last.saturating_sub(1));
    }
    // A walk that stopped short holds the oldest part of the window - it starts
    // at a checkpoint before the window opens and collects forward - so its
    // `last_px` would be a print from well before `to_ns`. Whether the budget
    // was spent or the tape ended, short of `to_ns` is no reading at all.
    if reached_ns < to_ns {
        debug_assert!(drained <= budget);
        return None;
    }
    let samples = prices.len().saturating_sub(1);
    if samples < MIN_VOL_SAMPLES {
        return None;
    }
    let span_ns = last_ts?.saturating_sub(first_ts?);
    if span_ns == 0 {
        return None;
    }
    let sum_sq = prices.windows(2).try_fold(0.0, |sum, pair| {
        if pair[0] == 0.0 {
            return None;
        }
        let r = (pair[1] - pair[0]) / pair[0];
        r.is_finite().then_some(sum + r * r)
    })?;
    let rms_return = (sum_sq / samples as f64).sqrt();
    let horizon_return =
        rms_return * ((samples as f64 * FILL_HORIZON_NS as f64) / span_ns as f64).sqrt();
    if !rms_return.is_finite() || !horizon_return.is_finite() {
        return None;
    }
    Some(VolReading {
        last_px: last_px?,
        last_ts_ns: last_ts?,
        rms_return,
        horizon_return,
        samples,
        span_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySource;
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
    fn scan(side: Side, price: i64, from_ns: u64) -> TriggerScan {
        TriggerScan {
            side,
            px: Decimal::from(price),
            kind: ScanKind::FillThrough,
            from_ns,
        }
    }
    /// A published book, at the same `ts_event` its parent burst prints at -
    /// which is the shape protocol 7 actually emits and the reason these walks
    /// cannot treat "an event at the horizon" as "the last print".
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

    // ------------------------------------------------------------------------
    // The trades-only assumption, driven against a hand-built stream rather than
    // asserted. Every walk in this file consumes `TickEvent`, and before the BBO
    // layer every one of them could assume the variant. These pin the repairs so
    // a future edit that reinstates the assumption fails here rather than in a
    // fill that silently never happened.
    // ------------------------------------------------------------------------

    #[test]
    fn a_quote_at_the_horizon_does_not_end_the_walk_before_its_parent_print() {
        // The quote and the print share `ts_event`, so a walk that stops on
        // "an event whose timestamp equals the horizon" never evaluates the
        // print - and the trigger it would have hit simply does not fire.
        let mut source = MemorySource::new(vec![quote(2, 100, 101), trade(2, 101)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 100, 0)], 2, 10);
        assert_eq!(
            walk.hits[0],
            Some(Hit {
                ts_ns: 2,
                px: Decimal::from(101)
            }),
            "the print sharing the horizon with its quote was never evaluated"
        );
    }

    #[test]
    fn a_later_print_sharing_the_horizon_still_triggers() {
        // The first print at `to_ns` cannot end the walk: a coarser source packs
        // many prints into one instant, and only the second one here crosses.
        let mut source = MemorySource::new(vec![quote(2, 100, 101), trade(2, 100), trade(2, 105)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 103, 0)], 2, 10);
        assert_eq!(
            walk.hits[0],
            Some(Hit {
                ts_ns: 2,
                px: Decimal::from(105)
            }),
            "the walk stopped on the first print at the horizon"
        );
    }

    #[test]
    fn a_frontier_is_only_claimed_once_the_walk_has_passed_it() {
        // The budget runs out between the quote and the print that shares its
        // instant. Claiming `to_ns` here would advance the caller's exclusive
        // frontier past a print it never read.
        let mut source = MemorySource::new(vec![trade(1, 99), quote(2, 100, 101), trade(2, 105)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 103, 0)], 2, 2);
        assert!(walk.hits[0].is_none());
        assert_eq!(
            walk.reached_ns, 1,
            "a walk that stopped mid-instant claimed that instant"
        );
    }

    #[test]
    fn a_source_that_ends_proves_its_final_instant_complete() {
        let mut source = MemorySource::new(vec![trade(1, 99), quote(2, 100, 101), trade(2, 105)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 1, 0)], 9, 100);
        assert!(walk.hits[0].is_none());
        assert_eq!(walk.reached_ns, 2, "an exhausted source drained everything");
    }

    #[test]
    fn a_quote_costs_the_walk_a_tick_of_its_budget() {
        // Not incidental: the sweep budget is denominated in ticks pulled, and
        // protocol 7 adds one quote per parent burst. A budget sized against
        // trade counts alone under-serves every walk by that factor.
        let mut source = MemorySource::new(vec![quote(1, 98, 99), trade(1, 99)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 100, 0)], 1, 10);
        assert_eq!(walk.drained, 2);
    }

    #[test]
    fn a_budget_spent_entirely_on_quotes_leaves_the_walk_short() {
        let mut source = MemorySource::new(vec![
            quote(1, 98, 99),
            quote(2, 98, 99),
            quote(3, 98, 99),
            trade(4, 101),
        ]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 100, 0)], 4, 2);
        assert!(walk.hits[0].is_none());
        assert!(
            walk.reached_ns < 4,
            "a short walk must not claim the horizon"
        );
    }

    #[test]
    fn vol_reading_ignores_quotes_and_still_ends_on_the_last_print() {
        // `last_px` is contractually the last print at or before the reading
        // instant, and the submit path decides marketability against exactly it.
        // A quote sharing the horizon's timestamp must not become the stopping
        // point, or that field silently holds the previous print.
        let mut ticks = Vec::new();
        for i in 1..=12_u64 {
            ticks.push(quote(i, 99, 100));
            ticks.push(trade(i, 100 + i as i64));
        }
        let mut source = MemorySource::new(ticks);
        let reading = vol_reading(&mut source, 0, 12, 100).expect("a full window reads");
        assert_eq!(reading.last_px, Decimal::from(112));
        assert_eq!(reading.last_ts_ns, 12);
        assert_eq!(
            reading.samples, 11,
            "quotes must not enter the return series"
        );
    }

    #[test]
    fn vol_reading_takes_the_last_print_of_a_shared_horizon_instant() {
        // `last_px` is the last print at or before the reading instant. Several
        // prints can share that instant, and the walk must land on the last of
        // them rather than on whichever arrived first.
        let mut ticks = Vec::new();
        for i in 1..=12_u64 {
            ticks.push(quote(i, 99, 100));
            ticks.push(trade(i, 100 + i as i64));
        }
        ticks.push(trade(12, 500));
        let mut source = MemorySource::new(ticks);
        let reading = vol_reading(&mut source, 0, 12, 100).expect("a full window reads");
        assert_eq!(reading.last_px, Decimal::from(500));
    }

    #[test]
    fn vol_reading_refuses_a_window_it_stopped_inside() {
        // The budget ends immediately after the quote at the horizon, so the
        // print sharing that instant was never consumed. Reporting a reading
        // here hands back the previous print as `last_px` while claiming the
        // window was covered - the silent staleness this function refuses.
        let mut ticks = Vec::new();
        for i in 1..=12_u64 {
            ticks.push(quote(i, 99, 100));
            ticks.push(trade(i, 100 + i as i64));
        }
        let mut source = MemorySource::new(ticks);
        // 11 full pairs, then the horizon's quote alone.
        assert!(vol_reading(&mut source, 0, 12, 23).is_none());
    }

    #[test]
    fn a_walk_reports_every_scan_that_triggered() {
        let mut source = MemorySource::new(vec![trade(1, 99), trade(2, 101)]);
        let walk = scan_triggers(
            &mut source,
            &[scan(Side::Buy, 100, 0), scan(Side::Sell, 100, 0)],
            2,
            10,
        );
        assert!(walk.hits.iter().all(Option::is_some));
    }
    #[test]
    fn a_walk_reports_the_price_and_instant_of_each_hit() {
        let mut source = MemorySource::new(vec![trade(1, 99), trade(2, 101)]);
        let walk = scan_triggers(
            &mut source,
            &[
                scan(Side::Buy, 100, 0),
                scan(Side::Sell, 100, 1),
                scan(Side::Buy, 90, 0),
            ],
            2,
            10,
        );
        assert_eq!(
            walk.hits[0],
            Some(Hit {
                ts_ns: 1,
                px: Decimal::from(99)
            })
        );
        assert_eq!(
            walk.hits[1],
            Some(Hit {
                ts_ns: 2,
                px: Decimal::from(101)
            })
        );
        assert_eq!(walk.hits[2], None);
    }

    #[test]
    fn a_touch_scan_hits_at_the_price_and_a_through_scan_does_not() {
        let at = Decimal::from(100);
        let mut source = MemorySource::new(vec![trade(1, 100)]);
        let scans = [
            TriggerScan {
                side: Side::Buy,
                px: at,
                kind: ScanKind::StopTrigger,
                from_ns: 0,
            },
            TriggerScan {
                side: Side::Buy,
                px: at,
                kind: ScanKind::FillThrough,
                from_ns: 0,
            },
        ];
        let walk = scan_triggers(&mut source, &scans, 1, 10);
        assert!(walk.hits[0].is_some());
        assert!(walk.hits[1].is_none());
    }
    #[test]
    fn a_print_that_jumps_past_a_trigger_still_triggers() {
        let mut source = MemorySource::new(vec![trade(1, 90)]);
        assert!(scan_triggers(&mut source, &[scan(Side::Buy, 100, 0)], 1, 10).hits[0].is_some());
    }
    #[test]
    fn a_walk_stops_once_every_scan_has_triggered() {
        let mut source = MemorySource::new(vec![trade(1, 99), trade(2, 98)]);
        assert_eq!(
            scan_triggers(&mut source, &[scan(Side::Buy, 100, 0)], 2, 10).drained,
            1
        );
    }
    #[test]
    fn walk_with_a_spent_budget_reports_only_what_it_proved() {
        // Was `(1, 1)`. Consuming the print at instant 1 does not establish that
        // instant 1 is finished - another event could share it - and the caller
        // advances an exclusive frontier to this number, so claiming 1 here
        // would let it step over anything else at that instant. Under-claiming
        // costs a re-walk; over-claiming loses a fill permanently.
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 100)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 99, 0)], 2, 1);
        assert_eq!((walk.reached_ns, walk.drained), (0, 1));
    }
    #[test]
    fn walk_pulls_exactly_one_event_past_the_boundary_to_prove_it() {
        // Was `walk_stops_at_an_exact_boundary_without_pulling_past_it`,
        // asserting `(2, 2)`. That efficiency property is incompatible with
        // correctness: the only evidence that instant 2 is fully drained is an
        // event at a later instant, so the walk must pull it. One extra tick of
        // budget buys the frontier its exclusive semantics depend on.
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 100), trade(3, 100)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 99, 0)], 2, 10);
        assert_eq!((walk.reached_ns, walk.drained), (2, 3));
    }
    #[test]
    fn walk_batches_every_scan_into_one_pass() {
        let ticks = vec![trade(1, 99), trade(2, 101)];
        let scans = [scan(Side::Buy, 100, 0), scan(Side::Sell, 100, 0)];
        let mut source = MemorySource::new(ticks.clone());
        let both = scan_triggers(&mut source, &scans, 2, 10);
        for (i, one) in scans.iter().enumerate() {
            let mut source = MemorySource::new(ticks.clone());
            assert_eq!(
                both.hits[i].is_some(),
                scan_triggers(&mut source, &[*one], 2, 10).hits[0].is_some()
            );
        }
    }
    #[test]
    fn walk_over_an_empty_scan_list_pulls_nothing() {
        let mut source = MemorySource::new(vec![trade(1, 100)]);
        let walk = scan_triggers(&mut source, &[], 9, 10);
        assert!(walk.hits.is_empty());
        assert_eq!((walk.reached_ns, walk.drained), (9, 0));
    }

    fn reading_ticks(cadence: u64, count: usize) -> Vec<TickEvent> {
        (0..count)
            .map(|i| trade((i as u64 + 1) * cadence, 100 + i as i64))
            .collect()
    }
    #[test]
    fn a_reading_refuses_below_the_minimum_sample_count() {
        let mut source = MemorySource::new(reading_ticks(1, MIN_VOL_SAMPLES));
        assert!(vol_reading(&mut source, 0, MIN_VOL_SAMPLES as u64, 100).is_none());
    }
    #[test]
    fn a_window_excludes_prints_outside_its_bounds() {
        let mut ticks = vec![trade(10, 999)];
        ticks.extend((1..=10).map(|i| trade(10 + i, 100 + i as i64)));
        ticks.push(trade(21, 777));
        let mut source = MemorySource::new(ticks);
        let reading = vol_reading(&mut source, 10, 20, 20).unwrap();
        assert_eq!(reading.last_px, Decimal::from(110));
        assert_eq!(reading.samples, 9);
    }
    #[test]
    fn a_reading_refuses_when_the_walk_exhausts_its_budget() {
        let mut source = MemorySource::new(reading_ticks(1, 20));
        assert!(vol_reading(&mut source, 0, 20, 10).is_none());
    }
    #[test]
    fn a_reading_refuses_a_zero_span_window() {
        let mut source = MemorySource::new((0..10).map(|i| trade(1, 100 + i)).collect());
        assert!(vol_reading(&mut source, 0, 1, 20).is_none());
    }
    #[test]
    fn the_last_price_is_the_last_print_at_or_before_the_upper_bound() {
        let mut ticks = reading_ticks(1, 10);
        ticks.push(trade(11, 999));
        let mut source = MemorySource::new(ticks);
        assert_eq!(
            vol_reading(&mut source, 0, 10, 20).unwrap().last_px,
            Decimal::from(109)
        );
    }
    #[test]
    fn horizon_scaling_is_the_square_root_of_the_observed_arrival_rate() {
        let mut source = MemorySource::new(reading_ticks(1_000_000_000, 10));
        let reading = vol_reading(&mut source, 0, 10_000_000_000, 20).unwrap();
        let expected =
            reading.rms_return * ((9.0 * FILL_HORIZON_NS as f64) / 9_000_000_000.0).sqrt();
        assert!((reading.horizon_return - expected).abs() < 1e-15);
    }
    #[test]
    fn doubling_the_arrival_rate_at_a_fixed_per_print_move_scales_the_band_by_sqrt_two() {
        let mut slow = MemorySource::new(reading_ticks(2_000_000_000, 10));
        let mut fast = MemorySource::new(reading_ticks(1_000_000_000, 10));
        let a = vol_reading(&mut slow, 0, 20_000_000_000, 20)
            .unwrap()
            .horizon_return;
        let b = vol_reading(&mut fast, 0, 10_000_000_000, 20)
            .unwrap()
            .horizon_return;
        assert!((b / a - 2.0_f64.sqrt()).abs() < 1e-12);
    }
}
