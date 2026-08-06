// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE PARITY GATE of notes/rust-rewrite-phases.md phase 2b: the aggregation
//! and inference layer must reproduce the committed artifact's inference
//! sections typed-canonical-identically, from the cached per-session records
//! phase 2a already proved.
//!
//! Inputs, all on disk and all committed:
//!
//! - `analysis/out/mnq-measure12a-observed.json` `per_session` (22 sessions),
//! - the eight `analysis/out/measure12a-cache/*.json` walk records (23
//!   sessions and one forensic record each),
//! - `bootstrap_multiplicities(22)`, computed internally.
//!
//! Compared against `analysis/mnq-measure-12a.json`:
//! `observed.monthly`, `observed.permutations_monthly`, the whole
//! `bootstrap` section (seed rule, replicate count and every `MetricRec`
//! field of all six families), the whole `ladder` section (every rung
//! record, `eligible`, `selected`, `verdict`), each seed's `blocks` and
//! `count_substitution`, `generated.central` and
//! `diagnostics.worsening_23`.
//!
//! Binding, cost, `refused_cells` and the validators are phase 2c's scope
//! and are deliberately NOT compared here.
//!
//! The artifact's semantics exercise the dark corners on purpose, and this
//! gate passing means the port reproduces them THROUGH the frozen rules
//! rather than by special case: the arrival family is incomplete with five
//! `force_refused` conditional metrics, the count substitution refuses most
//! hours, `worsening_23` is null via an unfired reversion rung, and the
//! verdict is `no-family-eligible`.
//!
//! `#[ignore]`d like the 2a gates: it reads ~65 MB of cached JSON and runs
//! 10,000 bootstrap replicates. Run it by name, in release:
//!
//! ```text
//! brokkr test -p mogwai-cli parity12a_aggregate
//! ```

use std::path::{Path, PathBuf};

use mogwai_lab::aggregate::assemble::{SeedRecord, measure};
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

#[test]
#[ignore = "reads the committed 12a caches and runs 10,000 bootstrap replicates; run explicitly in release"]
fn parity12a_aggregate_matches_the_committed_artifact() {
    let root = repo_root();
    let observed = read_json(&root.join(OBSERVED));
    let artifact = read_json(&root.join(ARTIFACT));

    let per_session: Vec<serde_json::Value> = observed["per_session"]
        .as_array()
        .expect("per_session is an array")
        .clone();
    assert_eq!(per_session.len(), 22, "the July delivery's usable sessions");

    // The walk cache is content-addressed, so index it by the seed it
    // carries and drive the seeds in ascending order.
    let cache_dir = root.join(GENERATED_CACHE);
    let mut by_seed: std::collections::BTreeMap<i64, serde_json::Value> =
        std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(&cache_dir).expect("listing the walk cache") {
        let path = entry.expect("a cache entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let value = read_json(&path);
        let Some(seed) = value["seed"].as_i64() else {
            continue;
        };
        assert!(
            by_seed.insert(seed, value).is_none(),
            "two cache records for seed {seed}"
        );
    }
    assert_eq!(
        by_seed.keys().copied().collect::<Vec<_>>(),
        mogwai_lab::subcontract::FINAL_SEEDS.to_vec(),
        "the cached seed set"
    );
    let seeds: Vec<SeedRecord> = by_seed
        .iter()
        .map(|(seed, value)| SeedRecord {
            seed: *seed,
            per_session: value["per_session"]
                .as_array()
                .expect("per_session is an array")
                .clone(),
            forensic: value["forensic"].clone(),
        })
        .collect();

    let t0 = std::time::Instant::now();
    let mults = bootstrap_multiplicities(per_session.len());
    let got = measure(&per_session, &seeds, &mults).expect("the measurement refused");
    eprintln!(
        "[parity12a] aggregation and inference: {:.1} s",
        t0.elapsed().as_secs_f64()
    );

    // -- The observed monthly aggregates.
    assert_canon_eq(
        &got.observed_monthly,
        &artifact["observed"]["monthly"],
        "observed.monthly",
    );
    assert_canon_eq(
        &got.observed_permutations_monthly,
        &artifact["observed"]["permutations_monthly"],
        "observed.permutations_monthly",
    );

    // -- The bootstrap section, every MetricRec field of all six families.
    assert_canon_eq(
        &got.ladder.bootstrap_json(got.replicates),
        &artifact["bootstrap"],
        "bootstrap",
    );

    // -- The ladder: every rung record, eligible, selected, verdict.
    assert_canon_eq(&got.ladder.ladder_json(), &artifact["ladder"], "ladder");

    // -- Each seed's blocks and count substitution.
    let want_seeds = artifact["generated"]["per_seed"]
        .as_array()
        .expect("per_seed is an array");
    assert_eq!(got.per_seed.len(), want_seeds.len(), "seed count");
    for ((seed, blocks, csub), want) in got.per_seed.iter().zip(want_seeds) {
        assert_eq!(Some(*seed), want["seed"].as_i64(), "seed order");
        assert_canon_eq(blocks, &want["blocks"], &format!("seed {seed} blocks"));
        assert_canon_eq(
            csub,
            &want["count_substitution"],
            &format!("seed {seed} count_substitution"),
        );
    }

    // -- The 8-seed central blocks and the central count substitution.
    assert_canon_eq(
        &got.central_json(),
        &artifact["generated"]["central"],
        "generated.central",
    );

    // -- The Amendment-E diagnostic.
    assert_canon_eq(
        &got.worsening_23_json(),
        &artifact["diagnostics"]["worsening_23"],
        "diagnostics.worsening_23",
    );

    // The dark corners the artifact actually carries, asserted by name so a
    // future regression that quietly makes them disappear fails loudly here
    // rather than silently widening a verdict.
    assert_eq!(got.ladder.verdict, "no-family-eligible");
    assert!(got.ladder.eligible.is_empty());
    assert!(got.ladder.selected.is_none());
    assert!(got.ladder.worsening_23.is_none());
    let arrival = got.ladder.envelope("arrival");
    assert!(
        !arrival.inventory_complete,
        "the arrival family is incomplete"
    );
    assert_eq!(
        arrival.metrics.iter().filter(|m| m.refused).count(),
        5,
        "the five force_refused conditional metrics"
    );
}
