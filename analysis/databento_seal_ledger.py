#!/usr/bin/env python3
"""The seal ledger, assignment-rooted per Amendment 2.

notes/successor-contract.md seals months of market data so that final
validation runs against evidence the design process never touched. This
module is the audit record that makes sealing mechanical, and the guards
that keep it honest.

WHY ASSIGNMENTS ARE THE ROOT. Version 1 keyed everything to a DELIVERED
FILE, and Amendment 2 replaced that because contract obligations attach at
ROLE ASSIGNMENT, which has existed since signature, not at delivery. The
concrete failure: Amendment 1 provision 3 made the seal information-level
and named vendor per-date record counts an inspection channel, so the
authorized design-month calendar sweep inspected months whose files are not
yet delivered - and a file-keyed ledger had no row to record it against. The
sharper corollary is what forced the repair: the same sweep against sealed
June would be contamination, and the ledger could not have recorded even
that. The detection instrument was blind exactly where bytes have not
landed.

THE THREE LEVELS.

  ASSIGNMENTS   immutable from contract signature. One per contract track,
                dataset, instrument and month. Roles live here.
  DELIVERIES    append-only children of an assignment. One per delivered
                file, with its own hash and provenance - never collapsed
                into the month.
  INSPECTIONS   append-only children of an assignment. Every act the
                contract classifies as looking, whether or not a file
                exists to look at.

PERMISSION IS DERIVED FROM ASSIGNMENT PLUS SCHEMA, never from a stored
state and never from the role alone. That is what keeps a new-design
month's TBBO open while its MBP-1 stays sealed.

STATE IS DERIVED, NOT STORED AS ONE ENUM. Spent, contaminated and inspected
are different dimensions; a single state machine collapses them and erases
history. Nothing here has a writable state field.

AUTHORITY IDENTITIES ARE IMMUTABLE SNAPSHOTS. A contract hash may never mean
"hash the mutable file on disk": landing Amendment 2 changed that hash, and
every earlier authorization would have appeared invalid. The identities
below are git blob ids of the exact signed snapshots, which are
content-addressed and cannot drift.

Usage:
    python3 -u analysis/databento_seal_ledger.py selftest
    python3 -u analysis/databento_seal_ledger.py migrate
    python3 -u analysis/databento_seal_ledger.py audit
    python3 -u analysis/databento_seal_ledger.py status
    python3 -u analysis/databento_seal_ledger.py verify
"""

import argparse
import datetime as dt
import hashlib
import itertools
import json
import os
import sys
import urllib.parse
from fractions import Fraction

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER_FILE = os.path.join(ROOT, "analysis", "databento-seal-ledger.json")
CALENDAR_ARTIFACT = os.path.join(ROOT, "analysis",
                                 "databento-calendar.json")
AUDIT_ARTIFACT = os.path.join(ROOT, "analysis",
                              "databento-seal-audit.json")
SELFTEST_DIR = os.path.join(ROOT, "analysis", "out", "seal-ledger-selftest")

LEDGER_VERSION = 2
TOOL_VERSION = 2

# IMMUTABLE AUTHORITY SNAPSHOTS. Each is the git blob id of
# notes/successor-contract.md as it stood at the commit that signed that
# authority - content-addressed, so it identifies exact bytes and cannot be
# changed by editing the working file. Recomputing these from the file on
# disk is precisely what Amendment 2 forbids.
AUTHORITIES = {
    "successor-contract-base": {
        "document": "notes/successor-contract.md",
        "commit": "45ae167",
        "git_blob": "61d1f135670a6ba6c40046d9af66ad5e9814ba6e",
        "signed": "2026-08-12",
        "signatory": "codex session 019ff4db-a23c-7bc1-907f-8921b3add799",
    },
    "successor-contract-amendment-1": {
        "document": "notes/successor-contract.md",
        "commit": "4f66f16",
        "git_blob": "f9c911364708d1e2d46b2ba2bdc0b4016f02b077",
        "signed": "2026-08-12",
        "signatory": "codex session 019ff4db-a23c-7bc1-907f-8921b3add799",
        "scope": "partial-month delivery, 2026-08, seal channels",
    },
    "successor-contract-amendment-2": {
        "document": "notes/successor-contract.md",
        "commit": "af9c6c4",
        "git_blob": "7d6cf722dee692af990bdaffe6cbba472c0ede75",
        "signed": "2026-08-12",
        "signatory": "codex session 019ff53d-5b7d-77c2-b759-4d7d7834c7d6",
        "scope": "the assignment-rooted ledger and its one-time migration",
    },
    "successor-contract-amendment-3": {
        "document": "notes/successor-contract.md",
        "commit": "426a7ad",
        "git_blob": "26b8b60b577c0c85a77d50ae57240dfceb7f6232",
        "signed": "2026-08-12",
        "signatory": "codex session 019ff53d-5b7d-77c2-b759-4d7d7834c7d6",
        "scope": "aggregate metadata is not inspection on "
                 "informational-granularity grounds, and the frozen "
                 "reconstruction-closure check that adjudicates it",
    },
}

BASE = "successor-contract-base"
AMEND1 = "successor-contract-amendment-1"
AMEND2 = "successor-contract-amendment-2"
# The authority for the adjudication criterion and its consequences. Bound
# into every adjudication event and into the audit provenance: the cache and
# checker hashes say WHICH bytes and WHICH code produced a verdict, but
# neither identifies the signed text that made that code authoritative.
AMEND3 = "successor-contract-amendment-3"

# Contract tracks. Shared bytes, separate science: the successor contract
# governs MNQ TBBO, the MBP-1 preservation ledger governs every MBP-1 file,
# and the MES-borrow track governs ES and MES. A month therefore has one
# assignment PER TRACK, which is how one month's TBBO stays open while its
# MBP-1 stays sealed without folding schema into the role.
TRACK_SUCCESSOR = "successor"
TRACK_MBP1 = "mbp1-preservation"
TRACK_BORROW = "mes-borrow"
TRACK_SCHEMAS = {
    TRACK_SUCCESSOR: ("tbbo",),
    TRACK_MBP1: ("mbp-1",),
    TRACK_BORROW: ("tbbo", "mbp-1"),
}
# Only the successor track can ever be open. Nothing signed authorizes an
# ES/MES or MBP-1 content read under this contract.
OPEN_TRACKS = (TRACK_SUCCESSOR,)

DATASET = "GLBX.MDP3"

ROLES = {
    "spent-design": "design evidence already consumed pre-contract",
    "new-design": "open for Stage M and Stage I",
    "primary-confirmation": "sealed; the Stage C month",
    "reserve-confirmation": "sealed; replaces primary ONLY on an INVALID "
                            "outcome that contaminated the primary",
    "unused-sealed": "sealed; may become design evidence only under a FUTURE "
                     "contract, never confirmation for this one",
}
OPEN_ROLES = ("new-design", "spent-design")

# THE MNQ ASSIGNMENT, immutable from signature. 2026-08 was added by
# Amendment 1 provision 2, so it carries a different authority chain.
MNQ_ASSIGNMENT = {
    "2025-08": "new-design",
    "2025-09": "new-design",
    "2025-10": "new-design",
    "2025-11": "new-design",
    "2025-12": "new-design",
    "2026-01": "new-design",
    "2026-02": "new-design",
    "2026-03": "new-design",
    "2026-04": "new-design",
    "2026-05": "reserve-confirmation",
    "2026-06": "primary-confirmation",
    "2026-07": "spent-design",
    "2026-08": "unused-sealed",
}
MONTHS_ADDED_BY_AMEND1 = ("2026-08",)

# The contract assignment time. The contract records a signature DATE and no
# clock time, so this is date precision and is never rewritten to a
# migration date - assignment_recorded_at carries that separately.
ROLE_ASSIGNED_AT = "2026-08-12T00:00:00+00:00"

INSTRUMENTS = ("MNQ", "ES", "MES")
BORROW_TRACK_INSTRUMENTS = ("ES", "MES")
ACQUISITION_MONTHS = frozenset(MNQ_ASSIGNMENT)
SCHEMAS = ("tbbo", "mbp-1")

DELIVERY_COMPLETE = "complete"
DELIVERY_INCOMPLETE = "incomplete_month_delivery"
DELIVERY_STATES = (DELIVERY_COMPLETE, DELIVERY_INCOMPLETE)

# Derived permissions. Not stored anywhere; computed on demand.
PERM_OPEN = "open"
PERM_SEALED = "sealed"
PERM_UNREAD_INCOMPLETE = "unread_incomplete"

# The closed inspection-channel vocabulary of Amendment 2. "other" exists so
# an unforeseen channel is recordable rather than unrepresentable, and it
# carries a description and a review precisely so it cannot become a quiet
# catch-all.
CHANNEL_FILE = "delivered-file-content"
CHANNEL_RECORD_COUNT = "vendor-record-count"
CHANNEL_OTHER = "other"
CHANNELS = (CHANNEL_FILE, CHANNEL_RECORD_COUNT, CHANNEL_OTHER)
# Channels that read a delivered file, and therefore REQUIRE a delivery
# reference. Every other channel FORBIDS one: a vendor-metadata inspection
# of an undelivered month has no file to point at, and letting it borrow one
# would misreport it as a file read.
FILE_CHANNELS = (CHANNEL_FILE,)

VERDICT_AUTHORIZED = "authorized"
VERDICT_UNAUTHORIZED = "unauthorized"
VERDICTS = (VERDICT_AUTHORIZED, VERDICT_UNAUTHORIZED)

# How well an observation time is known. Amendment 2 requires the migration
# to label the calendar sweep's timestamp an upper-bound proxy rather than
# claim it preserved what was never recorded.
BASIS_RECORDED = "recorded"
BASIS_UPPER_BOUND = "upper_bound_proxy"
BASIS_UNKNOWN = "unknown"
OBSERVED_BASES = (BASIS_RECORDED, BASIS_UPPER_BOUND, BASIS_UNKNOWN)


class Refusal(SystemExit):
    """A named, fail-closed refusal. Exits nonzero with the reason."""

    def __init__(self, reason):
        super().__init__("REFUSED: %s" % reason)


