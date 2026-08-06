# MNQ session calibration repair: protocol 11

Implementation specification, 2026-08-06, revision 3: the revision-1
freeze review returned sixteen required amendments, all incorporated -
the substantive one being the frozen-DOW conditional `intensity_hour`
estimator (a marginal-rate normalization would have double-counted day
concentration through the retained `dow_weight`). The revision-2
review returned three blockers, incorporated here: the 300 s cell
floor corrected to the value hour 20 can actually reach (the reviewer
withdrew their own earlier 8), the pooled-RMS failure stopping branch,
and the elimination of every schema ellipsis. The revision-2 review
also confirmed the composition premise (the tick-composition tool
drives exactly 2,000,000 measured parents per combination, so the
parents-identity assertion is sound) and pinned the fill-golden test
name.
Written against `reference/technical-implementation-spec.md` (the
contract this document must satisfy) and spawned from the owner's
2026-08-06 chart finding (Asia and London sessions far too quiet on
the landed protocol-10 tape) plus the protocol-10 RESULT records in
`notes/mnq-tbbo-fit-spec.md` (the envelope failure whose successor
work this spec sequences). Scope, estimators, gates and the
fold-versus-split decision were argued to consensus by both reviewers
on 2026-08-06 in one continuous session.

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it. When the landing is complete, what remains
true lives in the preset provenance, `reference/`, and the fit artifact.

---

## 1. The goal

Repair the MNQ session volatility and arrival calibration whose unit
mismatch makes the generated Asia and London sessions roughly five
times too quiet, land the tail-evidence instrumentation the protocol-12
tail repair needs, re-solve `vol_scalar` under the corrected curves,
bump `TAPE_PROTOCOL_VERSION` 10 -> 11 in the same indivisible landing,
and remeasure the composition budgets under a new 10-to-11 mode.

The tail-shape model itself (the t(4)/GARCH cluster-tail envelope
failure, 4,333 generated worst-minute ticks against the real month's
968) is EXCLUDED: protocol 12 owns tail-family selection, and this
spec's instrumentation deliverables are its evidence base. The split
was argued adversarially and conceded: the session repair is fully
understood mechanically while the tail family cannot be chosen without
the location evidence this spec produces, and a spec cannot contain
choose-later.

AMENDED at Brick V, both reviewers, after the first real fit: protocol
11 repairs the PER-PARENT hourly scale and the parent-arrival
calibration, and the fit proved both on the real month (arrival worst
hour 0.63 percent, parent-vol worst 6.6 percent, pooled wall-time and
pooled parent RMS passing). The hourly wall-time CONTOUR failed at the
reversion-heavy hours (300 s at hours 19, 20, 23; 60 s marginally at
hour 20) with arrival and per-parent scale matching to three or four
decimals at those very hours - a horizon-dependent residual after
every protocol-11 input matches, which classifies it as higher-order
aggregation evidence: an hour-dependent serial-dependence or
aggregation-law mismatch whose mechanism (persistence, bounce and
reversion, arrival clustering, boundary effects) remains UNSELECTED
among the protocol-12 candidates. Protocol 12 therefore inherits BOTH
the minute-range tail failures and the hour-dependent 60 s / 300 s
aggregation-contour failures, without any claim that one mechanism
fixes both. The hourly wall-time verdicts become recorded diagnostics
for the protocol-11 landing (section 4.5); nothing widens their band,
exempts a failing hour, changes a fitted value or erases a failed
verdict.

## 2. Survey of the ground

### 2.1 The defect

`vol_hour` carries a unit mismatch between fit and application:

- The MNQ curve was fitted at protocol 8 as a PER-MINUTE quantity: RMS
  of adjacent within-session one-minute close returns of NQ bars
  (provenance in `crates/mogwai-server/presets/mnq.toml`, fit report
  `notes/mnq-session-fit.md`). Peak-to-trough 3.38x (0.5533 at hour 3
  to 1.8702 at hour 14).
- The generator applies it PER PARENT EVENT:
  `SessionModulator::vol_mult` (`generated/session.rs`) multiplies each
  parent's latent return (`generated/source.rs`, the
  `session_vol_mult` composition site).

Minute-level realized volatility scales as per-parent scale times
sqrt(parents per minute); `intensity_hour` swings arrivals 27.5x
(0.1332 to 3.6634), so the generated minute-vol peak-to-trough is
about 3.38 x sqrt(27.5), roughly 17-18x, against the fitted per-minute
curve's 3.38x. Verified on the landed chart CSV: generated 5-minute
close-return RMS peak-to-trough 21.7x, print-count peak-to-trough
28.3x, against the sqrt-model prediction 17.5x - the mechanism is
confirmed, with the excess consistent with t(4) noise and grid effects
over one synthetic month. Overnight hours are therefore about 5x too
quiet at bar scale, which is the owner's observation.

`intensity_hour` is independently suspect: it is contract-VOLUME
intensity fitted from NQ bars (the recorded proxy caveat), and the
protocol-10 artifact's July MNQ diagnostics show print-count and
volume peak-to-trough near 14x - about half the shipped 27.51x. The
gap is instrument/window/unit dependence, not the count-versus-volume
ratio (measured 0.95-0.96). The real per-parent volatility curve is
directionally much flatter than the per-minute curve: decomposing the
3.38x minute curve by the July 14.3x activity curve implies a
per-parent peak-to-trough near 0.88 - possibly slightly INVERTED
(overnight parents individually move more). The direct estimator
decides; the decomposition is a diagnostic, never a fitted value.

Why nothing caught it: `session_modulation_reproduces_curves`
(`generated/tests.rs`, `#[ignore]`d - a plain `brokkr check` does not
run it) constructs the XBTUSD fingerprint anchor, whose `vol_hour`
genuinely IS a per-trade curve fitted from Kraken per-trade returns -
its per-parent semantics are correct for that input. The hole is that
nothing binds an MNQ provenance UNIT to the generic per-parent runtime
contract. The crypto presets are correct as shipped; only the MNQ
arrays (and MES by inheritance) are wrong.

