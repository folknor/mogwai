#!/usr/bin/env python3
"""POST-OBSERVATION conformance fixtures for targets F3 through F6.

READ THIS BEFORE CITING ANYTHING BELOW AS EVIDENCE.

These fixtures are NOT preregistered and NOT blind. All seven uncovered
2026-06 target levels - `size_round_frac`, `size_dispersion`,
`return_acf_lag1`, `abs_acf_lag1`, `abs_acf_lag10`, `abs_acf_lag50` and
`zero_change_frac` - were printed and read by the `build_targets equivalence`
run before any fixture here existed. That happened because the equivalence gate
was written to display the ungated targets alongside the gated ones, and it was
a sequencing error: the estimator conformance check belonged before the run that
produced real values, not after.

What that costs and what it does not:

  LOST      the claim that F3-F6 estimator conformance was established blind.
            It cannot be recovered by writing fixtures now.
  RETAINED  the rank-association verdict. The Spearman thresholds were fixed
            before any target existed and are unchanged. Only ONE month was
            observed, 2026-06, and one held-out level reveals nothing about its
            rank among the other six or about any correlation. No calibration
            month, no cross-month rank and no association result was seen.

So these fixtures validate IMPLEMENTATION BEHAVIOUR against independently
derived answers. That is worth having. It is not a preregistered conformance
claim and must never be described as one.

Every expected value below is derived from the explicit formula or from a
hand-auditable sequence, and the derivation is written beside it. None is taken
from production-estimator output - that would make the check circular. Fixture
shapes and tolerances were chosen to exercise the arithmetic, deliberately not
to resemble or admit the observed 2026-06 values, which sit nowhere near them.

Fixtures run through the REAL entry point: each is a genuine ZIP containing a
genuine Binance-layout CSV, handed to `build_targets.compute_targets` unchanged.
No production code was refactored to make it testable, so the parse path, the
grouping and the accumulation under test are the ones the batch runs.

    python3 analysis/conformance_f3_f6.py freeze   # write fixtures + expected
    python3 analysis/conformance_f3_f6.py verify   # run the estimators
"""

from __future__ import annotations

import argparse
import json
import math
import sys
import zipfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from build_targets import compute_targets  # noqa: E402
from probe_binance_aggtrades import AutoCorr  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "analysis/fixtures"
EXPECTED = ROOT / "analysis/conformance-f3-f6-expected.json"

# Tolerances FIXED BEFORE the production estimator was run against any fixture.
#
# EXACT where the arithmetic permits it: `zero_change_frac` and
# `size_round_frac` are ratios of small integers and must land on the nose.
#
# `size_dispersion` is a ratio of two geometric bin centres, so it passes
# through log10 and a fractional power of ten; 1e-9 relative is far tighter
# than any real estimator error and far looser than double-precision noise.
#
# The ACFs accumulate sums of squares over a dozen terms, so exact equality
# would be testing the summation order rather than the estimator.
REL_TOL_DISPERSION = 1e-9
REL_TOL_ACF = 1e-12

LVL_LOG_LO = 1e-6
LVL_PER_DEC = 10


def bin_centre(index: int) -> float:
    """The formula histogram_quantile uses, restated here for derivation.

    Restated rather than imported ON PURPOSE: an expected value computed by
    calling the production helper would prove only that the helper equals
    itself.
    """
    return LVL_LOG_LO * 10 ** ((index + 0.5) / LVL_PER_DEC)


def row(trade_id: int, price: str, qty: str, stamp: int, buyer_maker: str) -> str:
    # Binance spot layout: id, price, qty, quote_qty, time, is_buyer_maker
    return f"{trade_id},{price},{qty},0.0,{stamp},{buyer_maker}"


def alternating_prices(pairs: int) -> list[str]:
    """100, 200, 100, 200, ... so log returns alternate +ln2, -ln2 exactly."""
    out = []
    for i in range(2 * pairs + 1):
        out.append("200.0" if i % 2 else "100.0")
    return out


