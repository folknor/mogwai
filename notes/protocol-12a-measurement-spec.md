# Protocol 12a: the tail and aggregation measurement landing

REVISION 12 - the revision-10 freeze (Brick F signed by the
reviewing codex session 019fd565-1f36-7ab2-83e1-77af1b32b326 on
2026-08-06) plus the four narrow amendments A-D negotiated at the
Brick O implementation review, plus the two narrow amendments E-F
negotiated at the Brick O defect-repair review (co-signed by codex
session 019fd70c-0881-70c1-a3b8-5bfdc2879a08 on 2026-08-06; the
original session's cache had gone cold and was unresumable, so the
repair review restated the full context to a fresh session). The
repaired Brick O implementation was then verified against this
revision: session 019fd724-5f17-7e80-9720-388af117df9e signed the
eight repairs with two blockers (validator enforcement, the
replicate-gate override), and session
019fd731-0d4c-74f3-8046-fccaf7906a84 signed the final tree with no
residual violations on 2026-08-06. Brick G followed the same
protocol on 2026-08-06: session 019fd792-89b8-7783-bac1-2f0beb778373
signed the design with three rulings (complete sessions only; local
ARCH/GARCH constants pinned by coefficient recovery over traces;
Brick G owns cost12a, the cache runner and the eight FINAL walks)
and eight corrections; session 019fd7b3-4da1-73a3-a026-6c6d84e04dd1
found four residual violations in the implementation (cross-language
log-mid arithmetic, trade-driven initiation closure, a stale
arch_share_next under a superseding largest innovation, duplicate
shared-control refusals); session 019fd7c4-ae4e-7c52-9ba4-b80497657832
signed the repaired Brick G with no residual violations. The Brick M
driver went through four rounds on 2026-08-06: session
019fd7e1-a006-70a3-b64c-4f420a718117 ruled on the design (keep all 23
complete generated sessions; live observed rerun cross-checked against
the cache; cost-attestation replays of all eight walks under the
external sampler instead of the cached VmHWM figures; semantic gates
beyond the schema validator); sessions
019fd7ed-7810-7740-a1f6-02555306797e,
019fd805-22b1-7e53-a61c-2dad889ff4b6 and
019fd812-d81e-7711-9e47-f5a1a8f1c39d drove the read-only Brick G cache
lookup, sampler-death refusal, type-strict typed-canonical
comparisons, boolean-proof cost gates and mixed-type usable-list
refusals to a clean signature. The
Brick O implementation surfaced eight contract violations; per the
freeze protocol the brick failed, the amendments were argued to
consensus, and this revision writes them in. Revision 1 was refused
with 22 blockers, revision 2 with 11, revision 3 with 6, revision 4
with 5, revision 5 with 2, revision 6 with 2, revision 7 with 2,
revision 8 with 1, revision 9 with 1.

Amendment log (all six co-signed in negotiation, incorporated into
the body below):

- A: `PermRecord` carries per-horizon sufficient statistics
  (`return_count`, `sum_abs`, `max_abs`) instead of derived robust
  scales, with the frozen session-hour segment-combination rule and
  the zero convention (emitted zero returns count; all-zero windows
  give zero sums; no emitted windows give all-zero fields and the
  combined floor refuses).
- B: `Block4Map` gains the literal `"all"` pooled-hours key;
  `diagnostics.warmup_exclusions` stays integer-hour keyed.
- C: `boundary_localized` on a FIRED child-walk/reversion/garch rung
  is Boolean only when every localization input qualifies; otherwise
  null WITH a matching `RefusalRec` (never a fabricated false).
  Localization is metadata; its refusal does not revoke eligibility.
- D: `bootstrap.per_family` gains `inventory_complete`; a family
  with any refused required metric has `critical_value = null`,
  every envelope-dependent subcheck false, and cannot fire; the
  computable metrics keep their point/seed/fold evidence with null
  envelope fields.
- E: `worsening_23` is evaluated only after the signed-reversion rung
  fires. If the measurement produces its point estimate, standard
  error, UCB, and all 10,000 required bootstrap values, the existing
  uniform-versus-hour-resolved rule applies. If the measurement is
  refused, `diagnostics.worsening_23 = null`,
  `uniform_eligible = null`, and `required_resolution = null`.
  Exactly one logical `RefusalRec` with `scope = "reversion"` and
  `cell = "worsening_23"` owns those refusal-caused nulls; it appears
  once in top-level `diagnostics.refused_cells` and is mirrored once
  in the fired reversion rung's `refusals`. A refused measurement
  must never be coerced to `uniform_eligible = false` or
  `required_resolution = "hour-resolved"`. When the reversion rung
  does not fire, `diagnostics.worsening_23`, `uniform_eligible`, and
  `required_resolution` are null by inapplicability, with no
  `worsening_23` refusal record.
- F: a standardizer residual omitted for a nonpositive or non-finite
  trailing scale (the frozen Q1 exception) is recorded as one
  `RefusalRec` per `(side/seed, session, hour)` with a positive
  omission count, carried in that session's scoped `refusals` array
  and mirrored into top-level `diagnostics.refused_cells`; no
  duplicate record for the pooled `"all"` cell; the current return
  still enters history. These are the SOLE `RefusalRec` class owning
  omitted observations rather than refusal-caused nulls - the one
  narrow exception to the reverse direction of the section-10
  refusal-null pairing, and the exact-key validator recognizes them
  as such. A post-omission count falling below `MIN_RESIDUAL_CELL`
  produces a separate, ordinary family-metric refusal that owns that
  metric's null.
- Q1 ruling: required family cells qualify only when EVERY usable
  observed session (and, per seed, every complete generated session)
  meets the applicable floor - one failing session refuses the
  metric and makes its family inventory incomplete; unrelated
  families continue. No K-of-N fallback. Frozen exceptions:
  conditional adequacy keeps the 5.2 pooled/per-seed support rule;
  count windows use scheduled-exposure completeness; permutations
  use the Amendment-A combined session-hour count; standardizer-
  refused residuals are omitted before the `MIN_RESIDUAL_CELL` test.

Written against `reference/technical-implementation-spec.md`. Spawned
from the protocol-12 obligations recorded in
`notes/protocol-11-session-repair-spec.md` (section 1 as amended at
Brick V, and the RESULT records): the minute-range envelope failures
and the hour-dependent 60 s / 300 s wall-time contour failures that
protocol 11 inherited to its successor.

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it.

Both reviewers argued this spec's shape adversarially before drafting.
The central concession, recorded because it reverses the opening
position: the top-minute evidence describes two strata (high-parent
minutes at cash hours, low-parent minutes with large per-parent range
at quiet hours) but does not identify two mechanisms, because minute
range grows roughly with sqrt(parent count), so range divided by count
falls mechanically as counts rise, and the 256 top-minute records are
censored maxima without an all-minute denominator. A single t-GARCH
feedback process (one exceptional innovation raising sigma, persistence
0.979 carrying the episode) can generate both strata. Mechanism COUNT
is therefore an output of this measurement landing, not an input.

---

## 1. The goal

Land the measurement instruments and evidence that SELECT the
protocol-12 mechanism family (or prove no family eligible), without
changing the tape generation path. Five evidence blocks, two
permutation counterfactuals, one count-substitution counterfactual, a
preregistered Boolean eligibility ladder, and a committed verdict
artifact. `TAPE_PROTOCOL_VERSION` stays 11: nothing in this landing
may touch the generator state machine, and Brick G's gate pins that.

Protocol 12b - the mechanism implementation - is EXCLUDED and may not
be drafted until this landing's verdicts exist. That is not deferral:
12b's content is unknowable before these measurements, which is the
same argument that split protocol 11 from 12.

### 1.1 Proceed/close threshold

The landing closes with one of:

- `family-eligible: <ordered list>` - at least one ladder rung fires.
  Every fired rung is recorded independently (families are NOT
  mutually exclusive; co-firing is interaction evidence). 12b is
  drafted for the FIRST eligible family in ladder order; a
  two-mechanism 12b becomes drawable only if that family repairs its
  discriminating diagnostics in 12b and leaves a separately
  identified residual.
- `no-family-eligible` - a valid verdict, taken to the owner as a
  decision point, not forced into a knob.

### 1.2 Inherited obligations restated (binding on 12b)

- The hourly 60 s and 300 s wall-time bands `[0.8, 1.25]` are HARD
  gates for the 12b landing (Brick V amendment), measured with the
  protocol-11 estimator: the one-maximum-trimmed mean absolute
  fixed-horizon return (`robust_scale` below). Replacing them
  requires a defended preregistered estimator BEFORE 12b
  implementation, never after a miss.
