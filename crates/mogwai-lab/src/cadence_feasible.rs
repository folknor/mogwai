// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `analysis/check_cadence_feasible.py`'s L0 structural-proceed verdict.
//!
//! Ported in full: [`next_count`] (the parent/child geometric-mixture
//! inverse-CDF draw), [`verdict`] (the structural `PROCEED`/`CLOSE`/`STOP AND
//! ASK` threshold read directly off `cadence.json`'s measured
//! `children_mean`/`children_single_frac`) and [`density_passes`] (the
//! per-second density feasibility bands).
//!
//! [`simulate_markov`] is the default CLI path's density re-simulation, and
//! it is a gate rather than a diagnostic: `check_cadence_feasible.py` exits
//! non-zero when the realized density misses the feasibility bands, so a port
//! that stops after the structural verdict exits 0 where the script it
//! replaces does not. The phase-3a record called this a secondary diagnostic;
//! that is true of the verdict and false of the command, and the difference
//! was found by the 2026-08-08 program review.
//!
//! It was originally skipped as needing a from-scratch CPython Mersenne
//! Twister. Phase 3b then built exactly that for `minute_range_envelope`, so
//! the remaining work was `random()`, `weibullvariate` and the simulation
//! loop - see [`crate::fit::mtrand`], whose stream is pinned against CPython.
//!
//! Still not ported: the `--fit` and `--fit-markov` grid searches. They are
//! search tools rather than gates, and both need `math.gamma` at arbitrary
//! shape - the default path only ever evaluates `gamma(2.0)`, which is
//! exactly 1. That is why [`simulate_markov`] takes `innovation_mean` as a
//! parameter instead of computing it: the one value the shipped path needs is
//! exact, and no half-validated gamma is introduced to fake the rest.

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

/// The knobs `simulate_markov` takes, matching the Python's positional
/// parameters. The shipped default path passes quiet fraction 0.35,
/// persistence 0.90, ratio 150.0, shape 1.0 and calibration 0.944.
pub struct MarkovParams {
    pub quiet_fraction: f64,
    pub state_persistence: f64,
    pub quiet_active_ratio: f64,
    pub weibull_shape: f64,
    pub calibration: f64,
    /// `math.gamma(1.0 + 1.0 / weibull_shape)`. Exactly 1.0 at unit shape,
    /// which is all the shipped path needs; see the module docs.
    pub innovation_mean: f64,
}

