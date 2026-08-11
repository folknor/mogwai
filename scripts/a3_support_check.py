"""Why does A3 report over_cap for every cell of every family?

The census says 1,402 of 1,402 cells exceed A3_CAP on some gated hour, which
is the same shape the CLOSED run showed under the old gate. This asks whether
the driver is a genuine composition mismatch or the zero-numerator convention
firing at hours whose observed zero-window count barely clears the floor.

Reads the committed 12a artifact for the observed side and the closed screen
artifact for generated zero fractions (the RATIO is the same quantity under
both gate revisions; only the threshold changed).
"""

import json

GATED = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 17, 18, 19, 20, 22, 23]
FLOOR = 30

measure = json.load(open("analysis/mnq-measure-12a.json"))
block2 = measure["observed"]["monthly"]["block2"]

print("observed 1 s windows per gated hour, and the zero-window count")
print(f"{'hour':>4} {'scheduled':>10} {'zeros':>8} {'zero_frac':>10} {'>=floor':>8}")
obs_zero = {}
for hour in GATED:
    cell = block2.get(str(hour), {}).get("1")
    if cell is None:
        print(f"{hour:>4} {'MISSING':>10}")
        continue
    scheduled = cell["scheduled_windows"]
    zeros = cell["zero_windows"]
    obs_zero[hour] = (scheduled, zeros)
    frac = zeros / scheduled if scheduled else float("nan")
    print(f"{hour:>4} {scheduled:>10} {zeros:>8} {frac:>10.6f} "
          f"{'yes' if zeros >= FLOOR else 'NO':>8}")

# Generated side from the closed run: how often is the generated zero fraction
# exactly zero at a gated hour? That is what makes the deviation infinite.
screen = json.load(open("analysis/mnq-arrival-screen.json"))
print("\ngenerated zero-fraction ratios at gated hours, closed run")
print(f"{'hour':>4} {'cells_with_zero_generated':>26} {'cells_seen':>11}")
zero_gen = {h: 0 for h in GATED}
seen = {h: 0 for h in GATED}
for cell in screen["cells"]:
    for row in cell["a3"].get("per_seed_hour", []):
        hour = row["hour"]
        if hour not in seen:
            continue
        seen[hour] += 1
        if row["ratio"] == 0.0:
            zero_gen[hour] += 1
for hour in GATED:
    print(f"{hour:>4} {zero_gen[hour]:>26} {seen[hour]:>11}")

print("\nhours where EVERY observed-zero-bearing cell had generated zero:")
print([h for h in GATED if seen[h] and zero_gen[h] == seen[h]])

# The distribution of the ratio per gated hour, per family: can ANY cell land
# inside the amended cap, which in ratio terms is [0.5, 2.0]?
print("\nratio range per gated hour and family (closed run, per seed-hour)")
print(f"{'hour':>4} {'family':<14} {'min':>10} {'median':>10} {'max':>12} {'in_cap':>7}")
by = {}
for cell in screen["cells"]:
    fam = cell["family"]
    for row in cell["a3"].get("per_seed_hour", []):
        if row["hour"] in seen:
            by.setdefault((row["hour"], fam), []).append(row["ratio"])
for hour in GATED:
    for fam in sorted({c["family"] for c in screen["cells"]}):
        values = sorted(by.get((hour, fam), []))
        if not values:
            continue
        n = len(values)
        in_cap = sum(1 for v in values if 0.5 <= v <= 2.0)
        print(f"{hour:>4} {fam:<14} {values[0]:>10.4f} {values[n // 2]:>10.4f} "
              f"{values[-1]:>12.4f} {in_cap:>7}")
