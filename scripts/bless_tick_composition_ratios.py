#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only
"""Bless the reference for the `tick_composition_ratios.py` absorption.

Phase 4b item 3. Unlike `select_windows`, this script's inputs are all
COMMITTED - the six `analysis/tick-composition-protocol-N.json` fixtures - so
the reference is exact and cheap rather than a 1.5 GB archive sweep. What it
needs blessing for is the same reason as before: the script PRINTS its result
and pins nothing, so there is no artifact a port can be matched against.

It imports the module rather than reimplementing it, so the reference is the
Python's own arithmetic. Nothing here modifies the retiring script.

All four modes are captured, plus the two gate paths that produce no ratios:
the 8/9 identity verifier and the selftest. Their VERDICTS are what matters -
each either passes or raises - so the blessing records that they passed, which
is the claim a port has to reproduce.

    python3 scripts/bless_tick_composition_ratios.py
"""

import io
import json
import os
import sys
import traceback
from contextlib import redirect_stdout

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "analysis"))

import tick_composition_ratios as tcr  # noqa: E402

OUT = os.path.join(ROOT, "analysis", "tick-composition-ratios-blessed.json")


def run_mode(name):
    mode = tcr.MODES[name]
    before_path = tcr.ROOT / mode["before"]
    after_path = tcr.ROOT / mode["after"]
    before = json.loads(before_path.read_text())
    after = json.loads(after_path.read_text())
    return tcr.compare(name, before, after, before_path, after_path)


def gate(fn):
    """Runs a gate and records whether it passed, with the failure if not."""
    try:
        with redirect_stdout(io.StringIO()):
            fn()
        return {"passed": True}
    except BaseException as exc:  # noqa: BLE001 - the verdict is the point
        return {
            "passed": False,
            "error": f"{type(exc).__name__}: {exc}",
            "traceback": traceback.format_exc(limit=3),
        }


def main():
    modes = {name: run_mode(name) for name in sorted(tcr.MODES)}

    blessed = {
        "_doc": (
            "Reference for the tick_composition_ratios.py absorption, phase 4b item 3. "
            "Produced by scripts/bless_tick_composition_ratios.py from the Python's own "
            "functions while it was still runnable. The Rust port is matched against this. "
            "Floats are exact decimal round-trips, so the comparison is bit-exact."
        ),
        "constants": {
            "MODES": {
                name: {
                    "versions": list(mode["versions"]),
                    "before": mode["before"],
                    "after": mode["after"],
                    "same_pairing": mode["same_pairing"],
                    "acceptance": mode.get("acceptance"),
                    "baseline": dict(mode["baseline"]),
                }
                for name, mode in sorted(tcr.MODES.items())
            },
            "IDENTITY_SEPARATELY_VALIDATED": list(tcr.IDENTITY_SEPARATELY_VALIDATED),
            "CALENDAR_FREE": list(tcr.CALENDAR_FREE),
            "CALENDAR_BEARING": list(tcr.CALENDAR_BEARING),
        },
        "modes": modes,
        "gates": {
            "verify_8_9_identity": gate(
                lambda: tcr.verify_8_9_identity(
                    tcr.ROOT / "analysis/tick-composition-protocol-8.json",
                    tcr.ROOT / "analysis/tick-composition-protocol-9.json",
                )
            ),
            "selftest": gate(tcr.selftest),
        },
    }

    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump(blessed, fh, indent=1, sort_keys=False)
        fh.write("\n")

    print(f"wrote {OUT}")
    for name in sorted(modes):
        print(f"  {name}: proposed {modes[name]['proposed']}")
    for name, result in blessed["gates"].items():
        print(f"  gate {name}: {'PASS' if result['passed'] else 'FAIL'}")
        if not result["passed"]:
            print(f"    {result['error']}")


if __name__ == "__main__":
    main()
