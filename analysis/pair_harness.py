#!/usr/bin/env python3
"""The wave 1 pair harness: NQ versus MNQ under the frozen preregistration.

Implements `notes/pair-test-preregistration.md`. The acceptance rule is NOT
in this file: it is LOADED from `analysis/pair-test-preregistration.json`,
the frozen machine-readable half, and validated against this file's registry
of computable targets - an unknown target, an unknown class, or a missing
tolerance REFUSES rather than improvising. This harness never writes the
verdict artifact.

Three modes:

    selftest    the synthetic conformance fixtures; no real data, no seal
                broken. Step 2 of the preregistration's order of work.
    preflight   sealed-safe contract observation of a delivered directory:
                header, columns, timestamp resolution, leg and contract
                population, ordering, side population, session coverage.
                Prints NO target values. Step 3.
    report      the full run: per-target table, family table, verdict, and
                the separate protocol 8 volume-versus-count curves. Step 4.

Usage:
    python3 -u analysis/pair_harness.py selftest
    python3 -u analysis/pair_harness.py preflight research/market-data/databento/pairv/2026-07.2wk.trades
    python3 -u analysis/pair_harness.py report research/market-data/databento/pairv/2026-07.2wk.trades

Estimator lineage: `EventStats` and `AutoCorr` are imported unchanged for
the sweep family and every autocorrelation. Duration gaps are computed by
glue (EventStats cannot exclude halt-spanning gaps), and the glue's parent
grouping is CROSS-CHECKED against EventStats' event count per session, so
the two implementations of the same rule police each other.
"""

from __future__ import annotations

import argparse
import csv
import datetime as dt
import hashlib
import json
import math
import os
import statistics
import sys
from zoneinfo import ZoneInfo

from compression import zstd

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe_binance_aggtrades import AutoCorr  # noqa: E402
from probe_binance_trades import EventStats  # noqa: E402

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PREREG_FILE = os.path.join(ROOT, "analysis", "pair-test-preregistration.json")
LEDGER_FILE = os.path.join(ROOT, "analysis", "databento-jobs.json")
SELFTEST_DIR = os.path.join(ROOT, "analysis", "out", "pair-selftest")

# The preflight verdict artifact, written atomically beside the data. Report
# REQUIRES it and re-verifies its hashes, so invocation order cannot be
# faked and a re-delivered or edited file invalidates the old preflight.
PREFLIGHT_ARTIFACT = "preflight-pass.json"

UNAVAILABLE = "UNAVAILABLE"
SLACK = 1e-12

# Every target this harness knows how to compute, with its discrepancy
# class. The prereg JSON must agree exactly: a target it names that is not
# here, or a class mismatch, refuses. This is the prose-to-JSON contract.
REGISTERED_TARGETS = {
    "mean_event_duration_s": "ratio",
    "duration_dispersion_cv2": "ratio",
    "duration_acf_lag1": "correlation",
    "duration_acf_lag5": "correlation",
    "children_mean": "ratio",
    "children_single_frac": "fraction",
    "levels_mean": "ratio",
    "return_acf_lag1": "correlation",
    "abs_acf_lag1": "correlation",
    "abs_acf_lag10": "correlation",
    "abs_acf_lag50": "correlation",
    "zero_change_frac": "fraction",
    "side_flip_prob": "fraction",
    "side_run_length_mean": "ratio",
}

REQUIRED_COLUMNS = ("ts_event", "price", "size", "side", "symbol")

EXPECTED_FAMILIES = {
    "P1_cadence_level": (False, {"mean_event_duration_s"}),
    "P2_duration_shape": (True, {"duration_dispersion_cv2",
                                 "duration_acf_lag1", "duration_acf_lag5"}),
    "P3_sweep": (False, {"children_mean", "children_single_frac",
                          "levels_mean"}),
    "P4_return_shape": (True, {"return_acf_lag1", "abs_acf_lag1",
                               "abs_acf_lag10", "abs_acf_lag50"}),
    "P5_zero_change": (True, {"zero_change_frac"}),
    "P6_aggressor": (False, {"side_flip_prob", "side_run_length_mean"}),
}

EXPECTED_AGGREGATE_RULE = {
    "family_pass": "strict majority of the family's own targets",
    "verdict_pass": "all mandatory families pass AND strict majority of all evaluated families pass",
    "unavailable": "always fails the target, never a number",
    "insufficient_data": "fail",
    "partial_verdicts": "none",
}

EXPECTED_INFERENCE_RULES = {
    "parent_rule": "contiguous rows sharing event timestamp and aggressor side, within one leg",
    "event_clock": "exchange event timestamp, never receive timestamp",
    "closed_interval_gaps": "excluded from duration targets, counted",
}


class Refusal(Exception):
    """A deliberate refusal: the rule was NOT evaluated. main() exits 2 for
    these, so a caller can tell a refusal from a verdict FAIL (exit 1)."""


# ---------------------------------------------------------------------------
# The frozen rule
# ---------------------------------------------------------------------------


def load_prereg(path=PREREG_FILE):
    """Load and VALIDATE the frozen rule. Anything this harness cannot
    honour exactly is a refusal, never a silent adaptation."""
    try:
        prereg = json.loads(open(path).read())
    except (OSError, json.JSONDecodeError) as exc:
        raise Refusal("preregistration unreadable: %s" % exc)
    if prereg.get("prereg_schema_version") != 1:
        raise Refusal("preregistration schema version %r unknown" %
                      prereg.get("prereg_schema_version"))
    tol = prereg.get("tolerances", {})
    for key in ("ratio_tol_log", "fraction_tol", "correlation_tol",
                "robust_mult", "robust_min_frac"):
        value = tol.get(key)
        if not isinstance(value, (int, float)) or not math.isfinite(value) \
                or value <= 0:
            raise Refusal("tolerance %r is %r, not a positive finite number"
                          % (key, value))
    session = prereg.get("session", {})
    for key in ("expected_sessions", "min_usable_sessions",
                "first_trade_date", "last_trade_date", "timezone",
                "open_local", "close_local", "halt_local"):
        if key not in session:
            raise Refusal("session block missing %r" % key)
    for key in ("expected_sessions", "min_usable_sessions"):
        if not isinstance(session[key], int) or isinstance(session[key], bool) \
                or session[key] <= 0:
            raise Refusal("session %r is not a positive integer" % key)
    try:
        dates = trade_dates(prereg)
        session_table(prereg)
    except (ValueError, TypeError, KeyError) as exc:
        raise Refusal("invalid session calendar: %s" % exc)
    if len(dates) != session["expected_sessions"]:
        raise Refusal("session calendar generates %d sessions, expected %d" % (
            len(dates), session["expected_sessions"]))
    if session["min_usable_sessions"] > session["expected_sessions"]:
        raise Refusal("min_usable_sessions exceeds expected_sessions")
    families = prereg.get("families")
    if not isinstance(families, dict) or not families:
        raise Refusal("no families in the preregistration")
    if set(families) != set(EXPECTED_FAMILIES):
        raise Refusal("family registry differs from the executable rule")
    seen = set()
    for fname, family in families.items():
        if "mandatory" not in family or not isinstance(
                family["mandatory"], bool):
            raise Refusal("family %s has no boolean mandatory flag" % fname)
        targets = family.get("targets")
        if not isinstance(targets, dict) or not targets:
            raise Refusal("family %s has no targets" % fname)
        for tname, tclass in targets.items():
            if tname not in REGISTERED_TARGETS:
                raise Refusal("target %r is not computable by this harness; "
                              "refusing to improvise an estimator" % tname)
            if tclass != REGISTERED_TARGETS[tname]:
                raise Refusal("target %r declared class %r but the harness "
                              "computes class %r; prose and JSON have "
                              "diverged" % (tname, tclass,
                                            REGISTERED_TARGETS[tname]))
            if tname in seen:
                raise Refusal("target %r appears in two families" % tname)
            seen.add(tname)
        expected_mandatory, expected_targets = EXPECTED_FAMILIES[fname]
        if family["mandatory"] != expected_mandatory \
                or set(targets) != expected_targets:
            raise Refusal("family %s differs from the executable rule" % fname)
    if seen != set(REGISTERED_TARGETS):
        raise Refusal("target registry is incomplete")
    inference = prereg.get("inference", {})
    unsided = inference.get("max_unsided_frac")
    if not isinstance(unsided, (int, float)) or isinstance(unsided, bool) \
            or not math.isfinite(unsided) or not 0 <= unsided <= 1:
        raise Refusal("max_unsided_frac is not a finite fraction")
    for key, value in EXPECTED_INFERENCE_RULES.items():
        if inference.get(key) != value:
            raise Refusal("inference rule %r differs from the executable rule"
                          % key)
    if prereg.get("aggregate_rule") != EXPECTED_AGGREGATE_RULE:
        raise Refusal("aggregate rule differs from the executable rule")
    return prereg


# ---------------------------------------------------------------------------
# Sessions
# ---------------------------------------------------------------------------


def trade_dates(prereg):
    first = dt.date.fromisoformat(prereg["session"]["first_trade_date"])
    last = dt.date.fromisoformat(prereg["session"]["last_trade_date"])
    out = []
    day = first
    while day <= last:
        if day.weekday() < 5:
            out.append(day)
        day += dt.timedelta(days=1)
    return out


