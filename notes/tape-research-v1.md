# Tape research, version 1: what it produced

The measure-and-fit arc ran from roughly 2026-08-01 to 2026-08-12, spent over a
hundred commits, drove `TAPE_PROTOCOL_VERSION` from 1 into the twenties, and
ended with a tape the owner judged indistinguishable from the one it started
with. The owner closed it 2026-08-23.

This page replaces about 700 KB of protocol specs, preregistrations and
contracts, all of which are in git history. It carries the three things that
survive the approach dying: what was actually tried and what each attempt
returned, the facts about markets and about this generator that would still be
true under any methodology, and the diagnosis of why the loop produced nothing.

Notes-class: transient, no truth guarantee, nothing durable cites it. The data
format facts that came out of the same arc are durable and were moved to
`reference/corpus-formats.md` instead.

## What was tried, and what it returned

| Attempt | Outcome |
|---|---|
| Protocol 12a, the measurement ladder | `no-family-eligible`. No rung fired; the arrival family failed closed, meaning unmeasurable rather than passed. The owner overrode the ladder's silence to point 12b at arrival composition. |
| Protocol 12b, arrival composition | `no-arrival-admissible-candidate-in-frozen-search-space`, returned twice. The 2026-08-10 run over 787 cells rested on three defective gates; the 2026-08-11 run over 1,402 cells across five mechanism families is the real one. A3 fails all 1,402. Twenty cells fail A3 and nothing else. |
| Count curve | Ran, and its result is recorded only as quotations elsewhere. Generated Fano flat at 31 to 37 across every horizon; between-session Fano short of observed by 25 to 50 times. |
| Ordered counts | `insufficient_support` first, then `completed` after the structural-inapplicability amendment, with hour 20 structurally null. |
| Slow geometry | `completed`, selecting no architecture. Establishes that common-mode scores reject exchangeability over the July dates at p = 0.0215, and nothing narrower. |
| Successor contract, Stage M | Signed, never reached Stage F. Stopped at Tier 2's `no_one_month_slow_confirmation_design`. |
| Sampling frame | `FAIL`. Zero of six families passed, three of fourteen targets. Both mandatory families failed. Wider than the summaries said. |
| Pair test, NQ as an MNQ proxy | `fail` on mandatory family P5, `zero_change_frac`, median d = -0.1075 against a 0.10 tolerance. Twelve of fourteen targets passed. |

Two corrections to how these were being summarized. The sampling-frame failure
was total rather than marginal, and its two strongest results are negative
correlations that passed. And the 12b verdict string was returned twice with
completely different evidential standing, so a citation of it must say which run.

One outcome is not recorded anywhere: the C3 candidate was specified as the last
permitted Tier 2 attempt, and no document or commit records it being evaluated.
Either it never ran and the terminal outcome was declared by the direction
change, or its result was never written down. Do not assert it was evaluated.

## What is true about these markets

- Observed MNQ arrival clustering compounds across scale, and the correlation
  lives in the minutes-to-hour band rather than sub-second. Within-session Fano
  compounds nine to thirty times between 1 s and 300 s. Fitted single-exponential
  tail times run 278 to 3,277 s with a median near 1,000 s, assumption-conditioned
  and not proof of one correlation time.
- There is a stable session-wide common mode in MNQ arrival rates: hourwise
  permutation p = 0.0005, leave-one-out stable, with positive one-day dependence
  at p = 0.0015. It explains roughly 28 to 30 percent of the residual structure.
- The arrival mechanism must be right-skewed. Symmetric log-mixtures are ruled
  out by mechanism, not by taste: high-sigma log-OU cells failed by ratios of 47
  to over 15,000 because they buy the observed high-count tail at the price of
  silence the observed tape does not carry.
- Activity-conditioned clustering is refuted. The A3 crossing point does not move
  with hour activity, and the apparent gradient in pooled signed residuals is
  confounded, because the four hours with positive sign are simultaneously the
  four busiest and the four with the fewest observed zero windows.
- Hour 20, the partial session containing the close halt, is a genuinely distinct
  stratum, and it independently broke four measurements. Three effects are mixed
  in it: support geometry, estimation uncertainty, and possibly real close
  microstructure. The durable ruling is that the structure is calendar phase and
  segment position, not a free parameter attached to a UTC hour.
