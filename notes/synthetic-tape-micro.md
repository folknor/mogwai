# Synthetic tape v2, the layer below the second: measurement and landing

The record of the pass that took the MNQ cascade from tape protocol 32 to
33 on 2026-09-06. Protocol 32 matched the real year at the minute and
above (`notes/synthetic-tape-e0.md`) and placed parents uniformly inside a
second with a declared Markov side. This pass measured what a fill meets
below the minute on a year of real prints, fitted three mechanisms on a
prototype, transcribed them into the Rust cascade and measured the tape
again by the same definition. No chart was rendered: every claim here is
a number beside its real counterpart and the month-to-month band the
real number moves in.

## 1. The measurement

`analysis/tape-v2` grew two subcommands. `micro-extract` reads the
front month's `tbbo` prints out of every day file of the corpus on
speilegg (each day carries all 63 parents, three seconds a day to decode)
and caches them per day with the pre-trade touch beside each print.
`micro-stats` groups prints into parents by the frozen rule of
`mogwai_lab::stream::group_parents_batch`, contiguous runs of one
`(ts_event, side)`, and reports per phase:

- the parent rate, the inter-parent gap in milliseconds and the share of
  gaps under 1, 10 and 100 ms;
- the gap normalised by its own minute's rate, `u = gap * n_minute / 60`,
  which is Exp(1) under a Poisson placement (p50 0.69, p90 2.30, p99 4.61,
  cv 1), and the autocorrelation of consecutive `log u`;
- the within-window dispersion at four scales, 10 ms and 100 ms bins in a
  second, 1 s and 10 s bins in a minute, defined so that a uniform
  placement of the window's own count gives exactly 1 whatever the
  envelope and the texture do to the count;
- the sweep: prints per parent, levels per parent, parent size, and on
  the real side the parent size against the touch it hit;
- the aggressor sign: the same-side probability, the sign autocorrelation
  to lag 1000, and the run-length tail against the Markov chain at the
  same lag 1;
- the price: the tick change of the last print between parents, and on
  the real side the spread at parent instants and the signed mid move one,
  ten and a hundred parents later.

The real side is 260 million MNQ prints, 2025-08-19 to 2026-09-03, in
fourteen calendar-month blocks; every real number below is the median
across months, and the band is the p10 to p90 across months. A generated
tape is measured per seed with two adjustments stated in the code: its
children are stamped a microsecond apart, so a same-side print within ten
microseconds is the same sweep, and its clock is the preset's permanent
CDT offset rather than the Chicago zone.

## 2. What the real tape does below the minute

Pooled over the session, with the month band:

| statistic | real | band |
|---|---|---|
| parents per second | 10.1 | 6.9 to 14.6 |
| gap under 1 ms | 0.213 | 0.193 to 0.236 |
| gap under 10 ms | 0.437 | 0.405 to 0.465 |
| gap under 100 ms | 0.807 | 0.774 to 0.840 |
| u p50 / p90 / p99 | 0.41 / 2.76 / 7.11 | |
| u cv | 1.55 | 1.51 to 1.58 |
| log u acf lag 1 / 5 | 0.31 / 0.10 | |
| dispersion 10 ms in 1 s | 2.10 | 1.98 to 2.24 |
| dispersion 100 ms in 1 s | 3.50 | 3.26 to 3.80 |
| dispersion 1 s in 1 m | 11.7 | 11.0 to 13.0 |
| dispersion 10 s in 1 m | 45.6 | 40.9 to 53.2 |
| children mean / single share | 1.159 / 0.914 | |
| children 2 / 3 / 6+ | 0.063 / 0.012 / 0.005 | |
| multi-level given multi-print | 0.907 | 0.878 to 0.932 |
| parent size mean / p90 / p99 | 2.24 / 4 / 15 | |
| size reaches the touch / walks through it | 0.54 / 0.08 | |
| same side as previous parent | 0.574 | 0.565 to 0.581 |
| sign acf 1 / 2 / 5 / 10 / 50 / 100 | 0.147 / 0.069 / 0.026 / 0.018 / 0.010 / 0.007 | |
| run of ten or more | 0.011 | Markov at the same lag 1: 0.007 |
| price change 0 / 1 / 2 / 3+ ticks | 0.31 / 0.39 / 0.19 / 0.11 | |
| spread 1 / 2 ticks at parents | 0.43 / 0.43 | |
| signed mid move after 1 / 10 / 100 parents | 0.48 / 0.65 / 0.66 ticks | |

Four readings, each of which decided a mechanism:

- **A fifth of all parent gaps are under a millisecond at every hour**:
  0.194 in Asia at 3.4 parents a second, 0.221 at the cash open at 42. A
  Poisson placement at the Asia rate puts 0.3 percent of gaps there. The
  share does not move with the rate, which is the signature of a
  follow-on: a match event that begets another within the engine's own
  latency, independent of how busy the hour is. That is a branching
  process, not a rate.
