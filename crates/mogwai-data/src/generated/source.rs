// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The core path-dependent walk: [`GeneratedSource`] composes the arrival
//! duration clock, the GARCH latent mid, the bounce/drift price process and
//! the optional regime overlay into the [`crate::TickSource`] the running
//! server drives. Same seed plus tape anchor yields the same stream byte for
//! byte - see `clean_regime_is_byte_identical` in the test module for the
//! pinned golden sequence that must be re-blessed with any intentional walk
//! mechanism change.

use std::sync::Arc;

use mogwai_protocol::{
    AggressorSide, InstrumentDef, MarketRegime, QuoteTick, TradeTick, decimal_to_f64,
};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{ChiSquared, Distribution, LogNormal, Normal, Weibull};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::{TickEvent, TickFault, TickSource};

use super::arrival::{
    ArrivalEnv, ArrivalKernel, ArrivalState, CadenceWalk, PendingReopen, RuntimeModifiers,
};
use super::calendar::SessionCalendar;
use super::consts::{
    ARRIVAL_ACTIVE_CHILDREN_MULT, ARRIVAL_MEAN_CAL, ARRIVAL_QUIET_ACTIVE_RATIO,
    ARRIVAL_QUIET_CHILDREN_MULT, ARRIVAL_QUIET_FRACTION, ARRIVAL_STATE_PERSISTENCE,
    ARRIVAL_WEIBULL_MEAN, ARRIVAL_WEIBULL_SHAPE, DRIFT_RECENTER_FRAC, EVENT_PRICE_REPEAT_PROB,
    HIGH_REGIME_LEVEL_STEP_MULT, INTRA_EVENT_STEP_NS, LOW_INTENSITY_ARR_MULT,
    LOW_REGIME_LEVEL_STEP_MULT, MAX_SESSION_GAP_NS, MID_CEILING, NS_PER_HOUR,
    REALIZED_RETURN_CEILING, SIZE_DECIMALS, STUDENT_T_DF, STUDENT_T_UNIT_SCALE,
};
use super::dynamics::{
    ArrivalClock, BounceState, GarchVol, SweepBurst, SweepShape, draw_student_t,
};
use super::fingerprint::{Fingerprint, GeneratedSourceError, GeneratorScalars, SessionProfile};
use super::numeric::{decimal_from_f64, round_lot_size};
use super::quote::{PublishedBook, book_mid_ticks, place_book};
use super::regime::RegimeState;
use super::session::SessionModulator;

/// `Clone` is the substrate of the checkpointed seek (`CheckpointIndex`): the
/// generator is a path-dependent walk whose entire future is a pure function of
/// its current state, so a clone taken at tick N, advanced, reproduces ticks
/// N+1, N+2, ... byte-for-byte. Every field is `Clone`, including the
/// `ChaCha12Rng` (rand's `StdRng` is the same cipher but dropped `Clone`, which is
/// why the generator holds `ChaCha12Rng` directly).
#[derive(Clone)]
pub struct GeneratedSource {
    scalars: Arc<GeneratorScalars>,
    symbol: Arc<str>,
    rng: ChaCha12Rng,
    pub(super) clock_ns: u64,
    arrival: ArrivalClock,
    arrival_override: Option<bool>,
    arrival_kernel: Option<ArrivalKernel>,
    arrival_state: Option<ArrivalState>,
    arrival_env: Option<ArrivalEnv>,
    cadence_rng: Option<ChaCha12Rng>,
    fault: Option<TickFault>,
    pub(super) vol: GarchVol,
    session: SessionModulator,
    // `pub(super)` for the same reason as `vol` and `clock_ns`: the sibling test
    // module reads the trade-displacement state directly to pin that it never
    // varies, which is the diagnosis that gives the dynamic-displacement work
    // something to move against.
    pub(super) bounce: BounceState,
    duration_dist: Weibull<f64>,
    normal: Normal<f64>,
    chi_squared: ChiSquared<f64>,
    size_dist: LogNormal<f64>,
    pub(super) size_median: f64,
    size_grid: SizeGrid,
    calendar: Option<Arc<SessionCalendar>>,
    tick_f64: f64,
    regime: RegimeState,
    // The upper bound of the last instant span an armed `ReopenGap` was tested
    // against. It is NOT `clock_ns`: children advance the clock past their
    // parent, so starting the next test at the clock would leave the whole
    // intra-burst span untested, and an arm inside it could never satisfy the
    // crossing test again. Kept as its own field so the tested spans are
    // contiguous by CONSTRUCTION rather than by every clock mutation
    // remembering to be included.
    reopen_frontier_ns: u64,
    pub(super) shape: SweepShape,
    pub(super) burst: SweepBurst,
    last_event_price_ticks: Option<f64>,
    pending_quote: Option<QuoteTick>,
    last_book: Option<PublishedBook>,
    surge: Option<SurgeWindow>,
    // The observation-only volatility trace (the generator successor spec
    // 3.3): populated per parent event when enabled, consuming NO draws
    // and changing NOTHING about the walk, which the byte-identity test
    // pins. `take_vol_trace` hands the last event's record to the offline
    // forensic probe.
    vol_trace_enabled: bool,
    vol_trace: Option<VolTrace>,
    #[cfg(test)]
    pub(super) latent_updates: u64,
    #[cfg(test)]
    pub(super) repeat_draws: u64,
    #[cfg(test)]
    pub(super) rejected_repeat_draws: u64,
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
/// without constructing protocol decimals or cloning the symbol. The regular
/// [`TickSource`] path uses the same advancement primitives and materializes
/// the wire objects afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParentSummary {
    pub parent_ts_ns: u64,
    pub child_count: u32,
    pub child_stride_ns: u64,
}

