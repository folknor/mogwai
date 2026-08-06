// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Slice 2c-i of notes/rust-rewrite-phases.md: the section-10 artifact
//! assembler and its two validators, ported from `analysis/mnq_fit.py`'s
//! `assemble_measure12a_artifact`/`_assemble_measure12a`,
//! `measure12a_schema_errors`, `measure12a_semantic_errors`,
//! `load_brick_g_walks`, `json_safe` and `write_json_atomic`.
//!
//! This module does not run the corpus or the walks - it assembles the
//! section-10 shape from records phase 2b already computed
//! ([`super::assemble::measure`]) plus the raw cached seed records, and
//! validates the result. Live measurement (the resource sampler, the
//! fresh-tree gate, the in-process attestation replay, `mogwai measure`)
//! is slice 2c-ii's scope.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use super::assemble::{Measurement, SeedRecord, measure};
use super::context::ObsContext;
use super::{ja, js};
use crate::error::{LabError, LabResult};
use crate::kernel::typed_canon;
use crate::subcontract::{
    BOOTSTRAP_BASE_SEED, BOOTSTRAP_BLOCK_SESSIONS, BOOTSTRAP_REPLICATES, COLD_HOURS,
    CONTROL_ESCALATION_MAX, CONTROL_TIE_BASE_SEED, COUNT_WINDOWS_S, EXCEEDANCE_TICKS,
    FAIL_HOURS_60, FAIL_HOURS_300, FAMILY_ENVELOPE_LEVEL, FOLD_MIN_SESSIONS, GAP_CLOSE_EPS,
    GAP_CLOSE_LCB_MIN, GAP_CLOSE_MIN, HOT_HOURS, INITIATION_INNOVATION_MIN, INNOVATION_EXCEED_ABS,
    MATERIALITY_BAND, MIN_1S_CELL_RETURNS, MIN_5S_CELL_RETURNS, MIN_15S_CELL_RETURNS,
    MIN_60S_CELL_RETURNS, MIN_300S_CELL_RETURNS, MIN_BOUNDARY_60S_CELL_RETURNS,
    MIN_BOUNDARY_MINUTES_CELL, MIN_MINUTES_CELL, MIN_RESIDUAL_CELL, PARENT_COUNT_BIN_NAMES,
    PERMUTATION_BASE_SEED, PERMUTATION_REPLICATES, PERMUTATION_VARIANTS, RESIDUAL_EXCEED_MULTIPLES,
    RESIDUAL_MIN_HISTORY, RESIDUAL_WINDOW_S, SEED_DIRECTION_MIN, SIGMA_ESCALATION_MIN,
    SINCE_OPEN_BIN_NAMES, UNTIL_CLOSE_BIN_NAMES, WALL_HORIZONS_S,
};

use super::monthly::{aggregate_permutations, blocks_from_sessions};

// -- One cached generated-seed walk record -----------------------------------

/// One generated seed's raw cached record, as it sits on disk (Brick G /
/// Brick M walk cache): `{seed, per_session, forensic, cost}`.
#[derive(Clone)]
pub struct GeneratedSeed {
    pub seed: i64,
    pub per_session: Vec<Value>,
    pub forensic: Value,
    pub cost: Value,
}

impl GeneratedSeed {
    /// Read one from a cached JSON record, refusing a non-integer seed.
    pub fn from_cached(record: &Value) -> LabResult<Self> {
        let seed = record
            .get("seed")
            .and_then(strict_i64)
            .ok_or_else(|| LabError::refusal("cached walk record carries a non-integer seed"))?;
        Ok(Self {
            seed,
            per_session: ja(record, "per_session").to_vec(),
            forensic: record["forensic"].clone(),
            cost: record["cost"].clone(),
        })
    }
}

fn strict_i64(v: &Value) -> Option<i64> {
    // A JSON boolean is not a `Value::Number`, so `as_i64` already excludes
    // it; kept as a named helper so the "strict int" intent reads at call
    // sites the way `_strict_int` does in the Python.
    v.as_i64()
}

fn strict_f64(v: &Value) -> Option<f64> {
    if v.is_boolean() {
        return None;
    }
    v.as_f64()
}

// -- The public entrypoint and its internal body -----------------------------

/// `assemble_measure12a_artifact`: the production entrypoint. Refuses
/// UNCONDITIONALLY unless `mults` is exactly `BOOTSTRAP_REPLICATES` long -
/// the hard gate a truncated selftest/parity fixture must go around by
/// calling [`assemble_measure12a_body`] directly.
pub fn assemble_measure12a_artifact(
    observed: &Value,
    generated_seeds: &[GeneratedSeed],
    binding_extra: &Value,
    mults: &[Vec<i64>],
    cost: &Value,
) -> LabResult<Value> {
    if mults.len() as i64 != BOOTSTRAP_REPLICATES {
        return Err(LabError::refusal(format!(
            "bootstrap population {} is not the required {BOOTSTRAP_REPLICATES} replicates",
            mults.len()
        )));
    }
    assemble_measure12a_body(observed, generated_seeds, binding_extra, mults, cost)
}

