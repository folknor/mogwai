// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `analysis/check_cadence_feasible.py`'s L0 structural-proceed verdict.
//!
//! Ported in full: [`next_count`] (the parent/child geometric-mixture
//! inverse-CDF draw), [`verdict`] (the structural PROCEED/CLOSE/STOP AND
//! ASK threshold read directly off `cadence.json`'s measured
//! `children_mean`/`children_single_frac`) and [`density_passes`] (the
//! per-second density feasibility bands).
//!
//! NOT ported: the default (no-flag) CLI path's 3,000,000-event Markov
//! density RE-simulation (`simulate_markov`), which draws from
//! `random.Random(42)` via `weibullvariate` - reproducing it bit-exactly
//! would need a from-scratch port of CPython's Mersenne Twister and
//! `random.weibullvariate`, out of this slice's scope. The gate this module
//! supports is therefore the deterministic structural verdict, which is what
//! `notes/rust-rewrite-phases.md` phase 3a calls binding for this script;
//! the stochastic density recheck is a secondary diagnostic in the Python
//! (it degrades to `SystemExit` on failure, never on the structural verdict
//! alone) and is flagged in the phase landing record as a follow-up.

use serde_json::Value;

use crate::error::{LabError, LabResult};

/// `next_count`: draw a truncated (cap 4096) parent-child count from the
/// geometric-mixture inverse CDF. `uniform` must yield draws in `[0, 1)`.
#[must_use]
pub fn next_count(uniform: &mut impl FnMut() -> f64, q: f64, m: f64) -> (i64, bool) {
    next_count_capped(uniform, q, m, 4096)
}

/// `next_count` with an explicit cap, matching the Python's default
/// parameter.
#[must_use]
pub fn next_count_capped(
    uniform: &mut impl FnMut() -> f64,
    q: f64,
    m: f64,
    cap: i64,
) -> (i64, bool) {
    if uniform() < q {
        return (1, false);
    }
    let u = uniform();
    #[expect(
        clippy::cast_possible_truncation,
        reason = "floor() of a log-ratio, matching the Python's math.floor"
    )]
    let value = 1 + ((-u).ln_1p() / (-1.0 / m).ln_1p()).floor() as i64;
    (value.min(cap), value > cap)
}

/// Reads a required numeric field, refusing rather than defaulting.
///
/// The Python indexes these dictionaries directly, so a missing or
/// non-numeric field raises `KeyError`/`TypeError` and the command stops.
/// Substituting a default here would be silent acceptance of malformed input
/// under the parity contract in `reference/architecture.md` - the class that
/// manufactures an answer instead of refusing.
fn required(node: &Value, path: &[&str]) -> LabResult<f64> {
    let mut cursor = node;
    for key in path {
        cursor = cursor.get(*key).ok_or_else(|| {
            LabError::refusal(format!(
                "cadence measurement is missing `{}`; the Python raises KeyError here rather \
                 than reading it as zero",
                path.join(".")
            ))
        })?;
    }
    cursor.as_f64().ok_or_else(|| {
        LabError::refusal(format!(
            "cadence measurement field `{}` is not a number",
            path.join(".")
        ))
    })
}

/// `density_passes`: the per-second count density feasibility bands a
/// simulated cadence must clear against the measured `per_second_counts`.
///
/// # Errors
/// [`LabError::Refusal`] if either side is missing a required band field or
/// carries a non-numeric one. Note that defaulting was not merely imprecise
/// here: a missing `measured.mean` became zero and the first band then
/// divided by it.
pub fn density_passes(measured: &Value, realized: &Value) -> LabResult<bool> {
    let m = |k: &str| required(measured, &[k]);
    let r = |k: &str| required(realized, &[k]);
    Ok((r("mean")? / m("mean")? - 1.0).abs() <= 0.10
        && m("median")? - 1.0 <= r("median")?
        && r("median")? <= m("median")? + 1.0
        && 0.5 * m("p95")? <= r("p95")?
        && r("p95")? <= 2.0 * m("p95")?
        && (r("zero_frac")? - m("zero_frac")?).abs() <= 0.05)
}

/// `verdict`: the structural L0 proceed/close/stop verdict, read directly
/// off `cadence.json`'s `targets.children_mean`/`children_single_frac`
/// anchors.
///
/// # Errors
/// [`LabError::Refusal`] if either anchor is absent or non-numeric. This
/// previously read a missing anchor as zero, which let a document carrying
/// `children_mean` but no `children_single_frac` return CLOSE, and one
/// carrying neither return CLOSE as well - a verdict fabricated from input
/// the Python rejects outright.
pub fn verdict(cadence: &Value) -> LabResult<&'static str> {
    let mean = required(cadence, &["targets", "children_mean", "anchor"])?;
    let single = required(cadence, &["targets", "children_single_frac", "anchor"])?;
    Ok(if (3.0..=20.0).contains(&mean) && single < 0.90 {
        "PROCEED"
    } else if mean < 1.5 {
        "CLOSE"
    } else {
        "STOP AND ASK"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE CASE THAT PASSED OPEN. A document carrying `children_mean` but no
    /// `children_single_frac` used to read the missing anchor as zero, so
    /// `single < 0.90` held and the malformed measurement returned PROCEED.
    /// The Python raises `KeyError` on the same input.
    #[test]
    fn a_measurement_missing_an_anchor_refuses_rather_than_proceeding() {
        let malformed = serde_json::json!({
            "targets": {"children_mean": {"anchor": 3.0}}
        });
        let err = verdict(&malformed).expect_err("a missing anchor must refuse");
        assert!(err.to_string().contains("children_single_frac"), "{err}");
    }

    #[test]
    fn a_non_numeric_anchor_refuses() {
        let malformed = serde_json::json!({
            "targets": {
                "children_mean": {"anchor": "three"},
                "children_single_frac": {"anchor": 0.5},
            }
        });
        let err = verdict(&malformed).expect_err("a non-numeric anchor must refuse");
        assert!(err.to_string().contains("not a number"), "{err}");
    }

    #[test]
    fn a_well_formed_measurement_still_reaches_its_verdict() {
        let good = serde_json::json!({
            "targets": {
                "children_mean": {"anchor": 8.49},
                "children_single_frac": {"anchor": 0.5586},
            }
        });
        assert_eq!(verdict(&good).expect("well formed"), "PROCEED");
    }

    /// `density_passes` defaulted every band field too, and a missing
    /// `measured.mean` then divided by zero instead of reporting an invalid
    /// artifact.
    #[test]
    fn density_bands_refuse_a_measurement_missing_a_field() {
        let measured = serde_json::json!({"median": 4.0, "p95": 20.0, "zero_frac": 0.1});
        let realized =
            serde_json::json!({"mean": 3.0, "median": 4.0, "p95": 20.0, "zero_frac": 0.1});
        let err = density_passes(&measured, &realized).expect_err("a missing band must refuse");
        assert!(err.to_string().contains("mean"), "{err}");
    }

    #[test]
    fn geometric_sampler_uses_the_pinned_inverse_cdf() {
        let mut values = [0.9, 0.0, 0.9, 0.5].into_iter();
        let mut uniform = move || values.next().unwrap();
        assert_eq!(next_count(&mut uniform, 0.5, 4.0).0, 1);
        assert_eq!(next_count(&mut uniform, 0.5, 4.0).0, 3);
    }
}
