"""How much of the session-hour residual variance is just estimation error?

This exists because the first attempt at it was wrong by an order of magnitude
and was quoted from a throwaway command with no evidence record - both defects
worth not repeating.

THE ERROR. A session-hour mean rate is estimated from about 3,600 one-second
windows, and its sampling variance is NOT `Fano_1s * rate / n`. Counts are
serially correlated, so the uncertainty of the mean carries the INTEGRATED
covariance over the whole hour, which is what `Fano(T)` measures at `T` near the
hour. Using the one-second Fano assumes the correlation dies inside a second -
the exact assumption the count-curve measurement refuted.

Correct form, for a window of `T` seconds aggregated over `n_T` windows:

    Var(rate_hat) = Fano(T) * rate / (n_T * T)
    relative SD   = sqrt( Fano(T) / (rate * n_T * T) )

The largest measured horizon is 300 s, so `Fano_within(300)` gives a LOWER
BOUND on the hour-scale value: the curve is still climbing at 300 s, so the true
floor is higher than anything computable here. Establishing it exactly needs
contiguous within-session count sequences, which the pooled histograms cannot
reconstruct - that is another corpus pass, not an analysis.
"""

import json
import math

d = json.load(open("analysis/mnq-measure-12a.json"))
curve = json.load(open("analysis/out/count-curve-measurement.json"))
sessions = d["observed"]["per_session"]

rows = []
for session in sessions:
    entry = {}
    for hour, windows in session["block2"].items():
        cell = windows.get("1")
        if cell and cell["scheduled_windows"] > 0 and cell["mean"] > 0:
            entry[int(hour)] = (cell["mean"], cell["scheduled_windows"])
    rows.append(entry)
hours = [h for h in sorted(rows[0]) if all(h in r for r in rows)]

print("sampling floor for a session-hour mean rate, three assumptions")
print("F1  = one-second Fano, the WRONG assumption that made the first estimate")
print("F300 = within-session Fano at 300 s, a defensible LOWER bound")
print()
header = (
    f"{'hr':>3} {'resid_sd':>9} {'sd|F1':>8} {'sd|F300':>9} "
    f"{'var share F1':>13} {'var share F300':>15}"
)
print(header)
tot_r = tot_1 = tot_300 = 0.0
for hour in hours:
    logs = [math.log(r[hour][0]) for r in rows]
    mean = sum(logs) / len(logs)
    resid_sd = math.sqrt(sum((x - mean) ** 2 for x in logs) / (len(logs) - 1))
    rate = sum(r[hour][0] for r in rows) / len(rows)
    n = sum(r[hour][1] for r in rows) / len(rows)

    cell = curve["observed"][str(hour)]
    f1 = cell["1"]["fano_within"]["point"]
    f300 = cell["300"]["fano_within"]["point"]

    sd1 = math.sqrt(f1 / (rate * n))
    # at 300 s there are n/300 windows, each 300 s long: n_T * T is unchanged
    sd300 = math.sqrt(f300 / (rate * n))

    tot_r += resid_sd**2
    tot_1 += sd1**2
    tot_300 += sd300**2
    print(
        f"{hour:>3} {resid_sd:9.4f} {sd1:8.4f} {sd300:9.4f} "
        f"{sd1**2 / resid_sd**2:12.1%} {sd300**2 / resid_sd**2:14.1%}"
    )

print()
print(f"pooled: sampling variance is {tot_1 / tot_r:.1%} of residual variance "
      f"under the WRONG one-second assumption")
print(f"        and at least {tot_300 / tot_r:.1%} using the 300 s lower bound")
print()
print("The true floor is HIGHER than the second figure, because the Fano curve")
print("is still climbing at 300 s and an hour is twelve times longer. So the")
print("share of session-hour residual variance that is genuine between-session")
print("structure is materially smaller than the first estimate claimed, and")
print("cannot be settled without the contiguous count sequences.")
