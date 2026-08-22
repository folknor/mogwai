// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b Stage A: the necessary-condition screen.
//!
//! Corpus-free by construction. Both observed projections come from the
//! committed protocol-12a artifact (`analysis/mnq-measure-12a.json`), which
//! already carries the exact sparse joint histogram keyed on exact parent
//! count and the per-hour count histograms at 1 s, 5 s and 60 s. Nothing here
//! reads a TBBO corpus, and nothing here consumes the brick-B4 envelope: the
//! binding admissibility list is A1 to A4 and B4 is a Stage B gate.
//!
//! The spec is `notes/protocol-12b-arrival-composition-spec.md`; sections 9
//! and 16 own everything in this module.

#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Instant;

use mogwai_data::{
    ArrivalConfig, ArrivalRefusal, CadenceWalk, GeneratedSource, ParentSummary, SizeGrid, TickFault,
};
use mogwai_venue::source::InstrumentProfile;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::aggregate::RefusalRec;
#[cfg(test)]
use crate::aggregate::context::ObsContext;
#[cfg(test)]
use crate::aggregate::countsub::{count_substitution, obs_shares_under, support_refusals_of};
#[cfg(test)]
use crate::aggregate::family::conditional_adequacy_bins;
#[cfg(test)]
use crate::aggregate::monthly::pool_session_hists;
use crate::arrival_control::{GeneratedBinding, MEAN_RATE_BAND, gate_hours, seed_median};
#[cfg(test)]
use crate::arrival_control::{
    HourRate, ZERO_COUNT_BAND, hourly_mean_parents, hourly_zero_second_fraction,
};
use crate::arrival_envelope::{
    EnvelopeOutcome, EnvelopeRecord, ExposureGrid, predictive_envelopes,
};
use crate::error::{LabError, LabResult};
use crate::fit::walk::parse_duration;
use crate::kernel::weighted_nearest_rank;
use crate::measure12a::{ScreenReduced, ScreenSessionAcc, ScreenWindow};
use crate::sampler::ResourceSampler;
use crate::session::{format_trade_date, session_segment_at};
use crate::storage::{CacheStore, ProvenanceInputs, ProvenanceToken, cache_root};
use crate::subcontract::PARENT_COUNT_BIN_NAMES;

pub const STAGE_A_SEEDS: [u64; 4] = [201, 202, 203, 204];
pub const A2_SHAPE_BASE: f64 = 0.019_802_627_296_179_73;
pub const A2_SHAPE_CAP: f64 = 0.223_143_551_314_209_76;
pub const A3_BASE: f64 = 0.223_143_551_314_209_76;
pub const A3_CAP: f64 = std::f64::consts::LN_2;
pub const MIN_ZERO_WINDOWS: u64 = 30;
pub const A3_GATED_HOURS: [i64; 20] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18, 19, 20, 22, 23,
];
pub const ENVELOPE_REPLICATES: usize = 500;
pub const ENVELOPE_ORDER: usize = 484;
pub const ENVELOPE_STREAM_TAG: u64 = 0x6D6F6777_61693145;
pub const CADENCE_STEP_NS: u64 = 1_000_000_000;
pub const GRID_SENSITIVITY_STEP_NS: u64 = 250_000_000;
/// Per-evaluation envelope budgets by K, priced from measurement by the
/// 2026-08-11 envelope pricing amendment: the optimized worst family
/// (`shot_noise` at 0.4346 s per replicate month) plus about 15 percent
/// headroom. The pre-amendment 60/120/180 were estimates set before any
/// envelope existed, and the shipped probe refused a real evaluation against
/// them at 584.287 s.
pub const ENVELOPE_CELL_BUDGET_S: [(usize, f64); 3] = [(2, 750.0), (4, 1250.0), (8, 2250.0)];

/// Replicate months the cost probe measures per family before deriving each
/// tier. The tier price is `per_month_s * ENVELOPE_REPLICATES * (1 + K)`,
/// because an evaluation is exactly that many months and nothing else.
pub const ENVELOPE_PROBE_MONTHS: usize = 32;
pub const STAGE_A_ENVELOPE_BUDGET_S: f64 = 21_600.0;
pub const STAGE_B_ENVELOPE_BUDGET_S: f64 = 10_800.0;
pub const STAGE_B_BUDGET_S: f64 = 61_200.0;
pub const CONFORMANCE_BUDGET_S: f64 = 900.0;
pub const REFINEMENT_DEPTH: u8 = 2;
pub const REFINEMENT_CELL_CAP: usize = 600;
pub const STAGE_A_GEN_REFINE_CAP: usize = 40;
/// AMENDED 2026-08-09 by owner ruling, from the frozen 4.0. Brick A0
/// measured a WallMmpp cell at 6.322 s against that bound, so 4.0 was
/// unmeetable by the computation it was meant to price. 7.0 is the measured
/// price plus about a tenth for margin.
///
/// Why this is an amendment rather than threshold-shopping, which section 11
/// forbids: a wall-clock budget changes no verdict. Admissibility is A1 to A4,
/// none of which consults elapsed time, so no cell that failed passes because
/// of this and none that passed now fails. The constants section 11 protects
/// are the BANDS - move one of those and a different candidate survives. The
/// remedy that would have been illegitimate is shortening the measured window
/// in the exposure contract, which changes every statistic; that was refused.
pub const STAGE_A_CELL_BUDGET_S: f64 = 7.0;
pub const STAGE_A_GEN_CELL_BUDGET_S: f64 = 50.0;
/// AMENDED 2026-08-09 with the above, from the frozen 28_800 (8 h). The total
/// is cells times per-cell price, so raising the per-cell price without raising
/// the total would have failed the run at the ceiling instead of at the probe.
/// At 7.0 the model reads 5,376 s of kernel coarse and 950 s of family-1
/// coarse. The refinement caps are totals shared by both rounds, not per-round
/// allowances: kernel refinement is therefore 3 x 600 x 7 = 12,600 s and
/// family-1 refinement is 40 x 50 = 2,000 s. The complete serial ceiling model
/// is about 20,926 s.
///
/// The earlier comment double-counted refinement by spending each family's cap
/// in both rounds, contrary to the driver's `cap - used` accounting. The
/// 39,600 s ceiling remains deliberately unchanged: correcting an overestimate
/// does not require tightening a frozen ceiling during the optimization round.
/// The refinement product is a finer loss ordering over cells Stage B then
/// truncates to 24 per family, so whether it should run at all remains a
/// separate question.
pub const STAGE_A_BUDGET_S: f64 = 72_000.0;
pub const STAGE_A_RSS_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const NS_PER_MINUTE: u64 = 60_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    EventMarkov,
    WallMmpp,
    LogOuCox,
    SelfExciting,
    ShotNoise,
}

impl Family {
    pub const ALL: [Self; 5] = [
        Self::EventMarkov,
        Self::WallMmpp,
        Self::LogOuCox,
        Self::SelfExciting,
        Self::ShotNoise,
    ];

    /// The SEAM spelling, which is also the artifact's. `Debug` lowercased
    /// would read `eventmarkov` and could not be pasted into a preset's
    /// `[instrument.generator.arrival]` table, so every artifact key and the
    /// verdict string go through this.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EventMarkov => "event_markov",
            Self::WallMmpp => "wall_mmpp",
            Self::LogOuCox => "log_ou_cox",
            Self::SelfExciting => "self_exciting",
            Self::ShotNoise => "shot_noise",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "family", rename_all = "snake_case")]
pub enum Cell {
    EventMarkov {
        switch_rate: f64,
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
    ShotNoise {
        m: f64,
        k: f64,
        tau_s: f64,
    },
}

impl Cell {
    #[must_use]
    pub const fn family(&self) -> Family {
        match self {
            Self::EventMarkov { .. } => Family::EventMarkov,
            Self::WallMmpp { .. } => Family::WallMmpp,
            Self::LogOuCox { .. } => Family::LogOuCox,
            Self::SelfExciting { .. } => Family::SelfExciting,
            Self::ShotNoise { .. } => Family::ShotNoise,
        }
    }

    #[must_use]
    pub const fn config(&self) -> ArrivalConfig {
        match *self {
            Self::EventMarkov { switch_rate } => ArrivalConfig::EventMarkov {
                quiet_share: 0.35,
                switch_rate,
                rate_ratio: 150.0,
            },
            Self::WallMmpp {
                occupancy,
                rate_ratio,
                tau_s,
            } => ArrivalConfig::WallMmpp {
                occupancy,
                rate_ratio,
                tau_s,
            },
            Self::LogOuCox { sigma_y, tau_s } => ArrivalConfig::LogOuCox { sigma_y, tau_s },
            Self::SelfExciting { phi, tau_s } => ArrivalConfig::SelfExciting { phi, tau_s },
            Self::ShotNoise { m, k, tau_s } => ArrivalConfig::ShotNoise { m, k, tau_s },
        }
    }

    #[must_use]
    pub const fn fitted_params(&self) -> u8 {
        match self {
            Self::EventMarkov { .. } => 1,
            Self::WallMmpp { .. } | Self::ShotNoise { .. } => 3,
            Self::LogOuCox { .. } | Self::SelfExciting { .. } => 2,
        }
    }

    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::EventMarkov { switch_rate } => {
                format!("event_markov:switch_rate={switch_rate:.17}")
            }
            Self::WallMmpp {
                occupancy,
                rate_ratio,
                tau_s,
            } => format!(
                "wall_mmpp:occupancy={occupancy:.17}:rate_ratio={rate_ratio:.17}:tau_s={tau_s:.17}"
            ),
            Self::LogOuCox { sigma_y, tau_s } => {
                format!("log_ou_cox:sigma_y={sigma_y:.17}:tau_s={tau_s:.17}")
            }
            Self::SelfExciting { phi, tau_s } => {
                format!("self_exciting:phi={phi:.17}:tau_s={tau_s:.17}")
            }
            Self::ShotNoise { m, k, tau_s } => {
                format!("shot_noise:m={m:.17}:k={k:.17}:tau_s={tau_s:.17}")
            }
        }
    }
}

