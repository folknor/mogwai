# SPEC: the instrument model as a complete parameterization

## Standing references

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. Spawned from
`notes/problem-instrument-model.md` (the resolved problem statement) and the
PROBLEM STATEMENTS entry in `notes/todo.md` that owns it. The premises that
entry declares - accelerated forward tests, optional sim duration, no restart
and no resume, eagerly generated warmup, single-instrument strategies, one
`MOGWAI` venue - are inherited here and not re-argued.

Nautilus APIs are READ from `research/nautilus_trader` and BUILT against the
sibling `../nautilus_trader` checkout, per `AGENTS.md`. Every nautilus signature
quoted below was read from the in-tree copy.

This revision folds two review reports, `notes/spec-instrument-model-review-1.md`
and `notes/spec-instrument-model-review-2.md`, and the adjudications recorded in
the "Findings rejected" section at the end.

## What this spec builds

The venue stops modelling one shape (a spot currency pair) and starts modelling
an instrument as a bundle of knobs, of which spot and cash-settled futures are
the two classes the config surface can currently select. MNQ and MES become
real: a contract multiplier, a tick value distinct from a tick size, integer
quantities on the order path AND on the tape, margin posted rather than notional
reserved, continuous mark-to-market, daily variation-margin settlement, a
sub-hour session calendar with genuine weekend closure, and a maker/taker fee
schedule that reaches the consumer as booked commission.

Seven landings, each one coherent and fully intrusive, each keepable or
revertable on its own gates:

1. Instrument identity - the wire type, the config surface, the adapter's
   nautilus construction.
2. The generator's contract size grid - multiplier-aware, integral sizing, and
   the first `TAPE_PROTOCOL_VERSION` bump.
3. The futures ledger - margin, mark-to-market, settlement, breach, and the
   sweeper restructuring that makes venue-originated events deliverable.
4. The session calendar - hard closure, sub-hour boundaries, exchange-local
   time, and the second `TAPE_PROTOCOL_VERSION` bump.
5. Fees - the schedule, liquidity side on the wire, commission booking, the
   fee havoc arm.
6. Position identity - `position_id` end to end, netting versus hedging.
7. Presets and provenance - named bundles, override precedence, fitted-versus-
   declared marking.

Ordering is technical. 1 precedes everything, because nothing else has a class
to branch on. 2 precedes 3 because every futures gate from 3 onward drives a
real MNQ tape, and a tape emitting fractional contracts produces zero-quantity
ticks the adapter drops - the futures gates would be measuring a decimated feed.
3, 4, 5 and 6 are mutually independent given 1 and 2, and the order above is
chosen so the largest ledger rewrite lands before the things that read the
ledger. 7 lands last because a preset is a bundle of knobs and the knobs do not
all exist until 6 is in.

Between landing 1 and landing 3 the engine REFUSES a `Future` instrument at
construction, naming landing 3 as what serves it. That is sequencing under
requirement 6, not deferral under requirement 3: the refusal is deleted by
landing 3 in this same spec, and no gate is written against it surviving.
Landing 2 exercises the futures size grid at the `mogwai-data` level, below the
engine, so it is unaffected by that refusal.

## Survey of the ground

### The wire type and everything that reads it

`mogwai-protocol/src/instruments.rs` is 79 lines: `InstrumentDef` with
`symbol`, `base`, `quote`, `price_precision`, `size_precision`,
`price_increment`, `size_increment`, plus `default_instruments()` seeding one
BTCUSDT. Everything that touches it:

- `mogwai-engine/src/lib.rs` - `instruments: HashMap<Symbol, InstrumentDef>`,
  seeded from `EngineConfig::instruments`, exposed by `instrument_defs()`.
- `mogwai-engine/src/orders.rs` `validate_submit` - looks up the instrument for
  `size_increment`, `price_increment`, and (under `enforce_funds`)
  `instrument.quote` / `instrument.base` as the currency an order must cover.
  TWO further funds sites read the same pair and are equally in the blast
  radius: `validate_fill_funds`, which checks `qty * fill_px` against the quote
  at every fill site (submit, sweep, and the stop arms), and the funds arm of
  `on_modify`.
- `mogwai-engine/src/account.rs` - `apply_fill` books `last_qty` into `base`
  and `last_qty * last_px` into `quote`; `locked_balances` reserves the quote
  notional for a resting buy and the base quantity for a resting sell;
  `free_balance(currency)` is TOTAL MINUS LOCKED, which matters for the breach
  formula below.
- `mogwai-server/src/config.rs` - `ConfiguredInstrument` restates the seven
  fields (deliberately, so `deny_unknown_fields` works, since serde cannot
  combine it with `flatten`), plus `generator` and `session` sub-tables;
  `validate_instrument_def` bounds the strings against `MAX_SYMBOL_LEN` /
  `MAX_CURRENCY_LEN` and checks increment-versus-precision;
  `refuse_unfunded_quotes` refuses boot when `[balances]` omits the quote.
- `mogwai-server/src/source.rs` - `InstrumentProfile` pairs the def with its
  `GeneratorScalars` and `SessionProfile`; `default_profile` builds the
  built-in.
- `mogwai-server/src/http.rs` - `/instruments` serves `Vec<InstrumentDef>`.
- `mogwai-adapter/src/convert.rs` `instrument_any` - always
  `InstrumentAny::CurrencyPair`, resolving base and quote through
  `Currency::from_str`.
- `mogwai-adapter/src/client/{data,exec,shared}.rs` - a shared
  `Arc<Mutex<HashMap<Symbol, InstrumentDef>>>`, consulted for
  `price_precision` / `size_precision` on every tick, fill and report row.

Three findings from this survey that change the shape of the work:

**Integer quantities need no new validator on the ORDER path.**
`validate_submit` already runs
`on_increment(order.quantity, instrument.size_increment)`. A futures def with
`size_increment = 1` and `size_precision = 0` therefore already refuses a
fractional contract, and the adapter's `quantity()` already builds a
precision-0 `Quantity`. The gap is not the order validator; it is that nothing
in the config surface makes the combination reachable and coherent, and that the
GENERATOR ignores it - which is landing 2, not a footnote.

**Tick value must be derived, not stored.** Storing `tick_value` alongside
`price_increment` and `multiplier` admits a config where the three disagree.
`tick_value = price_increment * multiplier` is exact for both classes
(multiplier is 1 for spot), so the wire carries the two independent numbers and
callers derive the third. This satisfies decision 2's requirement that tick
value cross the wire without creating a contradiction knob.

**Reduce-only exclusivity is already correct.** `locked_balances` skips every
`order.submit.reduce_only` order outright, so two reduce-only bracket legs
against one position reserve nothing and cannot double-reserve. The problem
statement's worry ("two reduce-only bracket legs against one position are
exclusive rather than additive") is discharged by existing code, not by new
code. Landing 3 must not regress it: the futures reservation arm keeps the same
skip.

### The ledger

`Account` is `balances: HashMap<String, Decimal>` plus
`positions: HashMap<Symbol, PositionState { qty, avg_px }>`. Every accumulation
path is saturating (`add_clamped` / `sub_clamped` / `mul_clamped`) with a
once-per-key warning, because unbounded cross-fill accumulation was a reachable
panic. `snapshot` emits `AccountState { account_id, balances, positions,
ts_event }` where `Balance` is `{ currency, total, free, locked }` and
`Position` is `{ symbol, quantity, avg_px }`.

There is no mark price anywhere in the engine, no unrealized P&L, no margin,
and no notion of a settlement instant. `commission` is booked
direction-aware (a buy's cost adds it, a sell's proceeds subtract it) and is
always `Decimal::ZERO`.

The mark price the engine lacks does exist server-side:
`mogwai-server/src/fills.rs` exposes
`read_last(symbol: &str, ts: u64, profiles: &InstrumentProfiles) -> Option<Decimal>`.
`MarketReadingCache` also lives there, but two properties of it are load-bearing
and were misread by the first draft of this spec: it is a SINGLE-ENTRY cache
keyed on symbol plus bucket plus divergence multiplier plus tick cap, and it is
owned by the HTTP app state, NOT by `FillSweep`. A per-pass mark over more than
one symbol therefore evicts itself, and the sweeper cannot reach the cache at
all today. Landing 3 pays for that rather than assuming it away.

`mogwai-server/src/sweeper.rs` runs a per-interval pass that takes the engine
lock, so the mark-to-market hook is an existing loop, not a new task. But the
loop as written cannot host a mark: it does
`let scans = ...; if scans.is_empty() { continue; }` BEFORE taking the engine
lock and `if events.is_empty() { continue; }` before delivery, so a run with no
resting orders never reaches the body at all. Landing 3 restructures the pass.

### The session

`mogwai-data`'s `SessionProfile` is `intensity_hour: [f64; 24]`,
`vol_hour: [f64; 24]`, `dow_weight: [f64; 7]`. `validate()` requires every
element strictly positive AND bounds the sums: `intensity_hour` and
`dow_weight` one-sided above `SESSION_SHARE_SUM`, `vol_hour` two-sided around
`VOL_HOUR_SUM`. The positivity refusal is a normalization guard, not an
oversight - a genuinely closed Saturday is not expressible, only a very thin
one.

`SessionModulator` precomputes `intensity_hour[h] * 24.0` and
`dow_weight[d] * 7.0` and multiplies them per tick. `utc_hour_dow` derives hour
and day-of-week by integer division on the unix second, Sun=0, with no chrono
dependency and no time-zone concept at all. `GeneratedSource::next_duration_ns`
divides a Weibull draw by the arrival multiplier, and when that multiplier
falls below `SESSION_CLOSED_ARR_MULT` it routes to `closed_window_gap_ns`,
which exists because a near-zero share otherwise stretches a 7-second draw into
80 days and can saturate the f64-to-u64 cast at `u64::MAX`, freezing the clock
and breaking the strict monotonicity `monotonic_clock` pins. The cap it uses,
`MAX_SESSION_GAP_NS`, is 31_622_400 seconds - 366 days, not 80 days. That
distinction matters in landing 4.

