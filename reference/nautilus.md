# What nautilus can and cannot represent

The constraints the consumer framework places on this venue, and the rules the
venue must therefore enforce on itself. Every claim here is about nautilus, not
about mogwai, and each one is load-bearing because nothing in nautilus will
catch us being wrong about the thing it describes.

Read the source from `research/nautilus_trader`; build against the crates.io
release pinned in `mogwai-adapter/Cargo.toml`. The two are kept
in sync, so what is read here is what compiles.

Provenance, because it bears on how far each claim can be trusted. The spine of
this document came out of three read-only investigations of nautilus on
2026-08-14, made against HEAD `409214a` rather than against the pin. The claims
marked verified below were re-derived against the in-tree copy on 2026-08-29;
every claim here was re-swept at the 0.63 pin on 2026-09-02, at the 0.64 pin on
2026-09-15, at the 0.65 pin on 2026-10-05, at the 0.66 pin on 2026-10-16, and
at the 0.67 pin on 2026-10-29.

That first sweep was worth its cost, which is the argument for repeating it
after any pin bump: four claims had moved or were wrong. The equity double count
was read as gated on the account being a margin account (itself wrong, as the
0.65 sweep found); the inbound
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

The 0.65 sweep is the strongest case yet for the habit, and for reading the
mogwai side of each claim as well as the nautilus side. Three claims moved with
the release: commission lost its instrument fee schedule, binary-option expiry
became the one live case that acts, and the risk engine began refusing what it
cannot establish, which reaches mogwai through the cash account. But more than
twice that many had been wrong since before 0.64 and survived two sweeps: the
cash-account equity valuation, the liquidation provenance convention, the
inferred-fill consequence in rule 8, the validation nautilus applies to a
venue state, the spot pair's lot size default, and the public constructors.
And the audit of what mogwai itself does turned up the settlement breach of
rule 1 and the adapter's dropped venue-initiated fills, both of which this
document had stated the other way.

The 0.66 sweep moved only what reaches mogwai through the risk engine and the
reconciliation path; every other behavioural claim survived. The risk engine
now holds the free balance of an approved order until its account reflects it
(rule 5), and a reduce-only venue-initiated fill that would open a position is
refused. Four statements
had been wrong before the release and are corrected: account balances are
upserted per currency rather than replaced wholesale, the first state of an
account is exempt from the cash-account negative-balance refusal, the
reconciliation flag also rides fills built from venue fill reports and not only
synthesized ones, and the trailing-stop denial depends on the order's trigger
type.

The 0.67 sweep moved no behavioural claim about money, margin or the account
model; what the release changed there is precision and checking, which reaches
mogwai through rule 7 only. It moved three things at the edges. The risk engine
denies a whole order list when one member fails its checks, and re-checks an
emulated order when the emulator releases it (rule 5). The data side of the
inbound channel gained a correlated request failure, which the adapter now uses
for every history request it cannot serve (see the inbound channel below). And
`PositionStatusReport` gained an optional venue liquidation price, which the
adapter leaves unset because the venue's position state carries none.

## The venue is the sole authority for money

Verified at 0.67.

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
  reports and keeps no shadow ledger. Its validation is internal consistency
  only: `locked + free == total` within each reported triple, and on a cash
  account a refusal of any negative total, because an account built from venue
  state never allows borrowing. The refusal applies to every state after the
  one that creates the account, which is taken as is. Neither check is ever
  made against the fills nautilus actually saw. A refused state is logged and
  dropped, and the account keeps its previous balances for as long as the
  venue keeps reporting states it refuses. Balance rows are upserted per
  currency, never replaced as a set: a currency absent from a state keeps its
  last reported balance, so a venue that has emptied a currency must report a
  zero row, and a row the adapter drops as unrepresentable leaves the old value
  standing in nautilus. A wallet account is the exception to the authority as
  well as to the recompute path: it derives `locked` from its own reservations.
