"""Sub-minute extraction: one parent's front-month prints out of `tbbo`.

Every `tbbo` day file carries every instrument in the corpus, so a
one-product read decodes the whole day. This module does that once per day
and caches the front month's prints as `data/micro/tbbo/<parent>/<day>.
parquet`, one row per print with the pre-trade touch beside it, so the
target statistics (`micro_stats`) run on a few hundred small files rather
than a hundred gigabytes of zstd.

Front month per day is the contract the one-minute extractor chose
(`data/bars-1m/<parent>-days.parquet`); a day that table lacks falls back to
the outright with the most `tbbo` volume that day.

Prices are kept on Databento's fixed grid (int64, 1e-9 units) and
converted to ticks with the day's `min_price_increment` from `definition`,
so a level count is an exact integer and never a float comparison.
"""

from __future__ import annotations

import multiprocessing
import sys
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import polars as pl
from databento import DBNStore

from .corpus import DATA_DIR
from .frontmonth import day_files

FIXED_SCALE = 1_000_000_000
CHUNK = 2_000_000

PRINT_COLUMNS = [
    "ts_event",
    "ts_recv",
    "side",
    "price",
    "size",
    "sequence",
    "bid_px_00",
    "ask_px_00",
    "bid_sz_00",
    "ask_sz_00",
    "bid_ct_00",
    "ask_ct_00",
]


def micro_dir(schema: str, parent: str) -> Path:
    return DATA_DIR / "micro" / schema / parent


def definitions(def_path: str, parent: str) -> tuple[dict[int, str], float]:
    """instrument id -> raw symbol for the parent's outrights, and the tick.

    `min_price_increment` is read as a float price; every outright of one
    parent shares it, so the first is taken.
    """
    frame = pl.from_pandas(
        DBNStore.from_file(def_path)
        .to_df(price_type="float", map_symbols=False, pretty_ts=False)
        .reset_index()
    )
    picked = frame.filter(
        (pl.col("asset") == parent) & (pl.col("instrument_class") == "F")
    )
    ids = dict(
        zip(
            picked["instrument_id"].to_list(),
            picked["raw_symbol"].to_list(),
            strict=True,
        )
    )
    tick = float(picked["min_price_increment"][0]) if picked.height else 0.0
    return ids, tick


def front_symbol_table(parent: str) -> dict[str, str]:
    path = DATA_DIR / "bars-1m" / f"{parent}-days.parquet"
    if not path.exists():
        return {}
    days = pl.read_parquet(path)
    return dict(zip(days["day"].to_list(), days["symbol"].to_list(), strict=True))


def read_prints(path: str, ids: set[int]) -> pl.DataFrame:
    """Every `tbbo` print of the given instruments, decoded in chunks."""
    store = DBNStore.from_file(path)
    frames: list[pl.DataFrame] = []
    for chunk in store.to_df(
        count=CHUNK, price_type="fixed", map_symbols=False, pretty_ts=False
    ):
        chunk = chunk.reset_index()
        chunk = chunk[chunk["instrument_id"].isin(ids)]
        if len(chunk):
            frames.append(pl.from_pandas(chunk[["instrument_id", *PRINT_COLUMNS]]))
    if not frames:
        return pl.DataFrame()
    return pl.concat(frames)


