# Choosing an instrument preset

A symbol arrives and mogwai resolves its instrument knobs. The symbol may come
from the run config, from a websocket binding a river, or from a history query -
resolution is the same either way, and it is TOTAL: every wire-legal symbol
resolves to a shape, so nothing is refused for wanting a preset. Three presets
ship inside the `mogwai` binary - no data directory, no network fetch, nothing
outside the executable itself.

A preset is a named bundle of instrument knobs, not an admission record. Presets
make a symbol's river BETTER; they never decide whether it can be served.

## Listing and inspecting presets

```sh
mogwai presets
```

prints the three names. To see what a given preset actually sets, name it:

```sh
mogwai presets MNQ
```

This prints the preset's full TOML, including its `[provenance]` map - one
entry per knob, saying whether the number was measured (`fitted`), computed
from another fitted number (`derived`), or picked by hand (`declared`, with a
one-line reason). Read this before you run with a preset: the BTCUSDT
preset's
cadence numbers are fitted against Binance trade-level archives, while the
index-future presets' cadence is `derived` from 15-second OHLCV bars and their
clustering constants are `declared` with a rationale that says, plainly, that
they come from nowhere at all. A preset is not a claim that every number in it
is equally trustworthy, and the provenance map is how you find out which ones
are.

## Selecting a bundle in your config

Name the boot symbol at top level - the river a consumer gets when it binds
without naming one. If its name matches a shipped preset, case-insensitively,
that preset supplies the whole bundle.

```toml
symbol = "MNQ"
```

That is a complete config. Resolution uses this precedence: a preset bundle,
then default `[instrument]` knobs, then matching `[symbols.<SYM>]` knobs. A
per-symbol `preset` beats a default preset key, which beats a preset matching
the symbol, which beats the BTCUSDT default. An explicit preset remains useful
for serving one bundle under another name:

```toml
symbol = "FOOBAR"

[symbols.FOOBAR]
preset = "MNQ"
```

`[instrument]` is the DEFAULT overlay: it applies to every symbol this run
resolves, including one no config mentions, so a `preset` written there becomes
the bundle for the whole run. `[symbols.<SYM>]` is the same overlay shape
applied to one symbol on top of it.

## Overriding a knob

If you want the preset's shape but a different value for one or two knobs,
use `[instrument.override]` with dotted paths:

```toml
[instrument]
preset = "MNQ"

[instrument.override]
"class.multiplier" = "3"
```

Two rules, both enforced at boot with a message that names the problem:

- A top-level key in `[instrument]` is a legal explicit choice, logged with
  both values. It replaces the knob the bundle sets, or adds an optional
  section - `fees`, `margin`, `calendar` - the bundle leaves out. A key that is
  not an instrument field refuses boot by name. `[instrument.override]` is
  still the only way to reach a dotted path.
- Overriding a DOTTED path the bundle does not set refuses boot, so a typo in
  the override table is caught rather than silently ignored. The message names
  the bundle that was chosen.

Every override is logged at startup with both the preset's value and the
value you supplied, so a run's log makes the deviation from the preset
visible without you having to diff two TOML files.

Requested labels are case-exact river identities. Preset names and
`[symbols.*]` overlay keys are matched case-insensitively, so `mnq` may select
the MNQ knobs while remaining a different river and label from `MNQ`.

## What the shipped presets are

- **MNQ** - Micro E-mini Nasdaq-100 future. Two dollars per index point,
  whole-contract sizing, the published CME Sunday-evening-through-
  Friday-evening session with the daily maintenance halt and settlement
  window, margin posted per contract, `breach_action = "liquidate"`.
  Its cadence, size distribution, volatility scalar, start price, quote
  seams and - since tape protocol 11 - its hourly session arrays are fitted
  from the delivered July 2026 MNQ TBBO month; the preset's provenance map
  names the corpus per knob and the fit artifact `analysis/mnq-fit.json`
  records the estimators and verdicts. The protocol-11 session refit
  measures the arrays in the units the runtime applies: arrival intensity
  from inferred-parent counts (14.5x peak-to-trough, replacing the NQ-bar
  volume proxy) and per-parent volatility from quote-mid returns (nearly
  flat and slightly inverted - overnight trades individually move a touch
  more than cash-session trades), which together restore realistic Asia
  and London session amplitude at bar scale. The day-of-week weights keep
  their NQ-bar lineage.
- **MES** - Micro E-mini S&P 500 future. Built as an override of the MNQ
  preset: same session calendar and margin shape, five dollars per index
  point instead of two. That means MES BORROWS the fitted MNQ values - apart
  from the identity overrides (symbol, underlying, multiplier and start
  price), every fitted generator knob, quote seam and session curve is MNQ
  evidence, and the inherited corpus strings say so, so no MES corpus is
  implied. This is
  a stated stopgap product approximation, not a claim that MNQ evidence
  validates MES; a small ES/MES purchase is the recorded route to ending
  the borrow.
- **BTCUSDT** - a spot pair against USDT, always open (no calendar table),
  cadence and size distribution fitted against 30 days of Binance
  trade-level archives. (ETHUSDT and SOLUSDT presets shipped for a while as
  thin overrides of this one, restating only the symbol and base currency -
  identical generator paths, so a river differing from BTCUSDT's only in the
  symbol identity, at a BTC price level. They were retired 2026-08-09 because
  they added identity-only rows to the measurement oracle while contributing no
  distinct dynamics. That retirement says nothing about which symbols the venue
  may serve: a symbol with no preset is served the default shape, and no fit is
  a precondition of being served.)

A preset can itself be built as an override of another preset (MES over
MNQ); that nesting is internal to how the presets are authored; from the
operator's config, `preset = "MES"` behaves identically to any other preset
name.

## Serving a symbol without a preset

Every wire-legal string is served. An unmatched symbol gets the BTCUSDT preset's
spot, always-open, USDT-settled knobs under its own name. BTCUSDT is the default
because it makes no calendar or margin claim about an unfitted symbol, its
settlement currency is funded by the shipped balances, and its dynamics were
fitted from trade-level archives. Name a futures preset explicitly when a
future-shaped bundle is required. The unmatched symbol wears the default
preset's shape, never its river, because its requested label enters the
seed derivation - two symbols on the same bundle run different rivers.

A symbol nobody configured materializes its own river the first time it is
asked for, and from then on it appears in `/instruments` alongside the
configured shapes. Materialization is the one bounded resource here: a run
serves at most 256 distinct rivers, and the 257th is refused loudly, naming the
cap. That is a trust contract for a venue driven by its owner's own agents, not
a quota to plan around.

## Funding decides which presets your run can serve

A shape can only trade if its SETTLEMENT currency is funded in `[balances]`.
The rule lands in two places. A shape you configured is checked at boot: an
unfunded one refuses the run rather than rejecting every buy minutes in. Every
shipped preset is resolved at boot too, and one whose settlement currency is
unfunded is recorded as funding-barred - a consumer that later asks for a symbol
landing on that shape is refused when it binds, with a message naming the
symbol and the currency.

This bites the shipped default. The default balances fund USDT only, which
covers BTCUSDT and every unmatched symbol resolving through it, but NOT the
USD-settled MNQ and MES bundles. Fund USD in `[balances]` for a run whose
consumers may ask for the index futures. The payoff is that a funds rejection on
a served shape then means depletion and only depletion.
