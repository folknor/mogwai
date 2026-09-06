#!/usr/bin/env python3
"""The largest close-to-close moves at several horizons, per pane.

A big move's size says little without its duration: a 300-point move over
fifteen minutes at the open is an ordinary MNQ morning, the same move inside
two minutes is not. For each horizon this prints the largest absolute move
over any window of that many traded minutes, when it started, and how many
such windows exceed a threshold.

    python3 analysis/tape-v2/bars_bigmoves.py [--threshold 200] LABEL=CSV ...
"""

from __future__ import annotations

import argparse
import csv
from datetime import datetime, timezone

NS = 1_000_000_000
HORIZONS = (1, 2, 5, 10, 20, 30)


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--threshold", type=float, default=200.0)
    p.add_argument("--top", type=int, default=3)
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
                bars.append((datetime.fromtimestamp(t, tz=timezone.utc), float(row["close"])))
        print(f"{label}: {len(bars)} minutes")
        for h in HORIZONS:
            moves = [
                (bars[i + h][1] - bars[i][1], bars[i][0])
                for i in range(len(bars) - h)
                # Stay inside one session: a window across the close is a gap.
                if (bars[i + h][0] - bars[i][0]).total_seconds() == 60 * h
            ]
            over = sum(1 for m, _ in moves if abs(m) > args.threshold)
            top = sorted(moves, key=lambda x: -abs(x[0]))[: args.top]
            shown = "  ".join(f"{m:+7.1f} at {w:%a %H:%M}" for m, w in top)
            print(f"  {h:2d} min: over {args.threshold:.0f}: {over:4d}   largest {shown}")


if __name__ == "__main__":
    main()