- Nautilus is authority for position quantity, average open price, realized and
  unrealized PnL, and notional, all computed from fills plus the instrument
  definition.

They meet in exactly two places and neither raises on disagreement:
`Portfolio::equity`, and the risk engine's pre-trade balance and margin checks.

The consequence is the reason this document exists. Commission is not one
instance of a class of gaps, it is the general rule: nautilus computes
commission client-side only in its simulated engine, so a venue reporting none
is indistinguishable from one charging none. The one live exception is a fill
that reconciliation infers rather than receives, whose commission nautilus asks
of `ExecutionClient::calculate_commission`; the trait default answers none and
the mogwai adapter does not override it. Nothing in nautilus will catch mogwai
being wrong about money, anywhere.

## The rules that follow, which nothing checks

Each is a rule the venue enforces on itself or a defect nobody sees.

1. **Report cash-only balances, never mark-to-market equity.** Verified at
   0.67. `Portfolio::equity` starts from the account's reported balances total
   and adds a valuation of the open positions that nautilus computes itself, so
   a venue reporting an already-marked equity is double counted, and nothing
   detects it. Get this wrong and every forward test's equity curve is off by
   the marked amount a second time, with a plausible-looking chart and no error
   anywhere. This is the highest-consequence rule here.

   What nautilus adds depends on the account type. A margin account adds each
   open position's unrealized PnL; only an account nautilus calculates itself,
   which the live path never has, values premium-based classes by their mark
   instead. A cash account adds each open position's full signed mark value -
   its notional - except a position whose base currency is already a reported
   balance on an account with no base currency, which is the crypto spot
   shape; a wallet account follows the cash rule. So on a margin account the
   venue must report cash net of unrealized PnL, and on a cash account cash net of what it paid
   for positions, without also listing held instruments as money. The account
   type is the host's configuration, not ours, and the same venue balances are
   read under whichever type the host chose, so a venue that is right under one
   type can still be read wrongly under the other. An earlier version of this
   rule held that a cash account's equity was its balances total alone; that
   was false at every pin it was stated against.

   Audited against the engine on 2026-10-05, and breached in one place.
   `Account::snapshot` reports each currency's total straight from the cash
   balance ledger, and positions ride their own field. The ledger moves on
   fills - realized PnL and commission for futures, the full notional for an
   equity, both legs for spot - and on perpetual funding and forex swap, which
   are real cash transfers and honour the rule. It also moves on daily futures
   settlement: `Engine::settle_read` credits the variation to the balance and
   resets the venue position's average price to the settlement price, emitting
   only an `AccountState`. Real exchanges settle variation into cash the same
   way, but nautilus never sees the reset, because variation margin has no
   carrier (see the inbound channel below). On a margin account it keeps
   computing unrealized PnL from the original fill price, so every settlement
   since the last fill is counted twice until the position closes. The
   owner's ruling on which side yields is owed.

   Two pairings of instrument class and account type also misstate equity,
   both through the host's choice rather than any venue error. Futures on a
   cash account - the adapter's default `account_type` - are valued at their
   full notional on top of a balance that never paid it, a gross
   overstatement. An equity on a margin account is valued at its cash after
   purchase plus only its unrealized PnL, understating by the whole cost
   basis.
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
   upserts its balance rows regardless, but clears its local per-instrument
   lock table only under `event.is_reported && !event.balances.is_empty()`, so
   an unreported state leaves a stale lock table. On the live path that table
   is written only by the account manager, which never runs there, so the harm
   stays latent unless a host turns on `calculate_account_state`.
4. **Route margin by `instrument_id`.** Verified at 0.67 in behaviour as well
   as in the `MarginBalance` constructor signature: the margin account sends a
   `Some` row to its per-instrument map and a `None` row to its cross-margin
   map keyed by currency, and replaces both wholesale on every state that
   carries any balance or margin row. Two `None` rows in one currency
   therefore collapse to the last. Margins are the one thing replaced as a
   set; balances are upserted (see the money section).
