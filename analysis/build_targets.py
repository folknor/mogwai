#!/usr/bin/env python3
"""Per-month tick-level target vectors for the sampling-frame experiment.

Brick 4 of `notes/sampling-frame-preregistration.md`. One streaming traversal
per archive produces the fourteen targets across the six families of section 6:
four in F1, three in F2, two in F3, one in F4, three in F5, one in F6.

Every estimator is IMPORTED rather than reimplemented:

  F1 cadence, F2 sweep   `probe_binance_trades.EventStats`, the same class that
                         produced `analysis/cadence.json`
  ACFs                   `probe_binance_aggtrades.AutoCorr`
  round-lot tell         `characterize.decimals_used`
  size dispersion        `characterize.lvl_bin` / `histogram_quantile`

That is the point of the equivalence gate below: if this file computed its own
notion of a parent or a return, the experiment would be measuring an estimator
nobody has validated.

Modes:

    equivalence   2026-06 F1 and F2 against cadence.json, exact; the gate
    build         the batch, resumable, atomic per month
    freeze        assemble and freeze the 19-month target table

Usage:
    python3 -u analysis/build_targets.py equivalence
    python3 -u analysis/build_targets.py build
    python3 -u analysis/build_targets.py freeze
"""

from __future__ import annotations

import argparse
import json
import math
import os
import sys
import time
import zipfile
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from characterize import decimals_used, histogram_quantile, lvl_bin  # noqa: E402
from preflight import (  # noqa: E402
    IDX_ID,
    IDX_PRICE,
    IDX_TIME,
    SPAN,
    SYMBOL,
    _byte_lines,
    archive_identity,
    archive_path,
    months,
)
from probe_binance_aggtrades import AutoCorr  # noqa: E402
from probe_binance_trades import EventStats  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
OUT = ROOT / "analysis/targets"

IDX_QTY = 2
MAX_LAG = 50
# The only ACF lags any target reads. F4 takes lag 1; F5 takes 1, 10 and 50.
REPORTED_LAGS = (1, 10, 50)
LVL_BINS = 120

TARGET_SCHEMA_VERSION = 1

# The equivalence gate. Values recorded in analysis/cadence.json, produced by the
# standalone probe over this exact archive. Full float precision, not rounded:
# an exact comparison is only meaningful against the exact recorded value.
EQUIVALENCE_MONTH = (2026, 6)
EQUIVALENCE_EXPECT = {
    "mean_event_duration_s": 0.17104059420590498,
    "duration_dispersion_cv2": 4.618827832310333,
    "duration_acf_lag1": 0.32204142581620676,
    "duration_acf_lag5": 0.22388204486699373,
    "children_mean": 8.49053255324216,
    "children_single_frac": 0.5586834546003685,
    "levels_mean": 2.2471110339199503,
}

# F3 through F6 have NO existing Binance counterpart: characterize.py has only
# ever run on the Kraken corpus, whose whole-second timestamps make its cadence
# output incomparable. So the gate covers 7 of the 14 targets - exactly half.
# The other 7 are guarded only by importing their estimators unchanged, which is
# weaker, and this file says so rather than implying the gate covered everything.
UNGATED_TARGETS = (
    "size_round_frac", "size_dispersion", "return_acf_lag1",
    "abs_acf_lag1", "abs_acf_lag10", "abs_acf_lag50", "zero_change_frac",
)


def targets_path(year: int, month: int) -> Path:
    return OUT / f"{SYMBOL}-targets-{year:04d}-{month:02d}.json"


