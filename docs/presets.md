# Choosing an instrument preset

An operator who wants MNQ, MES, BTCUSDT, ETHUSDT or SOLUSDT does not have to
hand-write an `[instrument]` table. Five presets ship inside the `mogwai`
binary - no data directory, no network fetch, nothing outside the executable
itself - and a config selects one by name.

## Listing and inspecting presets

```sh
mogwai presets
```

prints the five names. To see what a given preset actually sets, name it:

```sh
mogwai presets MNQ
```

This prints the preset's full TOML, including its `[provenance]` map - one
entry per knob, saying whether the number was measured (`fitted`), computed
from another fitted number (`derived`), or picked by hand (`declared`, with a
one-line reason). Read this before you run with a preset: the crypto presets'
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
  Its cadence, size distribution, volatility scalar, start price and quote
  seams are fitted from the delivered July 2026 MNQ TBBO month; the preset's
  provenance map names the corpus per knob and the fit artifact
  `analysis/mnq-fit.json` records the estimators and verdicts.
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
- **BTCUSDT**, **ETHUSDT**, **SOLUSDT** - spot pairs against USDT, always
  open (no calendar table), cadence and size distribution fitted against 30
  days of Binance trade-level archives. ETHUSDT and SOLUSDT are built as
  overrides of the BTCUSDT preset, restating only the symbol and base
  currency; their cadence provenance is therefore the same fitted BTC
  numbers, not an independently fitted ETH or SOL corpus - which is exactly
  the kind of thing the provenance map exists to surface rather than hide.

A preset can itself be built as an override of another preset (MES over MNQ,
ETHUSDT and SOLUSDT over BTCUSDT); that nesting is internal to how the
presets are authored; from the operator's config, `preset = "MES"` behaves
identically to any other preset name.

## Inventing your own instrument

Presets are a convenience, not a requirement. Skip `preset` entirely and
write the full `[instrument]` table - `[instrument.class]`, and for a future
`[instrument.margin]`, plus optionally `[instrument.fees]` and
`[instrument.calendar]` - to model an instrument no preset covers (MCL, AAPL,
or any other bundle of the same knobs). See `docs/config.md` for the
complete field-by-field surface; this guide covers only the preset mechanism
that sits on top of it.
