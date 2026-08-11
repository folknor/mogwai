"""Read the count-curve measurement: the Fano growth curve, decomposed.

The preregistration forbids this measurement proposing or ranking a mechanism,
so this only reports. The decomposition is the load-bearing column: without it,
session-to-session rate heterogeneity is indistinguishable from within-session
minute-scale correlation, and a successor would be sent after the wrong thing.
"""

import json

d = json.load(open("analysis/out/count-curve-measurement.json"))
observed = d["observed"]
generated = d["generated"]
HORIZONS = ["1", "5", "15", "60", "300"]


obs_hours = observed
gen_hours = generated


def get(cell, *names):
    """Metrics are nested under an uncertainty record: observed carries point,
    standard_error and the two percentiles; generated carries per-seed values
    plus median and spread. Pull the central figure from either shape."""
    for name in names:
        if not isinstance(cell, dict) or name not in cell:
            continue
        value = cell[name]
        if isinstance(value, dict):
            for inner in ("point", "median", "value"):
                if inner in value:
                    return value[inner]
            return None
        return value
    return None


print("FANO ACROSS HORIZON, observed against generated, by hour")
print("obs = pooled observed, W = within-session, B = between-session,")
print("gen = median across the eight seeds, ratio = gen / obs")
print()
head = f"{'hr':>3} {'stat':>5} " + " ".join(f"{h + 's':>10}" for h in HORIZONS) + f"{'growth':>9}"
print(head)
for hour in sorted(obs_hours, key=int):
    ocell = obs_hours[hour]
    gcell = gen_hours.get(hour, {})
    rows = {}
    for label, source, names in (
        ("obs", ocell, ("fano", "fano_total")),
        ("W", ocell, ("fano_within", "within")),
        ("B", ocell, ("fano_between", "between")),
        ("gen", gcell, ("fano_median", "fano", "median")),
    ):
        values = []
        for h in HORIZONS:
            entry = source.get(h) if isinstance(source, dict) else None
            values.append(get(entry, *names) if isinstance(entry, dict) else None)
        rows[label] = values
    for label in ("obs", "W", "B", "gen"):
        values = rows[label]
        cells = " ".join(
            f"{v:10.1f}" if isinstance(v, (int, float)) else f"{'-':>10}" for v in values
        )
        first, last = values[0], values[-1]
        growth = (
            f"{last / first:8.1f}x"
            if isinstance(first, (int, float))
            and isinstance(last, (int, float))
            and first
            else f"{'-':>9}"
        )
        print(f"{hour:>3} {label:>5} {cells}{growth}")
    print()
