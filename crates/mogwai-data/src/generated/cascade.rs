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
//!   sums, times a fast texture: one more Ornstein-Uhlenbeck component at a
//!   few seconds with its own log-sd, the swell inside a minute that the
//!   minute-fitted texture cannot carry (real seconds inside one minute are
//!   dispersed five to sixteen times Poisson). The centring terms keep the
//!   mean rate on the envelope, and the envelope's median profile is lifted
//!   to its mean profile by the texture variance before it is normalised.
//! - Below the second the parents are a branching process under that rate
//!   (tape protocol 33): immigrants arrive as a Poisson count per second at
//!   the rate times `1 - n`, placed uniformly inside it, and every parent,
//!   immigrant or child, spawns Poisson(`n_j`) children at exponential
//!   offsets `tau_j` for each declared kernel, `n = sum n_j < 1`. The mean
//!   rate stays on the envelope; what the branching adds is the clustering
//!   the real tape shows inside a second, a fifth of all gaps under a
//!   millisecond at any hour and hundred-millisecond bins dispersed two to
//!   four times uniform. Children land in time order, may cross into later
//!   seconds, and are dropped at a closure.
//! - The aggressor side is order splitting: `sign_slots` metaorders are live
//!   at once, each with a side and a remaining print count drawn from a
//!   discrete Pareto tail of exponent `sign_alpha`; a parent repeats the
//!   previous side with probability `sign_repeat`, else takes a uniformly
//!   chosen slot's side and decrements it, an exhausted slot redrawing. This
//!   is what gives the real sign memory, an autocorrelation of 0.13 at one
//!   parent that is still 0.01 at fifty, which no Markov chain can.
//! - A parent has an impact on the mid (tape protocol 34), a propagator:
//!   it kicks the mid `permanent + transient` ticks in its own direction
//!   and the transient part decays per later parent, which this module
//!   hands the source as a move in ticks with each parent. With the sign
//!   memory above, the response to a parent grows from half a tick to two
//!   thirds over ten parents and stays, as the real one does.
//! - Each parent moves the log mid by `sigma * t_nu`, a standardised
//!   Student-t innovation, with `sigma = event_log_sigma * r / sqrt(v) *
//!   level_sigma`. The minute variance then follows the count, which is the
//!   time change the real range residual demands: its correlation with the
//!   volume residual is 0.74 where the square-root law predicts 0.75. No
//!   drift and no bounce regime: from a minute up the mid is a martingale,
//!   as the real close series is at every horizon from a minute to a
//!   session; below a minute the propagator above gives it the real
//!   tape's short memory.
//! - Jumps arrive at a rate proportional to the parent rate and move the mid
//!   by a lognormal multiple of the reference minute sd. They are the news
//!   component the summed innovations cannot make: the largest minute of a
//!   session, and the standardised minute kurtosis of nine.
//! - Every scheduled reopen applies a gap, lognormal around the session's
//!   sigma level, at the first parent after the closure.
//!
//! State is a dozen floats, the current second's bucket, the heap of
//! children still owed and the sign slots, all `Clone`, so the checkpoint
//! chain and the seek work unchanged.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::TapeRng;
use mogwai_protocol::AggressorSide;
use serde::Deserialize;

use std::sync::Arc;

use super::calendar::CompiledCalendar;
use super::children::{half_open_unit, normal_quantile, open_unit};
use super::consts::INTRA_EVENT_STEP_NS;
use super::draws::{POISSON_CEILING, coin, poisson_inverse, student_t_integer, unit_exponential};
use super::fingerprint::ScalarError;

const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_MINUTE: u64 = 60 * NS_PER_SECOND;
const MINUTES_PER_WEEK: u64 = 7 * 24 * 60;
/// Ceiling on the number of cascade components a preset may declare in
/// either group. Eight spans seconds to months at a factor of five per step.
pub const MAX_CASCADE_COMPONENTS: usize = 8;
/// Ceiling on the parents drawn into one second. A rate this high is a
/// misconfiguration, not a market; the cap keeps a runaway texture draw from
/// owing a million-child second to the venue. It is also the Poisson draws'
/// own ceiling, so the rate clamp and the count clamp are one number.
const MAX_PARENTS_PER_SECOND: f64 = POISSON_CEILING;
/// The supported Student-t degrees of freedom: integers, so the chi-squared
/// is a fixed number of uniforms with no rejection (tape protocol 39). The
/// lower end keeps the variance finite; the upper bounds the parent packet.
const STUDENT_DF_RANGE: std::ops::RangeInclusive<u32> = 3..=64;

/// The cascade's [`TapeRng`] domains (tape protocol 39). Each unit of work
/// reads a fixed number of words from its own domain, so where a unit's
/// words sit is a pure function of how many units of its kind came before:
/// an allocation count along the trajectory, not wall time and not the
/// printed-parent index. Work drawn and later discarded - an empty window,
/// an offspring past the heap ceiling, a staged immigrant a halt drops -
/// keeps its words spent; no counter is ever rewound.
const INIT_DOMAIN: u64 = 0x6361_7369_6E69_7439;
const COMPONENT_DOMAIN: u64 = 0x6361_7363_6F6D_7039;
const WINDOW_DOMAIN: u64 = 0x6361_7377_696E_6439;
const JUMP_DOMAIN: u64 = 0x6361_736A_756D_7039;
const IMMIGRANT_DOMAIN: u64 = 0x6361_7369_6D6D_6939;
const OFFSPRING_DOMAIN: u64 = 0x6361_736F_6666_7339;
const PARENT_DOMAIN: u64 = 0x6361_7370_6172_6E39;

/// The longest parent packet: four kernels, the t words at the largest
/// supported degrees of freedom, and the eight fixed words.
const MAX_PARENT_WORDS: usize = MAX_EXCITATION_KERNELS + 1 + 32 + 1 + 8;
/// Bound on a single parent's log move, jump and gap included. Twenty
/// percent in one event is far outside anything the fitted tails produce and
/// keeps the mid finite under any declared configuration.
pub(super) const MAX_EVENT_LOG_MOVE: f64 = 0.2;
/// Ceiling on the excitation kernels a preset may declare.
const MAX_EXCITATION_KERNELS: usize = 4;
/// Ceiling on the total branching ratio. Past it the cluster sizes, whose
/// mean is `1 / (1 - n)`, run into the hundreds and the process is a
/// misconfiguration rather than a market.
const MAX_BRANCHING_RATIO: f64 = 0.95;
/// Ceiling on children owed and not yet printed. The stationary backlog at
/// the busiest configured rate is a few hundred; a heap this deep means a
/// runaway kernel, and the excess is dropped rather than grown.
const MAX_PENDING_CHILDREN: usize = 65_536;
/// Ceiling on a metaorder's print count, so a Pareto draw at a tiny uniform
/// cannot pin one side for a session.
const MAX_METAORDER_PRINTS: f64 = 1_000_000.0;
/// Ceiling on the live metaorder slots.
const MAX_SIGN_SLOTS: u32 = 64;

