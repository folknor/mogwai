// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Corpus-free counterfactual evaluation of Stage A's skipped A2 envelopes.

use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Instant;

use anyhow::{Context, anyhow, bail};
use clap::Args;
use mogwai_lab::arrival_envelope::predictive_envelopes;
use mogwai_lab::arrival_screen::{A2_SHAPE_BASE, A2_SHAPE_CAP, Cell, ScreenContext};
use mogwai_lab::ledger::{fresh_tree_state, sha256_file};
use serde_json::{Map, Value, json};

const DEFAULT_SCREEN: &str = "analysis/mnq-arrival-screen.json";
const DEFAULT_MEASURE: &str = "analysis/mnq-measure-12a.json";
const DEFAULT_OUT: &str = "analysis/mnq-arrival-envelope-diagnostic.json";

#[derive(Args, Debug)]
pub struct ArrivalEnvelopeDiagnosticArgs {
    #[arg(long, value_name = "PATH")]
    pub screen: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub measure: Option<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
    /// Independent envelope workers. Defaults to machine parallelism.
    #[arg(long)]
    pub jobs: Option<usize>,
}

#[derive(Clone)]
struct SelectedCell {
    family: String,
    params: Value,
    cell: Cell,
    level: Value,
    shape: Value,
}

fn normalized_from_root(path: &Path) -> anyhow::Result<PathBuf> {
    let root = std::env::current_dir()?;
    Ok(if path.is_absolute() {
        path.strip_prefix(&root).unwrap_or(path).to_path_buf()
    } else {
        path.to_path_buf()
    })
}

fn refuse_screen_output(path: &Path) -> anyhow::Result<()> {
    if normalized_from_root(path)? == Path::new(DEFAULT_SCREEN) {
        bail!("arrival-envelope-diagnostic may not write the official screen artifact");
    }
    let same_existing_file = Path::new(DEFAULT_SCREEN).exists()
        && path.exists()
        && std::fs::canonicalize(path)? == std::fs::canonicalize(DEFAULT_SCREEN)?;
    if same_existing_file {
        bail!("arrival-envelope-diagnostic may not write the official screen artifact");
    }
    Ok(())
}

