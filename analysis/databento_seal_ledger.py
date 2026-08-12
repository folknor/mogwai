#!/usr/bin/env python3
"""The seal ledger: per-file role records, bound at delivery.

notes/successor-contract.md requires, per delivered file: dataset, schema,
instrument, month, content hash, delivery job, delivery timestamp, role,
role-assignment timestamp, the hash of the contract authorizing the role,
first-content-read timestamp, unseal authority, the reading process, artifact
outputs, and final state. This module is that record, and the guards that
keep it honest.

WHY IT IS WRITTEN BY THE DOWNLOAD FLOW. "ROLES ARE ASSIGNED NOW, IMMUTABLY,
BEFORE ANY CONTENT READ - not at Stage F, because selecting a confirmation
month after seeing design results would permit calendar or regime matching."
A sealing step a human performs after delivery is a step a human can forget,
and a role assigned after a file has been looked at is not an assignment, it
is a choice. So the role is DERIVED mechanically from the immutable
assignment table below, at the moment of delivery, and refused if it would
contradict a record already written.

ROLE IS DERIVED, NEVER PASSED IN. There is deliberately no caller-supplied
role argument anywhere in this module: a function that accepts one is a
function that can be handed the convenient answer. Month and schema
determine the role; the tables are the contract's, transcribed.

SCHEMA SEALS INDEPENDENTLY OF MONTH. The contract seals every MBP-1 file
outright - "separate manifest and ledger, all sealed; no content read under
this contract" - so MBP-1 for a new-design month is sealed even though that
month's TBBO is open. Sealing is a property of the pair, not of the month.

WHAT THIS MODULE WILL NOT DO. It cannot unseal a month on its own authority:
record_unseal refuses unless the caller names an authority and a reading
process, and it refuses outright for roles whose content read no contract
authorizes. It never deletes or rewrites a role. It never infers that a file
is readable from the fact that it was delivered.

Usage:
    python3 -u analysis/databento_seal_ledger.py selftest
    python3 -u analysis/databento_seal_ledger.py status
    python3 -u analysis/databento_seal_ledger.py verify
"""

import argparse
import datetime as dt
import hashlib
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER_FILE = os.path.join(ROOT, "analysis", "databento-seal-ledger.json")
CONTRACT_FILE = os.path.join(ROOT, "notes", "successor-contract.md")
SELFTEST_DIR = os.path.join(ROOT, "analysis", "out", "seal-ledger-selftest")

LEDGER_VERSION = 1
TOOL_VERSION = 1

# The contract's closed role set, transcribed verbatim.
ROLES = {
    "spent-design": "design evidence already consumed pre-contract",
    "new-design": "open for Stage M and Stage I",
    "primary-confirmation": "sealed; the Stage C month",
    "reserve-confirmation": "sealed; replaces primary ONLY on an INVALID "
                            "outcome that contaminated the primary",
    "unused-sealed": "sealed; may become design evidence only under a FUTURE "
                     "contract, never confirmation for this one",
}

# THE MNQ ASSIGNMENT, immutable from signature, plus Amendment 1 provision 2
# which adds 2026-08. Months absent from this table have no assigned role and
# cannot be recorded: a month the contract never assigned is not deliverable
# under it.
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

# ES and MES "are sealed under the master acquisition ledger and belong to
# the MES-borrow track, which writes its own fully separate contract - shared
# bytes, separate science. Nothing signed here authorizes any ES/MES content
# read." They are recorded so the bytes are accounted for, always sealed.
BORROW_TRACK_INSTRUMENTS = ("ES", "MES")

# The acquisition range. The manifest gives ES and MES TBBO and MBP-1 "same
# range" as MNQ, so the borrow track shares these months and nothing wider:
# a month outside the range refuses for every instrument alike, rather than
# ES silently accepting any month string as unused-sealed.
ACQUISITION_MONTHS = frozenset(MNQ_ASSIGNMENT)

# The closed schema set. Sealing is decided by membership, so an unrecognized
# schema must never reach that decision: "MBP-1", "mbp1" or "trades" would
# each miss SEALED_SCHEMAS and fall through to open for a design-role month.
# Anything not named here refuses at delivery.
SCHEMAS = ("tbbo", "mbp-1")
SEALED_SCHEMAS = ("mbp-1",)
OPEN_ROLES = ("new-design", "spent-design")

# Delivery states. Amendment 1 provision 1: a month truncated by a vendor
# entitlement or availability boundary is incomplete_month_delivery, treated
# as an UNDELIVERED FULL MONTH for every Stage M population purpose, and its
# covered bytes remain UNREAD regardless of role.
DELIVERY_COMPLETE = "complete"
DELIVERY_INCOMPLETE = "incomplete_month_delivery"
DELIVERY_STATES = (DELIVERY_COMPLETE, DELIVERY_INCOMPLETE)

