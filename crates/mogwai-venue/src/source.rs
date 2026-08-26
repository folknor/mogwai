// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic, lazily materialized rivers owned by one serving process.

use mogwai_data::TickEvent;
use mogwai_data::{
    CheckpointIndex, Fingerprint, GeneratedSource, GeneratorScalars, MergeSource, SessionProfile,
    SizeGrid, TickSource,
};
use mogwai_protocol::{InstrumentDef, MarketRegime, RunSeeds, Symbol, control::GeneratorArm};
use rust_decimal::Decimal;
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

pub fn fingerprint() -> &'static Fingerprint {
    Fingerprint::repo()
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
#[derive(Debug)]
pub struct InstrumentProfiles {
    cfg: Option<Arc<crate::config::Config>>,
    configured: HashMap<Symbol, Arc<InstrumentProfile>>,
    /// The symbol of the first profile handed to `from_profiles`, which
    /// `build_instrument_profiles` sweeps from the config's boot shape. Kept
    /// because the boot shape is not recoverable from the map: the config's
    /// top-level `symbol` may be absent, and an absent one does not mean
    /// `DEFAULT_PRESET` - `[instrument] preset = "MNQ"` with no top-level symbol
    /// resolves a shape whose symbol is MNQ, and looking up BTCUSDT would then
    /// refuse a config that boots perfectly well.
    boot: Option<Symbol>,
    /// Shapes resolved for symbols nobody configured, memoized under the exact
    /// requested label. Uncapped on purpose: a profile is two small structs,
    /// and the capped resource is the river, charged in `Rivers`.
    resolved: Mutex<HashMap<Symbol, Arc<InstrumentProfile>>>,
    /// Settlement currencies reachable from this run's closed shape set that
    /// its `[balances]` table does not fund. Computed once at boot; a request
    /// landing on one is refused at bind rather than left to a fill-time funds
    /// rejection an operator could have fixed.
    funding_barred: std::collections::HashSet<String>,
}

/// Why a symbol has no shape. Distinct from `MaterializeRefusal`, which is the
/// layer below: this one is decided from the config alone.
#[derive(Debug)]
pub enum ResolveRefusal {
    IllegalSymbol(String),
    Invalid(anyhow::Error),
    FundingBarred { symbol: String, currency: String },
}
impl std::fmt::Display for ResolveRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IllegalSymbol(reason) => write!(f, "illegal symbol: {reason}"),
            Self::Invalid(error) => write!(f, "invalid resolved shape: {error:#}"),
            // The `[balances]` key that would fund the shape IS the currency
            // code, so the message names it once and says where to put it.
            Self::FundingBarred { symbol, currency } => write!(
                f,
                "symbol {symbol} resolves to a shape settling in {currency}, which this run does not fund; add {currency} to [balances]"
            ),
        }
    }
}
impl std::error::Error for ResolveRefusal {}
impl InstrumentProfiles {
    pub fn from_profiles(profiles: Vec<InstrumentProfile>) -> Self {
        Self {
            cfg: None,
            boot: profiles
                .first()
                .map(|profile| std::sync::Arc::clone(&profile.def.symbol)),
            configured: profiles
                .into_iter()
                .map(|profile| (Arc::clone(&profile.def.symbol), Arc::new(profile)))
                .collect(),
            resolved: Mutex::new(HashMap::new()),
            funding_barred: std::collections::HashSet::new(),
        }
    }
    /// The total resolver: the same configured shapes, plus the config needed
    /// to resolve a label nobody configured. `from_profiles` stays non-total on
    /// purpose, so a test rig does not silently acquire consumer-driven
    /// resolution it was never written against.
    pub(crate) fn from_config(
        cfg: Arc<crate::config::Config>,
        profiles: Vec<InstrumentProfile>,
        funding_barred: std::collections::HashSet<String>,
    ) -> Self {
        let mut value = Self::from_profiles(profiles);
        value.cfg = Some(cfg);
        value.funding_barred = funding_barred;
        value
    }
    pub fn configured(&self, symbol: &str) -> Option<Arc<InstrumentProfile>> {
        self.configured.get(symbol).cloned()
    }
    /// The shape this run serves under `symbol`, resolving and memoizing one
    /// for a label nobody configured.
    ///
    /// A configured symbol answers without consulting the funding-barred set:
    /// boot already refused a configured shape it cannot fund, so a hit there
    /// is by construction fundable.
    ///
    /// The memo check, the TOML merge and the insertion all happen under one
    /// acquisition, so two concurrent callers on the same label cannot each
    /// retain a different `Arc` - `Arc::ptr_eq` is a property the river keying
    /// and the tests both rely on.
    pub(crate) fn resolve(&self, symbol: &str) -> Result<Arc<InstrumentProfile>, ResolveRefusal> {
        if let Some(profile) = self.configured(symbol) {
            return Ok(profile);
        }
        mogwai_protocol::validate_wire_symbol(symbol)
            .map_err(|reason| ResolveRefusal::IllegalSymbol(reason.to_owned()))?;
        let mut resolved = self
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(profile) = resolved.get(symbol) {
            return Ok(Arc::clone(profile));
        }
        let cfg = self.cfg.as_ref().ok_or_else(|| {
            ResolveRefusal::Invalid(anyhow::anyhow!(
                "this resolver was built from profiles alone and serves only configured symbols"
            ))
        })?;
        let profile =
            crate::config::profile_for(cfg, Some(symbol)).map_err(ResolveRefusal::Invalid)?;
        let currency = profile.def.class.settlement_currency();
        if self.funding_barred.contains(currency) {
            return Err(ResolveRefusal::FundingBarred {
                symbol: symbol.to_owned(),
                currency: currency.to_owned(),
            });
        }
        let profile = Arc::new(profile);
        resolved.insert(Arc::clone(&profile.def.symbol), Arc::clone(&profile));
        Ok(profile)
    }
    /// Every configured profile, in unspecified order. For the callers that
    /// pre-derive something per profile at construction; anything a human or a
    /// test reads sorts through `configured_symbols` instead.
    pub fn iter(&self) -> impl Iterator<Item = &InstrumentProfile> {
        self.configured.values().map(AsRef::as_ref)
    }
    /// Every symbol an operator configured, sorted. NOT "every symbol this run
    /// can serve" - resolution is total, so that set is every wire-legal
    /// string. Sorted because a human reads it and a test diffs it; `HashMap`
    /// order is neither.
    pub fn configured_symbols(&self) -> Vec<&str> {
        let mut symbols: Vec<_> = self.configured.keys().map(|symbol| &**symbol).collect();
        symbols.sort_unstable();
        symbols
    }
    /// The configured defs alone. `/instruments` answers from
    /// `Rivers::instrument_defs`, which unions these with what has actually
    /// materialized.
    pub fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs: Vec<_> = self
            .configured
            .values()
            .map(|profile| profile.def.clone())
            .collect();
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }
    /// The def of the shape this run boots its paced tape on, resolved by name.
    ///
    /// Not "the sole entry": once profiles are plural there need not be one, and
    /// a sole-entry rule would silently pick the wrong shape for a config with a
    /// top-level `symbol` plus a `[symbols.*]` table. An absent `symbol` falls
    /// back to the shape the sweep resolved first, which is exactly what the
    /// config's own defaulting produced for a `None` symbol.
    pub fn default_symbol_def(&self, symbol: Option<&str>) -> anyhow::Result<InstrumentDef> {
        let named = symbol
            .or(self.boot.as_deref())
            .unwrap_or(crate::config::DEFAULT_PRESET);
        self.configured(named)
            .map(|profile| profile.def.clone())
            .ok_or_else(|| anyhow::anyhow!("boot symbol {named} has no configured shape"))
    }
}
fn generator(
    label: &str,
    profile: &InstrumentProfile,
    identity: TapeIdentity,
    arm: Option<GeneratorArm>,
    run_origin_ns: u64,
) -> GeneratedSource {
    let source = GeneratedSource::new_with_session_profile(
        profile.scalars.clone(),
        identity.seeds.tape_for(label),
        TAPE_ORIGIN_NS,
        Fingerprint::repo(),
        &profile.session,
        identity.regime,
        SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    );
    // Applied to a source that has drawn nothing, which is what makes the
    // surge part of this river rather than an event in its walk. The offset is
    // resolved against the run origin here, once, so the generator only ever
    // sees an absolute instant on its own axis.
    match arm {
        Some(arm) => source.with_surge(
            run_origin_ns.saturating_add(arm.start_offset_ns),
            arm.duration_ms,
            arm.rate_mult(),
            arm.children_mult(),
        ),
        None => source,
    }
}

