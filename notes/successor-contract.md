# The arrival successor: a staged contract

FROZEN 2026-08-12, SIGNED by codex session
019ff4db-a23c-7bc1-907f-8921b3add799 on round 3. Round 1 was refused
with eight contract-level defects, round 2 with four blockers - the
slow-confirmation gate redesign, blinded atomic Stage C publication,
the partial-pass contradiction, and dual confirmation binding - all
incorporated. Amendments go through the rules in this document, never
edits.

`notes/`-class: transient, no truth guarantee, nothing durable cites it.

## Provenance, stated first because it is the honesty clause

This contract is designed AFTER observing protocol 12b, the count-curve
measurement, the ordered-count panels and the slow-geometry measurement.
Every criterion in it is therefore post-hoc with respect to the July
2026 MNQ TBBO month, which has served as design evidence seven times
(protocol 10 fit, protocol 11 refit, 12a, 12b, count curve, ordered
counts, slow geometry). July can never again be a confirmation
population, and no statistic computed from July confirms anything here.

Confirmation belongs to data this work has never touched. That is what
the sealed-corpus rules below exist to protect.

## What is established, with its evidence

Binding inputs, all committed:

- WITHIN-SESSION: the incumbent generator's Fano factor is FLAT at 31
  to 37 across 1, 5, 15, 60 and 300 s at every hour, where observed
  within-session Fano compounds 9 to 30 times. The incumbent
  over-clusters at one second by up to 6x and under-clusters at 300 s
  by up to 77x. (`notes/count-curve-preregistration.md`, its artifact.)
- The missing covariance lives in the MINUTES-TO-HOUR range: fitted
  single-exponential tail times 278 to 3,277 s across computed hours,
  median near 1,000 s, assumption-conditioned, against an incumbent
  correlation time of roughly 0.6 s.
  (`notes/ordered-counts-preregistration.md`, Panel A.)
- BETWEEN-SESSION: the generated tape carries far less between-session
  Fano than observed - a 25x to 50x shortfall in the ESTIMATED
  component, which at hour 19 at 300 s is 75 percent of the observed
  total. The observed component mixes genuine session-to-session
  heterogeneity with sampling variation from long within-session
  covariance; their division remains model-dependent, and the sampling
  share is material (pooled lower-approximation 19 percent, ranging 7
  to over 100 percent by hour). The slow component is justified; its
  required MAGNITUDE cannot be equated with the raw between-session
  shortfall. (Count-curve decomposition; ordered-counts Panel A.)
- A stable common session-wide mode exists (hourwise permutation
  p = 0.0005, leave-one-out stable) and the residual field has positive
  one-day dependence (p = 0.0015). The common-mode SCORES reject
  exchangeability over the July dates (shared-max permutation
  p = 0.0215) - establishing CALENDAR-ORGANIZED SCORE STRUCTURE and
  identifying nothing narrower. (Ordered-counts Panel B;
  `notes/slow-geometry-preregistration.md` RESULT.)
- THE SLOW COMPONENT'S ARCHITECTURE IS NOT IDENTIFIED. Three classes
  advance: A-only (continuous slow state through boundaries), B-only
  (boundary-associated session factor, possibly autocorrelated), A+B
  mixed. No measurement performed to date can select among them.
- HOUR 20 is a real stratum - deterministic support geometry, higher
  estimation uncertainty, possibly genuine close microstructure - and
  the relevant structure is CALENDAR PHASE AND SEGMENT POSITION, not a
  free parameter for UTC hour 20.
- 12b closed `no-arrival-admissible-candidate-in-frozen-search-space`
  with TWENTY cells (17 log-OU, 3 shot-noise) failing only A3. Those
  cells are EXPLORATORY evidence for this successor and nothing more;
  no criterion here may be tuned to admit or exclude them.

## The central lesson this contract is shaped by

Protocol 12b produced a VALID NEGATIVE in a search space frozen before
the multi-horizon target geometry was known. The measurement succeeded;
the space could not adjudicate the broader all-interval objective. This
contract therefore refuses to freeze a mechanism space now. It freezes
the STAGING, the DATA ROLES and the ANTI-GAMING RULES first, and lets
each stage freeze its own specifics in a dated preregistration once -
and only once - the evidence that stage depends on exists.

## The intervention design: four arms, operationally defined

```text
arm 0  incumbent            the shipped generator, unchanged
arm W  within-session only  may change ONLY the within-session latent
                            arrival process and its fitted parameters
arm S  slow component only  may change ONLY an added slow-state
                            module; the incumbent within-session
                            module and its constants stay identical
arm J  joint                a JOINT configuration, selected as such
```

