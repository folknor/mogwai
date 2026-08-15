// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic, lazily materialized rivers owned by one serving process.

use mogwai_data::TickEvent;
use mogwai_data::{
    CheckpointIndex, Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, SessionProfile,
    SizeGrid, TickSource,
};
use mogwai_protocol::{InstrumentDef, MarketRegime, RunSeeds, Symbol};
use rust_decimal::Decimal;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

pub fn fingerprint() -> &'static Fingerprint {
    static FP: OnceLock<Fingerprint> = OnceLock::new();
    FP.get_or_init(Fingerprint::from_repo_json)
}

/// Fixed epoch of every generated tape. The run proper begins one configured
/// warmup span after this instant.
pub const TAPE_ORIGIN_NS: u64 = 0;

#[derive(Debug, Clone)]
pub struct InstrumentProfile {
    pub def: InstrumentDef,
    pub scalars: GeneratorScalars,
    pub session: SessionProfile,
    /// Collateral policy, copied into the engine at `Run::new`. Absent for
    /// spot, mandatory for a future.
    pub margin: Option<crate::config::ConfiguredMargin>,
    /// Maker/taker schedule, copied into the engine at `Run::new`. Absent
    /// means the fee-free venue.
    pub fees: Option<crate::config::ConfiguredFees>,
    pub calendar: Option<mogwai_data::SessionCalendar>,
}
impl InstrumentProfile {
    pub fn new(
        def: InstrumentDef,
        mut scalars: GeneratorScalars,
        session: SessionProfile,
        margin: Option<crate::config::ConfiguredMargin>,
        fees: Option<crate::config::ConfiguredFees>,
        calendar: Option<mogwai_data::SessionCalendar>,
    ) -> Self {
        scalars.symbol = def.symbol.to_string();
        Self {
            def,
            scalars,
            session,
            margin,
            fees,
            calendar,
        }
    }
}
#[derive(Debug, Clone)]
pub struct InstrumentProfiles {
    by_symbol: HashMap<Symbol, InstrumentProfile>,
    /// The symbol of the FIRST profile handed to `from_profiles`, which
    /// `build_instrument_profiles` sweeps from the config's boot shape. Kept
    /// because the boot shape is not recoverable from the map: the config's
    /// top-level `symbol` may be absent, and an absent one does NOT mean
    /// `DEFAULT_PRESET` - `[instrument] preset = "MNQ"` with no top-level symbol
    /// resolves a shape whose symbol is MNQ, and looking up BTCUSDT would then
    /// refuse a config that boots perfectly well.
    boot: Option<Symbol>,
}
impl InstrumentProfiles {
    pub fn from_profiles(profiles: Vec<InstrumentProfile>) -> Self {
        Self {
            boot: profiles
                .first()
                .map(|profile| std::sync::Arc::clone(&profile.def.symbol)),
            by_symbol: profiles
                .into_iter()
                .map(|profile| (std::sync::Arc::clone(&profile.def.symbol), profile))
                .collect(),
        }
    }
    pub fn get(&self, symbol: &str) -> Option<&InstrumentProfile> {
        self.by_symbol.get(symbol)
    }
    /// Every symbol this run can synthesize, sorted, for refusal messages that
    /// must name what IS servable. Sorted because a refusal body is read by a
    /// human and diffed by a test; `HashMap` order is neither.
    pub fn served_symbols(&self) -> Vec<&str> {
        let mut symbols: Vec<_> = self.by_symbol.keys().map(|symbol| &**symbol).collect();
        symbols.sort_unstable();
        symbols
    }
    pub fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self
            .by_symbol
            .values()
            .map(|profile| profile.def.clone())
            .collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }
    /// The def of the shape this run boots its paced tape on, resolved BY NAME.
    ///
    /// Not "the sole entry": once profiles are plural there need not be one, and
    /// a sole-entry rule would silently pick the wrong shape for a config with a
    /// top-level `symbol` plus a `[symbols.*]` table. An absent `symbol` falls
    /// back to the shape the sweep resolved FIRST, which is exactly what the
    /// config's own defaulting produced for a `None` symbol.
    pub fn boot_symbol_def(&self, symbol: Option<&str>) -> anyhow::Result<InstrumentDef> {
        let named = symbol
            .or(self.boot.as_deref())
            .unwrap_or(crate::config::DEFAULT_PRESET);
        self.get(named)
            .map(|profile| profile.def.clone())
            .ok_or_else(|| anyhow::anyhow!("boot symbol {named} has no configured shape"))
    }
}
fn generator(label: &str, profile: &InstrumentProfile, identity: TapeIdentity) -> GeneratedSource {
    GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        identity.seeds.tape_for(label),
        TAPE_ORIGIN_NS,
        fingerprint(),
        &profile.session,
        identity.regime,
        SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    )
}

