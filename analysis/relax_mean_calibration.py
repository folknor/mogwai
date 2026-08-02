# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only
"""Adjudication sim for the D2 wall-time relaxation's Jensen mean drift.

Simulates the raw ACD recursion (no session envelope, no price process) in
three variants:

  baseline   psi = omega + alpha*d + beta*psi                     (today)
  d2         psi = m + w*(omega + alpha*d + beta*psi - m)         (spec D2)
  d2cal      same recursion run at an internal mean c = CAL * m: the
             intercept omega and the relaxation attractor scale together,
             so the whole psi process is lifted ~linearly (option 1).
             A pure attractor shift (omega fixed) was tried first and
             saturates: w ~ 0.999 in the ~7 s bulk, so the shift cannot
             recover the tail mass the relaxation removed.

with w = exp(-d_prev / tau). Reports realized mean, dispersion index
(var/mean of durations) and duration ACF lag1/lag5, plus a bisection on CAL
so the calibrated realized mean returns to mean_s. Targets for context:
windowed dispersion band [36.3 .. 1627.9], windowed ACF lag1 anchor 0.1603
(tol 0.14), mean gate abs(mean - 7.19) <= 10%. Pure stdlib, matching the
rest of analysis/.
"""

import math
import random
import sys

MEAN_S = 7.19
PHI = 0.9935
SHARE = 0.08
SHAPE = 0.60
N = 1_000_000
EPS_MEAN = math.gamma(1.0 + 1.0 / SHAPE)


def simulate(tau, cal, seed=42, n=N, phi=PHI, share=SHARE):
    rng = random.Random(seed)
    alpha = phi * share
    beta = phi - alpha
    c = cal * MEAN_S
    omega = c * (1.0 - phi)
    psi = c
    prev = c
    out = [0.0] * n
    weib = rng.weibullvariate
    exp = math.exp
    for i in range(n):
        raw = omega + alpha * prev + beta * psi
        if tau is None:
            psi = raw
        else:
            psi = c + exp(-prev / tau) * (raw - c)
        d = psi * (weib(1.0, SHAPE) / EPS_MEAN)
        if d < 1e-9:
            d = 1e-9
        prev = d
        out[i] = d
    return out


def stats(d):
    n = len(d)
    mean = sum(d) / n
    x = [v - mean for v in d]
    denom = sum(v * v for v in x)
    disp = denom / n / mean
    acf1 = sum(x[i] * x[i - 1] for i in range(1, n)) / denom
    acf5 = sum(x[i] * x[i - 5] for i in range(5, n)) / denom
    return mean, disp, acf1, acf5


def report(label, d):
    mean, disp, acf1, acf5 = stats(d)
    print(
        f"{label:30s} mean={mean:7.3f}  disp={disp:9.2f}  "
        f"acf1={acf1:6.4f}  acf5={acf5:6.4f}",
        flush=True,
    )
    return mean


def calibrate(tau, lo=1.0, hi=1.8, iters=10):
    for _ in range(iters):
        mid = 0.5 * (lo + hi)
        m = sum(simulate(tau, mid)) / N
        if m < MEAN_S:
            lo = mid
        else:
            hi = mid
    return 0.5 * (lo + hi)


def main():
    taus = [7200.0, 3600.0, 1800.0, 900.0]
    if len(sys.argv) > 1:
        taus = [float(a) for a in sys.argv[1:]]
    report("baseline (no relax)", simulate(None, 1.0))
    for tau in taus:
        report(f"d2 tau={tau:.0f} cal=1.0", simulate(tau, 1.0))
        cal = calibrate(tau)
        for seed in (42, 7, 1337):
            report(f"d2cal tau={tau:.0f} cal={cal:.4f} s{seed}",
                   simulate(tau, cal, seed=seed))


if __name__ == "__main__":
    main()