MODULE OWNERSHIP is part of the freeze: Stage F names the code seams
each arm may touch, and an arm touching anything outside its seam is an
INVALID arm run. RANDOM STREAMS are separated per module so that adding
S does not reorder W's draws; arms use matched seeds and common random
numbers wherever the construction permits; the same measurement
projections and exposure apply to every arm.

Arms W and S are DIAGNOSTIC CONTROLS: they exist so neither component
can hide a defect in the other, and so a compensating-errors story is
visible rather than latent. ACCEPTANCE BELONGS TO ARM J ALONE. No gate
on W or S alone can land a generator change, and no failure of W or S
alone blocks J - their readings are reported beside every J verdict.

JOINT SELECTION IS JOINT: separately selecting a W candidate and an S
candidate does not justify their combination, because nonlinear
interaction between the components is likely. Stage I evaluates joint
configurations AS joint candidates; the frozen arm J configuration is
selected on joint behavior, not assembled from per-component winners.

Arm S is defined against the INCUMBENT base, not a partially repaired
one - moving it onto a repaired base would collapse S toward J and
destroy the intervention contrast. It may well fail the multi-horizon
gates by construction; that is acceptable for a diagnostic control. The
claim is only that S leaves the incumbent within-session module
unchanged; its measured one-second behavior is an OUTCOME, not an
invariant.

The established component requirements, restated so no arm forgets
them: the within-session repair is a REDISTRIBUTION of clustering
across scales (reduce the one-second excess AND add
minute-through-five-minute covariance); the slow component must express
day-scale heterogeneity that survives the within/between decomposition.

## Staging

```text
Stage M   design measurement    on the DESIGN months. Replicates the
                                count curve, ordered counts and slow
                                geometry across the design population;
                                re-runs the full relevant 12a observed
                                measurement per month; runs the
                                calendar-adjusted persistence test
                                under its own frozen preregistration.
Stage F   freeze                mechanism search space, parameter
                                grids, the multi-horizon gates, the
                                sub-second criterion, the acceptance
                                experiment and every tolerance -
                                frozen in a dated, signed
                                preregistration AFTER Stage M is read
                                and BEFORE any candidate is evaluated.
Stage I   implement and select  candidates evaluated under the Stage F
                                contract on DESIGN data only. Produces
                                the frozen arm J configuration,
                                selected jointly.
Stage C   confirm               the sealed primary-confirmation month
                                is opened and arm J is judged against
                                it under the Stage F gates, once.
```

Each stage's preregistration goes through the same spar-to-signature
review as this document. A stage may not begin until its predecessor's
outcome is recorded.

STAGE M OBLIGATIONS, fixed now:

- The calendar-adjusted persistence test freezes its null, statistic,
  calendar adjustment, multiplicity rule, target effect and
  architecture-selection consequences BEFORE any design month is read,
  together with a power or minimum-detectable-effect analysis. "More
  sessions" is not automatically "adequately powered", and the test's
  adequacy is a computed claim, not an assumed one.
- ARCHITECTURE EXCLUSION NEEDS A SUFFICIENT TEST. Evidence for a
  boundary component cannot exclude a continuous one; failure to
  detect persistence cannot establish iid session factors; a
  calendar-adjusted rejection may identify calendar organization
  without identifying a stochastic state law. A class leaves the
  advancing set only under a preregistered test whose rejection
  logically excludes it.
- Per-month AND combined estimates are reported; months are never
  pooled into one target that erases regime variation.
- Stage M results are reported with and without July (`spent-design`),
  so nothing quietly leans on the month every criterion was designed
  against.

STAGE F REQUIRED CONTENT - the acceptance experiment, not merely
thresholds. Stage F must freeze at minimum:

- generated seed count, seed derivation, and exposure per seed
- the unit of replication (seed, session, session-hour, window)
- how observed uncertainty and generated variability combine into a
  verdict
- EQUIVALENCE MARGINS, not merely one-sided thresholds
- simultaneity across horizons, hours, components and ladder gates,
  with the multiplicity treatment stated
- minimum support and refusal semantics per statistic, each refusal
  mapped in advance to a verdict consequence
- whether PASS requires every hour, a frozen fraction, or a
  simultaneous aggregate - and how hour 20 enters the overall verdict
- exact treatment of zero probability, count mean and upper count
  tails
