"""Choose which tick-data windows to buy, using the cheap 1-minute bars as a
sampling frame.

The bars cannot see microstructure (aggregation destroys arrival burstiness,
bounce and size dispersion), so this stratifies on what a bar CAN measure -
volatility, volume level, volume burstiness, gap behaviour and the
zero-change fraction at minute scale - and picks the calendar months whose
feature vectors are farthest apart. The tick data then reports whether those
proxies coincided with microstructure regimes, which is itself a finding.

Two phases, cached so selection can be re-run without re-reading 1.5 GB:

    python3 analysis/select_windows.py features   # one pass per archive
    python3 analysis/select_windows.py select     # pick the windows

Session convention: CME trades 17:00 to 16:00 US Central, so a bar at or after
17:00 belongs to the NEXT calendar day's session. Prices in the _bk archives are
ratio back-adjusted, which preserves returns exactly; only levels are unusable.
"""

import collections
import datetime as dt
import json
import math
import os
import sys
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
MARKET_DATA = os.path.join(ROOT, "research", "market-data")
CACHE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "cme_daily_features.json")

ARCHIVES = {
    "NQ": "nq-1m_bk.zip",
    "ES": "es-1m_bk.zip",
    "CL": "cl-1m_bk.zip",
    "GC": "gc-1m.zip",
}

# Databento GLBX.MDP3 history does not reach back to 2007. Months before this
# are kept in the features (they describe the instrument) but are not eligible
# to be selected. Confirmed against the vendor: coverage starts 2010-06-06 for
# trades, tbbo, mbp-1, mbp-10, definition and statistics (mbo only from
# 2017-05-21).
#
# LOAD-BEARING BEYOND ELIGIBILITY: every z-score in `phase_select` is computed
# over the eligible months only, so moving this constant does not merely add or
# remove candidates, it re-centres and re-scales the feature space and can
# reshuffle a selection that otherwise looks stable. Change it and re-read the
# whole selection, not just the newly admitted months.
DATABENTO_START = "2010-06"

# How many month-equivalents of budget, and how to spend them.
BUDGET_MONTHS = 9

FEATURES = [
    "rv",             # realized volatility, sum of squared 1m log returns
    "vol_of_vol",     # dispersion of hourly realized vol within the session
    "volume",         # total contracts
    "volume_cv",      # per-minute volume burstiness, the cadence proxy
    "zero_change",    # fraction of minutes with no close-to-close change
    "gap",            # absolute overnight gap into the session
]


def parse_line(line):
    parts = line.rstrip("\r\n").split(";")
    if len(parts) != 7:
        return None
    # IndexError as well as ValueError: a time field carrying no colon leaves
    # `hms` one element long and `hms[1]` raises IndexError, which this used to
    # let escape and kill the whole feature sweep on one malformed row. A date
    # field with too few slashes raises ValueError on the unpack, which was
    # already covered.
    try:
        day, month, year = (int(x) for x in parts[0].split("/"))
        hms = [int(x) for x in parts[1].split(":")]
        stamp = dt.datetime(year, month, day, hms[0], hms[1], hms[2] if len(hms) > 2 else 0)
        return stamp, float(parts[2]), float(parts[5]), int(parts[6])
    except (ValueError, IndexError):
        return None


def session_date(stamp):
    """CME session runs 17:00 CT to 16:00 CT; label it by the closing date."""
    if stamp.hour >= 17:
        return (stamp + dt.timedelta(days=1)).date()
    return stamp.date()


