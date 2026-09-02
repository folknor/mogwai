# What nautilus can and cannot represent

The constraints the consumer framework places on this venue, and the rules the
venue must therefore enforce on itself. Every claim here is about nautilus, not
about mogwai, and each one is load-bearing because nothing in nautilus will
catch us being wrong about the thing it describes.

Read the source from `research/nautilus_trader`; build against the crates.io
release pinned in `mogwai-adapter/Cargo.toml`, currently 0.63. The two are kept
in sync, so what is read here is what compiles.

Provenance, because it bears on how far each claim can be trusted. The spine of
this document came out of three read-only investigations of nautilus on
2026-08-14, made against HEAD `409214a` rather than against the pin. The claims
marked verified below were re-derived against the in-tree copy on 2026-08-29,
and every claim here was re-swept at the 0.63 pin on 2026-09-02.

That first sweep was worth its cost, which is the argument for repeating it
after any pin bump: four claims had moved or were wrong. The equity double count
turned out to be gated on the account being a margin account; the inbound
channel had more variants than the three claimed; `Equity` hardcodes its
multiplier as well as its size fields; and rule 8's stated justification could
not be found in the tree at all.

The 0.63 sweep bears that out again. Every behavioural claim survived unchanged.
What moved was structural, and it was the one section previously carried without
re-reading: the per-type mandatory fields. That section used to be excused as a
restatement of constructor signatures which the compiler settles the moment
anything is built on them. The excuse is false and this sweep is how we found
out. The compiler settles only the fields a caller actually passes, so a field
wrongly described as mandatory is never contradicted by anyone who omits it -
and `convert` has always omitted the two the section got wrong. Read that
section as a behavioural claim owing verification like any other.

0.63 also moved construction itself. `new_checked` is now private on the
instrument types and construction goes through a `bon` builder, so "mandatory"
means a builder parameter that is not `Option` rather than a positional argument
that is not `None`. The correctness checks still run on `build`, so every
construction-time check below is unaffected.

## The venue is the sole authority for money

Verified 2026-08-29.

One boolean is the whole boundary of trust. `AccountBase` carries
`calculate_account_state`, set false whenever an account is materialized from a
venue-reported `AccountState`, which is the live path always; the backtest sets
it true. The portfolio's order handler early-returns on
`!calculate_account_state && !is_wallet`, so `AccountsManager` - the entire
balance, margin and PnL-to-balance arithmetic layer - never runs on live. The
wallet half of that condition is an exception worth knowing: a wallet account
takes the recompute path even on a venue-reported state.

The split is absolute:

- The venue is authority for balances total, free and locked, for initial and
  maintenance margin, and for commission. Nautilus blind-inserts what the venue
  reports and keeps no shadow ledger. Its only validation is that
  `locked + free == total` within each reported triple, which is internal
  consistency and never consistency with the fills nautilus actually saw.
- Nautilus is authority for position quantity, average open price, realized and
  unrealized PnL, and notional, all computed from fills plus the instrument
  definition.

They meet in exactly two places and neither raises on disagreement:
`Portfolio::equity`, and the risk engine's pre-trade margin check.

The consequence is the reason this document exists. Commission is not one
instance of a class of gaps, it is the general rule: nautilus computes
commission client-side only in its simulated engine, so a venue reporting none
is indistinguishable from one charging none. Nothing in nautilus will catch
mogwai being wrong about money, anywhere.

## The rules that follow, which nothing checks

Each is a rule the venue enforces on itself or a defect nobody sees.

1. **Report cash-only balances, never mark-to-market equity.** Verified.
   `Portfolio::equity` starts from the account's reported balances total and
   adds nautilus's own computed unrealized PnL, so a venue reporting an
   already-marked equity is double counted, and nothing detects it. Get this
   wrong and every forward test's equity curve is off by twice the unrealized
   PnL, with a plausible-looking chart and no error anywhere. This is the
   highest-consequence rule here.

   The addition is gated on the account being a margin account, so a cash
   account's equity is its balances total alone. The hazard is therefore live
   on the margin side - futures and perpetuals - and structurally absent on a
   cash account. Do not read that as a reason to relax the rule for cash: the
   account type is the host's configuration, not ours, and the same venue
   balances are read under whichever type the host chose.

   Honoured, audited 2026-08-29. `Account::snapshot` reports each currency's
   total straight from the cash balance ledger, and that ledger moves only on a
   fill - realized PnL, commission, and an equity trade's notional. Marking
   never touches it, and positions ride their own field. The equity branch of
   `apply_fill` states the reasoning at the site: crediting held shares as a
   balance too would double-count them and make them spendable as money.
