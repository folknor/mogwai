# The product-type plan

Status: plan, not a spec. Nothing here is implemented. Written 2026-08-14
against `research/nautilus_trader` at current HEAD (409214a), which is well
past the 0.61 release `mogwai-adapter` pins - see "The nautilus version
question" below for what that costs.

## The goal

mogwai acts as the exchange for any product type worth trading - spot,
perpetuals, dated futures, equities - and a strategy forward-tested against it
gets an account that is correct.

Four things must be true:

1. **Coverage.** Those four product types work. Adding an instrument is declaring
   what it is - its class, contract terms, session and economics - not writing
   engine code. If a new instrument requires the engine to learn something, the
   model is wrong.
2. **Account correctness.** Balances, margin, position and P&L at the end of a run
   are what a real venue would have produced. This is where the weight sits:
   nautilus is not the authority for any of it, and nothing compares the two.
3. **The product's economics reach the strategy.** Funding, settlement, expiry,
   liquidation, corporate actions are what make a perp a perp and a dated
   future a dated future. Where nautilus has no live-path carrier for one, the
   carrier is added upstream rather than laundered into an unattributed balance
   delta. A forward test that quietly loses funding lies about the strategy.
4. **It scales to N concurrent strategies**, each with its own account,
   accelerated, seeded, reproducible.

What it buys: no exchange accounts, ever. No KYC, no API keys, no discovering
the testnet does not list what you wanted to trade.

## What was established, and how

Three independent read-only investigations of nautilus at HEAD. Their findings
are the load-bearing facts below; where this document states something about
nautilus, it came from one of them and can be re-derived from the named types.

### Fact 1: account type is per-venue, and it forces a venue split

`AccountType` is `Cash | Margin | Betting | Wallet`. It is chosen by adapter
config, not by the instrument - there is no instrument-to-account-type function
anywhere in the tree, and every live adapter hardcodes its own. The account
object is materialized by `AccountAny::from_events`, switching on the
`account_type` the venue itself reported in its `AccountState`.

`AccountId` is venue-scoped. So one venue identity carries exactly one account
type for everything it lists.

Consequence: a single `MOGWAI` venue listing spot, a perp, MNQ and AAPL must
pick one account type for all four. `Margin` is the only value that passes both
of nautilus's mismatch checks across that set - and choosing it applies
leveraged margin to spot and permits naked equity shorts. That is not
"acceptable with caveats", it is wrong accounting.

The venue splits by account type. Distinct venue identities, distinct client
pairs, one account type each, all speaking the same wire protocol:

| venue identity | account type | products |
|---|---|---|
| `MOGWAI-SPOT` | Cash | spot pairs |
| `MOGWAI-PERP` | Margin | perpetuals |
| `MOGWAI-FUT` | Margin | dated futures (MNQ, ES) |
| `MOGWAI-EQ` | Cash, or Margin where shorting is wanted | equities |

The names are a proposal. The split is not: it is forced by nautilus's account
model and no amount of instrument declaration works around it.

This retires the standing "there is ONE `MOGWAI` venue, not one per asset
class" premise recorded in `notes/todo.md`. It dies for a mechanical reason,
not a taste one.

### Fact 2: on the live path, the venue is the sole authority for money

One boolean is the entire boundary of trust. `calculate_account_state` is
hardcoded `false` whenever an account is materialized from a venue-reported
`AccountState` - which is the live path, always; the backtest sets it true. The
portfolio's order and position handlers early-return on it, so `AccountsManager`
- the whole balance, margin and PnL-to-balance arithmetic layer - never runs on
live.

The split is absolute:

- **Venue is authority**: balances total/free/locked, initial and maintenance
  margin, commission. Nautilus blind-inserts what the venue reports and keeps no
  shadow ledger. The only validation is that `locked + free == total` within
  each reported triple - internal consistency, never consistency with the fills
  nautilus actually saw.
- **Nautilus is authority**: position quantity, average open price, realized and
  unrealized PnL, notional. All computed from fills plus the instrument
  definition.

They meet in exactly two places and neither raises on disagreement:
`Portfolio::equity`, and the risk engine's pre-trade margin check.

The commission gap recorded in `notes/todo.md` - nautilus computes commission
client-side only in its simulated engine, so a venue reporting none is
indistinguishable from one charging none - is therefore not one instance of a
class. It is the general rule. Commission is merely the case where the
client-side formula visibly exists and is wired only into the backtest.

Nothing in nautilus will catch mogwai being wrong about money. Anywhere.