def now_iso():
    return dt.datetime.now(dt.UTC).isoformat()


def sha256_file(path):
    hasher = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(1024 * 1024):
            hasher.update(chunk)
    return hasher.hexdigest()


# ---------------------------------------------------------------------------
# Derivation: role, track, permission
# ---------------------------------------------------------------------------


def check_in(value, allowed, what):
    if value not in allowed:
        raise Refusal("%s %r is outside the closed set %s" % (
            what, value, ", ".join(str(a) for a in allowed)))
    return value


def derive_role(instrument, month):
    """The role for an instrument-month, from the contract's tables. Never a
    parameter, so no caller can supply a convenient one."""
    if month not in ACQUISITION_MONTHS:
        raise Refusal("%s %s is outside the acquisition range; the "
                      "assignment table is immutable from signature" % (
                          instrument, month))
    check_in(instrument, INSTRUMENTS, "instrument")
    if instrument in BORROW_TRACK_INSTRUMENTS:
        return "unused-sealed"
    return MNQ_ASSIGNMENT[month]


def derive_track(instrument, schema):
    """Which contract track governs this instrument-schema pair."""
    check_in(schema, SCHEMAS, "schema")
    check_in(instrument, INSTRUMENTS, "instrument")
    if instrument in BORROW_TRACK_INSTRUMENTS:
        return TRACK_BORROW
    return TRACK_MBP1 if schema == "mbp-1" else TRACK_SUCCESSOR


def authority_chain(month):
    """The authorities that assigned this month's role. 2026-08 exists only
    because Amendment 1 added it, so its chain names that amendment."""
    if month in MONTHS_ADDED_BY_AMEND1:
        return [BASE, AMEND1]
    return [BASE]


def derive_permission(assignment, schema, delivery_state=DELIVERY_COMPLETE):
    """Whether content of this assignment, at this schema, may be read.

    Derived from assignment plus schema every time it is asked, never read
    out of a stored field. Amendment 1 provision 1 holds an
    entitlement-truncated slice unread regardless of role."""
    check_in(schema, SCHEMAS, "schema")
    check_in(delivery_state, DELIVERY_STATES, "delivery state")
    track = assignment["contract_track"]
    if schema not in TRACK_SCHEMAS[track]:
        raise Refusal("schema %s is not governed by track %s" % (
            schema, track))
    if delivery_state == DELIVERY_INCOMPLETE:
        return PERM_UNREAD_INCOMPLETE
    if track not in OPEN_TRACKS:
        return PERM_SEALED
    return (PERM_OPEN if assignment["role"] in OPEN_ROLES else PERM_SEALED)


# ---------------------------------------------------------------------------
# Identities
# ---------------------------------------------------------------------------


def assignment_id(track, dataset, instrument, month):
    return "%s|%s|%s|%s" % (track, dataset, instrument, month)


def delivery_id(assign_id, schema, filename):
    return "%s|%s|%s" % (assign_id, schema, filename)


def event_id(assign_id, channel, ordinal):
    return "%s|%s|%04d" % (assign_id, channel, ordinal)


# ---------------------------------------------------------------------------
# Persistence
# ---------------------------------------------------------------------------


def empty_ledger():
    return {"assignments": {}, "deliveries": {}, "inspections": []}


def load_ledger(path=LEDGER_FILE):
    """An unreadable ledger is FATAL, never discarded: it is the only record
    of which bytes may be looked at, and of what has already been seen."""
    if not os.path.exists(path):
        return empty_ledger()
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (json.JSONDecodeError, OSError) as exc:
        raise Refusal("seal ledger %s unreadable (%s); fix it by hand" % (
            path, exc)) from exc
    if data.get("_version") != LEDGER_VERSION:
        raise Refusal("seal ledger %s has version %r, expected %d" % (
            path, data.get("_version"), LEDGER_VERSION))
    out = empty_ledger()
    for section in out:
        value = data.get(section)
        if value is None:
            continue
        if not isinstance(value, type(out[section])):
            raise Refusal("seal ledger %s section %s has the wrong shape" % (
                path, section))
        out[section] = value
    return out


def save_ledger(ledger, path=LEDGER_FILE):
    """Atomic and durable: a role assignment or an inspection record that
    does not survive a power cut is one that can be quietly re-made."""
    payload = {"_version": LEDGER_VERSION, "tool_version": TOOL_VERSION,
               "authorities": AUTHORITIES}
    payload.update(ledger)
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


# ---------------------------------------------------------------------------
# Assignments
# ---------------------------------------------------------------------------


def ensure_assignment(ledger, instrument, month, track,
                      recorded_at=None, supersedes=None):
    """Create the immutable assignment root, or return the existing one.

    role_assigned_at is the CONTRACT assignment time and is never rewritten
    to a migration date; assignment_recorded_at carries when this row was
    written. An existing row whose immutable fields disagree is a refusal,
    never an overwrite."""
    check_in(track, tuple(TRACK_SCHEMAS), "contract track")
    role = derive_role(instrument, month)
    aid = assignment_id(track, DATASET, instrument, month)
    record = {
        "assignment_id": aid,
        "contract_track": track,
        "dataset": DATASET,
        "instrument": instrument,
        "month": month,
        "role": role,
        "role_assigned_at": ROLE_ASSIGNED_AT,
        "authority": authority_chain(month),
        "representation_authority": AMEND2,
        "supersedes": supersedes,
    }
    existing = ledger["assignments"].get(aid)
    if existing is not None:
        # representation_authority is in here deliberately: an assignment
        # stripped of the authority that authorized its REPRESENTATION is as
        # corrupt as one stripped of the authority that assigned its role,
        # and either way it must not silently become the root for new
        # deliveries.
        immutable = ("contract_track", "dataset", "instrument", "month",
                     "role", "role_assigned_at", "authority", "supersedes",
                     "representation_authority")
        disagree = [f for f in immutable if existing.get(f) != record[f]]
        if disagree:
            raise Refusal("assignment %s already exists and disagrees on %s; "
                          "assignments are immutable from signature" % (
                              aid, ", ".join(disagree)))
        return existing
    record["assignment_recorded_at"] = recorded_at or now_iso()
    ledger["assignments"][aid] = record
    return record


def create_all_assignments(ledger, recorded_at=None):
    """Every assignment the signed tables imply, across all three tracks.

    Creating an assignment row does NOT make an undelivered month part of
    any delivered population; it records that a role exists and has since
    signature. Amendment 2 states that consequence explicitly."""
    made = 0
    for month in sorted(ACQUISITION_MONTHS):
        for instrument in INSTRUMENTS:
            for track in sorted(TRACK_SCHEMAS):
                if instrument in BORROW_TRACK_INSTRUMENTS:
                    if track != TRACK_BORROW:
                        continue
                elif track == TRACK_BORROW:
                    continue
                before = len(ledger["assignments"])
                ensure_assignment(ledger, instrument, month, track,
                                  recorded_at=recorded_at)
                made += len(ledger["assignments"]) - before
    return made


def get_assignment(ledger, instrument, month, track):
    aid = assignment_id(track, DATASET, instrument, month)
    assignment = ledger["assignments"].get(aid)
    if assignment is None:
        raise Refusal("no assignment %s; assignment roots must exist before "
                      "a delivery or an inspection can attach to them" % aid)
    return assignment


# ---------------------------------------------------------------------------
# Deliveries
# ---------------------------------------------------------------------------


def record_delivery(ledger, instrument, month, schema, filename, sha256,
                    job_id, delivered_at, delivery_state=DELIVERY_COMPLETE,
                    covered_interval=None, edge_timestamps=None,
                    coverage_quote=None, coverage_checked_at=None,
                    vendor_object=None):
    """Attach a delivered file to its assignment, at delivery.

    Idempotent for an identical redelivery, and a refusal for anything that
    disagrees: a matching hash alone would let a stale or hand-edited record
    pass for a bound one, after which the caller marks the job settled and
    the contradiction is never surfaced."""
    check_in(schema, SCHEMAS, "schema")
    check_in(delivery_state, DELIVERY_STATES, "delivery state")
    if not filename:
        raise Refusal("%s %s %s needs a delivered filename; the ledger "
                      "enumerates files" % (instrument, month, schema))
    if delivery_state == DELIVERY_INCOMPLETE and not (covered_interval
                                                      and edge_timestamps):
        raise Refusal("%s %s %s is an incomplete_month_delivery and must "
                      "record BOTH its covered interval and its edge "
                      "timestamps; Amendment 1 provision 1" % (
                          instrument, month, schema))
    if delivery_state == DELIVERY_COMPLETE and (covered_interval
                                                or edge_timestamps):
        raise Refusal("%s %s %s claims a complete delivery yet carries the "
                      "interval and edges only a truncated one has" % (
                          instrument, month, schema))
    track = derive_track(instrument, schema)
    assignment = get_assignment(ledger, instrument, month, track)
    did = delivery_id(assignment["assignment_id"], schema, filename)
    record = {
        "delivery_id": did,
        "assignment_id": assignment["assignment_id"],
        "schema": schema,
        "filename": filename,
        "vendor_object": vendor_object or filename,
        "sha256": sha256,
        "delivery_job": job_id,
        "delivered_at": delivered_at,
        "delivery_state": delivery_state,
        "covered_interval": covered_interval,
        "edge_timestamps": edge_timestamps,
        "coverage_quote_at_pull": coverage_quote,
        "coverage_checked_at": coverage_checked_at,
    }
    existing = ledger["deliveries"].get(did)
    if existing is not None:
        immutable = [f for f in record if f != "delivered_at"]
        disagree = [f for f in immutable if existing.get(f) != record[f]]
        if disagree:
            raise Refusal("delivery %s already recorded and this redelivery "
                          "disagrees on %s; a differing redelivery is never "
                          "an overwrite" % (did, ", ".join(sorted(disagree))))
        return existing
    ledger["deliveries"][did] = record
    return record


# ---------------------------------------------------------------------------
# Inspection events
# ---------------------------------------------------------------------------


