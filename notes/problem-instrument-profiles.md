# PROBLEM: one fitted instrument, five traded ones, and no way to say which

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

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

The seam for this exists and is more complete than an earlier draft of this
document claimed. `InstrumentProfile` (`mogwai-server/src/source.rs`) bundles an
`InstrumentDef`, a `GeneratorScalars` and a `SessionProfile` per symbol, and
`reference/config.md` documents that each `[[instrument]]` table in the server
TOML carries all three - the wire definition fields, a full `generator` table,
and a `session` table of 24 hourly arrival shares, 24 hourly volatility
multipliers and 7 day-of-week weights, all value-validated at load. It says
outright that per-symbol exchange hours, maintenance breaks and weekend shape
belong there.

So per-symbol configuration exists and is validated. It is NOT complete, and an
earlier draft of this document over-corrected by calling it "fully configurable
in TOML" - `GeneratorScalars` exposes eight values, while the ACD shape
constants, `SIZE_LOG_SIGMA`, the GARCH parameters and the bounce dynamics remain
GLOBAL module constants that no config can reach. So the configurable surface is
the instrument's scale and session envelope; the arrival and volatility PROCESS
is one global shape for every symbol.

What is missing is therefore two things at different levels. At the mechanism
level: named built-in PRESETS, an OVERLAY so a consumer names one and overrides
two fields rather than supplying forty, PROVENANCE on the resulting effective
values, and a SELECTION mechanism. At the model level: whether the process
constants become per-instrument at all, which is the clustering question below.
The built-in default is built from cross-pair fingerprint medians, which is a
third and separate complaint about the default's provenance.

## Measured differences that a profile must be able to express

Binance spot, June 2026 (see `notes/problem-trade-cadence.md` for the full
table and provenance):

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
taker-buy share is 0.483-0.496 on all three; and the two busy books sweep
almost identically, 76.5% against 77.4% single-print events, while the thin one
barely sweeps at all. Those look like one shape.

Clustering does not. The dimensionless dispersion spans 3.57 to 10.01 across
three crypto majors on one venue, a 2.8x spread that survives the
timestamp-collapse correction. An earlier reading of BTC and SOL alone put them
within 30% and suggested one shared fitted shape; ETH breaks that, and the
design should not assume the cheap answer.

That conclusion rests on one month of one venue, and the April and May archives
for all three instruments are already on disk unexamined. Establishing whether
the 2.8x spread is stable across months, or is a June artefact, is cheap and
would either firm up the per-instrument-clustering conclusion or dissolve it. It
should happen before a spec is written against either answer.

So the open question is sharper than "does the pattern hold when CME arrives".
It is already not holding within crypto. If clustering is genuinely
per-instrument, it has to move into the profile - and it is currently four
GLOBAL module constants (`ACD_PERSISTENCE`, `ACD_FEEDBACK_SHARE`,
`ACD_WEIBULL_SHAPE`, `ACD_RELAX_MEAN_CAL`) that the realism gate is anchored on
with a single profile. Moving them per-instrument re-scopes that gate.

## The provenance problem

The five instruments will not be fitted from equivalent evidence:

- **BTC, ETH, SOL** have Binance trade-level archives: real inter-trade
  durations, sizes, aggressor, sweep structure.
- **MNQ, MES** have 15-second TradingView bars and nothing else: OHLCV only. No
  trade counts, no durations, no aggressor. A CME cadence cannot be fitted the
  way a crypto one can - it would be derived from volume-and-size arithmetic,
  and its clustering would come from nowhere at all. Note also that before a CME
  profile can be fitted at all, the venue has to be able to REPRESENT a futures
  contract, which it currently cannot - see
  `notes/problem-instrument-model.md`, which precedes this document.
- **Kraken** remains the only corpus with multi-year span, and it is the one
  the committed fingerprint and every realism anchor is built from - but its
  timestamps are whole seconds, all 81.8 million of them, so it cannot describe
  arrival structure below one second at all. See the resolution section of
  `notes/problem-trade-cadence.md`. A profile whose cadence is sub-second
  cannot inherit its clustering from this corpus, whatever else it inherits.

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
