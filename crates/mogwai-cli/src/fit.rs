// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai fit`: the protocol-11 session calibration, ported from
//! `analysis/mnq_fit.py`'s `fit` mode (phase 3b of
//! notes/rust-rewrite-phases.md).
//!
//! The driver itself lives in `mogwai_lab::fit::driver`; this module is the
//! CLI surface plus `mode_fit`'s clean-tree binding, which is the same
//! contract `mogwai measure` carries: `binding.harness_tree_commit` must
//! name exactly the code that ran, AND the Python's walk cache keys on that
//! commit, so a dirty tree refuses outright.

use std::path::PathBuf;

use anyhow::{Context, anyhow};
use clap::Args;
use mogwai_lab::fit::driver::{FitConfig, run_fit};
use mogwai_lab::ledger::require_clean_tree;
use mogwai_lab::storage::artifact_path;

const DEFAULT_CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const DEFAULT_LEDGER: &str = "analysis/databento-jobs.json";
const DEFAULT_PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
/// The Python-era scratch directory whose `cache/` subdirectory holds the
/// protocol-11 run's walk summaries.
const DEFAULT_PYTHON_CACHE_DIR: &str = "analysis/out/mnq-fit-scratch";
const DEFAULT_OUT: &str = "target/mogwai-fit/mnq-fit.json";

#[derive(Args)]
pub struct FitArgs {
    /// The delivered corpus directory.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
    /// The Databento job ledger. Read-only.
    #[arg(long, value_name = "PATH")]
    ledger: Option<PathBuf>,
    /// The committed preflight artifact this run's file hashes must match.
    #[arg(long, value_name = "PATH")]
    preflight: Option<PathBuf>,
    /// A pre-existing walk-cache directory (`mnq-fit-scratch`), read-only.
    /// Its entries are keyed by the harness commit of the run that produced
    /// them, so `--cache-commit` must name that commit for them to resolve.
    /// The surviving entries were written by the retired Python harness and
    /// cannot be refilled by it; this crate writes its own cache elsewhere,
    /// so the flag is for replaying that historical one.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// The harness commit the `--cache-dir` entries were keyed under.
    /// Defaults to this tree's HEAD, which is right for a fresh fit and
    /// wrong for replaying someone else's cache - hence the flag.
    #[arg(long, value_name = "SHA")]
    cache_commit: Option<String>,
    /// Where to write the fit artifact. An ARTIFACT (storage policy): never
    /// cached, never auto-deleted. Defaults UNDER `target/` so a bare
    /// invocation can never clobber the committed `analysis/mnq-fit.json`.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

/// `mode_fit`: bind the tree, run the fit, write the artifact, print the
/// verdict line the Python printed.
pub fn run(args: &FitArgs) -> anyhow::Result<()> {
    // Identity first, before a byte of CSV or a generator walk.
    let harness_commit = require_clean_tree().map_err(|e| anyhow!("{e}"))?;
    let out = artifact_path(args.out.as_deref(), DEFAULT_OUT);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let cfg = resolve(args, &harness_commit, &harness_commit);
    let artifact = run_fit(&cfg).map_err(|e| anyhow!("the fit refused: {e}"))?;

    let bytes = serde_json::to_vec_pretty(&artifact)?;
    std::fs::write(&out, &bytes).with_context(|| format!("writing {}", out.display()))?;

    let verdicts: serde_json::Map<String, serde_json::Value> = artifact["verdicts"]
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|(k, v)| (k, v["status"].clone()))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "verdicts": verdicts,
            "landing_set": artifact["landing_set"].clone(),
        }))?
    );
    println!("fit artifact -> {}", out.display());
    Ok(())
}

/// The config resolution, split out so the parity gate can build one
/// without going through clap or the clean-tree gate.
#[must_use]
pub fn resolve(args: &FitArgs, harness_commit: &str, default_cache_commit: &str) -> FitConfig {
    let python_cache_dir = args.cache_dir.clone().or_else(|| {
        let repo_default = PathBuf::from(DEFAULT_PYTHON_CACHE_DIR);
        repo_default.is_dir().then_some(repo_default)
    });
    FitConfig {
        corpus: args
            .corpus
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CORPUS)),
        ledger: args
            .ledger
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER)),
        preflight: args
            .preflight
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PREFLIGHT)),
        python_cache_dir,
        python_cache_commit: args
            .cache_commit
            .clone()
            .unwrap_or_else(|| default_cache_commit.to_string()),
        scratch_dir: PathBuf::from("target/mogwai-fit/scratch"),
        harness_commit: harness_commit.to_string(),
        native_cache: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The N1 fit-side gate: `fit` is now a call site of the SHARED
    /// `mogwai_lab::ledger::require_clean_tree`, and moving it must not change
    /// what an operator sees. Depends on the development tree being dirty, as
    /// its `minute_range_envelope` sibling does; on a clean tree calling `run`
    /// would launch a real fit, so the check is skipped rather than risked.
    #[test]
    fn fit_refuses_a_dirty_tree() {
        if require_clean_tree().is_ok() {
            return;
        }
        let err = run(&FitArgs {
            corpus: None,
            ledger: None,
            preflight: None,
            cache_dir: None,
            cache_commit: None,
            out: Some("target/fit-dirty-tree-test.json".into()),
        })
        .expect_err("this development tree is deliberately dirty");
        assert!(err.to_string().contains("working tree is dirty"));
    }
}
