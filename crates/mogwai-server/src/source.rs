// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic sources for the one eagerly warmed run tape.

use mogwai_data::TickEvent;
use mogwai_data::{
    CheckpointIndex, Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, SessionProfile,
    TickSource,
};
use mogwai_protocol::{InstrumentDef, MarketRegime, Symbol, default_instruments};
use rust_decimal::Decimal;
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
};

pub(crate) fn fingerprint() -> &'static Fingerprint {
    static FP: OnceLock<Fingerprint> = OnceLock::new();
    FP.get_or_init(Fingerprint::from_repo_json)
}
pub(crate) fn seed_for(symbol: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    symbol.bytes().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(PRIME)
    })
}

#[derive(Debug, Clone)]
pub(crate) struct InstrumentProfile {
    pub(crate) def: InstrumentDef,
    pub(crate) scalars: GeneratorScalars,
    pub(crate) session: SessionProfile,
}
impl InstrumentProfile {
    pub(crate) fn new(
        def: InstrumentDef,
        mut scalars: GeneratorScalars,
        session: SessionProfile,
    ) -> Self {
        scalars.symbol = def.symbol.clone();
        Self {
            def,
            scalars,
            session,
        }
    }
}
#[derive(Debug, Clone)]
pub(crate) struct InstrumentProfiles {
    by_symbol: HashMap<Symbol, InstrumentProfile>,
}
impl InstrumentProfiles {
    pub(crate) fn defaults() -> Self {
        Self::from_profiles(
            default_instruments()
                .into_iter()
                .map(|def| default_profile(def, fingerprint()))
                .collect(),
        )
    }
    pub(crate) fn from_profiles(profiles: Vec<InstrumentProfile>) -> Self {
        Self {
            by_symbol: profiles
                .into_iter()
                .map(|profile| (profile.def.symbol.clone(), profile))
                .collect(),
        }
    }
    pub(crate) fn get(&self, symbol: &str) -> Option<&InstrumentProfile> {
        self.by_symbol.get(symbol)
    }
    pub(crate) fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self
            .by_symbol
            .values()
            .map(|profile| profile.def.clone())
            .collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }
}
fn default_profile(def: InstrumentDef, fp: &Fingerprint) -> InstrumentProfile {
    let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, fp);
    scalars.modal_tick = def.price_increment;
    scalars.price_decimals = u32::from(def.price_precision);
    InstrumentProfile::new(def, scalars, fp.session_profile.clone())
}
fn generator(
    profile: &InstrumentProfile,
    data_origin: u64,
    regime: Option<MarketRegime>,
) -> GeneratedSource {
    GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        seed_for(&profile.def.symbol),
        data_origin,
        fingerprint(),
        &profile.session,
        regime,
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
pub(crate) const CHECKPOINT_K: usize = 8192;

/// Runaway backstop on a SINGLE `extend_to` walk. Deliberately NOT a request
/// budget: every legitimate target (the warmup floor, sim-now, a live cursor)
/// sits far inside it, and it exists only so a nonsensical far-future target
/// cannot spin the never-ending generator while holding the index lock.
const MAX_EXTEND_TICKS: usize = 1 << 30;

/// The run's one checkpoint chain, over the run's one realization. Process
/// global because the run is: one instrument, one regime, one origin for the
/// life of the process, so there is nothing left to key it by.
static INDEX: OnceLock<Mutex<CheckpointIndex>> = OnceLock::new();

/// The regime this run was launched with. Boot config now rather than a
/// per-subscription choice, so the whole process shares one realization.
static BOOT_REGIME: OnceLock<Option<MarketRegime>> = OnceLock::new();

pub(crate) fn set_boot_regime(regime: Option<MarketRegime>) {
    // A second set is a no-op by construction, which is the honest behaviour:
    // one process is one run and the regime is fixed at boot.
    if BOOT_REGIME.set(regime).is_err() {
        tracing::debug!("boot regime was already fixed; one process is one run");
    }
}

fn boot_regime() -> Option<MarketRegime> {
    BOOT_REGIME.get().copied().flatten()
}

/// The run's chain, built on first use and reused thereafter.
///
/// `data_origin` is read ONLY on that first call - it is checkpoint zero, and a
/// chain cannot be re-based without discarding every snapshot on it. That is
/// safe because the first caller is always `materialize_warmup` at boot, whose
/// origin is at or below every origin a later caller passes: the boot estimate
/// is taken before warmup generation while `Run::data_origin_ns` is derived
/// from the post-warmup `run_start_ns`, so the chain reaches at least as far
/// back as anything asked of it and every later `data_origin` argument is
/// already inside it.
fn index(
    symbol: &str,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<&'static Mutex<CheckpointIndex>> {
    if let Some(existing) = INDEX.get() {
        return Some(existing);
    }
    let profile = profiles.get(symbol)?;
    let origin = generator(profile, data_origin, boot_regime());
    Some(
        INDEX.get_or_init(|| {
            Mutex::new(CheckpointIndex::new(origin, CHECKPOINT_K, MAX_EXTEND_TICKS))
        }),
    )
}

fn locked(
    index: &'static Mutex<CheckpointIndex>,
) -> std::sync::MutexGuard<'static, CheckpointIndex> {
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The live tape's source, positioned at sim-now.
pub(crate) fn build_live_source(
    symbol: &str,
    profiles: &InstrumentProfiles,
    data_origin: u64,
    sim_now: u64,
) -> Option<Box<dyn TickSource>> {
    build_history_source(symbol, Some(sim_now), profiles, data_origin)
}

/// A source positioned at `start`, RESUMED from the run's checkpoint chain
/// rather than re-walked from the origin. `None` means the tape origin, which
/// is checkpoint zero and therefore free.
pub(crate) fn build_history_source(
    symbol: &str,
    start: Option<u64>,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Box<dyn TickSource>> {
    let index = index(symbol, profiles, data_origin)?;
    let positioned = locked(index).source_at_or_before(start.unwrap_or(data_origin));
    Some(Box::new(MergeSource::starting_at(
        vec![Box::new(positioned)],
        start,
    )))
}

/// Generate and HOLD `warmup_ns` of tape ending at `run_start_ns`, before the
/// readiness record is written.
///
/// Holding it is what the checkpoint chain does: once this returns, every
/// instant in `[data_origin, run_start_ns]` is reachable by a resume plus a
/// bounded residual replay, so a history request for the earliest servable
/// instant is ANSWERED rather than refused or silently served short. Returns
/// the number of snapshots retained, for the boot log.
pub(crate) fn materialize_warmup(
    symbol: &str,
    profiles: &InstrumentProfiles,
    regime: Option<MarketRegime>,
    data_origin: u64,
    run_start_ns: u64,
) -> anyhow::Result<usize> {
    set_boot_regime(regime);
    let Some(index) = index(symbol, profiles, data_origin) else {
        anyhow::bail!("configured warmup symbol {symbol} has no source");
    };
    let mut guard = locked(index);
    let mut frontier = guard.source_at_or_before(run_start_ns);
    // Draw one tick past the resumed frontier, so the chain is proven to REACH
    // the run start rather than merely to have been asked for it.
    anyhow::ensure!(
        frontier.next_tick().is_some(),
        "warmup generation produced no tick at the run start"
    );
    Ok(guard.checkpoint_count())
}

/// The last trade printed at or before `ts`. Positioned from the chain at a
/// checkpoint no later than `ts` and walked forward only the residual, so this
/// costs the same whether `ts` is one second or one day into the run.
pub(crate) fn last_trade_at_or_before(
    symbol: &str,
    ts: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Decimal> {
    let index = index(symbol, profiles, data_origin)?;
    let mut source = locked(index).source_at_or_before(ts);
    let mut last = None;
    while let Some(TickEvent::Trade(trade)) = source.next_tick() {
        if trade.ts_event > ts {
            break;
        }
        last = Some(trade.price);
    }
    last
}
