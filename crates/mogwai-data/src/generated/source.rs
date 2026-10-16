// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The core path-dependent walk: [`GeneratedSource`] composes the arrival
//! duration clock, the GARCH latent mid, the bounce/drift price process and
//! the optional regime overlay into the [`crate::RiverSource`] the running
//! venue drives. Same seed plus tape anchor yields the same stream byte for
//! byte - see `clean_regime_is_byte_identical` in the test module for the
//! pinned golden sequence that must be re-blessed with any intentional walk
//! mechanism change.
//!
//! The walk resolves one whole parent transaction at a time into a reused
//! plan of compact records, and readers drain that plan. A parent's footprint
//! is therefore known the moment it is drawn: a seek drops a parent that ends
//! before its target without cloning the generator to try it, and a checkpoint
//! extension advances whole parents only. Emitting and skipping a parent are
//! one code path, because materializing a compact record is a handful of
//! integer operations.
//!
//! One consequence of the determinism is worth stating before it is
//! rediscovered as a bug: a drought is per-config, not intermittent. Every boot
//! of a pinned epoch lands on the same stretch of tape, so a config whose epoch
//! sits in a low-arrival stretch reproduces that stretch on every boot, for as
//! long as the config is unchanged. A report that a freshly started server
//! correlates with empty history windows is this, not a startup or warm-up
//! defect.
//!
//! The fitted ACD arrival process is persistent and heavy-tailed, so a short
//! historical window legitimately holds zero trades. The history routes are
//! correct to answer an empty list there, and widening the window or seeding
//! filler to avoid the empty answer would be falsifying the river to flatter a
//! route.

use std::sync::Arc;

use mogwai_protocol::{AggressorSide, InstrumentDef, MarketRegime, decimal_to_f64};
use rand::distr::{Bernoulli, OpenClosed01};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{ChiSquared, Distribution, Normal, Weibull};
use rust_decimal::Decimal;

use crate::record::{
    ParentSource, ParentView, QuoteRec, Record, RecordScale, RiverSource, TradeRec,
    UnsizedParentView,
};
use crate::{TickEvent, TickFault};

use super::arrival::{
    ArrivalEnv, ArrivalKernel, ArrivalState, CADENCE_STEP_NS, CadenceWalk, PendingReopen,
    RuntimeModifiers,
};
use super::book::{BookSnapshot, DiscreteBook, Struck};
use super::calendar::{CompiledCalendar, SessionCalendar};
use super::cascade::{
    CascadeRefusal, CascadeState, CascadeTables, MAX_EVENT_LOG_MOVE, SecondModifiers,
};
use super::children::{LevelSteps, SizeStream};
use super::consts::{
    ARRIVAL_ACTIVE_CHILDREN_MULT, ARRIVAL_MEAN_CAL, ARRIVAL_QUIET_ACTIVE_RATIO,
    ARRIVAL_QUIET_CHILDREN_MULT, ARRIVAL_QUIET_FRACTION, ARRIVAL_STATE_PERSISTENCE,
    ARRIVAL_WEIBULL_MEAN, ARRIVAL_WEIBULL_SHAPE, CHILD_CAP, DRIFT_RECENTER_FRAC,
    EVENT_PRICE_REPEAT_PROB, HIGH_REGIME_LEVEL_STEP_MULT, INTRA_EVENT_STEP_NS,
    LOW_INTENSITY_ARR_MULT, LOW_REGIME_LEVEL_STEP_MULT, MAX_SESSION_GAP_NS, MID_CEILING,
    NS_PER_HOUR, REALIZED_RETURN_CEILING, SIZE_DECIMALS, STUDENT_T_DF, STUDENT_T_UNIT_SCALE,
};
use super::dynamics::{
    ArrivalClock, BounceLaws, BounceState, GarchVol, SweepBurst, SweepShape, draw_student_t,
};
use super::fingerprint::{
    Fingerprint, GeneratedSourceError, GeneratorScalars, ScalarError, SessionProfile,
};
use super::numeric::{PriceGrid, SIZE_UNITS_CEILING, SizeQuantizer, lot_exponent};
use super::quote::{PublishedBook, book_mid_ticks, place_book};
use super::regime::RegimeState;
use super::session::SessionModulator;

/// The most records one parent can emit, on any path: a placed parent is one
/// quote and at most `CHILD_CAP` children, and a discrete-book parent at most
/// 65 trades (64 validated ladder ratios plus the touch, one split) and two
/// quotes. The resolver asserts it, because the checkpoint extension's
/// overshoot bound and the plan buffer's high-water mark are both proved from
/// it.
pub const MAX_PARENT_RECORDS: usize = CHILD_CAP as usize + 2;

/// Everything about a river that is fixed once it is built: the config, the
/// record scale, the compiled calendar and session tables, the cascade tables
/// and the integrated environment, the samplers and the size grid. Held in
/// one `Arc`, so the lead and every checkpoint share it and a snapshot copies
/// only the walk's own state.
#[derive(Clone)]
struct RiverSpec {
    scalars: GeneratorScalars,
    scale: RecordScale,
    calendar: Option<Arc<CompiledCalendar>>,
    session: SessionModulator,
    // The activity cascade's tables (tape protocol 32), present on a
    // `Cascade` or `CascadeBook` walk.
    cascade: Option<CascadeTables>,
    // The integrated kernel and its environment, present on an `Integrated`
    // walk.
    arrival_kernel: Option<ArrivalKernel>,
    arrival_env: Option<ArrivalEnv>,
    /// The trade displacement in ticks, from the resolved
    /// `trade_displacement_ticks` seam, for which
    /// `TRADE_BOUNCE_HALF_WIDTH_TICKS` is only the default - so a preset may
    /// vary it per instrument. Fixed for the river's life, so the
    /// displacement is identical in every regime for one instrument; a
    /// state-dependent displacement would move it into the walk state. See
    /// the constant for why this is not a spread.
    trade_bounce_ticks: f64,
    duration_dist: Weibull<f64>,
    normal: Normal<f64>,
    chi_squared: ChiSquared<f64>,
    prices: PriceGrid,
    sizes: SizeQuantizer,
    /// The configured top-of-book sizes and the size increment as quantity
    /// mantissas at the river's scale, converted exactly once at build.
    top_bid_units: u64,
    top_ask_units: u64,
    min_units: u64,
    coins: ChildCoins,
    tick_f64: f64,
    surge: Option<SurgeWindow>,
}

/// The walk's mode and the mutable state only that mode carries. Which mode a
/// river walks is fixed by its config - the cascade, then an arrival kernel,
/// then the fingerprint-fitted legacy clock - so the variant never changes
/// after construction, and parent resolution dispatches on it once per parent.
#[derive(Clone)]
#[expect(
    clippy::large_enum_variant,
    reason = "one walk per source, held inline; boxing would add an allocation to every snapshot clone to save bytes no collection multiplies"
)]
enum Walk {
    Legacy(LegacyWalk),
    Integrated(IntegratedWalk),
    Cascade(CascadeState),
    CascadeBook(BookWalk),
}

/// The fingerprint-fitted walk: the Markov arrival clock, the bounce/drift
/// process, and the prior parent's book and final price the repeat branch
/// re-prices from.
#[derive(Clone)]
struct LegacyWalk {
    arrival: ArrivalClock,
    arrival_override: Option<bool>,
    bounce: BounceState,
    last_event_price_ticks: Option<f64>,
    last_book: Option<PublishedBook>,
}

/// The integrated-kernel walk: the kernel's latent state, its own cadence
/// stream, and the bounce/drift process placing the book. No repeat branch,
/// so no prior book or price.
#[derive(Clone)]
struct IntegratedWalk {
    state: ArrivalState,
    cadence_rng: ChaCha12Rng,
    bounce: BounceState,
}

/// The discrete-book walk (see the `book` module): the cascade owns the clock
/// and the external move, the book everything the touch does. Its rng
/// streams are its own, derived from the seed under domain tags, so book
/// parameters leave the cascade realization untouched.
#[derive(Clone)]
struct BookWalk {
    cascade: CascadeState,
    book: DiscreteBook,
    // The four-value snapshot last published on the wire, the deduplication
    // state: a quote whose four values equal it is suppressed with its
    // instant still reserved.
    last_published: Option<BookSnapshot>,
}

/// `Clone` is the substrate of the checkpointed seek (`CheckpointIndex`): the
/// generator is a path-dependent walk whose entire future is a pure function of
/// its current state, so a clone taken at a parent boundary, advanced,
/// reproduces the records after it byte-for-byte. The immutable half is one
/// shared `RiverSpec`; what a clone copies is the walk state below, including
/// the `ChaCha12Rng` (rand's `StdRng` is the same cipher but dropped `Clone`,
/// which is why the generator holds `ChaCha12Rng` directly).
#[derive(Clone)]
pub struct GeneratedSource {
    spec: Arc<RiverSpec>,
    walk: Walk,
    rng: ChaCha12Rng,
    /// The last instant the walk has reserved: after a parent resolves, the
    /// end of its reserved span - its last child on the placed paths, its
    /// post-trade slot on the discrete-book path. It is not the consumed
    /// position: a resolved parent may still hold records a reader has not
    /// taken. At a parent boundary the two coincide in the sense the
    /// checkpoint partition needs - every consumed record is at or before it.
    pub(super) clock_ns: u64,
    /// Whether any parent has been resolved or attempted. Sticky, so a source
    /// that has drawn can never read as fresh; `with_surge` refuses one.
    resolved_any: bool,
    /// The global index of the next placed child. Advanced at resolution by
    /// every placed parent's child count, on every path - emitted, skipped or
    /// dropped - and never by decoration, so a child's index, and so its
    /// size, does not depend on how the parent was read.
    next_child: u64,
    /// The resolved parent's first child index, and whether its trade
    /// quantities have been filled. A placed plan resolves undecorated; a
    /// discrete-book plan carries its own quantities and resolves decorated.
    plan_first_child: u64,
    plan_decorated: bool,
    /// The size decoration's stream. For the river's fixed size law, a
    /// child's size draw is a function of the seed and its index, so where this stream happens to sit is never
    /// state the tape depends on - `decorate` positions it at each parent's
    /// first child - and keeping it here only saves repositioning it between
    /// consecutive decorated parents.
    size_stream: SizeStream,
    /// Test-only cursor for `next_size`, which samples the river's size law
    /// from indices of its own.
    #[cfg(test)]
    size_probe: u64,
    /// The resolved parent's records, in emission order, and how many a
    /// reader has taken. Reused across parents, so its capacity is the
    /// high-water parent footprint and never more than `MAX_PARENT_RECORDS`.
    /// Cleared once drained, so a snapshot at a boundary copies no records.
    plan: Vec<Record>,
    cursor: usize,
    plan_parent_ts: u64,
    plan_trades: u32,
    fault: Option<TickFault>,
    pub(super) vol: GarchVol,
    // Read only by the test that pins the sampler's median; the walk reads
    // the quantizer derived from it.
    #[cfg(test)]
    pub(super) size_median: f64,
    regime: RegimeState,
    // The upper bound of the last instant span an armed `ReopenGap` was tested
    // against. It is not `clock_ns`: children advance the clock past their
    // parent, so starting the next test at the clock would leave the whole
    // intra-burst span untested, and an arm inside it could never satisfy the
    // crossing test again. Kept as its own field so the tested spans are
    // contiguous by construction rather than by every clock mutation
    // remembering to be included.
    reopen_frontier_ns: u64,
    /// The declared sweep law and the truncation counters the realism gate
    /// reads; each path keeps the accounting it always kept.
    pub(super) shape: SweepShape,
    // The observation-only volatility trace (the generator successor spec
    // 3.3): populated per parent event when enabled, consuming no draws
    // and changing nothing about the walk, which the byte-identity test
    // pins. `take_vol_trace` hands the last event's record to the offline
    // forensic probe, and `next_parent` carries it on the parent view.
    vol_trace_enabled: bool,
    vol_trace: Option<VolTrace>,
    #[cfg(test)]
    pub(super) latent_updates: u64,
    #[cfg(test)]
    pub(super) repeat_draws: u64,
    #[cfg(test)]
    pub(super) rejected_repeat_draws: u64,
    /// Trades the reader has taken from the current parent, for the tests that
    /// ask where a reader stands inside a sweep. The walk's own `burst` is
    /// drained the moment a parent resolves, so it no longer says anything
    /// about the reader.
    #[cfg(test)]
    reader_emitted: u32,
    /// The main-stream stage order of the shipped (contract A) event path,
    /// recorded as the event executes and cleared at the top of every
    /// `begin_event`. One tag per stage, not one per RNG draw: the stochastic
    /// contract fixes which stage runs before which, and the number of
    /// primitive draws a stage makes is a property of the distribution it
    /// samples (`draw_student_t` alone takes a normal and a chi-squared, and
    /// the ziggurat's rejection loop makes even that count variable), so a
    /// per-draw counter could not be pinned. Recording costs nothing in
    /// release: the field does not exist there.
    ///
    /// It is a soft gate and must be read as one. The pushes are hand-placed
    /// statements adjacent to the work, not instrumentation inside the draws,
    /// so what they pin is the order of the pushes. A change that moves a
    /// draw past its own tag - reordering `next_latent_mid` against
    /// `next_side_and_book` while both tags stay put - leaves this trace
    /// unchanged. What it does catch is a reordering of the stages as
    /// written, which is how the contract-A order is actually expressed here,
    /// and that is the whole of the claim V4 and V5 rest on it.
    ///
    /// Only `begin_event` feeds it. `begin_integrated_event` pushes nothing,
    /// so a generator whose scalars carry an `arrival` kernel traces an empty
    /// sequence rather than a wrong one; the reader in `arrival.rs` refuses on
    /// emptiness and says so.
    #[cfg(test)]
    pub(super) draw_stages: Vec<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeGrid {
    pub multiplier: Decimal,
    pub integral: bool,
    pub min_size: Decimal,
}

/// Compact description of one fully advanced stochastic parent event.
///
/// The generator has already consumed every child draw and updated all
/// path-dependent state when this is returned. Consumers that only need tape
/// composition can therefore count the quote and the fixed-stride child run
/// without reading the records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentSummary {
    pub parent_ts_ns: u64,
    pub child_count: u32,
    pub child_stride_ns: u64,
}