The rejected repair, recorded: dividing the applied multiplier by
sqrt(arrival_mult) in `SessionModulator`. It would corrupt the crypto
presets whose curve is genuinely per-trade, entangle the day-of-week
factor into an hour-only correction, hard-code an aggregation exponent
the tape need not obey, and leave the per-parent mechanism knowingly
false while a wall-time target reads correct by construction. The
modulator's semantics do not change in this spec.

### 2.2 The evidence

The same delivered July 2026 MNQ TBBO month as protocol 10:
`research/market-data/databento/mnqv/2026-07.full.tbbo/`, job
`GLBX-20260805-HAPEWPABKG`, ledger-bound and hash-verified. TBBO
carries the pre-trade top of book per trade, so adjacent
inferred-parent QUOTE-MID returns are computable - the fit target is
quote mids, never trade prices, because trade returns mix the
bounce/displacement layer into a multiplier that acts on the latent
mid upstream of it. Scale of the corpus, from the protocol-10
artifact: 29,605,800 inferred parents over 22 usable sessions, about
1.35 M parents per session, minimum observed row rate about 330 rows
per open minute - the thin-cell floor of 4.1 is far below any healthy
hour.

### 2.3 The target artifact

`crates/mogwai-server/presets/mnq.toml`:

| slot | disposition |
|---|---|
| `session.vol_hour` | refitted: per-parent hourly robust scale from July quote mids |
| `session.intensity_hour` | refitted: conditional hour parameter under frozen `dow_weight`, from inferred-parent counts |
| `session.dow_weight` | UNTOUCHED, byte for byte; NQ-bar provenance stands (22 sessions are ample per hour, thin per weekday; refitting it needs its own preregistered stability gate) |
| `generator.vol_scalar` | re-solved under the new arrays; lands fitted only if the whole volatility family, minute envelope included, passes - else best candidate under declared provenance, the protocol-10 precedent |
| everything else | untouched |

The two session arrays are ONE ATOMIC LANDING GROUP (Brick V
amendment): both receive fitted status only when session_arrival,
session_parent_vol, BOTH pooled wall-time gates, cadence
non-regression and the pooled parent RMS pass at probe and combined
stages. Hourly wall-time verdicts remain measured diagnostics and do
not participate in the protocol-11 landing decision. If any landing
gate fails, neither array, nor `vol_scalar`, nor protocol 11 lands - a
scalar calibrated under rejected arrays does not land. If the landing
gates and cadence pass, the `base_volatility` verdict splits three
ways: all checks pass, `vol_scalar` lands fitted; the pooled parent
RMS passes but one or more minute-envelope checks fail, the candidate
`vol_scalar` lands declared-best-candidate; the pooled parent RMS
FAILS, protocol 11 REFUSES and neither arrays, scalar, nor version
bump lands, regardless of the envelope verdict - landing arrays with
the old scalar would ship a known-wrong scale, and a candidate that
missed its primary scale target is not covered by the envelope-only
exception.

### 2.4 What depends on the values

Same golden inventory as protocol 10 (`notes/mnq-tbbo-fit-spec.md`
2.3), with identical conclusions: `clean_regime_is_byte_identical`,
the fill goldens, and the three seam tests construct the XBTUSD anchor
and never resolve presets - all MUST remain byte-unchanged, and their
passing without re-bless is itself a gate (exact commands in Brick L).
So must
`calendar_free_profiles_are_untouched_by_calendar_aware_normalization`
and `session_profile_rejects_non_normalized_curves` (validation
contexts do not change). The moving surface is `mogwai-server`:
`fitted_mnq_effective_values_are_the_artifact_values` (extended to pin
the session arrays and new `vol_scalar` - it currently pins no session
literals) and `mes_inherits_the_mnq_fit_loudly` (the new session
evidence inherited visibly).

Composition budgets: the shipped protocol-10 constants are
`CHECKPOINT_K = 16_777_216`, `SWEEP_DRAIN_BUDGET = 5_799_000_000`,
`MAX_WARMUP_MATERIALIZATION_TICKS = 667_299_000_000`,
`fanout_depth = 4_194_304` (derivation in `reference/performance.md`).
Session-array changes redistribute arrival density across hours, so
remeasurement is UNCONDITIONAL and needs `--mode independent_10_11`
with the protocol-10 fixture as the before side (Brick R2, with the
STRICTER session-reshape acceptance: protocol 11 changes timing and
prices, never parent fanout).

### 2.5 Instruments: what exists and what must be built

- `analysis/mnq_fit.py` (existing, protocol-10 harness): has the
  stream/parse/group/session machinery, the walk cache, the solve
  machinery, the envelope resampler, and the artifact pipeline. It
  LACKS: per-(session, hour) quote-mid return cells, per-hour-per-DOW
  inferred-parent counts (its session-curve diagnostic counts rows),
  the refit estimators, the wall-time hourly curves on the observed
  side, and the new gates. Its `run_fit` re-solves size, quote,
  displacement, start price and volatility - protocol 11 must NOT
  inherit that scope (4.4). Editing it is not a new script; no owner
  script gate applies. The sub-contract hash moves, so preflight
  reruns - the binding working as designed.
- Observed `horizon_vol` in the harness is aligned to each
  session-segment origin and resets at segment transitions; the
  generated summary's `horizon_vol` is aligned to measurement start.
  The two conventions DIFFER today, which is why 4.6 defines NEW
  gated statistics under one shared convention and leaves the legacy
  fields untouched for protocol-10 artifact compatibility.