/// `simulate_markov`: the density re-simulation the default CLI path gates on.
///
/// A draw-for-draw port. The order matters as much as the arithmetic: one
/// `random()` picks the initial state, then per event one `weibullvariate`
/// for the gap, `next_count`'s one or two draws for the child count, and one
/// `random()` for the state transition. Any extra or missing draw
/// desynchronizes the whole stream from CPython and the bands then judge a
/// different experiment.
///
/// # Errors
/// [`LabError::Refusal`] if the cadence measurement or fingerprint session
/// profile is missing a required field.
pub fn simulate_markov(
    cadence: &Value,
    fingerprint: &Value,
    events: usize,
    seed: u64,
    params: &MarkovParams,
) -> LabResult<Value> {
    let mut rng = crate::fit::mtrand::PyRandom::new(seed);
    let mean =
        required(cadence, &["targets", "mean_event_duration_s", "anchor"])? * params.calibration;
    let children_mean = required(cadence, &["targets", "children_mean", "anchor"])?;
    let single_frac = required(cadence, &["targets", "children_single_frac", "anchor"])?;
    let session = fingerprint
        .get("session_profile")
        .ok_or_else(|| LabError::refusal("fingerprint carries no session_profile"))?;
    let intensity = session
        .get("intensity_hour")
        .and_then(Value::as_array)
        .ok_or_else(|| LabError::refusal("session_profile carries no intensity_hour array"))?;
    let dow = session
        .get("dow_weight")
        .and_then(Value::as_array)
        .ok_or_else(|| LabError::refusal("session_profile carries no dow_weight array"))?;

    let active_mean =
        mean / ((1.0 - params.quiet_fraction) + params.quiet_fraction * params.quiet_active_ratio);
    let quiet_mean = active_mean * params.quiet_active_ratio;
    let q_to_a = (1.0 - params.state_persistence) * (1.0 - params.quiet_fraction);
    let a_to_q = (1.0 - params.state_persistence) * params.quiet_fraction;
    let mut quiet = rng.random() < params.quiet_fraction;
    let quiet_children_mean = children_mean * 0.20;
    let active_children_mean = children_mean * 1.430_769_230_769_230_8;
    let (quiet_q, quiet_m) = (0.0, quiet_children_mean);
    let quiet_single = 1.0 / quiet_children_mean;
    let active_single =
        (single_frac - params.quiet_fraction * quiet_single) / (1.0 - params.quiet_fraction);
    let active_m = (active_children_mean - 1.0) / (1.0 - active_single);
    let active_q = 1.0 - (active_children_mean - 1.0) / (active_m - 1.0);

    let mut clock = 0.0f64;
    let mut buckets: std::collections::HashMap<i64, i64> = std::collections::HashMap::new();
    let mut gaps: Vec<f64> = Vec::with_capacity(events);
    let mut truncated = 0i64;
    for _ in 0..events {
        let gap_mean = if quiet { quiet_mean } else { active_mean };
        let mut gap =
            gap_mean * rng.weibullvariate(1.0, params.weibull_shape) / params.innovation_mean;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "int(clock) on a positive elapsed-seconds float, matching Python"
        )]
        let whole = clock as i64;
        let hour = ((whole % 86_400) / 3_600) as usize;
        let day = (((whole / 86_400) + 4) % 7) as usize;
        let mut arrival = intensity
            .get(hour)
            .and_then(Value::as_f64)
            .ok_or_else(|| LabError::refusal("intensity_hour is shorter than 24 entries"))?
            * 24.0;
        arrival *= dow
            .get(day)
            .and_then(Value::as_f64)
            .ok_or_else(|| LabError::refusal("dow_weight is shorter than 7 entries"))?
            * 7.0;
        gap /= arrival;
        clock += gap;
        gaps.push(gap);
        let (q, m) = if quiet {
            (quiet_q, quiet_m)
        } else {
            (active_q, active_m)
        };
        let mut uniform = || rng.random();
        let (count, clipped) = next_count(&mut uniform, q, m);
        truncated += i64::from(clipped);
        #[expect(clippy::cast_possible_truncation, reason = "int(clock), as above")]
        let second = clock as i64;
        *buckets.entry(second).or_insert(0) += count;
        quiet = if quiet {
            rng.random() >= q_to_a
        } else {
            rng.random() < a_to_q
        };
    }

    #[expect(clippy::cast_possible_truncation, reason = "int(clock) + 1, as above")]
    let span = clock as i64 + 1;
    let mut histogram: std::collections::BTreeMap<i64, i64> = std::collections::BTreeMap::new();
    histogram.insert(0, span - buckets.len() as i64);
    for count in buckets.values() {
        *histogram.entry(*count).or_insert(0) += 1;
    }
    let quantile = |fraction: f64| -> LabResult<i64> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "math.ceil of a fraction of a positive span"
        )]
        let rank = (fraction * span as f64).ceil() as i64;
        let mut seen = 0i64;
        for (count, frequency) in &histogram {
            seen += frequency;
            if seen >= rank {
                return Ok(*count);
            }
        }
        Err(LabError::refusal("density quantile ran past the histogram"))
    };
    let total: i64 = histogram.iter().map(|(count, freq)| count * freq).sum();
    let gap_mean = crate::kernel::py_fsum(gaps.iter().copied()) / gaps.len() as f64;
    let gap_var = gap_pvariance(&gaps);
    let acf = |lag: usize| -> f64 {
        let numerator = crate::kernel::py_sum(
            gaps.iter()
                .zip(gaps.iter().skip(lag))
                .map(|(a, b)| (a - gap_mean) * (b - gap_mean)),
        );
        let denominator = crate::kernel::py_sum(gaps.iter().map(|v| {
            let d = v - gap_mean;
            d * d
        }));
        numerator / denominator
    };
    Ok(serde_json::json!({
        "mean": total as f64 / span as f64,
        "median": quantile(0.5)?,
        "p95": quantile(0.95)?,
        "zero_frac": histogram[&0] as f64 / span as f64,
        "truncation_frac": truncated as f64 / events as f64,
        "gap_mean": gap_mean,
        // `gap_mean * gap_mean` rather than `powi(2)`: `f64::powi` documents its
        // precision as varying by platform and Rust version, which would make
        // the exact variance above pointless by reintroducing a
        // platform-dependent rounding one operation later. A single
        // multiplication is correctly rounded everywhere.
        "gap_cv2": gap_var / (gap_mean * gap_mean),
        "gap_acf1": acf(1),
        "gap_acf5": acf(5),
    }))
}

