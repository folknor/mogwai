// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Trades-only tape reading for the venue's fill band: the sweep walk that
//! decides resting limits, and the volatility reading that sizes the band a
//! trigger is drawn from.

pub(crate) use mogwai_data::Walk;
use mogwai_data::{TriggerScan, VOL_WINDOW_NS};
use mogwai_engine::{MarketReading, PendingScan};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;

use crate::source::{self, InstrumentProfiles};

/// Ticks one sweep pass may drain per symbol before it reports where it got to
/// and stops. `BoundedSeek` caps only its `seek_to`; its `next_tick` delegates
/// uncapped and `GeneratedSource` never ends, so the drain needs its own budget
/// or a far-from-market order walks forever. 20,000 is two orders of magnitude
/// above the default 100 ms interval's expected handful of ticks at the fitted
/// BTCUSDT cadence, and still terminates a multi-hour gap in bounded work
/// across several passes.
pub(crate) const SWEEP_DRAIN_BUDGET: usize = 20_000;

/// Decide every scan on one symbol in one walk of the CLEAN tape.
///
/// ONE walk per symbol, not per order: every resting limit on a symbol shares
/// the same tape and the same pass span, so a per-order walk would pay a
/// checkpoint restore and a process-wide mutex acquisition per order per
/// interval - fifty resting limits at 100 ms would be five hundred restores a
/// second contending with `/trades` and market-price stamping. The scans'
/// `from_ns` may differ (orders rest at different instants); the walk starts at
/// the EARLIEST and each scan judges only ticks after its own bound.
///
/// The clean tape, not a regime'd realization: an armed `MarketRegime` is
/// per-subscription (`TapeKey` carries it) while an order belongs to an
/// account, so there is no single regime an order could be gated under. A
/// scenario that arms a drought silences its own DATA feed and leaves its fills
/// on the venue's canonical tape - the same property the acceptance-time reading
/// already has.
///
/// Composed from the same `build_history_source` the `/trades` cursor pages
/// through, so the prints deciding a fill are the prints the client can fetch
/// and check. `None` when the positioning seek could not reach the earliest
/// bound; the caller then leaves every frontier unadvanced rather than treating
/// an unreachable span as a span nothing triggered in.
///
/// Synchronous and CPU-bound. Callers run it on `spawn_blocking`, as the
/// `/trades` handler already does for the same synthesis.
///
/// The walk itself is `mogwai_data::scan_triggers`; this function is the
/// composition - it builds the source, converts the engine's `PendingScan` to
/// the tape-shaped `TriggerScan` the walk takes, and applies the server's
/// drain budget. The conversion is one allocation per symbol per pass and is
/// deliberate: `mogwai-data` does not depend on `mogwai-engine` (the dependency
/// runs the other way, through this crate) and the engine's scan additionally
/// carries the order identity and revision the tape has no business seeing.
pub(crate) fn scan_triggers(
    symbol: &str,
    scans: &[PendingScan],
    to_ns: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Walk> {
    let earliest = scans.iter().map(|scan| scan.from_ns).min()?;
    let mut source = source::build_history_source(
        symbol,
        Some(earliest.saturating_add(1)),
        profiles,
        data_origin,
    )?;
    let mapped: Vec<_> = scans
        .iter()
        .map(|scan| TriggerScan {
            side: scan.side,
            trigger_px: scan.trigger_px,
            from_ns: scan.from_ns,
        })
        .collect();
    Some(mogwai_data::scan_triggers(
        source.as_mut(),
        &mapped,
        to_ns,
        SWEEP_DRAIN_BUDGET,
    ))
}

/// What the venue reads off its own tape at a command instant: the last print
/// at or before `ts`, and the band half width in ticks that print's trailing
/// realized volatility prices.
///
/// One walk produces both, which is why the separate last-print walk the
/// penetration gate used is gone from this path:
///
/// ```text
/// band_ticks = clamp(floor(mult * horizon_return * last_px / increment),
///                    0, max_ticks)
/// ```
///
/// `horizon_return` carries the time dimension (see `mogwai_data::vol_reading`),
/// so ONE `mult` stays meaningful across instruments and cadences. `None`
/// whenever the reading would be untrue rather than imprecise - a cold
/// estimator, a truncated walk, a price that will not convert - and the engine's
/// no-reading path then rests the order untriggerable rather than guessing.
/// Refusing is the conservative answer: a zero band is the most PERMISSIVE fill
/// regime the venue has.
///
/// Synchronous and CPU-bound; callers run it on `spawn_blocking`.
pub(crate) fn read_market(
    symbol: &str,
    ts: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
    mult: f64,
    max_ticks: u32,
) -> Option<MarketReading> {
    let from_ns = ts.saturating_sub(VOL_WINDOW_NS).max(data_origin);
    let mut source = source::build_history_source(
        symbol,
        Some(from_ns.saturating_add(1)),
        profiles,
        data_origin,
    )?;
    let reading = mogwai_data::vol_reading(source.as_mut(), from_ns, ts, SWEEP_DRAIN_BUDGET)?;
    let increment = profiles.get(symbol)?.def.price_increment.to_f64()?;
    let last_px = reading.last_px.to_f64()?;
    let raw = mult * reading.horizon_return * last_px / increment;
    let ticks = if !raw.is_finite() || raw < 0.0 {
        0
    } else {
        raw.floor().min(f64::from(max_ticks)) as u32
    };
    Some(MarketReading {
        last_px: reading.last_px,
        band_ticks: ticks,
    })
}

/// The last print at or before `ts`, with no volatility reading attached.
///
/// Kept as a narrow fallback for exactly one caller: a price-less MARKET submit
/// still has to be stamped with a price for the protocol's own validator, and
/// `read_market` legitimately refuses (a cold estimator, a truncated walk) at
/// instants where a last print does exist. It is NOT a second answer to "what
/// is the market" that fill decisions may consult - every one of those goes
/// through `read_market`.
pub(crate) fn read_last(
    symbol: &str,
    ts: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Decimal> {
    source::last_trade_at_or_before(symbol, ts, profiles, data_origin)
}

#[cfg(test)]
mod tests {
    use mogwai_data::{TickEvent, TickSource};
    use mogwai_engine::PendingScan;
    use mogwai_protocol::Side;

    use super::*;

    const TEST_ORIGIN: u64 = 1_700_438_400_000_000_000;

    fn profiles() -> InstrumentProfiles {
        InstrumentProfiles::defaults()
    }

    fn tape(start: u64) -> Box<dyn TickSource> {
        source::build_history_source("BTCUSDT", Some(start), &profiles(), TEST_ORIGIN)
            .expect("configured deterministic BTC tape")
    }

    fn trades(start: u64, count: usize) -> Vec<(u64, Decimal)> {
        let mut tape = tape(start);
        (0..count)
            .map(
                |_| match tape.next_tick().expect("infinite generated tape") {
                    TickEvent::Trade(tick) => (tick.ts_event, tick.price),
                    TickEvent::Quote(_) => panic!("generated tape is trades-only"),
                },
            )
            .collect()
    }

    fn scan(id: &str, side: Side, trigger_px: Decimal, from_ns: u64) -> PendingScan {
        PendingScan {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            side,
            trigger_px,
            from_ns,
            revision: 0,
        }
    }

    #[test]
    fn triggers_only_on_prints_strictly_through_the_trigger() {
        let ticks = trades(TEST_ORIGIN, 2);
        let (tick_ts, tick_price) = ticks[0];
        let increment = Decimal::new(1, 2);
        let from = tick_ts.saturating_sub(1);
        let to = ticks[1].0;
        let probes = vec![
            scan("touch-buy", Side::Buy, tick_price, from),
            scan("touch-sell", Side::Sell, tick_price, from),
            scan("above-buy", Side::Buy, tick_price + increment, from),
            scan("above-sell", Side::Sell, tick_price + increment, from),
            scan("below-buy", Side::Buy, tick_price - increment, from),
            scan("below-sell", Side::Sell, tick_price - increment, from),
        ];
        let walk = scan_triggers("BTCUSDT", &probes, to, &profiles(), TEST_ORIGIN).expect("walk");
        assert_eq!(walk.triggered, vec![false, false, true, false, false, true]);
    }

    #[test]
    fn a_span_the_tape_never_crossed_triggers_nothing() {
        let probes = [scan("far", Side::Buy, Decimal::ONE, TEST_ORIGIN)];
        let walk = scan_triggers(
            "BTCUSDT",
            &probes,
            TEST_ORIGIN + 3_600_000_000_000,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("walk");
        assert_eq!(walk.triggered, vec![false]);
    }

    #[test]
    fn a_walk_stops_once_every_scan_has_triggered() {
        let ticks = trades(TEST_ORIGIN, 4);
        let probe = scan("one", Side::Buy, ticks[0].1 + Decimal::ONE, TEST_ORIGIN);
        let walk =
            scan_triggers("BTCUSDT", &[probe], ticks[3].0, &profiles(), TEST_ORIGIN).expect("walk");
        assert_eq!(walk.triggered, vec![true]);
        assert_eq!(walk.reached_ns, ticks[0].0);
    }

    #[test]
    fn a_truncated_drain_reports_where_it_stopped() {
        let first = trades(TEST_ORIGIN, 1).remove(0);
        let probes = [scan("never", Side::Buy, Decimal::ONE, TEST_ORIGIN)];
        // Deliberately much longer than a day: the fitted tape has fewer than
        // 20k prints in that window, so a day does not actually exercise the
        // drain cap.
        let to = TEST_ORIGIN + 3_650 * 24 * 3_600_000_000_000;
        let walk = scan_triggers("BTCUSDT", &probes, to, &profiles(), TEST_ORIGIN).expect("walk");
        assert!(walk.reached_ns < to);
        assert!(walk.reached_ns > first.0);
        assert_eq!(walk.triggered, vec![false]);
    }

    #[test]
    fn one_walk_serves_every_probe_on_a_symbol() {
        let ticks = trades(TEST_ORIGIN, 6);
        let probes = vec![
            scan("a", Side::Buy, ticks[0].1 + Decimal::ONE, TEST_ORIGIN),
            scan("b", Side::Sell, ticks[2].1 - Decimal::ONE, ticks[1].0),
            scan("c", Side::Buy, Decimal::ONE, ticks[3].0),
        ];
        let to = ticks[5].0;
        let batched =
            scan_triggers("BTCUSDT", &probes, to, &profiles(), TEST_ORIGIN).expect("batched");
        let singles: Vec<_> = probes
            .iter()
            .map(|probe| {
                scan_triggers(
                    "BTCUSDT",
                    std::slice::from_ref(probe),
                    to,
                    &profiles(),
                    TEST_ORIGIN,
                )
                .expect("single")
                .triggered[0]
            })
            .collect();
        assert_eq!(batched.triggered, singles);
    }

    #[test]
    fn the_deciding_prints_are_the_prints_trades_serves() {
        let ticks = trades(TEST_ORIGIN, 8);
        let probe = scan("served", Side::Buy, ticks[0].1 + Decimal::ONE, TEST_ORIGIN);
        let walk =
            scan_triggers("BTCUSDT", &[probe], ticks[7].0, &profiles(), TEST_ORIGIN).expect("walk");
        let expected = ticks
            .iter()
            .filter(|(ts, price)| *ts > TEST_ORIGIN && *price < ticks[0].1 + Decimal::ONE)
            .count() as u32;
        assert_eq!(walk.triggered, vec![expected > 0]);
    }

    #[test]
    fn last_trade_at_or_before_never_looks_ahead() {
        let ticks = trades(TEST_ORIGIN, 2);
        let before_second = ticks[1].0.saturating_sub(1);
        assert_eq!(
            read_last("BTCUSDT", before_second, &profiles(), TEST_ORIGIN),
            Some(ticks[0].1)
        );
    }

    #[test]
    fn sweep_pass_walks_only_the_new_span() {
        // Pin both ends to real adjacent tape prints. This measures the delta
        // span rather than assuming an arbitrary wall interval contains one.
        let ticks = trades(TEST_ORIGIN, 4);
        let first = scan_triggers(
            "BTCUSDT",
            &[scan("old", Side::Buy, Decimal::ONE, ticks[0].0)],
            ticks[1].0,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("first pass");
        let second = scan_triggers(
            "BTCUSDT",
            &[scan("old", Side::Buy, Decimal::ONE, first.reached_ns)],
            ticks[2].0,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("second pass");
        assert!(
            second.drained < 200,
            "second pass drained {} ticks",
            second.drained
        );
    }

    #[test]
    fn a_pass_costs_one_walk_per_symbol_not_per_order() {
        let to = TEST_ORIGIN + 100_000_000;
        let one = scan_triggers(
            "BTCUSDT",
            &[scan("one", Side::Buy, Decimal::ONE, TEST_ORIGIN)],
            to,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("one probe");
        let many: Vec<_> = (0..50)
            .map(|i| scan(&format!("many-{i}"), Side::Buy, Decimal::ONE, TEST_ORIGIN))
            .collect();
        let fifty =
            scan_triggers("BTCUSDT", &many, to, &profiles(), TEST_ORIGIN).expect("fifty probes");
        assert_eq!(fifty.drained, one.drained);
    }

    /// The calibration table `fill_band_vol_mult` is read off. Prints; asserts
    /// only that the instrument itself ran.
    ///
    /// One sim day of readings, one per sim minute, on the committed BTCUSDT
    /// profile and its `data_origin`, starting one sim hour in so the window is
    /// clear of both `VOL_WINDOW_NS` and the generator's own warmup. Quantiles
    /// are NEAREST-RANK on the sorted vector of non-`None` readings - element
    /// `ceil(q * m) - 1`, zero-indexed, no interpolation - stated because
    /// "median" and "p90" have three common definitions that disagree at these
    /// sample counts.
    ///
    /// The implied band is computed through the real `read_market` conversion
    /// per multiplier rather than by scaling one column, so the floor and the
    /// clamp are the shipped ones. A refusal count above zero is a finding in
    /// its own right and is read before the table is.
    ///
    /// PROCEED threshold: the chosen multiplier is the smallest whose MEDIAN
    /// implied band is at least 3 ticks and at most 100. Below 3 the band is
    /// indistinguishable from the degenerate `u = 0` case; above 100 the p90
    /// band approaches the move the tape makes over a whole `VOL_WINDOW_NS`, at
    /// which point a fill is decided by the draw rather than by the tape.
    #[test]
    #[ignore = "calibration instrument"]
    fn vol_probe() {
        const WARMUP_NS: u64 = 3_600_000_000_000;
        const PROBE_STRIDE_NS: u64 = 60_000_000_000;
        const PROBE_SAMPLES: usize = 1_440;
        const MULTIPLIERS: [f64; 7] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];
        let profiles = profiles();
        let increment = profiles
            .get("BTCUSDT")
            .expect("BTCUSDT profile")
            .def
            .price_increment;
        let instants: Vec<u64> = (0..PROBE_SAMPLES)
            .map(|k| TEST_ORIGIN + WARMUP_NS + k as u64 * PROBE_STRIDE_NS)
            .collect();

        // The estimator's own inputs, unscaled, plus the refusal census. Split
        // by cause: a budget exhaustion is a different finding from a cold
        // window, and only the first says to raise `SWEEP_DRAIN_BUDGET`.
        let mut readings = Vec::new();
        let (mut refused_cold, mut refused_drained) = (0, 0);
        for &ts in &instants {
            let from_ns = ts
                .saturating_sub(mogwai_data::VOL_WINDOW_NS)
                .max(TEST_ORIGIN);
            let mut source =
                source::build_history_source("BTCUSDT", Some(from_ns + 1), &profiles, TEST_ORIGIN)
                    .expect("probe tape");
            match mogwai_data::vol_reading(source.as_mut(), from_ns, ts, SWEEP_DRAIN_BUDGET) {
                Some(reading) => readings.push(reading),
                None => {
                    // Re-walk with an unbounded-in-practice budget: if that
                    // succeeds, the budget was what refused the reading.
                    let mut retry = source::build_history_source(
                        "BTCUSDT",
                        Some(from_ns + 1),
                        &profiles,
                        TEST_ORIGIN,
                    )
                    .expect("probe tape");
                    if mogwai_data::vol_reading(retry.as_mut(), from_ns, ts, usize::MAX).is_some() {
                        refused_drained += 1;
                    } else {
                        refused_cold += 1;
                    }
                }
            }
        }
        println!(
            "readings={} refused_cold={refused_cold} refused_budget={refused_drained} of {PROBE_SAMPLES}",
            readings.len()
        );
        assert!(
            !readings.is_empty(),
            "every calibration reading was refused"
        );
        assert_eq!(
            refused_drained, 0,
            "raise SWEEP_DRAIN_BUDGET before reading this table"
        );

        let quantile = |mut values: Vec<f64>, q: f64| -> f64 {
            values.sort_by(f64::total_cmp);
            values[((q * values.len() as f64).ceil() as usize).saturating_sub(1)]
        };
        let column = |pick: fn(&mogwai_data::VolReading) -> f64, q: f64| -> f64 {
            quantile(readings.iter().map(pick).collect(), q)
        };
        println!(
            "rms_return median={:.3e} p90={:.3e}",
            column(|r| r.rms_return, 0.5),
            column(|r| r.rms_return, 0.9)
        );
        println!(
            "samples median={:.0} span_ns median={:.0}",
            column(|r| r.samples as f64, 0.5),
            column(|r| r.span_ns as f64, 0.5)
        );
        println!(
            "horizon_return median={:.3e} p90={:.3e}",
            column(|r| r.horizon_return, 0.5),
            column(|r| r.horizon_return, 0.9)
        );

        for mult in MULTIPLIERS {
            let ticks: Vec<f64> = instants
                .iter()
                .filter_map(|&ts| read_market("BTCUSDT", ts, &profiles, TEST_ORIGIN, mult, 10_000))
                .map(|reading| f64::from(reading.band_ticks))
                .collect();
            let bps = |ticks: f64| {
                let last = column(|r| r.last_px.to_f64().unwrap_or_default(), 0.5);
                ticks * increment.to_f64().unwrap_or_default() / last * 10_000.0
            };
            let (median, p90) = (quantile(ticks.clone(), 0.5), quantile(ticks, 0.9));
            println!(
                "mult={mult:<4} median_ticks={median:<7.0} p90_ticks={p90:<7.0} median_bps={:<7.3} p90_bps={:.3}",
                bps(median),
                bps(p90)
            );
        }
    }

    /// The submit path's cost gate. Every limit submit and every price amend now
    /// pays a checkpoint restore plus up to `SWEEP_DRAIN_BUDGET` ticks, which
    /// the engine-side fill bench is structurally blind to because it takes a
    /// `MarketReading` as an argument.
    ///
    /// KEEP/REVERT: median at or below 5 ms, p99 at or below 25 ms. Above that
    /// the reading is re-scoped before the model ships - first lever a shorter
    /// `VOL_WINDOW_NS`, second caching one reading per symbol per sweep interval
    /// and serving submits from it, which is sound because the band is a coarse
    /// scale rather than a per-microsecond quantity.
    #[test]
    #[ignore = "latency instrument"]
    fn read_market_latency_stays_within_submit_budget() {
        let profiles = profiles();
        let ts = TEST_ORIGIN + 3_600_000_000_000;
        let _ = read_market("BTCUSDT", ts, &profiles, TEST_ORIGIN, 0.5, 200);
        let mut samples = Vec::with_capacity(100);
        for _ in 0..100 {
            let started = std::time::Instant::now();
            let _ = read_market("BTCUSDT", ts, &profiles, TEST_ORIGIN, 0.5, 200);
            samples.push(started.elapsed());
        }
        samples.sort_unstable();
        let median = samples[49];
        let p99 = samples[98];
        println!("read_market median={median:?} p99={p99:?}");
        assert!(median <= std::time::Duration::from_millis(5));
        assert!(p99 <= std::time::Duration::from_millis(25));
    }
}