/// Snapshot spacing of the run's checkpoint chain, in ticks. The generator is
/// path-dependent, so reaching an instant means walking to it; a snapshot every
/// `CHECKPOINT_K` ticks turns that O(distance) walk into a resume plus a
/// residual replay of fewer than `CHECKPOINT_K` ticks.
///
/// This chain is the mechanism that MATERIALIZES the warmup, which is why it
/// survives the lifecycle rewrite. What that rewrite removed is the per-request
/// seek BUDGET that used to cap it: there is no `MAX_HISTORY_SEEK_TICKS` any
/// more and no request is served short because it ran out of walk. A request
/// below the floor is refused by name instead, which is the whole point.
/// The stride is a latency/memory tradeoff, not a reach budget. Reach and
/// refusal ceilings are sized separately below. At 8,192 ticks the initial
/// grid bounds an ordinary restore to a few milliseconds while the hard
/// `MAX_CHECKPOINTS` cap still bounds retained generator clones; exceptionally
/// long runs coarsen the grid as documented by `CheckpointIndex`.
pub(crate) const CHECKPOINT_K: usize = 8_192;

/// Runaway backstop on a SINGLE extension while one river lock is held.
/// This retains the original one-billion-tick safety purpose: a nonsensical
/// far-future request cannot turn one lock acquisition into the
/// 667-billion-tick warmup reach bound below.
const MAX_EXTEND_TICKS: usize = 1 << 30;

/// Per-acquisition reach admitted across lock-releasing extension chunks.
/// Protocol 10's headroom rule set 667,299,000,000 against its
/// measured 2.055x warmup-reach expansion; protocol 11's session refit
/// measures an 11/10 ratio of 1.00036, and the standing rule - prior times
/// ratio times two, next-million rounding, then the larger of that and the
/// 81,123,436,742-frame required reach - lands here. Keeping it separate
/// from `MAX_EXTEND_TICKS` prevents a reach requirement from silently
/// disabling the per-lock runaway backstop. There is no absolute per-river
/// cap: later calls pay only their new monotonic delta, and an absolute fence
/// would wedge history for a sufficiently long run.
const MAX_WARMUP_MATERIALIZATION_TICKS: usize = 1_335_079_000_000;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RiverKey(Symbol);
impl RiverKey {
    pub(crate) fn for_symbol(symbol: &Symbol) -> Self {
        Self(Arc::clone(symbol))
    }
    #[expect(dead_code, reason = "piece 9 widens and inspects the river key")]
    pub(crate) fn symbol(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TapeIdentity {
    pub(crate) seeds: RunSeeds,
    pub(crate) regime: Option<MarketRegime>,
}

pub(crate) struct River {
    checkpoints: Mutex<CheckpointIndex>,
}

pub(crate) struct Rivers {
    identity: TapeIdentity,
    profiles: Arc<InstrumentProfiles>,
    rivers: Mutex<HashMap<RiverKey, Arc<River>>>,
}

fn locked(index: &Mutex<CheckpointIndex>) -> std::sync::MutexGuard<'_, CheckpointIndex> {
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(crate) enum History {
    Unconfigured,
    Source(Box<dyn TickSource>),
}

/// Test-only convenience: the `Option` shape the in-crate tests assert on,
/// collapsing "no such symbol" and "the reach failed" the way no production
/// caller may.
#[cfg(test)]
pub(crate) fn build_history_source(
    symbol: &str,
    start: Option<u64>,
    rivers: &Rivers,
) -> Option<Box<dyn TickSource>> {
    match rivers.history_source(symbol, start).ok()? {
        History::Source(source) => Some(source),
        History::Unconfigured => None,
    }
}

impl Rivers {
    pub(crate) fn new(identity: TapeIdentity, profiles: Arc<InstrumentProfiles>) -> Arc<Self> {
        Arc::new(Self {
            identity,
            profiles,
            rivers: Mutex::new(HashMap::new()),
        })
    }
    pub(crate) fn profiles(&self) -> &InstrumentProfiles {
        &self.profiles
    }
    #[expect(
        dead_code,
        reason = "registry identity is part of the owned handle contract"
    )]
    pub(crate) fn identity(&self) -> TapeIdentity {
        self.identity
    }
    fn river(&self, symbol: &str) -> Option<Arc<River>> {
        let profile = self.profiles.get(symbol)?;
        let key = RiverKey::for_symbol(&profile.def.symbol);
        // Derived inside the closure, not ahead of the lock: `river` is called
        // once per live tick and the seed is read once per river lifetime.
        let mut materialized: Option<u64> = None;
        let mut rivers = self
            .rivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Creation under the registry mutex makes concurrent first readers
        // share exactly one chain.
        let river = Arc::clone(rivers.entry(key).or_insert_with(|| {
            materialized = Some(self.identity.seeds.tape_for(symbol));
            Arc::new(River {
                checkpoints: Mutex::new(CheckpointIndex::new(
                    generator(symbol, profile, self.identity),
                    CHECKPOINT_K,
                    MAX_EXTEND_TICKS,
                )),
            })
        }));
        // Lock ordering is registry then release, river. Never hold both.
        drop(rivers);
        // After the drop: a `tracing` call runs arbitrary subscriber work, and
        // this closure body would run it inside the registry critical section
        // that every first reader of every symbol contends on.
        if let Some(tape_seed) = materialized {
            tracing::info!(symbol, tape_seed, "river materialized");
        }
        Some(river)
    }
    #[cfg(test)]
    pub(crate) fn river_handle_for_test(&self, symbol: &str) -> Option<Arc<River>> {
        self.river(symbol)
    }

