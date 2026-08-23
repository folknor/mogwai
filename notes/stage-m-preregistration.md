# Stage M: the design measurement, a preregistration

Frozen 2026-08-12, signed on round 4 by codex session
019ff4db-a23c-7bc1-907f-8921b3add799 - the session that signed the
governing successor contract (`notes/successor-contract.md`), whose
Stage M obligations this document discharges. Round 1 was refused
with six blockers (the calendar-adjusted test's overclaimed null, the
July/month seed-domain collision, the unspecified month-generic
bootstrap, the mislabeled leave-one-month-out coverage, the unfrozen
Tier 2 admission hurdle, the impossible incumbent-rejection path);
round 2 with two (the non-executable H5 alternative constructions,
the open Tier 2 to Stage F handoff); round 3 with three (the
reversed-direction predictive-level transformation, the incomplete W
definition, the unreproducible Gaussian draw). One interpretation is
binding at signature, recorded in the `EXCESS` section. Frozen before
any design-month content read; amendments go through review, never
edits.

`notes/`-class: transient, no truth guarantee, nothing durable cites it.

## Scope and the two tiers

Stage M characterizes the design target: the observed multi-month
distribution of the quantities the successor must reproduce. No
candidate, no arm. Its outputs feed Stage F; nothing in it confirms
anything.

Two tiers, with different rules:

- Tier 1, frozen inferential and descriptive statistics: the per-month
  measurements and the calendar-adjusted exchangeability test.
  Estimators, populations, refusal rules and the test's null are
  frozen here and may not change after any design-month read. Only
  Tier 1 results may be cited as established evidence by Stage F.
- Tier 2, exploratory design work: the slow-projection feasibility
  program. Design months are open evidence under the contract, and
  projection form may iterate on them freely - what is frozen is the
  admission hurdle every candidate faces (numeric, below), which the
  search may never weaken. Tier 2 may inform Stage F; it may never be
  quoted as inference, and no architecture class may be excluded from
  it.

Architecture exclusion is not attempted in Stage M, confirmed by the
reviewer: no distribution-free null on this population has a rejection
that logically excludes A-only, B-only or mixed. A boundary
discontinuity cannot exclude a continuous component; absence of one
cannot exclude a boundary component without a powered equivalence
test; within-hour observations cannot distinguish a sufficiently slow
continuous state from a session intercept, and multi-month calendar
structure does not lift that equivalence. All three classes advance
into Stage F, which discriminates by candidate behavior under frozen
gates. A believed-valid exclusion test is a new dated preregistration,
not an amendment here.

## Binding

```text
population       every month the seal ledger records with role
                 new-design at first Stage M read - nominally 2025-08
                 through 2026-04 MNQ TBBO - plus 2026-07 as
                 spent-design, method backcheck and with/without month
month identity   the canonical integer key YYYYMM, never an ordinal;
                 ordinals silently change meaning if delivery differs
                 from the nominal manifest
inputs           per-month corpus files and content hashes FROM THE
                 SEAL LEDGER, recorded verbatim in every artifact; a
                 hash mismatch at read time is input_mismatch and
                 STOPS STAGE M ENTIRELY - no other month is processed
                 past it
parent inference the 12a contract, subcontract hash
                 1ca79d9cd043e7ce4b8b633fdbcdf0547a02a26570ea9120eb0141254a8ad954
scheduling       12a section 3.2, unchanged
usable sessions  the 12a usable-session rule applied per month; each
                 month's usable count is reported. A month with fewer
                 than 15 usable sessions is flagged THIN with its
                 count and still measured - thinness is a result. The
                 flag does not by itself license or exclude the month
                 from any Tier 2 fit; each candidate projection's
                 specification states how thin months enter fitting,
                 and their validation failures count like any other.
```

A design month that fails delivery or hash verification at the ledger
stage is recorded as undelivered and the population proceeds without
it; the contract roles do not change. If the delivered design
population is smaller than SIX months, Stage M stops with outcome
`design_population_insufficient`. Six is an operational floor, frozen
now so nobody argues it against known contents; it is NOT claimed to
be sufficient for validating predictive coverage, which is Stage F's
problem to solve conservatively.

## Seed domains, and the July compatibility rule

Two disjoint domains, so the method backcheck can be exact:

- July (key 202607) runs under the original frozen seed derivations
  of each inherited document, with no month coordinate of any kind -
  byte-identical procedure to the committed runs.
- New design months run in a separately named Stage M domain: every
  inherited permutation and bootstrap replaces its document's seed
  constant input with
  `tuple_mix(STAGE_M_SEED, [YYYYMM, <the document's own tuple
  components in their frozen order>])`,
  `STAGE_M_SEED = 4483921760958317264`. The algorithms (the 12a 5.1
  Fisher-Yates with state advancement, the splitmix64 derivations)
  are unchanged; only the seed input is domain-separated.

