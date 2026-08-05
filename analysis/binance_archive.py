#!/usr/bin/env python3
"""Survey and verify Binance spot archives without parsing market data.

Stdlib only, like every script in this folder. Four modes, three of which touch
the network and NONE of which bulk-downloads a monthly trades archive:

    index        HEAD every candidate monthly archive: coverage and byte sizes
    transition   locate the millisecond-to-microsecond boundary via DAILY files
    verify       fetch missing .CHECKSUM files for held archives and verify
    fetch        the bulk downloader - BLOCKED until the fidelity gate is ruled on

`notes/sampling-frame-preregistration.md` section 3.3 authorizes index,
transition and verify. `fetch` refuses to run without an explicit unblock flag so
that the authorization boundary is enforced by the tool and not only by memory.

Nothing here transforms an archive. Files land byte-for-byte beside their
published checksum, which is the discipline the 2024-03-30 pair established and
the 2026-06 archives skipped.
"""

from __future__ import annotations

import argparse
import hashlib
import io
import sys
import urllib.error
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "research/market-data"

BASE = "https://data.binance.vision/data/spot"
UA = {"User-Agent": "mogwai-analysis/1.0 (offline research; stdlib urllib)"}

# BTCUSDT began trading in August 2017. Probing from a year earlier costs a
# handful of 404s and means the tool discovers coverage rather than assuming it.
FIRST_CANDIDATE = (2016, 1)

# The last COMPLETE calendar month relative to the preregistration's authoring
# date. Stated rather than computed from the clock: a run in a later month must
# be a deliberate edit, not a silent span change.
LAST_COMPLETE = (2026, 7)


def months(start: tuple[int, int], end: tuple[int, int]):
    y, m = start
    while (y, m) <= end:
        yield y, m
        m += 1
        if m == 13:
            y, m = y + 1, 1


def monthly_url(symbol: str, year: int, month: int) -> str:
    return f"{BASE}/monthly/trades/{symbol}/{symbol}-trades-{year:04d}-{month:02d}.zip"


def daily_url(symbol: str, year: int, month: int, day: int) -> str:
    return (
        f"{BASE}/daily/trades/{symbol}/"
        f"{symbol}-trades-{year:04d}-{month:02d}-{day:02d}.zip"
    )


def head(url: str) -> int | None:
    """Content-Length, or None when the object does not exist."""
    request = urllib.request.Request(url, method="HEAD", headers=UA)
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return int(response.headers.get("Content-Length", 0))
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        raise


def get(url: str) -> bytes:
    request = urllib.request.Request(url, headers=UA)
    with urllib.request.urlopen(request, timeout=300) as response:
        return response.read()


# ---------------------------------------------------------------------------
# index
# ---------------------------------------------------------------------------


def mode_index(symbol: str) -> None:
    print(f"monthly spot trades coverage for {symbol}")
    print(f"probing {FIRST_CANDIDATE} .. {LAST_COMPLETE}, HEAD only\n")
    present: list[tuple[int, int, int]] = []
    missing: list[tuple[int, int]] = []
    for year, month in months(FIRST_CANDIDATE, LAST_COMPLETE):
        size = head(monthly_url(symbol, year, month))
        if size is None:
            missing.append((year, month))
        else:
            present.append((year, month, size))
            print(f"  {year:04d}-{month:02d}  {size:>13,} bytes")
    if not present:
        print("no monthly archives found - check the symbol or the base URL")
        return
    total = sum(size for _, _, size in present)
    first = present[0]
    last = present[-1]
    print()
    print(f"present      {len(present)} months")
    print(f"absent       {len(missing)} of the probed range")
    print(f"first        {first[0]:04d}-{first[1]:02d}")
    print(f"last         {last[0]:04d}-{last[1]:02d}")
    print(f"total bytes  {total:,}  ({total / 1e9:.1f} GB)")
    # Contiguity matters: the preregistration wants a maximal CONTIGUOUS span,
    # so a hole is not merely a missing month, it truncates the usable run.
    holes = [f"{y:04d}-{m:02d}" for y, m in missing if (first[0], first[1]) < (y, m)]
    print(f"holes after first present month: {holes if holes else 'none'}")