fn command_text(program: &str, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new(program).args(args).output()?;
    if !output.status.success() {
        bail!("{program} failed while collecting artifact provenance");
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn cell_from_artifact(record: &Value) -> anyhow::Result<Cell> {
    let family = record["family"]
        .as_str()
        .ok_or_else(|| anyhow!("selected cell is missing family"))?;
    let mut object = record["params"]
        .as_object()
        .cloned()
        .ok_or_else(|| anyhow!("selected {family} cell is missing canonical params"))?;
    object.insert("family".into(), json!(family));
    serde_json::from_value(Value::Object(object)).context("decode selected canonical cell")
}

fn selected_cells(screen: &Value) -> anyhow::Result<Vec<SelectedCell>> {
    let cells = screen["cells"]
        .as_array()
        .ok_or_else(|| anyhow!("screen artifact has no cells array"))?;
    cells
        .iter()
        .filter(|cell| {
            cell["a1"]["passed"] == true
                && cell["a4"]["passed"] == true
                && cell["a2"]["verdict"] == "unresolved"
        })
        .map(|record| {
            if record["a3"]["passed"] != false || record["a3"].get("verdict").is_some() {
                bail!("a selected cell does not have A3 as its resolved hard failure");
            }
            let level = record["a2"]["level"]["per_seed"]
                .as_array()
                .ok_or_else(|| anyhow!("selected cell is missing A2 level ratios"))?
                .iter()
                .map(|row| json!({"seed":row["seed"],"ratio":row["ratio"]}))
                .collect();
            let shape = record["a2"]["shape"]["per_hour"]
                .as_array()
                .ok_or_else(|| anyhow!("selected cell is missing A2 shape deviations"))?
                .iter()
                .map(|row| json!({"hour":row["hour"],"deviation":row["deviation"]}))
                .collect();
            Ok(SelectedCell {
                family: record["family"]
                    .as_str()
                    .ok_or_else(|| anyhow!("selected cell is missing family"))?
                    .to_string(),
                params: record["params"].clone(),
                cell: cell_from_artifact(record)?,
                level: Value::Array(level),
                shape: Value::Array(shape),
            })
        })
        .collect()
}

fn evaluate(
    cell: &SelectedCell,
    grid: &mogwai_lab::arrival_envelope::ExposureGrid,
) -> anyhow::Result<Value> {
    let started = Instant::now();
    let outcome = predictive_envelopes(&cell.cell, grid, 2, true, false)
        .map_err(|error| anyhow!(error.to_string()))?;
    let envelope = outcome
        .a2
        .ok_or_else(|| anyhow!("A2 envelope was not returned"))?;
    let threshold = A2_SHAPE_CAP.min(A2_SHAPE_BASE + envelope);
    let deviations = cell
        .shape
        .as_array()
        .ok_or_else(|| anyhow!("A2 shape rows are missing"))?;
    let passed = deviations.iter().all(|row| {
        row["deviation"]
            .as_f64()
            .is_some_and(|deviation| deviation <= threshold)
    });
    Ok(json!({
        "family":cell.family,
        "params":cell.params,
        "a2_level_per_seed_ratios":cell.level,
        "a2_shape_raw_per_hour":cell.shape,
        "envelope_value":if envelope.is_finite(){Some(envelope)}else{None},
        "envelope_value_is_infinite":!envelope.is_finite(),
        "final_threshold":threshold,
        "ceiling_breached_replicates":outcome.ceiling_breached_replicates,
        "counterfactual_verdict":if passed{"passed"}else{"failed"},
        "wall_s":started.elapsed().as_secs_f64()
    }))
}

pub fn run(args: ArrivalEnvelopeDiagnosticArgs) -> anyhow::Result<Value> {
    if args.jobs == Some(0) {
        bail!("--jobs must be at least 1");
    }
    let jobs = args
        .jobs
        .unwrap_or_else(|| thread::available_parallelism().map_or(1, NonZeroUsize::get));
    let screen_path = args.screen.unwrap_or_else(|| DEFAULT_SCREEN.into());
    let measure_path = args.measure.unwrap_or_else(|| DEFAULT_MEASURE.into());
    let out = args.out.unwrap_or_else(|| DEFAULT_OUT.into());
    refuse_screen_output(&out)?;

    let total = Instant::now();
    let screen_bytes = std::fs::read(&screen_path)?;
    let screen: Value = serde_json::from_slice(&screen_bytes)?;
    if screen["binding"]["schema_version"] != 2 {
        bail!("arrival-envelope-diagnostic requires screen artifact schema version 2");
    }
    let selected = selected_cells(&screen)?;
    let context =
        ScreenContext::open(&measure_path, None).map_err(|error| anyhow!(error.to_string()))?;
    let grid = context
        .envelope_grid()
        .map_err(|error| anyhow!(error.to_string()))?;
    let next = Arc::new(AtomicUsize::new(0));
    let (send, receive) = mpsc::channel();
    thread::scope(|scope| {
        for _ in 0..jobs.min(selected.len().max(1)) {
            let send = send.clone();
            let next = Arc::clone(&next);
            let selected = &selected;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(cell) = selected.get(index) else {
                        break;
                    };
                    if send.send((index, evaluate(cell, grid))).is_err() {
                        break;
                    }
                }
            });
        }
    });
    drop(send);
    let mut evaluated: Vec<Option<Value>> = vec![None; selected.len()];
    for (index, result) in receive {
        evaluated[index] = Some(result?);
    }
    let cells: Vec<Value> = evaluated
        .into_iter()
        .map(|value| value.expect("worker returned every selected cell"))
        .collect();
    let passing = cells
        .iter()
        .filter(|cell| cell["counterfactual_verdict"] == "passed")
        .count();
    let (head, clean) = fresh_tree_state().map_err(|error| anyhow!(error.to_string()))?;
    let mut hashes = Map::new();
    hashes.insert(
        screen_path.to_string_lossy().into_owned(),
        json!(sha256_file(&screen_path).map_err(|error| anyhow!(error.to_string()))?),
    );
    hashes.insert(
        measure_path.to_string_lossy().into_owned(),
        json!(sha256_file(&measure_path).map_err(|error| anyhow!(error.to_string()))?),
    );
    let artifact = json!({
        "binding":{"commit":head,"clean_tree":clean,"input_hashes":hashes,
            "screen_binding_commit":screen["binding"]["harness_tree_commit"],
            "host_name":command_text("hostname", &[])?,"rustc_version":command_text("rustc", &["--version"])?},
        "selection":{"criteria":{"a1":"passed","a4":"passed","a2":"unresolved"},"selected":cells.len()},
        "counts":{"selected":cells.len(),"counterfactual_a2_passed":passing,"counterfactual_a2_failed":cells.len()-passing},
        "cells":cells,"cost":{"jobs":jobs,"total_wall_s":total.elapsed().as_secs_f64()}
    });
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = out.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&artifact)?)?;
    std::fs::rename(tmp, &out)?;
    println!("arrival envelope diagnostic -> {}", out.display());
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diagnostic_refuses_the_official_screen_artifact_path() {
        let error = run(ArrivalEnvelopeDiagnosticArgs {
            screen: Some("does-not-exist.json".into()),
            measure: Some("does-not-exist.json".into()),
            out: Some(DEFAULT_SCREEN.into()),
            jobs: Some(1),
        })
        .expect_err("the official artifact is never a diagnostic output")
        .to_string();
        assert!(
            error.contains("may not write the official screen artifact"),
            "{error}"
        );
    }

    #[test]
    fn committed_screen_selects_the_twenty_a3_only_failures() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../analysis/mnq-arrival-screen.json");
        let screen: Value = serde_json::from_slice(&std::fs::read(path).expect("screen artifact"))
            .expect("valid screen artifact");
        let selected = selected_cells(&screen).expect("selection");
        assert_eq!(selected.len(), 20);
        assert_eq!(
            selected
                .iter()
                .filter(|cell| cell.family == "log_ou_cox")
                .count(),
            17
        );
        assert_eq!(
            selected
                .iter()
                .filter(|cell| cell.family == "shot_noise")
                .count(),
            3
        );
    }
}
