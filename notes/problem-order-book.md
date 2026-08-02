# PROBLEM: the venue has no book, and the things it therefore cannot do are no longer edge cases

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. This is the only
document that revisits a founding assumption, and a sequencing correction from
an earlier draft: it resolves BEFORE the cadence document rather than after.
Under a matching venue the generator emits parent taker arrivals and the wire
prints fall out of matching, so this document decides what a trade IS and the
cadence document cannot pick a rate until it has.

The central question is also narrower than an earlier draft made it. The user
has answered the execution-semantics half: client orders should REST in a book
and be consumed by arriving flow, rather than being filled synthetically on
submit. What remains open is where the liquidity comes from and how much of a
book is exposed.

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

| | BTC | ETH | SOL |
|---|---|---|---|
| match events/sec | 5.84 | 6.78 | 1.94 |
| mean aggTrade rows per event | 2.25 | 1.75 | 1.02 |
| max rows in one event | **2,213** | **832** | **268** |
| events that are a single row | 76.5% | 77.4% | 98.6% |
| raw fills per event (rate ratio) | 8.5 | 6.9 | 6.4 |

Two cautions on reading that table, both of which an earlier draft got wrong.

**The rows are aggTrades, not price levels.** aggTrades merge fills at one price
and side, so a multi-row event spans multiple prices - but the probe counts
rows, not distinct prices, and has never verified the latter. "One taker swept
2,213 levels" is unsupported by this measurement; establishing it needs distinct
price count, price span and side consistency per inferred event.

**The last row is a different quantity and it inverts the story for SOL.** SOL
is 98.6% single-ROW while still averaging 6.4 raw fills per event, which means
many makers at ONE price - depth at a level rather than breadth across levels.
BTC's flow is broad; SOL's is deep. A ladder sized from the row counts alone
would get SOL badly wrong, and depth-per-level is exactly the quantity
queue-ahead was refused for want of.

**And the grouping is inferred, not identified.** The probe groups aggTrades
that share a microsecond timestamp. That is strong evidence of one match event -
at these rates a chance collision is essentially impossible - but there is no
taker-order id in the data, so a group is a hypothesis rather than a fact.

## Two independent axes, not one spectrum

An earlier draft listed four options as a single ladder. They are two separate
choices, and conflating them hid that "fitted depth" is not an alternative to
"a matching engine" - it is one possible liquidity SOURCE for one.

**Axis A, market-state fidelity: what exists to match against.**

1. Trades only - what the venue has today.
2. Synthetic top-of-book derived from the trade tape. Gives quotes and makes
   stop triggers well-defined. No depth, no queue.
3. A generated shallow ladder. Enables sweeps and queue position, but the depth
   profile is invented unless fitted - which is the objection that sank
   queue-ahead on 2026-08-02.
4. Fitted depth. Needs a book corpus. Binance publishes depth and bookTicker
   data, though NOT through the downloader vendored at
   `research/binance-public-data/` - its python directory carries aggTrade,
   trade and kline only, so acquiring it is unscoped work.

**Axis B, execution semantics: what happens to a client order.**

1. Synthetic fills on submit - today's behaviour at `penetration_ticks = 0`.
2. Account-local triggering - the order rests and the tape decides when it
   executes, which is what the penetration gate landed on 2026-08-02 already
   does.
3. Matching - the order rests IN the book and is consumed by arriving flow.

The user has chosen B3, and has explicitly ruled out orders of different
accounts matching each other. So the venue matches a client's orders against
exogenous liquidity, and remains single-tenant. That half is SETTLED and is not
reopened below.

That leaves axis A open, and A1 is excluded because B3 needs something to match
against.

