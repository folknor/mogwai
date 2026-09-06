# Synthetic tick tape for mogwai: research programme

Written 2026-09-02. Decision document for the owner, written to be executed
from by agents. States the settled premises, the fidelity target, the
recommended architecture, the fitting pipeline, the validation battery, and
the experiments that decide the remaining forks. Where this document guesses,
it says so.

Related: `mogwai/reference/north-star.md`, `mogwai/reference/glossary.md`.
Corpus: `/speilelg/databento` (see `~/Claude/dbpull/HANDOFF.md`).

---

## 1. Settled premises

These were decided by the owner and are not open here.

- **Output is tick-level.** Trades carry price, size, aggressor side and a
  nanosecond timestamp. Quotes carry top-of-book bid and ask price and size.
  Nothing deeper than the touch.
- **The water is exogenous.** Passenger orders never feed back into the tape.
  There is no impact, no queue competition, no liquidity consumption. The
  tape's trades are information a strategy reads; the venue's fill logic reads
  the quotes and prints. This removes the hardest part of order-book modelling.
- **The tape is a fitted generative process, not stitched real segments.**
  Real data is measured; a model is fitted; the model invents ticks that never
  happened but are statistically indistinguishable at the levels that matter.
  One seed is one path. Many seeds are the fitted distribution of worlds.
- **Four instrument classes, CME futures first.** Futures is fully worked
  here. Crypto, cash equities and forex are parameter deltas on the same
  machinery, with their own corpora still to be sourced. The standard tape
  (default shape for an unknown symbol) will be the crypto tape.
- **The futures tape carries the full standard week.** Sunday open, daily
  sessions, the daily maintenance close, the weekend close, and the ordinary
  price jump at every scheduled reopen. The `ReopenGap` havoc arm is a
  separate, unscheduled jump on top of that, never a substitute for it.
- **Session footprints are composable.** A tape is a sequence of session
  segments with phase labels. A user composes an endless Asia, a looping week
  without weekends, a New York afternoon only. The generator plays the list.
- **Out of scope.** Holidays, half days, cross-product dependence, market
  impact, book depth beyond the touch, contract rolls and expiry, opening and
  closing auctions, price limits (may become a declared knob later).
- **Determinism.** The tape is a pure function of the resolved bundle and the
  seed. Delivery speed is never part of identity.
- **Realism is instrumental.** A forward run proves execution robustness,
  never edge. The tape must be right where it decides whether and when a
  resting or conditional order fills, and right enough elsewhere that "many
  seeds" is an honest distribution of months.
- **Rust generation, small presets.** The generator runs inside
  `mogwai-venue`. A preset must be a TOML of knobs with provenance, on the
  order of kilobytes. Fitting may live in Python against the corpus.

---

## 2. What "realistic" means here: the fidelity hierarchy

Because realism serves execution-path fidelity, the properties are ranked.
Everything downstream (model choice, validation, experiment order) follows
this ranking.

**Tier 1, fill-deciding.** What determines whether and when a limit, stop or
conditional order fills.

- The path of the best bid and ask between trades: how often and by how much
  the touch moves, and how long it rests at a level.
- Trade-through versus touch: the probability that a level is traded at
  versus merely quoted at, as a function of distance from mid.
- Spread distribution in ticks and its dynamics (open, narrow, flicker).
- Sweep bursts: one aggressor producing several prints at the same or nearly
  the same timestamp, possibly walking through levels.
- Intra-second excursions: the largest move inside one second, and multi-tick
  jumps.
- The gap at every scheduled reopen, and the burst of activity after it.
- Print timing at sub-millisecond resolution: inter-event time distribution.

**Tier 2, indicator-deciding.** What a strategy's indicators compute from.

- Volume per bar and its intraday profile.
- Aggressor sign and its memory (delta, cumulative volume delta, imbalance).
- Trade size distribution including round-lot clustering.
- Touch sizes (bid and ask size), low fidelity acceptable.
- Tick volume for classes where that is what a strategy sees (forex).

**Tier 3, distribution-of-worlds.** What makes many seeds an honest sample
of months.

