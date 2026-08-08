#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only
"""Bless the reference artifact for the `select_windows.py` absorption.

Phase 4b item 2 has a problem the other absorptions do not: there is NO frozen
artifact to port against. `select_windows.py` prints its results and writes only
`analysis/cme_daily_features.json`, a regenerable gitignored cache. And
`targets-frozen.json`, which three documents described as its gate, is the
BTCUSDT microstructure target set that this script never touches.

So the order is: bless a reference from the CURRENT Python over the four CME
archives, commit it, and only then match the port to it. This script is that
blessing. It runs while `mnq_fit.py` and friends are still runnable, which is
what the review signature's ordering exists to guarantee.

It IMPORTS `select_windows` rather than reimplementing it, so the artifact is
the Python's own arithmetic and not a second opinion about it. Nothing here
modifies the retiring script.

What is captured is the DETERMINISTIC STRUCTURE, not the printed tables: the
per-month feature medians, the eligible span, the z-scored vectors with their
key order, the seeds, and the final selection. The printed tables are a
rendering of those; a port that reproduces the structure reproduces the tables,
while a port matched against printed text would be pinned to formatting.

Floats are written as repr, which round-trips exactly in both languages, so the
comparison is bit-exact rather than "close".

    python3 scripts/gen_select_windows_features.py   # if the cache is missing
    python3 scripts/bless_select_windows.py

The cache itself is NOT committed: it is 1.5 GB of archives reduced to a few MB
of derived daily features, regenerable by `select_windows.py features`, and the
blessing records its provenance instead.
"""

import hashlib
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "analysis"))

import select_windows as sw  # noqa: E402

OUT = os.path.join(ROOT, "analysis", "select-windows-blessed.json")


def archive_provenance():
    """Size and digest of each input archive.

    The cache is gitignored and the archives are large and out of tree, so the
    only way a later reader can tell whether a mismatch means the port drifted
    or the inputs changed is to record what the inputs were.
    """
    out = {}
    for symbol, name in sorted(sw.ARCHIVES.items()):
        path = os.path.join(sw.MARKET_DATA, name)
        digest = hashlib.sha256()
        with open(path, "rb") as fh:
            for chunk in iter(lambda: fh.read(1 << 20), b""):
                digest.update(chunk)
        out[symbol] = {
            "file": name,
            "bytes": os.path.getsize(path),
            "sha256": digest.hexdigest(),
        }
    return out


def main():
    if not os.path.exists(sw.CACHE):
        raise SystemExit(
            f"{sw.CACHE} is missing; run `python3 analysis/select_windows.py features` first"
        )
    with open(sw.CACHE) as fh:
        cache = json.load(fh)

    months = sw.monthly(cache)
    eligible = {m: r for m, r in months.items() if m >= sw.DATABENTO_START}
    vectors, keys = sw.zscore(eligible)

    nq_rv = keys.index("NQ.rv")
    stress = max(vectors, key=lambda m: vectors[m][nq_rv])
    recent = max(vectors)
    seeds = [stress] if stress == recent else [stress, recent]
    chosen = sw.farthest_point(vectors, sw.BUDGET_MONTHS, seeds)

    ordered = sorted(eligible, key=lambda m: eligible[m]["NQ.rv"])
    percentiles = {
        month: 100.0 * ordered.index(month) / (len(ordered) - 1) for month in sorted(chosen)
    }

    # phase_drift: yearly medians. NOTE it uses vals[len//2], the UPPER middle
    # on an even count, where monthly() uses the true median. That difference is
    # real rather than a slip, so the blessing records what the code does.
    drift_cols = [
        "NQ.zero_change", "NQ.volume_cv", "NQ.volume",
        "ES.zero_change", "CL.zero_change", "GC.zero_change",
    ]
    years = {}
    for month, row in months.items():
        years.setdefault(month[:4], []).append(row)
    drift = {}
    for year in sorted(years):
        rows = years[year]
        drift[year] = [
            sorted(r[col] for r in rows)[len(rows) // 2] for col in drift_cols
        ]

    # phase_plan.
    eligible_sorted = sorted(m for m in months if m >= sw.DATABENTO_START)
    recent = eligible_sorted[-30:]
    ranked = sorted(recent, key=lambda m: months[m]["NQ.rv"])
    stratified = []
    for pct in (0, 20, 40, 60, 80, 100):
        idx = min(len(ranked) - 1, int(round(pct / 100.0 * (len(ranked) - 1))))
        stratified.append([pct, ranked[idx]])
    eras = []
    for lo, hi in (("2010-06", "2013-12"), ("2014-01", "2017-12"), ("2018-01", "2021-12")):
        pool = [m for m in eligible_sorted if lo <= m <= hi]
        stress = max(pool, key=lambda m: months[m]["NQ.rv"])
        calm = min(pool, key=lambda m: months[m]["NQ.rv"])
        eras.append({
            "lo": lo, "hi": hi,
            "stress": stress, "stress_rv": months[stress]["NQ.rv"],
            "calm": calm, "calm_rv": months[calm]["NQ.rv"],
        })

    blessed = {
        "_doc": (
            "Reference artifact for the select_windows.py absorption, phase 4b item 2. "
            "Produced by scripts/bless_select_windows.py from the Python's own functions "
            "while it was still runnable. The Rust port is matched against this. Floats "
            "are exact decimal round-trips, so the comparison is bit-exact."
        ),
        "constants": {
            "DATABENTO_START": sw.DATABENTO_START,
            "BUDGET_MONTHS": sw.BUDGET_MONTHS,
            "FEATURES": list(sw.FEATURES),
            "ARCHIVES": dict(sorted(sw.ARCHIVES.items())),
        },
        "provenance": {
            "archives": archive_provenance(),
            "sessions_per_symbol": {s: len(v) for s, v in sorted(cache.items())},
        },
        "monthly": {m: months[m] for m in sorted(months)},
        "eligible": {
            "count": len(eligible),
            "first": min(eligible),
            "last": max(eligible),
        },
        "zscore_keys": list(keys),
        "vectors": {m: vectors[m] for m in sorted(vectors)},
        "seeds": seeds,
        "selection": {
            "chosen_in_pick_order": list(chosen),
            "chosen_sorted": sorted(chosen),
            "nq_rv_percentile": percentiles,
        },
        "drift": {"columns": drift_cols, "years": drift},
        "plan": {
            "pool_first": recent[0],
            "pool_last": recent[-1],
            "pool_len": len(recent),
            "stratified": stratified,
            "eras": eras,
        },
    }

    with open(OUT, "w", encoding="utf-8") as fh:
        json.dump(blessed, fh, indent=1, sort_keys=False)
        fh.write("\n")

    print(f"wrote {OUT}")
    print(f"  months: {len(months)}, eligible: {len(eligible)}")
    print(f"  seeds: {seeds}")
    print(f"  selection: {sorted(chosen)}")


if __name__ == "__main__":
    main()
