#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Run smoke modes repeatedly and report every failure with its reason.

Usage:

    python3 scripts/smoke-sweep.py [MODE ...] [REPEATS]
    python3 scripts/smoke-sweep.py [MODE ...] [--repeat N]

REPEATS is a bare integer in the final position, so `smoke-sweep.py stop 6`
means six runs of the stop mode - the reading anyone types. `--repeat N` says
the same thing and still works. With no MODE, sweeps every mode smoke.py knows;
with no repeat count, runs each mode once.

Modes are validated against smoke.py's own table before anything is spawned, so
a typo (or an integer in a position that cannot be a repeat count) is named as
such instead of being run as a mode and reported as a venue failure.

This exists to expose seed-dependent flakiness: a single green run of a mode
whose outcome depends on the run seed proves nothing.
"""

import argparse
import importlib.util
import os
import subprocess
import sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SMOKE = os.path.join(REPO, "scripts", "smoke.py")


def known_modes() -> list[str]:
    """smoke.py's own mode table, so this script cannot drift out of date.

    The previous hand-copied list had already lost `fees` and `futures`, which
    meant the whole-suite sweep silently skipped the two modes nobody was
    watching.
    """
    spec = importlib.util.spec_from_file_location("smoke", SMOKE)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return sorted(module.MODES)


def parse(argv: list[str], modes: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="smoke-sweep.py",
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument(
        "words",
        nargs="*",
        metavar="MODE ... [REPEATS]",
        help=f"modes to sweep (default: all of {', '.join(modes)}), "
        "optionally followed by a repeat count",
    )
    parser.add_argument(
        "--repeat",
        type=int,
        default=None,
        help="runs per mode; equivalent to a trailing bare integer",
    )
    parsed = parser.parse_args(argv)

    words = list(parsed.words)
    repeat = parsed.repeat
    if words and words[-1].isdigit():
        trailing = int(words.pop())
        if repeat is not None and repeat != trailing:
            parser.error(
                f"repeat count given twice and differently: --repeat {repeat} "
                f"and trailing {trailing}"
            )
        repeat = trailing
    if repeat is None:
        repeat = 1
    if repeat < 1:
        parser.error(f"a repeat count is at least 1, got {repeat}")

    unknown = [word for word in words if word not in modes]
    if unknown:
        parser.error(
            f"unknown mode(s) {', '.join(unknown)}; known modes are {', '.join(modes)}"
        )

    parsed.modes = words or modes
    parsed.repeat = repeat
    return parsed


def main() -> int:
    modes = known_modes()
    parsed = parse(sys.argv[1:], modes)

    failures = 0
    for mode in parsed.modes:
        for attempt in range(parsed.repeat):
            done = subprocess.run(
                [sys.executable, SMOKE, mode],
                cwd=REPO,
                capture_output=True,
                text=True,
            )
            label = f"{mode} #{attempt + 1}/{parsed.repeat}"
            if done.returncode == 0:
                tail = done.stdout.strip().splitlines()
                print(f"pass {label}: {tail[-1] if tail else ''}")
            else:
                failures += 1
                print(f"FAIL {label} exit {done.returncode}")
                print(done.stdout.strip())
                print(done.stderr.strip())
            sys.stdout.flush()
    total = len(parsed.modes) * parsed.repeat
    print(f"\n{failures} failures out of {total} runs")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