/// One parent event's volatility intermediates, observed off the real
/// `GarchVol::step` path - never a reimplementation, and no extra draws.
/// The forensic instrument of the generator successor spec (3.3): the
/// 420.75-point minute's diagnosis needs sigma's path, the clamp hits and
/// the innovation sequence, which no print-level dissection can see.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct VolTrace {
    pub innovation_raw: f64,
    pub innovation_std: f64,
    pub sigma2_candidate: f64,
    pub sigma2_realized: f64,
    pub sigma_cap_hit: bool,
    pub garch_scale: f64,
    pub base_return_unclipped: f64,
    pub base_return: f64,
    pub feedback_clamp_hit: bool,
    pub session_vol_mult: f64,
    pub regime_vol_mult: f64,
    pub pre_realized_return: f64,
    pub realized_return: f64,
    pub realized_clamp_hit: bool,
    pub mid_before: f64,
    pub mid_after: f64,
}

/// The probabilities fixed for a source's life, built once: the legacy
/// walk's price-repeat coin, and the three level-step probabilities a placed
/// sweep selects between, each computed by the expression that used to run
/// per child.
#[derive(Clone, Copy)]
struct ChildCoins {
    price_repeat: Bernoulli,
    level_step_cascade: f64,
    level_step_high: f64,
    level_step_low: f64,
}

impl ChildCoins {
    fn new(level_step_prob: f64) -> Self {
        Self {
            price_repeat: Bernoulli::new(EVENT_PRICE_REPEAT_PROB)
                .expect("probability validated into [0, 1]"),
            level_step_cascade: level_step_prob,
            level_step_high: (level_step_prob * HIGH_REGIME_LEVEL_STEP_MULT).min(1.0),
            level_step_low: level_step_prob * LOW_REGIME_LEVEL_STEP_MULT,
        }
    }
}

impl SizeGrid {
    #[must_use]
    pub fn spot() -> Self {
        Self {
            multiplier: Decimal::ONE,
            integral: false,
            min_size: Decimal::new(1, SIZE_DECIMALS),
        }
    }

    /// The grid an instrument's own sizing states, rather than one inferred
    /// from its class.
    ///
    /// This read `is_future()` and hardcoded whole contracts, which was right
    /// while every derivative was an exchange-listed future. It stopped being
    /// right when perpetuals landed: a crypto perp sizes fractionally - Binance
    /// quotes BTCUSDT.P in thousandths - so a class-derived grid would refuse
    /// the most common perpetual on the largest venue while claiming its
    /// generator config was invalid.
    ///
    /// Deriving from `size_increment` and `size_precision` moves no existing
    /// tape: a future configured with increment 1 and precision 0 lands on
    /// exactly the integral grid it had, and spot with 1e-8 lands on exactly
    /// `spot()`. The tape version is bumped anyway, because the rule is
    /// unconditional and a bump costs one integer.
    #[must_use]
    pub fn from_def(def: &InstrumentDef) -> Self {
        Self {
            multiplier: def.class.multiplier(),
            integral: def.size_precision == 0 && def.size_increment == Decimal::ONE,
            min_size: if def.size_increment > Decimal::ZERO {
                def.size_increment
            } else {
                Decimal::new(1, SIZE_DECIMALS)
            },
        }
    }

    /// The decimal scale this grid's quantities are recorded at: whole units
    /// on an integral grid; on a fractional one the draw precision
    /// (`SIZE_DECIMALS`) or the finest place any configured quantity needs,
    /// whichever is finer, so no configured quantity is rounded on the way in.
    fn record_dp(&self, top_sizes: &super::quote::TopOfBookSizes) -> u32 {
        if self.integral {
            return 0;
        }
        [self.min_size, top_sizes.bid, top_sizes.ask]
            .iter()
            .map(|value| value.normalize().scale())
            .fold(SIZE_DECIMALS, u32::max)
    }
}

#[derive(Clone)]
struct SurgeWindow {
    start_ns: u64,
    end_ns: u64,
    rate_mult: f64,
    children_mult: f64,
}

/// A configured quantity as a mantissa at the river's size scale, exact and
/// under the ceiling, or the named refusal.
fn configured_units(
    scale: &RecordScale,
    value: Decimal,
    field: &'static str,
) -> Result<u64, GeneratedSourceError> {
    scale
        .size_mantissa(value)
        .filter(|units| *units <= SIZE_UNITS_CEILING)
        .ok_or(GeneratedSourceError::Scalar(ScalarError::detailed(
            field,
            "not exactly representable at the record size scale, or above the quantity ceiling",
        )))
}

