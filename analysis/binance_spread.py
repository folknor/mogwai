"""Fail-closed parser, as-of join and spread smoke analysis for Binance archives.

Four DISTINCT stages, kept apart so a defect in one cannot be mistaken for a
defect in another:

  1. PARSE     archive fields into typed records. Never invents a timestamp the
               file does not carry.
  2. JOIN      newest quote with transaction_time <= trade.time; update id
               resolves quote ties.
  3. SEQUENCE  labels equal-millisecond cross-stream matches ambiguous. It must
               NOT use ZIP row order to manufacture cross-file ordering: two
               files written independently have no shared row order, and using
               one would invent precision the millisecond stamps do not have.
  4. PARENT    groups by the declared archive heuristic only, preserving raw
               group size and timestamp multiplicity so the coarsening that
               millisecond resolution forces is measurable rather than assumed.

The join SEMANTICS are pinned by `asof_join.py`'s fixture. This module cannot
reuse that implementation on real data - it rescans all quotes per trade, which
is O(n*m) and hopeless at 1.5M trades by 7.4M quotes - so it implements a
streaming merge and CROSS-CHECKS it against the fixture implementation on the
fixture data. Same contract, different algorithm, tested equivalent.

    python3 analysis/binance_spread.py smoke <trades.zip> <bookTicker.zip>
"""

import io
import json
import math
import os
import sys
import zipfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

import asof_join as aj
import roll_estimator as re


class FailClosed(Exception):
    """Evidence a parser cannot safely proceed against."""


TRADE_HEADER = ["id", "price", "qty", "quote_qty", "time", "is_buyer_maker"]
QUOTE_HEADER = [
    "update_id",
    "best_bid_price",
    "best_bid_qty",
    "best_ask_price",
    "best_ask_qty",
    "transaction_time",
    "event_time",
]


def _member_lines(path):
    with zipfile.ZipFile(path) as zf:
        infos = zf.infolist()
        if len(infos) != 1:
            raise FailClosed(f"{path}: expected one member, found {len(infos)}")
        with zf.open(infos[0]) as raw:
            stream = io.TextIOWrapper(raw, encoding="utf-8", newline="")
            for line in stream:
                line = line.rstrip("\r\n")
                if line:
                    yield line


def count_physical_rows(path):
    """Total physical rows, header included.

    File-level validation must see the WHOLE file. The streaming join stops
    pulling quotes once trades are exhausted, so quotes it never consumed would
    otherwise escape validation entirely - and "we validated the file" would
    mean "we validated the part we happened to read".
    """
    return sum(1 for _ in _member_lines(path))


def _positive_finite(value, label, row):
    if not math.isfinite(value) or value <= 0.0:
        raise FailClosed(f"row {row}: {label} is not finite and positive: {value!r}")
    return value


def _boolean(token, row):
    if token == "true":
        return True
    if token == "false":
        return False
    raise FailClosed(f"row {row}: invalid boolean {token!r}")


def stream_trades(path, stats):
    """Typed trades, fail-closed. Carries ONE timestamp because the file does."""
    expect = len(TRADE_HEADER)
    previous_time = None
    previous_id = None
    for row, line in enumerate(_member_lines(path)):
        fields = line.split(",")
        if row == 0:
            if fields != TRADE_HEADER:
                raise FailClosed(f"{path}: unexpected header {fields!r}")
            continue
        if len(fields) != expect:
            raise FailClosed(f"row {row}: field width {len(fields)} != {expect}")
        try:
            trade_id = int(fields[0])
            price = float(fields[1])
            qty = float(fields[2])
            time = int(fields[4])
        except ValueError as error:
            raise FailClosed(f"row {row}: conversion failure: {error}") from error
        _positive_finite(price, "price", row)
        _positive_finite(qty, "qty", row)
        if previous_time is not None and time < previous_time:
            raise FailClosed(f"row {row}: timestamp regression {time} < {previous_time}")
        if previous_id is not None:
            if trade_id < previous_id:
                raise FailClosed(f"row {row}: id regression {trade_id} < {previous_id}")
            if trade_id == previous_id:
                # A repeated id with different content is a conflict, not a gap.
                raise FailClosed(f"row {row}: conflicting duplicate id {trade_id}")
        previous_time, previous_id = time, trade_id
        stats["parsed"] += 1
        yield aj.Trade(time, price, qty, _boolean(fields[5], row))


