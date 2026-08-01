#!/usr/bin/env python3
"""Measure whether a freshly-booted mogwai serves an EMPTY historical warmup.

Boots a server on a private port with `scripts/probe-warmup.toml`, then walks
uptime and, at each step, asks `/trades` for the same shape of window a
nautilus bar warmup asks for: `[sim_now - span, sim_now]`. Prints the trade
count per (uptime, span) cell plus the tape density over the whole served
range, so "the warmup batch was empty" can be attributed to tape sparsity
versus a serving bug.

Usage: python3 scripts/probe_warmup_window.py [PORT] [CONFIG]

CONFIG defaults to `scripts/probe-warmup.toml` (the shipping 24h backfill
horizon). Pass `scripts/probe-warmup-long.toml` for a 7-day horizon, which is
what shows that the arrival rate recovers between droughts rather than decaying
monotonically. Findings are written up in `reference/architecture.md` under
"Tape arrival droughts".
"""

from __future__ import annotations

import json
import subprocess
import sys
import time
import urllib.error
import urllib.request

HOST = "127.0.0.1"
NS_PER_MIN = 60_000_000_000
# Warmup spans worth probing, in minutes: 5 and 50 are the `warmup_bars` values
# the QA run used against 1-minute bars, 120 is a generous control.
SPANS_MIN = (5, 50, 120)
# Wall seconds of server uptime to sample. At speed 60 these are 0, 5, 30 and
# 120 simulated minutes past the epoch.
UPTIMES_S = (0.0, 5.0, 30.0, 120.0)


def get(port: int, path: str) -> object:
    with urllib.request.urlopen(f"http://{HOST}:{port}{path}", timeout=30) as resp:
        return json.loads(resp.read().decode())


def wait_ready(port: int, proc: subprocess.Popen, deadline_s: float = 20.0) -> None:
    start = time.monotonic()
    while time.monotonic() - start < deadline_s:
        if proc.poll() is not None:
            raise SystemExit(f"server exited early with {proc.returncode}")
        try:
            # `/health` answers plain text, so probe it without JSON decoding.
            with urllib.request.urlopen(
                f"http://{HOST}:{port}/health", timeout=5
            ) as resp:
                resp.read()
            return
        except (urllib.error.URLError, ConnectionError, TimeoutError):
            time.sleep(0.05)
    raise SystemExit("server never became ready")


def trades(port: int, start_ns: int, end_ns: int) -> list:
    query = f"/trades?symbol=BTCUSDT&start={start_ns}&end={end_ns}&limit=1000"
    return get(port, query)


def main() -> None:
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8799
    cfg = sys.argv[2] if len(sys.argv) > 2 else "scripts/probe-warmup.toml"
    proc = subprocess.Popen(
        [
            "mogwai",
            "serve",
            "-f",
            "--config",
            cfg,
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
        boot = time.monotonic()
        clock = get(port, "/clock")
        origin = clock["data_origin_ns"]
        print(f"data_origin_ns={origin} server_now_ns={clock['server_now_ns']}")

        for target in UPTIMES_S:
            while time.monotonic() - boot < target:
                time.sleep(0.05)
            now = get(port, "/clock")["server_now_ns"]
            cells = []
            for span in SPANS_MIN:
                start = max(origin, now - span * NS_PER_MIN)
                cells.append(f"{span}m={len(trades(port, start, now))}")
            up = time.monotonic() - boot
            print(f"uptime={up:6.1f}s sim_now={now} " + " ".join(cells))

        # Per-hour density across the served range: the cells above only say
        # "the recent window is empty", this says whether the tape thins out as
        # it runs forward or is uniformly sparse.
        now = get(port, "/clock")["server_now_ns"]
        hour = 3_600_000_000_000
        counts = []
        edge = origin
        while edge < now:
            counts.append(len(trades(port, edge, min(edge + hour, now))))
            edge += hour
        print("trades per sim hour from origin: " + " ".join(str(c) for c in counts))

        # Density over the whole served range, as a baseline for the cells above.
        now = get(port, "/clock")["server_now_ns"]
        full = trades(port, origin, now)
        hours = (now - origin) / 3_600_000_000_000
        print(f"served range {hours:.2f}h -> {len(full)} trades (cap 1000)")
        if len(full) >= 2:
            gaps = [
                b["ts_event"] - a["ts_event"] for a, b in zip(full, full[1:])
            ]
            gaps.sort()
            mid = gaps[len(gaps) // 2] / 1e9
            print(
                f"inter-trade gap seconds: median={mid:.1f} "
                f"p90={gaps[int(len(gaps) * 0.9)] / 1e9:.1f} "
                f"max={gaps[-1] / 1e9:.1f}"
            )
    finally:
        proc.terminate()
        proc.wait(timeout=10)


if __name__ == "__main__":
    main()