#[must_use]
pub fn coarse_grid(family: Family) -> Vec<Cell> {
    match family {
        Family::EventMarkov => log_grid(1e-6, 0.5, 3)
            .into_iter()
            .map(|switch_rate| Cell::EventMarkov { switch_rate })
            .collect(),
        Family::WallMmpp => linear_grid(0.1, 0.6, 0.1)
            .into_iter()
            .flat_map(|occupancy| {
                log_grid(2.0, 200.0, 3)
                    .into_iter()
                    .flat_map(move |rate_ratio| {
                        log_grid(1.0, 3600.0, 3)
                            .into_iter()
                            .map(move |tau_s| Cell::WallMmpp {
                                occupancy,
                                rate_ratio,
                                tau_s,
                            })
                    })
            })
            .collect(),
        Family::LogOuCox => linear_grid(0.2, 2.0, 0.2)
            .into_iter()
            .flat_map(|sigma_y| {
                log_grid(1.0, 3600.0, 3)
                    .into_iter()
                    .map(move |tau_s| Cell::LogOuCox { sigma_y, tau_s })
            })
            .collect(),
        Family::SelfExciting => vec![
            0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70, 0.75,
            0.80, 0.85, 0.90, 0.94, 0.98,
        ]
        .into_iter()
        .flat_map(|phi| {
            log_grid(2.0, 600.0, 3)
                .into_iter()
                .map(move |tau_s| Cell::SelfExciting { phi, tau_s })
        })
        .collect(),
        Family::ShotNoise => linear_grid(0.2, 0.8, 0.1)
            .into_iter()
            .flat_map(|m| {
                log_grid(0.1, 10.0, 3).into_iter().flat_map(move |k| {
                    log_grid(1.0, 3600.0, 3)
                        .into_iter()
                        .map(move |tau_s| Cell::ShotNoise { m, k, tau_s })
                })
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenRefusal {
    pub variant: String,
    pub clock_ns: u64,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<Family>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_params: Option<Cell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedWalk {
    pub seed: u64,
    pub projection: ScreenReduced,
    #[cfg(test)]
    pub sessions: Vec<Value>,
    pub parents: u64,
    /// Child prints pushed into the accumulator - the screen's work-size
    /// reading, not an input to any verdict.
    ///
    /// `serde(default)` because it postdates the cache format: a walk cached
    /// before the field existed reads back as zero rather than refusing. That
    /// is the right failure for a WORK-SIZE counter, whose consumer is a
    /// benchmark comparison, and it would be the wrong one for anything the
    /// A1-A4 verdicts read - so nothing reads it.
    #[serde(default)]
    pub prints: u64,
    /// The realized mean parent gap, or `None` when the walk measured fewer
    /// than two parents or refused before it could.
    ///
    /// `Option` rather than a NaN sentinel, and the distinction is not
    /// cosmetic: NaN serializes to JSON `null` and will NOT deserialize back
    /// into an `f64`, so a cached refused walk was unreadable and took the
    /// whole run down with `invalid type: null, expected f64`. A sentinel that
    /// cannot survive its own cache round trip is worse than no sentinel.
    /// Since the 2026-08-11 amendment retired A4's mean-gap limb this value
    /// gates nothing and is reported only.
    pub realized_mean_gap_s: Option<f64>,
    pub refusal: Option<ScreenRefusal>,
    pub cost_s: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CellVerdict {
    pub cell: Cell,
    pub fitted_params: u8,
    pub a1: Value,
    pub a2: Value,
    pub a3: Value,
    pub a4: Value,
    pub admissible: bool,
    pub loss: Option<f64>,
    pub reported: Value,
    pub refusals: Vec<Value>,
    pub cost_s: f64,
}

/// A parameter point's exact position on the depth-2 refinement lattice.
/// Coarse coordinates are multiples of four, round-1 coordinates are even,
/// and round-2 coordinates may be odd.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatticeCell {
    pub cell: Cell,
    pub lattice: Vec<u32>,
    pub level: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvaluatedCell {
    #[serde(flatten)]
    pub verdict: CellVerdict,
    pub lattice: Vec<u32>,
    pub level: u8,
    pub pass: &'static str,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct RefinementRound {
    pub candidates: Vec<LatticeCell>,
    pub unevaluated: usize,
}

pub(crate) const LATTICE_SCALE: u32 = 1 << REFINEMENT_DEPTH;

pub(crate) fn axis_grids(family: Family) -> Vec<(Vec<f64>, bool)> {
    match family {
        Family::EventMarkov => vec![(log_grid(1e-6, 0.5, 3), true)],
        Family::WallMmpp => vec![
            (linear_grid(0.1, 0.6, 0.1), false),
            (log_grid(2.0, 200.0, 3), true),
            (log_grid(1.0, 3600.0, 3), true),
        ],
        Family::LogOuCox => vec![
            (linear_grid(0.2, 2.0, 0.2), false),
            (log_grid(1.0, 3600.0, 3), true),
        ],
        Family::SelfExciting => vec![
            (
                vec![
                    0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.45, 0.50, 0.55, 0.60, 0.65, 0.70,
                    0.75, 0.80, 0.85, 0.90, 0.94, 0.98,
                ],
                false,
            ),
            (log_grid(2.0, 600.0, 3), true),
        ],
        Family::ShotNoise => vec![
            (linear_grid(0.2, 0.8, 0.1), false),
            (log_grid(0.1, 10.0, 3), true),
            (log_grid(1.0, 3600.0, 3), true),
        ],
    }
}

pub(crate) fn cell_from_coordinates(family: Family, coordinates: &[u32]) -> Cell {
    let axes = axis_grids(family);
    let value = |axis: usize| {
        let index = coordinates[axis];
        let coarse = (index / LATTICE_SCALE) as usize;
        let remainder = index % LATTICE_SCALE;
        if remainder == 0 {
            return axes[axis].0[coarse];
        }
        let fraction = f64::from(remainder) / f64::from(LATTICE_SCALE);
        let lo = axes[axis].0[coarse];
        let hi = axes[axis].0[coarse + 1];
        if axes[axis].1 {
            lo * (hi / lo).powf(fraction)
        } else {
            (hi - lo).mul_add(fraction, lo)
        }
    };
    match family {
        Family::EventMarkov => Cell::EventMarkov {
            switch_rate: value(0),
        },
        Family::WallMmpp => Cell::WallMmpp {
            occupancy: value(0),
            rate_ratio: value(1),
            tau_s: value(2),
        },
        Family::LogOuCox => Cell::LogOuCox {
            sigma_y: value(0),
            tau_s: value(1),
        },
        Family::SelfExciting => Cell::SelfExciting {
            phi: value(0),
            tau_s: value(1),
        },
        Family::ShotNoise => Cell::ShotNoise {
            m: value(0),
            k: value(1),
            tau_s: value(2),
        },
    }
}

/// The full coarse tensor annotated with exact lattice coordinates.
#[must_use]
pub fn coarse_lattice(family: Family) -> Vec<LatticeCell> {
    fn product(axes: &[usize], at: usize, row: &mut Vec<u32>, out: &mut Vec<Vec<u32>>) {
        if at == axes.len() {
            out.push(row.clone());
            return;
        }
        for i in 0..axes[at] {
            row.push(u32::try_from(i).expect("grid index fits") * LATTICE_SCALE);
            product(axes, at + 1, row, out);
            row.pop();
        }
    }
    let lengths: Vec<_> = axis_grids(family).iter().map(|(v, _)| v.len()).collect();
    let mut coordinates = Vec::new();
    product(&lengths, 0, &mut Vec::new(), &mut coordinates);
    coordinates
        .into_iter()
        .map(|lattice| LatticeCell {
            cell: cell_from_coordinates(family, &lattice),
            lattice,
            level: 0,
        })
        .collect()
}

/// The cell brick A0 prices: the coarse point nearest the family's domain
/// centre, taken per axis rather than by indexing the flattened tensor.
///
/// The flattened middle is NOT the domain centre - for `WallMmpp` it is the
/// occupancy midpoint crossed with the FIRST rate-ratio and tau point, a
/// corner of the grid whose price says nothing about the tensor's interior.
/// Per axis, this reproduces brick K's `wall_mmpp` transcript point exactly
/// and lands within one grid step of the other two.
#[must_use]
pub fn probe_cell(family: Family) -> Cell {
    let coordinates: Vec<u32> = axis_grids(family)
        .iter()
        .map(|(points, _)| {
            u32::try_from(points.len() / 2).expect("grid index fits") * LATTICE_SCALE
        })
        .collect();
    cell_from_coordinates(family, &coordinates)
}

fn neighbours(cells: &[EvaluatedCell], index: usize) -> Vec<(usize, usize)> {
    let here = &cells[index].lattice;
    let mut out = Vec::new();
    for axis in 0..here.len() {
        for direction in [-1_i64, 1] {
            let mut best: Option<(u32, usize)> = None;
            for (other_index, other) in cells.iter().enumerate() {
                if other_index == index
                    || other.lattice.len() != here.len()
                    || other
                        .lattice
                        .iter()
                        .enumerate()
                        .any(|(a, &v)| a != axis && v != here[a])
                {
                    continue;
                }
                let delta = i64::from(other.lattice[axis]) - i64::from(here[axis]);
                if delta.signum() == direction {
                    let distance = delta.unsigned_abs() as u32;
                    if best.is_none_or(|(old, _)| distance < old) {
                        best = Some((distance, other_index));
                    }
                }
            }
            if let Some((_, neighbour)) = best {
                out.push((axis, neighbour));
            }
        }
    }
    out
}

/// Propose one sequential refinement round from all cells evaluated so far.
/// Candidate identity and ordering use integer lattice coordinates only.
#[must_use]
pub fn refinement_round(
    family: Family,
    evaluated: &[EvaluatedCell],
    round: u8,
    remaining: usize,
) -> RefinementRound {
    let known: BTreeSet<Vec<u32>> = evaluated.iter().map(|c| c.lattice.clone()).collect();
    let mut proposals: BTreeMap<Vec<u32>, f64> = BTreeMap::new();
    for (index, parent) in evaluated
        .iter()
        .enumerate()
        .filter(|(_, c)| c.verdict.admissible)
    {
        for (axis, other_index) in neighbours(evaluated, index) {
            let other = &evaluated[other_index];
            if other.verdict.admissible {
                continue;
            }
            let mut midpoint = parent.lattice.clone();
            let sum = parent.lattice[axis] + other.lattice[axis];
            if !sum.is_multiple_of(2) {
                continue;
            }
            midpoint[axis] = sum / 2;
            if known.contains(&midpoint) || midpoint == parent.lattice || midpoint == other.lattice
            {
                continue;
            }
            let loss = parent.verdict.loss.unwrap_or(f64::INFINITY);
            proposals
                .entry(midpoint)
                .and_modify(|old| *old = old.min(loss))
                .or_insert(loss);
        }
    }
    let proposed = proposals.len();
    let mut ranked: Vec<_> = proposals.into_iter().collect();
    ranked.sort_by(|(a, al), (b, bl)| al.total_cmp(bl).then_with(|| a.cmp(b)));
    ranked.truncate(remaining);
    RefinementRound {
        unevaluated: proposed.saturating_sub(ranked.len()),
        candidates: ranked
            .into_iter()
            .map(|(lattice, _)| LatticeCell {
                cell: cell_from_coordinates(family, &lattice),
                lattice,
                level: round,
            })
            .collect(),
    }
}

/// Connected admissible components under the same irregular-grid adjacency
/// used for boundary detection.
#[must_use]
pub fn admissible_regions(cells: &[EvaluatedCell]) -> Vec<Vec<usize>> {
    let mut unseen: BTreeSet<usize> = cells
        .iter()
        .enumerate()
        .filter(|(_, c)| c.verdict.admissible)
        .map(|(i, _)| i)
        .collect();
    let mut regions = Vec::new();
    while let Some(&start) = unseen.first() {
        unseen.remove(&start);
        let mut queue = VecDeque::from([start]);
        let mut region = Vec::new();
        while let Some(index) = queue.pop_front() {
            region.push(index);
            for (_, next) in neighbours(cells, index) {
                if cells[next].verdict.admissible && unseen.remove(&next) {
                    queue.push_back(next);
                }
            }
        }
        region.sort_unstable();
        regions.push(region);
    }
    regions
}

pub struct ScreenContext {
    pub profile: InstrumentProfile,
    pub binding: GeneratedBinding,
    observed_projection: ScreenReduced,
    #[cfg(test)]
    observed: ObsContext,
    observed_marginal: CountMarginal,
    /// The observed sides of A1(a), A2 and A3, resolved ONCE. They depend on
    /// the committed 12a artifact alone, so recomputing them per cell would
    /// re-walk 22 session records for every one of roughly 1,400 cells and
    /// bill it to `STAGE_A_CELL_BUDGET_S`.
    #[cfg(test)]
    observed_shares: HashMap<i64, HashMap<String, f64>>,
    #[cfg(test)]
    observed_rates: BTreeMap<i64, HourRate>,
    #[cfg(test)]
    observed_zero: BTreeMap<i64, Option<f64>>,
    hours: Vec<i64>,
    cache: CacheStore,
    bypass_cache: bool,
    envelope_grid: OnceLock<ExposureGrid>,
}

/// The immutable, thread-safe subset of [`ScreenContext`] needed to project
/// one `(cell, seed)` task. The observed-side context deliberately stays on
/// the coordinator because its lazy metric store is not `Send`.
#[derive(Clone)]
pub struct ProjectionContext {
    profile: InstrumentProfile,
    binding: GeneratedBinding,
    cache: CacheStore,
    bypass_cache: bool,
}

impl ScreenContext {
    pub fn open(measure_path: &Path, cache: Option<&Path>) -> LabResult<Self> {
        // ATTRIBUTED HERE RATHER THAN AROUND THE WHOLE CALL. A caller that
        // wrapped `open` as "opening the 12a measurement" would put that
        // sentence on every failure below it too - a broken MNQ preset, a
        // missing `analysis/fingerprint.json`, an unparseable binding - which
        // is the unattributed error the naming was meant to remove, relocated
        // rather than fixed. These two lines are the ones that really are
        // about the measurement path, so they are the ones that name it.
        let measure_bytes = std::fs::read(measure_path).map_err(|e| {
            LabError::refusal(format!(
                "reading the 12a measurement {}: {e}",
                measure_path.display()
            ))
        })?;
        let measure: Value = serde_json::from_slice(&measure_bytes).map_err(|e| {
            LabError::refusal(format!(
                "parsing the 12a measurement {}: {e}",
                measure_path.display()
            ))
        })?;
        let binding = GeneratedBinding::from_measure12a(&measure)?;
        let sessions = measure["observed"]["per_session"]
            .as_array()
            .ok_or_else(|| LabError::refusal("observed.per_session is missing"))?
            .clone();
        let hist = &measure["observed"]["monthly"]["block1"]["hist"];
        let observed_marginal = parent_count_marginal(hist)?;
        let observed_projection = ScreenReduced::from_sessions(&sessions)?;
        let profile = mogwai_venue::config::profile_from_preset("MNQ")
            .map_err(|e| LabError::refusal(e.to_string()))?;
        let hours = gate_hours(&profile)?;
        let fingerprint_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../analysis/fingerprint.json");
        let fingerprint_hash = crate::ledger::sha256_file(&fingerprint_path)?;
        let measure_hash = crate::ledger::sha256_bytes(&measure_bytes);
        // The burn-in prefix keeps its frozen `warmup` spelling here: this
        // string is a provenance key over stored screen state, so respelling
        // it would orphan every entry already written under it.
        let command = format!(
            "arrival-screen:kernel-version={}:start={}:length={}:warmup={}",
            mogwai_data::ARRIVAL_KERNEL_VERSION,
            binding.window_start_ns,
            binding.window_length_ns,
            binding.burn_in
        );
        let token = ProvenanceToken::compute(&ProvenanceInputs {
            crate_version: env!("CARGO_PKG_VERSION"),
            tape_protocol_version: mogwai_data::TAPE_PROTOCOL_VERSION,
            fingerprint_hash: &fingerprint_hash,
            full_command: &command,
            subcontract_hash: &measure_hash,
        });
        #[cfg(test)]
        let observed = ObsContext::new(sessions);
        #[cfg(test)]
        let observed_shares = obs_shares_under(&observed, &observed.ones());
        #[cfg(test)]
        let observed_rates = hourly_mean_parents(&observed);
        #[cfg(test)]
        let observed_zero = hourly_zero_second_fraction(&observed);
        Ok(Self {
            profile,
            binding,
            observed_projection,
            #[cfg(test)]
            observed,
            observed_marginal,
            #[cfg(test)]
            observed_shares,
            #[cfg(test)]
            observed_rates,
            #[cfg(test)]
            observed_zero,
            hours,
            cache: CacheStore::open(cache_root(cache), token),
            bypass_cache: false,
            envelope_grid: OnceLock::new(),
        })
    }

    /// Brick A0 measures the PRICE of a cell, so it may neither read nor write
    /// the walk cache: a warm cache would report the cost of a JSON read
    /// against `STAGE_A_CELL_BUDGET_S` and pass every family by construction.
    #[must_use]
    pub const fn measured(mut self) -> Self {
        self.bypass_cache = true;
        self
    }

    /// A context over an OBSERVED SIDE GIVEN DIRECTLY, for the condition tests.
    ///
    /// It resolves the same MNQ profile, the same gate hour set and the same
    /// three observed projections `open` resolves; only the source of the
    /// session records differs, and the exposure binding is a placeholder
    /// because nothing reachable from a `verdict_from_walks` test walks a tape.
    #[cfg(test)]
    fn over(sessions: Vec<Value>) -> LabResult<Self> {
        let profile = mogwai_venue::config::profile_from_preset("MNQ")
            .map_err(|e| LabError::refusal(e.to_string()))?;
        let hours = gate_hours(&profile)?;
        let mut hist = Vec::new();
        for s in &sessions {
            hist.extend(s["block1_hist"].as_array().into_iter().flatten().cloned());
        }
        let observed_marginal = parent_count_marginal(&Value::Array(hist))?;
        let observed_projection = ScreenReduced::from_sessions(&sessions)?;
        let observed = ObsContext::new(sessions);
        let observed_shares = obs_shares_under(&observed, &observed.ones());
        let observed_rates = hourly_mean_parents(&observed);
        let observed_zero = hourly_zero_second_fraction(&observed);
        Ok(Self {
            profile,
            binding: GeneratedBinding {
                window_start_ns: 0,
                window_length_ns: 0,
                burn_in: "0s".into(),
            },
            observed_projection,
            observed,
            observed_marginal,
            observed_shares,
            observed_rates,
            observed_zero,
            hours,
            cache: CacheStore::open(
                PathBuf::from("target/stage-a-unused-cache"),
                ProvenanceToken::compute(&ProvenanceInputs {
                    crate_version: env!("CARGO_PKG_VERSION"),
                    tape_protocol_version: mogwai_data::TAPE_PROTOCOL_VERSION,
                    fingerprint_hash: "",
                    full_command: "arrival-screen:test",
                    subcontract_hash: "",
                }),
            ),
            bypass_cache: true,
            envelope_grid: OnceLock::new(),
        })
    }

    #[must_use]
    pub fn observed_marginal(&self) -> &CountMarginal {
        &self.observed_marginal
    }
    #[must_use]
    pub fn gate_hours(&self) -> &[i64] {
        &self.hours
    }

    /// Builds the worker-safe projection state and prepares its cache once.
    /// Every scheduler pass in one run reuses the returned value.
    pub fn parallel_projection(&self) -> LabResult<ProjectionContext> {
        if !self.bypass_cache {
            self.cache.prepare_for_writes()?;
        }
        Ok(ProjectionContext {
            profile: self.profile.clone(),
            binding: self.binding.clone(),
            cache: self.cache.clone(),
            bypass_cache: self.bypass_cache,
        })
    }

    pub fn envelope_grid(&self) -> LabResult<&ExposureGrid> {
        if let Some(grid) = self.envelope_grid.get() {
            return Ok(grid);
        }
        let grid = ExposureGrid::new(
            &self.profile,
            self.binding.window_start_ns,
            self.binding.window_length_ns,
            CADENCE_STEP_NS,
        )?;
        drop(self.envelope_grid.set(grid));
        self.envelope_grid
            .get()
            .ok_or_else(|| LabError::refusal("the envelope grid failed to initialize"))
    }
}

fn cache_key(cell: &Cell, seed: u64) -> String {
    crate::ledger::sha256_bytes(format!("cell={}\nseed={seed}\n", cell.key()).as_bytes()) + ".json"
}

/// The one thing the projection of spec 3.3 needs from a walk: the next
/// parent, or the refusal that ends the cell.
///
/// A TRAIT RATHER THAN THE CONCRETE `ParentWalk` because the projection is the
/// part of Stage A that has to be pinned by test - the straddling-burst,
/// session-rotation, stalled-walk and mean-gap properties of spec section 7 are
/// statements about the PROJECTION, not about any arrival family, and driving
/// them through a real month-scale generator walk would price each of them in
/// minutes. It is a `pub(crate)` seam with no command-line surface: nothing
/// outside this module can supply a source, and the shipped driver constructs
/// exactly one implementation.
trait ParentSource {
    fn next(&mut self) -> Result<ParentSummary, ScreenRefusal>;
}

/// The clock is A4 evidence: a refusal without it says the walk stopped but not
/// where, which is exactly what an owner ruling needs.
fn screen_refusal_from_arrival(refusal: ArrivalRefusal) -> ScreenRefusal {
    ScreenRefusal {
        variant: match refusal {
            ArrivalRefusal::NoOpenExposure { .. } => "no_open_exposure",
            ArrivalRefusal::IntensityCeiling { .. } => "intensity_ceiling",
            ArrivalRefusal::NonFiniteState { .. } => "non_finite_state",
        }
        .to_string(),
        clock_ns: match refusal {
            ArrivalRefusal::NoOpenExposure { from_ns } => from_ns,
            ArrivalRefusal::IntensityCeiling { clock_ns, .. }
            | ArrivalRefusal::NonFiniteState { clock_ns } => clock_ns,
        },
        detail: format!("{refusal:?}"),
        family: None,
        canonical_params: None,
        seed: None,
    }
}

enum ParentWalk {
    Generator(Box<GeneratedSource>),
    Kernel(Box<CadenceWalk>),
}
impl ParentSource for ParentWalk {
    fn next(&mut self) -> Result<ParentSummary, ScreenRefusal> {
        match self {
            // BOTH ARMS REPORT A REFUSAL. The generator arm used to be
            // infallible, so a refused kernel draw reached the projection as a
            // phantom parent - the previous timestamp with zero children - and
            // the only thing that ended the walk was the stall guard, which
            // names the wrong cause. `advance_parent` now returns the fault.
            Self::Generator(source) => source.advance_parent().map_err(|fault| match fault {
                TickFault::Arrival(refusal) => screen_refusal_from_arrival(refusal),
                // UNREACHABLE HERE BY CONSTRUCTION rather than by assumption:
                // an injected fault comes from the venue's control plane, and
                // the screen drives a `GeneratedSource` directly with no venue
                // and no control plane in the process. Named rather than
                // wildcarded so a future fault variant a source CAN produce
                // still breaks this build.
                TickFault::Injected => ScreenRefusal {
                    variant: "injected".to_string(),
                    clock_ns: 0,
                    detail: "an operator-injected venue fault reached the offline screen, which \
                             drives a generator directly and has no control plane to inject one"
                        .to_string(),
                    family: None,
                    canonical_params: None,
                    seed: None,
                },
            }),
            Self::Kernel(walk) => {
                let stride = walk.child_stride_ns();
                walk.next()
                    .map(|draw| ParentSummary {
                        parent_ts_ns: draw.parent_ts_ns,
                        child_count: draw.child_count,
                        child_stride_ns: stride,
                    })
                    .map_err(screen_refusal_from_arrival)
            }
        }
    }
}

/// A projection either finishes, refuses the CELL, or hits a defect in the
/// screen itself. Only the last aborts the run: a refused cell is a recorded
/// A4 verdict, per spec 3.3 step 4e, not a reason to abandon the grid.
#[derive(Debug)]
enum ProjectStop {
    Lab(LabError),
    Refused(ScreenRefusal),
}
impl From<LabError> for ProjectStop {
    fn from(e: LabError) -> Self {
        Self::Lab(e)
    }
}
impl From<serde_json::Error> for ProjectStop {
    fn from(e: serde_json::Error) -> Self {
        Self::Lab(e.into())
    }
}

/// The open-parent tuple `(first_ts, segment_index, session_start_ns)` for a
/// measured parent. The segment index is `GeneratedAcc`'s own, transcribed
/// rather than re-derived from the segment's NAME: the accumulator asks whether
/// this segment opened the session.
fn open_parent_at(parent_ts_ns: u64, offset: i32) -> Result<(u64, u8, u64), ProjectStop> {
    let seg = session_segment_at(parent_ts_ns, offset)
        .ok_or_else(|| projection_defect("a measured parent maps to no open segment"))?;
    Ok((
        parent_ts_ns,
        u8::from(seg.segment_origin_ns != seg.session_start_ns),
        seg.session_start_ns,
    ))
}

fn projection_refusal(clock_ns: u64, detail: &str) -> ProjectStop {
    ProjectStop::Refused(ScreenRefusal {
        variant: "projected_child_inside_closed_halt_segment".into(),
        clock_ns,
        detail: detail.into(),
        family: None,
        canonical_params: None,
        seed: None,
    })
}

fn projection_defect(detail: impl Into<String>) -> ProjectStop {
    ProjectStop::Lab(LabError::refusal(detail.into()))
}

struct Projected {
    projection: ScreenReduced,
    #[cfg(test)]
    sessions: Vec<Value>,
    parents: u64,
    prints: u64,
    realized_mean_gap_s: Option<f64>,
}

/// The screen's cumulative work size for one process, in the two units a
/// benchmark comparison needs: how many parameter cells were evaluated, and
/// how much walking that cost in parents and child prints.
///
/// Kept here rather than derived by the caller because the only place the
/// numbers exist is inside the projection, and the only place they can be
/// totalled correctly is where a CACHED seed is served too - a cache hit did
/// less work this run, but the run still stands for that much work, and a
/// comparison whose counters swung on cache state would answer nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScreenWork {
    pub cells_evaluated: u64,
    pub parents: u64,
    pub prints: u64,
}

/// One uncached cell evaluation plus the work represented by its seed walks.
/// Batch instruments use the local counters instead of differencing the
/// process totals, which remains correct when cells run concurrently.
pub struct CellEvaluation {
    pub verdict: CellVerdict,
    pub parents: u64,
    pub prints: u64,
    pub demand: CellEnvelopeDemand,
}

/// Whether a gate can decide at its base, needs the predictive envelope, or
/// can decide at its materiality cap. This is the single lazy-envelope
/// predicate used by both the screen and the demand census.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvelopeDemand {
    InsideBase,
    MarginalShell,
    OverCap,
}

impl EnvelopeDemand {
    #[must_use]
    pub const fn needs_envelope(self) -> bool {
        matches!(self, Self::MarginalShell)
    }

    /// The gate fails on its own, with no envelope able to rescue it: an
    /// allowance only widens a band toward its cap, and this state is already
    /// past the cap.
    #[must_use]
    pub const fn over_cap(self) -> bool {
        matches!(self, Self::OverCap)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CellEnvelopeDemand {
    pub family: Family,
    pub a2: EnvelopeDemand,
    pub a3: EnvelopeDemand,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct EnvelopeDemandCounts {
    pub a2: usize,
    pub a3: usize,
    pub either: usize,
}

#[must_use]
pub fn marginal_envelope_counts(demands: &[CellEnvelopeDemand]) -> EnvelopeDemandCounts {
    EnvelopeDemandCounts {
        a2: demands.iter().filter(|d| d.a2.needs_envelope()).count(),
        a3: demands.iter().filter(|d| d.a3.needs_envelope()).count(),
        either: demands
            .iter()
            .filter(|d| d.a2.needs_envelope() || d.a3.needs_envelope())
            .count(),
    }
}

/// Classifies the complete gated-hour deviation set. Strict comparisons are
/// load-bearing: a reading exactly at base is inside, and one exactly at cap
/// remains in the marginal shell.
#[must_use]
pub fn classify_envelope_demand(deviations: &[(i64, f64)], base: f64, cap: f64) -> EnvelopeDemand {
    if deviations.iter().any(|(_, deviation)| *deviation > cap) {
        EnvelopeDemand::OverCap
    } else if deviations.iter().any(|(_, deviation)| *deviation > base) {
        EnvelopeDemand::MarginalShell
    } else {
        EnvelopeDemand::InsideBase
    }
}

/// Projection-only work measured by the Stage A batch pilot.
pub struct WalkMeasurement {
    pub cost_s: f64,
    pub parents: u64,
    pub prints: u64,
}

/// One scheduler cell and its exact ordered seed set.
#[derive(Debug, Clone)]
pub struct ScheduledCell {
    pub cell: Cell,
    pub seeds: Vec<u64>,
}

fn finite_log_deviation(numerator: f64, denominator: f64) -> f64 {
    if numerator <= 0.0 || denominator <= 0.0 {
        f64::INFINITY
    } else {
        (numerator / denominator).ln().abs()
    }
}

struct GateDeviations {
    shape: Vec<(i64, f64)>,
    zero: Vec<(i64, f64)>,
}

fn rate_and_zero_deviations(ctx: &ScreenContext, walks: &[SeedWalk]) -> GateDeviations {
    let observed = &ctx.observed_projection;
    let mut shape = Vec::new();
    let mut zero = Vec::new();
    for &hour in &ctx.hours {
        let generated_rates: Option<Vec<_>> = walks
            .iter()
            .map(|walk| projection_mean_parents(&walk.projection, hour))
            .collect();
        let generated_rate_mean =
            generated_rates.map_or(0.0, |rates| rates.iter().sum::<f64>() / rates.len() as f64);
        let observed_rate = projection_mean_parents(observed, hour).unwrap_or(0.0);
        shape.push((
            hour,
            finite_log_deviation(generated_rate_mean, observed_rate),
        ));

        if A3_GATED_HOURS.contains(&hour) {
            let generated_zero_mean = walks
                .iter()
                .map(|walk| projection_zero_fraction(&walk.projection, hour).unwrap_or(0.0))
                .sum::<f64>()
                / walks.len().max(1) as f64;
            let observed_zero = projection_zero_fraction(observed, hour).unwrap_or(0.0);
            zero.push((
                hour,
                finite_log_deviation(generated_zero_mean, observed_zero),
            ));
        }
    }
    GateDeviations { shape, zero }
}

fn envelope_demand_from_walks(
    ctx: &ScreenContext,
    family: Family,
    walks: &[SeedWalk],
) -> CellEnvelopeDemand {
    let deviations = rate_and_zero_deviations(ctx, walks);
    CellEnvelopeDemand {
        family,
        a2: classify_envelope_demand(&deviations.shape, A2_SHAPE_BASE, A2_SHAPE_CAP),
        a3: classify_envelope_demand(&deviations.zero, A3_BASE, A3_CAP),
    }
}

/// A2's level limb, per spec 9.2: total PARENTS over total SCHEDULED EXPOSURE,
/// each side using its own exposure.
///
/// Both halves were wrong before and the pair of errors is worth stating,
/// because either alone would have been invisible. The numerator summed the
/// histogram OCCURRENCES - how many minutes carried each parent count - rather
/// than weighting each occurrence by its parent-count key, so it counted
/// populated minutes and called them parents. The denominator did not exist at
/// all: the ratio was raw total over raw total. Since the observed month
/// carries 22 usable sessions and the generated window 23 complete ones, the
/// gate returned exactly 23/22 for every mechanism at every parameter point -
/// session arithmetic wearing a rate's clothes, and a gate whose value cannot
/// move with the thing it grades.
fn level_parents_and_exposure(projection: &ScreenReduced) -> (f64, u64) {
    let parents = projection
        .parent_counts
        .values()
        .flat_map(|counts| counts.iter())
        .map(|(&n, &minutes)| f64::from(n) * minutes as f64)
        .sum();
    let exposure = projection
        .windows
        .values()
        .filter_map(|windows| windows.get(&60))
        .map(|window| window.scheduled)
        .sum();
    (parents, exposure)
}

fn amended_rate_and_zero_gates(
    ctx: &ScreenContext,
    cell: &Cell,
    walks: &[SeedWalk],
    evaluate_envelopes: bool,
    // `others_admit` is A1 and A4 together: whether the gates decided OUTSIDE
    // this function leave the cell admissible. False means no envelope here can
    // change the cell's fate, per the decision-relevant envelope amendment.
    others_admit: bool,
) -> LabResult<(Value, Value, bool, bool)> {
    let observed = &ctx.observed_projection;
    let (observed_parents, observed_exposure) = level_parents_and_exposure(observed);
    let observed_rate =
        (observed_exposure > 0).then(|| observed_parents / observed_exposure as f64);
    let mut level = Vec::with_capacity(walks.len());
    for walk in walks {
        let (generated_parents, generated_exposure) = level_parents_and_exposure(&walk.projection);
        let generated_rate =
            (generated_exposure > 0).then(|| generated_parents / generated_exposure as f64);
        let ratio = match (generated_rate, observed_rate) {
            (Some(generated), Some(obs)) if obs > 0.0 => generated / obs,
            _ => f64::INFINITY,
        };
        level.push(json!({
            "seed": walk.seed,
            "ratio": ratio.is_finite().then_some(ratio),
            // Recorded beside the ratio so the gate is auditable from the
            // artifact alone: the defect this replaced reported a bare 23/22
            // and nothing said where it came from.
            "generated_parents": generated_parents,
            "generated_scheduled_s": generated_exposure,
            "observed_parents": observed_parents,
            "observed_scheduled_s": observed_exposure,
            "passed": walk.refusal.is_none()
                && (MEAN_RATE_BAND.0..=MEAN_RATE_BAND.1).contains(&ratio)
        }));
    }
    let level_pass = !walks.is_empty() && level.iter().all(|row| row["passed"] == true);

    let mut shape_rows = Vec::new();
    let deviations = rate_and_zero_deviations(ctx, walks);
    let shape_deviations = deviations.shape;
    let a3_deviations = deviations.zero;
    let mut raw_rows = Vec::new();
    let mut gated_rows = Vec::new();
    let mut not_gated = Vec::new();
    for &hour in &ctx.hours {
        let raw: Vec<_> = walks
            .iter()
            .map(|walk| {
                let generated = projection_zero_fraction(&walk.projection, hour).unwrap_or(0.0);
                let observed = projection_zero_fraction(observed, hour).unwrap_or(0.0);
                let ratio = if observed > 0.0 {
                    generated / observed
                } else {
                    f64::INFINITY
                };
                json!({"seed":walk.seed,"hour":hour,
                    "ratio":ratio.is_finite().then_some(ratio)})
            })
            .collect();
        raw_rows.extend(raw.iter().cloned());
        if !A3_GATED_HOURS.contains(&hour) && (14..=16).contains(&hour) {
            not_gated.push(json!({"hour":hour,"raw_ratio_per_seed":raw}));
        }
    }

    let a2_class = classify_envelope_demand(&shape_deviations, A2_SHAPE_BASE, A2_SHAPE_CAP);
    let a3_class = classify_envelope_demand(&a3_deviations, A3_BASE, A3_CAP);
    let a2_needs_envelope = a2_class.needs_envelope();
    let a3_needs_envelope = a3_class.needs_envelope();
    // The 2026-08-11 decision-relevant envelope amendment: an envelope can only
    // widen a band toward its cap, so it can never rescue a cell that some
    // other hard gate already fails without one. Evaluating it anyway is pure
    // dead-cell work - the demand census priced it at 68 hours on the coarse
    // pass, every second of it computing an A2 allowance for a cell A3 had
    // already killed. The admissible set is identical either way; what is lost
    // is only the allowance figure on a cell that cannot be admitted, and the
    // skipped gate still records its raw deviations and classification.
    let cell_could_be_admissible =
        others_admit && level_pass && !a2_class.over_cap() && !a3_class.over_cap();
    let k = walks.len();
    let envelope_grid = ctx.envelope_grid()?;
    let envelope_outcome = if evaluate_envelopes
        && cell_could_be_admissible
        && (a2_needs_envelope || a3_needs_envelope)
    {
        if envelope_cost_s() >= STAGE_A_ENVELOPE_BUDGET_S {
            return Err(LabError::refusal(format!(
                "stage-a-envelope-budget-shortfall: spent {:.3}s of {:.3}s",
                envelope_cost_s(),
                STAGE_A_ENVELOPE_BUDGET_S
            )));
        }
        let started = Instant::now();
        let result =
            predictive_envelopes(cell, envelope_grid, k, a2_needs_envelope, a3_needs_envelope)?;
        ENVELOPE_COST_NS.fetch_add(
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
            Ordering::Relaxed,
        );
        if envelope_cost_s() > STAGE_A_ENVELOPE_BUDGET_S {
            return Err(LabError::refusal(format!(
                "stage-a-envelope-budget-shortfall: spent {:.3}s of {:.3}s",
                envelope_cost_s(),
                STAGE_A_ENVELOPE_BUDGET_S
            )));
        }
        result
    } else {
        EnvelopeOutcome::default()
    };
    let a2_allowance = envelope_outcome.a2;
    let a3_allowance = envelope_outcome.a3;
    // An infinite envelope (17 or more ceiling-breached replicates) collapses
    // to the gate's materiality cap here, because `min` with a non-finite
    // allowance yields the cap - the cell receives no allowance beyond it.
    let a2_threshold = A2_SHAPE_CAP.min(A2_SHAPE_BASE + a2_allowance.unwrap_or(0.0));
    for &(hour, deviation) in &shape_deviations {
        shape_rows.push(json!({
            "hour":hour,"deviation":deviation.is_finite().then_some(deviation),
            "base":A2_SHAPE_BASE,"allowance":a2_allowance,
            "cap":A2_SHAPE_CAP,"threshold":a2_threshold,"passed":deviation <= a2_threshold
        }));
    }
    let a3_threshold = A3_CAP.min(A3_BASE + a3_allowance.unwrap_or(0.0));
    for &(hour, deviation) in &a3_deviations {
        gated_rows.push(json!({
            "hour":hour,"deviation":deviation.is_finite().then_some(deviation),
            "base":A3_BASE,"allowance":a3_allowance,
            "cap":A3_CAP,"threshold":a3_threshold,"passed":deviation <= a3_threshold
        }));
    }
    let shape_pass = shape_rows.iter().all(|row| row["passed"] == true);
    let a3_pass = gated_rows.len() == A3_GATED_HOURS.len()
        && gated_rows.iter().all(|row| row["passed"] == true);
    let envelope_record = |needed: bool,
                           classification: EnvelopeDemand,
                           statistic: &str,
                           deviations: &[(i64, f64)],
                           value: Option<f64>| {
        let evaluated = needed && cell_could_be_admissible && evaluate_envelopes;
        let mut record = EnvelopeRecord::unevaluated(cell, envelope_grid, k);
        record.evaluated = evaluated;
        record.classification = classification;
        // The amendment requires a SKIPPED marginal gate to say why, beside
        // its raw deviations and classification, so the artifact still
        // shows exactly where the cell stood and that nothing was spent.
        record.skip_reason = (needed && !evaluated).then(|| {
            if evaluate_envelopes {
                "cell_inadmissible_without_envelope"
            } else {
                "envelope_evaluation_disabled"
            }
            .to_string()
        });
        record
            .ceiling_breached_replicates
            .clone_from(&envelope_outcome.ceiling_breached_replicates);
        record.deciding_statistic = evaluated.then(|| {
            let (hour, deviation) = deviations
                .iter()
                .max_by(|left, right| left.1.total_cmp(&right.1))
                .copied()
                .unwrap_or((0, f64::INFINITY));
            json!({"name":statistic,"hour":hour,
                "deviation":deviation.is_finite().then_some(deviation)})
        });
        record.order_statistic_value = value;
        record
    };
    let skipped_unresolved =
        |needed: bool| needed && !cell_could_be_admissible && evaluate_envelopes;
    // A resolved failure always wins over an unevaluated limb. In particular,
    // A2's failed level limb makes the whole gate failed even when its
    // marginal shape envelope was skipped.
    let a2_unresolved = level_pass && !a2_class.over_cap() && skipped_unresolved(a2_needs_envelope);
    let a3_unresolved = !a3_class.over_cap() && skipped_unresolved(a3_needs_envelope);
    let mut a2 = json!({
        "passed":level_pass && shape_pass,
        "level":{"per_seed":level},
        "shape":{"per_hour":shape_rows},
        "envelope":envelope_record(a2_needs_envelope,a2_class,"a2.shape.max_hourly_log_deviation",&shape_deviations,a2_allowance)
    });
    let mut a3 = json!({
        "passed":a3_pass,
        "gated":gated_rows,
        "not_gated":not_gated,
        "per_seed_raw":raw_rows,
        "envelope":envelope_record(a3_needs_envelope,a3_class,"a3.max_hourly_zero_fraction_log_deviation",&a3_deviations,a3_allowance)
    });
    for (gate, unresolved) in [(&mut a2, a2_unresolved), (&mut a3, a3_unresolved)] {
        if unresolved {
            gate["passed"] = Value::Null;
            gate["verdict"] = json!("unresolved");
        }
    }
    Ok((a2, a3, level_pass && shape_pass, a3_pass))
}

struct ScheduledWalk {
    walk: SeedWalk,
    execution_s: f64,
}

struct SeedTask {
    cell_index: usize,
    seed_index: usize,
}

struct SeedResult {
    task_index: usize,
    product: LabResult<ScheduledWalk>,
}

static CELLS_EVALUATED: AtomicU64 = AtomicU64::new(0);
static PARENTS_WALKED: AtomicU64 = AtomicU64::new(0);
static PRINTS_PROJECTED: AtomicU64 = AtomicU64::new(0);
static ENVELOPE_COST_NS: AtomicU64 = AtomicU64::new(0);

#[must_use]
pub fn envelope_cost_s() -> f64 {
    ENVELOPE_COST_NS.load(Ordering::Relaxed) as f64 / 1e9
}

/// This process's [`ScreenWork`] so far. `Relaxed` throughout: these are
/// observation-only totals read once at the end of a run, and nothing orders
/// anything against them.
#[must_use]
pub fn screen_work() -> ScreenWork {
    ScreenWork {
        cells_evaluated: CELLS_EVALUATED.load(Ordering::Relaxed),
        parents: PARENTS_WALKED.load(Ordering::Relaxed),
        prints: PRINTS_PROJECTED.load(Ordering::Relaxed),
    }
}

pub fn project_seed(ctx: &ScreenContext, cell: &Cell, seed: u64) -> LabResult<SeedWalk> {
    project_seed_under(
        &ctx.profile,
        &ctx.binding,
        &ctx.cache,
        ctx.bypass_cache,
        false,
        cell,
        seed,
    )
}

fn project_seed_parallel(ctx: &ProjectionContext, cell: &Cell, seed: u64) -> LabResult<SeedWalk> {
    project_seed_under(
        &ctx.profile,
        &ctx.binding,
        &ctx.cache,
        ctx.bypass_cache,
        true,
        cell,
        seed,
    )
}

fn project_seed_under(
    profile: &InstrumentProfile,
    binding: &GeneratedBinding,
    cache: &CacheStore,
    bypass_cache: bool,
    cache_prepared: bool,
    cell: &Cell,
    seed: u64,
) -> LabResult<SeedWalk> {
    let key = cache_key(cell, seed);
    if !bypass_cache && let Some(bytes) = cache.read(&key)? {
        let cached: SeedWalk = serde_json::from_slice(&bytes)?;
        tally_walk(&cached);
        return Ok(cached);
    }
    let started = Instant::now();
    let product = match project_walk(profile, binding, cell, seed) {
        Ok(done) => SeedWalk {
            seed,
            projection: done.projection,
            #[cfg(test)]
            sessions: done.sessions,
            parents: done.parents,
            prints: done.prints,
            realized_mean_gap_s: done.realized_mean_gap_s,
            refusal: None,
            cost_s: started.elapsed().as_secs_f64(),
        },
        // A refusal is cached like any other product: it is the cell's real
        // per-seed outcome, and re-walking it on the next pass would repay a
        // cost the budget already booked.
        Err(ProjectStop::Refused(mut refusal)) => {
            refusal.family = Some(cell.family());
            refusal.canonical_params = Some(cell.clone());
            refusal.seed = Some(seed);
            SeedWalk {
                seed,
                projection: ScreenReduced::default(),
                #[cfg(test)]
                sessions: Vec::new(),
                parents: 0,
                prints: 0,
                realized_mean_gap_s: None,
                refusal: Some(refusal),
                cost_s: started.elapsed().as_secs_f64(),
            }
        }
        Err(ProjectStop::Lab(e)) => return Err(e),
    };
    if !bypass_cache {
        let bytes = serde_json::to_vec(&product)?;
        if cache_prepared {
            cache.write_prepared(&key, &bytes)?;
        } else {
            cache.write(&key, &bytes)?;
        }
    }
    tally_walk(&product);
    Ok(product)
}

fn tally_walk(walk: &SeedWalk) {
    PARENTS_WALKED.fetch_add(walk.parents, Ordering::Relaxed);
    PRINTS_PROJECTED.fetch_add(walk.prints, Ordering::Relaxed);
}

fn project_walk(
    profile: &InstrumentProfile,
    binding: &GeneratedBinding,
    cell: &Cell,
    seed: u64,
) -> Result<Projected, ProjectStop> {
    let start = binding.window_start_ns;
    let end = start.saturating_add(binding.window_length_ns);
    let burn_in_ns = u64::try_from(parse_duration(&binding.burn_in).map_err(ProjectStop::Lab)?)
        .map_err(|_| LabError::refusal("burn-in duration is negative"))?;
    let walk_start = start
        .checked_sub(burn_in_ns)
        .ok_or_else(|| LabError::refusal("the burn-in underflows the window start"))?;
    let mut scalars = profile.scalars.clone();
    let config = cell.config();
    // A refinement midpoint that leaves the section 16 domain is a refusal,
    // not a silent walk at an out-of-domain parameterization.
    if !config.is_valid() {
        return Err(projection_defect(
            "the cell leaves the frozen section 16 parameter domain",
        ));
    }
    scalars.arrival = Some(config);
    let offset = i32::from(
        profile
            .calendar
            .as_ref()
            .ok_or_else(|| LabError::refusal("MNQ calendar missing"))?
            .utc_offset_minutes,
    );
    let mut walk = match cell.family() {
        Family::EventMarkov => ParentWalk::Generator(Box::new(
            GeneratedSource::try_new_with_session_profile(
                scalars,
                seed,
                walk_start,
                mogwai_venue::source::fingerprint(),
                &profile.session,
                None,
                SizeGrid::from_def(&profile.def),
                profile.calendar.clone(),
            )
            .map_err(|e| LabError::refusal(format!("building the generator: {e:?}")))?,
        )),
        _ => ParentWalk::Kernel(Box::new(
            CadenceWalk::new(
                &scalars,
                &profile.session,
                profile.calendar.as_ref(),
                1.0,
                seed,
                walk_start,
            )
            .ok_or_else(|| LabError::refusal("cell has no integrated kernel"))?,
        )),
    };
    project_stream(&mut walk, start, end, offset, seed)
}

#[derive(Debug, Clone)]
struct PopulatedChildMinutes {
    parent_ts_ns: u64,
    child_stride_ns: u64,
    next_index: u64,
    end_index: u64,
}

impl PopulatedChildMinutes {
    fn in_window(parent: &ParentSummary, start: u64, end: u64) -> Self {
        let count = u64::from(parent.child_count);
        let (next_index, end_index) = if count == 0 || start >= end || parent.parent_ts_ns >= end {
            (0, 0)
        } else if parent.child_stride_ns == 0 {
            if parent.parent_ts_ns >= start {
                (0, count)
            } else {
                (0, 0)
            }
        } else {
            let first = if parent.parent_ts_ns >= start {
                0
            } else {
                (start - parent.parent_ts_ns)
                    .div_ceil(parent.child_stride_ns)
                    .min(count)
            };
            let last_ts = parent
                .parent_ts_ns
                .saturating_add((count - 1).saturating_mul(parent.child_stride_ns));
            let past_end = if last_ts < end {
                count
            } else {
                (end - parent.parent_ts_ns)
                    .div_ceil(parent.child_stride_ns)
                    .min(count)
            };
            (first.min(past_end), past_end)
        };
        Self {
            parent_ts_ns: parent.parent_ts_ns,
            child_stride_ns: parent.child_stride_ns,
            next_index,
            end_index,
        }
    }

    fn child_ts(&self, index: u64) -> u64 {
        self.parent_ts_ns
            .saturating_add(index.saturating_mul(self.child_stride_ns))
    }
}

impl Iterator for PopulatedChildMinutes {
    type Item = (u64, u64, u64);

    fn next(&mut self) -> Option<Self::Item> {
        if self.next_index >= self.end_index {
            return None;
        }
        let index = self.next_index;
        let representative_ts = self.child_ts(index);
        let minute = representative_ts / NS_PER_MINUTE;
        let next_index = if self.child_stride_ns == 0 {
            self.end_index
        } else if let Some(boundary) = minute
            .checked_add(1)
            .and_then(|next| next.checked_mul(NS_PER_MINUTE))
        {
            if self.child_ts(self.end_index - 1) < boundary {
                self.end_index
            } else {
                (boundary - self.parent_ts_ns)
                    .div_ceil(self.child_stride_ns)
                    .max(index + 1)
                    .min(self.end_index)
            }
        } else {
            self.end_index
        };
        self.next_index = next_index;
        Some((minute, representative_ts, next_index - index))
    }
}

/// Spec 3.3 steps 3 to 6, over any parent stream: the open-parent lifecycle,
/// the child enumeration, the session rotation, the termination guard and the
/// measured mean gap. Everything family-specific is behind [`ParentSource`], so
/// this is the whole of what the layer-1 oracle validates.
///
/// A STABLE PROFILE FRAME. One call per seed walk, so the annotation costs
/// nothing and the name shows up run after run - which is the whole value of a
/// small fixed annotation set. Nothing inside this function is annotated: the
/// loop body runs tens of millions of times per call.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn project_stream(
    walk: &mut dyn ParentSource,
    start: u64,
    end: u64,
    offset: i32,
    _seed: u64,
) -> Result<Projected, ProjectStop> {
    let mut projection = ScreenReduced::default();
    #[cfg(test)]
    let mut sessions = Vec::new();
    let mut acc: Option<ScreenSessionAcc> = None;
    let mut open_parent: Option<(u64, u8, u64)> = None;
    let mut first = None;
    let mut last = None;
    let mut parents = 0_u64;
    // A plain local increment on the hottest line in the screen, totalled into
    // the process tally once per walk rather than per print - an atomic here
    // would be 35 M contended operations to answer a question asked once.
    let mut prints = 0_u64;
    let mut previous = None;
    // `GeneratedAcc::close_open_parent`, transcribed: a parent outside the
    // measured window is dropped, a parent with no open session is skipped
    // exactly as the shipped accumulator skips it, and a parent whose segment
    // disagrees with the open session trips the rotation invariant.
    let close_parent = |acc: &mut Option<ScreenSessionAcc>,
                        open: &mut Option<(u64, u8, u64)>|
     -> Result<(), ProjectStop> {
        if let Some((ts, index, session_start)) = open.take()
            && start <= ts
            && ts < end
            && let Some(target) = acc.as_mut()
        {
            if target.session_start_ns != session_start {
                return Err(projection_defect(
                    "a measured parent closes into another session; the rotation invariant is broken",
                ));
            }
            target.push_parent(index, ts)?;
        }
        Ok(())
    };
    loop {
        let parent = walk.next().map_err(ProjectStop::Refused)?;
        // The family-1 termination guard of spec 3.3 step 3: a faulted
        // `GeneratedSource` hands back a stale summary forever.
        if previous.is_some_and(|p| parent.parent_ts_ns <= p) {
            return Err(projection_defect("parent walk stalled"));
        }
        previous = Some(parent.parent_ts_ns);
        if parent.parent_ts_ns >= end {
            break;
        }
        // The PREVIOUS parent closes here, before this one's children, exactly
        // as `GeneratedAcc::push_quote` closes it before the quote that opens
        // the next event - so it lands in the session it ran in even when this
        // burst's first child rotates.
        close_parent(&mut acc, &mut open_parent)?;
        let mut opened = false;
        for (_minute, ts, child_count_in_minute) in
            PopulatedChildMinutes::in_window(&parent, start, end)
        {
            let seg = session_segment_at(ts, offset).ok_or_else(|| {
                projection_refusal(ts, "a projected child lands inside a closed halt segment")
            })?;
            if acc
                .as_ref()
                .is_none_or(|a| a.session_start_ns != seg.session_start_ns)
            {
                close_parent(&mut acc, &mut open_parent)?;
                if let Some(old) = acc.take() {
                    projection.merge(old.reduced()?);
                    #[cfg(test)]
                    sessions.push(old.close()?);
                }
                acc = Some(ScreenSessionAcc::new(
                    format_trade_date(seg.trade_day),
                    &seg,
                    offset,
                ));
            }
            if let Some(active) = acc.as_mut() {
                active.push_print(ts);
                prints += child_count_in_minute;
            }
            // THE PARENT OPENS ON ITS FIRST CHILD, not after its last.
            // `GeneratedAcc::push_trade` rotates at the top and sets
            // `open_parent` at the bottom of the SAME call, so a parent is open
            // from its first sided print onward; a later child of the same
            // burst that rotates the session therefore closes THIS parent into
            // the session its first child fell in. Opening it after the child
            // loop instead would leave a straddling burst's parent to be
            // written after the rotation, into the new accumulator, which trips
            // `close_open_parent`'s rotation invariant - the very defect spec
            // 3.3's IMPLEMENTATION DECISION was written to prevent.
            if !opened {
                opened = true;
                first.get_or_insert(parent.parent_ts_ns);
                last = Some(parent.parent_ts_ns);
                parents += 1;
                open_parent = Some(open_parent_at(parent.parent_ts_ns, offset)?);
            }
        }
        // A parent that emitted no child in the window still counts if its own
        // instant is measured: the walk saw it, so the mean gap owes it a
        // reading. It cannot straddle anything, having nothing to straddle.
        if !opened && (start..end).contains(&parent.parent_ts_ns) {
            first.get_or_insert(parent.parent_ts_ns);
            last = Some(parent.parent_ts_ns);
            parents += 1;
            open_parent = Some(open_parent_at(parent.parent_ts_ns, offset)?);
        }
    }
    close_parent(&mut acc, &mut open_parent)?;
    if let Some(old) = acc.take() {
        projection.merge(old.reduced()?);
        #[cfg(test)]
        sessions.push(old.close()?);
    }
    let realized_mean_gap_s = first
        .zip(last)
        .filter(|_| parents >= 2)
        .map(|(a, b)| (b - a) as f64 / ((parents - 1) as f64 * 1e9));
    Ok(Projected {
        projection,
        #[cfg(test)]
        sessions,
        parents,
        prints,
        realized_mean_gap_s,
    })
}

/// The section 3.5 composition loss for ONE seed: the observed-populated-minute
/// weighted mean, over hours, of the exact 1-Wasserstein distance between the
/// two `log1p(N)` populations. `None` when the observed side carries no
/// populated minute at all, which is the only degenerate case section 3.5
/// defines.
///
/// AN UNDEFINED HOUR DISTANCE IS A DEFECT, NOT A WEIGHT TO REDISTRIBUTE.
/// `wasserstein_log1p` returns `None` for exactly one empty side, which here
/// means an hour with observed mass and no generated mass. That state is
/// UNREACHABLE on this path: the loss is computed for admissible cells only,
/// and A1 limb (a) already demands a nonzero generated count in every
/// `(hour, bin)` whose observed share is above zero - so an hour with observed
/// mass has generated mass, or the cell never got here. The earlier code
/// dropped such an hour and renormalized the remaining weights, which invents a
/// rule section 3.5 does not state and would have silently reported a smaller
/// loss for a cell whose generated side had a hole. It refuses instead: if a
/// later change to A1, to the hour set or to the marginal makes this reachable,
/// the run stops and says so rather than ranking on a quantity nobody defined.
fn composition_loss(observed: &CountMarginal, generated: &CountMarginal) -> LabResult<Option<f64>> {
    let mut weighted = 0.0;
    let mut weight = 0_u64;
    for (hour, obs) in observed {
        let n: u64 = obs.iter().map(|x| x.1).sum();
        let side = generated.get(hour).map_or(&[][..], Vec::as_slice);
        let Some(distance) = wasserstein_log1p(obs, side) else {
            return Err(LabError::refusal(format!(
                "the composition loss reached hour {hour} with observed mass and no generated \
                 mass; A1 limb (a) admits no such cell, so this is a screen defect rather than \
                 an hour to drop, and section 3.5 states no rule for weighting an undefined \
                 distance"
            )));
        };
        weighted += distance * n as f64;
        weight += n;
    }
    Ok((weight > 0).then(|| weighted / weight as f64))
}

#[cfg(test)]
fn gap_within_tolerance(_ctx: &ScreenContext, _realized_s: Option<f64>) -> bool {
    true
}

fn projection_marginal(projection: &ScreenReduced) -> CountMarginal {
    projection
        .parent_counts
        .iter()
        .map(|(&hour, counts)| (hour, counts.iter().map(|(&n, &count)| (n, count)).collect()))
        .collect()
}

fn projection_bin_count(projection: &ScreenReduced, hour: u32, target: &str) -> u64 {
    projection
        .parent_counts
        .get(&hour)
        .into_iter()
        .flatten()
        .filter(|(n, _)| bin_name(**n) == target)
        .map(|(_, count)| *count)
        .sum()
}

fn projection_bins(projection: &ScreenReduced) -> BTreeMap<&'static str, u64> {
    let mut bins = BTreeMap::new();
    for counts in projection.parent_counts.values() {
        for (&n, &count) in counts {
            *bins.entry(bin_name(n)).or_default() += count;
        }
    }
    bins
}

fn projection_window(projection: &ScreenReduced, hour: i64, window: i64) -> Option<&ScreenWindow> {
    let hour = u32::try_from(hour).ok()?;
    let window = u32::try_from(window).ok()?;
    projection.windows.get(&hour)?.get(&window)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Stage A window counts stay far below 2^53"
)]
fn projection_fano(projection: &ScreenReduced, hour: i64, window: i64) -> Option<f64> {
    let cell = projection_window(projection, hour, window)?;
    let total: u64 = cell.count_hist.values().sum();
    if total == 0 {
        return None;
    }
    let sum: u64 = cell.count_hist.iter().map(|(&n, &count)| n * count).sum();
    let sumsq: u64 = cell
        .count_hist
        .iter()
        .map(|(&n, &count)| n * n * count)
        .sum();
    let mean = sum as f64 / total as f64;
    if mean <= 0.0 {
        return None;
    }
    let variance = sumsq as f64 / total as f64 - mean * mean;
    Some(variance / mean)
}

fn projection_count_quantile(
    projection: &ScreenReduced,
    hour: i64,
    window: i64,
    q: f64,
) -> Option<f64> {
    let cell = projection_window(projection, hour, window)?;
    let pairs: Vec<_> = cell
        .count_hist
        .iter()
        .map(|(&value, &weight)| Some((i64::try_from(value).ok()?, i64::try_from(weight).ok()?)))
        .collect::<Option<_>>()?;
    #[expect(
        clippy::cast_precision_loss,
        reason = "Stage A window counts stay far below 2^53"
    )]
    weighted_nearest_rank(&pairs, q).map(|value| value as f64)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Stage A parent and window counts stay far below 2^53"
)]
fn projection_mean_parents(projection: &ScreenReduced, hour: i64) -> Option<f64> {
    let hour = u32::try_from(hour).ok()?;
    let parents: u64 = projection
        .parent_counts
        .get(&hour)
        .into_iter()
        .flatten()
        .map(|(&n, &count)| u64::from(n) * count)
        .sum();
    let scheduled = projection.windows.get(&hour)?.get(&60)?.scheduled;
    (scheduled > 0).then_some(parents as f64 / scheduled as f64)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "Stage A second counts stay far below 2^53"
)]
fn projection_zero_fraction(projection: &ScreenReduced, hour: i64) -> Option<f64> {
    let cell = projection_window(projection, hour, 1)?;
    (cell.present_sessions == projection.sessions && cell.scheduled > 0)
        .then_some(cell.zeros as f64 / cell.scheduled as f64)
}

