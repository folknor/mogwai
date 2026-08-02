# PROBLEM: the arrival and volatility process is one global shape, and measured clustering is not

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. Depends on
`notes/problem-trade-cadence.md`, which settles what a trade is and how fast
they arrive for ONE instrument; this document asks whether the process that
generates them is the same shape for every instrument.

This document was previously twice its current size and asked two questions at
once. The MECHANISM question - named presets, an overlay for per-knob override,
how provenance is recorded, and the precedence rule between them - is no longer
its own problem: the user has ruled that mogwai's instrument model is a complete
PARAMETERIZATION rather than an enum of supported instruments, and that a preset
is nothing more than a named bundle of otherwise-tunable config knobs. Profile,
preset and override collapse into one parameter set, and defining it is
`notes/problem-instrument-model.md`'s job. That half moved there entire.

What is left is empirical and no ruling touches it.

## The observation

The venue ships one instrument, BTCUSDT, whose numbers come from a cross-pair
median of eight Kraken pairs. Per-symbol configuration exists and is more
complete than an earlier draft claimed: `InstrumentProfile`
(`mogwai-server/src/source.rs`) bundles an `InstrumentDef`, a
`GeneratorScalars` and a `SessionProfile` per symbol, and `reference/config.md`
documents that each `[[instrument]]` table in the server TOML carries all three,
value-validated at load.