- `vol_scalar` carries declared-best-candidate status; the envelope
  failure is 12b's primary target.
- 12b adds a two-sided body gate: the p99 minute-range statistic gets
  a LOWER acceptance bound from the lower tail of the same resampled
  envelope machinery, so an over-damped model cannot pass by making
  every minute too small.
- The standing instrument-resolution decision (section 8) binds 12b.

## 2. Survey of the ground

### 2.1 The evidence this spec starts from

From the landed protocol-11 artifact `analysis/mnq-fit.json`
(harness commit ac6b016, corpus job GLBX-20260805-HAPEWPABKG,
22 usable sessions, 8 generated seeds):

- Minute-range envelope: generated p99.9 per seed 433-607 ticks vs
  resampled bound 399; per-seed max 812-3614 vs bound 968. p99 passes
  (215-241 vs 250). Body right, extreme tail 2-4x heavy.
- Hourly wall-time contour: 300 s fails at UTC hours 19, 20, 23
  (worst ratio 0.371 at hour 20, generated hot); 60 s fails at hour
  20 only (0.257). Hour 23 is COLD (0.78 diagnostic), so no uniform
  reversion knob can repair the contour.
- Per-seed top-32 worst minutes with trace coordinates.

Frozen hour sets used by the ladder: `FAIL_HOURS_300 = {19, 20, 23}`,
`FAIL_HOURS_60 = {20}`, `HOT_HOURS = {19, 20}` (generated hot),
`COLD_HOURS = {23}`.

### 2.2 The existing instruments (what is NOT built here)

- `VolTrace` (`crates/mogwai-data/src/generated/source.rs`) records,
  per parent: raw and standardized innovation, candidate and realized
  sigma2, sigma-cap and clamp flags, garch scale, unclipped and
  clipped base return, session and regime multipliers, previous
  realized return, realized return, `mid_before`/`mid_after`.
- `mogwai gen --type trace` emits one JSON line per parent in a
  window; the walk is byte-identical with tracing enabled, pinned by
  `trace_consumes_no_draws_and_leaves_the_tape_byte_identical`
  (crate `mogwai-data`).
- The fit harness `analysis/mnq_fit.py`: the streaming `observe()`
  corpus pass, the walk cache keyed on harness commit, the
  protocol-11 segment-local as-of and endpoint-hour conventions, the
  frozen `MNQ_DOW_WEIGHT`, splitmix64 seeding, nearest-rank
  quantiles, and the selftest harness.

Everything Block 5 needs is DERIVABLE from these: pre-draw sigma is
the PREVIOUS parent's realized sigma; latent-mid range from
`mid_before`/`mid_after`; trade range from ordinary printed prices;
signed runs from realized-return signs; the ARCH contribution from
`base_return` and the frozen ARCH coefficient; clamp counts from the
existing flags. 12a extends only offline consumers.

### 2.3 The version-rule reading (argued and settled)

An equality test proving a generator tap consumes no draws does NOT
exempt a generator-path change from the `TAPE_PROTOCOL_VERSION` bump:
the rule is deliberately conservative because nothing can detect a
missed bump. Resolution: 12a avoids the disputed change entirely.
Changes to analysis code, or to server-side consumers that only read
existing events and `VolTrace`, need no bump. Any new field, branch,
callback, buffer or observation hook inside `mogwai-data`'s generator
path would require the bump and is out of scope for 12a by
construction.

### 2.4 Walk-cache verdict

The protocol-11 summary caches CANNOT feed any complete 12a block:
Block 1 lacks the joint minute distribution, quote ranges and segment
buckets; Block 2 lacks sub-hour counts; Block 3 lacks the 1/5/15 s
horizons, component covariances and segment detail; Block 4 lacks
ordered parent returns; even the cached 60 s / 300 s records lack the
new segmentation. They are NOT reused. Brick G performs exactly one
fresh full FINAL walk per seed 1-8 into a distinct `measure12a` cache
keyed by the full command, harness commit and
measurement-subcontract hash.

## 3. Definitions and populations

### 3.1 Minutes, parents, ranges

A populated minute is a UTC-aligned half-open minute `[m, m+60s)`
containing at least one structurally valid TBBO row (observed side)
or at least one child trade (generated side). Parent count `N` is the
number of sided inferred parents attributed to the minute by
first-child timestamp. Trade range uses every structurally valid
print, including unsided observed rows. Quote-mid range uses
valid-book inferred parents only; it is recorded in half-ticks (the
mid can sit on a half-tick). A minute with prints but zero sided
parents occupies the parent-count-zero bin and has no `range/sqrt(N)`
value. All quantiles everywhere are exact nearest-rank, matching the
harness convention.

### 3.2 Bins, labels, and timestamp attribution

Parent-count bins (exact half-open intervals):

```text
{0}, [1, 65), [65, 257), [257, 1025), [1025, 4097), [4097, inf)
```

Diagnostic strata ONLY; never used in forensic control matching
(3.4). Exact `N` is retained in the Block 1 sparse histogram - the
bins never coarsen a computation, only reports and the count
substitution.

Segment-relative labels, two independent axes, one label per axis per
minute (a post-halt minute can be both reopen-near and close-near;
those effects are not pretended separable):

```text
since_segment_open:   [0, 300) | [300, 1800) | [1800, inf)
until_segment_close:  (1800, inf) | (300, 1800] | (0, 300]
```

Attribution, frozen: minute labels are evaluated at MINUTE START -
`since_segment_open = minute_start - segment_origin` and
`until_segment_close = segment_end - minute_start`. Fixed-horizon
returns receive segment labels at their ENDPOINT boundary. Block 2
count windows are half-open, segment-origin-aligned windows
`[origin + jW, origin + (j+1)W)` strictly contained in the open
segment, attributed by endpoint hour; windows crossing a UTC-hour
boundary are excluded; lag-1 pairs never cross a segment or UTC-hour
boundary; active runs reset at either boundary.

### 3.3 Cell qualification floors

The protocol-11 floors cover parent cells and the 60/300 s chains.
The new populations get explicit floors; a cell below floor is a
recorded refusal for REQUIRED statistics (the ladder inventories,
section 6.4 - a refused required cell fails its rung closed) and an
empty diagnostic bin (recorded, not failed) for descriptive ones.

Q1 ruling (all-session qualification, frozen): a required family
cell qualifies only when EVERY usable observed session meets its
floor, and, per generated seed, every complete generated session
meets it. Block 1 pools histograms only after that check; Blocks 3
and 4 therefore always carry all session votes. One failing session
refuses the metric and makes its family inventory incomplete
(Amendment D); only families consuming that cell fail - unrelated
families continue. No K-of-N fallback. Frozen exceptions: the 5.2
conditional-adequacy support rule; count windows judged by
scheduled-exposure completeness; permutations judged on the
Amendment-A combined session-hour count; standardizer-refused
residuals omitted before the `MIN_RESIDUAL_CELL` test.

```text
MIN_1S_CELL_RETURNS   = 2500
MIN_5S_CELL_RETURNS   = 500
MIN_15S_CELL_RETURNS  = 160
MIN_60S_CELL_RETURNS  = 40
MIN_300S_CELL_RETURNS = 6
MIN_RESIDUAL_CELL     = 1000   (post-warmup residuals per session-hour)
MIN_MINUTES_CELL      = 30     (populated minutes per session-hour)
```

Boundary-specific floors, applying ONLY to the named boundary cells
of section 6.4 (a first-300 s cell holds at most 5 minutes per
session; monthly inference still receives 22 session votes):

```text
MIN_BOUNDARY_MINUTES_CELL     = 4
MIN_BOUNDARY_60S_CELL_RETURNS = 3
```

Count windows include every scheduled half-open window in the open
segments, zero-count windows included; they are judged by expected
exposure completeness (scheduled windows present), not
populated-window count.

### 3.4a Deterministic derivation helper (shared by 3.4b and 5.1)

