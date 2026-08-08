#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only
"""Compare two `cme_daily_features.json` caches BIT-EXACTLY.

The `select_windows.py` port is gated against `select-windows-blessed.json`,
which is derived from the cache. That leaves one thing the gate cannot see: the
cache ITSELF, produced by the archive sweep in `build_features`. Two sweeps can
disagree on a session that no surviving month depends on - a sub-threshold day,
or a month that fails the fifteen-session floor - and the blessed comparison
would still pass.

So this compares the caches directly, session by session, feature by feature,
on bit patterns rather than on decimal text.

    python3 analysis/select_windows.py features
    mogwai select-windows features --cache target/rust-cme-features.json
    python3 scripts/compare_cme_caches.py analysis/cme_daily_features.json \\
        target/rust-cme-features.json
"""

import json
import struct
import sys


def bits(value):
    return struct.unpack("<Q", struct.pack("<d", float(value)))[0]


def main():
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} <left.json> <right.json>")
    with open(sys.argv[1]) as fh:
        left = json.load(fh)
    with open(sys.argv[2]) as fh:
        right = json.load(fh)

    if sorted(left) != sorted(right):
        raise SystemExit(f"symbols differ: {sorted(left)} against {sorted(right)}")

    differences = 0
    compared = 0
    for symbol in sorted(left):
        ldays, rdays = left[symbol], right[symbol]
        only_left = sorted(set(ldays) - set(rdays))
        only_right = sorted(set(rdays) - set(ldays))
        if only_left or only_right:
            print(f"{symbol}: sessions only on the left {only_left[:5]}, "
                  f"only on the right {only_right[:5]}")
            differences += len(only_left) + len(only_right)
        for day in sorted(set(ldays) & set(rdays)):
            for feature in sorted(ldays[day]):
                compared += 1
                lv, rv = ldays[day][feature], rdays[day][feature]
                if bits(lv) != bits(rv):
                    differences += 1
                    if differences <= 20:
                        print(f"{symbol} {day} {feature}: "
                              f"{lv!r} ({bits(lv):016x}) against {rv!r} ({bits(rv):016x})")

    print(f"compared {compared} values across {len(left)} symbols")
    if differences:
        raise SystemExit(f"FAIL: {differences} difference(s)")
    print("identical, bit for bit")


if __name__ == "__main__":
    main()