def _next_ordinal(ledger, assign_id, channel):
    return sum(1 for e in ledger["inspections"]
               if e["assignment_id"] == assign_id and e["channel"] == channel)


def _append_event(ledger, assignment, channel, scope, observed_at,
                  observed_at_basis, authority, reading_process,
                  authorization_basis, verdict, delivery=None,
                  description=None, review=None, artifacts=None,
                  recorded_at=None):
    """The one writer of inspection events. Append-only: it never mutates or
    removes an existing event, so an unauthorized event can never be
    converted in place into an authorized one."""
    check_in(channel, CHANNELS, "inspection channel")
    check_in(verdict, VERDICTS, "authorization verdict")
    check_in(observed_at_basis, OBSERVED_BASES, "observed_at basis")
    if channel == CHANNEL_OTHER and not (description and review):
        raise Refusal("channel 'other' requires both a description and a "
                      "review; it exists so an unforeseen channel is "
                      "recordable, not as a quiet catch-all")
    if channel in FILE_CHANNELS and delivery is None:
        raise Refusal("channel %s reads a delivered file and REQUIRES a "
                      "delivery reference" % channel)
    if channel not in FILE_CHANNELS and delivery is not None:
        raise Refusal("channel %s is not a delivered-file channel and must "
                      "NOT carry a delivery reference; borrowing one would "
                      "misreport vendor metadata as a file read" % channel)
    if not authority or not reading_process:
        raise Refusal("an inspection event needs a named authority and a "
                      "named reading process, or the explicit string "
                      "'unknown' with a reason; silence is not a record")
    aid = assignment["assignment_id"]
    record = {
        "event_id": event_id(aid, channel, _next_ordinal(ledger, aid,
                                                         channel)),
        "assignment_id": aid,
        "delivery_id": None if delivery is None else delivery["delivery_id"],
        "channel": channel,
        "scope": scope,
        "observed_at": observed_at,
        "observed_at_basis": observed_at_basis,
        "recorded_at": recorded_at or now_iso(),
        "authority": authority,
        "reading_process": reading_process,
        "authorization_basis": authorization_basis,
        "authorization_verdict": verdict,
        "artifacts": list(artifacts or []),
        "description": description,
        "review": review,
    }
    ledger["inspections"].append(record)
    return record


def record_inspection(ledger, instrument, month, schema, channel, scope,
                      authority, reading_process, filename=None,
                      artifacts=None, description=None, review=None,
                      observed_at=None):
    """PROSPECTIVE record: an inspection about to happen, or just happened,
    which must be authorized first.

    This is the guarded entry point. It refuses when the assignment does not
    permit a read at this schema, which is exactly what makes it unusable
    for recording contamination - use record_retrospective_inspection for
    that, which is a separate operation by design."""
    track = derive_track(instrument, schema)
    assignment = get_assignment(ledger, instrument, month, track)
    delivery = None
    delivery_state = DELIVERY_COMPLETE
    if channel in FILE_CHANNELS:
        if not filename:
            raise Refusal("a %s inspection needs the filename it read"
                          % channel)
        did = delivery_id(assignment["assignment_id"], schema, filename)
        delivery = ledger["deliveries"].get(did)
        if delivery is None:
            raise Refusal("no delivery %s; a file cannot be read before it "
                          "is delivered and bound" % did)
        delivery_state = delivery["delivery_state"]
    permission = derive_permission(assignment, schema, delivery_state)
    if permission != PERM_OPEN:
        raise Refusal("%s %s %s derives permission %r under role %r on track "
                      "%s; no content read is authorized. A sealed month's "
                      "schedule materializes exactly once, mechanically, "
                      "inside the blinded Stage C harness at unseal" % (
                          instrument, month, schema, permission,
                          assignment["role"], assignment["contract_track"]))
    return _append_event(
        ledger, assignment, channel, scope,
        observed_at or now_iso(), BASIS_RECORDED, authority, reading_process,
        "derived permission %s at time of read" % permission,
        VERDICT_AUTHORIZED, delivery=delivery, description=description,
        review=review, artifacts=artifacts)


def record_retrospective_inspection(
        ledger, instrument, month, schema, channel, scope, observed_at,
        observed_at_basis, authority, reading_process, verdict,
        authorization_basis, filename=None, artifacts=None,
        description=None, review=None, track=None):
    """RETROSPECTIVE record: an inspection that already happened, recorded
    now, whatever its authorization.

    Deliberately does NOT call the permission guard. If it did, the guard
    that refuses to read a sealed month would also refuse to record that
    someone had - which would make contamination unrecordable in exactly the
    case the ledger exists to catch. Amendment 2 requires these to be
    separate operations, and requires that no unauthorized event can later
    be converted in place into an authorized one: this only ever appends.

    `track` may be given explicitly for events whose schema is a REQUEST
    schema rather than a deliverable one - a record-count request against
    trades, definition or statistics has no delivery schema to derive a
    track from, and guessing one would be worse than being told."""
    track = track or derive_track(instrument, schema)
    assignment = get_assignment(ledger, instrument, month, track)
    delivery = None
    if channel in FILE_CHANNELS:
        if not filename:
            raise Refusal("a %s event needs the filename it read" % channel)
        did = delivery_id(assignment["assignment_id"], schema, filename)
        delivery = ledger["deliveries"].get(did)
        if delivery is None:
            raise Refusal("no delivery %s to attach a file-channel event to"
                          % did)
    return _append_event(
        ledger, assignment, channel, scope, observed_at, observed_at_basis,
        authority, reading_process, authorization_basis, verdict,
        delivery=delivery, description=description, review=review,
        artifacts=artifacts)


# ---------------------------------------------------------------------------
# Derived facts
# ---------------------------------------------------------------------------


def events_for(ledger, assign_id):
    return [e for e in ledger["inspections"]
            if e["assignment_id"] == assign_id]


def assignment_facts(ledger, assign_id):
    """The derived dimensions. Separate booleans and timestamps, never one
    exclusive enum: inspected, contaminated and spent are different facts
    and a single state field erases whichever it does not name."""
    assignment = ledger["assignments"][assign_id]
    events = events_for(ledger, assign_id)
    unauthorized = [e for e in events
                    if e["authorization_verdict"] == VERDICT_UNAUTHORIZED]
    observed = sorted(e["observed_at"] for e in events if e["observed_at"])
    unauth_obs = sorted(e["observed_at"] for e in unauthorized
                        if e["observed_at"])
    sealed_role = assignment["role"] not in OPEN_ROLES
    facts = {
        "assignment_id": assign_id,
        "role": assignment["role"],
        "contract_track": assignment["contract_track"],
        "has_any_inspection": bool(events),
        "has_unauthorized_inspection": bool(unauthorized),
        "first_inspected_at": observed[0] if observed else None,
        "first_unauthorized_inspection_at": (unauth_obs[0] if unauth_obs
                                             else None),
        "inspection_channels": sorted({e["channel"] for e in events}),
    }
    # The scientific disposition, which is a reading of the facts above and
    # not a fourth stored state. Any unauthorized inspection of a sealed
    # assignment contaminates the month regardless of whether a file was
    # ever delivered - Amendment 2 states that consequence explicitly.
    if unauthorized and (sealed_role
                         or assignment["contract_track"] not in OPEN_TRACKS):
        facts["disposition"] = "contaminated"
    elif unauthorized:
        facts["disposition"] = "unauthorized_inspection_of_open_assignment"
    elif events:
        facts["disposition"] = "inspected"
    else:
        facts["disposition"] = "no_recorded_inspection"
    return facts


def delivery_facts(ledger, did):
    """Per delivery, first_content_read_at DERIVED from the earliest
    delivered-file event of that delivery - with exactly that narrowed
    meaning, and no other. A design-channel inspection of the same month
    does not stamp it, and must not: Amendment 2 requires that authorized
    design-channel inspection never imply a delivered file was read."""
    reads = sorted(e["observed_at"] for e in ledger["inspections"]
                   if e["delivery_id"] == did and e["channel"] == CHANNEL_FILE
                   and e["observed_at"])
    return {
        "delivery_id": did,
        "first_content_read_at": reads[0] if reads else None,
        "content_read_count": len(reads),
    }


# ---------------------------------------------------------------------------
# Verification
# ---------------------------------------------------------------------------


