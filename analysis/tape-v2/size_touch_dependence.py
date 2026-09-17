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
    SIZE_SUPPORT,
    TARGETS,
    adopted_size_channel,
    book_config,
    deconvolve_sizes,
    match_bucket_of,
)

PHASES = ["all", "asia", "london", "ny_open", "ny_close"]
TAIL = f">{SIZE_CAP}"

BUCKET_TOUCHES = {b: [] for b in MATCH_BUCKETS}
for _t in range(1, SIZE_CAP + 1):
    BUCKET_TOUCHES[match_bucket_of(_t)].append(str(_t))


def predicted_from(p: dict, f: dict) -> dict[str, float]:
    """A mixture's P(size > t) at each integer touch, from a match
    table and an independent law. A bucket without an identified match
    probability predicts with p zero, the conservative side for a
    strictly-greater rate; the caller flags such buckets as thin."""
    total = sum(f.values())
    cdf = 0.0
    out = {}
    for t in range(1, SIZE_CAP + 1):
        cdf += f.get(str(t), 0.0) / total
        out[str(t)] = (1.0 - p.get(match_bucket_of(t), 0.0)) * (1.0 - cdf)
    return out


def predicted_gt(phase: str) -> dict[str, float]:
    """The adopted mixture's curve, from the phase's fitted row."""
    row = book_config(phase)
    p, f = adopted_size_channel(row)
    return predicted_from(p, f)


def state_medians(blocks: dict, phase: str, state: str) -> dict | None:
    """The per-state deconvolution inputs in the real-medians shape:
    median across months of the within-state match table and
    integer-support pmfs, the same three inputs and the same solve as
    the pooled derivation, run within the spread state."""
    rows = []
    for month in sorted(blocks):
        block = blocks[month].get(phase)
        if block is None:
            continue
        per_state = block["sweep"].get("touch", {}).get("by_spread", {}).get(state)
        if per_state is not None:
            rows.append(per_state)
    if not rows:
        return None

    def med(path: str, key: str) -> float:
        values = [r[path][key] for r in rows if key in r[path]]
        return float(np.median(values)) if values else float("nan")

    match = {b: med("match_by_touch", b) for b in MATCH_BUCKETS}
    return {
        "size_pmf_full": {k: med("size_pmf_full", k) for k in SIZE_SUPPORT},
        "touch_pmf_full": {k: med("touch_pmf_full", k) for k in SIZE_SUPPORT},
        "match_by_touch": {b: v for b, v in match.items() if np.isfinite(v)},
    }


def month_bucket_rows(
    counts: dict, pred: dict, min_n: int, groups: dict[str, list[str]] | None = None
) -> dict:
    """One month's real and predicted rate per group of touch values,
    prediction weighted by the month's own touch counts within the
    group."""
    rows = {}
    for label, keys in (groups or BUCKET_TOUCHES).items():
        n = gt = 0
        weighted = 0.0
        for key in keys:
            cell = counts.get(key)
            if cell is None:
                continue
            n += cell["n"]
            gt += cell["gt"]
            weighted += cell["n"] * pred[key]
        if n >= min_n:
            rows[label] = (n, gt / n, weighted / n)
    tail = counts.get(TAIL)
    if tail is not None and tail["n"] >= min_n:
        rows[TAIL] = (tail["n"], tail["gt"] / tail["n"], float("nan"))
    return rows


def summarize(name: str, per_month: list[dict], labels: list[str] | None = None) -> None:
    print(
        f"  {'touch':>6} {'months':>6} {'n/month':>9} "
        f"{'real':>7} {'p10':>7} {'p90':>7} {'pred':>7} "
        f"{'resid':>7} {'p10':>7} {'p90':>7}"
    )
    weights, residuals = [], []
    for bucket in labels if labels is not None else [*MATCH_BUCKETS, TAIL]:
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


def gt_rows(
    blocks: dict,
    phase: str,
    group: str,
    pred: dict,
    min_n: int,
    groups: dict[str, list[str]] | None = None,
) -> list[dict]:
    per_month = []
    for month in sorted(blocks):
        block = blocks[month].get(phase)
        if block is None:
            continue
        emitted = block["sweep"].get("touch", {}).get("size_gt_by_touch")
        if emitted is None:
            continue
        per_month.append(month_bucket_rows(emitted[group], pred, min_n, groups))
    return per_month


