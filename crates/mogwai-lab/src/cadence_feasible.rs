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
//! [`simulate_markov`] is the default CLI path's density RE-simulation, and
//! it is a GATE rather than a diagnostic: `check_cadence_feasible.py` exits
//! non-zero when the realized density misses the feasibility bands, so a port
//! that stops after the structural verdict exits 0 where the script it
//! replaces does not. The phase-3a record called this a secondary diagnostic;
//! that is true of the VERDICT and false of the COMMAND, and the difference
//! was found by the 2026-08-08 program review.
//!
//! It was originally skipped as needing a from-scratch CPython Mersenne
//! Twister. Phase 3b then built exactly that for `minute_range_envelope`, so
//! the remaining work was `random()`, `weibullvariate` and the simulation
//! loop - see [`crate::fit::mtrand`], whose stream is pinned against CPython.
//!
//! STILL NOT PORTED: the `--fit` and `--fit-markov` grid searches. They are
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
    let gap_var = gap_pvariance(&gaps, gap_mean);
    let acf = |lag: usize| -> f64 {
        let numerator = crate::kernel::py_sum(
            gaps.iter()
                .zip(gaps.iter().skip(lag))
                .map(|(a, b)| (a - gap_mean) * (b - gap_mean)),
        );
        let denominator = crate::kernel::py_sum(gaps.iter().map(|v| (v - gap_mean).powi(2)));
        numerator / denominator
    };
    Ok(serde_json::json!({
        "mean": total as f64 / span as f64,
        "median": quantile(0.5)?,
        "p95": quantile(0.95)?,
        "zero_frac": histogram[&0] as f64 / span as f64,
        "truncation_frac": truncated as f64 / events as f64,
        "gap_mean": gap_mean,
        "gap_cv2": gap_var / gap_mean.powi(2),
        "gap_acf1": acf(1),
        "gap_acf5": acf(5),
    }))
}

/// Population variance of the gap series, about an already-rounded mean.
///
/// AN ALGORITHMIC DEVIATION FROM `statistics.pvariance`, WITH NO BOUND. This is
/// the only field in the density report that is not bit-exact against CPython,
/// and the size of the difference is NOT small in general. Read the whole of
/// this before quoting a number from it.
///
/// `check_cadence_feasible.py:187` calls `statistics.pvariance(gaps)` with no
/// explicit `mu`, which does NOT subtract a rounded mean before squaring. It
/// evaluates `(n * sum(x^2) - sum(x)^2) / n^2` as an exact rational over the
/// binary64 inputs and rounds once, at the end - so CPython's result is
/// correctly rounded. This function instead sums squared deviations from the
/// rounded mean. Matching CPython needs the exact value of that cancellation,
/// which `py_fsum` cannot supply: its interface rounds before the
/// `n * Q - S * S` subtraction, so an exact port needs a retained expansion or
/// a fixed superaccumulator.
///
/// WHY NO ULP CEILING IS CLAIMED, having twice been claimed wrongly. A
/// "one-ULP" bound was asserted from a three-gap fixture, then refuted by
/// `--events 14`, which gives two; "two" was refuted by search, which gives
/// three. The framing was wrong in kind rather than in degree: on three
/// NEARLY-EQUAL gaps this function is wrong BY A FACTOR OF THREE, a 200 percent
/// relative error, because the true variance is then a difference of quantities
/// agreeing in all but the last two bits and the rounding of each square
/// dominates the result. That case is pinned in the `deviation` tests below
/// against CPython's exact value, and it is not adversarial - a quiet cadence
/// regime with near-quantized arrivals has exactly that shape. Any
/// fixture-derived ceiling here is an artifact of the fixture.
///
/// WHAT IS ACTUALLY GUARANTEED, and it is worth stating because it is stronger
/// than a cross-language bound in one respect and weaker in another. This value
/// is a deterministic, platform-independent function of its input: `py_fsum` is
/// exact summation, and IEEE 754 subtraction, multiplication and division are
/// correctly rounded on every conforming platform. So the venue's own numbers
/// are reproducible everywhere - the property the `sqrt` deviation elsewhere in
/// this workspace exists to protect. What is NOT guaranteed is agreement with
/// CPython, at any tolerance.
///
/// WHY IT IS TOLERATED HERE ANYWAY: the value reaches the report solely as
/// `gap_cv2`, and `density_passes` (`check_cadence_feasible.py:227`), which
/// decides the nonzero exit at `:276`, reads `mean`, `median`, `p95` and
/// `zero_frac`. It does not read `gap_cv2`. The field's only Python consumer is
/// the ranking score at `:220` inside `fit_markov`. So today the deviation is
/// observable in a printed diagnostic and cannot move a verdict, an exit
/// status, or any committed artifact.
///
/// THIS IS A HARD GATE ON `--fit-markov`, not a caveat. That mode's score
/// divides `gap_cv2` by a cadence anchor, sums it with six other terms and
/// sorts the grid by the result. Feeding an unbounded relative error into a
/// ranking that selects shipped constants is not acceptable at any grid size,
/// so `--fit-markov` may not land until this function computes the exact
/// value. It may not inherit the tolerance above by arguing the error is
/// usually small.
fn gap_pvariance(gaps: &[f64], gap_mean: f64) -> f64 {
    crate::kernel::py_fsum(gaps.iter().map(|g| (g - gap_mean).powi(2))) / gaps.len() as f64
}