- July MNQ is nearly a single-child tape: 90.5 percent one-print parents,
  `children_mean` 1.1711. The crypto-fitted conditioning structurally could not
  express that shape.
- MNQ volatility per event does not track the session volume profile. Refitting
  peak-to-trough from inferred parent counts rather than volume moved it from
  27.51x to 14.5x, and the per-minute volatility proxy's 3.4x swing was almost
  entirely arrival-density double counting. The true per-parent robust scale is
  nearly flat and slightly inverted, calmer in cash hours than overnight.
  Complicating this: over the ten July sessions of the pair test, volume did not
  overstate arrivals at all (ratio 0.95). Ten sessions is a point estimate, and
  the two results should travel together.
- NQ and MNQ agree on process shape and disagree on level. Long-memory ACFs
  within 0.008 absolute, sweep structure and return shape passing within a few
  percent, while MNQ prints about 4.3 times as often. So NQ informs shared
  process-shape reasoning even though it is not a purchasable proxy.
- Asia has the calmest texture of the four session windows, measured: median
  one-minute move 1.75 points against London 2.25, NY-afternoon 3.75, NY-morning
  5.00. Reopen gaps across the daily close are Asia-specific, around -1.31
  percent; London and NY have little to gap across.

## What is true about this generator

These are mechanism findings. They describe defects and behaviours in the code
that ships, and they outlive the gates that found them.

- **The incumbent's arrival clustering is at the wrong scale, and the mechanism
  is fully diagnosed.** The chain is indexed by event, so at several hundred to
  several thousand parents per minute a 0.10 switch rate gives a correlation
  length of about ten parents. One minute averages over many independent runs,
  and what survives into the minute count is a deterministic hourly rate plus
  Poisson noise, giving Fano near one by construction. The generator over-clusters
  at 1 s by up to six times and under-clusters at 300 s by up to seventy-seven.
  The repair is a redistribution of clustering across scales, not an added slow
  component: bolting slow variation on top preserves the one-second excess and
  probably worsens total dispersion. This is the most transferable finding here.
- The generated per-minute parent count is effectively degenerate. Seeds 1 and 2
  produce identical bin counts at hours 19 and 20, so seed-to-seed variation does
  not reach a bin edge.
- `ARRIVAL_MEAN_CAL = 0.944` is an artifact of the shipped sampling scheme, not a
  property of the process. It corrects that scheme's realized-mean inflation; the
  integrated frame has no such inflation, so applying it there double counts by
  exactly 1.05932. A Jensen-gap explanation was refuted in closed form. The
  shipped path therefore carries a 5.5 to 7.0 percent absolute-rate calibration
  conflict against the observed July month. It is a derived constant wearing a
  fitted constant's clothes.
- **The shipped generator hardcodes a July-style UTC offset and degenerates in
  winter.** Twelve of twenty-four Stage M control walks, exactly the winter
  rotation, collapsed the local-hour-22 stratum to zero variance. The 12a
  subcontract froze the daylight offset, so applied to a standard-time month it
  schedules a dead pre-open hour and excludes each session's real final trading
  hour: November put 840,315 rows outside declared sessions, 3.8 percent, against
  September's 0.5 percent. The method-independent lesson is that single-month
  validation is structurally blind to seasonal frame errors, so any regression
  suite needs one daylight month, one standard month and one transition month.
- The `children_mean` clamp defect: at an observed mean of 1.1711 the quiet-state
  multiplier draws an impossible sub-one mean, which gets clamped to one, breaking
  the mean-preserving identity. Realized mean inflates to about 1.44 at any
  configured value. A silent clamp turning a configured parameter into a constant.
- `step_child` advances the clock across the whole child burst, so arrival-only
  simulation is impossible with this generator: the next gap opens from the last
  child's timestamp. Three consecutive spec revisions asserted a shared-kernel
  claim this falsifies. `begin_event` additionally carries four runtime
  transformations around the gap, one of which moves price rather than time.
- The minute-range extreme tail is an unconstrained volatility-cluster
  phenomenon, not a rail artifact: the frozen trace of the 420.75-point minute
  shows zero clamp hits on all three rails. Body correct, extreme tail two to four
  times heavy, per-seed maxima reaching 4,333 ticks against the real month's 968.