`next_duration_ns` is NOT the final gap. `begin_event` divides its result by an
armed `FlowSurge`'s `rate_mult`, then adds a `ReopenGap` halt if one is crossed.
Any calendar correction must therefore run at the END of `begin_event`, on the
final instant, or a surge can shorten a weekend jump back into the closure.

### The generator's size grid

`GeneratedSource::new` derives
`size_median = typical_notional / start_price / exp(SIZE_LOG_SIGMA^2 / 2)` and
draws a lognormal around it; `round_lot_size(base, median)` snaps a
`size_round_frac` fraction of draws to `10^floor(log10(median))`, and
`next_size` floors the result at `Decimal::new(1, SIZE_DECIMALS)` - one
hundred-millionth. Both paths are pure notional-over-price arithmetic with no
multiplier and no integrality, which is exactly the failure the problem
statement names: for a contract, notional per unit is `multiplier * price` and
the result must be a whole number.

The consequence downstream is not cosmetic. The adapter builds `Quantity` at
`size_precision = 0`, and nautilus `TradeTick::new_checked` calls
`check_positive_quantity`, so every sub-half-contract print becomes a zero
quantity and a dropped tick. An MNQ tape on today's grid is silently decimated.
The preceding trade-cadence landing deliberately fixed SPOT sizing only and left
contract sizing here; landing 2 owns it.

### Admission sizing

`mogwai-protocol/src/sizing.rs` bounds the serialized bytes one `ClientMessage`
can make the engine produce, and the server reserves against that bound before
it lets the engine mutate. `account_state_max_bytes` counts
`BALANCE_ROW_MAX_BYTES` per balance and `POSITION_ROW_MAX_BYTES` per position.
`ORDER_EVENT_MAX_BYTES` bounds a live order event; `FILL_ROW_MAX_BYTES` bounds a
recorded fill row inside a `FillSnapshot` and is a SEPARATE constant that must
move whenever `OrderFilled` widens; `ORDER_STATUS_ROW_MAX_BYTES` bounds a status
row. Any new row TYPE on `AccountState`, and any new field on an existing row,
moves these constants. Under-reserving voids the whole admission contract, so
every landing that widens a wire struct must widen every matching constant in
the same commit.

`deliver` in the sweeper reserves with `reserve_swept(shape, emitted)` where
`emitted` is the count of ORDERS that emitted events. Venue-originated output
that no order emitted - a mark-only account snapshot, a liquidation cascade -
has nothing reserved for it today. Landing 3 fixes that explicitly.

### Reconciled against siblings

The one sibling problem statement still open is the surviving half of the
profile question - whether the arrival and volatility constants genuinely
differ per instrument. It is empirical and gates nothing here: this spec makes
those constants per-instrument by construction (they already are, as
`GeneratorScalars` fields on `InstrumentProfile`), and whether a given preset
sets them differently is a fitting question landing 7 records provenance for
rather than answers.

`notes/todo.md` carries an open item invalidating the 0.1603 duration ACF
anchor. It is not this spec's debt and is not touched: nothing here moves the
duration process. Landing 4 DOES move what the realism gate measures, and pays
that bill in its own gate section. The 12.6 ms cache-miss market reading IS
touched, by landing 3's mark, and that landing states the cost rather than
disclaiming it.

## Landing 1 - instrument identity

### Target artifacts

`mogwai-protocol/src/instruments.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WireAssetClass { Fx, Equity, Commodity, Index, Cryptocurrency }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum InstrumentClass {
    Spot {
        base: String,
        quote: String,
    },
    Future {
        /// The index or commodity the contract references. Reaches nautilus as
        /// `FuturesContract::underlying`, which must be non-empty ASCII.
        underlying: String,
        /// The currency P&L, margin and commission all book in.
        settlement_currency: String,
        /// Currency units per 1.0 of price. MNQ is 2, MES is 5.
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
}

impl InstrumentClass {
    /// The currency a position's value, margin and fees are denominated in.
    /// Spot answers with its quote, which is what the cash ledger already uses.
    pub fn settlement_currency(&self) -> &str;
    /// `1` for spot. Never zero or negative - `validate_instrument_def` refuses
    /// both, so callers may multiply without checking.
    pub fn multiplier(&self) -> Decimal;
    /// `Some(base)` for spot, `None` for a cash-settled future - a future has
    /// no deliverable leg, which is precisely why its ledger is single-currency.
    pub fn base_currency(&self) -> Option<&str>;
    pub fn is_future(&self) -> bool;
}

pub struct InstrumentDef {
    pub symbol: Symbol,
    pub class: InstrumentClass,
    pub price_precision: u8,
    pub size_precision: u8,
    pub price_increment: Decimal,
    pub size_increment: Decimal,
}

impl InstrumentDef {
    /// Currency value of one tick: `price_increment * multiplier`. Derived
    /// rather than stored, so a config cannot state a tick value that
    /// contradicts its own increment and multiplier. MNQ: 0.25 * 2 = 0.50.
    pub fn tick_value(&self) -> Decimal;
    /// Currency value of `qty` units at `px`: `qty * px * multiplier`. The one
    /// place notional is computed; every ledger and validation site calls it.
    pub fn notional(&self, qty: Decimal, px: Decimal) -> Option<Decimal>;
}
```

`notional` returns `Option` because it is `checked_mul` twice - the existing
`validate_submit` overflow refusal reads it directly and keeps its message.

`default_instruments()` keeps returning the BTCUSDT spot def, rewritten against
the new shape.

