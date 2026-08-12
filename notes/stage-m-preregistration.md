# Stage M: the design measurement, a preregistration

FROZEN 2026-08-12, SIGNED ON ROUND 4 by codex session
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
BINDING AT SIGNATURE, recorded in the EXCESS section. Frozen BEFORE
any design-month content read; amendments go through review, never
edits.

`notes/`-class: transient, no truth guarantee, nothing durable cites it.

## Scope and the two tiers

Stage M characterizes the DESIGN TARGET: the observed multi-month
distribution of the quantities the successor must reproduce. No
candidate, no arm. Its outputs feed Stage F; nothing in it confirms
anything.

Two tiers, with different rules:

- TIER 1, FROZEN INFERENTIAL AND DESCRIPTIVE STATISTICS: the per-month
  measurements and the calendar-adjusted exchangeability test.
  Estimators, populations, refusal rules and the test's null are
  frozen here and may not change after any design-month read. Only
  Tier 1 results may be cited as established evidence by Stage F.
- TIER 2, EXPLORATORY DESIGN WORK: the slow-projection feasibility
  program. Design months are open evidence under the contract, and
  projection FORM may iterate on them freely - what is frozen is the
  ADMISSION HURDLE every candidate faces (numeric, below), which the
  search may never weaken. Tier 2 may inform Stage F; it may never be
  quoted as inference, and no architecture class may be excluded from
  it.

ARCHITECTURE EXCLUSION IS NOT ATTEMPTED IN STAGE M, confirmed by the
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

- JULY (key 202607) runs under the ORIGINAL frozen seed derivations
  of each inherited document, with no month coordinate of any kind -
  byte-identical procedure to the committed runs.
- NEW DESIGN MONTHS run in a separately named Stage M domain: every
  inherited permutation and bootstrap replaces its document's seed
  constant input with
  `tuple_mix(STAGE_M_SEED, [YYYYMM, <the document's own tuple
  components in their frozen order>])`,
  `STAGE_M_SEED = 4483921760958317264`. The algorithms (the 12a 5.1
  Fisher-Yates with state advancement, the splitmix64 derivations)
  are unchanged; only the seed input is domain-separated.

The July backcheck MUST COMPLETE before the Stage M domain is used on
anything.

## The month-generic bootstrap, frozen explicitly

The inherited count-curve and ordered-count texts freeze a 22-session
pseudo-month because July has 22 usable sessions; that text did NOT
specify other months, and this is a DECLARED month-generic extension,
not a reading of the July text:

- Each per-month bootstrap replicate contains exactly that month's
  `S_m` usable sessions, using the same circular five-session block
  algorithm, ordering and wrapping semantics, with the replicate
  count (first 2,000) unchanged and the seed from the Stage M domain
  keyed by YYYYMM.
- July alone uses the original 22-session seed path, for exactness.

## Tier 1a: the per-month measurements, bound by reference

Each design month gets, independently, with per-month artifacts:

1. THE FULL RELEVANT 12a OBSERVED MEASUREMENT - the evidence blocks of
   `notes/protocol-12a-measurement-spec.md`, observed side, unamended.
   July's frozen ladder targets are NOT re-derived; per-month values
   characterize the design target's variation.
2. THE EXTENDED COUNT CURVE, observed side, as frozen in
   `notes/count-curve-preregistration.md`: horizons {1, 5, 15, 60,
   300} s, nested scheduling, hour 20 its own stratum, the
   within/between decomposition with its identity check at the frozen
   tolerance, zero probability, count mean, nearest-rank p99, the
   bootstrap per the month-generic rule above.
3. THE ORDERED-COUNT SEQUENCE AND PANELS, as frozen in
   `notes/ordered-counts-preregistration.md` including its
   structural-inapplicability amendment: retained one-second sequence
   with content hash, Panel A and Panel B complete.
4. THE SLOW-GEOMETRY REDUCTION, as frozen in
   `notes/slow-geometry-preregistration.md`: cross-fitted scores,
   S(g) with its shared-max permutation, C, C_star and both D strata
   as descriptive outputs.

Where a frozen document names July-specific constants (its corpus
hashes, its 22 sessions), the per-month application substitutes THAT
MONTH's ledger-bound inputs and usable sessions under the rules
frozen above; every other estimator choice binds unchanged. Any point
where the frozen text cannot be applied month-generically is recorded
as a refusal with reason, never adapted silently.

COMBINED REPORTING: per-month values always; combined summaries as the
across-month mean, standard deviation and min/max of each per-month
statistic, with the month count. Months are NEVER pooled into one
population before a statistic is computed. Everything is reported with
and without July.

