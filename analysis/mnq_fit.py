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
import subprocess
import sys

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
}

# Diagnostics ACF lags (4.8) - findings, never gates.
ACF_LAGS = (1, 10, 50)

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
    "MAX_WRONG_SIDE_SHARE", "TOLERANCES", "ACF_LAGS", "REFERENCE_SHAPE",
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


# ---------------------------------------------------------------------------
# Streaming input (4.1 stream contract)
# ---------------------------------------------------------------------------

REQUIRED_COLUMNS = (
    "ts_event", "instrument_id", "action", "side", "price", "size",
    "bid_px_00", "ask_px_00", "bid_sz_00", "ask_sz_00",
)


def iter_csv_zst(path):
    """Yield text lines from a .csv.zst, streaming, header included."""
    with open(path, "rb") as fh:
        reader = zstd.ZstdFile(fh)
        buffer = b""
        while True:
            chunk = reader.read(1 << 20)
            if not chunk:
                break
            buffer += chunk
            while True:
                nl = buffer.find(b"\n")
                if nl < 0:
                    break
                yield buffer[:nl].decode("utf-8")
                buffer = buffer[nl + 1:]
        if buffer:
            yield buffer.decode("utf-8")


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
    AT THE SEAM (the last data row of one file versus the first of the next;
    identical adjacent rows WITHIN a file are legitimate market data),
    per-price grid membership, strict B/A/N sides, action T on every row."""
    prev_ts = None
    seam_line = None  # last data row of the previous file, seam check only
    for path in paths:
        lines = iter_csv_zst(path)
        try:
            header = next(lines)
        except StopIteration:
            raise Refusal(f"{path} is empty") from None
        idx = column_indices(header)
        last_line = None
        first_data_row = True
        for line_no, line in enumerate(lines, start=2):
            if not line.strip():
                continue
            if first_data_row:
                if seam_line is not None and line == seam_line:
                    raise Refusal(
                        f"{path}:{line_no}: duplicates the previous file's "
                        "final row at the boundary; the files overlap"
                    )
                first_data_row = False
            parts = line.split(",")
            raw_ts = parts[idx["ts_event"]]
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
            action = parts[idx["action"]]
            if action != "T":
                raise Refusal(
                    f"{path}:{line_no}: action {action!r} is not T; the "
                    "tbbo schema carries one trade per row"
                )
            side = parts[idx["side"]]
            if side not in ("B", "A", "N"):
                raise Refusal(
                    f"{path}:{line_no}: side {side!r} outside the DBN "
                    "alphabet B/A/N"
                )
            price = int(parts[idx["price"]])
            bid_px = int(parts[idx["bid_px_00"]])
            ask_px = int(parts[idx["ask_px_00"]])
            for label, value in (("price", price), ("bid_px_00", bid_px),
                                 ("ask_px_00", ask_px)):
                if value > 0 and value % TICK_UNITS != 0:
                    raise Refusal(
                        f"{path}:{line_no}: {label} {value} is off the "
                        "0.25 grid"
                    )
            yield Row(
                ts, parts[idx["instrument_id"]], side, price,
                int(parts[idx["size"]]), bid_px, ask_px,
                int(parts[idx["bid_sz_00"]]), int(parts[idx["ask_sz_00"]]),
                classify_book(bid_px, ask_px),
            )
            prev_ts = ts
            last_line = line
        seam_line = last_line


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
        session, _segment = assign_session(row.ts)
        if session is None or session not in INVENTORY_STATUS:
            outside_sessions += 1
        else:
            state = per_session.setdefault(
                session, {"rows": 0, "ids": set(), "invalid_books": 0}
            )
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


def write_json_atomic(path: str, payload: dict) -> None:
    os.makedirs(os.path.dirname(path), exist_ok=True)
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(payload, fh, indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, path)


def mode_preflight() -> None:
    payload = run_preflight(DELIVERY_DIR)
    write_json_atomic(PREFLIGHT_ARTIFACT, payload)
    print(json.dumps(
        {k: v for k, v in payload.items() if k != "sessions"},
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
    session or segment boundary."""

    def __init__(self, lags):
        self.lags = tuple(lags)
        self.window: list[float] = []
        self.n = 0
        self.s = 0.0
        self.ss = 0.0
        self.cross = {lag: 0.0 for lag in self.lags}
        self.cross_n = {lag: 0 for lag in self.lags}

    def add(self, x: float) -> None:
        self.n += 1
        self.s += x
        self.ss += x * x
        for lag in self.lags:
            if len(self.window) >= lag:
                self.cross[lag] += x * self.window[-lag]
                self.cross_n[lag] += 1
        self.window.append(x)
        if len(self.window) > max(self.lags):
            self.window.pop(0)

    def reset_series(self) -> None:
        self.window.clear()

    def value(self, lag: int) -> float:
        if self.n < 2 or self.cross_n[lag] == 0:
            return float("nan")
        mean = self.s / self.n
        var = self.ss / self.n - mean * mean
        if var <= 0:
            return float("nan")
        cov = self.cross[lag] / self.cross_n[lag] - mean * mean
        return cov / var


