# The ordered-count measurement: a preregistration

FROZEN 2026-08-11, signed by codex session
019fefe4-b680-7e70-8a8e-9df36e0beecf after four rounds and eleven
closures - the identification limit, the impossible populations, the
Panel A estimators, the executable extrapolation, the Panel B
statistics, the bootstrap exclusion for chronological statistics, the
mechanical outcomes, the zero lag, the per-session hour-mean rule, the
conditional refit, and the splice discontinuity. Every one was a place
an implementer could have chosen differently.

`notes/`-class: transient, no truth guarantee, nothing durable cites it.

## Why this exists

The count-curve measurement (`notes/count-curve-preregistration.md`,
signed and run) established two things and left one unresolved.

ESTABLISHED. The incumbent's within-session Fano is FLAT - 31 to 37
across 1, 5, 15, 60 and 300 s at every hour - while observed
within-session Fano compounds by 9 to 30 times. The incumbent
over-clusters at one second and under-clusters at 300 s, and the
crossover survives removing session heterogeneity.

ESTABLISHED. The incumbent's between-session Fano is 25 to 50 times
short of observed, and at hour 19 at 300 s that component is 75 percent
of the observed total.

UNRESOLVED, and this document exists for it. `F_between` is the variance
of ESTIMATED session means, so it mixes genuine session-to-session
heterogeneity with the uncertainty of each session-hour mean - and that
uncertainty carries the integrated covariance over the hour, the very
quantity under study. The confound the decomposition removed at window
scale recurs one level up.

A FIRST ATTEMPT AT THE SAMPLING FLOOR WAS INVALID AND IS RECORDED AS
SUCH. It used the ONE-SECOND Fano, which assumes the correlation dies
inside a second - the assumption the count curve refuted - and put the
sampling share at 0.4 percent. A 300 s plug-in raises it materially, to
19.1 percent pooled, under a monotone-covariance and small-relative-error
approximation. That is not a bound: `Fano_within(300)` bounds the
hour-scale value only if the integrated covariance does not decline
between 300 and 3,600 s, which the observed rise supports but does not
prove; and it compares a coefficient of variation against a log-rate
standard deviation through a delta-method step. Hour 13 exceeds 100
percent, which demonstrates non-identification or approximation error
rather than showing its genuine variation is zero.

WHAT ORDERED COUNTS CAN AND CANNOT DO, stated correctly because an
earlier draft of this document overclaimed it. DIRECT SUB-HOUR
COVARIANCE ESTIMATION REQUIRES ORDERED COUNTS. SEPARATION OF HOUR-SCALE
LATENT PERSISTENCE FROM A SESSION EFFECT REMAINS MODEL-DEPENDENT AT THE
ONE-HOUR LIMIT: a latent state nearly constant over an hour is
observationally equivalent, within that hour, to a session intercept.
No amount of within-hour data crosses that boundary. What the sequence
buys is direct sub-hour measurement and better bounds; what Panel B
buys is cross-hour and cross-session evidence that bears on the same
question from outside the hour.

## Binding

```text
corpus job          GLBX-20260805-HAPEWPABKG
corpus files        glbx-mdp3-20260630.tbbo.csv.zst
                      18c218aa2dc4df44678222912d4811d5aeee4fd29df40577c0e432fc8a02dc01
                    glbx-mdp3-20260701-20260731.tbbo.csv.zst
                      b5ede9b3c0a544367196cad91c60bd35ec01a11f8e536a6a04525aa34fc0d6fd
population          the 22 usable sessions of analysis/mnq-measure-12a.json
parent inference    the 12a contract, subcontract hash
                      1ca79d9cd043e7ce4b8b633fdbcdf0547a02a26570ea9120eb0141254a8ad954
scheduling          12a section 3.2, unchanged
sequence artifact   analysis/out/ordered-counts.jsonl
summary artifact    analysis/out/ordered-counts-panels.json
```

The summary artifact carries the CONTENT HASH of the sequence artifact,
so a panel result can never be read against a sequence it was not
computed from.

## The extraction

ONE pass producing an IMMUTABLE ORDERED one-second parent-count
sequence. Record schema, frozen:

```text
session_date     YYYY-MM-DD
segment_index    integer, the open segment within the session
window_start_ns  half-open window start
window_end_ns    half-open window end
endpoint_hour    0..23, the 12a attribution
parent_count     u32
```