    pub(crate) fn next_live_tick(
        &self,
        symbol: &str,
    ) -> Option<(Option<TickEvent>, Option<mogwai_data::TickFault>)> {
        let river = self.river(symbol)?;
        let mut index = locked(&river.checkpoints);
        let tick = index.next_tick();
        let fault = tick.is_none().then(|| index.fault()).flatten();
        Some((tick, fault))
    }

    pub(crate) fn activate_live(&self, symbol: &str) -> bool {
        let Some(river) = self.river(symbol) else {
            return false;
        };
        locked(&river.checkpoints).activate_live();
        true
    }

    pub(crate) fn arm_flow_surge(
        &self,
        symbol: &str,
        start_ns: u64,
        duration_ms: u64,
        rate_mult: f64,
        children_mult: f64,
    ) -> bool {
        let Some(river) = self.river(symbol) else {
            return false;
        };
        locked(&river.checkpoints).arm_flow_surge(start_ns, duration_ms, rate_mult, children_mult);
        true
    }

    pub(crate) fn clear_flow_surge(&self, symbol: &str) -> bool {
        let Some(river) = self.river(symbol) else {
            return false;
        };
        locked(&river.checkpoints).clear_flow_surge();
        true
    }

    /// A source positioned at `start`, RESUMED from the run's checkpoint chain
    /// rather than re-walked from the origin. `None` means the tape origin, which
    /// is checkpoint zero and therefore free.
    ///
    /// The `MergeSource` around a ONE-element vector is not a leftover from the
    /// multi-symbol era and must not be "simplified" away. `TickSource::seek_to`
    /// CONSUMES the tick it returns, so something has to hold that first in-window
    /// tick until the stream is read; `MergeSource`'s per-source head buffer is what
    /// does it. Unwrapping this to the bare source silently drops the tick at
    /// exactly `start` - a one-tick-late history window that no type would catch.
    /// Replacing it means writing an equivalent one-tick pushback, which is the same
    /// object with a narrower name.
    ///
    /// That `start` is INCLUSIVE is relied on rather than incidental: the fill
    /// sweeper passes `from_ns + 1` precisely to get a window that excludes the
    /// instant it already processed, which only works if the tick at the requested
    /// instant is emitted.
    pub(crate) fn history_source(
        &self,
        symbol: &str,
        start: Option<u64>,
    ) -> anyhow::Result<History> {
        let target = start.unwrap_or(TAPE_ORIGIN_NS);
        let Some(river) = self.river(symbol) else {
            return Ok(History::Unconfigured);
        };
        reach_river(&river, target)?;
        let Some(positioned) = locked(&river.checkpoints).try_source_at_or_before(target) else {
            anyhow::bail!("river {symbol} cannot reach {target}");
        };
        Ok(History::Source(Box::new(MergeSource::starting_at(
            vec![Box::new(positioned)],
            start,
        ))))
    }

