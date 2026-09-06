#!/usr/bin/env python3
"""How far a tape travels, by phase, at several horizons.

Minute range says how much a bar wiggles; it says nothing about whether the
wiggles add up. This reports, per phase over weekday sessions: the range of
the whole phase per session (high minus low), the median absolute
close-to-close move at 5, 15, 30 and 60 minutes, and the minute range tail
(p90, max). Plus the week's total range and each session's range, which set
the price scale a chart pane draws on.

    python3 analysis/tape-v2/bars_moves.py LABEL=CSV [LABEL=CSV ...]
"""

from __future__ import annotations

import statistics
import sys

from bars_phases import PHASES, load, phase_of


def quantile(values: list[float], q: float) -> float:
    if not values:
        return float("nan")
    ordered = sorted(values)
    k = min(len(ordered) - 1, int(round(q * (len(ordered) - 1))))
    return ordered[k]


def sessions(rows: list[dict]) -> list[list[dict]]:
    # A session starts at the 22:00 UTC reopen. Traded minutes only.
    out: list[list[dict]] = []
    for r in rows:
        if r["v"] == 0.0:
            continue
        if r["mod"] == 22 * 60 or not out:
            out.append([])
        out[-1].append(r)
    return out


def main(argv: list[str]) -> None:
    panes = []
    for spec in argv:
        label, _, path = spec.partition("=")
        rows = load(path)
        for r in rows:
            r["close"] = None
        panes.append((label, rows))

    # Re-read closes; load() keeps range only.
    import csv

    for (label, rows), spec in zip(panes, argv):
        _, _, path = spec.partition("=")
        with open(path, newline="") as handle:
            for r, row in zip(rows, csv.DictReader(handle)):
                r["close"] = float(row["close"])
                r["high"] = float(row["high"])
                r["low"] = float(row["low"])

    print("== week range and per-session range (high - low), traded minutes ==")
    for label, rows in panes:
        traded = [r for r in rows if r["v"] > 0]
        week = max(r["high"] for r in traded) - min(r["low"] for r in traded)
        per = [
            max(r["high"] for r in s) - min(r["low"] for r in s)
            for s in sessions(rows)
        ]
        print(f"{label}: week {week:.0f}, sessions " + " ".join(f"{p:.0f}" for p in per))

    print("\n== per phase: median phase range per session ==")
    print("phase".ljust(10) + "".join(l.rjust(14) for l, _ in panes))
    for name, _, _ in PHASES:
        if name == "maint":
            continue
        cells = []
        for _, rows in panes:
            spans = []
            for s in sessions(rows):
                sel = [r for r in s if phase_of(r["mod"]) == name]
                if sel:
                    spans.append(max(r["high"] for r in sel) - min(r["low"] for r in sel))
            cells.append(f"{statistics.median(spans):.0f}".rjust(14) if spans else "-".rjust(14))
        print(name.ljust(10) + "".join(cells))

    print("\n== per phase: median |close move| at 5 / 15 / 30 / 60 minutes ==")
    print("phase".ljust(10) + "".join(l.rjust(26) for l, _ in panes))
    for name, _, _ in PHASES:
        if name == "maint":
            continue
        cells = []
        for _, rows in panes:
            parts = []
            for h in (5, 15, 30, 60):
                moves = []
                for s in sessions(rows):
                    for i in range(0, len(s) - h, h):
                        if phase_of(s[i]["mod"]) == name and phase_of(s[i + h]["mod"]) == name:
                            moves.append(abs(s[i + h]["close"] - s[i]["close"]))
                parts.append(f"{statistics.median(moves):5.1f}" if moves else "    -")
            cells.append(" ".join(parts).rjust(26))
        print(name.ljust(10) + "".join(cells))

    print("\n== per phase: minute range p90 / max ==")
    print("phase".ljust(10) + "".join(l.rjust(18) for l, _ in panes))
    for name, _, _ in PHASES:
        if name == "maint":
            continue
        cells = []
        for _, rows in panes:
            sel = [r["range"] for r in rows if phase_of(r["mod"]) == name and r["v"] > 0 and r["dow"] < 5]
            cells.append(f"{quantile(sel, 0.9):6.1f} / {max(sel):6.1f}".rjust(18) if sel else "-".rjust(18))
        print(name.ljust(10) + "".join(cells))

    print("\n== variance ratio: 60-minute move variance over 60 x 1-minute move variance, per phase ==")
    print("phase".ljust(10) + "".join(l.rjust(12) for l, _ in panes))
    for name, _, _ in PHASES:
        if name == "maint":
            continue
        cells = []
        for _, rows in panes:
            one = []
            sixty = []
            for s in sessions(rows):
                for i in range(1, len(s)):
                    if phase_of(s[i]["mod"]) == name and phase_of(s[i - 1]["mod"]) == name:
                        one.append(s[i]["close"] - s[i - 1]["close"])
                for i in range(0, len(s) - 60, 60):
                    if phase_of(s[i]["mod"]) == name and phase_of(s[i + 60]["mod"]) == name:
                        sixty.append(s[i + 60]["close"] - s[i]["close"])
            if len(one) > 2 and len(sixty) > 2:
                v1 = statistics.pvariance(one)
                v60 = statistics.pvariance(sixty)
                cells.append(f"{v60 / (60 * v1):.2f}".rjust(12))
            else:
                cells.append("-".rjust(12))
        print(name.ljust(10) + "".join(cells))


if __name__ == "__main__":
    main(sys.argv[1:])