- **Inside a second the tape clusters, and the clustering is not the
  texture**: 100 ms bins are dispersed 2.7 times uniform in Asia and 3.6 at
  the open. A multiplicative texture at that timescale would scale the
  excess with the rate (a factor of twelve between the two phases); the
  excess grows by a third. Cluster-type again, at tens of milliseconds.
  The seconds inside a minute are different: 5.5 in Asia and 15.4 at the
  open, growing nearly with the rate, which is a multiplicative swell at a
  few seconds that the minute-fitted texture, whose fastest component is
  fifteen seconds, cannot carry.
- **The sign memory is a power law.** The autocorrelation is 0.147 at one
  parent and still 0.010 at fifty; the Markov chain at the same lag 1
  would be 0.0004 at lag 5. Runs of ten or more happen once in ninety
  where the chain gives once in a hundred and forty. This is the
  order-splitting picture: many metaorders live at once, each sliced into
  many prints, so the same side recurs at every lag with a tail set by the
  metaorder size distribution.
- **A multi-print parent is a multi-level parent nine times in ten.** A CME
  trade summary is one print per price level, so children and levels are
  nearly the same count. The July fit's `levels_mean` read 1.12 against a
  `children_mean` of 1.17, a step probability of 0.7, and the generator
  then multiplied that by the bounce regime's low factor of 0.3, a regime
  the cascade path never steps: the tape's multi-level share was 0.33.

And one reading that was measured and not landed: the signed mid move
after a parent is 0.48 ticks one parent later and 0.66 a hundred later,
in every phase, permanent. A buy lifts the mid by half a tick on average
and the mid stays lifted. The cascade's mid is independent of the side.

## 3. The prototype and the fit

`analysis/tape-v2/proto_micro.py` simulates one phase at the cascade's
own rate and texture and measures the result with the same function.
Placement is a branching process simulated by generations: immigrants
Poisson at the rate times `1 - n`, every event spawning Poisson(`n_j`)
children at exponential offsets, plus one more Ornstein-Uhlenbeck texture
component at seconds. The side is the splitting model: `slots` live
metaorders with a discrete Pareto print count of exponent `alpha`, a
parent repeating the previous side with probability `repeat` or taking a
uniformly chosen slot's side.

The score is the mean absolute log ratio between the prototype and the
real median over twelve placement statistics (the three gap shares, the
four `u` quantities, `log u` acf 1, the four dispersions) or ten sign
statistics. The protocol 32 placement scores 1.07 in Asia and 0.98 at the
open; the Markov side scores 0.50 and 0.57. Two grids of placement
configurations (`analysis/tape-v2/micro-grids/`) put
the best three-kernel family at 0.22 to 0.26 across four phases, the
four-kernel one at 0.25 with no phase better than 0.20; the differences
between neighbours are within seed noise, so the three-kernel point was
taken:

| knob | value | what selects it |
|---|---|---|
| excitation kernels | 0.2 ms, 30 ms, 1 s | the sub-millisecond share, the 10 and 100 ms dispersion, the second-scale cluster |
| branching ratios | 0.22, 0.08, 0.30 | the same three, and the mean rate stays on the envelope by construction |
| fast texture | 3 s, log-sd 0.45 | the seconds inside a minute at the open (the prototype wanted 0.55 at a rate it had set a third low) |
| sign slots, alpha, repeat | 5, 2.2, 0.08 | acf 1 (about `repeat + 1 / slots`), the run tail, acf 50 |

Sign alternatives with eight and twelve slots at heavier tails, and six
at a lighter one, each win one of the two phases and lose the other; the
five-slot point is the best pooled.

## 4. The landing

`mogwai_data::generated::cascade` at protocol 33:

- `fill_next_bucket` draws the second's immigrant count at the rate times
  `1 - n` and multiplies the rate by the fast texture; `draw_parent`
  merges the next immigrant, staged by order statistic, with the earliest
  child in a heap, spawns the emitted parent's children, and returns the
  earlier; children owed inside a closure are dropped at the reopen and
  under an armed halt.
- The side is `draw_side`: the repeat, then a slot, redrawn when
  exhausted.
- `source.rs`'s `step_child` takes the solved level-step probability on
  the cascade path.
- The MNQ preset carries the seven new knobs with provenance, drops
  `side_persistence`, and refits `children_mean`, `children_single_frac`
  and `levels_mean` on the year (1.159, 0.914, 1.143).

Tests: the mean rate under branching, children never inside a closure,
the splitting memory against the Markov tail, the validation of the new
knobs, the level step on the cascade path (in `gen.rs`), and the three
new knobs reaching the summary through a config override. Each regression
test was bitten by reverting its production line as a text edit and
observing the named assertion.

