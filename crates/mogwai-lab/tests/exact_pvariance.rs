// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The differential gate for `mogwai_lab::exact::population_variance` against
//! CPython's `statistics.pvariance`.
//!
//! This exists because hand-picked cases are how the approach this replaced got
//! three successive ULP ceilings claimed and refuted. Six fixtures agreeing
//! proves that six fixtures agree. The parity claim here rests on a generated
//! sweep whose families were chosen to stress the arithmetic that actually
//! broke: clustered series, adjacent representable neighbours, and terms
//! spanning enough decades that naive accumulation drops the small ones.
//!
//! Inputs and expectations are raw bit patterns, so neither language's decimal
//! float parser sits on the critical path of a test about arithmetic.
//! Regenerate with `python3 scripts/gen_pvariance_cases.py`.
//!
//! Not `#[ignore]`d: the fixture is committed and the test needs no corpus, so
//! it runs in the ordinary gate rather than joining the parity gates that need
//! archives on disk.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    why: String,
    gaps: Vec<String>,
    expected: String,
}

fn parse_bits(hex: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(hex, 16).expect("16-digit hexadecimal bit pattern"))
}

#[test]
fn population_variance_matches_cpython_over_the_generated_sweep() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/pvariance_cases.json");
    let cases: Vec<Case> = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("reading {}: {e}", path.display())),
    )
    .expect("the fixture parses");

    assert!(
        cases.len() > 500,
        "the sweep is the evidence; a truncated fixture is not"
    );

    let mut per_family: BTreeMap<&str, usize> = BTreeMap::new();
    let mut nonzero = 0usize;
    let mut failures: Vec<String> = Vec::new();

    for case in &cases {
        *per_family.entry(case.why.as_str()).or_default() += 1;
        let gaps: Vec<f64> = case.gaps.iter().map(|b| parse_bits(b)).collect();
        let expected = parse_bits(&case.expected);
        if expected != 0.0 {
            nonzero += 1;
        }
        let got = mogwai_lab::exact::population_variance(&gaps);
        if got.to_bits() != expected.to_bits() {
            failures.push(format!(
                "{}: got {:?} ({:016x}) against CPython {:?} ({:016x}) for {:?}",
                case.why,
                got,
                got.to_bits(),
                expected,
                expected.to_bits(),
                case.gaps
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} cases diverge from statistics.pvariance:\n{}",
        failures.len(),
        cases.len(),
        failures
            .iter()
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );

    // The families have to actually be present, or a regenerated fixture could
    // quietly drop the ill-conditioned ones and leave a green test asserting
    // nothing about the case that motivated the module.
    for family in [
        "ordinary",
        "clustered",
        "adjacent",
        "wide",
        "pow2",
        "int",
        "identical",
        "subnormal",
    ] {
        assert!(
            per_family.get(family).copied().unwrap_or(0) >= 10,
            "family {family} is missing or too thin in the fixture: {per_family:?}"
        );
    }

    // The subnormal family has to actually BE subnormal. The original sweep
    // contained zero results, which exercise underflow to zero, and no nonzero
    // subnormals, which is the class where the rounding position changes rule -
    // and that gap is exactly what let a double-rounding defect through.
    let nonzero_subnormals = cases
        .iter()
        .filter(|c| c.why == "subnormal")
        .filter(|c| {
            let expected = parse_bits(&c.expected);
            expected > 0.0 && expected < f64::MIN_POSITIVE
        })
        .count();
    assert!(
        nonzero_subnormals >= 100,
        "the subnormal family must carry nonzero subnormal expectations, not zeros: \
         got {nonzero_subnormals}"
    );
    assert!(
        nonzero > cases.len() / 2,
        "most cases must have a nonzero variance, or the sweep is mostly testing zero"
    );
}
