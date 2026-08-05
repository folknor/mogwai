#!/usr/bin/env python3
"""Buy, poll and download the staged Databento purchase, doubly gated.

Implements notes/databento-downloader-spec.md. The pricing tool
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
               Without BOTH --confirm and --max-dollars it is a dry run that
               prints the same table as plan and submits nothing.

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
import hashlib
import json
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
        with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT) as resp:
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


def planned_quote(scope, window, schema):
    """The plan-time baseline: the quote the pricing script cached when the
    plan was accepted. Read from databento_price's own cache with its own key
    derivation, so a re-keying there cannot silently detach the baseline.
    Returns None when no baseline exists."""
    params = dp.query(scope, window, schema)
    key = dp.cache_key(
        "metadata.get_cost", dict(params, mode="historical-streaming"), False)
    entry = dp.cache().get(key)
    if entry is None:
        return None
    return dp.as_number(entry["body"])


# ---------------------------------------------------------------------------
# Ledger
# ---------------------------------------------------------------------------
# analysis/databento-jobs.json, committed. Keyed "scope|window|schema". Unlike
# the pricing cache, an unreadable ledger is FATAL, not discarded: discarding
# it is exactly how a paid job would be bought twice.


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


def save_ledger(jobs, path=LEDGER_FILE):
    tmp = path + ".tmp"
    with open(tmp, "w") as fh:
        json.dump({"_version": LEDGER_VERSION, "jobs": jobs}, fh,
                  indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, path)


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
    """
    if ledger_entry is not None and ledger_entry.get("job_id"):
        return False, "already submitted as job %s; poll, never resubmit" % (
            ledger_entry["job_id"])
    if not confirm:
        return False, "no --confirm; dry run, nothing submitted"
    if max_dollars is None:
        return False, "no --max-dollars; dry run, nothing submitted"
    if live is None or live < 0:
        return False, "live quote unreadable"
    if live > max_dollars:
        return False, "live quote %.2f exceeds remaining budget %.2f" % (
            live, max_dollars)
    if planned is None:
        return False, ("no plan-time quote in the pricing cache; run "
                       "databento_price.py plan first so drift is measurable")
    if planned <= 0:
        return False, "plan-time quote %.2f is not a usable baseline" % planned
    if live > planned * (1.0 + DRIFT_LIMIT):
        return False, "live quote %.2f drifted more than %d%% above the " \
            "plan-time quote %.2f" % (live, int(DRIFT_LIMIT * 100), planned)
    return True, "ok: %.2f within budget and within drift of %.2f" % (
        live, planned)


def submit_gated(scope, window, schema, confirm, max_dollars, ledger_entry,
                 post=http_json):
    """The ONE call site of batch.submit_job in this file, and the only
    function from which the string "batch.submit_job" is reachable. It
    re-prices fresh, asks submission_verdict, and refuses (raising, submitting
    nothing) unless the verdict is ok. Returns (job_dict, live_quote).

    `post` is injectable ONLY so the selftest can prove, without a network,
    that a failing verdict never reaches it; production callers do not pass
    it."""
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
    data = {
        "dataset": dp.DATASET,
        "start": start,
        "end": end,
        "symbols": symbols,
        "schema": schema,
        "stype_in": stype_in,
        "stype_out": STYPE_OUT,
        "encoding": ENCODING,
        "compression": COMPRESSION,
        "pretty_px": False,
        "pretty_ts": False,
        "map_symbols": True,   # SDK default for csv encoding
        "split_symbols": False,
        "split_duration": SPLIT_DURATION,
        "delivery": "download",
    }
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


# ---------------------------------------------------------------------------
# Polling and file listing
# ---------------------------------------------------------------------------


def poll_job_state(job_id):
    """The job's current vendor state via batch.list_jobs. Parameter names
    ("states", "since") and the list-of-dicts response are from the SDK's
    list_jobs(); the "id"/"state" keys per databento-ingest's list_jobs()."""
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
    for entry in entries:
        try:
            filename = str(entry["filename"])
            hash_str = str(entry["hash"])
            size = int(entry["size"])
            https_url = entry["urls"]["https"]
        except (KeyError, TypeError, ValueError) as exc:
            raise Refusal("file manifest entry unparseable (%s): %r" % (
                exc, entry))
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
    resp = urllib.request.urlopen(req, timeout=HTTP_TIMEOUT)
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
        # A promoted file was verified when promoted; re-verify size only.
        if os.path.getsize(dest_path) == expected_size:
            return True, expected_sha256
        os.remove(dest_path)  # wrong size cannot have passed the promote gate
    last_error = "not attempted"
    for attempt in range(retries):
        if attempt > 0:
            sleep(RETRY_DELAY_BASE * (2 ** (attempt - 1)))
        hasher = hashlib.sha256()
        offset = 0
        try:
            if os.path.exists(tmp_path):
                existing = os.path.getsize(tmp_path)
                if 0 < existing < expected_size:
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
                    os.remove(tmp_path)
                    last_error = zstd_error
                    continue
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
            if os.path.exists(tmp_path):
                os.remove(tmp_path)
            return False, "HTTP %d %s: not retryable" % (exc.code, exc.reason)
        except (urllib.error.URLError, ConnectionError, TimeoutError,
                OSError) as exc:
            # Transient: keep the temp for resume, retry with backoff.
            last_error = "%s: %s" % (type(exc).__name__, exc)
    if os.path.exists(tmp_path):
        os.remove(tmp_path)  # exhausted: a stale partial helps nobody
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