def verify(ledger):
    """Invariants over the whole ledger. Returns the named problem list."""
    problems = []
    for aid, assignment in sorted(ledger["assignments"].items()):
        expect = derive_role(assignment.get("instrument"),
                             assignment.get("month"))
        if assignment.get("role") != expect:
            problems.append("%s: role %r contradicts the assignment table, "
                            "which derives %r" % (aid, assignment.get("role"),
                                                  expect))
        if assignment.get("role") not in ROLES:
            problems.append("%s: role outside the closed set" % aid)
        if assignment.get("contract_track") not in TRACK_SCHEMAS:
            problems.append("%s: contract track outside the closed set" % aid)
        if assignment.get("role_assigned_at") != ROLE_ASSIGNED_AT:
            problems.append("%s: role_assigned_at %r is not the contract "
                            "assignment time; it must never be rewritten to "
                            "a migration date" % (
                                aid, assignment.get("role_assigned_at")))
        # The chain must be EXACTLY what the tables derive, order included.
        # Checking only that each name is known would pass an empty chain, a
        # chain missing Amendment 1 for a month that only exists because of
        # it, or a reordered one.
        if assignment.get("authority") != authority_chain(
                assignment.get("month")):
            problems.append("%s: authority chain %r is not the derived chain "
                            "%r" % (aid, assignment.get("authority"),
                                    authority_chain(assignment.get("month"))))
        if assignment.get("representation_authority") != AMEND2:
            problems.append("%s: representation authority %r is not %s" % (
                aid, assignment.get("representation_authority"), AMEND2))
        for authority in assignment.get("authority") or []:
            if authority not in AUTHORITIES:
                problems.append("%s: unknown authority %r" % (aid, authority))
        if aid != assignment_id(assignment.get("contract_track"),
                                assignment.get("dataset"),
                                assignment.get("instrument"),
                                assignment.get("month")):
            problems.append("%s: fields disagree with the key" % aid)
    for did, delivery in sorted(ledger["deliveries"].items()):
        if delivery.get("assignment_id") not in ledger["assignments"]:
            problems.append("%s: orphan delivery, no assignment root" % did)
        # Identity checked against the key and the parent, as assignments
        # are: a record whose fields were edited away from the key that
        # addresses it is corrupt however plausible each field looks.
        elif did != delivery_id(delivery["assignment_id"],
                                delivery.get("schema"),
                                delivery.get("filename")):
            problems.append("%s: fields disagree with the key" % did)
        parent = ledger["assignments"].get(delivery.get("assignment_id"))
        if parent is not None and delivery.get("schema") in SCHEMAS:
            governed = TRACK_SCHEMAS[parent["contract_track"]]
            if delivery["schema"] not in governed:
                problems.append("%s: schema %s is not governed by the "
                                "parent's track %s" % (
                                    did, delivery["schema"],
                                    parent["contract_track"]))
        if delivery.get("schema") not in SCHEMAS:
            problems.append("%s: schema outside the closed set" % did)
        if delivery.get("delivery_state") not in DELIVERY_STATES:
            problems.append("%s: delivery state outside the closed set" % did)
        incomplete = delivery.get("delivery_state") == DELIVERY_INCOMPLETE
        if incomplete and not delivery.get("covered_interval"):
            problems.append("%s: incomplete delivery, no covered interval"
                            % did)
        if incomplete and not delivery.get("edge_timestamps"):
            problems.append("%s: incomplete delivery, no edge timestamps"
                            % did)
        if not incomplete and (delivery.get("covered_interval")
                               or delivery.get("edge_timestamps")):
            problems.append("%s: complete delivery carrying interval or "
                            "edges" % did)
    seen_events = set()
    for event in ledger["inspections"]:
        eid = event.get("event_id")
        if eid in seen_events:
            problems.append("%s: duplicate event id" % eid)
        seen_events.add(eid)
        if event.get("assignment_id") not in ledger["assignments"]:
            problems.append("%s: orphan inspection, no assignment root" % eid)
        if event.get("channel") not in CHANNELS:
            problems.append("%s: channel outside the closed vocabulary" % eid)
        if event.get("authorization_verdict") not in VERDICTS:
            problems.append("%s: verdict outside the closed set" % eid)
        if event.get("observed_at_basis") not in OBSERVED_BASES:
            problems.append("%s: observed_at basis outside the closed set"
                            % eid)
        is_file = event.get("channel") in FILE_CHANNELS
        if is_file and not event.get("delivery_id"):
            problems.append("%s: file-channel event with no delivery" % eid)
        if not is_file and event.get("delivery_id"):
            problems.append("%s: non-file-channel event carrying a delivery "
                            "reference" % eid)
        if event.get("delivery_id"):
            referenced = ledger["deliveries"].get(event["delivery_id"])
            if referenced is None:
                problems.append("%s: delivery reference does not resolve"
                                % eid)
            # Existence is not enough. A June file event pointing at a
            # January delivery would otherwise verify, recording a read of
            # the wrong month against the wrong assignment.
            elif referenced["assignment_id"] != event.get("assignment_id"):
                problems.append("%s: references delivery %s, which belongs "
                                "to assignment %s, not %s" % (
                                    eid, event["delivery_id"],
                                    referenced["assignment_id"],
                                    event.get("assignment_id")))
        if event.get("channel") == CHANNEL_OTHER and not (
                event.get("description") and event.get("review")):
            problems.append("%s: channel other without description and "
                            "review" % eid)
    return problems


# ---------------------------------------------------------------------------
# The one-time migration
# ---------------------------------------------------------------------------


CALENDAR_SWEEP_SCOPE = ("per-date vendor record counts for every calendar "
                        "date in the month, MNQ.v.0 tbbo, via "
                        "metadata.get_record_count")


def migrate(ledger, calendar_path=CALENDAR_ARTIFACT, recorded_at=None):
    """Create assignment roots and import the already-executed calendar
    sweep as inspection events. Authorized by Amendment 2.

    THE HONEST IMPORT. The artifact records ONE artifact-level generation
    time and does not record per-request observation timestamps, an acting
    authority, or a reading process. So observed_at is the artifact's
    generated_at LABELED an upper-bound proxy - the sweep finished by then,
    it did not happen then - and authority and reading process are recorded
    as unknown with a migration reason rather than invented. No delivery
    timestamp is borrowed and the sweep is never represented as having read
    a delivered file: it imports on the vendor-record-count channel, which
    forbids a delivery reference outright."""
    made = create_all_assignments(ledger, recorded_at=recorded_at)
    if not os.path.exists(calendar_path):
        raise Refusal("no calendar artifact at %s to import" % calendar_path)
    with open(calendar_path) as fh:
        payload = json.load(fh)
    artifact_ref = {"path": os.path.relpath(calendar_path, ROOT),
                    "sha256": sha256_file(calendar_path)}
    generated_at = payload.get("generated_at")
    if not generated_at:
        raise Refusal("calendar artifact records no generated_at; there is "
                      "no defensible observation proxy to import")
    imported = 0
    for month in sorted(payload.get("months", {})):
        assignment = get_assignment(ledger, "MNQ", month, TRACK_SUCCESSOR)
        already = [e for e in events_for(ledger, assignment["assignment_id"])
                   if e["channel"] == CHANNEL_RECORD_COUNT
                   and any(a.get("path") == artifact_ref["path"]
                           for a in e["artifacts"])]
        if already:
            continue
        record_retrospective_inspection(
            ledger, "MNQ", month, "tbbo", CHANNEL_RECORD_COUNT,
            CALENDAR_SWEEP_SCOPE,
            observed_at=generated_at,
            observed_at_basis=BASIS_UPPER_BOUND,
            authority="unknown",
            reading_process="unknown",
            verdict=VERDICT_AUTHORIZED,
            authorization_basis=(
                "Amendment 1 provision 3 authorizes the design-month "
                "calendar sweep over new-design months and spent-design "
                "July only; this month held an open role on the successor "
                "track at the time of the sweep. Authority and reading "
                "process are recorded as unknown because the artifact never "
                "recorded them, and the migration must not claim to "
                "preserve what was never written."),
            artifacts=[artifact_ref])
        imported += 1
    return made, imported


# ---------------------------------------------------------------------------
# The bounded audit for the confirmation months
# ---------------------------------------------------------------------------


AUDIT_MONTHS = ("2026-05", "2026-06")

# ---------------------------------------------------------------------------
# The frozen reconstruction-closure check, Amendment 3
# ---------------------------------------------------------------------------
# The question a whole-month aggregate cannot answer on its own: can several
# recorded aggregates be COMBINED to reconstruct a count over some strict
# part of a sealed month? A month total is one scalar and exposes no
# within-month allocation, but a month total minus an overlapping window
# total can isolate a subspan, and that would be per-session-adjacent
# information obtained through arithmetic rather than through a request.
#
# The check ranges over EVERY recorded count request in the same observable
# domain, not merely those touching the sealed month, because a request
# wholly outside it can complete an equation with an overlapping one.
#
# BOUNDARY SEMANTICS, resolved rather than assumed away: vendor time bounds
# are half-open, start inclusive and end exclusive, so counts are additive
# over abutting intervals. This is recorded in the verdict. Anything the
# checker cannot resolve REFUSES; it never guesses.

# Enumerating every subspan is exponential, so the check works in the
# subspace of combinations that vanish outside the month and only enumerates
# when that subspace is small. Beyond this many atomic intervals inside a
# month with a multi-dimensional solution space, it refuses.
CLOSURE_MAX_ENUMERATED_INTERVALS = 16


def _rref(matrix):
    """Reduced row echelon form over the rationals. Exact, no floats: a
    containment decided by rounding is not a decision."""
    rows = [list(r) for r in matrix]
    if not rows:
        return [], []
    ncols = len(rows[0])
    pivots = []
    r = 0
    for c in range(ncols):
        pivot = next((i for i in range(r, len(rows)) if rows[i][c] != 0),
                     None)
        if pivot is None:
            continue
        rows[r], rows[pivot] = rows[pivot], rows[r]
        lead = rows[r][c]
        rows[r] = [x / lead for x in rows[r]]
        for i in range(len(rows)):
            if i != r and rows[i][c] != 0:
                factor = rows[i][c]
                rows[i] = [a - factor * b
                           for a, b in zip(rows[i], rows[r], strict=True)]
        pivots.append(c)
        r += 1
        if r == len(rows):
            break
    return rows[:r], pivots


def _rank(matrix):
    return len(_rref(matrix)[0])


def _solve(vectors, target):
    """Exact rational coefficients x with sum(x_j * vectors[j]) == target,
    or None when no such combination exists. This is what turns a claimed
    containment into a demonstrable one: the reader can multiply the
    recorded request counts by these coefficients and obtain the subspan
    count themselves."""
    rows = len(target)
    cols = len(vectors)
    augmented = [[vectors[j][i] for j in range(cols)] + [target[i]]
                 for i in range(rows)]
    reduced, pivots = _rref(augmented)
    if cols in pivots:
        return None
    x = [Fraction(0)] * cols
    for r, c in enumerate(pivots):
        x[c] = reduced[r][cols]
    verify_vec = [sum((x[j] * vectors[j][i] for j in range(cols)),
                      Fraction(0)) for i in range(rows)]
    return x if verify_vec == list(target) else None


def _nullspace(matrix, ncols):
    """Basis of {x : matrix @ x == 0}."""
    reduced, pivots = _rref(matrix)
    basis = []
    for free in (c for c in range(ncols) if c not in pivots):
        vec = [Fraction(0)] * ncols
        vec[free] = Fraction(1)
        for i, pivot in enumerate(pivots):
            vec[pivot] = -reduced[i][free]
        basis.append(vec)
    return basis


def _domain_of(params):
    """Every parameter affecting the counted population. Domains are never
    combined merely because their values correlate."""
    return (
        "metadata.get_record_count",
        params.get("dataset", [None])[0],
        params.get("symbols", [None])[0],
        params.get("schema", [None])[0],
        params.get("stype_in", [None])[0],
    )


