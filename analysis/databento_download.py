#!/usr/bin/env python3
"""Buy, poll and download the staged Databento purchase, doubly gated.

The staged-purchase downloader of DATA-PURCHASE-REPORT.md 9.6/14.1. The
pricing tool
analysis/databento_price.py remains the single source of truth for SCOPES,
WINDOWS, PLANS and session_bounds_utc; this file imports them and never
duplicates a window. This file is the ONLY place in analysis/ that can reach
batch.submit_job, and it can reach it through exactly one function,
submit_gated(), which enforces --confirm AND --max-dollars AND a fresh
re-price with a 10 percent drift refusal. A plain invocation submits nothing.

Four modes:

    selftest   fixture-driven tests of the gates, the ledger, and the download
               lifecycle; no network anywhere
    status     print the committed job ledger; offline
    plan       fresh (uncached) quotes for one scope/plan, the would-submit
               table with drift verdicts; free metadata endpoints only
    buy        the lifecycle driver: submit (gated), poll, download, verify.
               Whitelisted to the staged purchase - pairv/paircurrent, then
               nqv/contiguous once the paired test has landed - and
               all-or-nothing: preflight covers every row before the first
               POST, and the first failure stops further submissions.
               Without BOTH --confirm and --max-dollars it SUBMITS nothing,
               but it still polls and downloads jobs the ledger already owns
               over the network - those bytes are already paid for.
               Exit codes: 0 every plan entry settled, 3 nothing failed but
               jobs are still queued or processing (re-run to poll), 1 any
               failure or refusal. 3 is deliberately distinct so
               orchestration cannot mistake an undelivered purchase for a
               finished one.

Usage:
    python3 -u analysis/databento_download.py selftest
    python3 -u analysis/databento_download.py status
    python3 -u analysis/databento_download.py plan pairv paircurrent
    python3 -u analysis/databento_download.py buy pairv paircurrent
    python3 -u analysis/databento_download.py buy pairv paircurrent \
        --confirm --max-dollars 5

The ledger analysis/databento-jobs.json is committed; it is the spend record.
A ledger entry that carries a job id is NEVER resubmitted: re-running `buy`
polls and downloads instead. Delivered files land under
research/market-data/databento/<scope>/<window>.<schema>/ (gitignored), each
directory carrying a manifest.json with the verified inventory.

The key is read exactly the way databento_price reads it:
research/databento.key or DATABENTO_API_KEY. Never commit it.
"""

import argparse
import base64
import datetime as dt
import fcntl
import hashlib
import json
import math
import os
import shutil
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

from compression import zstd  # stdlib from Python 3.14

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databento_price as dp  # noqa: E402  (SCOPES, WINDOWS, PLANS, bounds)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LEDGER_FILE = os.path.join(ROOT, "analysis", "databento-jobs.json")
LANDING_ROOT = os.path.join(ROOT, "research", "market-data", "databento")
SELFTEST_DIR = os.path.join(ROOT, "analysis", "out", "downloader-selftest")

TOOL_VERSION = 1
LEDGER_VERSION = 1

# Refuse to submit if the fresh quote exceeds the plan-time quote by more than
# this fraction. Vendor rates are not eternal; a cached quote is only as good
# as its timestamp.
DRIFT_LIMIT = 0.10

# A drift comparison against an ancient baseline defeats the freshness the
# gate exists for: a six-month-old quote that drifted 9 percent would pass.
# Baselines older than this are treated as absent, and the refusal tells the
# operator to re-price (databento_price.py serves its cache first, so a
# genuine re-price needs its --refresh flag).
BASELINE_MAX_AGE_DAYS = 7

# Download engine constants, following the proven shape in
# research/databento-ingest/src/databento_ingest/downloader.py.
CHUNK_SIZE = 4 * 1024 * 1024
MAX_RETRIES = 5
RETRY_DELAY_BASE = 10  # seconds; 10, 20, 40, 80 between the five attempts
HTTP_TIMEOUT = 120

# Batch request shape. Field names and value spellings are taken verbatim from
# research/databento-python/databento/historical/api/batch.py submit_job(),
# whose POST body is asserted key-for-key in
# research/databento-python/tests/test_historical_batch.py
# test_batch_submit_job_sends_expected_request. The SDK passes Python bools to
# requests, which form-encodes str(bool), so "True"/"False" is what the live
# API demonstrably accepts; we reproduce that byte-for-byte rather than
# guessing at "true".
ENCODING = "csv"
COMPRESSION = "zstd"
SPLIT_DURATION = "month"
STYPE_OUT = "instrument_id"

# The request-defining fields shared by the submit body and pending-intent
# reconciliation. ONE table, so the matcher can never drift from what was
# actually posted. stype_in is per-scope and joins at use.
SUBMIT_DEFAULTS = {
    "encoding": ENCODING,
    "compression": COMPRESSION,
    "stype_out": STYPE_OUT,
    "split_duration": SPLIT_DURATION,
    "split_symbols": False,
    "delivery": "download",
    "pretty_px": False,
    "pretty_ts": False,
    "map_symbols": True,   # SDK default for csv encoding
}

# Job states, from research/databento-python/databento/common/enums.py
# JobState: queued, processing, done, expired. "downloaded" is OUR terminal
# ledger state, recorded after every file verified on disk; it is not a vendor
# state.
VENDOR_STATES = ("queued", "processing", "done", "expired")

# The zstd frame magic, checked before promoting a delivered .zst file. The
# streaming decompressor validates the frame header more deeply in
# check_zstd_prefix().
ZSTD_MAGIC = b"\x28\xb5\x2f\xfd"


class Refusal(SystemExit):
    """A named, fail-closed refusal. Exits nonzero with the reason."""

    def __init__(self, reason):
        super().__init__("REFUSED: %s" % reason)


# ---------------------------------------------------------------------------
# HTTP, deliberately uncached
# ---------------------------------------------------------------------------
# databento_price.request() is cache-first, which is right for a pricing sweep
# and wrong here: decision 4 of the spec requires the submit-time quote to be
# fresh, and submission itself must never be replayed from a cache. Errors
# RAISE instead of returning sentinels: a purchase path has no good use for a
# half-answer.


def _auth_header():
    token = base64.b64encode(("%s:" % dp.api_key()).encode("ascii"))
    return "Basic %s" % token.decode("ascii")


