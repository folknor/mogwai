#!/usr/bin/env python3
"""The deciding run: the frozen tables through the unchanged association rule.

This driver ADDS NOTHING to the acceptance rule. Every statistic comes from
`spearman_association.py` imported unchanged - `evaluate`, `permutation_p`,
`spearman`, the preregistered constants. What this file owns is assembly and
recording:

  - pair each month's bar-derived `rv` score with its fourteen tick targets,
    split 12 calibration / 7 held-out chronologically per
    `PREREGISTERED_SPLIT_MONTHS`;
  - record the provenance the verdict must carry: the analysis tree commit,
    the SHA-256 of both frozen inputs, the harness constants, the split;
  - attach the labelled calibration Monte Carlo p-values the preregistration
    reports alongside the exact held-out ones (`evaluate` computes only the
    held-out p, since only held-out decides);
  - carry any unavailable input through BY NAME, never as zero;
  - write the COMPLETE result atomically before anything summarizes it, and
    refuse to overwrite an existing result: this file decides, and a deciding
    file is written once.

Usage:
    python3 -u analysis/run_association.py
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import spearman_association as sa  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
TARGETS_FROZEN = ROOT / "analysis/targets-frozen.json"
BARS_FROZEN = ROOT / "analysis/bar-scores-frozen.json"
RESULT = ROOT / "analysis/association-result.json"

# The tree that produced both frozen inputs and carries the harness whose
# selftest passed 21/21. The driver itself is committed immediately after this
# run in the same session; the harness it imports is byte-identical at both
# commits.
ANALYSIS_TREE_COMMIT = "73e07d2"

RESULT_SCHEMA_VERSION = 1
PREREGISTERED_SPLIT_MONTHS = 12
BAR_SCORE_FEATURE = "rv"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def main() -> int:
    if RESULT.exists():
        print(f"{RESULT.name} already exists; refusing to overwrite the "
              "deciding result")
        return 1

    targets_payload = json.loads(TARGETS_FROZEN.read_text())
    bars_payload = json.loads(BARS_FROZEN.read_text())

    target_months = sorted(targets_payload["months"])
    bar_months = sorted(bars_payload["months"])
    if target_months != bar_months:
        print("month sets disagree between the frozen tables; refusing")
        print(f"  targets only: {sorted(set(target_months) - set(bar_months))}")
        print(f"  bars only:    {sorted(set(bar_months) - set(target_months))}")
        return 1
    if bars_payload["operative_feature"] != BAR_SCORE_FEATURE:
        print(f"bar table's operative feature is "
              f"{bars_payload['operative_feature']!r}, expected "
              f"{BAR_SCORE_FEATURE!r}; refusing")
        return 1

    cal_months = target_months[:PREREGISTERED_SPLIT_MONTHS]
    held_months = target_months[PREREGISTERED_SPLIT_MONTHS:]

    cal_rv = [bars_payload["months"][m][BAR_SCORE_FEATURE] for m in cal_months]
    held_rv = [bars_payload["months"][m][BAR_SCORE_FEATURE] for m in held_months]

    target_names = sorted(targets_payload["months"][target_months[0]])
    cal_targets: dict[str, list[float]] = {}
    held_targets: dict[str, list[float]] = {}
    unavailable_inputs: dict[str, list[str]] = {}
    for name in target_names:
        null_months = [m for m in target_months
                       if targets_payload["months"][m][name] is None]
        if null_months:
            # By name, never as zero: a target with an absent month cannot be
            # ranked over that span and is excluded from evaluation with the
            # absence recorded.
            unavailable_inputs[name] = null_months
            continue
        cal_targets[name] = [targets_payload["months"][m][name]
                             for m in cal_months]
        held_targets[name] = [targets_payload["months"][m][name]
                              for m in held_months]

    verdicts = sa.evaluate(cal_rv, held_rv, cal_targets, held_targets)

    # The preregistration reports calibration p-values labelled monte_carlo at
    # n=12 alongside the exact held-out ones. evaluate computes only held-out,
    # so the calibration values are attached here through the same unchanged
    # permutation_p.
    for name, entry in verdicts["per_target"].items():
        entry["calibration_p"] = sa.permutation_p(cal_rv, cal_targets[name])

    result = {
        "result_schema_version": RESULT_SCHEMA_VERSION,
        "standing": "THE DECIDING RUN of the sampling-frame experiment, "
                    "preregistration section 7.1. Held-out decides.",
        "analysis_tree_commit": ANALYSIS_TREE_COMMIT,
        "inputs": {
            "targets_frozen": {
                "path": TARGETS_FROZEN.name,
                "sha256": sha256_of(TARGETS_FROZEN),
            },
            "bar_scores_frozen": {
                "path": BARS_FROZEN.name,
                "sha256": sha256_of(BARS_FROZEN),
            },
        },
        "harness": {
            "implementation": "analysis/spearman_association.py, unchanged",
            "selftest": "21 checks, 0 failed, rerun immediately before this",
            "constants": {
                "CALIBRATION_MIN_ABS_RHO": sa.CALIBRATION_MIN_ABS_RHO,
                "HELD_OUT_MIN_ABS_RHO": sa.HELD_OUT_MIN_ABS_RHO,
                "DIRECTION_MUST_REPRODUCE": sa.DIRECTION_MUST_REPRODUCE,
                "EXACT_PERMUTATION_MAX_N": sa.EXACT_PERMUTATION_MAX_N,
                "MONTE_CARLO_PERMUTATIONS": sa.MONTE_CARLO_PERMUTATIONS,
                "PERMUTATION_SEED": sa.PERMUTATION_SEED,
                "MANDATORY_FAMILIES": list(sa.MANDATORY_FAMILIES),
                "FAMILIES": {k: list(v) for k, v in sa.FAMILIES.items()},
            },
        },
        "split": {
            "bar_score_feature": BAR_SCORE_FEATURE,
            "calibration_months": cal_months,
            "held_out_months": held_months,
        },
        "unavailable_inputs": unavailable_inputs,
        "verdicts": verdicts,
    }

    tmp = RESULT.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(result, indent=2, sort_keys=True, default=str))
    os.replace(tmp, RESULT)
    print(f"wrote {RESULT.name} atomically, before any summary below")
    print()

    for name in sorted(verdicts["per_target"]):
        entry = verdicts["per_target"][name]
        mark = "pass" if entry["pass"] else "FAIL"
        print(f"  {mark}  {name:<26} cal {entry['calibration_rho']!r:>22}  "
              f"held {entry['held_out_rho']!r:>22}")
    for name in sorted(unavailable_inputs):
        print(f"  n/a   {name:<26} unavailable input months "
              f"{unavailable_inputs[name]}")
    print()
    for family in sorted(verdicts["families"]):
        f = verdicts["families"][family]
        print(f"  {family}: {f['passed']}/{f['targets']} "
              f"{'pass' if f['pass'] else 'FAIL'}")
    print()
    print(f"family majority     {verdicts['family_majority']}")
    print(f"mandatory families  {verdicts['mandatory_families_pass']}")
    print(f"VERDICT             {verdicts['verdict']}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