2. **Build balance triples so the invariant holds by construction.** Verified.
   `AccountBalance::new_checked` returns an error unless
   `locked + free == total` at matching currencies, and `AccountBalance::new`
   unwraps it, so a rounding residue in a synthetic balance computation is a
   panic rather than a warning. `from_total_and_locked` derives `free` in
   fixed point so the invariant cannot break, and it returns a `Result` rather
   than panicking. It clamps `locked` into `[0, total]` when `total` is
   non-negative, and passes `locked` through verbatim when `total` is negative,
   so a borrow deficit or an underwater margin account preserves venue-reported
   reserved margin and lets `free` carry the shortfall.
3. **Set `is_reported` on every `AccountState`.** Verified: the cash account
   refreshes its balance table only under `event.is_reported && !balances.is_empty()`,
   so an unreported state leaves a stale local lock table.
4. **Route margin by `instrument_id`.** Verified in the `MarginBalance`
   constructor signature: cross margin uses `None`, per-instrument margin uses
   `Some`.
5. **Margin is two authorities that never meet.** The venue's reported margins
   populate the account, while the risk engine independently computes its own
   requirement and checks it against the venue's free balance. If the published
   `margin_init` and `margin_maint` do not match the regime the venue actually
   enforces, orders are denied or admitted wrongly and nothing logs it.

   Nautilus's side of that check is strictly a rate on notional: a margin
   account computes the requirement as notional times `margin_init` divided by
   the instrument's leverage. That is what makes the rule hard rather than
   clerical, because only one of the two margin bases a mogwai user can declare
   has a faithful rate. A fraction-of-notional declaration converts exactly; a
   fixed amount of settlement currency per contract, which is an exchange
   performance bond and mogwai's default basis, does not scale with price and
   therefore has no rate that stays correct as the tape moves. Publishing a rate
   fitted at one price would drift silently, which is the failure this rule
   describes rather than a fix for it.

   Audited 2026-08-29: `convert` sets neither field, so both take nautilus's
   zero default and its pre-trade check requires no margin on any mogwai
   instrument. The venue's own enforcement is unaffected and remains the only
   gate. What to publish instead is open work.
6. **The margin model default is leveraged**, dividing by leverage. Verified:
   `MarginModelAny::default` is the leveraged model, and a margin account takes
   it unless a host calls `set_margin_model`. A venue expecting
   fixed-percentage margin needs the standard model, the choice is the host's
   rather than ours, and getting it wrong is silent.
7. **Commission is venue-absolute.** Verified: a position accumulates
   `fill.commission` into its per-currency commission map directly, with no
   comparison against the instrument's maker or taker fees anywhere on the
   path. That is what makes the fee schedule load-bearing rather than
   decorative.
8. **`OrderFilled.reconciliation` is false on ordinary fills.** The field exists
   on the event and is exposed through the order-event trait, and nautilus's own
   reconciliation machinery constructs its synthesized fills with it set true.
   So the flag means the fill came out of reconciliation, and setting it on an
   ordinary venue fill misrepresents one as the other.

   Stated more narrowly than it was on 2026-08-14, deliberately. That version
   justified the rule by saying reconciliation-flagged fills take a different
   path through the commission-void logic. Audited 2026-08-29: no consumer in
   the execution or portfolio crates branches on the flag at all, and the
   commission-void path keys on voided quantity rather than on it. The rule is
   kept because a truthful label costs nothing and a false one is unrecoverable
   downstream, not because a divergent path was found.

## The live inbound channel is closed

Verified 2026-08-29. `ExecutionEvent` carries `Order`, three order-event batch
variants, `Report` and `Account`, and the data side carries market data. So a
live venue can push order events, execution reports, a wholesale `AccountState`
replacement, or market data, and nothing else. None of them means a payment, so
every venue-initiated account movement is laundered into one of those.

- **Liquidation works, and is the pattern to copy.** Verified: the engine's
  venue-initiated fill path names liquidation, auto-deleveraging and settlement
  together, with Hyperliquid liquidations as the worked case. The execution engine
  materializes an external order from a fill report carrying no matching local
  order, synthesizing initialized and accepted and then applying the fill. There
  is no distinct liquidation type; provenance survives only as free-form
  metadata on `OrderFilled.info`, where the existing convention is a
  `liquidation=true` key. Real adapters flatten liquidation, auto-deleveraging
  and settlement into this one shape.
