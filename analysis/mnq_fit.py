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

# Inverse-solve contract (protocol-10 spec 4.75, carried forward): the one
# remaining solve is vol_scalar. The size, sigma, frac and displacement
# grids left with their solves at protocol 11.
SOLVE_RELATIVE_STEP = 1e-3
VOL_GRID_POINTS = 32
VOL_SCALAR_DOMAIN = (1e-8, 1e-4)

# Displacement histogram bin (the observed displacement DIAGNOSTICS keep
# this convention; the displacement solve itself is gone).
DISPLACEMENT_BIN_TICKS = 0.05

# The protocol-11 session refit sub-contract
# (notes/protocol-11-session-repair-spec.md section 4). The cell floors are
# per (session, hour): parent-return cells need 1000 observations, horizon
# cells the maxima hour 20 can actually reach under the segment-aligned
# boundary rules (42 for 60 s, 6 for 300 s - the floors sit at 40 and 6).
MIN_PARENT_CELL_RETURNS = 1000
MIN_60S_CELL_RETURNS = 40
MIN_300S_CELL_RETURNS = 6
SESSION_HOUR_BAND = (0.8, 1.25)
ARRIVAL_HOUR_REL_TOL = 0.10
WALLTIME_POOLED_REL_TOL = 0.15
SESSION_ARRAY_DECIMALS = 6
TOP_MINUTE_RECORDS = 32
# The generator's weekly calendar has no July 3 holiday, so the FINAL
# window must carry exactly this many complete generated sessions per seed;
# any other count refuses (spec 4.5).
GENERATED_SESSIONS_PER_SEED = 23
# Mirrors the Brick T threshold frozen in the mogwai-data test; recorded
# here so the sub-contract hash binds the whole constants block.
SESSION_VOL_CORR_MIN = 0.90
# The shipped MNQ dow_weight, byte-for-byte (crates/mogwai-server/presets/
# mnq.toml): FROZEN, never refitted here (spec 2.3). The conditional
# intensity estimator solves the hour parameter GIVEN this day factor, and
# the Brick L preset test pins that the preset still carries exactly these
# values. Sun=0 .. Sat=6.
MNQ_DOW_WEIGHT = (1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000)

# Representability tolerances, target-local, boundaries inclusive. The
# protocol-10 size/quote/displacement/start-price rows are gone WITH their
# solves: the protocol-11 fit mode never executes them (spec 4.4), those
# preset values stay byte-for-byte at their landed protocol-10 readings,
# and the deleted machinery lives in git history bound to the protocol-10
# evidence commit.
TOLERANCES = {
    "mean_event_duration_s": ("relative", 0.10),
    "children_mean": ("relative", 0.10),
    "children_single_frac": ("absolute", 0.05),
    "levels_mean": ("relative", 0.15),
    "mid_rms": ("relative", 0.10),
    # One-sided upper gates against the session-block-resampled observed
    # envelope, judged per seed; the bound is data-derived at fit time
    # under the frozen RESAMPLE_* constants, not a constant here.
    "minute_range_p99": ("envelope_upper", "resampled"),
    "minute_range_p99.9": ("envelope_upper", "resampled"),
    "minute_range_max": ("envelope_upper", "resampled"),
    # Protocol-11 session gates (spec 4.5): per-exposed-hour checks, no
    # material-share escape, hour 21 excluded. `band` is a multiplicative
    # band on generated/observed.
    "session_arrival_hour": ("relative", ARRIVAL_HOUR_REL_TOL),
    "session_vol_hour": ("band", SESSION_HOUR_BAND),
    "walltime_hour_60": ("band", SESSION_HOUR_BAND),
    "walltime_hour_300": ("band", SESSION_HOUR_BAND),
    "walltime_pooled_60": ("relative", WALLTIME_POOLED_REL_TOL),
    "walltime_pooled_300": ("relative", WALLTIME_POOLED_REL_TOL),
}

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
    "VOL_GRID_POINTS", "VOL_SCALAR_DOMAIN", "DISPLACEMENT_BIN_TICKS",
    "TOLERANCES", "ACF_LAGS", "HORIZON_SECONDS",
    "REFERENCE_SHAPE",
    "RESAMPLE_SEED", "RESAMPLE_REPLICATES",
    "RESAMPLE_SESSIONS_PER_REPLICATE", "RESAMPLE_ENVELOPE_LEVEL",
    "MINUTE_RANGE_GATES",
    "MIN_PARENT_CELL_RETURNS", "MIN_60S_CELL_RETURNS",
    "MIN_300S_CELL_RETURNS", "SESSION_HOUR_BAND", "ARRIVAL_HOUR_REL_TOL",
    "WALLTIME_POOLED_REL_TOL", "SESSION_ARRAY_DECIMALS",
    "TOP_MINUTE_RECORDS", "GENERATED_SESSIONS_PER_SEED",
    "SESSION_VOL_CORR_MIN", "MNQ_DOW_WEIGHT",
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


def segment_end_ns(session: str, segment: str) -> int:
    """The calendar END of a session segment as a UTC epoch-ns instant:
    the trade date's 15:15 local (halt start) for `overnight`, its 16:00
    local (close) for `post_halt`. Horizon boundaries live strictly inside
    [origin, end) - the halt-start and close boundaries are never emitted
    (protocol-11 spec 4.6)."""
    day = dt.date.fromisoformat(session)
    local_min = HALT_START_LOCAL_MIN if segment == "overnight" \
        else SESSION_CLOSE_LOCAL_MIN
    base = dt.datetime(day.year, day.month, day.day,
                       tzinfo=dt.timezone.utc).timestamp()
    return int(base + local_min * 60 - UTC_OFFSET_MINUTES * 60) \
        * 1_000_000_000