def extract_day(
    parent: str,
    day: str,
    def_path: str,
    tbbo_path: str,
    front_symbol: str | None,
) -> pl.DataFrame:
    ids, tick = definitions(def_path, parent)
    if not ids or tick <= 0.0:
        return pl.DataFrame()
    prints = read_prints(tbbo_path, set(ids))
    if prints.height == 0:
        return pl.DataFrame()
    by_symbol = {v: k for k, v in ids.items()}
    if front_symbol in by_symbol:
        front_id = by_symbol[front_symbol]
    else:
        front_id = int(
            prints.group_by("instrument_id")
            .agg(pl.col("size").sum())
            .sort("size", descending=True)["instrument_id"][0]
        )
    tick_fixed = round(tick * FIXED_SCALE)
    out = (
        prints.filter(pl.col("instrument_id") == front_id)
        .sort("ts_event", "sequence")
        .with_columns(
            pl.col("ts_event").cast(pl.Int64),
            pl.col("ts_recv").cast(pl.Int64),
            (pl.col("price") // tick_fixed).cast(pl.Int64).alias("price_ticks"),
            (pl.col("bid_px_00") // tick_fixed).cast(pl.Int64).alias("bid_ticks"),
            (pl.col("ask_px_00") // tick_fixed).cast(pl.Int64).alias("ask_ticks"),
            pl.col("size").cast(pl.Int64),
            pl.col("bid_sz_00").cast(pl.Int64).alias("bid_sz"),
            pl.col("ask_sz_00").cast(pl.Int64).alias("ask_sz"),
            pl.col("bid_ct_00").cast(pl.Int64).alias("bid_ct"),
            pl.col("ask_ct_00").cast(pl.Int64).alias("ask_ct"),
            pl.lit(ids[front_id]).alias("symbol"),
            pl.lit(day).alias("day"),
            pl.lit(tick).alias("tick"),
        )
        .select(
            "ts_event",
            "ts_recv",
            "side",
            "price_ticks",
            "size",
            "sequence",
            "bid_ticks",
            "ask_ticks",
            "bid_sz",
            "ask_sz",
            "bid_ct",
            "ask_ct",
            "symbol",
            "day",
            "tick",
        )
    )
    return out


def _worker(args: tuple[str, str, str, str, str | None, str]) -> tuple[str, int]:
    parent, day, def_path, tbbo_path, front_symbol, out_path = args
    started = time.time()
    try:
        frame = extract_day(parent, day, def_path, tbbo_path, front_symbol)
    except Exception as err:
        print(f"{day}: {err!r}", file=sys.stderr)
        return day, -1
    if frame.height:
        frame.write_parquet(out_path)
    print(
        f"  {day}: {frame.height} prints in {time.time() - started:.0f}s",
        file=sys.stderr,
    )
    return day, frame.height


def extract_tbbo(
    parent: str, day_first: str, day_last: str, workers: int = 6
) -> Path:
    defs = day_files("definition", day_first, day_last)
    tbbo = day_files("tbbo", day_first, day_last)
    days = sorted(set(defs) & set(tbbo))
    out_dir = micro_dir("tbbo", parent)
    out_dir.mkdir(parents=True, exist_ok=True)
    fronts = front_symbol_table(parent)
    jobs = []
    for day in days:
        out_path = out_dir / f"{day}.parquet"
        if out_path.exists():
            continue
        jobs.append(
            (parent, day, defs[day], tbbo[day], fronts.get(day), str(out_path))
        )
    print(f"{parent}: {len(jobs)} days to extract, {len(days) - len(jobs)} cached")
    context = multiprocessing.get_context("spawn")
    total = 0
    with ProcessPoolExecutor(max_workers=workers, mp_context=context) as pool:
        for _day, rows in pool.map(_worker, jobs):
            if rows > 0:
                total += rows
    print(f"wrote {out_dir}: {total} prints over {len(jobs)} new days")
    return out_dir


def load_tbbo(parent: str, day_first: str | None = None, day_last: str | None = None) -> pl.DataFrame:
    out_dir = micro_dir("tbbo", parent)
    paths = sorted(out_dir.glob("*.parquet"))
    if day_first:
        paths = [p for p in paths if p.stem >= day_first]
    if day_last:
        paths = [p for p in paths if p.stem <= day_last]
    if not paths:
        raise SystemExit(f"no extracted tbbo under {out_dir}; run `tape-v2 micro-extract`")
    return pl.concat([pl.read_parquet(p) for p in paths]).sort("ts_event", "sequence")