- `gen --type summary` (`crates/mogwai-server/src/gen.rs`,
  `SummaryAcc`): has pooled `horizon_vol`, pooled
  `mid_return_{count,sum,sumsq}`, `minute_range_ticks_hist` and the
  two maxima. It LACKS every per-hour statistic, robust-scale
  accumulators, generated-session identity, and worst-minute location
  records. Brick G2 adds them under the exact schema in that brick.
- Candidate session arrays are ALREADY expressible: the scratch
  config's `[instrument.override]` table goes through
  `replace_dotted`, which replaces any path the preset sets,
  including `session.vol_hour` and `session.intensity_hour`. The
  harness's `scratch_config_text` needs only TOML array
  serialization for list-valued overrides.
- `analysis/tick_composition_ratios.py`: version-pinned modes through
  `independent_9_10`; Brick R2 adds `independent_10_11`.

## 3. The order of work

1. **Brick F0 - freeze FIRST.** Both reviewers freeze and sign this
   complete specification before any implementation brick starts.
2. Bricks G2, R2, T, H: build the instrumentation, the ratio mode,
   the generic estimator repair and the harness extensions. Run every
   named unit gate and `python3 analysis/mnq_fit.py selftest` while
   the tree is dirty.
3. **H-commit**: commit G2, R2, T and H together as an
   instrumentation-only protocol-10 landing after all their gates
   pass. This commit changes no tape generation and does not bump the
   tape protocol.
4. Brick M1: from that clean commit,
   `python3 analysis/mnq_fit.py preflight` (the sub-contract hash
   moved; preflight must rerun and rebind).
5. Brick M2: from the same clean commit,
   `python3 analysis/mnq_fit.py fit`.
6. Brick V: both reviewers read the bound artifact; the landing set
   derives from the recorded verdicts alone.
7. Bricks L and B as ONE keep/revert unit (landing + composition
   remeasure + any ceiling resize), then Brick D (documentation).

No owner gate appears in this sequence: no new script, no purchase.
The owner is brought in at the end with a regenerated chart.

## 4. The frozen measurement sub-contract

Additions to the existing sub-contract; every constant lands in
`SUBCONTRACT_KEYS` so the hash binds it. Unchanged protocol-10 rules
(input contract, preflight, parent inference, session inventory,
budgets `SEARCH_*`/`FINAL_*`, seeds, warmup, solve algorithm) are
incorporated by reference and NOT restated.

New constants, all frozen here:

```
MIN_PARENT_CELL_RETURNS = 1000
MIN_60S_CELL_RETURNS    = 40
MIN_300S_CELL_RETURNS   = 6
SESSION_HOUR_BAND       = (0.8, 1.25)
ARRIVAL_HOUR_REL_TOL    = 0.10
WALLTIME_POOLED_REL_TOL = 0.15
SESSION_ARRAY_DECIMALS  = 6
TOP_MINUTE_RECORDS      = 32
SESSION_VOL_CORR_MIN    = 0.90
```

There are 23 exposed UTC hours - every hour except 21, with hour 20
contributing only its open 45 minutes. The unexposed hour 21 (the
daily break covers it entirely under the permanent-CDT calendar) lands
as the conventional 1.0 in both arrays; the calendar keeps it off
every ordinary tape (confirmed inert: `utc_hour(clock_ns)` is read
after the arrival clock advances, and no advanced clock lands in a
closed window on the ordinary path).

### 4.1 The per-hour robust scale estimator (observed and generated)

For each usable session `s` and exposed UTC hour `h`:

1. Adjacent inferred-parent pre-trade quote-mid log returns, both
   parents inside the same session segment.
2. Each return attributed to its ENDPOINT parent's UTC hour - the
   application-site convention: `begin_event` advances the clock,
   then the latent update reads `vol_mult(clock_ns)`, so the return
   belongs to the new parent's hour.
3. Zero returns INCLUDED - the zero-change mass is signal.
4. A parent-return cell qualifies only with
   `MIN_PARENT_CELL_RETURNS = 1000` returns.
5. Accumulate `count`, `sum_abs`, `max_abs`; the cell scale is the
   one-maximum-trimmed mean absolute return:
   `(sum_abs - max_abs) / (count - 1)`.
6. The hourly value is the nearest-rank median of cell scales across
   sessions. EVERY usable observed session (and every COMPLETE
   generated session, 4.5) must supply a qualifying cell for every
   exposed hour; otherwise the refit REFUSES. At the corpus scale of
   2.2 a sub-1000 hour is bad or structurally different input, not a
   case to accommodate; no fallback is selected after seeing data.

The parent populations, pinned:

- Observed INTENSITY counts every sided inferred parent, regardless
  of book validity. An unsided row terminates any open parent and is
  not itself a parent.
- Observed VOLATILITY uses the subsequence of valid-book inferred
  parents. Invalid-book parents are omitted but do NOT reset the
  valid-mid chain; the chain resets only at a session-segment
  transition. Both contributing valid parents must belong to the same
  segment.
- Generated counts use quote-delimited completed parent bursts, with
  the parent attributed by its first child timestamp. Generated
  quote-mid returns use consecutive completed parents in the same
  segment, attributed by the endpoint parent's first timestamp.

Why this estimator: mean absolute return has finite sampling variance
under t(4) (unlike RMS, whose variance needs the nonexistent fourth
moment); including zeros preserves movement frequency where a
conditional-nonzero median discards it and a raw median can collapse
to zero on a 0.25 grid overnight; the single-maximum trim is O(1)
protection against one exceptional observation; the cross-session
median stops a few unusual sessions from steering the curve; and it
is exactly streamable - no arrays, no sketches, no second pass.
Rejected: smallest-nonzero quantile (data-adaptive, erases the zero
atom), fixed low quantile (can still be zero), conditional nonzero
median (discards frequency), RMS fallback after observing zeros
(preregistration violation reviving the known estimator defect).

