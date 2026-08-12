#!/usr/bin/env python3
"""Reduce a delivered TBBO month to the gen bars CSV schema.

Produces the observed counterpart of `mogwai gen --type bars`, so a real
month renders in analysis/plot_tape.py identically to a generated one and
the two can be flipped between in the same viewer. Reads the trade legs of
a delivered Databento TBBO csv.zst; emits open_ts,close_ts,open,high,low,
close,volume,trade_count at the requested interval, empty windows carried
forward as zero-volume bars exactly like gen's desert fill.

Usage:
  python3 analysis/observed_bars.py \
      --corpus research/market-data/databento/mnqv/2026-04.manifest.tbbo \
      --interval 60 --out analysis/out/observed-2026-04.csv
"""
import argparse
import csv
import glob
import os
import sys
from compression import zstd

NS = 1_000_000_000


def parse_args():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--corpus", required=True,
                   help="delivered TBBO month directory")
    p.add_argument("--interval", type=int, default=60,
                   help="bar interval in seconds (default 60)")
    p.add_argument("--out", required=True, help="output CSV path")
    return p.parse_args()


def trade_rows(path):
    with zstd.open(path, "rt", encoding="utf-8", newline="") as text:
        for row in csv.DictReader(text):
            # TBBO rows carry the trade leg in price/size; action T is
            # the trade print in the delivered csv encoding.
            if row.get("action") not in (None, "T"):
                continue
            # The delivery is pretty_px = False (the frozen submit flags),
            # so price arrives in 1e-9 point units.
            yield int(row["ts_event"]), float(row["price"]) / 1e9, int(row["size"])


def main():
    args = parse_args()
    files = sorted(glob.glob(os.path.join(args.corpus, "*.tbbo.csv.zst")))
    if not files:
        sys.exit(f"observed_bars: no *.tbbo.csv.zst under {args.corpus}")
    step = args.interval * NS

    os.makedirs(os.path.dirname(args.out) or ".", exist_ok=True)
    out = csv.writer(open(args.out, "w", newline=""))
    out.writerow(["open_ts", "close_ts", "open", "high", "low", "close",
                  "volume", "trade_count"])

    window = None  # (open_ts, o, h, l, c, vol, n)
    last_close = None

    def flush(w):
        out.writerow([w[0], w[0] + step, w[1], w[2], w[3], w[4], w[5], w[6]])

    for path in files:
        for ts, price, size in trade_rows(path):
            wstart = ts - ts % step
            if window is None:
                window = [wstart, price, price, price, price, size, 1]
                last_close = price
                continue
            if wstart == window[0]:
                window[2] = max(window[2], price)
                window[3] = min(window[3], price)
                window[4] = price
                window[5] += size
                window[6] += 1
                last_close = price
                continue
            flush(window)
            # Desert fill: carry the close forward through empty windows,
            # matching gen's zero-volume bars, but do not fabricate bars
            # across the closed calendar beyond a session gap heuristic of
            # one hour - a longer gap is a closure and stays a gap.
            gap = wstart - (window[0] + step)
            if 0 < gap <= 3600 * NS:
                t = window[0] + step
                while t < wstart:
                    out.writerow([t, t + step, last_close, last_close,
                                  last_close, last_close, 0, 0])
                    t += step
            window = [wstart, price, price, price, price, size, 1]
            last_close = price
    if window is not None:
        flush(window)
    print(f"observed_bars: wrote {args.out}")


if __name__ == "__main__":
    main()
