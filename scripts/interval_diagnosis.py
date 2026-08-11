"""The unifying interval diagnosis: is the fine-scale deficit one defect or two?

The owner goal is a plausible MNQ tape at ALL intervals, 15 s and 1 min named
explicitly. The measured state is monotone in horizon - generated over observed
robust_scale runs 0.657 at 1 s, 0.838 at 5 s, 0.897 at 15 s, 0.919 at 60 s and
0.950 at 300 s - and only 60 s and 300 s have ever been gated.

The working hypothesis, held as FALSIFIABLE rather than assumed: arrival
under-dispersion drives the fine-interval deficits while a per-parent volatility
scale compensates at the extreme tail. The reviewing session refused it as a
premise on a specific ground worth restating, because it is the thing this
script has to test rather than assume: with independent, roughly zero-mean
per-parent increments, Var(R_T) is about E[N_T] * Var(Z), so count variance does
NOT automatically raise return variance. Clustering could instead manufacture
more zero-return windows and heavier tails - lowering a robust central scale
while raising extremes. Direction depends on the mark, burst and arrival
coupling, which nobody has measured.

THREE PANELS, all from committed artifacts, no new data and no generator runs:

  1. COUNT PROCESS      Fano, zero probability and upper count quantiles.
  2. NORMALIZED TAIL    rms_scale over robust_scale, a shape statistic that is
                        invariant to any uniform scalar - so if it disagrees,
                        no `vol_scalar` change can fix it.
  3. TEMPORAL DEPENDENCE  the adjacent-horizon variance ratios and the
                        normalized covariance contribution. Under independence
                        VR is 1; above it means clustering carried across the
                        horizon, below it mean reversion. This is what
                        separates a SCALE defect from a DEPENDENCE defect.

Hour 20 is reported separately throughout: its partial-session structure already
produced a distinct incompatibility in the 12b close, and pooling it would hide
exactly the stratum that is known to behave differently.
"""

import json
import statistics

d = json.load(open("analysis/mnq-measure-12a.json"))
observed = d["observed"]["monthly"]
central = d["generated"]["central"]["blocks"]

HOURS = sorted((h for h in observed["block3"]["cells"]), key=int)
PARTIAL = "20"
NORMAL = [h for h in HOURS if h != PARTIAL]


def ratio(gen, obs):
    if gen is None or obs is None or obs == 0:
        return None
    return gen / obs


def summarize(label, values):
    clean = [v for v in values if v is not None]
    if not clean:
        return f"{label:>26} {'no data':>34}"
    return (
        f"{label:>26} median {statistics.median(clean):7.3f} "
        f"min {min(clean):7.3f} max {max(clean):7.3f}  n={len(clean)}"
    )


print("=" * 78)
print("PANEL 1: the count process - is the clustering deficit horizon-wide?")
print("=" * 78)
print("generated/observed, by count window. Only 1, 5 and 60 s were measured;")
print("15 s and 300 s counts do not exist in the artifact and would need a rerun.")
for stat in ("fano", "zero_fraction", "count_p99", "mean"):
    print()
    for window in ("1", "5", "60"):
        values = []
        for hour in NORMAL:
            o = observed["block2"].get(hour, {}).get(window, {}).get(stat)
            g = central["block2"].get(hour, {}).get(window, {}).get(stat)
            values.append(ratio(g, o))
        print(summarize(f"{stat} @ {window}s", values))
        if window == "60":
            o = observed["block2"][PARTIAL][window].get(stat)
            g = central["block2"][PARTIAL][window].get(stat)
            r = ratio(g, o)
            print(f"{'  hour 20 (partial)':>26} {r if r is None else round(r, 3)}")

print()
print("=" * 78)
print("PANEL 2: normalized tail shape - can ANY uniform scalar fix it?")
print("=" * 78)
print("rms_scale / robust_scale is scale-INVARIANT: multiplying every return by")
print("a constant leaves it unchanged. So a generated/observed ratio away from 1")
print("is a shape defect that no vol_scalar change can reach.")
print()
for horizon in ("1", "5", "15", "60", "300"):
    values = []
    for hour in NORMAL:
        oc = observed["block3"]["cells"].get(hour, {}).get(horizon, {})
        gc = central["block3"]["cells"].get(hour, {}).get(horizon, {})
        o_shape = ratio(oc.get("rms_scale"), oc.get("robust_scale"))
        g_shape = ratio(gc.get("rms_scale"), gc.get("robust_scale"))
        values.append(ratio(g_shape, o_shape))
    print(summarize(f"tail shape @ {horizon}s", values))
    oc = observed["block3"]["cells"][PARTIAL].get(horizon, {})
    gc = central["block3"]["cells"][PARTIAL].get(horizon, {})
    o_shape = ratio(oc.get("rms_scale"), oc.get("robust_scale"))
    g_shape = ratio(gc.get("rms_scale"), gc.get("robust_scale"))
    r = ratio(g_shape, o_shape)
    print(f"{'  hour 20 (partial)':>26} {r if r is None else round(r, 3)}")

print()
print("=" * 78)
print("PANEL 3: temporal dependence - a SCALE defect or a DEPENDENCE defect?")
print("=" * 78)
print("VR(h,H) = mean(R_H^2) / (k * mean(r_h^2)). Under independence VR = 1;")
print("above means variance compounds faster than iid (clustering carried across")
print("the horizon), below means mean reversion. A uniform scalar cancels in VR")
print("exactly, so a VR mismatch is ALSO beyond any vol_scalar repair.")
print()
pairs = sorted(observed["block3"]["pairs"][HOURS[0]].keys())
for pair in pairs:
    obs_vr, gen_vr, rel = [], [], []
    for hour in NORMAL:
        o = observed["block3"]["pairs"].get(hour, {}).get(pair, {}).get("vr")
        g = central["block3"]["pairs"].get(hour, {}).get(pair, {}).get("vr")
        if o is not None:
            obs_vr.append(o)
        if g is not None:
            gen_vr.append(g)
        rel.append(ratio(g, o))
    om = statistics.median(obs_vr) if obs_vr else float("nan")
    gm = statistics.median(gen_vr) if gen_vr else float("nan")
    print(f"  VR {pair:>7}   observed median {om:7.3f}   generated median {gm:7.3f}")
    print(summarize(f"    ratio {pair}", rel))

print()
print("normalized covariance contribution C_norm (0 = iid; the sign says whether")
print("cross-products add variance or cancel it):")
for pair in pairs:
    obs_c, gen_c = [], []
    for hour in NORMAL:
        o = observed["block3"]["pairs"].get(hour, {}).get(pair, {}).get("cov_contrib_norm")
        g = central["block3"]["pairs"].get(hour, {}).get(pair, {}).get("cov_contrib_norm")
        if o is not None:
            obs_c.append(o)
        if g is not None:
            gen_c.append(g)
    om = statistics.median(obs_c) if obs_c else float("nan")
    gm = statistics.median(gen_c) if gen_c else float("nan")
    print(f"  C_norm {pair:>7}   observed {om:+7.4f}   generated {gm:+7.4f}   gap {gm - om:+7.4f}")