- Volatility clustering across minutes, hours, days and weeks.
- Heavy tails of returns at bar horizons.
- Intraday and weekly seasonality of activity and volatility.
- Long memory of order-flow signs and of volatility.
- The signature plot (realised variance against sampling interval) and the
  variance ratio across horizons.

A candidate model that fails Tier 1 is rejected regardless of Tier 3.
A model that passes Tier 1 and 2 with a weak Tier 3 ships, with the weakness
named in the preset's provenance.

---

## 3. The corpus and what each part fits

Dataset `GLBX.MDP3`, parent symbology, 63 products across equity index,
crypto, rates, FX futures, energy, metals, agriculture and livestock. Spans as
pulled (approximate, from `dbpull/spec.conf`):

| schema | span | fits |
|---|---|---|
| `mbo` | 30 days | sweep structure, queue dynamics, cross-check of MBP-1 |
| `mbp-10` | 30 days | not needed for a top-of-book tape; useful for queue-reactive comparison |
| `mbp-1` | about 1 year | **the quote layer**: every top-of-book change, with trades |
| `tbbo` | about 7 months | trades with the book at the trade: sizes, sides, sweeps |
| `bbo-1s`, `bbo-1m` | 9 to 12 months | spread profiles by phase, cheap |
| `ohlcv-1s` | since 2010 | intraday activity and volatility profiles by phase |
| `ohlcv-1m`, `-1h`, `-1d` | since 2011 | the slow volatility driver, reopen gaps |
| `definition` | since 2012 | tick size, multiplier, contract selection |
| `statistics` | since 2010 | settlement, volume by contract for front-month selection |
| `status` | since 2011 | actual session state, to validate the phase partition |

Conventions for fitting:

- Use the exchange timestamp (`ts_event`), never the capture timestamp.
- Select one contract per product per day: the outright with the highest
  volume on that day from `statistics` or `definition`. Discard the day on
  either side of a roll for microstructure fitting. Rolls are out of scope for
  the tape, so the fitted object is "the front month as a continuous
  instrument".
- Partition every day into phases before fitting anything (section 4.1).
  Hawkes fits on non-stationary data produce spurious long memory; the
  partition is not optional.

Suggested fitting cases, chosen to span the tick regime and volatility range:

| product | why |
|---|---|
| ES, MES | large tick, densest tape, the reference case |
| NQ, MNQ | large tick, higher volatility, the owner's named examples |
| CL | medium tick, energy session shape |
| ZN | very large tick, low volatility, spread pinned at one tick |
| GC | medium tick, metals |
| 6E | FX futures, the proxy for a forex preset until a spot FX corpus exists |
| BTC (CME) | bridge to the crypto class, same engine on a 24/7 product |

---

## 4. Architecture: four layers

The generator is a composition of four independently fitted layers. Each is
stationary or deterministic by construction, which is what makes indefinite
generation safe. Each exposes the quantities the havoc arms need as
first-class knobs.

```
L0  Clock / footprint     deterministic; phase segments and transitions
L1  Slow driver           stationary latent volatility state
L2  Microstructure engine marked multivariate Hawkes in business time
L3  Time change + marks   business time -> simulated ns; sizes, sweeps
```

### 4.1 L0: clock and footprint

A **phase** is a labelled stretch of the trading week inside which activity
is treated as stationary up to the slow driver. Proposed futures taxonomy,
exchange-local (Chicago) time, to be validated against `status` and the
activity profile:

| phase | window (CT) | notes |
|---|---|---|
| `sunday_open` | Sun 17:00 to 18:00 | opening burst after weekend close |
| `asia` | 18:00 to 02:00 | quiet, spread pinned |
| `london` | 02:00 to 07:00 | European cash open at 02:00 |
| `ny_pre` | 07:00 to 08:30 | US data releases at 07:30 |
| `ny_open` | 08:30 to 10:30 | US cash open 08:30, peak activity |
| `ny_mid` | 10:30 to 13:00 | lunch trough |
| `ny_close` | 13:00 to 16:00 | cash close 15:00, futures settlement window |
| `maintenance` | 16:00 to 17:00 | closed |
| `daily_open` | 17:00 to 18:00 | reopen burst, smaller than Sunday |
| `weekend` | Fri 16:00 to Sun 17:00 | closed |

