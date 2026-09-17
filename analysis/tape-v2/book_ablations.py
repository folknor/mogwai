#!/usr/bin/env python3
"""The refit's ablation matrix: what causes the spread instability and
what merely bounds it.

Four arms at the converged pooled configuration, differing only in the
mechanism under test:

  a. exponential replenishment law (the falsified one) on the fitted
     ladder;
  b. per-state table on the flat declared ladder the first
     transcription walked;
  c. per-state table and fitted ladder with the recession cap removed;
  d. the full mechanism.

Reported per arm: the pooled score, the spread tail diagnostics and the
executed size, so the landing can state what the table fixes, what the
ladder fixes, and what the cap bounds.

    ssh speilegg uv --directory Claude/tape-v2 run python book_ablations.py
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_book import (  # noqa: E402
    adopted_size_channel,
    book_config,
    draw_sizes,
    measure,
    real_medians,
    score,
    simulate,
)
from proto_micro import splitting_signs  # noqa: E402

# The fitted row from the single source of truth (the adopted
# depth_ratios ride in it), plus the harness-only extras.
CONVERGED = {
    **book_config("all"),
    "slack_1": None,
    "slack_3": None,
    "replenish_exp": 0.0,
}
CONVERGED["slack_1"] = CONVERGED["slack"]
CONVERGED["slack_3"] = CONVERGED["slack"]

ARMS = [
    ("a exp law, fitted ladder", {"replenish_exp": 2.0}),
    ("b table, flat ladder", {"depth_ratios": "1,1,1,1,1,1,1"}),
    ("c table, fitted, no cap", {"max_spread": 50}),
    ("d full mechanism", {}),
]

COLUMNS = ["spread_5plus", "spread_p99", "executed_mean", "size_p99", "coin_rate"]


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", default="all")
    ap.add_argument("--parents", type=int, default=400_000)
    ap.add_argument("--seed", type=int, default=1)
    args = ap.parse_args()
    real = real_medians(args.phase)
    # The adopted arrays from the single source of truth, never a fresh
    # derivation: the ablations price mechanisms around the shipped fit.
    CONVERGED["p_match"], f_law = adopted_size_channel(CONVERGED)
    rng = np.random.default_rng(args.seed)
    sign = splitting_signs(rng, args.parents, 5, 2.2, 0.08)
    sizes = draw_sizes(rng, args.parents, f_law)
    print(f"{'arm':<28}{'score':>7}" + "".join(f"{c:>15}" for c in COLUMNS))
    for label, overrides in ARMS:
        cfg = argparse.Namespace(**{**CONVERGED, **overrides})
        sim = simulate(np.random.default_rng(args.seed + 1), sign, sizes, cfg)
        m = measure(sim)
        cells = "".join(f"{m[c]:>15.3f}" for c in COLUMNS)
        print(f"{label:<28}{score(m, real):>7.3f}{cells}")


if __name__ == "__main__":
    main()