/// Snapshot spacing of the run's checkpoint chain, in ticks. The generator is
/// path-dependent, so reaching an instant means walking to it; a snapshot every
/// `CHECKPOINT_K` ticks turns that O(distance) walk into a resume plus a
/// residual replay of fewer than `CHECKPOINT_K` ticks.
///
/// This chain is the mechanism that materializes the warmup, which is why it
/// survives the lifecycle rewrite. What that rewrite removed is the per-request
/// seek budget that used to cap it: there is no `MAX_HISTORY_SEEK_TICKS` any
/// more and no request is served short because it ran out of walk. A request
/// below the floor is refused by name instead, which is the whole point.
/// The stride is a latency/memory tradeoff, not a reach budget. Reach and
/// refusal ceilings are sized separately below. At 8,192 ticks the initial
/// grid bounds an ordinary restore to a few milliseconds while the hard
/// `MAX_CHECKPOINTS` cap still bounds retained generator clones; exceptionally
/// long runs coarsen the grid as documented by `CheckpointIndex`.
///
/// Choosing this value at all is only safe because the tape does not depend on
/// it. `CheckpointIndex::extend_toward` selects between whole-parent
/// advancement and per-tick advancement from `k`, the remaining budget and the
/// target, so a stride that changed the water would make the tape a function of
/// this venue-side constant, outside anything `TAPE_PROTOCOL_VERSION` could
/// catch. The two advancement paths are continuation-identical, and the gate on
/// that premise lives in `mogwai-data`, in
/// `compact_parent_advancement_matches_wire_frames_and_continuation`.
pub(crate) const CHECKPOINT_K: usize = 8_192;
/// How many distinct rivers one run may materialize.
///
/// The expensive resource is the river - a permanent checkpoint chain, and on a
/// bind a boat and its paced task - not the profile memo, which is two small
/// structs and stays uncapped. Charged where the river is created, so a history
/// poll and a socket bind spend the same budget, and there is no eviction: an
/// evicted river would resurrect at a different position, which is a worse lie
/// than a refusal. Exhaustion by 256 genuinely materialized rivers is an
/// operational contract, not a hole: this venue serves the owner's own agents,
/// one consumer population per run, and mounts no defence against a consumer that
/// spends the budget deliberately.
pub(crate) const MAX_MATERIALIZED_RIVERS: usize = 256;

/// Runaway backstop on a single extension while one river lock is held.
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

/// Which river a request names.
///
/// The bundle digest covers the resolved instrument shape, the tape seed and
/// the boot regime - all of it operator-owned, fixed at boot, and the same for
/// every passenger. The arm is not: it is chosen per passenger, so it is held
/// structurally rather than folded into the digest. That asymmetry is
/// deliberate. A digest collision here does not merely mislabel a river, it
/// hands a passenger water belonging to another key, and a value the passenger
/// controls is a value a passenger could search for collisions in. Keeping the
/// passenger-chosen half exact means equality can only be reached by asking for
/// the same arm.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RiverKey {
    symbol: Symbol,
    bundle: u64,
    arm: Option<GeneratorArm>,
}
impl RiverKey {
    /// A key that names no materializable river, for tests about machinery that
    /// only compares keys. Distinct `tag`s are distinct rivers.
    #[cfg(test)]
    pub(crate) fn synthetic(tag: u64) -> Self {
        Self {
            symbol: Symbol::from(format!("SYNTHETIC-{tag}").as_str()),
            bundle: tag,
            arm: None,
        }
    }

