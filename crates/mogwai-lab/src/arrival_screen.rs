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

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::Instant;

use mogwai_data::{
    ArrivalConfig, ArrivalRefusal, CadenceWalk, GeneratedSource, ParentSummary, SizeGrid,
};
use mogwai_server::source::InstrumentProfile;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::aggregate::context::ObsContext;
use crate::aggregate::countsub::{count_substitution, obs_shares_under, support_refusals_of};
use crate::aggregate::family::conditional_adequacy_bins;
use crate::aggregate::monthly::pool_session_hists;
use crate::arrival_control::{
    GeneratedBinding, HourRate, MEAN_RATE_BAND, ZERO_COUNT_BAND, gate_hours, hourly_mean_parents,
    hourly_zero_second_fraction, seed_median,
};
use crate::error::{LabError, LabResult};
use crate::fit::walk::parse_duration;
use crate::measure12a::{Scope, SessionAcc};
use crate::sampler::ResourceSampler;
use crate::session::{format_trade_date, session_segment_at};
use crate::storage::{CacheStore, ProvenanceInputs, ProvenanceToken, cache_root};

pub const STAGE_A_SEEDS: [u64; 4] = [201, 202, 203, 204];
pub const MEAN_GAP_REL_TOL_12B: f64 = 0.05;
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
/// At 7.0 the model reads 5,376 s of kernel coarse, 950 s of family-1 coarse,
/// 25,200 s of kernel refinement and 4,000 s of family-1 refinement, so about
/// 35,526 s; 39,600 (11 h) carries the same tenth of margin.
///
/// The refinement pass is 82 percent of that and its product is a finer loss
/// ORDERING over cells Stage B then truncates to 24 per family, so whether it
/// should run at all is a live question recorded in `notes/todo.md`. It is not
/// settled here: this constant funds the pass as frozen rather than quietly
/// deleting it.
pub const STAGE_A_BUDGET_S: f64 = 39_600.0;
pub const STAGE_A_RSS_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    EventMarkov,
    WallMmpp,
    LogOuCox,
    SelfExciting,
}

impl Family {
    pub const ALL: [Self; 4] = [
        Self::EventMarkov,
        Self::WallMmpp,
        Self::LogOuCox,
        Self::SelfExciting,
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
}

impl Cell {
    #[must_use]
    pub const fn family(&self) -> Family {
        match self {
            Self::EventMarkov { .. } => Family::EventMarkov,
            Self::WallMmpp { .. } => Family::WallMmpp,
            Self::LogOuCox { .. } => Family::LogOuCox,
            Self::SelfExciting { .. } => Family::SelfExciting,
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
        }
    }