impl GeneratedSource {
    #[cfg(test)]
    pub(super) fn shares_immutable_config_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.spec, &other.spec)
    }

    /// The trade displacement the river was built with.
    #[cfg(test)]
    pub(super) fn trade_bounce_ticks(&self) -> f64 {
        self.spec.trade_bounce_ticks
    }

    /// The bounce/drift state, on the walks that carry one.
    #[cfg(test)]
    pub(super) fn bounce(&self) -> &BounceState {
        match &self.walk {
            Walk::Legacy(walk) => &walk.bounce,
            Walk::Integrated(walk) => &walk.bounce,
            Walk::Cascade(_) | Walk::CascadeBook(_) => {
                panic!("the cascade walks carry no bounce state")
            }
        }
    }

    /// Install a flow surge on a source that has not drawn yet, consuming and
    /// returning it so it reads as part of construction.
    ///
    /// Taking `self` by value makes it read as construction, and the assert
    /// is what enforces it: a source that has resolved, or even attempted, a
    /// parent - reading one quote resolves its whole parent - is refused.
    /// The window is written through `Arc::make_mut`, so a spec already
    /// shared with a clone is copied, never mutated under that clone. A
    /// surge is part of a river's identity, so it has to be present before
    /// the first draw: a
    /// generator that acquired one part-way through its walk would produce a
    /// sequence no from-origin replay could reproduce, and every checkpoint
    /// taken before the arm would resume onto different water. That was the
    /// old mid-walk mutation, and the pinned control boundary and its
    /// walk-back fence existed only to contain it. Here there is nothing to
    /// contain: every snapshot of this source carries the same immutable
    /// window, so resuming from any of them stays on one realization, across
    /// the surge opening and its expiry alike.
    ///
    /// `start_ns` is a generator timestamp, not a wall instant and not a
    /// per-reader deadline. Two passengers naming the same window get the same
    /// water whenever they board, and a passenger boarding after it closed
    /// gets a river whose surge is in the past - which is what makes the water
    /// exogenous rather than a promise to whoever asked.
    #[must_use]
    pub fn with_surge(
        mut self,
        start_ns: u64,
        duration_ms: u64,
        rate_mult: f64,
        children_mult: f64,
    ) -> Self {
        assert!(
            !self.resolved_any && self.fault.is_none(),
            "with_surge applies to a source that has drawn nothing"
        );
        Arc::make_mut(&mut self.spec).surge = Some(SurgeWindow {
            start_ns,
            end_ns: start_ns.saturating_add(duration_ms.saturating_mul(1_000_000)),
            rate_mult,
            children_mult,
        });
        self
    }

    #[must_use]
    pub fn new(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
    ) -> Self {
        Self::new_with_session_profile(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            SizeGrid::spot(),
            None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the calendar is a construction input rather than a later attachment; see SessionModulator::new"
    )]
    #[must_use]
    pub fn new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
        calendar: Option<SessionCalendar>,
    ) -> Self {
        Self::build(
            scalars, seed, start_ts, fp, session, regime, size_grid, calendar,
        )
    }

    /// Fallible twin of [`GeneratedSource::new`]. Both `scalars` and the
    /// fingerprint's session profile `Deserialize` straight from user config, so
    /// a caller holding un-pre-validated input should route through this and
    /// surface a [`GeneratedSourceError`] rather than let the infallible `new`
    /// turn a config typo into a process panic. `new` is `try_new(..).expect(..)`.
    pub fn try_new(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_build(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            SizeGrid::spot(),
            None,
        )
    }

    pub fn try_new_with_size_grid(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_build(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            size_grid,
            None,
        )
    }

    /// Fallible twin of [`GeneratedSource::new_with_session_profile`] - same
    /// rationale as [`GeneratedSource::try_new`], but for the explicit-session
    /// path where the profile is also untrusted config.
    #[expect(
        clippy::too_many_arguments,
        reason = "the calendar is a construction input rather than a later attachment; see SessionModulator::new"
    )]
    pub fn try_new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
        calendar: Option<SessionCalendar>,
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_build(
            scalars, seed, start_ts, fp, session, regime, size_grid, calendar,
        )
    }

    /// Infallible wrapper: panics if either input is outside the fingerprint
    /// ranges. Callers building from the committed fingerprint (valid by
    /// construction) use this via `new` / `new_with_session_profile`; callers
    /// with untrusted config use the `try_*` twins above instead.
    #[expect(
        clippy::too_many_arguments,
        reason = "the calendar is a construction input rather than a later attachment; see SessionModulator::new"
    )]
    fn build(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
        calendar: Option<SessionCalendar>,
    ) -> Self {
        Self::try_build(
            scalars, seed, start_ts, fp, session, regime, size_grid, calendar,
        )
        .expect("generated source inputs are inside fingerprint ranges")
    }

    // The only fallible inputs are `scalars`/`session`/`calendar` and the
    // record scale they imply, guarded by the `?`-propagated checks at the
    // top. The distribution constructors below (`LogNormal`/`Weibull`/
    // `Normal`/`ChiSquared`) take compile-time constants that are always
    // valid params, so their `expect`s cannot fire - `unwrap_in_result` is
    // silenced here because there is no meaningful error variant to map them
    // onto, not because a failure is being swallowed.
    #[expect(
        clippy::unwrap_in_result,
        reason = "distribution params are constant and valid; only the validated scalars/session can fail"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "the calendar is a construction input rather than a later attachment; see SessionModulator::new"
    )]
    fn try_build(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        _fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
        calendar: Option<SessionCalendar>,
    ) -> Result<Self, GeneratedSourceError> {
        scalars.validate().map_err(GeneratedSourceError::Scalar)?;
        scalars
            .validate_size_grid(size_grid.min_size)
            .map_err(GeneratedSourceError::Scalar)?;
        // The discrete book walks whole units of the size increment: its
        // touch quantities, ladder growth and depletion are integer counts.
        // A fractional grid has no whole unit to count, so the book is
        // refused rather than silently rounding every quantity.
        if scalars.book.is_some() && !size_grid.integral {
            return Err(GeneratedSourceError::Scalar(ScalarError::detailed(
                "book",
                "requires an integral size grid",
            )));
        }
        for size in [scalars.top_sizes.bid, scalars.top_sizes.ask] {
            if size < size_grid.min_size || (size_grid.integral && size.fract() != Decimal::ZERO) {
                return Err(GeneratedSourceError::Scalar(ScalarError::field(
                    "top_sizes",
                )));
            }
        }
        // The record scale. Price: `price_decimals`, which validation bounds
        // so a mantissa at the latent mid's ceiling fits an `i64` with room
        // for every emitted offset; the tick is exact there by validation.
        // Size: see `SizeGrid::record_dp`. Every configured quantity is then
        // converted exactly or refused - never rounded during compilation.
        let scale = RecordScale::new(
            Arc::from(scalars.symbol.as_str()),
            scalars.price_decimals,
            size_grid.record_dp(&scalars.top_sizes),
        )
        .map_err(|error| {
            GeneratedSourceError::Scalar(match error {
                crate::record::ScaleError::Scale { field: "price", .. } => ScalarError::detailed(
                    "price_decimals",
                    "needs more decimal places than a record can carry",
                ),
                _ => ScalarError::detailed(
                    "size_increment",
                    "needs more decimal places than a record can carry",
                ),
            })
        })?;
        let tick_mantissa =
            scale
                .price_mantissa(scalars.modal_tick)
                .ok_or(GeneratedSourceError::Scalar(ScalarError::field(
                    "modal_tick",
                )))?;
        let min_units = configured_units(&scale, size_grid.min_size, "size_increment")?;
        let top_bid_units = configured_units(&scale, scalars.top_sizes.bid, "top_sizes")?;
        let top_ask_units = configured_units(&scale, scalars.top_sizes.ask, "top_sizes")?;
        if let Some(calendar) = &calendar {
            calendar
                .validate()
                .map_err(GeneratedSourceError::Calendar)?;
        }
        // Compiled once, after validation, and shared by the spec, the
        // cascade tables and the integrated environment alike.
        let calendar = calendar.map(|calendar| Arc::new(CompiledCalendar::new(&calendar)));
        let cascade = match (&scalars.cascade, &calendar) {
            (Some(config), Some(calendar)) => Some(
                CascadeTables::new(config, calendar, scalars.mean_event_duration_s)
                    .map_err(GeneratedSourceError::Scalar)?,
            ),
            (Some(_), None) => {
                return Err(GeneratedSourceError::Scalar(ScalarError::detailed(
                    "cascade",
                    "requires a calendar with an activity envelope",
                )));
            }
            (None, _) => None,
        };
        // Contextual, per the calendar-conditional contract. Without a calendar
        // the profile must still be the fraction/per-mean shares the legacy
        // arithmetic assumes, because that arithmetic is what runs. With one,
        // the modulator normalizes over open minutes and any positive scale is
        // equivalent - so requiring a sum would force a fitted profile to
        // satisfy a constraint that no longer means anything, and would make
        // Saturday's unidentifiable placeholder distort the six days that are
        // identifiable.
        session
            .validate_for(calendar.is_some())
            .map_err(GeneratedSourceError::Session)?;
        let mean_event_duration_s = scalars.mean_event_duration_s;
        let (quiet_fraction, quiet_active_ratio, switch_rate) = match scalars.arrival {
            Some(super::arrival::ArrivalConfig::EventMarkov {
                quiet_share,
                switch_rate,
                rate_ratio,
            }) => (quiet_share, rate_ratio, switch_rate),
            _ => (
                ARRIVAL_QUIET_FRACTION,
                ARRIVAL_QUIET_ACTIVE_RATIO,
                1.0 - ARRIVAL_STATE_PERSISTENCE,
            ),
        };
        let calibrated_mean_s = ARRIVAL_MEAN_CAL * mean_event_duration_s;
        let active_mean_s =
            calibrated_mean_s / ((1.0 - quiet_fraction) + quiet_fraction * quiet_active_ratio);
        let vol = GarchVol::new(decimal_to_f64(scalars.start_price), scalars.vol_scalar);
        // The config now names the quantity the sampler actually consumes.
        // The removed `typical_notional` took a circuitous route through price,
        // contract multiplier, and the lognormal mean correction. Besides
        // mixing quote currency with contracts, that made a field called
        // "typical" denote the arithmetic mean of a strongly skewed law.
        let size_median = decimal_to_f64(scalars.latent_size_median).max(f64::MIN_POSITIVE);
        let size_stream = SizeStream::new(
            seed,
            size_median,
            scalars.size_log_sigma,
            scalars.size_round_frac,
        );
        let shape = SweepShape::new(
            scalars.children_mean,
            scalars.children_single_frac,
            scalars.levels_mean,
        );
        // Built before the struct literal because it borrows `scalars.symbol`,
        // which the literal moves; `start_ts` is the tape anchor that RegimeState
        // needs to fail-close an already-elapsed ReopenGap.
        let regime = RegimeState::new(regime, start_ts, &scalars.symbol);
        let cadence = CadenceWalk::assemble_compiled(
            &scalars,
            session,
            calendar.as_ref(),
            regime.arrival_thin,
            seed,
            start_ts,
            CADENCE_STEP_NS,
        );
        let trade_bounce_ticks = scalars.trade_displacement_ticks.ticks();
        let tick_f64 = decimal_to_f64(scalars.modal_tick);
        let coins = ChildCoins::new(shape.level_step_prob);
        let sizes = SizeQuantizer::new(
            scale.size_dp(),
            size_grid.integral,
            min_units,
            lot_exponent(size_median),
        );
        // Every realization opens on the Buyer side regardless of seed:
        // `prev_side` seeds to Buyer and the low regime flips it only at
        // BOUNCE_LOW_FLIP_PROB (0.02), so a fresh stream prints a long Buyer run
        // before the first flip. This seed-independent opening bias is
        // deliberate and left as-is: it is a start-of-stream transient (a real
        // bounce process is equally free to open either way), it is fully
        // deterministic, and re-seeding the side would consume an extra RNG
        // draw for zero fidelity gain.
        let bounce = || BounceState {
            prev_side: AggressorSide::Buyer,
            high_regime: false,
            drift_ticks: 0,
            drift_dir: 1,
            drift_hot: false,
            laws: BounceLaws::new(),
        };
        let mut rng = ChaCha12Rng::seed_from_u64(seed);
        // The walk's mode, in `begin_event`'s old dispatch order. The
        // cascade's components start from their stationary law, which costs
        // main-stream draws only on a cascade source: a calendar-less source
        // takes exactly the draws it always did. The discrete book takes no
        // main-stream draw: its streams are derived from the seed under their
        // own domain tags, so a book preset's cascade realization is
        // byte-identical to the same preset without a book. The book opens
        // around the start-price anchor at the first phase row's knobs.
        let walk = match (&cascade, &scalars.book, cadence.kernel) {
            (Some(tables), book, _) => {
                let state = CascadeState::new(tables, &mut rng, start_ts);
                match book {
                    Some(config) => {
                        let anchor_ticks = decimal_to_f64(scalars.start_price) / tick_f64;
                        Walk::CascadeBook(BookWalk {
                            cascade: state,
                            book: DiscreteBook::new(seed, anchor_ticks, config.knobs_at(0)),
                            last_published: None,
                        })
                    }
                    None => Walk::Cascade(state),
                }
            }
            (None, _, Some(_)) => Walk::Integrated(IntegratedWalk {
                state: cadence.state.expect("a kernel assembles its state"),
                cadence_rng: cadence.rng.expect("a kernel assembles its cadence rng"),
                bounce: bounce(),
            }),
            (None, _, None) => Walk::Legacy(LegacyWalk {
                arrival: ArrivalClock {
                    active_mean_s,
                    quiet_mean_s: active_mean_s * quiet_active_ratio,
                    quiet_to_active: switch_rate * (1.0 - quiet_fraction),
                    active_to_quiet: switch_rate * quiet_fraction,
                    quiet: false,
                    last_quiet: false,
                },
                arrival_override: None,
                bounce: bounce(),
                last_event_price_ticks: None,
                last_book: None,
            }),
        };
        let spec = RiverSpec {
            session: SessionModulator::new(
                session,
                calendar.as_ref().map(|calendar| calendar.calendar()),
            ),
            scalars,
            scale,
            calendar,
            cascade,
            arrival_kernel: cadence.kernel,
            arrival_env: cadence.env,
            trade_bounce_ticks,
            duration_dist: Weibull::new(1.0, ARRIVAL_WEIBULL_SHAPE).expect("valid weibull"),
            normal: Normal::new(0.0, 1.0).expect("valid normal"),
            chi_squared: ChiSquared::new(STUDENT_T_DF).expect("valid chi-squared"),
            prices: PriceGrid { tick_mantissa },
            sizes,
            top_bid_units,
            top_ask_units,
            min_units,
            coins,
            tick_f64,
            surge: None,
        };
        Ok(Self {
            spec: Arc::new(spec),
            walk,
            rng,
            clock_ns: start_ts,
            resolved_any: false,
            next_child: 0,
            plan_first_child: 0,
            plan_decorated: true,
            size_stream,
            #[cfg(test)]
            size_probe: 0,
            plan: Vec::new(),
            cursor: 0,
            plan_parent_ts: start_ts,
            plan_trades: 0,
            fault: None,
            vol,
            #[cfg(test)]
            size_median,
            regime,
            reopen_frontier_ns: start_ts,
            shape,
            vol_trace_enabled: false,
            vol_trace: None,
            #[cfg(test)]
            latent_updates: 0,
            #[cfg(test)]
            repeat_draws: 0,
            #[cfg(test)]
            rejected_repeat_draws: 0,
            #[cfg(test)]
            reader_emitted: 0,
            #[cfg(test)]
            draw_stages: Vec::new(),
        })
    }

    /// The last instant the walk has reserved; see the field. A fresh source
    /// sits at its `start_ts`. `CheckpointIndex` reads it at parent boundaries
    /// only, where every record consumed so far is at or before it.
    #[must_use]
    pub fn clock_ns(&self) -> u64 {
        self.clock_ns
    }

    #[cfg(test)]
    pub(super) fn arrival_mult_for_test(&self, clock_ns: u64) -> f64 {
        self.spec.session.arrival_mult(clock_ns)
    }

    #[cfg(test)]
    pub(super) fn vol_mult_for_test(&self, clock_ns: u64) -> f64 {
        self.spec.session.vol_mult(clock_ns)
    }

    /// Force one arrival state for the committed tick-composition measurement.
    /// `None` restores the natural Markov chain. This is hidden from generated
    /// API documentation and is public only because Cargo examples compile as
    /// external crates; it is not a venue configuration seam. Only the legacy
    /// walk has a Markov arrival state to force; on every other walk this
    /// changes nothing, as it always has.
    #[doc(hidden)]
    pub fn set_arrival_quiet_for_measurement(&mut self, quiet: Option<bool>) {
        if let Walk::Legacy(walk) = &mut self.walk {
            walk.arrival_override = quiet;
        }
    }

    /// The active-state mean gap this source was built with, in seconds.
    ///
    /// The one observable of the calibrated side of `ARRIVAL_MEAN_CAL`. The
    /// composition happens inline in `new` and lands in a private field, and
    /// the factor cancels in every ratio a test could form from generated
    /// ticks, so without this reader the calibration gate can only assert that
    /// the integrated frame stays bare - half a claim. With it, the corrected
    /// half is exact: at `quiet_share` 0 the two states coincide and this must
    /// equal the calibration times the bare mean, bit for bit.
    /// Test-only: the generator itself reads the field directly, and a reader
    /// compiled into the shipped binary for one gate's benefit would be a
    /// public seam nobody asked for.
    #[cfg(test)]
    pub(super) fn active_mean_s(&self) -> f64 {
        match &self.walk {
            Walk::Legacy(walk) => walk.arrival.active_mean_s,
            _ => panic!("only the legacy walk has a Markov arrival clock"),
        }
    }

    /// One `Weibull(1, ARRIVAL_WEIBULL_SHAPE)` gap. rand_distr samples it as
    /// `scale * (-ln u).powf(1 / shape)` over `u` in `(0, 1]`; at the shipped
    /// shape of one that is `1 * (-ln u).powf(1)`, and both the unit scale and
    /// `powf(x, 1.0) == x` are exact, so the short form draws the same `u` and
    /// returns the same bits without the `powf`. Any other shape takes the
    /// distribution itself.
    fn draw_unit_weibull(&mut self) -> f64 {
        if ARRIVAL_WEIBULL_SHAPE == 1.0 {
            let u: f64 = self.rng.sample(OpenClosed01);
            -u.ln()
        } else {
            self.spec.duration_dist.sample(&mut self.rng)
        }
    }

    fn next_duration_ns(&mut self) -> u64 {
        let walk = legacy(&mut self.walk);
        if let Some(quiet) = walk.arrival_override {
            walk.arrival.quiet = quiet;
        }
        #[cfg(test)]
        self.draw_stages.push("gap");
        let raw_eps = self.draw_unit_weibull();
        let arrival = &legacy(&mut self.walk).arrival;
        let state_mean = if arrival.quiet {
            arrival.quiet_mean_s
        } else {
            arrival.active_mean_s
        };
        let duration_s = (state_mean * raw_eps / ARRIVAL_WEIBULL_MEAN).max(0.000_000_001);
        let flip_probability = if arrival.quiet {
            arrival.quiet_to_active
        } else {
            arrival.active_to_quiet
        };
        #[cfg(test)]
        self.draw_stages.push("flip");
        let flips = self.rng.random_bool(flip_probability);
        let walk = legacy(&mut self.walk);
        walk.arrival.advance_after_gap(flips);
        if let Some(quiet) = walk.arrival_override {
            walk.arrival.quiet = quiet;
        }
        let arr_mult = self.spec.session.arrival_mult(self.clock_ns);
        if arr_mult >= LOW_INTENSITY_ARR_MULT {
            // Open-market path: the multiplier sampled at the instant the gap
            // opens stretches the whole draw. The trailing `.min` is a pure
            // safety rail: for any multiplier above the gate and the validated
            // thin_factor range it is far out of reach of realistic draws, so
            // it never alters an open-market gap; it only guarantees the cast
            // can never saturate to u64::MAX.
            //
            // Note the asymmetry with the closed-window path below, which
            // integrates the session envelope hour by hour: here an armed
            // `LiquidityDrought` stretch stays a once-sampled multiplier on the
            // whole gap. That is deliberate - thin tape is a venue-wide
            // divergence the operator armed, not a session curve the generator
            // is meant to trace - and it is bounded by the cap above.
            let duration_s =
                ((duration_s / arr_mult) * self.regime.arrival_thin).max(0.000_000_001);
            return (duration_s * 1_000_000_000.0)
                .round()
                .max(1.0)
                .min(MAX_SESSION_GAP_NS as f64) as u64;
        }
        self.low_intensity_gap_ns(duration_s)
    }

    /// Wall-clock gap for a duration draw whose gap opens inside a closed
    /// session window (arrival multiplier below `LOW_INTENSITY_ARR_MULT`).
    ///
    /// The open-market path in `next_duration_ns` samples the arrival
    /// multiplier once and stretches the entire draw by `1/mult` - fine while
    /// the multiplier is O(1), catastrophic when a share is near zero: the
    /// stretched gap wildly overshoots the closed window (share 1e-6 turns a
    /// ~7 s draw into ~80 days) and an extreme share saturates the f64->u64
    /// cast at u64::MAX, pinning the parent clock there so every later parent
    /// carries the same `ts_event`.
    ///
    /// Clock advancement is the real invariant. Siblings do not tie and must
    /// not - they are stamped `parent + emitted * INTRA_EVENT_STEP_NS`, a 1 us
    /// stride, and the arrival kernel floors a parent's own advance at that
    /// same stride. So the trade stream is strictly increasing until the u64
    /// nanosecond epoch is exhausted, and a consumer paging history by a
    /// timestamp-only cursor depends on that (see
    /// `a_river_never_prints_two_trades_at_one_instant`, which is what would
    /// catch a change here). Only the quote and its first child share an
    /// instant, deliberately and harmlessly: they are served on separate
    /// pages, so the tie can never cut one.
    ///
    /// Here the draw is instead treated as a budget of un-modulated seconds
    /// and converted to wall time by integrating the piecewise-constant
    /// session intensity hour by hour: each wall hour consumes
    /// `hour_seconds * rate` of budget, so a closed hour consumes almost
    /// nothing and the budget is spent almost entirely in the first open
    /// hour - the tape resumes roughly when the market reopens, which is the
    /// trading-hours semantics the near-zero-share mechanism promises.
    /// Day-of-week transitions land on hour boundaries, so stepping hours
    /// re-samples both curves. Deterministic: no RNG is consumed; the walk is
    /// a pure function of `clock_ns`, the profile and the draw, so same seed +
    /// anchor still yields the same stream.
    ///
    /// Residual limitations, stated honestly:
    /// - only gaps that open below the gate take this path. A gap opening in
    ///   an open hour still crosses a later closed window at its open-hour
    ///   rate (a tick can print inside the closed window).
    /// - a profile whose every hour is effectively closed can never spend the
    ///   budget; the walk caps at `MAX_SESSION_GAP_NS` per gap, so the clock
    ///   advances strictly (one tick per ~year) instead of freezing. Reaching
    ///   u64::MAX at all now requires actually simulating the ~580-year u64
    ///   nanosecond epoch - an inherent representation limit, no longer a
    ///   session artifact.
    fn low_intensity_gap_ns(&self, duration_s: f64) -> u64 {
        let mut budget_s = duration_s;
        let mut pos_ns = self.clock_ns;
        let mut gap_ns: u64 = 0;
        while gap_ns < MAX_SESSION_GAP_NS {
            // Effective arrival rate over this hour segment: session shares are
            // validated strictly positive finite and thin_factor is validated
            // in [1, 1000], so the rate is positive - unless the product
            // underflows to 0.0, in which case `budget_s / rate` is +inf, the
            // residual branch below never fires, and the walk runs to the cap
            // (the venue is closed harder than f64 can express).
            let rate = self.spec.session.arrival_mult(pos_ns) / self.regime.arrival_thin;
            let to_boundary_ns = NS_PER_HOUR - (pos_ns % NS_PER_HOUR);
            let need_ns = (budget_s / rate) * 1_000_000_000.0;
            if need_ns <= to_boundary_ns as f64 {
                // The budget runs out inside this hour: spend the residual and
                // stop. `need_ns` is at most an hour in ns here, so the cast is
                // exact; the floor keeps the clock strictly advancing.
                let residual_ns = (need_ns.round() as u64).max(1);
                return gap_ns
                    .saturating_add(residual_ns)
                    .clamp(1, MAX_SESSION_GAP_NS);
            }
            budget_s -= (to_boundary_ns as f64 / 1_000_000_000.0) * rate;
            pos_ns = pos_ns.saturating_add(to_boundary_ns);
            gap_ns = gap_ns.saturating_add(to_boundary_ns);
        }
        MAX_SESSION_GAP_NS
    }

    fn next_latent_mid(&mut self) -> f64 {
        #[cfg(test)]
        {
            self.latent_updates += 1;
        }
        // The chi-squared draw inside `draw_student_t` is floored at
        // `f64::MIN_POSITIVE`: an unguarded 0.0 denominator makes `student_t`
        // `0.0/0.0 = NaN` when `normal` also happens to be 0.0, and
        // `f64::clamp` propagates NaN through `base_return` into `mid`,
        // poisoning the walk for the rest of the session.
        // Drawn raw then standardized in two named steps - numerically the
        // identical sequence `draw_standardized_innovation` performs - so the
        // observation-only trace can carry both without a second draw.
        let innovation_raw =
            draw_student_t(&mut self.rng, &self.spec.normal, &self.spec.chi_squared);
        let innovation = innovation_raw / STUDENT_T_UNIT_SCALE;
        // The base process is regime-independent by construction. `step` applies
        // the GARCH-state rail and the feedback rail, neither of which takes a
        // regime multiplier, so an armed storm cannot raise the state ceiling
        // and thereby alter every later recursion step. GARCH feedback sees the
        // un-modulated return, so volatility clustering is identical armed or
        // not; the envelopes below scale only what is realized.
        let mid_before = self.vol.mid;
        let step = self.vol.step(innovation);
        let base_return = step.base_return;
        // Vol composition convention (see also RegimeState::vol_mult): the
        // session envelope and the regime envelope compose multiplicatively here
        // (session 1.0 = no session bias, regime 1.0 = no regime bias, so the
        // product is the combined RMS scale). Inside the regime envelope a
        // SessionEdgeSpike instead composes additively onto the storm baseline
        // (vol_mult + edge_mult). The two conventions are intentional and rely on
        // both neutral values being 1.0; do not restructure either into the other
        // (a future regime that set both vol_mult and an edge spike would want the
        // add re-examined - today the match is exclusive so only one is non-unit).
        let session_vol_mult = self.spec.session.vol_mult(self.clock_ns);
        let regime_vol_mult = self.regime.vol_mult(self.clock_ns);
        let vol_mult = session_vol_mult * regime_vol_mult;
        // Realized ceiling: absolute, and deliberately not scaled by anything.
        // Every regime - VolStorm, SessionEdgeSpike, drought, reopen - and the
        // session curve reach the mid only through `vol_mult` above, and then
        // meet the same fixed ceiling. Scaling this by the regime is what used
        // to let a maximum-strength storm's permitted single-event move ride on
        // whatever the base rail happened to be; sizing it as a policy against
        // the measured envelope is what replaced that.
        //
        // Bounds one event. Cumulative movement is not bounded by it: a
        // sustained storm walks the mid as far as its duration allows.
        let pre_realized = base_return * vol_mult;
        let return_n = pre_realized.clamp(-REALIZED_RETURN_CEILING, REALIZED_RETURN_CEILING);
        self.vol.mid = (self.vol.mid * return_n.exp())
            .max(self.spec.tick_f64)
            .min(MID_CEILING);
        if self.vol_trace_enabled {
            self.vol_trace = Some(VolTrace {
                innovation_raw,
                innovation_std: innovation,
                sigma2_candidate: step.sigma2_candidate,
                sigma2_realized: step.sigma2,
                sigma_cap_hit: step.hit_variance_cap(),
                garch_scale: step.garch_scale,
                base_return_unclipped: step.unclipped_return,
                base_return,
                feedback_clamp_hit: step.hit_feedback_clamp(),
                session_vol_mult,
                regime_vol_mult,
                pre_realized_return: pre_realized,
                realized_return: return_n,
                realized_clamp_hit: pre_realized.abs() > REALIZED_RETURN_CEILING,
                mid_before,
                mid_after: self.vol.mid,
            });
        }
        self.vol.mid
    }

    /// Turn on the observation-only volatility trace. Consumes no draws and
    /// changes nothing about the walk - the byte-identity test pins it.
    pub fn enable_vol_trace(&mut self) {
        self.vol_trace_enabled = true;
    }

    /// The last parent event's volatility intermediates, if tracing is on.
    pub fn take_vol_trace(&mut self) -> Option<VolTrace> {
        self.vol_trace.take()
    }

    /// The bounce side and drift step, then the book placed on the drifted
    /// mid. The legacy and integrated walks only: the cascade draws its own
    /// side and places on the mid exactly.
    fn next_side_and_book(&mut self, mid: f64) -> (AggressorSide, PublishedBook) {
        let bounce = bounce_mut(&mut self.walk);
        let side = bounce.next_side(&mut self.rng);
        bounce.next_drift(&mut self.rng);
        let drifted_mid_ticks = mid / self.spec.tick_f64 + bounce.drift_ticks as f64;
        let book = place_book(
            drifted_mid_ticks,
            &self.spec.scalars.quoted_width,
            &self.spec.scalars.top_sizes,
        );
        (side, book)
    }

    /// One size from the river's size law, at test-only indices of its own,
    /// for the gates that pin the sampler.
    #[cfg(test)]
    pub(super) fn next_size(&mut self) -> Decimal {
        let mut stream = self.size_stream.clone();
        stream.seek(self.size_probe);
        self.size_probe += 1;
        let draw = stream.draw();
        self.spec
            .scale
            .size(self.spec.sizes.quantize(draw.base, draw.lot_snap))
    }

    /// Fill the resolved plan's placed-trade quantities from the size
    /// stream, positioned at the parent's first child, once. Every reader
    /// that hands out a record or a parent view calls it first, so no
    /// undecorated trade escapes; a discrete-book plan resolves decorated
    /// and is never touched, its quantities being the book's own.
    fn decorate(&mut self) {
        if self.plan_decorated {
            return;
        }
        self.size_stream.seek(self.plan_first_child);
        for record in &mut self.plan {
            if let Record::Trade(trade) = record {
                let draw = self.size_stream.draw();
                trade.qty = self.spec.sizes.quantize(draw.base, draw.lot_snap);
            }
        }
        self.plan_decorated = true;
    }

    /// A price coordinate in ticks as a record mantissa, or the terminal
    /// fault that refuses it.
    fn price(&self, ticks: f64) -> Result<i64, TickFault> {
        self.spec
            .prices
            .mantissa(ticks)
            .ok_or(TickFault::PriceUnrepresentable {
                clock_ns: self.clock_ns,
            })
    }

    fn book_price(&self, ticks: i64) -> Result<i64, TickFault> {
        self.spec
            .prices
            .mantissa_of_ticks(ticks)
            .ok_or(TickFault::PriceUnrepresentable {
                clock_ns: self.clock_ns,
            })
    }

    /// A discrete-book quantity in units of the size increment, as a record
    /// mantissa, saturating at the quantity ceiling.
    fn book_units(&self, units: u64) -> u64 {
        units
            .checked_mul(self.spec.min_units)
            .map_or(SIZE_UNITS_CEILING, |mantissa| {
                mantissa.min(SIZE_UNITS_CEILING)
            })
    }

    /// The placed book's quote record at the clock.
    fn placed_quote(&self, book: &PublishedBook) -> Result<Record, TickFault> {
        Ok(Record::Quote(QuoteRec {
            ts: self.clock_ns,
            bid_px: self.price(book.bid_ticks)?,
            ask_px: self.price(book.ask_ticks)?,
            bid_qty: self.spec.top_bid_units,
            ask_qty: self.spec.top_ask_units,
        }))
    }

    /// Step every child of the sweep just begun and append its trade records,
    /// their quantities left for `decorate`. The sweep is the parent's own: it
    /// starts and ends inside this call, because a parent always resolves
    /// whole. Its children's global indices are reserved here, whether or not
    /// anyone ever reads their sizes.
    fn emit_children(&mut self, mut burst: SweepBurst) -> Result<(), TickFault> {
        self.plan_first_child = self.next_child;
        self.next_child = self
            .next_child
            .checked_add(u64::from(burst.remaining))
            .expect("the child index space is not exhausted");
        self.plan_decorated = false;
        let mut steps = LevelSteps::new(self.level_step_prob(), burst.remaining, &mut self.rng);
        while burst.remaining > 0 {
            let price_ticks = self.step_child(&mut burst, &mut steps);
            let record = Record::Trade(TradeRec {
                ts: self.clock_ns,
                px: self.price(price_ticks)?,
                qty: 0,
                side: burst.side,
            });
            self.plan.push(record);
        }
        Ok(())
    }

    /// The level-step probability of the sweep about to be placed. The bounce
    /// regime never steps on the cascade path, so its low multiplier would pin
    /// the level step at a third of the fitted probability for good; the
    /// cascade takes the solved probability as it is (tape protocol 33). A
    /// real multi-print parent on CME is a multi-level parent nine times in
    /// ten, because a trade summary is one print per level. On the bounce
    /// walks the regime is fixed for the whole sweep: it moves once per
    /// parent, in `next_side`, before the sweep begins.
    fn level_step_prob(&self) -> f64 {
        let coins = &self.spec.coins;
        match &self.walk {
            Walk::Cascade(_) => coins.level_step_cascade,
            Walk::Legacy(LegacyWalk { bounce, .. })
            | Walk::Integrated(IntegratedWalk { bounce, .. }) => {
                if bounce.high_regime {
                    coins.level_step_high
                } else {
                    coins.level_step_low
                }
            }
            Walk::CascadeBook(_) => unreachable!("the discrete book places no sweep"),
        }
    }
}

