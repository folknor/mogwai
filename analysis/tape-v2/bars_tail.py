#!/usr/bin/env python3
"""The extreme minutes of each bars CSV.

Lists the largest H-L minutes per pane with their time, and counts minutes
above a few thresholds, because one impossible minute dominates the eye and
the median never sees it.

    python3 analysis/tape-v2/bars_tail.py [--top N] LABEL=CSV ...
"""

from __future__ import annotations

import argparse
import csv
from datetime import datetime, timezone

NS = 1_000_000_000
THRESHOLDS = (50.0, 100.0, 200.0)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--top", type=int, default=8)
    p.add_argument("panes", nargs="+", metavar="LABEL=CSV")
    args = p.parse_args()

    for spec in args.panes:
        label, _, path = spec.partition("=")
        bars = []
        with open(path, newline="") as handle:
            for row in csv.DictReader(handle):
                if int(row["trade_count"]) == 0:
                    continue
                t = int(row["open_ts"]) // NS
                bars.append(
                    (
                        float(row["high"]) - float(row["low"]),
                        datetime.fromtimestamp(t, tz=timezone.utc),
                        float(row["close"]) - float(row["open"]),
                    )
                )
        counts = " ".join(
            f">{int(th)}: {sum(1 for hl, _, _ in bars if hl > th)}"
            for th in THRESHOLDS
        )
        print(f"{label}: {len(bars)} minutes, H-L {counts}")
        for hl, when, co in sorted(bars, reverse=True)[: args.top]:
            print(f"  {when:%a %H:%M} UTC  H-L {hl:7.2f}  C-O {co:+8.2f}")


if __name__ == "__main__":
    main()