/// Population variance of the gap series, exactly as
/// `statistics.pvariance` computes it.
///
/// No deviation remains here. Every field of the density report is now bit-exact
/// against CPython, and the history is kept because it is the reason this
/// delegates rather than computing in floating point.
///
/// `check_cadence_feasible.py:187` calls `statistics.pvariance(gaps)` with no
/// explicit `mu`, which does NOT subtract a rounded mean before squaring. It
/// evaluates `(n * sum(x^2) - sum(x)^2) / n^2` as an exact rational over the
/// binary64 inputs and rounds once at the end. The obvious port - `py_fsum` over
/// squared deviations from the rounded mean - is not a last-bit difference from
/// that. It is ill-conditioned: for a clustered series the true variance is a
/// difference of quantities agreeing in almost every bit, so the rounding of
/// each individual square dominates the answer. On three nearly-equal gaps it
/// came out wrong by a factor of three.
///
/// That approach was defended here across three revisions of this comment, each
/// claiming a tighter ULP ceiling than the last - one from a constructed
/// three-gap vector, refuted by `--events 14` at two; then two, refuted by
/// search at three; then the clustered case, which showed the error has no
/// bound to find at all. The lesson is worth more than the code: a bound
/// established over the fixtures you happen to have is not a bound.
///
/// [`crate::exact::population_variance`] computes the same rational CPython
/// does, in integer arithmetic, with the single rounding at the end. It is
/// pinned against `statistics.pvariance` over a generated 820-case sweep
/// (`crates/mogwai-lab/tests/exact_pvariance.rs`) whose families deliberately
/// include the clustered and adjacent-neighbour cases that broke the old one.
fn gap_pvariance(gaps: &[f64]) -> f64 {
    crate::exact::population_variance(gaps)
}

#[cfg(test)]
mod gap_cv2_parity {
    use super::*;

    /// The reported `gap_cv2`, computed the way `density` reports it, so these
    /// cases pin the field rather than the intermediate variance. That
    /// distinction matters: the variance could be exact while the division that
    /// forms `gap_cv2` reintroduced a platform-dependent rounding, which is
    /// precisely what `powi(2)` would have done.
    fn gap_cv2(gaps: &[f64]) -> f64 {
        let gap_mean = crate::kernel::py_fsum(gaps.iter().copied()) / gaps.len() as f64;
        gap_pvariance(gaps) / (gap_mean * gap_mean)
    }

    /// The three-gap case, the vector originally built to discriminate against
    /// the old ill-conditioned implementation. CPython 3.14.6:
    /// `statistics.pvariance(gaps)` is `0.14509134298012094` and `gap_cv2` is
    /// `0.33706429233938623`. The old code returned `0.3370642923393863`.
    #[test]
    fn the_three_gap_case_matches_cpython_exactly() {
        let gaps = [0.154_148_210_468, 1.076_405_188_57, 0.737_720_944_656];
        assert_eq!(
            gap_pvariance(&gaps).to_bits(),
            0.145_091_342_980_120_94_f64.to_bits()
        );
        assert_eq!(
            gap_cv2(&gaps).to_bits(),
            0.337_064_292_339_386_23_f64.to_bits()
        );
    }

    /// Loads the committed inputs the CLI path uses, so the two simulation
    /// cases below exercise the real reported field rather than a vector.
    fn committed_inputs() -> (Value, Value) {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cadence = serde_json::from_str(
            &std::fs::read_to_string(root.join("analysis/cadence.json")).expect("cadence.json"),
        )
        .expect("cadence parses");
        let fingerprint = serde_json::from_str(
            &std::fs::read_to_string(root.join("analysis/fingerprint.json"))
                .expect("fingerprint.json"),
        )
        .expect("fingerprint parses");
        (cadence, fingerprint)
    }

