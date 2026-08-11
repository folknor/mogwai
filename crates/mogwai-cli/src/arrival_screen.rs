// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::Command;
use std::thread;
use std::time::Instant;

use anyhow::{anyhow, bail};
use clap::Args;
use mogwai_lab::arrival_envelope::probe_envelope_costs;
use mogwai_lab::arrival_screen::{
    A3_GATED_HOURS, BudgetGuard, CADENCE_STEP_NS, ENVELOPE_CELL_BUDGET_S, EvaluatedCell, Family,
    REFINEMENT_CELL_CAP, REFINEMENT_DEPTH, STAGE_A_BUDGET_S, STAGE_A_CELL_BUDGET_S,
    STAGE_A_ENVELOPE_BUDGET_S, STAGE_A_GEN_CELL_BUDGET_S, STAGE_A_GEN_REFINE_CAP,
    STAGE_A_RSS_BYTES, STAGE_A_SEEDS, STAGE_B_BUDGET_S, STAGE_B_ENVELOPE_BUDGET_S, ScheduledCell,
    ScreenContext, admissible_regions, budget_verdict, budgeted, census_cells_parallel,
    coarse_grid, coarse_lattice, envelope_cost_s, evaluate_cells_parallel,
    marginal_envelope_counts, measure_cell_walks, probe_cell, refinement_round, screen_work,
    write_artifact,
};
use mogwai_lab::ledger::{fresh_tree_state, require_clean_tree, sha256_file};
use mogwai_lab::sidecar;
use serde_json::{Map, Value, json};

const DEFAULT_MEASURE: &str = "analysis/mnq-measure-12a.json";
const DEFAULT_OUT: &str = "analysis/mnq-arrival-screen.json";
const DEFAULT_MAX_JOBS: usize = 16;

#[derive(Args, Debug)]
pub struct ArrivalScreenArgs {
    #[arg(long, value_name = "PATH")]
    pub measure: Option<PathBuf>,
    #[arg(long, value_name = "PATH", conflicts_with = "demand_census")]
    pub out: Option<PathBuf>,
    #[arg(long, conflicts_with = "demand_census")]
    pub cost_probe: bool,
    /// Count lazy predictive-envelope demand over the frozen coarse grid.
    #[arg(long, value_name = "PATH")]
    pub demand_census: Option<PathBuf>,
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
    /// Concurrent projection workers. Defaults to machine parallelism, capped at 16.
    #[arg(long)]
    pub jobs: Option<usize>,
}

fn artifact_cell(cell: &EvaluatedCell) -> anyhow::Result<Value> {
    let mut value = serde_json::to_value(&cell.verdict)?;
    let object = value
        .as_object_mut()
        .expect("CellVerdict serializes as object");
    let mut seam = object
        .remove("cell")
        .expect("serialized verdict carries cell");
    let seam_object = seam.as_object_mut().expect("Cell serializes as object");
    let family = seam_object
        .remove("family")
        .expect("tagged Cell carries family");
    object.insert("family".into(), family);
    object.insert("params".into(), Value::Object(std::mem::take(seam_object)));
    object.insert("lattice".into(), json!(cell.lattice));
    object.insert("level".into(), json!(cell.level));
    object.insert("pass".into(), json!(cell.pass));
    Ok(value)
}

fn cell_ref(cell: &EvaluatedCell) -> Value {
    json!({"family":cell.verdict.cell.family(),"params":cell.verdict.cell,
           "lattice":cell.lattice,"level":cell.level,"loss":cell.verdict.loss})
}