/// One parent event's volatility intermediates, observed off the REAL
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

#[derive(Clone, Copy)]
struct SizeDraw {
    base: f64,
    lot_snap: bool,
}

/// Everything one child print needs, captured at the instant the child is
/// stochastically resolved.
///
/// `price_ticks` is carried rather than re-read from `self.burst` after the
/// fact. The two agree today only because `step_child`'s tail happens not to
/// touch `price_ticks`, and a future change to that tail - the sweep-end
/// re-centring is already there and already writes neighbouring fields - would
/// otherwise silently move every child's printed price by one step.
#[derive(Clone, Copy)]
struct ChildDraw {
    size: SizeDraw,
    price_ticks: f64,
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

    /// The grid an instrument's OWN SIZING states, rather than one inferred
    /// from its class.
    ///
    /// This read `is_future()` and hardcoded whole contracts, which was right
    /// while every derivative was an exchange-listed future. It stopped being
    /// right when perpetuals landed: a crypto perp sizes fractionally - Binance
    /// quotes BTCUSDT.P in thousandths - so a class-derived grid would refuse
    /// the most common perpetual on the largest venue while claiming its
    /// generator config was invalid.
    ///
    /// Deriving from `size_increment` and `size_precision` moves NO existing
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
}

#[derive(Clone)]
struct SurgeWindow {
    start_ns: u64,
    end_ns: u64,
    rate_mult: f64,
    children_mult: f64,
}

