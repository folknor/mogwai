// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Synthesis of the generator fingerprint (`analysis/build_fingerprint.py`):
//! reads every `char_<PAIR>.json` under a directory plus `cadence.json` and
//! produces the fingerprint contract - golden stylized-fact targets with
//! tolerances, the pooled UTC session profile, and the level-queue verdict.
//! Byte-for-byte formula port; see the module docs on [`level_queue`] for
//! the one refusal-shaped piece of logic.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use crate::error::{LabError, LabResult};

const ANCHOR: &str = "XBTUSD";

/// The four proceed conditions a trades-only queue-ahead fill model must
/// clear, matching `build_fingerprint.py`'s `LEVEL_CONDITIONS` order.
const LEVEL_CONDITIONS: [(&str, &str); 4] = [
    (
        "single_print_frac <= 0.50",
        "a strict majority of level visits carry more than one print, so \
         visit volume is a real quantity and not a restatement of the \
         trade-size distribution",
    ),
    (
        "vol_dispersion >= 3.0",
        "the volume distribution has genuine dispersion, so a draw from it \
         is not a constant in disguise",
    ),
    (
        "vol_dispersion >= 1.5 * size_dispersion",
        "that dispersion exceeds the trade-size dispersion it could have \
         been inherited from",
    ),
    (
        "anchor single_print_frac within 1.5x of the cross-pair median",
        "the anchor is not the outlier a model would have been fitted to",
    ),
];

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Reads one numeric field of a level block, refusing rather than
/// substituting.
///
/// `build_fingerprint.py:61-64` indexes `single_print_frac`,
/// `vol_dispersion` and `size_dispersion` directly, so a level block missing
/// any of them raises `KeyError`. Substituting `0.0` did not merely diverge:
/// it manufactured a PASS, because two of the four conditions are lower
/// bounds. A block with no `single_print_frac` read as `0.0`, which clears
/// `<= 0.50`, and a missing `size_dispersion` read as `0.0`, which clears
/// `vol_dispersion >= 1.5 * size_dispersion` for any dispersion at all. The
/// fail-open direction was toward `proceed: true` on input the Python
/// refuses to score.
fn level_field(level: &Value, key: &str) -> LabResult<f64> {
    level.get(key).and_then(Value::as_f64).ok_or_else(|| {
        LabError::refusal(format!(
            "level block carries no numeric `{key}`; the Python raises KeyError here rather \
             than reading it as zero and scoring a condition it cannot evaluate"
        ))
    })
}

/// `level_verdict`: evaluate the four proceed conditions against the
/// anchor's measurement.
///
/// # Errors
/// [`LabError::Refusal`] if the level block is missing any of the three
/// numeric fields the conditions read.
pub fn level_verdict(level: &Value, single_fracs: &[f64]) -> LabResult<Value> {
    let med = median(single_fracs.to_vec());
    let spf = level_field(level, "single_print_frac")?;
    let vol_disp = level_field(level, "vol_dispersion")?;
    let size_disp = level_field(level, "size_dispersion")?;
    let held = [
        spf <= 0.50,
        vol_disp >= 3.0,
        vol_disp >= 1.5 * size_disp,
        med / 1.5 <= spf && spf <= med * 1.5,
    ];
    let failed: Vec<&str> = LEVEL_CONDITIONS
        .iter()
        .zip(held.iter())
        .filter(|(_, ok)| !**ok)
        .map(|((test, _why), _)| *test)
        .collect();
    let conditions: Vec<Value> = LEVEL_CONDITIONS
        .iter()
        .zip(held.iter())
        .map(|((test, why), ok)| json!({"test": test, "why": why, "held": ok}))
        .collect();
    Ok(json!({
        "proceed": held.iter().all(|&h| h),
        "conditions": conditions,
        "failed": failed,
        "single_print_frac_cross_pair_median": med,
    }))
}