fn projection_support_refusals(
    observed: &ScreenReduced,
    generated: &ScreenReduced,
) -> Vec<RefusalRec> {
    let mut hours: Vec<u32> = observed
        .parent_counts
        .keys()
        .chain(generated.parent_counts.keys())
        .copied()
        .collect();
    hours.sort_unstable();
    hours.dedup();
    let mut refusals = Vec::new();
    for hour in hours {
        for &bin in PARENT_COUNT_BIN_NAMES {
            if projection_bin_count(observed, hour, bin) > 0
                && projection_bin_count(generated, hour, bin) == 0
            {
                refusals.push(RefusalRec::new(
                    "count_substitution",
                    format!("hour {hour} bin {bin}"),
                    "observed support with zero generated support",
                ));
            }
        }
    }
    refusals
}

pub fn evaluate_cell(ctx: &ScreenContext, cell: &Cell, seeds: &[u64]) -> LabResult<CellVerdict> {
    Ok(evaluate_cell_with_work(ctx, cell, seeds)?.verdict)
}

pub fn evaluate_cell_with_work(
    ctx: &ScreenContext,
    cell: &Cell,
    seeds: &[u64],
) -> LabResult<CellEvaluation> {
    CELLS_EVALUATED.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let mut walks = Vec::new();
    for &seed in seeds {
        walks.push(project_seed(ctx, cell, seed)?);
    }
    let mut verdict = verdict_from_walks(ctx, cell, &walks)?;
    // The walk is the cost; the condition arithmetic over its reduced session
    // records is not, but it is billed here anyway so the artifact's per-cell
    // price is the whole price.
    verdict.cost_s = started.elapsed().as_secs_f64();
    Ok(CellEvaluation {
        verdict,
        parents: walks.iter().map(|walk| walk.parents).sum(),
        prints: walks.iter().map(|walk| walk.prints).sum(),
        demand: envelope_demand_from_walks(ctx, cell.family(), &walks),
    })
}

