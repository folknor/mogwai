#!/usr/bin/env python3
"""Fit the MNQ preset's per-instrument scalars from the delivered July TBBO.

The measurement harness of notes/mnq-tbbo-fit-spec.md (bricks H1-H2). Three
modes:

    selftest    synthetic conformance checks; no real data, no network
    preflight   fail-closed input contract over the delivered files; persists
                an artifact bound to input and sub-contract hashes
    fit         estimators, inverse solves through `brokkr run mogwai -- gen`,
                family-isolated representability probes, and the hash-bound
                artifact analysis/mnq-fit.json

The sub-contract constants below were frozen before the first real-data run
(Brick F); changing one after reading a result invalidates the acceptance
claim it supports. This harness never edits a preset: the landing is a
separate human act driven by the artifact's verdicts.

Usage:
    python3 analysis/mnq_fit.py selftest
    python3 analysis/mnq_fit.py preflight
    python3 analysis/mnq_fit.py fit
"""

from __future__ import annotations

import datetime as dt
import hashlib
import json
import math
import os
import random
import subprocess
import sys
import threading
from collections import deque
from concurrent.futures import ThreadPoolExecutor

from compression import zstd  # stdlib from Python 3.14

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER_FILE = os.path.join(ROOT, "analysis", "databento-jobs.json")
DELIVERY_DIR = os.path.join(
    ROOT, "research", "market-data", "databento", "mnqv", "2026-07.full.tbbo"
)
ARTIFACT_FILE = os.path.join(ROOT, "analysis", "mnq-fit.json")
PREFLIGHT_ARTIFACT = os.path.join(ROOT, "analysis", "out", "mnq-fit-preflight.json")
SCRATCH_DIR = os.path.join(ROOT, "analysis", "out", "mnq-fit-scratch")
SELFTEST_DIR = os.path.join(ROOT, "analysis", "out", "mnq-fit-selftest")

# ---------------------------------------------------------------------------
# The frozen sub-contract (spec section 4). Every constant here is part of
# the measurement contract; the artifact records this block's hash.
# ---------------------------------------------------------------------------

JOB_ID = "GLBX-20260805-HAPEWPABKG"
LEDGER_KEY = "mnqv|2026-07.full|tbbo"

# Input contract (4.1).
MAX_UNSIDED_SHARE = 0.01
MAX_INVALID_WIDTH_SHARE = 0.001
MIN_VALID_PARENT_QUOTE_SHARE = 0.95
MIN_DOMINANT_ID_SHARE = 1.0  # exact single-instrument purity, no-roll month
MAX_EXCLUDED_SESSIONS = 4
MIN_USABLE_SESSIONS = 18

# Prices are fixed-precision integers at 1e-9 units (pretty_px = false).
PRICE_UNITS_PER_POINT = 1_000_000_000
TICK_UNITS = 250_000_000  # 0.25 on the 1e-9 grid

# The CDT session model (permanent -300, DST unmodelled - the calendar the
# session profile was fitted against, and the one the preset ships).
UTC_OFFSET_MINUTES = -300
SESSION_OPEN_LOCAL_MIN = 17 * 60          # previous civil day, 17:00
SESSION_CLOSE_LOCAL_MIN = 16 * 60         # trade date, 16:00
HALT_START_LOCAL_MIN = 15 * 60 + 15       # 15:15
HALT_END_LOCAL_MIN = 15 * 60 + 30         # 15:30

# The frozen July 2026 session inventory (4.1): 23 weekday trade-date labels,
# July 3 excluded as the Independence Day early close, 22 expected full
# sessions. Carried as a table because the weekly calendar cannot encode
# holidays.
SESSION_INVENTORY = (
    ("2026-07-01", "full"), ("2026-07-02", "full"),
    ("2026-07-03", "early_close_excluded"),
    ("2026-07-06", "full"), ("2026-07-07", "full"), ("2026-07-08", "full"),
    ("2026-07-09", "full"), ("2026-07-10", "full"), ("2026-07-13", "full"),
    ("2026-07-14", "full"), ("2026-07-15", "full"), ("2026-07-16", "full"),
    ("2026-07-17", "full"), ("2026-07-20", "full"), ("2026-07-21", "full"),
    ("2026-07-22", "full"), ("2026-07-23", "full"), ("2026-07-24", "full"),
    ("2026-07-27", "full"), ("2026-07-28", "full"), ("2026-07-29", "full"),
    ("2026-07-30", "full"), ("2026-07-31", "full"),
)
EXPECTED_FULL_SESSIONS = 22

# Evaluation budgets (Brick G): exact UTC instants; the selftest asserts the
# ns values against the ISO strings.
SEARCH_START_ISO = "2026-07-05T22:00:00Z"
SEARCH_START_NS = 1_783_288_800_000_000_000
SEARCH_LENGTH = "7d"
SEARCH_SEEDS = (1, 2)
FINAL_START_ISO = "2026-06-30T22:00:00Z"
FINAL_START_NS = 1_782_856_800_000_000_000
FINAL_END_ISO = "2026-07-31T21:00:00Z"
FINAL_END_NS = 1_785_531_600_000_000_000
FINAL_LENGTH = "2674800s"
FINAL_SEEDS = (1, 2, 3, 4, 5, 6, 7, 8)
SUMMARY_WARMUP = "3d"

# Inverse-solve contract (4.75).
SOLVE_RELATIVE_STEP = 1e-3
SOLVE_ABSOLUTE_STEP_TICKS = 0.001  # displacement domain contains zero
SIZE_MEDIAN_DOMAIN = (0.5, 500.0)
SIZE_MEDIAN_GRID_POINTS = 64
SIZE_ROUND_FRAC_GRID = tuple(round(x * 0.01, 2) for x in range(0, 51))
DISPLACEMENT_GRID_POINTS = 32
VOL_GRID_POINTS = 32
VOL_SCALAR_DOMAIN = (1e-8, 1e-4)
SIZE_MEDIAN_IDENTIFIABILITY_FLOOR = 10.0

# Displacement contract (4.5).
DISPLACEMENT_BIN_TICKS = 0.05
MAX_WRONG_SIDE_SHARE = 0.05

# Representability tolerances (4.9), target-local, boundaries inclusive.
TOLERANCES = {
    "mean_event_duration_s": ("relative", 0.10),
    "children_mean": ("relative", 0.10),
    "children_single_frac": ("absolute", 0.05),
    "levels_mean": ("relative", 0.15),
    "size_ecdf_distance": ("ceiling", 0.10),
    "size_mean": ("relative", 0.15),
    "size_p90": ("size_tail", 0.20),
    "size_p99": ("size_tail", 0.20),
    "displacement_median": ("absolute", 0.25),
    "displacement_side_median": ("absolute", 0.50),
    "mid_rms": ("relative", 0.10),
    "width": ("exact", 0),
    "top_sizes": ("exact", 0),
    "start_price": ("exact", 0),
    # One-sided upper gates against the session-block-resampled observed
    # envelope, judged per seed; the bound is data-derived at fit time
    # under the frozen RESAMPLE_* constants, not a constant here.
    "minute_range_p99": ("envelope_upper", "resampled"),
    "minute_range_p99.9": ("envelope_upper", "resampled"),
    "minute_range_max": ("envelope_upper", "resampled"),
}

# The joint size solve (successor spec 3.2): 16 fixed sigmas, each a
# complete median solve, lexicographic winner, NO sigma refinement - the
# winning grid sigma IS the fitted value, a stated grid-resolution answer
# the representability gates judge.
SIGMA_GRID_POINTS = 16
SIGMA_GRID_DOMAIN = (0.4, 2.0)

# The minute-range envelope (successor spec 3.3): session-block resampling
# of the observed July per-minute tick ranges. One-sided upper gates at the
# per-statistic envelope level, judged PER SEED; p99.99 is computed as a
# diagnostic only.
RESAMPLE_SEED = 1
RESAMPLE_REPLICATES = 1000
RESAMPLE_SESSIONS_PER_REPLICATE = 22
RESAMPLE_ENVELOPE_LEVEL = 0.99
MINUTE_RANGE_GATES = ("p99", "p99.9", "max")

# Diagnostics ACF lags (4.8) - findings, never gates.
ACF_LAGS = (1, 10, 50)

# Fixed-horizon realized-vol diagnostics (4.7), seconds. Non-overlapping
# consecutive windows; on the observed side they are aligned to each
# session segment's calendar start (the halt and session boundaries are
# excluded by construction because a window never spans segments).
HORIZON_SECONDS = (60, 300)

# The crypto-fitted reference values the shared-shape diagnostic compares
# against (4.8): the committed fingerprint's price-sequence anchors (Kraken
# eight-pair fit, analysis/fingerprint.json) and the Binance three-pair
# cadence anchors (analysis/cadence.json), as recorded in
# DATA-PURCHASE-REPORT.md section 2.4. Findings, never gates.
REFERENCE_SHAPE = {
    "return_acf": {"1": -0.19697},
    "abs_return_acf": {"1": 0.30741, "10": 0.15649, "50": 0.12252},
    "zero_change_frac": 0.47376,
    "duration_dispersion_cv2": 4.6188,
    "duration_acf": {"1": 0.32204, "5": 0.22388},
}

# Open-minute exposure per exchange-local hour for one FULL session under
# the shipped CDT calendar: every open hour carries 60 minutes except local
# hour 15 (the 15:15-15:30 halt leaves 45) and hour 16 (the daily break,
# zero). The archive-side convention of the session fit: exposure comes
# from the calendar, never from row presence.
def open_minutes_by_local_hour() -> dict[int, int]:
    minutes = {h: 0 for h in range(24)}
    for h in range(24):
        if h == 16:
            continue
        if h == 15:
            minutes[h] = 45
            continue
        minutes[h] = 60
    return minutes

SUBCONTRACT_KEYS = [
    "JOB_ID", "LEDGER_KEY", "MAX_UNSIDED_SHARE", "MAX_INVALID_WIDTH_SHARE",
    "MIN_VALID_PARENT_QUOTE_SHARE", "MIN_DOMINANT_ID_SHARE",
    "MAX_EXCLUDED_SESSIONS", "MIN_USABLE_SESSIONS", "PRICE_UNITS_PER_POINT",
    "TICK_UNITS", "UTC_OFFSET_MINUTES", "SESSION_OPEN_LOCAL_MIN",
    "SESSION_CLOSE_LOCAL_MIN", "HALT_START_LOCAL_MIN", "HALT_END_LOCAL_MIN",
    "SESSION_INVENTORY", "EXPECTED_FULL_SESSIONS", "SEARCH_START_NS",
    "SEARCH_LENGTH", "SEARCH_SEEDS", "FINAL_START_NS", "FINAL_END_NS",
    "FINAL_LENGTH", "FINAL_SEEDS", "SUMMARY_WARMUP", "SOLVE_RELATIVE_STEP",
    "SOLVE_ABSOLUTE_STEP_TICKS", "SIZE_MEDIAN_DOMAIN",
    "SIZE_MEDIAN_GRID_POINTS", "SIZE_ROUND_FRAC_GRID",
    "DISPLACEMENT_GRID_POINTS", "VOL_GRID_POINTS", "VOL_SCALAR_DOMAIN",
    "SIZE_MEDIAN_IDENTIFIABILITY_FLOOR", "DISPLACEMENT_BIN_TICKS",
    "MAX_WRONG_SIDE_SHARE", "TOLERANCES", "ACF_LAGS", "HORIZON_SECONDS",
    "REFERENCE_SHAPE", "SIGMA_GRID_POINTS", "SIGMA_GRID_DOMAIN",
    "RESAMPLE_SEED", "RESAMPLE_REPLICATES",
    "RESAMPLE_SESSIONS_PER_REPLICATE", "RESAMPLE_ENVELOPE_LEVEL",
    "MINUTE_RANGE_GATES",
]


def subcontract_hash() -> str:
    blob = json.dumps(
        {k: globals()[k] for k in SUBCONTRACT_KEYS},
        sort_keys=True, default=list,
    ).encode()
    return hashlib.sha256(blob).hexdigest()


class Refusal(Exception):
    pass


# ---------------------------------------------------------------------------
# Sessions
# ---------------------------------------------------------------------------


INVENTORY_STATUS = dict(SESSION_INVENTORY)


def local_fields(ts_ns: int) -> tuple[str, int]:
    """(civil date string, minute of local day) on the permanent-CDT clock."""
    local_s = ts_ns // 1_000_000_000 + UTC_OFFSET_MINUTES * 60
    day = dt.datetime.fromtimestamp(local_s, dt.timezone.utc).date()
    minute = (local_s % 86_400) // 60
    return day.isoformat(), int(minute)


def assign_session(ts_ns: int) -> tuple[str | None, str | None]:
    """(trade-date label, segment) or (None, None) outside every window.

    A CME session runs prior-day 17:00 -> trade-date 16:00 local with the
    15:15-15:30 halt. Segments split at the halt so a gap, return, or
    diagnostic pair crossing it is excluded by construction: `overnight` is
    open through 15:15 of the trade date, `post_halt` is 15:30-16:00.
    """
    date, minute = local_fields(ts_ns)
    if minute >= SESSION_OPEN_LOCAL_MIN:
        trade_date = (
            dt.date.fromisoformat(date) + dt.timedelta(days=1)
        ).isoformat()
        return trade_date, "overnight"
    if minute < HALT_START_LOCAL_MIN:
        return date, "overnight"
    if minute < HALT_END_LOCAL_MIN:
        return None, None  # the halt itself
    if minute < SESSION_CLOSE_LOCAL_MIN:
        return date, "post_halt"
    return None, None  # 16:00-17:00 daily break


_MINUTE_FIELDS: dict[int, tuple] = {}