- Re-centring hourly means cannot buy within-hour mixture. The negative control
  failed with a normalizer drift of 1.0027, confirming the hourly means were never
  what was wrong.
- The exact-close endpoint second was being silently dropped once per session by
  an hour-attribution artifact: a scheduled, populated second whose only fault was
  mapping into an excluded UTC hour.
- `AutoCorr`'s zero-variance guard only fires for an exactly representable
  constant. A series constant at an irrational value leaves a positive float
  residue, the guard misses, and the result comes out of catastrophic
  cancellation. The exactly-zero path is pinned; this one is knowingly not.

## What is true about the segment sampler

Carried from the slice-1 work, which is the live direction and is not part of
what died.

- The 2026-08-18 owner gate accepted the texture of real resampled segments.
  That acceptance is the load-bearing empirical result validating the whole
  resampled premise, and it is unrecoverable except by re-rendering.
- The gaps-OFF control was not a control. Suppressing the measured gap injection
  does not suppress the level discontinuity between spliced segments, so the
  gaps-OFF arm still jumped 87, 74 and 71 points at seam multiples of 540 bars,
  and the A/B never isolated the feature it was built to judge.
- The owner's actual rejection, 307 points in two minutes, appeared identically
  in both arms. It is carried in from the segment data, not produced by
  composition.
- The library's bounded-variety problem, which was the one open design question
  between real-resampled and generated-with-features segments, was an artifact of
  having eleven delivered months. With the NAS corpus it is largely dissolved,
  and the open question now leans toward real segments by default.

## Why the loop produced nothing

Three documents each diagnosed one face of this and nobody assembled them. The
assembly, and the part after it, is inference rather than anything the documents
claim.

What they wrote down. The successor contract: 12b produced a valid negative in a
search space frozen before the multi-horizon target geometry was known, so the
measurement succeeded and the space could not adjudicate the objective. The
count-curve prereg, more sharply: every gate in 12a and 12b judged a single
horizon, A3 at one second and the ladder at sixty, so no statistic anywhere could
see a curve that is level when it should climb. The slow-geometry prereg named it
as a repeatable trap rather than an accident.

Four compounding mechanisms, of which only the first is in the documents.

**Freezing before the geometry is known is structural, not accidental.** The
integrity rule that makes each verdict trustworthy - freeze the criterion before
seeing the data - forces the search space to be specified at the moment of
maximum ignorance. 12b froze five mechanism families before anyone had measured
that the target was a Fano curve rather than a Fano point, and the measurement
that revealed it could not have run earlier without peeking. Every cycle
therefore buys one bit, "not this space", at a cost of weeks, and the amendment
rules forbid spending what you learned inside the same cycle. 12b's own close says
the search did not lack candidates but lacked agreement about what the criterion
should be. Twenty mechanisms passed three of four gates. Mechanism supply was
never the bottleneck.

**The gate machinery consumed the effort the search needed.** Three of the four
hard gates in the first 1,402-cell measurement were defective, and defective in
the flattering direction: A4 compared a wall-clock span including closures against
a cadence that cannot spend them, and A2's level limb summed histogram
occurrences with no exposure normalization, returning the ratio of generated to
observed session counts for every mechanism at every parameter point. A2 went
from zero passing to 618 passing on repair. The apparatus was where the bugs
were, and the apparatus was most of the work.

**The confirmation step was never affordable.** July was spent as design evidence
seven times over, so the contract correctly refuses to confirm anything on it,
and every criterion then needs a fresh month with its own purchase. Stage M's
Tier 2 then found that a one-month confirmation projection with adequate power
against the frozen alternatives may not exist at all, because raw between-month
regime spread dominates the effect being tested. That outcome is arithmetic, not
bureaucracy: a method whose validation step cannot be afforded cannot iterate,
and a method that cannot iterate cannot search.

**The objective was never a statistic.** The goal is a plausible tape a strategy
can be forward-tested against. Every gate measured a projection of that - a Fano
at a horizon, a count p99, a wall-time contour, a zero fraction. When the charts
were finally rendered, the defects the eye found were not among the things any
gate measured: the cash open does not ignite, there are no reopen gaps at all,
volume looks uniform. The proxy was not merely lossy, it was orthogonal to the
largest defects. Meanwhile the one gate that did address the real objective, the
owner's eye on real-segment texture, settled in an afternoon a question five
fitted mechanism families had not settled in a month.

