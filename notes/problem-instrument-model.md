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

## What the user has settled

This document's central fork is closed, and the answer is larger than the
question asked.

**First-class, and the model is a PARAMETERIZATION rather than an enum of
supported instruments.** mogwai mimics an exchange venue listing multiple
symbols. Presets exist - BTC, ETH, MNQ and whatever else is worth committing -
but a preset is nothing more than a bundle of otherwise-tunable config knobs
with a name on it, which is what the word means. A user who picks no preset must
be able to invent any instrument they want: MCL, MBT, AAPL. So the requirement
is not "support futures" but "expose a config surface complete enough that any
instrument the user names can be expressed", and the presets are the convenience
layer on top.

Three consequences.

- **The mechanism half of `notes/problem-instrument-profiles.md` collapses into
  this document.** Profile, preset and override stop being three concepts. There
  is one parameter set, committed bundles of values that name themselves, and a
  precedence rule for per-knob override. What stays in the profiles document is
  the MODEL question - whether the arrival and volatility process constants
  become per-instrument at all - which is empirical and separate.
- **Fees are config.** An exchange charges fees, and a config-complete
  instrument definition includes the schedule. A separate problem statement
  covering fees existed and was DELETED once this ruling landed: whether to
  model them at all, where they live and what grounds the defaults are all
  answered by it. The schedule rides the wire rather than staying server-local,
  because a consumer needs to know what trading costs BEFORE trading rather than
  only observing it per fill. What survived that document is carried in
  decision 8 below: the havoc question and the ledger's booking currency.
  Note the separate finding that closed the "declare fee-free" exit
  independently: nautilus computes commission client-side only in its SIMULATED
  matching engine (`execution/src/models/fee.rs`, used by backtest and the
  sandbox adapter). On the LIVE path, which is the reason mogwai exists,
  commission arrives on `OrderFilled.commission` from the venue's own fill
  report and is `Option<Money>` - so a venue reporting nothing is
  indistinguishable from a venue that charges nothing, and no consumer can
  correct for it without fabricating a number.
- **Margin is an instrument property; starting capital is an account one.**
  Initial and maintenance margin per contract belong in this parameter bundle.
  How much money a run starts with is `[balances]`, which already exists and
  becomes per-run by construction once one process serves one consumer. Neither
  needs a problem statement. The residue is spec work: the ledger must learn
  that a futures position reserves MARGIN rather than notional, and that two
  reduce-only bracket legs against one position are exclusive rather than
  additive.

**The config/havoc line, stated generally.** Config is the instrument's
IDENTITY - what it is when nothing is going wrong: class, multiplier, tick size
and tick value, lot rules, session and calendar, cadence, fees. Havoc is a
deliberate DEVIATION from that identity, armed at runtime and windowed. That
settles the boundary question this document raised below: a scheduled CME close
is the instrument being itself, so it is config, and `MarketRegime::ReopenGap`
remains havoc only for UNSCHEDULED halts. It also places a fee schedule that
changes mid-run, or a venue charging more than it advertised, squarely as
legitimate havoc.

**One `MOGWAI` venue, not several.** Considered and rejected: splitting into
per-asset-class venue identities would be more faithful to the fact that no real
exchange lists AAPL alongside MNQ alongside BTC, but it buys nothing here.
Nautilus routes execution clients per venue, so several venue ids mean several
factory pairs, connections and accounts; `AccountId` is per venue, so it would
split a strategy's account, except that strategies are single-instrument (see
below) so there is nothing to split; and broadarrow selects the keyless venue by
name, so extra ids are consumer friction. The only cost of one venue is
implausibility, which has no audience given the operator is an agent and a
strategy sees only its own instrument.

**Strategies are single-instrument.** No strategy on mogwai holds positions in
several symbols at once. This closes a gap that would otherwise be real: with
independent per-symbol generators the tapes carry no cross-instrument
correlation, so a multi-symbol strategy would be handed diversification that
does not exist in a market where BTC, ETH and SOL co-move and MNQ and MES are
near-identical. Since nothing observes the joint distribution, independence is
correct rather than a defect - and for a fleet marginalizing over symbols it is
what you want.

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
- sub-hour boundaries: the bins are whole hours. An earlier draft justified this
  by saying CME sessions do not start on the hour, which is FALSE for these
  contracts - CME equity index futures run 18:00 to 17:00 ET, both on the hour.
  The real sub-hour structure is the daily maintenance halt at 16:15 to 16:30
  ET, which whole-hour bins cannot express at either end. Right conclusion,
  wrong reason.
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

