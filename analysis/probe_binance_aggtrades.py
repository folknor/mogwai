#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Probe a Binance aggTrades monthly archive for the statistics the fingerprint
is fitted on, so a second venue can be compared against the Kraken corpus
without touching the committed pipeline.

Exploratory: this is not part of the fingerprint build. It answers one
question - does Binance BTCUSDT have the same SHAPE as the Kraken anchor at a
different scale, or a different shape - by computing the same estimands
`analysis/characterize.py` computes, with the same formulas.

Streaming and O(1) in memory over a multi-gigabyte member, matching the corpus
pass's discipline: a ring buffer for the duration ACF, running sums for
everything else.

Note on resolution: aggTrades timestamps are MICROSECONDS (16-digit; Binance
moved off milliseconds), so inter-trade gaps floor at 1 us and a taker sweep
landing several aggregated prints inside one microsecond reads as a run of
zero-length durations. The Kraken corpus carries decimal seconds, so the two
are not comparable in the far-left tail. Bulk and right-tail statistics remain
comparable; a "fraction of gaps under X" comparison does not.

Note on the dispersion index: `characterize.py` computes `var / mean`, which
carries units of seconds and is therefore NOT scale-free - halve the cadence
and the index halves with it, whatever the shape does. Comparing two venues at
different cadences needs the dimensionless `var / mean^2` (squared coefficient
of variation, 1 for a Poisson process). Both are printed, because the committed
band is stated in the first and only the second answers "is this the same
shape".

Usage:
    python3 analysis/probe_binance_aggtrades.py BTCUSDT-aggTrades-2026-06.zip