/// `_assemble_measure12a`: the section-10 artifact from the observed half
/// and the per-seed generated records, taking whatever bootstrap population
/// it is given (the selftest/parity truncated fixture goes through here).
pub fn assemble_measure12a_body(
    observed: &Value,
    generated_seeds: &[GeneratedSeed],
    binding_extra: &Value,
    mults: &[Vec<i64>],
    cost: &Value,
) -> LabResult<Value> {
    let per_session: Vec<Value> = ja(observed, "per_session").to_vec();
    let seed_records: Vec<SeedRecord> = generated_seeds
        .iter()
        .map(|g| SeedRecord {
            seed: g.seed,
            per_session: g.per_session.clone(),
            forensic: g.forensic.clone(),
        })
        .collect();
    let measurement: Measurement = measure(&per_session, &seed_records, mults)?;

    // generated.per_seed: 2b's (seed, blocks, count_substitution) plus the
    // verbatim forensic/cost this module owns.
    let mut gen_per_seed: Vec<Value> = Vec::with_capacity(generated_seeds.len());
    for ((seed, blocks, csub), g) in measurement.per_seed.iter().zip(generated_seeds.iter()) {
        debug_assert_eq!(*seed, g.seed, "seed order must match the input order");
        gen_per_seed.push(serde_json::json!({
            "seed": seed,
            "blocks": blocks,
            "count_substitution": csub,
            "forensic": g.forensic,
            "cost": g.cost,
        }));
    }

    // diagnostics.refused_cells: the frozen collection order (spec sec 10) -
    // family envelopes, rungs, seed support refusals (all from 2b, already
    // in that order), then the scoped per-session/forensic mirrors.
    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut refused_cells: Vec<Value> = Vec::new();
    let add_refusal = |rec: &Value, seen: &mut BTreeSet<(String, String, String)>, out: &mut Vec<Value>| {
        let key = (
            js(rec, "scope").to_string(),
            js(rec, "cell").to_string(),
            js(rec, "reason").to_string(),
        );
        if seen.insert(key) {
            out.push(rec.clone());
        }
    };
    for r in measurement.refusals() {
        add_refusal(&r.to_json(), &mut seen, &mut refused_cells);
    }
    for rec in &per_session {
        for r in ja(rec, "refusals") {
            add_refusal(r, &mut seen, &mut refused_cells);
        }
    }
    for g in generated_seeds {
        for rec in &g.per_session {
            for r in ja(rec, "refusals") {
                add_refusal(r, &mut seen, &mut refused_cells);
            }
        }
        for r in ja(&g.forensic, "refusals") {
            add_refusal(r, &mut seen, &mut refused_cells);
        }
    }

    // diagnostics.empty_bins: any (hour, parent-count-bin) with zero
    // observed block1 votes at the point estimate.
    let obs_ctx = ObsContext::new(per_session.clone());
    let ones = obs_ctx.ones();
    let mut empty_bins: Vec<Value> = Vec::new();
    for hour in 0..24i64 {
        for bin in PARENT_COUNT_BIN_NAMES {
            if obs_ctx.b1_bin_count(hour, bin, &ones) == 0 {
                empty_bins.push(serde_json::json!({
                    "scope": "observed block1",
                    "cell": format!("hour {hour} bin {bin}"),
                }));
            }
        }
    }

    // diagnostics.warmup_exclusions: summed by hour (as a string key, the
    // Python dict-with-int-keys JSON-serializes the same way), over every
    // hour any session's block4 carries other than the literal "all".
    let mut hours: BTreeSet<i64> = BTreeSet::new();
    for rec in &per_session {
        if let Some(b4) = rec.get("block4").and_then(Value::as_object) {
            for k in b4.keys() {
                if k != "all"
                    && let Ok(h) = k.parse::<i64>()
                {
                    hours.insert(h);
                }
            }
        }
    }
    let mut warmup_exclusions = serde_json::Map::new();
    for h in hours {
        let key = h.to_string();
        let mut sum = 0i64;
        for rec in &per_session {
            if let Some(cell) = rec.get("block4").and_then(|b4| b4.get(&key)) {
                sum += cell.get("warmup_excluded").and_then(Value::as_i64).unwrap_or(0);
            }
        }
        warmup_exclusions.insert(key, serde_json::json!(sum));
    }

    // binding: observed's binding fields with binding_extra layered on top
    // (harness_tree_commit, generated).
    let mut binding = observed["binding"]
        .as_object()
        .ok_or_else(|| LabError::refusal("observed.binding is not an object"))?
        .clone();
    if let Some(extra) = binding_extra.as_object() {
        for (k, v) in extra {
            binding.insert(k.clone(), v.clone());
        }
    }

    Ok(serde_json::json!({
        "binding": Value::Object(binding),
        "constants": constants_block(),
        "observed": {
            "per_session": per_session,
            "monthly": measurement.observed_monthly,
            "permutations_monthly": measurement.observed_permutations_monthly,
        },
        "generated": {
            "per_seed": gen_per_seed,
            "central": measurement.central_json(),
        },
        "bootstrap": measurement.ladder.bootstrap_json(measurement.replicates),
        "ladder": measurement.ladder.ladder_json(),
        "cost": cost,
        "diagnostics": {
            "warmup_exclusions": Value::Object(warmup_exclusions),
            "refused_cells": refused_cells,
            "empty_bins": empty_bins,
            "worsening_23": measurement.worsening_23_json(),
        },
    }))
}

/// The exact section-7 constant names and values, verbatim (spec-named,
/// mirroring `MEASURE12A_CONSTANT_NAMES`'s two aliased bin-name keys).
fn constants_block() -> Value {
    serde_json::json!({
        "FAIL_HOURS_300": FAIL_HOURS_300,
        "FAIL_HOURS_60": FAIL_HOURS_60,
        "HOT_HOURS": HOT_HOURS,
        "COLD_HOURS": COLD_HOURS,
        "RESIDUAL_WINDOW_S": RESIDUAL_WINDOW_S,
        "RESIDUAL_MIN_HISTORY": RESIDUAL_MIN_HISTORY,
        "RESIDUAL_EXCEED_MULTIPLES": RESIDUAL_EXCEED_MULTIPLES,
        "INNOVATION_EXCEED_ABS": INNOVATION_EXCEED_ABS,
        "PERMUTATION_REPLICATES": PERMUTATION_REPLICATES,
        "PERMUTATION_VARIANTS": PERMUTATION_VARIANTS,
        "BOOTSTRAP_REPLICATES": BOOTSTRAP_REPLICATES,
        "BOOTSTRAP_BLOCK_SESSIONS": BOOTSTRAP_BLOCK_SESSIONS,
        "BOOTSTRAP_BASE_SEED": BOOTSTRAP_BASE_SEED,
        "PERMUTATION_BASE_SEED": PERMUTATION_BASE_SEED,
        "CONTROL_TIE_BASE_SEED": CONTROL_TIE_BASE_SEED,
        "FAMILY_ENVELOPE_LEVEL": FAMILY_ENVELOPE_LEVEL,
        "SEED_DIRECTION_MIN": SEED_DIRECTION_MIN,
        "FOLD_MIN_SESSIONS": FOLD_MIN_SESSIONS,
        "MATERIALITY_BAND": [MATERIALITY_BAND.0, MATERIALITY_BAND.1],
        "GAP_CLOSE_MIN": GAP_CLOSE_MIN,
        "GAP_CLOSE_LCB_MIN": GAP_CLOSE_LCB_MIN,
        "GAP_CLOSE_EPS": GAP_CLOSE_EPS,
        "COUNT_WINDOWS_S": COUNT_WINDOWS_S,
        "WALL_HORIZONS_S": WALL_HORIZONS_S,
        "EXCEEDANCE_TICKS": EXCEEDANCE_TICKS,
        "MIN_1S_CELL_RETURNS": MIN_1S_CELL_RETURNS,
        "MIN_5S_CELL_RETURNS": MIN_5S_CELL_RETURNS,
        "MIN_15S_CELL_RETURNS": MIN_15S_CELL_RETURNS,
        "MIN_60S_CELL_RETURNS": MIN_60S_CELL_RETURNS,
        "MIN_300S_CELL_RETURNS": MIN_300S_CELL_RETURNS,
        "MIN_RESIDUAL_CELL": MIN_RESIDUAL_CELL,
        "MIN_MINUTES_CELL": MIN_MINUTES_CELL,
        "MIN_BOUNDARY_MINUTES_CELL": MIN_BOUNDARY_MINUTES_CELL,
        "MIN_BOUNDARY_60S_CELL_RETURNS": MIN_BOUNDARY_60S_CELL_RETURNS,
        "SIGMA_ESCALATION_MIN": SIGMA_ESCALATION_MIN,
        "CONTROL_ESCALATION_MAX": CONTROL_ESCALATION_MAX,
        "INITIATION_INNOVATION_MIN": INITIATION_INNOVATION_MIN,
        "PARENT_COUNT_BINS": PARENT_COUNT_BIN_NAMES,
        "SEGMENT_OPEN_BINS_S": SINCE_OPEN_BIN_NAMES,
        "SEGMENT_CLOSE_BINS_S": UNTIL_CLOSE_BIN_NAMES,
    })
}