def _local_ns(day, hhmm, tz):
    hour, minute = (int(x) for x in hhmm.split(":"))
    local = dt.datetime(day.year, day.month, day.day, hour, minute, tzinfo=tz)
    return int(local.timestamp() * 1_000_000_000)


def session_table(prereg):
    """Per trade date: open, close and halt bounds as UTC epoch ns."""
    session = prereg["session"]
    tz = ZoneInfo(session["timezone"])
    table = []
    for day in trade_dates(prereg):
        table.append({
            "date": day.isoformat(),
            "open_ns": _local_ns(day - dt.timedelta(days=1),
                                 session["open_local"], tz),
            "close_ns": _local_ns(day, session["close_local"], tz),
            "halt_lo_ns": _local_ns(day, session["halt_local"][0], tz),
            "halt_hi_ns": _local_ns(day, session["halt_local"][1], tz),
        })
    return table


def _place(row, ts_ns):
    if row["halt_lo_ns"] <= ts_ns < row["halt_hi_ns"]:
        return row["date"], "halt"
    return row["date"], "open"


def assign_session(ts_ns, table, hint=None):
    """(session_date, where) with where one of open|halt|outside.

    `hint` is an optional one-element list holding the index of the last
    matched session. Deliveries are near-sorted, so checking that row
    first turns the per-row cost from a table scan into one comparison;
    a miss falls back to the full scan, so out-of-order rows stay correct."""
    if hint is not None and hint[0] < len(table):
        row = table[hint[0]]
        if row["open_ns"] <= ts_ns < row["close_ns"]:
            return _place(row, ts_ns)
    for i, row in enumerate(table):
        if row["open_ns"] <= ts_ns < row["close_ns"]:
            if hint is not None:
                hint[0] = i
            return _place(row, ts_ns)
    return None, "outside"


# ---------------------------------------------------------------------------
# Row intake
# ---------------------------------------------------------------------------


def leg_of(symbol):
    """MNQ before NQ: the prefixes overlap and the longer one wins. A
    spread symbol (NQU6-NQZ6) also starts with a leg prefix but prints a
    price DIFFERENCE, so anything carrying a dash refuses outright."""
    if "-" in symbol:
        raise Refusal("row carries spread symbol %r; the legs are outrights "
                      "only" % symbol)
    if symbol.startswith("MNQ"):
        return "MNQ"
    if symbol.startswith("NQ"):
        return "NQ"
    raise Refusal("row carries symbol %r, neither leg; the harness does "
                  "not guess" % symbol)


def iter_csv_zst(path):
    """Yield (ts_event, price, size, side, symbol) from a delivered
    csv.zst. Column positions come from the header BY NAME; a required
    semantic column that is absent refuses, per the preregistration."""
    with zstd.open(path, "rt", encoding="utf-8", newline="") as fh:
        rows = csv.reader(fh)
        try:
            header = next(rows)
        except StopIteration:
            raise Refusal("%s is empty" % path)
        index = required_column_index(path, header)
        i_ts, i_px = index["ts_event"], index["price"]
        i_sz, i_sd, i_sy = index["size"], index["side"], index["symbol"]
        needed = max(index.values())
        for line_no, parts in enumerate(rows, 2):
            if not parts:
                continue
            if len(parts) <= needed:
                raise Refusal("%s line %d is shorter than its header" % (
                    path, line_no))
            try:
                yield (int(parts[i_ts]), int(parts[i_px]), int(parts[i_sz]),
                       parts[i_sd], parts[i_sy])
            except ValueError as exc:
                raise Refusal("%s line %d has an invalid numeric field: %s" % (
                    path, line_no, exc))


def required_column_index(path, header):
    missing = [name for name in REQUIRED_COLUMNS if name not in header]
    if missing:
        raise Refusal("%s missing required semantic columns: %s" % (
            path, ", ".join(missing)))
    return {name: header.index(name) for name in REQUIRED_COLUMNS}


# ---------------------------------------------------------------------------
# Accumulation: one streaming pass into per-(leg, session) state
# ---------------------------------------------------------------------------


class SessionLeg:
    """Everything one leg accumulates inside one session."""

    def __init__(self, halt_lo_ns, halt_hi_ns):
        self.rows = 0
        self.unsided = 0
        self.contracts = {}
        # Sweep family, unchanged lineage. Fed NANOSECOND stamps while
        # EventStats' own gap fields assume microseconds, so its gap
        # outputs are 1000x off here and must never be read: glue owns
        # every duration, and report() is mined for children/levels only.
        self.events = EventStats(True)
        # Glue parent grouping, cross-checked against EventStats.events.
        self.parents = 0
        self._cur_ts = None
        self._cur_side = None
        self._prev_parent_ts = None
        self._prev_parent_side = None
        self.duration_count = 0
        self.duration_sum = 0.0
        self.duration_sumsq = 0.0
        self.duration_acf = AutoCorr(5)
        self.halt_gaps_excluded = 0
        self.side_flips = 0
        self.halt_lo_ns = halt_lo_ns
        self.halt_hi_ns = halt_hi_ns
        # Per-print price series (all rows, side not required).
        self.ret_acf = AutoCorr(50, lags=(1, 10, 50))
        self.ret1_acf = AutoCorr(1)
        self.prev_px = None
        self.prev_log = 0.0
        self.zero_change = 0
        self.changes = 0
        self.volume = 0
        # Hour-of-day accumulators for the protocol 8 correction. Local
        # hour is supplied by the caller, derived from the calendar's fixed
        # window offset.
        self.hour_counts = [0] * 24
        self.hour_volume = [0] * 24

    def push(self, ts, px, size, side, symbol, local_hour=0):
        self.rows += 1
        self.volume += size
        self.hour_counts[local_hour] += 1
        self.hour_volume[local_hour] += size
        self.contracts[symbol] = self.contracts.get(symbol, 0) + 1
        if self.prev_px is not None:
            self.changes += 1
            if px == self.prev_px:
                self.zero_change += 1
            log_px = math.log(px)
            ret = log_px - self.prev_log
            self.ret1_acf.push(ret)
            self.ret_acf.push(abs(ret))
            self.prev_log = log_px
        else:
            self.prev_log = math.log(px)
        self.prev_px = px
        # The DBN side alphabet, from research/dbn/rust/dbn/src/enums.rs
        # Side: Bid=b'B' is a BUY aggressor, Ask=b'A' is a SELL aggressor,
        # None=b'N' is unspecified. 'S' is not in the vendor alphabet; an
        # implementation reading B/S would classify every real sell as
        # unsided and manufacture a FAIL through the unsided gate.
        if side not in ("B", "A"):
            self.unsided += 1
            return
        is_buy = side == "B"
        self.events.push(ts, is_buy, px)
        if ts != self._cur_ts or is_buy != self._cur_side:
            self.parents += 1
            if self._prev_parent_ts is not None:
                if self._prev_parent_ts < self.halt_lo_ns \
                        and ts >= self.halt_hi_ns:
                    self.halt_gaps_excluded += 1
                else:
                    gap = (ts - self._prev_parent_ts) / 1_000_000_000.0
                    self.duration_count += 1
                    self.duration_sum += gap
                    self.duration_sumsq += gap * gap
                    self.duration_acf.push(gap)
                self.side_flips += is_buy != self._prev_parent_side
            self._prev_parent_ts = ts
            self._prev_parent_side = is_buy
            self._cur_ts = ts
            self._cur_side = is_buy


def window_utc_offset_ns(prereg):
    """The exchange-local UTC offset, CONSTANT across the frozen window (all
    of July 2026 is CDT); refuse if a DST edge ever lands inside a future
    window rather than silently mislabeling hours. Every session boundary
    is checked, not just the window's endpoints, so an out-and-back
    transition inside a long window cannot slip through."""
    tz = ZoneInfo(prereg["session"]["timezone"])
    offsets = set()
    for row in session_table(prereg):
        for key in ("open_ns", "close_ns"):
            offsets.add(
                dt.datetime.fromtimestamp(row[key] / 1e9, tz).utcoffset())
    if len(offsets) != 1:
        raise Refusal("the window crosses a DST transition; hour-of-day "
                      "attribution needs a per-row timezone conversion")
    return int(offsets.pop().total_seconds() * 1_000_000_000)