The July backcheck must complete before the Stage M domain is used on
anything.

## The month-generic bootstrap, frozen explicitly

The inherited count-curve and ordered-count texts freeze a 22-session
pseudo-month because July has 22 usable sessions; that text did not
specify other months, and this is a declared month-generic extension,
not a reading of the July text:

- Each per-month bootstrap replicate contains exactly that month's
  `S_m` usable sessions, using the same circular five-session block
  algorithm, ordering and wrapping semantics, with the replicate
  count (first 2,000) unchanged and the seed from the Stage M domain
  keyed by YYYYMM.
- July alone uses the original 22-session seed path, for exactness.

## Tier 1a: the per-month measurements, bound by reference

Each design month gets, independently, with per-month artifacts:

1. The full relevant 12a observed measurement - the evidence blocks of
   `notes/protocol-12a-measurement-spec.md`, observed side, unamended.
   July's frozen ladder targets are NOT re-derived; per-month values
   characterize the design target's variation.
2. The extended count curve, observed side, as frozen in
   `notes/count-curve-preregistration.md`: horizons {1, 5, 15, 60,
   300} s, nested scheduling, hour 20 its own stratum, the
   within/between decomposition with its identity check at the frozen
   tolerance, zero probability, count mean, nearest-rank p99, the
   bootstrap per the month-generic rule above.
3. The ordered-count sequence and panels, as frozen in
   `notes/ordered-counts-preregistration.md` including its
   structural-inapplicability amendment: retained one-second sequence
   with content hash, Panel A and Panel B complete.
4. The slow-geometry reduction, as frozen in
   `notes/slow-geometry-preregistration.md`: cross-fitted scores,
   S(g) with its shared-max permutation, C, C_star and both D strata
   as descriptive outputs.

Where a frozen document names July-specific constants (its corpus
hashes, its 22 sessions), the per-month application substitutes that
month's ledger-bound inputs and usable sessions under the rules
frozen above; every other estimator choice binds unchanged. Any point
where the frozen text cannot be applied month-generically is recorded
as a refusal with reason, never adapted silently.

Combined reporting: per-month values always; combined summaries as the
across-month mean, standard deviation and min/max of each per-month
statistic, with the month count. Months are never pooled into one
population before a statistic is computed. Everything is reported with
and without July.

## Tier 1b: the calendar-adjusted exchangeability test

What it tests, stated at its true width because the July result was
first overclaimed the same way: the null is that the computed
cross-fitted scores are exchangeable among the dates sharing their
month and weekday class. The null is stated at the score level - the
object permuted is the object the claim is about, which is what makes
permuting computed scores valid without any equivariance argument
about the cross-fitting procedure.

Rejection establishes conditional non-exchangeability given month and
weekday - structure beyond weekday composition and month membership.
It does not identify serial dependence: week-index position, holiday
proximity, expiry and roll position, month-end position or local
regime structure all remain candidate explanations. This is
parameterization evidence with weekday removed as a confound, and it
is recorded as exactly that. Non-rejection is a reported bound, not
evidence of independence. Neither outcome excludes an architecture
class or licenses a mechanism; per-bin values are descriptive.

```text
scores       per month, the cross-fitted scores f_s exactly as frozen
             in the slow-geometry document, computed within that
             month only; no cross-month factor fits
permutation  scores move only within (month, weekday) cells; dates,
             gap graph, pair counts and every weekday marginal are
             preserved
statistic    S(g) as frozen in the slow-geometry document, computed
             over pairs WITHIN each month (pairs never cross months).
             THE PRIMARY INFERENTIAL STATISTIC is the MONTH-EQUAL
             pooled S(g): the unweighted mean over months of each
             month's per-bin S(g), because Stage C predicts ONE
             month, so the estimand is the expected gap covariance of
             a randomly selected design month. The pair-count-weighted
             pool is also reported, as a precision-oriented
             DESCRIPTIVE statistic (its estimand: the covariance of a
             randomly selected eligible pair), and carries no
             inference. A (month, bin) cell below 4 pairs is null for
             that month and excluded from the month-equal mean with
             its count; a pooled bin is supported when at least 5
             months contribute, else null with counts.
test         maximum absolute month-equal pooled S(g) over supported
             bins; gap bins {1, 2, 3, 4+} days
replicates   2,000, one shared permutation per replicate across all
             cells, bins and months
seeding      tuple_mix(STAGE_M_SEED, [0, replicate_index]) - the
             reserved pseudo-month key 0 marks the cross-month test -
             with the 12a 5.1 Fisher-Yates applied independently
             within each (month, weekday) cell in frozen order
             (months ascending by YYYYMM, weekdays Monday first)
p-value      (1 + count of null max >= observed max) / (1 + 2000),
             equality toward the null
```