# The fine grouping for the bucket-constancy test: every touch its own
# row through 10, the 11+ range in three slices. Buckets 1 through 5
# are exactly identified by the per-bucket solve, so their residuals
# check the solve; 6 through 10 and the 11+ slices check the declared
# bucket-constant law inside the multi-touch buckets.
FINE_GROUPS: dict[str, list[str]] = {str(t): [str(t)] for t in range(1, 11)}
FINE_GROUPS["11-15"] = [str(t) for t in range(11, 16)]
FINE_GROUPS["16-20"] = [str(t) for t in range(16, 21)]
FINE_GROUPS["21-30"] = [str(t) for t in range(21, 31)]


def bucket_medians(blocks: dict, phase: str) -> dict | None:
    """Median across months of the bucket-conditional inputs."""
    rows = []
    for month in sorted(blocks):
        block = blocks[month].get(phase)
        if block is None:
            continue
        touch = block["sweep"].get("touch", {})
        if "size_pmf_by_touch" in touch:
            rows.append(touch)
    if not rows:
        return None

    def med(values: list) -> float:
        present = [v for v in values if v is not None]
        return float(np.median(present)) if present else float("nan")

    return {
        "touch_pmf_full": {
            k: med([r["touch_pmf_full"].get(k) for r in rows]) for k in SIZE_SUPPORT
        },
        "match_by_touch": {
            b: med([r["match_by_touch"].get(b) for r in rows]) for b in MATCH_BUCKETS
        },
        "size_pmf_by_touch": {
            b: {
                k: med(
                    [
                        r["size_pmf_by_touch"].get(b, {}).get("size_pmf_full", {}).get(k)
                        for r in rows
                    ]
                )
                for k in SIZE_SUPPORT
            }
            for b in MATCH_BUCKETS
        },
    }


def deconvolve_bucket(
    m_v: dict, touch_w: dict[str, float], match_rate: float
) -> tuple[float, dict]:
    """One bucket's mixture: an exact-touch atom plus a
    bucket-conditional law. m_v is the within-bucket executed-size pmf,
    touch_w the within-bucket touch weights, match_rate the observed
    exact-match share. The same fixed point as the pooled solve, one
    bucket wide; the tail touch's coincidence is declared zero, per the
    pooled convention. Negative mass rejects the mixture."""
    f = {k: m_v.get(k, 0.0) for k in SIZE_SUPPORT}
    p = 0.0
    for _round in range(200):
        previous = dict(f)
        coincidence = min(
            0.99,
            sum(w * (0.0 if key == TAIL else f.get(key, 0.0)) for key, w in touch_w.items()),
        )
        p = max(0.0, (match_rate - coincidence) / (1.0 - coincidence))
        if p >= 0.999:
            raise SystemExit("matched mass approaching one")
        for key in SIZE_SUPPORT:
            matched = touch_w.get(key, 0.0) * p
            value = (m_v.get(key, 0.0) - matched) / (1.0 - p)
            if value < -1e-9:
                raise SystemExit(f"negative mass at {key}")
            f[key] = max(0.0, value)
        total = sum(f.values())
        f = {k: v / total for k, v in f.items()}
        if max(abs(f[k] - previous[k]) for k in f) < 1e-12:
            break
    else:
        raise SystemExit("no fixed point in 200 rounds")
    return p, f


