"""Print the `status` records for one parent's outrights on one day.

Intake step 3 checks the declared phase taxonomy against the exchange's
own session state. The `status` schema carries the venue's trading
status transitions per instrument; printing a day of them for the front
month answers, from the exchange's mouth, when the session opens, when it
closes, and whether any intraday halt exists on that product.
"""

from __future__ import annotations

import polars as pl
from databento import DBNStore

from .frontmonth import day_files, outrights_from
from .session import session_columns


def status_probe(parent: str, day: str) -> None:
    defs = day_files("definition", day, day)
    stats = day_files("status", day, day)
    if day not in defs or day not in stats:
        raise SystemExit(f"definition or status missing for {day}")
    symbols = outrights_from(defs[day], parent)
    frame = pl.from_pandas(
        DBNStore.from_file(stats[day])
        .to_df(map_symbols=False, pretty_ts=False)
        .reset_index()
    ).with_columns(pl.col("ts_event").cast(pl.Int64))
    frame = frame.filter(pl.col("instrument_id").is_in(list(symbols)))
    if frame.height == 0:
        print(f"no status records for {parent} outrights on {day}")
        return
    frame = session_columns(frame).with_columns(
        pl.col("instrument_id")
        .replace_strict(symbols, default=None)
        .alias("symbol"),
        (
            pl.from_epoch(pl.col("ts_event"), time_unit="ns")
            .dt.replace_time_zone("UTC")
            .dt.convert_time_zone("America/Chicago")
            .dt.strftime("%a %H:%M:%S")
        ).alias("chicago"),
    )
    cols = [
        c
        for c in (
            "chicago",
            "symbol",
            "action",
            "reason",
            "trading_event",
            "is_trading",
            "is_quoting",
            "is_short_sell_restricted",
        )
        if c in frame.columns
    ]
    with pl.Config(tbl_rows=-1, tbl_cols=-1, tbl_width_chars=200):
        print(frame.sort("ts_event").select(cols))