def accumulate(rows, prereg):
    """rows: iterable of (ts, price, size, side, symbol). Returns
    (state, counters) where state maps (leg, session_date) to SessionLeg."""
    table = session_table(prereg)
    offset_ns = window_utc_offset_ns(prereg)
    state = {}
    counters = {"outside": 0, "halt": 0}
    hint = [0]
    for ts, px, size, side, symbol in rows:
        date, where = assign_session(ts, table, hint)
        if where != "open":
            counters[where] += 1
            continue
        key = (leg_of(symbol), date)
        if key not in state:
            row = next(item for item in table if item["date"] == date)
            state[key] = SessionLeg(row["halt_lo_ns"], row["halt_hi_ns"])
        local_hour = ((ts + offset_ns) // 3_600_000_000_000) % 24
        state[key].push(ts, px, size, side, symbol, local_hour)
    return state, counters


# ---------------------------------------------------------------------------
# Per-session targets
# ---------------------------------------------------------------------------


def finite_acf(ac):
    """ac.acf(), unless the series is degenerate. AutoCorr's var<=0 branch
    returns 0.0 at EVERY lag, including restricted lags it never
    accumulated, and the correlation of a constant series is not 0.0 - it
    is undefined. Returning [] routes the callers' existing guards to
    UNAVAILABLE. The lineage class stays imported unchanged; this guard is
    glue."""
    if ac.n < 2:
        return []
    mean = ac.sum / ac.n
    if ac.sumsq / ac.n - mean * mean <= 0:
        return []
    return ac.acf()


def session_targets(leg_state, session_row, prereg):
    """One (leg, session) -> {target: value | UNAVAILABLE} plus diagnostics.

    The unsided gate applies to every side-aware target: parent grouping
    NEEDS the side, so P1, P2, P3 and P6 all sit behind it. The per-print
    price targets (P4, P5) do not."""
    out = {}
    diag = {"rows": leg_state.rows, "unsided": leg_state.unsided,
            "contracts": dict(leg_state.contracts)}

    unsided_frac = (leg_state.unsided / leg_state.rows
                    if leg_state.rows else 1.0)
    sided_ok = unsided_frac <= prereg["inference"]["max_unsided_frac"] + SLACK
    diag["unsided_frac"] = unsided_frac
    diag["sided_ok"] = sided_ok

    # Cross-check the two implementations of the parent rule.
    glue_parents = leg_state.parents
    events_parents = leg_state.events.events + (
        1 if leg_state.events.key_stamp is not None else 0)
    if sided_ok and glue_parents != events_parents:
        raise Refusal("parent grouping cross-check failed: glue %d vs "
                      "EventStats %d" % (glue_parents, events_parents))
    diag["parents"] = glue_parents

    side_targets = ("mean_event_duration_s", "duration_dispersion_cv2",
                    "duration_acf_lag1", "duration_acf_lag5",
                    "children_mean", "children_single_frac", "levels_mean",
                    "side_flip_prob", "side_run_length_mean")
    if not sided_ok:
        for name in side_targets:
            out[name] = UNAVAILABLE
    else:
        # Duration family from glue gaps, halt-spanning gaps excluded.
        diag["halt_gaps_excluded"] = leg_state.halt_gaps_excluded
        if leg_state.duration_count >= 6:
            mean = leg_state.duration_sum / leg_state.duration_count
            # Clamped at zero: near-constant gaps can push the sumsq form
            # microscopically negative, and a negative cv2 would refuse
            # through the ratio class by accident rather than on purpose.
            var = max(leg_state.duration_sumsq / leg_state.duration_count
                      - mean * mean, 0.0)
            out["mean_event_duration_s"] = mean if mean > 0 else UNAVAILABLE
            out["duration_dispersion_cv2"] = (
                var / (mean * mean) if mean > 0 else UNAVAILABLE)
            values = finite_acf(leg_state.duration_acf)
            out["duration_acf_lag1"] = values[0] if values else UNAVAILABLE
            out["duration_acf_lag5"] = (values[4] if len(values) > 4
                                        else UNAVAILABLE)
        else:
            for name in ("mean_event_duration_s", "duration_dispersion_cv2",
                         "duration_acf_lag1", "duration_acf_lag5"):
                out[name] = UNAVAILABLE

        if leg_state.events.events >= 2 or (
                leg_state.events.events >= 1
                and leg_state.events.key_stamp is not None):
            report = leg_state.events.report() if leg_state.events.gaps or \
                leg_state.parents >= 2 else None
            if report is not None:
                out["children_mean"] = report["children"]["mean"]
                out["children_single_frac"] = report["children"]["single_frac"]
                out["levels_mean"] = report["levels"]["mean"]
            else:
                for name in ("children_mean", "children_single_frac",
                             "levels_mean"):
                    out[name] = UNAVAILABLE
        else:
            for name in ("children_mean", "children_single_frac",
                         "levels_mean"):
                out[name] = UNAVAILABLE

        if leg_state.parents >= 2:
            flips = leg_state.side_flips
            out["side_flip_prob"] = flips / (leg_state.parents - 1)
            out["side_run_length_mean"] = leg_state.parents / (flips + 1)
        else:
            out["side_flip_prob"] = UNAVAILABLE
            out["side_run_length_mean"] = UNAVAILABLE

    ret1 = finite_acf(leg_state.ret1_acf)
    out["return_acf_lag1"] = ret1[0] if ret1 else UNAVAILABLE
    abs_acf = finite_acf(leg_state.ret_acf)
    out["abs_acf_lag1"] = (abs_acf[0] if abs_acf and abs_acf[0] is not None
                           else UNAVAILABLE)
    out["abs_acf_lag10"] = (abs_acf[9] if len(abs_acf) > 9
                            and abs_acf[9] is not None else UNAVAILABLE)
    out["abs_acf_lag50"] = (abs_acf[49] if len(abs_acf) > 49
                            and abs_acf[49] is not None else UNAVAILABLE)
    out["zero_change_frac"] = (leg_state.zero_change / leg_state.changes
                               if leg_state.changes else UNAVAILABLE)
    return out, diag


# ---------------------------------------------------------------------------
# Evaluation under the frozen rule
# ---------------------------------------------------------------------------


def discrepancy(cls, nq, mnq):
    """d(s) per the preregistration's classes, or UNAVAILABLE."""
    if nq == UNAVAILABLE or mnq == UNAVAILABLE:
        return UNAVAILABLE
    if cls == "ratio":
        if nq <= 0 or mnq <= 0:
            return UNAVAILABLE
        return math.log(nq / mnq)
    return nq - mnq


def class_tolerance(cls, prereg):
    tol = prereg["tolerances"]
    return {"ratio": tol["ratio_tol_log"],
            "fraction": tol["fraction_tol"],
            "correlation": tol["correlation_tol"]}[cls]


def evaluate_target(name, cls, nq_by_session, mnq_by_session, prereg):
    """The per-target rule: median within tolerance AND the robustness
    fraction of usable sessions individually within ROBUST_MULT times it.
    Leave-one-session-out medians are a reported diagnostic, never a gate."""
    tol = class_tolerance(cls, prereg)
    robust_mult = prereg["tolerances"]["robust_mult"]
    robust_min = prereg["tolerances"]["robust_min_frac"]
    min_usable = prereg["session"]["min_usable_sessions"]

    d = {}
    for session in sorted(set(nq_by_session) | set(mnq_by_session)):
        value = discrepancy(cls, nq_by_session.get(session, UNAVAILABLE),
                            mnq_by_session.get(session, UNAVAILABLE))
        if value != UNAVAILABLE:
            d[session] = value
    usable = len(d)
    if usable < min_usable:
        return {"target": name, "class": cls, "usable_sessions": usable,
                "median": UNAVAILABLE, "pass": False,
                "reason": "unavailable: %d usable sessions, need %d" % (
                    usable, min_usable),
                "d": d, "loo": []}
    values = list(d.values())
    med = statistics.median(values)
    median_ok = abs(med) <= tol + SLACK
    within = sum(1 for v in values if abs(v) <= robust_mult * tol + SLACK)
    robust_ok = within / usable >= robust_min - SLACK
    reasons = []
    if not median_ok:
        reasons.append("median |d| %.4f exceeds %.4f" % (abs(med), tol))
    if not robust_ok:
        reasons.append("only %d of %d sessions within %.1fx tolerance" % (
            within, usable, robust_mult))
    loo = []
    for i in range(len(values)):
        rest = values[:i] + values[i + 1:]
        loo.append(statistics.median(rest) if rest else UNAVAILABLE)
    return {"target": name, "class": cls, "usable_sessions": usable,
            "median": med, "pass": median_ok and robust_ok,
            "reason": "; ".join(reasons) if reasons else "passes",
            "d": d, "loo": loo}


def evaluate_all(target_tables, prereg, usable_sessions):
    """target_tables: {target: (nq_by_session, mnq_by_session)}. The
    aggregate rule of prereg section 5, driven ONLY by the fourteen targets
    - nothing else can enter, which is how the secondary measurements and
    the volume-versus-count outputs stay structurally unable to touch the
    verdict."""
    min_usable = prereg["session"]["min_usable_sessions"]
    if usable_sessions < min_usable:
        return {"verdict": "fail",
                "reason": "insufficient data: %d usable sessions, need %d" % (
                    usable_sessions, min_usable),
                "per_target": {}, "families": {},
                "mandatory_pass": False, "family_majority": False}
    per_target = {}
    families = {}
    for fname, family in prereg["families"].items():
        results = []
        for tname, tclass in family["targets"].items():
            nq_vals, mnq_vals = target_tables.get(tname, ({}, {}))
            result = evaluate_target(tname, tclass, nq_vals, mnq_vals,
                                     prereg)
            per_target[tname] = result
            results.append(result)
        passed = sum(1 for r in results if r["pass"])
        families[fname] = {
            "mandatory": family["mandatory"],
            "targets": len(results),
            "passed": passed,
            "pass": passed * 2 > len(results),
        }
    mandatory_pass = all(f["pass"] for f in families.values()
                         if f["mandatory"])
    passing = sum(1 for f in families.values() if f["pass"])
    family_majority = passing * 2 > len(families)
    verdict = "pass" if (mandatory_pass and family_majority) else "fail"
    return {"verdict": verdict,
            "reason": "per the frozen aggregate rule",
            "per_target": per_target, "families": families,
            "mandatory_pass": mandatory_pass,
            "family_majority": family_majority}


# ---------------------------------------------------------------------------
# Minority-contract exclusion and the full pipeline
# ---------------------------------------------------------------------------


def contract_exclusions(state):
    """Exclude every session containing a non-dominant window contract."""
    reasons = {}
    for leg in ("MNQ", "NQ"):
        totals = {}
        for (state_leg, _date), leg_state in state.items():
            if state_leg == leg:
                for symbol, count in leg_state.contracts.items():
                    totals[symbol] = totals.get(symbol, 0) + count
        if len(totals) <= 1:
            continue
        largest = max(totals.values())
        dominant = [symbol for symbol, count in totals.items()
                    if count == largest]
        minority = set(totals) - set(dominant) if len(dominant) == 1 \
            else set(totals)
        for (state_leg, date), leg_state in state.items():
            found = sorted(minority & set(leg_state.contracts))
            if state_leg == leg and found:
                reasons.setdefault(date, []).append(
                    "%s leg carries minority contract(s) %s" % (leg, found))
    return {date: "; ".join(items) for date, items in reasons.items()}


def pipeline(rows, prereg):
    """rows -> (verdict-result, correction, diagnostics). The single entry
    the report mode uses; the selftest drives it and its parts directly."""
    state, counters = accumulate(rows, prereg)
    table = {row["date"]: row for row in session_table(prereg)}
    excluded = contract_exclusions(state)
    sessions_present = sorted({date for _leg, date in state})
    # Usable means PAIRABLE: a session one leg never traded in cannot
    # contribute a discrepancy, so it must not pad the coverage count.
    usable = [s for s in sessions_present if s not in excluded
              and ("NQ", s) in state and ("MNQ", s) in state]

    per_leg = {}
    diagnostics = {"counters": counters, "excluded_sessions": excluded,
                   "sessions_present": sessions_present}
    for (leg, date), leg_state in state.items():
        if date in excluded:
            continue
        targets, diag = session_targets(leg_state, table[date], prereg)
        per_leg[(leg, date)] = targets
        diagnostics[("diag", leg, date)] = diag

    target_tables = {}
    for tname in REGISTERED_TARGETS:
        nq_vals = {d: per_leg[(leg, d)][tname]
                   for (leg, d) in per_leg if leg == "NQ"}
        mnq_vals = {d: per_leg[(leg, d)][tname]
                    for (leg, d) in per_leg if leg == "MNQ"}
        target_tables[tname] = (nq_vals, mnq_vals)

    result = evaluate_all(target_tables, prereg, len(usable))
    # The correction runs on the SAME paired usable-session set the verdict
    # covers, not on each leg's independent coverage: a session missing the
    # other leg, or excluded for any reason, must not enter one leg's curve
    # while the comparison never saw it.
    correction = volume_vs_count(state, set(usable), prereg)
    diagnostics["correction_sessions"] = {
        leg: correction.get(leg, {}).get("unused_sessions", [])
        for leg in ("MNQ", "NQ")}
    return result, correction, diagnostics


# ---------------------------------------------------------------------------
# The protocol 8 volume-versus-count correction: separate, never a verdict
# input. evaluate_all's signature cannot even see these outputs.
# ---------------------------------------------------------------------------


def open_minutes_by_hour(prereg):
    """Exposure per exchange-local hour, from the CALENDAR, not from row
    presence: an empty open hour must still widen its own denominator (the
    session-fit lesson). With a 17:00 open, 16:00 close and 15:15-15:30
    halt: hour 15 carries 45 open minutes, hour 16 carries none, every
    other hour carries 60."""
    session = prereg["session"]
    def minute_of(hhmm):
        hour, minute = (int(x) for x in hhmm.split(":"))
        return hour * 60 + minute
    open_m = minute_of(session["open_local"])
    close_m = minute_of(session["close_local"])
    halt_lo = minute_of(session["halt_local"][0])
    halt_hi = minute_of(session["halt_local"][1])
    minutes = [0] * 24
    for m in range(24 * 60):
        # Open when in [open, 24h) or [0, close), excluding the halt.
        is_open = (m >= open_m or m < close_m) and not (
            halt_lo <= m < halt_hi)
        if is_open:
            minutes[m // 60] += 1
    return minutes


def volume_vs_count(state, used_sessions, prereg):
    """The protocol 8 correction curves, per leg: mean trade COUNT per open
    minute and mean VOLUME per open minute by exchange-local hour, their
    peak-to-trough ratios, and the ratio of ratios. Diagnostic only: the
    verdict function cannot see any of this.

    `used_sessions` is the PAIRED usable-session set the verdict itself
    covers. Sessions a leg traded in but the comparison never used are
    reported as `unused_sessions`, never silently folded into a curve."""
    minutes_per_session = open_minutes_by_hour(prereg)
    out = {}
    for leg in ("MNQ", "NQ"):
        available = sorted({date for (state_leg, date) in state
                            if state_leg == leg})
        unused = [d for d in available if d not in used_sessions]
        counts = [0] * 24
        volume = [0] * 24
        sessions = 0
        for (state_leg, date), leg_state in state.items():
            if state_leg != leg or date not in used_sessions:
                continue
            sessions += 1
            for hour in range(24):
                counts[hour] += leg_state.hour_counts[hour]
                volume[hour] += leg_state.hour_volume[hour]
        if sessions == 0:
            out[leg] = {"sessions": 0, "sessions_available": len(available),
                        "unused_sessions": unused}
            continue
        count_rate = [
            counts[h] / (minutes_per_session[h] * sessions)
            if minutes_per_session[h] else None for h in range(24)]
        volume_rate = [
            volume[h] / (minutes_per_session[h] * sessions)
            if minutes_per_session[h] else None for h in range(24)]

        def peak_to_trough(rates):
            active = [r for r in rates if r is not None]
            if not active:
                return UNAVAILABLE
            if not any(active):
                return UNAVAILABLE
            if min(active) == 0:
                return math.inf
            return max(active) / min(active)

        count_ptt = peak_to_trough(count_rate)
        volume_ptt = peak_to_trough(volume_rate)
        ratio = (volume_ptt / count_ptt
                 if UNAVAILABLE not in (count_ptt, volume_ptt)
                 and math.isfinite(count_ptt)
                 and math.isfinite(volume_ptt)
                 else UNAVAILABLE)
        out[leg] = {
            "sessions": sessions,
            "sessions_available": len(available),
            "unused_sessions": unused,
            "count_per_min_by_hour": count_rate,
            "volume_per_min_by_hour": volume_rate,
            "count_peak_to_trough": count_ptt,
            "volume_peak_to_trough": volume_ptt,
            "volume_over_count_ptt": ratio,
        }
    return out


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------


def find_data_file(directory):
    names = [n for n in sorted(os.listdir(directory))
             if n.endswith(".csv.zst")]
    if len(names) != 1:
        raise Refusal("%s carries %d csv.zst members, expected exactly 1" % (
            directory, len(names)))
    return os.path.join(directory, names[0])


def sha256_file(path):
    hasher = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(4 * 1024 * 1024):
            hasher.update(chunk)
    return hasher.hexdigest()


def verify_input(directory, prereg, ledger_path=LEDGER_FILE):
    """Bind the input to the FROZEN identity before a byte of CSV is read.

    Three independent records must agree on the same file: the frozen
    preregistration's job id, the downloader ledger's entry for that job
    (with its verified delivery hashes), and the landed manifest.json that
    travels with the bytes. The file on disk is then re-hashed against the
    agreed digest. A substituted, corrupted or re-delivered file must not be
    able to decide the gate this harness feeds. Returns (path, sha256)."""
    frozen_job = prereg.get("job_id")
    if not frozen_job:
        raise Refusal("the preregistration carries no job_id to bind to")
    path = find_data_file(directory)
    name = os.path.basename(path)

    manifest_path = os.path.join(directory, "manifest.json")
    if not os.path.exists(manifest_path):
        raise Refusal("%s has no manifest.json; the delivery record must "
                      "travel with the bytes" % directory)
    try:
        manifest = json.loads(open(manifest_path).read())
    except (OSError, json.JSONDecodeError) as exc:
        raise Refusal("landed manifest unreadable: %s" % exc)
    if manifest.get("job_id") != frozen_job:
        raise Refusal("landed manifest names job %r, the frozen "
                      "preregistration binds to %r" % (
                          manifest.get("job_id"), frozen_job))
    manifest_files = manifest.get("files") or {}
    if name not in manifest_files:
        raise Refusal("manifest records no hash for %s" % name)

    try:
        ledger = json.loads(open(ledger_path).read())
    except (OSError, json.JSONDecodeError) as exc:
        raise Refusal("downloader ledger unreadable: %s" % exc)
    entries = [e for e in (ledger.get("jobs") or {}).values()
               if e.get("job_id") == frozen_job]
    if len(entries) != 1:
        raise Refusal("downloader ledger carries %d entries for job %s, "
                      "expected exactly 1" % (len(entries), frozen_job))
    ledger_files = entries[0].get("files") or {}
    if name not in ledger_files:
        raise Refusal("ledger records no verified hash for %s" % name)
    if ledger_files[name] != manifest_files[name]:
        raise Refusal("ledger and manifest disagree on %s's hash; resolve "
                      "before anything reads it" % name)

    actual = sha256_file(path)
    if actual != ledger_files[name]:
        raise Refusal("%s hashes %s... but the verified delivery was "
                      "%s...; the bytes on disk are not the bytes that "
                      "were bought" % (name, actual[:16],
                                       ledger_files[name][:16]))
    return path, actual


def prereg_hash(path=PREREG_FILE):
    return sha256_file(path)


def write_preflight_artifact(directory, payload):
    out = os.path.join(directory, PREFLIGHT_ARTIFACT)
    tmp = out + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(payload, fh, indent=1, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, out)
    return out


def mode_preflight(directory, prereg, ledger_path=LEDGER_FILE):
    """Contract observation, GATED. No target value is computed or printed.

    The estimators assume nanosecond timestamps and ordered rows: another
    unit would scale every duration and a regression would corrupt the
    contiguous grouping and the ACFs. So preflight REFUSES - it does not
    merely report - unless timestamps are uniformly 19-digit nanosecond
    epochs and ordering regressions are zero. On pass it persists an atomic
    artifact bound to the input hash and the preregistration hash; report
    requires that artifact, so invocation order cannot be skipped."""
    path, input_sha = verify_input(directory, prereg, ledger_path)
    table = session_table(prereg)
    with zstd.open(path, "rt", encoding="utf-8", newline="") as fh:
        csv_rows = csv.reader(fh)
        try:
            header = next(csv_rows)
        except StopIteration:
            raise Refusal("%s is empty" % path)
        print("file      %s" % os.path.basename(path))
        print("columns   %s" % ",".join(header))
        missing = [c for c in REQUIRED_COLUMNS if c not in header]
        print("required  %s" % ("all present" if not missing
                                else "MISSING: %s" % ", ".join(missing)))
        index = required_column_index(path, header)
        rows = 0
        per = {}
        sides = {}
        regressions = 0
        prev_ts = None
        ts_digits = set()
        needed = max(index.values())
        hint = [0]
        for line_no, parts in enumerate(csv_rows, 2):
            if not parts:
                continue
            if len(parts) <= needed:
                raise Refusal("%s line %d is shorter than its header" % (
                    path, line_no))
            try:
                # Parse everything report will parse: a delivery must not
                # pass preflight and then refuse at step 4 on a field
                # preflight never looked at (a decimal price, say).
                ts = int(parts[index["ts_event"]])
                int(parts[index["price"]])
                int(parts[index["size"]])
            except ValueError as exc:
                raise Refusal("%s line %d has an invalid numeric field: %s" % (
                    path, line_no, exc))
            symbol = parts[index["symbol"]]
            side = parts[index["side"]]
            rows += 1
            ts_digits.add(len(str(ts)))
            if prev_ts is not None and ts < prev_ts:
                regressions += 1
            prev_ts = ts
            date, where = assign_session(ts, table, hint)
            leg = leg_of(symbol)
            key = (leg, date if where == "open" else where)
            entry = per.setdefault(key, {"rows": 0, "contracts": set()})
            entry["rows"] += 1
            entry["contracts"].add(symbol)
            sides[side] = sides.get(side, 0) + 1
    print("rows      {:,}".format(rows))
    print("ts digits %s" % sorted(ts_digits))
    print("ordering  %d regressions" % regressions)
    print("sides     %s" % {k: v for k, v in sorted(sides.items())})
    for key in sorted(per, key=str):
        leg, where = key
        entry = per[key]
        print("  %-4s %-12s rows %10s  contracts %s" % (
            leg, where, "{:,}".format(entry["rows"]),
            sorted(entry["contracts"])))
    print("\npreflight prints NO target values, per the preregistration")
    if ts_digits != {19}:
        raise Refusal("timestamps are not uniformly 19-digit nanosecond "
                      "epochs (widths %s); the estimators would scale every "
                      "duration wrongly" % sorted(ts_digits))
    if regressions:
        raise Refusal("%d ordering regressions; contiguous grouping and the "
                      "ACFs assume ordered rows" % regressions)
    artifact = write_preflight_artifact(directory, {
        "input_file": os.path.basename(path),
        "input_sha256": input_sha,
        "prereg_sha256": prereg_hash(),
        "rows": rows,
        "ts_digits": sorted(ts_digits),
        "ordering_regressions": regressions,
        "sides": {k: v for k, v in sorted(sides.items())},
        "passed_at": dt.datetime.now(dt.timezone.utc).isoformat(),
    })
    print("PREFLIGHT PASS; artifact %s" % artifact)
    return 0


def require_preflight(directory, prereg, input_sha):
    """Report runs only against a preflight-passed input: the artifact must
    exist and its hashes must match BOTH the file on disk and the frozen
    rule, so neither a re-delivery nor a rule edit can ride on a stale
    pass."""
    artifact_path = os.path.join(directory, PREFLIGHT_ARTIFACT)
    if not os.path.exists(artifact_path):
        raise Refusal("no preflight artifact in %s; run preflight first, "
                      "per the frozen order of work" % directory)
    try:
        artifact = json.loads(open(artifact_path).read())
    except (OSError, json.JSONDecodeError) as exc:
        raise Refusal("preflight artifact unreadable: %s" % exc)
    if artifact.get("input_sha256") != input_sha:
        raise Refusal("preflight artifact was issued for input %s..., the "
                      "file on disk hashes %s...; re-run preflight" % (
                          str(artifact.get("input_sha256"))[:16],
                          input_sha[:16]))
    if artifact.get("prereg_sha256") != prereg_hash():
        raise Refusal("preflight artifact was issued under a different "
                      "preregistration; re-run preflight and record the "
                      "deviation that changed the frozen rule")


def mode_report(directory, prereg, ledger_path=LEDGER_FILE):
    path, input_sha = verify_input(directory, prereg, ledger_path)
    require_preflight(directory, prereg, input_sha)
    result, correction, diagnostics = pipeline(iter_csv_zst(path), prereg)
    print("counters  %s" % diagnostics["counters"])
    if diagnostics["excluded_sessions"]:
        print("EXCLUDED sessions:")
        for date, why in sorted(diagnostics["excluded_sessions"].items()):
            print("  %s: %s" % (date, why))
    print()
    print("%-24s %-11s %8s %6s  %s" % ("target", "class", "median",
                                       "usable", "verdict"))
    for name in sorted(result["per_target"]):
        r = result["per_target"][name]
        med = ("%8.4f" % r["median"]) if r["median"] != UNAVAILABLE \
            else "     n/a"
        print("%-24s %-11s %s %6d  %s" % (
            name, r["class"], med, r["usable_sessions"],
            "pass" if r["pass"] else "FAIL: %s" % r["reason"]))
    print()
    for fname in sorted(result["families"]):
        f = result["families"][fname]
        print("%-20s %d/%d %s%s" % (
            fname, f["passed"], f["targets"],
            "pass" if f["pass"] else "FAIL",
            "  [mandatory]" if f["mandatory"] else ""))
    print()
    print("mandatory families  %s" % result["mandatory_pass"])
    print("family majority     %s" % result["family_majority"])
    print("VERDICT             %s" % result["verdict"].upper())
    print("\nprotocol 8 volume-versus-count correction (NON-VERDICT):")
    for leg in ("MNQ", "NQ"):
        curves = correction.get(leg, {})
        if not curves.get("sessions"):
            print("  %s: no usable sessions" % leg)
            continue
        print("  %-4s sessions %d of %d  count ptt %s  volume ptt %s  "
              "volume/count ptt ratio %s  (fitted profile 27.51, per the "
              "frozen prereg volume_vs_count_correction)" % (
                  leg, curves["sessions"], curves["sessions_available"],
                  fmt(curves["count_peak_to_trough"]),
                  fmt(curves["volume_peak_to_trough"]),
                  fmt(curves["volume_over_count_ptt"])))
        if curves["unused_sessions"]:
            print("       unused sessions (present but outside the paired "
                  "usable set): %s" % curves["unused_sessions"])
    print("\nthe verdict artifact is NOT written by this harness")
    return 0 if result["verdict"] == "pass" else 1


def fmt(value):
    return "n/a" if value == UNAVAILABLE else "%.2f" % value


# ---------------------------------------------------------------------------
# Selftest
# ---------------------------------------------------------------------------


def selftest():
    """Wrapper so the fixture directory is removed even when a check or a
    fixture raises mid-run; reruns start clean either way."""
    import shutil
    try:
        _selftest_body()
    finally:
        shutil.rmtree(SELFTEST_DIR, ignore_errors=True)


def _selftest_body():
    prereg = load_prereg()
    table = session_table(prereg)
    dates = [row["date"] for row in table]
    by_date = {row["date"]: row for row in table}
    checks = []

    def check(name, condition):
        checks.append((name, bool(condition)))
        print("  %s %s" % ("ok  " if condition else "FAIL", name))

    def at(date, offset_ns):
        return by_date[date]["open_ns"] + offset_ns

    H = 3_600_000_000_000  # one hour in ns

    print("prose-to-JSON contract validation")
    check("the frozen preregistration validates", bool(prereg))
    import copy
    bad = copy.deepcopy(prereg)
    bad["families"]["P2_duration_shape"]["targets"]["bogus_target"] = "ratio"
    try:
        _validate_dict(bad)
        refused = False
    except Refusal:
        refused = True
    check("an unknown target refuses, never improvises", refused)
    bad = copy.deepcopy(prereg)
    bad["families"]["P5_zero_change"]["targets"]["zero_change_frac"] = \
        "geometric"
    try:
        _validate_dict(bad)
        refused = False
    except Refusal:
        refused = True
    check("an unknown class refuses", refused)
    bad = copy.deepcopy(prereg)
    del bad["tolerances"]["fraction_tol"]
    try:
        _validate_dict(bad)
        refused = False
    except Refusal:
        refused = True
    check("a missing tolerance refuses", refused)
    bad = copy.deepcopy(prereg)
    bad["families"]["P4_return_shape"]["targets"]["zero_change_frac"] = \
        "fraction"
    try:
        _validate_dict(bad)
        refused = False
    except Refusal:
        refused = True
    check("a target in two families refuses", refused)
    bad = copy.deepcopy(prereg)
    bad["aggregate_rule"]["family_pass"] = "at least one target"
    try:
        _validate_dict(bad)
        refused = False
    except Refusal:
        refused = True
    check("an aggregate-rule drift refuses", refused)
    bad = copy.deepcopy(prereg)
    bad["session"]["expected_sessions"] = 9
    try:
        _validate_dict(bad)
        refused = False
    except Refusal:
        refused = True
    check("an expected-session mismatch refuses", refused)

    print("session table and assignment")
    check("ten sessions, first and last as frozen",
          len(table) == 10 and dates[0] == "2026-07-06"
          and dates[-1] == "2026-07-17")
    assigned = [assign_session(at(d, H), table)[0] for d in dates]
    check("a trade one hour into each session lands in that session",
          assigned == dates)
    hint = [0]
    cached = [assign_session(at(d, H), table, hint)[0] for d in dates]
    check("the cached-scan path agrees with the pure scan", cached == dates)
    date0 = dates[0]
    check("a trade before the first open is outside",
          assign_session(by_date[date0]["open_ns"] - 1, table)
          == (None, "outside"))
    check("a trade exactly at open belongs to the session",
          assign_session(by_date[date0]["open_ns"], table)[0] == date0)
    check("a trade exactly at close is outside",
          assign_session(by_date[date0]["close_ns"], table)
          == (None, "outside"))
    check("a trade inside the halt is excluded as halt",
          assign_session(by_date[date0]["halt_lo_ns"] + 1, table)[1]
          == "halt")
    check("a trade at halt end is open again",
          assign_session(by_date[date0]["halt_hi_ns"], table)[1] == "open")
    weekend = dt.date(2026, 7, 11)
    check("no weekend session exists",
          weekend.isoformat() not in by_date)

    print("leg splitting through the real loader")
    os.makedirs(SELFTEST_DIR, exist_ok=True)
    csv_path = os.path.join(SELFTEST_DIR, "fixture.csv.zst")
    header = "ts_recv,ts_event,price,size,side,symbol"
    lines = [header]
    for i in range(3):
        lines.append("0,%d,100,1,B,NQU6" % at(date0, H + i * 1000))
    for i in range(2):
        lines.append("0,%d,100,1,A,MNQU6" % at(date0, H + i * 1000))
    with open(csv_path, "wb") as fh:
        fh.write(zstd.compress("\n".join(lines).encode()))
    rows = list(iter_csv_zst(csv_path))
    legs = {}
    for row in rows:
        legs[leg_of(row[4])] = legs.get(leg_of(row[4]), 0) + 1
    check("loader parses and the legs split 3 NQ / 2 MNQ",
          legs == {"NQ": 3, "MNQ": 2})
    bad_csv = os.path.join(SELFTEST_DIR, "noside.csv.zst")
    with open(bad_csv, "wb") as fh:
        fh.write(zstd.compress(b"ts_event,price,size,symbol\n1,2,3,NQU6"))
    try:
        list(iter_csv_zst(bad_csv))
        refused = False
    except Refusal:
        refused = True
    check("a missing semantic column refuses", refused)
    empty_csv = os.path.join(SELFTEST_DIR, "empty.csv.zst")
    with open(empty_csv, "wb") as fh:
        fh.write(zstd.compress(b""))
    try:
        list(iter_csv_zst(empty_csv))
        refused = False
    except Refusal:
        refused = True
    check("an empty delivered file refuses deliberately", refused)
    try:
        leg_of("ESU6")
        refused = False
    except Refusal:
        refused = True
    check("an unknown symbol refuses rather than guessing a leg", refused)
    try:
        leg_of("NQU6-NQZ6")
        refused = False
    except Refusal:
        refused = True
    check("a spread symbol refuses rather than polluting a leg", refused)

    print("parent grouping")
    t0 = at(date0, H)
    rows = [
        (t0, 100, 1, "B", "NQU6"),
        (t0, 101, 1, "B", "NQU6"),          # same ts+side: same parent
        (t0, 102, 1, "A", "NQU6"),          # same ts, side flip: new parent
        (t0, 103, 1, "B", "NQU6"),          # same ts, back to B: NEW parent
        (t0 + 1000, 104, 1, "B", "NQU6"),   # new ts: new parent
    ]
    state, _ = accumulate(rows, prereg)
    leg_state = state[("NQ", date0)]
    check("contiguous ts+side groups, non-contiguous same-ts stays split",
          leg_state.parents == 4)
    check("EventStats agrees with the glue grouping",
          leg_state.events.events + 1 == 4)
    check("parent glue retains no unbounded per-parent lists",
          not hasattr(leg_state, "parent_times")
          and not hasattr(leg_state, "parent_sides"))

    print("halt and cross-session duration gaps")
    halt_lo = by_date[date0]["halt_lo_ns"]
    halt_hi = by_date[date0]["halt_hi_ns"]
    date1 = dates[1]
    rows = []
    for i in range(8):  # eight parents before the halt, 1s apart: 7 gaps
        rows.append((halt_lo - 10_000_000_000 + i * 1_000_000_000,
                     100 + i, 1, "B", "NQU6"))
    rows.append((halt_hi + 1_000_000_000, 200, 1, "B", "NQU6"))
    rows.append((at(date1, H), 300, 1, "B", "NQU6"))  # next session
    state, counters = accumulate(rows, prereg)
    targets0, diag0 = session_targets(state[("NQ", date0)], by_date[date0],
                                      prereg)
    check("the halt-spanning gap is excluded and counted",
          diag0["halt_gaps_excluded"] == 1)
    check("seven retained one-second gaps give mean 1.0",
          isinstance(targets0["mean_event_duration_s"], float)
          and abs(targets0["mean_event_duration_s"] - 1.0) < 1e-9)
    check("sessions do not leak gaps into each other",
          state[("NQ", date1)].parents == 1
          and state[("NQ", date1)].duration_count == 0)
    halt_row = (halt_lo + 1000, 100, 1, "B", "NQU6")
    _state2, counters2 = accumulate([halt_row], prereg)
    check("a trade inside the halt is counted and dropped",
          counters2["halt"] == 1 and not _state2)

    print("unsided threshold below, at, above the frozen 1 percent")
    def unsided_session(n_unsided):
        rows = []
        for i in range(1000 - n_unsided):
            rows.append((t0 + i * 1_000_000_000, 100 + i % 3, 1, "B", "NQU6"))
        for i in range(n_unsided):
            rows.append((t0 + (1000 + i) * 1_000_000_000, 100, 1, "N",
                         "NQU6"))
        state, _ = accumulate(rows, prereg)
        return session_targets(state[("NQ", date0)], by_date[date0], prereg)
    below, _ = unsided_session(9)
    at_limit, _ = unsided_session(10)
    above, _ = unsided_session(11)
    check("0.9 percent unsided keeps side targets",
          below["children_mean"] != UNAVAILABLE)
    check("exactly 1.0 percent keeps side targets",
          at_limit["children_mean"] != UNAVAILABLE)
    check("1.1 percent makes every side target unavailable",
          above["children_mean"] == UNAVAILABLE
          and above["duration_acf_lag1"] == UNAVAILABLE
          and above["side_flip_prob"] == UNAVAILABLE)
    check("price targets survive the unsided gate",
          above["zero_change_frac"] != UNAVAILABLE)

    print("the vendor side alphabet: B buys, A sells, N unsided")
    abn_rows = [
        (t0, 100, 1, "B", "NQU6"),
        (t0 + 1_000_000_000, 101, 1, "A", "NQU6"),
        (t0 + 2_000_000_000, 102, 1, "B", "NQU6"),
        (t0 + 3_000_000_000, 103, 1, "A", "NQU6"),
        (t0 + 4_000_000_000, 104, 1, "N", "NQU6"),
    ]
    state, _ = accumulate(abn_rows, prereg)
    abn = state[("NQ", date0)]
    check("A is a SELL aggressor, not unsided: four sided parents",
          abn.parents == 4 and abn.unsided == 1)
    check("B to A to B to A is three flips",
          abn.side_flips == 3)
    # The N row above is 20 percent of five rows, correctly tripping the
    # unsided gate; flip probability is asserted on a gate-clean fixture.
    state, _ = accumulate(abn_rows[:4], prereg)
    abn_t, _ = session_targets(state[("NQ", date0)], by_date[date0], prereg)
    check("alternating sides read flip probability 1.0",
          abn_t["side_flip_prob"] == 1.0
          and abn_t["side_run_length_mean"] == 1.0)
    legacy = [(t0, 100, 1, "S", "NQU6")]
    state, _ = accumulate(legacy, prereg)
    check("'S' is NOT in the vendor alphabet and counts as unsided",
          state[("NQ", date0)].unsided == 1
          and state[("NQ", date0)].parents == 0)

    print("input identity binding: three records must agree on the bytes")

    def build_delivery(tag, csv_lines, manifest_job=None):
        d = os.path.join(SELFTEST_DIR, tag)
        os.makedirs(d, exist_ok=True)
        data = os.path.join(d, "glbx-fixture.trades.csv.zst")
        with open(data, "wb") as fh:
            fh.write(zstd.compress("\n".join(csv_lines).encode()))
        digest = sha256_file(data)
        name = os.path.basename(data)
        with open(os.path.join(d, "manifest.json"), "w") as fh:
            json.dump({"job_id": manifest_job or prereg["job_id"],
                       "files": {name: digest}}, fh)
        ledger = os.path.join(d, "ledger.json")
        with open(ledger, "w") as fh:
            json.dump({"_version": 1, "jobs": {"k": {
                "job_id": prereg["job_id"],
                "files": {name: digest}}}}, fh)
        return d, ledger, data

    base = at(date0, H)
    good_lines = [header]
    for i in range(6):
        good_lines.append("0,%d,100,1,%s,%s" % (
            base + i * 1000, "B" if i % 3 else "A",
            "NQU6" if i % 2 == 0 else "MNQU6"))
    good_dir, good_ledger, good_data = build_delivery("delivery", good_lines)
    path, input_sha = verify_input(good_dir, prereg, good_ledger)
    check("identity binding passes when all three records agree",
          path == good_data and input_sha == sha256_file(good_data))
    wrong_dir, wrong_ledger, _ = build_delivery(
        "wrongjob", good_lines, manifest_job="GLBX-SOMETHING-ELSE")
    try:
        verify_input(wrong_dir, prereg, wrong_ledger)
        refused = False
    except Refusal:
        refused = True
    check("a manifest naming another job refuses", refused)
    lonely_dir, lonely_ledger, _ = build_delivery("noledger", good_lines)
    with open(lonely_ledger, "w") as fh:
        json.dump({"_version": 1, "jobs": {}}, fh)
    try:
        verify_input(lonely_dir, prereg, lonely_ledger)
        refused = False
    except Refusal:
        refused = True
    check("a ledger without the frozen job refuses", refused)
    tam_dir, tam_ledger, tam_data = build_delivery("tampered", good_lines)
    with open(tam_data, "wb") as fh:
        fh.write(zstd.compress(b"entirely different bytes"))
    try:
        verify_input(tam_dir, prereg, tam_ledger)
        refused = False
    except Refusal:
        refused = True
    check("bytes that differ from the verified delivery refuse", refused)
    bare_dir = os.path.join(SELFTEST_DIR, "nomanifest")
    os.makedirs(bare_dir, exist_ok=True)
    with open(os.path.join(bare_dir, "x.csv.zst"), "wb") as fh:
        fh.write(zstd.compress(b"anything"))
    try:
        verify_input(bare_dir, prereg, good_ledger)
        refused = False
    except Refusal:
        refused = True
    check("a delivery without its manifest refuses", refused)

    print("the preflight gate and its artifact")
    rc = mode_preflight(good_dir, prereg, good_ledger)
    artifact_path = os.path.join(good_dir, PREFLIGHT_ARTIFACT)
    artifact = json.loads(open(artifact_path).read())
    check("preflight passes ns-clean ordered data and persists its artifact",
          rc == 0 and artifact["input_sha256"] == input_sha
          and artifact["prereg_sha256"] == prereg_hash())
    rc = mode_report(good_dir, prereg, good_ledger)
    check("report runs after preflight and lands the frozen "
          "insufficient-data fail on a tiny fixture", rc == 1)
    os.remove(artifact_path)
    try:
        mode_report(good_dir, prereg, good_ledger)
        refused = False
    except Refusal:
        refused = True
    check("report without a preflight artifact refuses", refused)
    mode_preflight(good_dir, prereg, good_ledger)
    artifact = json.loads(open(artifact_path).read())
    artifact["prereg_sha256"] = "0" * 64
    with open(artifact_path, "w") as fh:
        json.dump(artifact, fh)
    try:
        mode_report(good_dir, prereg, good_ledger)
        refused = False
    except Refusal:
        refused = True
    check("an artifact issued under another preregistration refuses",
          refused)
    ms_lines = [header, "0,1751234567890,100,1,B,NQU6"]
    ms_dir, ms_ledger, _ = build_delivery("millis", ms_lines)
    try:
        mode_preflight(ms_dir, prereg, ms_ledger)
        refused = False
    except Refusal:
        refused = True
    check("non-nanosecond timestamps REFUSE preflight, not merely print",
          refused
          and not os.path.exists(os.path.join(ms_dir, PREFLIGHT_ARTIFACT)))
    ooo_lines = [header,
                 "0,%d,100,1,B,NQU6" % (base + 2000),
                 "0,%d,100,1,B,NQU6" % base]
    ooo_dir, ooo_ledger, _ = build_delivery("outoforder", ooo_lines)
    try:
        mode_preflight(ooo_dir, prereg, ooo_ledger)
        refused = False
    except Refusal:
        refused = True
    check("ordering regressions REFUSE preflight", refused)
    fresh_lines = [header, "0,%d,100,1,B,NQU6" % (base + 5_000_000)]
    with open(good_data, "wb") as fh:
        fh.write(zstd.compress("\n".join(fresh_lines).encode()))
    digest = sha256_file(good_data)
    name = os.path.basename(good_data)
    with open(os.path.join(good_dir, "manifest.json"), "w") as fh:
        json.dump({"job_id": prereg["job_id"], "files": {name: digest}}, fh)
    with open(good_ledger, "w") as fh:
        json.dump({"_version": 1, "jobs": {"k": {
            "job_id": prereg["job_id"], "files": {name: digest}}}}, fh)
    try:
        mode_report(good_dir, prereg, good_ledger)
        refused = False
    except Refusal:
        refused = True
    check("a re-delivered file invalidates the old preflight artifact",
          refused)

    print("degenerate series: correlations refuse to fabricate a number")
    const_rows = [(t0 + i * 1_000_000_000, 500, 1, "B", "NQU6")
                  for i in range(100)]
    state, _ = accumulate(const_rows, prereg)
    const_t, _ = session_targets(state[("NQ", date0)], by_date[date0],
                                 prereg)
    check("a constant price gives UNAVAILABLE correlations, never 0.0",
          const_t["return_acf_lag1"] == UNAVAILABLE
          and const_t["abs_acf_lag1"] == UNAVAILABLE
          and const_t["abs_acf_lag50"] == UNAVAILABLE)
    check("the constant price still reads as pure zero-change",
          const_t["zero_change_frac"] == 1.0)
    check("constant one-second gaps give UNAVAILABLE duration acf, cv2 zero",
          const_t["duration_acf_lag1"] == UNAVAILABLE
          and const_t["duration_dispersion_cv2"] == 0.0)

    print("minority-contract session exclusion")
    rows = [(t0, 100, 1, "B", "MNQU6"), (t0 + 1000, 101, 1, "B", "MNQZ6")]
    state, _ = accumulate(rows, prereg)
    excluded = contract_exclusions(state)
    check("a second contract in a leg excludes the session by name",
          date0 in excluded and "MNQZ6" in excluded[date0])
    rows = [
        (t0, 100, 1, "B", "NQU6"),
        (t0 + 1000, 101, 1, "B", "NQU6"),
        (t0 + 2000, 102, 1, "B", "NQU6"),
        (at(date1, H), 103, 1, "B", "NQZ6"),
    ]
    state, _ = accumulate(rows, prereg)
    excluded = contract_exclusions(state)
    check("a minority contract isolated in its own session is excluded",
          date0 not in excluded and date1 in excluded
          and "NQZ6" in excluded[date1])

    print("tolerance boundaries, every class")
    def flat(value):
        return {d: value for d in dates}
    r = evaluate_target("children_mean", "ratio", flat(1.25), flat(1.0),
                        prereg)
    check("ratio at exactly 25 percent passes inclusively", r["pass"])
    r = evaluate_target("children_mean", "ratio", flat(1.2501), flat(1.0),
                        prereg)
    check("ratio just above 25 percent fails", not r["pass"])
    r = evaluate_target("zero_change_frac", "fraction", flat(0.60),
                        flat(0.50), prereg)
    check("fraction at exactly 0.10 passes", r["pass"])
    r = evaluate_target("zero_change_frac", "fraction", flat(0.6001),
                        flat(0.50), prereg)
    check("fraction just above 0.10 fails", not r["pass"])
    r = evaluate_target("abs_acf_lag1", "correlation", flat(0.30),
                        flat(0.20), prereg)
    check("correlation at exactly 0.10 passes", r["pass"])
    r = evaluate_target("abs_acf_lag1", "correlation", flat(0.3001),
                        flat(0.20), prereg)
    check("correlation just above 0.10 fails", not r["pass"])

    print("robustness boundary at 8, 9 and 10 usable sessions")
    def robust_case(n_sessions, n_outliers):
        nq = {}
        mnq = {}
        for i, d in enumerate(dates[:n_sessions]):
            mnq[d] = 0.50
            nq[d] = 0.50 if i >= n_outliers else 0.90  # far outside 1.5x
        return evaluate_target("zero_change_frac", "fraction", nq, mnq,
                               prereg)
    check("10 sessions, 7 within: exactly 70 percent passes",
          robust_case(10, 3)["pass"])
    check("10 sessions, 6 within fails robustness",
          not robust_case(10, 4)["pass"])
    check("9 sessions, 7 within passes", robust_case(9, 2)["pass"])
    check("9 sessions, 6 within fails", not robust_case(9, 3)["pass"])
    check("8 sessions, 6 within passes", robust_case(8, 2)["pass"])
    check("8 sessions, 5 within fails", not robust_case(8, 3)["pass"])
    check("7 usable sessions is unavailable and fails",
          not robust_case(7, 0)["pass"]
          and robust_case(7, 0)["median"] == UNAVAILABLE)

    print("families, majorities, mandatory paths")
    def tables(pass_targets):
        out = {}
        for name in REGISTERED_TARGETS:
            good = name in pass_targets
            out[name] = (flat(0.50 if good else 0.90), flat(0.50))
        return out
    all_names = set(REGISTERED_TARGETS)
    result = evaluate_all(tables(all_names), prereg, 10)
    check("everything passing gives PASS", result["verdict"] == "pass")
    result = evaluate_all(tables(set()), prereg, 10)
    check("everything failing gives FAIL", result["verdict"] == "fail")
    for fam, members in (
            ("P2", {"duration_dispersion_cv2", "duration_acf_lag1",
                    "duration_acf_lag5"}),
            ("P4", {"return_acf_lag1", "abs_acf_lag1", "abs_acf_lag10",
                    "abs_acf_lag50"}),
            ("P5", {"zero_change_frac"})):
        result = evaluate_all(tables(all_names - members), prereg, 10)
        check("failing mandatory %s alone fails the verdict" % fam,
              result["verdict"] == "fail"
              and result["family_majority"] is True)
    p2 = {"duration_dispersion_cv2", "duration_acf_lag1", "duration_acf_lag5"}
    result = evaluate_all(
        tables(all_names - {"duration_acf_lag5"}), prereg, 10)
    check("a family passes on 2 of 3, strict majority",
          result["families"]["P2_duration_shape"]["pass"]
          and result["verdict"] == "pass")
    result = evaluate_all(
        tables(all_names - {"duration_acf_lag5", "duration_acf_lag1"}),
        prereg, 10)
    check("a family fails on 1 of 3",
          not result["families"]["P2_duration_shape"]["pass"]
          and result["verdict"] == "fail")
    # Majority without mandatory already covered; mandatory without majority:
    mandatory_only = p2 | {"return_acf_lag1", "abs_acf_lag1", "abs_acf_lag10",
                           "abs_acf_lag50", "zero_change_frac"}
    result = evaluate_all(tables(mandatory_only), prereg, 10)
    check("all mandatory passing cannot carry a failed six-family majority",
          result["mandatory_pass"] is True
          and result["family_majority"] is False
          and result["verdict"] == "fail")

    print("P6 counts in the majority but can never become mandatory")
    check("the frozen rule marks P6 non-mandatory",
          prereg["families"]["P6_aggressor"]["mandatory"] is False)
    p6 = {"side_flip_prob", "side_run_length_mean"}
    result = evaluate_all(tables(all_names - p6), prereg, 10)
    check("P6 failing alone cannot fail the verdict",
          result["verdict"] == "pass")
    result = evaluate_all(tables(mandatory_only | p6), prereg, 10)
    check("P6 passing counts toward the family majority",
          result["family_majority"] is True
          and result["verdict"] == "pass")

    print("insufficient data and unavailability")
    result = evaluate_all(tables(all_names), prereg, 7)
    check("7 usable sessions fails as insufficient data",
          result["verdict"] == "fail"
          and "insufficient" in result["reason"])
    half = {d: 0.5 for d in dates[:5]}
    r = evaluate_target("zero_change_frac", "fraction", half, half, prereg)
    check("an under-covered target is UNAVAILABLE and fails",
          not r["pass"] and r["median"] == UNAVAILABLE)

    print("the correction cannot touch the verdict, and computes honestly")
    exposure = open_minutes_by_hour(prereg)
    check("hour 15 carries 45 open minutes around the halt",
          exposure[15] == 45)
    check("hour 16 is closed, zero minutes", exposure[16] == 0)
    check("hour 17 and hour 3 carry full hours",
          exposure[17] == 60 and exposure[3] == 60)
    check("total exposure is the 22.75-hour session",
          sum(exposure) == 22 * 60 + 45)
    # Two hours, known rates: 120 trades of size 1 in the hour after open,
    # 60 trades of size 4 in the following hour -> count rates 2.0 and 1.0
    # per minute (ptt 2.0), volume rates 2.0 and 4.0 (ptt 2.0), ratio 1.0.
    curve_rows = []
    for i in range(120):
        curve_rows.append((at(date0, i * 30_000_000_000), 100, 1, "B",
                           "MNQU6"))
    for i in range(60):
        curve_rows.append((at(date0, H + i * 60_000_000_000), 100, 4, "B",
                           "MNQU6"))
    state, _ = accumulate(curve_rows, prereg)
    curves = volume_vs_count(state, {date0}, prereg)["MNQ"]
    check("count curve reads 2.0 and 1.0 per minute",
          abs(curves["count_per_min_by_hour"][17] - 2.0) < 1e-9
          and abs(curves["count_per_min_by_hour"][18] - 1.0) < 1e-9)
    check("volume curve reads 2.0 and 4.0 per minute",
          abs(curves["volume_per_min_by_hour"][17] - 2.0) < 1e-9
          and abs(curves["volume_per_min_by_hour"][18] - 4.0) < 1e-9)
    check("zero troughs make the ratio of ratios unavailable",
          curves["volume_over_count_ptt"] == UNAVAILABLE)
    sparse_rows = [(at(date0, i * 30_000_000_000), 100, 1, "B", "MNQU6")
                   for i in range(120)]
    state, _ = accumulate(sparse_rows, prereg)
    sparse = volume_vs_count(state, {date0}, prereg)["MNQ"]
    check("an empty open hour makes peak-to-trough infinite",
          math.isinf(sparse["count_peak_to_trough"])
          and math.isinf(sparse["volume_peak_to_trough"]))
    outside_set = volume_vs_count(state, set(), prereg)["MNQ"]
    check("a session outside the paired usable set is reported, not used",
          outside_set["sessions"] == 0
          and outside_set["sessions_available"] == 1
          and outside_set["unused_sessions"] == [date0])
    tt = tables(all_names)
    before = evaluate_all(tt, prereg, 10)
    after = evaluate_all(tt, prereg, 10)
    check("evaluate_all sees only the fourteen targets",
          before == after)
    check("no correction output shares a target name",
          not (set(curves) & set(REGISTERED_TARGETS)))

    failed = [name for name, ok in checks if not ok]
    print("\n%d check(s), %d failed" % (len(checks), len(failed)))
    if failed:
        for name in failed:
            print("  FAIL", name)
        raise SystemExit(1)
    print("selftest PASS; the wave 1 seal was not touched")


def _validate_dict(prereg_dict):
    """Run load_prereg's validation on an in-memory dict, for fixtures."""
    path = os.path.join(SELFTEST_DIR, "prereg-fixture.json")
    os.makedirs(SELFTEST_DIR, exist_ok=True)
    with open(path, "w") as fh:
        json.dump(prereg_dict, fh)
    return load_prereg(path)


def main():
    sys.stdout.reconfigure(line_buffering=True)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=("selftest", "preflight", "report"))
    parser.add_argument("directory", nargs="?")
    args = parser.parse_args()
    try:
        if args.mode == "selftest":
            selftest()
            return
        if not args.directory:
            raise Refusal("%s needs the delivered directory" % args.mode)
        prereg = load_prereg()
        if args.mode == "preflight":
            sys.exit(mode_preflight(args.directory, prereg))
        sys.exit(mode_report(args.directory, prereg))
    except Refusal as exc:
        # Exit 2, distinct from a verdict FAIL's exit 1: a refusal means
        # the frozen rule was never evaluated at all.
        print("REFUSED: %s" % exc, file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
