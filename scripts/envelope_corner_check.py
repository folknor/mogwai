"""Adjudicate the conformance failure at log-OU (sigma_y 2.0, tau 3600).

Question: over M replicate two-session exposures, is a sample variance
far below the closed form a TYPICAL draw (statistical inevitability of
the corner) or an outlier (machinery defect)?

Pure python: X = exp(Y - sigma^2/2), Y exact OU on a 10 s grid (valid
since tau = 3600 s >> 10 s; the closed form is recomputed on the SAME
grid, so the comparison is self-consistent). Statistic = mean of X over
hour 0 across two independent sessions. The M=2000/1s-grid production
test is approximated at M = 800 / 10 s; the question is the SHAPE of the
sample-variance distribution, which both settings share.
"""

import math
import random

rng = random.Random(7)
sigma = 2.0
tau = 3600.0
dt = 10.0
n_steps = 360           # one hour at 10 s
sessions = 2
a = math.exp(-dt / tau)
b = sigma * math.sqrt(1.0 - a * a)

META = 15
M = 800

# closed-form Var of the per-exposure hourly mean of X (latent only)
var_sum = 0.0
for lag in range(n_steps):
    rho = math.exp(-lag * dt / tau)
    cov = math.exp(sigma * sigma * rho) - 1.0
    weight = n_steps if lag == 0 else 2.0 * (n_steps - lag)
    var_sum += weight * cov
var_stat = var_sum / (n_steps * n_steps) / sessions
print(f"closed-form Var(statistic) latent-only, 10 s grid: {var_stat:.4f}")

half = sigma * sigma / 2.0
sample_vars = []
for meta in range(META):
    stats = []
    for m in range(M):
        total = 0.0
        for s in range(sessions):
            y = sigma * rng.gauss(0.0, 1.0)
            acc = math.exp(y - half)
            for t in range(1, n_steps):
                y = a * y + b * rng.gauss(0.0, 1.0)
                acc += math.exp(y - half)
            total += acc / n_steps
        stats.append(total / sessions)
    mean = sum(stats) / M
    sv = sum((x - mean) ** 2 for x in stats) / (M - 1)
    sample_vars.append(sv)

sample_vars.sort()
n = len(sample_vars)
print(f"sample variance over M={M}, {META} meta-reps:")
print("  " + "  ".join(f"{v:.3f}" for v in sample_vars))
print(f"  median {sample_vars[n // 2]:.3f}  mean {sum(sample_vars) / n:.3f}")
below = sum(1 for v in sample_vars if abs(v - var_stat) > 0.5 * var_stat)
print(f"fraction failing the 0.5*closed arm: {below / n:.2f}")
