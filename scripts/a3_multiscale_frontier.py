"""Does a two-component log-OU reach count moments a SINGLE tau cannot?

Panel 2 of the frozen adjudication, done properly. The successor hypothesis is
that splitting the latent Gaussian variance between a fast and a slow OU
component decouples minute-scale dispersion, which A1 needs, from hourly
dispersion, which A2 constrains - at a total sigma A3 admits.

That hypothesis is only interesting if the mixture escapes what a single
correlation time already achieves. Both endpoints of the mixture ARE single
components (w = 0 and w = 1), and those are already on the frozen grid, so the
question is precisely whether an INTERMEDIATE split reaches a (minute, hourly)
pair off the single-tau curve. The test: match a mixture to a single tau on
60 s Fano, then compare 3600 s Fano. Lower is better - the same minute-scale
dispersion at less hourly cost.

The variance uses the exact one-dimensional reduction rather than a double sum,

    Var(N_T) = E[N_T] + 2 rate^2 * integral_0^T (T - u) * Cov_X(u) du
    Cov_X(u) = exp(Cov_Y(u)) - 1

because the double-sum version needs a step far below the smallest tau and
silently biases short-tau rows when it does not get one - which it did not, at
dt = 18 s against tau = 5 s, in the first attempt at this test.

Exploratory. Section 11 forbids this reaching back into 12b.
"""

import math

SIGMA2 = 0.64  # the total sigma A3 admits, from panel 1
RATE = 6.5  # parents per second at a mid-activity well-supported hour


def fano(w, tau_fast, tau_slow, window_s, steps):
    step = window_s / steps
    acc = 0.0
    for i in range(steps):
        u = (i + 0.5) * step
        cov_y = (1.0 - w) * SIGMA2 * math.exp(-u / tau_fast) + w * SIGMA2 * math.exp(
            -u / tau_slow
        )
        acc += (window_s - u) * math.expm1(cov_y) * step
    mean = RATE * window_s
    return (mean + 2.0 * RATE * RATE * acc) / mean


SINGLE_TAUS = (2.0, 5.0, 10.0, 21.5, 46.4, 100.0, 215.0, 464.0, 1000.0)

print("single-component frontier (exact 1-D reduction)")
print(f"{'tau':>8} {'F(60s)':>10} {'F(3600s)':>11}")
frontier = []
for tau in SINGLE_TAUS:
    f60 = fano(1.0, tau, tau, 60.0, 60_000)
    f3600 = fano(1.0, tau, tau, 3600.0, 360_000)
    frontier.append((f60, f3600, tau))
    print(f"{tau:8.1f} {f60:10.2f} {f3600:11.2f}")

print()
print("two-component mixtures, each compared with the single tau matched on F(60s)")
print(f"{'fast':>6} {'slow':>7} {'w':>5} {'F(60s)':>9} {'F(3600s)':>10} {'single ref':>11} {'verdict':>8}")
for tau_fast, tau_slow, w in (
    (5.0, 30.0, 0.5),
    (5.0, 60.0, 0.5),
    (2.0, 100.0, 0.5),
    (10.0, 215.0, 0.4),
    (2.0, 46.4, 0.6),
    (5.0, 46.4, 0.3),
):
    f60 = fano(w, tau_fast, tau_slow, 60.0, 60_000)
    f3600 = fano(w, tau_fast, tau_slow, 3600.0, 360_000)
    lower = max((p for p in frontier if p[0] <= f60), key=lambda p: p[0], default=None)
    upper = min((p for p in frontier if p[0] >= f60), key=lambda p: p[0], default=None)
    if lower and upper and upper[0] > lower[0]:
        frac = (f60 - lower[0]) / (upper[0] - lower[0])
        reference = lower[1] + frac * (upper[1] - lower[1])
    else:
        reference = float("nan")
    verdict = (
        "BETTER"
        if f3600 < reference * 0.9
        else ("same" if f3600 < reference * 1.1 else "worse")
    )
    print(
        f"{tau_fast:6.1f} {tau_slow:7.1f} {w:5.2f} {f60:9.2f} {f3600:10.2f} "
        f"{reference:11.2f} {verdict:>8}"
    )

print()
print("BETTER anywhere means the mixture buys minute-scale dispersion more")
print("cheaply in hourly dispersion than any single correlation time, and the")
print("successor class has moment-level room. 'same' or 'worse' everywhere means")
print("it lies inside the single-tau frontier and buys nothing the frozen grid")
print("did not already offer.")
