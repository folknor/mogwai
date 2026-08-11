# The slow-geometry measurement: a preregistration

FROZEN 2026-08-11, signed by codex session
019fefe4-b680-7e70-8a8e-9df36e0beecf after five rounds. The largest
change those rounds forced is recorded in the opening: this document set
out to SELECT a slow-component architecture and cannot, because the
permutation it proposed as a null for the boundary contrast was not one
- mechanically, since it moves scores while the contrast is computed
from `R_sh`, and inferentially, since destroying session ordering is no
null for "no boundary discontinuity". Without it, the measurement was
rewritten to do the smaller thing it can.

`notes/`-class: transient, no truth guarantee, nothing durable cites it.

## Measuring the slow component's geometry, for three classes that all advance

The within-session target geometry is known well enough to build
against: the incumbent's clustering sits sub-second where the observed
covariance lives in the minutes-to-hour range, and the successor's
within-session job is to REDISTRIBUTE it.

THE SLOW COMPONENT'S ARCHITECTURE IS NOT KNOWN, and three classes
remain compatible with everything measured:

```text
A-only     a CONTINUOUS SLOW STATE persisting through session
           boundaries
B-only     a BOUNDARY-ASSOCIATED SESSION FACTOR, possibly
           autocorrelated across days
A+B mixed  both
```

THIS DOCUMENT DOES NOT CHOOSE BETWEEN THEM. Its first draft tried to,
and the attempt failed on a defect worth recording: the permutation it
proposed as a null for the boundary contrast was not a null for it at
all, and no valid substitute could be frozen from what is known. All
three classes therefore advance, and what this measurement supplies is
their PARAMETER GEOMETRY.

Choosing between architectures on taste would repeat the central mistake
of protocol 12b - freezing a search space before the target geometry was
known - with better measurement discipline wrapped around the same
error. Inventing a null to permit that choice would be the same mistake
wearing a p-value.

No corpus pass. This is a new reduction of retained evidence, which is
why it needs its own dated preregistration.

## Binding

```text
sequence   analysis/out/ordered-counts.jsonl
           SHA-256 33aaf2c11d70a68c2ef91da88b69ad01dc9dd8ec1f3cbb0f9324593707e68a70
output     analysis/out/slow-geometry.json
```

THE RESIDUAL MATRIX IS CONSTRUCTED HERE FROM THE SEQUENCE, not consumed
from another note, so no implementation-specific matrix can be silently
inherited:

```text
parents_sh   sum of parent_count over the session-hour's windows
exposure_sh  count of those windows, in seconds
rate_sh      parents_sh / exposure_sh
L_sh         log(rate_sh)
R_sh         L_sh - (1 / S_h) * sum over s of L_sh
             i.e. centered on the per-hour mean of logs, which is the
             log of the geometric mean
```

A session-hour with `parents_sh = 0`, `exposure_sh = 0` or a missing
cell is EXCLUDED with its reason recorded, and every statistic reports
the cells it used. Sessions are ordered by calendar date ascending; the
actual dates are retained because elapsed separation depends on them.

## Statistic 1: does the COMMON MODE itself persist?

Established already: a stable common mode exists, and the residual field
has one-day dependence. NOT established: that the common mode's own
score is what persists. That needs a lag statistic on the scores.

CROSS-FITTED, so no session contributes to the direction it is scored
on. For each held-out session `s`:

```text
1  over the OTHER 21 sessions, per hour h, compute the training mean
   mu_h^(-s) and standard deviation sigma_h^(-s) of R, POPULATION
   denominator, matching 12a
2  z_sh = ( R_sh - mu_h^(-s) ) / sigma_h^(-s)
3  V^(-s) = leading eigenvector of the Pearson correlation matrix of R
   over those 21 sessions, normalized to UNIT EUCLIDEAN LENGTH
4  sign-align V^(-s) so the sum of its loadings is positive; on an
   exact tie, align so the loading of the LOWEST-NUMBERED hour is
   positive
5  f_s = dot( z_s , V^(-s) )
```

REFUSALS: a training hour with zero variance, a failed eigensolve, or a
session with any excluded cell refuses THAT SESSION's score, recorded
with the reason; the statistic continues over the sessions that scored.

SCORES ARE CENTERED before any autocovariance - `f_s * f_s'` is not an
autocovariance otherwise:

```text
fbar   = mean of f over the scored sessions
S(g)   = ( 1 / |G(g)| ) * sum over pairs at gap g of
         (f_s - fbar) * (f_s' - fbar)
```

