#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Probe Binance 1-second kline archives for arrival rate and trade size.

Klines carry `Number of trades` and `Taker buy base asset volume` per bar, so a
1s archive answers the two questions bars normally cannot - how many trades per
second, and how large is one - without needing trade-level data. At 1s
resolution there is no timestamp-collision artifact, unlike aggTrades, so the
per-second count distribution is a clean read on burstiness.

Counts here are RAW fills, which is the largest of the three trade definitions
(raw fills > aggTrades > match events; see notes/problem-trade-cadence.md).
Do not compare this number against an aggTrades rate without saying which is
which.

Streaming and O(1) in memory except for the per-second count vector, which is
one int per second of the archive (2.6M for a month).

Usage:
    python3 analysis/probe_binance_klines.py research/market-data/BTCUSDT-1s-2026-06.zip
    python3 analysis/probe_binance_klines.py research/market-data/*-1s-2026-06.zip
"""

import csv
import io
import statistics
import sys
import zipfile

# Binance kline columns, spot:
# 0 open_time, 1 open, 2 high, 3 low, 4 close, 5 volume, 6 close_time,
# 7 quote_asset_volume, 8 number_of_trades, 9 taker_buy_base, 10 taker_buy_quote
COL_VOLUME = 5
COL_QUOTE = 7
COL_COUNT = 8
COL_TAKER_BASE = 9


def probe(path):
    z = zipfile.ZipFile(path)
    member = z.namelist()[0]

    counts = []
    volume = 0.0
    quote = 0.0
    taker = 0.0

    with z.open(member) as f:
        for row in csv.reader(io.TextIOWrapper(f, newline="")):
            if not row or not row[0].lstrip("-").isdigit():
                continue
            counts.append(int(row[COL_COUNT]))
            volume += float(row[COL_VOLUME])
            quote += float(row[COL_QUOTE])
            taker += float(row[COL_TAKER_BASE])

    total = sum(counts)
    seconds = len(counts)
    ordered = sorted(counts)
    zero = sum(1 for c in counts if c == 0)

    print(path.split("/")[-1])
    print(f"  seconds         {seconds:,}")
    print(f"  raw trades      {total:,}")
    print(f"  trades/sec      mean {statistics.mean(counts):.2f}"
          f"  median {statistics.median(counts):.0f}"
          f"  p95 {ordered[int(0.95 * seconds)]}"
          f"  max {ordered[-1]}")
    print(f"  mean over median{statistics.mean(counts) / max(1, statistics.median(counts)):.1f}x"
          "   (burstiness: 1.0 would be a flat tape)")
    print(f"  zero-trade sec  {100 * zero / seconds:.2f}%")
    print(f"  mean trade size {volume / total:.6f}   (native units)")
    print(f"  notional/trade  ${quote / total:,.0f}")
    print(f"  notional/sec    ${quote / seconds:,.0f}")
    print(f"  taker-buy share {taker / volume:.4f}   (of base volume)")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        raise SystemExit("usage: probe_binance_klines.py <archive.zip> [...]")
    for p in sys.argv[1:]:
        probe(p)
