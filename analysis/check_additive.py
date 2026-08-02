#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Reject fingerprint regeneration that changes an existing value.

The queue-ahead measurement is only allowed to ADD keys to
`analysis/fingerprint.json`. Any pre-existing key that moves would re-bless the
generator's golden stream, which that measurement has no business doing, and a
`git diff --stat` cannot tell an added key from a changed one. So this compares
the committed pre-image key by key and exits non-zero on the first value that
differs. `--allow <dotted.key.path>` is repeatable and exempts exactly one key,
which is how a documentation-only edit to a `_doc` string lands without
weakening the default.
"""
import argparse
import json
import os
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
REL = "analysis/fingerprint.json"


def changed(old, new, path, allowed):
    """Yield the dotted path of every pre-existing key that moved or vanished."""
    if isinstance(old, dict):
        if not isinstance(new, dict):
            yield path or "<root>"
            return
        for key, value in old.items():
            sub = f"{path}.{key}" if path else key
            if sub in allowed:
                continue
            if key not in new:
                yield sub
            else:
                yield from changed(value, new[key], sub, allowed)
    elif old != new:
        yield path or "<root>"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--allow", action="append", default=[])
    args = parser.parse_args()
    old = json.loads(subprocess.check_output(
        ["git", "show", f"HEAD:{REL}"], cwd=REPO))
    with open(os.path.join(REPO, REL)) as handle:
        new = json.load(handle)
    moved = list(changed(old, new, "", set(args.allow)))
    if moved:
        raise SystemExit(
            "fingerprint changed pre-existing values: " + ", ".join(moved))


if __name__ == "__main__":
    main()