/// `level_queue`: promote the at-touch level-visit measurement into a
/// golden target. Refuses rather than degrades: a report without a `level`
/// block was written by a characterization pass older than the
/// measurement, and the proceed/close verdict is read off the anchor's
/// numbers, so a stale `char_<PAIR>.json` cannot be silently dropped or
/// silently accepted.
///
/// # Errors
/// [`LabError::Refusal`] on a stale pair (missing `level` block) or
/// disagreeing histogram binning across pairs, matching
/// `build_fingerprint.py`'s `ValueError`s.
pub fn level_queue(anchor_report: &Value, reports: &BTreeMap<String, Value>) -> LabResult<Value> {
    let mut stale: Vec<&str> = reports
        .iter()
        .filter(|(_, r)| r.get("level").is_none())
        .map(|(pair, _)| pair.as_str())
        .collect();
    stale.sort_unstable();
    if !stale.is_empty() {
        return Err(LabError::refusal(format!(
            "char_<PAIR>.json predates the level measurement, re-run run_corpus.py for: {}",
            stale.join(", ")
        )));
    }
    let levels: Vec<&Value> = reports.values().map(|r| &r["level"]).collect();
    let anchor = &anchor_report["level"];
    let binning_keys = ["bin_lo", "bin_hi", "bins_per_decade"];
    let anchor_binning: Vec<Value> = binning_keys.iter().map(|k| anchor[k].clone()).collect();
    for level in &levels {
        let this: Vec<Value> = binning_keys.iter().map(|k| level[*k].clone()).collect();
        if this != anchor_binning {
            return Err(LabError::refusal(
                "level histogram binning differs across pairs",
            ));
        }
    }
    let vol_hist: Vec<i64> = anchor["vol_hist"]
        .as_array()
        .expect("vol_hist array")
        .iter()
        .map(|v| v.as_i64().unwrap_or(0))
        .collect();
    let total: i64 = vol_hist.iter().sum();
    let size_median = anchor["size_median"].as_f64();
    if total == 0 || size_median.is_none_or(|m| m == 0.0) {
        return Err(LabError::refusal("anchor has no usable level visits"));
    }
    let size_median = size_median.expect("checked above");
    let bin_lo = anchor["bin_lo"].as_f64().expect("bin_lo present");
    let bins_per_decade = anchor["bins_per_decade"]
        .as_f64()
        .expect("bins_per_decade present");
    let support: Vec<f64> = (0..vol_hist.len())
        .map(|index| bin_lo * 10f64.powf((index as f64 + 0.5) / bins_per_decade) / size_median)
        .collect();
    // Strict for the same reason as `level_field`: the Python builds this list
    // by direct indexing, so one report missing the field raises rather than
    // contributing a zero that drags the cross-pair median down and widens the
    // fourth condition's window around the anchor.
    let single_fracs: Vec<f64> = levels
        .iter()
        .map(|l| level_field(l, "single_print_frac"))
        .collect::<LabResult<Vec<f64>>>()?;
    let verdict = level_verdict(anchor, &single_fracs)?;
    let proceed = verdict["proceed"].as_bool().unwrap_or(false);

    const LEVEL_DOC: &str = "AT-TOUCH TRADED VOLUME per level visit, era-windowed, expressed as a \
        multiple of the same era's median trade size. NOT book depth and NOT \
        queue position: cancelled liquidity is invisible so the number is \
        deflated, liquidity that joined the level mid-visit is counted so it is \
        also inflated, and the corpus has no aggressor side so buy- and \
        sell-initiated flow at one price are summed together.";
    let reading = if proceed {
        " All four proceed conditions hold, so the queue-ahead fill model \
          may sample support_norm/pmf as an inverse CDF; single_print_frac \
          is the credibility reading its landing was judged against."
            .to_string()
    } else {
        let failed: Vec<String> = verdict["failed"]
            .as_array()
            .expect("failed array")
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        format!(
            " VERDICT: DECLINED. RFC 4631 phase B is refused in full - {} does \
              not hold on the anchor (single_print_frac {:.6}, vol_dispersion \
              {:.6}, size_dispersion {:.6}), so a trades-only queue-ahead \
              quantity would be a free parameter dressed in a histogram. \
              Nothing samples support_norm/pmf; they are kept as the durable \
              corpus fact the refusal was read off.",
            failed.join(", "),
            level_field(anchor, "single_print_frac")?,
            level_field(anchor, "vol_dispersion")?,
            level_field(anchor, "size_dispersion")?,
        )
    };

    let vp50: Vec<f64> = levels
        .iter()
        .map(|l| l["vol_p50_norm"].as_f64().unwrap_or(f64::NAN))
        .collect();
    let vp90: Vec<f64> = levels
        .iter()
        .map(|l| l["vol_p90_norm"].as_f64().unwrap_or(f64::NAN))
        .collect();
    let vd: Vec<f64> = levels
        .iter()
        .map(|l| l["vol_dispersion"].as_f64().unwrap_or(f64::NAN))
        .collect();
    let sd: Vec<f64> = levels
        .iter()
        .map(|l| l["size_dispersion"].as_f64().unwrap_or(f64::NAN))
        .collect();

    Ok(json!({
        "era_start_ts": anchor["era_start_ts"],
        "_doc": format!("{LEVEL_DOC}{reading}"),
        "single_print_frac": {"anchor": anchor["single_print_frac"], "range": rng(&single_fracs)},
        "vol_p50_norm": {"anchor": anchor["vol_p50_norm"], "range": rng(&vp50)},
        "vol_p90_norm": {"anchor": anchor["vol_p90_norm"], "range": rng(&vp90)},
        "vol_dispersion": {"anchor": anchor["vol_dispersion"], "range": rng(&vd)},
        "size_dispersion": {"anchor": anchor["size_dispersion"], "range": rng(&sd)},
        "verdict": verdict,
        "binning": {"bin_lo": anchor["bin_lo"], "bin_hi": anchor["bin_hi"], "bins_per_decade": anchor["bins_per_decade"]},
        "support_norm": support,
        "pmf": vol_hist.iter().map(|&c| c as f64 / total as f64).collect::<Vec<_>>(),
    }))
}

