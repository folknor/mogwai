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
/// Returns below which a reading is REFUSED rather than reported. Refusing is
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
    /// Where the drain ACTUALLY got to, which its budget may leave short of
    /// `to_ns`. A caller advances its frontier to exactly this, never past it.
    pub reached_ns: u64,
    /// Ticks pulled, for the cost gates.
    pub drained: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct VolReading {
    /// Last print at or before `to_ns`. Only ever produced by a walk that
    /// actually REACHED `to_ns`.
    pub last_px: Decimal,
    /// Instant of that last print. Carried so a stop that triggers on arrival
    /// can name a REAL print off the canonical tape rather than inventing one.
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
        return Walk {
            hits: Vec::new(),
            reached_ns: to_ns,
            drained: 0,
        };
    };
    let mut hits = vec![None; scans.len()];
    let mut reached_ns = earliest;
    let mut drained = 0;
    for _ in 0..budget {
        let Some(event) = source.next_tick() else {
            break;
        };
        drained += 1;
        if event.ts_event() > to_ns {
            reached_ns = to_ns;
            break;
        }
        reached_ns = event.ts_event();
        if let TickEvent::Trade(trade) = event {
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
                return Walk {
                    hits,
                    reached_ns,
                    drained,
                };
            }
        }
        if reached_ns == to_ns {
            break;
        }
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
    let mut reached_ns = from_ns;
    let mut drained = 0;
    for _ in 0..budget {
        let Some(event) = source.next_tick() else {
            break;
        };
        drained += 1;
        if event.ts_event() > to_ns {
            reached_ns = to_ns;
            break;
        }
        reached_ns = event.ts_event();
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
        if reached_ns == to_ns {
            break;
        }
    }
    // A walk that stopped short holds the OLDEST part of the window - it starts
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
                kind: ScanKind::TriggerTouch,
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
    fn walk_with_a_spent_budget_reports_where_it_stopped() {
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 100)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 99, 0)], 2, 1);
        assert_eq!((walk.reached_ns, walk.drained), (1, 1));
    }
    #[test]
    fn walk_stops_at_an_exact_boundary_without_pulling_past_it() {
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 100), trade(3, 100)]);
        let walk = scan_triggers(&mut source, &[scan(Side::Buy, 99, 0)], 2, 10);
        assert_eq!((walk.reached_ns, walk.drained), (2, 2));
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
