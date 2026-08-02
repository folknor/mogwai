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
than for want of desire. What is NOT settled is how much book - specifically,
what exists for a client order to match AGAINST, which is axis A below. Whether
client orders rest or merely trigger IS settled (they rest: axis B, option 3);
an earlier version of this paragraph listed that as open, contradicting the
axis B section further down.

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

That measurement is a SHARED dependency, not a local gap. Price span per
inferred match event is simultaneously the depth-shape quantity this document
needs to choose between A3 and A4, and the slippage quantity
`notes/problem-refused-order-types.md` needs for its decision 3 (what a
triggered market order fills at). Both documents currently assert conclusions
that only this number can support. It is an extension to a probe that already
groups events, over archives already on disk, so it is the cheapest open
measurement in the set by a wide margin. Resolving it belongs at the spec
level, whichever of the two specs is written first.

**The last row is a different quantity and it inverts the story for SOL.** SOL
is 98.6% single-ROW while still averaging 6.4 raw fills per event, which means
many makers at ONE price - depth at a level rather than breadth across levels.
BTC's flow is broad; SOL's is deep. A ladder sized from the row counts alone
would get SOL badly wrong, and depth-per-level is exactly the quantity
queue-ahead was refused for want of.

**And the grouping is inferred, not identified.** The probe groups aggTrades
that share a microsecond timestamp. That is strong evidence of one match event,
and the arithmetic is worth stating rather than asserting: at 13.2 aggTrades per
second against microsecond resolution, two independent arrivals share a
microsecond with probability about 1.3e-5 per adjacent pair, against 55.5%
observed on BTC. Four orders of magnitude separate the coincidence hypothesis
from the observation. But there is no taker-order id in the data, so a group
remains a hypothesis rather than a fact.

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
5. Fitted top of book: a GROUNDED variant of 2, listed last so the A1-A4
   labels used elsewhere keep their meaning. Take best bid and ask from a
   bookTicker corpus rather than deriving a synthetic quote from the trade
   tape. Top-of-book data is structurally simpler than full depth diffs - best
   bid and ask updates, not a ladder with replenishment and cancellation
   dynamics - so it is the cheapest way to stop the SPREAD being a declared
   quantity.

   TWO CORRECTIONS to an earlier draft of this entry, both of which overstated
   it. First, this is NOT "one archive fetch": Binance's public-data
   documentation lists spot trades, aggTrades and klines, and does NOT list a
   historical spot bookTicker or depth archive, so sourcing it is unscoped work
   and may mean live collection, a licensed corpus, or accepting a futures
   corpus with the venue and asset-class mismatch that implies. Second, it does
   NOT reduce the declared surface to "one parameter". Top of book still leaves
   queue priority, hidden liquidity, replenishment, cancellation, order-size
   dependence and adverse selection unobserved. It shrinks what must be
   declared; it does not shrink it to one thing.

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
that closed queue-ahead.

An earlier draft answered that obligation by claiming fill outcomes are in
principle measurable from trade data in a way resting depth is not, so grounding
may be reachable here where it was not there. That claim does not survive
scrutiny in its strong form, and a spec author should not inherit it. **What
trade data constrains is an upper bound on fill, not a fill.** From trades alone
you recover how far price traded through a level and how much volume arrived
there. What you cannot recover is how much of that volume was AHEAD of your
order in the queue, and that fraction is exactly what decides whether your
resting order filled. There is no taker-order id, no maker id, and no depth, so
nothing in the corpus distinguishes a level that traded 100 units with you first
in line from the same level with you last.

So the probabilistic model does not escape the queue-ahead objection. It
RANDOMIZES it: an invented deterministic queue position is replaced by an
invented distribution over queue positions. That may well be the right trade,
and it is defensible under this project's own standard - a declared model is
acceptable if it says so - but it is a different claim from "grounding may be
reachable", and the difference is the whole honesty question. The residual free
parameter is the queue-ahead fraction, and it is DECLARED rather than fitted
unless something else grounds it.

