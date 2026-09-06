"""Real bars at any interval from the extracted tbbo prints, in the chart
gate's CSV shape, so a sub-minute generated tape can sit beside the real
one on the compare page.

`ohlcv-1m` is the corpus's finest bar, and a fifteen-second bar is what
shows whether the layer below the minute looks right. The extracted
prints (`micro-extract`) carry every front-month print with its size, so
bars at any interval are one group-by away. `trade_count` here is the
print count, which is what the generator's bars carry too.
"""

from __future__ import annotations

from pathlib import Path

import polars as pl

from .corpus import DATA_DIR
from .micro import load_tbbo

NS = 1_000_000_000
HEADER = [
    "open_ts",
    "close_ts",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "trade_count",
]


def write_bars(parent: str, first: str, last: str, interval_s: int) -> Path:
    prints = load_tbbo(parent, first, last).filter(pl.col("side") != "N")
    step = interval_s * NS
    tick = float(prints["tick"][0])
    bars = (
        prints.with_columns((pl.col("ts_event") // step * step).alias("open_ts"))
        .group_by("open_ts", maintain_order=True)
        .agg(
            pl.col("price_ticks").first().alias("open"),
            pl.col("price_ticks").max().alias("high"),
            pl.col("price_ticks").min().alias("low"),
            pl.col("price_ticks").last().alias("close"),
            pl.col("size").sum().alias("volume"),
            pl.len().alias("trade_count"),
        )
        .sort("open_ts")
        .with_columns(
            (pl.col("open_ts") + step).alias("close_ts"),
            (pl.col("open") * tick).alias("open"),
            (pl.col("high") * tick).alias("high"),
            (pl.col("low") * tick).alias("low"),
            (pl.col("close") * tick).alias("close"),
        )
        .select(HEADER)
    )
    out_dir = DATA_DIR / "e0"
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{parent}-real-{first}-{last}-{interval_s}s.csv"
    bars.write_csv(path)
    print(f"wrote {path}: {bars.height} bars")
    return path