const MEASURE12A_CONSTANT_NAMES: &[&str] = &[
    "FAIL_HOURS_300",
    "FAIL_HOURS_60",
    "HOT_HOURS",
    "COLD_HOURS",
    "RESIDUAL_WINDOW_S",
    "RESIDUAL_MIN_HISTORY",
    "RESIDUAL_EXCEED_MULTIPLES",
    "INNOVATION_EXCEED_ABS",
    "PERMUTATION_REPLICATES",
    "PERMUTATION_VARIANTS",
    "BOOTSTRAP_REPLICATES",
    "BOOTSTRAP_BLOCK_SESSIONS",
    "BOOTSTRAP_BASE_SEED",
    "PERMUTATION_BASE_SEED",
    "CONTROL_TIE_BASE_SEED",
    "FAMILY_ENVELOPE_LEVEL",
    "SEED_DIRECTION_MIN",
    "FOLD_MIN_SESSIONS",
    "MATERIALITY_BAND",
    "GAP_CLOSE_MIN",
    "GAP_CLOSE_LCB_MIN",
    "GAP_CLOSE_EPS",
    "COUNT_WINDOWS_S",
    "WALL_HORIZONS_S",
    "EXCEEDANCE_TICKS",
    "PARENT_COUNT_BINS",
    "SEGMENT_OPEN_BINS_S",
    "SEGMENT_CLOSE_BINS_S",
    "MIN_1S_CELL_RETURNS",
    "MIN_5S_CELL_RETURNS",
    "MIN_15S_CELL_RETURNS",
    "MIN_60S_CELL_RETURNS",
    "MIN_300S_CELL_RETURNS",
    "MIN_RESIDUAL_CELL",
    "MIN_MINUTES_CELL",
    "MIN_BOUNDARY_MINUTES_CELL",
    "MIN_BOUNDARY_60S_CELL_RETURNS",
    "SIGMA_ESCALATION_MIN",
    "CONTROL_ESCALATION_MAX",
    "INITIATION_INNOVATION_MIN",
];

const MEASURE12A_FAMILIES: [&str; 6] = [
    "child_walk",
    "arrival",
    "innovation",
    "reversion",
    "garch",
    "boundary",
];

fn rung_subchecks(name: &str) -> Option<&'static [&'static str]> {
    Some(match name {
        "child_walk" => &["a_print_excess", "b_mid_clean"],
        "arrival" => &["a_envelope", "b_closure", "c_conditional"],
        "innovation" => &["a_tail_ratio", "b_initiation", "c_controls"],
        "reversion" => &["a_closure", "b_folds", "c_covariance"],
        "garch" => &["a_closure", "b_escalation"],
        "boundary" => &["a_boundary_band", "b_comparator_clean", "c_no_prior_rung"],
        _ => return None,
    })
}

const B1_HIST_KEYS: &[&str] = &[
    "n",
    "quote_range_half_ticks",
    "trade_range_ticks",
    "hour",
    "since_open_bin",
    "until_close_bin",
    "count",
];
const B1_BIN_KEYS: &[&str] = &[
    "minute_count",
    "quote_range_denominator",
    "quote_range_p50",
    "quote_range_p90",
    "quote_range_p99",
    "quote_range_p999",
    "trade_range_p50",
    "trade_range_p90",
    "trade_range_p99",
    "trade_range_p999",
    "trade_range_sqrt_n_p50",
    "trade_range_sqrt_n_p90",
    "trade_range_sqrt_n_p99",
];
const B1_SUMMARY_EXTRA_KEYS: &[&str] = &[
    "n_p50",
    "n_p90",
    "n_p99",
    "n_p999",
    "exceed_399",
    "exceed_642",
    "exceed_968",
    "denominator",
    "trade_to_quote_p99_ratio",
    "by_parent_count_bin",
];
const B2_CELL_KEYS: &[&str] = &[
    "scheduled_windows",
    "zero_windows",
    "count_hist",
    "run_length_hist",
    "paired_lag_count",
    "sum_x",
    "sum_y",
    "sumsq_x",
    "sumsq_y",
    "sum_xy",
    "zero_fraction",
    "mean",
    "fano",
    "count_p90",
    "count_p99",
    "count_p999",
    "lag1_autocorr",
    "run_p90",
];
const B3_CELL_KEYS: &[&str] = &["return_count", "robust_scale", "rms_scale"];
const B3_PAIR_KEYS: &[&str] = &["window_count", "vr", "cov_contrib", "cov_contrib_norm"];
const B4_CELL_KEYS: &[&str] = &[
    "residual_count",
    "warmup_excluded",
    "zero_fraction",
    "nz_abs_p90",
    "nz_abs_p99",
    "nz_abs_p999",
    "ratio_p99_p90",
    "ratio_p999_p99",
    "exceed_4",
    "exceed_8",
    "exceed_16",
];
const PERM_KEYS: &[&str] = &[
    "segment_index",
    "hour",
    "variant",
    "replicate",
    "return_count_60",
    "sum_abs_60",
    "max_abs_60",
    "return_count_300",
    "sum_abs_300",
    "max_abs_300",
];
const FORENSIC_KEYS: &[&str] = &[
    "seed",
    "kind",
    "matched_extreme_minute_start",
    "minute_start_ns",
    "minute_end_ns",
    "utc_hour",
    "segment_index",
    "parent_count",
    "trade_count",
    "traced_parents",
    "largest_innovation_std",
    "largest_innovation_ts_ns",
    "innovation_exceed_4",
    "innovation_exceed_8",
    "innovation_exceed_16",
    "initiation",
    "sigma_start",
    "sigma_peak",
    "sigma_end",
    "sigma_escalation",
    "latent_mid_range_ticks",
    "quote_mid_range_half_ticks",
    "trade_range_ticks",
    "trade_to_quote_range_ratio",
    "quote_to_latent_range_ratio",
    "max_signed_run",
    "clamp_hits",
    "arch_share_next",
    "arch_share_minute_max",
];
const METRIC_KEYS: &[&str] = &[
    "name",
    "kind",
    "predicate",
    "point",
    "se",
    "interval_low",
    "interval_high",
    "band_low",
    "band_high",
    "outside_band",
    "envelope_excludes_edge",
    "interval_inside_band",
    "seed_same_side_count",
    "seed_inside_count",
    "seed_rule_pass",
    "fold_rule_pass",
    "refused",
];
/// `_METRIC_NULLABLE`: every metric key except the four that are never null
/// (`name`, `kind`, `predicate`, `refused`).
const METRIC_NULLABLE: &[&str] = &[
    "point",
    "se",
    "interval_low",
    "interval_high",
    "band_low",
    "band_high",
    "outside_band",
    "envelope_excludes_edge",
    "interval_inside_band",
    "seed_same_side_count",
    "seed_inside_count",
    "seed_rule_pass",
    "fold_rule_pass",
];
const RUNG_KEYS: &[&str] = &[
    "name",
    "subchecks",
    "fired",
    "boundary_localized",
    "refusals",
    "uniform_eligible",
    "required_resolution",
];
const COND_KEYS: &[&str] = &[
    "hour",
    "bin_name",
    "observed_p99",
    "generated_p99",
    "ratio",
    "interval_low",
    "interval_high",
    "interval_inside_band",
    "seed_inside_count",
    "required",
    "supported",
];
const COUNTSUB_KEYS: &[&str] = &[
    "shares_observed",
    "shares_generated",
    "weights",
    "refused_hours",
    "support_refusals",
    "counterfactual_p999",
    "counterfactual_exceed_968",
    "closure_p999",
    "closure_lcb",
    "conditional_adequacy",
    "diagnostic_closure_to_bound",
];

