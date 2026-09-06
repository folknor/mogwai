"""Emit a preset's `[instrument.calendar.envelope]` block from the real
profile, with the provenance entries that go with it.

The envelope is the L0 shape the generator applies per minute of session:
`volume` is the per-session normalised median volume (1.0 is the
session's mean minute) and `range` the per-session normalised median
high-low range (1.0 is the session's median minute), both over the full
sessions the profile was built from. `weekday_weight` is the median
session level by session weekday relative to the Monday-to-Friday mean,
indexed Sunday to Saturday with the two closed days at the 1.0
convention. `session_open_minute_of_day` is the calendar's local open,
declared.

Numbers are rounded to four decimals: the noise floor of a 260-session
median is well above that, and a preset is read by people.
"""

from __future__ import annotations

from pathlib import Path

import polars as pl

from .corpus import DATA_DIR
from .session import OPEN_MINUTE_OF_DAY, SESSION_MINUTES


def _array(name: str, values: list[float], per_line: int = 8) -> str:
    lines = [f"{name} = ["]
    for i in range(0, len(values), per_line):
        chunk = ", ".join(f"{v:.4f}" for v in values[i : i + per_line])
        lines.append(f"  {chunk},")
    lines.append("]")
    return "\n".join(lines)


def envelope_block(parent: str, corpus: str, window: str) -> str:
    profile = pl.read_parquet(
        DATA_DIR / "profile" / f"{parent}-real-profile.parquet"
    ).sort("session_minute")
    levels = pl.read_parquet(
        DATA_DIR / "profile" / f"{parent}-real-levels.parquet"
    )
    minutes = profile["session_minute"].to_list()
    if minutes != list(range(SESSION_MINUTES)):
        raise SystemExit(
            f"profile covers {len(minutes)} minutes, not every one of "
            f"{SESSION_MINUTES}; the envelope needs a full grid"
        )
    volume = profile["volume_norm_p50"].to_list()
    rng = profile["range_norm_p50"].to_list()
    by_weekday = (
        levels.group_by("weekday")
        .agg(pl.col("volume_mean").median())
        .sort("weekday")
    )
    weekday = dict(
        zip(
            by_weekday["weekday"].to_list(),
            by_weekday["volume_mean"].to_list(),
            strict=True,
        )
    )
    mean = sum(weekday[d] for d in range(1, 6)) / 5.0
    # polars weekday: 1 = Monday .. 7 = Sunday. The envelope's index is
    # Sunday = 0 .. Saturday = 6, matching `dow_weight`.
    weights = [1.0] + [weekday[d] / mean for d in range(1, 6)] + [1.0]

    sessions = levels["session_key"].n_unique()
    header = (
        "# The L0 activity envelope: per-minute-of-session shape, fitted as\n"
        f"# the median over {sessions} full sessions.\n"
        f"# Corpus: {corpus}.\n"
        f"# Window: {window}.\n"
        "# `volume` is normalised so the session's mean minute is 1.0 and\n"
        "# `range` so the session's median minute is 1.0; the generator\n"
        "# derives its per-parent volatility multiplier from the two as\n"
        "# range / sqrt(volume). Minute 0 is the 17:00 Chicago reopen, 930\n"
        "# the 08:30 cash open, 1320 the 15:00 settlement. Level is not\n"
        "# carried here: it is a slow regime and belongs to the driver.\n"
        "[instrument.calendar.envelope]\n"
        f"session_open_minute_of_day = {OPEN_MINUTE_OF_DAY}\n"
        "# Session weekday, Sunday to Saturday; the closed days are 1.0 by\n"
        "# convention. Median session level relative to the weekday mean.\n"
        f"weekday_weight = [{', '.join(f'{w:.4f}' for w in weights)}]\n"
    )
    body = _array("volume", volume) + "\n" + _array("range", rng) + "\n"
    fitted = (
        f'{{ kind = "fitted", corpus = "{corpus}", window = "{window}" }}'
    )
    provenance = (
        '"calendar.envelope.session_open_minute_of_day" = '
        '{ kind = "declared", rationale = "17:00 Chicago session open" }\n'
        f'"calendar.envelope.weekday_weight" = {fitted}\n'
        f'"calendar.envelope.volume" = {fitted}\n'
        f'"calendar.envelope.range" = {fitted}\n'
    )
    return header + body + "\n[provenance]\n" + provenance


def write_envelope_toml(parent: str, corpus: str, window: str) -> Path:
    out = DATA_DIR / "profile" / f"{parent}-envelope.toml"
    out.write_text(envelope_block(parent, corpus, window))
    print(f"wrote {out}")
    return out
