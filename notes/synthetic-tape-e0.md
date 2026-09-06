# Synthetic tape v2, experiment zero: results

Written 2026-09-06. The chart gate and the activity battery, run on the
shipped MNQ generator against the Databento corpus. Tooling is
`analysis/tape-v2` (see its README); the corpus lives on `speilegg` under
`/speilelg/databento`. Everything below was computed by that code and can
be regenerated from it.

## Owner's verdict on the first chart

Real ES and MNQ for the week of Sunday 2026-08-16 beside the shipped MNQ
generator at one-minute bars (`analysis/out/e0/compare.html`): "it looks
nothing like MNQ". Named defects, in the owner's words: volume too high
throughout the New York session, which in reality tapers to slightly above
London levels over the first four and a half hours; overnight volume
higher than normal.

## The activity envelope, real

Per-minute-of-session distributions over 260 full sessions per product
(2025-08-19 to 2026-09-03; twelve holiday and half-day sessions dropped by
a traded-minute count under 1,300). Shape is per-session normalised
volume, 1.0 being the session's mean minute, so it is independent of the
slow level. Multipliers are relative to `ny_mid` (10:30 to 13:00 Chicago).

| phase | MNQ volume | MNQ range | ES volume | ES range |
|---|---|---|---|---|
| open (17:00 to 18:00) | 0.16 | 0.54 | 0.06 | 0.45 |
| asia (18:00 to 02:00) | 0.16 | 0.51 | 0.05 | 0.40 |
| london (02:00 to 07:00) | 0.19 | 0.61 | 0.11 | 0.53 |
| ny_pre (07:00 to 08:30) | 0.43 | 0.75 | 0.28 | 0.69 |
| ny_open (08:30 to 10:30) | 2.29 | 1.71 | 1.95 | 1.50 |
| ny_mid (10:30 to 13:00) | 1.00 | 1.00 | 1.00 | 1.00 |
| ny_close (13:00 to 16:00) | 0.77 | 0.83 | 0.89 | 0.84 |

Within-phase structure, fitted as `phi_inf + a * exp(-t / tau)` on the
normalised median over the first hour after each event:

| event | MNQ | ES |
|---|---|---|
| cash open 08:30, volume | first minute 9.5x mean, peak/floor 2.0, tau 17 min | first minute 13.1x, peak/floor 3.1, tau 1.6 min |
| cash open 08:30, range | peak/floor 1.8, tau 13 min | peak/floor 1.5, tau 16 min |
| Sunday reopen 17:00, volume | peak/floor 4.7, tau 3.9 min | peak/floor 9.0, tau 1.9 min |
| daily reopen 17:00, volume | peak/floor 3.9, tau 3.1 min | peak/floor 6.4, tau 1.5 min |
| settlement 15:00 minute | 2.9x the close-phase median | 9.5x |

The ES and MNQ shapes differ, not only their levels: ES's open is sharper
and shorter, and ES's settlement spike is three times MNQ's relative to
its own close phase. The programme's "micros fit separately" rule holds
for L0 too.

## The level is a slow regime, not a constant

Per-session mean minute volume, MNQ: p10 914, p50 1,481, p90 2,068. By
month it runs from 915 (September 2025) to 2,232 (June 2026). The Asia
phase alone moves from a median of 136 contracts per minute to 876 across
the year. ES moves less (817 to 1,310 by month). This is the programme's
L1 in the volume dimension, and it is why the battery reports shape and
level separately: a pooled-year raw band is wide for the wrong reason,
and a candidate at any single level cannot sit inside it except by luck.

## The shipped generator against the envelope

Four seeds, four weeks each, 80 sessions. Ratio is candidate median over
real median per phase; inside is the share of candidate minutes within
the real p10 to p90 band at that minute of session; a matching tape
reads 1.00 and 0.80.

Level (raw minutes):

| phase | volume ratio | inside | range ratio | inside |
|---|---|---|---|---|
| open | 2.11 | 0.76 | 1.16 | 0.86 |
| asia | 2.77 | 0.57 | 1.40 | 0.84 |
| london | 1.77 | 0.78 | 1.07 | 0.85 |
| ny_pre | 1.79 | 0.50 | 1.58 | 0.66 |
| ny_open | 1.53 | 0.70 | 1.22 | 0.82 |
| ny_mid | 1.57 | 0.84 | 1.20 | 0.86 |
| ny_close | 1.62 | 0.71 | 1.20 | 0.83 |

Shape (per-session normalised):

| phase | volume ratio | inside | range ratio | inside |
|---|---|---|---|---|
| open | 1.31 | 0.89 | 0.86 | 0.73 |
| asia | 1.74 | 0.82 | 1.11 | 0.74 |
| london | 1.14 | 0.93 | 0.84 | 0.65 |
| ny_pre | 1.15 | 0.65 | 1.24 | 0.62 |
| ny_open | 0.98 | 0.81 | 0.95 | 0.72 |
| ny_mid | 1.01 | 0.98 | 0.97 | 0.79 |
| ny_close | 1.04 | 0.86 | 0.92 | 0.78 |