def fixtures() -> list[dict]:
    cases: list[dict] = []

    # ---------------------------------------------------------------- F4, F5
    # 13 prices alternating 100/200 give 12 log returns: +ln2, -ln2, ... six of
    # each, so the mean is exactly 0.
    #   var      = (ln2)^2
    #   cross[1] = 11 consecutive products, every one -(ln2)^2, over m = 11
    #   cov      = -(ln2)^2 - 0
    #   acf(1)   = -(ln2)^2 / (ln2)^2 = -1
    # Lag 10 is an EVEN offset, so every product is +(ln2)^2 and acf(10) = +1.
    # With 12 returns the acf loop stops at d = 11, so lag 50 is UNAVAILABLE.
    #
    # The abs_acf lags are DELIBERATELY NOT ASSERTED on this fixture. |return|
    # is the constant ln2, so the estimator is ill-conditioned: see the
    # `constant_nonzero_magnitude` finding recorded below. Pinning a value that
    # is decided by rounding would test the FPU, not the estimator.
    prices = alternating_prices(6)
    cases.append({
        "name": "alternating_returns",
        "why": "lag-1 ACF is exactly -1 by construction; lag 50 unavailable",
        "rows": [row(i + 1, p, "1.0", 1_000_000 * (i + 1), "true")
                 for i, p in enumerate(prices)],
        "expected": {
            "return_acf_lag1": -1.0,
            "abs_acf_lag50": None,
            "zero_change_frac": 0.0,
        },
        "tolerances": {
            "return_acf_lag1": REL_TOL_ACF,
            "zero_change_frac": "exact",
        },
    })

    # ---------------------------------------------------------------- F4, F5
    # A WELL-CONDITIONED abs-ACF case, replacing the ill-conditioned one above.
    # Returns alternate +ln2, -ln4, so |return| alternates ln2, ln4 - a genuine
    # two-value series with real variance rather than a constant.
    #
    # For any sequence alternating between a and b with an even count:
    #   mean      = (a + b) / 2
    #   var       = ((a - b) / 2)^2
    #   every lag-1 product is a*b, so cov = a*b - ((a + b)/2)^2
    #                                      = -((a - b)/2)^2
    #   acf(1)    = -1
    # With a = ln2 and b = ln4 the cancellation is benign: the terms differ by
    # the same order as their magnitude, unlike the constant case.
    #
    # The RETURN series is +a, -b, +a, -b, which is also two-valued, so its
    # lag-1 ACF is -1 by the identical algebra.
    #
    # Prices: 100 -> 200 (x2) -> 50 (/4) -> 100 (x2) -> 25 (/4) ...
    swing = ["100.0", "200.0", "50.0", "100.0", "25.0", "50.0", "12.5", "25.0",
             "6.25"]
    cases.append({
        "name": "two_valued_magnitudes",
        "why": "|return| alternates ln2, ln4 so the abs ACF is -1 and conditioned",
        "rows": [row(i + 1, p, "1.0", 1_000_000 * (i + 1), "true")
                 for i, p in enumerate(swing)],
        "expected": {
            "return_acf_lag1": -1.0,
            "abs_acf_lag1": -1.0,
            "abs_acf_lag10": None,
            "abs_acf_lag50": None,
        },
        "tolerances": {
            "return_acf_lag1": REL_TOL_ACF,
            "abs_acf_lag1": REL_TOL_ACF,
        },
        "derivation": "alternating a,b gives cov = -((a-b)/2)^2 = -var, so acf1 = -1",
    })

    # -------------------------------------------------------------------- F6
    # Prices 100, 100, 101, 101, 101 give 4 changes of which 3 are zero.
    #   zero_change_frac = 3 / 4 = 0.75, exactly.
    cases.append({
        "name": "zero_change_fraction",
        "why": "3 unchanged of 4 consecutive pairs, a ratio of small integers",
        "rows": [row(i + 1, p, "1.0", 1_000_000 * (i + 1), "true")
                 for i, p in enumerate(["100.0", "100.0", "101.0", "101.0", "101.0"])],
        "expected": {"zero_change_frac": 0.75},
        "tolerances": {"zero_change_frac": "exact"},
    })

    # -------------------------------------------------------------------- F6
    # A constant price makes every pair a zero change: 4 of 4.
    cases.append({
        "name": "constant_price",
        "why": "degenerate but defined: zero_change_frac is 1.0, not unavailable",
        "rows": [row(i + 1, "100.0", "1.0", 1_000_000 * (i + 1), "true")
                 for i in range(5)],
        "expected": {
            "zero_change_frac": 1.0,
            # Returns are all exactly 0.0, so sum, sumsq and mean are exactly 0
            # and the var <= 0 guard DOES fire, yielding 0.0. That is the
            # estimator substituting a number for an absence - the failure this
            # project keeps recording. This assertion PINS that path: it is a
            # regression contract, and a change to AutoCorr's guard would break
            # this fixture. Pinned rather than corrected because AutoCorr is
            # shared with the F1/F2 path whose bit-exact cadence.json
            # equivalence is a stronger guarantee than the fix would buy.
            "return_acf_lag1": 0.0,
        },
        "tolerances": {"zero_change_frac": "exact", "return_acf_lag1": "exact"},
        "finding": (
            "RECORDED LIMITATION, not a pinned contract - no assertion in this "
            "file protects it. The zero-variance guard fires only for an "
            "EXACTLY REPRESENTABLE constant, which the assertion above does "
            "pin. A series constant at an irrational value - |return| = ln2 in "
            "the alternating fixture - leaves a tiny positive float residue as "
            "its variance, so the guard misses and the ACF is decided by "
            "catastrophic cancellation. That behaviour is deliberately NOT "
            "asserted anywhere: the value is chosen by rounding, so pinning it "
            "would test the FPU. Real monthly series carry positive return "
            "variance and come nowhere near the degenerate case, so neither "
            "path fires in the batch. Any numerical-stability fix is separate "
            "future work and needs explicit cadence-impact analysis, because "
            "AutoCorr also computes the F1 duration ACFs."
        ),
    })

    # -------------------------------------------------------------- F4/F5 N/A
    # Two trades give ONE return. AutoCorr.acf refuses below n = 2 and returns
    # an empty list, so every ACF lag is UNAVAILABLE. This is the
    # insufficient-length case.
    cases.append({
        "name": "insufficient_length",
        "why": "one return is below AutoCorr's n>=2 floor; all ACF lags None",
        "rows": [row(1, "100.0", "1.0", 1_000_000, "true"),
                 row(2, "101.0", "1.0", 2_000_000, "false")],
        "expected": {
            "return_acf_lag1": None,
            "abs_acf_lag1": None,
            "abs_acf_lag10": None,
            "abs_acf_lag50": None,
            "zero_change_frac": 0.0,
        },
        "tolerances": {"zero_change_frac": "exact"},
    })

    # -------------------------------------------------------------------- F3
    # decimals_used strips trailing zeros, so "3.10" has ONE significant
    # decimal and "3.00" has none. round_frac counts entries with 0, 1 or 2.
    #   qualifying: 1.5, 1.25, 2, 3.10                       -> 4
    #   not:        0.125, 0.0625, 1.234, 9.8765, 0.001, 5.55555
    #   round_frac = 4 / 10 = 0.4, exactly.
    qtys = ["1.5", "1.25", "2", "3.10",
            "0.125", "0.0625", "1.234", "9.8765", "0.001", "5.55555"]
    cases.append({
        "name": "size_round_fraction",
        "why": "4 of 10 sizes carry at most two significant decimals",
        "rows": [row(i + 1, "100.0", q, 1_000_000 * (i + 1), "true")
                 for i, q in enumerate(qtys)],
        "expected": {"size_round_frac": 0.4},
        "tolerances": {"size_round_frac": "exact"},
    })

    # -------------------------------------------------------------------- F3
    # lvl_bin(v) = floor(log10(v / 1e-6) * 10).
    #   0.003 -> log10(3000)   = 3.47712 -> 34.77 -> bin 34
    #   3.0   -> log10(3e6)    = 6.47712 -> 64.77 -> bin 64
    # Both sit ~0.77 of the way into their bin, deliberately far from an edge so
    # the fixture cannot flip on a last-bit rounding difference.
    #
    # Ten rows, five in each bin. histogram_quantile takes threshold =
    # ceil(q * total):
    #   p50 -> ceil(5)  = 5  -> satisfied by bin 34 -> centre 10^(34.5/10) * 1e-6
    #   p90 -> ceil(9)  = 9  -> needs bin 64        -> centre 10^(64.5/10) * 1e-6
    #   dispersion = 10^((64.5 - 34.5)/10) = 10^3 = 1000
    cases.append({
        "name": "size_dispersion",
        "why": "two log bins exactly 30 apart give a dispersion of 1000",
        "rows": [row(i + 1, "100.0", "0.003" if i < 5 else "3.0",
                     1_000_000 * (i + 1), "true")
                 for i in range(10)],
        "expected": {"size_dispersion": bin_centre(64) / bin_centre(34)},
        "tolerances": {"size_dispersion": REL_TOL_DISPERSION},
        "derivation": "10 ** ((64.5 - 34.5) / 10) == 1000",
    })

    # ---------------------------------------------------------------- F3 N/A
    # Every quantity is zero, so nothing enters the size histogram and
    # histogram_quantile returns None for both percentiles. size_dispersion is
    # UNAVAILABLE - and must be None rather than 0.0 or 1.0, either of which
    # would read as a measured dispersion.
    cases.append({
        "name": "size_histogram_empty",
        "why": "no positive size, so dispersion is unavailable rather than 0 or 1",
        "rows": [row(i + 1, f"{100 + i}.0", "0", 1_000_000 * (i + 1), "true")
                 for i in range(5)],
        "expected": {"size_dispersion": None, "size_round_frac": 1.0},
        "tolerances": {"size_round_frac": "exact"},
    })

    return cases


