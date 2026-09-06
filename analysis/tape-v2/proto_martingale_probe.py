#!/usr/bin/env python3
"""Where does the prototype's long-horizon mean reversion come from?

Runs one simulated week and reports the variance ratio of the raw
one-second return series at several horizons inside open time, then the
same with the tick rounding, the gaps and the jumps switched off in turn.

    uv --directory analysis/tape-v2 run python proto_martingale_probe.py
"""

from __future__ import annotations

import numpy as np

import proto_engine as pe


def vr(ret: np.ndarray, open_mask: np.ndarray, horizons: list[int]) -> str:
    # Minute closes inside open time, one session at a time.
    n = len(ret) // 60 * 60
    r = ret[:n].reshape(-1, 60).sum(axis=1)
    om = open_mask[:n].reshape(-1, 60).all(axis=1)
    path = np.cumsum(r)
    # Split into open runs.
    runs = []
    start = None
    for i, o in enumerate(om):
        if o and start is None:
            start = i
        if not o and start is not None:
            runs.append(path[start:i])
            start = None
    if start is not None:
        runs.append(path[start:])
    v1 = np.var(np.concatenate([np.diff(c) for c in runs]))
    out = []
    for h in horizons:
        moves = np.concatenate([c[h:] - c[:-h] for c in runs if len(c) > h])
        out.append(f"h={h}: {np.var(moves) / (h * v1):.3f}")
    return "  ".join(out)


def main() -> None:
    p = pe.Params()
    horizons = [5, 60, 300, 1000]
    for label, changes in [
        ("as configured", {}),
        ("no jumps", {"jumps_per_session": 0.0}),
        ("no gaps", {"gap_median_ratio": 0.0}),
        ("no jumps, no gaps", {"jumps_per_session": 0.0, "gap_median_ratio": 0.0}),
        ("student df 30", {"student_df": 30.0}),
        ("no texture", {"texture_s0": 0.0}),
        ("no level", {"level_sd": 0.0, "sigma_level_extra_sd": 0.0}),
    ]:
        q = pe.Params(**{**p.__dict__, **changes})
        seconds, open_mask, counts, path = pe.simulate(1, 1, q)
        ret = np.diff(np.concatenate([[path[0]], path]))
        print(f"{label:<22} rounded path: {vr(ret, open_mask, horizons)}")


if __name__ == "__main__":
    main()