def compute_targets(path: Path) -> dict:
    """One traversal. Fifteen targets, every estimator imported."""
    events = EventStats(True)
    # Only lags 1, 10 and 50 are ever reported, so only those are accumulated.
    # Bit-identical to the full sweep at those lags and roughly sixteen times
    # cheaper in the inner loop; conformance_f3_f6 asserts the identity.
    ret_acf = AutoCorr(MAX_LAG, lags=REPORTED_LAGS)
    abs_acf = AutoCorr(MAX_LAG, lags=REPORTED_LAGS)
    size_hist = [0] * LVL_BINS
    # Only decimals <= 2 is ever read (size_round_frac), so a single counter
    # replaces the nine-bin histogram; the count is the same integer.
    size_round = 0
    zero_change = 0
    change_n = 0
    prev_px = None
    prev_log = 0.0
    rows = 0

    # CPython hot-loop bindings: global and attribute lookups hoisted out of a
    # loop that runs ~128M times per archive.
    log = math.log
    events_push = events.push
    ret_push = ret_acf.push
    abs_push = abs_acf.push
    _decimals_used = decimals_used
    _lvl_bin = lvl_bin

    started = time.time()
    with zipfile.ZipFile(path) as zf:
        info = zf.infolist()[0]
        with zf.open(info) as stream:
            for line in _byte_lines(stream):
                if line.endswith(b"\r"):
                    line = line[:-1]
                if not line:
                    continue
                # FULL split, no maxsplit: the real dumps carry a seventh
                # trailing column (is_best_match) beyond the documented six,
                # so capping at 5 would leave parts[5] as b"True,True" and
                # misread every buyer-maker row.
                parts = line.split(b",")
                if not parts[IDX_ID].lstrip(b"-").isdigit():
                    continue
                stamp_text = parts[IDX_TIME]
                if not stamp_text.isdigit():
                    continue
                rows += 1

                price_text = parts[IDX_PRICE]
                qty_text = parts[IDX_QTY]
                stamp = int(stamp_text)
                side_text = parts[5]
                side = side_text == b"True" or side_text == b"true"

                # F1 and F2, through the class that produced cadence.json. It
                # takes the price as an opaque key for the level count, so the
                # raw field is passed exactly as the standalone probe passes it.
                events_push(stamp, side, price_text)

                # float() parses ASCII bytes directly, so neither field is
                # decoded; decimals_used counts on the raw bytes the same way.
                px = float(price_text)
                qty = float(qty_text)

                if _decimals_used(qty_text) < 3:
                    size_round += 1
                if qty > 0:
                    size_hist[_lvl_bin(qty)] += 1

                if prev_px is not None:
                    change_n += 1
                    if px == prev_px:
                        zero_change += 1
                    # log(prev_px) is the previous row's log(px), retained
                    # rather than recomputed - the identical double either way.
                    log_px = log(px)
                    ret = log_px - prev_log
                    ret_push(ret)
                    abs_push(abs(ret))
                    prev_log = log_px
                else:
                    prev_log = log(px)
                prev_px = px

    report = events.report()
    racf = ret_acf.acf()
    aacf = abs_acf.acf()
    p50 = histogram_quantile(size_hist, 0.50)
    p90 = histogram_quantile(size_hist, 0.90)

    return {
        "rows": rows,
        "elapsed_s": round(time.time() - started, 1),
        "targets": {
            # F1 cadence
            "mean_event_duration_s": report["parent_gap"]["mean_s"],
            "duration_dispersion_cv2": report["parent_gap"]["cv2"],
            "duration_acf_lag1": report["parent_gap"]["acf_lag1"],
            "duration_acf_lag5": report["parent_gap"]["acf_lag5"],
            # F2 sweep
            "children_mean": report["children"]["mean"],
            "children_single_frac": report["children"]["single_frac"],
            "levels_mean": report["levels"]["mean"],
            # F3 size shape
            "size_round_frac": (size_round / rows) if rows else None,
            "size_dispersion": (p90 / p50) if (p90 and p50) else None,
            # F4 return ACF
            "return_acf_lag1": racf[0] if racf else None,
            # F5 absolute-return ACF
            "abs_acf_lag1": aacf[0] if aacf else None,
            "abs_acf_lag10": aacf[9] if len(aacf) > 9 else None,
            "abs_acf_lag50": aacf[49] if len(aacf) > 49 else None,
            # F6 zero change
            "zero_change_frac": (zero_change / change_n) if change_n else None,
        },
    }