def closure_check(months=AUDIT_MONTHS, cache=None):
    """Adjudicate every recorded count request against each sealed month.

    Returns a verdict dict. A REFUSAL is not a pass: the caller must treat
    it as unresolved."""
    entries = dp_cache() if cache is None else cache
    if entries is None:
        return {"verdict": "refused",
                "reason": "the response cache could not be enumerated; an "
                          "incomplete enumeration cannot support a closure "
                          "claim"}
    requests = []
    for key in sorted(entries):
        if "get_record_count" not in key:
            continue
        params = urllib.parse.parse_qs(key.split("|", 2)[-1])
        start = _parse_bound(params.get("start", [None])[0])
        end = _parse_bound(params.get("end", [None])[0])
        if start is None or end is None or end <= start:
            return {"verdict": "refused",
                    "reason": "malformed or unresolvable bounds on %s" % key}
        requests.append({"key": key, "domain": _domain_of(params),
                         "start": start, "end": end})
    findings = []
    for month in months:
        m_start, m_end = _month_session_bounds(month)
        if m_start is None or m_end is None:
            return {"verdict": "refused",
                    "reason": "could not resolve session bounds for %s"
                              % month}
        domains = sorted({r["domain"] for r in requests
                          if r["start"] < m_end and r["end"] > m_start},
                         key=str)
        for domain in domains:
            same = [r for r in requests if r["domain"] == domain]
            cuts = sorted({b for r in same for b in (r["start"], r["end"])}
                          | {m_start, m_end})
            atoms = list(itertools.pairwise(cuts))
            inside = [i for i, (a, b) in enumerate(atoms)
                      if a >= m_start and b <= m_end]
            if not inside:
                continue
            vectors = []
            for r in same:
                vectors.append([Fraction(1) if (a >= r["start"]
                                                and b <= r["end"])
                                else Fraction(0) for a, b in atoms])
            outcome = _closure_for_domain(vectors, atoms, inside, same)
            outcome.update({"month": month, "domain": list(domain),
                            "atomic_intervals": len(atoms),
                            "intervals_inside_month": len(inside),
                            "requests_in_domain": len(same)})
            findings.append(outcome)
    refused = [f for f in findings if f["result"] == "refused"]
    contained = [f for f in findings if f["result"] == "containment"]
    if refused:
        return {"verdict": "refused", "findings": findings,
                "reason": refused[0]["reason"]}
    return {"verdict": "containment" if contained else "no_containment",
            "findings": findings}


def _witness(vectors, requests, atoms, inside, indicator):
    """A containment reported with its coefficients and derived interval
    set, as the frozen check requires."""
    full = [Fraction(0)] * len(atoms)
    for i, value in enumerate(indicator):
        full[inside[i]] = value
    coefficients = _solve(vectors, full)
    witness = {
        "witness_indicator": [str(v) for v in indicator],
        "witness_intervals": [
            [atoms[inside[i]][0].isoformat(), atoms[inside[i]][1].isoformat()]
            for i, v in enumerate(indicator) if v != 0],
    }
    if coefficients is None:
        # Should be unreachable: the indicator was found IN the row span.
        witness["witness_coefficients"] = None
        witness["coefficient_note"] = ("the indicator lies in the row span "
                                       "but no coefficient vector was "
                                       "recovered; treat as unresolved")
        return witness
    witness["witness_coefficients"] = [
        {"request": requests[j]["key"], "coefficient": str(coefficients[j])}
        for j in range(len(requests)) if coefficients[j] != 0]
    return witness


def _closure_for_domain(vectors, atoms, inside, requests):
    """Does the rational row span contain the incidence vector of any
    nonempty PROPER subspan of the month - any union of atomic intervals
    wholly inside it, contiguous or not?"""
    if not vectors:
        return {"result": "no_containment", "reason": "no requests"}
    ncols = len(atoms)
    outside = [i for i in range(ncols) if i not in inside]
    basis, _ = _rref(vectors)
    if not basis:
        return {"result": "no_containment", "reason": "empty row span"}
    # Combinations of basis rows that vanish on every atomic interval
    # OUTSIDE the month. Anything not in this subspace cannot be a subspan
    # indicator, because such an indicator is zero outside by definition.
    if outside:
        constraint = [[basis[r][c] for r in range(len(basis))]
                      for c in outside]
        coeffs = _nullspace(constraint, len(basis))
    else:
        coeffs = [[Fraction(1) if i == j else Fraction(0)
                   for i in range(len(basis))] for j in range(len(basis))]
    if not coeffs:
        return {"result": "no_containment",
                "reason": "no combination of recorded requests vanishes "
                          "outside the month, so none can isolate a subspan"}
    inner = []
    for coeff in coeffs:
        vec = [sum((coeff[r] * basis[r][c] for r in range(len(basis))),
                   Fraction(0)) for c in range(ncols)]
        inner.append([vec[c] for c in inside])
    if len(inner) == 1:
        vec = inner[0]
        nonzero = {v for v in vec if v != 0}
        if len(nonzero) != 1:
            return {"result": "no_containment",
                    "reason": "the single isolating combination is not a "
                              "scalar multiple of a 0/1 indicator"}
        scale = nonzero.pop()
        indicator = [v / scale for v in vec]
        if all(v == 1 for v in indicator):
            return {"result": "no_containment",
                    "reason": "the only isolating combination is the whole "
                              "month, which is an admitted aggregate"}
        outcome = {"result": "containment",
                   "reason": "a proper subspan is reconstructible"}
        outcome.update(_witness(vectors, requests, atoms, inside, indicator))
        return outcome
    if len(inside) > CLOSURE_MAX_ENUMERATED_INTERVALS:
        return {"result": "refused",
                "reason": "%d atomic intervals inside the month with a "
                          "%d-dimensional isolating subspace exceeds the "
                          "frozen enumeration bound" % (len(inside),
                                                        len(inner))}
    full = (1 << len(inside)) - 1
    for mask in range(1, full):
        candidate = [Fraction(1) if mask & (1 << i) else Fraction(0)
                     for i in range(len(inside))]
        if _rank(inner) == _rank([*inner, candidate]):
            outcome = {"result": "containment",
                       "reason": "a proper subspan is reconstructible"}
            outcome.update(_witness(vectors, requests, atoms, inside,
                                    candidate))
            return outcome
    return {"result": "no_containment",
            "reason": "no proper subspan indicator lies in the row span"}


def adjudicate(ledger, months=AUDIT_MONTHS):
    """Run the frozen check and record its consequence. Amendment 3 leaves
    no interpretive room in either direction.

    A refusal records nothing and stays failed closed: it is not a pass."""
    verdict = closure_check(months)
    bindings = {"authority": AMEND3,
                "authority_git_blob": AUTHORITIES[AMEND3]["git_blob"],
                "cache_sha256": cache_hash(),
                "checker_sha256": checker_hash(),
                "closure_verdict": verdict["verdict"]}
    if verdict["verdict"] == "refused":
        return verdict, 0
    overlaps = _overlapping_count_requests(months)
    # Index the findings by the (month, domain) they adjudicate. A
    # containment in ONE domain of ONE month says nothing about any other:
    # the contract contaminates the affected month, and reducing the whole
    # result to a single boolean would mark unrelated requests, unrelated
    # domains and the other month unauthorized too.
    by_pair = {(f["month"], tuple(f["domain"])): f
               for f in verdict.get("findings", [])}
    recorded = 0
    for hit in overlaps:
        month = hit["month"]
        finding = by_pair.get((month, tuple(hit["domain"])))
        if finding is None:
            # Fail closed: an overlapping request the check never
            # adjudicated must not be recorded as authorized.
            return {"verdict": "refused",
                    "reason": "no closure finding for %s in domain %s; the "
                              "request was not adjudicated" % (
                                  month, hit["domain"])}, 0
        contained = finding["result"] == "containment"
        scope = ("pre-contract vendor record-count request, domain "
                 "symbols=%s schema=%s, span %s days, bounds %s to %s"
                 % (hit["symbols"], hit["schema"], hit["span_days"],
                    hit["start"], hit["end"]))
        basis = (
            "Amendment 3: a whole-month aggregate record count is not "
            "inspection, on informational-granularity grounds, and the "
            "frozen reconstruction-closure check found no proper subspan "
            "of this month reconstructible from any combination of "
            "recorded requests in THIS domain. Pre-signature timing is NOT "
            "the ground; the check is."
            if not contained else
            "Amendment 3: the frozen reconstruction-closure check found a "
            "proper subspan of this month reconstructible from recorded "
            "aggregates in THIS domain. Contaminating, without a further "
            "interpretive round. Witness: %s" % finding.get(
                "witness_coefficients"))
        # Dedup on scope AND binding, not scope alone. If the cache or the
        # checker has changed, the earlier verdict was reached against
        # different bytes or different code, and a fresh adjudication is
        # APPENDED rather than the stale one refreshed - events are never
        # rewritten in place.
        already = [e for e in events_for(
            ledger, assignment_id(TRACK_SUCCESSOR, DATASET, "MNQ", month))
            if e["scope"] == scope and bindings in (e.get("artifacts") or [])]
        if already:
            continue
        record_retrospective_inspection(
            ledger, "MNQ", month, "tbbo", CHANNEL_RECORD_COUNT, scope,
            observed_at=hit["fetched"],
            observed_at_basis=BASIS_RECORDED,
            authority="pre-contract pricing sweep, "
                      "analysis/databento_price.py",
            reading_process="databento_price.measure via phase_price or "
                            "phase_plan",
            verdict=(VERDICT_UNAUTHORIZED if contained
                     else VERDICT_AUTHORIZED),
            authorization_basis=basis,
            artifacts=[bindings],
            track=TRACK_SUCCESSOR)
        recorded += 1
    return verdict, recorded


