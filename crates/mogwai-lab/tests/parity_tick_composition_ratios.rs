// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Phase 4b item 3: the `tick_composition_ratios.py` port against its blessed
//! reference.
//!
//! Not `#[ignore]`d, and that is the point of this one. Every input is
//! committed - the six `analysis/tick-composition-protocol-N.json` fixtures and
//! `analysis/tick-composition-ratios-blessed.json` - so unlike the corpus gates
//! this runs on any clone, in the ordinary gate, every time. A sizing policy
//! that decides four shipped constants should not be checked only on the one
//! machine holding a data delivery.
//!
//! Floats are compared bit-exactly. The policy is `max` over ratios of
//! committed p999 values, then a power-of-two or next-million rounding, so
//! every step is exactly reproducible and a tolerance would only hide a defect.

use std::path::PathBuf;

use mogwai_lab::tick_composition_ratios as tcr;
use serde_json::Value;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &str) -> Value {
    let full = root().join(path);
    serde_json::from_str(
        &std::fs::read_to_string(&full)
            .unwrap_or_else(|e| panic!("reading {}: {e}", full.display())),
    )
    .unwrap_or_else(|e| panic!("parsing {}: {e}", full.display()))
}

fn blessed() -> Value {
    read("analysis/tick-composition-ratios-blessed.json")
}

/// The calendar classification the blessed run used.
///
/// The Python hardcoded these as literal preset tuples. The port derives them
/// from whether a preset HAS a calendar, which is the property that actually
/// makes the tape byte-identical - but this gate must reproduce the blessed
/// numbers, so it feeds the same classification the blessing ran under and the
/// derivation is checked separately by `the_calendar_split_is_derivable`.
fn preset_calendars(reference: &Value) -> tcr::PresetCalendars {
    let names = |key: &str| -> Vec<String> {
        reference["constants"][key]
            .as_array()
            .expect("preset list")
            .iter()
            .map(|v| v.as_str().expect("preset name").to_string())
            .collect()
    };
    tcr::PresetCalendars::new(names("CALENDAR_FREE"), names("CALENDAR_BEARING"))
}

fn assert_bits(label: &str, got: f64, want: f64) {
    assert_eq!(
        got.to_bits(),
        want.to_bits(),
        "{label}: {got:?} ({:016x}) against the blessed {want:?} ({:016x})",
        got.to_bits(),
        want.to_bits()
    );
}

fn assert_group(
    mode: &str,
    group: &str,
    got: &std::collections::BTreeMap<String, f64>,
    want: &Value,
) {
    let want = want
        .as_object()
        .unwrap_or_else(|| panic!("{mode}.{group} is not an object"));
    assert_eq!(
        got.len(),
        want.len(),
        "{mode}.{group}: key count differs, got {:?} against {:?}",
        got.keys().collect::<Vec<_>>(),
        want.keys().collect::<Vec<_>>()
    );
    for (key, value) in got {
        let expected = want
            .get(key)
            .unwrap_or_else(|| panic!("{mode}.{group}.{key} is absent from the blessed result"))
            .as_f64()
            .expect("numeric");
        assert_bits(&format!("{mode}.{group}.{key}"), *value, expected);
    }
}

#[test]
fn every_mode_reproduces_the_blessed_result() {
    let reference = blessed();
    let presets = preset_calendars(&reference);
    let modes = reference["modes"].as_object().expect("blessed modes");
    assert_eq!(
        modes.len(),
        tcr::MODES.len(),
        "the port and the blessing disagree about how many modes exist"
    );

    for mode in &tcr::MODES {
        let want = modes
            .get(mode.name)
            .unwrap_or_else(|| panic!("{} is absent from the blessed result", mode.name));
        let before = read(mode.before);
        let after = read(mode.after);
        let got = tcr::compare(mode, &before, &after, &presets)
            .unwrap_or_else(|e| panic!("{}: {e}", mode.name));

        assert_group(mode.name, "ratios", &got.ratios, &want["ratios"]);
        assert_group(mode.name, "observed", &got.observed, &want["observed"]);
        assert_group(
            mode.name,
            "required_reach",
            &got.required_reach,
            &want["required_reach"],
        );
        assert_group(mode.name, "proposed", &got.proposed, &want["proposed"]);
        assert_group(mode.name, "horizons", &got.horizons, &want["horizons"]);
    }
}

