"""Is the between-session variance a DAY EFFECT, or slow within-session state?

The count-curve measurement found the incumbent short of observed
between-session Fano by 25 to 50 times, and at hour 19 that component is 75
percent of the total long-horizon dispersion. But `F_between` is the variance of
ESTIMATED session means, and it contains genuine day-to-day heterogeneity,
sampling variation, latent persistence comparable to the one-hour stratum, and
cross-window covariance within the hour. A stationary process with no day factor
can produce a large one. So the source has to be established before any session
multiplier is preregistered.

THE DISCRIMINATING SIGNATURE. A day multiplier `D_s` acts on every hour of a
session at once, so after removing the per-hour level the residuals should show
broad SAME-SIGN coherence across hours and an approximately LOW-RANK covariance
structure - one factor explaining most of the variance, with same-sign loadings.
Slow within-session state instead produces coherence that decays with hour
separation and no dominant single factor.

Reads only the committed 12a artifact. Exploratory: nothing here may reopen 12b,
and any successor criterion is preregistered separately.
"""

import json
import math

d = json.load(open("analysis/mnq-measure-12a.json"))
sessions = d["observed"]["per_session"]

# rate[s][h] = parents per scheduled second at hour h of session s, from the
# 1 s count cell: mean is parents per 1 s window, which IS the rate.
rows = []
for session in sessions:
    entry = {}
    for hour, windows in session["block2"].items():
        cell = windows.get("1")
        if cell and cell["scheduled_windows"] > 0 and cell["mean"] > 0:
            entry[int(hour)] = cell["mean"]
    if entry:
        rows.append((session["session_date"], entry))

hours = sorted(set(h for _, e in rows for h in e))
hours = [h for h in hours if sum(1 for _, e in rows if h in e) == len(rows)]
print(f"{len(rows)} sessions, {len(hours)} hours present in every session")

# Residual after removing the per-hour level: r[s][h] = log(rate) - mean_s log(rate)
level = {
    h: sum(math.log(e[h]) for _, e in rows) / len(rows) for h in hours
}
resid = [[math.log(e[h]) - level[h] for h in hours] for _, e in rows]

# Per-session mean residual: the candidate day factor.
day = [sum(r) / len(r) for r in resid]
print()
print("per-session mean log residual (the candidate day factor):")
for (date, _), value in zip(rows, day):
    bar = "#" * int(abs(value) * 60)
    print(f"  {date}  {value:+.4f} {'' if value >= 0 else '-'}{bar}")

spread = math.sqrt(sum(v * v for v in day) / (len(day) - 1))
print(f"\nstandard deviation of the day factor: {spread:.4f} in log space")
print(f"which is a {math.exp(spread) - 1:+.1%} typical day-to-day rate swing")

# How much of the total residual variance does that one number explain?
total = sum(v * v for r in resid for v in r)
explained = sum(len(hours) * v * v for v in day)
print(f"\nshare of residual variance explained by a single per-session shift: "
      f"{explained / total:.1%}")

# Same-sign coherence: within a session, do the hours move together?
same_sign = 0
pairs = 0
for r in resid:
    for i in range(len(r)):
        for j in range(i + 1, len(r)):
            pairs += 1
            if r[i] * r[j] > 0:
                same_sign += 1
print(f"same-sign hour pairs within a session: {same_sign / pairs:.1%} "
      f"(50% would be no coherence)")

# Leading eigenvalue share of the across-hour correlation matrix, by power
# iteration. A day multiplier predicts one dominant same-sign factor.
n = len(hours)
cov = [[0.0] * n for _ in range(n)]
for r in resid:
    for i in range(n):
        for j in range(n):
            cov[i][j] += r[i] * r[j]
for i in range(n):
    for j in range(n):
        cov[i][j] /= len(resid) - 1
sd = [math.sqrt(cov[i][i]) for i in range(n)]
corr = [[cov[i][j] / (sd[i] * sd[j]) for j in range(n)] for i in range(n)]

vec = [1.0] * n
for _ in range(500):
    nxt = [sum(corr[i][j] * vec[j] for j in range(n)) for i in range(n)]
    norm = math.sqrt(sum(v * v for v in nxt))
    vec = [v / norm for v in nxt]
lead = sum(vec[i] * sum(corr[i][j] * vec[j] for j in range(n)) for i in range(n))
print(f"\nleading eigenvalue of the across-hour correlation matrix: {lead:.2f} of {n}")
print(f"share of correlation structure in one factor: {lead / n:.1%}")
positive = sum(1 for v in vec if v > 0)
print(f"loadings with the same sign: {max(positive, n - positive)} of {n}")
print()
print("READ: a day multiplier predicts a high single-factor share with")
print("nearly all loadings same-sign. Slow within-session state predicts")
print("coherence decaying with hour separation and no dominant factor.")
