#!/usr/bin/env python3
"""Answer four questions about bars CSVs side by side.

For each CSV: the runs of silent minutes (zero volume) by UTC time of day,
and per CME phase (UTC, daylight offset) the median minute range and median
minute volume over weekday sessions. Also the first ten minutes after the
cash open, one number per minute, because the open is where the eye looks.

    python3 analysis/tape-v2/bars_phases.py LABEL=CSV [LABEL=CSV ...]
"""

from __future__ import annotations

import csv
import statistics
import sys
from datetime import datetime, timezone

NS = 1_000_000_000

# Chicago daylight time is UTC-5. Phase edges in UTC minutes of day.
PHASES = [
    ("open", 22 * 60, 23 * 60),
    ("asia", 23 * 60, 24 * 60 + 7 * 60),
    ("london", 7 * 60, 12 * 60),
    ("ny_pre", 12 * 60, 13 * 60 + 30),
    ("ny_open", 13 * 60 + 30, 15 * 60 + 30),
    ("ny_mid", 15 * 60 + 30, 18 * 60),
    ("ny_close", 18 * 60, 21 * 60),
    ("maint", 21 * 60, 22 * 60),
]


def phase_of(minute_of_day: int) -> str:
    for name, lo, hi in PHASES:
        if lo <= minute_of_day < hi:
            return name
        if hi > 24 * 60 and (minute_of_day >= lo or minute_of_day < hi - 24 * 60):
            return name
    return "?"


def load(path: str) -> list[dict]:
    rows = []
    with open(path, newline="") as handle:
        for row in csv.DictReader(handle):
            ts = int(row["open_ts"])
            when = datetime.fromtimestamp(ts / NS, tz=timezone.utc)
            rows.append(
                {
                    "ts": ts,
                    "when": when,
                    "mod": when.hour * 60 + when.minute,
                    "dow": when.weekday(),
                    "range": float(row["high"]) - float(row["low"]),
                    "v": float(row["volume"]),
                    "n": int(row["trade_count"]),
                }
            )
    return rows


def silent_runs(rows: list[dict]) -> list[tuple[datetime, datetime, int]]:
    runs = []
    start = None
    count = 0
    for r in rows:
        if r["v"] == 0.0:
            if start is None:
                start = r["when"]
                count = 0
            count += 1
            last = r["when"]
        elif start is not None:
            runs.append((start, last, count))
            start = None
    if start is not None:
        runs.append((start, last, count))
    return runs


def main(argv: list[str]) -> None:
    panes = []
    for spec in argv:
        label, _, path = spec.partition("=")
        panes.append((label, load(path)))

    print("== bars, first, last ==")
    for label, rows in panes:
        print(
            f"{label}: {len(rows)} bars, {rows[0]['when']:%a %H:%M} to "
            f"{rows[-1]['when']:%a %H:%M} UTC"
        )

    print("\n== silent runs (zero volume), UTC ==")
    for label, rows in panes:
        runs = silent_runs(rows)
        print(f"{label}: {len(runs)} runs")
        for a, b, n in runs:
            print(f"  {a:%a %H:%M} to {b:%a %H:%M}  ({n} min)")

    print("\n== weekday sessions: median minute range / median volume per phase ==")
    header = "phase".ljust(10) + "".join(l.rjust(22) for l, _ in panes)
    print(header)
    for name, _, _ in PHASES:
        cells = []
        for _, rows in panes:
            sel = [
                r
                for r in rows
                if phase_of(r["mod"]) == name and r["v"] > 0 and r["dow"] < 5
            ]
            if not sel:
                cells.append("-".rjust(22))
                continue
            rng = statistics.median(r["range"] for r in sel)
            vol = statistics.median(r["v"] for r in sel)
            cells.append(f"{rng:8.2f} / {vol:8.0f}".rjust(22))
        print(name.ljust(10) + "".join(cells))

    print("\n== ratio of ny_open to asia median minute range ==")
    for label, rows in panes:
        def med(name: str) -> float:
            sel = [
                r["range"]
                for r in rows
                if phase_of(r["mod"]) == name and r["v"] > 0 and r["dow"] < 5
            ]
            return statistics.median(sel) if sel else float("nan")

        print(f"{label}: {med('ny_open') / med('asia'):.2f}")

    print("\n== first ten minutes after 13:30 UTC, median range over weekdays ==")
    for label, rows in panes:
        cells = []
        for k in range(10):
            sel = [
                r["range"]
                for r in rows
                if r["mod"] == 13 * 60 + 30 + k and r["dow"] < 5 and r["v"] > 0
            ]
            cells.append(f"{statistics.median(sel):6.2f}" if sel else "     -")
        print(f"{label:>14}: " + " ".join(cells))


if __name__ == "__main__":
    main(sys.argv[1:])
