// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `analysis/tick_composition_ratios.py`: the BBO budget sizing policy.
//!
//! Pairs protocol composition fixtures and applies the budget policy that
//! decides FOUR SHIPPED CONSTANTS - `CHECKPOINT_K`, the sweep drain budget, the
//! warmup materialization ceiling and `fanout_depth`. It is an independent
//! estimator, not a report generator: the resize rule in [`compare`] (worst
//! p999 ratio, two-times headroom, power-of-two or next-million rounding, then
//! the larger of that and the required reach) is the decision procedure those
//! constants come from.
//!
//! ITS OWN SUBCOMMAND, deliberately, rather than a `--report` mode on
//! `tick-composition`. That command MEASURES a tape and this one BLESSES the
//! measurement into constants; fusing them would let one invocation measure a
//! fixture and accept it in the same breath.
//!
//! # Two comparison contracts over one arithmetic
//!
//! The budget policy and the required-reach rule are identical either way,
//! which is exactly why they are not duplicated: two copies of a sizing rule
//! drift, and the drift arrives as a constant nobody can re-derive.
//!
//! - `projection` compares versions 6 and 7 from ONE traversal. Protocol 6 is a
//!   count projection of the protocol-7 stream, because quote placement draws no
//!   randomness, so the fixtures SHARE a `pairing_id` and a mismatch means one
//!   is stale.
//! - the `independent` modes compare TWO traversals. The pairings MUST differ;
//!   equal pairings would mean one run was compared with itself.
//!
//! # Why each mode carries its own baseline
//!
//! A ratio is meaningless without the number it resizes, and that number is
//! whatever shipped at the mode's BEFORE version - not whatever ships today.
//! Sharing one table resized the protocol-7 constants by the pre-protocol-7
//! baseline and under-proposed checkpoint and fanout by the factor protocol 7
//! had already absorbed, WHILE EVERY ACCEPTANCE ASSERTION STILL PASSED. So a
//! baseline is a historical record, frozen once its mode's resize has landed,
//! and it lives in [`MODES`] as committed DATA that is never re-derived.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::error::{LabError, LabResult};

/// Which acceptance gate a mode runs before any ratio is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Acceptance {
    /// Only the session shape moved: `parents` AND `ticks_per_parent` are
    /// frozen for every pairing, and the calendar-free presets byte-identical.
    /// A fanout change proves the landing touched something outside its scope.
    SessionReshape,
    /// A preset fit: the futures' cadence and fanout MAY move, since fitted
    /// scalars change what a parent looks like. The calendar-free presets are
    /// still frozen.
    PresetFit,
    /// `projection` mode runs no acceptance gate: one traversal, so there is no
    /// second tape to hold still.
    None,
}

/// The historical baseline a mode resizes from. Committed data, frozen once the
/// mode's resize has landed.
#[derive(Clone, Copy, Debug)]
pub struct Baseline {
    pub checkpoint_k: f64,
    pub sweep_drain_budget: f64,
    pub max_extend_ticks: f64,
    /// Separate from `max_extend_ticks` on purpose: before protocol 7 the
    /// warmup reach had no constant and borrowed the extend ceiling, so
    /// protocol 7's `MAX_WARMUP_MATERIALIZATION_TICKS` is what protocol 8
    /// resizes.
    pub warmup_baseline: f64,
    pub fanout_depth: f64,
}

/// One comparison contract.
pub struct Mode {
    pub name: &'static str,
    pub versions: (i64, i64),
    pub before: &'static str,
    pub after: &'static str,
    pub same_pairing: bool,
    pub acceptance: Acceptance,
    pub baseline: Baseline,
}