#[cfg(test)]
mod deviation {
    use super::*;

    /// Distance in representable steps. Stated as a count rather than a
    /// relative epsilon so a failure reports how far apart the values are in
    /// the only unit that does not drift with magnitude.
    pub(super) fn ulps_between(a: f64, b: f64) -> u64 {
        if a == b {
            return 0;
        }
        assert!(
            a.is_finite() && b.is_finite() && a.signum() == b.signum(),
            "ULP distance is only meaningful for finite same-signed values"
        );
        a.to_bits().abs_diff(b.to_bits())
    }

    /// THE THREE-GAP CASE. Pins THIS crate's value bit-exactly, as a regression
    /// pin on our own algorithm, and records CPython's differing value as an
    /// observation. It deliberately does NOT assert a bound: the assertion is
    /// that the two disagree, which is what makes the case evidence for the
    /// deviation existing.
    ///
    /// CPython 3.14.6: `statistics.pvariance(gaps)` is `0.14509134298012094`
    /// and `gap_cv2` is `0.33706429233938623`.
    #[test]
    fn the_three_gap_case_disagrees_with_cpython() {
        let gaps = [0.154_148_210_468, 1.076_405_188_57, 0.737_720_944_656];
        let gap_mean = crate::kernel::py_fsum(gaps.iter().copied()) / gaps.len() as f64;
        assert_eq!(
            gap_mean.to_bits(),
            0.656_091_447_898_f64.to_bits(),
            "the mean is exact; only the variance deviates, so a failure here means \
             something other than the variance moved"
        );

        let ours = gap_pvariance(&gaps, gap_mean);
        assert_eq!(
            ours.to_bits(),
            0.145_091_342_980_120_97_f64.to_bits(),
            "regression pin on our own algorithm, not a parity claim"
        );
        let cpython = 0.145_091_342_980_120_94_f64;
        assert_ne!(
            ours.to_bits(),
            cpython.to_bits(),
            "if these ever agree the case has stopped being evidence and the deviation \
             needs re-deriving rather than the test deleting"
        );
        assert_eq!(ulps_between(ours, cpython), 1, "observation, not a bound");
    }

