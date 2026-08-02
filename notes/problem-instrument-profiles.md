# PROBLEM: one fitted instrument, five traded ones, and no way to say which

Expanded from what would otherwise be a `notes/todo.md` entry. Depends on
`notes/problem-trade-cadence.md`, which settles what a trade is and how fast
they arrive; this document settles how that varies per instrument and who gets
to change it.

## What the user wants

The user trades MNQ, MES, BTC, SOL and ETH, plus others occasionally, and wants
mogwai to be able to shape its data toward any of them. Specifically they want
a config knob, set per strategy, that names a profile which is pre-registered
inside mogwai - so a scenario can ask for the venue to behave like one of these
instruments by name - and, in addition, the ability for the user to tune each
individual knob that a named profile would otherwise set. So: named presets
that carry fitted values, with per-knob override on top, rather than either a
fixed set of instruments or a bag of raw numbers.

## The observation

The venue ships one instrument, BTCUSDT, whose numbers come from a cross-pair
median of eight Kraken pairs. The instruments actually traded against it are
MNQ, MES, BTC, SOL and ETH - two CME index futures and three crypto majors,
with different tick sizes, session structures, cadences and liquidity. Today a
scenario cannot say "behave like SOL" or "behave like MNQ", and an operator who
wants a thinner or busier tape has one lever, `LiquidityDrought`, which only
thins and is a havoc arm rather than a profile.

The seam for this already exists and is half-built: `InstrumentProfile`
(`mogwai-server/src/source.rs`) bundles an `InstrumentDef`, a
`GeneratorScalars` and a `SessionProfile` per symbol, and `InstrumentProfiles`
holds them by symbol. What is missing is that the values are not per-instrument
in any meaningful sense - every profile is built from the same fingerprint
medians - and there is no way to name one from config.

## Measured differences that a profile must be able to express

Binance spot, June 2026 (see `notes/problem-trade-cadence.md` for the full
table and provenance):

| | BTC | ETH | SOL |
|---|---|---|---|
| raw trades/sec | 49.6 | 46.9 | 12.5 |
| seconds with zero trades | 13.4% | 26.2% | 38.9% |
| notional per trade | $311 | $151 | $191 |
| dimensionless dispersion, match events | 4.62 | pending | 3.57 |

Two readings of that table matter for the design. The SHAPE is close: mean over
median is 12-15x on all three, taker-buy share is 0.483-0.496 on all three, and
the dimensionless dispersion of BTC and SOL sit within 30%. The SCALE is not:
rate varies 4x and quiet-second fraction 3x across three instruments that are
all crypto majors on one venue.

If that pattern holds when CME data arrives, a profile is a small set of scale
knobs over one shared fitted shape. If MNQ and MES break it, clustering has to
move into the profile too - and clustering is currently four GLOBAL module
constants (`ACD_PERSISTENCE`, `ACD_FEEDBACK_SHARE`, `ACD_WEIBULL_SHAPE`,
`ACD_RELAX_MEAN_CAL`) that the realism gate is anchored on with a single
profile. Moving them per-instrument re-scopes that gate.

## The provenance problem

The five instruments will not be fitted from equivalent evidence:

- **BTC, ETH, SOL** have Binance trade-level archives: real inter-trade
  durations, sizes, aggressor, sweep structure.
- **MNQ, MES** have 15-second TradingView bars: OHLCV only. No trade counts, no
  durations, no aggressor. A CME cadence cannot be fitted the way a crypto one
  can - it would be derived from volume-and-size arithmetic, and its clustering
  would come from nowhere at all.
- **Kraken** remains the only corpus with multi-year span, and it is the one
  the committed fingerprint and every realism anchor is built from.

So a profile field can be fitted, derived, or declared, and those deserve
different trust. A declared 0.076 s cadence and a fitted one look identical in
a struct. This project already refused a model for exactly this class of reason
- queue-ahead was declined on 2026-08-02 because visit volume was not the
quantity it claimed to be - so shipping an unmarked declared value inside
something called a fitted profile would be the same failure with better
manners.

## The knob layering the user asked for

A scenario names a profile; the profile presets a set of knobs; the operator
overrides individual knobs on top. Two things that needs, neither of which
exists:

**A precedence rule, stated and loud.** The failure mode is an operator setting
a cadence under a profile that also sets one and silently getting whichever
wins. This venue's existing posture is to refuse ambiguity at boot rather than
resolve it quietly - `queue_ahead_enabled` without `penetration_ticks` is
refused, acceleration without a pinned `sim_epoch_ns` is refused - so the
consistent choice is either logging every override with both values or refusing
the combination outright.

**A decision about what a profile may NOT override.** A profile that can set
the ACD constants can move the tape outside the band the realism gate asserts,
at which point the gate is testing one profile and the served tape is another.
That is the same "the validated tape is not the served tape" collision the
drought decision faced and resolved in favour of fixing the mechanism.

## What must be decided

1. **What a profile owns.** Cadence, size, tick and session are natural. Are
   the clustering constants in or out? That answer depends on whether CME data
   shows a different shape, which is measurable once MES and MNQ arrive.
2. **How provenance is recorded**, so a fitted field and a declared one are
   distinguishable at the point of use, not just in a commit message.
3. **The precedence and refusal rules** for profile-versus-override.
4. **What the realism gate asserts** when there are five profiles and it
   currently runs one. Every profile, the default only, or a named subset.
5. **Whether the fingerprint stays single-corpus.** It is Kraken-derived and
   carries cross-pair bands from eight Kraken pairs. Adding Binance means
   either a second reader, a normalisation step, or a decision that the
   fingerprint describes shape only and per-instrument scale lives in profiles.

## What this document does not decide

The cadence target itself, which is the sibling document. Anything requiring
levels - a CME profile's book shape, per-instrument depth - belongs to the book
document. The `analysis/` test-harness question, already an open todo entry,
is adjacent and may be pulled in here since multi-corpus fitting makes the
estimator/consumer drift hazard worse.

## Known cost, not yet priced

Multi-corpus fitting means `analysis/characterize.py` grows a second input
format, or a normaliser lands in front of it. The eight-pair cross-venue bands
in the committed fingerprint were computed over one venue's CSV layout, and
`build_fingerprint.py` assumes it. Neither is hard; both are load-bearing for
every number the gate reads, and the pipeline has exactly one Python test file
today (`analysis/test_characterize.py`, landed 2026-08-02).