# ---------------------------------------------------------------------------
# transition
# ---------------------------------------------------------------------------


def timestamp_decimals(blob: bytes) -> tuple[int, int, int]:
    """Digit-width of the `time` column across a daily archive.

    Returns (rows, min_digits, max_digits). Millisecond epochs are 13 digits and
    microsecond epochs are 16, so the width IS the resolution and no assumption
    about the vendor's changeover is needed. A file mixing both widths is the
    interesting case and is why min and max are reported separately.
    """
    rows = 0
    lo, hi = 99, 0
    with zipfile.ZipFile(io.BytesIO(blob)) as archive:
        name = archive.namelist()[0]
        with archive.open(name) as handle:
            for raw in io.TextIOWrapper(handle, encoding="utf-8"):
                line = raw.strip()
                if not line:
                    continue
                parts = line.split(",")
                if not parts[0].isdigit():
                    continue  # header row
                stamp = parts[4]
                width = len(stamp)
                rows += 1
                lo = min(lo, width)
                hi = max(hi, width)
    return rows, lo, hi


def probe_day(symbol: str, year: int, month: int, day: int):
    url = daily_url(symbol, year, month, day)
    size = head(url)
    if size is None:
        return None
    blob = get(url)
    return timestamp_decimals(blob)


def mode_transition(symbol: str, day: int) -> None:
    """Binary search the resolution boundary using one DAILY file per probe.

    A daily archive is a few tens of megabytes against a monthly archive's
    ~900 MB, so locating the boundary this way costs a fraction of one month.
    """
    print(f"locating the timestamp resolution boundary for {symbol}")
    print(f"one daily probe per month, day {day:02d}, binary search\n")

    cache: dict[tuple[int, int], tuple[int, int, int] | None] = {}

    def width_at(year: int, month: int):
        key = (year, month)
        if key not in cache:
            cache[key] = probe_day(symbol, year, month, day)
            result = cache[key]
            if result is None:
                print(f"  {year:04d}-{month:02d}  absent")
            else:
                rows, lo, hi = result
                kind = "microsecond" if lo >= 16 else "millisecond"
                mixed = " MIXED" if lo != hi else ""
                print(
                    f"  {year:04d}-{month:02d}  {rows:>10,} rows  "
                    f"digits {lo}..{hi}  {kind}{mixed}"
                )
        return cache[key]

    candidates = [ym for ym in months((2023, 1), LAST_COMPLETE)]
    lo_idx, hi_idx = 0, len(candidates) - 1

    first = width_at(*candidates[lo_idx])
    last = width_at(*candidates[hi_idx])
    if first is None or last is None:
        print("\nboundary probes unavailable; widen the search range")
        return
    if first[1] >= 16:
        print("\nalready microsecond at the start of the search range")
        return
    if last[1] < 16:
        print("\nstill millisecond at the end of the search range")
        return

    while hi_idx - lo_idx > 1:
        mid = (lo_idx + hi_idx) // 2
        result = width_at(*candidates[mid])
        if result is None:
            lo_idx = mid
            continue
        if result[1] >= 16:
            hi_idx = mid
        else:
            lo_idx = mid

    print()
    print(f"last millisecond month  {candidates[lo_idx][0]:04d}-{candidates[lo_idx][1]:02d}")
    print(f"first microsecond month {candidates[hi_idx][0]:04d}-{candidates[hi_idx][1]:02d}")
    print()
    print("Sampled on ONE day per month. A month containing the changeover mid-month")
    print("would read as whichever resolution that day carries, so the boundary month")
    print("itself must be probed in full before it enters a uniform-resolution span.")


# ---------------------------------------------------------------------------
# verify
# ---------------------------------------------------------------------------