The multi-field permutation and control-tie derivations use ONE
helper over the unary `splitmix64` (bit-identical to
`crates/mogwai-protocol/src/seeds.rs`; tuple-mixing and shuffle test
vectors land in Brick O's selftest). Section 6.1 retains its
separately frozen bit-packed bootstrap derivation.

```text
tuple_mix(base, fields):
    x = base
    for value in fields, in listed order:
        x = splitmix64(x ^ u64(value))
    return x
```

Field encodings: session date as the integer `YYYYMMDD`; variant
tags sign = 0, magnitude = 1; all other fields as their natural u64.

### 3.4b Forensic extremes and controls (frozen selection)

Per generated seed: the maximum-range minute; the maximum
`range/sqrt(N)` minute over minutes with `N >= 1`; deduplicated.
Candidate controls for each extreme are populated minutes in the same
seed, segment and UTC hour, excluding every selected extreme and every
top-32 minute, with trade range at or below that seed/segment-hour
median. Select the candidate minimizing
`abs(log1p(N_control) - log1p(N_extreme))`; break ties by rank
`tuple_mix(CONTROL_TIE_BASE_SEED, [generated seed, extreme minute
start ns, candidate minute start ns])` with the LOWER rank winning,
then by earlier minute. Refuse (recorded) if no candidate exists.

### 3.5 Cross-session and cross-seed aggregation (the contract every
statistic obeys)

Per-session records are SUFFICIENT records - nothing coarser than
what reconstructs every downstream statistic exactly:

- Block 1: an exact sparse joint histogram keyed by
  `(N, quote_range_half_ticks | null, trade_range_ticks, hour,
  since_open_bin, until_close_bin)` with occurrence counts. Every
  Block 1 quantile, exceedance, ratio and the count substitution
  derive from it exactly.
- Block 2, per (hour, window length): scheduled and zero window
  counts; the exact count histogram; the run-length histogram; and
  the lag-1 sufficient moments `paired_lag_count, sum_x, sum_y,
  sumsq_x, sumsq_y, sum_xy` (a lone cross-product cannot reconstruct
  the correlation).
- Block 3, per (hour, horizon): return count, `robust_scale`,
  `rms_scale`; per (hour, adjacent-horizon pair): window count, the
  variance ratio, `C(h,H)` and its normalization.
- Block 4, per session-hour: the quantile/ratio/exceedance record.
- Permutations, per `(session, segment, hour, variant, replicate)`
  and horizon: `return_count`, `sum_abs` and `max_abs` over emitted
  fixed-horizon returns. Emitted zero returns count; an all-zero
  population has zero sums; no emitted windows gives all-zero
  fields (Amendment A).

Aggregation:

- Observed monthly: Blocks 1 and 2 pool the per-session histograms
  and moments; Blocks 3 and 4 take one vote per qualifying session,
  median across sessions. Permutations: within each
  `(session, hour, variant, replicate, horizon)`, combine the
  segment sufficient records per Amendment A, apply the combined
  horizon floor, and derive ONE session-hour robust scale; monthly
  aggregation takes the median across qualifying sessions per
  replicate index, then the median across the 16 replicate indices.
- Generated: EVERY scalar generated statistic is computed per seed
  (pooling that seed's month for Blocks 1-2, session-median for
  Blocks 3-4) and centralized by MEDIAN ACROSS THE EIGHT SEEDS. A
  cross-seed pooled histogram may be committed as a diagnostic but is
  never a ladder input.
- The bootstrap (6.1) resamples the observed PER-SESSION RECORDS and
  reruns exactly these aggregation rules; shuffles are never rerun
  inside the bootstrap.
- Empty/non-finite: refused cells carry `null` values plus a refusal
  record naming cell and reason; empty diagnostic bins carry zero
  counts.

## 4. The five evidence blocks

Blocks 1-4 are computed identically on the observed TBBO parent
stream and each generated seed's parent stream, under the protocol-11
segment-local as-of and endpoint-hour conventions. Nothing shuffles,
standardizes or windows across a halt, reopen, session open or close.
Block 5 is GENERATED-ONLY: the observed corpus has no latent
innovation or sigma state; its observed comparators are the Block 1,
3 and 4 statistics.

### Block 1: all-minute joint distribution

From the sparse histogram, per hour and segment-label pair:
populated-minute count; parent-count p50/p90/p99/p99.9; quote-mid and
trade range p50/p90/p99/p99.9 within each parent-count bin;
`trade_range_ticks / sqrt(N)` p50/p90/p99 (exact `N`); exceedance
counts and denominators above 399, 642, 968 ticks (strict `>`); per
hour the ratio `trade_range_p99 / quote_range_p99` with the quote
p99 converted from half-ticks to ticks before division, refused if
the quote p99 is zero or null. A populated minute with no valid
quote-mid parent has `quote_range_half_ticks = null`; null minutes
do not enter quote-range quantiles, and every quote-range statistic
carries its own explicit denominator. A minute with exactly one
valid quote has a zero quote range. No per-minute rows are
committed.

### Block 2: sub-hour arrival clustering

Per hour and window length in `COUNT_WINDOWS_S = {1, 5, 60}`:
zero-count fraction; mean; Fano factor (variance/mean); count
p90/p99/p99.9; lag-1 count autocorrelation (from the sufficient
moments); active-run-length p90. Zero-count fraction and lag-1 count
autocorrelation are DIAGNOSTICS only (they are zero-capable and
sign-capable; section 6.4 excludes them from the envelope inventory).

### Block 3: aggregation signature

Fixed-wall returns at `WALL_HORIZONS_S = {1, 5, 15, 60, 300}`,
segment-origin aligned, hour-crossing windows excluded, endpoint-hour
attributed. Per hour and horizon, BOTH scales from the same qualified
cells:

```text
robust_scale(H) = (sum(abs(R_H)) - max(abs(R_H))) / (n - 1)
rms_scale(H)    = sqrt(mean(R_H^2))
```

`robust_scale` is the inherited-contour estimator: it serves the
hourly contour comparisons, permutation gap closure, and rungs 4-5.
`rms_scale` serves the variance-ratio and covariance decomposition
only. Per hour and adjacent-horizon pair `h, H = k*h`, over the SAME
complete windows (segment-origin-aligned `H` windows whose `k`
components all exist):

```text
VR(h,H) = mean(R_H^2) / (k * mean(r_h^2))
C(h,H)  = mean(R_H^2 - sum_j r_h_j^2)      (= 2x cross-product sum)
C_norm  = C(h,H) / mean(R_H^2)
```

with window counts; zero or below-floor denominators refuse the
record. Lag-1 parent-return autocorrelation per hour stays one
descriptive scalar. Hour 20 additionally reported per segment-label
pair.

### Block 4: conditional tail evidence

The model-free past-only standardizer, identical on both sides:
return = adjacent valid quote-mid log return attributed to the
endpoint parent; history = endpoint returns in `[t - 300 s, t)`, same
open segment, may cross an hour boundary, never a
halt/reopen/open/close; minimum history 1000 returns, earlier returns
warmup-excluded (recorded, not failed); scale =
`(sum(abs(r_i)) - max(abs(r_i))) / (n - 1)` with the current return
excluded; refuse the residual if the scale is non-finite or
nonpositive; `z_t = r_t / scale_t`.

Zeros stay in history and residual populations. Per session-hour:
zero fraction; nonzero `abs(z)` p90, p99, p99.9; p99/p90 and
p99.9/p99; exceedance fractions at strict `>` 4, 8, 16
(`RESIDUAL_EXCEED_MULTIPLES`), denominator = all emitted residuals,
zeros included; warmup-exclusion and residual counts. p99.9/p50,
tail exponents and Hill estimates are cut.

### Block 5: forensic summaries (generated-only)

Per selected extreme and control minute (3.4), from the single
streaming pass (Brick G):

- `largest_innovation_std`: max `abs(innovation_std)` in the minute,
  earlier parent breaking ties; its timestamp.
- `innovation_exceed_counts`: counts of `abs(innovation_std)`
  strictly above 4, 8, 16 (`INNOVATION_EXCEED_ABS`; unit-variance
  units, split from the trimmed-scale residual multiples),
  denominator = traced parents in the minute.
- `initiation`: true iff the largest-innovation parent's timestamp is
  at or before the instant the minute's running quote-mid range first
  strictly exceeds half its final value; FALSE (never a refusal) when
  the final quote-mid range is zero - a child-only extreme must stay
  visible.
- `sigma_start = sqrt(previous_parent.sigma2_realized)` (the parent
  BEFORE the minute's first parent, so an initiating first-parent
  shock is not hidden; refused only when the minute's first parent is
  the first parent of the measured walk),
  `sigma_peak = max sqrt(sigma2_realized)` within the minute,
  `sigma_end = sqrt(last_parent.sigma2_realized)`,
  `sigma_escalation = sigma_peak / sigma_start` (refused if
  `sigma_start` nonpositive).