/// The legacy walk's own state. Called only on paths `begin_event` has
/// already dispatched to the legacy walk.
fn legacy(walk: &mut Walk) -> &mut LegacyWalk {
    match walk {
        Walk::Legacy(walk) => walk,
        _ => unreachable!("a legacy step on a non-legacy walk"),
    }
}

fn book_walk(walk: &mut Walk) -> &mut BookWalk {
    match walk {
        Walk::CascadeBook(walk) => walk,
        _ => unreachable!("a book step on a walk without a discrete book"),
    }
}

fn book_mut(walk: &mut Walk) -> &mut DiscreteBook {
    &mut book_walk(walk).book
}

fn last_published_mut(walk: &mut Walk) -> &mut Option<BookSnapshot> {
    &mut book_walk(walk).last_published
}

/// The bounce/drift state of the walks that place a drifted book.
fn bounce_mut(walk: &mut Walk) -> &mut BounceState {
    match walk {
        Walk::Legacy(walk) => &mut walk.bounce,
        Walk::Integrated(walk) => &mut walk.bounce,
        Walk::Cascade(_) | Walk::CascadeBook(_) => {
            unreachable!("the cascade walks carry no bounce state")
        }
    }
}

/// Round-lot scale selected from the latent distribution center. Keeping this
/// tiny derivation named makes its decade transitions directly testable: the
/// old crypto notional proxy pinned it at one and left every larger-contract
/// branch live but unexercised.
#[cfg(test)]
pub(super) fn integral_lot(latent_size_median: f64) -> f64 {
    10_f64.powf(latent_size_median.log10().floor()).max(1.0)
}

