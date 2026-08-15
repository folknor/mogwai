// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic sources for the one eagerly warmed run tape.

use mogwai_data::TickEvent;
use mogwai_data::{
    CheckpointIndex, Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, SessionProfile,
    SizeGrid, TickSource,
};
use mogwai_protocol::{InstrumentDef, MarketRegime, RunSeeds, Symbol};
use rust_decimal::Decimal;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
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
}
impl InstrumentProfiles {
    pub fn from_profiles(profiles: Vec<InstrumentProfile>) -> Self {
        Self {
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
}
fn generator(profile: &InstrumentProfile) -> GeneratedSource {
    let boot = *BOOT
        .get()
        .expect("warmup fixes the run tape before it is read");
    GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        boot.seeds.tape,
        TAPE_ORIGIN_NS,
        fingerprint(),
        &profile.session,
        boot.regime,
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

/// Runaway backstop on a SINGLE extension while the global index lock is held.
/// This retains the original one-billion-tick safety purpose: a nonsensical
/// far-future request cannot turn one lock acquisition into the
/// 667-billion-tick warmup reach bound below.
const MAX_EXTEND_TICKS: usize = 1 << 30;

/// Total legitimate boot reach admitted across lock-releasing extension
/// chunks. Protocol 10's headroom rule set 667,299,000,000 against its
/// measured 2.055x warmup-reach expansion; protocol 11's session refit
/// measures an 11/10 ratio of 1.00036, and the standing rule - prior times
/// ratio times two, next-million rounding, then the larger of that and the
/// 81,123,436,742-frame required reach - lands here. Keeping it separate
/// from `MAX_EXTEND_TICKS` prevents a reach requirement from silently
/// disabling the per-lock runaway backstop.
const MAX_WARMUP_MATERIALIZATION_TICKS: usize = 1_335_079_000_000;

/// The run's one checkpoint chain, over the run's one realization. Process
/// global because the run is: one instrument, one regime, one origin for the
/// life of the process, so there is nothing left to key it by.
struct RunIndex {
    symbol: Symbol,
    checkpoints: Mutex<CheckpointIndex>,
}

static INDEX: OnceLock<RunIndex> = OnceLock::new();

/// Everything that fixes WHICH tape this process serves: the run's derived
/// seeds and the regime it was launched with. Boot config now rather than a
/// per-subscription choice, so the whole process shares one realization. One
/// struct rather than two globals, because both are set at the same instant by
/// the same caller and read by the same function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BootTape {
    pub(crate) seeds: RunSeeds,
    pub(crate) regime: Option<MarketRegime>,
}

static BOOT: OnceLock<BootTape> = OnceLock::new();

/// Installs the boot tape for an in-crate test, or ASSERTS that the one already
/// installed is identical. The checkpoint chain is process-global, so whichever
/// test touches it first would otherwise fix the tape seed for every later test
/// in the binary and turn this into a silent no-op. Every in-crate test that
/// reaches the chain goes through here with the same `BootTape`, so a future
/// test wanting a different one fails immediately and by name.
#[cfg(test)]
pub(crate) fn set_boot_for_test(boot: BootTape) {
    let installed = *BOOT.get_or_init(|| boot);
    assert_eq!(
        installed, boot,
        "boot tape collision: installed {installed:?}, requested {boot:?}"
    );
}

/// The run's chain, rooted at the fixed tape origin and reused thereafter.
fn index(symbol: &str, profiles: &InstrumentProfiles) -> Option<&'static Mutex<CheckpointIndex>> {
    let profile = profiles.get(symbol)?;
    if let Some(existing) = INDEX.get() {
        return (existing.symbol.as_ref() == symbol).then_some(&existing.checkpoints);
    }
    let run_index = INDEX.get_or_init(|| RunIndex {
        symbol: std::sync::Arc::clone(&profile.def.symbol),
        checkpoints: Mutex::new(CheckpointIndex::new(
            generator(profile),
            CHECKPOINT_K,
            MAX_EXTEND_TICKS,
        )),
    });
    (run_index.symbol.as_ref() == symbol).then_some(&run_index.checkpoints)
}

fn locked(
    index: &'static Mutex<CheckpointIndex>,
) -> std::sync::MutexGuard<'static, CheckpointIndex> {
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The live tape's source, positioned at sim-now.
#[cfg(test)]
pub(crate) fn build_live_source(
    symbol: &str,
    profiles: &InstrumentProfiles,
    sim_now: u64,
) -> Option<Box<dyn TickSource>> {
    build_history_source(symbol, Some(sim_now), profiles)
}

/// Advance the one canonical run tape. The paced feed, checkpoint history,
/// fill scans and mark reads all branch from this same frontier.
pub(crate) fn next_live_tick(
    symbol: &str,
    profiles: &InstrumentProfiles,
) -> Option<(Option<TickEvent>, Option<mogwai_data::TickFault>)> {
    let index = index(symbol, profiles)?;
    let mut index = locked(index);
    let tick = index.next_tick();
    let fault = tick.is_none().then(|| index.fault()).flatten();
    Some((tick, fault))
}

pub(crate) fn activate_live(symbol: &str, profiles: &InstrumentProfiles) -> bool {
    let Some(index) = index(symbol, profiles) else {
        return false;
    };
    locked(index).activate_live();
    true
}

pub(crate) fn arm_flow_surge(
    symbol: &str,
    profiles: &InstrumentProfiles,
    start_ns: u64,
    duration_ms: u64,
    rate_mult: f64,
    children_mult: f64,
) -> bool {
    let Some(index) = index(symbol, profiles) else {
        return false;
    };
    locked(index).arm_flow_surge(start_ns, duration_ms, rate_mult, children_mult);
    true
}

pub(crate) fn clear_flow_surge(symbol: &str, profiles: &InstrumentProfiles) -> bool {
    let Some(index) = index(symbol, profiles) else {
        return false;
    };
    locked(index).clear_flow_surge();
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
pub(crate) fn build_history_source(
    symbol: &str,
    start: Option<u64>,
    profiles: &InstrumentProfiles,
) -> Option<Box<dyn TickSource>> {
    let index = index(symbol, profiles)?;
    let positioned = locked(index).try_source_at_or_before(start.unwrap_or(TAPE_ORIGIN_NS))?;
    Some(Box::new(MergeSource::starting_at(
        vec![Box::new(positioned)],
        start,
    )))
}

/// Generate and HOLD `warmup_ns` of tape ending at `run_start_ns`, before the
/// readiness record is written.
///
/// Holding it is what the checkpoint chain does: once this returns, every
/// instant in `[TAPE_ORIGIN_NS, run_start_ns]` is reachable by a resume plus a
/// bounded residual replay, so a history request for the earliest servable
/// instant is ANSWERED rather than refused or silently served short. Returns
/// the number of snapshots retained, for the boot log.
pub(crate) fn materialize_warmup(
    symbol: &str,
    profiles: &InstrumentProfiles,
    boot: BootTape,
    run_start_ns: u64,
) -> anyhow::Result<usize> {
    // `OnceLock::set` hands BACK the rejected value on failure, not the stored
    // one, so the installed tape is read separately or the message would name
    // the wrong seed.
    if BOOT.set(boot).is_err() {
        anyhow::bail!("boot tape was already fixed as {:?}", BOOT.get());
    }
    let Some(index) = index(symbol, profiles) else {
        anyhow::bail!("configured warmup symbol {symbol} has no source");
    };
    let mut walked_total = 0usize;
    loop {
        let (walked, frontier_ns, checkpoints) = {
            let mut guard = locked(index);
            let walked = guard.extend_toward(run_start_ns);
            (walked, guard.frontier_ns(), guard.checkpoint_count())
        };
        walked_total = walked_total.saturating_add(walked);
        anyhow::ensure!(
            walked_total <= MAX_WARMUP_MATERIALIZATION_TICKS,
            "warmup generation exceeded its measured {MAX_WARMUP_MATERIALIZATION_TICKS}-tick reach ceiling"
        );
        if frontier_ns >= run_start_ns {
            return Ok(checkpoints);
        }
        anyhow::ensure!(walked > 0, "warmup generator stopped before the run start");
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
pub(crate) fn last_trade_at_or_before(
    symbol: &str,
    ts: u64,
    profiles: &InstrumentProfiles,
) -> Option<Decimal> {
    let index = index(symbol, profiles)?;
    let mut budget = crate::fills::SWEEP_DRAIN_BUDGET;
    let mut back = 0usize;
    loop {
        let (mut source, exhausted) = locked(index).try_source_before_target(ts, back)?;
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
                return None;
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
            return last.or(fence_price);
        }
        budget = budget.saturating_sub(drained);
        if budget == 0 {
            return None;
        }
        back = back.saturating_mul(2).max(1);
    }
}