- `latent_mid_range_ticks`, `quote_mid_range_half_ticks`,
  `trade_range_ticks`, `trade_to_quote_range_ratio`,
  `quote_to_latent_range_ratio` (ratios of independently attained
  extrema, NOT attribution fractions; they need not relate
  additively).
- `max_signed_run`: longest run of same-sign nonzero realized
  returns.
- `clamp_hits`: count of any of the three clamp/cap flags.
- `arch_share_next = GARCH_ARCH * base_return_i^2 /
  sigma2_candidate_{i+1}` for the largest-innovation parent, where
  `i+1` is the next parent IN THE MEASURED WALK (it may lie in the
  following minute); null only when no successor parent exists in the
  walk; plus the minute maximum of that share over parents with
  successors.

Raw trace lines are reproducible from seed and window and are NOT
committed.

## 5. The counterfactuals

### 5.1 Permutations (observed-side only)

`PERMUTATION_VARIANTS = {sign, magnitude}` - exactly two. Per cell
`(session, open segment, UTC hour)`, 16 replicates per variant:

1. **Sign shuffle** - shuffle signs among nonzero returns; timestamps,
   zero locations and the magnitude order stay fixed.
2. **Magnitude shuffle** - shuffle nonzero absolute magnitudes;
   timestamps, zero locations and the ordered nonzero sign sequence
   stay fixed.

Seeds and algorithm, frozen: the shuffle state is
`tuple_mix(PERMUTATION_BASE_SEED, [session date, segment index,
hour, variant tag, replicate index])` (encodings per 3.4a), and the
permutation is Fisher-Yates over the eligible values in original
stream order:

```text
state = tuple_mix(PERMUTATION_BASE_SEED, fields)
for i from n - 1 down to 1:
    state = splitmix64(state)
    j = state mod (i + 1)
    swap(values[i], values[j])
```

Equal timestamps retain original stream order.

Reconstruction (frozen): partition every adjacent valid-mid return by
its endpoint cell; shuffle within each cell; concatenate cells back
in original segment order; set the segment's initial synthetic
log-mid to zero; cumulatively reconstruct the parent-mid path at the
original endpoint timestamps; run the ordinary segment-origin 60/300 s
boundary algorithm over the reconstructed path, recording
`robust_scale_60` and `robust_scale_300` (the arbitrary initial level
cancels from log returns).

Interpretation contract: the sign shuffle moving observed hot-hour
wall-time vol toward the generated curve supports missing real-tape
reversion; the magnitude shuffle doing so supports
volatility-clustering composition; both moving it materially means
interaction or non-identifiability, not automatic two-family
eligibility; neither moving it falsifies both explanations.

### 5.2 Count substitution (generated-side, semi-analytic)

Per seed `s`, with `o[h,b]` the observed populated-minute share of
parent-count bin `b` within hour `h` and `g[s,h,b]` the generated
share:

```text
w[s,h,b] = o[h,b] / g[s,h,b]
```

Each generated minute receives its hour-bin weight; within each hour
the weights preserve the original generated total weight. Weight
edge cases, frozen:

```text
o > 0, g > 0: weight = o / g
o = 0, g > 0: weight = 0
o = 0, g = 0: weight = null, bin ignored
o > 0, g = 0: support refusal; the implicated hour fails
```

A refused implicated hour fails rung 2 closed while the rest of the
artifact completes (never borrow a nearest bin - that fabricates
support). Pool all weighted hours preserving the generated hour
mixture and compute ONE full-month counterfactual minute-range p99.9
and `> 968` exceedance rate per seed.

Weighted nearest rank, literal: sort `(range_ticks, weight)`
ascending by range then minute start; return the first range whose
cumulative weight is at least `q * total_weight`. Exceedance rate =
`sum(weight where range > 968) / total_weight`.

The rung-2 closure target `T_obs` is the observed pooled minute-range
p99.9 POINT ESTIMATE (causal gap closure); closure to the 399-tick
resampled acceptance bound is recorded separately as a delivery
diagnostic, never a ladder input.

Conditional adequacy guard (rung 2c), fully frozen: the implicated
hours are exactly `FAIL_HOURS_300`; the statistic is
`trade_range_ticks / sqrt(N)` p99; the zero bin is excluded (sqrt(N)
undefined). Qualification, frozen without contradiction: a REQUIRED
bin is one whose pooled OBSERVED count is at least
`MIN_MINUTES_CELL`; required generated support means EVERY generated
seed's count in that bin is also at least `MIN_MINUTES_CELL`; a
required observed bin lacking required generated support fails rung
2c CLOSED (recorded - it is never silently nonqualifying). For every
implicated hour and required bin the generated/observed ratio must
be `inside_with_envelope` of `[0.8, 1.25]`, and these conditional
metrics JOIN the arrival family's envelope inventory (in addition to
its six primary metrics) so the critical value maximizes over both
sets. If the conditional
range law fails, count substitution does not identify arrival as
sufficient - the counterfactual only carries the interpretation
"coarse arrival composition under an adequate conditional range law".

### 5.3 Gap-closure metric (shared)

For a target statistic `T` with observed value `T_obs`, generated
`T_gen` and counterfactual `T_cf`:
`closure = (log(T_gen) - log(T_cf)) / (log(T_gen) - log(T_obs))`
for generated-side counterfactuals, and
`closure = (log(T_cf) - log(T_obs)) / (log(T_gen) - log(T_obs))`
for observed-side counterfactuals. Refused if any input is
nonpositive or the denominator's absolute value is below
`GAP_CLOSE_EPS = 1e-9`.

Confidence rules, frozen:

```text
single-target LCB:     nearest-rank p5 of the 10,000 bootstrap closures
multi-target joint LCB: per bootstrap replicate take the MINIMUM
                        closure across the required cells, then
                        nearest-rank p5 across those 10,000 minima
worsening_23 UCB:      nearest-rank p95 of the 10,000 bootstrap
                        worsening values
```

## 6. The eligibility ladder

### 6.1 Uncertainty rule (shared by every rung)

- 10,000 fixed-seed circular moving-block bootstrap replicates over
  the 22 observed per-session records; block length 5 sessions; each
  pseudo-month 22 sessions; aggregation per 3.5. Draws, frozen for
  bit reproducibility: sessions sorted ascending by date before
  resampling; each replicate draws exactly FIVE circular block
  starts, each block contributing five consecutive sessions
  (wrapping), concatenated in draw order and truncated to 22; each
  block start is derived by the unary `splitmix64` (bit-identical to
  `crates/mogwai-protocol/src/seeds.rs`, with Python test vectors in
  the selftest):

  ```text
  replicate_index = 0..9999
  block_index     = 0..4
  x     = BOOTSTRAP_BASE_SEED ^ (replicate_index << 8) ^ block_index
  start = splitmix64(x) mod 22
  ```

  Centering: `theta_hat` is the ORIGINAL 22-session
  estimate; `SE` is the sample standard deviation of the 10,000
  `theta_b` with `ddof = 1`; the studentized value is
  `(theta_b - theta_hat) / SE`.
- Metric kinds, frozen per metric in the 6.4 inventories:
  - `log_ratio` (strictly positive statistics): the metric is
    `log(T_gen_central / T_obs)`; band predicates compare against
    `log([0.8, 1.25])`.
  - `raw_diff` (signed or zero-capable statistics, e.g. `C_norm`):
    the metric is `T_gen_central - T_obs`, bootstrapped and
    studentized in RAW space; its interval is tested against ZERO,
    never against a multiplicative band; direction is the sign of
    the difference.
- The standard error of each metric is the sample standard deviation
  across the 10,000 outer replicates; zero or non-finite SE refuses
  that metric. For each replicate, divide its centered metric by the
  frozen outer SE, take the maximum absolute value across the
  family's complete inventory, and use the nearest-rank p95 of those
  maxima as the simultaneous critical value. The simultaneous
  interval is the point estimate plus/minus critical value times SE.
  Two frozen predicates:

  ```text
  outside_with_envelope: point outside the band AND the simultaneous
                         interval excludes the nearest band edge
                         (raw_diff: interval excludes zero in the
                         claimed direction)
  inside_with_envelope:  point inside the band AND the ENTIRE
                         simultaneous interval contained in the band
  ```

  `inside_with_envelope` governs child-walk `b_mid_clean`, arrival
  `c_conditional` and boundary `b_comparator_clean`; every other band
  predicate is `outside_with_envelope`.
- Permutation statistics inside the bootstrap: the 16 shuffled
  records per session and variant are precomputed once; each
  pseudo-month is evaluated under all 16 replicate indices and its
  counterfactual statistic is their median.
