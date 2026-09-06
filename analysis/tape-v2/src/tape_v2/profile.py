"""The activity envelope: per-minute-of-session distributions and the
intake fits that summarise them.

This is the L0 measurement from the programme (intake steps 3 to 5) and
the activity half of the validation battery in one place, because they
are the same computation run on different tapes:

- `session_matrix` turns bars into one row per (session, minute) with
  volume and high-low range, in the Chicago session frame.
- `full_sessions` drops sessions that are not a full standard day.
  Holidays and half days are out of the programme's scope, so a session
  with fewer than `MIN_BARS` traded minutes is excluded and named.
- `quantiles` gives, per minute of session, the cross-session p10, p25,
  p50, p75 and p90 of volume and of range. That is the real-side
  distribution the battery's containment test reads.
- `fit_envelope` reduces the quantile profile to the intake knobs: a
  volume and a range multiplier per phase relative to `ny_mid`, an opening
  ramp `phi_inf + a * exp(-t / tau)` fitted on the first hour after the
  cash open and after each reopen, and the settlement spike at 15:00.

Range is the volatility proxy here because the input is one-minute bars.
The programme's per-phase volatility multiplier is expected near one;
the number that comes out is recorded either way.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

import numpy as np
import polars as pl
from scipy.optimize import curve_fit

from .corpus import DATA_DIR
from .session import (
    CASH_CLOSE_MINUTE,
    CASH_OPEN_MINUTE,
    PHASES,
    SESSION_MINUTES,
    session_columns,
)

MIN_BARS = 1300
QUANTILES = [0.10, 0.25, 0.50, 0.75, 0.90]
RAMP_MINUTES = 60
REFERENCE_PHASE = "ny_mid"


def session_matrix(bars: pl.DataFrame) -> pl.DataFrame:
    """One row per (tape, session_date, session_minute): volume, range.

    `tape` names the source tape (the corpus, or one generated seed), so
    several seeds can share a matrix without their minutes merging. A
    frame without the column is one tape, `real`.
    """
    if "tape" not in bars.columns:
        bars = bars.with_columns(pl.lit("real").alias("tape"))
    frame = session_columns(bars).filter(
        (pl.col("session_minute") < SESSION_MINUTES)
        & (pl.col("weekday") <= 5)
    )
    return (
        frame.group_by(
            "tape", "session_date", "weekday", "session_minute", "phase"
        )
        .agg(
            pl.col("volume").sum(),
            (pl.col("high").max() - pl.col("low").min()).alias("range"),
        )
        .with_columns(
            (pl.col("tape") + "|" + pl.col("session_date").cast(pl.Utf8))
            .alias("session_key")
        )
        # Shape and level are separated here. The real activity level
        # moves by a factor of several across months (a slow regime, the
        # programme's L1), so pooling raw minutes across a year gives a
        # band that is wide for the wrong reason. Each session is
        # normalised to its own mean minute volume and its own median
        # minute range; the shape band is then tight, and the per-session
        # level is a distribution of its own (see `session_levels`).
        .with_columns(
            (pl.col("volume") / pl.col("volume").mean().over("session_key"))
            .alias("volume_norm"),
            (pl.col("range") / pl.col("range").median().over("session_key"))
            .alias("range_norm"),
        )
        .sort("tape", "session_date", "session_minute")
    )


QUANTITIES = ["volume", "range", "volume_norm", "range_norm"]


def session_levels(matrix: pl.DataFrame) -> pl.DataFrame:
    """One row per session: the level that normalisation divided out."""
    return (
        matrix.group_by("session_key", "tape", "session_date", "weekday")
        .agg(
            pl.col("volume").mean().alias("volume_mean"),
            pl.col("volume").sum().alias("volume_total"),
            pl.col("range").median().alias("range_median"),
        )
        .sort("tape", "session_date")
    )


def full_sessions(
    matrix: pl.DataFrame, min_bars: int = MIN_BARS
) -> tuple[pl.DataFrame, list[str]]:
    counts = (
        matrix.filter(pl.col("volume") > 0)
        .group_by("session_key")
        .len()
    )
    dropped = sorted(
        counts.filter(pl.col("len") < min_bars)["session_key"].to_list()
    )
    kept = matrix.filter(~pl.col("session_key").is_in(dropped))
    return kept, dropped


def quantiles(matrix: pl.DataFrame) -> pl.DataFrame:
    aggs = []
    for q in QUANTILES:
        tag = f"p{int(q * 100):02d}"
        for quantity in QUANTITIES:
            aggs.append(
                pl.col(quantity).quantile(q).alias(f"{quantity}_{tag}")
            )
    aggs.append(pl.len().alias("sessions"))
    return (
        matrix.group_by("session_minute", "phase")
        .agg(aggs)
        .sort("session_minute")
    )


def _ramp(t: np.ndarray, phi_inf: float, a: float, tau: float) -> np.ndarray:
    return phi_inf + a * np.exp(-t / tau)


def fit_ramp(profile: pl.DataFrame, start: int, column: str) -> dict:
    window = profile.filter(
        (pl.col("session_minute") >= start)
        & (pl.col("session_minute") < start + RAMP_MINUTES)
    ).sort("session_minute")
    t = (window["session_minute"] - start).to_numpy().astype(float)
    y = window[column].to_numpy().astype(float)
    if len(t) < 10:
        return {"minutes": len(t), "fit": None}
    guess = [float(y[-10:].mean()), float(max(y[0] - y[-10:].mean(), 1)), 10]
    try:
        params, _ = curve_fit(
            _ramp, t, y, p0=guess, bounds=([0, 0, 0.5], [np.inf, np.inf, 600])
        )
    except RuntimeError:
        return {"minutes": len(t), "fit": None}
    phi_inf, a, tau = (float(v) for v in params)
    resid = y - _ramp(t, phi_inf, a, tau)
    return {
        "minutes": len(t),
        "phi_inf": phi_inf,
        "a": a,
        "tau_minutes": tau,
        "peak_over_floor": (phi_inf + a) / phi_inf if phi_inf else None,
        "first_minute": float(y[0]),
        "rmse": float(np.sqrt(np.mean(resid**2))),
    }


def phase_table(profile: pl.DataFrame) -> dict[str, dict]:
    per_phase = (
        profile.group_by("phase")
        .agg(
            pl.col("volume_p50").median().alias("volume"),
            pl.col("range_p50").median().alias("range"),
            pl.col("volume_norm_p50").median().alias("volume_norm"),
            pl.col("range_norm_p50").median().alias("range_norm"),
            pl.len().alias("minutes"),
        )
    )
    rows = {r["phase"]: r for r in per_phase.to_dicts()}
    ref = rows[REFERENCE_PHASE]
    out = {}
    for name, _, _ in PHASES:
        row = rows.get(name)
        if row is None:
            continue
        out[name] = {
            "minutes": int(row["minutes"]),
            "volume_median": float(row["volume"]),
            "range_median": float(row["range"]),
            "volume_mult": float(row["volume_norm"] / ref["volume_norm"]),
            "range_mult": float(row["range_norm"] / ref["range_norm"]),
        }
    return out


def fit_envelope(matrix: pl.DataFrame, profile: pl.DataFrame) -> dict:
    monday = quantiles(matrix.filter(pl.col("weekday") == 1))
    other = quantiles(matrix.filter(pl.col("weekday") > 1))
    close_phase = profile.filter(pl.col("phase") == "ny_close")
    settle = profile.filter(pl.col("session_minute") == CASH_CLOSE_MINUTE)
    close_floor = float(close_phase["volume_norm_p50"].median())
    levels = session_levels(matrix)
    level_summary = {}
    for column in ("volume_mean", "range_median"):
        series = levels[column]
        level_summary[column] = {
            f"p{int(q * 100):02d}": float(series.quantile(q))
            for q in QUANTILES
        }
    by_month = (
        levels.group_by(pl.col("session_date").dt.strftime("%Y-%m"))
        .agg(
            pl.col("volume_mean").median().alias("volume_mean"),
            pl.col("range_median").median().alias("range_median"),
        )
        .sort("session_date")
    )
    level_summary["by_month"] = {
        r["session_date"]: {
            "volume_mean": float(r["volume_mean"]),
            "range_median": float(r["range_median"]),
        }
        for r in by_month.to_dicts()
    }
    return {
        "reference_phase": REFERENCE_PHASE,
        "phases": phase_table(profile),
        "ramps": {
            "cash_open_volume": fit_ramp(
                profile, CASH_OPEN_MINUTE, "volume_norm_p50"
            ),
            "cash_open_range": fit_ramp(
                profile, CASH_OPEN_MINUTE, "range_norm_p50"
            ),
            "sunday_open_volume": fit_ramp(monday, 0, "volume_norm_p50"),
            "daily_open_volume": fit_ramp(other, 0, "volume_norm_p50"),
        },
        "settlement_spike": {
            "minute": CASH_CLOSE_MINUTE,
            "volume_norm_p50": float(settle["volume_norm_p50"][0]),
            "over_close_phase_median": float(settle["volume_norm_p50"][0])
            / close_floor
            if close_floor
            else None,
        },
        "session_level": level_summary,
    }


def build_profile(parent: str, bars: pl.DataFrame, label: str) -> dict:
    """Write the matrix, the quantile profile and the envelope for a tape.

    `label` names the tape: `real` for the corpus, or a seed tag for a
    generated one. Returns the envelope dictionary.
    """
    matrix, dropped = full_sessions(session_matrix(bars))
    profile = quantiles(matrix)
    envelope = fit_envelope(matrix, profile)
    sessions = matrix["session_key"].n_unique()
    envelope["parent"] = parent
    envelope["label"] = label
    envelope["sessions_used"] = int(sessions)
    envelope["sessions_dropped"] = dropped
    out_dir = DATA_DIR / "profile"
    out_dir.mkdir(parents=True, exist_ok=True)
    matrix.write_parquet(out_dir / f"{parent}-{label}-matrix.parquet")
    profile.write_parquet(out_dir / f"{parent}-{label}-profile.parquet")
    session_levels(matrix).write_parquet(
        out_dir / f"{parent}-{label}-levels.parquet"
    )
    with open(out_dir / f"{parent}-{label}-envelope.json", "w") as f:
        json.dump(envelope, f, indent=1)
    print_envelope(envelope)
    return envelope


def print_envelope(env: dict) -> None:
    print(
        f"\n{env['parent']} {env['label']}: {env['sessions_used']} full "
        f"sessions, dropped {len(env['sessions_dropped'])}: "
        f"{env['sessions_dropped']}"
    )
    print(
        "phase      minutes  vol_median  vol_mult  rng_median  rng_mult  "
        "(mult = shape, per-session normalised, relative to ny_mid)"
    )
    for name, row in env["phases"].items():
        print(
            f"{name:9s}  {row['minutes']:7d}  {row['volume_median']:10.1f}  "
            f"{row['volume_mult']:8.2f}  {row['range_median']:10.2f}  "
            f"{row['range_mult']:8.2f}"
        )
    print("ramps on the normalised median (1.0 = the session's mean minute):")
    for name, ramp in env["ramps"].items():
        if ramp.get("fit", "ok") is None:
            print(f"  {name}: no fit ({ramp['minutes']} minutes)")
            continue
        print(
            f"  {name}: floor {ramp['phi_inf']:.2f}, burst {ramp['a']:.2f}, "
            f"tau {ramp['tau_minutes']:.1f} min, peak/floor "
            f"{ramp['peak_over_floor']:.1f}, first minute "
            f"{ramp['first_minute']:.2f}, rmse {ramp['rmse']:.2f}"
        )
    spike = env["settlement_spike"]
    print(
        f"settlement minute {spike['minute']}: normalised volume p50 "
        f"{spike['volume_norm_p50']:.2f}, "
        f"{spike['over_close_phase_median']:.1f}x the close phase median"
    )
    level = env["session_level"]
    vol = level["volume_mean"]
    rng = level["range_median"]
    print(
        "session level, mean minute volume p10/p50/p90: "
        f"{vol['p10']:.0f} / {vol['p50']:.0f} / {vol['p90']:.0f}; "
        "median minute range p10/p50/p90: "
        f"{rng['p10']:.2f} / {rng['p50']:.2f} / {rng['p90']:.2f}"
    )
    months = level["by_month"]
    print(
        "  by month, volume: "
        + ", ".join(f"{m} {v['volume_mean']:.0f}" for m, v in months.items())
    )


def load_profile(parent: str, label: str) -> tuple[pl.DataFrame, pl.DataFrame]:
    out_dir = DATA_DIR / "profile"
    matrix = out_dir / f"{parent}-{label}-matrix.parquet"
    profile = out_dir / f"{parent}-{label}-profile.parquet"
    for path in (matrix, profile):
        if not path.exists():
            raise SystemExit(f"{path} missing; run `tape-v2 profile` first")
    return pl.read_parquet(matrix), pl.read_parquet(profile)


def bars_from_csv(path: Path) -> pl.DataFrame:
    """A gen bars CSV as the frame `session_matrix` expects."""
    frame = pl.read_csv(path)
    return (
        frame.filter(pl.col("trade_count") > 0)
        .rename({"open_ts": "ts_event"})
        .with_columns(pl.lit(path.stem).alias("tape"))
        .select("ts_event", "tape", "open", "high", "low", "close", "volume")
    )


def profile_real(parent: str) -> dict:
    from .frontmonth import load_bars

    return build_profile(parent, load_bars(parent), "real")


def profile_gen(parent: str, label: str, csvs: list[Path]) -> dict:
    bars = pl.concat([bars_from_csv(p) for p in csvs])
    if bars.height == 0:
        sys.exit("no traded bars in the generated CSVs")
    return build_profile(parent, bars, label)