Phase boundaries are declared knobs. Everything inside a phase is fitted.
The phase list is the unit of composition: a footprint is an ordered list of
`(phase, duration)` and the generator plays it. An endless Asia is
`[(asia, inf)]`. A looping week without weekends is the week's list with
`weekend` removed and `friday_close -> sunday_open` replaced by
`maintenance -> daily_open`.

Per-phase level knobs (all fitted, section 6):

- baseline event intensity per event type (relative to a reference phase),
- volatility level multiplier,
- spread distribution in ticks,
- opening ramp: intensity and volatility decay after a reopen,
  `phi(t) = phi_inf + a * exp(-t / tau_open)`.

Transition knobs, keyed by the pair of phases across a closure:

- reopen gap: the distribution of (first mid after open minus last mid before
  close), in ticks, scaled by the slow driver's state at close. Expect a
  Student-t shape. Fitted from `ohlcv-1m`.
- whether the slow driver advances during the closure (declared; default: it
  advances by a fraction of the closed calendar time, fitted from the
  variance of the reopen gap against closure length).

### 4.2 L1: slow volatility driver

A stationary latent process `V(t)` that scales both event intensity and the
size of price moves. Timescales of minutes to months. This layer is what
makes seeds differ in "what kind of month it was".

Candidates:

| model | params | stationary | Markov | fits long memory | simulation cost |
|---|---|---|---|---|---|
| log-OU (one factor) | 3 | yes | yes | no | trivial |
| Markov-switching multifractal (Calvet-Fisher) | 4 | yes | yes | yes, multi-scale | trivial |
| rough volatility (fBm, H about 0.1) | 3 to 4 | with mean reversion | no | best | needs Markovian lift for indefinite runs |

**Recommendation: MSM as the default L1.** Four parameters (`m0`, `sigma_bar`,
`b`, `gamma_kbar`) with `kbar` about 8 components spanning timescales from
minutes to a year. It is a finite-state Markov chain, so it is stationary,
checkpointable in a handful of bytes, and costs nothing to advance. It
reproduces multi-scale volatility clustering and heavy tails in bar returns.
Rough volatility is the upgrade path if E3 (section 8) shows MSM misses the
realised-variance scaling at short horizons; a Markovian approximation with a
few exponentials keeps it in the same "small state" family.

Fitting: maximum likelihood on deseasonalised 1-minute or 5-minute returns
from `ohlcv-1m`, phase multipliers divided out first. Fit per product; the
parameters may turn out to be shared across products within an asset class,
which E3 also tests.

The driver runs in business time (section 4.4) and persists across segments,
so an endless Asia still has quiet and busy days.

### 4.3 L2: microstructure engine

A marked multivariate Hawkes process over the events that move the top of
book and print trades, in the family of Bacry and Muzy (2014). State is the
bid and ask on the tick grid with spread `s >= 1` tick.

Event types (six, `d = 6`):

| type | effect on state |
|---|---|
| `P+` | mid up: bid and ask both up one tick |
| `P-` | mid down |
| `S+` | spread widens by one tick (ask up or bid down, side drawn) |
| `S-` | spread narrows by one tick (only enabled when `s > 1`) |
| `T+` | buy trade prints at the ask |
| `T-` | sell trade prints at the bid |

Intensity of type `i` in business time `tau`:

```
lambda_i(tau) = mu_i + sum_j sum_m alpha_ijm * R_jm(tau)
R_jm(tau)     = sum_{tau_k^j < tau} beta_m * exp(-beta_m * (tau - tau_k^j))
```

The kernel for each pair `(i, j)` is a **sum of `M` exponentials** with
shared decay rates `beta_m` (about three, spanning milliseconds to minutes)
and pair-specific weights `alpha_ijm`. This is the load-bearing choice for
indefinite generation:

- `R_jm` is updated incrementally, so the generator carries `d * M` numbers
  of state and never scans history. Cost per event is `O(d * M)`.