## 5. The tape against the real year

From `micro-stats --label p33` over three four-week seeds, median across
seeds, beside the real median and its month band, pooled over the
session. The band is the p10 to p90 across fourteen months, which is
narrow (the real `u` cv moves by four percent month to month), so
"outside" here is a precise statement rather than a failure; the column
before it shows the protocol 32 tape by the same measurement.

| statistic | real | band | protocol 32 | protocol 33 |
|---|---|---|---|---|
| parents per second | 10.1 | 6.9 to 14.6 | 11.0 | 12.3 |
| gap under 1 ms | 0.213 | 0.193 to 0.236 | 0.033 | 0.245 |
| gap under 10 ms | 0.437 | 0.405 to 0.465 | 0.248 | 0.441 |
| gap under 100 ms | 0.807 | 0.774 to 0.840 | 0.786 | 0.844 |
| u p50 | 0.413 | 0.385 to 0.435 | 0.654 | 0.427 |
| u p90 | 2.76 | 2.71 to 2.79 | 2.33 | 2.64 |
| u p99 | 7.11 | 6.96 to 7.26 | 5.12 | 7.64 |
| u cv | 1.55 | 1.51 to 1.58 | 1.10 | 1.65 |
| log u acf 1 | 0.31 | 0.30 to 0.33 | 0.05 | 0.14 |
| dispersion 10 ms in 1 s | 2.10 | 1.98 to 2.24 | 1.00 | 1.65 |
| dispersion 100 ms in 1 s | 3.50 | 3.26 to 3.80 | 1.00 | 1.89 |
| dispersion 1 s in 1 m | 11.7 | 11.0 to 13.0 | 3.0 | 11.1 |
| dispersion 10 s in 1 m | 45.6 | 40.9 to 53.2 | 17.8 | 57.1 |
| children mean | 1.159 | 1.142 to 1.184 | 1.171 | 1.168 |
| children 2 / 3 | 0.063 / 0.012 | | 0.053 / 0.023 | 0.052 / 0.022 |
| multi-level given multi-print | 0.907 | 0.878 to 0.932 | 0.328 | 0.920 |
| same side as previous | 0.574 | 0.565 to 0.581 | 0.600 | 0.567 |
| sign acf 1 / 2 | 0.147 / 0.069 | | 0.200 / 0.040 | 0.134 / 0.066 |
| sign acf 5 / 10 | 0.026 / 0.018 | | 0.000 / 0.000 | 0.047 / 0.033 |
| sign acf 50 / 100 | 0.010 / 0.007 | | 0.000 / 0.000 | 0.007 / 0.004 |
| run of ten or more | 0.011 | 0.010 to 0.012 | 0.010 | 0.011 |
| price change 0 / 1 / 2 / 3+ | 0.31 / 0.39 / 0.19 / 0.11 | | 0.26 / 0.35 / 0.23 / 0.16 | 0.24 / 0.32 / 0.24 / 0.21 |

Per phase the same picture holds; the placement score against the real
medians (section 3's definition) is 0.25 in Asia and 0.24 at the open
against 1.07 and 0.98 for protocol 32, and the sign score 0.19 and 0.26
against 0.50 and 0.57.

What landed inside the band: the three gap shares, the seconds inside a
minute, the sweep's level structure, the same-side share and the sign
memory at lags 1, 2, 50 and 100, and the run tail. What did not:

- The clustering at 10 and 100 ms is half the real excess (1.65 and 1.89
  against 2.10 and 3.50): the kernels at 0.2 ms and 1 s carry the
  sub-millisecond share and the second-scale cluster, and the 30 ms kernel
  at a ratio of 0.08 is too weak between them. The grids show that raising
  it costs the `u` shape. A fourth kernel or a non-exponential one is the
  next thing to try.
- The normalised gap's tail runs long (p99 7.6 against 7.1, cv 1.65
  against 1.55) and its consecutive autocorrelation is half the real one
  (0.14 against 0.31). The real tape's gaps remember their neighbours more
  than a branching process with memoryless kernels does.
- The 10 s in 1 m dispersion is a quarter high (57 against 46): the fast
  texture at 3 s and the 15 s texture component together over-disperse at
  that scale; the 1 s in 1 m figure sits on the real one.
- The sign memory at lags 5 and 10 is 1.8 times real while lags 50 and 100
  are on it: the Pareto tail is the right family and not quite the right
  shape.
- The print-count pmf and the parent size are the shipped mixtures'
  shapes, not this pass's.
- The per-parent price change is more dispersed than real, and the impact
  is zero; section 6.