def parse_checksum(text: str) -> str:
    return text.split()[0].strip().lower()


def mode_verify(names: list[str]) -> None:
    """Backfill missing .CHECKSUM files for held archives and verify them.

    An archive already on disk without a retained checksum is UNVERIFIED, which
    the preregistration treats as absent. Presence is not verification.
    """
    if not names:
        names = sorted(p.name for p in DEST.glob("*-trades-*.zip"))
    if not names:
        print(f"no trades archives under {DEST}")
        return

    failures = 0
    for name in names:
        path = DEST / name
        if not path.exists():
            print(f"{name}: ABSENT")
            failures += 1
            continue
        sidecar = path.with_suffix(path.suffix + ".CHECKSUM")
        if not sidecar.exists():
            url = published_url_for(name)
            if url is None:
                print(f"{name}: no published URL derivable, cannot verify")
                failures += 1
                continue
            try:
                blob = get(url + ".CHECKSUM")
            except urllib.error.HTTPError as exc:
                print(f"{name}: checksum fetch failed, HTTP {exc.code}")
                failures += 1
                continue
            sidecar.write_bytes(blob)
            print(f"{name}: checksum backfilled")
        expected = parse_checksum(sidecar.read_text())
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                digest.update(chunk)
        actual = digest.hexdigest()
        if actual == expected:
            print(f"{name}: OK  {path.stat().st_size:,} bytes")
        else:
            print(f"{name}: MISMATCH\n  expected {expected}\n  actual   {actual}")
            failures += 1

    print()
    print("all verified" if failures == 0 else f"{failures} archive(s) unverified")
    if failures:
        sys.exit(1)


# ---------------------------------------------------------------------------
# fetch
# ---------------------------------------------------------------------------

# The authorized span: notes/sampling-frame-preregistration.md section 3.
FETCH_SPAN = ((2025, 1), (2026, 7))


def already_complete(path: Path) -> bool:
    """True only when the ZIP and its retained checksum both exist and agree.

    Presence is not completion. A file left over from a killed run looks exactly
    like a finished one on the filesystem, which is why the checksum decides.
    """
    sidecar = path.with_suffix(path.suffix + ".CHECKSUM")
    if not path.exists() or not sidecar.exists():
        return False
    expected = parse_checksum(sidecar.read_text())
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest() == expected


def stream_to(url: str, destination: Path) -> int:
    """Download to a .part file, returning bytes written.

    Never writes the final name. A crash mid-transfer leaves a .part, which no
    later step will mistake for a finished archive.
    """
    request = urllib.request.Request(url, headers=UA)
    written = 0
    with urllib.request.urlopen(request, timeout=600) as response:
        declared = response.headers.get("Content-Length")
        with destination.open("wb") as handle:
            while True:
                chunk = response.read(1 << 20)
                if not chunk:
                    break
                handle.write(chunk)
                written += len(chunk)
    if declared is not None and written != int(declared):
        raise OSError(
            f"partial download: {written} bytes written, {declared} declared"
        )
    return written