- Three exponentials approximate the power-law kernels the literature finds
  (Bacry, Jaisson, Muzy 2016; Hardiman, Bercot, Bouchaud 2013) well enough
  over the timescales a tape needs, and E2 checks that against sign memory
  and the signature plot.
- Stability is the spectral radius of the matrix `sum_m alpha_ijm` being
  below one. It is enforced as a constraint in fitting and recomputed as a
  derived knob when a preset loads, so a hand-edited preset that would explode
  is refused before it generates.

What the cross-excitation buys, mapped to the fidelity tiers:

- `T+ -> P+`: trades deplete the touch and move the price (Tier 1
  trade-through behaviour).
- `T+ -> T+`: order-flow sign memory (Tier 2).
- `P+ -> P-`: mean reversion at tick scale, bid-ask bounce, the signature
  plot (Tier 1 and 3).
- `P+ -> S+`, `S+ -> S-`: spread flicker after a move (Tier 1). The large-tick
  regime (ES, ZN) is the parameter region where `mu_S+` is tiny and `S-`
  excitation is strong, so the spread is pinned at one tick. The small-tick
  regime (crypto) is the opposite corner. The uncertainty-zone parameter
  `eta` of Robert and Rosenbaum (2011) is a derived diagnostic that confirms
  which regime a fitted preset sits in.

Multi-tick jumps: a mark on `P+`/`P-` for jump size, mostly one, with a fitted
heavy-tailed tail. Alternatively left to emerge from `P+ -> P+` clustering.
E4 decides.

Fitting: maximum likelihood for exponential kernels is standard, and the
`tick` library (Bacry et al.) provides both the parametric estimator and a
nonparametric kernel estimator to check the exponential mixture against.
Fit in business time on deseasonalised data within one phase at a time.

Alternative considered: the queue-reactive model (Huang, Lehalle, Rosenbaum
2015). It models the touch queues as a Markov process and produces very
realistic large-tick behaviour, and the 30 days of `mbo` are enough to fit
a level-1 version. It is the fallback if the Hawkes engine fails Tier 1 on
ES. It is not the default because its natural state is queue sizes, which
the tape needs only at low fidelity, and because havoc arms map less directly
onto it.

Havoc mapping on the recommended engine:

| arm | knob transformation |
|---|---|
| `VolStorm` | force or scale the L1 state upward for the window |
| `FlowSurge` | scale `mu_T+`, `mu_T-`, optionally with sign imbalance |
| `LiquidityDrought` | scale `mu_S+` up and `S-` excitation down; scale touch sizes down |

All three are multiplicative on named knobs and leave stability intact
provided the kernel weights are not scaled.

### 4.4 L3: time change and marks

**Time change.** The Hawkes engine runs in business time `tau`. Simulated
nanoseconds `t` advance as

```
dt = d_tau / ( phi_phase(t) * g(V(t)) )
```

where `phi_phase` is the L0 level and `g(V)` the L1 multiplier. Within a
segment `phi` is constant apart from the opening ramp, so simulation is exact
Ogata thinning with the current total intensity as the bound, re-bounded at
each segment boundary. Whether kernel timescales compress with activity (the
time-change hypothesis) or only baselines scale is E1, the first experiment,
because it decides whether one kernel serves every phase.

**Marks on trades.**

- Size: per phase and class, a mixture of point masses at round lots and a
  discretised heavy tail. Fitted from `tbbo`.
- Sweep multiplicity: one aggressor event emits `k` prints, `k` from a fitted
  probability mass function (geometric is the first guess). Prints in a sweep
  share the timestamp or are spaced by a fitted sub-microsecond distribution,
  measured from `mbo`. If the sweep exhausts the touch, it triggers `P+`
  immediately and the remaining prints walk to the next level.
- Aggressor side is the event type.

**Marks on quotes.**

- Touch sizes: a log-AR(1) per side updated at events, conditioned on spread
  and phase. Tier 2, low fidelity accepted.
- Ordering convention: within a nanosecond, trade prints precede the book
  update they cause, matching the exchange feed.