5. **Margin is two authorities that never meet.** The venue's reported margins
   populate the account, while the risk engine independently computes its own
   requirement and checks it against the venue's free balance. If the published
   `margin_init` and `margin_maint` do not match the regime the venue actually
   enforces, orders are denied or admitted wrongly and nothing logs it.

   Nautilus's side of that check is strictly a rate on notional: a margin
   account computes the requirement as notional times `margin_init` divided by
   the account's leverage for that instrument (`MarginAccount::get_leverage`,
   defaulting to one) under the default leveraged model. That is what makes the
   rule hard rather than
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
   gate. What to publish instead is open work. Re-checked at 0.67: a zero
   requirement still passes, but a missing balance row in the margin currency
   reads as zero free rather than skipping the check, which stops being
   harmless the moment a rate is published.

   0.65 also made the risk engine refuse what it cannot establish, and that
   reaches mogwai through the cash account rather than through margin. An order
   is denied outright when no account resolves for it, which the adapter
   prevents by holding `connect()` until the account is registered. A trailing
   stop with no trigger price is denied unless the cache holds the data its
   trigger type reads: a last trade for any type but bid/ask, a quote for
   bid/ask and for last-or-bid/ask, so a bid/ask trail is denied on a cached
   trade alone and a last-price trail on a cached quote alone. And a cash
   account reads a missing balance row as zero free: an opening buy whose
   notional currency the venue reports no balance in is denied with
   `NotionalExceedsFreeBalance`, unless the host enabled borrowing. That covers the venue's unfunded mode, which reports no rows
   until a fill creates one, and any funded account lacking the instrument's
   quote currency. Modifies run the same funding check, and a denial there
   is a local `OrderModifyRejected` the venue never sees. Nothing in this
   repository builds a risk engine, so no test here would notice any of it.

   0.66 added a third refusal, again host-side. The risk engine now keeps a
   reservation for every order it approves, holding the funds that order needs
   out of the free balance seen by later submissions until the order is open or
   closed in the cache (a wallet account also releases on in-flight). An order
   the venue has not yet acknowledged therefore costs free balance as a
   reservation until the acknowledgement lands, and an account state reporting
   the venue's own hold for it in the meantime is counted on top. Amendments
   check the account alone and skip the reservations. For mogwai, the delayed
   acknowledgement, the dropped acknowledgement and the venue blackout all
   hold reservations, so a burst of submissions into a slow venue is denied
   host-side with `CumulativeNotionalExceedsFreeBalance` or
   `NotionalExceedsFreeBalance` where a real venue would have admitted them.
   Cash accounts reserve for buys and for sells of the base asset; a margin
   account reserves only a positive initial margin, and `convert` publishes no
   margin rate, so on a margin account the reservation is zero today.

   0.67 widened two refusals, both host-side. When one member of an order list
   fails its risk checks, every other member is now denied with it, where 0.66
   denied that member and left its siblings unsent; for mogwai that means a
   bracket is refused whole before any leg reaches the venue, which never sees
   the list. And an order the host's emulator holds is risk-checked again at
   release, so an emulated stop can be denied at the moment it would have been
   sent, by the same funding and reservation checks as above.
6. **The margin model default is leveraged**, dividing by leverage. Verified:
   `MarginModelAny::default` is the leveraged model, and a margin account takes
   it unless a host calls `set_margin_model`. A venue expecting
   fixed-percentage margin needs the standard model, the choice is the host's
   rather than ours, and getting it wrong is silent.
