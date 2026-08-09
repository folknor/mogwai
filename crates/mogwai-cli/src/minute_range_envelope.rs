// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Brick B4's bound artifact.  The calculation deliberately reuses the
//! protocol-11 observed pass: later arrival bricks consume this committed
//! derivative instead of independently resampling the corpus.

use std::path::PathBuf;

use anyhow::{anyhow, bail};
use clap::Args;
use mogwai_lab::{
    fit::observe::observe,
    ledger::{require_clean_tree, verify_input},
    preflight::require_preflight,
    stream::{data_files, parse_stream},
    subcontract::{RESAMPLE_ENVELOPE_LEVEL, RESAMPLE_REPLICATES, RESAMPLE_SEED},
};
use serde_json::{Value, json};

const DEFAULT_CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const DEFAULT_LEDGER: &str = "analysis/databento-jobs.json";
const DEFAULT_PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
const DEFAULT_OUT: &str = "analysis/mnq-minute-range-envelope.json";

#[derive(Args)]
pub struct MinuteRangeEnvelopeArgs {
    #[arg(long)]
    corpus: Option<PathBuf>,
    #[arg(long)]
    ledger: Option<PathBuf>,
    #[arg(long)]
    preflight: Option<PathBuf>,
    #[arg(long)]
    out: Option<PathBuf>,
}

/// Runs the actual observed pass then binds the resampling result to this tree.
/// Kept public for the scratch-path regression test; it is intentionally not
/// given a test-only clean-tree escape hatch.
pub fn run(args: MinuteRangeEnvelopeArgs) -> anyhow::Result<Value> {
    let commit = require_clean_tree().map_err(|e| anyhow!("{e}"))?;
    let corpus = args.corpus.unwrap_or_else(|| DEFAULT_CORPUS.into());
    let ledger = args.ledger.unwrap_or_else(|| DEFAULT_LEDGER.into());
    let preflight = args.preflight.unwrap_or_else(|| DEFAULT_PREFLIGHT.into());
    let out = args.out.unwrap_or_else(|| DEFAULT_OUT.into());

    let hashes = verify_input(&corpus, &ledger).map_err(|e| anyhow!(e.to_string()))?;
    let (preflight_json, preflight_hash) =
        require_preflight(&hashes, &preflight).map_err(|e| anyhow!(e.to_string()))?;
    let usable = preflight_json["usable_sessions"]
        .as_array()
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| anyhow!("non-string usable session"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let observed = observe(
        parse_stream(data_files(&corpus).map_err(|e| anyhow!(e.to_string()))?),
        &usable,
    )
    .map_err(|e| anyhow!(e.to_string()))?;
    let envelope = observed["minute_range_envelope"].clone();
    if envelope["p99_lower"].is_null() || envelope["p99"].is_null() {
        bail!("observed pass did not produce a two-sided minute-range envelope");
    }
    let artifact = json!({
        "binding": {
            "harness_tree_commit": commit,
            "clean_tree": true,
            "inputs": { "corpus": corpus, "preflight": preflight, "file_hashes": hashes },
            "preflight_artifact_hash": preflight_hash,
            "corpus_job": preflight_json["job_id"],
            "method": {
                "name": "protocol-11 minute_range_envelope",
                "resample_seed": RESAMPLE_SEED,
                "replicates": RESAMPLE_REPLICATES,
                "upper_quantile": RESAMPLE_ENVELOPE_LEVEL,
            },
        },
        "envelope": envelope,
    });
    write_atomic(&out, &artifact)?;
    println!("minute-range envelope artifact -> {}", out.display());
    Ok(artifact)
}

fn write_atomic(path: &std::path::Path, value: &Value) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only meaningful on a DIRTY tree, and it must say so rather than assume
    /// it. Without the guard this test asserts the development tree is dirty,
    /// which makes the standing gate RED at every clean commit - exactly when
    /// the gate is most likely to be run, and the state every artifact command
    /// requires. Found 2026-08-09 by running the gate immediately after a
    /// commit; it had passed until then only because the tree happened to be
    /// dirty every previous time.
    ///
    /// The sibling in `arrival_control` carries the same guard for the same
    /// reason. On a clean tree the refusal cannot be provoked without dirtying
    /// the tree, which a unit test in the general lane must not do.
    #[test]
    fn minute_range_envelope_refuses_a_dirty_tree_before_reading_inputs() {
        if require_clean_tree().is_ok() {
            return;
        }
        let err = run(MinuteRangeEnvelopeArgs {
            corpus: None,
            ledger: None,
            preflight: None,
            out: Some("target/minute-range-envelope-test.json".into()),
        })
        .expect_err("this development tree is deliberately dirty");
        assert!(err.to_string().contains("working tree is dirty"));
    }
}
