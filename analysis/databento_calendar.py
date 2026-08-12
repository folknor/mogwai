#!/usr/bin/env python3
"""Build the design-month session calendar from unbilled vendor metadata.

Step 0 of the Stage M power analysis needs the ACTUAL trading sessions of the
design months, not a declared holiday table. Two candidate metadata sources
were probed; only one works:

  metadata.get_dataset_condition   REJECTED. It reports per-date dataset
                                   ingestion status, not venue activity:
                                   2025-12-25, 2026-01-01 and every Saturday
                                   and Sunday all report "available". Using it
                                   would place Christmas in the session
                                   population, which is worse than a declared
                                   holiday table because the error is
                                   invisible downstream.
  metadata.get_record_count        USED. A date whose CME session carries no
                                   trades returns 0, and a shortened session
                                   returns a proportionally reduced count, so
                                   the calendar is graded rather than binary.
                                   2026-07-03, the observed Independence Day
                                   early close, returns roughly a quarter of
                                   the weekday norm.

Dates are resolved through databento_price.session_bounds_utc, so date D means
the CME session running D-1 17:00 to D 16:00 Central, DST included - the same
session convention the rest of the intake uses. Saturday and Sunday therefore
fall out as zeros on their own, with no weekday table.

WHICH MONTHS MAY BE SWEPT is a contract question, not a convenience one, and
this script fails closed on it. Under notes/successor-contract.md Amendment 1
provision 3, a per-date record count is a per-session observable and therefore
INSPECTION of the month it covers. Sweeping a sealed month would contaminate
it before Stage C exists to spend it honestly. Provision 1 separately holds
2025-08 unread as an incomplete_month_delivery. The refusals below are those
two provisions in executable form; they are not tunable from the command line.

The classification thresholds live in the ARTIFACT, not in this file. They
shape the session population the power analysis runs over, so they must be
inspectable in the output rather than recoverable only by reading the code
that produced it.

Usage:
    python3 -u analysis/databento_calendar.py sweep
    python3 -u analysis/databento_calendar.py sweep --refresh
    python3 -u analysis/databento_calendar.py show

Every response is cached by databento_price, so a re-run is free and offline.
Metadata calls are unbilled and read no delivered file.
"""

import argparse
import datetime as dt
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databento_price as dp  # session_bounds_utc, request, DATASET
import databento_seal_ledger as seal  # the single source for roles

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ARTIFACT = os.path.join(ROOT, "analysis", "databento-calendar.json")

TOOL_VERSION = 1
ARTIFACT_VERSION = 1

# The instrument and schema the design corpus is pulled as. TBBO carries one
# record per trade with the book at trade time, so its record count IS the
# session's trade count - the observable the power analysis sizes against.
SYMBOL = "MNQ.v.0"
STYPE_IN = "continuous"
SCHEMA = "tbbo"

# The instrument these counts describe, for role derivation.
INSTRUMENT = "MNQ"

# Months held UNREAD for a reason the ROLE does not express. 2025-08 carries
# the open new-design role, so role alone would admit it; Amendment 1
# provision 1 holds its entitlement-truncated slice unread regardless. This
# is the only such case, and it is a delivery-state fact the calendar sweep
# cannot derive because no file has been delivered to it.
UNREAD_MONTHS = {
    "2025-08": "incomplete_month_delivery; Amendment 1 provision 1 holds the "
               "covered slice UNREAD and unavailable to Stage M unless a "
               "later reviewed amendment defines a partial-month estimand",
}

# Sweepable months are DERIVED, not transcribed. databento_seal_ledger's
# MNQ_ASSIGNMENT is the single source for roles, and a third copy of it here
# would go stale the next time an amendment moves the table - Amendment 1
# already moved it once, adding 2026-08. Deriving means a newly sealed month
# drops out of the sweep automatically, and a newly added month fails closed
# rather than being silently omitted.
def sweepable_months():
    """Every acquisition month whose role is open, minus those held unread.
    Sorted, so the artifact's month order is stable."""
    out = []
    for month in sorted(seal.ACQUISITION_MONTHS):
        if month in UNREAD_MONTHS:
            continue
        if seal.derive_role(INSTRUMENT, month) in seal.OPEN_ROLES:
            out.append(month)
    return out