- the consequence of an observed confirmation estimate whose
  confidence region is too wide to adjudicate an equivalence gate -
  decided AT STAGE F, informed by a power check against design months,
  never improvised at Stage C
- what happens when the observed design-month target distribution
  differs materially from July's frozen 12a targets: the ladder stays
  the unamended regression floor, and the multi-month Stage M
  measurements are the design target; neither silently substitutes
  for the other

## Data: the subscription manifest and the seal ledger

OWNER RULING, 2026-08-12: one Databento Standard subscription month
(199 USD) is authorized, superseding the credit-first policy for this
purchase only. The plan includes the trailing 12 months of L1 history
(TBBO and MBP-1 included) for CME Globex with no volume cap and no
license fees; the lookback window ROLLS, so everything wanted must be
pulled while the subscription is active.

THE MANIFEST, pulled in full during the subscription month, every file
hash-verified and ledger-bound:

```text
MNQ TBBO    every month available in the L1 window at pull time
ES  TBBO    same range   - MES-borrow track, separate contract
MES TBBO    same range   - MES-borrow track, separate contract
MNQ MBP-1   same range   - separate manifest and ledger, all sealed;
ES  MBP-1                  no content read under this contract; any
MES MBP-1                  future use needs its own contract. Rationale:
                           the rolling-window loss is irreversible and
                           sealed bytes are reversible. TBBO already
                           carries top-of-book, so MBP-1 is preserved
                           depth evidence for a future question, not a
                           requirement of any present one. The MBP-1
                           pull must not delay or endanger the TBBO
                           pull, and yields to it on any conflict.
```

ROLES ARE ASSIGNED NOW, IMMUTABLY, BEFORE ANY CONTENT READ - not at
Stage F, because selecting a confirmation month after seeing design
results would permit calendar or regime matching. The closed role set:

```text
spent-design           design evidence already consumed pre-contract
new-design             open for Stage M and Stage I
primary-confirmation   sealed; the Stage C month
reserve-confirmation   sealed; replaces primary ONLY on an INVALID
                       outcome that contaminated the primary; never
                       rescues a scientific FAIL
unused-sealed          sealed; may become design evidence only under a
                       FUTURE contract, never confirmation for this one
```

THE MNQ ASSIGNMENT:

```text
2026-07                  spent-design
2026-06                  primary-confirmation
2026-05                  reserve-confirmation
2025-08 .. 2026-04       new-design
```

THE RESERVE IS 2026-05, settled in round 2. The reserve exists to
substitute for June as fairly as possible, not to maximize design
sample size: the slow component concerns session-scale rate structure
and calendar organization, which drift across contract rolls,
volatility and participation regimes and seasonal calendar structure,
so the replacement month must minimize temporal extrapolation relative
to the month it replaces. Ending design at April makes every
confirmation strictly FORWARD of the design window - a feature - and
the remaining design population of roughly nine months plus
spent-design July is nowhere near the original 22-session problem.
Immutable from signature.

ES and MES months are sealed under the master acquisition ledger and
belong to the MES-borrow track, which writes its own fully separate
contract - shared bytes, separate science. Nothing signed here
authorizes any ES/MES content read.

THE SEAL LEDGER records, per delivered file: dataset, schema,
instrument, month, content hash, delivery job, delivery timestamp,
role, role-assignment timestamp, the hash of the contract authorizing
the role, first-content-read timestamp, unseal authority, the reading
process, artifact outputs, and final state.

WHAT COUNTS AS A CONTENT READ: parsing, decompressing, schema
validation, row counting, metadata inspection or preview generation of
a sealed file IS inspection. Byte-hashing the opaque delivered file is
not. A sealed month's first content read is its unseal, and an
unauthorized one is CONTAMINATION, recorded as such.

## The gates, in outline only

Frozen numerically at Stage F, not here - but their SHAPE is fixed now
so Stage F cannot quietly narrow the target back to a single horizon:

- MULTI-HORIZON COUNT GATES as a SIMULTANEOUS EQUIVALENCE construction
  over the WITHIN-SESSION count curve: both the absolute level AND the
  adjacent-horizon growth of the dispersion curve are constrained.
  Absolute level alone can pass a wrong curve; growth alone can pass a
  uniformly displaced one. The slow component is judged by its own
  gate, below, never by raw between-session Fano equivalence.
- Stated plainly: no single horizon can ESTABLISH realism alone, but a
  frozen failure at any gated horizon DOES reject. Rejection is
  per-horizon; acceptance is joint.
