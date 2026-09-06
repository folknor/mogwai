#!/usr/bin/env python3
"""What is left of a minute once the envelope is divided out.

The envelope is the cross-session median of per-session-normalised volume
and range at each minute of session. Dividing a session's minutes by it
leaves a residual whose distribution and autocorrelation are what the
arrival and volatility processes must reproduce: the envelope is the
deterministic rate, the residual is the stochastic texture.

Per phase this prints the residual's quantiles, its log standard
deviation, and its autocorrelation at several lags, pooled over full
sessions. Then the session level (the mean the normalisation divided out)
and its day-to-day autocorrelation, which is the L1 target.

    .venv/bin/python analysis/tape-v2/residuals.py --parent MNQ [--label real]
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import polars as pl

HERE = Path(__file__).resolve().parent
DATA = HERE / "data" / "profile"
PHASES = ["open", "asia", "london", "ny_pre", "ny_open", "ny_mid", "ny_close"]
LAGS = [1, 2, 5, 10, 15, 30, 60, 120, 240]


def acf_pooled(groups: list[np.ndarray], lag: int) -> float:
    num = 0.0
    den = 0.0
    for g in groups:
        if len(g) <= lag + 1:
            continue
        x = g - g.mean()
        num += float(np.dot(x[:-lag], x[lag:]))
        den += float(np.dot(x, x))
    return num / den if den else float("nan")


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--parent", default="MNQ")
    p.add_argument("--label", default="real")
    args = p.parse_args()
    matrix = pl.read_parquet(DATA / f"{args.parent}-{args.label}-matrix.parquet")
    profile = pl.read_parquet(DATA / f"{args.parent}-real-profile.parquet")
    env = profile.select(
        "session_minute", "volume_norm_p50", "range_norm_p50"
    )
    joined = (
        matrix.join(env, on="session_minute", how="inner")
        .with_columns(
            (pl.col("volume_norm") / pl.col("volume_norm_p50")).alias("vres"),
            (pl.col("range_norm") / pl.col("range_norm_p50")).alias("rres"),
        )
        .filter(pl.col("volume") > 0)
        .sort("session_key", "session_minute")
    )
    sessions = joined["session_key"].n_unique()
    print(f"{args.parent} {args.label}: {sessions} sessions")

    for quantity in ("vres", "rres"):
        print(f"\n== {quantity} residual per phase: quantiles, log-sd, acf ==")
        print(
            "phase".ljust(9)
            + "".join(f"p{q:02d}".rjust(7) for q in (1, 10, 25, 50, 75, 90, 99))
            + "logsd".rjust(7)
            + "".join(f"ac{lag}".rjust(7) for lag in LAGS)
        )
        for phase in PHASES:
            sel = joined.filter(pl.col("phase") == phase)
            values = sel[quantity].to_numpy()
            values = values[np.isfinite(values) & (values > 0)]
            if len(values) < 100:
                continue
            qs = np.quantile(values, [0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99])
            logsd = float(np.std(np.log(values)))
            groups = [
                np.log(g[quantity].to_numpy())
                for _, g in sel.group_by("session_key")
            ]
            acs = [acf_pooled(groups, lag) for lag in LAGS]
            print(
                f"{phase:<9}"
                + "".join(f"{q:7.2f}" for q in qs)
                + f"{logsd:7.2f}"
                + "".join(f"{a:7.2f}" for a in acs)
            )

    print("\n== whole-session residual acf (log), pooled over sessions ==")
    groups = [
        np.log(g["vres"].to_numpy()) for _, g in joined.group_by("session_key")
    ]
    print(
        "volume: "
        + " ".join(f"lag{lag}={acf_pooled(groups, lag):.2f}" for lag in LAGS)
    )
    groups = [
        np.log(g["rres"].to_numpy()) for _, g in joined.group_by("session_key")
    ]
    print(
        "range:  "
        + " ".join(f"lag{lag}={acf_pooled(groups, lag):.2f}" for lag in LAGS)
    )

    print("\n== cross-correlation of volume and range residuals (log), same minute ==")
    v = np.log(joined["vres"].to_numpy())
    r = np.log(joined["rres"].to_numpy())
    ok = np.isfinite(v) & np.isfinite(r)
    print(f"corr = {np.corrcoef(v[ok], r[ok])[0, 1]:.2f}")

    print("\n== session level: mean minute volume and median minute range per session ==")
    levels = pl.read_parquet(DATA / f"{args.parent}-{args.label}-levels.parquet").sort(
        "session_date"
    )
    for column in ("volume_mean", "range_median"):
        series = levels[column].to_numpy()
        log = np.log(series)
        qs = np.quantile(series, [0.1, 0.5, 0.9])
        x = log - log.mean()
        acs = [
            float(np.dot(x[:-k], x[k:]) / np.dot(x, x)) for k in (1, 2, 5, 10, 20)
        ]
        print(
            f"{column}: p10 {qs[0]:.1f} p50 {qs[1]:.1f} p90 {qs[2]:.1f}, "
            f"log-sd {log.std():.2f}, day acf "
            + " ".join(f"{a:.2f}" for a in acs)
        )
    v = np.log(levels["volume_mean"].to_numpy())
    r = np.log(levels["range_median"].to_numpy())
    print(f"corr(log volume level, log range level) = {np.corrcoef(v, r)[0, 1]:.2f}")


if __name__ == "__main__":
    main()
