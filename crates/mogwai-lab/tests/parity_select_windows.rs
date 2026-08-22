// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Phase 4b item 2: the `select_windows.py` port against its blessed reference.
//!
//! This absorption had no frozen artifact, so one was made first -
//! `analysis/select-windows-blessed.json`, written by
//! `scripts/bless_select_windows.py` from the Python's own functions while it
//! was still runnable. This gate matches the port to it.
//!
//! `#[ignore]`d and named `parity_select_windows_` because it re-reads the four
//! CME archives, roughly 1.5 GB living outside the tree under `research/`.
//! Everything downstream of the feature sweep is checked in the same run, so
//! one archive pass covers the whole pipeline.
//!
//! WHAT IS COMPARED IS THE STRUCTURE, not printed tables: per-month medians,
//! the eligible span, the z-scored vectors in key order, the seeds and the
//! selection. Floats are compared BIT-EXACTLY, and that is a demand rather than
//! optimism.
//!
//! Why bit equality is the right bar here needs stating precisely, because an
//! earlier version of this note overclaimed. The `sum()` SITES - the variance
//! sums, the z-score mean and variance, the farthest-point distances - go
//! through `py_sum`, which reproduces CPython's Neumaier compensation, and the
//! integer divisions go through `py_int_div`. But `ret2` and the hourly buckets
//! are NOT those: they are naive `+=` accumulations, because the Python
//! accumulates them in a loop rather than through `sum()`, and a compensated
//! sum there would be the mismatch. So the claim is not "everything is
//! compensated" - it is that each site matches the CPython construct it was
//! ported from, which is why anything less than bit equality is a defect rather
//! than a tolerance question.
//!
//! One deviation is deliberate and does not appear here: `select_windows::squared`
//! uses a multiply where the Python uses `** 2`. It changes eleven values in
//! the CACHE, none of which reaches a surviving monthly median on the committed
//! archives, so this artifact-level gate cannot see it. That is what
//! `cache_deviations` and `scripts/compare_cme_caches.py` are for.

use std::collections::BTreeMap;
use std::path::PathBuf;

use mogwai_lab::select_windows::{self as sw, Cache};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn blessed() -> serde_json::Value {
    let path = root().join("analysis/select-windows-blessed.json");
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
    )
    .expect("the blessed artifact parses")
}

fn build_cache() -> Cache {
    let market = root().join("research/market-data");
    sw::ARCHIVES
        .iter()
        .map(|(symbol, file)| {
            let path = market.join(file);
            let sessions = sw::build_features(&path)
                .unwrap_or_else(|e| panic!("building features for {}: {e}", path.display()));
            ((*symbol).to_string(), sessions)
        })
        .collect()
}

/// Bit equality, reported with both bit patterns so a failure says how far off
/// it is rather than just that it differs.
fn assert_bits(label: &str, got: f64, want: f64) {
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "{label}: {got:?} ({:016x}) against the blessed {want:?} ({:016x})",
        got.to_bits(),
        want.to_bits()
    );
}