def _overlapping_count_requests(months):
    """The event finder: recorded count requests overlapping a sealed month,
    with their genuine per-request fetch times."""
    import databento_price as dp
    entries = dp.read_cache_file() or {}
    out = []
    for key, value in sorted(entries.items()):
        if "get_record_count" not in key:
            continue
        params = urllib.parse.parse_qs(key.split("|", 2)[-1])
        start = _parse_bound(params.get("start", [None])[0])
        end = _parse_bound(params.get("end", [None])[0])
        if start is None or end is None:
            continue
        for month in months:
            m_start, m_end = _month_session_bounds(month)
            if start < m_end and end > m_start:
                out.append({
                    "month": month,
                    "domain": list(_domain_of(params)),
                    "symbols": params.get("symbols", [None])[0],
                    "schema": params.get("schema", [None])[0],
                    "start": start.isoformat(),
                    "end": end.isoformat(),
                    "span_days": round((end - start).total_seconds() / 86400,
                                       2),
                    # A genuine per-request timestamp, so this is recorded
                    # rather than an upper-bound proxy.
                    "fetched": value.get("fetched"),
                })
    return out


def dp_cache():
    import databento_price as dp
    return dp.read_cache_file()


def checker_hash():
    """The checker implementation hash, bound into the verdict so a verdict
    cannot be quietly re-used after the criterion's code changes."""
    return sha256_file(os.path.abspath(__file__))


def cache_hash():
    import databento_price as dp
    return sha256_file(dp.CACHE_FILE)


def _parse_bound(value):
    """A cached request bound as an aware datetime, or None."""
    if not value:
        return None
    try:
        stamp = dt.datetime.fromisoformat(value)
    except ValueError:
        return None
    return stamp if stamp.tzinfo else stamp.replace(tzinfo=dt.UTC)


def _month_session_bounds(month):
    """A month's UTC session interval, on the same convention every request
    in the cache was built with: date D covers D-1 17:00 to D 16:00
    Central."""
    import databento_price as dp
    year, mon = (int(x) for x in month.split("-"))
    nxt = ("%04d-01-01" % (year + 1) if mon == 12
           else "%04d-%02d-01" % (year, mon + 1))
    start, end = dp.session_bounds_utc("%04d-%02d-01" % (year, mon), nxt)
    return _parse_bound(start), _parse_bound(end)


def audit_inventory():
    """The frozen scope: every identified tool, cache, artifact and
    available request history. Returns the inventory with what each was
    examined for."""
    return [
        {"kind": "cache",
         "path": "analysis/databento_cache.json",
         "examined_for": "any metadata.get_record_count entry whose "
                         "parameters cover an audited month. This is the "
                         "closest thing to a request log that exists: every "
                         "successful vendor response this project fetched is "
                         "written here, keyed by endpoint and parameters."},
        {"kind": "artifact",
         "path": "analysis/databento-calendar.json",
         "examined_for": "whether an audited month appears among its swept "
                         "months"},
        {"kind": "artifact",
         "path": "analysis/databento-jobs.json",
         "examined_for": "delivered purchases covering an audited month, "
                         "recorded as observations only: opaque delivery "
                         "and byte hashing neither spend nor contaminate a "
                         "month, so sealed bytes are not evidence of "
                         "inspection"},
        {"kind": "ledger",
         "path": "analysis/databento-seal-ledger.json",
         "examined_for": "any recorded inspection event against an audited "
                         "assignment, on any channel. This is where a "
                         "content read would be recorded, so it is the "
                         "authoritative source rather than the presence of "
                         "bytes on disk"},
        {"kind": "tool",
         "path": "analysis/databento_calendar.py",
         "examined_for": "whether its sweepable set can include an audited "
                         "month"},
        {"kind": "tool",
         "path": "analysis/databento_download.py",
         "examined_for": "whether its buy whitelist can reach an audited "
                         "month"},
        {"kind": "landing",
         "path": "research/market-data/databento",
         "examined_for": "landed bytes for an audited month, recorded as "
                         "observations only, for the same reason as the job "
                         "ledger"},
    ]


def audit_confirmation_months(months=AUDIT_MONTHS):
    """A BOUNDED AUDIT CONCLUSION, never a proof.

    Enumeration of recorded channels can establish that no evidence of
    inspection was found. It cannot establish that no unrecorded process
    ever occurred - which is precisely the threat model behind retrospective
    contamination recording. The result says exactly that and no more."""
    import databento_calendar as dc
    import databento_price as dp

    findings = []
    # The pricing cache is the effective request history. Matching is by
    # INTERVAL OVERLAP against the month's session bounds, never by
    # substring: a July window starts 2026-06-30T22:00 UTC, because July's
    # sessions begin at the 30 June 17:00 Central boundary, so a substring
    # test reports July requests as June hits. Each overlap also records its
    # SPAN, because a month-aggregate count and a per-date count are
    # different observables and the contract's test turns on that.
    cache_hits = []
    for key in dp.read_cache_file() or {}:
        if "get_record_count" not in key:
            continue
        params = urllib.parse.parse_qs(key.split("|", 2)[-1])
        start = _parse_bound(params.get("start", [None])[0])
        end = _parse_bound(params.get("end", [None])[0])
        if start is None or end is None:
            cache_hits.append({"key": key, "month": "unparseable bounds",
                               "span_days": None})
            continue
        for month in months:
            m_start, m_end = _month_session_bounds(month)
            if start < m_end and end > m_start:
                cache_hits.append({
                    "key": key,
                    "month": month,
                    "span_days": round((end - start).total_seconds() / 86400,
                                       2),
                    "symbols": params.get("symbols", [None])[0],
                    "schema": params.get("schema", [None])[0],
                })
    # The overlap matcher is the EVENT FINDER. It says which requests touch
    # a sealed month; it does not adjudicate them. Amendment 3 gives that
    # job to the reconstruction-closure check alone, so overlaps are
    # observations here and the closure verdict below decides.
    closure = closure_check(months)
    findings.append({
        "source": "analysis/databento_cache.json",
        "result": "%d record-count request(s) overlap an audited month; "
                  "adjudicated by the closure check" % len(cache_hits),
        "hits": [],
        "observations": cache_hits,
    })
    # A PASSING CHECK IS NOT ELIGIBILITY. Amendment 3 requires the ledger to
    # stay failed closed until the check runs AND the requests to be
    # retained as recorded events on a pass. Running closure_check here
    # proves nothing was recorded, so eligibility additionally demands an
    # adjudication event per overlapping request, bound to the CURRENT
    # cache and checker hashes - a stale binding means the recorded verdict
    # was reached against different bytes or different code.
    bound_now = {"authority": AMEND3,
                 "authority_git_blob": AUTHORITIES[AMEND3]["git_blob"],
                 "cache_sha256": cache_hash(),
                 "checker_sha256": checker_hash(),
                 "closure_verdict": closure["verdict"]}
    adjudicated = load_ledger()
    unbound = []
    for hit in _overlapping_count_requests(months):
        aid = assignment_id(TRACK_SUCCESSOR, DATASET, "MNQ", hit["month"])
        matched = [e for e in events_for(adjudicated, aid)
                   if bound_now in (e.get("artifacts") or [])
                   and hit["start"] in (e.get("scope") or "")
                   and (hit["symbols"] or "") in (e.get("scope") or "")
                   and (hit["schema"] or "") in (e.get("scope") or "")]
        if not matched:
            unbound.append({"month": hit["month"], "symbols": hit["symbols"],
                            "schema": hit["schema"], "start": hit["start"]})
    findings.append({
        "source": "Amendment 3 adjudication record",
        "result": "every overlapping request carries an adjudication event "
                  "bound to the current cache and checker"
                  if not unbound else
                  "%d overlapping request(s) have no adjudication event "
                  "bound to the current cache and checker; run adjudicate"
                  % len(unbound),
        "hits": unbound,
        "observations": [],
        "binding": bound_now,
    })
    findings.append({
        "source": "reconstruction-closure check, Amendment 3",
        "result": {
            "no_containment": "no proper subspan of a sealed month is "
                              "reconstructible from recorded aggregates",
            "containment": "A PROPER SUBSPAN IS RECONSTRUCTIBLE",
            "refused": "the check REFUSED and the question is unresolved",
        }[closure["verdict"]],
        # A refusal is not a pass. Both a containment and a refusal must
        # keep the months ineligible.
        "hits": ([] if closure["verdict"] == "no_containment"
                 else [closure]),
        "observations": closure.get("findings", []),
        "boundary_semantics": "half-open, start inclusive and end "
                              "exclusive, so counts are additive over "
                              "abutting intervals",
        "cache_sha256": cache_hash(),
        "checker_sha256": checker_hash(),
    })
    # The calendar artifact.
    swept = []
    if os.path.exists(CALENDAR_ARTIFACT):
        with open(CALENDAR_ARTIFACT) as fh:
            swept = sorted(set(json.load(fh).get("months", {})) & set(months))
    findings.append({
        "source": "analysis/databento-calendar.json",
        "result": "no audited month among the swept months" if not swept
                  else "FOUND audited months in the sweep",
        "hits": swept,
    })
    # The tool's own reachable set.
    reachable = sorted(set(dc.sweepable_months()) & set(months))
    findings.append({
        "source": "analysis/databento_calendar.py",
        "result": "audited months are not reachable by the sweep"
                  if not reachable else "FOUND audited months reachable",
        "hits": reachable,
    })
    # DELIVERY IS NOT INSPECTION. The contract is explicit that pulling and
    # byte-hashing opaque bytes neither spends nor contaminates a month -
    # that is the entire premise of sealing them rather than declining to
    # buy them. Counting a delivered June archive as inspection evidence
    # would disqualify both confirmation populations the moment the manifest
    # lands, which is the opposite of what the seal is for. So deliveries
    # and landed bytes are recorded as OBSERVATIONS, never as hits, and what
    # is searched for is evidence that content was READ.
    jobs_path = os.path.join(ROOT, "analysis", "databento-jobs.json")
    delivered = []
    if os.path.exists(jobs_path):
        with open(jobs_path) as fh:
            for key in json.load(fh).get("jobs", {}):
                for month in months:
                    if month in key:
                        delivered.append(key)
    findings.append({
        "source": "analysis/databento-jobs.json",
        "result": "%d sealed delivery record(s); delivery is not inspection"
                  % len(delivered),
        "hits": [],
        "observations": delivered,
    })
    landing = os.path.join(ROOT, "research", "market-data", "databento")
    landed = []
    if os.path.isdir(landing):
        for scope in sorted(os.listdir(landing)):
            scope_dir = os.path.join(landing, scope)
            if not os.path.isdir(scope_dir):
                continue
            for name in sorted(os.listdir(scope_dir)):
                for month in months:
                    if month in name:
                        landed.append("%s/%s" % (scope, name))
    findings.append({
        "source": "research/market-data/databento",
        "result": "%d landed sealed director(ies); bytes on disk are not "
                  "inspection" % len(landed),
        "hits": [],
        "observations": landed,
    })
    # The seal ledger itself is where a content read would be recorded, so
    # it is the authoritative source for this question rather than the
    # presence of bytes.
    ledger = load_ledger()
    recorded = []
    for aid, assignment in ledger["assignments"].items():
        if assignment.get("month") not in months:
            continue
        facts = assignment_facts(ledger, aid)
        # Only UNAUTHORIZED events disqualify. Amendment 3 admits the
        # sixteen aggregate requests as authorized pre-contract pricing
        # events and requires them retained, so their presence must not
        # itself make the month ineligible.
        if facts["has_unauthorized_inspection"]:
            recorded.append({"assignment_id": aid,
                             "channels": facts["inspection_channels"],
                             "first_unauthorized_inspection_at": facts[
                                 "first_unauthorized_inspection_at"]})
    findings.append({
        "source": "analysis/databento-seal-ledger.json",
        "result": "no UNAUTHORIZED inspection event against an audited month"
                  if not recorded else "FOUND unauthorized inspection events",
        "hits": recorded,
        "observations": [],
    })
    any_hits = any(f["hits"] for f in findings)
    return {
        "_version": 1,
        "generated_at": now_iso(),
        # Both authorities, with their content-addressed identities:
        # Amendment 2 authorizes the audit's existence and shape, Amendment
        # 3 authorizes the criterion that decides its central question.
        "authority": [AMEND2, AMEND3],
        "authority_snapshots": {name: AUTHORITIES[name]["git_blob"]
                                for name in (AMEND2, AMEND3)},
        "months": list(months),
        "claim_type": "bounded_audit_conclusion",
        "claim": "No evidence of inspection was found in any enumerated "
                 "source. This is NOT a proof that no unrecorded process "
                 "ever inspected these months; enumeration of recorded "
                 "channels cannot establish that, which is why "
                 "retrospective contamination recording exists.",
        "inventory": audit_inventory(),
        "findings": findings,
        "eligible": not any_hits,
    }


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------