/// A placed parent's opening trade level: the book mid displaced by the
/// trade bounce toward the aggressor, rounded to the grid, floored at one tick.
fn placed_trade_ticks(book: &PublishedBook, aggressor: AggressorSide, bounce_ticks: f64) -> f64 {
    match aggressor {
        AggressorSide::Buyer => (book_mid_ticks(book) + bounce_ticks).round(),
        AggressorSide::Seller => (book_mid_ticks(book) - bounce_ticks).round(),
        AggressorSide::NoAggressor => unreachable!("a placed parent's side is native"),
    }
    .max(1.0)
}

pub(super) fn repeat_is_compatible(
    price_ticks: f64,
    frozen_book: &PublishedBook,
    aggressor: AggressorSide,
) -> bool {
    match aggressor {
        AggressorSide::Buyer => price_ticks >= book_mid_ticks(frozen_book),
        AggressorSide::Seller => price_ticks <= book_mid_ticks(frozen_book),
        AggressorSide::NoAggressor => false,
    }
}

impl RiverSource for GeneratedSource {
    fn scale(&self) -> &RecordScale {
        &self.spec.scale
    }

    fn next_record(&mut self) -> Option<Record> {
        if self.cursor == self.plan.len() {
            self.resolve_parent().ok()?;
        }
        self.decorate();
        let record = self.plan[self.cursor];
        #[cfg(test)]
        {
            if self.cursor == 0 {
                self.reader_emitted = 0;
            }
            self.reader_emitted += u32::from(matches!(record, Record::Trade(_)));
        }
        self.cursor += 1;
        if self.cursor == self.plan.len() {
            self.plan.clear();
            self.cursor = 0;
        }
        Some(record)
    }

    fn fault(&self) -> Option<TickFault> {
        self.fault
    }