#[test]
#[ignore = "re-reads the four CME archives under research/market-data"]
fn parity_select_windows_reproduces_the_blessed_reference() {
    let reference = blessed();
    let cache = build_cache();

    // THE ARCHIVES THEMSELVES, first and by DIGEST. A session count is not an
    // identity: archives can change while the qualifying-session count holds,
    // and changes confined to discarded sessions are exactly the class the
    // monthly artifact cannot see - which is the same blind spot that hid the
    // squaring deviation one layer below the blessed comparison. This assertion
    // used to check counts alone while its message claimed to establish that
    // the archives were the blessed ones. It did not.
    let market = root().join("research/market-data");
    let provenance = reference["provenance"]["archives"]
        .as_object()
        .expect("archive provenance");
    for (symbol, file) in sw::ARCHIVES {
        let recorded = &provenance[symbol];
        let path = market.join(file);
        let bytes =
            std::fs::read(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        assert_eq!(
            u64::try_from(bytes.len()).unwrap(),
            recorded["bytes"].as_u64().expect("recorded size"),
            "{symbol}: {file} is not the size the artifact was blessed over"
        );
        let digest = <sha2::Sha256 as sha2::Digest>::digest(&bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            recorded["sha256"].as_str().expect("recorded digest"),
            "{symbol}: {file} is not the archive the artifact was blessed over"
        );
    }

    let sessions = &reference["provenance"]["sessions_per_symbol"];
    for (symbol, rows) in &cache {
        assert_eq!(
            u64::try_from(rows.len()).unwrap(),
            sessions[symbol].as_u64().expect("session count"),
            "{symbol}: qualifying-session count differs"
        );
    }

    let months = sw::monthly(&cache).expect("month table");
    let blessed_monthly = reference["monthly"]
        .as_object()
        .expect("the blessed month table");
    assert_eq!(
        months.len(),
        blessed_monthly.len(),
        "month count differs from the blessed reference"
    );
    for (month, row) in &months {
        let want = blessed_monthly
            .get(month)
            .unwrap_or_else(|| panic!("{month} is absent from the blessed month table"));
        for (key, value) in row {
            let expected = want[key].as_f64().unwrap_or_else(|| {
                panic!("{month}.{key} is absent from the blessed row");
            });
            assert_bits(&format!("monthly {month}.{key}"), *value, expected);
        }
    }

    let selection = sw::select(&months).expect("selection");

    assert_eq!(
        selection.eligible_count,
        usize::try_from(reference["eligible"]["count"].as_u64().unwrap()).unwrap()
    );
    assert_eq!(
        selection.eligible_first,
        reference["eligible"]["first"].as_str().unwrap()
    );
    assert_eq!(
        selection.eligible_last,
        reference["eligible"]["last"].as_str().unwrap()
    );

    // The key ORDER is load-bearing: it is the layout of every vector, so a
    // permutation would leave the z-scores individually right and the distances
    // wrong.
    let blessed_keys: Vec<&str> = reference["zscore_keys"]
        .as_array()
        .expect("keys")
        .iter()
        .map(|k| k.as_str().expect("key"))
        .collect();
    assert_eq!(selection.keys, blessed_keys);

    let blessed_vectors = reference["vectors"].as_object().expect("vectors");
    assert_eq!(selection.vectors.len(), blessed_vectors.len());
    for (month, vector) in &selection.vectors {
        let want = blessed_vectors
            .get(month)
            .unwrap_or_else(|| panic!("{month} is absent from the blessed vectors"))
            .as_array()
            .expect("vector");
        assert_eq!(vector.len(), want.len(), "{month}: vector length");
        for (index, (value, expected)) in vector.iter().zip(want.iter()).enumerate() {
            assert_bits(
                &format!("vector {month}[{index}] ({})", selection.keys[index]),
                *value,
                expected.as_f64().expect("finite"),
            );
        }
    }

    let blessed_seeds: Vec<&str> = reference["seeds"]
        .as_array()
        .expect("seeds")
        .iter()
        .map(|s| s.as_str().expect("seed"))
        .collect();
    assert_eq!(selection.seeds, blessed_seeds, "the seeds differ");

    // Pick ORDER, not just membership. Farthest-point is greedy, so two runs
    // agreeing on the set while disagreeing on the order means the distances
    // differ and the agreement is luck.
    let blessed_order: Vec<&str> = reference["selection"]["chosen_in_pick_order"]
        .as_array()
        .expect("pick order")
        .iter()
        .map(|s| s.as_str().expect("month"))
        .collect();
    assert_eq!(
        selection.chosen, blessed_order,
        "the selection differs from the blessed reference"
    );

    // `drift` deliberately uses a DIFFERENT median from `monthly` - the upper
    // middle on an even count - so it gets its own comparison rather than being
    // assumed to follow from the month table matching.
    let drift = sw::drift(&months).expect("drift");
    let blessed_drift = reference["drift"]["years"]
        .as_object()
        .expect("blessed drift");
    assert_eq!(drift.len(), blessed_drift.len(), "year count");
    let drift_columns: Vec<&str> = reference["drift"]["columns"]
        .as_array()
        .expect("columns")
        .iter()
        .map(|c| c.as_str().expect("column"))
        .collect();
    assert_eq!(drift_columns, sw::DRIFT_COLUMNS.to_vec());
    for (year, values) in &drift {
        let want = blessed_drift
            .get(year)
            .unwrap_or_else(|| panic!("{year} is absent from the blessed drift"))
            .as_array()
            .expect("values");
        assert_eq!(values.len(), want.len(), "{year}: column count");
        for (index, (value, expected)) in values.iter().zip(want.iter()).enumerate() {
            assert_bits(
                &format!("drift {year}.{}", sw::DRIFT_COLUMNS[index]),
                *value,
                expected.as_f64().expect("finite"),
            );
        }
    }

    let plan = sw::plan(&months).expect("plan");
    let blessed_plan = &reference["plan"];
    assert_eq!(
        plan.pool_first,
        blessed_plan["pool_first"].as_str().unwrap()
    );
    assert_eq!(plan.pool_last, blessed_plan["pool_last"].as_str().unwrap());
    assert_eq!(
        u64::try_from(plan.pool_len).unwrap(),
        blessed_plan["pool_len"].as_u64().unwrap()
    );
    let blessed_strata = blessed_plan["stratified"].as_array().expect("strata");
    assert_eq!(plan.stratified.len(), blessed_strata.len());
    for ((percentile, month), want) in plan.stratified.iter().zip(blessed_strata.iter()) {
        let want = want.as_array().expect("pair");
        assert_eq!(*percentile, want[0].as_i64().expect("percentile"));
        assert_eq!(
            month,
            want[1].as_str().expect("month"),
            "the p{percentile} rung differs; note CPython rounds half to EVEN when indexing"
        );
    }
    let blessed_eras = blessed_plan["eras"].as_array().expect("eras");
    assert_eq!(plan.eras.len(), blessed_eras.len());
    for (era, want) in plan.eras.iter().zip(blessed_eras.iter()) {
        assert_eq!(era.lo, want["lo"].as_str().unwrap());
        assert_eq!(era.hi, want["hi"].as_str().unwrap());
        assert_eq!(era.stress, want["stress"].as_str().unwrap());
        assert_eq!(era.calm, want["calm"].as_str().unwrap());
        assert_bits(
            &format!("{}..{} stress rv", era.lo, era.hi),
            era.stress_rv,
            want["stress_rv"].as_f64().expect("finite"),
        );
        assert_bits(
            &format!("{}..{} calm rv", era.lo, era.hi),
            era.calm_rv,
            want["calm_rv"].as_f64().expect("finite"),
        );
    }

    let percentiles = sw::nq_rv_percentiles(&months, &selection.chosen).expect("percentiles");
    let blessed_percentiles = reference["selection"]["nq_rv_percentile"]
        .as_object()
        .expect("percentiles");
    assert_eq!(percentiles.len(), blessed_percentiles.len());
    for (month, value) in &percentiles {
        assert_bits(
            &format!("percentile {month}"),
            *value,
            blessed_percentiles[month].as_f64().expect("finite"),
        );
    }
}

/// THE CACHE-LEVEL GATE, one layer below the blessed artifact.
///
/// `select-windows-blessed.json` is derived FROM the daily feature cache, so it
/// cannot see a difference in a session that no surviving monthly median
/// depends on - and that is exactly where the squaring deviation lives. This
/// re-derives the difference set against the Python's own cache and asserts it
/// is EXACTLY the recorded manifest: no fewer, so a correction cannot silently
/// disappear, and no more, so a new mismatch cannot hide among the approved
/// ones.
///
/// Needs the archives, so it keeps the local-data caveat of the gate above -
/// but NOT the Python, which is the point: its reference side is the committed
/// `select-windows-python-cache.json`, so this survives item 7 intact rather
/// than becoming unrunnable when the oracle moves.
#[test]
#[ignore = "re-reads the four CME archives and the Python's feature cache"]
fn parity_select_windows_cache_deviations_are_exactly_the_recorded_ones() {
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("analysis/select-windows-cache-deviations.json"))
            .expect("the deviation manifest"),
    )
    .expect("the manifest parses");

    // THE COMMITTED PYTHON CACHE, not the regenerable gitignored one.
    //
    // This gate has to outlive the oracle. `analysis/cme_daily_features.json`
    // is produced by `select_windows.py features`, and phase 4b item 7 retires
    // that script - so a gate reading it would become permanently unrunnable
    // the moment the retirement lands, which is how a gate dies quietly rather
    // than loudly.
    //
    // The obvious repair is to reduce this to a snapshot of the eleven recorded
    // deviations. That was REFUSED, and the reason is the property being
    // protected: an eleven-row snapshot can confirm the known corrections still
    // hold, but it cannot notice a TWELFTH deviation appearing, which is the
    // entire point of re-deriving the difference set. So the whole 3.7 MB
    // Python cache is committed instead, under a name that says it is an
    // immutable reference rather than a working file.
    let python_cache: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root().join("analysis/select-windows-python-cache.json"))
            .expect("the committed Python feature cache"),
    )
    .expect("the cache parses");

    let cache = build_cache();
    let mut found: Vec<(String, String, String, u64, u64)> = Vec::new();
    let mut compared = 0u64;
    for (symbol, sessions) in &cache {
        let days = python_cache[symbol]
            .as_object()
            .unwrap_or_else(|| panic!("{symbol} is absent from the Python cache"));
        for (session, features) in sessions {
            let Some(want) = days.get(session) else {
                continue;
            };
            for feature in sw::FEATURES {
                compared += 1;
                let ours = features.get(feature).expect("named feature");
                let theirs = want[feature].as_f64().expect("numeric feature");
                if ours.to_bits() != theirs.to_bits() {
                    found.push((
                        symbol.clone(),
                        session.clone(),
                        feature.to_string(),
                        theirs.to_bits(),
                        ours.to_bits(),
                    ));
                }
            }
        }
    }
    found.sort();

    let recorded = manifest["deviations"].as_array().expect("deviations");
    assert_eq!(
        compared,
        manifest["compared_values"].as_u64().expect("count"),
        "the number of compared values moved, so the corpus is not the recorded one"
    );
    assert_eq!(
        found.len(),
        recorded.len(),
        "expected exactly {} approved deviation(s), found {}: {found:#?}",
        recorded.len(),
        found.len()
    );
    for ((symbol, session, feature, python_bits, rust_bits), want) in
        found.iter().zip(recorded.iter())
    {
        assert_eq!(symbol, want["symbol"].as_str().expect("symbol"));
        assert_eq!(session, want["session"].as_str().expect("session"));
        assert_eq!(feature, want["feature"].as_str().expect("feature"));
        assert_eq!(
            format!("{python_bits:016x}"),
            want["python_bits"].as_str().expect("python bits"),
            "{symbol} {session} {feature}: the Python side moved"
        );
        assert_eq!(
            format!("{rust_bits:016x}"),
            want["rust_bits"].as_str().expect("rust bits"),
            "{symbol} {session} {feature}: the corrected value moved"
        );
    }
}

/// The constants the reference was blessed under must still be the ones the
/// port uses. Cheap enough to run without the archives, and it catches the
/// case where someone moves `DATABENTO_START` and the ignored gate above does
/// not run for months - which matters more here than usual, because that
/// constant re-centres every z-score rather than merely filtering candidates.
#[test]
fn the_blessed_constants_match_the_port() {
    let reference = blessed();
    let constants = &reference["constants"];
    assert_eq!(
        constants["DATABENTO_START"].as_str().unwrap(),
        sw::DATABENTO_START
    );
    assert_eq!(
        constants["BUDGET_MONTHS"].as_u64().unwrap(),
        u64::try_from(sw::BUDGET_MONTHS).unwrap()
    );

    let features: Vec<&str> = constants["FEATURES"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert_eq!(features, sw::FEATURES.to_vec(), "feature order is layout");

    let archives: BTreeMap<String, String> = constants["ARCHIVES"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let ours: BTreeMap<String, String> = sw::ARCHIVES
        .iter()
        .map(|(s, f)| ((*s).to_string(), (*f).to_string()))
        .collect();
    assert_eq!(archives, ours);
}
