#!/usr/bin/env python3
"""Per-hour ratio of one bars CSV to another: volume and mean minute range.

`bars_profile.py` shows each tape's hourly profile as a share of its own
total, which is the right view for shape and the wrong view for level: two
tapes can agree on every share and differ by a third in volume. This
compares absolute quantities per UTC hour, candidate over reference, so a
ratio of 1.0 is "the same" and the level cannot hide. Stdlib only.

    python3 analysis/tape-v2/bars_ratio.py REFERENCE.csv CANDIDATE.csv
"""

from __future__ import annotations

import csv
import sys
from collections import defaultdict

NS_PER_SEC = 1_000_000_000


def hourly(path: str) -> tuple[dict[int, float], dict[int, float], dict]:
    volume: dict[int, float] = defaultdict(float)
    span: dict[int, float] = defaultdict(float)
    bars: dict[int, int] = defaultdict(int)
    with open(path, newline="") as handle:
        for row in csv.DictReader(handle):
            if int(row["trade_count"]) == 0:
                continue
            secs = int(row["open_ts"]) // NS_PER_SEC
            hour = (secs // 3600) % 24
            volume[hour] += float(row["volume"])
            span[hour] += float(row["high"]) - float(row["low"])
            bars[hour] += 1
    return volume, span, bars


def main(argv: list[str]) -> None:
    if len(argv) != 2:
        sys.exit(__doc__)
    ref_v, ref_s, ref_n = hourly(argv[0])
    can_v, can_s, can_n = hourly(argv[1])
    total_ref = sum(ref_v.values())
    total_can = sum(can_v.values())
    print(f"reference {argv[0]}")
    print(f"candidate {argv[1]}")
    print(f"total volume ratio {total_can / total_ref:.2f}")
    print(
        "utc_hour  vol_ref  vol_cand  vol_ratio  rng_ref  rng_cand  rng_ratio"
    )
    for hour in range(24):
        if ref_n[hour] == 0 or can_n[hour] == 0:
            continue
        rr = ref_s[hour] / ref_n[hour]
        rc = can_s[hour] / can_n[hour]
        print(
            f"{hour:8d}  {ref_v[hour]:7.0f}  {can_v[hour]:8.0f}  "
            f"{can_v[hour] / ref_v[hour]:9.2f}  {rr:7.2f}  {rc:8.2f}  "
            f"{rc / rr:9.2f}"
        )


if __name__ == "__main__":
    main(sys.argv[1:])
