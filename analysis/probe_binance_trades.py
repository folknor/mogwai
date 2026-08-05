#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Stream a Binance raw-trades archive and infer parent match events.

The archive has no event identifier. Results are reported for two inference
rules: consecutive rows with the same timestamp and taker side, which is the
primary rule, and consecutive rows with the same timestamp only, matching the
aggTrades probe. Memory is bounded by histograms and the current event.
"""

import math
import sys
import zipfile

try:
    from analysis.probe_binance_aggtrades import AutoCorr
    from analysis.preflight import _byte_lines
except ModuleNotFoundError:
    from probe_binance_aggtrades import AutoCorr
    from preflight import _byte_lines


class EventStats:
    __slots__ = (
        "with_side", "key_stamp", "key_side", "count", "prices", "events",
        "single", "single_level", "children_sum", "children_hist",
        "children_max", "levels_sum", "prev_time", "gaps", "gap_sum",
        "gap_sumsq", "gap_acf", "subsecond_distinct_gaps",
        "subsecond_gap_sum_us",
    )

    def __init__(self, with_side):
        self.with_side = with_side
        # The group key is held as two scalars rather than a (stamp, side)
        # tuple, so the hot path allocates nothing per row. A None stamp marks
        # "no open event"; real stamps are never None.
        self.key_stamp = None
        self.key_side = None
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
        if self.key_stamp is None:
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
            gap_us = self.key_stamp - self.prev_time
            gap = gap_us / 1_000_000.0
            self.gaps += 1
            self.gap_sum += gap
            self.gap_sumsq += gap * gap
            self.gap_acf.push(gap)
            if 0 < gap_us < 1_000_000:
                self.subsecond_distinct_gaps += 1
                self.subsecond_gap_sum_us += gap_us
        self.prev_time = self.key_stamp

    def push(self, stamp, side, price):
        if stamp != self.key_stamp or (self.with_side and side != self.key_side):
            self._close()
            self.key_stamp = stamp
            self.key_side = side
            self.count = 0
            # _close has already read len(self.prices), so the set is reused
            # rather than reallocated once per event (~15M events a month).
            self.prices.clear()
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
    primary_push = primary.push
    timestamp_push = timestamp.push
    with zipfile.ZipFile(path) as archive:
        info = archive.infolist()[0]
        with archive.open(info) as stream:
            # Raw byte lines, no TextIOWrapper decode and no csv state machine:
            # every field is plain ASCII and the layout carries no quoting. The
            # price stays raw bytes - both accumulators take it as an opaque
            # set key, so only the distinct count matters.
            for line in _byte_lines(stream):
                if line.endswith(b"\r"):
                    line = line[:-1]
                if not line:
                    continue
                # Full split: the real dumps carry a seventh trailing column
                # (is_best_match), so a maxsplit of 5 would fold it into the
                # side field.
                row = line.split(b",")
                if not row[0].lstrip(b"-").isdigit():
                    continue
                price = row[1]
                stamp = int(row[4])
                side_text = row[5]
                side = side_text == b"True" or side_text == b"true"
                rows += 1
                quote += float(row[3])
                if first is None:
                    first = stamp
                last = stamp
                second = stamp // 1_000_000
                second_counts[second] = second_counts.get(second, 0) + 1
                primary_push(stamp, side, price)
                timestamp_push(stamp, side, price)
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