    /// Generate and HOLD every instant up to `target_ns` on this river, so that
    /// each is reachable by a resume plus a bounded residual replay rather than
    /// refused or silently served short. Returns the number of snapshots
    /// retained, which is what the boot log reports for the warmup call.
    ///
    /// FIRST-TOUCH COST IS REAL. `serve.rs` pays this once at boot for the boot
    /// river; every other river is cold until something reads it, and that first
    /// distant history request pays the whole walk from `TAPE_ORIGIN_NS`
    /// synchronously, holding that river's mutex, and allocates up to
    /// `MAX_CHECKPOINTS` generator clones for it. Later requests on the same
    /// river pay only their new monotonic delta. Nothing pre-warms a non-boot
    /// river: doing so would multiply time-to-readiness by the number of
    /// configured shapes for a capability most runs never touch.
    pub(crate) fn ensure_reach(&self, symbol: &str, target_ns: u64) -> anyhow::Result<usize> {
        let river = self
            .river(symbol)
            .ok_or_else(|| anyhow::anyhow!("symbol {symbol} has no configured shape"))?;
        reach_river(&river, target_ns)
    }
}

fn reach_river(river: &River, target_ns: u64) -> anyhow::Result<usize> {
    reach_river_within(river, target_ns, MAX_WARMUP_MATERIALIZATION_TICKS)
}

/// Taking `&River` rather than a symbol is not a style choice: it makes it
/// impossible to re-enter the registry lock from a path that may already hold a
/// river lock, which is rule 2 of the registry's lock ordering.
///
/// `ceiling` is a parameter only so the tests can drive the refusal at a
/// reachable size; every production caller passes
/// `MAX_WARMUP_MATERIALIZATION_TICKS`.
fn reach_river_within(river: &River, target_ns: u64, ceiling: usize) -> anyhow::Result<usize> {
    let mut walked_total = 0usize;
    loop {
        let (walked, frontier_ns, checkpoints) = {
            let mut guard = locked(&river.checkpoints);
            let extended = guard.extend_toward_unless_live(target_ns);
            (extended, guard.frontier_ns(), guard.checkpoint_count())
        };
        let Some(walked) = walked else {
            // The paced worker owns this river's lead. Not an error: report what
            // is already reachable and let positioning refuse if it is short.
            return Ok(checkpoints);
        };
        walked_total = walked_total.saturating_add(walked);
        anyhow::ensure!(
            walked_total <= ceiling,
            "river generation exceeded its measured {ceiling}-tick per-call reach ceiling"
        );
        if frontier_ns >= target_ns {
            return Ok(checkpoints);
        }
        anyhow::ensure!(walked > 0, "generator stopped before {target_ns}");
    }
}