**Reopen.** At a closure transition: draw the gap from the transition knob,
apply it to bid and ask, reset the Hawkes excitation state to zero, apply the
opening ramp, continue.

### 4.5 State, determinism, checkpointing

Total mutable state is: bid, ask, `d * M` excitation registers, the MSM
component states, touch sizes, the phase cursor, and the RNG state. Well
under a kilobyte. Checkpoints are therefore cheap, and a river can be resumed
from any checkpoint rather than regenerated from origin. Use a counter-based
or stream RNG (Philox, ChaCha) seeded from the bundle hash and seed. Avoid
platform-dependent floating point (no fast-math, one `libm`) or two hosts
disagree on the same seed.

Preset size: per phase about 20 level knobs; kernel `d * d * M * 2` about
220 numbers; L1 four; transitions a handful each; marks a few dozen. Under
two thousand numbers in total. Tens of kilobytes of TOML.

---

## 5. Class deltas

The engine and the intake sequence are shared. What changes per class:

| | futures (CME) | crypto (standard) | cash equity | forex |
|---|---|---|---|---|
| clock | full week, daily close, weekend | flat 24/7, one phase, weak weekly cycle | RTH 6.5h with open and close bursts, overnight and weekend gaps; pre and post market as thin phases or excluded | 24/5, Sunday open, Friday close, London and New York overlap peaks |
| tick regime | large (ES, ZN) to medium (CL) | small; spread many ticks | both, by price level | pip grid; medium |
| trades | real prints, aggressor side | real prints, aggressor side | real prints, aggressor side | none: quote-only, tick volume = quote update count, one nominal print per update if a strategy needs volume |
| sizes | whole contracts, round lots | fractional, 1e-8 grid | 100-share lots and odd lots | not applicable |
| corpus | in hand | Binance pull planned (trades + book ticker) | to be sourced | 6E as proxy now; a retail quote feed later |
| gaps | maintenance and weekend | none | overnight and weekend, largest of the four | weekend only |

Perpetual and inverse instruments share the crypto tape; the difference is
settlement, which is not the tape's concern.

Forex is the quote-only corner of the same engine: `T+` and `T-` are disabled
and volume is derived. This is honest because a retail forex feed shows a
strategy exactly that.

---

## 6. The intake sequence: knobs, estimators, provenance

Onboarding a product is this list run in order, terminating in a TOML. Each
knob names its estimator and the schema it consumes. Provenance is `fitted`
(an estimator ran on this product's data), `derived` (computed from other
knobs), or `declared` (asserted by hand).

| step | knob group | estimator | data | provenance |
|---|---|---|---|---|
| 1 | tick size, multiplier | read | `definition` | fitted |
| 2 | contract per day | max daily volume among outrights | `statistics` | derived |
| 3 | phase boundaries | declared from the class taxonomy, checked against activity profile and `status` | `ohlcv-1s`, `status` | declared |
| 4 | per-phase activity and volatility multipliers | mean event rate and realised variance per phase, normalised to the reference phase | `ohlcv-1s`, `mbp-1` | fitted |
| 5 | opening ramp per reopen phase | least squares on the first hour's activity | `ohlcv-1s` | fitted |
| 6 | reopen gap per transition | empirical distribution, Student-t fit, scaled by L1 state at close | `ohlcv-1m` | fitted |
| 7 | L1 MSM parameters | MLE on deseasonalised 1m or 5m returns | `ohlcv-1m` | fitted |
| 8 | spread distribution per phase | empirical pmf in ticks | `bbo-1s` | fitted |
| 9 | Hawkes baselines and kernels | MLE, sum-of-exponentials, in business time, one phase at a time | `mbp-1` | fitted |
| 10 | spectral radius | from step 9 | | derived |
| 11 | trade size pmf per phase | empirical with round-lot masses and tail fit | `tbbo` | fitted |
| 12 | sweep multiplicity and intra-sweep spacing | empirical | `mbo`, `tbbo` | fitted |
| 13 | touch size process | log-AR(1) fit conditioned on spread | `mbp-1` | fitted |
| 14 | multi-tick jump pmf | empirical | `mbp-1` | fitted |
| 15 | uncertainty-zone `eta` | from the fitted engine | | derived |
| 16 | validation report | section 7 | | |

