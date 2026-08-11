"""The frozen exploratory adjudication that closes the 12b postmortem.

TWO PANELS, deliberately separate so the second cannot influence the first.

PANEL 1 asks whether the all-hours A3 rejection survives uncertainty. If the
per-hour A3-admissible sigma bands, widened by observed and generated
uncertainty, share a value that A1 also supports, then the rejection is
substantially an estimator or finite-window problem. If they do not overlap,
real hour structure remains.

PANEL 2 asks whether a two-component log-OU has MOMENT-LEVEL room to satisfy
A1 without breaking A2, at a total sigma A3 admits. Independent Gaussian OU
components with variances (1-w) and w of the total give

    Cov(Y(0), Y(u)) = (1-w) s2 exp(-u/tau_fast) + w s2 exp(-u/tau_slow)
    Cov(X(0), X(u)) = exp(Cov(Y(0), Y(u))) - 1

and the Cox count variance over a window T with baseline b follows. Shifting w
toward the slow component raises minute-scale dispersion at fixed marginal - but
it also raises HOURLY dispersion, so the test is two-sided.

STOP CONDITION, fixed before running: a positive result is evidence to simulate
the full A1 histogram in a successor protocol, never a claim that the class
works. A1 is a histogram-SUPPORT gate and matching moments does not populate
tail bins. A negative result closes the postmortem without inventing another
mechanism from this artifact.

Exploratory. Section 11 forbids any of this reaching back into 12b.
"""

import json
import math

CAP = math.log(2.0)
GATED = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18, 19, 20, 22, 23]
WELL = [h for h in GATED if h not in (13, 17, 18, 19)]

screen = json.load(open("analysis/mnq-arrival-screen.json"))
measure = json.load(open("analysis/mnq-measure-12a.json"))

# ---------------------------------------------------------------- panel 1

# Observed uncertainty by resampling whole SESSIONS, never seconds: the
# within-session dependence is exactly what a per-second bootstrap would destroy.
sessions = measure["observed"]["per_session"]
per_session_zero = {h: [] for h in GATED}
for session in sessions:
    for hour in GATED:
        cell = session["block2"].get(str(hour), {}).get("1")
        if cell and cell["scheduled_windows"]:
            per_session_zero[hour].append(
                (cell["zero_windows"], cell["scheduled_windows"])
            )


def session_resampled_spread(hour, replicates=2000, seed=20260811):
    """Relative standard error of the pooled observed zero fraction, in log
    space, from a session-level circular resample."""
    rows = per_session_zero[hour]
    if len(rows) < 2:
        return None
    state = seed
    values = []
    n = len(rows)
    for _ in range(replicates):
        zeros = 0
        scheduled = 0
        for _ in range(n):
            state = (state * 6364136223846793005 + 1442695040888963407) % (2**64)
            pick = rows[(state >> 33) % n]
            zeros += pick[0]
            scheduled += pick[1]
        if zeros > 0 and scheduled > 0:
            values.append(math.log(zeros / scheduled))
    if len(values) < 2:
        return None
    mean = sum(values) / len(values)
    var = sum((v - mean) ** 2 for v in values) / (len(values) - 1)
    return math.sqrt(var)


def signed_per_seed(cell, hour):
    return [
        math.log(r["ratio"])
        for r in cell["a3"].get("per_seed_raw", [])
        if r.get("hour") == hour and r.get("ratio") not in (None, 0.0)
    ]


log_ou = [c for c in screen["cells"] if c["family"] == "log_ou_cox"]
a1_sigmas = sorted({c["params"]["sigma_y"] for c in log_ou if c["a1"]["passed"]})

