# PROBLEM: every order fills instantly at its own price, and nothing decides whether it should have

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

**RESOLVED as to its central question.** The user has specified the fill model
directly and it needs no book. What remains open is one estimator and two
scalars, all of which are spec-level.

## The correction this document had to make about itself

Earlier drafts were titled "the venue has no book" and treated the founding
no-book assumption as the thing being reopened. That was wrong, and the error
has a traceable origin worth recording so it is not repeated.

The user described a BEHAVIOUR: client orders should rest and be consumed by
arriving flow, and accounts should never match each other. A drafting session
mapped that description onto an option it had labelled "B3 - Matching: the order
rests IN the book and is consumed by arriving flow", and then wrote "the user has
chosen B3". The label imported a book that was never asked for, and the document
spent thousands of words on how much of one to build. The user did not pick B3.
They never mentioned a book at all.

Two of this document's own conclusions came from the label rather than from the
requirement, and both dissolve with it. "A book that client orders mutate is
state the generator does not own" was called the sharpest technical risk here -
it does not arise, because no shared book exists. And the fill-allocation
problem, which consumed an entire section trying to ground a probability against
queue-ahead volume that trade data cannot observe, does not arise either.

The general lesson, since this is the second time a label has done unearned work
in these documents: when a user describes behaviour, record the behaviour. An
option list is a drafting convenience, and "the user chose option N" is an
attribution that must trace to the user actually choosing option N.

## What the user specified

A limit order gets a fill price RANGE predetermined at submit time. The engine
computes a band around the order's stated price, scaled to the instrument's
typical movement and quantised to its tick size, derived deterministically from
the same run seed the tape comes from. Then it waits. If the tape enters the
band, the order fills - IN FULL, at its stated price. If the tape never gets
there, the order rests until it is cancelled or the run ends.

That is the whole model. No book, no depth, no queue, no counterparty, no
allocation, no partial-fill model.

Why a range rather than a single price: a synthetic tape jumps, so an exact-tick
trigger would miss fills whenever price steps over it, and the miss rate would
be an artifact of tick size rather than of anything real. A band cannot be
jumped.

## What the model gives without being asked

- **Determinism is untouched.** The band is a pure function of the seed and the
  order, so the tape stays a pure function of (seed, config) and nothing the
  client does is visible to the market. This is the same exclusion that settles
  market impact: a client order of any size moves nothing, because impact would
  make the tape a function of client behaviour.
- **The queue-ahead problem disappears rather than being solved.** The band's
  randomness stands in for queue position and adverse selection without claiming
  to model either. An order at the touch sometimes does not fill, which is the
  real-world behaviour queue-ahead was being invented to produce - and it was
  refused on 2026-08-02 precisely because trade data cannot observe how much
  volume was ahead of you. Nothing here needs that quantity.
- **Maker and taker fall out.** A limit order filled this way is a MAKER fill; a
  market order is a TAKER fill. So the fee schedule's maker/taker split becomes
  expressible with no wire field for liquidity side - which matters, because the
  adapter currently hardcodes `LiquiditySide::Taker` at both fill-construction
  sites in `client/exec.rs`.
- **Stops need no new mechanism.** A stop triggers on the traded price - which is
  possible today and is the same predicate the penetration gate already uses -
  and the triggered market order takes its own band as slippage. One idea covers
  limits, stops and slippage.
- **Partial fills move to where they belong.** They are not normal venue
  behaviour under this model; `PartialFillNext` already exists as a havoc arm.
  That matches the project's posture: the honest path is clean, messiness is
  armed.
- **A marketable-on-arrival limit handles itself.** The tape is already inside
  the band at submit, so it fills immediately.

## The observation this replaces

`reference/architecture.md` states the founding assumption plainly: fills are
synthetic, there is no matching and no order book, and at the default
penetration setting a submit fills immediately and in full at its own price.
That assumption stays. What was wrong was not the absence of a book but the
absence of any question about whether a fill should have happened.

Three refusals were blamed on the missing book and only one of them was really
about depth:

- **Conditional order types are refused outright.** Attributed to having nothing
  to rest against and no trigger to watch. The trigger half was never true - a
  stop can trigger from traded prices - and the fill half is what this model
  answers. See `notes/problem-refused-order-types.md`.
- **`/quotes` is permanently empty**, so the penetration gate had to be specified
  against TRADED prices rather than the quote predicate RFC 4631 asks for. Under
  this model the traded-price predicate is the right one anyway, so the
  deviation stops being a deviation.
- **Queue position was measured and refused** on 2026-08-02 (`a9a12aa`), because
  there is no L2 in this project's lineage and a synthesized depth ladder would
  be invented microstructure sold as fitted realism. That refusal stands and is
  not reopened. This model does not need the quantity.

