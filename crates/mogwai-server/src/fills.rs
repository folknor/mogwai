// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Trades-only penetration counting for account-owned resting limit fills.

use mogwai_data::PenetrationScan;
pub(crate) use mogwai_data::Walk;
use mogwai_engine::PendingScan;
use rust_decimal::Decimal;

use crate::source::{self, InstrumentProfiles};

/// Ticks one sweep pass may drain per symbol before it reports where it got to
/// and stops. `BoundedSeek` caps only its `seek_to`; its `next_tick` delegates
/// uncapped and `GeneratedSource` never ends, so the drain needs its own budget
/// or a far-from-market order walks forever. 20,000 is two orders of magnitude
/// above the default 100 ms interval's expected handful of ticks at the fitted
/// BTCUSDT cadence, and still terminates a multi-hour gap in bounded work
/// across several passes.
pub(crate) const SWEEP_DRAIN_BUDGET: usize = 20_000;

/// Count penetrations for every scan on one symbol in one walk of the CLEAN
/// tape.
///
/// ONE walk per symbol, not per order: every resting limit on a symbol shares
/// the same tape and the same pass span, so a per-order walk would pay a
/// checkpoint restore and a process-wide mutex acquisition per order per
/// interval - fifty resting limits at 100 ms would be five hundred restores a
/// second contending with `/trades` and market-price stamping. The scans'
/// `from_ns` may differ (orders rest at different instants); the walk starts at
/// the EARLIEST and each scan counts only ticks after its own bound.
///
/// The clean tape, not a regime'd realization: an armed `MarketRegime` is
/// per-subscription (`TapeKey` carries it) while an order belongs to an
/// account, so there is no single regime an order could be gated under. A
/// scenario that arms a drought silences its own DATA feed and leaves its fills
/// on the venue's canonical tape - the same property market-order price
/// stamping already has.
///
/// Composed from the same `build_history_source` the `/trades` cursor pages
/// through, so the prints gating a fill are the prints the client can fetch and
/// check. `None` when the positioning seek could not reach the earliest bound;
/// the caller then leaves every frontier unadvanced rather than treating an
/// unreachable span as zero penetrations.
///
/// Synchronous and CPU-bound. Callers run it on `spawn_blocking`, as
/// `stamp_market_price` and the `/trades` handler already do for the same
/// synthesis.
///
/// The walk itself is `mogwai_data::count_penetrations`; this function is the
/// composition - it builds the source, converts the engine's `PendingScan` to
/// the tape-shaped `PenetrationScan` the walk takes, and applies the server's
/// drain budget. The conversion is one allocation per symbol per pass and is
/// deliberate: `mogwai-data` does not depend on `mogwai-engine` (the dependency
/// runs the other way, through this crate) and the engine's scan additionally
/// carries the order identity and revision the tape has no business seeing.
pub(crate) fn count_penetrations(
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
        None,
        profiles,
        data_origin,
    )?;
    let mapped: Vec<_> = scans
        .iter()
        .map(|scan| PenetrationScan {
            side: scan.side,
            price: scan.price,
            from_ns: scan.from_ns,
            remaining: scan.remaining,
        })
        .collect();
    Some(mogwai_data::count_penetrations(
        source.as_mut(),
        &mapped,
        to_ns,
        SWEEP_DRAIN_BUDGET,
    ))
}