// -- The recursive schema validator ------------------------------------------

struct SchemaValidator {
    errs: Vec<String>,
}

impl SchemaValidator {
    /// `keys_exact`: the object's key SET equals `want` exactly. Records a
    /// violation (and returns `false`) on a type mismatch or a key-set
    /// mismatch, matching the Python's `got != set(want)` symmetric-difference
    /// report.
    fn keys_exact(&mut self, obj: &Value, want: &[&str], where_: &str) -> bool {
        let Some(map) = obj.as_object() else {
            self.errs.push(format!("{where_}: not a dict"));
            return false;
        };
        let got: BTreeSet<&str> = map.keys().map(String::as_str).collect();
        let want_set: BTreeSet<&str> = want.iter().copied().collect();
        if got != want_set {
            let mut diff: Vec<&str> = got.symmetric_difference(&want_set).copied().collect();
            diff.sort_unstable();
            self.errs.push(format!("{where_}: key mismatch {diff:?}"));
            return false;
        }
        true
    }

    fn refusal_rec(&mut self, obj: &Value, where_: &str) {
        if self.keys_exact(obj, &["scope", "cell", "reason"], where_)
            && !["scope", "cell", "reason"]
                .iter()
                .all(|k| obj.get(*k).is_some_and(Value::is_string))
        {
            self.errs
                .push(format!("{where_}: RefusalRec fields must be three strings"));
        }
    }

    fn block1_summary_rec(&mut self, obj: &Value, where_: &str) {
        let want: Vec<&str> = B1_BIN_KEYS.iter().chain(B1_SUMMARY_EXTRA_KEYS).copied().collect();
        if self.keys_exact(obj, &want, where_)
            && let Some(bins) = obj.get("by_parent_count_bin")
        {
            let bwhere = format!("{where_}.by_parent_count_bin");
            if self.keys_exact(bins, PARENT_COUNT_BIN_NAMES, &bwhere)
                && let Some(map) = bins.as_object()
            {
                for (name, b) in map {
                    self.keys_exact(b, B1_BIN_KEYS, &format!("{where_}.bin[{name}]"));
                }
            }
        }
    }

    fn block1_blocks_rec(&mut self, obj: &Value, where_: &str, with_hist: bool) {
        let mut want: Vec<&str> = vec!["summary", "by_labels"];
        if with_hist {
            want.push("hist");
        }
        if !self.keys_exact(obj, &want, where_) {
            return;
        }
        if with_hist {
            for (i, row) in ja(obj, "hist").iter().enumerate() {
                self.keys_exact(row, B1_HIST_KEYS, &format!("{where_}.hist[{i}]"));
            }
        }
        if let Some(summary) = obj.get("summary").and_then(Value::as_object) {
            for (h, s) in summary {
                self.block1_summary_rec(s, &format!("{where_}.summary[{h}]"));
            }
        }
        if let Some(by_labels) = obj.get("by_labels").and_then(Value::as_object) {
            for (lp, per) in by_labels {
                if let Some(per) = per.as_object() {
                    for (h, s) in per {
                        self.block1_summary_rec(s, &format!("{where_}.by_labels[{lp}][{h}]"));
                    }
                }
            }
        }
    }

    fn block2_map(&mut self, obj: &Value, where_: &str) {
        let Some(map) = obj.as_object() else {
            self.errs.push(format!("{where_}: not a dict"));
            return;
        };
        for (h, per_w) in map {
            if let Some(per_w) = per_w.as_object() {
                for (w, c) in per_w {
                    self.keys_exact(c, B2_CELL_KEYS, &format!("{where_}[{h}][{w}]"));
                }
            }
        }
    }

    fn block3_rec(&mut self, obj: &Value, where_: &str) {
        if !self.keys_exact(
            obj,
            &["cells", "pairs", "lag1_parent_autocorr", "hour20_labels"],
            where_,
        ) {
            return;
        }
        if let Some(cells) = obj.get("cells").and_then(Value::as_object) {
            for (h, per) in cells {
                if let Some(per) = per.as_object() {
                    for (hz, c) in per {
                        self.keys_exact(c, B3_CELL_KEYS, &format!("{where_}.cells[{h}][{hz}]"));
                    }
                }
            }
        }
        if let Some(pairs) = obj.get("pairs").and_then(Value::as_object) {
            for (h, per) in pairs {
                if let Some(per) = per.as_object() {
                    for (p, c) in per {
                        self.keys_exact(c, B3_PAIR_KEYS, &format!("{where_}.pairs[{h}][{p}]"));
                    }
                }
            }
        }
        if let Some(hour20) = obj.get("hour20_labels").and_then(Value::as_object) {
            for (lp, per) in hour20 {
                if let Some(per) = per.as_object() {
                    for (hz, c) in per {
                        self.keys_exact(c, B3_CELL_KEYS, &format!("{where_}.hour20[{lp}][{hz}]"));
                    }
                }
            }
        }
    }

    fn block4_map(&mut self, obj: &Value, where_: &str) {
        let Some(map) = obj.as_object() else {
            self.errs.push(format!("{where_}: not a dict"));
            return;
        };
        if !map.contains_key("all") {
            self.errs.push(format!(
                "{where_}: missing the Amendment-B literal \"all\" pooled-hours cell"
            ));
        }
        for (h, c) in map {
            self.keys_exact(c, B4_CELL_KEYS, &format!("{where_}[{h}]"));
        }
    }

    fn seed_blocks(&mut self, obj: &Value, where_: &str, with_hist: bool) {
        if !self.keys_exact(obj, &["block1", "block2", "block3", "block4"], where_) {
            return;
        }
        self.block1_blocks_rec(&obj["block1"], &format!("{where_}.block1"), with_hist);
        self.block2_map(&obj["block2"], &format!("{where_}.block2"));
        self.block3_rec(&obj["block3"], &format!("{where_}.block3"));
        self.block4_map(&obj["block4"], &format!("{where_}.block4"));
    }
}

