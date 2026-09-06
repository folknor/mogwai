#!/usr/bin/env python3
"""The price-side targets for the v2 engine, from a year of one-minute bars.

Everything the residual analysis cannot see because the session matrix
keeps only volume and range: how minute returns are distributed, how they
add up across horizons, and how big the biggest moves are and where they
start. Per phase over full sessions, roll sessions dropped:

- standardised one-minute close returns: kurtosis, autocorrelation at
  short lags, so the innovation law and the tick-scale reversion are known;
- variance ratios at 5, 15, 30, 60 minutes against one minute, which is
  what says whether the walk trends, ranges, or diffuses;
- the ratio of the minute close-return sd to the median minute range, the
  conversion the range envelope needs to become a sigma;
- per session, the largest absolute move over 2, 5, 10, 20 and 30 minute
  windows, as a distribution across sessions, and the phase it starts in.

    uv --directory analysis/tape-v2 run python price_targets.py --parent MNQ
"""

from __future__ import annotations

import argparse
from pathlib import Path

import numpy as np
import polars as pl

from tape_v2.session import PHASES, phase_of, session_columns

HERE = Path(__file__).resolve().parent
MIN_BARS = 1300
HORIZONS = [2, 5, 10, 20, 30, 60]
NAMES = [n for n, _, _ in PHASES]


def acf(x: np.ndarray, lag: int) -> float:
    x = x - x.mean()
    return float(np.dot(x[:-lag], x[lag:]) / np.dot(x, x))