Decision 1 of an earlier draft - first-class or preset - is settled above and is
not repeated. The list is renumbered: it previously ran 1 to 6 and then repeated
4 and 5, so any citation of "decision 4" in an earlier document is ambiguous and
should be re-resolved against this list.

1. ~~**How much futures accounting.**~~ SETTLED: the full version, because it is
   arithmetic rather than a different ledger. An earlier draft offered a middle
   position - correct multiplier and tick value, integer quantities, no margin -
   as "dishonest in a stated and bounded way rather than silently". The user
   rejected the framing: this is not expensive.

   The engine already keeps per-symbol positions with a volume-weighted average
   price and per-currency balances, so unrealized P&L is
   `(mark - avg_px) * qty * multiplier` against the current tape price, which is
   arithmetic over data already present. Margin is a configured per-contract
   number times position size, and a comparison. A breach refuses new orders or
   force-closes - and a forced close is a market order, so it fills through the
   band model in `notes/problem-order-book.md` with no new machinery.

   So: multiplier, tick value, integer quantities, CONTINUOUS mark-to-market,
   margin held per contract, and a breach that refuses or liquidates. The
   question this answers is whether a forward test can report "profitable, but
   the account would have been liquidated on day three". It can.

   DAILY SETTLEMENT is the one genuinely separate piece and it must not be
   assumed away silently. Futures are marked to market daily and the difference
   moves in actual CASH - that is what variation margin is, and it is why a
   losing futures position generates margin calls rather than merely a worse
   balance. It fires at a defined SETTLEMENT TIME, which is not the session
   close: for CME equity index futures the settlement price is struck at 16:00
   ET while the session runs to 17:00 ET, with the maintenance halt at 16:15 to
   16:30. Three distinct daily timestamps, only one of which is a session
   boundary. Spot crypto has no settlement at all, so this is futures-only.
2. **What the wire carries.** `InstrumentDef` grows fields. A bare symbol can
   already distinguish `MNQU6` from `MNQZ6` - what the wire lacks is instrument
   CLASS, underlying, activation and expiry, the relationship between contracts
   of one underlying, multiplier and tick value, lot rules, margin and the fee
   schedule. The consumer needs the fee schedule BEFORE trading rather than
   only observing it per fill, which argues for the wire rather than
   server-local config.
3. **The completeness bound.** "Invent any instrument you wish" has one real
   limit and it should be stated rather than discovered: nautilus does not
   accept arbitrary instruments. The adapter must construct a CONCRETE type -
   `CurrencyPair`, `FuturesContract` and so on - so the config surface must
   carry enough to both SELECT the nautilus type and FILL it. This is a match
   statement over what each `InstrumentDef` declares rather than an
   architectural fork.

   Scope it to what is actually traded: spot pairs and cash-settled futures.
   A review pass proposed naming a finite set of semantic classes with explicit
   refusals for everything else, on the grounds that AAPL implies equities with
   short-sale, corporate actions and settlement, and MCL implies physical
   delivery. The user's answer is that this is irrelevant - those were
   illustrations of a COMPLETE CONFIG SURFACE, not a request for equities, and
   pre-emptively enumerating refusals for instruments nobody will trade is
   speculative work. Build the classes that are needed; adding one later is code
   rather than config, and that is the honest boundary.
4. **Continuous or dated.** The user's data is `MNQ1!` and `MES1!`, which are
   continuous front-month series, so a roll policy is forced: either the venue
   models a dated contract with an expiry and something rolls it, or it models a
   synthetic continuous instrument and says so.
5. ~~**Session fidelity.**~~ SETTLED, and much smaller than the question
   implied. The venue does not need a CALENDAR, it needs a SIMULATED one. There
   is nothing to reconcile against: the tape is synthetic and a run spans a few
   simulated days, so nobody is checking the venue's clock against a real
   exchange's.

   What the instrument config carries is a set of recurring daily timestamps at
   SUB-HOUR resolution - open, halt window, settlement, close - expressed in the
   instrument's own terms, plus genuine WEEKEND closure. A nine-day MNQ run
   should contain a weekend, and that weekend should be shut rather than thin.
   NOT modelled: holidays, early closes, DST. A run either spans a holiday or
   does not, and if it does the strategy sees a quiet day, which costs nothing
   to declare as unmodelled.

   Whether a stated timestamp matches CME's actual clock to the minute matters
   only insofar as the tape should feel like MNQ, which is a PRESET's business
   rather than the venue's.

   One implementation trap worth naming, because it looks like a bug to fix:
   `SessionProfile::validate()` requires every `dow_weight` to be STRICTLY
   POSITIVE, so a genuinely closed Saturday cannot be expressed today - only a
   very thin one. That refusal is not an oversight; it is a normalization guard
   against a config that silently compresses arrival rates. Real closure means
   either relaxing it deliberately for the weekend case or expressing closure as
   its own concept, not loosening the check.

   The `ReopenGap` half is settled by the config/havoc line above: a scheduled
   close is config, and that arm stays havoc for UNSCHEDULED halts only.

   A configured weekend also happens to resolve an ambiguity that recurs
   elsewhere - it is legitimate silence the consumer can know about IN ADVANCE,
   which is exactly the "venue asleep versus subscription dead" case, and it is
   neither ambiguous nor havoc.