### 4.2 `vol_hour` refit, normalization and materialization

The raw hourly scales of 4.1 (median across the 22 usable observed
sessions), normalized and materialized as follows. This HOUR-ONLY
normalization rule applies to `vol_hour`, the observed and generated
marginal arrival curves, and the normalized robust parent-vol and
wall-time curves; section 4.3 separately defines the conditional
`intensity_hour` parameter normalization (the composite
`W[h,d] * q[h] * w[d]` rule):

- Normalization sums hours in ascending UTC-hour order using binary64
  arithmetic with weights 60 for each exposed full hour, 45 for hour
  20, and 0 for hour 21; each exposed raw value is divided by that
  weighted mean. Hour 21 is set to exactly 1.0.
- Each fitted session-array value is materialized as
  `float(format(value, ".6f"))` (`SESSION_ARRAY_DECIMALS = 6`). The
  MATERIALIZED arrays, not the unrounded arrays, are installed in
  every scratch profile, judged by every FINAL gate, and written to
  the preset - the fitted array and the shipped array cannot silently
  differ.
- The artifact records `raw`, `normalized_unrounded` and
  `materialized` 24-element arrays; unexposed raw and unrounded
  entries are JSON null; the materialized hour-21 entry is 1.0.

The stored normalization is presentational - the modulator
re-normalizes over open minutes, so multiplying every exposed value by
one common constant must not change generated bytes - but the rule is
frozen so two implementers serialize identical arrays.

### 4.3 `intensity_hour` refit with frozen `dow_weight`

The runtime applies an hour-by-day product, and UTC hours 22-23 fall
on the PRIOR civil day of each trade session (day mix Sun-Thu) while
hours 0-20 run Mon-Fri. A marginal hourly rate normalization retaining
`dow_weight` would apply day concentration twice. The hour parameter
is therefore solved CONDITIONALLY on the frozen day factor,
closed-form.

For each exposed UTC hour `h` and UTC day-of-week `d`, let `C[h,d]` be
the count of inferred parents (intensity population, 4.1) whose FIRST
timestamp falls in that cell, and `E[h,d]` the calendar-open minutes
in that cell summed across the usable observed sessions (exposure from
the shipped calendar, never row presence). Let `w[d]` be the shipped
`dow_weight`, byte-unchanged. The raw hour parameter is

```
q[h] = sum_d C[h,d] / sum_d (E[h,d] * w[d])
```

which is the closed-form hour estimate in the multiplicative model
`E[C[h,d]] = E[h,d] * alpha * q[h] * w[d]` with the day factor frozen.
Hour 21 is excluded. For presentation, `q` is normalized over the
shipped calendar's complete weekly exposure table `W[h,d]`:

```
Z = sum_h,d W[h,d] * q[h] * w[d] / sum_h,d W[h,d]
intensity_hour[h] = q[h] / Z
```

then materialized per 4.2; hour 21 lands as exactly 1.0. The runtime
normalizer remains the final authority.

The artifact separately records: (1) the observed MARGINAL parent-rate
target `sum_d C[h,d] / sum_d E[h,d]`, normalized over exposed hours -
this is what the `session_arrival` gate compares generated marginal
rates against, never the conditional parameter array; (2) the fitted
conditional array; (3) the frozen `dow_weight`; (4) the full `E[h,d]`
and `C[h,d]` sufficient statistics.

`dow_weight` is not recomputed, not renormalized, not touched.

### 4.4 `vol_scalar` re-solve, and the fit-mode scope

The existing log-domain solve and pooled adjacent-parent quote-mid RMS
target, unchanged, with the candidate MATERIALIZED session arrays
installed in the scratch profile. Solve order: fit `intensity_hour` ->
fit `vol_hour` -> scratch profile with both candidate arrays and
unchanged `dow_weight` -> solve `vol_scalar` -> final combined
eight-seed run -> all gates evaluated together.

SEARCH versus FINAL, frozen: SEARCH evaluates ONLY the pooled
adjacent-parent quote-mid RMS objective for `vol_scalar`, on
`SEARCH_START_NS`/`SEARCH_LENGTH`/`SEARCH_SEEDS`. SEARCH does not
evaluate session cells, hourly curves, horizon gates, cadence gates or
minute-range envelopes; those fields may be present in summaries but
are ignored by the solve. Every gate in 4.5 is evaluated only on
FINAL-budget summaries.

Fit-mode scope, frozen: the protocol-11 fit mode does NOT execute or
adopt the protocol-10 size, quote, displacement, start-price, cadence
or shared-shape solves. Every non-scoped preset value is resolved from
the shipped MNQ profile and copied byte-for-byte into every candidate
profile. The only fitted values in this mode are `intensity_hour`,
`vol_hour` and `vol_scalar`. The cadence family is remeasured as a
NON-REGRESSION gate against the final candidate profile; its values
are never recomputed from the July observation.

### 4.5 Gate families (target-local, the protocol-10 discipline)

Generated sessions, defined exactly: a generated session is keyed by
`session_start_ns`, the UTC instant of 17:00 on the prior local civil
date under the preset's fixed `utc_offset_minutes = -300` (local
minute >= 17:00 opens the session dated the NEXT civil day; local
minute < 16:00 belongs to the session opened the prior day 17:00;
15:15-15:30 and 16:00-17:00 are closed; the overnight and post-halt
segments compact into one session record but maintain separate return
and horizon chains). A generated session is COMPLETE only when its
full 17:00-16:00 span, both open segments included, lies inside the
measurement interval; partial sessions are recorded but excluded from
cells and gates. The generator's weekly calendar has no July 3
holiday, so the FINAL window contains exactly 23 complete generated
sessions per seed; any other count REFUSES. The observed side has the
22 usable sessions of the frozen preflight inventory.