def mode_migrate():
    ledger = load_ledger()
    made, imported = migrate(ledger)
    problems = verify(ledger)
    if problems:
        print("MIGRATION PRODUCED AN INVALID LEDGER; nothing written:")
        for line in problems:
            print("   ", line)
        raise SystemExit(1)
    save_ledger(ledger)
    print("%d assignment(s) created, %d sweep event(s) imported -> %s" % (
        made, imported, LEDGER_FILE))


def mode_adjudicate():
    ledger = load_ledger()
    verdict, recorded = adjudicate(ledger)
    print("closure verdict: %s" % verdict["verdict"])
    if verdict["verdict"] == "refused":
        print("REFUSED: %s" % verdict.get("reason"))
        print("nothing recorded; the ledger stays failed closed")
        raise SystemExit(1)
    problems = verify(ledger)
    if problems:
        print("ADJUDICATION PRODUCED AN INVALID LEDGER; nothing written:")
        for line in problems:
            print("   ", line)
        raise SystemExit(1)
    save_ledger(ledger)
    print("%d event(s) recorded as %s" % (
        recorded, "CONTAMINATION" if verdict["verdict"] == "containment"
        else "authorized pre-contract pricing events"))
    if verdict["verdict"] == "containment":
        raise SystemExit(1)


def mode_audit():
    result = audit_confirmation_months()
    tmp = AUDIT_ARTIFACT + ".tmp"
    with open(tmp, "w") as fh:
        json.dump(result, fh, indent=1, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, AUDIT_ARTIFACT)
    for finding in result["findings"]:
        print("%-40s %s" % (finding["source"], finding["result"]))
    print("\nbounded audit conclusion: %s" % (
        "no inspection evidence found" if result["eligible"]
        else "INSPECTION EVIDENCE FOUND"))
    print("months remain eligible: %s -> %s" % (result["eligible"],
                                                AUDIT_ARTIFACT))
    if not result["eligible"]:
        raise SystemExit(1)


def mode_status():
    ledger = load_ledger()
    if not ledger["assignments"]:
        print("seal ledger empty (%s)" % LEDGER_FILE)
        return
    print("%s: %d assignment(s), %d delivery(ies), %d inspection(s)" % (
        LEDGER_FILE, len(ledger["assignments"]), len(ledger["deliveries"]),
        len(ledger["inspections"])))
    print("%-46s %-22s %-8s %s" % ("assignment", "role", "deliv",
                                   "disposition"))
    for aid in sorted(ledger["assignments"]):
        facts = assignment_facts(ledger, aid)
        count = sum(1 for d in ledger["deliveries"].values()
                    if d["assignment_id"] == aid)
        print("%-46s %-22s %-8d %s" % (aid, facts["role"], count,
                                       facts["disposition"]))


def mode_verify():
    problems = verify(load_ledger())
    if problems:
        print("SEAL LEDGER PROBLEMS: %d" % len(problems))
        for line in problems:
            print("   ", line)
        raise SystemExit(1)
    print("seal ledger verifies")