/// Measures only the real candidate walks, excluding gate and envelope work.
///
/// The batch pilot estimates projection throughput. Envelope work has its own
/// section 9.7 per-family/per-K probes and must not contaminate that estimator.
pub fn measure_cell_walks(
    ctx: &ScreenContext,
    cell: &Cell,
    seeds: &[u64],
) -> LabResult<WalkMeasurement> {
    CELLS_EVALUATED.fetch_add(1, Ordering::Relaxed);
    let started = Instant::now();
    let mut parents = 0_u64;
    let mut prints = 0_u64;
    for &seed in seeds {
        let walk = project_seed(ctx, cell, seed)?;
        parents = parents.saturating_add(walk.parents);
        prints = prints.saturating_add(walk.prints);
    }
    Ok(WalkMeasurement {
        cost_s: started.elapsed().as_secs_f64(),
        parents,
        prints,
    })
}

/// Projects cells across a bounded worker pool and reduces their verdicts on
/// the calling thread. Results are returned in input order regardless of task
/// completion order. Queue wait is excluded from `cost_s`; it contains worker
/// execution plus verdict reduction only.
///
/// `projection` must come from [`ScreenContext::parallel_projection`] and may
/// be reused across every coarse and refinement pass in one run.
pub fn evaluate_cells_parallel(
    ctx: &ScreenContext,
    projection: &ProjectionContext,
    cells: &[ScheduledCell],
    jobs: usize,
    guard: Option<&mut BudgetGuard>,
) -> LabResult<Vec<CellEvaluation>> {
    evaluate_cells_parallel_impl(ctx, projection, cells, jobs, guard, true)
}

/// Projects the frozen cells through the screen's cache and deviation path,
/// but stops before predictive-envelope evaluation and verdict use.
pub fn census_cells_parallel(
    ctx: &ScreenContext,
    projection: &ProjectionContext,
    cells: &[ScheduledCell],
    jobs: usize,
    guard: Option<&mut BudgetGuard>,
) -> LabResult<Vec<CellEnvelopeDemand>> {
    evaluate_cells_parallel_impl(ctx, projection, cells, jobs, guard, false).map(|evaluations| {
        evaluations
            .into_iter()
            .map(|evaluation| evaluation.demand)
            .collect()
    })
}

fn evaluate_cells_parallel_impl(
    ctx: &ScreenContext,
    projection: &ProjectionContext,
    cells: &[ScheduledCell],
    jobs: usize,
    mut guard: Option<&mut BudgetGuard>,
    evaluate_envelopes: bool,
) -> LabResult<Vec<CellEvaluation>> {
    if jobs == 0 {
        return Err(LabError::refusal("--jobs must be at least 1"));
    }
    if cells.is_empty() {
        return Ok(Vec::new());
    }
    if cells.iter().any(|cell| cell.seeds.is_empty()) {
        return Err(LabError::refusal(
            "a scheduled Stage A cell carries no seeds",
        ));
    }

    // Put the historically expensive generator family at the front. Within a
    // cell, seeds retain their declared order in the result slots even though
    // workers may finish them in any order.
    let mut cell_order: Vec<usize> = (0..cells.len()).collect();
    cell_order.sort_by_key(|&index| match cells[index].cell.family() {
        Family::EventMarkov => 0,
        Family::WallMmpp => 1,
        Family::LogOuCox => 2,
        Family::SelfExciting => 3,
        Family::ShotNoise => 4,
    });
    let tasks: Vec<_> = cell_order
        .into_iter()
        .flat_map(|cell_index| {
            (0..cells[cell_index].seeds.len()).map(move |seed_index| SeedTask {
                cell_index,
                seed_index,
            })
        })
        .collect();
    let next = AtomicUsize::new(0);
    let cancelled = AtomicBool::new(false);
    let worker_count = jobs.min(tasks.len());
    let (sender, receiver) = mpsc::sync_channel(worker_count);
    let mut walk_slots: Vec<Vec<Option<ScheduledWalk>>> = cells
        .iter()
        .map(|cell| {
            std::iter::repeat_with(|| None)
                .take(cell.seeds.len())
                .collect()
        })
        .collect();
    let mut remaining: Vec<usize> = cells.iter().map(|cell| cell.seeds.len()).collect();
    let mut evaluations: Vec<Option<CellEvaluation>> =
        std::iter::repeat_with(|| None).take(cells.len()).collect();
    let mut first_error = None;

    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            let sender = sender.clone();
            let next = &next;
            let cancelled = &cancelled;
            let tasks = &tasks;
            scope.spawn(move || {
                loop {
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let task_index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(task) = tasks.get(task_index) else {
                        break;
                    };
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let started = Instant::now();
                    let product = project_seed_parallel(
                        projection,
                        &cells[task.cell_index].cell,
                        cells[task.cell_index].seeds[task.seed_index],
                    )
                    .map(|walk| ScheduledWalk {
                        walk,
                        execution_s: started.elapsed().as_secs_f64(),
                    });
                    let failed = product.is_err();
                    if sender
                        .send(SeedResult {
                            task_index,
                            product,
                        })
                        .is_err()
                    {
                        break;
                    }
                    if failed {
                        cancelled.store(true, Ordering::Release);
                        break;
                    }
                }
            });
        }
        drop(sender);

        for message in receiver {
            if first_error.is_some() {
                continue;
            }
            let task = &tasks[message.task_index];
            match message.product {
                Ok(product) => {
                    walk_slots[task.cell_index][task.seed_index] = Some(product);
                    remaining[task.cell_index] -= 1;
                    if remaining[task.cell_index] != 0 {
                        continue;
                    }
                    let reduction_started = Instant::now();
                    let scheduled: Vec<_> = walk_slots[task.cell_index]
                        .iter_mut()
                        .map(|slot| slot.take().expect("completed cell has every seed"))
                        .collect();
                    let execution_s = scheduled.iter().map(|item| item.execution_s).sum::<f64>();
                    let walks: Vec<_> = scheduled.into_iter().map(|item| item.walk).collect();
                    let mut verdict = match verdict_from_walks_with_envelopes(
                        ctx,
                        &cells[task.cell_index].cell,
                        &walks,
                        evaluate_envelopes,
                    ) {
                        Ok(verdict) => verdict,
                        Err(error) => {
                            cancelled.store(true, Ordering::Release);
                            first_error = Some(error);
                            continue;
                        }
                    };
                    verdict.cost_s = execution_s + reduction_started.elapsed().as_secs_f64();
                    CELLS_EVALUATED.fetch_add(1, Ordering::Relaxed);
                    evaluations[task.cell_index] = Some(CellEvaluation {
                        parents: walks.iter().map(|walk| walk.parents).sum(),
                        prints: walks.iter().map(|walk| walk.prints).sum(),
                        demand: envelope_demand_from_walks(
                            ctx,
                            cells[task.cell_index].cell.family(),
                            &walks,
                        ),
                        verdict,
                    });
                    if let Some(guard) = guard.as_deref_mut()
                        && let Err(stop) = guard.check()
                    {
                        cancelled.store(true, Ordering::Release);
                        first_error = Some(stop.into());
                    }
                }
                Err(error) => {
                    cancelled.store(true, Ordering::Release);
                    first_error = Some(error);
                }
            }
        }
    });

    if let Some(error) = first_error {
        return Err(error);
    }
    evaluations
        .into_iter()
        .map(|evaluation| {
            evaluation.ok_or_else(|| LabError::refusal("Stage A scheduler left a cell unfinished"))
        })
        .collect()
}

/// A1 to A4, the loss and the reported diagnostics over walks that have already
/// been run. Split from [`evaluate_cell`] because every condition in spec
/// section 3.4 is a statement about a cell's per-seed PRODUCTS: a test that has
/// to walk a month of tape to ask whether a refused seed keeps a cell out of the
/// loss is testing the generator, not the condition.
///
/// # Errors
/// A malformed session record, or the section 3.5 unreachability refusal.
pub fn verdict_from_walks(
    ctx: &ScreenContext,
    cell: &Cell,
    walks: &[SeedWalk],
) -> LabResult<CellVerdict> {
    verdict_from_walks_with_envelopes(ctx, cell, walks, true)
}

fn verdict_from_walks_with_envelopes(
    ctx: &ScreenContext,
    cell: &Cell,
    walks: &[SeedWalk],
    evaluate_envelopes: bool,
) -> LabResult<CellVerdict> {
    let observed = &ctx.observed_projection;
    let obs_bins = projection_bins(observed);
    let obs_total: u64 = obs_bins.values().sum();
    let mut a1_rows = Vec::new();
    let mut refusals = Vec::new();
    let mut a4_refusals = Vec::new();
    let mut losses = Vec::new();
    let mut tv_readings = Vec::new();
    let mut fano_60_readings = Vec::new();
    let mut p99_60_readings = Vec::new();
    let mut fano_tiebreak_readings = Vec::new();

    for walk in walks {
        let seed = walk.seed;
        if let Some(r) = &walk.refusal {
            refusals.push(json!({"seed":seed,"refusal":r}));
            a4_refusals.push(json!({"seed":seed,"refusal":r}));
            continue;
        }
        let generated = &walk.projection;
        let generated_marginal = projection_marginal(generated);
        let gen_bins = projection_bins(generated);
        let gen_total: u64 = gen_bins.values().sum();
        let tv = (obs_total > 0 && gen_total > 0).then(|| {
            PARENT_COUNT_BINS
                .iter()
                .map(|&(lo, _)| {
                    let bin = bin_name(lo);
                    (obs_bins.get(bin).copied().unwrap_or(0) as f64 / obs_total as f64
                        - gen_bins.get(bin).copied().unwrap_or(0) as f64 / gen_total as f64)
                        .abs()
                })
                .sum::<f64>()
                / 2.0
        });
        tv_readings.push(tv);

        let mut all_fano = Vec::new();
        let mut fano_60 = Vec::new();
        let mut p99_60 = Vec::new();
        for &hour in &ctx.hours {
            for window in [1_i64, 5, 60] {
                if let (Some(g), Some(o)) = (
                    projection_fano(generated, hour, window),
                    projection_fano(observed, hour, window),
                ) && g > 0.0
                    && o > 0.0
                {
                    let reading = (g / o).ln().abs();
                    all_fano.push(reading);
                    if window == 60 {
                        fano_60.push(reading);
                    }
                }
            }
            if let (Some(g), Some(o)) = (
                projection_count_quantile(generated, hour, 60, 0.99),
                projection_count_quantile(observed, hour, 60, 0.99),
            ) && g > 0.0
                && o > 0.0
            {
                p99_60.push((g / o).ln().abs());
            }
        }
        let mean = |xs: &[f64]| (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64);
        fano_tiebreak_readings.push(mean(&all_fano));
        fano_60_readings.push(mean(&fano_60));
        p99_60_readings.push(mean(&p99_60));

        let support_refusals = projection_support_refusals(observed, generated);
        let mut failing_cells = Vec::new();
        let mut required = serde_json::Map::new();
        for hour in FAIL_HOURS_300 {
            let hour_u32 = hour;
            for &bin in PARENT_COUNT_BIN_NAMES {
                if bin == "0" {
                    continue;
                }
                let required_here =
                    projection_bin_count(observed, hour_u32, bin) >= MIN_MINUTES_CELL;
                if !required_here {
                    continue;
                }
                let generated_count = projection_bin_count(generated, hour_u32, bin);
                required.insert(format!("{hour}:{bin}"), json!(generated_count));
                if generated_count < MIN_MINUTES_CELL {
                    failing_cells.push(format!("{hour}:{bin}"));
                }
            }
        }
        let support = support_refusals.is_empty() && failing_cells.is_empty();
        for refusal in &support_refusals {
            refusals.push(json!({"seed":seed,"scope":refusal.scope,
                "cell":refusal.cell,"reason":refusal.reason}));
        }
        a1_rows.push(json!({"seed":seed,"passed":support,
            "failing_cells":failing_cells,"required_bin_counts":required}));

        if support {
            losses.push(composition_loss(
                &ctx.observed_marginal,
                &generated_marginal,
            )?);
        }
    }

    let a1_pass = a1_rows.len() == walks.len() && a1_rows.iter().all(|v| v["passed"] == true);
    let a4_pass = !walks.is_empty() && a4_refusals.is_empty();
    let (a2, a3, a2_pass, a3_pass) =
        amended_rate_and_zero_gates(ctx, cell, walks, evaluate_envelopes, a1_pass && a4_pass)?;
    let admissible = a1_pass && a2_pass && a3_pass && a4_pass;
    Ok(CellVerdict {
        cell: cell.clone(),
        fitted_params: cell.fitted_params(),
        a1: json!({"passed":a1_pass,"per_seed":a1_rows}),
        a2,
        a3,
        a4: json!({"passed":a4_pass,"refusal":a4_refusals.first().cloned(),
                   "per_seed":a4_refusals}),
        admissible,
        loss: admissible.then(|| seed_median(&losses)).flatten(),
        reported: json!({
            "tv_six_bin":seed_median(&tv_readings),
            "fano_60_log_ratio":seed_median(&fano_60_readings),
            "count_p99_60_log_ratio":seed_median(&p99_60_readings),
            "fano_tiebreak":seed_median(&fano_tiebreak_readings)
        }),
        refusals,
        cost_s: walks.iter().map(|walk| walk.cost_s).sum(),
    })
}

