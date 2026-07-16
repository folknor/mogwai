// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The core path-dependent walk: [`GeneratedSource`] composes the ACD
//! duration clock, the GARCH latent mid, the bounce/drift price process and
//! the optional regime overlay into the [`crate::TickSource`] the running
//! server drives. Same seed plus tape anchor yields the same stream byte for
//! byte - see `clean_regime_is_byte_identical` in the test module for the
//! pinned golden sequence this file must never perturb.

use mogwai_protocol::{AggressorSide, MarketRegime, TradeTick, decimal_to_f64};
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{ChiSquared, Distribution, LogNormal, Normal, Weibull};
use rust_decimal::Decimal;

use crate::{TickEvent, TickSource};

use super::consts::{
    ACD_FEEDBACK_SHARE, ACD_PERSISTENCE, ACD_WEIBULL_SHAPE, GARCH_SIGMA_CAP, HALF_SPREAD_TICKS,
    MAX_ABS_RETURN, MAX_SESSION_GAP_NS, MID_CEILING, NS_PER_HOUR, SESSION_CLOSED_ARR_MULT,
    SIZE_DECIMALS, SIZE_LOG_SIGMA, STUDENT_T_DF,
};
use super::dynamics::{AcdClock, BounceState, GarchVol};
use super::fingerprint::{Fingerprint, GeneratedSourceError, GeneratorScalars, SessionProfile};
use super::numeric::{WEIBULL_MEAN_SHAPE_060, decimal_from_f64, round_lot_size};
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
    acd: AcdClock,
    pub(super) vol: GarchVol,
    session: SessionModulator,
    bounce: BounceState,
    duration_dist: Weibull<f64>,
    normal: Normal<f64>,
    chi_squared: ChiSquared<f64>,
    size_dist: LogNormal<f64>,
    tick_f64: f64,
    regime: RegimeState,
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
        Self::new_with_session_profile(scalars, seed, start_ts, fp, &fp.session_profile, regime)
    }

    #[must_use]
    pub fn new_with_session_profile(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
    ) -> Self {
        Self::with_clamp_override(scalars, seed, start_ts, fp, session, regime, None)
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
    ) -> Result<Self, GeneratedSourceError> {
        Self::try_with_clamp_override(scalars, seed, start_ts, fp, session, regime, None)
    }

    /// Infallible wrapper: panics if either input is outside the fingerprint
    /// ranges. Callers building from the committed fingerprint (valid by
    /// construction) use this via `new` / `new_with_session_profile`; callers
    /// with untrusted config use the `try_*` twins above instead.
    fn with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
    ) -> Self {
        Self::try_with_clamp_override(scalars, seed, start_ts, fp, session, regime, clamp_override)
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
        reason = "distribution params are constant and valid; only the validated scalars/session can fail"
    )]
    fn try_with_clamp_override(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        session: &SessionProfile,
        regime: Option<MarketRegime>,
        clamp_override: Option<f64>,
    ) -> Result<Self, GeneratedSourceError> {
        scalars.validate(fp).map_err(GeneratedSourceError::Scalar)?;
        session.validate().map_err(GeneratedSourceError::Session)?;
        let mean_duration_s = scalars.mean_duration_s;
        let alpha = ACD_PERSISTENCE * ACD_FEEDBACK_SHARE;
        let beta = ACD_PERSISTENCE - alpha;
        let omega = mean_duration_s * (1.0 - ACD_PERSISTENCE);
        let vol = GarchVol::new(decimal_to_f64(scalars.start_price), scalars.vol_scalar);
        let size_median = decimal_to_f64(scalars.typical_size).max(f64::MIN_POSITIVE);
        let size_dist = LogNormal::new(size_median.ln(), SIZE_LOG_SIGMA).expect("valid lognormal");
        // Built before the struct literal because it borrows `scalars.symbol`,
        // which the literal moves; `start_ts` is the tape anchor RegimeState
        // needs to fail-close an already-elapsed ReopenGap.
        let regime = RegimeState::new(regime, clamp_override, start_ts, &scalars.symbol);
        Ok(Self {
            tick_f64: decimal_to_f64(scalars.modal_tick),
            scalars,
            rng: ChaCha12Rng::seed_from_u64(seed),
            clock_ns: start_ts,
            acd: AcdClock {
                omega,
                alpha,
                beta,
                psi: mean_duration_s,
                prev_duration_s: mean_duration_s,
                eps_mean: WEIBULL_MEAN_SHAPE_060,
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
            duration_dist: Weibull::new(1.0, ACD_WEIBULL_SHAPE).expect("valid weibull"),
            normal: Normal::new(0.0, 1.0).expect("valid normal"),
            chi_squared: ChiSquared::new(STUDENT_T_DF).expect("valid chi-squared"),
            size_dist,
            regime,
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
        )
    }

    fn next_duration_ns(&mut self) -> u64 {
        let raw_eps = self.duration_dist.sample(&mut self.rng);
        let eps = (raw_eps / self.acd.eps_mean).max(f64::MIN_POSITIVE);
        self.acd.psi = self.acd.omega
            + self.acd.alpha * self.acd.prev_duration_s
            + self.acd.beta * self.acd.psi;
        let duration_s = (self.acd.psi * eps).max(0.000_000_001);
        // ACD feedback sees the un-modulated duration so clustering dynamics are
        // unchanged; the session envelope only stretches or compresses the
        // realized gap.
        self.acd.prev_duration_s = duration_s;
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

    fn next_size(&mut self) -> Decimal {
        let base = self.size_dist.sample(&mut self.rng).max(f64::MIN_POSITIVE);
        let size = if self.rng.random_bool(self.scalars.size_round_frac) {
            round_lot_size(base)
        } else {
            decimal_from_f64(base).round_dp(SIZE_DECIMALS)
        };
        size.max(Decimal::new(1, SIZE_DECIMALS))
    }
}

impl TickSource for GeneratedSource {
    fn next_tick(&mut self) -> Option<TickEvent> {
        let dt_ns = self.next_duration_ns();
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
        let mid = self.next_latent_mid();
        let (price, aggressor) = self.next_price(mid);
        let size = self.next_size();
        Some(TickEvent::Trade(TradeTick {
            symbol: self.scalars.symbol.clone(),
            price,
            size,
            aggressor,
            ts_event: self.clock_ns,
        }))
    }
}