    #[must_use]
    pub const fn fitted_params(&self) -> u8 {
        match self {
            Self::EventMarkov { .. } => 1,
            Self::WallMmpp { .. } => 3,
            _ => 2,
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
        Family::SelfExciting => linear_grid(0.1, 0.85, 0.05)
            .into_iter()
            .flat_map(|phi| {
                log_grid(2.0, 600.0, 3)
                    .into_iter()
                    .map(move |tau_s| Cell::SelfExciting { phi, tau_s })
            })
            .collect(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenRefusal {
    pub variant: String,
    pub clock_ns: u64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeedWalk {
    pub seed: u64,
    pub sessions: Vec<Value>,
    pub parents: u64,
    pub realized_mean_gap_s: f64,
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

const LATTICE_SCALE: u32 = 1 << REFINEMENT_DEPTH;

fn axis_grids(family: Family) -> Vec<(Vec<f64>, bool)> {
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
            (linear_grid(0.1, 0.85, 0.05), false),
            (log_grid(2.0, 600.0, 3), true),
        ],
    }
}

fn cell_from_coordinates(family: Family, coordinates: &[u32]) -> Cell {
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
    observed: ObsContext,
    observed_marginal: CountMarginal,
    /// The observed sides of A1(a), A2 and A3, resolved ONCE. They depend on
    /// the committed 12a artifact alone, so recomputing them per cell would
    /// re-walk 22 session records for every one of roughly 1,400 cells and
    /// bill it to `STAGE_A_CELL_BUDGET_S`.
    observed_shares: HashMap<i64, HashMap<String, f64>>,
    observed_rates: BTreeMap<i64, HourRate>,
    observed_zero: BTreeMap<i64, Option<f64>>,
    hours: Vec<i64>,
    cache: CacheStore,
    bypass_cache: bool,
}

impl ScreenContext {
    pub fn open(measure_path: &Path, cache: Option<&Path>) -> LabResult<Self> {
        let measure_bytes = std::fs::read(measure_path)?;
        let measure: Value = serde_json::from_slice(&measure_bytes)?;
        let binding = GeneratedBinding::from_measure12a(&measure)?;
        let sessions = measure["observed"]["per_session"]
            .as_array()
            .ok_or_else(|| LabError::refusal("observed.per_session is missing"))?
            .clone();
        let hist = &measure["observed"]["monthly"]["block1"]["hist"];
        let observed_marginal = parent_count_marginal(hist)?;
        let profile = mogwai_server::config::profile_from_preset("MNQ")
            .map_err(|e| LabError::refusal(e.to_string()))?;
        let hours = gate_hours(&profile)?;
        let fingerprint_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../analysis/fingerprint.json");
        let fingerprint_hash = crate::ledger::sha256_file(&fingerprint_path)?;
        let measure_hash = crate::ledger::sha256_bytes(&measure_bytes);
        let command = format!(
            "arrival-screen:kernel-version={}:start={}:length={}:warmup={}",
            mogwai_data::ARRIVAL_KERNEL_VERSION,
            binding.window_start_ns,
            binding.window_length_ns,
            binding.warmup
        );
        let token = ProvenanceToken::compute(&ProvenanceInputs {
            crate_version: env!("CARGO_PKG_VERSION"),
            tape_protocol_version: mogwai_data::TAPE_PROTOCOL_VERSION,
            fingerprint_hash: &fingerprint_hash,
            full_command: &command,
            subcontract_hash: &measure_hash,
        });
        let observed = ObsContext::new(sessions);
        let observed_shares = obs_shares_under(&observed, &observed.ones());
        let observed_rates = hourly_mean_parents(&observed);
        let observed_zero = hourly_zero_second_fraction(&observed);
        Ok(Self {
            profile,
            binding,
            observed,
            observed_marginal,
            observed_shares,
            observed_rates,
            observed_zero,
            hours,
            cache: CacheStore::open(cache_root(cache), token),
            bypass_cache: false,
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
        let profile = mogwai_server::config::profile_from_preset("MNQ")
            .map_err(|e| LabError::refusal(e.to_string()))?;
        let hours = gate_hours(&profile)?;
        let mut hist = Vec::new();
        for s in &sessions {
            hist.extend(s["block1_hist"].as_array().into_iter().flatten().cloned());
        }
        let observed_marginal = parent_count_marginal(&Value::Array(hist))?;
        let observed = ObsContext::new(sessions);
        let observed_shares = obs_shares_under(&observed, &observed.ones());
        let observed_rates = hourly_mean_parents(&observed);
        let observed_zero = hourly_zero_second_fraction(&observed);
        Ok(Self {
            profile,
            binding: GeneratedBinding {
                window_start_ns: 0,
                window_length_ns: 0,
                warmup: "0s".into(),
            },
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

enum ParentWalk {
    Generator(Box<GeneratedSource>),
    Kernel(Box<CadenceWalk>),
}
impl ParentSource for ParentWalk {
    fn next(&mut self) -> Result<ParentSummary, ScreenRefusal> {
        match self {
            Self::Generator(source) => Ok(source.advance_parent()),
            Self::Kernel(walk) => {
                let stride = walk.child_stride_ns();
                walk.next()
                    .map(|draw| ParentSummary {
                        parent_ts_ns: draw.parent_ts_ns,
                        child_count: draw.child_count,
                        child_stride_ns: stride,
                    })
                    .map_err(|refusal| ScreenRefusal {
                        // The clock is A4 evidence: a refusal without it says
                        // the walk stopped but not where, which is exactly
                        // what an owner ruling needs.
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
                    })
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
    let seg = session_segment_at(parent_ts_ns, offset).ok_or_else(|| {
        projection_refusal(parent_ts_ns, "a measured parent maps to no open segment")
    })?;
    Ok((
        parent_ts_ns,
        u8::from(seg.segment_origin_ns != seg.session_start_ns),
        seg.session_start_ns,
    ))
}

fn projection_refusal(clock_ns: u64, detail: &str) -> ProjectStop {
    ProjectStop::Refused(ScreenRefusal {
        variant: "projection".into(),
        clock_ns,
        detail: detail.into(),
    })
}

struct Projected {
    sessions: Vec<Value>,
    parents: u64,
    realized_mean_gap_s: f64,
}

pub fn project_seed(ctx: &ScreenContext, cell: &Cell, seed: u64) -> LabResult<SeedWalk> {
    let key = cache_key(cell, seed);
    if !ctx.bypass_cache
        && let Some(bytes) = ctx.cache.read(&key)?
    {
        return Ok(serde_json::from_slice(&bytes)?);
    }
    let started = Instant::now();
    let product = match project_walk(ctx, cell, seed) {
        Ok(done) => SeedWalk {
            seed,
            sessions: done.sessions,
            parents: done.parents,
            realized_mean_gap_s: done.realized_mean_gap_s,
            refusal: None,
            cost_s: started.elapsed().as_secs_f64(),
        },
        // A refusal is cached like any other product: it is the cell's real
        // per-seed outcome, and re-walking it on the next pass would repay a
        // cost the budget already booked.
        Err(ProjectStop::Refused(refusal)) => SeedWalk {
            seed,
            sessions: Vec::new(),
            parents: 0,
            realized_mean_gap_s: f64::NAN,
            refusal: Some(refusal),
            cost_s: started.elapsed().as_secs_f64(),
        },
        Err(ProjectStop::Lab(e)) => return Err(e),
    };
    if !ctx.bypass_cache {
        ctx.cache.write(&key, &serde_json::to_vec(&product)?)?;
    }
    Ok(product)
}

fn project_walk(ctx: &ScreenContext, cell: &Cell, seed: u64) -> Result<Projected, ProjectStop> {
    let start = ctx.binding.window_start_ns;
    let end = start.saturating_add(ctx.binding.window_length_ns);
    let warmup_ns = u64::try_from(parse_duration(&ctx.binding.warmup).map_err(ProjectStop::Lab)?)
        .map_err(|_| LabError::refusal("warmup duration is negative"))?;
    let walk_start = start
        .checked_sub(warmup_ns)
        .ok_or_else(|| LabError::refusal("the warmup underflows the window start"))?;
    let mut scalars = ctx.profile.scalars.clone();
    let config = cell.config();
    // A refinement midpoint that leaves the section 16 domain is a refusal,
    // not a silent walk at an out-of-domain parameterization.
    if !config.is_valid() {
        return Err(projection_refusal(
            walk_start,
            "the cell leaves the frozen section 16 parameter domain",
        ));
    }
    scalars.arrival = Some(config);
    let offset = i32::from(
        ctx.profile
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
                mogwai_server::source::fingerprint(),
                &ctx.profile.session,
                None,
                SizeGrid::from_def(&ctx.profile.def),
                ctx.profile.calendar.clone(),
            )
            .map_err(|e| LabError::refusal(format!("building the generator: {e:?}")))?,
        )),
        _ => ParentWalk::Kernel(Box::new(
            CadenceWalk::new(
                &scalars,
                &ctx.profile.session,
                ctx.profile.calendar.as_ref(),
                1.0,
                seed,
                walk_start,
            )
            .ok_or_else(|| LabError::refusal("cell has no integrated kernel"))?,
        )),
    };
    project_stream(&mut walk, start, end, offset, seed)
}

/// Spec 3.3 steps 3 to 6, over any parent stream: the open-parent lifecycle,
/// the child enumeration, the session rotation, the termination guard and the
/// measured mean gap. Everything family-specific is behind [`ParentSource`], so
/// this is the whole of what the layer-1 oracle validates.
fn project_stream(
    walk: &mut dyn ParentSource,
    start: u64,
    end: u64,
    offset: i32,
    seed: u64,
) -> Result<Projected, ProjectStop> {
    let mut sessions = Vec::new();
    let mut acc: Option<SessionAcc> = None;
    let mut open_parent: Option<(u64, u8, u64)> = None;
    let mut first = None;
    let mut last = None;
    let mut parents = 0_u64;
    let mut previous = None;
    // `GeneratedAcc::close_open_parent`, transcribed: a parent outside the
    // measured window is dropped, a parent with no open session is skipped
    // exactly as the shipped accumulator skips it, and a parent whose segment
    // disagrees with the open session trips the rotation invariant.
    let close_parent = |acc: &mut Option<SessionAcc>,
                        open: &mut Option<(u64, u8, u64)>|
     -> Result<(), ProjectStop> {
        if let Some((ts, index, session_start)) = open.take()
            && start <= ts
            && ts < end
            && let Some(target) = acc.as_mut()
        {
            if target.session_start_ns != session_start {
                return Err(projection_refusal(
                    ts,
                    "a measured parent closes into another session; the rotation invariant \
                     is broken",
                ));
            }
            target.push_parent(index, ts, 0, 0, false)?;
        }
        Ok(())
    };
    loop {
        let parent = walk.next().map_err(ProjectStop::Refused)?;
        // The family-1 termination guard of spec 3.3 step 3: a faulted
        // `GeneratedSource` hands back a stale summary forever.
        if previous.is_some_and(|p| parent.parent_ts_ns <= p) {
            return Err(projection_refusal(
                parent.parent_ts_ns,
                "parent walk stalled",
            ));
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
        for i in 0..parent.child_count {
            let ts = parent
                .parent_ts_ns
                .saturating_add(u64::from(i).saturating_mul(parent.child_stride_ns));
            if !(start..end).contains(&ts) {
                continue;
            }
            if let Some(seg) = session_segment_at(ts, offset)
                && acc
                    .as_ref()
                    .is_none_or(|a| a.session_start_ns != seg.session_start_ns)
            {
                close_parent(&mut acc, &mut open_parent)?;
                if let Some(old) = acc.take() {
                    sessions.push(old.close_reduced(Scope::Generated { seed })?);
                }
                acc = Some(SessionAcc::new(
                    format_trade_date(seg.trade_day),
                    &seg,
                    offset,
                ));
            }
            if let Some(active) = acc.as_mut() {
                active.push_print(ts, 0);
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
        sessions.push(old.close_reduced(Scope::Generated { seed })?);
    }
    let realized_mean_gap_s = first
        .zip(last)
        .filter(|_| parents >= 2)
        .map_or(f64::NAN, |(a, b)| {
            (b - a) as f64 / ((parents - 1) as f64 * 1e9)
        });
    Ok(Projected {
        sessions,
        parents,
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

/// A4's mean-gap limb. A non-finite realized gap - fewer than two measured
/// parents - fails rather than propagating a NaN comparison.
fn gap_within_tolerance(ctx: &ScreenContext, realized_s: f64) -> bool {
    realized_s.is_finite()
        && ((realized_s / ctx.profile.scalars.mean_event_duration_s) - 1.0).abs()
            <= MEAN_GAP_REL_TOL_12B
}

pub fn evaluate_cell(ctx: &ScreenContext, cell: &Cell, seeds: &[u64]) -> LabResult<CellVerdict> {
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
    Ok(verdict)
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
                    "realized {} against declared {}",
                    walk.realized_mean_gap_s, ctx.profile.scalars.mean_event_duration_s
                ),
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
    fn a_child_with_no_segment_is_pushed_not_refused() {
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
        let ProjectStop::Lab(error) = stop else {
            panic!("the projection was stricter than `GeneratedAcc::push_trade`");
        };
        let text = error.to_string();
        assert!(
            text.contains("maps to no open segment"),
            "the refusal is the accumulator's own, raised at close: {text}"
        );
    }

    #[test]
    fn a_family_one_walk_that_stalls_refuses_instead_of_looping() {
        // Spec 3.3 step 3's termination guard. `advance_parent` cannot report
        // failure: a faulted source returns a stale summary forever, so the
        // projection refuses on the first non-advancing timestamp rather than
        // hanging for eight hours.
        let day = 20_000_u64;
        let start = day * DAY_NS + OPEN_NS;
        let mut source = Scripted::stalling(vec![(start + MINUTE_NS, 1)]);
        let stop = project_stream(&mut source, start, start + 60 * MINUTE_NS, 0, 1)
            .err()
            .expect("a stalled walk refuses");
        let ProjectStop::Refused(refusal) = stop else {
            panic!("a stall is a cell refusal, not a run abort");
        };
        assert_eq!(refusal.variant, "projection");
        assert!(refusal.detail.contains("stalled"), "{}", refusal.detail);
    }

    #[test]
    fn the_mean_gap_counts_measured_parents_only() {
        // Spec 3.3 step 6. Warmup parents are outside [start, end) and the
        // terminal lookahead parent is never projected, so the realized gap is
        // (last measured - first measured) / (measured - 1).
        let day = 20_000_u64;
        let start = day * DAY_NS + OPEN_NS;
        let end = start + 60 * MINUTE_NS;
        let mut source = Scripted::of(vec![
            (start - 10 * MINUTE_NS, 1), // warmup
            (start - MINUTE_NS, 1),      // warmup
            (start, 1),
            (start + 2 * MINUTE_NS, 1),
            (start + 6 * MINUTE_NS, 1),
            (end + MINUTE_NS, 1), // the terminal lookahead
        ]);
        let projected = project_stream(&mut source, start, end, 0, 1).expect("a clean projection");
        assert_eq!(
            projected.parents, 3,
            "warmup and lookahead are not measured"
        );
        assert!(
            (projected.realized_mean_gap_s - 180.0).abs() < 1e-9,
            "{}",
            projected.realized_mean_gap_s
        );

        // Fewer than two measured parents is a `MeanGap` refusal recorded as
        // NaN, never a division by zero.
        let mut lonely = Scripted::of(vec![(start, 1), (end + MINUTE_NS, 1)]);
        let one = project_stream(&mut lonely, start, end, 0, 1).expect("a clean projection");
        assert_eq!(one.parents, 1);
        assert!(one.realized_mean_gap_s.is_nan());
        let ctx = ScreenContext::over(observed_sessions()).expect("a screen context");
        assert!(
            !gap_within_tolerance(&ctx, one.realized_mean_gap_s),
            "a NaN gap fails A4 rather than comparing false into a pass"
        );
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
        assert_eq!(refusal.variant, "projection");
        assert!(
            refusal.detail.contains("no open segment"),
            "{}",
            refusal.detail
        );
    }

    // -- The conditions and the loss, spec 3.4 and 3.5 ----------------------

    fn measure12a() -> Value {
        serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json"))
            .expect("the committed 12a artifact")
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
        SeedWalk {
            seed,
            sessions,
            parents: 2,
            realized_mean_gap_s: ctx.profile.scalars.mean_event_duration_s,
            refusal: None,
            cost_s: 0.0,
        }
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
        let profile = mogwai_server::config::profile_from_preset("MNQ").expect("the MNQ preset");
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
        for rows in [&verdict.a2["per_seed_hour"], &verdict.a3["per_seed_hour"]] {
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
            sessions: Vec::new(),
            parents: 0,
            realized_mean_gap_s: f64::NAN,
            refusal: Some(ScreenRefusal {
                variant: "intensity_ceiling".into(),
                clock_ns: 1_234_567,
                detail: "IntensityCeiling".into(),
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
    fn the_coarse_pass_admits_a_superset_of_the_four_seed_pass() {
        // Spec section 5: every condition is per-seed and failure-monotone, so
        // a two-seed coarse pass can only ADMIT a superset of what four seeds
        // would admit. Exercised rather than asserted: the same cell is judged
        // on two seeds and on the same two plus two failing ones.
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
            bad.realized_mean_gap_s = ctx.profile.scalars.mean_event_duration_s * 2.0;
            four.push(bad);
        }
        let refined = verdict_from_walks(&ctx, &cell, &four).expect("a verdict");
        assert!(
            !refined.admissible,
            "a per-seed failure on a later seed removes the cell"
        );
        assert!(
            coarse_verdict.admissible || !refined.admissible,
            "the four-seed pass is never a superset of the two-seed one"
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
}