def build_features(symbol, archive):
    path = os.path.join(MARKET_DATA, archive)
    z = zipfile.ZipFile(path)
    member = z.infolist()[0].filename

    days = collections.OrderedDict()
    prev_stamp = None
    prev_close = None
    prev_session = None

    with z.open(member) as fh:
        for raw in fh:
            parsed = parse_line(raw.decode("ascii", "replace"))
            if parsed is None:
                continue
            stamp, open_px, close, vol = parsed
            if prev_stamp is not None and stamp <= prev_stamp:
                continue  # duplicate or backwards timestamp
            if close <= 0.0 or open_px <= 0.0:
                continue
            sess = session_date(stamp)
            if sess.weekday() == 5:
                continue  # stray Saturday bars

            slot = days.get(sess)
            if slot is None:
                slot = {
                    "ret2": 0.0, "n": 0, "volume": 0, "vols": [], "max_r2": 0.0,
                    "zero": 0, "hourly": collections.defaultdict(float), "gap": 0.0,
                }
                days[sess] = slot
                if prev_close is not None and prev_session is not None and sess != prev_session:
                    slot["gap"] = abs(math.log(open_px / prev_close))

            if prev_close is not None and prev_session == sess:
                # The vendor omits no-trade minutes rather than writing a
                # zero-volume bar, so a minute with no print is simply absent.
                # Those minutes are real zero-return, zero-volume observations,
                # and dropping them inflates zero_change and volume_cv in
                # exactly the illiquid regimes the stratification cares about.
                missing = int((stamp - prev_stamp).total_seconds() // 60) - 1
                if missing > 0:
                    slot["zero"] += missing
                    slot["n"] += missing
                    slot["vols"].extend([0] * missing)
                r = math.log(close / prev_close)
                slot["ret2"] += r * r
                # gc-1m.zip is RAW, not back-adjusted, so its roll sessions carry
                # a contract-switch jump as one enormous 1-minute return. Tracking
                # the session maximum lets it be removed below; dropping 1 of
                # ~1380 bars barely touches genuine volatility but kills the roll
                # artifact that would otherwise steer the window selection.
                if r * r > slot["max_r2"]:
                    slot["max_r2"] = r * r
                slot["hourly"][stamp.hour] += r * r
                if r == 0.0:
                    slot["zero"] += 1
                slot["n"] += 1
            slot["volume"] += vol
            slot["vols"].append(vol)

            prev_stamp, prev_close, prev_session = stamp, close, sess

    # A full CME session is about 1380 one-minute bars. The old threshold of 200
    # let early closes (Thanksgiving, Christmas Eve, July 3) through, and since
    # rv is an UNNORMALISED sum over however many bars exist, a stub session
    # understates volatility purely by bar count.
    full_session = 1380
    out = {}
    for sess, slot in days.items():
        if slot["n"] < 1000:
            continue  # holiday or half session, not a regime observation
        vols = slot["vols"]
        mean_v = sum(vols) / len(vols)
        var_v = sum((v - mean_v) ** 2 for v in vols) / len(vols)
        # Buckets accumulate squared returns, so take the root to get an hourly
        # VOLATILITY before measuring its dispersion; the coefficient of
        # variation of a variance is a different, more skewed quantity.
        hourly = [math.sqrt(v) for v in slot["hourly"].values()]
        mean_h = sum(hourly) / len(hourly) if hourly else 0.0
        var_h = (sum((h - mean_h) ** 2 for h in hourly) / len(hourly)) if hourly else 0.0
        # Drop the single largest squared return (the roll artifact in raw GC)
        # and scale to a full session so short days compare like for like.
        trimmed = max(0.0, slot["ret2"] - slot["max_r2"])
        out[sess.isoformat()] = {
            "rv": math.sqrt(trimmed * full_session / slot["n"]),
            "vol_of_vol": math.sqrt(var_h) / mean_h if mean_h > 0 else 0.0,
            "volume": float(slot["volume"]) * full_session / slot["n"],
            "volume_cv": math.sqrt(var_v) / mean_v if mean_v > 0 else 0.0,
            "zero_change": slot["zero"] / slot["n"],
            "gap": slot["gap"],
        }
    return out


def phase_features():
    cache = {}
    for symbol, archive in ARCHIVES.items():
        sys.stderr.write("scanning %s ...\n" % archive)
        cache[symbol] = build_features(symbol, archive)
        sys.stderr.write("  %s sessions\n" % len(cache[symbol]))
    with open(CACHE, "w") as fh:
        json.dump(cache, fh)
    print("wrote", CACHE)


def median(values):
    """True median. Taking the upper middle value on an even-sized month is a
    real bias here: months have 19-23 sessions, so the even case is common and
    it shifted which months the stratification picked."""
    ordered = sorted(values)
    n = len(ordered)
    if n == 0:
        raise ValueError("median of empty sequence")
    mid = n // 2
    if n % 2:
        return ordered[mid]
    return 0.5 * (ordered[mid - 1] + ordered[mid])


def monthly(cache):
    """Collapse sessions to months, per symbol, as medians (robust to spikes)."""
    months = collections.defaultdict(lambda: collections.defaultdict(list))
    for symbol, sessions in cache.items():
        for day, feats in sessions.items():
            months[day[:7]][symbol].append(feats)
    out = {}
    for month, by_symbol in months.items():
        if len(by_symbol) < len(ARCHIVES):
            continue
        row = {}
        ok = True
        for symbol, rows in by_symbol.items():
            if len(rows) < 15:
                ok = False
                break
            for feat in FEATURES:
                row["%s.%s" % (symbol, feat)] = median([r[feat] for r in rows])
        if ok:
            out[month] = row
    return out


def zscore(months):
    keys = sorted(next(iter(months.values())).keys())
    stats = {}
    for key in keys:
        vals = [m[key] for m in months.values()]
        mean = sum(vals) / len(vals)
        var = sum((v - mean) ** 2 for v in vals) / len(vals)
        stats[key] = (mean, math.sqrt(var) if var > 0 else 1.0)
    return {
        month: [(row[key] - stats[key][0]) / stats[key][1] for key in keys]
        for month, row in months.items()
    }, keys


def farthest_point(vectors, k, seeds):
    chosen = list(seeds)
    while len(chosen) < k:
        best, best_d = None, -1.0
        for month, vec in vectors.items():
            if month in chosen:
                continue
            d = min(
                math.sqrt(sum((a - b) ** 2 for a, b in zip(vec, vectors[c])))
                for c in chosen
            )
            if d > best_d:
                best, best_d = month, d
        chosen.append(best)
    return chosen


def phase_select():
    with open(CACHE) as fh:
        cache = json.load(fh)
    months = monthly(cache)
    eligible = {m: r for m, r in months.items() if m >= DATABENTO_START}
    vectors, keys = zscore(eligible)

    # Seed with the two months that must be in any sample: the most volatile
    # (stress regime) and the most recent (current algo population).
    nq_rv = keys.index("NQ.rv")
    stress = max(vectors, key=lambda m: vectors[m][nq_rv])
    recent = max(vectors)
    # Dedupe: if the most volatile month IS the most recent one, seeding with
    # both yields a basket one month short while looking full.
    seeds = [stress] if stress == recent else [stress, recent]
    chosen = farthest_point(vectors, BUDGET_MONTHS, seeds)

    print("eligible months: %d  (%s .. %s)" % (len(eligible), min(eligible), max(eligible)))
    print()
    print("%-9s %8s %8s %8s %8s %8s" % ("month", "NQ.rv", "NQ.vcv", "NQ.zero", "CL.rv", "GC.rv"))
    for month in sorted(chosen):
        row = eligible[month]
        print("%-9s %8.4f %8.3f %8.3f %8.4f %8.4f" % (
            month, row["NQ.rv"], row["NQ.volume_cv"], row["NQ.zero_change"],
            row["CL.rv"], row["GC.rv"],
        ))
    print()
    print("percentile of each pick within the eligible span (NQ realized vol):")
    ordered = sorted(eligible, key=lambda m: eligible[m]["NQ.rv"])
    for month in sorted(chosen):
        pct = 100.0 * ordered.index(month) / (len(ordered) - 1)
        print("  %-9s  %5.1f%%" % (month, pct))


def phase_drift():
    """Yearly medians. Answers whether microstructure proxies are era-stable,
    which decides how much of the budget may be spent on old data."""
    with open(CACHE) as fh:
        cache = json.load(fh)
    months = monthly(cache)
    years = collections.defaultdict(list)
    for month, row in months.items():
        years[month[:4]].append(row)
    cols = ["NQ.zero_change", "NQ.volume_cv", "NQ.volume", "ES.zero_change", "CL.zero_change", "GC.zero_change"]
    print("%-6s %12s %10s %14s %12s %12s %12s" % ("year", "NQ.zero", "NQ.vcv", "NQ.volume", "ES.zero", "CL.zero", "GC.zero"))
    for year in sorted(years):
        rows = years[year]
        out = []
        for col in cols:
            vals = sorted(r[col] for r in rows)
            out.append(vals[len(vals) // 2])
        print("%-6s %12.4f %10.3f %14.0f %12.4f %12.4f %12.4f" % (year, out[0], out[1], out[2], out[3], out[4], out[5]))


def phase_plan():
    """Budget-aware plan: bulk of the spend inside the current microstructure
    era, stratified by volatility, plus a few older probes for drift."""
    with open(CACHE) as fh:
        cache = json.load(fh)
    months = monthly(cache)
    eligible = sorted(m for m in months if m >= DATABENTO_START)

    # The last 30 ELIGIBLE months, which is not the last 30 CALENDAR months: a
    # month absent from the feature cache (no archive coverage, or a month the
    # roll-trim emptied) silently extends the pool further back in time. On the
    # committed archives the two coincide, so this is stated rather than fixed -
    # but the pool's true span is printed below precisely so a divergence is
    # visible rather than assumed away.
    recent = eligible[-30:]
    ranked = sorted(recent, key=lambda m: months[m]["NQ.rv"])
    print("current-era pool: %s .. %s  (%d months, last 30 ELIGIBLE not calendar)" % (
        recent[0], recent[-1], len(recent)))
    print()
    print("stratified by NQ realized vol within that pool:")
    for pct in (0, 20, 40, 60, 80, 100):
        idx = min(len(ranked) - 1, int(round(pct / 100.0 * (len(ranked) - 1))))
        month = ranked[idx]
        print("  p%-3d  %-9s  NQ.rv %7.4f  CL.rv %7.4f  GC.rv %7.4f  NQ.vcv %5.2f" % (
            pct, month, months[month]["NQ.rv"], months[month]["CL.rv"],
            months[month]["GC.rv"], months[month]["NQ.volume_cv"]))
    print()
    print("historical drift probes (highest NQ.rv per era):")
    for lo, hi in (("2010-06", "2013-12"), ("2014-01", "2017-12"), ("2018-01", "2021-12")):
        pool = [m for m in eligible if lo <= m <= hi]
        month = max(pool, key=lambda m: months[m]["NQ.rv"])
        calm = min(pool, key=lambda m: months[m]["NQ.rv"])
        print("  %s..%s  stress %-9s NQ.rv %7.4f   calm %-9s NQ.rv %7.4f" % (
            lo, hi, month, months[month]["NQ.rv"], calm, months[calm]["NQ.rv"]))


def main():
    phase = sys.argv[1] if len(sys.argv) > 1 else "select"
    if phase == "features":
        phase_features()
    elif phase == "select":
        phase_select()
    elif phase == "drift":
        phase_drift()
    elif phase == "plan":
        phase_plan()
    else:
        raise SystemExit("usage: select_windows.py [features|select|drift|plan]")


if __name__ == "__main__":
    main()
