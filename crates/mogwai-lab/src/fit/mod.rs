// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The protocol-11 fit: `analysis/mnq_fit.py`'s `fit` mode, ported.
//!
//! Layout mirrors the Python's own sections: `observe` is the one streaming
//! corpus pass with its three chains, `curves` the closed-form refit
//! constructors, `solve` the CRN solve machinery, `walk` the cached
//! `gen --type summary` evaluations, `driver` the `run_fit` orchestration
//! (vol solve, family probes, judge, verdicts, the `session_refit` record),
//! `diagnostics` the 4.8 findings block and `mtrand` CPython's Mersenne
//! Twister for the one resampled envelope.

pub mod curves;
pub mod diagnostics;
pub mod driver;
pub mod mtrand;
pub mod observe;
pub mod solve;
pub mod walk;

use serde_json::{Value, json};

use crate::subcontract::{ARRIVAL_HOUR_REL_TOL, SESSION_HOUR_BAND, WALLTIME_POOLED_REL_TOL};

pub use solve::SLACK;

/// A tolerance bound: a scalar, a multiplicative band, or the data-derived
/// resampled envelope (whose bound is computed at fit time, so the contract
/// records the word rather than a number).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Bound {
    Scalar(f64),
    Band(f64, f64),
    Resampled,
}

/// Representability tolerances, target-local, boundaries inclusive in every
/// class under the shared `SLACK` convention. The protocol-10
/// size/quote/displacement/start-price rows are gone with their solves.
#[allow(
    clippy::match_same_arms,
    reason = "one arm per TOLERANCES row, in the Python's own order; merging \
              rows that happen to share a bound today would hide which \
              targets are independently frozen"
)]
#[must_use]
pub fn tolerance(name: &str) -> (&'static str, Bound) {
    match name {
        "mean_event_duration_s" => ("relative", Bound::Scalar(0.10)),
        "children_mean" => ("relative", Bound::Scalar(0.10)),
        "children_single_frac" => ("absolute", Bound::Scalar(0.05)),
        "levels_mean" => ("relative", Bound::Scalar(0.15)),
        "mid_rms" => ("relative", Bound::Scalar(0.10)),
        "minute_range_p99" | "minute_range_p99.9" | "minute_range_max" => {
            ("envelope_upper", Bound::Resampled)
        }
        "session_arrival_hour" => ("relative", Bound::Scalar(ARRIVAL_HOUR_REL_TOL)),
        "session_vol_hour" | "walltime_hour_60" | "walltime_hour_300" => (
            "band",
            Bound::Band(SESSION_HOUR_BAND.0, SESSION_HOUR_BAND.1),
        ),
        "walltime_pooled_60" | "walltime_pooled_300" => {
            ("relative", Bound::Scalar(WALLTIME_POOLED_REL_TOL))
        }
        other => panic!("unknown tolerance {other}"),
    }
}

/// `list(TOLERANCES[m])` - the two-element record the artifact carries.
#[must_use]
pub fn tolerance_json(name: &str) -> Value {
    let (kind, bound) = tolerance(name);
    match bound {
        Bound::Scalar(x) => json!([kind, x]),
        Bound::Band(lo, hi) => json!([kind, [lo, hi]]),
        Bound::Resampled => json!([kind, "resampled"]),
    }
}

/// `within`: every class inclusive at the boundary under `SLACK`.
#[must_use]
pub fn within(kind: &str, bound: Bound, generated: f64, observed: f64) -> bool {
    match (kind, bound) {
        ("relative", Bound::Scalar(b)) => {
            (generated - observed).abs() <= b * observed.abs() + SLACK
        }
        ("absolute", Bound::Scalar(b)) => (generated - observed).abs() <= b + SLACK,
        ("ceiling", Bound::Scalar(b)) => generated <= b + SLACK,
        // Multiplicative band on generated/observed (spec 4.5), boundaries
        // inclusive under the shared slack convention.
        ("band", Bound::Band(lo, hi)) => {
            lo * observed - SLACK <= generated && generated <= hi * observed + SLACK
        }
        ("exact", _) => generated == observed,
        (k, _) => panic!("unknown tolerance kind {k}"),
    }
}

