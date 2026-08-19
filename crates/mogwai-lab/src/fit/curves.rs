// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The protocol-11 hour-curve constructors of `analysis/mnq_fit.py`
//! (spec 4.1-4.6): exposure tables, the shared hour-only normalization,
//! materialization, and the two record shapes a curve rides the artifact in.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use crate::error::{LabError, LabResult};
use crate::kernel::{nearest_rank_list, py_float_repr};
use crate::session::{segment_end_ns, segment_origin_ns};
use crate::subcontract::{
    MIN_60S_CELL_RETURNS, MIN_300S_CELL_RETURNS, MIN_PARENT_CELL_RETURNS, MNQ_DOW_WEIGHT,
    SESSION_ARRAY_DECIMALS,
};

/// `(UTC hour, UTC day-of-week with Sun=0)` - the runtime's own keying,
/// NOT the exchange-local hour the legacy session-curve diagnostics use.
#[must_use]
pub fn utc_hour_dow(ts_ns: i64) -> (usize, usize) {
    let s = ts_ns.div_euclid(1_000_000_000);
    (
        (s.rem_euclid(86_400) / 3_600) as usize,
        (s.div_euclid(86_400) + 4).rem_euclid(7) as usize,
    )
}

/// Calendar-open minutes per `(UTC hour, UTC dow)` cell, summed across the
/// given session labels. Exposure comes from the calendar, never from row
/// presence. 24 x 7, hour-major, Sun=0.
#[must_use]
pub fn exposure_by_hour_dow(sessions: &[String]) -> Vec<Vec<i64>> {
    let mut table = vec![vec![0i64; 7]; 24];
    for label in sessions {
        for segment in ["overnight", "post_halt"] {
            let start = segment_origin_ns(label, segment) as i64;
            let end = segment_end_ns(label, segment) as i64;
            let mut minute = start;
            while minute < end {
                let (hour, dow) = utc_hour_dow(minute);
                table[hour][dow] += 1;
                minute += 60_000_000_000;
            }
        }
    }
    table
}

/// One canonical calendar week's open minutes per `(UTC hour, UTC dow)`:
/// the `W[h,d]` table the conditional intensity normalization sums over
/// (spec 4.3), built from the frozen holiday-free SEARCH week.
#[must_use]
pub fn weekly_exposure_table() -> Vec<Vec<i64>> {
    exposure_by_hour_dow(&[
        "2026-07-06".to_string(),
        "2026-07-07".to_string(),
        "2026-07-08".to_string(),
        "2026-07-09".to_string(),
        "2026-07-10".to_string(),
    ])
}

/// The one-maximum-trimmed mean absolute return (spec 4.1).
#[must_use]
pub fn cell_scale(count: i64, sum_abs: f64, max_abs: f64) -> f64 {
    (sum_abs - max_abs) / ((count - 1) as f64)
}

/// The 23 exposed UTC hours: every hour except 21 (the daily break covers
/// it entirely under the permanent-CDT calendar).
#[must_use]
pub fn exposed_utc_hours() -> Vec<usize> {
    (0..24).filter(|h| *h != 21).collect()
}

/// Open minutes per UTC hour in one full session: 60 everywhere except hour
/// 20 (45, the halt) and hour 21 (0, the break).
#[must_use]
pub fn hour_exposure_weights() -> Vec<i64> {
    (0..24)
        .map(|h| match h {
            21 => 0,
            20 => 45,
            _ => 60,
        })
        .collect()
}

/// The shared hour-only normalization (spec 4.2): divide every exposed
/// value by the open-minute-exposure-weighted mean, summing in ascending
/// UTC-hour order in binary64. Hour 21 is set to exactly 1.0.
///
/// The Python accumulates with `+=` over a sorted dict walk, which is a
/// NAIVE left fold - deliberately not `kernel::py_sum`, because CPython's
/// compensated summation applies to the `sum(...)` builtin alone.
pub fn normalize_hour_curve(raw: &BTreeMap<usize, f64>) -> LabResult<Vec<f64>> {
    let weights = hour_exposure_weights();
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for (h, v) in raw {
        if *h == 21 {
            continue;
        }
        if !v.is_finite() {
            return Err(LabError::refusal(format!(
                "hour curve carries a non-finite value at hour {h}; a required session-refit \
                 value refuses rather than serializing as a string"
            )));
        }
        num += v * (weights[*h] as f64);
        den += weights[*h] as f64;
    }
    let mean = num / den;
    if !(mean.is_finite() && mean > 0.0) {
        return Err(LabError::refusal(
            "hour curve has a nonpositive or non-finite exposure-weighted mean; no real \
             evidence produces this",
        ));
    }
    // Python returns a dict over ALL 24 hours; an unexposed hour absent from
    // `raw` would KeyError, so every caller supplies the exposed 23 and hour
    // 21 is the conventional 1.0.
    Ok((0..24)
        .map(|h| if h == 21 { 1.0 } else { raw[&h] / mean })
        .collect())
}

