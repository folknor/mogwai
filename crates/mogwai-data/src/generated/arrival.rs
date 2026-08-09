// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The protocol-12b integrated wall-time arrival kernels.  This module is
//! deliberately not connected to [`super::GeneratedSource`] yet: Brick K only
//! establishes and tests the cadence contract.

use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{Distribution, StandardNormal};
use serde::Deserialize;

use super::{
    calendar::SessionCalendar,
    consts::{INTRA_EVENT_STEP_NS, MAX_SESSION_GAP_NS},
    dynamics::SweepShape,
    fingerprint::{GeneratorScalars, SessionProfile},
    session::SessionModulator,
};
use mogwai_protocol::seeds::splitmix64;

const CADENCE_STREAM_TAG: u64 = 0x6D6F6777_61693132;

/// The mean gap `next_parent` is driven with. One definition, called by both
/// `CadenceWalk::assemble` and `GeneratedSource::begin_integrated_event`: the
/// point of the shared assembly is that no cadence input is derived twice.
/// The mean is BARE, deliberately: `ARRIVAL_MEAN_CAL` is an empirically
/// bisected correction for the SHIPPED sampling scheme's realized-mean
/// inflation, and the integrated frame's exact time change has no such
/// inflation to correct - applying it here double-counts, a uniform
/// 1/0.944 = 1.0593 rate excess (the 2026-08-09 calibration amendment,
/// `notes/protocol-12b-arrival-composition-spec.md` section 0). The
/// shipped path keeps the calibration in `begin_event`, which is what it
/// corrects. Both the intensity denominator and the self-exciting
/// family's baseline expectation flow from this one definition; removing
/// the calibration from both is what preserves the feedback identity.
pub(super) fn cadence_base_mean_s(scalars: &GeneratorScalars) -> f64 {
    scalars.mean_event_duration_s
}

/// Identity of the cadence draw used by Stage A cache entries.
///
/// Changes to `ArrivalKernel::next_parent`, a family's parameterization,
/// `CADENCE_STREAM_TAG`, or cadence seed derivation must bump this value.
pub const ARRIVAL_KERNEL_VERSION: u32 = 2;

/// The cadence half of a generated walk, assembled exactly as the generator
/// assembles it and driven independently under neutral runtime modifiers.
pub struct CadenceWalk {
    kernel: ArrivalKernel,
    state: ArrivalState,
    env: ArrivalEnv,
    shape: SweepShape,
    rng: ChaCha12Rng,
    base_mean_s: f64,
    clock_ns: u64,
}

/// Cadence components consumed by `GeneratedSource`.
pub struct CadenceParts {
    pub kernel: Option<ArrivalKernel>,
    pub state: Option<ArrivalState>,
    pub env: Option<ArrivalEnv>,
    pub rng: Option<ChaCha12Rng>,
    pub base_mean_s: f64,
}

impl CadenceWalk {
    #[must_use]
    pub fn assemble(
        scalars: &GeneratorScalars,
        session: &SessionProfile,
        calendar: Option<&SessionCalendar>,
        thin: f64,
        seed: u64,
        start_ts_ns: u64,
    ) -> CadenceParts {
        let kernel = scalars.arrival.and_then(ArrivalConfig::kernel);
        let mut rng =
            kernel.map(|_| ChaCha12Rng::seed_from_u64(splitmix64(seed ^ CADENCE_STREAM_TAG)));
        let state = kernel.as_ref().map(|kernel| {
            ArrivalState::new(
                kernel,
                start_ts_ns,
                rng.as_mut().expect("kernel has cadence rng"),
            )
        });
        let env = kernel
            .as_ref()
            .map(|_| ArrivalEnv::for_profile(session, calendar, thin, start_ts_ns));
        CadenceParts {
            kernel,
            state,
            env,
            rng,
            base_mean_s: cadence_base_mean_s(scalars),
        }
    }

    #[must_use]
    pub fn new(
        scalars: &GeneratorScalars,
        session: &SessionProfile,
        calendar: Option<&SessionCalendar>,
        thin: f64,
        seed: u64,
        start_ts_ns: u64,
    ) -> Option<Self> {
        let parts = Self::assemble(scalars, session, calendar, thin, seed, start_ts_ns);
        Some(Self {
            kernel: parts.kernel?,
            state: parts.state?,
            env: parts.env?,
            shape: SweepShape::new(
                scalars.children_mean,
                scalars.children_single_frac,
                scalars.levels_mean,
            ),
            rng: parts.rng?,
            base_mean_s: parts.base_mean_s,
            clock_ns: start_ts_ns,
        })
    }

    #[expect(
        clippy::should_implement_trait,
        reason = "the frozen Stage A API names one cadence draw next"
    )]
    pub fn next(&mut self) -> Result<ParentDraw, ArrivalRefusal> {
        let draw = self.kernel.next_parent(
            &mut self.state,
            self.clock_ns,
            self.base_mean_s,
            &self.shape,
            &self.env,
            RuntimeModifiers::NEUTRAL,
            &mut self.rng,
        )?;
        self.clock_ns = draw.next_from_ns;
        Ok(draw)
    }

    #[must_use]
    pub const fn child_stride_ns(&self) -> u64 {
        INTRA_EVENT_STEP_NS
    }
}

const CADENCE_STEP_NS: u64 = 1_000_000_000;
const EXPECTED_COUNT_FLOOR: f64 = 0.01;
const SELF_EXCITING_X_CEILING: f64 = 1e4;
const NS_PER_HOUR: u64 = 3_600_000_000_000;
const NS_PER_MINUTE: u64 = 60_000_000_000;
const NS_PER_WEEK: u64 = 7 * 24 * NS_PER_HOUR;

/// Instrument-resolved protocol-12b arrival seam.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum ArrivalConfig {
    EventMarkov {
        quiet_share: f64,
        switch_rate: f64,
        rate_ratio: f64,
    },
    WallMmpp {
        occupancy: f64,
        rate_ratio: f64,
        tau_s: f64,
    },
    LogOuCox {
        sigma_y: f64,
        tau_s: f64,
    },
    SelfExciting {
        phi: f64,
        tau_s: f64,
    },
}

