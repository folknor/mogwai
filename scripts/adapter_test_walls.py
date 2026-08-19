#!/usr/bin/env python3
"""Per-test wall times for a set of already-built libtest binaries.

WHY THIS EXISTS. The `mogwai-adapter` socket suites were ~40 s under
`brokkr test -p mogwai-adapter "" --debug`, and for four rounds nobody had a
breakdown of where it goes. libtest's `--report-time` is nightly-only, and the
tool's own per-test wall is printed one test at a time, which would be one
invocation plus one cargo re-resolution PER TEST. This runs the ALREADY-BUILT test
binaries directly, one test per process, and times each - so a distribution
costs roughly one sweep's worth of wall rather than sixty-six.

Usage:
    python3 scripts/adapter_test_walls.py <test-binary> [<test-binary> ...]

Find the paths in the `Running tests/...` lines of a `brokkr test` run; the
hash in them moves whenever the build inputs do, so they are arguments rather
than something this script guesses.

HOW TO READ THE NUMBERS. One process per test is deliberate: every test in
these binaries is `flavor = "current_thread"` and binds its own stub, so a
test's wall is genuinely its own and the ~2 ms process spawn is common to all
of them. It is NOT the same measurement as the serial sweep, which reuses one
process per binary, so the column is a RANKING and an order-of-magnitude
accounting rather than a sum that must reconcile with the sweep's total. Wall
measurement at this level does not resolve below ~10 ms.

Not part of any gate. It is an instrument for a question that recurs - "which
tests are the wall?" - and it is generic over libtest binaries, so it is not
one round's scratch file.
"""

import subprocess
import sys
import time


def list_tests(binary: str) -> list[str]:
    """Every test in the binary, ignored ones included."""
    listing = subprocess.run(
        [binary, "--list", "--include-ignored", "--format", "terse"],
        capture_output=True,
        text=True,
        check=True,
    )
    return [
        line.removesuffix(": test")
        for line in listing.stdout.splitlines()
        if line.endswith(": test")
    ]


def time_test(binary: str, test: str) -> tuple[float, int]:
    """Wall milliseconds and exit status for one test, run alone."""
    start = time.perf_counter()
    finished = subprocess.run(
        [binary, "--exact", "--include-ignored", "--test-threads=1", test],
        capture_output=True,
        text=True,
        check=False,
    )
    return (time.perf_counter() - start) * 1000.0, finished.returncode


def main(binaries: list[str]) -> int:
    if not binaries:
        print(__doc__, file=sys.stderr)
        return 2
    rows = []
    for binary in binaries:
        label = binary.rsplit("/", 1)[-1].split("-")[0]
        for test in list_tests(binary):
            millis, status = time_test(binary, test)
            rows.append((millis, status, label, test))
    rows.sort(reverse=True)
    total = sum(row[0] for row in rows)
    for millis, status, label, test in rows:
        flag = "" if status == 0 else f"  FAILED({status})"
        print(f"{millis:8.0f} ms  {label:<22} {test}{flag}")
    print(f"{total:8.0f} ms  TOTAL over {len(rows)} tests, run one per process")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