/// `SESSION_ARRAY_DECIMALS` materialization (spec 4.2): the materialized
/// array, not the unrounded one, is what scratch profiles carry, FINAL
/// gates judge, and the preset ships.
#[must_use]
pub fn materialize_curve(normalized: &[f64]) -> Vec<f64> {
    normalized
        .iter()
        .map(|v| py_format_fixed(*v, SESSION_ARRAY_DECIMALS as usize))
        .collect()
}

/// `float(format(x, ".6f"))`: format at fixed precision, then parse back.
/// Rust's `{:.6}` and CPython's `format(x, '.6f')` both round-half-even off
/// the exact binary value, so the two agree.
#[must_use]
pub fn py_format_fixed(x: f64, decimals: usize) -> f64 {
    format!("{x:.decimals$}")
        .parse::<f64>()
        .expect("a fixed-precision rendering always parses")
}

fn raw_list24(raw: &BTreeMap<usize, f64>) -> Value {
    Value::Array(
        (0..24)
            .map(|h| match raw.get(&h) {
                Some(v) => json!(v),
                None => Value::Null,
            })
            .collect(),
    )
}

fn null_at_21(values: &[f64]) -> Value {
    Value::Array(
        (0..24)
            .map(|h| {
                if h == 21 {
                    Value::Null
                } else {
                    json!(values[h])
                }
            })
            .collect(),
    )
}

/// The raw / normalized_unrounded / materialized record a FITTED curve
/// carries in the artifact (spec Brick H).
pub fn curve_triple(raw: &BTreeMap<usize, f64>) -> LabResult<Value> {
    let normalized = normalize_hour_curve(raw)?;
    Ok(json!({
        "raw": raw_list24(raw),
        "normalized_unrounded": null_at_21(&normalized),
        "materialized": materialize_curve(&normalized),
    }))
}

/// Raw and normalized only - the shape of observed EVIDENCE targets, never
/// installed into a preset and judged unrounded.
pub fn curve_pair(raw: &BTreeMap<usize, f64>) -> LabResult<Value> {
    let normalized = normalize_hour_curve(raw)?;
    Ok(json!({
        "raw": raw_list24(raw),
        "normalized": null_at_21(&normalized),
    }))
}

/// The per-hour robust scale (spec 4.1 steps 4-6): every session must supply
/// a qualifying cell for every exposed hour or the refit REFUSES; the hourly
/// value is the nearest-rank median of cell scales.
pub fn hourly_robust_curve(
    cells: &Value,
    sessions: &[String],
    floor: i64,
    what: &str,
) -> LabResult<BTreeMap<usize, f64>> {
    let mut curve = BTreeMap::new();
    for hour in exposed_utc_hours() {
        let mut scales: Vec<f64> = Vec::new();
        for session in sessions {
            let cell = cells
                .get(session)
                .and_then(|s| s.get(hour.to_string()))
                .filter(|c| !c.is_null());
            let count = cell.and_then(|c| c["count"].as_i64()).unwrap_or(0);
            if cell.is_none() || count < floor {
                return Err(LabError::refusal(format!(
                    "{what}: session {session} hour {hour} has {count} returns against the \
                     floor of {floor}; every session must qualify for every exposed hour"
                )));
            }
            let cell = cell.expect("checked above");
            scales.push(cell_scale(
                count,
                cell["sum_abs"].as_f64().unwrap_or(f64::NAN),
                cell["max_abs"].as_f64().unwrap_or(f64::NAN),
            ));
        }
        scales.sort_by(f64::total_cmp);
        curve.insert(
            hour,
            nearest_rank_list(&scales, 0.5)
                .ok_or_else(|| LabError::refusal("empty list has no quantiles".to_string()))?,
        );
    }
    Ok(curve)
}

/// The `vol_hour` refit (spec 4.2).
pub fn fit_vol_hour(observed: &Value, usable: &[String]) -> LabResult<Value> {
    let raw = hourly_robust_curve(
        &observed["session_refit_raw"]["parent_vol_cells"],
        usable,
        MIN_PARENT_CELL_RETURNS,
        "parent-vol cell",
    )?;
    curve_triple(&raw)
}