    /// The CLI defaults, matching `check_cadence_feasible.py`'s.
    fn default_params() -> MarkovParams {
        MarkovParams {
            quiet_fraction: 0.35,
            state_persistence: 0.90,
            quiet_active_ratio: 150.0,
            weibull_shape: 1.0,
            calibration: 0.944,
            innovation_mean: 1.0,
        }
    }

    /// The `--events 14` path, which is what refuted the first ULP ceiling.
    /// This is a real CLI invocation rather than a constructed vector, and it
    /// used to disagree by two ULPs on `gap_cv2` while every other field agreed
    /// bit for bit.
    ///
    /// From `python3 analysis/check_cadence_feasible.py --events 14` on CPython
    /// 3.14.6: `gap_cv2` `0.6921791630839342`, `gap_mean`
    /// `0.0024246517715526728`, `gap_acf1` `-0.1481234936964859`, `gap_acf5`
    /// `-0.22459354537017304`, `mean` 203.0, `median` 203, `p95` 203,
    /// `zero_frac` 0.0.
    #[test]
    fn the_fourteen_event_cli_path_matches_cpython_field_for_field() {
        let (cadence, fingerprint) = committed_inputs();
        let got =
            simulate_markov(&cadence, &fingerprint, 14, 42, &default_params()).expect("simulates");
        assert_eq!(got["median"], 203);
        assert_eq!(got["p95"], 203);
        for (field, expected) in [
            ("mean", 203.0_f64),
            ("zero_frac", 0.0_f64),
            ("truncation_frac", 0.0_f64),
            ("gap_mean", 0.002_424_651_771_552_672_8_f64),
            ("gap_cv2", 0.692_179_163_083_934_2_f64),
            ("gap_acf1", -0.148_123_493_696_485_9_f64),
            ("gap_acf5", -0.224_593_545_370_173_04_f64),
        ] {
            let actual = got[field].as_f64().expect("numeric field");
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{field}: {actual:?} against CPython {expected:?}"
            );
        }
    }

    /// The default 3,000,000-event run, the one the subcommand performs with no
    /// arguments and the one whose exit status is the gate. Pinned bit-exact
    /// against `python3 analysis/check_cadence_feasible.py` on CPython 3.14.6,
    /// `gap_cv2` included, because before this the density report at the
    /// default event count had no gate of its own at all - the parity test next
    /// to it asserts only the structural verdict.
    ///
    /// Slower than the rest of this module by a wide margin, and deliberately
    /// not skipped: an unpinned default path is exactly how an unstated
    /// mismatch survived here once already.
    #[test]
    fn the_default_three_million_event_run_matches_cpython_field_for_field() {
        let (cadence, fingerprint) = committed_inputs();
        let got = simulate_markov(&cadence, &fingerprint, 3_000_000, 42, &default_params())
            .expect("simulates");
        assert_eq!(got["median"], 3);
        assert_eq!(got["p95"], 357);
        for (field, expected) in [
            ("mean", 51.019_534_657_973_01_f64),
            ("zero_frac", 0.129_516_386_850_407_45_f64),
            ("truncation_frac", 0.0_f64),
            ("gap_mean", 0.166_200_266_138_206_6_f64),
            ("gap_cv2", 4.853_557_527_000_749_f64),
            ("gap_acf1", 0.358_561_431_355_429_45_f64),
            ("gap_acf5", 0.239_282_477_238_891_87_f64),
        ] {
            let actual = got[field].as_f64().expect("numeric field");
            assert_eq!(
                actual.to_bits(),
                expected.to_bits(),
                "{field}: {actual:?} against CPython {expected:?}"
            );
        }
    }

    /// The case that killed the ULP framing, now exact. Three nearly-equal gaps
    /// make the true variance a difference of quantities agreeing in all but
    /// the last two bits; the old implementation was wrong by a factor of three
    /// here, returning `0x1.5555555555555p-107` against CPython's
    /// `0x1.c71c71c71c71cp-109`. Kept as the sharpest regression case in the
    /// suite, with inputs as bit patterns so it cannot drift through decimal
    /// parsing.
    #[test]
    fn three_nearly_equal_gaps_match_cpython_exactly() {
        let gaps = [
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBE),
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBE),
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBF),
        ];
        assert_eq!(
            gap_pvariance(&gaps).to_bits(),
            0x392C_71C7_1C71_C71C,
            "the factor-of-three error must be gone, not merely smaller"
        );
        assert_ne!(
            gap_pvariance(&gaps).to_bits(),
            0x3945_5555_5555_5555,
            "that is the OLD wrong value; seeing it again means the exact path was bypassed"
        );
    }
}