But `GeneratorScalars` exposes SEVEN numeric knobs, and they are all SCALE:
modal tick, price decimals, mean duration, size rounding, start price, typical
size, vol scalar. (The struct has eight fields; the eighth is `symbol`, which
names the instrument rather than scaling it. Earlier drafts said "eight
values".) The ACD shape constants, `SIZE_LOG_SIGMA`, the GARCH parameters and the
bounce dynamics are GLOBAL module constants that no config can reach. So the
configurable surface is an instrument's scale and session envelope, while the
arrival and volatility PROCESS is one global shape for every symbol.

The question this document exists to answer is whether that is a defect or a
finding. If the process really is one shape across instruments, global constants
are correct and cheap. The measurement below says it probably is not.

## Measured differences the process would have to express

Binance spot, June 2026 (see `notes/problem-trade-cadence.md` for the full table
and provenance):

| | BTC | ETH | SOL |
|---|---|---|---|
| raw trades/sec | 49.6 | 46.9 | 12.5 |
| seconds with zero trades | 13.4% | 26.2% | 38.9% |
| notional per trade | $311 | $151 | $191 |
| match events/sec | 5.84 | 6.78 | 1.94 |
| single-print share of events | 76.5% | 77.4% | 98.6% |
| dimensionless dispersion, match events | 4.62 | 10.01 | 3.57 |

Some of that is shared and some is not, and the split does not fall where a
first look suggested. Mean-over-median burstiness is 12-15x on all three - an
order of magnitude, not a fitted figure, since the medians are integer
per-second counts (4, 3, 1) with a quantisation floor of one whole trade;
taker-buy share is 0.483-0.496 on all three; and the two busy books sweep almost
identically, 76.5% against 77.4% single-print events, while the thin one barely
sweeps at all. Those look like one shape.

But note what "one shape" does and does not license, because an earlier draft of
this paragraph claimed scale knobs could carry them. They cannot. SWEEP
MULTIPLICITY and single-print share are properties of the parent-child execution
process - one taker order producing N fills at M prices - and NO current scalar
represents that process at all, because the generator emits independent
arrivals. Whether those figures are shared across instruments is a finding about
the world; expressing them requires structure the generator does not have, which
belongs to `notes/problem-order-book.md` and `notes/problem-trade-cadence.md`
rather than to a scale knob here.

Clustering does not. The dimensionless dispersion spans 3.57 to 10.01 across
three crypto majors on one venue, a 2.8x spread that survives the
timestamp-collapse correction. An earlier reading of BTC and SOL alone put them
within 30% and suggested one shared fitted shape; ETH breaks that, and the
design should not assume the cheap answer.

That conclusion rests on ONE MONTH of ONE VENUE, and confirming it is NOT the
cheap probe run an earlier draft of this document claimed. The dispersion
figures come from `analysis/probe_binance_aggtrades.py`, and aggTrades exist on
disk for JUNE ONLY - the April and May archives are 1-second klines, whose
resolution is precisely what `notes/problem-trade-cadence.md` demolishes the
Kraken corpus for, so they cannot describe sub-second arrival structure and
cannot speak to clustering at all. Establishing whether the 2.8x spread is
stable across months therefore means FETCHING roughly 3 GB of additional
aggTrades from the published URLs, not running an existing probe over data
already held. Still worth doing before a spec is written against either answer,
but priced honestly it is an acquisition rather than an afternoon.

So the open question is sharper than "does the pattern hold when CME arrives".
It is already not holding within crypto. If clustering is genuinely
per-instrument it has to become configurable - and it is currently FIVE global
module constants in `generated/consts.rs` (`ACD_PERSISTENCE`,
`ACD_FEEDBACK_SHARE`, `ACD_WEIBULL_SHAPE`, `ACD_WALL_RELAX_TAU_S`,
`ACD_RELAX_MEAN_CAL`) that the realism gate is anchored on with a single
profile. Earlier drafts named four and omitted `ACD_WALL_RELAX_TAU_S`, which is
the wall-time relaxation the drought fix turned on - so it is arguably the one
most likely to differ per instrument, since it governs how a quiet stretch
decays. Moving them per-instrument re-scopes that gate.

Two cautions on the inference itself, both of which the conclusion should carry.
A dispersion spread does not identify WHICH constants differ, or even establish
that ACD constants are the right place to absorb the difference: session
nonstationarity, the inferred-event grouping, and month effects are all
alternative explanations for the same summary statistic. And this document is
titled for arrival AND volatility while supplying no cross-instrument volatility
measurement at all - the GARCH side of the question is untested.

## The provenance problem

Retained here rather than moved, because it is about what the CORPORA can
support rather than about how a config field records itself. The five
instruments will not be fitted from equivalent evidence:

- **BTC, ETH, SOL** have Binance trade-level archives: real inter-trade
  durations, sizes, aggressor, sweep structure.
- **MNQ, MES** have 15-second TradingView bars and nothing else: OHLCV only. No
  trade counts, no durations, no aggressor. A CME cadence cannot be fitted the
  way a crypto one can - it would be derived from volume-and-size arithmetic,
  and its clustering would come from nowhere at all. Note also that before a CME
  profile can be fitted at all, the venue has to be able to REPRESENT a futures
  contract, which it currently cannot - see `notes/problem-instrument-model.md`,
  which precedes this document.
- **Kraken** remains the only corpus with multi-year span, and it is the one the
  committed fingerprint and every realism anchor is built from - but its
  timestamps are whole seconds, all 81.8 million of them, so it cannot describe
  arrival structure below one second at all. See the resolution section of
  `notes/problem-trade-cadence.md`. A profile whose cadence is sub-second cannot
  inherit its clustering from this corpus, whatever else it inherits.

So a process constant can be fitted, derived, or declared, and those deserve
different trust. A declared 0.076 s cadence and a fitted one look identical in a
struct. This project already refused a model for exactly this class of reason -
queue-ahead was declined on 2026-08-02 because visit volume was not the quantity
it claimed to be - so shipping an unmarked declared value inside something
called a fitted profile would be the same failure with better manners. HOW that
distinction gets recorded is the instrument model's mechanism question; WHETHER
the evidence exists to fit each instrument is this one's.

## What must be decided

1. **Do the process constants become per-instrument at all.** The 2.8x
   clustering spread says probably yes; one month of one venue says confirm it
   first - which means FETCHING further months of aggTrades, since the April and
   May archives held are klines and cannot describe sub-second structure.
2. **What the realism gate asserts** when several instruments exist and it
   currently runs one. Every instrument, the default only, or a named subset.
   This is forced by decision 1: the gate is anchored on the five global ACD
   constants, so making them configurable moves the tape out from under it.
3. **What a configured instrument may NOT change.** A config that can set the
   ACD constants can move the tape outside the band the realism gate asserts, at
   which point the gate is testing one instrument and the served tape is
   another. That is the same "the validated tape is not the served tape"
   collision the drought decision faced and resolved in favour of fixing the
   mechanism.
4. **Whether the fingerprint stays single-corpus.** It is Kraken-derived and
   carries cross-pair bands from eight Kraken pairs. Adding Binance means either
   a second reader, a normalisation step, or a decision that the fingerprint
   describes SHAPE only while per-instrument scale lives in config.

## What this document does not decide

The cadence target itself, which is the sibling document. The config surface,
presets, overlay, precedence and provenance recording, all of which are
`notes/problem-instrument-model.md`. Anything requiring levels - per-instrument
depth, a CME book shape - belongs to the book document. The `analysis/`
test-harness question, already an open todo entry, is adjacent and may be pulled
in here since multi-corpus fitting makes the estimator/consumer drift hazard
worse.

## Known cost, not yet priced

Multi-corpus fitting means `analysis/characterize.py` grows a second input
format, or a normaliser lands in front of it. The eight-pair cross-venue bands
in the committed fingerprint were computed over one venue's CSV layout, and
`build_fingerprint.py` assumes it. Neither is hard; both are load-bearing for
every number the gate reads, and the pipeline has exactly one Python test file
today (`analysis/test_characterize.py`, landed 2026-08-02).