/// `measure12a_schema_errors`: the recursive exact-key section-10 validator.
/// Returns a flat list of violation strings; empty means conformant.
#[must_use]
pub fn measure12a_schema_errors(artifact: &Value) -> Vec<String> {
    let mut v = SchemaValidator { errs: Vec::new() };

    if !v.keys_exact(
        artifact,
        &[
            "binding",
            "constants",
            "observed",
            "generated",
            "bootstrap",
            "ladder",
            "cost",
            "diagnostics",
        ],
        "top",
    ) {
        return v.errs;
    }

    let binding = &artifact["binding"];
    if v.keys_exact(
        binding,
        &[
            "harness_tree_commit",
            "job_id",
            "subcontract_hash",
            "preflight_artifact_hash",
            "file_hashes",
            "tape_protocol_version",
            "generated",
        ],
        "binding",
    ) {
        v.keys_exact(
            &binding["generated"],
            &["seeds", "window_start_ns", "window_length_ns", "warmup"],
            "binding.generated",
        );
    }

    v.keys_exact(&artifact["constants"], MEASURE12A_CONSTANT_NAMES, "constants");

    let observed = &artifact["observed"];
    let mut scoped_refusals: Vec<Value> = Vec::new();
    if v.keys_exact(
        observed,
        &["per_session", "monthly", "permutations_monthly"],
        "observed",
    ) {
        for (i, rec) in ja(observed, "per_session").iter().enumerate() {
            let where_ = format!("observed.per_session[{i}]");
            if !v.keys_exact(
                rec,
                &[
                    "session_date",
                    "segments",
                    "block1_hist",
                    "block2",
                    "block3",
                    "block4",
                    "permutations",
                    "refusals",
                ],
                &where_,
            ) {
                continue;
            }
            for (j, sg) in ja(rec, "segments").iter().enumerate() {
                v.keys_exact(
                    sg,
                    &["segment_index", "open_ns", "close_ns"],
                    &format!("{where_}.segments[{j}]"),
                );
            }
            for (j, row) in ja(rec, "block1_hist").iter().enumerate() {
                v.keys_exact(row, B1_HIST_KEYS, &format!("{where_}.hist[{j}]"));
            }
            v.block2_map(&rec["block2"], &format!("{where_}.block2"));
            v.block3_rec(&rec["block3"], &format!("{where_}.block3"));
            v.block4_map(&rec["block4"], &format!("{where_}.block4"));
            for (j, p) in ja(rec, "permutations").iter().enumerate() {
                v.keys_exact(p, PERM_KEYS, &format!("{where_}.perm[{j}]"));
            }
            for r in ja(rec, "refusals") {
                v.refusal_rec(r, &format!("{where_}.refusals"));
                scoped_refusals.push(r.clone());
            }
        }
        v.seed_blocks(&observed["monthly"], "observed.monthly", true);
        let pm = &observed["permutations_monthly"];
        if v.keys_exact(pm, PERMUTATION_VARIANTS, "permutations_monthly")
            && let Some(map) = pm.as_object()
        {
            for (variant, per) in map {
                if let Some(per) = per.as_object() {
                    for (h, c) in per {
                        v.keys_exact(
                            c,
                            &["robust_scale_60", "robust_scale_300"],
                            &format!("permutations_monthly[{variant}][{h}]"),
                        );
                    }
                }
            }
        }
    }

    let generated = &artifact["generated"];
    let mut rungs: Vec<Value> = Vec::new();
    if v.keys_exact(generated, &["per_seed", "central"], "generated") {
        for (i, g) in ja(generated, "per_seed").iter().enumerate() {
            let where_ = format!("generated.per_seed[{i}]");
            if !v.keys_exact(
                g,
                &["seed", "blocks", "count_substitution", "forensic", "cost"],
                &where_,
            ) {
                continue;
            }
            v.seed_blocks(&g["blocks"], &format!("{where_}.blocks"), true);
            let cs = &g["count_substitution"];
            if v.keys_exact(cs, COUNTSUB_KEYS, &format!("{where_}.count_substitution")) {
                for (j, rec) in ja(cs, "conditional_adequacy").iter().enumerate() {
                    v.keys_exact(rec, COND_KEYS, &format!("{where_}.cond_adequacy[{j}]"));
                }
                for r in ja(cs, "support_refusals") {
                    v.refusal_rec(r, &format!("{where_}.support_refusals"));
                    scoped_refusals.push(r.clone());
                }
            }
            let forensic = &g["forensic"];
            if v.keys_exact(forensic, &["records", "refusals"], &format!("{where_}.forensic")) {
                for (j, rec) in ja(forensic, "records").iter().enumerate() {
                    v.keys_exact(rec, FORENSIC_KEYS, &format!("{where_}.forensic[{j}]"));
                }
                for r in ja(forensic, "refusals") {
                    v.refusal_rec(r, &format!("{where_}.forensic.refusals"));
                    scoped_refusals.push(r.clone());
                }
            }
            v.keys_exact(&g["cost"], &["walk_s", "rss_bytes"], &format!("{where_}.cost"));
        }
        let central = &generated["central"];
        if v.keys_exact(
            central,
            &["blocks", "count_substitution", "pooled_diagnostic_hist"],
            "generated.central",
        ) {
            v.seed_blocks(&central["blocks"], "central.blocks", false);
            v.keys_exact(
                &central["count_substitution"],
                &["closure_p999_median", "refused_hour_union"],
                "central.count_substitution",
            );
        }
    }

    let bootstrap = &artifact["bootstrap"];
    if v.keys_exact(bootstrap, &["seed_rule", "replicates", "per_family"], "bootstrap") {
        let per_family = &bootstrap["per_family"];
        if v.keys_exact(per_family, &MEASURE12A_FAMILIES, "per_family")
            && let Some(map) = per_family.as_object()
        {
            for (fam, env) in map {
                let where_ = format!("per_family[{fam}]");
                if !v.keys_exact(
                    env,
                    &["metrics", "critical_value", "inventory_complete"],
                    &where_,
                ) {
                    continue;
                }
                let complete = env["inventory_complete"] == Value::Bool(true);
                if complete && env["critical_value"].is_null() {
                    v.errs
                        .push(format!("{where_}: complete inventory without a critical value"));
                }
                if !complete && !env["critical_value"].is_null() {
                    v.errs
                        .push(format!("{where_}: incomplete inventory with a critical value"));
                }
                for m in ja(env, "metrics") {
                    let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
                    let mwhere = format!("{where_}.metric[{name}]");
                    if !v.keys_exact(m, METRIC_KEYS, &mwhere) {
                        continue;
                    }
                    let kind = js(m, "kind");
                    if kind != "log_ratio" && kind != "raw_diff" {
                        v.errs.push(format!("{mwhere}: unknown kind"));
                    }
                    let predicate = js(m, "predicate");
                    if !["outside", "inside", "raw_direction"].contains(&predicate) {
                        v.errs.push(format!("{mwhere}: unknown predicate"));
                    }
                    let refused = m["refused"] == Value::Bool(true);
                    if refused {
                        if METRIC_NULLABLE.iter().any(|k| !m[*k].is_null()) {
                            v.errs
                                .push(format!("{mwhere}: refused metric carries non-null fields"));
                        }
                        continue;
                    }
                    if (predicate == "outside" || predicate == "inside") && kind != "log_ratio" {
                        v.errs
                            .push(format!("{mwhere}: band predicate on non-log_ratio kind"));
                    }
                    if m["point"].is_null()
                        || m["se"].is_null()
                        || m["seed_rule_pass"].is_null()
                        || m["fold_rule_pass"].is_null()
                    {
                        v.errs
                            .push(format!("{mwhere}: non-refused metric missing required evidence"));
                    }
                    let env_field = if predicate == "inside" {
                        "interval_inside_band"
                    } else {
                        "envelope_excludes_edge"
                    };
                    if complete {
                        if m["interval_low"].is_null()
                            || m["interval_high"].is_null()
                            || m[env_field].is_null()
                        {
                            v.errs
                                .push(format!("{mwhere}: complete family with null envelope fields"));
                        }
                    } else if !m["interval_low"].is_null()
                        || !m["interval_high"].is_null()
                        || !m["envelope_excludes_edge"].is_null()
                        || !m["interval_inside_band"].is_null()
                    {
                        v.errs.push(format!(
                            "{mwhere}: incomplete family with non-null envelope fields"
                        ));
                    }
                    if predicate == "raw_direction" {
                        if kind != "raw_diff" {
                            v.errs.push(format!("{mwhere}: raw_direction on non-raw_diff kind"));
                        }
                        if !m["band_low"].is_null() || !m["band_high"].is_null() {
                            v.errs.push(format!("{mwhere}: raw_direction carries a band"));
                        }
                    } else if m["band_low"].is_null() || m["band_high"].is_null() {
                        v.errs.push(format!("{mwhere}: band predicate without a band"));
                    }
                    if predicate == "outside" || predicate == "raw_direction" {
                        if !m["interval_inside_band"].is_null() || !m["seed_inside_count"].is_null()
                        {
                            v.errs
                                .push(format!("{mwhere}: inside-only evidence on an outside metric"));
                        }
                        if m["outside_band"].is_null() || m["seed_same_side_count"].is_null() {
                            v.errs.push(format!("{mwhere}: outside metric missing its evidence"));
                        }
                    }
                    if predicate == "inside" {
                        if !m["outside_band"].is_null() || !m["seed_same_side_count"].is_null() {
                            v.errs
                                .push(format!("{mwhere}: outside-only evidence on an inside metric"));
                        }
                        if m["seed_inside_count"].is_null() {
                            v.errs.push(format!("{mwhere}: inside metric missing its evidence"));
                        }
                    }
                }
            }
        }
    }

    let ladder = &artifact["ladder"];
    if v.keys_exact(ladder, &["rungs", "eligible", "selected", "verdict"], "ladder") {
        rungs = ja(ladder, "rungs").to_vec();
        let names: Vec<&str> = rungs
            .iter()
            .map(|r| r.get("name").and_then(Value::as_str).unwrap_or(""))
            .collect();
        if names != MEASURE12A_FAMILIES.as_slice() {
            v.errs
                .push("ladder: rungs not the six frozen names in ladder order".to_string());
        }
        for r in &rungs {
            let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
            let where_ = format!("rung[{name}]");
            if !v.keys_exact(r, RUNG_KEYS, &where_) {
                continue;
            }
            if let Some(want) = rung_subchecks(name) {
                let got: BTreeSet<&str> = r["subchecks"]
                    .as_object()
                    .map(|m| m.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                let want_set: BTreeSet<&str> = want.iter().copied().collect();
                if got != want_set {
                    v.errs
                        .push(format!("{where_}: subcheck keys not the frozen literal set"));
                }
            }
            for rr in ja(r, "refusals") {
                v.refusal_rec(rr, &format!("{where_}.refusals"));
                scoped_refusals.push(rr.clone());
            }
            let fired = r["fired"] == Value::Bool(true);
            if !fired && (!r["uniform_eligible"].is_null() || !r["required_resolution"].is_null()) {
                v.errs
                    .push(format!("{where_}: resolution fields non-null on an unfired rung"));
            }
        }
        let verdict = js(ladder, "verdict");
        if verdict != "family-eligible" && verdict != "no-family-eligible" {
            v.errs.push("ladder: unknown verdict".to_string());
        }
    }

    v.keys_exact(
        &artifact["cost"],
        &[
            "observed_s",
            "generated_s",
            "bootstrap_s",
            "total_s",
            "peak_rss_bytes",
            "scratch_bytes",
        ],
        "cost",
    );

    let diagnostics = &artifact["diagnostics"];
    let mut top_keys: BTreeSet<(String, String, String)> = BTreeSet::new();
    if v.keys_exact(
        diagnostics,
        &["warmup_exclusions", "refused_cells", "empty_bins", "worsening_23"],
        "diagnostics",
    ) {
        if let Some(map) = diagnostics["warmup_exclusions"].as_object() {
            for k in map.keys() {
                let ok = k.parse::<i64>().is_ok_and(|n| (0..=23).contains(&n))
                    && k.chars().all(|c| c.is_ascii_digit());
                if !ok {
                    v.errs
                        .push(format!("warmup_exclusions: non-integer-hour key {k:?}"));
                }
            }
        }
        let refused_cells = ja(diagnostics, "refused_cells");
        for r in refused_cells {
            v.refusal_rec(r, "refused_cells");
            top_keys.insert((
                r.get("scope").and_then(Value::as_str).unwrap_or("").to_string(),
                r.get("cell").and_then(Value::as_str).unwrap_or("").to_string(),
                r.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
            ));
        }
        if refused_cells.len() != top_keys.len() {
            v.errs
                .push("refused_cells: duplicate logical refusal records".to_string());
        }
        for b in ja(diagnostics, "empty_bins") {
            v.keys_exact(b, &["scope", "cell"], "empty_bins");
        }
        let w23 = &diagnostics["worsening_23"];
        if !w23.is_null() {
            v.keys_exact(w23, &["point", "se", "ucb"], "diagnostics.worsening_23");
        }

        // Refusal ownership in both directions.
        for r in &scoped_refusals {
            let key = (
                r.get("scope").and_then(Value::as_str).unwrap_or("").to_string(),
                r.get("cell").and_then(Value::as_str).unwrap_or("").to_string(),
                r.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
            );
            if !top_keys.contains(&key) {
                v.errs
                    .push(format!("scoped refusal {key:?} missing from refused_cells"));
            }
        }
        let mirrored: BTreeSet<(String, String, String)> = scoped_refusals
            .iter()
            .map(|r| {
                (
                    r.get("scope").and_then(Value::as_str).unwrap_or("").to_string(),
                    r.get("cell").and_then(Value::as_str).unwrap_or("").to_string(),
                    r.get("reason").and_then(Value::as_str).unwrap_or("").to_string(),
                )
            })
            .collect();
        for key in &top_keys {
            let scope = key.0.as_str();
            if !mirrored.contains(key) && !scope.starts_with("family:") && scope != "count_substitution"
            {
                v.errs.push(format!("top-level refusal {key:?} mirrored nowhere"));
            }
        }

        // Amendment E truth table on the reversion rung.
        if let Some(rev) = rungs.iter().find(|r| r.get("name").and_then(Value::as_str) == Some("reversion"))
        {
            let rev_keys: BTreeSet<&str> = rev
                .as_object()
                .map(|m| m.keys().map(String::as_str).collect())
                .unwrap_or_default();
            let want_rung_keys: BTreeSet<&str> = RUNG_KEYS.iter().copied().collect();
            if rev_keys == want_rung_keys {
                let w23_refs: Vec<&Value> = refused_cells
                    .iter()
                    .filter(|r| js(r, "scope") == "reversion" && js(r, "cell") == "worsening_23")
                    .collect();
                let fired = rev["fired"] == Value::Bool(true);
                if !fired {
                    if !diagnostics["worsening_23"].is_null() || !w23_refs.is_empty() {
                        v.errs.push(
                            "Amendment E: unfired reversion rung with a worsening_23 value or refusal"
                                .to_string(),
                        );
                    }
                } else if diagnostics["worsening_23"].is_null() {
                    let ok = rev["uniform_eligible"].is_null()
                        && rev["required_resolution"].is_null()
                        && w23_refs.len() == 1;
                    if !ok {
                        v.errs.push(
                            "Amendment E: refused worsening_23 without null resolution fields \
                             plus exactly one refusal record"
                                .to_string(),
                        );
                    }
                } else {
                    if rev["uniform_eligible"] == Value::Bool(true)
                        && rev["required_resolution"] != Value::String("uniform".into())
                    {
                        v.errs.push(
                            "Amendment E: uniform_eligible true without uniform resolution"
                                .to_string(),
                        );
                    } else if rev["uniform_eligible"] == Value::Bool(false)
                        && rev["required_resolution"] != Value::String("hour-resolved".into())
                    {
                        v.errs.push(
                            "Amendment E: uniform_eligible false without hour-resolved resolution"
                                .to_string(),
                        );
                    } else if rev["uniform_eligible"].is_null() {
                        v.errs.push(
                            "Amendment E: measured worsening_23 without a Boolean uniform_eligible"
                                .to_string(),
                        );
                    }
                    if !w23_refs.is_empty() {
                        v.errs.push(
                            "Amendment E: measured worsening_23 beside a worsening_23 refusal"
                                .to_string(),
                        );
                    }
                }
            }
        }
    }
    v.errs
}

// -- The semantic validator ---------------------------------------------------

const MEASURE12A_BUDGETS_S: &[(&str, f64)] = &[
    ("observed_s", 2.0 * 3600.0),
    ("generated_s", 10.0 * 3600.0),
    ("bootstrap_s", 2.0 * 3600.0),
    ("total_s", 12.0 * 3600.0),
];
const MEASURE12A_RSS_BUDGET: i64 = 4 << 30;
const MEASURE12A_SCRATCH_BUDGET: i64 = 20 << 30;

fn nonfinite_paths(node: &Value, path: &str, limit: usize, out: &mut Vec<String>) {
    if out.len() >= limit {
        return;
    }
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                nonfinite_paths(v, &format!("{path}.{k}"), limit, out);
                if out.len() >= limit {
                    return;
                }
            }
        }
        Value::Array(items) => {
            for (i, v) in items.iter().enumerate() {
                nonfinite_paths(v, &format!("{path}[{i}]"), limit, out);
                if out.len() >= limit {
                    return;
                }
            }
        }
        Value::Number(n) if n.as_f64().is_some_and(|f| !f.is_finite()) => {
            out.push(path.to_string());
        }
        _ => {}
    }
}

