// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE GOLDEN GATE of notes/rust-rewrite-phases.md slice 2c-ii/2c-iii:
//! running the `mogwai measure` driver ([`mogwai_cli::measure`]) from the
//! clean committed tree must reproduce `analysis/mnq-measure-12a.json`
//! typed-canonical-identically except the honestly-live fields (top-level
//! `cost`, `binding.harness_tree_commit`, and each seed's own `cost` - the
//! same three exclusions slice 2c-i's gate uses and for the same reason:
//! see `crates/mogwai-lab/tests/parity12a_i.rs`).
//!
//! Two tests, split on the OBLIGATION the 2c-ii golden-gate run recorded
//! (notes/rust-rewrite-phases.md): the monolithic single-test run (observed
//! pass plus eight in-process FINAL walks plus bootstrap) measured 288.1 s
//! total against `brokkr`'s hard 280 s per-test ceiling.
//!
//! - `parity12a_ii_fast_matches_the_committed_artifact_over_cached_walks`
//!   runs [`mogwai_cli::measure::WalkSource::PreAttestedCacheOnly`]: the
//!   observed pass is LIVE (~85 s, the authoritative cross-check and the
//!   piece this slice actually owns), but the eight walks are taken
//!   straight from the Brick G cache with NEITHER a fresh walk NOR the
//!   content-comparison attestation performed. That proves nothing about
//!   walk determinism on its own - it doesn't need to, because the nine
//!   `crates/mogwai-cli/tests/parity12a.rs` gates
//!   (`parity12a_observed_*`, `parity12a_generated_seed_1`..`_8`) already
//!   prove each walk reproduces its Brick G record independently, each
//!   comfortably under the ceiling. Together the two suites cover
//!   everything the monolithic test did; nothing this slice proved is
//!   weakened, the coverage moved to gates that fit. Fits in ~90 s.
//! - `parity12a_ii_live_full_run_matches_the_committed_artifact` is the
//!   real, full [`mogwai_cli::measure::WalkSource::LiveAttested`] path -
//!   `#[ignore]`d for cost, same as before, but ALSO documented as exceeding
//!   the per-test ceiling: run it directly via the release BINARY, not
//!   `brokkr test`, exactly as the 2c-ii golden-gate demonstration did (see
//!   the doc comment on that test for the invocation).
//!
//! Both call the driver directly (`mogwai_cli::measure::run_measure_with`),
//! shelling nothing, with `--out` (`cfg.out`) redirected to a SCRATCH path
//! under `target/`, never the committed artifact, and `--cache-dir`
//! redirected to a scratch copy of the real `analysis/out` caches so the
//! mandatory observed-cache rewrite never touches the committed file's
//! bytes.
//!
//! BOTH REQUIRE A CLEAN GIT TREE, by construction: `run_measure`/
//! `run_measure_with` bind `binding.harness_tree_commit` to `HEAD` and
//! refuse outright over a dirty working tree (`require_clean_tree`, ported
//! faithfully from `analysis/mnq_fit.py`). That is not a test-harness
//! inconvenience - it is the artifact's own binding contract.

use std::path::{Path, PathBuf};

use mogwai_cli::measure::{MeasureConfig, WalkSource};
use mogwai_lab::kernel::{canon_differences, typed_canon};