class _RefuseRedirect(urllib.request.HTTPRedirectHandler):
    """urllib follows redirects by default AND keeps the request's headers,
    so validating the original URL alone would not stop the auth header -
    which IS the key - from following a server redirect to an arbitrary
    host. Every authenticated request in this file goes through an opener
    that refuses redirects outright; the vendor API serves directly."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        raise urllib.error.HTTPError(
            req.full_url, code,
            "redirect to %r refused; the auth header follows no redirect"
            % newurl, headers, fp)


_OPENER = urllib.request.build_opener(_RefuseRedirect)


def http_json(endpoint, params=None, post_data=None):
    """GET (params) or POST (post_data, form-encoded) a hist API endpoint and
    return parsed JSON. Never cached. Failures raise Refusal with the key
    redacted, mirroring the redaction note in databento_price.request()."""
    url = "%s/%s" % (dp.BASE, endpoint)
    body = None
    if post_data is not None:
        # str() every value: this is exactly what requests does to the SDK's
        # dict (bools become "True"/"False", ints their decimal form).
        body = urllib.parse.urlencode(
            {k: str(v) for k, v in post_data.items()}).encode("ascii")
    elif params:
        url = "%s?%s" % (url, urllib.parse.urlencode(params))
    req = urllib.request.Request(
        url, data=body, headers={"Authorization": _auth_header()})
    try:
        with _OPENER.open(req, timeout=HTTP_TIMEOUT) as resp:
            text = resp.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", "replace")[:400]
        detail = detail.replace(dp.api_key(), "<REDACTED-KEY>")
        raise Refusal("%s returned HTTP %s: %s" % (endpoint, exc.code, detail))
    except urllib.error.URLError as exc:
        raise Refusal("%s unreachable: %s" % (endpoint, exc.reason))
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        raise Refusal("%s returned unparseable JSON" % endpoint)


def fresh_quote(scope, window, schema):
    """The live cost in dollars, fetched now, never from cache. Uses the same
    query and mode parameter as databento_price.measure() so the number is
    comparable with the plan-time baseline."""
    params = dp.query(scope, window, schema)
    value = dp.as_number(
        http_json("metadata.get_cost", dict(params, mode="historical-streaming")))
    if value is None or value < 0:
        raise Refusal("live quote for %s/%s/%s unreadable" % (
            scope, window[0], schema))
    return value


def planned_quote(scope, window, schema, now=None):
    """The plan-time baseline: the quote the pricing script cached when the
    plan was accepted. Read from databento_price's own cache with its own key
    derivation, so a re-keying there cannot silently detach the baseline.
    Returns None when no baseline exists OR when the cached one is older than
    BASELINE_MAX_AGE_DAYS - a stale baseline is treated as absent, never as
    a usable number."""
    params = dp.query(scope, window, schema)
    key = dp.cache_key(
        "metadata.get_cost", dict(params, mode="historical-streaming"), False)
    entry = dp.cache().get(key)
    if entry is None:
        return None
    try:
        fetched = dt.datetime.fromisoformat(str(entry.get("fetched", "")))
    except ValueError:
        return None
    if fetched.tzinfo is None:
        fetched = fetched.replace(tzinfo=dt.timezone.utc)
    now = now if now is not None else dt.datetime.now(dt.timezone.utc)
    if (now - fetched).total_seconds() > BASELINE_MAX_AGE_DAYS * 86400:
        return None
    return dp.as_number(entry["body"])


# ---------------------------------------------------------------------------
# Ledger
# ---------------------------------------------------------------------------
# analysis/databento-jobs.json, committed. Keyed "scope|window|schema". Unlike
# the pricing cache, an unreadable ledger is FATAL, not discarded: discarding
# it is exactly how a paid job would be bought twice.


LOCK_FILE = LEDGER_FILE + ".lock"


def acquire_buy_lock(path=LOCK_FILE):
    """Serialize buy runs across PROCESSES. Two armed invocations that each
    load a private ledger snapshot could both see no entry, both write an
    intent, and both POST - atomic file replacement prevents torn JSON but
    provides no mutual exclusion. The flock covers the entire lifecycle:
    ledger load, verdict, intent, POST, response persistence. It is held for
    the process lifetime and released by the kernel on ANY death, so a crash
    cannot leave a stale lock. Non-blocking: a second run refuses loudly
    rather than queueing behind a purchase it cannot see.

    The caller must keep the returned handle alive; closing it releases the
    lock."""
    handle = open(path, "w")
    try:
        fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        handle.close()
        raise Refusal("another databento_download run holds %s; concurrent "
                      "buys could each submit the same purchase" % path)
    return handle


def ledger_key(scope, window_name, schema):
    return "%s|%s|%s" % (scope, window_name, schema)


def load_ledger(path=LEDGER_FILE):
    if not os.path.exists(path):
        return {}
    try:
        with open(path) as fh:
            data = json.load(fh)
    except (json.JSONDecodeError, OSError) as exc:
        raise Refusal("ledger %s unreadable (%s); fix it by hand, it is the "
                      "spend record and will not be discarded" % (path, exc))
    if data.get("_version") != LEDGER_VERSION:
        raise Refusal("ledger %s has version %r, expected %d" % (
            path, data.get("_version"), LEDGER_VERSION))
    jobs = data.get("jobs")
    if not isinstance(jobs, dict):
        raise Refusal("ledger %s has no jobs table" % path)
    return jobs


def durable_json_write(path, payload):
    """Atomic AND durable: os.replace prevents torn JSON but does not survive
    a system crash before the page cache flushes, and the pre-POST intent is
    only worth writing if it outlives a power cut. Flush and fsync the temp,
    replace, then fsync the directory so the rename itself is durable."""
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


def save_ledger(jobs, path=LEDGER_FILE):
    durable_json_write(path, {"_version": LEDGER_VERSION, "jobs": jobs})


# ---------------------------------------------------------------------------
# The double gate
# ---------------------------------------------------------------------------


def submission_verdict(confirm, max_dollars, ledger_entry, live, planned):
    """The pure decision: may THIS entry be submitted, for THIS live quote,
    under THESE flags? Returns (ok, reason). Every refusal is named; the
    selftest drives this function directly against fixture argument sets.

    max_dollars is the REMAINING budget for the run: the caller subtracts each
    accepted quote before deciding the next entry, so a plan cannot slip past
    the cap one affordable row at a time.

    Every monetary value must be a FINITE number. NaN compares false with
    everything, so without the isfinite gates a NaN cap, quote or baseline
    would sail through every ordered comparison and submit.
    """
    if ledger_entry is not None and ledger_entry.get("job_id"):
        return False, "already submitted as job %s; poll, never resubmit" % (
            ledger_entry["job_id"])
    if ledger_entry is not None and ledger_entry.get("state") == "submitting":
        return False, ("durable pending intent with no job id; a previous run "
                       "may have died mid-submit, reconcile against "
                       "batch.list_jobs before submitting anything")
    if not confirm:
        return False, "no --confirm; dry run, nothing submitted"
    if max_dollars is None:
        return False, "no --max-dollars; dry run, nothing submitted"
    if not (isinstance(max_dollars, (int, float))
            and math.isfinite(max_dollars) and max_dollars > 0):
        return False, "max-dollars %r is not a positive finite number" % (
            max_dollars,)
    if live is None or not (isinstance(live, (int, float))
                            and math.isfinite(live)) or live < 0:
        return False, "live quote unreadable"
    if live > max_dollars:
        return False, "live quote %.2f exceeds remaining budget %.2f" % (
            live, max_dollars)
    if planned is None:
        return False, ("no plan-time quote in the pricing cache, or the "
                       "cached one is older than %d days; re-run "
                       "databento_price.py plan with --refresh so drift is "
                       "measured against a current baseline"
                       % BASELINE_MAX_AGE_DAYS)
    if not (isinstance(planned, (int, float)) and math.isfinite(planned)) \
            or planned <= 0:
        return False, "plan-time quote %r is not a usable baseline" % (planned,)
    if live > planned * (1.0 + DRIFT_LIMIT):
        return False, "live quote %.2f drifted more than %d%% above the " \
            "plan-time quote %.2f" % (live, int(DRIFT_LIMIT * 100), planned)
    return True, "ok: %.2f within budget and within drift of %.2f" % (
        live, planned)


def submit_gated(scope, window, schema, confirm, max_dollars, ledger_entry,
                 post=http_json, before_post=None):
    """The ONE call site of batch.submit_job in this file, and the only
    function from which the string "batch.submit_job" is reachable. It
    re-prices fresh, asks submission_verdict, and refuses (raising, submitting
    nothing) unless the verdict is ok. Returns (job_dict, live_quote).

    `post` is injectable ONLY so the selftest can prove, without a network,
    that a failing verdict never reaches it; production callers do not pass
    it.

    `before_post` runs after EVERY refusal opportunity and immediately before
    the POST. mode_buy uses it to write the durable submission intent to the
    ledger: the charge is incurred by the POST, so the intent must be on disk
    first or a crash between the POST and the ledger write would leave the
    next run free to buy the same request again. A refusal therefore never
    writes an intent, and an intent with no job id means exactly "the POST's
    fate is unknown, reconcile"."""
    window_name = window[0]
    live = fresh_quote(scope, window, schema) if post is http_json else \
        ledger_entry_live_for_selftest(ledger_entry)
    planned = planned_quote(scope, window, schema) if post is http_json else \
        ledger_entry_planned_for_selftest(ledger_entry)
    ok, reason = submission_verdict(
        confirm, max_dollars, ledger_entry, live, planned)
    if not ok:
        raise Refusal("%s/%s/%s: %s" % (scope, window_name, schema, reason))
    symbols, stype_in = dp.SCOPES[scope]
    if stype_in == "raw_symbol":
        # The staged purchase is continuous-symbology by design; the
        # whole-book scope exists only for pricing comparison.
        raise Refusal("%s uses raw_symbol; refusing a whole-book purchase" % scope)
    start, end = dp.session_bounds_utc(window[1], window[2])
    # POST body per the SDK's submit_job(); see the ENCODING block comment for
    # the provenance of every key.
    data = dict(SUBMIT_DEFAULTS, **{
        "dataset": dp.DATASET,
        "start": start,
        "end": end,
        "symbols": symbols,
        "schema": schema,
        "stype_in": stype_in,
    })
    if before_post is not None:
        before_post(live)
    job = post("batch.submit_job", post_data=data)
    # Response shape: a job-info dict whose "id" and "state" keys are what
    # research/databento-ingest/src/databento_ingest/batch.py reads from the
    # real API (job["id"], job.get("state")).
    if not isinstance(job, dict) or not job.get("id"):
        raise Refusal("submit_job response carried no job id: %r" % (job,))
    return job, live


def ledger_entry_live_for_selftest(entry):
    return (entry or {}).get("_selftest_live")


def ledger_entry_planned_for_selftest(entry):
    return (entry or {}).get("_selftest_planned")


# A job our uncertain POST created was received by the vendor within seconds
# of the intent timestamp. The adoption window is generous for clock skew and
# processing delay but rules out any HISTORICAL job with the same market-data
# selection - the P1 case where an earlier identical purchase would otherwise
# be adopted as ours.
RECONCILE_WINDOW_BEFORE_S = 600
RECONCILE_WINDOW_AFTER_S = 3600

# A no-match listing is NOT proof the POST failed: vendor job visibility may
# lag the charge. A pending intent may be cleared for resubmission only after
# the full adoption window has elapsed - if the job were going to surface
# inside the window, clearing before the window closes could resubmit while
# it still might - AND enough no-match listings with VALID timestamps spaced
# a real interval apart have been recorded. Back-to-back listings seconds
# apart confirm nothing about lag.
RECONCILE_CLEAR_MIN_AGE_S = RECONCILE_WINDOW_AFTER_S
RECONCILE_CLEAR_CONFIRMATIONS = 2
RECONCILE_CONFIRM_SPACING_S = 300


def pending_clear_verdict(entry, now=None):
    """May a no-match pending intent be cleared and resubmitted? Pure
    decision, fixture-driven in the selftest. Returns (ok, reason).

    Immediate clearing on a single empty listing would let an ambiguous POST
    whose job has not surfaced yet be bought again seconds later. Clearing
    requires the intent to have aged past RECONCILE_CLEAR_MIN_AGE_S and at
    least RECONCILE_CLEAR_CONFIRMATIONS successful listings that each found
    no matching job."""
    now = now if now is not None else dt.datetime.now(dt.timezone.utc)
    stamp = _parse_ts(entry.get("intent_at"))
    if stamp is None:
        return False, "no readable intent_at; resolve the ledger by hand"
    age = (now - stamp).total_seconds()
    if age < RECONCILE_CLEAR_MIN_AGE_S:
        return False, ("intent is %ds old, inside the %ds reconciliation "
                       "delay; vendor listing may lag the charge" % (
                           age, RECONCILE_CLEAR_MIN_AGE_S))
    listings = [_parse_ts(x) for x in entry.get("no_match_listings") or []]
    listings = [x for x in listings if x is not None]
    if len(listings) < RECONCILE_CLEAR_CONFIRMATIONS:
        return False, ("%d of %d confirming no-match listings with valid "
                       "timestamps recorded; re-run to confirm" % (
                           len(listings), RECONCILE_CLEAR_CONFIRMATIONS))
    spread = (max(listings) - min(listings)).total_seconds()
    if spread < RECONCILE_CONFIRM_SPACING_S:
        return False, ("confirming listings span %ds, need %ds between "
                       "first and last; back-to-back listings confirm "
                       "nothing about vendor lag" % (
                           spread, RECONCILE_CONFIRM_SPACING_S))
    return True, ("aged %ds with %d confirming no-match listings spanning "
                  "%ds" % (age, len(listings), spread))

# The request-defining fields the submit body carries beyond the market-data
# selection. Any of these the vendor echoes must match; a selection-identical
# job with a different encoding or split is NOT our job.
REQUEST_FIELDS = ("encoding", "compression", "stype_in", "stype_out",
                  "split_duration", "split_symbols", "delivery",
                  "pretty_px", "pretty_ts", "map_symbols")


def _parse_ts(value):
    try:
        stamp = dt.datetime.fromisoformat(str(value))
    except (TypeError, ValueError):
        return None
    if stamp.tzinfo is None:
        stamp = stamp.replace(tzinfo=dt.timezone.utc)
    return stamp


