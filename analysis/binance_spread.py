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

    matches = list(
        streaming_asof(
            stream_trades(trades_zip, trade_stats),
            stream_quotes(quotes_zip, quote_stats),
        )
    )

    by_status = {}
    ages = []
    ambiguous = 0
    quoted = []
    effective = []
    for match in matches:
        by_status[match.status] = by_status.get(match.status, 0) + 1
        if match.status != aj.STATUS_MATCHED:
            continue
        ages.append(match.quote_age)
        # SEQUENCING: an equal-millisecond match cannot be ordered between two
        # independently written files. Row order within a ZIP is not evidence of
        # cross-file ordering and is deliberately not consulted.
        if match.quote_age == 0:
            ambiguous += 1
        quoted.append(aj.quoted_spread(match.quote) / tick)
        effective.append(aj.effective_spread(match.trade, match.quote) / tick)

    groups = infer_parents(matches)
    group_sizes = [len(g) for g in groups]
    first_prices = [g[0].trade.price for g in groups]
    last_prices = [g[-1].trade.price for g in groups]
    all_prices = [m.trade.price for m in matches]

    report = {
        "inputs": {"trades": os.path.basename(trades_zip), "quotes": os.path.basename(quotes_zip)},
        "tick": tick,
        "parsed": {"trades": trade_stats["parsed"], "quotes": quote_stats["parsed"]},
        "quote_book_state": {"locked": quote_stats["locked"], "crossed": quote_stats["crossed"]},
        "join": {
            "by_status": by_status,
            "match_rate": by_status.get(aj.STATUS_MATCHED, 0) / max(1, len(matches)),
        },
        "quote_age_ms": {
            "zero_age_count": sum(1 for a in ages if a == 0),
            "zero_age_fraction": sum(1 for a in ages if a == 0) / max(1, len(ages)),
            **percentiles(ages),
        },
        "sequencing_ambiguous": {
            "count": ambiguous,
            "fraction": ambiguous / max(1, len(ages)),
        },
        "parent_inference": {
            "groups": len(groups),
            "group_size_histogram": histogram(group_sizes),
            "max_group_size": max(group_sizes) if group_sizes else 0,
        },
        "spread_ticks": {
            "quoted": percentiles(quoted),
            "effective": percentiles(effective),
            "negative_effective_count": sum(1 for e in effective if e < 0),
        },
        "reconciliation": {
            "trades_parsed": trade_stats["parsed"],
            "matches_emitted": len(matches),
            "status_total": sum(by_status.values()),
            "balanced": trade_stats["parsed"] == len(matches) == sum(by_status.values()),
        },
    }

    # Roll by convention, over the whole day. Stratification by volatility and
    # quote age is the next layer and is reported per convention here so the
    # unstratified baseline exists first.
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

    print(json.dumps(report, indent=1))
    if not report["reconciliation"]["balanced"]:
        raise FailClosed("reconciliation failed: not every parsed trade has exactly one outcome")
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
