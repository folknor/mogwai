# Choosing an instrument preset

An operator who wants MNQ, MES or BTCUSDT does not have to
hand-write an `[instrument]` table. Three presets ship inside the `mogwai`
binary - no data directory, no network fetch, nothing outside the executable
itself - and a config selects one by name.

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

## Selecting a preset in your config

Put `preset = "MNQ"` inside your `[instrument]` table. Nothing else goes in
that table directly - the preset supplies the whole instrument definition.

```toml
[instrument]
preset = "MNQ"
```

That's a complete, valid `[instrument]` table. Boot merges the preset's
knobs in and validates the result exactly as if you had written every field
by hand.

## Overriding a knob

If you want the preset's shape but a different value for one or two knobs,
use `[instrument.override]` with dotted paths:

```toml
[instrument]
preset = "MNQ"

[instrument.override]
symbol = "MNQZ6"
"class.multiplier" = "2"
```

Two rules, both enforced at boot with a message that names the problem:

- A key set both by the preset and restated at the top level of
  `[instrument]` refuses boot. `[instrument.override]` is the only sanctioned
  place to differ from a preset - there is no silent-last-writer-wins.
- Overriding a path the preset does not set also refuses boot, so a typo in
  the override table is caught rather than silently ignored.

Every override is logged at startup with both the preset's value and the
value you supplied, so a run's log makes the deviation from the preset
visible without you having to diff two TOML files.

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
  symbol identity, at a BTC price level; they were retired 2026-08-09 - an
  instrument that owes a realistic tape owes a corpus, a measurement and a
  fit, and these had a preset only.)

A preset can itself be built as an override of another preset (MES over
MNQ); that nesting is internal to how the presets are authored; from the
operator's config, `preset = "MES"` behaves identically to any other preset
name.

## Inventing your own instrument

Presets are a convenience, not a requirement. Skip `preset` entirely and
write the full `[instrument]` table - `[instrument.class]`, and for a future
`[instrument.margin]`, plus optionally `[instrument.fees]` and
`[instrument.calendar]` - to model an instrument no preset covers (MCL, AAPL,
or any other bundle of the same knobs). See `docs/config.md` for the
complete field-by-field surface; this guide covers only the preset mechanism
that sits on top of it.
