// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Bounded tape walks used by the venue's trigger-price fill model.
//!
//! Every walk here reads one river's compact records. Two rules bind all of
//! them, and both are about what a walk may claim rather than what it reads:
//!
//! - An instant is complete only once an event at a strictly later instant has
//!   been pulled, or the river ended cleanly. A parent publishes its quote and
//!   its first print at one instant, and a coarser river can carry many prints
//!   there, so the first event at an instant proves nothing about the rest.
//! - A river that stopped on a fault ended nowhere in particular. `None` from
//!   `next_record` is clean exhaustion only when `fault()` is empty; a faulted
//!   walk claims no completion past what it proved before the fault, and
//!   reports the fault so its caller cannot retire a frontier over work the
//!   river never produced.

use mogwai_protocol::{Hit, HitBook, ScanKind, Side};
use rust_decimal::Decimal;

use crate::TickFault;
use crate::record::{QuoteRec, Record, RecordScale, RiverSource, Round};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookState {
    pub bid_px: Decimal,
    pub ask_px: Decimal,
    pub bid_sz: Decimal,
    pub ask_sz: Decimal,
    pub ts_ns: u64,
}

impl BookState {
    /// A quote record read at its river's scale.
    #[must_use]
    pub fn from_quote(scale: &RecordScale, quote: &QuoteRec) -> Self {
        Self {
            bid_px: scale.price(quote.bid_px),
            ask_px: scale.price(quote.ask_px),
            bid_sz: scale.size(quote.bid_qty),
            ask_sz: scale.size(quote.ask_qty),
            ts_ns: quote.ts,
        }
    }
}

/// The book standing at `ts_ns`: the last quote at or before it, once the walk
/// has proved the instant complete. `None` when it could not prove it - a
/// spent budget or a fault - or when no quote precedes it.
#[must_use]
pub fn book_reading(source: &mut dyn RiverSource, ts_ns: u64, budget: usize) -> Option<BookState> {
    let mut last = None;
    let mut reached = false;
    for _ in 0..budget {
        let Some(record) = source.next_record() else {
            reached = source.fault().is_none();
            break;
        };
        if record.ts() > ts_ns {
            reached = true;
            break;
        }
        if let Record::Quote(quote) = record {
            last = Some(quote);
        }
    }
    let quote = reached.then_some(last).flatten()?;
    Some(BookState::from_quote(source.scale(), &quote))
}

#[must_use]
pub fn market_snapshot_reading(
    source: &mut dyn RiverSource,
    from_ns: u64,
    to_ns: u64,
    budget: usize,
) -> Option<(Option<VolReading>, BookState)> {
    let mut trades = Vec::new();
    let mut book = None;
    let mut reached = false;
    for _ in 0..budget {
        let Some(record) = source.next_record() else {
            reached = source.fault().is_none();
            break;
        };
        if record.ts() > to_ns {
            reached = true;
            break;
        }
        match record {
            Record::Trade(trade) if trade.ts > from_ns => {
                trades.push((trade.ts, trade.px));
            }
            Record::Quote(quote) => book = Some(quote),
            Record::Trade(_) => {}
        }
    }
    if !reached {
        return None;
    }
    let scale = source.scale();
    Some((
        vol_reading_from_trades(scale, &trades),
        BookState::from_quote(scale, &book?),
    ))
}

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
    /// trigger, or an untriggered conditional's stop price. Any decimal - it
    /// need not sit on the river's grid.
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
    /// Where the drain actually got to, which its budget or a fault may leave
    /// short of `to_ns`. A caller advances its frontier to exactly this, never
    /// past it.
    pub reached_ns: u64,
    /// Records pulled, for the cost gates.
    pub drained: usize,
    /// The fault that ended the river mid-walk, if one did. `reached_ns` is
    /// then only what was proved before it.
    pub fault: Option<TickFault>,
}

/// One scan's predicate compiled onto the river's integer price scale, so a
/// print is decided by one integer comparison that agrees exactly with the
/// decimal predicate (see `RecordScale::price_threshold` for the directions).
#[derive(Debug, Clone, Copy)]
enum Compiled {
    Lt(i128),
    Le(i128),
    Gt(i128),
    Ge(i128),
}

