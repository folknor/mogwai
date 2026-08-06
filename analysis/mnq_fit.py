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
    python3 analysis/mnq_fit.py cost12a
    python3 analysis/mnq_fit.py measure12a

`measure12a` is the protocol-12a measurement landing
(notes/protocol-12a-measurement-spec.md): the observed-side evidence
blocks and permutation counterfactuals (Brick O) plus the eight cached
generated FINAL walks (Brick G); the aggregation, ladder and committed
artifact land with Brick M. `cost12a` is the Brick G cost probe: a
7-day summary-vs-measure12a runtime and RSS gate that must pass before
the FINAL walks run.
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
import time
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
# The Brick V amendment, bound in the sub-contract: the hourly wall-time
# curves are recorded protocol-12 DIAGNOSTICS, not protocol-11 landing
# gates - the fit proved arrival and per-parent scale to fractions of a
# percent at the very hours whose 60s/300s contour missed, classifying the
# residual as an hour-dependent aggregation-law mismatch protocol 12
# inherits as hard successor gates. The bands and verdicts stay frozen; only
# their role in the landing decision moved.
WALLTIME_HOURLY_ROLE = "diagnostic"
# The shipped MNQ dow_weight, byte-for-byte (crates/mogwai-server/presets/
# mnq.toml): FROZEN, never refitted here (spec 2.3). The conditional
# intensity estimator solves the hour parameter GIVEN this day factor, and
# the Brick L preset test pins that the preset still carries exactly these
# values. Sun=0 .. Sat=6.
MNQ_DOW_WEIGHT = (1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000)

# ---------------------------------------------------------------------------
# The protocol-12a measurement sub-contract
# (notes/protocol-12a-measurement-spec.md, frozen at revision 11). These
# constants are measurement law for the measure12a mode; they join
# SUBCONTRACT_KEYS so preflight rebinds when any of them moves.
# ---------------------------------------------------------------------------

FAIL_HOURS_300 = (19, 20, 23)
FAIL_HOURS_60 = (20,)
HOT_HOURS = (19, 20)
COLD_HOURS = (23,)
RESIDUAL_WINDOW_S = 300
RESIDUAL_MIN_HISTORY = 1000
RESIDUAL_EXCEED_MULTIPLES = (4, 8, 16)   # trimmed-scale units, strict >
INNOVATION_EXCEED_ABS = (4, 8, 16)       # unit-variance units, strict >
PERMUTATION_REPLICATES = 16
PERMUTATION_VARIANTS = ("sign", "magnitude")  # tags 0 and 1 (3.4a)
BOOTSTRAP_REPLICATES = 10_000
BOOTSTRAP_BLOCK_SESSIONS = 5
BOOTSTRAP_BASE_SEED = 1342176408401967774
PERMUTATION_BASE_SEED = 7205759943768246531
CONTROL_TIE_BASE_SEED = 3141592653589793238
FAMILY_ENVELOPE_LEVEL = 0.95
SEED_DIRECTION_MIN = 7                   # of the 8 FINAL_SEEDS
FOLD_MIN_SESSIONS = 15
MATERIALITY_BAND = (0.8, 1.25)
GAP_CLOSE_MIN = 0.50
GAP_CLOSE_LCB_MIN = 0.25
GAP_CLOSE_EPS = 1e-9
COUNT_WINDOWS_S = (1, 5, 60)
WALL_HORIZONS_S = (1, 5, 15, 60, 300)
EXCEEDANCE_TICKS = (399, 642, 968)       # strict >
# Parent-count bins (spec 3.2): {0}, [1,65), [65,257), [257,1025),
# [1025,4097), [4097,inf) - exact half-open intervals, diagnostic strata
# only, named by their lower edge.
PARENT_COUNT_BIN_EDGES = (1, 65, 257, 1025, 4097)
PARENT_COUNT_BIN_NAMES = ("0", "1-64", "65-256", "257-1024",
                          "1025-4096", "4097+")
# Segment-relative label edges in seconds (spec 3.2): since_segment_open
# [0,300) | [300,1800) | [1800,inf); until_segment_close (1800,inf) |
# (300,1800] | (0,300]. Labels evaluated at minute start.
SEGMENT_LABEL_EDGES_S = (300, 1800)
SINCE_OPEN_BIN_NAMES = ("0-300", "300-1800", "1800+")
UNTIL_CLOSE_BIN_NAMES = ("1800+", "300-1800", "0-300")
MIN_1S_CELL_RETURNS = 2500
MIN_5S_CELL_RETURNS = 500
MIN_15S_CELL_RETURNS = 160
MIN_RESIDUAL_CELL = 1000
MIN_MINUTES_CELL = 30
MIN_BOUNDARY_MINUTES_CELL = 4
MIN_BOUNDARY_60S_CELL_RETURNS = 3
SIGMA_ESCALATION_MIN = 2.0
CONTROL_ESCALATION_MAX = 1.25
INITIATION_INNOVATION_MIN = 8


def splitmix64(x: int) -> int:
    """Bit-identical to crates/mogwai-protocol/src/seeds.rs."""
    x = (x + 0x9E37_79B9_7F4A_7C15) & 0xFFFF_FFFF_FFFF_FFFF
    z = x
    z = ((z ^ (z >> 30)) * 0xBF58_476D_1CE4_E5B9) & 0xFFFF_FFFF_FFFF_FFFF
    z = ((z ^ (z >> 27)) * 0x94D0_49BB_1331_11EB) & 0xFFFF_FFFF_FFFF_FFFF
    return z ^ (z >> 31)


def tuple_mix(base: int, fields) -> int:
    """The 3.4a multi-field derivation: fold splitmix64 over the fields in
    listed order. Session dates encode as the integer YYYYMMDD; variant
    tags sign=0, magnitude=1."""
    x = base & 0xFFFF_FFFF_FFFF_FFFF
    for value in fields:
        x = splitmix64(x ^ (int(value) & 0xFFFF_FFFF_FFFF_FFFF))
    return x


def fisher_yates(values: list, state: int) -> None:
    """The frozen 5.1 shuffle, in place over `values` in original stream
    order: state advances by splitmix64 per step, j = state mod (i+1)."""
    for i in range(len(values) - 1, 0, -1):
        state = splitmix64(state)
        j = state % (i + 1)
        values[i], values[j] = values[j], values[i]


def session_date_int(label: str) -> int:
    return int(label.replace("-", ""))


def parent_count_bin(n: int) -> str:
    if n == 0:
        return "0"
    for edge, name in zip(
        reversed(PARENT_COUNT_BIN_EDGES), reversed(PARENT_COUNT_BIN_NAMES)
    ):
        if n >= edge:
            return name
    raise AssertionError("unreachable: every n >= 1 has a bin")


def segment_labels(minute_start_ns: int, origin_ns: int,
                   end_ns: int) -> tuple[str, str]:
    """(since_open_bin, until_close_bin), evaluated at minute start
    (spec 3.2): since = minute_start - origin, until = end - minute_start."""
    since_s = (minute_start_ns - origin_ns) / 1e9
    until_s = (end_ns - minute_start_ns) / 1e9
    lo, hi = SEGMENT_LABEL_EDGES_S
    if since_s < lo:
        since = SINCE_OPEN_BIN_NAMES[0]
    elif since_s < hi:
        since = SINCE_OPEN_BIN_NAMES[1]
    else:
        since = SINCE_OPEN_BIN_NAMES[2]
    if until_s > hi:
        until = UNTIL_CLOSE_BIN_NAMES[0]
    elif until_s > lo:
        until = UNTIL_CLOSE_BIN_NAMES[1]
    else:
        until = UNTIL_CLOSE_BIN_NAMES[2]
    return since, until


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
    "SESSION_VOL_CORR_MIN", "MNQ_DOW_WEIGHT", "WALLTIME_HOURLY_ROLE",
    # Protocol 12a (notes/protocol-12a-measurement-spec.md section 7).
    "FAIL_HOURS_300", "FAIL_HOURS_60", "HOT_HOURS", "COLD_HOURS",
    "RESIDUAL_WINDOW_S", "RESIDUAL_MIN_HISTORY",
    "RESIDUAL_EXCEED_MULTIPLES", "INNOVATION_EXCEED_ABS",
    "PERMUTATION_REPLICATES", "PERMUTATION_VARIANTS",
    "BOOTSTRAP_REPLICATES", "BOOTSTRAP_BLOCK_SESSIONS",
    "BOOTSTRAP_BASE_SEED", "PERMUTATION_BASE_SEED",
    "CONTROL_TIE_BASE_SEED", "FAMILY_ENVELOPE_LEVEL",
    "SEED_DIRECTION_MIN", "FOLD_MIN_SESSIONS", "MATERIALITY_BAND",
    "GAP_CLOSE_MIN", "GAP_CLOSE_LCB_MIN", "GAP_CLOSE_EPS",
    "COUNT_WINDOWS_S", "WALL_HORIZONS_S", "EXCEEDANCE_TICKS",
    "PARENT_COUNT_BIN_EDGES", "PARENT_COUNT_BIN_NAMES",
    "SEGMENT_LABEL_EDGES_S", "SINCE_OPEN_BIN_NAMES",
    "UNTIL_CLOSE_BIN_NAMES", "MIN_1S_CELL_RETURNS",
    "MIN_5S_CELL_RETURNS", "MIN_15S_CELL_RETURNS", "MIN_RESIDUAL_CELL",
    "MIN_MINUTES_CELL", "MIN_BOUNDARY_MINUTES_CELL",
    "MIN_BOUNDARY_60S_CELL_RETURNS", "SIGMA_ESCALATION_MIN",
    "CONTROL_ESCALATION_MAX", "INITIATION_INNOVATION_MIN",
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


# ---------------------------------------------------------------------------
# Protocol 12a: the observed-side measurement engine (spec Brick O).
# One session at a time (the frozen memory contract): the stream stays
# chronological, at most one session's parent endpoints and returns are
# retained in packed arrays, the per-session sufficient records are
# emitted at session close and the arrays released.
# ---------------------------------------------------------------------------

HORIZON_PAIRS = ((1, 5), (5, 15), (15, 60), (60, 300))
VARIANT_TAGS = {"sign": 0, "magnitude": 1}
SEGMENT_INDEX = {"overnight": 0, "post_halt": 1}


def _minute_segment(minute_start_ns: int):
    """(session, segment) of a UTC minute. CME boundaries are minute
    aligned, so every populated minute lies in exactly one segment."""
    session, segment, _hour = minute_fields(minute_start_ns)
    return session, segment


