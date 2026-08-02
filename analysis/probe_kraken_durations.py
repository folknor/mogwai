#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Probe a Kraken corpus CSV for the duration statistics, in the two forms the
Binance probe prints, so the fitted corpus and the new ones are comparable.

Why this exists: `analysis/probe_binance_aggtrades.py` showed that Binance
stamps one match event with a single timestamp, so raw inter-print gaps mix
real arrivals with zero-length intra-sweep gaps, and the dimensionless
dispersion moves a lot when those are collapsed (BTC 11.63 -> 4.62). The
committed fingerprint's duration statistics come from this Kraken corpus and
have never been checked the same way. If Kraken carries its own timestamp
collisions, the fitted constants may describe an artifact rather than the
market - the same failure the Binance numbers nearly shipped with.

Prints raw and collapsed forms of the same statistics, era-windowed on
`DWELL_ERA_START_TS` to match what `characterize.py` reports and what the
committed band was re-derived against on 2026-08-02.

Streaming, O(max_lag) in memory, over a multi-gigabyte file.

Usage:
    python3 analysis/probe_kraken_durations.py             # anchor pair
    python3 analysis/probe_kraken_durations.py ETHUSD
    python3 analysis/probe_kraken_durations.py /abs/path/to/PAIR.csv
"""

import os
import sys

from characterize import DATA_DIR, MAX_LAG, AutoCorr, DWELL_ERA_START_TS

ANCHOR = "XBTUSD"


def probe(path):
    n = 0
    prev = None
    raw_n = 0
    raw_sum = 0.0
    raw_sumsq = 0.0
    raw_zero = 0
    raw_acf = AutoCorr(MAX_LAG)

    ev_prev = None
    ev_n = 0
    ev_sum = 0.0
    ev_sumsq = 0.0
    ev_acf = AutoCorr(MAX_LAG)
    burst_cur = 0
    burst_n = 0
    burst_sum = 0
    burst_max = 0

    first = last = None

    with open(path, "r", errors="replace") as f:
        for line in f:
            parts = line.split(",")
            if len(parts) < 3:
                continue
            try:
                ts = float(parts[0])
            except ValueError:
                continue
            if ts < DWELL_ERA_START_TS:
                continue
            n += 1
            if first is None:
                first = ts
            last = ts

            if prev is not None:
                dt = ts - prev
                if dt >= 0:
                    raw_n += 1
                    raw_sum += dt
                    raw_sumsq += dt * dt
                    if dt == 0.0:
                        raw_zero += 1
                    raw_acf.push(dt)
            prev = ts

            if ts != ev_prev:
                if ev_prev is not None:
                    dt = ts - ev_prev
                    ev_n += 1
                    ev_sum += dt
                    ev_sumsq += dt * dt
                    ev_acf.push(dt)
                    burst_n += 1
                    burst_sum += burst_cur
                    burst_max = max(burst_max, burst_cur)
                ev_prev = ts
                burst_cur = 1
            else:
                burst_cur += 1

    span = (last - first) if first is not None else 0.0
    print(f"file            {path}")
    print(f"trades in era   {n:,}   (era start {DWELL_ERA_START_TS})")
    print(f"span            {span / 86400:.1f} days")
    print(f"trades/sec      {n / span:.3f}")

    for label, cnt, s, sq, acf_obj in (
        ("raw prints", raw_n, raw_sum, raw_sumsq, raw_acf),
        ("collapsed", ev_n, ev_sum, ev_sumsq, ev_acf),
    ):
        mean = s / cnt if cnt else 0.0
        var = (sq / cnt - mean * mean) if cnt else 0.0
        a = acf_obj.acf()
        print(f"--- {label} ---")
        print(f"  gaps          {cnt:,}")
        print(f"  mean          {mean * 1000:.2f} ms")
        print(f"  var/mean      {(var / mean) if mean else 0:.2f}   (characterize.py units)")
        print(f"  var/mean^2    {(var / (mean * mean)) if mean else 0:.2f}   (dimensionless)")
        print(f"  ACF lag1      {a[0]:.4f}   lag5 {a[4]:.4f}")

    print(f"zero-gap frac   {raw_zero / raw_n:.4f}   (identical timestamps)")
    print(f"burst width     mean {burst_sum / burst_n:.2f}  max {burst_max}")


if __name__ == "__main__":
    arg = sys.argv[1] if len(sys.argv) > 1 else ANCHOR
    probe(arg if os.path.sep in arg else os.path.join(DATA_DIR, f"{arg}.csv"))