POPULATION: the sequence contains EVERY SCHEDULED one-second window of
every open segment of the 22 sessions, and nothing else. Unscheduled grid
seconds are not emitted, so `scheduled` is constant and is dropped from
the record - a field that is always true is a field an implementer can
misread. Hour-crossing exclusion does not apply at this resolution; it
applies when windows are aggregated.

CANONICAL ORDERING is `(session_date, segment_index, window_start_ns)`
ascending, which is total. The CONTENT HASH is SHA-256 over the file
bytes in that order. The sequence is RETAINED as the measurement's
sufficient evidence and never discarded after summaries; a later
statistic must be computable from it without another corpus pass.

THE RECONSTRUCTION BACKCHECK. The committed 12a Block 2 records at 1, 5
and 60 s must be reproducible from the sequence EXACTLY, on
`scheduled_windows`, `zero_windows` and `count_hist`. The sequence may be
staged internally to run it - the backcheck necessarily derives from the
sequence - but NO derived panel statistic is exposed or inspected until
it passes. A mismatch stops the measurement.

## Frozen grids

```text
Panel A lags     1, 2, 5, 10, 30, 60, 120, 300, 600, 1200, 1800 seconds
Panel A blocks   1, 5, 15, 60, 300, 900, 1800 seconds
```

The 3,600 s lag and the 3,600 s block are REMOVED from Panel A rather
than left to an implementer: a within-hour lag of 3,600 s has no pair
under the hour-crossing exclusion, and a 3,600 s block cannot exist under
that exclusion at all, still less at partial hour 20. Cross-hour
structure is Panel B's population, not Panel A's.

## Panel A: how uncertainty accumulates toward one hour

REPORTED IN RATE SPACE throughout, before any log transform, because the
delta-method step is one of the two approximations that made the first
attempt unreliable.

```text
centering        session-hour mean, not the hour-level mean across
                 sessions: Panel A measures WITHIN-session structure
pairs            both members in the same session, same segment and
                 same endpoint hour; a pair crossing a segment or UTC
                 hour boundary is INELIGIBLE
weighting        each eligible pair counts once, unweighted; sessions
                 with more eligible pairs therefore contribute more,
                 and the per-hour pair count is reported beside every
                 estimate
denominator      population, matching 12a
blocks           aligned to the segment origin; an INCOMPLETE block at a
                 segment or hour boundary is DISCARDED, never padded
```

ESTIMATORS, written out. With `x_sht` the count in window `t` of hour
`h` in session `s`, and `P_h(k)` the eligible ordered pairs at lag `k`:

```text
r_sht      = x_sht - mean over t of x_sh.         (session-hour centering)
gamma_h(0) = (1 / N_h) * sum over all s, t of r_sht^2
gamma_h(k) = (1 / |P_h(k)|) * sum over P_h(k) of r_sht * r_sh,t+k
G_h(k)     = (1 / |P_h(k)|) * sum over P_h(k) of (r_sh,t+k - r_sht)^2
```

`gamma_h(0)` is defined on the FULL population `N_h`, not on a paired
population, and interpolation NEVER supplies it - interpolation applies
to integer lags `k >= 1` only.

THE HOUR-MEAN VARIANCE, frozen. For contiguous runs `r` of lengths
`L_r` within one `(session, segment, hour)`, with `N = sum L_r`:

```text
per session s and hour h, over that session-hour's contiguous runs r
of lengths L_r with N_sh = sum L_r:

  v_sh = ( sum over r of [ L_r * gamma_h(0)
           + 2 * sum over k = 1..L_r-1 of (L_r - k) * gammatilde_h(k) ] )
         / N_sh^2

then over the S_h eligible sessions:

  v_h  = (1 / S_h) * sum over s of v_sh
```

where `gammatilde_h(k)` is the scenario-supplied covariance below.

RUN LENGTHS AND `N` ARE NEVER COMBINED ACROSS SESSIONS before the
finite-window formula is applied. Pooling them would estimate the
variance of a 22-session GRAND mean, which is a different quantity from
the one this panel needs - the uncertainty of a single session-hour
mean, which is what `F_between` confounds. Bootstrap replicates repeat
this rule under their own frozen session multiplicities.

This form also makes explicit that covariance ACROSS SEPARATE SEGMENTS
is treated as ZERO under Panel A - including across hour 20's halt,
which is the one place that assumption is doing visible work.