impl GeneratedSource {
    #[cfg(test)]
    pub(super) fn shares_immutable_config_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.scalars, &other.scalars)
            && match (&self.calendar, &other.calendar) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
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

    // The only fallible inputs are `scalars`/`session`/`calendar`, guarded by
    // the `?`-propagated `validate` calls at the top. The distribution
    // constructors below (`LogNormal`/`Weibull`/`Normal`/`ChiSquared`) take
    // compile-time constants that are always valid params, so their `expect`s cannot fire -
    // `unwrap_in_result` is silenced here because there is no meaningful error
    // variant to map them onto, not because a failure is being swallowed.
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
        for size in [scalars.top_sizes.bid, scalars.top_sizes.ask] {
            if size < size_grid.min_size || (size_grid.integral && size.fract() != Decimal::ZERO) {
                return Err(GeneratedSourceError::Scalar(
                    super::fingerprint::ScalarError { field: "top_sizes" },
                ));
            }
        }
        if let Some(calendar) = &calendar {
            calendar
                .validate()
                .map_err(GeneratedSourceError::Calendar)?;
        }
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
        let size_dist =
            LogNormal::new(size_median.ln(), scalars.size_log_sigma).expect("valid lognormal");
        let shape = SweepShape::new(
            scalars.children_mean,
            scalars.children_single_frac,
            scalars.levels_mean,
        );
        // Built before the struct literal because it borrows `scalars.symbol`,
        // which the literal moves; `start_ts` is the tape anchor RegimeState
        // needs to fail-close an already-elapsed ReopenGap.
        let regime = RegimeState::new(regime, start_ts, &scalars.symbol);
        let cadence = CadenceWalk::assemble(
            &scalars,
            session,
            calendar.as_ref(),
            regime.arrival_thin,
            seed,
            start_ts,
        );
        let trade_bounce_ticks = scalars.trade_displacement_ticks.ticks();
        let symbol = Arc::from(scalars.symbol.as_str());
        Ok(Self {
            tick_f64: decimal_to_f64(scalars.modal_tick),
            scalars: Arc::new(scalars),
            symbol,
            rng: ChaCha12Rng::seed_from_u64(seed),
            clock_ns: start_ts,
            arrival: ArrivalClock {
                active_mean_s,
                quiet_mean_s: active_mean_s * quiet_active_ratio,
                quiet_to_active: switch_rate * (1.0 - quiet_fraction),
                active_to_quiet: switch_rate * quiet_fraction,
                quiet: false,
                last_quiet: false,
            },
            arrival_override: None,
            arrival_kernel: cadence.kernel,
            arrival_state: cadence.state,
            arrival_env: cadence.env,
            cadence_rng: cadence.rng,
            fault: None,
            vol,
            session: SessionModulator::new(session, calendar.as_ref()),
            bounce: BounceState {
                // Every realization opens on the Buyer side regardless of seed:
                // `prev_side` seeds to Buyer and the low regime flips it only at
                // BOUNCE_LOW_FLIP_PROB (0.02), so a fresh stream prints a long
                // Buyer run before the first flip. This seed-independent opening
                // bias is deliberate and left as-is: it is a start-of-stream
                // transient (a real bounce process is equally free to open either
                // way), it is fully deterministic, and re-seeding the side would
                // consume an extra RNG draw and break the committed fingerprint's
                // byte-identical golden stream for zero fidelity gain.
                prev_side: AggressorSide::Buyer,
                high_regime: false,
                drift_ticks: 0,
                drift_dir: 1,
                drift_hot: false,
                trade_bounce_ticks,
            },
            duration_dist: Weibull::new(1.0, ARRIVAL_WEIBULL_SHAPE).expect("valid weibull"),
            normal: Normal::new(0.0, 1.0).expect("valid normal"),
            chi_squared: ChiSquared::new(STUDENT_T_DF).expect("valid chi-squared"),
            size_dist,
            size_median,
            size_grid,
            calendar: calendar.map(Arc::new),
            regime,
            reopen_frontier_ns: start_ts,
            shape,
            burst: SweepBurst::empty(),
            last_event_price_ticks: None,
            pending_quote: None,
            last_book: None,
            surge: None,
            vol_trace_enabled: false,
            vol_trace: None,
            #[cfg(test)]
            latent_updates: 0,
            #[cfg(test)]
            repeat_draws: 0,
            #[cfg(test)]
            rejected_repeat_draws: 0,
        })
    }

    /// The simulated instant the generator has reached: the `ts_event` of the
    /// last emitted tick, i.e. the clock the NEXT `next_tick` advances from. A
    /// fresh source sits at its `start_ts`. `CheckpointIndex` uses this to place
    /// snapshots and to binary-search them against a seek target.
    #[must_use]
    pub fn clock_ns(&self) -> u64 {
        self.clock_ns
    }

    /// The price of the last TRADE this walk has emitted, or `None` for a walk
    /// that has printed nothing yet. Mid-burst it is the current sweep's latest
    /// child print; at a parent boundary it is the completed event's final
    /// print. Both are state the walk already carries - no draw is consumed and
    /// nothing about the stream moves.
    ///
    /// It exists for the checkpoint walk-back's FENCE case: a `FlowSurge`
    /// control-boundary snapshot may have CONSUMED the last print at or before
    /// a reader's target, and resuming any earlier snapshot would replay across
    /// the arm and answer from a different tape. The snapshot itself still
    /// knows that print, and this is how it says so. Without it, a settlement
    /// instant landing between a control boundary and the next trade is
    /// unpriceable forever and the sweep frontier stalls permanently.
    #[must_use]
    pub fn last_trade_price(&self) -> Option<Decimal> {
        let price_ticks = if self.burst.emitted > 0 {
            // `step_child` leaves `burst.price_ticks` at the price of the child
            // it just drew, so between ticks this IS the latest print. A
            // completed burst keeps `emitted > 0` until the next parent begins,
            // where it agrees with `last_event_price_ticks` by construction.
            Some(self.burst.price_ticks)
        } else {
            self.last_event_price_ticks
        };
        // The exact materialization `next_child` performs, so the answer is
        // byte-identical to the print a replay would have re-emitted.
        price_ticks.map(|ticks| {
            decimal_from_f64(ticks * self.tick_f64).round_dp(self.scalars.price_decimals)
        })
    }

    #[cfg(test)]
    pub(super) fn arrival_mult_for_test(&self, clock_ns: u64) -> f64 {
        self.session.arrival_mult(clock_ns)
    }

    #[cfg(test)]
    pub(super) fn vol_mult_for_test(&self, clock_ns: u64) -> f64 {
        self.session.vol_mult(clock_ns)
    }

    /// Force one arrival state for the committed tick-composition measurement.
    /// `None` restores the natural Markov chain. This is hidden from generated
    /// API documentation and is public only because Cargo examples compile as
    /// external crates; it is not a venue configuration seam.
    #[doc(hidden)]
    pub fn set_arrival_quiet_for_measurement(&mut self, quiet: Option<bool>) {
        self.arrival_override = quiet;
    }

    fn next_duration_ns(&mut self) -> u64 {
        if let Some(quiet) = self.arrival_override {
            self.arrival.quiet = quiet;
        }
        let raw_eps = self.duration_dist.sample(&mut self.rng);
        let state_mean = if self.arrival.quiet {
            self.arrival.quiet_mean_s
        } else {
            self.arrival.active_mean_s
        };
        let duration_s = (state_mean * raw_eps / ARRIVAL_WEIBULL_MEAN).max(0.000_000_001);
        let flip_probability = if self.arrival.quiet {
            self.arrival.quiet_to_active
        } else {
            self.arrival.active_to_quiet
        };
        self.arrival
            .advance_after_gap(self.rng.random_bool(flip_probability));
        if let Some(quiet) = self.arrival_override {
            self.arrival.quiet = quiet;
        }
        let arr_mult = self.session.arrival_mult(self.clock_ns);
        if arr_mult >= LOW_INTENSITY_ARR_MULT {
            // Open-market path: the multiplier sampled at the instant the gap
            // opens stretches the whole draw - the original math, unchanged
            // bit for bit so the committed fingerprint's golden stream stays
            // byte-identical (its multipliers never go near the gate). The
            // trailing `.min` is a pure safety rail: for any multiplier above
            // the gate and the validated thin_factor range it is far out of
            // reach of realistic draws, so it never alters an open-market gap;
            // it only guarantees the cast can never saturate to u64::MAX.
            //
            // Note the asymmetry with the closed-window path below, which
            // integrates the session envelope hour by hour: here an armed
            // `LiquidityDrought` stretch stays a ONCE-SAMPLED multiplier on the
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

    /// Wall-clock gap for a duration draw whose gap OPENS inside a closed
    /// session window (arrival multiplier below `LOW_INTENSITY_ARR_MULT`).
    ///
    /// The open-market path in `next_duration_ns` samples the arrival
    /// multiplier once and stretches the entire draw by `1/mult` - fine while
    /// the multiplier is O(1), catastrophic when a share is near zero: the
    /// stretched gap wildly overshoots the closed window (share 1e-6 turns a
    /// ~7 s draw into ~80 days) and an extreme share saturates the f64->u64
    /// cast at u64::MAX, pinning the parent clock there so every later parent
    /// carries the same `ts_event`. Parent advancement is the real invariant:
    /// quote/first-child and sibling ties are intentional, while distinct
    /// parents must advance until the u64 representation itself is exhausted.
    ///
    /// Here the draw is instead treated as a BUDGET of un-modulated seconds
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
    /// - only gaps that OPEN below the gate take this path. A gap opening in
    ///   an open hour still crosses a later closed window at its open-hour
    ///   rate (a tick can print inside the closed window). That artifact
    ///   predates this path and is left in place deliberately: fixing it would
    ///   change every boundary-crossing gap and break the committed
    ///   fingerprint's byte-identical golden stream.
    /// - a profile whose EVERY hour is effectively closed can never spend the
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
            let rate = self.session.arrival_mult(pos_ns) / self.regime.arrival_thin;
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
        let innovation_raw = draw_student_t(&mut self.rng, &self.normal, &self.chi_squared);
        let innovation = innovation_raw / STUDENT_T_UNIT_SCALE;
        // The BASE process is regime-independent by construction. `step` applies
        // the GARCH-state rail and the feedback rail, neither of which takes a
        // regime multiplier, so an armed storm cannot raise the state ceiling
        // and thereby alter every later recursion step. GARCH feedback sees the
        // un-modulated return, so volatility clustering is identical armed or
        // not; the envelopes below scale only what is realized.
        let mid_before = self.vol.mid;
        let step = self.vol.step(innovation);
        let base_return = step.base_return;
        // Vol composition convention (see also RegimeState::vol_mult): the
        // session envelope and the regime envelope COMPOSE MULTIPLICATIVELY here
        // (session 1.0 = no session bias, regime 1.0 = no regime bias, so the
        // product is the combined RMS scale). Inside the regime envelope a
        // SessionEdgeSpike instead composes ADDITIVELY onto the storm baseline
        // (vol_mult + edge_mult). The two conventions are intentional and rely on
        // both neutral values being 1.0; do NOT restructure either into the other
        // (a future regime that set both vol_mult and an edge spike would want the
        // add re-examined - today the match is exclusive so only one is non-unit).
        let session_vol_mult = self.session.vol_mult(self.clock_ns);
        let regime_vol_mult = self.regime.vol_mult(self.clock_ns);
        let vol_mult = session_vol_mult * regime_vol_mult;
        // REALIZED ceiling: ABSOLUTE, and deliberately not scaled by anything.
        // Every regime - VolStorm, SessionEdgeSpike, drought, reopen - and the
        // session curve reach the mid only through `vol_mult` above, and then
        // meet the same fixed ceiling. Scaling this by the regime is what used
        // to let a maximum-strength storm's permitted single-event move ride on
        // whatever the base rail happened to be; sizing it as a policy against
        // the measured envelope is what replaced that.
        //
        // Bounds ONE event. Cumulative movement is not bounded by it: a
        // sustained storm walks the mid as far as its duration allows.
        let pre_realized = base_return * vol_mult;
        let return_n = pre_realized.clamp(-REALIZED_RETURN_CEILING, REALIZED_RETURN_CEILING);
        self.vol.mid = (self.vol.mid * return_n.exp())
            .max(self.tick_f64)
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

    fn next_side_and_book(&mut self, mid: f64) -> (AggressorSide, PublishedBook) {
        let side = self.bounce.next_side(&mut self.rng);
        self.bounce.next_drift(&mut self.rng);
        let drifted_mid_ticks = mid / self.tick_f64 + self.bounce.drift_ticks as f64;
        let book = place_book(
            drifted_mid_ticks,
            &self.scalars.quoted_width,
            &self.scalars.top_sizes,
        );
        (side, book)
    }

    fn draw_size(&mut self) -> SizeDraw {
        let base = self.size_dist.sample(&mut self.rng).max(f64::MIN_POSITIVE);
        let lot_snap = self.rng.random_bool(self.scalars.size_round_frac);
        SizeDraw { base, lot_snap }
    }

    fn materialize_size(&self, draw: SizeDraw) -> Decimal {
        let SizeDraw { base, lot_snap } = draw;
        if self.size_grid.integral {
            let raw = if lot_snap {
                // The lot on an integral grid is at least one contract, so a
                // median below ten contracts snaps to whole contracts rather
                // than to a fractional lot.
                let lot = integral_lot(self.size_median);
                decimal_from_f64((base / lot).round() * lot)
            } else {
                decimal_from_f64(base)
            };
            // Half-away-from-zero, not truncation (which biases every draw down
            // and piles the whole sub-1.0 mass onto the floor twice over) and
            // not banker's rounding (which measurably thins the odd sizes on a
            // 0.5-heavy discrete grid).
            return raw
                .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
                .max(self.size_grid.min_size);
        }
        // The spot arm is the pre-size-grid expression, character for
        // character: the lot-snap branch is NOT re-rounded to SIZE_DECIMALS,
        // because it never was, and `spot_draws_are_bit_identical_...` is only
        // provable if this arm is left alone rather than merely made similar.
        let size = if lot_snap {
            round_lot_size(base, self.size_median)
        } else {
            decimal_from_f64(base).round_dp(SIZE_DECIMALS)
        };
        size.max(self.size_grid.min_size)
    }

    #[cfg(test)]
    pub(super) fn next_size(&mut self) -> Decimal {
        let draw = self.draw_size();
        self.materialize_size(draw)
    }
}

/// Round-lot scale selected from the latent distribution center. Keeping this
/// tiny derivation named makes its decade transitions directly testable: the
/// old crypto notional proxy pinned it at one and left every larger-contract
/// branch live but unexercised.
pub(super) fn integral_lot(latent_size_median: f64) -> f64 {
    10_f64.powf(latent_size_median.log10().floor()).max(1.0)
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

impl TickSource for GeneratedSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        if self.fault.is_some() {
            return None;
        }
        if let Some(quote) = self.pending_quote.take() {
            return Some(TickEvent::Quote(quote));
        }
        if self.burst.remaining == 0 {
            self.begin_event(true);
            return self.pending_quote.take().map(TickEvent::Quote);
        }
        Some(TickEvent::Trade(self.next_child()))
    }

    fn arm_flow_surge(
        &mut self,
        start_ns: u64,
        duration_ms: u64,
        rate_mult: f64,
        children_mult: f64,
    ) {
        self.surge = Some(SurgeWindow {
            start_ns,
            end_ns: start_ns.saturating_add(duration_ms.saturating_mul(1_000_000)),
            rate_mult,
            children_mult,
        });
    }

    fn clear_flow_surge(&mut self) {
        self.surge = None;
    }

    fn fault(&self) -> Option<TickFault> {
        self.fault
    }

    fn seek_to(&mut self, start_ts: u64) -> Option<TickEvent> {
        loop {
            if self.pending_quote.is_none() && self.burst.remaining == 0 {
                let mut advanced = self.clone();
                let parent = advanced.advance_parent();
                let parent_end = parent.parent_ts_ns.saturating_add(
                    u64::from(parent.child_count.saturating_sub(1))
                        .saturating_mul(parent.child_stride_ns),
                );
                if parent_end < start_ts {
                    *self = advanced;
                    continue;
                }
            }
            let tick = self.next_tick()?;
            if tick.ts_event() >= start_ts {
                return Some(tick);
            }
        }
    }
}

