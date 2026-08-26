// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai fit`: the protocol-11 session calibration, ported from
//! the retired Python fit implementation's `fit` mode (phase 3b of
//! the retired rewrite plan).
//!
//! The driver itself lives in `mogwai_lab::fit::driver`; this module is the
//! CLI surface plus `mode_fit`'s clean-tree binding, which is the same
//! contract `mogwai measure` carries: `binding.harness_tree_commit` must
//! name exactly the code that ran, and the Python's walk cache keys on that
//! commit, so a dirty tree refuses outright.

use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use mogwai_lab::aggregate::artifact::write_json_atomic;
use mogwai_lab::delivery::{fresh_tree_state, require_clean_tree};
use mogwai_lab::fit::driver::{FitConfig, run_fit};
use mogwai_lab::storage::{ScratchDir, artifact_path, cache_root};

const DEFAULT_CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const DEFAULT_JOBS_MANIFEST: &str = "analysis/databento-jobs.json";
const DEFAULT_PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
/// The Python-era scratch directory whose `cache/` subdirectory holds the
/// protocol-11 run's walk summaries.
const DEFAULT_PYTHON_CACHE_DIR: &str = "analysis/out/mnq-fit-scratch";
/// Deliberately not the committed `analysis/mnq-fit.json`: a bare `fit` run
/// must not overwrite the blessed artifact.
///
/// It named `target/mogwai-fit/` until 2026-08-20, which got the "not the
/// committed artifact" half right and the directory half wrong twice over.
/// `artifact_path` resolves a bare default against the working directory by
/// design - an artifact is the operator's file and is deliberately never
/// cached - so a fit run from anywhere but the repository root created a
/// directory literally called `target` under the operator's feet, reading as a
/// build directory that `cargo clean` would take. `analysis/out/` is this
/// repository's gitignored output directory and is what `preflight` and
/// `measure` already default into, so the whole repo-scoped toolbox now writes
/// its unblessed output to one honestly repo-shaped place.
const DEFAULT_OUT: &str = "analysis/out/mnq-fit.json";