/// `measure12a_semantic_errors`: the Brick M semantic gates beyond the
/// exact-key validator - population cardinality, exact metric inventories,
/// monthly reconstruction, ladder coherence, finite numerics and the cost
/// contract. `usable` is the raw preflight usable-session list, taken as
/// `&[Value]` so a mixed-type list (the selftest's negative case) refuses by
/// name rather than by panicking on a type assumption.
#[must_use]
pub fn measure12a_semantic_errors(artifact: &Value, usable: &[Value]) -> Vec<String> {
    let mut errs: Vec<String> = Vec::new();
    let obs = &artifact["observed"];
    let per_session = ja(obs, "per_session");
    let dates: Vec<Option<&str>> = per_session
        .iter()
        .map(|r| r.get("session_date").and_then(Value::as_str))
        .collect();
    if dates.iter().any(Option::is_none) {
        errs.push("observed session dates carry non-string values".to_string());
    } else if usable.iter().any(|d| !d.is_string()) {
        errs.push(format!("the preflight usable list carries non-string entries: {usable:?}"));
    } else {
        let dates: Vec<&str> = dates.into_iter().map(Option::unwrap_or_default).collect();
        let mut usable_sorted: Vec<&str> = usable.iter().filter_map(Value::as_str).collect();
        usable_sorted.sort_unstable();
        let mut dates_sorted_set: Vec<&str> = dates.clone();
        dates_sorted_set.sort_unstable();
        dates_sorted_set.dedup();
        if dates != usable_sorted || dates != dates_sorted_set || dates.len() != 22 {
            errs.push(format!(
                "observed sessions {} do not equal the preflight usable list",
                dates.len()
            ));
        }
    }

    let per_seed = ja(&artifact["generated"], "per_seed");
    let seeds: Vec<Option<i64>> = per_seed.iter().map(|g| strict_i64(&g["seed"])).collect();
    let want_seeds: Vec<Option<i64>> = (1..=8).map(Some).collect();
    if seeds.iter().any(Option::is_none) || seeds != want_seeds {
        errs.push(format!("generated seeds {seeds:?} are not the strict integers 1..8"));
    }

    // Monthly and permutations_monthly must reconstruct exactly from
    // per_session, type-strictly.
    let canon = |x: &Value| typed_canon(&json_safe(x.clone()));
    if let Ok(rebuilt) = blocks_from_sessions(per_session) {
        if canon(&rebuilt) != canon(&obs["monthly"]) {
            errs.push("observed monthly does not reconstruct from per_session".to_string());
        }
    } else {
        errs.push("observed monthly does not reconstruct from per_session".to_string());
    }
    let perms: Vec<&[Value]> = per_session.iter().map(|r| ja(r, "permutations")).collect();
    let rebuilt_perms = aggregate_permutations(&perms);
    if canon(&rebuilt_perms) != canon(&obs["permutations_monthly"]) {
        errs.push("permutations_monthly does not reconstruct from per_session".to_string());
    }

    // Exact family metric inventories, names and order.
    if let Some(seed0) = per_seed.first() {
        let cond = ja(&seed0["count_substitution"], "conditional_adequacy");
        let mut expected: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        {
            let mut names: Vec<String> =
                FAIL_HOURS_300.iter().map(|h| format!("print_excess_h{h}")).collect();
            for h in FAIL_HOURS_300 {
                for w in [60, 300] {
                    names.push(format!("quote_robust_{w}_h{h}"));
                }
            }
            expected.insert("child_walk", names);
        }
        {
            let mut names: Vec<String> =
                FAIL_HOURS_300.iter().map(|h| format!("fano_60_h{h}")).collect();
            names.extend(FAIL_HOURS_300.iter().map(|h| format!("count_p99_60_h{h}")));
            for r in cond {
                if r["required"] == Value::Bool(true) {
                    let hour = r.get("hour").and_then(Value::as_i64).unwrap_or(-1);
                    let bin_name = r.get("bin_name").and_then(Value::as_str).unwrap_or("");
                    names.push(format!("cond_sqrtn_p99_h{hour}_{bin_name}"));
                }
            }
            expected.insert("arrival", names);
        }
        {
            let mut names: Vec<String> =
                FAIL_HOURS_300.iter().map(|h| format!("tail_ratio_h{h}")).collect();
            names.push("tail_ratio_all".to_string());
            expected.insert("innovation", names);
        }
        expected.insert(
            "reversion",
            vec![
                "robust_300_h19".to_string(),
                "robust_300_h20".to_string(),
                "robust_60_h20".to_string(),
                "covnorm_h19".to_string(),
                "covnorm_h20".to_string(),
            ],
        );
        expected.insert(
            "garch",
            vec![
                "robust_300_h19".to_string(),
                "robust_300_h20".to_string(),
                "robust_60_h20".to_string(),
            ],
        );
        {
            let mut names = Vec::new();
            for case in ["pre_halt_close", "post_halt_reopen"] {
                for suffix in ["", "_comparator"] {
                    for stem in ["quote_p99", "robust_60"] {
                        names.push(format!("{stem}_{case}{suffix}"));
                    }
                }
            }
            expected.insert("boundary", names);
        }
        for (fam, names) in &expected {
            let got: Vec<Option<&str>> = ja(&artifact["bootstrap"]["per_family"][fam], "metrics")
                .iter()
                .map(|m| m.get("name").and_then(Value::as_str))
                .collect();
            let want: Vec<Option<&str>> = names.iter().map(|n| Some(n.as_str())).collect();
            if got != want {
                errs.push(format!("family {fam} inventory {got:?} is not the frozen {want:?}"));
            }
        }
    }

    // Ladder coherence.
    let ladder = &artifact["ladder"];
    let rungs = ja(ladder, "rungs");
    let fired: Vec<&str> = rungs
        .iter()
        .filter(|r| r["fired"] == Value::Bool(true))
        .map(|r| r.get("name").and_then(Value::as_str).unwrap_or(""))
        .collect();
    let eligible: Vec<&str> = ja(ladder, "eligible")
        .iter()
        .map(|v| v.as_str().unwrap_or(""))
        .collect();
    if eligible != fired {
        errs.push("eligible does not equal the fired rungs in order".to_string());
    }
    let selected = fired.first().copied();
    let got_selected = ladder.get("selected").and_then(Value::as_str);
    if got_selected != selected {
        errs.push("selected is not the first eligible or null".to_string());
    }
    let verdict = if fired.is_empty() {
        "no-family-eligible"
    } else {
        "family-eligible"
    };
    if js(ladder, "verdict") != verdict {
        errs.push("verdict disagrees with the fired rungs".to_string());
    }

    // Finite numerics.
    let mut nonfinite = Vec::new();
    nonfinite_paths(artifact, "artifact", 8, &mut nonfinite);
    for p in nonfinite {
        errs.push(format!("non-finite value at {p}"));
    }

    // The cost contract.
    let cost = &artifact["cost"];
    for (key, bound) in MEASURE12A_BUDGETS_S {
        let v = strict_f64(&cost[*key]);
        match v {
            Some(x) if x.is_finite() && x >= 0.0 => {
                if x > *bound {
                    errs.push(format!("cost.{key} {x:.1}s breaches the {bound}s budget"));
                }
            }
            _ => errs.push(format!("cost.{key} is not a nonnegative finite number")),
        }
    }
    let phase_sum_ok = MEASURE12A_BUDGETS_S
        .iter()
        .all(|(k, _)| strict_f64(&cost[*k]).is_some())
        && cost["total_s"]
            == serde_json::json!(
                strict_f64(&cost["observed_s"]).unwrap_or(f64::NAN)
                    + strict_f64(&cost["generated_s"]).unwrap_or(f64::NAN)
                    + strict_f64(&cost["bootstrap_s"]).unwrap_or(f64::NAN)
            );
    if !phase_sum_ok {
        errs.push("cost.total_s is not the exact sum of its phases".to_string());
    }
    for (key, bound) in [
        ("peak_rss_bytes", MEASURE12A_RSS_BUDGET),
        ("scratch_bytes", MEASURE12A_SCRATCH_BUDGET),
    ] {
        match strict_i64(&cost[key]) {
            Some(x) if x >= 0 => {
                if x > bound {
                    errs.push(format!("cost.{key} {x} breaches the {bound} budget"));
                }
            }
            _ => errs.push(format!("cost.{key} is not a nonnegative strict integer")),
        }
    }
    errs
}

