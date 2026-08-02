#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Count the timestamp precision actually present in a corpus CSV.

A duration model can only be fitted over gaps its source data can express. This
counts, over every row, how many significant decimal places the timestamp
column carries, split by the era window - so "the corpus resolves to one
second" is a measurement rather than an assumption.

It exists because the Kraken anchor turned out to carry WHOLE-SECOND timestamps
on all 81,810,187 rows, which means its finest expressible inter-trade gap is
one second and 61% of consecutive trades record a gap of exactly zero. The
duration ACF this corpus measured on the raw series (0.1603) collapses to
0.0012 once same-timestamp trades are treated as one arrival - a whole-second
corpus cannot adjudicate sub-second arrival structure, which is why the
raw-fill cadence work (git history, `ca72e89` onward) fitted the arrival clock
against microsecond-stamped Binance archives instead.

The consequence generalises to any corpus considered for fitting: check the
resolution before trusting a duration statistic drawn from it, and never fit a
cadence finer than the source can express.

Usage:
    python3 analysis/probe_timestamp_precision.py                  # anchor pair
    python3 analysis/probe_timestamp_precision.py ETHUSD
    python3 analysis/probe_timestamp_precision.py /abs/path/to/FILE.csv
"""

import collections
import os
import sys

from characterize import DATA_DIR, DWELL_ERA_START_TS

ANCHOR = "XBTUSD"


def probe(path):
    counts = collections.Counter()
    rows = 0
    with open(path, "r", errors="replace") as f:
        for line in f:
            field = line.split(",", 1)[0]
            if len(field) < 5:
                continue
            try:
                value = float(field)
            except ValueError:
                continue
            rows += 1
            places = len(field.split(".")[1].rstrip("0")) if "." in field else 0
            counts[(places, value >= DWELL_ERA_START_TS)] += 1

    print(f"file          {path}")
    print(f"rows          {rows:,}")
    print(f"era start     {DWELL_ERA_START_TS}")
    for (places, in_era), count in sorted(counts.items()):
        era = "in era" if in_era else "pre-era"
        print(f"  {places} decimals, {era}: {count:,}")

    finest = max((p for (p, _), c in counts.items() if c), default=0)
    print(f"finest resolution present: {10 ** -finest:g} s")
    if finest == 0:
        print("  -> gaps below one second are NOT expressible in this corpus;")
        print("     any duration statistic drawn from it is quantized to 1 s.")


if __name__ == "__main__":
    arg = sys.argv[1] if len(sys.argv) > 1 else ANCHOR
    probe(arg if os.path.sep in arg else os.path.join(DATA_DIR, f"{arg}.csv"))