#[derive(Args)]
pub struct FitArgs {
    /// The delivered corpus directory.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
    /// The Databento jobs manifest. Read-only.
    #[arg(long, value_name = "PATH")]
    jobs_manifest: Option<PathBuf>,
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
    /// Where to write the fit artifact. An artifact (storage policy): never
    /// cached, never auto-deleted. Defaults to `analysis/out/mnq-fit.json`,
    /// a distinct path in this repository's gitignored output directory, so a
    /// bare invocation can never clobber the committed `analysis/mnq-fit.json`.
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
    // The scratch class, same resolution as `arrival-control`'s. This was
    // `PathBuf::from("target/mogwai-fit/scratch")` - CWD-relative, so a fit
    // started anywhere but the repository root created a directory named
    // `target` under the operator's feet and left it there. What goes in it is
    // one small scratch config per walk. `ScratchDir` puts it under the storage
    // policy's cache root with a leaf unique to this process, and removes it on
    // drop, including on the early returns between here and the write.
    //
    // Bind the guard. `ScratchDir::new(..)?.path().to_path_buf()` compiles and
    // deletes the directory before the first walk.
    let scratch = ScratchDir::new(&cache_root(None))?;
    let cfg = resolve(args, &harness_commit, &harness_commit, scratch.path());
    let artifact = run_fit(&cfg).map_err(|e| anyhow!("the fit refused: {e}"))?;
    // The fit driver serialized `binding.harness_tree_commit` out of the
    // commit resolved above, so this artifact is tree-attested exactly as the
    // others are - and the roster test in `attestation` cannot see it, because
    // the serialization is in `mogwai-lab`. Refuse a scripted attestation
    // before the bytes reach disk.
    crate::attestation::refuse_scripted_tree_attestation()?;
    // And re-attest, the way `measure` and `arrival-control` do. The gate at
    // the top of this function ran minutes ago; `binding.harness_tree_commit`
    // claims to name exactly the code that ran, so a HEAD that moved or a tree
    // that went dirty in between makes that claim false and the artifact
    // unbound. Checking only at entry states the contract without keeping it.
    let (head, clean) = fresh_tree_state().map_err(|e| anyhow!("{e}"))?;
    if !clean || head != harness_commit {
        bail!("the tree changed during the fit run; the artifact is unbound");
    }
    // Atomic, like every sibling writer: a fit is a multi-minute run and a
    // bare `fs::write` truncates the previous artifact in place before it has
    // the new bytes, so an interruption leaves neither.
    write_json_atomic(&out, &artifact).with_context(|| format!("writing {}", out.display()))?;

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
pub fn resolve(
    args: &FitArgs,
    harness_commit: &str,
    default_cache_commit: &str,
    scratch_dir: &Path,
) -> FitConfig {
    let python_cache_dir = args.cache_dir.clone().or_else(|| {
        let repo_default = PathBuf::from(DEFAULT_PYTHON_CACHE_DIR);
        repo_default.is_dir().then_some(repo_default)
    });
    FitConfig {
        corpus: args
            .corpus
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_CORPUS)),
        jobs_manifest: args
            .jobs_manifest
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_JOBS_MANIFEST)),
        preflight: args
            .preflight
            .clone()
            .unwrap_or_else(|| PathBuf::from(DEFAULT_PREFLIGHT)),
        python_cache_dir,
        python_cache_commit: args
            .cache_commit
            .clone()
            .unwrap_or_else(|| default_cache_commit.to_string()),
        scratch_dir: scratch_dir.to_path_buf(),
        harness_commit: harness_commit.to_string(),
        native_cache: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::rc::Rc;

    use mogwai_lab::delivery::{ScriptedTree, TreeQuery, install_tree_oracle};

    fn missing_inputs(out: &str) -> FitArgs {
        FitArgs {
            corpus: Some("no/such/fit-corpus".into()),
            jobs_manifest: Some("no/such/fit-jobs-manifest.json".into()),
            preflight: Some("no/such/fit-preflight.json".into()),
            cache_dir: None,
            cache_commit: None,
            out: Some(out.into()),
        }
    }

    /// The N1 fit-side gate: `fit` is a call site of the shared
    /// `mogwai_lab::delivery::require_clean_tree`, and moving it must not change
    /// what an operator sees.
    ///
    /// Both verdicts are injected, and that is the whole design. This test
    /// used to return early on a clean tree - so on the state every gate run
    /// is meant to happen in, it asserted nothing at all - and the reason
    /// given was that calling `run` on a clean tree would launch a real fit.
    /// The seam removes both problems at once: no git is consulted, the tree
    /// state is whatever the test says, and the clean direction stops at the
    /// corpus that is not there.
    #[test]
    fn fit_refuses_a_dirty_tree_before_the_corpus_and_binds_a_clean_one() {
        let dirty = Rc::new(ScriptedTree::dirty("d1r7y"));
        let err = {
            let _guard = install_tree_oracle(Rc::clone(&dirty));
            run(&missing_inputs("target/fit-dirty-tree-test.json"))
                .expect_err("a dirty tree refuses")
        };
        assert!(
            err.to_string().contains("the working tree is dirty"),
            "{err}"
        );
        // The status read alone, so nothing past the gate ran: the fit never
        // reached the corpus it would have refused on next.
        assert_eq!(dirty.queries(), vec![TreeQuery::Status]);

        let clean = Rc::new(ScriptedTree::clean("c1ean"));
        let err = {
            let _guard = install_tree_oracle(Rc::clone(&clean));
            run(&missing_inputs("target/fit-clean-tree-test.json"))
                .expect_err("the corpus is not there either")
        };
        assert!(
            err.to_string().contains("the fit refused"),
            "a clean tree must be bound and the run carried into the fit: {err}"
        );
        assert_eq!(clean.queries(), vec![TreeQuery::Status, TreeQuery::Head]);
    }
}
