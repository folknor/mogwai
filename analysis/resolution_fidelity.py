#!/usr/bin/env python3
"""Does millisecond truncation distort F1 and F2, and does it do so with activity?

The resolution-fidelity gate of `notes/sampling-frame-preregistration.md` section
3.2, computed on ONE verified microsecond month. Thresholds are preregistered in
that document and mirrored here as named constants; changing one after reading a
result invalidates the acceptance claim it supports.

The question is NOT whether truncation loses information - it obviously does.
It is whether the loss is UNIFORM or ACTIVITY-DEPENDENT. A uniform bias shifts
every month equally and is tolerable. An activity-dependent bias merges more
events in busy months than quiet ones, and since volume is a stratifying feature
that manufactures exactly the separation the sampling-frame experiment is trying
to measure. That is the Kraken failure of preregistration section 2 in weaker
form.

The event estimator is imported from `probe_binance_trades`, unchanged, so this
probe cannot quietly define its own notion of a parent.

Usage:
    python3 analysis/resolution_fidelity.py
    python3 analysis/resolution_fidelity.py --archive <path-to-monthly-zip>
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import zipfile
from pathlib import Path

try:
    from analysis.probe_binance_trades import EventStats
except ModuleNotFoundError:
    from probe_binance_trades import EventStats

ROOT = Path(__file__).resolve().parents[1]
DEFAULT_ARCHIVE = ROOT / "research/market-data/BTCUSDT-trades-2026-06.zip"

# ---------------------------------------------------------------------------
# Preregistered thresholds. Mirrors section 3.2 of the preregistration.
# ---------------------------------------------------------------------------

MAX_PARENT_COUNT_LOSS = 0.10
MAX_MULTI_NATIVE_PARENT_FRAC = 0.10
MAX_SCALE_TARGET_RELATIVE_SHIFT = 0.10
MAX_BOUNDED_TARGET_ABSOLUTE_SHIFT = 0.05
MAX_MERGE_RATE_DECILE_SPREAD = 0.05

SCALE_TARGETS = (
    ("F1", "mean_event_duration_s", ("parent_gap", "mean_s")),
    ("F1", "duration_dispersion_cv2", ("parent_gap", "cv2")),
    ("F2", "children_mean", ("children", "mean")),
    ("F2", "levels_mean", ("levels", "mean")),
)
BOUNDED_TARGETS = (
    ("F1", "duration_acf_lag1", ("parent_gap", "acf_lag1")),
    ("F1", "duration_acf_lag5", ("parent_gap", "acf_lag5")),
    ("F2", "children_single_frac", ("children", "single_frac")),
)

MINUTE_US = 60_000_000


class MergeTracker:
    """Counts millisecond parents that swallowed more than one native parent.

    A native parent is a maximal run of rows sharing microsecond timestamp and
    side; a millisecond parent is the same over the truncated stamp. Because
    truncation is monotone, every native parent sits inside exactly one
    millisecond parent, so the merge count is well defined by construction.
    """

    def __init__(self) -> None:
        self.ms_key = None
        self.native_key = None
        self.natives_in_current = 0
        self.ms_parents = 0
        self.ms_parents_merged = 0
        self.native_parents = 0

    def push(self, stamp: int, side: bool) -> None:
        ms_key = (stamp // 1000, side)
        native_key = (stamp, side)
        if ms_key != self.ms_key:
            self._close()
            self.ms_key = ms_key
            self.natives_in_current = 0
            self.native_key = None
        if native_key != self.native_key:
            self.native_key = native_key
            self.natives_in_current += 1
            self.native_parents += 1

    def _close(self) -> None:
        if self.ms_key is None:
            return
        self.ms_parents += 1
        if self.natives_in_current > 1:
            self.ms_parents_merged += 1

    def finish(self) -> None:
        self._close()
        self.ms_key = None


class PerMinute:
    """Per-minute trade count and parent counts under both resolutions."""

    def __init__(self) -> None:
        self.trades: dict[int, int] = {}
        self.native: dict[int, int] = {}
        self.ms: dict[int, int] = {}
        self._native_key = None
        self._ms_key = None

    def push(self, stamp: int, side: bool) -> None:
        minute = stamp // MINUTE_US
        self.trades[minute] = self.trades.get(minute, 0) + 1
        native_key = (stamp, side)
        if native_key != self._native_key:
            self._native_key = native_key
            self.native[minute] = self.native.get(minute, 0) + 1
        ms_key = (stamp // 1000, side)
        if ms_key != self._ms_key:
            self._ms_key = ms_key
            self.ms[minute] = self.ms.get(minute, 0) + 1

    def decile_merge_rates(self) -> list[dict]:
        """Mean merge rate per activity decile, deciles cut on trade count.

        Minutes with a single native parent carry no information about merging
        and are kept rather than dropped: excluding them would preferentially
        remove quiet minutes and bias the very comparison being made.
        """
        minutes = sorted(self.trades, key=lambda m: self.trades[m])
        if not minutes:
            return []
        buckets: list[dict] = []
        n = len(minutes)
        for d in range(10):
            lo = (n * d) // 10
            hi = (n * (d + 1)) // 10
            chunk = minutes[lo:hi]
            if not chunk:
                continue
            native = sum(self.native.get(m, 0) for m in chunk)
            ms = sum(self.ms.get(m, 0) for m in chunk)
            trades = sum(self.trades[m] for m in chunk)
            buckets.append(
                {
                    "decile": d,
                    "minutes": len(chunk),
                    "trades": trades,
                    "mean_trades_per_minute": trades / len(chunk),
                    "native_parents": native,
                    "ms_parents": ms,
                    "merge_rate": (1 - ms / native) if native else 0.0,
                }
            )
        return buckets


def dig(report: dict, path: tuple[str, str]) -> float:
    return report[path[0]][path[1]]


def run(archive: Path) -> dict:
    native = EventStats(True)
    truncated = EventStats(True)
    merges = MergeTracker()
    per_minute = PerMinute()
    rows = 0

    with zipfile.ZipFile(archive) as zf:
        info = zf.getinfo(zf.namelist()[0])
        with zf.open(info) as stream:
            for row in csv.reader(io.TextIOWrapper(stream, newline="")):
                if not row or not row[0].lstrip("-").isdigit():
                    continue
                price = row[1]
                stamp = int(row[4])
                side = row[5].strip().lower() == "true"
                rows += 1
                native.push(stamp, side, price)
                # Truncation keeps MICROSECOND units so both estimators share
                # one gap scale; only the resolution changes.
                truncated.push((stamp // 1000) * 1000, side, price)
                merges.push(stamp, side)
                per_minute.push(stamp, side)
    merges.finish()

    native_report = native.report()
    ms_report = truncated.report()

    parent_loss = 1 - ms_report["events"] / native_report["events"]
    multi_frac = (
        merges.ms_parents_merged / merges.ms_parents if merges.ms_parents else 0.0
    )
    deciles = per_minute.decile_merge_rates()
    spread = (
        deciles[-1]["merge_rate"] - deciles[0]["merge_rate"] if len(deciles) >= 2 else 0.0
    )

    shifts = []
    for family, name, path in SCALE_TARGETS:
        a = dig(native_report, path)
        b = dig(ms_report, path)
        rel = abs(b - a) / abs(a) if a else float("inf")
        shifts.append(
            {
                "family": family,
                "target": name,
                "kind": "scale",
                "native": a,
                "millisecond": b,
                "relative_shift": rel,
                "threshold": MAX_SCALE_TARGET_RELATIVE_SHIFT,
                "pass": rel <= MAX_SCALE_TARGET_RELATIVE_SHIFT,
            }
        )
    for family, name, path in BOUNDED_TARGETS:
        a = dig(native_report, path)
        b = dig(ms_report, path)
        absolute = abs(b - a)
        shifts.append(
            {
                "family": family,
                "target": name,
                "kind": "bounded",
                "native": a,
                "millisecond": b,
                "absolute_shift": absolute,
                "threshold": MAX_BOUNDED_TARGET_ABSOLUTE_SHIFT,
                "pass": absolute <= MAX_BOUNDED_TARGET_ABSOLUTE_SHIFT,
            }
        )

    checks = {
        "parent_count_loss": {
            "value": parent_loss,
            "threshold": MAX_PARENT_COUNT_LOSS,
            "pass": parent_loss <= MAX_PARENT_COUNT_LOSS,
        },
        "multi_native_parent_frac": {
            "value": multi_frac,
            "threshold": MAX_MULTI_NATIVE_PARENT_FRAC,
            "pass": multi_frac <= MAX_MULTI_NATIVE_PARENT_FRAC,
        },
        "merge_rate_decile_spread": {
            "value": spread,
            "threshold": MAX_MERGE_RATE_DECILE_SPREAD,
            "pass": spread <= MAX_MERGE_RATE_DECILE_SPREAD,
        },
        "target_shifts": {
            "value": sum(0 if s["pass"] else 1 for s in shifts),
            "threshold": 0,
            "pass": all(s["pass"] for s in shifts),
        },
    }

    return {
        "archive": str(archive),
        "rows": rows,
        "native_parents": native_report["events"],
        "millisecond_parents": ms_report["events"],
        "checks": checks,
        "shifts": shifts,
        "activity_deciles": deciles,
        "gate_passes": all(c["pass"] for c in checks.values()),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, default=DEFAULT_ARCHIVE)
    parser.add_argument("--json", type=Path)
    args = parser.parse_args()

    result = run(args.archive)

    print(f"archive               {result['archive']}")
    print(f"rows                  {result['rows']:,}")
    print(f"native parents        {result['native_parents']:,}")
    print(f"millisecond parents   {result['millisecond_parents']:,}")
    print()
    print("preregistered checks:")
    for name, check in result["checks"].items():
        verdict = "PASS" if check["pass"] else "FAIL"
        print(
            f"  {verdict}  {name:<28} {check['value']:>12.6f}  "
            f"limit {check['threshold']}"
        )
    print()
    print("F1 and F2 target shifts:")
    for shift in result["shifts"]:
        verdict = "PASS" if shift["pass"] else "FAIL"
        moved = shift.get("relative_shift", shift.get("absolute_shift"))
        print(
            f"  {verdict}  {shift['family']} {shift['target']:<26} "
            f"{shift['native']:>12.6f} -> {shift['millisecond']:<12.6f} "
            f"moved {moved:.6f}"
        )
    print()
    print("merge rate by activity decile (the decisive check):")
    for bucket in result["activity_deciles"]:
        print(
            f"  d{bucket['decile']}  {bucket['mean_trades_per_minute']:>10.1f} trades/min"
            f"   merge rate {bucket['merge_rate']:.6f}"
        )
    print()
    print("GATE PASSES" if result["gate_passes"] else "GATE FAILS")

    if args.json:
        args.json.write_text(json.dumps(result, indent=2, sort_keys=True))
        print(f"wrote {args.json}")


if __name__ == "__main__":
    main()