def stream_quotes(path, stats):
    expect = len(QUOTE_HEADER)
    previous_txn = None
    previous_id = None
    for row, line in enumerate(_member_lines(path)):
        fields = line.split(",")
        if row == 0:
            if fields != QUOTE_HEADER:
                raise FailClosed(f"{path}: unexpected header {fields!r}")
            continue
        if len(fields) != expect:
            raise FailClosed(f"row {row}: field width {len(fields)} != {expect}")
        try:
            update_id = int(fields[0])
            bid = float(fields[1])
            ask = float(fields[3])
            txn = int(fields[5])
            event = int(fields[6])
        except ValueError as error:
            raise FailClosed(f"row {row}: conversion failure: {error}") from error
        _positive_finite(bid, "bid", row)
        _positive_finite(ask, "ask", row)
        if previous_txn is not None and txn < previous_txn:
            raise FailClosed(f"row {row}: transaction_time regression")
        if previous_id is not None:
            if update_id < previous_id:
                raise FailClosed(f"row {row}: update_id regression")
            if update_id == previous_id:
                raise FailClosed(f"row {row}: conflicting duplicate update_id {update_id}")
        previous_txn, previous_id = txn, update_id
        stats["parsed"] += 1
        # Locked and crossed books are REPORTED, not rejected: they are evidence
        # about the venue, and dropping them would hide it.
        if bid > ask:
            stats["crossed"] += 1
        elif bid == ask:
            stats["locked"] += 1
        yield aj.Quote(txn, event, update_id, bid, ask)


def streaming_asof(trades, quotes, max_age=None):
    """Merge join. Both inputs must be non-decreasing in their join key, which
    the file contracts established. Keeps only the newest eligible quote, so
    memory is O(1) rather than O(quotes)."""
    quote_iter = iter(quotes)
    held = None
    pending = next(quote_iter, None)
    for trade in trades:
        while pending is not None and pending.transaction_time <= trade.time:
            held = pending
            pending = next(quote_iter, None)
        if held is None:
            yield aj.Match(trade, None, None, aj.STATUS_NO_QUOTE_BEFORE)
            continue
        age = trade.time - held.transaction_time
        if max_age is not None and age > max_age:
            yield aj.Match(trade, held, age, aj.STATUS_STALE)
            continue
        yield aj.Match(trade, held, age, aj.STATUS_MATCHED)


def cross_check_against_fixture():
    """The streaming join must agree with the fixture-pinned implementation.

    Different algorithm, same contract. Without this the fixture would certify
    an implementation that never touches real data.
    """
    quotes = aj._fixture()
    trades = [aj.Trade(t, 1.0, 1.0, False) for t in (999, 1000, 2000, 2999, 3500, 5500)]
    expected = aj.asof_join(trades, quotes)
    actual = list(streaming_asof(trades, quotes))
    if actual != expected:
        raise FailClosed(f"streaming join disagrees with the pinned contract:\n{actual}\n{expected}")


def infer_parents(matches):
    """Contiguous runs sharing timestamp AND aggressor side.

    The ONLY grouping signal is the declared heuristic. Group sizes and the
    timestamp multiplicity that produced them are both returned, so the
    coarsening millisecond resolution forces is measurable instead of assumed.
    """
    groups = []
    current = []
    for match in matches:
        trade = match.trade
        if current:
            head = current[0].trade
            same = head.time == trade.time and head.is_buyer_maker == trade.is_buyer_maker
            if not same:
                groups.append(current)
                current = []
        current.append(match)
    if current:
        groups.append(current)
    return groups


ZERO_AGE = "zero_ambiguous"