/// The benchmark channels' view of a screen run: the work it did, and the peak
/// footprint it did it in.
///
/// EVERY TIMING GETS ITS WORK SIZE BESIDE IT. A screen wall that moves is
/// ambiguous on its own - fewer cells evaluated, a cheaper refinement, a
/// shorter walk and genuinely faster code all read the same. `cells_evaluated`
/// and `parents` are the identity-bearing pair under the Stage A free-lane
/// rules (nothing on the measurement side of the draw may change either), and
/// `prints` follows the parents. They are reported on the FIFO for the phase
/// timeline and on stderr for the tracked regression row.
///
/// This is the LAST thing a successful run does, and it is unconditional: a
/// run that emitted its counters only under a flag would be a run whose
/// baseline nobody has.
fn report_work(peak_rss_bytes: u64) {
    let work = screen_work();
    sidecar::report(
        "cells_evaluated",
        i64::try_from(work.cells_evaluated).unwrap_or(i64::MAX),
    );
    sidecar::report("parents", i64::try_from(work.parents).unwrap_or(i64::MAX));
    sidecar::report("prints", i64::try_from(work.prints).unwrap_or(i64::MAX));
    sidecar::report(
        "peak_rss_bytes",
        i64::try_from(peak_rss_bytes).unwrap_or(i64::MAX),
    );
}