# Final states, closed set.
STATE_OPEN = "open"                    # content read permitted
STATE_SEALED = "sealed"                # content read forbidden
STATE_UNREAD_INCOMPLETE = "unread_incomplete"   # Amendment 1 provision 1
STATE_READ = "read"                    # a first content read is recorded
STATE_SPENT = "spent"                  # a confirmation verdict was produced
STATE_CONTAMINATED = "contaminated"    # unauthorized read, recorded as such
STATES = (STATE_OPEN, STATE_SEALED, STATE_UNREAD_INCOMPLETE, STATE_READ,
          STATE_SPENT, STATE_CONTAMINATED)


class Refusal(SystemExit):
    """A named, fail-closed refusal. Exits nonzero with the reason."""

    def __init__(self, reason):
        super().__init__("REFUSED: %s" % reason)


def contract_hash(path=CONTRACT_FILE):
    """SHA-256 of the contract authorizing every role in this ledger.

    Bound into each record rather than cited by path. The contract is
    notes/-class, which carries no truth guarantee and can be edited, so a
    path reference would let a rewritten contract silently re-authorize
    records assigned under the old text. A hash makes that loud: verify()
    reports every record whose authorizing bytes no longer exist."""
    if not os.path.exists(path):
        raise Refusal("no contract at %s; roles cannot be authorized" % path)
    hasher = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(1024 * 1024):
            hasher.update(chunk)
    return hasher.hexdigest()


def check_schema(schema):
    """Refuse any schema outside the closed set, before it can reach a
    sealing decision that works by membership."""
    if schema not in SCHEMAS:
        raise Refusal("schema %r is outside the manifest's closed set %s; an "
                      "unrecognized schema must not fall through to open"
                      % (schema, ", ".join(SCHEMAS)))
    return schema


def derive_role(instrument, month):
    """The role for this instrument-month, from the contract's tables. Never
    a parameter, so no caller can supply a convenient one."""
    if month not in ACQUISITION_MONTHS:
        raise Refusal("%s %s is outside the acquisition range; the "
                      "assignment table is immutable from signature and a "
                      "month absent from it is not deliverable under this "
                      "contract" % (instrument, month))
    if instrument in BORROW_TRACK_INSTRUMENTS:
        return "unused-sealed"
    if instrument != "MNQ":
        raise Refusal("instrument %r appears in no assignment table; this "
                      "contract assigns MNQ, ES and MES only" % instrument)
    return MNQ_ASSIGNMENT[month]


def check_delivery_state(delivery_state):
    """Refuse a delivery state outside the closed set.

    Without this, derive_state's else-branch treats every unrecognized value
    as complete, so a record edited to carry a nonsense delivery state would
    derive as OPEN. That is the incomplete slice's escape hatch: give it an
    unknown state, clear its interval fields, and the truncated month reads
    as a complete one."""
    if delivery_state not in DELIVERY_STATES:
        raise Refusal("delivery state %r is outside the closed set %s; an "
                      "unrecognized value must not fall through to complete"
                      % (delivery_state, ", ".join(DELIVERY_STATES)))
    return delivery_state


def state_progression_ok(derived, stored):
    """Whether a stored state is the derived initial state or a legitimate
    progression from it. Only an open file may progress, and only into the
    terminal states a read produces."""
    if stored == derived:
        return True
    return (derived == STATE_OPEN
            and stored in (STATE_READ, STATE_SPENT, STATE_CONTAMINATED))


def derive_state(role, schema, delivery_state):
    """The initial state, derived from role, schema and delivery state
    together. Sealing is a property of the instrument-month-schema triple,
    not of the month alone."""
    check_schema(schema)
    check_delivery_state(delivery_state)
    if delivery_state == DELIVERY_INCOMPLETE:
        # Provision 1 holds the covered slice unread whatever the role.
        return STATE_UNREAD_INCOMPLETE
    if schema in SEALED_SCHEMAS:
        return STATE_SEALED
    return STATE_OPEN if role in OPEN_ROLES else STATE_SEALED


def now_iso():
    return dt.datetime.now(dt.UTC).isoformat()


def record_key(instrument, month, schema, filename):
    """One record per DELIVERED FILE, which is what the contract's ledger
    enumerates. The instrument-month-schema triple names a sealed
    POPULATION, not a file: a job can deliver several files for one triple,
    and keying on the triple alone would either alias the second file onto
    the first or reject it as a differing redelivery, losing its individual
    hash and delivery provenance either way. Sealing policy still derives
    from the triple; identity does not."""
    return "%s|%s|%s|%s" % (instrument, month, schema, filename)


def load_ledger(path=LEDGER_FILE):
    """An unreadable ledger is FATAL, never discarded. It is the only record
    of which bytes may be looked at."""
    if not os.path.exists(path):
        return {}
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (json.JSONDecodeError, OSError) as exc:
        raise Refusal("seal ledger %s unreadable (%s); fix it by hand, it is "
                      "the record of what may be read" % (path, exc)) from exc
    if data.get("_version") != LEDGER_VERSION:
        raise Refusal("seal ledger %s has version %r, expected %d" % (
            path, data.get("_version"), LEDGER_VERSION))
    records = data.get("records")
    if not isinstance(records, dict):
        raise Refusal("seal ledger %s has no records table" % path)
    return records


