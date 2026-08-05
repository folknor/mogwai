#!/usr/bin/env python3
"""Search for the fixture vectors `spearman_association.selftest` hardcodes.

A fixture asserted from intuition is a fixture that can be wrong in the same
direction as the code it guards. The first draft of the selftest hand-picked a
"null" permutation that turned out to carry rho = 0.7143 - above the held-out
acceptance threshold - which silently converted four downstream checks into
assertions that a PASSING vector fails. The selftest caught it, which is the
argument for having written the selftest first.

This enumerates all 5,040 permutations of a 7-point ranking and reports the
vectors with the properties the fixtures actually need, so the hardcoded values
carry a derivation rather than a guess.

Run it when a fixture constant needs to change; it is not part of any gate.
"""

from __future__ import annotations

import itertools
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from spearman_association import (  # noqa: E402
    HELD_OUT_MIN_ABS_RHO,
    leave_one_out,
    spearman,
)

XS = [float(v) for v in range(1, 8)]
BASE = [float(v) for v in range(1, 8)]


def main() -> None:
    perms = list(itertools.permutations(BASE))

    nearest = min(perms, key=lambda p: abs(spearman(XS, list(p))))
    print("closest-to-zero permutation")
    print(f"  {tuple(int(v) for v in nearest)}  rho = {spearman(XS, list(nearest)):+.6f}")
    print()

    # A usable null must fail the held-out threshold with margin, so that a
    # small change to the threshold does not silently reclassify the fixture.
    nulls = [
        p for p in perms if abs(spearman(XS, list(p))) < HELD_OUT_MIN_ABS_RHO / 2
    ]
    print(f"permutations with |rho| < {HELD_OUT_MIN_ABS_RHO / 2}: {len(nulls)}")
    for p in sorted(nulls, key=lambda p: abs(spearman(XS, list(p))))[:5]:
        print(f"  {tuple(int(v) for v in p)}  rho = {spearman(XS, list(p)):+.6f}")
    print()

    # A sign-reversal fixture needs a base association strong enough that the
    # reversal is surprising, not a coin flip around zero.
    reversals = []
    for p in perms:
        rho = spearman(XS, list(p))
        if abs(rho) < 0.15:
            continue
        loo = leave_one_out(XS, list(p))
        if loo["sign_reverses"]:
            reversals.append((p, rho, loo["min"], loo["max"]))
    reversals.sort(key=lambda t: -abs(t[1]))
    print(f"sign-reversing permutations with |base rho| >= 0.15: {len(reversals)}")
    for p, rho, lo, hi in reversals[:5]:
        print(
            f"  {tuple(int(v) for v in p)}  base = {rho:+.6f}  "
            f"leave-one-out {lo:+.6f} .. {hi:+.6f}"
        )


if __name__ == "__main__":
    main()