def mode_equivalence() -> int:
    year, month = EQUIVALENCE_MONTH
    path = archive_path(year, month)
    print("equivalence gate: F1 and F2 against analysis/cadence.json")
    print(f"archive {path.name}\n", flush=True)
    result = compute_targets(path)
    targets = result["targets"]

    failures = 0
    for name, expected in EQUIVALENCE_EXPECT.items():
        actual = targets[name]
        ok = actual == expected
        if not ok:
            failures += 1
        print(f"  {'ok  ' if ok else 'FAIL'}  {name:<26} {actual!r}")
        if not ok:
            print(f"        expected {expected!r}")
    print()
    print("ungated targets, no Binance counterpart exists to check them against:")
    for name in UNGATED_TARGETS:
        print(f"    {name:<26} {targets[name]!r}")
    print()
    print(f"elapsed {result['elapsed_s']}s for {result['rows']:,} rows")
    print(f"gate covers {len(EQUIVALENCE_EXPECT)} of "
          f"{len(EQUIVALENCE_EXPECT) + len(UNGATED_TARGETS)} targets")
    print("EQUIVALENT" if not failures else "EQUIVALENCE FAILED")
    return 1 if failures else 0


def build_month(year: int, month: int) -> dict:
    path = targets_path(year, month)
    archive = archive_path(year, month)
    identity = archive_identity(archive)
    if path.exists():
        try:
            cached = json.loads(path.read_text())
        except json.JSONDecodeError:
            cached = None
        if (
            cached
            and cached.get("complete")
            and cached.get("target_schema_version") == TARGET_SCHEMA_VERSION
            and cached.get("archive_identity", {}).get("published_sha256")
            == identity.get("published_sha256")
            and cached.get("archive_identity", {}).get("size") == identity.get("size")
        ):
            cached["from_cache"] = True
            return cached

    result = compute_targets(archive)
    result.update({
        "symbol": SYMBOL,
        "year": year,
        "month": month,
        "target_schema_version": TARGET_SCHEMA_VERSION,
        "archive_identity": identity,
        "complete": True,
    })
    OUT.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(result, indent=2, sort_keys=True))
    os.replace(tmp, path)
    result["from_cache"] = False
    return result


def mode_build(jobs: int = 4) -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    wanted = list(months(*SPAN))
    workers = max(1, min(len(wanted), jobs))
    print(f"target vectors for {len(wanted)} months, {workers} workers\n", flush=True)
    done = 0
    with ProcessPoolExecutor(max_workers=workers) as pool:
        futures = {
            pool.submit(build_month, year, month): (year, month)
            for year, month in wanted
        }
        for future in as_completed(futures):
            year, month = futures[future]
            result = future.result()
            done += 1
            tag = "cached" if result["from_cache"] else f"{result['elapsed_s']}s"
            # PROGRESS ONLY. No target value is printed, deliberately.
            #
            # An earlier version of this line displayed children_mean,
            # mean_event_duration_s and zero_change_frac for every month, which
            # made the progress log a cross-month disclosure channel for three
            # of the fourteen targets - inside the one stage whose contract is
            # that the table is frozen WITHOUT inspection. Knowing one month's
            # level is harmless; seeing nineteen months side by side is the rank
            # information the association test exists to measure.
            print(
                f"[{done:2d}/{len(wanted)}] {year:04d}-{month:02d}  "
                f"rows {result['rows']:,}  {tag}",
                flush=True,
            )
    print()
    print(f"all {len(wanted)} months built")
    return 0


def mode_freeze() -> int:
    wanted = list(months(*SPAN))
    table = {}
    for year, month in wanted:
        path = targets_path(year, month)
        if not path.exists():
            print(f"{year:04d}-{month:02d}  missing; cannot freeze")
            return 1
        table[f"{year:04d}-{month:02d}"] = json.loads(path.read_text())["targets"]

    frozen = ROOT / "analysis/targets-frozen.json"
    if frozen.exists():
        print(f"{frozen.name} already exists; refusing to overwrite a frozen table")
        return 1
    frozen.write_text(json.dumps({
        "symbol": SYMBOL,
        "target_schema_version": TARGET_SCHEMA_VERSION,
        "gated_targets": sorted(EQUIVALENCE_EXPECT),
        "ungated_targets": sorted(UNGATED_TARGETS),
        "months": table,
    }, indent=2, sort_keys=True))
    print(f"froze {len(table)} months to {frozen.name}")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("equivalence", "build", "freeze"))
    parser.add_argument("--jobs", type=int, default=4)
    args = parser.parse_args()
    if args.mode == "build":
        sys.exit(mode_build(args.jobs))
    sys.exit({"equivalence": mode_equivalence, "freeze": mode_freeze}[args.mode]())


if __name__ == "__main__":
    main()
