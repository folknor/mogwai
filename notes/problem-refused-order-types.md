# PROBLEM: the venue refuses the order types the user actually trades with

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. Related to
`notes/problem-order-book.md` but not the same question: that document settles
HOW an order fills, this one asks WHICH orders the venue accepts. It resolved to
a seeded, volatility-scaled fill band and no book at all, which removes this
document's hardest obstacle - a triggered stop now has a defensible fill price,
so the remaining work here is the order-type surface itself rather than the
execution model under it.

## What the user wants

The user trades MNQ, MES, BTC, SOL and ETH, and the normal shape for those is a
position with a protective stop leg. mogwai is the venue they forward-test
against. Today it cannot accept a stop of any kind, so the strategies they
actually run cannot be exercised on the only venue available for running them.
They have not asked for a specific order-type list; what they want is for the
venue to stop being the reason a real strategy shape is untestable.

## The observation

`mogwai-adapter` refuses every conditional order type at conversion, with
`SUBMIT_FAILED: unsupported order type <T>`, and the bridge fails closed rather
than trading unprotected. Failing closed is correct. An earlier draft went
further and called the REFUSAL itself correct "given a bookless venue", on the
grounds that a stop has nothing to rest against and no trigger to watch. That is
too strong, and this document refutes it below: a stop can trigger from TRADED
prices, which is what the penetration gate already does, and the account-owned
sweeper is already the loop a trigger needs. What a bookless venue genuinely
lacks is a defensible FILL after the trigger, not the trigger. So the refusal is
a defensible response to an unsolved fill problem rather than a necessary
consequence of having no book.

`reference/architecture.md` already records the consequence in full, and states
it as a documented coverage hole rather than pending work:

> a nautilus/broadarrow strategy whose protective leg is a stop-MARKET (the
> shape `ba man execution` recommends when bounding loss matters) cannot be
> forward-tested on MOGWAI at all, and MOGWAI is the only keyless venue
> available for forward testing.

The suggested replacement is a protective LIMIT, "remembering that a bookless
venue fills it instantly at its own price" - which is to say the replacement
does not model a stop at all. A protective limit that fills instantly is not a
stop-loss under any reading; it is a resting order at a price the market has
not reached.

## The standing decision this now conflicts with

On 2026-07-31 broadarrow's standing note recorded that the protocol owes no
order-type growth beyond Market and Limit, and that the once-floated
market-if-touched extension is dead - the triggering Pine shape being invalid
on TradingView and nautilus being unable to rest an MIT faithfully. That
decision is captured in `notes/todo.md` and cited in the architecture
reference.

Two things about it have changed. It was made on the premise that the gap was a
nuisance for one order shape, and the user's own trading makes it structural:
every instrument they named is routinely traded with a protective stop.

More importantly, broadarrow is INCIDENTAL. mogwai implements nautilus's
`DataClient`/`ExecutionClient` traits and registers factories for the `MOGWAI`
venue; it is a nautilus adapter, and broadarrow is one consumer that happens to
be the only one today. So that note records a consumer's preference, not a
constraint on the venue, and what actually determines the owed surface is what
nautilus strategies emit. Reopening it does not require their consent, though
telling them is courteous.

## What the consumer surface actually contains

Scoping this to stop-market and stop-limit, as an earlier draft did, understates
it. broadarrow's own execution reference maps `Market`, `Limit`, `StopMarket`,
`StopLimit` and `TrailingStopMarket`, and describes two-leg protective brackets
where both legs must remain present, implemented as separate reduce-only orders
rather than a venue-side OCO list.

That matters in three ways the earlier draft missed:

- **Trailing stops need venue-side state** - a per-tick high-water mark - and
  are materially more work than a fixed trigger.
- **Protective pairs need SOMETHING that stops both legs filling.** What that
  something is, is open. Note what broadarrow does today, as evidence rather
  than as a constraint: it emits two orders that are NOT venue-linked, both
  reduce-only, and expects the engine to clamp or cancel the sibling once the
  position is exhausted. So the minimum the venue owes to accept that shape is
  reduce-only semantics, not a venue OCO abstraction - but since mogwai is a
  nautilus adapter rather than a broadarrow accessory, what it OUGHT to support
  is whatever nautilus expresses, which includes order lists and contingency
  types the current consumer happens not to use.
- **Reservations double-count.** Two protective legs against one position both
  reserve the held asset unless the ledger understands that they are exclusive.
- **`position_id` is dropped.** nautilus carries it on submission and the
  adapter discards it when building the wire order. Independent of any consumer,
  that decides whether opposing orders net or hedge.

The wire has none of the fields any of this needs: one optional price, no
trigger price, no trigger type, no reduce-only flag, no trailing geometry, no
linkage metadata. Adding two enum variants would not make a real strategy shape
testable.

## A fifth option nobody enumerated

nautilus can emulate conditional orders client-side: `emulation_trigger` and
`OrderEmulator` hold the order in the strategy's own process and release a
market order to the venue when the trigger fires. Several of its own adapters
set it. That would make protective stops testable with no protocol growth and no
book at all.