    /// Resolve forward, dropping each whole parent whose reserved span ends
    /// before `start_ts` - no clone, no second resolution - and park inside
    /// the first parent that reaches it, returning its first record at or
    /// after `start_ts`. A parent's reserved end bounds every record it
    /// emits, so a parent ending below the target has nothing in the window.
    fn seek_to(&mut self, start_ts: u64) -> Option<Record> {
        loop {
            if self.cursor == self.plan.len() {
                self.resolve_parent().ok()?;
                if self.clock_ns < start_ts {
                    self.plan.clear();
                    self.cursor = 0;
                    continue;
                }
            }
            let record = self.next_record()?;
            if record.ts() >= start_ts {
                return Some(record);
            }
        }
    }
}

impl ParentSource for GeneratedSource {
    fn next_parent(&mut self) -> Option<ParentView<'_>> {
        assert!(
            self.at_parent_boundary(),
            "next_parent called mid-parent, after a partial record read"
        );
        self.resolve_parent().ok()?;
        self.decorate();
        // Consumed whole: the next call clears it before resolving.
        self.cursor = self.plan.len();
        Some(ParentView {
            parent_ts: self.plan_parent_ts,
            records: &self.plan,
            trades: self.plan_trades,
            trace: if self.vol_trace_enabled {
                self.vol_trace
            } else {
                None
            },
        })
    }

    /// `next_parent` without the size draws. Sizes ride their own
    /// child-indexed stream, which `decorate` seeks to the parent's first
    /// child before every use, so leaving a parent undecorated moves no draw
    /// of the main stream and no quantity of any later parent: the child
    /// indices were reserved at resolution either way.
    fn next_parent_unsized(&mut self) -> Option<UnsizedParentView<'_>> {
        assert!(
            self.at_parent_boundary(),
            "next_parent_unsized called mid-parent, after a partial record read"
        );
        self.resolve_parent().ok()?;
        self.cursor = self.plan.len();
        Some(
            ParentView {
                parent_ts: self.plan_parent_ts,
                records: &self.plan,
                trades: self.plan_trades,
                trace: if self.vol_trace_enabled {
                    self.vol_trace
                } else {
                    None
                },
            }
            .unsized_view(),
        )
    }
}

impl GeneratedSource {
    /// Whether no resolved record is still owed to a reader.
    #[must_use]
    pub fn at_parent_boundary(&self) -> bool {
        self.cursor == self.plan.len()
    }

    /// Trades of the current parent a reader has not taken yet.
    #[cfg(test)]
    pub(super) fn owed_children(&self) -> u32 {
        self.plan[self.cursor..]
            .iter()
            .filter(|record| matches!(record, Record::Trade(_)))
            .count() as u32
    }

    /// Whether the walk is the discrete-book path, whose parents publish
    /// their quotes and first trade off the parent instant.
    #[cfg(test)]
    pub(super) fn is_book_walk(&self) -> bool {
        matches!(self.walk, Walk::CascadeBook(_))
    }

    /// Trades of the current (or just drained) parent a reader has taken.
    #[cfg(test)]
    pub(super) fn emitted_children(&self) -> u32 {
        self.reader_emitted
    }

    /// The edge view of the next record: the protocol event it denotes. For
    /// tests and offline readers that want `TickEvent`s; the serving path
    /// reads records.
    pub fn next_tick(&mut self) -> Option<TickEvent> {
        let record = self.next_record()?;
        Some(self.spec.scale.event(&record))
    }

    /// Resolve the next whole parent into the plan. On success the plan holds
    /// at least one and at most `MAX_PARENT_RECORDS` records and the cursor is
    /// at its start; on failure the fault is latched and the plan is empty, so
    /// a failed parent publishes nothing - neither emit nor skip can observe a
    /// prefix of it. A latched fault is terminal: no later call draws again.
    fn resolve_parent(&mut self) -> Result<(), TickFault> {
        self.resolved_any = true;
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        self.plan.clear();
        self.cursor = 0;
        self.plan_trades = 0;
        // A placed sweep clears this as it reserves its children; a
        // discrete-book parent's quantities are written at resolution.
        self.plan_decorated = true;
        match self.begin_event() {
            Ok(()) => {
                assert!(
                    (1..=MAX_PARENT_RECORDS).contains(&self.plan.len()),
                    "a parent emitted {} records, outside [1, {MAX_PARENT_RECORDS}]",
                    self.plan.len()
                );
                self.plan_trades = self
                    .plan
                    .iter()
                    .filter(|record| matches!(record, Record::Trade(_)))
                    .count() as u32;
                Ok(())
            }
            Err(fault) => {
                self.plan.clear();
                self.fault = Some(fault);
                Err(fault)
            }
        }
    }

    /// Advance exactly one parent for the checkpoint extension, reporting how
    /// many records it emitted. Requires a parent boundary.
    pub(super) fn skip_parent(&mut self) -> Result<usize, TickFault> {
        assert!(
            self.at_parent_boundary(),
            "skip_parent requires a parent boundary"
        );
        self.resolve_parent()?;
        let emitted = self.plan.len();
        self.plan.clear();
        self.cursor = 0;
        Ok(emitted)
    }

    /// Advance exactly one parent and all of its children, reporting its
    /// timing and child count.
    ///
    /// Fallible because the arrival kernel can refuse. The refusal is
    /// returned; the fault stays latched on `self`, so `fault()` also reports
    /// it, and nothing is drawn after it.
    pub fn advance_parent(&mut self) -> Result<ParentSummary, TickFault> {
        assert!(
            self.at_parent_boundary(),
            "advance_parent requires a parent boundary"
        );
        self.resolve_parent()?;
        let summary = ParentSummary {
            parent_ts_ns: self.plan_parent_ts,
            child_count: self.plan_trades,
            child_stride_ns: INTRA_EVENT_STEP_NS,
        };
        self.plan.clear();
        self.cursor = 0;
        Ok(summary)
    }

    /// Advances one integrated parent and exposes the cadence latent used for
    /// that parent. This is crate-visible solely for the Brick K transcript
    /// replay: it lets the Stage B integration test compare the full record,
    /// rather than letting a latent-state drift hide behind matching times.
    ///
    /// Test-gated because it exists for no other caller: without the `cfg` the
    /// lib target builds it unused and `--all-targets` clippy reports dead
    /// code, which is the honest reading - a production seam with no
    /// production caller should not be shipped as one.
    #[cfg(test)]
    pub(crate) fn advance_parent_with_arrival_latent(&mut self) -> (ParentSummary, u64) {
        assert!(
            matches!(self.walk, Walk::Integrated(_)),
            "arrival transcript requires an integrated kernel"
        );
        let summary = self
            .advance_parent()
            .expect("arrival transcript requires a non-refusing walk");
        let Walk::Integrated(walk) = &self.walk else {
            unreachable!("the walk's mode never changes")
        };
        let latent_x = self
            .spec
            .arrival_kernel
            .expect("integrated branch has kernel")
            .latent_x(&walk.state)
            .to_bits();
        (summary, latent_x)
    }

    #[cfg(test)]
    pub(super) fn diagnostic_place_book(&self) -> PublishedBook {
        place_book(
            self.vol.mid / self.spec.tick_f64 + self.bounce().drift_ticks as f64,
            &self.spec.scalars.quoted_width,
            &self.spec.scalars.top_sizes,
        )
    }

    fn begin_integrated_event(
        &mut self,
        rate_mult: f64,
        children_mult: f64,
    ) -> Result<(), TickFault> {
        let spec = Arc::clone(&self.spec);
        let kernel = spec.arrival_kernel.expect("integrated branch has kernel");
        // The kernel tests the crossing against its own walk start, which is the
        // clock after the previous burst's children stepped it past their
        // parent. An arm inside that intra-burst span is still owed, so it is
        // presented at the first instant the kernel can act on: the effective
        // instant is the arm's own `at_ts` whenever it is genuinely ahead, and
        // otherwise the very next nanosecond. `reopen_frontier_ns` is what
        // guarantees a still-pending arm sits strictly after the last tested
        // span, so this can only move an arm the walk would otherwise strand.
        let pending_reopen =
            self.regime
                .pending_reopen()
                .map(|(at_ts_ns, shift_ns)| PendingReopen {
                    at_ts_ns: at_ts_ns.max(self.clock_ns.saturating_add(1)),
                    shift_ns,
                });
        let Walk::Integrated(walk) = &mut self.walk else {
            unreachable!("an integrated step on a non-integrated walk")
        };
        let draw = kernel
            .next_parent(
                &mut walk.state,
                self.clock_ns,
                super::arrival::cadence_base_mean_s(&spec.scalars),
                &self.shape,
                spec.arrival_env
                    .as_ref()
                    .expect("integrated branch has environment"),
                RuntimeModifiers {
                    rate_mult,
                    children_mult,
                    pending_reopen,
                },
                &mut walk.cadence_rng,
            )
            .map_err(TickFault::Arrival)?;
        self.clock_ns = draw.parent_ts_ns;
        if draw.reopen_applied {
            let reopen = self
                .regime
                .take_reopen_crossed(self.reopen_frontier_ns, draw.parent_ts_ns)
                .expect("arrival kernel and regime disagree about reopen crossing");
            self.vol.mid = (self.vol.mid * reopen.gap_frac.exp())
                .max(spec.tick_f64)
                .min(MID_CEILING);
        }
        self.reopen_frontier_ns = self.clock_ns;
        let mid = self.next_latent_mid();
        let (aggressor, book) = self.next_side_and_book(mid);
        let price_ticks = placed_trade_ticks(&book, aggressor, spec.trade_bounce_ticks);
        self.plan_parent_ts = self.clock_ns;
        let quote = self.placed_quote(&book)?;
        self.plan.push(quote);
        self.emit_children(SweepBurst {
            remaining: draw.child_count,
            emitted: 0,
            parent_ts_ns: self.clock_ns,
            side: aggressor,
            price_ticks,
        })
    }

    /// Draw the next cascade parent and apply everything external to it: the
    /// clock advance, an armed reopen's halt and gap, and the bucket reset.
    /// Returns the parent and the external log move (diffusion, jump and gap;
    /// never the propagator impact, which the two paths supply differently -
    /// the placed-book path adds the cascade propagator, the discrete-book
    /// path takes the book's own anchor impact instead).
    fn draw_cascade_parent(
        &mut self,
        rate_mult: f64,
    ) -> Result<(super::cascade::CascadeParent, f64), TickFault> {
        let spec = Arc::clone(&self.spec);
        let tables = spec.cascade.as_ref().expect("cascade branch has tables");
        let state = match &mut self.walk {
            Walk::Cascade(state) => state,
            Walk::CascadeBook(walk) => &mut walk.cascade,
            Walk::Legacy(_) | Walk::Integrated(_) => {
                unreachable!("a cascade step on a non-cascade walk")
            }
        };
        if !state.has_parent() {
            // A filled bucket owes at least one parent by construction, so
            // one fill is one parent.
            let modifiers = SecondModifiers {
                rate_mult,
                arrival_thin: self.regime.arrival_thin,
                vol_mult: self.regime.vol_mult(self.clock_ns),
            };
            if let Err(CascadeRefusal::ClockExhausted) =
                state.fill_next_bucket(tables, &mut self.rng, self.clock_ns, modifiers)
            {
                return Err(TickFault::CascadeClockExhausted {
                    clock_ns: self.clock_ns,
                });
            }
        }
        let parent = state.draw_parent(tables, &mut self.rng, self.clock_ns);
        self.clock_ns = parent.ts_ns;
        let mut log_move = parent.log_move;
        // An armed `ReopenGap` is the unscheduled jump on top of the
        // cascade's own scheduled ones: the halt moves the clock, the gap
        // moves the mid, and the calendar stays authoritative over whether
        // the halt's end may print.
        if let Some(reopen) = self
            .regime
            .take_reopen_crossed(self.reopen_frontier_ns, self.clock_ns)
        {
            self.clock_ns = self.clock_ns.saturating_add(reopen.halt_ns);
            log_move += reopen.gap_frac;
            if let Some(calendar) = &spec.calendar
                && !calendar.is_open(self.clock_ns)
            {
                self.clock_ns = calendar.next_open_ns(self.clock_ns);
            }
            state.reset_bucket(self.clock_ns);
        }
        self.reopen_frontier_ns = self.clock_ns;
        Ok((parent, log_move))
    }

