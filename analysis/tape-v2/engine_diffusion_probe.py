#!/usr/bin/env python3
"""Measure the engine's per-parent mid diffusion from a placed-book
quotes CSV (`gen --type quotes` on a preset without a book table).

On the placed-book path the published mid is the rounded latent mid, so
consecutive-quote mid changes are the cascade's own per-parent
innovation plus its propagator - the diffusion the prototype must PIN,
not fit. Free-fitting the prototype's diffusion knobs re-creates the
transfer failure the first transcription paid for: a book fitted
against a diffusion the engine does not have.

    python3 engine_diffusion_probe.py data/mnq-book-quotes-w1.csv
"""

from __future__ import annotations

import argparse

import numpy as np

FAST_NS = 100_000_000
TICK = 0.25
# The candidate clock: a generated tape runs on the preset's fixed CDT
# offset, the same convention micro_stats applies to a gen CSV.
UTC_OFFSET_MINUTES = -300
SESSION_OPEN_MINUTE = 17 * 60


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("csv")
    ap.add_argument(
        "--minutes",
        default=None,
        help="session-minute range lo:hi (e.g. 60:540 for asia) to pin "
        "one phase's diffusion; default pools the whole tape",
    )
    args = ap.parse_args()
    data = np.genfromtxt(args.csv, delimiter=",", names=True, max_rows=4_000_000)
    mid = (data["bid_px"] + data["ask_px"]) / 2.0
    ts = data["ts_event"]
    if args.minutes:
        lo, hi = (int(v) for v in args.minutes.split(":"))
        minute_of_day = (ts / 60e9 + UTC_OFFSET_MINUTES) % 1440
        session_minute = (minute_of_day - SESSION_OPEN_MINUTE) % 1440
        keep = (session_minute >= lo) & (session_minute < hi)
        mid = mid[keep]
        ts = ts[keep]
    gap = np.diff(ts)
    dmid = np.abs(np.diff(mid)) / TICK
    fast = gap < FAST_NS
    d = dmid[fast]
    edges = [
        ("0", 0.0, 0.25),
        ("0.5", 0.25, 0.75),
        ("1", 0.75, 1.25),
        ("1.5", 1.25, 1.75),
        ("2", 1.75, 2.25),
        ("2.5+", 2.25, None),
    ]
    print(f"fast pairs {int(fast.sum())} of {dmid.size}")
    for label, lo, hi in edges:
        mask = (d >= lo) if hi is None else (d >= lo) & (d < hi)
        print(f"  dmid {label}: {float(mask.mean()):.4f}")
    for p in (0.9, 0.99, 0.999):
        print(f"  p{int(p * 1000)}: {float(np.quantile(d, p)):.2f} ticks")
    print(f"  mean abs: {float(d.mean()):.3f} ticks")


if __name__ == "__main__":
    main()
