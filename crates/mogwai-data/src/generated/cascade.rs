// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The activity cascade: the synthetic tape v2 walk, landed at tape
//! protocol 32 beside the fingerprint-fitted walk rather than in place of
//! it. A preset selects it by carrying `[instrument.generator.cascade]`,
//! which requires a calendar with an activity envelope; every calendar-less
//! preset keeps the old walk byte for byte.
//!
//! The mechanism, measured on a year of MNQ one-minute bars (260 full
//! sessions) and stated in full here because the preset carries only the
//! numbers:
//!
//! - The calendar owns whether the market is open. The envelope owns the
//!   deterministic activity shape per minute of session, `v(m)` for arrivals
//!   and `r(m)` for range, each normalised to the session.
//! - Everything stochastic about activity is one multiplicative log-Gaussian
//!   cascade: independent Ornstein-Uhlenbeck components, unit variance each,
//!   combined with declared weights, at timescales from seconds to a month.
//!   The minute-scale group is the texture: it carries an amplitude that
//!   shrinks with activity, `s(m) = s0 * v(m)^(-gamma)`, because the real
//!   residual is lognormal with a log-sd of 0.70 overnight and 0.36 at the
//!   cash open. The day-scale group is the level, the slow regime that makes
//!   one month twice as busy as another.
//! - The parent rate in a second is the envelope's mean rate times
//!   `exp(s * T - s^2 / 2 + level_sd * L)`, `T` and `L` the two cascade
//!   sums; the count in the second is Poisson at that rate and the parents
//!   are placed uniformly inside it. The centring term keeps the mean rate
//!   on the envelope, and the envelope's median profile is lifted to its
//!   mean profile by the same texture variance before it is normalised.
//! - Each parent moves the log mid by `sigma * t_nu`, a standardised
//!   Student-t innovation, with `sigma = event_log_sigma * r / sqrt(v) *
//!   level_sigma`. The minute variance then follows the count, which is the
//!   time change the real range residual demands: its correlation with the
//!   volume residual is 0.74 where the square-root law predicts 0.75. No
//!   drift and no bounce regime: the mid is a martingale, as the real close
//!   series is at every horizon from a minute to a session.
//! - Jumps arrive at a rate proportional to the parent rate and move the mid
//!   by a lognormal multiple of the reference minute sd. They are the news
//!   component the summed innovations cannot make: the largest minute of a
//!   session, and the standardised minute kurtosis of nine.
//! - Every scheduled reopen applies a gap, lognormal around the session's
//!   sigma level, at the first parent after the closure.
//!
//! State is a dozen floats plus the current second's bucket, all `Clone`,
//! so the checkpoint chain and the seek work unchanged.

use mogwai_protocol::AggressorSide;
use rand::RngExt;
use rand_chacha::ChaCha12Rng;
use rand_distr::{ChiSquared, Distribution, Normal, Poisson};
use serde::Deserialize;

use super::calendar::SessionCalendar;
use super::consts::INTRA_EVENT_STEP_NS;
use super::dynamics::draw_student_t;
use super::fingerprint::ScalarError;

const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_MINUTE: u64 = 60 * NS_PER_SECOND;
const MINUTES_PER_WEEK: u64 = 7 * 24 * 60;
/// Ceiling on the number of cascade components a preset may declare in
/// either group. Eight spans seconds to months at a factor of five per step.
pub const MAX_CASCADE_COMPONENTS: usize = 8;
/// Ceiling on the parents drawn into one second. A rate this high is a
/// misconfiguration, not a market; the cap keeps a runaway texture draw from
/// owing a million-child second to the venue.
const MAX_PARENTS_PER_SECOND: f64 = 1_000_000.0;
/// Bound on a single parent's log move, jump and gap included. Twenty
/// percent in one event is far outside anything the fitted tails produce and
/// keeps the mid finite under any declared configuration.
const MAX_EVENT_LOG_MOVE: f64 = 0.2;

