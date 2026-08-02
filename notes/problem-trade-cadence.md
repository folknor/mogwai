# PROBLEM: the tape is two orders of magnitude too slow, and "trades per second" is undefined

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry; it outgrew a
bullet.

SEQUENCING CORRECTION: an earlier draft claimed this document resolves first.
It does not. `notes/problem-order-book.md` decides whether client orders rest in
a book and are consumed by arriving flow, and under that answer the generator
emits parent taker arrivals while the wire prints fall out of matching - which
redefines what a trade IS before this document can pick a rate for it. The book
document resolves first; this one is written assuming its answer.

## What the user wants

Looking at the generated tape the user said there are still way too few trades
in some places, and asked what an active crypto pair actually does per second
on a weekday - the answer given at the time being roughly 1-5 trades/sec on a
mid-tier venue and tens per second on Binance, against the 0.14/sec mogwai
produces. The Binance half of that was subsequently measured and is the table
below. The MID-TIER half is RECOLLECTION and stays unmeasured: no mid-tier
corpus exists in this project, Kraken being the only non-Binance data held and
its whole-second timestamps unable to express sub-second arrival structure at
all. Nothing in this document is sized off the 1-5 figure, so it is kept as the
framing that started the question rather than as evidence. They want the
default tape to look like a real active instrument rather than a thin one, they
consider the Kraken lineage incidental (it was an easy place to get a lot of
data, not a claim about which venue mogwai is), and they expect this to want
havoc knobs as well as an honest default, though which knobs and how was left
open.

## The observation

The default BTCUSDT tape runs at `mean_duration_s = 7.19`, which is 0.14
trades per second. Real Binance BTCUSDT in June 2026 ran at 49.6 raw trades
per second. That is a factor of 350.

This is not the drought defect, which was fixed on 2026-08-02 (`ca72e89`). That
one was about hour-scale SILENCE - a quiet stretch self-prolonging because the
ACD memory decayed per tick rather than in wall time. The dwell bound that
landed with it is holding: zero empty hours at both the test seed and the
production FNV seed, p999 gap 258 s against a 448 s bound. The tape no longer
goes dark. It is simply, uniformly, far too slow, and no committed gate says
otherwise because nothing sets a floor on density.

## Measured, from archives held locally and NOT committed

Binance spot, June 2026, from 1s klines (`Number of trades` column) and
aggTrades. Archives are in `research/market-data/`, which is gitignored;
`analysis/probe_binance_aggtrades.py` computes the trade-level statistics.

| | BTC | ETH | SOL |
|---|---|---|---|
| raw trades/sec, mean | 49.6 | 46.9 | 12.5 |
| raw trades/sec, median | 4 | 3 | 1 |
| raw trades/sec, p95 | 257 | 233 | 57 |
| seconds with zero trades | 13.4% | 26.2% | 38.9% |
| mean trade size, native | 0.00492 BTC | 0.0890 ETH | 2.746 SOL |
| mean trade size, NOTIONAL | $311 | $151 | $191 |
| notional/sec | $15,426 | $7,084 | $2,393 |
| taker-buy share of volume | 0.483 | 0.495 | 0.496 |

Cross-validated: TradingView's 15s export of the same pair over a different
window gives 0.176 BTC/sec against the klines' 0.244 BTC/sec for June - same
order, independent source. The aggTrades and kline readings agree to three
digits on the aggregation factor (3.77) and on taker-buy share.

## The data, and how to reproduce every number in this document

**Where it lives.** `research/market-data/`. `research` is gitignored, so none
of it is committed and a fresh clone has none of it. Roughly 1.5 GB:

- `BTCUSDT-1s-{2026-04,05,06}.zip`, plus `ETHUSDT-` and `SOLUSDT-` for the same
  three months - Binance spot 1-second klines, monthly archives.
- `BTCUSDT-1s-2026-07-*.zip` - July dailies, because Binance publishes the
  monthly archive with a lag and July was not up yet. These overlap the
  TradingView export window, which is how the cross-validation above was done.