| family | contents | verdict rule |
|---|---|---|
| `session_arrival` | `intensity_hour` | per exposed hour: generated marginal parents-per-open-minute (pooled counts and exposure across `FINAL_SEEDS`, normalized once) within `ARRIVAL_HOUR_REL_TOL = 10%` relative of the observed marginal target of 4.3 |
| `session_parent_vol` | `vol_hour` | per exposed hour: generated curve within `SESSION_HOUR_BAND = [0.8, 1.25]` multiplicative of the observed factor |
| `session_walltime` | wall-time contour | pooled 60 s and 300 s RMS are LANDING GATES at `WALLTIME_POOLED_REL_TOL = 15%` relative; the hourly 60 s and 300 s robust curves retain their frozen `[0.8, 1.25]` verdicts as recorded protocol-12 DIAGNOSTICS (Brick V amendment; `WALLTIME_HOURLY_ROLE = "diagnostic"` binds this in the sub-contract) |
| `base_volatility` | `vol_scalar` + pooled RMS + minute envelope | pooled parent RMS 10% relative (existing); the three per-seed minute-range envelope gates (existing, unchanged) |

The landing predicates, explicit:

```
walltime_pooled_ok = probe AND combined pass at BOTH pooled horizons
walltime_hourly_ok = probe AND combined pass every exposed-hour
                     60 s and 300 s check   (RECORDED, never gating)
session_ok = session_arrival_ok AND session_parent_vol_ok
             AND walltime_pooled_ok
```

The no-hour-escape rule continues to govern the recorded
`walltime_hourly_ok` verdict - no hour is exempted and no band widens -
it simply no longer controls the protocol-11 landing. Protocol 12 must
treat the hourly 60 s and 300 s bands as HARD successor gates beside
the minute-range envelope, unless its spec replaces them before
implementation with a defended, preregistered estimator; they cannot
silently disappear.

Generated central curves for `session_parent_vol` and
`session_walltime`: (1) raw hourly median across COMPLETE sessions
within each seed; (2) normalize that seed's curve per 4.2 (no
materialization rounding on generated curves); (3) record every
normalized per-seed curve; (4) nearest-rank median at p = 0.5 across
`FINAL_SEEDS` per hour; the across-seed curve is NOT renormalized.
Per-seed curves are diagnostics; only the minute-range tails stay
per-seed gates. Hour 21 is excluded from every session gate.

Probe overrides, exact - every other value resolves unchanged from
the shipped protocol-10 preset:

- `session_arrival`: candidate `intensity_hour` alone.
- `session_parent_vol`, `session_walltime`, `base_volatility`: both
  candidate session arrays and the solved candidate `vol_scalar`.
- final combined: both candidate arrays and candidate `vol_scalar`.

Identical override sets may share cached FINAL walks; every family
records its own target-local checks. The final combined run is
attempted regardless of an individual family-probe miss, so the
artifact records interactions; landing still requires both the
relevant probe and combined checks.

Landing rule: the atomic group of 2.3, including its three-way
`base_volatility` split - the pooled-RMS-failure branch refuses
protocol 11 outright. Cadence gates rerun unchanged
on the combined profile; a cadence regression REFUSES protocol 11
outright rather than expanding scope. The `[0.8, 1.25]` band reuses
the preregistered materiality ratio from the session-fit era rules;
there is NO material-share escape - with 23 exposed hourly slots,
allowing one failed hour would permit the exact visible defect under
repair.

### 4.6 Wall-time horizon statistics (new, shared convention)

The new protocol-11 horizon statistics split by role (Brick V
amendment): the POOLED horizon RMS values are protocol-11 landing
gates, the HOURLY robust contours are frozen-band diagnostics and
mandatory protocol-12 evidence. Neither reuses nor changes the legacy
`horizon_vol` field on either side (the two legacy conventions differ,
2.5, and protocol-10 artifact compatibility keeps them frozen). All
measurement conventions and floors below are unchanged by the
amendment. For BOTH observed and generated data:

1. State is independent per session segment and horizon W in
   {60 s, 300 s} (`HORIZON_SECONDS`, unchanged).
2. Boundaries are `segment_origin + k * W`, integer k >= 1.
3. State resets at the session open, the halt start, the halt end and
   the daily close.
4. The first boundary having an as-of mid establishes the previous
   boundary mid and emits no return.
5. The as-of mid at a boundary is the last valid quote mid at or
   before that boundary.
6. A boundary exactly equal to a parent timestamp is emitted only
   after all parents with that timestamp have updated the as-of mid.
7. No return crosses a segment boundary or UTC hour boundary.
8. A retained return is attributed to its endpoint UTC hour.
9. The hourly cell uses the 4.1 trimmed-mean absolute estimator.
10. The pooled 60 s and 300 s RMS gates pool the same accepted return
    population before taking RMS.

Horizon cells use separate frozen floors: `MIN_60S_CELL_RETURNS = 40`
and `MIN_300S_CELL_RETURNS = 6`. The 300 s value is the theoretical
MAXIMUM hour 20 can reach under the boundary rules above - overnight
portion 20:00-20:15 admits endpoints 20:05 and 20:10 (20:15 is the
halt), the post-halt portion 20:30-21:00 spends 20:35 establishing
the previous mid and admits 20:40 through 20:55, so 2 + 4 = 6 - and
requiring all six is deliberate at the observed density. The 60 s
maximum for hour 20 is 42 (14 overnight after the hour-boundary
exclusion, 28 post-halt), so its floor of 40 stands. A full unsplit
hour holds at most 60 and 12 windows. Every usable observed session
and every complete generated session must qualify at both horizons for
every exposed hour; no fallback.

Independent observed and generated fixtures must prove identical
boundary behavior: equal timestamps, halt resets, hour boundaries,
and hour 20.

### 4.7 Tail-evidence instrumentation (deliverable, not gate)