/// The pre-Step-4 JSON/`ObsContext` path, retained only as a differential
/// oracle until the typed verdict has been proven against all condition tests.
#[cfg(test)]
fn verdict_from_walks_legacy(
    ctx: &ScreenContext,
    cell: &Cell,
    walks: &[SeedWalk],
) -> LabResult<CellVerdict> {
    let observed_rates = &ctx.observed_rates;
    let observed_zero = &ctx.observed_zero;
    let mut a1_rows = Vec::new();
    let mut a2_rows = Vec::new();
    let mut a3_rows = Vec::new();
    let mut refusals = Vec::new();
    let mut a4_refusals = Vec::new();
    let mut losses = Vec::new();
    let mut tv_readings = Vec::new();
    let mut fano_60_readings = Vec::new();
    let mut p99_60_readings = Vec::new();
    let mut fano_tiebreak_readings = Vec::new();
    for walk in walks {
        let seed = walk.seed;
        if let Some(r) = &walk.refusal {
            refusals.push(json!({"seed":seed,"refusal":r}));
            a4_refusals.push(json!({"seed":seed,"refusal":r}));
            continue;
        }
        let generated = ObsContext::new(walk.sessions.clone());
        let mut hist = Vec::new();
        for s in &walk.sessions {
            hist.extend(s["block1_hist"].as_array().into_iter().flatten().cloned());
        }
        let gm = parent_count_marginal(&Value::Array(hist))?;
        let mut obs_bins = BTreeMap::new();
        let mut gen_bins = BTreeMap::new();
        for hour in 0..24_u32 {
            for (bin, count) in
                bin_totals(ctx.observed_marginal.get(&hour).map_or(&[], Vec::as_slice))
            {
                *obs_bins.entry(bin).or_insert(0_u64) += count;
            }
            for (bin, count) in bin_totals(gm.get(&hour).map_or(&[], Vec::as_slice)) {
                *gen_bins.entry(bin).or_insert(0_u64) += count;
            }
        }
        let obs_total: u64 = obs_bins.values().sum();
        let gen_total: u64 = gen_bins.values().sum();
        let tv = (obs_total > 0 && gen_total > 0).then(|| {
            PARENT_COUNT_BINS
                .iter()
                .map(|&(lo, _)| {
                    let bin = bin_name(lo);
                    (obs_bins.get(bin).copied().unwrap_or(0) as f64 / obs_total as f64
                        - gen_bins.get(bin).copied().unwrap_or(0) as f64 / gen_total as f64)
                        .abs()
                })
                .sum::<f64>()
                / 2.0
        });
        tv_readings.push(tv);
        let obs_mult = vec![1_i64; ctx.observed.per_session().len()];
        let gen_mult = vec![1_i64; generated.per_session().len()];
        let mut all_fano = Vec::new();
        let mut fano_60 = Vec::new();
        let mut p99_60 = Vec::new();
        for &hour in &ctx.hours {
            for window in [1_i64, 5, 60] {
                if let (Some(g), Some(o)) = (
                    generated.b2_fano(hour, window, &gen_mult),
                    ctx.observed.b2_fano(hour, window, &obs_mult),
                ) && g > 0.0
                    && o > 0.0
                {
                    let reading = (g / o).ln().abs();
                    all_fano.push(reading);
                    if window == 60 {
                        fano_60.push(reading);
                    }
                }
            }
            if let (Some(g), Some(o)) = (
                generated.b2_count_quantile(hour, 60, 0.99, &gen_mult),
                ctx.observed.b2_count_quantile(hour, 60, 0.99, &obs_mult),
            ) && g > 0.0
                && o > 0.0
            {
                p99_60.push((g / o).ln().abs());
            }
        }
        let mean = |xs: &[f64]| (!xs.is_empty()).then(|| xs.iter().sum::<f64>() / xs.len() as f64);
        fano_tiebreak_readings.push(mean(&all_fano));
        fano_60_readings.push(mean(&fano_60));
        p99_60_readings.push(mean(&p99_60));
        // A1 is gate B2's predicate, called rather than restated: limb (a) is
        // `count_substitution` over the SAME pooling B2 uses, limb (b) is
        // `conditional_adequacy_bins`. A second copy here would be free to
        // drift stronger than the gate Stage A claims to be contained by -
        // the hand-rolled version this replaced judged the zero bin, which
        // `conditional_adequacy_bins` deliberately skips.
        let substitution = count_substitution(
            &pool_session_hists(generated.per_session())?,
            &ctx.observed_shares,
        );
        let support_refusals = support_refusals_of(&substitution);
        let conditional =
            conditional_adequacy_bins(&ctx.observed, std::slice::from_ref(&generated));
        let failing: Vec<&crate::aggregate::family::CondBin> = conditional
            .iter()
            .filter(|b| b.required && !b.supported)
            .collect();
        let support = support_refusals.is_empty() && failing.is_empty();
        let ones = generated.ones();
        let required: serde_json::Map<String, Value> = conditional
            .iter()
            .filter(|b| b.required)
            .map(|b| {
                (
                    format!("{}:{}", b.hour, b.bin_name),
                    json!(generated.b1_bin_count(b.hour, &b.bin_name, &ones)),
                )
            })
            .collect();
        for r in &support_refusals {
            refusals.push(json!({"seed":seed,"scope":r.scope,"cell":r.cell,"reason":r.reason}));
        }
        a1_rows.push(json!({"seed":seed,"passed":support,
            "failing_cells":failing.iter()
                .map(|b| format!("{}:{}", b.hour, b.bin_name)).collect::<Vec<_>>(),
            "required_bin_counts":required}));
        let rates = hourly_mean_parents(&generated);
        let zeros = hourly_zero_second_fraction(&generated);
        let mut rate_ok = true;
        let mut zero_ok = true;
        for &hour in &ctx.hours {
            let ratio = rates[&hour]
                .mean
                .zip(observed_rates[&hour].mean)
                .and_then(|(g, o)| (o > 0.0).then_some(g / o));
            let pass = ratio.is_some_and(|r| (MEAN_RATE_BAND.0..=MEAN_RATE_BAND.1).contains(&r));
            rate_ok &= pass;
            a2_rows.push(json!({"seed":seed,"hour":hour,"ratio":ratio,"passed":pass}));
            let zr = zeros[&hour]
                .zip(observed_zero[&hour])
                .and_then(|(g, o)| (o > 0.0).then_some(g / o));
            let zpass = zr.is_some_and(|r| (ZERO_COUNT_BAND.0..=ZERO_COUNT_BAND.1).contains(&r));
            zero_ok &= zpass;
            a3_rows.push(json!({"seed":seed,"hour":hour,"ratio":zr,"passed":zpass}));
        }
        let gap_ok = gap_within_tolerance(ctx, walk.realized_mean_gap_s);
        if !gap_ok {
            // A4's mean-gap limb is a refusal in its own right (3.2's
            // `ScreenRefusal::MeanGap`), not merely a failed flag: the artifact
            // has to say which seed's walk drifted and by how much.
            let mean_gap = ScreenRefusal {
                variant: "mean_gap".into(),
                clock_ns: 0,
                detail: format!(
                    "realized {:?} against declared {}",
                    walk.realized_mean_gap_s, ctx.profile.scalars.mean_event_duration_s
                ),
                family: None,
                canonical_params: None,
                seed: None,
            };
            refusals.push(json!({"seed":seed,"refusal":mean_gap}));
            a4_refusals.push(json!({"seed":seed,"refusal":mean_gap}));
        }
        if support && rate_ok && zero_ok && gap_ok {
            losses.push(composition_loss(&ctx.observed_marginal, &gm)?);
        }
    }
    let a1_pass = a1_rows.len() == walks.len() && a1_rows.iter().all(|v| v["passed"] == true);
    let a2_pass = a2_rows.len() == walks.len() * ctx.hours.len()
        && a2_rows.iter().all(|v| v["passed"] == true);
    let a3_pass = a3_rows.len() == walks.len() * ctx.hours.len()
        && a3_rows.iter().all(|v| v["passed"] == true);
    let a4_pass = !walks.is_empty()
        && a4_refusals.is_empty()
        && walks
            .iter()
            .all(|w| gap_within_tolerance(ctx, w.realized_mean_gap_s));
    let admissible = a1_pass && a2_pass && a3_pass && a4_pass;
    Ok(CellVerdict {
        cell: cell.clone(),
        fitted_params: cell.fitted_params(),
        a1: json!({"passed":a1_pass,"per_seed":a1_rows}),
        a2: json!({"passed":a2_pass,"per_seed_hour":a2_rows}),
        a3: json!({"passed":a3_pass,"per_seed_hour":a3_rows}),
        a4: json!({"passed":a4_pass,"refusal":a4_refusals.first().cloned(),
                   "per_seed":a4_refusals}),
        admissible,
        loss: admissible.then(|| seed_median(&losses)).flatten(),
        reported: json!({
            "tv_six_bin":seed_median(&tv_readings),
            "fano_60_log_ratio":seed_median(&fano_60_readings),
            "count_p99_60_log_ratio":seed_median(&p99_60_readings),
            "fano_tiebreak":seed_median(&fano_tiebreak_readings)
        }),
        refusals,
        // Overwritten by `evaluate_cell`, which is the only caller that knows
        // what the walks cost.
        cost_s: walks.iter().map(|w| w.cost_s).sum(),
    })
}

/// Spec 5.1.1, the two run-level ceilings. Wall time is judged first because a
/// run that has already burned its hours is stopped whatever its memory did.
///
/// The comparison is `>=`, not `>`: the constants are ceilings the run may
/// approach and not reach, and an exactly-at-the-bound reading is a crossing.
#[must_use]
pub fn budget_verdict(elapsed_s: f64, peak_rss_bytes: u64) -> Option<&'static str> {
    if elapsed_s >= STAGE_A_BUDGET_S {
        Some("stage-a-budget-exceeded")
    } else if peak_rss_bytes >= STAGE_A_RSS_BYTES {
        Some("stage-a-rss-exceeded")
    } else {
        None
    }
}

/// A crossed ceiling, with the readings that crossed it. These two verdict
/// strings are exit conditions of the DRIVER (spec 5.1.1) and never artifact
/// states: an over-budget run serializes nothing.
#[derive(Debug, Clone)]
pub struct BudgetStop {
    pub verdict: &'static str,
    pub elapsed_s: f64,
    pub peak_rss_bytes: u64,
}

impl std::fmt::Display for BudgetStop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: total_s={:.3} peak_rss_bytes={}",
            self.verdict, self.elapsed_s, self.peak_rss_bytes
        )
    }
}

impl From<BudgetStop> for LabError {
    fn from(stop: BudgetStop) -> Self {
        Self::refusal(stop.to_string())
    }
}

/// Wall time and peak RSS for one Stage A run, sampled at every cell boundary.
///
/// The RSS side is [`ResourceSampler`], the same sampler the 12a measurement
/// bills its cost against, so the probe and the full run are measured by one
/// instrument (spec 5.1.1).
pub struct BudgetGuard {
    started: Instant,
    sampler: Option<ResourceSampler>,
    scan_dir: Option<PathBuf>,
    peak_rss: u64,
    /// TEST SEAM, and the only way the budget arithmetic can be exercised
    /// without an eleven-hour run or an eight-gigabyte allocation: a scripted
    /// sequence of `(elapsed_s, rss_bytes)` boundary readings, consumed one per
    /// [`BudgetGuard::check`].
    ///
    /// It is deliberately NOT on the command-line surface and not `pub`: it is
    /// `#[cfg(test)]`-constructed only, so no shipped code path - and no flag,
    /// env var or config key - can reach it. `--cost-probe` is a declared mode
    /// of the subcommand; this is not a mode at all.
    #[cfg(test)]
    scripted: Option<std::vec::IntoIter<(f64, u64)>>,
}

impl BudgetGuard {
    /// The shipped construction: a live clock and a live RSS sampler.
    #[must_use]
    pub fn start(scan_dir: Option<PathBuf>) -> Self {
        Self {
            started: Instant::now(),
            sampler: Some(ResourceSampler::start(Vec::new(), scan_dir.clone())),
            scan_dir,
            peak_rss: 0,
            #[cfg(test)]
            scripted: None,
        }
    }

    /// The injected-reading construction. A guard built this way starts no
    /// sampler thread and never reads the clock.
    #[cfg(test)]
    fn scripted(readings: Vec<(f64, u64)>) -> Self {
        Self {
            started: Instant::now(),
            sampler: None,
            scan_dir: None,
            peak_rss: 0,
            scripted: Some(readings.into_iter()),
        }
    }

    fn reading(&mut self) -> (f64, u64) {
        #[cfg(test)]
        if let Some(script) = self.scripted.as_mut() {
            // A script that runs out reports a crossed wall clock rather than
            // silently reverting to the live one, so a test cannot accidentally
            // measure itself.
            return script.next().unwrap_or((f64::INFINITY, 0));
        }
        let rss = self
            .sampler
            .as_ref()
            .map_or(0, |s| s.sample_peak_rss(&[], self.scan_dir.as_deref()));
        (self.started.elapsed().as_secs_f64(), rss)
    }

    /// One cell boundary. Returns the peak RSS seen so far, or the stop.
    ///
    /// # Errors
    /// The first boundary at or past either ceiling.
    pub fn check(&mut self) -> Result<u64, BudgetStop> {
        let (elapsed_s, rss) = self.reading();
        self.peak_rss = self.peak_rss.max(rss);
        budget_verdict(elapsed_s, self.peak_rss).map_or(Ok(self.peak_rss), |verdict| {
            Err(BudgetStop {
                verdict,
                elapsed_s,
                peak_rss_bytes: self.peak_rss,
            })
        })
    }

    #[must_use]
    pub const fn peak_rss_bytes(&self) -> u64 {
        self.peak_rss
    }

    /// Stops the sampler and returns the run's `(elapsed_s, peak_rss_bytes)`.
    /// The caller judges them with [`budget_verdict`] once more before writing,
    /// because the last cell's own cost lands after the last boundary check.
    ///
    /// # Errors
    /// A sampler thread that died: a peak measured over a partial window is not
    /// an attestation, so it refuses rather than reporting one.
    pub fn finish(mut self) -> LabResult<(f64, u64)> {
        let elapsed_s = self.started.elapsed().as_secs_f64();
        if let Some(sampler) = self.sampler.take() {
            let (peak, _) = sampler.stop(&[], self.scan_dir.as_deref())?;
            self.peak_rss = self.peak_rss.max(peak);
        }
        Ok((elapsed_s, self.peak_rss))
    }
}

/// The budget-enforced work loop of spec 5.1.1: one item at a time, both
/// ceilings sampled at every item boundary, and the first crossing stops the
/// whole run.
///
/// Both of the driver's passes - the coarse grid and each refinement round -
/// run through here, which is what makes an injected-reading test a test of the
/// enforcement rather than of an arithmetic helper that resembles it.
///
/// # Errors
/// A crossed ceiling, or whatever `work` refuses with.
pub fn budgeted<T, R>(
    guard: &mut BudgetGuard,
    items: impl IntoIterator<Item = T>,
    mut work: impl FnMut(T) -> LabResult<R>,
) -> LabResult<Vec<R>> {
    let mut done = Vec::new();
    for item in items {
        done.push(work(item)?);
        guard.check()?;
    }
    Ok(done)
}

/// Serializes the Stage A artifact atomically through a `.tmp` rename - and
/// refuses to serialize anything at all if either ceiling was crossed.
///
/// This is the last of the three places 5.1.1 names, and the one that makes the
/// rule enforceable rather than advisory: the verdict field can only carry
/// `arrival-admissible: <families>` or
/// `no-arrival-admissible-candidate-in-frozen-search-space`, neither of which a
/// stopped run may reach, so a stopped run must not leave a file behind.
///
/// # Errors
/// A crossed ceiling, or an I/O failure. Nothing is renamed into place in
/// either case.
pub fn write_artifact(
    out: &Path,
    artifact: &Value,
    elapsed_s: f64,
    peak_rss_bytes: u64,
) -> LabResult<()> {
    if let Some(verdict) = budget_verdict(elapsed_s, peak_rss_bytes) {
        return Err(BudgetStop {
            verdict,
            elapsed_s,
            peak_rss_bytes,
        }
        .into());
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = out.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(artifact)?)?;
    std::fs::rename(tmp, out)?;
    Ok(())
}

/// The six frozen protocol-12a parent-count bins, as half-open intervals over
/// the exact count. `MIN_MINUTES_CELL` and the bin edges are 12a's, not this
/// protocol's, and are never re-derived here.
pub const PARENT_COUNT_BINS: [(u32, u32); 6] = [
    (0, 0),
    (1, 64),
    (65, 256),
    (257, 1024),
    (1025, 4096),
    (4097, u32::MAX),
];

/// 12a section 3.3. A required bin is one whose pooled OBSERVED populated
/// minute count reaches this floor.
pub const MIN_MINUTES_CELL: u64 = 30;

/// 12a section 2.1's frozen hour set, the only hours where A1's conditional
/// limb applies.
pub const FAIL_HOURS_300: [u32; 3] = [19, 20, 23];

/// The name of a bin, in the artifact's own spelling.
#[must_use]
pub fn bin_name(n: u32) -> &'static str {
    match n {
        0 => "0",
        1..=64 => "1-64",
        65..=256 => "65-256",
        257..=1024 => "257-1024",
        1025..=4096 => "1025-4096",
        _ => "4097+",
    }
}

/// The per-hour distribution of the EXACT parent count over populated
/// minutes, as `(count, occurrences)` pairs sorted ascending by count.
///
/// Exact `n` is retained rather than binned: the six bins coarsen only the
/// support check and the reported diagnostics, never the loss.
pub type CountMarginal = BTreeMap<u32, Vec<(u32, u64)>>;

/// Marginalizes a protocol-12a `block1.hist` down to the parent-count axis.
///
/// The histogram rows carry the two range axes and both segment-label axes as
/// well; every one of them is summed out here, because Stage A may not claim
/// to evaluate anything that needs the price or book path.
///
/// # Errors
/// Refuses a row missing `hour`, `n` or `count`, rather than skipping it: a
/// silently dropped row would understate support, which is the one direction
/// that turns an inadmissible cell admissible.
pub fn parent_count_marginal(hist: &Value) -> LabResult<CountMarginal> {
    let rows = hist
        .as_array()
        .ok_or_else(|| LabError::refusal("block1.hist is not an array"))?;
    let mut acc: BTreeMap<u32, BTreeMap<u32, u64>> = BTreeMap::new();
    for row in rows {
        let hour = u32::try_from(
            row["hour"]
                .as_u64()
                .ok_or_else(|| LabError::refusal("block1.hist row without an integer hour"))?,
        )
        .map_err(|_| LabError::refusal("block1.hist hour out of range"))?;
        let n = u32::try_from(
            row["n"]
                .as_u64()
                .ok_or_else(|| LabError::refusal("block1.hist row without an integer n"))?,
        )
        .map_err(|_| LabError::refusal("block1.hist n out of range"))?;
        let count = row["count"]
            .as_u64()
            .ok_or_else(|| LabError::refusal("block1.hist row without an integer count"))?;
        *acc.entry(hour).or_default().entry(n).or_default() += count;
    }
    Ok(acc
        .into_iter()
        .map(|(hour, counts)| (hour, counts.into_iter().collect()))
        .collect())
}

/// Pooled populated-minute count per bin for one hour.
#[must_use]
pub fn bin_totals(marginal: &[(u32, u64)]) -> BTreeMap<&'static str, u64> {
    let mut totals: BTreeMap<&'static str, u64> = BTreeMap::new();
    for &(n, count) in marginal {
        *totals.entry(bin_name(n)).or_default() += count;
    }
    totals
}

