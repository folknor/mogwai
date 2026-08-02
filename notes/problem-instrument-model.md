# PROBLEM: the venue can only model spot pairs, and half the instruments traded are futures

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. Precedes
`notes/problem-instrument-profiles.md`: a profile cannot be fitted for an
instrument the venue is incapable of representing.

## What the user wants

To forward-test the instruments they actually trade: MNQ, MES, BTC, SOL and ETH,
plus others on occasion. Two of those five are CME index futures. They have
supplied 15-second bar exports for both MNQ1! and MES1! alongside the Binance
crypto archives, which is a clear statement that both are meant to be first-class
targets rather than crypto with a different label.

## The observation

mogwai has one instrument shape, and it is a spot currency pair.

- `InstrumentDef` carries `symbol`, `base`, `quote`, price and size precision,
  and price and size increments. That is the whole model.
- The adapter always constructs a nautilus `CurrencyPair` at conversion.
- The engine is a cash-balance ledger: per-currency balances and per-symbol VWAP
  positions, booking fill deltas on top of a funded seed, with free balance
  derived from resting-order reservations.

Nothing in that can express a contract multiplier, an expiry, a continuous-contract
roll, a tick VALUE distinct from a tick SIZE, initial or maintenance margin,
settlement, or futures profit and loss. MNQ is $2 per index point; MES is $5.
A one-tick move is 0.25 index points, so $0.50 and $1.25 respectively - and the
venue has no field in which that fact can live.

So an "MNQ profile" today can only mean: a spot-like synthetic symbol whose TAPE
resembles MNQ's. Prices would look right and everything about what a fill costs
or what a position is worth would be wrong.

## And the session envelope cannot express a closed market

Separate from the instrument definition, and a real gap for CME - though
narrower than an earlier draft of this document claimed. `SessionProfile` is
`intensity_hour: [f64; 24]`, `vol_hour: [f64; 24]` and `dow_weight: [f64; 7]`,
and its `validate()` requires every element to be STRICTLY POSITIVE.

Exact zero is therefore refused, but the generator DOES have a deliberate
near-zero-share closed-window mechanism, so a daily maintenance break is not
wholly unrepresentable - it is approximable as a very thin hour rather than a
shut one. That approximation is imperfect in a specific way worth knowing: a gap
that opens just before a closure behaves differently from a genuine halt.

What remains genuinely absent:

- a genuinely closed hour, as opposed to a very thin one
- sub-hour boundaries: the bins are whole hours, and CME sessions do not start
  on the hour
- a holiday calendar or an early close
- exchange-local time: the bins are UTC, so a DST-observing CME session drifts
  by an hour twice a year against fixed bins
- contract rolls, which change the instrument rather than its session

A crypto-shaped diurnal weighting can approximate "busier in the afternoon" and,
with the near-zero mechanism, "almost nothing overnight". Whether that
approximation is adequate, or whether an index-futures profile needs a real
calendar, is a decision rather than an obvious answer.

Related boundary question: `MarketRegime::ReopenGap { halt_secs }` already
models a halt - as HAVOC. A scheduled CME close is not havoc, it is the
instrument's normal behaviour, so the profile/havoc line needs restating rather
than assuming the existing arm covers it.

## Why this is not merely cosmetic

- **Sizing.** The sibling cadence document proposes deriving trade size from a
  notional target divided by price, which works for a linear spot pair and fails
  immediately for a contract: notional per MNQ contract is the multiplier times
  the index, and quantity is a whole number of contracts. "Notional over price"
  produces a fractional contract.
- **Funding and reservations.** A cash ledger reserves the quote currency for a
  resting buy. A futures position posts margin, and the reservation is not the
  notional. Any strategy with a protective bracket would double-reserve against
  a model that does not know what it is holding.
- **The realism gate.** The generator's `round_lot_size` thresholds (1.0 / 10.0 /
  0.1) are crypto-shaped. A contract instrument trades in integers.
- **The consumer's view.** nautilus distinguishes instrument classes and the
  adapter currently collapses everything to `CurrencyPair`, so a strategy that
  asks its instrument what it is gets a wrong answer rather than an unsupported
  one.

## The prior decision this touches

`reference/architecture.md` records that the order-type set is Market and Limit
"deliberately and permanently", reasoning that a bookless venue has nothing for a
conditional order to rest against. That is a statement about order types rather
than instrument classes, but it comes from the same root - the venue models as
little as it can get away with - and both are being reopened at once. They should
be reopened knowingly rather than by accident.

## What must be decided

1. **Are MNQ and MES first-class futures, or behaviour presets?** A preset is a
   spot symbol whose tape imitates a future, is cheap, and is honest only if
   labelled as such. First-class means an instrument class, a multiplier, a tick
   value, an expiry or an explicit decision to model a continuous contract, and a
   ledger that understands margin. The implementation scope differs by an order
   of magnitude and everything downstream depends on which.
2. **If first-class: how much futures accounting.** Margin and settlement are a
   different ledger, not an extension of this one. A middle position exists -
   correct multiplier and tick value, integer quantities, no margin - which is
   dishonest in a stated and bounded way rather than silently.
3. **What the wire carries.** `InstrumentDef` would grow fields. A bare symbol
   can already distinguish `MNQU6` from `MNQZ6` - what the wire lacks is
   instrument CLASS, underlying, activation and expiry, and the relationship
   between contracts of one underlying, not the ability to name two of them.
4. **Continuous or dated.** The user's data is `MNQ1!` and `MES1!`, which are
   continuous front-month series, so a roll policy is forced: either the venue
   models a dated contract with an expiry and something rolls it, or it models a
   synthetic continuous instrument and says so.
5. **Session fidelity.** Whether the near-zero approximation above is adequate,
   or whether index futures need exact sub-hour sessions, exchange-local time
   with DST, holidays and early closes. And where the boundary sits against
   `MarketRegime::ReopenGap`, which already models a halt as HAVOC - a scheduled
   close is not havoc, so either the profile grows calendar support or the two
   concepts need separating deliberately.
6. **Netting, hedging and position identity.** nautilus carries `position_id` on
   submission and the adapter currently DROPS it when building the wire order.
   That is a gap regardless of instrument class, and it decides whether two
   opposing orders net or hedge - which for a futures account is the difference
   between one position and two.
4. **What the adapter converts to.** nautilus has instrument types beyond
   `CurrencyPair`; picking the right one is a compatibility decision with the
   consumer, not an internal one.
5. **Whether the corpus supports any of it.** The CME data held is 15-second
   OHLCV bars with no trade counts, no durations and no aggressor. Everything
   the profile document says about fitted-versus-declared provenance applies
   here twice over.

## What this document does not decide

The profile mechanism - named presets, overlays, per-knob override - which is
`notes/problem-instrument-profiles.md` and which assumes the instrument can be
represented at all. Nor the cadence, the book, or the order types, though note
that a futures instrument makes the order-type question sharper: the protective
stop shapes the user trades are most standard on exactly these contracts.

## Known cost, explicitly not a decision input

Per the user's standing instruction, resource cost does not shape this. The
relevant cost here is not resources but blast radius: the instrument definition
is a wire type, so it reaches the protocol, the engine's validation and ledger,
the adapter's conversion, the generator's grid, the config surface and every
fixture in the test suite.
