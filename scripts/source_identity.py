#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Print the source identity brokkr stamps into the venue and lab crates.

brokkr's `[cargo_env]` runs this before every cargo that can run a build
script and sets the one line it prints as `MOGWAI_SOURCE_IDENTITY`. The
`mogwai-venue` and `mogwai-lab` build scripts rerun only when that value
changes, rather than whenever `.git/index` moves, which reran the venue script
and rebuilt `mogwai-venue` and `mogwai-cli` on every staging or status refresh.

The line carries every value either script stamps, so a build script under
brokkr shells out to nothing:

    <full HEAD sha> <clean|dirty> <commit time as YYYY-MM-DD HH:MM:SS UTC>

The dirty flag is a non-empty `git status --porcelain` over the whole
repository, untracked files included, exactly as the venue's own fallback
computes it. Each build script parses what it needs and renders `unknown` for
anything it cannot read, so a checkout git cannot describe prints the single
word `unknown` here rather than failing: brokkr treats a non-zero exit as
fatal for the whole run, and a build outside a checkout degrades to `unknown`
anyway. The root comes from this file's own location, not the working
directory, so a `--commit` worktree build describes that worktree.
"""

import os
import pathlib
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent


def git(*args: str) -> str | None:
    try:
        out = subprocess.run(
            ["git", "-C", str(ROOT), *args],
            capture_output=True,
            text=True,
            env={**os.environ, "TZ": "UTC"},
        )
    except OSError:
        return None
    return out.stdout.strip() if out.returncode == 0 else None


def main() -> int:
    sha = git("rev-parse", "HEAD")
    commit_time = git("log", "-1", "--date=format-local:%Y-%m-%d %H:%M:%S UTC", "--format=%cd")
    # `--no-optional-locks` keeps `git status` from refreshing `.git/index`.
    status = git("--no-optional-locks", "status", "--porcelain")
    if not sha or not commit_time or status is None:
        print("unknown")
        return 0
    print(f"{sha} {'dirty' if status else 'clean'} {commit_time}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
