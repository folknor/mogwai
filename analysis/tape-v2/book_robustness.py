#!/usr/bin/env python3
"""The robustness battery: the executable verdict over the fitted rows.

Two halves per phase row, both graded by the one shared `grade`
function (the CLI report and the refit grid call the same one, so no
runner can answer a different question):

- The center verdict, at the pinned sampling recipe from
  book-config.json (`gate_recipe`): every non-tier-2 cell inside its
  phase band or classified through the deferral ledger; on the pooled
  row the three frozen exceptions bind one-sided at frozen value plus
  the declared allowance; the tail and unexecuted contracts hold. This
  is the deterministic reproduction of the adjudicated grading.
  Further seeds are measured and reported as diagnostics - a band-edge
  cell flipping under another realization is a property of the edge,
  not the row - and never decide the verdict.

- The neighborhood: a joint jitter of the tracking knobs (both target
  shares on the simplex, slack, narrowing, widening, the impact terms)
  across seeds. The verdict requires every sample to keep all three
  impact lags above the declared collapse floor (book-config.json's
  neighborhood block, half of real) and to honor the tail and
  unexecuted contracts; the share of samples under the report
  threshold (three quarters of real) is a per-lag diagnostic, and the
  score spread is a reported diagnostic with no declared threshold.
  The floor replaced the original 0.75-everywhere criterion by owner
  ruling 2026-09-17: that criterion was never met by the recorded
  ninth-spar acceptance, and an absolute floor cannot distinguish
  smooth degradation from a nearby cliff, so the close's measured
  cliff stays recorded as unresolved in the spec.

Deferred cells' center values are printed beside their ledger records,
so movement from the adjudicated state is visible evidence rather than
silently retained candidacy.

    ssh speilegg uv --directory Claude/tape-v2 run python book_robustness.py
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))

from proto_book import (  # noqa: E402
    BOOK_CONFIG,
    adopted_size_channel,
    book_config,
    cell_name,
    draw_sizes,
    grade,
    load_gate,
    measure,
    real_band,
    real_medians,
    score,
    simulate,
)
from proto_micro import splitting_signs  # noqa: E402

ROW_PHASES = ["all", "asia", "london", "ny_open", "ny_close"]

# `depth_ratios` deliberately absent: the base row from book-config.json
# carries the adopted vector, and an override here would grade a ladder
# the preset does not ship.
EXTRAS = {
    "slack_1": None,
    "slack_3": None,
    "replenish_exp": 0.0,
}

JITTER = {
    "p_target_1": 0.05,
    "p_target_2": 0.05,
    "slack": 0.5,
    "p_narrow": 0.04,
    "p_widen": 0.05,
    "g": 0.03,
    "a_t": 0.04,
}


def phase_row(phase: str) -> dict:
    base = book_config(phase)
    base.update(EXTRAS)
    if base["slack_1"] is None:
        base["slack_1"] = base["slack"]
    if base["slack_3"] is None:
        base["slack_3"] = base["slack"]
    base["p_match"], base["_f_law"] = adopted_size_channel(base)
    return base


def run_one(cfg: dict, parents: int, seed: int, sim_seed: int) -> dict:
    rng = np.random.default_rng(seed)
    recipe = json.loads(BOOK_CONFIG.read_text())["gate_recipe"]
    sign = splitting_signs(
        rng, parents, recipe["sign_slots"], recipe["sign_alpha"], recipe["sign_mix"]
    )
    sizes = draw_sizes(rng, parents, cfg["_f_law"])
    ns = argparse.Namespace(**{k: v for k, v in cfg.items() if k != "_f_law"})
    return measure(simulate(np.random.default_rng(sim_seed), sign, sizes, ns))


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--parents", type=int, default=120_000)
    ap.add_argument("--samples", type=int, default=24)
    ap.add_argument("--seeds", type=int, default=2)
    ap.add_argument(
        "--diagnostic-seeds",
        type=int,
        default=2,
        help="extra center realizations reported beside the pinned one",
    )
    args = ap.parse_args()
    gate = load_gate()
    recipe = json.loads(BOOK_CONFIG.read_text())["gate_recipe"]
    jitter_rng = np.random.default_rng(99)
    failures = 0
    for phase in ROW_PHASES:
        real = real_medians(phase)
        band = real_band(phase)
        base = phase_row(phase)

        # The center verdict at the pinned recipe.
        center = run_one(base, recipe["parents"], recipe["seed"], recipe["sim_seed"])
        graded = grade(center, phase, band, gate)
        center_ok = not graded["unexpected"] and not graded["contracts"]
        print(
            f"{phase:<9} center score {score(center, real):.3f} "
            f"i1/i10/i100 {center['impact']['1']:.2f}/{center['impact']['10']:.2f}/"
            f"{center['impact']['100']:.2f}  "
            f"unexpected {len(graded['unexpected'])}  deferred {len(graded['deferred'])}"
        )
        for key, sub, c, lo, hi in graded["unexpected"]:
            print(f"          UNEXPECTED {cell_name(key, sub)}: {c:.4f}  bound {lo:.4f}..{hi:.4f}")
        for name, value, bound in graded["contracts"]:
            print(f"          CONTRACT {name}: {value:.4f} over {bound:.4f}")
        ledger_by_cell = {
            (e["key"], e.get("sub")): e for e in gate["ledger"].get(phase, [])
        }
        for key, sub, c, lo, hi in graded["deferred"]:
            recorded = ledger_by_cell[(key, sub)]["value"]
            print(
                f"          deferred {cell_name(key, sub)}: {c:.4f} "
                f"(ledger {recorded:.4f}, band {lo:.4f}..{hi:.4f})"
            )

        # Extra center realizations: diagnostics only.
        for extra in range(1, args.diagnostic_seeds + 1):
            seed = recipe["seed"] + extra
            m = run_one(base, recipe["parents"], seed, seed + 1)
            g = grade(m, phase, band, gate)
            print(
                f"          seed {seed} diagnostic: unexpected {len(g['unexpected'])} "
                f"({', '.join(cell_name(k, s) for k, s, *_ in g['unexpected']) or 'none'})"
            )

        # The neighborhood: the collapse floor and the one-sided
        # contracts bind; the sub-threshold share and the score spread
        # are diagnostics.
        nbhd = json.loads(BOOK_CONFIG.read_text())["neighborhood"]
        floor = nbhd["collapse_floor"]
        mark = nbhd["report_threshold"]
        scores, floor_kept, contract_fails = [], [], 0
        sub_mark = {lag: 0 for lag in (1, 10, 100)}
        samples_run = 0
        worst = (float("inf"), None)
        for sample in range(1, args.samples + 1):
            cfg = dict(base)
            for knob, width in JITTER.items():
                cfg[knob] = float(cfg[knob] + jitter_rng.uniform(-width, width))
            cfg["p_target_1"] = float(np.clip(cfg["p_target_1"], 0.05, 0.9))
            cfg["p_target_2"] = float(
                np.clip(cfg["p_target_2"], 0.05, 0.95 - cfg["p_target_1"])
            )
            cfg["slack"] = max(0.5, cfg["slack"])
            # The adopted mechanism is shared slack: the per-state copies
            # follow the jittered value, or the jitter tests a
            # state-dependent perturbation nobody adopted.
            cfg["slack_1"] = cfg["slack"]
            cfg["slack_3"] = cfg["slack"]
            for seed in range(1, args.seeds + 1):
                m = run_one(cfg, args.parents, seed, seed + 1)
                g = grade(m, phase, band, gate)
                scores.append(score(m, real))
                samples_run += 1
                kept = all(
                    m["impact"][str(lag)] >= floor * real["impact"][str(lag)]
                    for lag in (1, 10, 100)
                )
                floor_kept.append(kept)
                for lag in (1, 10, 100):
                    if m["impact"][str(lag)] < mark * real["impact"][str(lag)]:
                        sub_mark[lag] += 1
                if not kept and m["impact"]["10"] < worst[0]:
                    worst = (m["impact"]["10"], cfg)
                contract_fails += len(g["contracts"])
        lags_kept = all(floor_kept)
        sub_share = "/".join(
            f"{sub_mark[lag] / samples_run:.2f}" for lag in (1, 10, 100)
        )
        print(
            f"          nbhd score p50 {np.median(scores):.3f} max {max(scores):.3f} "
            f"(diagnostic)  floor kept {np.mean(floor_kept):.2f}  "
            f"sub-{mark} share i1/i10/i100 {sub_share}  "
            f"contract violations {contract_fails}"
        )
        if worst[1] is not None:
            deltas = "  ".join(
                f"{k}{worst[1][k] - base[k]:+.3f}"
                for k in JITTER
                if abs(worst[1][k] - base[k]) > 1e-9
            )
            print(f"          worst i10 {worst[0]:.2f} at {deltas}")
        ok = center_ok and lags_kept and contract_fails == 0
        if not ok:
            failures += 1
        print(
            f"          verdict {'PASS' if ok else 'FAIL'} "
            f"(center {'ok' if center_ok else 'unexpected'}, "
            f"lags kept {lags_kept}, contracts {contract_fails})"
        )
    if failures:
        raise SystemExit(f"{failures} phase rows failed the robustness verdict")


if __name__ == "__main__":
    main()