The standard (crypto) tape is the same list with one phase and no
transitions; until the Binance corpus is fitted its knobs are declared, and
the preset says so.

---

## 7. Validation battery

Every metric is computed per real day (or per real week for the slow ones)
across the corpus, which gives an empirical cross-day distribution. The same
metric is computed per synthetic seed. The pass criterion is containment:
the synthetic distribution must sit inside the real one's central band
(quantile containment or a two-sample test that does not reject at a declared
level). A single synthetic path matching the real average is not the test.

**Tier 1.**

- Inter-event time distribution at sub-millisecond, millisecond and second
  scales, per event type.
- Touch resting-time distribution: how long the bid or ask sits at a level.
- Trade-through versus touch probability as a function of distance from mid.
- Spread pmf in ticks, and the distribution of spread episode durations.
- Sweep multiplicity pmf and intra-sweep spacing.
- Largest intra-second excursion; multi-tick jump frequency.
- Reopen gap distribution per transition; activity in the first ten minutes
  after each reopen.

**Tier 2.**

- Volume per minute by phase.
- Aggressor sign autocorrelation out to a few thousand trades.
- Trade size pmf, including the round-lot masses.
- Touch size marginals.

**Tier 3.**

- Autocorrelation of absolute 1-minute returns out to a week.
- Tail index of 1-minute and 1-hour returns (Hill estimator).
- Intraday and weekly activity and volatility profiles.
- Signature plot from 1 second to 5 minutes; variance ratios.

**Fill-path consistency, the most direct test of what mogwai proves.** A
fixed set of probe orders is replayed against real and synthetic tapes under
the venue's own fill rule: at each minute, a limit buy at `k` ticks below mid
and a stop at `k` ticks above, for `k` in `{1, 2, 5, 10}`, each with a fixed
horizon. Compare the distributions of time-to-fill and fill rate. If this
passes and Tier 3 is weak, the tape ships. If this fails and Tier 3 is
perfect, it does not.

**Discriminator test.** A classifier trained to tell real from synthetic
one-minute windows of ticks. Its accuracy above chance is a scalar summary
of everything the battery missed.

**Rendered chart.** The standing gate under the owner's eye, unchanged.

---

## 8. Experiments, in order

Each names the decision its result changes.

- **E1, one kernel or many.** Fit the Hawkes engine separately in `asia` and
  `ny_open` on ES after deseasonalising. Compare kernel decay rates in
  business time. Decides whether a preset carries one kernel with per-phase
  levels (composable, small) or a kernel per phase (larger, fits better).
- **E2, exponential mixture versus power law.** Fit `M = 1, 2, 3` exponentials
  and a nonparametric kernel on ES `mbp-1`. Compare sign autocorrelation and
  the signature plot. Decides `M` and whether the sum-of-exponentials family
  is enough.
- **E3, MSM versus rough volatility.** Fit both on ES and CL `ohlcv-1m`.
  Compare realised-variance scaling from one minute to one month and the
  containment of synthetic monthly volatility in the real distribution.
  Decides L1. Also tests whether L1 parameters are shared within a class.
- **E4, six types versus four, and jump marks.** Fit the engine with and
  without explicit spread events on ES and CL. Decides whether the small
  engine suffices for large-tick products and whether multi-tick jumps need
  a mark.
- **E5, throughput.** Count `mbp-1` events per day for ES, NQ, CL and 6E.
  Prototype the Rust generator with `d = 6, M = 3`, measure events per second
  per core, and state what a month of ES-density warmup costs. Resource cost
  shapes no decision, but this number decides whether a bar-density warmup
  mode is worth having.
- **E6, fill-path consistency on ES.** Run the probe-order test on the E1 to
  E4 winner. This is the gate for the first preset.

E1 and E2 run first and in parallel; they need only ES `mbp-1` and one phase
each.

---

## 9. Risks and open questions

