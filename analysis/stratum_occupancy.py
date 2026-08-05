#!/usr/bin/env python3
"""Power check for the held-out deciding strata of the sampling-frame experiment.

Preregistration `notes/sampling-frame-preregistration.md` fixes quartile stratum
boundaries on the calibration span and applies them unchanged to the held-out
span. That makes held-out cell occupancy a RANDOM quantity, not a design
quantity, and the deciding comparison needs both the calm and the extreme cell
populated. This prices the risk before any archive is downloaded.

Under stationarity a held-out month lands in each of the four calibration
quartiles with probability 0.25, independently. That is the OPTIMISTIC case:
real volatility is persistent month to month, so the true occupancy is more
clustered than multinomial and these numbers are an upper bound on the chance of
a usable split.

Usage:
    python3 analysis/stratum_occupancy.py            # the preregistered 12/7
    python3 analysis/stratum_occupancy.py 30 12      # calibration, held-out
"""

from __future__ import annotations

import sys
from math import comb

# The preregistered gate: both deciding cells need at least this many months, or
# pooled standard deviation is undefined or meaningless.
MIN_MONTHS_PER_DECIDING_STRATUM = 2

STRATA = 4


def both_cells_ok(held_out: int, floor: int) -> float:
    """P(calm >= floor AND extreme >= floor) over an exact multinomial."""
    p = 1.0 / STRATA
    rest_p = 1.0 - 2 * p
    total = 0.0
    for calm in range(held_out + 1):
        for extreme in range(held_out - calm + 1):
            if calm < floor or extreme < floor:
                continue
            rest = held_out - calm - extreme
            total += (
                comb(held_out, calm)
                * comb(held_out - calm, extreme)
                * p**calm
                * p**extreme
                * rest_p**rest
            )
    return total


def one_cell(held_out: int, k: int) -> float:
    p = 1.0 / STRATA
    return comb(held_out, k) * p**k * (1 - p) ** (held_out - k)


def main() -> None:
    args = sys.argv[1:]
    calibration = int(args[0]) if args else 12
    held_out = int(args[1]) if len(args) > 1 else 7

    print(f"calibration months {calibration}, held-out months {held_out}")
    print(f"floor per deciding stratum: {MIN_MONTHS_PER_DECIDING_STRATUM}")
    print()
    print("held-out occupancy of ONE stratum:")
    for k in range(min(held_out, 5) + 1):
        print(f"  exactly {k}: {one_cell(held_out, k):.4f}")
    print()
    ok = both_cells_ok(held_out, MIN_MONTHS_PER_DECIDING_STRATUM)
    print(f"P(calm and extreme both reach the floor) = {ok:.4f}")
    print(f"P(the experiment stops on this gate)     = {1 - ok:.4f}")
    print()
    print("held-out months needed for the gate to clear at a given confidence:")
    for target in (0.80, 0.90, 0.95):
        n = held_out
        while both_cells_ok(n, MIN_MONTHS_PER_DECIDING_STRATUM) < target:
            n += 1
            if n > 400:
                break
        print(f"  {target:.0%}: {n} held-out months")


if __name__ == "__main__":
    main()