### Fact 3: the live inbound channel is closed, and product economics mostly
### have nowhere to land

A live venue can push exactly three shapes into nautilus: order events, a
wholesale `AccountState` replacement, or market data. There is no fourth. Every
venue-initiated account movement must be laundered into one of those.

Per event type:

- **Liquidation** works, and is the pattern to copy. The execution engine
  materializes an external order from a fill report carrying no matching local
  order - synthesizing initialized and accepted, then applying the fill. Its doc
  comment names Hyperliquid liquidations as the case. Real adapters flatten
  liquidation, ADL and settlement into this shape. There is no distinct
  liquidation type; provenance survives only as free-form metadata on
  `OrderFilled.info`, where the existing convention is a `liquidation=true` key.
- **Funding** is built and unreachable. `FundingSettlement` and
  `PositionAdjustmentType::Funding` exist with full semantics including
  rollback, and are wired exclusively into the backtest `SimulatedExchange`. No
  live client can construct or deliver either; `ExecutionEvent` has no variant
  for them. `FundingRateUpdate` is fully live but carries a rate, not a payment
  - no account, no amount. So every shipped perp adapter drops it: BitMEX
  explicitly skips funding executions, Bybit excludes them from
  `is_exchange_generated`, Binance parses a `FundingFee` reason and maps it to
  nothing. Funding reaches the portfolio only as an unattributed balance delta
  inside the next `AccountState`. It cannot use the liquidation trick because it
  moves cash without moving quantity, and the fill shape cannot express that.
- **Expiry and halts** are live-deliverable but inert. `InstrumentClose` with
  `InstrumentCloseType::ContractExpired` and `InstrumentStatus` with
  `MarketStatusAction::Halt` both have real live subscriptions, and
  `InstrumentStatus` even has its own top-level `DataEvent` variant. But the
  only code that acts on them - cancelling orders, closing positions, setting
  market status - lives in `OrderMatchingEngine`, instantiated only by the
  backtest exchange and the sandbox adapter. On live they inform the strategy
  and change nothing.
- **Variation margin settlement, dividends and splits** have no carrier at all,
  live or backtest. Zero corporate-action types anywhere in model, execution,
  live or backtest. A split currently arrives from Bybit as an ordinary fill
  with no semantic marker.

The pattern across all of it: the live path can inform but cannot act.

## The topology

The user owns the venue process. This is the correction that reorganizes
everything else, and it inverts what the code assumes today.

Today the venue's life is bound to its launcher, down to `PR_SET_PDEATHSIG`
firing on the death of the launching thread. The client owns the venue. That is
correct and cheap for a single fire-and-forget accelerated run, and it is
backwards for an exchange.

On a real exchange the venue is the durable thing and the client is transient:
resting orders outlive your disconnect, your crash and your redeploy. That is
the topology to build:

- The user starts long-lived venues, one per product class per Fact 1.
- Agents connect over an explicit URL. `mogwai serve --config <path>` already
  supports this standing alone - `--launcher-pid` is optional and `--duration`
  defaults to indefinite - and `mogwai-adapter` never launches anything, it only
  dials a required `base_url`. Both halves exist today.
- The shipped launcher remains valid for one-shot runs. It becomes one consumer
  of the serve path rather than the mechanism.

### The consequence: the venue must become multi-account

Each agent runs its own nautilus node with its own execution client and its own
`AccountId`. Today mogwai has one account per venue config
(`DEFAULT_ACCOUNT_ID = "MOGWAI-001"`, validated for the `ISSUER-NUMBER` shape at
load). A long-lived venue serving many agents cannot work that way.

Account identity moves to the connection. Established at connect, per client,
the way a real exchange does it with API keys - not baked into the venue's
config file. What follows:

- Per-account balances, margin ledger, positions and resting orders.
- The resting book is shared and the accounts are not. This is the first time
  mogwai's book has to distinguish whose order is whose, and it is the point at
  which self-trade between two agents becomes possible and must be ruled on.
- Funding, settlement and expiry are per-account events fired from one venue
  clock over many ledgers.
- `expected_run_seed` binds a client to a run today. Under a long-lived venue
  it binds a client to a venue instance, which is still what it is for.

This is the largest single piece of work in the plan and it is a precondition
for the workload, not an optimization of it.

### What this does NOT change

Resting orders are not a lifecycle problem within a run. Everything is
accelerated: a limit order resting three sim-days is minutes of wall clock. The
run is a single continuous session by design - no restart, no resume, reproduce
by re-running the seed. A resting book is state the engine holds and the
sweeper already walks it. The lifecycle question was only ever about who owns
the process, and the answer is the user.