6. **Netting, hedging and position identity.** nautilus carries `position_id` on
   submission and the adapter currently DROPS it when building the wire order -
   verified: the identifier appears nowhere in `mogwai-adapter/src`. That is a
   gap regardless of instrument class, and it decides whether two opposing
   orders net or hedge, which for a futures account is the difference between
   one position and two.
7. **Whether the corpus supports any of it.** The CME data held is 15-second
   OHLCV bars with no trade counts, no durations and no aggressor. Everything
   the profile document says about fitted-versus-declared provenance applies
   here twice over.
8. **Fee residue.** Which currency a commission books in and how, given the
   ledger is per-currency with reservations derived from resting orders. Small:
   `account.rs` already books commission direction-aware, a buy's cost adding it
   and a sell's proceeds subtracting it, with a comment recording that the math
   is wired and waiting for a source. And which havoc arms reach a fee schedule.

   ONE DEPENDENCY, found in review and not obvious: a MAKER/TAKER schedule
   cannot be implemented honestly yet, because the venue has no way to say which
   a fill was. The wire carries no liquidity side and the adapter hardcodes
   `LiquiditySide::Taker` at both fill-construction sites in
   `client/exec.rs`. Under the fill model in `notes/problem-order-book.md` the
   classification falls out by construction - a limit order filled by the tape
   reaching its band is a MAKER fill, a market order is a TAKER fill - so the
   dependency is on that model rather than on any matching engine, and it is
   satisfied the moment the model lands.

   Also worth stating rather than assuming: treating fees and margin as
   INSTRUMENT IDENTITY is a design decision, not a description of how markets
   work. Real fee schedules vary by account tier and liquidity role, and CME
   states margin requirements vary by product, volatility, portfolio and time. A
   fixed per-contract margin and a fixed schedule are legitimate declared
   simplifications; they should be labelled as the venue's model rather than
   presented as the market's.
9. **The precedence and refusal rules for preset-versus-override.** Moved here
   from `notes/problem-instrument-profiles.md` with the mechanism half. A user
   names a preset; the preset sets a bundle of knobs; the user then overrides
   individual ones on top. The failure mode is setting a cadence under a preset
   that also sets one and silently getting whichever wins. This venue's existing
   posture is to refuse ambiguity at boot rather than resolve it quietly -
   `queue_ahead_enabled` without `penetration_ticks` is refused, acceleration
   without a pinned `sim_epoch_ns` is refused - so the consistent choice is
   either logging every override with both values or refusing the combination
   outright.
10. **How provenance is recorded**, so a FITTED field and a DECLARED one are
    distinguishable at the point of use rather than only in a commit message.
    Also moved here. This matters most for the presets, because they will not be
    built from equivalent evidence: BTC, ETH and SOL have Binance trade-level
    archives, while MNQ and MES have 15-second OHLCV bars and nothing else, so
    a CME preset's cadence is derived arithmetic and its clustering comes from
    nowhere at all. A declared 0.076 s cadence and a fitted one look identical
    in a struct. This project already refused a model for exactly that class of
    reason - queue-ahead, on 2026-08-02 - so an unmarked declared value inside
    something presented as fitted would be the same failure with better manners.
    Which corpora can support which instrument stays with
    `notes/problem-instrument-profiles.md`; the recording mechanism is here.

## What this document does not decide

The MODEL half of `notes/problem-instrument-profiles.md`: whether the arrival
and volatility process constants become per-instrument at all, which is an
empirical question about measured clustering rather than a config-surface one.
The MECHANISM half of that document - named presets, overlays, per-knob
override - is no longer separate and lands here, per the ruling above. Nor the
cadence, the book, or the order types, though note
that a futures instrument makes the order-type question sharper: the protective
stop shapes the user trades are most standard on exactly these contracts.

## Known cost, explicitly not a decision input

Per the user's standing instruction, resource cost does not shape this. The
relevant cost here is not resources but blast radius: the instrument definition
is a wire type, so it reaches the protocol, the engine's validation and ledger,
the adapter's conversion, the generator's grid, the config surface and every
fixture in the test suite.