/// The mode table, verbatim from the Python's `MODES`.
///
/// EVERY BASELINE HERE IS HISTORY. A protocol-12 comparison adds a mode whose
/// baseline is the protocol-11 column; it does not edit the ones below.
pub const MODES: [Mode; 4] = [
    Mode {
        name: "projection",
        versions: (6, 7),
        before: "analysis/tick-composition-protocol-6.json",
        after: "analysis/tick-composition-protocol-7.json",
        same_pairing: true,
        acceptance: Acceptance::None,
        // Pre-protocol-7, as shipped when the 6-to-7 comparison was made.
        baseline: Baseline {
            checkpoint_k: 262_144.0,
            sweep_drain_budget: 5_000_000.0,
            max_extend_ticks: 1_073_741_824.0,
            warmup_baseline: 1_073_741_824.0,
            fanout_depth: 65_536.0,
        },
    },
    Mode {
        name: "independent",
        versions: (7, 8),
        before: "analysis/tick-composition-protocol-7.json",
        after: "analysis/tick-composition-protocol-8.json",
        same_pairing: false,
        acceptance: Acceptance::SessionReshape,
        // The protocol-7 constants as shipped when this comparison was made.
        // Protocol 8 has since replaced four of the five with the values this
        // mode proposed, so these are NOT current. `max_extend_ticks` is the
        // exception and still ships at 1 << 30: it is a per-lock runaway
        // backstop rather than a reach ceiling, and is deliberately not scaled.
        baseline: Baseline {
            checkpoint_k: 1_048_576.0,
            sweep_drain_budget: 282_000_000.0,
            max_extend_ticks: 1_073_741_824.0,
            warmup_baseline: 81_124_000_000.0,
            fanout_depth: 262_144.0,
        },
    },
    Mode {
        name: "independent_9_10",
        versions: (9, 10),
        before: "analysis/tick-composition-protocol-9.json",
        after: "analysis/tick-composition-protocol-10.json",
        same_pairing: false,
        acceptance: Acceptance::PresetFit,
        baseline: Baseline {
            checkpoint_k: 4_194_304.0,
            sweep_drain_budget: 1_434_000_000.0,
            max_extend_ticks: 1_073_741_824.0,
            warmup_baseline: 162_349_000_000.0,
            fanout_depth: 1_048_576.0,
        },
    },
    Mode {
        name: "independent_10_11",
        versions: (10, 11),
        before: "analysis/tick-composition-protocol-10.json",
        after: "analysis/tick-composition-protocol-11.json",
        same_pairing: false,
        acceptance: Acceptance::SessionReshape,
        baseline: Baseline {
            checkpoint_k: 16_777_216.0,
            sweep_drain_budget: 5_799_000_000.0,
            max_extend_ticks: 1_073_741_824.0,
            warmup_baseline: 667_299_000_000.0,
            fanout_depth: 4_194_304.0,
        },
    },
];

/// The three fields the 8/9 identity gate validates SEPARATELY before the
/// generic equality comparison omits them. They are not ignored: the version
/// labels are asserted directly, the pairing ids must both exist and differ,
/// and `projection` must equal the producer's canonical label for its own
/// version.
pub const IDENTITY_SEPARATELY_VALIDATED: [&str; 3] =
    ["tape_protocol_version", "pairing_id", "projection"];

/// THE REJECTED PROTOCOL-11 FANOUT PROPOSAL, carried forward as DATA.
///
/// `independent_10_11` mechanically proposes a `fanout_depth` of 16,777,216.
/// That proposal was REJECTED - `mogwai-server`'s
/// `the_fanout_default_carries_the_protocol_11_exception` pins the exception -
/// and without this record the next comparison re-proposes it and re-litigates
/// a settled ruling. A sizing tool that cannot remember a refusal will keep
/// making it.
pub const REJECTED_PROPOSALS: [(&str, &str, f64); 1] =
    [("independent_10_11", "fanout_depth", 16_777_216.0)];

/// Looks up a mode by name.
///
/// # Errors
/// [`LabError::Refusal`] if no mode carries that name.
pub fn mode(name: &str) -> LabResult<&'static Mode> {
    MODES
        .iter()
        .find(|m| m.name == name)
        .ok_or_else(|| LabError::refusal(format!("unknown mode `{name}`")))
}

/// `power_of_two`: the next power of two at or above `value`.
fn power_of_two(value: f64) -> f64 {
    // `1 << math.ceil(math.log2(value))`. Kept in f64 because the proposals
    // reach past 2^40 and the Python's result is a Python int of arbitrary
    // width; every value the policy produces is exactly representable.
    let exponent = value.log2().ceil();
    exponent.exp2()
}