/// The `[instrument.generator.cascade]` table. Every field is a knob with
/// its own provenance entry in the preset.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CascadeConfig {
    /// Texture timescales, minutes. Real MNQ residual autocorrelation is
    /// reproduced by four to five components from a quarter of a minute to
    /// ninety minutes.
    pub texture_tau_minutes: Vec<f64>,
    /// Variance weights of the texture components, summing to one.
    pub texture_weights: Vec<f64>,
    /// `s0`: the texture's log amplitude at the session's mean minute.
    pub texture_amplitude: f64,
    /// `gamma`: how the amplitude shrinks with the envelope's activity.
    pub texture_exponent: f64,
    /// Level timescales, minutes: a session, a week, a month.
    pub level_tau_minutes: Vec<f64>,
    pub level_weights: Vec<f64>,
    /// Log-sd of the session level of arrivals across sessions.
    pub level_log_sd: f64,
    /// Exponent of the per-parent sigma level on the arrival level.
    pub sigma_level_exponent: f64,
    /// Log-sd of the per-parent sigma level's own slow component.
    pub sigma_level_log_sd: f64,
    /// Per-parent log-return sd at the reference minute, before the range
    /// shape and the level.
    pub event_log_sigma: f64,
    /// Degrees of freedom of the per-parent Student-t innovation.
    pub student_df: f64,
    /// Reopen gap median, in units of the reference minute sd.
    pub gap_median_ratio: f64,
    /// Reopen gap log-sd.
    pub gap_log_sd: f64,
    /// Where the gap's normal draw is clamped, in sds. The lognormal fits
    /// the real gaps through p99 and is too heavy past it: the real year's
    /// largest gap, 56 times the median, is the 2.5-sigma point of the
    /// fitted law, so a draw beyond 2.75 describes nothing observed.
    pub gap_log_clamp_sd: f64,
    /// Mean jumps per session at the reference level.
    pub jumps_per_session: f64,
    /// Jump size median, in units of the reference minute sd at the day's
    /// sigma level, scaled by the local minute sd's ratio to that reference
    /// raised to `jump_local_exponent`.
    pub jump_size: f64,
    /// Jump size log-sd.
    pub jump_log_sd: f64,
    /// Where the jump's normal draw is clamped, in sds, for the reason the
    /// gap's is: the real open's largest minute in a year, 197 points, is
    /// the 2.6-sigma point of the fitted law.
    pub jump_log_clamp_sd: f64,
    /// How much a jump's size follows the minute sd where it lands. Zero is
    /// news of one size at any hour; one is a fixed multiple of the local
    /// minute. Real MNQ's largest minute per phase runs 140 to 390 points
    /// across a threefold spread in minute sd, which a half power gives.
    pub jump_local_exponent: f64,
    /// The volume a jump brings: a kick, in units of the texture's own
    /// log-sd, applied to the two fastest texture components when a jump
    /// lands, so the minutes after it print a burst that tapers. Real MNQ
    /// minutes over 60 points carry twice the phase's median volume at the
    /// median and five times at p90.
    pub jump_volume_kick: f64,
    /// Probability a parent takes the previous parent's aggressor side.
    pub side_persistence: f64,
}

fn finite_nonnegative(value: f64) -> bool {
    value.is_finite() && value >= 0.0
}

fn valid_components(taus: &[f64], weights: &[f64]) -> bool {
    if taus.is_empty() || taus.len() > MAX_CASCADE_COMPONENTS || weights.len() != taus.len() {
        return false;
    }
    if !taus.iter().all(|tau| tau.is_finite() && *tau > 0.0) {
        return false;
    }
    if !weights.iter().all(|w| w.is_finite() && *w >= 0.0) {
        return false;
    }
    let sum: f64 = weights.iter().sum();
    (sum - 1.0).abs() <= 1e-6
}