7. **Commission is venue-absolute.** Verified at 0.67: a position accumulates
   `fill.commission` into its per-currency commission map directly. Since 0.67
   that sum is kept at `Money`'s native precision rather than rounded to the
   currency's, and a fill whose cumulative commission would leave `Money`'s
   bounds is refused before it changes the position. Neither reaches mogwai
   today: `convert::money` builds every commission at the currency's precision,
   so there is nothing finer to accumulate. The venue states a commission on
   every fill, and a zero reaches nautilus as a zero in the venue-named
   currency, never as an absent commission, on the live fill and on the fill
   report reconciliation rebuilds alike; a live fill carries no commission only
   when the venue's amount or currency code cannot be represented, and says so
   in a warning. Since 0.65
   instruments carry no maker or taker fee at all; the rates moved to the venue
   `fee_model`, which only the simulated matching engine reads, so the live
   path has no fee schedule to compare a reported commission against. That is
   what makes the venue's fee schedule load-bearing rather than decorative.
8. **`OrderFilled.reconciliation` is false on ordinary fills.** The field exists
   on the event and is exposed through the order-event trait, and nautilus's own
   reconciliation machinery constructs every fill it builds with it set true,
   both the fills it synthesizes and the fills it builds from venue fill
   reports. So the flag means the fill came out of reconciliation, and setting
   it on an ordinary venue fill misrepresents one as the other.

   Stated more narrowly than it was on 2026-08-14, deliberately. That version
   justified the rule by saying reconciliation-flagged fills take a different
   path through the commission-void logic. Audited 2026-08-29: no consumer in
   the execution or portfolio crates branched on the flag at all, and the
   commission-void path keys on voided quantity rather than on it, and still
   does at 0.67. The use of the flag changed at 0.64, moved again at 0.65 and
   held at 0.66 and 0.67: `has_active_inferred_fill` in the live crate
   now delegates to `inferred_reconciliation_trade_ids` in the execution crate,
   which counts a fill as inferred only when the flag is true and its trade id
   equals the deterministic reconciliation id recomputed from the replayed
   order. The flag is a precondition of that path rather than the whole of it,
   so a venue fill falsely flagged true is not mistaken for a synthesized one;
   an earlier version of this paragraph said it would be, which was never so.
   It is pushed onto a path built only for synthesized fills all the same. The
   rule stands because a truthful label costs nothing and a false one is
   unrecoverable downstream; the adapter sets it false on every venue fill.

## The live inbound channel is closed

Verified at 0.67. `ExecutionEvent` carries `Order`, three order-event batch
variants, `Report` and `Account`, and the data side carries market data plus the
answers to the host's own correlated requests: a response, a response envelope,
or, since 0.67, a request failure naming its reason. So a live venue can push
order events, execution reports, an `AccountState` (whose balance rows are
upserted per currency and whose margin rows replace the previous set), or
market data, and nothing else unbidden. None of them means a payment, so every
venue-initiated account movement is laundered into one of those.

- **Liquidation works, and is the pattern to copy.** Verified: the engine's
  venue-initiated fill path names liquidation, auto-deleveraging and settlement
  together, with Hyperliquid liquidations as the worked case. The execution engine
  materializes an external order from a fill report carrying no matching local
  order, synthesizing initialized and accepted and then applying the fill. The
  materialized order is reduce-only, owned by the instrument's external-order
  claim or else `EXTERNAL`, and dropped outright if the host filters unclaimed
  external orders. Because it is reduce-only, the engine refuses to open or
  reopen a position from its fill, so the pattern carries closes and
  reductions only; a venue-initiated fill that would build exposure is logged
  as an error and its position effect is lost. A fill that predates an open
  position which a synthetic reconciliation order opened, and which that
  position does not already hold, updates its order but not the position,
  which is how a venue replaying executions after a restart avoids doubling
  it. A venue that sends the status report and its fills together can emit
  them as one `ExecutionReport::OrderWithFills` (the emitter's
  `send_order_with_fills`), which the engine reconciles through
  `reconcile_order_with_fills`: the real fills are applied first and only the
  uncovered residual is synthesized. There is no distinct liquidation type,
  and on this path no provenance survives at all: `FillReport` has no info bag, and the engine
  builds the resulting `OrderFilled` with `info` empty. A `liquidation=true`
  key on `OrderFilled.info` appears only in nautilus's own test fixtures, and
  the shipped adapters, Hyperliquid and Binance included, record a liquidation
  only in a log line. An earlier version of this entry called that key the
  existing convention; it never was. Real adapters flatten liquidation,
  auto-deleveraging and settlement into this one shape, indistinguishable
  downstream from any other external fill.

  The mogwai adapter does not use this path yet, which is owed. It drops a fill
  for an order its own mirror does not know, with a warning, and emits no fill
  report on the live stream, so a venue liquidation or risk flatten reaches a
  nautilus host only as the `AccountState` change it causes.
