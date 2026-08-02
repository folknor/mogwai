#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Streaming stylized-fact characterization of one Kraken pair file.

Phase 0: measure the stylized facts that define "looks real" in one streaming
pass with bounded memory. The estimands here are exactly the acceptance criteria
the synthetic generator must reproduce; characterize() is importable so a driver
can fan it across several pairs.

Memory discipline: the trade series is never materialized. Autocorrelations use a
fixed-length ring buffer plus running cross-sums (O(max_lag)); histograms are
bounded; the tick-increment counter is capped; the session profile is a 24x7
grid. The file is read in place; only a small JSON report is written.

Usage:
    python3 analysis/characterize.py [PAIR]      # default XBTUSD
    python3 analysis/characterize.py /abs/path/to/FILE.csv
"""

import json
import math
import os
import sys
import time

DATA_DIR = os.environ.get(
    "MOGWAI_DATA_DIR", "/home/folk/Kraken"
)
MAX_LAG = 50
LOG_DUR_BINS = 40
DWELL_ERA_START_TS = 1_546_300_800  # 2019-01-01T00:00:00Z
DWELL_LOG_BINS = 160
DWELL_LOG_LO_S = 1.0
DWELL_LOG_HI_S = 604_800.0
TICK_DICT_CAP = 500_000  # bound the distinct-increment counter
LVL_LOG_LO = 1e-6
LVL_LOG_HI = 1e6
LVL_PER_DEC = 10
LVL_BINS = 120


def lvl_bin(value):
    """Regular log bin for era-windowed level-visit volumes and sizes."""
    if value < LVL_LOG_LO:
        return 0
    bucket = int(math.floor(math.log10(value / LVL_LOG_LO) * LVL_PER_DEC))
    return min(LVL_BINS - 1, max(0, bucket))


def histogram_quantile(hist, q):
    """Geometric bin-centre quantile for the level histogram."""
    total = sum(hist)
    if total == 0:
        return None
    threshold = math.ceil(q * total)
    cumulative = 0
    for index, count in enumerate(hist):
        cumulative += count
        if cumulative >= threshold:
            return LVL_LOG_LO * 10 ** ((index + 0.5) / LVL_PER_DEC)
    raise AssertionError("histogram quantile did not find a bin")


class LevelVisits:
    """Streaming O(1) accumulator of at-touch traded volume per level visit.

    A level VISIT is a maximal run of consecutive prints at one price, and its
    summed size is how much the market traded at that price before the price
    moved off it. That estimand is AT-TOUCH TRADED VOLUME, not book depth and
    not a queue position: liquidity that was cancelled rather than traded is
    invisible so the number is deflated, liquidity that joined the level
    mid-visit is traded and counted so it is also inflated, and a trades-only
    corpus has no aggressor side so buy- and sell-initiated flow at one price
    are summed together.

    Both accumulators are windowed on the same modern era as the dwell
    statistics, so the visit volumes and the trade sizes that normalize them
    share one window and one binning. A visit is binned only if it OPENED in
    era, so a visit straddling the boundary is dropped entirely rather than
    contributing a truncated volume. The file is time-ordered, so a visit can
    cross the boundary at most once.
    """

    def __init__(self, era_start_ts=DWELL_ERA_START_TS):
        self.era_start_ts = era_start_ts
        self.px = None
        self.vol = 0.0
        self.n = 0
        self.open_ok = False
        self.vol_hist = [0] * LVL_BINS
        self.n_hist = [0] * 12  # 1,2,...,10, 11-20, 21+ prints per visit
        self.count = 0
        self.single = 0
        self.size_hist = [0] * LVL_BINS  # era-windowed sizes, the normalizer

    @staticmethod
    def n_bin(prints):
        if prints <= 10:
            return prints - 1
        return 10 if prints <= 20 else 11

    def push(self, ts, px, sz):
        if ts >= self.era_start_ts and sz > 0:
            self.size_hist[lvl_bin(sz)] += 1
        if px == self.px:
            self.vol += sz
            self.n += 1
        else:
            self.close()
            self.px = px
            self.vol = sz
            self.n = 1
            self.open_ok = ts >= self.era_start_ts

    def close(self):
        """Bin the open visit. Called on every price change and once at EOF."""
        if self.px is None or not self.open_ok:
            return
        self.vol_hist[lvl_bin(self.vol)] += 1
        self.n_hist[self.n_bin(self.n)] += 1
        self.count += 1
        self.single += int(self.n == 1)
        self.open_ok = False  # binned once, even if close() is called again

    def report(self):
        size_p50 = histogram_quantile(self.size_hist, 0.5)
        size_p90 = histogram_quantile(self.size_hist, 0.9)
        vol_p50 = histogram_quantile(self.vol_hist, 0.5)
        vol_p90 = histogram_quantile(self.vol_hist, 0.9)
        return {
            "era_start_ts": self.era_start_ts,
            "n_visits": self.count,
            "single_print_frac": self.single / self.count if self.count else None,
            "bin_lo": LVL_LOG_LO,
            "bin_hi": LVL_LOG_HI,
            "bins_per_decade": LVL_PER_DEC,
            "vol_hist": self.vol_hist,
            "n_hist": self.n_hist,
            "size_median": size_p50,
            "vol_p50_norm": vol_p50 / size_p50 if vol_p50 and size_p50 else None,
            "vol_p90_norm": vol_p90 / size_p50 if vol_p90 and size_p50 else None,
            "vol_dispersion": vol_p90 / vol_p50 if vol_p90 and vol_p50 else None,
            "size_dispersion": size_p90 / size_p50 if size_p90 and size_p50 else None,
        }


class AutoCorr:
    """Streaming autocorrelation up to max_lag via ring buffer + cross-sums."""

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
        self.cross[0] += x * x
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
            if m <= 0:
                out.append(0.0)
                continue
            cov = self.cross[d] / m - mean * mean
            out.append(cov / var)
        return out


def log_bin(value, lo, hi, nbins):
    if value <= lo:
        return 0
    if value >= hi:
        return nbins - 1
    frac = (math.log(value) - math.log(lo)) / (math.log(hi) - math.log(lo))
    return min(nbins - 1, int(frac * nbins))


def decimals_used(num_str):
    """Significant decimal places after stripping trailing zeros (round-lot tell)."""
    s = num_str.strip()
    if "." not in s:
        return 0
    frac = s.split(".", 1)[1].rstrip("0")
    return len(frac)


def dwell_stats(first_ts, last_ts, seen_hours):
    """Return complete-era-hour dwell statistics from occupied UTC hours."""
    if first_ts is None or last_ts is None:
        return 0.0, 0
    start_hour = math.ceil(max(first_ts, DWELL_ERA_START_TS) / 3600.0)
    end_hour = math.floor(last_ts / 3600.0) - 1
    if end_hour < start_hour:
        return 0.0, 0
    empty = 0
    longest = 0
    run = 0
    for hour in range(start_hour, end_hour + 1):
        if hour in seen_hours:
            run = 0
        else:
            empty += 1
            run += 1
            longest = max(longest, run)
    total = end_hour - start_hour + 1
    return empty / total, longest


def characterize(path):
    pair = os.path.splitext(os.path.basename(path))[0]

    n = 0
    first_ts = last_ts = None
    prev_ts = prev_px = None

    dur_sum = dur_sumsq = 0.0
    dur_n = 0
    dur_hist = [0] * LOG_DUR_BINS
    dwell_hist = [0] * DWELL_LOG_BINS
    dwell_n = 0
    dwell_sum = 0.0
    dwell_sumsq = 0.0
    dwell_max = 0.0
    dwell_seen_hours = set()

    ret_acf = AutoCorr(MAX_LAG)
    abs_acf = AutoCorr(MAX_LAG)
    dur_acf = AutoCorr(MAX_LAG)
    # Era-windowed duration ACF: same in-window gap population as the dwell
    # statistics, so the duration targets the gate reads are all judged over
    # the era the default profile actually claims. The full-span dur_acf is
    # kept for documentation and offline comparison.
    dwell_acf = AutoCorr(MAX_LAG)

    zero_change = 0
    change_n = 0
    tick_counts = {}  # quantized nonzero |dpx| -> count (bounded by TICK_DICT_CAP)
    tick_capped = False
    price_dec_hist = {}  # price-field decimal-place count -> frequency

    size_log_hist = [0] * 30
    size_dec_hist = [0] * 9  # size significant decimals 0..8+
    size_n = 0
    visits = LevelVisits()

    sess_count = [[0] * 7 for _ in range(24)]
    sess_sumsq_ret = [[0.0] * 7 for _ in range(24)]

    t0 = time.time()
    with open(path, "r", errors="replace") as f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            parts = line.split(",")
            if len(parts) < 3:
                continue
            try:
                ts = float(parts[0])
                px = float(parts[1])
                sz = float(parts[2])
            except ValueError:
                continue
            if px <= 0:
                continue
            n += 1
            if first_ts is None:
                first_ts = ts
            last_ts = ts

            if ts >= DWELL_ERA_START_TS:
                dwell_seen_hours.add(int(ts) // 3600)

            visits.push(ts, px, sz)

            tsec = int(ts)
            hour = (tsec // 3600) % 24
            dow = ((tsec // 86400) + 4) % 7  # Sun=0 (1970-01-01 Thu -> 4)

            pd = decimals_used(parts[1])
            price_dec_hist[pd] = price_dec_hist.get(pd, 0) + 1

            size_n += 1
            if sz > 0:
                size_log_hist[min(29, max(0, int(math.log10(sz)) + 9))] += 1
            size_dec_hist[min(8, decimals_used(parts[2]))] += 1

            if prev_ts is not None:
                dt = ts - prev_ts
                if dt >= 0:
                    dur_n += 1
                    dur_sum += dt
                    dur_sumsq += dt * dt
                    dur_hist[log_bin(max(dt, 1e-3), 1e-3, 86400.0, LOG_DUR_BINS)] += 1
                    dur_acf.push(dt)
                    # A gap belongs to the trade that closes it. Keeping only
                    # in-window closers makes the era boundary deterministic.
                    if ts >= DWELL_ERA_START_TS:
                        dwell_n += 1
                        dwell_sum += dt
                        dwell_sumsq += dt * dt
                        dwell_acf.push(dt)
                        dwell_max = max(dwell_max, dt)
                        dwell_hist[log_bin(
                            max(dt, DWELL_LOG_LO_S),
                            DWELL_LOG_LO_S,
                            DWELL_LOG_HI_S,
                            DWELL_LOG_BINS,
                        )] += 1

            if prev_px is not None:
                dpx = px - prev_px
                change_n += 1
                if dpx == 0:
                    zero_change += 1
                else:
                    q = round(abs(dpx), 8)
                    if q in tick_counts:
                        tick_counts[q] += 1
                    elif not tick_capped:
                        tick_counts[q] = 1
                        if len(tick_counts) >= TICK_DICT_CAP:
                            tick_capped = True
                ret = math.log(px) - math.log(prev_px)
                ret_acf.push(ret)
                abs_acf.push(abs(ret))
                sess_sumsq_ret[hour][dow] += ret * ret
            sess_count[hour][dow] += 1

            prev_ts = ts
            prev_px = px

    visits.close()

    # modal tick + low percentiles of the nonzero increment
    modal_tick = tick_p10 = tick_p50 = None
    if tick_counts:
        modal_tick = max(tick_counts.items(), key=lambda kv: kv[1])[0]
        items = sorted(tick_counts.items())
        tot = sum(c for _v, c in items)
        cum = 0
        for v, c in items:
            cum += c
            if tick_p10 is None and cum >= 0.10 * tot:
                tick_p10 = v
            if cum >= 0.50 * tot:
                tick_p50 = v
                break
    price_dec_mode = (
        max(price_dec_hist.items(), key=lambda kv: kv[1])[0]
        if price_dec_hist
        else None
    )

    dur_mean = dur_sum / dur_n if dur_n else 0.0
    dur_var = (dur_sumsq / dur_n - dur_mean**2) if dur_n else 0.0
    dwell_mean = dwell_sum / dwell_n if dwell_n else 0.0
    dwell_var = (dwell_sumsq / dwell_n - dwell_mean**2) if dwell_n else 0.0
    empty_hour_frac, max_empty_hour_run_h = dwell_stats(
        first_ts, last_ts, dwell_seen_hours
    )
    dwell_p999_s = None
    if dwell_n:
        threshold = math.ceil(0.999 * dwell_n)
        cumulative = 0
        for index, count in enumerate(dwell_hist):
            cumulative += count
            if cumulative >= threshold:
                if index == DWELL_LOG_BINS - 1:
                    raise ValueError("dwell p999 landed in the saturated bin")
                dwell_p999_s = math.exp(
                    math.log(DWELL_LOG_LO_S)
                    + (index + 1)
                    * (math.log(DWELL_LOG_HI_S) - math.log(DWELL_LOG_LO_S))
                    / DWELL_LOG_BINS
                )
                break

    return {
        "pair": pair,
        "path": path,
        "n_trades": n,
        "first_ts": first_ts,
        "last_ts": last_ts,
        "span_days": round((last_ts - first_ts) / 86400, 1)
        if first_ts and last_ts
        else None,
        "duration": {
            "mean_s": dur_mean,
            "var_s2": dur_var,
            "dispersion_index": (dur_var / dur_mean) if dur_mean else None,
            "log_hist": dur_hist,
            "acf": dur_acf.acf(),
            "dwell": {
                "era_start_ts": DWELL_ERA_START_TS,
                "n_gaps": dwell_n,
                "mean_s": dwell_mean,
                "var_s2": dwell_var,
                "dispersion_index": (
                    (dwell_var / dwell_mean) if dwell_mean else None
                ),
                "acf": dwell_acf.acf(),
                "max_gap_s": dwell_max,
                "gap_p999_s": dwell_p999_s,
                "dwell_hist": dwell_hist,
                "empty_hour_frac": empty_hour_frac,
                "max_empty_hour_run_h": max_empty_hour_run_h,
            },
        },
        "returns": {
            "acf": ret_acf.acf(),
            "abs_acf": abs_acf.acf(),
            "zero_change_frac": (zero_change / change_n) if change_n else None,
            "modal_tick": modal_tick,
            "tick_p10": tick_p10,
            "tick_p50": tick_p50,
            "tick_dict_capped": tick_capped,
            "price_decimals_mode": price_dec_mode,
        },
        "size": {
            "log10_hist": size_log_hist,
            "decimals_used_hist": size_dec_hist,
            "round_frac": (
                sum(size_dec_hist[:3]) / size_n if size_n else None
            ),  # <=2 decimals
        },
        "level": visits.report(),
        "session": {
            "count_hour_dow": sess_count,
            "sumsq_ret_hour_dow": sess_sumsq_ret,
        },
        "elapsed_s": round(time.time() - t0, 1),
    }


def resolve_path(arg):
    if arg is None:
        return os.path.join(DATA_DIR, "XBTUSD.csv")
    if os.path.sep in arg or arg.endswith(".csv"):
        return arg if os.path.isabs(arg) else os.path.join(DATA_DIR, arg)
    return os.path.join(DATA_DIR, arg + ".csv")


def main():
    path = resolve_path(sys.argv[1] if len(sys.argv) > 1 else None)
    if not os.path.isfile(path):
        print(f"file not found: {path}", file=sys.stderr)
        return 1
    rep = characterize(path)
    out = os.path.join(os.path.dirname(__file__), f"char_{rep['pair']}.json")
    with open(out, "w") as f:
        json.dump(rep, f)

    d, r = rep["duration"], rep["returns"]
    print(f"pair={rep['pair']}  trades={rep['n_trades']:,}  "
          f"span={rep['span_days']}d  elapsed={rep['elapsed_s']}s")
    print(f"duration: mean={d['mean_s']:.3f}s  dispersion_index="
          f"{d['dispersion_index']:.1f}")
    dwell = d["dwell"]
    print(f"era duration: mean={dwell['mean_s']:.3f}s  dispersion_index="
          f"{dwell['dispersion_index']:.1f}  "
          f"acf1-5: {[round(x, 3) for x in dwell['acf'][:5]]}")
    print(f"dwell: max_gap={dwell['max_gap_s']:.3f}s  p999="
          f"{dwell['gap_p999_s']:.3f}s  empty_hours="
          f"{dwell['empty_hour_frac']:.4f}  max_empty_run="
          f"{dwell['max_empty_hour_run_h']}h")
    print(f"returns: zero-change={r['zero_change_frac']:.3f}  "
          f"modal_tick={r['modal_tick']}  price_decimals={r['price_decimals_mode']}")
    print(f"  ret  acf1-5: {[round(x, 3) for x in r['acf'][:5]]}")
    print(f"  |ret| acf1-5: {[round(x, 3) for x in r['abs_acf'][:5]]}")
    print(f"size: round_frac={rep['size']['round_frac']:.3f}  "
          f"dec_hist={rep['size']['decimals_used_hist']}")
    print(f"report: {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
