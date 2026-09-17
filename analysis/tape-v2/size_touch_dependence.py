#!/usr/bin/env python3
"""The conditional size-dependence diagnostic (authorized 2026-09-17):
does real executed size depend on the displayed touch beyond the
adopted match channel?

The adopted size channel draws the non-match arm independently of the
touch, so the mixture predicts the strictly-greater rate at touch t
exactly: P(size > t | T = t) = (1 - p(t)) * (1 - F_cdf(t)), with p the
deconvolved match table and F the deconvolved independent law from
book-config.json. The prediction is not flat in t, so the diagnostic is
the discrepancy from this curve, never the curve's slope. The equality
cells are consistent by construction (p was solved from them); the
strictly-greater curve is the first statistic the mixture predicts
without having been fitted to it.

Rules, as specified under the tenth spar's review:

- strictly-greater counts over all valid parents at the touch; observed
  equals stay in the denominator (independent draws produce equality
  too);
- the predicted rate is averaged over the real touch distribution
  within each bucket - the emitted per-integer counts are the weights -
  never computed at a representative touch;
- computed per month and phase first, summarized across months after;
- spread conditioning available (--by-spread), on the same emission;
- the pooled >30 tail is explicit: F pools its tail mass, so the
  prediction is not identified there; the tail line reports the real
  rate with no prediction and enters no summary.

A persistent residual rejects the adopted independent mixture under
matched observations. It does not identify the replacement: an
arbitrary touch-conditional F makes the match-versus-conditional-law
decomposition non-unique, so a mechanism cycle must declare how that
decomposition is identified before it becomes an intake re-derivation.

    ssh speilegg uv --directory Claude/tape-v2 run python \
        size_touch_dependence.py [--by-spread] [--min-n 200]
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
    SIZE_CAP,
    TARGETS,
    adopted_size_channel,
    book_config,
    match_bucket_of,
)

PHASES = ["all", "asia", "london", "ny_open", "ny_close"]
TAIL = f">{SIZE_CAP}"

BUCKET_TOUCHES = {b: [] for b in MATCH_BUCKETS}
for _t in range(1, SIZE_CAP + 1):
    BUCKET_TOUCHES[match_bucket_of(_t)].append(str(_t))


def predicted_gt(phase: str) -> dict[str, float]:
    """The mixture's P(size > t) at each integer touch, from the adopted
    arrays of the phase's fitted row."""
    row = book_config(phase)
    p, f = adopted_size_channel(row)
    total = sum(f.values())
    cdf = 0.0
    out = {}
    for t in range(1, SIZE_CAP + 1):
        cdf += f.get(str(t), 0.0) / total
        out[str(t)] = (1.0 - p[match_bucket_of(t)]) * (1.0 - cdf)
    return out


def month_bucket_rows(counts: dict, pred: dict, min_n: int) -> dict:
    """One month's real and predicted rate per bucket, prediction
    weighted by the month's own touch counts within the bucket."""
    rows = {}
    for bucket in MATCH_BUCKETS:
        n = gt = 0
        weighted = 0.0
        for key in BUCKET_TOUCHES[bucket]:
            cell = counts.get(key)
            if cell is None:
                continue
            n += cell["n"]
            gt += cell["gt"]
            weighted += cell["n"] * pred[key]
        if n >= min_n:
            rows[bucket] = (n, gt / n, weighted / n)
    tail = counts.get(TAIL)
    if tail is not None and tail["n"] >= min_n:
        rows[TAIL] = (tail["n"], tail["gt"] / tail["n"], float("nan"))
    return rows


def summarize(name: str, per_month: list[dict]) -> None:
    print(
        f"  {'touch':>6} {'months':>6} {'n/month':>9} "
        f"{'real':>7} {'p10':>7} {'p90':>7} {'pred':>7} "
        f"{'resid':>7} {'p10':>7} {'p90':>7}"
    )
    weights, residuals = [], []
    for bucket in [*MATCH_BUCKETS, TAIL]:
        rows = [m[bucket] for m in per_month if bucket in m]
        if not rows:
            continue
        n = np.array([r[0] for r in rows], dtype=float)
        real = np.array([r[1] for r in rows])
        pred = np.array([r[2] for r in rows])
        resid = real - pred
        line = (
            f"  {bucket:>6} {len(rows):>6} {np.median(n):>9.0f} "
            f"{np.median(real):>7.4f} {np.quantile(real, 0.1):>7.4f} "
            f"{np.quantile(real, 0.9):>7.4f}"
        )
        if bucket == TAIL:
            line += f" {'n/a':>7} {'n/a':>7} {'n/a':>7} {'n/a':>7}"
        else:
            line += (
                f" {np.median(pred):>7.4f} {np.median(resid):>+7.4f} "
                f"{np.quantile(resid, 0.1):>+7.4f} {np.quantile(resid, 0.9):>+7.4f}"
            )
            weights.append(n.sum())
            residuals.append(float(np.median(resid)))
        print(line)
    if weights:
        w = np.array(weights) / np.sum(weights)
        r = np.array(residuals)
        print(
            f"  {name}: touch-weighted median residual "
            f"{float(np.sum(w * r)):+.4f}, weighted mean abs "
            f"{float(np.sum(w * np.abs(r))):.4f}"
        )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--by-spread", action="store_true")
    ap.add_argument("--min-n", type=int, default=200)
    args = ap.parse_args()
    blocks = json.loads(TARGETS.read_text())["blocks"]
    groups = ["1", "2", "3+"] if args.by_spread else ["all"]
    missing = 0
    for phase in PHASES:
        pred = predicted_gt(phase)
        for group in groups:
            per_month = []
            for month in sorted(blocks):
                block = blocks[month].get(phase)
                if block is None:
                    continue
                emitted = block["sweep"].get("touch", {}).get("size_gt_by_touch")
                if emitted is None:
                    missing += 1
                    continue
                per_month.append(
                    month_bucket_rows(emitted[group], pred, args.min_n)
                )
            if not any(per_month):
                continue
            title = phase if group == "all" else f"{phase} spread {group}"
            print(f"== {title}")
            summarize(title, per_month)
    if missing:
        raise SystemExit(
            f"{missing} phase-month blocks lack size_gt_by_touch: rerun "
            "the real micro-stats pass with the current extractor"
        )


if __name__ == "__main__":
    main()
