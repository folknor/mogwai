#!/usr/bin/env python3
"""Identify which tape a quotes CSV is: spread pmf, size pmf, and the
mid's grid parity. A placed constant-width book has one spread value and
integer-tick mids; a running discrete book varies both.

    python3 quote_csv_probe.py data/mnq-book-quotes-w1.csv
"""

from __future__ import annotations

import sys
from collections import Counter

import numpy as np

TICK = 0.25


def main() -> None:
    data = np.genfromtxt(sys.argv[1], delimiter=",", names=True, max_rows=2_000_000)
    spread = np.round((data["ask_px"] - data["bid_px"]) / TICK).astype(int)
    print("spread pmf:", dict(sorted(Counter(spread.tolist()).most_common(8))))
    sizes = data["bid_sz"].astype(int)
    print("bid_sz pmf:", dict(sorted(Counter(sizes.tolist()).most_common(8))))
    mid2 = np.round((data["bid_px"] + data["ask_px"]) / TICK).astype(int)
    print("mid on half-tick share:", float((mid2 % 2 == 1).mean()))


if __name__ == "__main__":
    main()