- THE SUB-SECOND CRITERION: the successor to 12b's A3, designed at
  Stage F from the Stage M evidence, with the 12b twenty-cell residue
  disclosed as exploratory context in the freeze document. It judges
  one-second composition as one point ON the horizon curve, never as
  the sole gate.
- HOUR-20 STRATUM: every gate reports it separately; no gate pools it
  with ordinary hours.
- THE 12a REALISM LADDER stays green and unamended as the REGRESSION
  FLOOR - necessary, not sufficient, and never the multi-month realism
  target.
- SLOW-COMPONENT DIAGNOSTICS carried without inference: the score-gap
  curve is reported for every arm; no two-day oscillation is encoded;
  no architecture class is excluded except under Stage M's
  preregistered exclusion tests.

THE SLOW-CONFIRMATION GATE, settled in round 2. Raw between-session
Fano is a MIXED ESTIMAND on any single month - genuine slow
heterogeneity plus sampling variation induced by within-session
covariance, unidentified at the one-hour boundary - so an equivalence
margin wide enough to survive that uncertainty would be scientifically
vacuous, and a power calculation cannot manufacture identification.
Stage C therefore confirms the slow repair through a preregistered,
LOW-DIMENSIONAL, ONE-MONTH PREDICTIVE PROJECTION: a predictive
COMPATIBILITY gate, never labeled population-parameter equivalence.
Stage M supplies the across-month reference distribution of the
projection; Stage C asks whether the sealed month and the generated
month ensemble are compatible under it. Stage F must define a
projection that:

- is computable from one month
- retains session ordering and calendar structure where relevant
- has an observed sampling distribution estimated from the design
  months WITHOUT treating hours as independent replicates, and
  REFLECTING month-to-month regime variation - so that a rejection
  means outside the observed range of months, not merely different
  from their mean. BINDING INTERPRETATION, attached at signature:
  "observed range of months" means the preregistered PREDICTIVE
  DISTRIBUTION across months, including sampling uncertainty and
  regime variation - NOT the literal empirical minimum-to-maximum
  envelope, which is outlier-sensitive and provides no controlled
  predictive coverage. Stage F freezes that predictive construction
  and its coverage rule.
- is evaluated identically on observed and generated month-scale
  samples
- detects both ABSENCE and PATHOLOGICAL EXCESS of slow variation (a
  bounded excess check, so an arbitrarily large slow component cannot
  pass a mere presence test)
- CANNOT pass the incumbent with high probability under the design
  evidence
- has demonstrated power against scientifically unacceptable
  alternatives, not merely against exact zero
- remains separate from the within-session count-curve gate

Candidate ingredients, not yet frozen: session-level common-mode score
dispersion; calendar-adjusted lag or gap summaries with adequate
support; a sampling-adjusted slow-variance projection derived from the
ordered counts; cross-hour coherence of session-rate residuals. The
generated ensemble may be widened to control Monte Carlo error; that
is ancillary and never counts as additional observed information.

IF STAGE M CAN PRODUCE NO SUCH PROJECTION, the contract STOPS BEFORE
STAGE I with the named outcome
`no_one_month_slow_confirmation_design`: the one-month confirmation
architecture cannot validate the joint repair. The choices that
outcome opens all require a NEW signed contract - redefining June and
May jointly as a two-month confirmation population, acquiring or
preserving a different multi-month holdout, or narrowing the landing
claim to a within-session repair with no accepted slow component. A
design-only slow component may NOT land under this contract's PASS:
judging the slow half on design data alone while PASS sounds like
confirmation of the whole joint repair would contradict the reason the
sealed month exists.

## Stage C: execution binding and outcomes

BOUND BEFORE UNSEAL, recorded in the Stage C artifact: candidate
configuration hash, source commit, built binary hash (bindable only
here, since implementation follows Stage F), preset and fingerprint
hashes, `TAPE_PROTOCOL_VERSION`, gate implementation hash, gate
contract (Stage F document) hash, seed list and exposure, expected
output paths, execution host and toolchain provenance, and a
mechanical preflight outcome that reads NO confirmation content.

BOTH CONFIRMATION PATHS ARE BOUND BEFORE JUNE IS UNSEALED, because the
reserve cannot run under literally identical binding - its input hash,
calendar dates, session count and support geometry necessarily differ:

```text
primary binding   June input hashes and mechanically derived schedule
reserve binding   May input hashes and mechanically derived schedule
shared binding    candidate, binary rules, gates, seeds, exposure,
                  estimators, tolerances, refusal mapping
```