Gap bins `{1, 2, 3, 4 or more}` calendar days, `|G(g)|` reported beside
every value, MINIMUM SUPPORT 8 pairs, below which the bin is null with
its count. With 22 sessions there are only 21 adjacent pairs, so
refusals are expected and are a result.

## Statistic 2: covariance by ELAPSED SEPARATION, boundaries marked

The discriminating geometry. A continuous slow state decays in elapsed
time and does not care about a session boundary; a boundary-associated
factor produces a within-session shift and a DISCONTINUITY at it.

```text
cell timestamp  the SCHEDULED-EXPOSURE-WEIGHTED MIDPOINT of the
                session-hour's windows - not the nominal hour start,
                because hour 20 is partial and a nominal timestamp
                would misplace it
pairs           UNORDERED, each unordered pair counted once, endpoints
                ordered by (session_date, hour) for reproducibility
separation      absolute difference of the two timestamps, in hours
class           WITHIN if both cells share a session, else CROSS
statistic       C(bin, class) = mean over that bin and class of
                R_sh * R_s'h'  (R is already per-hour centered)
```

BIN EDGES, half-open and exhaustive, in hours:

```text
[1,2) [2,3) [3,6) [6,12) [12,24) [24,48) [48,72) [72,96) [96,inf)
```

MINIMUM SUPPORT 8 pairs per `(bin, class)`, else null with the count.

HOUR 20 PAIR STATISTICS ARE THEIR OWN STRATUM and never enter the
ordinary pool. Every bin and class is reported twice: the ORDINARY
stratum over pairs with NO hour-20 endpoint, and the hour-20 stratum
over pairs with AT LEAST ONE hour-20 endpoint - which is stated that way
because a cross-session pair can have BOTH endpoints at hour 20, and
"pairs involving one" would leave that case undefined.

THE "NEVER POOLED" CLAIM IS NARROWED TO WHAT IS TRUE. Statistic 1
NECESSARILY combines hour 20 with the other hour coordinates, because a
factor projection is a dot product over all 23 of them. The accurate
statement is that hour 20 remains an EXPLICIT FACTOR COORDINATE, whose
loading is reported alongside the rest, and that its PAIR statistics are
never pooled with ordinary pairs. An earlier draft claimed the stronger
thing and was wrong.

## Statistic 3: what survives removing the common mode?

Subtraction happens in STANDARDIZED coordinates, because the loading
lives there and `R_sh - f_s * V_h` would be dimensionally wrong:

```text
z_star_sh = z_sh - f_s * V_h^(-s)
```

ONLY THE ELAPSED-SEPARATION COVARIANCE IS RECOMPUTED:

```text
C_star(bin, class) = mean over that bin and class of
                     z_star_sh * z_star_s'h'
```

STATISTIC 1 IS NOT REPEATED after residualization, and an earlier draft
that said it was carried a type error: `S(g)` is defined on SCALAR
session scores while `z_star` is an hour VECTOR, so "`S(g)` with
`z_star` in place of the original inputs" is undefined. Producing a
residualized scalar score would require fitting a second cross-fitted
factor, which this document explicitly excludes.

UNITS DIFFER AND ARE NOT COMPARABLE: `C` is in log-rate units,
`C_star` in standardized units. They are never subtracted from or
divided by one another, and the artifact labels each.

## Permutation and multiplicity

```text
what moves      the cross-fitted SCORES are permuted among the FIXED
                session dates; the calendar-gap and elapsed-separation
                pair sets stay fixed
replicates      2,000
shuffle         Fisher-Yates with the 12a 5.1 state advancement:
                  for i from n-1 down to 1:
                      state = splitmix64(state)
                      j = state mod (i + 1)
                      swap(values[i], values[j])
seeding         tuple_mix(SLOW_GEOMETRY_PERM_SEED, [replicate_index])
                SLOW_GEOMETRY_PERM_SEED = 5177340928461523719
sharing         ONE permutation per replicate, SHARED across every bin,
                so the bins remain jointly comparable
```

WHAT THIS PERMUTATION IS A NULL FOR, and it is ONLY this: `S(g)`, the
persistence of the cross-fitted common-mode SCORE across calendar gaps.
Multiplicity across the gap bins is handled by a MAX STATISTIC - the
maximum absolute `S(g)` across supported bins against the distribution
of that same maximum under the shared permutation:

```text
p = (1 + count of null max >= observed max) / (1 + 2000)
```

equality counting toward the null. Per-bin values are reported for
description and carry NO independent inferential claim.

