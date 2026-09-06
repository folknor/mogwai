#!/usr/bin/env python3
"""Prototype of the sub-minute layer under the activity cascade.

Two mechanisms, each to be transcribed into the Rust cascade if it lands:

- Placement. The cascade sets a rate per second; today the second's count
  is Poisson at that rate and the parents sit uniformly inside it. Here
  the rate is instead the mean intensity of a self-exciting process: the
  immigrant rate is `rate * (1 - n)` and every parent spawns Poisson(n)
  children at exponential offsets, a mixture of two timescales. The mean
  rate stays on the envelope; the sub-second clustering the real tape
  shows (within-second dispersion of two to four against one) comes from
  the branching. Simulated by generations, vectorised.
- Sign. The real aggressor sign has a slow, power-law memory (acf 0.13 at
  lag 1, still 0.01 at lag 50) that no Markov chain reproduces. The
  candidate is the order-splitting picture: `slots` metaorders are live
  at once, each with a side and a remaining print count drawn from a
  discrete Pareto tail; a parent takes a uniformly chosen slot's side and
  decrements it; an exhausted slot redraws. Consecutive parents share a
  slot with probability `1 / slots`, which is the lag-1 autocorrelation,
  and the decay follows the size tail.

The statistics are `tape_v2.micro_stats.compute_targets`, the same as the
real and generated measurement, on a synthetic prints frame framed as a
candidate. The texture between seconds is the cascade's own, at the
phase's amplitude, so the 1 s in 1 m dispersion includes what the fastest
texture component already provides.

    uv --directory analysis/tape-v2 run python proto_micro.py --n 0.5 \\
        --tau 0.1 1.5 --tau-w 0.5 --slots 8 --alpha 1.5
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np
import polars as pl

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "src"))

from tape_v2.micro_stats import compute_targets, flatten  # noqa: E402

NS = 1_000_000_000
# A Monday 17:00 CDT in summer, so the candidate frame's fixed offset and
# the phase table agree: 2026-08-17 22:00 UTC.
ANCHOR = 1_787_004_000
CDT = -5 * 3600
PHASE_WINDOWS = {
    # minute of session ranges, from tape_v2.session
    "asia": (60, 540),
    "london": (540, 840),
    "ny_open": (930, 1050),
    "ny_close": (1200, 1380),
}
# Parents per second at the phase mean and the texture amplitude there,
# read off the real year (rate) and the cascade's `s(m)` (amplitude).
PHASE_LEVELS = {
    "asia": (3.2, 0.62),
    "london": (3.6, 0.60),
    "ny_open": (26.0, 0.38),
    "ny_close": (9.0, 0.47),
}
TEXTURE_TAU_S = np.array([15.0, 60.0, 300.0, 1500.0, 5400.0, 10800.0])
TEXTURE_W = np.array([0.46, 0.12, 0.16, 0.11, 0.03, 0.12])


def ou_paths(rng: np.random.Generator, n: int, taus: np.ndarray) -> np.ndarray:
    out = np.empty((len(taus), n))
    for j, tau in enumerate(taus):
        rho = np.exp(-1.0 / tau)
        eps = rng.standard_normal(n) * np.sqrt(1.0 - rho * rho)
        y = rng.standard_normal()
        path = np.empty(n)
        for i in range(n):
            y = rho * y + eps[i]
            path[i] = y
        out[j] = path
    return out


def second_rates(
    rng: np.random.Generator,
    seconds: int,
    rate: float,
    s: float,
    fast_tau: float,
    fast_s: float,
) -> np.ndarray:
    """The cascade's rate per second at a flat envelope: rate * texture,
    times an independent fast texture at `fast_tau` seconds with log-sd
    `fast_s`, the seconds-scale swell the minute-fitted texture lacks."""
    paths = ou_paths(rng, seconds, TEXTURE_TAU_S)
    texture = (np.sqrt(TEXTURE_W)[:, None] * paths).sum(axis=0)
    fast = ou_paths(rng, seconds, np.array([fast_tau]))[0]
    return rate * np.exp(s * texture - 0.5 * s * s + fast_s * fast - 0.5 * fast_s * fast_s)


def branching_times(
    rng: np.random.Generator,
    rates: np.ndarray,
    n: float,
    taus: list[float],
    tau_w: list[float],
) -> np.ndarray:
    """Event times in seconds from the origin of `rates`."""
    seconds = rates.size
    immigrant_counts = rng.poisson(rates * (1.0 - n))
    starts = np.repeat(np.arange(seconds, dtype=float), immigrant_counts)
    times = starts + rng.random(starts.size)
    generation = times
    all_times = [times]
    tau_w = np.array(tau_w) / np.sum(tau_w)
    while generation.size:
        counts = rng.poisson(n, generation.size)
        parents = np.repeat(generation, counts)
        if parents.size == 0:
            break
        which = rng.choice(len(taus), size=parents.size, p=tau_w)
        offsets = rng.exponential(np.array(taus)[which])
        generation = parents + offsets
        generation = generation[generation < seconds]
        all_times.append(generation)
    return np.sort(np.concatenate(all_times))


def markov_signs(rng: np.random.Generator, count: int, p_same: float) -> np.ndarray:
    flips = rng.random(count) >= p_same
    sign = np.empty(count, dtype=np.int8)
    s = 1
    for i in range(count):
        if flips[i]:
            s = -s
        sign[i] = s
    return sign


def discrete_pareto(rng: np.random.Generator, size: int, alpha: float, cap: int) -> np.ndarray:
    """Remaining print counts with P(L >= l) = l^(-alpha), l >= 1."""
    u = rng.random(size)
    return np.minimum(np.floor(u ** (-1.0 / alpha)).astype(np.int64), cap)


def splitting_signs(
    rng: np.random.Generator,
    count: int,
    slots: int,
    alpha: float,
    mix: float = 0.0,
    cap: int = 100_000,
) -> np.ndarray:
    """Order-splitting signs, with a `mix` chance of repeating the previous
    parent's side outright (the same aggressor's next slice, arriving
    before anyone else's)."""
    side = rng.choice(np.array([-1, 1], dtype=np.int8), size=slots)
    left = discrete_pareto(rng, slots, alpha, cap)
    picks = rng.integers(0, slots, size=count)
    repeat = rng.random(count) < mix
    redraw_side = rng.choice(np.array([-1, 1], dtype=np.int8), size=count)
    redraw_left = discrete_pareto(rng, count, alpha, cap)
    sign = np.empty(count, dtype=np.int8)
    prev = np.int8(1)
    for i in range(count):
        if repeat[i]:
            sign[i] = prev
            continue
        k = picks[i]
        if left[k] == 0:
            side[k] = redraw_side[i]
            left[k] = redraw_left[i]
        sign[i] = side[k]
        left[k] -= 1
        prev = sign[i]
    return sign