May may NOT receive a newly chosen exposure, support threshold,
month-specific tolerance or calendar treatment after June is
contaminated; if a frozen rule is structurally inapplicable on May,
its already-frozen refusal consequence governs. Stage C runs once per
eligible confirmation month, at most twice in total.

BLINDED ATOMIC PUBLICATION, so the unspent-versus-contaminated
distinction is mechanical rather than judged: the Stage C harness
withholds ALL confirmation-derived values until (1) input and seal
checks pass, (2) execution completes, (3) every required statistic
adjudicates or reaches its frozen refusal rule, (4) artifact
completeness and integrity checks pass, and (5) the verdict is
mechanically committed. Until that point, external output carries only
progress and non-content-bearing error codes - no per-hour estimate,
histogram, partial gate reading, log line containing market-derived
values, or inspectable intermediate artifact.

```text
PASS      every Stage F gate adjudicates and passes on the sealed
          month.
FAIL      every required gate adjudicates under its frozen rules and
          at least one rejects. FAIL is scientific: it can only be
          produced by adjudicated statistics, never by plumbing.
INVALID   the run does not produce a scientific verdict: seal or
          content-hash mismatch; wrong binary, candidate, preset,
          seed set, exposure or gate artifact; implementation failure;
          a required statistic refusing without a predeclared verdict
          rule; insufficient observed or generated support outside the
          frozen rules; partial execution or corrupt output.
```

If every required gate adjudicates and at least one rejects, the
outcome is FAIL, regardless of how many others pass. If any required
gate does not adjudicate and its frozen rule does not convert that
condition into a scientific rejection, the outcome is INVALID. A
refusal may map to FAIL only where Stage F establishes that the
refusal itself is scientifically meaningful; a computational inability
never is.

INVALID AND CONTAMINATION are distinct ledger states, decided by the
blinded-publication rule above:

- Failure BEFORE atomic verdict publication, with no derived value
  externally observable: the month is NOT spent. The defect is
  repaired under the amendment rules and Stage C re-runs against the
  SAME month. An authorized content read by the blinded harness alone
  does not spend the month; human or adaptive-process access to
  derived content does.
- Any derived value made observable before failure: the month is
  CONTAMINATED and spent without a verdict. Stage C moves to the
  reserve-confirmation month, once, under the pre-bound reserve
  binding and the shared verdict procedure.
- FAIL spends the month, always. The reserve NEVER rescues a FAIL.

WHAT PASS AUTHORIZES: preparation of the landing, not the landing
itself. The landing then requires: complete implementation tests, the
unconditional `TAPE_PROTOCOL_VERSION` bump, re-blessed deterministic
artifacts where output legitimately moved, a preset provenance ledger
for every new knob, a clean full gate (`brokkr check --gate`),
verification that non-MNQ presets did not change unintentionally, and
the recording of the confirmation month as spent.

WHAT FAIL AUTHORIZES: recording the failure verbatim; NO landing; the
confirmation month is spent. A second confirmation attempt needs a new
untouched population, which this contract does not pre-authorize.

## Amendments and restarts

- Before any candidate output is exposed, a defect amendment may
  replace Stage F. Replacing Stage F INVALIDATES every candidate
  evaluation performed under the old freeze; Stage I restarts from
  scratch under the replacement.
- Any Stage F change after Stage I candidate selection requires
  candidate RESELECTION under the changed freeze.
- Once any valid Stage C statistic is exposed, no amendment can
  preserve confirmation status - there is no amended re-read of an
  exposed confirmation month.
- Any change to the candidate, implementation, gate code or execution
  binding after unseal spends the month; re-confirmation on it is
  forbidden.
- Every amendment is reviewed, names what changed, and argues
  non-result-drivenness explicitly; the restart rules above apply
  regardless of how good the argument is.

## Anti-gaming, inherited and extended

- No estimator, bin, threshold, tolerance, grid or seed change after
  seeing results, at any stage, outside the amendment-and-restart
  rules above.
- Nothing may select the friendlier architecture, family, criterion or
  confirmation population after seeing which is friendlier.
- The twenty 12b cells may inform Stage F qualitatively and are named
  there; no Stage F constant may be derived from their parameter
  values.
- Every stage records refusals with reasons; a statistic that cannot
  be computed is null with its count, never zero, never silently
  dropped.
- Determinism per binary and green statistical gates remain the
  correctness contract; bit-reproducibility across toolchains is not
  promised and not required.

## Amendment 1, 2026-08-12: partial-month delivery and seal channels

