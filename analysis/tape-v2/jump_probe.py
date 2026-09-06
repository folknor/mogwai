#!/usr/bin/env python3
"""What a big minute looks like: how big, when, and with how much volume.

Per phase over full sessions: the 1-minute H-L at p99, p99.9 and the max;
the largest minutes with their volume as a multiple of that session's
phase median; and the count of minutes over 100 points that came with
less than twice the phase's median volume, which is a jump nobody traded.

    uv --directory analysis/tape-v2 run python jump_probe.py [--csv ...]
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import polars as pl

from tape_v2.session import PHASES, session_columns

HERE = Path(__file__).resolve().parent
NAMES = [n for n, _, _ in PHASES]


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--csv", nargs="*", type=Path, default=None)
    p.add_argument("--top", type=int, default=8)
    args = p.parse_args()
    if args.csv:
        frames = []
        for path in args.csv:
            frame = pl.read_csv(path).filter(pl.col("trade_count") > 0)
            frames.append(
                frame.select(
                    pl.col("open_ts").alias("ts_event"),
                    pl.lit(path.stem).alias("symbol"),
                    pl.col("high"), pl.col("low"), pl.col("close"),
                    pl.col("volume").cast(pl.Float64),
                )
            )
        raw = pl.concat(frames)
    else:
        raw = pl.read_parquet(HERE / "data" / "bars-1m" / "MNQ.parquet").select(
            "ts_event", "symbol", "high", "low", "close", "volume"
        )
    bars = (
        session_columns(raw)
        .filter((pl.col("session_minute") < 1380) & (pl.col("weekday") <= 5))
        .with_columns(
            (pl.col("symbol") + "|" + pl.col("session_date").cast(pl.Utf8)).alias("key"),
            (pl.col("high") - pl.col("low")).alias("hl"),
        )
    )
    counts = bars.group_by("key").len()
    keep = counts.filter(pl.col("len") >= 1300)["key"]
    bars = bars.filter(pl.col("key").is_in(keep.implode()))
    bars = bars.with_columns(
        (pl.col("volume") / pl.col("volume").median().over("key", "phase")).alias("vol_ratio")
    )
    print(f"{bars['key'].n_unique()} sessions")
    print("\n== 1-minute H-L per phase: p99, p99.9, max; silent jumps (over 100 points, volume under 2x the phase median) ==")
    for name in NAMES:
        sel = bars.filter(pl.col("phase") == name)
        hl = sel["hl"].to_numpy()
        silent = sel.filter((pl.col("hl") > 100) & (pl.col("vol_ratio") < 2.0)).height
        loud = sel.filter(pl.col("hl") > 100).height
        print(
            f"{name:<9} p99 {np.quantile(hl, 0.99):6.1f}  p99.9 {np.quantile(hl, 0.999):6.1f}  "
            f"max {hl.max():6.1f}   minutes over 100: {loud:3d}, silent {silent:3d}"
        )
    print(f"\n== the {args.top} largest minutes in the overnight phases, with volume ratio ==")
    night = bars.filter(pl.col("phase").is_in(["open", "asia", "london"])).sort("hl", descending=True)
    for row in night.head(args.top).iter_rows(named=True):
        print(
            f"  {row['session_date']} minute {row['session_minute']:4d} {row['phase']:<7} "
            f"H-L {row['hl']:6.1f}  volume {row['volume']:7.0f}  x{row['vol_ratio']:.1f} the phase median"
        )
    print(f"\n== the {args.top} largest minutes anywhere, with volume ratio ==")
    for row in bars.sort("hl", descending=True).head(args.top).iter_rows(named=True):
        print(
            f"  {row['session_date']} minute {row['session_minute']:4d} {row['phase']:<8} "
            f"H-L {row['hl']:6.1f}  volume {row['volume']:7.0f}  x{row['vol_ratio']:.1f} the phase median"
        )
    big = bars.filter(pl.col("hl") > 60)
    print(
        f"\nminutes over 60 points: {big.height}; their volume ratio p10/p50/p90 = "
        f"{np.quantile(big['vol_ratio'].to_numpy(), [0.1, 0.5, 0.9]).round(1)}"
    )


if __name__ == "__main__":
    main()
