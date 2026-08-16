# Choosing an instrument preset

An operator names a symbol and mogwai resolves its tape knobs. Three presets ship inside the `mogwai`
binary - no data directory, no network fetch, nothing outside the executable
itself.

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

Name the boot symbol at top level. If its name matches a shipped preset, case-insensitively, that
preset supplies the whole bundle.

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

A preset is a named knob bundle, not an admission record. Every symbol is
servable: an unmatched symbol resolves through the BTCUSDT default under its
own name. Presets improve a symbol's tape; they do not authorize the symbol.
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
  identical generator paths, so a tape differing from BTCUSDT's only in the
  symbol identity, at a BTC price level. They were retired 2026-08-09 because
  they added identity-only rows to the measurement oracle while contributing no
  distinct dynamics. That retirement says nothing about which symbols the venue
  may serve: a symbol with no preset is served the default tape, and no fit is
  a precondition of being served.)

A preset can itself be built as an override of another preset (MES over
MNQ); that nesting is internal to how the presets are authored; from the
operator's config, `preset = "MES"` behaves identically to any other preset
name.

## Serving a symbol without a preset

Every string is served. An unmatched symbol gets the BTCUSDT preset's spot,
always-open, USDT-settled knobs under its own name. BTCUSDT is the default
because it makes no calendar or margin claim about an unfitted symbol, its
settlement currency is funded by the shipped balances, and its dynamics were
fitted from trade-level archives. Name a futures preset explicitly when a
future-shaped bundle is required. The unmatched symbol wears the default
preset's shape, never its tape path, because its requested label enters the
seed derivation.