def write_fixture(case: dict) -> Path:
    FIXTURES.mkdir(parents=True, exist_ok=True)
    path = FIXTURES / f"{case['name']}.zip"
    body = "\n".join(case["rows"]) + "\n"
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as zf:
        zf.writestr(f"{case['name']}.csv", body)
    return path


def mode_freeze() -> int:
    if EXPECTED.exists():
        print(f"{EXPECTED.name} already exists; refusing to overwrite frozen "
              "expectations")
        print("Deleting it to re-freeze is a DEVIATION and must be recorded in "
              "notes/sampling-frame-preregistration.md with the reason.")
        return 1
    cases = fixtures()
    payload = {
        "standing": "POST-OBSERVATION. Not preregistered, not blind. All seven "
                    "uncovered 2026-06 target levels were read before these "
                    "fixtures existed. They validate implementation behaviour "
                    "only.",
        "derivation": "Every expected value comes from an explicit formula or a "
                      "hand-auditable sequence, never from production output.",
        "tolerances_fixed_before_running": {
            "exact": ["zero_change_frac", "size_round_frac"],
            "size_dispersion_relative": REL_TOL_DISPERSION,
            "acf_relative": REL_TOL_ACF,
        },
        "cases": [],
    }
    for case in cases:
        write_fixture(case)
        payload["cases"].append({
            "name": case["name"],
            "why": case["why"],
            "rows": len(case["rows"]),
            "expected": case["expected"],
            "tolerances": case.get("tolerances", {}),
            "derivation": case.get("derivation"),
            "finding": case.get("finding"),
        })
    EXPECTED.write_text(json.dumps(payload, indent=2, sort_keys=True))
    print(f"froze {len(cases)} fixtures to {FIXTURES.name}/ and {EXPECTED.name}")
    for case in cases:
        print(f"  {case['name']:<24} {case['why']}")
    return 0


