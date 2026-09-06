#!/usr/bin/env python3
"""Prototype of the v2 activity-cascade engine, at one-second resolution.

The mechanism, which the Rust engine transcribes:

- The calendar owns whether the market is open. The envelope owns the
  deterministic activity shape per minute of session, `v(m)` for arrivals
  and `r(m)` for range, each normalised to the session (mean 1 and median 1).
- Everything stochastic about activity is one multiplicative log-Gaussian
  cascade: a sum of independent Ornstein-Uhlenbeck components at timescales
  from seconds to a month, unit variance each, combined with declared
  weights. The minute-scale components carry a texture amplitude that
  shrinks with activity, `s(t) = s0 * v^(-gamma)`, which is what the real
  residuals show: log-sd 0.70 overnight and 0.36 at the cash open. The
  day-scale components carry the session-level amplitude.
- The parent arrival rate per second is `base * v * weekday * exp(texture
  + level - centring)`; the count in a second is Poisson at that rate. The
  centring keeps the mean rate on the envelope.
- Each parent moves the mid by `sigma_e * t_nu`, a Student-t innovation,
  with `sigma_e = c * r / sqrt(v) * level_sigma`: the minute variance then
  follows the count (the time change), and the range envelope's departure
  from the square root of the volume envelope is carried per parent. No
  drift, no bounce regime: the mid is a martingale on the tick grid.
- Every scheduled reopen applies a gap drawn heavy-tailed around the
  session's range level.

Writes one-minute bars in the gen CSV shape for the battery scripts:

    uv --directory analysis/tape-v2 run python proto_engine.py \\
        --seeds 1 2 3 4 --weeks 4 --out-prefix data/gen/MNQ-proto
"""

from __future__ import annotations

import argparse
import csv
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
PRESET = HERE.parent.parent / "crates" / "mogwai-venue" / "presets" / "mnq.toml"
NS = 1_000_000_000
ANCHOR = 1_786_917_600  # Sunday 2026-08-16 22:00 UTC, the chart week.
CDT = -5 * 3600
SESSION_MINUTES = 1380
OPEN_MINUTE_OF_DAY = 17 * 60


@dataclass
class Params:
    # Minute-scale cascade: timescales in minutes and variance weights.
    # Weights solved from the real Asia residual autocorrelation (0.52,
    # 0.39, 0.27, 0.20, 0.17, 0.09, 0.03 at lags 1, 2, 5, 10, 15, 30, 60)
    # at the minute level, where the 15-second component reads as white.
    # Second-level weights: the 15-second component keeps about 47 percent
    # of its variance in a minute average, the one-minute component 74
    # percent, so the minute-level fractions (0.36 white, 0.15, 0.25, 0.22,
    # 0.02) are what these become after averaging.
    fast_tau: tuple = (0.25, 1.0, 5.0, 25.0, 90.0)
    fast_w: tuple = (0.52, 0.135, 0.18, 0.15, 0.0135)
    texture_s0: float = 0.54
    texture_gamma: float = 0.24
    # Day-scale cascade: timescales in minutes, weights, amplitude in log.
    slow_tau: tuple = (420.0, 8640.0, 47520.0)
    slow_w: tuple = (0.2, 0.3, 0.5)
    level_sd: float = 0.34
    # Per-event sigma level: exponent on the volume level plus its own
    # slow process.
    sigma_level_exp: float = 0.57
    sigma_level_extra_sd: float = 0.207
    # Parents per session-mean minute (the level the envelope multiplies).
    parents_per_minute: float = 747.0
    # Per-event sigma scale in points at the reference minute, before the
    # envelope and level.
    sigma_e: float = 0.21
    student_df: float = 4.0
    # Reopen gap: lognormal, median in units of the reference minute sd
    # (sigma_e * sqrt(parents_per_minute)) times the day's sigma level. The
    # real year: median 9.8 points against a 10.7-point reference sd, p90 at
    # 9 times the median, p99 at 41 times, which a log-sd of 1.6 gives.
    gap_median_ratio: float = 0.9
    gap_log_sd: float = 1.6
    # Jumps: the news component. A jump arrives at a rate proportional to
    # the parent rate, `jumps_per_session` per session at the reference
    # level, and moves the mid by `jump_size` reference minute sds times a
    # lognormal spread, with a random sign. This is what the summed
    # innovations cannot make: the largest minute of a session.
    jumps_per_session: float = 3.0
    jump_size: float = 4.0
    jump_log_sd: float = 0.5
    tick: float = 0.25
    start_price: float = 28284.0