Reading: the shipped hourly profile gets the phase-level shape of the New
York session about right and puts too much of the session overnight
(Asia 1.74x its real share). Its level is 1.5 to 2.8 times real in every
phase, worst overnight, which is the owner's two named defects in
numbers. Within phases it has none of the structure above: the ramp fits
on the shipped tape find no burst at the cash open (first minute 3.3x the
mean against 9.5x real), none at either reopen, and a settlement minute
at 0.4x its phase median where real is 2.9x. The shipped week's minute
ranges run 1.2 to 1.6 times real by level and close to real by shape.

## Two facts about the calendar

The shipped calendar (`mogwai_lab::session` and the MNQ preset) halts
trading from 15:15 to 15:30 Chicago daily. The exchange's `status` feed
for ES on 2026-08-18 shows four transitions and no such halt: trading at
17:00, closed at 16:00, pre-open quoting at 16:45, no-cancel at 16:59:30.
Real MNQ printed in every one of those fifteen minutes on 2026-08-17,
between 129 and 1,155 contracts a minute. The generator empties 75
minutes a week that CME trades; `mogwai_lab::session` asserts the halt is
exactly fifteen minutes long.

Databento's `ohlcv-1m` emits a bar for every minute of the session on
these products: 6,900 bars per five-session week, none empty, so no
minute-fill logic is needed on the real side.

## The landing: the envelope generator, tape protocol 31

L0 landed the same day. A calendar may carry a `SessionEnvelope`: the
normalised `volume` and `range` arrays above at one-minute resolution, a
weekday weight, and the local open minute; the modulator reads it in place
of the hourly curves, deriving the per-parent volatility multiplier as
range over the square root of volume. The MNQ preset carries the fitted
arrays (`tape-v2 envelope-toml`, spliced by `splice_envelope.py`), and its
calendar no longer carves the 15:15 halt. Overriding a hourly session
curve on an enveloped bundle is refused by name. Calendar-less presets are
byte-identical.

The battery on the new tape, four seeds, 80 sessions:

| phase | level, volume ratio | shape, volume ratio | shape, inside | shape, range ratio |
|---|---|---|---|---|
| open | 1.77 | 1.12 | 0.98 | 0.97 |
| asia | 1.78 | 1.12 | 0.97 | 0.95 |
| london | 1.74 | 1.12 | 0.95 | 0.96 |
| ny_pre | 1.68 | 1.08 | 0.99 | 1.05 |
| ny_open | 1.77 | 1.14 | 1.00 | 1.18 |
| ny_mid | 1.77 | 1.14 | 1.00 | 1.10 |
| ny_close | 1.67 | 1.08 | 0.98 | 0.97 |

And the envelope fits on the new tape against real: cash-open peak/floor
2.0 with tau 16 minutes (real 2.0, 17); Sunday reopen 3.8 (real 4.7);
daily reopen 4.0 (real 3.9); settlement 2.9x (real 2.9x).

Reading: the shape is now the real shape, to within the 8 to 14 percent by
which a tape with no day-to-day regime variation has a higher normalised
median than a real one (its minutes are less skewed, so `inside` reads
above 0.80). The level is a flat 1.7 in every phase, which is the whole
remaining volume defect and is L1's by construction. The range shape is
within 5 percent overnight and 10 to 18 percent through the cash session,
where the shipped tape's minute-range envelope was already known to
overproduce extremes.

The chart gate: `analysis/out/e0/compare.html` now carries four panes,
the two real weeks, the shipped generator and the envelope generator, on
one linked axis.

## The owner's reading of the protocol 31 chart, 2026-09-06

Three defects, all in the price walk and the event clock, none reachable
through the envelope: a 400-point collapse and recovery inside three
minutes on a Wednesday afternoon (real MNQ moves 300 to 400 points in
the New York open over seven to twenty minutes, never in two); the week
travelling 2,218 points against 1,137 real with every phase after the
open trending; and overnight volume a flat wall at twice the real level
where real is quiet with bursts that taper. The chart itself had two
defects too: panes synced by bar index drifted an hour a day apart, and
ES had no business on the page. Both fixed in `compare.py`, which now
draws every pane on one minute grid with a hover readout.

## The measurements behind the second landing

Run on the real year (260 full sessions, `residuals.py` on the profile
matrix and `price_targets.py` on the bars), because the week could not
separate level from mechanism.