IT IS NOT A NULL FOR `C`, `C_star` OR `D`, and an earlier draft of this
document wrongly used it as one. Two reasons, either sufficient. First,
mechanically: the permutation moves scalar SCORES among dates, while
`C` and `D` are computed from `R_sh` directly, so permuting scores
leaves them UNCHANGED - it is not a weak null, it is not a null at all.
Second, and deeper: permuting session order tests temporal ordering or
cross-session independence. It does NOT test equality of WITHIN and
CROSS covariance CONDITIONAL ON ELAPSED SEPARATION, because a smooth
continuous process can carry nonzero covariance on both sides of a
boundary and destroying its ordering is no null for "no boundary
discontinuity."

No replacement permutation is improvised here. A valid boundary test
would need a justified exchangeability model or an explicit covariance
model whose null is `D(bin) = 0` conditional on elapsed separation, and
that is a separate preregistration if it is ever wanted.

## The boundary contrast, and what it turned out it cannot do

The boundary contrast, at every separation bin where BOTH classes have
support, computed SEPARATELY for the two pair strata:

```text
D_ordinary(bin) = C_ordinary(bin, WITHIN) - C_ordinary(bin, CROSS)
D_hour20(bin)   = C_hour20(bin, WITHIN)   - C_hour20(bin, CROSS)
```

They are never maximized jointly and never combined; each is reported on
its own, because hour 20's support geometry differs from every other
hour's and a joint statistic would let one stratum's scarcity move the
other's reading.

THIS MEASUREMENT SELECTS NO ARCHITECTURE. `D` has no valid null - see
the permutation section - and none is improvised to preserve a selection
this evidence cannot make. `C`, `C_star` and `D` are therefore
DESCRIPTIVE, and the mechanical consequence is:

```text
ALL THREE ADVANCE AS SEPARATE SUCCESSOR CLASSES:
  A-only      continuous slow state
  B-only      boundary-associated session factor
  A+B mixed   both
```

Even a valid boundary test would not have selected `B-only`: a detected
discontinuity supports the PRESENCE of a boundary-associated component
and says nothing about the ABSENCE of a continuous one, so without an
equivalence test it cannot separate `B-only` from `mixed`. The branches
a valid test would have supported are recorded so a later reader sees
what was and was not on offer:

```text
discontinuity detected      reject A-only; advance B-only and mixed
none detected               advance all three
```

WHAT THIS MEASUREMENT IS FOR, restated to match what it can do: it sets
the PARAMETER GEOMETRY of the three classes - the timescales, the
magnitudes, the hour-20 stratum's behavior - rather than eliminating any
of them. That is a smaller claim than the document's first draft made
and it is the one the evidence supports.

THE REMAINING STATISTICS AND THEIR ROLE, stated so none of them
acquires one by implication: `S(g)` and its permutation p-value, and
`C_star`, are PARAMETERIZATION EVIDENCE ONLY. Significant score
persistence does not change which classes advance; it informs how the
slow component of any of them is parameterized. Low support is likewise
a result and changes no branch.

Nothing may select the friendlier architecture after seeing which is
friendlier - which is now trivially satisfied, since nothing selects at
all.

## Uncertainty ownership, per statistic

```text
S(g)                       permutation null only
C and C_star               descriptive point estimates plus pair counts
D_ordinary and D_hour20    descriptive point estimates plus pair counts
f_s and V^(-s)             point estimates only
```

NO BOOTSTRAP APPEARS IN THIS MEASUREMENT. Every statistic here is
defined by actual elapsed separation or by a permutable score set, and
the 12a circular block resample manufactures adjacency the calendar does
not contain - the reason already frozen in the ordered-count document.

## Artifact contents

All pair counts; the factor normalization and sign-alignment outcome per
held-out session; every per-session score; every held-out loading; the
residualized field's statistics; null counts and p-values SCOPED TO
`S(g)`, which is the only statistic carrying inference; every refusal
with its reason; THE CLASSES ADVANCED, which are fixed at `A-only`,
`B-only` and `A+B mixed` and are not a result; the sequence content
hash; this document's identity; the implementing commit; and the
outcome.

```text
completed             every frozen statistic produced or explicitly
                      refused under a stated rule
input_mismatch        the bound sequence hash does not match; refuse
                      without computing
insufficient_support  a frozen statistic could not be computed and no
                      refusal rule covers the case
```

## What this does NOT do

It does not choose a parameterization, fit anything, or rank a
candidate - AND it does not choose an architecture class either. All
three advance regardless of what it finds. What it produces is their
PARAMETER GEOMETRY: the timescales, the magnitudes, and the hour-20
stratum's behavior, measured rather than assumed.

