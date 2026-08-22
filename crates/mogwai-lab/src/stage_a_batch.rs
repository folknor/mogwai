// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Deterministic sampling plan and estimator for the Stage A batch instrument.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::arrival_screen::{
    Cell, Family, LATTICE_SCALE, LatticeCell, REFINEMENT_CELL_CAP, STAGE_A_GEN_REFINE_CAP,
    STAGE_A_SEEDS, axis_grids, cell_from_coordinates, probe_cell,
};
use crate::delivery::sha256_bytes;
use crate::error::{LabError, LabResult};

pub const MANIFEST_SCHEMA_VERSION: u32 = 2;
pub const PILOT_SCHEMA_VERSION: u32 = 1;
pub const ESTIMATOR_VERSION: u32 = 2;
pub const SELECTION_ALGORITHM_VERSION: u32 = 1;
pub const ANCHOR_RULE_VERSION: u32 = 1;
pub const ANCHOR_SELECTION_SEED: u64 = 0x414e_4348_4f52_3031;
pub const PILOT_SELECTION_SEED: u64 = 0x5049_4c4f_545f_3031;
pub const FINAL_SELECTION_SEED: u64 = 0x4649_4e41_4c5f_3031;
pub const PILOT_WALK_SEED: u64 = STAGE_A_SEEDS[0];
pub const ADAPTIVE_SAMPLE_COUNT: usize = 10;
pub const PILOT_CELLS_PER_STRATUM: usize = 3;

const HASH_DOMAIN_ANCHOR: &[u8] = b"stage-a-batch/anchor/v1";
const HASH_DOMAIN_PILOT: &[u8] = b"stage-a-batch/pilot/v1";
const HASH_DOMAIN_FINAL: &[u8] = b"stage-a-batch/final/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleKind {
    Anchor,
    Probability,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct StratumId {
    pub family: Family,
    pub level: u8,
    pub region: u8,
}