/// `verdict`: the structural L0 proceed/close/stop verdict, read directly
/// off `cadence.json`'s `targets.children_mean`/`children_single_frac`
/// anchors.
///
/// # Errors
/// [`LabError::Refusal`] if either anchor is absent or non-numeric. This
/// previously read a missing anchor as zero, which let a document carrying
/// `children_mean` but no `children_single_frac` return `CLOSE`, and one
/// carrying neither return `CLOSE` as well - a verdict fabricated from input
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

    /// The small-event simulation fixture. Five thousand events over the
    /// committed cadence and fingerprint, against a direct run of
    /// `check_cadence_feasible.simulate_markov` with the same arguments. Small
    /// enough to run in a unit test, long enough that a draw-consumption,
    /// bucketing or state-update difference desynchronizes the stream and
    /// moves every field - which is what makes it a discriminator rather than
    /// a smoke test.
    #[test]
    fn the_simulation_reproduces_cpython_over_five_thousand_events() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let cadence: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("analysis/cadence.json")).expect("cadence.json"),
        )
        .expect("cadence parses");
        let fingerprint: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join("analysis/fingerprint.json"))
                .expect("fingerprint.json"),
        )
        .expect("fingerprint parses");
        let params = MarkovParams {
            quiet_fraction: 0.35,
            state_persistence: 0.90,
            quiet_active_ratio: 150.0,
            weibull_shape: 1.0,
            calibration: 0.944,
            innovation_mean: 1.0,
        };
        let got = simulate_markov(&cadence, &fingerprint, 5000, 42, &params).expect("simulates");
        // From `python3 -c "... simulate_markov(data, 5000, 42, 0.35, 0.90,
        // 150.0, 1.0, 0.944)"` on CPython 3.14.6.
        assert_eq!(got["median"], 4);
        assert_eq!(got["p95"], 385);
        assert_eq!(got["truncation_frac"], 0.0);
        // Exact, not within a tolerance. The `1e-12` relative band this loop
        // used to carry was wide enough to hide a real defect: every one of
        // these fields is bit-reproducible against CPython, so a tolerance
        // bought nothing except silence about the one field that genuinely
        // deviates. `mean` and `zero_frac` are gate-driving inputs to
        // `density_passes`; the ACFs are not, but they are exact and stay
        // pinned exact - adjacency to `gap_cv2` is not a reason to loosen them.
        for (field, expected) in [
            ("mean", 61.865_979_381_443_296_f64),
            ("zero_frac", 0.069_219_440_353_460_98_f64),
            ("gap_mean", 0.135_782_319_836_986_05_f64),
            ("gap_acf1", 0.360_406_934_815_399_5_f64),
            ("gap_acf5", 0.258_181_483_555_129_2_f64),
        ] {
            let actual = got[field].as_f64().expect("numeric field");
            assert!(
                actual.to_bits() == expected.to_bits(),
                "{field}: {actual:?} against CPython {expected:?}, and this field is pinned \
                 bit-exact rather than to a tolerance"
            );
        }
        // `gap_cv2` agrees bit for bit at this event count, and that agreement
        // is incidental rather than guaranteed - see `gap_pvariance` for why no
        // bound holds in general, and the `deviation` module for two real cases
        // where this same field disagrees. Pinned exactly anyway: if it ever
        // moves here, something changed in the summation or the draw stream,
        // which is worth a loud failure even though cross-language agreement is
        // not claimed.
        let cv2 = got["gap_cv2"].as_f64().expect("numeric gap_cv2");
        assert_eq!(
            cv2.to_bits(),
            4.411_391_713_266_563_f64.to_bits(),
            "gap_cv2 moved at 5,000 events, where it had agreed with CPython exactly"
        );
    }

    /// The case that passed open. A document carrying `children_mean` but no
    /// `children_single_frac` used to read the missing anchor as zero, so
    /// `single < 0.90` held and the malformed measurement returned `PROCEED`.
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