impl CascadeConfig {
    pub fn validate(&self) -> Result<(), ScalarError> {
        if !valid_components(&self.texture_tau_minutes, &self.texture_weights) {
            return Err(ScalarError::detailed(
                "cascade",
                "texture components need matching positive timescales and weights summing to one",
            ));
        }
        if !valid_components(&self.level_tau_minutes, &self.level_weights) {
            return Err(ScalarError::detailed(
                "cascade",
                "level components need matching positive timescales and weights summing to one",
            ));
        }
        if !finite_nonnegative(self.texture_amplitude) || self.texture_amplitude > 3.0 {
            return Err(ScalarError::detailed("cascade", "texture_amplitude"));
        }
        if !self.texture_exponent.is_finite() || !(0.0..=2.0).contains(&self.texture_exponent) {
            return Err(ScalarError::detailed("cascade", "texture_exponent"));
        }
        if !finite_nonnegative(self.level_log_sd) || self.level_log_sd > 3.0 {
            return Err(ScalarError::detailed("cascade", "level_log_sd"));
        }
        if !self.sigma_level_exponent.is_finite()
            || !(0.0..=2.0).contains(&self.sigma_level_exponent)
        {
            return Err(ScalarError::detailed("cascade", "sigma_level_exponent"));
        }
        if !finite_nonnegative(self.sigma_level_log_sd) || self.sigma_level_log_sd > 3.0 {
            return Err(ScalarError::detailed("cascade", "sigma_level_log_sd"));
        }
        if !self.event_log_sigma.is_finite()
            || self.event_log_sigma <= 0.0
            || self.event_log_sigma > 1e-2
        {
            return Err(ScalarError::detailed("cascade", "event_log_sigma"));
        }
        // The innovation is standardised by sqrt(df / (df - 2)), which needs
        // a finite variance.
        if !self.student_df.is_finite() || self.student_df <= 2.0 {
            return Err(ScalarError::detailed("cascade", "student_df"));
        }
        if !finite_nonnegative(self.gap_median_ratio) || !finite_nonnegative(self.gap_log_sd) {
            return Err(ScalarError::detailed("cascade", "gap"));
        }
        if self.gap_log_sd > 5.0 {
            return Err(ScalarError::detailed("cascade", "gap_log_sd"));
        }
        if !self.gap_log_clamp_sd.is_finite() || !(0.5..=10.0).contains(&self.gap_log_clamp_sd) {
            return Err(ScalarError::detailed("cascade", "gap_log_clamp_sd"));
        }
        if !finite_nonnegative(self.jumps_per_session)
            || !finite_nonnegative(self.jump_size)
            || !finite_nonnegative(self.jump_log_sd)
            || self.jump_log_sd > 5.0
            || !finite_nonnegative(self.jump_volume_kick)
            || self.jump_volume_kick > 10.0
            || !self.jump_local_exponent.is_finite()
            || !(0.0..=1.0).contains(&self.jump_local_exponent)
            || !self.jump_log_clamp_sd.is_finite()
            || !(0.5..=10.0).contains(&self.jump_log_clamp_sd)
        {
            return Err(ScalarError::detailed("cascade", "jumps"));
        }
        if !self.side_persistence.is_finite() || !(0.0..1.0).contains(&self.side_persistence) {
            return Err(ScalarError::detailed("cascade", "side_persistence"));
        }
        Ok(())
    }
}

/// The runtime modifiers a second is drawn under: the surge's rate
/// multiplier and the regime overlay's thinning and volatility multiplier.
#[derive(Debug, Clone, Copy)]
pub(super) struct SecondModifiers {
    pub(super) rate_mult: f64,
    pub(super) arrival_thin: f64,
    pub(super) vol_mult: f64,
}

/// One parent, as the cascade draws it: its instant and the log move it
/// applies to the mid, gap and jump included.
#[derive(Debug, Clone, Copy)]
pub(super) struct CascadeParent {
    pub(super) ts_ns: u64,
    pub(super) log_move: f64,
    pub(super) side: AggressorSide,
    /// The standardised innovation and the per-parent sigma behind the
    /// diffusive part of `log_move`, for the observation-only trace.
    pub(super) innovation: f64,
    pub(super) sigma: f64,
}

/// Why the cascade cannot draw another parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CascadeRefusal {
    /// The calendar never opens again, or the clock cannot be represented.
    ClockExhausted,
}

/// Everything precomputed from the config and the calendar's envelope,
/// shared by the lead and every checkpoint.
#[derive(Clone)]
pub(super) struct CascadeTables {
    config: CascadeConfig,
    calendar: SessionCalendar,
    /// Per minute of session: the envelope's mean arrival shape, normalised
    /// to mean one over the calendar's open week minutes with the weekday
    /// weight applied.
    arrival: Vec<f64>,
    /// Per minute of session: the texture amplitude `s(m)`.
    texture_s: Vec<f64>,
    /// Per minute of session: `r(m) / sqrt(v(m))`, the per-parent sigma
    /// shape.
    sigma_shape: Vec<f64>,
    weekday: [f64; 7],
    texture_tau_s: Vec<f64>,
    level_tau_s: Vec<f64>,
    texture_sqrt_w: Vec<f64>,
    level_sqrt_w: Vec<f64>,
    parents_per_second: f64,
    session_seconds: f64,
    /// `event_log_sigma * sqrt(parents per minute)`: the sd of a reference
    /// minute's log return, the unit gaps and jumps are stated in.
    reference_log_sd: f64,
    student_scale: f64,
    normal: Normal<f64>,
    chi_squared: ChiSquared<f64>,
}