The power analysis, run and recorded before any design-month content
read, on calendar data only (CME trading calendars are knowable
without corpus content). The simulation law, fully specified:

```text
process       scalar scores only; factor directions and cross-fitting
              are NOT simulated - a DECLARED SIMPLIFICATION, recorded:
              the analysis calibrates the test's sensitivity to
              score-level persistence, not the full pipeline
index         AR(1) over trading-session index within each month
              (calendar gaps do not add decay - declared, because the
              alternative was equally arguable and one had to be
              frozen); factor initialized at its stationary
              distribution per month, independent across months
variance      total per-score variance fixed to July's sample score
              variance (July is spent-design; using it is not
              leakage)
factor share  lambda in {0.25, 0.5, 0.75}: the factor contributes
              lambda of total variance, iid Gaussian noise the rest -
              gridded because the factor-to-noise ratio is
              load-bearing for power
persistence   rho in {0.3, 0.5, 0.7, 0.9} over the session index
weekdays      no weekday effects added - declared; the test being
              calibrated conditions weekday out
population    the actual design-month calendars, sessions per month
              as scheduled
runs          500 simulations per (rho, lambda) cell, the frozen test
              applied to each
reporting     the full power surface with a binomial 95 percent
              interval per cell (500 runs put the Monte Carlo
              standard error near 2.2 points at power 0.5), and the
              minimum rho per lambda reaching 80 percent power
```

Predeclared interpretation rule: if recorded power at
(rho = 0.5, lambda = 0.5) is below 50 percent, the test still runs
but its non-rejection is uninformative and may not be cited as
evidence against persistence. Power above 50 percent is not thereby
"adequate"; the surface speaks, not a threshold.

## Tier 2: the slow-projection feasibility program

Goal: hand Stage F a one-month slow-confirmation projection meeting
the contract's eight requirements, or record that the frozen
feasibility program identified none - an open finite search cannot
establish nonexistence over an unspecified projection universe, and
this document does not claim it can.

What leave-one-month-out coverage is and is not, stated before the
procedure because the label is load-bearing: Tier 2 coverage is an
adaptively selected internal design diagnostic. The search tries
candidates against the same months repeatedly, so passing the hurdle
does not establish nominal out-of-sample coverage - it is feasibility
screening and calibration evidence. Stage F must account for the
adaptive search when freezing the final projection (conservative
coverage, complexity restriction, or a fully specified predictive
construction), and Stage C is the only untouched validation of
whatever is selected. Per the contract's binding interpretation, the
predictive region is always a frozen predictive distribution
incorporating between-month variation and within-month estimation
uncertainty, never an empirical min-max envelope.

The admission hurdle, frozen numerically now so the search cannot
weaken its own judge. A candidate projection is admissible only if
all of the following hold:

```text
H1  dimension   the projection vector has at most 4 coordinates
H2  verdict     the compatibility rule is ONE joint statistic over
                the vector with a single frozen rejection region -
                multiplicity handled by construction, never
                per-coordinate tests
H3  coverage    at the frozen 90 percent predictive level, at most
                ONE design month falls outside its leave-one-month-
                out region; thin months count like any other, and an
                hour-20-driven failure counts - no stratum carve-out
H4  incumbent   at least 23 of the 24 frozen incumbent control
                months (below) are rejected by the candidate's
                compatibility rule fitted on all design months
H5  power       at least 160 of 200 simulations rejected for EACH
                frozen unacceptable alternative (below)
H6  refusals    the candidate specifies its refusal semantics and how
                thin months enter fitting; an unspecified case found
                during validation is an inadmissibility, not a patch
```

The integers are the rule; no implementation decides how fractional
counts round.

The frozen unacceptable alternatives for H5, executable as written or
not frozen at all. Both operate on the session-hour log-rate residual
field `R_sh` as defined in the slow-geometry document, per month. H5
evaluation therefore applies to the projection's session-hour-level
inputs; a candidate requiring finer-than-session-hour inputs must
define, in its own specification, how these perturbations lift to its
input level, or it is inadmissible under H6.

Simulation geometry, shared by both alternatives and matching the
Stage C use: simulation `i` targets the leave-one-month-out fold of
month `m(i)`, months cycling in ascending YYYYMM order over
i = 1..200. The candidate's predictive distribution is fitted on the
remaining months unperturbed; the held-out month is perturbed by the
alternative's construction; the compatibility rule is evaluated on
that perturbed month, and rejection is counted per simulation. Missing
cells stay missing; the perturbation touches only cells that exist;
thin months take their turn in rotation like any other; hour 20 is
treated exactly like every other hour in both constructions. Seeds:
`tuple_mix(STAGE_M_ALT_SEED, [alternative_id, i])`,
`STAGE_M_ALT_SEED = 3958267140192837465`, alternative_id 1 = NO-SLOW,
2 = `EXCESS`.

