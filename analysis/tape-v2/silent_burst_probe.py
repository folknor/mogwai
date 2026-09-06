#!/usr/bin/env python3
"""How often does volume burst without the range following?

A burst is a minute whose volume is over `--vol-mult` times the median of
the trailing thirty traded minutes; it is silent when its H-L is under
`--range-mult` times the trailing median range. Reported per phase as the
share of bursts that are silent, and the burst rate itself, so a tape
whose bursts are too quiet and a tape that bursts too often are told
apart.

    uv --directory analysis/tape-v2 run python silent_burst_probe.py [--csv ...]
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import polars as pl

from tape_v2.session import PHASES, session_columns

HERE = Path(__file__).resolve().parent
NAMES = [n for n, _, _ in PHASES]
WINDOW = 30


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--csv", nargs="*", type=Path, default=None)
    p.add_argument("--vol-mult", type=float, default=3.0)
    p.add_argument("--range-mult", type=float, default=1.2)
    args = p.parse_args()
    if args.csv:
        frames = []
        for path in args.csv:
            frame = pl.read_csv(path).filter(pl.col("trade_count") > 0)
            frames.append(
                frame.select(
                    pl.col("open_ts").alias("ts_event"),
                    pl.lit(path.stem).alias("symbol"),
                    pl.col("high"), pl.col("low"),
                    pl.col("volume").cast(pl.Float64),
                )
            )
        raw = pl.concat(frames)
    else:
        raw = pl.read_parquet(HERE / "data" / "bars-1m" / "MNQ.parquet").select(
            "ts_event", "symbol", "high", "low", "volume"
        )
    bars = (
        session_columns(raw)
        .filter((pl.col("session_minute") < 1380) & (pl.col("weekday") <= 5))
        .with_columns(
            (pl.col("symbol") + "|" + pl.col("session_date").cast(pl.Utf8)).alias("key"),
            (pl.col("high") - pl.col("low")).alias("hl"),
        )
        .sort("key", "session_minute")
        .with_columns(
            pl.col("volume").shift(1).rolling_median(WINDOW).over("key").alias("vol_med"),
            pl.col("hl").shift(1).rolling_median(WINDOW).over("key").alias("hl_med"),
        )
        .filter(pl.col("vol_med").is_not_null() & (pl.col("vol_med") > 0) & (pl.col("hl_med") > 0))
        .with_columns(
            (pl.col("volume") > args.vol_mult * pl.col("vol_med")).alias("burst"),
            (pl.col("hl") < args.range_mult * pl.col("hl_med")).alias("quiet"),
        )
    )
    print(f"{bars['key'].n_unique()} sessions; burst = volume over {args.vol_mult}x the trailing "
          f"{WINDOW}-minute median, silent = H-L under {args.range_mult}x its trailing median")
    print("phase".ljust(9) + "minutes".rjust(9) + "bursts".rjust(8) + "per 1000".rjust(10) + "silent".rjust(8) + "silent share".rjust(14) + "  median range mult of bursts")
    for name in NAMES:
        sel = bars.filter(pl.col("phase") == name)
        n = sel.height
        bursts = sel.filter(pl.col("burst"))
        silent = bursts.filter(pl.col("quiet")).height
        ratio = (bursts["hl"] / bursts["hl_med"]).to_numpy()
        med = float(np.median(ratio)) if len(ratio) else float("nan")
        print(
            f"{name:<9}{n:9d}{bursts.height:8d}{1000 * bursts.height / n:10.1f}{silent:8d}"
            f"{(silent / bursts.height if bursts.height else float('nan')):14.2f}  {med:.2f}"
        )


if __name__ == "__main__":
    main()