def reconcile_pending(scope, window, schema, intent_at, fetch=http_json):
    """Find the vendor job a durable pending intent may have created.

    A pending intent means a previous run wrote its intent and then the POST's
    fate became unknown. The vendor listing is authoritative: a job that was
    created appears in batch.list_jobs, one that was not does not. Returns the
    single matching job dict, or None when no matching job exists (the intent
    is then safe to clear and resubmit).

    Adoption requires ALL of: the market-data selection (dataset, schema,
    symbols as a case-folded set, bounds), every request-defining field the
    vendor echoes (encoding, compression, symbology, splits, delivery), and a
    received timestamp inside the intent's submission window - a job created
    by our POST was received within seconds of the intent, so anything
    outside the window is a provably historical job with the same selection,
    ignored rather than adopted.

    Refuses on ambiguity, always closed, never open: more than one full
    match; symbols or bounds matching without the other; a selection match
    whose echoed request fields differ; or a full match whose received
    timestamp is missing or unreadable. Each of those is indistinguishable
    from the vendor echoing a field in a format this matcher does not
    understand, and clearing an intent on a format misunderstanding is how a
    job gets bought twice. Two residual risks, both accepted: a vendor that
    echoes BOTH symbols and bounds unrecognizably, and an echo that omits
    every request-defining field - fields are compared only where echoed, so
    a fieldless echo rests adoption on selection plus the intent window
    alone. The first armed run validates the echo formats in practice."""
    intent_stamp = _parse_ts(intent_at)
    if intent_stamp is None:
        raise Refusal("pending intent carries no readable intent_at "
                      "timestamp; resolve the ledger by hand")
    lo = intent_stamp - dt.timedelta(seconds=RECONCILE_WINDOW_BEFORE_S)
    hi = intent_stamp + dt.timedelta(seconds=RECONCILE_WINDOW_AFTER_S)
    params = dp.query(scope, window, schema)
    want_symbols = symbol_set(params["symbols"])
    listed = fetch("batch.list_jobs", params={
        "states": ",".join(VENDOR_STATES)})
    if not isinstance(listed, list):
        raise Refusal("batch.list_jobs returned %s during reconciliation" %
                      type(listed).__name__)
    exact, near = [], []
    for job in listed:
        if not (job.get("dataset") == params["dataset"]
                and job.get("schema") == params["schema"]):
            continue
        symbols_match = symbol_set(job.get("symbols")) == want_symbols
        bounds_match = (
            str(job.get("start", "")).startswith(params["start"])
            and str(job.get("end", "")).startswith(params["end"]))
        if not (symbols_match and bounds_match):
            if symbols_match or bounds_match:
                near.append(job)
            continue
        expected_fields = dict(SUBMIT_DEFAULTS, stype_in=params["stype_in"])
        fields_ok = all(
            str(job[field]).strip().lower()
            == str(expected_fields[field]).strip().lower()
            for field in REQUEST_FIELDS if field in job)
        if not fields_ok:
            near.append(job)
            continue
        received = _parse_ts(job.get("ts_received"))
        if received is None:
            # Explicit two-step: a PRESENT-but-null ts_received must still
            # fall back to ts_queued, which dict.get's default would not do.
            received = _parse_ts(job.get("ts_queued"))
        if received is None:
            # Full selection and field match but no readable timestamp:
            # cannot distinguish our job from a historical twin.
            near.append(job)
            continue
        if lo <= received <= hi:
            exact.append(job)
        # else: provably historical, ignored.
    if len(exact) > 1:
        raise Refusal("reconciliation found %d vendor jobs matching "
                      "%s/%s/%s inside the intent window; resolve the "
                      "ledger by hand" % (len(exact), scope, window[0],
                                          schema))
    if not exact and near:
        raise Refusal("reconciliation found %d dataset+schema job(s) that "
                      "half-match on symbols, bounds, request fields or "
                      "timestamp readability; the vendor may echo a field in "
                      "another format, resolve by hand rather than risk a "
                      "double buy" % len(near))
    return exact[0] if exact else None


def symbol_set(value):
    """Symbols as a case-folded set: list or comma string, any order, any
    spacing. A set compare cannot be fooled by echo formatting; only a
    genuinely different symbol population differs."""
    if isinstance(value, list):
        parts = [str(s) for s in value]
    else:
        parts = str(value or "").split(",")
    return frozenset(p.strip().upper() for p in parts if p.strip())


def sha256_file(path):
    hasher = hashlib.sha256()
    with open(path, "rb") as fh:
        while chunk := fh.read(CHUNK_SIZE):
            hasher.update(chunk)
    return hasher.hexdigest()


# ---------------------------------------------------------------------------
# Polling and file listing
# ---------------------------------------------------------------------------


def poll_job_state(job_id):
    """The job's current vendor state via batch.list_jobs. The "states"
    parameter and the list-of-dicts response are from the SDK's list_jobs();
    the "id"/"state" keys per databento-ingest's list_jobs()."""
    jobs = http_json("batch.list_jobs", params={
        "states": ",".join(VENDOR_STATES)})
    if not isinstance(jobs, list):
        raise Refusal("batch.list_jobs returned %s, expected a list" %
                      type(jobs).__name__)
    for job in jobs:
        if job.get("id") == job_id:
            return job.get("state", "unknown")
    return "missing"


def parse_file_manifest(entries):
    """Normalize a batch.list_files response. Shape per the SDK's _BatchJob:
    each entry carries "filename", "hash" ("sha256:<hex>"), "size" (int) and
    "urls" with an "https" key; the test manifest in
    tests/test_historical_batch.py shows the same four keys. Anything missing
    is a hard refusal, matching the SDK's own KeyError-to-error behavior."""
    if not isinstance(entries, list) or not entries:
        raise Refusal("batch.list_files returned no files")
    out = []
    seen = set()
    for entry in entries:
        try:
            filename = str(entry["filename"])
            hash_str = str(entry["hash"])
            size = int(entry["size"])
            https_url = entry["urls"]["https"]
        except (KeyError, TypeError, ValueError) as exc:
            raise Refusal("file manifest entry unparseable (%s): %r" % (
                exc, entry))
        # The filename is vendor input that becomes a path under the landing
        # directory. Anything but a plain basename - absolute, dotted, or
        # separator-bearing - could write outside it; a duplicate would
        # overwrite a sibling and collapse two files into one manifest entry.
        if (not filename or filename != os.path.basename(filename)
                or filename in (".", "..") or "\\" in filename):
            raise Refusal("manifest filename %r is not a plain basename; "
                          "refusing a path escape" % filename)
        if filename in seen:
            raise Refusal("manifest lists %r twice; downloads would "
                          "overwrite each other" % filename)
        seen.add(filename)
        # The fetcher attaches the Basic-auth header - which IS the key - to
        # whatever URL this entry supplies. A wrong or tampered manifest must
        # not be able to point that header anywhere else, and a file:// URL
        # would even be opened locally by urllib.
        split = urllib.parse.urlsplit(str(https_url))
        host = (split.hostname or "").lower()
        if split.scheme != "https" or not (
                host == "databento.com" or host.endswith(".databento.com")):
            raise Refusal("%s: url %r is not https on databento.com; the "
                          "auth header follows no other destination" % (
                              filename, https_url))
        algo, _, hex_digest = hash_str.partition(":")
        if algo != "sha256" or not hex_digest:
            raise Refusal("%s: unsupported hash %r, only sha256" % (
                filename, hash_str))
        out.append({"filename": filename, "size": size,
                    "sha256": hex_digest, "url": https_url})
    return out


def list_job_files(job_id):
    return parse_file_manifest(
        http_json("batch.list_files", params={"job_id": job_id}))


# ---------------------------------------------------------------------------
# Download engine
# ---------------------------------------------------------------------------
# The lifecycle of spec decision 6, per file: .downloading temp, streaming
# SHA-256, size check, atomic promote, Range resume with a 206-or-restart
# guard, bounded retries with exponential backoff. A hash or size mismatch
# deletes the temp and fails the file hard.


def urllib_fetch(url, offset):
    """Open a streaming GET, optionally ranged. Returns (status, reader)
    where reader has .read(n) and .close(). The production fetcher; the
    selftest substitutes fixture fetchers with the same signature."""
    headers = {"Authorization": _auth_header()}
    if offset > 0:
        headers["Range"] = "bytes=%d-" % offset
    req = urllib.request.Request(url, headers=headers)
    resp = _OPENER.open(req, timeout=HTTP_TIMEOUT)
    return resp.status, resp


def check_zstd_prefix(path):
    """Validate that the file starts a real zstd frame: magic bytes, then the
    stdlib streaming decompressor over the first chunk, which raises ZstdError
    on a malformed header. Cheap delivery-format check, not a full decode."""
    with open(path, "rb") as fh:
        prefix = fh.read(CHUNK_SIZE)
    if not prefix.startswith(ZSTD_MAGIC):
        return "no zstd magic"
    try:
        # max_length bounds the OUTPUT: the frame header is still fully
        # parsed and a malformed one still raises, but a pathological
        # high-ratio frame cannot balloon a 4 MB prefix into gigabytes.
        zstd.ZstdDecompressor().decompress(prefix, 1024)
    except zstd.ZstdError as exc:
        return "zstd frame invalid: %s" % exc
    return None


