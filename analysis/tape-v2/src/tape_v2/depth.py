"""Depth extraction: one parent's front-month pre-trade books out of `mbp-10`.

The corpus holds 46 days of `mbp-10` from 2026-07-20. Every day file
carries every instrument, so a one-product read decodes the whole day.
This module caches, per day, the ten-level pre-trade book at each parent
observation as `data/micro/mbp-10/<parent>/<day>.parquet`, and
`depth_stats` aggregates the non-parametric conditioned profile the
ladder fit selects its family from.

Each `mbp-10` record carries the book state after its event is applied,
so the pre-trade book of a trade is the book columns of the record
before it. Trades are grouped into parents by the frozen
`(ts, side)` run rule with the same ten-microsecond merge the `tbbo`
extraction uses, and the parent's book is the first child's pre-trade
book.

What the profile can and cannot identify, stated where the fit reads it:
ten published levels bound what is visible, so the profile identifies
displayed depth shape through level ten and says nothing about where
liquidity actually ends. `depth_levels` stays a declared display bound;
only the shape is fitted.

Prices are kept on Databento's fixed grid and converted to ticks with
the day's `min_price_increment`, so level gaps are exact integers.
"""

from __future__ import annotations

import json
import multiprocessing
import sys
import time
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
import polars as pl
from databento import DBNStore

from .corpus import DATA_DIR
from .frontmonth import day_files
from .micro import FIXED_SCALE, definitions, front_symbol_table, micro_dir
from .session import session_columns

CHUNK = 2_000_000
MERGE_NS = 10_000
LEVELS = 10
PHASE_ORDER = ["all", "open", "asia", "london", "ny_pre", "ny_open", "ny_mid", "ny_close"]
SPREAD_STATES = [("1", 1, 2), ("2", 2, 3), ("3+", 3, None)]
TOUCH_BUCKETS = [("1-2", 1, 3), ("3-5", 3, 6), ("6+", 6, None)]

LEVEL_COLUMNS = [
    f"{side}_{kind}_{i:02d}"
    for side in ("bid", "ask")
    for kind in ("px", "sz")
    for i in range(LEVELS)
]


def read_trade_books(path: str, ids: set[int]) -> pl.DataFrame:
    """Every trade of the given instruments with its pre-trade ten-level
    book, decoded in chunks.

    An `mbp-10` day of every instrument does not fit in memory, so the
    shift to the previous record - the book the trade struck - happens
    per chunk, per instrument, with the last record of each instrument
    carried across the chunk boundary. Only trade rows and their
    pre-books accumulate; everything else is dropped with the chunk.
    File order is the exchange's event order and is what makes "the
    previous record" meaningful, so no re-sort happens here.
    """
    store = DBNStore.from_file(path)
    columns = ["ts_event", "sequence", "action", "side", "size", *LEVEL_COLUMNS]
    frames: list[pl.DataFrame] = []
    carry: pl.DataFrame | None = None
    for chunk in store.to_df(
        count=CHUNK, price_type="fixed", map_symbols=False, pretty_ts=False
    ):
        chunk = chunk.reset_index()
        chunk = chunk[chunk["instrument_id"].isin(ids)]
        if not len(chunk):
            continue
        frame = pl.from_pandas(chunk[["instrument_id", *columns]]).with_columns(
            pl.lit(False).alias("carried")
        )
        if carry is not None:
            frame = pl.concat([carry.with_columns(pl.lit(True).alias("carried")), frame])
        carry = frame.group_by("instrument_id", maintain_order=True).last().drop("carried")
        shifted = frame.with_columns(
            [
                pl.col(c).shift(1).over("instrument_id").alias(f"pre_{c}")
                for c in LEVEL_COLUMNS
            ]
        )
        trades = shifted.filter(
            (pl.col("action") == "T") & ~pl.col("carried")
        ).drop("carried", *LEVEL_COLUMNS)
        if trades.height:
            frames.append(trades)
    if not frames:
        return pl.DataFrame()
    return pl.concat(frames)


