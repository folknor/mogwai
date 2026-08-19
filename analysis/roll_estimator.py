"""Stratified Roll estimator, and the conformance runner for its shared fixture.

The Rust synthetic harness implements this same estimator against the generator;
this is the archive-analysis side. The two are deliberately kept separate - Rust
tests generator truth, Python handles archive analysis, and moving either before
the file contract is known would add integration work without reducing parser or
join risk.

What makes "the same estimator on both corpora" a claim rather than a hope is
`spread_conformance.json`: a fixed set of price series, per-change volatilities,
strata and expected outputs that BOTH implementations run. Equivalence is
tested, not asserted.

    python3 analysis/roll_estimator.py conformance
"""

import json
import math
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FIXTURE = os.path.join(ROOT, "analysis", "spread_conformance.json")

STATUS_MATCHED = "matched"
STATUS_UNAVAILABLE = "unavailable"
STATUS_FAIL_CLOSED = "fail_closed"


def stratum_of(vol, boundaries):
    return sum(1 for b in boundaries if vol >= b)


def roll_in_stratum(prices, change_vol, boundaries, stratum, tick, min_pairs):
    """Roll over the pairs whose LATER change falls in `stratum`.

    A pair spans `changes[i]` and `changes[i+1]` and contributes
    `dP_t * dP_{t-1}`. It is assigned by `change_vol[i+1]` - the later of its two
    changes - so a stratum boundary cannot claim a pair that straddles it, and
    the assigning volatility is one that was observable strictly before `dP_t`.
    Assigning by `change_vol[i]` instead would stratify on a term being
    multiplied and amplify the very relationship being measured.

    Returns `(status, pairs, covariance, roll_ticks)`. A non-negative covariance
    is UNAVAILABLE rather than zero: the bounce signature the estimator assumes
    is absent, so it has no value, and reporting zero would be a measurement
    nobody made.
    """
    changes = [b - a for a, b in zip(prices, prices[1:])]
    pairs = []
    for i in range(max(0, len(changes) - 1)):
        if i + 1 >= len(change_vol):
            continue
        vol = change_vol[i + 1]
        if vol is None:
            continue
        if stratum_of(vol, boundaries) == stratum:
            pairs.append((changes[i], changes[i + 1]))

    if len(pairs) < min_pairs:
        return STATUS_FAIL_CLOSED, len(pairs), None, None

    mean_a = sum(p[0] for p in pairs) / len(pairs)
    mean_b = sum(p[1] for p in pairs) / len(pairs)
    covariance = sum((a - mean_a) * (b - mean_b) for a, b in pairs) / len(pairs)
    if covariance < 0.0:
        return STATUS_MATCHED, len(pairs), covariance, 2.0 * math.sqrt(-covariance) / tick
    return STATUS_UNAVAILABLE, len(pairs), covariance, None


def conformance():
    with open(FIXTURE) as fh:
        spec = json.load(fh)
    # FIRST STATEMENT AFTER THE PARSE, matching the Rust half's `assert_eq!`.
    # The Rust half asserts this; printing it is not the same thing. A schema
    # bump that ADDS a gated field leaves this runner green while reading the
    # fixture under rules it was never re-read against, so the two halves would
    # stop being held to one contract exactly when the contract moved. It has to
    # precede every other read of `spec`: a v2 that renames or moves
    # `tolerance` would otherwise raise `KeyError` before the guard ran, and a
    # traceback is not the message this exists to deliver.
    if spec["version"] != 1:
        raise SystemExit(f"fixture version {spec['version']} != 1; re-read it")
    tol = spec["tolerance"]
    failures = 0
    print(f"spread conformance fixture v{spec['version']}, tolerance {tol:g}")
    for case in spec["cases"]:
        status, pairs, cov, roll = roll_in_stratum(
            case["prices"],
            case["change_vol"],
            case["boundaries"],
            case["stratum"],
            case["tick"],
            case["min_pairs"],
        )
        want = case["expect"]
        problems = []
        if status != want["status"]:
            problems.append(f"status {status} != {want['status']}")
        if pairs != want["pairs"]:
            problems.append(f"pairs {pairs} != {want['pairs']}")
        for label, got, expected in (
            ("covariance", cov, want["covariance"]),
            ("roll_ticks", roll, want["roll_ticks"]),
        ):
            if expected is None:
                if got is not None:
                    problems.append(f"{label} {got} should be absent")
            elif got is None:
                problems.append(f"{label} absent, expected {expected}")
            elif abs(got - expected) > tol:
                problems.append(f"{label} {got!r} != {expected!r}")
        if problems:
            failures += 1
            print(f"  FAIL  {case['name']}: {'; '.join(problems)}")
        else:
            print(f"  ok    {case['name']}")
    if failures:
        raise SystemExit(f"{failures} conformance case(s) failed")
    print("spread conformance: all cases passed")


def main():
    phase = sys.argv[1] if len(sys.argv) > 1 else "conformance"
    if phase != "conformance":
        raise SystemExit("usage: roll_estimator.py conformance")
    conformance()


if __name__ == "__main__":
    main()
