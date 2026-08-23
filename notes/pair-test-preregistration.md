# Wave 1 preregistration: is NQ a usable MNQ proxy?

Frozen 2026-08-05, before any delivered byte is read. The delivered files of
job `GLBX-20260805-JUBCRPRLG8` (`analysis/databento-jobs.json`, wave 1 of
`DATA-PURCHASE-REPORT.md` section 9.7) remain sealed until section 6's order
of work reaches them. Everything decision-shaped in this document - targets,
estimators, tolerances, mandatory families, the aggregate rule, the verdict
semantics - is fixed here first, so no observation can quietly become a
criterion. The discipline follows `notes/sampling-frame-preregistration.md`,
including its recorded failure (section 3.9: estimator conformance landed
after observation); here conformance comes first, by order of work.

**The machine-readable half is `analysis/pair-test-preregistration.json`,
frozen and committed together with this document.** The pair harness loads
that file rather than mirroring constants in code, the
`spread_conformance.json` pattern: neither prose nor code can quietly
redefine the rule. A divergence between the two files is itself a
deviation.

## 1. The question, and what the verdict controls

One question: **can NQ trade evidence stand in for MNQ trade evidence in the
places wave 2A would use it** - the long-memory, persistence and
process-shape families? Both legs of the delivered window are the same
market, same sessions, same index, same 0.25 tick; they differ in contract
size and participant mix. If their process shape agrees within the
tolerances below, four months of NQ history can back MNQ's shape targets; if
not, it cannot, and no quantity of NQ data fixes that.

The verdict controls exactly one thing:

- **FAIL permanently skips wave 2A.** The NQ contiguous basket is not
  bought, not redirected, not revisited under this program.
- **PASS unlocks wave 2A but does not authorize it.** Authorization is a
  separate explicit instruction, per the standing rule.
- **Wave 2B is available under either verdict**, because the per-instrument
  quote seams need MNQ quotes regardless; it waits on its own groundwork,
  pricing and authorization and is not touched by this document's rule.

Data insufficiency is a FAIL, not a third state: wave 2A money must not
move on an unproven proxy. The failure is recorded as
`fail (insufficient data)` so the reason is not laundered into a
measurement.

**Excluded from the verdict by construction: every quote and spread claim.**
Wave 1 is trades only. Quoted width, effective spread, top sizes and
displacement-versus-book are out of scope here and belong to wave 2B
evidence; any spread-flavored number computed from trades alone (Roll-style
estimators) is already established as unusable for calibration and does not
appear below.

## 2. Unit of analysis, sessions, and boundaries

The unit is the **CME session** (trade date): 17:00 previous day to 16:00,
America/Chicago, with the 15:15-15:30 maintenance halt, per the corrected
calendar in `mnq.toml`. The delivered window spans the sessions dated
2026-07-06 through 2026-07-17: **10 expected sessions**, no CME holiday
inside.

- `EXPECTED_SESSIONS = 10`
- `MIN_USABLE_SESSIONS = 8` - below this, the verdict is
  `fail (insufficient data)`.

Boundary rules, fixed now:

1. A trade is assigned to a session by its event timestamp against the
   session bounds; trades outside every declared open window (including
   inside the halt) are counted and reported, and excluded from session
   estimators.
2. A parent gap that spans a calendar-closed interval (the halt, or a
   session boundary) is excluded from every duration target - it measures
   closure, not cadence - and the exclusion count is reported. No gap
   crosses sessions: each session's estimators see only that session.
3. **Continuous-contract handling.** Both legs were bought as `v.0`. The
   preflight records the distinct underlying contracts per leg per session.
   The window sits after the 2026-06-19 quarterly expiry, so ONE contract
   per leg is expected throughout. If a second contract appears in a leg,
   every session containing the minority contract is excluded fail-closed
   and named; exclusions dropping usable sessions below
   `MIN_USABLE_SESSIONS` fail the verdict per section 1.

## 3. Estimators, imported not reinvented

The estimators are the ones the fingerprint lineage already validated,
imported unchanged: `EventStats` (from `probe_binance_trades`, side-aware
grouping) and `AutoCorr` (from `probe_binance_aggtrades`, restricted lags
where only specific lags are read - the bit-identity of that option is
already pinned by `conformance_f3_f6.py`). New glue (leg splitting, session
assignment, halt-gap exclusion, the side-run statistics) gets its own
synthetic-fixture conformance before any real byte is read - section 6.

Parent/sweep inference, identical to the recorded rule of report 11.1:

- A parent groups contiguous rows sharing event timestamp and aggressor
  side, within one leg. Non-contiguous rows sharing a timestamp are
  separate parents.
- Timestamp resolution is observed and reported, never assumed. The event
  clock is the exchange event timestamp; the receive timestamp is not used
  for grouping.
- The aggressor side comes from the schema's side field. Rows whose side is
  neither buy nor sell are counted, reported, and excluded from side-aware
  targets; if they exceed `MAX_UNSIDED_FRAC = 0.01` of a session's rows,
  every side-dependent target is unavailable for that session rather than
  computed on a filtered stream and presented as whole.
- Where the schema cannot support an estimator at all (a required semantic
  field absent), that target is unavailable - never zero, never improvised
  from a different column. Unavailable targets fail, per section 5.

## 4. Targets, families, tolerances

Per session and per leg, fourteen targets in six families. For each target,
the session-paired discrepancy is:

- ratio class (positive-valued): `d(s) = log(NQ(s) / MNQ(s))`
- difference class (fractions and correlations): `d(s) = NQ(s) - MNQ(s)`

