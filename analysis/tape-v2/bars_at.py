#!/usr/bin/env python3
"""Print the bars around a compare-page bar index.

The compare page's grid is the union of every pane's traded minutes, so a
bar index there is not a minute offset from the origin. This rebuilds the
same grid from the CSVs and prints each pane's bar at the requested indexes
with a few neighbours.

    python3 analysis/tape-v2/bars_at.py --index 4043 --around 3 LABEL=CSV ...
"""

from __future__ import annotations

import argparse
import csv
from datetime import datetime, timezone

NS = 1_000_000_000


def load(path: str) -> dict[int, dict]:
    out = {}
    with open(path, newline="") as handle:
        for row in csv.DictReader(handle):
            if int(row["trade_count"]) == 0:
                continue
            out[int(row["open_ts"]) // NS] = row
    return out


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--index", type=int, required=True)
    p.add_argument("--around", type=int, default=2)
    p.add_argument("panes", nargs="+", metavar="LABEL=CSV")
    args = p.parse_args()

    panes = []
    for spec in args.panes:
        label, _, path = spec.partition("=")
        panes.append((label, load(path)))
    # The page's grid: every minute from the first traded one to the last,
    # closures included as blank minutes.
    first = min(min(bars) for _, bars in panes)
    last = max(max(bars) for _, bars in panes)
    grid = list(range(first, last + 60, 60))

    for index in range(args.index - args.around, args.index + args.around + 1):
        if not 0 <= index < len(grid):
            continue
        t = grid[index]
        when = datetime.fromtimestamp(t, tz=timezone.utc)
        print(f"bar {index}  {when:%a %H:%M} UTC")
        for label, bars in panes:
            row = bars.get(t)
            if row is None:
                print(f"  {label}: closed")
                continue
            hl = float(row["high"]) - float(row["low"])
            print(
                f"  {label}: O {row['open']} H {row['high']} L {row['low']} "
                f"C {row['close']}  H-L {hl:.2f}  vol {row['volume']}  "
                f"trades {row['trade_count']}"
            )


if __name__ == "__main__":
    main()