def hist_median(hist: dict[int, int], bin_width: float) -> float:
    """Median of a binned histogram, read at the bin center."""
    total = sum(hist.values())
    if total == 0:
        return float("nan")
    rank = max(1, math.ceil(0.5 * total))
    seen = 0
    for k in sorted(hist):
        seen += hist[k]
        if seen >= rank:
            return (k + 0.5) * bin_width
    raise AssertionError("rank walked past the histogram")


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
    per_session_parents: dict[str, int] = {}
    last_trade_price_units = None  # last valid trade in usable sessions

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
        per_session_parents[parent["session"]] = (
            per_session_parents.get(parent["session"], 0) + 1
        )
        sided_rows += parent["rows"]
        if parent["rows"] == 1:
            single_parents += 1
        level_sum += len(parent["levels"])

        here = (parent["session"], parent["segment"])
        # Chain 1: cadence, every parent.
        if prev_cadence is not None and prev_cadence[1:] == here:
            gap_ns = parent["first_ts"] - prev_cadence[0]
            gap_sum_ns += gap_ns
            gaps += 1
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
            if signed < 0:
                wrong_side += 1
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
        session, segment = assign_session(row.ts)
        if session not in usable_set:
            if current is not None:
                close_parent(current)
                current = None
            continue
        sizes.add(row.size)
        last_trade_price_units = row.price
        # Session curves bucket by EXCHANGE-LOCAL hour (the wave-1 and
        # session-fit convention), never UTC.
        _date, local_minute = local_fields(row.ts)
        hour = local_minute // 60
        hour_count[hour] += 1
        hour_volume[hour] += row.size
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

    if parents == 0:
        raise Refusal("no parents in usable sessions")
    # The grouping conformance gate lives in the selftest: the streaming pass
    # is compared against group_parents_batch, a genuinely independent
    # index-based implementation, over adversarial fixtures. A same-pass
    # counter would repeat this pass's own transition logic and prove
    # nothing.

    all_disp: dict[int, int] = {}
    for h in disp_hist.values():
        for k, v in h.items():
            all_disp[k] = all_disp.get(k, 0) + v
    max_width_count = max(width_hist.values())
    width_mode = min(k for k in width_hist if width_hist[k] == max_width_count)
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
        "size_histogram": {str(k): v for k, v in sorted(sizes.counts.items())},
        "size_mean": sizes.mean(),
        "size_quantiles": {
            f"p{int(q * 100)}": sizes.nearest_rank(q)
            for q in (0.50, 0.75, 0.90, 0.95, 0.99)
        },
        "size_floor_mass": sizes.counts.get(1, 0) / sizes.total,
        "width_hist": {str(k): v for k, v in sorted(width_hist.items())},
        "width_mode": width_mode,
        "width_modal_mass": width_hist[width_mode] / sum(width_hist.values()),
        "top_bid_median": bid_sizes.nearest_rank(0.5),
        "top_ask_median": ask_sizes.nearest_rank(0.5),
        "displacement_hist": {
            side: {str(k): v for k, v in sorted(h.items())}
            for side, h in disp_hist.items()
        },
        "displacement_median_ticks": hist_median(
            all_disp, DISPLACEMENT_BIN_TICKS
        ),
        "displacement_buyer_median_ticks": hist_median(
            disp_hist["B"], DISPLACEMENT_BIN_TICKS
        ),
        "displacement_seller_median_ticks": hist_median(
            disp_hist["A"], DISPLACEMENT_BIN_TICKS
        ),
        "wrong_side_share": wrong_side / valid_quote_parents
        if valid_quote_parents else float("nan"),
        "valid_quote_parents": valid_quote_parents,
        "mid_rms": math.sqrt(mid_sumsq / mid_count)
        if mid_count else float("nan"),
        "mid_return_count": mid_count,
        "eligible_gaps": gaps,
        "last_price_points": last_price_points,
        "per_session_parents": per_session_parents,
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
    runner; selftests inject fakes instead."""
    os.makedirs(SCRATCH_DIR, exist_ok=True)
    config_path = os.path.join(SCRATCH_DIR, f"candidate-{os.getpid()}.toml")
    out_path = os.path.join(SCRATCH_DIR, f"summary-{os.getpid()}-{seed}.json")
    with open(config_path, "w") as fh:
        fh.write(scratch_config_text(overrides))
    cmd = [
        "brokkr", "run", "--release", "mogwai", "--", "gen",
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
        return json.load(fh)


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
                key = round(float(k) / DISPLACEMENT_BIN_TICKS)
                dst[key] = dst.get(key, 0) + v
    width: dict[int, int] = {}
    for s in summaries:
        for k, v in s["width_ticks_histogram"].items():
            width[int(k)] = width.get(int(k), 0) + v
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
            absolute_step: float | None = None):
    """4.75 refinement, as amended after the selftest caught the original
    incumbent-driven survivor rule misconverging (an endpoint incumbent can
    control bracket selection without directional information): CLASSIC
    TERNARY COMPARISON. Each iteration evaluates m1 and m2 at 1/3 and 2/3;
    the survivor is [a, m2] when f(m1) <= f(m2) - the tie keeps the left -
    else [m1, b]. The coarse grid selects the basin; ternary refinement
    makes the explicit local-unimodality assumption within it. The returned
    candidate is the best point EVER evaluated, smaller winning ties."""
    def xform(x):
        return math.log(x) if log_domain else x

    def unxform(x):
        return math.exp(x) if log_domain else x

    a, b = xform(lo), xform(hi)
    best_x = best_score = None
    evaluations = 0

    def consider(x):
        nonlocal best_x, best_score, evaluations
        evaluations += 1
        score = evaluate(unxform(x))
        if best_score is None or score < best_score or (
            score == best_score and x < best_x
        ):
            best_x, best_score = x, score
        return score

    consider(a)
    consider(b)
    while True:
        span = b - a
        if absolute_step is not None:
            if span <= absolute_step:
                termination = f"absolute step <= {absolute_step}"
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


def solve_scalar(evaluate, lo: float, hi: float, points: int,
                 log_domain: bool, absolute_step: float | None = None):
    """Coarse grid then trisection of the winner's neighbor bracket; a
    boundary winner takes its single inside neighbor interval. Returns the
    solve record the artifact schema requires."""
    if log_domain:
        step = (math.log(hi) - math.log(lo)) / (points - 1)
        grid = [math.exp(math.log(lo) + i * step) for i in range(points)]
    else:
        step = (hi - lo) / (points - 1)
        grid = [lo + i * step for i in range(points)]
    scores = [evaluate(x) for x in grid]
    best_i = min(range(len(grid)), key=lambda i: (scores[i], grid[i]))
    left = grid[max(0, best_i - 1)]
    right = grid[min(len(grid) - 1, best_i + 1)]
    tie_break = "smaller candidate on equal scores"
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
        evaluate, left, right, log_domain, absolute_step
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
                  length: str) -> dict:
    return pooled([
        run_summary(overrides, seed, start_ns, length, SUMMARY_WARMUP)
        for seed in seeds
    ])


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
    "size": ("latent_size_median",),  # size_round_frac joins on the branch
    "quote": ("quoted_width", "top_sizes"),
    "displacement": ("trade_displacement_ticks",),
    "volatility": ("vol_scalar",),
    "start_price": ("start_price",),
}


def run_fit(directory: str = DELIVERY_DIR,
            run_summary=run_summary_subprocess,
            harness_commit: str | None = None,
            ledger_path: str | None = None,
            preflight_artifact_path: str = PREFLIGHT_ARTIFACT) -> dict:
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

    # --- size family: model A always; model B only when identifiability is
    # reachable (the jointly accepted cost guard) ---
    def size_eval_factory(frac):
        def evaluate(median):
            overrides = {"generator.latent_size_median": f"{median:.6f}"}
            if frac is not None:
                overrides["generator.size_round_frac"] = frac
            gen = summaries_for(run_summary, overrides, SEARCH_SEEDS,
                                SEARCH_START_NS, SEARCH_LENGTH)
            return size_objective(gen["size_histogram"], observed)
        return evaluate

    model_a = solve_scalar(size_eval_factory(None), *SIZE_MEDIAN_DOMAIN,
                           SIZE_MEDIAN_GRID_POINTS, log_domain=True)
    a_median = model_a["best_candidate"]
    run_model_b = (
        a_median >= SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
        or observed["size_quantiles"]["p50"]
        >= SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
    )
    model_b = None
    if run_model_b:
        b_results = []
        for frac in SIZE_ROUND_FRAC_GRID:
            record = solve_scalar(
                size_eval_factory(frac), *SIZE_MEDIAN_DOMAIN,
                SIZE_MEDIAN_GRID_POINTS, log_domain=True,
            )
            b_results.append((tuple(record["search_score"]), frac, record))
        b_results.sort(key=lambda t: (t[0], t[1], t[2]["best_candidate"]))
        b_score, b_frac, model_b = b_results[0]
        model_b = dict(model_b, frac=b_frac)
    if model_b is not None and tuple(model_b["search_score"]) < tuple(
        model_a["search_score"]
    ):
        chosen_median = model_b["best_candidate"]
        chosen_frac = model_b["frac"]
        chosen_model = "B"
    else:
        chosen_median, chosen_frac, chosen_model = a_median, None, "A"
    identifiable = chosen_median >= SIZE_MEDIAN_IDENTIFIABILITY_FLOOR
    fitted["latent_size_median"] = chosen_median
    if identifiable and chosen_model == "B":
        fitted["size_round_frac"] = chosen_frac
    solves["latent_size_median"] = {
        "model_a": model_a, "model_b": model_b, "chosen_model": chosen_model,
    }
    solves["size_round_frac"] = {
        "identifiable": identifiable,
        "branch": "joint" if identifiable and chosen_model == "B"
        else "declared",
        **({} if run_model_b else {
            "status": "skipped-structurally-moot",
            "model_a_median": a_median,
            "observed_size_p50": observed["size_quantiles"]["p50"],
            "identifiability_floor": SIZE_MEDIAN_IDENTIFIABILITY_FLOOR,
        }),
    }

    # --- displacement: inverse solve with the fitted width installed ---
    disp_target = observed["displacement_median_ticks"]

    def disp_eval(scalar):
        gen = summaries_for(
            run_summary,
            {"generator.trade_displacement_ticks.ticks": scalar,
             "generator.quoted_width.ticks": fitted["quoted_width"]},
            SEARCH_SEEDS, SEARCH_START_NS, SEARCH_LENGTH,
        )
        return abs(combined_displacement(gen) - disp_target)

    disp_solve = solve_scalar(
        disp_eval, 0.0, 2.0 * fitted["quoted_width"],
        DISPLACEMENT_GRID_POINTS, log_domain=False,
        absolute_step=SOLVE_ABSOLUTE_STEP_TICKS,
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

    vol_solve = solve_scalar(vol_eval, *VOL_SCALAR_DOMAIN, VOL_GRID_POINTS,
                             log_domain=True)
    fitted["vol_scalar"] = vol_solve["best_candidate"]
    solves["vol_scalar"] = dict(vol_solve, target=observed["mid_rms"])

    # --- family probes then the final combined run (4.9) ---
    def family_overrides(family: str) -> dict:
        if family == "cadence":
            return dict(cadence_overrides)
        if family == "size":
            over = {"generator.latent_size_median": f"{chosen_median:.6f}"}
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

    def judge(gen: dict, family: str) -> dict:
        results = {}
        if family in ("cadence", "volatility"):
            for name in FAMILY_SLOTS["cadence"]:
                kind, bound = TOLERANCES[name]
                results[name] = within(kind, bound, gen[name], observed[name])
        if family == "size":
            score = size_objective(gen["size_histogram"], observed)
            results["size_ecdf_distance"] = (
                score[0] <= TOLERANCES["size_ecdf_distance"][1]
            )
            hist = gen["size_histogram"]
            total = sum(hist.values())
            gen_mean = sum(k * v for k, v in hist.items()) / total
            results["size_mean"] = within(
                "relative", TOLERANCES["size_mean"][1], gen_mean,
                observed["size_mean"],
            )
            for name, p in (("size_p90", 0.90), ("size_p99", 0.99)):
                results[name] = within(
                    "size_tail", TOLERANCES[name][1],
                    nearest_rank_of(hist, p),
                    observed["size_quantiles"][f"p{int(p * 100)}"],
                )
        if family == "displacement":
            results["displacement_median"] = within(
                "absolute", TOLERANCES["displacement_median"][1],
                combined_displacement(gen), disp_target,
            )
            for side in ("B", "A"):
                results[f"displacement_side_{side}"] = within(
                    "absolute", TOLERANCES["displacement_side_median"][1],
                    hist_median(gen["displacement_hist"][side],
                                DISPLACEMENT_BIN_TICKS),
                    disp_target,
                )
        if family == "volatility":
            results["mid_rms"] = within(
                "relative", TOLERANCES["mid_rms"][1], gen["mid_rms"],
                observed["mid_rms"],
            )
        if family == "quote":
            wh = gen["width_histogram"]
            gen_mode = min(
                (k for k in wh if wh[k] == max(wh.values())), default=None
            ) if wh else None
            results["width"] = gen_mode == fitted["quoted_width"]
            results["top_bid"] = (
                bool(gen["bid_size_histogram"])
                and nearest_rank_of(gen["bid_size_histogram"], 0.5)
                == fitted["top_sizes"]["bid"]
            )
            results["top_ask"] = (
                bool(gen["ask_size_histogram"])
                and nearest_rank_of(gen["ask_size_histogram"], 0.5)
                == fitted["top_sizes"]["ask"]
            )
        if family == "start_price":
            # The gate is exact scratch-profile resolution: the walk with the
            # configured value must run at all (a bad value refuses at
            # profile construction). First-book displacement is a reported
            # diagnostic, never a gate (4.9).
            results["scratch_config_accepted"] = True
            results["first_book_mid_diagnostic"] = gen.get("first_book_mid")
        return results

    def family_passes(results: dict) -> bool:
        return all(
            v for k, v in results.items()
            if not k.endswith("_diagnostic")
        )

    probe_results: dict[str, dict] = {}
    probe_errors: dict[str, str] = {}
    probe_gens: dict[str, dict] = {}
    for family in FAMILIES:
        try:
            gen = summaries_for(run_summary, family_overrides(family),
                                FINAL_SEEDS, FINAL_START_NS, FINAL_LENGTH)
            probe_gens[family] = gen
            probe_results[family] = judge(gen, family)
        except Refusal as exc:
            probe_results[family] = {"probe_run": False}
            probe_errors[family] = str(exc)

    # Final-budget objective scores for the solved parameters, attached to
    # their solve records per the frozen artifact schema: the same objective
    # each search minimized, re-read from the family probe's pooled month.
    if "size" in probe_gens:
        solves["latent_size_median"]["final_score"] = list(
            size_objective(probe_gens["size"]["size_histogram"], observed)
        )
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
    cadence_pass = family_passes(probe_results["cadence"])

    passing = set()
    for family in FAMILIES:
        ok = family_passes(probe_results[family])
        if family == "displacement" and wrong_side_blocked:
            ok = False
        if family == "volatility" and not cadence_pass:
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
            gen = summaries_for(run_summary, combined_overrides, FINAL_SEEDS,
                                FINAL_START_NS, FINAL_LENGTH)
            for family in passing:
                combined_results[family] = judge(gen, family)
        except Refusal as exc:
            # A failed combined run fits NOTHING: no target may take fitted
            # provenance from a configuration that never produced its final
            # verdict. The artifact still lands, carrying the failure.
            combined_results = {}
            combined_error = str(exc)

    verdicts: dict[str, dict] = {}
    landing: list[str] = []
    for family in FAMILIES:
        probe_ok = family in passing
        combined_ok = family in combined_results and family_passes(
            combined_results[family]
        )
        status = "fitted" if probe_ok and combined_ok else (
            "declared-misrepresented"
        )
        reason = probe_errors.get(family)
        if family == "displacement" and wrong_side_blocked:
            reason = (
                f"wrong-side share {observed['wrong_side_share']:.4f} "
                f"exceeds {MAX_WRONG_SIDE_SHARE}; the generator "
                "structurally forbids wrong-side prints"
            )
        if family == "volatility" and not cadence_pass:
            reason = "cadence failed; volatility depends on fitted cadence"
        if combined_error is not None and family in passing:
            reason = f"the combined run failed: {combined_error}"
        verdicts[family] = {
            "status": status,
            "probe": probe_results[family],
            "combined": combined_results.get(family),
            **({"reason": reason} if reason else {}),
        }
        if status == "fitted":
            landing.extend(FAMILY_SLOTS[family])
            if family == "size" and fitted.get("size_round_frac") is not None:
                landing.append("size_round_frac")
    if combined_error is not None:
        verdicts["combined_run"] = {
            "status": "failed", "reason": combined_error,
        }
    if not cadence_pass:
        # The landing STOPS outright: no slot lands, whatever the other
        # families measured. Their verdicts remain in the artifact as the
        # record of what was measured.
        landing = []
        verdicts["landing"] = {
            "status": "stopped",
            "reason": "the cadence family failed wholesale; the generator "
                      "cannot represent MNQ cadence and the landing stops",
        }

    return {
        "binding": {
            "job_id": JOB_ID,
            "file_hashes": hashes,
            "preflight_artifact_hash": preflight_hash,
            "subcontract_hash": subcontract_hash(),
            "harness_tree_commit": harness_commit or git_commit(),
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
        "diagnostics": observed["diagnostics"],
        "fitted_candidates": fitted,
        "landing_set": sorted(landing),
    }


def git_commit() -> str:
    proc = subprocess.run(["git", "rev-parse", "HEAD"], capture_output=True,
                          text=True, cwd=ROOT)
    return proc.stdout.strip() if proc.returncode == 0 else "unknown"


def mode_fit() -> None:
    artifact = run_fit()
    write_json_atomic(ARTIFACT_FILE, artifact)
    print(json.dumps(
        {"verdicts": {k: v["status"] if isinstance(v, dict) else v
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
    dup = _st_row(t0)
    check("identical adjacent rows WITHIN a file are legitimate",
          len(list(parse_stream(stream_of([dup, dup])))) == 2)
    check("a duplicated row AT THE SEAM refuses",
          refuses(lambda: list(parse_stream(stream_of([dup], [dup]))),
                  "boundary"))
    check("distinct rows at one timestamp cross the seam as one parent",
          len(group_parents_batch(list(parse_stream(stream_of(
              [_st_row(t0)], [_st_row(t0, size=2)]))))) == 1)

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
        manifest = {
            "job_id": job if not tamper_manifest else "GLBX-OTHER",
            "files": {"glbx-st.tbbo.csv.zst": digest},
        }
        with open(os.path.join(fake_dir, "manifest.json"), "w") as fh:
            json.dump(manifest, fh)
        ledger = {"_version": 1, "jobs": {LEDGER_KEY: {
            "state": ledger_state, "job_id": job,
            "files": {"glbx-st.tbbo.csv.zst": digest},
        }}}
        ledger_path = os.path.join(fake_dir, "ledger.json")
        with open(ledger_path, "w") as fh:
            json.dump(ledger, fh)
        return ledger_path

    # A usable month: one parent per second for 100 seconds in each of 22
    # sessions, prices walking a few ticks (so mid returns exist and the
    # volatility target is nonzero), plus controllable defect rows.
    def month_lines(extra=(), unsided=0, locked=0, impure_session=None):
        lines = []
        for label, status in SESSION_INVENTORY:
            if status != "full":
                continue
            base = _st_ts(label, 12)
            iid = "99999" if label == impure_session else "12345"
            for i in range(100):
                price = 23_000 * 10**9 + (i % 3) * TICK_UNITS
                lines.append(_st_row(base + i * 10**9, price=price,
                                     iid="12345"))
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
            # a two-point histogram whose balance follows the median.
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
    check("cadence reproduces exactly and lands",
          artifact["verdicts"]["cadence"]["status"] == "fitted")
    check("the displacement solve converges on its target",
          abs(artifact["fitted_candidates"]["trade_displacement_ticks"]
              - artifact["observed"]["displacement_median_ticks"])
          <= 3 * DISPLACEMENT_BIN_TICKS)
    check("model B is skipped as structurally moot at a one-lot median",
          artifact["solves"]["size_round_frac"].get("status")
          == "skipped-structurally-moot")
    check("size_round_frac stays declared on the moot branch",
          "size_round_frac" not in artifact["landing_set"])
    check("the quote family gates width AND both top sizes",
          {"width", "top_bid", "top_ask"}
          <= set(artifact["verdicts"]["quote"]["probe"]))
    check("start_price carries its slot in the landing set when it passes",
          ("start_price" in artifact["landing_set"])
          == (artifact["verdicts"]["start_price"]["status"] == "fitted"))
    check("every solve record carries the frozen schema fields",
          all(
              {"domain", "coarse_points", "coarse_grid", "best_candidate",
               "termination",
               "tie_break", "evaluations"}
              <= set(artifact["solves"][k] if k != "latent_size_median"
                     else artifact["solves"][k]["model_a"])
              for k in ("latent_size_median", "trade_displacement_ticks",
                        "vol_scalar")
          ))

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
          artifact["verdicts"]["volatility"]["status"]
          == "declared-misrepresented"
          and artifact["verdicts"]["cadence"]["status"] == "fitted")
    check("the failed family's slot stays out of the landing set",
          "vol_scalar" not in artifact["landing_set"]
          and "children_mean" in artifact["landing_set"])

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
          and artifact["verdicts"]["landing"]["status"] == "stopped")

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
          artifact["verdicts"]["combined_run"]["status"] == "failed"
          and artifact["landing_set"] == []
          and all(v["status"] != "fitted"
                  for k, v in artifact["verdicts"].items()
                  if k in FAMILIES))

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
    if mode == "selftest":
        run_selftest()
    elif mode == "preflight":
        mode_preflight()
    else:
        mode_fit()


if __name__ == "__main__":
    main()