## Measured: sweep structure, which this model does not represent

From `analysis/probe_binance_aggtrades.py` over June 2026 archives in
`research/market-data/` (see `notes/problem-trade-cadence.md` for provenance).
Binance stamps every fill of one match event with a single `transact_time`, so
collapsing to distinct timestamps recovers an INFERRED arrival and its
multiplicity.

| | BTC | ETH | SOL |
|---|---|---|---|
| inferred events/sec | 5.84 | 6.78 | 1.94 |
| mean aggTrade rows per event | 2.25 | 1.75 | 1.02 |
| max rows in one event | **2,213** | **832** | **268** |
| events that are a single row | 76.5% | 77.4% | 98.6% |
| raw fills per event (rate ratio) | 8.5 | 6.9 | 6.4 |

Three cautions, all of which earlier drafts got wrong at least once.

**The grouping is inferred, not identified.** The probe groups aggTrades sharing
a microsecond timestamp. The arithmetic strongly supports it - at 13.2
aggTrades/sec against microsecond resolution, two independent arrivals collide
with probability about 1.3e-5 per adjacent pair, against 55.5% observed on BTC -
but Binance does not guarantee that a shared timestamp identifies one taker
order, and there is no taker-order id in the data. The rows above are
DISTINCT-TIMESTAMP INFERRED EVENTS. An earlier draft of
`notes/problem-trade-cadence.md` upgraded this to recovered fact; it is not.

**The rows are aggTrades, not price levels.** aggTrades merge fills at one price
and side, so the probe counts rows rather than distinct prices, and has never
verified the latter. "One taker swept 2,213 levels" is unsupported. An earlier
draft went further and concluded "BTC's flow is broad, SOL's is deep" - that
outruns the measurement in the same way, one paragraph after conceding the
limit.

**Price span per inferred event has never been measured**, and it is the
quantity `notes/problem-refused-order-types.md` needs for its slippage question.
It is an extension to a probe that already groups events, over archives already
on disk. Resolve at spec level.

None of this structure is represented by the fill model, and under it none of it
needs to be: a sweep is a property of how real liquidity is consumed, and this
venue does not model liquidity. It is recorded because the cadence document's
choice of what a `TradeTick` represents still depends on it.

## What must be decided

All spec-level. The model's shape is settled.

1. **What sets the band's width.** It must scale with how much the instrument
   moves, so that one configuration is meaningful across instruments and
   regimes. ATR was the user's illustration rather than a specification, and it
   has a practical drawback: ATR is defined on BARS, and this venue ships
   trades - the adapter fabricates bars client-side - so computing it venue-side
   means inventing a bar concept to size a fill band. RECOMMENDED: trailing
   realized volatility over a window of recent trades, which needs no bars, is
   one pass over data the engine already holds, adapts to regime automatically,
   and is well-defined from the first order because warmup guarantees history.
   The runner-up, more faithful and more coupled: the generator already computes
   a volatility state internally, so the band could be a multiple of current
   GARCH sigma - which would also widen fill bands automatically under a
   `VolStorm` arm. The cost is that `mogwai-engine` is venue-agnostic and
   deliberately does not know how the tape was made, so this either couples the
   engine to generator internals or puts volatility on the tape seam. This
   choice sets a scale and nothing else; it is swappable later.
2. **The band's scale and shape** in units of whatever decision 1 picks -
   whether it is symmetric around the stated price, and how wide. These are
   DECLARED values, and the honest label applies: nothing fits them, and the
   repository owner accepts them by inspecting behaviour.
3. **The derived RNG stream.** The band must be drawn from a stream derived from
   the run seed and the order's identity, NOT from the generator's own
   `ChaCha12Rng`. Drawing from the generator's stream would advance it, making
   the tape depend on how many orders the client placed - the determinism
   violation this model otherwise avoids entirely.
4. **Self-trade within one account.** One account can hold both sides. Under
   this model orders never interact at all, so the answer is probably "it is
   impossible rather than prevented", but it should be stated.

## What this document does not decide

The order-type surface itself, which is `notes/problem-refused-order-types.md` -
though this document removes that document's hardest obstacle, since a triggered
stop now has a defensible fill. Nor the arrival rate or the instrument model.

Note what is NO LONGER a dependency: earlier drafts had this document resolving
before `notes/problem-trade-cadence.md`, on the grounds that under matching the
generator emits parent arrivals and wire prints fall out of matching. With no
matching, the tape is generated independently of anything a client does, and the
two documents are independent.

## Known cost

Small, which is the point. The penetration gate landed on 2026-08-02 is the
skeleton of the band's trigger rather than something superseded, and the
account-owned sweeper is the loop that watches for it. What changes is that a
fixed penetration in ticks becomes a seeded, volatility-scaled band drawn per
order.
