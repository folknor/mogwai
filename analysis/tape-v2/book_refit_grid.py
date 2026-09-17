#!/usr/bin/env python3
"""The bounded refit experiment for the phase rows, under the revised
grading (tenth spar): the three exception-family cells graded against
their phase bands, ledger cells classified as deferrals, one shared
`grade` function.

Per phase, a grid over the tracking knobs around the approved row.
Feasibility comes before ranking: a candidate must hold every impact
cell (lags 1, 10, 100 and the two by-spread cells) inside its phase
band and honor the tail and unexecuted contracts. Feasible candidates
rank lexicographically by unexpected-miss count, then score. Deferred
cells stay classified but their movement from the ledger's recorded
value is reported, because an optimizer that pays for a new cell by
worsening a deferred witness keeps its miss count while abandoning the
adjudicated state.

The search runs at a reduced sample size; the selected candidate is
re-graded at the pinned recipe (400k) before anything is concluded.
What a grid can conclude is bounded: a clean candidate proceeds to the
neighborhood battery; no candidate in the searched space means exactly
that - the response direction (flat, interacting, or impact collapse)
is what the report is for.

    ssh speilegg uv --directory Claude/tape-v2 run python book_refit_grid.py --phase ny_close
"""

from __future__ import annotations

import argparse
import itertools
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from book_robustness import phase_row, run_one  # noqa: E402
from proto_book import (  # noqa: E402
    BOOK_CONFIG,
    cell_name,
    grade,
    load_gate,
    real_band,
    real_medians,
    score,
)

IMPACT_CELLS = [
    ("impact", "1"),
    ("impact", "10"),
    ("impact", "100"),
    ("impact_spread_1", None),
    ("impact_spread_2", None),
]

# Offsets around the approved row. p_target_1 explores downward only:
# the close fitted it with dprice-1 unguarded, and the failing cells
# all want less one-tick bounce, never more.
OFFSETS = {
    "p_target_1": (-0.15, -0.05, 0.0),
    "p_narrow": (-0.06, 0.0, 0.06),
    "p_widen": (-0.06, 0.0, 0.06),
    "replenish_2": (-1.5, 0.0, 1.5),
    "replenish_3": (-1.0, 0.0, 1.0),
    "slack": (-0.5, 0.0, 0.5),
}


def clamp(cfg: dict) -> dict:
    cfg["p_target_1"] = float(np.clip(cfg["p_target_1"], 0.05, 0.9))
    cfg["p_target_2"] = float(np.clip(cfg["p_target_2"], 0.05, 0.95 - cfg["p_target_1"]))
    for knob in ("p_narrow", "p_widen"):
        cfg[knob] = float(np.clip(cfg[knob], 0.02, 0.98))
    cfg["replenish_2"] = max(1.0, cfg["replenish_2"])
    cfg["replenish_3"] = max(1.0, cfg["replenish_3"])
    cfg["slack"] = max(0.5, cfg["slack"])
    cfg["slack_1"] = cfg["slack"]
    cfg["slack_3"] = cfg["slack"]
    return cfg


def impact_feasible(m: dict, band: dict) -> bool:
    for key, sub in IMPACT_CELLS:
        c = m[key] if sub is None else m[key][sub]
        lo, hi = band[(key, sub)]
        if not (np.isfinite(c) and lo <= c <= hi):
            return False
    return True


def deferred_movement(graded: dict, ledger_by_cell: dict) -> float:
    """The worst relative movement of any deferred cell away from its
    ledger-recorded value, signed positive when the miss deepened."""
    worst = 0.0
    for key, sub, c, lo, hi in graded["deferred"]:
        recorded = ledger_by_cell[(key, sub)]["value"]
        edge = lo if recorded < lo else hi
        worsening = (abs(c - edge) - abs(recorded - edge)) / max(abs(edge), 1e-9)
        worst = max(worst, worsening)
    return worst


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--phase", required=True)
    ap.add_argument("--parents", type=int, default=120_000)
    ap.add_argument("--top", type=int, default=10)
    args = ap.parse_args()
    gate = load_gate()
    recipe = json.loads(BOOK_CONFIG.read_text())["gate_recipe"]
    real = real_medians(args.phase)
    band = real_band(args.phase)
    base = phase_row(args.phase)
    ledger_by_cell = {
        (e["key"], e.get("sub")): e for e in gate["ledger"].get(args.phase, [])
    }

    names = list(OFFSETS)
    rows = []
    infeasible = 0
    for deltas in itertools.product(*OFFSETS.values()):
        cfg = clamp({**base, **{n: base[n] + d for n, d in zip(names, deltas)}})
        m = run_one(cfg, args.parents, recipe["seed"], recipe["sim_seed"])
        graded = grade(m, args.phase, band, gate)
        feasible = impact_feasible(m, band) and not graded["contracts"]
        if not feasible:
            infeasible += 1
            continue
        rows.append(
            (
                len(graded["unexpected"]),
                score(m, real),
                deltas,
                [cell_name(k, s) for k, s, *_ in graded["unexpected"]],
                deferred_movement(graded, ledger_by_cell),
            )
        )
    rows.sort(key=lambda r: (r[0], r[1]))
    print(
        f"{args.phase}: {len(rows)} feasible of {len(rows) + infeasible} "
        f"candidates at {args.parents} parents"
    )
    for miss, s, deltas, cells, movement in rows[: args.top]:
        knobs = "  ".join(f"{n}{d:+.2f}" for n, d in zip(names, deltas) if d)
        print(
            f"  miss {miss}  score {s:.3f}  deferred moved {movement:+.3f}  "
            f"[{', '.join(cells) or 'clean'}]  {knobs or 'center'}"
        )
    if not rows:
        print("  no feasible candidate in the searched space")
        return

    print("re-grading the best candidate at the pinned recipe:")
    _miss, _s, deltas, _cells, _move = rows[0]
    cfg = clamp({**base, **{n: base[n] + d for n, d in zip(names, deltas)}})
    m = run_one(cfg, recipe["parents"], recipe["seed"], recipe["sim_seed"])
    graded = grade(m, args.phase, band, gate)
    print(
        f"  score {score(m, real):.3f}  unexpected {len(graded['unexpected'])}  "
        f"deferred {len(graded['deferred'])}  contracts {len(graded['contracts'])}"
    )
    for key, sub, c, lo, hi in graded["unexpected"]:
        print(f"  UNEXPECTED {cell_name(key, sub)}: {c:.4f}  band {lo:.4f}..{hi:.4f}")
    for key, sub, c, lo, hi in graded["deferred"]:
        recorded = ledger_by_cell[(key, sub)]["value"]
        print(
            f"  deferred {cell_name(key, sub)}: {c:.4f} (ledger {recorded:.4f}, "
            f"band {lo:.4f}..{hi:.4f})"
        )
    impacts = "  ".join(
        f"i{lag} {m['impact'][str(lag)]:.2f}/{real['impact'][str(lag)]:.2f}"
        for lag in (1, 10, 100)
    )
    print(f"  {impacts}")


if __name__ == "__main__":
    main()
