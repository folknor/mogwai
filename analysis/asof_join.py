"""Strict as-of join of quotes onto trades, plus its adversarial contract test.

This module deliberately knows NOTHING about Binance column positions. It joins
TYPED RECORDS, so a defect in the temporal join and a defect in archive parsing
cannot be confused for one another: parse into these records in one place, test
the join semantics here, and a failure tells you which of the two is wrong.

The join contract, stated before any real file was inspected so that a file
discovery cannot quietly redefine it:

  - Eligibility is on TRANSACTION time, never on publication/event time. The two
    are different values and a venue may publish long after it matched.
  - A quote is eligible when `quote.transaction_time <= trade.time`. Equality at
    the boundary IS accepted.
  - A future quote is NEVER selected, even when it is closer in absolute time.
    Choosing the nearest quote in either direction permits lookahead, and
    lookahead in a spread study manufactures the result.
  - Among eligible quotes the NEWEST wins.
  - Quote age is exactly `trade.time - quote.transaction_time`.
  - Ties in transaction time resolve only through a documented sequence rule
    (update id). Where that rule cannot order them, the match is LABELLED
    ambiguous rather than silently guessed.
  - Trades before the first quote, and quotes staler than the caller's bound,
    fail closed into explicitly named categories - never dropped, never
    back-filled.
  - Quotes outside the retained coverage interval are not borrowed.
  - Input order does not affect the result when sequence metadata makes the true
    order unambiguous.

Run the contract test with:

    python3 analysis/asof_join.py selftest
"""

import sys
from typing import NamedTuple, Optional


class Quote(NamedTuple):
    """One book-top observation.

    `transaction_time` is when the venue matched; `event_time` is when it
    published. They are separate fields on purpose - conflating them is a
    lookahead bug that reads as a latency measurement.
    """

    transaction_time: int
    event_time: int
    update_id: int
    bid: float
    ask: float


class Trade(NamedTuple):
    time: int
    price: float
    qty: float
    is_buyer_maker: bool


class Match(NamedTuple):
    """A joined trade, or a named reason there is no join.

    `status` is one of the STATUS_* constants. Every trade produces exactly one
    Match, so counts always reconcile and nothing is lost by filtering.
    """

    trade: Trade
    quote: Optional[Quote]
    quote_age: Optional[int]
    status: str


STATUS_MATCHED = "matched"
STATUS_AMBIGUOUS_SEQUENCE = "ambiguous_sequence"
STATUS_NO_QUOTE_BEFORE = "no_quote_before"
STATUS_STALE = "stale_beyond_bound"
STATUS_OUTSIDE_COVERAGE = "outside_coverage"


def aggressor_sign(trade):
    """+1 buyer initiated, -1 seller initiated.

    `is_buyer_maker` true means the BUYER was resting, so the seller crossed the
    spread and the trade is seller initiated. Getting this backwards flips every
    effective spread's sign, which is why it is one named function rather than
    an inline expression repeated at each use.
    """
    return -1 if trade.is_buyer_maker else 1


def effective_spread(trade, quote):
    """`2 * sign * (price - mid)`. Never clamped.

    A negative value is evidence of a stale quote, sequencing ambiguity or price
    improvement. Clamping it to zero erases exactly the diagnostic that says the
    join is wrong.
    """
    mid = (quote.bid + quote.ask) / 2.0
    return 2.0 * aggressor_sign(trade) * (trade.price - mid)


def quoted_spread(quote):
    return quote.ask - quote.bid


def update_ids_monotonic(quotes):
    """Whether update ids strictly increase in transaction-time order.

    When they do, a transaction-time tie has a documented resolution and the
    join is deterministic. When they do not, ties are ambiguous and are labelled
    as such - the alternative is picking one silently and calling it data.
    """
    # Sorted by (transaction_time, update_id), not by time alone: a time-only
    # sort leaves ties in INPUT order, which would make this answer depend on
    # how the caller happened to stack the file.
    ordered = sorted(quotes, key=lambda q: (q.transaction_time, q.update_id))
    return all(a.update_id < b.update_id for a, b in zip(ordered, ordered[1:]))


