#!/usr/bin/env python3
"""Check that an arming POST that EVICTS an older divergence says so.

The engine's armed-divergence queue is bounded at `MAX_ARMED_DIVERGENCES`, and
at the cap each new arm sheds the oldest entry. This walks a fresh server past
the cap and asserts that (a) every arm below the cap acks with an empty body,
(b) the first arm past the cap acks `202` with a body naming the discarded
entry, and (c) the entry named is the OLDEST one, not the one just posted.

Requires a mogwai server it can boot itself; nothing else may be arming against
it, since the queue is process-global.

Usage: python3 scripts/probe_arm_eviction.py [PORT]
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

HOST = "127.0.0.1"
CAP = 1024


def arm(port: int, payload: dict) -> tuple[int, str]:
    req = urllib.request.Request(
        f"http://{HOST}:{port}/control/divergence",
        data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return resp.status, resp.read().decode()


def partial(coid: str) -> dict:
    return {
        "type": "PartialFillNext",
        "client_order_id": coid,
        "fraction": "0.5",
    }


def wait_ready(port: int, proc: subprocess.Popen, deadline_s: float = 20.0) -> None:
    start = time.monotonic()
    while time.monotonic() - start < deadline_s:
        if proc.poll() is not None:
            raise SystemExit(f"server exited early with {proc.returncode}")
        try:
            with urllib.request.urlopen(
                f"http://{HOST}:{port}/health", timeout=5
            ) as resp:
                resp.read()
            return
        except (urllib.error.URLError, ConnectionError, TimeoutError):
            time.sleep(0.05)
    raise SystemExit("server never became ready")


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8802
    proc = subprocess.Popen(
        [
            "mogwai",
            "serve",
            "-f",
            "--addr",
            f"{HOST}:{port}",
            "--log-file",
            "scripts/probe-warmup.log",
        ],
        cwd=".",
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_ready(port, proc)
        for i in range(CAP):
            status, body = arm(port, partial(f"O-{i}"))
            assert status == 202, f"arm {i} answered {status}"
            assert body == "", f"arm {i} below the cap reported an eviction: {body}"
        status, body = arm(port, partial("OVERFLOW"))
        assert status == 202, f"the over-cap arm answered {status}"
        assert "discarded" in body, f"the over-cap arm hid its eviction: {body!r}"
        assert '"O-0"' in body, f"the shed entry is not the oldest: {body!r}"
        print("PASS: an evicting arm acks 202 and names the discarded divergence")
        print(f"  body: {body}")
    finally:
        proc.terminate()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
