#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Re-derive `cadence.json`'s targets from its OWN committed per-pair readings.

`build_cadence.py` is the source of truth and re-probes the raw-trade archives;
this is the cheap path for the case where only the BAND RULE changed, since the
per-pair statistics the rule consumes are already committed in `cadence.json`.
It reads and writes nothing else, and it re-mirrors the result into
`fingerprint.json` exactly as `build_fingerprint.py` would - the `cadence` block
and the five cadence-derived `scalar_ranges` entries.

Run `build_cadence.py` instead whenever the MEASUREMENT is what changed.
"""

import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

from build_cadence import PAIRS, band  # noqa: E402

FLOORS = {
    "duration_dispersion_cv2": 1.0,
    "duration_acf_lag1": 1e-6,
    "duration_acf_lag5": 0.0,
    "levels_mean": 1.0,
    "mean_event_duration_s": 1e-9,
    "children_mean": 1.0 + 1e-9,
    "children_single_frac": 0.0,
    "typical_notional": 1e-9,
}
MIRRORED = (
    "mean_event_duration_s",
    "children_mean",
    "children_single_frac",
    "levels_mean",
    "typical_notional",
)


def fields(reports):
    primary = {pair: reports[pair]["timestamp_and_side"] for pair in PAIRS}
    return {
        "mean_event_duration_s": [primary[p]["parent_gap"]["mean_s"] for p in PAIRS],
        "children_mean": [primary[p]["children"]["mean"] for p in PAIRS],
        "children_single_frac": [primary[p]["children"]["single_frac"] for p in PAIRS],
        "levels_mean": [primary[p]["levels"]["mean"] for p in PAIRS],
        "typical_notional": [reports[p]["mean_notional"] for p in PAIRS],
        "duration_dispersion_cv2": [primary[p]["parent_gap"]["cv2"] for p in PAIRS],
        "duration_acf_lag1": [primary[p]["parent_gap"]["acf_lag1"] for p in PAIRS],
        "duration_acf_lag5": [primary[p]["parent_gap"]["acf_lag5"] for p in PAIRS],
    }


def main():
    cadence_path = os.path.join(HERE, "cadence.json")
    fingerprint_path = os.path.join(HERE, "fingerprint.json")
    with open(cadence_path) as stream:
        cadence = json.load(stream)
    for name, values in fields(cadence["pairs"]).items():
        old = cadence["targets"][name]["range"]
        new = band(values, FLOORS.get(name))
        cadence["targets"][name]["range"] = new
        print(f"{name}: [{old['min']:.6g}, {old['max']:.6g}]"
              f" -> [{new['min']:.6g}, {new['max']:.6g}]")
    with open(cadence_path, "w") as stream:
        stream.write(json.dumps(cadence, indent=2, sort_keys=True) + "\n")

    with open(fingerprint_path) as stream:
        fingerprint = json.load(stream)
    fingerprint["cadence"] = cadence
    for name in MIRRORED:
        fingerprint["scalar_ranges"][name] = cadence["targets"][name]["range"]
    # `build_fingerprint.py` writes insertion order with no trailing newline;
    # match it exactly so this tool's output is a value diff, not a re-ordering.
    with open(fingerprint_path, "w") as stream:
        json.dump(fingerprint, stream, indent=2)
    print(f"rewrote {cadence_path} and {fingerprint_path}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