def load_envelope() -> tuple[np.ndarray, np.ndarray, np.ndarray]:
    with open(PRESET, "rb") as f:
        preset = tomllib.load(f)
    env = preset["instrument"]["calendar"]["envelope"]
    return (
        np.array(env["volume"], dtype=float),
        np.array(env["range"], dtype=float),
        np.array(env["weekday_weight"], dtype=float),
    )


def ou_path(rng: np.random.Generator, n: int, tau_seconds: float) -> np.ndarray:
    """Unit-variance OU sampled every second, started stationary."""
    rho = np.exp(-1.0 / tau_seconds)
    eps = rng.standard_normal(n) * np.sqrt(1.0 - rho * rho)
    out = np.empty(n)
    y = rng.standard_normal()
    for i in range(n):
        y = rho * y + eps[i]
        out[i] = y
    return out


def ou_paths_fast(rng: np.random.Generator, n: int, tau_seconds: float) -> np.ndarray:
    """Same as ou_path, vectorised through an IIR filter."""
    from scipy.signal import lfilter

    rho = np.exp(-1.0 / tau_seconds)
    eps = rng.standard_normal(n) * np.sqrt(1.0 - rho * rho)
    y0 = rng.standard_normal()
    out, _ = lfilter([1.0], [1.0, -rho], eps, zi=[rho * y0])
    return out