def age_stratum(age, positive_boundaries):
    """Zero age is its OWN CATEGORICAL stratum, not the bottom bin.

    A zero-age join is not "very fresh". At millisecond stamps an equal-time
    quote may actually FOLLOW the trade, so the as-of rule can admit lookahead
    inside the millisecond. That is a different kind of thing from a 3 ms quote,
    and binning them together would let an unorderable majority set the tone of
    a cell that reads as freshness.
    """
    if age == 0:
        return ZERO_AGE
    return f"age_{sum(1 for b in positive_boundaries if age > b)}"


def trailing_vol_events(prices, horizon):
    """Volatility AT each price, from returns strictly before the change leaving
    it. Same information set as the synthetic harness: it may include the return
    arriving at the price, never the one leaving."""
    returns = [math.log(b / a) for a, b in zip(prices, prices[1:])]
    out = [None] * len(prices)
    for i in range(horizon, len(prices)):
        window = returns[i - horizon:i]
        out[i] = math.sqrt(sum(r * r for r in window) / len(window))
    return out


def quantiles(values, points):
    ordered = sorted(values)
    return [ordered[min(len(ordered) - 1, int((len(ordered) - 1) * p))] for p in points]


def percentiles(values, points=(0.5, 0.9, 0.99, 1.0)):
    if not values:
        return {}
    ordered = sorted(values)
    return {
        f"p{int(p * 100)}": ordered[min(len(ordered) - 1, int((len(ordered) - 1) * p))]
        for p in points
    }


def histogram(values, cap=8):
    counts = {}
    for value in values:
        key = value if value < cap else f"{cap}+"
        counts[str(key)] = counts.get(str(key), 0) + 1
    return dict(sorted(counts.items(), key=lambda kv: (len(kv[0]), kv[0])))


