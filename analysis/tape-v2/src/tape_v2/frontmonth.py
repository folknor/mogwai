"""Front-month one-minute bars for one parent across a day range, cached.

Per split day: the day's `definition` file maps instrument id to raw
symbol, the outrights of the parent are those with `instrument_class ==
"F"` and `asset == parent`, and the front month is the outright with the
largest `ohlcv-1m` volume that day. That is the programme's "one contract
per product per day" rule. Roll hygiene (dropping the day either side of a
symbol change) is applied downstream by whoever fits, from the per-day
table this writes beside the bars.

Output: `data/bars-1m/<parent>.parquet` with `ts_event, symbol, day, open,
high, low, close, volume`, and `data/bars-1m/<parent>-days.parquet` with
`day, symbol, bars, volume`. Days are processed in parallel; each worker
opens two files.
"""

from __future__ import annotations

import multiprocessing
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import polars as pl
from databento import DBNStore

from .corpus import DATA_DIR, load_index

BAR_COLUMNS = [
    "ts_event",
    "instrument_id",
    "open",
    "high",
    "low",
    "close",
    "volume",
]


def day_files(schema: str, day_first: str, day_last: str) -> dict[str, str]:
    """Split day -> path for `schema`, later job winning a duplicated day."""
    index = load_index()
    rows = (
        index.filter(
            (pl.col("schema") == schema)
            & (pl.col("day_start") == pl.col("day_end"))
            & (pl.col("day_start") >= day_first)
            & (pl.col("day_start") <= day_last)
        )
        .sort("day_start", "job_id")
        .group_by("day_start", maintain_order=True)
        .last()
    )
    return dict(
        zip(rows["day_start"].to_list(), rows["path"].to_list(), strict=True)
    )


def outrights_from(def_path: str, parent: str) -> dict[int, str]:
    frame = pl.from_pandas(
        DBNStore.from_file(def_path)
        .to_df(map_symbols=False, pretty_ts=False)
        .reset_index()
    )
    picked = frame.filter(
        (pl.col("asset") == parent) & (pl.col("instrument_class") == "F")
    )
    return dict(
        zip(
            picked["instrument_id"].to_list(),
            picked["raw_symbol"].to_list(),
            strict=True,
        )
    )


def extract_day(
    parent: str, day: str, def_path: str, bars_path: str
) -> pl.DataFrame:
    symbols = outrights_from(def_path, parent)
    if not symbols:
        return pl.DataFrame()
    pandas_frame = (
        DBNStore.from_file(bars_path)
        .to_df(price_type="float", map_symbols=False, pretty_ts=False)
        .reset_index()
    )
    bars = pl.from_pandas(pandas_frame[BAR_COLUMNS]).with_columns(
        pl.col("ts_event").cast(pl.Int64)
    )
    bars = bars.filter(pl.col("instrument_id").is_in(list(symbols)))
    if bars.height == 0:
        return pl.DataFrame()
    front_id = int(
        bars.group_by("instrument_id")
        .agg(pl.col("volume").sum())
        .sort("volume", descending=True)["instrument_id"][0]
    )
    return (
        bars.filter(pl.col("instrument_id") == front_id)
        .sort("ts_event")
        .with_columns(
            pl.lit(symbols[front_id]).alias("symbol"),
            pl.lit(day).alias("day"),
        )
        .select(
            "ts_event", "symbol", "day", "open", "high", "low", "close",
            "volume",
        )
    )


def _worker(args: tuple[str, str, str, str]) -> tuple[str, pl.DataFrame]:
    parent, day, def_path, bars_path = args
    try:
        return day, extract_day(parent, day, def_path, bars_path)
    except Exception as err:
        # Reported per day and the day skipped; one bad file must not
        # take the other three hundred with it.
        print(f"{day}: {err!r}", file=sys.stderr)
        return day, pl.DataFrame()


def extract(
    parent: str, day_first: str, day_last: str, workers: int = 8
) -> Path:
    defs = day_files("definition", day_first, day_last)
    bars = day_files("ohlcv-1m", day_first, day_last)
    days = sorted(set(defs) & set(bars))
    missing = sorted((set(defs) | set(bars)) - set(days))
    if missing:
        print(f"skipping {len(missing)} days lacking one of the two files")
    jobs = [(parent, d, defs[d], bars[d]) for d in days]
    frames: list[pl.DataFrame] = []
    # Spawn, never fork: the parent has already run polars to read the
    # index, so it carries a live thread pool, and a forked child inherits
    # its locks without the threads that hold them. The fork variant
    # deadlocked every worker silently on the first day.
    context = multiprocessing.get_context("spawn")
    with ProcessPoolExecutor(max_workers=workers, mp_context=context) as pool:
        for i, (_day, frame) in enumerate(pool.map(_worker, jobs), 1):
            if frame.height:
                frames.append(frame)
            if i % 25 == 0:
                print(f"  {i}/{len(jobs)} days", file=sys.stderr)
    if not frames:
        raise SystemExit(f"no bars for {parent} in {day_first}..{day_last}")
    out = pl.concat(frames).sort("ts_event")
    out_dir = DATA_DIR / "bars-1m"
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{parent}.parquet"
    out.write_parquet(path)
    per_day = (
        out.group_by("day", "symbol")
        .agg(pl.len().alias("bars"), pl.col("volume").sum())
        .sort("day")
    )
    per_day.write_parquet(out_dir / f"{parent}-days.parquet")
    rolls = per_day.filter(
        pl.col("symbol") != pl.col("symbol").shift(1)
    )["day"].to_list()[1:]
    print(
        f"wrote {path}: {out.height} bars over {per_day.height} days, "
        f"front month changed on {rolls}"
    )
    return path


def load_bars(parent: str) -> pl.DataFrame:
    path = DATA_DIR / "bars-1m" / f"{parent}.parquet"
    if not path.exists():
        raise SystemExit(f"{path} missing; run `tape-v2 extract` first")
    return pl.read_parquet(path)