/// `million`: round up to the next whole million.
fn million(value: f64) -> f64 {
    (value / 1_000_000.0).ceil() * 1_000_000.0
}

/// The presets a comparison holds byte-identical, derived from the CALENDAR
/// rather than hardcoded.
///
/// The Python carried `CALENDAR_FREE` and `CALENDAR_BEARING` as literal tuples
/// of preset names. That does not survive an open instrument set: the next
/// instrument would silently fall into neither list and its rows would be
/// checked by nothing. A preset is calendar-free exactly when it has no
/// calendar - its normalizer is then the literal 1.0, which is WHY its tape is
/// byte-identical across a session reshape - so the classification is a
/// property of the preset and is read from it.
pub struct PresetCalendars {
    pub calendar_free: Vec<String>,
    pub calendar_bearing: Vec<String>,
}

impl PresetCalendars {
    /// Classifies by presence of a calendar.
    #[must_use]
    pub fn new(calendar_free: Vec<String>, calendar_bearing: Vec<String>) -> Self {
        Self {
            calendar_free,
            calendar_bearing,
        }
    }

    /// DERIVES the split by asking each preset whether it has a calendar,
    /// through the server's own loader.
    ///
    /// This is the replacement for the Python's hardcoded tuples, and the
    /// reason is the open instrument set: a sixth preset added to a hardcoded
    /// list would fall into neither class, and the acceptance gate would then
    /// check NOTHING for it while still passing. Derivation cannot have that
    /// gap - a preset either carries a calendar or it does not.
    ///
    /// # Errors
    /// [`LabError::Refusal`] if a named preset cannot be loaded. A preset that
    /// does not resolve is not silently dropped: dropping it is exactly the
    /// failure mode this replaces.
    pub fn derive(presets: &[String]) -> LabResult<Self> {
        let mut calendar_free = Vec::new();
        let mut calendar_bearing = Vec::new();
        for name in presets {
            let profile = mogwai_server::config::profile_from_preset(name)
                .map_err(|e| LabError::refusal(format!("loading preset {name}: {e}")))?;
            if profile.calendar.is_some() {
                calendar_bearing.push(name.clone());
            } else {
                calendar_free.push(name.clone());
            }
        }
        Ok(Self {
            calendar_free,
            calendar_bearing,
        })
    }

    /// The preset names appearing in a composition fixture, which is where the
    /// list to classify comes from - the fixture, not a constant.
    ///
    /// # Errors
    /// [`LabError::Refusal`] if the fixture carries no `entries` array.
    pub fn presets_in(fixture: &Value) -> LabResult<Vec<String>> {
        let entries = fixture
            .get("entries")
            .and_then(Value::as_array)
            .ok_or_else(|| LabError::refusal("fixture carries no `entries` array"))?;
        let mut names: Vec<String> = entries
            .iter()
            .filter_map(|row| row.get("preset").and_then(Value::as_str))
            .map(str::to_owned)
            .collect();
        names.sort();
        names.dedup();
        Ok(names)
    }

    fn is_free(&self, preset: &str) -> bool {
        self.calendar_free.iter().any(|p| p == preset)
    }

    fn is_bearing(&self, preset: &str) -> bool {
        self.calendar_bearing.iter().any(|p| p == preset)
    }
}

/// A row key: preset, seed, configuration.
type RowKey = (String, i64, String);

fn row_key(row: &Value) -> LabResult<RowKey> {
    let preset = row
        .get("preset")
        .and_then(Value::as_str)
        .ok_or_else(|| LabError::refusal("entry carries no string `preset`"))?;
    let seed = row
        .get("seed")
        .and_then(Value::as_i64)
        .ok_or_else(|| LabError::refusal("entry carries no integer `seed`"))?;
    let configuration = row
        .get("configuration")
        .and_then(Value::as_str)
        .ok_or_else(|| LabError::refusal("entry carries no string `configuration`"))?;
    Ok((preset.to_string(), seed, configuration.to_string()))
}