def refusal_reason(month):
    """Why a month is not sweepable, or None when it is."""
    if month in UNREAD_MONTHS:
        return UNREAD_MONTHS[month]
    if month not in seal.ACQUISITION_MONTHS:
        return "outside the acquisition range"
    role = seal.derive_role(INSTRUMENT, month)
    if role in seal.OPEN_ROLES:
        return None
    return ("role %s is sealed; Amendment 1 provision 3 - a per-date record "
            "count is a per-session observable and so is inspection. A "
            "sealed month's schedule materializes exactly once, "
            "mechanically, inside the blinded Stage C harness at unseal"
            % role)

# DECLARED CLASSIFICATION, copied verbatim into the artifact. A session is
# graded against the month's own median trading session rather than an
# absolute count, because monthly volume varies by more than the half-session
# distinction being drawn: an absolute boundary calibrated on a busy month
# would grade a quiet month's full sessions as half sessions.
CLASSIFICATION = {
    "method": "fraction_of_month_median_trading_session",
    "closure_max_records": 0,
    "half_session_upper_fraction": 0.5,
    "reference": "median record count over the month's non-closure dates",
    "grades": {
        "closure": "records equal to closure_max_records",
        "half_session": "records below half_session_upper_fraction times the "
                        "reference",
        "full_session": "every remaining date",
    },
    "rationale": "These boundaries decide the session population the Stage M "
                 "step-0 power analysis runs over, so they are declared in "
                 "the artifact rather than held as constants in "
                 "analysis/databento_calendar.py, and are inspectable "
                 "without reading the generating code.",
}


class Refusal(SystemExit):
    """A named, fail-closed refusal. Exits nonzero with the reason."""

    def __init__(self, reason):
        super().__init__("REFUSED: %s" % reason)