impl Compiled {
    fn new(scale: &RecordScale, scan: &TriggerScan) -> Self {
        let ceil = || scale.price_threshold(scan.px, Round::Ceil);
        let floor = || scale.price_threshold(scan.px, Round::Floor);
        match (scan.kind, scan.side) {
            // `trades_through`: a buy fills on a print strictly below it.
            (ScanKind::FillThrough, Side::Buy) => Self::Lt(ceil()),
            (ScanKind::FillThrough, Side::Sell) => Self::Gt(floor()),
            // `touches_trigger` takes a buy stop at or above it, and
            // `touches_toward` a touched sell; the mirror pair at or below.
            (ScanKind::StopTrigger, Side::Buy) | (ScanKind::TouchedTrigger, Side::Sell) => {
                Self::Ge(ceil())
            }
            (ScanKind::StopTrigger, Side::Sell) | (ScanKind::TouchedTrigger, Side::Buy) => {
                Self::Le(floor())
            }
        }
    }

    fn holds(self, px: i64) -> bool {
        let px = i128::from(px);
        match self {
            Self::Lt(t) => px < t,
            Self::Le(t) => px <= t,
            Self::Gt(t) => px > t,
            Self::Ge(t) => px >= t,
        }
    }
}

/// Walk one tape once for every scan on it, reporting which triggers a print
/// went strictly through.
///
/// One walk per tape shared by every scan, per-scan `from_ns`, early return
/// once every scan has triggered, and a total-function empty-scan branch: the
/// structural properties the fill sweep is built on.
#[must_use]
pub fn scan_triggers(
    source: &mut dyn RiverSource,
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
        // to, and this branch reads no record, so it consumes no frontier either -
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
            fault: None,
        };
    };
    let compiled: Vec<Compiled> = scans
        .iter()
        .map(|scan| Compiled::new(source.scale(), scan))
        .collect();
    let mut hits = vec![None; scans.len()];
    // Per-print pre-filter: the inner loop is O(scans) per print, and the tape
    // runs to thousands of prints a pass. Each bound is the extreme of one
    // comparator group in the direction its predicate opens - a `<` group
    // keeps its largest threshold, a `>` group its smallest - so "no scan in
    // this group can hit" is exact rather than heuristic. A skipped print
    // therefore cannot hide a hit, and the `all(is_some)` early return is safe
    // to skip with it because no hit was recorded.
    let mut lt_max: Option<i128> = None;
    let mut le_max: Option<i128> = None;
    let mut gt_min: Option<i128> = None;
    let mut ge_min: Option<i128> = None;
    for predicate in &compiled {
        match *predicate {
            Compiled::Lt(t) => lt_max = Some(lt_max.map_or(t, |bound| bound.max(t))),
            Compiled::Le(t) => le_max = Some(le_max.map_or(t, |bound| bound.max(t))),
            Compiled::Gt(t) => gt_min = Some(gt_min.map_or(t, |bound| bound.min(t))),
            Compiled::Ge(t) => ge_min = Some(ge_min.map_or(t, |bound| bound.min(t))),
        }
    }
    // `reached_ns` names a timestamp this walk has drained completely, because
    // the caller advances an exclusive frontier to it and never revisits what it
    // passed. See the module doc for the completeness rule.
    let mut reached_ns = earliest;
    let mut last_ts: Option<u64> = None;
    let mut drained = 0;
    let mut proved_past = false;
    let mut fault = None;
    let mut last_book: Option<QuoteRec> = None;
    for _ in 0..budget {
        let Some(record) = source.next_record() else {
            fault = source.fault();
            if fault.is_none() {
                // The river ended, so its final instant carries nothing
                // further and is itself fully drained.
                if let Some(previous) = last_ts {
                    reached_ns = reached_ns.max(previous);
                }
                proved_past = true;
            }
            break;
        };
        drained += 1;
        if record.ts() > to_ns {
            reached_ns = to_ns;
            proved_past = true;
            break;
        }
        if let Some(previous) = last_ts
            && record.ts() != previous
        {
            reached_ns = reached_ns.max(previous);
        }
        last_ts = Some(record.ts());
        let trade = match record {
            Record::Quote(quote) => {
                last_book = Some(quote);
                continue;
            }
            Record::Trade(trade) => trade,
        };
        let px = i128::from(trade.px);
        let any_price_can_hit = lt_max.is_some_and(|t| px < t)
            || le_max.is_some_and(|t| px <= t)
            || gt_min.is_some_and(|t| px > t)
            || ge_min.is_some_and(|t| px >= t);
        if !any_price_can_hit {
            continue;
        }
        let scale = source.scale();
        for ((hit, scan), predicate) in hits.iter_mut().zip(scans).zip(&compiled) {
            if hit.is_none() && trade.ts > scan.from_ns && predicate.holds(trade.px) {
                // A hit with no book is still a hit. Suppressing it here
                // would leave the order resting as though the tape had
                // never reached its trigger, and the scan would re-ask the
                // same question of the same span forever; the engine
                // cancels it with a named reason instead.
                //
                // The ladder is left unresolved: this walk knows the
                // river, not the preset. The venue fills it in from the
                // resolved profile before the engine sees the walk.
                *hit = Some(Hit {
                    ts_ns: trade.ts,
                    px: scale.price(trade.px),
                    book: last_book.map(|quote| {
                        let book = BookState::from_quote(scale, &quote);
                        HitBook::unresolved(book.bid_px, book.ask_px, book.bid_sz, book.ask_sz)
                    }),
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
                fault: None,
            };
        }
    }
    if !proved_past
        && fault.is_none()
        && let Some(last) = last_ts
    {
        // The budget ran out mid-stream. Every instant strictly before the last
        // one seen is drained - the river is time-ordered and was read
        // contiguously - so the frontier can advance to just short of it. That
        // instant itself may still carry events this walk never pulled.
        reached_ns = reached_ns.max(last.saturating_sub(1));
    }
    Walk {
        hits,
        reached_ns,
        drained,
        fault,
    }
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

/// One walk of `(from_ns, to_ns]` producing the trailing realized volatility
/// and the last print.
///
/// `None` in every case where the reading would be untrue rather than merely
/// imprecise: the span carries no trade, it carries fewer than
/// `MIN_VOL_SAMPLES` returns, its samples span zero sim time, the walk stopped
/// short of `to_ns` (a spent `budget`, a fault, or a river that ended), or a
/// price failed to convert to a finite `f64`. A caller never receives a
/// partial or stale reading: `last_px` is documented as the last print at or
/// before the reading instant and the submit path decides marketability
/// against exactly that number, so an older print in that field would be a lie
/// rather than an approximation.
#[must_use]
pub fn vol_reading(
    source: &mut dyn RiverSource,
    from_ns: u64,
    to_ns: u64,
    budget: usize,
) -> Option<VolReading> {
    let mut trades: Vec<(u64, i64)> = Vec::new();
    let mut reached_ns = from_ns;
    // Distinct from the last print, which feeds `last_ts_ns` and `span_ns`.
    // This one is the last event of any kind, and exists only to decide when
    // an instant has been drained.
    let mut last_event_ts: Option<u64> = None;
    let mut proved_past = false;
    for _ in 0..budget {
        let Some(record) = source.next_record() else {
            if source.fault().is_some() {
                return None;
            }
            if let Some(previous) = last_event_ts {
                reached_ns = reached_ns.max(previous);
            }
            proved_past = true;
            break;
        };
        if record.ts() > to_ns {
            reached_ns = to_ns;
            proved_past = true;
            break;
        }
        if let Some(previous) = last_event_ts
            && record.ts() != previous
        {
            reached_ns = reached_ns.max(previous);
        }
        last_event_ts = Some(record.ts());
        if let Record::Trade(trade) = record
            && trade.ts > from_ns
        {
            trades.push((trade.ts, trade.px));
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
        return None;
    }
    vol_reading_from_trades(source.scale(), &trades)
}

/// The estimator's arithmetic over a window's trades, already collected in tape
/// order: `(ts_event, price mantissa)` for every print in `(from_ns, to_ns]`,
/// read at `scale`.
///
/// This is the one implementation of the reading's math, shared by the walk
/// above and by any holder of a resident window (the venue's per-boat rolling
/// window). Both paths must produce bit-identical readings from the same
/// trades, which is why the caller hands over record mantissas and the `f64`
/// conversion happens here, once, through `RecordScale::price_f64`, the same
/// way for everyone. The refusals (sample floor, zero span, a non-finite
/// price) are part of the estimator's identity and live here with it.
///
/// What this function cannot check, and the caller owes: that the list really
/// is every print of the window and that the window's upper bound was proved
/// reached. A caller that hands over a partial list gets a confidently wrong
/// reading, which is exactly the staleness the walk's `reached_ns` check
/// refuses.
#[must_use]
pub fn vol_reading_from_trades(scale: &RecordScale, trades: &[(u64, i64)]) -> Option<VolReading> {
    let samples = trades.len().saturating_sub(1);
    if samples < MIN_VOL_SAMPLES {
        return None;
    }
    let mut prices = Vec::with_capacity(trades.len());
    for (_, px) in trades {
        let price = scale.price_f64(*px);
        if !price.is_finite() {
            return None;
        }
        prices.push(price);
    }
    let (first_ts, _) = *trades.first()?;
    let (last_ts, last_px) = *trades.last()?;
    let span_ns = last_ts.saturating_sub(first_ts);
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
        last_px: scale.price(last_px),
        last_ts_ns: last_ts,
        rms_return,
        horizon_return,
        samples,
        span_ns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{MemoryRiver, TradeRec};
    use mogwai_protocol::{AggressorSide, Symbol};

    fn scale() -> RecordScale {
        RecordScale::new(Symbol::from("BTCUSDT"), 0, 0).unwrap()
    }
    fn river(records: Vec<Record>) -> MemoryRiver {
        MemoryRiver::new(scale(), records)
    }
    fn trade(ts: u64, price: i64) -> Record {
        Record::Trade(TradeRec {
            ts,
            px: price,
            qty: 1,
            side: AggressorSide::NoAggressor,
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
    /// A published book, at the same instant its parent burst prints at -
    /// which is the shape the generator actually emits and the reason these
    /// walks cannot treat "an event at the horizon" as "the last print".
    fn quote(ts: u64, bid: i64, ask: i64) -> Record {
        Record::Quote(QuoteRec {
            ts,
            bid_px: bid,
            ask_px: ask,
            bid_qty: 1,
            ask_qty: 1,
        })
    }

    /// A river that ends on a fault after its records.
    struct Faulting(MemoryRiver);
    impl RiverSource for Faulting {
        fn scale(&self) -> &RecordScale {
            self.0.scale()
        }
        fn next_record(&mut self) -> Option<Record> {
            self.0.next_record()
        }
        fn fault(&self) -> Option<TickFault> {
            Some(TickFault::Injected)
        }
    }

    #[test]
    fn book_reading_never_looks_ahead() {
        let mut source = river(vec![quote(10, 99, 101), quote(20, 109, 111)]);
        let book = book_reading(&mut source, 15, 10).expect("the earlier quote is covered");
        assert_eq!(book.ts_ns, 10);
        assert_eq!(book.bid_px, Decimal::from(99));
        assert_eq!(book.ask_px, Decimal::from(101));
    }

    #[test]
    fn book_reading_refuses_when_its_budget_does_not_prove_the_instant() {
        let mut source = river(vec![quote(10, 99, 101), quote(20, 109, 111)]);
        assert_eq!(book_reading(&mut source, 15, 1), None);
    }

    /// The fault rule, on every reader: a river that stopped on a fault proved
    /// nothing about the span it cut off, so no reader may treat its end as
    /// clean exhaustion. Each assertion's clean twin succeeds on the same
    /// records, so the refusal is the fault's and nothing else's.
    #[test]
    fn a_fault_is_never_read_as_completion() {
        let records = || {
            let mut records = vec![quote(1, 99, 101)];
            records.extend((1..=12).map(|i| trade(i, 100 + i as i64)));
            records
        };
        let clean = || river(records());
        let faulting = || Faulting(river(records()));

        assert!(book_reading(&mut clean(), 50, 100).is_some());
        assert!(book_reading(&mut faulting(), 50, 100).is_none());
        assert!(market_snapshot_reading(&mut clean(), 0, 50, 100).is_some());
        assert!(market_snapshot_reading(&mut faulting(), 0, 50, 100).is_none());
        assert!(vol_reading(&mut clean(), 0, 12, 100).is_some());
        assert!(vol_reading(&mut faulting(), 0, 12, 100).is_none());

        let walk = scan_triggers(&mut clean(), &[scan(Side::Buy, 1, 0)], 50, 100);
        assert_eq!((walk.reached_ns, walk.fault), (12, None));
        let walk = scan_triggers(&mut faulting(), &[scan(Side::Buy, 1, 0)], 50, 100);
        assert_eq!(walk.fault, Some(TickFault::Injected));
        assert_eq!(
            walk.reached_ns, 11,
            "a faulted walk claimed its final instant complete"
        );
    }

    // ------------------------------------------------------------------------
    // The trades-only assumption, driven against a hand-built stream rather than
    // asserted. These pin the repairs so a future edit that reinstates the
    // assumption fails here rather than in a fill that silently never happened.
    // ------------------------------------------------------------------------

    #[test]
    fn a_quote_at_the_horizon_does_not_end_the_walk_before_its_parent_print() {
        // The quote and the print share an instant, so a walk that stops on
        // "an event whose timestamp equals the horizon" never evaluates the
        // print - and the trigger it would have hit simply does not fire.
        let mut source = river(vec![quote(2, 100, 101), trade(2, 101)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 100, 0)], 2, 10);
        assert_eq!(
            walk.hits[0],
            Some(Hit {
                ts_ns: 2,
                px: Decimal::from(101),
                book: Some(HitBook::unresolved(
                    Decimal::from(100),
                    Decimal::from(101),
                    Decimal::ONE,
                    Decimal::ONE
                )),
            }),
            "the print sharing the horizon with its quote was never evaluated"
        );
    }

    #[test]
    fn a_later_print_sharing_the_horizon_still_triggers() {
        // The first print at `to_ns` cannot end the walk: a coarser source packs
        // many prints into one instant, and only the second one here crosses.
        let mut source = river(vec![quote(2, 100, 101), trade(2, 100), trade(2, 105)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 103, 0)], 2, 10);
        assert_eq!(
            walk.hits[0],
            Some(Hit {
                ts_ns: 2,
                px: Decimal::from(105),
                book: Some(HitBook::unresolved(
                    Decimal::from(100),
                    Decimal::from(101),
                    Decimal::ONE,
                    Decimal::ONE
                )),
            }),
            "the walk stopped on the first print at the horizon"
        );
    }

    #[test]
    fn a_frontier_is_only_claimed_once_the_walk_has_passed_it() {
        // The budget runs out between the quote and the print that shares its
        // instant. Claiming `to_ns` here would advance the caller's exclusive
        // frontier past a print it never read.
        let mut source = river(vec![trade(1, 99), quote(2, 100, 101), trade(2, 105)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Sell, 103, 0)], 2, 2);
        assert!(walk.hits[0].is_none());
        assert_eq!(
            walk.reached_ns, 1,
            "a walk that stopped mid-instant claimed that instant"
        );
    }

    #[test]
    fn a_source_that_ends_proves_its_final_instant_complete() {
        let mut source = river(vec![trade(1, 99), quote(2, 100, 101), trade(2, 105)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 1, 0)], 9, 100);
        assert!(walk.hits[0].is_none());
        assert_eq!(walk.reached_ns, 2, "an exhausted source drained everything");
    }

    #[test]
    fn a_quote_costs_the_walk_a_record_of_its_budget() {
        // Not incidental: the sweep budget is denominated in records pulled,
        // and every parent carries a quote. A budget sized against trade
        // counts alone under-serves every walk by that factor.
        let mut source = river(vec![quote(1, 98, 99), trade(1, 99)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 100, 0)], 1, 10);
        assert_eq!(walk.drained, 2);
    }

    #[test]
    fn a_budget_spent_entirely_on_quotes_leaves_the_walk_short() {
        let mut source = river(vec![
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

    /// Every scan kind on both sides, against thresholds on and off the
    /// river's grid: the compiled integer predicate hits exactly where the
    /// decimal predicate does. A threshold rounded the wrong way loses or
    /// invents a hit at the neighbouring grid price, which is what the
    /// off-grid rows probe.
    #[test]
    fn compiled_predicates_hit_exactly_where_the_decimal_predicates_do() {
        let scale = RecordScale::new(Symbol::from("X"), 2, 0).unwrap();
        let prices: Vec<i64> = (9_990..=10_030).collect();
        for threshold in ["100.2", "100.20", "100.205", "100.195", "99.999", "100"] {
            let px: Decimal = threshold.parse().unwrap();
            for kind in [
                ScanKind::FillThrough,
                ScanKind::StopTrigger,
                ScanKind::TouchedTrigger,
            ] {
                for side in [Side::Buy, Side::Sell] {
                    let scan = TriggerScan {
                        side,
                        px,
                        kind,
                        from_ns: 0,
                    };
                    let predicate = Compiled::new(&scale, &scan);
                    for &m in &prices {
                        assert_eq!(
                            predicate.holds(m),
                            kind.hit(side, px, scale.price(m)),
                            "{kind:?} {side:?} at {threshold} against {}",
                            scale.price(m)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn vol_reading_ignores_quotes_and_still_ends_on_the_last_print() {
        // `last_px` is contractually the last print at or before the reading
        // instant, and the submit path decides marketability against exactly it.
        // A quote sharing the horizon's timestamp must not become the stopping
        // point, or that field silently holds the previous print.
        let mut records = Vec::new();
        for i in 1..=12_u64 {
            records.push(quote(i, 99, 100));
            records.push(trade(i, 100 + i as i64));
        }
        let mut source = river(records);
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
        let mut records = Vec::new();
        for i in 1..=12_u64 {
            records.push(quote(i, 99, 100));
            records.push(trade(i, 100 + i as i64));
        }
        records.push(trade(12, 500));
        let mut source = river(records);
        let reading = vol_reading(&mut source, 0, 12, 100).expect("a full window reads");
        assert_eq!(reading.last_px, Decimal::from(500));
    }

    #[test]
    fn vol_reading_refuses_a_window_it_stopped_inside() {
        // The budget ends immediately after the quote at the horizon, so the
        // print sharing that instant was never consumed. Reporting a reading
        // here hands back the previous print as `last_px` while claiming the
        // window was covered - the silent staleness this function refuses.
        let mut records = Vec::new();
        for i in 1..=12_u64 {
            records.push(quote(i, 99, 100));
            records.push(trade(i, 100 + i as i64));
        }
        let mut source = river(records);
        // 11 full pairs, then the horizon's quote alone.
        assert!(vol_reading(&mut source, 0, 12, 23).is_none());
    }

    #[test]
    fn a_walk_reports_every_scan_that_triggered() {
        let mut source = river(vec![
            quote(1, 98, 100),
            trade(1, 99),
            quote(2, 100, 102),
            trade(2, 101),
        ]);
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
        let mut source = river(vec![
            quote(1, 98, 100),
            trade(1, 99),
            quote(2, 100, 102),
            trade(2, 101),
        ]);
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
                px: Decimal::from(99),
                book: Some(HitBook::unresolved(
                    Decimal::from(98),
                    Decimal::from(100),
                    Decimal::ONE,
                    Decimal::ONE
                )),
            })
        );
        assert_eq!(
            walk.hits[1],
            Some(Hit {
                ts_ns: 2,
                px: Decimal::from(101),
                book: Some(HitBook::unresolved(
                    Decimal::from(100),
                    Decimal::from(102),
                    Decimal::ONE,
                    Decimal::ONE
                )),
            })
        );
        assert_eq!(walk.hits[2], None);
    }

    #[test]
    fn a_touch_scan_hits_at_the_price_and_a_through_scan_does_not() {
        let at = Decimal::from(100);
        let mut source = river(vec![quote(1, 99, 101), trade(1, 100)]);
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
        let mut source = river(vec![quote(1, 89, 91), trade(1, 90)]);
        assert!(scan_triggers(&mut source, &[scan(Side::Buy, 100, 0)], 1, 10).hits[0].is_some());
    }
    #[test]
    fn a_walk_stops_once_every_scan_has_triggered() {
        let mut source = river(vec![
            quote(1, 98, 100),
            trade(1, 99),
            quote(2, 97, 99),
            trade(2, 98),
        ]);
        assert_eq!(
            scan_triggers(&mut source, &[scan(Side::Buy, 100, 0)], 2, 10).drained,
            2
        );
    }
    #[test]
    fn walk_with_a_spent_budget_reports_only_what_it_proved() {
        // Consuming the print at instant 1 does not establish that instant 1
        // is finished - another event could share it - and the caller advances
        // an exclusive frontier to this number, so claiming 1 here would let it
        // step over anything else at that instant. Under-claiming costs a
        // re-walk; over-claiming loses a fill permanently.
        let mut source = river(vec![trade(1, 100), trade(2, 100)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 99, 0)], 2, 1);
        assert_eq!((walk.reached_ns, walk.drained), (0, 1));
    }
    #[test]
    fn walk_pulls_exactly_one_event_past_the_boundary_to_prove_it() {
        // The only evidence that instant 2 is fully drained is an event at a
        // later instant, so the walk must pull it. One extra record of budget
        // buys the frontier its exclusive semantics depend on.
        let mut source = river(vec![trade(1, 100), trade(2, 100), trade(3, 100)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 99, 0)], 2, 10);
        assert_eq!((walk.reached_ns, walk.drained), (2, 3));
    }
    #[test]
    fn walk_batches_every_scan_into_one_pass() {
        let records = vec![trade(1, 99), trade(2, 101)];
        let scans = [scan(Side::Buy, 100, 0), scan(Side::Sell, 100, 0)];
        let both = scan_triggers(&mut river(records.clone()), &scans, 2, 10);
        for (i, one) in scans.iter().enumerate() {
            assert_eq!(
                both.hits[i].is_some(),
                scan_triggers(&mut river(records.clone()), &[*one], 2, 10).hits[0].is_some()
            );
        }
    }
    #[test]
    fn walk_over_an_empty_scan_list_pulls_nothing() {
        let mut source = river(vec![trade(1, 100)]);
        let walk = scan_triggers(&mut source, &[], 9, 10);
        assert!(walk.hits.is_empty());
        assert_eq!((walk.reached_ns, walk.drained), (9, 0));
    }

    fn reading_records(cadence: u64, count: usize) -> Vec<Record> {
        (0..count)
            .map(|i| trade((i as u64 + 1) * cadence, 100 + i as i64))
            .collect()
    }
    #[test]
    fn a_reading_refuses_below_the_minimum_sample_count() {
        let mut source = river(reading_records(1, MIN_VOL_SAMPLES));
        assert!(vol_reading(&mut source, 0, MIN_VOL_SAMPLES as u64, 100).is_none());
    }
    #[test]
    fn a_window_excludes_prints_outside_its_bounds() {
        let mut records = vec![trade(10, 999)];
        records.extend((1..=10).map(|i| trade(10 + i, 100 + i as i64)));
        records.push(trade(21, 777));
        let mut source = river(records);
        let reading = vol_reading(&mut source, 10, 20, 20).unwrap();
        assert_eq!(reading.last_px, Decimal::from(110));
        assert_eq!(reading.samples, 9);
    }
    #[test]
    fn a_reading_refuses_when_the_walk_exhausts_its_budget() {
        let mut source = river(reading_records(1, 20));
        assert!(vol_reading(&mut source, 0, 20, 10).is_none());
    }
    #[test]
    fn a_reading_refuses_a_zero_span_window() {
        let mut source = river((0..10).map(|i| trade(1, 100 + i)).collect());
        assert!(vol_reading(&mut source, 0, 1, 20).is_none());
    }
    #[test]
    fn the_last_price_is_the_last_print_at_or_before_the_upper_bound() {
        let mut records = reading_records(1, 10);
        records.push(trade(11, 999));
        let mut source = river(records);
        assert_eq!(
            vol_reading(&mut source, 0, 10, 20).unwrap().last_px,
            Decimal::from(109)
        );
    }
    #[test]
    fn horizon_scaling_is_the_square_root_of_the_observed_arrival_rate() {
        let mut source = river(reading_records(1_000_000_000, 10));
        let reading = vol_reading(&mut source, 0, 10_000_000_000, 20).unwrap();
        let expected =
            reading.rms_return * ((9.0 * FILL_HORIZON_NS as f64) / 9_000_000_000.0).sqrt();
        assert!((reading.horizon_return - expected).abs() < 1e-15);
    }
    #[test]
    fn doubling_the_arrival_rate_at_a_fixed_per_print_move_scales_the_band_by_sqrt_two() {
        let a = vol_reading(
            &mut river(reading_records(2_000_000_000, 10)),
            0,
            20_000_000_000,
            20,
        )
        .unwrap()
        .horizon_return;
        let b = vol_reading(
            &mut river(reading_records(1_000_000_000, 10)),
            0,
            10_000_000_000,
            20,
        )
        .unwrap()
        .horizon_return;
        assert!((b / a - 2.0_f64.sqrt()).abs() < 1e-12);
    }
}
