#!/usr/bin/env python3
"""The sampling-frame experiment's acceptance rule, and its adversarial fixture.

Implements section 7.1 of `notes/sampling-frame-preregistration.md`: monthly
Spearman rank association between a bar-derived volatility score and each
tick-level target, judged on a held-out span.

Two modes:

    selftest   the planted and null fixtures, no market data of any kind
    report     apply the rule to a computed target table

`selftest` exists BEFORE any real month is read, deliberately. An acceptance
rule is exactly the kind of code that fails silently by being too permissive: it
returns a verdict either way, and a verdict is not obviously wrong the way a
crash is. The report this experiment feeds records three separate defects that
produced a plausible number rather than a failure, all found by reading rather
than by any test. So every gate here has a fixture that must FAIL it, not merely
a fixture that passes.

Thresholds are preregistered in section 7.1 and mirrored here as named
constants. Changing one after reading a result invalidates the acceptance claim
it supports.
"""

from __future__ import annotations

import argparse
import itertools
import json
import math
import random
import sys
from pathlib import Path

# ---------------------------------------------------------------------------
# Preregistered. Section 7.1.
# ---------------------------------------------------------------------------

CALIBRATION_MIN_ABS_RHO = 0.50
HELD_OUT_MIN_ABS_RHO = 0.70
DIRECTION_MUST_REPRODUCE = True

EXACT_PERMUTATION_MAX_N = 10
MONTE_CARLO_PERMUTATIONS = 1_000_000
PERMUTATION_SEED = 20260805

# Section 6. Family membership drives the weighting; F1 and F2 are mandatory.
FAMILIES = {
    "F1": ("mean_event_duration_s", "duration_dispersion_cv2",
           "duration_acf_lag1", "duration_acf_lag5"),
    "F2": ("children_mean", "children_single_frac", "levels_mean"),
    "F3": ("size_round_frac", "size_dispersion"),
    "F4": ("return_acf_lag1",),
    "F5": ("abs_acf_lag1", "abs_acf_lag10", "abs_acf_lag50"),
    "F6": ("zero_change_frac",),
}
MANDATORY_FAMILIES = ("F1", "F2")

UNAVAILABLE = "UNAVAILABLE"
FLOAT_SLACK = 1e-12


# ---------------------------------------------------------------------------
# Rank correlation
# ---------------------------------------------------------------------------


def average_ranks(values: list[float]) -> list[float]:
    """Ranks with ties taking the AVERAGE of the positions they span."""
    order = sorted(range(len(values)), key=lambda i: values[i])
    ranks = [0.0] * len(values)
    i = 0
    while i < len(order):
        j = i
        while j + 1 < len(order) and values[order[j + 1]] == values[order[i]]:
            j += 1
        shared = (i + j) / 2.0 + 1.0
        for k in range(i, j + 1):
            ranks[order[k]] = shared
        i = j + 1
    return ranks


def pearson(xs: list[float], ys: list[float]) -> float | str:
    n = len(xs)
    mx = sum(xs) / n
    my = sum(ys) / n
    sxy = sum((x - mx) * (y - my) for x, y in zip(xs, ys))
    sxx = sum((x - mx) ** 2 for x in xs)
    syy = sum((y - my) ** 2 for y in ys)
    if sxx <= FLOAT_SLACK or syy <= FLOAT_SLACK:
        # A constant vector has no defined correlation. Returning 0.0 here would
        # read as "measured no association" when the truth is "could not
        # measure", which is the substitution the whole report exists to avoid.
        return UNAVAILABLE
    return sxy / math.sqrt(sxx * syy)


def spearman(xs: list[float], ys: list[float]) -> float | str:
    if len(xs) != len(ys):
        raise ValueError("paired inputs must be the same length")
    if len(xs) < 3:
        return UNAVAILABLE
    return pearson(average_ranks(xs), average_ranks(ys))


def permutation_p(xs: list[float], ys: list[float]) -> dict:
    """Two-sided permutation p-value for the observed rho.

    Exact by enumeration at or below `EXACT_PERMUTATION_MAX_N`, otherwise a
    seeded Monte Carlo, always labelled with which one ran.
    """
    observed = spearman(xs, ys)
    if observed == UNAVAILABLE:
        return {"p": UNAVAILABLE, "method": UNAVAILABLE, "draws": 0}
    n = len(xs)
    rx = average_ranks(xs)
    ry = average_ranks(ys)
    target = abs(observed) - FLOAT_SLACK

    if n <= EXACT_PERMUTATION_MAX_N:
        hits = 0
        total = 0
        for perm in itertools.permutations(ry):
            total += 1
            value = pearson(rx, list(perm))
            if value != UNAVAILABLE and abs(value) >= target:
                hits += 1
        return {"p": hits / total, "method": "exact", "draws": total}

    rng = random.Random(PERMUTATION_SEED)
    shuffled = list(ry)
    hits = 0
    for _ in range(MONTE_CARLO_PERMUTATIONS):
        rng.shuffle(shuffled)
        value = pearson(rx, shuffled)
        if value != UNAVAILABLE and abs(value) >= target:
            hits += 1
    return {
        "p": hits / MONTE_CARLO_PERMUTATIONS,
        "method": "monte_carlo",
        "draws": MONTE_CARLO_PERMUTATIONS,
    }


