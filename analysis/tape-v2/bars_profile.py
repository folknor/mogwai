#!/usr/bin/env python3
"""Hourly profile of a bars CSV: bars, empty bars, volume, mean range.

A companion to the chart gate for the questions the eye raises but cannot
answer with a number: where the empty minutes fall, how volume splits across
sessions, how wide the average minute is per hour. One row per UTC hour of
day, summed across the days in the file. Stdlib only.

    python3 analysis/tape-v2/bars_profile.py CSV [CSV ...]
"""

from __future__ import annotations

import csv
import sys
from collections import defaultdict

NS_PER_SEC = 1_000_000_000


def profile(path: str) -> None:
    bars: dict[int, int] = defaultdict(int)
    empty: dict[int, int] = defaultdict(int)
    volume: dict[int, float] = defaultdict(float)
    span: dict[int, float] = defaultdict(float)
    days: set[int] = set()
    with open(path, newline="") as handle:
        for row in csv.DictReader(handle):
            secs = int(row["open_ts"]) // NS_PER_SEC
            hour = (secs // 3600) % 24
            days.add(secs // 86400)
            bars[hour] += 1
            if int(row["trade_count"]) == 0:
                empty[hour] += 1
            volume[hour] += float(row["volume"])
            span[hour] += float(row["high"]) - float(row["low"])
    total_volume = sum(volume.values())
    print(f"\n{path}")
    print(f"{len(days)} UTC days, {sum(bars.values())} bars, "
          f"{sum(empty.values())} empty, volume {total_volume:.0f}")
    print("utc_hour  bars  empty  volume_pct  mean_range")
    for hour in range(24):
        if bars[hour] == 0:
            continue
        pct = 100.0 * volume[hour] / total_volume if total_volume else 0.0
        rng = span[hour] / bars[hour]
        print(f"{hour:8d}  {bars[hour]:4d}  {empty[hour]:5d}  "
              f"{pct:10.1f}  {rng:10.2f}")


def main(argv: list[str]) -> None:
    if not argv:
        sys.exit(__doc__)
    for path in argv:
        profile(path)


if __name__ == "__main__":
    main(sys.argv[1:])