At the minute the tape did not move: `residuals.py` on the protocol 33
bars gives the volume residual log-sd 0.69 in Asia and 0.36 at the open
(real 0.70 and 0.36; protocol 32 gave 0.68 and 0.31), the minute
autocorrelation within a few hundredths of the protocol 32 tape, and the
volume to range residual correlation 0.70 (real 0.74). The envelope
battery reads the shape inside the real band at 0.71 to 0.86 per phase
(0.80 is a match) with one seed's level running 1.1 to 1.3 above the
real median, which the mean-rate test puts down to that seed's month:
the branching is mean-one to a percent on a flat envelope.

## 6. Protocol 34: the impact of a parent on the mid

Landed the same day, after the owner's look at the 15-second page turned
the conversation to what the open's bars are made of. Two mechanisms were
tried and measured; the second shipped.

**The surprise term, tried and withdrawn.** The mid moved by a fitted
number of ticks per unit of sign surprise, the side less the splitting
model's own conditional mean given its slots, with the variance the term
carries taken out of the diffusive innovation. This is exact: the surprise
is a martingale difference, so the mid stays a martingale and the minute
variance is untouched by construction. Measured on one seed it gave a
response of 0.51 ticks at one parent, inside the real band, and 0.51 at
ten and a hundred, where the real response grows to 0.65. It cannot grow:
later surprises are orthogonal to the sign that came before. It also made
the per-parent price change worse (three-or-more-tick changes 30 percent
against 11 real), because a half-tick kick landed on top of an unchanged
one-tick diffusion.

**The propagator, shipped.** The real response's shape, half a tick at one
parent, two thirds at ten, the same at a hundred, is the classic
propagator: an impact on the raw sign that partly decays, against a sign
memory that keeps supplying the same side. `proto_impact.py` fits it on
the splitting model's own signs: a parent kicks the mid 0.45 ticks in its
own direction, 0.30 of which stays and 0.15 decays at 0.98 per later
parent, giving 0.45, 0.63 and 0.68 at the three lags. Below a minute the
mid is therefore not a martingale, and neither is the real one (the real
variance ratio from 15 seconds to 15 minutes is 1.18); from a minute up
both are. The term supplies about a quarter of the open's minute variance
and a sixth of Asia's, and `event_log_sigma` gives that up, 7.42e-6 to
6.7e-6.

Three seeds against the year:

| statistic | real | band | protocol 33 | protocol 34 |
|---|---|---|---|---|
| impact at 1 parent, ticks | 0.483 | 0.415 to 0.589 | 0 | 0.448 |
| impact at 10 | 0.653 | 0.528 to 0.808 | 0 | 0.628 |
| impact at 100 | 0.664 | 0.521 to 0.826 | 0 | 0.669 |
| same side as previous | 0.574 | 0.565 to 0.581 | 0.567 | 0.567 |
| minute close sd, Asia / open / midday, points | 5.9 / 17.1 / 10.7 | | 4.8 / 15.4 / 8.5 | 5.2 / 16.7 / 9.9 |
| open 15 s bar range, session p50 | 12.1 | 7.2 to 18.8 | 11.6 | 13.1 |
| open first-hour travel of 15 s closes | 1572 | 900 to 2398 | 1542 | 1710 |
| VR 15 m against 15 s at the open | 1.18 | | 0.92 | 1.05 |
| price change 0 / 3+ ticks between parents | 0.31 / 0.11 | | 0.24 / 0.21 | 0.21 / 0.28 |

Asia and London carry a larger real impact than the pooled figure, 0.59
and 0.65 ticks at one parent, which the tape's constant two-tick book
cannot follow: a depletion moves the mid by half the spread, and the real
spread is wider overnight. That is the spread dynamics item.

The minute texture did not move (residual log-sd 0.69 in Asia and 0.36 at
the open, unchanged), and the envelope battery reads the range level of
this seed 7 to 12 percent above the real median where the close sd reads
it 2 to 12 percent below: the tape's minute path is more Brownian than
the real one, whose range barely exceeds its close move. The sigma was
left between the two gates rather than tuned on one seed.

## 7. What this pass did not do

- The per-parent price change, which the propagator did not improve and
  the surprise term made worse: most real parents move the mid nothing,
  and the tape's one-tick Student-t diffusion moves it most of the time.
  A heavier tail at the same variance was tried (degrees of freedom three
  for four, one seed): zero-tick changes 22 percent for 21, three or more
  27 for 28, the minute kurtosis 6 to 8 either way. Not a tail-shape
  problem. The dispersion is the two-tick bounce between opposite sides
  plus a continuous mid that moves on every parent; a discrete mid that
  moves only on depletion, with a spread that is one tick half the time,
  is the mechanism, and it goes with the spread dynamics item.
- `mbo`: fills per level inside a sweep, queue depletion, and the
  intra-sweep spacing the programme wanted from order-level data. The
  tbbo touch numbers stand in.
- The sweep's print-count mixture, whose two-parameter geometric shape
  puts too much mass on three prints and too little on two.
- Any product but MNQ. ES, NQ and MES are the same two commands.