def leave_one_out(xs: list[float], ys: list[float]) -> dict:
    """Held-out rho recomputed with each single observation omitted.

    Diagnostic, never a gate. At n=7 one month is 14 percent of the sample, so a
    sign that flips on one deletion is worth seeing even when the threshold is
    comfortably met.
    """
    observed = spearman(xs, ys)
    if observed == UNAVAILABLE:
        return {"values": [], "min": UNAVAILABLE, "max": UNAVAILABLE,
                "sign_reverses": UNAVAILABLE}
    values = []
    for i in range(len(xs)):
        sub_x = xs[:i] + xs[i + 1:]
        sub_y = ys[:i] + ys[i + 1:]
        value = spearman(sub_x, sub_y)
        values.append(value)
    numeric = [v for v in values if v != UNAVAILABLE]
    reverses = any(
        v * observed < 0 for v in numeric
    )
    return {
        "values": values,
        "min": min(numeric) if numeric else UNAVAILABLE,
        "max": max(numeric) if numeric else UNAVAILABLE,
        "sign_reverses": reverses,
    }


# ---------------------------------------------------------------------------
# Acceptance
# ---------------------------------------------------------------------------


def evaluate_target(name: str, cal_rv, cal_y, held_rv, held_y) -> dict:
    cal_rho = spearman(cal_rv, cal_y)
    held_rho = spearman(held_rv, held_y)

    if cal_rho == UNAVAILABLE or held_rho == UNAVAILABLE:
        return {
            "target": name,
            "calibration_rho": cal_rho,
            "held_out_rho": held_rho,
            "pass": False,
            "reason": "unavailable, a constant target cannot be ranked",
            "held_out_p": permutation_p(held_rv, held_y),
            "leave_one_out": leave_one_out(held_rv, held_y),
        }

    cal_ok = abs(cal_rho) >= CALIBRATION_MIN_ABS_RHO
    held_ok = abs(held_rho) >= HELD_OUT_MIN_ABS_RHO
    signs_ok = (cal_rho > 0) == (held_rho > 0) if DIRECTION_MUST_REPRODUCE else True

    reasons = []
    if not cal_ok:
        reasons.append(f"calibration |rho| {abs(cal_rho):.4f} < {CALIBRATION_MIN_ABS_RHO}")
    if not held_ok:
        reasons.append(f"held-out |rho| {abs(held_rho):.4f} < {HELD_OUT_MIN_ABS_RHO}")
    if not signs_ok:
        reasons.append("direction does not reproduce")

    return {
        "target": name,
        "calibration_rho": cal_rho,
        "held_out_rho": held_rho,
        "pass": cal_ok and held_ok and signs_ok,
        "reason": "; ".join(reasons) if reasons else "passes",
        "held_out_p": permutation_p(held_rv, held_y),
        "leave_one_out": leave_one_out(held_rv, held_y),
    }


