#!/usr/bin/env python3
"""Pin the prototype's diffusion knobs to the engine's measured
per-parent innovation.

The book prototype must not free-fit `sigma_e`, `sig_ln`, `df` or the
impact terms: in the engine those are the cascade's own diffusion and
propagator, which the book cannot move, and a book fitted against a
diffusion the engine does not have is the transfer failure the first
transcription paid for. This script simulates the prototype's latent
path alone - diffusion plus impact on the splitting sign model, no book
- rounds it to the tick grid exactly as the placed book publishes it,
and scores its fast-bucket mid-change pmf against the engine's, measured
by `engine_diffusion_probe.py` on a placed-book quotes tape.

The output is the (sigma_e, sig_ln, df) triple the book refit holds
fixed.

    python3 diffusion_pin.py --engine-pmf 0.29,0.20,0.34,0.05,0.07,0.05
"""

from __future__ import annotations

import argparse
import itertools
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_micro import splitting_signs  # noqa: E402

EDGES = [
    ("0", 0.0, 0.25),
    ("0.5", 0.25, 0.75),
    ("1", 0.75, 1.25),
    ("1.5", 1.25, 1.75),
    ("2", 1.75, 2.25),
    ("2.5+", 2.25, None),
]


def latent_pmf(
    rng: np.random.Generator,
    sign: np.ndarray,
    sigma_e: float,
    sig_ln: float,
    df: float,
    rho_sig: float,
    g: float,
    a_t: float,
    rho: float,
) -> dict:
    n = sign.size
    normals = rng.standard_normal(n)
    level = np.empty(n)
    acc = 0.0
    scale = np.sqrt(1.0 - rho_sig**2)
    for k in range(n):
        acc = rho_sig * acc + scale * normals[k]
        level[k] = acc
    sigma = sigma_e * np.exp(sig_ln * level - 0.5 * sig_ln**2)
    t_draws = rng.standard_t(df, size=n) / np.sqrt(df / (df - 2.0))
    register = np.empty(n)
    acc = 0.0
    for k in range(n):
        register[k] = acc
        acc = rho * acc + sign[k]
    impact = g * sign + a_t * (sign - (1.0 - rho) * register)
    x = np.cumsum(sigma * t_draws + impact)
    mid = np.round(x)
    d = np.abs(np.diff(mid))
    return {
        label: float(
            ((d >= lo).mean() if hi is None else ((d >= lo) & (d < hi)).mean())
        )
        for label, lo, hi in EDGES
    }


def score(candidate: dict, engine: dict) -> float:
    misses = []
    for label, _lo, _hi in EDGES:
        c, e = candidate[label], engine[label]
        # The half-tick buckets are structurally empty on both sides
        # (integer mid lattice): matching zeros agree, a lone zero is a
        # full miss.
        if c <= 0.0 and e <= 0.0:
            misses.append(0.0)
        elif c <= 0.0 or e <= 0.0:
            misses.append(1.0)
        else:
            misses.append(abs(np.log(c / e)))
    return float(np.mean(misses))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--engine-pmf",
        required=True,
        help="engine fast-bucket dmid pmf, six comma-separated masses "
        "(0, 0.5, 1, 1.5, 2, 2.5+) from engine_diffusion_probe.py",
    )
    ap.add_argument("--parents", type=int, default=400_000)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--rho-sig", type=float, default=0.995)
    ap.add_argument("--g", type=float, default=0.31)
    ap.add_argument("--a-t", type=float, default=0.32)
    ap.add_argument("--rho", type=float, default=0.9)
    ap.add_argument("--slots", type=int, default=5)
    ap.add_argument("--alpha", type=float, default=2.2)
    ap.add_argument("--mix", type=float, default=0.08)
    args = ap.parse_args()
    masses = [float(v) for v in args.engine_pmf.split(",")]
    engine = {label: masses[i] for i, (label, _lo, _hi) in enumerate(EDGES)}
    rng = np.random.default_rng(args.seed)
    sign = splitting_signs(rng, args.parents, args.slots, args.alpha, args.mix)
    grid = {
        "sigma_e": (0.7, 0.8, 0.9, 1.0),
        "sig_ln": (0.4, 0.6, 0.8),
        "df": (6.0, 8.0, 12.0),
    }
    best: tuple = (1e9, None, None)
    for sigma_e, sig_ln, df in itertools.product(*grid.values()):
        pmf = latent_pmf(
            np.random.default_rng(args.seed + 1),
            sign,
            sigma_e,
            sig_ln,
            df,
            args.rho_sig,
            args.g,
            args.a_t,
            args.rho,
        )
        s = score(pmf, engine)
        print(
            f"sigma_e={sigma_e}  sig_ln={sig_ln}  df={df}  score {s:.3f}  "
            + "  ".join(f"{k}={v:.3f}" for k, v in pmf.items())
        )
        if s < best[0]:
            best = (s, (sigma_e, sig_ln, df), pmf)
    s, knobs, pmf = best
    print(f"best {s:.3f} at sigma_e={knobs[0]} sig_ln={knobs[1]} df={knobs[2]}")
    print("engine " + "  ".join(f"{k}={v:.3f}" for k, v in engine.items()))
    print("pinned " + "  ".join(f"{k}={v:.3f}" for k, v in pmf.items()))


if __name__ == "__main__":
    main()