- **Phase stationarity is an assumption.** Data-release minutes inside
  `ny_pre` and settlement inside `ny_close` are bursts the phase average
  smooths over. If they matter for fills, they become sub-phases. The
  activity profile at one-minute resolution shows whether they do.
- **Endless Asia is an extrapolation.** No real Asia session lasts a week.
  The slow driver keeps the tape from being flat, but nothing in the data
  says what a week-long quiet phase looks like. State this in the preset
  provenance of any footprint that repeats a phase.
- **Hawkes near-criticality.** Fitted spectral radii on real order flow sit
  close to one. Fits must be constrained away from one by a declared margin,
  or long paths accumulate variance the data does not show.
- **Roll days and the front-month choice** are fitting hygiene, not tape
  features, but a careless choice leaks the roll's abnormal microstructure
  into the fit.
- **Micro contracts.** MES and MNQ trade on their own books with their own
  flow. Fit them separately; do not derive from ES with a size rescale until
  a comparison shows that is adequate.
- **Floating-point determinism across hosts.** Named in 4.5. Cheap to get
  right at the start and expensive later.
- **Corpus for equities and spot forex** does not exist yet. 6E is a proxy
  for forex dynamics with a futures session and a futures tick.
- **Glossary wording.** `ReopenGap` reads as though scheduled gaps are havoc.
  The owner has said they are not; the entry may want a sentence saying the
  scheduled week's gaps are the tape's own.

---

## 10. Literature

Point processes and order flow:

- Bacry, Mastromatteo, Muzy (2015). "Hawkes processes in finance." Market
  Microstructure and Liquidity. The survey.
- Bacry, Muzy (2014). "Hawkes model for price and trades high-frequency
  dynamics." Quantitative Finance. The engine's ancestor.
- Bacry, Jaisson, Muzy (2016). "Estimation of slowly decreasing Hawkes
  kernels: application to high-frequency order book dynamics." Quantitative
  Finance. Power-law kernels, the eight-type model.
- Hardiman, Bercot, Bouchaud (2013). "Critical reflexivity in financial
  markets: a Hawkes process analysis." European Physical Journal B.
  Near-criticality.
- Jaisson, Rosenbaum (2015). "Limit theorems for nearly unstable Hawkes
  processes." Annals of Applied Probability.
- Ogata (1981). "On Lewis' simulation method for point processes." IEEE
  Transactions on Information Theory. Thinning.
- Lillo, Farmer (2004). "The long memory of the efficient market." Studies in
  Nonlinear Dynamics and Econometrics. Sign memory.
- Engle, Russell (1998). "Autoregressive conditional duration." Econometrica.

Order book and tick regime:

- Huang, Lehalle, Rosenbaum (2015). "Simulating and analyzing order book
  data: the queue-reactive model." Journal of the American Statistical
  Association. The fallback engine.
- Robert, Rosenbaum (2011). "A new approach for the dynamics of
  ultra-high-frequency data: the model with uncertainty zones." Journal of
  Financial Econometrics. `eta`.
- Dayri, Rosenbaum (2015). "Large tick assets: implicit spread and optimal
  tick size." Market Microstructure and Liquidity.
- Eisler, Bouchaud, Kockelkoren (2012). "The price impact of order book
  events." Quantitative Finance.
- Bouchaud, Bonart, Donier, Gould (2018). "Trades, Quotes and Prices."
  Cambridge University Press. The textbook for all of the above.

Volatility:

- Calvet, Fisher (2004). "How to forecast long-run volatility: regime
  switching and the estimation of multifractal processes." Journal of
  Financial Econometrics. MSM.
- Gatheral, Jaisson, Rosenbaum (2018). "Volatility is rough." Quantitative
  Finance.
- Andersen, Bollerslev (1997). "Intraday periodicity and volatility
  persistence in financial markets." Journal of Empirical Finance.
  Deseasonalising.
- Cont (2001). "Empirical properties of asset returns: stylized facts and
  statistical issues." Quantitative Finance. The Tier 3 checklist.

Software:

- `tick` (Bacry, Bompaire, Gaïffas, Poulsen). Python library for Hawkes
  estimation, parametric and nonparametric.
- `databento-python` and DBN for corpus access.