def simulate_phase(
    rng: np.random.Generator,
    phase: str,
    sessions: int,
    n: float,
    taus: list[float],
    tau_w: list[float],
    sign_model: str,
    p_same: float,
    slots: int,
    alpha: float,
    mix: float,
    fast_tau: float,
    fast_s: float,
) -> pl.DataFrame:
    lo, hi = PHASE_WINDOWS[phase]
    rate, s = PHASE_LEVELS[phase]
    frames = []
    for day in range(sessions):
        # Sessions start at 17:00 local; the phase window is minutes from it.
        origin = ANCHOR + day * 86_400 + lo * 60
        seconds = (hi - lo) * 60
        rates = second_rates(rng, seconds, rate, s, fast_tau, fast_s)
        times = branching_times(rng, rates, n, taus, tau_w)
        count = times.size
        if sign_model == "markov":
            sign = markov_signs(rng, count, p_same)
        else:
            sign = splitting_signs(rng, count, slots, alpha, mix)
        ts = (origin * NS + (times * NS).astype(np.int64)).astype(np.int64)
        frames.append(
            pl.DataFrame(
                {
                    "ts_event": ts,
                    "sign": sign,
                    "price_ticks": np.zeros(count, dtype=np.int64),
                    "size": np.ones(count, dtype=np.int64),
                    "bid_ticks": pl.Series([None] * count, dtype=pl.Int64),
                    "ask_ticks": pl.Series([None] * count, dtype=pl.Int64),
                    "bid_sz": pl.Series([None] * count, dtype=pl.Int64),
                    "ask_sz": pl.Series([None] * count, dtype=pl.Int64),
                }
            )
        )
    return pl.concat(frames).sort("ts_event")


KEYS = [
    "parents/s",
    "gap <1ms",
    "gap <10ms",
    "gap <100ms",
    "u p50",
    "u p90",
    "u p99",
    "u cv",
    "log u acf1",
    "log u acf5",
    "disp 10ms/1s",
    "disp 100ms/1s",
    "disp 1s/1m",
    "disp 10s/1m",
    "p same side",
    "sign acf1",
    "sign acf2",
    "sign acf5",
    "sign acf10",
    "sign acf50",
    "sign acf100",
    "sign acf1000",
    "run mean",
    "run >=5",
    "run >=10",
]


