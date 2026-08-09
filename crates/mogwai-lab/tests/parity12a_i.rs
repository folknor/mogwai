// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE GATE of notes/rust-rewrite-phases.md slice 2c-i: assembling the
//! section-10 artifact PURELY FROM THE CACHES on disk (no live corpus pass,
//! no walks) reproduces `analysis/mnq-measure-12a.json` typed-canonically
//! identically except `cost` (a live measurement) and
//! `binding.harness_tree_commit` (HEAD moves between runs) - and that both
//! validators accept the result. Runs in seconds; `#[ignore]`d only because
//! it reads ~65 MB of committed JSON, same convention as the 2a/2b gates.
//!
//! ```text
//! test -p mogwai-lab parity12a_i
//! ```

use std::path::{Path, PathBuf};

use mogwai_lab::aggregate::artifact::{
    GeneratedSeed, assemble_measure12a_artifact, measure12a_schema_errors,
    measure12a_semantic_errors,
};
use mogwai_lab::aggregate::bootstrap::bootstrap_multiplicities;
use mogwai_lab::kernel::{canon_differences, typed_canon};

const DIFF_REPORT: usize = 25;

const OBSERVED: &str = "analysis/out/mnq-measure12a-observed.json";
const GENERATED_CACHE: &str = "analysis/out/measure12a-cache";
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

/// A provisional cost record, well inside every budget - the live cost
/// contract is slice 2c-ii's scope; this slice only proves the semantic
/// validator accepts a well-formed one.
fn provisional_cost() -> serde_json::Value {
    serde_json::json!({
        "observed_s": 1.0, "generated_s": 1.0, "bootstrap_s": 1.0,
        "total_s": 3.0, "peak_rss_bytes": 1, "scratch_bytes": 1,
    })
}

