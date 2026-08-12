#!/usr/bin/env python3
"""Drive the manifest pull, one single-row plan at a time.

The manifest is seventy-eight plans - thirteen months by three instruments
by two schemas - deliberately split so one bad month cannot abort
seventy-seven good ones. That makes hand-driving impractical, so this walks
the list. It adds NO authority of its own: every submission still goes
through databento_download's double gate, its whitelist, its strict
subscription verdict and its seal binding. This only decides the ORDER and
does the waiting.

ORDER IS A CONTRACT REQUIREMENT, not a preference. The manifest says the
MBP-1 pull "must not delay or endanger the TBBO pull, and yields to it on
any conflict", so TBBO is a complete wave before MBP-1 begins. Within a
wave, months run newest first: the entitlement window ROLLS, so the oldest
month is the one closest to being lost, but the newest is the one most
likely to be wanted first if the run is interrupted - and the edge month is
already truncated, so it is pulled first of all.

WHY SUBPROCESSES rather than importing mode_buy: the buy lifecycle takes a
process-lifetime flock and exits by raising SystemExit, and the exit codes
are the documented interface - 0 settled, 3 nonterminal so poll again, 1
anything failed. Running it as a child keeps that contract intact and keeps
one plan's crash from taking the driver down with it.

Usage:
    python3 -u analysis/databento_settle.py plan tbbo
    python3 -u analysis/databento_settle.py run tbbo
    python3 -u analysis/databento_settle.py run mbp-1
    python3 -u analysis/databento_settle.py run tbbo --only 2026-01
"""

import argparse
import os
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import databento_price as dp

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOWNLOADER = os.path.join(ROOT, "analysis", "databento_download.py")

# A cap that admits a covered row, which quotes 0.00, and refuses anything
# priced. Belt and braces beside the downloader's strict zero rule: two
# independent reasons a priced request cannot be bought by accident.
MAX_DOLLARS = "0.01"

# Vendor batch jobs take minutes. Backoff is bounded and polite; the vendor
# is not being hammered, and a job that never finishes is a failure to
# report rather than a loop to spin in.
POLL_FIRST = 20
POLL_MAX = 120
POLL_TIMEOUT = 3600

EXIT_SETTLED = 0
EXIT_FAILED = 1
EXIT_NONTERMINAL = 3


def manifest_months():
    """Every manifest month, edge first, then newest to oldest, tail last.

    The edge month leads because it is the only one already truncated by the
    rolling window: if anything is going to be lost to a delay, it is that
    one."""
    months = [dp.MANIFEST_EDGE_MONTH[0].split(".")[0]]
    months += sorted(dp.MANIFEST_FULL_MONTHS, reverse=True)
    months.append(dp.MANIFEST_TAIL_MONTH[0].split(".")[0])
    return months


def wave_plans(schema):
    """The plans of one wave, as (scope, plan, month) in pull order."""
    out = []
    for month in manifest_months():
        for instrument, scope in sorted(dp.MANIFEST_INSTRUMENTS.items()):
            out.append((scope, dp.manifest_plan_name(instrument, month,
                                                     schema), month))
    return out


def tbbo_outstanding():
    """TBBO plans the job ledger does not record as downloaded.

    The manifest requires the MBP-1 pull to yield to the TBBO pull on any
    conflict, so this is a PREREQUISITE and not a preference: documenting
    the order in a docstring while letting the MBP-1 wave start on demand
    would leave the requirement unenforced."""
    import databento_download as dl
    jobs = dl.load_ledger()
    outstanding = []
    for scope, plan, _month in wave_plans("tbbo"):
        window, schema = dl.resolve_plan(scope, plan)[0]
        entry = jobs.get(dl.ledger_key(scope, window[0], schema))
        if not entry or entry.get("state") != "downloaded":
            outstanding.append("%s/%s: %s" % (
                scope, plan,
                "no ledger entry" if not entry
                else "state %s" % entry.get("state")))
    return outstanding


def run_plan(scope, plan, armed):
    """One invocation of the downloader. Returns its exit code."""
    argv = [sys.executable, "-u", DOWNLOADER, "buy", scope, plan]
    if armed:
        argv += ["--confirm", "--max-dollars", MAX_DOLLARS]
    # S603: a fixed argv of our own interpreter and our own script, with no
    # shell and no caller-supplied string; scope and plan are validated
    # against the manifest tables before they get here.
    result = subprocess.run(argv, check=False)  # noqa: S603
    return result.returncode