- `{BTCUSDT,ETHUSDT,SOLUSDT}-aggTrades-2026-06.zip` - trade-level, June only.
  BTC 498 MB, ETH 466 MB, SOL 91 MB.
- `BINANCE_BTCUSDT, 15S_*.csv`, `CME_MINI_MNQ1!, 15S_*.csv` and
  `CME_MINI_MES1!, 15S_*.csv` - hand-exported from TradingView, 15-second OHLCV.
  The two CME files are the only non-crypto data held and the only source for a
  futures session envelope. Both span 2026-07-27 to 2026-07-31, roughly 22,000
  bars each: FIVE DAYS, one partial week. That span limit binds harder than the
  format limit for the questions the instrument model asks - a week contains no
  holiday, no early close, no DST transition and no contract roll, which are
  exactly the four things its session-fidelity and continuous-versus-dated
  decisions need evidence for.

**How to get more.** `research/binance-public-data/` is a vendored checkout of
Binance's own downloader. Its scripts need `pandas`, which is not installed
here, so the archives above were fetched straight off the published URLs
instead - the layout is stable and needs no dependency:

    https://data.binance.vision/data/spot/monthly/klines/<SYM>/1s/<SYM>-1s-<YYYY-MM>.zip
    https://data.binance.vision/data/spot/daily/klines/<SYM>/1s/<SYM>-1s-<YYYY-MM-DD>.zip
    https://data.binance.vision/data/spot/monthly/aggTrades/<SYM>/<SYM>-aggTrades-<YYYY-MM>.zip

That checkout also carries `download-trade.py`, which fetches RAW trades rather
than aggregated ones. Nothing here has used it yet, and it is the measurement
this document's clustering question actually needs - see below.

A consequence a spec author has to resolve rather than inherit: none of this is
reproducible from a fresh clone. The archives are gitignored, the Kraken path is
a machine-specific absolute path, nothing is checksummed, and the spec contract
requires gates to be exact copy-pasteable commands. Either the acquisition
becomes reproducible and pinned, or the derived artifacts get committed and the
raw archives stop being cited as evidence for a gate.

**The probes.** Both are committed and streaming. The aggTrades probe is O(1);
the kline probe retains one integer per second of the archive so it can report
quantiles, which is about 2.6M integers for a month:

    python3 analysis/probe_binance_klines.py research/market-data/BTCUSDT-1s-2026-06.zip
    python3 analysis/probe_binance_aggtrades.py research/market-data/BTCUSDT-aggTrades-2026-06.zip

The kline probe produces the rate and size table above. The aggTrades probe
produces the duration statistics, and prints them twice: once over raw
aggTrades and once collapsed to distinct timestamps, which is the correction
the next section explains. It also prints the sweep-width distribution, which
is a by-product of the collapse and is new structure mogwai does not model.

The Kraken corpus is at `/home/folk/Kraken` (`MOGWAI_DATA_DIR`), and two probes
read it directly, both importing their shared machinery from `characterize.py`
so they compute through the same code path the fingerprint does:

    python3 analysis/probe_kraken_durations.py            # raw vs collapsed durations
    python3 analysis/probe_timestamp_precision.py         # what resolution the file carries

Three older helpers are worth knowing about: `analysis/decode_dwell_bins.py`
reads the committed `analysis/char_*.json` histograms without needing the
corpus disk at all, `analysis/probe_warmup_window.py` counts trades per
simulated hour off a running server's `/trades`, and `analysis/run_corpus.py`
plus `analysis/build_fingerprint.py` are what regenerate the committed
fingerprint.

**A caveat that applies to every duration number below.** aggTrades timestamps
are MICROSECONDS - 16 digits, Binance moved off milliseconds - and the first
pass of this analysis divided by 1,000, producing a 30,000-day span for a
one-month file. Anything quoting an inter-trade duration from before that fix
is wrong by 1000x.

## The definitional problem: three LAYERS, not three options

"Trades per second" has three values on the same instrument in the same month,
differing by 8.5x on BTC:

| layer | BTC | what it is |
|---|---|---|
| raw trades | 49.6/sec | every fill, as counted by the kline `Number of trades` |
| aggTrades | 13.2/sec | fills merged by price and side within one taker order |
| match events | 5.8/sec | one taker order hitting the book, however many makers |

An earlier draft presented these as mutually exclusive meanings for one emitted
`TradeTick`, and that is wrong. They are layers of one process: a match event is
a PARENT arrival, it produces one or more child executions at one or more
prices, and a feed then publishes those children either raw or aggregated
according to its own contract. A model needs the parent arrival rate AND a child
multiplicity rule; picking "match event" as the tape's unit would discard the
sweep structure entirely, and picking "raw fill" without a parent model produces
independent arrivals that no real book generates.

Note also that the choice of what to PUBLISH is mogwai's feed contract, not a
fact about venues. An earlier draft asserted that no venue publishes raw fills
individually; Binance's spot `@trade` stream does exactly that, and nautilus
makes raw-versus-aggregated ticks configurable in its Binance integration.

The collapse from aggTrades to match events is close to recovering what the
exchange actually did, and an earlier draft of this paragraph called it exactly
that - "not a modelling choice". That overstates it, and `notes/problem-order-book.md`
states the caveat correctly where this document dropped it: Binance documents an
aggTrade as the aggregation of fills for one taker order, but does not guarantee
that a shared timestamp identifies one order across price levels, and there is
no taker-order id in the data. So the grouping is strong INFERENCE, not
identification. Binance stamps every fill of one match event with a single
`transact_time`, and 55.5% of consecutive BTC aggTrades share a timestamp. At 13.2 aggTrades/sec against microsecond
resolution, two independent arrivals collide with probability about 1.3e-5 per
adjacent pair - four orders of magnitude below the 55.5% observed - so the
collisions are the signature of a taker sweeping several makers rather than
chance.

A `TradeTick` on this venue is one print at one price with one size. Which layer
the emitted tape corresponds to is the feed-contract half of the question, and
the parent rate is the other half:

- **Match event** is what the exchange did, and mogwai has no book, so one
  synthetic trade being one taker order is arguably the honest analogue.
- **aggTrade** is what a real exchange WebSocket feed publishes, so it is what
  a consumer's tape would actually show. If mogwai's job is to look like a feed
  a client consumes, this is the target.
- **Raw trade** is what the kline count reports and is the number a naive
  "trades per second" comparison reaches for. It is also a real publication
  contract: Binance's spot `@trade` stream carries individual fills and nautilus
  makes raw-versus-aggregated configurable, so choosing it is a decision about
  what mogwai's feed claims to be, not an indefensible option.

Nothing downstream can be sized until this is picked - and note the sequencing:
under the book document's B3, wire prints fall out of matching, so this choice
is partly pre-empted rather than free.

## Three scope cautions on the numbers above

**Mean and median are not alternative targets.** A process can have a mean of
49.6, a median of 4 and 13.4% empty seconds simultaneously - that IS the shape.
The mean constrains total flow; the median, the quantiles and the zero-second
fraction constrain its distribution. An earlier draft treated picking one as the
decision, which would have specified a tape matching one moment of the
distribution and nothing else.

**The measurements are whole-month, weekends included.** The question that
started this was weekday activity. Crypto trades continuously so the distortion
is modest, but it is not zero, and for the CME instruments it would be
enormous - the session document's territory.

**Rates and sizes above are quoted at different layers.** The $311 notional per
trade is a RAW FILL figure; the 5.84/sec is a MATCH EVENT figure. A match event
averages about 8.5 raw fills on BTC, so its size is the sum of its children and
is not $311. Any model that pairs a parent rate with a child size is wrong by
that factor.

## Sizing is per-notional, not per-instrument

Mean trade size differs by three orders of magnitude in native units across
BTC, ETH and SOL, and by less than 2x in notional: $311, $151, $191. So the
current `typical_size` default (0.1, operator-set, per instrument, native
units) is the wrong shape of knob. A notional target divided by the
instrument's price gives a new instrument a defensible size from two numbers
already known, with no fit.

