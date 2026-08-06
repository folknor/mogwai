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

/// `density_passes`: the per-second count density feasibility bands a
/// simulated cadence must clear against the measured `per_second_counts`.
#[must_use]
pub fn density_passes(measured: &Value, realized: &Value) -> bool {
    let m = |k: &str| measured[k].as_f64().unwrap_or(0.0);
    let r = |k: &str| realized[k].as_f64().unwrap_or(0.0);
    (r("mean") / m("mean") - 1.0).abs() <= 0.10
        && m("median") - 1.0 <= r("median")
        && r("median") <= m("median") + 1.0
        && 0.5 * m("p95") <= r("p95")
        && r("p95") <= 2.0 * m("p95")
        && (r("zero_frac") - m("zero_frac")).abs() <= 0.05
}

/// `verdict`: the structural L0 proceed/close/stop verdict, read directly
/// off `cadence.json`'s `targets.children_mean`/`children_single_frac`
/// anchors.
#[must_use]
pub fn verdict(cadence: &Value) -> &'static str {
    let mean = cadence["targets"]["children_mean"]["anchor"]
        .as_f64()
        .unwrap_or(0.0);
    let single = cadence["targets"]["children_single_frac"]["anchor"]
        .as_f64()
        .unwrap_or(0.0);
    if (3.0..=20.0).contains(&mean) && single < 0.90 {
        "PROCEED"
    } else if mean < 1.5 {
        "CLOSE"
    } else {
        "STOP AND ASK"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometric_sampler_uses_the_pinned_inverse_cdf() {
        let mut values = [0.9, 0.0, 0.9, 0.5].into_iter();
        let mut uniform = move || values.next().unwrap();
        assert_eq!(next_count(&mut uniform, 0.5, 4.0).0, 1);
        assert_eq!(next_count(&mut uniform, 0.5, 4.0).0, 3);
    }
}
