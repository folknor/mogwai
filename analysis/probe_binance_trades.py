#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Stream a Binance raw-trades archive and infer parent match events.

The archive has no event identifier. Results are reported for two inference
rules: consecutive rows with the same timestamp and taker side, which is the
primary rule, and consecutive rows with the same timestamp only, matching the
aggTrades probe. Memory is bounded by histograms and the current event.
"""

import csv
import io
import math
import sys
import zipfile

try:
    from analysis.probe_binance_aggtrades import AutoCorr
except ModuleNotFoundError:
    from probe_binance_aggtrades import AutoCorr


class EventStats:
    def __init__(self, with_side):
        self.with_side = with_side
        self.key = None
        self.time = None
        self.count = 0
        self.prices = set()
        self.events = 0
        self.single = 0
        self.single_level = 0
        self.children_sum = 0
        self.children_hist = {}
        self.children_max = 0
        self.levels_sum = 0
        self.prev_time = None
        self.gaps = 0
        self.gap_sum = 0.0
        self.gap_sumsq = 0.0
        self.gap_acf = AutoCorr(5)
        self.subsecond_distinct_gaps = 0
        self.subsecond_gap_sum_us = 0

    def _close(self):
        if self.key is None:
            return
        self.events += 1
        self.children_sum += self.count
        self.children_max = max(self.children_max, self.count)
        self.children_hist[self.count] = self.children_hist.get(self.count, 0) + 1
        levels = len(self.prices)
        self.levels_sum += levels
        self.single += self.count == 1
        self.single_level += levels == 1
        if self.prev_time is not None:
            gap_us = self.time - self.prev_time
            gap = gap_us / 1_000_000.0
            self.gaps += 1
            self.gap_sum += gap
            self.gap_sumsq += gap * gap
            self.gap_acf.push(gap)
            if 0 < gap_us < 1_000_000:
                self.subsecond_distinct_gaps += 1
                self.subsecond_gap_sum_us += gap_us
        self.prev_time = self.time

    def push(self, stamp, side, price):
        key = (stamp, side) if self.with_side else stamp
        if key != self.key:
            self._close()
            self.key = key
            self.time = stamp
            self.count = 0
            self.prices = set()
        self.count += 1
        self.prices.add(price)

    def report(self):
        self._close()
        mean_gap = self.gap_sum / self.gaps
        var_gap = self.gap_sumsq / self.gaps - mean_gap * mean_gap
        rank95 = math.ceil(0.95 * self.events)
        seen = 0
        p95 = 0
        for count in sorted(self.children_hist):
            seen += self.children_hist[count]
            if seen >= rank95:
                p95 = count
                break
        acf = self.gap_acf.acf()
        acf.extend([0.0] * (5 - len(acf)))
        return {
            "events": self.events,
            "children": {
                "mean": self.children_sum / self.events,
                "single_frac": self.single / self.events,
                "p95": p95,
                "max": self.children_max,
            },
            "levels": {
                "mean": self.levels_sum / self.events,
                "single_frac": self.single_level / self.events,
            },
            "parent_gap": {
                "mean_s": mean_gap,
                "var_over_mean": var_gap / mean_gap,
                "cv2": var_gap / (mean_gap * mean_gap),
                "acf_lag1": acf[0],
                "acf_lag5": acf[4],
            },
            "subsecond_distinct_gap_mean_us": (
                self.subsecond_gap_sum_us / self.subsecond_distinct_gaps
                if self.subsecond_distinct_gaps else None
            ),
        }


def probe(path):
    primary = EventStats(True)
    timestamp = EventStats(False)
    rows = 0
    quote = 0.0
    first = None
    last = None
    second_counts = {}
    with zipfile.ZipFile(path) as archive:
        info = archive.getinfo(archive.namelist()[0])
        with archive.open(info) as stream:
            for row in csv.reader(io.TextIOWrapper(stream, newline="")):
                if not row or not row[0].lstrip("-").isdigit():
                    continue
                price = row[1]
                stamp = int(row[4])
                side = row[5].strip().lower() == "true"
                rows += 1
                quote += float(row[3])
                first = stamp if first is None else first
                last = stamp
                second = stamp // 1_000_000
                second_counts[second] = second_counts.get(second, 0) + 1
                primary.push(stamp, side, price)
                timestamp.push(stamp, side, price)
    span_seconds = (last // 1_000_000 - first // 1_000_000) + 1
    histogram = {}
    for value in second_counts.values():
        histogram[value] = histogram.get(value, 0) + 1
    missing = span_seconds - len(second_counts)
    histogram[0] = missing
    rank50 = math.ceil(0.50 * span_seconds)
    rank95 = math.ceil(0.95 * span_seconds)
    seen = 0
    median = p95 = 0
    for count in sorted(histogram):
        seen += histogram[count]
        if seen >= rank50 and median == 0:
            median = count
        if seen >= rank95:
            p95 = count
            break
    result = {
        "rows": rows,
        "bytes": info.file_size,
        "span_days": span_seconds / 86400.0,
        "raw_fills_per_second": rows / span_seconds,
        "mean_notional": quote / rows,
        "per_second_counts": {
            "mean": rows / span_seconds,
            "median": median,
            "p95": p95,
            "zero_frac": missing / span_seconds,
        },
        "timestamp_and_side": primary.report(),
        "timestamp_only": timestamp.report(),
    }
    print(path)
    print(f"rows {rows:,}, span {result['span_days']:.2f} days")
    for name in ("timestamp_and_side", "timestamp_only"):
        report = result[name]
        print(f"{name}: children {report['children']}, levels {report['levels']}")
        print(f"{name}: parent gap {report['parent_gap']}")
    print(f"raw fills/sec {result['per_second_counts']}")
    print(f"mean notional {result['mean_notional']:.6f}")
    return result


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: probe_binance_trades.py <archive.zip>")
    probe(sys.argv[1])