THE VARIOGRAM IS COMPUTED FROM PAIRED DIFFERENCES DIRECTLY, not as
`2 * (gamma_0 - gamma_k)`: that identity holds only when `gamma_0` is
taken on the same paired endpoint population, and edge effects break it
otherwise.

Block-mean variance, for block length `B`, over blocks `j` lying wholly
inside one `(session, segment, hour)`:

```text
m_shj  = (1 / B) * sum of x over block j
VB_h(B) = (1 / |J_h(B)|) * sum over s, j of (m_shj - mean over t of x_sh.)^2
```

pooled across sessions with the population denominator, and `|J_h(B)|`
reported beside it.

THE HOUR-MEAN EXTRAPOLATION CANNOT BE EXACT. Three
ASSUMPTION-CONDITIONED ESTIMATES are reported side by side. They are NOT
a bracket and NOT a confidence bound - no ordering among them is proved -
and their minimum and maximum are reported only as a SCENARIO ENVELOPE:

```text
truncated  sum the interpolated gamma to 1,800 s, zero beyond
held       as truncated, plus gamma_h(1800) held constant to 3,600 s
fitted     fit gamma_h(k) = A * exp(-k / tau) and integrate to 3,600 s
```

INTERPOLATION between measured lags, needed because only eleven lags are
measured and the sum runs over every integer lag: LINEAR IN `k` between
adjacent measured lags, for integer `k >= 1` only. `gamma_h(0)` is
measured directly and is never interpolated or substituted.

THE THREE SCENARIOS DIFFER ONLY BEYOND 1,800 s. For `k <= 1800` all
three use the measured-and-interpolated `gamma_h(k)`; the fitted
scenario does NOT overwrite the measured range with its fit. Beyond
1,800 s: `truncated` supplies zero, `held` supplies `gamma_h(1800)`,
`fitted` supplies `A * exp(-k / tau)`.

THE FIT, fully specified: fitting lags are the tail
`{60, 120, 300, 600, 1200, 1800}`; the model is `A * exp(-k / tau)`;
because `log gamma = log A - k / tau` is linear, the objective is
ORDINARY LEAST SQUARES of `log gamma_h(k)` on `k`, unweighted, over
those fitting lags with `gamma_h(k) > 0`; bounds are `A > 0` and
`tau` in `[1, 86400]` seconds. A fitted `tau` outside that range is
clamped to the bound and `A` is refitted as the CONDITIONAL OLS OPTIMUM
at that `tau`:

```text
log A = mean over fitting lags i of ( log gamma_h(k_i) + k_i / tau_clamped )
```

The refit is the conditional optimum and NOTHING MORE - it does not
make the tail continuous with the measured range, and an earlier draft
of this document wrongly claimed it did. THE SPLICE DISCONTINUITY IS
THEREFORE RECORDED: the fitted value immediately beyond 1,800 s against
the measured `gamma_h(1800)`, reported per hour, so a reader can see how
far the extrapolated tail departs from the last measured point. The
clamp itself is recorded when it binds. If fewer than three
fitting lags carry a positive `gamma_h(k)`, or the fitted slope is
non-negative (a non-decaying tail), the fitted estimate is NULL with
the reason - never silently replaced by one of the other two.

REFUSALS. An hour with fewer than 30 eligible pairs at a lag reports
that lag as null with the pair count, never as zero. A negative
`gamma_h(k)` is reported as measured, never clamped.

## Panel B: the geometry of the residual

```text
residual       R_sh = log( rate_sh / geometric mean over s of rate_.h )
               LOG-RATE, stated explicitly because Panel A is
               rate-space and the two must not be confused
blocks         Asia 0..6, London 7..12, cash 13..19, hour 20 alone,
               post-close 22..23. A block residual is the UNWEIGHTED
               MEAN of its member hours' R_sh for that session.
               Reported: the 5 by 5 Pearson correlation matrix of block
               residuals across the 22 sessions, and each block's
               variance share as Var(block_b) divided by the sum over
               b of Var(block_b). No contrast beyond that matrix is
               computed.
matrix         PEARSON CORRELATION of the 23 hour residuals, not
               covariance, so hours of unequal variance do not dominate
               the factor
```

CONSECUTIVE-SESSION DEPENDENCE, per hour and pooled, over pairs of
sessions whose ACTUAL elapsed calendar gap falls in bin `g`, with bins
`{1, 2, 3, 4 or more}` days:

```text
C_h(g) = (1 / |Q_h(g)|) * sum over (s, s') in Q_h(g) of R_sh * R_s'h
```

