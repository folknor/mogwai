#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Decode the committed char_*.json duration log-histograms.

Review-validation helper for the arrival-drought spec: prints, per pair, the
top duration bins (the hour-scale-and-beyond gap mass) so the dwell claims in
the reviews can be checked against committed data without the corpus disk.
Bin edges mirror characterize.py: 40 log bins over [1e-3, 86400) s, with the
top bin SATURATING (log_bin returns nbins-1 for any value >= hi).
"""

import glob
import json
import math
import os

LO, HI, NBINS = 1e-3, 86400.0, 40


def edge(i):
    return LO * (HI / LO) ** (i / NBINS)


def main():
    here = os.path.dirname(__file__)
    for path in sorted(glob.glob(os.path.join(here, "char_*.json"))):
        rep = json.load(open(path))
        dur = rep["duration"]
        hist = dur["log_hist"]
        total = sum(hist)
        # bins whose lower edge is >= 1 hour
        hour_bins = [i for i in range(NBINS) if edge(i) >= 3600.0]
        gaps_over_1h = sum(hist[i] for i in hour_bins)
        top8 = [
            (round(edge(i)), round(edge(i + 1)), hist[i])
            for i in range(NBINS - 8, NBINS)
        ]
        print(rep["pair"])
        print(f"  trades={rep['n_trades']:,} span={rep['span_days']}d "
              f"mean_s={dur['mean_s']:.3f} disp={dur['dispersion_index']:.1f}")
        print(f"  gaps>=1h: {gaps_over_1h} of {total:,}")
        for lo_e, hi_e, c in top8:
            mark = " (SATURATED, >= hi)" if hi_e >= HI else ""
            print(f"    [{lo_e:>6} .. {hi_e:>6}) s: {c}{mark}")
    fp = json.load(open(os.path.join(here, "fingerprint.json")))
    md = fp["empirical_ranges"]["mean_event_duration_s"]
    print("fingerprint empirical_ranges.mean_event_duration_s:", md)
    disp = fp["golden_targets"]["duration_dispersion_cv2"]
    print("fingerprint golden_targets.duration_dispersion_cv2:", disp)


if __name__ == "__main__":
    main()