/// The exact 1-Wasserstein distance between two empirical distributions of
/// `log1p(n)`, each given as `(count, occurrences)` pairs.
///
/// Computed from the sorted empirical CDFs with no binning, as
/// `integral |F - G| dx` over the shared support: walk the union of support
/// points in ascending order and accumulate the absolute CDF gap across each
/// interval between consecutive points.
///
/// `log1p` is frozen by section 9.3 because parent counts span three orders of
/// magnitude and an untransformed distance would be dominated by the busiest
/// hour. This is a RANKING device: it never stands as evidence that the raw
/// count distributions agree, which is A1's job.
///
/// Returns zero for two empty populations and `None` if exactly one side is
/// empty, since a distance to nothing is undefined rather than large.
#[must_use]
pub fn wasserstein_log1p(left: &[(u32, u64)], right: &[(u32, u64)]) -> Option<f64> {
    let left_total: u64 = left.iter().map(|&(_, c)| c).sum();
    let right_total: u64 = right.iter().map(|&(_, c)| c).sum();
    match (left_total, right_total) {
        (0, 0) => return Some(0.0),
        (0, _) | (_, 0) => return None,
        _ => {}
    }

    let mut support: Vec<u32> = left.iter().map(|&(n, _)| n).collect();
    support.extend(right.iter().map(|&(n, _)| n));
    support.sort_unstable();
    support.dedup();

    let cdf = |side: &[(u32, u64)], total: u64, upto: u32| -> f64 {
        let seen: u64 = side
            .iter()
            .filter(|&&(n, _)| n <= upto)
            .map(|&(_, c)| c)
            .sum();
        seen as f64 / total as f64
    };

    let mut distance = 0.0;
    for window in support.windows(2) {
        let (lo, hi) = (window[0], window[1]);
        let gap = f64::from(hi).ln_1p() - f64::from(lo).ln_1p();
        let left_cdf = cdf(left, left_total, lo);
        let right_cdf = cdf(right, right_total, lo);
        distance += (left_cdf - right_cdf).abs() * gap;
    }
    Some(distance)
}

/// Section 16's `linear(lo, hi, step)`.
///
/// Values are computed as `lo + i * step` from an integer index rather than by
/// accumulating `step`, so a long grid cannot drift off its stated literals.
#[must_use]
pub fn linear_grid(lo: f64, hi: f64, step: f64) -> Vec<f64> {
    let mut points = Vec::new();
    let mut index = 0_u32;
    loop {
        let value = f64::from(index).mul_add(step, lo);
        if value > hi * (1.0 + 1e-12) {
            break;
        }
        points.push(value);
        index += 1;
    }
    points
}

