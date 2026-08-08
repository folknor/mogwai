#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only
"""Confirm WHY the Rust `select_windows` cache differs from the Python's.

Eleven of 111,396 cached values disagree by one or two ULPs, all of them
`volume_cv` or `vol_of_vol`. Both are `sqrt(variance) / mean`, and both
variances are built in Python as `sum((x - mean) ** 2 ...)`. The port uses
`d * d`.

`d * d` is a single IEEE multiply and therefore always correctly rounded.
`d ** 2` goes through `libm`'s `pow`, which is not - it disagrees with the
correctly rounded product in roughly one value in 1,163 over this domain. So
the hypothesis is that the port is RIGHT and CPython is carrying a libm
artifact, exactly as with `x ** 0.5` against `sqrt` in the fingerprint work.

This re-reads ONE archive and recomputes the named session both ways, so the
hypothesis is confirmed on the actual failing data rather than on a random
sample that merely shows the two operations can differ.

    python3 scripts/probe_cme_square.py CL 2018-04-09 volume_cv
"""

import math
import os
import struct
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(ROOT, "analysis"))

import select_windows as sw  # noqa: E402


def bits(value):
    return struct.unpack("<Q", struct.pack("<d", float(value)))[0]


def collect(symbol, want_session):
    """Re-run the sweep for one archive, keeping the raw slot for one session."""
    import collections
    import zipfile

    path = os.path.join(sw.MARKET_DATA, sw.ARCHIVES[symbol])
    z = zipfile.ZipFile(path)
    member = z.infolist()[0].filename
    days = collections.OrderedDict()
    prev_stamp = prev_close = prev_session = None
    with z.open(member) as fh:
        for raw in fh:
            parsed = sw.parse_line(raw.decode("ascii", "replace"))
            if parsed is None:
                continue
            stamp, open_px, close, vol = parsed
            if prev_stamp is not None and stamp <= prev_stamp:
                continue
            if close <= 0.0 or open_px <= 0.0:
                continue
            sess = sw.session_date(stamp)
            if sess.weekday() == 5:
                continue
            slot = days.get(sess)
            if slot is None:
                slot = {"ret2": 0.0, "n": 0, "volume": 0, "vols": [], "max_r2": 0.0,
                        "zero": 0, "hourly": collections.defaultdict(float), "gap": 0.0}
                days[sess] = slot
                if prev_close is not None and prev_session is not None and sess != prev_session:
                    slot["gap"] = abs(math.log(open_px / prev_close))
            if prev_close is not None and prev_session == sess:
                missing = int((stamp - prev_stamp).total_seconds() // 60) - 1
                if missing > 0:
                    slot["zero"] += missing
                    slot["n"] += missing
                    slot["vols"].extend([0] * missing)
                r = math.log(close / prev_close)
                slot["ret2"] += r * r
                if r * r > slot["max_r2"]:
                    slot["max_r2"] = r * r
                slot["hourly"][stamp.hour] += r * r
                if r == 0.0:
                    slot["zero"] += 1
                slot["n"] += 1
            slot["volume"] += vol
            slot["vols"].append(vol)
            prev_stamp, prev_close, prev_session = stamp, close, sess
    for sess, slot in days.items():
        if sess.isoformat() == want_session:
            return slot
    raise SystemExit(f"{want_session} not found in {symbol}")


def main():
    symbol, session, feature = sys.argv[1], sys.argv[2], sys.argv[3]
    slot = collect(symbol, session)

    if feature == "volume_cv":
        vols = slot["vols"]
        mean = sum(vols) / len(vols)
        with_pow = sum((v - mean) ** 2 for v in vols) / len(vols)
        with_mul = sum((v - mean) * (v - mean) for v in vols) / len(vols)
    elif feature == "vol_of_vol":
        hourly = [math.sqrt(v) for v in slot["hourly"].values()]
        mean = sum(hourly) / len(hourly)
        with_pow = sum((h - mean) ** 2 for h in hourly) / len(hourly)
        with_mul = sum((h - mean) * (h - mean) for h in hourly) / len(hourly)
    else:
        raise SystemExit("feature must be volume_cv or vol_of_vol")

    print(f"{symbol} {session} {feature}")
    print(f"  mean            {mean!r}")
    print(f"  variance ** 2   {with_pow!r}  ({bits(with_pow):016x})")
    print(f"  variance d*d    {with_mul!r}  ({bits(with_mul):016x})")
    print(f"  variance differ {bits(with_pow) != bits(with_mul)}")
    pow_result = math.sqrt(with_pow) / mean
    mul_result = math.sqrt(with_mul) / mean
    print(f"  feature ** 2    {pow_result!r}  ({bits(pow_result):016x})")
    print(f"  feature d*d     {mul_result!r}  ({bits(mul_result):016x})")
    print(f"  feature differ  {bits(pow_result) != bits(mul_result)}")


if __name__ == "__main__":
    main()
