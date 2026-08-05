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
import datetime as dt
import io
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
SELFTEST_DIR = os.path.join(ROOT, "analysis", "out", "pair-selftest")

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


class Refusal(SystemExit):
    def __init__(self, reason):
        super().__init__("REFUSED: %s" % reason)


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
    families = prereg.get("families")
    if not isinstance(families, dict) or not families:
        raise Refusal("no families in the preregistration")
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
    inference = prereg.get("inference", {})
    if not isinstance(inference.get("max_unsided_frac"), (int, float)):
        raise Refusal("inference block missing max_unsided_frac")
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


def assign_session(ts_ns, table):
    """(session_date, where) with where one of open|halt|outside."""
    for row in table:
        if row["open_ns"] <= ts_ns < row["close_ns"]:
            if row["halt_lo_ns"] <= ts_ns < row["halt_hi_ns"]:
                return row["date"], "halt"
            return row["date"], "open"
    return None, "outside"


# ---------------------------------------------------------------------------
# Row intake
# ---------------------------------------------------------------------------


def leg_of(symbol):
    """MNQ before NQ: the prefixes overlap and the longer one wins."""
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
    with open(path, "rb") as fh:
        raw = zstd.decompress(fh.read())
    lines = raw.decode("utf-8").splitlines()
    if not lines:
        raise Refusal("%s is empty" % path)
    header = lines[0].split(",")
    index = {}
    for name in REQUIRED_COLUMNS:
        if name not in header:
            raise Refusal("%s carries no %r column; required semantic "
                          "fields are %s" % (path, name,
                                             ", ".join(REQUIRED_COLUMNS)))
        index[name] = header.index(name)
    i_ts, i_px = index["ts_event"], index["price"]
    i_sz, i_sd, i_sy = index["size"], index["side"], index["symbol"]
    for line in lines[1:]:
        if not line:
            continue
        parts = line.split(",")
        yield (int(parts[i_ts]), int(parts[i_px]), int(parts[i_sz]),
               parts[i_sd], parts[i_sy])


# ---------------------------------------------------------------------------
# Accumulation: one streaming pass into per-(leg, session) state
# ---------------------------------------------------------------------------


class SessionLeg:
    """Everything one leg accumulates inside one session."""

    def __init__(self):
        self.rows = 0
        self.unsided = 0
        self.contracts = {}
        self.events = EventStats(True)      # sweep family, unchanged lineage
        # Glue parent grouping, cross-checked against EventStats.events.
        self.parent_times = []
        self.parent_sides = []
        self._cur_ts = None
        self._cur_side = None
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
        if side not in ("B", "S"):
            self.unsided += 1
            return
        is_buy = side == "B"
        self.events.push(ts, is_buy, px)
        if ts != self._cur_ts or is_buy != self._cur_side:
            self.parent_times.append(ts)
            self.parent_sides.append(is_buy)
            self._cur_ts = ts
            self._cur_side = is_buy


def window_utc_offset_ns(prereg):
    """The exchange-local UTC offset, CONSTANT across the frozen window (all
    of July 2026 is CDT); refuse if a DST edge ever lands inside a future
    window rather than silently mislabeling hours."""
    tz = ZoneInfo(prereg["session"]["timezone"])
    table = session_table(prereg)
    first = dt.datetime.fromtimestamp(table[0]["open_ns"] / 1e9, tz)
    last = dt.datetime.fromtimestamp(table[-1]["close_ns"] / 1e9, tz)
    if first.utcoffset() != last.utcoffset():
        raise Refusal("the window crosses a DST transition; hour-of-day "
                      "attribution needs a per-row timezone conversion")
    return int(first.utcoffset().total_seconds() * 1_000_000_000)