def extract_day(
    parent: str,
    day: str,
    def_path: str,
    mbp_path: str,
    front_symbol: str | None,
) -> pl.DataFrame:
    ids, tick = definitions(def_path, parent)
    if not ids or tick <= 0.0:
        return pl.DataFrame()
    all_trades = read_trade_books(mbp_path, set(ids))
    if all_trades.height == 0:
        return pl.DataFrame()
    by_symbol = {v: k for k, v in ids.items()}
    if front_symbol in by_symbol:
        front_id = by_symbol[front_symbol]
    else:
        front_id = int(
            all_trades.group_by("instrument_id")
            .agg(pl.col("size").sum())
            .sort("size", descending=True)["instrument_id"][0]
        )
    tick_fixed = round(tick * FIXED_SCALE)
    trades = (
        all_trades.filter(
            (pl.col("instrument_id") == front_id) & pl.col("side").is_in(["A", "B"])
        )
        .drop_nulls(subset=["pre_bid_px_00", "pre_ask_px_00"])
        .sort("ts_event", "sequence")
        .with_columns(
            pl.col("ts_event").cast(pl.Int64),
            pl.col("size").cast(pl.Int64),
        )
    )
    if trades.height == 0:
        return pl.DataFrame()
    key_change = (
        (pl.col("ts_event") - pl.col("ts_event").shift(1) > MERGE_NS)
        | (pl.col("side") != pl.col("side").shift(1))
    )
    grouped = trades.with_columns(
        key_change.fill_null(True).cast(pl.Int64).cum_sum().alias("parent")
    )
    parents = grouped.group_by("parent", maintain_order=True).agg(
        pl.col("ts_event").first(),
        pl.col("side").first(),
        pl.col("size").sum().alias("parent_size"),
        *[pl.col(f"pre_{c}").first().alias(c) for c in LEVEL_COLUMNS],
    )
    px_cols = [c for c in LEVEL_COLUMNS if "_px_" in c]
    sz_cols = [c for c in LEVEL_COLUMNS if "_sz_" in c]
    return parents.drop("parent").with_columns(
        *[
            (pl.col(c) // tick_fixed).cast(pl.Int64).alias(c.replace("_px_", "_ticks_"))
            for c in px_cols
        ],
        *[pl.col(c).cast(pl.Int64) for c in sz_cols],
        pl.lit(day).alias("day"),
        pl.lit(tick).alias("tick"),
    ).drop(px_cols)


def _worker(args: tuple[str, str, str, str, str | None, str]) -> tuple[str, int]:
    parent, day, def_path, mbp_path, front_symbol, out_path = args
    started = time.time()
    try:
        frame = extract_day(parent, day, def_path, mbp_path, front_symbol)
    except Exception as err:
        print(f"{day}: {err!r}", file=sys.stderr)
        return day, -1
    if frame.height:
        frame.write_parquet(out_path)
    print(
        f"  {day}: {frame.height} parents in {time.time() - started:.0f}s",
        file=sys.stderr,
    )
    return day, frame.height


def extract_mbp10(parent: str, day_first: str, day_last: str, workers: int = 4) -> Path:
    defs = day_files("definition", day_first, day_last)
    mbp = day_files("mbp-10", day_first, day_last)
    days = sorted(set(defs) & set(mbp))
    out_dir = micro_dir("mbp-10", parent)
    out_dir.mkdir(parents=True, exist_ok=True)
    fronts = front_symbol_table(parent)
    jobs = []
    for day in days:
        out_path = out_dir / f"{day}.parquet"
        if out_path.exists():
            continue
        jobs.append((parent, day, defs[day], mbp[day], fronts.get(day), str(out_path)))
    print(f"{parent}: {len(jobs)} days to extract, {len(days) - len(jobs)} cached")
    context = multiprocessing.get_context("spawn")
    total = 0
    with ProcessPoolExecutor(max_workers=workers, mp_context=context) as pool:
        for _day, rows in pool.map(_worker, jobs):
            if rows > 0:
                total += rows
    print(f"wrote {out_dir}: {total} parents over {len(jobs)} new days")
    return out_dir


def load_mbp10(parent: str) -> pl.DataFrame:
    out_dir = micro_dir("mbp-10", parent)
    paths = sorted(out_dir.glob("*.parquet"))
    if not paths:
        raise SystemExit(f"no extracted mbp-10 under {out_dir}; run `tape-v2 depth-extract`")
    return pl.concat([pl.read_parquet(p) for p in paths]).sort("ts_event")


# ------------------------------------------------------------- statistics


def quantiles(values: np.ndarray, probs: list[float]) -> dict:
    if values.size == 0:
        return {}
    out = {f"p{int(p * 100)}": float(np.quantile(values, p)) for p in probs}
    out["mean"] = float(values.mean())
    return out


def struck_matrix(frame: pl.DataFrame, kind: str) -> np.ndarray:
    """Level columns of the struck side as one (n, LEVELS) array: an
    aggressor buy strikes the ask ladder, a sell the bid."""
    buys = frame["side"].to_numpy() == "A"
    ask = np.column_stack(
        [frame[f"ask_{kind}_{i:02d}"].to_numpy() for i in range(LEVELS)]
    )
    bid = np.column_stack(
        [frame[f"bid_{kind}_{i:02d}"].to_numpy() for i in range(LEVELS)]
    )
    return np.where(buys[:, None], ask, bid)


def profile_block(frame: pl.DataFrame) -> dict:
    """The non-parametric depth profile of one conditioned slice."""
    sz = struck_matrix(frame, "sz").astype(float)
    ticks = struck_matrix(frame, "ticks").astype(float)
    buys = frame["side"].to_numpy() == "A"
    # Signed distance from the touch in ticks, positive going deeper.
    away = np.where(buys[:, None], ticks - ticks[:, :1], ticks[:, :1] - ticks)
    occupied = (sz > 0) & np.isfinite(ticks) & (ticks > 0)
    touch = sz[:, 0]
    out: dict = {"n": int(frame.height)}
    out["occupancy"] = [float(occupied[:, i].mean()) for i in range(LEVELS)]
    # Price gaps between consecutive occupied levels: the declared ladder
    # assumes every intervening tick is occupied (gap 1 everywhere).
    gaps = np.diff(away, axis=1)
    both = occupied[:, :-1] & occupied[:, 1:]
    flat = gaps[both]
    out["gap_pmf"] = {
        "1": float((flat == 1).mean()) if flat.size else float("nan"),
        "2": float((flat == 2).mean()) if flat.size else float("nan"),
        "3+": float((flat >= 3).mean()) if flat.size else float("nan"),
    }
    out["size_by_level"] = [
        quantiles(sz[occupied[:, i], i], [0.1, 0.5, 0.9]) for i in range(LEVELS)
    ]
    with np.errstate(divide="ignore", invalid="ignore"):
        ratio = sz / touch[:, None]
    out["ratio_to_touch_by_level"] = [
        quantiles(ratio[occupied[:, i] & (touch > 0), i], [0.1, 0.5, 0.9])
        for i in range(1, LEVELS)
    ]
    cum = np.where(occupied, sz, 0.0).cumsum(axis=1)
    out["cum_depth_by_level"] = [
        quantiles(cum[:, i], [0.1, 0.5, 0.9]) for i in range(LEVELS)
    ]
    # Cumulative depth within a tick distance of the touch, the quantity
    # the crossing's walk actually meets.
    out["cum_depth_by_ticks"] = {
        str(t): quantiles(
            np.where(occupied & (away < t), sz, 0.0).sum(axis=1), [0.1, 0.5, 0.9]
        )
        for t in (2, 4, 8)
    }
    # The bound-adequacy diagnostic: how often an executed parent size
    # exceeds the struck side's displayed depth within k levels. Executed
    # size cannot see unfilled demand, so this is a model diagnostic, not
    # an identified exhaustion rate.
    size = frame["parent_size"].to_numpy().astype(float)
    out["size_over_depth"] = {
        str(k): float((size > cum[:, k - 1]).mean()) for k in (4, 8, 10)
    }
    return out


def depth_stats(parent: str) -> None:
    frame = load_mbp10(parent)
    frame = session_columns(frame, "ts_event")
    frame = frame.filter(pl.col("phase") != "closed")
    spread = (pl.col("ask_ticks_00") - pl.col("bid_ticks_00")).alias("spread")
    touch = (
        pl.when(pl.col("side") == "A")
        .then(pl.col("ask_sz_00"))
        .otherwise(pl.col("bid_sz_00"))
        .alias("touch")
    )
    frame = frame.with_columns(spread, touch).filter(pl.col("spread") >= 1)
    out: dict = {"parent": parent, "days": sorted(frame["day"].unique().to_list())}
    for phase in PHASE_ORDER:
        sliced = frame if phase == "all" else frame.filter(pl.col("phase") == phase)
        if sliced.height == 0:
            continue
        block: dict = {"pooled": profile_block(sliced)}
        for label, lo, hi in SPREAD_STATES:
            rows = sliced.filter(
                (pl.col("spread") >= lo)
                & (pl.col("spread") < hi if hi else pl.lit(True))
            )
            if rows.height:
                block[f"spread_{label}"] = profile_block(rows)
        for label, lo, hi in TOUCH_BUCKETS:
            rows = sliced.filter(
                (pl.col("touch") >= lo)
                & (pl.col("touch") < hi if hi else pl.lit(True))
            )
            if rows.height:
                block[f"touch_{label}"] = profile_block(rows)
        out[phase] = block
    # Monthly stability: the struck-side level means per month, for the
    # pooling trap - a smooth pooled multiplier no individual month or
    # state follows.
    months: dict = {}
    for month in sorted({d[:7] for d in frame["day"].to_list()}):
        rows = frame.filter(pl.col("day").str.starts_with(month))
        sz = struck_matrix(rows, "sz").astype(float)
        occ = sz > 0
        months[month] = [
            float(sz[occ[:, i], i].mean()) if occ[:, i].any() else float("nan")
            for i in range(LEVELS)
        ]
    out["monthly_level_means"] = months
    path = DATA_DIR / "micro" / f"{parent}-depth-profile.json"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(out))
    print(f"wrote {path}: {frame.height} parents, {len(out['days'])} days")