- **Funding is built and unreachable.** Verified at 0.67:
  `FundingSettlement` appears only in the model's event definitions and in the
  backtest crate, nowhere in live, execution or common, and `ExecutionEvent`
  has no variant that could carry it. `FundingSettlement` and the funding
  position-adjustment type exist with full semantics including rollback, and
  only the backtest exchange constructs either; the portfolio and strategy
  crates consume the adjustment event, but nothing live produces one. No live
  client can construct or deliver either, and `ExecutionEvent` has no variant
  for them.
  `FundingRateUpdate` is fully live but carries a rate rather than a payment, so
  it names no account and no amount. Every shipped perp adapter therefore drops
  funding, and it reaches the portfolio only as an unattributed balance delta
  inside the next `AccountState`. It cannot use the liquidation trick, because
  funding moves cash without moving quantity and the fill shape cannot express
  that.
- **Expiry and halts are deliverable and almost entirely inert.** Verified at
  0.67: `InstrumentClose` is carried widely through the data, serialization and
  persistence layers, and the only place that acts on it in full - cancelling
  orders, closing positions, setting market status - is the matching engine,
  which only the backtest exchange and the sandbox adapter instantiate. Since
  0.65 the live execution engine acts on one narrow case: a `ContractExpired`
  close for a cached `BinaryOption` settles every open position in it at the
  close price and stops later fills from moving those positions. The live node
  subscribes that for every execution client's venue unless the client
  declares `settles_contract_expirations`. No other class is touched and no
  order is cancelled, and mogwai publishes no binary option, so for every class
  it serves expiry still changes nothing on live. `InstrumentStatus` carrying a
  halt is cached and published by the data engine and acted on by nothing
  outside the matching engine. On live they inform the strategy and change
  nothing.
- **Variation margin settlement, dividends and splits have no carrier at all**,
  live or backtest. Verified: no dividend, split or corporate-action type exists
  anywhere under the model crate.

The pattern across all of it: the live path can inform but cannot act, with the
single exception of binary-option expiry. Where an upstream carrier does not
exist, the honest course is what real venues do - funding through
`AccountState`, liquidation and expiry settlement through venue-initiated
fills, halts through `InstrumentStatus` - because it is what a real
integration experiences, and it is lossy in exactly the way that motivates the
upstream fix. The mogwai adapter does the first today: funding and every other
venue-initiated balance movement reach nautilus as `AccountState`. The other
two are owed. Venue-initiated fills are dropped at the adapter, as the
liquidation entry above records, and the adapter publishes no
`InstrumentStatus` or `InstrumentClose` at all.

## Publishing an instrument

`reference/glossary.md` owns the instrument classes. This section says only what
nautilus demands of each, and what it silently gets wrong.

