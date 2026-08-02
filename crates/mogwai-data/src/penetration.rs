// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! One bounded, shared tape walk for penetration-gated resting limits.

use mogwai_protocol::{Side, trades_through};
use rust_decimal::Decimal;

use crate::{TickEvent, TickSource};

/// One resting limit the walk counts prints for.
///
/// A tape-shaped mirror of `mogwai_engine::PendingScan`, carrying only the three
/// fields the predicate reads plus the count still owed. It is deliberately not
/// that type: `mogwai-data` does not depend on `mogwai-engine` (the dependency
/// runs the other way, through the server), and the engine's scan additionally
/// carries the order identity and revision the tape has no business seeing.
#[derive(Debug, Clone, Copy)]
pub struct PenetrationScan {
    pub side: Side,
    pub price: Decimal,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
    /// Penetrations still required. A zero-remaining scan is counted for and
    /// ignored, never treated as satisfied.
    pub remaining: u32,
}

/// What one shared tape walk found.
#[derive(Debug, Clone)]
pub struct Walk {
    /// Per scan, in the input's order: penetrations counted in
    /// `(scan.from_ns, reached_ns]`.
    pub counted: Vec<u32>,
    /// The instant the drain ACTUALLY reached - `to_ns` when the span was
    /// covered, otherwise the `ts_event` of the last tick examined before the
    /// budget was spent. The caller advances each frontier to exactly this and
    /// never past it, so a truncated pass loses nothing and the next pass
    /// resumes where this one stopped.
    pub reached_ns: u64,
    /// Ticks pulled from the source. Read by the server's cost gates: one
    /// symbol walk must not multiply this by the number of scans, nor re-cover
    /// an already-swept frontier.
    pub drained: usize,
}

/// Count strictly-through prints for every scan in one walk of one tape.
///
/// The source arrives already positioned by the caller, so this function is
/// tape-agnostic: the server hands it the same history source `/trades` pages
/// through, tests hand it a `MemorySource`, the benches hand it a bare
/// `GeneratedSource`. `budget` bounds the drain, because `GeneratedSource` never
/// ends and a far-from-market order would otherwise walk forever; the budget is
/// sweep policy and so belongs to the caller, not to this module.
///
/// ONE walk per tape, not per order: every scan on a symbol shares the tape and
/// the pass span. The scans' `from_ns` may differ; each counts only prints after
/// its own bound. Synchronous and CPU-bound.
///
/// Returns as soon as every scan has reached its `remaining`, or when the span
/// `(earliest from_ns, to_ns]` is covered, or when `budget` is spent - whichever
/// comes first. `reached_ns` is where the drain actually got to, so a truncated
/// pass loses no span.
///
/// The empty-scan branch is unreachable from the server wrapper, which returns
/// `None` before it can be hit; it is a total-function obligation of this
/// signature and not live sweeper behaviour.
#[must_use]
pub fn count_penetrations(
    source: &mut dyn TickSource,
    scans: &[PenetrationScan],
    to_ns: u64,
    budget: usize,
) -> Walk {
    let Some(earliest) = scans.iter().map(|scan| scan.from_ns).min() else {
        return Walk {
            counted: Vec::new(),
            reached_ns: to_ns,
            drained: 0,
        };
    };
    let mut counted = vec![0_u32; scans.len()];
    let mut reached_ns = earliest;
    // Counted OUTSIDE the loop, so every exit reports what the pass really
    // pulled. A loop-local counter left the early exits reporting the whole
    // budget, which is the number the server's cost gates read.
    let mut drained = 0;
    for _ in 0..budget {
        let Some(event) = source.next_tick() else {
            break;
        };
        drained += 1;
        if event.ts_event() > to_ns {
            // The span is fully covered: the frontier moves to the pass's own
            // target, not to a tick that lies beyond it.
            reached_ns = to_ns;
            break;
        }
        reached_ns = event.ts_event();
        if let TickEvent::Trade(trade) = event {
            for (index, scan) in scans.iter().enumerate() {
                if counted[index] < scan.remaining
                    && trade.ts_event > scan.from_ns
                    && trades_through(scan.side, scan.price, trade.price)
                {
                    counted[index] = counted[index].saturating_add(1);
                }
            }
            if scans
                .iter()
                .enumerate()
                .all(|(index, scan)| counted[index] >= scan.remaining)
            {
                return Walk {
                    counted,
                    reached_ns,
                    drained,
                };
            }
        }
        // An exact boundary has fully covered the requested inclusive span; do
        // not ask the generator for another tick merely to discover that it
        // lies beyond it.
        if reached_ns == to_ns {
            break;
        }
    }
    Walk {
        counted,
        reached_ns,
        drained,
    }
}

#[cfg(test)]
mod tests {
    use mogwai_protocol::{AggressorSide, TradeTick};

    use super::*;
    use crate::MemorySource;

    fn trade(ts_event: u64, price: i64) -> TickEvent {
        TickEvent::Trade(TradeTick {
            symbol: "BTCUSDT".into(),
            price: Decimal::from(price),
            size: Decimal::ONE,
            aggressor: AggressorSide::NoAggressor,
            ts_event,
        })
    }

    fn scan(side: Side, price: i64, from_ns: u64, remaining: u32) -> PenetrationScan {
        PenetrationScan {
            side,
            price: Decimal::from(price),
            from_ns,
            remaining,
        }
    }

    #[test]
    fn walk_counts_only_prints_strictly_through() {
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 99), trade(3, 101)]);
        let walk = count_penetrations(
            &mut source,
            &[scan(Side::Buy, 100, 0, 1), scan(Side::Sell, 100, 0, 1)],
            3,
            10,
        );
        assert_eq!(walk.counted, vec![1, 1]);
    }

    #[test]
    fn walk_with_a_spent_budget_reports_where_it_stopped() {
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 100)]);
        let walk = count_penetrations(&mut source, &[scan(Side::Buy, 99, 0, 1)], 2, 1);
        assert_eq!(walk.reached_ns, 1);
        assert_eq!(walk.drained, 1);
    }

    #[test]
    fn walk_stops_at_an_exact_boundary_without_pulling_past_it() {
        let mut source = MemorySource::new(vec![trade(1, 100), trade(2, 100), trade(3, 100)]);
        let walk = count_penetrations(&mut source, &[scan(Side::Buy, 99, 0, 1)], 2, 10);
        assert_eq!(walk.reached_ns, 2);
        assert_eq!(walk.drained, 2);
    }

    #[test]
    fn walk_batches_every_scan_into_one_pass() {
        let ticks = vec![trade(1, 99), trade(2, 101)];
        let scans = [scan(Side::Buy, 100, 0, 1), scan(Side::Sell, 100, 0, 1)];
        let mut batched = MemorySource::new(ticks.clone());
        let both = count_penetrations(&mut batched, &scans, 2, 10);
        for (index, single) in scans.iter().enumerate() {
            let mut source = MemorySource::new(ticks.clone());
            assert_eq!(
                both.counted[index],
                count_penetrations(&mut source, &[*single], 2, 10).counted[0]
            );
        }
    }

    #[test]
    fn walk_over_an_empty_scan_list_pulls_nothing() {
        let mut source = MemorySource::new(vec![trade(1, 100)]);
        let walk = count_penetrations(&mut source, &[], 9, 10);
        assert!(walk.counted.is_empty());
        assert_eq!(walk.reached_ns, 9);
        assert_eq!(walk.drained, 0);
        assert!(source.next_tick().is_some());
    }
}