def utc_hour_dow(ts_ns: int) -> tuple[int, int]:
    """(UTC hour, UTC day-of-week with Sun=0) - the runtime's own keying
    (mogwai-data session.rs utc_hour_dow), NOT the exchange-local hour the
    legacy session-curve diagnostics use."""
    s = ts_ns // 1_000_000_000
    return int((s % 86_400) // 3_600), int((s // 86_400 + 4) % 7)


def exposure_by_hour_dow(sessions: list[str]) -> list[list[int]]:
    """Calendar-open minutes per (UTC hour, UTC dow) cell, summed across the
    given session labels. Exposure comes from the calendar, never from row
    presence (the session-fit rule). 24 x 7, hour-major, Sun=0."""
    table = [[0] * 7 for _ in range(24)]
    for label in sessions:
        for segment in ("overnight", "post_halt"):
            start = segment_origin_ns(label, segment)
            end = segment_end_ns(label, segment)
            minute = start
            while minute < end:
                hour, dow = utc_hour_dow(minute)
                table[hour][dow] += 1
                minute += 60_000_000_000
    return table


def weekly_exposure_table() -> list[list[int]]:
    """One canonical calendar week's open minutes per (UTC hour, UTC dow):
    the W[h,d] table the conditional intensity normalization sums over
    (spec 4.3). Built from the five weekday sessions of an arbitrary
    holiday-free week; the weekly calendar repeats, so any such week is
    THE week."""
    # 2026-07-06 (Mon) through 2026-07-10 (Fri): the frozen SEARCH week.
    return exposure_by_hour_dow(
        ["2026-07-06", "2026-07-07", "2026-07-08", "2026-07-09",
         "2026-07-10"]
    )


def cell_scale(count: int, sum_abs: float, max_abs: float) -> float:
    """The one-maximum-trimmed mean absolute return (spec 4.1)."""
    return (sum_abs - max_abs) / (count - 1)


def exposed_utc_hours() -> list[int]:
    """The 23 exposed UTC hours: every hour except 21 (the daily break
    covers it entirely under the permanent-CDT calendar). Hour 20 is
    partially exposed at 45 open minutes."""
    return [h for h in range(24) if h != 21]


def hour_exposure_weights() -> dict[int, int]:
    """Open minutes per UTC hour in one full session: 60 everywhere except
    hour 20 (45, the halt) and hour 21 (0, the break). The weights of the
    shared hour-only normalization rule (spec 4.2)."""
    return {h: (0 if h == 21 else 45 if h == 20 else 60) for h in range(24)}


def normalize_hour_curve(raw: dict[int, float]) -> dict[int, float]:
    """The shared hour-only normalization (spec 4.2): divide every exposed
    value by the open-minute-exposure-weighted mean, summing in ascending
    UTC-hour order in binary64. Hour 21 is set to exactly 1.0."""
    weights = hour_exposure_weights()
    num = 0.0
    den = 0.0
    for h in sorted(raw):
        if h == 21:
            continue
        if not math.isfinite(raw[h]):
            raise Refusal(
                f"hour curve carries a non-finite value at hour {h}; a "
                "required session-refit value refuses rather than "
                "serializing as a string"
            )
        num += raw[h] * weights[h]
        den += weights[h]
    mean = num / den
    if not (math.isfinite(mean) and mean > 0):
        raise Refusal(
            "hour curve has a nonpositive or non-finite exposure-weighted "
            "mean; no real evidence produces this"
        )
    return {h: (1.0 if h == 21 else raw[h] / mean) for h in range(24)}


def materialize_curve(normalized: dict[int, float]) -> list[float]:
    """SESSION_ARRAY_DECIMALS materialization (spec 4.2): the materialized
    array, not the unrounded one, is what scratch profiles carry, FINAL
    gates judge, and the preset ships."""
    return [
        float(format(normalized[h], f".{SESSION_ARRAY_DECIMALS}f"))
        for h in range(24)
    ]


def curve_triple(raw: dict[int, float]) -> dict:
    """The raw / normalized_unrounded / materialized record a FITTED curve
    carries in the artifact (spec Brick H). Unexposed entries are null in
    the first two; the materialized hour-21 entry is 1.0. Materialization
    exists for INSTALLED arrays; evidence targets use curve_pair."""
    normalized = normalize_hour_curve(raw)
    return {
        "raw": [raw.get(h) for h in range(24)],
        "normalized_unrounded": [
            None if h == 21 else normalized[h] for h in range(24)
        ],
        "materialized": materialize_curve(normalized),
    }


def curve_pair(raw: dict[int, float]) -> dict:
    """Raw and normalized only - the shape of observed EVIDENCE targets
    (the marginal arrival target, the wall-time hourly curves). These are
    never installed into a preset, so they carry no materialized form and
    the gates judge the normalized values unrounded."""
    normalized = normalize_hour_curve(raw)
    return {
        "raw": [raw.get(h) for h in range(24)],
        "normalized": [
            None if h == 21 else normalized[h] for h in range(24)
        ],
    }


def hourly_robust_curve(cells: dict, sessions: list[str], floor: int,
                        what: str) -> dict[int, float]:
    """The per-hour robust scale (spec 4.1 steps 4-6): every session must
    supply a qualifying cell for every exposed hour or the refit REFUSES;
    the hourly value is the nearest-rank median of cell scales."""
    curve: dict[int, float] = {}
    for hour in exposed_utc_hours():
        scales = []
        for session in sessions:
            cell = cells.get(session, {}).get(str(hour))
            if cell is None or cell["count"] < floor:
                have = 0 if cell is None else cell["count"]
                raise Refusal(
                    f"{what}: session {session} hour {hour} has {have} "
                    f"returns against the floor of {floor}; every session "
                    "must qualify for every exposed hour"
                )
            scales.append(cell_scale(cell["count"], cell["sum_abs"],
                                     cell["max_abs"]))
        curve[hour] = nearest_rank_list(sorted(scales), 0.5)
    return curve


def fit_vol_hour(observed: dict, usable: list[str]) -> dict:
    """The vol_hour refit (spec 4.2)."""
    raw = hourly_robust_curve(
        observed["session_refit_raw"]["parent_vol_cells"], usable,
        MIN_PARENT_CELL_RETURNS, "parent-vol cell",
    )
    return curve_triple(raw)


def fit_intensity_hour(observed: dict, usable: list[str]) -> dict:
    """The conditional intensity refit under frozen dow_weight (spec 4.3).

    The runtime applies intensity_hour[h] * dow_weight[d], and UTC hours
    22-23 run on a Sun-Thu day mix while hours 0-20 run Mon-Fri, so
    normalizing the marginal rate and retaining dow_weight would apply day
    concentration twice. q[h] is the closed-form hour estimate with the
    day factor frozen; the presentation normalization sums the composite
    over the canonical week's exposure table."""
    c_hd = observed["session_refit_raw"]["parent_count_by_hour_dow"]
    e_hd = exposure_by_hour_dow(usable)
    w = MNQ_DOW_WEIGHT
    q: dict[int, float] = {}
    marginal_raw: dict[int, float] = {}
    for hour in exposed_utc_hours():
        counts = sum(c_hd[hour])
        exposure = sum(e_hd[hour])
        weighted = sum(e_hd[hour][d] * w[d] for d in range(7))
        if exposure == 0 or weighted == 0:
            raise Refusal(f"intensity: exposed hour {hour} has no exposure")
        q[hour] = counts / weighted
        marginal_raw[hour] = counts / exposure
    week = weekly_exposure_table()
    num = 0.0
    den = 0.0
    for hour in sorted(q):
        for d in range(7):
            num += week[hour][d] * q[hour] * w[d]
            den += week[hour][d]
    z = num / den
    conditional = {h: (1.0 if h == 21 else q[h] / z) for h in range(24)}
    return {
        "raw": [q.get(h) for h in range(24)],
        "normalized_unrounded": [
            None if h == 21 else conditional[h] for h in range(24)
        ],
        "materialized": [
            float(format(conditional[h], f".{SESSION_ARRAY_DECIMALS}f"))
            if h != 21 else 1.0
            for h in range(24)
        ],
        # The MARGINAL target the session_arrival gate compares generated
        # marginal rates against - never the conditional array (spec 4.3).
        # Evidence, not an installed array: raw and normalized only.
        "marginal_target": curve_pair(marginal_raw),
        "parent_count_by_hour_dow": c_hd,
        "open_minutes_by_hour_dow": e_hd,
        "dow_weight": list(w),
    }


def observed_walltime_curves(observed: dict, usable: list[str]) -> dict:
    """The observed hourly wall-time robust curves at both horizons
    (spec 4.6), plus the pooled RMS the pooled gates read."""
    cells = observed["session_refit_raw"]["horizon_cells"]
    floors = {60: MIN_60S_CELL_RETURNS, 300: MIN_300S_CELL_RETURNS}
    out = {}
    for h in HORIZON_SECONDS:
        raw = hourly_robust_curve(
            cells[str(h)], usable, floors[h], f"{h}s horizon cell",
        )
        out[str(h)] = {
            # Evidence targets, never installed: raw and normalized only,
            # judged unrounded (spec walltime_block).
            "hourly": curve_pair(raw),
            "pooled_rms":
                observed["session_refit_raw"]["walltime_pooled"][str(h)]["rms"],
            "return_count":
                observed["session_refit_raw"]["walltime_pooled"][str(h)]["count"],
        }
    return out


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
    # Protocol-11 session-refit cells (spec 4.1-4.6), keyed on UTC hour -
    # the runtime's own keying, unlike the exchange-local legacy curves.
    pv_cells: dict[tuple, list] = {}     # (session, hour) -> [n, sum, max]
    c_hd = [[0] * 7 for _ in range(24)]  # parent counts, hour x dow
    hz_cells = {60: {}, 300: {}}         # (session, hour) -> [n,s,sq,sa,ma]
    hz_pooled = {60: [0, 0.0, 0.0], 300: [0, 0.0, 0.0]}
    # The 4.6 horizon chains, SEPARATE from the legacy hz state: the new
    # convention settles trailing boundaries through the segment end (the
    # generated side does, and zero returns from a quiet tail are signal),
    # where the legacy chain drops boundaries after the segment's final
    # parent. key/end/state/last_mid mirror the generated implementation.
    nhz = {"key": None, "end": 0, "state": {}, "last_mid": None}

    def nhz_boundary(h: int, st: dict, session: str, boundary: int,
                     as_of) -> None:
        # One 4.6 boundary: establish on the first as-of, then emit unless
        # the window crosses a UTC hour boundary (rule 7); attribution is
        # the endpoint hour (rule 8).
        if as_of is None or as_of <= 0:
            return
        prev = st["nprev"]
        st["nprev"] = as_of
        if prev is None:
            return
        window_start = boundary - h * 10**9
        b_hour = utc_hour_dow(boundary)[0]
        if utc_hour_dow(window_start)[0] != b_hour:
            return
        r = math.log(as_of / prev)
        cell = hz_cells[h].setdefault((session, b_hour),
                                      [0, 0.0, 0.0, 0.0, 0.0])
        cell[0] += 1
        cell[1] += r
        cell[2] += r * r
        cell[3] += abs(r)
        if abs(r) > cell[4]:
            cell[4] = abs(r)
        pooled = hz_pooled[h]
        pooled[0] += 1
        pooled[1] += r
        pooled[2] += r * r

    def nhz_settle(until_exclusive: int) -> None:
        # Advance every horizon chain of the ACTIVE segment through
        # boundaries strictly before `until_exclusive` (and always strictly
        # inside the segment), using the running as-of mid.
        session = nhz["key"][0]
        for h in HORIZON_SECONDS:
            st = nhz["state"][h]
            w_ns = h * 10**9
            limit = min(until_exclusive, nhz["end"])
            while st["nnext"] < limit:
                nhz_boundary(h, st, session, st["nnext"], nhz["last_mid"])
                st["nnext"] += w_ns

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
        # Protocol-11 arrival evidence (spec 4.3): every sided parent, by
        # the UTC hour and dow of its first timestamp.
        p11_hour, p11_dow = utc_hour_dow(parent["first_ts"])
        c_hd[p11_hour][p11_dow] += 1
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
                # Protocol-11 parent-vol cell (spec 4.1): the SAME adjacent
                # valid-mid returns, zeros included, endpoint UTC hour.
                cell = pv_cells.setdefault(
                    (parent["session"], p11_hour), [0, 0.0, 0.0]
                )
                cell[0] += 1
                cell[1] += abs(r)
                if abs(r) > cell[2]:
                    cell[2] = abs(r)
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

            # Protocol-11 horizon chains (spec 4.6), separate state: on a
            # segment change, the OUTGOING segment settles through its
            # calendar end first (trailing zero returns from a quiet tail
            # are signal), then the new chains start at the new origin.
            if here != nhz["key"]:
                if nhz["key"] is not None:
                    nhz_settle(nhz["end"])
                origin = segment_origin_ns(*here)
                nhz["key"] = here
                nhz["end"] = segment_end_ns(*here)
                nhz["state"] = {
                    h: {"nnext": origin + h * 10**9, "nprev": None}
                    for h in HORIZON_SECONDS
                }
                nhz["last_mid"] = None
            nhz_settle(ts_ns)
            nhz["last_mid"] = mid_units

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
    # The final segment's protocol-11 horizon chains settle through its
    # calendar end, exactly as a segment switch would have.
    if nhz["key"] is not None:
        nhz_settle(nhz["end"])
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
        # Protocol-11 raw session-refit evidence (spec 4.1-4.6): cells and
        # sufficient statistics only; the refit constructors and floors act
        # on these downstream, so the artifact records exactly what was
        # measured before any curve is built.
        "session_refit_raw": {
            "parent_count_by_hour_dow": c_hd,
            "parent_vol_cells": {
                session: {
                    str(hour): {"count": c[0], "sum_abs": c[1],
                                "max_abs": c[2]}
                    for (s, hour), c in sorted(pv_cells.items())
                    if s == session
                }
                for session in sorted({s for s, _ in pv_cells})
            },
            "horizon_cells": {
                str(h): {
                    session: {
                        str(hour): {
                            "count": c[0], "sum": c[1], "sumsq": c[2],
                            "sum_abs": c[3], "max_abs": c[4],
                        }
                        for (s, hour), c in sorted(hz_cells[h].items())
                        if s == session
                    }
                    for session in sorted({s for s, _ in hz_cells[h]})
                }
                for h in HORIZON_SECONDS
            },
            "walltime_pooled": {
                str(h): {
                    "count": hz_pooled[h][0], "sum": hz_pooled[h][1],
                    "sumsq": hz_pooled[h][2],
                    "rms": math.sqrt(hz_pooled[h][2] / hz_pooled[h][0])
                    if hz_pooled[h][0] else float("nan"),
                }
                for h in HORIZON_SECONDS
            },
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
            elif isinstance(value, (list, tuple)):
                # Candidate session arrays (protocol-11 spec Brick H): a
                # TOML float array, full repr precision - the values are
                # already SESSION_ARRAY_DECIMALS-materialized upstream.
                body = ", ".join(repr(float(v)) for v in value)
                lines.append(f'"{path}" = [{body}]')
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


def within(kind: str, bound, generated, observed) -> bool:
    if kind == "relative":
        return abs(generated - observed) <= bound * abs(observed) + SLACK
    if kind == "absolute":
        return abs(generated - observed) <= bound + SLACK
    if kind == "ceiling":
        return generated <= bound + SLACK
    if kind == "band":
        # Multiplicative band on generated/observed (protocol-11 spec 4.5),
        # boundaries inclusive under the shared SLACK convention.
        lo, hi = bound
        return (lo * observed - SLACK <= generated
                <= hi * observed + SLACK)
    if kind == "exact":
        return generated == observed
    raise AssertionError(f"unknown tolerance kind {kind}")


def nearest_rank_of(hist: dict[int, int], q: float) -> int:
    qq = Quantiles()
    qq.counts = dict(hist)
    qq.total = sum(hist.values())
    return qq.nearest_rank(q)


# The protocol-11 gate families (spec 4.5), target-local. session_walltime
# gates the atomic landing group without owning a landable slot of its own.
FAMILIES = ("session_arrival", "session_parent_vol", "session_walltime",
            "base_volatility")

# The landable slots and the judge metrics their verdicts read. The two
# session arrays are ONE ATOMIC LANDING GROUP: both land only if all three
# session families pass at both stages and cadence passes on the combined
# profile. The landing set is derived from these verdicts ALONE.
TARGETS = (
    ("intensity_hour", "session_arrival", ("session_arrival_hour",)),
    ("vol_hour", "session_parent_vol", ("session_vol_hour",)),
    # base_volatility carries the cadence four beside its own metrics so a
    # cadence miss inside the family is visible in the verdict rather than
    # failing it with all-true checks; cadence separately gates protocol 11
    # outright (a cadence regression REFUSES, spec 4.5).
    ("vol_scalar", "base_volatility",
     ("mid_rms", "minute_range_p99", "minute_range_p99.9",
      "minute_range_max", "mean_event_duration_s", "children_mean",
      "children_single_frac", "levels_mean")),
)


def run_fit(directory: str = DELIVERY_DIR,
            run_summary=run_summary_subprocess,
            harness_commit: str | None = None,
            ledger_path: str | None = None,
            preflight_artifact_path: str = PREFLIGHT_ARTIFACT) -> dict:
    """The protocol-11 session calibration fit
    (notes/protocol-11-session-repair-spec.md). Scope, frozen: the two
    session arrays and vol_scalar, nothing else. No protocol-10 solve is
    executed - every other preset value resolves from the shipped MNQ
    preset byte-for-byte through the scratch config's `preset = "MNQ"`
    inheritance, and the deleted protocol-10 machinery lives in git
    history bound to its evidence commit."""
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

    # --- closed-form session refits (4.2, 4.3, 4.6). The OBSERVED cell
    # floors refuse HERE - sub-floor observed cells are input evidence
    # failing, which stops the analysis outright. (Generated sub-floor
    # cells later fail their FAMILY instead: a candidate tape that cannot
    # populate an hour is a measured verdict, not an input failure.)
    vol_hour_fit = fit_vol_hour(observed, usable)
    intensity_fit = fit_intensity_hour(observed, usable)
    walltime_obs = observed_walltime_curves(observed, usable)
    candidate_vol_hour = vol_hour_fit["materialized"]
    candidate_intensity = intensity_fit["materialized"]
    session_overrides = {
        "session.vol_hour": candidate_vol_hour,
        "session.intensity_hour": candidate_intensity,
    }

    # --- vol_scalar re-solve (4.4): the existing log-domain solve against
    # pooled adjacent-parent quote-mid RMS, SEARCH budget only, with both
    # MATERIALIZED candidate arrays installed. SEARCH evaluates nothing
    # else: no session cells, no hourly curves, no envelopes.
    def vol_overrides(scalar: float) -> dict:
        return dict(session_overrides,
                    **{"generator.vol_scalar": scalar})

    def vol_eval(scalar):
        gen = summaries_for(run_summary, vol_overrides(scalar),
                            SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)
        if not math.isfinite(gen["mid_rms"]) or gen["mid_rms"] <= 0:
            return float("inf")
        return abs(gen["mid_rms"] - observed["mid_rms"]) / observed["mid_rms"]

    prewarm_walks(run_summary,
                  [vol_overrides(x) for x in coarse_grid(
                      *VOL_SCALAR_DOMAIN, VOL_GRID_POINTS, log_domain=True)],
                  SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH)
    vol_solve = solve_scalar(vol_eval, *VOL_SCALAR_DOMAIN, VOL_GRID_POINTS,
                             log_domain=True, objective_threshold=0.001)
    candidate_vol_scalar = vol_solve["best_candidate"]
    solves = {"vol_scalar": dict(vol_solve, target=observed["mid_rms"])}

    # --- generated-side evidence (4.5-4.6) ---
    weights = hour_exposure_weights()
    floors = {"parent": MIN_PARENT_CELL_RETURNS,
              60: MIN_60S_CELL_RETURNS, 300: MIN_300S_CELL_RETURNS}

    def seed_curves(summary: dict) -> dict:
        cells = summary.get("session_cells") or []
        complete = [c for c in cells if c["complete"]]
        if len(complete) != GENERATED_SESSIONS_PER_SEED:
            raise Refusal(
                f"seed {summary.get('seed')}: {len(complete)} complete "
                "generated sessions against the required "
                f"{GENERATED_SESSIONS_PER_SEED}"
            )
        shortfalls: list[dict] = []

        def curve_from(kind):
            # EVERY failing seed/session/hour/count is recorded before the
            # family fails - the diagnostic trail is the point, so the scan
            # never stops at the first miss.
            raw = {}
            qualified = True
            for hour in exposed_utc_hours():
                scales = []
                for sess in complete:
                    if kind == "parent":
                        cell = sess["mid_abs_by_hour"][hour]
                    else:
                        cell = sess[f"horizon_{kind}_by_hour"][hour]
                    if cell["count"] < floors[kind]:
                        shortfalls.append({
                            "kind": str(kind), "hour": hour,
                            "seed": summary.get("seed"),
                            "session_start_ns": sess["session_start_ns"],
                            "count": cell["count"], "floor": floors[kind],
                        })
                        qualified = False
                        continue
                    scales.append(cell_scale(
                        cell["count"], cell["sum_abs"], cell["max_abs"]
                    ))
                if qualified:
                    raw[hour] = nearest_rank_list(sorted(scales), 0.5)
            if not qualified:
                return None, None
            # Evidence curves carry exposed hours only (curve24 null at 21);
            # the normalizer's conventional hour-21 value belongs to
            # installed arrays.
            normalized = {
                h: v for h, v in normalize_hour_curve(raw).items()
                if h != 21
            }
            return raw, normalized

        parent_vol_raw, parent_vol = curve_from("parent")
        wt60_raw, wt60 = curve_from(60)
        wt300_raw, wt300 = curve_from(300)
        counts = [0] * 24
        pooled_wt = {60: [0, 0.0], 300: [0, 0.0]}  # count, sumsq
        for sess in complete:
            for hour in range(24):
                counts[hour] += sess["parent_count_by_hour"][hour]
                for h in (60, 300):
                    cell = sess[f"horizon_{h}_by_hour"][hour]
                    pooled_wt[h][0] += cell["count"]
                    pooled_wt[h][1] += cell["sumsq"]
        return {
            "seed": summary.get("seed"),
            "complete_sessions": len(complete),
            "parent_vol": parent_vol,
            "parent_vol_raw": parent_vol_raw,
            "walltime_60": wt60,
            "walltime_60_raw": wt60_raw,
            "walltime_300": wt300,
            "walltime_300_raw": wt300_raw,
            "arrival_count_by_hour": counts,
            "walltime_pooled": pooled_wt,
            "shortfalls": shortfalls,
            # The raw generated evidence rides the artifact (spec Brick H
            # per-seed records): session cells for the record, top-minute
            # locations for protocol 12.
            "session_cells": summary.get("session_cells"),
            "top_minutes": summary.get("top_minutes"),
        }

    def generated_evidence(per_seed_summaries: list[dict]) -> dict:
        seeds = [seed_curves(s) for s in per_seed_summaries]
        n_seeds = len(seeds)
        # Arrival: pooled counts and exposure across seeds, normalized ONCE
        # (spec 4.5) - the marginal curve the arrival gate compares.
        rate_raw = {}
        for hour in exposed_utc_hours():
            total = sum(s["arrival_count_by_hour"][hour] for s in seeds)
            denom = weights[hour] * GENERATED_SESSIONS_PER_SEED * n_seeds
            rate_raw[hour] = total / denom
        # Evidence curve, exposed hours only: the normalizer's conventional
        # hour-21 value belongs to INSTALLED arrays, and a curve24 evidence
        # record carries null there.
        arrival = {
            h: v for h, v in normalize_hour_curve(rate_raw).items()
            if h != 21
        }

        def central(key):
            # Per-seed normalized curves, nearest-rank median across seeds
            # per hour; the across-seed curve is NOT renormalized.
            if any(s[key] is None for s in seeds):
                return None
            return {
                hour: nearest_rank_list(
                    sorted(s[key][hour] for s in seeds), 0.5
                )
                for hour in exposed_utc_hours()
            }

        pooled_rms = {}
        for h in (60, 300):
            count = sum(s["walltime_pooled"][h][0] for s in seeds)
            sumsq = sum(s["walltime_pooled"][h][1] for s in seeds)
            # A zero-return horizon is a deliberate FAILED measurement,
            # represented as null - never NaN, which the strict writer must
            # not be handed (frozen non-finite rule).
            pooled_rms[h] = math.sqrt(sumsq / count) if count else None
        return {
            "per_seed": seeds,
            "arrival_marginal": arrival,
            "central": {"parent_vol": central("parent_vol"),
                        "walltime_60": central("walltime_60"),
                        "walltime_300": central("walltime_300")},
            "walltime_pooled_rms": pooled_rms,
            "shortfalls": [f for s in seeds for f in s["shortfalls"]],
        }

    # Gate references: the CANDIDATE vol_hour is judged MATERIALIZED (it is
    # the installed array), while the marginal target and wall-time curves
    # are observed evidence and are judged normalized-unrounded (spec 4.5,
    # blocker-3 amendment).
    obs_marginal = intensity_fit["marginal_target"]["normalized"]
    obs_vol_curve = vol_hour_fit["materialized"]
    obs_wt = {h: walltime_obs[str(h)] for h in (60, 300)}
    cadence_names = ("mean_event_duration_s", "children_mean",
                     "children_single_frac", "levels_mean")

    def judge(family: str, gen: dict, evidence, per_seed=None) -> dict:
        checks: dict = {}
        measured: dict = {}
        targets: dict = {}

        def hour_gate(name: str, curve, reference: list) -> None:
            kind, bound = TOLERANCES[name]
            per_hour = {}
            ok = curve is not None
            for hour in exposed_utc_hours():
                if curve is None:
                    per_hour[str(hour)] = None
                    continue
                good = within(kind, bound, curve[hour], reference[hour])
                per_hour[str(hour)] = good
                ok = ok and good
            checks[name] = ok
            measured[name] = None if curve is None else [
                None if h == 21 else curve[h] for h in range(24)
            ]
            targets[name] = [None if h == 21 else reference[h]
                             for h in range(24)]
            measured[f"{name}_per_hour"] = per_hour

        if family == "session_arrival":
            hour_gate("session_arrival_hour",
                      evidence["arrival_marginal"], obs_marginal)
        if family == "session_parent_vol":
            hour_gate("session_vol_hour",
                      evidence["central"]["parent_vol"], obs_vol_curve)
        if family == "session_walltime":
            for h in (60, 300):
                hour_gate(f"walltime_hour_{h}",
                          evidence["central"][f"walltime_{h}"],
                          obs_wt[h]["hourly"]["normalized"])
                kind, bound = TOLERANCES[f"walltime_pooled_{h}"]
                gen_rms = evidence["walltime_pooled_rms"][h]
                checks[f"walltime_pooled_{h}"] = (
                    gen_rms is not None
                    and within(kind, bound, gen_rms,
                               obs_wt[h]["pooled_rms"])
                )
                measured[f"walltime_pooled_{h}"] = gen_rms
                targets[f"walltime_pooled_{h}"] = obs_wt[h]["pooled_rms"]
        if family == "base_volatility":
            # The cadence four ride in this family's verdict so a cadence
            # miss is visible here; cadence separately REFUSES protocol 11.
            for name in cadence_names:
                kind, bound = TOLERANCES[name]
                checks[name] = within(kind, bound, gen[name],
                                      observed[name])
                measured[name] = gen[name]
                targets[name] = observed[name]
            checks["mid_rms"] = within(
                "relative", TOLERANCES["mid_rms"][1], gen["mid_rms"],
                observed["mid_rms"],
            )
            measured["mid_rms"] = gen["mid_rms"]
            targets["mid_rms"] = observed["mid_rms"]
            # Minute-range gates: one-sided upper against the resampled
            # observed envelope, judged PER SEED.
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
        if evidence is not None and evidence["shortfalls"]:
            # Recorded per seed/hour/count; the family fails through its
            # None curve, this is the diagnostic trail.
            measured["generated_cell_shortfalls"] = evidence["shortfalls"]
        return {"checks": checks, "measured": measured, "targets": targets}

    def family_passes(results: dict) -> bool:
        checks = results["checks"]
        return bool(checks) and all(checks.values())

    # --- family probes then the final combined run (4.5). The probe
    # override sets are exact: arrival carries the candidate intensity
    # alone; the other three carry both arrays plus the solved scalar and
    # therefore share cached FINAL walks with the combined run.
    combined_overrides = dict(session_overrides,
                              **{"generator.vol_scalar":
                                 candidate_vol_scalar})
    probe_defs = {
        "session_arrival": {"session.intensity_hour": candidate_intensity},
        "session_parent_vol": dict(combined_overrides),
        "session_walltime": dict(combined_overrides),
        "base_volatility": dict(combined_overrides),
    }
    prewarm_walks(run_summary,
                  list(probe_defs.values()) + [combined_overrides],
                  FINAL_SEEDS, FINAL_START_NS, FINAL_LENGTH)
    probe_results: dict[str, dict] = {}
    probe_errors: dict[str, str] = {}
    for family in FAMILIES:
        try:
            ss = summaries_for(
                run_summary, probe_defs[family],
                FINAL_SEEDS, FINAL_START_NS, FINAL_LENGTH, with_seeds=True,
            )
            evidence = generated_evidence(ss["per_seed"])
            probe_results[family] = judge(
                family, ss["pooled"], evidence, per_seed=ss["per_seed"]
            )
        except Refusal as exc:
            probe_results[family] = {
                "checks": {"probe_run": False}, "measured": {},
                "targets": {},
            }
            probe_errors[family] = str(exc)

    # The final combined run is attempted REGARDLESS of individual probe
    # misses, so the artifact records interactions; landing still requires
    # both stages.
    combined_results: dict[str, dict] = {}
    combined_evidence = None
    combined_error = None
    try:
        ss = summaries_for(
            run_summary, combined_overrides, FINAL_SEEDS,
            FINAL_START_NS, FINAL_LENGTH, with_seeds=True,
        )
        combined_evidence = generated_evidence(ss["per_seed"])
        for family in FAMILIES:
            combined_results[family] = judge(
                family, ss["pooled"], combined_evidence,
                per_seed=ss["per_seed"],
            )
        gen_rms = ss["pooled"]["mid_rms"]
        solves["vol_scalar"]["final_score"] = (
            abs(gen_rms - observed["mid_rms"]) / observed["mid_rms"]
            if math.isfinite(gen_rms) and observed["mid_rms"] > 0
            else None
        )
    except Refusal as exc:
        combined_error = str(exc)

    def family_ok(family: str) -> bool:
        return (family_passes(probe_results[family])
                and combined_error is None
                and family in combined_results
                and family_passes(combined_results[family]))

    session_ok = all(
        family_ok(f)
        for f in ("session_arrival", "session_parent_vol",
                  "session_walltime")
    )
    base_probe = probe_results["base_volatility"]
    base_combined = combined_results.get("base_volatility")

    def base_checks(names) -> bool:
        return (combined_error is None and base_combined is not None
                and all(base_probe["checks"].get(n) is True for n in names)
                and all(base_combined["checks"].get(n) is True
                        for n in names))

    cadence_ok = base_checks(cadence_names)
    pooled_rms_ok = base_checks(("mid_rms",))
    envelope_ok = base_checks(
        tuple(f"minute_range_{stat}" for stat in MINUTE_RANGE_GATES)
    )

    # The three-way landing rule (spec 2.3): the arrays are one atomic
    # group behind all three session families AND cadence AND the pooled
    # RMS - a pooled-RMS failure REFUSES protocol 11 outright, because
    # landing arrays with the old scalar ships a known-wrong scale and a
    # candidate that missed its primary scale target is not covered by the
    # envelope-only exception. Only the envelope may fail alone, landing
    # the scalar as declared-best-candidate.
    arrays_land = session_ok and cadence_ok and pooled_rms_ok
    vol_fitted = arrays_land and envelope_ok

    def failure_reason(family: str) -> str | None:
        if family in probe_errors:
            return probe_errors[family]
        if combined_error is not None:
            return f"the combined run failed: {combined_error}"
        if not cadence_ok:
            return ("cadence regressed under the candidate arrays; a "
                    "cadence regression refuses protocol 11 outright")
        if not pooled_rms_ok:
            return ("the pooled parent RMS missed its target; protocol 11 "
                    "refuses rather than landing arrays with a known-wrong "
                    "scale")
        if not session_ok:
            return "a session family failed; the atomic group does not land"
        return None

    verdicts: dict[str, dict] = {}
    for target, family, metrics in TARGETS:
        probe = probe_results[family]
        combined = combined_results.get(family)
        if target in ("intensity_hour", "vol_hour"):
            status = "fitted" if arrays_land else "declared-misrepresented"
            reason = None if arrays_land else failure_reason(family)
        else:  # vol_scalar
            if vol_fitted:
                status, reason = "fitted", None
            elif arrays_land:
                status = "declared-best-candidate"
                reason = ("the minute-range envelope failed; the best "
                          "candidate is carried under declared provenance "
                          "as protocol 12's motivating evidence")
            else:
                status = "declared-misrepresented"
                reason = failure_reason(family)
        verdicts[target] = {
            "family": family,
            "status": status,
            "tolerance": {
                m: list(TOLERANCES[m]) for m in metrics
            },
            "measured": {
                "probe": {m: probe["measured"].get(m) for m in metrics},
                "combined": None if combined is None else {
                    m: combined["measured"].get(m) for m in metrics
                },
            },
            "observed": {m: probe["targets"].get(m) for m in metrics},
            "checks": {
                "probe": {m: probe["checks"].get(m) for m in metrics},
                "combined": None if combined is None else {
                    m: combined["checks"].get(m) for m in metrics
                },
            },
            **({"reason": reason} if reason else {}),
        }

    # The landing set derives from the verdicts alone and names only
    # `fitted` slots - the slots whose provenance kind flips. A
    # declared-best-candidate scalar lands its VALUE under declared
    # provenance (the protocol-10 Brick L value rule) but never joins the
    # landing set; nothing lands at all unless the atomic group does.
    landing = [] if not arrays_land else sorted(
        target for target, verdict in verdicts.items()
        if verdict["status"] == "fitted"
    )
    fitted_candidates = {
        "intensity_hour": candidate_intensity,
        "vol_hour": candidate_vol_hour,
        "vol_scalar": candidate_vol_scalar,
    }

    diagnostics = dict(observed["diagnostics"])
    # Generated cell shortfalls are a DIAGNOSTIC trail (the family fails
    # through its None curve); they live here, outside the exact
    # session_refit schema.
    diagnostics["generated_cell_shortfalls"] = (
        combined_evidence["shortfalls"] if combined_evidence else None
    )
    diagnostics["sqrt_decomposition"] = {
        # Lineage and directionality ONLY (spec 4.8): retired NQ per-minute
        # proxy against the July MNQ parent-count curve; this does not
        # estimate the generated tape's aggregation exponent.
        "note": "retired per-minute vol_hour vs sqrt of the July parent "
                "marginal; lineage diagnostic, not an exponent estimate",
        "retired_vol_hour_peak_to_trough": 1.8702 / 0.5533,
        "fitted_parent_vol_curve": vol_hour_fit["materialized"],
        "fitted_curve_inverted": bool(
            max(
                vol_hour_fit["materialized"][h]
                for h in range(24) if h != 21 and 0 <= h <= 8
            ) > 1.0
        ),
    }

    # --- the frozen session_refit artifact block (spec Brick H) ---
    def as_list24(curve, zero=None):
        if curve is None:
            return None
        return [curve.get(h, zero) for h in range(24)]

    def observed_cell_records(cells_map, horizon: bool) -> list:
        zero = ({"count": 0, "sum": 0.0, "sumsq": 0.0, "sum_abs": 0.0,
                 "max_abs": 0.0} if horizon
                else {"count": 0, "sum_abs": 0.0, "max_abs": 0.0})
        return [
            {"session": label,
             "cells": [cells_map[label].get(str(h), zero)
                       for h in range(24)]}
            for label in sorted(cells_map)
        ]

    def per_seed_record(seed_rec: dict) -> dict:
        def seed_pooled(h: int):
            count, sumsq = seed_rec["walltime_pooled"][h]
            return (math.sqrt(sumsq / count) if count else None), count

        walltime = {}
        for h in (60, 300):
            rms, count = seed_pooled(h)
            # The complete walltime_block shape, per seed (frozen schema).
            walltime[str(h)] = {
                "hourly": {
                    "raw": as_list24(seed_rec[f"walltime_{h}_raw"]),
                    "normalized": as_list24(seed_rec[f"walltime_{h}"]),
                },
                "pooled_rms": rms,
                "return_count": count,
            }
        return {
            "seed": seed_rec["seed"],
            "session_cells": seed_rec["session_cells"],
            "parent_vol_curve": {
                "raw": as_list24(seed_rec["parent_vol_raw"]),
                "normalized": as_list24(seed_rec["parent_vol"]),
            },
            "walltime_curves": walltime,
            "arrival_count_by_hour": seed_rec["arrival_count_by_hour"],
            "top_minutes": seed_rec["top_minutes"],
        }

    def session_gate_records(metric: str, family: str) -> list:
        # BOTH stage records always exist (frozen schema): a stage whose run
        # never happened carries an all-null record with pass null, never a
        # bare null in the record's place.
        records = []
        for stage, results in (("probe", probe_results.get(family)),
                               ("combined", combined_results.get(family))):
            per_hour_map = (results or {"measured": {}})["measured"].get(
                f"{metric}_per_hour") or {}
            gen_curve = results["measured"].get(metric) if results else None
            obs_curve = results["targets"].get(metric) if results else None
            worst_hour = None
            worst_ratio = None
            if gen_curve and obs_curve:
                for h in exposed_utc_hours():
                    g, o = gen_curve[h], obs_curve[h]
                    if g is None or o is None or o == 0:
                        continue
                    ratio = abs(g / o - 1.0)
                    if worst_ratio is None or ratio > worst_ratio:
                        worst_ratio, worst_hour = ratio, h
            records.append({
                "family": family,
                "stage": stage,
                "per_hour": [
                    None if h == 21 else per_hour_map.get(str(h))
                    for h in range(24)
                ],
                "worst_hour": worst_hour,
                "worst_ratio": worst_ratio,
                "pass": results["checks"].get(metric) if results else None,
            })
        return records

    def metric_verdict_record(metric: str, family: str) -> dict:
        # The existing protocol-10 verdict-record schema, per metric:
        # family, STATUS, tolerance, measured/observed/checks by stage.
        # Status vocabulary per the re-signed amendment: passed iff both
        # stage checks read true, not-run when either stage never produced
        # a check, failed otherwise.
        probe = probe_results.get(family)
        combined = combined_results.get(family)
        probe_check = probe["checks"].get(metric) if probe else None
        combined_check = combined["checks"].get(metric) if combined \
            else None
        if probe_check is None or combined_check is None:
            status = "not-run"
        elif probe_check is True and combined_check is True:
            status = "passed"
        else:
            status = "failed"
        return {
            "family": family,
            "status": status,
            "tolerance": list(TOLERANCES[metric]),
            "measured": {
                "probe": probe["measured"].get(metric) if probe else None,
                "combined": combined["measured"].get(metric)
                if combined else None,
            },
            "observed": probe["targets"].get(metric) if probe else None,
            "checks": {
                "probe": probe_check,
                "combined": combined_check,
            },
        }

    raw_obs = observed["session_refit_raw"]
    session_refit = {
        # Exactly the nine section-4 constants (frozen schema);
        # GENERATED_SESSIONS_PER_SEED stays bound through SUBCONTRACT_KEYS.
        "constants": {
            k: globals()[k] for k in (
                "MIN_PARENT_CELL_RETURNS", "MIN_60S_CELL_RETURNS",
                "MIN_300S_CELL_RETURNS", "SESSION_HOUR_BAND",
                "ARRIVAL_HOUR_REL_TOL", "WALLTIME_POOLED_REL_TOL",
                "SESSION_ARRAY_DECIMALS", "TOP_MINUTE_RECORDS",
                "SESSION_VOL_CORR_MIN",
            )
        },
        "observed": {
            "session_count": len(usable),
            "parent_count_by_hour": [
                sum(row) for row in raw_obs["parent_count_by_hour_dow"]
            ],
            "parent_count_by_hour_dow":
                raw_obs["parent_count_by_hour_dow"],
            "open_minutes_by_hour_dow":
                intensity_fit["open_minutes_by_hour_dow"],
            "parent_rate_target": intensity_fit["marginal_target"],
            "parent_vol_cells": observed_cell_records(
                raw_obs["parent_vol_cells"], horizon=False
            ),
            "parent_vol_curve": vol_hour_fit,
            "horizon_60_cells": observed_cell_records(
                raw_obs["horizon_cells"]["60"], horizon=True
            ),
            "horizon_300_cells": observed_cell_records(
                raw_obs["horizon_cells"]["300"], horizon=True
            ),
            "walltime_curves": walltime_obs,
        },
        "candidate": {
            "intensity_hour": {
                k: intensity_fit[k]
                for k in ("raw", "normalized_unrounded", "materialized")
            },
            "vol_hour": vol_hour_fit,
            "dow_weight": list(MNQ_DOW_WEIGHT),
            "vol_scalar": candidate_vol_scalar,
        },
        "generated": {
            "final_seeds": list(FINAL_SEEDS),
            "per_seed": [
                per_seed_record(s)
                for s in (combined_evidence["per_seed"]
                          if combined_evidence else [])
            ],
            "central_curves": {
                "parent_vol": as_list24(
                    combined_evidence["central"]["parent_vol"]
                ) if combined_evidence else None,
                "walltime_60": as_list24(
                    combined_evidence["central"]["walltime_60"]
                ) if combined_evidence else None,
                "walltime_300": as_list24(
                    combined_evidence["central"]["walltime_300"]
                ) if combined_evidence else None,
                "arrival_marginal": as_list24(
                    combined_evidence["arrival_marginal"]
                ) if combined_evidence else None,
            },
        },
        "verdicts": {
            "session_arrival": session_gate_records(
                "session_arrival_hour", "session_arrival"),
            "session_parent_vol": session_gate_records(
                "session_vol_hour", "session_parent_vol"),
            "session_walltime_60": session_gate_records(
                "walltime_hour_60", "session_walltime"),
            "session_walltime_300": session_gate_records(
                "walltime_hour_300", "session_walltime"),
            "walltime_pooled_60": metric_verdict_record(
                "walltime_pooled_60", "session_walltime"),
            "walltime_pooled_300": metric_verdict_record(
                "walltime_pooled_300", "session_walltime"),
            "mid_rms": metric_verdict_record("mid_rms", "base_volatility"),
            "minute_range_p99": metric_verdict_record(
                "minute_range_p99", "base_volatility"),
            "minute_range_p99.9": metric_verdict_record(
                "minute_range_p99.9", "base_volatility"),
            "minute_range_max": metric_verdict_record(
                "minute_range_max", "base_volatility"),
        },
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
        "session_refit": session_refit,
        "landing_rule": {
            "session_ok": session_ok,
            "cadence_ok": cadence_ok,
            "pooled_rms_ok": pooled_rms_ok,
            "envelope_ok": envelope_ok,
            "arrays_land": arrays_land,
            **({"combined_error": combined_error}
               if combined_error else {}),
        },
        "verdicts": verdicts,
        "diagnostics": diagnostics,
        "fitted_candidates": fitted_candidates,
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
    check("band boundaries are inclusive at both edges",
          within("band", SESSION_HOUR_BAND, 0.8, 1.0)
          and within("band", SESSION_HOUR_BAND, 1.25, 1.0)
          and not within("band", SESSION_HOUR_BAND, 0.7999, 1.0)
          and not within("band", SESSION_HOUR_BAND, 1.2501, 1.0))
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

    print("the protocol-11 refit constructors")
    check("a cell more than half zeros yields the trimmed-mean scale",
          abs(cell_scale(1000, 40.0, 0.5) - 39.5 / 999) < 1e-15
          and cell_scale(1000, 40.0, 0.5) > 0)
    flat_norm = normalize_hour_curve({h: 2.5 for h in range(24)})
    check("the hour normalization centers the exposure-weighted mean on one",
          all(abs(flat_norm[h] - 1.0) < 1e-12 for h in exposed_utc_hours())
          and flat_norm[21] == 1.0)
    mat = materialize_curve(flat_norm)
    check("materialization is idempotent",
          [float(format(v, f".{SESSION_ARRAY_DECIMALS}f")) for v in mat]
          == mat)
    qual_cells = {
        s: {str(h): {"count": 1000, "sum_abs": 10.0, "max_abs": 0.1}
            for h in range(24) if h != 21}
        for s in ("2026-07-06", "2026-07-07")
    }
    ok_curve = hourly_robust_curve(qual_cells,
                                   ["2026-07-06", "2026-07-07"],
                                   MIN_PARENT_CELL_RETURNS, "t")
    check("a qualifying cell set produces the median cell scale",
          abs(ok_curve[0] - cell_scale(1000, 10.0, 0.1)) < 1e-15)
    import copy as _copy
    sub_floor = _copy.deepcopy(qual_cells)
    sub_floor["2026-07-07"]["3"]["count"] = MIN_PARENT_CELL_RETURNS - 1
    check("one sub-floor observed cell refuses by session and hour",
          refuses(lambda: hourly_robust_curve(
              sub_floor, ["2026-07-06", "2026-07-07"],
              MIN_PARENT_CELL_RETURNS, "parent-vol cell"), "hour 3"))
    missing_cell = _copy.deepcopy(qual_cells)
    del missing_cell["2026-07-07"]["4"]
    check("a missing observed cell refuses like a sub-floor one",
          refuses(lambda: hourly_robust_curve(
              missing_cell, ["2026-07-06", "2026-07-07"],
              MIN_PARENT_CELL_RETURNS, "parent-vol cell"), "hour 4"))
    check("the horizon floors are separate constants each below its "
          "hour-20 maximum",
          MIN_60S_CELL_RETURNS <= 42 and MIN_300S_CELL_RETURNS <= 6
          and refuses(lambda: hourly_robust_curve(
              {s: {str(h): {"count": MIN_60S_CELL_RETURNS - 1,
                            "sum_abs": 1.0, "max_abs": 0.1}
                   for h in range(24) if h != 21}
               for s in ("2026-07-06",)},
              ["2026-07-06"], MIN_60S_CELL_RETURNS, "60s horizon cell"),
              "60s"))

    # Endpoint-hour attribution through the REAL observe pass: two parents
    # straddling an hour boundary produce one return, and it must land in
    # the ENDPOINT hour - an off-by-one implementation lands it in hour 11.
    attribution_lines = [
        _st_row(_st_ts("2026-07-15", 11, 59, 59),
                price=23_000 * 10**9),
        _st_row(_st_ts("2026-07-15", 12, 0, 1),
                price=23_000 * 10**9 + TICK_UNITS),
    ]
    ledger_path = build_delivery(attribution_lines)
    attr_obs = observe(parse_stream(data_files(fake_dir)), ["2026-07-15"])
    attr_cells = attr_obs["session_refit_raw"]["parent_vol_cells"]
    check("a boundary-crossing return lands in its endpoint hour",
          attr_cells["2026-07-15"].get("12", {}).get("count") == 1
          and "11" not in attr_cells["2026-07-15"])

    # The frozen-DOW conditional estimator against a fixture where the
    # marginal and conditional estimates DIFFER: counts proportional to
    # exposure make the marginal exactly flat, while hours 22-23 run on a
    # Sun-Thu day mix whose mean dow_weight exceeds the Mon-Fri mix of
    # hours 0-20, so the conditional hour parameter must dip there. A
    # marginal-normalizing implementation reads flat everywhere and fails
    # this check.
    week5 = ["2026-07-06", "2026-07-07", "2026-07-08", "2026-07-09",
             "2026-07-10"]
    e_hd_fix = exposure_by_hour_dow(week5)
    cond_fit = fit_intensity_hour(
        {"session_refit_raw": {"parent_count_by_hour_dow": [
            [e_hd_fix[h][d] * 10 for d in range(7)] for h in range(24)
        ]}},
        week5,
    )
    check("counts proportional to exposure give an exactly flat marginal",
          all(abs(cond_fit["marginal_target"]["normalized"][h] - 1.0)
              < 1e-5 for h in exposed_utc_hours()))
    check("the conditional estimate dips where the day mix is Sun-heavy",
          cond_fit["materialized"][22] < cond_fit["materialized"][12] * 0.95
          and cond_fit["materialized"][23]
          < cond_fit["materialized"][12] * 0.95)
    check("the frozen dow_weight rides the artifact record unchanged",
          cond_fit["dow_weight"] == list(MNQ_DOW_WEIGHT))

    check("list overrides serialize as TOML float arrays",
          '"session.vol_hour" = [1.0, 0.5]'
          in scratch_config_text({"session.vol_hour": [1.0, 0.5]}))

    print("the fit driver against a fake generator")

    # A DENSE month: every usable session covers every exposed UTC hour
    # with 1002 one-second parents, so all three observed cell floors
    # qualify (hour 20 starts at its post-halt 20:30). Hours 22-23 sit on
    # the prior civil date - the session opens there.
    def dense_month_lines():
        lines = []
        for label, status in SESSION_INVENTORY:
            if status != "full":
                continue
            prior = (dt.date.fromisoformat(label)
                     - dt.timedelta(days=1)).isoformat()
            for h in exposed_utc_hours():
                date = prior if h >= 22 else label
                # Rows spread across the hour's WHOLE exposure, not its
                # first minutes: a front-loaded hour leaves trailing
                # boundaries at frozen as-of mids, and hour 20's different
                # zero-return density would warp the observed wall-time
                # curve. Prices are TIMESTAMP-HASHED, not periodic: with a
                # periodic price and fixed spacing, the fixed-horizon
                # boundaries sample a deterministic phase lattice and hour
                # 20's different spacing lands a wildly different scale.
                if h == 20:
                    blocks = [(_st_ts(date, 20, 0), 302, 2_980_000_000),
                              (_st_ts(date, 20, 30), 704, 2_556_000_000)]
                else:
                    blocks = [(_st_ts(date, h, 0), 1002, 3_590_000_000)]
                for base, count, step in blocks:
                    for i in range(count):
                        ts = base + i * step
                        # splitmix64-style finalizer: a plain multiplicative
                        # hash mod 7 stays lattice-correlated on arithmetic
                        # second progressions, and different row spacings
                        # then read different return scales.
                        v = (ts // 10**9) & 0xFFFFFFFFFFFFFFFF
                        v = (v ^ (v >> 30)) * 0xBF58476D1CE4E5B9 \
                            & 0xFFFFFFFFFFFFFFFF
                        v = (v ^ (v >> 27)) * 0x94D049BB133111EB \
                            & 0xFFFFFFFFFFFFFFFF
                        draw = (v ^ (v >> 31)) % 7
                        price = 23_000 * 10**9 + draw * TICK_UNITS
                        lines.append(_st_row(ts, price=price))
        lines.sort(key=lambda l: int(l.split(",")[1]))
        return lines

    dense = dense_month_lines()
    ledger_path = build_delivery(dense)
    payload = run_preflight(fake_dir, ledger_path)
    st_preflight = os.path.join(SELFTEST_DIR, "driver-preflight.json")
    write_json_atomic(st_preflight, payload)
    dense_usable = payload["usable_sessions"]
    dense_observed = observe(parse_stream(data_files(fake_dir)),
                             dense_usable)
    dense_vh = fit_vol_hour(dense_observed, dense_usable)
    dense_wt = observed_walltime_curves(dense_observed, dense_usable)
    check("the dense month qualifies every observed cell floor",
          len(dense_vh["materialized"]) == 24
          and set(dense_wt) == {"60", "300"})

    # The honest fake generator: it applies the runtime model - candidate
    # intensity TIMES the frozen day mix for arrivals, candidate vol_hour
    # as the per-parent scale - so the clean roundtrip PROVES the
    # conditional estimator inverts exactly what the runtime applies.
    week_table = weekly_exposure_table()
    dow_mix = {
        h: (sum(week_table[h][d] * MNQ_DOW_WEIGHT[d] for d in range(7))
            / sum(week_table[h])) if sum(week_table[h]) else 0.0
        for h in range(24)
    }
    hour_weights = hour_exposure_weights()
    target_mid_rms = dense_observed["mid_rms"]

    def p11_fake_factory(state, distort=None):
        distort = distort or {}

        def fake(overrides, seed, start_ns, length, warmup):
            state["calls"] += 1
            state["override_sets"].append(dict(overrides))
            vol = float(overrides.get("generator.vol_scalar", 1e-6))
            vh = list(overrides.get("session.vol_hour", [1.0] * 24))
            ih = list(overrides.get("session.intensity_hour", [1.0] * 24))
            if "vol_curve_warp" in distort:
                vh[2] *= distort["vol_curve_warp"]
            if "arrival_warp" in distort:
                ih[2] *= distort["arrival_warp"]
            cadence_warp = distort.get("cadence_warp", 1.0)
            parents = 1000
            singles = int(dense_observed["children_single_frac"] * parents)
            mid_rms_gen = (target_mid_rms * 3.0 if distort.get("dead_vol")
                           else vol * 10.0)
            minute_key = max(1, int(vol * 1.2e6))
            if distort.get("inflate_minutes"):
                minute_key *= 10
            # Session cells: the per-parent scale is vol * vh[h]; the
            # wall-time scale tracks the pooled observed reading at the
            # solved scalar; arrivals follow intensity times day mix.
            wt_scale = {
                60: dense_wt["60"]["pooled_rms"] * vol
                / (target_mid_rms / 10.0),
                300: dense_wt["300"]["pooled_rms"] * vol
                / (target_mid_rms / 10.0),
            }
            n_sessions = GENERATED_SESSIONS_PER_SEED \
                - (1 if distort.get("drop_session") else 0)
            cells = []
            for k in range(n_sessions):
                def abs_cell(scale, count):
                    return {"count": count,
                            "sum_abs": scale * (count - 1) + scale,
                            "max_abs": scale}

                def hz_cell(scale, count):
                    return {"count": count, "sum": 0.0,
                            "sumsq": scale * scale * count,
                            "sum_abs": scale * (count - 1) + scale,
                            "max_abs": scale}

                record = {
                    "session_start_ns": k * 10**9,
                    "session_end_ns": k * 10**9 + 1,
                    "complete": True,
                    "parent_count_by_hour": [
                        0 if h == 21 else max(
                            MIN_PARENT_CELL_RETURNS + 1,
                            int(6000 * ih[h] * dow_mix[h]
                                * hour_weights[h] / 60.0),
                        )
                        for h in range(24)
                    ],
                    "mid_abs_by_hour": [
                        abs_cell(0.0, 0) if h == 21
                        else abs_cell(vol * vh[h],
                                      MIN_PARENT_CELL_RETURNS + 200)
                        for h in range(24)
                    ],
                    "horizon_60_by_hour": [
                        hz_cell(0.0, 0) if h == 21
                        else hz_cell(wt_scale[60] * vh[h],
                                     42 if h == 20 else 59)
                        for h in range(24)
                    ],
                    "horizon_300_by_hour": [
                        hz_cell(0.0, 0) if h == 21
                        else hz_cell(wt_scale[300] * vh[h],
                                     6 if h == 20 else 11)
                        for h in range(24)
                    ],
                }
                cells.append(record)
            return {
                "seed": seed, "parents": parents,
                "sided_rows": int(dense_observed["children_mean"] * parents),
                "single_parents": singles,
                "level_count_sum": int(dense_observed["levels_mean"]
                                       * parents),
                "gap_sum_ns": int(dense_observed["mean_event_duration_s"]
                                  * cadence_warp * 1e9 * parents),
                "eligible_gaps": parents,
                "size_histogram": {"1": parents},
                "bid_size_histogram": {"3": parents},
                "ask_size_histogram": {"4": parents},
                "width_ticks_histogram": {"2": parents},
                "buyer_displacement_hist": {"0.50": parents},
                "seller_displacement_hist": {"0.50": parents},
                "mid_return_count": parents,
                "mid_return_sum": 0.0,
                "mid_return_sumsq": mid_rms_gen * mid_rms_gen * parents,
                "minute_range_ticks_hist": {str(minute_key): 500},
                "minute_range_max_ticks": minute_key,
                "minute_range_second_max_ticks": minute_key,
                "horizon_vol": {},
                "session_cells": cells,
                "top_minutes": [],
                "first_book_mid": "23000",
                "measured_from_ns": start_ns,
                "measured_until_ns": start_ns,
            }
        return fake

    def driver(distort=None):
        state = {"calls": 0, "override_sets": []}
        artifact = run_fit(directory=fake_dir,
                           run_summary=p11_fake_factory(state, distort),
                           harness_commit="selftest",
                           ledger_path=ledger_path,
                           preflight_artifact_path=st_preflight)
        return artifact, state

    artifact, state = driver()
    check("the fake-generator fit produces a bound artifact",
          artifact["binding"]["harness_tree_commit"] == "selftest"
          and artifact["binding"]["subcontract_hash"] == subcontract_hash())
    check("the clean roundtrip lands the atomic group and the scalar",
          artifact["landing_set"]
          == ["intensity_hour", "vol_hour", "vol_scalar"]
          and all(artifact["verdicts"][t]["status"] == "fitted"
                  for t in ("intensity_hour", "vol_hour", "vol_scalar")))
    check("the conditional roundtrip closes: the honest fake's marginal "
          "matches the observed marginal through the day mix",
          artifact["session_refit"]["verdicts"]["session_arrival"][0]
          ["pass"] is True
          and artifact["session_refit"]["verdicts"]["session_arrival"][0]
          ["stage"] == "probe")
    check("the vol solve converges on the pooled parent RMS",
          abs(artifact["solves"]["vol_scalar"]["best_candidate"] * 10.0
              - target_mid_rms) / target_mid_rms <= 0.001 + SLACK)
    sr = artifact["session_refit"]
    check("session_refit carries the frozen schema top to bottom",
          set(sr) == {"constants", "observed", "candidate", "generated",
                      "verdicts"}
          and set(sr["constants"]) == {
              "MIN_PARENT_CELL_RETURNS", "MIN_60S_CELL_RETURNS",
              "MIN_300S_CELL_RETURNS", "SESSION_HOUR_BAND",
              "ARRIVAL_HOUR_REL_TOL", "WALLTIME_POOLED_REL_TOL",
              "SESSION_ARRAY_DECIMALS", "TOP_MINUTE_RECORDS",
              "SESSION_VOL_CORR_MIN"}
          and artifact["landing_rule"]["arrays_land"] is True
          and sr["candidate"]["dow_weight"] == list(MNQ_DOW_WEIGHT)
          and len(sr["observed"]["parent_vol_cells"]) == 22
          and len(sr["observed"]["parent_vol_cells"][0]["cells"]) == 24
          and len(sr["observed"]["horizon_60_cells"]) == 22
          and len(sr["generated"]["per_seed"]) == len(FINAL_SEEDS)
          and set(sr["generated"]["per_seed"][0]) == {
              "seed", "session_cells", "parent_vol_curve",
              "walltime_curves", "arrival_count_by_hour", "top_minutes"}
          and set(sr["generated"]["per_seed"][0]["walltime_curves"]["60"])
          == {"hourly", "pooled_rms", "return_count"}
          and sr["generated"]["per_seed"][0]["parent_vol_curve"]["raw"][21]
          is None
          and len(sr["generated"]["per_seed"][0]["session_cells"])
          == GENERATED_SESSIONS_PER_SEED
          and sr["generated"]["central_curves"]["arrival_marginal"][21]
          is None
          and sr["observed"]["parent_rate_target"].keys()
          == {"raw", "normalized"}
          and sr["observed"]["walltime_curves"]["60"]["hourly"].keys()
          == {"raw", "normalized"}
          and set(sr["verdicts"]) == {
              "session_arrival", "session_parent_vol",
              "session_walltime_60", "session_walltime_300",
              "walltime_pooled_60", "walltime_pooled_300", "mid_rms",
              "minute_range_p99", "minute_range_p99.9",
              "minute_range_max"}
          and [r["stage"] for r in sr["verdicts"]["session_arrival"]]
          == ["probe", "combined"]
          and all(
              r["pass"] is True
              and all(r["per_hour"][h] is not None
                      for h in exposed_utc_hours())
              and r["per_hour"][21] is None
              for r in sr["verdicts"]["session_arrival"]
          )
          and set(sr["verdicts"]["mid_rms"]) == {
              "family", "status", "tolerance", "measured", "observed",
              "checks"}
          and sr["verdicts"]["mid_rms"]["status"] == "passed"
          and sr["verdicts"]["minute_range_max"]["status"] == "passed"
          and set(sr["verdicts"]["walltime_pooled_60"]["measured"])
          == {"probe", "combined"})
    check("every verdict carries family, both stages and tolerance",
          all(
              {"family", "status", "tolerance", "measured", "observed",
               "checks"} <= set(artifact["verdicts"][t])
              and artifact["verdicts"][t]["family"] == fam
              for t, fam, _metrics in TARGETS
          ))
    arrival_sets = [
        s for s in state["override_sets"]
        if "session.intensity_hour" in s and "session.vol_hour" not in s
    ]
    check("family isolation: the arrival probe carries the candidate "
          "intensity ALONE",
          arrival_sets
          and all("generator.vol_scalar" not in s for s in arrival_sets))

    print("the atomic landing group, all three base_volatility branches")
    artifact, _state = driver({"inflate_minutes": True})
    check("an envelope-only failure lands the arrays and declares the "
          "scalar best-candidate",
          artifact["landing_set"] == ["intensity_hour", "vol_hour"]
          and artifact["verdicts"]["vol_scalar"]["status"]
          == "declared-best-candidate"
          and artifact["landing_rule"]["envelope_ok"] is False
          and artifact["landing_rule"]["arrays_land"] is True
          and artifact["session_refit"]["verdicts"]["minute_range_max"]
          ["status"] == "failed")
    artifact, _state = driver({"dead_vol": True})
    check("a pooled-RMS failure refuses protocol 11 outright, envelope "
          "notwithstanding",
          artifact["landing_set"] == []
          and artifact["verdicts"]["vol_hour"]["status"]
          == "declared-misrepresented"
          and "pooled parent RMS"
          in artifact["verdicts"]["vol_hour"]["reason"])
    artifact, _state = driver({"vol_curve_warp": 2.0})
    check("a warped generated vol curve fails its session family and "
          "nothing lands",
          artifact["landing_set"] == []
          and artifact["session_refit"]["verdicts"]["session_parent_vol"]
          [0]["pass"] is False
          and artifact["session_refit"]["verdicts"]["session_parent_vol"]
          [0]["worst_hour"] == 2
          and "session family"
          in artifact["verdicts"]["vol_hour"]["reason"])
    artifact, _state = driver({"arrival_warp": 1.2})
    check("a warped arrival curve fails session_arrival at its hour",
          artifact["landing_set"] == []
          and artifact["session_refit"]["verdicts"]["session_arrival"][0]
          ["per_hour"][2] is False
          and artifact["session_refit"]["verdicts"]["session_arrival"][0]
          ["worst_hour"] == 2)
    artifact, _state = driver({"cadence_warp": 1.5})
    check("a cadence regression refuses protocol 11 outright",
          artifact["landing_set"] == []
          and "cadence" in artifact["verdicts"]["vol_hour"]["reason"])
    artifact, _state = driver({"drop_session": True})
    check("a wrong generated session count fails every probe and the "
          "combined run, landing nothing",
          artifact["landing_set"] == []
          and artifact["landing_rule"].get("combined_error") is not None
          and "complete generated sessions"
          in artifact["landing_rule"]["combined_error"])
    check("a stage that never ran carries the all-null record, never a "
          "fabricated bool",
          all(r["pass"] is None
              and all(v is None for v in r["per_hour"])
              for r in artifact["session_refit"]["verdicts"]
              ["session_arrival"])
          and artifact["session_refit"]["verdicts"]["mid_rms"]["status"]
          == "not-run")

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
