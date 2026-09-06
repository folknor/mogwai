#!/usr/bin/env python3
"""Fit the propagator impact on the splitting sign model.

The mid's impact component after parent k is

    M_k = g_inf * sum_{j<=k} s_j + a * R_k,   R_k = rho * R_{k-1} + s_k

so a parent moves the mid by `(g_inf + a) * s_k - a * (1 - rho) * R_{k-1}`
ticks: a kick on its own sign, of which the transient part `a` decays at
`rho` per parent and the permanent part `g_inf` stays. The response the
year measures, `R(l) = E[(mid_{k+l-1} - mid_{k-1}) * s_k]` over the mid
before each parent, is 0.48 ticks at l = 1, 0.65 at 10 and 0.66 at 100:
the growth from 1 to 10 is the flow memory (a buy is followed by more
buys) and the flatness from 10 to 100 is the impact decaying against it.
The diffusion is orthogonal to the sign and adds nothing to R, so it is
left out here; the minute variance it must supply is refitted on the
engine.

    uv --directory analysis/tape-v2 run python proto_impact.py --grid
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_micro import splitting_signs  # noqa: E402

REAL = {1: 0.483, 10: 0.653, 100: 0.664}
LAGS = (1, 2, 5, 10, 20, 50, 100, 200)


def impact_path(sign: np.ndarray, g_inf: float, a: float, rho: float) -> np.ndarray:
    """The impact component of the mid after each parent, in ticks."""
    permanent = g_inf * np.cumsum(sign)
    register = np.empty(sign.size)
    r = 0.0
    for i in range(sign.size):
        r = rho * r + sign[i]
        register[i] = r
    return permanent + a * register


def response(sign: np.ndarray, mid_after: np.ndarray) -> dict[int, float]:
    # mid before parent k is mid_after[k - 1].
    pre = np.concatenate([[0.0], mid_after[:-1]])
    out = {}
    for lag in LAGS:
        move = (pre[lag:] - pre[:-lag]) * sign[:-lag]
        out[lag] = float(move.mean())
    return out


def variance_factor(sign: np.ndarray, mid_after: np.ndarray, n: int) -> float:
    """Variance of the impact component over `n` parents, per parent,
    against a unit-variance iid kick: the flow-memory amplification."""
    steps = mid_after[n::n] - mid_after[:-n:n]
    kick = np.diff(mid_after)
    return float(steps.var() / (n * kick.var()))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--g-inf", type=float, default=0.25)
    ap.add_argument("--a", type=float, default=0.3)
    ap.add_argument("--rho", type=float, default=0.9)
    ap.add_argument("--slots", type=int, default=5)
    ap.add_argument("--alpha", type=float, default=2.2)
    ap.add_argument("--mix", type=float, default=0.08)
    ap.add_argument("--parents", type=int, default=2_000_000)
    ap.add_argument("--grid", action="store_true")
    args = ap.parse_args()
    rng = np.random.default_rng(1)
    sign = splitting_signs(rng, args.parents, args.slots, args.alpha, args.mix).astype(float)
    if args.grid:
        print(f"{'g_inf':>6}{'a':>6}{'rho':>6}" + "".join(f"R({l})".rjust(8) for l in LAGS) + "   F(200) F(2000)  score")
        for g_inf in (0.15, 0.2, 0.25, 0.3, 0.35):
            for a in (0.15, 0.25, 0.35, 0.45):
                for rho in (0.8, 0.9, 0.95, 0.98):
                    mid = impact_path(sign, g_inf, a, rho)
                    r = response(sign, mid)
                    score = np.mean([abs(np.log(r[l] / REAL[l])) for l in REAL])
                    print(
                        f"{g_inf:>6.2f}{a:>6.2f}{rho:>6.2f}"
                        + "".join(f"{r[l]:>8.3f}" for l in LAGS)
                        + f"   {variance_factor(sign, mid, 200):6.2f} {variance_factor(sign, mid, 2000):6.2f}  {score:.3f}"
                    )
        return
    mid = impact_path(sign, args.g_inf, args.a, args.rho)
    r = response(sign, mid)
    print("lag      response   real")
    for lag in LAGS:
        print(f"{lag:<8} {r[lag]:8.3f}   {REAL.get(lag, float('nan')):5.3f}")
    print(f"variance factor over 200 parents {variance_factor(sign, mid, 200):.2f}, over 2000 {variance_factor(sign, mid, 2000):.2f}")


if __name__ == "__main__":
    main()