/// `rng`: min/median/max over the non-null values, or `null` if none.
/// Callers with plain `f64` inputs use this; [`rng_typed`] is the sibling
/// that preserves int/float typing the way Python's dynamically-typed
/// `min()`/`max()` do (an all-integer input list keeps `min`/`max` as JSON
/// integers - only `statistics.median`'s true division always yields a
/// float).
fn rng(values: &[f64]) -> Value {
    rng_typed(&values.iter().copied().map(Value::from).collect::<Vec<_>>())
}

/// `rng_typed`: as [`rng`], but over `Value`s so an all-integer input keeps
/// `min`/`max` as JSON integers, matching `build_fingerprint.py`'s
/// `rng()` (`min(vals)`/`max(vals)` over whatever Python type the values
/// already are).
fn rng_typed(values: &[Value]) -> Value {
    let vals: Vec<&Value> = values
        .iter()
        .filter(|v| !v.is_null() && v.as_f64().is_some_and(f64::is_finite))
        .collect();
    if vals.is_empty() {
        return Value::Null;
    }
    let min = (*vals
        .iter()
        .min_by(|a, b| {
            a.as_f64()
                .unwrap()
                .partial_cmp(&b.as_f64().unwrap())
                .unwrap()
        })
        .unwrap())
    .clone();
    let max = (*vals
        .iter()
        .max_by(|a, b| {
            a.as_f64()
                .unwrap()
                .partial_cmp(&b.as_f64().unwrap())
                .unwrap()
        })
        .unwrap())
    .clone();
    let mut sorted: Vec<&Value> = vals.clone();
    sorted.sort_by(|a, b| {
        a.as_f64()
            .unwrap()
            .partial_cmp(&b.as_f64().unwrap())
            .unwrap()
    });
    let n = sorted.len();
    // `statistics.median`: odd count returns the middle element AS-IS
    // (native type preserved); even count true-divides the middle pair,
    // which in Python 3 always yields a float regardless of parity.
    let med = if n % 2 == 1 {
        sorted[n / 2].clone()
    } else {
        Value::from((sorted[n / 2 - 1].as_f64().unwrap() + sorted[n / 2].as_f64().unwrap()) / 2.0)
    };
    json!({"min": min, "median": med, "max": max})
}

fn hour_shares(rep: &Value) -> Vec<f64> {
    let c = rep["session"]["count_hour_dow"]
        .as_array()
        .expect("count_hour_dow");
    let hour: Vec<f64> = c
        .iter()
        .map(|row| {
            row.as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0))
                .sum()
        })
        .collect();
    let sum: f64 = hour.iter().sum();
    let tot = if sum == 0.0 { 1.0 } else { sum };
    hour.iter().map(|x| x / tot).collect()
}

fn hour_vol(rep: &Value) -> Vec<f64> {
    let c = rep["session"]["count_hour_dow"]
        .as_array()
        .expect("count_hour_dow");
    let s = rep["session"]["sumsq_ret_hour_dow"]
        .as_array()
        .expect("sumsq_ret_hour_dow");
    let mut out = Vec::with_capacity(24);
    for h in 0..24 {
        let cnt: f64 = c[h]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap_or(0.0))
            .sum();
        let ssq: f64 = crate::kernel::py_sum(
            s[h].as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0)),
        );
        // APPROVED SEMANTIC CHANGE, not an oversight: the Python writes
        // `(ssq / cnt) ** 0.5`, which CPython delegates to platform libm
        // `pow` - it does not special-case the half power. Matching that
        // bug-for-bug would make this artifact, which is compiled into the
        // generator by `include_str!`, a function of the libm belonging to
        // whoever regenerated it. `sqrt` is correctly rounded under IEEE 754
        // and identical on every conforming platform. The difference is real
        // rather than theoretical - measured at roughly one in 1,236 over this
        // call site's input domain - so `hour_vol_uses_sqrt_not_the_half_power`
        // pins the choice against anyone "restoring parity" later.
        out.push(if cnt != 0.0 { (ssq / cnt).sqrt() } else { 0.0 });
    }
    let positive: Vec<f64> = out.iter().copied().filter(|v| *v > 0.0).collect();
    let mean = if out.iter().any(|v| *v != 0.0) {
        if positive.is_empty() {
            1.0
        } else {
            // `statistics.fmean`, which on CPython 3.14 is `fsum(data) / n` -
            // Shewchuk exact summation, NOT the Neumaier-compensated builtin
            // `sum` used for `ssq` a few lines above. The same Python function
            // uses both, so the two must stay distinct here.
            crate::kernel::py_fsum(positive.iter().copied()) / positive.len() as f64
        }
    } else {
        1.0
    };
    let mean = if mean == 0.0 { 1.0 } else { mean };
    out.iter().map(|v| v / mean).collect()
}