/// The mode table is committed data and must match what the blessing ran under,
/// baselines included. Sharing or re-deriving a baseline is the defect this
/// whole per-mode structure exists to prevent: it once under-proposed two
/// constants while every acceptance assertion still passed.
#[test]
fn the_mode_table_matches_the_blessed_constants() {
    let reference = blessed();
    let want = reference["constants"]["MODES"]
        .as_object()
        .expect("blessed mode table");
    assert_eq!(want.len(), tcr::MODES.len());

    for mode in &tcr::MODES {
        let recorded = &want[mode.name];
        assert_eq!(
            recorded["versions"][0].as_i64().unwrap(),
            mode.versions.0,
            "{}: before version",
            mode.name
        );
        assert_eq!(
            recorded["versions"][1].as_i64().unwrap(),
            mode.versions.1,
            "{}: after version",
            mode.name
        );
        assert_eq!(recorded["before"].as_str().unwrap(), mode.before);
        assert_eq!(recorded["after"].as_str().unwrap(), mode.after);
        assert_eq!(
            recorded["same_pairing"].as_bool().unwrap(),
            mode.same_pairing
        );

        let baseline = &recorded["baseline"];
        assert_bits(
            &format!("{}.baseline.checkpoint_k", mode.name),
            mode.baseline.checkpoint_k,
            baseline["checkpoint_k"].as_f64().unwrap(),
        );
        assert_bits(
            &format!("{}.baseline.sweep_drain_budget", mode.name),
            mode.baseline.sweep_drain_budget,
            baseline["sweep_drain_budget"].as_f64().unwrap(),
        );
        assert_bits(
            &format!("{}.baseline.max_extend_ticks", mode.name),
            mode.baseline.max_extend_ticks,
            baseline["max_extend_ticks"].as_f64().unwrap(),
        );
        assert_bits(
            &format!("{}.baseline.warmup_baseline", mode.name),
            mode.baseline.warmup_baseline,
            baseline["warmup_baseline"].as_f64().unwrap(),
        );
        assert_bits(
            &format!("{}.baseline.fanout_depth", mode.name),
            mode.baseline.fanout_depth,
            baseline["fanout_depth"].as_f64().unwrap(),
        );
    }
}

/// The 8/9 identity gate, which produces no ratios: its verdict is the claim.
/// The blessing recorded that it passed, so a port that cannot pass it has not
/// reproduced the Python.
#[test]
fn the_eight_nine_identity_gate_passes_as_it_did_for_the_python() {
    let reference = blessed();
    assert!(
        reference["gates"]["verify_8_9_identity"]["passed"]
            .as_bool()
            .expect("recorded verdict"),
        "the blessing recorded a FAILING identity gate; the port cannot be matched to it"
    );
    tcr::verify_8_9_identity(
        &root().join("analysis/tick-composition-protocol-8.json"),
        &root().join("analysis/tick-composition-protocol-9.json"),
    )
    .expect("the identity gate must pass on the committed fixtures");
}

/// A gate that only ever passes proves nothing about what it refuses, so the
/// identity check is shown to refuse on a fixture pair it should reject: the
/// same file twice carries one pairing id, which is the "compared with itself"
/// case the gate exists to catch.
#[test]
fn the_eight_nine_identity_gate_refuses_a_self_comparison() {
    let eight = root().join("analysis/tick-composition-protocol-8.json");
    let error = tcr::verify_8_9_identity(&eight, &eight)
        .expect_err("comparing a fixture with itself must refuse");
    let text = error.to_string();
    assert!(
        text.contains("protocol 8, not 9") || text.contains("same pairing id"),
        "the refusal must name the reason, got: {text}"
    );
}

/// The derivation reproduces the hardcoded split. Without this the claim that
/// the classification is now preset-derived would be untested prose: the port
/// could derive something different and the parity gate above would not notice,
/// because it feeds the blessed lists in deliberately.
///
/// Presets come from the fixture rather than a constant, which is the whole
/// point - a sixth instrument appears in the fixture and gets classified,
/// where a hardcoded tuple would have left it in neither class and silently
/// unchecked.
#[test]
fn deriving_the_calendar_split_from_presets_matches_the_python_tuples() {
    let reference = blessed();
    let fixture = read("analysis/tick-composition-protocol-11.json");
    let names = tcr::PresetCalendars::presets_in(&fixture).expect("preset names");
    assert_eq!(
        names.len(),
        5,
        "the protocol-11 fixture names five presets, got {names:?}"
    );

    // Two of the five (ETHUSDT, SOLUSDT) retired with the 2026-08-09 owner
    // ruling and classify through the retired-preset table; the derivation
    // over a historical fixture must still work, which is what this exercises.
    let derived =
        tcr::PresetCalendars::derive(&names).expect("every preset resolves or is retired");

    let expect = |key: &str| -> Vec<String> {
        let mut out: Vec<String> = reference["constants"][key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        out.sort();
        out
    };
    let mut free = derived.calendar_free.clone();
    free.sort();
    let mut bearing = derived.calendar_bearing.clone();
    bearing.sort();

    assert_eq!(
        free,
        expect("CALENDAR_FREE"),
        "the derived calendar-free set differs from the Python's hardcoded tuple"
    );
    assert_eq!(
        bearing,
        expect("CALENDAR_BEARING"),
        "the derived calendar-bearing set differs from the Python's hardcoded tuple"
    );
}

/// The calendar split covers every preset in the fixtures. The Python carried
/// literal tuples; a sixth preset would have fallen into neither and been
/// checked by nothing.
#[test]
fn the_calendar_split_is_derivable() {
    let reference = blessed();
    let free: Vec<String> = reference["constants"]["CALENDAR_FREE"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    let bearing: Vec<String> = reference["constants"]["CALENDAR_BEARING"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();

    // Every preset named in either list appears in the fixtures, and no preset
    // in the fixtures is missing from both - the gap a hardcoded tuple leaves.
    let fixture = read("analysis/tick-composition-protocol-11.json");
    let mut seen: Vec<String> = fixture["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|row| row["preset"].as_str().expect("preset").to_string())
        .collect();
    seen.sort();
    seen.dedup();
    for preset in &seen {
        assert!(
            free.contains(preset) || bearing.contains(preset),
            "{preset} appears in the fixtures but in neither calendar class, so the acceptance \
             gate would check nothing for it"
        );
    }
}
