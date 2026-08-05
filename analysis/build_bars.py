#!/usr/bin/env python3
"""Build 1-minute bars from the trade ticks, and gate them against vendor klines.

Brick 3 of `notes/sampling-frame-preregistration.md`. Bars are built from the
SAME ticks the tick side reads - never from vendor klines - so that a later
disagreement between the bar side and the tick side cannot be blamed on two
aggregation conventions.

The vendor klines earn one job and only one: for 2026-06, the single month where
held klines and held trades overlap, they are an INDEPENDENT check that the bar
construction is right. That is a stronger gate than self-consistency against the
tick side, which would pass even if the binning convention were wrong.

Agreement is preregistered in brick 3 and is EXACT. `Decimal` summation over the
decimal-string quantities is order-independent and lossless, so the usual reason
to allow a floating-point tolerance does not apply; OHLC are selected values
rather than sums and cannot drift at all. A disagreement is a FINDING about a
convention difference - bin boundary, self-trade handling, which trades the
vendor counts - to be identified and recorded, never a tolerance to widen until
it passes.

Modes:

    crosscheck   2026-06 tick bars against vendor klines, exact, the gate
    build        write per-month 1-minute bars for the confirmed span

Usage:
    python3 -u analysis/build_bars.py crosscheck
    python3 -u analysis/build_bars.py build
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
from decimal import Decimal
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

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

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "research/market-data"
OUT = ROOT / "analysis/bars"

# Preregistered binning. Left-closed, right-open, keyed by bin OPEN time.
MINUTE_US = 60_000_000

IDX_QTY = 2

# Vendor 1-second kline layout, read from the archive rather than assumed:
# open_time_us, open, high, low, close, volume, close_time_us, quote_volume,
# count, taker_buy_base, taker_buy_quote, ignore
K_OPEN_TIME, K_OPEN, K_HIGH, K_LOW, K_CLOSE, K_VOLUME = 0, 1, 2, 3, 4, 5
K_COUNT = 8

CROSSCHECK_MONTH = (2026, 6)


def kline_path(year: int, month: int) -> Path:
    return DEST / f"{SYMBOL}-1s-{year:04d}-{month:02d}.zip"


def bars_path(year: int, month: int) -> Path:
    return OUT / f"{SYMBOL}-1m-{year:04d}-{month:02d}.json"


class Bar:
    __slots__ = ("open", "high", "low", "close", "volume", "count")

    def __init__(self, price: Decimal, qty: Decimal) -> None:
        self.open = price
        self.high = price
        self.low = price
        self.close = price
        self.volume = qty
        self.count = 1

    def add(self, price: Decimal, qty: Decimal) -> None:
        if price > self.high:
            self.high = price
        if price < self.low:
            self.low = price
        self.close = price
        self.volume += qty
        self.count += 1

    def as_dict(self) -> dict:
        return {
            "open": str(self.open),
            "high": str(self.high),
            "low": str(self.low),
            "close": str(self.close),
            "volume": str(self.volume),
            "count": self.count,
        }


def bars_from_ticks(path: Path) -> tuple[dict[int, Bar], int]:
    """One streaming pass. Returns bars keyed by bin open time, plus row count."""
    bars: dict[int, Bar] = {}
    rows = 0
    with zipfile.ZipFile(path) as zf:
        info = zf.getinfo(zf.namelist()[0])
        with zf.open(info) as stream:
            for line in _byte_lines(stream):
                if line.endswith(b"\r"):
                    line = line.rstrip(b"\r")
                if not line:
                    continue
                parts = line.split(b",")
                if not parts[IDX_ID].lstrip(b"-").isdigit():
                    continue  # header
                stamp_text = parts[IDX_TIME]
                if not stamp_text.isdigit():
                    continue
                rows += 1
                # Truncation, never rounding: the bin is floor(t / 60s).
                key = int(stamp_text) // MINUTE_US
                price = Decimal(parts[IDX_PRICE].decode("ascii"))
                qty = Decimal(parts[IDX_QTY].decode("ascii"))
                bar = bars.get(key)
                if bar is None:
                    bars[key] = Bar(price, qty)
                else:
                    bar.add(price, qty)
    return bars, rows


def bars_from_klines(path: Path) -> dict[int, Bar]:
    """Aggregate vendor 1-second klines up to the same 1-minute grid.

    Seconds are already ordered in the archive, so open comes from the first
    second present in the minute and close from the last. A second with no
    trades is simply absent from the archive, which is why an absent MINUTE and
    a zero-trade minute have to be treated as the same thing on both sides.
    """
    bars: dict[int, Bar] = {}
    with zipfile.ZipFile(path) as zf:
        info = zf.getinfo(zf.namelist()[0])
        with zf.open(info) as stream:
            for line in _byte_lines(stream):
                if line.endswith(b"\r"):
                    line = line.rstrip(b"\r")
                if not line:
                    continue
                parts = line.split(b",")
                if not parts[K_OPEN_TIME].lstrip(b"-").isdigit():
                    continue
                count = int(parts[K_COUNT])
                if count == 0:
                    # A zero-trade second contributes no price and no volume.
                    # Including its flat OHLC would invent prices the tape never
                    # printed and would corrupt the minute's high and low.
                    continue
                key = int(parts[K_OPEN_TIME]) // MINUTE_US
                o = Decimal(parts[K_OPEN].decode("ascii"))
                h = Decimal(parts[K_HIGH].decode("ascii"))
                low = Decimal(parts[K_LOW].decode("ascii"))
                c = Decimal(parts[K_CLOSE].decode("ascii"))
                v = Decimal(parts[K_VOLUME].decode("ascii"))
                bar = bars.get(key)
                if bar is None:
                    bar = Bar(o, v)
                    bar.high = h
                    bar.low = low
                    bar.close = c
                    bar.count = count
                    bars[key] = bar
                else:
                    if h > bar.high:
                        bar.high = h
                    if low < bar.low:
                        bar.low = low
                    bar.close = c
                    bar.volume += v
                    bar.count += count
    return bars


def mode_crosscheck() -> int:
    year, month = CROSSCHECK_MONTH
    trades = archive_path(year, month)
    klines = kline_path(year, month)
    print(f"tick bars   {trades.name}")
    print(f"vendor      {klines.name}")
    print("agreement is EXACT on open, high, low, close, volume, count\n", flush=True)

    started = time.time()
    tick_bars, rows = bars_from_ticks(trades)
    print(f"built {len(tick_bars):,} tick minutes from {rows:,} rows "
          f"in {time.time() - started:.1f}s", flush=True)
    vendor_bars = bars_from_klines(klines)
    print(f"aggregated {len(vendor_bars):,} vendor minutes\n", flush=True)

    only_tick = sorted(set(tick_bars) - set(vendor_bars))
    only_vendor = sorted(set(vendor_bars) - set(tick_bars))
    shared = sorted(set(tick_bars) & set(vendor_bars))

    mismatches: dict[str, list] = {
        "open": [], "high": [], "low": [], "close": [], "volume": [], "count": [],
    }
    for key in shared:
        a, b = tick_bars[key], vendor_bars[key]
        if a.open != b.open:
            mismatches["open"].append(key)
        if a.high != b.high:
            mismatches["high"].append(key)
        if a.low != b.low:
            mismatches["low"].append(key)
        if a.close != b.close:
            mismatches["close"].append(key)
        if a.volume != b.volume:
            mismatches["volume"].append(key)
        if a.count != b.count:
            mismatches["count"].append(key)

    print(f"minutes only in tick bars   {len(only_tick)}")
    print(f"minutes only in vendor bars {len(only_vendor)}")
    print(f"minutes compared            {len(shared):,}")
    for field, keys in mismatches.items():
        print(f"  {field:<7} mismatches {len(keys):,}")

    failures = len(only_tick) + len(only_vendor) + sum(len(v) for v in mismatches.values())
    if failures:
        print("\nfirst disagreements, for diagnosis:")
        for field, keys in mismatches.items():
            for key in keys[:3]:
                a, b = tick_bars[key], vendor_bars[key]
                print(f"  minute {key} ({key * 60}s) {field}: "
                      f"tick {getattr(a, field)} vs vendor {getattr(b, field)}")
        for key in only_tick[:3]:
            print(f"  minute {key} present only in tick bars, "
                  f"count {tick_bars[key].count}")
        for key in only_vendor[:3]:
            print(f"  minute {key} present only in vendor bars, "
                  f"count {vendor_bars[key].count}")
        print("\nGATE FAILS - this is a convention finding to identify, "
              "not a tolerance to widen")
        return 1
    print("\nGATE PASSES - tick-built bars reproduce the vendor klines exactly")
    return 0


# ---------------------------------------------------------------------------
# The five features. Definitions lifted from select_windows.py so the CME rule
# is the thing under test, not a reimplementation of it.
# ---------------------------------------------------------------------------

FEATURE_SCHEMA_VERSION = 1

# A full 24/7 day is 1440 one-minute bars, replacing the CME pipeline's 1380 and
# its early-close handling. A crypto day is never short, so the normalization is
# a constant here rather than a correction.
FULL_DAY_MINUTES = 1440

# select_windows drops a day below this many bars as a holiday or half session.
# Kept proportional to the CME threshold of 1000 out of 1380.
MIN_DAY_MINUTES = 1044

DAY_US = 86_400_000_000
HOUR_US = 3_600_000_000

# `gap` is DELIBERATELY ABSENT, never zero. A 24/7 market has no overnight gap
# to measure, and writing 0.0 would encode a measured no-gap observation where
# the truth is that the quantity does not exist. Section 5.1 records that what
# is validated is a five-feature variant of the CME rule.
FEATURE_NAMES = ("rv", "vol_of_vol", "volume", "volume_cv", "zero_change")

# Only this one enters the operative Spearman association. The other four are
# computed for descriptive compatibility with the CME pipeline and MUST NOT
# influence ranking, thresholds, exclusions or the verdict.
OPERATIVE_FEATURE = "rv"


def daily_features(bars: dict[int, Bar]) -> dict[str, dict]:
    """Per-UTC-day features from 1-minute bars, following select_windows.py.

    The day replaces the CME session, since a 24/7 market has none. The
    single-largest-squared-return trim is RETAINED even though crypto has no
    contract roll: it is a fixed transformation of the CME rule under test, it
    applies uniformly to every day, and removing it would make this a different
    rule than the one Basket B uses.
    """
    days: dict[int, dict] = {}
    prev_close = None
    prev_minute = None
    prev_day = None

    for minute in sorted(bars):
        bar = bars[minute]
        day = (minute * MINUTE_US) // DAY_US
        slot = days.get(day)
        if slot is None:
            slot = {
                "ret2": 0.0, "n": 0, "volume": 0.0, "vols": [],
                "max_r2": 0.0, "zero": 0, "hourly": {},
            }
            days[day] = slot

        if prev_close is not None and prev_day == day:
            # A minute with no trade is absent from the bars, exactly as the
            # vendor omits it. Those are real zero-return, zero-volume
            # observations and dropping them would inflate zero_change and
            # volume_cv in precisely the illiquid regimes the strata care about.
            missing = minute - prev_minute - 1
            if missing > 0:
                slot["zero"] += missing
                slot["n"] += missing
                slot["vols"].extend([0.0] * missing)
            close = float(bar.close)
            r = math.log(close / prev_close)
            slot["ret2"] += r * r
            if r * r > slot["max_r2"]:
                slot["max_r2"] = r * r
            hour = ((minute * MINUTE_US) % DAY_US) // HOUR_US
            slot["hourly"][hour] = slot["hourly"].get(hour, 0.0) + r * r
            if r == 0.0:
                slot["zero"] += 1
            slot["n"] += 1

        volume = float(bar.volume)
        slot["volume"] += volume
        slot["vols"].append(volume)
        prev_close = float(bar.close)
        prev_minute = minute
        prev_day = day

    out: dict[str, dict] = {}
    for day, slot in days.items():
        if slot["n"] < MIN_DAY_MINUTES:
            continue
        vols = slot["vols"]
        mean_v = sum(vols) / len(vols)
        var_v = sum((v - mean_v) ** 2 for v in vols) / len(vols)
        hourly = [math.sqrt(v) for v in slot["hourly"].values()]
        mean_h = sum(hourly) / len(hourly) if hourly else 0.0
        var_h = (sum((h - mean_h) ** 2 for h in hourly) / len(hourly)) if hourly else 0.0
        trimmed = max(0.0, slot["ret2"] - slot["max_r2"])
        out[str(day)] = {
            "rv": math.sqrt(trimmed * FULL_DAY_MINUTES / slot["n"]),
            "vol_of_vol": math.sqrt(var_h) / mean_h if mean_h > 0 else 0.0,
            "volume": slot["volume"] * FULL_DAY_MINUTES / slot["n"],
            "volume_cv": math.sqrt(var_v) / mean_v if mean_v > 0 else 0.0,
            "zero_change": slot["zero"] / slot["n"],
        }
    return out


def median(values: list[float]) -> float:
    ordered = sorted(values)
    n = len(ordered)
    if n == 0:
        return 0.0
    mid = n // 2
    if n % 2:
        return ordered[mid]
    return (ordered[mid - 1] + ordered[mid]) / 2.0


def monthly_features(daily: dict[str, dict]) -> dict[str, float]:
    return {
        name: median([d[name] for d in daily.values()])
        for name in FEATURE_NAMES
    }


def build_month(year: int, month: int) -> dict:
    """Worker: one traversal, bars plus features, atomic write, resumable."""
    path = bars_path(year, month)
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
            and cached.get("feature_schema_version") == FEATURE_SCHEMA_VERSION
            and cached.get("archive_identity", {}).get("published_sha256")
            == identity.get("published_sha256")
            and cached.get("archive_identity", {}).get("size") == identity.get("size")
        ):
            cached["from_cache"] = True
            return cached

    started = time.time()
    bars, rows = bars_from_ticks(archive)
    daily = daily_features(bars)
    payload = {
        "symbol": SYMBOL,
        "year": year,
        "month": month,
        "rows": rows,
        "minutes": len(bars),
        "feature_schema_version": FEATURE_SCHEMA_VERSION,
        "archive_identity": identity,
        "days_used": len(daily),
        "daily_features": daily,
        "monthly_features": monthly_features(daily),
        "bars": {str(k): b.as_dict() for k, b in sorted(bars.items())},
        "elapsed_s": round(time.time() - started, 1),
        "complete": True,
    }
    OUT.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(payload, sort_keys=True))
    os.replace(tmp, path)
    payload["from_cache"] = False
    return payload


def mode_build(jobs: int = 4) -> int:
    OUT.mkdir(parents=True, exist_ok=True)
    wanted = list(months(*SPAN))
    workers = max(1, min(len(wanted), jobs))
    print(f"building 1-minute bars and features for {len(wanted)} months, "
          f"{workers} workers\n", flush=True)
    done = 0
    failures = 0
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
            monthly = result["monthly_features"]
            print(
                f"[{done:2d}/{len(wanted)}] {year:04d}-{month:02d}  "
                f"{result['minutes']:,} min  {result['days_used']} days  "
                f"rv {monthly['rv']:.6f}  {tag}",
                flush=True,
            )
            if result["days_used"] == 0:
                failures += 1
                print(f"           - no usable days", flush=True)
    print()
    if failures:
        print(f"{failures} month(s) produced no usable days")
        return 1
    print(f"all {len(wanted)} months built")
    return 0


def mode_freeze() -> int:
    """Assemble and FREEZE the 19-month bar-score table before any association."""
    wanted = list(months(*SPAN))
    table = {}
    for year, month in wanted:
        path = bars_path(year, month)
        if not path.exists():
            print(f"{year:04d}-{month:02d}  missing; cannot freeze")
            return 1
        payload = json.loads(path.read_text())
        table[f"{year:04d}-{month:02d}"] = payload["monthly_features"]

    frozen = ROOT / "analysis/bar-scores-frozen.json"
    if frozen.exists():
        print(f"{frozen.name} already exists; refusing to overwrite a frozen table")
        return 1
    payload = {
        "symbol": SYMBOL,
        "feature_schema_version": FEATURE_SCHEMA_VERSION,
        "operative_feature": OPERATIVE_FEATURE,
        "features_present": list(FEATURE_NAMES),
        "gap": "ABSENT by construction; a 24/7 market has no overnight gap and "
               "zero would falsely encode a measured no-gap value",
        "note": "Only the operative feature enters the Spearman association. The "
                "other four are descriptive compatibility with the CME pipeline "
                "and must not influence ranking, thresholds, exclusions or verdict.",
        "months": table,
    }
    frozen.write_text(json.dumps(payload, indent=2, sort_keys=True))
    print(f"froze {len(table)} months to {frozen.name}")
    print(f"\n{'month':<9} {'rv':>12} {'vol_of_vol':>12} {'volume':>16} "
          f"{'volume_cv':>11} {'zero_change':>12}")
    for key in sorted(table):
        row = table[key]
        print(f"{key:<9} {row['rv']:>12.6f} {row['vol_of_vol']:>12.6f} "
              f"{row['volume']:>16.2f} {row['volume_cv']:>11.4f} "
              f"{row['zero_change']:>12.6f}")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("crosscheck", "build", "freeze"))
    parser.add_argument("--jobs", type=int, default=4,
                        help="parallel workers; do not widen without a measured win")
    args = parser.parse_args()
    if args.mode == "build":
        sys.exit(mode_build(args.jobs))
    sys.exit({"crosscheck": mode_crosscheck, "freeze": mode_freeze}[args.mode]())


if __name__ == "__main__":
    main()