print("== PANEL 1: does the A3 rejection survive uncertainty? ==\n")
print(f"{'hour':>4} {'obs_se':>8} {'gen_se':>8} {'A3-admissible sigma_y band':>34}")
bands = {}
for hour in GATED:
    obs_se = session_resampled_spread(hour)
    per_sigma = {}
    for cell in log_ou:
        values = signed_per_seed(cell, hour)
        if values:
            per_sigma.setdefault(cell["params"]["sigma_y"], []).extend(values)
    if not per_sigma:
        continue
    gen_se = 0.0
    for values in per_sigma.values():
        if len(values) > 1:
            m = sum(values) / len(values)
            gen_se = max(
                gen_se, math.sqrt(sum((v - m) ** 2 for v in values) / (len(values) - 1))
            )
    widen = CAP + 2.0 * math.sqrt((obs_se or 0.0) ** 2 + gen_se**2)
    admitted = sorted(
        s
        for s, values in per_sigma.items()
        if abs(sum(values) / len(values)) <= widen
    )
    bands[hour] = set(admitted)
    text = (
        f"{admitted[0]:.1f} to {admitted[-1]:.1f}" if admitted else "EMPTY"
    )
    print(f"{hour:>4} {(obs_se or float('nan')):8.4f} {gen_se:8.4f} {text:>34}")

common_all = set.intersection(*(bands[h] for h in GATED if h in bands)) if bands else set()
common_well = (
    set.intersection(*(bands[h] for h in WELL if h in bands)) if bands else set()
)
print()
print("sigma_y admitted by EVERY gated hour after uncertainty:", sorted(common_all) or "NONE")
print("sigma_y admitted by every WELL-SUPPORTED hour:        ", sorted(common_well) or "NONE")
print("sigma_y where A1 support passes:                      ", a1_sigmas or "NONE")
print("OVERLAP with A1 (all hours):     ", sorted(common_all & set(a1_sigmas)) or "NONE")
print("OVERLAP with A1 (well-supported):", sorted(common_well & set(a1_sigmas)) or "NONE")

# ---------------------------------------------------------------- panel 2

print("\n== PANEL 2: does a two-component log-OU have moment-level room? ==\n")


def cox_count_variance(total_sigma2, w, tau_fast, tau_slow, window_s, rate_per_s):
    """Var(N) over `window_s` for a Cox process whose latent multiplier is
    exp(Y - s2/2) with Y a two-component OU. Doubly-stochastic:
    Var(N) = E[N] + rate^2 * double integral of Cov(X_s, X_t)."""
    steps = 240
    dt = window_s / steps
    mean = rate_per_s * window_s
    acc = 0.0
    for i in range(steps):
        for j in range(steps):
            u = abs(i - j) * dt
            cov_y = (1.0 - w) * total_sigma2 * math.exp(-u / tau_fast) + (
                w
            ) * total_sigma2 * math.exp(-u / tau_slow)
            acc += (math.exp(cov_y) - 1.0) * dt * dt
    return mean + rate_per_s * rate_per_s * acc


def fano(total_sigma2, w, tau_fast, tau_slow, window_s, rate_per_s):
    var = cox_count_variance(total_sigma2, w, tau_fast, tau_slow, window_s, rate_per_s)
    return var / (rate_per_s * window_s)


TAU_FAST, TAU_SLOW = 5.0, 600.0
RATE = 6.5  # parents per second, a mid-activity well-supported hour
candidates = sorted(common_well) or sorted(common_all) or [0.8]
print(f"total sigma_y from panel 1 (A3-admitted): {candidates}")
print(f"tau_fast={TAU_FAST} s, tau_slow={TAU_SLOW} s, baseline {RATE} parents/s\n")
print(f"{'sigma':>6} {'w_slow':>7} {'Fano 1s':>9} {'Fano 60s':>10} {'Fano 300s':>11} {'Fano 3600s':>12}")
for sigma in candidates:
    s2 = sigma * sigma
    for w in (0.0, 0.25, 0.5, 0.75, 1.0):
        row = [
            fano(s2, w, TAU_FAST, TAU_SLOW, window, RATE)
            for window in (1.0, 60.0, 300.0, 3600.0)
        ]
        print(
            f"{sigma:6.1f} {w:7.2f} {row[0]:9.3f} {row[1]:10.2f} {row[2]:11.2f} {row[3]:12.2f}"
        )

print()
print("READ: rising w moves variance to the slow component. If Fano at 60 s can")
print("rise materially while Fano at 3600 s stays within the hourly A2 allowance,")
print("the class has moment-level room. If both move together, it does not, and")
print("the postmortem closes without proposing a successor mechanism.")
