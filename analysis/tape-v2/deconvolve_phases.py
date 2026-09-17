#!/usr/bin/env python3
"""Per-phase deconvolution with the identifiability evidence beside it.

For each fitted phase (and the pooled block) this derives the
touch-conditional match table p(t) and the independent size law F from
the phase's median-month inputs, and prints the evidence the fourth
spar's identifiability gate asks for: per-bucket touch observation
counts by month, and the per-month deconvolved p(t) spread - the joint
inputs propagated through the derivation, not the raw match rate, since
stable observed matching can conceal unstable inferred matching when
the coincidence term moves.

The declared thin-bucket rule (see `THIN_FLOOR`): a bucket whose median
monthly observation count falls below the floor takes the pooled p
inside the joint solve (`deconvolve_sizes(..., fixed_p=...)`), and the
phase F is recomputed under that fixture - never patched after solving,
which would break the mixture.

    python3 deconvolve_phases.py                  # the evidence table
    python3 deconvolve_phases.py --adopt          # print adopted rows as JSON
    python3 deconvolve_phases.py --write-config   # adopt into book-config.json
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_book import (  # noqa: E402
    MATCH_BUCKETS,
    SIZE_SUPPORT,
    TARGETS,
    deconvolve_sizes,
    match_bucket_of,
    real_medians,
)

PHASES = ["all", "asia", "london", "ny_open", "ny_close"]
BOOK_CONFIG = HERE / "book-config.json"

# The identifiability floor, in touch observations per month (median
# month): below it a bucket's monthly p is dominated by sampling noise
# and the phase inherits the pooled behavioral probability instead.
# Measured 2026-09-16: the thinnest bucket anywhere (london 11+) never
# falls under 5900 in any month, so no fallback fires at this floor.
THIN_FLOOR = 2000

# The adopted depth-ratio ladder: the exercised prefix of the fitted
# mbp-10 profile - the prototype walks ratios[: max_levels - 1] at
# max_levels 8, so only these seven entered any fit or gate. The
# profile's trailing two fitted values, 3.33 and 3.33 (levels 8 and 9),
# are retained here for the owed 8-versus-10 display-bound replay,
# which must replay identical requested parents through both bounds.
ADOPTED_RATIOS = [2.0, 2.4, 2.5, 2.83, 3.0, 3.0, 3.33]
UNEXERCISED_TAIL = [3.33, 3.33]


def bucket_counts(block: dict) -> dict[str, float]:
    """Touch observations per match bucket: the trichotomy denominator
    times the touch mass falling in the bucket."""
    touch = block["sweep"]["touch"]
    n = float(touch["n"])
    masses: dict[str, float] = {b: 0.0 for b in MATCH_BUCKETS}
    for key, mass in touch["touch_pmf_full"].items():
        bucket = "11+" if key.startswith(">") else match_bucket_of(int(key))
        masses[bucket] += float(mass)
    return {b: n * masses[b] for b in MATCH_BUCKETS}


def monthly_p(blocks: dict, phase: str) -> dict[str, list[float]]:
    """The deconvolved p(t) per month - the joint size, touch and match
    inputs propagated through the derivation."""
    spread: dict[str, list[float]] = {b: [] for b in MATCH_BUCKETS}
    for month in sorted(blocks):
        block = blocks[month].get(phase)
        if block is None:
            continue
        real = real_medians(phase, month=month)
        try:
            p, _f = deconvolve_sizes(real)
        except SystemExit:
            continue
        for bucket in MATCH_BUCKETS:
            if bucket in p:
                spread[bucket].append(p[bucket])
    return spread


def adopt(phase: str, pooled_p: dict, blocks: dict) -> tuple[dict, dict, list[str]]:
    """The adopted row for a phase: thin buckets fixed to the pooled p
    inside the joint solve, F recomputed under that fixture."""
    counts = [bucket_counts(blocks[m][phase]) for m in sorted(blocks) if phase in blocks[m]]
    thin = [
        bucket
        for bucket in MATCH_BUCKETS
        if float(np.median([c[bucket] for c in counts])) < THIN_FLOOR
    ]
    fixed = {bucket: pooled_p[bucket] for bucket in thin}
    p, f = deconvolve_sizes(real_medians(phase), fixed_p=fixed)
    return p, f, thin


def adopted_rows(blocks: dict) -> dict:
    pooled_p, _pooled_f = deconvolve_sizes(real_medians("all"))
    out = {}
    for phase in PHASES:
        if phase == "all":
            p, f = deconvolve_sizes(real_medians("all"))
            thin: list[str] = []
        else:
            p, f, thin = adopt(phase, pooled_p, blocks)
        out[phase] = {
            "p_match": [p.get(b, 0.0) for b in MATCH_BUCKETS],
            "size_law": [f[k] for k in SIZE_SUPPORT],
            "pooled_fallback_buckets": thin,
        }
    return out


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--adopt", action="store_true")
    ap.add_argument("--write-config", action="store_true")
    args = ap.parse_args()
    blocks = json.loads(TARGETS.read_text())["blocks"]
    pooled_p, _pooled_f = deconvolve_sizes(real_medians("all"))

    if args.adopt or args.write_config:
        rows = adopted_rows(blocks)
        if args.adopt:
            print(json.dumps(rows, indent=1))
            return
        cfg = json.loads(BOOK_CONFIG.read_text())
        cfg["base"]["depth_ratios"] = ADOPTED_RATIOS
        cfg["base"]["p_match"] = rows["all"]["p_match"]
        cfg["base"]["size_law"] = rows["all"]["size_law"]
        for phase in PHASES:
            if phase == "all":
                continue
            cfg["phases"].setdefault(phase, {})
            cfg["phases"][phase]["p_match"] = rows[phase]["p_match"]
            cfg["phases"][phase]["size_law"] = rows[phase]["size_law"]
        cfg["deconvolution"] = {
            "comment": "The touch-conditional match table and independent "
            "size law, deconvolved per phase by deconvolve_phases.py from "
            "the per-month integer pmfs and match-by-touch in the real "
            "targets; the derivation contract and expected outputs are "
            "deconvolution-fixture.json. Thin buckets (median monthly "
            "observations under thin_floor) would take the pooled p inside "
            "the joint solve; none fired at the recorded floor.",
            "thin_floor": THIN_FLOOR,
            "pooled_fallback_buckets": {
                phase: rows[phase]["pooled_fallback_buckets"]
                for phase in PHASES
                if phase != "all"
            },
            "depth_ratio_tail_unexercised": UNEXERCISED_TAIL,
        }
        BOOK_CONFIG.write_text(json.dumps(cfg, indent=1) + "\n")
        print(f"wrote {BOOK_CONFIG}")
        return

    for phase in PHASES:
        print(f"== {phase}")
        counts = [
            bucket_counts(blocks[m][phase]) for m in sorted(blocks) if phase in blocks[m]
        ]
        spread = monthly_p(blocks, phase)
        p, _f = deconvolve_sizes(real_medians(phase))
        for bucket in MATCH_BUCKETS:
            per_month = [c[bucket] for c in counts]
            values = spread[bucket]
            lo, hi = (
                (float(np.percentile(values, 10)), float(np.percentile(values, 90)))
                if values
                else (float("nan"), float("nan"))
            )
            flag = " THIN" if float(np.median(per_month)) < THIN_FLOOR else ""
            print(
                f"  {bucket:>5}  n/month p50 {np.median(per_month):>9.0f}  "
                f"min {min(per_month):>8.0f}  p {p.get(bucket, float('nan')):.3f}  "
                f"monthly p10-p90 {lo:.3f}-{hi:.3f}  months {len(values)}{flag}"
            )


if __name__ == "__main__":
    main()