impl StratumId {
    #[must_use]
    pub fn key(&self) -> String {
        format!(
            "{}:{}:{:03b}",
            self.family.as_str(),
            self.level,
            self.region
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbabilityRatio {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PanelCell {
    pub family: Family,
    pub level: u8,
    pub lattice: Vec<u32>,
    pub parameter_bits: Vec<u64>,
    pub cell: Cell,
    pub stratum: StratumId,
    pub kind: SampleKind,
    pub quick: bool,
    pub seeds: Vec<u64>,
    pub inclusion_probability: ProbabilityRatio,
    pub extrapolation_weight: ProbabilityRatio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StratumPlan {
    pub id: StratumId,
    pub population_size: u32,
    pub anchor_count: u32,
    pub probability_population_size: u32,
    pub probability_sample_count: u32,
    pub pilot_sample_count: u32,
    pub pilot_mean_cost_s: f64,
    pub pilot_sample_variance_s2: f64,
    pub pooled_variance_s2: f64,
    pub shrunk_variance_s2: f64,
    pub allocation_score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SelectionSeeds {
    pub anchor: u64,
    pub pilot: u64,
    pub final_selection: u64,
    pub pilot_walk: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefinementCap {
    pub family: Family,
    pub cap: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchManifest {
    pub schema_version: u32,
    pub estimator_version: u32,
    pub selection_algorithm_version: u32,
    pub anchor_rule_version: u32,
    pub measurement_sha256: String,
    pub tape_protocol_version: u32,
    pub arrival_kernel_version: u32,
    pub selection_seeds: SelectionSeeds,
    pub refinement_caps: Vec<RefinementCap>,
    pub pilot_readings_sha256: String,
    pub quantile_boundaries: BTreeMap<String, Vec<u32>>,
    pub strata: Vec<StratumPlan>,
    pub quick: Vec<PanelCell>,
    pub full: Vec<PanelCell>,
    pub plan_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PilotCell {
    pub family: Family,
    pub level: u8,
    pub lattice: Vec<u32>,
    pub stratum: StratumId,
}

pub fn resolve_cell(family: Family, level: u8, lattice: &[u32]) -> LabResult<Cell> {
    population(family, level)
        .into_iter()
        .find(|cell| cell.lattice == lattice)
        .map(|cell| cell.cell)
        .ok_or_else(|| {
            LabError::refusal(format!(
                "{} level {level} lattice {lattice:?} is outside the sampling frame",
                family.as_str()
            ))
        })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PilotReading {
    pub family: Family,
    pub level: u8,
    pub lattice: Vec<u32>,
    pub stratum: StratumId,
    pub cost_s: f64,
    pub parents: u64,
    pub prints: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PilotArtifact {
    pub schema_version: u32,
    pub selection_seed: u64,
    pub walk_seed: u64,
    pub measurement_sha256: String,
    pub readings: Vec<PilotReading>,
}

#[derive(Debug, Clone)]
struct Frame {
    by_stratum: BTreeMap<StratumId, Vec<LatticeCell>>,
    boundaries: BTreeMap<String, Vec<u32>>,
}

#[derive(Debug, Clone, Serialize)]
struct HashableManifest<'a> {
    schema_version: u32,
    estimator_version: u32,
    selection_algorithm_version: u32,
    anchor_rule_version: u32,
    measurement_sha256: &'a str,
    tape_protocol_version: u32,
    arrival_kernel_version: u32,
    selection_seeds: &'a SelectionSeeds,
    refinement_caps: &'a [RefinementCap],
    pilot_readings_sha256: &'a str,
    quantile_boundaries: &'a BTreeMap<String, Vec<u32>>,
    strata: &'a [StratumPlan],
    quick: &'a [PanelCell],
    full: &'a [PanelCell],
}

#[must_use]
pub fn population(family: Family, level: u8) -> Vec<LatticeCell> {
    fn enumerate(axes: &[usize], at: usize, row: &mut Vec<u32>, out: &mut Vec<Vec<u32>>) {
        if at == axes.len() {
            out.push(row.clone());
            return;
        }
        let max = u32::try_from(axes[at] - 1).expect("axis length fits") * LATTICE_SCALE;
        for coordinate in 0..=max {
            row.push(coordinate);
            enumerate(axes, at + 1, row, out);
            row.pop();
        }
    }

    let axes: Vec<_> = axis_grids(family)
        .iter()
        .map(|(axis, _)| axis.len())
        .collect();
    let mut coordinates = Vec::new();
    enumerate(&axes, 0, &mut Vec::new(), &mut coordinates);
    coordinates
        .into_iter()
        .filter(|coordinates| lattice_level(coordinates) == Some(level))
        .map(|lattice| LatticeCell {
            cell: cell_from_coordinates(family, &lattice),
            lattice,
            level,
        })
        .collect()
}

#[must_use]
pub fn lattice_level(coordinates: &[u32]) -> Option<u8> {
    let odd = coordinates.iter().filter(|value| *value % 2 == 1).count();
    let half = coordinates
        .iter()
        .filter(|value| *value % LATTICE_SCALE == 2)
        .count();
    let coarse = coordinates
        .iter()
        .filter(|value| *value % LATTICE_SCALE == 0)
        .count();
    if coarse == coordinates.len() {
        Some(0)
    } else if half == 1 && coarse + half == coordinates.len() {
        Some(1)
    } else if (odd == 1 && coarse + odd == coordinates.len())
        || (half == 2 && coarse + half == coordinates.len())
    {
        Some(2)
    } else {
        None
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

fn family_code(family: Family) -> u8 {
    match family {
        Family::EventMarkov => 0,
        Family::WallMmpp => 1,
        Family::LogOuCox => 2,
        Family::SelfExciting => 3,
        Family::ShotNoise => 5,
    }
}

fn selection_digest(domain: &[u8], seed: u64, cell: &LatticeCell) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(domain);
    bytes.extend_from_slice(&seed.to_le_bytes());
    bytes.push(family_code(cell.cell.family()));
    bytes.push(cell.level);
    bytes.extend_from_slice(
        &u32::try_from(cell.lattice.len())
            .expect("axis count fits")
            .to_le_bytes(),
    );
    for coordinate in &cell.lattice {
        bytes.extend_from_slice(&coordinate.to_le_bytes());
    }
    for bits in parameter_bits(&cell.cell) {
        bytes.extend_from_slice(&bits.to_le_bytes());
    }
    sha256_bytes(&bytes)
}

fn sorted_selection(
    cells: &[LatticeCell],
    domain: &[u8],
    seed: u64,
    count: usize,
) -> Vec<LatticeCell> {
    let mut ranked: Vec<_> = cells
        .iter()
        .cloned()
        .map(|cell| (selection_digest(domain, seed, &cell), cell))
        .collect();
    ranked.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.lattice.cmp(&right.1.lattice))
    });
    ranked
        .into_iter()
        .take(count)
        .map(|(_, cell)| cell)
        .collect()
}

fn complete_frame() -> Frame {
    let mut by_stratum = BTreeMap::new();
    let mut boundaries = BTreeMap::new();
    for family in Family::ALL {
        for level in 0..=2 {
            let cells = population(family, level);
            let axis_count = cells[0].lattice.len();
            let mut cuts = Vec::with_capacity(axis_count);
            for axis in 0..axis_count {
                let mut values: Vec<_> = cells.iter().map(|cell| cell.lattice[axis]).collect();
                values.sort_unstable();
                values.dedup();
                cuts.push(values[(values.len() - 1) / 2]);
            }
            boundaries.insert(format!("{}:{level}", family.as_str()), cuts.clone());
            for cell in cells {
                let region = cell
                    .lattice
                    .iter()
                    .zip(&cuts)
                    .enumerate()
                    .fold(0u8, |bits, (axis, (value, cut))| {
                        bits | (u8::from(value > cut) << axis)
                    });
                let id = StratumId {
                    family,
                    level,
                    region,
                };
                by_stratum.entry(id).or_insert_with(Vec::new).push(cell);
            }
        }
    }
    Frame {
        by_stratum,
        boundaries,
    }
}

fn coarse_anchor(family: Family) -> LatticeCell {
    let lattice: Vec<_> = axis_grids(family)
        .iter()
        .map(|(axis, _)| u32::try_from(axis.len() / 2).expect("axis index fits") * LATTICE_SCALE)
        .collect();
    LatticeCell {
        cell: probe_cell(family),
        lattice,
        level: 0,
    }
}

#[must_use]
pub fn refinement_anchor_candidates(family: Family) -> Vec<LatticeCell> {
    fn corners(
        maxima: &[u32],
        quarter_axis: usize,
        at: usize,
        row: &mut Vec<u32>,
        out: &mut Vec<Vec<u32>>,
    ) {
        if at == maxima.len() {
            out.push(row.clone());
            return;
        }
        if at == quarter_axis {
            for coordinate in [1, maxima[at] - 1] {
                row.push(coordinate);
                corners(maxima, quarter_axis, at + 1, row, out);
                row.pop();
            }
        } else {
            for coordinate in [0, maxima[at]] {
                row.push(coordinate);
                corners(maxima, quarter_axis, at + 1, row, out);
                row.pop();
            }
        }
    }

    let maxima: Vec<_> = axis_grids(family)
        .iter()
        .map(|(axis, _)| u32::try_from(axis.len() - 1).expect("axis index fits") * LATTICE_SCALE)
        .collect();
    let mut coordinates = Vec::new();
    for quarter_axis in 0..maxima.len() {
        corners(&maxima, quarter_axis, 0, &mut Vec::new(), &mut coordinates);
    }
    coordinates
        .into_iter()
        .map(|lattice| LatticeCell {
            cell: cell_from_coordinates(family, &lattice),
            lattice,
            level: 2,
        })
        .collect()
}

#[must_use]
pub fn anchors() -> Vec<LatticeCell> {
    let mut out = Vec::with_capacity(8);
    for family in Family::ALL {
        out.push(coarse_anchor(family));
        let candidates = refinement_anchor_candidates(family);
        out.push(
            sorted_selection(&candidates, HASH_DOMAIN_ANCHOR, ANCHOR_SELECTION_SEED, 1).remove(0),
        );
    }
    out
}

fn stratum_for(frame: &Frame, cell: &LatticeCell) -> Option<StratumId> {
    frame
        .by_stratum
        .iter()
        .find(|(id, cells)| {
            id.family == cell.cell.family()
                && id.level == cell.level
                && cells
                    .iter()
                    .any(|candidate| candidate.lattice == cell.lattice)
        })
        .map(|(id, _)| id.clone())
}

#[must_use]
pub fn pilot_plan() -> Vec<PilotCell> {
    let frame = complete_frame();
    let anchor_set: BTreeSet<_> = anchors()
        .into_iter()
        .map(|cell| (cell.cell.family(), cell.level, cell.lattice))
        .collect();
    let mut plan = Vec::new();
    for (id, cells) in &frame.by_stratum {
        let available: Vec<_> = cells
            .iter()
            .filter(|cell| {
                !anchor_set.contains(&(cell.cell.family(), cell.level, cell.lattice.clone()))
            })
            .cloned()
            .collect();
        let count = PILOT_CELLS_PER_STRATUM.min(available.len());
        for cell in sorted_selection(&available, HASH_DOMAIN_PILOT, PILOT_SELECTION_SEED, count) {
            plan.push(PilotCell {
                family: id.family,
                level: id.level,
                lattice: cell.lattice,
                stratum: id.clone(),
            });
        }
    }
    plan
}

fn sample_variance(values: &[f64]) -> f64 {
    if values.len() < 2 {
        return 0.0;
    }
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (values.len() - 1) as f64
}

fn allocate_adaptive(strata: &mut [StratumPlan]) -> LabResult<()> {
    let score_sum: f64 = strata.iter().map(|stratum| stratum.allocation_score).sum();
    if !score_sum.is_finite() || score_sum <= 0.0 {
        return Err(LabError::refusal(
            "pilot produced no finite allocation score",
        ));
    }
    let mut allocations = vec![0usize; strata.len()];
    let mut remainders = Vec::with_capacity(strata.len());
    let mut assigned = 0usize;
    for (index, stratum) in strata.iter().enumerate() {
        let quota = ADAPTIVE_SAMPLE_COUNT as f64 * stratum.allocation_score / score_sum;
        let floor = quota.floor() as usize;
        allocations[index] = floor;
        assigned += floor;
        remainders.push((quota - floor as f64, stratum.id.clone(), index));
    }
    remainders.sort_by(|left, right| right.0.total_cmp(&left.0).then(left.1.cmp(&right.1)));
    for (_, _, index) in remainders
        .into_iter()
        .take(ADAPTIVE_SAMPLE_COUNT - assigned)
    {
        allocations[index] += 1;
    }
    for (stratum, extra) in strata.iter_mut().zip(allocations) {
        let maximum = usize::try_from(stratum.probability_population_size)
            .map_err(|_| LabError::refusal("stratum population does not fit usize"))?;
        stratum.probability_sample_count = u32::try_from((1 + extra).min(maximum))
            .map_err(|_| LabError::refusal("stratum sample count does not fit u32"))?;
    }
    let selected: u64 = strata
        .iter()
        .map(|stratum| u64::from(stratum.probability_sample_count))
        .sum();
    let expected = u64::try_from(strata.len() + ADAPTIVE_SAMPLE_COUNT)
        .map_err(|_| LabError::refusal("adaptive sample count does not fit u64"))?;
    if selected != expected {
        return Err(LabError::refusal(format!(
            "adaptive allocation selected {selected} probability cells"
        )));
    }
    Ok(())
}

fn panel_cell(
    cell: LatticeCell,
    stratum: StratumId,
    kind: SampleKind,
    quick: bool,
    sample_count: u32,
    probability_population: u32,
) -> PanelCell {
    let seeds = if cell.level == 0 {
        STAGE_A_SEEDS[..2].to_vec()
    } else {
        STAGE_A_SEEDS.to_vec()
    };
    let (inclusion_probability, extrapolation_weight) = match kind {
        SampleKind::Anchor => (
            ProbabilityRatio {
                numerator: 1,
                denominator: 1,
            },
            ProbabilityRatio {
                numerator: 1,
                denominator: 1,
            },
        ),
        SampleKind::Probability => (
            ProbabilityRatio {
                numerator: sample_count,
                denominator: probability_population,
            },
            ProbabilityRatio {
                numerator: probability_population,
                denominator: sample_count,
            },
        ),
    };
    PanelCell {
        family: cell.cell.family(),
        level: cell.level,
        lattice: cell.lattice,
        parameter_bits: parameter_bits(&cell.cell),
        cell: cell.cell,
        stratum,
        kind,
        quick,
        seeds,
        inclusion_probability,
        extrapolation_weight,
    }
}

pub fn build_manifest(
    measurement_sha256: String,
    pilot: &PilotArtifact,
) -> LabResult<BatchManifest> {
    if pilot.schema_version != PILOT_SCHEMA_VERSION
        || pilot.selection_seed != PILOT_SELECTION_SEED
        || pilot.walk_seed != PILOT_WALK_SEED
        || pilot.measurement_sha256 != measurement_sha256
    {
        return Err(LabError::refusal(
            "pilot identity does not match the manifest builder",
        ));
    }
    let expected_pilot = pilot_plan();
    if pilot.readings.len() != expected_pilot.len() {
        return Err(LabError::refusal(format!(
            "pilot has {} readings, expected {}",
            pilot.readings.len(),
            expected_pilot.len()
        )));
    }
    for expected in &expected_pilot {
        if !pilot.readings.iter().any(|reading| {
            reading.family == expected.family
                && reading.level == expected.level
                && reading.lattice == expected.lattice
                && reading.stratum == expected.stratum
                && reading.cost_s.is_finite()
                && reading.cost_s > 0.0
        }) {
            return Err(LabError::refusal(format!(
                "pilot is missing {} {:?}",
                expected.stratum.key(),
                expected.lattice
            )));
        }
    }

    let frame = complete_frame();
    let anchors = anchors();
    let anchor_set: BTreeSet<_> = anchors
        .iter()
        .map(|cell| (cell.cell.family(), cell.level, cell.lattice.clone()))
        .collect();
    let mut pool_values: BTreeMap<(Family, u8), Vec<f64>> = BTreeMap::new();
    for reading in &pilot.readings {
        pool_values
            .entry((reading.family, reading.level))
            .or_default()
            .push(reading.cost_s);
    }
    let pool_variances: BTreeMap<_, _> = pool_values
        .into_iter()
        .map(|(key, values)| (key, sample_variance(&values)))
        .collect();

    let mut strata = Vec::with_capacity(frame.by_stratum.len());
    for (id, cells) in &frame.by_stratum {
        let values: Vec<_> = pilot
            .readings
            .iter()
            .filter(|reading| reading.stratum == *id)
            .map(|reading| reading.cost_s)
            .collect();
        let mean = values.iter().sum::<f64>() / values.len() as f64;
        let variance = sample_variance(&values);
        let pooled = pool_variances[&(id.family, id.level)];
        let floor = (mean * f64::EPSILON.sqrt()).powi(2);
        let shrunk = (0.5f64.mul_add(variance, 0.5 * pooled)).max(floor);
        let anchor_count = anchors
            .iter()
            .filter(|anchor| stratum_for(&frame, anchor).as_ref() == Some(id))
            .count();
        let probability_population = cells.len() - anchor_count;
        let score = probability_population as f64 * shrunk.sqrt() / mean.sqrt();
        strata.push(StratumPlan {
            id: id.clone(),
            population_size: u32::try_from(cells.len())
                .map_err(|_| LabError::refusal("stratum population does not fit u32"))?,
            anchor_count: u32::try_from(anchor_count)
                .map_err(|_| LabError::refusal("anchor count does not fit u32"))?,
            probability_population_size: u32::try_from(probability_population)
                .map_err(|_| LabError::refusal("probability population does not fit u32"))?,
            probability_sample_count: 1,
            pilot_sample_count: u32::try_from(values.len())
                .map_err(|_| LabError::refusal("pilot count does not fit u32"))?,
            pilot_mean_cost_s: mean,
            pilot_sample_variance_s2: variance,
            pooled_variance_s2: pooled,
            shrunk_variance_s2: shrunk,
            allocation_score: score,
        });
    }
    allocate_adaptive(&mut strata)?;
    let stratum_lookup: BTreeMap<_, _> = strata
        .iter()
        .map(|stratum| (stratum.id.clone(), stratum))
        .collect();

    let mut quick = Vec::with_capacity(anchors.len());
    let mut full = Vec::with_capacity(72);
    for anchor in anchors {
        let id = stratum_for(&frame, &anchor)
            .ok_or_else(|| LabError::refusal("derived anchor is outside the complete frame"))?;
        let entry = panel_cell(anchor, id, SampleKind::Anchor, true, 1, 1);
        quick.push(entry.clone());
        full.push(entry);
    }
    for (id, cells) in &frame.by_stratum {
        let available: Vec<_> = cells
            .iter()
            .filter(|cell| {
                !anchor_set.contains(&(cell.cell.family(), cell.level, cell.lattice.clone()))
            })
            .cloned()
            .collect();
        let plan = stratum_lookup[id];
        let sample_count = usize::try_from(plan.probability_sample_count)
            .map_err(|_| LabError::refusal("probability sample count does not fit usize"))?;
        let selected = sorted_selection(
            &available,
            HASH_DOMAIN_FINAL,
            FINAL_SELECTION_SEED,
            sample_count,
        );
        for cell in selected {
            full.push(panel_cell(
                cell,
                id.clone(),
                SampleKind::Probability,
                false,
                plan.probability_sample_count,
                plan.probability_population_size,
            ));
        }
    }
    quick.sort_by(panel_order);
    full.sort_by(panel_order);

    let pilot_bytes = serde_json::to_vec(pilot)?;
    let refinement_caps = Family::ALL
        .into_iter()
        .map(|family| RefinementCap {
            family,
            cap: u32::try_from(if family == Family::EventMarkov {
                STAGE_A_GEN_REFINE_CAP
            } else {
                REFINEMENT_CELL_CAP
            })
            .expect("cap fits"),
        })
        .collect();
    let mut manifest = BatchManifest {
        schema_version: MANIFEST_SCHEMA_VERSION,
        estimator_version: ESTIMATOR_VERSION,
        selection_algorithm_version: SELECTION_ALGORITHM_VERSION,
        anchor_rule_version: ANCHOR_RULE_VERSION,
        measurement_sha256,
        tape_protocol_version: mogwai_data::TAPE_PROTOCOL_VERSION,
        arrival_kernel_version: mogwai_data::ARRIVAL_KERNEL_VERSION,
        selection_seeds: SelectionSeeds {
            anchor: ANCHOR_SELECTION_SEED,
            pilot: PILOT_SELECTION_SEED,
            final_selection: FINAL_SELECTION_SEED,
            pilot_walk: PILOT_WALK_SEED,
        },
        refinement_caps,
        pilot_readings_sha256: sha256_bytes(&pilot_bytes),
        quantile_boundaries: frame.boundaries,
        strata,
        quick,
        full,
        plan_sha256: String::new(),
    };
    manifest.plan_sha256 = manifest_hash(&manifest)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn panel_order(left: &PanelCell, right: &PanelCell) -> std::cmp::Ordering {
    left.family
        .cmp(&right.family)
        .then(left.level.cmp(&right.level))
        .then(left.kind.cmp(&right.kind))
        .then(left.lattice.cmp(&right.lattice))
}

pub fn manifest_hash(manifest: &BatchManifest) -> LabResult<String> {
    let hashable = HashableManifest {
        schema_version: manifest.schema_version,
        estimator_version: manifest.estimator_version,
        selection_algorithm_version: manifest.selection_algorithm_version,
        anchor_rule_version: manifest.anchor_rule_version,
        measurement_sha256: &manifest.measurement_sha256,
        tape_protocol_version: manifest.tape_protocol_version,
        arrival_kernel_version: manifest.arrival_kernel_version,
        selection_seeds: &manifest.selection_seeds,
        refinement_caps: &manifest.refinement_caps,
        pilot_readings_sha256: &manifest.pilot_readings_sha256,
        quantile_boundaries: &manifest.quantile_boundaries,
        strata: &manifest.strata,
        quick: &manifest.quick,
        full: &manifest.full,
    };
    Ok(sha256_bytes(&serde_json::to_vec(&hashable)?))
}

pub fn validate_manifest(manifest: &BatchManifest) -> LabResult<()> {
    if manifest.schema_version != MANIFEST_SCHEMA_VERSION
        || manifest.estimator_version != ESTIMATOR_VERSION
        || manifest.selection_algorithm_version != SELECTION_ALGORITHM_VERSION
        || manifest.anchor_rule_version != ANCHOR_RULE_VERSION
    {
        return Err(LabError::refusal(
            "unsupported Stage A batch manifest version",
        ));
    }
    if manifest.selection_seeds.anchor != ANCHOR_SELECTION_SEED
        || manifest.selection_seeds.pilot != PILOT_SELECTION_SEED
        || manifest.selection_seeds.final_selection != FINAL_SELECTION_SEED
        || manifest.selection_seeds.pilot_walk != PILOT_WALK_SEED
    {
        return Err(LabError::refusal("manifest selection seeds changed"));
    }
    if manifest.tape_protocol_version != mogwai_data::TAPE_PROTOCOL_VERSION
        || manifest.arrival_kernel_version != mogwai_data::ARRIVAL_KERNEL_VERSION
    {
        return Err(LabError::refusal(
            "manifest tape or arrival-kernel version changed",
        ));
    }
    if manifest.quick.len() != 10 || manifest.full.len() != 98 {
        return Err(LabError::refusal(format!(
            "manifest sizes are quick={} full={}",
            manifest.quick.len(),
            manifest.full.len()
        )));
    }
    let frame = complete_frame();
    if manifest.quantile_boundaries != frame.boundaries || manifest.strata.len() != 78 {
        return Err(LabError::refusal(
            "manifest quantile partition does not match the complete frame",
        ));
    }
    let generator_cap = u32::try_from(STAGE_A_GEN_REFINE_CAP)
        .map_err(|_| LabError::refusal("generator refinement cap does not fit u32"))?;
    let kernel_cap = u32::try_from(REFINEMENT_CELL_CAP)
        .map_err(|_| LabError::refusal("kernel refinement cap does not fit u32"))?;
    let expected_caps = Family::ALL.map(|family| RefinementCap {
        family,
        cap: if family == Family::EventMarkov {
            generator_cap
        } else {
            kernel_cap
        },
    });
    if manifest.refinement_caps != expected_caps {
        return Err(LabError::refusal("manifest refinement caps changed"));
    }
    let quick_ids: BTreeSet<_> = manifest
        .quick
        .iter()
        .map(|cell| (cell.family, cell.level, cell.lattice.clone()))
        .collect();
    let full_ids: BTreeSet<_> = manifest
        .full
        .iter()
        .map(|cell| (cell.family, cell.level, cell.lattice.clone()))
        .collect();
    if quick_ids.len() != manifest.quick.len()
        || full_ids.len() != manifest.full.len()
        || !quick_ids.is_subset(&full_ids)
        || !manifest
            .quick
            .iter()
            .all(|quick| manifest.full.contains(quick))
    {
        return Err(LabError::refusal(
            "quick tasks are not an unchanged subset of the full panel",
        ));
    }
    let anchor_ids: BTreeSet<_> = anchors()
        .into_iter()
        .map(|cell| (cell.cell.family(), cell.level, cell.lattice))
        .collect();
    if quick_ids != anchor_ids {
        return Err(LabError::refusal(
            "quick panel is not the derived anchor set",
        ));
    }
    for family in Family::ALL {
        let family_anchors: Vec<_> = manifest
            .quick
            .iter()
            .filter(|cell| cell.family == family)
            .collect();
        if family_anchors.len() != 2
            || family_anchors.iter().filter(|cell| cell.level == 0).count() != 1
            || family_anchors.iter().filter(|cell| cell.level == 2).count() != 1
        {
            return Err(LabError::refusal(
                "each family must have one coarse and one depth-2 anchor",
            ));
        }
    }
    for anchor in manifest.quick.iter().filter(|cell| cell.level != 0) {
        let maxima: Vec<_> = axis_grids(anchor.family)
            .iter()
            .map(|(axis, _)| {
                u32::try_from(axis.len() - 1).expect("axis index fits") * LATTICE_SCALE
            })
            .collect();
        let quarter_axes = anchor
            .lattice
            .iter()
            .zip(&maxima)
            .filter(|(coordinate, maximum)| **coordinate == 1 || **coordinate == **maximum - 1)
            .count();
        let corner_axes = anchor
            .lattice
            .iter()
            .zip(&maxima)
            .filter(|(coordinate, maximum)| **coordinate == 0 || **coordinate == **maximum)
            .count();
        if quarter_axes != 1 || corner_axes + quarter_axes != anchor.lattice.len() {
            return Err(LabError::refusal(
                "refinement anchor violates the corner-adjacent rule",
            ));
        }
    }
    for cell in &manifest.full {
        let resolved = resolve_cell(cell.family, cell.level, &cell.lattice);
        let resolved_matches = resolved.is_ok_and(|resolved| resolved == cell.cell);
        if cell.cell.family() != cell.family
            || cell.parameter_bits != parameter_bits(&cell.cell)
            || lattice_level(&cell.lattice) != Some(cell.level)
            || !resolved_matches
            || stratum_for(
                &frame,
                &LatticeCell {
                    cell: cell.cell.clone(),
                    lattice: cell.lattice.clone(),
                    level: cell.level,
                },
            ) != Some(cell.stratum.clone())
        {
            return Err(LabError::refusal("panel cell identity is inconsistent"));
        }
        let expected_seeds = if cell.level == 0 { 2 } else { 4 };
        if cell.seeds != STAGE_A_SEEDS[..expected_seeds] {
            return Err(LabError::refusal("panel seed multiplicity changed"));
        }
        if cell.inclusion_probability.numerator == 0
            || cell.inclusion_probability.denominator == 0
            || cell.inclusion_probability.numerator > cell.inclusion_probability.denominator
            || cell.extrapolation_weight.numerator == 0
            || cell.extrapolation_weight.denominator == 0
        {
            return Err(LabError::refusal("invalid inclusion probability or weight"));
        }
        match cell.kind {
            SampleKind::Anchor => {
                if !cell.quick
                    || cell.inclusion_probability.numerator != 1
                    || cell.inclusion_probability.denominator != 1
                    || cell.extrapolation_weight.numerator != 1
                    || cell.extrapolation_weight.denominator != 1
                {
                    return Err(LabError::refusal("anchor is not certainty weighted"));
                }
            }
            SampleKind::Probability => {
                if cell.quick
                    || anchor_ids.contains(&(cell.family, cell.level, cell.lattice.clone()))
                {
                    return Err(LabError::refusal("anchor leaked into probability sample"));
                }
            }
        }
    }
    for stratum in &manifest.strata {
        let probability_count = manifest
            .full
            .iter()
            .filter(|cell| cell.stratum == stratum.id && cell.kind == SampleKind::Probability)
            .count();
        let probability_count = u32::try_from(probability_count)
            .map_err(|_| LabError::refusal("manifest sample count does not fit u32"))?;
        if probability_count != stratum.probability_sample_count
            || stratum.probability_sample_count == 0
            || stratum.probability_population_size + stratum.anchor_count != stratum.population_size
        {
            return Err(LabError::refusal(
                "stratum sample accounting is inconsistent",
            ));
        }
        for cell in manifest
            .full
            .iter()
            .filter(|cell| cell.stratum == stratum.id && cell.kind == SampleKind::Probability)
        {
            if cell.inclusion_probability.numerator != stratum.probability_sample_count
                || cell.inclusion_probability.denominator != stratum.probability_population_size
                || cell.extrapolation_weight.numerator != stratum.probability_population_size
                || cell.extrapolation_weight.denominator != stratum.probability_sample_count
            {
                return Err(LabError::refusal(
                    "probability cell weight does not match its stratum",
                ));
            }
        }
        let frame_cells = frame
            .by_stratum
            .get(&stratum.id)
            .ok_or_else(|| LabError::refusal("manifest contains an unknown stratum"))?;
        let available: Vec<_> = frame_cells
            .iter()
            .filter(|cell| {
                !anchor_ids.contains(&(cell.cell.family(), cell.level, cell.lattice.clone()))
            })
            .cloned()
            .collect();
        let sample_count = usize::try_from(stratum.probability_sample_count)
            .map_err(|_| LabError::refusal("probability sample count does not fit usize"))?;
        let expected_selection: BTreeSet<_> = sorted_selection(
            &available,
            HASH_DOMAIN_FINAL,
            FINAL_SELECTION_SEED,
            sample_count,
        )
        .into_iter()
        .map(|cell| cell.lattice)
        .collect();
        let actual_selection: BTreeSet<_> = manifest
            .full
            .iter()
            .filter(|cell| cell.stratum == stratum.id && cell.kind == SampleKind::Probability)
            .map(|cell| cell.lattice.clone())
            .collect();
        if actual_selection != expected_selection {
            return Err(LabError::refusal(
                "probability sample is not the derived lowest-hash selection",
            ));
        }
    }
    for family in Family::ALL {
        for level in 0..=2 {
            let represented: f64 = manifest
                .full
                .iter()
                .filter(|cell| cell.family == family && cell.level == level)
                .map(|cell| {
                    f64::from(cell.extrapolation_weight.numerator)
                        / f64::from(cell.extrapolation_weight.denominator)
                })
                .sum();
            let expected = population(family, level).len() as f64;
            if (represented - expected).abs() > 1e-9 {
                return Err(LabError::refusal(format!(
                    "represented population {represented} != {expected} for {} level {level}",
                    family.as_str()
                )));
            }
        }
    }
    let expected_hash = manifest_hash(manifest)?;
    if manifest.plan_sha256 != expected_hash {
        return Err(LabError::refusal(format!(
            "manifest plan hash does not match its contents: expected {expected_hash}, got {}",
            manifest.plan_sha256
        )));
    }
    Ok(())
}

pub fn parse_manifest(bytes: &[u8]) -> LabResult<BatchManifest> {
    let manifest: BatchManifest = serde_json::from_slice(bytes)?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_population_counts_match_the_sampling_frame() {
        let expected = [
            (Family::EventMarkov, [19, 18, 36]),
            (Family::WallMmpp, [504, 1_314, 3_769]),
            (Family::LogOuCox, [120, 218, 535]),
            (Family::SelfExciting, [171, 314, 772]),
            (Family::ShotNoise, [588, 1_547, 4_450]),
        ];
        for (family, counts) in expected {
            for (level, count) in counts.into_iter().enumerate() {
                assert_eq!(population(family, level as u8).len(), count);
            }
        }
    }

    #[test]
    fn product_partition_has_seventy_eight_nonempty_strata() {
        let frame = complete_frame();
        assert_eq!(frame.by_stratum.len(), 78);
        assert!(frame.by_stratum.values().all(|cells| !cells.is_empty()));
    }

    #[test]
    fn anchor_candidate_counts_and_shapes_are_frozen() {
        let expected = [
            (Family::EventMarkov, 2),
            (Family::WallMmpp, 24),
            (Family::LogOuCox, 8),
            (Family::SelfExciting, 8),
            (Family::ShotNoise, 24),
        ];
        for (family, count) in expected {
            let candidates = refinement_anchor_candidates(family);
            assert_eq!(candidates.len(), count);
            for candidate in candidates {
                assert_eq!(candidate.level, 2);
                assert_eq!(lattice_level(&candidate.lattice), Some(2));
                assert_eq!(
                    candidate
                        .lattice
                        .iter()
                        .filter(|coordinate| *coordinate % 2 == 1)
                        .count(),
                    1
                );
            }
        }
    }

    #[test]
    fn pilot_is_three_nonanchors_per_stratum() {
        let anchors: BTreeSet<_> = anchors()
            .into_iter()
            .map(|cell| (cell.cell.family(), cell.level, cell.lattice))
            .collect();
        let pilot = pilot_plan();
        assert_eq!(pilot.len(), 234);
        let mut counts = BTreeMap::new();
        for cell in pilot {
            assert!(!anchors.contains(&(cell.family, cell.level, cell.lattice)));
            *counts.entry(cell.stratum).or_insert(0usize) += 1;
        }
        assert_eq!(counts.len(), 78);
        assert!(counts.values().all(|count| *count == 3));
    }

    #[test]
    fn synthetic_pilot_builds_a_stable_valid_manifest() {
        let readings = pilot_plan()
            .into_iter()
            .enumerate()
            .map(|(index, cell)| PilotReading {
                family: cell.family,
                level: cell.level,
                lattice: cell.lattice,
                stratum: cell.stratum,
                cost_s: 1.0 + f64::from(u32::try_from(index).expect("pilot index fits")) / 1_000.0,
                parents: 1,
                prints: 1,
            })
            .collect();
        let pilot = PilotArtifact {
            schema_version: PILOT_SCHEMA_VERSION,
            selection_seed: PILOT_SELECTION_SEED,
            walk_seed: PILOT_WALK_SEED,
            measurement_sha256: "measurement".into(),
            readings,
        };
        let manifest = build_manifest("measurement".into(), &pilot).expect("manifest builds");
        assert_eq!(manifest.quick.len(), 10);
        assert_eq!(manifest.full.len(), 98);
        assert_eq!(
            manifest.plan_sha256,
            manifest_hash(&manifest).expect("hashes")
        );
        let bytes = serde_json::to_vec(&manifest).expect("serializes");
        assert_eq!(parse_manifest(&bytes).expect("parses"), manifest);
    }

    #[test]
    fn committed_manifest_is_self_consistent() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../analysis/stage-a-batch-manifest.json"
        ));
        let manifest = parse_manifest(bytes).expect("committed manifest validates");
        // Re-blessed at tape protocol 22, which the manifest hashes over: the
        // constant is an input to the plan identity, so a bump moves this by
        // construction and the artifact and this literal move together.
        assert_eq!(
            manifest.plan_sha256,
            "d27e4b7b0f7289c4752b10463df3c2f2699aaff0d6917b1c81f39729a82b8ad0"
        );
        assert_eq!(
            manifest.plan_sha256,
            manifest_hash(&manifest).expect("hashes")
        );
        let pilot_bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../analysis/stage-a-batch-pilot.json"
        ));
        assert_eq!(manifest.pilot_readings_sha256, sha256_bytes(pilot_bytes));
        let pilot: PilotArtifact = serde_json::from_slice(pilot_bytes).expect("pilot parses");
        let rebuilt = build_manifest(manifest.measurement_sha256.clone(), &pilot)
            .expect("manifest re-derives from the committed pilot");
        assert_eq!(rebuilt, manifest);
    }

    #[test]
    fn malformed_lattice_refuses_instead_of_indexing_out_of_bounds() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../analysis/stage-a-batch-manifest.json"
        ));
        let mut manifest: BatchManifest = serde_json::from_slice(bytes).expect("manifest parses");
        let cell = manifest
            .full
            .iter_mut()
            .find(|cell| {
                cell.family == Family::EventMarkov
                    && cell.level == 2
                    && cell.kind == SampleKind::Probability
            })
            .expect("event depth-2 probability cell exists");
        cell.lattice[0] = u32::MAX;
        manifest.plan_sha256 = manifest_hash(&manifest).expect("hashes");
        // NAMED, not merely `is_err()`. The plan hash is recomputed above so
        // the mutation is not caught for free by the identity check, which
        // means the refusal that fires is the whole content of the test - and
        // an unnamed `is_err()` is satisfied by any OTHER check tripping,
        // including one broken by the re-hash itself.
        let text = format!("{:#}", validate_manifest(&manifest).expect_err("refuses"));
        assert!(
            text.contains("panel cell identity is inconsistent"),
            "the out-of-range lattice must fail the cell identity check: {text}"
        );
    }

    #[test]
    fn quick_task_and_probability_selection_are_both_rederived() {
        let bytes = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../analysis/stage-a-batch-manifest.json"
        ));
        let manifest = parse_manifest(bytes).expect("manifest validates");