    pub(crate) fn resolve(
        profile: &InstrumentProfile,
        identity: TapeIdentity,
        arm: Option<GeneratorArm>,
    ) -> Self {
        let mut digest = std::collections::hash_map::DefaultHasher::new();
        let hash_f64 = |value: f64, digest: &mut std::collections::hash_map::DefaultHasher| {
            value.to_bits().hash(digest);
        };
        profile.def.symbol.hash(&mut digest);
        identity
            .seeds
            .tape_for(&profile.def.symbol)
            .hash(&mut digest);
        serde_json::to_string(&identity.regime)
            .expect("MarketRegime serializes")
            .hash(&mut digest);
        // Canonical field sequence. Keep it explicit: neither Debug output nor
        // map order is an identity contract.
        serde_json::to_string(&profile.def)
            .expect("InstrumentDef serializes")
            .hash(&mut digest);
        let scalars = &profile.scalars;
        scalars.symbol.hash(&mut digest);
        scalars.modal_tick.hash(&mut digest);
        scalars.price_decimals.hash(&mut digest);
        for value in [
            scalars.mean_event_duration_s,
            scalars.children_mean,
            scalars.children_single_frac,
            scalars.levels_mean,
            scalars.size_round_frac,
            scalars.size_log_sigma,
            scalars.vol_scalar,
            scalars.trade_displacement_ticks.ticks(),
        ] {
            hash_f64(value, &mut digest);
        }
        scalars.start_price.hash(&mut digest);
        scalars.latent_size_median.hash(&mut digest);
        scalars.quoted_width.ticks().get().hash(&mut digest);
        scalars.top_sizes.bid.hash(&mut digest);
        scalars.top_sizes.ask.hash(&mut digest);
        // `depth_levels` and `depth_growth` are deliberately absent. This
        // digest identifies a river - the bytes a tape produces - and the
        // ladder is derived at read time from the published top of book, so
        // moving either knob moves no tape byte. Hashing them would split the
        // river cache on a parameter the river does not depend on.
        match scalars.arrival {
            None => 0_u8.hash(&mut digest),
            Some(mogwai_data::ArrivalConfig::EventMarkov {
                quiet_share,
                switch_rate,
                rate_ratio,
            }) => {
                1_u8.hash(&mut digest);
                for value in [quiet_share, switch_rate, rate_ratio] {
                    hash_f64(value, &mut digest);
                }
            }
            Some(mogwai_data::ArrivalConfig::WallMmpp {
                occupancy,
                rate_ratio,
                tau_s,
            }) => {
                2_u8.hash(&mut digest);
                for value in [occupancy, rate_ratio, tau_s] {
                    hash_f64(value, &mut digest);
                }
            }
            Some(mogwai_data::ArrivalConfig::LogOuCox { sigma_y, tau_s }) => {
                3_u8.hash(&mut digest);
                for value in [sigma_y, tau_s] {
                    hash_f64(value, &mut digest);
                }
            }
            Some(mogwai_data::ArrivalConfig::SelfExciting { phi, tau_s }) => {
                4_u8.hash(&mut digest);
                for value in [phi, tau_s] {
                    hash_f64(value, &mut digest);
                }
            }
            Some(mogwai_data::ArrivalConfig::ShotNoise { m, k, tau_s }) => {
                5_u8.hash(&mut digest);
                for value in [m, k, tau_s] {
                    hash_f64(value, &mut digest);
                }
            }
        }
        for value in profile
            .session
            .intensity_hour
            .into_iter()
            .chain(profile.session.vol_hour)
            .chain(profile.session.dow_weight)
        {
            hash_f64(value, &mut digest);
        }
        if let Some(calendar) = &profile.calendar {
            calendar.utc_offset_minutes.hash(&mut digest);
            for window in &calendar.open_windows {
                window.start_minute.hash(&mut digest);
                window.end_minute.hash(&mut digest);
            }
            calendar.settlement_minute_of_day.hash(&mut digest);
        } else {
            0_u8.hash(&mut digest);
        }
        Self {
            symbol: Arc::clone(&profile.def.symbol),
            bundle: digest.finish(),
            arm,
        }
    }
    pub(crate) fn symbol(&self) -> &str {
        &self.symbol
    }
    pub(crate) fn bundle_digest(&self) -> u64 {
        self.bundle
    }
    /// A key naming a river no configured profile resolves to, so a test can
    /// force the placement path to fail without a second venue.
    #[cfg(test)]
    pub(crate) fn with_unresolvable_bundle(&self) -> Self {
        Self {
            symbol: Arc::clone(&self.symbol),
            bundle: self.bundle ^ 0x5fd0_e9a5_1c37_b2e1,
            arm: self.arm,
        }
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
    /// The served run's origin on the tape axis, which is the warmup span
    /// because the generator starts at `TAPE_ORIGIN_NS`. A generator arm's
    /// offset counts from here rather than from the tape origin: an offset of
    /// zero should mean the instant the run begins serving, not an instant
    /// buried in the warmup where a one-hour window could open and close
    /// before any passenger sees live water.
    run_origin_ns: u64,
    profiles: Arc<InstrumentProfiles>,
    /// Resolved once per configured profile at construction. `resolve` walks
    /// the whole knob bundle and serializes two values, while river resolution
    /// happens on every history read, every mark read and every placement, so
    /// re-deriving it per lookup would put a digest on the serving hot path.
    keys: Mutex<HashMap<(Symbol, Option<GeneratorArm>), RiverKey>>,
    rivers: Mutex<HashMap<RiverKey, Arc<River>>>,
}

fn locked(index: &Mutex<CheckpointIndex>) -> std::sync::MutexGuard<'_, CheckpointIndex> {
    index
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Why a river could not be produced. One layer below `ResolveRefusal`: these
/// arise only on the paths that actually spend the capped resource.
#[derive(Debug)]
pub(crate) enum MaterializeRefusal {
    Resolve(ResolveRefusal),
    CapacityExhausted {
        cap: usize,
        count: usize,
    },
    KeyMismatch {
        symbol: String,
    },
    /// The river exists but could not be walked to the requested instant. A
    /// synthesis failure, not a configuration one.
    Reach(anyhow::Error),
}
impl std::fmt::Display for MaterializeRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Resolve(error) => error.fmt(f),
            Self::CapacityExhausted { cap, count } => write!(
                f,
                "this run has materialized {count} rivers and its cap is {cap}; no further symbol can be served"
            ),
            Self::KeyMismatch { symbol } => write!(
                f,
                "the river key passed for symbol {symbol} is not the key this run resolved for it"
            ),
            Self::Reach(error) => write!(f, "{error:#}"),
        }
    }
}
impl MaterializeRefusal {
    /// Whether this is the venue failing to keep a promise it already made,
    /// rather than an answer to what the caller asked for.
    ///
    /// The split decides two consumer-visible things: the status a refused
    /// upgrade carries, and whether the run latches a terminal fault. Getting it
    /// wrong in either direction is worse than the defect it closes. A river cap
    /// is reachable, intentional and documented, so latching it would turn
    /// ordinary capacity admission into a kill switch; a synthesis failure
    /// reported as a bad request sends a consumer to fix a request that was
    /// never wrong, and leaves `/health` saying the venue is fine.
    pub(crate) fn is_venue_fault(&self) -> bool {
        match self {
            // `Reach` is the shape that was validated and whose generator still
            // could not walk the river to the placement origin. `KeyMismatch` is
            // an internal routing mistake: the run resolved a different key for
            // this symbol than the one it was handed, which no caller can
            // produce. Different causes, same consequence for the consumer -
            // there is nothing it can change - so both are ours.
            Self::Reach(_) | Self::KeyMismatch { .. } => true,
            // Deliberate resource admission, and a resolution the request or the
            // config owns. Both are answers, not failures.
            Self::CapacityExhausted { .. } | Self::Resolve(_) => false,
        }
    }
}
impl std::error::Error for MaterializeRefusal {}
impl From<ResolveRefusal> for MaterializeRefusal {
    fn from(value: ResolveRefusal) -> Self {
        Self::Resolve(value)
    }
}
struct BoatCursor {
    first: Option<TickEvent>,
    source: GeneratedSource,
}
impl TickSource for BoatCursor {
    fn next_tick(&mut self) -> Option<TickEvent> {
        self.first.take().or_else(|| self.source.next_tick())
    }
    fn fault(&self) -> Option<mogwai_data::TickFault> {
        self.source.fault()
    }
}

