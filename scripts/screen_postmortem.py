"""Post-mortem of analysis/mnq-arrival-screen.json.

Answers: which admissibility condition killed each cell, per family;
how close the near-misses were on each condition; and whether any
condition was satisfiable anywhere in the frozen grid at all.
"""

import json
from collections import Counter, defaultdict

d = json.load(open("analysis/mnq-arrival-screen.json"))
cells = d["cells"]

fam_total = Counter()
fam_fail = defaultdict(Counter)  # family -> condition -> fail count
fam_pass = defaultdict(Counter)  # family -> condition -> pass count

for c in cells:
    fam = c["family"]
    fam_total[fam] += 1
    for cond in ("a1", "a2", "a3", "a4"):
        rec = c.get(cond)
        if rec is None:
            continue
        if rec.get("passed"):
            fam_pass[fam][cond] += 1
        else:
            fam_fail[fam][cond] += 1

print("== per-family condition pass/fail (of evaluated cells) ==")
for fam in sorted(fam_total):
    parts = []
    for cond in ("a1", "a2", "a3", "a4"):
        p = fam_pass[fam][cond]
        f = fam_fail[fam][cond]
        parts.append(f"{cond}: {p} pass / {f} fail")
    print(f"{fam:15s} n={fam_total[fam]:4d}  " + "  ".join(parts))

# Which conditions does each cell fail (the joint pattern)?
print("\n== failure patterns per family ==")
pat = defaultdict(Counter)
for c in cells:
    failed = tuple(
        cond
        for cond in ("a1", "a2", "a3", "a4")
        if c.get(cond) is not None and not c[cond].get("passed")
    )
    pat[c["family"]][failed] += 1
for fam in sorted(pat):
    for p, n in pat[fam].most_common():
        print(f"{fam:15s} fails {p or '(none)'}: {n}")

# A1 near misses: cells with fewest failing required bins, and which
# bins are never reached anywhere in the grid.
print("\n== A1: closest cells and never-reached bins ==")
never = None
best = []
for c in cells:
    a1 = c.get("a1")
    if a1 is None:
        continue
    fail_bins = set()
    for s in a1["per_seed"]:
        fail_bins |= set(s["failing_cells"])
    if never is None:
        never = set(fail_bins)
    else:
        never &= fail_bins
    best.append((len(fail_bins), c["family"], c["params"], sorted(fail_bins)))
best.sort(key=lambda t: t[0])
for n, fam, params, bins in best[:8]:
    print(f"{n} failing bins  {fam}  {params}  {bins}")
print("bins failed by EVERY evaluated cell:", sorted(never or []))

# A2/A3: distribution of worst-hour ratio per cell, per family.
print("\n== A2: per-cell worst ratio (min and max over hours/seeds) ==")
for fam in sorted(fam_total):
    spans = []
    for c in cells:
        if c["family"] != fam or c.get("a2") is None:
            continue
        rs = [r["ratio"] for r in c["a2"]["per_seed_hour"]]
        if rs:
            spans.append((min(rs), max(rs)))
    if not spans:
        continue
    closest = min(spans, key=lambda t: max(abs(t[0] - 1), abs(t[1] - 1)))
    print(f"{fam:15s} cells={len(spans)}  closest cell span "
          f"[{closest[0]:.4f}, {closest[1]:.4f}] vs band [0.98, 1.02]")

print("\n== A3: same for zero-count ratio ==")
for fam in sorted(fam_total):
    spans = []
    for c in cells:
        if c["family"] != fam or c.get("a3") is None:
            continue
        rs = [r["ratio"] for r in c["a3"].get("per_seed_hour", [])]
        if rs:
            spans.append((min(rs), max(rs)))
    if not spans:
        continue
    closest = min(spans, key=lambda t: max(abs(t[0] - 1), abs(t[1] - 1)))
    print(f"{fam:15s} cells={len(spans)}  closest cell span "
          f"[{closest[0]:.4f}, {closest[1]:.4f}] vs band [0.8, 1.25]")

# Deep dive: the shipped-point event_markov cell's A4 and A3 records.
print("\n== shipped-point event_markov cell: A4 and A3 detail ==")
for c in cells:
    if c["family"] == "event_markov" and abs(c["params"]["switch_rate"] - 0.1) < 1e-9:
        print("A4:", json.dumps(c["a4"], indent=1)[:2500])
        print("A3:", json.dumps(c["a3"], indent=1)[:2500])
        print("cell refusals:", json.dumps(c["refusals"])[:800])
        break

# Refusal breakdown
print("\n== refusals ==")
rc = Counter()
for r in d["refusals"]:
    rc[(r.get("family"), r.get("kind") or r.get("condition"))] += 1
for k, n in rc.most_common(12):
    print(k, n)
