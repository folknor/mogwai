#!/usr/bin/env python3
"""Repeat one already-built libtest binary under deliberate parallel load.

The workspace's focused runner (`brokkr test`) always passes
`--test-threads=1`, which is exactly the condition under which the two
intermittent lifecycle leads never fire: both were only ever observed
aborting a full parallel `brokkr check --gate` sweep, where dozens of venues
and their tapes contend for the host. This runs the binary libtest already
built, with the thread count, the repetition, and the optional background CPU
load the hunt needs, and reports how many rounds went red plus the panic lines
from every red one.

Usage:
  python3 scripts/flake_hunt.py <test-binary> --threads N --rounds M
                                [--filter SUBSTRING] [--load K]

`--load K` spins K busy processes for the duration, which is what turns an
idle 32-core host into something resembling a saturated gate sweep.
"""

from __future__ import annotations

import argparse
import multiprocessing
import subprocess
import sys


def _burn(stop) -> None:
    x = 0
    while not stop.is_set():
        x = (x * 1103515245 + 12345) % 2147483647


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("binary")
    parser.add_argument("--threads", type=int, default=16)
    parser.add_argument("--rounds", type=int, default=10)
    parser.add_argument("--filter", default="")
    parser.add_argument("--load", type=int, default=0)
    args = parser.parse_args()

    stop = multiprocessing.Event()
    burners = [
        multiprocessing.Process(target=_burn, args=(stop,), daemon=True)
        for _ in range(args.load)
    ]
    for burner in burners:
        burner.start()

    failed = 0
    try:
        for round_index in range(1, args.rounds + 1):
            argv = [
                args.binary,
                "--include-ignored",
                "--nocapture",
                f"--test-threads={args.threads}",
            ]
            if args.filter:
                argv.append(args.filter)
            done = subprocess.run(argv, capture_output=True, text=True, check=False)
            if done.returncode == 0:
                print(f"round {round_index}/{args.rounds}: pass", flush=True)
                continue
            failed += 1
            print(
                f"round {round_index}/{args.rounds}: FAIL (total {failed})", flush=True
            )
            lines = (done.stdout + done.stderr).splitlines()
            noise = ("empirical-corpus-range", "instrument preset override")
            # FROM THE FIRST PANIC TO THE END, ONCE. Without the break this
            # printed the whole remaining output per panic line - the first
            # panic dumped the tail, and every later one dumped it again - so a
            # red round's report grew quadratically in the number of panics and
            # buried the failure it was meant to show.
            for index, line in enumerate(lines):
                if "panicked at" in line:
                    for tail in lines[index:]:
                        if any(word in tail for word in noise):
                            continue
                        print(f"    {tail}", flush=True)
                    break
    finally:
        stop.set()
        for burner in burners:
            burner.join(timeout=5)

    print(f"{failed} of {args.rounds} rounds failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