- **Funding is built and unreachable.** Verified: `FundingSettlement` appears
  only in the model's event definitions and in the backtest crate, nowhere in
  live, execution or common, and `ExecutionEvent` has no variant that could
  carry it. `FundingSettlement` and the funding
  position-adjustment type exist with full semantics including rollback, and are
  wired exclusively into the backtest exchange. No live client can construct or
  deliver either, and `ExecutionEvent` has no variant for them.
  `FundingRateUpdate` is fully live but carries a rate rather than a payment, so
  it names no account and no amount. Every shipped perp adapter therefore drops
  funding, and it reaches the portfolio only as an unattributed balance delta
  inside the next `AccountState`. It cannot use the liquidation trick, because
  funding moves cash without moving quantity and the fill shape cannot express
  that.
- **Expiry and halts are deliverable but inert.** Verified: `InstrumentClose`
  is carried widely through the data, serialization and persistence layers, and
  the only place that acts on it - cancelling orders, closing positions, setting
  market status - is the matching engine, which only the backtest exchange and
  the sandbox adapter instantiate. `InstrumentStatus` carrying a halt is the
  same shape. On live they inform the strategy and change nothing.
- **Variation margin settlement, dividends and splits have no carrier at all**,
  live or backtest. Verified: no dividend, split or corporate-action type exists
  anywhere under the model crate.

The pattern across all of it: the live path can inform but cannot act. Where an
upstream carrier does not exist, mogwai does what real venues do - funding
through `AccountState`, liquidation and expiry settlement through
venue-initiated fills, halts through `InstrumentStatus`. That is honest, because
it is what a real integration experiences, and it is lossy in exactly the way
that motivates the upstream fix.

## Publishing an instrument

`reference/glossary.md` owns the instrument classes. This section says only what
nautilus demands of each, and what it silently gets wrong.

`convert::instrument_any` maps our six classes onto nautilus types: `spot` to
`CurrencyPair`, `future` to `FuturesContract`, `equity` to `Equity`, `perpetual`
and `inverse` both to `CryptoPerpetual`, and `forex` to a named refusal because
nautilus ships no leveraged-FX instrument. The refusal is deliberate and its
reasoning lives with the open work.

Mandatory fields beyond the common spine, re-derived at the 0.63 pin rather than
carried: a spot pair owes base and quote currency; an equity owes its currency;
a `CryptoPerpetual` owes base, quote and settlement currency and the inverse
flag; a dated future owes asset class, underlying, activation and expiration
timestamps, currency, multiplier and lot size.

Multiplier and lot size are non-optional positive quantities on the dated type
alone. On both perpetual types and on the spot pair they are optional, so a
declaration omitting them gets nautilus's default of one. The previous version
of this paragraph claimed they were mandatory on the perpetual types too, and
that `CryptoPerpetual` owes underlying and asset class; neither is true. Those
two fields exist on `PerpetualContract`, the newer generic type, which is
required to carry them - but that is the type we do not publish, and mixing its
signature into the elder crypto-only type is what produced the error.

Four traps, each of which must be guarded rather than exposed as a knob:

- **Quanto valuation is inferred, never declared.** Verified: `is_quanto` is a
  trait default reading true when the instrument has a base currency, its
  settlement currency differs from that base, and settlement is not equivalent
  to quote under nautilus's own currency-equivalence rule. `cost_currency`
  switches on it, so setting settlement currency casually flips a linear perp
  into quanto valuation, silently, and the valuation currency changes with it.
- **Price precision must equal the price increment's precision**, checked at
  construction. Verified on both `Equity` and `FuturesContract`: the two cannot
  be declared independently, and the venue's tick grid must agree exactly or
  construction errors.
- **`Equity` has no size precision, size increment or multiplier.** Verified:
  the trait hardcodes size precision zero, size increment one and multiplier
  one, so fractional-share equities are not expressible at the 0.63 pin. The
  hardcoded multiplier is the same mechanism that makes the `forex` refusal
  necessary rather than fixable with an info bag, since nautilus computes
  notional itself at an implicit multiplier of one.
- **Activation and expiration are mandatory on dated types.** Verified: both are
  bare `UnixNanos` on `FuturesContract` and on both of its constructors - the
  private `new_checked` and the public builder - optional only in the accessor's
  return type. A synthetic future must invent a contract lifecycle rather than a
  single symbol, which means a roll schedule. At 0.62 this said three
  constructors; 0.63 removed the panicking `new` and made `new_checked` private,
  leaving the builder as the only public way in.

And one place nautilus will not catch a misdeclaration. Verified 2026-09-02 at
the 0.63 pin: the
backtest exchange refuses a cash account trading a perpetual, but the check is a
hardcoded match over the two crypto perpetual types and the generic perpetual
type only. `FuturesContract` is absent, so a cash account holds a dated future
with no complaint. Do not rely on nautilus to catch a misdeclared future.