fn index(fixture: &Value) -> LabResult<BTreeMap<RowKey, &Value>> {
    let entries = fixture
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| LabError::refusal("fixture carries no `entries` array"))?;
    let mut out = BTreeMap::new();
    for row in entries {
        out.insert(row_key(row)?, row);
    }
    Ok(out)
}

/// EVERY numeric measurement leaf, not merely the ratio inputs: a NaN in a
/// field today's ratios skip would survive into a fixture the next comparison
/// reads. Shared by BOTH independent acceptance gates - a validator present in
/// only one path is a validator the other path silently lacks.
fn assert_leaves_finite_positive(key: &RowKey, path: &str, node: &Value) -> LabResult<()> {
    match node {
        // Booleans are skipped rather than treated as 1/0, matching the
        // Python's `isinstance(node, bool)` guard ahead of its numeric check -
        // in Python a bool IS an int, so without that guard `False` would fail
        // the positivity assertion.
        Value::Number(number) => {
            let value = number
                .as_f64()
                .ok_or_else(|| LabError::refusal(format!("{key:?}: {path} is not numeric")))?;
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                Err(LabError::refusal(format!(
                    "{key:?}: {path} is {value}, not finite and positive"
                )))
            }
        }
        Value::Object(map) => {
            for (name, child) in map {
                assert_leaves_finite_positive(key, &format!("{path}.{name}"), child)?;
            }
            Ok(())
        }
        // Bools, strings, arrays and nulls carry no numeric leaf to validate.
        _ => Ok(()),
    }
}

fn assert_row_leaves_finite_positive(key: &RowKey, row: &Value) -> LabResult<()> {
    let Some(map) = row.as_object() else {
        return Err(LabError::refusal(format!(
            "{key:?}: entry is not an object"
        )));
    };
    for (name, node) in map {
        if matches!(name.as_str(), "preset" | "configuration" | "seed") {
            continue;
        }
        assert_leaves_finite_positive(key, name, node)?;
    }
    Ok(())
}

fn field<'a>(row: &'a Value, key: &RowKey, name: &str) -> LabResult<&'a Value> {
    row.get(name)
        .ok_or_else(|| LabError::refusal(format!("{key:?}: entry carries no `{name}`")))
}

/// The `session_reshape` acceptance gate, run BEFORE any ratio.
///
/// The session profile changes WHEN events happen, never how many: child count
/// comes from the arrival-state chain and the surge window, neither of which
/// reads the profile, so the draw sequence is identical across the change. That
/// is why `ticks_per_parent` is frozen for all five presets, and why a fanout
/// move proves the landing did something outside its authorized scope.
fn assert_unchanged_where_the_tape_did_not_move(
    old: &BTreeMap<RowKey, &Value>,
    new: &BTreeMap<RowKey, &Value>,
    presets: &PresetCalendars,
) -> LabResult<()> {
    for (key, old_row) in old {
        let new_row = new[key];
        if field(old_row, key, "parents")? != field(new_row, key, "parents")? {
            return Err(LabError::refusal(format!("{key:?}: parents moved")));
        }
        if field(old_row, key, "ticks_per_parent")? != field(new_row, key, "ticks_per_parent")? {
            return Err(LabError::refusal(format!(
                "{key:?}: parent fanout moved, so the change did something beyond reshaping \
                 the session and no budget ratio from this run is valid"
            )));
        }
        if presets.is_free(&key.0) {
            if old_row != &new_row {
                return Err(LabError::refusal(format!(
                    "{key:?}: a calendar-free preset moved. Its normalizer is the literal 1.0 \
                     and its tape must be byte-identical, so this is an unintended tape change \
                     rather than a session reshape"
                )));
            }
        } else if !presets.is_bearing(&key.0) {
            return Err(LabError::refusal(format!(
                "{key:?}: unknown preset {}",
                key.0
            )));
        }
        assert_row_leaves_finite_positive(key, old_row)?;
        assert_row_leaves_finite_positive(key, new_row)?;
    }
    Ok(())
}

