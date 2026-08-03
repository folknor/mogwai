#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Does a venue whose launcher dies instantly go on serving?

FINDINGS #8 reports that it does: the `launcher died during startup` guard
compares getppid() before and after arming PR_SET_PDEATHSIG, so a launcher that
is already gone at the first sample reads 1 twice, sees no change, and boots.

What that report does not separate is whether STDOUT closes the same hole. The
launcher contract says a launcher captures stdout, and the venue writes its
readiness line there after warmup - so a dead launcher's closed read end should
make that write fail with EPIPE and take the venue down anyway, just later.

This probe runs both arrangements against the real binary:

  piped    - the launcher captures stdout, then dies immediately. If EPIPE
             covers the gap, the venue must be gone.
  inherit  - the launcher lets the venue inherit its stdout, then dies. Nothing
             closes, so only the guard could catch it.

Usage: python3 scripts/probe_orphan_guard.py [path-to-mogwai]
"""

import json
import os
import subprocess
import sys
import time

BIN = sys.argv[1] if len(sys.argv) > 1 else "target/release/mogwai"
SETTLE_S = 6.0


def spawn_and_die(pipe_stdout: bool, announce_pid: bool = False) -> int:
    """Fork a launcher that spawns the venue and exits at once.

    The intermediate is a real process rather than a thread, because
    PR_SET_PDEATHSIG watches the parent THREAD and we want the parent gone
    entirely - the zero-delay case from the report.
    """
    read_fd, write_fd = os.pipe()
    pid = os.fork()
    if pid == 0:
        os.close(read_fd)
        stdout = subprocess.PIPE if pipe_stdout else None
        argv = [BIN, "serve", "--duration", "120s"]
        if announce_pid:
            # What the shipped launcher passes. The venue can then check it
            # still HAS this parent, instead of inferring a death from a change
            # it cannot see when the launcher was already gone.
            argv += ["--launcher-pid", str(os.getpid())]
        child = subprocess.Popen(
            argv,
            stdout=stdout,
            stderr=subprocess.DEVNULL,
        )
        os.write(write_fd, json.dumps({"venue": child.pid}).encode())
        os.close(write_fd)
        # Exit NOW, before the venue can reach main and sample its parent.
        os._exit(0)

    os.close(write_fd)
    with os.fdopen(read_fd) as handle:
        venue_pid = json.loads(handle.read())["venue"]
    os.waitpid(pid, 0)
    return venue_pid


def alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def ppid_of(pid: int) -> str:
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("PPid:"):
                    return line.split()[1]
    except FileNotFoundError:
        return "-"
    return "?"


def main() -> None:
    print(f"binary: {BIN}")
    print(f"{'arrangement':<16} {'venue pid':>9} {'alive':>6} {'ppid':>5}  verdict")
    failures = 0
    arrangements = (
        # Contract-following launcher: stdout captured, so the readiness write
        # lands on a pipe whose read end died with it.
        ("piped", True, False),
        # Neither guard: inherits stdout AND does not identify itself.
        ("inherit", False, False),
        # Identifies itself, which is what the shipped launcher does. This is
        # the only arrangement the guard itself can catch.
        ("inherit+pid", False, True),
    )
    for label, pipe_stdout, announce_pid in arrangements:
        venue = spawn_and_die(pipe_stdout, announce_pid)
        # Longer than a default warmup, so the venue has reached the point where
        # it would have written its readiness line and started serving.
        time.sleep(SETTLE_S)
        still = alive(venue)
        verdict = "ORPHAN SERVING" if still else "reaped"
        if still:
            failures += 1
        print(f"{label:<16} {venue:>9} {str(still):>6} {ppid_of(venue):>5}  {verdict}")
        if still:
            os.kill(venue, 9)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
