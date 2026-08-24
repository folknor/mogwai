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

from smoke import (  # noqa: E402  (the launcher contract lives there)
    Venue,
    WsClient,
    divergence_body,
)

CAP = 1024


def arm(addr: str, payload: dict) -> tuple[int, dict]:
    request = urllib.request.Request(
        f"http://{addr}/control/divergence",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(request, timeout=30) as resp:
        body = resp.read().decode()
    return resp.status, json.loads(body) if body else {}


def partial(client_order_id: str) -> dict:
    return divergence_body(
        "PartialFillNext", client_order_id=client_order_id, fraction="0.5"
    )


def main() -> None:
    with Venue(config=None, duration=None) as venue:
        # Board before arming, and stay boarded for the whole walk. The armed
        # queue whose cap this probes lives on an ACCOUNT's engine, and accounts
        # are minted when a socket presents one - so a probe that armed against
        # a venue nobody had connected to was arming into the run's replay
        # record and no engine at all. `Run::arm` reports an eviction only from
        # an engine it actually armed, so with no account every arm answered
        # "accepted" with nothing shed, the over-cap arm included, and the
        # probe's entire subject was unobservable. It read as a failing
        # assertion rather than a vacuous one only by luck.
        ws = WsClient(venue.addr, venue.symbol)
        try:
            walk(venue)
        finally:
            ws.close()


def walk(venue: Venue) -> None:
    for index in range(CAP):
        if index % 128 == 0:
            print(f"  armed {index}/{CAP}", flush=True)
        status, body = arm(venue.addr, partial(f"O-{index}"))
        assert status == 202, f"arm {index} answered {status}"
        assert body == {"status": "accepted"}, (
            f"arm {index} below the cap reported an eviction: {body}"
        )
    status, body = arm(venue.addr, partial("OVERFLOW"))
    assert status == 202, f"the over-cap arm answered {status}"
    # The ack is a JSON object now rather than a prose body: `detail` says an
    # eviction happened and `evicted` carries the shed divergence itself, so the
    # identity of the oldest entry is read from a field instead of scraped out
    # of a `Debug` rendering that happened to contain it.
    assert "discarded" in body.get("detail", ""), (
        f"the over-cap arm hid its eviction: {body!r}"
    )
    assert body.get("evicted", {}).get("client_order_id") == "O-0", (
        f"the shed entry is not the oldest: {body!r}"
    )
    print("PASS: an evicting arm acks 202 and names the discarded divergence")
    print(f"  body: {body}")


if __name__ == "__main__":
    main()
