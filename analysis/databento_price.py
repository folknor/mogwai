"""Price candidate Databento requests before buying any of them.

Answers three questions the vendor cost calculator left open:

  1. What does each candidate window actually cost, per schema?
  2. Do the pre-2016 windows really cost nothing, or does the query match zero
     records? A zero cost and a zero record count mean the same number of
     dollars and very different things.
  3. Does narrowing from the whole CME book to four front-month symbols cut the
     cost enough to change the plan from part-months back to full months?

Talks to the historical REST API directly with urllib rather than the vendor
SDK: the SDK is not packaged for Debian and PEP 668 blocks a system pip, and
analysis/ is stdlib-only by convention. Metadata calls are not billed, so this
is safe to run repeatedly.

    python3 analysis/databento_price.py info      # dataset range, schemas, raw JSON
    python3 analysis/databento_price.py resolve   # symbology, incl. the 2011/2016 zero
    python3 analysis/databento_price.py price     # the cost matrix
    python3 analysis/databento_price.py plan      # total the recommended basket
    python3 analysis/databento_price.py cache     # what the response cache holds

Every successful response is cached to analysis/databento_cache.json, keyed by
endpoint and parameters, so a re-run of a phase is free and offline. A full
price sweep is several hundred round trips at roughly a second each; without a
cache, changing one window meant re-fetching all of them, and a Ctrl-C threw the
whole sweep away. Errors are never cached, so a transient failure does not
become a permanent one. Pass --refresh to bypass and overwrite.

The key is read from research/databento.key (research/ is gitignored) or from
the DATABENTO_API_KEY environment variable. Never commit it.
"""

import base64
import datetime as dt
import json
import os
import sys
import urllib.error
import urllib.parse
import urllib.request
from zoneinfo import ZoneInfo

# The bar archives are in CME time and select_windows.py labels a session by its
# 16:00 Central close, so a session D runs from D-1 17:00 to D 16:00 Central.
# Databento reads a date-only bound as UTC midnight, which would clip the first
# five or six hours off the intended opening session and pull in the same slice
# of the session after the end. Every bound is therefore sent as an explicit
# UTC instant derived from the Central session boundary, DST included.
CME_TZ = ZoneInfo("America/Chicago")