    /// THE CASE THAT REFUTED THE FIRST BOUND, kept because it is a real CLI
    /// invocation rather than a constructed vector:
    /// `check_cadence_feasible.py --events 14` against
    /// `mogwai cadence-feasible --events 14`. Every other reported field agrees
    /// bit for bit, including both ACFs; `gap_cv2` is two ULPs apart. Recorded
    /// so nobody re-derives a one-ULP ceiling from the three-gap case alone.
    ///
    /// CPython 3.14.6 prints `gap_cv2` `0.6921791630839342`; this crate prints
    /// `0.6921791630839345`. Both commands exit nonzero, because the density
    /// bands fail either way - which is the concrete demonstration that the
    /// field does not gate.
    #[test]
    fn the_fourteen_event_cli_case_disagrees_by_more_than_the_three_gap_case() {
        let ours = 0.692_179_163_083_934_5_f64;
        let cpython = 0.692_179_163_083_934_2_f64;
        assert_eq!(
            ulps_between(ours, cpython),
            2,
            "the CLI path produces a larger distance than the constructed case, which is \
             why no ceiling is claimed from either"
        );
    }

    /// THE CASE THAT KILLED THE ULP FRAMING ENTIRELY. Three nearly-equal gaps
    /// make the true variance a difference of quantities that agree in all but
    /// the last two bits, so this function's rounding of each square dominates
    /// the answer completely and the result is WRONG BY A FACTOR OF THREE. Not
    /// an adversarial construction: a quiet cadence regime with near-quantized
    /// arrivals produces exactly this shape.
    ///
    /// That is the evidence that no ULP ceiling can be stated: the error here is
    /// 200 percent, not two steps. It also reframes what the deviation IS. It is
    /// not a last-bit rounding difference that happens to be visible; it is an
    /// ill-conditioned algorithm whose relative error is unbounded, tolerated
    /// only because the field it feeds cannot currently gate anything.
    ///
    /// Reference from CPython 3.14.6 over the same three inputs:
    /// `statistics.pvariance(gaps)` is `2.7391003653507353e-33`, exactly
    /// `0x1.c71c71c71c71cp-109`. This function returns
    /// `8.217301096052206e-33`, exactly `0x1.5555555555555p-107`. The inputs
    /// are given as hex float literals so the case cannot drift through decimal
    /// parsing.
    #[test]
    fn three_nearly_equal_gaps_are_wrong_by_a_factor_of_three() {
        let gaps = [
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBE),
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBE),
            f64::from_bits(0x3FEF_FFFF_FFFF_FFBF),
        ];
        let gap_mean = crate::kernel::py_fsum(gaps.iter().copied()) / gaps.len() as f64;
        assert_eq!(
            gap_mean.to_bits(),
            0x3FEF_FFFF_FFFF_FFBF,
            "the mean is exact here too, so the whole disagreement is the variance"
        );

        let ours = gap_pvariance(&gaps, gap_mean);
        assert_eq!(
            ours.to_bits(),
            0x3945_5555_5555_5555,
            "regression pin on our own value, 0x1.5555555555555p-107"
        );

        // CPython's exact result, 0x1.c71c71c71c71cp-109.
        let cpython = f64::from_bits(0x392C_71C7_1C71_C71C);
        assert!(ours > 0.0 && cpython > 0.0);
        let ratio = ours / cpython;
        assert!(
            (ratio - 3.0).abs() < 1e-9,
            "expected our value to be three times CPython's, got ratio {ratio} \
             from {ours:?} against {cpython:?}"
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

    /// THE SMALL-EVENT SIMULATION FIXTURE. Five thousand events over the
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
        // EXACT, not within a tolerance. The `1e-12` relative band this loop
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
        // `gap_cv2` AGREES BIT FOR BIT AT THIS EVENT COUNT, and that agreement
        // is incidental rather than guaranteed - see `gap_pvariance` for why no
        // bound holds in general, and the `deviation` module for two real cases
        // where this same field disagrees. Pinned exactly anyway: if it ever
        // moves HERE, something changed in the summation or the draw stream,
        // which is worth a loud failure even though cross-language agreement is
        // not claimed.
        let cv2 = got["gap_cv2"].as_f64().expect("numeric gap_cv2");
        assert_eq!(
            cv2.to_bits(),
            4.411_391_713_266_563_f64.to_bits(),
            "gap_cv2 moved at 5,000 events, where it had agreed with CPython exactly"
        );
    }

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