def save_ledger(records, path=LEDGER_FILE):
    """Atomic and durable, like the job ledger: a role assignment that does
    not survive a power cut is a role that can be reassigned after a look."""
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump({"_version": LEDGER_VERSION, "tool_version": TOOL_VERSION,
                   "records": records}, fh, indent=1, sort_keys=True)
        fh.write("\n")
        fh.flush()
        os.fsync(fh.fileno())
    os.replace(tmp, path)
    dir_fd = os.open(os.path.dirname(path) or ".", os.O_RDONLY)
    try:
        os.fsync(dir_fd)
    finally:
        os.close(dir_fd)


def record_delivery(records, instrument, month, schema, filename, dataset,
                    sha256, job_id, delivered_at,
                    delivery_state=DELIVERY_COMPLETE,
                    covered_interval=None, edge_timestamps=None,
                    coverage_quote=None, coverage_checked_at=None,
                    contract_sha=None):
    """Bind a delivered file to its role, at delivery, before any read.

    Idempotent for an identical redelivery: the same bytes under the same key
    return the existing record untouched. DIFFERENT bytes under an existing
    key are a refusal, never an overwrite - a second delivery that differs is
    either a vendor change or the wrong file, and both need a human.

    coverage_quote and coverage_checked_at record the entitlement verdict AT
    PULL TIME, with its timestamp, because the subscription window rolls: a
    month that quoted zero today can quote nonzero next month, so "covered"
    is an observation with a date, not a standing property of the file."""
    if delivery_state not in DELIVERY_STATES:
        raise Refusal("delivery state %r is not one of %s" % (
            delivery_state, ", ".join(DELIVERY_STATES)))
    if not filename:
        raise Refusal("%s %s %s needs a delivered filename; the ledger "
                      "enumerates files, not populations" % (
                          instrument, month, schema))
    if delivery_state == DELIVERY_INCOMPLETE and not (covered_interval
                                                      and edge_timestamps):
        raise Refusal("%s %s %s is an incomplete_month_delivery and must "
                      "record BOTH its exact covered interval and its edge "
                      "timestamps; Amendment 1 provision 1 requires them "
                      "alongside the opaque hash" % (instrument, month,
                                                     schema))
    check_schema(schema)
    role = derive_role(instrument, month)
    state = derive_state(role, schema, delivery_state)
    key = record_key(instrument, month, schema, filename)
    authorizing = contract_sha or contract_hash()
    existing = records.get(key)
    if existing is not None:
        # Idempotency must mean "the same delivery, recorded again", not
        # "close enough". A matching hash alone would let a stale or
        # hand-edited record - one authorized under superseded contract
        # bytes, or carrying a role the table no longer derives - be
        # returned as success, after which the caller marks the job
        # downloaded and the contradiction is never surfaced. Every
        # immutable field must agree, or this is a conflict for a human.
        expected = {
            # Identity. These also compose the key, so a disagreement means
            # the record's own fields were edited away from the key that
            # addresses it.
            "instrument": instrument,
            "month": month,
            "schema": schema,
            "filename": filename,
            # Provenance and authorization.
            "sha256": sha256,
            "dataset": dataset,
            "delivery_job": job_id,
            "role": role,
            "delivery_state": delivery_state,
            "covered_interval": covered_interval,
            "edge_timestamps": edge_timestamps,
            "authorizing_contract_sha256": authorizing,
        }
        disagree = [field for field, want in expected.items()
                    if existing.get(field) != want]
        if disagree:
            raise Refusal("%s is already recorded and this redelivery "
                          "disagrees on %s; a differing redelivery is never "
                          "an overwrite, and a record that contradicts the "
                          "current contract or delivery must be resolved by "
                          "hand" % (key, ", ".join(sorted(disagree))))
        # The stored state is not in the comparison above because it is
        # allowed to have MOVED since delivery - an open file that has been
        # read is legitimately STATE_READ. What it may not be is a state
        # unreachable from the derived one, which is what a corrupted or
        # hand-opened record looks like.
        if not state_progression_ok(state, existing.get("state")):
            raise Refusal("%s is already recorded with state %r, which is "
                          "not %r nor a legitimate progression from it; "
                          "reporting this as a settled redelivery would let "
                          "a corrupt seal record pass for a bound one" % (
                              key, existing.get("state"), state))
        return existing
    records[key] = {
        "instrument": instrument,
        "month": month,
        "schema": schema,
        "filename": filename,
        "dataset": dataset,
        "sha256": sha256,
        "delivery_job": job_id,
        "delivered_at": delivered_at,
        "delivery_state": delivery_state,
        "covered_interval": covered_interval,
        "edge_timestamps": edge_timestamps,
        "coverage_quote_at_pull": coverage_quote,
        "coverage_checked_at": coverage_checked_at,
        "role": role,
        "role_assigned_at": now_iso(),
        "authorizing_contract_sha256": authorizing,
        "first_content_read_at": None,
        "unseal_authority": None,
        "reading_process": None,
        "artifact_outputs": [],
        "state": state,
    }
    return records[key]


