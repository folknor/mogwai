// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{anyhow, bail};
use clap::Args;
use mogwai_lab::arrival_screen::{
    BudgetGuard, EvaluatedCell, Family, REFINEMENT_CELL_CAP, REFINEMENT_DEPTH, STAGE_A_BUDGET_S,
    STAGE_A_CELL_BUDGET_S, STAGE_A_GEN_CELL_BUDGET_S, STAGE_A_GEN_REFINE_CAP, STAGE_A_RSS_BYTES,
    STAGE_A_SEEDS, ScreenContext, admissible_regions, budget_verdict, budgeted, coarse_grid,
    coarse_lattice, evaluate_cell, probe_cell, refinement_round, write_artifact,
};
use mogwai_lab::ledger::{fresh_tree_state, require_clean_tree, sha256_file};
use serde_json::{Map, Value, json};

const DEFAULT_MEASURE: &str = "analysis/mnq-measure-12a.json";
const DEFAULT_OUT: &str = "analysis/mnq-arrival-screen.json";

#[derive(Args, Debug)]
pub struct ArrivalScreenArgs {
    #[arg(long, value_name = "PATH")]
    pub measure: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
    #[arg(long)]
    pub cost_probe: bool,
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
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

pub fn run(args: ArrivalScreenArgs) -> anyhow::Result<Value> {
    let measure = args.measure.unwrap_or_else(|| DEFAULT_MEASURE.into());
    let out = args.out.unwrap_or_else(|| DEFAULT_OUT.into());
    let commit = if args.cost_probe {
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
        let cells = budgeted(&mut guard, Family::ALL, |family| {
            let verdict = evaluate_cell(&context, &probe_cell(family), &STAGE_A_SEEDS[..2])?;
            let limit = if family == Family::EventMarkov {
                STAGE_A_GEN_CELL_BUDGET_S
            } else {
                STAGE_A_CELL_BUDGET_S
            };
            if verdict.cost_s > limit {
                return Err(mogwai_lab::error::LabError::refusal(format!(
                    "stage-a-cell-budget-exceeded: {family:?} cost {:.3}s exceeds {limit:.3}s",
                    verdict.cost_s
                )));
            }
            Ok(verdict)
        })
        .map_err(|e| anyhow!(e.to_string()))?;
        let (elapsed_s, peak_rss) = guard.finish().map_err(|e| anyhow!(e.to_string()))?;
        if let Some(verdict) = budget_verdict(elapsed_s, peak_rss) {
            bail!("{verdict}: total_s={elapsed_s:.3} peak_rss_bytes={peak_rss}");
        }
        let result = json!({"mode":"cost_probe","cells":cells,"peak_rss_bytes":peak_rss,
            "budgets":{"kernel_cell_s":STAGE_A_CELL_BUDGET_S,
            "generator_cell_s":STAGE_A_GEN_CELL_BUDGET_S,"total_s":STAGE_A_BUDGET_S,
            "rss_bytes":STAGE_A_RSS_BYTES}});
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(result);
    }

    let coarse_started = Instant::now();
    let mut by_family: BTreeMap<Family, Vec<EvaluatedCell>> = BTreeMap::new();
    for family in Family::ALL {
        let evaluated = budgeted(&mut guard, coarse_lattice(family), |point| {
            Ok(EvaluatedCell {
                verdict: evaluate_cell(&context, &point.cell, &STAGE_A_SEEDS[..2])?,
                lattice: point.lattice,
                level: 0,
                pass: "coarse",
            })
        })
        .map_err(|e| anyhow!(e.to_string()))?;
        by_family.insert(family, evaluated);
    }
    let coarse_s = coarse_started.elapsed().as_secs_f64();
    let refine_started = Instant::now();
    let mut unevaluated: BTreeMap<Family, BTreeMap<String, usize>> = BTreeMap::new();
    for family in Family::ALL {
        let cap = if family == Family::EventMarkov {
            STAGE_A_GEN_REFINE_CAP
        } else {
            REFINEMENT_CELL_CAP
        };
        let mut used = 0_usize;
        for round in 1..=REFINEMENT_DEPTH {
            let proposal = refinement_round(family, &by_family[&family], round, cap - used);
            unevaluated
                .entry(family)
                .or_default()
                .insert(format!("round_{round}"), proposal.unevaluated);
            used += proposal.candidates.len();
            let refined = budgeted(&mut guard, proposal.candidates, |point| {
                Ok(EvaluatedCell {
                    verdict: evaluate_cell(&context, &point.cell, &STAGE_A_SEEDS)?,
                    lattice: point.lattice,
                    level: point.level,
                    pass: "refine",
                })
            })
            .map_err(|e| anyhow!(e.to_string()))?;
            by_family
                .get_mut(&family)
                .expect("family initialized")
                .extend(refined);
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
        "binding":{"harness_tree_commit":commit,"clean_tree":true,"schema_version":1,
            "input_hashes":{measure.to_string_lossy().to_string():sha256_file(&measure).map_err(|e|anyhow!(e.to_string()))?},
            "exposure":{"instrument":"MNQ","preset":"crates/mogwai-server/presets/mnq.toml",
                "window_start_ns":context.binding.window_start_ns,"window_length_ns":context.binding.window_length_ns,
                "warmup":context.binding.warmup,"divergence":Value::Null,"regime":"neutral"},
            "stage_a_seeds":STAGE_A_SEEDS,"coarse_seeds":&STAGE_A_SEEDS[..2],"gate_hours":hours,
            "unexposed_hours":unexposed,"tape_protocol_version":mogwai_data::TAPE_PROTOCOL_VERSION,
            "kernel_version":mogwai_data::ARRIVAL_KERNEL_VERSION,
            "spec":"notes/protocol-12b-arrival-composition-spec.md section 9, bricks A0 and A"},
        "search_space":search_space,"cells":cells_json,"admissible_region":regions_json,"refusals":refusals,
        "cost":{"coarse_s":coarse_s,"refine_s":refine_s,"total_s":total.elapsed().as_secs_f64(),
            "peak_rss_bytes":peak_rss,"per_family_cell_s":per_family_cost,
            "budgets":{"STAGE_A_CELL_BUDGET_S":STAGE_A_CELL_BUDGET_S,
                "STAGE_A_GEN_CELL_BUDGET_S":STAGE_A_GEN_CELL_BUDGET_S,
                "STAGE_A_BUDGET_S":STAGE_A_BUDGET_S,"STAGE_A_RSS_BYTES":STAGE_A_RSS_BYTES}},
        "verdict":verdict
    });
    write_artifact(&out, &artifact, total.elapsed().as_secs_f64(), peak_rss)
        .map_err(|e| anyhow!(e.to_string()))?;
    Ok(artifact)
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
            cache: None,
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