def asof_join(trades, quotes, coverage=None, max_age=None):
    """Join the newest quote at or before each trade.

    `coverage` is an optional `(start, end)` interval, inclusive of both ends,
    outside which quotes are not borrowed and trades are not matched.
    `max_age` bounds acceptable staleness; beyond it a trade fails closed into
    STATUS_STALE rather than carrying a quote nobody should trust.
    """
    monotonic = update_ids_monotonic(quotes)
    # Sorting by (transaction_time, update_id) is what makes the result
    # independent of input order whenever the sequence metadata is unambiguous.
    ordered = sorted(quotes, key=lambda q: (q.transaction_time, q.update_id))
    if coverage is not None:
        start, end = coverage
        ordered = [q for q in ordered if start <= q.transaction_time <= end]

    results = []
    for trade in trades:
        if coverage is not None and not (coverage[0] <= trade.time <= coverage[1]):
            results.append(Match(trade, None, None, STATUS_OUTSIDE_COVERAGE))
            continue

        # Eligible means transaction_time <= trade.time. Equality is accepted;
        # a strictly-less rule would discard same-instant matches that are
        # genuinely available, and a nearest-in-either-direction rule would
        # reach into the future.
        eligible = [q for q in ordered if q.transaction_time <= trade.time]
        if not eligible:
            results.append(Match(trade, None, None, STATUS_NO_QUOTE_BEFORE))
            continue

        newest_time = eligible[-1].transaction_time
        tied = [q for q in eligible if q.transaction_time == newest_time]
        chosen = eligible[-1]
        age = trade.time - chosen.transaction_time

        if len(tied) > 1 and not monotonic:
            # Several quotes share the winning instant and nothing orders them.
            # Report the ambiguity; do not pick one and pretend.
            results.append(Match(trade, chosen, age, STATUS_AMBIGUOUS_SEQUENCE))
            continue
        if max_age is not None and age > max_age:
            results.append(Match(trade, chosen, age, STATUS_STALE))
            continue
        results.append(Match(trade, chosen, age, STATUS_MATCHED))
    return results


# ---------------------------------------------------------------------------
# Adversarial contract test.
#
# Values are chosen so that every off-by-one and every wrong-column choice
# produces a DIFFERENT answer: transaction and event times differ, quote ages
# are all distinct, update ids are far from timestamps, and bids and asks are
# unique per quote. A fixture where two mistakes agree teaches nothing.
# ---------------------------------------------------------------------------

def _q(txn, event, uid, bid, ask):
    return Quote(txn, event, uid, bid, ask)


def _fixture():
    # Transaction times deliberately trail event times by varying amounts, so a
    # join that reads the wrong column selects a different quote.
    # Ids are spaced with GAPS so a later test can insert a tie-breaking quote
    # that keeps global monotonicity. Consecutive numbering would have made the
    # resolvable-tie case unrepresentable, which is a fixture defect that reads
    # as a code defect.
    return [
        _q(1000, 1500, 7001, 100.01, 100.03),
        _q(2000, 2900, 7002, 200.02, 200.06),
        _q(3000, 3100, 7003, 300.03, 300.09),
        _q(5000, 5050, 7010, 500.05, 500.15),
    ]


def _check(name, actual, expected):
    if actual != expected:
        raise AssertionError(f"{name}: expected {expected!r}, got {actual!r}")
    print(f"  ok  {name}")