`|Q_h(g)|` is reported beside every value. MINIMUM SUPPORT is 8 pairs;
below it the bin is NULL with its pair count, never zero. With 22
sessions there are only 21 adjacent pairs in total, so several bins are
expected to refuse and that is a reported outcome rather than a fault.

THE POOLED STATISTIC is the UNWEIGHTED MEAN of the available `C_h(g)`
over hours that did not refuse, with the contributing hour count
reported - unweighted so that busy hours do not dominate a statistic
whose subject is session-level structure.

`C_h(g)` AND THE POOLED STATISTIC GET THEIR OWN NULL: the same 2,000
hourwise permutations below recompute them, giving each its own p-value
by the same convention. The leading-eigenvalue permutation is not
automatically a null for a covariance statistic, and reusing it as one
would be an error.

PERMUTATION NULL for the leading factor:

```text
replicates     2,000
shuffle        Fisher-Yates in original order, exactly the 12a 5.1
               algorithm INCLUDING its state advancement:
                 for i from n - 1 down to 1:
                     state = splitmix64(state)
                     j = state mod (i + 1)
                     swap(values[i], values[j])
seeding        state = tuple_mix(ORDERED_COUNTS_PERM_SEED, [hour,
               replicate_index]) per the 12a 3.4a convention, so each
               HOUR is permuted INDEPENDENTLY - which destroys the
               common mode while preserving every hour marginal
ORDERED_COUNTS_PERM_SEED = 8934572019384756123
statistic      the leading eigenvalue of the correlation matrix
p-value        (1 + count of null values >= observed) / (1 + 2000),
               the finite-sample convention; EQUALITY COUNTS TOWARD THE
               NULL. This is the only inferential summary reported; no
               percentile is quoted alongside it, since the p-value is
               already exact and a second summary invites reading
               whichever is kinder.
```

SIGNS are aligned so the majority loading is positive BEFORE any
leave-one-out comparison. STABILITY is leave-one-session-out, 22 refits,
reporting the leading variance share and the count of sign flips against
the full-sample loading. It is DETERMINISTIC and carries no bootstrap.

HOUR 20 IS ITS OWN BLOCK, which is the only way it can be both reported
separately and used in the block contrasts. The earlier draft put it
inside a `13..20` cash block while claiming it was separate throughout;
that was a contradiction and is resolved by making cash `13..19`.

## Uncertainty and stopping

Panel A estimates, and the Panel B block matrix and factor statistics,
carry session-level uncertainty from the SAME bootstrap the earlier
preregistration froze: the first 2,000 replicates of the 12a section 6.1
circular five-session block resample, its ordering, `splitmix64` seed
derivation, wrapping and 22-session pseudo-month unchanged. Reported as
the point estimate, the standard error as the sample standard deviation
of the replicate estimates, and nearest-rank 2.5 and 97.5 percentiles.

BOOTSTRAP FAILURES ARE NEVER SILENTLY DROPPED. A replicate can fail
legitimately - a null correlation, support below a frozen minimum, or a
refused exponential fit. If ANY of the 2,000 replicates fails for a
statistic, that statistic's uncertainty is reported as NULL together
with the COUNT OF FINITE REPLICATES and the failure reason. Uncertainty
is never computed over the surviving subset, because a bootstrap over
the replicates that happened to succeed answers a question nobody asked.

THE CONSECUTIVE-SESSION COVARIANCE TAKES NO BOOTSTRAP, and the reason is
structural rather than a preference: the circular block resample joins
sampled blocks end to end and MANUFACTURES ADJACENCY that the calendar
does not contain, so applying it to a statistic defined by actual
elapsed gap would measure the resampler. It is reported as a point
estimate with its pair count, and its only null is the permutation
above. Leave-one-session-out stability is likewise DETERMINISTIC.

THE PANELS ARE INDEPENDENT. Failure or refusal in one may NOT be
repaired using the other, and each reports its own outcome.

MECHANICAL OUTCOMES, one per panel, chosen by the implementation from a
closed set - "ambiguity" may be a word in the eventual reading, never a
status the code selects:

```text
completed             every frozen statistic produced or explicitly
                      refused under a stated rule
backcheck_mismatch    the 1, 5, 60 s reconstruction failed; no panel
                      statistic is exposed
insufficient_support  a frozen statistic could not be computed and no
                      refusal rule covers the case
```