def selftest():
    checks = []

    def check(name, condition):
        checks.append((name, bool(condition)))
        print("  %s %s" % ("ok  " if condition else "FAIL", name))

    def refuses(fn, *a, **k):
        try:
            fn(*a, **k)
            return False
        except Refusal:
            return True

    sha = "a" * 64

    print("authorities are immutable snapshots, not the file on disk")
    check("four signed authorities are pinned", len(AUTHORITIES) == 4)
    check("every signed amendment has a snapshot",
          all(name in AUTHORITIES for name in (BASE, AMEND1, AMEND2,
                                               AMEND3)))
    check("the snapshots are distinct documents",
          len({a["git_blob"] for a in AUTHORITIES.values()}) == 4)
    check("each names a commit and a content-addressed blob",
          all(a.get("commit") and a.get("git_blob")
              for a in AUTHORITIES.values()))
    check("no authority is derived by hashing the working file",
          "contract_hash" not in globals())

    print("roles and tracks derive from the tables")
    check("June is primary-confirmation",
          derive_role("MNQ", "2026-06") == "primary-confirmation")
    check("2026-08 came from Amendment 1 and says so",
          authority_chain("2026-08") == [BASE, AMEND1])
    check("a signature-table month names only the base contract",
          authority_chain("2026-01") == [BASE])
    check("MNQ tbbo is the successor track",
          derive_track("MNQ", "tbbo") == TRACK_SUCCESSOR)
    check("MNQ mbp-1 is the preservation track",
          derive_track("MNQ", "mbp-1") == TRACK_MBP1)
    check("ES is the borrow track",
          derive_track("ES", "tbbo") == TRACK_BORROW)
    check("an unknown schema refuses", refuses(derive_track, "MNQ", "trades"))
    check("a month outside the range refuses",
          refuses(derive_role, "MNQ", "2024-05"))

    print("assignments are immutable roots")
    led = empty_ledger()
    made = create_all_assignments(led, recorded_at="2026-08-12T10:00:00+00:00")
    check("every month gets a root on every governing track",
          made == len(led["assignments"]) and made == 13 * 4)
    jan_tbbo = get_assignment(led, "MNQ", "2026-01", TRACK_SUCCESSOR)
    jan_mbp = get_assignment(led, "MNQ", "2026-01", TRACK_MBP1)
    check("one month carries separate roots per track",
          jan_tbbo["assignment_id"] != jan_mbp["assignment_id"]
          and jan_tbbo["role"] == jan_mbp["role"] == "new-design")
    check("role_assigned_at is the contract time, not the migration time",
          jan_tbbo["role_assigned_at"] == ROLE_ASSIGNED_AT
          and jan_tbbo["assignment_recorded_at"] != ROLE_ASSIGNED_AT)
    check("creating a root again is idempotent",
          ensure_assignment(led, "MNQ", "2026-01",
                            TRACK_SUCCESSOR)["assignment_id"]
          == jan_tbbo["assignment_id"])
    tampered = json.loads(json.dumps(led))
    tampered["assignments"][jan_tbbo["assignment_id"]]["role"] = "spent-design"
    check("a root whose role was edited refuses re-creation",
          refuses(ensure_assignment, tampered, "MNQ", "2026-01",
                  TRACK_SUCCESSOR))

    print("permission derives from assignment plus schema")
    check("an open month's TBBO is open",
          derive_permission(jan_tbbo, "tbbo") == PERM_OPEN)
    check("the SAME month's MBP-1 is sealed",
          derive_permission(jan_mbp, "mbp-1") == PERM_SEALED)
    jun = get_assignment(led, "MNQ", "2026-06", TRACK_SUCCESSOR)
    check("June's TBBO is sealed", derive_permission(jun, "tbbo")
          == PERM_SEALED)
    check("an incomplete delivery is unread whatever the role",
          derive_permission(jan_tbbo, "tbbo", DELIVERY_INCOMPLETE)
          == PERM_UNREAD_INCOMPLETE)
    check("an unknown delivery state refuses rather than deriving complete",
          refuses(derive_permission, jan_tbbo, "tbbo", "partial"))
    check("a schema the track does not govern refuses",
          refuses(derive_permission, jan_tbbo, "mbp-1"))

    print("deliveries are children, never the root")
    d = record_delivery(led, "MNQ", "2026-01", "tbbo", "jan.zst", sha, "JOB-A",
                        "2026-08-12T00:00:00+00:00")
    check("a delivery attaches to its assignment",
          d["assignment_id"] == jan_tbbo["assignment_id"])
    d2 = record_delivery(led, "MNQ", "2026-01", "tbbo", "jan2.zst", "b" * 64,
                         "JOB-A", "2026-08-12T00:00:00+00:00")
    check("a second file of the same month is its own delivery",
          d2["delivery_id"] != d["delivery_id"]
          and len(led["deliveries"]) == 2)
    check("an identical redelivery is idempotent",
          record_delivery(led, "MNQ", "2026-01", "tbbo", "jan.zst", sha,
                          "JOB-A", "2026-08-13T00:00:00+00:00") is d)
    check("a redelivery with different bytes refuses",
          refuses(record_delivery, led, "MNQ", "2026-01", "tbbo", "jan.zst",
                  "c" * 64, "JOB-A", "2026-08-12T00:00:00+00:00"))
    check("a complete delivery carrying an interval refuses",
          refuses(record_delivery, led, "MNQ", "2026-02", "tbbo", "f.zst",
                  sha, "J", "t", covered_interval=["a", "b"]))
    check("an incomplete delivery without edges refuses",
          refuses(record_delivery, led, "MNQ", "2025-08", "tbbo", "aug.zst",
                  sha, "J", "t", delivery_state=DELIVERY_INCOMPLETE,
                  covered_interval=["a", "b"]))

    print("inspection events, prospective")
    ev = record_inspection(led, "MNQ", "2026-01", "tbbo",
                           CHANNEL_RECORD_COUNT, "per-date counts",
                           "owner", "calendar sweep")
    check("an open month admits a vendor-metadata inspection",
          ev["authorization_verdict"] == VERDICT_AUTHORIZED
          and ev["delivery_id"] is None)
    check("a vendor-metadata event may not carry a delivery reference",
          refuses(_append_event, led, jan_tbbo, CHANNEL_RECORD_COUNT, "s",
                  "t", BASIS_RECORDED, "a", "p", "b", VERDICT_AUTHORIZED,
                  delivery=d))
    check("a file-channel event REQUIRES a delivery reference",
          refuses(_append_event, led, jan_tbbo, CHANNEL_FILE, "s", "t",
                  BASIS_RECORDED, "a", "p", "b", VERDICT_AUTHORIZED))
    fev = record_inspection(led, "MNQ", "2026-01", "tbbo", CHANNEL_FILE,
                            "stage m measure", "owner", "measure12a",
                            filename="jan.zst")
    check("a delivered file read attaches to its delivery",
          fev["delivery_id"] == d["delivery_id"])
    check("channel other needs a description and a review",
          refuses(record_inspection, led, "MNQ", "2026-01", "tbbo",
                  CHANNEL_OTHER, "s", "owner", "p"))
    check("a sealed month refuses a prospective inspection",
          refuses(record_inspection, led, "MNQ", "2026-06", "tbbo",
                  CHANNEL_RECORD_COUNT, "per-date counts", "owner", "sweep"))
    check("a sealed MBP-1 refuses even in an open month",
          refuses(record_inspection, led, "MNQ", "2026-01", "mbp-1",
                  CHANNEL_RECORD_COUNT, "per-date counts", "owner", "sweep"))
    check("an unnamed authority refuses",
          refuses(record_inspection, led, "MNQ", "2026-01", "tbbo",
                  CHANNEL_RECORD_COUNT, "s", "", "p"))

    print("contamination is recordable, which is the whole point")
    before = len(led["inspections"])
    contam = record_retrospective_inspection(
        led, "MNQ", "2026-06", "tbbo", CHANNEL_RECORD_COUNT,
        "per-date counts of a sealed month",
        observed_at="2026-08-01T00:00:00+00:00",
        observed_at_basis=BASIS_UPPER_BOUND,
        authority="unknown", reading_process="unknown",
        verdict=VERDICT_UNAUTHORIZED,
        authorization_basis="none; sealed under Amendment 1 provision 3")
    check("a sealed month's contamination records despite the guard",
          len(led["inspections"]) == before + 1
          and contam["authorization_verdict"] == VERDICT_UNAUTHORIZED)
    check("occurrence and recording times are separate",
          contam["observed_at"] != contam["recorded_at"])
    facts = assignment_facts(led, jun["assignment_id"])
    check("June now derives as contaminated",
          facts["disposition"] == "contaminated"
          and facts["has_unauthorized_inspection"]
          and facts["first_unauthorized_inspection_at"]
          == "2026-08-01T00:00:00+00:00")
    check("the prospective guard STILL refuses June after the record",
          refuses(record_inspection, led, "MNQ", "2026-06", "tbbo",
                  CHANNEL_RECORD_COUNT, "s", "owner", "p"))

    print("derived facts, not one enum")
    facts = assignment_facts(led, jan_tbbo["assignment_id"])
    check("an inspected open month is inspected, not contaminated",
          facts["disposition"] == "inspected"
          and facts["has_any_inspection"]
          and not facts["has_unauthorized_inspection"])
    check("channels are listed rather than collapsed",
          facts["inspection_channels"] == sorted(
              [CHANNEL_FILE, CHANNEL_RECORD_COUNT]))
    check("an untouched month has no recorded inspection",
          assignment_facts(led, get_assignment(
              led, "MNQ", "2026-03",
              TRACK_SUCCESSOR)["assignment_id"])["disposition"]
          == "no_recorded_inspection")
    dfacts = delivery_facts(led, d["delivery_id"])
    check("first_content_read_at derives from the FILE event only",
          dfacts["first_content_read_at"] == fev["observed_at"])
    check("a delivery never read has no content read time",
          delivery_facts(led, d2["delivery_id"])["first_content_read_at"]
          is None)
    # The precise misreport Amendment 2 was written to prevent.
    d3 = record_delivery(led, "MNQ", "2026-02", "tbbo", "feb.zst", sha,
                         "JOB-B", "2026-08-12T00:00:00+00:00")
    record_inspection(led, "MNQ", "2026-02", "tbbo", CHANNEL_RECORD_COUNT,
                      "per-date counts", "owner", "calendar sweep")
    check("a design-channel inspection does NOT stamp a file read",
          delivery_facts(led, d3["delivery_id"])["first_content_read_at"]
          is None
          and assignment_facts(
              led, d3["assignment_id"])["has_any_inspection"])

    print("verification and round trip")
    check("a consistent ledger verifies", verify(led) == [])
    bad = json.loads(json.dumps(led))
    bad["inspections"][0]["channel"] = "sneaky"
    check("an out-of-vocabulary channel is caught",
          any("closed vocabulary" in p for p in verify(bad)))
    bad = json.loads(json.dumps(led))
    bad["assignments"][jan_tbbo["assignment_id"]]["role_assigned_at"] = (
        "2026-09-01T00:00:00+00:00")
    check("a role_assigned_at rewritten to a later date is caught",
          any("never be rewritten" in p for p in verify(bad)))
    bad = json.loads(json.dumps(led))
    bad["deliveries"].pop(d["delivery_id"])
    check("an event whose delivery vanished is caught",
          any("does not resolve" in p for p in verify(bad)))
    for label, mutation in (
            ("emptied", []),
            ("reordered", [AMEND1, BASE]),
            ("wrong but known", [AMEND2]),
            ("missing amendment 1", [BASE])):
        bad = json.loads(json.dumps(led))
        target = assignment_id(TRACK_SUCCESSOR, DATASET, "MNQ", "2026-08")
        bad["assignments"][target]["authority"] = mutation
        check("an %s authority chain is caught" % label,
              any("is not the derived chain" in p for p in verify(bad)))
    bad = json.loads(json.dumps(led))
    bad["assignments"][jan_tbbo["assignment_id"]].pop(
        "representation_authority")
    check("a stripped representation authority is caught",
          any("representation authority" in p for p in verify(bad)))
    check("re-creating a root with a stripped authority refuses",
          refuses(ensure_assignment, bad, "MNQ", "2026-01", TRACK_SUCCESSOR))
    # A file event pointed at another month's delivery: existence alone
    # would pass, recording a read of the wrong month.
    bad = json.loads(json.dumps(led))
    for event in bad["inspections"]:
        if event["channel"] == CHANNEL_FILE:
            event["assignment_id"] = jun["assignment_id"]
            break
    check("a file event referencing another assignment's delivery is caught",
          any("not %s" % jun["assignment_id"] in p or "belongs" in p
              for p in verify(bad)))
    bad = json.loads(json.dumps(led))
    victim = bad["deliveries"][d["delivery_id"]]
    victim["filename"] = "renamed.zst"
    check("a delivery whose fields drift from its key is caught",
          any("disagree with the key" in p for p in verify(bad)))

    if os.path.exists(SELFTEST_DIR):
        import shutil
        shutil.rmtree(SELFTEST_DIR)
    os.makedirs(SELFTEST_DIR)
    path = os.path.join(SELFTEST_DIR, "seal.json")
    save_ledger(led, path)
    check("round trip preserves every section", load_ledger(path) == led)
    with open(path, "w") as fh:
        fh.write("{ not json")
    check("an unreadable ledger is fatal, not discarded",
          refuses(load_ledger, path))
    check("a missing ledger is empty, not fatal",
          load_ledger(os.path.join(SELFTEST_DIR, "absent.json"))
          == empty_ledger())
    import shutil
    shutil.rmtree(SELFTEST_DIR)

    failed = [name for name, ok in checks if not ok]
    print("\n%d check(s), %d failed" % (len(checks), len(failed)))
    if failed:
        for name in failed:
            print("  FAIL", name)
        raise SystemExit(1)
    print("selftest PASS")


def main():
    sys.stdout.reconfigure(line_buffering=True)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["selftest", "migrate", "adjudicate",
                                         "audit", "status", "verify"])
    args = parser.parse_args()
    if args.mode == "selftest":
        selftest()
    elif args.mode == "migrate":
        mode_migrate()
    elif args.mode == "adjudicate":
        mode_adjudicate()
    elif args.mode == "audit":
        mode_audit()
    elif args.mode == "status":
        mode_status()
    else:
        mode_verify()


if __name__ == "__main__":
    main()