def record_unseal(records, instrument, month, schema, filename, authority,
                  process, artifact_outputs=None, contract_sha=None):
    """Record a first content read. Refuses every case the contract forbids,
    and refuses silence: an unseal with no named authority and no named
    reading process is not a record of anything.

    AUTHORIZATION IS REDERIVED, NEVER READ OUT OF THE RECORD. The stored
    state is mutable JSON on disk; if permission were taken from it, editing
    June's state to "open" would be enough to authorize reading the
    primary-confirmation corpus, and the same one-word edit would open MBP-1
    or the borrow track. So the role and the initial restriction are
    recomputed here from instrument, month, schema and delivery state - the
    same derivation delivery used - and the stored role must agree with it.
    Both the DERIVED restriction and the STORED state must be exactly open.
    Rederiving alone is not enough, because the derivation itself consumes a
    stored field: flipping delivery_state from incomplete_month_delivery to
    complete would make 2025-08 derive as open. Requiring agreement means a
    single forged field contradicts the other rather than granting a read.

    The authorizing contract hash is checked HERE, not only in verify. An
    invariant that holds only when someone remembers to run the checker is
    not an invariant on the authorization path.

    Honest limit: this is tamper-EVIDENT, not tamper-proof. Anyone who can
    write the ledger can in principle forge a fully self-consistent record,
    and no unkeyed check can stop that. What it does stop is the realistic
    case - one field edited, by hand or by a well-meaning script - becoming
    permission to read a sealed corpus."""
    key = record_key(instrument, month, schema, filename)
    entry = records.get(key)
    if entry is None:
        raise Refusal("%s has no delivery record; a file cannot be unsealed "
                      "before it is bound" % key)
    if not authority or not process:
        raise Refusal("%s needs both an unseal authority and a named reading "
                      "process; an unattributed read is indistinguishable "
                      "from contamination" % key)
    check_schema(schema)
    current = contract_sha or contract_hash()
    if entry.get("authorizing_contract_sha256") != current:
        raise Refusal("%s was authorized under contract %s but the contract "
                      "on disk now hashes %s; the bytes that assigned this "
                      "role no longer exist and no read is authorized until "
                      "the ledger is reconciled against the current "
                      "contract" % (
                          key,
                          str(entry.get("authorizing_contract_sha256"))[:12],
                          current[:12]))
    role = derive_role(instrument, month)
    if entry.get("role") != role:
        raise Refusal("%s stores role %r but the assignment table derives "
                      "%r; the ledger has been edited and no read is "
                      "authorized until that is resolved" % (
                          key, entry.get("role"), role))
    derived = derive_state(role, schema, entry.get("delivery_state"))
    stored = entry.get("state")
    if derived == STATE_UNREAD_INCOMPLETE or stored == STATE_UNREAD_INCOMPLETE:
        raise Refusal("%s is an incomplete_month_delivery and stays UNREAD "
                      "unless a later reviewed amendment defines a "
                      "partial-month estimand; Amendment 1 provision 1" % key)
    if stored in (STATE_READ, STATE_SPENT, STATE_CONTAMINATED):
        raise Refusal("%s already carries a first content read at %s; a "
                      "first read happens once" % (
                          key, entry.get("first_content_read_at")))
    if derived != STATE_OPEN or stored != STATE_OPEN:
        raise Refusal("%s does not authorize a read: it derives as %r and "
                      "stores %r, and both must be open. Unsealing a "
                      "confirmation month is Stage C's act alone, and MBP-1 "
                      "and the borrow track need a new signed contract"
                      % (key, derived, stored))
    if (entry.get("delivery_state") == DELIVERY_COMPLETE
            and (entry.get("covered_interval")
                 or entry.get("edge_timestamps"))):
        raise Refusal("%s claims a complete delivery yet carries the covered "
                      "interval and edge timestamp only a truncated one has; "
                      "the record contradicts itself" % key)
    entry["first_content_read_at"] = now_iso()
    entry["unseal_authority"] = authority
    entry["reading_process"] = process
    entry["artifact_outputs"] = list(artifact_outputs or [])
    entry["state"] = STATE_READ
    return entry