/// The protocol-11 gate families (spec 4.5), target-local.
pub const FAMILIES: [&str; 4] = [
    "session_arrival",
    "session_parent_vol",
    "session_walltime",
    "base_volatility",
];

/// The landable slots and the judge metrics their verdicts read.
#[must_use]
pub fn targets() -> Vec<(&'static str, &'static str, Vec<&'static str>)> {
    vec![
        (
            "intensity_hour",
            "session_arrival",
            vec!["session_arrival_hour"],
        ),
        ("vol_hour", "session_parent_vol", vec!["session_vol_hour"]),
        (
            "vol_scalar",
            "base_volatility",
            vec![
                "mid_rms",
                "minute_range_p99",
                "minute_range_p99.9",
                "minute_range_max",
                "mean_event_duration_s",
                "children_mean",
                "children_single_frac",
                "levels_mean",
            ],
        ),
    ]
}

/// The cadence four, which ride the `base_volatility` family's verdict so a
/// cadence miss is visible there; cadence separately refuses protocol 11.
pub const CADENCE_NAMES: [&str; 4] = [
    "mean_event_duration_s",
    "children_mean",
    "children_single_frac",
    "levels_mean",
];

/// The minute-range statistics that gate (p99.99 is a diagnostic only).
pub const MINUTE_RANGE_GATES: [&str; 3] = ["p99", "p99.9", "max"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::fit::observe::minute_range_envelope;

    /// The selftest's tolerance-class battery, boundary by boundary.
    #[test]
    fn every_tolerance_class_is_inclusive_at_its_boundary() {
        assert!(within("relative", Bound::Scalar(0.10), 1.1, 1.0));
        assert!(!within("relative", Bound::Scalar(0.10), 1.101, 1.0));
        assert!(within("absolute", Bound::Scalar(0.05), 0.55, 0.5));
        assert!(!within("absolute", Bound::Scalar(0.05), 0.551, 0.5));
        assert!(within("ceiling", Bound::Scalar(0.10), 0.10, f64::NAN));
        assert!(!within("ceiling", Bound::Scalar(0.10), 0.1001, f64::NAN));
        let band = Bound::Band(SESSION_HOUR_BAND.0, SESSION_HOUR_BAND.1);
        assert!(within("band", band, 0.8, 1.0));
        assert!(within("band", band, 1.25, 1.0));
        assert!(!within("band", band, 0.7999, 1.0));
        assert!(!within("band", band, 1.2501, 1.0));
        assert!(within("exact", Bound::Scalar(0.0), 1.0, 1.0));
        assert!(!within("exact", Bound::Scalar(0.0), 1.0, 2.0));
    }

    #[test]
    fn the_tolerance_records_serialize_as_two_element_lists() {
        assert_eq!(tolerance_json("mid_rms"), json!(["relative", 0.10]));
        assert_eq!(
            tolerance_json("session_vol_hour"),
            json!(["band", [0.8, 1.25]])
        );
        assert_eq!(
            tolerance_json("minute_range_max"),
            json!(["envelope_upper", "resampled"])
        );
    }

    #[test]
    fn the_minute_range_envelope_supplies_a_lower_bound() {
        // The separated session ranges make the lower and upper resampled
        // p99 tails observably different. This catches an implementation
        // that merely copies the old upper bound into the new field.
        let sessions = BTreeMap::from([
            ("quiet".to_string(), (1..=100).collect()),
            ("active".to_string(), (1_001..=1_100).collect()),
        ]);

        let envelope = minute_range_envelope(&sessions).expect("nonempty sessions");
        let lower = envelope["p99_lower"].as_i64().expect("p99 lower bound");
        let upper = envelope["p99"].as_i64().expect("p99 upper bound");

        assert!(lower < upper, "lower tail must not duplicate upper tail");
        assert!((1..=1_100).contains(&lower));
    }
}
