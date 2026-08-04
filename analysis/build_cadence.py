#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Build the committed cadence measurement from available raw-trade archives."""

import datetime
import json
from pathlib import Path

try:
    from analysis.probe_binance_trades import probe
except ModuleNotFoundError:
    from probe_binance_trades import probe

ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "research" / "market-data"
OUTPUT = ROOT / "analysis" / "cadence.json"
PAIRS = ("BTCUSDT", "ETHUSDT", "SOLUSDT")


WIDEN = 1.5


def band(values, floor=None):
    """Per-pair extremes widened by a FACTOR of 1.5 on each side.

    Multiplicative, not spread-relative: a spread-relative widening on a
    three-pair sample lets a wide spread drag the low edge to zero, which
    produced committed bands admitting a zero mean gap and a `levels_mean`
    below one - values the generator's own validator refuses. The floors are
    the same invariants stated where the band is built, so a re-run re-derives
    them instead of a reader trusting that the data happened to be sane.
    """
    low = min(values) / WIDEN
    high = max(values) * WIDEN
    if floor is not None:
        low = max(floor, low)
    return {"min": low, "median": sorted(values)[len(values) // 2], "max": high}


def solve_shape(children_mean, single_frac, levels_mean):
    fallback = single_frac < 1.0 / children_mean
    if fallback:
        q = 0.0
        m = children_mean
    else:
        m = (children_mean - 1.0) / (1.0 - single_frac)
        q = 1.0 - (children_mean - 1.0) / (m - 1.0)
    return {
        "q": q,
        "m": m,
        "level_step_prob": (levels_mean - 1.0) / (children_mean - 1.0),
        "fallback_pure_geometric": fallback,
    }


def build():
    reports = {}
    archives = []
    for pair in PAIRS:
        path = DATA / f"{pair}-trades-2026-06.zip"
        if not path.exists():
            raise FileNotFoundError(path)
        report = probe(path)
        reports[pair] = report
        archives.append({
            "name": path.name,
            "bytes": path.stat().st_size,
            "rows": report["rows"],
            "span_days": report["span_days"],
        })
    primary = {pair: reports[pair]["timestamp_and_side"] for pair in PAIRS}
    fields = {
        "mean_event_duration_s": [primary[p]["parent_gap"]["mean_s"] for p in PAIRS],
        "children_mean": [primary[p]["children"]["mean"] for p in PAIRS],
        "children_single_frac": [primary[p]["children"]["single_frac"] for p in PAIRS],
        "levels_mean": [primary[p]["levels"]["mean"] for p in PAIRS],
        "mean_trade_notional": [reports[p]["mean_notional"] for p in PAIRS],
        "duration_dispersion_cv2": [primary[p]["parent_gap"]["cv2"] for p in PAIRS],
        "duration_acf_lag1": [primary[p]["parent_gap"]["acf_lag1"] for p in PAIRS],
        "duration_acf_lag5": [primary[p]["parent_gap"]["acf_lag5"] for p in PAIRS],
    }
    # Hard floors, each one an invariant rather than a taste: a Poisson process
    # is the least-clustered tape the gate may accept (cv2 1.0); a lag-1 gap ACF
    # at or below zero means there is no clustering left to reproduce at all; a
    # sweep touches at least one level; and a mean gap is strictly positive.
    floors = {
        "duration_dispersion_cv2": 1.0,
        "duration_acf_lag1": 1e-6,
        "duration_acf_lag5": 0.0,
        "levels_mean": 1.0,
        "mean_event_duration_s": 1e-9,
        "children_mean": 1.0 + 1e-9,
        "children_single_frac": 0.0,
        "mean_trade_notional": 1e-9,
    }
    targets = {}
    for name, values in fields.items():
        targets[name] = {"anchor": values[0], "range": band(values, floors.get(name))}
    targets["per_second_counts"] = reports["BTCUSDT"]["per_second_counts"]
    anchor = primary["BTCUSDT"]
    result = {
        "provenance": {
            "generated_utc": datetime.datetime.now(datetime.timezone.utc).isoformat(),
            "archives": archives,
        },
        "anchor": "BTCUSDT",
        "pairs": reports,
        "targets": targets,
        "shape": solve_shape(
            anchor["children"]["mean"],
            anchor["children"]["single_frac"],
            anchor["levels"]["mean"],
        ),
    }
    OUTPUT.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    return result


if __name__ == "__main__":
    built = build()
    anchor = built["targets"]
    print(f"wrote {OUTPUT}")
    print(json.dumps({name: value["anchor"] if "anchor" in value else value
                      for name, value in anchor.items()}, indent=2))