        let mut changed_quick = manifest.clone();
        changed_quick.quick[0].seeds.push(999);
        changed_quick.plan_sha256 = manifest_hash(&changed_quick).expect("hashes");
        // Both refusals are NAMED for the reason given in
        // `malformed_lattice_refuses_instead_of_indexing_out_of_bounds`: the
        // hash is re-blessed first, so which check fires IS the property.
        let quick_text = format!(
            "{:#}",
            validate_manifest(&changed_quick).expect_err("refuses")
        );
        assert!(
            quick_text.contains("quick tasks are not an unchanged subset of the full panel"),
            "a quick task diverging from its full-panel twin must fail the subset check: \
             {quick_text}"
        );

        let mut changed_selection = manifest.clone();
        let stratum = changed_selection
            .strata
            .iter()
            .find(|stratum| stratum.probability_sample_count == 1)
            .expect("singleton stratum exists")
            .clone();
        let selected = changed_selection
            .full
            .iter()
            .find(|cell| cell.stratum == stratum.id && cell.kind == SampleKind::Probability)
            .expect("selected cell exists")
            .lattice
            .clone();
        let anchor_ids: BTreeSet<_> = anchors()
            .into_iter()
            .map(|cell| (cell.cell.family(), cell.level, cell.lattice))
            .collect();
        let replacement = complete_frame().by_stratum[&stratum.id]
            .iter()
            .find(|cell| {
                cell.lattice != selected
                    && !anchor_ids.contains(&(cell.cell.family(), cell.level, cell.lattice.clone()))
            })
            .expect("replacement cell exists")
            .clone();
        let replacement = panel_cell(
            replacement,
            stratum.id.clone(),
            SampleKind::Probability,
            false,
            stratum.probability_sample_count,
            stratum.probability_population_size,
        );
        let slot = changed_selection
            .full
            .iter_mut()
            .find(|cell| cell.stratum == stratum.id && cell.kind == SampleKind::Probability)
            .expect("selection slot exists");
        *slot = replacement;
        changed_selection.plan_sha256 = manifest_hash(&changed_selection).expect("hashes");
        let selection_text = format!(
            "{:#}",
            validate_manifest(&changed_selection).expect_err("refuses")
        );
        assert!(
            selection_text.contains("probability sample is not the derived lowest-hash selection"),
            "swapping the drawn probability cell must fail the selection re-derivation: \
             {selection_text}"
        );
    }
}