The summary gains a fixed-capacity worst-minute collection,
`TOP_MINUTE_RECORDS = 32` per seed. The contract:

- A populated minute is the half-open UTC interval
  `[minute_start_ns, minute_start_ns + 60s)` containing at least one
  in-window trade.
- `trade_count` counts all trades in the interval; `parent_count`
  counts completed parents by first-child timestamp.
- `low_price`/`high_price` serialize as normalized decimal strings;
  `range_ticks` is the exact integer
  `(high_price - low_price) / tick` - a nonintegral result refuses.
- `top_minutes` has length `min(TOP_MINUTE_RECORDS,
  populated_minutes)`, ordered by `(range_ticks descending,
  minute_start_ns ascending)`; repeated equal ranges occupy distinct
  entries; the first and second records must reproduce the existing
  maximum and second-maximum semantics exactly.
- `trace_from_ns = minute_start_ns`,
  `trace_until_ns = minute_start_ns + 60s`, making a follow-up
  forensic-trace invocation mechanical.
- Each record: `minute_start_ns`, `minute_end_ns`, `utc_hour`,
  `range_ticks`, `parent_count`, `trade_count`, `low_price`,
  `high_price`, `trace_from_ns`, `trace_until_ns`.

The artifact carries all records per seed. These records exist to
explain protocol 11's own envelope verdict (did redistribution move
the worst minute, help it, or worsen overnight?) and to hand protocol
12 its evidence - durable observation capability, not scaffolding.

### 4.8 Diagnostics (findings, never gates)

- The sqrt-decomposition, recorded as a LINEAGE AND DIRECTIONALITY
  diagnostic only: it compares the retired NQ per-minute proxy with
  the July MNQ parent-count curve and therefore does not estimate the
  generated tape's aggregation exponent.
- The July count-versus-volume session curves, re-reported beside the
  retired volume-proxy caveat.
- Whether the fitted per-parent curve is inverted (overnight above
  1.0), stated explicitly either way.

## 5. Bricks

Each brick names its gate with the exact command. The suite stays
green at every boundary. Brick F0 (section 3) precedes all of them.

**Brick G2 - summary instrumentation (Rust, `gen.rs`).** Extend
`SummaryAcc` with generated-session cell structure and the top-minute
records, under this exact schema:

```
struct AbsCell     { count: u64, sum_abs: f64, max_abs: f64 }
struct HorizonCell { count: u64, sum: f64, sumsq: f64,
                     sum_abs: f64, max_abs: f64 }
struct GeneratedSessionCells {
    session_start_ns: u64,
    session_end_ns: u64,
    complete: bool,
    parent_count_by_hour: [u64; 24],
    mid_abs_by_hour: [AbsCell; 24],
    horizon_60_by_hour: [HorizonCell; 24],
    horizon_300_by_hour: [HorizonCell; 24],
}
struct TopMinuteRecord {
    minute_start_ns: u64,
    minute_end_ns: u64,
    utc_hour: u8,
    range_ticks: u64,
    parent_count: u64,
    trade_count: u64,
    low_price: String,
    high_price: String,
    trace_from_ns: u64,
    trace_until_ns: u64,
}
// SummaryAcc gains:
//   session_cells: Vec<GeneratedSessionCells>,  (ascending session_start_ns)
//   top_minutes: Vec<TopMinuteRecord>,          (the 4.7 contract)
```

Hour arrays index UTC hour 0-23; empty cells are zero-valued
accumulators, never omitted; binary64 sums accumulate in stream order;
non-finite values refuse serialization. Session identity and
completeness per 4.5; the horizon cells follow the 4.6 convention
(segment-origin alignment - deliberately NOT the legacy
measurement-start alignment of `horizon_vol`, which stays untouched).
Existing fields are unchanged; existing consumers see a superset.
Per-session structure is required, not per-hour pooling: the observed
estimator gives each session one vote, and pooling would weight
sessions by parent count - not estimator-equivalent.

Gates:
`brokkr test -p mogwai-server summary_matches_an_independent_tick_walk`
(extended in place: the independent accumulation covers every new
field exactly, including session completeness at window edges and the
4.6 boundary rules - equal timestamps, halt resets, hour boundaries,
hour 20),
`brokkr test -p mogwai-server minute_ranges_match_an_independent_bar_pass`
(extended: the independent bar pass reproduces the top-minute records,
covering empty summaries, fewer than 32 minutes, repeated maxima and
tie ordering),
`brokkr test -p mogwai-server each_candidate_scalar_moves_the_summary`
(extended: `session.vol_hour` and `session.intensity_hour` overrides
demonstrably reach the new measurements). `brokkr check` green.

**Brick R2 - the 10-to-11 ratio mode.** Extend
`analysis/tick_composition_ratios.py` with `--mode independent_10_11`:
fixtures `tick-composition-protocol-10.json` (existing, frozen) /
`tick-composition-protocol-11.json`; baseline table entry with the
protocol-10 constants of 2.4. The gate uses the STRICTER
session-reshape acceptance (the 7-to-8 discipline, not the 9-to-10
preset-fit allowance):

- `parents` identical for every pairing;
- `ticks_per_parent` identical for every pairing;
- every field identical for calendar-free presets;
- all numeric fields finite and positive;
- distinct pairing ids; canonical producer labels per version.

Only calendar-bearing timing and price-derived composition fields may
move: a parent-count or fanout change proves protocol 11 affected
something outside the authorized session reshape and refuses the
comparison. Historical modes and fixtures untouched. Gate:
`python3 analysis/tick_composition_ratios.py --selftest` extended to
cover the new mode both ways. An edit to an existing reviewed script.