def verify(records, contract_sha=None):
    """Invariants over the whole ledger. Returns the named problem list,
    empty when everything holds."""
    current = contract_sha or contract_hash()
    problems = []
    for key in sorted(records):
        entry = records[key]
        role = entry.get("role")
        if role not in ROLES:
            problems.append("%s: role %r is outside the closed set" % (
                key, role))
        if entry.get("state") not in STATES:
            problems.append("%s: state %r is outside the closed set" % (
                key, entry.get("state")))
        expected = derive_role(entry["instrument"], entry["month"])
        if role != expected:
            problems.append("%s: recorded role %r contradicts the assignment "
                            "table, which says %r" % (key, role, expected))
        if entry.get("authorizing_contract_sha256") != current:
            problems.append("%s: authorized under contract %s, but the "
                            "contract on disk now hashes %s" % (
                                key,
                                str(entry.get(
                                    "authorizing_contract_sha256"))[:12],
                                current[:12]))
        # The stored state must agree with what role, schema and delivery
        # state derive. Disagreement is the signature of a hand edit, and it
        # is exactly what record_unseal refuses on, so the checker must see
        # it too. STATE_READ and the terminal states are legitimate
        # progressions from open, not disagreements.
        if entry.get("delivery_state") not in DELIVERY_STATES:
            problems.append("%s: delivery state %r is outside the closed set"
                            % (key, entry.get("delivery_state")))
        # Membership is checked first: derive_state refuses an unknown
        # schema or delivery state, and the checker must REPORT that rather
        # than raise out of the middle of a verification pass.
        if (role in ROLES and entry.get("schema") in SCHEMAS
                and entry.get("delivery_state") in DELIVERY_STATES):
            expect_state = derive_state(role, entry["schema"],
                                        entry["delivery_state"])
            stored = entry.get("state")
            if not state_progression_ok(expect_state, stored):
                problems.append("%s: stores state %r but role, schema and "
                                "delivery state derive %r" % (
                                    key, stored, expect_state))
        if (entry.get("delivery_state") == DELIVERY_COMPLETE
                and (entry.get("covered_interval")
                     or entry.get("edge_timestamps"))):
            problems.append("%s: complete delivery carrying a covered "
                            "interval or edge timestamp" % key)
        read_at = entry.get("first_content_read_at")
        if read_at and entry.get("state") == STATE_SEALED:
            problems.append("%s: sealed yet carries a content read at %s" % (
                key, read_at))
        if read_at and not entry.get("unseal_authority"):
            problems.append("%s: content read at %s with no named authority"
                            % (key, read_at))
        incomplete = entry.get("delivery_state") == DELIVERY_INCOMPLETE
        if incomplete and not entry.get("covered_interval"):
            problems.append("%s: incomplete delivery with no covered interval"
                            % key)
        if incomplete and not entry.get("edge_timestamps"):
            problems.append("%s: incomplete delivery with no edge timestamps"
                            % key)
        if entry.get("schema") not in SCHEMAS:
            problems.append("%s: schema %r is outside the closed set" % (
                key, entry.get("schema")))
        if not entry.get("filename"):
            problems.append("%s: no delivered filename recorded" % key)
    return problems


