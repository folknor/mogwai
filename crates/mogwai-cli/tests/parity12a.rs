// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE PARITY GATES of the retired rewrite plan, phase 2a: the unified
//! `mogwai_lab::measure12a` engine must reproduce the two committed 12a
//! record sets typed-canonical-identically.
//!
//! - `parity12a_observed_per_session_matches_the_committed_artifact` runs the
//!   engine over the delivered July corpus and compares against the
//!   `per_session` array of `analysis/out/mnq-measure12a-observed.json` -
//!   every session, every block, all 768 permutation records per session,
//!   every refusal.
//! - `parity12a_generated_walk_records_match_the_committed_cache` runs a
//!   fresh FINAL walk per seed 1-8 and compares against each
//!   `analysis/out/measure12a-cache/*.json` record with the `cost` field
//!   excluded (wall time and RSS are not reproducible by construction).
//!
//! They live HERE rather than in `mogwai-lab` because the generated side
//! needs preset/profile resolution from `mogwai-venue`, and `mogwai-lab`
//! must not depend on `mogwai-venue` - that would drag the axum stack under
//! the lab. The engine and its unit tests stay in the lab.
//!
//! Both are `#[ignore]`d: the observed gate reads a multi-GB corpus and the
//! generated gate runs eight month-long walks. Run them by name, in release:
//!
//! ```text
//! test -p mogwai-cli parity12a_observed
//! test -p mogwai-cli parity12a_generated
//! ```

use std::path::{Path, PathBuf};

use mogwai_lab::kernel::{canon_differences, typed_canon};

/// How many differing paths a failure reports. One leaf cannot tell a lone
/// last-ulp float from a whole block off its convention.
const DIFF_REPORT: usize = 25;

const CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
const OBSERVED: &str = "analysis/out/mnq-measure12a-observed.json";
const GENERATED_CACHE: &str = "analysis/out/measure12a-cache";

/// The repository root: the tests run with the crate directory as CWD, so
/// walk up to the workspace.
fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop(); // crates/
    dir.pop(); // repo root
    dir
}

fn read_json(path: &Path) -> serde_json::Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

/// Compare two JSON values under the type-strict canonical form, reporting
/// the FIRST differing path rather than dumping two 20 MB trees.
fn assert_canon_eq(got: &serde_json::Value, want: &serde_json::Value, what: &str) {
    if typed_canon(got) == typed_canon(want) {
        return;
    }
    let diffs = canon_differences(got, want, what, DIFF_REPORT);
    panic!(
        "{what} diverges from the committed record ({} shown, port first / committed second):\n  {}",
        diffs.len(),
        diffs.join("\n  ")
    );
}

// -- Gate 1: the observed side ----------------------------------------------

#[test]
#[ignore = "reads the multi-GB delivered July corpus; run explicitly in release"]
fn parity12a_observed_per_session_matches_the_committed_artifact() {
    let root = repo_root();
    let corpus = root.join(CORPUS);
    let committed = root.join(OBSERVED);
    assert!(
        corpus.is_dir(),
        "the delivered corpus is absent at {}",
        corpus.display()
    );
    assert!(
        committed.is_file(),
        "the committed record is absent at {}",
        committed.display()
    );

    let preflight = read_json(&root.join(PREFLIGHT));
    let usable: Vec<String> = preflight["usable_sessions"]
        .as_array()
        .expect("usable_sessions is an array")
        .iter()
        .map(|v| v.as_str().expect("a session label").to_string())
        .collect();
    assert_eq!(
        usable.len(),
        22,
        "the July delivery carries 22 usable sessions"
    );

    let files = mogwai_lab::stream::data_files(&corpus).expect("listing the corpus");
    let t0 = std::time::Instant::now();
    let records =
        mogwai_lab::measure12a::observed::observe(mogwai_lab::stream::parse_stream(files), &usable)
            .expect("the observed measurement pass refused");
    eprintln!(
        "[parity12a] observed pass: {:.1} s",
        t0.elapsed().as_secs_f64()
    );

    let want = read_json(&committed);
    let want_sessions = want["per_session"]
        .as_array()
        .expect("per_session is an array");
    assert_eq!(records.len(), want_sessions.len(), "session count");
    for (got, want) in records.iter().zip(want_sessions.iter()) {
        let date = got["session_date"].as_str().unwrap_or("?");
        assert_canon_eq(got, want, &format!("per_session[{date}]"));
    }
    // Belt and braces: the whole array in one canonical comparison, so a
    // difference in ORDER cannot slip through the pairwise loop.
    assert_canon_eq(
        &serde_json::Value::Array(records),
        &serde_json::Value::Array(want_sessions.clone()),
        "per_session",
    );
}

// -- Gate 2: the generated side ---------------------------------------------