def _wall_boundaries(seg: dict, horizon_s: int, origin: int, end: int):
    """The 4.6-convention fixed-horizon chain over one segment's valid-mid
    endpoint series, returning per-boundary records:

        (boundary_ns, asof_logmid | None, emitted, hour, ret | None)

    asof at a boundary is the log mid of the last endpoint with ts <=
    boundary (equal timestamps update first - the protocol-11 pending
    rule). The first boundary with an asof establishes; a boundary emits
    unless its window crosses a UTC hour; boundaries run origin + k*W
    strictly inside the segment; trailing boundaries settle through the
    segment end."""
    w_ns = horizon_s * 10**9
    ts = seg["mid_ts"]
    logmid = seg["mid_log"]
    n = len(ts)
    out = []
    i = 0
    prev = None
    b = origin + w_ns
    while b < end:
        while i < n and ts[i] <= b:
            i += 1
        asof = logmid[i - 1] if i > 0 else None
        emitted = False
        ret = None
        if asof is not None:
            if prev is not None:
                b_hour = (b // 3_600_000_000_000) % 24
                s_hour = ((b - w_ns) // 3_600_000_000_000) % 24
                if b_hour == s_hour:
                    emitted = True
                    ret = asof - prev
            prev = asof
        b_hour = (b // 3_600_000_000_000) % 24
        out.append((b, asof, emitted, b_hour, ret))
        b += w_ns
    return out


def _robust_from_stats(count: int, sum_abs: float, max_abs: float):
    if count < 2:
        return None
    return (sum_abs - max_abs) / (count - 1)


class _M12aSession:
    """Packed per-session accumulation state."""

    def __init__(self, session: str):
        import array
        self.session = session
        self.segments: dict[str, dict] = {}
        self.order: list[str] = []
        # minute_idx -> [trade_lo, trade_hi] over ALL structurally valid
        # prints (unsided and invalid-book included).
        self.trade_min: dict[int, list] = {}
        # minute_idx -> [mid2_lo, mid2_hi] in half-ticks, valid-book
        # inferred parents only.
        self.quote_min: dict[int, list] = {}
        # minute_idx -> sided parent count by first-child timestamp.
        self.n_min: dict[int, int] = {}
        self._array = array

    def seg(self, segment: str) -> dict:
        s = self.segments.get(segment)
        if s is None:
            s = self.segments[segment] = {
                "parent_ts": self._array.array("q"),
                "mid_ts": self._array.array("q"),
                "mid_log": self._array.array("d"),
            }
            self.order.append(segment)
        return s


def _m12a_block1(state: _M12aSession) -> list[dict]:
    """The exact sparse joint histogram (spec 3.5), one record per
    distinct key with its occurrence count."""
    hist: dict[tuple, int] = {}
    minutes = set(state.trade_min) | set(state.n_min)
    for minute in minutes:
        start_ns = minute * 60_000_000_000
        session, segment = _minute_segment(start_ns)
        if session != state.session or segment is None:
            # A structurally valid print outside the session's own open
            # segments would be a session-assignment defect.
            raise Refusal(
                f"minute {minute} carries rows but maps to "
                f"({session}, {segment}) not ({state.session}, open)"
            )
        origin = segment_origin_ns(session, segment)
        end = segment_end_ns(session, segment)
        since_bin, until_bin = segment_labels(start_ns, origin, end)
        hour = (start_ns // 3_600_000_000_000) % 24
        tr = state.trade_min.get(minute)
        trade_ticks = (tr[1] - tr[0]) // TICK_UNITS if tr else 0
        qr = state.quote_min.get(minute)
        quote_half = (qr[1] - qr[0]) if qr else None
        n = state.n_min.get(minute, 0)
        key = (n, quote_half, trade_ticks, hour, since_bin, until_bin)
        hist[key] = hist.get(key, 0) + 1
    return [
        {"n": k[0], "quote_range_half_ticks": k[1],
         "trade_range_ticks": k[2], "hour": k[3],
         "since_open_bin": k[4], "until_close_bin": k[5], "count": v}
        for k, v in sorted(
            hist.items(),
            key=lambda kv: (kv[0][0], -1 if kv[0][1] is None else kv[0][1],
                            kv[0][2], kv[0][3], kv[0][4], kv[0][5]),
        )
    ]


def _window_schedule(session: str, segment: str, w: int) -> list:
    """The pure-calendar window schedule ONE iterator both the block-2
    accumulator and the expected-exposure judge consume, so their
    endpoint rules cannot drift: (start, stop, endpoint_hour | None)
    for every segment-origin-aligned half-open window strictly
    contained in the segment. None marks an hour-crossing window
    (endpoint-hour attribution: a window ending exactly ON the hour
    boundary crosses) - excluded from every cell, resets runs and
    lag-1 pairs."""
    origin = segment_origin_ns(session, segment)
    end = segment_end_ns(session, segment)
    out = []
    w_ns = w * 10**9
    start = origin
    while start + w_ns <= end:
        stop = start + w_ns
        s_hour = (start // 3_600_000_000_000) % 24
        e_hour = (stop // 3_600_000_000_000) % 24
        out.append((start, stop, e_hour if s_hour == e_hour else None))
        start = stop
    return out


_EXPECTED_WINDOWS_CACHE: dict[tuple, dict] = {}


def expected_scheduled_windows(session: str, hour: int, w: int) -> int:
    """The calendar-derived expected scheduled-window count for one
    (session, hour, window length), independent of any candidate's own
    data (spec 3.3 scheduled-exposure completeness): 59 per full hour
    at 60 s, 44 at the halt hour."""
    key = (session, w)
    counts = _EXPECTED_WINDOWS_CACHE.get(key)
    if counts is None:
        counts = {}
        for segment in ("overnight", "post_halt"):
            for _start, _stop, h in _window_schedule(
                    session, segment, w):
                if h is not None:
                    counts[h] = counts.get(h, 0) + 1
        _EXPECTED_WINDOWS_CACHE[key] = counts
    return counts.get(hour, 0)


def _m12a_block2(state: _M12aSession) -> dict:
    """Per (hour, window_s): scheduled/zero window counts, exact count and
    run-length histograms, and the lag-1 sufficient moments. Windows are
    half-open, segment-origin aligned, strictly contained in the open
    segment, attributed by endpoint hour; hour-crossing windows excluded;
    lag-1 pairs and runs reset at segment and hour boundaries."""
    cells: dict[tuple, dict] = {}

    def cell(hour: int, w: int) -> dict:
        c = cells.get((hour, w))
        if c is None:
            c = cells[(hour, w)] = {
                "scheduled_windows": 0, "zero_windows": 0,
                "count_hist": {}, "run_length_hist": {},
                "paired_lag_count": 0, "sum_x": 0, "sum_y": 0,
                "sumsq_x": 0, "sumsq_y": 0, "sum_xy": 0,
            }
        return c

    for segment in state.order:
        seg = state.segments[segment]
        pts = seg["parent_ts"]
        n = len(pts)
        for w in COUNT_WINDOWS_S:
            i = 0
            prev_count = None   # lag-1 partner within segment+hour
            prev_hour = None    # hour of the previous QUALIFIED window
            run = 0             # open run of nonzero windows, prev_hour

            def close_run():
                nonlocal run
                if run:
                    rc = cell(prev_hour, w)["run_length_hist"]
                    rc[run] = rc.get(run, 0) + 1
                run = 0

            for start, stop, hour in _window_schedule(
                    state.session, segment, w):
                while i < n and pts[i] < start:
                    i += 1
                j = i
                while j < n and pts[j] < stop:
                    j += 1
                count = j - i
                i = j
                if hour is None:
                    # Hour-crossing window - INCLUDING one ending
                    # exactly on the hour boundary (endpoint-hour
                    # attribution, matching the fixed-horizon
                    # convention): excluded; runs and pairs reset.
                    close_run()
                    prev_count = None
                    prev_hour = None
                    continue
                if prev_hour is not None and hour != prev_hour:
                    # Hour boundary between windows: runs and pairs reset.
                    close_run()
                    prev_count = None
                c = cell(hour, w)
                c["scheduled_windows"] += 1
                if count == 0:
                    c["zero_windows"] += 1
                ch = c["count_hist"]
                ch[count] = ch.get(count, 0) + 1
                if prev_count is not None:
                    c["paired_lag_count"] += 1
                    c["sum_x"] += prev_count
                    c["sum_y"] += count
                    c["sumsq_x"] += prev_count * prev_count
                    c["sumsq_y"] += count * count
                    c["sum_xy"] += prev_count * count
                prev_count = count
                prev_hour = hour
                if count > 0:
                    run += 1
                else:
                    close_run()
            close_run()
    out: dict[str, dict] = {}
    for (hour, w), c in sorted(cells.items()):
        # The serialized per-session Block2Cell carries the derived
        # fields beside the sufficient statistics (spec section 10).
        out.setdefault(str(hour), {})[str(w)] = finish_block2_cell(c)
    return out


def _m12a_block3(state: _M12aSession) -> dict:
    """Per (hour, horizon) robust/rms sufficient statistics, per (hour,
    adjacent pair) VR / covariance records, hour-20 label-pair cells, and
    the per-hour lag-1 parent-return descriptive scalar. Also returns the
    per-(segment, hour, horizon) emitted-window index ranges the
    permutation stage consumes."""
    cells: dict[tuple, list] = {}      # (hour, h) -> [n, sum, sumsq,
    #                                     sum_abs, max_abs]
    pairs: dict[tuple, list] = {}      # (hour, (h,H)) -> [n, sum_RH2,
    #                                     sum_comp2]
    h20: dict[tuple, list] = {}        # (labels, h) -> cell list
    lag1: dict[int, list] = {}         # hour -> [n, sx, sy, sxx, syy, sxy]
    perm_windows: dict[tuple, dict] = {}

    for segment in state.order:
        seg = state.segments[segment]
        origin = segment_origin_ns(state.session, segment)
        end = segment_end_ns(state.session, segment)
        # Amendment A: every (segment, hour) the segment spans - every
        # UTC hour interval with a positive intersection with
        # [origin, end) - materializes a permutation cell, so hours
        # with no emitted windows still emit all-zero PermRecords.
        hour_ns = 3_600_000_000_000
        t = (origin // hour_ns) * hour_ns
        while t < end:
            perm_windows.setdefault((segment, (t // hour_ns) % 24), {})
            t += hour_ns
        series = {
            h: _wall_boundaries(seg, h, origin, end)
            for h in WALL_HORIZONS_S
        }
        asof_at = {
            h: {b: a for (b, a, _e, _hh, _r) in series[h]}
            for h in WALL_HORIZONS_S
        }
        for h in WALL_HORIZONS_S:
            for b, _asof, emitted, hour, ret in series[h]:
                if not emitted:
                    continue
                cell = cells.setdefault((hour, h), [0, 0.0, 0.0, 0.0, 0.0])
                cell[0] += 1
                cell[1] += ret
                cell[2] += ret * ret
                cell[3] += abs(ret)
                if abs(ret) > cell[4]:
                    cell[4] = abs(ret)
                if hour == 20:
                    labels = segment_labels(b, origin, end)
                    hc = h20.setdefault((labels, h), [0, 0.0, 0.0, 0.0, 0.0])
                    hc[0] += 1
                    hc[1] += ret
                    hc[2] += ret * ret
                    hc[3] += abs(ret)
                    if abs(ret) > hc[4]:
                        hc[4] = abs(ret)
        for (h, big) in HORIZON_PAIRS:
            k = big // h
            h_asof = asof_at[h]
            h_ns = h * 10**9
            for b, _asof, emitted, hour, ret in series[big]:
                if not emitted:
                    continue
                comp2 = 0.0
                ok = True
                for j in range(k):
                    hi = h_asof.get(b - j * h_ns)
                    lo = h_asof.get(b - (j + 1) * h_ns)
                    if hi is None or lo is None:
                        ok = False
                        break
                    d = hi - lo
                    comp2 += d * d
                if not ok:
                    continue
                pc = pairs.setdefault((hour, (h, big)), [0, 0.0, 0.0])
                pc[0] += 1
                pc[1] += ret * ret
                pc[2] += comp2
        # Permutation window index ranges: for 60/300, emitted windows map
        # to the half-open return-index range with endpoint ts in
        # (b - W, b]. Return i is the adjacent-mid return ENDING at
        # mid_ts[i+1] (the first endpoint has no return).
        ts = seg["mid_ts"]
        n_mid = len(ts)
        for h in (60, 300):
            w_ns = h * 10**9
            i = 1  # endpoint index; return index is endpoint index - 1
            for b, _asof, emitted, hour, _ret in series[h]:
                lo_ts = b - w_ns
                while i < n_mid and ts[i] <= lo_ts:
                    i += 1
                j = i
                while j < n_mid and ts[j] <= b:
                    j += 1
                if emitted:
                    pw = perm_windows.setdefault((segment, hour), {})
                    pw.setdefault(h, []).append((i - 1, j - 1))
                i = j
        # Lag-1 parent-return scalar: consecutive adjacent-mid returns in
        # the segment, attributed by the LATER return's endpoint hour.
        logmid = seg["mid_log"]
        prev_r = None
        for idx in range(1, n_mid):
            r = logmid[idx] - logmid[idx - 1]
            if prev_r is not None:
                hour = (ts[idx] // 3_600_000_000_000) % 24
                acc = lag1.setdefault(hour, [0, 0.0, 0.0, 0.0, 0.0, 0.0])
                acc[0] += 1
                acc[1] += prev_r
                acc[2] += r
                acc[3] += prev_r * prev_r
                acc[4] += r * r
                acc[5] += prev_r * r
            prev_r = r

    def cell_dict(c: list) -> dict:
        # The serialized Block3Cell (spec section 10): derived scales,
        # never accumulator internals.
        n = c[0]
        return {
            "return_count": n,
            "robust_scale": _robust_from_stats(n, c[3], c[4]),
            "rms_scale": math.sqrt(c[2] / n) if n else None,
        }

    def pair_dict(pc: list) -> dict:
        # The serialized Block3Pair: window count plus derived VR and
        # covariance records.
        n, sum_rh2, sum_comp2 = pc
        return {
            "window_count": n,
            "vr": (sum_rh2 / sum_comp2) if sum_comp2 > 0 else None,
            "cov_contrib": (sum_rh2 - sum_comp2) / n if n else None,
            "cov_contrib_norm": (
                (sum_rh2 - sum_comp2) / sum_rh2 if sum_rh2 > 0 else None
            ),
        }

    def corr(acc: list):
        n, sx, sy, sxx, syy, sxy = acc
        if n < 2:
            return None
        vx = sxx - sx * sx / n
        vy = syy - sy * sy / n
        if vx <= 0 or vy <= 0:
            return None
        return (sxy - sx * sy / n) / math.sqrt(vx * vy)

    return {
        "cells": {
            str(hour): {
                str(h): cell_dict(cells[(hour, h)])
                for h in WALL_HORIZONS_S if (hour, h) in cells
            }
            for hour in sorted({hh for hh, _ in cells})
        },
        "pairs": {
            str(hour): {
                f"{h}-{big}": pair_dict(pairs[(hour, (h, big))])
                for (h, big) in HORIZON_PAIRS
                if (hour, (h, big)) in pairs
            }
            for hour in sorted({hh for hh, _ in pairs})
        },
        "lag1_parent_autocorr": {
            str(hour): corr(acc) for hour, acc in sorted(lag1.items())
        },
        "hour20_labels": _h20_shape(h20),
    }, perm_windows


def _h20_shape(h20: dict) -> dict:
    out: dict[str, dict] = {}
    for (labels, h), c in sorted(h20.items()):
        key = f"{labels[0]}|{labels[1]}"
        out.setdefault(key, {})[str(h)] = {
            "return_count": c[0],
            "robust_scale": _robust_from_stats(c[0], c[3], c[4]),
            "rms_scale": math.sqrt(c[2] / c[0]) if c[0] else None,
        }
    return out


def _m12a_block4(state: _M12aSession) -> tuple[dict, list]:
    """The model-free past-only standardizer (spec block 4): trailing
    300 s same-segment history, minimum 1000 returns, one-max-trimmed
    mean absolute scale excluding the current return, z = r / scale.
    Zeros stay in history and population. A nonpositive or non-finite
    scale OMITS that residual (the return still enters history) per
    the rev-11 frozen exception; the omissions come back as one
    Amendment-F RefusalRec per (session, hour) - the sole RefusalRec
    class owning omitted observations rather than refusal-caused
    nulls. No duplicate record for the pooled "all" cell."""
    per_hour: dict[int, dict] = {}
    omitted: dict[int, int] = {}

    def hour_acc(hour: int) -> dict:
        acc = per_hour.get(hour)
        if acc is None:
            acc = per_hour[hour] = {
                "residual_count": 0, "warmup_excluded": 0, "zeros": 0,
                "nz_abs": [], "exceed": [0] * len(RESIDUAL_EXCEED_MULTIPLES),
            }
        return acc

    win_ns = RESIDUAL_WINDOW_S * 10**9
    for segment in state.order:
        seg = state.segments[segment]
        ts = seg["mid_ts"]
        logmid = seg["mid_log"]
        n = len(ts)
        window = deque()      # (endpoint_ts, abs_r)
        maxq = deque()        # decreasing abs_r
        run_sum = 0.0
        for idx in range(1, n):
            t = ts[idx]
            r = logmid[idx] - logmid[idx - 1]
            hour = (t // 3_600_000_000_000) % 24
            lo = t - win_ns
            while window and window[0][0] < lo:
                old = window.popleft()
                run_sum -= old[1]
                if maxq and maxq[0] == old[1]:
                    maxq.popleft()
            count = len(window)
            if count < RESIDUAL_MIN_HISTORY:
                hour_acc(hour)["warmup_excluded"] += 1
                hour_acc("all")["warmup_excluded"] += 1
            else:
                mx = maxq[0]
                scale = (run_sum - mx) / (count - 1)
                if not math.isfinite(scale) or scale <= 0:
                    # Omitted from the residual population of BOTH the
                    # hour cell and the pooled "all" cell; the return
                    # still enters history below.
                    omitted[hour] = omitted.get(hour, 0) + 1
                else:
                    z = r / scale
                    az = abs(z)
                    # The per-hour cell AND the pooled all-hours cell
                    # (the innovation family's fourth inventory
                    # metric).
                    for acc in (hour_acc(hour), hour_acc("all")):
                        acc["residual_count"] += 1
                        if r == 0.0:
                            acc["zeros"] += 1
                        else:
                            acc["nz_abs"].append(az)
                        for mi, mult in enumerate(
                                RESIDUAL_EXCEED_MULTIPLES):
                            if az > mult:
                                acc["exceed"][mi] += 1
            a = abs(r)
            window.append((t, a))
            run_sum += a
            while maxq and maxq[-1] < a:
                maxq.pop()
            maxq.append(a)

    out: dict[str, dict] = {}
    for hour, acc in sorted(per_hour.items(), key=lambda kv: str(kv[0])):
        rc = acc["residual_count"]
        nz = sorted(acc["nz_abs"])
        p90 = nearest_rank_list(nz, 0.90) if nz else None
        p99 = nearest_rank_list(nz, 0.99) if nz else None
        p999 = nearest_rank_list(nz, 0.999) if nz else None
        out[str(hour)] = {
            "residual_count": rc,
            "warmup_excluded": acc["warmup_excluded"],
            "zero_fraction": acc["zeros"] / rc if rc else None,
            "nz_abs_p90": p90, "nz_abs_p99": p99, "nz_abs_p999": p999,
            "ratio_p99_p90": (p99 / p90) if p99 and p90 else None,
            "ratio_p999_p99": (p999 / p99) if p999 and p99 else None,
            "exceed_4": acc["exceed"][0] / rc if rc else None,
            "exceed_8": acc["exceed"][1] / rc if rc else None,
            "exceed_16": acc["exceed"][2] / rc if rc else None,
        }
    refusals = [
        {"scope": f"observed session {state.session}",
         "cell": f"block4 hour {hour} standardizer",
         "reason": f"{n} residuals omitted: nonpositive or "
                   f"non-finite trailing scale"}
        for hour, n in sorted(omitted.items())
    ]
    return out, refusals


def _windows_stats(values: list[float], windows: list[list[int]]):
    """(count, sum_abs, max_abs) over the emitted windows' return sums
    - the Amendment A sufficient statistics. Emitted zero returns
    count; an all-zero population has zero sums."""
    cnt = 0
    s_abs = 0.0
    m_abs = 0.0
    for widx in windows:
        s = 0.0
        for p in widx:
            s += values[p]
        a = abs(s)
        cnt += 1
        s_abs += a
        if a > m_abs:
            m_abs = a
    return cnt, s_abs, m_abs


def _m12a_permutations(state: _M12aSession, perm_windows: dict) -> list:
    """The 5.1 counterfactuals: per (segment, hour) cell, both variants,
    16 replicates. The boundary partition and establishment depend only
    on timestamps, so each emitted window's permuted return is the sum of
    permuted cell values over its fixed index range."""
    records = []
    for segment in state.order:
        seg = state.segments[segment]
        logmid = seg["mid_log"]
        returns = [logmid[i] - logmid[i - 1] for i in range(1, len(logmid))]
        ts = seg["mid_ts"]
        # One pass grouping return indices by endpoint hour.
        by_hour: dict[int, list[int]] = {}
        for i in range(len(returns)):
            by_hour.setdefault(
                (ts[i + 1] // 3_600_000_000_000) % 24, []
            ).append(i)
        hours_here = sorted({
            hour for (sgm, hour) in perm_windows if sgm == segment
        })
        for hour in hours_here:
            pw = perm_windows[(segment, hour)]
            # The cell's return positions: endpoint hour == hour. The
            # emitted windows lie wholly inside the hour, so every index
            # they cover belongs to the cell. A cell with ZERO adjacent
            # returns still emits its records (Amendment A): its emitted
            # windows carry zero returns, the sums are zero, and the
            # combined floor decides qualification downstream.
            cell_idx = by_hour.get(hour, [])
            values = [returns[i] for i in cell_idx]
            pos_of = {orig: k for k, orig in enumerate(cell_idx)}
            windows = {
                h: [
                    [pos_of[i] for i in range(lo, hi) if i in pos_of]
                    for (lo, hi) in pw.get(h, [])
                ]
                for h in (60, 300)
            }
            nz_pos = [k for k, v in enumerate(values) if v != 0.0]
            date_int = session_date_int(state.session)
            seg_idx = SEGMENT_INDEX[segment]
            for variant in PERMUTATION_VARIANTS:
                vtag = VARIANT_TAGS[variant]
                for rep in range(PERMUTATION_REPLICATES):
                    state_seed = tuple_mix(
                        PERMUTATION_BASE_SEED,
                        [date_int, seg_idx, hour, vtag, rep],
                    )
                    perm = list(nz_pos)
                    fisher_yates(perm, state_seed)
                    shuffled = list(values)
                    if variant == "sign":
                        # Shuffle SIGNS among nonzero: magnitude order
                        # fixed, sign sequence permuted.
                        signs = [1.0 if values[p] > 0 else -1.0
                                 for p in perm]
                        for k, p in enumerate(nz_pos):
                            shuffled[p] = abs(values[p]) * signs[k]
                    else:
                        # Shuffle MAGNITUDES among nonzero: sign sequence
                        # fixed, magnitudes permuted.
                        mags = [abs(values[p]) for p in perm]
                        for k, p in enumerate(nz_pos):
                            sgn = 1.0 if values[p] > 0 else -1.0
                            shuffled[p] = sgn * mags[k]
                    rec = {
                        "segment_index": seg_idx, "hour": hour,
                        "variant": variant, "replicate": rep,
                    }
                    for h in (60, 300):
                        cnt, s_abs, m_abs = _windows_stats(
                            shuffled, windows[h]
                        )
                        rec[f"return_count_{h}"] = cnt
                        rec[f"sum_abs_{h}"] = s_abs
                        rec[f"max_abs_{h}"] = m_abs
                    records.append(rec)
    return records


def m12a_close_session(state: _M12aSession) -> dict:
    # Both calendar segments exist for every usable session, parents or
    # not: a segment without sided parents still contributes its
    # scheduled zero-count windows (exposure completeness) and its
    # never-established horizon chains.
    state.seg("overnight")
    state.seg("post_halt")
    block3, perm_windows = _m12a_block3(state)
    block4, block4_refusals = _m12a_block4(state)
    segments = [
        {"segment_index": SEGMENT_INDEX[sgm],
         "open_ns": segment_origin_ns(state.session, sgm),
         "close_ns": segment_end_ns(state.session, sgm)}
        for sgm in state.order
    ]
    return {
        "session_date": state.session,
        "segments": segments,
        "block1_hist": _m12a_block1(state),
        "block2": _m12a_block2(state),
        "block3": block3,
        "block4": block4,
        "permutations": _m12a_permutations(state, perm_windows),
        "refusals": block4_refusals,
    }


# --- 12a aggregation (spec 3.5): per-session records to monthly -----------


HORIZON_FLOORS = {
    1: "MIN_1S_CELL_RETURNS", 5: "MIN_5S_CELL_RETURNS",
    15: "MIN_15S_CELL_RETURNS", 60: "MIN_60S_CELL_RETURNS",
    300: "MIN_300S_CELL_RETURNS",
}


def horizon_floor(h: int) -> int:
    return globals()[HORIZON_FLOORS[h]]


def median_or_none(values: list):
    vals = sorted(v for v in values if v is not None)
    if not vals:
        return None
    return vals[(len(vals) - 1) // 2] if len(vals) % 2 else (
        # Nearest-rank median: the harness convention takes the
        # ceil(n/2)-th order statistic, matching nearest_rank_list(0.5).
        vals[len(vals) // 2 - 1]
    )


def pool_block1_hists(hists: list[list[dict]]) -> dict[tuple, int]:
    pooled: dict[tuple, int] = {}
    for hist in hists:
        for rec in hist:
            key = (rec["n"], rec["quote_range_half_ticks"],
                   rec["trade_range_ticks"], rec["hour"],
                   rec["since_open_bin"], rec["until_close_bin"])
            pooled[key] = pooled.get(key, 0) + rec["count"]
    return pooled


def hist_to_records(pooled: dict[tuple, int]) -> list[dict]:
    return [
        {"n": k[0], "quote_range_half_ticks": k[1],
         "trade_range_ticks": k[2], "hour": k[3],
         "since_open_bin": k[4], "until_close_bin": k[5], "count": v}
        for k, v in sorted(
            pooled.items(),
            key=lambda kv: (kv[0][0], -1 if kv[0][1] is None else kv[0][1],
                            kv[0][2], kv[0][3], kv[0][4], kv[0][5]),
        )
    ]


def weighted_nearest_rank(pairs: list[tuple], q: float):
    """Exact nearest rank over (value, weight) pairs: sort ascending by
    value, return the first value whose cumulative weight reaches
    q * total (spec 5.2's literal rule, shared by every quantile over
    histogram mass)."""
    if not pairs:
        return None
    pairs = sorted(pairs)
    total = sum(w for _v, w in pairs)
    if total <= 0:
        return None
    target = q * total
    cum = 0
    for v, w in pairs:
        cum += w
        if cum >= target:
            return v
    return pairs[-1][0]


def block1_summary(pooled: dict[tuple, int], hour_filter=None,
                   label_filter=None) -> dict:
    """One Block1Summary over the pooled sparse histogram, optionally
    restricted to one hour and/or one (since, until) label pair."""
    rows = [
        (k, c) for k, c in pooled.items()
        if (hour_filter is None or k[3] == hour_filter)
        and (label_filter is None or (k[4], k[5]) == label_filter)
    ]
    minute_count = sum(c for _k, c in rows)
    quote_rows = [(k, c) for k, c in rows if k[1] is not None]
    quote_denom = sum(c for _k, c in quote_rows)

    def q_of(pairs, q):
        return weighted_nearest_rank(pairs, q)

    n_pairs = [(k[0], c) for k, c in rows]
    tr_pairs = [(k[2], c) for k, c in rows]
    qr_pairs = [(k[1], c) for k, c in quote_rows]
    sq_pairs = [
        (k[2] / math.sqrt(k[0]), c) for k, c in rows if k[0] >= 1
    ]
    exceed = {
        t: sum(c for k, c in rows if k[2] > t) for t in EXCEEDANCE_TICKS
    }
    tr_p99 = q_of(tr_pairs, 0.99)
    qr_p99 = q_of(qr_pairs, 0.99)
    ratio = None
    if tr_p99 is not None and qr_p99 not in (None, 0):
        ratio = tr_p99 / (qr_p99 / 2)  # half-ticks to ticks before division

    def bin_summary(bin_name: str) -> dict:
        brows = [(k, c) for k, c in rows
                 if parent_count_bin(k[0]) == bin_name]
        bcount = sum(c for _k, c in brows)
        bq = [(k[1], c) for k, c in brows if k[1] is not None]
        bt = [(k[2], c) for k, c in brows]
        bs = ([(k[2] / math.sqrt(k[0]), c) for k, c in brows]
              if bin_name != "0" else [])
        return {
            "minute_count": bcount,
            "quote_range_denominator": sum(c for _k, c in bq),
            "quote_range_p50": q_of(bq, 0.50),
            "quote_range_p90": q_of(bq, 0.90),
            "quote_range_p99": q_of(bq, 0.99),
            "quote_range_p999": q_of(bq, 0.999),
            "trade_range_p50": q_of(bt, 0.50),
            "trade_range_p90": q_of(bt, 0.90),
            "trade_range_p99": q_of(bt, 0.99),
            "trade_range_p999": q_of(bt, 0.999),
            "trade_range_sqrt_n_p50": q_of(bs, 0.50),
            "trade_range_sqrt_n_p90": q_of(bs, 0.90),
            "trade_range_sqrt_n_p99": q_of(bs, 0.99),
        }

    return {
        "minute_count": minute_count,
        "quote_range_denominator": quote_denom,
        "n_p50": q_of(n_pairs, 0.50), "n_p90": q_of(n_pairs, 0.90),
        "n_p99": q_of(n_pairs, 0.99), "n_p999": q_of(n_pairs, 0.999),
        "quote_range_p50": q_of(qr_pairs, 0.50),
        "quote_range_p90": q_of(qr_pairs, 0.90),
        "quote_range_p99": qr_p99,
        "quote_range_p999": q_of(qr_pairs, 0.999),
        "trade_range_p50": q_of(tr_pairs, 0.50),
        "trade_range_p90": q_of(tr_pairs, 0.90),
        "trade_range_p99": tr_p99,
        "trade_range_p999": q_of(tr_pairs, 0.999),
        "trade_range_sqrt_n_p50": q_of(sq_pairs, 0.50),
        "trade_range_sqrt_n_p90": q_of(sq_pairs, 0.90),
        "trade_range_sqrt_n_p99": q_of(sq_pairs, 0.99),
        "exceed_399": exceed[399], "exceed_642": exceed[642],
        "exceed_968": exceed[968], "denominator": minute_count,
        "trade_to_quote_p99_ratio": ratio,
        "by_parent_count_bin": {
            name: bin_summary(name) for name in PARENT_COUNT_BIN_NAMES
        },
    }


def block1_blocks(pooled: dict[tuple, int]) -> dict:
    hours = sorted({k[3] for k in pooled})
    label_pairs = sorted({(k[4], k[5]) for k in pooled})
    return {
        "hist": hist_to_records(pooled),
        "summary": {
            str(h): block1_summary(pooled, hour_filter=h) for h in hours
        },
        "by_labels": {
            f"{lp[0]}|{lp[1]}": {
                str(h): block1_summary(pooled, hour_filter=h,
                                       label_filter=lp)
                for h in sorted({k[3] for k in pooled
                                 if (k[4], k[5]) == lp})
            }
            for lp in label_pairs
        },
    }


def pool_block2(sessions: list[dict]) -> dict:
    """Pool the exact per-session count histograms and moments, then
    derive the scalar fields."""
    pooled: dict[tuple, dict] = {}
    for rec in sessions:
        for hour_s, per_w in rec.items():
            for w_s, c in per_w.items():
                key = (int(hour_s), int(w_s))
                p = pooled.get(key)
                if p is None:
                    p = pooled[key] = {
                        "scheduled_windows": 0, "zero_windows": 0,
                        "count_hist": {}, "run_length_hist": {},
                        "paired_lag_count": 0, "sum_x": 0, "sum_y": 0,
                        "sumsq_x": 0, "sumsq_y": 0, "sum_xy": 0,
                    }
                p["scheduled_windows"] += c["scheduled_windows"]
                p["zero_windows"] += c["zero_windows"]
                for k, v in c["count_hist"].items():
                    kk = int(k)
                    p["count_hist"][kk] = p["count_hist"].get(kk, 0) + v
                for k, v in c["run_length_hist"].items():
                    kk = int(k)
                    p["run_length_hist"][kk] = (
                        p["run_length_hist"].get(kk, 0) + v
                    )
                for f in ("paired_lag_count", "sum_x", "sum_y",
                          "sumsq_x", "sumsq_y", "sum_xy"):
                    p[f] += c[f]
    out: dict[str, dict] = {}
    for (hour, w), p in sorted(pooled.items()):
        out.setdefault(str(hour), {})[str(w)] = finish_block2_cell(p)
    return out


def finish_block2_cell(p: dict) -> dict:
    sched = p["scheduled_windows"]
    total = sum(p["count_hist"].values())
    ssum = sum(k * v for k, v in p["count_hist"].items())
    ssq = sum(k * k * v for k, v in p["count_hist"].items())
    mean = ssum / total if total else None
    var = ssq / total - mean * mean if total else None
    fano = var / mean if mean else None
    n = p["paired_lag_count"]
    lag1 = None
    if n >= 2:
        vx = p["sumsq_x"] - p["sum_x"] ** 2 / n
        vy = p["sumsq_y"] - p["sum_y"] ** 2 / n
        if vx > 0 and vy > 0:
            lag1 = (p["sum_xy"] - p["sum_x"] * p["sum_y"] / n) \
                / math.sqrt(vx * vy)
    count_pairs = list(p["count_hist"].items())
    runs = list(p["run_length_hist"].items())
    return {
        "scheduled_windows": sched,
        "zero_windows": p["zero_windows"],
        "count_hist": {str(k): v for k, v in sorted(p["count_hist"].items())},
        "run_length_hist": {
            str(k): v for k, v in sorted(p["run_length_hist"].items())
        },
        "paired_lag_count": n,
        "sum_x": p["sum_x"], "sum_y": p["sum_y"],
        "sumsq_x": p["sumsq_x"], "sumsq_y": p["sumsq_y"],
        "sum_xy": p["sum_xy"],
        "zero_fraction": p["zero_windows"] / sched if sched else None,
        "mean": mean,
        "fano": fano,
        "count_p90": weighted_nearest_rank(count_pairs, 0.90),
        "count_p99": weighted_nearest_rank(count_pairs, 0.99),
        "count_p999": weighted_nearest_rank(count_pairs, 0.999),
        "lag1_autocorr": lag1,
        "run_p90": weighted_nearest_rank(runs, 0.90) if runs else None,
    }


def aggregate_block3(sessions: list[dict]) -> dict:
    """One vote per qualifying session, median across sessions (spec
    3.5), over the serialized Block3Cell/Block3Pair records. Pair
    records qualify on the BIG horizon's floor over window_count.
    Non-qualifying sessions are skipped HERE (this is the descriptive
    monthly record); the ladder's all-session qualification (Q1) is
    enforced separately in the metric framework."""
    cells: dict[tuple, list] = {}
    pairs: dict[tuple, list] = {}
    lag1: dict[int, list] = {}
    h20: dict[tuple, list] = {}
    for rec in sessions:
        for hour_s, per_h in rec["cells"].items():
            for h_s, c in per_h.items():
                h = int(h_s)
                if c["return_count"] < horizon_floor(h):
                    continue
                cells.setdefault((int(hour_s), h), []).append(
                    (c["robust_scale"], c["rms_scale"],
                     c["return_count"])
                )
        for hour_s, per_pair in rec["pairs"].items():
            for pair_s, pc in per_pair.items():
                big = int(pair_s.split("-")[1])
                if pc["window_count"] < horizon_floor(big):
                    continue
                pairs.setdefault((int(hour_s), pair_s), []).append(
                    (pc["vr"], pc["cov_contrib"],
                     pc["cov_contrib_norm"], pc["window_count"])
                )
        for hour_s, v in rec["lag1_parent_autocorr"].items():
            lag1.setdefault(int(hour_s), []).append(v)
        for lp, per_h in rec["hour20_labels"].items():
            for h_s, c in per_h.items():
                h = int(h_s)
                floor = (MIN_BOUNDARY_60S_CELL_RETURNS
                         if h == 60 else horizon_floor(h))
                if c["return_count"] < floor:
                    continue
                h20.setdefault((lp, h), []).append((
                    c["robust_scale"], c["rms_scale"],
                    c["return_count"],
                ))
    return {
        "cells": {
            str(hour): {
                str(h): {
                    "return_count": sum(v[2] for v in votes),
                    "robust_scale": median_or_none(
                        [v[0] for v in votes]
                    ),
                    "rms_scale": median_or_none([v[1] for v in votes]),
                }
                for h in WALL_HORIZONS_S if (hour, h) in cells
                for votes in [cells[(hour, h)]]
            }
            for hour in sorted({hh for hh, _ in cells})
        },
        "pairs": {
            str(hour): {
                pair_s: {
                    "window_count": sum(v[3] for v in votes),
                    "vr": median_or_none([v[0] for v in votes]),
                    "cov_contrib": median_or_none(
                        [v[1] for v in votes]
                    ),
                    "cov_contrib_norm": median_or_none(
                        [v[2] for v in votes]
                    ),
                }
                for (hh, pair_s), votes in sorted(pairs.items())
                if hh == hour
            }
            for hour in sorted({hh for hh, _ in pairs})
        },
        "lag1_parent_autocorr": {
            str(hour): median_or_none(vals)
            for hour, vals in sorted(lag1.items())
        },
        "hour20_labels": {
            lp: {
                str(h): {
                    "return_count": sum(v[2] for v in votes),
                    "robust_scale": median_or_none(
                        [v[0] for v in votes]
                    ),
                    "rms_scale": median_or_none([v[1] for v in votes]),
                }
                for (lpp, h), votes in sorted(h20.items()) if lpp == lp
            }
            for lp in sorted({lpp for lpp, _ in h20})
        },
    }


def aggregate_block4(sessions: list[dict]) -> dict:
    """Per hour: median across qualifying sessions (residual_count at
    MIN_RESIDUAL_CELL) of each quantile/ratio/exceedance field."""
    fields = ("zero_fraction", "nz_abs_p90", "nz_abs_p99", "nz_abs_p999",
              "ratio_p99_p90", "ratio_p999_p99",
              "exceed_4", "exceed_8", "exceed_16")
    hours = sorted({h for rec in sessions for h in rec})
    out = {}
    for hour in hours:
        qualifying = [
            rec[str(hour)] for rec in sessions
            if str(hour) in rec
            and rec[str(hour)]["residual_count"] >= MIN_RESIDUAL_CELL
        ]
        total = sum(
            rec[str(hour)]["residual_count"] for rec in sessions
            if str(hour) in rec
        )
        warm = sum(
            rec[str(hour)]["warmup_excluded"] for rec in sessions
            if str(hour) in rec
        )
        out[str(hour)] = {
            "residual_count": total,
            "warmup_excluded": warm,
            **{
                f: median_or_none([q[f] for q in qualifying])
                for f in fields
            },
        }
    return out


def aggregate_permutations(per_session: list[list[dict]]) -> dict:
    """Per (variant, hour): the Amendment-A session-hour combination
    (segment records pooled by count/sum_abs/max_abs, combined-count
    floor), then median across qualifying sessions per replicate
    index, then median across the 16 replicates (spec 3.5)."""
    by_key: dict[tuple, dict[int, list]] = {}
    for records in per_session:
        combined: dict[tuple, list] = {}
        for rec in records:
            for h in (60, 300):
                key = (rec["variant"], rec["hour"], h,
                       rec["replicate"])
                acc = combined.setdefault(key, [0, 0.0, 0.0])
                acc[0] += rec[f"return_count_{h}"]
                acc[1] += rec[f"sum_abs_{h}"]
                if rec[f"max_abs_{h}"] > acc[2]:
                    acc[2] = rec[f"max_abs_{h}"]
        for (variant, hour, h, rep), acc in combined.items():
            if acc[0] < horizon_floor(h):
                continue
            by_key.setdefault((variant, hour, h), {}).setdefault(
                rep, []
            ).append(_robust_from_stats(acc[0], acc[1], acc[2]))
    out: dict[str, dict] = {}
    for variant in PERMUTATION_VARIANTS:
        vout: dict[str, dict] = {}
        hours = sorted({k[1] for k in by_key if k[0] == variant})
        for hour in hours:
            entry = {}
            for h in (60, 300):
                reps = by_key.get((variant, hour, h), {})
                rep_medians = [
                    median_or_none(vals) for _rep, vals in sorted(
                        reps.items()
                    )
                ]
                entry[f"robust_scale_{h}"] = median_or_none(rep_medians)
            vout[str(hour)] = entry
        out[variant] = vout
    return out


# --- 12a bootstrap (spec 6.1): fixed-seed circular moving-block ------------


def bootstrap_multiplicities(n_sessions: int) -> list[list[int]]:
    """Per replicate, the session-index multiplicity vector of one
    pseudo-month: sessions sorted ascending, exactly five circular block
    starts of five consecutive sessions each, concatenated in draw order
    and truncated to n_sessions; block start = splitmix64(BASE ^
    (replicate << 8) ^ block) mod n_sessions."""
    out = []
    for rep in range(BOOTSTRAP_REPLICATES):
        mult = [0] * n_sessions
        drawn = 0
        for block in range(5):
            start = splitmix64(
                (BOOTSTRAP_BASE_SEED ^ (rep << 8) ^ block)
                & 0xFFFF_FFFF_FFFF_FFFF
            ) % n_sessions
            for k in range(BOOTSTRAP_BLOCK_SESSIONS):
                if drawn >= n_sessions:
                    break
                mult[(start + k) % n_sessions] += 1
                drawn += 1
        out.append(mult)
    return out


def weighted_median_votes(values: list, mult: list[int]):
    """Nearest-rank median of the session votes under the replicate's
    multiplicities. `values[i]` may be None (non-qualifying session:
    contributes nothing)."""
    pairs = sorted(
        (v, mult[i]) for i, v in enumerate(values)
        if v is not None and mult[i] > 0
    )
    total = sum(w for _v, w in pairs)
    if total == 0:
        return None
    # Nearest-rank median = ceil(total/2)-th order statistic.
    target = (total + 1) // 2
    cum = 0
    for v, w in pairs:
        cum += w
        if cum >= target:
            return v
    return pairs[-1][0]


class QuantileSupport:
    """Per-session cumulative counts over a shared sorted support, for
    O(sessions * log support) pooled quantiles under resampling."""

    def __init__(self, per_session_pairs: list[list[tuple]]):
        support = sorted({
            v for pairs in per_session_pairs for v, _w in pairs
        })
        self.support = support
        index = {v: i for i, v in enumerate(support)}
        self.cum = []
        self.totals = []
        for pairs in per_session_pairs:
            arr = [0] * len(support)
            for v, w in pairs:
                arr[index[v]] += w
            for i in range(1, len(support)):
                arr[i] += arr[i - 1]
            self.cum.append(arr)
            self.totals.append(arr[-1] if support else 0)

    def quantile(self, q: float, mult: list[int]):
        if not self.support:
            return None
        total = sum(
            m * t for m, t in zip(mult, self.totals)
        )
        if total <= 0:
            return None
        target = q * total
        lo, hi = 0, len(self.support) - 1
        while lo < hi:
            mid = (lo + hi) // 2
            cum = sum(
                m * arr[mid] for m, arr in zip(mult, self.cum) if m
            )
            if cum >= target:
                hi = mid
            else:
                lo = mid + 1
        return self.support[lo]


def nearest_rank_p(sorted_vals: list, q: float):
    """Nearest-rank quantile over an already sorted list."""
    if not sorted_vals:
        return None
    rank = math.ceil(q * len(sorted_vals))
    rank = min(max(rank, 1), len(sorted_vals))
    return sorted_vals[rank - 1]


def measure12a_observe(rows_iter, usable: list[str]) -> list[dict]:
    """The observed half of the 12a artifact: one chronological pass,
    one session retained at a time."""
    if not all(isinstance(d, str) for d in usable):
        # A mixed-type usable list would raise a raw TypeError from the
        # final sorted() comparison; refuse it by name up front.
        raise Refusal(
            f"the usable session list carries non-string entries: "
            f"{usable!r}"
        )
    usable_set = set(usable)
    records: list[dict] = []
    state: _M12aSession | None = None
    current = None

    def close_parent(parent):
        seg = state.seg(parent["segment"])
        seg["parent_ts"].append(parent["first_ts"])
        minute = parent["first_ts"] // 60_000_000_000
        state.n_min[minute] = state.n_min.get(minute, 0) + 1
        if parent["book"] == "normal":
            mid2 = (parent["bid_px"] + parent["ask_px"]) // TICK_UNITS
            qm = state.quote_min.get(minute)
            if qm is None:
                state.quote_min[minute] = [mid2, mid2]
            else:
                if mid2 < qm[0]:
                    qm[0] = mid2
                if mid2 > qm[1]:
                    qm[1] = mid2
            mid_units = (parent["bid_px"] + parent["ask_px"]) / 2
            seg["mid_ts"].append(parent["first_ts"])
            seg["mid_log"].append(math.log(mid_units))

    for row in rows_iter:
        session, segment, _hour = minute_fields(row.ts)
        if session not in usable_set:
            if current is not None:
                close_parent(current)
                current = None
            continue
        if state is None or state.session != session:
            if current is not None:
                close_parent(current)
                current = None
            if state is not None:
                records.append(m12a_close_session(state))
            state = _M12aSession(session)
        minute = row.ts // 60_000_000_000
        tm = state.trade_min.get(minute)
        if tm is None:
            state.trade_min[minute] = [row.price, row.price]
        else:
            if row.price < tm[0]:
                tm[0] = row.price
            if row.price > tm[1]:
                tm[1] = row.price
        if row.side == "N":
            if current is not None:
                close_parent(current)
                current = None
            continue
        if current is not None and (
            current["ts"] == row.ts and current["side"] == row.side
        ):
            current["rows"] += 1
        else:
            if current is not None:
                close_parent(current)
            current = {
                "ts": row.ts, "side": row.side, "segment": segment,
                "first_ts": row.ts, "rows": 1, "book": row.book,
                "bid_px": row.bid_px, "ask_px": row.ask_px,
            }
    if current is not None:
        close_parent(current)
    if state is not None:
        records.append(m12a_close_session(state))
    if [r["session_date"] for r in records] != sorted(usable):
        raise Refusal(
            "measure12a session records do not match the usable set: "
            f"{[r['session_date'] for r in records]} vs {sorted(usable)}"
        )
    return records


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
    # Scratch paths key on the walk's cache hash PLUS the invoking thread:
    # the hash keeps distinct walks disjoint (the old per-PID scheme raced
    # under parallelism), and the thread id keeps DUPLICATE submissions of
    # one walk disjoint too - the prewarm dedupes those, but a defense
    # relying on every caller deduping is no defense. Duplicate walks then
    # converge on one cache entry through the atomic write below.
    invocation = f"{cache_key[:16]}-{threading.get_ident()}"
    config_path = os.path.join(SCRATCH_DIR, f"candidate-{invocation}.toml")
    out_path = os.path.join(SCRATCH_DIR, f"summary-{invocation}.json")
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


MEASURE12A_CACHE_DIR = os.path.join(ROOT, "analysis", "out",
                                    "measure12a-cache")


def run_measure12a_subprocess(seed: int) -> dict:
    """One FINAL `gen --type measure12a` walk into the DISTINCT
    measure12a cache (spec 2.4): keyed by the full command, the harness
    commit and the measurement-subcontract hash, never shared with the
    protocol-11 summary cache. The committed MNQ preset drives the walk -
    no overrides."""
    warm_gen_build()
    cmd_tail = [
        "gen", "--symbol", "MNQ", "--type", "measure12a",
        "--seed", str(seed), "--start", str(FINAL_START_NS),
        "--length", FINAL_LENGTH, "--warmup", SUMMARY_WARMUP,
    ]
    cache_key = hashlib.sha256(json.dumps(
        {"cmd": cmd_tail, "commit": git_commit(),
         "subcontract": subcontract_hash()},
        sort_keys=True,
    ).encode()).hexdigest()
    os.makedirs(MEASURE12A_CACHE_DIR, exist_ok=True)
    cache_path = os.path.join(MEASURE12A_CACHE_DIR, cache_key + ".json")
    if os.path.exists(cache_path):
        with open(cache_path) as fh:
            return json.load(fh)
    out_path = os.path.join(
        MEASURE12A_CACHE_DIR, f"walk-{seed}-{cache_key[:16]}.tmp.json"
    )
    if os.path.exists(out_path):
        os.remove(out_path)
    cmd = gen_command_prefix() + cmd_tail + ["--out", out_path]
    proc = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT)
    if proc.returncode != 0:
        raise Refusal(
            f"measure12a walk failed ({' '.join(cmd)}):\n"
            f"{proc.stderr[-2000:]}"
        )
    with open(out_path) as fh:
        record = json.load(fh)
    tmp = f"{cache_path}.{os.getpid()}.tmp"
    with open(tmp, "w") as fh:
        json.dump(record, fh)
    os.replace(tmp, cache_path)
    os.remove(out_path)
    return record


def _tree_rss_bytes(pid: int) -> int:
    """VmRSS summed over the process and its live descendants, one
    sample. Vanished processes read as zero (the sampler races exits)."""
    total = 0
    stack = [pid]
    while stack:
        p = stack.pop()
        try:
            with open(f"/proc/{p}/status") as fh:
                for line in fh:
                    if line.startswith("VmRSS:"):
                        total += int(line.split()[1]) * 1024
                        break
            with open(f"/proc/{p}/task/{p}/children") as fh:
                stack.extend(int(c) for c in fh.read().split())
        except (FileNotFoundError, ProcessLookupError, ValueError):
            continue
    return total


def _timed_probe(cmd: list[str]) -> tuple[float, int]:
    """(wall seconds, peak process-tree RSS bytes at 1 s sampling)."""
    t0 = time.monotonic()
    proc = subprocess.Popen(
        cmd, cwd=ROOT,
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True,
    )
    peak = 0
    while proc.poll() is None:
        peak = max(peak, _tree_rss_bytes(proc.pid))
        time.sleep(1.0)
    runtime = time.monotonic() - t0
    if proc.returncode != 0:
        stderr = proc.stderr.read() if proc.stderr else ""
        raise Refusal(
            f"cost probe failed ({' '.join(cmd)}):\n{stderr[-2000:]}"
        )
    return runtime, peak


def mode_cost12a() -> None:
    """The Brick G cost probe (spec section 9): `--type summary` then
    `--type measure12a` sequentially, same release binary, seed 1, same
    anchor, warmup and 7-day window. Enforces measure12a runtime at most
    1.5x the summary runtime and peak process-tree RSS at most 1 GiB;
    a breach fails the brick for accumulator redesign, never a relaxed
    budget."""
    warm_gen_build()
    os.makedirs(SCRATCH_DIR, exist_ok=True)
    results = {}
    for kind in ("summary", "measure12a"):
        out_path = os.path.join(SCRATCH_DIR, f"cost12a-{kind}.json")
        if os.path.exists(out_path):
            os.remove(out_path)
        cmd = gen_command_prefix() + [
            "gen", "--symbol", "MNQ", "--type", kind,
            "--seed", "1", "--start", str(FINAL_START_NS),
            "--length", "7d", "--warmup", SUMMARY_WARMUP,
            "--out", out_path,
        ]
        runtime, peak = _timed_probe(cmd)
        os.remove(out_path)
        results[kind] = {"runtime_s": runtime, "peak_rss_bytes": peak}
    ratio = (results["measure12a"]["runtime_s"]
             / results["summary"]["runtime_s"])
    verdict = {
        "summary": results["summary"],
        "measure12a": results["measure12a"],
        "runtime_ratio": ratio,
        "runtime_bound": 1.5,
        "rss_bound_bytes": 1 << 30,
    }
    print(json.dumps(verdict, indent=2))
    if ratio > 1.5:
        raise Refusal(
            f"cost12a: measure12a runtime ratio {ratio:.3f} exceeds 1.5 "
            f"- the brick stops for accumulator redesign"
        )
    if results["measure12a"]["peak_rss_bytes"] > (1 << 30):
        raise Refusal(
            "cost12a: measure12a peak RSS "
            f"{results['measure12a']['peak_rss_bytes']} exceeds 1 GiB "
            "- the brick stops for accumulator redesign"
        )
    print("cost12a PASS")


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
    # Dedupe identical walks BEFORE fan-out: the protocol-11 probe sets
    # deliberately share one override set (spec 4.5, shared cached FINAL
    # walks), and submitting the same (overrides, seed) twice would race
    # two subprocesses onto one cache entry for no work saved.
    unique: dict[str, tuple] = {}
    for overrides in override_sets:
        for seed in seeds:
            key = json.dumps({"overrides": overrides, "seed": seed},
                             sort_keys=True, default=list)
            unique.setdefault(key, (overrides, seed))
    with ThreadPoolExecutor(WALK_JOBS) as pool:
        futures = [
            pool.submit(run_summary, overrides, seed, start_ns, length,
                        SUMMARY_WARMUP)
            for overrides, seed in unique.values()
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

    def stage_checks(family: str, names) -> bool:
        probe = probe_results.get(family)
        combined = combined_results.get(family)
        return (combined_error is None
                and probe is not None and combined is not None
                and all(probe["checks"].get(n) is True for n in names)
                and all(combined["checks"].get(n) is True for n in names))

    # The Brick V amendment: the wall-time family splits by role. The
    # pooled gates land; the hourly contour is RECORDED (protocol 12
    # inherits it as a hard successor gate) and never gates protocol 11.
    walltime_pooled_ok = stage_checks(
        "session_walltime", ("walltime_pooled_60", "walltime_pooled_300")
    )
    walltime_hourly_ok = stage_checks(
        "session_walltime", ("walltime_hour_60", "walltime_hour_300")
    )
    session_ok = (
        family_ok("session_arrival")
        and family_ok("session_parent_vol")
        and walltime_pooled_ok
    )
    cadence_ok = stage_checks("base_volatility", cadence_names)
    pooled_rms_ok = stage_checks("base_volatility", ("mid_rms",))
    envelope_ok = stage_checks(
        "base_volatility",
        tuple(f"minute_range_{stat}" for stat in MINUTE_RANGE_GATES),
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
            return ("a session landing gate failed; the atomic group does "
                    "not land")
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
            "walltime_pooled_ok": walltime_pooled_ok,
            "walltime_hourly_ok": walltime_hourly_ok,
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


# --- 12a count substitution (spec 5.2) -------------------------------------


def observed_bin_shares(pooled: dict[tuple, int]) -> dict:
    """o[h][b]: the observed populated-minute share of parent-count bin b
    within hour h, from the pooled sparse histogram."""
    counts: dict[int, dict[str, int]] = {}
    for k, c in pooled.items():
        h = k[3]
        b = parent_count_bin(k[0])
        counts.setdefault(h, {})[b] = counts.get(h, {}).get(b, 0) + c
    return {
        h: {
            b: n / total for b, n in per_bin.items()
            for total in [sum(per_bin.values())]
        }
        for h, per_bin in counts.items()
    }


def count_substitution(gen_hist: dict[tuple, int],
                       obs_shares: dict) -> dict:
    """One seed's counterfactual (spec 5.2): reweight the generated
    populated minutes so each hour's parent-count-bin frequencies match
    the observed shares, preserving the hour's total weight, then ONE
    full-month weighted-nearest-rank minute-range p99.9 and > 968
    exceedance rate. Edge cases frozen: o>0,g>0 -> o/g; o=0,g>0 -> 0;
    o=0,g=0 -> null, bin ignored; o>0,g=0 -> support refusal, the hour
    fails."""
    gen_counts: dict[int, dict[str, int]] = {}
    for k, c in gen_hist.items():
        h = k[3]
        b = parent_count_bin(k[0])
        per = gen_counts.setdefault(h, {})
        per[b] = per.get(b, 0) + c
    weights: dict[int, dict[str, float | None]] = {}
    refused_hours: list[int] = []
    support_refusals: list[dict] = []
    for h in sorted(set(gen_counts) | set(obs_shares)):
        g_per = gen_counts.get(h, {})
        g_total = sum(g_per.values())
        o_per = obs_shares.get(h, {})
        w_per: dict[str, float | None] = {}
        raw: dict[str, float] = {}
        refused = False
        for b in PARENT_COUNT_BIN_NAMES:
            o = o_per.get(b, 0.0)
            g = g_per.get(b, 0) / g_total if g_total else 0.0
            if o > 0 and g > 0:
                raw[b] = o / g
            elif o == 0 and g > 0:
                raw[b] = 0.0
            elif o == 0 and g == 0:
                w_per[b] = None  # bin ignored
            else:  # o > 0, g == 0: support refusal
                refused = True
                support_refusals.append({
                    "scope": "count_substitution",
                    "cell": f"hour {h} bin {b}",
                    "reason": "observed support with zero generated "
                              "support",
                })
        if refused or not g_total:
            if g_total or o_per:
                # A generated-supported hour with a refusing bin, or
                # an observed hour with NO generated support at all:
                # both refuse the hour (spec 5.2 union rule).
                refused_hours.append(h)
            weights[h] = {b: None for b in PARENT_COUNT_BIN_NAMES}
            continue
        # Preserve the hour's original generated total weight.
        wsum = sum(
            raw.get(b, 0.0) * g_per.get(b, 0) for b in raw
        )
        scale = g_total / wsum if wsum > 0 else None
        for b in raw:
            w_per[b] = raw[b] * scale if scale is not None else None
        if scale is None:
            refused_hours.append(h)
            w_per = {b: None for b in PARENT_COUNT_BIN_NAMES}
        weights[h] = {
            b: w_per.get(b) for b in PARENT_COUNT_BIN_NAMES
        }
    # Pool all weighted hours, preserving the generated hour mixture.
    pairs: list[tuple] = []
    total_w = 0.0
    exceed_w = 0.0
    for k, c in gen_hist.items():
        h = k[3]
        if h in refused_hours:
            continue
        b = parent_count_bin(k[0])
        w = weights.get(h, {}).get(b)
        if w is None:
            continue
        wt = w * c
        pairs.append((k[2], wt))
        total_w += wt
        if k[2] > 968:
            exceed_w += wt
    cf_p999 = weighted_nearest_rank(pairs, 0.999)
    return {
        "shares_observed": {
            str(h): {b: obs_shares.get(h, {}).get(b, 0.0)
                     for b in PARENT_COUNT_BIN_NAMES}
            for h in sorted(obs_shares)
        },
        "shares_generated": {
            str(h): {
                b: (gen_counts[h].get(b, 0) / total
                    if (total := sum(gen_counts[h].values())) else 0.0)
                for b in PARENT_COUNT_BIN_NAMES
            }
            for h in sorted(gen_counts)
        },
        "weights": {
            str(h): {b: w for b, w in per.items()}
            for h, per in sorted(weights.items())
        },
        "refused_hours": sorted(refused_hours),
        "support_refusals": support_refusals,
        "counterfactual_p999": cf_p999,
        "counterfactual_exceed_968": (
            exceed_w / total_w if total_w > 0 else None
        ),
    }


def gap_closure(t_gen, t_cf, t_obs, generated_side: bool):
    """Spec 5.3. Refused (None) on nonpositive inputs or a denominator
    below GAP_CLOSE_EPS."""
    if any(v is None or v <= 0 for v in (t_gen, t_cf, t_obs)):
        return None
    denom = math.log(t_gen) - math.log(t_obs)
    if abs(denom) < GAP_CLOSE_EPS:
        return None
    if generated_side:
        return (math.log(t_gen) - math.log(t_cf)) / denom
    return (math.log(t_cf) - math.log(t_obs)) / denom


# --- 12a metric framework (spec 6.1): observed evaluators, envelopes -------


LOG_BAND = (math.log(MATERIALITY_BAND[0]), math.log(MATERIALITY_BAND[1]))


def fold_multiplicities(sessions: list[str]) -> list[list[int]]:
    """Leave-one-ISO-week-out 0/1 vectors; a fold qualifies when at
    least FOLD_MIN_SESSIONS sessions remain; partial weeks are their own
    folds."""
    weeks: dict[tuple, list[int]] = {}
    for i, label in enumerate(sessions):
        iso = dt.date.fromisoformat(label).isocalendar()
        weeks.setdefault((iso[0], iso[1]), []).append(i)
    folds = []
    for _wk, idxs in sorted(weeks.items()):
        if len(sessions) - len(idxs) >= FOLD_MIN_SESSIONS:
            mult = [1] * len(sessions)
            for i in idxs:
                mult[i] = 0
            folds.append(mult)
    return folds


class ObsContext:
    """Observed-side metric evaluators over the per-session records:
    every statistic is a function of a session-multiplicity vector, so
    the point estimate (all ones), the bootstrap replicates, and the
    leave-one-week folds all run the same code path."""

    def __init__(self, per_session: list[dict]):
        self.per_session = per_session
        self.sessions = [r["session_date"] for r in per_session]
        self.n = len(per_session)
        self._cache: dict = {}

    def ones(self) -> list[int]:
        return [1] * self.n

    # -- Block 1 quantiles ---------------------------------------------
    def b1_support(self, field: str, hour=None, labels=None,
                   bin_name=None) -> QuantileSupport:
        key = ("b1", field, hour, labels, bin_name)
        hit = self._cache.get(key)
        if hit is not None:
            return hit
        per = []
        for rec in self.per_session:
            pairs = []
            for row in rec["block1_hist"]:
                if hour is not None and row["hour"] != hour:
                    continue
                if labels is not None and (
                    row["since_open_bin"], row["until_close_bin"]
                ) != labels:
                    continue
                if bin_name is not None and \
                        parent_count_bin(row["n"]) != bin_name:
                    continue
                if field == "trade":
                    pairs.append((row["trade_range_ticks"], row["count"]))
                elif field == "quote":
                    if row["quote_range_half_ticks"] is not None:
                        pairs.append((row["quote_range_half_ticks"],
                                      row["count"]))
                elif field == "sqrtn":
                    if row["n"] >= 1:
                        pairs.append((
                            row["trade_range_ticks"]
                            / math.sqrt(row["n"]), row["count"],
                        ))
                else:
                    raise AssertionError(field)
            per.append(pairs)
        hit = QuantileSupport(per)
        self._cache[key] = hit
        return hit

    def b1_bin_count(self, hour: int, bin_name: str,
                     mult: list[int]) -> int:
        key = ("b1count", hour, bin_name)
        counts = self._cache.get(key)
        if counts is None:
            counts = []
            for rec in self.per_session:
                c = sum(
                    row["count"] for row in rec["block1_hist"]
                    if row["hour"] == hour
                    and parent_count_bin(row["n"]) == bin_name
                )
                counts.append(c)
            self._cache[key] = counts
        return sum(m * c for m, c in zip(mult, counts))

    # -- Block 2 -------------------------------------------------------
    def _b2_cells(self, hour: int, w: int) -> list:
        key = ("b2", hour, w)
        cells = self._cache.get(key)
        if cells is None:
            cells = []
            for rec in self.per_session:
                c = rec["block2"].get(str(hour), {}).get(str(w))
                if c is None:
                    cells.append(None)
                    continue
                hist = {int(k): v for k, v in c["count_hist"].items()}
                total = sum(hist.values())
                s = sum(k * v for k, v in hist.items())
                sq = sum(k * k * v for k, v in hist.items())
                cells.append((total, s, sq))
            self._cache[key] = cells
            self._cache[("b2q", hour, w)] = QuantileSupport([
                ([] if rec["block2"].get(str(hour), {}).get(str(w))
                 is None else [
                    (int(k), v) for k, v in rec["block2"][str(hour)][
                        str(w)]["count_hist"].items()
                ])
                for rec in self.per_session
            ])
        return cells

    def b2_fano(self, hour: int, w: int, mult: list[int]):
        cells = self._b2_cells(hour, w)
        total = s = sq = 0
        for m, c in zip(mult, cells):
            if c is None or m == 0:
                continue
            total += m * c[0]
            s += m * c[1]
            sq += m * c[2]
        if total == 0:
            return None
        mean = s / total
        if mean <= 0:
            return None
        var = sq / total - mean * mean
        return var / mean

    def b2_count_quantile(self, hour: int, w: int, q: float,
                          mult: list[int]):
        self._b2_cells(hour, w)
        return self._cache[("b2q", hour, w)].quantile(q, mult)

    # -- Block 3 session votes -----------------------------------------
    def b3_votes(self, hour: int, h: int, stat: str) -> list:
        key = ("b3", hour, h, stat)
        votes = self._cache.get(key)
        if votes is None:
            field = "robust_scale" if stat == "robust" else "rms_scale"
            votes = []
            for rec in self.per_session:
                c = rec["block3"]["cells"].get(str(hour), {}).get(str(h))
                if c is None or c["return_count"] < horizon_floor(h):
                    votes.append(None)
                else:
                    votes.append(c[field])
            self._cache[key] = votes
        return votes

    def b3_cov_votes(self, hour: int, pair: str) -> list:
        key = ("b3cov", hour, pair)
        votes = self._cache.get(key)
        if votes is None:
            big = int(pair.split("-")[1])
            votes = []
            for rec in self.per_session:
                pc = rec["block3"]["pairs"].get(str(hour), {}).get(pair)
                if pc is None or pc["window_count"] < horizon_floor(big):
                    votes.append(None)
                else:
                    votes.append(pc["cov_contrib_norm"])
            self._cache[key] = votes
        return votes

    def b3_boundary_votes(self, label_pair: str, h: int,
                          stat: str) -> list:
        key = ("b3h20", label_pair, h, stat)
        votes = self._cache.get(key)
        if votes is None:
            floor = (MIN_BOUNDARY_60S_CELL_RETURNS if h == 60
                     else horizon_floor(h))
            field = "robust_scale" if stat == "robust" else "rms_scale"
            votes = []
            for rec in self.per_session:
                c = rec["block3"]["hour20_labels"].get(
                    label_pair, {}
                ).get(str(h))
                if c is None or c["return_count"] < floor:
                    votes.append(None)
                else:
                    votes.append(c[field])
            self._cache[key] = votes
        return votes

    # -- Block 4 session votes -----------------------------------------
    def b4_votes(self, hour_key: str, field: str) -> list:
        key = ("b4", hour_key, field)
        votes = self._cache.get(key)
        if votes is None:
            votes = []
            for rec in self.per_session:
                c = rec["block4"].get(hour_key)
                if c is None or \
                        c["residual_count"] < MIN_RESIDUAL_CELL:
                    votes.append(None)
                else:
                    votes.append(c[field])
            self._cache[key] = votes
        return votes

    # -- Q1 qualification helpers --------------------------------------
    def minute_counts(self, hour=None, labels=None) -> list[int]:
        key = ("mcount", hour, labels)
        counts = self._cache.get(key)
        if counts is None:
            counts = []
            for rec in self.per_session:
                c = sum(
                    row["count"] for row in rec["block1_hist"]
                    if (hour is None or row["hour"] == hour)
                    and (labels is None or (
                        row["since_open_bin"], row["until_close_bin"]
                    ) == labels)
                )
                counts.append(c)
            self._cache[key] = counts
        return counts

    def b2_scheduled(self, hour: int, w: int) -> list:
        return [
            (rec["block2"].get(str(hour), {}).get(str(w)) or {}).get(
                "scheduled_windows"
            )
            for rec in self.per_session
        ]

    # -- Permutation session values ------------------------------------
    def perm_votes(self, variant: str, hour: int, h: int,
                   rep: int) -> list:
        """One session-hour robust scale per session (Amendment A):
        segment sufficient records combined by count sum / sum_abs sum
        / max_abs max, refused below the horizon floor on the COMBINED
        count."""
        key = ("perm", variant, hour, h, rep)
        votes = self._cache.get(key)
        if votes is None:
            votes = []
            for rec in self.per_session:
                cnt = 0
                s_abs = 0.0
                m_abs = 0.0
                for p in rec["permutations"]:
                    if p["variant"] != variant or p["hour"] != hour \
                            or p["replicate"] != rep:
                        continue
                    cnt += p[f"return_count_{h}"]
                    s_abs += p[f"sum_abs_{h}"]
                    if p[f"max_abs_{h}"] > m_abs:
                        m_abs = p[f"max_abs_{h}"]
                if cnt < horizon_floor(h):
                    votes.append(None)
                else:
                    votes.append(_robust_from_stats(cnt, s_abs, m_abs))
            self._cache[key] = votes
        return votes

    def perm_value(self, variant: str, hour: int, h: int,
                   mult: list[int]):
        """The frozen rule: the pseudo-month is evaluated under all 16
        replicate indices; its counterfactual statistic is their
        median. Q1 strictness (no K-of-N): a missing or non-finite
        session vote in ANY replicate - regardless of the multiplicity
        vector - refuses the whole statistic, as does a missing
        per-replicate median."""
        per_rep = []
        for rep in range(PERMUTATION_REPLICATES):
            votes = self.perm_votes(variant, hour, h, rep)
            if any(v is None or not math.isfinite(v) for v in votes):
                return None
            med = weighted_median_votes(votes, mult)
            if med is None or not math.isfinite(med):
                return None
            per_rep.append(med)
        return median_or_none(per_rep)

    def b3_robust_strict(self, hour: int, h: int, mult: list[int]):
        """The all-session-strict robust scale the closure and
        worsening statistics consume: any missing or non-finite
        session vote refuses the statistic outright, regardless of
        the bootstrap or fold multiplicities."""
        votes = self.b3_votes(hour, h, "robust")
        if any(v is None or not math.isfinite(v) for v in votes):
            return None
        return weighted_median_votes(votes, mult)


def stdev_ddof1(values: list[float]):
    vals = [v for v in values if v is not None and math.isfinite(v)]
    if len(vals) < 2:
        return None
    mean = sum(vals) / len(vals)
    var = sum((v - mean) ** 2 for v in vals) / (len(vals) - 1)
    return math.sqrt(var)


def evaluate_family(family: str, metrics: list[dict],
                    mults: list[list[int]],
                    folds: list[list[int]], ones: list[int]) -> dict:
    """One family's simultaneous envelope (spec 6.1, Amendment D and
    the Q1 all-session rule). Each metric dict carries: name, kind
    (log_ratio | raw_diff), predicate (outside | inside |
    raw_direction), obs_fn(mult), gen_central, gen_seeds, and
    optionally qualify_refusals (pre-computed Q1 refusal strings) or
    force_refused (a required metric present but unsupported).

    Returns {metrics: [MetricRec], critical_value | None,
    inventory_complete, refusals: [RefusalRec]}."""
    refusal_recs: list[dict] = []

    def refuse(name: str, reason: str) -> None:
        refusal_recs.append({
            "scope": f"family:{family}",
            "cell": name,
            "reason": reason,
        })

    prepared = []
    for m in metrics:
        # Every cause for one refused metric aggregates into exactly
        # ONE RefusalRec (spec section 10 ownership): force_refused,
        # the deterministic observed-seed-session qualification lines,
        # point-input failure, bootstrap failure, SE failure.
        reasons: list[str] = []
        t_obs = None
        point = None
        reps = None
        se = None
        if m.get("force_refused"):
            reasons.append(m["force_refused"])
        reasons.extend(m.get("qualify_refusals", ()))
        if not reasons:
            t_obs = m["obs_fn"](ones)
            g = m["gen_central"]
            if m["kind"] == "log_ratio":
                if t_obs is None or t_obs <= 0 or g is None or g <= 0:
                    reasons.append(
                        "nonpositive or missing point inputs")
                else:
                    point = math.log(g / t_obs)
            else:
                if t_obs is None or g is None:
                    reasons.append("missing point inputs")
                else:
                    point = g - t_obs
        if not reasons:
            reps = []
            for mult in mults:
                tb = m["obs_fn"](mult)
                if m["kind"] == "log_ratio":
                    reps.append(
                        math.log(m["gen_central"] / tb)
                        if tb is not None and tb > 0 else None
                    )
                else:
                    reps.append(
                        m["gen_central"] - tb
                        if tb is not None else None
                    )
            # A missing or non-finite replicate refuses the metric -
            # never a silent omission from the SE population.
            if any(r is None or not math.isfinite(r) for r in reps):
                reasons.append(
                    "missing or non-finite bootstrap replicate")
            else:
                se = stdev_ddof1(reps)
                if se is None or se == 0 or not math.isfinite(se):
                    reasons.append("zero or non-finite bootstrap SE")
                    se = None
        refused = bool(reasons)
        if refused:
            refuse(m["name"], "; ".join(reasons))
        prepared.append({**m, "t_obs": t_obs, "point": point,
                         "reps": reps, "se": se, "refused": refused})

    inventory_complete = all(not p["refused"] for p in prepared)
    critical = None
    if inventory_complete and prepared:
        maxima = []
        for i in range(len(mults)):
            worst = max(
                abs(p["reps"][i] - p["point"]) / p["se"]
                for p in prepared
            )
            maxima.append(worst)
        maxima.sort()
        critical = nearest_rank_p(maxima, FAMILY_ENVELOPE_LEVEL)
        if critical is None or not math.isfinite(critical):
            inventory_complete = False
            critical = None
    if not inventory_complete:
        # Exactly one family-envelope refusal owns the envelope-only
        # nulls on otherwise computable metrics (Amendment D).
        refuse("envelope", "incomplete metric inventory - no "
                           "simultaneous critical value")

    records = []
    for p in prepared:
        rec = {
            "name": p["name"], "kind": p["kind"],
            "predicate": p["predicate"],
            "point": None, "se": None,
            "interval_low": None, "interval_high": None,
            "band_low": None, "band_high": None,
            "outside_band": None, "envelope_excludes_edge": None,
            "interval_inside_band": None,
            "seed_same_side_count": None, "seed_inside_count": None,
            "seed_rule_pass": None, "fold_rule_pass": None,
            "refused": p["refused"],
        }
        if p["refused"]:
            records.append(rec)
            continue
        point = p["point"]
        if critical is not None:
            half = critical * p["se"]
            lo, hi = point - half, point + half
        else:
            # Computable metric in an incomplete family: point, SE,
            # band, point-only predicate, seed and fold evidence stay;
            # the envelope fields are null (Amendment D).
            lo = hi = None
        rec.update({"point": point, "se": p["se"],
                    "interval_low": lo, "interval_high": hi})
        band = LOG_BAND if p["kind"] == "log_ratio" else None
        seeds = [
            s for s in p["gen_seeds"] if s is not None
        ]

        def seed_metric(s):
            if p["kind"] == "log_ratio":
                return (math.log(s / p["t_obs"])
                        if s > 0 and p["t_obs"] and p["t_obs"] > 0
                        else None)
            return s - p["t_obs"] if p["t_obs"] is not None else None

        seed_points = [seed_metric(s) for s in seeds]
        seed_points = [s for s in seed_points if s is not None]

        def fold_points():
            out = []
            for f in folds:
                tf = p["obs_fn"](f)
                if p["kind"] == "log_ratio":
                    out.append(
                        math.log(p["gen_central"] / tf)
                        if tf is not None and tf > 0 else None
                    )
                else:
                    out.append(
                        p["gen_central"] - tf
                        if tf is not None else None
                    )
            return out

        if p["predicate"] == "outside":
            rec["band_low"], rec["band_high"] = band
            below = point < band[0]
            above = point > band[1]
            rec["outside_band"] = below or above
            rec["envelope_excludes_edge"] = (
                (below and hi < band[0]) or (above and lo > band[1])
            ) if lo is not None else None
            same_side = sum(
                1 for s in seed_points
                if (s < band[0] and below) or (s > band[1] and above)
            )
            rec["seed_same_side_count"] = same_side
            rec["seed_rule_pass"] = same_side >= SEED_DIRECTION_MIN
            fp = fold_points()
            rec["fold_rule_pass"] = bool(fp) and all(
                v is not None and (
                    (below and v < band[0]) or (above and v > band[1])
                )
                for v in fp
            )
        elif p["predicate"] == "inside":
            rec["band_low"], rec["band_high"] = band
            inside = band[0] <= point <= band[1]
            rec["interval_inside_band"] = (
                inside and band[0] <= lo and hi <= band[1]
            ) if lo is not None else None
            cnt = sum(
                1 for s in seed_points if band[0] <= s <= band[1]
            )
            rec["seed_inside_count"] = cnt
            rec["seed_rule_pass"] = cnt >= SEED_DIRECTION_MIN
            fp = fold_points()
            rec["fold_rule_pass"] = bool(fp) and all(
                v is not None and band[0] <= v <= band[1] for v in fp
            )
        else:  # raw_direction
            claimed = 1 if point > 0 else -1 if point < 0 else 0
            rec["outside_band"] = claimed != 0
            rec["envelope_excludes_edge"] = (
                (claimed > 0 and lo > 0) or (claimed < 0 and hi < 0)
            ) if lo is not None else None
            same = sum(
                1 for s in seed_points
                if (claimed > 0 and s > 0) or (claimed < 0 and s < 0)
            )
            rec["seed_same_side_count"] = same
            rec["seed_rule_pass"] = same >= SEED_DIRECTION_MIN
            fp = fold_points()
            rec["fold_rule_pass"] = bool(fp) and all(
                v is not None and (
                    (claimed > 0 and v > 0) or (claimed < 0 and v < 0)
                )
                for v in fp
            )
        records.append(rec)
    return {"metrics": records, "critical_value": critical,
            "inventory_complete": inventory_complete,
            "refusals": refusal_recs}


# --- 12a ladder (spec 6.2-6.4) ---------------------------------------------
# Every statistic is a stat_fn(ctx, mult) shared verbatim between the
# observed side (obs_ctx, resampled) and the generated side (one
# ObsContext per seed over that seed's generated sessions, all-ones).

BOUNDARY_CELLS = {
    "pre_halt_close": {
        "boundary": ("1800+", "0-300"),
        "comparator": ("1800+", "300-1800"),
    },
    "post_halt_reopen": {
        "boundary": ("0-300", "300-1800"),
        "comparator": ("300-1800", "300-1800"),
    },
}
INTERIOR_LABELS = ("300-1800", "300-1800")
CLOSURE_CELLS = ((19, 300), (20, 300), (20, 60))


def stat_print_excess(hour):
    def fn(ctx, mult):
        tr = ctx.b1_support("trade", hour=hour).quantile(0.99, mult)
        qr = ctx.b1_support("quote", hour=hour).quantile(0.99, mult)
        if tr is None or qr in (None, 0):
            return None
        return tr / (qr / 2)  # half-ticks to ticks before division
    return fn


def stat_robust(hour, h):
    def fn(ctx, mult):
        return weighted_median_votes(
            ctx.b3_votes(hour, h, "robust"), mult
        )
    return fn


def stat_covnorm(hour, pair="60-300"):
    def fn(ctx, mult):
        return weighted_median_votes(ctx.b3_cov_votes(hour, pair), mult)
    return fn


def stat_fano(hour, w=60):
    def fn(ctx, mult):
        return ctx.b2_fano(hour, w, mult)
    return fn


def stat_count_p99(hour, w=60):
    def fn(ctx, mult):
        return ctx.b2_count_quantile(hour, w, 0.99, mult)
    return fn


def stat_tail_ratio(hour_key):
    def fn(ctx, mult):
        return weighted_median_votes(
            ctx.b4_votes(hour_key, "ratio_p999_p99"), mult
        )
    return fn


def stat_cond_sqrtn(hour, bin_name):
    def fn(ctx, mult):
        return ctx.b1_support(
            "sqrtn", hour=hour, bin_name=bin_name
        ).quantile(0.99, mult)
    return fn


def stat_boundary_quote(labels):
    def fn(ctx, mult):
        return ctx.b1_support("quote", labels=labels).quantile(0.99, mult)
    return fn


def stat_boundary_robust(labels):
    key = f"{labels[0]}|{labels[1]}"

    def fn(ctx, mult):
        return weighted_median_votes(
            ctx.b3_boundary_votes(key, 60, "robust"), mult
        )
    return fn


def stat_minute_p999(ctx, mult):
    return ctx.b1_support("trade").quantile(0.999, mult)


def conditional_adequacy_bins(obs_ctx: ObsContext,
                              gen_ctxs: list[ObsContext]) -> list:
    """The rung-2c required bins (spec 5.2): per implicated hour, a bin
    is REQUIRED when its pooled observed minute count reaches
    MIN_MINUTES_CELL; required generated support means every seed's
    count reaches the floor too. Returns
    (hour, bin_name, required, supported)."""
    ones_obs = obs_ctx.ones()
    out = []
    for hour in FAIL_HOURS_300:
        for bin_name in PARENT_COUNT_BIN_NAMES:
            if bin_name == "0":
                continue  # sqrt(N) undefined
            required = obs_ctx.b1_bin_count(
                hour, bin_name, ones_obs
            ) >= MIN_MINUTES_CELL
            supported = all(
                g.b1_bin_count(hour, bin_name, g.ones())
                >= MIN_MINUTES_CELL
                for g in gen_ctxs
            )
            out.append((hour, bin_name, required, supported))
    return out


def q1_vote_refusals(ctxs_named: list, votes_fn, cell: str) -> list:
    """Q1 all-session rule for vote-based metrics: every session of
    every context must qualify; each failure is one refusal string."""
    out = []
    for who, ctx in ctxs_named:
        for i, v in enumerate(votes_fn(ctx)):
            if v is None:
                out.append(
                    f"{who} session {ctx.sessions[i]} below floor "
                    f"at {cell}"
                )
    return out


def q1_floor_refusals(ctxs_named: list, counts_fn, floor: int,
                      cell: str) -> list:
    out = []
    for who, ctx in ctxs_named:
        for i, c in enumerate(counts_fn(ctx)):
            if c is None or c < floor:
                out.append(
                    f"{who} session {ctx.sessions[i]} carries {c} "
                    f"below floor {floor} at {cell}"
                )
    return out


def q1_exposure_refusals(ctxs_named: list, hour: int, w: int) -> list:
    """Count windows are judged by scheduled-exposure completeness:
    every session must carry exactly the scheduled count its own
    CALENDAR expects (never a max over the candidate's sessions - that
    would be self-referential). A missing serialized cell counts as
    zero; an expected zero with a scheduled zero passes."""
    out = []
    for who, ctx in ctxs_named:
        sched = ctx.b2_scheduled(hour, w)
        for i, s in enumerate(sched):
            expected = expected_scheduled_windows(
                ctx.sessions[i], hour, w
            )
            if (s or 0) != expected:
                out.append(
                    f"{who} session {ctx.sessions[i]} schedules {s} "
                    f"of {expected} calendar windows at hour {hour} "
                    f"w {w}"
                )
    return out


def build_family_metrics(obs_ctx: ObsContext,
                         gen_ctxs: list[ObsContext]) -> dict:
    """The 6.4 inventories: metric definitions with observed evaluators
    bound to obs_ctx, generated values from the per-seed contexts, and
    the Q1 all-session qualification refusals attached per metric."""
    everyone = [("observed", obs_ctx)] + [
        (f"seed {i + 1}", g) for i, g in enumerate(gen_ctxs)
    ]

    def defn(name, kind, predicate, stat_fn, qualify=None,
             force_refused=None):
        gen_vals = [stat_fn(g, g.ones()) for g in gen_ctxs]
        return {
            "name": name, "kind": kind, "predicate": predicate,
            "obs_fn": lambda mult, f=stat_fn: f(obs_ctx, mult),
            "gen_seeds": gen_vals,
            "gen_central": median_or_none(gen_vals),
            "qualify_refusals": qualify(everyone) if qualify else (),
            "force_refused": force_refused,
        }

    def q_minutes(hour):
        return lambda named: q1_floor_refusals(
            named, lambda ctx: ctx.minute_counts(hour=hour),
            MIN_MINUTES_CELL, f"hour {hour} minutes",
        )

    def q_votes(votes_fn, cell):
        return lambda named: q1_vote_refusals(named, votes_fn, cell)

    def q_exposure(hour, w=60):
        return lambda named: q1_exposure_refusals(named, hour, w)

    def q_labels(labels):
        return lambda named: q1_floor_refusals(
            named, lambda ctx: ctx.minute_counts(labels=labels),
            MIN_BOUNDARY_MINUTES_CELL,
            f"boundary minutes {labels[0]}|{labels[1]}",
        )

    fams: dict[str, list] = {}
    fams["child_walk"] = [
        defn(f"print_excess_h{h}", "log_ratio", "outside",
             stat_print_excess(h), qualify=q_minutes(h))
        for h in FAIL_HOURS_300
    ] + [
        defn(f"quote_robust_{w}_h{h}", "log_ratio", "inside",
             stat_robust(h, w),
             qualify=q_votes(
                 lambda ctx, hh=h, ww=w: ctx.b3_votes(hh, ww,
                                                      "robust"),
                 f"robust {w} hour {h}",
             ))
        for h in FAIL_HOURS_300 for w in (60, 300)
    ]
    arrival = [
        defn(f"fano_60_h{h}", "log_ratio", "outside", stat_fano(h),
             qualify=q_exposure(h))
        for h in FAIL_HOURS_300
    ] + [
        defn(f"count_p99_60_h{h}", "log_ratio", "outside",
             stat_count_p99(h), qualify=q_exposure(h))
        for h in FAIL_HOURS_300
    ]
    cond_bins = conditional_adequacy_bins(obs_ctx, gen_ctxs)
    for hour, bin_name, required, supported in cond_bins:
        if not required:
            continue
        if supported:
            arrival.append(defn(
                f"cond_sqrtn_p99_h{hour}_{bin_name}", "log_ratio",
                "inside", stat_cond_sqrtn(hour, bin_name),
            ))
        else:
            # A required-but-unsupported conditional metric stays
            # PRESENT as a refused record (Amendment D), never
            # omitted from the inventory.
            arrival.append(defn(
                f"cond_sqrtn_p99_h{hour}_{bin_name}", "log_ratio",
                "inside", stat_cond_sqrtn(hour, bin_name),
                force_refused="required observed bin without required "
                              "generated support",
            ))
    fams["arrival"] = arrival
    fams["_cond_bins"] = cond_bins
    fams["innovation"] = [
        defn(f"tail_ratio_h{h}", "log_ratio", "outside",
             stat_tail_ratio(str(h)),
             qualify=q_votes(
                 lambda ctx, hh=h: ctx.b4_votes(str(hh),
                                                "ratio_p999_p99"),
                 f"residuals hour {h}",
             ))
        for h in FAIL_HOURS_300
    ] + [
        defn("tail_ratio_all", "log_ratio", "outside",
             stat_tail_ratio("all"),
             qualify=q_votes(
                 lambda ctx: ctx.b4_votes("all", "ratio_p999_p99"),
                 "residuals all-hours",
             )),
    ]
    fams["reversion"] = [
        defn(f"robust_300_h{h}", "log_ratio", "outside",
             stat_robust(h, 300),
             qualify=q_votes(
                 lambda ctx, hh=h: ctx.b3_votes(hh, 300, "robust"),
                 f"robust 300 hour {h}",
             ))
        for h in HOT_HOURS
    ] + [
        defn("robust_60_h20", "log_ratio", "outside",
             stat_robust(20, 60),
             qualify=q_votes(
                 lambda ctx: ctx.b3_votes(20, 60, "robust"),
                 "robust 60 hour 20",
             )),
    ] + [
        defn(f"covnorm_h{h}", "raw_diff", "raw_direction",
             stat_covnorm(h),
             qualify=q_votes(
                 lambda ctx, hh=h: ctx.b3_cov_votes(hh, "60-300"),
                 f"covnorm hour {h}",
             ))
        for h in HOT_HOURS
    ]
    fams["garch"] = [
        defn(f"robust_300_h{h}", "log_ratio", "outside",
             stat_robust(h, 300),
             qualify=q_votes(
                 lambda ctx, hh=h: ctx.b3_votes(hh, 300, "robust"),
                 f"robust 300 hour {h}",
             ))
        for h in (19, 20)
    ] + [
        defn("robust_60_h20", "log_ratio", "outside",
             stat_robust(20, 60),
             qualify=q_votes(
                 lambda ctx: ctx.b3_votes(20, 60, "robust"),
                 "robust 60 hour 20",
             )),
    ]
    boundary = []
    for case, cells in BOUNDARY_CELLS.items():
        for labels, suffix, predicate in (
            (cells["boundary"], "", "outside"),
            (cells["comparator"], "_comparator", "inside"),
        ):
            boundary.append(defn(
                f"quote_p99_{case}{suffix}", "log_ratio", predicate,
                stat_boundary_quote(labels), qualify=q_labels(labels),
            ))
            boundary.append(defn(
                f"robust_60_{case}{suffix}", "log_ratio", predicate,
                stat_boundary_robust(labels),
                qualify=q_votes(
                    lambda ctx, lb=labels: ctx.b3_boundary_votes(
                        f"{lb[0]}|{lb[1]}", 60, "robust"
                    ),
                    f"boundary robust {labels[0]}|{labels[1]}",
                ),
            ))
    fams["boundary"] = boundary
    return fams


def closure_analysis(obs_ctx: ObsContext, gen_ctxs: list[ObsContext],
                     variant: str, mults: list[list[int]]) -> dict:
    """Sign/magnitude shuffle gap closures over the frozen cells, with
    the 5.3 confidence rules: per-cell point closures, the multi-target
    joint LCB (per-replicate minimum across cells, nearest-rank p5)."""
    ones = obs_ctx.ones()
    cells = []
    for hour, h in CLOSURE_CELLS:
        gen_vals = [
            weighted_median_votes(g.b3_votes(hour, h, "robust"), g.ones())
            for g in gen_ctxs
        ]
        t_gen = median_or_none(gen_vals)
        cells.append({
            "hour": hour, "horizon": h, "t_gen": t_gen,
            "obs_fn": lambda mult, hh=hour, w=h:
                obs_ctx.b3_robust_strict(hh, w, mult),
            "cf_fn": lambda mult, hh=hour, w=h:
                obs_ctx.perm_value(variant, hh, w, mult),
        })
    point_closures = []
    for c in cells:
        point_closures.append(gap_closure(
            c["t_gen"], c["cf_fn"](ones), c["obs_fn"](ones),
            generated_side=False,
        ))
    minima = []
    for mult in mults:
        worst = None
        refused = False
        for c in cells:
            cl = gap_closure(
                c["t_gen"], c["cf_fn"](mult), c["obs_fn"](mult),
                generated_side=False,
            )
            if cl is None:
                refused = True
                break
            if worst is None or cl < worst:
                worst = cl
        if not refused and worst is not None:
            minima.append(worst)
    minima.sort()
    # Spec 5.3 strictness: the joint LCB exists only when EVERY
    # bootstrap replicate produced a value - an unavailable replicate
    # refuses the result and the consuming rung fails closed.
    joint_lcb = (nearest_rank_p(minima, 0.05)
                 if len(minima) == len(mults) else None)
    return {
        "cells": [
            {"hour": c["hour"], "horizon": c["horizon"],
             "closure": pc}
            for c, pc in zip(cells, point_closures)
        ],
        "joint_lcb": joint_lcb,
        "all_points_pass": all(
            pc is not None and pc >= GAP_CLOSE_MIN
            for pc in point_closures
        ),
    }


def worsening_23_analysis(obs_ctx: ObsContext,
                          gen_ctxs: list[ObsContext],
                          mults: list[list[int]]) -> dict:
    """worsening_23 = |log(G/P)| - |log(G/O)| at 300 s robust scale;
    UCB = nearest-rank p95 of the bootstrap values (spec 6.2 rung 4)."""
    gen_vals = [
        weighted_median_votes(g.b3_votes(23, 300, "robust"), g.ones())
        for g in gen_ctxs
    ]
    t_gen = median_or_none(gen_vals)

    def value(mult):
        o = obs_ctx.b3_robust_strict(23, 300, mult)
        p = obs_ctx.perm_value("sign", 23, 300, mult)
        if t_gen is None or t_gen <= 0 or o is None or o <= 0 \
                or p is None or p <= 0:
            return None
        return abs(math.log(t_gen / p)) - abs(math.log(t_gen / o))

    point = value(obs_ctx.ones())
    reps = sorted(
        v for v in (value(m) for m in mults) if v is not None
    )
    # Spec 5.3 / Amendment E strictness: the whole diagnostic refuses
    # (None - section 10 makes the OBJECT nullable, not its members)
    # unless the point and every bootstrap replicate produced a value.
    if point is None or len(reps) != len(mults):
        return None
    return {"point": point, "se": stdev_ddof1(reps),
            "ucb": nearest_rank_p(reps, 0.95)}


class CountSubEval:
    """Per-seed generated support for the 5.2 counterfactual under a
    resampled observed share vector: per (hour, bin) cumulative counts
    over the shared trade-range support."""

    def __init__(self, gen_hist: dict[tuple, int]):
        support = sorted({k[2] for k in gen_hist})
        self.support = support
        index = {v: i for i, v in enumerate(support)}
        groups: dict[tuple, list] = {}
        totals: dict[tuple, int] = {}
        exceeds: dict[tuple, int] = {}
        for k, c in gen_hist.items():
            g = (k[3], parent_count_bin(k[0]))
            arr = groups.get(g)
            if arr is None:
                arr = groups[g] = [0] * len(support)
            arr[index[k[2]]] += c
            totals[g] = totals.get(g, 0) + c
            if k[2] > 968:
                exceeds[g] = exceeds.get(g, 0) + c
        for arr in groups.values():
            for i in range(1, len(support)):
                arr[i] += arr[i - 1]
        self.groups = groups
        self.totals = totals
        self.exceeds = exceeds
        self.gen_shares = {}
        hour_totals: dict[int, int] = {}
        for (h, _b), t in totals.items():
            hour_totals[h] = hour_totals.get(h, 0) + t
        self.hour_totals = hour_totals
        for (h, b), t in totals.items():
            self.gen_shares[(h, b)] = t / hour_totals[h]

    def counterfactual(self, obs_shares: dict):
        """(p999, exceed_rate, refused_hours). obs_shares: {hour:
        {bin: share}} over the observed populated minutes. Hours are
        the UNION of observed and generated: an observed hour with no
        generated support is itself a support refusal."""
        weights: dict[tuple, float] = {}
        refused_hours = set()
        for h in set(obs_shares) - set(self.hour_totals):
            # Observed minutes at an hour the generated month never
            # populates: o > 0 with g = 0 across every bin.
            refused_hours.add(h)
        for h, h_total in self.hour_totals.items():
            raw: dict[str, float] = {}
            refused = False
            o_per = obs_shares.get(h, {})
            for b in PARENT_COUNT_BIN_NAMES:
                o = o_per.get(b, 0.0)
                g = self.gen_shares.get((h, b), 0.0)
                if o > 0 and g > 0:
                    raw[b] = o / g
                elif o == 0 and g > 0:
                    raw[b] = 0.0
                elif o > 0 and g == 0:
                    refused = True
            if refused:
                refused_hours.add(h)
                continue
            wsum = sum(
                raw[b] * self.totals.get((h, b), 0) for b in raw
            )
            if wsum <= 0:
                refused_hours.add(h)
                continue
            scale = h_total / wsum
            for b, w in raw.items():
                weights[(h, b)] = w * scale
        live = [
            (g, w) for g, w in weights.items()
            if g[0] not in refused_hours
        ]
        total_w = sum(w * self.totals[g] for g, w in live)
        if total_w <= 0 or not self.support:
            return None, None, sorted(refused_hours)
        exceed_w = sum(w * self.exceeds.get(g, 0) for g, w in live)
        target = 0.999 * total_w
        lo, hi = 0, len(self.support) - 1
        while lo < hi:
            mid = (lo + hi) // 2
            cum = sum(w * self.groups[g][mid] for g, w in live)
            if cum >= target:
                hi = mid
            else:
                lo = mid + 1
        return (self.support[lo], exceed_w / total_w,
                sorted(refused_hours))


def obs_shares_under(obs_ctx: ObsContext, mult: list[int]) -> dict:
    shares: dict[int, dict[str, float]] = {}
    for hour in range(24):
        per = {}
        total = 0
        for b in PARENT_COUNT_BIN_NAMES:
            c = obs_ctx.b1_bin_count(hour, b, mult)
            per[b] = c
            total += c
        if total:
            shares[hour] = {b: c / total for b, c in per.items()}
    return shares


def count_substitution_closures(obs_ctx: ObsContext,
                                gen_hists: list[dict],
                                mults: list[list[int]]) -> dict:
    """Rung 2b: per-seed closures of the pooled minute-range p99.9 gap
    under the count substitution; the replicate statistic is the 8-seed
    median closure; LCB = nearest-rank p5."""
    evals = [CountSubEval(h) for h in gen_hists]
    t_gens = [
        weighted_nearest_rank(
            [(k[2], c) for k, c in h.items()], 0.999
        )
        for h in gen_hists
    ]
    ones = obs_ctx.ones()

    def seed_closures(mult):
        shares = obs_shares_under(obs_ctx, mult)
        t_obs = stat_minute_p999(obs_ctx, mult)
        out = []
        for ev, t_gen in zip(evals, t_gens):
            cf, _ex, refused = ev.counterfactual(shares)
            if refused:
                # A support refusal nulls the seed's closure (spec
                # 5.2): never a closure over partial support.
                out.append(None)
                continue
            out.append(gap_closure(t_gen, cf, t_obs,
                                   generated_side=True))
        return out

    point_by_seed = seed_closures(ones)
    # Any refused seed nulls the aggregate (Q3 ruling: a support
    # refusal makes the statistic unavailable, never a median over
    # fewer seeds).
    point = (median_or_none(point_by_seed)
             if all(c is not None for c in point_by_seed) else None)
    reps = []
    for mult in mults:
        seed_vals = seed_closures(mult)
        if all(c is not None for c in seed_vals):
            reps.append(median_or_none(seed_vals))
    reps.sort()
    lcb = (nearest_rank_p(reps, 0.05)
           if len(reps) == len(mults) else None)
    diagnostics = [
        gap_closure(t_gen, cf, 399.0, generated_side=True)
        for (t_gen, cf) in zip(
            t_gens,
            [ev.counterfactual(
                obs_shares_under(obs_ctx, ones)
            )[0] for ev in evals],
        )
    ]
    return {
        "per_seed_closure": point_by_seed,
        "closure_median": point,
        "closure_lcb": lcb,
        "diagnostic_closure_to_bound": diagnostics,
    }


def forensic_subchecks(per_seed_forensic: list[dict]) -> dict:
    """The trace-based rung inputs (rungs 3b, 3c, 5b) over the per-seed
    forensic records."""
    init_seeds = 0
    init_controls = 0
    esc_seeds = 0
    control_escs = []
    for seed_rec in per_seed_forensic:
        recs = seed_rec["records"]
        extreme = next(
            (r for r in recs if r["kind"] == "extreme_range"), None
        )
        control = next(
            (r for r in recs
             if r["kind"] == "control"
             and extreme is not None
             and r["matched_extreme_minute_start"]
             == extreme["minute_start_ns"]),
            None,
        )
        if extreme is not None:
            if extreme["initiation"] and \
                    extreme["largest_innovation_std"] \
                    > INITIATION_INNOVATION_MIN:
                init_seeds += 1
            esc = extreme["sigma_escalation"]
            if esc is not None and esc >= SIGMA_ESCALATION_MIN:
                esc_seeds += 1
        if control is not None:
            if control["initiation"] and \
                    control["largest_innovation_std"] \
                    > INITIATION_INNOVATION_MIN:
                init_controls += 1
            if control["sigma_escalation"] is not None:
                control_escs.append(control["sigma_escalation"])
    return {
        "initiation_seed_count": init_seeds,
        "initiation_control_count": init_controls,
        "escalation_seed_count": esc_seeds,
        "control_escalation_median": median_or_none(control_escs),
    }


def evaluate_ladder(obs_ctx: ObsContext, gen_ctxs: list[ObsContext],
                    gen_hists: list[dict],
                    per_seed_forensic: list[dict],
                    mults: list[list[int]]) -> dict:
    """The frozen 6.2 ladder: every fired rung recorded, 12b takes the
    first."""
    folds = fold_multiplicities(obs_ctx.sessions)
    ones = obs_ctx.ones()
    fams = build_family_metrics(obs_ctx, gen_ctxs)
    cond_bins = fams.pop("_cond_bins")
    envelopes = {
        fam: evaluate_family(fam, defs, mults, folds, ones)
        for fam, defs in fams.items()
    }

    def metric(fam, name):
        for m in envelopes[fam]["metrics"]:
            if m["name"] == name:
                return m
        raise AssertionError(f"{fam}/{name} missing from the envelope")

    def complete(fam):
        return envelopes[fam]["inventory_complete"]

    def fires_outside(fam, m):
        # Envelope-dependent: false whenever the family inventory is
        # incomplete (Amendment D).
        return (complete(fam) and not m["refused"]
                and bool(m["outside_band"])
                and bool(m["envelope_excludes_edge"])
                and bool(m["seed_rule_pass"])
                and bool(m["fold_rule_pass"]))

    def clean_inside(fam, m):
        return (complete(fam) and not m["refused"]
                and bool(m["interval_inside_band"])
                and bool(m["seed_rule_pass"])
                and bool(m["fold_rule_pass"]))

    rungs = []
    refusals: list[dict] = []

    # Rung 1: child-walk isolation, paired BY HOUR.
    per_hour = {}
    for h in FAIL_HOURS_300:
        a = fires_outside(
            "child_walk", metric("child_walk", f"print_excess_h{h}")
        )
        b = all(
            clean_inside(
                "child_walk",
                metric("child_walk", f"quote_robust_{w}_h{h}"),
            )
            for w in (60, 300)
        )
        per_hour[h] = a and b
    sub1 = {
        "a_print_excess": any(
            fires_outside(
                "child_walk",
                metric("child_walk", f"print_excess_h{h}"),
            )
            for h in FAIL_HOURS_300
        ),
        "b_mid_clean": any(per_hour.values()),
    }
    rungs.append({"name": "child_walk", "subchecks": sub1,
                  "fired": any(per_hour.values())})

    # Rung 2: arrival sufficiency.
    a_env = any(
        fires_outside("arrival", metric("arrival", f"{stat}_h{h}"))
        for h in FAIL_HOURS_300
        for stat in ("fano_60", "count_p99_60")
    )
    csub = count_substitution_closures(obs_ctx, gen_hists, mults)
    b_closure = (
        csub["closure_median"] is not None
        and csub["closure_median"] >= GAP_CLOSE_MIN
        and csub["closure_lcb"] is not None
        and csub["closure_lcb"] > GAP_CLOSE_LCB_MIN
    )
    cond_ok = True
    for hour, bin_name, required, _supported in cond_bins:
        if not required:
            continue
        # An unsupported required bin is a refused (force_refused)
        # metric: clean_inside is false and the family inventory is
        # incomplete, so the rung fails closed with the refusal
        # mirrored from the family envelope.
        if not clean_inside("arrival", metric(
            "arrival", f"cond_sqrtn_p99_h{hour}_{bin_name}"
        )):
            cond_ok = False
    sub2 = {"a_envelope": a_env, "b_closure": b_closure,
            "c_conditional": cond_ok}
    rungs.append({"name": "arrival", "subchecks": sub2,
                  "fired": all(sub2.values())})

    # Rung 3: innovation tail.
    forensics = forensic_subchecks(per_seed_forensic)
    a_tail = any(
        fires_outside("innovation", metric("innovation", name))
        for name in (
            [f"tail_ratio_h{h}" for h in FAIL_HOURS_300]
            + ["tail_ratio_all"]
        )
    )
    b_init = forensics["initiation_seed_count"] >= SEED_DIRECTION_MIN
    c_controls = forensics["initiation_control_count"] <= 2
    sub3 = {"a_tail_ratio": a_tail, "b_initiation": b_init,
            "c_controls": c_controls}
    rungs.append({"name": "innovation", "subchecks": sub3,
                  "fired": all(sub3.values())})

    # Rung 4: signed reversion. a_closure consumes required reversion
    # inventory cells, so it is envelope-dependent: false whenever the
    # family inventory is incomplete (Amendment D).
    sign = closure_analysis(obs_ctx, gen_ctxs, "sign", mults)
    a_closure = (
        complete("reversion")
        and sign["all_points_pass"]
        and sign["joint_lcb"] is not None
        and sign["joint_lcb"] > GAP_CLOSE_LCB_MIN
    )
    # Fold stability: each cell's closure sign agrees with that cell's
    # own point closure across every qualifying fold.
    point_sign = {
        (c["hour"], c["horizon"]): (c["closure"] or 0) > 0
        for c in sign["cells"]
    }
    folds_ok = bool(folds)
    for f in folds:
        for hour, h in CLOSURE_CELLS:
            gen_vals = [
                weighted_median_votes(
                    g.b3_votes(hour, h, "robust"), g.ones()
                )
                for g in gen_ctxs
            ]
            cl = gap_closure(
                median_or_none(gen_vals),
                obs_ctx.perm_value("sign", hour, h, f),
                obs_ctx.b3_robust_strict(hour, h, f),
                generated_side=False,
            )
            if cl is None or (cl > 0) != point_sign[(hour, h)]:
                folds_ok = False

    def covariance_fires(h: int) -> bool:
        m = metric("reversion", f"covnorm_h{h}")
        return (not m["refused"] and m["point"] is not None
                and m["point"] > 0 and m["interval_low"] is not None
                and m["interval_low"] > 0)

    c_cov = all(covariance_fires(h) for h in HOT_HOURS)
    sub4 = {"a_closure": a_closure, "b_folds": folds_ok,
            "c_covariance": c_cov}
    fired4 = all(sub4.values())
    # Amendment E: worsening_23 is evaluated only after the rung
    # fires. A refused measurement records null resolution fields WITH
    # exactly one matching RefusalRec - never a fabricated
    # hour-resolved. An unfired rung leaves everything null by
    # inapplicability, no refusal.
    w23 = None
    uniform = None
    resolution = None
    if fired4:
        w23 = worsening_23_analysis(obs_ctx, gen_ctxs, mults)
        if w23 is None:
            refusals.append({
                "scope": "reversion",
                "cell": "worsening_23",
                "reason": "worsening_23 refused: missing point or "
                          "incomplete bootstrap population",
            })
        else:
            uniform = w23["ucb"] <= 0
            resolution = "uniform" if uniform else "hour-resolved"
    rungs.append({"name": "reversion", "subchecks": sub4,
                  "fired": fired4, "uniform_eligible": uniform,
                  "required_resolution": resolution})

    # Rung 5: GARCH persistence. a_closure is the envelope-dependent
    # subcheck: false whenever the family inventory is incomplete
    # (Amendment D); b_escalation keeps its measured forensic boolean.
    mag = closure_analysis(obs_ctx, gen_ctxs, "magnitude", mults)
    a5 = (
        complete("garch")
        and mag["all_points_pass"]
        and mag["joint_lcb"] is not None
        and mag["joint_lcb"] > GAP_CLOSE_LCB_MIN
    )
    b5 = (
        forensics["escalation_seed_count"] >= SEED_DIRECTION_MIN
        and forensics["control_escalation_median"] is not None
        and forensics["control_escalation_median"]
        < CONTROL_ESCALATION_MAX
    )
    sub5 = {"a_closure": a5, "b_escalation": b5}
    rungs.append({"name": "garch", "subchecks": sub5,
                  "fired": all(sub5.values())})

    # Rung 6: boundary-local state.
    no_prior = not any(r["fired"] for r in rungs)
    b_ok = False
    comp_ok = False
    for case in BOUNDARY_CELLS:
        for stem in ("quote_p99", "robust_60"):
            m_b = metric("boundary", f"{stem}_{case}")
            m_c = metric("boundary", f"{stem}_{case}_comparator")
            if fires_outside("boundary", m_b) \
                    and clean_inside("boundary", m_c):
                b_ok = True
                comp_ok = True
    sub6 = {"a_boundary_band": b_ok, "b_comparator_clean": comp_ok,
            "c_no_prior_rung": no_prior}
    rungs.append({"name": "boundary", "subchecks": sub6,
                  "fired": all(sub6.values())})

    # Localization flags (6.2, Amendment C): Boolean only for a FIRED
    # child_walk / reversion / garch rung when every input qualifies;
    # null WITH a refusal when such a rung cannot measure it. The
    # child-walk discrepancy is the label-filtered PRINT-EXCESS ratio
    # (trade p99 over quote p99 in ticks); reversion and GARCH use the
    # label-filtered hour-20 robust_scale_60. Point estimates only.
    def stat_boundary_excess(labels):
        def fn(ctx, mult):
            tr = ctx.b1_support("trade", labels=labels).quantile(
                0.99, mult
            )
            qr = ctx.b1_support("quote", labels=labels).quantile(
                0.99, mult
            )
            if tr is None or qr in (None, 0):
                return None
            return tr / (qr / 2)
        return fn

    def boundary_log_ratio(stat_fn):
        gen_vals = [stat_fn(g, g.ones()) for g in gen_ctxs]
        g = median_or_none(gen_vals)
        o = stat_fn(obs_ctx, ones)
        if g is None or g <= 0 or o is None or o <= 0:
            return None
        return math.log(g / o)

    def localization(rung_name, stat_for):
        mags = []
        for case in BOUNDARY_CELLS:
            v = boundary_log_ratio(
                stat_for(BOUNDARY_CELLS[case]["boundary"])
            )
            if v is None:
                refusals.append({
                    "scope": rung_name,
                    "cell": f"localization boundary {case}",
                    "reason": "localization input refused",
                })
                return None
            mags.append(abs(v))
        interior = boundary_log_ratio(stat_for(INTERIOR_LABELS))
        if interior is None or interior == 0:
            refusals.append({
                "scope": rung_name,
                "cell": "localization interior",
                "reason": "localization ratio undefined",
            })
            return None
        return max(mags) >= 2 * abs(interior)

    loc_stats = {
        "child_walk": stat_boundary_excess,
        "reversion": stat_boundary_robust,
        "garch": stat_boundary_robust,
    }
    for rung in rungs:
        loc = None
        if rung["fired"] and rung["name"] in loc_stats:
            loc = localization(rung["name"], loc_stats[rung["name"]])
        rung["boundary_localized"] = loc
        rung.setdefault("uniform_eligible", None)
        rung.setdefault("required_resolution", None)
        # The rung mirrors its own refusals plus the consumed
        # families' envelope and metric refusals (Amendment D).
        fam_for_rung = {
            "child_walk": ["child_walk"], "arrival": ["arrival"],
            "innovation": ["innovation"], "reversion": ["reversion"],
            "garch": ["garch"], "boundary": ["boundary"],
        }[rung["name"]]
        rung["refusals"] = [
            r for r in refusals if r["scope"] == rung["name"]
        ] + [
            r for fam in fam_for_rung
            for r in envelopes[fam]["refusals"]
        ]

    eligible = [r["name"] for r in rungs if r["fired"]]
    return {
        "envelopes": envelopes,
        "count_substitution": csub,
        "cond_bins": cond_bins,
        "sign_closure": sign,
        "magnitude_closure": mag,
        "worsening_23": w23,
        "forensic_subchecks": forensics,
        "rungs": rungs,
        "eligible": eligible,
        "selected": eligible[0] if eligible else None,
        "verdict": "family-eligible" if eligible
        else "no-family-eligible",
    }


# ---------------------------------------------------------------------------
# Protocol 12a: the measure12a mode (spec Bricks O and M). Brick O lands
# the observed half; the generated half arrives with Brick G's
# GenType::Measure12a cache and the ladder verdict with Brick M.
# ---------------------------------------------------------------------------

MEASURE12A_OBSERVED_CACHE = os.path.join(
    ROOT, "analysis", "out", "mnq-measure12a-observed.json"
)
MEASURE12A_ARTIFACT = os.path.join(ROOT, "analysis", "mnq-measure-12a.json")


def run_measure12a_observed(directory: str = DELIVERY_DIR,
                            ledger_path: str | None = None,
                            preflight_artifact_path: str =
                            PREFLIGHT_ARTIFACT) -> dict:
    """The observed half: per-session sufficient records plus the monthly
    aggregates, bound to the input and sub-contract hashes."""
    hashes = verify_input(directory, ledger_path)
    preflight, preflight_hash = require_preflight(
        hashes, preflight_artifact_path
    )
    usable = preflight["usable_sessions"]
    per_session = measure12a_observe(
        parse_stream(data_files(directory)), usable
    )
    pooled = pool_block1_hists([r["block1_hist"] for r in per_session])
    monthly = {
        "block1": block1_blocks(pooled),
        "block2": pool_block2([r["block2"] for r in per_session]),
        "block3": aggregate_block3([r["block3"] for r in per_session]),
        "block4": aggregate_block4([r["block4"] for r in per_session]),
    }
    return {
        "binding": {
            "job_id": JOB_ID,
            "subcontract_hash": subcontract_hash(),
            "preflight_artifact_hash": preflight_hash,
            "file_hashes": hashes,
            "tape_protocol_version": 11,
        },
        "per_session": per_session,
        "monthly": monthly,
        "permutations_monthly": aggregate_permutations(
            [r["permutations"] for r in per_session]
        ),
    }


def tree_median(trees: list):
    """Recursive 8-seed median over IDENTICALLY SHAPED JSON trees:
    every seed must carry the same key sets (a divergence refuses -
    the seeds run one code path over one calendar, so shape drift is
    a defect, never data); a numeric leaf where ANY seed is None
    centralizes to None (the strict contract: no median over fewer
    than the full seed set). Strings and bools must agree."""
    if all(isinstance(t, dict) for t in trees):
        key_sets = [tuple(sorted(t.keys())) for t in trees]
        if len(set(key_sets)) != 1:
            raise Refusal(
                "generated seed trees diverge in shape: "
                f"{sorted(set(key_sets))[:2]}"
            )
        return {
            k: tree_median([t[k] for t in trees])
            for k in sorted(trees[0])
        }
    if any(t is None for t in trees):
        return None
    if all(isinstance(v, (int, float)) and not isinstance(v, bool)
           for v in trees):
        return median_or_none(trees)
    if len({json.dumps(t, sort_keys=True, default=str)
            for t in trees}) != 1:
        raise Refusal("generated seed trees diverge on a non-numeric "
                      f"leaf: {trees[:2]}")
    return trees[0]


def central_blocks_from_seeds(seed_blocks: list[dict]) -> dict:
    """CentralBlocks (spec section 10): SeedBlocks minus block1.hist,
    every scalar the 8-seed median."""
    stripped = []
    for b in seed_blocks:
        b1 = {k: v for k, v in b["block1"].items() if k != "hist"}
        stripped.append({**b, "block1": b1})
    return tree_median(stripped)


def assemble_measure12a_artifact(observed: dict, generated_seeds: list,
                                 binding_extra: dict,
                                 mults: list[list[int]],
                                 cost: dict) -> dict:
    """The production artifact assembler: refuses UNCONDITIONALLY
    unless the supplied bootstrap population is exactly the frozen
    BOOTSTRAP_REPLICATES. The selftest builds its truncated schema
    fixture through the internal body below, never through this
    entrypoint."""
    if len(mults) != BOOTSTRAP_REPLICATES:
        raise Refusal(
            f"bootstrap population {len(mults)} is not the required "
            f"{BOOTSTRAP_REPLICATES} replicates"
        )
    return _assemble_measure12a(observed, generated_seeds,
                                binding_extra, mults, cost)


def _assemble_measure12a(observed: dict, generated_seeds: list,
                         binding_extra: dict,
                         mults: list[list[int]],
                         cost: dict) -> dict:
    """The section-10 artifact from the observed half and the per-seed
    generated records. Each generated seed record carries
    {seed, per_session (the same record shape), forensic, cost}."""
    per_session = observed["per_session"]
    obs_ctx = ObsContext(per_session)
    gen_ctxs = [ObsContext(g["per_session"]) for g in generated_seeds]
    gen_hists = [
        pool_block1_hists(
            [r["block1_hist"] for r in g["per_session"]]
        )
        for g in generated_seeds
    ]
    per_seed_forensic = [g["forensic"] for g in generated_seeds]
    ladder = evaluate_ladder(
        obs_ctx, gen_ctxs, gen_hists, per_seed_forensic, mults
    )
    obs_shares = obs_shares_under(obs_ctx, obs_ctx.ones())

    def cond_adequacy_records(seed_ctx: ObsContext) -> list:
        out = []
        ones_o = obs_ctx.ones()
        for hour, bin_name, required, supported in ladder["cond_bins"]:
            rec = {"hour": hour, "bin_name": bin_name,
                   "observed_p99": None, "generated_p99": None,
                   "ratio": None, "interval_low": None,
                   "interval_high": None,
                   "interval_inside_band": None,
                   "seed_inside_count": None,
                   "required": required, "supported": supported}
            if required and supported:
                obs_p99 = stat_cond_sqrtn(hour, bin_name)(
                    obs_ctx, ones_o
                )
                gen_p99 = stat_cond_sqrtn(hour, bin_name)(
                    seed_ctx, seed_ctx.ones()
                )
                mrec = next(
                    (m for m in ladder["envelopes"]["arrival"][
                        "metrics"]
                     if m["name"]
                     == f"cond_sqrtn_p99_h{hour}_{bin_name}"),
                    None,
                )
                rec.update({
                    "observed_p99": obs_p99,
                    "generated_p99": gen_p99,
                    "ratio": (gen_p99 / obs_p99
                              if obs_p99 and gen_p99 else None),
                    "interval_low": mrec["interval_low"]
                    if mrec else None,
                    "interval_high": mrec["interval_high"]
                    if mrec else None,
                    "interval_inside_band":
                        mrec["interval_inside_band"] if mrec else None,
                    "seed_inside_count": mrec["seed_inside_count"]
                    if mrec else None,
                })
            out.append(rec)
        return out

    gen_per_seed = []
    for g, gctx, hist, closure in zip(
        generated_seeds, gen_ctxs, gen_hists,
        ladder["count_substitution"]["per_seed_closure"],
    ):
        csub = count_substitution(hist, obs_shares)
        csub["closure_p999"] = closure
        csub["closure_lcb"] = (
            ladder["count_substitution"]["closure_lcb"]
        )
        csub["conditional_adequacy"] = cond_adequacy_records(gctx)
        csub["diagnostic_closure_to_bound"] = None
        seed_sessions = g["per_session"]
        pooled = pool_block1_hists(
            [r["block1_hist"] for r in seed_sessions]
        )
        gen_per_seed.append({
            "seed": g["seed"],
            "blocks": {
                "block1": block1_blocks(pooled),
                "block2": pool_block2(
                    [r["block2"] for r in seed_sessions]
                ),
                "block3": aggregate_block3(
                    [r["block3"] for r in seed_sessions]
                ),
                "block4": aggregate_block4(
                    [r["block4"] for r in seed_sessions]
                ),
            },
            "count_substitution": csub,
            "forensic": g["forensic"],
            "cost": g["cost"],
        })
    diag_closures = (
        ladder["count_substitution"]["diagnostic_closure_to_bound"]
    )
    for rec, diag in zip(gen_per_seed, diag_closures):
        rec["count_substitution"]["diagnostic_closure_to_bound"] = diag

    central_blocks = central_blocks_from_seeds(
        [rec["blocks"] for rec in gen_per_seed]
    )
    rungs_out = []
    for rung in ladder["rungs"]:
        rungs_out.append({
            "name": rung["name"],
            "subchecks": rung["subchecks"],
            "fired": rung["fired"],
            "boundary_localized": rung["boundary_localized"],
            "refusals": rung["refusals"],
            "uniform_eligible": rung["uniform_eligible"],
            "required_resolution": rung["required_resolution"],
        })
    # Every refusal-null pairs with exactly one top-level record; the
    # per-session and forensic arrays are scoped mirrors (spec sec 10).
    refused_cells: list[dict] = []
    seen_refusals: set[tuple] = set()

    def add_refusal(rec: dict) -> None:
        key = (rec["scope"], rec["cell"], rec["reason"])
        if key not in seen_refusals:
            seen_refusals.add(key)
            refused_cells.append(rec)

    for env in ladder["envelopes"].values():
        for rec in env["refusals"]:
            add_refusal(rec)
    for rung in ladder["rungs"]:
        for rec in rung["refusals"]:
            add_refusal(rec)
    for seed_rec in gen_per_seed:
        for rec in seed_rec["count_substitution"]["support_refusals"]:
            add_refusal(rec)
    # The scoped per-session and forensic arrays are MIRRORS of
    # top-level records (spec section 10): observed per-session
    # refusals (the Amendment-F standardizer omissions among them),
    # generated per-seed per-session refusals, per-seed forensic
    # refusals.
    for rec in per_session:
        for r in rec["refusals"]:
            add_refusal(r)
    for g in generated_seeds:
        for rec in g["per_session"]:
            for r in rec["refusals"]:
                add_refusal(r)
        for r in g["forensic"]["refusals"]:
            add_refusal(r)

    empty_bins = [
        {"scope": "observed block1", "cell": f"hour {h} bin {b}"}
        for h in range(24)
        for b in PARENT_COUNT_BIN_NAMES
        if obs_ctx.b1_bin_count(h, b, obs_ctx.ones()) == 0
    ]

    return {
        "binding": {**observed["binding"], **binding_extra},
        # The exact section 7 names, verbatim values (the Python-side
        # bin constants serialize under their spec names).
        "constants": {
            **{k: globals()[k] for k in (
                "FAIL_HOURS_300", "FAIL_HOURS_60", "HOT_HOURS",
                "COLD_HOURS", "RESIDUAL_WINDOW_S",
                "RESIDUAL_MIN_HISTORY", "RESIDUAL_EXCEED_MULTIPLES",
                "INNOVATION_EXCEED_ABS", "PERMUTATION_REPLICATES",
                "PERMUTATION_VARIANTS", "BOOTSTRAP_REPLICATES",
                "BOOTSTRAP_BLOCK_SESSIONS", "BOOTSTRAP_BASE_SEED",
                "PERMUTATION_BASE_SEED", "CONTROL_TIE_BASE_SEED",
                "FAMILY_ENVELOPE_LEVEL", "SEED_DIRECTION_MIN",
                "FOLD_MIN_SESSIONS", "MATERIALITY_BAND",
                "GAP_CLOSE_MIN", "GAP_CLOSE_LCB_MIN", "GAP_CLOSE_EPS",
                "COUNT_WINDOWS_S", "WALL_HORIZONS_S",
                "EXCEEDANCE_TICKS",
                "MIN_1S_CELL_RETURNS", "MIN_5S_CELL_RETURNS",
                "MIN_15S_CELL_RETURNS", "MIN_60S_CELL_RETURNS",
                "MIN_300S_CELL_RETURNS", "MIN_RESIDUAL_CELL",
                "MIN_MINUTES_CELL", "MIN_BOUNDARY_MINUTES_CELL",
                "MIN_BOUNDARY_60S_CELL_RETURNS",
                "SIGMA_ESCALATION_MIN", "CONTROL_ESCALATION_MAX",
                "INITIATION_INNOVATION_MIN",
            )},
            "PARENT_COUNT_BINS": PARENT_COUNT_BIN_NAMES,
            "SEGMENT_OPEN_BINS_S": SINCE_OPEN_BIN_NAMES,
            "SEGMENT_CLOSE_BINS_S": UNTIL_CLOSE_BIN_NAMES,
        },
        "observed": {
            "per_session": per_session,
            "monthly": observed["monthly"],
            "permutations_monthly": observed["permutations_monthly"],
        },
        "generated": {
            "per_seed": gen_per_seed,
            "central": {
                "blocks": central_blocks,
                "count_substitution": {
                    "closure_p999_median": (
                        ladder["count_substitution"]["closure_median"]
                    ),
                    "refused_hour_union": sorted({
                        h for rec in gen_per_seed
                        for h in rec["count_substitution"][
                            "refused_hours"]
                    }),
                },
                "pooled_diagnostic_hist": None,
            },
        },
        "bootstrap": {
            "seed_rule": "splitmix64(BOOTSTRAP_BASE_SEED xor "
                         "(replicate << 8) xor block) mod sessions",
            "replicates": len(mults),
            "per_family": {
                fam: {
                    "metrics": env["metrics"],
                    "critical_value": env["critical_value"],
                    "inventory_complete": env["inventory_complete"],
                }
                for fam, env in ladder["envelopes"].items()
            },
        },
        "ladder": {
            "rungs": rungs_out,
            "eligible": ladder["eligible"],
            "selected": ladder["selected"],
            "verdict": ladder["verdict"],
        },
        "cost": cost,
        "diagnostics": {
            "warmup_exclusions": {
                hour: sum(
                    r["block4"].get(hour, {}).get("warmup_excluded", 0)
                    for r in per_session
                )
                for hour in sorted({
                    h for r in per_session for h in r["block4"]
                    if h != "all"
                })
            },
            "refused_cells": refused_cells,
            "empty_bins": empty_bins,
            "worsening_23": ladder["worsening_23"],
        },
    }


MEASURE12A_CONSTANT_NAMES = (
    "FAIL_HOURS_300", "FAIL_HOURS_60", "HOT_HOURS", "COLD_HOURS",
    "RESIDUAL_WINDOW_S", "RESIDUAL_MIN_HISTORY",
    "RESIDUAL_EXCEED_MULTIPLES", "INNOVATION_EXCEED_ABS",
    "PERMUTATION_REPLICATES", "PERMUTATION_VARIANTS",
    "BOOTSTRAP_REPLICATES", "BOOTSTRAP_BLOCK_SESSIONS",
    "BOOTSTRAP_BASE_SEED", "PERMUTATION_BASE_SEED",
    "CONTROL_TIE_BASE_SEED", "FAMILY_ENVELOPE_LEVEL",
    "SEED_DIRECTION_MIN", "FOLD_MIN_SESSIONS", "MATERIALITY_BAND",
    "GAP_CLOSE_MIN", "GAP_CLOSE_LCB_MIN", "GAP_CLOSE_EPS",
    "COUNT_WINDOWS_S", "WALL_HORIZONS_S", "EXCEEDANCE_TICKS",
    "PARENT_COUNT_BINS", "SEGMENT_OPEN_BINS_S", "SEGMENT_CLOSE_BINS_S",
    "MIN_1S_CELL_RETURNS", "MIN_5S_CELL_RETURNS",
    "MIN_15S_CELL_RETURNS", "MIN_60S_CELL_RETURNS",
    "MIN_300S_CELL_RETURNS", "MIN_RESIDUAL_CELL", "MIN_MINUTES_CELL",
    "MIN_BOUNDARY_MINUTES_CELL", "MIN_BOUNDARY_60S_CELL_RETURNS",
    "SIGMA_ESCALATION_MIN", "CONTROL_ESCALATION_MAX",
    "INITIATION_INNOVATION_MIN",
)

MEASURE12A_FAMILIES = ("child_walk", "arrival", "innovation",
                       "reversion", "garch", "boundary")

MEASURE12A_RUNG_SUBCHECKS = {
    "child_walk": {"a_print_excess", "b_mid_clean"},
    "arrival": {"a_envelope", "b_closure", "c_conditional"},
    "innovation": {"a_tail_ratio", "b_initiation", "c_controls"},
    "reversion": {"a_closure", "b_folds", "c_covariance"},
    "garch": {"a_closure", "b_escalation"},
    "boundary": {"a_boundary_band", "b_comparator_clean",
                 "c_no_prior_rung"},
}

_B1_HIST_KEYS = {"n", "quote_range_half_ticks", "trade_range_ticks",
                 "hour", "since_open_bin", "until_close_bin", "count"}
_B1_BIN_KEYS = {
    "minute_count", "quote_range_denominator",
    "quote_range_p50", "quote_range_p90", "quote_range_p99",
    "quote_range_p999", "trade_range_p50", "trade_range_p90",
    "trade_range_p99", "trade_range_p999", "trade_range_sqrt_n_p50",
    "trade_range_sqrt_n_p90", "trade_range_sqrt_n_p99",
}
_B1_SUMMARY_KEYS = _B1_BIN_KEYS | {
    "n_p50", "n_p90", "n_p99", "n_p999",
    "exceed_399", "exceed_642", "exceed_968", "denominator",
    "trade_to_quote_p99_ratio", "by_parent_count_bin",
}
_B2_CELL_KEYS = {
    "scheduled_windows", "zero_windows", "count_hist",
    "run_length_hist", "paired_lag_count", "sum_x", "sum_y",
    "sumsq_x", "sumsq_y", "sum_xy", "zero_fraction", "mean", "fano",
    "count_p90", "count_p99", "count_p999", "lag1_autocorr", "run_p90",
}
_B3_CELL_KEYS = {"return_count", "robust_scale", "rms_scale"}
_B3_PAIR_KEYS = {"window_count", "vr", "cov_contrib",
                 "cov_contrib_norm"}
_B4_CELL_KEYS = {
    "residual_count", "warmup_excluded", "zero_fraction",
    "nz_abs_p90", "nz_abs_p99", "nz_abs_p999",
    "ratio_p99_p90", "ratio_p999_p99",
    "exceed_4", "exceed_8", "exceed_16",
}
_PERM_KEYS = {
    "segment_index", "hour", "variant", "replicate",
    "return_count_60", "sum_abs_60", "max_abs_60",
    "return_count_300", "sum_abs_300", "max_abs_300",
}
_FORENSIC_KEYS = {
    "seed", "kind", "matched_extreme_minute_start", "minute_start_ns",
    "minute_end_ns", "utc_hour", "segment_index", "parent_count",
    "trade_count", "traced_parents", "largest_innovation_std",
    "largest_innovation_ts_ns", "innovation_exceed_4",
    "innovation_exceed_8", "innovation_exceed_16", "initiation",
    "sigma_start", "sigma_peak", "sigma_end", "sigma_escalation",
    "latent_mid_range_ticks", "quote_mid_range_half_ticks",
    "trade_range_ticks", "trade_to_quote_range_ratio",
    "quote_to_latent_range_ratio", "max_signed_run", "clamp_hits",
    "arch_share_next", "arch_share_minute_max",
}
_METRIC_KEYS = {
    "name", "kind", "predicate", "point", "se", "interval_low",
    "interval_high", "band_low", "band_high", "outside_band",
    "envelope_excludes_edge", "interval_inside_band",
    "seed_same_side_count", "seed_inside_count",
    "seed_rule_pass", "fold_rule_pass", "refused",
}
_METRIC_NULLABLE = _METRIC_KEYS - {"name", "kind", "predicate",
                                   "refused"}
_RUNG_KEYS = {"name", "subchecks", "fired", "boundary_localized",
              "refusals", "uniform_eligible", "required_resolution"}
_COND_KEYS = {
    "hour", "bin_name", "observed_p99", "generated_p99", "ratio",
    "interval_low", "interval_high", "interval_inside_band",
    "seed_inside_count", "required", "supported",
}
_COUNTSUB_KEYS = {
    "shares_observed", "shares_generated", "weights", "refused_hours",
    "support_refusals", "counterfactual_p999",
    "counterfactual_exceed_968", "closure_p999", "closure_lcb",
    "conditional_adequacy", "diagnostic_closure_to_bound",
}


def measure12a_schema_errors(artifact: dict) -> list[str]:
    """The recursive section-10 exact-schema validator: every listed
    key present, no unlisted key, the literal rung subcheck sets, the
    refusal-null pairing in both directions (with the sole Amendment-F
    standardizer-omission exception), and the Amendment E truth table.
    Returns a flat list of violation strings; empty means conformant."""
    errs: list[str] = []

    def keys_exact(obj, want, where) -> bool:
        if not isinstance(obj, dict):
            errs.append(f"{where}: not a dict")
            return False
        got = set(obj.keys())
        if got != set(want):
            errs.append(f"{where}: key mismatch {sorted(got ^ set(want))}")
            return False
        return True

    def refusal_rec(obj, where) -> None:
        if keys_exact(obj, {"scope", "cell", "reason"}, where):
            if not all(isinstance(obj[k], str)
                       for k in ("scope", "cell", "reason")):
                errs.append(f"{where}: RefusalRec fields must be "
                            f"three strings")

    def block1_summary_rec(obj, where) -> None:
        if keys_exact(obj, _B1_SUMMARY_KEYS, where):
            bins = obj["by_parent_count_bin"]
            if keys_exact(bins, set(PARENT_COUNT_BIN_NAMES),
                          f"{where}.by_parent_count_bin"):
                for name, b in bins.items():
                    keys_exact(b, _B1_BIN_KEYS, f"{where}.bin[{name}]")

    def block1_blocks_rec(obj, where, with_hist: bool) -> None:
        want = {"summary", "by_labels"} | ({"hist"} if with_hist
                                           else set())
        if not keys_exact(obj, want, where):
            return
        if with_hist:
            for i, row in enumerate(obj["hist"]):
                keys_exact(row, _B1_HIST_KEYS, f"{where}.hist[{i}]")
        for h, s in obj["summary"].items():
            block1_summary_rec(s, f"{where}.summary[{h}]")
        for lp, per in obj["by_labels"].items():
            for h, s in per.items():
                block1_summary_rec(s, f"{where}.by_labels[{lp}][{h}]")

    def block2_map(obj, where) -> None:
        for h, per_w in obj.items():
            for w, c in per_w.items():
                keys_exact(c, _B2_CELL_KEYS, f"{where}[{h}][{w}]")

    def block3_rec(obj, where) -> None:
        if not keys_exact(obj, {"cells", "pairs",
                                "lag1_parent_autocorr",
                                "hour20_labels"}, where):
            return
        for h, per in obj["cells"].items():
            for hz, c in per.items():
                keys_exact(c, _B3_CELL_KEYS,
                           f"{where}.cells[{h}][{hz}]")
        for h, per in obj["pairs"].items():
            for p, c in per.items():
                keys_exact(c, _B3_PAIR_KEYS,
                           f"{where}.pairs[{h}][{p}]")
        for lp, per in obj["hour20_labels"].items():
            for hz, c in per.items():
                keys_exact(c, _B3_CELL_KEYS,
                           f"{where}.hour20[{lp}][{hz}]")

    def block4_map(obj, where) -> None:
        if "all" not in obj:
            errs.append(f"{where}: missing the Amendment-B literal "
                        f"\"all\" pooled-hours cell")
        for h, c in obj.items():
            keys_exact(c, _B4_CELL_KEYS, f"{where}[{h}]")

    def seed_blocks(obj, where, with_hist: bool) -> None:
        if not keys_exact(obj, {"block1", "block2", "block3",
                                "block4"}, where):
            return
        block1_blocks_rec(obj["block1"], f"{where}.block1", with_hist)
        block2_map(obj["block2"], f"{where}.block2")
        block3_rec(obj["block3"], f"{where}.block3")
        block4_map(obj["block4"], f"{where}.block4")

    if not keys_exact(artifact, {"binding", "constants", "observed",
                                 "generated", "bootstrap", "ladder",
                                 "cost", "diagnostics"}, "top"):
        return errs

    binding = artifact["binding"]
    if keys_exact(binding, {"harness_tree_commit", "job_id",
                            "subcontract_hash",
                            "preflight_artifact_hash", "file_hashes",
                            "tape_protocol_version", "generated"},
                  "binding"):
        keys_exact(binding["generated"],
                   {"seeds", "window_start_ns", "window_length_ns",
                    "warmup"}, "binding.generated")

    keys_exact(artifact["constants"], MEASURE12A_CONSTANT_NAMES,
               "constants")

    observed = artifact["observed"]
    scoped_refusals: list[dict] = []
    if keys_exact(observed, {"per_session", "monthly",
                             "permutations_monthly"}, "observed"):
        for i, rec in enumerate(observed["per_session"]):
            where = f"observed.per_session[{i}]"
            if not keys_exact(rec, {"session_date", "segments",
                                    "block1_hist", "block2", "block3",
                                    "block4", "permutations",
                                    "refusals"}, where):
                continue
            for j, sg in enumerate(rec["segments"]):
                keys_exact(sg, {"segment_index", "open_ns",
                                "close_ns"}, f"{where}.segments[{j}]")
            for j, row in enumerate(rec["block1_hist"]):
                keys_exact(row, _B1_HIST_KEYS, f"{where}.hist[{j}]")
            block2_map(rec["block2"], f"{where}.block2")
            block3_rec(rec["block3"], f"{where}.block3")
            block4_map(rec["block4"], f"{where}.block4")
            for j, p in enumerate(rec["permutations"]):
                keys_exact(p, _PERM_KEYS, f"{where}.perm[{j}]")
            for r in rec["refusals"]:
                refusal_rec(r, f"{where}.refusals")
                scoped_refusals.append(r)
        seed_blocks(observed["monthly"], "observed.monthly",
                    with_hist=True)
        pm = observed["permutations_monthly"]
        if keys_exact(pm, set(PERMUTATION_VARIANTS),
                      "permutations_monthly"):
            for v, per in pm.items():
                for h, c in per.items():
                    keys_exact(c, {"robust_scale_60",
                                   "robust_scale_300"},
                               f"permutations_monthly[{v}][{h}]")

    generated = artifact["generated"]
    if keys_exact(generated, {"per_seed", "central"}, "generated"):
        for i, g in enumerate(generated["per_seed"]):
            where = f"generated.per_seed[{i}]"
            if not keys_exact(g, {"seed", "blocks",
                                  "count_substitution", "forensic",
                                  "cost"}, where):
                continue
            seed_blocks(g["blocks"], f"{where}.blocks",
                        with_hist=True)
            cs = g["count_substitution"]
            if keys_exact(cs, _COUNTSUB_KEYS,
                          f"{where}.count_substitution"):
                for j, rec in enumerate(cs["conditional_adequacy"]):
                    keys_exact(rec, _COND_KEYS,
                               f"{where}.cond_adequacy[{j}]")
                for r in cs["support_refusals"]:
                    refusal_rec(r, f"{where}.support_refusals")
                    scoped_refusals.append(r)
            if keys_exact(g["forensic"], {"records", "refusals"},
                          f"{where}.forensic"):
                for j, rec in enumerate(g["forensic"]["records"]):
                    keys_exact(rec, _FORENSIC_KEYS,
                               f"{where}.forensic[{j}]")
                for r in g["forensic"]["refusals"]:
                    refusal_rec(r, f"{where}.forensic.refusals")
                    scoped_refusals.append(r)
            keys_exact(g["cost"], {"walk_s", "rss_bytes"},
                       f"{where}.cost")
        central = generated["central"]
        if keys_exact(central, {"blocks", "count_substitution",
                                "pooled_diagnostic_hist"},
                      "generated.central"):
            seed_blocks(central["blocks"], "central.blocks",
                        with_hist=False)
            keys_exact(central["count_substitution"],
                       {"closure_p999_median", "refused_hour_union"},
                       "central.count_substitution")

    bootstrap = artifact["bootstrap"]
    if keys_exact(bootstrap, {"seed_rule", "replicates",
                              "per_family"}, "bootstrap"):
        per_family = bootstrap["per_family"]
        if keys_exact(per_family, set(MEASURE12A_FAMILIES),
                      "per_family"):
            for fam, env in per_family.items():
                where = f"per_family[{fam}]"
                if not keys_exact(env, {"metrics", "critical_value",
                                        "inventory_complete"}, where):
                    continue
                complete = env["inventory_complete"] is True
                if complete and env["critical_value"] is None:
                    errs.append(f"{where}: complete inventory "
                                f"without a critical value")
                if not complete and env["critical_value"] is not None:
                    errs.append(f"{where}: incomplete inventory "
                                f"with a critical value")
                for m in env["metrics"]:
                    mwhere = f"{where}.metric[{m.get('name')}]"
                    if not keys_exact(m, _METRIC_KEYS, mwhere):
                        continue
                    if m["kind"] not in ("log_ratio", "raw_diff"):
                        errs.append(f"{mwhere}: unknown kind")
                    if m["predicate"] not in ("outside", "inside",
                                              "raw_direction"):
                        errs.append(f"{mwhere}: unknown predicate")
                    if m["refused"]:
                        # An all-null refused MetricRec, distinct from
                        # predicate-irrelevant nulls.
                        if any(m[k] is not None
                               for k in _METRIC_NULLABLE):
                            errs.append(f"{mwhere}: refused metric "
                                        f"carries non-null fields")
                        continue
                    if m["predicate"] in ("outside", "inside") \
                            and m["kind"] != "log_ratio":
                        errs.append(f"{mwhere}: band predicate on "
                                    f"non-log_ratio kind")
                    # Required evidence on a non-refused metric: the
                    # point, SE and the split-rule verdicts always;
                    # the envelope fields exactly when the family
                    # inventory is complete (Amendment D).
                    if m["point"] is None or m["se"] is None \
                            or m["seed_rule_pass"] is None \
                            or m["fold_rule_pass"] is None:
                        errs.append(f"{mwhere}: non-refused metric "
                                    f"missing required evidence")
                    env_field = ("interval_inside_band"
                                 if m["predicate"] == "inside"
                                 else "envelope_excludes_edge")
                    if complete:
                        if m["interval_low"] is None \
                                or m["interval_high"] is None \
                                or m[env_field] is None:
                            errs.append(f"{mwhere}: complete family "
                                        f"with null envelope fields")
                    else:
                        if any(m[k] is not None for k in
                               ("interval_low", "interval_high",
                                "envelope_excludes_edge",
                                "interval_inside_band")):
                            errs.append(f"{mwhere}: incomplete "
                                        f"family with non-null "
                                        f"envelope fields")
                    if m["predicate"] == "raw_direction":
                        if m["kind"] != "raw_diff":
                            errs.append(f"{mwhere}: raw_direction on "
                                        f"non-raw_diff kind")
                        if m["band_low"] is not None \
                                or m["band_high"] is not None:
                            errs.append(f"{mwhere}: raw_direction "
                                        f"carries a band")
                    elif m["band_low"] is None \
                            or m["band_high"] is None:
                        errs.append(f"{mwhere}: band predicate "
                                    f"without a band")
                    if m["predicate"] in ("outside", "raw_direction"):
                        if m["interval_inside_band"] is not None \
                                or m["seed_inside_count"] is not None:
                            errs.append(f"{mwhere}: inside-only "
                                        f"evidence on an outside "
                                        f"metric")
                        if m["outside_band"] is None \
                                or m["seed_same_side_count"] is None:
                            errs.append(f"{mwhere}: outside metric "
                                        f"missing its evidence")
                    if m["predicate"] == "inside":
                        if m["outside_band"] is not None \
                                or m["seed_same_side_count"] \
                                is not None:
                            errs.append(f"{mwhere}: outside-only "
                                        f"evidence on an inside "
                                        f"metric")
                        if m["seed_inside_count"] is None:
                            errs.append(f"{mwhere}: inside metric "
                                        f"missing its evidence")

    ladder = artifact["ladder"]
    rungs = []
    if keys_exact(ladder, {"rungs", "eligible", "selected",
                           "verdict"}, "ladder"):
        rungs = ladder["rungs"]
        if [r.get("name") for r in rungs] != list(MEASURE12A_FAMILIES):
            errs.append("ladder: rungs not the six frozen names in "
                        "ladder order")
        for r in rungs:
            where = f"rung[{r.get('name')}]"
            if not keys_exact(r, _RUNG_KEYS, where):
                continue
            want = MEASURE12A_RUNG_SUBCHECKS.get(r["name"])
            if want is not None and set(r["subchecks"]) != want:
                errs.append(f"{where}: subcheck keys not the frozen "
                            f"literal set")
            for rr in r["refusals"]:
                refusal_rec(rr, f"{where}.refusals")
                scoped_refusals.append(rr)
            if not r["fired"] and (r["uniform_eligible"] is not None
                                   or r["required_resolution"]
                                   is not None):
                errs.append(f"{where}: resolution fields non-null on "
                            f"an unfired rung")
        if ladder["verdict"] not in ("family-eligible",
                                     "no-family-eligible"):
            errs.append("ladder: unknown verdict")

    keys_exact(artifact["cost"], {"observed_s", "generated_s",
                                  "bootstrap_s", "total_s",
                                  "peak_rss_bytes", "scratch_bytes"},
               "cost")

    diagnostics = artifact["diagnostics"]
    top_keys: set[tuple] = set()
    if keys_exact(diagnostics, {"warmup_exclusions", "refused_cells",
                                "empty_bins", "worsening_23"},
                  "diagnostics"):
        for k in diagnostics["warmup_exclusions"]:
            if not (isinstance(k, str) and k.isdigit()
                    and 0 <= int(k) <= 23):
                errs.append(f"warmup_exclusions: non-integer-hour "
                            f"key {k!r}")
        for r in diagnostics["refused_cells"]:
            refusal_rec(r, "refused_cells")
            top_keys.add((r.get("scope"), r.get("cell"),
                          r.get("reason")))
        if len(diagnostics["refused_cells"]) != len(top_keys):
            errs.append("refused_cells: duplicate logical refusal "
                        "records")
        for b in diagnostics["empty_bins"]:
            keys_exact(b, {"scope", "cell"}, "empty_bins")
        w23 = diagnostics["worsening_23"]
        if w23 is not None:
            keys_exact(w23, {"point", "se", "ucb"},
                       "diagnostics.worsening_23")

        # Refusal ownership in both directions: every scoped record
        # mirrors exactly one top-level record; every top-level record
        # is mirrored somewhere or is a family-envelope/metric record.
        for r in scoped_refusals:
            key = (r.get("scope"), r.get("cell"), r.get("reason"))
            if key not in top_keys:
                errs.append(f"scoped refusal {key} missing from "
                            f"refused_cells")
        mirrored = {
            (r.get("scope"), r.get("cell"), r.get("reason"))
            for r in scoped_refusals
        }
        for key in top_keys:
            scope = key[0] or ""
            if key not in mirrored and not scope.startswith("family:") \
                    and scope != "count_substitution":
                errs.append(f"top-level refusal {key} mirrored "
                            f"nowhere")

        # Amendment E truth table on the reversion rung.
        rev = next((r for r in rungs
                    if r.get("name") == "reversion"), None)
        if rev is not None and set(rev.keys()) == _RUNG_KEYS:
            w23_refs = [
                r for r in diagnostics["refused_cells"]
                if r["scope"] == "reversion"
                and r["cell"] == "worsening_23"
            ]
            if not rev["fired"]:
                if diagnostics["worsening_23"] is not None or w23_refs:
                    errs.append("Amendment E: unfired reversion rung "
                                "with a worsening_23 value or refusal")
            elif diagnostics["worsening_23"] is None:
                ok = (rev["uniform_eligible"] is None
                      and rev["required_resolution"] is None
                      and len(w23_refs) == 1)
                if not ok:
                    errs.append("Amendment E: refused worsening_23 "
                                "without null resolution fields plus "
                                "exactly one refusal record")
            else:
                if rev["uniform_eligible"] is True \
                        and rev["required_resolution"] != "uniform":
                    errs.append("Amendment E: uniform_eligible true "
                                "without uniform resolution")
                elif rev["uniform_eligible"] is False \
                        and rev["required_resolution"] \
                        != "hour-resolved":
                    errs.append("Amendment E: uniform_eligible false "
                                "without hour-resolved resolution")
                elif rev["uniform_eligible"] is None:
                    errs.append("Amendment E: measured worsening_23 "
                                "without a Boolean uniform_eligible")
                if w23_refs:
                    errs.append("Amendment E: measured worsening_23 "
                                "beside a worsening_23 refusal")
    return errs


def _fresh_tree_state() -> tuple[str, bool]:
    """(HEAD, clean) read FRESH from git - never the cached
    git_commit() - so a HEAD move or a new dirty file DURING the run is
    caught at the final gate before the artifact writes."""
    status = subprocess.run(["git", "status", "--porcelain"],
                            capture_output=True, text=True, cwd=ROOT)
    head = subprocess.run(["git", "rev-parse", "HEAD"],
                          capture_output=True, text=True, cwd=ROOT)
    clean = (status.returncode == 0 and status.stdout.strip() == ""
             and head.returncode == 0)
    return head.stdout.strip(), clean


class _ResourceSampler:
    """1 s background sampling of this process tree's RSS (the walk
    subprocesses are children, so the tree covers them) and the
    measure12a on-disk scratch footprint including replay temporaries;
    peaks retained. Runs across the WHOLE mode including artifact
    serialization (the json_safe copy is a late memory peak)."""

    def __init__(self):
        self.peak_rss = 0
        self.peak_scratch = 0
        self.failure: str | None = None
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def start(self):
        self._thread.start()
        return self

    @staticmethod
    def _scratch_bytes() -> int:
        # Replay temporaries appear and vanish between listdir and
        # getsize: that race is tolerated per file, never fatal.
        total = 0
        paths = [MEASURE12A_OBSERVED_CACHE]
        if os.path.isdir(MEASURE12A_CACHE_DIR):
            paths.extend(
                os.path.join(MEASURE12A_CACHE_DIR, name)
                for name in os.listdir(MEASURE12A_CACHE_DIR)
            )
        for path in paths:
            try:
                total += os.path.getsize(path)
            except OSError:
                continue
        return total

    def sample(self) -> None:
        self.peak_rss = max(self.peak_rss, _tree_rss_bytes(os.getpid()))
        self.peak_scratch = max(self.peak_scratch,
                                self._scratch_bytes())

    def _run(self) -> None:
        try:
            while not self._stop.is_set():
                self.sample()
                self._stop.wait(1.0)
        except Exception as exc:  # noqa: BLE001 - any death voids the run
            self.failure = repr(exc)

    def stop(self) -> None:
        """A dead sampler VOIDS the cost attestation: the run refuses
        rather than reporting a peak measured over a partial window."""
        self._stop.set()
        self._thread.join()
        if self.failure is not None:
            raise Refusal(f"the resource sampler died: {self.failure}")
        self.sample()


def load_brick_g_walks() -> dict[int, dict]:
    """READ-ONLY index of the Brick G walk cache, grouped by seed. The
    cache keys embed the commit that produced them, so a later commit
    cannot re-derive the file names; the attestation instead loads
    every committed record and requires the eight seeds EXACTLY once
    each. An absent or ambiguous cache refuses - Brick M never fills
    the reference cache itself (that would compare a replay against a
    same-run twin instead of the Brick G record)."""
    if not os.path.isdir(MEASURE12A_CACHE_DIR):
        raise Refusal(
            "no Brick G walk cache exists; the Brick G walks must land "
            "before Brick M runs"
        )
    by_seed: dict[int, dict] = {}
    for name in sorted(os.listdir(MEASURE12A_CACHE_DIR)):
        if not name.endswith(".json") or ".tmp" in name:
            continue
        with open(os.path.join(MEASURE12A_CACHE_DIR, name)) as fh:
            record = json.load(fh)
        if not isinstance(record, dict):
            raise Refusal(f"Brick G cache record {name} is not an object")
        seed = record.get("seed")
        if not _strict_int(seed):
            raise Refusal(
                f"Brick G cache record {name} carries a non-integer "
                f"seed {seed!r}"
            )
        if seed in by_seed:
            raise Refusal(
                f"ambiguous Brick G cache: seed {seed} appears in more "
                f"than one record"
            )
        by_seed[seed] = record
    if sorted(by_seed) != list(range(1, 9)):
        raise Refusal(
            f"the Brick G cache carries seeds {sorted(by_seed)}, "
            f"not 1..8"
        )
    return by_seed


def replay_measure12a_walk(seed: int, attested: dict) -> dict:
    """The Brick M cost-attestation replay: one FRESH walk under the
    external sampler, content-compared (cost fields excluded - they are
    live measurements) against the read-only Brick G record `attested`;
    ANY divergence refuses. The replayed record is the authoritative
    artifact input - no selection among outputs."""
    out_path = os.path.join(MEASURE12A_CACHE_DIR,
                            f"replay-{seed}.tmp.json")
    if os.path.exists(out_path):
        os.remove(out_path)
    cmd = gen_command_prefix() + [
        "gen", "--symbol", "MNQ", "--type", "measure12a",
        "--seed", str(seed), "--start", str(FINAL_START_NS),
        "--length", FINAL_LENGTH, "--warmup", SUMMARY_WARMUP,
        "--out", out_path,
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True, cwd=ROOT)
    if proc.returncode != 0:
        raise Refusal(
            f"measure12a replay failed ({' '.join(cmd)}):\n"
            f"{proc.stderr[-2000:]}"
        )
    with open(out_path) as fh:
        replay = json.load(fh)
    os.remove(out_path)
    replayed = {k: v for k, v in replay.items() if k != "cost"}
    reference = {k: v for k, v in attested.items() if k != "cost"}
    if _typed_canon(replayed) != _typed_canon(reference):
        raise Refusal(
            f"seed {seed} replay diverges from the cached Brick G walk"
        )
    return replay


def _typed_canon(x) -> str:
    """TYPE-STRICT canonical serialization for equality gates: Python
    equality treats 1 == True == 1.0, so a JSON mutation from 1 to true
    would escape a plain != - every leaf carries an explicit type tag
    and floats compare by repr."""
    def tag(node):
        if isinstance(node, bool):
            return ["b", node]
        if isinstance(node, int):
            return ["i", node]
        if isinstance(node, float):
            return ["f", repr(node)]
        if isinstance(node, str):
            return ["s", node]
        if node is None:
            return ["n"]
        if isinstance(node, list):
            return ["l", [tag(v) for v in node]]
        if isinstance(node, dict):
            return ["d", sorted((str(k), tag(v))
                                for k, v in node.items())]
        return ["x", repr(node)]
    return json.dumps(tag(x))


def _strict_int(v) -> bool:
    return isinstance(v, int) and not isinstance(v, bool)


def _strict_number(v) -> bool:
    return isinstance(v, (int, float)) and not isinstance(v, bool)


def _nonfinite_paths(node, path: str = "artifact") -> list[str]:
    out: list[str] = []
    if isinstance(node, dict):
        for k, v in node.items():
            out.extend(_nonfinite_paths(v, f"{path}.{k}"))
    elif isinstance(node, list):
        for i, v in enumerate(node):
            out.extend(_nonfinite_paths(v, f"{path}[{i}]"))
    elif isinstance(node, float) and not math.isfinite(node):
        out.append(path)
    return out


MEASURE12A_BUDGETS_S = {
    "observed_s": 2 * 3600, "generated_s": 10 * 3600,
    "bootstrap_s": 2 * 3600, "total_s": 12 * 3600,
}
MEASURE12A_RSS_BUDGET = 4 << 30
MEASURE12A_SCRATCH_BUDGET = 20 << 30


def measure12a_semantic_errors(artifact: dict,
                               usable: list[str]) -> list[str]:
    """The Brick M semantic gates beyond the exact-key validator:
    population cardinality, exact metric inventories, monthly
    reconstruction, ladder coherence, finite numerics, and the cost
    contract."""
    errs: list[str] = []
    obs = artifact["observed"]
    dates = [r["session_date"] for r in obs["per_session"]]
    if not all(isinstance(d, str) for d in dates):
        errs.append("observed session dates carry non-string values")
    elif not all(isinstance(d, str) for d in usable):
        errs.append(f"the preflight usable list carries non-string "
                    f"entries: {usable!r}")
    elif dates != sorted(usable) or dates != sorted(set(dates)) \
            or len(dates) != 22:
        errs.append(f"observed sessions {len(dates)} do not equal the "
                    f"preflight usable list")
    seeds = [g["seed"] for g in artifact["generated"]["per_seed"]]
    if not all(_strict_int(s) for s in seeds) \
            or seeds != list(range(1, 9)):
        errs.append(f"generated seeds {seeds!r} are not the strict "
                    f"integers 1..8")

    # Monthly blocks and permutations_monthly must reconstruct exactly
    # from the per-session records - type-strictly (1 vs true vs 1.0
    # are distinct).
    def canon(x):
        return _typed_canon(json_safe(x))

    pooled = pool_block1_hists(
        [r["block1_hist"] for r in obs["per_session"]]
    )
    rebuilt = {
        "block1": block1_blocks(pooled),
        "block2": pool_block2([r["block2"] for r in obs["per_session"]]),
        "block3": aggregate_block3(
            [r["block3"] for r in obs["per_session"]]
        ),
        "block4": aggregate_block4(
            [r["block4"] for r in obs["per_session"]]
        ),
    }
    if canon(rebuilt) != canon(obs["monthly"]):
        errs.append("observed monthly does not reconstruct from "
                    "per_session")
    if canon(aggregate_permutations(
        [r["permutations"] for r in obs["per_session"]]
    )) != canon(obs["permutations_monthly"]):
        errs.append("permutations_monthly does not reconstruct from "
                    "per_session")

    # Exact family metric inventories, names and order.
    cond = artifact["generated"]["per_seed"][0]["count_substitution"][
        "conditional_adequacy"]
    expected = {
        "child_walk": [f"print_excess_h{h}" for h in FAIL_HOURS_300] + [
            f"quote_robust_{w}_h{h}"
            for h in FAIL_HOURS_300 for w in (60, 300)
        ],
        "arrival": [f"fano_60_h{h}" for h in FAIL_HOURS_300]
        + [f"count_p99_60_h{h}" for h in FAIL_HOURS_300]
        + [f"cond_sqrtn_p99_h{r['hour']}_{r['bin_name']}"
           for r in cond if r["required"]],
        "innovation": [f"tail_ratio_h{h}" for h in FAIL_HOURS_300]
        + ["tail_ratio_all"],
        "reversion": ["robust_300_h19", "robust_300_h20",
                      "robust_60_h20", "covnorm_h19", "covnorm_h20"],
        "garch": ["robust_300_h19", "robust_300_h20", "robust_60_h20"],
        "boundary": [
            f"{stem}_{case}{suffix}"
            for case in ("pre_halt_close", "post_halt_reopen")
            for suffix in ("", "_comparator")
            for stem in ("quote_p99", "robust_60")
        ],
    }
    for fam, names in expected.items():
        got = [m["name"] for m in
               artifact["bootstrap"]["per_family"][fam]["metrics"]]
        if got != names:
            errs.append(f"family {fam} inventory {got} is not the "
                        f"frozen {names}")

    # Ladder coherence.
    ladder = artifact["ladder"]
    fired = [r["name"] for r in ladder["rungs"] if r["fired"]]
    if ladder["eligible"] != fired:
        errs.append("eligible does not equal the fired rungs in order")
    selected = fired[0] if fired else None
    if ladder["selected"] != selected:
        errs.append("selected is not the first eligible or null")
    verdict = "family-eligible" if fired else "no-family-eligible"
    if ladder["verdict"] != verdict:
        errs.append("verdict disagrees with the fired rungs")

    # Finite numerics: a non-finite float would serialize as a string
    # AFTER validation, so it must refuse here.
    errs.extend(f"non-finite value at {p}"
                for p in _nonfinite_paths(artifact)[:8])

    # The cost contract: finite, nonnegative, arithmetically
    # consistent (EXACT phase sum), within every budget.
    cost = artifact["cost"]
    for key, bound in MEASURE12A_BUDGETS_S.items():
        v = cost[key]
        if not (_strict_number(v) and math.isfinite(v) and v >= 0):
            errs.append(f"cost.{key} is not a nonnegative finite number")
        elif v > bound:
            errs.append(f"cost.{key} {v:.1f}s breaches the {bound}s "
                        f"budget")
    if not all(_strict_number(cost[k]) for k in MEASURE12A_BUDGETS_S) \
            or cost["total_s"] != (cost["observed_s"]
                                   + cost["generated_s"]
                                   + cost["bootstrap_s"]):
        errs.append("cost.total_s is not the exact sum of its phases")
    for key, bound in (("peak_rss_bytes", MEASURE12A_RSS_BUDGET),
                       ("scratch_bytes", MEASURE12A_SCRATCH_BUDGET)):
        v = cost[key]
        if not (_strict_int(v) and v >= 0):
            errs.append(f"cost.{key} is not a nonnegative strict "
                        f"integer")
        elif v > bound:
            errs.append(f"cost.{key} {v} breaches the {bound} budget")
    return errs


def mode_measure12a() -> None:
    harness_commit = require_clean_tree()
    # The Brick G references load READ-ONLY before anything runs: an
    # absent or ambiguous walk cache refuses up front.
    brick_g = load_brick_g_walks()
    with open(PREFLIGHT_ARTIFACT) as fh:
        usable = json.load(fh)["usable_sessions"]
    sampler = _ResourceSampler().start()
    # Observed pass, LIVE (the authoritative input): the pre-existing
    # observed cache carries no commit binding, so it serves as a
    # MANDATORY structural cross-check - absence or divergence refuses.
    t0 = time.monotonic()
    observed = run_measure12a_observed()
    observed_s = time.monotonic() - t0
    live = json.loads(json.dumps(json_safe(observed)))
    if not os.path.exists(MEASURE12A_OBSERVED_CACHE):
        raise Refusal(
            "no cached observed half to cross-check against; the "
            "Brick G observed pass must exist"
        )
    with open(MEASURE12A_OBSERVED_CACHE) as fh:
        cached_obs = json.load(fh)
    if _typed_canon(cached_obs) != _typed_canon(live):
        raise Refusal(
            "the live observed pass diverges from the cached "
            "observed half"
        )
    write_json_atomic(MEASURE12A_OBSERVED_CACHE, observed)
    print(f"observed pass: {observed_s:.1f}s")
    # The eight FINAL walks as cost-attestation replays under the
    # sampler (Brick M ruling: cached VmHWM figures cannot stand in for
    # the external process-tree measurement).
    t1 = time.monotonic()
    generated_seeds = []
    for seed in range(1, 9):
        record = replay_measure12a_walk(seed, brick_g[seed])
        generated_seeds.append(record)
        print(f"seed {seed} replay attested: "
              f"{len(record['per_session'])} complete sessions")
    generated_s = time.monotonic() - t1
    # Input-side population gates.
    for g in generated_seeds:
        g_dates = [r["session_date"] for r in g["per_session"]]
        if not all(isinstance(d, str) for d in g_dates):
            raise Refusal(
                f"seed {g['seed']} carries non-string session dates"
            )
        if g_dates != sorted(set(g_dates)) or len(g_dates) != 23:
            raise Refusal(
                f"seed {g['seed']} carries {len(g_dates)} sessions, "
                f"not 23 sorted unique"
            )
    if len({tuple(r["session_date"] for r in g["per_session"])
            for g in generated_seeds}) != 1:
        raise Refusal("the generated seeds disagree on session dates")
    # Assembly with a provisional MUTABLE cost record (two-phase: the
    # bootstrap clock stops after assembly, then the fields finalize
    # in place before validation).
    t2 = time.monotonic()
    mults = bootstrap_multiplicities(len(usable))
    cost = {"observed_s": observed_s, "generated_s": generated_s,
            "bootstrap_s": 0.0, "total_s": 0.0,
            "peak_rss_bytes": 0, "scratch_bytes": 0}
    artifact = assemble_measure12a_artifact(
        observed, generated_seeds,
        {"harness_tree_commit": harness_commit,
         "generated": {"seeds": list(range(1, 9)),
                       "window_start_ns": FINAL_START_NS,
                       "window_length_ns": FINAL_END_NS
                       - FINAL_START_NS,
                       "warmup": SUMMARY_WARMUP}},
        mults, cost,
    )
    # A throwaway serialization pass realizes the late json_safe memory
    # peak while the sampler still runs and BEFORE the cost freezes.
    json.dumps(json_safe(artifact))
    cost["bootstrap_s"] = time.monotonic() - t2
    cost["total_s"] = (cost["observed_s"] + cost["generated_s"]
                       + cost["bootstrap_s"])
    sampler.stop()
    cost["peak_rss_bytes"] = sampler.peak_rss
    cost["scratch_bytes"] = sampler.peak_scratch
    errs = measure12a_schema_errors(artifact)
    errs.extend(measure12a_semantic_errors(artifact, usable))
    if errs:
        raise Refusal(
            "the measure12a artifact violates the contract: "
            + "; ".join(errs[:10])
        )
    head, clean = _fresh_tree_state()
    if not clean or head != harness_commit:
        raise Refusal(
            "the tree changed during the measure12a run; the artifact "
            "is unbound"
        )
    write_json_atomic(MEASURE12A_ARTIFACT, artifact)
    print(f"artifact -> {MEASURE12A_ARTIFACT}")
    print(f"cost: {json.dumps(cost)}")
    print(f"eligible: {artifact['ladder']['eligible']}")
    print(f"selected: {artifact['ladder']['selected']}")
    print(f"verdict: {artifact['ladder']['verdict']}")


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
            pooled_warp = distort.get("walltime_pooled_warp", 1.0)
            wt_scale = {
                60: dense_wt["60"]["pooled_rms"] * vol
                / (target_mid_rms / 10.0) * pooled_warp,
                300: dense_wt["300"]["pooled_rms"] * vol
                / (target_mid_rms / 10.0) * pooled_warp,
            }
            # An hour-local wall-time warp fails the hourly contour while
            # leaving the pooled RMS inside its tolerance - the amended
            # landing predicate's deciding case.
            hour_warp = distort.get("walltime_hour_warp", 1.0)
            wt_h = [hour_warp if h == 2 else 1.0 for h in range(24)]
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
                        else hz_cell(wt_scale[60] * vh[h] * wt_h[h],
                                     42 if h == 20 else 59)
                        for h in range(24)
                    ],
                    "horizon_300_by_hour": [
                        hz_cell(0.0, 0) if h == 21
                        else hz_cell(wt_scale[300] * vh[h] * wt_h[h],
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
          and "session landing gate"
          in artifact["verdicts"]["vol_hour"]["reason"])
    artifact, _state = driver({"walltime_hour_warp": 1.6})
    check("an hourly wall-time failure with passing pooled gates still "
          "lands the arrays as a recorded diagnostic",
          artifact["landing_set"]
          == ["intensity_hour", "vol_hour", "vol_scalar"]
          and artifact["landing_rule"]["walltime_hourly_ok"] is False
          and artifact["landing_rule"]["walltime_pooled_ok"] is True
          and artifact["landing_rule"]["arrays_land"] is True
          and artifact["session_refit"]["verdicts"]["session_walltime_60"]
          [0]["pass"] is False
          and artifact["session_refit"]["verdicts"]["session_walltime_60"]
          [0]["worst_hour"] == 2)
    artifact, _state = driver({"walltime_pooled_warp": 1.5})
    check("a pooled wall-time failure blocks the atomic group",
          artifact["landing_set"] == []
          and artifact["landing_rule"]["walltime_pooled_ok"] is False
          and artifact["landing_rule"]["arrays_land"] is False
          and "session landing gate"
          in artifact["verdicts"]["intensity_hour"]["reason"])
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

    # -- Protocol 12a (notes/protocol-12a-measurement-spec.md) ----------

    print("12a deterministic derivations")
    check("splitmix64 matches the reference vector",
          splitmix64(0) == 0xE220A8397B1DCDAF
          and splitmix64(1) == 0x910A2DEC89025CC1)
    check("tuple_mix folds fields in listed order",
          tuple_mix(7, [1, 2]) == splitmix64(splitmix64(7 ^ 1) ^ 2)
          and tuple_mix(7, [2, 1]) != tuple_mix(7, [1, 2]))
    vals = [10, 20, 30, 40, 50]
    fisher_yates(vals, 42)
    check("the frozen Fisher-Yates is reproducible",
          vals == [50, 20, 30, 10, 40])
    mults_full = bootstrap_multiplicities(22)
    check("bootstrap draws: 10000 replicates of exactly 22 sessions",
          len(mults_full) == BOOTSTRAP_REPLICATES
          and all(sum(mu) == 22 for mu in mults_full))
    check("bootstrap draws are bit-reproducible",
          mults_full[0] == bootstrap_multiplicities(22)[0]
          and mults_full[0] == [1, 1, 2, 2, 2, 1, 1, 0, 0, 1, 2, 1, 1,
                                1, 2, 1, 1, 1, 1, 0, 0, 0])

    print("12a aggregation conventions")
    check("weighted nearest rank follows the literal 5.2 rule",
          weighted_nearest_rank([(5, 1), (1, 3)], 0.5) == 1
          and weighted_nearest_rank([(5, 1), (1, 3)], 0.9) == 5
          and weighted_nearest_rank([], 0.5) is None)
    check("median_or_none takes the ceil(n/2)-th order statistic",
          median_or_none([3, 1, 2]) == 2
          and median_or_none([4, 1, 2, 3]) == 2
          and median_or_none([None, None]) is None)
    qs = QuantileSupport([[(1, 3), (5, 2)], [(2, 4), (5, 1)]])
    check("QuantileSupport matches direct pooled quantiles",
          qs.quantile(0.5, [1, 1]) ==
          weighted_nearest_rank([(1, 3), (5, 2), (2, 4), (5, 1)], 0.5)
          and qs.quantile(0.99, [1, 0]) == 5)
    check("tree_median is strict: identical shapes, any-None nulls",
          tree_median([{"a": 1}, {"a": 3}, {"a": 2}]) == {"a": 2}
          and tree_median([{"a": 1}, {"a": None}]) == {"a": None}
          and refuses(
              lambda: tree_median([{"a": 1}, {"b": 2}]),
              "diverge in shape",
          ))

    print("12a segment labels and bins")
    check("parent-count bins are exact half-open intervals with zero",
          [parent_count_bin(n) for n in (0, 1, 64, 65, 256, 257, 4096,
                                         4097)]
          == ["0", "1-64", "1-64", "65-256", "65-256", "257-1024",
              "1025-4096", "4097+"])
    origin, end = 0, 3_600_000_000_000
    check("segment labels evaluate at minute start on both axes",
          segment_labels(0, origin, end) == ("0-300", "1800+")
          and segment_labels(299 * 10**9, origin, end)
          == ("0-300", "1800+")
          and segment_labels(300 * 10**9, origin, end)
          == ("300-1800", "1800+")
          and segment_labels(3_400 * 10**9, origin, end)
          == ("1800+", "0-300"))

    print("12a evidence blocks on a crafted session")
    m12a_rows = []
    st_base = int(dt.datetime(
        2026, 7, 7, 0, 0, 0, tzinfo=dt.timezone.utc
    ).timestamp())
    px0 = 23_000 * PRICE_UNITS_PER_POINT
    px = px0
    for k in range(6000):
        t_ns = (st_base + k * 2) * 10**9
        if k % 7 == 0:
            px += TICK_UNITS if (k // 7) % 2 == 0 else -TICK_UNITS
        side = "B" if k % 2 == 0 else "A"
        if k % 97 == 0:
            side = "N"
        m12a_rows.append(Row(
            t_ns, "1", side, px, 1, px - TICK_UNITS, px + TICK_UNITS,
            3, 3, classify_book(px - TICK_UNITS, px + TICK_UNITS),
        ))
    m12a_recs = measure12a_observe(iter(m12a_rows), ["2026-07-07"])
    m12a_rec0 = m12a_recs[0]
    check("the session record covers exactly the usable set",
          [r["session_date"] for r in m12a_recs] == ["2026-07-07"])
    hist_minutes = sum(r["count"] for r in m12a_rec0["block1_hist"])
    check("block 1 counts every populated minute exactly once",
          hist_minutes == 200)  # 6000 rows * 2s = 200 minutes
    n_total = sum(
        r["n"] * r["count"] for r in m12a_rec0["block1_hist"]
    )
    check("block 1 parent counts sum to the sided parents",
          n_total == sum(
              len(g) > 0 for g in group_parents_batch(m12a_rows)
          ))
    b2c = m12a_rec0["block2"]["0"]["60"]
    check("block 2 schedules 59 qualified 60s windows in a full hour",
          # The window ending exactly ON the hour boundary is
          # hour-crossing by endpoint attribution and excluded.
          b2c["scheduled_windows"] == 59
          and b2c["paired_lag_count"] == 58)
    b2c1 = m12a_rec0["block2"]["0"]["1"]
    check("block 2 includes zero-count windows in the schedule",
          b2c1["scheduled_windows"] == 3599
          and b2c1["zero_windows"] > 0)
    check("block 2 per-session cells carry the derived fields",
          "fano" in b2c and "count_p99" in b2c
          and "zero_fraction" in b2c)
    b2c20 = m12a_rec0["block2"]["20"]["60"]
    check("an empty calendar segment still schedules its windows",
          # Hour 20: 15 overnight windows (20:00-20:15, the segment-end
          # window included - a segment end is not an hour boundary)
          # plus 29 post-halt (20:30-21:00, the 21:00 window excluded),
          # every one zero-count in this early-hours fixture.
          b2c20["scheduled_windows"] == 44
          and b2c20["zero_windows"] == 44)
    c60 = m12a_rec0["block3"]["cells"]["1"]["60"]
    check("block 3 hour cells carry the serialized Block3Cell",
          c60["return_count"] == 60 - 1
          and c60["robust_scale"] is not None
          and c60["rms_scale"] is not None)
    pair = m12a_rec0["block3"]["pairs"]["1"]["60-300"]
    check("block 3 covariance windows require all components",
          pair["window_count"] > 0
          and pair["vr"] is not None
          and pair["vr"] < 1.0)  # reverting fixture
    check("block 4 warmup-excludes a history below 1000 returns",
          m12a_rec0["block4"]["0"]["warmup_excluded"] > 0
          and m12a_rec0["block4"]["0"]["residual_count"] == 0)
    check("block 4 carries the pooled all-hours cell",
          "all" in m12a_rec0["block4"])
    # Amendment F: a flat tape drives the trailing scale to zero once
    # the 1000-return history fills; the residuals are OMITTED (the
    # returns stay in history) and come back as one standardizer
    # RefusalRec per (session, hour) - never a raised Refusal.
    flat_rows = [
        Row((st_base * 10**9) + k * 200_000_000, "1",
            "B" if k % 2 == 0 else "A", px0, 1,
            px0 - TICK_UNITS, px0 + TICK_UNITS, 3, 3,
            classify_book(px0 - TICK_UNITS, px0 + TICK_UNITS))
        for k in range(2400)
    ]
    flat_rec = measure12a_observe(iter(flat_rows), ["2026-07-07"])[0]
    check("a nonpositive standardizer scale omits and records",
          any("standardizer" in r["cell"] for r in flat_rec["refusals"])
          and all(sorted(r.keys()) == ["cell", "reason", "scope"]
                  for r in flat_rec["refusals"])
          and flat_rec["block4"]["0"]["residual_count"] == 0
          and flat_rec["block4"]["0"]["warmup_excluded"] > 0)

    print("12a permutation invariants")
    perm_recs = m12a_rec0["permutations"]
    check("both variants and all 16 replicates are present",
          {p["variant"] for p in perm_recs}
          == set(PERMUTATION_VARIANTS)
          and {p["replicate"] for p in perm_recs}
          == set(range(PERMUTATION_REPLICATES)))
    check("permutation records are bit-reproducible",
          measure12a_observe(iter(m12a_rows), ["2026-07-07"])[0][
              "permutations"] == perm_recs)
    check("permutation records carry the Amendment-A statistics",
          all(
              f"{f}_{h}" in perm_recs[0]
              for f in ("return_count", "sum_abs", "max_abs")
              for h in (60, 300)
          ))
    check("the identity window-sum reproduces the statistics exactly",
          _windows_stats([1.0, -1.0, 2.0], [[0, 1], [2]])
          == (2, 2.0, 2.0))
    values = [0.0, 1.5, -2.0, 0.0, 0.5]
    nz = [i for i, v in enumerate(values) if v != 0.0]
    perm = list(nz)
    fisher_yates(perm, 7)
    sign_shuffled = list(values)
    signs = [1.0 if values[p] > 0 else -1.0 for p in perm]
    for k, p in enumerate(nz):
        sign_shuffled[p] = abs(values[p]) * signs[k]
    check("the sign shuffle preserves magnitudes and zero locations",
          [abs(v) for v in sign_shuffled] == [abs(v) for v in values]
          and [i for i, v in enumerate(sign_shuffled) if v == 0.0]
          == [0, 3])
    mag_shuffled = list(values)
    mags = [abs(values[p]) for p in perm]
    for k, p in enumerate(nz):
        sgn = 1.0 if values[p] > 0 else -1.0
        mag_shuffled[p] = sgn * mags[k]
    check("the magnitude shuffle preserves the sign sequence",
          [v > 0 for v in mag_shuffled if v != 0.0]
          == [v > 0 for v in values if v != 0.0]
          and sorted(abs(v) for v in mag_shuffled)
          == sorted(abs(v) for v in values))

    print("12a counterfactual mechanics")
    check("gap closure is exact on both sides",
          gap_closure(4.0, 2.0, 2.0, generated_side=True) == 1.0
          and gap_closure(4.0, 4.0, 2.0, generated_side=False) == 1.0
          and gap_closure(4.0, 3.0, 2.0, generated_side=True)
          == (math.log(4) - math.log(3)) / (math.log(4) - math.log(2)))
    check("gap closure refuses a vanished denominator and nonpositives",
          gap_closure(2.0, 1.5, 2.0, generated_side=True) is None
          and gap_closure(0.0, 1.0, 2.0, generated_side=True) is None
          and gap_closure(2.0, None, 1.0, generated_side=True) is None)
    obs_shares_fx = {13: {"1-64": 1.0}}
    gen_hist_fx = {
        (10, 4, 10, 13, "1800+", "1800+"): 50,
        (5000, 4, 100, 13, "1800+", "1800+"): 50,
    }
    csub_fx = count_substitution(gen_hist_fx, obs_shares_fx)
    check("count substitution zeroes observed-empty generated bins",
          csub_fx["counterfactual_p999"] == 10
          and csub_fx["weights"]["13"]["4097+"] is not None
          and csub_fx["refused_hours"] == [])
    csub_refuse = count_substitution(
        {(10, 4, 10, 13, "1800+", "1800+"): 50},
        {13: {"1-64": 0.5, "4097+": 0.5}},
    )
    check("observed support without generated support refuses the hour",
          csub_refuse["refused_hours"] == [13]
          and len(csub_refuse["support_refusals"]) == 1
          and csub_refuse["counterfactual_p999"] is None)

    print("12a ladder verdict cases")

    def suff(n, a):
        return {"count": n, "sum": 0.0, "sumsq": n * a * a,
                "sum_abs": n * a, "max_abs": a}

    m12a_dates = [
        label for label, status in SESSION_INVENTORY if status == "full"
    ]

    def fake_rec(date, i, *, robust=1.0, sign_perm=None, mag_perm=None,
                 cov=0.0, fano_d=3, tail=2.0, print_excess=1.0,
                 boundary_quote=10, comp_quote=10, minutes=None,
                 h23_robust=None, robust_jitter=True):
        """One schema-complete per-session record with analytic knobs.
        `i` jitters most statistics so bootstrap SEs exist (quantile
        carriers vary across sessions); hour 23 stays unjittered so the
        worsening statistic can be pinned exactly. robust_jitter=False
        pins the robust cells session-constant, so their metrics refuse
        on zero bootstrap SE while the closures stay computable - the
        Amendment-D completeness-gate regressions need exactly that."""
        j = 1.0 + 0.01 * ((i % 5) - 2)
        rj = j if robust_jitter else 1.0
        rows = []
        # Fail-hour minutes for the conditional guard and bin counts:
        # bin 1-64 (N=10), small trade ranges so the excess carrier
        # owns the p99.
        for h in FAIL_HOURS_300:
            rows.append({
                "n": 10, "quote_range_half_ticks": 16,
                "trade_range_ticks": 5 + (i % 3), "hour": h,
                "since_open_bin": "1800+", "until_close_bin": "1800+",
                "count": 40,
            })
        # Print-excess carrier at every fail hour: quote p99 stays 16
        # half-ticks = 8 ticks. The trade value is UNIQUE per session
        # (i mod 22) so the pooled p99 moves with the resampled session
        # mix - a discrete quantile whose top value is shared by many
        # sessions has zero bootstrap variance and refuses its metric.
        for h in FAIL_HOURS_300:
            rows.append({
                "n": 20,
                "quote_range_half_ticks": 16,
                "trade_range_ticks":
                    max(1, round(print_excess * 8)) * 3 + (i % 22),
                "hour": h, "since_open_bin": "1800+",
                "until_close_bin": "1800+", "count": 20000,
            })
        # The carrier mass dwarfs the label rows below so the pooled
        # hour-20 quote p99 stays the carrier's 16 half-ticks - the
        # boundary cells influence only their label-filtered quantiles.
        # Boundary and comparator cells (quote ranges in half-ticks),
        # session-unique for the same reason.
        rows.append({
            "n": 5,
            "quote_range_half_ticks": boundary_quote * 2 + (i % 22),
            "trade_range_ticks": 5, "hour": 20,
            "since_open_bin": "1800+", "until_close_bin": "0-300",
            "count": 40,
        })
        rows.append({
            "n": 5,
            "quote_range_half_ticks": comp_quote * 2 + (i % 22),
            "trade_range_ticks": 5, "hour": 20,
            "since_open_bin": "1800+", "until_close_bin": "300-1800",
            "count": 40,
        })
        rows.append({
            "n": 5, "quote_range_half_ticks": 20 + (i % 22),
            "trade_range_ticks": 5,
            "hour": 20, "since_open_bin": "0-300",
            "until_close_bin": "300-1800", "count": 40,
        })
        rows.append({
            "n": 5, "quote_range_half_ticks": 20 + (i % 22),
            "trade_range_ticks": 5,
            "hour": 20, "since_open_bin": "300-1800",
            "until_close_bin": "300-1800", "count": 40,
        })
        if minutes:
            rows.extend(minutes)
        # A one-mass session-unique top value gives the pooled count
        # p99 bootstrap variance without disturbing mean or Fano.
        count_hist = {30 - fano_d: 30, 30 + fano_d: 29,
                      30 + fano_d + (i % 22) + 1: 1}

        # The scheduled counts come from the session's own CALENDAR
        # (defect-4 exposure completeness): 59 per full hour at 60 s,
        # 44 at the halt hour; hours the calendar never schedules get
        # no cell.
        def b2_cell_for(exp):
            return finish_block2_cell({
                "scheduled_windows": exp, "zero_windows": 0,
                "count_hist": dict(count_hist),
                "run_length_hist": {60: 1},
                "paired_lag_count": 59, "sum_x": 0, "sum_y": 0,
                "sumsq_x": 0, "sumsq_y": 0, "sum_xy": 0,
            })

        block2: dict[str, dict] = {}
        for h in range(24):
            for w in COUNT_WINDOWS_S:
                exp = expected_scheduled_windows(date, h, w)
                if exp:
                    block2.setdefault(str(h), {})[str(w)] = \
                        b2_cell_for(exp)

        def b3cell(n, a):
            return {"return_count": n, "robust_scale": a,
                    "rms_scale": a}

        cells = {}
        pairs = {}
        for h in range(24):
            r = robust * rj
            if h == 23 and h23_robust is not None:
                r = h23_robust
            cells[str(h)] = {
                "60": b3cell(60, r),
                "300": b3cell(12, r),
            }
            cn = cov * (1.0 + 0.002 * (i % 5))
            pairs[str(h)] = {"60-300": {
                "window_count": 12,
                "vr": 1.0 / (1.0 - cn) if cn != 1.0 else None,
                "cov_contrib": cn / 12,
                "cov_contrib_norm": cn,
            }}
        h20 = {
            lp: {"60": b3cell(10, robust * rj)}
            for lp in ("1800+|0-300", "0-300|300-1800",
                       "1800+|300-1800", "300-1800|300-1800")
        }
        b4 = {}
        for key in [str(h) for h in range(24)] + ["all"]:
            b4[key] = {
                "residual_count": 2000, "warmup_excluded": 0,
                "zero_fraction": 0.4,
                "nz_abs_p90": 2.0, "nz_abs_p99": 4.0,
                "nz_abs_p999": 4.0 * tail * j,
                "ratio_p99_p90": 2.0,
                "ratio_p999_p99": tail * j,
                "exceed_4": 0.01, "exceed_8": 0.001,
                "exceed_16": 0.0001,
            }
        perms = []
        for variant in PERMUTATION_VARIANTS:
            pv = sign_perm if variant == "sign" else mag_perm
            for rep in range(PERMUTATION_REPLICATES):
                for h in list(range(19, 24)):
                    v = (pv if pv is not None else robust) * rj
                    if h == 23:
                        v = (pv if pv is not None else robust) \
                            if h23_robust is None else h23_robust
                    # Amendment A sufficient statistics: every window
                    # return equal to v gives robust exactly v.
                    perms.append({
                        "segment_index": 0, "hour": h,
                        "variant": variant, "replicate": rep,
                        "return_count_60": 60,
                        "sum_abs_60": 60 * v, "max_abs_60": v,
                        "return_count_300": 12,
                        "sum_abs_300": 12 * v, "max_abs_300": v,
                    })
        return {
            "session_date": date,
            "segments": [],
            "block1_hist": rows,
            "block2": block2,
            "block3": {"cells": cells, "pairs": pairs,
                       "lag1_parent_autocorr": {},
                       "hour20_labels": h20},
            "block4": b4,
            "permutations": perms,
            "refusals": [],
        }

    def fake_ctxs(obs_kw: dict, gen_kw: dict):
        obs = ObsContext([
            fake_rec(d, i, **obs_kw) for i, d in enumerate(m12a_dates)
        ])
        gens = [
            ObsContext([
                fake_rec(d, i + s, **gen_kw)
                for i, d in enumerate(m12a_dates)
            ])
            for s in range(8)
        ]
        hists = [
            pool_block1_hists(
                [r["block1_hist"] for r in g.per_session]
            )
            for g in gens
        ]
        return obs, gens, hists

    def forensic_fx(seed, kind, *, initiation=False, inn=2.0, esc=1.0,
                    matched=None, minute=1):
        """One schema-complete ForensicRec (spec section 10)."""
        return {
            "seed": seed, "kind": kind,
            "matched_extreme_minute_start": matched,
            "minute_start_ns": minute,
            "minute_end_ns": minute + 60 * 10**9,
            "utc_hour": 19, "segment_index": 0, "parent_count": 10,
            "trade_count": 20, "traced_parents": 10,
            "largest_innovation_std": inn,
            "largest_innovation_ts_ns": minute,
            "innovation_exceed_4": 1, "innovation_exceed_8": 0,
            "innovation_exceed_16": 0,
            "initiation": initiation, "sigma_start": 1.0,
            "sigma_peak": esc, "sigma_end": 1.0,
            "sigma_escalation": esc,
            "latent_mid_range_ticks": 5,
            "quote_mid_range_half_ticks": 10, "trade_range_ticks": 6,
            "trade_to_quote_range_ratio": 1.2,
            "quote_to_latent_range_ratio": 1.0,
            "max_signed_run": 3, "clamp_hits": 0,
            "arch_share_next": 0.1, "arch_share_minute_max": 0.2,
        }

    def quiet_forensic(initiation=False, inn=2.0, esc=1.0,
                       control_esc=1.0):
        return [{"records": [
            forensic_fx(s + 1, "extreme_range", initiation=initiation,
                        inn=inn, esc=esc),
            forensic_fx(s + 1, "control", matched=1, esc=control_esc),
        ], "refusals": []} for s in range(8)]

    st_mults = bootstrap_multiplicities(22)[:300]

    def run_case(obs_kw, gen_kw, forensic=None):
        obs, gens, hists = fake_ctxs(obs_kw, gen_kw)
        return evaluate_ladder(
            obs, gens, hists,
            forensic if forensic is not None else quiet_forensic(),
            st_mults,
        )

    base = {}
    ladder_none = run_case(base, base)
    check("an identical pair is no-family-eligible",
          ladder_none["verdict"] == "no-family-eligible"
          and ladder_none["eligible"] == [])

    # GARCH alone: generated hot 2x at the contour cells, the magnitude
    # shuffle closes the gap fully, forensic escalation fires.
    ladder_garch = run_case(
        {"robust": 1.0, "mag_perm": 2.0, "sign_perm": 1.0},
        {"robust": 2.0},
        forensic=quiet_forensic(esc=3.0, control_esc=1.0),
    )
    check("GARCH fires alone on magnitude closure plus escalation",
          ladder_garch["eligible"] == ["garch"]
          and ladder_garch["selected"] == "garch")
    check("the unfired rungs carry null localization",
          all(r["boundary_localized"] is None
              for r in ladder_garch["rungs"] if r["name"] != "garch"))

    # Reversion alone: the sign shuffle closes the gap, the covariance
    # direction fires, hour 23 pinned equal so the uniform form stays
    # eligible.
    ladder_rev = run_case(
        {"robust": 1.0, "sign_perm": 2.0, "mag_perm": 1.0,
         "cov": -0.5, "h23_robust": 2.0},
        {"robust": 2.0, "cov": 0.0, "h23_robust": 2.0},
    )
    rev_rung = next(
        r for r in ladder_rev["rungs"] if r["name"] == "reversion"
    )
    check("reversion fires on closure, folds and covariance",
          "reversion" in ladder_rev["eligible"]
          and rev_rung["subchecks"]
          == {"a_closure": True, "b_folds": True, "c_covariance": True})
    check("an unharmed hour 23 keeps the uniform form eligible",
          rev_rung["uniform_eligible"] is True
          and rev_rung["required_resolution"] == "uniform")

    # Innovation alone: heavy generated standardized tail, initiating
    # extremes, clean controls.
    ladder_inn = run_case(
        {"tail": 1.0}, {"tail": 2.0},
        forensic=quiet_forensic(initiation=True, inn=12.0),
    )
    check("innovation fires on the tail ratio plus initiation",
          ladder_inn["eligible"] == ["innovation"])

    # Child-walk alone: print excess without mid excess.
    ladder_cw = run_case(
        {"print_excess": 1.0}, {"print_excess": 2.0},
    )
    check("child-walk fires on print excess with a clean mid",
          ladder_cw["eligible"] == ["child_walk"])

    # Arrival alone: dispersed generated counts plus a count
    # substitution that closes the tail gap - the generated months
    # carry a heavy zero-observed-share bin at hour 13 whose reweight
    # removes the tail.
    hour13_base = [{
        "n": 10, "quote_range_half_ticks": 12,
        "trade_range_ticks": 10, "hour": 13,
        "since_open_bin": "1800+", "until_close_bin": "1800+",
        "count": 40,
    }]
    hour13_heavy = hour13_base + [{
        "n": 5000, "quote_range_half_ticks": 12,
        "trade_range_ticks": 100, "hour": 13,
        "since_open_bin": "1800+", "until_close_bin": "1800+",
        "count": 4000,
    }]
    ladder_arr = run_case(
        {"fano_d": 3, "minutes": hour13_base},
        {"fano_d": 9, "minutes": hour13_heavy},
    )
    check("arrival fires on dispersion plus a closing substitution",
          ladder_arr["eligible"] == ["arrival"])

    # Precedence: innovation and GARCH conditions fire both rungs (the
    # ordinary t-GARCH composition - the spec records co-firing as
    # interaction evidence); the ladder selects the first in order.
    ladder_multi = run_case(
        {"tail": 1.0, "robust": 1.0, "mag_perm": 2.0, "sign_perm": 1.0},
        {"tail": 2.0, "robust": 2.0},
        forensic=quiet_forensic(initiation=True, inn=12.0, esc=3.0,
                                control_esc=1.0),
    )
    check("co-firing families are all recorded and the first selected",
          set(ladder_multi["eligible"]) >= {"innovation", "garch"}
          and ladder_multi["selected"] == "innovation")

    # Boundary alone: the boundary cell out of band, its matched
    # comparator clean, no prior rung.
    ladder_bnd = run_case(
        {"boundary_quote": 10, "comp_quote": 10},
        {"boundary_quote": 20, "comp_quote": 10},
    )
    check("boundary fires only as the residual with a clean comparator",
          ladder_bnd["eligible"] == ["boundary"])

    check("the zero-denominator closure refuses instead of firing",
          ladder_none["count_substitution"]["closure_median"] is None
          and not ladder_none["rungs"][1]["subchecks"]["b_closure"])

    # Amendment D: one below-floor session in a required cell refuses
    # the metric, marks the family inventory incomplete, and fails the
    # rung closed even though its other metrics fire - with the
    # refusal mirrored onto the rung.
    thin_sessions = [
        fake_rec(d, i) for i, d in enumerate(m12a_dates)
    ]
    thin_sessions[3]["block3"]["cells"]["20"]["300"][
        "return_count"] = MIN_300S_CELL_RETURNS - 1
    obs_thin = ObsContext(thin_sessions)
    gens_g, hists_g = fake_ctxs(
        {}, {"robust": 2.0}
    )[1:]
    ladder_thin = evaluate_ladder(
        obs_thin, gens_g, hists_g,
        quiet_forensic(esc=3.0, control_esc=1.0), st_mults,
    )
    garch_env = ladder_thin["envelopes"]["garch"]
    garch_rung = next(
        r for r in ladder_thin["rungs"] if r["name"] == "garch"
    )
    check("a below-floor session fails its family closed",
          not garch_env["inventory_complete"]
          and garch_env["critical_value"] is None
          and not garch_rung["fired"]
          and any("below floor" in r["reason"]
                  for r in garch_rung["refusals"]))
    refused_garch = [
        m for m in garch_env["metrics"] if m["refused"]
    ]
    check("the refused metric is all-null and the computable ones "
          "keep point evidence with null envelope fields",
          refused_garch
          and all(m["point"] is None for m in refused_garch)
          and any(
              not m["refused"] and m["point"] is not None
              and m["interval_low"] is None
              for m in garch_env["metrics"]
          ))

    # Amendment D completeness gate: the raw magnitude closure and the
    # forensic escalation BOTH pass while the garch inventory refuses
    # on zero bootstrap SE (session-constant robust cells, which leave
    # every closure input computable) - the envelope-dependent
    # a_closure must gate on completeness and the rung must not fire.
    ladder_gate = run_case(
        {"robust": 1.0, "mag_perm": 2.0, "sign_perm": 1.0,
         "robust_jitter": False},
        {"robust": 2.0},
        forensic=quiet_forensic(esc=3.0, control_esc=1.0),
    )
    gate_rung = next(
        r for r in ladder_gate["rungs"] if r["name"] == "garch"
    )
    check("an incomplete garch inventory gates a passing closure",
          not ladder_gate["envelopes"]["garch"]["inventory_complete"]
          and ladder_gate["magnitude_closure"]["all_points_pass"]
          and ladder_gate["magnitude_closure"]["joint_lcb"] is not None
          and ladder_gate["magnitude_closure"]["joint_lcb"]
          > GAP_CLOSE_LCB_MIN
          and gate_rung["subchecks"]["a_closure"] is False
          and gate_rung["subchecks"]["b_escalation"] is True
          and not gate_rung["fired"])

    print("12a artifact schema")
    obs_fx, gens_fx, hists_fx = fake_ctxs(base, base)
    observed_fx = {
        "binding": {"harness_tree_commit": "TESTCOMMIT",
                    "job_id": "TEST", "subcontract_hash": "x",
                    "preflight_artifact_hash": "y", "file_hashes": {},
                    "tape_protocol_version": 11},
        "per_session": obs_fx.per_session,
        "monthly": {
            "block1": block1_blocks(pool_block1_hists(
                [r["block1_hist"] for r in obs_fx.per_session]
            )),
            "block2": pool_block2(
                [r["block2"] for r in obs_fx.per_session]
            ),
            "block3": aggregate_block3(
                [r["block3"] for r in obs_fx.per_session]
            ),
            "block4": aggregate_block4(
                [r["block4"] for r in obs_fx.per_session]
            ),
        },
        "permutations_monthly": aggregate_permutations(
            [r["permutations"] for r in obs_fx.per_session]
        ),
    }
    gen_seeds_fx = [
        {"seed": s + 1, "per_session": gens_fx[s].per_session,
         "forensic": quiet_forensic()[s],
         "cost": {"walk_s": 1.0, "rss_bytes": 1}}
        for s in range(8)
    ]
    binding_extra_fx = {
        "generated": {"seeds": list(range(1, 9)),
                      "window_start_ns": FINAL_START_NS,
                      "window_length_ns": FINAL_END_NS
                      - FINAL_START_NS, "warmup": SUMMARY_WARMUP}}
    cost_fx = {"observed_s": 1.0, "generated_s": 1.0,
               "bootstrap_s": 1.0, "total_s": 3.0,
               "peak_rss_bytes": 1, "scratch_bytes": 1}
    check("assembly refuses a truncated bootstrap population",
          refuses(
              lambda: assemble_measure12a_artifact(
                  observed_fx, gen_seeds_fx, binding_extra_fx,
                  st_mults, cost_fx,
              ),
              "replicates",
          ))
    # The selftest ladder runs on the documented truncated bootstrap
    # population, so the schema fixture is built through the internal
    # assembly body - the production entrypoint above refuses it.
    artifact_fx = _assemble_measure12a(
        observed_fx, gen_seeds_fx, binding_extra_fx, st_mults, cost_fx,
    )
    schema_errs = measure12a_schema_errors(artifact_fx)
    check("the recursive section-10 validator passes the artifact",
          schema_errs == [] or bool(print(schema_errs[:8])))
    bad_fx = json.loads(json.dumps(json_safe(artifact_fx)))
    bad_fx["observed"]["monthly"]["block4"]["19"]["session_votes"] = 22
    check("the validator rejects an injected uncontracted field",
          any("block4" in e
              for e in measure12a_schema_errors(bad_fx)))

    # The review mutation battery: each mutation of the valid artifact
    # must produce at least one schema error.
    def mutated(mutator):
        m = json.loads(json.dumps(json_safe(artifact_fx)))
        mutator(m)
        return measure12a_schema_errors(m)

    def mut_warmup(a):
        a["diagnostics"]["warmup_exclusions"]["banana"] = 1

    def mut_amendment_e(a):
        # A fired reversion rung with a measured worsening_23 but null
        # resolution fields must be rejected.
        a["ladder"]["rungs"][3]["fired"] = True
        a["diagnostics"]["worsening_23"] = {
            "point": 0.0, "se": 1.0, "ucb": 0.0,
        }

    def mut_dup(a):
        rec = {"scope": "family:child_walk", "cell": "x",
               "reason": "y"}
        a["diagnostics"]["refused_cells"].extend([rec, dict(rec)])

    def mut_metric_null(a):
        a["bootstrap"]["per_family"]["child_walk"]["metrics"][0][
            "point"] = None

    def mut_kind(a):
        a["bootstrap"]["per_family"]["child_walk"]["metrics"][0][
            "kind"] = "bogus"

    def mut_no_all(a):
        del a["observed"]["per_session"][0]["block4"]["all"]

    check("the validator rejects the review mutation battery",
          all(mutated(f) for f in (
              mut_warmup, mut_amendment_e, mut_dup, mut_metric_null,
              mut_kind, mut_no_all,
          )))
    check("the artifact carries exactly the frozen top-level keys",
          sorted(artifact_fx.keys())
          == ["binding", "bootstrap", "constants", "cost",
              "diagnostics", "generated", "ladder", "observed"])
    check("every rung record carries the frozen key set",
          all(
              sorted(r.keys()) == ["boundary_localized", "fired",
                                   "name", "refusals",
                                   "required_resolution", "subchecks",
                                   "uniform_eligible"]
              for r in artifact_fx["ladder"]["rungs"]
          ))
    check("the six rungs appear in ladder order",
          [r["name"] for r in artifact_fx["ladder"]["rungs"]]
          == ["child_walk", "arrival", "innovation", "reversion",
              "garch", "boundary"])
    metric_keys = {
        "name", "kind", "predicate", "point", "se", "interval_low",
        "interval_high", "band_low", "band_high", "outside_band",
        "envelope_excludes_edge", "interval_inside_band",
        "seed_same_side_count", "seed_inside_count",
        "seed_rule_pass", "fold_rule_pass", "refused",
    }
    check("every metric record carries EXACTLY the frozen key set",
          all(
              set(m.keys()) == metric_keys
              for fam in artifact_fx["bootstrap"]["per_family"].values()
              for m in fam["metrics"]
          ))
    check("every family record carries exactly metrics, critical "
          "value and inventory_complete",
          all(
              sorted(fam.keys()) == ["critical_value",
                                     "inventory_complete", "metrics"]
              for fam in artifact_fx["bootstrap"]["per_family"].values()
          ))
    check("every refusal record is the three-string RefusalRec",
          all(
              sorted(r.keys()) == ["cell", "reason", "scope"]
              for r in artifact_fx["diagnostics"]["refused_cells"]
          ))
    seed0 = artifact_fx["generated"]["per_seed"][0]
    check("count substitution carries the conditional adequacy "
          "records",
          isinstance(
              seed0["count_substitution"]["conditional_adequacy"],
              list,
          )
          and all(
              sorted(rec.keys()) == sorted([
                  "hour", "bin_name", "observed_p99", "generated_p99",
                  "ratio", "interval_low", "interval_high",
                  "interval_inside_band", "seed_inside_count",
                  "required", "supported",
              ])
              for rec in seed0["count_substitution"][
                  "conditional_adequacy"]
          ))
    check("empty observed bins are recorded as diagnostics",
          all(
              sorted(b.keys()) == ["cell", "scope"]
              for b in artifact_fx["diagnostics"]["empty_bins"]
          ))
    check("the constants block equals the exact section 7 name set",
          set(artifact_fx["constants"].keys())
          == set(MEASURE12A_CONSTANT_NAMES))
    check("the artifact serializes as strict JSON",
          isinstance(json.dumps(json_safe(artifact_fx)), str))
    check("central blocks drop the histogram and keep the medians",
          "hist" not in artifact_fx["generated"]["central"]["blocks"][
              "block1"]
          and artifact_fx["generated"]["central"]["blocks"]["block3"][
              "cells"]["19"]["300"]["robust_scale"] is not None)
    check("the semantic gates pass the fixture artifact",
          measure12a_semantic_errors(artifact_fx, list(m12a_dates))
          == [])
    check("a mixed-type usable list refuses by name on both paths",
          refuses(
              lambda: measure12a_observe(iter([]), ["2026-07-01", 1]),
              "non-string",
          )
          and any(
              "non-string" in e
              for e in measure12a_semantic_errors(
                  artifact_fx, ["2026-07-01", 1]
              )
          ))

    print(f"{checks} check(s), 0 failed")
    print("selftest PASS")


def main() -> None:
    if len(sys.argv) != 2 or sys.argv[1] not in ("selftest", "preflight",
                                                 "fit", "measure12a",
                                                 "cost12a"):
        raise SystemExit(__doc__)
    mode = sys.argv[1]
    try:
        if mode == "selftest":
            run_selftest()
        elif mode == "preflight":
            mode_preflight()
        elif mode == "measure12a":
            mode_measure12a()
        elif mode == "cost12a":
            mode_cost12a()
        else:
            mode_fit()
    except Refusal as exc:
        # The refusal messages are the interface; a raw traceback buries
        # them.
        raise SystemExit(f"refused: {exc}") from None


if __name__ == "__main__":
    main()
