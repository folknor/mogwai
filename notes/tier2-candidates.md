# Tier 2 candidate projections: the working document

`notes/`-class, and TIER 2 EXPLORATORY by its own nature: this
document records candidate designs and their reasoning BEFORE
evaluation. Nothing here is inference; the admission hurdle H1-H6 in
`notes/stage-m-preregistration.md` is the only judge, and candidates
are committed to the append-only Tier 2 artifact before their H3-H5
results are computed, in this document's order.

REVISION 3, ruled COMMITTABLE by codex session
019ff685-4057-70f2-8d66-38933b9fb3e2. Revision 1 was refused with
four blockers (the false normal-approximation threshold calibration,
the wrong EXCESS direction claim for the persistence coordinate, the
internally contradictory surviving-coordinates rule, the inexecutable
refusal details); revision 2 with three (the overstated EXCESS c1
construction proof, the designation contradiction, a wrong worked
threshold number). C1 and C2 as specified here may be committed to
the append-only Tier 2 artifact in this order.

REVISION 4 adds the C1/C2 outcome record, the bounded continuation
under the adjudication of reviewer session 019ff6c9, and candidate
C3, ruled COMMITTABLE as search order 3 by that session after one
refusal (the misdefined n1 denominator, corrected to the covariance
form).

## What a candidate must be

From the signed contract and prereg: at most 4 coordinates over one
month's session-local residual field and scores (H1); ONE joint
compatibility statistic with a single frozen rejection region (H2);
computable from one month; the JOINT projection detecting both
absence and pathological excess of slow variation - not every
coordinate individually in both tails; rejecting at least 23 of 24
incumbent control months (H4); rejecting at least 160 of 200
simulations of each frozen alternative (H5); complete refusal
semantics including thin-month treatment (H6).

## The shared predictive construction, one definition for every use

Used identically in LOMO folds, incumbent-control evaluation, H5
folds and Stage C; only the reference population changes:

```text
reference    the design months 2025-09 through 2026-04, July
             EXCLUDED (matching the W convention); in a LOMO or H5
             fold the held-out month leaves, n = 7; for incumbent
             controls and at Stage C all eight remain, n = 8
z_k          (c_k - m_k) / (s_k * sqrt(1 + 1/n))
             m_k, s_k the sample mean and sample (n-1) standard
             deviation of coordinate k over the n reference months
T            max over the K DECLARED coordinates of abs(z_k)
threshold    t_{n-1}(1 - 0.10 / (2K)) - the Bonferroni Student
             predictive max rule at joint level 90 percent: each
             z_k is Student t with n-1 degrees of freedom under the
             reference-normal working model, the two-sided budget
             0.10 splits evenly over K coordinates, and validity
             does not assume coordinate independence
verdict      T > threshold rejects (incompatible); T <= threshold is
             compatible
```

THE FORMULA IS THE FROZEN OBJECT; no worked numbers are quoted here,
because a draft of this document misquoted one - the implementation
that evaluates the frozen formula produces and records the thresholds
it uses. H3 then TESTS the empirical coverage of this a priori rule;
it is never used to choose it.

REFUSALS, closing every gap named in review: any non-finite
coordinate, any s_k = 0, any eigensolver failure, or any reference
month whose coordinate cannot be computed REFUSES - and a refusal
anywhere makes the CANDIDATE INADMISSIBLE under H6, because every
declared coordinate must adjudicate for every evaluated month,
simulation and reference month. There is no surviving-coordinates
evaluation: a variable-dimensional projection would judge
refusal-heavy months by an easier test and let missingness remove
exactly the coordinate that detects the defect. C1 is three
coordinates everywhere; C2 is four everywhere. At Stage C, any
declared coordinate refusing on the sealed month maps to INVALID
under the contract's rules, never to a verdict.

## Candidate C1: three-coordinate Student-max compatibility

COORDINATES, per month, from the session-local residual field `R_sh`
(Amendment 2 coordinates):

```text
c1  BETWEEN-SESSION MAGNITUDE: the population variance across the
    month's complete-hour sessions of the session mean residual
    u_s = unweighted mean over the 23 local hours of R_sh (the W_m
    construction; a session missing any hour cell is excluded with
    its count recorded; fewer than 2 eligible sessions refuses)
c2  CROSS-HOUR COHERENCE (leading standardized variance share): the
    leading eigenvalue of the 23x23 Pearson correlation matrix of R
    over the month's complete sessions, divided by 23. Population
    normalization; every one of the 23 hour columns must have
    nonzero variance across the sessions used, else the coordinate
    refuses; the eigensolver is the slow-geometry Jacobi
    implementation with its existing failure rule, failure refusing
c3  ONE-DAY CALENDAR-ORDER COORDINATE: the mean over
    adjacent-calendar-date session pairs (gap exactly 1 day, pairs
    ordered ascending by date, each unordered pair counted once,
    minimum 8 pairs else refuse) of (u_s - ubar)(u_s' - ubar), ubar
    the month mean of u over eligible sessions. Motivated by the
    Tier 1b conditional non-exchangeability result, and stated at
    its true width: Tier 1b rejected on cross-fitted scores over
    four gap bins with weekday preserved, NOT on one-day covariance
    of session means, and no positivity of the sealed month's value
    is assumed - the two-sided predictive rule asks only whether it
    resembles the design months
```