def mode_status():
    records = load_ledger()
    if not records:
        print("seal ledger empty (%s)" % LEDGER_FILE)
        return
    print("%s: %d record(s)" % (LEDGER_FILE, len(records)))
    print("%-22s %-22s %-20s %s" % ("file", "role", "state", "read"))
    for key in sorted(records):
        entry = records[key]
        print("%-22s %-22s %-20s %s" % (
            key, entry.get("role"), entry.get("state"),
            entry.get("first_content_read_at") or "-"))


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
    fake_contract = "c" * 64
    # The 2025-08 entitlement edge as a UTC instant: the covered slice starts
    # at the 08-11 17:00 Central session boundary, not at bare 08-12.
    EDGE = "2025-08-11T22:00:00"

    print("roles derive from the tables, never from a caller")
    check("June is primary-confirmation",
          derive_role("MNQ", "2026-06") == "primary-confirmation")
    check("May is reserve-confirmation",
          derive_role("MNQ", "2026-05") == "reserve-confirmation")
    check("July is spent-design",
          derive_role("MNQ", "2026-07") == "spent-design")
    check("a design month is new-design",
          derive_role("MNQ", "2026-01") == "new-design")
    check("2026-08 is unused-sealed by Amendment 1 provision 2",
          derive_role("MNQ", "2026-08") == "unused-sealed")
    check("ES is borrow-track sealed",
          derive_role("ES", "2026-01") == "unused-sealed")
    check("MES is borrow-track sealed",
          derive_role("MES", "2026-01") == "unused-sealed")
    check("an unassigned month refuses",
          refuses(derive_role, "MNQ", "2024-05"))
    check("an unknown instrument refuses",
          refuses(derive_role, "NQ", "2026-01"))
    check("ES outside the acquisition range refuses, not unused-sealed",
          refuses(derive_role, "ES", "2024-05"))
    check("MES outside the acquisition range refuses",
          refuses(derive_role, "MES", "2011-08"))

    print("the schema set is closed")
    check("tbbo is admitted", check_schema("tbbo") == "tbbo")
    check("mbp-1 is admitted", check_schema("mbp-1") == "mbp-1")
    for bad in ("MBP-1", "mbp1", "trades", "", None):
        check("schema %r refuses rather than falling through to open" % (bad,),
              refuses(check_schema, bad))
    check("derive_state refuses an unknown schema outright",
          refuses(derive_state, "new-design", "trades", DELIVERY_COMPLETE))

    print("state derives from role, schema and delivery together")
    check("new-design TBBO is open",
          derive_state("new-design", "tbbo", DELIVERY_COMPLETE) == STATE_OPEN)
    check("new-design MBP-1 is sealed even though the month is open",
          derive_state("new-design", "mbp-1",
                       DELIVERY_COMPLETE) == STATE_SEALED)
    check("primary-confirmation TBBO is sealed",
          derive_state("primary-confirmation", "tbbo",
                       DELIVERY_COMPLETE) == STATE_SEALED)
    check("spent-design TBBO is open",
          derive_state("spent-design", "tbbo",
                       DELIVERY_COMPLETE) == STATE_OPEN)
    check("an incomplete delivery is unread whatever the role",
          derive_state("new-design", "tbbo",
                       DELIVERY_INCOMPLETE) == STATE_UNREAD_INCOMPLETE)
    # Without a closed check, an unrecognized delivery state falls through
    # derive_state's else-branch to complete, which is the truncated month's
    # escape hatch.
    for bad in ("partial", "COMPLETE", "", None):
        check("delivery state %r refuses rather than deriving complete"
              % (bad,), refuses(derive_state, "new-design", "tbbo", bad))

    print("delivery binds the role before any read")
    recs = {}
    jun = "glbx-mdp3-202606.tbbo.csv.zst"
    entry = record_delivery(recs, "MNQ", "2026-06", "tbbo", jun, "GLBX.MDP3",
                            sha, "JOB-A", "2026-08-12T00:00:00+00:00",
                            contract_sha=fake_contract)
    check("June TBBO lands sealed with its role bound",
          entry["role"] == "primary-confirmation"
          and entry["state"] == STATE_SEALED
          and entry["first_content_read_at"] is None
          and entry["authorizing_contract_sha256"] == fake_contract)
    check("the role carries an assignment timestamp",
          entry["role_assigned_at"].endswith("+00:00"))
    check("the record names the delivered file", entry["filename"] == jun)
    same = record_delivery(recs, "MNQ", "2026-06", "tbbo", jun, "GLBX.MDP3",
                           sha, "JOB-A", "2026-08-12T00:00:00+00:00",
                           contract_sha=fake_contract)
    check("an identical redelivery is idempotent", same is entry)
    # Idempotency must mean the same delivery, not merely the same bytes: a
    # stale or hand-edited record must not be returned as success and let the
    # caller mark the job downloaded.
    for field, value in (("dataset", "OTHER.DATASET"),
                         ("delivery_job", "JOB-ELSEWHERE"),
                         ("role", "new-design"),
                         ("delivery_state", DELIVERY_INCOMPLETE),
                         ("authorizing_contract_sha256", "f" * 64),
                         ("instrument", "ES"),
                         ("month", "2026-01"),
                         ("schema", "mbp-1"),
                         ("filename", "somethingelse.zst"),
                         ("state", STATE_OPEN)):
        stale = json.loads(json.dumps(recs))
        stale[record_key("MNQ", "2026-06", "tbbo", jun)][field] = value
        check("a redelivery disagreeing on %s refuses" % field,
              refuses(record_delivery, stale, "MNQ", "2026-06", "tbbo", jun,
                      "GLBX.MDP3", sha, "JOB-A",
                      "2026-08-12T00:00:00+00:00",
                      contract_sha=fake_contract))
    check("a redelivery under the same everything still succeeds",
          record_delivery(recs, "MNQ", "2026-06", "tbbo", jun, "GLBX.MDP3",
                          sha, "JOB-A", "2026-08-13T00:00:00+00:00",
                          contract_sha=fake_contract) is entry)
    check("re-binding preserves the ORIGINAL delivery timestamp",
          entry["delivered_at"] == "2026-08-12T00:00:00+00:00")
    check("a redelivery with different bytes refuses",
          refuses(record_delivery, recs, "MNQ", "2026-06", "tbbo", jun,
                  "GLBX.MDP3", "b" * 64, "JOB-B",
                  "2026-08-12T00:00:00+00:00", contract_sha=fake_contract))
    # A second FILE of the same triple is a distinct record, not an alias and
    # not a differing redelivery: the ledger enumerates files.
    second = record_delivery(recs, "MNQ", "2026-06", "tbbo",
                             "glbx-mdp3-202606.tbbo.part2.csv.zst",
                             "GLBX.MDP3", "b" * 64, "JOB-A",
                             "2026-08-12T00:00:00+00:00",
                             contract_sha=fake_contract)
    check("a second file of the same triple gets its own record and hash",
          second is not entry and second["sha256"] == "b" * 64
          and len([k for k in recs if k.startswith("MNQ|2026-06|tbbo|")]) == 2)
    check("delivery without a filename refuses",
          refuses(record_delivery, recs, "MNQ", "2026-04", "tbbo", "",
                  "GLBX.MDP3", sha, "JOB-X", "2026-08-12T00:00:00+00:00",
                  contract_sha=fake_contract))
    check("delivery of an unknown schema refuses",
          refuses(record_delivery, recs, "MNQ", "2026-04", "trades", "f.zst",
                  "GLBX.MDP3", sha, "JOB-X", "2026-08-12T00:00:00+00:00",
                  contract_sha=fake_contract))
    check("an incomplete delivery without a covered interval refuses",
          refuses(record_delivery, recs, "MNQ", "2025-08", "tbbo", "aug.zst",
                  "GLBX.MDP3", sha, "JOB-C", "2026-08-12T00:00:00+00:00",
                  delivery_state=DELIVERY_INCOMPLETE,
                  edge_timestamps=[{"side": "start",
                                    "instant": EDGE}],
                  contract_sha=fake_contract))
    check("an incomplete delivery without an edge timestamp refuses",
          refuses(record_delivery, recs, "MNQ", "2025-08", "tbbo", "aug.zst",
                  "GLBX.MDP3", sha, "JOB-C", "2026-08-12T00:00:00+00:00",
                  delivery_state=DELIVERY_INCOMPLETE,
                  covered_interval=["2025-08-12T08:00:00+00:00",
                                    "2025-09-01T00:00:00+00:00"],
                  contract_sha=fake_contract))
    aug = record_delivery(recs, "MNQ", "2025-08", "tbbo", "aug.zst",
                          "GLBX.MDP3", sha, "JOB-C",
                          "2026-08-12T00:00:00+00:00",
                          delivery_state=DELIVERY_INCOMPLETE,
                          covered_interval=["2025-08-12T08:00:00+00:00",
                                            "2025-09-01T00:00:00+00:00"],
                          edge_timestamps=[{"side": "start",
                                            "instant": EDGE}],
                          coverage_quote=9.41,
                          coverage_checked_at="2026-08-12T00:00:00+00:00",
                          contract_sha=fake_contract)
    check("2025-08 records its interval, edge and coverage quote",
          aug["state"] == STATE_UNREAD_INCOMPLETE
          and aug["edge_timestamps"] and aug["coverage_quote_at_pull"] == 9.41)

    print("unsealing refuses everything the contract forbids")
    jan = "glbx-mdp3-202601.tbbo.csv.zst"
    janmbp = "glbx-mdp3-202601.mbp-1.csv.zst"
    check("a sealed confirmation month cannot be unsealed here",
          refuses(record_unseal, recs, "MNQ", "2026-06", "tbbo", jun,
                  "owner", "stage-c"))
    check("an incomplete month stays unread",
          refuses(record_unseal, recs, "MNQ", "2025-08", "tbbo", "aug.zst",
                  "owner", "stage-m"))
    check("an unrecorded file cannot be unsealed",
          refuses(record_unseal, recs, "MNQ", "2026-01", "tbbo", jan,
                  "owner", "stage-m"))
    record_delivery(recs, "MNQ", "2026-01", "tbbo", jan, "GLBX.MDP3", sha,
                    "JOB-D", "2026-08-12T00:00:00+00:00",
                    contract_sha=fake_contract)
    check("an open month needs a named authority",
          refuses(record_unseal, recs, "MNQ", "2026-01", "tbbo", jan, "", "x"))
    check("an open month needs a named process",
          refuses(record_unseal, recs, "MNQ", "2026-01", "tbbo", jan,
                  "owner", ""))
    read = record_unseal(recs, "MNQ", "2026-01", "tbbo", jan, "owner",
                         "stage-m measure", ["analysis/out/m-2026-01.json"],
                         contract_sha=fake_contract)
    check("an open month records its read, authority and outputs",
          read["state"] == STATE_READ and read["first_content_read_at"]
          and read["unseal_authority"] == "owner"
          and read["artifact_outputs"] == ["analysis/out/m-2026-01.json"])
    check("a second first-read refuses",
          refuses(record_unseal, recs, "MNQ", "2026-01", "tbbo", jan,
                  "owner", "again"))
    mbp = record_delivery(recs, "MNQ", "2026-01", "mbp-1", janmbp,
                          "GLBX.MDP3", sha, "JOB-E",
                          "2026-08-12T00:00:00+00:00",
                          contract_sha=fake_contract)
    check("MBP-1 of an open month is sealed", mbp["state"] == STATE_SEALED)
    check("MBP-1 of an open month cannot be read",
          refuses(record_unseal, recs, "MNQ", "2026-01", "mbp-1", janmbp,
                  "owner", "stage-m"))

    # The P1 that motivated rederiving: stored state is mutable JSON, so a
    # one-word edit must not become permission.
    print("a tampered state does not become permission")
    for key, label in ((record_key("MNQ", "2026-06", "tbbo", jun),
                        "June TBBO"),
                       (record_key("MNQ", "2026-01", "mbp-1", janmbp),
                        "open-month MBP-1"),
                       (record_key("MNQ", "2025-08", "tbbo", "aug.zst"),
                        "the incomplete slice")):
        forged = json.loads(json.dumps(recs))
        forged[key]["state"] = STATE_OPEN
        parts = key.split("|")
        check("%s forged to open is still refused" % label,
              refuses(record_unseal, forged, parts[0], parts[1], parts[2],
                      parts[3], "owner", "stage-m"))
    forged = json.loads(json.dumps(recs))
    fkey = record_key("MNQ", "2026-06", "tbbo", jun)
    forged[fkey]["state"] = STATE_OPEN
    forged[fkey]["role"] = "new-design"
    check("June forged to an open ROLE and state is still refused",
          refuses(record_unseal, forged, "MNQ", "2026-06", "tbbo", jun,
                  "owner", "stage-m"))
    # Rederiving consumes delivery_state, so that field is itself an attack
    # surface: flipping it to complete makes 2025-08 derive as open.
    akey0 = record_key("MNQ", "2025-08", "tbbo", "aug.zst")
    forged = json.loads(json.dumps(recs))
    forged[akey0]["delivery_state"] = DELIVERY_COMPLETE
    check("the incomplete slice forged to a complete delivery is refused",
          refuses(record_unseal, forged, "MNQ", "2025-08", "tbbo", "aug.zst",
                  "owner", "stage-m", contract_sha=fake_contract))
    forged = json.loads(json.dumps(recs))
    forged[akey0]["delivery_state"] = DELIVERY_COMPLETE
    forged[akey0]["state"] = STATE_OPEN
    check("forging BOTH delivery state and state still hits the "
          "self-contradiction check",
          refuses(record_unseal, forged, "MNQ", "2025-08", "tbbo", "aug.zst",
                  "owner", "stage-m", contract_sha=fake_contract))

    # The full escape hatch, end to end: give the truncated slice an
    # unrecognized delivery state so it derives as complete, forge the state
    # to open, and clear the interval fields that would contradict it.
    forged = json.loads(json.dumps(recs))
    forged[akey0]["delivery_state"] = "partial"
    forged[akey0]["state"] = STATE_OPEN
    forged[akey0]["covered_interval"] = None
    forged[akey0]["edge_timestamps"] = None
    check("an unknown delivery state cannot launder the incomplete slice",
          refuses(record_unseal, forged, "MNQ", "2025-08", "tbbo", "aug.zst",
                  "owner", "stage-m", contract_sha=fake_contract))
    problems = verify(forged, contract_sha=fake_contract)
    check("verify reports the unknown delivery state rather than raising",
          any("delivery state" in p and "closed set" in p for p in problems))

    print("authorization checks the contract hash, not just verify")
    forged = json.loads(json.dumps(recs))
    okkey = record_key("MNQ", "2026-04", "tbbo", "apr.zst")
    record_delivery(forged, "MNQ", "2026-04", "tbbo", "apr.zst", "GLBX.MDP3",
                    sha, "JOB-F", "2026-08-12T00:00:00+00:00",
                    contract_sha=fake_contract)
    check("an open record reads under its authorizing contract",
          record_unseal(forged, "MNQ", "2026-04", "tbbo", "apr.zst", "owner",
                        "stage-m", contract_sha=fake_contract)["state"]
          == STATE_READ)
    forged2 = json.loads(json.dumps(recs))
    record_delivery(forged2, "MNQ", "2026-04", "tbbo", "apr.zst",
                    "GLBX.MDP3", sha, "JOB-F", "2026-08-12T00:00:00+00:00",
                    contract_sha=fake_contract)
    check("the same record refuses once the contract bytes change",
          refuses(record_unseal, forged2, "MNQ", "2026-04", "tbbo",
                  "apr.zst", "owner", "stage-m", contract_sha="e" * 64))
    check("the record was left unread by the refusal",
          forged2[okkey]["state"] == STATE_OPEN
          and forged2[okkey]["first_content_read_at"] is None)

    print("verify catches tampering")
    check("a consistent ledger verifies",
          verify(recs, contract_sha=fake_contract) == [])
    problems = verify(recs, contract_sha="d" * 64)
    check("a changed contract invalidates every authorization",
          len(problems) == len(recs))
    tampered = json.loads(json.dumps(recs))
    tampered[fkey]["role"] = "new-design"
    problems = verify(tampered, contract_sha=fake_contract)
    check("a hand-edited role contradicts the table and is caught",
          any("contradicts the assignment table" in p for p in problems))
    tampered = json.loads(json.dumps(recs))
    tampered[fkey]["first_content_read_at"] = "2026-08-12T00:00:00+00:00"
    problems = verify(tampered, contract_sha=fake_contract)
    check("a sealed record carrying a read is caught",
          any("sealed yet carries a content read" in p for p in problems))
    tampered = json.loads(json.dumps(recs))
    akey = record_key("MNQ", "2025-08", "tbbo", "aug.zst")
    tampered[akey]["edge_timestamps"] = None
    problems = verify(tampered, contract_sha=fake_contract)
    check("an incomplete record stripped of its edge timestamp is caught",
          any("no edge timestamps" in p for p in problems))
    tampered = json.loads(json.dumps(recs))
    tampered[akey]["schema"] = "trades"
    problems = verify(tampered, contract_sha=fake_contract)
    check("a record edited to an out-of-set schema is caught",
          any("outside the closed set" in p for p in problems))

    print("ledger round trip")
    if os.path.exists(SELFTEST_DIR):
        import shutil
        shutil.rmtree(SELFTEST_DIR)
    os.makedirs(SELFTEST_DIR)
    path = os.path.join(SELFTEST_DIR, "seal.json")
    save_ledger(recs, path)
    loaded = load_ledger(path)
    check("round trip preserves every record", loaded == recs)
    with open(path, "w") as fh:
        fh.write("{ not json")
    check("an unreadable seal ledger is fatal, not discarded",
          refuses(load_ledger, path))
    check("a missing seal ledger is empty, not fatal",
          load_ledger(os.path.join(SELFTEST_DIR, "absent.json")) == {})
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
    parser.add_argument("mode", choices=["selftest", "status", "verify"])
    args = parser.parse_args()
    if args.mode == "selftest":
        selftest()
    elif args.mode == "status":
        mode_status()
    else:
        mode_verify()


if __name__ == "__main__":
    main()
