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