def compare(actual, expected, tolerance) -> tuple[bool, str]:
    if expected is None:
        return actual is None, f"{actual!r} vs None"
    if actual is None:
        return False, f"None vs {expected!r}"
    if tolerance == "exact" or tolerance is None:
        return actual == expected, f"{actual!r} vs {expected!r}"
    if expected == 0.0:
        return abs(actual) <= tolerance, f"{actual!r} vs {expected!r}"
    rel = abs(actual - expected) / abs(expected)
    return rel <= tolerance, f"{actual!r} vs {expected!r}  rel {rel:.3e}"


def restricted_lag_identity() -> tuple[int, int]:
    """AutoCorr with restricted lags must be BIT-IDENTICAL at those lags.

    `build_targets` accumulates only lags 1, 10 and 50 because they are the only
    ones any target reads, which cuts the dominant inner loop about sixteenfold.
    That is only legitimate if it changes no reported number. `cross[d]` is an
    independent running sum per lag, so it should not - and "should not" is not
    evidence, hence this check.

    A pseudo-random sequence is used rather than a smooth one: a degenerate or
    highly structured input could mask an indexing error by making several lags
    coincide. Seeded, so the check is reproducible.
    """
    import random

    rng = random.Random(20260805)
    series = [rng.gauss(0.0, 1.0) for _ in range(500)]

    full = AutoCorr(50)
    restricted = AutoCorr(50, lags=(1, 10, 50))
    for x in series:
        full.push(x)
        restricted.push(x)
    a = full.acf()
    b = restricted.acf()

    checks = 0
    failures = 0
    for lag in (1, 10, 50):
        checks += 1
        idx = lag - 1
        same = a[idx] == b[idx]
        if not same:
            failures += 1
        print(f"  {'ok  ' if same else 'FAIL'}  restricted_lag_identity  "
              f"lag {lag:<15} {b[idx]!r} vs {a[idx]!r}")
    # Non-requested lags must read None, never a number.
    for lag in (2, 25, 49):
        checks += 1
        is_none = b[lag - 1] is None
        if not is_none:
            failures += 1
        print(f"  {'ok  ' if is_none else 'FAIL'}  restricted_lag_identity  "
              f"lag {lag} unrequested  {b[lag - 1]!r} is None")
    return checks, failures


