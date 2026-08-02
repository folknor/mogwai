# PROBLEM: the venue has no book, and the things it therefore cannot do are no longer edge cases

Expanded from what would otherwise be a `notes/todo.md` entry. This is the
largest of the arrival/profile/book documents and the only one that revisits a
founding assumption, so it is resolved last of the three - but note that its
answer changes what the other two are worth building.

## What the user wants

After seeing that real flow arrives as match events sweeping several price
levels at once, and that mogwai cannot represent that, the user said: I guess
we need a book. The immediate motivations are that the instruments they trade
(MNQ, MES, BTC, SOL, ETH) are normally traded with protective stop legs, which
this venue cannot accept at all; that the sweep structure visible in the data
is unrepresentable without levels; and that several refusals already on record
- queue position, quote-based fills - were refusals for want of depth rather
than for want of desire. What is NOT settled is how much book: whether client
orders should merely be TRIGGERED by book state, or should REST in it and be
consumed by arriving flow.

## The observation

`reference/architecture.md` states the founding assumption plainly: fills are
synthetic, there is no matching and no order book, and at the default
penetration setting a submit fills immediately and in full at its own price.
That assumption is load-bearing across the whole system, and it has been
producing refusals at an increasing rate:

- **Conditional order types are refused outright.** The adapter rejects
  stop-market, stop-limit and market-if-touched at conversion, because there is
  nothing to rest against and no trigger to watch. The architecture reference
  records the consequence: a nautilus strategy whose protective leg is a
  stop-market cannot be forward-tested on MOGWAI at all, and MOGWAI is the only
  keyless venue `ba forward` can use. This is a hole in the venue's core
  purpose, not a fidelity gap.
- **`/quotes` is permanently empty.** The penetration gate therefore had to be
  specified against TRADED prices rather than the quote predicate RFC 4631
  actually asks for - a deviation the spec had to state twice and carry into
  `reference/architecture.md`.
- **Queue position was measured and refused** on 2026-08-02 (`a9a12aa`),
  because there is no L2 anywhere in this project's lineage and a synthesized
  depth ladder would be invented microstructure sold as fitted realism.
- **Sweeps are unrepresentable.** Every synthetic trade is an independent
  arrival at one price.

## Measured: what the tape actually delivers

From `analysis/probe_binance_aggtrades.py` over June 2026 archives in
`research/market-data/` (see `notes/problem-trade-cadence.md` for provenance
and the microsecond caveat). Binance stamps every fill of one match event with
a single `transact_time`, so collapsing to distinct timestamps recovers the
real arrival and its multiplicity:

| | BTC | SOL |
|---|---|---|
| match events/sec | 5.84 | 1.94 |
| mean prints per event | 2.25 | 1.02 |
| max prints in one event | **2,213** | **268** |
| events that are a single print | 76.5% | 98.6% |

The tail is the point. Three quarters of match events are one print, and then
occasionally one taker order produces two thousand. That is a book being swept
through many levels at one instant, and it is precisely the event that trades
through a resting limit - the thing the penetration gate exists to model and
currently approximates with independent arrivals.

## The spectrum, and why the middle is the trap

1. **Synthetic top-of-book.** Derive a bid/ask around the trade tape. Gives
   quotes, makes stop triggers well-defined, unlocks the conditional order
   types. No depth, no queue, fills stay synthetic. Cheapest by a wide margin.
2. **A generated shallow ladder.** N levels around mid. Enables sweeps and
   queue position - but the depth profile is invented unless fitted, which
   walks straight back into the objection that sank queue-ahead. Most of the
   complexity, none of the grounding.
3. **Fitted depth.** Needs a book corpus. Binance publishes depth and
   bookTicker data, though NOT through the downloader vendored at
   `research/binance-public-data/` - its python directory carries aggTrade,
   trade and kline only, so acquiring depth is itself unscoped work.
4. **A true matching engine.** Client orders rest in the book; synthetic flow
   consumes them. Stops, queue position, partial fills and price improvement
   all fall out of one mechanism rather than being modelled separately. This is
   the largest change the project has contemplated.

1 and 4 are the coherent stopping points. 2 is named here only so it is
explicitly rejected rather than drifted into.

## What must be decided

1. **Triggered or resting.** Do client orders merely react to book state, or do
   they sit in it and get consumed? This single question separates option 1
   from option 4 and determines whether this is weeks or months.
2. **Where the book comes from.** Derived from the trade tape, generated from a
   fitted depth model, or replayed. Only the second and third need a corpus
   that does not currently exist here.
3. **What happens to the synthetic-fill path.** Under option 4 the penetration
   gate, the account-owned sweeper, the scan frontier and the golden fill
   distribution - all landed 2026-08-02 - are modelling something the matching
   engine would do natively. Whether they are kept as a bookless mode, adapted,
   or removed is a real decision with a lot of committed work behind it.
4. **Whether the generator must produce book events.** Generating a believable
   book is a materially harder modelling problem than generating a trade tape,
   and the committed fingerprint contains no depth at all.

## What this document does not decide

The order-type surface itself - which conditionals, their trigger semantics,
what a stop does on a gap - which is the sibling execution-semantics document
and only becomes answerable once this one picks a level. Nor the arrival rate
or per-instrument profiles, which are the other two siblings; note though that
if option 4 lands, the arrival process feeds a matching engine rather than the
wire directly, which changes what "a trade" even means in
`notes/problem-trade-cadence.md`.

## Known cost, not yet priced

Under option 4 essentially every design decision landed on 2026-08-02 gets
revisited, and several deliberate refusals reopen. The order-type refusal
disappears; the quote-predicate deviation disappears; queue-ahead stops being
declined for want of L2. Determinism is the sharpest technical risk: the
byte-identical golden stream and the checkpointed seek both rest on the
generator being a pure function of its state, and a book that client orders
mutate is state the generator does not own.