The synthesis for whatever comes next: **preregistration is the right discipline
for adjudicating a claim and the wrong discipline for searching a space.** It
converts exploration into a sequence of expensive one-bit experiments, each
frozen at the moment of least understanding, each requiring a confirmation
population that was not available. Use it to certify a candidate already believed
in. Do not use it to find one. And put the cheapest gate that can see the actual
objective, which here is a rendered chart, at the front of the loop rather than
after it.

This sits on top of what `CLAUDE.md`'s process reset already ruled about the
consensus gate converging to the verifier's utility function. That mechanism
explains the volume of the churn; these four explain why the churn produced
nothing even where the measurements were correct.

One thing worth recording in the discipline's favour. The count-curve
preregistration bound generator configuration and fingerprint identity to an
artifact that recorded neither, and its generated backcheck would have compared
across a version bump without saying so. The implementing session caught it and
refused to proceed. The discipline worked exactly as designed while producing
nothing, which is the whole shape of the problem.

## Live inputs to whatever v2 becomes

- Derive `zero_change_frac` rather than fitting it. The pair test is the evidence:
  NQ and MNQ share a tick, an index, sessions and minutes, and differ by 0.1075
  because MNQ prints 4.3 times as often. So the driver is tick size relative to
  per-print price movement, which is jointly the tick-to-price ratio and the
  arrival rate, rather than the tick-to-price ratio alone as the purchase report
  framed it. Independently, over nineteen BTCUSDT months it does not track a
  volatility score (held-out rho -0.643, unstable), so it is not a
  stress-responsive knob either. Fitting it as a free per-instrument parameter is
  fitting a quantity two other parameters already determine. The Kraken tick grids
  remain a cheap multi-instrument validation, unharmed by whole-second timestamps.
- The arrival-clustering repair in the generator findings above, if any successor
  keeps a generative arrival process at all.
- The three shipped defects: the `ARRIVAL_MEAN_CAL` double count, the hardcoded
  July UTC offset, and the `children_mean` clamp. These are live regardless of
  direction and are carried in `todo.md`.

## Appendix: the phase 0 Kraken characterization

Salvaged from `analysis/findings.md`, which was otherwise an orphan. Eight
Kraken pairs, 298,003,956 trades, anchored on `XBTUSD`. This is the multi-
instrument tick-grid evidence the `zero_change_frac` derivation would be
validated against, and it is free: the pairs span seven orders of magnitude of
tick size at prices ranging over four, which is exactly the spread a
tick-relative-to-price rule needs to be tested across.

| pair | trades | disp | ret1 | abs ret1 | zchg | tick | pdec |
|---|--:|--:|--:|--:|--:|--:|--:|
| `XBTUSD` | 81,810,187 | 36 | -0.20 | 0.31 | 0.47 | 0.1 | 1 |
| `USDTUSD` | 67,308,081 | 145 | -0.06 | 0.15 | 0.75 | 0.0001 | 4 |
| `ETHUSD` | 53,416,611 | 67 | -0.18 | 0.25 | 0.42 | 0.01 | 2 |
| `XRPUSD` | 24,848,999 | 332 | -0.17 | 0.27 | 0.38 | 1e-05 | 5 |
| `SOLUSD` | 21,973,204 | 184 | -0.09 | 0.25 | 0.54 | 0.01 | 2 |
| `XDGUSD` | 19,185,656 | 1628 | -0.12 | 0.21 | 0.36 | 1e-07 | 7 |
| `ADAUSD` | 16,352,376 | 407 | -0.16 | 0.30 | 0.34 | 1e-06 | 6 |
| `DOTUSD` | 13,108,842 | 132 | -0.07 | 0.20 | 0.34 | 0.0001 | 4 |

Pooled UTC intensity peaks at the London and New York overlap (16:00 at 5.55
percent) and troughs in the Asian small hours (05:00 at 3.12 percent), a swing
of about 1.8x - far shallower than a futures session profile, as a 24/7 venue
should be. Modern-era dwell on the anchor from 2019-01-01: mean gap 2.871 s,
p999 89.391 s, max 24,756 s, empty-hour fraction 0.000456, longest empty run
five hours.