EXPECTED SENSITIVITIES, per alternative, stated as expectations and
not pointwise guarantees except where construction proves them:

```text
INCUMBENT   c1 far below reference (the generator carries almost no
            session heterogeneity), c2 below, c3 near zero:
            rejection expected through c1 and c2
NO-SLOW     c1 falls (per-hour independent permutation destroys the
            coherent contribution to the cross-hour mean variance),
            c2 falls, c3 is driven toward its permutation baseline:
            rejection expected through c1 and c2
EXCESS      c1 rises IN EXPECTATION over the Gaussian draws - the
            sample variance of u + g is Var(u) + Var(g) + 2 Cov(u,g)
            and the realized covariance term can make an individual
            simulation fall, so this is an expected sensitivity,
            not a pointwise construction proof; c2 generally rises
            (the added factor is cross-hour coherent); c3 does NOT
            rise - the added factors are iid across sessions with
            zero population adjacent-day covariance, and centering
            can induce a small negative finite-sample contribution:
            rejection expected through c1, with c2 supporting
```

No independence among coordinates is claimed; c1 and c2 respond to
overlapping structure (c1 keeps scale, c2 discards it), which under
max-z gives that structure more chances to drive the maximum - H5
judges whether the resulting projection does useful work.

## Candidate C2: C1 plus the score-dispersion coordinate

```text
c4  SCORE DISPERSION: the population variance of the month's
    cross-fitted scores f_s over its scored sessions (Amendment 1
    complete-case cross-fit, Amendment 2 coordinates; fewer than 8
    scored sessions refuses)
```

Same construction with K = 4. IN EVERY H5 SIMULATION THE SCORES ARE
RECOMPUTED FROM THE PERTURBED FIELD - evaluating c4 on original
scores after perturbing R_sh would make it unresponsive by
construction. Expected sensitivities: EXCESS raises c4 (the
cross-fitted common direction captures much of the uniform factor,
subject to fold estimation); NO-SLOW lowers it (the recomputed
cross-fit loses its common session mode); the incumbent sits far
below reference.

C2 exists because the common-mode score is the object the slow
component will most directly parameterize; it costs one coordinate
against H1's cap, a larger refusal surface (it needs the cross-fit to
score, which the complete-coordinate rule turns into candidate
inadmissibility if any month refuses), and the recomputation cost in
H5.

## Search order

C1 first, C2 second, committed to the Tier 2 artifact in that order
before any H3-H5 result is computed. If both are admissible, the
frozen designation rule selects C1 UNCONDITIONALLY: fewest
coordinates is the first criterion and C1 has three against C2's
four - the H4 and H5 counts are reached only among candidates tied
on dimension. C2's role while C1 is admissible is therefore
diagnostic comparison only. Further candidates, if these fail, get
their reasoning recorded here before commitment, per the frozen
search-order rule.

## The C1/C2 outcome, and the bounded continuation

Both candidates were evaluated and FAILED: H3 passed cleanly on both
(all eight LOMO months compatible - the predictive construction is
calibrated), but H4 rejected only 3 of 24 and 2 of 24 incumbent
controls against the required 23, H5 NO-SLOW rejected 16 of 200 and
1 of 200 against the required 160, and twelve incumbent control
walks - exactly the winter-calendar rotation, where the shipped
generator's internal July-style UTC offset degenerates the
local-hour-22 stratum to zero variance - refused under the frozen
complete-coordinate rule, failing H6. The adjudication (reviewer
session 019ff6c9) ruled: the twelve refusals STAND against C1 and C2
with no post-hoc reinterpretation, and would not have been
outcome-determinative anyway; the premature designation record is
preserved as history with its terminal interpretation superseded,
since the search was not closed; and the RAW MONTH-SCALAR
STUDENT-MAX FAMILY IS DEAD - the seven-month predictive reference
absorbs the defect because raw between-month regime spread dominates
the shift from destroying within-month coherence.

THE CONTINUATION IS BOUNDED, frozen before C3 is evaluated: exactly
ONE more candidate, C3, search order 3, under the UNCHANGED hurdle,
level, reference months, seeds, alternatives and designation
ordering; only within-month NORMALIZED coordinates; at most three
coordinates, two preferred; every coordinate defined when hour
columns have zero variance; NO C4 FOLLOWS IF C3 FAILS - the terminal
outcome then stands and the contract's named owner exits open under
a new contract.

## Candidate C3: two within-month normalized coordinates

COORDINATES, per month, from the session-local residual field over
the month's COMPLETE sessions (a session missing any hour cell is
excluded with its count recorded; fewer than 2 complete sessions
refuses the candidate for that month):