Recorded BEFORE the pull, in response to vendor entitlement metadata
only - no market data was inspected, no population chosen for
friendliness, no threshold or statistic moved. Prompted by the mapped
subscription-window edge: the trailing-12 boundary slices 2025-08
mid-month (covered back half quotes 9.41 for the rest), and 2026-08
exists partially to the pull morning.

1. INCOMPLETE MONTH DELIVERY, defined: a month whose acquisition
   interval is truncated by a vendor entitlement or availability
   boundary is `incomplete_month_delivery`, and is treated as an
   UNDELIVERED FULL MONTH for every Stage M population purpose. It is
   NOT a thin month: thinness is few usable sessions in a delivered
   month under the 12a rule; entitlement truncation is procurement
   absence, and admitting a slice would give it equal month weight at
   half exposure and substitute a different calendar population after
   signature - any short slice with 15 clean sessions could otherwise
   masquerade as a month. The covered bytes MAY be pulled and
   preserved (the loss is irreversible, sealed bytes are not),
   recorded with role unchanged, delivery state, exact covered
   interval, edge timestamp and opaque hash - and remain UNREAD and
   unavailable to Stage M unless a later reviewed amendment defines a
   partial-month estimand and re-runs every affected pre-content
   calculation. 2025-08 is the recorded instance. The uncovered
   portion is NOT purchased.
2. 2026-08, absent from the immutable assignment table, is added by
   this amendment: partial TBBO and partial MBP-1, role
   `unused-sealed`, coverage end at the exact vendor timestamp,
   content read forbidden, future use by a new signed contract only.
3. SEAL CHANNELS, clarified after a near-miss: the seal protects
   INFORMATION, not file handles. Any channel yielding per-session
   observables of a sealed month - vendor metadata such as per-date
   record counts explicitly included - is INSPECTION under the
   content-read rule. Per-session record counts are session-level
   activity data, the very observable the slow-confirmation
   projection is built from. Entitlement and cost metadata, which
   yield a price and not a per-session observable, are not
   inspection; neither is byte-hashing. The design-month calendar
   sweep therefore covers new-design months and spent-design July
   only; the confirmation months' schedules materialize exactly once,
   mechanically, inside the blinded Stage C harness at unseal.

## Amendment 2, 2026-08-12: the assignment-rooted seal ledger

Recorded after the acquisition track found that the seal ledger as
originally specified - one row per DELIVERED file - cannot represent a
category of action the contract itself classifies as inspection.
Amendment 1 provision 3 made the seal INFORMATION-level and named
vendor per-date record counts an inspection channel; the authorized
design-month calendar sweep then inspected months whose files are not
yet delivered, and the file-keyed ledger had no row to record it
against. The sharper corollary that forces the repair: the same sweep
against sealed June would be genuine contamination, and the ledger
could not record even that - the detection instrument was blind
exactly where bytes have not landed. Nothing improper occurred; the
audit record could not represent an authorized action and would have
misreported a later one (`first_content_read_at` stamped on a
delivered file for a month already inspected through another channel).

NON-RESULT-DRIVEN, stated explicitly: no role, population, threshold,
statistic or scientific consequence changes. This amendment changes
the contract's ledger field list and record granularity, which is
contract text - calling it an implementation correction would evade
the amendment rule.

THE STRUCTURE. The ledger is re-rooted on ASSIGNMENTS, because
contract obligations attach at role assignment - which has existed
since signature - not at delivery. Per-file records remain
first-class children; they are not collapsed into month records.

- ASSIGNMENTS, immutable: assignment identity; contract track (this
  contract, the MES-borrow track, the MBP-1 preservation ledger);
  dataset; instrument; month; role; role_assigned_at, which is the
  CONTRACT ASSIGNMENT TIME and is never rewritten to a migration
  date (assignment_recorded_at may differ); the authorizing
  authority identity; a nullable supersedes reference so a future
  contract can assign a new role to preserved bytes without erasing
  history. Schema is NOT folded into the role: permissions derive
  from assignment plus delivery or channel schema, preserving that a
  new-design month's TBBO is open while its MBP-1 stays sealed.
- DELIVERIES, append-only with controlled state progression, child of
  an assignment: delivery identity, schema, vendor object identity,
  content hash, delivery job, delivered_at, delivery state, covered
  interval and edge timestamps.