impl GeneratedSource {
    pub(super) fn at_parent_boundary(&self) -> bool {
        self.pending_quote.is_none() && self.burst.remaining == 0
    }

    /// Advance exactly one parent and all of its children without constructing
    /// protocol objects.
    ///
    /// Every random draw belongs to the advancement side of the seam. In
    /// particular, size draws and child price evolution still run even though
    /// their `Decimal` representation is discarded. Starting this operation in
    /// the middle of a wire parent would make its compact run ambiguous, so the
    /// boundary is enforced rather than silently repaired.
    #[must_use]
    pub fn advance_parent(&mut self) -> ParentSummary {
        assert!(
            self.pending_quote.is_none() && self.burst.remaining == 0,
            "advance_parent requires a parent boundary"
        );
        self.begin_event(false);
        let summary = ParentSummary {
            parent_ts_ns: self.burst.parent_ts_ns,
            child_count: self.burst.remaining,
            child_stride_ns: INTRA_EVENT_STEP_NS,
        };
        while self.burst.remaining > 0 {
            self.step_child();
        }
        summary
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
            self.arrival_kernel.is_some(),
            "arrival transcript requires an integrated kernel"
        );
        let summary = self.advance_parent();
        let latent_x = self
            .arrival_kernel
            .expect("integrated branch has kernel")
            .latent_x(
                self.arrival_state
                    .as_ref()
                    .expect("integrated branch has state"),
            )
            .to_bits();
        (summary, latent_x)
    }

    #[cfg(test)]
    pub(super) fn diagnostic_place_book(&self) -> PublishedBook {
        place_book(
            self.vol.mid / self.tick_f64 + self.bounce.drift_ticks as f64,
            &self.scalars.quoted_width,
            &self.scalars.top_sizes,
        )
    }

    fn begin_integrated_event(
        &mut self,
        materialize_quote: bool,
        rate_mult: f64,
        children_mult: f64,
    ) {
        let kernel = self.arrival_kernel.expect("integrated branch has kernel");
        // The kernel tests the crossing against its own walk start, which is the
        // clock AFTER the previous burst's children stepped it past their
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
        let draw = match kernel.next_parent(
            self.arrival_state
                .as_mut()
                .expect("integrated branch has state"),
            self.clock_ns,
            super::arrival::cadence_base_mean_s(&self.scalars),
            &self.shape,
            self.arrival_env
                .as_ref()
                .expect("integrated branch has environment"),
            RuntimeModifiers {
                rate_mult,
                children_mult,
                pending_reopen,
            },
            self.cadence_rng
                .as_mut()
                .expect("integrated branch has cadence rng"),
        ) {
            Ok(draw) => draw,
            Err(refusal) => {
                self.fault = Some(TickFault::Arrival(refusal));
                return;
            }
        };
        self.clock_ns = draw.parent_ts_ns;
        if draw.reopen_applied {
            let reopen = self
                .regime
                .take_reopen_crossed(self.reopen_frontier_ns, draw.parent_ts_ns)
                .expect("arrival kernel and regime disagree about reopen crossing");
            self.vol.mid = (self.vol.mid * reopen.gap_frac.exp())
                .max(self.tick_f64)
                .min(MID_CEILING);
        }
        self.reopen_frontier_ns = self.clock_ns;
        let mid = self.next_latent_mid();
        let (aggressor, book) = self.next_side_and_book(mid);
        let price_ticks = match aggressor {
            AggressorSide::Buyer => {
                (book_mid_ticks(&book) + self.bounce.trade_bounce_ticks).round()
            }
            AggressorSide::Seller => {
                (book_mid_ticks(&book) - self.bounce.trade_bounce_ticks).round()
            }
            AggressorSide::NoAggressor => unreachable!("bounce only emits buyer or seller"),
        }
        .max(1.0);
        self.pending_quote = materialize_quote.then(|| QuoteTick {
            symbol: Arc::clone(&self.symbol),
            bid_px: book.bid_price(self.tick_f64, self.scalars.price_decimals),
            ask_px: book.ask_price(self.tick_f64, self.scalars.price_decimals),
            bid_sz: book.bid_size,
            ask_sz: book.ask_size,
            ts_event: self.clock_ns,
        });
        self.last_book = Some(book);
        self.burst = SweepBurst {
            remaining: draw.child_count,
            emitted: 0,
            parent_ts_ns: self.clock_ns,
            side: aggressor,
            price_ticks,
        };
    }

    fn begin_event(&mut self, materialize_quote: bool) {
        let (rate_mult, children_mult) = self
            .surge
            .as_ref()
            .filter(|window| (window.start_ns..window.end_ns).contains(&self.clock_ns))
            .map_or((1.0, 1.0), |window| {
                (window.rate_mult, window.children_mult)
            });
        if self.arrival_kernel.is_some() {
            self.begin_integrated_event(materialize_quote, rate_mult, children_mult);
            return;
        }
        let dt_ns = ((self.next_duration_ns() as f64 / rate_mult).round() as u64).max(1);
        // Order is load-bearing: next_duration_ns reads the arrival multiplier at
        // the START of the gap (the clock has not advanced yet), then the clock
        // steps, then next_latent_mid reads the volatility multiplier at the
        // instant the trade PRINTS. A duration belongs to the session window it
        // opens in; a trade's volatility belongs to the window it prints in. Do
        // not reorder these three lines to "tidy" them - it silently shifts which
        // session window each tick is attributed to.
        self.clock_ns = self.clock_ns.saturating_add(dt_ns);
        // Calendar closure is part of the clock step. Crossing consumers must
        // see its post-jump frontier, otherwise an arm inside the skipped span
        // is stranded forever once the frontier advances past it.
        if let Some(calendar) = &self.calendar
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
                .max(self.tick_f64)
                .min(MID_CEILING);
        }
        // An unscheduled halt can itself end inside the next scheduled close.
        // The calendar remains authoritative over whether a tick may print.
        if let Some(calendar) = &self.calendar
            && !calendar.is_open(self.clock_ns)
        {
            self.clock_ns = calendar.next_open_ns(self.clock_ns);
        }
        self.reopen_frontier_ns = self.clock_ns;
        let mid = self.next_latent_mid();
        let (aggressor, fresh_book) = self.next_side_and_book(mid);
        // Sweep size is conditional on the arrival state, and the two branches
        // are constructed to reproduce the DECLARED unconditional shape:
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
        // The branch is selected on the BASE configured mean, never the
        // surge-effective one, so no runtime path ever crosses between the
        // two arithmetics (the generator successor spec, 3.1). Above the
        // floor - base quiet mean over one child - the legacy identity
        // holds and runs byte-for-byte; the crypto anchor at 8.49 lives
        // here always, surged or not. At or below it, the legacy identity
        // is broken by construction: the quiet mean clamps to one, the
        // active residual clamps to zero, and any configured near-one mean
        // generates ~1.44 with the single fraction collapsed - the July
        // MNQ fit failure. The floor-aware solve re-derives both active
        // parameters from the unconditional TARGETS at the current
        // effective mean, preserving them exactly.
        let floor_branch = self.scalars.children_mean * ARRIVAL_QUIET_CHILDREN_MULT <= 1.0;
        let (state_mean, state_single_frac) = if !floor_branch {
            let state_children_mult = if self.arrival.last_quiet {
                ARRIVAL_QUIET_CHILDREN_MULT
            } else {
                ARRIVAL_ACTIVE_CHILDREN_MULT
            };
            let single = if self.arrival.last_quiet {
                self.scalars.children_single_frac
            } else {
                ((self.scalars.children_single_frac
                    - ARRIVAL_QUIET_FRACTION
                        / (self.scalars.children_mean * ARRIVAL_QUIET_CHILDREN_MULT))
                    / (1.0 - ARRIVAL_QUIET_FRACTION))
                    .max(0.0)
            };
            (
                self.scalars.children_mean * children_mult * state_children_mult,
                single,
            )
        } else {
            let effective_mean = self.scalars.children_mean * children_mult;
            let quiet_eff = effective_mean * ARRIVAL_QUIET_CHILDREN_MULT;
            let quiet_mean = quiet_eff.max(1.0);
            let quiet_single = if quiet_eff <= 1.0 {
                1.0
            } else {
                self.scalars.children_single_frac.max(1.0 / quiet_eff)
            };
            if self.arrival.last_quiet {
                (quiet_mean, quiet_single)
            } else {
                let active_mean = (effective_mean - ARRIVAL_QUIET_FRACTION * quiet_mean)
                    / (1.0 - ARRIVAL_QUIET_FRACTION);
                let active_single = (self.scalars.children_single_frac
                    - ARRIVAL_QUIET_FRACTION * quiet_single)
                    / (1.0 - ARRIVAL_QUIET_FRACTION);
                (active_mean, active_single)
            }
        };
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
                    // Levels are not drawn here: `next_child` walks the grid
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
        let repeat_draw =
            count == 1 && !self.bounce.high_regime && self.rng.random_bool(EVENT_PRICE_REPEAT_PROB);
        let compatible_repeat = repeat_draw
            && self
                .last_event_price_ticks
                .zip(self.last_book.as_ref())
                .is_some_and(|(price, frozen_book)| {
                    repeat_is_compatible(price, frozen_book, aggressor)
                });
        #[cfg(test)]
        {
            self.repeat_draws += u64::from(repeat_draw);
            self.rejected_repeat_draws += u64::from(repeat_draw && !compatible_repeat);
        }
        let book = if compatible_repeat {
            self.last_book
                .clone()
                .expect("compatible repeat has a prior book")
        } else {
            fresh_book
        };
        let normal_price_ticks = match aggressor {
            AggressorSide::Buyer => {
                (book_mid_ticks(&book) + self.bounce.trade_bounce_ticks).round()
            }
            AggressorSide::Seller => {
                (book_mid_ticks(&book) - self.bounce.trade_bounce_ticks).round()
            }
            AggressorSide::NoAggressor => unreachable!("bounce only emits buyer or seller"),
        }
        .max(1.0);
        let price_ticks = if compatible_repeat {
            self.last_event_price_ticks
                .expect("compatible repeat has a prior price")
        } else {
            normal_price_ticks
        };
        self.pending_quote = materialize_quote.then(|| QuoteTick {
            symbol: Arc::clone(&self.symbol),
            bid_px: book.bid_price(self.tick_f64, self.scalars.price_decimals),
            ask_px: book.ask_price(self.tick_f64, self.scalars.price_decimals),
            bid_sz: book.bid_size,
            ask_sz: book.ask_size,
            ts_event: self.clock_ns,
        });
        self.last_book = Some(book);
        self.burst = SweepBurst {
            remaining: count,
            emitted: 0,
            parent_ts_ns: self.clock_ns,
            side: aggressor,
            price_ticks,
        };
    }

    fn step_child(&mut self) -> ChildDraw {
        let level_step_prob = if self.bounce.high_regime {
            (self.shape.level_step_prob * HIGH_REGIME_LEVEL_STEP_MULT).min(1.0)
        } else {
            self.shape.level_step_prob * LOW_REGIME_LEVEL_STEP_MULT
        };
        if self.burst.emitted > 0 && self.rng.random_bool(level_step_prob) {
            match self.burst.side {
                AggressorSide::Buyer => self.burst.price_ticks += 1.0,
                AggressorSide::Seller => self.burst.price_ticks -= 1.0,
                AggressorSide::NoAggressor => unreachable!("parent side is native"),
            }
        }
        self.burst.price_ticks = self.burst.price_ticks.max(1.0);
        self.clock_ns = self
            .burst
            .parent_ts_ns
            .saturating_add(u64::from(self.burst.emitted).saturating_mul(INTRA_EVENT_STEP_NS));
        let draw = ChildDraw {
            size: self.draw_size(),
            price_ticks: self.burst.price_ticks,
        };
        self.burst.remaining -= 1;
        self.burst.emitted += 1;
        if self.burst.remaining == 0 {
            // End of the sweep: remember where it finished (the repeat branch
            // in `begin_event` re-prices from here) and RE-CENTRE the bounce
            // drift on the residual between the last printed level and the
            // latent mid. `BounceState::next_drift` still takes its per-event
            // step, but it no longer accumulates across events - see the field
            // comment on `drift_ticks`.
            self.last_event_price_ticks = Some(self.burst.price_ticks);
            self.bounce.drift_ticks = (DRIFT_RECENTER_FRAC
                * (self.burst.price_ticks - self.vol.mid / self.tick_f64))
                .round() as i64;
        }
        draw
    }

    fn next_child(&mut self) -> TradeTick {
        let draw = self.step_child();
        TradeTick {
            symbol: Arc::clone(&self.symbol),
            price: decimal_from_f64(draw.price_ticks * self.tick_f64)
                .round_dp(self.scalars.price_decimals),
            size: self.materialize_size(draw.size),
            aggressor: self.burst.side,
            ts_event: self.clock_ns,
        }
    }
}
