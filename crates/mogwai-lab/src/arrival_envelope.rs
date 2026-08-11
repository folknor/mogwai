// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b section 9.7 predictive-envelope count simulator.

use std::array;
use std::time::Instant;

use mogwai_data::{
    ARRIVAL_KERNEL_VERSION, ARRIVAL_X_CEILING, ArrivalEnv, CadenceWalk, GeneratedSource,
    ParentSummary, SizeGrid,
};
use mogwai_server::source::InstrumentProfile;
use rand::{RngExt, SeedableRng};
use rand_chacha::ChaCha12Rng;
use rand_distr::{Distribution, Exp, Exp1, Gamma, Poisson, StandardNormal};
use serde::Serialize;
use serde_json::Value;

#[cfg(test)]
use crate::arrival_screen::CADENCE_STEP_NS;
use crate::arrival_screen::{
    A3_GATED_HOURS, Cell, ENVELOPE_CELL_BUDGET_S, ENVELOPE_ORDER, ENVELOPE_PROBE_MONTHS,
    ENVELOPE_REPLICATES, ENVELOPE_STREAM_TAG, EnvelopeDemand, Family,
};
use crate::error::{LabError, LabResult};
use crate::kernel::tuple_mix;
use crate::ledger::sha256_bytes;

const NS_PER_SECOND: u64 = 1_000_000_000;
const NS_PER_HOUR: u64 = 3_600 * NS_PER_SECOND;
const EXPECTED_COUNT_FLOOR: f64 = 0.01;

#[derive(Debug, Clone)]
pub struct ExposureGrid {
    pub start_ns: u64,
    pub end_ns: u64,
    pub step_ns: u64,
    pub identity_hash: u64,
    baseline: Vec<f64>,
    hours: Vec<u8>,
    scheduled: Vec<bool>,
}