- Seed rule, split by predicate kind: for an OUTSIDE metric, at
  least 7 of 8 seeds lie on the same outside side of the band; for
  an INSIDE metric, at least 7 of 8 seeds individually lie inside
  the band with NO same-direction requirement (a clean metric's
  seeds may straddle 1.0); for a RAW-DIRECTION metric, at least 7 of
  8 seed point differences have the claimed STRICT sign - zero
  supports neither sign - and `seed_same_side_count` records this
  same-sign count for both outside and raw-direction predicates.
- Predicate/kind constraints: `outside` and `inside` require
  `kind = log_ratio`; `raw_direction` requires `kind = raw_diff`.
- Fold rule, split by predicate kind (leave-one-calendar-week-out:
  ISO weeks of the session date; a fold qualifies if at least 15
  sessions remain; partial weeks are their own folds):
  - outside metric: every qualifying fold stays outside on the SAME
    side;
  - inside metric: every qualifying fold's point stays inside the
    band;
  - raw-direction (`raw_diff`) metric: every qualifying fold retains
    the claimed sign.

### 6.2 Ladder order and rungs

Order = smallest causal and behavioral blast radius that can explain
the failed outputs, NOT ease of implementation. Rungs are evaluated
top to bottom; EVERY fired rung is recorded; 12b takes the first.
Boundary is a localization dimension, not a standalone mechanism,
except as rung 6's residual.

1. **Child-walk isolation.** Fires iff, for at least one hour in
   `FAIL_HOURS_300`, the 8-seed median of generated
   `trade_range_p99 / quote_range_p99` divided by the observed ratio
   is `outside_with_envelope` of `[0.8, 1.25]`, AND the generated
   quote-mid `robust_scale_60` and `robust_scale_300` ratios at that
   hour are `inside_with_envelope` (the excess lives in the print
   layer, not the mid).
2. **Arrival sufficiency.** Fires iff (a) at least one inventory
   metric (6.4) is outside `[0.8, 1.25]` with envelope, AND (b) the
   count substitution closes at least `GAP_CLOSE_MIN = 0.50` of the
   pooled minute-range p99.9 log gap with bootstrap lower confidence
   bound above `GAP_CLOSE_LCB_MIN = 0.25`, AND (c) the conditional
   adequacy guard (5.2) holds.
3. **Innovation tail.** Fires iff (a) the generated/observed ratio of
   nonzero `abs(z)` p99.9/p99 is outside `[0.8, 1.25]` with envelope,
   AND (b) `initiation` is true with `largest_innovation_std > 8` for
   the max-range extreme in at least 7 of 8 seeds, AND (c) the same
   conjunction holds for at most 2 of 8 matched controls.
4. **Signed reversion.** The family fires iff (a) the sign shuffle's
   closure of the `robust_scale` wall-time log-ratio gap is at least
   0.50 at EVERY hour in `HOT_HOURS` at 300 s and at hour 20 at
   60 s, with the multi-target joint LCB above 0.25, AND (b) the
   closure sign agrees across every qualifying leave-one-week fold,
   AND (c) covariance direction: at BOTH `HOT_HOURS`,
   `D_C = C_norm_generated - C_norm_observed` has simultaneous
   `interval_low > 0` (the observed covariance is more negative than
   the generated - real reversion the generator lacks). Subcheck
   keys: `{a_closure, b_folds, c_covariance}`. Hour-23 resolution,
   settled in 12a, does not block the family - it is metadata after
   eligibility, selecting the 12b form:

   ```text
   gap_real_23  = abs(log(G_23 / O_23))
   gap_perm_23  = abs(log(G_23 / P_23))
   worsening_23 = gap_perm_23 - gap_real_23
   ```

   at 300 s with `robust_scale`; `uniform_eligible = true` iff the
   `worsening_23` UCB (5.3) is `<= 0`; otherwise
   `uniform_eligible = false` and
   `required_resolution = "hour-resolved"`. Amendment E:
   `worsening_23` is evaluated only after this rung fires; a refused
   measurement records `uniform_eligible = null` and
   `required_resolution = null` with exactly one matching
   `RefusalRec` (`scope = "reversion"`, `cell = "worsening_23"`) -
   never a fabricated `hour-resolved`.
5. **GARCH persistence.** Fires iff (a) the magnitude shuffle's
   closure of the `robust_scale` gap is at least 0.50 at every
   `HOT_HOURS` 300 s cell and at hour 20 at 60 s, with the
   multi-target joint LCB above 0.25 (the pooled minute-range p99.9
   alternative of revision 3 is CUT: no permuted minute-range
   statistic exists - reconstructing child-price ranges from
   shuffled quote-mid returns would require another, unspecified
   model), AND (b) `sigma_escalation >= 2.0` for the max-range
   extreme in at least 7 of 8 seeds while the matched controls'
   median `sigma_escalation < 1.25`. Subcheck keys:
   `{a_closure, b_escalation}`. (Rev-2's rung 5c is DROPPED: an
   exceptional innovation causing sigma escalation is the ordinary
   t-GARCH composition; innovation and GARCH may both fire, and
   ladder order selects innovation first.)
6. **Boundary-local state.** Fires iff no rung above fired, AND at
   least one of the four boundary metrics (6.4) is
   `outside_with_envelope` of `[0.8, 1.25]`, AND the same metric
   over that boundary case's MATCHED comparator cell is
   `inside_with_envelope`:

   ```text
   pre_halt_close boundary:    since_open [1800, inf), until_close (0, 300]
   pre_halt_close comparator:  since_open [1800, inf), until_close (300, 1800]
   post_halt_reopen boundary:  since_open [0, 300),    until_close (300, 1800]
   post_halt_reopen comparator: since_open [300, 1800), until_close (300, 1800]
   ```

Localization flag: `boundary_localized` is `bool | null`. It is
computed ONLY for a FIRED child-walk, reversion or GARCH rung (whose
Block 1/3 inputs exist label-resolved): true iff the rung's
discrepancy magnitude in the boundary cells is at least twice its
interior magnitude. The child-walk discrepancy is the label-filtered
`trade_range_p99 / quote_range_p99` print-excess log ratio; reversion
and GARCH use the label-filtered hour-20 `robust_scale_60` log ratio;
boundary magnitude = max over the two boundary cases; interior = the
matched interior cell; point estimates, no envelope. Amendment C:
the flag is Boolean only when every localization input qualifies and
the ratio is defined; otherwise null WITH a matching `RefusalRec` -
never a fabricated false. Localization is metadata: its refusal does
not revoke eligibility. It stays null without a refusal for every
unfired rung and for arrival, innovation and boundary (Blocks 2 and
4 carry no label-resolved statistics).

### 6.3 Reversion falsifiers (recorded so 12b cannot un-know them)

The reversion family is rejected iff any of its three rung subchecks
fails - `a_closure`, `b_folds`, `c_covariance` - and by nothing
else. There is no second rejection system: 6.2's rung 4 is the whole
test. Cross-family evidence (rungs 1, 3, 5 firing) is NOT a
falsifier - co-firing is recorded as interaction evidence.

### 6.4 Family metric inventories (the exact envelope sets)

All `log_ratio` kind unless marked `raw_diff`. These inventories are
the REQUIRED statistics of 3.3; a refused required cell fails its
rung closed and is recorded.

- Child-walk (9): the three print-excess ratios
  (`trade_range_p99 / quote_range_p99` per hour in `FAIL_HOURS_300`,
  outside predicates) PLUS the six quote-mid clean metrics
  (`robust_scale_60` and `robust_scale_300` per hour in
  `FAIL_HOURS_300`, inside predicates). The critical value maximizes
  over all nine. The rung logic pairs them BY HOUR: rung 1 fires at
  hour h only when that hour's print-excess is outside AND that same
  hour's two quote-mid metrics are inside - unrelated hours cannot
  satisfy each half.
- Arrival (6 primary + conditional): Fano factor at 60 s and count
  p99 at 60 s, per hour in `FAIL_HOURS_300`, PLUS one conditional
  adequacy metric for every required hour/bin under 5.2. (Zero-count
  fraction and lag-1 count autocorrelation are diagnostics only -
  zero-capable and sign-capable.)
- Innovation tail (4): nonzero `abs(z)` p99.9/p99 per hour in
  `FAIL_HOURS_300` plus pooled all-hours; rung 3a fires when AT
  LEAST ONE of the four is `outside_with_envelope`.