const DIFF_REPORT: usize = 25;
const OBSERVED_CACHE: &str = "analysis/out/mnq-measure12a-observed.json";
const WALK_CACHE: &str = "analysis/out/measure12a-cache";
const ARTIFACT: &str = "analysis/mnq-measure-12a.json";

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
/// observed-cache rewrite inside `run_measure_with` never touches the
/// committed bytes: this gate compares CONTENT against the committed
/// artifact, it does not require the committed cache files to stay
/// byte-identical.
fn seed_scratch_cache(root: &Path, scratch_root: &str) -> PathBuf {
    let scratch = root.join(scratch_root).join("cache");
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

fn assert_matches_committed(artifact: &serde_json::Value, root: &Path) {
    let want = read_json(&root.join(ARTIFACT));
    let mut got_cmp = artifact.clone();
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

#[test]
#[ignore = "reads the full delivered corpus; run explicitly in release, from a CLEAN git tree"]
fn parity12a_ii_fast_matches_the_committed_artifact_over_cached_walks() {
    let root = repo_root();
    let cache_dir = seed_scratch_cache(&root, "target/parity12a-ii-fast-scratch");
    let out = root.join("target/parity12a-ii-fast-scratch/mnq-measure-12a.json");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("creating the scratch output dir");
    }

    // The driver's defaults are CWD-relative (correct for the CLI run from
    // the repo root); cargo test's CWD is the crate dir, so the real
    // inputs are passed root-anchored.
    let cfg = MeasureConfig::resolve(
        Some(root.join("research/market-data/databento/mnqv/2026-07.full.tbbo")),
        Some(root.join("analysis/databento-jobs.json")),
        Some(root.join("analysis/out/mnq-fit-preflight.json")),
        Some(cache_dir),
        Some(out.clone()),
    );

    let outcome = mogwai_cli::measure::run_measure_with(&cfg, WalkSource::PreAttestedCacheOnly)
        .unwrap_or_else(|e| panic!("mogwai measure refused: {e:#}"));

    println!("cost: {}", outcome.cost);
    println!("verdict: {}", outcome.artifact["ladder"]["verdict"]);
    assert_eq!(outcome.artifact["ladder"]["verdict"], "no-family-eligible");

    let written = read_json(&out);
    assert_canon_eq(&written, &outcome.artifact, "the written artifact vs the returned one");
    assert_matches_committed(&outcome.artifact, &root);
}

#[test]
#[ignore = "reads the full delivered corpus and runs eight in-process walks (measured ~288 s \
            total, over brokkr's hard 280 s per-test ceiling): run the release BINARY directly, \
            not `brokkr test`, from a CLEAN git tree - e.g. `cargo build --release -p mogwai-cli` \
            then drive `mogwai_cli::measure::run_measure_with(&cfg, WalkSource::LiveAttested)` \
            (or the equivalent `target/release/mogwai measure --cache-dir <scratch> --out \
            <scratch>` invocation) exactly as the 2c-ii golden-gate demonstration did - see the \
            2c-ii landing record in notes/rust-rewrite-phases.md for the measured cost fields \
            (observed 83.0 s, generated 202.1 s, bootstrap 3.1 s, total 288.1 s, peak RSS \
            2.93 GiB, scratch 93 MB) and verdict (no-family-eligible) from that run"]
fn parity12a_ii_live_full_run_matches_the_committed_artifact() {
    let root = repo_root();
    let cache_dir = seed_scratch_cache(&root, "target/parity12a-ii-live-scratch");
    let out = root.join("target/parity12a-ii-live-scratch/mnq-measure-12a.json");
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).expect("creating the scratch output dir");
    }

    let cfg = MeasureConfig::resolve(
        Some(root.join("research/market-data/databento/mnqv/2026-07.full.tbbo")),
        Some(root.join("analysis/databento-jobs.json")),
        Some(root.join("analysis/out/mnq-fit-preflight.json")),
        Some(cache_dir),
        Some(out.clone()),
    );

    let outcome = mogwai_cli::measure::run_measure_with(&cfg, WalkSource::LiveAttested)
        .unwrap_or_else(|e| panic!("mogwai measure refused: {e:#}"));

    println!("cost: {}", outcome.cost);
    println!("eligible: {}", outcome.artifact["ladder"]["eligible"]);
    println!("selected: {}", outcome.artifact["ladder"]["selected"]);
    println!("verdict: {}", outcome.artifact["ladder"]["verdict"]);
    assert_eq!(outcome.artifact["ladder"]["verdict"], "no-family-eligible");

    let written = read_json(&out);
    assert_canon_eq(&written, &outcome.artifact, "the written artifact vs the returned one");
    assert_matches_committed(&outcome.artifact, &root);
}
