"""Where do the A3 residuals sit, and does their SIGN turn over with activity?

The 12b close leaves one question for a successor: why does parent-second
occupancy miss across every family at its fitted hourly rate? The candidate
answer is that one hour-invariant dispersion law cannot hold across the
baseline's activity range, so the missing degree of freedom is
activity-conditioned clustering with global parameters.

The discriminating signal is the SIGN of the residual against activity, not its
magnitude: a sign that turns over as activity rises is the activity-conditioned
signature, while a constant sign points elsewhere.

TWO POPULATIONS, deliberately. The twenty frontier cells are decision-facing but
selected on A1, and conditioning on A1 can manufacture an apparent relationship.
The 618 complete A2 passers are the reference. A signature present in both is a
property of the mechanisms; one peculiar to the twenty is a property of the
selection.

Exploratory. Nothing here may reopen 12b - section 11 - and any successor
criterion must be preregistered before its cells are looked at.
"""

import json
import math

SCREEN = "analysis/mnq-arrival-screen.json"
COUNTERFACTUAL = "analysis/mnq-a2-envelope-counterfactual.json"

screen = json.load(open(SCREEN))
counter = json.load(open(COUNTERFACTUAL))

# Observed hourly activity, as parents per scheduled minute, from the committed
# 12a artifact by way of the screen's own observed side.
measure = json.load(open("analysis/mnq-measure-12a.json"))
block2 = measure["observed"]["monthly"]["block2"]
activity = {}
for hour, windows in block2.items():
    cell = windows.get("1")
    if cell is None:
        continue
    scheduled = cell["scheduled_windows"]
    if scheduled:
        activity[int(hour)] = cell["mean"]  # mean parents per 1 s window

frontier_keys = {
    (c["family"], json.dumps(c["params"], sort_keys=True)) for c in counter["cells"]
}


def signed_residuals(cell):
    """Signed log ratio per gated hour: positive means the generated side has
    MORE empty seconds than observed, negative fewer. The artifact stores the
    absolute deviation, so the sign is recovered from the raw per-seed ratios."""
    out = []
    for row in cell["a3"].get("gated", []):
        hour = row["hour"]
        raw = [
            r["ratio"]
            for r in cell["a3"].get("per_seed_raw", [])
            if r.get("hour") == hour and r.get("ratio") is not None
        ]
        if not raw:
            continue
        mean = sum(raw) / len(raw)
        if mean <= 0.0:
            out.append((hour, float("-inf")))
        else:
            out.append((hour, math.log(mean)))
    return out


def summarize(label, cells):
    print(f"\n== {label}: {len(cells)} cells ==")
    print(f"{'hour':>4} {'activity':>10} {'n':>5} {'neg':>5} {'pos':>5} "
          f"{'median signed log ratio':>24}")
    per_hour = {}
    for cell in cells:
        for hour, value in signed_residuals(cell):
            per_hour.setdefault(hour, []).append(value)
    for hour in sorted(per_hour):
        values = sorted(per_hour[hour])
        n = len(values)
        neg = sum(1 for v in values if v < 0)
        pos = n - neg
        finite = [v for v in values if math.isfinite(v)]
        med = finite[len(finite) // 2] if finite else float("-inf")
        print(f"{hour:>4} {activity.get(hour, float('nan')):10.4f} {n:>5} "
              f"{neg:>5} {pos:>5} {med:>24.4f}")


frontier = [
    c
    for c in screen["cells"]
    if (c["family"], json.dumps(c["params"], sort_keys=True)) in frontier_keys
]
reference = [c for c in screen["cells"] if c["a2"]["passed"]]

summarize("frontier (A1, A2, A4 pass; A3 alone fails)", frontier)
summarize("reference (complete A2 passers)", reference)

print("\n== sign turnover against activity ==")
for label, cells in (("frontier", frontier), ("reference", reference)):
    rows = []
    for cell in cells:
        for hour, value in signed_residuals(cell):
            if hour in activity and math.isfinite(value):
                rows.append((activity[hour], value))
    if not rows:
        continue
    rows.sort()
    half = len(rows) // 2
    quiet = rows[:half]
    busy = rows[half:]
    qm = sorted(v for _, v in quiet)[len(quiet) // 2]
    bm = sorted(v for _, v in busy)[len(busy) // 2]
    print(f"{label:10s} quiet-half median {qm:+.4f}   busy-half median {bm:+.4f}"
          f"   turnover: {'YES' if qm * bm < 0 else 'no'}")