- INSPECTION EVENTS, append-only, child of an assignment: event
  identity; nullable delivery reference - REQUIRED for a
  delivered-file channel, FORBIDDEN for an undelivered
  vendor-metadata channel; channel from a CLOSED VOCABULARY
  (initially delivered-file-content, vendor-record-count, and other,
  which requires a description and review); scope; observed_at and
  recorded_at as SEPARATE timestamps, because retrospectively
  discovered contamination is recorded later than it occurred;
  authority; reading process; authorization basis and verdict;
  artifact references by immutable identity (hashes, never only
  paths). Multiple events per (assignment, channel) are possible.
  An UNAUTHORIZED event must remain recordable: retrospective event
  recording is a SEPARATE operation from prospective read
  authorization and must not call the same guard. No deletion, and
  no in-place conversion of an unauthorized event into an authorized
  one.
- DERIVED STATE, never independently editable, and not one exclusive
  enum - spent, contaminated and inspected are different dimensions
  and a single state machine erases history. The ledger derives at
  least: has_any_inspection, has_unauthorized_inspection,
  first_inspected_at, first_unauthorized_inspection_at, and the
  scientific disposition. Per delivery, first_content_read_at is
  DERIVED from the earliest delivered-file inspection event of that
  delivery (or retained as a checked denormalization with exactly
  that meaning); per assignment, first_inspected_at derives from all
  inspection events.

AUTHORITY IDENTITIES ARE IMMUTABLE SNAPSHOTS. A contract hash may
never mean "hash the mutable file on disk": landing this amendment
changes that hash and would make every earlier authorization appear
invalid. The ledger records the hash of the exact signed base
contract snapshot, the hash of each signed amendment, and the
explicit chain of authorities applicable to each record. Existing
assignments remain authorized by the signed base contract plus
Amendment 1 where applicable; Amendment 2 authorizes the
representation and the migration.

THE ONE-TIME MIGRATION, authorized here: assignment roots are created
from the immutable signed assignment table, and the already-executed
calendar sweep is imported as inspection events from its own
artifact. The artifact records one artifact-level generation time and
does NOT record per-request observation timestamps, acting authority
or reading process - so the migration must not claim to preserve
what was never recorded. The honest import: `observed_at` is set to
the artifact's `generated_at` and LABELED an upper-bound proxy, not
an actual observation time; authority and reading process are either
reconstructed from separately named evidence with that provenance
recorded, or entered as unknown with a migration-specific reason; the
channel and the artifact hash import as recorded. No delivery
timestamp is borrowed and the sweep is never represented as having
read a delivered file.

FOR 2026-05 AND 2026-06 the migration records a BOUNDED AUDIT
CONCLUSION, not a proof - enumeration of recorded channels can
establish that no evidence of inspection was found, never that no
unrecorded process ever occurred, which is precisely the threat model
behind retrospective contamination recording. The frozen scope: every
identified tool, cache, artifact, and available vendor request
history or log is enumerated, the inventory and the result are
recorded, and the two months remain eligible confirmation
populations only if that exhaustive audit finds no inspection
evidence.

FROZEN CONSEQUENCES, conditions of the countersignature:

- Creating assignment rows does NOT make an undelivered month part of
  the delivered Stage M population.
- Stage M eligibility remains complete delivered MNQ TBBO, with the
  incomplete-delivery and minimum-population rules unchanged.
- Authorized design-channel inspection does not imply any delivered
  file was read.
- Any unauthorized inspection of a sealed assignment contaminates the
  month regardless of delivery existence.
- A contamination record may be entered retrospectively, with
  evidence, under separate occurrence and recording timestamps.
- 2026-05 and 2026-06 remain eligible confirmation populations only
  under the bounded audit conclusion above.

## Amendment 3, 2026-08-12: aggregate metadata and reconstruction closure

A RESULT-AWARE INTERPRETIVE AMENDMENT, labeled honestly: the
Amendment 2 migration audit found SIXTEEN record-count requests
overlapping 2026-05 or 2026-06 before this question had an answer, so
the facts were known when this text was written, and calling it
non-result-driven would be false. Its admissibility rests on the
safeguards at the end of this section, foremost that the reviewer and
not the implementer froze the criterion, and that the criterion binds
symmetrically whichever way it falls.

THE FACTS. All sixteen requests are aggregate counts over 21 to 31
day spans - single integers, zero per-date requests - made 2026-08-04
and 05, a week before signature, by the pricing sweep, covering MNQ
and NQ across tbbo, trades, definition and statistics.

THE RULING (reviewer session 019ff53d):

- A WHOLE-MONTH AGGREGATE RECORD COUNT IS NOT INSPECTION under
  Amendment 1 provision 3. The ground is INFORMATIONAL GRANULARITY
  alone: one aggregate scalar exposes no within-month allocation.
