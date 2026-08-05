#!/usr/bin/env python3
"""Micro-benchmark: bytes.split row parsing versus a delimiter-index parser.

Settles the preflight.py hot-loop question empirically rather than by
assumption: the scan consumes only fields 0, 4 and 5 of each 6-field row, so
does splitting the whole line (one C call, six slice allocations) beat four
find() calls plus three slices? Both variants also produce the field count,
which the split gets for free and the find variant must count.

Run: python3 -u analysis/bench_rowparse.py
"""

import time

N_ROWS = 1_000_000


def make_sample() -> list[bytes]:
    rows = []
    trade_id = 5_000_000_000
    stamp = 1_750_000_000_000_000
    for i in range(N_ROWS):
        trade_id += 1
        stamp += 7919 if i % 9 else 0
        side = b"true" if i % 3 else b"false"
        rows.append(
            b"%d,104250.1,0.00512,533.76,%d,%s" % (trade_id, stamp, side)
        )
    return rows


def variant_split(rows: list[bytes]) -> int:
    total = 0
    for line in rows:
        parts = line.split(b",")
        n_fields = len(parts)
        trade_id = int(parts[0])
        stamp = int(parts[4])
        side = parts[5] == b"true"
        total += n_fields + (stamp & 1) + side + (trade_id & 1)
    return total


def variant_find(rows: list[bytes]) -> int:
    total = 0
    for line in rows:
        find = line.find
        a = find(b",")
        b = find(b",", a + 1)
        c = find(b",", b + 1)
        d = find(b",", c + 1)
        e = find(b",", d + 1)
        n_fields = line.count(b",") + 1
        trade_id = int(line[:a])
        stamp = int(line[d + 1:e])
        side = line[e + 1:] == b"true"
        total += n_fields + (stamp & 1) + side + (trade_id & 1)
    return total


def main() -> None:
    rows = make_sample()
    for name, fn in (("split", variant_split), ("find", variant_find)):
        best = None
        checksum = None
        for _ in range(3):
            started = time.perf_counter()
            checksum = fn(rows)
            elapsed = time.perf_counter() - started
            if best is None or elapsed < best:
                best = elapsed
        rate = N_ROWS / best / 1e6
        print(f"{name:>6}: best {best:.3f}s  {rate:.2f}M rows/s  checksum {checksum}")


if __name__ == "__main__":
    main()