def session_bounds_utc(start_date, end_date):
    """Sessions [start_date, end_date) as Central time, returned as UTC instants."""
    first = dt.date.fromisoformat(start_date)
    last = dt.date.fromisoformat(end_date)
    open_ct = dt.datetime.combine(
        first - dt.timedelta(days=1), dt.time(17, 0), tzinfo=CME_TZ)
    close_ct = dt.datetime.combine(
        last - dt.timedelta(days=1), dt.time(16, 0), tzinfo=CME_TZ)
    to_utc = lambda x: x.astimezone(dt.timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")
    return to_utc(open_ct), to_utc(close_ct)

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KEY_FILE = os.path.join(ROOT, "research", "databento.key")
CACHE_FILE = os.path.join(ROOT, "analysis", "databento_cache.json")
CACHE_VERSION = 1

BASE = "https://hist.databento.com/v0"
DATASET = "GLBX.MDP3"

# Symbol scopes to compare. The whole book is what the vendor calculator was
# quoting; the narrowed scopes are the lever worth testing.
SCOPES = {
    "book": ("ALL_SYMBOLS", "raw_symbol"),
    "parent": ("NQ.FUT,ES.FUT,CL.FUT,GC.FUT", "parent"),
    "continuous": ("NQ.c.0,ES.c.0,CL.c.0,GC.c.0", "continuous"),
    # The micros did not list until May 2019, so they cannot appear in any
    # drift probe before then. MNQ against NQ is the useful pairing: same index,
    # same 0.25 tick, one tenth the notional, different participant mix, so a
    # difference in their fitted constants is about the contract rather than
    # about the market.
    "micros": ("MNQ.c.0,MES.c.0", "continuous"),
    "targets": ("NQ.c.0,ES.c.0,CL.c.0,GC.c.0,MNQ.c.0,MES.c.0", "continuous"),
    # Narrower sets, to find the cheapest scope that still answers the
    # questions: NQ alone, the NQ/MNQ controlled pair, and equity index only.
    "nq": ("NQ.c.0", "continuous"),
    # c.0 is CALENDAR ranked and holds the nearest expiry until it expires, so
    # through a roll it tracks the contract liquidity is leaving. v.0 follows
    # volume, which is the contract a strategy would actually trade.
    "nqv": ("NQ.v.0", "continuous"),
    "nqmnq": ("NQ.c.0,MNQ.c.0", "continuous"),
    # The pair on v.0, which is what would actually be bought: the paired
    # comparison has to be the same ranking as the rest of the basket or the
    # difference it measures is partly a difference in contract selection.
    "pairv": ("NQ.v.0,MNQ.v.0", "continuous"),
    # MNQ alone on v.0: the wave 2B scope. Wave 1 judged NQ not a usable MNQ
    # proxy, so direct MNQ evidence is the operative path; the quote seams are
    # per-instrument and need MNQ quotes under either pair verdict.
    "mnqv": ("MNQ.v.0", "continuous"),
    # GC alone. Not a preset target, but the only cheap way to get a SECOND
    # tick-size-to-price ratio: NQ and MNQ share the 0.25 tick and the same
    # index, so they cannot between them test whether a derived zero_change_frac
    # generalises. GC's 0.1 tick at a different price level can.
    "gcv": ("GC.v.0", "continuous"),
    "equity": ("NQ.c.0,ES.c.0,MNQ.c.0,MES.c.0", "continuous"),
}

# The micros listed in May 2019, so a window earlier than that prices
# identically with and without them. That was verified empirically against the
# 2011-08 and 2016-01 windows rather than encoded as a table: a LISTED_FROM map
# lived here, was read by nothing, and would have been a second source of truth
# for a fact the API already answers.

# Candidate windows. Part-month slices avoid the quarterly roll (third Friday,
# volume roll about a week earlier) by sitting after it in Mar/Jun/Sep months.
# Strata from select_windows.py AFTER the roll-trim and session-normalisation
# fixes. The pre-fix run named 2025-09 and 2025-01; both were artifacts of raw
# GC roll jumps and unnormalised short sessions, and neither survives.
#
# Part-month slices avoid the quarterly roll. Third Fridays are 2024-09-20,
# 2026-03-20 and 2026-06-19, so a slice that merely sits late in the month still
# straddles expiry; these end before it instead.
WINDOWS = [
    ("2020-03.full", "2020-03-01", "2020-04-01", "extreme stress, 2x current-era max"),
    ("2020-03.2wk", "2020-03-09", "2020-03-23", "the dislocation itself"),
    ("2024-05.full", "2024-05-01", "2024-06-01", "calmest current-era, p0"),
    ("2024-05.2wk", "2024-05-06", "2024-05-20", "no index roll"),
    ("2024-11.full", "2024-11-01", "2024-12-01", "p40"),
    ("2024-11.2wk", "2024-11-04", "2024-11-18", "no index roll"),
    ("2024-09.full", "2024-09-01", "2024-10-01", "p60"),
    ("2024-09.2wk", "2024-09-03", "2024-09-17", "ends before the 09-20 expiry"),
    ("2025-11.full", "2025-11-01", "2025-12-01", "p80"),
    ("2025-11.2wk", "2025-11-03", "2025-11-17", "no index roll"),
    ("2025-04.full", "2025-04-01", "2025-05-01", "current-era NQ stress, p100"),
    ("2025-04.2wk", "2025-04-07", "2025-04-21", "no index roll"),
    ("2026-03.full", "2026-03-01", "2026-04-01", "recent high-vol, p90"),
    ("2026-03.2wk", "2026-03-02", "2026-03-16", "ends before the 03-20 expiry"),
    ("2026-06.full", "2026-06-01", "2026-07-01", "most recent"),
    ("2026-06.3wk", "2026-06-22", "2026-07-13", "post-expiry, day-of-week repeats"),
    ("2011-08.full", "2011-08-01", "2011-09-01", "drift probe, pre-micro era"),
    ("2011-08.2wk", "2011-08-01", "2011-08-15", "drift probe, pre-micro era"),
    ("2016-01.full", "2016-01-01", "2016-02-01", "drift probe, pre-micro era"),
    # Contiguous recent months, added 2026-08-05 after the sampling-frame
    # experiment FAILED its preregistered association test and the
    # preregistered consequence replaced regime-selected windows with
    # contiguous recent ones. No stratum labels: recency IS the selection.
    ("2026-04.full", "2026-04-01", "2026-05-01", "contiguous recent"),
    ("2026-05.full", "2026-05-01", "2026-06-01", "contiguous recent"),
    ("2026-07.full", "2026-07-01", "2026-08-01", "contiguous recent, newest complete"),
    ("2026-07.2wk", "2026-07-06", "2026-07-20", "paired test, post 06-19 expiry, no roll"),
]

SCHEMAS = ["trades", "tbbo", "definition", "statistics"]


_KEY = None


def api_key():
    # Memoised: this used to re-read the key file on every one of several
    # hundred requests per sweep.
    global _KEY
    if _KEY is None:
        key = os.environ.get("DATABENTO_API_KEY")
        if not key and os.path.exists(KEY_FILE):
            with open(KEY_FILE) as fh:
                key = fh.read().strip()
        if not key:
            raise SystemExit(
                "no API key: write it to research/databento.key or set DATABENTO_API_KEY"
            )
        _KEY = key
    return _KEY


_CACHE = None
REFRESH = False
CACHE_HITS = 0
CACHE_MISSES = 0


def cache():
    """The on-disk response cache, loaded once. A corrupt or stale-versioned
    file is discarded rather than fatal: it holds nothing that cannot be
    re-fetched for free."""
    global _CACHE
    if _CACHE is None:
        _CACHE = {}
        if os.path.exists(CACHE_FILE):
            try:
                with open(CACHE_FILE) as fh:
                    loaded = json.load(fh)
                if loaded.get("_version") == CACHE_VERSION:
                    _CACHE = loaded.get("entries", {})
            except (json.JSONDecodeError, OSError, AttributeError):
                pass
    return _CACHE


def cache_key(endpoint, params, raw):
    # Sorted params so key order in a caller's dict cannot split one logical
    # request across two entries. raw is part of the key because it changes the
    # TYPE of what is returned, not just the formatting.
    items = sorted((params or {}).items())
    return "%s|%s|%s" % (endpoint, "raw" if raw else "json",
                         urllib.parse.urlencode(items))


def cache_store(key, value):
    """Write through immediately. A sweep is minutes long and interrupting it
    is normal, so batching the flush to exit would lose exactly the work the
    cache exists to preserve."""
    cache()[key] = {"fetched": dt.datetime.now(dt.timezone.utc).isoformat(),
                    "body": value}
    tmp = CACHE_FILE + ".tmp"
    with open(tmp, "w") as fh:
        json.dump({"_version": CACHE_VERSION, "entries": _CACHE}, fh,
                  indent=1, sort_keys=True)
    os.replace(tmp, CACHE_FILE)


def request(endpoint, params=None, raw=False):
    """GET an endpoint. Basic auth with the key as username, empty password.

    Successful responses are served from and written to the disk cache. Failures
    are returned but NOT cached, so a network blip does not freeze into a
    permanent wrong answer for a window."""
    global CACHE_HITS, CACHE_MISSES
    key = cache_key(endpoint, params, raw)
    if not REFRESH and key in cache():
        CACHE_HITS += 1
        return cache()[key]["body"]
    CACHE_MISSES += 1
    url = "%s/%s" % (BASE, endpoint)
    if params:
        url = "%s?%s" % (url, urllib.parse.urlencode(params))
    token = base64.b64encode(("%s:" % api_key()).encode("ascii")).decode("ascii")
    req = urllib.request.Request(url, headers={"Authorization": "Basic %s" % token})
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            body = resp.read().decode("utf-8")
    except urllib.error.HTTPError as exc:
        # Databento echoes an invalid Basic-auth username back in the error
        # body, and the username IS the key. Redact before this reaches stdout
        # or a captured log.
        detail = exc.read().decode("utf-8", "replace")[:400]
        detail = detail.replace(api_key(), "<REDACTED-KEY>")
        return {"_error": "HTTP %s %s" % (exc.code, detail)}
    except urllib.error.URLError as exc:
        return {"_error": "URL %s" % exc.reason}
    if raw:
        cache_store(key, body)
        return body
    try:
        value = json.loads(body)
    except json.JSONDecodeError:
        # Unparseable is a server-side oddity, not a settled answer. Return it
        # so the caller can flag it, but do not cache it.
        return {"_raw": body}
    cache_store(key, value)
    return value


def query(scope, window, schema):
    symbols, stype_in = SCOPES[scope]
    _name, start, end, _why = window
    utc_start, utc_end = session_bounds_utc(start, end)
    return {
        "dataset": DATASET,
        "symbols": symbols,
        "schema": schema,
        "start": utc_start,
        "end": utc_end,
        "stype_in": stype_in,
    }


def measure(scope, window, schema):
    params = query(scope, window, schema)
    cost = request("metadata.get_cost", dict(params, mode="historical-streaming"))
    count = request("metadata.get_record_count", params)
    return cost, count


def billable_bytes(scope, window, schema):
    """Billable size in bytes. This is the DBN-encoded size, which is what a
    batch download actually transfers; CSV output is substantially larger."""
    return as_number(request("metadata.get_billable_size",
                             query(scope, window, schema)))


def as_number(value):
    if isinstance(value, dict):
        if "_error" in value:
            return None
        for key in ("cost", "result", "value"):
            if key in value:
                value = value[key]
                break
        else:
            return None
    try:
        return float(value)
    except (TypeError, ValueError):
        return None


def phase_info():
    for endpoint, params in (
        ("metadata.list_datasets", None),
        ("metadata.list_schemas", {"dataset": DATASET}),
        ("metadata.get_dataset_range", {"dataset": DATASET}),
    ):
        print("=" * 78)
        print(endpoint, params or "")
        print(json.dumps(request(endpoint, params), indent=1)[:2000])


def phase_resolve():
    """The 2011/2016 windows quoted at zero. If symbology fails to resolve there
    but succeeds in 2025, the zero was an empty match, not free data."""
    probes = [("2011-08", "2011-08-01", "2011-08-31"),
              ("2016-01", "2016-01-01", "2016-01-31"),
              ("2025-04", "2025-04-01", "2025-04-30")]
    for scope, (symbols, stype_in) in SCOPES.items():
        if stype_in == "raw_symbol":
            continue
        for label, start, end in probes:
            res = request("symbology.resolve", {
                "dataset": DATASET,
                "symbols": symbols,
                "stype_in": stype_in,
                "stype_out": "instrument_id",
                "start_date": start,
                "end_date": end,
            })
            print("-" * 60)
            print("%s  %s" % (scope, label))
            print(json.dumps(res, indent=1)[:900])


def phase_price():
    scopes = sys.argv[2].split(",") if len(sys.argv) > 2 else ["continuous"]
    schemas = sys.argv[3].split(",") if len(sys.argv) > 3 else SCHEMAS
    unknown = [s for s in scopes if s not in SCOPES]
    if unknown:
        raise SystemExit("unknown scope(s): %s; known: %s" % (
            ", ".join(unknown), ", ".join(sorted(SCOPES))))
    for scope in scopes:
        print("=" * 78)
        print("scope: %s  symbols=%s  stype_in=%s" % (scope, SCOPES[scope][0], SCOPES[scope][1]))
        print("%-16s %-12s %12s %16s" % ("window", "schema", "cost", "records"))
        for window in WINDOWS:
            for schema in schemas:
                cost, count = measure(scope, window, schema)
                dollars, records = as_number(cost), as_number(count)
                # A zero cost, a zero record count and a failed parse are three
                # different things and the whole point of this script is to keep
                # them apart. Flags accumulate rather than overwrite.
                flags = []
                if records == 0:
                    flags.append("EMPTY, not free")
                if records is None:
                    flags.append("COUNT UNREADABLE %r" % (count,)[:1])
                if dollars is None:
                    flags.append("COST UNREADABLE %r" % (cost,)[:1])
                flag = ("   <-- " + "; ".join(flags)) if flags else ""
                print("%-16s %-12s %12s %16s%s" % (
                    window[0], schema,
                    "n/a" if dollars is None else "%.2f" % dollars,
                    "n/a" if records is None else "{:,}".format(int(records)),
                    flag,
                ))


# The basket recommended from the bar-derived stratification. Schema per window
# reflects where spread behaviour is the open question (calm, stress, and the
# 2020 dislocation get TBBO; the rest get trades).
# Priced against scope "nq". Narrowing to the one instrument that actually needs
# a preset buys FULL months across every stratum plus TBBO on three of them,
# where the four- and six-symbol scopes could only afford part-months. TBBO goes
# where spread behaviour is the open question: the calm baseline, the current-era
# stress peak, and the 2020 dislocation.
PLAN = [
    ("2024-05.full", "tbbo"),    # p0, calmest current-era
    ("2024-09.full", "trades"),  # p60
    ("2024-11.full", "trades"),  # p40
    ("2025-11.full", "trades"),  # p80
    ("2025-04.full", "tbbo"),    # p100, current-era stress peak
    ("2026-03.full", "trades"),  # recent high-vol
    ("2026-06.full", "trades"),  # most recent, current algo population
    ("2020-03.full", "tbbo"),    # extreme dislocation
    ("2011-08.full", "trades"),  # drift probe, pre-micro era
]

# The alternative argued on review: buy SPREAD everywhere rather than length
# everywhere. Ten sessions already exhausts the precision available for a CV, a
# lag-1 ACF or a log-size sigma, while the fill band is not fitted at all and
# only TBBO can fit it, so length is the cheaper thing to give up.
#
# Two windows stay full months on purpose. The abs-return ACF tail at lag 50 and
# the GARCH persistence term are the one target family where sample LENGTH is
# the binding constraint rather than sample count, so the calm anchor and the
# stress anchor keep their full spans and everything else shortens.
DEPTH_PLAN = [
    ("2024-05.full", "tbbo"),    # p0 anchor, full month for the ACF tail
    ("2025-04.full", "tbbo"),    # p100 anchor, full month for the ACF tail
    ("2024-09.2wk", "tbbo"),     # p60
    ("2025-11.2wk", "tbbo"),     # p80
    ("2026-03.2wk", "tbbo"),     # recent high-vol
    ("2026-06.3wk", "tbbo"),     # most recent, current algo population
    ("2020-03.2wk", "tbbo"),     # the dislocation itself
    ("2011-08.2wk", "trades"),   # drift probe; drift needs no spread
]

# The contract-versus-market question on its own, to be priced against scope
# "pairv" so both legs are the same instants and the same volume ranking. This
# is the test whose answer decides whether an NQ-fitted preset is legitimate for
# MNQ at all, so it is worth knowing its price in isolation from the basket it
# would otherwise be bought after.
PAIR_PLAN = [("2024-05.2wk", "trades")]

# A second tick grid, priced on its own. Two windows so the ratio is measured at
# two different volatilities rather than at one point.
GRID_PLAN = [("2024-05.2wk", "trades"), ("2025-04.2wk", "trades")]

# The replacement design, 2026-08-05, after the sampling-frame FAIL rejected
# every volatility-stratified basket above. Contiguous recent months at full
# coverage, all TBBO: the failed frame removes the license to pick regimes, so
# recency is the whole selection; TBBO because the synthetic decomposition
# established that a trade-derived proxy cannot serve as the dependent
# variable in a spread fit, and the protocol 7 width and displacement seams
# are the slots only quote evidence can fill; full months because the
# abs-return ACF tail and GARCH persistence are where contiguous LENGTH binds,
# an argument the FAIL does not touch.
CONTIGUOUS_PLAN = [
    ("2026-04.full", "tbbo"),
    ("2026-05.full", "tbbo"),
    ("2026-06.full", "tbbo"),
    ("2026-07.full", "tbbo"),
]

# The paired NQ/MNQ test on a RECENT no-roll window. The pair question - is NQ
# a valid proxy for MNQ - survives the sampling-frame FAIL untouched, but its
# old 2024-05 window was itself regime-selected as the p0 calm month, which is
# exactly the rationale the FAIL rejected. A recent post-expiry fortnight,
# 2026-07-06 to 07-20: front month stable after the 06-19 quarterly expiry, no
# roll inside the window, and recency serves the protocol 8 volume-versus-count
# obligation directly. Trades schema: the pair question needs counts and
# moments, not quotes, per the original 9.3 design.
PAIR_CURRENT_PLAN = [("2026-07.2wk", "trades")]

# Wave 2B of 9.7: direct MNQ TBBO, priced against scope "mnqv". This is the
# OPERATIVE FAIL-BRANCH composition, fixed now that wave 1's verdict is final:
# the pair test FAILED, so the target instrument carries two contiguous months
# of trade-and-quote evidence itself. A pass would have bought 2026-07 alone;
# that is provenance, not a latent branch - the plan never inspects the
# verdict. Both windows already exist in WINDOWS (added for the superseded
# nqv contiguous design); reusing them is deliberate, since WINDOWS describes
# time bounds, not instruments, and the cache keys on the full request
# parameters. Do not duplicate them for MNQ.
MNQ2B_PLAN = [
    ("2026-07.full", "tbbo"),
    ("2026-06.full", "tbbo"),
]

PLANS = {"basket": PLAN, "depth": DEPTH_PLAN, "pair": PAIR_PLAN,
         "grid": GRID_PLAN, "contiguous": CONTIGUOUS_PLAN,
         "paircurrent": PAIR_CURRENT_PLAN, "mnq2b": MNQ2B_PLAN}
BUDGET = 125.0


def phase_plan():
    scope = sys.argv[2] if len(sys.argv) > 2 else "book"
    variant = sys.argv[3] if len(sys.argv) > 3 else "basket"
    if scope not in SCOPES:
        raise SystemExit("unknown scope %s; known: %s" % (
            scope, ", ".join(sorted(SCOPES))))
    if variant not in PLANS:
        raise SystemExit("unknown plan %s; known: %s" % (
            variant, ", ".join(sorted(PLANS))))
    plan = PLANS[variant]
    by_name = {w[0]: w for w in WINDOWS}
    total = 0.0
    print("scope: %s   plan: %s" % (scope, variant))
    print("%-16s %-10s %12s %16s  %s" % ("window", "schema", "cost", "records", "why"))
    failures = []
    total_bytes = 0.0
    for name, schema in plan:
        window = by_name[name]
        cost, count = measure(scope, window, schema)
        dollars, records = as_number(cost), as_number(count)
        size = billable_bytes(scope, window, schema)
        if size is not None:
            total_bytes += size
        # Fail closed. A total that silently skips an unreadable row looks
        # authoritative and under-quotes the basket, which is precisely the
        # mistake this script exists to prevent.
        if dollars is None or records is None or dollars < 0:
            failures.append("%s/%s unreadable" % (name, schema))
        elif records == 0:
            failures.append("%s/%s matched zero records" % (name, schema))
        else:
            total += dollars
        print("%-16s %-10s %12s %16s %10s  %s" % (
            name, schema,
            "n/a" if dollars is None else "%.2f" % dollars,
            "n/a" if records is None else "{:,}".format(int(records)),
            "n/a" if size is None else "%.2f GB" % (size / 1e9),
            window[3],
        ))
    # definition and statistics are rounding error but are required to resolve
    # the front month and to recover the tick grid the adjusted bars destroyed.
    #
    # The record count is fetched here and used ONLY for its None check, which
    # looks wasteful and is deliberate: an unreadable count means the query
    # itself did not resolve, and a cost that parses beside a count that does
    # not is exactly the case where a plausible-looking 0.00 would be added to
    # the total. Unlike the window loop above, a count of ZERO is not a failure
    # here - a month with no statistics messages is normal - so only the
    # unreadable case is caught. The extra call is free after the first run,
    # being cached.
    extras = 0.0
    for schema in ("definition", "statistics"):
        for name, _ in plan:
            cost, count = measure(scope, by_name[name], schema)
            dollars, records = as_number(cost), as_number(count)
            if dollars is None or records is None:
                failures.append("%s/%s unreadable" % (name, schema))
            else:
                extras += dollars
    print()
    if failures:
        print("REFUSING TO TOTAL: %d row(s) did not return a usable answer" % len(failures))
        for line in failures:
            print("   ", line)
        print("partial sum was %.2f; treat it as a LOWER BOUND, not a quote" % (total + extras))
        return
    print("windows %.2f  definition+statistics %.2f" % (total, extras))
    print("total %.2f of %.2f budget, headroom %.2f" % (
        total + extras, BUDGET, BUDGET - total - extras))
    print("download %.2f GB (DBN encoding; CSV output is several times larger)"
          % (total_bytes / 1e9))


def phase_cache():
    """Report what is cached and how old it is. Prices are not eternal, so an
    entry's age is part of reading it."""
    entries = cache()
    if not entries:
        print("cache empty (%s)" % CACHE_FILE)
        return
    by_endpoint = {}
    oldest, newest = None, None
    for key, entry in entries.items():
        endpoint = key.split("|", 1)[0]
        by_endpoint[endpoint] = by_endpoint.get(endpoint, 0) + 1
        stamp = entry.get("fetched", "")
        oldest = stamp if oldest is None or stamp < oldest else oldest
        newest = stamp if newest is None or stamp > newest else newest
    print("%s" % CACHE_FILE)
    print("%d entries, %.1f KB" % (
        len(entries), os.path.getsize(CACHE_FILE) / 1024.0))
    print("fetched between %s and %s" % (oldest, newest))
    for endpoint in sorted(by_endpoint):
        print("  %-32s %5d" % (endpoint, by_endpoint[endpoint]))


def main():
    # Line-buffer stdout: piped to a file this is otherwise 8 KB block-buffered,
    # so a long sweep looks wedged for minutes before the first row appears.
    sys.stdout.reconfigure(line_buffering=True)
    global REFRESH
    if "--refresh" in sys.argv:
        REFRESH = True
        sys.argv.remove("--refresh")
    phase = sys.argv[1] if len(sys.argv) > 1 else "info"
    phases = {"info": phase_info, "resolve": phase_resolve,
              "price": phase_price, "plan": phase_plan, "cache": phase_cache}
    if phase not in phases:
        raise SystemExit(
            "usage: databento_price.py [info|resolve|price|plan|cache] [scope] [--refresh]")
    phases[phase]()
    if phase != "cache" and (CACHE_HITS or CACHE_MISSES):
        print("\n[cache] %d hit(s), %d fetch(es)%s" % (
            CACHE_HITS, CACHE_MISSES, ", --refresh forced" if REFRESH else ""))


if __name__ == "__main__":
    main()