def bucket_conditional(blocks: dict, min_n: int) -> None:
    """The touch-conditional candidate. Touches 1 through 5 take the
    empirical conditional size law whole: in a singleton bucket the
    exact-touch atom and the law's own mass at that size are one
    observable, so a mixture split there is unidentified by
    construction and the simulator needs only the conditional draw.
    The multi-touch buckets keep the mixture form - the atom's location
    moves with the touch inside the bucket, and sharing F within the
    bucket identifies the split. The residual table is then the test of
    the one assumption left: rows 1 through 5 close by construction up
    to monthly variation, and the within-bucket rows of 6-10 and the
    11+ slices measure bucket-constancy of the non-match law."""
    labels = [*FINE_GROUPS, TAIL]
    for phase in PHASES:
        print(f"== {phase} bucket-conditional law")
        med = bucket_medians(blocks, phase)
        if med is None:
            print("  no size_pmf_by_touch emission; rerun the real pass")
            continue
        pred: dict = {}
        p_by: dict = {}
        failed = False
        for bucket in ("1", "2", "3", "4", "5"):
            m_v = med["size_pmf_by_touch"][bucket]
            total = sum(v for v in m_v.values() if np.isfinite(v))
            if total <= 0:
                print(f"  bucket {bucket}: thin inputs")
                failed = True
                continue
            t = int(bucket)
            above = sum(
                v
                for k, v in m_v.items()
                if np.isfinite(v) and (k.startswith(">") or int(k) > t)
            )
            pred[bucket] = above / total
        for bucket in ("6-10", "11+"):
            keys = list(BUCKET_TOUCHES[bucket])
            if bucket == "11+":
                keys.append(TAIL)
            weights = {k: med["touch_pmf_full"].get(k, 0.0) for k in keys}
            total = sum(w for w in weights.values() if np.isfinite(w))
            match_rate = med["match_by_touch"].get(bucket, float("nan"))
            if total <= 0 or not np.isfinite(match_rate):
                print(f"  bucket {bucket}: thin inputs, no solve")
                failed = True
                continue
            weights = {k: w / total for k, w in weights.items()}
            try:
                p_by[bucket], f = deconvolve_bucket(
                    med["size_pmf_by_touch"][bucket], weights, match_rate
                )
            except SystemExit as stop:
                print(f"  bucket {bucket}: mixture rejected - {stop}")
                failed = True
                continue
            f_total = sum(f.values())
            for t in [int(k) for k in BUCKET_TOUCHES[bucket]]:
                cdf = sum(f.get(str(s), 0.0) for s in range(1, t + 1)) / f_total
                pred[str(t)] = (1.0 - p_by[bucket]) * (1.0 - cdf)
        if failed:
            continue
        p_line = "  ".join(f"{b} {p_by[b]:.3f}" for b in ("6-10", "11+"))
        direct = "  ".join(f"{b} {pred[b]:.4f}" for b in ("1", "2", "3", "4", "5"))
        print(f"  direct conditional gt at 1-5: {direct}")
        print(f"  mixture p: {p_line}")
        summarize(
            phase, gt_rows(blocks, phase, "all", pred, min_n, FINE_GROUPS), labels
        )


def conditional(blocks: dict, min_n: int) -> None:
    """The candidate extension: p and F deconvolved per spread state by
    the ratified identification scheme, then the same residual table
    under the conditional mixture. A rejected state (negative mass) is
    the finding for that state, not an error."""
    for phase in PHASES:
        print(f"== {phase} conditional mixture")
        for state in ("1", "2", "3+"):
            real = state_medians(blocks, phase, state)
            if real is None:
                print(f"  spread {state}: no by_spread emission; rerun the real pass")
                continue
            try:
                p, f = deconvolve_sizes(real)
            except SystemExit as stop:
                print(f"  spread {state}: mixture rejected - {stop}")
                continue
            thin = [b for b in MATCH_BUCKETS if b not in real["match_by_touch"]]
            p_line = "  ".join(f"{b} {p.get(b, float('nan')):.3f}" for b in MATCH_BUCKETS)
            note = f"  thin {','.join(thin)}" if thin else ""
            print(f"  spread {state}  p: {p_line}{note}")
            pred = predicted_from(p, f)
            summarize(
                f"{phase} spread {state}", gt_rows(blocks, phase, state, pred, min_n)
            )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--by-spread", action="store_true")
    ap.add_argument(
        "--conditional",
        action="store_true",
        help="deconvolve per spread state and grade the conditional mixture",
    )
    ap.add_argument(
        "--bucket-conditional",
        action="store_true",
        help="deconvolve per touch bucket and grade the fine-grained curve",
    )
    ap.add_argument("--min-n", type=int, default=200)
    args = ap.parse_args()
    blocks = json.loads(TARGETS.read_text())["blocks"]
    if args.conditional:
        conditional(blocks, args.min_n)
        return
    if args.bucket_conditional:
        bucket_conditional(blocks, args.min_n)
        return
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