def settle_wave(plans, timeout=POLL_TIMEOUT):
    """Submit every plan, THEN poll and download until all are settled.

    The vendor processes jobs in parallel and takes minutes per job. Driving
    plan-by-plan - submit, wait alone, download, next - serialized that
    processing and spent roughly two hundred idle seconds per plan waiting
    for one job while thirty-eight others could have been running. Measured
    on the first wave: nine plans, every one of them waiting by itself.

    So phase one submits everything and phase two collects. Downloads stay
    SERIAL on purpose: they are bandwidth-bound, so running them at once
    would divide one pipe and multiply partial files in flight, which is
    cost without benefit.

    Nothing about the guards changes. Each plan is still its own single-row
    invocation through the double gate, the strict subscription verdict, the
    whitelist and the seal binding. Submitting earlier is a scheduling
    decision, not an authorization one - and the exit-code contract already
    anticipated it: 3 means come back and poll."""
    failures = []
    pending = []
    print("=== phase 1: submitting %d plan(s)" % len(plans))
    for index, (scope, plan, month) in enumerate(plans, start=1):
        code = run_plan(scope, plan, armed=True)
        if code == EXIT_FAILED:
            print("  [%d/%d] %s FAILED to submit" % (index, len(plans), plan))
            failures.append("%s/%s submit exit %d" % (scope, plan, code))
            break
        if code == EXIT_NONTERMINAL:
            # Only unsettled plans carry into phase two. Carrying settled
            # ones would re-hash every landed file on the first poll cycle,
            # which by the end of a wave is tens of gigabytes of rework.
            pending.append((scope, plan, month))
        print("  [%d/%d] %s %s" % (
            index, len(plans), plan,
            "settled already" if code == EXIT_SETTLED else "submitted"))
    if failures:
        return failures
    print("\n=== phase 2: %d plan(s) to collect" % len(pending))
    started = time.time()
    delay = POLL_FIRST
    while pending:
        still = []
        for scope, plan, month in pending:
            code = run_plan(scope, plan, armed=True)
            if code == EXIT_SETTLED:
                print("  settled %s" % plan)
            elif code == EXIT_NONTERMINAL:
                still.append((scope, plan, month))
            else:
                failures.append("%s/%s exit %d" % (scope, plan, code))
        pending = still
        if not pending:
            break
        if failures:
            break
        if time.time() - started > timeout:
            print("  TIMEOUT after %.0fs with %d plan(s) nonterminal" % (
                time.time() - started, len(pending)))
            failures.extend("%s/%s still nonterminal" % (s, p)
                            for s, p, _m in pending)
            break
        print("  %d plan(s) nonterminal; sleeping %ds" % (len(pending),
                                                          delay))
        time.sleep(delay)
        delay = min(delay * 2, POLL_MAX)
    return failures


def main():
    sys.stdout.reconfigure(line_buffering=True)
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["plan", "run"])
    parser.add_argument("schema", choices=list(dp.MANIFEST_SCHEMAS))
    parser.add_argument("--only", default=None, choices=manifest_months(),
                        metavar="YYYY-MM",
                        help="restrict to exactly this manifest month")
    args = parser.parse_args()
    plans = wave_plans(args.schema)
    if args.only:
        # Exact month equality against the parsed month, not a substring
        # over the plan name: "2026" or "tbbo" would otherwise select most
        # of the wave while claiming to name one month.
        plans = [(s, p, m) for s, p, m in plans if m == args.only]
    if not plans:
        raise SystemExit("no plans matched")
    if args.mode == "plan":
        print("%d plan(s) in the %s wave, in order:" % (len(plans),
                                                        args.schema))
        for scope, plan, _month in plans:
            print("  %-6s %s" % (scope, plan))
        print("\nnothing submitted; this mode never submits")
        return
    if args.schema == "mbp-1":
        outstanding = tbbo_outstanding()
        if outstanding:
            print("REFUSED: the MBP-1 wave yields to the TBBO pull, and %d "
                  "TBBO plan(s) are not settled:" % len(outstanding))
            for line in outstanding[:10]:
                print("   ", line)
            if len(outstanding) > 10:
                print("    ... and %d more" % (len(outstanding) - 10))
            raise SystemExit(1)
    failures = settle_wave(plans)
    print()
    if failures:
        print("WAVE FAILED: %d" % len(failures))
        for line in failures:
            print("   ", line)
        raise SystemExit(1)
    print("wave %s settled: %d plan(s)" % (args.schema, len(plans)))


if __name__ == "__main__":
    main()