PLACEMENT_KEYS = [
    "gap <1ms",
    "gap <10ms",
    "gap <100ms",
    "u p50",
    "u p90",
    "u p99",
    "u cv",
    "log u acf1",
    "disp 10ms/1s",
    "disp 100ms/1s",
    "disp 1s/1m",
    "disp 10s/1m",
]
SIGN_KEYS = [
    "p same side",
    "sign acf1",
    "sign acf2",
    "sign acf5",
    "sign acf10",
    "sign acf50",
    "sign acf100",
    "run mean",
    "run >=5",
    "run >=10",
]


def score(proto: dict[str, float], real: dict[str, float], keys: list[str]) -> float:
    """Mean absolute log ratio over the keys: 0.1 is ten percent off."""
    terms = []
    for key in keys:
        p, r = proto[key], real[key]
        if np.isnan(p) or np.isnan(r):
            continue
        # Autocorrelations near zero are compared on a floor so a sign
        # flip at the third decimal is not an infinite miss.
        if "acf" in key:
            p, r = max(abs(p), 0.005), max(abs(r), 0.005)
        terms.append(abs(np.log(p / r)))
    return float(np.mean(terms)) if terms else float("nan")


def real_medians(parent: str, phase: str) -> dict[str, float] | None:
    path = HERE / "data" / "micro" / f"{parent}-real-targets.json"
    if not path.exists():
        return None
    blocks = json.loads(path.read_text())["blocks"]
    rows = [flatten(b[phase]) for b in blocks.values() if phase in b]
    return {k: float(np.nanmedian([r[k] for r in rows])) for k in KEYS}


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--phases", nargs="*", default=["asia", "ny_open"])
    ap.add_argument("--sessions", type=int, default=3)
    ap.add_argument("--n", type=float, default=0.5, help="branching ratio")
    ap.add_argument("--tau", nargs="*", type=float, default=[0.1, 1.5])
    ap.add_argument("--tau-w", nargs="*", type=float, default=[0.5, 0.5])
    ap.add_argument("--sign", choices=["markov", "split"], default="split")
    ap.add_argument("--p-same", type=float, default=0.6)
    ap.add_argument("--slots", type=int, default=8)
    ap.add_argument("--alpha", type=float, default=1.5)
    ap.add_argument("--mix", type=float, default=0.0)
    ap.add_argument("--fast-tau", type=float, default=3.0)
    ap.add_argument("--fast-s", type=float, default=0.0)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--parent", default="MNQ")
    ap.add_argument(
        "--grid",
        type=Path,
        default=None,
        help="JSON list of placement configs to score against the real "
        "medians; each is a dict of n, tau, tau_w, fast_tau, fast_s",
    )
    args = ap.parse_args()
    rng = np.random.default_rng(args.seed)
    if args.grid is not None:
        configs = json.loads(args.grid.read_text())
        print(f"{'config':<44}" + "".join(f"{p[:8]:>9}" for p in args.phases) + f"{'mean':>9}")
        for cfg in configs:
            scores = []
            for phase in args.phases:
                prints = simulate_phase(
                    rng,
                    phase,
                    args.sessions,
                    cfg["n"],
                    cfg["tau"],
                    cfg["tau_w"],
                    "markov",
                    0.5,
                    1,
                    2.0,
                    0.0,
                    cfg.get("fast_tau", 3.0),
                    cfg.get("fast_s", 0.0),
                )
                proto = flatten(compute_targets(prints, candidate=True)[phase])
                real = real_medians(args.parent, phase)
                scores.append(score(proto, real, PLACEMENT_KEYS))
            label = (
                f"n={cfg['n']} tau={cfg['tau']} w={cfg['tau_w']} "
                f"fs={cfg.get('fast_s', 0.0)}@{cfg.get('fast_tau', 3.0)}"
            )
            print(
                f"{label:<44}"
                + "".join(f"{s:>9.3f}" for s in scores)
                + f"{np.mean(scores):>9.3f}"
            )
        return
    for phase in args.phases:
        prints = simulate_phase(
            rng,
            phase,
            args.sessions,
            args.n,
            args.tau,
            args.tau_w,
            args.sign,
            args.p_same,
            args.slots,
            args.alpha,
            args.mix,
            args.fast_tau,
            args.fast_s,
        )
        targets = compute_targets(prints, candidate=True)
        proto = flatten(targets[phase])
        real = real_medians(args.parent, phase)
        print(f"\n{phase}: {prints.height} parents over {args.sessions} sessions")
        print(f"{'':20}{'proto':>10}{'real':>10}")
        for key in KEYS:
            r = real[key] if real else float("nan")
            print(f"{key:<20}{proto[key]:>10.3f}{r:>10.3f}")
        if real:
            print(
                f"score placement {score(proto, real, PLACEMENT_KEYS):.3f}  "
                f"sign {score(proto, real, SIGN_KEYS):.3f}"
            )


if __name__ == "__main__":
    main()