    /// One parent under the activity cascade: the next parent the cascade
    /// owes, the mid moved by its log move, the book placed on the mid, the
    /// side the cascade drew, and the sweep from the declared child mixture.
    /// No GARCH step, no bounce regime, no drift, no price repeat: the mid is
    /// a martingale on the tick grid and the book follows it exactly.
    fn begin_cascade_event(&mut self, rate_mult: f64, children_mult: f64) -> Result<(), TickFault> {
        let (parent, log_move) = self.draw_cascade_parent(rate_mult)?;
        let mut log_move = log_move;
        let mid_before = self.vol.mid;
        // The propagator (tape protocol 34): the parent's impact move in
        // ticks, converted to a log move at the current mid and added to
        // the cascade's diffusive move, jump and gap. The variance it adds
        // over a minute is fitted out of `event_log_sigma` in the preset.
        let tick_f64 = self.spec.tick_f64;
        let impact_log = parent.impact_ticks * tick_f64 / mid_before.max(tick_f64);
        log_move = (log_move + impact_log).clamp(-MAX_EVENT_LOG_MOVE, MAX_EVENT_LOG_MOVE);
        self.vol.mid = (self.vol.mid * log_move.exp())
            .max(tick_f64)
            .min(MID_CEILING);
        if self.vol_trace_enabled {
            // The observation-only record, in the cascade's terms: there is
            // no GARCH recursion, so the candidate and realized variances are
            // both the second's per-parent variance and no rail can be hit;
            // the session multiplier is the sigma shape the second carried,
            // and the realized return is the whole move, jump and gap
            // included, which is what moved the mid.
            let sigma2 = parent.sigma * parent.sigma;
            self.vol_trace = Some(VolTrace {
                innovation_raw: parent.innovation,
                innovation_std: parent.innovation,
                sigma2_candidate: sigma2,
                sigma2_realized: sigma2,
                sigma_cap_hit: false,
                garch_scale: parent.sigma,
                base_return_unclipped: parent.sigma * parent.innovation,
                base_return: parent.sigma * parent.innovation,
                feedback_clamp_hit: false,
                session_vol_mult: 1.0,
                regime_vol_mult: self.regime.vol_mult(self.clock_ns),
                pre_realized_return: log_move,
                realized_return: log_move,
                realized_clamp_hit: false,
                mid_before,
                mid_after: self.vol.mid,
            });
        }
        let book = place_book(
            self.vol.mid / tick_f64,
            &self.spec.scalars.quoted_width,
            &self.spec.scalars.top_sizes,
        );
        let aggressor = parent.side;
        let price_ticks = placed_trade_ticks(&book, aggressor, self.spec.trade_bounce_ticks);
        let count = if children_mult == 1.0 {
            self.shape.next_count(&mut self.rng)
        } else {
            self.shape.next_count_scaled(children_mult, &mut self.rng)
        };
        self.plan_parent_ts = self.clock_ns;
        let quote = self.placed_quote(&book)?;
        self.plan.push(quote);
        self.emit_children(SweepBurst {
            remaining: count,
            emitted: 0,
            parent_ts_ns: self.clock_ns,
            side: aggressor,
            price_ticks,
        })
    }

    /// One parent on the discrete-book path (see the `book` module).
    /// The cascade still owns the clock and the external move (diffusion,
    /// jump, gap); everything the touch does - the spread, the depletion, the
    /// impact, the printed prices and sizes - is the book's, resolved into
    /// the plan the wire and the checkpoint drain identically.
    fn begin_cascade_book_event(
        &mut self,
        rate_mult: f64,
        children_mult: f64,
    ) -> Result<(), TickFault> {
        let (parent, log_move) = self.draw_cascade_parent(rate_mult)?;
        let mid_before = self.vol.mid;
        // External move only: diffusion, jump and gap. The propagator is not
        // applied here - the book's anchor impact replaces it (step 5).
        let ext = log_move.clamp(-MAX_EVENT_LOG_MOVE, MAX_EVENT_LOG_MOVE);
        self.vol.mid = (self.vol.mid * ext.exp())
            .max(self.spec.tick_f64)
            .min(MID_CEILING);
        self.resolve_book_parent(parent.side, children_mult)?;
        if self.vol_trace_enabled {
            // The observation-only record, in the book path's terms: the
            // diffusive part is the cascade second's, and the realized
            // return is everything that moved the mid across the parent -
            // the external move here plus the book's anchor impact applied
            // inside `resolve_book_parent`. Reconstructed from the mids so
            // the closed-12b control walk sees the move that actually
            // landed; consumes no draws and moves no byte.
            let sigma2 = parent.sigma * parent.sigma;
            let realized = (self.vol.mid / mid_before.max(f64::MIN_POSITIVE)).ln();
            self.vol_trace = Some(VolTrace {
                innovation_raw: parent.innovation,
                innovation_std: parent.innovation,
                sigma2_candidate: sigma2,
                sigma2_realized: sigma2,
                sigma_cap_hit: false,
                garch_scale: parent.sigma,
                base_return_unclipped: parent.sigma * parent.innovation,
                base_return: parent.sigma * parent.innovation,
                feedback_clamp_hit: false,
                session_vol_mult: 1.0,
                regime_vol_mult: self.regime.vol_mult(self.clock_ns),
                pre_realized_return: realized,
                realized_return: realized,
                realized_clamp_hit: false,
                mid_before,
                mid_after: self.vol.mid,
            });
        }
        Ok(())
    }

    /// Resolve one book parent into its records, advancing the clock across
    /// every reserved slot. The causal order is the spec's: project against
    /// the externally moved anchor, draw the parent size, walk the frozen
    /// ladder, settle the depletion, then move the anchor by the book's
    /// impact and relax the spread against the moved anchor, and project
    /// once more. Quotes whose four values repeat the last published snapshot
    /// are suppressed, their instants still reserved.
    fn resolve_book_parent(
        &mut self,
        side: AggressorSide,
        children_mult: f64,
    ) -> Result<(), TickFault> {
        let spec = Arc::clone(&self.spec);
        let tick_f64 = spec.tick_f64;
        let config = spec.scalars.book.as_ref().expect("book path has a config");
        // The phase row is the law in effect at this minute of session. A
        // parent outside any session would silently take row zero's law, so
        // absence is an invariant failure rather than an implicit phase:
        // the book requires the cascade, the cascade requires a calendar
        // with an envelope, and the envelope schedules parents only inside
        // open windows.
        let (_day, minute) = spec
            .calendar
            .as_ref()
            .expect("the book path requires a calendar")
            .session_position(self.clock_ns)
            .expect("a book parent was drawn outside any session window");
        let knobs = config.knobs_at(minute as u32);
        let struck = match side {
            AggressorSide::Buyer => Struck::Ask,
            AggressorSide::Seller => Struck::Bid,
            AggressorSide::NoAggressor => unreachable!("the cascade only draws buyer or seller"),
        };
        // The parent size is entirely the book's: the touch bucket's
        // deconvolved match probability takes the displayed quantity,
        // else the deconvolved independent law, both on the book stream.
        // The main-stream size law and the regime children multiplier do
        // not reach this path - a scaled size would break the measured
        // match cells the channel is fitted to. `children_mult` is
        // accepted for signature parity with the placed path and unused.
        let _ = children_mult;

        let anchor_ext = self.vol.mid / tick_f64;
        let book = book_mut(&mut self.walk);
        book.project(anchor_ext, knobs);
        // The replacement-queue renewal, at the ruled placement: after
        // the parent's projection, before its pre-trade book is
        // recorded.
        book.renew(knobs);
        let pre = book.snapshot();
        let size_units = book.effective_size(struck, knobs);
        let walk = book.execute(struck, size_units, &config.depth_ratios);
        book.settle(struck, &walk, knobs);
        let reports = book.split_reports(&walk, knobs);
        let impact_ticks = book.anchor_impact_ticks(struck, knobs);

        // Step 5's anchor half, applied to the latent mid, then the spread
        // relaxes and the book projects against the moved anchor (steps 5-6).
        let impact_log = impact_ticks * tick_f64 / self.vol.mid.max(tick_f64);
        self.vol.mid = (self.vol.mid * impact_log.exp())
            .max(tick_f64)
            .min(MID_CEILING);
        let anchor_moved = self.vol.mid / tick_f64;
        let book = book_mut(&mut self.walk);
        book.relax(anchor_moved, &config.depth_ratios, knobs);
        book.project(anchor_moved, knobs);
        let post = book.snapshot();

        self.build_book_plan(&pre, &reports, &post, side)
    }

    /// Lay the reserved-slot schedule and write the emitted records. Slot
    /// zero is the pre-trade quote, slots one to `n` the `n` trade records,
    /// slot `n + 1` the post-trade quote; every slot is one intra-event step,
    /// and the clock advances to the last reserved slot whether or not its
    /// quote was emitted. A quote equal in all four published values to the
    /// last one is suppressed; the origin always publishes.
    fn build_book_plan(
        &mut self,
        pre: &BookSnapshot,
        reports: &[super::book::LevelFill],
        post: &BookSnapshot,
        side: AggressorSide,
    ) -> Result<(), TickFault> {
        let parent_ts = self.clock_ns;
        self.plan_parent_ts = parent_ts;
        self.publish_quote(pre, parent_ts)?;
        for (index, fill) in reports.iter().enumerate() {
            let ts =
                parent_ts.saturating_add(((index as u64) + 1).saturating_mul(INTRA_EVENT_STEP_NS));
            let record = Record::Trade(TradeRec {
                ts,
                px: self.book_price(fill.price_ticks)?,
                qty: self.book_units(fill.units),
                side,
            });
            self.plan.push(record);
        }
        let post_slot = (reports.len() as u64) + 1;
        let post_ts = parent_ts.saturating_add(post_slot.saturating_mul(INTRA_EVENT_STEP_NS));
        self.publish_quote(post, post_ts)?;
        // The next parent floors strictly after the last reserved slot,
        // whether or not the post-trade quote was emitted.
        self.clock_ns = post_ts;
        Ok(())
    }

    /// Append a published quote for a snapshot, or nothing when its four
    /// values repeat the last published snapshot (deduplication). The origin,
    /// with no prior snapshot, always publishes. Updates the dedup state
    /// whenever a quote is emitted.
    fn publish_quote(&mut self, snap: &BookSnapshot, ts: u64) -> Result<(), TickFault> {
        if last_published_mut(&mut self.walk).as_ref() == Some(snap) {
            return Ok(());
        }
        let record = Record::Quote(QuoteRec {
            ts,
            bid_px: self.book_price(snap.bid_ticks)?,
            ask_px: self.book_price(snap.ask_ticks)?,
            bid_qty: self.book_units(snap.bid_units),
            ask_qty: self.book_units(snap.ask_units),
        });
        *last_published_mut(&mut self.walk) = Some(*snap);
        self.plan.push(record);
        Ok(())
    }