def durable_json_write(path, payload):
    """Atomic AND durable, like the job and seal ledgers. This artifact
    declares the classification thresholds the Stage M session population is
    built on, so it is load-bearing rather than a cache that can be
    regenerated for free - it inherits the ledgers' durability, not the
    pricing cache's casualness."""
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(payload, fh, indent=1, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
    dir_fd = os.open(os.path.dirname(path) or ".", os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)


def month_dates(month):
    """Every calendar date in a YYYY-MM month, as ISO strings."""
    year, mon = (int(x) for x in month.split("-"))
    first = dt.date(year, mon, 1)
    nxt = dt.date(year + 1, 1, 1) if mon == 12 else dt.date(year, mon + 1, 1)
    out, day = [], first
    while day < nxt:
        out.append(day.isoformat())
        day += dt.timedelta(days=1)
    return out


def guard(month):
    """Refuse an unsweepable month before any request is built."""
    reason = refusal_reason(month)
    if reason is not None:
        raise Refusal("%s may not be swept: %s" % (month, reason))


def session_record_count(date_str):
    """Trades in the CME session labelled by this date, or None when the
    count is unreadable. None is never collapsed to zero: zero means the
    venue did not trade, unreadable means we do not know, and conflating
    them would silently delete a session from the population."""
    nxt = (dt.date.fromisoformat(date_str) + dt.timedelta(days=1)).isoformat()
    start, end = dp.session_bounds_utc(date_str, nxt)
    params = {"dataset": dp.DATASET, "symbols": SYMBOL, "schema": SCHEMA,
              "stype_in": STYPE_IN, "start": start, "end": end}
    value = dp.as_number(dp.request("metadata.get_record_count", params))
    return None if value is None else int(value)


def classify(counts):
    """Grade each date against the month's own median trading session, per
    the declared CLASSIFICATION. Unreadable dates grade as null rather than
    as any session type."""
    trading = sorted(c for c in counts.values()
                     if c is not None and c > CLASSIFICATION[
                         "closure_max_records"])
    reference = None
    if trading:
        mid = len(trading) // 2
        reference = (trading[mid] if len(trading) % 2
                     else (trading[mid - 1] + trading[mid]) / 2.0)
    cutoff = (None if reference is None
              else reference * CLASSIFICATION["half_session_upper_fraction"])
    graded = {}
    for date_str, count in counts.items():
        if count is None:
            graded[date_str] = None
        elif count <= CLASSIFICATION["closure_max_records"]:
            graded[date_str] = "closure"
        elif cutoff is not None and count < cutoff:
            graded[date_str] = "half_session"
        else:
            graded[date_str] = "full_session"
    return graded, reference


def sweep_month(month, role):
    guard(month)
    counts = {}
    unreadable = []
    for date_str in month_dates(month):
        count = session_record_count(date_str)
        counts[date_str] = count
        if count is None:
            unreadable.append(date_str)
    graded, reference = classify(counts)
    tally = {}
    for grade in graded.values():
        label = "unreadable" if grade is None else grade
        tally[label] = tally.get(label, 0) + 1
    print("%-8s %-13s full %2d  half %2d  closed %2d  unreadable %d" % (
        month, role, tally.get("full_session", 0),
        tally.get("half_session", 0), tally.get("closure", 0),
        tally.get("unreadable", 0)))
    return {
        "role": role,
        "median_trading_session_records": reference,
        "tally": tally,
        "unreadable_dates": unreadable,
        "dates": {d: {"records": counts[d], "grade": graded[d]}
                  for d in sorted(counts)},
    }


def mode_sweep(args):
    if args.refresh:
        dp.REFRESH = True
    months = {}
    for month in sweepable_months():
        months[month] = sweep_month(month,
                                    seal.derive_role(INSTRUMENT, month))
    excluded = {m: refusal_reason(m) for m in sorted(seal.ACQUISITION_MONTHS)
                if refusal_reason(m) is not None}
    payload = {
        "_version": ARTIFACT_VERSION,
        "tool_version": TOOL_VERSION,
        "generated_at": dt.datetime.now(dt.UTC).isoformat(),
        "source": {
            "endpoint": "metadata.get_record_count",
            "dataset": dp.DATASET,
            "symbols": SYMBOL,
            "schema": SCHEMA,
            "stype_in": STYPE_IN,
            "session_convention": "date D is the CME session from D-1 17:00 "
                                  "to D 16:00 Central, DST included, via "
                                  "databento_price.session_bounds_utc",
            "billing": "metadata endpoints are unbilled; no delivered file "
                       "is opened",
        },
        "excluded_months": excluded,
        "classification": CLASSIFICATION,
        "months": months,
        # A partial sweep must not look like a complete one. Without this the
        # only tell is a non-empty unreadable_dates buried per month, while
        # the artifact carries the same version marker either way.
        "sweep_complete": not any(m["unreadable_dates"]
                                  for m in months.values()),
    }
    durable_json_write(ARTIFACT, payload)
    total = sum(m["tally"].get("full_session", 0)
                + m["tally"].get("half_session", 0)
                for m in months.values())
    print("\n%d month(s), %d trading session(s) -> %s" % (
        len(months), total, ARTIFACT))
    unreadable = sum(len(m["unreadable_dates"]) for m in months.values())
    if unreadable:
        print("%d unreadable date(s); recorded as null, never as zero" %
              unreadable)
        raise SystemExit(1)


def mode_show():
    if not os.path.exists(ARTIFACT):
        raise Refusal("no artifact at %s; run sweep first" % ARTIFACT)
    with open(ARTIFACT) as fh:
        payload = json.load(fh)
    # Validate the version fatally, as the ledgers do. Reading a future or
    # unversioned artifact's "months" and printing it as though the schema
    # matched is how a silently-changed layout becomes a wrong session count.
    if payload.get("_version") != ARTIFACT_VERSION:
        raise Refusal("%s has version %r, expected %d" % (
            ARTIFACT, payload.get("_version"), ARTIFACT_VERSION))
    print("%s  generated %s" % (ARTIFACT, payload.get("generated_at")))
    if not payload.get("sweep_complete", False):
        print("PARTIAL SWEEP: some dates were unreadable")
    print("%-8s %-13s %6s %6s %6s  %s" % (
        "month", "role", "full", "half", "closed", "median records"))
    for month in sorted(payload.get("months", {})):
        entry = payload["months"][month]
        tally = entry["tally"]
        median = entry["median_trading_session_records"]
        print("%-8s %-13s %6d %6d %6d  %s" % (
            month, entry["role"], tally.get("full_session", 0),
            tally.get("half_session", 0), tally.get("closure", 0),
            "n/a" if median is None else "%.0f" % median))


def main():
    sys.stdout.reconfigure(line_buffering=True)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["sweep", "show"])
    parser.add_argument("--refresh", action="store_true",
                        help="bypass the databento_price response cache")
    args = parser.parse_args()
    if args.mode == "sweep":
        mode_sweep(args)
    else:
        mode_show()


if __name__ == "__main__":
    main()