/// The `preset_fit` acceptance gate, run BEFORE any ratio.
///
/// The fit touches only the calendar-bearing presets, so the calendar-free
/// tapes must be byte-identical. The futures MAY move - cadence and fanout
/// both, since the fitted scalars change what a parent looks like - but every
/// measurement entering a ratio must be finite and positive on BOTH sides: a
/// NaN would poison the maximum silently and a zero would divide loudly, and
/// neither is a verdict.
fn assert_crypto_frozen_and_futures_finite(
    old: &BTreeMap<RowKey, &Value>,
    new: &BTreeMap<RowKey, &Value>,
    presets: &PresetCalendars,
) -> LabResult<()> {
    for (key, old_row) in old {
        let new_row = new[key];
        if field(old_row, key, "parents")? != field(new_row, key, "parents")? {
            return Err(LabError::refusal(format!("{key:?}: parents moved")));
        }
        if presets.is_free(&key.0) {
            if old_row != &new_row {
                return Err(LabError::refusal(format!(
                    "{key:?}: a calendar-free preset moved. The fit changes only the futures \
                     presets, so this is an unintended tape change and no budget ratio from \
                     this run is valid"
                )));
            }
            continue;
        }
        if !presets.is_bearing(&key.0) {
            return Err(LabError::refusal(format!(
                "{key:?}: unknown preset {}",
                key.0
            )));
        }
        assert_row_leaves_finite_positive(key, old_row)?;
        assert_row_leaves_finite_positive(key, new_row)?;
    }
    Ok(())
}

/// The proposals and the evidence behind them.
#[derive(Debug)]
pub struct Comparison {
    pub ratios: BTreeMap<String, f64>,
    pub observed: BTreeMap<String, f64>,
    pub required_reach: BTreeMap<String, f64>,
    pub proposed: BTreeMap<String, f64>,
    pub horizons: BTreeMap<String, f64>,
}

fn p999(row: &Value, key: &RowKey, field_name: &str) -> LabResult<f64> {
    row.get(field_name)
        .and_then(|node| node.get("p999"))
        .and_then(Value::as_f64)
        .ok_or_else(|| LabError::refusal(format!("{key:?}: {field_name}.p999 is missing")))
}

fn frames_p999(row: &Value, key: &RowKey, speed: &str) -> LabResult<f64> {
    row.get("frames_per_wall_second")
        .and_then(|node| node.get(speed))
        .and_then(|node| node.get("p999"))
        .ok_or_else(|| {
            LabError::refusal(format!(
                "{key:?}: frames_per_wall_second.{speed}.p999 is missing"
            ))
        })?
        .as_f64()
        .ok_or_else(|| {
            LabError::refusal(format!(
                "{key:?}: frames_per_wall_second.{speed}.p999 is not numeric"
            ))
        })
}

/// The speeds the fanout ratio walks, in the Python's tuple order.
const SPEEDS: [&str; 2] = ["1.0", "10.0"];