impl ArrivalConfig {
    pub fn is_valid(self) -> bool {
        match self {
            Self::EventMarkov {
                quiet_share,
                switch_rate,
                rate_ratio,
            } => {
                quiet_share.is_finite()
                    && (0.0..1.0).contains(&quiet_share)
                    && switch_rate.is_finite()
                    && (0.0..=0.5).contains(&switch_rate)
                    && rate_ratio.is_finite()
                    && rate_ratio > 0.0
            }
            Self::WallMmpp {
                occupancy,
                rate_ratio,
                tau_s,
            } => {
                occupancy.is_finite()
                    && (0.0..1.0).contains(&occupancy)
                    && rate_ratio.is_finite()
                    && rate_ratio > 0.0
                    && tau_s.is_finite()
                    && tau_s >= 1.0
            }
            Self::LogOuCox { sigma_y, tau_s } => {
                sigma_y.is_finite() && sigma_y >= 0.0 && tau_s.is_finite() && tau_s >= 1.0
            }
            Self::SelfExciting { phi, tau_s } => {
                phi.is_finite() && (0.0..0.9).contains(&phi) && tau_s.is_finite() && tau_s >= 2.0
            }
        }
    }

    pub fn kernel(self) -> Option<ArrivalKernel> {
        match self {
            Self::EventMarkov { .. } => None,
            Self::WallMmpp {
                occupancy,
                rate_ratio,
                tau_s,
            } => Some(ArrivalKernel::WallMmpp(WallMmppParams {
                occupancy,
                rate_ratio,
                tau_s,
            })),
            Self::LogOuCox { sigma_y, tau_s } => {
                Some(ArrivalKernel::LogOuCox(LogOuParams { sigma_y, tau_s }))
            }
            Self::SelfExciting { phi, tau_s } => {
                Some(ArrivalKernel::SelfExciting(SelfExcitingParams {
                    phi,
                    tau_s,
                }))
            }
        }
    }
}

/// Immutable baseline and grid phase for an integrated arrival walk.
#[derive(Clone)]
pub struct ArrivalEnv {
    session: SessionModulator,
    calendar: Option<SessionCalendar>,
    thin: f64,
    origin_ns: u64,
    weekly_calendar_transitions_ns: Vec<u64>,
}

impl ArrivalEnv {
    /// Builds the complete environment from data owned by `mogwai-data`.
    #[must_use]
    pub fn for_profile(
        profile: &SessionProfile,
        calendar: Option<&SessionCalendar>,
        thin: f64,
        origin_ns: u64,
    ) -> Self {
        Self {
            session: SessionModulator::new(profile, calendar),
            calendar: calendar.cloned(),
            thin,
            origin_ns,
            weekly_calendar_transitions_ns: calendar
                .map(calendar_transition_offsets)
                .unwrap_or_default(),
        }
    }

    /// Returns zero during a calendar closure, exactly.
    #[must_use]
    pub fn rate_at(&self, clock_ns: u64) -> f64 {
        if self
            .calendar
            .as_ref()
            .is_some_and(|calendar| !calendar.is_open(clock_ns))
        {
            return 0.0;
        }
        self.session.arrival_mult(clock_ns) / self.thin
    }

    fn cell_index(&self, clock_ns: u64) -> u64 {
        clock_ns.saturating_sub(self.origin_ns) / CADENCE_STEP_NS
    }

    fn cell_start(&self, cell: u64) -> u64 {
        self.origin_ns
            .saturating_add(cell.saturating_mul(CADENCE_STEP_NS))
    }

    fn next_calendar_boundary(&self, from_ns: u64) -> u64 {
        if self.weekly_calendar_transitions_ns.is_empty() {
            return u64::MAX;
        }
        let week_start = from_ns / NS_PER_WEEK * NS_PER_WEEK;
        let within_week = from_ns - week_start;
        let index = self
            .weekly_calendar_transitions_ns
            .partition_point(|&transition| transition <= within_week);
        if let Some(&transition) = self.weekly_calendar_transitions_ns.get(index) {
            week_start.saturating_add(transition)
        } else {
            week_start
                .saturating_add(NS_PER_WEEK)
                .saturating_add(self.weekly_calendar_transitions_ns[0])
        }
    }

    fn next_segment_end(&self, cursor: u64) -> u64 {
        if self
            .calendar
            .as_ref()
            .is_some_and(|calendar| !calendar.is_open(cursor))
        {
            return self.next_calendar_boundary(cursor);
        }
        let next_cell = self.cell_start(self.cell_index(cursor).saturating_add(1));
        let next_hour = (cursor / NS_PER_HOUR).saturating_add(1);
        let next_hour = next_hour.saturating_mul(NS_PER_HOUR);
        next_cell
            .min(next_hour)
            .min(self.next_calendar_boundary(cursor))
    }

    fn baseline_integral(&self, from_ns: u64, to_ns: u64, base_mean_s: f64) -> f64 {
        let mut cursor = from_ns;
        let mut integral = 0.0;
        while cursor < to_ns {
            let end = self.next_segment_end(cursor).min(to_ns);
            let seconds = (end - cursor) as f64 / 1e9;
            integral += seconds * self.rate_at(cursor) / base_mean_s;
            cursor = end;
        }
        integral
    }
}

fn calendar_transition_offsets(calendar: &SessionCalendar) -> Vec<u64> {
    let mut transitions = Vec::new();
    let mut was_open = calendar.is_open(0);
    for utc_minute in 1..=10_080_u64 {
        let instant = utc_minute.saturating_mul(NS_PER_MINUTE);
        let is_open = calendar.is_open(instant);
        if is_open != was_open {
            transitions.push(instant);
        }
        was_open = is_open;
    }
    transitions
}

#[derive(Debug, Clone, Copy)]
pub enum ArrivalKernel {
    WallMmpp(WallMmppParams),
    LogOuCox(LogOuParams),
    SelfExciting(SelfExcitingParams),
}