NO-SLOW, destroying cross-hour session coherence while preserving
every hour marginal: for each hour `h` of the held-out month,
independently permute the values `R_sh` across that month's sessions
`s` (an independent permutation per (hour, simulation), drawn by the
12a 5.1 Fisher-Yates from the seed above with `h` appended to the
tuple). Each hour's marginal distribution is unchanged by
construction; any session-level common mode is destroyed. The
earlier same-month-profile-redraw wording is retracted - it described
a different construction that preserves coherence.

`EXCESS`, adding a session factor at a frozen magnitude. `W` is a
frozen Tier 2 reduction of Tier 1a evidence (Tier 1a reports `R_sh`;
it does not report `W_m`), estimated once, before H5 runs, and
recorded:

```text
u_ms  the unweighted mean of R_msh over every existing hour cell h
      of session s in month m, hour 20 included. A session missing
      one or more of that month's measured hour cells is EXCLUDED
      from the W estimation, with its identity and missing count
      recorded - matching the slow-geometry rule that a session with
      an excluded cell refuses its score.
W_m   the population variance of u_ms across the eligible sessions
      of month m; null with its session count if fewer than 2
      sessions are eligible.
W     the unweighted mean of W_m across the new-design months only -
      July is excluded, declared: W characterizes the design
      population Stage C predicts from. Null W_m months are excluded
      with their count. If fewer than FOUR months contribute, Stage M
      records excess_baseline_unavailable, H5 cannot run, no
      candidate can be admissible, and Tier 2 ends with
      no_one_month_slow_confirmation_design.
```

Thin months enter W identically to any other month.

```text
factor   g_s per eligible session of the held-out month, iid
         Normal(0, 4 * W) - 4x means the ADDED factor has variance
         four times the estimate; independent across sessions and
         months, shared across every hour of its session (uniform
         hour loading), NO calendar persistence - this alternative is
         excess magnitude, not persistence, a declared scope limit
applied  R_sh becomes R_sh + g_s for every existing cell of session s
```

Binding interpretation at signature: "eligible session" means
eligibility under the complete-hour rule above - a session excluded
from the W estimation receives no factor and its existing cells are
unperturbed, and the artifact reports the count of such sessions per
simulation. This dilutes the alternative slightly in months with
incomplete sessions, which is the conservative direction for a power
hurdle. Perturbing every session with any existing cell instead would
be a substantive change requiring review before any inspection.

The draw, frozen to the operation because a seed plus "Normal" does
not define values:

```text
seed   state = tuple_mix(STAGE_M_ALT_SEED, [2, i, YYYYMM,
       session_date]) with session_date the YYYYMMDD integer - one
       independently derived seed per (simulation, session), so
       session iteration order cannot change any value
u1,u2  two successive splitmix64 advances of that state, each mapped
       to (0,1) as (state >> 11) * 2^-53, with a zero u1 replaced by
       2^-53
z      sqrt(-2 * ln(u1)) * cos(2 * pi * u2)   (Box-Muller, cosine
       term only)
g_s    z * sqrt(4 * W)
```

Canonical session ordering, everywhere either alternative iterates
sessions (including the NO-SLOW Fisher-Yates input vectors): ascending
session date. Both constructions, their seeds and the estimated `W`
with its per-month `W_m` table are recorded verbatim in the artifact.

The incumbent control runs, authorizing what round 1 wrongly banned:
Tier 2 requires evaluating candidates on incumbent-generated months,
and the committed artifacts do not necessarily contain every
projection input (ordered generated session-hour fields, generated
common-mode scores, calendar-gap statistics). Incumbent-only
generator runs are therefore authorized under this preregistration:
the shipped generator, unchanged, 24 month-scale walks, seeds
`tuple_mix(STAGE_M_INCUMBENT_SEED, [i])` for i in 1..24,
`STAGE_M_INCUMBENT_SEED = 6172038459284617530`, each walk projected
onto the calendar of a design month in rotation (months ascending,
cycled). These are control runs, strictly separated from candidate
evaluation: no candidate mechanism ever runs in Stage M, and the
incumbent runs may not be inspected for anything except projection
evaluation. Cost: month-scale walks priced at roughly 25 s each,
about ten minutes total.

Procedure: candidates drawn from the contract's ingredient list
(session-level common-mode score dispersion; calendar-adjusted lag or
gap summaries with adequate support; a sampling-adjusted slow-variance
projection derived from the ordered counts; cross-hour coherence of
session-rate residuals; a bounded excess check), iterated freely,
every candidate tried and rejected recorded with reasons. The program
ends in exactly one of two recorded outcomes: one designated
projection specification (statistic vector, predictive construction,
coverage rule, refusal semantics, thin-month treatment) handed to
Stage F, or `no_one_month_slow_confirmation_design`, which stops the
contract before Stage I per its terms.

