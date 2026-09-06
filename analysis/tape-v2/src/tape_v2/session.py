"""The CME session frame and the v2 phase taxonomy.

A session opens at 17:00 Chicago time and closes at 16:00 the next civil
day; the hour between is the daily maintenance closure. Minute of session
runs 0 at 17:00 through 1379 at 15:59; a bar stamped inside the closure
gets a minute of 1380 or more and the phase `closed`. The session date is
the civil day the session closes on, so Sunday evening belongs to Monday's
session and the Monday session is the one that follows the weekend.

Timestamps are converted through the `America/Chicago` zone, DST included,
which is the intake step the v1 winter failure demanded: a fixed UTC offset
is wrong for half the year.

The phase boundaries are the programme's declared taxonomy (Chicago clock),
with the two opening phases folded into one `open` bucket here and split
by weekday where the fit needs it: the Monday session's open follows the
weekend and the others follow the maintenance hour.

    open      17:00 - 18:00   minute    0 -   59
    asia      18:00 - 02:00   minute   60 -  539
    london    02:00 - 07:00   minute  540 -  839
    ny_pre    07:00 - 08:30   minute  840 -  929
    ny_open   08:30 - 10:30   minute  930 - 1049
    ny_mid    10:30 - 13:00   minute 1050 - 1199
    ny_close  13:00 - 16:00   minute 1200 - 1379
"""

from __future__ import annotations

import polars as pl

CHICAGO = "America/Chicago"
OPEN_MINUTE_OF_DAY = 17 * 60
SESSION_MINUTES = 23 * 60

PHASES: list[tuple[str, int, int]] = [
    ("open", 0, 60),
    ("asia", 60, 540),
    ("london", 540, 840),
    ("ny_pre", 840, 930),
    ("ny_open", 930, 1050),
    ("ny_mid", 1050, 1200),
    ("ny_close", 1200, 1380),
]

CASH_OPEN_MINUTE = 930
CASH_CLOSE_MINUTE = 1320


def phase_of(minute: int) -> str:
    for name, lo, hi in PHASES:
        if lo <= minute < hi:
            return name
    return "closed"


def phase_expr(minute: pl.Expr) -> pl.Expr:
    expr = pl.lit("closed")
    for name, lo, hi in reversed(PHASES):
        expr = (
            pl.when((minute >= lo) & (minute < hi))
            .then(pl.lit(name))
            .otherwise(expr)
        )
    return expr


def session_columns(
    frame: pl.DataFrame,
    ts_col: str = "ts_event",
    utc_offset_minutes: int | None = None,
) -> pl.DataFrame:
    """Add session_date, weekday (1 = Monday), session_minute and phase.

    `ts_col` is nanoseconds since the epoch, UTC. Real data is framed
    through the Chicago zone; a generated tape is framed by the fixed
    offset its preset's calendar declares (`utc_offset_minutes`, the
    permanent CDT clock of the futures presets), because the generator
    does not model the DST transition and its dates are the 1970 origin
    where the zone would place the sessions an hour off.
    """
    if utc_offset_minutes is None:
        local = (
            pl.from_epoch(pl.col(ts_col), time_unit="ns")
            .dt.replace_time_zone("UTC")
            .dt.convert_time_zone(CHICAGO)
        )
    else:
        local = pl.from_epoch(
            pl.col(ts_col) + utc_offset_minutes * 60 * 1_000_000_000,
            time_unit="ns",
        )
    # `dt.hour()` and `dt.minute()` are Int8 in polars; hour * 60 overflows
    # it silently, so widen before any arithmetic.
    hour = local.dt.hour().cast(pl.Int32)
    minute = local.dt.minute().cast(pl.Int32)
    minute_of_day = hour * 60 + minute
    session_minute = (
        pl.when(minute_of_day >= OPEN_MINUTE_OF_DAY)
        .then(minute_of_day - OPEN_MINUTE_OF_DAY)
        .otherwise(minute_of_day + 1440 - OPEN_MINUTE_OF_DAY)
    )
    session_date = (
        pl.when(minute_of_day >= OPEN_MINUTE_OF_DAY)
        .then(local.dt.date() + pl.duration(days=1))
        .otherwise(local.dt.date())
    )
    return frame.with_columns(
        session_date.alias("session_date"),
        session_minute.alias("session_minute"),
    ).with_columns(
        pl.col("session_date").dt.weekday().alias("weekday"),
        phase_expr(pl.col("session_minute")).alias("phase"),
    )