/// One FINAL walk, constructed exactly the way `gen.rs`'s `build_source`
/// does: the committed MNQ preset, no overrides, the walk starting at
/// `FINAL_START_NS - SUMMARY_BURN_IN` with the vol trace enabled, measuring
/// `[FINAL_START_NS, FINAL_START_NS + FINAL_LENGTH)`.
fn run_final_walk(seed: u64) -> serde_json::Value {
    use mogwai_data::{TickEvent, TickSource};
    use mogwai_lab::subcontract::{FINAL_LENGTH, FINAL_START_NS, SUMMARY_BURN_IN};

    let profile = mogwai_venue::config::profile_from_preset("MNQ")
        .expect("the committed MNQ preset resolves");
    let calendar = profile
        .calendar
        .as_ref()
        .expect("MNQ carries a session calendar");
    let offset = i32::from(calendar.utc_offset_minutes);

    let start = u64::try_from(FINAL_START_NS).expect("the anchor is positive");
    let length_s: u64 = FINAL_LENGTH
        .trim_end_matches('s')
        .parse()
        .expect("FINAL_LENGTH is seconds");
    let end = start + length_s * 1_000_000_000;
    let burn_in_days: u64 = SUMMARY_BURN_IN
        .trim_end_matches('d')
        .parse()
        .expect("SUMMARY_BURN_IN days");
    let walk_start = start - burn_in_days * 86_400 * 1_000_000_000;

    let mut source = mogwai_data::GeneratedSource::try_new_with_session_profile(
        profile.scalars.clone(),
        seed,
        walk_start,
        mogwai_venue::source::fingerprint(),
        &profile.session,
        None, // no regime override: the committed preset drives the walk
        mogwai_data::SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    )
    .expect("building the generator");
    source.enable_vol_trace();

    let mut acc = mogwai_lab::measure12a::generated::GeneratedAcc::new(
        seed,
        start,
        end,
        offset,
        profile.scalars.modal_tick,
    );
    while let Some(event) = source.next_tick() {
        if event.ts_event() >= end {
            break;
        }
        match event {
            TickEvent::Quote(q) => {
                let trace = source.take_vol_trace();
                acc.push_quote(&q, trace).expect("quote");
            }
            TickEvent::Trade(t) => acc.push_trade(&t).expect("trade"),
        }
    }
    acc.finish()
        .expect("the generated measurement pass refused")
}

/// One seed's gate. Split per seed rather than looped inside one test so no
/// single test carries eight month-long walks (the runner caps a test's wall
/// time), and so a divergence names its seed in the failure itself.
fn assert_seed_parity(seed: u64) {
    let root = repo_root();
    let cache_dir = root.join(GENERATED_CACHE);
    assert!(
        cache_dir.is_dir(),
        "the walk cache is absent at {}",
        cache_dir.display()
    );

    // The cache is content-addressed, so index it by the seed it carries.
    let mut by_seed: std::collections::BTreeMap<u64, serde_json::Value> =
        std::collections::BTreeMap::new();
    for entry in std::fs::read_dir(&cache_dir).expect("listing the walk cache") {
        let path = entry.expect("a cache entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let value = read_json(&path);
        let Some(cached) = value["seed"].as_u64() else {
            continue;
        };
        assert!(
            by_seed.insert(cached, value).is_none(),
            "two cache records for seed {cached}"
        );
    }
    let seeds: Vec<u64> = mogwai_lab::subcontract::FINAL_SEEDS
        .iter()
        .map(|s| u64::try_from(*s).expect("positive seeds"))
        .collect();
    assert_eq!(
        by_seed.keys().copied().collect::<Vec<_>>(),
        seeds,
        "the cached seed set"
    );

    let t0 = std::time::Instant::now();
    let got = run_final_walk(seed);
    eprintln!(
        "[parity12a] seed {seed} walk: {:.1} s",
        t0.elapsed().as_secs_f64()
    );
    let mut want = by_seed[&seed].clone();
    // The cost field records wall time and peak RSS: not reproducible by
    // construction, and explicitly outside this gate.
    want.as_object_mut()
        .expect("a cache record is an object")
        .remove("cost");
    assert_canon_eq(&got, &want, &format!("seed {seed}"));
}

macro_rules! seed_gate {
    ($name:ident, $seed:expr) => {
        #[test]
        #[ignore = "runs a month-long walk; run explicitly in release"]
        fn $name() {
            assert_seed_parity($seed);
        }
    };
}

seed_gate!(parity12a_generated_seed_1, 1);
seed_gate!(parity12a_generated_seed_2, 2);
seed_gate!(parity12a_generated_seed_3, 3);
seed_gate!(parity12a_generated_seed_4, 4);
seed_gate!(parity12a_generated_seed_5, 5);
seed_gate!(parity12a_generated_seed_6, 6);
seed_gate!(parity12a_generated_seed_7, 7);
seed_gate!(parity12a_generated_seed_8, 8);