impl CascadeTables {
    pub(super) fn new(
        config: &CascadeConfig,
        calendar: &SessionCalendar,
        mean_event_duration_s: f64,
    ) -> Result<Self, ScalarError> {
        config.validate()?;
        let Some(envelope) = calendar.envelope.as_ref() else {
            return Err(ScalarError::detailed(
                "cascade",
                "requires a calendar with an activity envelope",
            ));
        };
        let minutes = envelope.minutes();
        let texture_s: Vec<f64> = envelope
            .volume
            .iter()
            .map(|v| config.texture_amplitude * v.powf(-config.texture_exponent))
            .collect();
        // The envelope is a cross-session median profile; the rate the
        // cascade centres on is a mean, and a lognormal minute's median sits
        // exp(-s^2 / 2) below its mean, more so overnight where the texture
        // is wider. Lift each minute by its own texture variance first.
        let lifted: Vec<f64> = envelope
            .volume
            .iter()
            .zip(&texture_s)
            .map(|(v, s)| v * (0.5 * s * s).exp())
            .collect();
        let mut sum = 0.0;
        let mut open = 0.0;
        for minute in 0..MINUTES_PER_WEEK {
            let clock_ns = minute * NS_PER_MINUTE;
            if !calendar.is_open(clock_ns) {
                continue;
            }
            if let Some((day, m)) = calendar.session_position(clock_ns) {
                sum += lifted[m] * envelope.weekday_weight[day];
                open += 1.0;
            }
        }
        if open == 0.0 || sum <= 0.0 {
            return Err(ScalarError::detailed(
                "cascade",
                "the calendar exposes no envelope minute",
            ));
        }
        let mean = sum / open;
        let arrival = lifted.iter().map(|v| v / mean).collect();
        let sigma_shape = envelope
            .volume
            .iter()
            .zip(&envelope.range)
            .map(|(v, r)| r / v.sqrt())
            .collect();
        let parents_per_second = 1.0 / mean_event_duration_s;
        let student_scale = (config.student_df / (config.student_df - 2.0)).sqrt();
        Ok(Self {
            config: config.clone(),
            calendar: calendar.clone(),
            arrival,
            texture_s,
            sigma_shape,
            weekday: envelope.weekday_weight,
            texture_tau_s: config
                .texture_tau_minutes
                .iter()
                .map(|t| t * 60.0)
                .collect(),
            level_tau_s: config.level_tau_minutes.iter().map(|t| t * 60.0).collect(),
            texture_sqrt_w: config.texture_weights.iter().map(|w| w.sqrt()).collect(),
            level_sqrt_w: config.level_weights.iter().map(|w| w.sqrt()).collect(),
            parents_per_second,
            session_seconds: minutes as f64 * 60.0,
            reference_log_sd: config.event_log_sigma * (parents_per_second * 60.0).sqrt(),
            student_scale,
            normal: Normal::new(0.0, 1.0)
                .map_err(|_| ScalarError::detailed("cascade", "unit normal"))?,
            chi_squared: ChiSquared::new(config.student_df)
                .map_err(|_| ScalarError::detailed("cascade", "student_df"))?,
        })
    }
}

/// The mutable walk state of the cascade.
#[derive(Clone)]
pub(super) struct CascadeState {
    texture: Vec<f64>,
    level: Vec<f64>,
    sigma_extra: Vec<f64>,
    /// Start of the second whose bucket is current.
    second_ns: u64,
    /// Whether `second_ns` has been filled yet. The origin second is filled
    /// without stepping the components first.
    filled: bool,
    /// Parents still owed inside the current second.
    remaining: u32,
    /// The instant the next parent is drawn after, and the second's end.
    next_from_ns: u64,
    window_end_ns: u64,
    /// Per-parent log-return sd for the current second.
    second_sigma: f64,
    /// A jump owed to the next parent, in log return.
    pending_jump: f64,
    /// A reopen has been crossed since the last parent; the next one carries
    /// the gap.
    pending_gap: bool,
    /// At least one parent has been drawn, so a closure crossed from here on
    /// is a real reopen rather than the origin sitting inside one.
    started: bool,
    last_side: AggressorSide,
}