- PRE-SIGNATURE TIMING DOES NOT INDEPENDENTLY EXCUSE an inspection.
  Had these requests yielded prohibited information, the later role
  assignment could not cleanse it.
- THE NARROW CONSISTENCY PRINCIPLE, and only it: a channel expressly
  exempted by the contract does not become inspection merely because
  another non-inspection aggregate is informationally comparable to
  it. NO general exemption follows for monotone transforms - a
  monotone image can be injective and exactly invertible, and whether
  an aggregate preserves, reduces or reveals information depends on
  its resolution, formula, auxiliary knowledge and domain.

THE FROZEN RECONSTRUCTION-CLOSURE CHECK, which alone adjudicates the
sixteen and any future recorded metadata against a sealed month. Its
input is EVERY recorded count request in the same observable domain -
not merely requests touching the sealed month, because a request
wholly outside it can complete an equation with an overlapping one.

```text
domain      endpoint semantics, dataset, complete symbol expression,
            schema, stype and every parameter affecting the counted
            population; domains are never combined merely because
            their values correlate
partition   time is cut at every recorded request boundary and at
            the sealed-month boundaries, yielding atomic intervals
vectors     each count request is an exact incidence vector over the
            atomic intervals
test        does the RATIONAL ROW SPAN of all recorded requests in
            the domain contain the incidence vector of any nonempty
            PROPER SUBSPAN of the sealed month - a subspan being ANY
            UNION of atomic intervals wholly inside the month, not
            only a contiguous interval, so disjoint session groups
            cannot slip through
witness     a containment is reported with its coefficients and the
            derived interval set
refusals    uncertain endpoint additivity, unresolved time-boundary
            semantics, malformed parameters or incomplete cache
            enumeration REFUSE the check - fail closed, never guess
binding     the exact cache hash and the checker implementation hash
            are bound into the verdict
```

THE SIXTEEN are recorded in the ledger as AUTHORIZED PRE-CONTRACT
PRICING EVENTS if and only if the mechanical check independently
reproduces the reviewer's reading of the recorded bounds (June NQ
month totals coexisting with a June-21-to-July-12 window and July
totals confound the June tail with the July prefix rather than
isolating a June subspan; the MNQ June domains lack any isolating
overlap; May has whole-month observations only). If the check finds a
containment, the affected month is CONTAMINATED, recorded
retrospectively under Amendment 2, WITHOUT another interpretive
round.

SAFEGUARDS, conditions of the countersignature:

- The ledger remains failed closed until the check runs.
- The reviewer, not the implementer, froze this criterion.
- The criterion applies symmetrically whether it preserves or
  contaminates 2026-05 and 2026-06.
- The actual requests are adjudicated ONLY by the frozen mechanical
  check.
- A failure contaminates without further interpretation; a pass
  restores eligibility but erases neither the events nor the audit
  history.

## Signature record

SIGNED on round 3 by codex session
019ff4db-a23c-7bc1-907f-8921b3add799, 2026-08-12, with one binding
interpretation recorded in the slow-confirmation gate section. The
signing verdict, verbatim: round 3 has no remaining blocking defect;
May 2026 is accepted as the immutable reserve confirmation month; raw
between-session Fano equivalence is correctly excluded from Stage C; a
design-only slow repair cannot land under this contract's PASS.

Amendment 1 signed same day, same session - see its section for
scope. Its countersignature attached one interpretive note on
provision 3: before unseal, Stage C prebinds the schedule DERIVATION
ALGORITHM and the input identity, never the resulting data-dependent
schedule, which is derived atomically inside the blinded execution.

Amendment 2 signed 2026-08-12 by codex session
019ff53d-5b7d-77c2-b759-4d7d7834c7d6 - a FRESH reviewer session that
took over after 019ff4db's cache went cold, grounded itself in the
signed documents, and refused twice (the month-keyed-collapse shape
it replaced with the assignment-rooted hierarchy; then the
unpreservable migration fields and the overclaimed May/June proof)
before signing. The session's response text misprinted its own
identity; the ID here is taken from the captured session envelope.

Amendment 3 signed 2026-08-12 by the same session, 019ff53d, after
one substantive round in which it ruled the sixteen aggregate counts
non-inspection on informational-granularity grounds, rejected the
proposed general monotone-transform principle as unsound, required
the closure check to range over all same-domain requests with
subspans as arbitrary interval unions, and required the result-aware
label in place of a false non-result-driven claim.
