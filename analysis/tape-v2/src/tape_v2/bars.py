"""Real one-minute bars for one front-month contract, in the chart gate's
CSV shape.

The chart gate (`analysis/plot_tape.py`) reads the bars CSV `mogwai gen
--type bars` emits: `open_ts,close_ts,open,high,low,close,volume,
trade_count` with nanosecond timestamps. This module produces the same
shape from the corpus so a real week and a generated week render through
one tool and are compared by eye on equal terms.

Selection: a parent (ES, MNQ, ...) and a closed range of split days. The
first day's `definition` records map instrument id to raw symbol; outright
futures of the parent are those with `instrument_class == "F"` and `asset`
equal to the parent. Among them the contract with the largest summed
`ohlcv-1m` volume over the range is the front month, which is the
programme's "one contract per product per day" rule collapsed to one
contract per range. A range that straddles a roll would pick one side and
show the other side's thinner tape; keep ranges inside a contract's front
period.

`ohlcv-1m` carries no trade count, so `trade_count` here is a presence
flag: 1 for a minute with volume, 0 otherwise. The chart only tests it for
zero, which is what marks an empty window grey. Anything reading this CSV
as a count is reading the wrong file.

Where a split day is present in two jobs (the top-up daemon refetches a
trailing window), the job whose id carries the later submission date wins.
That is a stated rule, not directory order.
"""

from __future__ import annotations

import sys
from pathlib import Path

import polars as pl
from databento import DBNStore

from .corpus import DATA_DIR, load_index

NS_PER_MIN = 60 * 1_000_000_000

BARS_HEADER = [
    "open_ts",
    "close_ts",
    "open",
    "high",
    "low",
    "close",
    "volume",
    "trade_count",
]


def files_for(schema: str, day_first: str, day_last: str) -> list[Path]:
    """Paths of the day files for `schema` covering [day_first, day_last].

    Only day-split files are considered, which is every file in the
    corpus today; a multi-day file would need a different reader.
    """
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
        .sort("day_start")
    )
    have = set(rows["day_start"].to_list())
    wanted = _days_between(day_first, day_last)
    missing = [d for d in wanted if d not in have]
    if missing:
        print(
            f"{schema}: no file for split days {missing} "
            "(weekend days are expected to be absent)",
            file=sys.stderr,
        )
    return [Path(p) for p in rows["path"].to_list()]


def _days_between(day_first: str, day_last: str) -> list[str]:
    from datetime import date, timedelta

    start = date(int(day_first[:4]), int(day_first[4:6]), int(day_first[6:]))
    end = date(int(day_last[:4]), int(day_last[4:6]), int(day_last[6:]))
    out = []
    cursor = start
    while cursor <= end:
        out.append(cursor.strftime("%Y%m%d"))
        cursor += timedelta(days=1)
    return out


def outrights(parent: str, day: str) -> dict[int, str]:
    """instrument id -> raw symbol for the parent's outright futures."""
    paths = files_for("definition", day, day)
    if not paths:
        raise SystemExit(f"no definition file for {day}")
    store = DBNStore.from_file(paths[0])
    frame = pl.from_pandas(
        store.to_df(map_symbols=False, pretty_ts=False).reset_index()
    )
    picked = frame.filter(
        (pl.col("asset") == parent) & (pl.col("instrument_class") == "F")
    ).select("instrument_id", "raw_symbol")
    return dict(
        zip(
            picked["instrument_id"].to_list(),
            picked["raw_symbol"].to_list(),
            strict=True,
        )
    )


def minute_bars(day_first: str, day_last: str, ids: set[int]) -> pl.DataFrame:
    """All ohlcv-1m records for the given instrument ids over the range."""
    frames = []
    for path in files_for("ohlcv-1m", day_first, day_last):
        store = DBNStore.from_file(path)
        pandas_frame = store.to_df(
            price_type="float", map_symbols=False, pretty_ts=False
        ).reset_index()
        frame = pl.from_pandas(pandas_frame)
        frames.append(frame.filter(pl.col("instrument_id").is_in(list(ids))))
    if not frames:
        raise SystemExit("no ohlcv-1m files in range")
    return pl.concat(frames)


def front_month_bars(
    parent: str, day_first: str, day_last: str
) -> tuple[str, pl.DataFrame]:
    """The bars CSV frame for the front month, plus the symbol chosen."""
    symbols = outrights(parent, day_first)
    if not symbols:
        raise SystemExit(f"no outright futures for {parent} on {day_first}")
    bars = minute_bars(day_first, day_last, set(symbols))
    volume = (
        bars.group_by("instrument_id")
        .agg(pl.col("volume").sum())
        .sort("volume", descending=True)
    )
    front_id = int(volume["instrument_id"][0])
    front = symbols[front_id]
    ranked = ", ".join(
        f"{symbols[int(i)]}={v}"
        for i, v in zip(
            volume["instrument_id"].to_list(),
            volume["volume"].to_list(),
            strict=True,
        )
    )
    print(f"{parent}: front month {front}; volume by contract: {ranked}")
    chosen = bars.filter(pl.col("instrument_id") == front_id).sort(
        "ts_event"
    )
    ts = chosen["ts_event"]
    if ts.dtype != pl.Int64:
        ts = ts.cast(pl.Datetime("ns")).cast(pl.Int64)
    out = pl.DataFrame(
        {
            "open_ts": ts,
            "close_ts": ts + NS_PER_MIN,
            "open": chosen["open"],
            "high": chosen["high"],
            "low": chosen["low"],
            "close": chosen["close"],
            "volume": chosen["volume"],
            "trade_count": (chosen["volume"] > 0).cast(pl.Int64),
        }
    )
    return front, out


def write_bars(parent: str, day_first: str, day_last: str) -> Path:
    front, frame = front_month_bars(parent, day_first, day_last)
    out_dir = DATA_DIR / "e0"
    out_dir.mkdir(parents=True, exist_ok=True)
    path = out_dir / f"{parent}-{front}-{day_first}-{day_last}-1m.csv"
    frame.select(BARS_HEADER).write_csv(path)
    first = frame["open_ts"][0]
    last = frame["open_ts"][-1]
    print(f"wrote {path}: {frame.height} bars, open_ts {first}..{last}")
    return path