pub fn run(args: ArrivalScreenArgs) -> anyhow::Result<Value> {
    if args.jobs == Some(0) {
        bail!("--jobs must be at least 1");
    }
    let jobs = args.jobs.unwrap_or_else(|| {
        thread::available_parallelism()
            .map_or(1, NonZeroUsize::get)
            .min(DEFAULT_MAX_JOBS)
    });
    if args.demand_census.is_some() && args.out.is_some() {
        bail!("--demand-census is mutually exclusive with --out");
    }
    if args.demand_census.is_some() && args.cost_probe {
        bail!("--demand-census is mutually exclusive with --cost-probe");
    }
    let measure = args.measure.unwrap_or_else(|| DEFAULT_MEASURE.into());
    let demand_census = args.demand_census;
    let out = args.out.unwrap_or_else(|| DEFAULT_OUT.into());
    if let Some(path) = demand_census.as_deref() {
        refuse_committed_census_path(path)?;
    }
    let commit = if args.cost_probe || demand_census.is_some() {
        None
    } else {
        Some(require_clean_tree().map_err(|e| anyhow!(e.to_string()))?)
    };
    let context =
        ScreenContext::open(&measure, args.cache.as_deref()).map_err(|e| anyhow!(e.to_string()))?;
    // The probe prices the walk itself; the grid run is allowed its cache.
    let context = if args.cost_probe {
        context.measured()
    } else {
        context
    };
    let mut guard = BudgetGuard::start(args.cache.clone());
    let total = Instant::now();

    if args.cost_probe {
        sidecar::marker("probe");
        let cells = budgeted(&mut guard, Family::ALL, |family| {
            let measurement =
                measure_cell_walks(&context, &probe_cell(family), &STAGE_A_SEEDS[..2])?;
            let limit = if family == Family::EventMarkov {
                STAGE_A_GEN_CELL_BUDGET_S
            } else {
                STAGE_A_CELL_BUDGET_S
            };
            if measurement.cost_s > limit {
                return Err(mogwai_lab::error::LabError::refusal(format!(
                    "stage-a-cell-budget-exceeded: {family:?} cost {:.3}s exceeds {limit:.3}s",
                    measurement.cost_s
                )));
            }
            Ok(json!({"family":family,"cost_s":measurement.cost_s,
                "parents":measurement.parents,"prints":measurement.prints,"budget_s":limit}))
        })
        .map_err(|e| anyhow!(e.to_string()))?;
        let envelope_probes = probe_envelope_costs(
            &context.profile,
            context.binding.window_start_ns,
            context.binding.window_length_ns,
            CADENCE_STEP_NS,
        )
        .map_err(|e| anyhow!(e.to_string()))?;
        let (elapsed_s, peak_rss) = guard.finish().map_err(|e| anyhow!(e.to_string()))?;
        if let Some(verdict) = budget_verdict(elapsed_s, peak_rss) {
            bail!("{verdict}: total_s={elapsed_s:.3} peak_rss_bytes={peak_rss}");
        }
        let result = json!({"mode":"cost_probe","cells":cells,
            "envelope_probes":envelope_probes,"peak_rss_bytes":peak_rss,
            "budgets":{"kernel_cell_s":STAGE_A_CELL_BUDGET_S,
            "generator_cell_s":STAGE_A_GEN_CELL_BUDGET_S,"total_s":STAGE_A_BUDGET_S,
            "rss_bytes":STAGE_A_RSS_BYTES,"envelope_cell_s":ENVELOPE_CELL_BUDGET_S,
            "stage_a_envelope_s":STAGE_A_ENVELOPE_BUDGET_S,
            "stage_b_envelope_s":STAGE_B_ENVELOPE_BUDGET_S,
            "stage_b_total_s":STAGE_B_BUDGET_S}});
        println!("{}", serde_json::to_string_pretty(&result)?);
        report_work(peak_rss);
        return Ok(result);
    }

    if let Some(census_out) = demand_census {
        let projection = context
            .parallel_projection()
            .map_err(|e| anyhow!(e.to_string()))?;
        let coarse_points: Vec<_> = Family::ALL
            .into_iter()
            .flat_map(|family| {
                coarse_lattice(family)
                    .into_iter()
                    .map(move |point| (family, point))
            })
            .collect();
        let scheduled: Vec<_> = coarse_points
            .iter()
            .map(|(_, point)| ScheduledCell {
                cell: point.cell.clone(),
                seeds: STAGE_A_SEEDS[..2].to_vec(),
            })
            .collect();
        sidecar::kv("jobs", jobs);
        sidecar::marker("demand_census");
        let demands =
            census_cells_parallel(&context, &projection, &scheduled, jobs, Some(&mut guard))
                .map_err(|e| anyhow!(e.to_string()))?;
        let (elapsed_s, peak_rss) = guard.finish().map_err(|e| anyhow!(e.to_string()))?;
        if let Some(verdict) = budget_verdict(elapsed_s, peak_rss) {
            bail!("{verdict}: total_s={elapsed_s:.3} peak_rss_bytes={peak_rss}");
        }
        let mut counts = Map::new();
        let mut envelope_demand = Map::new();
        for family in Family::ALL {
            let family_demands: Vec<_> = demands
                .iter()
                .filter(|demand| demand.family == family)
                .collect();
            let gate_counts = |gate: fn(
                &mogwai_lab::arrival_screen::CellEnvelopeDemand,
            )
                -> mogwai_lab::arrival_screen::EnvelopeDemand| {
                let states: Vec<_> = family_demands.iter().map(|demand| gate(demand)).collect();
                json!({
                    "inside_base":states.iter().filter(|state| matches!(state, mogwai_lab::arrival_screen::EnvelopeDemand::InsideBase)).count(),
                    "marginal_shell":states.iter().filter(|state| matches!(state, mogwai_lab::arrival_screen::EnvelopeDemand::MarginalShell)).count(),
                    "over_cap":states.iter().filter(|state| matches!(state, mogwai_lab::arrival_screen::EnvelopeDemand::OverCap)).count(),
                    "total_cells_evaluated":states.len()
                })
            };
            let owned_demands: Vec<_> = family_demands.iter().map(|d| **d).collect();
            let marginal = marginal_envelope_counts(&owned_demands);
            counts.insert(
                family.as_str().into(),
                json!({
                    "a2_shape":gate_counts(|d| d.a2),
                    "a3":gate_counts(|d| d.a3),
                    "total_cells_evaluated":family_demands.len()
                }),
            );
            envelope_demand.insert(family.as_str().into(), json!(marginal));
        }
        let search_space: BTreeMap<_, _> = Family::ALL
            .into_iter()
            .map(|family| {
                let grid = coarse_grid(family);
                (
                    family.as_str().to_string(),
                    json!({"point_count":grid.len(),"grid":grid}),
                )
            })
            .collect();
        let artifact = json!({
            "mode":"envelope_demand_census",
            "binding":{
                "input_hashes":{measure.to_string_lossy().to_string():sha256_file(&measure).map_err(|e|anyhow!(e.to_string()))?},
                "exposure":{"instrument":"MNQ","preset":"crates/mogwai-server/presets/mnq.toml",
                    "window_start_ns":context.binding.window_start_ns,
                    "window_length_ns":context.binding.window_length_ns,"warmup":context.binding.warmup,
                    "divergence":Value::Null,"regime":"neutral"},
                "coarse_seeds":&STAGE_A_SEEDS[..2],
                "tape_protocol_version":mogwai_data::TAPE_PROTOCOL_VERSION,
                "kernel_version":mogwai_data::ARRIVAL_KERNEL_VERSION,
                "spec":"notes/protocol-12b-arrival-composition-spec.md section 9.7, Brick E"
            },
            "search_space":search_space,
            "counts":counts,
            "total_cells_evaluated":demands.len(),
            "envelope_demand":envelope_demand,
            "cost":{"wall_s":total.elapsed().as_secs_f64(),"peak_rss_bytes":peak_rss,"jobs":jobs}
        });
        write_artifact(&census_out, &artifact, elapsed_s, peak_rss)
            .map_err(|e| anyhow!(e.to_string()))?;
        report_work(peak_rss);
        return Ok(artifact);
    }

    let envelope_probes = probe_envelope_costs(
        &context.profile,
        context.binding.window_start_ns,
        context.binding.window_length_ns,
        CADENCE_STEP_NS,
    )
    .map_err(|e| anyhow!(e.to_string()))?;

    let projection = context
        .parallel_projection()
        .map_err(|e| anyhow!(e.to_string()))?;
    sidecar::kv("jobs", jobs);
    sidecar::marker("coarse");
    let coarse_started = Instant::now();
    let mut coarse_points = Vec::new();
    for family in Family::ALL {
        coarse_points.extend(
            coarse_lattice(family)
                .into_iter()
                .map(|point| (family, point)),
        );
    }
    let scheduled: Vec<_> = coarse_points
        .iter()
        .map(|(_, point)| ScheduledCell {
            cell: point.cell.clone(),
            seeds: STAGE_A_SEEDS[..2].to_vec(),
        })
        .collect();
    let evaluations =
        evaluate_cells_parallel(&context, &projection, &scheduled, jobs, Some(&mut guard))
            .map_err(|e| anyhow!(e.to_string()))?;
    let mut by_family: BTreeMap<Family, Vec<EvaluatedCell>> = Family::ALL
        .into_iter()
        .map(|family| (family, Vec::new()))
        .collect();
    for ((family, point), evaluation) in coarse_points.into_iter().zip(evaluations) {
        by_family
            .get_mut(&family)
            .expect("family initialized")
            .push(EvaluatedCell {
                verdict: evaluation.verdict,
                lattice: point.lattice,
                level: 0,
                pass: "coarse",
            });
    }
    let coarse_s = coarse_started.elapsed().as_secs_f64();
    sidecar::marker("refine");
    let refine_started = Instant::now();
    let mut unevaluated: BTreeMap<Family, BTreeMap<String, usize>> = BTreeMap::new();
    let mut used: BTreeMap<Family, usize> =
        Family::ALL.into_iter().map(|family| (family, 0)).collect();
    for round in 1..=REFINEMENT_DEPTH {
        let mut round_points = Vec::new();
        for family in Family::ALL {
            let cap = if family == Family::EventMarkov {
                STAGE_A_GEN_REFINE_CAP
            } else {
                REFINEMENT_CELL_CAP
            };
            let proposal =
                refinement_round(family, &by_family[&family], round, cap - used[&family]);
            unevaluated
                .entry(family)
                .or_default()
                .insert(format!("round_{round}"), proposal.unevaluated);
            *used.get_mut(&family).expect("family initialized") += proposal.candidates.len();
            round_points.extend(proposal.candidates.into_iter().map(|point| (family, point)));
        }
        let scheduled: Vec<_> = round_points
            .iter()
            .map(|(_, point)| ScheduledCell {
                cell: point.cell.clone(),
                seeds: STAGE_A_SEEDS.to_vec(),
            })
            .collect();
        let evaluations =
            evaluate_cells_parallel(&context, &projection, &scheduled, jobs, Some(&mut guard))
                .map_err(|e| anyhow!(e.to_string()))?;
        for ((family, point), evaluation) in round_points.into_iter().zip(evaluations) {
            by_family
                .get_mut(&family)
                .expect("family initialized")
                .push(EvaluatedCell {
                    verdict: evaluation.verdict,
                    lattice: point.lattice,
                    level: point.level,
                    pass: "refine",
                });
        }
    }
    let refine_s = refine_started.elapsed().as_secs_f64();
    let (elapsed_s, peak_rss) = guard.finish().map_err(|e| anyhow!(e.to_string()))?;
    if let Some(verdict) = budget_verdict(elapsed_s, peak_rss) {
        bail!("{verdict}: total_s={elapsed_s:.3} peak_rss_bytes={peak_rss}");
    }

    let mut regions_json = Map::new();
    let mut cells_json = Vec::new();
    let mut refusals = Vec::new();
    let mut per_family_cost = Map::new();
    let mut surviving = Vec::new();
    for family in Family::ALL {
        let evaluated = &by_family[&family];
        let regions = admissible_regions(evaluated);
        if !regions.is_empty() {
            surviving.push(family);
        }
        let axes = mogwai_lab::arrival_screen::coarse_lattice(family);
        let dims = evaluated.first().map_or(0, |c| c.lattice.len());
        let maxima: Vec<u32> = (0..dims)
            .map(|axis| axes.iter().map(|c| c.lattice[axis]).max().unwrap_or(0))
            .collect();
        let admissible: Vec<_> = evaluated
            .iter()
            .filter(|c| c.verdict.admissible)
            .map(cell_ref)
            .collect();
        let endpoints: Vec<_> = evaluated
            .iter()
            .filter(|c| {
                c.verdict.admissible
                    && c.lattice
                        .iter()
                        .enumerate()
                        .any(|(a, &v)| v == 0 || v == maxima[a])
            })
            .map(cell_ref)
            .collect();
        let region_values: Vec<_> = regions
            .iter()
            .map(|region| {
                region
                    .iter()
                    .map(|&i| cell_ref(&evaluated[i]))
                    .collect::<Vec<_>>()
            })
            .collect();
        regions_json.insert(family.as_str().to_string(), json!({
            "regions":region_values,"cells":admissible,"endpoints_admissible":endpoints,
            "refinement_candidates_unevaluated":unevaluated.get(&family).cloned().unwrap_or_default()
        }));
        let costs: Vec<f64> = evaluated.iter().map(|c| c.verdict.cost_s).collect();
        per_family_cost.insert(family.as_str().to_string(), json!({
            "cells":costs.len(),"total_s":costs.iter().sum::<f64>(),
            "mean_s":if costs.is_empty(){0.0}else{costs.iter().sum::<f64>() / costs.len() as f64}
        }));
        for cell in evaluated {
            refusals.extend(cell.verdict.refusals.iter().cloned());
            cells_json.push(artifact_cell(cell)?);
        }
    }
    let verdict = if surviving.is_empty() {
        "no-arrival-admissible-candidate-in-frozen-search-space".to_string()
    } else {
        format!(
            "arrival-admissible: {}",
            surviving
                .iter()
                .map(|f| f.as_str().to_string())
                .collect::<Vec<_>>()
                .join(",")
        )
    };
    let search_space: BTreeMap<_, _> = Family::ALL
        .into_iter()
        .map(|family| {
            let grid = coarse_grid(family);
            (
                family.as_str().to_string(),
                json!({"point_count":grid.len(),"grid":grid}),
            )
        })
        .collect();
    let hours = context.gate_hours();
    let unexposed: Vec<_> = (0_i64..24).filter(|h| !hours.contains(h)).collect();
    let commit = commit.expect("full run has commit");
    let (head, clean) = fresh_tree_state().map_err(|e| anyhow!(e.to_string()))?;
    if !clean || head != commit {
        bail!("the artifact is unbound");
    }
    let artifact = json!({
        "binding":{"harness_tree_commit":commit,"clean_tree":true,"schema_version":2,
            "input_hashes":{measure.to_string_lossy().to_string():sha256_file(&measure).map_err(|e|anyhow!(e.to_string()))?},
            "exposure":{"instrument":"MNQ","preset":"crates/mogwai-server/presets/mnq.toml",
                "window_start_ns":context.binding.window_start_ns,"window_length_ns":context.binding.window_length_ns,
                "warmup":context.binding.warmup,"divergence":Value::Null,"regime":"neutral"},
            "stage_a_seeds":STAGE_A_SEEDS,"coarse_seeds":&STAGE_A_SEEDS[..2],"gate_hours":hours,
            "unexposed_hours":unexposed,"tape_protocol_version":mogwai_data::TAPE_PROTOCOL_VERSION,
            "kernel_version":mogwai_data::ARRIVAL_KERNEL_VERSION,
            "spec":"notes/protocol-12b-arrival-composition-spec.md section 9, bricks A0 and A"},
        "search_space":search_space,"cells":cells_json,"admissible_region":regions_json,"refusals":refusals,
        "a3_gated_hours":A3_GATED_HOURS,
        "envelope_cost":{"probe_costs_per_family_per_k":envelope_probes,
            "aggregate_demand_s":envelope_cost_s(),"budget_s":STAGE_A_ENVELOPE_BUDGET_S,
            "stopped_on_shortfall":false},
        "cost":{"jobs":jobs,"coarse_s":coarse_s,"refine_s":refine_s,"total_s":total.elapsed().as_secs_f64(),
            "peak_rss_bytes":peak_rss,"per_family_cell_s":per_family_cost,
            "budgets":{"STAGE_A_CELL_BUDGET_S":STAGE_A_CELL_BUDGET_S,
                "STAGE_A_GEN_CELL_BUDGET_S":STAGE_A_GEN_CELL_BUDGET_S,
                "STAGE_A_BUDGET_S":STAGE_A_BUDGET_S,
                "STAGE_A_ENVELOPE_BUDGET_S":STAGE_A_ENVELOPE_BUDGET_S,
                "STAGE_B_ENVELOPE_BUDGET_S":STAGE_B_ENVELOPE_BUDGET_S,
                "STAGE_B_BUDGET_S":STAGE_B_BUDGET_S,
                "STAGE_A_RSS_BYTES":STAGE_A_RSS_BYTES}},
        "verdict":verdict
    });
    write_artifact(&out, &artifact, total.elapsed().as_secs_f64(), peak_rss)
        .map_err(|e| anyhow!(e.to_string()))?;
    sidecar::kv("coarse_s", format!("{coarse_s:.3}"));
    sidecar::kv("refine_s", format!("{refine_s:.3}"));
    report_work(peak_rss);
    Ok(artifact)
}