Consequence: mogwai's current tape is wrong in BOTH factors and they partly
cancel. Trades are far too large and ~350x too rare, so volume is low by much
less than either factor alone, and anyone checking volume alone would under-read
the defect.

CORRECTED, and the correction is one this document raises three paragraphs
below and then failed to apply here. An earlier draft said ~20x too large and
~17x low volume, computed against `typical_size = 0.1`. But the generator takes
`typical_size` as the lognormal MEDIAN and `SIZE_LOG_SIGMA = 1.15`, so the
generated MEAN size is `0.1 * exp(1.15^2 / 2)`, about 0.194 BTC - roughly 39x
Binance's measured mean of 0.00492 BTC, not 20x. Volume is correspondingly about
9x low rather than 17x: 0.14/sec times 0.194 is 0.027 BTC/sec against Binance's
0.244. The defect in cadence is unchanged; the size defect is twice as large as
stated and the volume gap half as large.

Three things the "notional over price" proposal leaves open, all of which a
spec would have to pin. WHICH price - the generated mid drifts, and the anchor
is `START_PRICE_USD 60_000` while the archive window's real price was not that.
MEAN or MEDIAN - the generator draws lognormal with `SIZE_LOG_SIGMA = 1.15` and
takes `typical_size` in as the MEDIAN, so handing it a mean notional target
overshoots by roughly `exp(sigma^2 / 2)`, about 1.9x. And DISCRETENESS - a
futures contract trades in whole units, so notional over price yields a
fractional contract and the knob shape fails for exactly the two instruments
that motivated profiles.

## Why the dispersion band breaks on any rescale

`analysis/characterize.py` computes `dispersion_index` as `var / mean`, which
carries units of seconds. It is not scale-free: halve the cadence and the index
halves, whatever the shape does. The committed band - re-derived era-windowed
on 2026-08-02 to `[36.3 .. 1627.9]` - is stated in those units.

So a cadence change will fail the realism gate mechanically, for reasons that
have nothing to do with the arrival shape. Measured, same data, both forms:

| | Kraken XBTUSD (era-windowed) | Binance BTC (match events) | Binance SOL |
|---|---|---|---|
| mean gap | 2.871 s | 0.171 s | 0.516 s |
| `var / mean` (committed units) | 36.28 | 0.79 | 1.85 |
| `var / mean^2` (dimensionless) | 12.64 | 4.62 | 3.57 |

CORRECTED. An earlier draft said the difference between Kraken and Binance in
the committed units was 41x and "ENTIRELY cadence". Its own table refutes both
halves: 36.28 / 0.79 is 45.9, not 41, and it decomposes into two factors rather
than one. About 16.8x of it is mean-gap scale (2.871 / 0.171), which is the
units problem this section is about. The remaining ~2.74x (12.64 / 4.62) is a
genuinely different dimensionless SHAPE - Kraken's arrivals cluster harder than
Binance's even after the units are removed, though the whole-second quantisation
finding below is a live alternative explanation for that residue. The point the
section exists to make survives intact: the band is scale-dependent and will
fail mechanically on any rescale. It is just not the whole story.

## The fitted corpus cannot see the cadence being targeted

Kraken was then checked the same way, with `analysis/probe_kraken_durations.py`,
which imports `AutoCorr` and the era constant from `characterize.py` so it
computes the ACF through the same code path the fingerprint does. It reproduces
the committed numbers exactly on raw prints - `var/mean` 36.28, ACF lag1 0.1603
- and then says something worse than an outlier:

| Kraken XBTUSD, era-windowed | raw prints | collapsed to distinct timestamps |
|---|---|---|
| mean gap | 2,871 ms | 7,348 ms |
| dimensionless dispersion | 12.64 | 600.80 |
| ACF lag1 | 0.1603 | 0.0012 |
| identical-timestamp fraction | 61.1% | |
| burst width | mean 2.57, max 2,578 | |