- Reversion (5): `robust_scale_300` at `HOT_HOURS`,
  `robust_scale_60` at hour 20, `C_norm(60,300)` at `HOT_HOURS`
  (`raw_diff`).
- GARCH (3): `robust_scale_300` at hours 19 and 20,
  `robust_scale_60` at hour 20. (Rev-4's pooled minute-range p99.9
  is removed: that alternative was cut from rung 5 and must not
  linger here where a refusal could fail GARCH closed. The
  magnitude-shuffle closure and the forensic escalation remain its
  actual selectors.)
- Boundary (8): the four boundary metrics (outside predicates) PLUS
  their four matched comparator metrics (inside predicates); the
  critical value maximizes over all eight; the rung pairs each
  boundary metric with ITS OWN comparator - the outside and inside
  halves must hold for the same metric at the same boundary case:

  ```text
  pre_halt_close:   since_open [1800, inf) AND until_close (0, 300]
  post_halt_reopen: since_open [0, 300)    AND until_close (300, 1800]
  metrics: quote_mid_range_p99 and robust_scale_60, each boundary
           cell and its matched comparator cell (6.2 rung 6)
  ```

## 7. Frozen constants

```text
FAIL_HOURS_300            = 19, 20, 23
FAIL_HOURS_60             = 20
HOT_HOURS                 = 19, 20
COLD_HOURS                = 23
RESIDUAL_WINDOW_S         = 300
RESIDUAL_MIN_HISTORY      = 1000
RESIDUAL_EXCEED_MULTIPLES = 4, 8, 16     (trimmed-scale units, strict >)
INNOVATION_EXCEED_ABS     = 4, 8, 16     (unit-variance units, strict >)
PERMUTATION_REPLICATES    = 16
PERMUTATION_VARIANTS      = sign, magnitude
BOOTSTRAP_REPLICATES      = 10000
BOOTSTRAP_BLOCK_SESSIONS  = 5
BOOTSTRAP_BASE_SEED       = 1342176408401967774
PERMUTATION_BASE_SEED     = 7205759943768246531
CONTROL_TIE_BASE_SEED     = 3141592653589793238
                            (the artifact records all three as JSON
                             integers)
FAMILY_ENVELOPE_LEVEL     = 0.95 simultaneous, nearest-rank
SEED_DIRECTION_MIN        = 7 of 8
FOLD_MIN_SESSIONS         = 15
MATERIALITY_BAND          = 0.8, 1.25
GAP_CLOSE_MIN             = 0.50
GAP_CLOSE_LCB_MIN         = 0.25
GAP_CLOSE_EPS             = 1e-9
COUNT_WINDOWS_S           = 1, 5, 60
WALL_HORIZONS_S           = 1, 5, 15, 60, 300
EXCEEDANCE_TICKS          = 399, 642, 968 (strict >)
PARENT_COUNT_BINS         = 0 | 1-64 | 65-256 | 257-1024 | 1025-4096 | 4097+
SEGMENT_OPEN_BINS_S       = 0-300 | 300-1800 | 1800+
SEGMENT_CLOSE_BINS_S      = 1800+ | 300-1800 | 0-300
MIN_1S_CELL_RETURNS       = 2500
MIN_5S_CELL_RETURNS       = 500
MIN_15S_CELL_RETURNS      = 160
MIN_60S_CELL_RETURNS      = 40
MIN_300S_CELL_RETURNS     = 6
MIN_RESIDUAL_CELL         = 1000
MIN_MINUTES_CELL          = 30
MIN_BOUNDARY_MINUTES_CELL = 4
MIN_BOUNDARY_60S_CELL_RETURNS = 3
SIGMA_ESCALATION_MIN      = 2.0
CONTROL_ESCALATION_MAX    = 1.25
INITIATION_INNOVATION_MIN = 8
```

If implementation proves a frozen constant or statistic unmeasurable,
that brick FAILS and stops. A reviewed amendment restarts Brick F
before implementation resumes. No artifact may be produced under a
partially amended contract.

## 8. Standing instrument-resolution decision (binds 12b)

Confirmed by both reviewers, standing:

- MNQ-only evidence may not change shared Student-t or GARCH defaults.
- Any eligible shared-shape parameter becomes instrument-resolved in
  12b: the legacy/default branch preserves arithmetic, draw order and
  crypto tapes byte-for-byte with no re-bless; MNQ receives the fitted
  override; MES inheritance requires explicit provenance.
- A structural mechanism that cannot preserve the legacy branch
  exactly is ineligible absent separately scoped crypto evidence.
- The eventual MNQ generator change bumps `TAPE_PROTOCOL_VERSION` to
  13. (AMENDED 2026-08-09 from 12, reviewed and co-signed by codex
  session 019fe781-e6dd-7172-b700-22df68b83271 under this document's own
  stopping rule, formally restarting Brick F for the amendment: the
  12b arrival-frame calibration repair changes outputs for
  already-valid integrated `(config, seed)` configurations and
  therefore consumes identity 12 as a process-wide repair; the
  eventual MNQ mechanism landing consumes identity 13. The ladder,
  measurements, gates, artifacts and verdict of this document are
  explicitly NOT amended.)

## 9. Bricks

### Brick F: freeze

This document argued to consensus and frozen; the reviewing session's
sign-off recorded in this file.

### Brick O: observed-side measurement

Extend `analysis/mnq_fit.py` with a `measure12a` mode (no new
script). Computes Blocks 1-4 observed side and both permutation
variants; emits the observed half of the artifact under the section
10 schema. Memory contract: the stream stays chronological; at most
ONE session's parent endpoints and returns are retained, in packed
numeric arrays; permutation records are computed when the session
closes; per-session sufficient records are emitted and the arrays
released before the next session. Corpus-wide parent rows are never
retained. The section 7 constants join `SUBCONTRACT_KEYS` (preflight
rebinds).