fn refuse_committed_census_path(path: &std::path::Path) -> anyhow::Result<()> {
    let root = std::env::current_dir()?;
    let candidate = if path.is_absolute() {
        path.strip_prefix(&root).unwrap_or(path)
    } else {
        path
    };
    if candidate == std::path::Path::new(DEFAULT_OUT) {
        bail!("--demand-census may not write the screen artifact");
    }
    let output = Command::new("git")
        .args(["ls-files", "--error-unmatch", "--"])
        .arg(candidate)
        .output()?;
    if output.status.success() {
        bail!("--demand-census may not overwrite a committed artifact path");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    fn args(measure: &str, out: &str, cost_probe: bool) -> ArrivalScreenArgs {
        ArrivalScreenArgs {
            measure: Some(measure.into()),
            out: Some(out.into()),
            cost_probe,
            demand_census: None,
            cache: None,
            jobs: Some(1),
        }
    }

    /// The clean-tree attestation runs BEFORE the 12a artifact is opened, so a
    /// dirty tree is reported as a dirty tree rather than as whatever the
    /// inputs happen to say. Pinned by pointing the run at a path that cannot
    /// be read: whichever refusal comes back names which check ran first.
    ///
    /// Both tree states are asserted rather than one being skipped - the pin is
    /// the ORDER, and the order is visible from either state.
    #[test]
    fn arrival_screen_refuses_a_dirty_tree_before_reading_inputs() {
        let (_, clean) = fresh_tree_state().expect("a git tree");
        let out = "target/stage-a-dirty-tree-test.json";
        drop(std::fs::remove_file(out));
        let error = run(args(
            "analysis/this-12a-artifact-does-not-exist.json",
            out,
            false,
        ))
        .expect_err("an unreadable measure path cannot produce an artifact")
        .to_string();
        if clean {
            assert!(
                !error.contains("clean"),
                "the tree is clean, so the input read is what must fail: {error}"
            );
        } else {
            assert!(
                error.contains("clean") || error.contains("dirty"),
                "a dirty tree must be refused before the input is read: {error}"
            );
        }
        assert!(!Path::new(out).exists());
    }

    #[test]
    fn arrival_screen_refuses_zero_workers_before_touching_inputs() {
        let out = "target/stage-a-zero-jobs-test.json";
        drop(std::fs::remove_file(out));
        let mut args = args("analysis/this-12a-artifact-does-not-exist.json", out, false);
        args.jobs = Some(0);
        let error = run(args)
            .expect_err("zero workers cannot run the screen")
            .to_string();
        assert!(error.contains("--jobs must be at least 1"), "{error}");
        assert!(!Path::new(out).exists());
    }

    /// `--cost-probe` writes no committed artifact, so it may not demand a
    /// clean tree - its whole purpose is to be run before the work that
    /// produces one. The probe's four measured cells are month-scale walks and
    /// are NOT run here: what this pins is that the probe path reaches its
    /// inputs with the tree in whatever state it is in, and that it names no
    /// output file. The measured run itself is brick A0's own gate, an
    /// orchestrated `arrival-screen --cost-probe` invocation.
    #[test]
    fn arrival_screen_cost_probe_needs_no_clean_tree_and_writes_no_artifact() {
        let out = "target/stage-a-cost-probe-test.json";
        drop(std::fs::remove_file(out));
        let error = run(args(
            "analysis/this-12a-artifact-does-not-exist.json",
            out,
            true,
        ))
        .expect_err("an unreadable measure path stops the probe")
        .to_string();
        assert!(
            !error.contains("clean") && !error.contains("dirty"),
            "the probe consulted the tree state: {error}"
        );
        assert!(!Path::new(out).exists());
        // And the probe branch returns before any write even when it succeeds:
        // the artifact path is never touched on that path at all.
        assert!(!Path::new(DEFAULT_OUT).with_extension("json.tmp").exists());
    }

    #[test]
    fn demand_census_refuses_screen_output_and_any_out_pairing() {
        let mut direct = args(
            "analysis/this-12a-artifact-does-not-exist.json",
            DEFAULT_OUT,
            false,
        );
        direct.out = None;
        direct.demand_census = Some(DEFAULT_OUT.into());
        let error = run(direct)
            .expect_err("the census may not target the screen artifact")
            .to_string();
        assert!(error.contains("screen artifact"), "{error}");

        let mut paired = args(
            "analysis/this-12a-artifact-does-not-exist.json",
            "target/ordinary-screen.json",
            false,
        );
        paired.demand_census = Some("target/demand-census.json".into());
        let error = run(paired)
            .expect_err("--out and --demand-census must conflict")
            .to_string();
        assert!(error.contains("mutually exclusive with --out"), "{error}");
        assert!(!Path::new("target/ordinary-screen.json").exists());
        assert!(!Path::new("target/demand-census.json").exists());
    }

    #[test]
    fn demand_census_needs_no_clean_tree_and_writes_no_screen_artifact() {
        let census = "target/demand-census-no-input.json";
        drop(std::fs::remove_file(census));
        let mut census_args = args(
            "analysis/this-12a-artifact-does-not-exist.json",
            "target/unused-screen-output.json",
            false,
        );
        census_args.out = None;
        census_args.demand_census = Some(census.into());
        let error = run(census_args)
            .expect_err("an unreadable measure path stops the census")
            .to_string();
        assert!(
            !error.contains("clean") && !error.contains("dirty"),
            "the census consulted tree cleanliness: {error}"
        );
        assert!(!Path::new(census).exists());
        assert!(!Path::new(DEFAULT_OUT).with_extension("json.tmp").exists());
    }

    /// Spec section 6: every evaluated cell appears, admissible or not, with
    /// its per-condition verdicts. Run against the committed artifact once it
    /// exists; before then the test returns, exactly as the landed control's
    /// B8-absence pin does.
    #[test]
    fn the_screen_artifact_carries_every_evaluated_cell_and_its_verdict() {
        let path = Path::new(DEFAULT_OUT);
        if !path.exists() {
            return;
        }
        let artifact: Value =
            serde_json::from_slice(&std::fs::read(path).expect("the screen artifact"))
                .expect("valid JSON");
        assert_eq!(
            artifact["binding"]["tape_protocol_version"],
            json!(mogwai_data::TAPE_PROTOCOL_VERSION),
            "Stage A lands no generator change"
        );
        let cells = artifact["cells"].as_array().expect("cells");
        let mut coarse: BTreeMap<String, usize> = BTreeMap::new();
        for cell in cells {
            for key in [
                "family",
                "params",
                "lattice",
                "level",
                "pass",
                "a1",
                "a2",
                "a3",
                "a4",
                "admissible",
                "loss",
                "reported",
                "cost_s",
                "fitted_params",
            ] {
                assert!(
                    cell.get(key).is_some(),
                    "a cell record is missing {key}: {cell}"
                );
            }
            assert!(
                cell["admissible"].is_boolean(),
                "every evaluated cell carries a verdict"
            );
            if cell["admissible"] == json!(false) {
                assert!(cell["loss"].is_null(), "an inadmissible cell is not ranked");
            }
            for gate in ["a1", "a2", "a3", "a4"] {
                if cell[gate]["verdict"] == "unresolved" {
                    assert!(cell[gate]["passed"].is_null());
                    assert!(!cell["admissible"].as_bool().expect("cell verdict"));
                    assert!(
                        ["a1", "a2", "a3", "a4"].into_iter().any(|other| {
                            other != gate && cell[other]["passed"] == false
                        }),
                        "an unresolved gate cannot decide admissibility: {cell}"
                    );
                } else {
                    assert!(cell[gate]["passed"].is_boolean());
                    assert!(cell[gate].get("verdict").is_none());
                }
            }
            if cell["pass"] == json!("coarse") {
                *coarse
                    .entry(cell["family"].as_str().expect("family").to_string())
                    .or_default() += 1;
            }
        }
        // The coarse pass evaluates the frozen grid in full: no family may be
        // short a cell, which is what a silent trim would look like.
        for family in Family::ALL {
            assert_eq!(
                coarse.get(family.as_str()).copied().unwrap_or(0),
                coarse_grid(family).len(),
                "{} coarse cells",
                family.as_str()
            );
        }
        let verdict = artifact["verdict"].as_str().expect("a verdict");
        assert!(
            verdict.starts_with("arrival-admissible: ")
                || verdict == "no-arrival-admissible-candidate-in-frozen-search-space",
            "an artifact may carry no other verdict: {verdict}"
        );
    }
}
