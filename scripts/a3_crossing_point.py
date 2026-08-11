"""Does the dispersion that gets the zero fraction RIGHT move with the hour?

The 12b close leaves a criterion question for a successor, and the pooled
sign-versus-activity cut cannot answer it: the four hours whose residual sign is
positive are simultaneously the four highest-activity hours AND the four with
the fewest observed zero windows, 44 to 260 against 576 to 8,714 elsewhere. That
confound makes "activity-conditioned clustering" and "tiny observed denominator"
indistinguishable on that cut, so this asks a cleaner question instead.

The clean signal: at the SAME well-supported hours, the twenty frontier cells
(high dispersion, A1-passing) generate too MANY empty seconds while the 618
complete A2 passers (lower dispersion) generate too FEW. A sign change between
those populations implies a crossing - some dispersion at which the zero
fraction is right.

So: per hour, find the sigma_y where log-OU's signed residual crosses zero, at
fixed tau. If that crossing sits at the SAME sigma for every well-supported
hour, one global dispersion law could satisfy the zero fraction and the failure
lies elsewhere. If it MOVES systematically with hour activity, then no
hour-invariant dispersion can be right everywhere, and activity-conditioned
clustering is the named missing degree of freedom.

log-OU only: it is the family with a single dispersion knob, so the crossing is
one-dimensional and unambiguous. Exploratory - section 11 forbids this reaching
back into 12b.
"""

import json
import math

screen = json.load(open("analysis/mnq-arrival-screen.json"))
measure = json.load(open("analysis/mnq-measure-12a.json"))

block2 = measure["observed"]["monthly"]["block2"]
activity = {int(h): w["1"]["mean"] for h, w in block2.items() if "1" in w}
zeros = {int(h): w["1"]["zero_windows"] for h, w in block2.items() if "1" in w}

# Well-supported gated hours only: the four thin ones are excluded because their
# observed denominator is of order 1e-3 and drives the residual on its own.
WELL = [h for h in (0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 20, 22, 23)]


def signed_residual(cell, hour):
    raw = [
        r["ratio"]
        for r in cell["a3"].get("per_seed_raw", [])
        if r.get("hour") == hour and r.get("ratio") is not None
    ]
    if not raw:
        return None
    mean = sum(raw) / len(raw)
    return math.log(mean) if mean > 0.0 else float("-inf")


by_tau = {}
for cell in screen["cells"]:
    if cell["family"] != "log_ou_cox":
        continue
    by_tau.setdefault(cell["params"]["tau_s"], []).append(cell)

taus = sorted(by_tau)
shown = [t for t in taus if t in (10.0, 100.0, 1000.0)] or taus[:3]

print("sigma_y at which the signed zero-fraction residual crosses zero")
print("(below it the generated tape has too FEW empty seconds, above it too many)")
print()
header = f"{'hour':>4} {'activity':>9} {'obs_zeros':>10} " + " ".join(
    f"tau={t:<8.0f}" for t in shown
)
print(header)
crossings = {t: [] for t in shown}
for hour in sorted(WELL, key=lambda h: activity[h]):
    row = []
    for tau in shown:
        cells = sorted(by_tau[tau], key=lambda c: c["params"]["sigma_y"])
        crossing = None
        previous = None
        for cell in cells:
            value = signed_residual(cell, hour)
            if value is None or not math.isfinite(value):
                continue
            sigma = cell["params"]["sigma_y"]
            if previous is not None and previous[1] < 0.0 <= value:
                crossing = (previous[0] + sigma) / 2.0
            previous = (sigma, value)
        if crossing is not None:
            crossings[tau].append((activity[hour], crossing))
        row.append(f"{crossing:12.2f}" if crossing is not None else f"{'none':>12}")
    print(f"{hour:>4} {activity[hour]:9.2f} {zeros[hour]:10d} " + " ".join(row))

print()
for tau in shown:
    points = crossings[tau]
    if len(points) < 3:
        print(f"tau={tau:<8.0f} too few crossings to read")
        continue
    lo = [c for a, c in points if a < 7.0]
    hi = [c for a, c in points if a >= 7.0]
    if lo and hi:
        print(
            f"tau={tau:<8.0f} crossing at quiet hours {sum(lo) / len(lo):.2f}, "
            f"at busier hours {sum(hi) / len(hi):.2f}, "
            f"spread {max(c for _, c in points) - min(c for _, c in points):.2f}"
        )
