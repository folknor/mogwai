"""Second-pass checks on analysis/mnq-arrival-screen.json.

Verifies the claims from the codex session: (a) no cell passes A1 and A2
jointly; (b) the refusal breakdown by scope/variant; (c) whether A3
survives an observed-support floor; (d) how close the A1-passing cells
come on A2 and A3, per parameter, to judge refinement potential.
"""

import json
from collections import Counter, defaultdict

d = json.load(open("analysis/mnq-arrival-screen.json"))
cells = d["cells"]

print("== joint A1+A2 pass ==")
joint = [c for c in cells if c["a1"]["passed"] and c["a2"]["passed"]]
print("cells passing both A1 and A2:", len(joint))
for c in joint:
    print(" ", c["family"], c["params"])

print("\n== refusal breakdown (top-level artifact list) ==")
rc = Counter()
for r in d["refusals"]:
    scope = r.get("scope")
    variant = None
    inner = r.get("refusal")
    if isinstance(inner, dict):
        variant = inner.get("variant") or (inner.get("refusal") or {}).get("variant")
    rc[(scope, variant, tuple(sorted(r.keys())))] += 1
for k, n in rc.most_common(10):
    print(n, k)

print("\n== A1-passing cells: their A2 and A3 distances ==")
for c in cells:
    if not c["a1"]["passed"]:
        continue
    a2 = [r["ratio"] for r in c["a2"]["per_seed_hour"]]
    a3 = [r["ratio"] for r in c["a3"]["per_seed_hour"]]
    a2_lo, a2_hi = min(a2), max(a2)
    a3_lo, a3_hi = min(a3), max(a3)
    a2_bad = sum(1 for r in a2 if not 0.98 <= r <= 1.02)
    a3_bad = sum(1 for r in a3 if not 0.8 <= r <= 1.25)
    print(f"{c['family']:12s} {str(c['params']):55s} "
          f"A2 [{a2_lo:.3f},{a2_hi:.3f}] bad {a2_bad}/{len(a2)}  "
          f"A3 [{a3_lo:.3f},{a3_hi:.3f}] bad {a3_bad}/{len(a3)}")

print("\n== A2-passing log_ou_cox cells: their A1 failing bins ==")
for c in cells:
    if c["family"] != "log_ou_cox" or not c["a2"]["passed"]:
        continue
    bins = set()
    for s in c["a1"]["per_seed"]:
        bins |= set(s["failing_cells"])
    print(f"{str(c['params']):45s} A1 fail bins: {sorted(bins)}")

# A3 with an observed-support floor: we do not have observed zero-window
# counts in the cell records, so approximate the codex check by dropping
# the cash hours 13..20 entirely and seeing if any cell passes on the rest.
print("\n== A3 restricted to hours outside 13..20 ==")
best = []
for c in cells:
    rs = [r for r in c["a3"]["per_seed_hour"] if not 13 <= r["hour"] <= 20]
    bad = [r for r in rs if not 0.8 <= r["ratio"] <= 1.25]
    best.append((len(bad), c["family"], c["params"]))
best.sort(key=lambda t: t[0])
n_pass = sum(1 for b in best if b[0] == 0)
print("cells passing restricted A3:", n_pass)
for b in best[:6]:
    print(" ", b)