`mogwai-server/src/config.rs`:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredInstrument {
    pub(crate) symbol: mogwai_protocol::Symbol,
    /// REQUIRED. No serde default: a defaulted `Spot` has no base or quote to
    /// supply, and an empty pair would fail the non-blank validator anyway.
    pub(crate) class: ConfiguredClass,
    pub(crate) price_precision: u8,
    pub(crate) size_precision: u8,
    pub(crate) price_increment: Decimal,
    pub(crate) size_increment: Decimal,
    /// Futures only; refused on a spot instrument, and REQUIRED on a future
    /// from landing 3 onward.
    pub(crate) margin: Option<ConfiguredMargin>,
    /// Both classes. Absent means the fee-free venue this repo ships today.
    pub(crate) fees: Option<ConfiguredFees>,
    pub(crate) generator: mogwai_data::GeneratorScalars,
    pub(crate) session: mogwai_data::SessionProfile,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ConfiguredClass {
    Spot { base: String, quote: String },
    Future {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredMargin {
    pub(crate) initial_per_contract: Decimal,
    pub(crate) maintenance_per_contract: Decimal,
    #[serde(default)]
    pub(crate) breach_action: BreachAction,
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BreachAction { #[default] Refuse, Liquidate }

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredFees {
    pub(crate) maker: FeeRate,
    pub(crate) taker: FeeRate,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub(crate) enum FeeRate {
    /// Basis points of filled notional. The crypto shape.
    BasisPoints { rate: Decimal },
    /// Flat currency amount per contract filled. The CME shape.
    PerContract { amount: Decimal },
}
```

The `[instrument.class]` table nests one level below `[instrument]` and carries
its own `kind` tag; the wire enum's tag is `class`, and the two names differ
deliberately because the config key IS `class`. A worked TOML pair, since the
nesting is not obvious:

```toml
[instrument]
symbol = "BTCUSDT"
price_precision = 2
size_precision = 8
price_increment = "0.01"
size_increment = "0.00000001"

[instrument.class]
kind = "spot"
base = "BTC"
quote = "USDT"

[instrument]
symbol = "MNQ"
price_precision = 2
size_precision = 0
price_increment = "0.25"
size_increment = "1"

[instrument.class]
kind = "future"
underlying = "NQ"
settlement_currency = "USD"
multiplier = "2"
asset_class = "index"

[instrument.margin]
initial_per_contract = "2000"
maintenance_per_contract = "1800"
breach_action = "liquidate"
```

Because `class` is required and `deny_unknown_fields` is on, EVERY existing
config file carrying top-level `base` and `quote` stops parsing. That is a
breaking config change, stated rather than papered over with a serde default
that cannot exist. Migrating every shipped config and fixture under
`crates/mogwai-server/tests/configs/`, `scripts/`, and any config named in
`reference/config.md` is a deliverable OF THIS LANDING, in the same commit.

Margin and fees stay SERVER-SIDE in landing 1 and never reach `InstrumentDef`,
per the problem statement's decision 2: they are not inputs nautilus needs in
order to value a position. Landings 3 and 5 consume them from
`InstrumentProfile`, which grows the fields.

`mogwai-adapter/src/convert.rs`:

```rust
pub(crate) fn instrument_any(
    def: &InstrumentDef,
    ts_init: UnixNanos,
) -> anyhow::Result<InstrumentAny>
```

matches on `def.class`. Spot keeps today's `CurrencyPair::new` call verbatim.
`Future` calls `FuturesContract::new_checked` with:

- `instrument_id` / `raw_symbol` as today,
- `asset_class` mapped from `WireAssetClass`,
- `exchange: Some(Ustr::from("MOGWAI"))`,
- `underlying: Ustr::from(&class.underlying)`,
- `activation_ns: UnixNanos::from(0)` and
  `expiration_ns: UnixNanos::from(i64::MAX as u64)` - the synthetic continuous
  instrument of decision 4, expiring in the year 2262. NOT `u64::MAX`:
  `UnixNanos::as_i64` asserts `<= i64::MAX`, and `to_datetime_utc` /
  `to_rfc3339` go through it, so a `u64::MAX` expiry panics the moment any
  consumer or log line formats it. `i64::MAX` keeps the "no plausible
  days-to-expiry" property without arming a panic,
- `currency` from `settlement_currency`,
- `price_precision`, `price_increment` as today,
- `multiplier: quantity(class.multiplier, multiplier_scale)?` where
  `multiplier_scale = class.multiplier.scale() as u8`, so a fractional
  multiplier survives instead of being truncated to an integer or to zero.
  `validate_instrument_def` refuses a scale above 9, which is nautilus'
  representable limit,
- `lot_size: quantity(Decimal::ONE, 0)?`. NOT `def.size_increment`:
  `FuturesContract::new_checked` HARDCODES `size_increment = Quantity::from(1)`
  and `size_precision = 0`, so a configured increment of 2 would be enforced by
  mogwai and advertised to nautilus as 1. The resolution is upstream of the
  conversion - `validate_instrument_def` refuses any futures `size_increment`
  other than exactly 1, so what mogwai enforces and what nautilus advertises are
  the same number by construction,
- every `Option` argument `None`, including `margin_init` / `margin_maint` /
  `maker_fee` / `taker_fee`, per decision 2,
- `ts_event: UnixNanos::from(0)`, `ts_init`.

`new_checked` rather than `new`: the constructor asserts on a non-ASCII
underlying and a non-positive multiplier or lot size, and this crate's standing
rule is that no unsupervised reader or exec task holds a panicking constructor.

### Validation

`validate_instrument_def` grows, in addition to what it already checks:

- `Future.multiplier > 0`, with `scale() <= 9`; a zero, negative or
  over-scaled multiplier is refused by name.
- `Future.underlying` non-blank, ASCII, within `MAX_SYMBOL_LEN`.
- `Future.settlement_currency` non-blank and within `MAX_CURRENCY_LEN`; same
  for spot's base and quote, unchanged.
- A `Future` whose `size_increment` is not exactly `1` is refused, and so is a
  `Future` with `size_precision != 0`. A fractional contract has no meaning, and
  a non-unit increment cannot be expressed to nautilus.
- `margin` present on a `Spot` is refused; `margin` absent on a `Future` is
  refused from landing 3 onward (landing 1 already refuses futures outright).
- Inside `ConfiguredMargin`: `initial_per_contract >= maintenance_per_contract`
  and `maintenance_per_contract > 0`. An initial below maintenance is a config
  that opens a position already in breach.

`refuse_unfunded_quotes` is rewritten to `refuse_unfunded_settlement`: it
demands `[balances]` fund `class.settlement_currency()`, which is the quote for
spot (unchanged behaviour) and the settlement currency for a future. The
existing boot-error message keeps its shape.

A `Future` instrument additionally fails boot in landing 1 with
`"futures instruments are served from the margin-ledger landing; this build
accepts spot only"`. That refusal is deleted by landing 3.

### The blast radius, precisely

- `mogwai-engine`: FOUR funds sites, not two. `validate_submit`'s funds arm
  reads `instrument.class.settlement_currency()` for a buy and
  `instrument.class.base_currency()` for a sell, with the sell arm refusing a
  future outright until landing 3 (a cash-settled future has no base leg to
  reserve). `validate_fill_funds` and the funds arm of `on_modify` take the same
  accessors, so a futures buy is not margin-checked at submit and
  notional-checked at fill. `apply_fill` and `locked_balances` likewise. Spot
  behaviour is bit-identical.
- `mogwai-server`: `ConfiguredInstrument::def` builds the class;
  `InstrumentProfile` carries `margin` and `fees` alongside the def;
  `/instruments` serves the new shape. That endpoint's JSON DOES change - the
  seven flat fields become five plus a tagged `class` object - and no
  byte-identity claim is made for it. The byte-identity claim in this spec is
  about the admission-governed socket path only.
- `mogwai-adapter`: the `def()` fixture is triplicated across three test
  modules (`convert.rs`, and two client test modules) - all three move to one
  shared `tests/common` constructor as part of this landing rather than being
  edited three times.
- `mogwai-protocol/src/sizing.rs`: `InstrumentDef` is not itself reserved
  against (it goes out over HTTP, not the admission-governed socket), so no
  constant moves here. Stated explicitly so a reviewer does not have to
  re-derive it.

### Gates

- `brokkr fmt`, then `brokkr check --gate` - the whole suite including the four
  socket-backed adapter binaries, which is mandatory because this landing
  touches `mogwai-adapter`.
- `brokkr test -p mogwai-protocol instrument_def_round_trips` - extended to
  round-trip both variants and to pin the exact JSON tag (`"class": "spot"` /
  `"class": "future"`), since both ends serialize against it.
- New, `mogwai-protocol`: `tick_value_derives_from_increment_and_multiplier`,
  asserting MNQ (increment 0.25, multiplier 2) reads 0.50 and MES (increment
  0.25, multiplier 5) reads 1.25 - the two numbers the problem statement names.
- New, `mogwai-server`: `a_future_with_a_non_unit_size_increment_refuses_boot`,
  `a_future_with_an_unfunded_settlement_currency_refuses_boot`,
  `a_margin_table_with_initial_below_maintenance_refuses_boot`, and
  `a_config_with_top_level_base_and_quote_refuses_boot_naming_the_class_table`
  - the migration break, pinned so it produces a legible message rather than a
  bare serde error.
- New, `mogwai-adapter`: `a_future_def_builds_a_futures_contract`, asserting
  the returned `InstrumentAny` is the `FuturesContract` variant, that its
  multiplier survives at its own scale, that `lot_size` and `size_increment`
  both read 1, and that `expiration_ns.to_rfc3339()` RETURNS rather than
  panics. Run it with
  `brokkr test -p mogwai-adapter a_future_def_builds_a_futures_contract`.
- `python3 scripts/smoke.py` - the default mode, spawning its own venue, proving
  the migrated spot config still serves end to end.

Keep/revert: kept if `brokkr check --gate` is green and the smoke passes with
the shipped spot default behaving byte-for-byte unchanged on the socket path. A
spot-path behavioural difference is a revert, not a re-bless: landing 1 is a
re-parameterization, and spot moving means the parameterization is wrong.

## Landing 2 - the generator's contract size grid

### What is wrong

`size_median = typical_notional / start_price / exp(sigma^2 / 2)` answers "how
many UNITS of base is a typical trade", which is the right question for spot and
the wrong one for a contract, where a unit is worth `multiplier * price`. The
draw is then rounded to `SIZE_DECIMALS = 8` and floored at `1e-8`, so an MNQ
tape emits sizes like `0.00714` contracts, which the adapter turns into a
precision-0 `Quantity` of zero and nautilus drops.

### Target artifacts

`mogwai-data`, in `generated/` alongside the source:

```rust
/// How the generator turns a notional target into a printable size. Spot is
/// the identity of today's behaviour; a future is whole contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeGrid {
    /// Currency units per 1.0 of price. `1` for spot.
    pub multiplier: Decimal,
    /// Sizes are whole numbers, floored at `min_size`.
    pub integral: bool,
    /// Smallest printable size. `10^-SIZE_DECIMALS` for spot, `1` for a future.
    pub min_size: Decimal,
}

impl SizeGrid {
    /// Today's behaviour exactly: multiplier 1, non-integral, 1e-8 floor.
    pub fn spot() -> Self;
    /// Derived from the instrument class. `mogwai-data` already depends on
    /// `mogwai-protocol`, so this is not a new edge.
    pub fn from_def(def: &InstrumentDef) -> Self;
}
```

`GeneratedSource` gains one field, `size_grid: SizeGrid`, and the constructor
chain grows one argument at the bottom only:

```rust
impl GeneratedSource {
    /// Unchanged signature. Delegates with `SizeGrid::spot()`, so every
    /// existing caller and every spot fixture is untouched.
    pub fn new(scalars, seed, start_ts, fp, regime) -> Self;
    pub fn try_new(scalars, seed, start_ts, fp, regime) -> Result<Self, GeneratedSourceError>;

    /// The futures entry point. The server calls this for every instrument,
    /// passing `SizeGrid::from_def`.
    pub fn try_new_with_size_grid(
        scalars: GeneratorScalars,
        seed: u64,
        start_ts: u64,
        fp: &Fingerprint,
        regime: Option<MarketRegime>,
        size_grid: SizeGrid,
    ) -> Result<Self, GeneratedSourceError>;
}
```

`new_with_session_profile`, `with_clamp_override` and `try_with_clamp_override`
each take the grid as a trailing argument; the public two-argument-fewer forms
above are the only ones outside `mogwai-data` that anything calls.

### The sizing algorithm

At construction:

```
notional_per_unit = start_price * size_grid.multiplier
size_median = typical_notional / notional_per_unit / exp(SIZE_LOG_SIGMA^2 / 2)
size_median = size_median.max(f64::MIN_POSITIVE)
```

For spot the multiplier is 1 and this is arithmetically the current expression,
which is what makes the spot-identity gate below provable rather than asserted.

Per draw, in `next_size`:

```
base = lognormal(ln(size_median), SIZE_LOG_SIGMA) draw, max MIN_POSITIVE
if size_round_frac hit:  raw = round_lot_size_on_grid(base, size_median, grid)
else:                    raw = base
size = quantize(raw, grid)
```

`quantize`:

- Non-integral grid (spot): `decimal_from_f64(raw).round_dp(SIZE_DECIMALS)`
  then `.max(grid.min_size)`. Identical to today.
- Integral grid (future): `decimal_from_f64(raw)` rounded to zero decimal places
  with `RoundingStrategy::MidpointAwayFromZero`, then `.max(grid.min_size)`,
  i.e. floored at one contract. ROUND-HALF-AWAY-FROM-ZERO, not truncation and
  not banker's rounding: truncation biases every draw down and turns the whole
  sub-1.0 mass into the floor twice over, and banker's rounding on a 0.5-heavy
  discrete grid measurably thins the odd sizes.

`round_lot_size_on_grid` is `round_lot_size` with one guard: on an integral
grid the lot is `max(1, 10^floor(log10(median)))`, so a median below 10
contracts snaps to whole contracts rather than to a fractional lot. For
`SizeGrid::spot()` it is `round_lot_size` verbatim.

The floor at one contract is not cosmetic. It is what guarantees the adapter's
precision-0 `Quantity` is never zero and `check_positive_quantity` never drops a
tick. Because the floor truncates the lower tail, the realized mean contract
size runs ABOVE `typical_notional / notional_per_unit` whenever the median is
small; that is a declared property of the grid, measured by the gate below
rather than pretended away.

### The tape version

`mogwai_data::TAPE_PROTOCOL_VERSION` moves from 2 to 3 in this landing. The
invariant in `AGENTS.md` makes the bump mandatory for any change to generated
output, and while the spot path is byte-identical by construction, the futures
path is a new tape shape and the constructor surface changed. The version string
`mogwai --version` reports and the lifecycle test that reads it move with it.

### Server wiring

`mogwai-server/src/source.rs` builds each `GeneratedSource` through
`try_new_with_size_grid(.., SizeGrid::from_def(&profile.def))`. Nothing else in
the server changes.

### Gates

- `brokkr fmt`, then `brokkr check` (this landing does not touch
  `mogwai-adapter`).
- New, `mogwai-data`, each runnable as `brokkr test -p mogwai-data <NAME>`:
  - `contract_sizes_are_whole_numbers_and_never_zero` - 100_000 draws off an MNQ
    grid, every one an integer, every one at least 1.
  - `contract_size_median_tracks_notional_over_multiplier_times_price` - the
    median draw off a grid with `typical_notional = 200_000`,
    `start_price = 21_000`, `multiplier = 2` sits at 4 or 5 contracts, i.e. it
    divided by 42_000 and not by 21_000. This is the regression a
    multiplier-blind grid produces, wrong by exactly the multiplier.
  - `the_integral_floor_lifts_the_realized_mean_above_the_notional_target` -
    pins the declared property above with a measured ratio, so a later change to
    the rounding rule cannot move it silently.
  - `spot_draws_are_bit_identical_across_the_size_grid_change` - the same seed
    and scalars through the old expression and the new `SizeGrid::spot()` path
    produce the identical `Vec<Decimal>`. This is the landing's central
    guarantee.
  - `a_lot_snap_on_an_integral_grid_never_produces_a_fractional_lot`.
- The EXISTING `realism` test and the dwell family
  (`run_seeded_tape_dwell_is_bounded`, `dwell_is_bounded_across_run_seeds`) run
  against the spot default and must be UNCHANGED. Any drift is a revert.
- `brokkr test -p mogwai-server version_string_reports_the_tape_version` - the
  existing lifecycle assertion, now reading 3.
- `python3 scripts/smoke.py`.

Keep/revert: kept if the spot draws are bit-identical, the realism and dwell
readings are unchanged, and the contract gates pass. A spot draw difference is a
revert - the grid is meant to generalize the arithmetic, not to move it.

## Landing 3 - the futures ledger

### What changes semantically

For a `Future`, a position is not an inventory of base currency. It is a signed
contract count whose value moves in the settlement currency, whose exposure is
collateralized by posted margin rather than by the notional, and whose
accumulated profit becomes CASH once a day.

### Target artifacts

`mogwai-engine/src/account.rs`:

```rust
pub(crate) struct PositionState {
    pub(crate) qty: Decimal,
    pub(crate) avg_px: Decimal,
    /// Last mark applied. Futures only; zero for spot, which never marks.
    pub(crate) mark_px: Decimal,
}

/// Per-instrument collateral policy, copied off `InstrumentProfile` into the
/// engine at construction. Absent for spot, REQUIRED for a future.
pub(crate) struct MarginPolicy {
    pub(crate) initial_per_contract: Decimal,
    pub(crate) maintenance_per_contract: Decimal,
    pub(crate) breach_action: BreachAction,
    /// UTC minute-of-day the settlement price is struck. Temporary: landing 4
    /// moves this onto the calendar and reinterprets it in local time.
    pub(crate) settlement_minute_of_day: u16,
}
```

`ConfiguredMargin` gains `settlement_minute_of_day: u16` in THIS landing (it was
absent from the landing 1 definition on purpose - landing 1 has no settlement
concept and refuses futures outright). `Engine` grows
`margin: HashMap<Symbol, MarginPolicy>` and one method each for the three
arithmetic facts:

```rust
impl Engine {
    /// `(mark - avg_px) * qty * multiplier`, signed, saturating. Zero for a
    /// spot instrument, which carries its P&L in its base balance instead.
    pub(crate) fn unrealized_pnl(&self, symbol: &Symbol) -> Decimal;
    /// Keyed by SYMBOL, because `PostedMargin` is a per-symbol row and
    /// `AccountState.margins` is a per-symbol `Vec`. Keying by currency would
    /// collapse two USD-settled futures into one row.
    /// `maintenance_per_contract * |qty|` per open futures position, plus
    /// `initial_per_contract * leaves_qty` over every resting non-reduce-only
    /// futures order on that symbol.
    pub(crate) fn margin_requirement(&self) -> HashMap<Symbol, PostedMargin>;
    /// Applies a mark to every futures position. Returns the events the pass
    /// must deliver: exactly one `AccountState` whenever any mark moved, plus
    /// whatever the breach policy produced.
    pub fn mark(&mut self, marks: &[(Symbol, Decimal)], ts: u64) -> MarkOutcome;
    /// Strikes the settlement price and moves variation margin into cash.
    pub fn settle(&mut self, marks: &[(Symbol, Decimal)], ts: u64) -> MarkOutcome;
}

/// Venue-originated output, counted separately from order-emitted output
/// because `reserve_swept` needs to know how much of the pass nobody's order
/// paid for.
pub struct MarkOutcome {
    pub events: Vec<ServerMessage>,
    /// Orders the liquidation policy synthesized. Zero in the common case.
    pub originated_orders: usize,
}
```

`apply_fill` gains a class arm. For `Future`:

- no base leg at all - there is no deliverable currency,
- realized P&L on a quantity-reducing fill books directly into the settlement
  balance as `(fill_px - avg_px) * closed_qty * multiplier` for a long being
  reduced (sign-flipped for a short),
- a quantity-increasing fill moves no cash; it moves the VWAP and raises the
  margin requirement, which `locked_balances` picks up.

`locked_balances` gains the same arm: a resting non-reduce-only futures order
reserves `initial_per_contract * leaves_qty` in the settlement currency
REGARDLESS OF SIDE (a short future posts the same collateral as a long), and an
open futures position reserves `maintenance_per_contract * |qty|`. Reduce-only
orders keep their existing unconditional skip, which is what makes two bracket
legs exclusive rather than additive.

`validate_submit`'s funds arm for a future compares
`free_balance(settlement) >= initial_per_contract * quantity` instead of
comparing against the notional. `validate_fill_funds` and `on_modify`'s funds
arm take the SAME margin comparison - a futures fill notional-checked against a
cash balance would reject every contract fill the moment margin is a fraction of
notional, which is always. The notional overflow refusal stays and now runs
through `def.notional()`, so it accounts for the multiplier.

`mogwai-protocol/src/messages.rs`:

```rust
pub struct AccountState {
    pub account_id: AccountId,
    pub balances: Vec<Balance>,
    pub positions: Vec<Position>,
    /// Posted collateral per instrument. Empty for a spot run, so a spot
    /// consumer sees byte-identical wire output to today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub margins: Vec<PostedMargin>,
    pub ts_event: u64,
}

pub struct PostedMargin {
    pub symbol: Symbol,
    pub currency: String,
    pub initial: Decimal,
    pub maintenance: Decimal,
}

pub struct Position {
    pub symbol: Symbol,
    pub quantity: Decimal,
    pub avg_px: Decimal,
    /// Last mark the venue applied. Absent when the venue does not mark, which
    /// is every spot instrument.
    #[serde(default, skip_serializing_if = "Decimal::is_zero")]
    pub mark_px: Decimal,
    /// Signed, in the instrument's settlement currency. Absent for spot.
    #[serde(default, skip_serializing_if = "Decimal::is_zero")]
    pub unrealized_pnl: Decimal,
}
```

`skip_serializing_if` on both new fields, not `#[serde(default)]` alone:
`default` governs DESERIALIZATION, and a zero `Decimal` still serializes. Without
the skip the byte-identity claim below is simply false, and its gate would fail
as the first draft specified it.

This is the one place the ruling in decision 2 needs care, and the distinction
is worth stating so it is not read as a contradiction: what crosses the wire
here is not the instrument's margin PARAMETER (that stays server-side, as the
ruling says) but the account's POSTED margin, which is account state and which
the venue is authoritative for. Nautilus has a first-class carrier for exactly
that - `MarginBalance::new_checked(initial: Money, maintenance: Money,
instrument_id: Option<InstrumentId>)` - and `generate_account_state` in
`mogwai-adapter/src/client/exec.rs` already takes a `Vec<MarginBalance>` it
currently always passes empty.

`mogwai-protocol/src/sizing.rs` grows:

```rust
/// One `PostedMargin` row: `symbol`, `currency`, two decimals, key names and
/// punctuation - about 130, rounded to 192 on top of the charged strings.
pub const MARGIN_ROW_MAX_BYTES: usize = 192 + ESC * (MAX_SYMBOL_LEN + MAX_CURRENCY_LEN);
```

`BookShape` grows `margins: usize`, populated in `Engine::book_shape()` (the one
place `BookShape` is built) as the count of symbols with either an open futures
position or a resting futures order, and counted by `account_state_max_bytes`.
`POSITION_ROW_MAX_BYTES` rises from 128 to 256 for the two new decimals.
`swept_fill_max_bytes` widens `margins` by `orders` for the same reason it
widens balances and positions: a sweep's first fill in a new symbol introduces
a margin row the pre-sweep snapshot never had.

### The sweeper pass, restructured

The current loop cannot host a mark, and the first draft's snippet would never
have executed. The pass is rewritten:

```rust
let mut last_swept_ns = sim_now_ns(sim);   // initialized once, before the loop
loop {
    // ... the existing select! on sleep and completion ...
    let to_ns = sim_now_ns(sim);
    let scans = { sweep.run.engine.lock().await.pending_scans() };
    // NO early `continue` on an empty scan set: a run holding an open futures
    // position and no resting order still has to mark.
    let results = if scans.is_empty() { Vec::new() } else { /* the existing
        group-by-symbol spawn_blocking walk, unchanged */ };
    let marks = /* read_last per symbol with an open futures position or a
        resting futures order, at to_ns */;
    let settlements = sweep.run.settlement_instants(last_swept_ns, to_ns);
    let settle_marks: Vec<Vec<(Symbol, Decimal)>> =
        settlements.iter().map(|at| /* read_last per symbol AT `at` */).collect();

    let mut engine = sweep.run.engine.lock().await;
    let (mut events, emitted) = engine.apply_scans(&results, to_ns);
    let mut originated = 0usize;
    for (at, marks_at) in settlements.iter().zip(&settle_marks) {
        let out = engine.settle(marks_at, *at);
        events.extend(out.events);
        originated += out.originated_orders;
    }
    let out = engine.mark(&marks, to_ns);
    events.extend(out.events);
    originated += out.originated_orders;
    let shape = engine.book_shape();
    drop(engine);
    last_swept_ns = to_ns;
    if events.is_empty() { continue; }
    deliver(&sweep.run, &shape, &events, emitted, originated, to_ns);
}
```

Four properties of that ordering are load-bearing:

**The mark runs last, and it owns the pass's account snapshot.** `apply_scans`
already emits one `AccountState` before `mark` would run, and a snapshot taken
before the mark reports stale `mark_px` and `unrealized_pnl`. `mark` therefore
SUPPRESSES the `apply_scans` snapshot when it runs on a futures book and emits
exactly one post-mark snapshot in its place: `apply_scans` gains a flag telling
it to withhold the snapshot, and `mark` returns the single authoritative one.
Exactly one `AccountState` per pass, before and after this change.

**Settlement is struck at the settlement INSTANT, not at the sweep boundary.**
`settlement_instants(from_ns, to_ns) -> Vec<u64>` returns every crossed instant
in `(from, to]` rather than a bool, and each gets its own `read_last` at its own
timestamp. A bool cannot name the price, and at `speed = 100` with a coarse
sweep interval one pass can span more than a sim day and cross more than one
settlement - which the bool form would book once, at whatever price the sweep
boundary happened to see. In landing 3 the instants come from the UTC
`settlement_minute_of_day`; landing 4 replaces the source, not the signature.

**Venue-originated output is reserved for.** `deliver` takes `originated`
alongside `emitted` and reserves `reserve_swept(shape, emitted + originated)`,
plus one flat `account_state_max_bytes(shape)` for the mark-only snapshot on a
pass where `emitted == 0`. A liquidation cascade emits order frames no client
order paid for; without this the admission contract is silently voided, which by
requirement 5 means the brick is not laid.

**`last_swept_ns` is real state**, initialized before the loop and advanced
unconditionally at the end of every pass including the ones that `continue`.
Advancing it only on delivery would let a settlement instant fall in a skipped
window and never be booked.

### The cost of marking, stated honestly

`FillSweep` gains `market_readings: Arc<fills::MarketReadingCache>`, the same
`Arc` the HTTP app state holds, so the sweep and the submit path share a bucket
when they land in one. That sharing is real but partial: the cache is a
SINGLE-ENTRY cache keyed on symbol plus bucket plus divergence multiplier plus
tick cap, so a mark over two futures symbols evicts itself between them, and a
mark contends with the submit path's entry. The honest statement is therefore
the opposite of the first draft's: a futures run pays roughly one `read_last`
walk per marked symbol per sweep pass, on the MISS path that
`notes/todo.md`'s 12.6 ms item is about. This landing consequently DOES move that
item's cost profile and says so; it does not close the item, and a
`criterion` bench of one mark pass over one and over four futures symbols is a
deliverable of this landing so the number is on record in
`reference/performance.md` rather than guessed.

### Settlement and breach

`settle` books, per open futures position, `(settle_px - avg_px) * qty *
multiplier` into the settlement balance and then sets `avg_px = settle_px` and
`mark_px = settle_px`. That is variation margin: the difference moves in actual
cash, which is why a losing futures position drains the account rather than
merely carrying a worse unrealized number.

Breach handling, evaluated at the end of every `mark` and every `settle`:

```
equity      = total_balance(settlement) + sum(unrealized_pnl over futures)
maintenance = sum(maintenance_per_contract * |qty|)
breach      = equity < maintenance
```

TOTAL balance, not `free_balance`. `free_balance` is total minus locked, and
`locked_balances` already reserves the maintenance requirement, so
`free_balance + unrealized < maintenance` subtracts maintenance twice and
declares a breach on an account with 3_000 cash against a 2_000 requirement.

Under `Refuse`, an engine flag is set that makes `validate_submit` reject every
non-reduce-only order with `"margin breach: account equity below maintenance
requirement"`, and it clears when equity recovers. Under `Liquidate`, the
engine synthesizes a reduce-only market order per open futures position, sized
to the whole position, and runs it through the ordinary submit path so it fills
through the existing band model with no new machinery - which is exactly the
property decision 1 relied on. Each such order increments
`MarkOutcome::originated_orders`.

The landing-1 boot refusal on `Future` is deleted here, and `margin` becomes
mandatory on a futures table.

### Gates

- `brokkr fmt`, then `brokkr check --gate`.
- New, `mogwai-engine`, run individually with
  `brokkr test -p mogwai-engine <NAME>`:
  - `a_futures_fill_books_no_base_currency_leg` - the ledger has exactly one
    currency entry after a futures buy.
  - `a_futures_position_values_at_multiplier_times_points` - two MNQ contracts
    bought at 21000.00 and marked at 21001.00 read an unrealized P&L of 4.00
    (1 point, multiplier 2, two contracts), not 2.00 and not 42002.00.
  - `a_resting_futures_order_reserves_margin_not_notional` - a resting buy of
    one MNQ contract at 21000 with `initial_per_contract = 2000` locks 2000,
    not 42000, and locks the same 2000 on the sell side.
  - `a_futures_fill_is_funds_checked_against_margin_not_notional` - the
    `validate_fill_funds` arm, which would reject every contract fill if it kept
    the notional comparison.
  - `two_reduce_only_legs_reserve_nothing_against_one_position` - the
    exclusivity property, pinned rather than assumed.
  - `daily_settlement_moves_unrealized_into_cash_and_resets_avg_px`.
  - `an_equity_above_maintenance_with_maintenance_locked_is_not_a_breach` - the
    3_000-cash, 2_000-maintenance case the double-counting formula fails.
  - `a_maintenance_breach_under_refuse_rejects_new_risk_but_not_reduce_only`.
  - `a_maintenance_breach_under_liquidate_closes_through_the_fill_band` -
    asserts the liquidating fill's price differs from the mark by a band draw,
    i.e. that it went through the ordinary submit path.
  - `margin_requirement_keeps_two_usd_settled_futures_as_two_rows`.
  - `worst_case_reservation_covers_actual_output` - the EXISTING test, extended
    with a futures book carrying margin rows AND with a liquidation cascade,
    which is the case `emitted` alone under-reserves.
- New, `mogwai-server`:
  - `a_futures_run_marks_with_no_resting_orders` - the pass reaches `mark` on an
    empty scan set, which the old early `continue` made impossible.
  - `a_pass_emits_exactly_one_account_state_after_marking` - the suppression
    contract.
  - `two_settlements_crossed_by_one_pass_book_at_their_own_prices` - the case a
    boolean `crosses_settlement` gets wrong.
- `python3 scripts/smoke.py futures --config crates/mogwai-server/tests/configs/mnq.toml`
  - a new `futures` smoke mode, registered in `MODES` and `MODE_CONFIGS`. NOT
  `brokkr run mogwai -- serve` followed by `smoke.py`: `serve` defaults to
  `--addr 127.0.0.1:0` and refuses port 0 without `--ready-fd`, and `smoke.py`
  spawns its own venue regardless, so the two-command form neither boots nor
  connects to what it booted. The fixture config
  `crates/mogwai-server/tests/configs/mnq.toml` and the smoke mode are both
  deliverables of this landing.
- A `criterion` bench of the mark pass, with its number recorded in
  `reference/performance.md`.

Keep/revert: kept if every gate above is green AND a spot run's `AccountState`
bytes are unchanged (the `skip_serializing_if` on `margins`, `mark_px` and
`unrealized_pnl` are what make that true, and
`a_spot_account_state_is_wire_identical_to_the_previous_shape` pins it).
A spot wire change is a revert.

## Landing 4 - the session calendar

### Target artifacts

`mogwai-data`, new module `generated/calendar.rs`:

```rust
/// Half-open local-time intervals within the week when the market is OPEN,
/// measured in minutes since local Sunday 00:00. `0 .. 10_080`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WeeklyWindow { pub start_minute: u32, pub end_minute: u32 }

#[derive(Debug, Clone, Deserialize)]
pub struct SessionCalendar {
    /// Minutes east of UTC the instrument's civil week is expressed in.
    /// Negative for the Americas. FIXED - no DST, declared unmodelled.
    pub utc_offset_minutes: i16,
    /// Sorted, non-overlapping, at most one wrapping past Sunday 00:00.
    /// Empty means always open, which is the crypto case and the default.
    pub open_windows: Vec<WeeklyWindow>,
    /// Local minute-of-day the settlement price is struck. Futures only.
    pub settlement_minute_of_day: Option<u16>,
}

impl SessionCalendar {
    pub fn validate(&self) -> Result<(), CalendarError>;
    /// Is the market open at this absolute nanosecond?
    pub fn is_open(&self, clock_ns: u64) -> bool;
    /// The first instant at or after `clock_ns` at which the market is open.
    /// `clock_ns` itself when already open. Always terminates: `validate`
    /// refuses a calendar with no open window at all.
    pub fn next_open_ns(&self, clock_ns: u64) -> u64;
    /// Every settlement instant in `(from_ns, to_ns]`, ascending. A pass at
    /// high `speed` can cross more than one.
    pub fn settlement_instants(&self, from_ns: u64, to_ns: u64) -> Vec<u64>;
}
```

`ConfiguredInstrument` gains
`pub(crate) calendar: Option<mogwai_data::SessionCalendar>` in THIS landing.
It was deliberately absent from landing 1's definition: a field whose type does
not exist yet cannot compile, and every landing boundary in this spec has to be
green on its own.

`validate` refuses: an empty-but-present window list (state absence as an
absent table, not an empty one), overlapping windows, a window with
`start == end`, more than one wrap, an offset outside `-720 ..= 840`, and a
`settlement_minute_of_day` that falls outside every open window - a settlement
price cannot be struck on a market that is shut.

MNQ is ten windows: Sunday 18:00 to Monday 16:15, Monday 16:30 to 17:00,
Monday 18:00 to Tuesday 16:15, and so on to Friday 16:30 to 17:00, at
`utc_offset_minutes = -300`, `settlement_minute_of_day = 960` (16:00). Saturday
carries no window at all and is therefore genuinely shut, which is the property
`SessionProfile`'s strict positivity cannot express. BTCUSDT carries no
calendar table, and nothing about its tape changes.

### The generator

`GeneratedSource` gains `calendar: Option<SessionCalendar>`, threaded through
the same constructor chain landing 2 widened.

The correction is applied at the END of `begin_event`, on the FINAL instant,
after the surge division and after any `ReopenGap` halt:

```rust
// ... existing: dt_ns from next_duration_ns divided by rate_mult, clock
// advanced, ReopenGap halt applied ...
if let Some(cal) = &self.calendar
    && !cal.is_open(self.clock_ns)
{
    // Jump the closure whole. No ticks are emitted inside it, the GARCH
    // state does not update across it, and the drift recentering does not
    // run - a shut market has no price process, and pretending otherwise
    // is what makes a reopen gap look like ordinary volatility.
    self.clock_ns = cal.next_open_ns(self.clock_ns);
}
```

Applying it inside `next_duration_ns` instead - as the first draft did - is
wrong in both directions: `begin_event` divides that return value by an armed
`FlowSurge`'s `rate_mult`, so a weekend jump can be shortened back INTO the
closure, and a candidate judged open before the division can land inside one
after it. The correction must be the last thing that touches the instant.

Three consequences that must be stated rather than discovered.

**`MAX_SESSION_GAP_NS` is not in the way.** The cap is 31_622_400 seconds - 366
days - and the CME weekend is 48 hours, so a calendar jump never approaches it
and no exemption is needed. The first draft asserted the opposite and proposed a
gate (`a_calendar_jump_is_not_capped_by_max_session_gap`) that asserts something
which cannot happen; it is deleted. The 80 days in the survey is the FAILURE the
constant guards against, not the constant. `closed_window_gap_ns` and
`SESSION_CLOSED_ARR_MULT` stay exactly as they are for the near-zero-share case,
which remains legal; the calendar is a second, hard mechanism layered above
them, not a replacement.

**`SessionProfile` keeps its strict-positivity guard, untouched.** Closure is
now its own concept, which is the resolution the problem statement asked for
("either relaxing it deliberately for the weekend case or expressing closure as
its own concept, not loosening the check"). This spec takes the second option.
The hour and day curves continue to shape intensity WITHIN an open session, and
a calendar-bearing instrument's `dow_weight` for a fully closed day is simply
never consulted.

**The checkpoint index must survive it.** `CheckpointIndex` binary-searches
snapshots by `clock_ns`, and a weekend is a 48-hour span with no tick in it.
A seek targeting an instant inside a closure must resolve to the last tick
before it, not fail. `seek_to` already handles sparse spans; the gate below
pins it against a span two orders of magnitude sparser than any it has seen.

`TAPE_PROTOCOL_VERSION` moves from 3 to 4 in this landing, per the same
`AGENTS.md` invariant landing 2 obeyed. Calendar-aware generation changes what a
seed produces for a calendar-bearing instrument, and the constructor surface
moves again.

### Order admission while closed

A closed market is not merely a silent feed, and the spec has to say which. The
ruling: while the calendar reports closed for an instrument,

- new market orders and marketable limits on that instrument are REJECTED with
  `"market closed"` - this is a market-state refusal, not a client-preference
  one, and it is what a real venue does,
- resting orders PERSIST and simply do not fill, because there is no tape to
  trigger against,
- `mark_px` FREEZES at the last print before the close, and the frozen mark is
  what unrealized P&L and the breach check read across the closure,
- settlement cannot be struck inside a closure, which `validate` already
  guarantees by refusing a settlement minute outside every open window.

Without this rule a market order during a weekend fills off the stale pre-close
reading, which is the single most misleading behaviour a forward test could
inherit.

### Server and adapter

`ConfiguredInstrument::calendar` is threaded into `InstrumentProfile` and from
there into `GeneratedSource`. `Run::settlement_instants` - already the sweeper's
call site since landing 3 - switches its source from the UTC
`settlement_minute_of_day` on `ConfiguredMargin` to the calendar's local-time
one, and the temporary field is deleted from `ConfiguredMargin`. The signature
does not move, so no landing 3 gate is re-blessed; the landing 3 settlement
tests configure a zero UTC offset precisely so that they read the same instants
either way.

The adapter needs nothing. A closed market is silence on a subscribed feed, and
the venue already has no liveness contract that a scheduled closure violates -
which is the point the problem statement made about a configured weekend being
legitimate silence the consumer can know about in advance.

### Gates

- `brokkr fmt`, then `brokkr check` (this landing does not touch
  `mogwai-adapter`).
- New, `mogwai-data`, each runnable as `brokkr test -p mogwai-data <NAME>`:
  - `a_calendar_weekend_emits_no_tick_and_reopens_exactly_on_the_boundary` -
    the CME calendar, one simulated fortnight, asserting zero ticks between
    Friday 17:00 and Sunday 18:00 local and a first tick at or after the reopen
    minute.
  - `the_maintenance_halt_is_expressible_at_sub_hour_resolution` - no tick
    between 16:15 and 16:30 local, which whole-hour bins provably cannot do.
  - `an_armed_flow_surge_cannot_shorten_a_closure_jump` - the same fortnight
    with a `FlowSurge` armed across the weekend, asserting still zero ticks
    inside it. This is the gate for applying the correction after the division
    rather than before, and the first draft's ordering fails it.
  - `checkpoint_resume_across_a_closure_is_byte_identical` - the existing
    `checkpoint_resume_is_byte_identical` shape, run over a calendar-bearing
    source spanning a weekend.
  - `a_calendar_with_no_open_window_is_refused` and
    `a_settlement_minute_outside_every_window_is_refused`.
  - `settlement_instants_returns_every_crossing_in_a_multi_day_span`.
- New, `mogwai-engine` / `mogwai-server`:
  - `a_market_order_while_closed_is_rejected_not_filled_off_a_stale_print`.
  - `a_resting_order_survives_a_closure_and_fills_after_the_reopen`.
  - `the_mark_freezes_across_a_closure`.
- The EXISTING `realism` test in `mogwai-data/src/generated/tests.rs` and the
  dwell family (`run_seeded_tape_dwell_is_bounded`,
  `dwell_is_bounded_across_run_seeds`) run against the CALENDARLESS default and
  must be unchanged. This is the landing's central risk and its central
  guarantee: the calendar is opt-in, absent from the default profile, and
  therefore cannot move an anchor fitted to a 24/7 crypto corpus. Any drift in
  `realism` is a revert.
- A calendar-bearing instrument gets its OWN dwell expectation rather than
  inheriting the crypto one, because a 48-hour scheduled gap trivially blows
  `max_gap_s` and `max_empty_hour_run_h`. New:
  `assert_calendar_dwell_excludes_closed_hours`, measuring dwell over OPEN
  hours only. Naming this is required by requirement 5: the existing gate does
  not reach the new behaviour, so the instrument that measures it is itself a
  brick, laid in this landing.
- `brokkr test -p mogwai-server version_string_reports_the_tape_version` -
  now reading 4.
- `python3 -m unittest discover -s analysis -t .` - unchanged and expected
  green; the offline analysis has no calendar concept and gains none.

Keep/revert: kept if the default-profile realism and dwell readings are
unchanged and the calendar tests pass. This landing has no re-bless: it adds a
behaviour reachable only under a config key nothing in the repo's defaults
sets.

## Landing 5 - fees

### The dependency, now discharged

Decision 8 recorded that a maker/taker schedule could not be implemented
honestly because the venue had no way to say which a fill was. The fill model
landed (`a214996` and follow-ons) and the classifier now falls out by
construction, in code that already exists: `on_submit` computes

```rust
let marketable = order.order_type == OrderType::Market
    || reading.is_some_and(|value| trades_through(order.side, trigger_px, value.last_px));
```

and the sweep's StopLimit arm computes the same predicate again at trigger time.

### The liquidity rule, stated per path

Liquidity side is decided at the instant the FILL is produced, not at the
instant the order arrives, and the code already has both branches:

- Market order, or a limit marketable on arrival: TAKER.
- Resting limit filled later by the sweep: MAKER.
- Triggered stop-market: TAKER.
- Triggered stop-limit that trades through at its own stated price in the same
  sweep hit: TAKER. One that does NOT trade through becomes a resting limit and
  is a MAKER fill when the sweep later fills it.

The first draft's blanket rule ("a triggered stop produces a market order and is
therefore a taker fill") is wrong for StopLimit, which the sweep judges "exactly
as a limit submitted at this instant" against its own price. Its gate would have
baked the wrong answer in.

### Target artifacts

`mogwai-protocol/src/messages.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiquiditySide { Maker, Taker }

pub struct OrderFilled {
    // ... unchanged fields ...
    pub commission: Decimal,
    /// Currency `commission` is denominated in. Always the instrument's
    /// settlement currency; carried explicitly so the adapter stops inferring
    /// it from the quote leg, which has no meaning for a future.
    pub commission_currency: String,
    pub liquidity_side: LiquiditySide,
    pub ts_event: u64,
}
```

`ORDER_EVENT_MAX_BYTES` rises by `ESC * MAX_CURRENCY_LEN` plus a flat 64 for the
key names and the enum spelling, and SO DOES `FILL_ROW_MAX_BYTES`, by the same
amount. They are separate constants: `FILL_ROW_MAX_BYTES` bounds a recorded fill
row inside a `FillSnapshot`, and widening only the event constant under-reserves
every snapshot of a fee-bearing run.

`mogwai-engine` owns the schedule:

```rust
/// Built from `ConfiguredFees` at engine construction, one per symbol. The
/// engine owns it because commission is booked in `apply_fill`, and nothing
/// server-side may reach into a fill after the fact.
pub(crate) struct FeeSchedule { pub maker: FeeRate, pub taker: FeeRate }
```

`Engine` carries `fees: HashMap<Symbol, FeeSchedule>` copied off
`InstrumentProfile`. Validation, in `validate_instrument_def` at boot:
`BasisPoints.rate` in `0 ..= 1000` (ten percent, an absurd but finite ceiling)
and `PerContract.amount >= 0`; a negative rate of either kind is refused by name
rather than silently paying the client a rebate. Rebates are not modelled.

One function computes the charge:

```rust
/// Commission for one fill, always non-negative and always in the
/// instrument's settlement currency. `BasisPoints` charges against
/// `def.notional(qty, px)` so a future is charged on multiplier-aware
/// notional; `PerContract` charges `amount * qty` and ignores price entirely,
/// which is the CME shape.
fn commission(def: &InstrumentDef, sched: &FeeSchedule, side: LiquiditySide,
              qty: Decimal, px: Decimal) -> Decimal;
```

`apply_fill` needs no structural change: the direction-aware handling is
already wired and already correct (a buy's cost adds commission, a sell's
proceeds subtract it), with a comment saying so and saying it is waiting for a
source. This landing supplies the source. For a future there is no base leg, so
the commission is a straight debit from the settlement balance.

`mogwai-adapter/src/client/exec.rs`: the two hardcoded `LiquiditySide::Taker`
sites read `fill.liquidity_side`, and the two `convert::money(fill.commission,
quote_currency)` sites resolve `fill.commission_currency` instead. The existing
degrade-with-a-warning behaviour on an unrepresentable amount is kept verbatim.

### The fee havoc arm

The config/havoc line says a fee schedule that changes mid-run, or a venue
charging more than it advertised, is legitimate havoc. One new arm on
`control::Divergence`:

```rust
/// Charge `mult` times the configured schedule for `window_ms`. The venue
/// advertised one number and bills another, which is a real venue failure
/// mode and is invisible to any consumer that does not reconcile its own
/// cost model against reported commission.
FeeSurcharge { mult: Decimal, window_ms: u64 },
```

Storage and lifecycle, spelled out because "armed like its siblings" is not an
artifact: the engine holds ONE optional `FeeSurchargeWindow { mult: Decimal,
start_ns: u64, end_ns: u64 }`, single-slot, replaced outright on re-arm (the
later arm wins and the earlier is discarded, matching every other single-slot
divergence). It is evaluated in SIM time inside `commission`, by testing
`(start_ns..end_ns).contains(&fill_ts)`; it is cleared lazily on the first
evaluation past `end_ns` rather than by a timer, so there is no task and no
wall-clock dependence. Validated like its siblings: `mult` in `(0, 100]`,
`window_ms` at most `MAX_DIVERGENCE_MS`. Documented in `reference/havoc.md` in
the same landing.

### Gates

- `brokkr fmt`, then `brokkr check --gate` - touches `mogwai-adapter`.
- New, `mogwai-engine`:
  - `a_resting_limit_filled_by_the_sweep_books_the_maker_rate`
  - `a_marketable_order_books_the_taker_rate`
  - `a_triggered_stop_market_books_the_taker_rate`
  - `a_triggered_stop_limit_that_trades_through_books_the_taker_rate`
  - `a_triggered_stop_limit_that_rests_books_the_maker_rate_when_it_fills` -
    the pair of cases a blanket stop-is-taker rule gets wrong.
  - `per_contract_fees_ignore_price_and_scale_with_contracts` - four MNQ
    contracts at any price cost `4 * amount`.
  - `basis_point_fees_on_a_future_charge_multiplier_aware_notional` - the
    regression that a naive `qty * px` would produce, wrong by the multiplier.
  - `a_negative_fee_rate_refuses_boot`.
  - `a_fee_surcharge_bills_above_the_advertised_schedule_and_expires_on_sim_time`
  - `a_re_armed_fee_surcharge_replaces_the_earlier_window`
  - `worst_case_reservation_covers_actual_output` - re-run against the widened
    `ORDER_EVENT_MAX_BYTES` and `FILL_ROW_MAX_BYTES`, with a recorded fill
    snapshot in the book so the second constant is actually exercised.
- New, `mogwai-adapter`: `a_maker_fill_reports_maker_liquidity_side` and
  `a_futures_commission_books_in_the_settlement_currency`, the latter being the
  one that fails today by inferring the quote leg.
- `python3 scripts/smoke.py fees --config crates/mogwai-server/tests/configs/fees.toml`
  - a new smoke mode asserting a non-zero commission on a fee-configured run.
  The live path is the whole reason fees exist, since nautilus computes
  commission client-side only in its simulated matching engine.

Keep/revert: kept if the gates are green and a fee-free run (no
`[instrument.fees]` table, which is what every shipped config has) books zero
commission with the same wire bytes as before apart from the two new fields.

## Landing 6 - position identity

`position_id` appears nowhere in `mogwai-adapter/src` - the adapter drops what
nautilus hands it on submission, constructs live fills with `position_id = None`
and position reports with no venue position id. That is a gap regardless of
class, and it decides whether two opposing orders net or hedge, which for a
futures account is the difference between one position and two.

### The wire, end to end

A venue-assigned hedging id is useless if it stops at `SubmitOrder`. The field
travels the whole path:

- `SubmitOrder` gains `#[serde(default, skip_serializing_if = "Option::is_none")]
  pub position_id: Option<String>`, capped at `MAX_CLIENT_ID_LEN` by
  `validate_submit_order`.
- `OrderFilled` gains the same field, carrying the id the venue ACTUALLY booked
  the fill against - which under hedging may be one the venue assigned rather
  than one the client supplied.
- `OrderStatusInfo` and the recorded fill row gain it, so a reconciliation
  after a reconnect agrees with the live stream.
- `Position` gains it, so a snapshot names which position is which.

The engine keys `Account::positions` on `(Symbol, Option<String>)` and the run
carries an `oms_type: OmsType` config knob (`Netting` default, `Hedging`
opt-in): under `Netting` the key's position component is forced to `None` and a
client-supplied id is echoed back but not honoured for keying; under `Hedging`
it is the key, and an order carrying no id opens a fresh venue-assigned one
(`"{symbol}-{monotonic counter}"`, stable within a run, capped at
`MAX_CLIENT_ID_LEN`).

### No OMS refusal, and no account-type refusal

`mogwai-adapter/src/config.rs` carries `oms_type` on the exec config and
defaults `account_type` to `AccountType::Cash`. The venue does NOT refuse a
client whose declared OMS or account type differs from the run's. mogwai must
not reject a client over a position-management knob: it supports BOTH netting
and hedging, defaults to netting, and serves whichever the client asks for.
Hedging is built here, not refused.

What replaces the refusal is observability: `/health` reports the run's
`oms_type`, and the server logs it once at boot. `/instruments` is left alone -
it serves `Vec<InstrumentDef>`, the OMS is a run-level property and not an
instrument one, and the first draft's "boot-time check against what
`/instruments` reports" was reading a field that endpoint neither has nor
should have.

A consumer configured `AccountType::Cash` against a futures run is likewise not
refused. Nautilus' `CashAccount` has no margin storage, so the `MarginBalance`
rows the adapter forwards are dropped client-side; that is a consumer-side
consequence recorded under accepted costs, not a reason for the venue to
disconnect anybody.

`POSITION_ROW_MAX_BYTES`, `ORDER_STATUS_ROW_MAX_BYTES`, `ORDER_EVENT_MAX_BYTES`
and `FILL_ROW_MAX_BYTES` each rise by `ESC * MAX_CLIENT_ID_LEN`.

### Gates

- `brokkr fmt`, then `brokkr check --gate`.
- New, `mogwai-engine`: `netting_collapses_two_opposing_fills_into_one_position`,
  `hedging_keeps_two_opposing_fills_as_two_positions`,
  `a_hedging_order_without_a_position_id_opens_a_venue_assigned_one`,
  `a_hedging_fill_reports_the_position_id_the_venue_booked_it_against`,
  `worst_case_reservation_covers_actual_output` re-run against all four widened
  constants.
- New, `mogwai-adapter`: `a_submitted_position_id_reaches_the_wire`,
  `a_venue_assigned_position_id_reaches_the_nautilus_fill_report`, and
  `a_cash_configured_client_still_connects_to_a_futures_run` - the explicit
  no-refusal gate.
- New, `mogwai-server`: `health_reports_the_run_oms_type`.

Keep/revert: kept if netting behaviour under the default is identical to
today's symbol-keyed behaviour. Netting is the default precisely so this
landing is a no-op for every existing consumer.

## Landing 7 - presets and provenance

### Target artifacts

Presets are committed TOML under `crates/mogwai-server/presets/` -
`mnq.toml`, `mes.toml`, `btcusdt.toml`, `ethusdt.toml`, `solusdt.toml` - each a
complete `[instrument]` table plus a mandatory `[provenance]` table, embedded
with `include_str!` so a preset ships in the binary and a fresh clone needs no
data directory.

The merge is a concrete algorithm over a concrete representation, not a
disposition:

```rust
/// The operator's `[instrument]` table BEFORE typed deserialization, so a
/// preset can be merged into it and the result checked once. `toml::Table`
/// rather than `ConfiguredInstrument`, because a half-specified instrument does
/// not typecheck and must not have to.
pub(crate) struct RawInstrumentTable(toml::Table);

/// Dotted paths into the instrument table: `class.multiplier`,
/// `margin.initial_per_contract`, `generator.typical_notional`. One flat
/// namespace, so provenance and overrides key the same way.
pub(crate) type KnobPath = String;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Provenance {
    /// Measured from a named corpus over a named window.
    Fitted { corpus: String, window: String },
    /// Computed from other fitted quantities. `from` names them.
    Derived { from: Vec<KnobPath> },
    /// Chosen. `rationale` says why, and is not optional.
    Declared { rationale: String },
}

/// `[provenance]` is a MAP from dotted knob path to entry, not a lone enum -
/// a single `Provenance` cannot express "every knob has one", which is the
/// promise being made.
pub(crate) type ProvenanceMap = HashMap<KnobPath, Provenance>;
```

Merge algorithm, in order:

1. Flatten both the preset's `[instrument]` table and the operator's into dotted
   `KnobPath -> toml::Value` maps. Flattening stops at a leaf scalar or array;
   `[instrument.override]` is excised first and not flattened as a knob.
2. Any path present in BOTH flattened maps is a BOOT ERROR naming the preset
   value and the operator value.
3. Every path in `[instrument.override]` must exist in the preset's flattened
   map; one that does not is a boot error naming the typo. Each override
   replaces the preset value and is LOGGED at boot with both values.
4. The merged flat map is unflattened back to a `toml::Table` and deserialized
   into `ConfiguredInstrument`, which runs every landing-1 validator unchanged.

```toml
preset = "MNQ"          # in the operator's own [instrument] table
[instrument.override]   # the sanctioned place to differ from a preset
"class.multiplier" = "2"
```

There is no silent winner: a key set both by the preset and at the top level of
`[instrument]` refuses boot, and the override sub-table is the unambiguous form.

Boot REFUSES a preset whose `[provenance]` map does not carry an entry for
every path in the preset's flattened knob map. That is the mechanism decision 10
asked for: a declared 0.076 s cadence and a fitted one look identical in a
struct, and this project already refused a model (queue-ahead, 2026-08-02) for
exactly that class of reason. The provenance of the shipped presets is honest
about the asymmetry the problem statement recorded - BTC, ETH and SOL cadence
knobs are `Fitted` against the Binance trade-level archives; MNQ and MES cadence
is `Derived` from 15-second OHLCV bar counts and their clustering constants are
`Declared` with a rationale saying they come from nowhere at all.

A new `mogwai presets [NAME]` CLI subcommand lists the presets and prints one
preset's knobs alongside its provenance, so the asymmetry is visible to whoever
picks one rather than buried in a commit message.

### Gates

- `brokkr fmt`, then `brokkr check`.
- New, `mogwai-server`:
  - `every_shipped_preset_parses_and_validates` - iterating the embedded set,
    so a preset cannot be committed broken.
  - `every_shipped_preset_declares_provenance_for_every_knob_it_sets` -
    comparing the flattened knob map against the provenance map key-for-key.
  - `a_preset_key_restated_at_the_top_level_refuses_boot`.
  - `an_override_of_a_path_the_preset_does_not_set_refuses_boot`.
  - `an_override_table_entry_wins_and_is_logged_with_both_values`.
  - `the_mnq_preset_reads_two_dollars_per_point_and_fifty_cents_per_tick` - the
    two numbers a wrong preset would be silently wrong about.
- `python3 scripts/smoke.py futures --config crates/mogwai-server/tests/configs/mnq-preset.toml`.

Keep/revert: kept if every preset validates and the no-preset path is
unchanged.

## Stopping rule

The teardown stops at the instrument definition, the size grid, the ledger, the
session mechanism, the fee path and the preset layer. Explicitly NOT in scope:

- **Any change to broadarrow.** broadarrow is a separate repository that depends
  on this one and is never a build input here; its preferences are not authority
  over mogwai's surface. Its MOGWAI venue resolver currently rejects every
  derivative product and constructs the exec client from defaults, so the known
  consequence of this spec is stated plainly: the capability lands here and the
  only current consumer cannot yet reach it. Making it reachable is work in that
  repository, not a companion change specified here, and no gate in this
  document depends on it.
- **Contract rolls, expiry, activation and front-month switching.** Decision 4
  settled the synthetic continuous instrument. The accepted cost, restated so
  nobody rediscovers it as a defect: no forward test on MOGWAI can exercise a
  contract roll.
- **Holidays, early closes and DST.** Decision 5. A run either spans a holiday
  or does not; if it does, the strategy sees an ordinary day.
- **Equities, physical delivery, short-sale rules, corporate actions.**
  Decision 3. AAPL and MCL were illustrations of a complete config surface, not
  requests. Adding a class later is code rather than config, and that is the
  honest boundary.
- **Cross-instrument correlation.** Strategies are single-instrument, so
  independent per-symbol tapes are correct rather than defective.
- **Fee rebates and tiered schedules.** A negative rate refuses boot. Real
  maker rebates exist; modelling them is a later decision, not a silent one.
- **Whether BTC and ETH genuinely need different process constants.** A fitting
  question for whoever authors each preset, answerable only when trade-level
  archives spanning years arrive. This spec makes the difference EXPRESSIBLE,
  which is all that gates anything.
- **The 12.6 ms cache-miss market reading, and the 0.1603 duration ACF anchor.**
  Both are open `notes/todo.md` items with their own owners. Landing 3 MOVES the
  first one's cost profile - a futures run marks per symbol per pass on the miss
  path - and records the measured number; it does not close the item. Landing 4
  does not touch the duration process.
- **`MarketIfTouched`, trailing stops and two-leg brackets.** Ruled out rather
  than deferred.

## Accepted costs, stated rather than discovered

**A nautilus margin account computes zero margin locally.** Decision 2 keeps
the instrument's margin parameters off the wire, so
`FuturesContract.margin_init` and `margin_maint` are `None` and a consumer's
own `calculate_initial_margin` returns zero. mogwai's ledger is authoritative
and reports posted margin as `AccountState.margins`, which the adapter forwards
as `MarginBalance` rows. A consumer that trusts its local computation instead
of the reported state will disagree with the venue. That is the correct
division of authority for a live venue adapter, and it is a real difference
from the backtest path a consumer may be used to.

**A cash-configured consumer discards the margin rows.** `MogwaiExecClientConfig`
defaults `account_type` to `AccountType::Cash`, and nautilus' `CashAccount` has
no margin storage, so a client that leaves the default while trading futures
sees `MarginBalance` rows silently dropped on its own side. mogwai does not
refuse such a client - it is a client-side configuration consequence, and
refusing a connection over it would be the venue legislating a consumer's
preferences.

**Marking costs a tape walk per futures symbol per sweep pass.** The single-entry
`MarketReadingCache` cannot hold more than one symbol-bucket, so a multi-symbol
futures run pays the miss path. The number is measured and recorded in
`reference/performance.md` by landing 3 rather than assumed.

**The integral size floor lifts the realized mean above the notional target.**
One contract is the smallest printable size, so the lower tail of the lognormal
piles onto the floor. A grid whose median sits near one contract therefore
prints a mean notional above `typical_notional`. Landing 2 measures it.

**Fees and margin are treated as instrument identity, which is not how markets
work.** Real fee schedules vary by account tier and liquidity role, and CME
margin varies by product, volatility, portfolio and time. A fixed per-contract
margin and a fixed schedule are declared simplifications of the venue's model,
not descriptions of the market's, and `reference/architecture.md` says so in
the same landing that introduces them.

**Landing 1 breaks every existing config file.** `class` is required and
`deny_unknown_fields` is on, so a top-level `base`/`quote` table stops parsing.
Every shipped config and fixture migrates in that same commit, and the boot error
names the replacement table.

**The blast radius is the wire.** `InstrumentDef` is a wire type, so landing 1
alone reaches the protocol crate, the engine's four funds sites and ledger, the
adapter's conversion, the config surface and every instrument fixture in the
test suite; landing 2 reaches the generator's grid and the tape version. Per the
user's standing instruction, resource cost shapes nothing here; this is named as
scope, not as a reason to do less.

## Documentation owed, per landing

Bundled with the code commit each belongs to, never committed alone:

- Landing 1: `reference/config.md` gains the `[instrument.class]` surface and
  documents the migration off top-level `base`/`quote`;
  `reference/architecture.md` stops describing the venue as spot-only;
  `reference/glossary.md` gains instrument class, multiplier and tick value.
- Landing 2: `reference/architecture.md` gains the size grid and the tape
  version bump to 3; `reference/glossary.md` gains contract size grid.
- Landing 3: `reference/architecture.md` gains the margin ledger, mark-to-
  market, settlement and the restructured sweep pass;
  `reference/performance.md` gains the mark-pass measurement;
  `reference/glossary.md` gains posted margin and variation margin.
- Landing 4: `reference/config.md` gains the calendar table;
  `reference/architecture.md` distinguishes a scheduled close (config) from
  `ReopenGap` (havoc, unscheduled only) and records the bump to 4.
- Landing 5: `reference/havoc.md` gains `FeeSurcharge`; `reference/config.md`
  gains the fee table; the standing claim that the engine books zero commission
  is deleted from `notes/todo.md`'s hardcoded-value inventory.
- Landing 6: `reference/config.md` gains `oms_type`; `docs/` records that the
  venue serves both OMS types and refuses neither.
- Landing 7: `reference/cli.md` gains `mogwai presets`; `docs/` gains a preset
  guide, since choosing a preset is a usage question rather than a
  how-it-is-built one.

When landing 7 lands, `notes/problem-instrument-model.md` is DELETED and this
spec with it, per the todo file's own rule: what must endure moves into
`reference/`, and what does not, dies.

## Findings rejected

Both review reports are folded above except for the following, which were
adjudicated and rejected. They are recorded so nobody re-raises them as gaps.

- **R2 finding 1, the broadarrow companion change.** Rejected as out of scope.
  broadarrow is a separate repository that depends on this one; its venue
  resolver rejecting derivatives is work in that repository. The consequence is
  recorded in the stopping rule instead. The other half of that finding - the
  OMS refusal - was VALID and is fixed above by deleting the refusal entirely.
- **R2 finding 1 and R1 finding 5, refusing an OMS or account-type mismatch.**
  Rejected as a design error in the first draft rather than adopted from the
  reviewers. The venue supports both netting and hedging, defaults to netting,
  accepts whichever the client asks for, and never rejects a client over a
  position-management or account-type knob. Hedging is built in landing 6; the
  cash-account consequence is recorded as an accepted cost.