def mode_verify() -> int:
    if not EXPECTED.exists():
        print("no frozen expectations; run freeze first")
        return 1
    frozen = json.loads(EXPECTED.read_text())
    print(frozen["standing"])
    print()
    failures = 0
    checks = 0
    lag_checks, lag_failures = restricted_lag_identity()
    checks += lag_checks
    failures += lag_failures
    for case in frozen["cases"]:
        path = FIXTURES / f"{case['name']}.zip"
        if not path.exists():
            print(f"  FAIL  {case['name']}: fixture missing")
            failures += 1
            continue
        targets = compute_targets(path)["targets"]
        for field, expected in case["expected"].items():
            checks += 1
            tolerance = case.get("tolerances", {}).get(field)
            ok, detail = compare(targets[field], expected, tolerance)
            if not ok:
                failures += 1
            print(f"  {'ok  ' if ok else 'FAIL'}  {case['name']:<24} "
                  f"{field:<20} {detail}")
    print()
    print(f"{checks} checks, {failures} failed")
    print("CONFORMANT, post-observation standing" if not failures
          else "CONFORMANCE FAILED")
    return 1 if failures else 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("freeze", "verify"))
    args = parser.parse_args()
    sys.exit({"freeze": mode_freeze, "verify": mode_verify}[args.mode]())


if __name__ == "__main__":
    main()
