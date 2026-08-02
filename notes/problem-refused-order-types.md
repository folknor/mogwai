# PROBLEM: the venue refuses the order types the user actually trades with

Expanded from what would otherwise be a `notes/todo.md` entry. Related to
`notes/problem-order-book.md` but not the same question: that document asks how
much book to build, this one asks what execution surface the venue owes its
consumer. A book is one way to pay for it, not the only one, and the debt
exists whether or not a book lands.

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
than trading unprotected. The refusal itself is correct given a bookless venue:
a stop has nothing to rest against and no trigger to watch.

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

It was made on the premise that the gap was a nuisance for one order shape. The
user's own trading makes it structural: every instrument they named is
routinely traded with a protective stop. Reopening a recorded decision needs to
be deliberate and the reasons written down, which is the main reason this is a
problem statement rather than a spec.

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
- **A defensible fill after the trigger.** A triggered stop-market becomes a
  market order, and a bookless venue fills market orders at their own submitted
  price - which for a stop is meaningless. Real stops slip, and the sweep tail
  measured in the sibling document (up to 2,213 prints in one match event on
  BTC) is exactly the event that produces the slippage. Without a book there is
  no principled price to fill at; with only top-of-book there is a price but no
  depth to walk.

## What must be decided

1. **Which types the venue accepts.** Stop-market and stop-limit at minimum,
   given the user's trading. Market-if-touched was explicitly killed and stays
   killed unless re-argued.
2. **What the trigger reads** - traded price, or a quote that does not yet
   exist. This is the same predicate question the penetration gate answered
   under duress, and answering both the same way is worth something.
3. **What a triggered market order fills at.** The honest options are a
   top-of-book price, a swept average over generated depth, or an explicitly
   declared slippage model. Filling at the submitted trigger price would be a
   lie of the same class the queue-ahead refusal rejected.
4. **Whether the standing no-growth decision is reopened**, and if so, whether
   broadarrow is consulted first - it is their note, and their consumer.
5. **Whether this can land before the book question resolves.** A
   trade-triggered stop filling at top-of-book is buildable on option 1 of the
   book document; a faithful one probably is not.

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
