#!/usr/bin/env python3
"""The deconvolution regression gate: an executable verdict, exit
nonzero on failure.

Reads only the committed fixture, so it runs on any clone without the
gitignored real targets. Three checks:

- Every phase block's derivation reproduces: `deconvolve_sizes` on the
  block's own committed inputs (with the recorded fallback buckets
  fixed to the pooled p inside the joint solve) must equal the
  expected p and F.
- The reconstruction gate: the expected p and F, pushed back through
  the mixture with the block's touch pmf, must reproduce the observed
  match-by-touch and size marginal within the declared tolerances.
  Without fallback this is the fixed point's identity; under fallback
  it is the check that the fixture's constraint still tells the truth
  about the phase it constrains.
- The rejecting case must be refused.

    python3 deconvolution_check.py
"""

from __future__ import annotations

import sys
from pathlib import Path

import json

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_book import (  # noqa: E402
    MATCH_BUCKETS,
    SIZE_CAP,
    SIZE_SUPPORT,
    deconvolve_sizes,
    match_bucket_of,
)

FIXTURE = HERE / "deconvolution-fixture.json"
# Reproduction is the same code on the same inputs: exact up to float
# noise from json round-tripping.
REPRODUCE_TOL = 1e-9
# The declared reconstruction tolerances (rules.thin_bucket): the
# fallback may move a bucket's reconstructed match rate at most this
# far from the phase's observation, and the marginal at most this much
# total variation.
MATCH_TOL = 0.02
MARGINAL_TV_TOL = 0.01


def reconstruct(p: dict, f: dict, touch_pmf: dict) -> tuple[dict, dict]:
    """The mixture forward: observed match-by-touch and size marginal
    from the behavioral p, the independent law and the touch pmf."""
    tail = f">{SIZE_CAP}"
    bucket_touches: dict = {b: [] for b in MATCH_BUCKETS}
    for t in range(1, SIZE_CAP + 1):
        bucket_touches[match_bucket_of(t)].append(str(t))
    bucket_touches["11+"].append(tail)
    match = {}
    for bucket in MATCH_BUCKETS:
        masses = [(key, touch_pmf.get(key, 0.0)) for key in bucket_touches[bucket]]
        total = sum(w for _k, w in masses)
        if total <= 0.0:
            continue
        coincidence = (
            sum(w * (0.0 if key == tail else f.get(key, 0.0)) for key, w in masses) / total
        )
        pb = p.get(bucket, 0.0)
        match[bucket] = pb + (1.0 - pb) * coincidence
    matched_at = {
        key: touch_pmf.get(key, 0.0)
        * p.get("11+" if key == tail else match_bucket_of(int(key)), 0.0)
        for key in SIZE_SUPPORT
    }
    matched_total = sum(matched_at.values())
    marginal = {
        key: matched_at[key] + (1.0 - matched_total) * f.get(key, 0.0)
        for key in SIZE_SUPPORT
    }
    return match, marginal


def main() -> int:
    fixture = json.loads(FIXTURE.read_text())
    failures = []
    pooled_p = fixture["phases"]["all"]["expected_p"]
    for phase, block in fixture["phases"].items():
        fixed = {b: pooled_p[b] for b in block["pooled_fallback_buckets"]}
        try:
            p, f = deconvolve_sizes(block["input"], fixed_p=fixed or None)
        except SystemExit as err:
            failures.append(f"{phase}: derivation refused: {err}")
            continue
        for bucket, expected in block["expected_p"].items():
            if abs(p.get(bucket, 0.0) - expected) > REPRODUCE_TOL:
                failures.append(
                    f"{phase}: p[{bucket}] {p.get(bucket, 0.0)} != expected {expected}"
                )
        for key, expected in block["expected_f"].items():
            if abs(f.get(key, 0.0) - expected) > REPRODUCE_TOL:
                failures.append(f"{phase}: F[{key}] {f.get(key, 0.0)} != expected {expected}")
        match, marginal = reconstruct(
            block["expected_p"], block["expected_f"], block["input"]["touch_pmf_full"]
        )
        for bucket, observed in block["input"]["match_by_touch"].items():
            if abs(match.get(bucket, 0.0) - observed) > MATCH_TOL:
                failures.append(
                    f"{phase}: reconstructed match[{bucket}] "
                    f"{match.get(bucket, 0.0):.4f} vs observed {observed:.4f}"
                )
        tv = 0.5 * sum(
            abs(marginal[k] - block["input"]["size_pmf_full"].get(k, 0.0))
            for k in SIZE_SUPPORT
        )
        if tv > MARGINAL_TV_TOL:
            failures.append(f"{phase}: reconstructed marginal TV {tv:.5f}")
    try:
        deconvolve_sizes(fixture["rejecting_case"])
        failures.append("the rejecting case was not refused")
    except SystemExit:
        pass
    for line in failures:
        print(f"FAIL {line}")
    if failures:
        return 1
    print(f"deconvolution check: {len(fixture['phases'])} phases reproduce, "
          "reconstruction inside tolerance, rejecting case refused")
    return 0


if __name__ == "__main__":
    sys.exit(main())