It is probably the wrong answer here, but the reason needs stating precisely
because an earlier draft overstated it. The claim was that under emulation "no
havoc can delay, drop or reject it". That is false: the emulator releases a
MARKET order when the trigger fires, and havoc reaches that order, its ack and
its fill exactly as it reaches any other. What emulation puts beyond havoc's
reach is the venue-side RESTING and TRIGGERING behaviour specifically - the
protective leg never exists at the venue, so nothing can delay its acceptance,
reject it on arrival, drop it while it rests, or fire it late. Since exercising
the live path is the entire reason this venue exists, that is still a real loss,
and it is the loss to argue from. The option should be rejected in writing on
those grounds rather than left unmentioned.

## What a stop needs that does not exist

- **A trigger price source.** Stops trigger off the market, and mogwai's only
  market signal is its trade tape - `/quotes` is empty. Triggering off traded
  prices is possible today and is exactly the deviation the penetration gate
  already had to make; triggering off a quote needs at minimum a synthetic
  top-of-book.
- **Trigger evaluation over time.** A resting order that must be watched
  against arriving ticks. This machinery now EXISTS: the account-owned sweeper
  landed 2026-08-02 (`b8031d8`) walks the tape per account and re-evaluates
  resting orders against it, which is structurally the same loop a stop trigger
  needs.
- ~~**A defensible fill after the trigger.**~~ SUPPLIED by the fill model. A
  triggered stop-market becomes a market order, and the objection was that a
  bookless venue fills market orders at their own submitted price, which for a
  stop is meaningless. Under `notes/problem-order-book.md` a market order takes
  its own seeded, volatility-scaled band, so the slippage is defensible without
  any depth to walk. Real stops slip, and the band is what makes this one slip.

  What the band's WIDTH should be for a triggered stop is a spec-level scale
  question, and the measurement that would inform it is still owed: price SPAN
  per inferred match event has never been computed. The sweep tail quoted
  elsewhere (up to 2,213 aggTrade ROWS in one inferred event on BTC) counts rows
  rather than distinct prices, so it does not establish how far a marketable
  order actually walks. One probe extension over archives already on disk would
  settle it. Until then the slippage magnitude is an unquantified mechanism
  rather than a measured one.

## What must be decided

1. **Which types and which SHAPES the venue accepts.** Stop-market and
   stop-limit at minimum, given the user's trading; trailing stops and
   two-leg brackets with reduce-only or OCO linkage are what the consumer
   actually emits, and a decision to omit them is a decision that real strategy
   shapes stay untestable. Market-if-touched was explicitly killed and stays
   killed unless re-argued.
2. ~~**What the trigger reads.**~~ SETTLED: the traded price. There is no quote
   and there is not going to be one, and the fill model reads traded prices too,
   so the venue answers the predicate question the same way everywhere - which
   also retires the deviation the penetration gate had to declare against RFC
   4631's quote predicate.
3. ~~**What a triggered market order fills at.**~~ SETTLED by
   `notes/problem-order-book.md`: its own seeded, volatility-scaled band, the
   same mechanism a limit order uses. The options an earlier draft listed - a
   top-of-book price, a swept average over generated depth, or a separately
   declared slippage model - all presumed structure the venue is not building.
   Filling at the submitted trigger price would still be a lie of the class the
   queue-ahead refusal rejected; the band is what prevents it.
4. **Whether the standing no-growth decision is reopened**, and if so, whether
   broadarrow is consulted first - it is their note, and their consumer.
5. **Which havoc arms extend to a conditional order.** The argument against
   client-side emulation is precisely that the venue must SEE the protective leg
   so havoc can reach it - and then no document says which arms apply to a
   trigger. Delayed trigger, rejected trigger, dropped trigger conversion, and
   how submit-time and trigger-time divergences compose are all unasked.
6. **Lifecycle of a resting conditional.** Whether a trigger price can be
   amended, what cancel means before versus after triggering, what
   `QueryOrders` must report for one, and what happens to a resting stop when
   its instance dies or its account is reaped.
7. **The adjacent flags.** Reduce-only as a standalone property rather than only
   as bracket linkage, time-in-force including GTD, and post-only. The engine's
   `pending_scans` already filters on order type AND time-in-force, so the
   surface is wider than the trigger itself.

## What this document does not decide

How much book to build, which is the sibling document, though decisions 3 and 5
here are constrained by its answer. Nor arrival rate or profiles. Queue
position stays refused on its own measured grounds and is not reopened by
anything here.

## Known cost, not yet priced

Order-type growth touches the wire protocol, the adapter's conversion layer,
the engine's order model and every admission and sizing bound that enumerates
order types - `mogwai-protocol`'s size model charges worst-case bytes per
message shape. The engine's `pending_scans` already filters on order type and
time-in-force, so the resting-order machinery has a natural place to grow. The
smoke harness has no conditional-order scenario at all.