## The instrument declaration

The unit that declares an instrument needs a name; it is not what this
repository currently calls a preset, and that word is deliberately not used
here.

An instrument declaration is the complete statement of what an instrument is.
It governs three things and nothing else:

1. **The nautilus instrument definition** the venue publishes, because nautilus
   computes all position arithmetic from it and will happily compute it wrong.
2. **The venue identity** it belongs to, which follows from its account type per
   Fact 1.
3. **The economics** the venue must emit - which of the four event generators are
   armed, and on what schedule.

The rule: the engine owns the mechanisms, the declaration arms them. If adding
an instrument requires an engine change, the model has been violated.

### What the declaration must carry, per Fact 1's taxonomy

Nautilus models 18 tradable instrument variants unified by `InstrumentAny`. The
relevant ones and what each demands:

| product | nautilus type | mandatory beyond the common spine |
|---|---|---|
| spot pair | `CurrencyPair` | `base_currency`, `quote_currency` |
| equity | `Equity` | `currency`, optional `isin` |
| perpetual | `PerpetualContract` | `underlying`, `asset_class`, `quote_currency`, `settlement_currency`, `is_inverse`, multiplier, lot size |
| dated future | `FuturesContract` | `asset_class`, `underlying`, `activation_ns`, `expiration_ns`, `currency`, multiplier, lot size |

Two choices worth recording:

- Prefer `PerpetualContract` over `CryptoPerpetual`. The latter is the
  crypto-only elder; the former is the newer generic that works on any asset
  class and does not corner us when a non-crypto perp shows up.
- `multiplier` and `lot_size` are non-optional positive `Quantity` on every
  dated and perpetual type, and absent or optional everywhere else. A futures
  declaration owes both; a spot declaration omits both and gets 1.

### Traps that must be guarded rather than exposed as knobs

- `is_quanto` is never declared. It is inferred from whether settlement
  currency differs from both base and quote. Setting settlement currency
  casually flips a linear perp into quanto valuation, silently, and the
  valuation currency changes with it. Settlement currency needs a guard, not a
  free slot.
- `price_precision` must equal `price_increment.precision`, checked at
  construction. Precision and increment cannot be declared independently - they
  are generated as a pair, and the venue's tick grid must agree exactly or
  construction errors.
- `Equity` has no size precision or size increment constructor arguments; the
  trait hardcodes precision 0 and increment 1. Fractional-share equities are not
  expressible as `Equity` at HEAD. If fractional equities matter, that is an
  upstream change.
- `activation_ns` and `expiration_ns` are mandatory and non-optional on dated
  types. A synthetic MNQ must invent a contract lifecycle, not just a symbol -
  which means the declaration owns a roll schedule, not one expiry.
- Nautilus's cash-account guard is incomplete. The backtest exchange refuses a
  cash account trading futures or perps, but the check is a hardcoded match
  listing only `CryptoPerpetual`, `CryptoFuture` and `PerpetualContract`.
  `FuturesContract` is absent. A cash account holds MNQ with no complaint. Do
  not rely on nautilus to catch a misdeclared futures instrument. (Upstream
  candidate - see below.)

## Account correctness: the rules mogwai must not get wrong

These follow from Fact 2. Nothing checks any of them, so each is a rule the
venue enforces on itself or a defect nobody sees.

1. **Report cash-only balances, never mark-to-market equity.** `Portfolio::equity`
   is the venue's reported total balance plus nautilus's own computed unrealized
   PnL. Perp venues typically report equity already marked to market; nothing
   detects the double count. Get this wrong and every forward test's equity
   curve is off by 2x uPnL, with a perfectly plausible-looking chart and no
   error anywhere. This is the single highest-consequence rule in the document.
2. **Build balance triples with** `from_total_and_locked`. `AccountBalance::new`
   panics when `locked + free != total`. Rounding in a synthetic balance
   computation is a crash, not a warning.
3. **Set** `is_reported: true` on every `AccountState`. Otherwise cash accounts keep
   a stale local lock table.
4. **Cross margin uses** `instrument_id: None` on `MarginBalance` entries;
   per-instrument margin uses `Some`. The routing is by that field.
5. **Margin is two authorities that never meet.** The venue's reported margins
   populate the account; the risk engine independently computes its own
   requirement from `margin_init` and leverage and checks it against the
   venue's free balance. If the declared `margin_init`/`margin_maint` do not
   match the regime mogwai actually enforces, orders are denied or admitted
   wrongly and nothing logs it. The declaration and the ledger must be generated
   from one source.
