#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Run smoke modes repeatedly and report every failure with its reason.

Usage: python3 scripts/smoke-sweep.py [--repeat N] [MODE ...]

With no MODE, sweeps every mode smoke.py knows. This exists to expose
seed-dependent flakiness: a single green run of a mode whose outcome depends on
the run seed proves nothing.
"""

import argparse
import os
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SMOKE = os.path.join(REPO, "scripts", "smoke.py")

ALL_MODES = [
    "default",
    "heartbeat",
    "accelerated",
    "admission",
    "command-latency",
    "band",
    "band-swept",
    "stop",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("modes", nargs="*", default=None)
    parser.add_argument("--repeat", type=int, default=1)
    parsed = parser.parse_args()
    modes = parsed.modes or ALL_MODES

    failures = 0
    for mode in modes:
        for attempt in range(parsed.repeat):
            done = subprocess.run(
                [sys.executable, SMOKE, mode],
                cwd=REPO,
                capture_output=True,
                text=True,
            )
            label = f"{mode} #{attempt + 1}"
            if done.returncode == 0:
                print(f"pass {label}: {done.stdout.strip().splitlines()[-1]}")
            else:
                failures += 1
                print(f"FAIL {label} exit {done.returncode}")
                print(done.stdout.strip())
                print(done.stderr.strip())
            sys.stdout.flush()
    print(f"\n{failures} failures")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