def smoke(trades_zip, quotes_zip, tick=0.1):
    cross_check_against_fixture()
    trade_stats = {"parsed": 0}
    quote_stats = {"parsed": 0, "locked": 0, "crossed": 0}

    # File-level counts, taken independently of the join so unconsumed rows are
    # still validated and still reconcile.
    trade_rows = count_physical_rows(trades_zip)
    quote_rows = count_physical_rows(quotes_zip)

    matches = list(
        streaming_asof(
            stream_trades(trades_zip, trade_stats),
            stream_quotes(quotes_zip, quote_stats),
        )
    )

    by_status = {}
    ages = []
    quoted = []
    effective = []
    per_trade = []
    for match in matches:
        by_status[match.status] = by_status.get(match.status, 0) + 1
        if match.status != aj.STATUS_MATCHED:
            per_trade.append(None)
            continue
        ages.append(match.quote_age)
        q = aj.quoted_spread(match.quote) / tick
        e = aj.effective_spread(match.trade, match.quote) / tick
        quoted.append(q)
        effective.append(e)
        # SEQUENCING: an equal-millisecond match cannot be ordered between two
        # independently written files. Row order within a ZIP is not evidence of
        # cross-file ordering and is deliberately not consulted.
        per_trade.append({"age": match.quote_age, "quoted": q, "effective": e})

    positive_ages = [a for a in ages if a > 0]
    # Boundaries from POSITIVE ages only, and fixed here - before any Roll value
    # in this run has been looked at.
    age_boundaries = quantiles(positive_ages, (0.5, 0.9)) if positive_ages else []

    def negatives(subset):
        return {
            "count": sum(1 for e in subset if e < 0),
            "fraction": (sum(1 for e in subset if e < 0) / len(subset)) if subset else None,
            "n": len(subset),
        }

    zero_eff = [p["effective"] for p in per_trade if p and p["age"] == 0]
    positive_eff = [p["effective"] for p in per_trade if p and p["age"] > 0]
    negative_breakdown = {
        "zero_age_sequencing_ambiguous": negatives(zero_eff),
        "strictly_positive_age": negatives(positive_eff),
        "by_positive_age_stratum": {
            stratum: negatives(
                [
                    p["effective"]
                    for p in per_trade
                    if p and p["age"] > 0 and age_stratum(p["age"], age_boundaries) == stratum
                ]
            )
            for stratum in sorted(
                {age_stratum(a, age_boundaries) for a in positive_ages}
            )
        },
    }

    groups = infer_parents(matches)
    group_sizes = [len(g) for g in groups]
    first_prices = [g[0].trade.price for g in groups]
    last_prices = [g[-1].trade.price for g in groups]
    all_prices = [m.trade.price for m in matches]

    zero_count = sum(1 for a in ages if a == 0)
    report = {
        "inputs": {"trades": os.path.basename(trades_zip), "quotes": os.path.basename(quotes_zip)},
        "tick": tick,
        "row_accounting": {
            "trades": {
                "physical_rows": trade_rows,
                "header_rows": 1,
                "data_rows": trade_rows - 1,
                "parsed": trade_stats["parsed"],
                "rejected": 0,
                "skipped_blank": 0,
                "balanced": trade_rows - 1 == trade_stats["parsed"],
            },
            "quotes": {
                "physical_rows": quote_rows,
                "header_rows": 1,
                "data_rows": quote_rows - 1,
                # The join stops pulling quotes once trades run out. Consumed
                # and unconsumed are reported separately: a single "parsed"
                # number here would silently mean "the part we read".
                "consumed_by_join": quote_stats["parsed"],
                "unconsumed_trailing": (quote_rows - 1) - quote_stats["parsed"],
                "balanced": (quote_rows - 1) >= quote_stats["parsed"],
            },
            "note": "the earlier 1,469,268 figure was PHYSICAL rows including the header; 1,469,267 is parsed data rows. Same file, different quantity.",
        },
        "quote_book_state": {"locked": quote_stats["locked"], "crossed": quote_stats["crossed"]},
        "join": {
            "by_status": by_status,
            "match_rate": by_status.get(aj.STATUS_MATCHED, 0) / max(1, len(matches)),
        },
        "quote_age_ms": {
            "zero_age_count": zero_count,
            "zero_age_fraction": zero_count / max(1, len(ages)),
            "positive_age_boundaries": age_boundaries,
            **percentiles(ages),
        },
        "sequencing_ambiguous": {
            "count": zero_count,
            "fraction": zero_count / max(1, len(ages)),
        },
        "negative_effective_spread": negative_breakdown,
        "parent_inference": {
            "groups": len(groups),
            "group_size_histogram": histogram(group_sizes),
            "max_group_size": max(group_sizes) if group_sizes else 0,
        },
        "spread_ticks": {
            "quoted": percentiles(quoted),
            "effective": percentiles(effective),
        },
        "reconciliation": {
            "trade_physical_rows": trade_rows,
            "header": 1,
            "trades_parsed": trade_stats["parsed"],
            "matches_emitted": len(matches),
            "status_total": sum(by_status.values()),
            "every_physical_row_accounted": trade_rows
            == 1 + trade_stats["parsed"],
            "every_parsed_trade_has_one_outcome": trade_stats["parsed"]
            == len(matches)
            == sum(by_status.values()),
        },
    }

    report["roll_unstratified_ticks"] = {}
    for name, series in (
        ("roll_first_child", first_prices),
        ("roll_last_child", last_prices),
        ("roll_all_prints", all_prices),
    ):
        vols = [1.0] * max(0, len(series) - 1)
        status, pairs, cov, roll = re.roll_in_stratum(series, vols, [10.0], 0, tick, 500)
        report["roll_unstratified_ticks"][name] = {
            "status": status,
            "pairs": pairs,
            "covariance": cov,
            "roll_ticks": roll,
        }

    # ---- the matrix: convention x volatility stratum x quote-age stratum ----
    horizon = 64
    parent_vols = trailing_vol_events(last_prices, horizon)
    present = [v for v in parent_vols if v is not None]
    vol_boundaries = quantiles(present, (0.25, 0.75, 0.95)) if present else []
    # One attribute set per parent event, taken from the event's FIRST trade,
    # which is the trade whose quote the parent was joined against.
    parent_age = [g[0].quote_age for g in groups]
    parent_amb = [1 if g[0].quote_age == 0 else 0 for g in groups]
    parent_quoted = [
        aj.quoted_spread(g[0].quote) / tick if g[0].quote else None for g in groups
    ]
    parent_eff = [
        aj.effective_spread(g[0].trade, g[0].quote) / tick if g[0].quote else None
        for g in groups
    ]

    def cell(series, change_vol, change_age, vol_s, age_s, min_pairs=500):
        """One matrix cell. A pair is keyed on its LATER change for BOTH axes."""
        changes = [b - a for a, b in zip(series, series[1:])]
        pairs, idx = [], []
        for i in range(max(0, len(changes) - 1)):
            if i + 1 >= len(change_vol) or i + 1 >= len(change_age):
                continue
            v, a = change_vol[i + 1], change_age[i + 1]
            if v is None or a is None:
                continue
            if sum(1 for b in vol_boundaries if v >= b) != vol_s:
                continue
            if age_stratum(a, age_boundaries) != age_s:
                continue
            pairs.append((changes[i], changes[i + 1]))
            idx.append(i + 2)
        out = {"covariance_pairs": len(pairs), "trades": len(idx)}
        if len(pairs) < min_pairs:
            out.update({"status": "fail_closed", "roll_ticks": None, "covariance_sign": None})
            return out
        ma = sum(p[0] for p in pairs) / len(pairs)
        mb = sum(p[1] for p in pairs) / len(pairs)
        cov = sum((a - ma) * (b - mb) for a, b in pairs) / len(pairs)
        eff = [parent_eff[j] for j in idx if j < len(parent_eff) and parent_eff[j] is not None]
        qu = [parent_quoted[j] for j in idx if j < len(parent_quoted) and parent_quoted[j] is not None]
        amb = [parent_amb[j] for j in idx if j < len(parent_amb)]
        out.update(
            {
                "status": "matched" if cov < 0 else "unavailable",
                "covariance_sign": "negative" if cov < 0 else "non_negative",
                "roll_ticks": (2.0 * math.sqrt(-cov) / tick) if cov < 0 else None,
                "quoted_spread_median": quantiles(qu, (0.5,))[0] if qu else None,
                "effective_spread_median": quantiles(eff, (0.5,))[0] if eff else None,
                "negative_effective_fraction": (
                    sum(1 for e in eff if e < 0) / len(eff) if eff else None
                ),
                "sequencing_ambiguous_fraction": (sum(amb) / len(amb)) if amb else None,
            }
        )
        return out

    age_strata = [ZERO_AGE] + sorted(
        {age_stratum(a, age_boundaries) for a in positive_ages}
    )
    change_vol = parent_vols[: max(0, len(last_prices) - 1)]
    change_age = parent_age[: max(0, len(last_prices) - 1)]
    report["matrix"] = {
        "volatility_horizon_parent_events": horizon,
        "volatility_boundaries": vol_boundaries,
        "age_boundaries_positive_only": age_boundaries,
        "cells": {
            f"roll_last_child|vol{v}|{a}": cell(last_prices, change_vol, change_age, v, a)
            for v in range(len(vol_boundaries) + 1)
            for a in age_strata
        },
    }

    print(json.dumps(report, indent=1))
    if not report["reconciliation"]["every_parsed_trade_has_one_outcome"]:
        raise FailClosed("reconciliation failed: not every parsed trade has exactly one outcome")
    if not report["reconciliation"]["every_physical_row_accounted"]:
        raise FailClosed("reconciliation failed: physical rows do not account")
    return report


def main():
    if len(sys.argv) == 2 and sys.argv[1] == "selftest":
        cross_check_against_fixture()
        print("streaming join agrees with the pinned as-of contract")
        return
    if len(sys.argv) != 4 or sys.argv[1] != "smoke":
        raise SystemExit("usage: binance_spread.py smoke <trades.zip> <bookTicker.zip> | selftest")
    smoke(sys.argv[2], sys.argv[3])


if __name__ == "__main__":
    main()