def download_file(fetch, url, dest_path, expected_size, expected_sha256,
                  retries=MAX_RETRIES, sleep=time.sleep):
    """Download one file through the full lifecycle. Returns (ok, detail);
    on success detail is the verified sha256 hex, on failure the named
    reason. Never leaves a corrupt final file: the promote happens only after
    size, hash and zstd checks all pass."""
    tmp_path = dest_path + ".downloading"
    if os.path.exists(dest_path):
        # A promoted file is a CLAIM of past verification, not proof: size
        # alone would accept same-length corruption or replacement forever.
        # Re-prove the bytes; anything short of a full hash match is
        # discarded and re-downloaded.
        if (os.path.getsize(dest_path) == expected_size
                and sha256_file(dest_path) == expected_sha256):
            return True, expected_sha256
        os.remove(dest_path)
    last_error = "not attempted"
    for attempt in range(retries):
        if attempt > 0:
            sleep(RETRY_DELAY_BASE * (2 ** (attempt - 1)))
        hasher = hashlib.sha256()
        offset = 0
        try:
            if os.path.exists(tmp_path):
                existing = os.path.getsize(tmp_path)
                if existing == expected_size:
                    # A complete temp from an interrupted run: hash it and
                    # promote for free rather than re-downloading the bytes.
                    if sha256_file(tmp_path) == expected_sha256:
                        if dest_path.endswith(".zst"):
                            zstd_error = check_zstd_prefix(tmp_path)
                            if zstd_error:
                                # The hash proves these ARE the vendor's
                                # bytes; a refetch would fetch identical
                                # bytes and fail identically. Fail now.
                                os.remove(tmp_path)
                                return False, ("vendor bytes hash-match but "
                                               "%s; a refetch would repeat "
                                               "them" % zstd_error)
                            os.replace(tmp_path, dest_path)
                            return True, expected_sha256
                        os.replace(tmp_path, dest_path)
                        return True, expected_sha256
                    os.remove(tmp_path)
                elif 0 < existing < expected_size:
                    with open(tmp_path, "rb") as fh:
                        while chunk := fh.read(CHUNK_SIZE):
                            hasher.update(chunk)
                    offset = existing
                else:
                    os.remove(tmp_path)
            status, reader = fetch(url, offset)
            try:
                if offset > 0 and status != 206:
                    # Server ignored the Range header; restart from zero
                    # rather than appending a full body to a partial file.
                    hasher = hashlib.sha256()
                    offset = 0
                with open(tmp_path, "ab" if offset > 0 else "wb") as fh:
                    while chunk := reader.read(CHUNK_SIZE):
                        fh.write(chunk)
                        hasher.update(chunk)
            finally:
                reader.close()
            actual_size = os.path.getsize(tmp_path)
            if actual_size != expected_size:
                os.remove(tmp_path)
                last_error = "size mismatch: expected %d, got %d" % (
                    expected_size, actual_size)
                continue
            digest = hasher.hexdigest()
            if digest != expected_sha256:
                os.remove(tmp_path)
                last_error = "sha256 mismatch: expected %s..., got %s..." % (
                    expected_sha256[:16], digest[:16])
                continue
            if dest_path.endswith(".zst"):
                zstd_error = check_zstd_prefix(tmp_path)
                if zstd_error:
                    # Hash already matched, so these ARE the vendor's bytes
                    # and a retry would download and reject them again.
                    os.remove(tmp_path)
                    return False, ("vendor bytes hash-match but %s; a "
                                   "refetch would repeat them" % zstd_error)
            os.replace(tmp_path, dest_path)
            return True, digest
        except urllib.error.HTTPError as exc:
            # 4xx is a CLIENT error - bad auth, expired job URL - and five
            # retries over 150 seconds of backoff cannot fix it. Fail the
            # file immediately; retrying is reserved for the transient
            # classes below plus 408, 429 and 5xx.
            if exc.code in (408, 429) or exc.code >= 500:
                last_error = "HTTP %d %s" % (exc.code, exc.reason)
                continue
            # The partial temp is KEPT: vendor download URLs expire, and the
            # bytes already fetched are still valid content a re-listed URL
            # can resume - discarding 90 percent of a multi-gigabyte file
            # over an expired link would waste exactly what resume exists
            # to save. A stale partial can never promote wrongly: promotion
            # requires the full size and hash.
            return False, "HTTP %d %s: not retryable" % (exc.code, exc.reason)
        except (urllib.error.URLError, ConnectionError, TimeoutError,
                OSError) as exc:
            # Transient: keep the temp for resume, retry with backoff.
            last_error = "%s: %s" % (type(exc).__name__, exc)
    # The partial temp survives exhaustion for the same reason as the
    # non-retryable 4xx path above: the bytes remain valid content the next
    # run can resume, and promotion is gated on full size plus hash, so a
    # stale partial can never be promoted wrongly.
    return False, "failed after %d attempts: %s" % (retries, last_error)


def download_job_files(files, dest_dir, fetch=urllib_fetch,
                       retries=MAX_RETRIES, sleep=time.sleep):
    """Download every file of a job. Returns (verified, failures) where
    verified maps filename to sha256 and failures is the named list. The
    caller fails the run on ANY failure: partial success is not success."""
    os.makedirs(dest_dir, exist_ok=True)
    verified, failures = {}, []
    for info in files:
        dest = os.path.join(dest_dir, info["filename"])
        ok, detail = download_file(
            fetch, info["url"], dest, info["size"], info["sha256"],
            retries=retries, sleep=sleep)
        if ok:
            print("  OK   %s (%.2f MB)" % (info["filename"], info["size"] / 1e6))
            verified[info["filename"]] = detail
        else:
            print("  FAIL %s: %s" % (info["filename"], detail))
            failures.append("%s: %s" % (info["filename"], detail))
    return verified, failures


def landing_dir(scope, window_name, schema):
    return os.path.join(LANDING_ROOT, scope, "%s.%s" % (window_name, schema))


def verify_landing(dest_dir, files):
    """Re-hash previously landed files against their recorded digests.
    Returns the named failure list, empty when everything proves out. A
    ledger state of `downloaded` is a claim; the bytes are re-proven on
    every run, because a skipped check would leave same-length corruption
    or replacement marked verified forever."""
    if not files:
        return ["no files recorded in ledger entry"]
    bad = []
    for filename, digest in sorted(files.items()):
        path = os.path.join(dest_dir, filename)
        if not os.path.exists(path):
            bad.append("%s: missing from landing dir" % filename)
        elif sha256_file(path) != digest:
            bad.append("%s: sha256 mismatch on disk" % filename)
    return bad


def write_manifest(dest_dir, entry):
    """The per-directory provenance record. The committed record is the
    ledger; this one travels with the (gitignored) bytes. Durable like the
    ledger, and for the same reason."""
    durable_json_write(os.path.join(dest_dir, "manifest.json"),
                       dict(entry, tool_version=TOOL_VERSION))


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------


# buy is whitelisted to the staged 9.6 purchase and nothing else. The pricing
# tool exposes every scope/plan combination for COMPARISON, including the
# superseded regime-selected baskets; none of those may be bought, and the
# contiguous stage stays locked until the paired test - the asymmetric stop
# condition - has landed AND been affirmatively judged.
AUTHORIZED_BUYS = {
    ("pairv", "paircurrent"): None,
    ("nqv", "contiguous"): ("pairv", "2026-07.2wk", "trades"),
}

# The affirmative analysis decision that unlocks stage two. This tool only
# READS it; it is written by the analysis that judges the paired test, after
# a human has read that result. Delivery is not judgment: a downloaded pair
# whose proxy test FAILED must keep the 71.79-dollar second stage locked, so
# "downloaded" alone never unlocks anything. The verdict binds to the exact
# job id and the exact delivered file hashes, so a verdict written against
# one delivery cannot authorize a different one.
PAIR_VERDICT_FILE = os.path.join(
    ROOT, "analysis", "databento-pair-verdict.json")


def authorize_buy(scope, variant, jobs, verdict_path=PAIR_VERDICT_FILE):
    """Refuse any buy outside the staged design; refuse a stage whose
    prerequisite has not been downloaded AND affirmatively judged. Every
    branch fails closed with a named reason."""
    prereq = AUTHORIZED_BUYS.get((scope, variant), "unlisted")
    if prereq == "unlisted":
        allowed = ", ".join("%s/%s" % pair for pair in sorted(AUTHORIZED_BUYS))
        raise Refusal("buy is whitelisted to the staged purchase (%s); "
                      "%s/%s is not in it" % (allowed, scope, variant))
    if prereq is None:
        return
    key = ledger_key(*prereq)
    entry = jobs.get(key)
    if not entry or entry.get("state") != "downloaded":
        raise Refusal("stage locked: %s/%s requires the paired test %s to "
                      "be downloaded first" % (scope, variant, key))
    if not os.path.exists(verdict_path):
        raise Refusal("stage locked: no pair verdict at %s; the paired test "
                      "must be ANALYZED and affirmatively passed, delivery "
                      "alone unlocks nothing" % verdict_path)
    try:
        verdict = json.loads(open(verdict_path).read())
    except (json.JSONDecodeError, OSError) as exc:
        raise Refusal("stage locked: pair verdict unreadable (%s)" % exc)
    if verdict.get("verdict") != "pass":
        raise Refusal("stage locked: pair verdict is %r, not the required "
                      "affirmative pass" % verdict.get("verdict"))
    if verdict.get("job_id") != entry.get("job_id"):
        raise Refusal("stage locked: pair verdict names job %r but the "
                      "ledger's paired test is job %r" % (
                          verdict.get("job_id"), entry.get("job_id")))
    if verdict.get("files") != entry.get("files"):
        raise Refusal("stage locked: pair verdict's file hashes do not match "
                      "the delivered files; the verdict must be re-issued "
                      "against the bytes actually analyzed")


def plan_prior_spend(jobs, scope, variant):
    """Dollars already committed to THIS plan's entries, submitted or
    pending. --max-dollars caps CUMULATIVE plan spend: a run that submitted
    one job and crashed must not grant the rerun the full cap again for the
    remaining jobs."""
    total = 0.0
    for window, schema in resolve_plan(scope, variant):
        entry = jobs.get(ledger_key(scope, window[0], schema))
        if not entry:
            continue
        spent = entry.get("live_quote_at_submit")
        if spent is None:
            spent = entry.get("live_quote_at_intent")
        if isinstance(spent, (int, float)) and math.isfinite(spent):
            total += spent
    return total


def resolve_plan(scope, variant):
    if scope not in dp.SCOPES:
        raise Refusal("unknown scope %s; known: %s" % (
            scope, ", ".join(sorted(dp.SCOPES))))
    if variant not in dp.PLANS:
        raise Refusal("unknown plan %s; known: %s" % (
            variant, ", ".join(sorted(dp.PLANS))))
    by_name = {w[0]: w for w in dp.WINDOWS}
    return [(by_name[name], schema) for name, schema in dp.PLANS[variant]]