**Every timestamp in the corpus is a whole second.** Counted over the entire
file by `analysis/probe_timestamp_precision.py`: 81,810,187 rows, all with zero
significant decimal places, in both eras. So the
finest inter-trade gap the corpus can express is one second, 61% of consecutive
trades record a gap of exactly zero, and the duration ACF of 0.1603 - a
COMMITTED fitted target, the one the drought retune was tuned against on
2026-08-02 - collapses to 0.0012 when same-second trades are treated as one
arrival. That target is measuring how trades clump inside a one-second bucket,
not how arrivals cluster in time.

**And the blast radius is wider than the two anchors named above.** An earlier
draft scoped this to `duration_acf` lag1 and the dispersion band. But the
committed `duration_acf_anchor` is a TEN-LAG VECTOR, not one number, and
`return_acf_lag1` (-0.197), the three `abs_return_acf` lags and
`zero_change_frac` (0.474) are all computed PER PRINT over a corpus where 61.1%
of consecutive prints share a whole-second stamp. `return_acf_lag1` in
particular is read as a bid-ask-bounce signature, which is a claim about
intra-second print ORDERING - and the corpus cannot order within a second. If
same-second bucketing is an artefact it is an artefact for all of these, so the
inventory of gates that survive a cadence change is smaller than this document
previously implied. Establishing which of them are measuring the bucket rather
than the market is spec-level work and has not been done.

At today's cadence this is survivable: a 7.19 s mean gap sits well above the
quantum, so the fitted process operates in a range the data can describe. At
the cadence this document proposes targeting - anywhere from 1 to 50 trades per
second - every gap falls below the corpus's resolution, and the fitted
constants would be extrapolating into a region their source data cannot
constrain at all.

The consequence for this document's resolution is direct: **the target cadence
cannot be fitted from the Kraken corpus.** Binance aggTrades, at microsecond
resolution, is the only source here that can constrain sub-second arrival
structure, which makes the cadence work a refit against a new corpus rather
than a rescale of the existing one - a materially larger change than "move one
scalar", and one that touches the constants the realism gate asserts.

## What must be decided

1. **Which definition of a trade the tape emits.** Match event, aggTrade, or
   raw fill. Everything else is sized off this.
2. **The target rate, and in what form.** Not a single scalar: mean, median,
   quantiles and the zero-second fraction each constrain a different part of the
   distribution and a real process satisfies all of them at once. The 12-15x
   mean-over-median spread is an order of magnitude rather than a fitted target,
   since the medians are integer per-second counts with a one-trade
   quantisation floor.
3. **Whether the dispersion band is restated dimensionlessly** or rescaled with
   the cadence. Restating is the honest fix; rescaling preserves the committed
   numbers at the cost of keeping a scale-dependent gate. Either way the band
   needs a SOURCE, and no obvious one exists: the committed band is a cross-pair
   spread over eight Kraken pairs, and this document establishes that the Kraken
   corpus cannot describe sub-second structure at all, so neither its 12.64 raw
   nor its 600.80 collapsed figure can found a new band. The Binance
   measurements span 3.57 to 10.01 over three instruments of one venue in one
   month, which is a candidate and a thin one. The same question applies to the
   duration ACF anchor of 0.1603, which this document demolishes as an artefact
   of one-second bucketing without proposing what replaces it - and that anchor
   is a committed target the drought retune was tuned against, so it is a
   currently-green gate being invalidated with no successor named.
4. **Whether size belongs in this document at all.** The notional-derived
   proposal above fails for a contract instrument, and
   `notes/problem-instrument-model.md` owns whether contracts exist. Either
   sizing resolves here for spot only and reopens under the instrument model, or
   it moves there wholesale. It cannot resolve here for both.
5. **Whether a `TradeTick` needs an exchange trade id or sequence number.** The
   adapter derives `TradeId` from the tick's own fields, so several children of
   one parent event sharing a timestamp and price can collide. Denser tape, more
   collisions.

## What this document does not decide