    fn begin_event(&mut self) -> Result<(), TickFault> {
        #[cfg(test)]
        self.draw_stages.clear();
        let (rate_mult, children_mult) = self
            .spec
            .surge
            .as_ref()
            .filter(|window| (window.start_ns..window.end_ns).contains(&self.clock_ns))
            .map_or((1.0, 1.0), |window| {
                (window.rate_mult, window.children_mult)
            });
        match self.walk {
            Walk::Legacy(_) => self.begin_legacy_event(rate_mult, children_mult),
            Walk::Integrated(_) => self.begin_integrated_event(rate_mult, children_mult),
            Walk::Cascade(_) => self.begin_cascade_event(rate_mult, children_mult),
            Walk::CascadeBook(_) => self.begin_cascade_book_event(rate_mult, children_mult),
        }
    }

    /// One parent on the fingerprint-fitted walk: the Markov gap, the
    /// calendar and reopen clock steps, the GARCH mid, the bounce-placed book,
    /// the state-conditioned sweep and the price-repeat branch.
    fn begin_legacy_event(&mut self, rate_mult: f64, children_mult: f64) -> Result<(), TickFault> {
        let spec = Arc::clone(&self.spec);
        let dt_ns = ((self.next_duration_ns() as f64 / rate_mult).round() as u64).max(1);
        // Order is load-bearing: next_duration_ns reads the arrival multiplier at
        // the start of the gap (the clock has not advanced yet), then the clock
        // steps, then next_latent_mid reads the volatility multiplier at the
        // instant the trade prints. A duration belongs to the session window it
        // opens in; a trade's volatility belongs to the window it prints in. Do
        // not reorder these three lines to "tidy" them - it silently shifts which
        // session window each tick is attributed to.
        self.clock_ns = self.clock_ns.saturating_add(dt_ns);
        // Calendar closure is part of the clock step. Crossing consumers must
        // see its post-jump frontier, otherwise an arm inside the skipped span
        // is stranded forever once the frontier advances past it.
        if let Some(calendar) = &spec.calendar
            && !calendar.is_open(self.clock_ns)
        {
            self.clock_ns = calendar.next_open_ns(self.clock_ns);
        }
        if let Some(reopen) = self
            .regime
            .take_reopen_crossed(self.reopen_frontier_ns, self.clock_ns)
        {
            self.clock_ns = self.clock_ns.saturating_add(reopen.halt_ns);
            self.vol.mid = (self.vol.mid * reopen.gap_frac.exp())
                .max(spec.tick_f64)
                .min(MID_CEILING);
        }
        // An unscheduled halt can itself end inside the next scheduled close.
        // The calendar remains authoritative over whether a tick may print.
        if let Some(calendar) = &spec.calendar
            && !calendar.is_open(self.clock_ns)
        {
            self.clock_ns = calendar.next_open_ns(self.clock_ns);
        }
        self.reopen_frontier_ns = self.clock_ns;
        #[cfg(test)]
        self.draw_stages.push("latent_mid");
        let mid = self.next_latent_mid();
        #[cfg(test)]
        self.draw_stages.push("side_book");
        let (aggressor, fresh_book) = self.next_side_and_book(mid);
        // Sweep size is conditional on the arrival state, and the two branches
        // are constructed to reproduce the declared unconditional shape:
        //
        // - the mean, exactly, by the identity ARRIVAL_*_CHILDREN_MULT carry;
        // - the single-child fraction, by giving the active state whatever
        //   residual mass is left once the quiet state has contributed its own.
        //   The quiet mean is far enough below the declared mean that its
        //   mixture solve falls into the pure-geometric fallback of section
        //   2.3, so the quiet state's realized single fraction is 1 / (C_bar *
        //   QUIET_MULT); that is the term subtracted here. The `.max(0.0)`
        //   is the honest floor for a config where no residual is left - the
        //   quiet state alone then already over-supplies single-child events.
        //
        // The shape is re-solved per event rather than cached because a
        // `FlowSurge` may also be scaling `children_mean` right now. It is
        // arithmetic on five floats, no allocation; `self.shape` stays the
        // owner of `level_step_prob` and of the truncation counters the realism
        // gate reads, which is why they are folded back in below.
        // The branch is selected on the base configured mean, never the
        // surge-effective one, so no runtime path ever crosses between the
        // two arithmetics (the generator successor spec, 3.1). Above the
        // floor - base quiet mean over one child - the legacy identity
        // holds; the crypto anchor at 8.49 lives here always, surged or not.
        // At or below it, the legacy identity is broken by construction: the
        // quiet mean clamps to one, the active residual clamps to zero, and
        // any configured near-one mean generates ~1.44 with the single
        // fraction collapsed - the July MNQ fit failure. The floor-aware
        // solve re-derives both active parameters from the unconditional
        // targets at the current effective mean, preserving them exactly.
        let scalars = &spec.scalars;
        let last_quiet = legacy(&mut self.walk).arrival.last_quiet;
        let floor_branch = scalars.children_mean * ARRIVAL_QUIET_CHILDREN_MULT <= 1.0;
        let (state_mean, state_single_frac) = if !floor_branch {
            let state_children_mult = if last_quiet {
                ARRIVAL_QUIET_CHILDREN_MULT
            } else {
                ARRIVAL_ACTIVE_CHILDREN_MULT
            };
            let single = if last_quiet {
                scalars.children_single_frac
            } else {
                ((scalars.children_single_frac
                    - ARRIVAL_QUIET_FRACTION
                        / (scalars.children_mean * ARRIVAL_QUIET_CHILDREN_MULT))
                    / (1.0 - ARRIVAL_QUIET_FRACTION))
                    .max(0.0)
            };
            (
                scalars.children_mean * children_mult * state_children_mult,
                single,
            )
        } else {
            let effective_mean = scalars.children_mean * children_mult;
            let quiet_eff = effective_mean * ARRIVAL_QUIET_CHILDREN_MULT;
            let quiet_mean = quiet_eff.max(1.0);
            let quiet_single = if quiet_eff <= 1.0 {
                1.0
            } else {
                scalars.children_single_frac.max(1.0 / quiet_eff)
            };
            if last_quiet {
                (quiet_mean, quiet_single)
            } else {
                let active_mean = (effective_mean - ARRIVAL_QUIET_FRACTION * quiet_mean)
                    / (1.0 - ARRIVAL_QUIET_FRACTION);
                let active_single = (scalars.children_single_frac
                    - ARRIVAL_QUIET_FRACTION * quiet_single)
                    / (1.0 - ARRIVAL_QUIET_FRACTION);
                (active_mean, active_single)
            }
        };
        #[cfg(test)]
        self.draw_stages.push("child");
        let count = {
            // A single fraction at or above one means "always exactly one
            // child" - the floor branch's quiet state. The mixture solve
            // would divide by zero there, so it is expressed directly.
            let count = if state_single_frac >= 1.0 {
                // One uniform consumed, mirroring the mixture's q-branch
                // draw contract, so the always-one state has the same RNG
                // stride as a taken single-child mixture draw.
                let _always_one: f64 = self.rng.random();
                1
            } else {
                let mut event_shape = SweepShape::new(
                    state_mean,
                    state_single_frac,
                    // Levels are not drawn here: `step_child` walks the grid
                    // off `self.shape.level_step_prob`, which is solved once
                    // from the declared `levels_mean` and never
                    // state-conditioned. Any value in [1, mean] would do;
                    // 1.0 says "unused" plainly.
                    1.0,
                );
                let count = event_shape.next_count(&mut self.rng);
                self.shape.truncated += event_shape.truncated;
                count
            };
            self.shape.drawn += 1;
            count
        };
        let high_regime = legacy(&mut self.walk).bounce.high_regime;
        let repeat_draw = count == 1 && !high_regime && self.rng.sample(spec.coins.price_repeat);
        let walk = legacy(&mut self.walk);
        let compatible_repeat = repeat_draw
            && walk
                .last_event_price_ticks
                .zip(walk.last_book.as_ref())
                .is_some_and(|(price, frozen_book)| {
                    repeat_is_compatible(price, frozen_book, aggressor)
                });
        let (book, price_ticks) = if compatible_repeat {
            (
                walk.last_book
                    .clone()
                    .expect("compatible repeat has a prior book"),
                walk.last_event_price_ticks
                    .expect("compatible repeat has a prior price"),
            )
        } else {
            let price_ticks = placed_trade_ticks(&fresh_book, aggressor, spec.trade_bounce_ticks);
            (fresh_book, price_ticks)
        };
        #[cfg(test)]
        {
            self.repeat_draws += u64::from(repeat_draw);
            self.rejected_repeat_draws += u64::from(repeat_draw && !compatible_repeat);
        }
        self.plan_parent_ts = self.clock_ns;
        let quote = self.placed_quote(&book)?;
        self.plan.push(quote);
        legacy(&mut self.walk).last_book = Some(book);
        // The sweep's level-step gaps, the last main-stream stage of a parent
        // since tape protocol 36; sizes are drawn from their own stream.
        #[cfg(test)]
        self.draw_stages.push("level_steps");
        self.emit_children(SweepBurst {
            remaining: count,
            emitted: 0,
            parent_ts_ns: self.clock_ns,
            side: aggressor,
            price_ticks,
        })
    }

    /// One child of a placed sweep: the level step, the floor, the instant.
    /// Returns the level it prints at. A step landing while the price sits on
    /// the one-tick floor still counts, and still consumed its draw.
    fn step_child(&mut self, burst: &mut SweepBurst, steps: &mut LevelSteps) -> f64 {
        if steps.steps_at(burst.emitted, &mut self.rng) {
            match burst.side {
                AggressorSide::Buyer => burst.price_ticks += 1.0,
                AggressorSide::Seller => burst.price_ticks -= 1.0,
                AggressorSide::NoAggressor => unreachable!("parent side is native"),
            }
        }
        burst.price_ticks = burst.price_ticks.max(1.0);
        self.clock_ns = burst
            .parent_ts_ns
            .saturating_add(u64::from(burst.emitted).saturating_mul(INTRA_EVENT_STEP_NS));
        let price_ticks = burst.price_ticks;
        burst.remaining -= 1;
        burst.emitted += 1;
        if burst.remaining == 0 {
            // End of the sweep. The legacy walk remembers where it finished
            // (its repeat branch re-prices from here), and both bounce walks
            // re-center the drift on the residual between the last printed
            // level and the latent mid. `BounceState::next_drift` still takes
            // its per-event step, but it no longer accumulates across events -
            // see the field comment on `drift_ticks`. The cascade reads
            // neither, so it keeps neither.
            let recentred = (DRIFT_RECENTER_FRAC
                * (burst.price_ticks - self.vol.mid / self.spec.tick_f64))
                .round() as i64;
            match &mut self.walk {
                Walk::Legacy(walk) => {
                    walk.last_event_price_ticks = Some(burst.price_ticks);
                    walk.bounce.drift_ticks = recentred;
                }
                Walk::Integrated(walk) => walk.bounce.drift_ticks = recentred,
                Walk::Cascade(_) | Walk::CascadeBook(_) => {}
            }
        }
        price_ticks
    }
}