// -- The Brick G walk cache ---------------------------------------------------

/// `load_brick_g_walks`: a READ-ONLY index of the Brick G walk cache,
/// grouped by the seed embedded in each record. Refuses an absent cache
/// directory, an ambiguous seed, a non-integer/malformed seed field, or a
/// seed set other than exactly 1..8.
pub fn load_brick_g_walks(cache_dir: &Path) -> LabResult<BTreeMap<i64, Value>> {
    if !cache_dir.is_dir() {
        return Err(LabError::refusal(
            "no Brick G walk cache exists; the Brick G walks must land before Brick M runs",
        ));
    }
    let mut by_seed: BTreeMap<i64, Value> = BTreeMap::new();
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(cache_dir)?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path.file_name().and_then(std::ffi::OsStr::to_str).unwrap_or("");
        if !name.ends_with(".json") || name.contains(".tmp") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let record: Value = serde_json::from_str(&text)?;
        if !record.is_object() {
            return Err(LabError::refusal(format!(
                "Brick G cache record {name} is not an object"
            )));
        }
        let seed = record
            .get("seed")
            .and_then(strict_i64)
            .ok_or_else(|| {
                LabError::refusal(format!(
                    "Brick G cache record {name} carries a non-integer seed {:?}",
                    record.get("seed")
                ))
            })?;
        if by_seed.contains_key(&seed) {
            return Err(LabError::refusal(format!(
                "ambiguous Brick G cache: seed {seed} appears in more than one record"
            )));
        }
        by_seed.insert(seed, record);
    }
    let got: Vec<i64> = by_seed.keys().copied().collect();
    if got != (1..=8).collect::<Vec<i64>>() {
        return Err(LabError::refusal(format!(
            "the Brick G cache carries seeds {got:?}, not 1..8"
        )));
    }
    Ok(by_seed)
}