def print_review(scope, variant, confirm, max_dollars, jobs=None):
    """The would-submit table: fresh quote, plan-time baseline, drift, and
    the verdict each row would receive. Shared by plan (always) and buy
    (before acting). Returns the rows for buy to act on.

    Quotes are fetched ONLY for rows eligible for a new submission. An entry
    that already carries a job id, or a pending intent, needs no quote to be
    polled, reconciled, downloaded or re-verified - so a metadata outage
    must not block recovery of already-paid data. A per-row quote failure
    likewise marks that row refused rather than aborting the run.

    Returns (rows, quote_failures): the rows for buy to act on, and how many
    eligible rows could not be quoted, so plan mode can exit nonzero instead
    of presenting an outage as a clean review."""
    if jobs is None:
        jobs = load_ledger()
    rows = []
    quote_failures = 0
    remaining = max_dollars
    print("scope: %s   plan: %s   %s" % (
        scope, variant,
        "ARMED (confirm + max-dollars)" if confirm and max_dollars is not None
        else "DRY RUN"))
    print("%-16s %-10s %10s %10s %8s  %s" % (
        "window", "schema", "live", "planned", "drift", "verdict"))
    for window, schema in resolve_plan(scope, variant):
        key = ledger_key(scope, window[0], schema)
        entry = jobs.get(key)
        if entry is not None and (entry.get("job_id")
                                  or entry.get("state") == "submitting"):
            status = ("job %s, %s" % (entry["job_id"],
                                      entry.get("state", "?"))
                      if entry.get("job_id") else "pending intent")
            print("%-16s %-10s %10s %10s %8s  owned: %s" % (
                window[0], schema, "-", "-", "-", status))
            rows.append((window, schema, key, entry, False))
            continue
        try:
            live = fresh_quote(scope, window, schema)
        except Refusal as exc:
            print("%-16s %-10s %10s %10s %8s  %s" % (
                window[0], schema, "n/a", "-", "-", exc))
            rows.append((window, schema, key, entry, False))
            quote_failures += 1
            continue
        planned = planned_quote(scope, window, schema)
        ok, reason = submission_verdict(
            confirm, remaining, entry, live, planned)
        if ok and remaining is not None:
            remaining -= live
        drift = ("%+.1f%%" % ((live / planned - 1.0) * 100)
                 if planned else "n/a")
        print("%-16s %-10s %10.2f %10s %8s  %s" % (
            window[0], schema, live,
            "n/a" if planned is None else "%.2f" % planned, drift, reason))
        rows.append((window, schema, key, entry, ok))
    return rows, quote_failures


def mode_plan(args):
    _rows, quote_failures = print_review(
        args.scope, args.plan, confirm=False, max_dollars=None)
    print("\nnothing submitted; this mode never submits")
    if quote_failures:
        print("%d row(s) could not be quoted; this review is incomplete" %
              quote_failures)
        raise SystemExit(1)


def mode_status(args):
    jobs = load_ledger()
    if not jobs:
        print("ledger empty (%s)" % LEDGER_FILE)
        return
    print("%s: %d entr%s" % (LEDGER_FILE, len(jobs),
                             "y" if len(jobs) == 1 else "ies"))
    total = 0.0
    for key in sorted(jobs):
        entry = jobs[key]
        cost = entry.get("live_quote_at_submit")
        if isinstance(cost, (int, float)):
            total += cost
        print("  %-40s %-12s job=%s  %s  %s" % (
            key, entry.get("state", "?"), entry.get("job_id", "-"),
            "n/a" if cost is None else "%.2f" % cost,
            entry.get("submitted_at", "")))
    print("total submitted spend: %.2f" % total)


def mode_buy(args):
    """The lifecycle driver, in phases, so money-touching work is
    all-or-nothing:

    1. AUTHORIZE: only the staged 9.6 plans; the contiguous stage stays
       locked until the paired test has landed.
    2. RECONCILE pending intents (armed runs), with the no-match delay: an
       empty listing is not proof the POST failed, so clearing requires age
       plus repeated confirmations.
    3. REVIEW: quotes and verdicts for submission-eligible rows, with
       --max-dollars capping CUMULATIVE plan spend across runs.
    4. SUBMIT, armed runs only, all-or-nothing: every eligible row must pass
       preflight before the FIRST post, and the first failure or unknown
       outcome stops all further submissions - a four-month plan must never
       buy months 1, 3 and 4 around a failed month 2.
    5. SETTLE: poll, download and re-verify every owned entry.

    Exit codes: 0 settled, 3 nonterminal (pending or undelivered), 1 any
    failure."""
    # The lock covers the ENTIRE lifecycle - ledger load, verdicts, intent
    # writes, POSTs, response persistence - and is held until process exit.
    buy_lock = acquire_buy_lock()  # noqa: F841  kept alive on purpose
    jobs = load_ledger()
    authorize_buy(args.scope, args.plan, jobs)
    armed = args.confirm and args.max_dollars is not None
    failures = []
    waiting = []

    # Phase 2: reconcile pending intents before anything is priced.
    for window, schema in resolve_plan(args.scope, args.plan):
        key = ledger_key(args.scope, window[0], schema)
        entry = jobs.get(key)
        if not (entry and entry.get("state") == "submitting"
                and not entry.get("job_id")):
            continue
        print("== %s: pending intent" % key)
        if not armed:
            print("  run armed to reconcile")
            waiting.append("%s: pending intent unreconciled" % key)
            continue
        try:
            match = reconcile_pending(args.scope, window, schema,
                                      entry.get("intent_at"))
        except Refusal as exc:
            print("  %s" % exc)
            failures.append("%s: reconciliation refused" % key)
            continue
        if match is not None:
            entry.update({
                "job_id": match["id"],
                "state": match.get("state", "queued"),
                "reconciled_at": dt.datetime.now(dt.timezone.utc).isoformat(),
                # The intent's quote stands in for the submit-time quote so
                # mode_status's spend total stays honest: this job WAS
                # bought, at approximately this price.
                "live_quote_at_submit": entry.get("live_quote_at_intent"),
            })
            jobs[key] = entry
            save_ledger(jobs)
            print("  adopted vendor job %s from pending intent" % match["id"])
            continue
        # No match THIS listing. Record the confirmation and clear only when
        # the pure verdict says the intent has aged past vendor-lag doubt.
        entry.setdefault("no_match_listings", []).append(
            dt.datetime.now(dt.timezone.utc).isoformat())
        save_ledger(jobs)
        ok, reason = pending_clear_verdict(entry)
        if ok:
            del jobs[key]
            save_ledger(jobs)
            print("  pending intent cleared for resubmission: %s" % reason)
        else:
            print("  intent stays pending: %s" % reason)
            waiting.append("%s: pending, %s" % (key, reason))

    # Phase 3: review, with the cap reduced by spend already committed to
    # this plan in earlier runs - submitted or still pending.
    cap = args.max_dollars
    if cap is not None:
        prior = plan_prior_spend(jobs, args.scope, args.plan)
        if prior:
            print("\nprior committed spend for this plan: %.2f; "
                  "cap %.2f leaves %.2f" % (prior, cap, cap - prior))
        cap -= prior
    print()
    rows, _quote_failures = print_review(
        args.scope, args.plan, args.confirm, cap, jobs)

    def is_eligible(key):
        entry = jobs.get(key)
        return entry is None or (not entry.get("job_id")
                                 and entry.get("state") != "submitting")

    eligible = [(w, s, k, ok) for w, s, k, _e, ok in rows if is_eligible(k)]

    # Phase 4: all-or-nothing submission.
    if eligible and not armed:
        for _w, _s, key, ok in eligible:
            print("dry run: would %ssubmit %s" % ("" if ok else "NOT ", key))
    elif eligible and armed:
        preflight_failed = [key for _w, _s, key, ok in eligible if not ok]
        if preflight_failed:
            print("\nPREFLIGHT FAILED; nothing submitted, the plan submits "
                  "all rows or none:")
            for key in preflight_failed:
                failures.append("%s: preflight failed" % key)
                print("  %s" % key)
        else:
            halted = False
            remaining = cap
            for window, schema, key, _ok in eligible:
                print("\n== %s" % key)
                if halted:
                    print("  skipped: an earlier submission failed and the "
                          "plan stops at the first failure")
                    failures.append("%s: skipped after earlier failure" % key)
                    continue

                def write_intent(live, _key=key, _window=window,
                                 _schema=schema):
                    # Durable BEFORE the POST: see submit_gated's
                    # before_post note. Runs only after every refusal
                    # opportunity has passed.
                    jobs[_key] = {
                        "state": "submitting",
                        "intent_at":
                            dt.datetime.now(dt.timezone.utc).isoformat(),
                        "scope": args.scope,
                        "window": _window[0],
                        "schema": _schema,
                        "live_quote_at_intent": live,
                    }
                    save_ledger(jobs)

                try:
                    # ledger_entry None by construction: eligibility was
                    # checked against the live ledger above, and the intent
                    # this run writes must not refuse its own POST.
                    job, live = submit_gated(
                        args.scope, window, schema, True, remaining,
                        None, before_post=write_intent)
                except Refusal as exc:
                    # A verdict refusal happens BEFORE the intent write. An
                    # intent that exists here means the POST itself failed
                    # with unknown fate; it stays for reconciliation. Either
                    # way, no further row is submitted.
                    print("  %s" % exc)
                    failures.append("%s: not submitted" % key)
                    halted = True
                    continue
                remaining -= live
                jobs[key] = {
                    "job_id": job["id"],
                    "state": job.get("state", "queued"),
                    "submitted_at":
                        dt.datetime.now(dt.timezone.utc).isoformat(),
                    "live_quote_at_submit": live,
                    "planned_quote": planned_quote(args.scope, window,
                                                   schema),
                    "scope": args.scope,
                    "window": window[0],
                    "schema": schema,
                    "encoding": ENCODING,
                    "compression": COMPRESSION,
                    "split_duration": SPLIT_DURATION,
                }
                save_ledger(jobs)
                print("  submitted job %s at %.2f" % (job["id"], live))

    # Phase 5: settle every entry that owns a vendor job.
    for window, schema in resolve_plan(args.scope, args.plan):
        key = ledger_key(args.scope, window[0], schema)
        entry = jobs.get(key)
        if not entry or not entry.get("job_id"):
            continue
        print("\n== %s" % key)
        if entry.get("state") == "downloaded":
            dest = landing_dir(args.scope, window[0], schema)
            bad = verify_landing(dest, entry.get("files") or {})
            if bad:
                print("  landed files FAIL re-verification:")
                for line in bad:
                    print("    %s" % line)
                failures.extend("%s/%s" % (key, line) for line in bad)
            else:
                print("  already downloaded; %d file(s) re-verified by hash" %
                      len(entry["files"]))
            continue
        state = poll_job_state(entry["job_id"])
        entry["state"] = state
        save_ledger(jobs)
        print("  job %s state: %s" % (entry["job_id"], state))
        if state == "expired":
            failures.append("%s: job expired before download" % key)
            continue
        if state == "missing":
            # A ledger-known job id the vendor listing no longer contains,
            # e.g. expired and aged out. "Re-run later" would loop forever.
            failures.append("%s: job %s missing from the vendor listing; "
                            "resolve the ledger by hand" % (
                                key, entry["job_id"]))
            continue
        if state != "done":
            print("  not yet deliverable (%s); re-run buy to poll" % state)
            waiting.append("%s: %s" % (key, state))
            continue
        files = list_job_files(entry["job_id"])
        dest = landing_dir(args.scope, window[0], schema)
        verified, file_failures = download_job_files(files, dest)
        if file_failures:
            failures.extend("%s/%s" % (key, f) for f in file_failures)
            continue
        entry["state"] = "downloaded"
        entry["files"] = verified
        # Manifest BEFORE the ledger state: a crash between the two then
        # leaves a done-state entry that re-verifies and re-promotes next
        # run, rather than a downloaded-state entry whose promised
        # manifest.json never got written.
        write_manifest(dest, entry)
        save_ledger(jobs)
        print("  %d file(s) verified into %s" % (len(verified), dest))

    print()
    if failures:
        print("FAILED: %d item(s) incomplete" % len(failures))
        for line in failures:
            print("   ", line)
        raise SystemExit(1)
    if waiting:
        # A distinct NONTERMINAL exit: nothing failed, but claiming "settled"
        # here would let orchestration treat an undelivered purchase as
        # finished. Exit 3 means exactly "come back and poll".
        print("WAITING: %d item(s) nonterminal; re-run buy to continue" %
              len(waiting))
        for line in waiting:
            print("   ", line)
        raise SystemExit(3)
    print("all plan entries settled")