Selftest additions (exact final count pinned at O's landing):
standardizer warmup and refusal; permutation invariants (sign shuffle
preserves magnitudes and zero locations; magnitude shuffle preserves
the sign sequence; identity permutation reproduces the original
statistics); reconstruction determinism byte-for-byte; bootstrap
determinism; nearest-rank and weighted-nearest-rank conventions; one
handcrafted fixture per block; every refusal branch; the ladder
verdict cases (each family alone, multiple with precedence, none,
all-null, zero-denominator); exact artifact key sets.

Gate, exact commands:

```text
python3 analysis/mnq_fit.py selftest
brokkr check
```

### Brick G: generated-side measurement

Adds a consumer-only `GenType::Measure12a` to
`crates/mogwai-server/src/gen.rs`: a `Measure12aAcc` that enables the
existing `VolTrace`, consumes each generated parent and its children
once, computes Blocks 1-4 streaming, retains one compact forensic
accumulator per populated minute plus the predecessor state
`sigma_start` needs and one deferred successor record for
`arch_share_next` (which requires LOOKAHEAD to the next parent, so
the record completes when that parent arrives), applies the frozen
extreme/control selection after the walk, and emits Blocks 1-5 and
the count-substitution inputs as JSON. It neither adds nor changes
any field, branch, callback, buffer or draw in `GeneratedSource`
(crate `mogwai-data` untouched). One fresh FINAL walk per seed 1-8
into the `measure12a` cache (2.4).

Cost probe, run FIRST via a dedicated harness mode:

```text
python3 analysis/mnq_fit.py cost12a
```

which runs `--type summary` then `--type measure12a` sequentially,
same release binary, seed 1, same anchor, warmup and 7-day window,
and enforces `measure12a runtime <= 1.5 x summary runtime` and
`peak RSS <= 1 GiB` (RSS sampled from the live process tree at 1 s
intervals, summed per sample, maximum over samples). Failure stops
the brick for accumulator redesign - the final budget is never
relaxed instead.

Gate, exact commands:

```text
brokkr test -p mogwai-cli measure12a_matches_independent_recompute
brokkr test -p mogwai-cli measure12a_selection_is_deterministic
brokkr test -p mogwai-cli measure12a_consumer_leaves_tape_byte_identical
brokkr test -p mogwai-data trace_consumes_no_draws_and_leaves_the_tape_byte_identical
brokkr check
```

(`measure12a_consumer_leaves_tape_byte_identical` compares the bar
tape emitted by a `--type bars` run against one where the
`Measure12aAcc` consumed the same walk - the consumer must not
perturb draws; `summary` emits no tape bytes so it cannot anchor this
test.)

### Brick M: the measurement run and verdicts

Runs from a clean committed O+G tree:

```text
python3 analysis/mnq_fit.py preflight
python3 analysis/mnq_fit.py measure12a
```

The ladder evaluates; `analysis/mnq-measure-12a.json` commits with
the binding block. Cost contract, recorded in the artifact and gated:

```text
observed pass + permutations: <= 2 h
eight generated walks:        <= 10 h
bootstrap + verdicts:         <= 2 h
total wall time:              <= 12 h
peak aggregate RSS:           <= 4 GiB   (1 s process-tree sampling)
scratch footprint:            <= 20 GiB  (max on-disk cache before cleanup)
```

Generated-walk parallelism is capped so aggregate RSS respects the
limit. A budget breach is a failed brick (stop and redesign), not a
relaxed budget.

Output: `family-eligible: <list>` or `no-family-eligible`; either way
the verdict goes to the owner before 12b is drafted.

### Ordering and keep/revert

F -> O -> G -> M; the suite green at each boundary; O and G are pure
additions revertible by removing the mode; M is a run plus one
committed artifact. Nothing changes generated bytes; no re-bless
anywhere.

## 10. Artifact schema (exact)

Serialization rules: all maps keyed by integers (hours, seeds, N,
ticks) serialize the key as its decimal string; arrays of sessions
sort ascending by date, seeds ascending 1-8, hours ascending 0-23,
horizons and window lengths ascending. Refusal ownership model,
frozen: every null CAUSED BY REFUSAL has exactly one matching record
in top-level `diagnostics.refused_cells`; the per-session and
forensic `refusals` arrays are scoped MIRRORS of the corresponding
top-level records (no scope of its own for observed monthly,
generated blocks or generated central). Nulls caused by defined
emptiness (the parenthesized rules on the shapes below) are NOT
refusals and appear only in `diagnostics.empty_bins` when a whole
bin is empty. Exact-key selftests assert this whole tree: every
listed key present, no unlisted key, and the refusal-null pairing in
both directions - with the Amendment-F standardizer-omission records
as the sole class of RefusalRec owning omitted observations rather
than refusal-caused nulls.

Shared shapes (every field listed is required; `| null` marks the
only nullable fields):

```text
RefusalRec   = {scope, cell, reason}          (three strings)

Block1Hist   = [{n, quote_range_half_ticks | null, trade_range_ticks,
                 hour, since_open_bin, until_close_bin, count}]

Block1BinSummary = {minute_count,
                    quote_range_denominator,
                    quote_range_p50 | null, quote_range_p90 | null,
                    quote_range_p99 | null, quote_range_p999 | null,
                    trade_range_p50 | null, trade_range_p90 | null,
                    trade_range_p99 | null, trade_range_p999 | null,
                    trade_range_sqrt_n_p50 | null,
                    trade_range_sqrt_n_p90 | null,
                    trade_range_sqrt_n_p99 | null}
                   (every quantile null when minute_count = 0; the
                    sqrt-n fields additionally null for the zero bin)

Block1Summary = {minute_count, quote_range_denominator,
                 n_p50, n_p90, n_p99, n_p999,
                 quote_range_p50 | null, quote_range_p90 | null,
                 quote_range_p99 | null, quote_range_p999 | null,
                 trade_range_p50, trade_range_p90, trade_range_p99,
                 trade_range_p999,
                 trade_range_sqrt_n_p50, trade_range_sqrt_n_p90,
                 trade_range_sqrt_n_p99,
                 exceed_399, exceed_642, exceed_968, denominator,
                 trade_to_quote_p99_ratio | null,
                 by_parent_count_bin: {bin_name: Block1BinSummary}}

Block2Cell   = {scheduled_windows, zero_windows,
                count_hist: {count: windows},
                run_length_hist: {length: runs},
                paired_lag_count, sum_x, sum_y, sumsq_x, sumsq_y,
                sum_xy, zero_fraction, mean, fano | null,
                count_p90, count_p99, count_p999,
                lag1_autocorr | null, run_p90 | null}
               (fano null when mean = 0; lag1_autocorr null when
                the paired variance is zero; run_p90 null when
                run_length_hist is empty)

Block3Cell   = {return_count, robust_scale | null, rms_scale | null}

Block3Pair   = {window_count, vr | null, cov_contrib | null,
                cov_contrib_norm | null}

ObservedBlock3 = {cells: {hour: {horizon_s: Block3Cell}},
                  pairs: {hour: {pair: Block3Pair}},
                  lag1_parent_autocorr: {hour: value | null},
                  hour20_labels: {label_pair:
                                  {horizon_s: Block3Cell}}}

Block4Map    = {hour: {residual_count, warmup_excluded,
                       zero_fraction | null,
                       nz_abs_p90 | null, nz_abs_p99 | null,
                       nz_abs_p999 | null,
                       ratio_p99_p90 | null, ratio_p999_p99 | null,
                       exceed_4 | null, exceed_8 | null,
                       exceed_16 | null}}
               (zero_fraction and every tail field null when
                residual_count = 0. Amendment B: the hour keys are
                the integer decimal strings PLUS the literal "all"
                pooled-hours cell of the same shape - the innovation
                family's fourth metric input. warmup_exclusions in
                diagnostics stays integer-hour keyed.)

Block1Blocks = {hist: Block1Hist,
                summary: {hour: Block1Summary},
                by_labels: {label_pair: {hour: Block1Summary}}}

Block2Map    = {hour: {window_s: Block2Cell}}

SeedBlocks   = {block1: Block1Blocks, block2: Block2Map,
                block3: ObservedBlock3, block4: Block4Map}
               (identical shape for observed monthly and per seed)

CentralBlocks = same keys as SeedBlocks minus block1.hist
                (scalars are 8-seed medians; hist never centralizes)

CondAdequacyRec = {hour, bin_name, observed_p99 | null,
                   generated_p99 | null, ratio | null,
                   interval_low | null, interval_high | null,
                   interval_inside_band | null,
                   seed_inside_count | null, required, supported}
                  (required = observed pooled count at floor;
                   supported = every seed at floor; the nullable
                   fields null when not required or not supported)

CountSubstitutionRec = {shares_observed: {hour: {bin_name: share}},
                        shares_generated: {hour: {bin_name: share}},
                        weights: {hour: {bin_name: weight | null}},
                        refused_hours: [hour],
                        support_refusals: [RefusalRec],
                        counterfactual_p999 | null,
                        counterfactual_exceed_968 | null,
                        closure_p999 | null, closure_lcb | null,
                        conditional_adequacy: [CondAdequacyRec],
                        diagnostic_closure_to_bound | null}

PermRecord   = {segment_index, hour, variant, replicate,
                return_count_60, sum_abs_60, max_abs_60,
                return_count_300, sum_abs_300, max_abs_300}
               (Amendment A: sufficient statistics, robust scales
                derived downstream. return_count counts EMITTED
                returns, zeros included; all-zero windows give zero
                sums; no emitted windows give all-zero fields. The
                session-hour statistic pools the session's segments:
                count = sum, sum_abs = sum, max_abs = max, robust =
                (sum_abs - max_abs)/(count - 1), refused when the
                COMBINED count is below the horizon floor. Cells
                with zero adjacent returns still emit records.)

ForensicRec  = {seed, kind: extreme_range | extreme_sqrt | control,
                matched_extreme_minute_start | null,
                minute_start_ns, minute_end_ns, utc_hour,
                segment_index, parent_count, trade_count,
                traced_parents,
                largest_innovation_std, largest_innovation_ts_ns,
                innovation_exceed_4, innovation_exceed_8,
                innovation_exceed_16,
                initiation, sigma_start | null, sigma_peak,
                sigma_end, sigma_escalation | null,
                latent_mid_range_ticks,
                quote_mid_range_half_ticks | null,
                trade_range_ticks,
                trade_to_quote_range_ratio | null,
                quote_to_latent_range_ratio | null,
                max_signed_run, clamp_hits,
                arch_share_next | null, arch_share_minute_max | null}

MetricRec    = {name, kind: log_ratio | raw_diff,
                predicate: outside | inside | raw_direction,
                point | null, se | null,
                interval_low | null, interval_high | null,
                band_low | null, band_high | null,
                outside_band | null, envelope_excludes_edge | null,
                interval_inside_band | null,
                seed_same_side_count | null,
                seed_inside_count | null,
                seed_rule_pass | null, fold_rule_pass | null,
                refused}
               (predicate-specific evidence: outside metrics carry
                outside_band, envelope_excludes_edge and the
                outside-side count in seed_same_side_count;
                raw_direction metrics carry outside_band and
                envelope_excludes_edge with the interval-excludes-
                zero meaning, band_low/band_high null, and the
                strict claimed-sign count in seed_same_side_count;
                inside metrics carry interval_inside_band and
                seed_inside_count; the other count field is null;
                every field irrelevant to the predicate is null;
                seed_rule_pass and fold_rule_pass apply the split
                rules of 6.1; a refused metric has every nullable
                field null and refused = true. The child-walk and
                boundary clean metrics use this same record
                contract, not rung-level booleans alone.)

RungRec      = {name, subchecks: {key: bool}, fired,
                boundary_localized | null,
                refusals: [RefusalRec],
                uniform_eligible | null, required_resolution | null}
               (boundary_localized is Boolean for a fired
                child_walk, reversion or garch rung when every
                localization input qualifies and the ratio is
                defined; null with exactly one matching localization
                RefusalRec when such a fired rung cannot measure
                localization; null WITHOUT a localization refusal
                for every unfired rung and for arrival, innovation
                and boundary (Amendment C). uniform_eligible and
                required_resolution are both null unless the
                reversion rung fired; a fired reversion rung with a
                MEASURED worsening_23 records either uniform_eligible
                = true with required_resolution = "uniform" or
                uniform_eligible = false with required_resolution =
                "hour-resolved"; a fired reversion rung with a
                REFUSED worsening_23 records both null with exactly
                one matching worsening_23 RefusalRec, per Amendment
                E - never a fabricated hour-resolved)
```

The six rung subcheck key sets, literal and exhaustive:

```text
child_walk: {a_print_excess, b_mid_clean}
arrival:    {a_envelope, b_closure, c_conditional}
innovation: {a_tail_ratio, b_initiation, c_controls}
reversion:  {a_closure, b_folds, c_covariance}
garch:      {a_closure, b_escalation}
boundary:   {a_boundary_band, b_comparator_clean, c_no_prior_rung}
```

Top level:

```text
mnq-measure-12a.json
  binding: {harness_tree_commit, job_id, subcontract_hash,
            preflight_artifact_hash, file_hashes,
            tape_protocol_version = 11,
            generated: {seeds = [1..8], window_start_ns,
                        window_length_ns, warmup}}
  constants: every section 7 name, verbatim values
  observed:
    per_session: [{session_date,
                   segments: [{segment_index, open_ns, close_ns}],
                   block1_hist: Block1Hist,
                   block2: Block2Map,
                   block3: ObservedBlock3,
                   block4: Block4Map,
                   permutations: [PermRecord],
                   refusals: [RefusalRec]}]
    monthly: SeedBlocks
    permutations_monthly: {variant: {hour:
      {robust_scale_60 | null, robust_scale_300 | null}}}
  generated:
    per_seed: [{seed, blocks: SeedBlocks,
                count_substitution: CountSubstitutionRec,
                forensic: {records: [ForensicRec],
                           refusals: [RefusalRec]},
                cost: {walk_s, rss_bytes}}]
    central: {blocks: CentralBlocks,
              count_substitution: {closure_p999_median | null,
                                   refused_hour_union: [hour]},
              pooled_diagnostic_hist: Block1Hist | null}
  bootstrap: {seed_rule, replicates,
              per_family: {family: {metrics: [MetricRec],
                                    critical_value | null,
                                    inventory_complete}}}
             (Amendment D: inventory_complete is true iff every
              required metric produced all finite bootstrap values,
              a finite positive SE, and the complete-family critical
              value; critical_value numeric iff inventory_complete.
              A refused metric has refused = true, all nullable
              fields null, and exactly one matching RefusalRec; a
              computable metric in an incomplete family keeps
              refused = false with its point, SE, band, point-only
              predicate, seed and fold evidence, but null
              interval_low/interval_high/envelope_excludes_edge/
              interval_inside_band. Every envelope-dependent rung
              subcheck consuming an incomplete family is false;
              forensic subchecks keep their measured booleans; the
              rung cannot fire and mirrors the refusal records.
              Every required conditional metric remains PRESENT in
              metrics: unsupported generated support produces a
              refused MetricRec, never omission from the inventory.
              Whenever inventory_complete = false, exactly one
              family-envelope RefusalRec names the incomplete metric
              inventory or the failed complete-family critical-value
              construction; that record owns the envelope-only nulls
              on otherwise computable metrics, each individually
              refused metric additionally has its own metric
              RefusalRec, and the consuming rung mirrors both.)
  ladder: {rungs: [RungRec], eligible: [names], selected: name | null,
           verdict: family-eligible | no-family-eligible}
  cost: {observed_s, generated_s, bootstrap_s, total_s,
         peak_rss_bytes, scratch_bytes}
  diagnostics: {warmup_exclusions: {hour: count},
                refused_cells: [RefusalRec],
                empty_bins: [{scope, cell}],
                worsening_23: {point, se, ucb} | null}
```

## 11. RESULT (Brick M, 2026-08-06)

The measurement ran from the clean committed tree 1e9506c and the
artifact `analysis/mnq-measure-12a.json` landed under both validation
gates. VERDICT: `no-family-eligible` - no rung fired. Cost: observed
333.6 s, generated replays 324.3 s, bootstrap and assembly 10.5 s,
total 668.4 s; peak tree RSS 734 MiB; scratch 83 MB - every budget
held. Five of six family inventories are COMPLETE; the verdict is
measured, not a refusal cascade, with one deliberate exception:

- ARRIVAL failed CLOSED, and it carries the loudest evidence. The
  generated arrival process is massively under-dispersed against the
  observed tape: Fano-factor log ratios at the fail hours are -1.22 to
  -3.07 (generated 3.4x to 21x less dispersed) and count-p99 log
  ratios -0.43 to -1.33, all with 7-of-8 seed agreement and every fold
  retaining the side. But five required conditional bins have observed
  support the generated months never populate at floor (Amendment D
  fails the rung closed), and the count substitution refuses 22 of 24
  hours on observed-support-without-generated-support, so the closure
  is unmeasurable. The generated parent-count COMPOSITION is too far
  from the observed one for the counterfactual to have support at all.
- INNOVATION subcheck a FIRED: the pooled nonzero abs-z p99.9/p99
  ratio is outside the band with envelope, seeds and folds (generated
  tail 31 percent heavy). But initiation held in only 4 of 8 seeds
  (7 required) and the escalation contrast with controls was not
  clean, so b and c failed on measured forensic evidence.
- REVERSION and GARCH: the 300 s wall-time discrepancy is present in
  point estimates (generated hot 27 to 36 percent at hours 19 and 20)
  but the 95 percent simultaneous envelopes (critical values 2.65 to
  3.09) do not exclude the band edge over 22 sessions, and neither
  shuffle closure cleared the 0.50 floor everywhere; the hour-19
  covariance direction came out opposite-signed.
- CHILD_WALK and BOUNDARY: clean in point; nothing to fire.

The verdict went to the owner (spec 1.1) and the owner RULED,
2026-08-06: protocol 12b targets the ARRIVAL COMPOSITION, framed as
repair-until-measurable. Because no rung fired, this is a recorded
owner override of the ladder's silence, not a ladder output, grounded
in two facts: the point evidence (generated Fano 3.4x to 21x low at
every fail hour, 7 of 8 seeds, every fold) and the failure mode itself
(the counterfactual refused 22 of 24 hours for missing generated
support - the rung could not measure arrival at all, which is
composition evidence in its own right). The 12b success criterion is
therefore NOT assumed eligibility: 12b repairs the generated
parent-count composition until the frozen 12a arrival counterfactual
HAS support, then the unamended 12a ladder re-runs and eligibility is
measured. Constraints binding that spec: the section-8
instrument-resolution decision (the arrival chain is shared shape, so
MNQ receives an instrument-resolved override with the legacy branch
byte-preserved, no re-bless), the Brick V wall-time hard gates of
section 1.2, and `TAPE_PROTOCOL_VERSION` 13 (AMENDED 2026-08-09 from
12, same coordinated amendment as section 8: identity 12 is consumed
by the arrival-frame calibration repair). Drafting waits on codex
review capacity (exhausted for the week of 2026-08-06); the freeze
protocol applies in full.

## 12. Stopping rule

Out of scope: any generator change (`mogwai-data` is untouched), any
preset change, any `TAPE_PROTOCOL_VERSION` bump, the 12b mechanism
spec, the two-sided p99 band implementation (12b), the ES/MES corpus,
the reopen-gap limitation, and the fanout investigation. 12a lands
measurements and verdicts only.
