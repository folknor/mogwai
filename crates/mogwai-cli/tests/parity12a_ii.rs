// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE GOLDEN GATE of notes/rust-rewrite-phases.md slice 2c-ii: running the
//! live `mogwai measure` driver ([`mogwai_cli::measure::run_measure`]) from
//! the clean committed tree must reproduce `analysis/mnq-measure-12a.json`
//! typed-canonical-identically except the honestly-live fields (top-level
//! `cost`, `binding.harness_tree_commit`, and each seed's own `cost` - the
//! same three exclusions slice 2c-i's gate uses and for the same reason:
//! see `crates/mogwai-lab/tests/parity12a_i.rs`).
//!
//! Shells nothing - it calls the driver function directly, with `--out`
//! (`cfg.out`) redirected to a SCRATCH path under `target/`, never the
//! committed artifact, and with `--cache-dir` redirected to a scratch copy
//! of the real `analysis/out` caches so the mandatory observed-cache
//! rewrite lands in scratch rather than mutating the committed
//! `mnq-measure12a-observed.json`'s on-disk bytes.
//!
//! `#[ignore]`d: it reads the full ~35M-row delivered corpus (observed pass,
//! ~80-90 s per the phase-2a gate timings) and runs the eight FINAL walks
//! in-process (~26 s each). Run explicitly, in release:
//!
//! ```text
//! brokkr test -p mogwai-cli parity12a_ii
//! ```
//!
//! REQUIRES A CLEAN GIT TREE, by construction: `run_measure` binds
//! `binding.harness_tree_commit` to `HEAD` and refuses outright over a
//! dirty working tree (`require_clean_tree`, ported faithfully from
//! `analysis/mnq_fit.py`). That is not a test-harness inconvenience - it is
//! the artifact's own binding contract, and this test exercises it exactly
//! as `mode_measure12a` does.

use std::path::{Path, PathBuf};

use mogwai_cli::measure::MeasureConfig;
use mogwai_lab::kernel::{canon_differences, typed_canon};

const DIFF_REPORT: usize = 25;
const OBSERVED_CACHE: &str = "analysis/out/mnq-measure12a-observed.json";
const WALK_CACHE: &str = "analysis/out/measure12a-cache";
const ARTIFACT: &str = "analysis/mnq-measure-12a.json";
const SCRATCH_ROOT: &str = "target/parity12a-ii-scratch";

fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

fn read_json(path: &Path) -> serde_json::Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn assert_canon_eq(got: &serde_json::Value, want: &serde_json::Value, what: &str) {
    if typed_canon(got) == typed_canon(want) {
        return;
    }
    let diffs = canon_differences(got, want, what, DIFF_REPORT);
    panic!(
        "{what} diverges from the committed artifact ({} shown, port first / committed second):\n  {}",
        diffs.len(),
        diffs.join("\n  ")
    );
}

/// A scratch copy of the real `analysis/out` caches, so the mandatory
/// observed-cache rewrite inside `run_measure` never touches the committed
/// bytes: this gate compares CONTENT against the committed artifact, it
/// does not require the committed cache files to stay byte-identical.
fn seed_scratch_cache(root: &Path) -> PathBuf {
    let scratch = root.join(SCRATCH_ROOT).join("cache");
    let walk_dst = scratch.join("measure12a-cache");
    std::fs::create_dir_all(&walk_dst).expect("creating the scratch cache dir");
    std::fs::copy(root.join(OBSERVED_CACHE), scratch.join("mnq-measure12a-observed.json"))
        .expect("seeding the scratch observed cache");
    for entry in std::fs::read_dir(root.join(WALK_CACHE)).expect("listing the walk cache") {
        let path = entry.expect("a cache entry").path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            let name = path.file_name().expect("a file name");
            std::fs::copy(&path, walk_dst.join(name)).expect("seeding a walk cache record");
        }
    }
    scratch
}

#[test]
#[ignore = "reads the full delivered corpus and runs eight in-process walks; run explicitly in \
            release, from a CLEAN git tree"]
fn parity12a_ii_live_run_matches_the_committed_artifact() {
    let root = repo_root();
    let cache_dir = seed_scratch_cache(&root);
    let out = root.join(SCRATCH_ROOT).join("mnq-measure-12a.json");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("creating the scratch output dir");
    }

    // The driver's defaults are CWD-relative (correct for the CLI run
    // from the repo root); cargo test's CWD is the crate dir, so the
    // real inputs are passed root-anchored.
    let cfg = MeasureConfig::resolve(
        Some(root.join("research/market-data/databento/mnqv/2026-07.full.tbbo")),
        Some(root.join("analysis/databento-jobs.json")),
        Some(root.join("analysis/out/mnq-fit-preflight.json")),
        Some(cache_dir),
        Some(out.clone()),
    );

    let outcome = mogwai_cli::measure::run_measure(&cfg)
        .unwrap_or_else(|e| panic!("mogwai measure refused: {e:#}"));

    println!("cost: {}", outcome.cost);
    println!("eligible: {}", outcome.artifact["ladder"]["eligible"]);
    println!("selected: {}", outcome.artifact["ladder"]["selected"]);
    println!("verdict: {}", outcome.artifact["ladder"]["verdict"]);
    assert_eq!(outcome.artifact["ladder"]["verdict"], "no-family-eligible");

    // The artifact must also have landed on disk at the scratch path -
    // never over the committed reference.
    let written = read_json(&out);
    assert_canon_eq(&written, &outcome.artifact, "the written artifact vs the returned one");

    let want = read_json(&root.join(ARTIFACT));
    let mut got_cmp = outcome.artifact.clone();
    let mut want_cmp = want.clone();
    got_cmp["cost"] = serde_json::Value::Null;
    want_cmp["cost"] = serde_json::Value::Null;
    got_cmp["binding"]["harness_tree_commit"] = serde_json::Value::Null;
    want_cmp["binding"]["harness_tree_commit"] = serde_json::Value::Null;
    for seed in got_cmp["generated"]["per_seed"].as_array_mut().unwrap() {
        seed["cost"] = serde_json::Value::Null;
    }
    for seed in want_cmp["generated"]["per_seed"].as_array_mut().unwrap() {
        seed["cost"] = serde_json::Value::Null;
    }
    assert_canon_eq(&got_cmp, &want_cmp, "artifact");
}
