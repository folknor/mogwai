// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The core path-dependent walk: [`GeneratedSource`] composes the arrival
//! duration clock, the GARCH latent mid, the bounce/drift price process and
//! the optional regime overlay into the [`crate::TickSource`] the running
//! server drives. Same seed plus tape anchor yields the same stream byte for
//! byte - see `clean_regime_is_byte_identical` in the test module for the
//! pinned golden sequence that must be re-blessed with any intentional walk
//! mechanism change.

use mogwai_protocol::{AggressorSide, InstrumentDef, MarketRegime, TradeTick, decimal_to_f64};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{ChiSquared, Distribution, LogNormal, Normal, Weibull};
use rust_decimal::{Decimal, RoundingStrategy};

use crate::{TickEvent, TickSource};

use super::calendar::SessionCalendar;
use super::consts::{
    ARRIVAL_ACTIVE_CHILDREN_MULT, ARRIVAL_MEAN_CAL, ARRIVAL_QUIET_ACTIVE_RATIO,
    ARRIVAL_QUIET_CHILDREN_MULT, ARRIVAL_QUIET_FRACTION, ARRIVAL_STATE_PERSISTENCE,
    ARRIVAL_WEIBULL_MEAN, ARRIVAL_WEIBULL_SHAPE, DRIFT_RECENTER_FRAC, EVENT_PRICE_REPEAT_PROB,
    GARCH_SIGMA_CAP, HALF_SPREAD_TICKS, HIGH_REGIME_LEVEL_STEP_MULT, INTRA_EVENT_STEP_NS,
    LOW_REGIME_LEVEL_STEP_MULT, MAX_ABS_RETURN, MAX_SESSION_GAP_NS, MID_CEILING, NS_PER_HOUR,
    SESSION_CLOSED_ARR_MULT, SIZE_DECIMALS, SIZE_LOG_SIGMA, STUDENT_T_DF,
};
use super::dynamics::{ArrivalClock, BounceState, GarchVol, SweepBurst, SweepShape};
use super::fingerprint::{Fingerprint, GeneratedSourceError, GeneratorScalars, SessionProfile};
use super::numeric::{decimal_from_f64, round_lot_size};
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
    scalars: GeneratorScalars,
    rng: ChaCha12Rng,
    pub(super) clock_ns: u64,
    arrival: ArrivalClock,
    pub(super) vol: GarchVol,
    session: SessionModulator,
    bounce: BounceState,
    duration_dist: Weibull<f64>,
    normal: Normal<f64>,
    chi_squared: ChiSquared<f64>,
    size_dist: LogNormal<f64>,
    pub(super) size_median: f64,
    size_grid: SizeGrid,
    calendar: Option<SessionCalendar>,
    tick_f64: f64,
    regime: RegimeState,
    pub(super) shape: SweepShape,
    pub(super) burst: SweepBurst,
    last_event_price_ticks: Option<f64>,
    surge: Option<SurgeWindow>,
    #[cfg(test)]
    pub(super) latent_updates: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeGrid {
    pub multiplier: Decimal,
    pub integral: bool,
    pub min_size: Decimal,
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

    #[must_use]
    pub fn from_def(def: &InstrumentDef) -> Self {
        if def.class.is_future() {
            Self {
                multiplier: def.class.multiplier(),
                integral: true,
                min_size: Decimal::ONE,
            }
        } else {
            Self::spot()
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
        )
    }

    #[must_use]
    pub fn new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
    ) -> Self {
        Self::with_clamp_override(
            scalars, seed, start_ts, fp, session, regime, None, size_grid,
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
        Self::try_with_clamp_override(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            None,
            SizeGrid::spot(),
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
        Self::try_with_clamp_override(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            None,
            size_grid,
        )
    }

    /// Fallible twin of [`GeneratedSource::new_with_session_profile`] - same
    /// rationale as [`GeneratedSource::try_new`], but for the explicit-session
    /// path where the profile is also untrusted config.
    pub fn try_new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_with_clamp_override(
            scalars, seed, start_ts, fp, session, regime, None, size_grid,
        )
    }

    /// Infallible wrapper: panics if either input is outside the fingerprint
    /// ranges. Callers building from the committed fingerprint (valid by
    /// construction) use this via `new` / `new_with_session_profile`; callers
    /// with untrusted config use the `try_*` twins above instead.
    #[expect(
        clippy::too_many_arguments,
        reason = "constructor threads the complete generator parameterization"
    )]
    fn with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
        size_grid: SizeGrid,
    ) -> Self {
        Self::try_with_clamp_override(
            scalars,
            seed,
            start_ts,
            fp,
            session,
            regime,
            clamp_override,
            size_grid,
        )
        .expect("generated source inputs are inside fingerprint ranges")
    }

    // The only fallible inputs are `scalars`/`session`, guarded by the two
    // `?`-propagated `validate` calls at the top. The distribution constructors
    // below (`LogNormal`/`Weibull`/`Normal`/`ChiSquared`) take compile-time
    // constants that are always valid params, so their `expect`s cannot fire -
    // `unwrap_in_result` is silenced here because there is no meaningful error
    // variant to map them onto, not because a failure is being swallowed.
    #[expect(
        clippy::unwrap_in_result,
        clippy::too_many_arguments,
        reason = "distribution params are constant and valid; only the validated scalars/session can fail"
    )]
    fn try_with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        _fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
        size_grid: SizeGrid,
    ) -> Result<Self, GeneratedSourceError> {
        scalars.validate().map_err(GeneratedSourceError::Scalar)?;
        scalars
            .validate_size_grid(size_grid.min_size)
            .map_err(GeneratedSourceError::Scalar)?;
        session.validate().map_err(GeneratedSourceError::Session)?;
        let mean_event_duration_s = scalars.mean_event_duration_s;
        let calibrated_mean_s = ARRIVAL_MEAN_CAL * mean_event_duration_s;
        let active_mean_s = calibrated_mean_s
            / ((1.0 - ARRIVAL_QUIET_FRACTION)
                + ARRIVAL_QUIET_FRACTION * ARRIVAL_QUIET_ACTIVE_RATIO);
        let vol = GarchVol::new(decimal_to_f64(scalars.start_price), scalars.vol_scalar);
        // The config now names the quantity the sampler actually consumes.
        // The removed `typical_notional` took a circuitous route through price,
        // contract multiplier, and the lognormal mean correction. Besides
        // mixing quote currency with contracts, that made a field called
        // "typical" denote the arithmetic mean of a strongly skewed law.
        let size_median = decimal_to_f64(scalars.latent_size_median).max(f64::MIN_POSITIVE);
        let size_dist = LogNormal::new(size_median.ln(), SIZE_LOG_SIGMA).expect("valid lognormal");
        let shape = SweepShape::new(
            scalars.children_mean,
            scalars.children_single_frac,
            scalars.levels_mean,
        );
        // Built before the struct literal because it borrows `scalars.symbol`,
        // which the literal moves; `start_ts` is the tape anchor RegimeState
        // needs to fail-close an already-elapsed ReopenGap.
        let regime = RegimeState::new(regime, clamp_override, start_ts, &scalars.symbol);
        Ok(Self {
            tick_f64: decimal_to_f64(scalars.modal_tick),
            scalars,
            rng: ChaCha12Rng::seed_from_u64(seed),
            clock_ns: start_ts,
            arrival: ArrivalClock {
                active_mean_s,
                quiet_mean_s: active_mean_s * ARRIVAL_QUIET_ACTIVE_RATIO,
                quiet_to_active: (1.0 - ARRIVAL_STATE_PERSISTENCE) * (1.0 - ARRIVAL_QUIET_FRACTION),
                active_to_quiet: (1.0 - ARRIVAL_STATE_PERSISTENCE) * ARRIVAL_QUIET_FRACTION,
                quiet: false,
                last_quiet: false,
            },
            vol,
            session: SessionModulator::new(session),
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
                half_spread_ticks: HALF_SPREAD_TICKS,
            },
            duration_dist: Weibull::new(1.0, ARRIVAL_WEIBULL_SHAPE).expect("valid weibull"),
            normal: Normal::new(0.0, 1.0).expect("valid normal"),
            chi_squared: ChiSquared::new(STUDENT_T_DF).expect("valid chi-squared"),
            size_dist,
            size_median,
            size_grid,
            calendar: None,
            regime,
            shape,
            burst: SweepBurst::empty(),
            last_event_price_ticks: None,
            surge: None,
            #[cfg(test)]
            latent_updates: 0,
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

    #[must_use]
    pub fn with_calendar(mut self, calendar: Option<SessionCalendar>) -> Self {
        self.calendar = calendar;
        self
    }

    #[cfg(test)]
    pub(super) fn new_with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
        clamp_mult: f64,
    ) -> Self {
        Self::with_clamp_override(
            scalars,
            seed,
            start_ts,
            fp,
            &fp.session_profile,
            regime,
            Some(clamp_mult),
            SizeGrid::spot(),
        )
    }

    fn next_duration_ns(&mut self) -> u64 {
        let raw_eps = self.duration_dist.sample(&mut self.rng);
        self.arrival.last_quiet = self.arrival.quiet;
        let state_mean = if self.arrival.quiet {
            self.arrival.quiet_mean_s
        } else {
            self.arrival.active_mean_s
        };
        let duration_s = (state_mean * raw_eps / ARRIVAL_WEIBULL_MEAN).max(0.000_000_001);
        self.arrival.quiet = if self.arrival.quiet {
            !self.rng.random_bool(self.arrival.quiet_to_active)
        } else {
            self.rng.random_bool(self.arrival.active_to_quiet)
        };
        let arr_mult = self.session.arrival_mult(self.clock_ns);
        if arr_mult >= SESSION_CLOSED_ARR_MULT {
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
        self.closed_window_gap_ns(duration_s)
    }

    /// Wall-clock gap for a duration draw whose gap OPENS inside a closed
    /// session window (arrival multiplier below `SESSION_CLOSED_ARR_MULT`).
    ///
    /// The open-market path in `next_duration_ns` samples the arrival
    /// multiplier once and stretches the entire draw by `1/mult` - fine while
    /// the multiplier is O(1), catastrophic when a share is near zero: the
    /// stretched gap wildly overshoots the closed window (share 1e-6 turns a
    /// ~7 s draw into ~80 days) and an extreme share saturates the f64->u64
    /// cast at u64::MAX, pinning the clock there so every later tick carries
    /// the same `ts_event` - breaking the strict monotonicity `monotonic_clock`
    /// pins and the ordering `MergeSource` and `seek_to` rely on.
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
    fn closed_window_gap_ns(&self, duration_s: f64) -> u64 {
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
        let normal = self.normal.sample(&mut self.rng);
        // Guard against a chi-squared draw that underflows to exactly 0.0: an
        // unguarded 0.0 denominator makes `student_t` `0.0/0.0 = NaN` when
        // `normal` also happens to be 0.0, and `f64::clamp` propagates NaN
        // through `base_return` into `mid`, poisoning the walk for the rest of
        // the session. Astronomically unlikely from a continuous distribution,
        // but cheap to close off (matches the `f64::MIN_POSITIVE` floors used
        // elsewhere in this file for the same reason).
        let chi = self
            .chi_squared
            .sample(&mut self.rng)
            .max(f64::MIN_POSITIVE);
        let student_t = normal / (chi / STUDENT_T_DF).sqrt();
        self.vol.sigma2 = self.vol.a0
            + self.vol.a1 * self.vol.prev_return.powi(2)
            + self.vol.b1 * self.vol.sigma2;
        let sigma_cap = (GARCH_SIGMA_CAP * self.regime.clamp_mult).powi(2);
        self.vol.sigma2 = self.vol.sigma2.min(sigma_cap);
        // FEEDBACK clamp: `base_return` (which feeds `prev_return`) and the
        // sigma2 cap above use the regime's BASE clamp lift (vol_mult for a
        // storm, the test override, or 1.0). A SessionEdgeSpike deliberately
        // does NOT lift this - keeping the GARCH recursion state (sigma2,
        // prev_return) byte-identical to a clean run is what lets the spike
        // leave zero trace outside its hour window.
        let feedback_cap = MAX_ABS_RETURN * self.regime.clamp_mult;
        let base_return = (self.vol.sigma2.sqrt() * student_t).clamp(-feedback_cap, feedback_cap);
        // GARCH feedback sees the un-modulated return so volatility clustering
        // is unchanged; the session envelope scales the realized RMS on top,
        // then the hard clamp still bounds the mid update.
        self.vol.prev_return = base_return;
        // Vol composition convention (see also RegimeState::vol_mult): the
        // session envelope and the regime envelope COMPOSE MULTIPLICATIVELY here
        // (session 1.0 = no session bias, regime 1.0 = no regime bias, so the
        // product is the combined RMS scale). Inside the regime envelope a
        // SessionEdgeSpike instead composes ADDITIVELY onto the storm baseline
        // (vol_mult + edge_mult). The two conventions are intentional and rely on
        // both neutral values being 1.0; do NOT restructure either into the other
        // (a future regime that set both vol_mult and an edge spike would want the
        // add re-examined - today the match is exclusive so only one is non-unit).
        let vol_mult = self.session.vol_mult(self.clock_ns) * self.regime.vol_mult(self.clock_ns);
        // REALIZED clamp: the composed return that actually moves the mid uses
        // the WINDOWED clamp. For VolStorm and the clean/drought/reopen regimes
        // this equals `feedback_cap` bit for bit (edge_extra is 0), so their
        // streams are unchanged. A SessionEdgeSpike lifts it only INSIDE its
        // hour window, by exactly the same (1.0 + extra_vol_mult) that amplifies
        // vol_mult there - so a large extra_vol_mult no longer saturates the
        // realized spike against MAX_ABS_RETURN the way it did when this clamp
        // was pinned at 1.0. Outside the window `realized_clamp_mult` returns the
        // base clamp, so every out-of-window return stays byte-identical.
        let realized_cap = MAX_ABS_RETURN * self.regime.realized_clamp_mult(self.clock_ns);
        let return_n = (base_return * vol_mult).clamp(-realized_cap, realized_cap);
        self.vol.mid = (self.vol.mid * return_n.exp())
            .max(self.tick_f64)
            .min(MID_CEILING);
        self.vol.mid
    }

    fn next_price(&mut self, mid: f64) -> (Decimal, AggressorSide) {
        let side = self.bounce.next_side(&mut self.rng);
        self.bounce.next_drift(&mut self.rng);
        let mid_ticks = mid / self.tick_f64 + self.bounce.drift_ticks as f64;
        let price_ticks = match side {
            AggressorSide::Buyer => (mid_ticks + self.bounce.half_spread_ticks).ceil(),
            AggressorSide::Seller => (mid_ticks - self.bounce.half_spread_ticks).floor(),
            // Invariant-protected, not a runtime check: `side` is the return of
            // `BounceState::next_side` directly above, whose every branch yields
            // Buyer or Seller (its flip match collapses NoAggressor into Buyer).
            // The generator never produces a neutral aggressor - that side exists
            // only for the CSV/tick-rule lineage - so this arm is dead by
            // construction. It stays as a guard so a future edit to next_side that
            // started emitting NoAggressor would fail loudly here rather than
            // silently quoting a mid-priced trade.
            AggressorSide::NoAggressor => unreachable!("bounce only emits buyer or seller"),
        };
        // `mid` is floored at one tick (see next_latent_mid), but drift_ticks
        // is an unbounded accumulated random walk with no such floor: a long
        // enough same-direction high-regime streak can push mid_ticks (and
        // hence price_ticks) to zero or negative, quoting a zero/negative
        // price. Clamp the quoted tick count the same way mid itself is
        // clamped, so the drifted quote can never undercut one tick.
        let price_ticks = price_ticks.max(1.0);
        let price = decimal_from_f64(price_ticks * self.tick_f64);
        (price.round_dp(self.scalars.price_decimals), side)
    }

    pub(super) fn next_size(&mut self) -> Decimal {
        let base = self.size_dist.sample(&mut self.rng).max(f64::MIN_POSITIVE);
        let lot_snap = self.rng.random_bool(self.scalars.size_round_frac);
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
}

/// Round-lot scale selected from the latent distribution center. Keeping this
/// tiny derivation named makes its decade transitions directly testable: the
/// old crypto notional proxy pinned it at one and left every larger-contract
/// branch live but unexercised.
pub(super) fn integral_lot(latent_size_median: f64) -> f64 {
    10_f64.powf(latent_size_median.log10().floor()).max(1.0)
}

impl TickSource for GeneratedSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        if self.burst.remaining == 0 {
            self.begin_event();
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
}

impl GeneratedSource {
    fn begin_event(&mut self) {
        let (rate_mult, children_mult) = self
            .surge
            .as_ref()
            .filter(|window| (window.start_ns..window.end_ns).contains(&self.clock_ns))
            .map_or((1.0, 1.0), |window| {
                (window.rate_mult, window.children_mult)
            });
        let dt_ns = ((self.next_duration_ns() as f64 / rate_mult).round() as u64).max(1);
        // Order is load-bearing: next_duration_ns reads the arrival multiplier at
        // the START of the gap (the clock has not advanced yet), then the clock
        // steps, then next_latent_mid reads the volatility multiplier at the
        // instant the trade PRINTS. A duration belongs to the session window it
        // opens in; a trade's volatility belongs to the window it prints in. Do
        // not reorder these three lines to "tidy" them - it silently shifts which
        // session window each tick is attributed to.
        let old_clock_ns = self.clock_ns;
        self.clock_ns = self.clock_ns.saturating_add(dt_ns);
        if let Some(reopen) = self.regime.take_reopen_crossed(old_clock_ns, self.clock_ns) {
            self.clock_ns = self.clock_ns.saturating_add(reopen.halt_ns);
            self.vol.mid = (self.vol.mid * reopen.gap_frac.exp())
                .max(self.tick_f64)
                .min(MID_CEILING);
        }
        if let Some(calendar) = &self.calendar
            && !calendar.is_open(self.clock_ns)
        {
            self.clock_ns = calendar.next_open_ns(self.clock_ns);
        }
        let mid = self.next_latent_mid();
        let (price, aggressor) = self.next_price(mid);
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
        let state_children_mult = if self.arrival.last_quiet {
            ARRIVAL_QUIET_CHILDREN_MULT
        } else {
            ARRIVAL_ACTIVE_CHILDREN_MULT
        };
        let state_single_frac = if self.arrival.last_quiet {
            self.scalars.children_single_frac
        } else {
            ((self.scalars.children_single_frac
                - ARRIVAL_QUIET_FRACTION
                    / (self.scalars.children_mean * ARRIVAL_QUIET_CHILDREN_MULT))
                / (1.0 - ARRIVAL_QUIET_FRACTION))
                .max(0.0)
        };
        let count = {
            let mut event_shape = SweepShape::new(
                self.scalars.children_mean * children_mult * state_children_mult,
                state_single_frac,
                // Levels are not drawn here: `next_child` walks the grid off
                // `self.shape.level_step_prob`, which is solved once from the
                // declared `levels_mean` and never state-conditioned. Any
                // value in [1, mean] would do; 1.0 says "unused" plainly.
                1.0,
            );
            let count = event_shape.next_count(&mut self.rng);
            self.shape.drawn += 1;
            self.shape.truncated += event_shape.truncated;
            count
        };
        let price_ticks = if count == 1
            && !self.bounce.high_regime
            && self.rng.random_bool(EVENT_PRICE_REPEAT_PROB)
        {
            self.last_event_price_ticks
                .unwrap_or_else(|| decimal_to_f64(price) / self.tick_f64)
        } else {
            decimal_to_f64(price) / self.tick_f64
        };
        self.burst = SweepBurst {
            remaining: count,
            emitted: 0,
            parent_ts_ns: self.clock_ns,
            side: aggressor,
            price_ticks,
        };
    }

    fn next_child(&mut self) -> TradeTick {
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
        let price = decimal_from_f64(self.burst.price_ticks * self.tick_f64)
            .round_dp(self.scalars.price_decimals);
        let tick = TradeTick {
            symbol: self.scalars.symbol.clone(),
            price,
            size: self.next_size(),
            aggressor: self.burst.side,
            ts_event: self.clock_ns,
        };
        self.burst.remaining -= 1;
        self.burst.emitted += 1;
        if self.burst.remaining == 0 {
            // End of the sweep: remember where it finished (the repeat branch
            // in `begin_event` quotes from here) and RE-CENTRE the bounce
            // drift on the residual between the last printed level and the
            // latent mid. `BounceState::next_drift` still takes its per-event
            // step, but it no longer accumulates across events - see the field
            // comment on `drift_ticks`.
            self.last_event_price_ticks = Some(self.burst.price_ticks);
            self.bounce.drift_ticks = (DRIFT_RECENTER_FRAC
                * (self.burst.price_ticks - self.vol.mid / self.tick_f64))
                .round() as i64;
        }
        tick
    }
}