## Tier 1b: the calendar-adjusted exchangeability test

WHAT IT TESTS, stated at its true width because the July result was
first overclaimed the same way: the null is that THE COMPUTED
CROSS-FITTED SCORES ARE EXCHANGEABLE AMONG THE DATES SHARING THEIR
MONTH AND WEEKDAY CLASS. The null is stated at the score level - the
object permuted is the object the claim is about, which is what makes
permuting computed scores valid without any equivariance argument
about the cross-fitting procedure.

REJECTION ESTABLISHES conditional non-exchangeability given month and
weekday - structure beyond weekday composition and month membership.
It does NOT identify serial dependence: week-index position, holiday
proximity, expiry and roll position, month-end position or local
regime structure all remain candidate explanations. This is
PARAMETERIZATION EVIDENCE with weekday removed as a confound, and it
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

THE POWER ANALYSIS, run and recorded BEFORE any design-month content
read, on calendar data only (CME trading calendars are knowable
without corpus content). The simulation law, fully specified:

```text
process       scalar scores only; factor directions and cross-fitting
              are NOT simulated - a DECLARED SIMPLIFICATION, recorded:
              the analysis calibrates the test's sensitivity to
              score-level persistence, not the full pipeline
index         AR(1) over TRADING-SESSION INDEX within each month
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

PREDECLARED INTERPRETATION RULE: if recorded power at
(rho = 0.5, lambda = 0.5) is below 50 percent, the test still runs
but its non-rejection is uninformative and may not be cited as
evidence against persistence. Power above 50 percent is not thereby
"adequate"; the surface speaks, not a threshold.

## Tier 2: the slow-projection feasibility program

Goal: hand Stage F a one-month slow-confirmation projection meeting
the contract's eight requirements, or RECORD THAT THE FROZEN
FEASIBILITY PROGRAM IDENTIFIED NONE - an open finite search cannot
establish nonexistence over an unspecified projection universe, and
this document does not claim it can.

WHAT LEAVE-ONE-MONTH-OUT COVERAGE IS AND IS NOT, stated before the
procedure because the label is load-bearing: Tier 2 coverage is an
ADAPTIVELY SELECTED INTERNAL DESIGN DIAGNOSTIC. The search tries
candidates against the same months repeatedly, so passing the hurdle
does NOT establish nominal out-of-sample coverage - it is feasibility
screening and calibration evidence. Stage F must account for the
adaptive search when freezing the final projection (conservative
coverage, complexity restriction, or a fully specified predictive
construction), and Stage C is the ONLY untouched validation of
whatever is selected. Per the contract's binding interpretation, the
predictive region is always a frozen predictive distribution
incorporating between-month variation and within-month estimation
uncertainty, never an empirical min-max envelope.

THE ADMISSION HURDLE, frozen numerically NOW so the search cannot
weaken its own judge. A candidate projection is admissible only if
ALL of the following hold:

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

THE FROZEN UNACCEPTABLE ALTERNATIVES for H5, executable as written or
not frozen at all. Both operate on the SESSION-HOUR LOG-RATE RESIDUAL
FIELD `R_sh` as defined in the slow-geometry document, per month. H5
evaluation therefore applies to the projection's session-hour-level
inputs; a candidate requiring finer-than-session-hour inputs must
define, in its own specification, how these perturbations lift to its
input level, or it is inadmissible under H6.

SIMULATION GEOMETRY, shared by both alternatives and matching the
Stage C use: simulation `i` targets the leave-one-month-out fold of
month `m(i)`, months cycling in ascending YYYYMM order over
i = 1..200. The candidate's predictive distribution is fitted on the
remaining months UNPERTURBED; the held-out month is perturbed by the
alternative's construction; the compatibility rule is evaluated on
that perturbed month, and rejection is counted per simulation. Missing
cells stay missing; the perturbation touches only cells that exist;
thin months take their turn in rotation like any other; hour 20 is
treated exactly like every other hour in both constructions. Seeds:
`tuple_mix(STAGE_M_ALT_SEED, [alternative_id, i])`,
`STAGE_M_ALT_SEED = 3958267140192837465`, alternative_id 1 = NO-SLOW,
2 = EXCESS.

NO-SLOW, destroying cross-hour session coherence while preserving
every hour marginal: for each hour `h` of the held-out month,
INDEPENDENTLY permute the values `R_sh` across that month's sessions
`s` (an independent permutation per (hour, simulation), drawn by the
12a 5.1 Fisher-Yates from the seed above with `h` appended to the
tuple). Each hour's marginal distribution is unchanged by
construction; any session-level common mode is destroyed. The
earlier same-month-profile-redraw wording is RETRACTED - it described
a different construction that preserves coherence.

EXCESS, adding a session factor at a frozen magnitude. `W` is a
FROZEN TIER 2 REDUCTION of Tier 1a evidence (Tier 1a reports `R_sh`;
it does not report `W_m`), estimated ONCE, before H5 runs, and
recorded:

```text
u_ms  the UNWEIGHTED mean of R_msh over every existing hour cell h
      of session s in month m, hour 20 included. A session missing
      one or more of that month's measured hour cells is EXCLUDED
      from the W estimation, with its identity and missing count
      recorded - matching the slow-geometry rule that a session with
      an excluded cell refuses its score.