/// Test-only convenience: the `Option` shape the in-crate tests assert on,
/// collapsing every materialization refusal the way no production caller may.
#[cfg(test)]
pub(crate) fn build_history_source(
    symbol: &str,
    start: Option<u64>,
    rivers: &Rivers,
) -> Option<Box<dyn TickSource>> {
    rivers
        .history_source(&rivers.key_for_symbol(symbol).ok()?, start)
        .ok()
}

impl Rivers {
    pub(crate) fn new(
        identity: TapeIdentity,
        run_origin_ns: u64,
        profiles: Arc<InstrumentProfiles>,
    ) -> Arc<Self> {
        let keys = profiles
            .iter()
            .map(|profile| {
                (
                    (Arc::clone(&profile.def.symbol), None),
                    RiverKey::resolve(profile, identity, None),
                )
            })
            .collect();
        Arc::new(Self {
            identity,
            run_origin_ns,
            profiles,
            keys: Mutex::new(keys),
            rivers: Mutex::new(HashMap::new()),
        })
    }
    /// The configured profiles behind this registry. Test-only since piece 13:
    /// every serving path resolves through `resolve_profile`, and a production
    /// caller reaching past it for the configured map is exactly the
    /// configured-only lookup this landing deleted.
    #[cfg(test)]
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
    /// The sole configured-profile lookup in river resolution. Piece 13 makes
    /// this function return the default shape for an absent symbol; the rest
    /// of the boatyard already operates on resolved profiles and keys.
    pub(crate) fn resolve_profile(
        &self,
        symbol: &str,
    ) -> Result<Arc<InstrumentProfile>, ResolveRefusal> {
        self.profiles.resolve(symbol)
    }
    /// The key for a profile this registry was built from, under a given arm.
    ///
    /// This is the only place a key is minted, and every key that exists came
    /// from here - which is what `river` relies on instead of re-deriving. The
    /// memo is keyed by symbol and arm because one label now names several
    /// rivers; it exists because resolution happens on every history read,
    /// every mark read and every placement, and the bundle digest must not sit
    /// on that path. The first request for a given pair pays the digest and
    /// every later one pays a hash lookup.
    pub(crate) fn resolve_key(
        &self,
        profile: &InstrumentProfile,
        arm: Option<GeneratorArm>,
    ) -> RiverKey {
        let memo = (Arc::clone(&profile.def.symbol), arm);
        let mut keys = self
            .keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(key) = keys.get(&memo) {
            return key.clone();
        }
        let key = RiverKey::resolve(profile, self.identity, arm);
        keys.insert(memo, key.clone());
        key
    }
    /// Whether this registry issued `key`.
    ///
    /// It used to ask a different question - whether `key` equals the key this
    /// registry resolves for its symbol - and that question only had one
    /// answer, the clean one. Once a label names several rivers, re-deriving
    /// can produce at most the armless key, so a surged key would be refused as
    /// a mismatch and deleting the check outright would admit a key nothing
    /// issued. Asking whether the key was issued here is the question that
    /// survives the fork: a caller selects among rivers this registry minted
    /// rather than describing one it must then be trusted about.
    fn issued(&self, key: &RiverKey) -> bool {
        self.keys
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(Arc::clone(&key.symbol), key.arm))
            .is_some_and(|issued| issued == key)
    }
    fn river(&self, key: &RiverKey) -> Result<Arc<River>, MaterializeRefusal> {
        let profile = self.resolve_profile(key.symbol())?;
        if !self.issued(key) {
            return Err(MaterializeRefusal::KeyMismatch {
                symbol: key.symbol().to_owned(),
            });
        }
        let symbol = key.symbol();
        // Derived inside the closure, not ahead of the lock: `river` is called
        // once per live tick and the seed is read once per river lifetime.
        let mut materialized: Option<u64> = None;
        let mut rivers = self
            .rivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Creation under the registry mutex makes concurrent first readers
        // share exactly one chain.
        if let Some(river) = rivers.get(key) {
            return Ok(Arc::clone(river));
        }
        if rivers.len() >= MAX_MATERIALIZED_RIVERS {
            return Err(MaterializeRefusal::CapacityExhausted {
                cap: MAX_MATERIALIZED_RIVERS,
                count: rivers.len(),
            });
        }
        let river = Arc::clone(rivers.entry(key.clone()).or_insert_with(|| {
            materialized = Some(self.identity.seeds.tape_for(symbol));
            Arc::new(River {
                checkpoints: Mutex::new(CheckpointIndex::new(
                    generator(symbol, &profile, self.identity, key.arm, self.run_origin_ns),
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
            tracing::info!(
                symbol,
                tape_seed,
                bundle = key.bundle_digest(),
                "river materialized"
            );
        }
        Ok(river)
    }
    #[cfg(test)]
    pub(crate) fn river_handle_for_test(&self, symbol: &str) -> Option<Arc<River>> {
        let profile = self.resolve_profile(symbol).ok()?;
        self.river(&self.resolve_key(&profile, None)).ok()
    }

    /// The symbols whose water actually exists right now. A control that means
    /// "everything" iterates these rather than the configured set, so clearing
    /// never materializes a chain nothing has asked for.
    /// Deduplicated, because it answers a question about symbols while iterating
    /// rivers. One label resolves to one river today so the two counts agree,
    /// and this looks redundant; the moment generator havoc enters river
    /// identity a label names several rivers and an undeduplicated list would
    /// report the same symbol once per fork - to `/instruments`, to the funding
    /// index check, to anything else that reads it as a set.
    pub(crate) fn materialized_symbols(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self
            .rivers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keys()
            .map(|key| key.symbol().to_owned())
            .collect();
        symbols.sort();
        symbols.dedup();
        symbols
    }

    /// What `/instruments` answers: the configured shapes unioned with every
    /// shape this run has materialized a river for, sorted by symbol.
    ///
    /// Materialized, not merely resolved. Resolution is total, so a memo-shaped
    /// list would advertise labels nothing had registered; a river is spent by
    /// a socket bind AND by a history poll alike, so the list grows exactly
    /// when the capped resource does.
    pub(crate) fn instrument_defs(&self) -> Vec<InstrumentDef> {
        let mut defs = self.profiles.instrument_defs();
        let advertised: std::collections::HashSet<String> =
            defs.iter().map(|def| def.symbol.to_string()).collect();
        for symbol in self.materialized_symbols() {
            if !advertised.contains(&symbol)
                && let Ok(profile) = self.resolve_profile(&symbol)
            {
                defs.push(profile.def.clone());
            }
        }
        defs.sort_by(|a, b| a.symbol.cmp(&b.symbol));
        defs
    }

    /// A source positioned at `start`, resumed from the run's checkpoint chain
    /// rather than re-walked from the origin. `None` means the tape origin, which
    /// is checkpoint zero and therefore free.
    ///
    /// The `MergeSource` around a one-element vector is not a leftover from the
    /// multi-symbol era and must not be "simplified" away. `TickSource::seek_to`
    /// consumes the tick it returns, so something has to hold that first in-window
    /// tick until the stream is read; `MergeSource`'s per-source head buffer is what
    /// does it. Unwrapping this to the bare source silently drops the tick at
    /// exactly `start` - a one-tick-late history window that no type would catch.
    /// Replacing it means writing an equivalent one-tick pushback, which is the same
    /// object with a narrower name.
    ///
    /// That `start` is inclusive is relied on rather than incidental: the fill
    /// sweeper passes `from_ns + 1` precisely to get a window that excludes the
    /// instant it already processed, which only works if the tick at the requested
    /// instant is emitted.
    /// Takes a key, never a label, like every other water read on this registry.
    /// See `Rivers::key_for_symbol` for why the two are not interchangeable and
    /// where the remaining label-to-key resolutions live.
    pub(crate) fn history_source(
        &self,
        key: &RiverKey,
        start: Option<u64>,
    ) -> Result<Box<dyn TickSource>, MaterializeRefusal> {
        let target = start.unwrap_or(TAPE_ORIGIN_NS);
        let river = self.river(key)?;
        reach_river(&river, target).map_err(MaterializeRefusal::Reach)?;
        let Some(positioned) = locked(&river.checkpoints).try_source_before(target) else {
            return Err(MaterializeRefusal::Reach(anyhow::anyhow!(
                "river {} cannot reach {target}",
                key.symbol()
            )));
        };
        Ok(Box::new(MergeSource::starting_at(
            vec![Box::new(positioned)],
            start,
        )))
    }

    /// Materialize the unarmed river for `symbol` and answer nothing but
    /// whether it could be. The pre-check the operator history routes run so a
    /// cap exhaustion or a barred shape is a 400 naming its reason rather than
    /// a 500 raised out of the synthesis task - and so a poll advertises the
    /// shape it just spent a river on, which is the same event.
    ///
    /// Unarmed, explicitly, and that is the whole of what the fork changed
    /// here. This is the operator's view of a label, so it names the river a
    /// passenger carrying no generator arm would board. A passenger that
    /// carries one is on other water and reads it over its own socket.
    pub(crate) fn materialize(&self, symbol: &str) -> Result<(), MaterializeRefusal> {
        let key = self.key_for_symbol(symbol)?;
        self.river(&key)?;
        Ok(())
    }

    /// The only way a label becomes water, and it is deliberately awkward to
    /// reach.
    ///
    /// A symbol is instrument identity and not market-realization identity. It
    /// selects a price increment, a calendar, a settlement currency, a wire
    /// field - and it must not select a river, because the end state has
    /// generator havoc inside river identity, so one label will name several
    /// rivers: a passenger carrying an arm boards a different river than one
    /// without it. Every water read therefore takes a `RiverKey`, and a
    /// passenger always has one, because it holds a boat.
    ///
    /// What this protects against is a fork that lands halfway. If pricing,
    /// trigger scans, marks or settlement could still reach water with only a
    /// label, adding the arm to the key would fork the delivered tape while
    /// execution went on reading whichever river the label resolved to - the
    /// passenger would watch surged prints and be filled against water it never
    /// saw. There is now no function that will hand out water for a label
    /// except this one.
    ///
    /// The callers that legitimately have no key, each a named boundary rather
    /// than a convenience. Each of them now names the unarmed river of that
    /// label, which is a choice rather than the only possibility it once was:
    ///
    /// - `/trades` and `/quotes`, which name a symbol and no passenger. They
    ///   are the operator's view of a label, not a consumer route: a passenger
    ///   reads its own water over its own socket, where the connection already
    ///   names the river and cannot name it wrong.
    /// - The perpetual funding index, which names an index symbol its boat does
    ///   not ride and therefore has no key for. The index is a property of the
    ///   instrument rather than of any one passenger's realization, so the
    ///   unarmed river is the answer rather than a fallback.
    ///
    /// The generator control plane used to be the third. It is gone: a
    /// generator arm rides the upgrade and enters the key, so there is no
    /// longer a route that reaches an existing chain in order to change it.
    pub(crate) fn key_for_symbol(&self, symbol: &str) -> Result<RiverKey, MaterializeRefusal> {
        let profile = self.resolve_profile(symbol)?;
        Ok(self.resolve_key(&profile, None))
    }

    /// The refusal stays typed rather than flattening into an `anyhow::Error`,
    /// and that is load-bearing rather than tidiness. The caller has to tell a
    /// river cap - reachable, intentional, and a bad request - from a validated
    /// shape whose generator could not produce it, which is a venue fault. Both
    /// arrived here as one opaque error, so every placement failure was reported
    /// to the consumer as its own mistake.
    pub(crate) fn place_cursor(
        &self,
        key: &RiverKey,
        origin_ns: u64,
    ) -> Result<Box<dyn TickSource + Send>, MaterializeRefusal> {
        let river = self.river(key)?;
        reach_river(&river, origin_ns).map_err(MaterializeRefusal::Reach)?;
        let mut positioned = locked(&river.checkpoints)
            .try_source_before(origin_ns)
            .ok_or_else(|| {
                MaterializeRefusal::Reach(anyhow::anyhow!(
                    "river {} cannot reach {origin_ns}",
                    key.symbol()
                ))
            })?;
        let first = positioned.seek_to(origin_ns);
        Ok(Box::new(BoatCursor {
            first,
            source: positioned,
        }))
    }

    /// Generate and hold every instant up to `target_ns` on this river, so that
    /// each is reachable by a resume plus a bounded residual replay rather than
    /// refused or silently served short. Returns the number of snapshots
    /// retained, which is what the boot log reports for the warmup call.
    ///
    /// First-touch cost is real. `serve.rs` pays this once at boot for the boot
    /// river; every other river is cold until something reads it, and that first
    /// distant history request pays the whole walk from `TAPE_ORIGIN_NS`
    /// synchronously, holding that river's mutex, and allocates up to
    /// `MAX_CHECKPOINTS` generator clones for it. Later requests on the same
    /// river pay only their new monotonic delta. Nothing pre-warms a non-boot
    /// river: doing so would multiply time-to-readiness by the number of
    /// configured shapes for a capability most runs never touch.
    pub(crate) fn ensure_reach(
        &self,
        key: &RiverKey,
        target_ns: u64,
    ) -> Result<usize, MaterializeRefusal> {
        let river = self.river(key)?;
        reach_river(&river, target_ns).map_err(MaterializeRefusal::Reach)
    }

    /// A test's label-to-key resolution, named so it cannot be mistaken for the
    /// production boundaries.
    ///
    /// Tests build a fresh registry inline and then ask about a symbol, which is
    /// the one place a by-label call is harmless: there is exactly one river per
    /// label in a fixture and no passenger to be wrong about. Production water
    /// reads take a key, and `key_for_symbol` names the few boundaries that may
    /// still resolve one.
    #[cfg(test)]
    pub(crate) fn test_key(&self, symbol: &str) -> RiverKey {
        self.key_for_symbol(symbol)
            .expect("a fixture symbol resolves to a river")
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
            let extended = guard.extend_toward(target_ns);
            (extended, guard.frontier_ns(), guard.checkpoint_count())
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
/// The walk-back is not an optimization to remove. A positioned source resumes
/// after the tick its checkpoint last consumed, so the residual covers only
/// `(checkpoint clock, ts]`. When the last print at or before `ts` fell on or
/// before that clock - `ts` sitting between a parent's final trade and the next
/// parent is the easy case - the residual contains no trade at all and the
/// honest answer is not `None`. Resuming progressively earlier snapshots
/// recovers it, doubling the step so the retry count is logarithmic in the
/// chain rather than one walk per checkpoint. Callers treat `None` as "the tape
/// could not be read", and the sweeper's settlement frontier refuses to retire
/// a span on it, so answering `None` where a print exists would stall the
/// frontier forever rather than merely losing one reading.
///
/// The retreat ends at the origin and needs no recovery there. It used to be
/// fenced at a `FlowSurge` control boundary, because resuming earlier and
/// replaying across a mid-walk arm would answer from a different tape, and the
/// fence then needed a way to read a print the boundary snapshot had already
/// consumed. A surge belongs to the river now, so no snapshot fences anything
/// and the origin is a complete answer rather than a wall: it has consumed
/// nothing, so its residual is the whole river and any print at or before `ts`
/// is in it.
impl Rivers {
    /// Takes a key rather than a label, for the reason `key_for_symbol` states:
    /// a settlement price is water, and water is never selected by name.
    pub(crate) fn last_trade_at_or_before(
        &self,
        key: &RiverKey,
        ts: u64,
    ) -> anyhow::Result<Option<Decimal>> {
        self.last_trade_at_or_before_with_budget(key, ts, crate::fills::SWEEP_DRAIN_BUDGET)
    }

    fn last_trade_at_or_before_with_budget(
        &self,
        key: &RiverKey,
        ts: u64,
        mut budget: usize,
    ) -> anyhow::Result<Option<Decimal>> {
        let river = self.river(key).map_err(anyhow::Error::new)?;
        reach_river(&river, ts)?;
        let mut back = 0usize;
        loop {
            let Some((mut source, exhausted)) =
                locked(&river.checkpoints).try_source_before_target(ts, back)
            else {
                anyhow::bail!("river {} cannot position at or before {ts}", key.symbol());
            };
            let mut last = None;
            let mut drained = 0usize;
            while drained < budget {
                let Some(tick) = source.next_tick() else {
                    break;
                };
                drained += 1;
                let TickEvent::Trade(trade) = tick else {
                    continue;
                };
                if trade.ts_event > ts {
                    break;
                }
                last = Some(trade.price);
            }
            if last.is_some() {
                return Ok(last);
            }
            if drained == budget {
                anyhow::bail!(
                    "last-trade walk for {} exhausted its drain budget before finding a print at or before {ts}",
                    key.symbol()
                );
            }
            if exhausted {
                return Ok(None);
            }
            budget = budget.saturating_sub(drained);
            if budget == 0 {
                anyhow::bail!(
                    "last-trade walk for {} exhausted its drain budget before finding a print at or before {ts}",
                    key.symbol()
                );
            }
            back = back.saturating_mul(2).max(1);
        }
    }
}

#[cfg(test)]
mod river_tests {
    use super::*;

    fn total_rivers() -> Arc<Rivers> {
        let profiles = Arc::new(
            crate::config::build_instrument_profiles(&crate::config::Config::default()).unwrap(),
        );
        Rivers::new(
            TapeIdentity {
                seeds: RunSeeds::from_run_seed(7),
                regime: None,
            },
            TAPE_ORIGIN_NS,
            profiles,
        )
    }

    /// The widened `RiverKey`. A symbol nobody configured keys a river of its
    /// own - distinct from the default preset's, whose bundle it borrows, and
    /// distinct from a second unconfigured label sharing that same bundle. That
    /// falls out of the profile being resolved under the requested label, which
    /// is why the label is never normalized on the way in.
    #[test]
    fn an_unconfigured_symbol_keys_its_own_river() {
        let rivers = total_rivers();
        let default = rivers.resolve_key(
            &rivers
                .resolve_profile(crate::config::DEFAULT_PRESET)
                .unwrap(),
            None,
        );
        let foobar = rivers.resolve_key(&rivers.resolve_profile("FOOBAR").unwrap(), None);
        let other = rivers.resolve_key(&rivers.resolve_profile("BARFOO").unwrap(), None);
        assert_ne!(foobar, default);
        assert_ne!(foobar, other);
        // And exact case: `foobar` is a different label, so a different river.
        let miscased = rivers.resolve_key(&rivers.resolve_profile("foobar").unwrap(), None);
        assert_ne!(foobar, miscased);
    }

    /// The fork itself, at the level the key decides it: one label, one
    /// profile, two arms, three distinct rivers.
    ///
    /// The clean key and an armed key must differ, or a passenger carrying an
    /// arm would board the water someone else is already reading and the whole
    /// point of putting the arm in identity is lost. Two different arms must
    /// differ from each other too - otherwise a surged passenger and a
    /// differently-surged one would share, which is the same defect wearing a
    /// disguise.
    #[test]
    fn a_generator_arm_forks_the_river_under_one_label() {
        let rivers = total_rivers();
        let profile = rivers
            .resolve_profile(crate::config::DEFAULT_PRESET)
            .unwrap();
        let clean = rivers.resolve_key(&profile, None);
        let surged = rivers.resolve_key(
            &profile,
            GeneratorArm::normalize(0, 60_000, 4.0, 2.0).unwrap(),
        );
        let harder = rivers.resolve_key(
            &profile,
            GeneratorArm::normalize(0, 60_000, 8.0, 2.0).unwrap(),
        );
        let later = rivers.resolve_key(
            &profile,
            GeneratorArm::normalize(30_000_000_000, 60_000, 4.0, 2.0).unwrap(),
        );
        assert_ne!(clean, surged, "an arm names other water than none");
        assert_ne!(surged, harder, "a different multiplier is different water");
        assert_ne!(later, surged, "a different coordinate is different water");
        assert_eq!(
            clean.symbol(),
            surged.symbol(),
            "the fork is under one label, which is what makes it a fork"
        );

        // The sharing half, and the one that keeps the river cap honest: the
        // same arm asked for twice is one river, not two.
        assert_eq!(
            surged,
            rivers.resolve_key(
                &profile,
                GeneratorArm::normalize(0, 60_000, 4.0, 2.0).unwrap()
            ),
            "two passengers naming one window share one river"
        );
    }

    /// A key this registry never issued is refused rather than served.
    ///
    /// The check used to re-derive the clean key and compare, which after the
    /// fork would refuse every armed key - the armless derivation is the only
    /// one it could produce. Asking whether the registry issued the key is the
    /// question that survives, and this pins both directions: an issued armed
    /// key resolves, a fabricated one does not.
    #[test]
    fn only_a_key_this_registry_issued_resolves_to_water() {
        let rivers = total_rivers();
        let profile = rivers
            .resolve_profile(crate::config::DEFAULT_PRESET)
            .unwrap();
        let surged = rivers.resolve_key(
            &profile,
            GeneratorArm::normalize(0, 60_000, 4.0, 2.0).unwrap(),
        );
        assert!(
            rivers.river(&surged).is_ok(),
            "an armed key the registry minted names real water"
        );
        assert!(
            matches!(
                rivers.river(&surged.with_unresolvable_bundle()),
                Err(MaterializeRefusal::KeyMismatch { .. })
            ),
            "a key nothing issued is refused rather than silently resolved"
        );
    }

    #[test]
    fn a_last_trade_read_reports_an_unissued_key() {
        let rivers = total_rivers();
        let key = rivers.test_key("BTCUSDT").with_unresolvable_bundle();
        let error = rivers
            .last_trade_at_or_before(&key, TAPE_ORIGIN_NS)
            .expect_err("an internal routing refusal is not a quiet tape");
        assert!(
            error.to_string().contains("not the key this run resolved"),
            "the materialization classification survives to the caller: {error:#}"
        );
    }

    #[test]
    fn a_last_trade_read_reports_a_budget_refusal_but_keeps_a_found_print() {
        let rivers = crate::fills::test_rivers();
        let key = rivers.test_key("BTCUSDT");
        let mut source = rivers.history_source(&key, None).unwrap();
        let first_trade = loop {
            match source.next_tick().expect("the fixture prints") {
                TickEvent::Trade(trade) => break trade,
                TickEvent::Quote(_) => {}
            }
        };

        let error = rivers
            .last_trade_at_or_before_with_budget(&key, first_trade.ts_event, 1)
            .expect_err("the opening quote spends a one-tick budget before the trade");
        assert!(error.to_string().contains("drain budget"));

        assert_eq!(
            rivers
                .last_trade_at_or_before_with_budget(&key, first_trade.ts_event, 2)
                .expect("the trade fits exactly inside the budget"),
            Some(first_trade.price),
            "a walk that found its print on the final admitted tick keeps it"
        );
    }

    /// Which refusals are the venue's own, which is what decides whether a
    /// refused placement latches the run terminal and what status the consumer
    /// is handed.
    ///
    /// Both directions are asserted, because both are dangerous. A river cap
    /// that latched would make ordinary capacity admission a kill switch for
    /// every account on a shared exchange; a synthesis failure that did not
    /// latch leaves the venue announcing itself healthy while refusing every
    /// upgrade and telling each consumer its request was bad.
    #[test]
    fn only_the_venue_owned_refusals_are_venue_faults() {
        assert!(
            MaterializeRefusal::Reach(anyhow::anyhow!("could not walk the river")).is_venue_fault(),
            "a validated shape whose generator could not produce it is the venue's own failure"
        );
        assert!(
            MaterializeRefusal::KeyMismatch {
                symbol: "BTCUSDT".to_owned(),
            }
            .is_venue_fault(),
            "an internal routing mismatch is not something a caller can produce or change"
        );
        assert!(
            !MaterializeRefusal::CapacityExhausted { cap: 4, count: 4 }.is_venue_fault(),
            "the river cap is deliberate resource admission, so latching it would make ordinary \
             capacity a venue kill switch"
        );
    }

    #[test]
    fn resolving_the_same_symbol_twice_returns_one_river() {
        let rivers = total_rivers();
        let first = rivers.river_handle_for_test("FOOBAR").expect("first river");
        let second = rivers
            .river_handle_for_test("FOOBAR")
            .expect("second river");
        assert!(Arc::ptr_eq(&first, &second), "one chain per label");
        assert_eq!(rivers.materialized_symbols().len(), 1);
    }

    /// The advertised set is the materialized set (adjudication ruling 3): a
    /// resolve that spent no river must not advertise, and a history poll must.
    #[test]
    fn only_materialized_shapes_are_advertised() {
        let rivers = total_rivers();
        rivers.resolve_profile("FOOBAR").expect("resolves");
        assert!(
            rivers
                .instrument_defs()
                .iter()
                .all(|def| def.symbol.as_ref() != "FOOBAR"),
            "a memo hit that materialized nothing must not advertise"
        );
        rivers
            .history_source(&rivers.test_key("FOOBAR"), None)
            .expect("history poll");
        assert!(
            rivers
                .instrument_defs()
                .iter()
                .any(|def| def.symbol.as_ref() == "FOOBAR"),
            "a poll materializes, and the materialized set is what is advertised"
        );
    }

    /// The cap is a bound, not merely an error: concurrent racers at the
    /// boundary must not push the river map past it between the check and the
    /// insert.
    #[test]
    fn concurrent_materialization_at_the_cap_boundary_holds_the_bound() {
        let rivers = total_rivers();
        for index in 0..MAX_MATERIALIZED_RIVERS - 1 {
            rivers.materialize(&format!("S{index}")).unwrap();
        }
        let threads: Vec<_> = (0..8)
            .map(|index| {
                let rivers = Arc::clone(&rivers);
                std::thread::spawn(move || rivers.materialize(&format!("RACER{index}")))
            })
            .collect();
        let outcomes: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_ok()).count(),
            1,
            "exactly one racer may take the last slot"
        );
        assert_eq!(rivers.materialized_symbols().len(), MAX_MATERIALIZED_RIVERS);
    }

    #[test]
    fn resolve_is_total_over_wire_legal_symbols_and_memoizes() {
        let rivers = total_rivers();
        let first = rivers.resolve_profile("FOOBAR").unwrap();
        let second = rivers.resolve_profile("FOOBAR").unwrap();
        assert_eq!(first.def.symbol.as_ref(), "FOOBAR");
        assert_eq!(first.def.class.settlement_currency(), "USDT");
        assert!(Arc::ptr_eq(&first, &second));
        assert!(rivers.resolve_profile("not legal!").is_err());
    }

    #[test]
    fn concurrent_resolves_of_one_symbol_share_a_profile() {
        let rivers = total_rivers();
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let rivers = Arc::clone(&rivers);
                std::thread::spawn(move || rivers.resolve_profile("FOOBAR").unwrap())
            })
            .collect();
        let profiles: Vec<_> = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect();
        assert!(
            profiles
                .iter()
                .all(|profile| Arc::ptr_eq(&profiles[0], profile))
        );
    }

    #[test]
    fn materialization_refuses_past_the_cap_without_exceeding_it() {
        let rivers = total_rivers();
        for index in 0..MAX_MATERIALIZED_RIVERS {
            rivers
                .history_source(&rivers.test_key(&format!("S{index}")), None)
                .unwrap();
        }
        let error = match rivers.history_source(&rivers.test_key("OVER_CAP"), None) {
            Ok(_) => panic!("the 257th river materialized"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            MaterializeRefusal::CapacityExhausted {
                cap: MAX_MATERIALIZED_RIVERS,
                count: MAX_MATERIALIZED_RIVERS
            }
        ));
        assert_eq!(rivers.materialized_symbols().len(), MAX_MATERIALIZED_RIVERS);
    }

    #[test]
    fn concurrent_first_readers_share_one_river() {
        let rivers = crate::fills::test_rivers();
        // Collect the spawns before joining any of them: chaining `.map(spawn)`
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
    fn a_boat_placed_after_a_history_read_still_starts_at_the_river_origin() {
        let rivers = crate::fills::test_rivers();
        rivers
            .ensure_reach(&rivers.test_key("BTCUSDT"), 3_600_000_000_000)
            .unwrap();
        let profile = rivers.resolve_profile("BTCUSDT").unwrap();
        let key = rivers.resolve_key(&profile, None);
        let mut cursor = rivers.place_cursor(&key, TAPE_ORIGIN_NS).unwrap();
        let mut history = rivers
            .history_source(&rivers.test_key("BTCUSDT"), Some(TAPE_ORIGIN_NS))
            .unwrap();
        assert_eq!(
            cursor.next_tick().unwrap().ts_event(),
            history.next_tick().unwrap().ts_event()
        );
    }

    #[test]
    fn a_wound_down_boat_leaves_its_river_extendable() {
        let rivers = crate::fills::test_rivers();
        let profile = rivers.resolve_profile("BTCUSDT").unwrap();
        let key = rivers.resolve_key(&profile, None);
        drop(rivers.place_cursor(&key, TAPE_ORIGIN_NS).unwrap());
        let before = locked(&rivers.river(&key).unwrap().checkpoints).frontier_ns();
        rivers
            .ensure_reach(
                &rivers.test_key("BTCUSDT"),
                before.saturating_add(60_000_000_000),
            )
            .unwrap();
        assert!(
            locked(&rivers.river(&key).unwrap().checkpoints).frontier_ns()
                >= before.saturating_add(60_000_000_000)
        );
    }

    #[test]
    fn a_placed_cursor_yields_every_row_at_the_origin_instant() {
        let rivers = crate::fills::test_rivers();
        let profile = rivers.resolve_profile("BTCUSDT").unwrap();
        let key = rivers.resolve_key(&profile, None);
        let mut expected = rivers
            .history_source(&rivers.test_key("BTCUSDT"), None)
            .unwrap();
        let first = expected.next_tick().unwrap();
        let instant = first.ts_event();
        let mut count = 1;
        while expected
            .next_tick()
            .is_some_and(|tick| tick.ts_event() == instant)
        {
            count += 1;
        }
        let mut cursor = rivers.place_cursor(&key, TAPE_ORIGIN_NS).unwrap();
        let mut actual = 0;
        while cursor
            .next_tick()
            .is_some_and(|tick| tick.ts_event() == instant)
        {
            actual += 1;
        }
        assert_eq!(actual, count);
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
            .ensure_reach(&rivers.test_key("BTCUSDT"), 3_600_000_000_000)
            .expect("a cold river materializes on demand");
        assert!(reached > 0, "the walk retained checkpoints");
        rivers
            .history_source(&rivers.test_key("BTCUSDT"), Some(3_600_000_000_000))
            .expect("cold history");
    }

    /// A second configured shape is realized as its own river, under its own
    /// def and chain, locked independently of the boot river's.
    #[test]
    fn a_second_river_is_realized_under_its_own_def_and_chain() {
        let rivers = crate::fills::test_rivers_with_a_second_symbol();
        let boot = rivers.river_handle_for_test("BTCUSDT").unwrap();
        let second = rivers.river_handle_for_test("SECOND").unwrap();
        assert!(
            !Arc::ptr_eq(&boot, &second),
            "one chain per configured shape"
        );
        let mut boot_source = rivers
            .history_source(&rivers.test_key("BTCUSDT"), Some(TAPE_ORIGIN_NS))
            .expect("boot river history");
        let boot_prints: Vec<_> = std::iter::from_fn(|| boot_source.next_tick())
            .filter_map(|tick| match tick {
                TickEvent::Trade(trade) => Some((trade.ts_event, trade.price)),
                _ => None,
            })
            .take(32)
            .collect();
        assert_eq!(boot_prints.len(), 32, "the boot river prints");
        let mut second_source = rivers
            .history_source(&rivers.test_key("SECOND"), Some(TAPE_ORIGIN_NS))
            .expect("second river history");
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
            "symbol labels select distinct rivers"
        );
        // Materializing the second river leaves the boot river where it was:
        // the chains advance independently, which is the whole point of keying
        // them.
        rivers
            .ensure_reach(&rivers.test_key("SECOND"), 600_000_000_000)
            .expect("second river materializes");
        assert!(locked(&second.checkpoints).frontier_ns() >= 600_000_000_000);
        assert_eq!(locked(&boot.checkpoints).frontier_ns(), TAPE_ORIGIN_NS);
    }

    /// A blown reach ceiling is an error, never an empty window: swallowing it
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
