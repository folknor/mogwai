#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 folknor
# SPDX-License-Identifier: AGPL-3.0-only

"""Reconnaissance pass over the Kraken trade-history dump.

Phase 0, step 0: before any statistical characterization, learn the lay of the
land cheaply. Enumerate every file with its size, confirm the CSV line format,
and recover each file's covered time span by reading only the first and last
lines (seek, do not scan). Nothing here loads a whole file, and nothing is
written to the data drive; a small manifest lands under analysis/.

Disk/memory discipline (the data is 45GB on a separate drive with little room on
the project drive):
  - Read the CSVs in place under DATA_DIR; never copy them.
  - Hold only metadata in memory - one (path, size, span) record per file.
  - Write only a small JSON manifest into the repo, never onto DATA_DIR.
"""

import json
import os
import sys

DATA_DIR = os.environ.get(
    "MOGWAI_DATA_DIR", "/home/folk/Kraken"
)
OUT = os.path.join(os.path.dirname(__file__), "recon.json")
TAIL_BYTES = 8192  # enough to capture the final newline-terminated record


def first_line(path):
    with open(path, "r", errors="replace") as f:
        for line in f:
            line = line.strip()
            if line:
                return line
    return ""


def last_line(path, size):
    if size == 0:
        return ""
    with open(path, "rb") as f:
        f.seek(max(0, size - TAIL_BYTES))
        chunk = f.read()
    text = chunk.decode("utf-8", errors="replace")
    for line in reversed(text.splitlines()):
        line = line.strip()
        if line:
            return line
    return ""


def first_field_ts(line):
    """The Kraken schema is ts,price,size; return the leading timestamp or None."""
    if not line:
        return None
    head = line.split(",", 1)[0].strip()
    try:
        return float(head)
    except ValueError:
        return None


def walk(data_dir):
    files = []
    for root, _dirs, names in os.walk(data_dir):
        for name in names:
            path = os.path.join(root, name)
            try:
                size = os.path.getsize(path)
            except OSError:
                continue
            files.append((path, size))
    return files


def main():
    if not os.path.isdir(DATA_DIR):
        print(f"DATA_DIR not found: {DATA_DIR}", file=sys.stderr)
        return 1

    files = walk(DATA_DIR)
    files.sort(key=lambda t: t[1], reverse=True)
    total_bytes = sum(s for _p, s in files)

    ext_counts = {}
    for path, _size in files:
        ext = os.path.splitext(path)[1].lower() or "<none>"
        ext_counts[ext] = ext_counts.get(ext, 0) + 1

    # Characterize the largest, the median, and the smallest file as samples.
    sample_idx = sorted({0, len(files) // 2, len(files) - 1}) if files else []
    samples = []
    for i in sample_idx:
        path, size = files[i]
        head = first_line(path)
        tail = last_line(path, size)
        samples.append(
            {
                "path": path,
                "size_bytes": size,
                "first_line": head,
                "last_line": tail,
                "first_ts": first_field_ts(head),
                "last_ts": first_field_ts(tail),
                "field_count": len(head.split(",")) if head else 0,
            }
        )

    manifest = {
        "data_dir": DATA_DIR,
        "file_count": len(files),
        "total_bytes": total_bytes,
        "total_gib": round(total_bytes / 1024**3, 2),
        "ext_counts": ext_counts,
        "largest": [
            {"path": p, "size_bytes": s, "size_mib": round(s / 1024**2, 1)}
            for p, s in files[:15]
        ],
        "samples": samples,
    }

    with open(OUT, "w") as f:
        json.dump(manifest, f, indent=2)

    print(f"files: {len(files)}   total: {manifest['total_gib']} GiB")
    print(f"extensions: {ext_counts}")
    print("largest 5:")
    for entry in manifest["largest"][:5]:
        print(f"  {entry['size_mib']:>9} MiB  {entry['path']}")
    print("samples:")
    for s in samples:
        print(f"  fields={s['field_count']}  first={s['first_line']!r}")
        print(f"            last={s['last_line']!r}")
    print(f"manifest written: {OUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