// -- json_safe / write_json_atomic -------------------------------------------

/// `json_safe`: recursively replace non-finite floats with the strings
/// `"nan"`, `"inf"`, `"-inf"` - `serde_json` would otherwise refuse to
/// serialize them (matching the Python's `allow_nan=False`), and a bare
/// `NaN`/`Infinity` token is non-standard JSON a strict consumer would
/// refuse to parse.
#[must_use]
pub fn json_safe(v: Value) -> Value {
    match v {
        Value::Number(ref n) => match n.as_f64() {
            Some(f) if !f.is_finite() => Value::String(
                if f.is_nan() {
                    "nan"
                } else if f > 0.0 {
                    "inf"
                } else {
                    "-inf"
                }
                .to_string(),
            ),
            _ => v,
        },
        Value::Object(map) => Value::Object(map.into_iter().map(|(k, v)| (k, json_safe(v))).collect()),
        Value::Array(items) => Value::Array(items.into_iter().map(json_safe).collect()),
        other => other,
    }
}

/// `write_json_atomic`: `json_safe`, then a `.tmp` write plus rename so a
/// reader never observes a partial file.
pub fn write_json_atomic(path: &Path, payload: &Value) -> LabResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let safe = json_safe(payload.clone());
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = std::path::PathBuf::from(tmp);
    {
        let mut file = std::fs::File::create(&tmp)?;
        use std::io::Write as _;
        serde_json::to_writer_pretty(&file, &safe)?;
        file.write_all(b"\n")?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}
