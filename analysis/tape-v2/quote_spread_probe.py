#!/usr/bin/env python3
"""Quick diagnostic on a `gen --type quotes` CSV: the pre-trade spread pmf
and the midpoint-change pmf between consecutive published quotes, the two
channels codex separated for the book dprice failure - a spread stuck wide
inflates the bounce tail, a midpoint that moves every quote is the
touch-translation channel. Stdlib only.

    python3 analysis/tape-v2/quote_spread_probe.py CSV [TICK]
"""

from __future__ import annotations

import csv
import sys
from collections import Counter


def main(argv: list[str]) -> None:
    path = argv[0]
    tick = float(argv[1]) if len(argv) > 1 else 0.25
    spreads: Counter[int] = Counter()
    mids: list[float] = []
    with open(path) as f:
        reader = csv.reader(f)
        next(reader)
        for row in reader:
            bid, ask = float(row[1]), float(row[2])
            spreads[round((ask - bid) / tick)] += 1
            mids.append((bid + ask) / 2.0)
    total = sum(spreads.values())
    print(f"quotes {total}")
    print("pre-trade spread pmf (real MNQ pooled: 1t 0.43, 2t 0.43):")
    for s in sorted(spreads):
        print(f"  {s} tick: {spreads[s] / total:.3f}")
    changes: Counter[float] = Counter()
    for i in range(1, len(mids)):
        changes[round(abs(mids[i] - mids[i - 1]) / tick, 2)] += 1
    dtot = sum(changes.values())
    print("midpoint change between consecutive quotes, in ticks:")
    for d in sorted(changes)[:10]:
        print(f"  {d}: {changes[d] / dtot:.3f}")
    unchanged = changes.get(0.0, 0) / dtot
    print(f"midpoint unchanged share: {unchanged:.3f}")


if __name__ == "__main__":
    main(sys.argv[1:])