/// The last trade printed at or before `ts` on the clean tape: the venue's
/// reading of whether a limit was already marketable when it was accepted.
///
/// Re-exported from `source` so the whole penetration seam is reachable through
/// this module; the walk itself belongs next to the other generator walks.
pub(crate) fn last_trade_at_or_before(
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
        source::build_history_source("BTCUSDT", Some(start), None, &profiles(), TEST_ORIGIN)
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

    fn scan(id: &str, side: Side, price: Decimal, from_ns: u64, remaining: u32) -> PendingScan {
        PendingScan {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            side,
            price,
            from_ns,
            remaining,
            revision: 0,
        }
    }

    #[test]
    fn counts_only_prints_strictly_through_the_limit() {
        let ticks = trades(TEST_ORIGIN, 2);
        let (tick_ts, tick_price) = ticks[0];
        let increment = Decimal::new(1, 2);
        let from = tick_ts.saturating_sub(1);
        let to = ticks[1].0;
        let probes = vec![
            scan("touch-buy", Side::Buy, tick_price, from, 1),
            scan("touch-sell", Side::Sell, tick_price, from, 1),
            scan("above-buy", Side::Buy, tick_price + increment, from, 1),
            scan("above-sell", Side::Sell, tick_price + increment, from, 1),
            scan("below-buy", Side::Buy, tick_price - increment, from, 1),
            scan("below-sell", Side::Sell, tick_price - increment, from, 1),
        ];
        let walk =
            count_penetrations("BTCUSDT", &probes, to, &profiles(), TEST_ORIGIN).expect("walk");
        assert_eq!(walk.counted, vec![0, 0, 1, 0, 0, 1]);
    }

    #[test]
    fn a_span_with_no_penetration_counts_zero() {
        let probes = [scan("far", Side::Buy, Decimal::ONE, TEST_ORIGIN, 1)];
        let walk = count_penetrations(
            "BTCUSDT",
            &probes,
            TEST_ORIGIN + 3_600_000_000_000,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("walk");
        assert_eq!(walk.counted, vec![0]);
    }

    #[test]
    fn counting_stops_at_the_threshold() {
        let ticks = trades(TEST_ORIGIN, 4);
        let probe = scan("one", Side::Buy, ticks[0].1 + Decimal::ONE, TEST_ORIGIN, 1);
        let walk = count_penetrations("BTCUSDT", &[probe], ticks[3].0, &profiles(), TEST_ORIGIN)
            .expect("walk");
        assert_eq!(walk.counted, vec![1]);
        assert_eq!(walk.reached_ns, ticks[0].0);
    }

    #[test]
    fn a_truncated_drain_reports_where_it_stopped() {
        let first = trades(TEST_ORIGIN, 1).remove(0);
        let probes = [scan("never", Side::Buy, Decimal::ONE, TEST_ORIGIN, 1)];
        // Deliberately much longer than a day: the fitted tape has fewer than
        // 20k prints in that window, so a day does not actually exercise the
        // drain cap.
        let to = TEST_ORIGIN + 3_650 * 24 * 3_600_000_000_000;
        let walk =
            count_penetrations("BTCUSDT", &probes, to, &profiles(), TEST_ORIGIN).expect("walk");
        assert!(walk.reached_ns < to);
        assert!(walk.reached_ns > first.0);
        assert_eq!(walk.counted, vec![0]);
    }

    #[test]
    fn one_walk_serves_every_probe_on_a_symbol() {
        let ticks = trades(TEST_ORIGIN, 6);
        let probes = vec![
            scan("a", Side::Buy, ticks[0].1 + Decimal::ONE, TEST_ORIGIN, 10),
            scan("b", Side::Sell, ticks[2].1 - Decimal::ONE, ticks[1].0, 10),
            scan("c", Side::Buy, Decimal::ONE, ticks[3].0, 10),
        ];
        let to = ticks[5].0;
        let batched =
            count_penetrations("BTCUSDT", &probes, to, &profiles(), TEST_ORIGIN).expect("batched");
        let singles: Vec<_> = probes
            .iter()
            .map(|probe| {
                count_penetrations(
                    "BTCUSDT",
                    std::slice::from_ref(probe),
                    to,
                    &profiles(),
                    TEST_ORIGIN,
                )
                .expect("single")
                .counted[0]
            })
            .collect();
        assert_eq!(batched.counted, singles);
    }

    #[test]
    fn the_counted_prints_are_the_prints_trades_serves() {
        let ticks = trades(TEST_ORIGIN, 8);
        let probe = scan(
            "served",
            Side::Buy,
            ticks[0].1 + Decimal::ONE,
            TEST_ORIGIN,
            100,
        );
        let walk = count_penetrations("BTCUSDT", &[probe], ticks[7].0, &profiles(), TEST_ORIGIN)
            .expect("walk");
        let expected = ticks
            .iter()
            .filter(|(ts, price)| *ts > TEST_ORIGIN && *price < ticks[0].1 + Decimal::ONE)
            .count() as u32;
        assert_eq!(walk.counted, vec![expected]);
    }

    #[test]
    fn last_trade_at_or_before_never_looks_ahead() {
        let ticks = trades(TEST_ORIGIN, 2);
        let before_second = ticks[1].0.saturating_sub(1);
        assert_eq!(
            last_trade_at_or_before("BTCUSDT", before_second, &profiles(), TEST_ORIGIN),
            Some(ticks[0].1)
        );
        assert_eq!(
            source::current_price("BTCUSDT", &profiles(), TEST_ORIGIN, before_second),
            Some(ticks[1].1)
        );
    }

    #[test]
    fn sweep_pass_walks_only_the_new_span() {
        // Pin both ends to real adjacent tape prints. This measures the delta
        // span rather than assuming an arbitrary wall interval contains one.
        let ticks = trades(TEST_ORIGIN, 4);
        let first = count_penetrations(
            "BTCUSDT",
            &[scan("old", Side::Buy, Decimal::ONE, ticks[0].0, 1)],
            ticks[1].0,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("first pass");
        let second = count_penetrations(
            "BTCUSDT",
            &[scan("old", Side::Buy, Decimal::ONE, first.reached_ns, 1)],
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
        let one = count_penetrations(
            "BTCUSDT",
            &[scan("one", Side::Buy, Decimal::ONE, TEST_ORIGIN, 1)],
            to,
            &profiles(),
            TEST_ORIGIN,
        )
        .expect("one probe");
        let many: Vec<_> = (0..50)
            .map(|i| {
                scan(
                    &format!("many-{i}"),
                    Side::Buy,
                    Decimal::ONE,
                    TEST_ORIGIN,
                    1,
                )
            })
            .collect();
        let fifty = count_penetrations("BTCUSDT", &many, to, &profiles(), TEST_ORIGIN)
            .expect("fifty probes");
        assert_eq!(fifty.drained, one.drained);
    }
}