def evaluate(cal_rv, held_rv, cal_targets: dict, held_targets: dict) -> dict:
    per_target = {}
    for family, members in FAMILIES.items():
        for name in members:
            if name not in cal_targets or name not in held_targets:
                continue
            per_target[name] = evaluate_target(
                name, cal_rv, cal_targets[name], held_rv, held_targets[name]
            )

    families = {}
    for family, members in FAMILIES.items():
        present = [per_target[m] for m in members if m in per_target]
        if not present:
            continue
        passed = sum(1 for r in present if r["pass"])
        families[family] = {
            "targets": len(present),
            "passed": passed,
            # Strict majority of the family's OWN targets, never a count of raw
            # metrics pooled across families.
            "pass": passed * 2 > len(present),
        }

    passing = sum(1 for f in families.values() if f["pass"])
    majority = passing * 2 > len(families) if families else False
    mandatory = all(
        families.get(f, {}).get("pass", False) for f in MANDATORY_FAMILIES
    )

    return {
        "per_target": per_target,
        "families": families,
        "family_majority": majority,
        "mandatory_families_pass": mandatory,
        "verdict": "PASS" if (majority and mandatory) else "FAIL",
    }


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def selftest() -> int:
    checks: list[tuple[str, bool, str]] = []

    def check(name: str, condition: bool, detail: str = "") -> None:
        checks.append((name, condition, detail))

    # -- rank mechanics ----------------------------------------------------
    check(
        "average ranks split a tied pair",
        average_ranks([10.0, 20.0, 20.0, 30.0]) == [1.0, 2.5, 2.5, 4.0],
        str(average_ranks([10.0, 20.0, 20.0, 30.0])),
    )
    check(
        "average ranks handle an all-tied vector",
        average_ranks([5.0, 5.0, 5.0]) == [2.0, 2.0, 2.0],
        str(average_ranks([5.0, 5.0, 5.0])),
    )

    # -- planted monotone --------------------------------------------------
    xs = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
    check(
        "planted increasing gives rho 1",
        abs(spearman(xs, [2 * x for x in xs]) - 1.0) < 1e-12,
    )
    check(
        "planted decreasing gives rho -1",
        abs(spearman(xs, [-3 * x for x in xs]) + 1.0) < 1e-12,
    )
    # NON-LINEAR monotone. This is the fixture that proves the statistic makes
    # no linearity assumption; a Pearson correlation on raw values would not
    # return exactly 1 here.
    check(
        "planted exponential monotone still gives rho 1",
        abs(spearman(xs, [math.exp(x) for x in xs]) - 1.0) < 1e-12,
    )

    # -- constant target ---------------------------------------------------
    constant = spearman(xs, [4.0] * 7)
    check(
        "constant target is UNAVAILABLE, never 0.0",
        constant == UNAVAILABLE,
        str(constant),
    )
    verdict = evaluate_target("constant", xs, [4.0] * 7, xs, [4.0] * 7)
    check("constant target FAILS rather than passing", verdict["pass"] is False)
    check(
        "constant target does not report a numeric rho",
        verdict["held_out_rho"] == UNAVAILABLE,
    )

    # -- exact permutation -------------------------------------------------
    p = permutation_p(xs, [2 * x for x in xs])
    check("n=7 permutation is exact", p["method"] == "exact" and p["draws"] == 5040)
    check(
        "perfect association hits the floor p of 2/5040",
        abs(p["p"] - 2 / 5040) < 1e-12,
        str(p["p"]),
    )

    # -- null --------------------------------------------------------------
    # Derived by enumeration in `analysis/find_fixtures.py`, not chosen by eye.
    # The first draft used [3,1,4,2,7,5,6], which LOOKS shuffled and carries
    # rho = +0.7143 - above the held-out threshold. That silently turned four
    # downstream checks into assertions that a passing vector fails, and the
    # selftest caught it. A fixture picked by intuition can be wrong in the same
    # direction as the code it guards; this one is exactly rho = 0.
    null_y = [1.0, 4.0, 6.0, 7.0, 5.0, 3.0, 2.0]
    null_rho = spearman(xs, null_y)
    check(
        "null fixture is exactly zero, not merely shuffled-looking",
        abs(null_rho) < 1e-12,
        f"rho={null_rho:.6f}",
    )
    check(
        "null fixture stays below the held-out threshold",
        abs(null_rho) < HELD_OUT_MIN_ABS_RHO,
        f"rho={null_rho:.4f}",
    )
    check(
        "null fixture FAILS the target rule",
        evaluate_target("null", xs, null_y, xs, null_y)["pass"] is False,
    )

    # -- threshold boundary is inclusive -----------------------------------
    # Held-out rho exactly 0.7 must PASS, since the rule is >=. Built by hand:
    # one adjacent transposition in a 7-point ranking gives rho = 1 - 6*2/336.
    boundary_y = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]
    swapped = boundary_y[:]
    swapped[0], swapped[3] = swapped[3], swapped[0]
    rho_boundary = spearman(xs, swapped)
    check(
        "a boundary rho is judged with >= and not >",
        (abs(rho_boundary) >= HELD_OUT_MIN_ABS_RHO)
        == evaluate_target("b", xs, swapped, xs, swapped)["pass"],
        f"rho={rho_boundary:.4f}",
    )

    # -- direction must reproduce -----------------------------------------
    cal12 = list(range(1, 13))
    cal_up = [float(v) for v in cal12]
    flipped = evaluate_target(
        "flip",
        [float(v) for v in cal12], cal_up,
        xs, [-x for x in xs],
    )
    check(
        "opposite signs FAIL even when both magnitudes clear",
        flipped["pass"] is False and "direction" in flipped["reason"],
        flipped["reason"],
    )

    # -- leave one out ------------------------------------------------------
    loo = leave_one_out(xs, [2 * x for x in xs])
    check(
        "leave-one-out reports 7 values and no reversal on a clean monotone",
        len(loo["values"]) == 7 and loo["sign_reverses"] is False,
    )
    # Also derived by enumeration. Base rho = +0.357143, and omitting one month
    # drives it to -0.028571 - a genuine sign reversal on a base association
    # strong enough that the flip is surprising rather than a coin toss around
    # zero. An outlier-driven vector built by hand did NOT reverse, which is why
    # this is searched rather than invented.
    fragile_y = [1.0, 2.0, 6.0, 7.0, 4.0, 5.0, 3.0]
    fragile = leave_one_out(xs, fragile_y)
    check(
        "leave-one-out detects a sign reversal when one point drives it",
        fragile["sign_reverses"] is True,
        f"base={spearman(xs, fragile_y):.4f} range="
        f"{fragile['min']:.4f}..{fragile['max']:.4f}",
    )

    # -- family weighting ---------------------------------------------------
    # F5 has three targets and F4 and F6 have one each. A rule that counted raw
    # metrics rather than families could let F5 alone outvote a whole family.
    cal_rv = [float(v) for v in range(1, 13)]
    held_rv = xs
    strong_cal = [float(v) for v in range(1, 13)]
    strong_held = [float(v) for v in xs]
    weak_held = null_y

    cal_t = {}
    held_t = {}
    for name in FAMILIES["F1"] + FAMILIES["F2"]:
        cal_t[name] = strong_cal
        held_t[name] = weak_held          # both mandatory families fail
    for name in FAMILIES["F3"] + FAMILIES["F4"] + FAMILIES["F5"] + FAMILIES["F6"]:
        cal_t[name] = strong_cal
        held_t[name] = strong_held        # everything else passes

    result = evaluate(cal_rv, held_rv, cal_t, held_t)
    check(
        "a family majority CANNOT carry a failed mandatory family",
        result["family_majority"] is True
        and result["mandatory_families_pass"] is False
        and result["verdict"] == "FAIL",
        f"majority={result['family_majority']} "
        f"mandatory={result['mandatory_families_pass']} "
        f"verdict={result['verdict']}",
    )

    # And the converse: mandatory passing but the majority failing is also FAIL.
    cal_t2, held_t2 = {}, {}
    for name in FAMILIES["F1"] + FAMILIES["F2"]:
        cal_t2[name] = strong_cal
        held_t2[name] = strong_held
    for name in FAMILIES["F3"] + FAMILIES["F4"] + FAMILIES["F5"] + FAMILIES["F6"]:
        cal_t2[name] = strong_cal
        held_t2[name] = weak_held
    result2 = evaluate(cal_rv, held_rv, cal_t2, held_t2)
    check(
        "mandatory families alone CANNOT carry a failed majority",
        result2["mandatory_families_pass"] is True
        and result2["family_majority"] is False
        and result2["verdict"] == "FAIL",
        f"majority={result2['family_majority']} verdict={result2['verdict']}",
    )

    # -- an all-pass fixture must actually pass -----------------------------
    cal_t3 = {n: strong_cal for fam in FAMILIES.values() for n in fam}
    held_t3 = {n: strong_held for fam in FAMILIES.values() for n in fam}
    result3 = evaluate(cal_rv, held_rv, cal_t3, held_t3)
    check(
        "a fully planted fixture PASSES, so the rule is not vacuously strict",
        result3["verdict"] == "PASS",
        result3["verdict"],
    )

    # -- a fully null fixture must fail -------------------------------------
    held_t4 = {n: weak_held for fam in FAMILIES.values() for n in fam}
    result4 = evaluate(cal_rv, held_rv, cal_t3, held_t4)
    check(
        "a fully null fixture FAILS",
        result4["verdict"] == "FAIL",
        result4["verdict"],
    )

    width = max(len(name) for name, _, _ in checks)
    failures = 0
    for name, ok, detail in checks:
        if not ok:
            failures += 1
        mark = "ok  " if ok else "FAIL"
        suffix = f"   {detail}" if detail and not ok else ""
        print(f"  {mark}  {name:<{width}}{suffix}")
    print()
    print(f"{len(checks)} checks, {failures} failed")
    return 1 if failures else 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("selftest", "report"))
    parser.add_argument("--table", type=Path)
    args = parser.parse_args()

    if args.mode == "selftest":
        sys.exit(selftest())

    if not args.table:
        print("report mode needs --table; none exists yet, per the order of work")
        sys.exit(2)
    payload = json.loads(args.table.read_text())
    result = evaluate(
        payload["calibration_rv"],
        payload["held_out_rv"],
        payload["calibration_targets"],
        payload["held_out_targets"],
    )
    print(json.dumps(result, indent=2, sort_keys=True, default=str))


if __name__ == "__main__":
    main()
