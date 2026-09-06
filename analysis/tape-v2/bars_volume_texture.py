#!/usr/bin/env python3
"""Overnight volume texture: level, spread and burstiness per pane.

For the overnight phases (open, asia, london, weekday sessions) this prints
the minute-volume quantiles, the coefficient of variation, the share of
minutes above twice and below half the phase median, and the volume
autocorrelation at a few lags. A tape whose minutes are a deterministic
rate plus Poisson noise reads as a flat wall; a real night is quiet with
bursts that taper.

    python3 analysis/tape-v2/bars_volume_texture.py LABEL=CSV ...
"""

from __future__ import annotations

import statistics
import sys

from bars_phases import load, phase_of

PHASES = ("open", "asia", "london", "ny_pre", "ny_open", "ny_mid", "ny_close")
LAGS = (1, 5, 15, 60)


def quantile(values: list[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(round(q * (len(ordered) - 1))))]


def autocorr(series: list[float], lag: int) -> float:
    n = len(series) - lag
    if n < 2:
        return float("nan")
    mean = statistics.fmean(series)
    var = sum((x - mean) ** 2 for x in series)
    if var == 0:
        return float("nan")
    cov = sum((series[i] - mean) * (series[i + lag] - mean) for i in range(n))
    return cov / var


def main(argv: list[str]) -> None:
    panes = [(spec.partition("=")[0], load(spec.partition("=")[2])) for spec in argv]
    for name in PHASES:
        print(f"== {name} ==")
        print(
            "pane".ljust(8)
            + "p10".rjust(7) + "p25".rjust(7) + "p50".rjust(7) + "p75".rjust(7)
            + "p90".rjust(7) + "p99".rjust(7) + "cv".rjust(7)
            + ">2xmed".rjust(8) + "<half".rjust(7)
            + "".join(f"ac{lag}".rjust(7) for lag in LAGS)
        )
        for label, rows in panes:
            sel = [r["v"] for r in rows if phase_of(r["mod"]) == name and r["v"] > 0 and r["dow"] < 5]
            if len(sel) < 10:
                continue
            med = statistics.median(sel)
            mean = statistics.fmean(sel)
            cv = statistics.pstdev(sel) / mean
            high = sum(1 for v in sel if v > 2 * med) / len(sel)
            low = sum(1 for v in sel if v < 0.5 * med) / len(sel)
            qs = "".join(f"{quantile(sel, q):7.0f}" for q in (0.1, 0.25, 0.5, 0.75, 0.9, 0.99))
            acs = "".join(f"{autocorr(sel, lag):7.2f}" for lag in LAGS)
            print(f"{label:<8}{qs}{cv:7.2f}{high:8.2f}{low:7.2f}{acs}")
        print()


if __name__ == "__main__":
    main(sys.argv[1:])
