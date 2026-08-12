#!/usr/bin/env python3
"""Generate the Stage M Amendment 4 frozen timezone authority artifact.

Extracts every America/Chicago UTC-offset transition in the frozen
coverage window from the host zoneinfo (whose tzdb release is recorded
in the artifact), bisected to the second. Runtime scheduling reads the
generated artifact, never the host zoneinfo, so results do not depend
on a mutable zoneinfo installation. The artifact's sha256 is printed
for hash-binding into the amendment.
"""
import hashlib
import json
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

ZONE = "America/Chicago"
COVERAGE_START = datetime(2024, 1, 1, tzinfo=timezone.utc)
COVERAGE_END = datetime(2030, 12, 31, 23, 59, 59, tzinfo=timezone.utc)
OUT = "analysis/tz-america-chicago-2026c.json"


def tzdb_release():
    with open("/usr/share/zoneinfo/tzdata.zi", encoding="utf-8") as f:
        header = f.readline().strip()
    # "# version 2026c"
    return header.split()[-1]


def transitions(zone):
    found = []
    cur = COVERAGE_START
    prev_off = cur.astimezone(zone).utcoffset()
    step = timedelta(hours=1)
    while cur < COVERAGE_END:
        nxt = cur + step
        off = nxt.astimezone(zone).utcoffset()
        if off != prev_off:
            lo, hi = cur, nxt
            while (hi - lo).total_seconds() > 1:
                mid = (lo + (hi - lo) / 2).replace(microsecond=0)
                if mid.astimezone(zone).utcoffset() == prev_off:
                    lo = mid
                else:
                    hi = mid
            found.append({
                "utc_instant": hi.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "offset_before_s": int(prev_off.total_seconds()),
                "offset_after_s": int(off.total_seconds()),
            })
            prev_off = off
        cur = nxt
    return found


def main():
    release = tzdb_release()
    artifact = {
        "_purpose": (
            "Stage M Amendment 4 frozen timezone authority: America/Chicago "
            "UTC offset transitions. Runtime scheduling reads THIS artifact, "
            "never the host zoneinfo."
        ),
        "zone": ZONE,
        "tzdb_release": release,
        "source": (
            "host /usr/share/zoneinfo (tzdata.zi header version), extracted "
            "via Python zoneinfo with one-second bisection at each offset "
            "change by analysis/generate_tz_artifact.py"
        ),
        "coverage_utc": {"start": "2024-01-01", "end": "2030-12-31"},
        "standard_offset_s": -21600,
        "daylight_offset_s": -18000,
        "transitions": transitions(ZoneInfo(ZONE)),
    }
    body = json.dumps(artifact, indent=1) + "\n"
    with open(OUT, "w", encoding="utf-8") as f:
        f.write(body)
    digest = hashlib.sha256(body.encode()).hexdigest()
    print("release:", release)
    print("transitions:", len(artifact["transitions"]))
    print("first:", artifact["transitions"][0])
    print("last:", artifact["transitions"][-1])
    print("path:", OUT)
    print("sha256:", digest)


if __name__ == "__main__":
    main()
