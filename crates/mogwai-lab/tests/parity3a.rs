// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Phase-3a parity gates (`notes/rust-rewrite-phases.md`): reproduce the
//! committed `analysis/fingerprint.json` and `analysis/cadence.json` from
//! their recorded inputs, and the `check_cadence_feasible.py` structural
//! verdict. Ignored - they need `analysis/char_*.json` (gitignored, local
//! corpus output) and/or `research/market-data/*-trades-2026-06.zip` on
//! disk, and are named `parity3a_*` to match the `parity12a_*` convention.

use std::path::PathBuf;

use mogwai_lab::kernel::{canon_differences, typed_canon};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
#[ignore = "needs analysis/char_*.json (gitignored, local corpus output) and analysis/cadence.json on disk"]
fn parity3a_fingerprint_matches_the_committed_artifact() {
    let root = repo_root();
    let committed: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("analysis/fingerprint.json")).unwrap(),
    )
    .unwrap();
    let built = mogwai_lab::fingerprint::build_fingerprint(
        &root.join("analysis"),
        &root.join("analysis/cadence.json"),
    )
    .expect("fingerprint synthesis");

    let diffs = canon_differences(&committed, &built, "$", 20);
    // KNOWN, VERIFIED input drift, not a port defect: the on-disk
    // `analysis/char_*.json` files (gitignored, locally regenerated) no
    // longer reproduce the committed fingerprint.json's
    // `empirical_ranges.modal_tick.max` (0.25 in the commit; 0.1 from every
    // currently-committed pair's `returns.modal_tick`, XBTUSD's own value).
    // Confirmed independently: running `analysis/build_fingerprint.py`
    // itself (unmodified) against these same char_*.json files reproduces
    // 0.1, not 0.25 - so this is stale input, not a Rust/Python
    // disagreement. See notes/rust-rewrite-phases.md's phase-3a landing
    // record. Every other leaf, including every float in `session_profile`
    // and `golden_targets`, is typed-canon identical.
    let allowed: &[&str] = &["$.empirical_ranges.modal_tick.max"];
    let unexpected: Vec<&String> = diffs
        .iter()
        .filter(|d| !allowed.iter().any(|a| d.starts_with(*a)))
        .collect();
    assert!(
        unexpected.is_empty(),
        "rebuilt fingerprint diverges beyond the known input-drift leaf: {unexpected:#?}"
    );
}

#[test]
#[ignore = "needs research/market-data/{BTCUSDT,ETHUSDT,SOLUSDT}-trades-2026-06.zip on disk"]
fn parity3a_cadence_matches_the_committed_artifact() {
    let root = repo_root();
    let committed: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("analysis/cadence.json")).unwrap())
            .unwrap();
    let mut built =
        mogwai_lab::cadence::build(&root.join("research/market-data")).expect("cadence synthesis");

    // `provenance.generated_utc` is a live timestamp field, excluded from
    // comparison the same way the 12a parity gates exclude `cost` and
    // `binding.harness_tree_commit`.
    let mut committed = committed;
    committed["provenance"]
        .as_object_mut()
        .unwrap()
        .remove("generated_utc");
    built["provenance"]
        .as_object_mut()
        .unwrap()
        .entry("generated_utc")
        .or_insert(serde_json::Value::Null);
    committed["provenance"]
        .as_object_mut()
        .unwrap()
        .entry("generated_utc")
        .or_insert(serde_json::Value::Null);

    assert_eq!(
        typed_canon(&committed),
        typed_canon(&built),
        "rebuilt cadence diverges from the committed artifact"
    );
}

#[test]
fn parity3a_cadence_feasible_verdict_matches_the_committed_cadence() {
    let root = repo_root();
    let cadence: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("analysis/cadence.json")).unwrap())
            .unwrap();
    // `check_cadence_feasible.py`'s `verdict()` over the committed
    // cadence.json prints "parent/child verdict: PROCEED" and proceeds -
    // reproduced exactly here.
    assert_eq!(
        mogwai_lab::cadence_feasible::verdict(&cadence).expect("committed cadence is well formed"),
        "PROCEED"
    );
}