## RESULT, 2026-08-11

Ran artifact-only at commit `69aa132`, outcome `completed`, sequence
hash matched, 506 cells included and none excluded, 22 cross-fitted
scores with zero refusals. All three classes advanced, as the fixed rule
requires.

```text
gap        S(g)       pairs
1 day    + 0.8624       17
2 days   - 4.9303       12
3 days   - 1.0383       11
4+ days  - 0.0851      191

shared-permutation max statistic  4.9303
null exceedances                  42 of 2,000
p-value                           0.0215
```

Descriptive contrasts, carrying no inference by construction:
`D_ordinary` is -0.0167, -0.0074 and +0.0001 at its three supported
bins; `D_hour20` is -0.0383, -0.0093, -0.0013, -0.0163 and +0.0050
across five.

THE CORRECT READING, which is narrower than the p-value invites and is
stated here because the first attempt to describe it overreached:

> Cross-fitted common-mode scores reject exchangeability over the fixed
> July dates under the preregistered shared-max permutation,
> `p = 0.0215`. The maximum is the negative two-day covariance estimate
> from 12 pairs. The result establishes CALENDAR-ORGANIZED SCORE
> STRUCTURE, but does not identify persistence, oscillation, mean
> reversion, or a weekday effect.

The permutation is VALID; what was too broad was the label on it. It
tests exactly one null - that the scores are exchangeable among the
fixed July session dates - preserving the dates, the gap graph, the pair
counts and the unequal bin sizes while destroying every association
between score and calendar position. Rejection therefore cannot separate
stochastic serial dependence from weekday or holiday structure, a local
regime pattern, oscillation, or another calendar-linked effect.

Three candidate worries, resolved:

- OSCILLATION IS NOT ESTABLISHED. The negative two-day estimate is
  descriptive evidence only; twelve pairs cannot license an oscillatory
  successor component, and that bin carried no independent inferential
  claim under the frozen max test.
- LOW SUPPORT DOES NOT INVALIDATE THE P-VALUE. The permutation
  reproduces the same pair counts in every replicate, so the two-day
  bin's greater sampling variability is represented in the null maximum
  distribution. Power concentrates in sparse bins and the effect
  estimate is unstable, but the exchangeability p-value is not
  anti-conservative for that reason.
- CALENDAR CONFOUNDING IS THE REAL LIMIT, though not by the route first
  guessed: the retained dates are all weekdays and Friday-to-Monday
  pairs fall mainly in the THREE-day bin, so the two-day result is not a
  weekend contrast. The gap bins nonetheless carry different weekday
  compositions, and the permutation shuffles scores away from the
  calendar positions a persistence test would want preserved.

CONSEQUENCES FOR THE SUCCESSOR, binding: all three classes continue to
advance; NO two-day oscillation is encoded and NO iid or session
component is excluded on this result; the score-gap curve is reported as
a DIAGNOSTIC for every class; and the final multi-horizon tape gates
judge the joint arm rather than this July lag pattern becoming a
mechanism requirement.

A calendar-adjusted persistence test - weekday-preserving randomization,
or a frozen calendar-effect model followed by a residual dependence test
- would need its own preregistration and more design data, and at 22
sessions would be weak. IT MAY NOT BE PAID FOR WITH JUNE: June cannot
both resolve this geometry and remain the untouched acceptance holdout.
Resolving it would need separate design data or a separately authorized
split.

## The June corpus, stated at its true weight

The successor's final acceptance needs an OBSERVED holdout not used to
choose its architecture, horizons, tolerances or parameter grid.
Generated seed holdout is not a substitute: it re-draws from the same
fitted model rather than supplying independent market evidence. July
2026 MNQ TBBO cannot be that holdout - it has served protocol 10's fit,
protocol 11's session refit, 12a's measurement, 12b's screen, the count
curve, the ordered counts and this measurement.

THE ACCURATE STATEMENT, correcting an earlier draft of this document
that claimed the purchase report's named condition "has been reached":

> The work has identified an untouched June corpus as a prospective
> confirmation population. This supplies grounds for the owner to
> consider a purchase decision contract; it is not itself that contract
> and does not satisfy or bypass the existing purchase prohibition.

A purchase contract would still have to freeze, BEFORE acquisition, the
successor acceptance criteria, the allowed uses of June, the possible
outcomes and what each outcome authorizes. July's result must not itself
unlock the purchase - which is exactly what `DATA-PURCHASE-REPORT.md`
warned against when it recorded that `mnq06` stays off the whitelist and
fails closed.