Volume residual (minute volume over the envelope's median), per phase:
lognormal to two decimals everywhere, log-sd 0.70 in Asia, 0.58 London,
0.45 midday, 0.36 at the open; autocorrelation in Asia 0.52, 0.39, 0.27,
0.20, 0.17, 0.09, 0.03 at lags 1, 2, 5, 10, 15, 30, 60 minutes, the same
shape in every phase. Range residual: log-sd 0.46, correlated 0.74 with
the volume residual, which is what a pure time change predicts (0.75
under the square-root law), the leftover 0.30 being the sampling spread
of a Brownian range. So activity and volatility are one multiplicative
log-Gaussian process and the minute variance follows the count. Session
level: log-sd 0.34 for volume and 0.42 for range, correlated 0.87,
day-to-day autocorrelation 0.78, 0.66, 0.55, 0.48, 0.28 at lags 1, 2, 5,
10, 20 sessions.

Price: one-minute returns uncorrelated at every lag in every phase,
variance ratios 0.9 to 1.1 out to the session, so the close is a
martingale with no drift and no bounce regime at minute scale. Minute sd
5.9 Asia, 6.8 London, 8.4 pre, 17.1 open, 10.7 midday, 9.2 close.
Standardised minute kurtosis 9 to 10 in the busy phases, far more
overnight from news. Per session, the largest move over 2, 5, 10, 20, 30
and 60 minutes has medians 80, 108, 139, 175, 188 and 221 points and
ninetieth percentiles 145, 183, 228, 303, 331 and 399, starting in the
two hours around the cash open three times in four; the largest single
minute is 82 at the median and 253 at p99. Session range 445 at the
median, week 1,064. Reopen gap 9.6 points at the median, 63 at p90, 316
at p99, heavy-tailed.

This is a different model from the Hawkes engine the programme named for
L2, and the owner delegated the choice. A Hawkes count is not lognormal
and its near-critical tail is heavier than the real p99 of 4.8 times the
median; the multi-scale log-Gaussian family, which is the programme's L1
(MSM) extended down to seconds, matches the marginal, the two-scale
autocorrelation and the level in one construction, and the v1 objection
to log-OU arrivals was to a single-scale fit at the second level with a
sigma several times this one. The decision and its evidence are recorded
in `notes/synthetic-tape-tree-context.md`.

## The second landing: the activity cascade, tape protocol 32

A second walk inside `GeneratedSource`, selected by
`[instrument.generator.cascade]`, mechanism stated in
`mogwai_data::generated::cascade`. Prototyped in Python at one-second
resolution (`proto_engine.py`) against the numbers above, then
transcribed. The MNQ preset carries it with its parent gap refit to 747 a
minute (the year's median session at 1.98 contracts a parent), and the
crypto lineage is untouched.

The Rust tape against the real year, four seeds of four weeks (80
sessions) unless stated:

| statistic | real | cascade |
|---|---|---|
| Asia volume residual log-sd | 0.69 | 0.67 |
| Asia residual acf, lags 1, 5, 15, 30, 60 | 0.52, 0.27, 0.17, 0.09, 0.03 | 0.63, 0.37, 0.22, 0.14, 0.04 |
| whole-session volume acf, lags 60, 120, 240 | 0.18, 0.11, 0.03 | 0.16, 0.09, -0.01 |
| range residual log-sd, corr with volume | 0.46, 0.74 | 0.46, 0.67 |
| session level p10, p50, p90 | 914, 1,478, 2,069 | 1,104, 1,458, 2,043 |
| minute sd Asia, open, midday | 5.9, 17.1, 10.7 | 5.5, 17.6, 9.6 |
| variance ratio, 60 min, Asia and open | 1.02, 0.96 | 1.03, 1.10 |
| largest 2-min move per session p50, p90, p99 | 80, 145, 240 | 76, 146, 198 |
| largest 20-min move per session p50, p90, p99 | 175, 303, 504 | 157, 282, 441 |
| largest minute per session p50, p90, p99 | 82, 158, 253 | 78, 133, 214 |
| largest Asia minute in 80 sessions, and 258 real | 140 | 98 |
| minutes over 60 points, volume over the phase median, p50 and p90 | 2.0x, 5.6x | 1.8x, 3.2x |
| range efficiency (range over realised vol), median | 1.55 | 1.53 |
| session range median, week range median | 445, 1,064 | 432, 982 |
| reopen gap p50, p90, p99 | 9.6, 63, 316 | 7.6, 66, 313 |

The owner's second reading found two things in the first cascade chart.
A 244-point drop inside one Asia minute on ordinary volume: real Asia's
largest minute in the year is 140 and its four minutes over 100 all came
with two to ten times the phase's median volume. So a jump is now sized
against the square root of the minute sd where it lands, and it kicks the
two fastest texture components so the minutes after it print a burst.
And the overnight volume read flat: the whole-session autocorrelation at
one and two hours was 0.07 and 0.04 against 0.18 and 0.11 real, a missing
three-hour swell, now a sixth texture component. Both rows above are
after those changes. The regenerated chart week then drew a 1,300-point
reopen gap, the 3.2-sigma tail of the lognormal gap law where the real
year's largest gap, 541 points, is the 2.5-sigma point; the draw is now
clamped at 2.75 sigma, a declared knob, so the law's tail past what a
year shows is not printed. The chart week still carries a gap near that
clamp, about 600 points into Tuesday, which is a once-a-year event
landing in the one week on the page.

The owner's third reading found two jumps in consecutive minutes at a
cash open, 340 points in two minutes. The adjacency was a feedback loop:
a jump kicked the volume texture and the jump rate followed the parent
rate, so one jump nearly doubled the chance of the next. The jump rate
now follows the envelope and the slow level only, and the jump draw is
clamped at 2.75 sigma like the gap, since the real open's largest minute
of 197 points is the 2.6-sigma point of the fitted law. Lesson: a rate
that follows a state the event itself excites is self-exciting whether or
not it was meant to be.

The owner's reading of the open's first fifteen minutes as thin did not
survive the numbers: the generator's first open minute is 6.05 times the
session's median minute range against 6.2 real, decaying with a time
constant of 10 minutes against 13, and the charted morning's minute
ranges were larger than the real morning's. Direction was the difference:
real ran 150 points in fifteen minutes, the seed chopped in a band. Both
are martingales; which morning trends is the draw.

The owner's verdict on the fourth chart, 2026-09-06: "near something
actually resembling MNQ". The page carried real MNQ over two seeds of the
same week.

One seed of 52 weeks (260 sessions, before the jump and swell changes)
had the variance ratio at 1.00 out to 690 minutes and 0.90 at the
session, and range efficiency 1.45. The shape battery sits inside the
band in every phase. The cash-open ramp fits at peak over floor 2.1 and
tau 15 minutes against 2.0 and 17, the reopens at 3.4 and 3.7 against
4.7 and 3.9, the settlement minute at 2.6x against 2.9x.

What reads light: the largest 20-minute move per session, about a tenth
under real at every quantile, and the day-to-day autocorrelation of the
level, which four weeks per seed cannot show. What is declared rather
than fitted: the aggressor side persistence and everything below a
second, which no bar can see. The chart gate:
`analysis/out/e0/compare.html`, real MNQ over the cascade week, one
shared minute grid.

## What this decides

- L0 is built first, as the programme orders, and it is built as a
  per-minute-of-session shape, not as twenty-four hourly multipliers.
  Landed at protocol 31 (above), provenance `fitted`.
- Level and texture are one multi-scale log-Gaussian cascade, and the
  price variance follows the count. Landed at protocol 32 (above). The
  programme's L1 and L2 are one layer on this instrument; the Hawkes
  engine is not built and, on this evidence, is not owed.
- The 15:15 halt is out of the venue's calendar. The lab's session frame
  still carves it; that is filed in the todo against the closed-arc
  ruling.
- The battery runs on every candidate from here, shape and level, before
  a chart is rendered for the owner; `price_targets.py`, `residuals.py`
  and `vr_long.py` are the price and texture halves of it.

## Process lessons from this pass, for the record

- A share-of-total profile matched within a few percent while the level
  was off by a third. Compare absolute quantities first; normalise only
  when the normalisation is the claim.
- `fork()` after polars has started its thread pool deadlocks every
  worker silently, with zero CPU and no error. Spawn.
- `dt.hour()` is an 8-bit integer in polars; hour times 60 overflows it
  without a warning.
- Several seeds share (session, minute) keys; a matrix needs a tape
  column or their minutes merge. The same trap one level up: several
  seeds share dates, so a week or a gap keyed on the date alone pools
  four price levels into one and reports 2,700-point weeks and
  2,000-point gaps that nothing produced.
- A session's summed return is invariant under shuffling its minutes, so
  a shuffle test cannot see long-horizon dependence; the pooled
  autocorrelation out to the horizon, or the per-session ratio of the
  squared session move to the summed squared minutes, can.
- A prototype at 40 sessions cannot resolve a range-efficiency
  difference of 0.2; the Rust engine gives 260 sessions in seconds, so
  a question about a session-scale statistic is asked of the engine, not
  the prototype.
- Tail quantiles at 80 sessions rest on a handful of events: the open's
  largest minute moved from 219 to 302 between two runs that differed
  only in their draws. A tail claim needs the 52-week seed.
- The page must draw a closure as blank space. With traded minutes only
  on the grid, a reopen gap read as a jump between neighbouring bars and
  the hour that passed was invisible; two of the owner's questions were
  that.
- The eye judges the price line on the pane's own scale, and a pane
  whose week travelled 30 percent further draws every bar 30 percent
  shorter. When the eye and the minute numbers disagree, check the week
  range in the label first.
