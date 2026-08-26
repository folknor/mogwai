// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Phase 3b's binding parity gate (the retired rewrite plan): `run_fit`,
//! reading the delivered July corpus and replaying the protocol-11 run's
//! walk cache at `analysis/out/mnq-fit-scratch` under that run's harness
//! commit, reproduces the committed `analysis/mnq-fit.json`
//! typed-canon-identically minus the honestly-live fields.
//!
//! `#[ignore]`d like every other `parity*` gate: it needs the delivered
//! corpus and the Python-era cache on local disk, neither of which is in
//! the repo.
//!
//! Live-field exclusions, enumerated - the same discipline slice 2c-i and
//! 2c-ii applied to the 12a artifact:
//!
//! - `binding.harness_tree_commit`: the committed artifact binds the commit
//!   the Python ran from. Any Rust run binds whatever HEAD it ran from, so
//!   this field can only ever differ; it is not a measurement.
//! - `binding.subcontract_hash`: CONFIRMED stale-input drift, not a port
//!   defect. The committed artifact records
//!   `35e5b033133f12205cf26938db10d16b7bbd9f7f686cc82b91e265ffea1e1924`,
//!   which is the sub-contract as it stood at the protocol-11 fit. The
//!   protocol-12a constants joined `SUBCONTRACT_KEYS` afterwards, so
//!   the retired Python fit implementation's OWN `subcontract_hash()` returns
//!   `1ca79d9cd043e7ce4b8b633fdbcdf0547a02a26570ea9120eb0141254a8ad954`
//!   today - byte-identical to what this port computes, and to what the
//!   committed `analysis/out/mnq-fit-preflight.json` already records.
//!   Verified by running the Python directly, not inferred.
//! - `binding.preflight_artifact_hash`: same class. The committed artifact
//!   hashes the preflight FILE as it stood then
//!   (`adf6b8e7...`); the file on disk today hashes to `96013588...`,
//!   which is what both the Python and this port compute now. The
//!   `file_hashes` cross-check - the one that actually binds the corpus -
//!   passes, and `binding.file_hashes` itself IS compared.
//!
//! Nothing else is excluded. In particular the `solves` block (including
//! `evaluations`, `termination` and `final_score`), every `session_refit`
//! record, `landing_rule`, `verdicts`, `diagnostics` and the whole
//! `observed` block are compared byte-of-meaning. There is no timing field
//! in this artifact - `mnq-fit.json` carries no `cost` object, which is why
//! the exclusion list is shorter than 12a's.

use std::path::{Path, PathBuf};

use mogwai_lab::kernel::{canon_differences, typed_canon};
use serde_json::Value;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/crates/mogwai-cli.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate sits two levels under the repo root")
        .to_path_buf()
}

/// Strip the enumerated live fields from both sides before comparing.
fn without_live_fields(mut v: Value) -> Value {
    if let Some(binding) = v.get_mut("binding").and_then(Value::as_object_mut) {
        binding.remove("harness_tree_commit");
        binding.remove("subcontract_hash");
        binding.remove("preflight_artifact_hash");
    }
    v
}

#[test]
#[ignore = "needs the delivered corpus and the Python-era walk cache on local disk"]
fn parity3b_fit_matches_the_committed_artifact_over_the_python_walk_cache() {
    let root = repo_root();
    let committed_path = root.join("analysis/mnq-fit.json");
    let committed: Value = serde_json::from_slice(
        &std::fs::read(&committed_path).expect("the committed fit artifact"),
    )
    .expect("valid JSON");
    // The cache entries are keyed by the harness commit of the run that
    // produced them, which the artifact itself records.
    let cache_commit = committed["binding"]["harness_tree_commit"]
        .as_str()
        .expect("a bound commit")
        .to_string();

    let cfg = mogwai_lab::fit::driver::FitConfig {
        corpus: root.join("research/market-data/databento/mnqv/2026-07.full.tbbo"),
        jobs_manifest: root.join("analysis/databento-jobs.json"),
        preflight: root.join("analysis/out/mnq-fit-preflight.json"),
        python_cache_dir: Some(root.join("analysis/out/mnq-fit-scratch")),
        python_cache_commit: cache_commit,
        scratch_dir: root.join("target/parity3b-scratch"),
        // The gate is not a landing run; it binds nothing, so it records the
        // committed artifact's own commit and excludes the field from the
        // compare regardless.
        harness_commit: "parity3b-gate".to_string(),
        native_cache: None,
    };
    let got = mogwai_lab::fit::driver::run_fit(&cfg).expect("the fit");

    // Never overwrite a committed artifact: the gate's own output goes to
    // target/ scratch so a failure is inspectable.
    let scratch_out = root.join("target/parity3b-scratch/mnq-fit-rust.json");
    if let Some(p) = scratch_out.parent() {
        std::fs::create_dir_all(p).expect("scratch dir");
    }
    std::fs::write(
        &scratch_out,
        serde_json::to_vec_pretty(&got).expect("serializable"),
    )
    .expect("writing the scratch artifact");

    let a = without_live_fields(committed);
    let b = without_live_fields(got);
    if typed_canon(&a) != typed_canon(&b) {
        let diffs = canon_differences(&a, &b, "artifact", 40);
        panic!(
            "the fit artifact diverges from the committed one at {} places:\n{}",
            diffs.len(),
            diffs.join("\n")
        );
    }
}
