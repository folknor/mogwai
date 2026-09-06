#!/usr/bin/env python3
"""The first cash hour at fifteen seconds: the owner's scale.

Per session, over 13:30 to 14:30 UTC (session minutes 930 to 989): the
15-second bar range quantiles, the travel of 15-second closes, the hour's
range, the flat-close share, and the variance ratios of 1, 5, 15 and 60
minute returns against the 15-second return, all pooled across sessions.
Real bars come from the extracted tbbo prints on the run host, one month
at a time; a candidate is a gen bars CSV at 15 seconds framed on the
preset's fixed clock.

    uv --directory analysis/tape-v2 run python open15.py --real
    uv --directory analysis/tape-v2 run python open15.py --csv data/gen/MNQ-p33-s1-4w-15s.csv
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np
import polars as pl

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "src"))

from tape_v2.session import session_columns  # noqa: E402

NS = 1_000_000_000
STEP = 15 * NS
FIRST, LAST = 930, 990
HORIZONS = (4, 20, 60, 240)  # in 15 s bars: 1, 5, 15, 60 minutes


def bars_from_prints(prints: pl.DataFrame) -> pl.DataFrame:
    tick = float(prints["tick"][0])
    return (
        prints.filter(pl.col("side") != "N")
        .with_columns((pl.col("ts_event") // STEP * STEP).alias("ts"))
        .group_by("ts", maintain_order=True)
        .agg(
            (pl.col("price_ticks").max() * tick).alias("high"),
            (pl.col("price_ticks").min() * tick).alias("low"),
            (pl.col("price_ticks").last() * tick).alias("close"),
            pl.col("size").sum().alias("volume"),
        )
        .sort("ts")
    )


def bars_from_csv(path: Path) -> pl.DataFrame:
    return (
        pl.read_csv(path)
        .filter(pl.col("volume") > 0)
        .rename({"open_ts": "ts"})
        .select("ts", "high", "low", "close", "volume")
    )


def first_hour(bars: pl.DataFrame, offset: int | None) -> list[dict]:
    framed = session_columns(bars, "ts", offset).filter(
        (pl.col("session_minute") >= FIRST) & (pl.col("session_minute") < LAST)
    )
    rows = []
    for key, s in framed.group_by("session_date", maintain_order=True):
        if s.height < 220:
            continue
        s = s.sort("ts")
        close = s["close"].to_numpy()
        rng = (s["high"] - s["low"]).to_numpy()
        logc = np.log(close)
        r15 = np.diff(logc)
        row = {
            "session": str(key[0]),
            "bar_p50": float(np.median(rng)),
            "bar_p90": float(np.quantile(rng, 0.9)),
            "bar_max": float(rng.max()),
            "travel": float(np.abs(np.diff(close)).sum()),
            "hour_range": float(close.max() - close.min()),
            "flat": float((np.diff(close) == 0).mean()),
            "var15": float(np.mean(r15 * r15)),
        }
        for k in HORIZONS:
            r = logc[k::k] - logc[:-k:k]
            row[f"var{k}"] = float(np.mean(r * r)) if r.size else float("nan")
        rows.append(row)
    return rows


def report(rows: list[dict], label: str) -> None:
    if not rows:
        print(f"{label}: no full first hours")
        return
    frame = pl.DataFrame(rows)
    q = lambda col, p: float(np.quantile(frame[col].to_numpy(), p))  # noqa: E731
    v15 = float(frame["var15"].mean())
    vr = "  ".join(
        f"VR({k // 4}m) {float(frame[f'var{k}'].mean()) / (k * v15):.2f}" for k in HORIZONS
    )
    print(
        f"{label:<24} sessions {frame.height:3d}  "
        f"15s bar p50 {q('bar_p50', 0.1):.1f}/{q('bar_p50', 0.5):.1f}/{q('bar_p50', 0.9):.1f}  "
        f"bar p90 {q('bar_p90', 0.5):.1f}  bar max {q('bar_max', 0.5):.0f}  "
        f"travel {q('travel', 0.1):.0f}/{q('travel', 0.5):.0f}/{q('travel', 0.9):.0f}  "
        f"hour range {q('hour_range', 0.5):.0f}  travel/range {np.median(frame['travel'] / frame['hour_range']):.1f}  "
        f"flat {q('flat', 0.5):.2f}  {vr}"
    )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--parent", default="MNQ")
    ap.add_argument("--real", action="store_true")
    ap.add_argument("--csv", nargs="*", type=Path, default=[])
    args = ap.parse_args()
    if args.real:
        from tape_v2.micro import load_tbbo, micro_dir

        months = sorted({p.stem[:6] for p in micro_dir("tbbo", args.parent).glob("*.parquet")})
        rows: list[dict] = []
        for month in months:
            prints = load_tbbo(args.parent, month + "01", month + "31")
            rows.extend(first_hour(bars_from_prints(prints), None))
            print(f"  {month}: {len(rows)} sessions so far", file=sys.stderr)
        report(rows, "real year")
    for path in args.csv:
        report(first_hour(bars_from_csv(path), -300), path.stem[:24])


if __name__ == "__main__":
    main()