Per-instrument profiles, which are the subject of the sibling document - this
one settles what a trade is and how fast they arrive for ONE instrument.
Clustering constants and whether they are shared or per-instrument, likewise.
The sweep structure the collapse exposed (76.5% of BTC match events are one
print, tail to 2,213) is real and unmodelled, but it needs levels to land in,
so it belongs to the book document. No change to the drought dwell bound, which
is orthogonal and holding.

## One correctness consequence, and one non-input

**Correctness.** A dense tape strains the seek budgets, and the shape of the
strain differs by path - an earlier draft's flat "190,000 ticks equals N hours"
arithmetic describes none of them accurately, because clean tapes use a
checkpoint index and a WARM seek is flat in checkpoint spacing rather than
linear in tape length. The three paths need measuring separately:

- **Cold clean seek.** The checkpoint extension is capped, so the first
  positioning into a long horizon is where exhaustion bites.
- **Warm clean seek.** Flat via the checkpoint index; the caller's residual seek
  past the nearest checkpoint is what is bounded.
- **Regime seek.** Bypasses the checkpoint index entirely, so it degrades
  fastest and is the path an armed scenario takes.

The consequences are already visible in the code's own vocabulary: a websocket
subscribe can report `SeekBudgetExhausted`, and an HTTP history request can turn
an exhausted seek into an EMPTY SUCCESSFUL response - a venue silently serving
less history than it advertises, which is a lie about the data rather than a
slowdown.

RESOLVED by the lifecycle document, and recorded here because this document
raised it. The seek budget is a LATENCY bound on the request path, not a memory
bound: history is never retained, and a request for past data re-synthesizes it
from the nearest checkpoint, so 190,000 ticks is sized in `source.rs` against
measured synthesis throughput of ~1.9M ticks/sec to fit a ~100 ms request
budget. Under a DECLARED warmup duration the venue knows at boot how much tape
it owes and generates it eagerly, so no client request needs a long seek and the
cap has nothing left to protect. The user's ruling is that warmup length is
their decision, not the venue's. What survives into the spec is the SHAPE of the
startup cost rather than a single number, because the number depends on decision
1 above and swings 8.5x across its candidate answers. A two-year warmup at the
match-event rate is roughly 368 million ticks, about 3 minutes at the measured
~1.9M ticks/sec; at the aggTrade rate roughly 830 million and ~7 minutes; at the
raw-fill rate roughly 3.2 billion and ~28 minutes. An earlier draft quoted only
the last of those as if the layer were settled. Separately, the checkpoint index
retains one generator clone per `CHECKPOINT_K` ticks, so checkpoint spacing
rather than the seek cap is the knob that trades memory against seek latency.
Neither is a reason to cap the request; both belong in the spec so an extreme
warmup fails loudly rather than mysteriously.

**Acceleration is a premise, and it is a correctness bound rather than a cost.**
Forward tests always run accelerated. A denser tape multiplies the work per unit
of WALL time at any given speed, and the adapter's `MIN_WALL_REQUEST_TIMEOUT_SECS`
of 1 is flagged in its own comment as the tightest cap on usable sim speed. A
request that times out is a failed run, not a slow one, so this sits outside the
resource-cost exclusion below and the spec has to price it.

An earlier draft also claimed a truncated `SWEEP_DRAIN_BUDGET` pass returns
`complete: false` and stalls the scan frontier. There is no such field. A
truncated walk reports `reached_ns`, the frontier advances to that point, and
the next pass resumes - the consequence is catch-up latency, which is cost.

**Not a decision input.** Per the user's standing instruction, resource cost
does not shape any decision here. A denser tape is proportionally more work per
unit of sim time, the golden fill distribution and every realism anchor
re-bless, and `reference/performance.md`'s figures were measured on the sparse
tape. None of that argues for a lower cadence. The multiplier itself is
undetermined until the layer is picked - from 0.14/sec it is 42x at the
match-event rate, 94x at the aggTrade rate and 354x at the raw-fill rate, and an
earlier draft's unexplained "30x" corresponded to none of them.