Option A5 above is what could ground part of it. bookTicker gives real best bid
and ask, so the spread and the top-of-book dynamics stop being assumed, and the
declared surface shrinks from a whole fill model to one parameter.

**Effort asymmetry, since the natural objection is that this research is harder
than just building the book.** It is not, and the reason is that A4's research
is a SUPERSET of A2's rather than an alternative to it. A4 needs everything A2
needs, plus a depth corpus (full L2 diff streams are tens of GB per month per
symbol, against roughly 500 MB for a month of aggTrades), plus a fitted
limit-order-book model - ladder shape, replenishment, cancellation - which is
strictly harder statistics than a fill probability rather than easier, plus
price-time-priority matching in the engine, plus the determinism break this
document's cost section already names as the sharpest technical risk.

The decisive difference is termination. A2 can END at "here is the model, here
is what grounds each parameter, here is which one is declared", which is a
finite research session. A4 has no comparable stopping point, because the reason
to acquire depth data is to stop declaring things, so every unfitted assumption
reads as unfinished work. Stated as an absolute - "A4 cannot terminate because
nothing may be declared" - that is a methodological assertion rather than a
fact, and it is false in the strong form: every fitted book model retains
assumptions and declared boundaries too. The defensible version is about
EXPECTATION, not possibility. The honest counterweight remains that A2 has no
ground truth, so its model can be stated but never verified, while A4 is
expensive and checkable.

**This is not resolved here and should not be.** It is genuinely probability and
statistics work and it warrants its own research session, ahead of any spec.
What this document fixes is the terms: the bound above, the declared residual,
and A5 as the option that shrinks it.

One correction an earlier draft got wrong: stops do NOT simply fall out of
matching. A conditional order normally sits outside the visible book until its
trigger fires, so it needs trigger semantics of its own regardless of how
sophisticated the matching is.

## What must be decided

1. **Which point on axis A.** A2 with a probabilistic allocation model, A5 with
   the same model over a bookTicker-fitted top of book, A3 with a generated
   ladder, or A4 with fitted depth. A2 is the cheapest and the user has argued
   it is sufficient; A5 grounds the spread and shrinks - but does not eliminate
   - what A2 must declare, at the cost of sourcing a top-of-book corpus that is
   NOT in Binance's public archive; A3 is the one that reintroduces invented
   microstructure; A4 needs
   a book corpus that does not exist here and cannot be fetched with the
   vendored downloader.
2. **How the allocation model is grounded.** Under A2 or A5 this is the whole
   question: what determines fill probability and partial quantity from
   penetration depth and arriving volume, and whether those parameters are
   measured, fitted, or declared. Per the bound established above the queue-ahead
   fraction cannot be measured from trades at all, so the realistic question is
   how small the declared surface can be made rather than whether one exists. A
   declared model is acceptable if it says so; an unlabelled one is the
   queue-ahead failure again. THIS IS A RESEARCH SESSION IN ITS OWN RIGHT and
   precedes the spec.
3. **What reaches the wire.** Whether the book is internal only or exposed -
   whether `/quotes` finally fills in, whether the protocol grows snapshots or
   deltas, and what a consumer may subscribe to. A matching venue and a public
   depth feed are separate deliverables and only the first is implied by B3.
4. ~~**Whether client fills join the public tape.**~~ CLOSED by the user. They
   do NOT print. The reasoning is not a weighing of the two sides but an
   exclusion: the tape must be a deterministic function of (config, seed), and a
   client fill on the public tape makes it a function of client behaviour
   instead. The consequence the open version of this decision raised stands and
   is simply accepted - the client trades against a market that cannot see it,
   which is what a PRICE-TAKER simulator means. The same ruling excludes market
   impact generally: a client order of any size moves nothing, not as a
   simplification to be bounded by a participation limit, but because impact is
   incompatible with the determinism contract. State it in the spec so a
   consumer knows the venue makes no claim about size-dependent execution.
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
