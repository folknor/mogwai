"""Attainability check for the cross-paired long-tau conformance cells.

For each proposed long-tau replacement cell, simulate the conformance
statistic (hour-0 mean latent multiplier over a two-session exposure,
latent-only) at a 10 s grid, measure the M-replicate sample variance
across META meta-reps, and report the fraction failing the frozen
0.5-closed-form tolerance arm. The rejected corner failed this at 0.67;
an attainable cell should fail at (near) zero.

self_exciting (phi 0.10, tau 600) is argued analytically instead: it
draws no latent randomness, X - 1 = phi * u with u a decayed count
residual, so at phi 0.10 the statistic's variance is Poisson-dominated
and its sample variance concentrates like a chi-square with thousands
of degrees of freedom.
"""

import math
import random

DT = 10.0
N_STEPS = 360
SESSIONS = 2
META = 15
M = 800


def meta_run(label, init, step, var_x, tau):
    var_sum = 0.0
    for lag in range(N_STEPS):
        cov = var_x * math.exp(-lag * DT / tau)
        weight = N_STEPS if lag == 0 else 2.0 * (N_STEPS - lag)
        var_sum += weight * cov
    closed = var_sum / (N_STEPS * N_STEPS) / SESSIONS
    rng = random.Random(11)
    fails = 0
    svs = []
    for meta in range(META):
        stats = []
        for m in range(M):
            total = 0.0
            for s in range(SESSIONS):
                state = init(rng)
                acc = 0.0
                for t in range(N_STEPS):
                    if t:
                        state = step(state, rng)
                    acc += x_of(label, state)
                total += acc / N_STEPS
            stats.append(total / SESSIONS)
        mean = sum(stats) / M
        sv = sum((v - mean) ** 2 for v in stats) / (M - 1)
        svs.append(sv)
        if abs(sv - closed) > 0.5 * closed:
            fails += 1
    svs.sort()
    print(f"{label}: closed {closed:.5f}  sample median {svs[len(svs) // 2]:.5f}"
          f"  range [{svs[0]:.5f}, {svs[-1]:.5f}]  fail frac {fails / META:.2f}")


def x_of(label, state):
    if label.startswith("log_ou"):
        sigma = 0.2
        return math.exp(state - sigma * sigma / 2.0)
    if label.startswith("wall_mmpp"):
        # q 0.10, r 2: x_quiet = 1/(q + (1-q) r), x_active = r * x_quiet
        xq = 1.0 / (0.10 + 0.90 * 2.0)
        return xq if state else 2.0 * xq
    if label.startswith("shot_noise"):
        return 1.0 - 0.2 + state
    raise AssertionError(label)


# log_ou_cox (sigma_y 0.2, tau 3600)
def logou_init(rng):
    return 0.2 * rng.gauss(0.0, 1.0)


def logou_step(y, rng):
    a = math.exp(-DT / 3600.0)
    return a * y + 0.2 * math.sqrt(1.0 - a * a) * rng.gauss(0.0, 1.0)


# wall_mmpp (q 0.10, r 2, tau 3600): state True = quiet
def mmpp_init(rng):
    return rng.random() < 0.10


def mmpp_step(quiet, rng):
    flip = 1.0 - math.exp(-DT / 3600.0)
    u = rng.random()
    if quiet:
        return not (u < 0.90 * flip)
    return u < 0.10 * flip


# shot_noise (m 0.2, k 10, tau 3600): lambda = k / tau
def shot_init(rng):
    return rng.gammavariate(10.0, 0.02)


def shot_step(s, rng):
    d = math.exp(-DT / 3600.0)
    lam_dt = 10.0 / 3600.0 * DT
    # exact compound-Poisson OU increment
    n = 0
    threshold = math.exp(-lam_dt)
    p = 1.0
    while True:
        p *= rng.random()
        if p <= threshold:
            break
        n += 1
    j = 0.0
    for _ in range(n):
        u = rng.random() * DT
        e = rng.expovariate(1.0 / 0.02)
        j += e * math.exp(-(DT - u) / 3600.0)
    return d * s + j


mmpp_xq = 1.0 / (0.10 + 0.90 * 2.0)
mmpp_var = 0.10 * 0.90 * (2.0 * mmpp_xq - mmpp_xq) ** 2
meta_run("log_ou (0.2, 3600)", logou_init, logou_step,
         math.exp(0.2 * 0.2) - 1.0, 3600.0)
meta_run("wall_mmpp (0.10, 2, 3600)", mmpp_init, mmpp_step,
         mmpp_var, 3600.0)
meta_run("shot_noise (0.2, 10, 3600)", shot_init, shot_step,
         0.2 * 0.2 / 10.0, 3600.0)
