#!/usr/bin/env python3
"""Pooled minute-return autocorrelation out to the session horizon, and its
weighted cumulative sum, which is the variance ratio minus one.

    uv --directory analysis/tape-v2 run python acf_long.py [--csv ...]
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import polars as pl

from tape_v2.session import session_columns

HERE = Path(__file__).resolve().parent
LAGS = [1, 2, 3, 5, 10, 20, 30, 60, 120, 240, 480, 690, 1000, 1300]


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--csv", nargs="*", type=Path, default=None)
    p.add_argument("--demean", action="store_true", help="subtract each session's mean return first")
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
    series = []
    for _, g in bars.sort("key", "session_minute").group_by("key", maintain_order=True):
        if g.height >= 1379 and g["symbol"].n_unique() == 1:
            r = np.diff(g["close"].to_numpy())
            if args.demean:
                r = r - r.mean()
            series.append(r)
    print(f"{len(series)} sessions")
    den = sum(float(np.dot(r, r)) for r in series)
    acf = {}
    for k in range(1, 1380):
        acf[k] = sum(float(np.dot(r[:-k], r[k:])) for r in series if len(r) > k) / den
    for h in [60, 300, 690, 1379]:
        cum = 2 * sum((1 - k / h) * acf[k] for k in range(1, h))
        print(f"VR({h}) - 1 from acf: {cum:+.3f}")
    print("acf at lags: " + "  ".join(f"{k}:{acf[k]:+.4f}" for k in LAGS))
    # Where the mass sits: sum of acf over lag bands.
    bands = [(1, 5), (6, 30), (31, 120), (121, 480), (481, 1379)]
    print("acf sum by band: " + "  ".join(f"{a}-{b}:{sum(acf[k] for k in range(a, b + 1)):+.3f}" for a, b in bands))


if __name__ == "__main__":
    main()