def minute_fields(ts_ns: int) -> tuple:
    """(session, segment, exchange-local hour), memoized on the UTC
    minute. The permanent -300 offset is a whole number of minutes, so
    all three are constant within one UTC minute; a month is ~44k
    distinct minutes against 35M rows, and the memo removes the per-row
    datetime and ISO-string construction of local_fields/assign_session
    without touching their semantics."""
    key = ts_ns // 60_000_000_000
    hit = _MINUTE_FIELDS.get(key)
    if hit is None:
        session, segment = assign_session(ts_ns)
        _date, local_minute = local_fields(ts_ns)
        hit = (session, segment, local_minute // 60)
        _MINUTE_FIELDS[key] = hit
    return hit


# ---------------------------------------------------------------------------
# Streaming input (4.1 stream contract)
# ---------------------------------------------------------------------------

REQUIRED_COLUMNS = (
    "ts_event", "instrument_id", "action", "side", "price", "size",
    "bid_px_00", "ask_px_00", "bid_sz_00", "ask_sz_00",
)


def iter_csv_zst(path):
    """Yield text lines from a .csv.zst, streaming, header included. A
    trailing \\r is stripped so CRLF data parses identically to LF and the
    seam comparison sees the same bytes either way. Each chunk is split
    exactly once: the per-line remainder slice this replaced re-copied
    the buffer tail for every row, quadratic per chunk."""
    with open(path, "rb") as fh:
        reader = zstd.ZstdFile(fh)
        pending = b""
        while True:
            chunk = reader.read(1 << 20)
            if not chunk:
                break
            pieces = (pending + chunk).split(b"\n")
            pending = pieces.pop()
            for raw in pieces:
                if raw.endswith(b"\r"):
                    raw = raw[:-1]
                yield raw.decode("utf-8")
        if pending:
            if pending.endswith(b"\r"):
                pending = pending[:-1]
            yield pending.decode("utf-8")


def column_indices(header_line: str) -> dict[str, int]:
    names = header_line.strip().split(",")
    missing = [c for c in REQUIRED_COLUMNS if c not in names]
    if missing:
        raise Refusal(
            f"header is missing required column(s) {', '.join(missing)}; "
            f"got: {header_line.strip()}"
        )
    return {c: names.index(c) for c in REQUIRED_COLUMNS}


def data_files(directory: str) -> list[str]:
    names = sorted(n for n in os.listdir(directory) if n.endswith(".csv.zst"))
    if not names:
        raise Refusal(f"no .csv.zst files under {directory}")
    return [os.path.join(directory, n) for n in names]


class Row:
    __slots__ = ("ts", "instrument_id", "side", "price", "size",
                 "bid_px", "ask_px", "bid_sz", "ask_sz", "book")

    def __init__(self, ts, instrument_id, side, price, size,
                 bid_px, ask_px, bid_sz, ask_sz, book):
        self.ts = ts
        self.instrument_id = instrument_id
        self.side = side
        self.price = price
        self.size = size
        self.bid_px = bid_px
        self.ask_px = ask_px
        self.bid_sz = bid_sz
        self.ask_sz = ask_sz
        self.book = book


def classify_book(bid_px: int, ask_px: int) -> str:
    if bid_px <= 0 or ask_px <= 0:
        return "nonpositive"
    if ask_px < bid_px:
        return "crossed"
    if ask_px == bid_px:
        return "locked"
    return "normal"


def parse_stream(paths):
    """Yield Row objects across the ordered files as ONE stream: 19-digit ns
    timestamps, monotone ordering across the file boundary, no duplicate row
    AT THE SEAM (any row of the previous file's FINAL TIMESTAMP recurring
    among the next file's rows at that timestamp - monotonicity confines an
    overlap to that instant, so this covers multi-row overlaps, not just an
    exact last-row/first-row match; identical adjacent rows WITHIN a file
    are legitimate market data), per-price grid membership, strict B/A/N
    sides, action T on every row."""
    prev_ts = None
    seam_ts = None          # the previous file's final timestamp
    seam_lines: set[str] = set()  # its rows at that timestamp, seam check only
    for path in paths:
        lines = iter_csv_zst(path)
        try:
            header = next(lines)
        except StopIteration:
            raise Refusal(f"{path} is empty") from None
        idx = column_indices(header)
        # Locals for the per-row hot path: a dict subscript per field per
        # row is measurable at 35M rows.
        i_ts, i_iid, i_action, i_side = (
            idx["ts_event"], idx["instrument_id"], idx["action"], idx["side"]
        )
        i_price, i_size = idx["price"], idx["size"]
        i_bpx, i_apx = idx["bid_px_00"], idx["ask_px_00"]
        i_bsz, i_asz = idx["bid_sz_00"], idx["ask_sz_00"]
        tail_ts = None
        tail_lines: set[str] = set()
        in_seam = seam_ts is not None
        for line_no, line in enumerate(lines, start=2):
            if not line.strip():
                continue
            parts = line.split(",")
            raw_ts = parts[i_ts]
            if len(raw_ts) != 19 or not raw_ts.isdigit():
                raise Refusal(
                    f"{path}:{line_no}: ts_event {raw_ts!r} is not a "
                    "19-digit nanosecond epoch"
                )
            ts = int(raw_ts)
            if prev_ts is not None and ts < prev_ts:
                raise Refusal(
                    f"{path}:{line_no}: ordering regression "
                    f"({ts} after {prev_ts}) - zero are tolerated"
                )
            if in_seam:
                if ts != seam_ts:
                    in_seam = False
                elif line in seam_lines:
                    raise Refusal(
                        f"{path}:{line_no}: duplicates a row of the previous "
                        "file's final timestamp at the boundary; the files "
                        "overlap"
                    )
            action = parts[i_action]
            if action != "T":
                raise Refusal(
                    f"{path}:{line_no}: action {action!r} is not T; the "
                    "tbbo schema carries one trade per row"
                )
            side = parts[i_side]
            if side not in ("B", "A", "N"):
                raise Refusal(
                    f"{path}:{line_no}: side {side!r} outside the DBN "
                    "alphabet B/A/N"
                )
            price = int(parts[i_price])
            bid_px = int(parts[i_bpx])
            ask_px = int(parts[i_apx])
            if price > 0 and price % TICK_UNITS != 0:
                raise Refusal(
                    f"{path}:{line_no}: price {price} is off the 0.25 grid"
                )
            if bid_px > 0 and bid_px % TICK_UNITS != 0:
                raise Refusal(
                    f"{path}:{line_no}: bid_px_00 {bid_px} is off the "
                    "0.25 grid"
                )
            if ask_px > 0 and ask_px % TICK_UNITS != 0:
                raise Refusal(
                    f"{path}:{line_no}: ask_px_00 {ask_px} is off the "
                    "0.25 grid"
                )
            yield Row(
                ts, parts[i_iid], side, price,
                int(parts[i_size]), bid_px, ask_px,
                int(parts[i_bsz]), int(parts[i_asz]),
                classify_book(bid_px, ask_px),
            )
            prev_ts = ts
            if ts != tail_ts:
                tail_ts = ts
                tail_lines = {line}
            else:
                tail_lines.add(line)
        if tail_ts is None:
            raise Refusal(
                f"{path} carries a header but no data rows; an empty "
                "intermediate file would also silently reset the seam check"
            )
        seam_ts, seam_lines = tail_ts, tail_lines


def group_parents_batch(rows: list[Row]) -> list[list[Row]]:
    """The INDEPENDENT second implementation of the frozen grouping rule,
    used by the selftest to police the streaming pass: index-based over a
    materialized list, splitting wherever the (ts, side) key changes.
    Unsided rows never enter a group AND terminate the open one - the rule
    is CONTIGUOUS runs, so B,N,B at one timestamp is two parents."""
    groups: list[list[Row]] = []
    i = 0
    while i < len(rows):
        if rows[i].side == "N":
            i += 1
            continue
        j = i
        while (
            j + 1 < len(rows)
            and rows[j + 1].ts == rows[i].ts
            and rows[j + 1].side == rows[i].side
        ):
            j += 1
        groups.append(rows[i:j + 1])
        i = j + 1
    return groups


# ---------------------------------------------------------------------------
# Identity binding (4.1): ledger + manifest + rehash, before a byte of CSV.
# ---------------------------------------------------------------------------


def sha256_file(path: str) -> str:
    digest = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            chunk = fh.read(1 << 20)
            if not chunk:
                break
            digest.update(chunk)
    return digest.hexdigest()


def verify_input(directory: str, ledger_path: str | None = None) -> dict:
    with open(ledger_path or LEDGER_FILE) as fh:
        ledger = json.load(fh)
    entry = (ledger.get("jobs") or {}).get(LEDGER_KEY)
    if not isinstance(entry, dict):
        raise Refusal(f"ledger carries no entry for {LEDGER_KEY}")
    if entry.get("state") != "downloaded":
        raise Refusal(
            f"ledger entry state is {entry.get('state')!r}, not downloaded"
        )
    if entry.get("job_id") != JOB_ID:
        raise Refusal(
            f"ledger names job {entry.get('job_id')!r}, the sub-contract "
            f"binds {JOB_ID}"
        )
    manifest_path = os.path.join(directory, "manifest.json")
    with open(manifest_path) as fh:
        manifest = json.load(fh)
    if manifest.get("job_id") != JOB_ID:
        raise Refusal(
            f"manifest names job {manifest.get('job_id')!r}, not {JOB_ID}"
        )
    ledger_files = entry.get("files") or {}
    manifest_files = manifest.get("files") or {}
    if not ledger_files:
        raise Refusal("the ledger entry carries no file inventory")
    # Ledger and manifest must AGREE EXACTLY before the disk is consulted:
    # equal key sets, equal hashes. An extra manifest entry is as much a
    # disagreement as a missing one - verified equal on the real delivery.
    if ledger_files != manifest_files:
        only_ledger = sorted(set(ledger_files) - set(manifest_files))
        only_manifest = sorted(set(manifest_files) - set(ledger_files))
        moved = sorted(
            n for n in set(ledger_files) & set(manifest_files)
            if ledger_files[n] != manifest_files[n]
        )
        raise Refusal(
            "ledger and manifest inventories disagree "
            f"(only ledger: {only_ledger}; only manifest: {only_manifest}; "
            f"hash mismatch: {moved}); the landing is not the delivery the "
            "ledger recorded"
        )
    # Presence is judged against the WHOLE directory: the ledger inventories
    # the sidecars (manifest, condition, metadata) alongside the data files,
    # and deriving presence from the csv.zst-filtered list read every
    # sidecar as missing - the first real preflight refused on exactly that.
    # Presence means a REGULAR FILE: a name shadowed by a directory or a
    # dangling symlink is not a delivered file.
    with os.scandir(directory) as entries:
        on_disk = {
            e.name for e in entries if e.is_file(follow_symlinks=False)
        }
    absent = sorted(set(ledger_files) - on_disk)
    if absent:
        raise Refusal(
            f"ledger inventory file(s) missing from disk: {absent}; the "
            "delivery is incomplete and hashing the remainder proves nothing"
        )
    hashes = {}
    for path in data_files(directory):
        name = os.path.basename(path)
        expected = ledger_files.get(name)
        if not expected:
            raise Refusal(f"{name} is on disk but not in the ledger inventory")
        actual = sha256_file(path)
        if actual != expected:
            raise Refusal(
                f"{name}: sha256 {actual} does not match the ledger's "
                f"{expected}; the bytes on disk are not the delivery"
            )
        hashes[name] = actual
    return hashes


# ---------------------------------------------------------------------------
# Preflight (4.1)
# ---------------------------------------------------------------------------


def run_preflight(directory: str, ledger_path: str | None = None) -> dict:
    hashes = verify_input(directory, ledger_path)
    rows = 0
    unsided = 0
    book_counts = {"normal": 0, "locked": 0, "crossed": 0, "nonpositive": 0}
    outside_sessions = 0
    per_session: dict[str, dict] = {}
    parent_total = 0
    parent_valid_quote = 0
    prev_key = None

    for row in parse_stream(data_files(directory)):
        rows += 1
        if row.side == "N":
            unsided += 1
        book_counts[row.book] += 1
        session, _segment, _hour = minute_fields(row.ts)
        if session is None or session not in INVENTORY_STATUS:
            outside_sessions += 1
        else:
            state = per_session.get(session)
            if state is None:
                state = per_session[session] = {
                    "rows": 0, "ids": set(), "invalid_books": 0
                }
            state["rows"] += 1
            state["ids"].add(row.instrument_id)
            if row.book != "normal":
                state["invalid_books"] += 1
        if row.side == "N":
            # An unsided row terminates the open parent: the rule is
            # CONTIGUOUS runs, so B,N,B at one timestamp is two parents.
            prev_key = None
        else:
            key = (row.ts, row.side)
            if key != prev_key:
                parent_total += 1
                if row.book == "normal":
                    parent_valid_quote += 1
                prev_key = key

    if rows == 0:
        raise Refusal("the stream carried no rows")
    unsided_share = unsided / rows
    if unsided_share > MAX_UNSIDED_SHARE:
        raise Refusal(
            f"unsided share {unsided_share:.6f} exceeds {MAX_UNSIDED_SHARE}"
        )
    invalid = rows - book_counts["normal"]
    invalid_share = invalid / rows
    if invalid_share > MAX_INVALID_WIDTH_SHARE:
        raise Refusal(
            f"invalid-width share {invalid_share:.6f} (locked+crossed+"
            f"nonpositive) exceeds {MAX_INVALID_WIDTH_SHARE}"
        )
    if parent_total == 0:
        raise Refusal("no sided parents in the stream")
    quote_share = parent_valid_quote / parent_total
    if quote_share < MIN_VALID_PARENT_QUOTE_SHARE:
        raise Refusal(
            f"valid parent-quote coverage {quote_share:.6f} is below "
            f"{MIN_VALID_PARENT_QUOTE_SHARE}"
        )

    excluded = []
    usable = []
    for label, status in SESSION_INVENTORY:
        if status != "full":
            continue
        state = per_session.get(label)
        if state is None or state["rows"] == 0:
            excluded.append([label, "absent"])
            continue
        # Exact purity (MIN_DOMINANT_ID_SHARE = 1.0): every row of a usable
        # session resolves to ONE outright instrument id. The symbol column
        # echoes the continuous label and is never the witness.
        if len(state["ids"]) > 1:
            excluded.append([label, f"impure: ids {sorted(state['ids'])}"])
            continue
        usable.append(label)
    if len(excluded) > MAX_EXCLUDED_SESSIONS:
        raise Refusal(
            f"{len(excluded)} sessions excluded ({excluded}); more than "
            f"{MAX_EXCLUDED_SESSIONS}"
        )
    if len(usable) < MIN_USABLE_SESSIONS:
        raise Refusal(
            f"only {len(usable)} usable sessions of the expected "
            f"{EXPECTED_FULL_SESSIONS}; fewer than {MIN_USABLE_SESSIONS}"
        )

    return {
        "job_id": JOB_ID,
        "file_hashes": hashes,
        "subcontract_hash": subcontract_hash(),
        "rows": rows,
        "unsided": unsided,
        "unsided_share": unsided_share,
        "book_counts": book_counts,
        "invalid_width_share": invalid_share,
        "parents_seen": parent_total,
        "valid_parent_quote_share": quote_share,
        "rows_outside_declared_sessions": outside_sessions,
        "sessions": {
            label: {
                "rows": per_session.get(label, {}).get("rows", 0),
                "invalid_books": per_session.get(label, {}).get(
                    "invalid_books", 0
                ),
                "status": (
                    "early_close_excluded"
                    if INVENTORY_STATUS[label] != "full"
                    else ("usable" if label in usable else "excluded")
                ),
            }
            for label, _status in SESSION_INVENTORY
        },
        "excluded_sessions": excluded,
        "usable_sessions": usable,
    }


def json_safe(obj):
    """Recursively replace non-finite floats with the strings "nan",
    "inf", "-inf". `json.dump` would otherwise emit the non-standard
    tokens NaN/Infinity and a strict consumer of the artifact would
    refuse to parse it."""
    if isinstance(obj, float) and not math.isfinite(obj):
        if math.isnan(obj):
            return "nan"
        return "inf" if obj > 0 else "-inf"
    if isinstance(obj, dict):
        return {k: json_safe(v) for k, v in obj.items()}
    if isinstance(obj, (list, tuple)):
        return [json_safe(v) for v in obj]
    return obj


def write_json_atomic(path: str, payload: dict) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(json_safe(payload), fh, indent=1, sort_keys=True,
                  allow_nan=False)
        fh.write("\n")
    os.replace(tmp, path)


def mode_preflight() -> None:
    payload = run_preflight(DELIVERY_DIR)
    write_json_atomic(PREFLIGHT_ARTIFACT, payload)
    print(json.dumps(
        json_safe({k: v for k, v in payload.items() if k != "sessions"}),
        indent=1, sort_keys=True, default=str,
    ))
    print(f"preflight PASS -> {PREFLIGHT_ARTIFACT}")


def require_preflight(hashes: dict,
                      artifact_path: str = PREFLIGHT_ARTIFACT) -> tuple:
    """(preflight payload, sha256 of the artifact FILE BYTES)."""
    if not os.path.exists(artifact_path):
        raise Refusal("no preflight artifact; run preflight first")
    with open(artifact_path) as fh:
        artifact = json.load(fh)
    if artifact.get("file_hashes") != hashes:
        raise Refusal(
            "preflight artifact hashes do not match the bytes on disk; "
            "re-run preflight against the current delivery"
        )
    if artifact.get("subcontract_hash") != subcontract_hash():
        raise Refusal(
            "the sub-contract changed after preflight; a rule edit cannot "
            "ride on an old preflight - re-run preflight"
        )
    return artifact, sha256_file(artifact_path)


# ---------------------------------------------------------------------------
# Observed estimators (4.2-4.8): one streaming pass over usable sessions,
# with three independent chains - cadence over ALL sided parents, quote-mid
# returns over adjacent valid-quote parents in one segment, and the
# shared-shape diagnostics over all sided parents, reset at every session
# or segment change.
# ---------------------------------------------------------------------------


class Quantiles:
    """Bounded discrete histogram with exact nearest-rank quantiles."""

    def __init__(self):
        self.counts: dict[int, int] = {}
        self.total = 0
        self.sum = 0

    def add(self, value: int) -> None:
        self.counts[value] = self.counts.get(value, 0) + 1
        self.total += 1
        self.sum += value

    def nearest_rank(self, q: float) -> int:
        if self.total == 0:
            raise Refusal("empty histogram has no quantiles")
        rank = max(1, math.ceil(q * self.total))
        seen = 0
        for value in sorted(self.counts):
            seen += self.counts[value]
            if seen >= rank:
                return value
        raise AssertionError("rank walked past the histogram")

    def mean(self) -> float:
        return self.sum / self.total if self.total else float("nan")


class Acf:
    """Streaming autocorrelation at fixed lags over one long series;
    `reset_series` empties the lag window so pairs never straddle a
    session or segment boundary.

    Each lag carries its own pair-only moments (left and right members
    separately): the value is the Pearson correlation of exactly the
    accepted (x_t, x_{t-lag}) pairs. A global mean over ALL observations
    would weight boundary observations unequally across many short
    segments and bias the estimate, especially at lags 10 and 50."""

    def __init__(self, lags):
        self.lags = tuple(lags)
        # maxlen evicts the stale head in C; a list's pop(0) shifted the
        # whole window per observation.
        self.window: deque[float] = deque(maxlen=max(self.lags))
        self.stats = {
            lag: {"n": 0, "sx": 0.0, "sy": 0.0, "sxx": 0.0, "syy": 0.0,
                  "sxy": 0.0}
            for lag in self.lags
        }

    def add(self, x: float) -> None:
        for lag in self.lags:
            if len(self.window) >= lag:
                y = self.window[-lag]
                st = self.stats[lag]
                st["n"] += 1
                st["sx"] += x
                st["sy"] += y
                st["sxx"] += x * x
                st["syy"] += y * y
                st["sxy"] += x * y
        self.window.append(x)

    def reset_series(self) -> None:
        self.window.clear()

    def value(self, lag: int) -> float:
        st = self.stats[lag]
        n = st["n"]
        if n < 2:
            return float("nan")
        mx = st["sx"] / n
        my = st["sy"] / n
        vx = st["sxx"] / n - mx * mx
        vy = st["syy"] / n - my * my
        if vx <= 0 or vy <= 0:
            return float("nan")
        cov = st["sxy"] / n - mx * my
        return cov / math.sqrt(vx * vy)


def hist_quantile(hist: dict[int, int], q: float, bin_width: float) -> float:
    """Nearest-rank quantile of a binned histogram, read at the bin
    center."""
    total = sum(hist.values())
    if total == 0:
        return float("nan")
    rank = max(1, math.ceil(q * total))
    seen = 0
    for k in sorted(hist):
        seen += hist[k]
        if seen >= rank:
            return (k + 0.5) * bin_width
    raise AssertionError("rank walked past the histogram")


def hist_median(hist: dict[int, int], bin_width: float) -> float:
    return hist_quantile(hist, 0.5, bin_width)


def dist_stats(values: list[float]) -> dict:
    """Nearest-rank median/IQR plus min and max over a small list of
    per-session values (the 4.2 stability-diagnostic shape)."""
    if not values:
        return {"median": float("nan"), "p25": float("nan"),
                "p75": float("nan"), "iqr": float("nan"),
                "min": float("nan"), "max": float("nan")}
    ordered = sorted(values)
    n = len(ordered)

    def rank(q):
        return ordered[max(1, math.ceil(q * n)) - 1]

    p25, p75 = rank(0.25), rank(0.75)
    return {"median": rank(0.5), "p25": p25, "p75": p75, "iqr": p75 - p25,
            "min": ordered[0], "max": ordered[-1]}


def segment_origin_ns(session: str, segment: str) -> int:
    """The calendar start of a session segment as a UTC epoch-ns instant:
    the previous civil day's 17:00 local for `overnight`, the trade
    date's 15:30 local for `post_halt`."""
    day = dt.date.fromisoformat(session)
    if segment == "overnight":
        local_day = day - dt.timedelta(days=1)
        local_min = SESSION_OPEN_LOCAL_MIN
    else:
        local_day = day
        local_min = HALT_END_LOCAL_MIN
    base = dt.datetime(local_day.year, local_day.month, local_day.day,
                       tzinfo=dt.timezone.utc).timestamp()
    return int(base + local_min * 60 - UTC_OFFSET_MINUTES * 60) \
        * 1_000_000_000


def nearest_rank_list(sorted_values: list, q: float):
    """Nearest-rank quantile of an ascending list."""
    if not sorted_values:
        raise Refusal("empty list has no quantiles")
    rank = max(1, math.ceil(q * len(sorted_values)))
    return sorted_values[rank - 1]


def minute_range_envelope(session_ranges: dict[str, list[int]]) -> dict:
    """The successor spec 3.3 envelope: RESAMPLE_REPLICATES replicates,
    each drawing RESAMPLE_SESSIONS_PER_REPLICATE sessions WITH replacement
    (matching one generated seed month's exposure), pooling their minute
    tick ranges, and recording nearest-rank p99, p99.9, p99.99 and the
    maximum; the envelope is the one-sided upper bound at
    RESAMPLE_ENVELOPE_LEVEL of each statistic across replicates. p99.99 is
    a DIAGNOSTIC, never a gate. Deterministic under RESAMPLE_SEED."""
    rng = random.Random(RESAMPLE_SEED)
    sessions = sorted(session_ranges)
    if not sessions:
        raise Refusal("no sessions carry minute ranges")
    stats: dict[str, list] = {"p99": [], "p99.9": [], "p99.99": [], "max": []}
    for _ in range(RESAMPLE_REPLICATES):
        pool: list[int] = []
        for _ in range(RESAMPLE_SESSIONS_PER_REPLICATE):
            pool.extend(session_ranges[rng.choice(sessions)])
        pool.sort()
        stats["p99"].append(nearest_rank_list(pool, 0.99))
        stats["p99.9"].append(nearest_rank_list(pool, 0.999))
        stats["p99.99"].append(nearest_rank_list(pool, 0.9999))
        stats["max"].append(pool[-1])
    return {
        name: nearest_rank_list(sorted(values), RESAMPLE_ENVELOPE_LEVEL)
        for name, values in stats.items()
    }


def observe(rows_iter, usable: list[str]) -> dict:
    usable_set = set(usable)
    parents = 0
    sided_rows = 0
    single_parents = 0
    level_sum = 0
    gap_sum_ns = 0
    gaps = 0
    dur_sum = 0.0
    dur_sumsq = 0.0
    sizes = Quantiles()
    width_hist: dict[int, int] = {}
    bid_sizes = Quantiles()
    ask_sizes = Quantiles()
    disp_hist = {"B": {}, "A": {}}
    disp_categories = {
        side: {"wrong_side": 0, "inside_mid": 0, "at_touch": 0,
               "beyond_touch": 0}
        for side in ("B", "A")
    }
    wrong_side = 0
    valid_quote_parents = 0
    mid_count = 0
    mid_sumsq = 0.0
    ret_acf = Acf(ACF_LAGS)
    absret_acf = Acf(ACF_LAGS)
    dur_acf = Acf((1, 5))
    zero_changes = 0
    price_changes = 0
    hour_count = {h: 0 for h in range(24)}
    hour_volume = {h: 0 for h in range(24)}
    # Per-session cadence accumulators (4.2 stability diagnostics).
    session_cad: dict[str, dict] = {}
    # Fixed-horizon realized-vol accumulators (4.7), per horizon:
    # [count, sum, sumsq] of as-of quote-mid log returns over consecutive
    # windows aligned to the segment's calendar start.
    hz_acc = {h: [0, 0.0, 0.0] for h in HORIZON_SECONDS}
    hz = {"key": None, "state": {}, "last_mid": None}
    last_trade_price_units = None  # last valid trade in usable sessions
    # [minute index, session, low units, high units] of the open minute,
    # and the completed per-session tick-range lists the envelope resamples.
    minute_current = None
    session_minute_ranges: dict[str, list[int]] = {}
    # The 4.3 size population: ALL structurally valid prints, unsided and
    # invalid-book included. Counted here so the artifact states the
    # population and its composition explicitly - the generated side has
    # neither class by construction, and the preflight caps bound the
    # resulting drift.
    pop_prints = 0
    pop_unsided = 0
    pop_invalid_book = 0

    def hz_emit(h: int, st: dict, mid: float) -> None:
        if mid is None or mid <= 0:
            return
        if st["prev"] is not None:
            r = math.log(mid / st["prev"])
            acc = hz_acc[h]
            acc[0] += 1
            acc[1] += r
            acc[2] += r * r
        st["prev"] = mid

    current = None
    prev_cadence = None    # (first_ts, session, segment) - ALL parents
    prev_mid = None        # (mid_units, session, segment) - valid-quote only
    prev_diag = None       # (price_units, session, segment) - ALL parents

    def close_parent(parent):
        nonlocal parents, sided_rows, single_parents, level_sum
        nonlocal gap_sum_ns, gaps, dur_sum, dur_sumsq
        nonlocal wrong_side, valid_quote_parents
        nonlocal mid_count, mid_sumsq
        nonlocal zero_changes, price_changes
        nonlocal prev_cadence, prev_mid, prev_diag
        parents += 1
        cad = session_cad.get(parent["session"])
        if cad is None:
            cad = session_cad[parent["session"]] = {
                "parents": 0, "rows": 0, "singles": 0, "levels": 0,
                "gaps": 0, "gap_ns": 0,
            }
        cad["parents"] += 1
        cad["rows"] += parent["rows"]
        cad["levels"] += len(parent["levels"])
        sided_rows += parent["rows"]
        if parent["rows"] == 1:
            single_parents += 1
            cad["singles"] += 1
        level_sum += len(parent["levels"])

        here = (parent["session"], parent["segment"])
        # Chain 1: cadence, every parent.
        if prev_cadence is not None and prev_cadence[1:] == here:
            gap_ns = parent["first_ts"] - prev_cadence[0]
            gap_sum_ns += gap_ns
            gaps += 1
            cad["gaps"] += 1
            cad["gap_ns"] += gap_ns
            dur_s = gap_ns / 1e9
            dur_sum += dur_s
            dur_sumsq += dur_s * dur_s
            dur_acf.add(dur_s)
        elif prev_cadence is not None:
            dur_acf.reset_series()
        prev_cadence = (parent["first_ts"], *here)

        # Chain 2: quote-mid returns, valid-quote parents only.
        if parent["book"] == "normal":
            valid_quote_parents += 1
            width_ticks = (parent["ask_px"] - parent["bid_px"]) // TICK_UNITS
            width_hist[width_ticks] = width_hist.get(width_ticks, 0) + 1
            bid_sizes.add(parent["bid_sz"])
            ask_sizes.add(parent["ask_sz"])
            mid_units = (parent["bid_px"] + parent["ask_px"]) / 2
            raw_ticks = (parent["first_price"] - mid_units) / TICK_UNITS
            signed = raw_ticks if parent["side"] == "B" else -raw_ticks
            # Touch categories (4.5) on exact integers: d2 is twice the
            # signed displacement in price units, touch2 twice the
            # half-spread; both are exact multiples of TICK_UNITS.
            d2 = 2 * parent["first_price"] - parent["bid_px"] \
                - parent["ask_px"]
            if parent["side"] == "A":
                d2 = -d2
            touch2 = parent["ask_px"] - parent["bid_px"]
            cats = disp_categories[parent["side"]]
            if d2 < 0:
                wrong_side += 1
                cats["wrong_side"] += 1
            elif d2 == touch2:
                cats["at_touch"] += 1
            elif d2 > touch2:
                cats["beyond_touch"] += 1
            else:
                cats["inside_mid"] += 1
            bin_key = math.floor(signed / DISPLACEMENT_BIN_TICKS)
            disp_hist[parent["side"]][bin_key] = (
                disp_hist[parent["side"]].get(bin_key, 0) + 1
            )
            if prev_mid is not None and prev_mid[1:] == here \
                    and prev_mid[0] > 0 and mid_units > 0:
                r = math.log(mid_units / prev_mid[0])
                mid_count += 1
                mid_sumsq += r * r
            prev_mid = (mid_units, *here)

            # Fixed-horizon windows (4.7): boundaries at origin + k*W in
            # the segment; the as-of mid at a boundary is the last valid
            # parent mid at or before it. A boundary exactly at ts_ns is
            # NOT emitted here: it stays pending until a strictly later
            # parent flushes it, so equal-timestamp parents all update the
            # boundary's as-of mid first. (A boundary coinciding with the
            # segment's final parent is dropped with the other post-final
            # boundaries.)
            ts_ns = parent["first_ts"]
            if here != hz["key"]:
                hz["key"] = here
                origin = segment_origin_ns(*here)
                hz["state"] = {
                    h: {"origin": origin, "next": origin + h * 10**9,
                        "prev": None}
                    for h in HORIZON_SECONDS
                }
                hz["last_mid"] = None
            for h in HORIZON_SECONDS:
                st = hz["state"][h]
                w_ns = h * 10**9
                if hz["last_mid"] is None and st["next"] < ts_ns:
                    # No as-of mid exists yet: dead boundaries carry no
                    # observation; jump to the first boundary >= ts.
                    k = (ts_ns - st["origin"] + w_ns - 1) // w_ns
                    st["next"] = st["origin"] + w_ns * max(1, k)
                while st["next"] < ts_ns:
                    hz_emit(h, st, hz["last_mid"])
                    st["next"] += w_ns
            hz["last_mid"] = mid_units

        # Chain 3: shared-shape diagnostics, every parent.
        if prev_diag is not None and prev_diag[1:] == here:
            price_changes += 1
            if parent["first_price"] == prev_diag[0]:
                zero_changes += 1
            if parent["first_price"] > 0 and prev_diag[0] > 0:
                r = math.log(parent["first_price"] / prev_diag[0])
                ret_acf.add(r)
                absret_acf.add(abs(r))
        elif prev_diag is not None:
            ret_acf.reset_series()
            absret_acf.reset_series()
        prev_diag = (parent["first_price"], *here)

    for row in rows_iter:
        session, segment, hour = minute_fields(row.ts)
        if session not in usable_set:
            if current is not None:
                close_parent(current)
                current = None
            continue
        # DELIBERATELY before the side and book branches (4.3): side and
        # book validity are not properties of the size process, so every
        # print's size is size evidence, the hour curves are a data
        # diagnostic that classifies nothing, and the terminal anchor needs
        # a price, not a side. The composition lands in `size_population`.
        sizes.add(row.size)
        pop_prints += 1
        if row.side == "N":
            pop_unsided += 1
        if row.book != "normal":
            pop_invalid_book += 1
        last_trade_price_units = row.price
        # Session curves bucket by EXCHANGE-LOCAL hour (the wave-1 and
        # session-fit convention), never UTC; minute_fields carries it.
        hour_count[hour] += 1
        hour_volume[hour] += row.size
        # Per-minute tick ranges, PER SESSION, for the resampled envelope
        # (successor spec 3.3): UTC minute buckets, minutes with at least
        # one print, high-low in integer ticks - the identical convention
        # the generated summary carries.
        minute = row.ts // 60_000_000_000
        if minute_current is not None and minute_current[0] == minute:
            if row.price < minute_current[2]:
                minute_current[2] = row.price
            if row.price > minute_current[3]:
                minute_current[3] = row.price
        else:
            if minute_current is not None:
                session_minute_ranges.setdefault(
                    minute_current[1], []
                ).append(
                    (minute_current[3] - minute_current[2]) // TICK_UNITS
                )
            minute_current = [minute, session, row.price, row.price]
        if row.side == "N":
            # Contiguity: an unsided row terminates the open parent.
            if current is not None:
                close_parent(current)
                current = None
            continue
        if current is not None and (
            current["ts"] == row.ts and current["side"] == row.side
        ):
            current["rows"] += 1
            current["levels"].add(row.price)
        else:
            if current is not None:
                close_parent(current)
            current = {
                "ts": row.ts, "side": row.side,
                "session": session, "segment": segment,
                "first_ts": row.ts, "first_price": row.price,
                "rows": 1, "levels": {row.price},
                "book": row.book,
                "bid_px": row.bid_px, "ask_px": row.ask_px,
                "bid_sz": row.bid_sz, "ask_sz": row.ask_sz,
            }
    if current is not None:
        close_parent(current)
    if minute_current is not None:
        session_minute_ranges.setdefault(minute_current[1], []).append(
            (minute_current[3] - minute_current[2]) // TICK_UNITS
        )
    # The envelope resamples the usable-session population EXACTLY: a
    # usable session without minute ranges, or a range block outside the
    # usable set, is an estimator defect that must refuse rather than
    # quietly change the resampling population.
    if set(session_minute_ranges) != usable_set:
        missing = sorted(usable_set - set(session_minute_ranges))
        extra = sorted(set(session_minute_ranges) - usable_set)
        raise Refusal(
            "minute-range session blocks do not match the usable set "
            f"(missing: {missing}; outside: {extra})"
        )

    if parents == 0:
        raise Refusal("no parents in usable sessions")
    # The grouping conformance gate lives in the selftest: the streaming pass
    # is compared against group_parents_batch, a genuinely independent
    # index-based implementation, over adversarial fixtures. A same-pass
    # counter would repeat this pass's own transition logic and prove
    # nothing.

    if not width_hist:
        raise Refusal("no valid-quote parents in usable sessions")
    all_disp: dict[int, int] = {}
    for h in disp_hist.values():
        for k, v in h.items():
            all_disp[k] = all_disp.get(k, 0) + v
    max_width_count = max(width_hist.values())
    width_mode = min(k for k in width_hist if width_hist[k] == max_width_count)
    width_total = sum(width_hist.values())
    width_mad = sum(
        abs(k - width_mode) * v for k, v in width_hist.items()
    ) / width_total

    per_session_cadence = {
        label: {
            "parents": c["parents"],
            "mean_event_duration_s": c["gap_ns"] / c["gaps"] / 1e9
            if c["gaps"] else float("nan"),
            "children_mean": c["rows"] / c["parents"],
            "children_single_frac": c["singles"] / c["parents"],
            "levels_mean": c["levels"] / c["parents"],
        }
        for label, c in sorted(session_cad.items())
    }
    cadence_stability = {
        metric: dist_stats([
            v[metric] for v in per_session_cadence.values()
            if math.isfinite(v[metric])
        ])
        for metric in ("mean_event_duration_s", "children_mean",
                       "children_single_frac", "levels_mean")
    }

    def category_fractions(counts: dict[str, int]) -> dict:
        total = sum(counts.values())
        return {
            **{k: (v / total if total else float("nan"))
               for k, v in counts.items()},
            "parents": total,
        }
    combined_categories = {
        k: disp_categories["B"][k] + disp_categories["A"][k]
        for k in disp_categories["B"]
    }
    cv2 = float("nan")
    if gaps > 1:
        mean_d = dur_sum / gaps
        var_d = dur_sumsq / gaps - mean_d * mean_d
        cv2 = var_d / (mean_d * mean_d) if mean_d > 0 else float("nan")
    last_price_points = (
        f"{last_trade_price_units / PRICE_UNITS_PER_POINT:.2f}"
        if last_trade_price_units is not None else None
    )

    return {
        "parents": parents,
        "sided_rows": sided_rows,
        "mean_event_duration_s": (gap_sum_ns / gaps) / 1e9
        if gaps else float("nan"),
        "children_mean": sided_rows / parents,
        "children_single_frac": single_parents / parents,
        "levels_mean": level_sum / parents,
        "size_population": {
            # The frozen 4.3 population, stated so the verdict reader sees
            # the observed-vs-generated asymmetry and its preflight-capped
            # bound here instead of discovering it in the code: the
            # generated population carries no unsided or invalid-book
            # class by construction.
            "definition": "all prints in usable sessions, unsided and "
                          "invalid-book included",
            "prints": pop_prints,
            "sided": pop_prints - pop_unsided,
            "unsided": pop_unsided,
            "valid_book": pop_prints - pop_invalid_book,
            "invalid_book": pop_invalid_book,
        },
        "size_histogram": {str(k): v for k, v in sorted(sizes.counts.items())},
        "size_mean": sizes.mean(),
        "size_quantiles": {
            f"p{int(q * 100)}": sizes.nearest_rank(q)
            for q in (0.50, 0.75, 0.90, 0.95, 0.99)
        },
        "size_floor_mass": sizes.counts.get(1, 0) / sizes.total,
        "width_hist": {str(k): v for k, v in sorted(width_hist.items())},
        "width_mode": width_mode,
        "width_modal_mass": width_hist[width_mode] / width_total,
        "width_median": nearest_rank_of(width_hist, 0.5),
        "width_p90": nearest_rank_of(width_hist, 0.90),
        "width_mad_from_mode": width_mad,
        "top_bid_median": bid_sizes.nearest_rank(0.5),
        "top_ask_median": ask_sizes.nearest_rank(0.5),
        "bid_size_histogram": {
            str(k): v for k, v in sorted(bid_sizes.counts.items())
        },
        "ask_size_histogram": {
            str(k): v for k, v in sorted(ask_sizes.counts.items())
        },
        "top_size_quantiles": {
            side: {
                f"p{int(q * 100)}": qs.nearest_rank(q)
                for q in (0.50, 0.90, 0.95, 0.99)
            }
            for side, qs in (("bid", bid_sizes), ("ask", ask_sizes))
        },
        "displacement_hist": {
            side: {str(k): v for k, v in sorted(h.items())}
            for side, h in disp_hist.items()
        },
        "displacement_median_ticks": hist_median(
            all_disp, DISPLACEMENT_BIN_TICKS
        ),
        "displacement_p90_ticks": hist_quantile(
            all_disp, 0.90, DISPLACEMENT_BIN_TICKS
        ),
        "displacement_buyer_median_ticks": hist_median(
            disp_hist["B"], DISPLACEMENT_BIN_TICKS
        ),
        "displacement_seller_median_ticks": hist_median(
            disp_hist["A"], DISPLACEMENT_BIN_TICKS
        ),
        "displacement_buyer_p90_ticks": hist_quantile(
            disp_hist["B"], 0.90, DISPLACEMENT_BIN_TICKS
        ),
        "displacement_seller_p90_ticks": hist_quantile(
            disp_hist["A"], 0.90, DISPLACEMENT_BIN_TICKS
        ),
        "displacement_fractions": {
            "combined": category_fractions(combined_categories),
            "B": category_fractions(disp_categories["B"]),
            "A": category_fractions(disp_categories["A"]),
        },
        "wrong_side_share": wrong_side / valid_quote_parents
        if valid_quote_parents else float("nan"),
        "valid_quote_parents": valid_quote_parents,
        "mid_rms": math.sqrt(mid_sumsq / mid_count)
        if mid_count else float("nan"),
        "mid_return_count": mid_count,
        "eligible_gaps": gaps,
        "last_price_points": last_price_points,
        "minute_ranges_by_session": {
            label: sorted(ranges)
            for label, ranges in sorted(session_minute_ranges.items())
        },
        "minute_range_observed": (lambda pooled: {
            "p99": nearest_rank_list(pooled, 0.99),
            "p99.9": nearest_rank_list(pooled, 0.999),
            "p99.99": nearest_rank_list(pooled, 0.9999),
            "max": pooled[-1],
        })(sorted(
            r for ranges in session_minute_ranges.values() for r in ranges
        )),
        "minute_range_envelope": minute_range_envelope(session_minute_ranges),
        "per_session_parents": {
            label: v["parents"] for label, v in per_session_cadence.items()
        },
        "per_session_cadence": per_session_cadence,
        "cadence_stability": cadence_stability,
        "horizon_vol": {
            str(h): {
                "count": acc[0], "sum": acc[1], "sumsq": acc[2],
                "rms": math.sqrt(acc[2] / acc[0]) if acc[0]
                else float("nan"),
            }
            for h, acc in hz_acc.items()
        },
        "diagnostics": build_diagnostics(
            zero_changes, price_changes, ret_acf, absret_acf, dur_acf, cv2,
            hour_count, hour_volume, len(set(usable)),
        ),
    }


def build_diagnostics(zero_changes, price_changes, ret_acf, absret_acf,
                      dur_acf, cv2, hour_count, hour_volume,
                      usable_count) -> dict:
    """4.8: shared-shape observed values BESIDE the crypto-fitted reference
    they are compared against, and the exposure-normalized count-vs-volume
    session curves with the peak-to-trough comparison. Findings, never
    gates."""
    observed_shape = {
        "zero_change_frac": zero_changes / price_changes
        if price_changes else float("nan"),
        "return_acf": {str(lag): ret_acf.value(lag) for lag in ACF_LAGS},
        "abs_return_acf": {
            str(lag): absret_acf.value(lag) for lag in ACF_LAGS
        },
        "duration_acf": {"1": dur_acf.value(1), "5": dur_acf.value(5)},
        "duration_dispersion_cv2": cv2,
    }

    def diffs(obs, ref):
        if isinstance(ref, dict):
            return {
                k: diffs(obs.get(k, float("nan")), v) for k, v in ref.items()
            }
        return obs - ref if isinstance(obs, float) and math.isfinite(obs) \
            else None

    exposure_min = open_minutes_by_local_hour()
    count_rate = {}
    volume_rate = {}
    for h in range(24):
        minutes = exposure_min[h] * usable_count
        if minutes == 0:
            continue
        count_rate[str(h)] = hour_count[h] / minutes
        volume_rate[str(h)] = hour_volume[h] / minutes

    def peak_to_trough(rates: dict) -> float:
        # The wave-1 convention: zero-rate OPEN hours stay in - an empty
        # open hour widens its own denominator, and a zero trough reads as
        # infinity rather than being silently dropped.
        values = list(rates.values())
        if not values:
            return float("nan")
        trough = min(values)
        if trough == 0:
            return float("inf")
        return max(values) / trough

    count_ptt = peak_to_trough(count_rate)
    volume_ptt = peak_to_trough(volume_rate)
    return {
        "shared_shape": {
            "observed": observed_shape,
            "reference": REFERENCE_SHAPE,
            "difference": {
                k: diffs(observed_shape[k], REFERENCE_SHAPE[k])
                for k in REFERENCE_SHAPE
            },
        },
        "count_vs_volume": {
            "count_per_open_minute_by_local_hour": count_rate,
            "volume_per_open_minute_by_local_hour": volume_rate,
            "count_peak_to_trough": count_ptt,
            "volume_peak_to_trough": volume_ptt,
            # The wave-1 quantity exactly: volume ptt over count ptt
            # (13.04 / 13.67 = 0.95 on the ten July sessions).
            "volume_over_count_ptt_ratio": volume_ptt / count_ptt
            if math.isfinite(count_ptt) and math.isfinite(volume_ptt)
            and count_ptt > 0
            else float("nan"),
            "wave1_ten_session_reference": 0.95,
        },
    }


# ---------------------------------------------------------------------------
# The generator instrument (Brick G) and inverse solves (4.75)
# ---------------------------------------------------------------------------


# Execution mechanics, not measurement contract: the walk fan-out width.
# Walks are independent subprocesses over disjoint scratch paths; the
# results are byte-identical to a serial run (CRN determinism), so this
# is deliberately OUTSIDE the sub-contract hash.
WALK_JOBS = 12

_GEN_BINARY = os.path.join(ROOT, "target", "release", "mogwai")
_GEN_DIRECT = False
_WARM_LOCK = threading.Lock()


def warm_gen_build() -> None:
    """One `brokkr run` pass so cargo's freshness check runs exactly once
    per fit. Later walks exec the release binary directly: per-walk
    `brokkr run` pays metadata discovery plus the freshness check and
    serializes every parallel walk on cargo's global lock. The tree is
    clean (require_clean_tree) and untouched for the run's duration, so
    one successful freshness pass attests the binary for the whole fit.
    If the warm pass fails, walks fall back to per-walk `brokkr run`,
    which is slower but never stale."""
    global _GEN_DIRECT
    with _WARM_LOCK:
        if _GEN_DIRECT:
            return
        proc = subprocess.run(
            ["brokkr", "run", "--release", "mogwai", "--", "gen", "--help"],
            capture_output=True, text=True, cwd=ROOT,
        )
        _GEN_DIRECT = (
            proc.returncode == 0 and os.access(_GEN_BINARY, os.X_OK)
        )


def gen_command_prefix() -> list[str]:
    if _GEN_DIRECT:
        return [_GEN_BINARY]
    return ["brokkr", "run", "--release", "mogwai", "--"]


def scratch_config_text(overrides: dict[str, object]) -> str:
    lines = ["[instrument]", 'preset = "MNQ"']
    if overrides:
        lines.append("[instrument.override]")
        for path, value in sorted(overrides.items()):
            if isinstance(value, str):
                lines.append(f'"{path}" = "{value}"')
            else:
                lines.append(f'"{path}" = {value}')
    return "\n".join(lines) + "\n"


def run_summary_subprocess(overrides: dict, seed: int, start_ns: int,
                           length: str, warmup: str) -> dict:
    """One `brokkr run mogwai -- gen --type summary` walk. The production
    runner; selftests inject fakes instead.

    Walks are cached under SCRATCH_DIR/cache, keyed by the full invocation
    plus the harness commit: the generator is deterministic (CRN), so a
    repeated evaluation is pure waste, and a crashed multi-hour fit resumes
    from the cache instead of from zero. The commit is a sound key because
    run_fit refuses to start from a dirty tree."""
    cache_key = hashlib.sha256(json.dumps(
        {"overrides": overrides, "seed": seed, "start_ns": start_ns,
         "length": length, "warmup": warmup, "commit": git_commit()},
        sort_keys=True, default=list,
    ).encode()).hexdigest()
    cache_dir = os.path.join(SCRATCH_DIR, "cache")
    cache_path = os.path.join(cache_dir, cache_key + ".json")
    if os.path.exists(cache_path):
        with open(cache_path) as fh:
            return json.load(fh)
    os.makedirs(SCRATCH_DIR, exist_ok=True)
    # Scratch paths key on the walk's cache hash, never the PID: distinct
    # concurrent walks own disjoint files by construction, where a shared
    # per-PID config raced under any parallelism and corrupted silently.
    config_path = os.path.join(
        SCRATCH_DIR, f"candidate-{cache_key[:16]}.toml"
    )
    out_path = os.path.join(SCRATCH_DIR, f"summary-{cache_key[:16]}.json")
    with open(config_path, "w") as fh:
        fh.write(scratch_config_text(overrides))
    # A stale summary from an earlier walk must never be read as this
    # walk's output if gen exits 0 without writing.
    if os.path.exists(out_path):
        os.remove(out_path)
    cmd = gen_command_prefix() + [
        "gen",
        "--config", config_path, "--type", "summary",
        "--seed", str(seed), "--start", str(start_ns),
        "--length", length, "--warmup", warmup,
        "--out", out_path,
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT)
    if proc.returncode != 0:
        raise Refusal(
            f"summary walk failed ({' '.join(cmd)}):\n{proc.stderr[-2000:]}"
        )
    with open(out_path) as fh:
        summary = json.load(fh)
    os.makedirs(cache_dir, exist_ok=True)
    tmp = f"{cache_path}.{threading.get_ident()}.tmp"
    with open(tmp, "w") as fh:
        json.dump(summary, fh)
    os.replace(tmp, cache_path)
    os.remove(config_path)
    os.remove(out_path)
    return summary


def pooled(summaries: list[dict]) -> dict:
    parents = sum(s["parents"] for s in summaries)
    sided = sum(s["sided_rows"] for s in summaries)
    singles = sum(s["single_parents"] for s in summaries)
    levels = sum(s["level_count_sum"] for s in summaries)
    gaps = sum(s["eligible_gaps"] for s in summaries)
    gap_ns = sum(s["gap_sum_ns"] for s in summaries)
    size_hist: dict[int, int] = {}
    bid_hist: dict[int, int] = {}
    ask_hist: dict[int, int] = {}
    for s in summaries:
        for src, dst in (
            (s["size_histogram"], size_hist),
            (s["bid_size_histogram"], bid_hist),
            (s["ask_size_histogram"], ask_hist),
        ):
            for k, v in src.items():
                key = int(float(k))
                dst[key] = dst.get(key, 0) + v
    mid_n = sum(s["mid_return_count"] for s in summaries)
    mid_ss = sum(s["mid_return_sumsq"] for s in summaries)
    buyer: dict[int, int] = {}
    seller: dict[int, int] = {}
    for s in summaries:
        for hist, dst in ((s["buyer_displacement_hist"], buyer),
                          (s["seller_displacement_hist"], seller)):
            for k, v in hist.items():
                # The keys are bin LEFT EDGES the generator printed from
                # index * 0.05; round(), not floor(), recovers the index
                # because 0.05 is not exactly representable (e.g. "0.15"
                # parses to 2.9999.../0.05 - floor would be off by one).
                key = round(float(k) / DISPLACEMENT_BIN_TICKS)
                dst[key] = dst.get(key, 0) + v
    width: dict[int, int] = {}
    for s in summaries:
        for k, v in s["width_ticks_histogram"].items():
            width[int(k)] = width.get(int(k), 0) + v
    horizon: dict[str, dict] = {}
    for s in summaries:
        for h, rec in (s.get("horizon_vol") or {}).items():
            dst = horizon.setdefault(
                str(h), {"count": 0, "sum": 0.0, "sumsq": 0.0}
            )
            dst["count"] += rec["count"]
            dst["sum"] += rec["sum"]
            dst["sumsq"] += rec["sumsq"]
    for rec in horizon.values():
        rec["rms"] = math.sqrt(rec["sumsq"] / rec["count"]) \
            if rec["count"] else float("nan")
    first_mid = summaries[0].get("first_book_mid") if summaries else None
    return {
        "parents": parents,
        "mean_event_duration_s": (gap_ns / gaps) / 1e9
        if gaps else float("nan"),
        "children_mean": sided / parents if parents else float("nan"),
        "children_single_frac": singles / parents if parents else float("nan"),
        "levels_mean": levels / parents if parents else float("nan"),
        "size_histogram": size_hist,
        "bid_size_histogram": bid_hist,
        "ask_size_histogram": ask_hist,
        "mid_rms": math.sqrt(mid_ss / mid_n) if mid_n else float("nan"),
        "displacement_hist": {"B": buyer, "A": seller},
        "width_histogram": width,
        "horizon_vol": horizon,
        "first_book_mid": first_mid,
    }


def size_objective(generated_hist: dict[int, int], observed: dict) -> tuple:
    """Lexicographic per 4.75: (max ECDF distance, |mean diff|,
    sum |quantile diffs|, |floor-mass diff|)."""
    gen_total = sum(generated_hist.values())
    obs_hist = {int(k): v for k, v in observed["size_histogram"].items()}
    obs_total = sum(obs_hist.values())
    if gen_total == 0 or obs_total == 0:
        return (float("inf"),) * 4
    support = sorted(set(generated_hist) | set(obs_hist))
    gen_c = obs_c = 0
    ecdf = 0.0
    for v in support:
        gen_c += generated_hist.get(v, 0)
        obs_c += obs_hist.get(v, 0)
        ecdf = max(ecdf, abs(gen_c / gen_total - obs_c / obs_total))
    gen_mean = sum(v * n for v, n in generated_hist.items()) / gen_total
    q = Quantiles()
    q.counts = dict(generated_hist)
    q.total = gen_total
    quantile_diff = sum(
        abs(q.nearest_rank(p) - observed["size_quantiles"][f"p{int(p * 100)}"])
        for p in (0.50, 0.75, 0.90, 0.95, 0.99)
    )
    floor_diff = abs(
        generated_hist.get(1, 0) / gen_total - observed["size_floor_mass"]
    )
    return (ecdf, abs(gen_mean - observed["size_mean"]), quantile_diff,
            floor_diff)


def trisect(evaluate, lo: float, hi: float, log_domain: bool,
            absolute_step: float | None = None,
            objective_threshold: float | None = None, seeds=()):
    """4.75 refinement: CLASSIC TERNARY COMPARISON with coarse-score
    seeding. Each iteration evaluates the two trisection points and keeps
    [a, m2] when f(m1) <= f(m2) (the tie keeps the left), else [m1, b] -
    the fresh interior pair alone decides the bracket, because an
    incumbent (endpoint or seed) carries no directional information and
    provably dragged the bracket off the optimum (the 3.2 reproduction).
    Seeding and ternary were never in conflict: seeds save re-evaluating
    already-paid coarse scores and keep best-ever tracking honest, while
    the survivor decision costs the same two evaluations either way. The
    returned candidate is the best point ever evaluated, smaller winning
    score ties. Parameter-specific objective thresholds may terminate
    before the bracket-width condition."""
    def xform(x):
        return math.log(x) if log_domain else x

    def unxform(x):
        return math.exp(x) if log_domain else x

    a, b = xform(lo), xform(hi)
    best_x = best_score = None
    evaluations = 0

    def record(x, score):
        nonlocal best_x, best_score
        if best_score is None or score < best_score or (
            score == best_score and x < best_x
        ):
            best_x, best_score = x, score
        return score

    def consider(x):
        nonlocal evaluations
        evaluations += 1
        return record(x, evaluate(unxform(x)))

    # `seeds` carries points the coarse grid already scored (both
    # bracket endpoints AND the coarse winner between them): a known
    # score is recorded, never re-evaluated (CRN determinism makes a
    # re-evaluation identical and thus pure waste - here whole generator
    # walks), and recording the coarse winner keeps best-ever tracking
    # honest when refinement never beats it.
    seeded = {xform(x): s for x, s in seeds}
    for endpoint in (a, b):
        if endpoint in seeded:
            record(endpoint, seeded[endpoint])
        else:
            consider(endpoint)
    for x, s in seeded.items():
        if x != a and x != b:
            record(x, s)
    while True:
        if objective_threshold is not None \
                and best_score <= objective_threshold + SLACK:
            termination = f"objective <= {objective_threshold}"
            break
        span = b - a
        if absolute_step is not None:
            if span <= absolute_step:
                termination = f"absolute step <= {absolute_step}"
                break
        elif log_domain:
            # a and b are logs, so their span IS the relative width:
            # log(hi/lo) <= log1p(step). Dividing a log span by |log x|
            # is not a relative error in x - it over-refined near x = 1
            # and under-refined far from it.
            if span <= math.log1p(SOLVE_RELATIVE_STEP):
                termination = f"relative step <= {SOLVE_RELATIVE_STEP}"
                break
        else:
            mid_abs = max(abs(a), abs(b), 1e-30)
            if span / mid_abs <= SOLVE_RELATIVE_STEP:
                termination = f"relative step <= {SOLVE_RELATIVE_STEP}"
                break
        m1 = a + span / 3
        m2 = a + 2 * span / 3
        f1 = consider(m1)
        f2 = consider(m2)
        if f1 <= f2:
            b = m2
        else:
            a = m1
    return unxform(best_x), best_score, termination, evaluations


def coarse_grid(lo: float, hi: float, points: int,
                log_domain: bool) -> list[float]:
    """The deterministic coarse grid, shared by solve_scalar and the
    prewarm calls so both name exactly the same candidates."""
    if log_domain:
        step = (math.log(hi) - math.log(lo)) / (points - 1)
        return [math.exp(math.log(lo) + i * step) for i in range(points)]
    step = (hi - lo) / (points - 1)
    return [lo + i * step for i in range(points)]


def solve_scalar(evaluate, lo: float, hi: float, points: int,
                 log_domain: bool, absolute_step: float | None = None,
                 objective_threshold: float | None = None):
    """Coarse grid then trisection of the winner's neighbor bracket; a
    boundary winner takes its single inside neighbor interval. Returns the
    solve record the artifact schema requires."""
    grid = coarse_grid(lo, hi, points, log_domain)
    scores = [evaluate(x) for x in grid]
    best_i = min(range(len(grid)), key=lambda i: (scores[i], grid[i]))
    left = grid[max(0, best_i - 1)]
    right = grid[min(len(grid) - 1, best_i + 1)]
    tie_break = "smaller candidate on equal scores"
    if objective_threshold is not None \
            and scores[best_i] <= objective_threshold + SLACK:
        return {
            "domain": [lo, hi], "coarse_points": points,
            "coarse_grid": grid, "best_candidate": grid[best_i],
            "search_score": scores[best_i],
            "termination": f"objective <= {objective_threshold}",
            "tie_break": tie_break, "evaluations": points,
        }
    if left == right:
        return {
            "domain": [lo, hi], "coarse_points": points, "coarse_grid": grid,
            "best_candidate": grid[best_i],
            "search_score": list(scores[best_i])
            if isinstance(scores[best_i], tuple) else scores[best_i],
            "termination": "degenerate single-point domain",
            "tie_break": tie_break, "evaluations": points,
        }
    best_x, best_score, termination, extra = trisect(
        evaluate, left, right, log_domain, absolute_step,
        objective_threshold,
        seeds=(
            (left, scores[max(0, best_i - 1)]),
            (grid[best_i], scores[best_i]),
            (right, scores[min(len(grid) - 1, best_i + 1)]),
        ),
    )
    return {
        "domain": [lo, hi], "coarse_points": points, "coarse_grid": grid,
        "best_candidate": best_x,
        "search_score": list(best_score)
        if isinstance(best_score, tuple) else best_score,
        "termination": termination,
        "tie_break": tie_break,
        "evaluations": points + extra,
    }


# ---------------------------------------------------------------------------
# The fit driver
# ---------------------------------------------------------------------------


def summaries_for(run_summary, overrides: dict, seeds, start_ns: int,
                  length: str, with_seeds: bool = False):
    # Seeds of one evaluation are independent walks; fan them out when the
    # runner is the real subprocess (an injected selftest fake stays
    # serial so its call order remains deterministic). `map` preserves
    # seed order, so `pooled` sees exactly the serial list. The probe
    # paths pass with_seeds=True and receive the named SeedSummaries shape
    # {"pooled": ..., "per_seed": [...]} - the per-seed minute-range gates
    # need the raw summaries - while every solver path keeps the pooled
    # dict alone, semantics unchanged.
    if run_summary is run_summary_subprocess and len(seeds) > 1:
        with ThreadPoolExecutor(min(len(seeds), WALK_JOBS)) as pool:
            raw = list(pool.map(
                lambda seed: run_summary(overrides, seed, start_ns, length,
                                         SUMMARY_WARMUP),
                seeds,
            ))
    else:
        raw = [
            run_summary(overrides, seed, start_ns, length, SUMMARY_WARMUP)
            for seed in seeds
        ]
    if with_seeds:
        # The NAMED SeedSummaries shape the successor spec pins: positional
        # unpacking invited exactly the consumer swap the names prevent.
        return {"pooled": pooled(raw), "per_seed": raw}
    return pooled(raw)


def prewarm_walks(run_summary, override_sets, seeds, start_ns: int,
                  length: str) -> None:
    """Populate the walk cache in parallel for evaluations whose override
    sets are known up front (every coarse grid, every family probe). The
    solver then replays them serially from the cache, so its evaluation
    order, tie-breaks and the selftest's determinism assertions are
    untouched - the cache is the synchronization point. A failing walk is
    swallowed here and left for the serial pass to re-raise
    deterministically. The sequential trisection tail is deliberately not
    prewarmed: its points depend on earlier scores."""
    if run_summary is not run_summary_subprocess:
        return
    warm_gen_build()
    with ThreadPoolExecutor(WALK_JOBS) as pool:
        futures = [
            pool.submit(run_summary, overrides, seed, start_ns, length,
                        SUMMARY_WARMUP)
            for overrides in override_sets
            for seed in seeds
        ]
        for future in futures:
            try:
                future.result()
            except Refusal:
                pass


# Boundary slack for the inclusive tolerance comparisons, the pair-harness
# convention: a bound like 0.10 is not exactly representable in binary, so a
# discrepancy of exactly-the-bound computes a hair above it. The slack is
# far below any measurement resolution and exists only to make "inclusive"
# mean inclusive.
SLACK = 1e-12


def within(kind: str, bound: float, generated, observed) -> bool:
    if kind == "relative":
        return abs(generated - observed) <= bound * abs(observed) + SLACK
    if kind == "absolute":
        return abs(generated - observed) <= bound + SLACK
    if kind == "ceiling":
        return generated <= bound + SLACK
    if kind == "size_tail":
        return abs(generated - observed) <= max(
            1.0, bound * abs(observed)
        ) + SLACK
    if kind == "exact":
        return generated == observed
    raise AssertionError(f"unknown tolerance kind {kind}")


def combined_displacement(gen: dict) -> float:
    all_hist: dict[int, int] = {}
    for h in gen["displacement_hist"].values():
        for k, v in h.items():
            all_hist[k] = all_hist.get(k, 0) + v
    return hist_median(all_hist, DISPLACEMENT_BIN_TICKS)


def nearest_rank_of(hist: dict[int, int], q: float) -> int:
    qq = Quantiles()
    qq.counts = dict(hist)
    qq.total = sum(hist.values())
    return qq.nearest_rank(q)


FAMILIES = ("cadence", "size", "quote", "displacement", "volatility",
            "start_price")

FAMILY_SLOTS = {
    "cadence": ("mean_event_duration_s", "children_mean",
                "children_single_frac", "levels_mean"),
    "size": ("latent_size_median", "size_log_sigma"),
    # size_round_frac joins the size family on the joint branch
    "quote": ("quoted_width", "top_sizes"),
    "displacement": ("trade_displacement_ticks",),
    "volatility": ("vol_scalar",),
    "start_price": ("start_price",),
}

# The per-target verdict layout (Brick F schema): every landable slot is
# a target with its family and the judge metrics its verdict reads. The
# landing set is derived from these verdicts ALONE.
TARGETS = (
    ("mean_event_duration_s", "cadence", ("mean_event_duration_s",)),
    ("children_mean", "cadence", ("children_mean",)),
    ("children_single_frac", "cadence", ("children_single_frac",)),
    ("levels_mean", "cadence", ("levels_mean",)),
    ("latent_size_median", "size",
     ("size_ecdf_distance", "size_mean", "size_p90", "size_p99")),
    ("size_log_sigma", "size",
     ("size_ecdf_distance", "size_mean", "size_p90", "size_p99")),
    ("size_round_frac", "size",
     ("size_ecdf_distance", "size_mean", "size_p90", "size_p99")),
    ("quoted_width", "quote", ("width",)),
    ("top_sizes", "quote", ("top_bid", "top_ask")),
    ("trade_displacement_ticks", "displacement",
     ("displacement_median", "displacement_side_B", "displacement_side_A")),
    # The volatility probe runs with fitted cadence installed and its
    # family pass reads EVERY check, cadence included; the cadence metrics
    # are listed here so a cadence miss inside the volatility probe is
    # visible in this verdict instead of failing it with all-true checks.
    ("vol_scalar", "volatility",
     ("mid_rms", "minute_range_p99", "minute_range_p99.9",
      "minute_range_max", "mean_event_duration_s", "children_mean",
      "children_single_frac", "levels_mean")),
    ("start_price", "start_price", ("scratch_config_accepted",)),
)

# Judge metric -> TOLERANCES row, where the names differ.
METRIC_TOLERANCE = {
    "displacement_side_B": "displacement_side_median",
    "displacement_side_A": "displacement_side_median",
    "top_bid": "top_sizes",
    "top_ask": "top_sizes",
    "scratch_config_accepted": "start_price",
}

# Extra judge-measured keys a target's verdict carries beyond its gate
# metrics (reported diagnostics that ride with the verdict).
MEASURED_EXTRAS = {"start_price": ("first_book_mid", "start_price")}


def run_fit(directory: str = DELIVERY_DIR,
            run_summary=run_summary_subprocess,
            harness_commit: str | None = None,
            ledger_path: str | None = None,
            preflight_artifact_path: str = PREFLIGHT_ARTIFACT) -> dict:
    # Identity first, before a byte of CSV or a generator walk: a real run
    # (no injected commit) refuses on a dirty tree.
    if harness_commit is None:
        harness_commit = require_clean_tree()
    hashes = verify_input(directory, ledger_path)
    preflight, preflight_hash = require_preflight(
        hashes, preflight_artifact_path
    )
    usable = preflight["usable_sessions"]
    observed = observe(parse_stream(data_files(directory)), usable)

    solves: dict[str, dict] = {}
    fitted: dict[str, object] = {}

    # Closed-form selections.
    fitted["quoted_width"] = observed["width_mode"]
    fitted["top_sizes"] = {
        "bid": observed["top_bid_median"], "ask": observed["top_ask_median"],
    }
    fitted["start_price"] = observed["last_price_points"]
    fitted.update({
        "mean_event_duration_s": observed["mean_event_duration_s"],
        "children_mean": observed["children_mean"],
        "children_single_frac": observed["children_single_frac"],
        "levels_mean": observed["levels_mean"],
    })
    cadence_overrides = {
        "generator.mean_event_duration_s": observed["mean_event_duration_s"],
        "generator.children_mean": observed["children_mean"],
        "generator.children_single_frac": observed["children_single_frac"],
        "generator.levels_mean": observed["levels_mean"],
    }

    # --- size family: the JOINT (sigma, median) solve of the successor
    # spec 3.2 - sixteen fixed sigmas, each a COMPLETE median solve, the
    # winner compared lexicographically with NO sigma refinement (the
    # winning grid sigma IS the fitted value, a stated grid-resolution
    # answer the representability gates judge) - then model B's frac grid
    # at the winning sigma behind the moot guard.
    def size_overrides(median: float, frac, sigma: float) -> dict:
        overrides = {
            "generator.latent_size_median": f"{median:.6f}",
            "generator.size_log_sigma": sigma,
        }
        if frac is not None:
            overrides["generator.size_round_frac"] = frac
        return overrides

    def size_eval_factory(frac, sigma: float):
        def evaluate(median):
            gen = summaries_for(run_summary,
                                size_overrides(median, frac, sigma),
                                SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)
            return size_objective(gen["size_histogram"], observed)
        return evaluate

    size_grid = coarse_grid(*SIZE_MEDIAN_DOMAIN, SIZE_MEDIAN_GRID_POINTS,
                            log_domain=True)
    sigma_step = (SIGMA_GRID_DOMAIN[1] - SIGMA_GRID_DOMAIN[0]) \
        / (SIGMA_GRID_POINTS - 1)
    sigma_grid = [
        SIGMA_GRID_DOMAIN[0] + i * sigma_step
        for i in range(SIGMA_GRID_POINTS)
    ]
    prewarm_walks(run_summary,
                  [size_overrides(m, None, sg)
                   for sg in sigma_grid for m in size_grid],
                  SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)
    per_sigma = []
    for sigma in sigma_grid:
        record = solve_scalar(size_eval_factory(None, sigma),
                              *SIZE_MEDIAN_DOMAIN,
                              SIZE_MEDIAN_GRID_POINTS, log_domain=True)
        per_sigma.append((tuple(record["search_score"]), sigma, record))
    # Ties break toward the smaller sigma, then the smaller median.
    per_sigma.sort(key=lambda t: (t[0], t[1], t[2]["best_candidate"]))
    _a_score, chosen_sigma, model_a = per_sigma[0]
    fitted["size_log_sigma"] = chosen_sigma
    a_median = model_a["best_candidate"]
    observed_p50 = observed["size_quantiles"]["p50"]
    # The moot guard (amendment, 2026-08-05, restoring the design-round
    # rule): model B runs only if model A's solved median or the observed
    # p50 reaches the identifiability floor. Below it, integral_lot is 1
    # and the frac is structurally inert - both size arms materialize
    # identically - so the 51 frac solves would be 51 reruns of model A
    # with a dead knob: identical walks, identical scores, differing only
    # in tie-break bookkeeping. The skip record with its precondition
    # values IS the complete evidence on that branch.
    if (a_median >= SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
            or observed_p50 >= SIZE_MEDIAN_IDENTIFIABILITY_FLOOR):
        prewarm_walks(run_summary,
                      [size_overrides(m, f, chosen_sigma)
                       for f in SIZE_ROUND_FRAC_GRID for m in size_grid],
                      SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)
        b_results = []
        for frac in SIZE_ROUND_FRAC_GRID:
            record = solve_scalar(
                size_eval_factory(frac, chosen_sigma), *SIZE_MEDIAN_DOMAIN,
                SIZE_MEDIAN_GRID_POINTS, log_domain=True,
            )
            b_results.append((tuple(record["search_score"]), frac, record))
        b_results.sort(key=lambda t: (t[0], t[1], t[2]["best_candidate"]))
        _b_score, b_frac, model_b = b_results[0]
        model_b = dict(model_b, frac=b_frac)
        if tuple(model_b["search_score"]) < tuple(model_a["search_score"]):
            chosen_median = model_b["best_candidate"]
            chosen_frac = model_b["frac"]
            chosen_model = "B"
        else:
            chosen_median, chosen_frac, chosen_model = a_median, None, "A"
    else:
        model_b = {
            "skipped": "structurally-moot",
            "model_a_median": a_median,
            "observed_p50": observed_p50,
            "floor": SIZE_MEDIAN_IDENTIFIABILITY_FLOOR,
        }
        chosen_median, chosen_frac, chosen_model = a_median, None, "A"
    identifiable = chosen_median >= SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
    # The landing value is the MATERIALIZED median: the scratch-config
    # transport writes the override as f"{median:.6f}", so that rounded
    # value is what every judged walk actually ran and what the preset
    # must pin. The raw optimum stays in the solve record.
    fitted["latent_size_median"] = float(f"{chosen_median:.6f}")
    if identifiable and chosen_model == "B":
        fitted["size_round_frac"] = chosen_frac
    solves["latent_size_median"] = {
        "model_a": model_a, "model_b": model_b, "chosen_model": chosen_model,
        "raw_optimum": chosen_median,
    }
    # The winning sigma's solve record materialized at the top level per
    # the Brick F schema (domain, coarse grid, best candidate, search
    # score, termination, evaluations), with the sigma dimension's own
    # domain and grid beside it and every per-sigma record retained.
    solves["size_log_sigma"] = {
        "domain": list(SIGMA_GRID_DOMAIN),
        "coarse_grid": sigma_grid,
        "coarse_points": SIGMA_GRID_POINTS,
        "best_candidate": chosen_sigma,
        "search_score": model_a["search_score"],
        # These describe the SIGMA search, not the winning median subsolve:
        # the sigma dimension is a fixed grid with no refinement by
        # contract, and each inner median solve's own termination and cost
        # live under per_sigma.
        "termination": "fixed 16-point sigma grid exhausted, no refinement",
        "evaluations": SIGMA_GRID_POINTS,
        "nested_median_evaluations": sum(
            rec["evaluations"] for _score, _sg, rec in per_sigma
        ),
        "tie_break": "smaller sigma, then smaller median",
        "per_sigma": [
            {"sigma": sg, "record": rec} for _score, sg, rec in per_sigma
        ],
    }
    solves["size_round_frac"] = {
        "identifiable": identifiable,
        "branch": "joint" if identifiable and chosen_model == "B"
        else "declared",
    }

    # --- displacement: inverse solve with the fitted width installed ---
    disp_target = observed["displacement_median_ticks"]

    def disp_overrides(scalar: float) -> dict:
        return {"generator.trade_displacement_ticks.ticks": scalar,
                "generator.quoted_width.ticks": fitted["quoted_width"]}

    def disp_eval(scalar):
        gen = summaries_for(
            run_summary, disp_overrides(scalar),
            SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH,
        )
        return abs(combined_displacement(gen) - disp_target)

    prewarm_walks(run_summary,
                  [disp_overrides(x) for x in coarse_grid(
                      0.0, 2.0 * fitted["quoted_width"],
                      DISPLACEMENT_GRID_POINTS, log_domain=False)],
                  SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)

    # Both medians are bin centers on the shared 0.05-tick grid, so their
    # difference is a multiple of the bin: only an exact match can
    # terminate early, and a nonzero threshold would imply a resolution
    # the estimator does not have.
    disp_solve = solve_scalar(
        disp_eval, 0.0, 2.0 * fitted["quoted_width"],
        DISPLACEMENT_GRID_POINTS, log_domain=False,
        absolute_step=SOLVE_ABSOLUTE_STEP_TICKS,
        objective_threshold=0.0,
    )
    fitted["trade_displacement_ticks"] = disp_solve["best_candidate"]
    solves["trade_displacement_ticks"] = dict(disp_solve, target=disp_target)

    # --- volatility: inverse solve with fitted cadence installed ---
    def vol_eval(scalar):
        gen = summaries_for(
            run_summary,
            dict(cadence_overrides, **{"generator.vol_scalar": scalar}),
            SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH,
        )
        if not math.isfinite(gen["mid_rms"]) or gen["mid_rms"] <= 0:
            return float("inf")
        return abs(gen["mid_rms"] - observed["mid_rms"]) / observed["mid_rms"]

    prewarm_walks(run_summary,
                  [dict(cadence_overrides, **{"generator.vol_scalar": x})
                   for x in coarse_grid(*VOL_SCALAR_DOMAIN, VOL_GRID_POINTS,
                                        log_domain=True)],
                  SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)
    vol_solve = solve_scalar(vol_eval, *VOL_SCALAR_DOMAIN, VOL_GRID_POINTS,
                             log_domain=True, objective_threshold=0.001)
    fitted["vol_scalar"] = vol_solve["best_candidate"]
    solves["vol_scalar"] = dict(vol_solve, target=observed["mid_rms"])

    # --- family probes then the final combined run (4.9) ---
    def family_overrides(family: str) -> dict:
        if family == "cadence":
            return dict(cadence_overrides)
        if family == "size":
            over = {
                "generator.latent_size_median": f"{chosen_median:.6f}",
                "generator.size_log_sigma": chosen_sigma,
            }
            if fitted.get("size_round_frac") is not None:
                over["generator.size_round_frac"] = fitted["size_round_frac"]
            return over
        if family == "quote":
            return {
                "generator.quoted_width.ticks": fitted["quoted_width"],
                "generator.top_sizes.bid": str(fitted["top_sizes"]["bid"]),
                "generator.top_sizes.ask": str(fitted["top_sizes"]["ask"]),
            }
        if family == "displacement":
            return {
                "generator.trade_displacement_ticks.ticks":
                    fitted["trade_displacement_ticks"],
                "generator.quoted_width.ticks": fitted["quoted_width"],
            }
        if family == "volatility":
            return dict(cadence_overrides,
                        **{"generator.vol_scalar": fitted["vol_scalar"]})
        if family == "start_price":
            return {"generator.start_price": fitted["start_price"]}
        raise AssertionError(family)

    def judge(gen: dict, family: str, per_seed=None) -> dict:
        """Per-metric pass/fail (`checks`), the generated values the
        checks read (`measured`), and the empirical values they were
        held against (`targets`) - the per-target verdicts are assembled
        from these three. `per_seed` carries the raw per-seed summaries
        the volatility family's minute-range gates judge seed by seed."""
        checks: dict = {}
        measured: dict = {}
        targets: dict = {}
        if family in ("cadence", "volatility"):
            for name in FAMILY_SLOTS["cadence"]:
                kind, bound = TOLERANCES[name]
                checks[name] = within(kind, bound, gen[name], observed[name])
                measured[name] = gen[name]
                targets[name] = observed[name]
        if family == "size":
            score = size_objective(gen["size_histogram"], observed)
            checks["size_ecdf_distance"] = within(
                "ceiling", TOLERANCES["size_ecdf_distance"][1], score[0],
                None,
            )
            measured["size_ecdf_distance"] = score[0]
            hist = gen["size_histogram"]
            total = sum(hist.values())
            gen_mean = sum(k * v for k, v in hist.items()) / total
            checks["size_mean"] = within(
                "relative", TOLERANCES["size_mean"][1], gen_mean,
                observed["size_mean"],
            )
            measured["size_mean"] = gen_mean
            targets["size_mean"] = observed["size_mean"]
            for name, p in (("size_p90", 0.90), ("size_p99", 0.99)):
                gen_q = nearest_rank_of(hist, p)
                obs_q = observed["size_quantiles"][f"p{int(p * 100)}"]
                checks[name] = within(
                    "size_tail", TOLERANCES[name][1], gen_q, obs_q,
                )
                measured[name] = gen_q
                targets[name] = obs_q
        if family == "displacement":
            gen_median = combined_displacement(gen)
            checks["displacement_median"] = within(
                "absolute", TOLERANCES["displacement_median"][1],
                gen_median, disp_target,
            )
            measured["displacement_median"] = gen_median
            targets["displacement_median"] = disp_target
            # Side gates are side-vs-side (the amended 4.9): a symmetric
            # generator always produces buyer ~ seller ~ scalar, so gating
            # both sides against the POOLED observed median would pass a
            # generator whose asymmetry is simply not represented - the
            # exact condition declared-misrepresented exists to catch.
            for side, obs_key in (
                ("B", "displacement_buyer_median_ticks"),
                ("A", "displacement_seller_median_ticks"),
            ):
                name = f"displacement_side_{side}"
                side_target = observed[obs_key]
                side_median = hist_median(gen["displacement_hist"][side],
                                          DISPLACEMENT_BIN_TICKS)
                measured[name] = side_median
                if math.isfinite(side_target):
                    checks[name] = within(
                        "absolute",
                        TOLERANCES["displacement_side_median"][1],
                        side_median, side_target,
                    )
                    targets[name] = side_target
                else:
                    # A side with zero valid-quote parents in the data has
                    # no observed median: the gate is explicitly vacuous
                    # and reported, never a NaN comparison failing quietly.
                    checks[name] = True
                    targets[name] = (
                        "vacuous: no valid-quote parents on this side"
                    )
        if family == "volatility":
            checks["mid_rms"] = within(
                "relative", TOLERANCES["mid_rms"][1], gen["mid_rms"],
                observed["mid_rms"],
            )
            measured["mid_rms"] = gen["mid_rms"]
            targets["mid_rms"] = observed["mid_rms"]
            # Minute-range gates (successor spec 3.3): one-sided upper
            # against the resampled observed envelope, judged PER SEED -
            # never one pooled maximum against one observed month, an
            # eightfold exposure asymmetry.
            envelope = observed["minute_range_envelope"]
            seed_stats = []
            for summary in per_seed or []:
                hist = {
                    int(k): v
                    for k, v in summary["minute_range_ticks_hist"].items()
                }
                seed_stats.append({
                    "p99": nearest_rank_of(hist, 0.99) if hist else 0,
                    "p99.9": nearest_rank_of(hist, 0.999) if hist else 0,
                    "max": summary["minute_range_max_ticks"],
                })
            for stat in MINUTE_RANGE_GATES:
                name = f"minute_range_{stat}"
                bound = envelope[stat]
                values = [s[stat] for s in seed_stats]
                checks[name] = bool(values) and all(
                    v <= bound + SLACK for v in values
                )
                measured[name] = values
                targets[name] = bound
        if family == "quote":
            wh = gen["width_histogram"]
            gen_mode = min(
                (k for k in wh if wh[k] == max(wh.values())), default=None
            ) if wh else None
            checks["width"] = gen_mode == fitted["quoted_width"]
            measured["width"] = gen_mode
            targets["width"] = fitted["quoted_width"]
            for name, hist_key, side in (
                ("top_bid", "bid_size_histogram", "bid"),
                ("top_ask", "ask_size_histogram", "ask"),
            ):
                gen_median = nearest_rank_of(gen[hist_key], 0.5) \
                    if gen[hist_key] else None
                checks[name] = gen_median == fitted["top_sizes"][side]
                measured[name] = gen_median
                targets[name] = fitted["top_sizes"][side]
        if family == "start_price":
            # The gate is exact scratch-profile resolution: the walk with the
            # configured value must run at all (a bad value refuses at
            # profile construction). First-book displacement is a reported
            # diagnostic, never a gate (4.9).
            checks["scratch_config_accepted"] = True
            measured["first_book_mid"] = gen.get("first_book_mid")
            targets["start_price"] = fitted["start_price"]
        return {"checks": checks, "measured": measured, "targets": targets}

    def family_passes(results: dict) -> bool:
        checks = results["checks"]
        return bool(checks) and all(checks.values())

    probe_results: dict[str, dict] = {}
    probe_errors: dict[str, str] = {}
    probe_gens: dict[str, dict] = {}
    prewarm_walks(run_summary,
                  [family_overrides(family) for family in FAMILIES],
                  FINAL_SEEDS, FINAL_START_NS, FINAL_LENGTH)
    for family in FAMILIES:
        try:
            seed_summaries = summaries_for(
                run_summary, family_overrides(family),
                FINAL_SEEDS, FINAL_START_NS, FINAL_LENGTH, with_seeds=True,
            )
            gen = seed_summaries["pooled"]
            probe_gens[family] = gen
            probe_results[family] = judge(
                gen, family, per_seed=seed_summaries["per_seed"]
            )
        except Refusal as exc:
            probe_results[family] = {
                "checks": {"probe_run": False}, "measured": {},
                "targets": {},
            }
            probe_errors[family] = str(exc)

    # Final-budget objective scores for the solved parameters, attached to
    # their solve records per the frozen artifact schema: the same objective
    # each search minimized, re-read from the family probe's pooled month.
    if "size" in probe_gens:
        final_size_score = list(
            size_objective(probe_gens["size"]["size_histogram"], observed)
        )
        solves["latent_size_median"]["final_score"] = final_size_score
        # The size objective is SHARED: sigma and median were solved jointly
        # against it, so the winning sigma's final-budget score is the same
        # reading.
        solves["size_log_sigma"]["final_score"] = final_size_score
    if "displacement" in probe_gens:
        solves["trade_displacement_ticks"]["final_score"] = abs(
            combined_displacement(probe_gens["displacement"]) - disp_target
        )
    if "volatility" in probe_gens:
        gen_rms = probe_gens["volatility"]["mid_rms"]
        solves["vol_scalar"]["final_score"] = (
            abs(gen_rms - observed["mid_rms"]) / observed["mid_rms"]
            if math.isfinite(gen_rms) and observed["mid_rms"] > 0
            else None
        )

    wrong_side_blocked = observed["wrong_side_share"] > MAX_WRONG_SIDE_SHARE
    cadence_probe_pass = family_passes(probe_results["cadence"])

    passing = set()
    for family in FAMILIES:
        ok = family_passes(probe_results[family])
        if family == "displacement" and wrong_side_blocked:
            ok = False
        if family == "volatility" and not cadence_probe_pass:
            ok = False
        if ok:
            passing.add(family)

    combined_results: dict[str, dict] = {}
    combined_error = None
    if passing:
        combined_overrides: dict = {}
        for family in passing:
            combined_overrides.update(family_overrides(family))
        try:
            seed_summaries = summaries_for(
                run_summary, combined_overrides, FINAL_SEEDS,
                FINAL_START_NS, FINAL_LENGTH, with_seeds=True,
            )
            for family in passing:
                combined_results[family] = judge(
                    seed_summaries["pooled"], family,
                    per_seed=seed_summaries["per_seed"],
                )
        except Refusal as exc:
            # A failed combined run fits NOTHING: no target may take fitted
            # provenance from a configuration that never produced its final
            # verdict. The artifact still lands, carrying the failure.
            combined_results = {}
            combined_error = str(exc)

    # The cadence family must hold in BOTH stages: a probe pass that the
    # combined run then contradicts is still a cadence failure, and the
    # wholesale stop and the volatility dependency read the final word,
    # not the probe alone.
    cadence_ok = (
        cadence_probe_pass
        and combined_error is None
        and "cadence" in combined_results
        and family_passes(combined_results["cadence"])
    )

    def stage_view(stage: dict | None, keys) -> dict | None:
        if stage is None:
            return None
        return {k: stage.get(k) for k in keys}

    verdicts: dict[str, dict] = {}
    for target, family, metrics in TARGETS:
        probe = probe_results[family]
        combined = combined_results.get(family)
        probe_ok = family in passing
        combined_ok = combined is not None and family_passes(combined)
        fit_ok = probe_ok and combined_ok and combined_error is None
        status = "fitted" if fit_ok else "declared-misrepresented"
        reason = probe_errors.get(family)
        if family == "displacement" and wrong_side_blocked:
            reason = (
                f"wrong-side share {observed['wrong_side_share']:.4f} "
                f"exceeds {MAX_WRONG_SIDE_SHARE}; the generator "
                "structurally forbids wrong-side prints"
            )
        if family == "volatility" and not cadence_probe_pass:
            status = "stopped"
            reason = "cadence failed; volatility depends on fitted cadence"
        if combined_error is not None and probe_ok:
            reason = f"the combined run failed: {combined_error}"
        if target == "size_round_frac":
            # The identifiability rule, not the score, decides the
            # provenance claim (4.75); only the joint (model B) branch
            # can carry a fitted frac.
            if not identifiable:
                status = "declared-unidentifiable"
                reason = (
                    f"winning median {chosen_median:.4f} is below the "
                    f"identifiability floor "
                    f"{SIZE_MEDIAN_IDENTIFIABILITY_FLOOR}; integral_lot 1 "
                    "makes the frac structurally inert on this grid"
                )
            elif chosen_model != "B":
                status = "declared-unidentifiable"
                reason = (
                    "model A won the lexicographic comparison; no joint "
                    "frac estimate is claimed and the declared value stays"
                )
        keys = metrics + MEASURED_EXTRAS.get(target, ())
        verdicts[target] = {
            "family": family,
            "status": status,
            "tolerance": {
                m: list(TOLERANCES[METRIC_TOLERANCE.get(m, m)])
                for m in metrics
            },
            "measured": {
                "probe": stage_view(probe["measured"], keys),
                "combined": stage_view(
                    combined["measured"] if combined else None, keys
                ),
            },
            "observed": stage_view(probe["targets"], keys),
            "checks": {
                "probe": stage_view(probe["checks"], metrics),
                "combined": stage_view(
                    combined["checks"] if combined else None, metrics
                ),
            },
            **({"reason": reason} if reason else {}),
        }

    if not cadence_ok and combined_error is None:
        # The landing STOPS outright: no slot lands, whatever the other
        # families measured. Their verdicts remain in the artifact as the
        # record of what was measured, downgraded from fitted to stopped.
        for target, verdict in verdicts.items():
            if verdict["status"] == "fitted":
                verdict["status"] = "stopped"
                verdict["reason"] = (
                    "the cadence family failed wholesale; the generator "
                    "cannot represent MNQ cadence and the landing stops"
                )

    landing = sorted(
        target for target, verdict in verdicts.items()
        if verdict["status"] == "fitted"
    )

    diagnostics = dict(observed["diagnostics"])
    diagnostics["horizon_vol"] = {
        "observed": observed["horizon_vol"],
        "generated_volatility_probe":
            probe_gens.get("volatility", {}).get("horizon_vol"),
    }

    return {
        "binding": {
            "job_id": JOB_ID,
            "file_hashes": hashes,
            "preflight_artifact_hash": preflight_hash,
            "subcontract_hash": subcontract_hash(),
            "harness_tree_commit": harness_commit,
        },
        "sessions": {
            "inventory": preflight["sessions"],
            "usable_count": len(usable),
        },
        "preflight": {
            k: preflight[k]
            for k in ("rows", "unsided_share", "invalid_width_share",
                      "book_counts", "valid_parent_quote_share")
        },
        "observed": observed,
        "solves": solves,
        "verdicts": verdicts,
        "diagnostics": diagnostics,
        "fitted_candidates": fitted,
        "landing_set": landing,
    }


_GIT_COMMIT_CACHE: str | None = None


def git_commit() -> str:
    global _GIT_COMMIT_CACHE
    if _GIT_COMMIT_CACHE is None:
        proc = subprocess.run(["git", "rev-parse", "HEAD"],
                              capture_output=True, text=True, cwd=ROOT)
        _GIT_COMMIT_CACHE = (
            proc.stdout.strip() if proc.returncode == 0 else "unknown"
        )
    return _GIT_COMMIT_CACHE


def require_clean_tree() -> str:
    """The commit that binds a real fit artifact. A dirty tree refuses:
    `harness_tree_commit` must name exactly the code that ran (the walk
    cache also keys on it), and `git rev-parse HEAD` on a dirty tree names
    code that did not."""
    proc = subprocess.run(["git", "status", "--porcelain"],
                          capture_output=True, text=True, cwd=ROOT)
    if proc.returncode != 0:
        raise Refusal("git status failed; the harness tree is unidentifiable")
    if proc.stdout.strip():
        raise Refusal(
            "the working tree is dirty; an artifact may only bind a commit "
            "that is exactly the code that ran - commit first"
        )
    commit = git_commit()
    if commit == "unknown":
        raise Refusal("git rev-parse failed; the harness tree is "
                      "unidentifiable")
    return commit


def mode_fit() -> None:
    artifact = run_fit()
    write_json_atomic(ARTIFACT_FILE, artifact)
    print(json.dumps(
        {"verdicts": {k: v["status"]
                      for k, v in artifact["verdicts"].items()},
         "landing_set": artifact["landing_set"]},
        indent=1, sort_keys=True, default=str,
    ))
    print(f"fit artifact -> {ARTIFACT_FILE}")


# ---------------------------------------------------------------------------
# Selftest (H2): synthetic conformance, no real data, no subprocess.
# ---------------------------------------------------------------------------


def _st_write_zst(path: str, lines: list[str]) -> None:
    data = ("\n".join(lines) + "\n").encode()
    with open(path, "wb") as fh:
        fh.write(zstd.compress(data))


_ST_HEADER = (
    "ts_recv,ts_event,rtype,publisher_id,instrument_id,action,side,depth,"
    "price,size,flags,ts_in_delta,sequence,bid_px_00,ask_px_00,bid_sz_00,"
    "ask_sz_00,bid_ct_00,ask_ct_00,symbol"
)


def _st_row(ts: int, side: str = "B", price: int = 23_000 * 10**9,
            size: int = 1, bid: int | None = None, ask: int | None = None,
            iid: str = "12345", action: str = "T",
            bid_sz: int = 3, ask_sz: int = 4) -> str:
    bid = bid if bid is not None else price - TICK_UNITS
    ask = ask if ask is not None else price
    return (
        f"{ts},{ts:019d},1,1,{iid},{action},{side},0,{price},{size},0,0,1,"
        f"{bid},{ask},{bid_sz},{ask_sz},1,1,MNQ.v.0"
    )


def _st_ts(label: str, hour_utc: int, minute: int = 0,
           second: int = 0) -> int:
    """A UTC instant inside the given trade-date's session (hour in UTC)."""
    day = dt.datetime.fromisoformat(label + "T00:00:00+00:00")
    return int((day.timestamp() + hour_utc * 3600 + minute * 60 + second)
               * 1_000_000_000)


def run_selftest() -> None:
    checks = 0

    def check(label: str, ok: bool) -> None:
        nonlocal checks
        checks += 1
        if not ok:
            raise SystemExit(f"  FAIL {label}")
        print(f"  ok   {label}")

    def refuses(fn, needle: str = "") -> bool:
        try:
            fn()
        except Refusal as exc:
            return needle in str(exc)
        return False

    os.makedirs(SELFTEST_DIR, exist_ok=True)

    print("budget instants")
    iso_ns = lambda iso: int(dt.datetime.fromisoformat(
        iso.replace("Z", "+00:00")).timestamp()) * 1_000_000_000
    check("SEARCH_START_NS matches its ISO instant",
          iso_ns(SEARCH_START_ISO) == SEARCH_START_NS)
    check("FINAL_START_NS matches its ISO instant",
          iso_ns(FINAL_START_ISO) == FINAL_START_NS)
    check("FINAL_END_NS matches its ISO instant",
          iso_ns(FINAL_END_ISO) == FINAL_END_NS)
    check("FINAL_LENGTH spans exactly start to end",
          FINAL_START_NS + int(FINAL_LENGTH[:-1]) * 10**9 == FINAL_END_NS)

    print("session assignment against the frozen inventory")
    check("the July 1 session opens at the June 30 22:00 UTC instant",
          assign_session(FINAL_START_NS) == ("2026-07-01", "overnight"))
    check("a cash-session afternoon row lands overnight-segment pre-halt",
          assign_session(_st_ts("2026-07-15", 19)) == ("2026-07-15",
                                                       "overnight"))
    check("a 15:20 Chicago row is the halt and belongs to no session",
          assign_session(_st_ts("2026-07-15", 20, 20)) == (None, None))
    check("a 15:45 Chicago row is the post-halt segment",
          assign_session(_st_ts("2026-07-15", 20, 45)) == ("2026-07-15",
                                                           "post_halt"))
    check("a 16:30 Chicago row is the daily break and belongs to no session",
          assign_session(_st_ts("2026-07-15", 21, 30)) == (None, None))
    check("a Saturday row belongs to no July label",
          assign_session(_st_ts("2026-07-11", 12))[0]
          not in INVENTORY_STATUS)
    check("July 3 is inventoried as the early close",
          INVENTORY_STATUS["2026-07-03"] == "early_close_excluded")
    check("the inventory carries 23 labels and 22 full sessions",
          len(SESSION_INVENTORY) == 23
          and sum(1 for _, s in SESSION_INVENTORY if s == "full")
          == EXPECTED_FULL_SESSIONS)

    print("book classification")
    check("normal / locked / crossed / nonpositive classify as named",
          classify_book(1, 2) == "normal"
          and classify_book(2, 2) == "locked"
          and classify_book(3, 2) == "crossed"
          and classify_book(0, 2) == "nonpositive")

    print("the stream contract")
    t0 = _st_ts("2026-07-15", 12)

    def stream_of(*files):
        paths = []
        for i, lines in enumerate(files):
            path = os.path.join(SELFTEST_DIR, f"st-{i}.csv.zst")
            _st_write_zst(path, [_ST_HEADER, *lines])
            paths.append(path)
        return paths

    good = list(parse_stream(stream_of([_st_row(t0), _st_row(t0 + 1)])))
    check("a well-formed stream parses", len(good) == 2)
    check("a short timestamp refuses",
          refuses(lambda: list(parse_stream(stream_of(
              [_st_row(t0).replace(f"{t0:019d}", "123")]))), "19-digit"))
    check("an ordering regression refuses",
          refuses(lambda: list(parse_stream(stream_of(
              [_st_row(t0 + 5), _st_row(t0)]))), "regression"))
    check("an off-grid price refuses",
          refuses(lambda: list(parse_stream(stream_of(
              [_st_row(t0, price=23_000 * 10**9 + 1)]))), "off the"))
    check("an off-grid bid refuses",
          refuses(lambda: list(parse_stream(stream_of(
              [_st_row(t0, bid=23_000 * 10**9 - 7)]))), "off the"))
    check("a side outside B/A/N refuses",
          refuses(lambda: list(parse_stream(stream_of(
              [_st_row(t0, side="S")]))), "alphabet"))
    check("a non-trade action refuses",
          refuses(lambda: list(parse_stream(stream_of(
              [_st_row(t0, action="M")]))), "action"))
    check("a missing required column refuses",
          refuses(lambda: list(parse_stream([(lambda p: (_st_write_zst(
              p, ["ts_event,price", "1,2"]), p)[1])(
              os.path.join(SELFTEST_DIR, "st-cols.csv.zst"))])),
              "missing required"))
    crlf_path = os.path.join(SELFTEST_DIR, "st-crlf.csv.zst")
    with open(crlf_path, "wb") as fh:
        fh.write(zstd.compress(
            ("\r\n".join([_ST_HEADER, _st_row(t0)]) + "\r\n").encode()
        ))
    check("CRLF line endings parse identically to LF",
          len(list(parse_stream([crlf_path]))) == 1)
    dup = _st_row(t0)
    check("identical adjacent rows WITHIN a file are legitimate",
          len(list(parse_stream(stream_of([dup, dup])))) == 2)
    check("a duplicated row AT THE SEAM refuses",
          refuses(lambda: list(parse_stream(stream_of([dup], [dup]))),
                  "boundary"))
    check("an overlap deeper than the final row still refuses at the seam",
          refuses(lambda: list(parse_stream(stream_of(
              [dup, _st_row(t0, size=2)], [dup]))), "boundary"))
    check("distinct rows at one timestamp cross the seam as one parent",
          len(group_parents_batch(list(parse_stream(stream_of(
              [_st_row(t0)], [_st_row(t0, size=2)]))))) == 1)
    check("a header-only data file refuses instead of resetting the seam",
          refuses(lambda: list(parse_stream(stream_of([dup], []))),
                  "no data rows"))

    print("streaming autocorrelation over exactly the accepted pairs")
    acf = Acf((1,))
    for x in (1.0, 2.0, 3.0, 4.0):
        acf.add(x)
    check("a perfectly linear series correlates at exactly 1",
          abs(acf.value(1) - 1.0) < 1e-12)
    acf = Acf((1,))
    for x in (1.0, 2.0):
        acf.add(x)
    acf.reset_series()
    for x in (3.0, 4.0):
        acf.add(x)
    check("a reset drops the straddling pair and keeps the rest exact",
          abs(acf.value(1) - 1.0) < 1e-12)

    print("grouping: streaming vs the independent batch implementation")
    rows = list(parse_stream(stream_of([
        _st_row(t0, side="B"), _st_row(t0, side="B", size=2),
        _st_row(t0, side="A"),          # same ts, side flip: new parent
        _st_row(t0 + 1, side="A"),      # ts change: new parent
        _st_row(t0 + 1, side="N"),      # unsided: never grouped
        _st_row(t0 + 2, side="B"),
    ])))
    groups = group_parents_batch(rows)
    check("the batch grouping finds the adversarial fixture's 4 parents",
          [len(g) for g in groups] == [2, 1, 1, 1])
    observed_fixture = observe(iter(rows), ["2026-07-15"])
    check("the streaming pass agrees with the batch grouping",
          observed_fixture["parents"] == len(groups)
          and observed_fixture["sided_rows"] == sum(len(g) for g in groups))
    bnb = list(parse_stream(stream_of([
        _st_row(t0, side="B"), _st_row(t0, side="N"), _st_row(t0, side="B"),
    ])))
    bnb_groups = group_parents_batch(bnb)
    bnb_observed = observe(iter(bnb), ["2026-07-15"])
    check("an unsided row TERMINATES the parent: B,N,B at one ts is two",
          [len(g) for g in bnb_groups] == [1, 1]
          and bnb_observed["parents"] == 2)

    print("observe: the three chains")
    s1 = _st_ts("2026-07-15", 19, 0)       # pre-halt
    s2 = _st_ts("2026-07-15", 20, 45)      # post-halt
    chain_rows = list(parse_stream(stream_of([
        _st_row(s1, side="B"),
        _st_row(s1 + 10**9, side="A"),
        _st_row(s1 + 2 * 10**9, side="B", bid=0),   # invalid book parent
        _st_row(s1 + 3 * 10**9, side="A"),
        _st_row(s2, side="B"),                       # crosses the halt
        _st_row(s2 + 10**9, side="A"),
    ])))
    obs = observe(iter(chain_rows), ["2026-07-15"])
    check("cadence gaps chain across an invalid-book parent",
          obs["eligible_gaps"] == 4)  # 3 pre-halt gaps + 1 post-halt gap
    check("a gap crossing the halt is excluded",
          obs["mean_event_duration_s"] == 1.0)
    check("an invalid-book parent carries no quote observation",
          obs["valid_quote_parents"] == 5)
    check("the invalid parent still counts in cadence",
          obs["parents"] == 6)
    check("the last valid trade prices the terminal anchor",
          obs["last_price_points"] == "23000.00")
    check("the size population records the all-prints composition",
          obs["size_population"]["prints"] == 6
          and obs["size_population"]["sided"] == 6
          and obs["size_population"]["invalid_book"] == 1
          and obs["size_population"]["valid_book"] == 5)

    print("displacement touch categories and the horizon windows")
    p = 23_000 * 10**9
    two_wide = dict(bid=p - 2 * TICK_UNITS, ask=p)
    cat_rows = list(parse_stream(stream_of([
        _st_row(s1, price=p, **two_wide),                    # at touch
        _st_row(s1 + 10**9, price=p - TICK_UNITS, **two_wide),   # inside
        _st_row(s1 + 2 * 10**9, price=p + TICK_UNITS, **two_wide),  # beyond
        _st_row(s1 + 3 * 10**9, price=p - 2 * TICK_UNITS,
                **two_wide),                                 # wrong side
    ])))
    cat_obs = observe(iter(cat_rows), ["2026-07-15"])
    check("the four touch categories each take a quarter",
          cat_obs["displacement_fractions"]["B"]
          == {"wrong_side": 0.25, "inside_mid": 0.25, "at_touch": 0.25,
              "beyond_touch": 0.25, "parents": 4}
          and cat_obs["wrong_side_share"] == 0.25)
    check("the quote diagnostics report median, p90 and MAD from the mode",
          cat_obs["width_median"] == 2 and cat_obs["width_p90"] == 2
          and cat_obs["width_mad_from_mode"] == 0.0
          and cat_obs["top_size_quantiles"]["bid"]["p99"] == 3)
    check("displacement p90 is reported beside the median",
          math.isfinite(cat_obs["displacement_p90_ticks"]))
    hz_rows = list(parse_stream(stream_of([
        _st_row(s1, price=p),
        _st_row(s1 + 61 * 10**9, price=p + TICK_UNITS),
        _st_row(s1 + 122 * 10**9, price=p + 2 * TICK_UNITS),
    ])))
    hz_obs = observe(iter(hz_rows), ["2026-07-15"])
    check("60s windows aligned to the segment start observe two returns",
          hz_obs["horizon_vol"]["60"]["count"] == 2
          and hz_obs["horizon_vol"]["60"]["rms"] > 0)
    check("the 300s horizon sees no completed window in 122 seconds",
          hz_obs["horizon_vol"]["300"]["count"] == 0)
    # s1 sits exactly on a 60s boundary (21h past the segment origin), and
    # TWO valid-quote parents share that timestamp with different mids: the
    # boundary's as-of mid must be the LAST of them, not the first.
    tie_rows = list(parse_stream(stream_of([
        _st_row(s1, side="B", price=p, bid=p - 2 * TICK_UNITS, ask=p),
        _st_row(s1, side="A", price=p, bid=p, ask=p + 2 * TICK_UNITS),
        _st_row(s1 + 60 * 10**9, price=p, bid=p - 2 * TICK_UNITS, ask=p),
        _st_row(s1 + 61 * 10**9, price=p, bid=p - 2 * TICK_UNITS, ask=p),
    ])))
    tie_obs = observe(iter(tie_rows), ["2026-07-15"])
    tie_expected = math.log((p - TICK_UNITS) / (p + TICK_UNITS))
    check("a boundary's as-of mid is the last parent at that timestamp",
          tie_obs["horizon_vol"]["60"]["count"] == 1
          and abs(tie_obs["horizon_vol"]["60"]["sum"] - tie_expected)
          < 1e-12)
    check("per-session cadence stability is summarized across sessions",
          hz_obs["cadence_stability"]["mean_event_duration_s"]["median"]
          == 61.0
          and hz_obs["per_session_cadence"]["2026-07-15"]["parents"] == 3)
    invalid_rows = list(parse_stream(stream_of([
        _st_row(s1, bid=0), _st_row(s1 + 10**9, bid=0),
    ])))
    check("a stream with no valid-quote parents refuses, not crashes",
          refuses(lambda: observe(iter(invalid_rows), ["2026-07-15"]),
                  "no valid-quote"))

    print("preflight thresholds at their boundaries")
    fake_dir = os.path.join(SELFTEST_DIR, "delivery")
    os.makedirs(fake_dir, exist_ok=True)

    def build_delivery(lines, ledger_state="downloaded",
                       job=JOB_ID, tamper_manifest=False,
                       tamper_hash=False):
        for stale in os.listdir(fake_dir):
            os.remove(os.path.join(fake_dir, stale))
        data_path = os.path.join(fake_dir, "glbx-st.tbbo.csv.zst")
        _st_write_zst(data_path, [_ST_HEADER, *lines])
        digest = sha256_file(data_path)
        if tamper_hash:
            digest = "0" * 64
        # The inventory mirrors the REAL delivery: sidecars are inventoried
        # alongside the data file. The first real preflight refused because
        # the fixture ledgers listed only csv.zst files and never exercised
        # a sidecar-bearing inventory against the completeness check.
        inventory = {
            "glbx-st.tbbo.csv.zst": digest,
            "condition.json": "d" * 64,
            "manifest.json": "f" * 64,
            "metadata.json": "e" * 64,
        }
        manifest = {
            "job_id": job if not tamper_manifest else "GLBX-OTHER",
            "files": dict(inventory),
        }
        with open(os.path.join(fake_dir, "manifest.json"), "w") as fh:
            json.dump(manifest, fh)
        with open(os.path.join(fake_dir, "metadata.json"), "w") as fh:
            json.dump({"job_id": job}, fh)
        with open(os.path.join(fake_dir, "condition.json"), "w") as fh:
            json.dump({}, fh)
        ledger = {"_version": 1, "jobs": {LEDGER_KEY: {
            "state": ledger_state, "job_id": job,
            "files": dict(inventory),
        }}}
        ledger_path = os.path.join(fake_dir, "ledger.json")
        with open(ledger_path, "w") as fh:
            json.dump(ledger, fh)
        return ledger_path

    # A usable month: one parent per second for 100 seconds in each of 22
    # sessions, prices walking a few ticks (so mid returns exist and the
    # volatility target is nonzero), plus controllable defect rows.
    def month_lines(extra=(), unsided=0, locked=0, impure_session=None,
                    size=1):
        lines = []
        for label, status in SESSION_INVENTORY:
            if status != "full":
                continue
            base = _st_ts(label, 12)
            iid = "99999" if label == impure_session else "12345"
            for i in range(100):
                price = 23_000 * 10**9 + (i % 3) * TICK_UNITS
                lines.append(_st_row(base + i * 10**9, price=price,
                                     iid="12345", size=size))
            if label == impure_session:
                lines.append(_st_row(base + 100 * 10**9, iid=iid))
        base = _st_ts("2026-07-15", 13)
        for i in range(unsided):
            lines.append(_st_row(base + i * 10**9, side="N"))
        for i in range(locked):
            price = 23_000 * 10**9
            lines.append(_st_row(base + (unsided + i) * 10**9,
                                 bid=price, ask=price))
        lines.extend(extra)
        return sorted(lines, key=lambda l: int(l.split(",")[1]))

    ledger_path = build_delivery(month_lines())
    payload = run_preflight(fake_dir, ledger_path)
    check("a clean synthetic month preflights",
          len(payload["usable_sessions"]) == 22)
    # The EXACT boundary: 99 sided + 1 unsided is a share of exactly 0.01,
    # which must pass the unsided gate inclusively - proven by the refusal
    # then coming from the SESSION check further down the preflight, not
    # from the unsided gate.
    base = _st_ts("2026-07-15", 12)
    boundary_lines = [_st_row(base + i * 10**9) for i in range(99)]
    boundary_lines.append(_st_row(base + 99 * 10**9, side="N"))
    ledger_path = build_delivery(boundary_lines)
    check("an unsided share of exactly the bound passes that gate",
          refuses(lambda: run_preflight(fake_dir, ledger_path),
                  "sessions excluded"))
    boundary_lines = [_st_row(base + i * 10**9) for i in range(98)]
    boundary_lines.append(_st_row(base + 98 * 10**9, side="N"))
    boundary_lines.append(_st_row(base + 99 * 10**9, side="N"))
    ledger_path = build_delivery(boundary_lines)
    check("one unsided row past the bound refuses on the unsided gate",
          refuses(lambda: run_preflight(fake_dir, ledger_path), "unsided"))
    total = payload["rows"]
    ledger_path = build_delivery(month_lines(unsided=total // 20))
    check("an unsided share above the bound refuses",
          refuses(lambda: run_preflight(fake_dir, ledger_path), "unsided"))
    ledger_path = build_delivery(month_lines(locked=total // 100))
    check("a locked-heavy book refuses on the invalid-width share",
          refuses(lambda: run_preflight(fake_dir, ledger_path),
                  "invalid-width"))
    ledger_path = build_delivery(month_lines(impure_session="2026-07-20"))
    payload = run_preflight(fake_dir, ledger_path)
    check("an impure session is excluded by name, exact purity",
          ["2026-07-20", "impure: ids ['12345', '99999']"]
          in payload["excluded_sessions"])
    lines = [
        l for l in month_lines()
        if not any(l.split(",")[4] == "12345"
                   and assign_session(int(l.split(",")[1]))[0] == label
                   for label in ("2026-07-06", "2026-07-07", "2026-07-08",
                                 "2026-07-09", "2026-07-10"))
    ]
    ledger_path = build_delivery(lines)
    check("five missing sessions refuse on MAX_EXCLUDED_SESSIONS",
          refuses(lambda: run_preflight(fake_dir, ledger_path), "excluded"))

    print("identity binding")
    ledger_path = build_delivery(month_lines(), ledger_state="done")
    check("a merely submitted delivery refuses",
          refuses(lambda: run_preflight(fake_dir, ledger_path),
                  "downloaded"))
    ledger_path = build_delivery(month_lines(), job="GLBX-OTHER")
    check("a foreign job id refuses",
          refuses(lambda: run_preflight(fake_dir, ledger_path), "job"))
    ledger_path = build_delivery(month_lines(), tamper_manifest=True)
    check("a manifest naming another job refuses",
          refuses(lambda: run_preflight(fake_dir, ledger_path), "manifest"))
    ledger_path = build_delivery(month_lines(), tamper_hash=True)
    check("tampered bytes refuse on the rehash",
          refuses(lambda: run_preflight(fake_dir, ledger_path), "sha256"))
    ledger_path = build_delivery(month_lines())
    for inventory_path in (ledger_path,
                           os.path.join(fake_dir, "manifest.json")):
        with open(inventory_path) as fh:
            doc = json.load(fh)
        files = doc["jobs"][LEDGER_KEY]["files"] \
            if "jobs" in doc else doc["files"]
        files["phantom.tbbo.csv.zst"] = "0" * 64
        with open(inventory_path, "w") as fh:
            json.dump(doc, fh)
    check("an inventoried file missing from disk refuses outright",
          refuses(lambda: run_preflight(fake_dir, ledger_path),
                  "missing from disk"))

    print("artifact serialization stays strict JSON")
    nan_probe = os.path.join(SELFTEST_DIR, "nan-probe.json")
    write_json_atomic(nan_probe, {
        "a": float("nan"), "b": float("inf"),
        "c": [float("-inf"), 1.5], "d": {"e": float("nan")},
    })
    with open(nan_probe) as fh:
        raw = fh.read()
    loaded = json.loads(raw)
    check("non-finite floats serialize as strings, never NaN/Infinity",
          "NaN" not in raw and "Infinity" not in raw
          and loaded["a"] == "nan" and loaded["b"] == "inf"
          and loaded["c"][0] == "-inf" and loaded["d"]["e"] == "nan")

    print("preflight artifact binding")
    ledger_path = build_delivery(month_lines())
    payload = run_preflight(fake_dir, ledger_path)
    st_artifact = os.path.join(SELFTEST_DIR, "preflight-pass.json")
    write_json_atomic(st_artifact, payload)
    got, digest = require_preflight(payload["file_hashes"], st_artifact)
    check("a matching preflight artifact is accepted and hashed",
          got["rows"] == payload["rows"] and len(digest) == 64)
    check("mismatched delivery hashes refuse the stale artifact",
          refuses(lambda: require_preflight({"x": "y"}, st_artifact),
                  "re-run preflight"))
    tampered = dict(payload, subcontract_hash="0" * 64)
    write_json_atomic(st_artifact, tampered)
    check("a sub-contract edit cannot ride on an old preflight",
          refuses(lambda: require_preflight(payload["file_hashes"],
                                            st_artifact), "sub-contract"))

    print("tolerance boundaries, inclusive in every class")
    check("relative at exactly the bound passes",
          within("relative", 0.10, 1.10, 1.0)
          and not within("relative", 0.10, 1.101, 1.0))
    check("absolute at exactly the bound passes",
          within("absolute", 0.05, 0.55, 0.5)
          and not within("absolute", 0.05, 0.551, 0.5))
    check("ceiling at exactly the bound passes",
          within("ceiling", 0.10, 0.10, None)
          and not within("ceiling", 0.10, 0.1001, None))
    check("size_tail allows the larger of one contract or 20 percent",
          within("size_tail", 0.20, 3, 2)
          and within("size_tail", 0.20, 12, 10)
          and not within("size_tail", 0.20, 13, 10))
    check("exact is exact",
          within("exact", 0, 1, 1) and not within("exact", 0, 1, 2))

    print("the solve mechanics")
    solve = solve_scalar(lambda x: abs(x - 3.2), 0.0, 10.0, 11,
                         log_domain=False)
    check("trisection converges on a plain objective",
          abs(solve["best_candidate"] - 3.2) < 0.01)
    solve = solve_scalar(lambda x: 0.0, 0.0, 10.0, 11, log_domain=False)
    check("a flat objective tie-breaks to the smaller candidate",
          solve["best_candidate"] <= 1.0)
    solve = solve_scalar(lambda x: -x, 0.0, 10.0, 11, log_domain=False)
    check("a boundary winner refines its single inside neighbor interval",
          abs(solve["best_candidate"] - 10.0) < 0.02)
    seeded_calls: list[float] = []
    seeded_best, _score, _term, _n = trisect(
        lambda x: (seeded_calls.append(x), abs(x - 2.0))[1],
        0.0, 3.0, log_domain=False, absolute_step=0.1,
        seeds=((0.0, 0.5), (3.0, 1.0)),
    )
    check("seeded endpoints are never re-evaluated",
          0.0 not in seeded_calls and 3.0 not in seeded_calls)
    check("the fresh interior pair, not the seeds, decides the bracket",
          abs(seeded_best - 2.0) < 0.1)
    solve = solve_scalar(
        lambda x: abs(x - 3.2), 0.0, 10.0, 11, log_domain=False,
        objective_threshold=0.25,
    )
    check("an objective threshold stops after the coarse grid",
          solve["termination"] == "objective <= 0.25"
          and solve["evaluations"] == 11)
    solve = solve_scalar(lambda x: 0.0, 1e-8, 1e-4, 11, log_domain=True)
    check("log-domain relative termination reads the log span directly",
          solve["termination"] == f"relative step <= {SOLVE_RELATIVE_STEP}"
          and solve["best_candidate"] <= 1.001e-8
          and solve["evaluations"] < 60)
    calls: list[float] = []
    solve_scalar(lambda x: (calls.append(x), abs(x - 5))[1],
                 0.0, 10.0, 11, log_domain=False)
    first = list(calls)
    calls.clear()
    solve_scalar(lambda x: (calls.append(x), abs(x - 5))[1],
                 0.0, 10.0, 11, log_domain=False)
    check("the search is deterministic end to end (CRN by construction)",
          calls == first)

    print("the fit driver against a fake generator")

    def fake_summary_factory(state):
        def fake(overrides, seed, start_ns, length, warmup):
            state["calls"] += 1
            median = float(overrides.get("generator.latent_size_median",
                                         "1.0"))
            frac = float(overrides.get("generator.size_round_frac", 0.2))
            disp = float(overrides.get(
                "generator.trade_displacement_ticks.ticks", 0.5))
            vol = float(overrides.get("generator.vol_scalar", 1e-6))
            width = int(overrides.get("generator.quoted_width.ticks", 1))
            med = float(overrides.get("generator.mean_event_duration_s",
                                      0.17104))
            cm = float(overrides.get("generator.children_mean", 8.49))
            csf = float(overrides.get("generator.children_single_frac",
                                      0.5587))
            lm = float(overrides.get("generator.levels_mean", 2.2471))
            parents = 1000
            singles = int(csf * parents)
            # A generator whose outputs equal its inputs: cadence and vol
            # reproduce exactly, displacement lands at the scalar, sizes are
            # a two-point histogram whose balance follows the median. Its
            # size histogram is sided-parents-only, so it never exercises
            # the (preflight-bounded) all-prints asymmetry of the observed
            # 4.3 population.
            ones = max(1, int(parents * (1.0 / (1.0 + median))))
            fives = parents - ones
            disp_bin = math.floor(disp / DISPLACEMENT_BIN_TICKS)
            return {
                "seed": seed, "parents": parents,
                "sided_rows": int(cm * parents),
                "single_parents": singles,
                "level_count_sum": int(lm * parents),
                "gap_sum_ns": int(med * 1e9 * parents),
                "eligible_gaps": parents,
                "size_histogram": {"1": ones, "5": fives},
                "bid_size_histogram": {"3": parents},
                "ask_size_histogram": {"4": parents},
                "width_ticks_histogram": {str(width): parents},
                "buyer_displacement_hist": {
                    f"{disp_bin * DISPLACEMENT_BIN_TICKS:.2f}": parents // 2
                },
                "seller_displacement_hist": {
                    f"{disp_bin * DISPLACEMENT_BIN_TICKS:.2f}": parents // 2
                },
                "mid_return_count": parents,
                "mid_return_sum": 0.0,
                "mid_return_sumsq": (vol * 10.0) ** 2 * parents,
                # Minute ranges scale with vol so the volatility family's
                # envelope gates see a generator the solve can steer: at
                # the solved scalar the single range key sits inside the
                # observed envelope, and an inflated scalar overshoots it.
                "minute_range_ticks_hist": {
                    str(max(1, int(vol * 1.2e6))): 500,
                },
                "minute_range_max_ticks": max(1, int(vol * 1.2e6)),
                "minute_range_second_max_ticks": max(1, int(vol * 1.2e6)),
                "horizon_vol": {},
                "first_book_mid": overrides.get("generator.start_price",
                                                "21000"),
                "measured_from_ns": start_ns,
                "measured_until_ns": start_ns,
            }
        return fake

    ledger_path = build_delivery(month_lines())
    payload = run_preflight(fake_dir, ledger_path)
    st_preflight = os.path.join(SELFTEST_DIR, "driver-preflight.json")
    write_json_atomic(st_preflight, payload)
    state = {"calls": 0}
    artifact = run_fit(directory=fake_dir,
                       run_summary=fake_summary_factory(state),
                       harness_commit="selftest",
                       ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    check("the fake-generator fit produces a bound artifact",
          artifact["binding"]["harness_tree_commit"] == "selftest"
          and artifact["binding"]["subcontract_hash"] == subcontract_hash())
    check("cadence reproduces exactly and every cadence target lands",
          all(artifact["verdicts"][t]["status"] == "fitted"
              for t in FAMILY_SLOTS["cadence"]))
    check("the displacement solve converges on its target",
          abs(artifact["fitted_candidates"]["trade_displacement_ticks"]
              - artifact["observed"]["displacement_median_ticks"])
          <= 3 * DISPLACEMENT_BIN_TICKS)
    # The all-buyer month has no seller prints: the seller side gate must
    # be vacuous and say so, never a NaN comparison failing quietly.
    disp_verdict = artifact["verdicts"]["trade_displacement_ticks"]
    check("a side absent from the data gates vacuously and says so",
          disp_verdict["checks"]["probe"]["displacement_side_A"] is True
          and "vacuous"
          in disp_verdict["observed"]["displacement_side_A"])
    # The moot guard (amendment, 2026-08-05): a one-lot tape leaves both
    # preconditions below the floor, so model B is SKIPPED as
    # structurally moot and the skip record carries the precondition
    # values - this supersedes the pre-amendment behavior of solving all
    # 51 fracs before the identifiability decision.
    moot_b = artifact["solves"]["latent_size_median"]["model_b"]
    check("model B is skipped structurally moot on a one-lot tape",
          moot_b["skipped"] == "structurally-moot"
          and moot_b["observed_p50"] == 1
          and moot_b["model_a_median"]
          < SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
          and moot_b["floor"] == SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
          and artifact["solves"]["latent_size_median"]["chosen_model"]
          == "A")
    check("size_round_frac stays declared on the moot branch",
          "size_round_frac" not in artifact["landing_set"]
          and artifact["verdicts"]["size_round_frac"]["status"]
          == "declared-unidentifiable")
    check("the quote family gates width AND both top sizes",
          set(artifact["verdicts"]["quoted_width"]["checks"]["probe"])
          == {"width"}
          and set(artifact["verdicts"]["top_sizes"]["checks"]["probe"])
          == {"top_bid", "top_ask"})
    check("start_price carries its slot in the landing set when it passes",
          ("start_price" in artifact["landing_set"])
          == (artifact["verdicts"]["start_price"]["status"] == "fitted"))
    check("every verdict is per target with tolerance and measured values",
          all(
              {"family", "status", "tolerance", "measured", "observed",
               "checks"} <= set(artifact["verdicts"][t])
              and artifact["verdicts"][t]["family"] == fam
              and artifact["verdicts"][t]["measured"]["probe"] is not None
              for t, fam, _metrics in TARGETS
          ))
    check("a fitted target's measured value sits inside its tolerance",
          artifact["verdicts"]["children_mean"]["tolerance"]
          == {"children_mean": ["relative", 0.10]}
          and artifact["verdicts"]["children_mean"]["measured"]["combined"]
          ["children_mean"]
          == artifact["observed"]["children_mean"])
    check("the landing set derives from target verdicts alone",
          artifact["landing_set"]
          == sorted(t for t, v in artifact["verdicts"].items()
                    if v["status"] == "fitted"))
    sigma_solve = artifact["solves"]["size_log_sigma"]
    check("the sigma grid is solved per sigma with the winner recorded",
          len(sigma_solve["per_sigma"]) == SIGMA_GRID_POINTS
          and sigma_solve["best_candidate"] in sigma_solve["coarse_grid"]
          and artifact["fitted_candidates"]["size_log_sigma"]
          == sigma_solve["best_candidate"])
    check("size_log_sigma carries its own per-target verdict",
          artifact["verdicts"]["size_log_sigma"]["family"] == "size")
    vol_probe = artifact["verdicts"]["vol_scalar"]["checks"]["probe"]
    check("the minute-range gates judge the volatility probe per seed",
          all(vol_probe[f"minute_range_{stat}"] is True
              for stat in MINUTE_RANGE_GATES))
    # HETEROGENEOUS session blocks with one RARE extreme (a single 999 in
    # one session): sampling rows instead of sessions, ignoring the frozen
    # seed, drawing the wrong session count, or moving the envelope level
    # each produce a DIFFERENT exact result than the frozen expectation -
    # p99 pins the common tier while the tails pin the rare-session draw
    # composition. Computed once under RESAMPLE_SEED 1 and frozen; the
    # determinism property under test IS the freeze.
    labels = [l for l, s in SESSION_INVENTORY if s == "full"]
    hetero = {
        label: [(i * 7 + j) % 60 for j in range(50)]
        for i, label in enumerate(labels)
    }
    hetero[labels[7]] = hetero[labels[7]] + [999]
    env_a = minute_range_envelope(hetero)
    env_b = minute_range_envelope(hetero)
    check("the resampled envelope is deterministic under its frozen seed",
          env_a == env_b and set(env_a) == {"p99", "p99.9", "p99.99", "max"})
    check("the heterogeneous envelope matches its frozen expectation",
          env_a == {"p99": 59, "p99.9": 999, "p99.99": 999, "max": 999})
    both = summaries_for(fake_summary_factory({"calls": 0}),
                         {}, (1, 2), 0, "1d", with_seeds=True)
    check("with_seeds returns the NAMED pooled and per_seed members",
          set(both) == {"pooled", "per_seed"} and len(both["per_seed"]) == 2
          and both["pooled"]["parents"] == 2000)

    print("an inflated volatility overshoots the minute envelope")

    def inflated_minutes_summary(overrides, seed, start_ns, length, warmup):
        base = fake_summary_factory({"calls": 0})(
            overrides, seed, start_ns, length, warmup
        )
        if "generator.vol_scalar" in overrides:
            base["minute_range_ticks_hist"] = {"100": 500}
            base["minute_range_max_ticks"] = 100
            base["minute_range_second_max_ticks"] = 100
        return base

    write_json_atomic(st_preflight, payload)
    inflated = run_fit(directory=fake_dir,
                       run_summary=inflated_minutes_summary,
                       harness_commit="selftest",
                       ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    inflated_checks = inflated["verdicts"]["vol_scalar"]["checks"]["probe"]
    check("minute gates fail the volatility family while mid_rms passes",
          inflated_checks["mid_rms"] is True
          and inflated_checks["minute_range_max"] is False
          and inflated["verdicts"]["vol_scalar"]["status"]
          == "declared-misrepresented"
          and "vol_scalar" not in inflated["landing_set"])
    check("every solve record carries the frozen schema fields",
          all(
              {"domain", "coarse_points", "coarse_grid", "best_candidate",
               "termination",
               "tie_break", "evaluations"}
              <= set(artifact["solves"][k] if k != "latent_size_median"
                     else artifact["solves"][k]["model_a"])
              for k in ("latent_size_median", "size_log_sigma",
                        "trade_displacement_ticks", "vol_scalar")
          )
          and "per_sigma" in artifact["solves"]["size_log_sigma"]
          and "final_score" in artifact["solves"]["size_log_sigma"])
    check("the sigma record's cost fields describe the sigma search",
          sigma_solve["evaluations"] == SIGMA_GRID_POINTS
          and sigma_solve["termination"]
          == "fixed 16-point sigma grid exhausted, no refinement"
          and sigma_solve["nested_median_evaluations"] == sum(
              entry["record"]["evaluations"]
              for entry in sigma_solve["per_sigma"]
          ))

    print("the moot guard opens when the observed p50 reaches the floor")
    ledger_path = build_delivery(month_lines(size=12))
    payload12 = run_preflight(fake_dir, ledger_path)
    write_json_atomic(st_preflight, payload12)
    artifact = run_fit(directory=fake_dir,
                       run_summary=fake_summary_factory({"calls": 0}),
                       harness_commit="selftest", ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    open_b = artifact["solves"]["latent_size_median"]["model_b"]
    check("a 12-lot tape solves model B in full, no skip record",
          "skipped" not in open_b
          and "search_score" in open_b
          and "frac" in open_b
          and artifact["observed"]["size_quantiles"]["p50"] == 12)
    # Restore the one-lot delivery the following sections' ledger_path
    # and payload refer to.
    ledger_path = build_delivery(month_lines())
    payload = run_preflight(fake_dir, ledger_path)

    print("family isolation")

    def broken_vol_summary(overrides, seed, start_ns, length, warmup):
        base = fake_summary_factory({"calls": 0})(
            overrides, seed, start_ns, length, warmup
        )
        if "generator.vol_scalar" in overrides:
            base["mid_return_sumsq"] = float("nan")
        return base

    write_json_atomic(st_preflight, payload)
    artifact = run_fit(directory=fake_dir,
                       run_summary=broken_vol_summary,
                       harness_commit="selftest",
                       ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    check("a failed volatility family does not block cadence landing",
          artifact["verdicts"]["vol_scalar"]["status"]
          == "declared-misrepresented"
          and artifact["verdicts"]["children_mean"]["status"] == "fitted")
    check("the failed family's slot stays out of the landing set",
          "vol_scalar" not in artifact["landing_set"]
          and "children_mean" in artifact["landing_set"])

    print("side-vs-side displacement gates catch unrepresented asymmetry")

    # Buyers print 3 ticks above mid, sellers 1 tick: the pooled median is
    # the seller's 1.025, so a symmetric generator solved to the pooled
    # target passes the pooled gate while sitting 2 ticks from the observed
    # buyer median - only the side-vs-side gate catches it.
    def asym_month_lines():
        lines = []
        for label, status in SESSION_INVENTORY:
            if status != "full":
                continue
            base = _st_ts(label, 12)
            for i in range(100):
                w = (i % 3) * TICK_UNITS
                bid = 23_000 * 10**9 - 2 * TICK_UNITS + w
                ask = 23_000 * 10**9 + w
                if i % 2 == 0:
                    lines.append(_st_row(base + i * 10**9, side="B",
                                         price=ask + 2 * TICK_UNITS,
                                         bid=bid, ask=ask))
                else:
                    lines.append(_st_row(base + i * 10**9, side="A",
                                         price=bid, bid=bid, ask=ask))
        return lines

    ledger_path = build_delivery(asym_month_lines())
    payload = run_preflight(fake_dir, ledger_path)
    write_json_atomic(st_preflight, payload)
    artifact = run_fit(directory=fake_dir,
                       run_summary=fake_summary_factory({"calls": 0}),
                       harness_commit="selftest", ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    asym_verdict = artifact["verdicts"]["trade_displacement_ticks"]
    check("the pooled gate passes while the buyer side gate fails",
          asym_verdict["checks"]["probe"]["displacement_median"] is True
          and asym_verdict["checks"]["probe"]["displacement_side_B"] is False
          and asym_verdict["status"] == "declared-misrepresented")
    check("the side targets are the observed per-side medians",
          asym_verdict["observed"]["displacement_side_B"]
          == artifact["observed"]["displacement_buyer_median_ticks"]
          and asym_verdict["observed"]["displacement_side_A"]
          == artifact["observed"]["displacement_seller_median_ticks"])

    print("a wholesale cadence failure stops the landing outright")

    def bad_cadence_summary(overrides, seed, start_ns, length, warmup):
        base = fake_summary_factory({"calls": 0})(
            overrides, seed, start_ns, length, warmup
        )
        # 30% too many children everywhere: cadence misses its 10% band.
        base["sided_rows"] = int(base["sided_rows"] * 1.3)
        return base

    write_json_atomic(st_preflight, payload)
    artifact = run_fit(directory=fake_dir, run_summary=bad_cadence_summary,
                       harness_commit="selftest", ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    check("cadence failure empties the landing set entirely",
          artifact["landing_set"] == []
          and artifact["verdicts"]["children_mean"]["status"]
          == "declared-misrepresented"
          and artifact["verdicts"]["start_price"]["status"] == "stopped"
          and artifact["verdicts"]["vol_scalar"]["status"] == "stopped")

    print("a combined-run cadence failure also stops the landing")

    def combined_cadence_failure(overrides, seed, start_ns, length, warmup):
        base = fake_summary_factory({"calls": 0})(
            overrides, seed, start_ns, length, warmup
        )
        # Only the combined profile carries cadence AND quote seams
        # together; the cadence probe alone stays faithful, so the probe
        # passes and the combined run contradicts it.
        if ("generator.children_mean" in overrides
                and "generator.quoted_width.ticks" in overrides):
            base["sided_rows"] = int(base["sided_rows"] * 1.3)
        return base

    write_json_atomic(st_preflight, payload)
    artifact = run_fit(directory=fake_dir,
                       run_summary=combined_cadence_failure,
                       harness_commit="selftest", ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    check("a combined-run cadence contradiction stops the landing",
          artifact["landing_set"] == []
          and artifact["verdicts"]["children_mean"]["status"]
          == "declared-misrepresented"
          and artifact["verdicts"]["quoted_width"]["status"] == "stopped")

    print("a combined-run refusal lands the artifact and fits nothing")

    def broken_combined_summary(overrides, seed, start_ns, length, warmup):
        # Only the combined run merges cadence with the quote seams; a
        # family probe never carries both.
        if ("generator.children_mean" in overrides
                and "generator.quoted_width.ticks" in overrides):
            raise Refusal("combined walk fixture failure")
        return fake_summary_factory({"calls": 0})(
            overrides, seed, start_ns, length, warmup
        )

    write_json_atomic(st_preflight, payload)
    artifact = run_fit(directory=fake_dir,
                       run_summary=broken_combined_summary,
                       harness_commit="selftest", ledger_path=ledger_path,
                       preflight_artifact_path=st_preflight)
    check("the combined-run failure is recorded and nothing lands fitted",
          artifact["landing_set"] == []
          and all(v["status"] != "fitted"
                  for v in artifact["verdicts"].values())
          and "combined walk fixture failure"
          in artifact["verdicts"]["children_mean"]["reason"])

    print("an extra manifest file breaks exact inventory equality")
    ledger_path = build_delivery(month_lines())
    manifest_path = os.path.join(fake_dir, "manifest.json")
    with open(manifest_path) as fh:
        manifest = json.load(fh)
    manifest["files"]["surprise.json"] = "0" * 64
    with open(manifest_path, "w") as fh:
        json.dump(manifest, fh)
    check("an extra manifest entry refuses, naming only-manifest",
          refuses(lambda: run_preflight(fake_dir, ledger_path),
                  "only manifest"))

    print(f"{checks} check(s), 0 failed")
    print("selftest PASS")


def main() -> None:
    if len(sys.argv) != 2 or sys.argv[1] not in ("selftest", "preflight",
                                                 "fit"):
        raise SystemExit(__doc__)
    mode = sys.argv[1]
    try:
        if mode == "selftest":
            run_selftest()
        elif mode == "preflight":
            mode_preflight()
        else:
            mode_fit()
    except Refusal as exc:
        # The refusal messages are the interface; a raw traceback buries
        # them.
        raise SystemExit(f"refused: {exc}") from None


if __name__ == "__main__":
    main()