impl CascadeState {
    pub(super) fn new(tables: &CascadeTables, rng: &mut ChaCha12Rng, start_ts: u64) -> Self {
        let draw = |rng: &mut ChaCha12Rng, n: usize| -> Vec<f64> {
            (0..n).map(|_| tables.normal.sample(rng)).collect()
        };
        let texture = draw(rng, tables.texture_tau_s.len());
        let level = draw(rng, tables.level_tau_s.len());
        let sigma_extra = draw(rng, tables.level_tau_s.len());
        let second_ns = start_ts / NS_PER_SECOND * NS_PER_SECOND;
        Self {
            texture,
            level,
            sigma_extra,
            second_ns,
            filled: false,
            remaining: 0,
            next_from_ns: start_ts,
            window_end_ns: second_ns + NS_PER_SECOND,
            second_sigma: 0.0,
            pending_jump: 0.0,
            pending_gap: false,
            started: false,
            last_side: AggressorSide::Buyer,
        }
    }

    /// Forget the current bucket: the clock has been moved past it by
    /// something outside the cascade (an armed halt).
    pub(super) fn reset_bucket(&mut self, clock_ns: u64) {
        self.remaining = 0;
        self.second_ns = clock_ns / NS_PER_SECOND * NS_PER_SECOND;
        self.filled = false;
    }

    /// Whether a parent is still owed inside the current second.
    pub(super) fn has_parent(&self) -> bool {
        self.remaining > 0
    }

    fn step_components(&mut self, tables: &CascadeTables, rng: &mut ChaCha12Rng, elapsed_s: f64) {
        for (y, tau) in self.texture.iter_mut().zip(&tables.texture_tau_s) {
            let rho = (-elapsed_s / tau).exp();
            *y = rho * *y + (1.0 - rho * rho).sqrt() * tables.normal.sample(rng);
        }
        for (y, tau) in self.level.iter_mut().zip(&tables.level_tau_s) {
            let rho = (-elapsed_s / tau).exp();
            *y = rho * *y + (1.0 - rho * rho).sqrt() * tables.normal.sample(rng);
        }
        for (y, tau) in self.sigma_extra.iter_mut().zip(&tables.level_tau_s) {
            let rho = (-elapsed_s / tau).exp();
            *y = rho * *y + (1.0 - rho * rho).sqrt() * tables.normal.sample(rng);
        }
    }

    fn texture_sum(&self, tables: &CascadeTables) -> f64 {
        self.texture
            .iter()
            .zip(&tables.texture_sqrt_w)
            .map(|(y, w)| y * w)
            .sum()
    }

    fn level_sum(&self, tables: &CascadeTables) -> f64 {
        self.level
            .iter()
            .zip(&tables.level_sqrt_w)
            .map(|(y, w)| y * w)
            .sum()
    }

    fn sigma_extra_sum(&self, tables: &CascadeTables) -> f64 {
        self.sigma_extra
            .iter()
            .zip(&tables.level_sqrt_w)
            .map(|(y, w)| y * w)
            .sum()
    }

    /// The per-parent sigma level: the slow multiplier on `event_log_sigma`.
    fn level_sigma(&self, tables: &CascadeTables) -> f64 {
        let config = &tables.config;
        (config.sigma_level_exponent * config.level_log_sd * self.level_sum(tables)
            + config.sigma_level_log_sd * self.sigma_extra_sum(tables))
        .exp()
    }