/// The conditional intensity refit under frozen `dow_weight` (spec 4.3).
pub fn fit_intensity_hour(observed: &Value, usable: &[String]) -> LabResult<Value> {
    let c_hd = &observed["session_refit_raw"]["parent_count_by_hour_dow"];
    let e_hd = exposure_by_hour_dow(usable);
    let w = MNQ_DOW_WEIGHT;
    let mut q: BTreeMap<usize, f64> = BTreeMap::new();
    let mut marginal_raw: BTreeMap<usize, f64> = BTreeMap::new();
    for hour in exposed_utc_hours() {
        let row = c_hd[hour]
            .as_array()
            .ok_or_else(|| LabError::refusal("parent_count_by_hour_dow is not a 24x7 table"))?;
        // Integer sums: a naive fold is exact, matching the Python's
        // `sum()` over ints.
        let counts: i64 = row.iter().map(|v| v.as_i64().unwrap_or(0)).sum();
        let exposure: i64 = e_hd[hour].iter().sum();
        // `sum(...)` over FLOATS: CPython's compensated Kahan-Babuska-Neumaier
        // summation, not a naive fold (the phase-2b pin). A naive fold here
        // moves the last ulp of every normalized intensity value.
        let weighted: f64 = crate::kernel::py_sum((0..7).map(|d| (e_hd[hour][d] as f64) * w[d]));
        if exposure == 0 || weighted == 0.0 {
            return Err(LabError::refusal(format!(
                "intensity: exposed hour {hour} has no exposure"
            )));
        }
        q.insert(hour, (counts as f64) / weighted);
        marginal_raw.insert(hour, (counts as f64) / (exposure as f64));
    }
    let week = weekly_exposure_table();
    let mut num = 0.0f64;
    let mut den = 0.0f64;
    for (hour, qh) in &q {
        for d in 0..7 {
            num += (week[*hour][d] as f64) * qh * w[d];
            den += week[*hour][d] as f64;
        }
    }
    let z = num / den;
    let conditional: Vec<f64> = (0..24)
        .map(|h| if h == 21 { 1.0 } else { q[&h] / z })
        .collect();
    Ok(json!({
        "raw": raw_list24(&q),
        "normalized_unrounded": null_at_21(&conditional),
        "materialized": (0..24)
            .map(|h| if h == 21 { 1.0 } else {
                py_format_fixed(conditional[h], SESSION_ARRAY_DECIMALS as usize)
            })
            .collect::<Vec<f64>>(),
        // The MARGINAL target the session_arrival gate compares generated
        // marginal rates against - never the conditional array (spec 4.3).
        "marginal_target": curve_pair(&marginal_raw)?,
        "parent_count_by_hour_dow": c_hd.clone(),
        "open_minutes_by_hour_dow": e_hd,
        "dow_weight": w,
    }))
}

/// The observed hourly wall-time robust curves at both horizons (spec 4.6),
/// plus the pooled RMS the pooled gates read.
pub fn observed_walltime_curves(observed: &Value, usable: &[String]) -> LabResult<Value> {
    let cells = &observed["session_refit_raw"]["horizon_cells"];
    let mut out = serde_json::Map::new();
    for h in [60i64, 300] {
        let floor = if h == 60 {
            MIN_60S_CELL_RETURNS
        } else {
            MIN_300S_CELL_RETURNS
        };
        let raw = hourly_robust_curve(
            &cells[h.to_string()],
            usable,
            floor,
            &format!("{h}s horizon cell"),
        )?;
        let pooled = &observed["session_refit_raw"]["walltime_pooled"][h.to_string()];
        out.insert(
            h.to_string(),
            json!({
                "hourly": curve_pair(&raw)?,
                "pooled_rms": pooled["rms"].clone(),
                "return_count": pooled["count"].clone(),
            }),
        );
    }
    Ok(Value::Object(out))
}

