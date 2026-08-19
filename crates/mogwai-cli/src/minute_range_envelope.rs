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
    ledger::{fresh_tree_state, require_clean_tree, verify_input},
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

    // The refusal names the ledger it was verifying against: a bare io error
    // here reads as an unattributed "No such file or directory", which is
    // also indistinguishable from every other read this command performs.
    let hashes = verify_input(&corpus, &ledger).map_err(|e| {
        anyhow!(
            "verifying {} against {}: {e}",
            corpus.display(),
            ledger.display()
        )
    })?;
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
    // `clean_tree: true` below is asserted, not measured - the claim rests
    // entirely on the `require_clean_tree` at the top of this function, so a
    // scripted reader would write it unchallenged.
    crate::attestation::refuse_scripted_tree_attestation()?;
    // AND THE GATE AT THE TOP RAN BEFORE A FULL CORPUS PASS. `clean_tree:
    // true` and `harness_tree_commit` claim the tree was this commit and clean
    // when the artifact was produced; a HEAD that moved or a tree that went
    // dirty during the pass makes both false. Re-attest, as `measure`,
    // `arrival-control`, `arrival-screen` and `fit` do.
    let (head, clean) = fresh_tree_state().map_err(|e| anyhow!("{e}"))?;
    if !clean || head != commit {
        bail!("the tree changed during the minute-range envelope run; the artifact is unbound");
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

    use std::rc::Rc;

    use mogwai_lab::ledger::{ScriptedTree, TreeQuery, install_tree_oracle};

    fn missing_inputs(out: &str) -> MinuteRangeEnvelopeArgs {
        MinuteRangeEnvelopeArgs {
            corpus: Some("no/such/envelope-corpus".into()),
            ledger: Some("no/such/envelope-ledger.json".into()),
            preflight: Some("no/such/envelope-preflight.json".into()),
            out: Some(out.into()),
        }
    }

    /// The tree gate runs BEFORE any input is read, and both verdicts are
    /// injected so the claim is checked in the state the gate is actually run
    /// in. It used to return early on a clean tree - which is every gate run -
    /// after a 2026-08-09 fix for the inverse defect, asserting the
    /// development tree was dirty and going red at every clean commit. The
    /// early return cured the redness and preserved the vacuity; the seam
    /// removes both, because the tree state is now the test's to state.
    #[test]
    fn minute_range_envelope_refuses_a_dirty_tree_before_reading_inputs() {
        let dirty = Rc::new(ScriptedTree::dirty("d1r7y"));
        let err = {
            let _guard = install_tree_oracle(Rc::clone(&dirty));
            run(missing_inputs("target/minute-range-envelope-test.json"))
                .expect_err("a dirty tree refuses")
        };
        assert!(
            err.to_string().contains("the working tree is dirty"),
            "{err}"
        );
        // The refusal names the tree and NOT the ledger, and the query log
        // says why: the run stopped on the status read, so the inputs below
        // were never opened.
        assert!(
            !err.to_string().contains("no/such/envelope-ledger.json"),
            "the inputs were reached before the tree was checked: {err}"
        );
        assert_eq!(dirty.queries(), vec![TreeQuery::Status]);

        let clean = Rc::new(ScriptedTree::clean("c1ean"));
        let err = {
            let _guard = install_tree_oracle(Rc::clone(&clean));
            run(missing_inputs(
                "target/minute-range-envelope-clean-test.json",
            ))
            .expect_err("the corpus is not there either")
        };
        assert!(
            err.to_string().contains("no/such/envelope-ledger.json"),
            "a clean tree must be bound and the run carried into the inputs: {err}"
        );
        assert_eq!(clean.queries(), vec![TreeQuery::Status, TreeQuery::Head]);
    }
}