```
RATIO_TOL        = log(1.25)   # |median d| within 25 percent
FRACTION_TOL     = 0.10        # absolute, for fractions in [0,1]
CORRELATION_TOL  = 0.10        # absolute, for ACF values
ROBUST_MULT      = 1.5         # per-session slack multiplier
ROBUST_MIN_FRAC  = 0.70        # fraction of usable sessions that must
                               # individually sit within ROBUST_MULT * tol
```

A target passes when both hold over its usable sessions: the median of
`d(s)` sits within its class tolerance, AND at least `ROBUST_MIN_FRAC` of
sessions individually sit within `ROBUST_MULT` times that tolerance. A
target with fewer than `MIN_USABLE_SESSIONS` usable sessions (after named
exclusions) is unavailable and fails. Leave-one-session-out medians are
reported as a diagnostic, never a gate.

Tolerance rationale, stated so it cannot be restated later: these bands are
deliberately tighter than the fingerprint's cross-pair `empirical_ranges`
(which span multiples) because a proxy must be better than the tolerance
machinery it feeds, and looser than single-session noise at n=10. They are
choices, fixed a priori; their power is unknown and no result may tune them.

| Family | Targets | Class | Mandatory |
|---|---|---|---|
| P1 cadence level | `mean_event_duration_s` | ratio | no |
| P2 duration shape | `duration_dispersion_cv2`; `duration_acf_lag1`; `duration_acf_lag5` | ratio; corr; corr | **YES** |
| P3 sweep | `children_mean`; `children_single_frac`; `levels_mean` | ratio; frac; ratio | no |
| P4 return shape | `return_acf_lag1`; `abs_acf_lag1`; `abs_acf_lag10`; `abs_acf_lag50` | corr | **YES** |
| P5 zero change | `zero_change_frac` | frac | **YES** |
| P6 aggressor | side flip probability; mean same-side run length | frac; ratio | no |

Price-sequence targets (P4, P5) are computed on the per-print series, the
fingerprint's convention. P6 is computed on the parent sequence.

**Why the mandatory set is P2, P4, P5:** wave 2A buys NQ months for
long-memory, persistence and process-shape evidence - exactly these
families. P1 and P3 levels are expected to differ (MNQ prints more than NQ;
that is why wave 2B buys MNQ directly for the per-instrument scalars in
both branches), so they inform without deciding. P6 is new territory with
no validated lineage behind its estimator and enters as a family on
probation: reported, counted in the majority, never mandatory.

**Secondary measurements, decisively non-verdict:** trade-size moments
(mean size, log-size sigma, p95/p50), per-second count distributions, and
notional. Sizes differ across the contracts by construction, so no size
number can support or rescue the proxy verdict. Secondary measurements
appear in the report for wave 2B design and cannot rescue a failed
mandatory family - nothing outside the fourteen targets can.

## 5. The aggregate rule

Retained exactly from the sampling-frame design, because it survived
adversarial fixturing there:

1. A family passes on a strict majority of its own targets.
2. The verdict is `pass` only when all mandatory families (P2, P4, P5) pass
   and a strict majority of all evaluated families pass.
3. Unavailable is never a number and always fails the target it afflicts.
4. Anything else is `fail`. There is no partial verdict, no
   shape-only-pass, no reweighting after the fact.

The verdict artifact `analysis/databento-pair-verdict.json` is written only
after a human has read the result table, on explicit instruction, bound to
the job id and delivered-file hashes as `authorize_buy` already enforces.
The harness never writes it.

## 6. Order of work, with the seal

1. This document, committed. **The freeze.**
2. Conformance fixtures for the pair harness: synthetic two-leg CSVs with
   hand-derived expectations for leg splitting, session assignment,
   halt-gap exclusion, contiguous-parent inference, the side-run
   statistics, and every unavailable path. All checks green before step 3.
3. **Preflight, sealed-safe, on the delivered file:** header and column
   observation, timestamp resolution and units, row counts per leg,
   ordering, side-field population, contract mapping per session, session
   coverage. It prints no target values. Physical-schema observations are
   recorded facts; they may make a target unavailable through section 3's
   rules, but they cannot alter a tolerance, a family, or the aggregate
   rule - any such change after this point is a recorded deviation in this
   document, with the sampling-frame note's 3.7/3.9 entries as the format.
4. The harness run: per-target table, family table, verdict, all printed
   together once.
5. Human reads. On instruction, the verdict artifact is written.
6. Wave 2 decisions follow the verdict per section 1 - separately
   authorized either way.

## 7. The protocol 8 volume-versus-count correction, separate by design

The second job this purchase carries (report 11 step 6) is NOT part of the
verdict and shares only the parsing. From the MNQ leg (the target
instrument), per exchange-local hour over the calendar's open minutes:
mean trade count per minute and mean volume per minute, aggregated over
usable sessions into two hour-of-day curves. Reported: both curves, their
peak-to-trough ratios, and the ratio of ratios - the factor by which the
fitted 27.51x volume-derived intensity swing overstates the arrival-count
swing. The NQ leg's curves are computed identically as a cross-check.

Limitation, recorded now: ten July sessions against a 2020-2026 fitted
profile is a point estimate in one month of one year; it measures the
proxy direction and rough magnitude, not a seasonal correction. Whether
`mnq.toml`'s provenance caveat becomes a measured correction on this
evidence alone is a decision for after the numbers exist, and it is a
provenance decision, not part of this verdict.