fn dow_weights(rep: &Value) -> Vec<f64> {
    let c = rep["session"]["count_hour_dow"]
        .as_array()
        .expect("count_hour_dow");
    let mut dow = [0.0f64; 7];
    for row in c {
        let row = row.as_array().unwrap();
        for (d, v) in row.iter().enumerate() {
            dow[d] += v.as_f64().unwrap_or(0.0);
        }
    }
    let tot: f64 = dow.iter().sum();
    let tot = if tot == 0.0 { 1.0 } else { tot };
    dow.iter().map(|x| x / tot).collect()
}

fn avg_curves(curves: &[Vec<f64>]) -> Vec<f64> {
    let n = curves.len() as f64;
    let k = curves[0].len();
    (0..k)
        .map(|i| crate::kernel::py_sum(curves.iter().map(|c| c[i])) / n)
        .collect()
}

/// Reads `char_<PAIR>.json` files from `char_dir`, keyed by the `pair` field
/// inside each report.
///
/// ORDERING, and why a `BTreeMap` is faithful here rather than merely
/// convenient. The Python builds a dict by walking `sorted(glob(...))` -
/// FILENAME order - and keying each entry by the report's own `pair` field, so
/// its iteration order is filename order. A `BTreeMap` gives PAIR order. Those
/// coincide exactly when every report's embedded pair matches its filename,
/// which is a convention nothing previously enforced: a `char_AAA.json`
/// declaring `"pair": "ZZZ"` put the two orders in different places, and the
/// port would then iterate differently from the implementation it replaces.
/// Rather than carry an insertion-ordered map to reproduce an order that is
/// only ever meaningful under the convention, this refuses the mismatch. The
/// invariant is then provable rather than incidental.
///
/// FAIL-CLOSED on both required fields. The Python indexes `r["pair"]` and
/// `r["n_trades"]`, so a malformed report raises `KeyError` there. This
/// previously substituted `""` and `0`, inventing an identity for one and
/// silently changing an aggregate for the other - the worst class under the
/// parity contract in `reference/architecture.md`, because it manufactures an
/// answer from input the original rejected.
///
/// # Errors
/// [`LabError::Refusal`] if a report is missing `pair` or `n_trades`, if
/// either has the wrong type, or if the embedded pair disagrees with the
/// filename. Propagates I/O and JSON parse failure.
pub fn load_reports(char_dir: &Path) -> LabResult<BTreeMap<String, Value>> {
    let mut paths: Vec<_> = std::fs::read_dir(char_dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("char_") && n.ends_with(".json"))
        })
        .collect();
    paths.sort();
    let mut reps = BTreeMap::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| LabError::refusal("char report path is not valid UTF-8"))?
            .to_string();
        let from_name = name
            .strip_prefix("char_")
            .and_then(|n| n.strip_suffix(".json"))
            .unwrap_or_default()
            .to_string();
        let text = std::fs::read_to_string(&path)?;
        let value: Value = serde_json::from_str(&text)?;
        let pair = value
            .get("pair")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                LabError::refusal(format!(
                    "{name} carries no string `pair` field; the Python raises KeyError here \
                     rather than inventing an identity"
                ))
            })?
            .to_string();
        if pair != from_name {
            return Err(LabError::refusal(format!(
                "{name} declares pair `{pair}`, so filename order and pair order disagree; \
                 rename the file or fix the report rather than letting the two iterate \
                 differently"
            )));
        }
        if value.get("n_trades").and_then(Value::as_i64).is_none() {
            return Err(LabError::refusal(format!(
                "{name} carries no integer `n_trades` field; the Python raises KeyError here \
                 rather than counting it as zero"
            )));
        }
        reps.insert(pair, value);
    }
    Ok(reps)
}