def write_manifest(dest_dir, entry):
    """The per-directory provenance record of spec decision 7. The committed
    record is the ledger; this one travels with the (gitignored) bytes."""
    tmp = os.path.join(dest_dir, "manifest.json.tmp")
    with open(tmp, "w") as fh:
        json.dump(dict(entry, tool_version=TOOL_VERSION), fh,
                  indent=1, sort_keys=True)
        fh.write("\n")
    os.replace(tmp, os.path.join(dest_dir, "manifest.json"))


# ---------------------------------------------------------------------------
# Modes
# ---------------------------------------------------------------------------


def resolve_plan(scope, variant):
    if scope not in dp.SCOPES:
        raise Refusal("unknown scope %s; known: %s" % (
            scope, ", ".join(sorted(dp.SCOPES))))
    if variant not in dp.PLANS:
        raise Refusal("unknown plan %s; known: %s" % (
            variant, ", ".join(sorted(dp.PLANS))))
    by_name = {w[0]: w for w in dp.WINDOWS}
    return [(by_name[name], schema) for name, schema in dp.PLANS[variant]]


def print_review(scope, variant, confirm, max_dollars):
    """The would-submit table: fresh quote, plan-time baseline, drift, and
    the verdict each row would receive. Shared by plan (always) and buy
    (before acting). Returns the rows for buy to act on."""
    jobs = load_ledger()
    rows = []
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
        live = fresh_quote(scope, window, schema)
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
    return rows


def mode_plan(args):
    print_review(args.scope, args.plan, confirm=False, max_dollars=None)
    print("\nnothing submitted; this mode never submits")


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
    """The lifecycle driver. For each plan row: an entry with a job id is
    polled and, when done, downloaded and verified; an entry without one goes
    through submit_gated(), which is the only path to batch.submit_job and
    refuses unless the double gate and the drift check all pass. Any failed
    file fails the run with a nonzero exit and a named list."""
    rows = print_review(args.scope, args.plan, args.confirm, args.max_dollars)
    jobs = load_ledger()
    remaining = args.max_dollars
    failures = []
    for window, schema, key, entry, would_submit in rows:
        print("\n== %s" % key)
        entry = jobs.get(key)  # reread: earlier rows may have updated it
        if entry is None or not entry.get("job_id"):
            if not (args.confirm and args.max_dollars is not None):
                print("  dry run: would %ssubmit" %
                      ("" if would_submit else "NOT "))
                continue
            try:
                job, live = submit_gated(
                    args.scope, window, schema, args.confirm, remaining, entry)
            except Refusal as exc:
                print("  %s" % exc)
                failures.append("%s: not submitted" % key)
                continue
            remaining -= live
            entry = {
                "job_id": job["id"],
                "state": job.get("state", "queued"),
                "submitted_at": dt.datetime.now(dt.timezone.utc).isoformat(),
                "live_quote_at_submit": live,
                "planned_quote": planned_quote(args.scope, window, schema),
                "scope": args.scope,
                "window": window[0],
                "schema": schema,
                "encoding": ENCODING,
                "compression": COMPRESSION,
                "split_duration": SPLIT_DURATION,
            }
            jobs[key] = entry
            save_ledger(jobs)
            print("  submitted job %s at %.2f" % (job["id"], live))
        if entry.get("state") == "downloaded":
            print("  already downloaded and verified")
            continue
        state = poll_job_state(entry["job_id"])
        entry["state"] = state
        save_ledger(jobs)
        print("  job %s state: %s" % (entry["job_id"], state))
        if state == "expired":
            failures.append("%s: job expired before download" % key)
            continue
        if state != "done":
            print("  not ready; re-run buy later to poll again")
            continue
        files = list_job_files(entry["job_id"])
        dest = landing_dir(args.scope, window[0], schema)
        verified, file_failures = download_job_files(files, dest)
        if file_failures:
            failures.extend("%s/%s" % (key, f) for f in file_failures)
            continue
        entry["state"] = "downloaded"
        entry["files"] = verified
        save_ledger(jobs)
        write_manifest(dest, entry)
        print("  %d file(s) verified into %s" % (len(verified), dest))
    print()
    if failures:
        print("FAILED: %d item(s) incomplete" % len(failures))
        for line in failures:
            print("   ", line)
        raise SystemExit(1)
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
        "urls": {"https": "https://example.invalid/f.csv.zst"},
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
    check("existing verified file is skipped", ok)

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
    forbidden_calls = []

    def fetch_forbidden(url, offset):
        forbidden_calls.append(offset)
        raise urllib.error.HTTPError(url, 403, "Forbidden", None, None)

    ok, detail = download_file(fetch_forbidden, "u", dest, len(payload), sha,
                               retries=5, sleep=lambda s: None)
    check("HTTP 403 fails fast, no retries",
          not ok and "not retryable" in detail and len(forbidden_calls) == 1
          and not os.path.exists(dest + ".downloading"))

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
                        help="hard cap on this run's total submission cost; "
                             "required, with --confirm, to submit anything")
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