**Brick T - the generic estimator repair (`mogwai-data` tests only).**
`session_modulation_reproduces_curves` replaces its per-hour RMS with
the median absolute latent-parent return scale, discharging the
recorded follow-up that FORBIDS any `vol_hour` refit while the noisy
RMS estimator stands (latent mids are continuous, so the tick-grid
zero atom does not apply to the generic test). The correlation
threshold is frozen NOW: `SESSION_VOL_CORR_MIN = 0.90`, inclusive. If
the frozen test budget does not satisfy it, Brick T fails and
implementation stops for renewed design review; the threshold is not
lowered from the observed result. The test's meaning - a generic
per-parent mechanism check on the XBTUSD anchor - is preserved. Gate:

```
brokkr test -p mogwai-data session_modulation_reproduces_curves
brokkr check
```

(the test is `#[ignore]`d; the focused runner is required, a plain
check does not execute it). No golden moves.

**Brick H - harness estimators and gates.** In `analysis/mnq_fit.py`:
the 4.1 observed cells (streamed beside the existing observe pass,
with the `C[h,d]`/`E[h,d]` sufficient statistics), the 4.2/4.3 refit
constructors with materialization, TOML array serialization in
`scratch_config_text`, the 4.4 fit-mode scope (a protocol-11 mode
that copies every non-scoped value byte-for-byte and never invokes
the protocol-10 solves), the 4.5 families and probe tables, the 4.6
observed horizon statistics, the 4.8 diagnostics, and the artifact
schema extension:

