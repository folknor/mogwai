#!/usr/bin/env python3
"""Run characterize() across a representative set of pairs in parallel.

The model is symbol-agnostic, so the corpus's job is only to confirm the
stylized-fact shape repeats across instruments and to bound the per-instrument
scalars (price level, tick, size, activity). A handful of representative pairs
settles both far cheaper than the full 1119-file grind. Each pair streams in its
own process (true parallelism, bounded memory per worker); results land as
char_<PAIR>.json and a compact cross-pair table prints at the end.

Usage:  python3 analysis/run_corpus.py [PAIR ...]
"""

import json
import os
import sys
from multiprocessing import Pool

from characterize import DATA_DIR, characterize

DEFAULT_PAIRS = [
    "XBTUSD", "ETHUSD", "USDTUSD", "SOLUSD",
    "ADAUSD", "XDGUSD", "XRPUSD", "DOTUSD",
]
HERE = os.path.dirname(__file__)


def usable(pair):
    path = os.path.join(DATA_DIR, pair + ".csv")
    try:
        return os.path.getsize(path) > 0
    except OSError:
        return False


def work(pair):
    rep = characterize(os.path.join(DATA_DIR, pair + ".csv"))
    with open(os.path.join(HERE, f"char_{pair}.json"), "w") as f:
        json.dump(rep, f)
    return rep


def main():
    requested = sys.argv[1:] or DEFAULT_PAIRS
    pairs = [p for p in requested if usable(p)]
    skipped = [p for p in requested if p not in pairs]
    if skipped:
        print(f"skipped (missing/empty): {skipped}")
    if not pairs:
        print("no usable pairs", file=sys.stderr)
        return 1
    print(f"characterizing {len(pairs)} pairs in parallel: {pairs}")

    # cap workers so peak IO/CPU stays sane; memory per worker is trivial
    with Pool(processes=min(len(pairs), 6)) as pool:
        reps = pool.map(work, pairs)

    reps.sort(key=lambda r: r["n_trades"], reverse=True)
    print("\n== cross-pair stylized facts (confirm universality) ==")
    hdr = (f"{'pair':<9}{'trades':>13}  {'disp':>7}  {'ret1':>6}  "
           f"{'|ret|1':>6}  {'zchg':>5}  {'tick':>10}  {'pdec':>4}  {'rnd':>5}")
    print(hdr)
    for r in reps:
        d, q = r["duration"], r["returns"]
        print(f"{r['pair']:<9}{r['n_trades']:>13,}  "
              f"{d['dispersion_index']:>7.0f}  "
              f"{q['acf'][0]:>6.2f}  {q['abs_acf'][0]:>6.2f}  "
              f"{q['zero_change_frac']:>5.2f}  "
              f"{str(q['modal_tick']):>10}  {str(q['price_decimals_mode']):>4}  "
              f"{r['size']['round_frac']:>5.2f}")
    print("\ndisp=duration dispersion index (Poisson=1); ret1/|ret|1=lag-1 ACF; "
          "zchg=zero-price-change frac; rnd=size <=2 decimals frac")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