W_m   the POPULATION variance of u_ms across the eligible sessions
      of month m; null with its session count if fewer than 2
      sessions are eligible.
W     the UNWEIGHTED mean of W_m across the NEW-DESIGN months only -
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

BINDING INTERPRETATION AT SIGNATURE: "eligible session" means
eligibility under the complete-hour rule above - a session excluded
from the W estimation receives NO factor and its existing cells are
unperturbed, and the artifact reports the count of such sessions per
simulation. This dilutes the alternative slightly in months with
incomplete sessions, which is the conservative direction for a power
hurdle. Perturbing every session with any existing cell instead would
be a substantive change requiring review before any inspection.

THE DRAW, frozen to the operation because a seed plus "Normal" does
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

CANONICAL SESSION ORDERING, everywhere either alternative iterates
sessions (including the NO-SLOW Fisher-Yates input vectors): ascending
session date. Both constructions, their seeds and the estimated `W`
with its per-month `W_m` table are recorded verbatim in the artifact.

THE INCUMBENT CONTROL RUNS, authorizing what round 1 wrongly banned:
Tier 2 requires evaluating candidates on incumbent-generated months,
and the committed artifacts do not necessarily contain every
projection input (ordered generated session-hour fields, generated
common-mode scores, calendar-gap statistics). INCUMBENT-ONLY
generator runs are therefore authorized under this preregistration:
the shipped generator, unchanged, 24 month-scale walks, seeds
`tuple_mix(STAGE_M_INCUMBENT_SEED, [i])` for i in 1..24,
`STAGE_M_INCUMBENT_SEED = 6172038459284617530`, each walk projected
onto the calendar of a design month in rotation (months ascending,
cycled). These are CONTROL runs, strictly separated from candidate
evaluation: no candidate mechanism ever runs in Stage M, and the
incumbent runs may not be inspected for anything except projection
evaluation. Cost: month-scale walks priced at roughly 25 s each,
about ten minutes total.

PROCEDURE: candidates drawn from the contract's ingredient list
(session-level common-mode score dispersion; calendar-adjusted lag or
gap summaries with adequate support; a sampling-adjusted slow-variance
projection derived from the ordered counts; cross-hour coherence of
session-rate residuals; a bounded excess check), iterated freely,
every candidate tried and rejected recorded with reasons. The program
ends in exactly one of two recorded outcomes: ONE designated
projection specification (statistic vector, predictive construction,
coverage rule, refusal semantics, thin-month treatment) handed to
Stage F, or `no_one_month_slow_confirmation_design`, which stops the
contract before Stage I per its terms.

THE DESIGNATION RULE, predeclared so Tier 2 hands Stage F one object
and not a friendly menu. Among all candidates clearing H1-H6, exactly
one is designated, by this ordered rule: (1) fewest coordinates;
(2) tie: highest incumbent rejection count under H4; (3) tie: highest
minimum rejection count across the two H5 alternatives; (4) tie: the
earliest admissible candidate in the recorded search order. The rule
is mechanical and the artifact shows its evaluation.

THE HANDOFF TO STAGE F, mechanically closed:

- Stage F may freeze the designated projection UNCHANGED; or
- ANY change whatsoever - coordinates, predictive construction, joint
  statistic, coverage region or level, refusal semantics - produces a
  NEW projection that must re-run the ENTIRE H1-H6 hurdle before
  Stage F may freeze it. A changed projection that fails the re-run,
  or does not run it, produces
  `no_one_month_slow_confirmation_design`.
- There is NO pre-authorized transformation. A round-3 draft
  authorized raising the predictive level as "conservative"; that was
  a reversed direction and is RETRACTED - raising predictive coverage
  WIDENS the acceptance region, easing H3 while weakening H4 and H5,
  and no direction-only change to predictive coverage is uniformly
  conservative for both observed coverage and defect detection.