def selftest():
    quotes = _fixture()
    print("as-of join contract")

    # Equality at the boundary is ACCEPTED.
    m = asof_join([Trade(2000, 1.0, 1.0, False)], quotes)[0]
    _check("equality at boundary accepted", m.quote.update_id, 7002)
    _check("age is zero at exact boundary", m.quote_age, 0)

    # A future quote is never selected even when far closer in absolute time:
    # trade at 2999 is 1 ns from the 3000 quote and 999 from the 2000 quote.
    m = asof_join([Trade(2999, 1.0, 1.0, False)], quotes)[0]
    _check("future quote never selected", m.quote.update_id, 7002)
    _check("age measured from transaction time", m.quote_age, 999)

    # The NEWEST eligible quote wins, not the first or the nearest by event time.
    m = asof_join([Trade(4999, 1.0, 1.0, False)], quotes)[0]
    _check("newest eligible wins", m.quote.update_id, 7003)
    _check("age from newest eligible", m.quote_age, 1999)

    # Eligibility uses TRANSACTION time. Quote 7002 published at 2900; a trade at
    # 2500 must still see it, because the venue matched it at 2000.
    m = asof_join([Trade(2500, 1.0, 1.0, False)], quotes)[0]
    _check("eligibility on transaction not event time", m.quote.update_id, 7002)

    # Trades before the first quote fail closed into a named category.
    m = asof_join([Trade(999, 1.0, 1.0, False)], quotes)[0]
    _check("trade before first quote", m.status, STATUS_NO_QUOTE_BEFORE)
    _check("no quote carried", m.quote, None)

    # Staleness fails closed rather than carrying an untrustworthy quote.
    m = asof_join([Trade(4999, 1.0, 1.0, False)], quotes, max_age=100)[0]
    _check("stale beyond bound", m.status, STATUS_STALE)
    _check("stale still reports its age", m.quote_age, 1999)

    # Duplicate transaction times with NON-monotonic ids are ambiguous.
    ambiguous = quotes + [_q(3000, 3200, 6999, 301.03, 301.09)]
    m = asof_join([Trade(3500, 1.0, 1.0, False)], ambiguous)[0]
    _check("tie without ordering is labelled", m.status, STATUS_AMBIGUOUS_SEQUENCE)

    # The same tie WITH monotonic ids resolves deterministically. 7004 sits
    # between 7003 (txn 3000) and 7010 (txn 5000), so ids still increase in
    # transaction order and the sequence rule applies.
    resolved = quotes + [_q(3000, 3200, 7004, 301.03, 301.09)]
    m = asof_join([Trade(3500, 1.0, 1.0, False)], resolved)[0]
    _check("tie with ordering resolves", m.status, STATUS_MATCHED)
    _check("tie resolved to highest id", m.quote.update_id, 7004)

    # Coverage boundaries are not crossed to borrow a quote.
    m = asof_join([Trade(2500, 1.0, 1.0, False)], quotes, coverage=(2500, 6000))[0]
    _check("no borrowing across coverage start", m.status, STATUS_NO_QUOTE_BEFORE)
    m = asof_join([Trade(9000, 1.0, 1.0, False)], quotes, coverage=(0, 6000))[0]
    _check("trade outside coverage", m.status, STATUS_OUTSIDE_COVERAGE)

    # Input order must not matter when sequence metadata is unambiguous.
    shuffled = [quotes[2], quotes[0], quotes[3], quotes[1]]
    trades = [Trade(t, 1.0, 1.0, False) for t in (1000, 2500, 3500, 5500)]
    _check(
        "input order irrelevant",
        asof_join(trades, shuffled),
        asof_join(trades, quotes),
    )

    # Aggressor sign and effective spread, including the unclamped negative.
    buyer_taker = Trade(3000, 300.09, 1.0, False)
    seller_taker = Trade(3000, 300.03, 1.0, True)
    _check("buyer taker sign", aggressor_sign(buyer_taker), 1)
    _check("seller taker sign", aggressor_sign(seller_taker), -1)
    q = quotes[2]
    _check("quoted spread", round(quoted_spread(q), 6), 0.06)
    _check("effective at ask", round(effective_spread(buyer_taker, q), 6), 0.06)
    _check("effective at bid", round(effective_spread(seller_taker, q), 6), 0.06)
    # Price improvement inside the mid gives a NEGATIVE effective spread, and it
    # is reported rather than clamped.
    improved = Trade(3000, 300.05, 1.0, False)
    _check("negative effective not clamped", round(effective_spread(improved, q), 6), -0.02)

    # Every trade produces exactly one Match, so counts reconcile.
    many = [Trade(t, 1.0, 1.0, False) for t in (500, 1000, 2999, 9999)]
    _check("one result per trade", len(asof_join(many, quotes, coverage=(0, 6000))), 4)

    print("as-of join contract: all checks passed")


def main():
    phase = sys.argv[1] if len(sys.argv) > 1 else "selftest"
    if phase != "selftest":
        raise SystemExit("usage: asof_join.py selftest")
    selftest()


if __name__ == "__main__":
    main()
