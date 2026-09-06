#!/usr/bin/env python3
"""Rasterise bars CSVs to a PNG so an agent can look at a tape.

The chart gate is an HTML page for a human eye. An agent reading a PNG is
the nearest thing it has to an eye, so this draws the close line and the
volume bars of each CSV as one pane, stacked, with an optional time window
to zoom on. Stdlib only: the PNG is written with zlib and struct.

    python3 analysis/tape-v2/sketch.py --out sketch.png \\
        [--from-ts NS --to-ts NS] LABEL=CSV [LABEL=CSV ...]
"""

from __future__ import annotations

import argparse
import csv
import struct
import sys
import zlib

WIDTH = 1600
PANE_H = 260
VOL_H = 60
MARGIN = 8
BG = (19, 23, 34)
GRID = (42, 46, 57)
LINE = (209, 212, 220)
UP = (38, 166, 154)
DOWN = (239, 83, 80)
TEXT = (120, 123, 134)

NS_PER_SEC = 1_000_000_000


class Canvas:
    def __init__(self, width: int, height: int) -> None:
        self.w = width
        self.h = height
        self.px = bytearray(BG * (width * height))

    def put(self, x: int, y: int, rgb: tuple[int, int, int]) -> None:
        if 0 <= x < self.w and 0 <= y < self.h:
            i = 3 * (y * self.w + x)
            self.px[i : i + 3] = bytes(rgb)

    def vline(self, x: int, y0: int, y1: int, rgb) -> None:
        if y0 > y1:
            y0, y1 = y1, y0
        for y in range(y0, y1 + 1):
            self.put(x, y, rgb)

    def hline(self, x0: int, x1: int, y: int, rgb) -> None:
        for x in range(x0, x1 + 1):
            self.put(x, y, rgb)

    def line(self, x0: int, y0: int, x1: int, y1: int, rgb) -> None:
        dx = abs(x1 - x0)
        dy = -abs(y1 - y0)
        sx = 1 if x0 < x1 else -1
        sy = 1 if y0 < y1 else -1
        err = dx + dy
        while True:
            self.put(x0, y0, rgb)
            if x0 == x1 and y0 == y1:
                return
            e2 = 2 * err
            if e2 >= dy:
                err += dy
                x0 += sx
            if e2 <= dx:
                err += dx
                y0 += sy

    def png(self) -> bytes:
        raw = bytearray()
        stride = self.w * 3
        for y in range(self.h):
            raw.append(0)
            raw += self.px[y * stride : (y + 1) * stride]

        def chunk(tag: bytes, body: bytes) -> bytes:
            crc = zlib.crc32(tag + body) & 0xFFFFFFFF
            return struct.pack(">I", len(body)) + tag + body + struct.pack(
                ">I", crc
            )

        header = struct.pack(">IIBBBBB", self.w, self.h, 8, 2, 0, 0, 0)
        return (
            b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(bytes(raw), 6))
            + chunk(b"IEND", b"")
        )


def load(path: str, lo: int | None, hi: int | None) -> list[dict]:
    out = []
    with open(path, newline="") as handle:
        for row in csv.DictReader(handle):
            ts = int(row["open_ts"])
            if lo is not None and ts < lo:
                continue
            if hi is not None and ts >= hi:
                continue
            out.append(
                {
                    "ts": ts,
                    "o": float(row["open"]),
                    "h": float(row["high"]),
                    "l": float(row["low"]),
                    "c": float(row["close"]),
                    "v": float(row["volume"]),
                    "empty": int(row["trade_count"]) == 0,
                }
            )
    return out


def draw_pane(
    cv: Canvas, top: int, rows: list[dict], lo_ts: int, hi_ts: int
) -> None:
    price_top = top + MARGIN
    price_bot = top + PANE_H - VOL_H - MARGIN
    vol_top = top + PANE_H - VOL_H
    vol_bot = top + PANE_H - 2
    span = max(hi_ts - lo_ts, 1)

    def xcoord(ts: int) -> int:
        return int((ts - lo_ts) / span * (WIDTH - 1))

    pmin = min(r["l"] for r in rows if not r["empty"])
    pmax = max(r["h"] for r in rows if not r["empty"])
    prange = max(pmax - pmin, 1e-9)
    vmax = max(r["v"] for r in rows) or 1.0

    def ycoord(p: float) -> int:
        return int(price_bot - (p - pmin) / prange * (price_bot - price_top))

    # Day boundaries at 00:00 UTC as faint verticals.
    day = (lo_ts // (86400 * NS_PER_SEC) + 1) * 86400 * NS_PER_SEC
    while day < hi_ts:
        cv.vline(xcoord(day), top, top + PANE_H - 1, GRID)
        day += 86400 * NS_PER_SEC
    cv.hline(0, WIDTH - 1, top, GRID)

    # Volume bars first so the price line overdraws them.
    for r in rows:
        x = xcoord(r["ts"])
        hgt = int(r["v"] / vmax * (vol_bot - vol_top))
        cv.vline(x, vol_bot - hgt, vol_bot, UP if r["c"] >= r["o"] else DOWN)

    # High-low wick per bar, then the close line on top.
    for r in rows:
        if r["empty"]:
            continue
        x = xcoord(r["ts"])
        cv.vline(x, ycoord(r["h"]), ycoord(r["l"]), GRID)
    prev = None
    for r in rows:
        if r["empty"]:
            prev = None
            continue
        pt = (xcoord(r["ts"]), ycoord(r["c"]))
        if prev is not None:
            cv.line(prev[0], prev[1], pt[0], pt[1], LINE)
        prev = pt


def main(argv: list[str]) -> None:
    p = argparse.ArgumentParser(prog="sketch.py")
    p.add_argument("--out", required=True)
    p.add_argument("--from-ts", type=int, default=None)
    p.add_argument("--to-ts", type=int, default=None)
    p.add_argument("panes", nargs="+", metavar="LABEL=CSV")
    args = p.parse_args(argv)

    panes = []
    for spec in args.panes:
        label, _, path = spec.partition("=")
        rows = load(path, args.from_ts, args.to_ts)
        if not rows:
            sys.exit(f"sketch: no rows in window for {path}")
        panes.append((label, rows))

    lo = args.from_ts if args.from_ts is not None else min(
        r[0]["ts"] for _, r in panes
    )
    hi = args.to_ts if args.to_ts is not None else max(
        r[-1]["ts"] for _, r in panes
    ) + 60 * NS_PER_SEC
    cv = Canvas(WIDTH, PANE_H * len(panes))
    for i, (label, rows) in enumerate(panes):
        draw_pane(cv, i * PANE_H, rows, lo, hi)
        print(
            f"pane {i}: {label}, {len(rows)} bars, "
            f"price {min(r['l'] for r in rows):.2f}.."
            f"{max(r['h'] for r in rows):.2f}"
        )
    with open(args.out, "wb") as f:
        f.write(cv.png())
    print(f"sketch: wrote {args.out}")


if __name__ == "__main__":
    main(sys.argv[1:])