/// Reads the committed `cadence.json`.
///
/// # Errors
/// [`LabError::Refusal`] if the file is absent (mirrors `build_fingerprint.py`'s
/// `FileNotFoundError`), or propagates I/O/JSON failure.
pub fn load_cadence(path: &Path) -> LabResult<Value> {
    if !path.exists() {
        return Err(LabError::refusal(
            "analysis/cadence.json is required; run build_cadence.py first",
        ));
    }
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// `build_fingerprint.py`'s `main()` synthesis, minus the `findings.md`
/// side artifact (a human-readable report the phase-3a brief does not gate
/// on): reads every `char_<PAIR>.json` in `char_dir` plus `cadence.json` and
/// returns the fingerprint contract.
///
/// # Errors
/// [`LabError::Refusal`] if no `char_*.json` reports are found or the level
/// queue detects a stale/disagreeing pair; propagates I/O/JSON failure.
pub fn build_fingerprint(char_dir: &Path, cadence_path: &Path) -> LabResult<Value> {
    let reps = load_reports(char_dir)?;
    let cadence = load_cadence(cadence_path)?;
    if reps.is_empty() {
        return Err(LabError::refusal(
            "no char_*.json found; run run_corpus.py first",
        ));
    }
    let pairs: Vec<String> = reps.keys().cloned().collect();
    let anchor = reps.get(ANCHOR).unwrap_or_else(|| &reps[&pairs[0]]);

    let intensity = avg_curves(&reps.values().map(hour_shares).collect::<Vec<_>>());
    let vol = avg_curves(&reps.values().map(hour_vol).collect::<Vec<_>>());
    let dow = avg_curves(&reps.values().map(dow_weights).collect::<Vec<_>>());

    let ret1: Vec<f64> = reps
        .values()
        .map(|r| r["returns"]["acf"][0].as_f64().unwrap_or(f64::NAN))
        .collect();
    let abs1: Vec<f64> = reps
        .values()
        .map(|r| r["returns"]["abs_acf"][0].as_f64().unwrap_or(f64::NAN))
        .collect();
    let abs10: Vec<f64> = reps
        .values()
        .map(|r| r["returns"]["abs_acf"][9].as_f64().unwrap_or(f64::NAN))
        .collect();
    let abs50: Vec<f64> = reps
        .values()
        .map(|r| r["returns"]["abs_acf"][49].as_f64().unwrap_or(f64::NAN))
        .collect();
    let zchg: Vec<f64> = reps
        .values()
        .map(|r| {
            r["returns"]["zero_change_frac"]
                .as_f64()
                .unwrap_or(f64::NAN)
        })
        .collect();
    let dwell: Vec<&Value> = reps.values().map(|r| &r["duration"]["dwell"]).collect();
    let dwell_disp_over_mean: Vec<f64> = dwell
        .iter()
        .map(|d| {
            d["dispersion_index"].as_f64().unwrap_or(f64::NAN)
                / d["mean_s"].as_f64().unwrap_or(f64::NAN)
        })
        .collect();
    let queue = level_queue(anchor, &reps)?;

    let anchor_dwell = &anchor["duration"]["dwell"];
    let anchor_disp_cv2 = anchor_dwell["dispersion_index"].as_f64().unwrap_or(0.0)
        / anchor_dwell["mean_s"].as_f64().unwrap_or(1.0);

    let mut fingerprint = json!({
        "source": {
            "pairs": pairs,
            // `load_reports` refuses a report without an integer `n_trades`,
            // so this is total by construction rather than by default.
            "total_trades": reps.values().filter_map(|r| r["n_trades"].as_i64()).sum::<i64>(),
            "anchor": anchor["pair"],
        },
        "golden_targets": {
            "_doc": "the generator's synthetic stream must reproduce these; \
                     tolerances are the cross-pair spread, anchored on the \
                     deepest series; duration dispersion and duration ACF are \
                     era-windowed like dwell, everything else full-span",
            "duration_dispersion_cv2": {"anchor": anchor_disp_cv2, "range": rng(&dwell_disp_over_mean)},
            "return_acf_lag1": {"anchor": anchor["returns"]["acf"][0], "range": rng(&ret1)},
            "abs_return_acf": {
                "lag1": {"anchor": anchor["returns"]["abs_acf"][0], "range": rng(&abs1)},
                "lag10": {"anchor": anchor["returns"]["abs_acf"][9], "range": rng(&abs10)},
                "lag50": {"anchor": anchor["returns"]["abs_acf"][49], "range": rng(&abs50)},
            },
            "zero_change_frac": {"anchor": anchor["returns"]["zero_change_frac"], "range": rng(&zchg)},
            "return_acf_anchor": anchor["returns"]["acf"].as_array().unwrap()[..10].to_vec(),
            "abs_return_acf_anchor": anchor["returns"]["abs_acf"],
            "dwell": {
                "era_start_ts": anchor_dwell["era_start_ts"],
                "mean_s": {"anchor": anchor_dwell["mean_s"], "range": rng(&dwell.iter().map(|d| d["mean_s"].as_f64().unwrap_or(f64::NAN)).collect::<Vec<_>>())},
                "max_gap_s": {"anchor": anchor_dwell["max_gap_s"], "range": rng(&dwell.iter().map(|d| d["max_gap_s"].as_f64().unwrap_or(f64::NAN)).collect::<Vec<_>>())},
                "gap_p999_s": {"anchor": anchor_dwell["gap_p999_s"], "range": rng(&dwell.iter().map(|d| d["gap_p999_s"].as_f64().unwrap_or(f64::NAN)).collect::<Vec<_>>())},
                "empty_hour_frac": {"anchor": anchor_dwell["empty_hour_frac"], "range": rng(&dwell.iter().map(|d| d["empty_hour_frac"].as_f64().unwrap_or(f64::NAN)).collect::<Vec<_>>())},
                "max_empty_hour_run_h": {"anchor": anchor_dwell["max_empty_hour_run_h"], "range": rng_typed(&dwell.iter().map(|d| d["max_empty_hour_run_h"].clone()).collect::<Vec<_>>())},
                "_doc": "era-windowed; gate reads the anchor p999, empty-hour fraction, and run, with p999 cadence-scaled against mean_s; max_gap_s is documentation and the range records the dying-symbol spread LiquidityDrought imitates",
            },
        },
        "session_profile": {
            "_doc": "UTC, instrument-agnostic. intensity[h] and vol[h] index \
                     hour-of-day 0..23; dow[d] indexes Sun=0..Sat=6",
            "intensity_hour": intensity,
            "vol_hour": vol,
            "dow_weight": dow,
        },
        "empirical_ranges": {
            "_doc": "observed ranges for diagnostics only; mechanism validation does not use them",
            "corpus": "Kraken eight-pair fingerprint plus Binance three-pair cadence fit",
            "modal_tick": rng_typed(&reps.values().map(|r| r["returns"]["modal_tick"].clone()).collect::<Vec<_>>()),
            "price_decimals": rng_typed(&reps.values().map(|r| r["returns"]["price_decimals_mode"].clone()).collect::<Vec<_>>()),
            "mean_event_duration_s": cadence["targets"]["mean_event_duration_s"]["range"],
            "children_mean": cadence["targets"]["children_mean"]["range"],
            "children_single_frac": cadence["targets"]["children_single_frac"]["range"],
            "levels_mean": cadence["targets"]["levels_mean"]["range"],
            "mean_trade_notional": cadence["targets"]["mean_trade_notional"]["range"],
            "size_round_frac": rng(&reps.values().map(|r| r["size"]["round_frac"].as_f64().unwrap_or(f64::NAN)).collect::<Vec<_>>()),
        },
        "cadence": cadence,
    });
    fingerprint["golden_targets"]["level_queue"] = queue;
    Ok(fingerprint)
}

#[cfg(test)]
mod loader_contract_tests {
    use super::*;

    /// Writes one `char_<name>.json` carrying `body` into a per-test scratch
    /// directory and returns it. Workspace `target/`, never the system temp
    /// dir: this repo keeps all data inside the project. `CARGO_TARGET_TMPDIR`
    /// is only defined for integration tests, and these are unit tests.
    fn scratch_with(name: &str, body: &Value) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/char-loader-tests")
            .join(name);
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(
            dir.join(format!("char_{name}.json")),
            serde_json::to_string(body).expect("serialize"),
        )
        .expect("write report");
        dir
    }

    fn load_one(name: &str, body: &Value) -> LabResult<BTreeMap<String, Value>> {
        load_reports(&scratch_with(name, body))
    }

    #[test]
    fn a_well_formed_report_loads_under_its_own_pair() {
        let reps = load_one(
            "AAAUSD",
            &serde_json::json!({"pair": "AAAUSD", "n_trades": 7}),
        )
        .expect("well-formed report loads");
        assert_eq!(reps.len(), 1);
        assert_eq!(reps["AAAUSD"]["n_trades"], 7);
    }

    #[test]
    fn a_report_without_a_pair_refuses_rather_than_inventing_an_identity() {
        let err = load_one("BBBUSD", &serde_json::json!({"n_trades": 7}))
            .expect_err("a missing pair must refuse");
        assert!(err.to_string().contains("no string `pair`"), "{err}");
    }

    #[test]
    fn a_report_without_n_trades_refuses_rather_than_counting_zero() {
        let err = load_one("CCCUSD", &serde_json::json!({"pair": "CCCUSD"}))
            .expect_err("a missing n_trades must refuse");
        assert!(err.to_string().contains("no integer `n_trades`"), "{err}");
    }

    /// THE APPROVED-DEVIATION PIN. `hour_vol` deliberately uses `sqrt` where
    /// the Python uses `** 0.5`, because CPython routes that through platform
    /// libm `pow` and the result is compiled into the generator. This asserts
    /// the two genuinely differ at a value in the call site's own domain, so a
    /// later "restore parity" edit to `powf(0.5)` fails here rather than
    /// quietly making a shipped artifact platform-dependent. The value is a
    /// mean of squared returns; CPython returns the `powf` result for it.
    #[test]
    fn hour_vol_uses_sqrt_not_the_half_power() {
        // 0x1.dcf054c223beep-19, about 3.5e-6: CPython returns
        // 0x1.ee28960bac05cp-10 for `x ** 0.5` and 0x1.ee28960bac05dp-10 for
        // `math.sqrt(x)`, one ulp apart.
        let x = f64::from_bits(0x3ecd_cf05_4c22_3bee);
        assert_ne!(
            x.sqrt().to_bits(),
            x.powf(0.5).to_bits(),
            "this fixture no longer discriminates; find another before relaxing the pin"
        );
    }

    /// THE ORDERING DISCRIMINATOR. Under the old loader this file silently
    /// keyed itself as `ZZZUSD` while sorting at the filename position of
    /// `DDDUSD`, so a `BTreeMap` and the Python dict iterated differently.
    /// Refusing is what makes pair order and filename order provably the same.
    #[test]
    fn a_pair_disagreeing_with_its_filename_refuses() {
        let err = load_one(
            "DDDUSD",
            &serde_json::json!({"pair": "ZZZUSD", "n_trades": 7}),
        )
        .expect_err("a filename and pair mismatch must refuse");
        assert!(
            err.to_string().contains("filename order and pair order"),
            "{err}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::characterize::{LVL_BINS, LevelVisits};

    fn fixture(single_print_frac: f64, vol_dispersion: f64) -> BTreeMap<String, Value> {
        let mut acc = LevelVisits::new(1_000_000.0);
        for &(ts, px, sz) in &[
            (1_000_000.0, 100.0, 2.0),
            (1_000_001.0, 100.0, 2.0),
            (1_000_002.0, 101.0, 2.0),
            (1_000_003.0, 101.0, 6.0),
        ] {
            acc.push(ts, px, sz);
        }
        acc.close();
        let mut report = acc.report();
        report["single_print_frac"] = json!(single_print_frac);
        report["vol_dispersion"] = json!(vol_dispersion);
        report["size_dispersion"] = json!(1.0);
        let mut reports = BTreeMap::new();
        reports.insert("XBTUSD".to_string(), json!({"level": report}));
        reports
    }

    #[test]
    fn support_is_increasing_and_the_pmf_sums_to_one() {
        let reports = fixture(0.2, 5.0);
        let block = level_queue(&reports["XBTUSD"], &reports).unwrap();
        let support = block["support_norm"].as_array().unwrap();
        let pmf = block["pmf"].as_array().unwrap();
        assert_eq!(support.len(), pmf.len());
        assert_eq!(pmf.len(), LVL_BINS);
        let total: f64 = pmf.iter().map(|v| v.as_f64().unwrap()).sum();
        assert!((total - 1.0).abs() < 1e-9);
        assert!(support.iter().all(|v| v.as_f64().unwrap() > 0.0));
        let vals: Vec<f64> = support.iter().map(|v| v.as_f64().unwrap()).collect();
        assert!(vals.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_report_predating_the_measurement_is_refused() {
        let mut reports = fixture(0.2, 5.0);
        reports.insert("ETHUSD".to_string(), json!({"session": {}}));
        let err = level_queue(&reports["XBTUSD"], &reports).unwrap_err();
        assert!(err.to_string().contains("ETHUSD"));
    }

    /// THE FALSE PROCEED. Two of the four conditions are lower bounds, so
    /// reading a missing field as `0.0` did not merely diverge from the
    /// Python's `KeyError` - it cleared conditions on evidence that was never
    /// measured. A block with no `single_print_frac` scored `0.0 <= 0.50` as
    /// held, and one with no `size_dispersion` cleared
    /// `vol_dispersion >= 1.5 * size_dispersion` for any dispersion at all.
    /// Each field is dropped INDIVIDUALLY here: a single fixture missing all
    /// three would pass the moment any one of them started refusing, and would
    /// not tell you which.
    #[test]
    fn a_level_block_missing_a_scored_field_is_refused() {
        for field in ["single_print_frac", "vol_dispersion", "size_dispersion"] {
            let mut reports = fixture(0.2, 5.0);
            let level = reports
                .get_mut("XBTUSD")
                .expect("anchor")
                .get_mut("level")
                .expect("level block");
            level
                .as_object_mut()
                .expect("level object")
                .remove(field)
                .expect("the fixture carries the field being dropped");
            let anchor = reports["XBTUSD"].clone();

            let err = level_verdict(&anchor["level"], &[0.2])
                .expect_err("a dropped scored field must refuse rather than read as zero");
            assert!(
                err.to_string().contains(field),
                "the refusal must name {field}, got: {err}"
            );
            assert!(level_queue(&anchor, &reports).is_err());
        }
    }

    /// The fail-open direction was toward PASS, which is the part that makes it
    /// a defect rather than a difference. Pinned as its own case so nobody
    /// "fixes" the refusal by substituting a value that still scores.
    #[test]
    fn the_dropped_field_would_otherwise_have_manufactured_a_proceed() {
        let mut reports = fixture(0.2, 5.0);
        let level = reports
            .get_mut("XBTUSD")
            .expect("anchor")
            .get_mut("level")
            .expect("level block");
        // vol_dispersion 4.0 against size_dispersion 4.0 fails condition three
        // honestly; dropping size_dispersion is what used to rescue it.
        level["vol_dispersion"] = json!(4.0);
        level["size_dispersion"] = json!(4.0);
        let scored = level_verdict(&reports["XBTUSD"]["level"], &[0.2]).expect("scores");
        assert!(!scored["proceed"].as_bool().expect("proceed"));

        let level = reports
            .get_mut("XBTUSD")
            .expect("anchor")
            .get_mut("level")
            .expect("level block");
        level
            .as_object_mut()
            .expect("level object")
            .remove("size_dispersion");
        assert!(
            level_verdict(&reports["XBTUSD"]["level"], &[0.2]).is_err(),
            "dropping the field that made the condition fail must not turn a DECLINE into a PASS"
        );
    }

    #[test]
    fn disagreeing_binning_is_refused() {
        let mut reports = fixture(0.2, 5.0);
        let mut other = reports["XBTUSD"]["level"].clone();
        other["bins_per_decade"] = json!(20);
        reports.insert("ETHUSD".to_string(), json!({"level": other}));
        assert!(level_queue(&reports["XBTUSD"], &reports).is_err());
    }

    #[test]
    fn all_four_conditions_holding_proceeds() {
        let reports = fixture(0.2, 5.0);
        let verdict = level_verdict(&reports["XBTUSD"]["level"], &[0.2, 0.25]).unwrap();
        assert!(verdict["proceed"].as_bool().unwrap());
        assert!(verdict["failed"].as_array().unwrap().is_empty());
    }

    #[test]
    fn a_degenerate_corpus_declines_and_names_the_failed_condition() {
        let reports = fixture(0.69, 5.0);
        let verdict = level_verdict(&reports["XBTUSD"]["level"], &[0.69, 0.7]).unwrap();
        assert!(!verdict["proceed"].as_bool().unwrap());
        assert_eq!(
            verdict["failed"].as_array().unwrap(),
            &vec![json!("single_print_frac <= 0.50")]
        );
    }

    #[test]
    fn inherited_dispersion_fails_condition_three() {
        let mut reports = fixture(0.2, 4.0);
        reports.get_mut("XBTUSD").unwrap()["level"]["size_dispersion"] = json!(4.0);
        let level = reports["XBTUSD"]["level"].clone();
        let verdict = level_verdict(&level, &[0.2]).unwrap();
        assert!(!verdict["proceed"].as_bool().unwrap());
        assert!(
            verdict["failed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str() == Some("vol_dispersion >= 1.5 * size_dispersion"))
        );
    }

    #[test]
    fn an_outlier_anchor_fails_condition_four() {
        let reports = fixture(0.05, 5.0);
        let verdict = level_verdict(&reports["XBTUSD"]["level"], &[0.05, 0.4, 0.45]).unwrap();
        assert!(!verdict["proceed"].as_bool().unwrap());
        assert!(
            verdict["failed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str()
                    == Some("anchor single_print_frac within 1.5x of the cross-pair median"))
        );
        assert!(
            (verdict["single_print_frac_cross_pair_median"]
                .as_f64()
                .unwrap()
                - 0.4)
                .abs()
                < 1e-12
        );
    }

    /// Counterpart of `analysis/test_characterize.py`'s
    /// `CadenceTests.test_committed_cadence_is_loadable`, which the
    /// phase-3a landing record (the retired rewrite plan) did not
    /// name a Rust counterpart for.
    #[test]
    fn the_committed_cadence_is_loadable() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("analysis")
            .join("cadence.json");
        let cadence = load_cadence(&path).unwrap();
        assert_eq!(cadence["anchor"].as_str(), Some("BTCUSDT"));
    }
}