def main() -> None:
    p = argparse.ArgumentParser()
    p.add_argument("--parent", default="MNQ")
    p.add_argument("--bars", default=None)
    p.add_argument(
        "--csv", nargs="*", type=Path, default=None,
        help="candidate gen bars CSVs instead of the real parquet, one per seed",
    )
    args = p.parse_args()
    if args.csv:
        frames = []
        for path in args.csv:
            frame = pl.read_csv(path).filter(pl.col("trade_count") > 0)
            frames.append(
                frame.select(
                    pl.col("open_ts").alias("ts_event"),
                    pl.lit(path.stem).alias("symbol"),
                    pl.col("open"), pl.col("high"), pl.col("low"), pl.col("close"),
                    pl.col("volume").cast(pl.Float64),
                )
            )
        raw = pl.concat(frames)
    else:
        path = Path(args.bars) if args.bars else HERE / "data" / "bars-1m" / f"{args.parent}.parquet"
        raw = pl.read_parquet(path)
    bars = session_columns(raw).filter(
        (pl.col("session_minute") < 1380) & (pl.col("weekday") <= 5)
    )
    # A candidate carries several seeds on the same dates, one symbol each,
    # so a session is keyed by symbol and date. On the real tape a session
    # with two symbols is a roll and is dropped.
    if args.csv:
        bars = bars.with_columns(
            (pl.col("symbol") + "|" + pl.col("session_date").cast(pl.Utf8)).alias("session_key")
        )
    else:
        bars = bars.with_columns(pl.col("session_date").cast(pl.Utf8).alias("session_key"))
    per_session = bars.group_by("session_key").agg(
        pl.len().alias("bars"), pl.col("symbol").n_unique().alias("symbols")
    )
    keep = per_session.filter(
        (pl.col("bars") >= MIN_BARS) & (pl.col("symbols") == 1)
    )["session_key"]
    bars = bars.filter(pl.col("session_key").is_in(keep.implode())).sort(
        "session_key", "session_minute"
    )
    sessions = [g for _, g in bars.group_by("session_key", maintain_order=True)]
    print(f"{args.parent}: {len(sessions)} full single-contract sessions")

    # Per session: minute close returns, the session's own range median for
    # standardisation, and phase labels.
    per_phase_ret: dict[str, list[np.ndarray]] = {n: [] for n in NAMES}
    per_phase_std: dict[str, list[np.ndarray]] = {n: [] for n in NAMES}
    per_phase_range: dict[str, list[np.ndarray]] = {n: [] for n in NAMES}
    for s in sessions:
        close = s["close"].to_numpy()
        minute = s["session_minute"].to_numpy()
        rng = (s["high"] - s["low"]).to_numpy()
        ret = np.diff(close)
        ret_minute = minute[1:]
        level = np.median(rng)
        for name, lo, hi in PHASES:
            mask = (ret_minute >= lo) & (ret_minute < hi)
            if mask.sum() > 10:
                per_phase_ret[name].append(ret[mask])
                per_phase_std[name].append(ret[mask] / level)
            rmask = (minute >= lo) & (minute < hi)
            per_phase_range[name].append(rng[rmask])

    print("\n== one-minute close returns per phase (points): sd, kurtosis, acf, sd/median-range ==")
    print("phase".ljust(9) + "sd".rjust(7) + "kurt".rjust(7) + "".join(f"ac{k}".rjust(7) for k in (1, 2, 3, 5, 10)) + "sd/rng".rjust(8) + "std-kurt".rjust(9))
    for name in NAMES:
        r = np.concatenate(per_phase_ret[name])
        z = np.concatenate(per_phase_std[name])
        rr = np.concatenate(per_phase_range[name])
        kurt = float(np.mean((r - r.mean()) ** 4) / np.var(r) ** 2)
        zk = float(np.mean((z - z.mean()) ** 4) / np.var(z) ** 2)
        acs = "".join(f"{acf(r, k):7.3f}" for k in (1, 2, 3, 5, 10))
        print(f"{name:<9}{r.std():7.2f}{kurt:7.1f}{acs}{r.std() / np.median(rr):8.3f}{zk:9.1f}")

    print("\n== variance ratio var(h-min return) / (h var(1-min)), per phase, pooled ==")
    print("phase".ljust(9) + "".join(f"h={h}".rjust(8) for h in (2, 5, 15, 30, 60)))
    for name, lo, hi in PHASES:
        cells = []
        one = []
        for s in sessions:
            close = s["close"].to_numpy()
            minute = s["session_minute"].to_numpy()
            mask = (minute >= lo) & (minute < hi)
            c = close[mask]
            if len(c) > 11:
                one.append(np.diff(c))
        v1 = np.var(np.concatenate(one))
        for h in (2, 5, 15, 30, 60):
            hs = []
            for s in sessions:
                close = s["close"].to_numpy()
                minute = s["session_minute"].to_numpy()
                mask = (minute >= lo) & (minute < hi)
                c = close[mask]
                if len(c) > h + 1:
                    hs.append(c[h:] - c[:-h])
            if not hs:
                cells.append("-".rjust(8))
                continue
            vh = np.var(np.concatenate(hs))
            cells.append(f"{vh / (h * v1):8.2f}")
        print(f"{name:<9}" + "".join(cells))

    print("\n== per-session largest |move| over h minutes: distribution across sessions, and start phase ==")
    print("h".ljust(4) + "p50".rjust(8) + "p90".rjust(8) + "p99".rjust(8) + "max".rjust(8) + "  share starting in ny_open / ny_pre / other")
    for h in HORIZONS:
        maxima = []
        starts = []
        for s in sessions:
            close = s["close"].to_numpy()
            minute = s["session_minute"].to_numpy()
            if len(close) <= h:
                continue
            move = close[h:] - close[:-h]
            i = int(np.argmax(np.abs(move)))
            maxima.append(abs(move[i]))
            starts.append(phase_of(int(minute[i])))
        m = np.array(maxima)
        st = np.array(starts)
        qs = np.quantile(m, [0.5, 0.9, 0.99])
        print(
            f"{h:<4}{qs[0]:8.1f}{qs[1]:8.1f}{qs[2]:8.1f}{m.max():8.1f}  "
            f"{np.mean(st == 'ny_open'):.2f} / {np.mean(st == 'ny_pre'):.2f} / "
            f"{np.mean(~np.isin(st, ['ny_open', 'ny_pre'])):.2f}"
        )

    print("\n== per-session largest one-minute H-L: distribution across sessions ==")
    m = np.array([(s["high"] - s["low"]).max() for s in sessions])
    qs = np.quantile(m, [0.5, 0.9, 0.99])
    print(f"p50 {qs[0]:.1f} p90 {qs[1]:.1f} p99 {qs[2]:.1f} max {m.max():.1f}")

    print("\n== session range (high - low over the session) across sessions, and week range across weeks ==")
    sr = np.array([float(s["high"].max() - s["low"].min()) for s in sessions])
    qs = np.quantile(sr, [0.1, 0.5, 0.9])
    print(f"session: p10 {qs[0]:.0f} p50 {qs[1]:.0f} p90 {qs[2]:.0f}")
    # Range efficiency: the session range over the session's realised
    # volatility (root sum of squared minute returns). A Brownian path
    # gives about 1.5 at the median; lower means the path folds back on
    # itself at some horizon, higher means it trends.
    eff = []
    for s in sessions:
        close = s["close"].to_numpy()
        rv = float(np.sqrt(np.sum(np.diff(close) ** 2)))
        if rv > 0:
            eff.append(float(s["high"].max() - s["low"].min()) / rv)
    e = np.array(eff)
    qs = np.quantile(e, [0.1, 0.5, 0.9])
    print(f"range efficiency (range / realised vol): p10 {qs[0]:.2f} p50 {qs[1]:.2f} p90 {qs[2]:.2f}")
    rv = np.array([float(np.sqrt(np.sum(np.diff(s["close"].to_numpy()) ** 2))) for s in sessions])
    qs = np.quantile(rv, [0.1, 0.5, 0.9])
    print(f"realised vol per session: p10 {qs[0]:.0f} p50 {qs[1]:.0f} p90 {qs[2]:.0f}")
    weeks = bars.with_columns(
        (pl.col("symbol") + "|" + pl.col("session_date").dt.strftime("%G-%V")).alias("week")
    ).group_by("week").agg(
        (pl.col("high").max() - pl.col("low").min()).alias("range"),
        pl.col("session_date").n_unique().alias("days"),
    ).filter(pl.col("days") == 5)
    wr = weeks["range"].to_numpy()
    qs = np.quantile(wr, [0.1, 0.5, 0.9])
    print(f"week ({len(wr)} full weeks): p10 {qs[0]:.0f} p50 {qs[1]:.0f} p90 {qs[2]:.0f}")

    print("\n== reopen gap: |first close of session - last close of previous|, points, and as a share of the previous session's range ==")
    gaps = []
    shares = []
    for a, b in zip(sessions, sessions[1:]):
        if a["symbol"][0] != b["symbol"][0]:
            continue
        gap = float(b["open"][0] - a["close"][-1])
        gaps.append(abs(gap))
        shares.append(abs(gap) / float(a["high"].max() - a["low"].min()))
    g = np.array(gaps)
    qs = np.quantile(g, [0.5, 0.9, 0.99])
    print(f"points: p50 {qs[0]:.1f} p90 {qs[1]:.1f} p99 {qs[2]:.1f} max {g.max():.1f}")
    qs = np.quantile(np.array(shares), [0.5, 0.9, 0.99])
    print(f"share of previous session range: p50 {qs[0]:.2f} p90 {qs[1]:.2f} p99 {qs[2]:.2f}")


if __name__ == "__main__":
    main()
