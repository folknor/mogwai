#!/usr/bin/env python3
"""Render the UTC session profile from a characterize.py report.

Reads char_<PAIR>.json (no re-scan of the dump) and collapses the 24x7
hour-of-day x day-of-week grid into the two seasonality curves that matter:
trade intensity by UTC hour and per-trade volatility by UTC hour, plus the
day-of-week split. Text bars only; this is a look, not a deliverable.
"""

import json
import math
import os
import sys

# characterize.py uses dow = (days_since_epoch + 4) % 7, the standard Sun=0
# convention (1970-01-01 Thursday -> 4). So index 0=Sun, 1=Mon, ... 6=Sat.
MON_FIRST = [1, 2, 3, 4, 5, 6, 0]
MON_NAMES = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"]


def bar(value, peak, width=40):
    if peak <= 0:
        return ""
    return "#" * int(round(width * value / peak))


def main():
    pair = sys.argv[1] if len(sys.argv) > 1 else "XBTUSD"
    path = os.path.join(os.path.dirname(__file__), f"char_{pair}.json")
    with open(path) as f:
        rep = json.load(f)
    count = rep["session"]["count_hour_dow"]
    sumsq = rep["session"]["sumsq_ret_hour_dow"]

    hour_count = [sum(count[h]) for h in range(24)]
    hour_ss = [sum(sumsq[h]) for h in range(24)]
    # per-trade RMS return by hour, in basis points
    hour_vol = [
        (math.sqrt(hour_ss[h] / hour_count[h]) * 1e4) if hour_count[h] else 0.0
        for h in range(24)
    ]
    total = sum(hour_count) or 1
    peak_c = max(hour_count)
    peak_v = max(hour_vol)

    print(f"== {pair} session profile ({sum(hour_count):,} trades) ==\n")
    print("UTC hour | trade intensity (share) | per-trade vol (bps)")
    for h in range(24):
        share = 100 * hour_count[h] / total
        print(
            f"  {h:02d}:00  {share:5.2f}%  {bar(hour_count[h], peak_c, 24):<24}"
            f"  {hour_vol[h]:5.1f} {bar(hour_vol[h], peak_v, 16)}"
        )

    print("\nday-of-week (Mon..Sun):")
    dow_count = [0] * 7
    for h in range(24):
        for d in range(7):
            dow_count[d] += count[h][d]
    dtot = sum(dow_count) or 1
    peak_d = max(dow_count[i] for i in MON_FIRST)
    for name, idx in zip(MON_NAMES, MON_FIRST):
        share = 100 * dow_count[idx] / dtot
        print(f"  {name}  {share:5.2f}%  {bar(dow_count[idx], peak_d, 30)}")

    # session anchors for reference
    print("\nanchors (UTC): Asia~00-08  London open~07-08  "
          "NY open~13:30  London-NY overlap~13-16  NY close~21")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