"""

import csv
import io
import math
import sys
import zipfile

MAX_LAG = 5


class AutoCorr:
    """Streaming autocorrelation up to max_lag, mirroring characterize.py."""

    def __init__(self, max_lag):
        self.k = max_lag
        self.ring = [0.0] * max_lag
        self.pos = 0
        self.filled = 0
        self.n = 0
        self.sum = 0.0
        self.sumsq = 0.0
        self.cross = [0.0] * (max_lag + 1)

    def push(self, x):
        self.n += 1
        self.sum += x
        self.sumsq += x * x
        d = 1
        i = self.pos
        while d <= self.filled:
            i = (i - 1) % self.k
            self.cross[d] += x * self.ring[i]
            d += 1
        self.ring[self.pos] = x
        self.pos = (self.pos + 1) % self.k
        if self.filled < self.k:
            self.filled += 1

    def acf(self):
        if self.n < 2:
            return []
        mean = self.sum / self.n
        var = self.sumsq / self.n - mean * mean
        if var <= 0:
            return [0.0] * self.k
        out = []
        for d in range(1, self.k + 1):
            m = self.n - d
            cov = self.cross[d] / m - mean * mean
            out.append(cov / var)
        return out


def probe(path):
    z = zipfile.ZipFile(path)
    member = z.namelist()[0]

    n = 0
    prev_ms = None
    dur_n = 0
    dur_sum = 0.0
    dur_sumsq = 0.0
    dur_zero = 0
    dur_acf = AutoCorr(MAX_LAG)
    # Collapsed view: Binance stamps one match event - a taker order sweeping
    # several makers - with a single transact_time, so consecutive aggTrades
    # sharing a timestamp are one arrival with a multiplicity, not several
    # arrivals a microsecond apart. Grouping by distinct timestamp recovers the
    # real inter-event duration, and the group size is the sweep width.
    ev_prev = None
    ev_n = 0
    ev_sum = 0.0
    ev_sumsq = 0.0
    ev_acf = AutoCorr(MAX_LAG)
    sweep_cur = 0
    sweep_n = 0
    sweep_sum = 0
    sweep_max = 0
    sweep_hist = {}
    size_sum = 0.0
    size_hist = {}
    maker_qty = 0.0
    first_ms = last_ms = None

    with z.open(member) as f:
        for row in csv.reader(io.TextIOWrapper(f, newline="")):
            if not row or not row[0].lstrip("-").isdigit():
                continue
            qty = float(row[2])
            us = int(row[5])
            is_maker = row[6].strip().lower() == "true"
            n += 1
            if first_ms is None:
                first_ms = us
            last_ms = us
            size_sum += qty
            if qty > 0:
                b = min(29, max(0, int(math.log10(qty)) + 9))
                size_hist[b] = size_hist.get(b, 0) + 1
            if is_maker:
                maker_qty += qty
            if prev_ms is not None:
                dt = (us - prev_ms) / 1_000_000.0
                if dt >= 0:
                    dur_n += 1
                    dur_sum += dt
                    dur_sumsq += dt * dt
                    if dt == 0.0:
                        dur_zero += 1
                    dur_acf.push(dt)
            prev_ms = us

            if us != ev_prev:
                if ev_prev is not None:
                    dt = (us - ev_prev) / 1_000_000.0
                    ev_n += 1
                    ev_sum += dt
                    ev_sumsq += dt * dt
                    ev_acf.push(dt)
                    sweep_n += 1
                    sweep_sum += sweep_cur
                    sweep_max = max(sweep_max, sweep_cur)
                    b = min(9, sweep_cur)
                    sweep_hist[b] = sweep_hist.get(b, 0) + 1
                ev_prev = us
                sweep_cur = 1
            else:
                sweep_cur += 1

    mean = dur_sum / dur_n if dur_n else 0.0
    var = (dur_sumsq / dur_n - mean * mean) if dur_n else 0.0
    span_s = (last_ms - first_ms) / 1_000_000.0 if first_ms is not None else 0.0
    acf = dur_acf.acf()

    print(f"file            {path}")
    print(f"trades          {n:,}")
    print(f"span            {span_s / 86400:.2f} days")
    print(f"trades/sec      {n / span_s:.2f}")
    print(f"duration mean   {mean * 1000:.2f} ms")
    print(f"duration var    {var:.6f} s2")
    print(f"dispersion idx  {(var / mean) if mean else 0:.2f}   (characterize.py var/mean, SCALE-DEPENDENT)")
    print(f"cv squared      {(var / (mean * mean)) if mean else 0:.2f}   (var/mean^2, dimensionless, Poisson=1)")
    print(f"zero-gap frac   {dur_zero / dur_n:.4f}   (microsecond resolution floor)")
    print(f"duration ACF    lag1 {acf[0]:.4f}  lag5 {acf[4]:.4f}")
    print(f"mean trade size {size_sum / n:.6f}")
    print(f"taker-buy share {1 - (maker_qty / size_sum):.4f}")
    top = sorted(size_hist.items(), key=lambda kv: -kv[1])[:5]
    print(f"size decades    {[(b - 9, c) for b, c in top]}   (log10 bin, count)")

    ev_mean = ev_sum / ev_n if ev_n else 0.0
    ev_var = (ev_sumsq / ev_n - ev_mean * ev_mean) if ev_n else 0.0
    ev_a = ev_acf.acf()
    print("--- collapsed to distinct timestamps (one match event per arrival) ---")
    print(f"events          {ev_n + 1:,}   ({(ev_n + 1) / n:.3f} of aggTrades)")
    print(f"events/sec      {(ev_n + 1) / span_s:.2f}")
    print(f"duration mean   {ev_mean * 1000:.2f} ms")
    print(f"dispersion idx  {(ev_var / ev_mean) if ev_mean else 0:.3f}   (var/mean, scale-dependent)")
    print(f"cv squared      {(ev_var / (ev_mean * ev_mean)) if ev_mean else 0:.2f}   (var/mean^2, dimensionless)")
    print(f"duration ACF    lag1 {ev_a[0]:.4f}  lag5 {ev_a[4]:.4f}")
    print(f"sweep width     mean {sweep_sum / sweep_n:.2f}  max {sweep_max}")
    sw = sorted(sweep_hist.items())
    print(f"sweep hist      {[(k, round(100 * v / sweep_n, 1)) for k, v in sw]}   (aggTrades per event, pct; 9 is 9+)")


if __name__ == "__main__":
    probe(sys.argv[1] if len(sys.argv) > 1 else "BTCUSDT-aggTrades-2026-06.zip")