/// `f"{x}"` for the tolerance-record strings the artifact carries.
#[must_use]
pub fn py_repr(x: f64) -> String {
    py_float_repr(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Selftest: "a cell more than half zeros yields the trimmed-mean scale".
    #[test]
    fn cell_scale_is_the_one_maximum_trimmed_mean() {
        let s = cell_scale(1000, 40.0, 0.5);
        assert!((s - 39.5 / 999.0).abs() < 1e-15);
        assert!(s > 0.0);
    }

    /// Selftest: "the hour normalization centers the exposure-weighted mean
    /// on one" and hour 21 is exactly 1.0.
    ///
    /// THE RAW CURVE IS NOT FLAT, and that is the whole test. Under a flat
    /// input every exposed hour normalizes to exactly 1.0 whatever the
    /// weighting scheme is - any weighted average of a constant is that
    /// constant, so the weights cancel identically and the assertion pins
    /// nothing. Hour 20 carries 45 open minutes against every other exposed
    /// hour's 60, so putting the outlier THERE is what makes the weights
    /// observable: the weighted mean and the plain mean over 23 hours differ,
    /// and only one of them centers this curve.
    #[test]
    fn the_hour_normalization_centers_the_exposure_weighted_mean_on_one() {
        let raw: BTreeMap<usize, f64> = (0..24)
            .map(|h| (h, if h == 20 { 100.0 } else { 1.0 }))
            .collect();
        let norm = normalize_hour_curve(&raw).unwrap();

        // Written out rather than recomputed from `hour_exposure_weights`,
        // which would pin the normalization against its own weight table:
        // 22 hours at 60 minutes plus hour 20's 45, hour 21 unexposed.
        let weighted_den: f64 = 22.0 * 60.0 + 45.0;
        let weighted_mean: f64 = (22.0 * 60.0 * 1.0 + 45.0 * 100.0) / weighted_den;
        assert!((weighted_mean - 4.263_736_263_736_264).abs() < 1e-12);
        for h in exposed_utc_hours() {
            let expected = raw[&h] / weighted_mean;
            assert!(
                (norm[h] - expected).abs() < 1e-12,
                "hour {h}: {} is not {expected}",
                norm[h]
            );
        }
        assert_eq!(norm[21], 1.0);

        // The property the name claims, stated directly: the EXPOSURE-weighted
        // mean of the normalized curve is one.
        let exposed = exposed_utc_hours();
        let centered = exposed
            .iter()
            .map(|&h| norm[h] * if h == 20 { 45.0 } else { 60.0 })
            .sum::<f64>()
            / weighted_den;
        assert!((centered - 1.0).abs() < 1e-12);

        // And the sensitivity that makes the two schemes distinguishable: an
        // UNWEIGHTED mean over the same 23 hours is emphatically not one, so a
        // normalization that dropped the weights would fail above rather than
        // pass for free.
        let unweighted = exposed.iter().map(|&h| norm[h]).sum::<f64>() / (exposed.len() as f64);
        assert!(
            (unweighted - 1.0).abs() > 0.2,
            "the fixture must separate the weighted mean from the plain one, got {unweighted}"
        );
    }

    /// Selftest: "materialization is idempotent".
    ///
    /// Non-flat for the same reason: a curve that is already exact at
    /// `SESSION_ARRAY_DECIMALS` is a fixed point of any rounding, so a flat
    /// input tests only that the second pass leaves an integer alone. These
    /// values do not survive six decimals untouched - the first pass MOVES
    /// them - and idempotence is then a claim about the rounding.
    #[test]
    fn materialization_is_idempotent() {
        let raw: BTreeMap<usize, f64> = (0..24)
            .map(|h| (h, 1.0 + (h as f64) * std::f64::consts::PI / 7.0))
            .collect();
        let normalized = normalize_hour_curve(&raw).unwrap();
        let mat = materialize_curve(&normalized);
        assert_eq!(materialize_curve(&mat), mat);
        // The first pass has to be doing work, or the claim is vacuous.
        assert!(
            normalized
                .iter()
                .zip(&mat)
                .any(|(before, after)| before != after),
            "the fixture rounds to itself, so idempotence says nothing here"
        );
    }

    /// Selftest: a sub-floor cell refuses by name.
    #[test]
    fn a_sub_floor_cell_refuses() {
        let mut cells = serde_json::Map::new();
        for s in ["2026-07-06", "2026-07-07"] {
            let mut per_hour = serde_json::Map::new();
            for h in exposed_utc_hours() {
                per_hour.insert(
                    h.to_string(),
                    json!({"count": 1000, "sum_abs": 10.0, "max_abs": 0.1}),
                );
            }
            cells.insert(s.to_string(), Value::Object(per_hour));
        }
        let sessions = vec!["2026-07-06".to_string(), "2026-07-07".to_string()];
        let ok = hourly_robust_curve(&Value::Object(cells.clone()), &sessions, 1000, "x").unwrap();
        assert_eq!(ok.len(), 23);
        let thin = hourly_robust_curve(&Value::Object(cells), &sessions, 1001, "parent-vol cell");
        assert!(thin.is_err());
    }

    /// The exposure table is the calendar's, never row presence: one July
    /// session carries 1380 open minutes (23 hours minus the 15-minute
    /// halt).
    #[test]
    fn one_session_exposes_the_calendar_open_minutes() {
        let table = exposure_by_hour_dow(&["2026-07-06".to_string()]);
        let total: i64 = table.iter().flat_map(|r| r.iter()).sum();
        // overnight 17:00 -> 15:15 is 1335 minutes, post_halt 15:30 -> 16:00
        // is 30: the daily break and the halt carry no exposure.
        assert_eq!(total, 1335 + 30);
        assert_eq!(table[21].iter().sum::<i64>(), 0);
    }
}