    /// Advance to the next second that owes at least one parent, drawing
    /// its count, its sigma and any jump. Closures are crossed in one step
    /// per component and mark the next parent as carrying the reopen gap.
    pub(super) fn fill_next_bucket(
        &mut self,
        tables: &CascadeTables,
        rng: &mut ChaCha12Rng,
        clock_ns: u64,
        modifiers: SecondModifiers,
    ) -> Result<(), CascadeRefusal> {
        let config = &tables.config;
        loop {
            if self.filled {
                let Some(next) = self.second_ns.checked_add(NS_PER_SECOND) else {
                    return Err(CascadeRefusal::ClockExhausted);
                };
                self.step_components(tables, rng, 1.0);
                self.second_ns = next;
            }
            self.filled = true;
            if !tables.calendar.is_open(self.second_ns) {
                let reopen = tables.calendar.next_open_ns(self.second_ns);
                if reopen == u64::MAX {
                    return Err(CascadeRefusal::ClockExhausted);
                }
                let elapsed_s = (reopen - self.second_ns) as f64 / NS_PER_SECOND as f64;
                self.step_components(tables, rng, elapsed_s);
                self.second_ns = reopen;
                if self.started {
                    self.pending_gap = true;
                }
            }
            let window_start = self
                .second_ns
                .max(clock_ns.saturating_add(INTRA_EVENT_STEP_NS));
            let Some(window_end) = self.second_ns.checked_add(NS_PER_SECOND) else {
                return Err(CascadeRefusal::ClockExhausted);
            };
            if window_start >= window_end {
                continue;
            }
            let Some((day, minute)) = tables.calendar.session_position(self.second_ns) else {
                // An open minute past the envelope's length: the calendar and
                // the envelope disagree, and the envelope wins by silence.
                continue;
            };
            let s = tables.texture_s[minute];
            let level_sum = self.level_sum(tables);
            // The rate the envelope and the slow level set, before the
            // texture: what the jump rate follows, so a jump's own volume
            // kick cannot raise the chance of the next jump.
            let slow_rate = tables.parents_per_second
                * tables.arrival[minute]
                * tables.weekday[day]
                * (config.level_log_sd * level_sum).exp();
            let base_rate = slow_rate * (s * self.texture_sum(tables) - 0.5 * s * s).exp();
            let rate = base_rate * modifiers.rate_mult / modifiers.arrival_thin;
            let window_s = (window_end - window_start) as f64 / NS_PER_SECOND as f64;
            let count = poisson(rng, (rate * window_s).min(MAX_PARENTS_PER_SECOND));
            let level_sigma = self.level_sigma(tables);
            self.second_sigma = config.event_log_sigma
                * tables.sigma_shape[minute]
                * level_sigma
                * modifiers.vol_mult;
            if config.jumps_per_session > 0.0 {
                let jump_rate = config.jumps_per_session * slow_rate * window_s
                    / (tables.parents_per_second * tables.session_seconds);
                let jumps = poisson(rng, jump_rate);
                // The minute sd where the jump lands, against the reference
                // minute at the same sigma level; the jump follows the ratio
                // to the declared power, so a night jump is smaller than a
                // cash-open one but not in full proportion.
                let reference_sd = tables.reference_log_sd * level_sigma;
                let local_minute_sd = self.second_sigma * (60.0 * base_rate).sqrt();
                let local_scale = (local_minute_sd / reference_sd).powf(config.jump_local_exponent);
                for _ in 0..jumps {
                    let draw = tables
                        .normal
                        .sample(rng)
                        .clamp(-config.jump_log_clamp_sd, config.jump_log_clamp_sd);
                    let magnitude = config.jump_size
                        * reference_sd
                        * local_scale
                        * (config.jump_log_sd * draw).exp();
                    let sign = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
                    self.pending_jump += sign * magnitude;
                    // The volume the news brings, as excitement in the two
                    // fastest texture components: the following seconds and
                    // minutes print a burst that decays at their timescales.
                    for (y, w) in self.texture.iter_mut().zip(&tables.texture_sqrt_w).take(2) {
                        if *w > 0.0 {
                            *y += config.jump_volume_kick / (2.0 * *w);
                        }
                    }
                }
            }
            self.remaining = count;
            self.next_from_ns = window_start;
            self.window_end_ns = window_end;
            if count > 0 {
                return Ok(());
            }
        }
    }

    /// Draw the next parent of the current second. Requires `has_parent`.
    pub(super) fn draw_parent(
        &mut self,
        tables: &CascadeTables,
        rng: &mut ChaCha12Rng,
        clock_ns: u64,
    ) -> CascadeParent {
        debug_assert!(self.remaining > 0, "draw_parent needs a filled bucket");
        let config = &tables.config;
        // Sequential order statistics: with n parents left in the window,
        // the next lands after a fraction 1 - u^(1/n) of what remains.
        let n = f64::from(self.remaining);
        let span = self.window_end_ns.saturating_sub(self.next_from_ns) as f64;
        let u: f64 = rng.random();
        let offset = span * (1.0 - u.powf(1.0 / n));
        let ts_ns = self
            .next_from_ns
            .saturating_add(offset as u64)
            .max(clock_ns.saturating_add(INTRA_EVENT_STEP_NS));
        self.next_from_ns = ts_ns;
        self.remaining -= 1;

        let innovation =
            draw_student_t(rng, &tables.normal, &tables.chi_squared) / tables.student_scale;
        let mut log_move = self.second_sigma * innovation;
        if self.pending_jump != 0.0 {
            log_move += self.pending_jump;
            self.pending_jump = 0.0;
        }
        if self.pending_gap {
            self.pending_gap = false;
            let draw = tables
                .normal
                .sample(rng)
                .clamp(-config.gap_log_clamp_sd, config.gap_log_clamp_sd);
            let magnitude = config.gap_median_ratio
                * tables.reference_log_sd
                * self.level_sigma(tables)
                * (config.gap_log_sd * draw).exp();
            let sign = if rng.random_bool(0.5) { 1.0 } else { -1.0 };
            log_move += sign * magnitude;
        }
        self.started = true;
        if !rng.random_bool(config.side_persistence) {
            self.last_side = match self.last_side {
                AggressorSide::Buyer => AggressorSide::Seller,
                AggressorSide::Seller | AggressorSide::NoAggressor => AggressorSide::Buyer,
            };
        }
        CascadeParent {
            ts_ns,
            log_move: log_move.clamp(-MAX_EVENT_LOG_MOVE, MAX_EVENT_LOG_MOVE),
            side: self.last_side,
            innovation,
            sigma: self.second_sigma,
        }
    }
}

