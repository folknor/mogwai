#!/usr/bin/env python3
"""Write the shared deconvolution fixture the derivation is pinned by.

The deconvolution is implemented once, in Python (`deconvolve_sizes`);
the engine consumes its outputs as preset data rather than porting the
solve, so there is no second implementation to drift. What the fixture
pins instead is the artifact chain: `deconvolution_check.py` re-runs
the derivation from the fixture's own committed inputs and fails on
any divergence from the expected outputs, and a Rust test asserts the
committed preset rows equal these expected values, so neither a moved
derivation nor a transcription typo can land silently.

The fixture carries, per phase (pooled plus the four fitted phases):
the median-month inputs, the expected p(t) and F to full float
precision, and the identifiability evidence the fourth spar's gate
asks for - per-bucket touch observation counts by month and the
per-month deconvolved p spread (the joint inputs propagated through
the derivation, since a stable observed match rate can conceal an
unstable inferred p when the coincidence term moves). Beside them: the
declared rules (integer support, tail atom, coincidence convention,
normalization, fixed-point tolerance, the thin-bucket fallback), and a
crafted rejecting case whose matched mass exceeds the marginal at one
size, which the implementation must refuse.

    ssh speilegg uv --directory Claude/tape-v2 run python make_deconvolution_fixture.py

The output is committed at analysis/tape-v2/deconvolution-fixture.json.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from deconvolve_phases import (  # noqa: E402
    PHASES,
    THIN_FLOOR,
    adopt,
    bucket_counts,
    monthly_p,
)
from proto_book import (  # noqa: E402
    MATCH_BUCKETS,
    SIZE_CAP,
    TAIL_REP,
    TARGETS,
    deconvolve_sizes,
    real_medians,
)

OUT = HERE / "deconvolution-fixture.json"


def main() -> None:
    blocks = json.loads(TARGETS.read_text())["blocks"]
    pooled_p, _pooled_f = deconvolve_sizes(real_medians("all"))
    phases = {}
    for phase in PHASES:
        real = real_medians(phase)
        if phase == "all":
            p, f = deconvolve_sizes(real)
            thin: list[str] = []
        else:
            p, f, thin = adopt(phase, pooled_p, blocks)
        counts = [
            bucket_counts(blocks[m][phase]) for m in sorted(blocks) if phase in blocks[m]
        ]
        spread = monthly_p(blocks, phase)
        phases[phase] = {
            "input": {
                "size_pmf_full": real["size_pmf_full"],
                "touch_pmf_full": real["touch_pmf_full"],
                "match_by_touch": real["match_by_touch"],
            },
            "expected_p": p,
            "expected_f": f,
            "pooled_fallback_buckets": thin,
            "evidence": {
                bucket: {
                    "observations_per_month_p50": float(
                        np.median([c[bucket] for c in counts])
                    ),
                    "observations_per_month_min": float(min(c[bucket] for c in counts)),
                    "monthly_p_p10": float(np.percentile(spread[bucket], 10)),
                    "monthly_p_p90": float(np.percentile(spread[bucket], 90)),
                    "months": len(spread[bucket]),
                }
                for bucket in MATCH_BUCKETS
            },
        }
    rejecting = {
        "size_pmf_full": {"1": 0.799, "2": 0.001, "3": 0.2},
        "touch_pmf_full": {"1": 0.3, "2": 0.5, "3": 0.2},
        "match_by_touch": {"1": 0.8, "2": 0.9, "3": 0.3},
    }
    fixture = {
        "version": 2,
        "rules": {
            "support": f"integer masses at 1..{SIZE_CAP}, tail pooled as >{SIZE_CAP}",
            "tail_representative": TAIL_REP,
            "coincidence": "touch-mass-weighted F at each integer touch in the "
            "match bucket; the tail atom's coincidence is declared zero",
            "match_buckets": "1,2,3,4,5 individually, 6-10, 11+ (tail included)",
            "matched_mass": "subtracted at each integer size, touch mass times "
            "the bucket's p",
            "normalization": "F renormalized to one every round",
            "fixed_point": "iterate until max per-key delta under 1e-10, at "
            "most 80 rounds, else reject",
            "rejection": "any F mass under -1e-9 before clamping rejects the "
            "mixture; clamp only within that tolerance",
            "thin_bucket": "a bucket whose median monthly touch observations "
            f"fall under {THIN_FLOOR} takes the pooled p fixed inside the "
            "joint solve and the phase F is recomputed under it; the "
            "original phase observations stay recorded here so the fallback "
            "cannot validate itself",
        },
        # The pooled block's inputs and outputs, kept at the top level so
        # version-1 consumers of the pooled derivation read unchanged.
        "input": phases["all"]["input"],
        "expected_p": phases["all"]["expected_p"],
        "expected_f": phases["all"]["expected_f"],
        "phases": phases,
        "rejecting_case": rejecting,
    }
    OUT.write_text(json.dumps(fixture, indent=1))
    print(f"wrote {OUT}")
    try:
        deconvolve_sizes(
            {
                "size_pmf_full": rejecting["size_pmf_full"],
                "touch_pmf_full": rejecting["touch_pmf_full"],
                "match_by_touch": rejecting["match_by_touch"],
            }
        )
    except SystemExit as err:
        print(f"rejecting case refused as required: {err}")
        return
    raise SystemExit("the rejecting case was not refused")


if __name__ == "__main__":
    main()