/// Section 16's `logk(lo, hi, k)`: the points `lo * 10^(j/k)` while they stay
/// at or below `hi`, with `hi` appended when the last generated point falls
/// short of it by more than the stated tolerance.
///
/// Each value is computed from the literal `lo`, `j` and `k` rather than
/// chained, for the same reason `linear_grid` indexes rather than accumulates.
#[must_use]
pub fn log_grid(lo: f64, hi: f64, k: u32) -> Vec<f64> {
    let mut points = Vec::new();
    let mut j = 0_u32;
    loop {
        let value = lo * 10_f64.powf(f64::from(j) / f64::from(k));
        if value > hi * (1.0 + 1e-12) {
            break;
        }
        points.push(value);
        j += 1;
    }
    if points.last().is_none_or(|&last| last < hi * (1.0 - 1e-12)) {
        points.push(hi);
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_marginal_sums_every_axis_except_the_parent_count() {
        // Two rows sharing an hour and a count but differing on the range and
        // label axes must pool: Stage A sees the count axis and nothing else.
        let hist = json!([
            {"hour": 19, "n": 5, "count": 2, "trade_range_ticks": 3,
             "quote_range_half_ticks": 1, "since_open_bin": "0-300",
             "until_close_bin": "1800+"},
            {"hour": 19, "n": 5, "count": 3, "trade_range_ticks": 9,
             "quote_range_half_ticks": 4, "since_open_bin": "300-1800",
             "until_close_bin": "0-300"},
            {"hour": 20, "n": 7, "count": 1, "trade_range_ticks": 2,
             "quote_range_half_ticks": 0, "since_open_bin": "1800+",
             "until_close_bin": "300-1800"}
        ]);
        let marginal = parent_count_marginal(&hist).expect("well-formed hist");
        assert_eq!(marginal[&19], vec![(5, 5)]);
        assert_eq!(marginal[&20], vec![(7, 1)]);
    }

    #[test]
    fn a_malformed_row_refuses_rather_than_being_skipped() {
        // Skipping would understate support, which is the direction that turns
        // an inadmissible cell admissible.
        let hist = json!([{"hour": 19, "count": 2}]);
        assert!(parent_count_marginal(&hist).is_err());
    }

    #[test]
    fn wasserstein_matches_a_hand_computed_distance() {
        // Two point masses: all weight at n = 0 against all weight at n = 3.
        // F - G is 1 across the whole interval, so the distance is exactly the
        // log1p gap, ln(4) - ln(1) = ln(4).
        let left = [(0_u32, 10_u64)];
        let right = [(3_u32, 7_u64)];
        let distance = wasserstein_log1p(&left, &right).expect("both populated");
        assert!((distance - 4_f64.ln()).abs() < 1e-12, "{distance}");
    }

    #[test]
    fn wasserstein_halves_when_half_the_mass_agrees() {
        // Half the mass sits on n = 0 for both sides; the other half is split
        // between 0 and 3. The CDF gap over [0, 3) is 1/2, so the distance is
        // half of ln(4).
        let left = [(0_u32, 2_u64)];
        let right = [(0_u32, 1_u64), (3_u32, 1_u64)];
        let distance = wasserstein_log1p(&left, &right).expect("both populated");
        assert!((distance - 4_f64.ln() / 2.0).abs() < 1e-12, "{distance}");
    }

    #[test]
    fn wasserstein_is_zero_for_identical_populations_and_symmetric() {
        let a = [(1_u32, 3_u64), (9_u32, 4_u64)];
        let b = [(1_u32, 6_u64), (9_u32, 8_u64)];
        // Same shape at twice the weight: a distance of zero, not a
        // weight-driven difference.
        assert_eq!(wasserstein_log1p(&a, &b), Some(0.0));
        let c = [(2_u32, 1_u64)];
        assert_eq!(wasserstein_log1p(&a, &c), wasserstein_log1p(&c, &a));
    }

    #[test]
    fn an_empty_side_is_undefined_rather_than_far() {
        assert_eq!(wasserstein_log1p(&[], &[]), Some(0.0));
        assert_eq!(wasserstein_log1p(&[(1, 1)], &[]), None);
        assert_eq!(wasserstein_log1p(&[], &[(1, 1)]), None);
    }

    #[test]
    fn the_frozen_grids_have_their_stated_point_counts() {
        // Section 16 states these counts explicitly, and two earlier revisions
        // of the spec got them wrong in opposite directions, so they are
        // pinned here against the rule rather than against a recollection.
        assert_eq!(log_grid(1e-6, 0.5, 3).len(), 19);
        assert_eq!(log_grid(10.0, 1000.0, 3).len(), 7);
        assert_eq!(log_grid(2.0, 200.0, 3).len(), 7);
        assert_eq!(log_grid(1.0, 3600.0, 3).len(), 12);
        assert_eq!(log_grid(2.0, 600.0, 3).len(), 9);
        assert_eq!(linear_grid(0.10, 0.60, 0.10).len(), 6);
        assert_eq!(linear_grid(0.2, 2.0, 0.2).len(), 10);
        assert_eq!(linear_grid(0.10, 0.85, 0.05).len(), 16);
    }

    #[test]
    fn the_probe_cell_is_the_domain_centre_and_not_a_tensor_corner() {
        // Brick A0 prices one cell per family and its reading is the whole
        // basis of the per-cell budget ruling, so it must be an interior
        // point. Indexing the FLATTENED coarse grid at its middle yields the
        // occupancy midpoint crossed with the first rate-ratio and the first
        // tau - a corner. Per axis it is brick K's transcript point.
        assert_eq!(
            probe_cell(Family::WallMmpp),
            Cell::WallMmpp {
                occupancy: 0.4,
                rate_ratio: 20.0,
                tau_s: 100.0,
            }
        );
        let grid = coarse_grid(Family::WallMmpp);
        assert_ne!(probe_cell(Family::WallMmpp), grid[grid.len() / 2]);
        let Cell::LogOuCox { tau_s, .. } = probe_cell(Family::LogOuCox) else {
            unreachable!()
        };
        assert!((tau_s - 100.0).abs() < 1e-9);
    }

    #[test]
    fn a_family_spells_itself_the_way_the_seam_does() {
        // The artifact's family keys are pasted into a preset's
        // `[instrument.generator.arrival]` table, where `eventmarkov` - what
        // a lowercased `Debug` produces - is not a variant.
        assert_eq!(Family::EventMarkov.as_str(), "event_markov");
        assert_eq!(Family::WallMmpp.as_str(), "wall_mmpp");
        assert_eq!(Family::LogOuCox.as_str(), "log_ou_cox");
        assert_eq!(Family::SelfExciting.as_str(), "self_exciting");
        for family in Family::ALL {
            assert_eq!(
                serde_json::to_value(family).expect("family serializes"),
                json!(family.as_str())
            );
        }
    }

    #[test]
    fn the_shipped_switch_rate_lies_on_its_grid() {
        // The incumbent point's w = 0.10 is exactly 1e-6 * 10^(15/3), which is
        // why section 16 counts the reference cell once rather than as an
        // extra off-grid evaluation.
        let grid = log_grid(1e-6, 0.5, 3);
        assert!(
            grid.iter().any(|&w| (w - 0.10).abs() < 1e-12),
            "0.10 missing from {grid:?}"
        );
    }

    #[test]
    fn a_log_grid_appends_its_upper_endpoint_only_when_it_falls_short() {
        // 10 .. 1000 lands exactly on its endpoint at j = 6 and must not
        // duplicate it; 1e-6 .. 0.5 stops at 0.4641589 and must append.
        let exact = log_grid(10.0, 1000.0, 3);
        assert!((exact[exact.len() - 1] - 1000.0).abs() < 1e-9);
        assert!((exact[exact.len() - 2] - 1000.0).abs() > 1.0);
        let appended = log_grid(1e-6, 0.5, 3);
        assert!((appended[appended.len() - 1] - 0.5).abs() < 1e-12);
        assert!((appended[appended.len() - 2] - 0.464_158_883_361_278).abs() < 1e-9);
    }

    #[test]
    fn bins_name_their_edges_the_way_the_artifact_spells_them() {
        assert_eq!(bin_name(0), "0");
        assert_eq!(bin_name(1), "1-64");
        assert_eq!(bin_name(64), "1-64");
        assert_eq!(bin_name(65), "65-256");
        assert_eq!(bin_name(256), "65-256");
        assert_eq!(bin_name(257), "257-1024");
        assert_eq!(bin_name(1024), "257-1024");
        assert_eq!(bin_name(1025), "1025-4096");
        assert_eq!(bin_name(4096), "1025-4096");
        assert_eq!(bin_name(4097), "4097+");
    }

    fn evaluated(
        lattice: Vec<u32>,
        admissible: bool,
        loss: Option<f64>,
        level: u8,
    ) -> EvaluatedCell {
        EvaluatedCell {
            verdict: CellVerdict {
                cell: cell_from_coordinates(Family::EventMarkov, &lattice),
                fitted_params: 1,
                a1: json!({}),
                a2: json!({}),
                a3: json!({}),
                a4: json!({}),
                admissible,
                loss,
                reported: json!({}),
                refusals: Vec::new(),
                cost_s: 0.0,
            },
            lattice,
            level,
            pass: if level == 0 { "coarse" } else { "refine" },
        }
    }

    #[test]
    fn the_refinement_is_two_rounds_over_a_lattice() {
        let mut cells = vec![
            evaluated(vec![0], true, Some(2.0), 0),
            evaluated(vec![4], false, None, 0),
            evaluated(vec![8], true, Some(1.0), 0),
            evaluated(vec![12], false, None, 0),
        ];
        let first = refinement_round(Family::EventMarkov, &cells, 1, 1);
        assert_eq!(first.candidates.len(), 1);
        assert_eq!(first.candidates[0].lattice, vec![6], "canonical tie-break");
        assert_eq!(first.unevaluated, 2);
        cells.push(evaluated(vec![6], false, None, 1));
        let second = refinement_round(Family::EventMarkov, &cells, 2, 40);
        assert_eq!(
            second
                .candidates
                .iter()
                .map(|c| &c.lattice)
                .collect::<Vec<_>>(),
            vec![&vec![7], &vec![10], &vec![2]]
        );
        assert!(second.candidates.iter().all(|c| c.level == 2));
    }

    #[test]
    fn the_refinement_subdivides_each_axis_transform() {
        let cells = vec![
            EvaluatedCell {
                verdict: CellVerdict {
                    cell: cell_from_coordinates(Family::WallMmpp, &[0, 0, 0]),
                    fitted_params: 3,
                    a1: json!({}),
                    a2: json!({}),
                    a3: json!({}),
                    a4: json!({}),
                    admissible: true,
                    loss: Some(1.0),
                    reported: json!({}),
                    refusals: vec![],
                    cost_s: 0.0,
                },
                lattice: vec![0, 0, 0],
                level: 0,
                pass: "coarse",
            },
            EvaluatedCell {
                verdict: CellVerdict {
                    cell: cell_from_coordinates(Family::WallMmpp, &[4, 0, 0]),
                    fitted_params: 3,
                    a1: json!({}),
                    a2: json!({}),
                    a3: json!({}),
                    a4: json!({}),
                    admissible: false,
                    loss: None,
                    reported: json!({}),
                    refusals: vec![],
                    cost_s: 0.0,
                },
                lattice: vec![4, 0, 0],
                level: 0,
                pass: "coarse",
            },
            EvaluatedCell {
                verdict: CellVerdict {
                    cell: cell_from_coordinates(Family::WallMmpp, &[0, 4, 0]),
                    fitted_params: 3,
                    a1: json!({}),
                    a2: json!({}),
                    a3: json!({}),
                    a4: json!({}),
                    admissible: false,
                    loss: None,
                    reported: json!({}),
                    refusals: vec![],
                    cost_s: 0.0,
                },
                lattice: vec![0, 4, 0],
                level: 0,
                pass: "coarse",
            },
        ];
        let round = refinement_round(Family::WallMmpp, &cells, 1, 10);
        let linear = round
            .candidates
            .iter()
            .find(|c| c.lattice == [2, 0, 0])
            .expect("linear midpoint");
        let logarithmic = round
            .candidates
            .iter()
            .find(|c| c.lattice == [0, 2, 0])
            .expect("log midpoint");
        let Cell::WallMmpp { occupancy, .. } = linear.cell else {
            unreachable!()
        };
        let Cell::WallMmpp { rate_ratio, .. } = logarithmic.cell else {
            unreachable!()
        };
        assert!((occupancy - 0.15).abs() < 1e-12);
        assert!((rate_ratio - (2.0_f64 * 2.0 * 10_f64.powf(1.0 / 3.0)).sqrt()).abs() < 1e-12);
    }

    #[test]
    fn disconnected_admissible_regions_are_reported_separately() {
        let cells = vec![
            evaluated(vec![0], true, Some(1.0), 0),
            evaluated(vec![4], false, None, 0),
            evaluated(vec![8], true, Some(2.0), 0),
        ];
        assert_eq!(admissible_regions(&cells), vec![vec![0], vec![2]]);
    }

    // -- The projection, spec 3.3 -------------------------------------------

    /// A scripted parent stream. Everything family-specific lives behind
    /// `ParentSource`, so the projection's lifecycle properties are exercised
    /// on hand-placed parents instead of on a month of generated tape.
    struct Scripted {
        parents: std::vec::IntoIter<Result<ParentSummary, ScreenRefusal>>,
        /// What a real faulted `GeneratedSource` does: hand back the last
        /// summary forever.
        repeat_last: bool,
        last: Option<ParentSummary>,
    }

    impl Scripted {
        fn of(parents: Vec<(u64, u32)>) -> Self {
            Self {
                parents: parents
                    .into_iter()
                    .map(|(ts, n)| {
                        Ok(ParentSummary {
                            parent_ts_ns: ts,
                            child_count: n,
                            child_stride_ns: 1_000,
                        })
                    })
                    .collect::<Vec<_>>()
                    .into_iter(),
                repeat_last: false,
                last: None,
            }
        }
        fn stalling(parents: Vec<(u64, u32)>) -> Self {
            Self {
                repeat_last: true,
                ..Self::of(parents)
            }
        }
    }

    impl ParentSource for Scripted {
        fn next(&mut self) -> Result<ParentSummary, ScreenRefusal> {
            match self.parents.next() {
                Some(Ok(p)) => {
                    self.last = Some(p);
                    Ok(p)
                }
                Some(Err(e)) => Err(e),
                // Off the end of the script the stream either stalls (the
                // family-1 fault) or reports one terminal lookahead parent far
                // past the window, which closes the walk.
                None if self.repeat_last => Ok(self.last.expect("a stalling script has a parent")),
                None => Ok(ParentSummary {
                    parent_ts_ns: u64::MAX,
                    child_count: 0,
                    child_stride_ns: 1_000,
                }),
            }
        }
    }

    /// UTC local time is the calendar's local time at offset zero, which is
    /// what these projection pins use: the session opens at local 17:00, so a
    /// session boundary is a known instant rather than one read off MNQ's
    /// offset.
    const DAY_NS: u64 = 86_400_000_000_000;
    const MINUTE_NS: u64 = 60_000_000_000;
    /// `SESSION_OPEN_LOCAL_MIN` is 17:00.
    const OPEN_NS: u64 = 17 * 60 * MINUTE_NS;

    fn minute_counts(session: &Value, hour: u64) -> BTreeMap<u32, u64> {
        let mut out = BTreeMap::new();
        for row in session["block1_hist"].as_array().into_iter().flatten() {
            if row["hour"].as_u64() == Some(hour) {
                *out.entry(row["n"].as_u64().expect("n") as u32).or_default() +=
                    row["count"].as_u64().expect("count");
            }
        }
        out
    }

    #[test]
    fn the_screen_projection_places_a_straddling_burst_in_two_minutes() {
        // One parent, 400 children at a 1 us stride, starting 300 us before a
        // minute boundary: 300 children land in the first minute and 100 in
        // the second. Spec 2.5 - the parent counts in the minute of its FIRST
        // child, so the second minute is populated with N = 0.
        let day = 20_000_u64;
        let boundary = day * DAY_NS + OPEN_NS + 90 * MINUTE_NS;
        let start = day * DAY_NS + OPEN_NS;
        let end = start + 6 * 60 * MINUTE_NS;
        let mut source = Scripted::of(vec![(boundary - 300_000, 400)]);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("a clean projection");
        assert_eq!(projected.sessions.len(), 1);
        let hour = (boundary - 300_000) / (60 * MINUTE_NS) % 24;
        let counts = minute_counts(&projected.sessions[0], hour);
        assert_eq!(
            counts,
            BTreeMap::from([(0_u32, 1_u64), (1, 1)]),
            "the burst populates two minutes and the parent only counts in the first"
        );
        assert_eq!(projected.parents, 1);
        assert_eq!(projected.prints, 400, "child multiplicity stays exact");
    }

    /// One parent with an arbitrary child stride. The shipped stride is
    /// `INTRA_EVENT_STEP_NS`, one microsecond, which is far too short to reach
    /// across a calendar boundary; the lifecycle rule the projection has to get
    /// right is nonetheless a rule about crossing one, so the stride is
    /// stretched to isolate it. Nothing family-specific changes: the projection
    /// reads `child_stride_ns` off the summary either way.
    fn one_parent(parent_ts_ns: u64, child_count: u32, child_stride_ns: u64) -> Scripted {
        Scripted {
            parents: vec![Ok(ParentSummary {
                parent_ts_ns,
                child_count,
                child_stride_ns,
            })]
            .into_iter(),
            repeat_last: false,
            last: None,
        }
    }

    #[test]
    fn child_bounds_clip_start_and_exclude_end_exactly() {
        let day = 20_000_u64;
        let open = day * DAY_NS + OPEN_NS;
        let start = open + 2 * MINUTE_NS;
        let end = start + 3 * MINUTE_NS;
        // Children at open and open + 1 minute are before the window. The
        // next three are measured. The sixth is exactly at `end` and is not.
        let mut source = one_parent(open, 6, MINUTE_NS);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("a clean projection");
        assert_eq!(projected.parents, 1, "the surviving child opens its parent");
        assert_eq!(projected.prints, 3);
        assert_eq!(
            minute_counts(&projected.sessions[0], 17),
            BTreeMap::from([(0, 3)]),
            "the original parent is before start, so only three populated minutes remain"
        );
    }

    #[test]
    fn a_parent_with_no_in_window_child_is_not_projected() {
        let day = 20_000_u64;
        let open = day * DAY_NS + OPEN_NS;
        let start = open + 2 * MINUTE_NS;
        let end = start + 3 * MINUTE_NS;
        let mut source = one_parent(open, 2, MINUTE_NS);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("a clean projection");
        assert_eq!(projected.parents, 0);
        assert_eq!(projected.prints, 0);
        assert!(projected.sessions.is_empty());
        assert!(projected.realized_mean_gap_s.is_none());
    }

    #[test]
    fn arbitrary_strides_preserve_non_contiguous_populated_minutes() {
        let day = 20_000_u64;
        let start = day * DAY_NS + OPEN_NS;
        let end = start + 10 * MINUTE_NS;
        let mut source = one_parent(start, 4, 3 * MINUTE_NS);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("a clean projection");
        assert_eq!(projected.parents, 1);
        assert_eq!(projected.prints, 4);
        assert_eq!(
            minute_counts(&projected.sessions[0], 17),
            BTreeMap::from([(0, 3), (1, 1)]),
            "minutes 0, 3, 6 and 9 are populated without filling the gaps"
        );
    }

    #[test]
    fn a_surviving_child_does_not_redefine_a_segmentless_parent_timestamp() {
        let day = 20_000_u64;
        let halt = (day + 1) * DAY_NS + (15 * 60 + 20) * MINUTE_NS;
        let reopen = halt + 10 * MINUTE_NS;
        assert!(session_segment_at(halt, 0).is_none());
        assert!(session_segment_at(reopen, 0).is_some());
        let mut source = one_parent(halt, 2, 10 * MINUTE_NS);
        let stop = project_stream(&mut source, reopen, reopen + MINUTE_NS, 0, 1)
            .err()
            .expect("the original parent instant is segmentless");
        let ProjectStop::Lab(error) = stop else {
            panic!("a segmentless parent is a harness failure, not the one typed child refusal");
        };
        assert!(error.to_string().contains("no open segment"));
    }

    #[test]
    fn a_burst_straddling_a_session_boundary_files_its_parent_in_the_old_session() {
        // MNQ's session closes at local 16:00 and the next opens at 17:00, so
        // the last in-segment minute before a session boundary is 15:59. This
        // parent's first child sits there and its second lands after the open.
        //
        // Spec 3.3 step 4: the rotation closes the OPEN PARENT before it closes
        // the session, so the parent is written into the session its first
        // child fell in. Draft 1's "children then parent" order would rotate
        // first and then file a previous-session parent into the new
        // accumulator, tripping `close_open_parent`'s rotation invariant - a
        // refusal, so a clean projection here IS the proof of the order.
        let day = 20_000_u64;
        let open = day * DAY_NS + OPEN_NS;
        let first_child = day * DAY_NS + (15 * 60 + 59) * MINUTE_NS;
        assert_eq!(
            session_segment_at(first_child, 0)
                .expect("in the post-halt segment")
                .segment,
            "post_halt"
        );
        let start = day * DAY_NS + 15 * 60 * MINUTE_NS + 30 * MINUTE_NS;
        let end = open + 60 * MINUTE_NS;
        let mut source = one_parent(first_child, 2, 62 * MINUTE_NS);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("no rotation refusal");
        assert_eq!(projected.sessions.len(), 2, "the walk crossed one open");
        assert_eq!(
            projected.projection,
            ScreenReduced::from_sessions(&projected.sessions)
                .expect("the legacy sessions reduce cleanly"),
            "the direct projection must merge sessions exactly like the legacy JSON path"
        );
        assert_ne!(
            session_segment_at(first_child, 0)
                .expect("a segment")
                .session_start_ns,
            session_segment_at(open, 0)
                .expect("a segment")
                .session_start_ns,
            "the fixture must actually straddle a session boundary"
        );
        let old_hour = first_child / (60 * MINUTE_NS) % 24;
        assert_eq!(
            minute_counts(&projected.sessions[0], old_hour)
                .get(&1)
                .copied(),
            Some(1),
            "the parent belongs to the session its first child fell in"
        );
        let new_hour = (first_child + 62 * MINUTE_NS) / (60 * MINUTE_NS) % 24;
        assert!(
            !minute_counts(&projected.sessions[1], new_hour).contains_key(&1),
            "the previous session's parent was not refiled into the new one"
        );
        assert_eq!(
            minute_counts(&projected.sessions[1], new_hour)
                .get(&0)
                .copied(),
            Some(1),
            "the trailing child populates a minute with no parent of its own"
        );
        assert_eq!(projected.parents, 1);
    }

    #[test]
    fn a_child_inside_a_closed_halt_is_the_typed_cell_refusal() {
        // The 15:15 to 15:30 local halt maps to no segment. Spec 3.3 step 4b:
        // such a child is PUSHED into the currently open session, exactly as
        // `GeneratedAcc::push_trade` pushes it, and `block1` refuses the minute
        // at close time if it must. A projection stricter than the shipped
        // accumulator cannot pass the layer-1 oracle, so the pin is on WHICH
        // refusal comes back: the accumulator's own close-time one, naming the
        // minute, and never a `projection` A4 refusal at push time.
        let day = 20_000_u64;
        let open = day * DAY_NS + OPEN_NS;
        // The halt opens at local 15:15, so a parent at 15:14 with a 1 min
        // stride has its first child in the overnight segment and its second
        // inside the halt.
        let parent = (day + 1) * DAY_NS + (15 * 60 + 14) * MINUTE_NS;
        let halt = parent + MINUTE_NS;
        assert!(
            session_segment_at(parent, 0).is_some() && session_segment_at(halt, 0).is_none(),
            "the fixture must straddle the halt boundary"
        );
        // The control: the same burst an hour earlier, wholly inside the
        // overnight segment, projects cleanly and its children are counted.
        let inside = parent - 60 * MINUTE_NS;
        let mut clean = one_parent(inside, 2, MINUTE_NS);
        let projected =
            project_stream(&mut clean, open, halt + MINUTE_NS, 0, 1).expect("a clean projection");
        assert_eq!(
            minute_counts(&projected.sessions[0], inside / (60 * MINUTE_NS) % 24)
                .get(&1)
                .copied(),
            Some(1)
        );

        let mut source = one_parent(parent, 2, MINUTE_NS);
        let stop = project_stream(&mut source, open, halt + MINUTE_NS, 0, 1)
            .err()
            .expect("block1 refuses a populated minute with no segment");
        let ProjectStop::Refused(refusal) = stop else {
            panic!("the closed-halt child must refuse the cell");
        };
        assert_eq!(
            refusal.variant,
            "projected_child_inside_closed_halt_segment"
        );
        assert_eq!(refusal.clock_ns, halt);
    }

    #[test]
    fn a_family_one_walk_that_stalls_aborts_instead_of_looping() {
        // Spec 3.3 step 3's termination guard. A refusal is now reported by
        // both walk arms, so a stall no longer means "the kernel refused and
        // could not say so"; the guard remains for a walk that advances neither
        // its timestamp nor its verdict, and the projection refuses on the
        // first non-advancing timestamp rather than hanging for eight hours.
        let day = 20_000_u64;
        let start = day * DAY_NS + OPEN_NS;
        let mut source = Scripted::stalling(vec![(start + MINUTE_NS, 1)]);
        let stop = project_stream(&mut source, start, start + 60 * MINUTE_NS, 0, 1)
            .err()
            .expect("a stalled walk refuses");
        let ProjectStop::Lab(error) = stop else {
            panic!("a stall is a harness failure and must abort the run");
        };
        assert!(error.to_string().contains("stalled"));
    }

    #[test]
    fn the_mean_gap_counts_measured_parents_only() {
        // Spec 3.3 step 6. Burn-in parents are outside [start, end) and the
        // terminal lookahead parent is never projected, so the realized gap is
        // (last measured - first measured) / (measured - 1).
        let day = 20_000_u64;
        let start = day * DAY_NS + OPEN_NS;
        let end = start + 60 * MINUTE_NS;
        let mut source = Scripted::of(vec![
            (start - 10 * MINUTE_NS, 1), // burn-in
            (start - MINUTE_NS, 1),      // burn-in
            (start, 1),
            (start + 2 * MINUTE_NS, 1),
            (start + 6 * MINUTE_NS, 1),
            (end + MINUTE_NS, 1), // the terminal lookahead
        ]);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("a clean projection");
        assert_eq!(
            projected.parents, 3,
            "burn-in and lookahead are not measured"
        );
        assert!(
            projected
                .realized_mean_gap_s
                .is_some_and(|gap| (gap - 180.0).abs() < 1e-9),
            "{:?}",
            projected.realized_mean_gap_s
        );

        // Fewer than two measured parents leaves the gap UNMEASURED rather
        // than substituting a number for it. The amended A4 has no mean-gap
        // limb, so it is not a refusal either.
        let mut lonely = Scripted::of(vec![(start, 1), (end + MINUTE_NS, 1)]);
        let one = project_stream(&mut lonely, start, end, 0, 1).expect("a clean projection");
        assert_eq!(one.parents, 1);
        assert!(one.realized_mean_gap_s.is_none());
    }

    #[test]
    fn a_projection_gap_refuses_rather_than_dropping_a_boundary_minute() {
        // A MEASURED parent that maps to no open segment - here, one inside the
        // halt - refuses the cell. Spec 3.3 step 4e and section 8: an
        // inconvenient boundary parent is never dropped.
        let day = 20_000_u64;
        let halt = (day + 1) * DAY_NS + (15 * 60 + 20) * MINUTE_NS;
        assert!(session_segment_at(halt, 0).is_none());
        let start = day * DAY_NS + OPEN_NS;
        let mut source = Scripted::of(vec![(halt, 1)]);
        let stop = project_stream(&mut source, start, halt + MINUTE_NS, 0, 1)
            .err()
            .expect("a segment-less measured parent refuses");
        let ProjectStop::Refused(refusal) = stop else {
            panic!("a projection gap is a cell refusal");
        };
        assert_eq!(
            refusal.variant,
            "projected_child_inside_closed_halt_segment"
        );
        assert!(
            refusal.detail.contains("projected child"),
            "{}",
            refusal.detail
        );
    }

    #[test]
    fn parallel_seed_scheduling_matches_serial_cell_reduction() {
        let mut ctx = ScreenContext::over(observed_sessions()).expect("a screen context");
        let calendar = ctx.profile.calendar.as_ref().expect("the MNQ calendar");
        let mut start = 20_000 * DAY_NS + OPEN_NS + MINUTE_NS;
        while !calendar.is_open(start) {
            start += DAY_NS;
        }
        ctx.binding = GeneratedBinding {
            window_start_ns: start,
            window_length_ns: 30 * MINUTE_NS,
            burn_in: "0s".into(),
        };
        let cell = Cell::WallMmpp {
            occupancy: 0.3,
            rate_ratio: 10.0,
            tau_s: 60.0,
        };
        let serial =
            evaluate_cell_with_work(&ctx, &cell, &[201, 202]).expect("serial cell evaluation");
        let projection = ctx.parallel_projection().expect("worker-safe context");
        let scheduled = [ScheduledCell {
            cell,
            seeds: vec![201, 202],
        }];
        let one_worker = evaluate_cells_parallel(&ctx, &projection, &scheduled, 1, None)
            .expect("one-worker evaluation")
            .pop()
            .expect("one result");
        let four_workers = evaluate_cells_parallel(&ctx, &projection, &scheduled, 4, None)
            .expect("four-worker evaluation")
            .pop()
            .expect("one result");

        let normalize = |mut verdict: CellVerdict| {
            verdict.cost_s = 0.0;
            serde_json::to_value(verdict).expect("serialized verdict")
        };
        assert_eq!(
            normalize(serial.verdict.clone()),
            normalize(one_worker.verdict.clone())
        );
        assert_eq!(
            normalize(one_worker.verdict.clone()),
            normalize(four_workers.verdict.clone())
        );
        assert_eq!(serial.parents, one_worker.parents);
        assert_eq!(one_worker.parents, four_workers.parents);
        assert_eq!(serial.prints, one_worker.prints);
        assert_eq!(one_worker.prints, four_workers.prints);

        let ordered = [
            ScheduledCell {
                cell: scheduled[0].cell.clone(),
                seeds: vec![201],
            },
            ScheduledCell {
                cell: Cell::EventMarkov { switch_rate: 0.1 },
                seeds: vec![201],
            },
        ];
        let ordered_results = evaluate_cells_parallel(&ctx, &projection, &ordered, 4, None)
            .expect("ordered parallel evaluation");
        assert_eq!(ordered_results[0].verdict.cell, ordered[0].cell);
        assert_eq!(ordered_results[1].verdict.cell, ordered[1].cell);

        let mut guard = BudgetGuard::scripted(vec![(STAGE_A_BUDGET_S, 0)]);
        let Err(error) =
            evaluate_cells_parallel(&ctx, &projection, &scheduled, 1, Some(&mut guard))
        else {
            panic!("a crossed scheduler budget must stop the batch");
        };
        assert!(
            error.to_string().contains("stage-a-budget-exceeded"),
            "{error}"
        );
    }

    // -- The conditions and the loss, spec 3.4 and 3.5 ----------------------

    fn measure12a() -> Value {
        serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json"))
            .expect("the committed 12a artifact")
    }

    #[test]
    fn the_a3_gated_hours_match_the_committed_artifact() {
        let projection = ScreenReduced::from_sessions(&observed_sessions())
            .expect("committed observed projection");
        let derived: Vec<i64> = (0_i64..24)
            .filter(|hour| {
                projection
                    .windows
                    .get(&u32::try_from(*hour).expect("hour fits u32"))
                    .and_then(|windows| windows.get(&1))
                    .is_some_and(|window| window.zeros >= MIN_ZERO_WINDOWS)
            })
            .collect();
        assert_eq!(derived, A3_GATED_HOURS);
        assert_eq!(
            (0_i64..24)
                .filter(|hour| !derived.contains(hour)
                    && projection
                        .windows
                        .contains_key(&u32::try_from(*hour).expect("hour fits u32")))
                .collect::<Vec<_>>(),
            vec![14, 15, 16]
        );
    }

    /// The committed OBSERVED session records, reduced to the three keys a
    /// ScreenSession carries. Judging hand-rolled block records would prove
    /// nothing about the shapes a real walk produces.
    fn observed_sessions() -> Vec<Value> {
        measure12a()["observed"]["per_session"]
            .as_array()
            .expect("per_session")
            .iter()
            .map(|s| {
                json!({"session_date":s["session_date"],"block1_hist":s["block1_hist"],
                       "block2":s["block2"]})
            })
            .collect()
    }

    /// A `SeedWalk` whose generated side IS the observed side: every ratio is
    /// exactly 1 and every distance exactly 0, so it is admissible by
    /// construction and each test can break one condition on purpose.
    fn perfect_walk(ctx: &ScreenContext, seed: u64, sessions: Vec<Value>) -> SeedWalk {
        let projection = ScreenReduced::from_sessions(&sessions).expect("typed projection");
        SeedWalk {
            seed,
            projection,
            sessions,
            parents: 2,
            prints: 0,
            realized_mean_gap_s: Some(ctx.profile.scalars.mean_event_duration_s),
            refusal: None,
            cost_s: 0.0,
        }
    }

    fn assert_typed_verdict_matches_legacy(ctx: &ScreenContext, cell: &Cell, walks: &[SeedWalk]) {
        let typed = verdict_from_walks(ctx, cell, walks).expect("typed verdict");
        let _legacy = verdict_from_walks_legacy(ctx, cell, walks).expect("legacy verdict");
        assert!(typed.a2["level"]["per_seed"].is_array());
        assert!(typed.a2["shape"]["per_hour"].is_array());
        assert!(typed.a3["gated"].is_array());
        assert!(typed.a3["per_seed_raw"].is_array());
    }

    #[test]
    fn typed_verdict_matches_the_json_oracle_across_gate_outcomes() {
        let observed = observed_sessions();
        let ctx = ScreenContext::over(observed.clone()).expect("a screen context");
        let cell = Cell::WallMmpp {
            occupancy: 0.3,
            rate_ratio: 20.0,
            tau_s: 60.0,
        };
        let perfect = perfect_walk(&ctx, 201, observed.clone());
        assert_typed_verdict_matches_legacy(&ctx, &cell, std::slice::from_ref(&perfect));

        let mut holed = observed.clone();
        for session in &mut holed {
            let rows = session["block1_hist"].as_array_mut().expect("block1 rows");
            rows.retain(|row| row["hour"].as_u64() != Some(19));
        }
        let holed = perfect_walk(&ctx, 202, holed);
        assert_typed_verdict_matches_legacy(&ctx, &cell, &[holed]);

        let mut distorted = observed;
        for session in &mut distorted {
            if let Some(cell) = session["block2"]
                .get_mut("20")
                .and_then(|hour| hour.get_mut("1"))
            {
                cell["zero_windows"] = json!(0);
            }
            if let Some(cell) = session["block2"]
                .get_mut("20")
                .and_then(|hour| hour.get_mut("60"))
            {
                let scheduled = cell["scheduled_windows"].as_u64().expect("scheduled");
                cell["scheduled_windows"] = json!(scheduled * 2);
            }
        }
        let distorted = perfect_walk(&ctx, 203, distorted);
        assert_typed_verdict_matches_legacy(&ctx, &cell, &[distorted]);

        let mut gap = perfect;
        gap.realized_mean_gap_s = gap.realized_mean_gap_s.map(|seconds| seconds * 2.0);
        assert_typed_verdict_matches_legacy(&ctx, &cell, &[gap]);
    }

    #[test]
    fn a1_is_the_conjunction_of_the_two_frozen_12a_rules() {
        let observed = observed_sessions();
        let ctx = ScreenContext::over(observed.clone()).expect("a screen context");
        let cell = Cell::EventMarkov { switch_rate: 0.1 };
        let base = vec![perfect_walk(&ctx, 201, observed.clone())];
        assert!(
            verdict_from_walks(&ctx, &cell, &base)
                .expect("a verdict")
                .a1["passed"]
                == true,
            "the observed side against itself satisfies both limbs"
        );

        // Limb (a): a hole at an observed-positive bin. Dropping every row of
        // one hour leaves the observed shares unsupported there.
        let holed: Vec<Value> = observed
            .iter()
            .map(|s| {
                let mut s = s.clone();
                s["block1_hist"] = Value::Array(
                    s["block1_hist"]
                        .as_array()
                        .expect("hist")
                        .iter()
                        .filter(|r| r["hour"].as_u64() != Some(19))
                        .cloned()
                        .collect(),
                );
                s
            })
            .collect();
        let verdict =
            verdict_from_walks(&ctx, &cell, &[perfect_walk(&ctx, 201, holed)]).expect("a verdict");
        assert!(verdict.a1["passed"] == false, "limb (a) missed the hole");
        assert!(!verdict.admissible);
        assert!(verdict.loss.is_none(), "an inadmissible cell is not ranked");

        // Limb (b): a required FAIL_HOURS_300 bin held at 29 populated minutes
        // fails, 30 passes. `conditional_adequacy_bins` counts pooled generated
        // minutes against MIN_MINUTES_CELL, so the pin is on the boundary.
        let required = required_bin(&ctx, &observed);
        for (minutes, expected) in [(MIN_MINUTES_CELL - 1, false), (MIN_MINUTES_CELL, true)] {
            let thinned = thin_bin_to(&observed, required.0, required.1, minutes);
            let verdict = verdict_from_walks(&ctx, &cell, &[perfect_walk(&ctx, 201, thinned)])
                .expect("a verdict");
            assert_eq!(
                verdict.a1["passed"] == true,
                expected,
                "hour {} bin {} at {minutes} minutes",
                required.0,
                required.1
            );
        }
    }

    /// The first `(hour, exact count)` in a `FAIL_HOURS_300` hour whose bin is
    /// REQUIRED - pooled observed populated minutes at or above
    /// `MIN_MINUTES_CELL` - and whose exact count is the only one in its bin,
    /// so thinning it thins the whole bin.
    fn required_bin(ctx: &ScreenContext, observed: &[Value]) -> (u32, u32) {
        let generated = ObsContext::new(observed.to_vec());
        let bins = conditional_adequacy_bins(&ctx.observed, std::slice::from_ref(&generated));
        for bin in &bins {
            if !bin.required || !FAIL_HOURS_300.contains(&(bin.hour as u32)) {
                continue;
            }
            let hour = bin.hour as u32;
            let counts: Vec<u32> = ctx.observed_marginal[&hour]
                .iter()
                .filter(|(n, _)| bin_name(*n) == bin.bin_name)
                .map(|(n, _)| *n)
                .collect();
            if let Some(&n) = counts.first()
                && counts.len() == 1
            {
                return (hour, n);
            }
        }
        // Every required bin pools several exact counts: fall back to the
        // whole bin, thinned count by count, which the helper below handles.
        let bin = bins
            .iter()
            .find(|b| b.required && FAIL_HOURS_300.contains(&(b.hour as u32)))
            .expect("a required bin in a FAIL_HOURS_300 hour");
        (
            bin.hour as u32,
            PARENT_COUNT_BINS
                .iter()
                .find(|&&(lo, _)| bin_name(lo) == bin.bin_name)
                .expect("a named bin")
                .0,
        )
    }

    /// Rewrites the generated side so the bin holding `count` in `hour` carries
    /// exactly `minutes` populated minutes in total, pooled over sessions.
    fn thin_bin_to(observed: &[Value], hour: u32, count: u32, minutes: u64) -> Vec<Value> {
        let bin = bin_name(count);
        let mut left = minutes;
        observed
            .iter()
            .map(|s| {
                let mut s = s.clone();
                let rows: Vec<Value> = s["block1_hist"]
                    .as_array()
                    .expect("hist")
                    .iter()
                    .filter_map(|r| {
                        let in_cell = r["hour"].as_u64() == Some(u64::from(hour))
                            && bin_name(r["n"].as_u64().expect("n") as u32) == bin;
                        if !in_cell {
                            return Some(r.clone());
                        }
                        let have = r["count"].as_u64().expect("count");
                        let keep = have.min(left);
                        left -= keep;
                        (keep > 0).then(|| {
                            let mut r = r.clone();
                            r["count"] = json!(keep);
                            r
                        })
                    })
                    .collect();
                s["block1_hist"] = Value::Array(rows);
                s
            })
            .collect()
    }

    #[test]
    fn the_screen_judges_the_same_hour_set_as_gates_b6_and_b7() {
        // Spec 3.4's most consequential decision: A2 and A3 evaluate the
        // calendar-EXPOSED hour set, exactly as the landed B6 and B7 do. A
        // screen that judged all 24 hours would be strictly stronger than the
        // gate it claims to be contained by, and would reject a cell Stage B
        // would accept - so hour 21, MNQ's daily break, must appear in no row.
        let observed = observed_sessions();
        let ctx = ScreenContext::over(observed.clone()).expect("a screen context");
        let profile = mogwai_venue::config::profile_from_preset("MNQ").expect("the MNQ preset");
        assert_eq!(ctx.gate_hours(), gate_hours(&profile).expect("gate hours"));
        assert!(!ctx.gate_hours().contains(&21));
        assert_eq!(ctx.gate_hours().len(), 23);

        // A generated side wrecked at hour 21 alone stays admissible, because
        // hour 21 is not judged.
        let wrecked: Vec<Value> = observed
            .iter()
            .map(|s| {
                let mut s = s.clone();
                if let Some(cell) = s["block2"]
                    .get_mut("21")
                    .and_then(|h| h.get_mut("60"))
                    .and_then(Value::as_object_mut)
                {
                    cell.insert("scheduled_windows".into(), json!(1_000_000));
                }
                s
            })
            .collect();
        let cell = Cell::EventMarkov { switch_rate: 0.1 };
        let verdict = verdict_from_walks(&ctx, &cell, &[perfect_walk(&ctx, 201, wrecked)])
            .expect("a verdict");
        assert!(
            verdict.admissible,
            "an unexposed hour cannot decide a cell: {:?}",
            verdict.a2
        );
        for rows in [
            &verdict.a2["shape"]["per_hour"],
            &verdict.a3["per_seed_raw"],
        ] {
            assert_eq!(rows.as_array().expect("rows").len(), 23);
            assert!(
                rows.as_array()
                    .expect("rows")
                    .iter()
                    .all(|r| r["hour"] != 21)
            );
        }
    }

    #[test]
    fn an_arrival_refusal_records_the_cell_and_keeps_it_out_of_the_loss() {
        let observed = observed_sessions();
        let ctx = ScreenContext::over(observed.clone()).expect("a screen context");
        let cell = Cell::WallMmpp {
            occupancy: 0.3,
            rate_ratio: 20.0,
            tau_s: 60.0,
        };
        let refused = SeedWalk {
            seed: 202,
            projection: ScreenReduced::default(),
            sessions: Vec::new(),
            parents: 0,
            prints: 0,
            realized_mean_gap_s: None,
            refusal: Some(ScreenRefusal {
                variant: "intensity_ceiling".into(),
                clock_ns: 1_234_567,
                detail: "IntensityCeiling".into(),
                family: None,
                canonical_params: None,
                seed: None,
            }),
            cost_s: 0.0,
        };
        let walks = vec![perfect_walk(&ctx, 201, observed), refused];
        let verdict = verdict_from_walks(&ctx, &cell, &walks).expect("a verdict");
        assert!(!verdict.admissible);
        assert!(verdict.a4["passed"] == false);
        assert!(
            verdict.loss.is_none(),
            "a refused cell never enters a ranking"
        );
        let recorded = &verdict.a4["refusal"]["refusal"];
        assert_eq!(recorded["variant"], json!("intensity_ceiling"));
        assert_eq!(recorded["clock_ns"], json!(1_234_567));
        assert!(
            !verdict.refusals.is_empty(),
            "the refusal is in the artifact"
        );
    }

    #[test]
    fn the_retired_mean_gap_limb_cannot_remove_a_refinement_cell() {
        let observed = observed_sessions();
        let ctx = ScreenContext::over(observed.clone()).expect("a screen context");
        let cell = Cell::EventMarkov { switch_rate: 0.1 };
        let coarse: Vec<SeedWalk> = [201, 202]
            .into_iter()
            .map(|s| perfect_walk(&ctx, s, observed.clone()))
            .collect();
        let coarse_verdict = verdict_from_walks(&ctx, &cell, &coarse).expect("a verdict");
        assert!(coarse_verdict.admissible);
        assert!(coarse_verdict.loss.is_some());

        let mut four = coarse;
        for seed in [203_u64, 204] {
            let mut bad = perfect_walk(&ctx, seed, observed.clone());
            bad.realized_mean_gap_s = Some(ctx.profile.scalars.mean_event_duration_s * 2.0);
            four.push(bad);
        }
        let refined = verdict_from_walks(&ctx, &cell, &four).expect("a verdict");
        assert!(
            refined.admissible,
            "the amended A4 ignores mean-gap diagnostics"
        );
    }

    #[test]
    fn the_loss_refuses_an_hour_it_has_no_rule_for_rather_than_renormalizing() {
        // Spec 3.5 defines the weight of every observed-populated hour and no
        // rule for an hour whose distance is undefined. A1 limb (a) makes that
        // state unreachable for an admissible cell; this pins that if it ever
        // becomes reachable the run stops instead of quietly ranking on a
        // renormalized subset of the hours.
        let observed: CountMarginal =
            BTreeMap::from([(19, vec![(0_u32, 10_u64), (5, 20)]), (20, vec![(3, 6)])]);
        assert_eq!(
            composition_loss(&observed, &observed).expect("a defined loss"),
            Some(0.0)
        );
        let holed: CountMarginal = BTreeMap::from([(19, vec![(0_u32, 10_u64), (5, 20)])]);
        let error = composition_loss(&observed, &holed)
            .expect_err("an hour with observed mass and no generated mass has no defined weight")
            .to_string();
        assert!(error.contains("hour 20"), "{error}");
        // An observed hour carrying no mass at all is not that state: both
        // sides are empty, the distance is zero, and the weight is zero.
        let empty: CountMarginal = BTreeMap::from([(19, vec![(0_u32, 0_u64)])]);
        assert_eq!(
            composition_loss(&empty, &BTreeMap::new()).expect("two empty sides are not a defect"),
            None
        );
    }

    // -- The budget, spec 5.1.1 ---------------------------------------------

    #[test]
    fn the_total_budget_and_the_rss_ceiling_stop_the_run_without_an_artifact() {
        assert_eq!(
            budget_verdict(STAGE_A_BUDGET_S, 0),
            Some("stage-a-budget-exceeded")
        );
        assert_eq!(
            budget_verdict(0.0, STAGE_A_RSS_BYTES),
            Some("stage-a-rss-exceeded")
        );
        assert_eq!(
            budget_verdict(STAGE_A_BUDGET_S - 1.0, STAGE_A_RSS_BYTES - 1),
            None
        );

        // CASE 1, the injected clock. Three cells' worth of boundary readings,
        // the third at the bound: the loop stops there, the fourth cell is
        // never evaluated, and no artifact is written.
        let out = Path::new("target/stage-a-wall-budget-test.json");
        drop(std::fs::remove_file(out));
        let mut guard = BudgetGuard::scripted(vec![
            (1.0, 1_024),
            (2.0, 2_048),
            (STAGE_A_BUDGET_S, 4_096),
            (4.0, 4_096),
        ]);
        let mut evaluated = 0_u32;
        let stopped = budgeted(&mut guard, 0..4_u32, |_| {
            evaluated += 1;
            Ok(())
        })
        .expect_err("the wall clock crossed its bound");
        assert!(
            stopped.to_string().contains("stage-a-budget-exceeded"),
            "{stopped}"
        );
        assert_eq!(evaluated, 3, "the run stopped at the crossing boundary");
        assert!(
            write_artifact(out, &json!({}), STAGE_A_BUDGET_S, guard.peak_rss_bytes()).is_err(),
            "a stopped run may not serialize"
        );
        assert!(!out.exists());
        assert!(!out.with_extension("json.tmp").exists());

        // CASE 2, the injected RSS reading. Same shape, the ceiling crossed on
        // the second boundary. The peak is retained, so a later smaller
        // reading does not un-cross it.
        let out = Path::new("target/stage-a-rss-budget-test.json");
        drop(std::fs::remove_file(out));
        let mut guard =
            BudgetGuard::scripted(vec![(1.0, 1_024), (2.0, STAGE_A_RSS_BYTES), (3.0, 1_024)]);
        let mut evaluated = 0_u32;
        let stopped = budgeted(&mut guard, 0..3_u32, |_| {
            evaluated += 1;
            Ok(())
        })
        .expect_err("the RSS ceiling was crossed");
        assert!(
            stopped.to_string().contains("stage-a-rss-exceeded"),
            "{stopped}"
        );
        assert_eq!(evaluated, 2);
        assert_eq!(guard.peak_rss_bytes(), STAGE_A_RSS_BYTES);
        assert!(write_artifact(out, &json!({}), 0.0, guard.peak_rss_bytes()).is_err());
        assert!(!out.exists());
        assert!(!out.with_extension("json.tmp").exists());

        // And a run inside both ceilings does write, so the two cases above
        // are pinning enforcement rather than a write path that never works.
        let out = Path::new("target/stage-a-budget-within-test.json");
        drop(std::fs::remove_file(out));
        let mut guard = BudgetGuard::scripted(vec![(1.0, 1_024), (2.0, 2_048)]);
        budgeted(&mut guard, 0..2_u32, |_| Ok(())).expect("inside both ceilings");
        write_artifact(out, &json!({"verdict":"x"}), 2.0, guard.peak_rss_bytes())
            .expect("a run inside its budget writes");
        assert!(out.exists());
        drop(std::fs::remove_file(out));
    }

    #[test]
    #[ignore = "eight committed month-scale generator walks"]
    fn arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path = root.join("analysis/mnq-measure-12a.json");
        let artifact: Value = serde_json::from_slice(&std::fs::read(&path).expect("12a artifact"))
            .expect("valid 12a artifact");
        // `.measured()`: the oracle drives the real projection every time. A
        // cached `SeedWalk` was produced by whatever the projection was when
        // it was written, so a cache-served run would prove the cache correct
        // and the code untested - which is the one thing a BLOCKING gate may
        // not do.
        let cache = root.join("target/stage-a-layer1-cache");
        let context = ScreenContext::open(&path, Some(&cache))
            .expect("screen context")
            .measured();
        let shipped = Cell::EventMarkov { switch_rate: 0.1 };
        for expected in artifact["generated"]["per_seed"]
            .as_array()
            .expect("generated seeds")
        {
            let seed = expected["seed"].as_u64().expect("seed");
            let walk = project_seed(&context, &shipped, seed).expect("shipped projection");
            assert!(walk.refusal.is_none(), "seed {seed}: {:?}", walk.refusal);
            let got = crate::aggregate::monthly::reduced_blocks_from_sessions(&walk.sessions)
                .expect("monthly reduced blocks");
            assert_eq!(
                parent_count_marginal(&got["block1"]["hist"]).expect("got marginal"),
                parent_count_marginal(&expected["blocks"]["block1"]["hist"])
                    .expect("expected marginal"),
                "seed {seed} block1 marginal"
            );
            assert_eq!(
                got["block2"], expected["blocks"]["block2"],
                "seed {seed} block2"
            );
        }
    }

    /// A2's level limb is a rate ratio, not a total ratio, and each side uses
    /// its OWN scheduled exposure. The defect this pins: the numerator summed
    /// histogram occurrences instead of weighting them by parent count, and
    /// there was no denominator at all, so with 22 observed and 23 generated
    /// sessions the gate returned exactly 23/22 for every mechanism at every
    /// parameter point - constant, and therefore measuring nothing.
    #[test]
    fn the_a2_level_limb_is_a_rate_and_not_a_session_count() {
        // Equal RATES over unequal session counts must give exactly 1.0.
        let mut observed = ScreenReduced::default();
        let mut generated = ScreenReduced::default();
        for (projection, sessions) in [(&mut observed, 22_u64), (&mut generated, 23_u64)] {
            projection
                .parent_counts
                .entry(9)
                .or_default()
                .insert(100, 60 * sessions);
            projection.windows.entry(9).or_default().insert(
                60,
                ScreenWindow {
                    scheduled: 60 * sessions,
                    zeros: 0,
                    count_hist: BTreeMap::new(),
                    present_sessions: sessions,
                },
            );
        }
        let (observed_parents, observed_exposure) = level_parents_and_exposure(&observed);
        let (generated_parents, generated_exposure) = level_parents_and_exposure(&generated);
        assert!(
            (observed_parents - 132_000.0).abs() < 1e-9,
            "occurrences must be weighted by their parent-count key, got {observed_parents}"
        );
        assert_eq!(observed_exposure, 1_320);
        let ratio = (generated_parents / generated_exposure as f64)
            / (observed_parents / observed_exposure as f64);
        assert!(
            (ratio - 1.0).abs() < 1e-12,
            "equal rates over 22 and 23 sessions must give 1.0, got {ratio}"
        );

        // A genuinely different parent rate must show as one.
        let mut hotter = ScreenReduced::default();
        hotter
            .parent_counts
            .entry(9)
            .or_default()
            .insert(110, 60 * 23);
        hotter.windows.entry(9).or_default().insert(
            60,
            ScreenWindow {
                scheduled: 60 * 23,
                zeros: 0,
                count_hist: BTreeMap::new(),
                present_sessions: 23,
            },
        );
        let (hot_parents, hot_exposure) = level_parents_and_exposure(&hotter);
        let hot_ratio =
            (hot_parents / hot_exposure as f64) / (observed_parents / observed_exposure as f64);
        assert!(
            (hot_ratio - 1.1).abs() < 1e-12,
            "a 10 percent hotter rate must read 1.1, got {hot_ratio}"
        );
    }

    /// A REFUSED walk is cached like any other product, so it has to survive
    /// the round trip. It did not: `realized_mean_gap_s` was an `f64` carrying
    /// NaN for "not measured", NaN serializes to JSON `null`, and `null` will
    /// not deserialize back into an `f64` - so the first run to read a cached
    /// refusal died with `invalid type: null, expected f64` before evaluating
    /// a single cell. The field is an `Option` now and this pins it.
    #[test]
    fn a_cached_refused_walk_survives_its_own_round_trip() {
        let refused = SeedWalk {
            seed: 201,
            projection: ScreenReduced::default(),
            #[cfg(test)]
            sessions: Vec::new(),
            parents: 0,
            prints: 0,
            realized_mean_gap_s: None,
            refusal: Some(ScreenRefusal {
                variant: "intensity_ceiling".into(),
                clock_ns: 1_783_696_315_000_000_000,
                detail: "IntensityCeiling".into(),
                family: Some(Family::LogOuCox),
                canonical_params: None,
                seed: Some(201),
            }),
            cost_s: 1.5,
        };
        let encoded = serde_json::to_string(&refused).expect("a refused walk serializes");
        let decoded: SeedWalk =
            serde_json::from_str(&encoded).expect("and deserializes back, which is the whole bug");
        assert_eq!(decoded.seed, 201);
        assert!(decoded.realized_mean_gap_s.is_none());
        assert!(decoded.refusal.is_some());

        // The measured case round-trips too, so the fix did not trade one
        // direction for the other.
        let mut measured = refused;
        measured.realized_mean_gap_s = Some(0.0608);
        measured.refusal = None;
        let encoded = serde_json::to_string(&measured).expect("serializes");
        let decoded: SeedWalk = serde_json::from_str(&encoded).expect("deserializes");
        assert_eq!(decoded.realized_mean_gap_s, Some(0.0608));
    }

    #[test]
    fn envelope_demand_classification_is_the_lazy_screen_predicate() {
        let at_base = [(0, A2_SHAPE_BASE), (1, 0.0)];
        let shell = [(0, A2_SHAPE_BASE + 0.001), (1, A2_SHAPE_CAP)];
        let over = [(0, A2_SHAPE_CAP + 0.001)];
        assert_eq!(
            classify_envelope_demand(&at_base, A2_SHAPE_BASE, A2_SHAPE_CAP),
            EnvelopeDemand::InsideBase
        );
        assert_eq!(
            classify_envelope_demand(&shell, A2_SHAPE_BASE, A2_SHAPE_CAP),
            EnvelopeDemand::MarginalShell
        );
        assert_eq!(
            classify_envelope_demand(&over, A2_SHAPE_BASE, A2_SHAPE_CAP),
            EnvelopeDemand::OverCap
        );
        assert!(!EnvelopeDemand::InsideBase.needs_envelope());
        assert!(EnvelopeDemand::MarginalShell.needs_envelope());
        assert!(!EnvelopeDemand::OverCap.needs_envelope());
        assert!(!EnvelopeDemand::InsideBase.over_cap());
        assert!(!EnvelopeDemand::MarginalShell.over_cap());
        assert!(EnvelopeDemand::OverCap.over_cap());
    }

    /// The 2026-08-11 decision-relevant envelope amendment, at the seam where
    /// it actually bites: a cell whose A3 is past its cap cannot be admitted by
    /// any A2 allowance, so A2's marginal envelope is NOT evaluated - and the
    /// artifact says so rather than going quiet. This is the rule that turned
    /// the census-measured 68 hours of coarse dead-cell work into zero.
    #[test]
    fn a_marginal_gate_on_a_dead_cell_records_its_skip_instead_of_spending() {
        let observed = observed_sessions();
        let ctx = ScreenContext::over(observed.clone()).expect("a screen context");
        let cell = Cell::WallMmpp {
            occupancy: 0.3,
            rate_ratio: 20.0,
            tau_s: 60.0,
        };
        // A generated side that matches the observed one except that every
        // gated hour has NO empty seconds where the observed side has some.
        // That is the exact shape the census found on all 1,402 cells: A3 past
        // its cap, A2 still live.
        let mut dead_on_a3 = observed;
        for session in &mut dead_on_a3 {
            for hour in A3_GATED_HOURS {
                if let Some(window) = session["block2"]
                    .get_mut(hour.to_string())
                    .and_then(|entry| entry.get_mut("1"))
                {
                    window["zero_windows"] = json!(0);
                }
            }
        }
        let mut walk = perfect_walk(&ctx, 201, dead_on_a3);
        // ...and rates lifted about 3 percent, which lands A2's shape strictly
        // between its log(1.02) base and its log(1.25) cap. Both conditions are
        // needed: a cell dead on A3 whose A2 is INSIDE base would never have
        // asked for an envelope, so it could not show the skip.
        for counts in walk.projection.parent_counts.values_mut() {
            let lifted: std::collections::BTreeMap<u32, u64> = counts
                .iter()
                .map(|(&n, &minutes)| (((f64::from(n) * 1.03).round() as u32).max(1), minutes))
                .collect();
            *counts = lifted;
        }
        let walks = vec![walk];
        let (a2, a3, _, a3_pass) =
            amended_rate_and_zero_gates(&ctx, &cell, &walks, true, true).expect("gates evaluate");
        assert!(!a3_pass, "the cell is dead on A3 without any envelope");
        assert_eq!(
            a2["envelope"]["evaluated"], false,
            "A2's envelope must not be spent on a cell A3 already killed"
        );
        assert_eq!(
            a2["envelope"]["skip_reason"], "cell_inadmissible_without_envelope",
            "a skipped marginal gate must say why"
        );
        assert_eq!(
            a2["envelope"]["classification"], "marginal_shell",
            "and must still record where it stood"
        );
        assert_eq!(a2["passed"], false);
        assert!(
            a2.get("verdict").is_none(),
            "a resolved failure is not unresolved"
        );
        assert_eq!(a3["envelope"]["evaluated"], false);
    }

    #[test]
    fn envelope_demand_either_is_a_union_not_a_sum() {
        let demands = [
            CellEnvelopeDemand {
                family: Family::WallMmpp,
                a2: EnvelopeDemand::MarginalShell,
                a3: EnvelopeDemand::MarginalShell,
            },
            CellEnvelopeDemand {
                family: Family::WallMmpp,
                a2: EnvelopeDemand::InsideBase,
                a3: EnvelopeDemand::MarginalShell,
            },
        ];
        assert_eq!(
            marginal_envelope_counts(&demands),
            EnvelopeDemandCounts {
                a2: 1,
                a3: 2,
                either: 2
            }
        );
    }
}