/// `compare`: the sizing policy.
///
/// # Errors
/// [`LabError::Refusal`] on a version mismatch, a pairing relation the mode
/// forbids, a row-key disagreement, or any acceptance-gate failure. Every one
/// of these refuses BEFORE a ratio is computed, because a ratio over a tape
/// that moved for the wrong reason is a number with no meaning rather than a
/// slightly wrong one.
pub fn compare(
    mode: &Mode,
    before: &Value,
    after: &Value,
    presets: &PresetCalendars,
) -> LabResult<Comparison> {
    let (lo, hi) = mode.versions;
    let before_version = before
        .get("tape_protocol_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| LabError::refusal("before fixture carries no tape_protocol_version"))?;
    let after_version = after
        .get("tape_protocol_version")
        .and_then(Value::as_i64)
        .ok_or_else(|| LabError::refusal("after fixture carries no tape_protocol_version"))?;
    if before_version != lo {
        return Err(LabError::refusal(format!(
            "before fixture is protocol {before_version}, not {lo} as mode {} requires",
            mode.name
        )));
    }
    if after_version != hi {
        return Err(LabError::refusal(format!(
            "after fixture is protocol {after_version}, not {hi} as mode {} requires",
            mode.name
        )));
    }
    for fixture in [before, after] {
        let parents = fixture
            .get("parent_events_per_combination")
            .and_then(Value::as_i64)
            .ok_or_else(|| LabError::refusal("fixture carries no parent_events_per_combination"))?;
        if parents != 2_000_000 {
            return Err(LabError::refusal(format!(
                "parent_events_per_combination is {parents}, not 2000000"
            )));
        }
    }

    // Matching row keys prove only that the two files describe the same
    // combinations, never the same TAPE. The pairing identifier carries that,
    // and which relation it must satisfy is the whole difference between the
    // modes. Fixtures predating the identifier carry no such evidence at all,
    // which is a refusal with a reason rather than a missing key.
    let pairing_before = before.get("pairing_id");
    let pairing_after = after.get("pairing_id");
    let (Some(pairing_before), Some(pairing_after)) = (pairing_before, pairing_after) else {
        return Err(LabError::refusal(
            "these fixtures predate pairing identifiers, so nothing shows which traversals \
             produced them; regenerate with `mogwai tick-composition`",
        ));
    };
    if mode.same_pairing {
        if pairing_before != pairing_after {
            return Err(LabError::refusal(
                "projection mode compares two counter sets from ONE traversal, and these carry \
                 different pairings; regenerate with a single `mogwai tick-composition` run",
            ));
        }
    } else if pairing_before == pairing_after {
        return Err(LabError::refusal(
            "independent mode compares two SEPARATELY measured tapes, and these carry the same \
             pairing - the same run is being compared with itself",
        ));
    }

    let old = index(before)?;
    let new = index(after)?;
    if old.keys().collect::<Vec<_>>() != new.keys().collect::<Vec<_>>() {
        return Err(LabError::refusal(
            "the two fixtures describe different combinations",
        ));
    }

    match mode.acceptance {
        Acceptance::SessionReshape => {
            assert_unchanged_where_the_tape_did_not_move(&old, &new, presets)?;
        }
        Acceptance::PresetFit => assert_crypto_frozen_and_futures_finite(&old, &new, presets)?,
        Acceptance::None => {}
    }

    // The worst p999 ratio over every combination, skipping rows whose OLD
    // value is zero - the Python's `if old[k][field]["p999"] > 0`.
    let ratio = |field_name: &str| -> LabResult<f64> {
        let mut worst = f64::NEG_INFINITY;
        for (key, old_row) in &old {
            let old_value = p999(old_row, key, field_name)?;
            if old_value > 0.0 {
                let new_value = p999(new[key], key, field_name)?;
                worst = worst.max(new_value / old_value);
            }
        }
        if worst.is_finite() {
            Ok(worst)
        } else {
            Err(LabError::refusal(format!(
                "{field_name}: no combination carries a positive baseline p999"
            )))
        }
    };

    let mut fanout_ratio = f64::NEG_INFINITY;
    for (key, old_row) in &old {
        for speed in SPEEDS {
            let old_value = frames_p999(old_row, key, speed)?;
            if old_value > 0.0 {
                fanout_ratio = fanout_ratio.max(frames_p999(new[key], key, speed)? / old_value);
            }
        }
    }
    if !fanout_ratio.is_finite() {
        return Err(LabError::refusal(
            "no combination carries a positive baseline frames_per_wall_second",
        ));
    }

    let mut ratios = BTreeMap::new();
    ratios.insert("checkpoint_k".to_string(), ratio("ticks_per_sim_second")?);
    ratios.insert(
        "sweep_drain_budget".to_string(),
        ratio("ticks_per_vol_window")?,
    );
    ratios.insert(
        "warmup_materialization_ticks".to_string(),
        ratio("ticks_per_warmup")?,
    );
    ratios.insert("fanout_depth".to_string(), fanout_ratio);

    // The required reach is the larger of two candidates, and which one wins is
    // the whole argument: a rate projected across the window, or the window
    // count actually observed. `reference/performance.md` quotes both and says
    // the observed ones lose, so both are emitted - a figure a document cites
    // and its source does not produce is a figure that drifts.
    let mut rate = f64::NEG_INFINITY;
    let mut vol_window_count = f64::NEG_INFINITY;
    let mut warmup_window_count = f64::NEG_INFINITY;
    for (key, row) in &new {
        rate = rate.max(p999(row, key, "ticks_per_sim_second")?);
        vol_window_count = vol_window_count.max(p999(row, key, "ticks_per_vol_window")?);
        warmup_window_count = warmup_window_count.max(p999(row, key, "ticks_per_warmup")?);
    }

    let mut observed = BTreeMap::new();
    observed.insert("frames_per_sim_second".to_string(), rate);
    observed.insert("vol_window_count".to_string(), vol_window_count);
    observed.insert("warmup_window_count".to_string(), warmup_window_count);

    let mut required_reach = BTreeMap::new();
    required_reach.insert(
        "sweep_drain_budget".to_string(),
        vol_window_count.max(rate * 300.0),
    );
    required_reach.insert(
        "warmup_materialization_ticks".to_string(),
        warmup_window_count.max(rate * 86_400.0),
    );

    let baseline = &mode.baseline;
    let mut proposed = BTreeMap::new();
    proposed.insert(
        "checkpoint_k".to_string(),
        power_of_two(baseline.checkpoint_k * ratios["checkpoint_k"] * 2.0),
    );
    proposed.insert(
        "sweep_drain_budget".to_string(),
        million(
            (baseline.sweep_drain_budget * ratios["sweep_drain_budget"] * 2.0)
                .max(required_reach["sweep_drain_budget"]),
        ),
    );
    proposed.insert("max_extend_ticks".to_string(), baseline.max_extend_ticks);
    proposed.insert(
        "warmup_materialization_ticks".to_string(),
        million(
            (baseline.warmup_baseline * ratios["warmup_materialization_ticks"] * 2.0)
                .max(required_reach["warmup_materialization_ticks"]),
        ),
    );
    proposed.insert(
        "fanout_depth".to_string(),
        power_of_two(baseline.fanout_depth * ratios["fanout_depth"] * 2.0),
    );

    let mut fanout_old = f64::INFINITY;
    let mut fanout_new = f64::INFINITY;
    for (key, old_row) in &old {
        for speed in SPEEDS {
            let old_value = frames_p999(old_row, key, speed)?;
            if old_value > 0.0 {
                fanout_old = fanout_old.min(baseline.fanout_depth / old_value);
            }
            let new_value = frames_p999(new[key], key, speed)?;
            if new_value > 0.0 {
                fanout_new = fanout_new.min(proposed["fanout_depth"] / new_value);
            }
        }
    }
    let mut horizons = BTreeMap::new();
    horizons.insert("fanout_old_wall_seconds".to_string(), fanout_old);
    horizons.insert("fanout_new_wall_seconds".to_string(), fanout_new);

    Ok(Comparison {
        ratios,
        observed,
        required_reach,
        proposed,
        horizons,
    })
}

/// The Brick B0 identity gate: a freshly measured protocol-9 fixture must equal
/// the historical protocol-8 fixture everywhere but the three separately
/// validated fields, verifying `5fc974d`'s byte-identity claim against the
/// current implementation.
///
/// # Errors
/// [`LabError::Refusal`] on a version mismatch, a missing or equal pairing, a
/// non-canonical projection label, or ANY other differing field.
pub fn verify_8_9_identity(eight_path: &Path, nine_path: &Path) -> LabResult<()> {
    let eight: Value = serde_json::from_str(&std::fs::read_to_string(eight_path)?)?;
    let nine: Value = serde_json::from_str(&std::fs::read_to_string(nine_path)?)?;
    for (fixture, want, path) in [(&eight, 8, eight_path), (&nine, 9, nine_path)] {
        let version = fixture
            .get("tape_protocol_version")
            .and_then(Value::as_i64)
            .ok_or_else(|| LabError::refusal("fixture carries no tape_protocol_version"))?;
        if version != want {
            return Err(LabError::refusal(format!(
                "{} is protocol {version}, not {want}",
                path.display()
            )));
        }
    }

    let (Some(pairing_eight), Some(pairing_nine)) =
        (eight.get("pairing_id"), nine.get("pairing_id"))
    else {
        return Err(LabError::refusal(
            "a fixture carries no pairing id, so nothing shows which traversal produced it; \
             regenerate with `mogwai tick-composition`",
        ));
    };
    if pairing_eight == pairing_nine {
        return Err(LabError::refusal(
            "the two fixtures carry the same pairing id, so the same traversal is being \
             compared with itself rather than remeasured",
        ));
    }

    for fixture in [&eight, &nine] {
        let version = fixture["tape_protocol_version"]
            .as_i64()
            .expect("checked above");
        let expected = format!("all protocol-{version} frames");
        let projection = fixture.get("projection").and_then(Value::as_str);
        if projection != Some(expected.as_str()) {
            return Err(LabError::refusal(format!(
                "projection is {projection:?}, not the producer's canonical {expected:?} - a \
                 projection differing beyond the version number is a real change, not metadata"
            )));
        }
    }

    let strip = |value: &Value| -> BTreeMap<String, Value> {
        value
            .as_object()
            .expect("fixtures are objects")
            .iter()
            .filter(|(k, _)| !IDENTITY_SEPARATELY_VALIDATED.contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    };
    let stripped_eight = strip(&eight);
    let stripped_nine = strip(&nine);
    let mut fields: Vec<&String> = stripped_eight.keys().chain(stripped_nine.keys()).collect();
    fields.sort();
    fields.dedup();
    for field_name in fields {
        match (
            stripped_eight.get(field_name),
            stripped_nine.get(field_name),
        ) {
            (Some(left), Some(right)) => {
                if left != right {
                    return Err(LabError::refusal(format!(
                        "identity check: field {field_name} differs between protocol 8 and the \
                         remeasured protocol 9 - the byte-identity claim of 5fc974d does not \
                         hold on the current implementation"
                    )));
                }
            }
            _ => {
                return Err(LabError::refusal(format!(
                    "identity check: field {field_name} exists in only one fixture"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_of_two_rounds_up_to_the_next_power() {
        assert_eq!(power_of_two(1.0), 1.0);
        assert_eq!(power_of_two(1.5), 2.0);
        assert_eq!(power_of_two(2.0), 2.0);
        assert_eq!(power_of_two(2.5), 4.0);
        assert_eq!(power_of_two(262_144.0), 262_144.0);
        assert_eq!(power_of_two(262_145.0), 524_288.0);
    }

    #[test]
    fn million_rounds_up_to_the_next_whole_million() {
        assert_eq!(million(1.0), 1_000_000.0);
        assert_eq!(million(1_000_000.0), 1_000_000.0);
        assert_eq!(million(1_000_001.0), 2_000_000.0);
        assert_eq!(million(281_999_999.0), 282_000_000.0);
    }

    /// The baselines are HISTORY, not current values, and the table must not be
    /// silently re-derived from whatever ships today. Pinned so an edit is a
    /// deliberate act with a failing test attached.
    #[test]
    fn the_mode_baselines_are_frozen_history() {
        let projection = mode("projection").unwrap();
        assert_eq!(projection.baseline.checkpoint_k, 262_144.0);
        assert_eq!(projection.baseline.fanout_depth, 65_536.0);

        let independent = mode("independent").unwrap();
        assert_eq!(independent.baseline.checkpoint_k, 1_048_576.0);
        assert_eq!(independent.baseline.warmup_baseline, 81_124_000_000.0);

        // `max_extend_ticks` is 1 << 30 in EVERY mode: it is a per-lock runaway
        // backstop rather than a reach ceiling, and is deliberately never
        // scaled by a ratio.
        for m in &MODES {
            assert_eq!(
                m.baseline.max_extend_ticks, 1_073_741_824.0,
                "{}: the extend backstop must not be resized",
                m.name
            );
        }
    }

    /// The rejected protocol-11 fanout proposal has to stay findable, or the
    /// next comparison re-proposes it and re-litigates a settled ruling.
    #[test]
    fn the_rejected_fanout_proposal_is_recorded() {
        let (mode_name, field, value) = REJECTED_PROPOSALS[0];
        assert_eq!(mode_name, "independent_10_11");
        assert_eq!(field, "fanout_depth");
        assert_eq!(value, 16_777_216.0);
        assert!(
            mode(mode_name).is_ok(),
            "the rejection names a mode that must exist"
        );
    }
}
