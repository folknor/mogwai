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

RETAINED THROUGH THE RETIREMENT. This script imports nothing from `analysis/`,
so it survives phase 4b item 7 - and its reference side now defaults to the
COMMITTED `analysis/select-windows-python-cache.json` rather than the
regenerable `cme_daily_features.json`, which `select_windows.py` produced and
which nothing can produce any more. The committed copy is the frozen Python
oracle: 3.7 MB, all 111,396 values, kept whole precisely so a TWELFTH deviation
would still be visible. An eleven-row snapshot of the known corrections could
not do that.

    mogwai select-windows features --cache target/rust-cme-features.json
    python3 scripts/compare_cme_caches.py target/rust-cme-features.json
"""

import json
import os
import struct
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MANIFEST = os.path.join(ROOT, "analysis", "select-windows-cache-deviations.json")
# The frozen Python oracle. `cme_daily_features.json` was the working file and
# is no longer producible; this is the committed copy the gate reads.
PYTHON_CACHE = os.path.join(ROOT, "analysis", "select-windows-python-cache.json")


def bits(value):
    return struct.unpack("<Q", struct.pack("<d", float(value)))[0]


def main():
    args = [a for a in sys.argv[1:] if a != "--write-manifest"]
    write_manifest = "--write-manifest" in sys.argv
    if len(args) == 1:
        # The common case after retirement: only the Rust side varies, so the
        # frozen oracle is the default rather than something to remember.
        python_path, rust_path = PYTHON_CACHE, args[0]
    elif len(args) == 2:
        python_path, rust_path = args
    else:
        raise SystemExit(
            f"usage: {sys.argv[0]} [<python.json>] <rust.json> [--write-manifest]"
        )
    with open(python_path) as fh:
        left = json.load(fh)
    with open(rust_path) as fh:
        right = json.load(fh)
    entries = []

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
                    entries.append({
                        "symbol": symbol,
                        "session": day,
                        "feature": feature,
                        "python_bits": f"{bits(lv):016x}",
                        "rust_bits": f"{bits(rv):016x}",
                    })
                    if differences <= 20:
                        print(f"{symbol} {day} {feature}: "
                              f"{lv!r} ({bits(lv):016x}) against {rv!r} ({bits(rv):016x})")

    print(f"compared {compared} values across {len(left)} symbols")

    if write_manifest:
        entries.sort(key=lambda e: (e["symbol"], e["session"], e["feature"]))
        manifest = {
            "_doc": (
                "The INTENTIONAL cache-level corrections from select_windows::squared, which "
                "squares with a multiply where the Python uses ** 2 and libm pow. Recorded "
                "here because analysis/select-windows-blessed.json is derived FROM the cache "
                "and structurally cannot see a difference in a session no surviving monthly "
                "median depends on. Every entry is a value where the Rust is correctly "
                "rounded and CPython is not. Regenerate with scripts/compare_cme_caches.py "
                "--write-manifest."
            ),
            "compared_values": compared,
            "deviations": entries,
        }
        with open(MANIFEST, "w", encoding="utf-8") as fh:
            json.dump(manifest, fh, indent=1)
            fh.write("\n")
        print(f"wrote {MANIFEST} with {len(entries)} deviation(s)")
        return

    if differences:
        raise SystemExit(f"FAIL: {differences} difference(s)")
    print("identical, bit for bit")


if __name__ == "__main__":
    main()