The designation rule, predeclared so Tier 2 hands Stage F one object
and not a friendly menu. Among all candidates clearing H1-H6, exactly
one is designated, by this ordered rule: (1) fewest coordinates;
(2) tie: highest incumbent rejection count under H4; (3) tie: highest
minimum rejection count across the two H5 alternatives; (4) tie: the
earliest admissible candidate in the recorded search order. The rule
is mechanical and the artifact shows its evaluation.

The handoff to Stage F, mechanically closed:

- Stage F may freeze the designated projection unchanged; or
- Any change whatsoever - coordinates, predictive construction, joint
  statistic, coverage region or level, refusal semantics - produces a
  new projection that must re-run the entire H1-H6 hurdle before
  Stage F may freeze it. A changed projection that fails the re-run,
  or does not run it, produces
  `no_one_month_slow_confirmation_design`.
- There is no pre-authorized transformation. A round-3 draft
  authorized raising the predictive level as "conservative"; that was
  a reversed direction and is retracted - raising predictive coverage
  widens the acceptance region, easing H3 while weakening H4 and H5,
  and no direction-only change to predictive coverage is uniformly
  conservative for both observed coverage and defect detection.
- Stage F may not weaken any hurdle, widen the coordinate cap, select
  among candidates (Tier 2 already designated), or prefer any
  projection for anticipated candidate friendliness.

Search order is mechanical: it is the chronological order in which a
complete candidate specification is committed to the append-only
Tier 2 artifact before that candidate's H3-H5 results are computed;
it may not be reordered retrospectively.

## Ordering, backchecks and the anti-peek rule

```text
step 0   this document signs; the power analysis runs on calendar
         data only and its surface is recorded in the artifact
step 1   THE JULY METHOD BACKCHECK: the complete Tier 1a machinery
         runs on spent-design July under the ORIGINAL seed paths.
         EXACT EQUALITY is required on every DETERMINISTIC POINT
         STATISTIC both compute (count-curve records, ordered-count
         panel point estimates, slow-geometry scores and S(g)), and
         on permutation and bootstrap outputs ONLY where the original
         algorithm and committed artifact retain comparable values -
         the backcheck artifact distinguishes point-estimate equality
         from null-distribution equality per statistic. A mismatch is
         method_mismatch: STOP, nothing else is read.
step 2   Tier 1a runs per design month, ascending YYYYMM; Tier 1b
         runs once all months' scores exist
step 3   the incumbent control runs execute under their frozen seeds
step 4   Tier 2 runs; it may re-read design months freely
```

No design-month content is read before step 1 passes.

## Outcomes

```text
completed                          every Tier 1 statistic produced or
                                   explicitly refused under a stated
                                   rule; Tier 2 reached one of its
                                   two outcomes
method_mismatch                    the July backcheck failed; no
                                   design month was read
input_mismatch                     a ledger hash failed at read time;
                                   Stage M stops entirely
design_population_insufficient     fewer than six design months
                                   delivered
```

`no_one_month_slow_confirmation_design` is a Tier 2 outcome recorded
within `completed` - the measurement succeeded; the confirmation
architecture did not.

## Cost, priced before it is authorized

Ten corpus passes (nine design months plus the July backcheck) at
roughly 334 s per 873 MB month is about 56 minutes of streaming
alone; per-month panels and 2,000-replicate bootstraps come on top
and may dominate. The run plan is therefore per-month invocations
with the lock released between months; any single invocation
projected past 20 minutes is cleared with the owner first. The
incumbent control runs add about ten minutes.

## Amendment 1, 2026-08-12: complete-case training in the cross-fit

Signed 2026-08-12 by codex session
019ff606-1f16-7800-88ee-e52f083fac70, the fresh reviewer that issued
the underlying conditional ruling after the prior sessions went
cache-cold.

A reviewed amendment with its own signature, not an interpretation:
the frozen slow-geometry construction uses all other sessions as
training, and changing that population changes the training moments,
correlation, loading and score. The month-generic refusal clause says
what to do when the formula cannot apply; it does not authorize
inventing a missing-data estimator, so this text goes through the
amendment boundary.

The trigger, disclosed: the 2026-03 Tier 1a run crashed because
training sessions carry excluded session-hour cells. The cause is
deterministic schedule geometry - the US daylight-saving transition
shifts the CME schedule's UTC hours mid-month, so 2026-03 sessions
before March 8 populate a different UTC hour set (March 2 through 6
carry an excluded hour-22 cell). Retaining the mechanically-correct
fold refusal would make Tier 1b systematically blind to exactly the
calendar-phase variation Stage M exists to characterize.

The rule, frozen:

- A training session with any excluded cell is removed entirely from
  that fold - before every per-hour moment and the correlation
  matrix - so the statistics stay internally consistent over one
  population. The estimand changes and is named honestly: the
  cross-fit trains on the complete-case training population, not on
  all other sessions.