/// The last trade printed at or before `ts`. Positioned from the chain at a
/// checkpoint no later than `ts` and walked forward only the residual, so this
/// costs the same whether `ts` is one second or one day into the run.
///
/// The WALK-BACK is not an optimization to remove. A positioned source resumes
/// AFTER the tick its checkpoint last consumed, so the residual covers only
/// `(checkpoint clock, ts]`. When the last print at or before `ts` fell on or
/// before that clock - `ts` sitting between a parent's final trade and the next
/// parent is the easy case, and a `FlowSurge` control boundary snapshots at an
/// arbitrary instant - the residual contains no trade at all and the honest
/// answer is not `None`. Resuming progressively earlier snapshots recovers it,
/// doubling the step so the retry count is logarithmic in the chain rather than
/// one walk per checkpoint. Callers treat `None` as "the tape could not be
/// read", and the sweeper's settlement frontier refuses to retire a span on it,
/// so answering `None` where a print exists would stall the frontier forever
/// rather than merely losing one reading.
///
/// The walk-back is FENCED at a `FlowSurge` control boundary (resuming earlier
/// and replaying across the arm would answer from a different tape), so the
/// fence itself needs its own recovery: when the earliest permitted snapshot's
/// residual holds no print, the print the reader wants is one that snapshot
/// CONSUMED, and the snapshot still knows it. `last_trade_price` is that
/// answer. Without it, a settlement instant landing between an arm and the
/// next trade would be unpriceable forever and would freeze the settlement
/// frontier for the rest of the run.
impl Rivers {
    pub(crate) fn last_trade_at_or_before(
        &self,
        symbol: &str,
        ts: u64,
    ) -> anyhow::Result<Option<Decimal>> {
        let Some(river) = self.river(symbol) else {
            return Ok(None);
        };
        reach_river(&river, ts)?;
        let mut budget = crate::fills::SWEEP_DRAIN_BUDGET;
        let mut back = 0usize;
        loop {
            let Some((mut source, exhausted)) =
                locked(&river.checkpoints).try_source_before_target(ts, back)
            else {
                return Ok(None);
            };
            // Captured BEFORE draining: it is the resume point's own last consumed
            // print, whose ts is at most the snapshot clock and therefore strictly
            // before `ts` (the partition is strict). Only consulted when the chain
            // is exhausted and the residual held nothing.
            let fence_price = if exhausted {
                source.last_trade_price()
            } else {
                None
            };
            let mut last = None;
            let mut drained = 0usize;
            while let Some(tick) = source.next_tick() {
                if drained >= budget {
                    return Ok(None);
                }
                drained += 1;
                let TickEvent::Trade(trade) = tick else {
                    continue;
                };
                if trade.ts_event > ts {
                    break;
                }
                last = Some(trade.price);
            }
            if last.is_some() || exhausted {
                return Ok(last.or(fence_price));
            }
            budget = budget.saturating_sub(drained);
            if budget == 0 {
                return Ok(None);
            }
            back = back.saturating_mul(2).max(1);
        }
    }
}

#[cfg(test)]
mod river_tests {
    use super::*;

