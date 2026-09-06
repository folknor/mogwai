#!/usr/bin/env python3
"""Variance ratios out to the session horizon, real and candidate.

    uv --directory analysis/tape-v2 run python vr_long.py [--csv ...]
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import polars as pl

from tape_v2.session import session_columns

HERE = Path(__file__).resolve().parent
HORIZONS = [1, 5, 30, 60, 120, 300, 690, 1379]


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--csv", nargs="*", type=Path, default=None)
    args = p.parse_args()
    if args.csv:
        frames = []
        for path in args.csv:
            frame = pl.read_csv(path).filter(pl.col("trade_count") > 0)
            frames.append(
                frame.select(
                    pl.col("open_ts").alias("ts_event"),
                    pl.lit(path.stem).alias("symbol"),
                    pl.col("close"),
                )
            )
        raw = pl.concat(frames)
    else:
        raw = pl.read_parquet(HERE / "data" / "bars-1m" / "MNQ.parquet").select(
            "ts_event", "symbol", "close"
        )
    bars = session_columns(raw).filter(
        (pl.col("session_minute") < 1380) & (pl.col("weekday") <= 5)
    ).with_columns(
        (pl.col("symbol") + "|" + pl.col("session_date").cast(pl.Utf8)).alias("key")
    )
    sessions = []
    for _, g in bars.sort("key", "session_minute").group_by("key", maintain_order=True):
        if g.height >= 1300 and g["symbol"].n_unique() == 1:
            sessions.append(g["close"].to_numpy())
    print(f"{len(sessions)} sessions")
    v1 = np.var(np.concatenate([np.diff(c) for c in sessions]))
    for h in HORIZONS:
        moves = np.concatenate([c[h:] - c[:-h] for c in sessions if len(c) > h])
        print(f"h={h:5d}: VR {np.var(moves) / (h * v1):.3f}   mean|move| {np.mean(np.abs(moves)):.1f}")
    # Session return variance against the sum of minute variances, per
    # session, then the median ratio: a per-session VR that a level mixture
    # cannot inflate.
    ratios = []
    for c in sessions:
        r = np.diff(c)
        if np.sum(r * r) > 0:
            ratios.append((c[-1] - c[0]) ** 2 / np.sum(r * r))
    print(f"per-session (close - open)^2 / sum r^2: mean {np.mean(ratios):.3f} (1.0 for a martingale)")


if __name__ == "__main__":
    main()