fn poisson(rng: &mut ChaCha12Rng, lambda: f64) -> u32 {
    if !lambda.is_finite() || lambda <= 0.0 {
        return 0;
    }
    let draw: f64 = Poisson::new(lambda)
        .expect("a positive finite rate is a valid Poisson parameter")
        .sample(rng);
    draw.clamp(0.0, MAX_PARENTS_PER_SECOND) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::calendar::{SessionEnvelope, WeeklyWindow};
    use rand::SeedableRng;

    pub(crate) fn config() -> CascadeConfig {
        CascadeConfig {
            texture_tau_minutes: vec![0.25, 1.0, 5.0, 25.0, 90.0],
            texture_weights: vec![0.52, 0.135, 0.18, 0.15, 0.015],
            texture_amplitude: 0.54,
            texture_exponent: 0.24,
            level_tau_minutes: vec![420.0, 8640.0, 47520.0],
            level_weights: vec![0.2, 0.3, 0.5],
            level_log_sd: 0.34,
            sigma_level_exponent: 0.57,
            sigma_level_log_sd: 0.207,
            event_log_sigma: 7.4e-6,
            student_df: 4.0,
            gap_median_ratio: 0.9,
            gap_log_sd: 1.6,
            gap_log_clamp_sd: 2.75,
            jumps_per_session: 3.0,
            jump_size: 4.0,
            jump_log_sd: 0.5,
            jump_log_clamp_sd: 2.75,
            jump_volume_kick: 1.5,
            jump_local_exponent: 0.5,
            side_persistence: 0.6,
        }
    }

    /// A calendar open every weekday from 17:00 to 16:00 local at offset 0,
    /// closed Friday 16:00 to Sunday 17:00, with a flat envelope.
    pub(crate) fn calendar(volume: Vec<f64>, range: Vec<f64>) -> SessionCalendar {
        let mut windows = Vec::new();
        for day in 0..5_u32 {
            windows.push(WeeklyWindow {
                start_minute: day * 1_440 + 1_020,
                end_minute: (day + 1) * 1_440 + 960,
            });
        }
        SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: windows,
            settlement_minute_of_day: Some(900),
            envelope: Some(SessionEnvelope {
                session_open_minute_of_day: 1_020,
                weekday_weight: [1.0; 7],
                volume,
                range,
            }),
        }
    }

    #[test]
    fn a_cascade_without_an_envelope_is_refused() {
        let mut calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        calendar.envelope = None;
        let Err(error) = CascadeTables::new(&config(), &calendar, 0.08) else {
            panic!("a cascade without an envelope was accepted");
        };
        assert_eq!(error.field, "cascade");
        assert!(error.detail.unwrap().contains("envelope"));
    }

    #[test]
    fn mismatched_or_unnormalised_components_are_refused() {
        let mut bad = config();
        bad.texture_weights.pop();
        let Err(error) =
            CascadeTables::new(&bad, &calendar(vec![1.0; 1_380], vec![1.0; 1_380]), 0.08)
        else {
            panic!("mismatched components were accepted");
        };
        assert_eq!(error.field, "cascade");
        let mut bad = config();
        bad.level_weights = vec![0.5, 0.3, 0.3];
        assert!(bad.validate().is_err());
        let mut bad = config();
        bad.student_df = 2.0;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn the_lifted_envelope_has_mean_one_over_open_minutes() {
        // A flat median envelope lifts to a flat mean envelope, which then
        // normalises to exactly one at every minute.
        let tables = CascadeTables::new(
            &config(),
            &calendar(vec![1.0; 1_380], vec![1.0; 1_380]),
            0.08,
        )
        .unwrap();
        for value in &tables.arrival {
            assert!((value - 1.0).abs() < 1e-12, "{value}");
        }
        // A shaped envelope keeps mean one after the lift.
        let mut volume = vec![0.5; 1_380];
        for v in volume.iter_mut().skip(690) {
            *v = 1.5;
        }
        let tables =
            CascadeTables::new(&config(), &calendar(volume, vec![1.0; 1_380]), 0.08).unwrap();
        let mean: f64 = tables.arrival.iter().sum::<f64>() / tables.arrival.len() as f64;
        assert!((mean - 1.0).abs() < 1e-9, "{mean}");
        // The quiet half is lifted more than the busy half.
        assert!(tables.arrival[0] / 0.5 > tables.arrival[1_000] / 1.5);
    }

    #[test]
    fn a_closure_is_crossed_in_one_step_and_the_next_parent_carries_the_gap() {
        let mut config = config();
        config.jumps_per_session = 0.0;
        config.gap_median_ratio = 50.0;
        config.gap_log_sd = 0.0;
        config.gap_log_clamp_sd = 1.0;
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = CascadeTables::new(&config, &calendar, 0.08).unwrap();
        let mut rng = ChaCha12Rng::seed_from_u64(7);
        // Thursday 1970-01-01 is local day 4. Start one second before the
        // Thursday 16:00 close so the first parent lands before it and the
        // second crosses the maintenance hour.
        let start = 959 * NS_PER_MINUTE + 59 * NS_PER_SECOND;
        let mut state = CascadeState::new(&tables, &mut rng, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let mut parents = Vec::new();
        while parents.len() < 3 {
            if !state.has_parent() {
                state
                    .fill_next_bucket(&tables, &mut rng, clock, modifiers)
                    .unwrap();
            }
            let parent = state.draw_parent(&tables, &mut rng, clock);
            clock = parent.ts_ns;
            parents.push(parent);
        }
        // The gap is fifty reference sds and the ordinary move is a fraction
        // of one, so the first parent after the reopen is unmistakable.
        let threshold = 10.0 * tables.reference_log_sd;
        let reopen_ns = 1_020 * NS_PER_MINUTE;
        let after: Vec<_> = parents.iter().filter(|p| p.ts_ns >= reopen_ns).collect();
        let before: Vec<_> = parents.iter().filter(|p| p.ts_ns < reopen_ns).collect();
        assert!(!after.is_empty() && !before.is_empty());
        assert!(after[0].log_move.abs() > threshold, "{:?}", after[0]);
        assert!(after[1..].iter().all(|p| p.log_move.abs() < threshold));
        assert!(before.iter().all(|p| p.log_move.abs() < threshold));
        // Nothing printed inside the closure.
        assert!(parents.iter().all(|p| calendar.is_open(p.ts_ns)));
    }

    #[test]
    fn parents_are_strictly_increasing_and_the_rate_is_the_configured_one() {
        let mut config = config();
        config.level_log_sd = 0.0;
        config.sigma_level_log_sd = 0.0;
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = CascadeTables::new(&config, &calendar, 0.08).unwrap();
        let mut rng = ChaCha12Rng::seed_from_u64(3);
        let start = 1_020 * NS_PER_MINUTE;
        let end = start + 600 * NS_PER_MINUTE;
        let mut state = CascadeState::new(&tables, &mut rng, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let mut count = 0_u64;
        loop {
            if !state.has_parent() {
                state
                    .fill_next_bucket(&tables, &mut rng, clock, modifiers)
                    .unwrap();
            }
            let parent = state.draw_parent(&tables, &mut rng, clock);
            if parent.ts_ns >= end {
                break;
            }
            assert!(parent.ts_ns > clock);
            clock = parent.ts_ns;
            count += 1;
        }
        // Ten hours at 12.5 parents a second is 450,000; the texture's
        // minute-scale components average out over that span to a few
        // percent.
        let expected = 12.5 * 600.0 * 60.0;
        let ratio = count as f64 / expected;
        assert!((0.85..1.15).contains(&ratio), "ratio {ratio}");
    }
}