def mode_fetch(symbol: str) -> None:
    wanted = list(months(*FETCH_SPAN))
    print(f"authorized span {FETCH_SPAN[0]} .. {FETCH_SPAN[1]}, {len(wanted)} months")
    print(f"destination {DEST}\n")

    # Coverage is confirmed for EVERY month before anything downloads. A hole
    # discovered halfway through would leave a partial corpus that looks
    # deliberate, and the preregistration requires a contiguous span.
    print("confirming coverage (HEAD only):")
    sizes: dict[tuple[int, int], int] = {}
    for year, month in wanted:
        size = head(monthly_url(symbol, year, month))
        if size is None:
            print(f"  {year:04d}-{month:02d}  ABSENT")
            print("\nunexpected coverage: refusing to fetch a non-contiguous span")
            sys.exit(1)
        sizes[(year, month)] = size
    total = sum(sizes.values())
    print(f"  all {len(wanted)} months present, {total / 1e9:.1f} GB\n")

    done = 0
    fetched = 0
    for year, month in wanted:
        name = f"{symbol}-trades-{year:04d}-{month:02d}.zip"
        path = DEST / name
        sidecar = path.with_suffix(path.suffix + ".CHECKSUM")
        part = path.with_suffix(path.suffix + ".part")

        if already_complete(path):
            print(f"{name}: already verified, skipping")
            done += 1
            continue

        url = monthly_url(symbol, year, month)
        try:
            checksum_blob = get(url + ".CHECKSUM")
        except urllib.error.HTTPError as exc:
            print(f"{name}: NO PUBLISHED CHECKSUM, HTTP {exc.code} - failing closed")
            sys.exit(1)
        sidecar.write_bytes(checksum_blob)
        expected = parse_checksum(sidecar.read_text())

        try:
            written = stream_to(url, part)
        except OSError as exc:
            print(f"{name}: {exc} - failing closed")
            part.unlink(missing_ok=True)
            sys.exit(1)

        digest = hashlib.sha256()
        with part.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1 << 20), b""):
                digest.update(chunk)
        actual = digest.hexdigest()
        if actual != expected:
            print(f"{name}: CHECKSUM MISMATCH - failing closed")
            print(f"  expected {expected}")
            print(f"  actual   {actual}")
            part.unlink(missing_ok=True)
            sys.exit(1)

        # Only now does the final name appear. Rename is the commit point.
        part.rename(path)
        fetched += 1
        done += 1
        print(f"{name}: fetched and verified, {written:,} bytes")

    print()
    print(f"{done}/{len(wanted)} months verified ({fetched} newly fetched)")
    if done != len(wanted):
        sys.exit(1)


def published_url_for(name: str) -> str | None:
    """Reconstruct the vendor URL from a retained filename.

    Only spot trades archives are derivable here; anything else returns None
    rather than guessing a path that might silently fetch the wrong object.
    """
    stem = name[: -len(".zip")] if name.endswith(".zip") else name
    parts = stem.split("-")
    if len(parts) < 3 or parts[1] != "trades":
        return None
    symbol = parts[0]
    tail = parts[2:]
    if len(tail) == 2:
        return f"{BASE}/monthly/trades/{symbol}/{stem}.zip"
    if len(tail) == 3:
        return f"{BASE}/daily/trades/{symbol}/{stem}.zip"
    return None


# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "mode", choices=("index", "transition", "verify", "fetch")
    )
    parser.add_argument("--symbol", default="BTCUSDT")
    parser.add_argument("--day", type=int, default=15)
    parser.add_argument("--name", action="append", default=[])
    parser.add_argument(
        "--i-have-ruled-on-the-fidelity-gate",
        action="store_true",
        help="unblock bulk fetch; see notes/sampling-frame-preregistration.md 3.3",
    )
    args = parser.parse_args()

    if args.mode == "index":
        mode_index(args.symbol)
    elif args.mode == "transition":
        mode_transition(args.symbol, args.day)
    elif args.mode == "verify":
        mode_verify(args.name)
    else:
        if not args.i_have_ruled_on_the_fidelity_gate:
            print(
                "bulk fetch is BLOCKED. notes/sampling-frame-preregistration.md\n"
                "section 3.3 authorizes index, transition and verify only, until\n"
                "the resolution-fidelity gate in 3.2 is computed and ruled on."
            )
            sys.exit(2)
        if args.symbol != "BTCUSDT":
            # The replication embargo is enforced here rather than remembered.
            print(
                f"refusing to fetch {args.symbol}. Only BTCUSDT is authorized;\n"
                "ETHUSDT stays uninspected and unmeasured until the BTCUSDT\n"
                "verdict is frozen. See preregistration section 2."
            )
            sys.exit(2)
        mode_fetch(args.symbol)


if __name__ == "__main__":
    main()