```text
n1  SESSION-MEAN VARIANCE RATIO, dimensionless. With S the 23x23
    POPULATION COVARIANCE matrix of R across complete sessions (the
    same S as n2):
      numerator    population variance across complete sessions of
                   u_s = unweighted mean over the 23 local hours of
                   R_sh, which equals (1/23^2) * ones' S ones
      denominator  trace(S) / 23^2 - the variance the session mean
                   of 23 INDEPENDENT hours would inherit, each hour
                   at its own across-session variance; defined with
                   zero-variance columns
      n1 = numerator / denominator = ones' S ones / trace(S)
      a zero trace refuses; under hour independence n1 sits at 1,
      and under a perfectly coherent session factor it approaches 23
    Both numerator and denominator scale with the month's volatility
    regime, so the ratio's cross-month spread is the structural
    quantity, not the regime. An earlier draft normalized by the
    WITHIN-session cross-hour variance, which is the wrong axis for
    an independence baseline and was scaled wrongly; the covariance
    form above is the frozen definition.
n2  COHERENCE AGAINST THE WITHIN-MONTH PERMUTATION BASELINE,
    dimensionless: let S be the 23x23 POPULATION COVARIANCE matrix
    of R across complete sessions (columns are local hours, raw
    residuals, never per-column standardized), and
    share = lambda_1(S) / trace(S), the leading eigenvalue share
    under trace normalization - DEFINED even when hour columns have
    zero variance, which is why covariance replaces Pearson
    correlation here. The baseline: 200 within-month permutations,
    each hour column INDEPENDENTLY permuted across the month's
    complete sessions (session vectors ordered ascending by date),
    permutation p of hour h seeded
    tuple_mix(STAGE_M_SEED, [3, YYYYMM, p, h]) with the 12a 5.1
    Fisher-Yates - reserved component key 3, disjoint from the
    Tier 1b key 0 and the power key 1.
      n2 = (share_observed - mean_p(share_p)) / sd_p(share_p)
    with sd_p the sample standard deviation over the 200 permutation
    shares; sd_p = 0 refuses. The eigensolver is the slow-geometry
    Jacobi with its failure rule, failure refusing.
```

NO CALENDAR-ORDER COORDINATE: none of the three defect worlds -
incumbent, NO-SLOW, EXCESS - is separated by calendar ordering
(EXCESS has no persistence by construction, NO-SLOW destroys
ordering along with coherence, the incumbent has neither), so a
third coordinate would add refusal surface and multiplicity cost
with no distinct sensitivity, per the continuation boundary's
two-preferred guidance.

THE JOINT STATISTIC: the shared predictive construction above,
K = 2 - Student max over the two standardized coordinates,
threshold t_{n-1}(1 - 0.10/(2*2)), the formula frozen and the
implementation recording the evaluated thresholds.

EXPECTED DIRECTIONS, expectations not pointwise guarantees except
where construction proves more:

```text
INCUMBENT   n1 near the independence value (the generator carries
            almost no session heterogeneity), far below the observed
            months, whose Tier 1a evidence shows strong session
            structure; n2 near zero because there is no cross-hour
            session coherence to exceed the permutation baseline:
            rejection expected through both, in the LOW tail
NO-SLOW     n2 is DISTRIBUTIONALLY AT THE BASELINE BY CONSTRUCTION -
            the alternative's per-hour independent permutation is
            the same operation as the baseline's, so share_observed
            for a NO-SLOW month is one more draw from the
            permutation distribution and n2 concentrates near zero;
            n1 falls toward the independence value: rejection
            expected through both, in the LOW tail
EXCESS      n1 rises in expectation (the added factor variance is 4x
            the observed baseline; the realized covariance term can
            move an individual simulation); n2 generally rises (the
            added factor is cross-hour coherent): rejection expected
            through n1 primarily, HIGH tail
```

The load-bearing hope, stated so its failure is legible: the
OBSERVED months' n1 and n2 must be far from the incumbent and
NO-SLOW values relative to their own cross-month spread - which is
exactly what normalization exists to arrange, and exactly what the
raw scalars failed. If the observed months' normalized coherence and
variance ratio still vary so much that the predictive band reaches
the independence values, C3 fails honestly and the terminal outcome
stands.

REFUSAL SEMANTICS, complete: fewer than 2 complete sessions, a zero
n1 denominator, sd_p = 0, eigensolver failure, or any non-finite
value refuses - and any refusal anywhere (evaluated month,
simulation, reference month, control month) is CANDIDATE
INADMISSIBILITY under H6, no surviving-coordinate evaluation, a
Stage C refusal mapping to INVALID. In every H5 simulation both
coordinates are recomputed from the perturbed field, the n2
permutation baseline included (its seeds keyed by the simulation's
month as frozen above; the baseline permutations are a deterministic
function of the perturbed field and the frozen seeds). The twelve
degenerate incumbent controls are computable under C3 by
construction: zero-variance columns are defined under trace
normalization, which was a design requirement of the continuation.