impl ExposureGrid {
    pub fn new(
        profile: &InstrumentProfile,
        start_ns: u64,
        length_ns: u64,
        step_ns: u64,
    ) -> LabResult<Self> {
        if step_ns == 0 || !NS_PER_SECOND.is_multiple_of(step_ns) {
            return Err(LabError::refusal(
                "the envelope grid step must divide one second exactly",
            ));
        }
        let end_ns = start_ns.saturating_add(length_ns);
        let env = ArrivalEnv::for_profile_with_step(
            &profile.session,
            profile.calendar.as_ref(),
            1.0,
            start_ns,
            step_ns,
        );
        let mut baseline = Vec::new();
        let mut hours = Vec::new();
        let mut scheduled = Vec::new();
        let dt_s = step_ns as f64 / NS_PER_SECOND as f64;
        let mut clock_ns = start_ns;
        while clock_ns < end_ns {
            let exposed = profile
                .calendar
                .as_ref()
                .is_none_or(|calendar| calendar.is_open(clock_ns));
            baseline.push(if exposed {
                dt_s * env.rate_at(clock_ns) / profile.scalars.mean_event_duration_s
            } else {
                0.0
            });
            hours.push(((clock_ns / NS_PER_HOUR) % 24) as u8);
            scheduled.push(exposed);
            clock_ns = clock_ns.saturating_add(step_ns);
        }
        let mut identity = Vec::with_capacity(64 + baseline.len() * 10);
        identity.extend_from_slice(&start_ns.to_le_bytes());
        identity.extend_from_slice(&length_ns.to_le_bytes());
        identity.extend_from_slice(&step_ns.to_le_bytes());
        for ((expected, hour), open) in baseline.iter().zip(&hours).zip(&scheduled) {
            identity.extend_from_slice(&expected.to_bits().to_le_bytes());
            identity.push(*hour);
            identity.push(u8::from(*open));
        }
        let digest = sha256_bytes(&identity);
        let identity_hash = u64::from_str_radix(&digest[..16], 16)
            .map_err(|error| LabError::refusal(format!("invalid exposure digest: {error}")))?;
        Ok(Self {
            start_ns,
            end_ns,
            step_ns,
            identity_hash,
            baseline,
            hours,
            scheduled,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.baseline.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.baseline.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MonthStats {
    pub hourly_rate: [f64; 24],
    pub hourly_zero_fraction: [f64; 24],
    /// The simulated month left the representable intensity range. Spec 9.7's
    /// replicate ceiling rule: this is a REPLICATE outcome, not a run-ending
    /// fault - the replicate's max deviation becomes infinite and evaluation
    /// continues. A4 still refuses actual candidate walks that breach.
    pub ceiling_breached: bool,
}

/// One gate's envelope plus the replicate indices whose constituent months
/// left the representable range (spec 9.7).
#[derive(Debug, Clone, Default)]
pub struct EnvelopeOutcome {
    pub a2: Option<f64>,
    pub a3: Option<f64>,
    pub ceiling_breached_replicates: Vec<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeRecord {
    pub evaluated: bool,
    pub deciding_statistic: Option<Value>,
    pub order_statistic_value: Option<f64>,
    pub replicates: usize,
    pub order: usize,
    pub k: usize,
    /// Replicate INDICES with at least one ceiling-breached constituent month,
    /// observed or generated alike (spec 9.7 and the 9.5 field list).
    pub ceiling_breached_replicates: Vec<usize>,
    /// Where this gate's deviations stood: inside base, in the marginal shell,
    /// or past the cap. Recorded whether or not an envelope was evaluated.
    pub classification: EnvelopeDemand,
    /// Why a MARGINAL gate went unevaluated, per the 2026-08-11
    /// decision-relevant envelope amendment. `None` when the gate needed no
    /// envelope or actually got one.
    pub skip_reason: Option<String>,
    pub stream_identity_fields: EnvelopeStreamIdentity,
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeStreamIdentity {
    pub family_id: u64,
    pub parameter_f64_bits_in_declared_order: Vec<u64>,
    pub k: usize,
    pub cadence_step_ns: u64,
    pub arrival_kernel_version: u32,
    pub exposure_hash: u64,
    pub replicate: [usize; 2],
    pub side: [u64; 2],
    pub member: [usize; 2],
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvelopeProbeCost {
    pub family: Family,
    pub k: usize,
    /// DERIVED, not measured: `per_month_s * ENVELOPE_REPLICATES * (1 + k)`.
    pub cost_s: f64,
    pub budget_s: f64,
    pub passed: bool,
    /// The measured unit behind `cost_s`, recorded so a later run can tell a
    /// pricing regression from a derivation error, and so an optimization round
    /// has a number to move.
    pub measured_months: usize,
    pub measured_s: f64,
    pub per_month_s: f64,
    /// Work size beside the timing, as everywhere else.
    pub grid_cells: usize,
    pub work_sink: f64,
}

impl EnvelopeRecord {
    #[must_use]
    pub fn unevaluated(cell: &Cell, grid: &ExposureGrid, k: usize) -> Self {
        Self {
            evaluated: false,
            deciding_statistic: None,
            order_statistic_value: None,
            classification: EnvelopeDemand::InsideBase,
            skip_reason: None,
            replicates: ENVELOPE_REPLICATES,
            order: ENVELOPE_ORDER,
            k,
            ceiling_breached_replicates: Vec::new(),
            stream_identity_fields: stream_identity_fields(cell, grid, k),
        }
    }
}

#[must_use]
pub fn stream_identity_fields(
    cell: &Cell,
    grid: &ExposureGrid,
    k: usize,
) -> EnvelopeStreamIdentity {
    EnvelopeStreamIdentity {
        family_id: family_id(cell.family()),
        parameter_f64_bits_in_declared_order: parameter_bits(cell),
        k,
        cadence_step_ns: grid.step_ns,
        arrival_kernel_version: ARRIVAL_KERNEL_VERSION,
        exposure_hash: grid.identity_hash,
        replicate: [1, ENVELOPE_REPLICATES],
        side: [0, 1],
        member: [0, k],
    }
}

fn family_id(family: Family) -> u64 {
    match family {
        Family::EventMarkov => 1,
        Family::WallMmpp => 2,
        Family::LogOuCox => 3,
        Family::SelfExciting => 4,
        Family::ShotNoise => 6,
    }
}

fn parameter_bits(cell: &Cell) -> Vec<u64> {
    match cell {
        Cell::EventMarkov { switch_rate } => vec![switch_rate.to_bits()],
        Cell::WallMmpp {
            occupancy,
            rate_ratio,
            tau_s,
        } => vec![occupancy.to_bits(), rate_ratio.to_bits(), tau_s.to_bits()],
        Cell::LogOuCox { sigma_y, tau_s } => vec![sigma_y.to_bits(), tau_s.to_bits()],
        Cell::SelfExciting { phi, tau_s } => vec![phi.to_bits(), tau_s.to_bits()],
        Cell::ShotNoise { m, k, tau_s } => vec![m.to_bits(), k.to_bits(), tau_s.to_bits()],
    }
}

#[must_use]
pub fn envelope_seed(
    cell: &Cell,
    k: usize,
    grid: &ExposureGrid,
    replicate: usize,
    side: u64,
    member: usize,
) -> u64 {
    let mut fields = Vec::with_capacity(10 + parameter_bits(cell).len());
    fields.push(family_id(cell.family()));
    fields.extend(parameter_bits(cell));
    fields.push(k as u64);
    fields.push(grid.step_ns);
    fields.push(u64::from(ARRIVAL_KERNEL_VERSION));
    fields.push(grid.identity_hash);
    fields.push(replicate as u64);
    fields.push(side);
    fields.push(member as u64);
    tuple_mix(ENVELOPE_STREAM_TAG, &fields)
}

enum State {
    EventMarkov { quiet: bool },
    WallMmpp { quiet: bool },
    LogOuCox { y: f64 },
    SelfExciting { a: f64 },
    ShotNoise { s: f64 },
}

fn initial_state(cell: &Cell, rng: &mut ChaCha12Rng) -> State {
    match cell {
        Cell::EventMarkov { .. } => State::EventMarkov { quiet: false },
        Cell::WallMmpp { occupancy, .. } => State::WallMmpp {
            quiet: rng.random::<f64>() < *occupancy,
        },
        Cell::LogOuCox { sigma_y, .. } => State::LogOuCox {
            y: *sigma_y * <StandardNormal as Distribution<f64>>::sample(&StandardNormal, rng),
        },
        Cell::SelfExciting { .. } => State::SelfExciting { a: 1.0 },
        Cell::ShotNoise { m, k, .. } => State::ShotNoise {
            s: Gamma::new(*k, *m / *k)
                .expect("validated shot-noise Gamma parameters")
                .sample(rng),
        },
    }
}

fn level(cell: &Cell, state: &State) -> f64 {
    match (cell, state) {
        (Cell::EventMarkov { .. }, State::EventMarkov { quiet }) => {
            let active = 0.65 + 0.35 * 150.0;
            if *quiet { active / 150.0 } else { active }
        }
        (
            Cell::WallMmpp {
                occupancy,
                rate_ratio,
                ..
            },
            State::WallMmpp { quiet },
        ) => {
            let denom = occupancy + (1.0 - occupancy) * rate_ratio;
            if *quiet {
                1.0 / denom
            } else {
                *rate_ratio / denom
            }
        }
        (Cell::LogOuCox { sigma_y, .. }, State::LogOuCox { y }) => {
            (*y - sigma_y * sigma_y / 2.0).exp()
        }
        (Cell::SelfExciting { phi, .. }, State::SelfExciting { a }) => (1.0 - phi) + phi * *a,
        (Cell::ShotNoise { m, .. }, State::ShotNoise { s }) => 1.0 - m + *s,
        _ => f64::NAN,
    }
}

/// Everything a family's per-cell transition needs that does NOT vary from cell
/// to cell, built once per walk.
///
/// The loop below runs 2.67 million times per month at the frozen exposure, and
/// it used to rebuild `Poisson`, `Exp` and `exp(-dt/tau)` inside every one of
/// them - constants, all of them, since `dt`, `tau`, `k` and `m` are fixed for
/// the walk. Hoisting is BIT-IDENTICAL rather than merely law-preserving: the
/// distribution objects are the same values built by the same code, sampled by
/// the same algorithm against the same stream, so no envelope number, gate
/// verdict or conformance figure moves. That is why this is an optimization
/// rather than a re-bless.
enum Consts {
    EventMarkov,
    WallMmpp {
        flip: f64,
    },
    LogOuCox {
        d: f64,
        spread: f64,
    },
    SelfExciting {
        d: f64,
    },
    ShotNoise {
        d: f64,
        jumps: Poisson<f64>,
        size: Exp<f64>,
    },
}

impl Consts {
    fn new(cell: &Cell, dt_s: f64) -> LabResult<Self> {
        Ok(match *cell {
            Cell::EventMarkov { .. } => Self::EventMarkov,
            Cell::WallMmpp { tau_s, .. } => Self::WallMmpp {
                flip: 1.0 - (-dt_s / tau_s).exp(),
            },
            Cell::LogOuCox { sigma_y, tau_s } => {
                let d = (-dt_s / tau_s).exp();
                Self::LogOuCox {
                    d,
                    spread: sigma_y * (1.0 - d * d).sqrt(),
                }
            }
            Cell::SelfExciting { tau_s, .. } => Self::SelfExciting {
                d: (-dt_s / tau_s).exp(),
            },
            Cell::ShotNoise { m, k, tau_s } => Self::ShotNoise {
                d: (-dt_s / tau_s).exp(),
                jumps: Poisson::new(k / tau_s * dt_s)
                    .map_err(|error| LabError::refusal(error.to_string()))?,
                size: Exp::new(k / m).map_err(|error| LabError::refusal(error.to_string()))?,
            },
        })
    }
}

fn transition(
    cell: &Cell,
    consts: &Consts,
    state: &mut State,
    expected: f64,
    count: u64,
    dt_s: f64,
    rng: &mut ChaCha12Rng,
) {
    match (cell, state) {
        (Cell::EventMarkov { switch_rate }, State::EventMarkov { quiet }) => {
            for _ in 0..count {
                let p = if *quiet {
                    *switch_rate * 0.65
                } else {
                    *switch_rate * 0.35
                };
                if rng.random::<f64>() < p {
                    *quiet = !*quiet;
                }
            }
        }
        (Cell::WallMmpp { occupancy, .. }, State::WallMmpp { quiet }) => {
            let Consts::WallMmpp { flip } = consts else {
                return;
            };
            let p = if *quiet {
                (1.0 - occupancy) * flip
            } else {
                *occupancy * flip
            };
            if rng.random::<f64>() < p {
                *quiet = !*quiet;
            }
        }
        (Cell::LogOuCox { .. }, State::LogOuCox { y }) => {
            let Consts::LogOuCox { d, spread } = consts else {
                return;
            };
            let z = <StandardNormal as Distribution<f64>>::sample(&StandardNormal, rng);
            *y = d.mul_add(*y, spread * z);
        }
        (Cell::SelfExciting { .. }, State::SelfExciting { a }) => {
            let Consts::SelfExciting { d } = consts else {
                return;
            };
            let observed = if expected >= EXPECTED_COUNT_FLOOR {
                count as f64 / expected
            } else {
                1.0
            };
            *a = d.mul_add(*a, (1.0 - d) * observed);
        }
        (Cell::ShotNoise { tau_s, .. }, State::ShotNoise { s }) => {
            let Consts::ShotNoise { d, jumps, size } = consts else {
                return;
            };
            let drawn = jumps.sample(rng) as u64;
            *s *= d;
            for _ in 0..drawn {
                let arrival_s = rng.random::<f64>() * dt_s;
                *s += size.sample(rng) * (-(dt_s - arrival_s) / *tau_s).exp();
            }
        }
        _ => {}
    }
}

pub fn simulate_month(cell: &Cell, grid: &ExposureGrid, seed: u64) -> LabResult<MonthStats> {
    let mut rng = ChaCha12Rng::seed_from_u64(seed);
    let mut state = initial_state(cell, &mut rng);
    let mut counts = [0_u64; 24];
    let mut exposure_s = [0.0_f64; 24];
    let mut zeros = [0_u64; 24];
    let mut seconds = [0_u64; 24];
    let mut second_count = 0_u64;
    let mut second_scheduled = false;
    let per_second = NS_PER_SECOND / grid.step_ns;
    let dt_s = grid.step_ns as f64 / NS_PER_SECOND as f64;
    let consts = Consts::new(cell, dt_s)?;
    for index in 0..grid.len() {
        let x = level(cell, &state);
        if !x.is_finite() || x > ARRIVAL_X_CEILING {
            // Spec 9.7's replicate ceiling rule: stop this month immediately
            // and report the breach. The caller makes the replicate's max
            // deviation infinite; it never ends the run. The partial per-hour
            // statistics are left as accumulated and are never read, because a
            // breached month contributes only its infinite deviation.
            return Ok(MonthStats {
                ceiling_breached: true,
                ..MonthStats::default()
            });
        }
        let expected = if cell.family() == Family::EventMarkov {
            grid.baseline[index] / 0.944
        } else {
            grid.baseline[index]
        };
        let count = if cell.family() == Family::EventMarkov && expected > 0.0 {
            let mut count = 0_u64;
            let mut remaining_s = dt_s;
            loop {
                // `Exp1` sampled directly and scaled, rather than a fresh `Exp`
                // per iteration: this loop runs about sixteen times per grid
                // cell and tens of millions of times per month, and the rate
                // moves with the latent state on every pass, so there is
                // nothing to hoist - only the construction to remove. The
                // scaling reproduces `Exp`'s own arithmetic (it stores the
                // reciprocal and multiplies), so this is BIT-IDENTICAL.
                //
                // Inverse CDF was tried here and measured 10 percent SLOWER on
                // this toolchain and RNG than the ziggurat, which takes its
                // rejection branch rarely enough to beat an unconditional
                // logarithm. Recorded so the substitution is not made blindly,
                // NOT as a claim about every future toolchain: a cheaper `ln`
                // or a different sampler could reverse it, and the way to know
                // is to measure again. The durable figure is in
                // `reference/performance.md`.
                let rate = expected / dt_s * level(cell, &state);
                let gap_s = <Exp1 as Distribution<f64>>::sample(&Exp1, &mut rng) * (1.0 / rate);
                if gap_s >= remaining_s {
                    break;
                }
                remaining_s -= gap_s;
                count = count.saturating_add(1);
                transition(cell, &consts, &mut state, expected, 1, dt_s, &mut rng);
            }
            count
        } else if expected > 0.0 {
            Poisson::new(expected * x)
                .map_err(|error| LabError::refusal(error.to_string()))?
                .sample(&mut rng) as u64
        } else {
            0
        };
        let hour = usize::from(grid.hours[index]);
        if grid.scheduled[index] {
            counts[hour] = counts[hour].saturating_add(count);
            exposure_s[hour] += dt_s;
            second_count = second_count.saturating_add(count);
            second_scheduled = true;
        }
        if cell.family() != Family::EventMarkov {
            transition(cell, &consts, &mut state, expected, count, dt_s, &mut rng);
        }
        if (index as u64 + 1).is_multiple_of(per_second) {
            if second_scheduled {
                seconds[hour] += 1;
                zeros[hour] += u64::from(second_count == 0);
            }
            second_count = 0;
            second_scheduled = false;
        }
    }
    Ok(MonthStats {
        ceiling_breached: false,
        hourly_rate: array::from_fn(|h| {
            if exposure_s[h] > 0.0 {
                counts[h] as f64 / exposure_s[h]
            } else {
                0.0
            }
        }),
        hourly_zero_fraction: array::from_fn(|h| {
            if seconds[h] > 0 {
                zeros[h] as f64 / seconds[h] as f64
            } else {
                0.0
            }
        }),
    })
}

fn stats_from_parent_walk(
    grid: &ExposureGrid,
    mut next: impl FnMut() -> LabResult<ParentSummary>,
) -> LabResult<MonthStats> {
    let dt_s = grid.step_ns as f64 / NS_PER_SECOND as f64;
    let mut counts = [0_u64; 24];
    let mut exposure_s = [0.0_f64; 24];
    for index in 0..grid.len() {
        if grid.scheduled[index] {
            exposure_s[usize::from(grid.hours[index])] += dt_s;
        }
    }

    let second_len = usize::try_from(
        grid.end_ns
            .saturating_sub(grid.start_ns)
            .div_ceil(NS_PER_SECOND),
    )
    .map_err(|_| LabError::refusal("candidate-walk second count does not fit usize"))?;
    let mut second_counts = vec![0_u32; second_len];
    let mut previous = None;
    loop {
        let parent = next()?;
        if previous.is_some_and(|clock| parent.parent_ts_ns <= clock) {
            return Err(LabError::refusal("candidate walk stalled"));
        }
        previous = Some(parent.parent_ts_ns);
        if parent.parent_ts_ns >= grid.end_ns {
            break;
        }
        if parent.parent_ts_ns < grid.start_ns {
            continue;
        }
        let cell_index =
            usize::try_from(parent.parent_ts_ns.saturating_sub(grid.start_ns) / grid.step_ns)
                .map_err(|_| LabError::refusal("candidate-walk cell index does not fit usize"))?;
        if cell_index >= grid.len() || !grid.scheduled[cell_index] {
            return Err(LabError::refusal(format!(
                "candidate parent at {} is outside the frozen exposure",
                parent.parent_ts_ns
            )));
        }
        let hour = usize::from(grid.hours[cell_index]);
        counts[hour] = counts[hour].saturating_add(1);
        let second =
            usize::try_from(parent.parent_ts_ns.saturating_sub(grid.start_ns) / NS_PER_SECOND)
                .map_err(|_| LabError::refusal("candidate-walk second index does not fit usize"))?;
        second_counts[second] = second_counts[second].saturating_add(1);
    }

    let mut zeros = [0_u64; 24];
    let mut seconds = [0_u64; 24];
    for (second, &count) in second_counts.iter().enumerate() {
        let second = u64::try_from(second)
            .map_err(|_| LabError::refusal("candidate-walk second index does not fit u64"))?;
        let clock_ns = grid
            .start_ns
            .saturating_add(second.saturating_mul(NS_PER_SECOND));
        let cell_index = usize::try_from(clock_ns.saturating_sub(grid.start_ns) / grid.step_ns)
            .map_err(|_| LabError::refusal("second-aligned cell index does not fit usize"))?;
        if cell_index < grid.len() && grid.scheduled[cell_index] {
            let hour = usize::from(grid.hours[cell_index]);
            seconds[hour] += 1;
            zeros[hour] += u64::from(count == 0);
        }
    }
    Ok(MonthStats {
        // A real candidate walk that breached would have refused upstream under
        // A4; reaching here means it did not.
        ceiling_breached: false,
        hourly_rate: array::from_fn(|hour| {
            if exposure_s[hour] > 0.0 {
                counts[hour] as f64 / exposure_s[hour]
            } else {
                0.0
            }
        }),
        hourly_zero_fraction: array::from_fn(|hour| {
            if seconds[hour] > 0 {
                zeros[hour] as f64 / seconds[hour] as f64
            } else {
                0.0
            }
        }),
    })
}

/// Simulates the real candidate walk used by the section 9.7 fidelity gate.
pub fn simulate_candidate_month(
    cell: &Cell,
    grid: &ExposureGrid,
    profile: &InstrumentProfile,
    seed: u64,
) -> LabResult<MonthStats> {
    let mut scalars = profile.scalars.clone();
    scalars.arrival = Some(cell.config());
    match cell.family() {
        Family::EventMarkov => {
            let mut source = GeneratedSource::try_new_with_session_profile(
                scalars,
                seed,
                grid.start_ns,
                mogwai_server::source::fingerprint(),
                &profile.session,
                None,
                SizeGrid::from_def(&profile.def),
                profile.calendar.clone(),
            )
            .map_err(|error| {
                LabError::refusal(format!("building fidelity generator: {error:?}"))
            })?;
            stats_from_parent_walk(grid, || Ok(source.advance_parent()))
        }
        _ => {
            let mut walk = CadenceWalk::new_with_step(
                &scalars,
                &profile.session,
                profile.calendar.as_ref(),
                1.0,
                seed,
                grid.start_ns,
                grid.step_ns,
            )
            .ok_or_else(|| LabError::refusal("fidelity cell has no integrated kernel"))?;
            stats_from_parent_walk(grid, || {
                walk.next()
                    .map(|draw| ParentSummary {
                        parent_ts_ns: draw.parent_ts_ns,
                        child_count: draw.child_count,
                        child_stride_ns: walk.child_stride_ns(),
                    })
                    .map_err(|refusal| {
                        LabError::refusal(format!("fidelity candidate refused: {refusal:?}"))
                    })
            })
        }
    }
}

fn log_deviation(numerator: f64, denominator: f64) -> f64 {
    if numerator <= 0.0 || denominator <= 0.0 {
        f64::INFINITY
    } else {
        (numerator / denominator).ln().abs()
    }
}

pub fn predictive_envelopes(
    cell: &Cell,
    grid: &ExposureGrid,
    k: usize,
    need_a2: bool,
    need_a3: bool,
) -> LabResult<EnvelopeOutcome> {
    let mut a2 = Vec::with_capacity(ENVELOPE_REPLICATES);
    let mut a3 = Vec::with_capacity(ENVELOPE_REPLICATES);
    let mut ceiling_breached_replicates = Vec::new();
    for replicate in 1..=ENVELOPE_REPLICATES {
        let observed = simulate_month(cell, grid, envelope_seed(cell, k, grid, replicate, 0, 0))?;
        let mut generated_rate = [0.0; 24];
        let mut generated_zero = [0.0; 24];
        // Spec 9.7's replicate ceiling rule: ANY breached constituent month
        // makes this replicate's max deviation infinite for EVERY requested
        // gate, the remaining members are not simulated, and evaluation
        // continues with the next replicate. Up to 16 such replicates leave
        // rank 484 finite; 17 or more make the envelope infinite, which the
        // gate's materiality cap then truncates.
        let mut breached = observed.ceiling_breached;
        if !breached {
            for member in 1..=k {
                let generated = simulate_month(
                    cell,
                    grid,
                    envelope_seed(cell, k, grid, replicate, 1, member),
                )?;
                if generated.ceiling_breached {
                    breached = true;
                    break;
                }
                for hour in 0..24 {
                    generated_rate[hour] += generated.hourly_rate[hour] / k as f64;
                    generated_zero[hour] += generated.hourly_zero_fraction[hour] / k as f64;
                }
            }
        }
        if breached {
            ceiling_breached_replicates.push(replicate);
            if need_a2 {
                a2.push(f64::INFINITY);
            }
            if need_a3 {
                a3.push(f64::INFINITY);
            }
            continue;
        }
        if need_a2 {
            a2.push(
                (0..24)
                    .filter(|hour| {
                        grid.scheduled
                            .iter()
                            .zip(&grid.hours)
                            .any(|(open, h)| *open && usize::from(*h) == *hour)
                    })
                    .map(|hour| log_deviation(generated_rate[hour], observed.hourly_rate[hour]))
                    .fold(0.0, f64::max),
            );
        }
        if need_a3 {
            a3.push(
                A3_GATED_HOURS
                    .iter()
                    .map(|&hour| {
                        let hour = hour as usize;
                        log_deviation(generated_zero[hour], observed.hourly_zero_fraction[hour])
                    })
                    .fold(0.0, f64::max),
            );
        }
    }
    a2.sort_by(f64::total_cmp);
    a3.sort_by(f64::total_cmp);
    Ok(EnvelopeOutcome {
        a2: need_a2.then(|| a2[ENVELOPE_ORDER - 1]),
        a3: need_a3.then(|| a3[ENVELOPE_ORDER - 1]),
        ceiling_breached_replicates,
    })
}

#[must_use]
pub const fn envelope_probe_cell(family: Family) -> Cell {
    match family {
        Family::EventMarkov => Cell::EventMarkov { switch_rate: 0.5 },
        Family::WallMmpp => Cell::WallMmpp {
            occupancy: 0.10,
            rate_ratio: 200.0,
            tau_s: 1.0,
        },
        // 1.4, not 2.0: a real candidate walk at sigma 2.0 breaches
        // ARRIVAL_X_CEILING within a month and refuses under A4, so it can
        // probe neither cost nor fidelity. tau stays at its minimum.
        Family::LogOuCox => Cell::LogOuCox {
            sigma_y: 1.4,
            tau_s: 1.0,
        },
        Family::SelfExciting => Cell::SelfExciting {
            phi: 0.98,
            tau_s: 2.0,
        },
        Family::ShotNoise => Cell::ShotNoise {
            m: 0.8,
            k: 10.0,
            tau_s: 1.0,
        },
    }
}

/// Runs the frozen section 9.7 worst-cell probes for every family and K.
pub fn probe_envelope_costs(
    profile: &InstrumentProfile,
    start_ns: u64,
    length_ns: u64,
    step_ns: u64,
) -> LabResult<Vec<EnvelopeProbeCost>> {
    let grid = ExposureGrid::new(profile, start_ns, length_ns, step_ns)?;
    let mut costs = Vec::with_capacity(Family::ALL.len() * ENVELOPE_CELL_BUDGET_S.len());
    for family in Family::ALL {
        let cell = envelope_probe_cell(family);
        // UNIT AND DERIVE, per the 2026-08-11 pricing amendment. The old probe
        // ran a FULL evaluation per tier - 8,500 months per family, about 2.5
        // hours in total - to learn a price that is nothing but a month count
        // times a unit. Measuring the unit once and multiplying costs about 34
        // seconds for the whole probe and is strictly more informative, because
        // the artifact then carries the unit a later optimization moves.
        //
        // The linearity is exact in WORK and estimated in WALL: the evidence is
        // a real 1,500-month evaluation that measured 584.287 s against a
        // 558.8 s derivation, 4.5 percent apart on a fifty-fold extrapolation,
        // which the 15 percent headroom absorbs.
        let started = Instant::now();
        let mut sink = 0.0;
        for replicate in 1..=ENVELOPE_PROBE_MONTHS {
            let month = simulate_month(
                &cell,
                &grid,
                envelope_seed(&cell, 2, &grid, replicate, 1, 1),
            )?;
            sink += month.hourly_rate.iter().sum::<f64>();
        }
        let measured_s = started.elapsed().as_secs_f64();
        let per_month_s = measured_s / ENVELOPE_PROBE_MONTHS as f64;
        for &(k, budget_s) in &ENVELOPE_CELL_BUDGET_S {
            let cost_s = per_month_s * ENVELOPE_REPLICATES as f64 * (1 + k) as f64;
            let passed = cost_s <= budget_s;
            costs.push(EnvelopeProbeCost {
                family,
                k,
                cost_s,
                budget_s,
                passed,
                measured_months: ENVELOPE_PROBE_MONTHS,
                measured_s,
                per_month_s,
                grid_cells: grid.len(),
                work_sink: sink,
            });
            if !passed {
                return Err(LabError::refusal(format!(
                    "envelope-cell-budget-exceeded: {} K={k} derives {cost_s:.3}s from a \
                     measured {per_month_s:.4}s per month, exceeding {budget_s:.3}s",
                    family.as_str()
                )));
            }
        }
    }
    Ok(costs)
}

fn exogenous_closed_form(cell: &Cell, grid: &ExposureGrid, target_hour: usize) -> Option<f64> {
    let dt_s = grid.step_ns as f64 / NS_PER_SECOND as f64;
    let components: Vec<(f64, f64)> = match *cell {
        Cell::WallMmpp {
            occupancy,
            rate_ratio,
            tau_s,
        } => {
            let denom = occupancy + (1.0 - occupancy) * rate_ratio;
            let quiet = 1.0 / denom;
            let active = rate_ratio / denom;
            let variance =
                occupancy * (quiet - 1.0).powi(2) + (1.0 - occupancy) * (active - 1.0).powi(2);
            vec![(variance, (-dt_s / tau_s).exp())]
        }
        Cell::ShotNoise { m, k, tau_s } => {
            vec![(m * m / k, (-dt_s / tau_s).exp())]
        }
        Cell::LogOuCox { sigma_y, tau_s } => {
            let rho = (-dt_s / tau_s).exp();
            let sigma2 = sigma_y * sigma_y;
            let mut coefficient = 1.0;
            let mut out = Vec::new();
            for n in 1..=64_u32 {
                coefficient *= sigma2 / f64::from(n);
                out.push((coefficient, rho.powf(f64::from(n))));
                if coefficient < 1e-15 {
                    break;
                }
            }
            out
        }
        _ => return None,
    };
    let mut overlap = vec![0.0; components.len()];
    let mut extra = 0.0;
    let mut expectation = 0.0;
    for index in 0..grid.len() {
        for (z, (_, rho)) in overlap.iter_mut().zip(&components) {
            *z *= rho;
        }
        if usize::from(grid.hours[index]) != target_hour {
            continue;
        }
        let b = grid.baseline[index];
        expectation += b;
        for (z, (coefficient, _)) in overlap.iter_mut().zip(&components) {
            extra += coefficient * b * (b + 2.0 * *z);
            *z += b;
        }
    }
    (expectation > 0.0).then(|| (expectation + extra) / expectation.powi(2))
}

fn self_exciting_closed_form(
    phi: f64,
    tau_s: f64,
    grid: &ExposureGrid,
    target_hour: usize,
) -> Option<f64> {
    let dt_s = grid.step_ns as f64 / NS_PER_SECOND as f64;
    let d = (-dt_s / tau_s).exp();
    let rho = d + (1.0 - d) * phi;
    let mut var_u = 0.0;
    let mut cov_sum_u = 0.0;
    let mut var_sum = 0.0;
    let mut expectation = 0.0;
    for index in 0..grid.len() {
        let b = grid.baseline[index];
        let target = usize::from(grid.hours[index]) == target_hour;
        let var_count = b + b * b * phi * phi * var_u;
        if target {
            var_sum += var_count + 2.0 * b * phi * cov_sum_u;
            expectation += b;
        }
        if b >= EXPECTED_COUNT_FLOOR {
            cov_sum_u = if target {
                rho * (cov_sum_u + b * phi * var_u) + (1.0 - d)
            } else {
                rho * cov_sum_u
            };
            var_u = rho * rho * var_u + (1.0 - d).powi(2) / b;
        } else {
            cov_sum_u = d * (cov_sum_u + if target { b * phi * var_u } else { 0.0 });
            var_u *= d * d;
        }
    }
    (expectation > 0.0).then(|| var_sum / expectation.powi(2))
}

#[must_use]
pub fn closed_form_variance_ratio(
    cell: &Cell,
    grid: &ExposureGrid,
    target_hour: usize,
) -> Option<f64> {
    match *cell {
        Cell::SelfExciting { phi, tau_s } => {
            self_exciting_closed_form(phi, tau_s, grid, target_hour)
        }
        _ => exogenous_closed_form(cell, grid, target_hour),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::arrival_screen::{CONFORMANCE_BUDGET_S, GRID_SENSITIVITY_STEP_NS};

    const M_CONF: usize = 2_000;

    fn conformance_cells() -> Vec<Cell> {
        vec![
            Cell::WallMmpp {
                occupancy: 0.30,
                rate_ratio: 20.0,
                tau_s: 46.4158883361278,
            },
            // Corners CROSS-PAIRED per the 2026-08-11 conformance-cell
            // amendment: each parameter extreme meets the tau endpoint whose
            // estimator dispersion permits a discriminating test, so both tau
            // endpoints stay covered without the statistically unattainable
            // heavy-parameter long-tau combinations (spec 9.7; evidence in
            // scripts/envelope_corner_check.py and _check2.py).
            Cell::WallMmpp {
                occupancy: 0.10,
                rate_ratio: 2.0,
                tau_s: 3600.0,
            },
            Cell::WallMmpp {
                occupancy: 0.60,
                rate_ratio: 200.0,
                tau_s: 1.0,
            },
            Cell::LogOuCox {
                sigma_y: 1.0,
                tau_s: 46.4158883361278,
            },
            Cell::LogOuCox {
                sigma_y: 0.2,
                tau_s: 3600.0,
            },
            // 1.4, not the domain maximum 2.0: at sigma 2.0 an
            // ARRIVAL_X_CEILING breach needs only a 5.6 sigma excursion,
            // which this workload's 3.3e8 effective draws hit near-certainly.
            Cell::LogOuCox {
                sigma_y: 1.4,
                tau_s: 1.0,
            },
            Cell::SelfExciting {
                phi: 0.55,
                tau_s: 43.088693800637664,
            },
            Cell::SelfExciting {
                phi: 0.10,
                tau_s: 600.0,
            },
            Cell::SelfExciting {
                phi: 0.98,
                tau_s: 2.0,
            },
            Cell::ShotNoise {
                m: 0.5,
                k: 1.0,
                tau_s: 46.4158883361278,
            },
            Cell::ShotNoise {
                m: 0.2,
                k: 10.0,
                tau_s: 3600.0,
            },
            Cell::ShotNoise {
                m: 0.8,
                k: 0.1,
                tau_s: 1.0,
            },
        ]
    }

    #[test]
    fn the_envelope_stream_identity_mixes_every_frozen_field() {
        let profile = mogwai_server::config::profile_from_preset("MNQ").expect("MNQ profile");
        let one_second = ExposureGrid::new(
            &profile,
            1_782_856_800_000_000_000,
            10_000_000_000,
            CADENCE_STEP_NS,
        )
        .expect("one-second grid");
        let quarter_second = ExposureGrid::new(
            &profile,
            1_782_856_800_000_000_000,
            10_000_000_000,
            GRID_SENSITIVITY_STEP_NS,
        )
        .expect("quarter-second grid");
        let base = Cell::WallMmpp {
            occupancy: 0.3,
            rate_ratio: 20.0,
            tau_s: 60.0,
        };
        let changed_parameter = Cell::WallMmpp {
            occupancy: 0.4,
            rate_ratio: 20.0,
            tau_s: 60.0,
        };
        let seeds = [
            envelope_seed(&base, 2, &one_second, 1, 0, 0),
            envelope_seed(&changed_parameter, 2, &one_second, 1, 0, 0),
            envelope_seed(
                &Cell::LogOuCox {
                    sigma_y: 0.3,
                    tau_s: 60.0,
                },
                2,
                &one_second,
                1,
                0,
                0,
            ),
            envelope_seed(&base, 4, &one_second, 1, 0, 0),
            envelope_seed(&base, 2, &quarter_second, 1, 0, 0),
            envelope_seed(&base, 2, &one_second, 2, 0, 0),
            envelope_seed(&base, 2, &one_second, 1, 1, 0),
            envelope_seed(&base, 2, &one_second, 1, 1, 1),
        ];
        let unique: std::collections::BTreeSet<_> = seeds.into_iter().collect();
        assert_eq!(unique.len(), seeds.len());
        assert_eq!(
            envelope_seed(&base, 2, &one_second, 1, 0, 0),
            envelope_seed(&base, 2, &one_second, 1, 0, 0)
        );
        let fields = stream_identity_fields(&base, &one_second, 2);
        assert_eq!(fields.arrival_kernel_version, ARRIVAL_KERNEL_VERSION);
        assert_eq!(fields.replicate, [1, ENVELOPE_REPLICATES]);
        assert_eq!(fields.side, [0, 1]);
        assert_eq!(fields.member, [0, 2]);
    }

    #[test]
    #[ignore = "release-mode conformance gate, bounded by 900 seconds"]
    fn the_envelope_matches_the_closed_forms_where_they_are_exact() {
        let started = Instant::now();
        let profile = mogwai_server::config::profile_from_preset("MNQ").expect("MNQ profile");
        let start_ns = 1_782_856_800_000_000_000;
        let grid = ExposureGrid::new(
            &profile,
            start_ns,
            48 * 3_600 * NS_PER_SECOND,
            CADENCE_STEP_NS,
        )
        .expect("two-session exposure");
        for cell in conformance_cells() {
            let months: Vec<_> = (1..=M_CONF)
                .map(|replicate| {
                    let seed = envelope_seed(&cell, 1, &grid, replicate, 1, 1);
                    simulate_month(&cell, &grid, seed).expect("conformance exposure")
                })
                .collect();
            for hour in [0_usize, 14, 19] {
                let closed = closed_form_variance_ratio(&cell, &grid, hour)
                    .expect("kernel family has a closed form");
                let expectation: f64 = grid
                    .baseline
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| usize::from(grid.hours[*i]) == hour)
                    .map(|(_, b)| *b)
                    .sum();
                let values: Vec<f64> = months
                    .iter()
                    .map(|month| {
                        month.hourly_rate[hour]
                            * grid
                                .scheduled
                                .iter()
                                .enumerate()
                                .filter(|(i, open)| **open && usize::from(grid.hours[*i]) == hour)
                                .count() as f64
                            / expectation
                    })
                    .collect();
                let mean = values.iter().sum::<f64>() / M_CONF as f64;
                let sample_var =
                    values.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (M_CONF - 1) as f64;
                let mu4 = values.iter().map(|x| (x - mean).powi(4)).sum::<f64>() / M_CONF as f64;
                let sigma4 = sample_var * sample_var;
                let se_plugin = ((mu4 - sigma4 * (M_CONF - 3) as f64 / (M_CONF - 1) as f64)
                    / M_CONF as f64)
                    .max(0.0)
                    .sqrt();
                let tolerance = (5.0 * se_plugin).min(0.5 * closed);
                assert!(
                    (sample_var - closed).abs() <= tolerance,
                    "{cell:?} hour {hour}: sample={sample_var} closed={closed} tolerance={tolerance}"
                );
            }
        }
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!("conformance_wall_s={elapsed:.3}");
        assert!(elapsed <= CONFORMANCE_BUDGET_S);
    }

    fn mean_and_variance(values: &[f64]) -> (f64, f64) {
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = values
            .iter()
            .map(|value| (value - mean).powi(2))
            .sum::<f64>()
            / (values.len() - 1) as f64;
        (mean, variance)
    }

    fn assert_fidelity(
        cell: &Cell,
        step_ns: u64,
        statistic: &str,
        hour: usize,
        idealized: &[f64],
        candidate: &[f64],
    ) {
        let (idealized_mean, idealized_variance) = mean_and_variance(idealized);
        let (candidate_mean, candidate_variance) = mean_and_variance(candidate);
        let combined_se = (idealized_variance / idealized.len() as f64
            + candidate_variance / candidate.len() as f64)
            .sqrt();
        let difference = (idealized_mean - candidate_mean).abs();
        assert!(
            difference <= 5.0 * combined_se,
            "{cell:?} step_ns={step_ns} {statistic} hour={hour}: idealized={idealized_mean} candidate={candidate_mean} difference={difference} combined_se={combined_se}"
        );
    }

    /// The spec 9.7 fidelity comparison for ONE probe family at ONE grid step.
    ///
    /// SPLIT BY (family, step) rather than run as one test, and the reason is a
    /// harness ceiling rather than a contract change: the whole gate is 640
    /// month-scale walks, past the per-test hang watchdog every lane of this
    /// workspace runs under, so a single test would be permanently unrunnable -
    /// which section 18's "every gate needs a command that actually runs"
    /// ruling forbids. Every test below shares the frozen test name as its
    /// PREFIX, so the Brick E gate command still selects the complete gate by
    /// substring while each part gets its own watchdog budget. Nothing
    /// asserted, no constant and no cell moves.
    fn assert_family_fidelity(family: Family, step_ns: u64) {
        const FIDELITY_MONTHS: usize = 32;
        let profile = mogwai_server::config::profile_from_preset("MNQ").expect("MNQ profile");
        let start_ns = 1_782_856_800_000_000_000;
        let length_ns = 2_674_800_000_000_000;
        let cell = envelope_probe_cell(family);
        let grid =
            ExposureGrid::new(&profile, start_ns, length_ns, step_ns).expect("frozen exposure");
        let mut idealized = Vec::with_capacity(FIDELITY_MONTHS);
        let mut candidate = Vec::with_capacity(FIDELITY_MONTHS);
        for replicate in 1..=FIDELITY_MONTHS {
            idealized.push(
                simulate_month(
                    &cell,
                    &grid,
                    envelope_seed(&cell, 1, &grid, replicate, 3, 0),
                )
                .expect("idealized fidelity month"),
            );
            candidate.push(
                simulate_candidate_month(
                    &cell,
                    &grid,
                    &profile,
                    envelope_seed(&cell, 1, &grid, replicate, 2, 0),
                )
                .expect("real candidate fidelity month"),
            );
        }
        for &hour in &A3_GATED_HOURS {
            let hour = usize::try_from(hour).expect("gated hour is nonnegative");
            let idealized_rate: Vec<_> = idealized
                .iter()
                .map(|month| month.hourly_rate[hour])
                .collect();
            let candidate_rate: Vec<_> = candidate
                .iter()
                .map(|month| month.hourly_rate[hour])
                .collect();
            assert_fidelity(
                &cell,
                step_ns,
                "hourly_rate",
                hour,
                &idealized_rate,
                &candidate_rate,
            );
            let idealized_zero: Vec<_> = idealized
                .iter()
                .map(|month| month.hourly_zero_fraction[hour])
                .collect();
            let candidate_zero: Vec<_> = candidate
                .iter()
                .map(|month| month.hourly_zero_fraction[hour])
                .collect();
            assert_fidelity(
                &cell,
                step_ns,
                "hourly_zero_fraction",
                hour,
                &idealized_zero,
                &candidate_zero,
            );
        }
    }

    macro_rules! fidelity_gate {
        ($name:ident, $family:expr, $step:expr) => {
            #[test]
            #[ignore = "month-scale two-implementation fidelity gate"]
            fn $name() {
                assert_family_fidelity($family, $step);
            }
        };
    }

    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_event_markov_1s,
        Family::EventMarkov,
        CADENCE_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_event_markov_250ms,
        Family::EventMarkov,
        GRID_SENSITIVITY_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_wall_mmpp_1s,
        Family::WallMmpp,
        CADENCE_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_wall_mmpp_250ms,
        Family::WallMmpp,
        GRID_SENSITIVITY_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_log_ou_cox_1s,
        Family::LogOuCox,
        CADENCE_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_log_ou_cox_250ms,
        Family::LogOuCox,
        GRID_SENSITIVITY_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_self_exciting_1s,
        Family::SelfExciting,
        CADENCE_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_self_exciting_250ms,
        Family::SelfExciting,
        GRID_SENSITIVITY_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_shot_noise_1s,
        Family::ShotNoise,
        CADENCE_STEP_NS
    );
    fidelity_gate!(
        the_envelope_simulator_is_faithful_to_the_candidate_walks_shot_noise_250ms,
        Family::ShotNoise,
        GRID_SENSITIVITY_STEP_NS
    );
}