6. **The margin model default is leveraged** (divides by leverage). A futures or
   equity venue expecting fixed-percentage margin needs `StandardMarginModel`,
   and getting this wrong is silent.
7. **Commission is venue-absolute.** `fill.commission` is copied verbatim and never
   checked against the instrument's maker/taker fees. The fee schedule is
   already an instrument knob; this is the rule that makes it load-bearing
   rather than decorative.
8. `OrderFilled.reconciliation` must be false on ordinary fills.
   Reconciliation-flagged fills take different paths in the commission-void
   logic.

## Found while doing this: a probable defect in mogwai's existing havoc

`Position::apply_fill` suppresses duplicate fills keyed on `trade_id` plus
`causation_id`. If mogwai's duplicate-fill divergence repeats the `trade_id`,
nautilus drops the duplicate with a warning and it never reaches the accounting
path at all - the arm arms, the wire carries it, and nothing downstream is
exercised.

Not verified against what the divergence seam actually emits. If it is true, the
arm has been certifying less than it appears to, and the fix is a distinct
`trade_id` per delivery. Worth confirming independently of this plan.

## The upstream work

Landing these in nautilus is what separates "mogwai models the product" from
"mogwai fakes it the way every other adapter fakes it". Ordered by leverage.

1. **Lift** `FundingSettlement` **onto the live path.** The types exist, the semantics
   exist including rollback, and they are wired only into the backtest. Every
   shipped perp adapter currently launders funding through an unattributed
   balance delta. This is the highest-value change in the list: it is additive,
   it has an obvious shape (an `ExecutionEvent` variant plus a live emitter
   method), and mogwai emitting it correctly is what makes the gap visible.
2. **Make expiry act on the live path.** `InstrumentClose` with `ContractExpired`
   already arrives; the machinery that cancels orders and closes positions is
   matching-engine-only. Dated futures are not honestly forward-testable until
   this exists.
3. **Complete the cash-account guard** to include `FuturesContract` and the option
   types. Small, obviously correct, and directly on the MNQ path.
4. **Corporate actions.** Genuinely new: no dividend or split type exists anywhere.
   Splits are the hard half because they rewrite an open position's quantity and
   average price retroactively. Only needed when equities become real.

Where an upstream change has not landed, mogwai does what real venues do -
funding through `AccountState`, liquidation and expiry settlement through
venue-initiated fills, halts through `InstrumentStatus`. That is honest, because
it is what a real integration experiences, and it is lossy in exactly the way
that motivates the upstream fix.

### The nautilus version question

`mogwai-adapter` pins the published 0.61 crates so a fresh clone builds without
a sibling checkout. Everything above was established against HEAD. Two costs
follow and neither is settled here:

- Findings may not hold on 0.61. The taxonomy in particular has moved:
  `PerpetualContract` is described as the newer generic type, so it may not
  exist at 0.61 at all.
- Any product type depending on an upstream change pins mogwai to a nautilus
  version carrying it. The plan must state, per product type, which side of that
  line it falls on.

## Build order

Each step is complete and useful on its own.

0. **Decide** the nautilus version posture, since it gates everything downstream.
1. **Multi-account venue.** Account identity at connect, per-account ledgers and
   resting orders, shared book. Precondition for the workload; nothing else in
   the plan is reachable at scale without it.
2. **Venue split by account type.** Distinct venue identities and client pairs.
   Forced by Fact 1.
3. **The instrument declaration.** Whatever it ends up being called: the four
   nautilus types above, the guarded fields, generated so declaration and ledger
   share one source.
4. **Spot and perpetuals.** Spot is the degenerate case that emits nothing.
   Perpetuals need funding - through `AccountState` first, through the upstream
   event once it lands - plus liquidation, which already works via
   venue-initiated fills.
5. **Dated futures.** Contract lifecycle and roll schedule, variation margin,
   expiry settlement. Needs upstream item 2 to be honest.
6. **Equities.** Sessions with real auctions and halts, then corporate actions,
   which need upstream item 4.

## What this plan does not cover

- **The tape.** Out of scope by owner ruling. The mechanism generalizes across
  product types; whether any given instrument's generated tape is realistic is a
  separate question with a separate answer.
- **Who starts the venues** and how N of them are supervised. The user owns the
  process; how that is operated is not mogwai's design problem.
- **Throughput.** Whether N venues fit on the machine remains excluded per the
  standing instruction that resource cost shapes no decision here.
