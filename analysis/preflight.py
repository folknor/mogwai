#!/usr/bin/env python3
"""One-pass preflight over the BTCUSDT monthly spot trades corpus.

Step 3 of the order of work in `notes/sampling-frame-preregistration.md`. For
each archive it establishes, in a SINGLE streaming traversal:

  - timestamp resolution, as the digit width of the epoch column
  - the six file contracts of report section 11.1
  - inferred-parent group-size distributions under BOTH grouping rules
  - malformed rows, ordering regressions, duplicates, coverage boundaries

It computes NO target metrics. Not one F1 or F2 value is calculated or written,
because the confirmed span and the calibration boundary must be fixed before any
target exists. The boundary is in any case determined by month COUNT and
chronology alone - `PREREGISTERED_SPLIT_MONTHS = 12`, first twelve calibrate -
so no statistic produced here can move it. That is belt and braces, deliberately.

Group-size DISTRIBUTIONS are diagnostics required by preregistration section 9,
so the histogram is emitted. Derived cadence statistics are not: the mean group
size IS `children_mean`, an F2 target, and this stage does not produce targets.

Three modes:

    validate   integrated pass over ONE month, asserted against the standalone
               instruments; run this before trusting the batch
    run        the batch, resumable, atomic per month
    report     summarize completed per-month results and propose the span

Usage:
    python3 -u analysis/preflight.py validate
    python3 -u analysis/preflight.py run
    python3 -u analysis/preflight.py report
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import zipfile
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DEST = ROOT / "research/market-data"
OUT = ROOT / "analysis/preflight"

SYMBOL = "BTCUSDT"
SPAN = ((2025, 1), (2026, 7))

# Bumping this invalidates every cached per-month result. Resume compares it, so
# a changed estimator cannot be silently mixed with results from the old one.
SCHEMA_VERSION = 2

# Expected microsecond epoch width. 13 digits is millisecond, 16 microsecond.
MICROSECOND_DIGITS = 16

# The documented spot layout. Position, not name, is what the parser uses, so
# the header is checked AGAINST this rather than trusted to define it.
EXPECTED_COLUMNS = ("id", "price", "qty", "quote_qty", "time", "is_buyer_maker")
IDX_ID, IDX_PRICE, IDX_TIME, IDX_SIDE = 0, 1, 4, 5

# Known-good values for `validate`, taken from analysis/cadence.json, which was
# produced by the standalone probe_binance_trades instrument over this exact
# archive. Asserting against recorded integers is far stronger than eyeballing
# two outputs side by side.
VALIDATION_MONTH = (2026, 6)
VALIDATION_EXPECT = {
    "rows": 128_668_052,
    "parents_timestamp_and_side": 15_154_297,
    "parents_timestamp_only": 15_149_814,
    "timestamp_digits": (16, 16),
}

# Group-size histogram is kept exactly up to this size and pooled above it, so a
# pathological month cannot balloon the result file.
HIST_CAP = 256


def months(start, end):
    y, m = start
    while (y, m) <= end:
        yield y, m
        m += 1
        if m == 13:
            y, m = y + 1, 1


def archive_path(year: int, month: int) -> Path:
    return DEST / f"{SYMBOL}-trades-{year:04d}-{month:02d}.zip"


def result_path(year: int, month: int) -> Path:
    return OUT / f"{SYMBOL}-{year:04d}-{month:02d}.json"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def archive_identity(path: Path, rehash: bool = False) -> dict:
    """What ties a cached result to the archive it was computed from.

    NOT a re-hash by default, deliberately. Every archive in this corpus was
    verified twice already - inline at fetch and by a separate `binance_archive
    verify` pass - and its published digest is retained in the `.CHECKSUM`
    sidecar. Hashing again immediately before decompressing re-reads all 17.1 GB
    to re-establish something already established, and doubles the run's disk
    traffic to 34 GB, which is exactly the pressure that pushes the worker pool
    from CPU-bound into IO-bound.

    Integrity on the scan path is not lost by skipping it: `zipfile` validates
    CRC32 per member as the stream is read to completion, so a scan that
    finishes has already proven its archive's bytes are intact.

    What the identity must actually catch is a cached result belonging to a
    DIFFERENT archive than the one on disk now. Published digest plus size plus
    mtime does that for accidental replacement, truncation or re-download, at
    the cost of one `stat`. It would not catch a deliberate swap of both ZIP and
    sidecar, which is outside any threat model for immutable vendor artifacts in
    a gitignored directory - and `--rehash` covers that case on demand.
    """
    stat = path.stat()
    sidecar = path.with_suffix(path.suffix + ".CHECKSUM")
    published = None
    if sidecar.exists():
        published = sidecar.read_text().split()[0].strip().lower()
    return {
        "published_sha256": published,
        "recomputed_sha256": sha256_of(path) if rehash else None,
        "size": stat.st_size,
        "mtime_ns": stat.st_mtime_ns,
    }


def percentile(hist: dict[int, int], total: int, q: float) -> int:
    """Rank-based percentile straight off a size histogram."""
    if total == 0:
        return 0
    target = q * total
    seen = 0
    for size in sorted(hist):
        seen += hist[size]
        if seen >= target:
            return size
    return max(hist) if hist else 0


def _byte_lines(stream, chunk_size=1 << 22):
    """Newline-split lines from a binary stream, no per-row UTF-8 decode."""
    tail = b""
    read = stream.read
    while True:
        chunk = read(chunk_size)
        if not chunk:
            break
        lines = (tail + chunk).split(b"\n")
        tail = lines.pop()
        yield from lines
    if tail:
        yield tail


def scan(path: Path) -> dict:
    """The single traversal. Everything below comes from one pass over the rows.

    The hot loop works on raw bytes (every field is ASCII) and fast-paths the
    dominant row shape - 6 fields, 16 timestamp digits, literal true/false.
    A row whose side is not exactly true or false is malformed, not defaulted.
    `validate` asserts the counts stay bit-exact against cadence.json.
    """
    rows = 0
    malformed = 0
    field_counts: dict[int, int] = {}
    digit_counts: dict[int, int] = {}
    fields_expected = 0  # fast-path tally for the documented 6-column layout
    digits_expected = 0  # fast-path tally for the microsecond digit width

    header: list[str] | None = None

    first_time = None
    last_time = None
    time_regressions = 0
    id_regressions = 0
    id_ties = 0
    id_gaps = 0
    prev_time = None
    prev_id = None

    adjacent_dupe_rows = 0
    prev_raw = None

    time_ties = 0
    tie_run = 0
    max_tie_run = 0

    # Two grouping rules, tracked simultaneously. Only COUNTS and the size
    # histogram are kept; no cadence statistic is derived. The timestamp+side
    # key is held as two scalars so no tuple is allocated per row.
    key_stamp = None
    key_flag = None
    key_time = None
    size_side = 0
    size_time = 0
    parents_side = 0
    parents_time = 0
    hist_side: dict[int, int] = {}
    hist_time: dict[int, int] = {}

    n_expected = len(EXPECTED_COLUMNS)
    cap = HIST_CAP
    over = HIST_CAP + 1

    started = time.time()
    with zipfile.ZipFile(path) as zf:
        info = zf.getinfo(zf.namelist()[0])
        member = info.filename
        with zf.open(info) as raw_stream:
            for line in _byte_lines(raw_stream):
                if line.endswith(b"\r"):
                    line = line.rstrip(b"\r")
                if not line:
                    continue
                parts = line.split(b",")
                n_fields = len(parts)
                # One parse attempt per field instead of isdigit-then-int
                # double scans; a short row surfaces as IndexError. The header
                # is excluded from field_counts - it is not a data row and
                # stable_field_count is a fail-closed diagnostic about data
                # rows - but malformed rows still count.
                try:
                    trade_id = int(parts[IDX_ID])
                    stamp_text = parts[IDX_TIME]
                    stamp = int(stamp_text)
                    side_text = parts[IDX_SIDE]
                except (ValueError, IndexError):
                    if header is None and rows == 0:
                        header = [p.decode("utf-8") for p in parts]
                        continue
                    field_counts[n_fields] = field_counts.get(n_fields, 0) + 1
                    malformed += 1
                    continue
                if side_text == b"True":
                    side = True
                elif side_text == b"False":
                    side = False
                elif side_text == b"true":
                    side = True
                elif side_text == b"false":
                    side = False
                else:
                    # Anything but a literal true/false (either casing, as the
                    # archives write capitalized) is malformed, not a silent
                    # False: the row is excluded and the fail-closed
                    # malformed_rows diagnostic will flag the month.
                    field_counts[n_fields] = field_counts.get(n_fields, 0) + 1
                    malformed += 1
                    continue

                rows += 1
                if n_fields == n_expected:
                    fields_expected += 1
                else:
                    field_counts[n_fields] = field_counts.get(n_fields, 0) + 1
                if len(stamp_text) == MICROSECOND_DIGITS:
                    digits_expected += 1
                else:
                    digit_counts[len(stamp_text)] = (
                        digit_counts.get(len(stamp_text), 0) + 1
                    )

                if line == prev_raw:
                    adjacent_dupe_rows += 1
                prev_raw = line

                if first_time is None:
                    first_time = stamp

                if prev_time is not None:
                    if stamp < prev_time:
                        time_regressions += 1
                    elif stamp == prev_time:
                        time_ties += 1
                        tie_run += 1
                        if tie_run > max_tie_run:
                            max_tie_run = tie_run
                    else:
                        tie_run = 0
                prev_time = stamp

                if prev_id is not None:
                    if trade_id < prev_id:
                        id_regressions += 1
                    elif trade_id == prev_id:
                        id_ties += 1
                    elif trade_id - prev_id > 1:
                        id_gaps += 1
                prev_id = trade_id

                if stamp != key_stamp or side != key_flag:
                    if size_side:
                        parents_side += 1
                        bucket = size_side if size_side <= cap else over
                        hist_side[bucket] = hist_side.get(bucket, 0) + 1
                    key_stamp = stamp
                    key_flag = side
                    size_side = 0
                size_side += 1

                if stamp != key_time:
                    if size_time:
                        parents_time += 1
                        bucket = size_time if size_time <= cap else over
                        hist_time[bucket] = hist_time.get(bucket, 0) + 1
                    key_time = stamp
                    size_time = 0
                size_time += 1

    last_time = prev_time
    if size_side:
        parents_side += 1
        bucket = size_side if size_side <= cap else over
        hist_side[bucket] = hist_side.get(bucket, 0) + 1
    if size_time:
        parents_time += 1
        bucket = size_time if size_time <= cap else over
        hist_time[bucket] = hist_time.get(bucket, 0) + 1
    if fields_expected:
        field_counts[n_expected] = field_counts.get(n_expected, 0) + fields_expected
    if digits_expected:
        digit_counts[MICROSECOND_DIGITS] = (
            digit_counts.get(MICROSECOND_DIGITS, 0) + digits_expected
        )

    lo = min(digit_counts) if digit_counts else 0
    hi = max(digit_counts) if digit_counts else 0

    return {
        "schema_version": SCHEMA_VERSION,
        "member": member,
        "uncompressed_bytes": info.file_size,
        "rows": rows,
        "elapsed_s": round(time.time() - started, 1),
        "contract_1_header": {
            "present": header is not None,
            "columns": header,
            "matches_expected_positions": (
                header is not None
                and len(header) >= len(EXPECTED_COLUMNS)
                and header[IDX_TIME].strip().lower() in ("time", "transact_time")
                and "buyer" in header[IDX_SIDE].strip().lower()
            ),
        },
        "contract_2_layout": {
            "field_counts": field_counts,
            "stable_field_count": len(field_counts) == 1,
            "timestamp_digit_counts": digit_counts,
            "timestamp_digits_min": lo,
            "timestamp_digits_max": hi,
            "uniform_resolution": lo == hi,
            "is_microsecond": lo == hi == MICROSECOND_DIGITS,
        },
        "contract_3_timestamps": {
            # Spot trades carries ONE timestamp column, so there is no
            # transaction-versus-publication distinction available here. That is
            # a property of the data, recorded rather than resolved by guessing.
            "timestamp_columns": 1,
            "has_event_time": False,
        },
        "contract_4_ordering": {
            "time_regressions": time_regressions,
            "id_regressions": id_regressions,
            "sorted": time_regressions == 0 and id_regressions == 0,
        },
        "contract_5_duplicates": {
            "adjacent_identical_rows": adjacent_dupe_rows,
            "id_ties": id_ties,
            "id_gaps_gt_one": id_gaps,
            "timestamp_ties": time_ties,
            # Reported in ROWS: a run of N equal stamps has N-1 adjacent ties,
            # which is what the loop counts, so convert at the boundary.
            "max_timestamp_tie_run": max_tie_run + 1 if max_tie_run else 0,
        },
        "contract_6_coverage": {
            "first_time": first_time,
            "last_time": last_time,
        },
        "malformed_rows": malformed,
        "group_sizes": {
            "timestamp_and_side": {
                "parents": parents_side,
                "median": percentile(hist_side, parents_side, 0.50),
                "p95": percentile(hist_side, parents_side, 0.95),
                "p999": percentile(hist_side, parents_side, 0.999),
                "histogram_capped_at": HIST_CAP,
                "histogram": {str(k): v for k, v in sorted(hist_side.items())},
            },
            "timestamp_only": {
                "parents": parents_time,
                "median": percentile(hist_time, parents_time, 0.50),
                "p95": percentile(hist_time, parents_time, 0.95),
                "p999": percentile(hist_time, parents_time, 0.999),
                "histogram_capped_at": HIST_CAP,
                "histogram": {str(k): v for k, v in sorted(hist_time.items())},
            },
        },
    }


def diagnostics_verdict(result: dict) -> tuple[bool, list[str]]:
    """Preregistration section 9. A month failing any of these FAILS CLOSED."""
    problems = []
    layout = result["contract_2_layout"]
    if not layout["uniform_resolution"]:
        problems.append(
            "mixed timestamp resolution inside the month: "
            f"digits {layout['timestamp_digits_min']}..{layout['timestamp_digits_max']}"
        )
    if not layout["is_microsecond"]:
        problems.append(
            f"not microsecond stamped: digits {layout['timestamp_digits_min']}"
        )
    if not layout["stable_field_count"]:
        problems.append(f"unstable field count: {layout['field_counts']}")
    if result["malformed_rows"]:
        problems.append(f"{result['malformed_rows']} malformed rows")
    if not result["contract_4_ordering"]["sorted"]:
        problems.append(
            f"ordering regressions: {result['contract_4_ordering']}"
        )
    if result["rows"] == 0:
        problems.append("no rows")
    return (not problems), problems


def load_result(year: int, month: int, identity: dict) -> dict | None:
    """Resume only from a COMPLETE, schema-matched result tied to this archive.

    The `complete` check is deliberately independent of identity: a result can
    belong to the right archive and still be half-written. Those are different
    failures and neither subsumes the other.
    """
    path = result_path(year, month)
    if not path.exists():
        return None
    try:
        payload = json.loads(path.read_text())
    except json.JSONDecodeError:
        return None
    if payload.get("schema_version") != SCHEMA_VERSION:
        return None
    if not payload.get("complete"):
        return None
    cached = payload.get("archive_identity")
    if not cached:
        return None
    # Every recorded component must match. A recomputed digest is compared only
    # when the cached result carries one, so `--rehash` strengthens a result
    # rather than invalidating every result written without it.
    for field in ("published_sha256", "size", "mtime_ns"):
        if cached.get(field) != identity.get(field):
            return None
    if cached.get("recomputed_sha256") and identity.get("recomputed_sha256"):
        if cached["recomputed_sha256"] != identity["recomputed_sha256"]:
            return None
    return payload


def write_result(year: int, month: int, payload: dict) -> None:
    """Atomic: a kill mid-write cannot leave a half-written result behind."""
    OUT.mkdir(parents=True, exist_ok=True)
    path = result_path(year, month)
    tmp = path.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(payload, indent=2, sort_keys=True))
    os.replace(tmp, path)


def mode_validate() -> int:
    year, month = VALIDATION_MONTH
    path = archive_path(year, month)
    print(f"validating the integrated pass against standalone results")
    print(f"archive {path.name}\n", flush=True)
    result = scan(path)

    checks = []
    checks.append(
        ("row count matches probe_binance_trades",
         result["rows"] == VALIDATION_EXPECT["rows"],
         f"{result['rows']:,} vs {VALIDATION_EXPECT['rows']:,}")
    )
    checks.append(
        ("timestamp+side parents match cadence.json",
         result["group_sizes"]["timestamp_and_side"]["parents"]
         == VALIDATION_EXPECT["parents_timestamp_and_side"],
         f"{result['group_sizes']['timestamp_and_side']['parents']:,} vs "
         f"{VALIDATION_EXPECT['parents_timestamp_and_side']:,}")
    )
    checks.append(
        ("timestamp-only parents match cadence.json",
         result["group_sizes"]["timestamp_only"]["parents"]
         == VALIDATION_EXPECT["parents_timestamp_only"],
         f"{result['group_sizes']['timestamp_only']['parents']:,} vs "
         f"{VALIDATION_EXPECT['parents_timestamp_only']:,}")
    )
    digits = (
        result["contract_2_layout"]["timestamp_digits_min"],
        result["contract_2_layout"]["timestamp_digits_max"],
    )
    checks.append(
        ("timestamp digits match binance_archive.transition",
         digits == VALIDATION_EXPECT["timestamp_digits"],
         f"{digits} vs {VALIDATION_EXPECT['timestamp_digits']}")
    )
    ok, problems = diagnostics_verdict(result)
    checks.append(("month passes its own diagnostics", ok, "; ".join(problems)))

    failures = 0
    for name, passed, detail in checks:
        if not passed:
            failures += 1
        print(f"  {'ok  ' if passed else 'FAIL'}  {name}   {detail}", flush=True)
    print()
    print(f"elapsed {result['elapsed_s']}s for {result['rows']:,} rows")
    print("VALIDATED" if not failures else "VALIDATION FAILED")
    return 1 if failures else 0


def run_month(year: int, month: int, rehash: bool = False) -> dict:
    """Worker for `run`: identity, resume check, scan, verdict, atomic write.

    Each month is independent, so the batch fans out across processes; the
    single-traversal-per-archive rule holds because each worker still makes
    exactly one pass over its own archive - and, unless `--rehash` is set, that
    pass is the ONLY read of the archive.
    """
    path = archive_path(year, month)
    identity = archive_identity(path, rehash=rehash)
    cached = load_result(year, month, identity)
    if cached:
        cached["from_cache"] = True
        return cached
    result = scan(path)
    ok, problems = diagnostics_verdict(result)
    result["archive"] = path.name
    result["archive_identity"] = identity
    result["year"] = year
    result["month"] = month
    result["diagnostics_pass"] = ok
    result["problems"] = problems
    result["complete"] = True
    write_result(year, month, result)
    result["from_cache"] = False
    return result


def mode_run(jobs: int | None = None, rehash: bool = False) -> int:
    wanted = list(months(*SPAN))
    present = []
    failed = []
    for year, month in wanted:
        if archive_path(year, month).exists():
            present.append((year, month))
        else:
            print(f"{year:04d}-{month:02d}  ARCHIVE MISSING", flush=True)
            failed.append((year, month, "archive missing"))
    # Default is deliberately conservative: 19 workers means 19 concurrent
    # archive reads plus inflates, and storage contention makes the run less
    # predictable. Going wider is an explicit --jobs choice.
    workers = max(1, min(len(present), jobs or 4))
    print(
        f"preflight over {len(wanted)} months, one traversal each, "
        f"{workers} workers\n",
        flush=True,
    )
    done = 0
    with ProcessPoolExecutor(max_workers=workers) as pool:
        futures = {
            pool.submit(run_month, year, month, rehash): (year, month)
            for year, month in present
        }
        for future in as_completed(futures):
            year, month = futures[future]
            result = future.result()
            done += 1
            ok = result["diagnostics_pass"]
            tag = "cached" if result["from_cache"] else f"{result['elapsed_s']}s"
            print(
                f"[{done:2d}/{len(present)}] {year:04d}-{month:02d}  "
                f"rows {result['rows']:,}  "
                f"parents {result['group_sizes']['timestamp_and_side']['parents']:,}  "
                f"digits {result['contract_2_layout']['timestamp_digits_min']}  "
                f"{tag}  {'pass' if ok else 'FAIL'}",
                flush=True,
            )
            if not ok:
                for problem in result["problems"]:
                    print(f"           - {problem}", flush=True)
                failed.append((year, month, "; ".join(result["problems"])))

    failed.sort()
    print()
    if failed:
        print(f"{len(failed)} month(s) FAILED preflight:")
        for year, month, why in failed:
            print(f"  {year:04d}-{month:02d}  {why}")
        return 1
    print(f"all {len(wanted)} months pass preflight")
    return 0


def mode_report() -> int:
    wanted = list(months(*SPAN))
    results = []
    for year, month in wanted:
        path = result_path(year, month)
        if not path.exists():
            print(f"{year:04d}-{month:02d}  no result yet")
            continue
        results.append(json.loads(path.read_text()))
    if len(results) != len(wanted):
        print(f"\n{len(results)}/{len(wanted)} months complete; span not confirmable yet")
        return 1

    print(f"{'month':<9} {'rows':>14} {'parents':>13} {'p95':>5} {'p999':>6} "
          f"{'maxtie':>7} {'digits':>7}  verdict")
    for r in results:
        gs = r["group_sizes"]["timestamp_and_side"]
        print(
            f"{r['year']:04d}-{r['month']:02d}  {r['rows']:>14,} {gs['parents']:>13,} "
            f"{gs['p95']:>5} {gs['p999']:>6} "
            f"{r['contract_5_duplicates']['max_timestamp_tie_run']:>7} "
            f"{r['contract_2_layout']['timestamp_digits_min']:>7}  "
            f"{'pass' if r['diagnostics_pass'] else 'FAIL'}"
        )

    # Longest contiguous run of passing months. A failure TRUNCATES rather than
    # being bridged, per preregistration section 3.
    best_start = best_len = 0
    run_start = run_len = 0
    for i, r in enumerate(results):
        if r["diagnostics_pass"]:
            if run_len == 0:
                run_start = i
            run_len += 1
            if run_len > best_len:
                best_start, best_len = run_start, run_len
        else:
            run_len = 0

    print()
    if best_len == 0:
        print("no contiguous passing span")
        return 1
    span = results[best_start:best_start + best_len]
    print(f"confirmed contiguous span: {span[0]['year']:04d}-{span[0]['month']:02d} "
          f"through {span[-1]['year']:04d}-{span[-1]['month']:02d}, {best_len} months")
    if best_len < 17:
        print(f"held-out side would be {best_len - 12}; MIN_HELD_OUT_MONTHS is 5")
    cal = span[:12]
    held = span[12:]
    print(f"calibration  {cal[0]['year']:04d}-{cal[0]['month']:02d} .. "
          f"{cal[-1]['year']:04d}-{cal[-1]['month']:02d}  ({len(cal)} months)")
    if held:
        print(f"held out     {held[0]['year']:04d}-{held[0]['month']:02d} .. "
              f"{held[-1]['year']:04d}-{held[-1]['month']:02d}  ({len(held)} months)")
    return 0


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("validate", "run", "report"))
    parser.add_argument(
        "--jobs",
        type=int,
        default=None,
        help="parallel workers for run mode; conservative default of 4",
    )
    parser.add_argument(
        "--rehash",
        action="store_true",
        help=(
            "recompute each archive SHA-256 before scanning. Off by default: "
            "the corpus is already verified twice over and zipfile checks CRC32 "
            "during the scan, so this only doubles disk traffic"
        ),
    )
    args = parser.parse_args()
    if args.mode == "run":
        sys.exit(mode_run(args.jobs, args.rehash))
    sys.exit({"validate": mode_validate, "report": mode_report}[args.mode]())


if __name__ == "__main__":
    main()