    #[test]
    fn concurrent_first_readers_share_one_river() {
        let rivers = crate::fills::test_rivers();
        // COLLECT the spawns before joining any of them: chaining `.map(spawn)`
        // straight into `.map(join)` is lazy, so each thread would be joined
        // before the next is spawned and the contention this test exists to
        // exercise would never happen.
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let rivers = Arc::clone(&rivers);
                std::thread::spawn(move || rivers.river_handle_for_test("BTCUSDT").unwrap())
            })
            .collect();
        let handles: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(handles.iter().all(|river| Arc::ptr_eq(&handles[0], river)));
    }

    #[test]
    fn the_live_river_is_not_extended_by_a_reader() {
        let rivers = crate::fills::test_rivers();
        let river = rivers.river_handle_for_test("BTCUSDT").unwrap();
        let before = locked(&river.checkpoints).frontier_ns();
        assert!(rivers.activate_live("BTCUSDT"));
        assert!(
            rivers
                .history_source("BTCUSDT", Some(before.saturating_add(86_400_000_000_000)))
                .is_err()
        );
        assert_eq!(locked(&river.checkpoints).frontier_ns(), before);
    }

    /// The race the sequential test above CANNOT catch, and the reason
    /// `extend_toward_unless_live` exists rather than an `is_live` accessor a
    /// caller consults before extending.
    #[test]
    fn activation_racing_a_cold_reach_never_moves_the_live_frontier() {
        for _ in 0..16 {
            let rivers = crate::fills::test_rivers();
            let river = rivers.river_handle_for_test("BTCUSDT").unwrap();
            let reader = {
                let rivers = Arc::clone(&rivers);
                std::thread::spawn(move || {
                    // Either verdict is legal; what may never happen is a walk
                    // that lands AFTER the activation.
                    drop(rivers.history_source("BTCUSDT", Some(3_600_000_000_000)));
                })
            };
            let activator = {
                let rivers = Arc::clone(&rivers);
                let river = Arc::clone(&river);
                std::thread::spawn(move || {
                    assert!(rivers.activate_live("BTCUSDT"));
                    locked(&river.checkpoints).frontier_ns()
                })
            };
            reader.join().unwrap();
            let at_activation = activator.join().unwrap();
            // Whether the reader won the race or lost it, nothing may have
            // extended the frontier AFTER the paced worker took the lead.
            assert_eq!(locked(&river.checkpoints).frontier_ns(), at_activation);
        }
    }

    /// A river nothing warmed answers an instant a long way from the origin.
    ///
    /// This does NOT bite on removing the `reach_river` call in
    /// `history_source`: `CheckpointIndex::try_source_before_target` extends a
    /// non-live index itself, and `MAX_EXTEND_TICKS` is far above any distance a
    /// test can walk, so positioning alone would answer here too. What the
    /// explicit reach adds is the ceiling, the named error on a stopped
    /// generator, and the atomic live check - all covered by the tests around
    /// this one. What this test pins is the capability: no boot warmup ran for
    /// this river and it answers anyway.
    #[test]
    fn a_cold_river_reaches_an_instant_far_past_the_origin() {
        let rivers = crate::fills::test_rivers();
        let reached = rivers
            .ensure_reach("BTCUSDT", 3_600_000_000_000)
            .expect("a cold river materializes on demand");
        assert!(reached > 0, "the walk retained checkpoints");
        assert!(matches!(
            rivers
                .history_source("BTCUSDT", Some(3_600_000_000_000))
                .expect("cold history"),
            History::Source(_)
        ));
    }

    /// A second configured shape is realized under its OWN def, on its OWN
    /// chain and tape path, locked independently of the boot river's.
    #[test]
    fn a_second_river_is_realized_under_its_own_def_and_chain() {
        let rivers = crate::fills::test_rivers_with_a_second_symbol();
        let boot = rivers.river_handle_for_test("BTCUSDT").unwrap();
        let second = rivers.river_handle_for_test("SECOND").unwrap();
        assert!(
            !Arc::ptr_eq(&boot, &second),
            "one chain per configured shape"
        );
        let History::Source(mut boot_source) = rivers
            .history_source("BTCUSDT", Some(TAPE_ORIGIN_NS))
            .expect("boot river history")
        else {
            panic!("the boot symbol is configured");
        };
        let boot_prints: Vec<_> = std::iter::from_fn(|| boot_source.next_tick())
            .filter_map(|tick| match tick {
                TickEvent::Trade(trade) => Some((trade.ts_event, trade.price)),
                _ => None,
            })
            .take(32)
            .collect();
        assert_eq!(boot_prints.len(), 32, "the boot river prints");
        let History::Source(mut second_source) = rivers
            .history_source("SECOND", Some(TAPE_ORIGIN_NS))
            .expect("second river history")
        else {
            panic!("the second symbol is configured");
        };
        let second_prints: Vec<_> = std::iter::from_fn(|| second_source.next_tick())
            .filter_map(|tick| match tick {
                TickEvent::Trade(trade) => {
                    assert_eq!(trade.symbol.as_ref(), "SECOND");
                    Some((trade.ts_event, trade.price))
                }
                _ => None,
            })
            .take(32)
            .collect();
        assert_eq!(second_prints.len(), 32, "the second river prints");
        assert_ne!(
            boot_prints, second_prints,
            "symbol labels select distinct tapes"
        );
        // Materializing the second river leaves the boot river where it was:
        // the chains advance independently, which is the whole point of keying
        // them.
        rivers
            .ensure_reach("SECOND", 600_000_000_000)
            .expect("second river materializes");
        assert!(locked(&second.checkpoints).frontier_ns() >= 600_000_000_000);
        assert_eq!(locked(&boot.checkpoints).frontier_ns(), TAPE_ORIGIN_NS);
    }

    /// A blown reach ceiling is an ERROR, never an empty window: swallowing it
    /// would leave `/trades` answering `200 []`, indistinguishable from a window
    /// nothing traded in.
    #[test]
    fn a_reach_failure_is_an_error_not_an_empty_window() {
        let rivers = crate::fills::test_rivers();
        let river = rivers.river_handle_for_test("BTCUSDT").unwrap();
        let error = reach_river_within(&river, 3_600_000_000_000, 16)
            .expect_err("16 ticks cannot reach an hour of tape");
        assert!(format!("{error}").contains("reach ceiling"));
    }
}