`convert::instrument_any` maps our six classes onto nautilus types: `spot` to
`CurrencyPair`, `future` to `FuturesContract`, `equity` to `Equity`, `perpetual`
and `inverse` to a nautilus type chosen by their declared asset class -
`CryptoPerpetual` for cryptocurrency, `PerpetualContract` (the cross-asset
generic, which carries the asset class and the underlying) for everything
else - and `forex` to a named refusal because nautilus ships no leveraged-FX
instrument with a multiplier: its `Cfd` can represent a fiat pair but
hardcodes the multiplier to one. The refusal is deliberate and its reasoning
lives with the open work. The split follows nautilus's own taxonomy rather
than publishing one type uniformly, and the choice is load-bearing: nautilus's
risk engine accepts every inverse `CryptoPerpetual` on its full-position exit
path but a `PerpetualContract` only when linear, so moving crypto inverses
onto the generic would silently change risk-engine behavior on any venue a
host lists in the risk engine's `full_position_exit_venues`, which defaults to
empty. A non-crypto linear
perpetual publishes `base_currency: None` - its underlying is an asset
identity, not a currency, and the costing path reads the base only for an
inverse - while a non-crypto inverse states its settlement currency as the
base, which the checked constructor requires.

Mandatory fields beyond the common spine, re-derived at the 0.67 pin rather than
carried: a spot pair owes base and quote currency; an equity owes its currency;
a `CryptoPerpetual` owes base, quote and settlement currency and the inverse
flag; a `PerpetualContract` owes underlying, asset class, quote and settlement
currency and the inverse flag, with base optional except on an inverse; a dated
future owes asset class, underlying, activation and expiration timestamps,
currency, multiplier and lot size. 0.65 removed `maker_fee` and `taker_fee`
from every instrument type; they were optional, `convert` never set them, and
the mandatory sets did not move.

Multiplier and lot size are non-optional positive quantities on the dated type
alone. On both perpetual types and on the spot pair they are optional. Omitted,
the multiplier defaults to one everywhere and lot size defaults to one on the
perpetual types, while the spot pair keeps an absent lot size as `None`; an
earlier version of this paragraph gave the spot pair the default too. The
previous version
of this paragraph claimed they were mandatory on the perpetual types too, and
that `CryptoPerpetual` owes underlying and asset class; neither is true. Those
two fields exist on `PerpetualContract`, which is required to carry them -
and now publishes them for the non-crypto perpetual classes, taken from the
wire. Mixing the generic type's signature into the elder crypto-only one is
what produced the original error.

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
  one, so fractional-share equities are not expressible. The
  hardcoded multiplier is the same mechanism that makes the `forex` refusal
  necessary rather than fixable with an info bag, since nautilus computes
  notional itself at an implicit multiplier of one. `FuturesContract` has the
  same shape on its size fields: its constructor sets size precision zero and
  size increment one with no parameter for either, and defaults
  `min_quantity` to one, which is why `convert` refuses a fractional contract.
- **Activation and expiration are mandatory on dated types.** Verified: both are
  bare `UnixNanos` on `FuturesContract` and on every constructor - the private
  `new_checked`, the public positional `build_checked`, and the fluent
  `builder()` that `bon` generates over it - optional only in the accessor's
  return type. A synthetic future must invent a contract lifecycle rather than a
  single symbol, which means a roll schedule. At 0.62 this said three
  constructors; 0.63 removed the panicking `new` and made `new_checked` private,
  leaving `build_checked` and its builder as the public ways in. An earlier
  version called the builder the only one.

The 0.64 sweep moved two claims and no trap: the cash-account guard below now
covers dated futures, and rule 8's audit sentence gained a consumer of the flag.
The 0.65 sweep moved no claim in this section; what it corrected here had been
wrong since before 0.64. The 0.66 sweep moved none either: every instrument
type gained only a check that a named tick scheme is registered, and `convert`
names none. Nor did the 0.67 sweep: no instrument type's
fields or checks changed.

One guard is backtest-only. Verified 2026-10-29 at the 0.67 pin:
`SimulatedExchange::add_instrument` refuses a cash account trading
`CryptoPerpetual`, `CryptoFuture`, `FuturesContract` or `PerpetualContract`. At
0.63 the match left out both dated types, so a cash account held a dated future
with no complaint. The check lives in the backtest exchange, so a live node
against this venue still meets no such refusal: do not rely on nautilus to catch
a misdeclared future on the live path.