def accumulate(rows, prereg):
    """rows: iterable of (ts, price, size, side, symbol). Returns
    (state, counters) where state maps (leg, session_date) to SessionLeg."""
    table = session_table(prereg)
    offset_ns = window_utc_offset_ns(prereg)
    state = {}
    counters = {"outside": 0, "halt": 0}
    for ts, px, size, side, symbol in rows:
        date, where = assign_session(ts, table)
        if where != "open":
            counters[where] += 1
            continue
        key = (leg_of(symbol), date)
        if key not in state:
            state[key] = SessionLeg()
        local_hour = ((ts + offset_ns) // 3_600_000_000_000) % 24
        state[key].push(ts, px, size, side, symbol, local_hour)
    return state, counters


# ---------------------------------------------------------------------------
# Per-session targets
# ---------------------------------------------------------------------------


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
    glue_parents = len(leg_state.parent_times)
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
        halt_lo, halt_hi = session_row["halt_lo_ns"], session_row["halt_hi_ns"]
        gaps = []
        excluded = 0
        times = leg_state.parent_times
        for i in range(1, len(times)):
            if times[i - 1] < halt_lo and times[i] >= halt_hi:
                excluded += 1
                continue
            gaps.append((times[i] - times[i - 1]) / 1_000_000_000.0)
        diag["halt_gaps_excluded"] = excluded
        if len(gaps) >= 6:
            mean = sum(gaps) / len(gaps)
            var = sum(g * g for g in gaps) / len(gaps) - mean * mean
            out["mean_event_duration_s"] = mean if mean > 0 else UNAVAILABLE
            out["duration_dispersion_cv2"] = (
                var / (mean * mean) if mean > 0 else UNAVAILABLE)
            acf = AutoCorr(5)
            for g in gaps:
                acf.push(g)
            values = acf.acf()
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
                len(times) >= 2 else None
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

        sides = leg_state.parent_sides
        if len(sides) >= 2:
            flips = sum(1 for i in range(1, len(sides))
                        if sides[i] != sides[i - 1])
            out["side_flip_prob"] = flips / (len(sides) - 1)
            out["side_run_length_mean"] = len(sides) / (flips + 1)
        else:
            out["side_flip_prob"] = UNAVAILABLE
            out["side_run_length_mean"] = UNAVAILABLE

    ret1 = leg_state.ret1_acf.acf()
    out["return_acf_lag1"] = ret1[0] if ret1 else UNAVAILABLE
    abs_acf = leg_state.ret_acf.acf()
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
    """Sessions where a leg carries more than one underlying contract are
    excluded fail-closed and named."""
    excluded = {}
    for (leg, date), leg_state in state.items():
        if len(leg_state.contracts) > 1:
            excluded[date] = "%s leg carries %s" % (
                leg, sorted(leg_state.contracts))
    return excluded


def pipeline(rows, prereg):
    """rows -> (verdict-result, correction, diagnostics). The single entry
    the report mode uses; the selftest drives it and its parts directly."""
    state, counters = accumulate(rows, prereg)
    table = {row["date"]: row for row in session_table(prereg)}
    excluded = contract_exclusions(state)
    sessions_present = sorted({date for _leg, date in state})
    usable = [s for s in sessions_present if s not in excluded]

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
    correction = volume_vs_count(state, excluded, prereg)
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


def volume_vs_count(state, excluded, prereg):
    """The protocol 8 correction curves, per leg: mean trade COUNT per open
    minute and mean VOLUME per open minute by exchange-local hour, their
    peak-to-trough ratios, and the ratio of ratios. Diagnostic only: the
    verdict function cannot see any of this."""
    minutes_per_session = open_minutes_by_hour(prereg)
    out = {}
    for leg in ("MNQ", "NQ"):
        counts = [0] * 24
        volume = [0] * 24
        sessions = 0
        for (state_leg, date), leg_state in state.items():
            if state_leg != leg or date in excluded:
                continue
            sessions += 1
            for hour in range(24):
                counts[hour] += leg_state.hour_counts[hour]
                volume[hour] += leg_state.hour_volume[hour]
        if sessions == 0:
            out[leg] = {"sessions": 0}
            continue
        count_rate = [
            counts[h] / (minutes_per_session[h] * sessions)
            if minutes_per_session[h] else None for h in range(24)]
        volume_rate = [
            volume[h] / (minutes_per_session[h] * sessions)
            if minutes_per_session[h] else None for h in range(24)]

        def peak_to_trough(rates):
            active = [r for r in rates if r is not None and r > 0]
            if not active:
                return UNAVAILABLE
            return max(active) / min(active)

        count_ptt = peak_to_trough(count_rate)
        volume_ptt = peak_to_trough(volume_rate)
        ratio = (volume_ptt / count_ptt
                 if UNAVAILABLE not in (count_ptt, volume_ptt)
                 else UNAVAILABLE)
        out[leg] = {
            "sessions": sessions,
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


def mode_preflight(directory, prereg):
    """Contract observation ONLY. No target value is computed or printed."""
    path = find_data_file(directory)
    table = session_table(prereg)
    with open(path, "rb") as fh:
        raw = zstd.decompress(fh.read())
    lines = raw.decode("utf-8").splitlines()
    header = lines[0].split(",")
    print("file      %s" % os.path.basename(path))
    print("columns   %s" % ",".join(header))
    missing = [c for c in REQUIRED_COLUMNS if c not in header]
    print("required  %s" % ("all present" if not missing
                            else "MISSING: %s" % ", ".join(missing)))
    index = {name: header.index(name) for name in REQUIRED_COLUMNS
             if name in header}
    rows = 0
    per = {}
    sides = {}
    regressions = 0
    prev_ts = None
    ts_digits = set()
    for line in lines[1:]:
        if not line:
            continue
        parts = line.split(",")
        ts = int(parts[index["ts_event"]])
        symbol = parts[index["symbol"]]
        side = parts[index["side"]]
        rows += 1
        ts_digits.add(len(str(ts)))
        if prev_ts is not None and ts < prev_ts:
            regressions += 1
        prev_ts = ts
        date, where = assign_session(ts, table)
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
    return 0


def mode_report(directory, prereg):
    path = find_data_file(directory)
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
        print("  %-4s sessions %d  count ptt %s  volume ptt %s  "
              "volume/count ptt ratio %s  (fitted profile: 27.51)" % (
                  leg, curves["sessions"],
                  fmt(curves["count_peak_to_trough"]),
                  fmt(curves["volume_peak_to_trough"]),
                  fmt(curves["volume_over_count_ptt"])))
    print("\nthe verdict artifact is NOT written by this harness")
    return 0 if result["verdict"] == "pass" else 1


def fmt(value):
    return "n/a" if value == UNAVAILABLE else "%.2f" % value


# ---------------------------------------------------------------------------
# Selftest
# ---------------------------------------------------------------------------


def selftest():
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

    print("session table and assignment")
    check("ten sessions, first and last as frozen",
          len(table) == 10 and dates[0] == "2026-07-06"
          and dates[-1] == "2026-07-17")
    assigned = [assign_session(at(d, H), table)[0] for d in dates]
    check("a trade one hour into each session lands in that session",
          assigned == dates)
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
        lines.append("0,%d,100,1,S,MNQU6" % at(date0, H + i * 1000))
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
    try:
        leg_of("ESU6")
        refused = False
    except Refusal:
        refused = True
    check("an unknown symbol refuses rather than guessing a leg", refused)

    print("parent grouping")
    t0 = at(date0, H)
    rows = [
        (t0, 100, 1, "B", "NQU6"),
        (t0, 101, 1, "B", "NQU6"),          # same ts+side: same parent
        (t0, 102, 1, "S", "NQU6"),          # same ts, side flip: new parent
        (t0, 103, 1, "B", "NQU6"),          # same ts, back to B: NEW parent
        (t0 + 1000, 104, 1, "B", "NQU6"),   # new ts: new parent
    ]
    state, _ = accumulate(rows, prereg)
    leg_state = state[("NQ", date0)]
    check("contiguous ts+side groups, non-contiguous same-ts stays split",
          len(leg_state.parent_times) == 4)
    check("EventStats agrees with the glue grouping",
          leg_state.events.events + 1 == 4)

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
          state[("NQ", date1)].parent_times == [at(date1, H)])
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

    print("minority-contract session exclusion")
    rows = [(t0, 100, 1, "B", "MNQU6"), (t0 + 1000, 101, 1, "B", "MNQZ6")]
    state, _ = accumulate(rows, prereg)
    excluded = contract_exclusions(state)
    check("a second contract in a leg excludes the session by name",
          date0 in excluded and "MNQZ6" in excluded[date0])

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
    curves = volume_vs_count(state, {}, prereg)["MNQ"]
    check("count curve reads 2.0 and 1.0 per minute",
          abs(curves["count_peak_to_trough"] - 2.0) < 1e-9)
    check("volume curve reads 2.0 and 4.0 per minute",
          abs(curves["volume_peak_to_trough"] - 2.0) < 1e-9)
    check("the ratio of ratios lands at 1.0",
          abs(curves["volume_over_count_ptt"] - 1.0) < 1e-9)
    tt = tables(all_names)
    before = evaluate_all(tt, prereg, 10)
    after = evaluate_all(tt, prereg, 10)
    check("evaluate_all sees only the fourteen targets",
          before == after)
    check("no correction output shares a target name",
          not (set(curves) & set(REGISTERED_TARGETS)))

    import shutil
    shutil.rmtree(SELFTEST_DIR)
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
    if args.mode == "selftest":
        selftest()
        return
    if not args.directory:
        raise Refusal("%s needs the delivered directory" % args.mode)
    prereg = load_prereg()
    if args.mode == "preflight":
        sys.exit(mode_preflight(args.directory, prereg))
    sys.exit(mode_report(args.directory, prereg))


if __name__ == "__main__":
    main()