def session_minute(second_utc: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Minute of session and session weekday for each UTC second."""
    local = second_utc + CDT
    minute_of_day = (local // 60) % 1440
    day = (local // 86400 + 4) % 7  # 1970-01-01 was a Thursday: day 4.
    after_open = minute_of_day >= OPEN_MINUTE_OF_DAY
    minute = np.where(
        after_open,
        minute_of_day - OPEN_MINUTE_OF_DAY,
        minute_of_day + 1440 - OPEN_MINUTE_OF_DAY,
    )
    session_day = np.where(after_open, (day + 1) % 7, day)
    return minute.astype(int), session_day.astype(int)


def simulate(seed: int, weeks: int, p: Params, start: int = ANCHOR):
    rng = np.random.default_rng(seed)
    volume_env, range_env, weekday = load_envelope()
    # The envelope is a cross-session median profile. The rate the cascade
    # centres on is a mean, and the median of a lognormal minute sits
    # exp(-s^2/2) below its mean, more so overnight where the texture is
    # wider. Lift each minute by its own texture variance, then normalise
    # the session mean to one, so the mean rate the engine produces is the
    # envelope's mean profile and the level constant means what it says.
    minute_index = np.arange(SESSION_MINUTES)
    s_minute = p.texture_s0 * np.power(volume_env, -p.texture_gamma)
    volume_env = volume_env * np.exp(0.5 * s_minute * s_minute)
    volume_env = volume_env / volume_env.mean()
    del minute_index
    n = weeks * 7 * 86400
    seconds = start + np.arange(n)
    minute, sday = session_minute(seconds)
    open_mask = (minute < SESSION_MINUTES) & (sday >= 1) & (sday <= 5)
    v = np.where(open_mask, volume_env[np.minimum(minute, SESSION_MINUTES - 1)], 1.0)
    r = np.where(open_mask, range_env[np.minimum(minute, SESSION_MINUTES - 1)], 1.0)
    w = np.where(open_mask, weekday[sday], 1.0)

    # Cascades. Fast ones run in calendar seconds; they keep running through
    # closures, which is harmless because nothing is emitted there.
    texture = np.zeros(n)
    for tau, wt in zip(p.fast_tau, p.fast_w):
        texture += np.sqrt(wt) * ou_paths_fast(rng, n, tau * 60.0)
    level = np.zeros(n)
    for tau, wt in zip(p.slow_tau, p.slow_w):
        level += np.sqrt(wt) * ou_paths_fast(rng, n, tau * 60.0)
    sigma_extra = np.zeros(n)
    for tau, wt in zip(p.slow_tau, p.slow_w):
        sigma_extra += np.sqrt(wt) * ou_paths_fast(rng, n, tau * 60.0)

    s = p.texture_s0 * np.power(v, -p.texture_gamma)
    log_rate = (
        np.log(p.parents_per_minute / 60.0 * v * w)
        + s * texture
        - 0.5 * s * s
        + p.level_sd * level
    )
    rate = np.where(open_mask, np.exp(log_rate), 0.0)
    counts = rng.poisson(rate)

    level_sigma = np.exp(
        p.sigma_level_exp * p.level_sd * level + p.sigma_level_extra_sd * sigma_extra
    )
    sigma = p.sigma_e * r / np.sqrt(v) * level_sigma

    # Per-second return: sum of `count` Student-t innovations, standardised
    # to unit variance, times sigma. Drawn in one batch and reduced.
    total = int(counts.sum())
    innov = rng.standard_t(p.student_df, total) / np.sqrt(
        p.student_df / (p.student_df - 2.0)
    )
    edges = np.concatenate([[0], np.cumsum(counts)])
    sums = np.add.reduceat(
        np.concatenate([innov, [0.0]]), np.minimum(edges[:-1], total)
    )
    sums = np.where(counts > 0, sums, 0.0)
    ret = sigma * sums

    # Jumps, at a rate that follows the arrival rate so they cluster where
    # the market is busy.
    jump_rate = rate / (p.parents_per_minute * SESSION_MINUTES) * p.jumps_per_session
    jump_count = rng.poisson(jump_rate)
    jidx = np.flatnonzero(jump_count)
    if len(jidx):
        ref_sd = p.sigma_e * np.sqrt(p.parents_per_minute) * level_sigma[jidx]
        jumps = (
            p.jump_size
            * ref_sd
            * np.exp(p.jump_log_sd * rng.standard_normal(len(jidx)))
            * rng.choice([-1.0, 1.0], len(jidx))
            * jump_count[jidx]
        )
        ret[jidx] += jumps

    # Reopen gaps at the first open second after each closure.
    reopen = open_mask & ~np.concatenate([[False], open_mask[:-1]])
    idx = np.flatnonzero(reopen)
    # The session's range level is the sigma level times the reference
    # median minute range; the envelope's range median is 1 at the
    # reference, and sigma_e * sqrt(parents) approximates it.
    reference_sd = p.sigma_e * np.sqrt(p.parents_per_minute) * level_sigma[idx]
    gaps = (
        p.gap_median_ratio
        * reference_sd
        * np.exp(p.gap_log_sd * rng.standard_normal(len(idx)))
        * rng.choice([-1.0, 1.0], len(idx))
    )
    ret[idx] += gaps

    path = p.start_price + np.cumsum(ret)
    path = np.round(path / p.tick) * p.tick
    return seconds, open_mask, counts, path


def write_bars(path_out: Path, seconds, open_mask, counts, path, mean_size: float):
    """One-minute bars over open minutes, gen CSV shape."""
    n = len(seconds)
    minutes = n // 60
    sec = seconds[: minutes * 60].reshape(minutes, 60)
    om = open_mask[: minutes * 60].reshape(minutes, 60)
    cnt = counts[: minutes * 60].reshape(minutes, 60)
    px = path[: minutes * 60].reshape(minutes, 60)
    with open(path_out, "w", newline="") as f:
        wr = csv.writer(f)
        wr.writerow(["open_ts", "close_ts", "open", "high", "low", "close", "volume", "trade_count"])
        for i in range(minutes):
            if not om[i].any():
                continue
            trades = int(cnt[i].sum())
            # The open is the first print of the minute, as a real bar's is,
            # so a reopen gap shows between bars rather than inside one.
            closes = px[i]
            o = float(closes[0])
            h = float(closes.max())
            lo = float(closes.min())
            c = float(closes[-1])
            wr.writerow(
                [
                    int(sec[i][0]) * NS,
                    (int(sec[i][0]) + 60) * NS,
                    f"{o:.2f}",
                    f"{h:.2f}",
                    f"{lo:.2f}",
                    f"{c:.2f}",
                    int(round(trades * mean_size)),
                    trades,
                ]
            )


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--seeds", type=int, nargs="+", default=[1])
    ap.add_argument("--weeks", type=int, default=1)
    ap.add_argument("--out-prefix", default="data/gen/MNQ-proto")
    ap.add_argument("--mean-size", type=float, default=1.98)
    ap.add_argument("--set", nargs="*", default=[], help="name=value overrides")
    args = ap.parse_args()
    p = Params()
    for item in args.set:
        name, _, value = item.partition("=")
        current = getattr(p, name)
        if isinstance(current, tuple):
            setattr(p, name, tuple(float(x) for x in value.split(",")))
        else:
            setattr(p, name, type(current)(value))
    for seed in args.seeds:
        seconds, open_mask, counts, path = simulate(seed, args.weeks, p)
        out = HERE / f"{args.out_prefix}-s{seed}-{args.weeks}w-1m.csv"
        out.parent.mkdir(parents=True, exist_ok=True)
        write_bars(out, seconds, open_mask, counts, path, args.mean_size)
        print(f"wrote {out}: {int(counts.sum())} parents")


if __name__ == "__main__":
    main()