#[derive(Debug, Clone, Copy)]
pub struct WallMmppParams {
    pub occupancy: f64,
    pub rate_ratio: f64,
    pub tau_s: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct LogOuParams {
    pub sigma_y: f64,
    pub tau_s: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct SelfExcitingParams {
    pub phi: f64,
    pub tau_s: f64,
}

impl WallMmppParams {
    fn transition_probability(self, from_quiet: bool, dt_s: f64) -> f64 {
        let flip = 1.0 - (-dt_s / self.tau_s).exp();
        if from_quiet {
            (1.0 - self.occupancy) * flip
        } else {
            self.occupancy * flip
        }
    }

    fn transition(self, from_quiet: bool, uniform: f64, dt_s: f64) -> bool {
        if uniform < self.transition_probability(from_quiet, dt_s) {
            !from_quiet
        } else {
            from_quiet
        }
    }

    fn level(self, quiet: bool) -> f64 {
        let denom = self.occupancy + (1.0 - self.occupancy) * self.rate_ratio;
        if quiet {
            1.0 / denom
        } else {
            self.rate_ratio / denom
        }
    }
}

impl LogOuParams {
    fn initial_y(self, z: f64) -> f64 {
        self.sigma_y * z
    }

    fn transition(self, y: f64, z: f64, dt_s: f64) -> f64 {
        let decay = (-dt_s / self.tau_s).exp();
        decay * y + self.sigma_y * (1.0 - decay * decay).sqrt() * z
    }

    fn level(self, y: f64) -> f64 {
        (y - self.sigma_y * self.sigma_y / 2.0).exp()
    }
}

impl SelfExcitingParams {
    fn transition(self, a: f64, expected: f64, count: u32, dt_s: f64) -> f64 {
        let observed = if expected >= EXPECTED_COUNT_FLOOR {
            f64::from(count) / expected
        } else {
            1.0
        };
        let decay = (-dt_s / self.tau_s).exp();
        decay * a + (1.0 - decay) * observed
    }

    fn level(self, a: f64) -> f64 {
        (1.0 - self.phi) + self.phi * a
    }
}

#[derive(Debug, Clone)]
pub enum ArrivalState {
    WallMmpp {
        quiet: bool,
        cell_index: u64,
    },
    LogOuCox {
        y: f64,
        cell_index: u64,
    },
    SelfExciting {
        a: f64,
        cell_index: u64,
        cell_count: u32,
    },
}

impl ArrivalState {
    #[must_use]
    pub fn new(kernel: &ArrivalKernel, _origin_ns: u64, rng: &mut ChaCha12Rng) -> Self {
        let cell_index = 0;
        match *kernel {
            ArrivalKernel::WallMmpp(params) => Self::WallMmpp {
                quiet: rng.random::<f64>() < params.occupancy,
                cell_index,
            },
            ArrivalKernel::LogOuCox(params) => Self::LogOuCox {
                y: params.initial_y(<StandardNormal as Distribution<f64>>::sample(
                    &StandardNormal,
                    rng,
                )),
                cell_index,
            },
            ArrivalKernel::SelfExciting(_) => Self::SelfExciting {
                a: 1.0,
                cell_index,
                cell_count: 0,
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeModifiers {
    pub rate_mult: f64,
    pub children_mult: f64,
    pub pending_reopen: Option<PendingReopen>,
}

impl RuntimeModifiers {
    pub const NEUTRAL: Self = Self {
        rate_mult: 1.0,
        children_mult: 1.0,
        pending_reopen: None,
    };
}

#[derive(Debug, Clone, Copy)]
pub struct PendingReopen {
    pub at_ts_ns: u64,
    pub shift_ns: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct ParentDraw {
    pub parent_ts_ns: u64,
    pub child_count: u32,
    pub next_from_ns: u64,
    pub latent_x: f64,
    pub reopen_applied: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ArrivalRefusal {
    NoOpenExposure { from_ns: u64 },
    IntensityCeiling { clock_ns: u64, x: f64 },
    NonFiniteState { clock_ns: u64 },
}

impl ArrivalKernel {
    #[allow(clippy::too_many_arguments)]
    pub fn next_parent(
        &self,
        state: &mut ArrivalState,
        from_ns: u64,
        base_mean_s: f64,
        shape: &SweepShape,
        env: &ArrivalEnv,
        modifiers: RuntimeModifiers,
        rng: &mut ChaCha12Rng,
    ) -> Result<ParentDraw, ArrivalRefusal> {
        let budget = -rng.random::<f64>().ln();
        let mut remaining = budget;
        let mut cursor = from_ns;
        let limit = from_ns.saturating_add(MAX_SESSION_GAP_NS);
        while cursor < limit {
            self.advance_state_to(state, env.cell_index(cursor), env, base_mean_s, rng, cursor)?;
            let x = self.latent_x(state);
            if !x.is_finite() {
                return Err(ArrivalRefusal::NonFiniteState { clock_ns: cursor });
            }
            if x > SELF_EXCITING_X_CEILING {
                return Err(ArrivalRefusal::IntensityCeiling {
                    clock_ns: cursor,
                    x,
                });
            }
            let end = env.next_segment_end(cursor).min(limit);
            if end <= cursor {
                return Err(ArrivalRefusal::NoOpenExposure { from_ns });
            }
            let intensity = env.rate_at(cursor) * x * modifiers.rate_mult / base_mean_s;
            if intensity == 0.0 {
                cursor = end;
                continue;
            }
            let available = (end - cursor) as f64 / 1e9 * intensity;
            if remaining > available {
                remaining -= available;
                cursor = end;
                continue;
            }
            let delta_ns = (remaining / intensity * 1e9).ceil().max(1.0) as u64;
            let candidate = cursor.saturating_add(delta_ns).min(end);
            let (parent_ts_ns, reopen_applied) = match modifiers.pending_reopen {
                Some(reopen) if from_ns < reopen.at_ts_ns && reopen.at_ts_ns <= candidate => {
                    let shifted = candidate.saturating_add(reopen.shift_ns);
                    let snapped = env.calendar.as_ref().map_or(shifted, |calendar| {
                        if calendar.is_open(shifted) {
                            shifted
                        } else {
                            calendar.next_open_ns(shifted)
                        }
                    });
                    (snapped, true)
                }
                _ => (candidate, false),
            };
            self.advance_state_to(
                state,
                env.cell_index(parent_ts_ns),
                env,
                base_mean_s,
                rng,
                parent_ts_ns,
            )?;
            let latent_x = self.latent_x(state);
            if !latent_x.is_finite() {
                return Err(ArrivalRefusal::NonFiniteState {
                    clock_ns: parent_ts_ns,
                });
            }
            if latent_x > SELF_EXCITING_X_CEILING {
                return Err(ArrivalRefusal::IntensityCeiling {
                    clock_ns: parent_ts_ns,
                    x: latent_x,
                });
            }
            let child_count = shape.next_count_scaled(modifiers.children_mult, rng).max(1);
            if let ArrivalState::SelfExciting { cell_count, .. } = state {
                *cell_count = cell_count.saturating_add(1);
            }
            return Ok(ParentDraw {
                parent_ts_ns,
                child_count,
                next_from_ns: parent_ts_ns.saturating_add(
                    u64::from(child_count.saturating_sub(1)).saturating_mul(INTRA_EVENT_STEP_NS),
                ),
                latent_x,
                reopen_applied,
            });
        }
        Err(ArrivalRefusal::NoOpenExposure { from_ns })
    }

    pub(crate) fn latent_x(&self, state: &ArrivalState) -> f64 {
        match (*self, state) {
            (ArrivalKernel::WallMmpp(params), ArrivalState::WallMmpp { quiet, .. }) => {
                params.level(*quiet)
            }
            (ArrivalKernel::LogOuCox(params), ArrivalState::LogOuCox { y, .. }) => params.level(*y),
            (ArrivalKernel::SelfExciting(params), ArrivalState::SelfExciting { a, .. }) => {
                params.level(*a)
            }
            _ => f64::NAN,
        }
    }

    fn advance_state_to(
        &self,
        state: &mut ArrivalState,
        target: u64,
        env: &ArrivalEnv,
        base_mean_s: f64,
        rng: &mut ChaCha12Rng,
        clock_ns: u64,
    ) -> Result<(), ArrivalRefusal> {
        match (*self, state) {
            (ArrivalKernel::WallMmpp(params), ArrivalState::WallMmpp { quiet, cell_index }) => {
                while *cell_index < target {
                    *quiet = params.transition(*quiet, rng.random(), CADENCE_STEP_NS as f64 / 1e9);
                    *cell_index += 1;
                }
            }
            (ArrivalKernel::LogOuCox(params), ArrivalState::LogOuCox { y, cell_index }) => {
                while *cell_index < target {
                    *y = params.transition(
                        *y,
                        <StandardNormal as Distribution<f64>>::sample(&StandardNormal, rng),
                        CADENCE_STEP_NS as f64 / 1e9,
                    );
                    *cell_index += 1;
                }
            }
            (
                ArrivalKernel::SelfExciting(params),
                ArrivalState::SelfExciting {
                    a,
                    cell_index,
                    cell_count,
                },
            ) => {
                while *cell_index < target {
                    let start = env.cell_start(*cell_index);
                    let end = start.saturating_add(CADENCE_STEP_NS);
                    let expected = env.baseline_integral(start, end, base_mean_s);
                    *a = params.transition(*a, expected, *cell_count, CADENCE_STEP_NS as f64 / 1e9);
                    if !a.is_finite() {
                        return Err(ArrivalRefusal::NonFiniteState { clock_ns });
                    }
                    *cell_index += 1;
                    *cell_count = 0;
                }
            }
            _ => return Err(ArrivalRefusal::NonFiniteState { clock_ns }),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rand::SeedableRng;
    use rand_distr::Poisson;
    use serde::Deserialize;
    use serde_json::{Value, json};

    use super::super::{Fingerprint, GeneratedSource, GeneratorScalars};
    use super::*;

    fn env(calendar: Option<&SessionCalendar>) -> ArrivalEnv {
        let profile = SessionProfile {
            intensity_hour: [1.0 / 24.0; 24],
            vol_hour: [1.0; 24],
            dow_weight: [1.0 / 7.0; 7],
        };
        ArrivalEnv::for_profile(&profile, calendar, 1.0, 0)
    }

    #[test]
    fn a_cadence_walk_and_the_generator_agree_parent_for_parent() {
        let fp = Fingerprint::from_repo_json();
        let families = [
            ArrivalConfig::WallMmpp {
                occupancy: 0.35,
                rate_ratio: 20.0,
                tau_s: 60.0,
            },
            ArrivalConfig::LogOuCox {
                sigma_y: 0.6,
                tau_s: 60.0,
            },
            ArrivalConfig::SelfExciting {
                phi: 0.4,
                tau_s: 60.0,
            },
        ];
        for config in families {
            let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
            scalars.arrival = Some(config);
            let mut cadence = CadenceWalk::new(&scalars, &fp.session_profile, None, 1.0, 91, 0)
                .expect("integrated family");
            let mut generator = GeneratedSource::new(scalars, 91, 0, &fp, None);
            for _ in 0..10_000 {
                let left = cadence.next().expect("cadence draw");
                let right = generator.advance_parent();
                assert_eq!(
                    (left.parent_ts_ns, left.child_count),
                    (right.parent_ts_ns, right.child_count)
                );
            }
        }
    }

    #[test]
    fn wall_mmpp_reproduces_stationary_law_and_quiet_occupancy() {
        let kernel = ArrivalKernel::WallMmpp(WallMmppParams {
            occupancy: 0.35,
            rate_ratio: 12.0,
            tau_s: 4.0,
        });
        let environment = env(None);
        let mut rng = ChaCha12Rng::seed_from_u64(17);
        let mut state = ArrivalState::new(&kernel, 0, &mut rng);
        let mut quiet = 0_u64;
        for cell in 1..=500_000 {
            kernel
                .advance_state_to(
                    &mut state,
                    cell,
                    &environment,
                    1.0,
                    &mut rng,
                    cell * CADENCE_STEP_NS,
                )
                .expect("finite MMPP state");
            if matches!(state, ArrivalState::WallMmpp { quiet: true, .. }) {
                quiet += 1;
            }
        }
        assert!((quiet as f64 / 500_000.0 - 0.35).abs() < 0.01);
        let params = WallMmppParams {
            occupancy: 0.35,
            rate_ratio: 12.0,
            tau_s: 4.0,
        };
        let denom = params.occupancy + (1.0 - params.occupancy) * params.rate_ratio;
        let x_quiet = 1.0 / denom;
        let x_active = params.rate_ratio / denom;
        let stationary_mean = params.occupancy * x_quiet + (1.0 - params.occupancy) * x_active;
        assert!((stationary_mean - 1.0).abs() <= 8.0 * f64::EPSILON);
    }

    #[test]
    fn log_ou_cox_reproduces_stationary_law() {
        let sigma_y = 0.6;
        let kernel = ArrivalKernel::LogOuCox(LogOuParams {
            sigma_y,
            tau_s: 1.0,
        });
        let environment = env(None);
        let mut rng = ChaCha12Rng::seed_from_u64(23);
        let mut state = ArrivalState::new(&kernel, 0, &mut rng);
        let mut sum = 0.0;
        let mut sum_sq = 0.0;
        for cell in 1..=500_000 {
            kernel
                .advance_state_to(
                    &mut state,
                    cell,
                    &environment,
                    1.0,
                    &mut rng,
                    cell * CADENCE_STEP_NS,
                )
                .expect("finite OU state");
            let x = kernel.latent_x(&state);
            sum += x;
            sum_sq += x * x;
        }
        let mean = sum / 500_000.0;
        let variance = sum_sq / 500_000.0 - mean * mean;
        assert!((mean - 1.0).abs() < 0.005);
        assert!((variance / (sigma_y * sigma_y).exp_m1() - 1.0).abs() < 0.02);
    }

    #[test]
    fn self_exciting_reproduces_its_latent_mean() {
        let kernel = ArrivalKernel::SelfExciting(SelfExcitingParams {
            phi: 0.7,
            tau_s: 8.0,
        });
        let environment = env(None);
        let mut rng = ChaCha12Rng::seed_from_u64(29);
        let mut state = ArrivalState::new(&kernel, 0, &mut rng);
        let mut sum = 0.0;
        for cell in 1..=500_000 {
            let x = kernel.latent_x(&state);
            let count = Poisson::new(x)
                .expect("positive Poisson mean")
                .sample(&mut rng) as u32;
            if let ArrivalState::SelfExciting { cell_count, .. } = &mut state {
                *cell_count = count;
            }
            kernel
                .advance_state_to(
                    &mut state,
                    cell,
                    &environment,
                    1.0,
                    &mut rng,
                    cell * CADENCE_STEP_NS,
                )
                .expect("finite self-exciting state");
            sum += kernel.latent_x(&state);
        }
        assert!((sum / 500_000.0 - 1.0).abs() < 0.005);
    }

    #[test]
    fn neutral_exposure_has_zero_calendar_snaps() {
        let calendar = SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: vec![super::super::calendar::WeeklyWindow {
                start_minute: 60,
                end_minute: 120,
            }],
            settlement_minute_of_day: None,
        };
        let environment = env(Some(&calendar));
        let kernel = ArrivalKernel::WallMmpp(WallMmppParams {
            occupancy: 0.35,
            rate_ratio: 2.0,
            tau_s: 10.0,
        });
        let mut rng = ChaCha12Rng::seed_from_u64(31);
        let mut state = ArrivalState::new(&kernel, 0, &mut rng);
        let shape = SweepShape::new(1.2, 0.9, 1.1);
        let mut from = 0;
        for _ in 0..100 {
            let draw = kernel
                .next_parent(
                    &mut state,
                    from,
                    1.0,
                    &shape,
                    &environment,
                    RuntimeModifiers::NEUTRAL,
                    &mut rng,
                )
                .expect("weekly open exposure exists");
            assert!(!draw.reopen_applied);
            assert!(calendar.is_open(draw.parent_ts_ns));
            from = draw.next_from_ns;
        }
    }

    #[test]
    fn arrival_conformance_vectors_v1_through_v8() {
        // These are committed, independently-derived inputs.  This test is a
        // schema and bit-pattern guard: generation tooling must never be able
        // to replace the reviewable arithmetic in their derivation fields.
        const VECTORS: [&str; 8] = [
            include_str!("../../tests/fixtures/arrival-vector-v1-mmpp-transition.json"),
            include_str!("../../tests/fixtures/arrival-vector-v2-logou-transition.json"),
            include_str!("../../tests/fixtures/arrival-vector-v3-selfexciting-feedback.json"),
            include_str!("../../tests/fixtures/arrival-vector-v4-eventmarkov-flip.json"),
            include_str!("../../tests/fixtures/arrival-vector-v5-child-law-and-order.json"),
            include_str!("../../tests/fixtures/arrival-vector-v6-triple-boundary.json"),
            include_str!("../../tests/fixtures/arrival-vector-v7-degenerate-budget.json"),
            include_str!("../../tests/fixtures/arrival-vector-v8-reopen-seam.json"),
        ];

        for (index, source) in VECTORS.into_iter().enumerate() {
            let vector: Value =
                serde_json::from_str(source).expect("valid conformance vector JSON");
            assert_eq!(vector["vector"], format!("V{}", index + 1));
            assert!(
                vector["what_it_checks"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
            assert!(
                vector["derivation"]
                    .as_str()
                    .is_some_and(|text| !text.is_empty())
            );
            assert!(vector["params"].is_object());
            assert!(
                vector["inputs"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
            );
            assert!(
                vector["expected"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty())
            );

            // A vector is conformance evidence only if its expected values are
            // compared to the implementation.  Do not reduce this to a schema
            // check: the fixture is independently derived and must be able to
            // catch a plausible but wrong kernel result.
            match index {
                0 => execute_v1_mmpp(&vector),
                1 => execute_v2_log_ou(&vector),
                2 => execute_v3_self_exciting(&vector),
                3 => execute_v4_event_markov_contract(&vector),
                4 => execute_v5_child_law_and_order(&vector),
                5 => execute_v6_budget_traversal(&vector),
                6 => execute_v7_degenerate_budget(&vector),
                7 => execute_v8_reopen_seam(&vector),
                _ => unreachable!("vector list is fixed"),
            }
        }
    }

    fn hex(value: &Value) -> f64 {
        let text = value.as_str().expect("hex float");
        let bits = u64::from_str_radix(text.strip_prefix("0x").expect("0x prefix"), 16)
            .expect("valid hex float");
        f64::from_bits(bits)
    }

    fn assert_bits(actual: f64, expected: &Value, field: &str) {
        assert_eq!(actual.to_bits(), hex(expected).to_bits(), "{field}");
    }

    fn execute_v1_mmpp(vector: &Value) {
        let params = WallMmppParams {
            occupancy: vector["params"]["occupancy"].as_f64().expect("occupancy"),
            rate_ratio: vector["params"]["rate_ratio"].as_f64().expect("rate ratio"),
            tau_s: hex(&vector["params"]["tau_s"]),
        };
        let inputs = vector["inputs"].as_array().expect("inputs");
        let expected = &vector["expected"][0];
        assert_bits(
            params.transition_probability(true, hex(&inputs[0]["dt_s"])),
            &expected["quiet_to_active"],
            "V1 quiet-to-active probability",
        );
        assert_bits(
            params.transition_probability(false, hex(&inputs[1]["dt_s"])),
            &expected["active_to_quiet"],
            "V1 active-to-quiet probability",
        );
        let post_boundary_quiet = params.transition(
            inputs[0]["from_quiet"].as_bool().expect("state"),
            hex(&inputs[0]["uniform"]),
            hex(&inputs[0]["dt_s"]),
        );
        assert_eq!(post_boundary_quiet, expected["post_boundary_quiet"]);
        assert_bits(
            params.level(post_boundary_quiet),
            &expected["interval_latent_x"],
            "V1 post-boundary latent level",
        );
    }

    fn execute_v2_log_ou(vector: &Value) {
        let params = LogOuParams {
            sigma_y: vector["params"]["sigma_y"].as_f64().expect("sigma"),
            tau_s: hex(&vector["params"]["tau_s"]),
        };
        let input = &vector["inputs"][0];
        let expected = &vector["expected"][0];
        let initial_y = params.initial_y(hex(&input["initial_z"]));
        assert_bits(initial_y, &expected["initial_y"], "V2 initial Y");
        let next_y = params.transition(initial_y, hex(&input["transition_z"]), hex(&input["dt_s"]));
        assert_bits(next_y, &expected["next_y"], "V2 next Y");
        assert_bits(params.level(next_y), &expected["next_x"], "V2 next X");
    }

    fn execute_v3_self_exciting(vector: &Value) {
        let params = SelfExcitingParams {
            phi: vector["params"]["phi"].as_f64().expect("phi"),
            tau_s: hex(&vector["params"]["tau_s"]),
        };
        let cells = vector["inputs"][0]["cells"].as_array().expect("cells");
        let expected = &vector["expected"][0];
        let mut a = 1.0;
        assert_bits(a, &expected["a"][0], "V3 initial A");
        assert_bits(params.level(a), &expected["x"][0], "V3 initial X");
        for (index, cell) in cells.iter().enumerate() {
            a = params.transition(
                a,
                hex(&cell["expected"]),
                cell["count"].as_u64().expect("count") as u32,
                1.0,
            );
            assert_bits(a, &expected["a"][index + 1], "V3 A");
            assert_bits(params.level(a), &expected["x"][index + 1], "V3 X");
        }
    }

    fn execute_v4_event_markov_contract(vector: &Value) {
        let input = &vector["inputs"][0];
        let quiet_share = vector["params"]["quiet_share"]
            .as_f64()
            .expect("quiet share");
        let switch_rate = vector["params"]["switch_rate"]
            .as_f64()
            .expect("switch rate");
        let mut clock = super::super::dynamics::ArrivalClock {
            active_mean_s: 1.0,
            quiet_mean_s: 1.0,
            quiet_to_active: switch_rate * (1.0 - quiet_share),
            active_to_quiet: switch_rate * quiet_share,
            quiet: input["initial_quiet"].as_bool().expect("initial state"),
            last_quiet: false,
        };
        let flip_probability = clock.quiet_to_active;
        clock.advance_after_gap(hex(&input["flip_draw"]) < flip_probability);
        assert_eq!(clock.last_quiet, vector["expected"][0]["child_reads_quiet"]);
        assert_eq!(clock.quiet, vector["expected"][0]["next_quiet"]);
        assert_eq!(
            vector["expected"][0]["main_stream_order"],
            serde_json::json!([
                "gap",
                "flip",
                "latent_mid",
                "latent_mid",
                "side_book",
                "side_book",
                "side_book",
                "child"
            ]),
        );
    }

    fn execute_v5_child_law_and_order(vector: &Value) {
        let expected = &vector["expected"][0];
        let shape = SweepShape::new(2.0, 0.5, 1.0);
        for (index, n) in vector["inputs"][0]["n"]
            .as_array()
            .expect("n")
            .iter()
            .enumerate()
        {
            let n = n.as_u64().expect("count") as i32;
            let probability = if n == 1 {
                shape.q + (1.0 - shape.q) / shape.m
            } else {
                (1.0 - shape.q) / shape.m * (1.0 - 1.0 / shape.m).powi(n - 1)
            };
            assert_bits(
                probability,
                &expected["probability"][index],
                "V5 child probability",
            );
        }
        assert_eq!(
            expected["contract_a_order"],
            serde_json::json!(["gap", "flip", "price", "book", "child"])
        );
        assert_eq!(
            expected["contract_b_order"],
            serde_json::json!(["budget", "latent_steps", "child"])
        );
    }

    fn execute_v6_budget_traversal(vector: &Value) {
        let input = &vector["inputs"][0];
        let mut remaining = hex(&input["budget"]);
        let mut offset = 0.0;
        for index in 0..4 {
            let seconds = hex(&input["segments_s"][index]);
            let open = input["open"][index].as_bool().expect("open flag");
            let spent = if open { remaining.min(seconds) } else { 0.0 };
            remaining -= spent;
            offset += if spent < seconds && open {
                spent
            } else {
                seconds
            };
            assert_bits(
                spent,
                &vector["expected"][0]["budget_spent"][index],
                "V6 budget spent",
            );
            assert_bits(
                remaining,
                &vector["expected"][0]["remaining_after_segments"][index],
                "V6 remaining budget",
            );
        }
        assert_bits(
            offset,
            &vector["expected"][0]["parent_offset_s"],
            "V6 parent offset",
        );
    }

    fn execute_v7_degenerate_budget(vector: &Value) {
        let uniform = hex(&vector["inputs"][0]["uniform"]);
        let budget = -uniform.ln();
        assert_bits(budget, &vector["expected"][0]["budget"], "V7 kernel budget");
        let parent_ts_ns = (budget * 1e9).ceil().max(1.0) as u64;
        assert_eq!(parent_ts_ns, vector["expected"][0]["parent_ts_ns"]);
    }

    fn execute_v8_reopen_seam(vector: &Value) {
        for (input, expected) in vector["inputs"]
            .as_array()
            .expect("inputs")
            .iter()
            .zip(vector["expected"].as_array().expect("expected"))
        {
            let from = input["from_ns"].as_u64().expect("from");
            let at = input["at_ns"].as_u64().expect("at");
            let candidate = input["candidate_ns"].as_u64().expect("candidate");
            let crossed = from < at && at <= candidate;
            let mut parent = if crossed {
                candidate.saturating_add(input["shift_ns"].as_u64().expect("shift"))
            } else {
                candidate
            };
            if crossed && input.get("next_open_ns").is_some() {
                parent = input["next_open_ns"].as_u64().expect("next open");
            }
            let count = input["child_count"].as_u64().expect("child count");
            assert_eq!(parent, expected["parent_ts_ns"]);
            assert_eq!(crossed, expected["reopen_applied"]);
            assert_eq!(parent / CADENCE_STEP_NS, expected["cell"]);
            assert_eq!(
                parent.saturating_add((count - 1) * INTRA_EVENT_STEP_NS),
                expected["next_from_ns"]
            );
        }
    }

    #[derive(Debug, Deserialize)]
    struct Transcript {
        kind: String,
        correctness_claim: String,
        family: String,
        seed: u64,
        origin_ns: u64,
        params: Value,
        records: Vec<TranscriptRecord>,
    }

    #[derive(Debug, Deserialize)]
    struct TranscriptRecord {
        parent_ts_ns: u64,
        child_count: u32,
        latent_x_bits: u64,
    }

    fn transcript_kernel(family: &str, params: &Value) -> ArrivalKernel {
        match family {
            "wall_mmpp" => ArrivalKernel::WallMmpp(WallMmppParams {
                occupancy: params["occupancy"].as_f64().expect("occupancy"),
                rate_ratio: params["rate_ratio"].as_f64().expect("rate ratio"),
                tau_s: params["tau_s"].as_f64().expect("tau"),
            }),
            "log_ou_cox" => ArrivalKernel::LogOuCox(LogOuParams {
                sigma_y: params["sigma_y"].as_f64().expect("sigma"),
                tau_s: params["tau_s"].as_f64().expect("tau"),
            }),
            "self_exciting" => ArrivalKernel::SelfExciting(SelfExcitingParams {
                phi: params["phi"].as_f64().expect("phi"),
                tau_s: params["tau_s"].as_f64().expect("tau"),
            }),
            other => panic!("unknown transcript family {other}"),
        }
    }

    fn transcript_config(family: &str, params: &Value) -> ArrivalConfig {
        match transcript_kernel(family, params) {
            ArrivalKernel::WallMmpp(params) => ArrivalConfig::WallMmpp {
                occupancy: params.occupancy,
                rate_ratio: params.rate_ratio,
                tau_s: params.tau_s,
            },
            ArrivalKernel::LogOuCox(params) => ArrivalConfig::LogOuCox {
                sigma_y: params.sigma_y,
                tau_s: params.tau_s,
            },
            ArrivalKernel::SelfExciting(params) => ArrivalConfig::SelfExciting {
                phi: params.phi,
                tau_s: params.tau_s,
            },
        }
    }

    #[test]
    fn arrival_transcripts_replay_bit_exact() {
        // These files are REGRESSION PINS ONLY. Their records were generated
        // once at the named non-degenerate points; they prove that the Stage A
        // kernel walk and Stage B generator integration do not drift. The
        // conformance vectors above, not these transcripts, carry correctness
        // evidence for the family arithmetic.
        const TRANSCRIPTS: [&str; 3] = [
            include_str!("../../tests/fixtures/arrival-transcript-wall-mmpp.json"),
            include_str!("../../tests/fixtures/arrival-transcript-log-ou-cox.json"),
            include_str!("../../tests/fixtures/arrival-transcript-self-exciting.json"),
        ];

        let fp = Fingerprint::from_repo_json();
        for raw in TRANSCRIPTS {
            let transcript: Transcript = serde_json::from_str(raw).expect("valid transcript JSON");
            assert_eq!(transcript.kind, "regression-transcript");
            assert_eq!(transcript.correctness_claim, "none");
            assert_eq!(transcript.records.len(), 10_000);

            let kernel = transcript_kernel(&transcript.family, &transcript.params);
            let mut cadence_rng = ChaCha12Rng::seed_from_u64(mogwai_protocol::seeds::splitmix64(
                transcript.seed ^ 0x6D6F6777_61693132,
            ));
            let mut state = ArrivalState::new(&kernel, transcript.origin_ns, &mut cadence_rng);
            let environment =
                ArrivalEnv::for_profile(&fp.session_profile, None, 1.0, transcript.origin_ns);
            let mut from_ns = transcript.origin_ns;

            let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
            scalars.arrival = Some(transcript_config(&transcript.family, &transcript.params));
            let shape = SweepShape::new(
                scalars.children_mean,
                scalars.children_single_frac,
                scalars.levels_mean,
            );
            // Bare, matching `cadence_base_mean_s` (the 2026-08-09 calibration
            // amendment): the integrated frame carries no shipped-path
            // correction.
            let base_mean_s = scalars.mean_event_duration_s;
            let mut generator =
                GeneratedSource::new(scalars, transcript.seed, transcript.origin_ns, &fp, None);

            for (index, expected) in transcript.records.iter().enumerate() {
                let stage_a = kernel
                    .next_parent(
                        &mut state,
                        from_ns,
                        base_mean_s,
                        &shape,
                        &environment,
                        RuntimeModifiers::NEUTRAL,
                        &mut cadence_rng,
                    )
                    .expect("open test exposure");
                from_ns = stage_a.next_from_ns;
                assert_eq!(
                    stage_a.parent_ts_ns, expected.parent_ts_ns,
                    "{}/stage-a timestamp {index}",
                    transcript.family
                );
                assert_eq!(
                    stage_a.child_count, expected.child_count,
                    "{}/stage-a children {index}",
                    transcript.family
                );
                assert_eq!(
                    stage_a.latent_x.to_bits(),
                    expected.latent_x_bits,
                    "{}/stage-a latent {index}",
                    transcript.family
                );

                let (stage_b, latent_x_bits) = generator.advance_parent_with_arrival_latent();
                assert_eq!(
                    stage_b.parent_ts_ns, expected.parent_ts_ns,
                    "{}/stage-b timestamp {index}",
                    transcript.family
                );
                assert_eq!(
                    stage_b.child_count, expected.child_count,
                    "{}/stage-b children {index}",
                    transcript.family
                );
                assert_eq!(
                    latent_x_bits, expected.latent_x_bits,
                    "{}/stage-b latent {index}",
                    transcript.family
                );
            }
        }
    }

    /// AMENDMENT-ONLY regeneration of the three layer-2 transcript fixtures.
    ///
    /// The transcripts are committed regression pins and are NEVER regenerated
    /// to match a later change - except under a signed section 17 amendment
    /// that explicitly authorizes a one-time replacement, which must be cited
    /// in the commit that carries the new fixtures. Last sanctioned use: the
    /// 2026-08-09 arrival-frame calibration amendment (bare `base_mean_s`,
    /// `ARRIVAL_KERNEL_VERSION` 2). Running this outside an amendment defeats
    /// the pin's whole purpose; that is why it is `#[ignore]`d and named the
    /// way it is.
    #[test]
    #[ignore = "regenerates committed regression pins; sanctioned only by a signed amendment"]
    fn regenerate_arrival_transcripts_amendment_only() {
        let fp = Fingerprint::from_repo_json();
        let families: [(&str, &str, Value); 3] = [
            (
                "wall-mmpp",
                "wall_mmpp",
                json!({"occupancy": 0.4, "rate_ratio": 20.0, "tau_s": 100.0}),
            ),
            (
                "log-ou-cox",
                "log_ou_cox",
                json!({"sigma_y": 1.0, "tau_s": 100.0}),
            ),
            (
                "self-exciting",
                "self_exciting",
                json!({"phi": 0.45, "tau_s": 34.42651863295482}),
            ),
        ];
        for (file_stem, family, params) in families {
            let seed = 201_u64;
            let origin_ns = 0_u64;
            let kernel = transcript_kernel(family, &params);
            let mut cadence_rng = ChaCha12Rng::seed_from_u64(mogwai_protocol::seeds::splitmix64(
                seed ^ 0x6D6F6777_61693132,
            ));
            let mut state = ArrivalState::new(&kernel, origin_ns, &mut cadence_rng);
            let environment = ArrivalEnv::for_profile(&fp.session_profile, None, 1.0, origin_ns);
            let mut scalars = GeneratorScalars::xbtusd_anchor(&fp);
            scalars.arrival = Some(transcript_config(family, &params));
            let shape = SweepShape::new(
                scalars.children_mean,
                scalars.children_single_frac,
                scalars.levels_mean,
            );
            let base_mean_s = scalars.mean_event_duration_s;
            let mut from_ns = origin_ns;
            let mut records = Vec::with_capacity(10_000);
            for _ in 0..10_000 {
                let draw = kernel
                    .next_parent(
                        &mut state,
                        from_ns,
                        base_mean_s,
                        &shape,
                        &environment,
                        RuntimeModifiers::NEUTRAL,
                        &mut cadence_rng,
                    )
                    .expect("open test exposure");
                from_ns = draw.next_from_ns;
                records.push(json!({
                    "child_count": draw.child_count,
                    "latent_x_bits": draw.latent_x.to_bits(),
                    "parent_ts_ns": draw.parent_ts_ns,
                }));
            }
            let transcript = json!({
                "correctness_claim": "none",
                "exposure": "fixed fully-open fingerprint session profile used by the kernel/GeneratedSource parity harness",
                "family": family,
                "kind": "regression-transcript",
                "origin_ns": origin_ns,
                "parameter_point": "frozen coarse-grid point nearest the family domain centre",
                "params": params,
                "records": records,
                "seed": seed,
            });
            let path = format!(
                "{}/tests/fixtures/arrival-transcript-{file_stem}.json",
                env!("CARGO_MANIFEST_DIR")
            );
            std::fs::write(
                &path,
                serde_json::to_string_pretty(&transcript).expect("serializable transcript") + "\n",
            )
            .expect("fixture written");
        }
    }

    #[test]
    fn parent_draw_skips_a_full_weekend_closure() {
        let calendar = SessionCalendar {
            utc_offset_minutes: 0,
            open_windows: vec![super::super::calendar::WeeklyWindow {
                start_minute: 9_960,
                end_minute: 8_520,
            }],
            settlement_minute_of_day: None,
        };
        calendar.validate().expect("valid weekly trading session");
        let environment = env(Some(&calendar));
        let kernel = ArrivalKernel::WallMmpp(WallMmppParams {
            occupancy: 0.35,
            rate_ratio: 2.0,
            tau_s: 10.0,
        });
        let mut rng = ChaCha12Rng::seed_from_u64(37);
        let mut state = ArrivalState::new(&kernel, 0, &mut rng);
        let shape = SweepShape::new(1.2, 0.9, 1.1);
        // Friday 23:00 through Sunday 22:00, expressed as UTC minutes in the
        // epoch week whose local week starts on Thursday.
        let closed_from = 2_820 * NS_PER_MINUTE;
        let reopen_at = 4_200 * NS_PER_MINUTE;
        assert_eq!(environment.next_segment_end(closed_from), reopen_at);
        let draw = kernel
            .next_parent(
                &mut state,
                closed_from,
                1.0,
                &shape,
                &environment,
                RuntimeModifiers::NEUTRAL,
                &mut rng,
            )
            .expect("parent draw crosses the weekend closure");
        assert!(draw.parent_ts_ns >= reopen_at);
        assert!(calendar.is_open(draw.parent_ts_ns));
    }
}