Named shapes, used below. `curve24` is a 24-element array of numbers
with JSON `null` at every entry the curve does not define (hour 21
everywhere; additionally every non-exposed entry of a raw array).
`triple` is `{"raw": curve24, "normalized_unrounded": curve24,
"materialized": [24 numbers]}` where the materialized array has 1.0 at
hour 21 and no nulls. `abs_cell` is `{"count": int, "sum_abs": num,
"max_abs": num}`; `horizon_cell` adds `"sum"` and `"sumsq"`. An
observed session-cell record is `{"session": "<inventory label>",
"cells": [24 x abs_cell]}` (or `horizon_cell`), ordered by inventory
label ascending. A generated session-cell record mirrors
`GeneratedSessionCells` field for field with hour arrays as 24-element
JSON arrays, ordered ascending by `session_start_ns`. `walltime_block`
is `{"60": {"hourly": {"raw": curve24, "normalized": curve24},
"pooled_rms": num|null, "return_count": int}, "300": {same shape}}` -
`pooled_rms` is null exactly when `return_count` is 0, a deliberate
failed measurement rather than a NaN the strict writer must refuse. A
session-gate verdict record is `{"family": str,
"stage": "probe"|"combined", "per_hour": [24 x bool|null],
"worst_hour": int|null, "worst_ratio": num|null, "pass": bool|null}`:
for a stage that RAN, per_hour is null only at hour 21 and pass is a
bool; a stage that never ran (a probe refusal, a failed combined run)
carries an all-null per_hour with pass null - both stage records always
exist, and a fabricated bool would claim a measurement that never
happened. A metric verdict record is the existing protocol-10
verdict-record schema (family, status, tolerance, measured and checks
by stage, observed) with `status` drawn from
`{"passed", "failed", "not-run"}` - passed iff both stage checks read
true, not-run when either stage never produced a check. (Amended at
the revision-3 implementation review, re-signed narrowly: the original
shapes required `pass: bool` and numeric `pooled_rms` unconditionally
and omitted the metric records' status field.)

```
"session_refit": {
  "constants": {the nine section-4 constants, name: value},
  "observed": {
    "session_count": 22,
    "parent_count_by_hour": [24 ints],
    "parent_count_by_hour_dow": [24 x [7 ints]],   (hour-major, Sun=0)
    "open_minutes_by_hour_dow": [24 x [7 ints]],
    "parent_rate_target": {"raw": curve24, "normalized": curve24},
    "parent_vol_cells": [22 observed session-cell records, abs_cell],
    "parent_vol_curve": triple,
    "horizon_60_cells": [22 records, horizon_cell],
    "horizon_300_cells": [22 records, horizon_cell],
    "walltime_curves": walltime_block
  },
  "candidate": {
    "intensity_hour": triple,        (4.3 normalization)
    "vol_hour": triple,              (4.2 normalization)
    "dow_weight": [7 numbers],       (the frozen shipped values)
    "vol_scalar": number
  },
  "generated": {
    "final_seeds": [1,2,3,4,5,6,7,8],
    "per_seed": [8 records, FINAL_SEEDS order: {
      "seed": int,
      "session_cells": [23 generated session-cell records],
      "parent_vol_curve": {"raw": curve24, "normalized": curve24},
      "walltime_curves": walltime_block,
      "arrival_count_by_hour": [24 ints],
      "top_minutes": [TopMinuteRecord fields as JSON, 4.7 order]
    }],
    "central_curves": {
      "parent_vol": curve24,
      "walltime_60": curve24,
      "walltime_300": curve24,
      "arrival_marginal": curve24    (pooled across seeds, 4.5)
    }
  },
  "verdicts": {
    "session_arrival":    [session-gate verdict records, both stages],
    "session_parent_vol": [same],
    "session_walltime_60": [same], "session_walltime_300": [same],
    "walltime_pooled_60": existing verdict-record schema,
    "walltime_pooled_300": existing verdict-record schema,
    "mid_rms" and the three "minute_range_*": the existing
      protocol-10 verdict-record schema, unchanged, by explicit
      reference - metric keys "mid_rms", "minute_range_p99",
      "minute_range_p99.9", "minute_range_max"
  }
}
```

Existing writer discipline reused: sorted keys, one-space indent,
`allow_nan=False`, final newline; a non-finite required value refuses
rather than serializing as a string. The artifact carries the exact
MATERIALIZED arrays installed into scratch profiles.

Selftest additions mirror the doctrine: every new tolerance boundary
inclusively; the all-sessions-qualify refusal (parent and both
horizon floors separately); the zero-heavy cell fixture (a cell more
than half zeros yields the trimmed-mean scale, not zero); the
endpoint-hour attribution fixture (an off-by-one hour lands a
different answer); the frozen-DOW conditional estimator against a
hand-computed `C`/`E`/`w` fixture where the marginal estimate and the
conditional estimate DIFFER (the Sun-Thu hours 22-23 asymmetry
expressed in miniature); array-override serialization round-trip;
materialization idempotence; the 23-generated-session refusal both
ways; family isolation of the new probes; the atomic landing-group
verdict paths, all three branches (session fail blocks everything;
envelope-only fail lands arrays with vol_scalar declared; pooled-RMS
fail refuses protocol 11 outright even with a passing envelope).

Built dirty; committed in H-commit on its passing selftest
(`python3 analysis/mnq_fit.py selftest`) together with G2, R2, T.

**Brick M1 - preflight.** From the clean H-commit tree:
`python3 analysis/mnq_fit.py preflight`. The moved sub-contract hash
forces the rerun; a refusal stops the spec.

**Brick M2 - fit.** Same clean tree:
`python3 analysis/mnq_fit.py fit` writes `analysis/mnq-fit.json`
atomically, bound to input hashes, the new sub-contract hash, the
fresh preflight artifact and the harness tree commit. The artifact
does not bind the LATER preset commit; instead it records the exact
materialized scratch arrays, and Brick L's tests prove the preset
resolves to those same values.

**Brick V - verdicts.** Family probes and the final combined run per
4.5; both reviewers read the artifact; the landing set derives from
the recorded verdicts alone.

**Brick L - the landing.** In ONE change: `mnq.toml` session arrays
(the atomic group, only on the amended 2.3 landing predicate) with
provenance `kind = "fitted"`, corpus
`"MNQ.v.0 GLBX.MDP3 TBBO, job GLBX-20260805-HAPEWPABKG"`, window
`"2026-07 full month, 22 usable sessions"`, estimator named in the
preset comment block (per-parent quote-mid robust scale; conditional
frozen-DOW arrival); the caveat prose above `[instrument.session]`
rewritten (the volume-proxy caveat retires for `intensity_hour`, the
per-minute lineage note retires for `vol_hour`, the dow_weight NQ-bar
caveat stays); `vol_scalar` per its verdict (fitted, or
declared-best-candidate with the recorded miss); `TAPE_PROTOCOL_VERSION`
10 -> 11; `fitted_mnq_effective_values_are_the_artifact_values`
extended to pin the session arrays, `vol_scalar` and provenance
kinds; `mes_inherits_the_mnq_fit_loudly` re-blessed with the
inherited session evidence visible. Verification on the BUILT tree,
exact commands:

```
brokkr check --gate
brokkr test -p mogwai-data session_modulation_reproduces_curves
brokkr test -p mogwai-data clean_regime_is_byte_identical
brokkr test -p mogwai-data calendar_free_profiles_are_untouched_by_calendar_aware_normalization
brokkr test -p mogwai-data session_profile_rejects_non_normalized_curves
brokkr test -p mogwai-data a_quote_precedes_every_parent_burst
brokkr test -p mogwai-data the_trade_displacement_never_varies
brokkr test -p mogwai-data synthetic_spread_decomposition_at_protocol_seven --timeout 280
brokkr test -p mogwai-server fill_distribution_matches_the_golden
```

The invariant and golden tests in this list receive NO re-bless - any
movement there is an implementation failure, not a re-bless
opportunity. The two preset-resolution expectations
(`fitted_mnq_effective_values_are_the_artifact_values`,
`mes_inherits_the_mnq_fit_loudly`) move DELIBERATELY as specified
above; they are the only sanctioned expectation changes.

**Brick B - composition remeasure (unconditional, on the UNCOMMITTED
Brick L tree).**
`brokkr run mogwai -- tick-composition --out analysis/tick-composition-protocol-11.json`,
then `python3 analysis/tick_composition_ratios.py --mode independent_10_11`.
Any ceiling resize under the standing policy joins the Brick L
keep/revert unit; no committed state carries protocol 11 with
unmeasured ceilings.

**Brick D - documentation.** `docs/presets.md` and the protocol
history in `reference/` updated; `reference/performance.md` gains the
protocol-11 composition section; `DATA-PURCHASE-REPORT.md` records the
volume-proxy obligation resolved and the session-fit supersession;
the `notes/` fit report written from the artifact. Bundled with the
code commits. Gate: `brokkr check --gate` green after the edits.

## 6. MES inheritance

Unchanged from protocol 10 (`notes/mnq-tbbo-fit-spec.md` section 6):
MES inherits every refitted session value through `preset = "MNQ"`,
loudly, corpus strings naming MNQ evidence, the ES/MES purchase
remaining the recorded route to ending the borrow.

## 7. Keep/revert and stopping rule

The H-commit (G2 + R2 + T + H) is an independent instrumentation-only
landing kept on its own gates regardless of the fit outcome. Bricks L
and B are ONE keep/revert unit. The measurement bricks carry no revert
obligation - artifacts stay as the record.

Out of scope, named: the tail-shape family and any change to GARCH
constants, Student-t df, bounce, sweep/level-step, drift recentering
(protocol 12, which also inherits the hourly wall-time contour
evidence); reopen gaps (separate phenomenology, its own future item);
`dow_weight` refit; the dynamic width/displacement response (14.4-A);
`mnq06`, MBO, ES/MES evidence; fingerprint re-anchoring; any
`SessionModulator` semantic change; calendar values; the legacy
`horizon_vol` fields. If a LANDING GATE fails (the amended 2.3
predicate - the hourly wall-time diagnostics are recorded, never
gating), nothing lands and the outcome is a measured-failure report -
a legitimate result of this spec, not a failure of it.
