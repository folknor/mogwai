#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Check that an arming POST that EVICTS an older divergence says so.

The engine's armed-divergence queue is bounded at `MAX_ARMED_DIVERGENCES`, and
at the cap each new arm sheds the oldest entry. This walks a fresh venue past
the cap and asserts that (a) every arm below the cap acks with an empty body,
(b) the first arm past the cap acks `202` with a body naming the discarded
entry, and (c) the entry named is the OLDEST one, not the one just posted.

This probe survives the lifecycle rewrite because its subject does: it probes
divergence ARMING, not history. What changed is only how it reaches a venue -
it boots its own on an ephemeral port and learns the endpoint from the
readiness record, exactly as `scripts/smoke.py` does, because there is no
fixed port to connect to any more and no pid file to find one by.

The armed queue is per-RUN state, and this probe owns the run it boots, so
nothing else can be arming against it.

Usage: python3 scripts/probe_arm_eviction.py
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from smoke import Venue  # noqa: E402  (the launcher contract lives there)

CAP = 1024


def arm(addr: str, payload: dict) -> tuple[int, str]:
    request = urllib.request.Request(
        f"http://{addr}/control/divergence",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as resp:
        return resp.status, resp.read().decode()


def partial(client_order_id: str) -> dict:
    return {
        "type": "PartialFillNext",
        "client_order_id": client_order_id,
        "fraction": "0.5",
    }


def main() -> None:
    with Venue(config=None, duration=None) as venue:
        for index in range(CAP):
            if index % 128 == 0:
                print(f"  armed {index}/{CAP}", flush=True)
            status, body = arm(venue.addr, partial(f"O-{index}"))
            assert status == 202, f"arm {index} answered {status}"
            assert body == "", f"arm {index} below the cap reported an eviction: {body}"
        status, body = arm(venue.addr, partial("OVERFLOW"))
        assert status == 202, f"the over-cap arm answered {status}"
        assert "discarded" in body, f"the over-cap arm hid its eviction: {body!r}"
        assert '"O-0"' in body, f"the shed entry is not the oldest: {body!r}"
        print("PASS: an evicting arm acks 202 and names the discarded divergence")
        print(f"  body: {body}")


if __name__ == "__main__":
    main()