- The held-out complete-cell requirement is unchanged: an incomplete
  held-out session still refuses its own score.
- At least 12 complete training sessions must remain after removing
  the held-out session, else that held-out score refuses. Twelve is
  support-aware, disclosed: it was chosen with the March pattern
  known - 18 usable sessions, five incomplete, leaving exactly 12
  complete training sessions per fold - and is not result-driven with
  respect to any score (none existed; the runs crashed first). Its
  substantive basis: a majority of the nominal training population is
  retained and only the leading direction is estimated - no
  covariance inverse, no full-rank requirement, no secondary factor.
  A floor below 12 needs a new stability analysis.
- Drop identities, reasons, the resulting training count and the
  retained training identities are recorded per fold.
- Zero-training-variance and eigensolve refusals are unchanged.
- Completed months re-run under this amendment, July included as a
  regression check: months without excluded cells must reproduce
  their existing scores exactly.
- November is not recovered, stated against the temptation to claim
  otherwise: 2025-11's held-out sessions themselves refuse on hour
  22, which this amendment cannot rescue. It recovers the
  complete-session portion of March only; the program retains a
  narrower, honestly-recorded DST selection effect.
- Tier 1b support rules are unchanged; a month contributes whatever
  supported bins its scored sessions produce.

The DST finding, recorded now and carried into Stage F as a binding
input: the observed target has a twice-yearly Central-time-to-UTC
phase transition that the July-fitted generator cannot express.
Complete-case scoring is a missing-data repair, not a model of that
transition. Stage F must decide how calendar phase enters the
successor mechanism and its gates, and must not treat March's
surviving post-transition scores as representing the whole month.

## Amendment 2, 2026-08-12: session-local coordinates for the slow-geometry reduction

Signed 2026-08-12 by codex session
019ff606-1f16-7800-88ee-e52f083fac70 after one refusal round (the
Amendment 1 supersession, the executable diagnostic denominator, the
complete moved-object re-verification list). The diagnostic bounds
were explicitly accepted as written.

Result-awareness, disclosed first: this amendment was ruled after the
support pattern was known (September 17 scores, October 23, and
November through February at zero scores - every standard-time
session refuses on the unpopulated UTC hour-22 cell) and after
September and October score values were exposed. It is blind to every
winter score value: none has ever been computed in any coordinate
system.

The defect: the slow-geometry hour set is a daylight-time UTC hour
set. In standard time the CME 17:00 Central open maps to UTC 23:00,
so the whole winter season refuses and Tier 1b silently becomes a
daylight-only estimand - half the design population excluded for
deterministic clock labeling. Amendment 1 cannot reach this: the
refusals are held-out, not training.

Supersession: Amendment 2 explicitly overturns Amendment 1's scope
conclusions - the claims that November is not recovered, that only
March's complete-session portion is recovered, and that the residual
DST effect is narrow were drawn before the season-wide support
pattern was known and no longer stand. Amendment 1's complete-case
training rule, its 12-session floor, its per-fold records and its
Stage F DST-finding obligation all remain in force unchanged; only
its conclusions about which months the program recovers are
superseded by this amendment's coordinate repair.

The repair, ruled option (ii): one global session-local coordinate
system for the slow-geometry reduction - a coordinate repair, not a
month selection, preserving a common 23-coordinate factor object
across seasons. Frozen:

```text
q          = floor((window_end_ns - scheduled_session_open_ns) / 3600 s)
local_hour = min(q, 22)
```

The min preserves the inherited endpoint attribution while assigning
the exact-close endpoint to the final coordinate. An implementation
may instead map each inherited UTC endpoint-hour cell to its
scheduled-open-relative coordinate, provided it states and tests
exact equivalence to the formula. The coordinate set is always 0..22
in canonical ascending order, frozen. Local hour 22 is the partial,
halt-containing stratum formerly labeled UTC hour 20 in
daylight-time months; every amended artifact and later document
names it local hour 22 or the partial/halt stratum, never hour 20
without qualification.

Scope: this amendment changes the coordinates of the slow-geometry
reduction only - the residual field it consumes, the cross-fitted
factor, scores, loadings, z_star, C_star, C, D and their pair
strata. The stratum formerly keyed on UTC hour 20 is the local-22
stratum. Panel A, Panel B and the count curve remain on their
inherited UTC endpoint-hour coordinates unless separately amended.

Byte identity is not assumed: reordering matrix columns can change
the Jacobi eigensolver's operation order, and mathematical
invariance is not byte identity. Exact equality is required where
achieved; if eigensolver ordering alone moves floating-point values,
that fact is recorded and agreement is judged under this separately
frozen, executable diagnostic - for scalar values a and b:

```text
abs(a - b) <= max(1e-9, 1e-12 * max(abs(a), abs(b)))
```

applied elementwise to vectors and matrices after the frozen
coordinate permutation and sign alignment, reported per statistic.
Nulls, support counts, refusal identities, permutation exceedance
counts and p-values derived from identical integer counts require
exact equality. Any discrepancy beyond the bound, or any discrepancy
not attributable to operation order, stops the amendment.

Re-verification, in order, March running only after every check
passes:

1. July: exact equality of the reconstructed session-hour parent
   totals, exposures, log rates and residuals under the explicit
   UTC-to-local bijection; then the entire moved reduction - scores,
   loadings (after applying the same coordinate permutation), z_star,
   S(g), C, C_star, both D strata, pair counts, supported-bin
   counts, refusal records, stratum assignments, permutation
   exceedance count and p-value - exact where achieved, else the
   frozen diagnostic above.
2. September and October: the same old-versus-new comparison over
   the same complete object list; their scores are already exposed,
   so any change is reported as a correction, never treated as new
   evidence.
3. November through February: verified to produce complete
   local-coordinate cells and scores subject to the unchanged
   refusal rules (Amendment 1's complete-case training and floor
   included).
4. March runs only after 1 through 3 pass; then April.

## Amendment 3, 2026-08-12: the exact-close second, and the July re-bless

Signed 2026-08-12 by codex session
019ff606-1f16-7800-88ee-e52f083fac70, first submission.

Result-aware, disclosed first: this amendment was ruled after the
Amendment 2 July gate stopped and the cause was attributed with data -
the enumerated 22-cell input-delta pattern and the approximate S(2)
movement (about 2.7e-6) were known. No winter or March slow-geometry
value existed in any coordinate system.

What was disproved: Amendment 2 stated that the min clause "preserves
the inherited endpoint attribution". It does not, at exactly one
second per session: the exact-close endpoint second is a scheduled,
populated second inside the trading session whose prior omission
resulted only from its endpoint mapping into excluded UTC hour 21.
The July gate stop was therefore correct and the discrepancy is
neither operation order nor implementation defect - it is the signed
formula being more correct than the inherited behavior.

The ruling: the signed mapping stands. Preserving the omission would
convert an accidental coordinate artifact into a permanent
measurement rule merely to retain byte identity, against the standing
re-bless discipline. July is re-blessed under session-local
coordinates as the Stage M reference.

The revised July gate, frozen:

- The 506 session-hour cells align one-to-one under the UTC-to-local
  bijection.
- Exactly the 22 local-hour-22 cells, one per July session, may
  differ at the input level, and each permitted difference must equal
  exactly the scheduled exact-close one-second window: exposure 2700
  to 2701, parent delta equal to that window's recorded parent count.
- No other cell, exposure, parent total, session identity or stratum
  assignment may differ.
- The complete moved reduction is still compared, and every changed
  value, pair count, support status, refusal, permutation exceedance
  count and p-value is recorded.
- The Amendment 2 numeric diagnostic no longer adjudicates these
  causally authorized changes; it remains applicable to any
  difference claimed to arise only from coordinate permutation or
  eigensolver operation order.
- The three-way attribution regression: (1) the original UTC
  reduction; (2) the session-local reduction with exact-close seconds
  temporarily excluded, comparison only - run 2 must reproduce the
  original moved reduction under Amendment 2's equality rules, which
  isolates the coordinate permutation; (3) the signed session-local
  reduction including them - the difference between runs 2 and 3
  must arise solely from the enumerated 22 seconds. Any downstream
  difference not mechanically attributable to the 22 authorized
  input deltas stops the amendment.
- The comparison harness's key-alignment defect (a structural
  mismatch presented as a value comparison) is fixed before this
  gate runs again; a structural mismatch is not a valid regression
  result even when it reports zero numeric differences.
- The resulting July artifact is re-blessed as the Stage M
  session-local reference before September onward proceeds.

The supersession record, preserved in the re-bless artifact: the
original artifact and hash; the re-blessed artifact and hash; the
Amendment 3 authority and implementing commit; the complete
old-to-new comparison; the enumerated 22 input deltas; the three-way
attribution result; and the explicit status that the original July
artifact remains the historical result of the signed UTC-coordinate
measurement, superseded as the Stage M reference by the session-local
artifact. The slow-geometry preregistration's result text is not
rewritten: its quoted S(2) = -4.9303 and p = 0.0215 accurately report
the original measurement at their displayed precision. Stage M
combined reporting and Tier 1b use only the re-blessed session-local
July artifact.

## Amendment 4, 2026-08-12: the DST-aware schedule frame

Signed 2026-08-12 by codex session
019ff606-1f16-7800-88ee-e52f083fac70 after one refusal round (the
unbound timezone authority); the countersignature verified the
artifact hash, release identity, coverage, transition count, permitted
offsets and boundary transitions directly.

