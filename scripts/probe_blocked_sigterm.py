#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Does a launcher with SIGTERM blocked defeat PR_SET_PDEATHSIG?

A blocked signal mask is INHERITED ACROSS EXEC. The venue asks the kernel to
send it SIGTERM when its parent dies, so a launcher that happened to block
SIGTERM - a perfectly ordinary thing for a supervisor to do around a spawn -
handed the venue a mask in which that signal can never be delivered. The venue
then outlives its launcher with the parent-death signal pending forever.

Neither the pid check nor the stdout backstop covers this: both are boot-time,
and the launcher here dies AFTER a successful boot.

Usage: python3 scripts/probe_blocked_sigterm.py [path-to-mogwai]
"""

import json
import os
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.request

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/release/mogwai"
SETTLE_S = 3.0


def launch_with_blocked_sigterm() -> dict:
    """Spawn the venue from a process that has SIGTERM blocked, then exit."""
    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_fd)
        signal.pthread_sigmask(signal.SIG_BLOCK, {signal.SIGTERM})
        child = subprocess.Popen(
            [BIN, "serve", "--duration", "120s", "--launcher-pid", str(os.getpid())],
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        # Wait for a real boot, so this is the after-readiness case rather than
        # anything the startup guards could catch.
        line = child.stdout.readline()
        os.write(write_fd, line or b"{}\n")
        os.close(write_fd)
        os._exit(0)

    os.close(write_fd)
    with os.fdopen(read_fd) as handle:
        record = handle.readline()
    os.waitpid(pid, 0)
    return json.loads(record) if record.strip() else {}


def health(addr: str) -> str:
    try:
        with urllib.request.urlopen(f"http://{addr}/health", timeout=2) as resp:
            return str(resp.status)
    except (urllib.error.URLError, OSError):
        return "unreachable"


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def main() -> None:
    record = launch_with_blocked_sigterm()
    if not record:
        print("the venue never reported ready; cannot judge")
        sys.exit(2)

    venue, addr = record["pid"], record["addr"]
    time.sleep(SETTLE_S)
    still, served = alive(venue), health(addr)
    print(f"venue pid {venue} at {addr}")
    print(f"  {SETTLE_S}s after the launcher exited: alive={still} /health={served}")
    if still:
        print("  VERDICT: ORPHAN SERVING - a blocked SIGTERM defeated the parent-death signal")
        os.kill(venue, 9)
        sys.exit(1)
    print("  VERDICT: reaped")


if __name__ == "__main__":
    main()
