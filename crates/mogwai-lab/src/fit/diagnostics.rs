// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `build_diagnostics` from the retired Python fit implementation (spec 4.8): shared-shape
//! observed values beside the crypto-fitted reference they are compared
//! against, plus the exposure-normalized count-vs-volume session curves.
//! Findings, never gates.

use serde_json::{Value, json};

use crate::fit::observe::Acf;
use crate::subcontract::ACF_LAGS;

/// The crypto-fitted reference values the shared-shape diagnostic compares
/// against: the committed fingerprint's price-sequence anchors and the
/// Binance three-pair cadence anchors, as recorded in the purchase report.
#[must_use]
pub fn reference_shape() -> Value {
    json!({
        "return_acf": {"1": -0.19697},
        "abs_return_acf": {"1": 0.30741, "10": 0.15649, "50": 0.12252},
        "zero_change_frac": 0.47376,
        "duration_dispersion_cv2": 4.6188,
        "duration_acf": {"1": 0.32204, "5": 0.22388},
    })
}

/// Open-minute exposure per exchange-local hour for one full session: every
/// open hour carries 60 minutes except local hour 15 (the 15:15-15:30 halt
/// leaves 45) and hour 16 (the daily break, zero).
#[must_use]
pub fn open_minutes_by_local_hour() -> [i64; 24] {
    std::array::from_fn(|h| match h {
        16 => 0,
        15 => 45,
        _ => 60,
    })
}

/// `diffs`: observed minus reference, recursing through the reference's own
/// shape and yielding null wherever the observed value is not a finite
/// float. The Python's `obs.get(k, nan)` for a missing key lands here as a
/// NaN that fails the finite test, so the null is reached the same way.
fn diffs(obs: &Value, reference: &Value) -> Value {
    if let Some(map) = reference.as_object() {
        return Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), diffs(obs.get(k).unwrap_or(&Value::Null), v)))
                .collect(),
        );
    }
    match (obs.as_f64(), reference.as_f64()) {
        (Some(o), Some(r)) if o.is_finite() => json!(o - r),
        _ => Value::Null,
    }
}

/// The wave-1 convention: zero-rate open hours stay in - an empty open hour
/// widens its own denominator, and a zero trough reads as infinity rather
/// than being silently dropped.
fn peak_to_trough(rates: &serde_json::Map<String, Value>) -> f64 {
    let values: Vec<f64> = rates.values().filter_map(Value::as_f64).collect();
    if values.is_empty() {
        return f64::NAN;
    }
    let trough = values.iter().copied().fold(f64::INFINITY, f64::min);
    if trough == 0.0 {
        return f64::INFINITY;
    }
    values.iter().copied().fold(f64::NEG_INFINITY, f64::max) / trough
}

#[allow(
    clippy::too_many_arguments,
    reason = "the Python passes exactly these nine accumulators; bundling them \
              into a struct would invent a shape the original does not have"
)]
#[must_use]
pub fn build_diagnostics(
    zero_changes: i64,
    price_changes: i64,
    ret_acf: &Acf,
    absret_acf: &Acf,
    dur_acf: &Acf,
    cv2: f64,
    hour_count: &[i64; 24],
    hour_volume: &[i64; 24],
    usable_count: i64,
) -> Value {
    let acf_map = |acf: &Acf| -> Value {
        Value::Object(
            ACF_LAGS
                .iter()
                .map(|lag| (lag.to_string(), json!(acf.value(*lag as usize))))
                .collect(),
        )
    };
    let observed_shape = json!({
        "zero_change_frac": if price_changes > 0 {
            zero_changes as f64 / price_changes as f64
        } else { f64::NAN },
        "return_acf": acf_map(ret_acf),
        "abs_return_acf": acf_map(absret_acf),
        "duration_acf": {"1": dur_acf.value(1), "5": dur_acf.value(5)},
        "duration_dispersion_cv2": cv2,
    });

    let exposure_min = open_minutes_by_local_hour();
    let mut count_rate = serde_json::Map::new();
    let mut volume_rate = serde_json::Map::new();
    for h in 0..24usize {
        let minutes = exposure_min[h] * usable_count;
        if minutes == 0 {
            continue;
        }
        count_rate.insert(h.to_string(), json!(hour_count[h] as f64 / minutes as f64));
        volume_rate.insert(h.to_string(), json!(hour_volume[h] as f64 / minutes as f64));
    }
    let count_ptt = peak_to_trough(&count_rate);
    let volume_ptt = peak_to_trough(&volume_rate);

    let reference = reference_shape();
    let difference = Value::Object(
        reference
            .as_object()
            .expect("an object literal")
            .iter()
            .map(|(k, v)| (k.clone(), diffs(&observed_shape[k], v)))
            .collect(),
    );

    json!({
        "shared_shape": {
            "observed": observed_shape,
            "reference": reference,
            "difference": difference,
        },
        "count_vs_volume": {
            "count_per_open_minute_by_local_hour": Value::Object(count_rate),
            "volume_per_open_minute_by_local_hour": Value::Object(volume_rate),
            "count_peak_to_trough": count_ptt,
            "volume_peak_to_trough": volume_ptt,
            "volume_over_count_ptt_ratio": if count_ptt.is_finite() && volume_ptt.is_finite()
                && count_ptt > 0.0 { volume_ptt / count_ptt } else { f64::NAN },
            "wave1_ten_session_reference": 0.95,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hour_exposure_leaves_the_halt_and_the_break_out() {
        let m = open_minutes_by_local_hour();
        assert_eq!(m[15], 45);
        assert_eq!(m[16], 0);
        assert_eq!(m[0], 60);
    }

    #[test]
    fn a_zero_trough_reads_as_infinity_rather_than_being_dropped() {
        let mut rates = serde_json::Map::new();
        rates.insert("0".into(), json!(0.0));
        rates.insert("1".into(), json!(2.0));
        assert!(peak_to_trough(&rates).is_infinite());
    }

    #[test]
    fn the_difference_block_keys_off_the_reference_shape() {
        let obs = json!({"zero_change_frac": 0.5});
        let d = diffs(&obs["zero_change_frac"], &json!(0.47376));
        assert!((d.as_f64().unwrap() - (0.5 - 0.47376)).abs() < 1e-15);
    }
}