Result-aware, disclosed first: ruled after the winter per-hour
artifacts and the support pattern were known, blind to every
corrected winter slow-geometry score - none has existed under any
correct frame.

The defect is source-population, not another slow-geometry defect:
the 12a subcontract freezes `UTC_OFFSET_MINUTES = -300`, the daylight
offset, so the signed July scheduler is non-generic. Applied to
standard-time months it schedules a dead pre-open hour and excludes
each session's real final trading hour (November: 840,315 rows
outside declared sessions, 3.8 percent, against September's 0.5).
The winter artifacts labeled `completed` cannot remain scientific
Stage M measurements; the binding "scheduling: 12a section 3.2,
unchanged" is replaced for all non-July applications of Stage M.

The frozen conversion:

- The frozen timezone authority, one selected implementation, exact
  identity bound at signature:

  ```text
  zone          America/Chicago
  tzdb release  2026c
  source        analysis/tz-america-chicago-2026c.json, generated by
                analysis/generate_tz_artifact.py from the host
                zoneinfo whose tzdata.zi header names release 2026c,
                one-second bisection at each offset change
  sha256        afe9024d96f492ad6ae821455cb60ff4127b2312acb0bd164003f95c127739bc
  coverage      2024-01-01 through 2030-12-31 inclusive, 14
                transitions, offsets -21600 and -18000 only
  ```

  Runtime scheduling reads this artifact, never the host zoneinfo.
  The Rust-database alternative from the ruling is not taken; no
  post-signature implementation choice remains in the population
  definition. A future tzdb release only matters if US law changes a
  transition inside the coverage window, which would be a reviewed
  amendment replacing the artifact and hash.
- Each session's four boundaries convert separately at their own
  civil instants - previous-date 17:00 open, trade-date 15:15 halt
  start, 15:30 halt end, 16:00 close - using the offset returned for
  each instant, never one offset sampled once and reused.
- Ambiguous or nonexistent civil instants refuse rather than guess.
- The derived UTC bounds are recorded per session in the artifacts.
- A mechanical check per ordinary full session: bounds strictly
  increasing, the halt exactly 15 minutes, scheduled open exposure
  exactly 81,900 seconds (23 hours minus the halt). US transitions
  occur while this market is closed, so no ordinary session should
  contain an internal offset change - but the implementation derives
  every boundary independently and refuses on any duration or
  exposure mismatch rather than assuming that fact.

Scope and re-runs:

- July remains under the original 12a subcontract and seed path, for
  historical identity and the backcheck.
- Every new-design month uses the DST-aware schedule. All affected
  months - 2025-11, 2025-12, 2026-01, 2026-02 and mixed 2026-03 -
  re-run from preflight through every schedule-dependent Tier 1a
  output: the full relevant 12a observed measurement, the ordered
  sequence, the count curve, both ordered-count panels and slow
  geometry. Tier 1b runs only from corrected artifacts.
- September and October receive a mechanical schedule-equivalence
  check; if their derived bounds are identical to the frame they were
  measured under, their artifacts remain valid with the check
  recorded - no ceremonial re-run.
- 2026-04 uses the corrected scheduler on its first run.
- The same calendar rule binds everywhere session geometry is
  constructed or projected: Tier 2 incumbent controls, H5 alternative
  simulations, Stage F generated and observed evaluations, and both
  Stage C confirmation bindings - otherwise observed and generated
  months could be evaluated on different frames. Updating the
  scheduling rule authorizes no calendar materialization for the
  sealed months now; their schedules still materialize only inside
  the blinded Stage C harness.

The invalidation record, append-only: every superseded artifact and
its hash; its former `completed` outcome preserved, never relabeled
as though the original run had refused; the new status
`superseded_invalid_schedule_frame`; the precise defect and affected
months; the amendment authority and implementing commit; the
corrected replacement artifact and hash; and old-to-new population
diagnostics - outside-session rows, scheduled exposure, usable
sessions, per-session bounds.

The validation limitation, recorded permanently: July-only backchecks
establish implementation continuity within one offset regime and
cannot validate month-generic calendar correctness - single-month
method validation is structurally blind to seasonal frame errors.
Stage F must include at least one daylight month, one standard-time
month and one transition month in its schedule and frame regression
tests.

## What Stage M may NOT do

- No candidate generator runs. The only generator runs are the 24
  frozen incumbent control walks above.
- No architecture exclusion, no mechanism proposal, no ranking.
- No acceptance criterion: Stage F freezes criteria after this is
  read, from the owner goal, never from what any candidate passes.
- No re-cutting after inspection - scoped precisely: Tier 1 and the
  Tier 2 admission hurdle are frozen; Tier 2 projection form iterates
  by design. A new Tier 1 statistic, horizon, month role or stratum
  is a new dated preregistration.