/// The `[instrument.generator.cascade]` table. Every field is a knob with
/// its own provenance entry in the preset.
///
/// `Serialize` exists for one consumer: the river-identity digest hashes
/// the canonical serialization, because every field here moves tape bytes
/// and a key that omitted them would let two different cascades collide in
/// the river cache.
#[derive(Debug, Clone, PartialEq, Deserialize, serde::Serialize)]
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
    /// Timescale, seconds, of the fast texture: the rate's swell inside a
    /// minute, independent of the minute-fitted texture above.
    pub fast_texture_tau_s: f64,
    /// Log-sd of the fast texture. Zero disables it.
    pub fast_texture_log_sd: f64,
    /// Branching ratio per excitation kernel, `n_j`: the mean children a
    /// parent spawns at that kernel's timescale. Their sum is below one.
    /// Empty declares no excitation: parents are the immigrants alone.
    pub excitation_ratio: Vec<f64>,
    /// Timescale, seconds, of each excitation kernel's exponential offset.
    pub excitation_tau_s: Vec<f64>,
    /// Live metaorders whose sides the parents draw from.
    pub sign_slots: u32,
    /// Tail exponent of a metaorder's print count, `P(L >= l) = l^-alpha`.
    pub sign_alpha: f64,
    /// Probability a parent repeats the previous parent's side outright
    /// before consulting a slot.
    pub sign_repeat: f64,
    /// The propagator: a parent moves the mid by `permanent + transient`
    /// ticks in its own direction, and the transient part decays at
    /// `impact_transient_decay` per later parent, so the mid's impact
    /// component after parent `k` is `permanent * sum(s) + transient * R`,
    /// `R = decay * R + s`. With the sign memory the splitting model
    /// carries, this gives the real response: half a tick one parent
    /// later, growing to two thirds by ten as the same side keeps coming,
    /// and flat past that as the transient decays against it. The variance
    /// the term supplies over a minute is a share of the diffusive one and
    /// is fitted out of `event_log_sigma`.
    pub impact_permanent_ticks: f64,
    pub impact_transient_ticks: f64,
    pub impact_transient_decay: f64,
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
        if !self.fast_texture_tau_s.is_finite() || self.fast_texture_tau_s <= 0.0 {
            return Err(ScalarError::detailed("cascade", "fast_texture_tau_s"));
        }
        if !finite_nonnegative(self.fast_texture_log_sd) || self.fast_texture_log_sd > 3.0 {
            return Err(ScalarError::detailed("cascade", "fast_texture_log_sd"));
        }
        if self.excitation_ratio.len() != self.excitation_tau_s.len()
            || self.excitation_ratio.len() > MAX_EXCITATION_KERNELS
            || !self.excitation_ratio.iter().all(|n| finite_nonnegative(*n))
            || !self
                .excitation_tau_s
                .iter()
                .all(|tau| tau.is_finite() && *tau > 0.0)
        {
            return Err(ScalarError::detailed(
                "cascade",
                "excitation kernels need matching non-negative ratios and positive timescales",
            ));
        }
        if self.excitation_ratio.iter().sum::<f64>() > MAX_BRANCHING_RATIO {
            return Err(ScalarError::detailed(
                "cascade",
                "the excitation ratios sum past the branching ceiling",
            ));
        }
        if self.sign_slots == 0 || self.sign_slots > MAX_SIGN_SLOTS {
            return Err(ScalarError::detailed("cascade", "sign_slots"));
        }
        if !self.sign_alpha.is_finite() || self.sign_alpha <= 1.0 || self.sign_alpha > 10.0 {
            return Err(ScalarError::detailed("cascade", "sign_alpha"));
        }
        if !self.sign_repeat.is_finite() || !(0.0..1.0).contains(&self.sign_repeat) {
            return Err(ScalarError::detailed("cascade", "sign_repeat"));
        }
        if !finite_nonnegative(self.impact_permanent_ticks)
            || self.impact_permanent_ticks > 10.0
            || !finite_nonnegative(self.impact_transient_ticks)
            || self.impact_transient_ticks > 10.0
            || !self.impact_transient_decay.is_finite()
            || !(0.0..1.0).contains(&self.impact_transient_decay)
        {
            return Err(ScalarError::detailed("cascade", "impact"));
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
        // a finite variance, and drawn from a fixed number of uniforms, which
        // needs an integer. A fractional value is refused, never rounded.
        if !self.student_df.is_finite()
            || self.student_df.fract() != 0.0
            || !(f64::from(*STUDENT_DF_RANGE.start())..=f64::from(*STUDENT_DF_RANGE.end()))
                .contains(&self.student_df)
        {
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
        Ok(())
    }

    /// The total branching ratio: the share of parents that are children.
    fn branching_ratio(&self) -> f64 {
        self.excitation_ratio.iter().sum()
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
    /// The propagator's move for this parent, in ticks: its own kick less
    /// the decay of what earlier parents left in the transient register.
    pub(super) impact_ticks: f64,
    /// The sweep's child-count words, branch then geometric, from the
    /// parent's own packet. The placed path turns them into its count; the
    /// discrete book sizes its parent its own way and leaves them unread.
    pub(super) count_words: [u64; 2],
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
    /// The generator's own compiled calendar, shared: the walk asks it
    /// whether a second is open, where the next open is and which session
    /// minute it falls in, every second.
    calendar: Arc<CompiledCalendar>,
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
    /// `student_df` as the integer the validation proved it is.
    student_df: u32,
    one_second: OneSecondSteps,
}

impl CascadeTables {
    /// Words one component step reads: every texture component, the fast
    /// one, every level and every sigma-extra component. The steps read them
    /// one per component; this is the stride the allocation pin checks.
    #[cfg(test)]
    fn component_words(&self) -> usize {
        self.texture_tau_s.len() + 1 + 2 * self.level_tau_s.len()
    }

    /// Words one parent reads, in packet order: a count per excitation
    /// kernel; the t numerator, `df / 2` chi-squared words and an odd word
    /// when `df` is odd; the gap normal and sign; the repeat coin, the slot,
    /// the renewal side and the renewal Pareto; the sweep's branch and
    /// geometric words. Every word is drawn whether or not its branch runs.
    fn parent_words(&self) -> usize {
        let df = self.student_df as usize;
        self.config.excitation_ratio.len() + 1 + df / 2 + df % 2 + 8
    }
}

/// Every component's `ar1_coefficients` at a one-second step, the step the
/// walk takes on every open second, solved once rather than an `exp` and a
/// `sqrt` per component per second. `level` serves the sigma-extra
/// components too, which share the level taus.
#[derive(Clone)]
struct OneSecondSteps {
    texture: Vec<(f64, f64)>,
    fast: (f64, f64),
    level: Vec<(f64, f64)>,
}

impl CascadeTables {
    pub(super) fn new(
        config: &CascadeConfig,
        compiled: &Arc<CompiledCalendar>,
        mean_event_duration_s: f64,
    ) -> Result<Self, ScalarError> {
        config.validate()?;
        let calendar = compiled.calendar();
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
        let texture_tau_s: Vec<f64> = config
            .texture_tau_minutes
            .iter()
            .map(|t| t * 60.0)
            .collect();
        let level_tau_s: Vec<f64> = config.level_tau_minutes.iter().map(|t| t * 60.0).collect();
        let one_second = OneSecondSteps {
            texture: texture_tau_s
                .iter()
                .map(|tau| ar1_coefficients(1.0, *tau))
                .collect(),
            fast: ar1_coefficients(1.0, config.fast_texture_tau_s),
            level: level_tau_s
                .iter()
                .map(|tau| ar1_coefficients(1.0, *tau))
                .collect(),
        };
        Ok(Self {
            config: config.clone(),
            calendar: Arc::clone(compiled),
            arrival,
            texture_s,
            sigma_shape,
            weekday: envelope.weekday_weight,
            texture_tau_s,
            level_tau_s,
            one_second,
            texture_sqrt_w: config.texture_weights.iter().map(|w| w.sqrt()).collect(),
            level_sqrt_w: config.level_weights.iter().map(|w| w.sqrt()).collect(),
            parents_per_second,
            session_seconds: minutes as f64 * 60.0,
            reference_log_sd: config.event_log_sigma * (parents_per_second * 60.0).sqrt(),
            student_scale,
            student_df: config.student_df as u32,
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
    /// The fast texture component.
    fast: f64,
    /// An immigrant drawn by order statistic and not yet printed, because
    /// a child came first.
    staged_immigrant_ns: Option<u64>,
    /// Children owed by parents already printed, earliest first.
    children: BinaryHeap<Reverse<u64>>,
    /// The live metaorders: side and prints remaining.
    slots: Vec<(AggressorSide, u32)>,
    last_side: AggressorSide,
    /// The propagator's transient register: the decayed sum of past signs.
    impact_register: f64,
    /// Where each kind of work reads its words. Their positions are the
    /// allocation counters, so they clone with the walk and a checkpoint
    /// resumes every one of them where the lead was.
    streams: CascadeStreams,
    /// Component steps, evaluated windows and jumps, counted where each
    /// happens and apart from any word read, so the allocation pin can hold
    /// the words consumed against the units that consumed them.
    #[cfg(test)]
    units: [u64; 3],
}

/// The cascade's per-kind streams, one [`TapeRng`] per domain of the river.
#[derive(Clone)]
struct CascadeStreams {
    component: TapeRng,
    window: TapeRng,
    jump: TapeRng,
    immigrant: TapeRng,
    offspring: TapeRng,
    parent: TapeRng,
}

impl CascadeStreams {
    fn new(seed: u64) -> Self {
        Self {
            component: TapeRng::new(seed, COMPONENT_DOMAIN),
            window: TapeRng::new(seed, WINDOW_DOMAIN),
            jump: TapeRng::new(seed, JUMP_DOMAIN),
            immigrant: TapeRng::new(seed, IMMIGRANT_DOMAIN),
            offspring: TapeRng::new(seed, OFFSPRING_DOMAIN),
            parent: TapeRng::new(seed, PARENT_DOMAIN),
        }
    }
}

/// A standard normal from one word: the normal quantile of an open uniform.
fn normal_word(rng: &mut TapeRng) -> f64 {
    normal_quantile(open_unit(rng.next_word()))
}

impl CascadeState {
    /// The walk at `start_ts` for the river seeded `seed`. The components
    /// start from their stationary law and the sign slots from theirs, all
    /// read from the river's initialisation domain, so no other stream is
    /// touched by construction.
    pub(super) fn new(tables: &CascadeTables, seed: u64, start_ts: u64) -> Self {
        let mut init = TapeRng::new(seed, INIT_DOMAIN);
        let mut draw = |n: usize| -> Vec<f64> { (0..n).map(|_| normal_word(&mut init)).collect() };
        let texture = draw(tables.texture_tau_s.len());
        let level = draw(tables.level_tau_s.len());
        let sigma_extra = draw(tables.level_tau_s.len());
        let fast = normal_word(&mut init);
        let slots = (0..tables.config.sign_slots)
            .map(|_| {
                let side = coin_side(init.next_word());
                (
                    side,
                    metaorder_prints(init.next_word(), tables.config.sign_alpha),
                )
            })
            .collect();
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
            fast,
            staged_immigrant_ns: None,
            children: BinaryHeap::new(),
            slots,
            last_side: AggressorSide::Buyer,
            impact_register: 0.0,
            streams: CascadeStreams::new(seed),
            #[cfg(test)]
            units: [0; 3],
        }
    }

    /// Forget the current bucket: the clock has been moved past it by
    /// something outside the cascade (an armed halt). Children owed before
    /// the new clock would have printed inside the halt, so they are gone.
    ///
    /// The components do not evolve across the halt: the next fill starts at
    /// the new clock with the values they had, and no component step is
    /// allocated for the jump. That is the halt model as it stands, not an
    /// accident of the addressing; a halt that should age the activity would
    /// need its own transition and its own component step. A staged immigrant
    /// is dropped with its word spent; the window's unplaced immigrants were
    /// never allocated and cost nothing.
    pub(super) fn reset_bucket(&mut self, clock_ns: u64) {
        self.remaining = 0;
        self.staged_immigrant_ns = None;
        self.drop_children_before(clock_ns);
        self.second_ns = clock_ns / NS_PER_SECOND * NS_PER_SECOND;
        self.filled = false;
    }

    /// Whether a parent is still owed inside the current second: an
    /// immigrant not yet placed, one staged, or a child landing before the
    /// second ends.
    pub(super) fn has_parent(&self) -> bool {
        self.remaining > 0
            || self.staged_immigrant_ns.is_some()
            || self
                .next_child_ns()
                .is_some_and(|ts| ts < self.window_end_ns)
    }

    fn next_child_ns(&self) -> Option<u64> {
        self.children.peek().map(|Reverse(ts)| *ts)
    }

    fn drop_children_before(&mut self, clock_ns: u64) {
        while self.next_child_ns().is_some_and(|ts| ts < clock_ns) {
            self.children.pop();
        }
    }

    /// One component step over `elapsed_s`: one allocation of
    /// `component_words` words, one per component in a fixed order.
    fn step_components(&mut self, tables: &CascadeTables, elapsed_s: f64) {
        #[cfg(test)]
        {
            self.units[0] += 1;
        }
        let coefficients = |tau: f64| ar1_coefficients(elapsed_s, tau);
        let rng = &mut self.streams.component;
        for (y, tau) in self.texture.iter_mut().zip(&tables.texture_tau_s) {
            *y = ar1_step(*y, coefficients(*tau), rng);
        }
        let fast = coefficients(tables.config.fast_texture_tau_s);
        self.fast = ar1_step(self.fast, fast, rng);
        for (y, tau) in self.level.iter_mut().zip(&tables.level_tau_s) {
            *y = ar1_step(*y, coefficients(*tau), rng);
        }
        for (y, tau) in self.sigma_extra.iter_mut().zip(&tables.level_tau_s) {
            *y = ar1_step(*y, coefficients(*tau), rng);
        }
    }

    /// `step_components(.., 1.0)` over the coefficients the tables solved
    /// once for a one-second step, which is every step but a closure's.
    /// Word order and arithmetic are the general step's.
    fn step_components_one_second(&mut self, tables: &CascadeTables) {
        #[cfg(test)]
        {
            self.units[0] += 1;
        }
        let one = &tables.one_second;
        let rng = &mut self.streams.component;
        for (y, c) in self.texture.iter_mut().zip(&one.texture) {
            *y = ar1_step(*y, *c, rng);
        }
        self.fast = ar1_step(self.fast, one.fast, rng);
        for (y, c) in self.level.iter_mut().zip(&one.level) {
            *y = ar1_step(*y, *c, rng);
        }
        for (y, c) in self.sigma_extra.iter_mut().zip(&one.level) {
            *y = ar1_step(*y, *c, rng);
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
        clock_ns: u64,
        modifiers: SecondModifiers,
    ) -> Result<(), CascadeRefusal> {
        let config = &tables.config;
        loop {
            if self.filled {
                let Some(next) = self.second_ns.checked_add(NS_PER_SECOND) else {
                    return Err(CascadeRefusal::ClockExhausted);
                };
                self.step_components_one_second(tables);
                self.second_ns = next;
            }
            self.filled = true;
            if !tables.calendar.is_open(self.second_ns) {
                let reopen = tables.calendar.next_open_ns(self.second_ns);
                if reopen == u64::MAX {
                    return Err(CascadeRefusal::ClockExhausted);
                }
                let elapsed_s = (reopen - self.second_ns) as f64 / NS_PER_SECOND as f64;
                self.step_components(tables, elapsed_s);
                // Children owed inside the closure would print into it;
                // the closure wins and they are dropped.
                self.drop_children_before(reopen);
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
            let fast_s = config.fast_texture_log_sd;
            let base_rate = slow_rate
                * (s * self.texture_sum(tables) - 0.5 * s * s + fast_s * self.fast
                    - 0.5 * fast_s * fast_s)
                    .exp();
            let rate = base_rate * modifiers.rate_mult / modifiers.arrival_thin;
            let window_s = (window_end - window_start) as f64 / NS_PER_SECOND as f64;
            // The window's two words, the immigrant count and the jump count,
            // allocated for every window evaluated here, empty or not, and
            // whether or not the preset declares jumps.
            #[cfg(test)]
            {
                self.units[1] += 1;
            }
            let count_word = self.streams.window.next_word();
            let jump_word = self.streams.window.next_word();
            // The immigrants: the branching fills the rest of the rate with
            // children, so the mean count stays on the envelope.
            let immigrant_rate = rate * (1.0 - config.branching_ratio());
            let count = poisson_inverse(
                half_open_unit(count_word),
                (immigrant_rate * window_s).min(MAX_PARENTS_PER_SECOND),
            );
            let level_sigma = self.level_sigma(tables);
            self.second_sigma = config.event_log_sigma
                * tables.sigma_shape[minute]
                * level_sigma
                * modifiers.vol_mult;
            if config.jumps_per_session > 0.0 {
                let jump_rate = config.jumps_per_session * slow_rate * window_s
                    / (tables.parents_per_second * tables.session_seconds);
                let jumps = poisson_inverse(half_open_unit(jump_word), jump_rate);
                // The minute sd where the jump lands, against the reference
                // minute at the same sigma level; the jump follows the ratio
                // to the declared power, so a night jump is smaller than a
                // cash-open one but not in full proportion.
                let reference_sd = tables.reference_log_sd * level_sigma;
                let local_minute_sd = self.second_sigma * (60.0 * base_rate).sqrt();
                let local_scale = (local_minute_sd / reference_sd).powf(config.jump_local_exponent);
                for _ in 0..jumps {
                    #[cfg(test)]
                    {
                        self.units[2] += 1;
                    }
                    // A jump's two words, its size normal and its sign.
                    let draw = normal_word(&mut self.streams.jump)
                        .clamp(-config.jump_log_clamp_sd, config.jump_log_clamp_sd);
                    let magnitude = config.jump_size
                        * reference_sd
                        * local_scale
                        * (config.jump_log_sd * draw).exp();
                    let sign = if coin(self.streams.jump.next_word(), 0.5) {
                        1.0
                    } else {
                        -1.0
                    };
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
            // A child owed before the window opens is one the clock has
            // already passed; it prints at the window's first instant.
            if self.has_parent() {
                return Ok(());
            }
        }
    }

    /// The next immigrant's instant inside the current second, staged so a
    /// child landing earlier can print first without disturbing the order
    /// statistics that place the immigrants.
    fn stage_immigrant(&mut self) -> Option<u64> {
        if self.staged_immigrant_ns.is_none() && self.remaining > 0 {
            // Sequential order statistics: with n parents left in the
            // window, the next lands after a fraction 1 - u^(1/n) of what
            // remains, written as -expm1(ln(u) / n) so a large n does not
            // cancel. One immigrant word per staging, never per parent: a
            // child printing first leaves the staged instant in place.
            let n = f64::from(self.remaining);
            let span = self.window_end_ns.saturating_sub(self.next_from_ns) as f64;
            let u = open_unit(self.streams.immigrant.next_word());
            let offset = span * -(u.ln() / n).exp_m1();
            let ts_ns = self.next_from_ns.saturating_add(offset as u64);
            self.next_from_ns = ts_ns;
            self.remaining -= 1;
            self.staged_immigrant_ns = Some(ts_ns);
        }
        self.staged_immigrant_ns
    }

    /// Spawn the children a parent at `ts_ns` owes: Poisson(`n_j`) per
    /// kernel, from the parent packet's one word per kernel, at exponential
    /// offsets, one offspring word each. Children past the heap ceiling are
    /// dropped, which only a runaway kernel reaches; their words stay spent.
    fn spawn_children(&mut self, tables: &CascadeTables, count_words: &[u64], ts_ns: u64) {
        let config = &tables.config;
        let kernels = config.excitation_ratio.iter().zip(&config.excitation_tau_s);
        for ((ratio, tau), word) in kernels.zip(count_words) {
            let count = poisson_inverse(half_open_unit(*word), *ratio);
            for _ in 0..count {
                let offset_s = tau * unit_exponential(self.streams.offspring.next_word());
                let offset_ns = (offset_s * NS_PER_SECOND as f64).round();
                if !offset_ns.is_finite() || self.children.len() >= MAX_PENDING_CHILDREN {
                    continue;
                }
                let Some(child_ns) = ts_ns.checked_add(offset_ns as u64) else {
                    continue;
                };
                self.children.push(Reverse(child_ns));
            }
        }
    }

    /// The propagator's move for a parent of `side`, in ticks, and the
    /// register's step: the kick on its own sign less the decay of the
    /// transient the earlier parents left.
    fn impact_move(&mut self, tables: &CascadeTables, side: AggressorSide) -> f64 {
        let config = &tables.config;
        let sign = sign_of(side);
        let decay = config.impact_transient_decay;
        let previous = self.impact_register;
        self.impact_register = decay * previous + sign;
        (config.impact_permanent_ticks + config.impact_transient_ticks) * sign
            - config.impact_transient_ticks * (1.0 - decay) * previous
    }

    /// The parent's aggressor side under the order-splitting model, from the
    /// packet's four side words: the repeat coin, the slot, and the renewal
    /// side and print count, which are read only when the slot is exhausted.
    /// A repeat leaves every slot untouched.
    fn draw_side(&mut self, tables: &CascadeTables, words: [u64; 4]) -> AggressorSide {
        let config = &tables.config;
        let [repeat, slot, renew_side, renew_prints] = words;
        if coin(repeat, config.sign_repeat) {
            return self.last_side;
        }
        let slots = self.slots.len();
        let k = ((half_open_unit(slot) * slots as f64) as usize).min(slots - 1);
        let (side, remaining) = &mut self.slots[k];
        if *remaining == 0 {
            *side = coin_side(renew_side);
            *remaining = metaorder_prints(renew_prints, config.sign_alpha);
        }
        *remaining -= 1;
        self.last_side = *side;
        *side
    }

    /// Draw the next parent of the current second. Requires `has_parent`.
    ///
    /// Every call allocates one parent packet of `parent_words` words, read
    /// whole up front whichever branches the parent takes, immigrant or
    /// child: the kernels' counts, the t words, the gap pair, the four side
    /// words and the two sweep words, in that order.
    pub(super) fn draw_parent(&mut self, tables: &CascadeTables, clock_ns: u64) -> CascadeParent {
        debug_assert!(self.has_parent(), "draw_parent needs a filled bucket");
        let config = &tables.config;
        let mut packet = [0_u64; MAX_PARENT_WORDS];
        let packet = &mut packet[..tables.parent_words()];
        for word in packet.iter_mut() {
            *word = self.streams.parent.next_word();
        }
        let (kernel_words, rest) = packet.split_at(config.excitation_ratio.len());
        let df = tables.student_df as usize;
        let (t_words, rest) = rest.split_at(1 + df / 2 + df % 2);
        let [
            gap_normal,
            gap_sign,
            repeat,
            slot,
            renew_side,
            renew_prints,
            branch,
            geometric,
        ] = *rest
        else {
            unreachable!("the packet's fixed tail is eight words")
        };
        let immigrant = self.stage_immigrant();
        let child = self.next_child_ns().filter(|ts| *ts < self.window_end_ns);
        let raw_ns = match (immigrant, child) {
            (Some(i), Some(c)) if c <= i => {
                self.children.pop();
                c
            }
            (None, Some(c)) => {
                self.children.pop();
                c
            }
            (Some(i), _) => {
                self.staged_immigrant_ns = None;
                i
            }
            (None, None) => unreachable!("has_parent held"),
        };
        let ts_ns = raw_ns.max(clock_ns.saturating_add(INTRA_EVENT_STEP_NS));
        self.spawn_children(tables, kernel_words, ts_ns);

        let (numerator, chi) = t_words
            .split_first()
            .expect("the t words open with the numerator");
        let (chi_words, odd_word) = if df % 2 == 1 {
            let (odd, even) = chi.split_last().expect("an odd df has its odd word");
            (even, Some(*odd))
        } else {
            (chi, None)
        };
        let innovation = student_t_integer(*numerator, chi_words, odd_word) / tables.student_scale;
        let mut log_move = self.second_sigma * innovation;
        if self.pending_jump != 0.0 {
            log_move += self.pending_jump;
            self.pending_jump = 0.0;
        }
        if self.pending_gap {
            self.pending_gap = false;
            let draw = normal_quantile(open_unit(gap_normal))
                .clamp(-config.gap_log_clamp_sd, config.gap_log_clamp_sd);
            let magnitude = config.gap_median_ratio
                * tables.reference_log_sd
                * self.level_sigma(tables)
                * (config.gap_log_sd * draw).exp();
            let sign = if coin(gap_sign, 0.5) { 1.0 } else { -1.0 };
            log_move += sign * magnitude;
        }
        self.started = true;
        let side = self.draw_side(tables, [repeat, slot, renew_side, renew_prints]);
        let impact_ticks = self.impact_move(tables, side);
        CascadeParent {
            ts_ns,
            log_move: log_move.clamp(-MAX_EVENT_LOG_MOVE, MAX_EVENT_LOG_MOVE),
            side,
            innovation,
            sigma: self.second_sigma,
            impact_ticks,
            count_words: [branch, geometric],
        }
    }
}

/// An Ornstein-Uhlenbeck component's exact discretisation over `elapsed_s`:
/// the decay `rho` and the innovation scale `sqrt(1 - rho^2)` that keeps the
/// component at unit stationary variance.
fn ar1_coefficients(elapsed_s: f64, tau: f64) -> (f64, f64) {
    let rho = (-elapsed_s / tau).exp();
    (rho, (1.0 - rho * rho).sqrt())
}

fn ar1_step(y: f64, (rho, innovation_scale): (f64, f64), rng: &mut TapeRng) -> f64 {
    rho * y + innovation_scale * normal_word(rng)
}

fn sign_of(side: AggressorSide) -> f64 {
    match side {
        AggressorSide::Buyer => 1.0,
        AggressorSide::Seller => -1.0,
        AggressorSide::NoAggressor => 0.0,
    }
}

/// A side from one word: buyer on an even coin.
fn coin_side(word: u64) -> AggressorSide {
    if coin(word, 0.5) {
        AggressorSide::Buyer
    } else {
        AggressorSide::Seller
    }
}

/// A metaorder's print count from one word: discrete Pareto,
/// `P(L >= l) = l^-alpha`, by inversion of a half-open uniform.
fn metaorder_prints(word: u64, alpha: f64) -> u32 {
    let u = half_open_unit(word);
    let draw = (1.0 - u).powf(-1.0 / alpha).floor();
    draw.clamp(1.0, MAX_METAORDER_PRINTS) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::calendar::{SessionCalendar, SessionEnvelope, WeeklyWindow};

    /// The tables over a calendar compiled for the purpose, as the
    /// generator compiles its own.
    fn build_tables(
        config: &CascadeConfig,
        calendar: &SessionCalendar,
        mean_event_duration_s: f64,
    ) -> Result<CascadeTables, ScalarError> {
        CascadeTables::new(
            config,
            &Arc::new(CompiledCalendar::new(calendar)),
            mean_event_duration_s,
        )
    }

    pub(crate) fn config() -> CascadeConfig {
        CascadeConfig {
            texture_tau_minutes: vec![0.25, 1.0, 5.0, 25.0, 90.0],
            texture_weights: vec![0.52, 0.135, 0.18, 0.15, 0.015],
            texture_amplitude: 0.54,
            texture_exponent: 0.24,
            fast_texture_tau_s: 3.0,
            fast_texture_log_sd: 0.55,
            excitation_ratio: vec![0.2, 0.1, 0.3],
            excitation_tau_s: vec![0.0003, 0.03, 1.0],
            sign_slots: 5,
            sign_alpha: 2.2,
            sign_repeat: 0.08,
            impact_permanent_ticks: 0.3,
            impact_transient_ticks: 0.15,
            impact_transient_decay: 0.98,
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
        let Err(error) = build_tables(&config(), &calendar, 0.08) else {
            panic!("a cascade without an envelope was accepted");
        };
        assert_eq!(error.field, "cascade");
        assert!(error.detail.unwrap().contains("envelope"));
    }

    #[test]
    fn mismatched_or_unnormalised_components_are_refused() {
        let mut bad = config();
        bad.texture_weights.pop();
        let Err(error) = build_tables(&bad, &calendar(vec![1.0; 1_380], vec![1.0; 1_380]), 0.08)
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
        let tables = build_tables(
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
        let tables = build_tables(&config(), &calendar(volume, vec![1.0; 1_380]), 0.08).unwrap();
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
        let tables = build_tables(&config, &calendar, 0.08).unwrap();
        // Thursday 1970-01-01 is local day 4. Start ten seconds before the
        // Thursday 16:00 close so parents certainly land before it (a
        // single second can draw an empty count under a low texture) and
        // the walk then crosses the maintenance hour.
        let start = 959 * NS_PER_MINUTE + 50 * NS_PER_SECOND;
        let mut state = CascadeState::new(&tables, 7, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let mut parents: Vec<CascadeParent> = Vec::new();
        let reopen_ns = 1_020 * NS_PER_MINUTE;
        // Walk until two parents have printed past the reopen: the last
        // second before the close holds a dozen parents and their children,
        // and the closure sits between them and the first reopened one.
        while parents.iter().filter(|p| p.ts_ns >= reopen_ns).count() < 2 {
            if !state.has_parent() {
                state.fill_next_bucket(&tables, clock, modifiers).unwrap();
            }
            let parent = state.draw_parent(&tables, clock);
            clock = parent.ts_ns;
            parents.push(parent);
        }
        // The gap is fifty reference sds and the ordinary move is a fraction
        // of one, so the first parent after the reopen is unmistakable.
        let threshold = 10.0 * tables.reference_log_sd;
        let after: Vec<_> = parents.iter().filter(|p| p.ts_ns >= reopen_ns).collect();
        let before: Vec<_> = parents.iter().filter(|p| p.ts_ns < reopen_ns).collect();
        assert!(
            !after.is_empty() && !before.is_empty(),
            "before {} after {} first {:?}",
            before.len(),
            after.len(),
            parents.first().map(|p| p.ts_ns)
        );
        assert!(after[0].log_move.abs() > threshold, "{:?}", after[0]);
        assert!(after[1..].iter().all(|p| p.log_move.abs() < threshold));
        assert!(before.iter().all(|p| p.log_move.abs() < threshold));
        // Nothing printed inside the closure.
        assert!(parents.iter().all(|p| calendar.is_open(p.ts_ns)));
    }

    /// The positions of every cascade stream, in a fixed order.
    fn positions(state: &CascadeState) -> [u64; 6] {
        let s = &state.streams;
        [
            s.component.position(),
            s.window.position(),
            s.jump.position(),
            s.immigrant.position(),
            s.offspring.position(),
            s.parent.position(),
        ]
    }

    /// Every unit of work allocates its fixed stride and nothing else: a
    /// parent its packet, an offspring one word per child its packet's
    /// kernel words count, an immigrant one word exactly when one is staged
    /// (never when a child prints ahead of a staged immigrant), a window its
    /// pair, a jump its pair and a component step its full set. The
    /// per-second units are held against counts taken where each unit
    /// happens; the per-parent ones against what the packet says. Checked
    /// call by call over walks that cross closures, jumps, children printing
    /// ahead of staged immigrants and windows that owe nothing, at an even
    /// and an odd `student_df` and with jumps on and off. Not covered here:
    /// the initialisation domain, which nothing reads twice, and the heap
    /// ceiling's drop, which only a runaway kernel reaches.
    #[test]
    fn every_unit_allocates_its_fixed_stride() {
        for (jumps_per_session, student_df) in [(200.0, 4.0), (0.0, 5.0)] {
            check_unit_strides(jumps_per_session, student_df);
        }
    }

    fn check_unit_strides(jumps_per_session: f64, student_df: f64) {
        let mut config = config();
        config.jumps_per_session = jumps_per_session;
        config.student_df = student_df;
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = build_tables(&config, &calendar, 0.08).unwrap();
        let start = 959 * NS_PER_MINUTE;
        let mut state = CascadeState::new(&tables, 11, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let component = tables.component_words() as u64;
        let packet = tables.parent_words() as u64;
        let (mut reused, mut staged, mut children_first) = (0, 0, 0);
        let mut clock = start;
        for parent_index in 0..60_000_u64 {
            if !state.has_parent() {
                let before = positions(&state);
                state.fill_next_bucket(&tables, clock, modifiers).unwrap();
                let after = positions(&state);
                let [steps, windows, jumps] = state.units;
                assert_eq!(after[0], steps * component, "component words against steps");
                assert_eq!(after[1], 2 * windows, "window words against windows");
                assert_eq!(after[2], 2 * jumps, "jump words against jumps");
                assert!(after[1] > before[1], "a fill evaluated no window");
                assert_eq!(after[3..], before[3..], "a fill touched per-parent streams");
            }
            // The kernel counts this parent's packet will produce, read from
            // a clone of its stream before the draw.
            let mut peek = state.streams.parent.clone();
            let expected_offspring: u64 = config
                .excitation_ratio
                .iter()
                .map(|ratio| u64::from(poisson_inverse(half_open_unit(peek.next_word()), *ratio)))
                .sum();
            let will_stage = state.staged_immigrant_ns.is_none() && state.remaining > 0;
            let had_staged = state.staged_immigrant_ns.is_some();
            let before = positions(&state);
            let parent = state.draw_parent(&tables, clock);
            let after = positions(&state);
            assert_eq!(after[5] - before[5], packet, "parent {parent_index}");
            assert_eq!(
                after[4] - before[4],
                expected_offspring,
                "parent {parent_index}"
            );
            assert_eq!(
                after[3] - before[3],
                u64::from(will_stage),
                "parent {parent_index}"
            );
            assert_eq!(
                after[..3],
                before[..3],
                "a parent touched per-second streams"
            );
            staged += u64::from(will_stage);
            reused += u64::from(had_staged);
            children_first += u64::from(state.staged_immigrant_ns.is_some());
            clock = parent.ts_ns;
        }
        assert_eq!(
            positions(&state)[3],
            staged,
            "immigrant words are one per staging"
        );
        assert!(reused > 0, "no staged immigrant was ever reused");
        assert!(
            children_first > 0,
            "no child ever printed ahead of a staged immigrant"
        );
        assert_eq!(
            state.units[2] > 0,
            jumps_per_session > 0.0,
            "jumps were drawn exactly when declared"
        );
        assert!(state.units[0] > 0 && state.units[1] > 0);
    }

    /// A clone taken with a staged immigrant and children pending continues
    /// exactly as the original, parent for parent and position for position.
    #[test]
    fn a_clone_mid_walk_continues_exactly() {
        let config = config();
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = build_tables(&config, &calendar, 0.08).unwrap();
        let start = 1_020 * NS_PER_MINUTE;
        let mut state = CascadeState::new(&tables, 5, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let step = |state: &mut CascadeState, clock: &mut u64| {
            if !state.has_parent() {
                state.fill_next_bucket(&tables, *clock, modifiers).unwrap();
            }
            let parent = state.draw_parent(&tables, *clock);
            *clock = parent.ts_ns;
            (
                parent.ts_ns,
                parent.log_move.to_bits(),
                parent.side,
                parent.count_words,
                parent.impact_ticks.to_bits(),
                parent.innovation.to_bits(),
                parent.sigma.to_bits(),
                positions(state),
            )
        };
        let found = (0..100_000).any(|_| {
            step(&mut state, &mut clock);
            state.staged_immigrant_ns.is_some() && !state.children.is_empty()
        });
        assert!(
            found,
            "no instant with a staged immigrant and pending children"
        );
        let mut twin = state.clone();
        let mut twin_clock = clock;
        for _ in 0..5_000 {
            assert_eq!(
                step(&mut state, &mut clock),
                step(&mut twin, &mut twin_clock)
            );
        }
    }

    /// A reset drops the staged immigrant with its word spent, rewinds no
    /// stream and steps no component: across a halt of several seconds the
    /// components keep their values until the next fill steps them once.
    #[test]
    fn a_reset_spends_and_rewinds_nothing() {
        let config = config();
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = build_tables(&config, &calendar, 0.08).unwrap();
        let start = 1_020 * NS_PER_MINUTE;
        let mut state = CascadeState::new(&tables, 9, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let found = (0..100_000).any(|_| {
            if !state.has_parent() {
                state.fill_next_bucket(&tables, clock, modifiers).unwrap();
            }
            clock = state.draw_parent(&tables, clock).ts_ns;
            state.staged_immigrant_ns.is_some()
        });
        assert!(found, "no immigrant was ever staged");
        let before = positions(&state);
        let components = (state.texture.clone(), state.level.clone(), state.fast);
        let halted = clock + 5 * NS_PER_SECOND;
        state.reset_bucket(halted);
        assert_eq!(positions(&state), before);
        assert_eq!(state.units[0] * tables.component_words() as u64, before[0]);
        assert_eq!(
            (state.texture.clone(), state.level.clone(), state.fast),
            components,
            "the halt aged the components"
        );
        assert!(state.staged_immigrant_ns.is_none());
        // The halted second is filled without a step first, as the origin
        // second is; only a window that owes nothing steps on from it.
        let windows = state.units[1];
        state.fill_next_bucket(&tables, halted, modifiers).unwrap();
        let after = positions(&state);
        assert_eq!(after[3], before[3], "a fill allocated an immigrant");
        assert_eq!(
            state.units[0] - before[0] / tables.component_words() as u64,
            state.units[1] - windows - 1,
            "a step for each window after the halted one, none for the halt"
        );
    }

    /// The t's degrees of freedom are integers from 3 to 64; a fractional
    /// value is refused rather than rounded.
    #[test]
    fn student_df_is_an_integer_in_its_range() {
        for df in [3.0, 4.0, 5.0, 64.0] {
            let mut fine = config();
            fine.student_df = df;
            assert!(fine.validate().is_ok(), "{df}");
        }
        for df in [2.0, 2.5, 4.5, 65.0, f64::NAN, f64::INFINITY] {
            let mut bad = config();
            bad.student_df = df;
            assert!(bad.validate().is_err(), "{df}");
        }
    }

    /// Walk parents from `start` until `end`, returning them in order.
    fn walk(config: &CascadeConfig, seed: u64, start: u64, end: u64) -> Vec<CascadeParent> {
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = build_tables(config, &calendar, 0.08).unwrap();
        let mut state = CascadeState::new(&tables, seed, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let mut parents = Vec::new();
        loop {
            if !state.has_parent() {
                state.fill_next_bucket(&tables, clock, modifiers).unwrap();
            }
            let parent = state.draw_parent(&tables, clock);
            if parent.ts_ns >= end {
                break;
            }
            assert!(parent.ts_ns > clock, "{} after {clock}", parent.ts_ns);
            clock = parent.ts_ns;
            parents.push(parent);
        }
        parents
    }

    #[test]
    fn excitation_keeps_the_mean_rate_on_the_envelope() {
        // Sixty percent of parents are children, and the immigrant rate is
        // thinned by the same share, so the count over ten hours is the
        // configured one within the texture's own spread.
        let mut config = config();
        config.level_log_sd = 0.0;
        config.sigma_level_log_sd = 0.0;
        config.jumps_per_session = 0.0;
        let start = 1_020 * NS_PER_MINUTE;
        let parents = walk(&config, 11, start, start + 600 * NS_PER_MINUTE);
        let expected = 12.5 * 600.0 * 60.0;
        let ratio = parents.len() as f64 / expected;
        assert!((0.9..1.1).contains(&ratio), "ratio {ratio}");
        // And the branching is visible: a fifth of the gaps are under a
        // millisecond, which a Poisson placement at twelve a second cannot
        // reach (its share is one percent).
        let short = parents
            .windows(2)
            .filter(|w| w[1].ts_ns - w[0].ts_ns < 1_000_000)
            .count();
        let share = short as f64 / parents.len() as f64;
        assert!(
            (0.12..0.30).contains(&share),
            "sub-millisecond share {share}"
        );
    }

    #[test]
    fn children_never_print_inside_a_closure() {
        // A one-second kernel at a high ratio owes children well past the
        // Friday close; every parent still lands on an open minute.
        let mut config = config();
        config.excitation_ratio = vec![0.9];
        config.excitation_tau_s = vec![5.0];
        config.jumps_per_session = 0.0;
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        // Friday 1970-01-02 is local day 5: start a minute before its 16:00
        // close and walk into the Sunday reopen.
        let start = (1_440 + 959) * NS_PER_MINUTE;
        let parents = walk(&config, 5, start, start + 3 * 1_440 * NS_PER_MINUTE);
        assert!(parents.iter().all(|p| calendar.is_open(p.ts_ns)));
        let reopen = (3 * 1_440 + 1_020) * NS_PER_MINUTE;
        assert!(
            parents.iter().any(|p| p.ts_ns >= reopen),
            "walked past the reopen"
        );
    }

    #[test]
    fn sides_carry_the_splitting_memory() {
        // Five slots and an eight percent repeat put the same-side share
        // near 0.57, and runs of ten or more happen more often than the
        // Markov chain at that share allows.
        let mut config = config();
        config.jumps_per_session = 0.0;
        let start = 1_020 * NS_PER_MINUTE;
        let parents = walk(&config, 3, start, start + 300 * NS_PER_MINUTE);
        let same = parents
            .windows(2)
            .filter(|w| w[0].side == w[1].side)
            .count() as f64
            / (parents.len() - 1) as f64;
        assert!((0.53..0.62).contains(&same), "same-side share {same}");
        let mut runs = Vec::new();
        let mut run = 1_usize;
        for w in parents.windows(2) {
            if w[0].side == w[1].side {
                run += 1;
            } else {
                runs.push(run);
                run = 1;
            }
        }
        let long = runs.iter().filter(|r| **r >= 10).count() as f64 / runs.len() as f64;
        let markov = same.powi(9);
        assert!(
            long > 1.3 * markov,
            "long runs {long} against markov {markov}"
        );
    }

    #[test]
    fn the_propagator_kicks_with_the_side_and_the_transient_reverts() {
        // Summed over the walk the transient part cancels, so the impact
        // component's drift is the permanent kick on the net sign; and a
        // parent's own move is in its own direction by the full kick less
        // a bounded decay of what came before.
        let mut config = config();
        config.jumps_per_session = 0.0;
        let start = 1_020 * NS_PER_MINUTE;
        let parents = walk(&config, 9, start, start + 300 * NS_PER_MINUTE);
        let kick = config.impact_permanent_ticks + config.impact_transient_ticks;
        let own = parents
            .iter()
            .map(|p| p.impact_ticks * sign_of(p.side))
            .sum::<f64>()
            / parents.len() as f64;
        assert!(
            (0.8 * kick..=kick).contains(&own),
            "own-direction move {own} against kick {kick}"
        );
        let net_sign: f64 = parents.iter().map(|p| sign_of(p.side)).sum();
        let total: f64 = parents.iter().map(|p| p.impact_ticks).sum();
        let permanent = config.impact_permanent_ticks * net_sign;
        // The transient register is bounded by 1 / (1 - decay), so the
        // total can differ from the permanent drift by at most that many
        // transient ticks.
        let bound = config.impact_transient_ticks / (1.0 - config.impact_transient_decay) + 1.0;
        assert!(
            (total - permanent).abs() <= bound,
            "total {total} permanent {permanent}"
        );
        assert!(net_sign.abs() > 100.0, "the sign memory leaves a net sign");
    }

    #[test]
    fn excitation_and_sign_configs_are_validated() {
        let mut bad = config();
        bad.excitation_ratio = vec![0.5, 0.5];
        bad.excitation_tau_s = vec![0.1, 1.0];
        assert!(bad.validate().is_err(), "ratios summing to one");
        let mut bad = config();
        bad.excitation_tau_s.pop();
        assert!(bad.validate().is_err(), "mismatched kernels");
        let mut bad = config();
        bad.sign_slots = 0;
        assert!(bad.validate().is_err(), "no slots");
        let mut bad = config();
        bad.sign_alpha = 1.0;
        assert!(bad.validate().is_err(), "infinite-mean metaorder");
        let mut bad = config();
        bad.sign_repeat = 1.0;
        assert!(bad.validate().is_err(), "repeat forever");
        let mut fine = config();
        fine.excitation_ratio.clear();
        fine.excitation_tau_s.clear();
        fine.fast_texture_log_sd = 0.0;
        assert!(
            fine.validate().is_ok(),
            "no excitation and no fast texture is a valid preset"
        );
    }

    #[test]
    fn parents_are_strictly_increasing_and_the_rate_is_the_configured_one() {
        let mut config = config();
        config.level_log_sd = 0.0;
        config.sigma_level_log_sd = 0.0;
        let calendar = calendar(vec![1.0; 1_380], vec![1.0; 1_380]);
        let tables = build_tables(&config, &calendar, 0.08).unwrap();
        let start = 1_020 * NS_PER_MINUTE;
        let end = start + 600 * NS_PER_MINUTE;
        let mut state = CascadeState::new(&tables, 3, start);
        let modifiers = SecondModifiers {
            rate_mult: 1.0,
            arrival_thin: 1.0,
            vol_mult: 1.0,
        };
        let mut clock = start;
        let mut count = 0_u64;
        loop {
            if !state.has_parent() {
                state.fill_next_bucket(&tables, clock, modifiers).unwrap();
            }
            let parent = state.draw_parent(&tables, clock);
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