# ---------------------------------------------------------------------------
# Selftest: fixtures only, no network
# ---------------------------------------------------------------------------


def selftest():
    checks = []

    def check(name, condition):
        checks.append((name, bool(condition)))
        print("  %s %s" % ("ok  " if condition else "FAIL", name))

    print("double gate: submission_verdict against fixture argument sets")
    submitted = {"job_id": "GLBX-20260805-TEST"}
    for name, confirm, cap, entry, live, planned, expect in [
        ("plain invocation refuses", False, None, None, 1.0, 1.0, False),
        ("confirm alone refuses", True, None, None, 1.0, 1.0, False),
        ("max-dollars alone refuses", False, 10.0, None, 1.0, 1.0, False),
        ("both flags submit", True, 10.0, None, 1.0, 1.0, True),
        ("over budget refuses", True, 0.5, None, 1.0, 1.0, False),
        ("10% drift boundary passes", True, 10.0, None, 1.10, 1.0, True),
        ("over 10% drift refuses", True, 10.0, None, 1.11, 1.0, False),
        ("no baseline refuses", True, 10.0, None, 1.0, None, False),
        ("zero baseline refuses", True, 10.0, None, 1.0, 0.0, False),
        ("unreadable live refuses", True, 10.0, None, None, 1.0, False),
        ("keyed job never resubmits", True, 10.0, submitted, 1.0, 1.0, False),
        ("keyed job blocks even armed", True, 1000.0, submitted, 0.01, 0.01,
         False),
        ("pending intent refuses", True, 10.0, {"state": "submitting"},
         1.0, 1.0, False),
        ("nan cap refuses", True, float("nan"), None, 1.0, 1.0, False),
        ("inf cap refuses", True, float("inf"), None, 1.0, 1.0, False),
        ("zero cap refuses", True, 0.0, None, 0.0, 1.0, False),
        ("negative cap refuses", True, -5.0, None, 1.0, 1.0, False),
        ("nan live refuses", True, 10.0, None, float("nan"), 1.0, False),
        ("nan baseline refuses", True, 10.0, None, 1.0, float("nan"), False),
        ("inf baseline refuses", True, 10.0, None, 1.0, float("inf"), False),
    ]:
        ok, _reason = submission_verdict(confirm, cap, entry, live, planned)
        check(name, ok == expect)

    print("submit_gated never reaches the post on a failing verdict")
    calls = []

    def spy_post(endpoint, params=None, post_data=None):
        calls.append(endpoint)
        return {"id": "GLBX-FIXTURE", "state": "queued"}

    window = dp.WINDOWS[0]
    entry = {"_selftest_live": 1.0, "_selftest_planned": 1.0}
    try:
        submit_gated("pairv", window, "trades", False, None, entry,
                     post=spy_post)
        gated = False
    except Refusal:
        gated = True
    check("unarmed call refused before post", gated and not calls)
    job, live = submit_gated("pairv", window, "trades", True, 10.0, entry,
                             post=spy_post)
    check("armed call posts exactly once", calls == ["batch.submit_job"])
    check("job id surfaced", job["id"] == "GLBX-FIXTURE" and live == 1.0)

    sequence = []

    def seq_post(endpoint, params=None, post_data=None):
        sequence.append("post")
        return {"id": "GLBX-FIXTURE", "state": "queued"}

    submit_gated("pairv", window, "trades", True, 10.0, entry,
                 post=seq_post, before_post=lambda live: sequence.append(
                     "intent"))
    check("intent is written after the verdict and before the post",
          sequence == ["intent", "post"])
    refused_sequence = []
    try:
        submit_gated("pairv", window, "trades", False, None, entry,
                     post=seq_post,
                     before_post=lambda live: refused_sequence.append(
                         "intent"))
    except Refusal:
        pass
    check("a refusal never writes an intent", refused_sequence == [])
    try:
        submit_gated("book", window, "trades", True, 10.0, entry,
                     post=spy_post)
        raw_refused = False
    except Refusal:
        raw_refused = True
    check("raw_symbol scope refused", raw_refused and len(calls) == 1)

    print("ledger: round trip, idempotency, unreadable refusal")
    if os.path.exists(SELFTEST_DIR):
        shutil.rmtree(SELFTEST_DIR)
    os.makedirs(SELFTEST_DIR)
    ledger_path = os.path.join(SELFTEST_DIR, "jobs.json")
    key = ledger_key("pairv", "2026-07.2wk", "trades")
    save_ledger({key: {"job_id": "GLBX-A", "state": "queued"}}, ledger_path)
    loaded = load_ledger(ledger_path)
    check("round trip preserves entry", loaded[key]["job_id"] == "GLBX-A")
    ok, reason = submission_verdict(True, 1000.0, loaded[key], 1.0, 1.0)
    check("reloaded keyed job never resubmits",
          not ok and "GLBX-A" in reason)
    with open(ledger_path, "w") as fh:
        fh.write("{ not json")
    try:
        load_ledger(ledger_path)
        refused = False
    except Refusal:
        refused = True
    check("unparseable ledger is fatal, not discarded", refused)
    check("missing ledger is empty, not fatal",
          load_ledger(os.path.join(SELFTEST_DIR, "absent.json")) == {})

    print("submit body carries the SDK's exact field set")
    body_keys = {"dataset", "start", "end", "symbols", "schema", "stype_in",
                 "stype_out", "encoding", "compression", "pretty_px",
                 "pretty_ts", "map_symbols", "split_symbols",
                 "split_duration", "delivery"}
    captured = {}

    def capture_post(endpoint, params=None, post_data=None):
        captured.update(post_data)
        return {"id": "GLBX-FIXTURE", "state": "queued"}

    submit_gated("pairv", window, "trades", True, 10.0, entry,
                 post=capture_post)
    check("field set matches test_batch_submit_job_sends_expected_request "
          "minus split_size", set(captured) == body_keys)
    check("csv zstd month continuous", (
        captured["encoding"] == "csv" and captured["compression"] == "zstd"
        and captured["split_duration"] == "month"
        and captured["stype_in"] == "continuous"))
    check("bounds are session UTC instants",
          captured["start"].endswith(":00") and "T" in captured["end"])

    print("file manifest parsing, shape from tests/test_historical_batch.py")
    payload = b"fixture file content, compressed in spirit only"
    sha = hashlib.sha256(payload).hexdigest()
    manifest = [{
        "filename": "glbx-mdp3-20260706.trades.csv.zst",
        "hash": "sha256:%s" % sha,
        "size": len(payload),
        "urls": {"https": "https://hist.databento.com/f.csv.zst"},
    }]
    files = parse_file_manifest(manifest)
    check("manifest normalized", files[0]["sha256"] == sha
          and files[0]["size"] == len(payload))
    try:
        parse_file_manifest([{"filename": "x", "hash": "md5:aa", "size": 1,
                              "urls": {"https": "u"}}])
        refused = False
    except Refusal:
        refused = True
    check("non-sha256 hash refused", refused)
    try:
        parse_file_manifest([{"filename": "x", "hash": "sha256:aa",
                              "size": 1, "urls": {}}])
        refused = False
    except Refusal:
        refused = True
    check("missing https url refused", refused)
    for label, name in [("traversal", "../escape.csv"),
                        ("absolute", "/etc/passwd"),
                        ("backslash", "..\\escape.csv"),
                        ("dot", ".")]:
        try:
            parse_file_manifest([dict(manifest[0], filename=name)])
            refused = False
        except Refusal:
            refused = True
        check("%s filename refused" % label, refused)
    try:
        parse_file_manifest([manifest[0], dict(manifest[0])])
        refused = False
    except Refusal:
        refused = True
    check("duplicate filename refused", refused)
    for label, url in [("http scheme", "http://hist.databento.com/f"),
                       ("foreign host", "https://evil.example.com/f"),
                       ("suffix spoof", "https://notdatabento.com/f"),
                       ("file scheme", "file:///etc/passwd")]:
        try:
            parse_file_manifest([dict(manifest[0], urls={"https": url})])
            refused = False
        except Refusal:
            refused = True
        check("%s url refused, key follows nothing else" % label, refused)

    print("pending-intent reconciliation against fixture job listings")
    params = dp.query("pairv", window, "trades")
    intent_at = "2026-08-05T12:00:00+00:00"
    vendor_job = {
        "id": "GLBX-RECOVERED", "state": "done",
        "dataset": params["dataset"], "schema": params["schema"],
        "symbols": params["symbols"],
        "start": params["start"] + ".000000000Z",
        "end": params["end"] + ".000000000Z",
        "ts_received": "2026-08-05T12:00:31+00:00",
        "encoding": "csv", "compression": "zstd",
        "stype_in": "continuous", "delivery": "download",
    }
    other = dict(vendor_job, id="GLBX-OTHER", schema="tbbo")
    match = reconcile_pending("pairv", window, "trades", intent_at,
                              fetch=lambda *a, **k: [other, vendor_job])
    check("intent adopts the one matching vendor job",
          match is not None and match["id"] == "GLBX-RECOVERED")
    match = reconcile_pending("pairv", window, "trades", intent_at,
                              fetch=lambda *a, **k: [other])
    check("no matching job returns None for the clear-delay gate to judge",
          match is None)
    scrambled = dict(vendor_job, symbols=[
        s.strip().lower() for s in
        reversed(params["symbols"].split(","))])
    match = reconcile_pending("pairv", window, "trades", intent_at,
                              fetch=lambda *a, **k: [scrambled])
    check("reordered lowercase list symbols still adopt",
          match is not None and match["id"] == "GLBX-RECOVERED")
    odd_symbols = dict(vendor_job, symbols="NQU6,MNQU6")
    try:
        reconcile_pending("pairv", window, "trades", intent_at,
                          fetch=lambda *a, **k: [odd_symbols])
        refused = False
    except Refusal:
        refused = True
    check("symbols-format mismatch with matching bounds refuses, "
          "never clears", refused)
    try:
        reconcile_pending("pairv", window, "trades", intent_at,
                          fetch=lambda *a, **k: [vendor_job,
                                                 dict(vendor_job)])
        refused = False
    except Refusal:
        refused = True
    check("two exact matches refuse", refused)
    odd_bounds = dict(vendor_job, start="1780000000000000000")
    try:
        reconcile_pending("pairv", window, "trades", intent_at,
                          fetch=lambda *a, **k: [odd_bounds])
        refused = False
    except Refusal:
        refused = True
    check("bounds-format mismatch refuses rather than clears", refused)
    historical = dict(vendor_job, id="GLBX-OLD",
                      ts_received="2026-06-01T12:00:00+00:00")
    match = reconcile_pending("pairv", window, "trades", intent_at,
                              fetch=lambda *a, **k: [historical])
    check("an identical job outside the intent window is ignored, "
          "not adopted", match is None)
    match = reconcile_pending("pairv", window, "trades", intent_at,
                              fetch=lambda *a, **k: [historical, vendor_job])
    check("the in-window job is chosen over its historical twin",
          match is not None and match["id"] == "GLBX-RECOVERED")
    no_ts = {k: v for k, v in vendor_job.items() if k != "ts_received"}
    try:
        reconcile_pending("pairv", window, "trades", intent_at,
                          fetch=lambda *a, **k: [no_ts])
        refused = False
    except Refusal:
        refused = True
    check("a full match without a readable timestamp refuses", refused)
    odd_encoding = dict(vendor_job, encoding="dbn")
    try:
        reconcile_pending("pairv", window, "trades", intent_at,
                          fetch=lambda *a, **k: [odd_encoding])
        refused = False
    except Refusal:
        refused = True
    check("a selection match with a different echoed encoding refuses",
          refused)
    try:
        reconcile_pending("pairv", window, "trades", None,
                          fetch=lambda *a, **k: [vendor_job])
        refused = False
    except Refusal:
        refused = True
    check("a missing intent timestamp refuses reconciliation", refused)

    print("no-match intents cannot clear inside the vendor-lag window")
    verdict_now = dt.datetime(2026, 8, 5, 12, 30, tzinfo=dt.timezone.utc)
    recent = {"intent_at": "2026-08-05T12:25:00+00:00",
              "no_match_listings": ["a", "b", "c"]}
    ok, reason = pending_clear_verdict(recent, now=verdict_now)
    check("a recent unmatched intent cannot resubmit, whatever the "
          "listing count", not ok and "delay" in reason)
    aged_unconfirmed = {"intent_at": "2026-08-05T11:00:00+00:00",
                        "no_match_listings": ["one"]}
    ok, reason = pending_clear_verdict(aged_unconfirmed, now=verdict_now)
    check("an aged intent with one listing still waits",
          not ok and "confirming" in reason)
    aged_confirmed = {"intent_at": "2026-08-05T11:00:00+00:00",
                      "no_match_listings": ["2026-08-05T12:00:00+00:00",
                                            "2026-08-05T12:10:00+00:00"]}
    ok, _reason = pending_clear_verdict(aged_confirmed, now=verdict_now)
    check("aged plus two spaced valid listings clears", ok)
    back_to_back = {"intent_at": "2026-08-05T11:00:00+00:00",
                    "no_match_listings": ["2026-08-05T12:10:00+00:00",
                                          "2026-08-05T12:10:30+00:00"]}
    ok, reason = pending_clear_verdict(back_to_back, now=verdict_now)
    check("back-to-back listings confirm nothing and wait",
          not ok and "span" in reason)
    ok, _reason = pending_clear_verdict({"no_match_listings": ["a", "b"]},
                                        now=verdict_now)
    check("an intent without a timestamp never clears", not ok)

    print("the cap is cumulative across runs")
    crash_jobs = {
        ledger_key("nqv", "2026-04.full", "tbbo"):
            {"job_id": "GLBX-M1", "live_quote_at_submit": 16.08},
        ledger_key("nqv", "2026-05.full", "tbbo"):
            {"state": "submitting", "live_quote_at_intent": 17.47},
    }
    prior = plan_prior_spend(crash_jobs, "nqv", "contiguous")
    check("prior spend counts submitted and pending plan entries",
          abs(prior - 33.55) < 1e-9)
    # Crash-and-resume: a 100-dollar cap with 33.55 committed leaves 66.45;
    # the remaining two months at 19.26 + 18.98 fit, but a full-cap reset
    # would have allowed rebuying everything. Prove the reduced cap refuses
    # what the full cap would have accepted.
    ok, _reason = submission_verdict(True, 100.0 - prior, None, 70.0, 70.0)
    check("a rerun cannot spend the full cap again", not ok)
    ok, _reason = submission_verdict(True, 100.0 - prior, None, 38.24, 38.24)
    check("the reduced cap still admits the genuinely remaining rows", ok)
    check("plans without ledger entries carry no prior spend",
          plan_prior_spend({}, "nqv", "contiguous") == 0.0)

    print("buy is whitelisted to the staged purchase")
    try:
        authorize_buy("nqv", "basket", {})
        refused = False
    except Refusal:
        refused = True
    check("a superseded basket cannot be bought", refused)
    try:
        authorize_buy("pairv", "contiguous", {})
        refused = False
    except Refusal:
        refused = True
    check("a mismatched scope/plan pairing cannot be bought", refused)
    try:
        authorize_buy("pairv", "paircurrent", {})
        allowed = True
    except Refusal:
        allowed = False
    check("stage one is available with an empty ledger", allowed)
    try:
        authorize_buy("nqv", "contiguous", {})
        refused = False
    except Refusal:
        refused = True
    check("stage two is locked before the paired test lands", refused)
    pair_files = {"glbx-mdp3-20260706.trades.csv.zst": "a" * 64}
    downloaded_pair = {ledger_key("pairv", "2026-07.2wk", "trades"):
                       {"job_id": "GLBX-PAIR", "state": "downloaded",
                        "files": pair_files}}
    submitted_only = {ledger_key("pairv", "2026-07.2wk", "trades"):
                      {"job_id": "GLBX-PAIR", "state": "done"}}
    try:
        authorize_buy("nqv", "contiguous", submitted_only)
        refused = False
    except Refusal:
        refused = True
    check("a merely submitted pair does not unlock stage two", refused)
    verdict_path = os.path.join(SELFTEST_DIR, "pair-verdict.json")

    def try_stage_two():
        try:
            authorize_buy("nqv", "contiguous", downloaded_pair,
                          verdict_path=verdict_path)
            return True
        except Refusal:
            return False

    check("a downloaded pair with NO verdict artifact stays locked",
          not try_stage_two())
    with open(verdict_path, "w") as fh:
        json.dump({"verdict": "fail", "job_id": "GLBX-PAIR",
                   "files": pair_files}, fh)
    check("a FAILED pair verdict keeps stage two locked",
          not try_stage_two())
    with open(verdict_path, "w") as fh:
        json.dump({"verdict": "pass", "job_id": "GLBX-OTHER",
                   "files": pair_files}, fh)
    check("a pass verdict for a different job id stays locked",
          not try_stage_two())
    with open(verdict_path, "w") as fh:
        json.dump({"verdict": "pass", "job_id": "GLBX-PAIR",
                   "files": {"glbx-mdp3-20260706.trades.csv.zst": "b" * 64}},
                  fh)
    check("a pass verdict against different bytes stays locked",
          not try_stage_two())
    with open(verdict_path, "w") as fh:
        json.dump({"verdict": "pass", "job_id": "GLBX-PAIR",
                   "files": pair_files}, fh)
    check("only the affirmative verdict bound to job and bytes unlocks",
          try_stage_two())

    print("buy-lock mutual exclusion")
    lock_path = os.path.join(SELFTEST_DIR, "jobs.json.lock")
    first = acquire_buy_lock(lock_path)
    try:
        acquire_buy_lock(lock_path)
        refused = False
    except Refusal:
        refused = True
    check("a second concurrent run refuses while the lock is held", refused)
    first.close()
    second = acquire_buy_lock(lock_path)
    check("the lock is reacquirable after release", second is not None)
    second.close()

    print("redirects are refused wherever the auth header travels")
    redirect_req = urllib.request.Request("https://hist.databento.com/x")
    try:
        _RefuseRedirect().redirect_request(
            redirect_req, None, 302, "Found", {},
            "https://evil.example.com/steal")
        refused = False
    except urllib.error.HTTPError as exc:
        refused = exc.code == 302
    check("redirect raises instead of following with the key", refused)

    print("owned entries recover without quotes")
    owned_key = ledger_key("pairv", "2026-07.2wk", "trades")
    saved_fresh = globals()["fresh_quote"]

    def outage(*args, **kwargs):
        raise Refusal("metadata outage fixture")

    globals()["fresh_quote"] = outage
    try:
        rows, quote_failures = print_review(
            "pairv", "paircurrent", False, None,
            {owned_key: {"job_id": "GLBX-A", "state": "done"}})
        check("an owned row is reviewed without any quote fetch",
              rows[0][4] is False and rows[0][3]["job_id"] == "GLBX-A"
              and quote_failures == 0)
        rows, quote_failures = print_review(
            "pairv", "paircurrent", True, 100.0, {})
        check("a per-row quote outage is contained and counted",
              rows[0][4] is False and quote_failures == 1)
    finally:
        globals()["fresh_quote"] = saved_fresh

    print("baseline age gate")
    quote_key = dp.cache_key(
        "metadata.get_cost", dict(params, mode="historical-streaming"), False)
    saved_cache = dict(dp.cache())
    try:
        dp.cache()[quote_key] = {"fetched": "2026-08-01T00:00:00+00:00",
                                 "body": 24.06}
        fresh_now = dt.datetime(2026, 8, 5, tzinfo=dt.timezone.utc)
        stale_now = dt.datetime(2026, 9, 1, tzinfo=dt.timezone.utc)
        check("a fresh baseline is read",
              planned_quote("pairv", window, "trades", now=fresh_now) == 24.06)
        check("a stale baseline is treated as absent",
              planned_quote("pairv", window, "trades", now=stale_now) is None)
        dp.cache()[quote_key] = {"fetched": "not a timestamp", "body": 24.06}
        check("an unparseable baseline timestamp is treated as absent",
              planned_quote("pairv", window, "trades", now=fresh_now) is None)
    finally:
        dp.cache().clear()
        dp.cache().update(saved_cache)

    print("download lifecycle against fixture fetchers")

    class Reader:
        def __init__(self, data):
            self.data = data
            self.pos = 0

        def read(self, n):
            chunk = self.data[self.pos:self.pos + n]
            self.pos += len(chunk)
            return chunk

        def close(self):
            pass

    def fetch_full(url, offset):
        return (206, Reader(payload[offset:])) if offset else \
            (200, Reader(payload))

    dest = os.path.join(SELFTEST_DIR, "clean.csv")
    ok, detail = download_file(fetch_full, "u", dest, len(payload), sha)
    check("clean download promotes", ok and detail == sha
          and os.path.getsize(dest) == len(payload)
          and not os.path.exists(dest + ".downloading"))
    ok, _detail = download_file(fetch_full, "u", dest, len(payload), sha)
    check("existing verified file is skipped after rehash", ok)
    with open(dest, "wb") as fh:
        fh.write(b"z" * len(payload))  # same length, different bytes
    ok, detail = download_file(fetch_full, "u", dest, len(payload), sha)
    with open(dest, "rb") as fh:
        healed = fh.read()
    check("same-length corruption is caught by rehash and re-downloaded",
          ok and detail == sha and healed == payload)

    dest = os.path.join(SELFTEST_DIR, "resume.csv")
    with open(dest + ".downloading", "wb") as fh:
        fh.write(payload[:10])
    seen_offsets = []

    def fetch_ranged(url, offset):
        seen_offsets.append(offset)
        return (206, Reader(payload[offset:])) if offset else \
            (200, Reader(payload))

    ok, detail = download_file(fetch_ranged, "u", dest, len(payload), sha)
    check("resume asks from the partial's size and verifies",
          ok and detail == sha and seen_offsets == [10])

    dest = os.path.join(SELFTEST_DIR, "complete-temp.csv")
    with open(dest + ".downloading", "wb") as fh:
        fh.write(payload)
    fetches = []
    ok, detail = download_file(
        lambda url, offset: fetches.append(offset) or (200, Reader(payload)),
        "u", dest, len(payload), sha)
    check("a complete valid temp promotes without refetching",
          ok and detail == sha and fetches == []
          and os.path.getsize(dest) == len(payload))
    dest = os.path.join(SELFTEST_DIR, "complete-bad-temp.csv")
    with open(dest + ".downloading", "wb") as fh:
        fh.write(b"q" * len(payload))
    ok, detail = download_file(fetch_full, "u", dest, len(payload), sha)
    check("a complete corrupt temp is discarded and re-downloaded",
          ok and detail == sha)

    dest = os.path.join(SELFTEST_DIR, "no206.csv")
    with open(dest + ".downloading", "wb") as fh:
        fh.write(payload[:10])

    def fetch_ignores_range(url, offset):
        return 200, Reader(payload)  # full body regardless of Range

    ok, detail = download_file(fetch_ignores_range, "u", dest,
                               len(payload), sha)
    check("range ignored: restart from zero, still correct",
          ok and detail == sha)

    dest = os.path.join(SELFTEST_DIR, "badhash.csv")

    def fetch_corrupt(url, offset):
        return 200, Reader(b"x" * len(payload))

    ok, detail = download_file(fetch_corrupt, "u", dest, len(payload), sha,
                               retries=2, sleep=lambda s: None)
    check("hash mismatch fails hard and deletes the temp",
          not ok and "sha256 mismatch" in detail
          and not os.path.exists(dest)
          and not os.path.exists(dest + ".downloading"))

    dest = os.path.join(SELFTEST_DIR, "short.csv")

    def fetch_short(url, offset):
        return 200, Reader(payload[:5])

    ok, detail = download_file(fetch_short, "u", dest, len(payload), sha,
                               retries=2, sleep=lambda s: None)
    check("size mismatch fails hard", not ok and "size mismatch" in detail
          and not os.path.exists(dest))

    dest = os.path.join(SELFTEST_DIR, "flaky.csv")
    attempts = []

    def fetch_flaky(url, offset):
        attempts.append(offset)
        if len(attempts) == 1:
            raise ConnectionResetError("mid-air disconnect")
        return (206, Reader(payload[offset:])) if offset else \
            (200, Reader(payload))

    ok, detail = download_file(fetch_flaky, "u", dest, len(payload), sha,
                               retries=3, sleep=lambda s: None)
    check("transient error retries with backoff and succeeds",
          ok and detail == sha and len(attempts) == 2)

    dest = os.path.join(SELFTEST_DIR, "forbidden.csv")
    with open(dest + ".downloading", "wb") as fh:
        fh.write(payload[:10])  # a mid-download partial when the URL expires
    forbidden_calls = []

    def fetch_forbidden(url, offset):
        forbidden_calls.append(offset)
        raise urllib.error.HTTPError(url, 403, "Forbidden", None, None)

    ok, detail = download_file(fetch_forbidden, "u", dest, len(payload), sha,
                               retries=5, sleep=lambda s: None)
    check("HTTP 403 fails fast AND preserves the resumable partial",
          not ok and "not retryable" in detail
          and forbidden_calls == [10]
          and os.path.getsize(dest + ".downloading") == 10)

    print("zstd delivery check via compression.zstd")
    zpath = os.path.join(SELFTEST_DIR, "real.csv.zst")
    zbytes = zstd.compress(b"ts,price,size\n1,2,3\n")
    zsha = hashlib.sha256(zbytes).hexdigest()

    def fetch_zstd(url, offset):
        return 200, Reader(zbytes)

    ok, detail = download_file(fetch_zstd, "u", zpath, len(zbytes), zsha)
    check("valid zstd frame promotes", ok and detail == zsha)
    fpath = os.path.join(SELFTEST_DIR, "fake.csv.zst")
    fake = b"not zstd at all, padded to a plausible length....."
    fsha = hashlib.sha256(fake).hexdigest()

    def fetch_fake(url, offset):
        return 200, Reader(fake)

    ok, detail = download_file(fetch_fake, "u", fpath, len(fake), fsha,
                               retries=1, sleep=lambda s: None)
    check("hash-valid but non-zstd delivery refused",
          not ok and "magic" in detail and not os.path.exists(fpath))

    print("fail closed on partial success")
    both = [
        dict(files[0], filename="good.csv", url="good"),
        dict(files[0], filename="bad.csv", url="bad"),
    ]

    def fetch_mixed(url, offset):
        return 200, Reader(payload if url == "good" else b"y" * len(payload))

    outdir = os.path.join(SELFTEST_DIR, "landing")
    verified, failed = download_job_files(both, outdir, fetch=fetch_mixed,
                                          retries=1, sleep=lambda s: None)
    check("good file verified, bad file named",
          list(verified) == ["good.csv"] and len(failed) == 1
          and failed[0].startswith("bad.csv"))

    print("downloaded-state re-verification")
    good_path = os.path.join(outdir, "good.csv")
    check("verify_landing passes intact files",
          verify_landing(outdir, {"good.csv": sha256_file(good_path)}) == [])
    bad = verify_landing(outdir, {"good.csv": "0" * 64,
                                  "gone.csv": "0" * 64})
    check("verify_landing names corruption and absence",
          len(bad) == 2 and any("mismatch" in b for b in bad)
          and any("missing" in b for b in bad))
    check("verify_landing refuses an empty file record",
          verify_landing(outdir, {}) != [])

    print("landing layout and manifest")
    check("landing dir shape", landing_dir("pairv", "2026-07.2wk", "trades")
          .endswith(os.path.join("research", "market-data", "databento",
                                 "pairv", "2026-07.2wk.trades")))
    write_manifest(outdir, {"job_id": "GLBX-A", "files": verified})
    with open(os.path.join(outdir, "manifest.json")) as fh:
        man = json.load(fh)
    check("manifest records job, files and tool version",
          man["job_id"] == "GLBX-A" and man["tool_version"] == TOOL_VERSION
          and "good.csv" in man["files"])

    shutil.rmtree(SELFTEST_DIR)
    failed = [name for name, ok in checks if not ok]
    print("\n%d check(s), %d failed" % (len(checks), len(failed)))
    if failed:
        for name in failed:
            print("  FAIL", name)
        raise SystemExit(1)
    print("selftest PASS")


# ---------------------------------------------------------------------------


def main():
    sys.stdout.reconfigure(line_buffering=True)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode",
                        choices=["selftest", "status", "plan", "buy"])
    parser.add_argument("scope", nargs="?",
                        help="a databento_price scope, e.g. pairv")
    parser.add_argument("plan", nargs="?",
                        help="a databento_price plan, e.g. paircurrent")
    parser.add_argument("--confirm", action="store_true",
                        help="arm submission; alone it still submits nothing")
    parser.add_argument("--max-dollars", type=float, default=None,
                        help="cumulative cap on the PLAN's submitted spend "
                             "across runs - prior submitted and pending "
                             "entries count against it; required, with "
                             "--confirm, to submit anything")
    args = parser.parse_args()
    if args.mode == "selftest":
        selftest()
    elif args.mode == "status":
        mode_status(args)
    else:
        if not args.scope or not args.plan:
            raise Refusal("%s needs a scope and a plan" % args.mode)
        if args.mode == "plan":
            mode_plan(args)
        else:
            mode_buy(args)


if __name__ == "__main__":
    main()