### Amendment, 2026-08-11: the structural-inapplicability refusal

Signed by codex session 019fefe4-b680-7e70-8a8e-9df36e0beecf.

The first run returned `insufficient_support` for Panel A, mechanically
correctly under the text as signed: hour 20 has ZERO eligible aligned
1,800 s blocks, and no refusal rule covered that. The cause is
deterministic schedule geometry - hour 20 is the partial-session hour
AND contains the halt, so it has no contiguous run that long - and the
outcome overstated the loss, since 22 of 23 hours computed.

```text
If the frozen schedule produces ZERO eligible complete blocks for a
(hour, B) pair, the block statistic is null, count 0, reason
structurally_inapplicable. Every downstream statistic requiring a
structurally null input is null with the reason PROPAGATED. A
structural null is an explicit refusal covered by `completed`; it is
NOT `insufficient_support`.
```

The existing fewer-than-30-pairs rule already covers the zero-pair lag
and does not change. NO grid, estimate, tolerance or computed value
changes: only the summary is regenerated FROM THE RETAINED SEQUENCE,
with no corpus rerun - which is what retaining the sequence was for.

The original `insufficient_support` outcome is preserved here in
amendment history rather than erased; the amended artifact records
`completed` with hour 20's 1,800 s block and its dependents structurally
null.

NO SUCCESSOR PARAMETERIZATION until both panels are read. The
slow-or-session component is NOT frozen as a scalar day factor here. The
current defensible statement, which this measurement exists to improve:
a same-sign session-wide mode explains about 28 to 30 percent of the
sample residual structure, and the division of the remainder among
correlated sampling variation, time-of-day interaction and longer-lived
latent state is UNRESOLVED.

ANY LATER LAG GRID OR STATISTIC requires a new dated preregistration.
The retained sequence makes that cheap; it does not make it free of
preregistration.

## How the results must and must not be read

Recorded with the measurement because two over-readings were made and
corrected on the day, and the corrections are easy to lose.

THE FITTED TAUS - 278 to 3,277 s across the hours that computed, median
near 1,000 s - are ASSUMPTION-CONDITIONED single-exponential tail times.
They do NOT prove the observed law has one correlation time. What they
do establish, decisively, is that the missing covariance lives in the
MINUTES-TO-HOUR range rather than the incumbent's sub-second range.

HOUR 20's 39.9 PERCENT block-variance share does NOT mean one hour
generates 39.9 percent of all session variation. Hour 20 is a
SINGLE-HOUR residual while Asia, London, cash and post-close are
AVERAGES over several hours, and averaging reduces variance - they are
different filters. The statistic says hour 20 varies much more than the
other block averages, and nothing stronger.

PANEL B ESTABLISHES TWO SEPARATE FACTS, not one: a stable common mode
exceeds the hourwise permutation null, and the residual field has
positive one-day dependence. It does NOT establish that the leading
common-factor SCORE is what persists across days. Claiming the common
factor is autocorrelated would need a preregistered lag statistic on its
scores, which this measurement does not contain.

HOUR 20 IS A REAL STRATUM because it is structurally different, and its
recurrence across the 12b close, the A3 incompatibility and this
measurement is not pattern-matching - but neither does recurrence prove
one shared mechanism defect. Three effects are mixed in it:
deterministic support geometry from the halt and partial exposure,
greater estimation uncertainty from fewer scheduled seconds, and
possibly genuine pre-halt, post-halt, settlement or close
microstructure. The durable reading:

> The partial-session, halt-containing stratum repeatedly exposes
> failures in support, conditioning and observed behavior, so successors
> must preserve it explicitly. The relevant structure is CALENDAR PHASE
> AND SEGMENT POSITION, not a special free parameter for UTC hour 20.

## What this feeds

The successor contract, when drafted, carries four arms reported
separately - incumbent, within-session repair only, slow/session
component only, and joint - so neither component can hide a defect in
the other. Two requirements are already established and are recorded
here so the measurement serves them: reduce the incumbent's excessive
one-second clustering, and add covariance at minute through
five-minute scales. Those are a REDISTRIBUTION of clustering across
scales rather than an added slow component; adding slow variation on top
of the incumbent would preserve the one-second excess and likely worsen
total dispersion.

## Cost

One corpus pass over 873 MB on host `bygg`; the 12a record prices the
observed pass at about 334 s. The ordered sequence is roughly 1.8 million
scheduled seconds, a small artifact beside the screen outputs.