- Stage F may not weaken any hurdle, widen the coordinate cap, select
  among candidates (Tier 2 already designated), or prefer any
  projection for anticipated candidate friendliness.

SEARCH ORDER IS MECHANICAL: it is the chronological order in which a
complete candidate specification is committed to the append-only
Tier 2 artifact BEFORE that candidate's H3-H5 results are computed;
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
and may dominate. The run plan is therefore PER-MONTH INVOCATIONS
with the lock released between months; any single invocation
projected past 20 minutes is cleared with the owner first. The
incumbent control runs add about ten minutes.

## Amendment 1, 2026-08-12: complete-case training in the cross-fit

SIGNED 2026-08-12 by codex session
019ff606-1f16-7800-88ee-e52f083fac70, the fresh reviewer that issued
the underlying conditional ruling after the prior sessions went
cache-cold.

A REVIEWED AMENDMENT with its own signature, not an interpretation:
the frozen slow-geometry construction uses ALL other sessions as
training, and changing that population changes the training moments,
correlation, loading and score. The month-generic refusal clause says
what to do when the formula cannot apply; it does not authorize
inventing a missing-data estimator, so this text goes through the
amendment boundary.

THE TRIGGER, disclosed: the 2026-03 Tier 1a run crashed because
training sessions carry excluded session-hour cells. The cause is
deterministic schedule geometry - the US daylight-saving transition
shifts the CME schedule's UTC hours mid-month, so 2026-03 sessions
before March 8 populate a different UTC hour set (March 2 through 6
carry an excluded hour-22 cell). Retaining the mechanically-correct
fold refusal would make Tier 1b systematically blind to exactly the
calendar-phase variation Stage M exists to characterize.

THE RULE, frozen:

- A training session with ANY excluded cell is removed ENTIRELY from
  that fold - before every per-hour moment and the correlation
  matrix - so the statistics stay internally consistent over one
  population. THE ESTIMAND CHANGES and is named honestly: the
  cross-fit trains on the COMPLETE-CASE training population, not on
  all other sessions.
- The held-out complete-cell requirement is UNCHANGED: an incomplete
  held-out session still refuses its own score.
- At least 12 COMPLETE training sessions must remain after removing
  the held-out session, else that held-out score refuses. TWELVE IS
  SUPPORT-AWARE, disclosed: it was chosen with the March pattern
  known - 18 usable sessions, five incomplete, leaving exactly 12
  complete training sessions per fold - and is not result-driven with
  respect to any score (none existed; the runs crashed first). Its
  substantive basis: a majority of the nominal training population is
  retained and only the leading direction is estimated - no
  covariance inverse, no full-rank requirement, no secondary factor.
  A floor below 12 needs a new stability analysis.
- Drop identities, reasons, the resulting training count and the
  retained training identities are recorded PER FOLD.
- Zero-training-variance and eigensolve refusals are unchanged.
- COMPLETED MONTHS RE-RUN under this amendment, July included as a
  regression check: months without excluded cells must reproduce
  their existing scores EXACTLY.
- NOVEMBER IS NOT RECOVERED, stated against the temptation to claim
  otherwise: 2025-11's held-out sessions themselves refuse on hour
  22, which this amendment cannot rescue. It recovers the
  complete-session portion of March only; the program retains a
  narrower, honestly-recorded DST selection effect.
- Tier 1b support rules are unchanged; a month contributes whatever
  supported bins its scored sessions produce.

THE DST FINDING, recorded now and carried into Stage F as a binding
input: the observed target has a TWICE-YEARLY Central-time-to-UTC
phase transition that the July-fitted generator cannot express.
Complete-case scoring is a missing-data repair, not a model of that
transition. Stage F must decide how calendar phase enters the
successor mechanism and its gates, and must not treat March's
surviving post-transition scores as representing the whole month.

## What Stage M may NOT do

- No candidate generator runs. The ONLY generator runs are the 24
  frozen incumbent control walks above.
- No architecture exclusion, no mechanism proposal, no ranking.
- No acceptance criterion: Stage F freezes criteria after this is
  read, from the owner goal, never from what any candidate passes.
- No re-cutting after inspection - scoped precisely: Tier 1 and the
  Tier 2 ADMISSION HURDLE are frozen; Tier 2 projection FORM iterates
  by design. A new Tier 1 statistic, horizon, month role or stratum
  is a new dated preregistration.