#[test]
#[ignore = "reads the committed 12a caches; run explicitly in release"]
fn parity12a_i_assembles_the_committed_artifact_from_cache() {
    let root = repo_root();
    let observed = read_json(&root.join(OBSERVED));
    let artifact = read_json(&root.join(ARTIFACT));

    let cache_dir = root.join(GENERATED_CACHE);
    let by_seed = mogwai_lab::aggregate::artifact::load_brick_g_walks(&cache_dir)
        .expect("the Brick G walk cache loads");
    let generated_seeds: Vec<GeneratedSeed> = by_seed
        .into_values()
        .map(|record| GeneratedSeed::from_cached(&record).expect("a well-formed cached record"))
        .collect();

    let usable: Vec<serde_json::Value> = observed["per_session"]
        .as_array()
        .expect("per_session is an array")
        .iter()
        .map(|r| r["session_date"].clone())
        .collect();

    let mults = bootstrap_multiplicities(observed["per_session"].as_array().unwrap().len());
    let binding_extra = &artifact["binding"];

    let got = assemble_measure12a_artifact(
        &observed,
        &generated_seeds,
        binding_extra,
        &mults,
        &provisional_cost(),
    )
    .expect("assembly refuses");

    // Compare everything typed-canon-identically except cost and
    // binding.harness_tree_commit.
    let mut got_cmp = got.clone();
    let mut want_cmp = artifact.clone();
    got_cmp["cost"] = serde_json::Value::Null;
    want_cmp["cost"] = serde_json::Value::Null;
    got_cmp["binding"]["harness_tree_commit"] = serde_json::Value::Null;
    want_cmp["binding"]["harness_tree_commit"] = serde_json::Value::Null;
    // Each seed's own `cost` (`walk_s`/`rss_bytes`) is ALSO a live wall-time
    // and process-memory measurement, pasted verbatim from whichever walk
    // most recently populated the Brick G cache file for that seed - it is
    // not reproducible from the committed artifact's own snapshot of it any
    // more than the top-level `cost` object is. Excluded from the compare
    // for the same reason.
    for seed in got_cmp["generated"]["per_seed"].as_array_mut().unwrap() {
        seed["cost"] = serde_json::Value::Null;
    }
    for seed in want_cmp["generated"]["per_seed"].as_array_mut().unwrap() {
        seed["cost"] = serde_json::Value::Null;
    }
    assert_canon_eq(&got_cmp, &want_cmp, "artifact");

    let schema_errs = measure12a_schema_errors(&got);
    assert!(
        schema_errs.is_empty(),
        "schema validator on the assembled artifact: {schema_errs:?}"
    );
    let semantic_errs = measure12a_semantic_errors(&got, &usable);
    assert!(
        semantic_errs.is_empty(),
        "semantic validator on the assembled artifact: {semantic_errs:?}"
    );

    // -- The negative batteries the Python selftest pins, re-anchored on
    // this real assembled artifact as the mutation baseline.

    // The public entrypoint refuses a truncated bootstrap population; the
    // internal body (used above) does not.
    let truncated = &mults[..100];
    let err = assemble_measure12a_artifact(
        &observed,
        &generated_seeds,
        binding_extra,
        truncated,
        &provisional_cost(),
    )
    .expect_err("a truncated bootstrap population must refuse");
    assert!(
        err.to_string().contains("replicates"),
        "refusal must name the replicate-count mismatch: {err}"
    );

    // A mixed-type usable list refuses by name.
    let mixed_usable: Vec<serde_json::Value> =
        vec![serde_json::json!("2026-07-01"), serde_json::json!(1)];
    let mixed_errs = measure12a_semantic_errors(&got, &mixed_usable);
    assert!(
        mixed_errs.iter().any(|e| e.contains("non-string")),
        "a mixed-type usable list must refuse by name: {mixed_errs:?}"
    );

    // An injected uncontracted field is rejected.
    let mut bad = got.clone();
    bad["observed"]["monthly"]["block4"]["19"]["session_votes"] = serde_json::json!(22);
    let bad_errs = measure12a_schema_errors(&bad);
    assert!(
        bad_errs.iter().any(|e| e.contains("block4")),
        "an injected uncontracted field must be rejected: {bad_errs:?}"
    );

    // The review mutation battery: each mutation must produce at least one
    // schema error.
    type Mutator = Box<dyn Fn(&mut serde_json::Value)>;
    let mutators: Vec<(&str, Mutator)> = vec![
        (
            "warmup key",
            Box::new(|a: &mut serde_json::Value| {
                a["diagnostics"]["warmup_exclusions"]["banana"] = serde_json::json!(1);
            }),
        ),
        (
            "Amendment E",
            Box::new(|a: &mut serde_json::Value| {
                a["ladder"]["rungs"][3]["fired"] = serde_json::json!(true);
                a["diagnostics"]["worsening_23"] =
                    serde_json::json!({"point": 0.0, "se": 1.0, "ucb": 0.0});
            }),
        ),
        (
            "duplicate refusal",
            Box::new(|a: &mut serde_json::Value| {
                let rec = serde_json::json!({
                    "scope": "family:child_walk", "cell": "x", "reason": "y",
                });
                let arr = a["diagnostics"]["refused_cells"].as_array_mut().unwrap();
                arr.push(rec.clone());
                arr.push(rec);
            }),
        ),
        (
            "null metric point",
            Box::new(|a: &mut serde_json::Value| {
                a["bootstrap"]["per_family"]["child_walk"]["metrics"][0]["point"] =
                    serde_json::Value::Null;
            }),
        ),
        (
            "bogus metric kind",
            Box::new(|a: &mut serde_json::Value| {
                a["bootstrap"]["per_family"]["child_walk"]["metrics"][0]["kind"] =
                    serde_json::json!("bogus");
            }),
        ),
        (
            "missing block4 all",
            Box::new(|a: &mut serde_json::Value| {
                a["observed"]["per_session"][0]["block4"]
                    .as_object_mut()
                    .unwrap()
                    .remove("all");
            }),
        ),
    ];
    for (name, mutator) in mutators {
        let mut m = got.clone();
        mutator(&mut m);
        let errs = measure12a_schema_errors(&m);
        assert!(
            !errs.is_empty(),
            "mutation {name:?} was not rejected by the schema validator"
        );
    }
}
