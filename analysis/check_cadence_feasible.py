#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Read the measured cadence and issue the specification's L0 verdict.

The full parent-clock simulation is intentionally kept separate from the
corpus probe. This first validates the structural proceed threshold and reports
the measured density targets used by the simulator implementation.
"""

import argparse
import json
import math
import random
import statistics
from pathlib import Path

PATH = Path(__file__).with_name("cadence.json")
FINGERPRINT = Path(__file__).with_name("fingerprint.json")
EVENTS = 3_000_000


def next_count(uniform, q, m, cap=4096):
    if uniform() < q:
        return 1, False
    value = 1 + math.floor(math.log1p(-uniform()) / math.log1p(-1.0 / m))
    return min(cap, value), value > cap


def simulate(data, events=EVENTS, seed=42, *, persistence=0.9935,
             feedback=0.08, weibull_shape=0.60, relax_mean_cal=1.2293):
    rng = random.Random(seed)
    target = data["targets"]
    mean = target["mean_event_duration_s"]["anchor"]
    shape = data["shape"]
    fp = json.loads(FINGERPRINT.read_text())
    session = fp["session_profile"]
    alpha = persistence * feedback
    beta = persistence - alpha
    calibrated = relax_mean_cal * mean
    omega = calibrated * (1.0 - persistence)
    psi = calibrated
    previous = calibrated
    weibull_mean = math.gamma(1.0 + 1.0 / weibull_shape)
    clock = 0.0
    buckets = {}
    truncated = 0
    for _ in range(events):
        raw_eps = rng.weibullvariate(1.0, weibull_shape)
        eps = max(raw_eps / weibull_mean, float.fromhex("0x0.0000000000001p-1022"))
        unrelaxed = omega + alpha * previous + beta * psi
        relax = math.exp(-previous / 7200.0)
        psi = calibrated + relax * (unrelaxed - calibrated)
        duration = max(psi * eps, 1e-9)
        previous = duration
        whole = int(clock)
        hour = (whole % 86400) // 3600
        dow = ((whole // 86400) + 4) % 7
        arrival = session["intensity_hour"][hour] * 24.0
        arrival *= session["dow_weight"][dow] * 7.0
        clock += duration / arrival
        count, clipped = next_count(rng.random, shape["q"], shape["m"])
        truncated += clipped
        second = int(clock)
        buckets[second] = buckets.get(second, 0) + count
    span = int(clock) + 1
    histogram = {0: span - len(buckets)}
    for count in buckets.values():
        histogram[count] = histogram.get(count, 0) + 1
    def quantile(fraction):
        rank = math.ceil(fraction * span)
        seen = 0
        for count in sorted(histogram):
            seen += histogram[count]
            if seen >= rank:
                return count
        raise AssertionError("quantile rank is covered")
    total = sum(count * frequency for count, frequency in histogram.items())
    return {
        "mean": total / span,
        "median": quantile(0.5),
        "p95": quantile(0.95),
        "zero_frac": histogram[0] / span,
        "truncation_frac": truncated / events,
    }


def fit(data, events, candidates, seed=42):
    """Evaluate deterministic candidate tuples against the unmodified gate."""
    measured = data["targets"]["per_second_counts"]
    rows = []
    for persistence, feedback, shape, calibration in candidates:
        realized = simulate(
            data,
            events=events,
            seed=seed,
            persistence=persistence,
            feedback=feedback,
            weibull_shape=shape,
            relax_mean_cal=calibration,
        )
        score = (
            abs(realized["mean"] / measured["mean"] - 1.0)
            + abs(realized["median"] - measured["median"]) / measured["median"]
            + abs(realized["p95"] / measured["p95"] - 1.0)
            + abs(realized["zero_frac"] - measured["zero_frac"]) / 0.05
        )
        rows.append((score, density_passes(measured, realized),
                     (persistence, feedback, shape, calibration), realized))
    return sorted(rows, key=lambda row: row[0])


def candidate_grid():
    # Wide enough to test whether the ACD family, rather than its old Kraken
    # fit, can represent the raw-trade density. Kept explicit for provenance.
    for persistence in (0.90, 0.95, 0.98, 0.9935, 0.998):
        for feedback in (0.02, 0.08, 0.25, 0.50, 0.80):
            for shape in (0.30, 0.40, 0.50, 0.60, 0.80):
                for calibration in (0.75, 1.00, 1.25, 1.50):
                    yield persistence, feedback, shape, calibration


def simulate_markov(data, events, seed, quiet_fraction, state_persistence,
                    quiet_active_ratio, weibull_shape, calibration=1.0):
    rng = random.Random(seed)
    target = data["targets"]
    mean = target["mean_event_duration_s"]["anchor"] * calibration
    shape = data["shape"]
    fp = json.loads(FINGERPRINT.read_text())
    session = fp["session_profile"]
    active_mean = mean / ((1.0 - quiet_fraction) + quiet_fraction * quiet_active_ratio)
    quiet_mean = active_mean * quiet_active_ratio
    q_to_a = (1.0 - state_persistence) * (1.0 - quiet_fraction)
    a_to_q = (1.0 - state_persistence) * quiet_fraction
    quiet = rng.random() < quiet_fraction
    quiet_children_mean = target["children_mean"]["anchor"] * 0.20
    active_children_mean = target["children_mean"]["anchor"] * 1.4307692307692308
    quiet_q, quiet_m = 0.0, quiet_children_mean
    quiet_single = 1.0 / quiet_children_mean
    active_single = ((target["children_single_frac"]["anchor"]
                      - quiet_fraction * quiet_single) / (1.0 - quiet_fraction))
    active_m = (active_children_mean - 1.0) / (1.0 - active_single)
    active_q = 1.0 - (active_children_mean - 1.0) / (active_m - 1.0)
    innovation_mean = math.gamma(1.0 + 1.0 / weibull_shape)
    clock = 0.0
    buckets = {}
    gaps = []
    truncated = 0
    for _ in range(events):
        gap_mean = quiet_mean if quiet else active_mean
        gap = gap_mean * rng.weibullvariate(1.0, weibull_shape) / innovation_mean
        whole = int(clock)
        hour = (whole % 86400) // 3600
        dow = ((whole // 86400) + 4) % 7
        arrival = session["intensity_hour"][hour] * 24.0
        arrival *= session["dow_weight"][dow] * 7.0
        gap /= arrival
        clock += gap
        gaps.append(gap)
        count, clipped = next_count(
            rng.random,
            quiet_q if quiet else active_q,
            quiet_m if quiet else active_m,
        )
        truncated += clipped
        second = int(clock)
        buckets[second] = buckets.get(second, 0) + count
        if quiet:
            quiet = rng.random() >= q_to_a
        else:
            quiet = rng.random() < a_to_q
    span = int(clock) + 1
    histogram = {0: span - len(buckets)}
    for count in buckets.values():
        histogram[count] = histogram.get(count, 0) + 1
    def quantile(fraction):
        rank = math.ceil(fraction * span)
        seen = 0
        for count in sorted(histogram):
            seen += histogram[count]
            if seen >= rank:
                return count
        raise AssertionError
    total = sum(count * frequency for count, frequency in histogram.items())
    gap_mean = statistics.fmean(gaps)
    gap_var = statistics.pvariance(gaps)
    def acf(lag):
        numerator = sum((a - gap_mean) * (b - gap_mean)
                        for a, b in zip(gaps, gaps[lag:]))
        denominator = sum((value - gap_mean) ** 2 for value in gaps)
        return numerator / denominator
    return {
        "mean": total / span, "median": quantile(0.5), "p95": quantile(0.95),
        "zero_frac": histogram[0] / span, "truncation_frac": truncated / events,
        "gap_mean": gap_mean, "gap_cv2": gap_var / gap_mean ** 2,
        "gap_acf1": acf(1), "gap_acf5": acf(5),
    }


def markov_grid():
    for quiet_fraction in (0.32, 0.36, 0.40, 0.44, 0.48):
        for persistence in (0.82, 0.86, 0.89, 0.92, 0.95):
            for ratio in (100.0, 200.0, 400.0, 800.0):
                for shape in (0.8, 1.0, 1.2, 1.4):
                    for calibration in (0.9, 1.05, 1.2, 1.35):
                        yield quiet_fraction, persistence, ratio, shape, calibration


def fit_markov(data, events):
    measured = data["targets"]["per_second_counts"]
    cadence = data["targets"]
    rows = []
    for constants in markov_grid():
        realized = simulate_markov(data, events, 42, *constants)
        score = (abs(realized["mean"] / measured["mean"] - 1.0)
                 + abs(realized["median"] - measured["median"]) / measured["median"]
                 + abs(realized["p95"] / measured["p95"] - 1.0)
                 + abs(realized["zero_frac"] - measured["zero_frac"]) / 0.05
                 + abs(realized["gap_cv2"] / cadence["duration_dispersion_cv2"]["anchor"] - 1.0)
                 + abs(realized["gap_acf1"] - cadence["duration_acf_lag1"]["anchor"])
                 + abs(realized["gap_acf5"] - cadence["duration_acf_lag5"]["anchor"]))
        rows.append((score, density_passes(measured, realized), constants, realized))
    return sorted(rows, key=lambda row: row[0])


def density_passes(measured, realized):
    return (
        abs(realized["mean"] / measured["mean"] - 1.0) <= 0.10
        and measured["median"] - 1 <= realized["median"] <= measured["median"] + 1
        and 0.5 * measured["p95"] <= realized["p95"] <= 2.0 * measured["p95"]
        and abs(realized["zero_frac"] - measured["zero_frac"]) <= 0.05
    )


def verdict(data):
    mean = data["targets"]["children_mean"]["anchor"]
    single = data["targets"]["children_single_frac"]["anchor"]
    if 3.0 <= mean <= 20.0 and single < 0.90:
        return "PROCEED"
    if mean < 1.5:
        return "CLOSE"
    return "STOP AND ASK"


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--fit", action="store_true")
    parser.add_argument("--fit-markov", action="store_true")
    parser.add_argument("--events", type=int, default=EVENTS)
    parser.add_argument("--top", type=int, default=20)
    args = parser.parse_args()
    data = json.loads(PATH.read_text())
    result = verdict(data)
    print(f"parent/child verdict: {result}")
    measured = data["targets"]["per_second_counts"]
    print(f"measured density: {measured}")
    if result != "PROCEED":
        raise SystemExit(result)
    if args.fit:
        rows = fit(data, args.events, candidate_grid())
        for score, passed, constants, realized in rows[:args.top]:
            print(json.dumps({"score": score, "passes": passed,
                              "constants": constants, "density": realized},
                             sort_keys=True))
        raise SystemExit(0 if any(row[1] for row in rows) else 1)
    if args.fit_markov:
        rows = fit_markov(data, args.events)
        for score, passed, constants, realized in rows[:args.top]:
            print(json.dumps({"score": score, "passes": passed,
                              "constants": constants, "density": realized}, sort_keys=True))
        raise SystemExit(0 if any(row[1] for row in rows) else 1)
    realized = simulate_markov(data, args.events, 42, 0.35, 0.90, 150.0, 1.0, 0.944)
    print(f"simulated density: {realized}")
    if not density_passes(measured, realized):
        raise SystemExit("STOP: the specified ACD clock fails the density feasibility bands")