**A2 is not excluded, contrary to an objection raised in review.** The objection
was that top-of-book without depth or displayed quantity has nothing to allocate
a fill against, so it cannot support B3. The user's answer is that the
allocation does not have to come from modelled depth: whether a resting order
fills, and how much of it, can be computed from what the tape actually did -
how far price traded through the level, how much volume arrived - with a random
component and a probability of success. That is the shape of nautilus's own
`FillModel` and of what RFC 4631 calls a probabilistic fill model, and it
composes with what already exists here: the penetration gate landed on
2026-08-02 is the deterministic skeleton of exactly that, and the missing
dimensions are allocation probability and partial quantity rather than a
rewrite.

What that inherits is a grounding obligation. A fill probability chosen from
nowhere is invented microstructure with better manners, which is the objection
that closed queue-ahead. The difference is that fill outcomes are in principle
measurable from trade data in a way resting depth is not, so grounding may be
reachable here where it was not there. Whether it actually is, is open.

One correction an earlier draft got wrong: stops do NOT simply fall out of
matching. A conditional order normally sits outside the visible book until its
trigger fires, so it needs trigger semantics of its own regardless of how
sophisticated the matching is.

## What must be decided

1. **Which point on axis A.** A2 with a probabilistic allocation model, A3 with
   a generated ladder, or A4 with fitted depth. A2 is the cheapest and the user
   has argued it is sufficient; A3 is the one that reintroduces invented
   microstructure; A4 needs a book corpus that does not exist here and cannot be
   fetched with the vendored downloader.
2. **How the allocation model is grounded.** Under A2 this is the whole
   question: what determines fill probability and partial quantity from
   penetration depth and arriving volume, and whether those parameters are
   measured from trade data, fitted, or declared. A declared model is
   acceptable if it says so; an unlabelled one is the queue-ahead failure again.
3. **What reaches the wire.** Whether the book is internal only or exposed -
   whether `/quotes` finally fills in, whether the protocol grows snapshots or
   deltas, and what a consumer may subscribe to. A matching venue and a public
   depth feed are separate deliverables and only the first is implied by B3.
4. **Whether client fills join the public tape.** If a client's fill prints, two
   instances at the same seed diverge the moment either trades, which changes
   what a path is (`notes/problem-seeds-and-paths.md`) and what determinism
   means. If it does not print, the client is trading against a market that
   cannot see it.
5. **Whether determinism-given-identical-client-input replaces byte-identical
   determinism** as the contract. Today the generator is a pure function of its
   state and the checkpointed seek depends on it. A book that client orders
   mutate is state the generator does not own.
6. **What happens to the synthetic-fill path.** The penetration gate, the
   account-owned sweeper, the scan frontier and the golden fill distribution -
   all landed 2026-08-02 - either become the skeleton of the allocation model
   under A2, or are superseded under A3/A4. That is a real decision with a lot
   of committed work behind it.
7. **Self-trade within one account.** Accounts never match each other by the
   user's ruling, but one account can hold both sides. Whether that fills, is
   prevented, or is simply impossible under the chosen model needs stating.

## What this document does not decide

The order-type surface itself - which conditionals, their trigger semantics,
what a stop does on a gap - which is the sibling document. Note the dependency
runs BOTH ways and the index records it: the order surface constrains this
document's choice, because a market-state model has to support whatever
triggers, fills and reduce-only behaviour the venue owes, while the
implementation of those types lands after this resolves. Nor does this decide
the arrival rate or per-instrument profiles; note though that under B3 the
arrival process feeds matching rather than the wire directly, which changes what
"a trade" means in `notes/problem-trade-cadence.md`.

## Known cost, explicitly not a decision input

Per the user's standing instruction, resource cost does not shape this.
Recorded because the blast radius is large rather than because it argues
against: essentially every design decision landed on 2026-08-02 gets revisited,
and several deliberate refusals reopen. The order-type refusal
disappears; the quote-predicate deviation disappears; queue-ahead stops being
declined for want of L2. Determinism is the sharpest technical risk: the
byte-identical golden stream and the checkpointed seek both rest on the
generator being a pure function of its state, and a book that client orders
mutate is state the generator does not own.
