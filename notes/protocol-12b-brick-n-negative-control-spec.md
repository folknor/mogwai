# Protocol 12b brick N: the negative control, implementation spec

DRAFT 2, 2026-08-09 (draft 1 revised under two independent reviews;
every folded finding is marked REVIEW and section 12 records what was
rejected). Written against
`reference/technical-implementation-spec.md`. Spawned from
`notes/protocol-12b-arrival-composition-spec.md` section 0, item 1,
whose binding sections are 5.5 (the mechanism, the fit/test seed split,
the predicted failure), brick N (schema and gate commands), 10.2 gates
B1 to B7 with B8 inapplicable, 7 (seed sets), 8 (exposure), 13
(artifact binding blocks) and 16 (constants).

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it.

The 12b document remains the contract of record. Every constant, seed
set, gate definition and gate command below is LIFTED VERBATIM from
the sections item 1 names. Where this spec adds anything, it adds
mechanism - types, call sites, procedures - never a threshold, never a
verdict, never a relaxation. Four places where building exposed a fact
the 12b document does not state are marked OBSTACLE and resolved here
without amending anything frozen; none changes a gate threshold or a
gate wording.

---

## 1. The goal, and where it stops

Build `mogwai arrival-control` and produce the committed artifact
`analysis/mnq-arrival-control.json`: the deterministic hourly
re-centring of 12b section 5.5, fitted on `CONTROL_FIT_SEEDS`, judged
out of sample on `CONTROL_TEST_SEEDS` against hard gates B1 to B7.

Stop points, both owner decisions, lifted from item 1:

- A PASS - all of B1 to B7 pass - ends the whole 12b landing with
  `negative-control-passed` and stops for an owner ruling. Items 2 and
  3 are then never specced. The implementer halts; no Stage A work
  follows from this spec under any outcome.
- A FAILURE is recorded in the artifact with the failing gate names,
  verdict `negative-control-failed`, and the loop proceeds to item 2.
  The implementer still halts here: item 2 is a separate spec.

B8 is ABSENT by inapplicability, not recorded as passed or refused:
the control has no cadence grid to be sensitive to (5.5).

No tape byte moves. `TAPE_PROTOCOL_VERSION` stays 11 (12b section 1.2
obligation 4 puts the bump at brick S, and at no earlier commit). No
preset changes. `crates/mogwai-data`, `crates/mogwai-protocol`,
`crates/mogwai-engine`, `crates/mogwai-adapter`,
`analysis/fingerprint.json` and every file under
`crates/mogwai-server/presets/` are untouched by this brick.

## 2. Survey of the ground

### 2.1 What already exists and is reused unchanged

The control needs no new measurement machinery. Every statistic it
judges is already computed by landed code, and this brick's job is
wiring, not estimation.

- `mogwai_lab::aggregate::context::ObsContext` is constructed from a
  vector of per-session records (`ObsContext::new(per_session)`) and
  memoizes every block accessor the gates need: `minute_counts`,
  `b1_bin_count`, `b2_scheduled`, `b2_fano`, `b2_count_quantile`,
  `b3_votes`, `b3_robust_strict`.
- `mogwai_lab::aggregate::countsub::{count_substitution,
  obs_shares_under, support_refusals_of}` is the FROZEN 12a count
  substitution, unamended. `mogwai_lab::aggregate::family::
  conditional_adequacy_bins` is the 5.2 conditional adequacy guard.
- `mogwai_lab::aggregate::monthly::pool_session_hists` builds the
  `PooledHist` the substitution consumes from a walk's per-session
  records.
- `mogwai_lab::measure12a::generated::GeneratedAcc` is the generated
  front-end of the unified block engine. Its `finish()` returns
  `{seed, per_session, forensic}` whose `per_session` records carry
  `block1_hist`, `block2`, `block3`, `block4`, `permutations`,
  `refusals`, `segments`, `session_date` - the exact shape
  `ObsContext::new` reads, and the same shape the observed side has.
- `mogwai_lab::summary::summarize` is the `gen --type summary`
  accumulator, producing `minute_range_ticks_hist` and
  `minute_range_max_ticks`, which is where gate B4's per-seed
  statistics come from.
- `mogwai_lab::fit::walk::{Overrides, OverrideValue,
  scratch_config_text, profile_from_config, run_summary_walk}` is the
  protocol-11 walk layer. It writes a scratch
  `[instrument.override]` TOML and resolves it through the server's
  own `Config::load`, and `"session.intensity_hour"` is an already
  supported override path (`crates/mogwai-server/src/config.rs` lists
  it among the conditional hour parameters, and
  `fit::driver` drives its arrival probe through exactly that key).
  This is the mechanism by which a re-centred curve reaches the
  generator WITHOUT editing a preset.
- `mogwai_cli::measure::run_final_walk` is the exposure contract of
  12b section 8 already in code: MNQ through the profile machinery, no
  divergence, vol trace on, walk start at
  `FINAL_START_NS - SUMMARY_WARMUP`, measured window
  `[FINAL_START_NS, FINAL_START_NS + FINAL_LENGTH)`. REVIEW: it is a
  reference, NOT a call site. It lives in `mogwai-cli`, which depends
  on `mogwai-lab` and never the reverse, so `control_walk` - which
  lives in the lab - cannot call it. See section 2.7.
- `crates/mogwai-cli/src/minute_range_envelope.rs` is the template for
  an artifact-writing subcommand: clean-tree attestation, hash-bound
  inputs, atomic write.

### 2.2 The observed side is committed; no corpus is read

`analysis/mnq-measure-12a.json` carries `observed.per_session` - all
22 usable sessions, each a full block record - alongside
`observed.monthly`. So the observed `ObsContext` is constructed
directly from the committed artifact, and this brick reads NO TBBO
corpus, needs no delivered data on disk, and runs on any clone. That
is a stronger property than 12b section 9 claims for Stage A, and it
holds here for the same reason: the substitution, the wall-time
contour, the mean rate and the zero-second fraction are all functions
of the block records, not of the tape.

The one input that is neither the 12a artifact nor a generated walk is
`analysis/mnq-minute-range-envelope.json`, brick B4's committed bound
(commit `8e75450`). Gate B4 reads it and does not recompute it, per
brick B4 amendment 2.

### 2.3 What `fit::driver` does with the same curve, and why it is not reused

`fit::driver` already solves for `intensity_hour` and already runs
arrival probes under an `"session.intensity_hour"` override. It is NOT
reused, and the reason is not taste: its objective is the protocol-11
`session_arrival_hour` relative tolerance over its own observed pass,
its walks are cached under the protocol-11 cache keys, and its verdict
machinery is the protocol-11 family/target structure. The control's
correction is closed form with no objective and no search (5.5), its
seeds are disjoint from every fit seed, and its gates are the 12b
Stage B set. Bolting it onto the fit driver would import a solver, a
cache and a verdict vocabulary the control must not have. What IS
reused is `fit::walk`, which is the walk layer and carries none of
that.

### 2.4 OBSTACLE 1: the re-centring is not exactly self-consistent

Derived here because the 12b document states the rescale's neutrality
(5.5) but not this, and an implementer who does not know it will read
a uniform B6 offset as a bug.

`SessionModulator::new` stores `arr_hour[h] = intensity_hour[h] * 24`
and `arrival_mult(t) = arr_hour[h] * arr_dow[d] / arrival_normalizer`,
where `arrival_normalizer` is the exposure-weighted mean of
`arr_hour * arr_dow` over the calendar's open minutes of one week.
Write `g[h]` for the generated mean parents per scheduled minute at
hour `h` under the shipped curve, `o[h]` for the observed one, and
`ratio[h] = g[h] / o[h]`. The correction sets
`arr'[h] = arr[h] / ratio[h]`, so

```text
new rate at h  ~  g[h] * (arr'[h] / N') / (arr[h] / N)
               =  g[h] * (1 / ratio[h]) * (N / N')
               =  o[h] * K,        K = N / N'
```

with `N` and `N'` the old and new normalizers. `K` is a SINGLE factor
common to every hour, so the correction reproduces the observed hourly
shape exactly and the observed hourly LEVEL up to `K`. `K` is exactly
computable before any walk runs, from the calendar's open-minute
exposure weights, and it is 1 only when `ratio` is constant.

REVIEW, and this is a correction to draft 1 rather than a gloss. The
shipped MNQ `intensity_hour` does NOT sum to 1: it sums to 23.862306,
because it is a MEAN-1 curve, and neither `config.rs` nor `session.rs`
enforces sum-to-one anywhere. 5.5's "rescaled to sum to 1" therefore
changes the curve's SCALE by roughly 23.86 while changing no generated
rate (the `arrival_normalizer` divides the scale straight back out,
which is exactly 5.5's neutrality claim). A `K` computed naively as
`N / N'` between a sum-23.86 old curve and a sum-1 new curve is that
scale factor, about 23.86, and carries no information at all.

`normalizer_drift` is therefore defined SCALE-INVARIANTLY: both curves
are divided by their own `py_sum` before the exposure-weighted
normalizer is formed, so `K` is the ratio of the two normalized
normalizers and is 1 exactly when `ratio[h]` is constant. That is the
number section 2.4 has always been about; draft 1 simply specified an
arithmetic that did not compute it. The pinned test
`normalizer_drift_is_one_for_a_constant_ratio` binds this definition.

Two consequences worth stating rather than leaving implicit. First,
the sum-to-1 rescale is cosmetic for the generator but takes the
scratch override's curve OUT of the shipped preset's mean-1
convention; the artifact records `new_curve` as written, and a reader
comparing it column-wise against `mnq.toml` must divide by 24 (or
multiply the preset curve by `1/py_sum`) first. Second, the
divergence between 5.5's rescale convention and the shipped preset
convention is a real inconsistency in the FROZEN document. It is
recorded here and flagged upward; it is NOT amended here, because it
moves no generated rate and section 17 makes any amendment a Brick-F
restart.

Consequence, recorded rather than corrected: gate B6
(`MEAN_RATE_BAND = [0.98, 1.02]`, per hour AND per seed) is judged
against a mechanism carrying a uniform multiplicative offset `K` plus
the seed noise in the four-seed `g[h]` estimate. Protocol 11 fitted
the hourly marginal to a worst-hour error of 0.63 percent
(`crates/mogwai-server/presets/mnq.toml`), so `ratio[h]` is within
roughly one percent of 1 and `K` is expected well inside the band -
but this is a prediction, not a guarantee, and a B6 failure whose
per-hour ratios are all equal to within estimation noise is THIS
effect and must be reported as such rather than as a mechanism defect.

The correction is NOT amended to divide `K` out. Section 5.5 is
frozen: `new_curve[h] = old_curve[h] / ratio[h]`, rescaled to sum to
1, one iteration, no search. Dividing by `K` would be a second
iteration, and 12b section 17 makes an amendment a Brick-F restart.
The artifact records `K` as `normalizer_drift` so the reader can see
the offset instead of inferring it.

### 2.5 OBSTACLE 2: two accumulators, one tape, two walks

`GeneratedAcc` (gates B2, B3, B6, B7) and `summary::summarize` (gate
B4) are both fed by driving a `GeneratedSource` to exhaustion over the
measured window, and `summarize` takes `&mut dyn TickSource` and owns
its own loop. Teeing one tick stream into both would require a new
source adapter in `mogwai-data`, which this brick may not touch.

Resolved by running the walk TWICE per test seed, once per
accumulator. This is sound rather than merely convenient: the
generator is deterministic in `(profile, seed, start)`, and all three
are identical between the two passes, so the second walk replays the
same tape byte for byte. The tick-composition and parity gates already
rest on exactly that determinism. The cost is stated in section 7.

### 2.6 The clean-tree helper is about to have four copies

REVIEW: draft 1 said three and named two. There are already THREE
private, identical copies of `require_clean_tree` - in
`crates/mogwai-cli/src/measure.rs`, `crates/mogwai-cli/src/fit.rs` and
`crates/mogwai-cli/src/minute_range_envelope.rs` - and this brick
would add a fourth. Rather than paste it, it moves to
`mogwai_lab::ledger` and ALL THREE existing call sites are rewritten
to call it, `fit.rs` included; migrating two and leaving the third
would leave the exact drift hazard the move exists to remove.

Two details draft 1 got wrong by omission:

- The existing copies return `anyhow::Result<String>`; a lab home
  returns `LabResult<String>`. So N1 is NOT a pure refactor - the
  error TYPE changes at three call sites, and with it the message
  text unless the conversion preserves it. The shared function is
  therefore specified as `pub fn require_clean_tree() -> LabResult<String>`
  with the two existing message strings preserved BYTE FOR BYTE
  ("git status failed; the harness tree is unidentifiable" and "the
  working tree is dirty; an artifact may only bind a commit that is
  exactly the code that ran - commit first"), and each CLI call site
  maps the error with `map_err(|e| anyhow!("{e}"))` so its rendered
  text is unchanged. `minute_range_envelope_refuses_a_dirty_tree_before_reading_inputs`
  asserts on that text and must stay green untouched, which is what
  makes it the N1 gate.
- N1 owes a fit-side regression gate too, since `fit.rs` is now a call
  site: whichever existing `mogwai-cli` fit test exercises the dirty
  refusal is re-run in the N1 gate list, and if none exists, N1 adds
  `fit_refuses_a_dirty_tree` rather than migrating `fit.rs` untested.

`measure.rs`'s `fresh_tree_state` (which tolerates a dirty tree and
reports the fact) stays where it is; it is a different contract - and
section 2.8 makes this brick a consumer of that contract as well.

### 2.7 OBSTACLE 3: the exposure contract must be reimplemented in the lab

REVIEW, and the largest mechanism gap in draft 1. `control_walk` is a
`mogwai-lab` function; `run_final_walk` is a `mogwai-cli` function;
`mogwai-cli` depends on `mogwai-lab` and never the reverse. The
`GeneratedAcc` half of `control_walk` therefore CANNOT call
`run_final_walk` and must be written afresh in the lab crate.
`fit::walk::run_summary_walk` covers only the `summarize` half.

This is the one place where two copies of the exposure contract exist,
so the spec pins the copy element for element rather than saying "the
same exposure". The lab-side `GeneratedAcc` pass MUST match
`mogwai_cli::measure::run_final_walk` in every one of:

- the profile resolved for MNQ through `InstrumentProfiles::defaults()`
  with `config::profile_from_preset("MNQ")` as the fallback, and then
  - for the corrected-curve walks only - the scratch
    `[instrument.override]` `session.intensity_hour` applied through
    `fit::walk::{scratch_config_text, profile_from_config}`, which is
    the ONLY difference between the two copies;
- `calendar.utc_offset_minutes` as the accumulator's hour offset,
  refusing if the preset carries no calendar;
- `scalars.modal_tick` as the tick size the accumulator quantizes on;
- `enable_vol_trace()` on the source before the loop (it consumes no
  draws, so it does not perturb the tape - verified, and this is what
  makes section 2.5's two-pass determinism claim hold);
- the walk starting at `window_start_ns - warmup` and the loop
  breaking on the first event with timestamp `>= window_start_ns +
  window_length_ns`, i.e. a half-open measured window;
- no divergence armed, regime neutral, nanosecond rounding unchanged.

`run_summary_walk` takes `length` and `warmup` as duration STRINGS
while `GeneratedBinding` carries `window_length_ns: u64`. The
conversion is part of this brick and is specified, not left to the
implementer: `window_length_ns` is rendered as `"<n>s"` with `n =
window_length_ns / 1_000_000_000`, refusing if the value is not a
whole number of seconds (it is: 2674800000000000 ns = 2674800 s), and
`warmup` is passed through as the string the 12a binding already
carries. `GeneratedBinding` gains
`pub fn length_arg(&self) -> LabResult<String>` for exactly this, and
N2 pins it with `the_binding_renders_its_window_length_as_whole_seconds`.

The two copies are held together by a test, not by discipline:
N2 adds `the_lab_walk_matches_the_measure_exposure_contract`, which
runs the lab-side `GeneratedAcc` pass with `curve: None` at the 12a
binding's own window against seed 1 and compares its `per_session`
record - `cost` removed - to `mogwai_cli::measure::run_final_walk(1)`.
Because `mogwai-lab` cannot depend on `mogwai-cli`, that test lives in
`crates/mogwai-cli/tests/` and is therefore an N3 test, listed there;
N2 lands the lab pass, N3 lands the equality pin. If the two ever
diverge, that test is what says so.

### 2.8 OBSTACLE 4: hour 21 is unexposed, and draft 1 failed B6 and B7 on it

REVIEW, blocking in draft 1. Every one of the 22 committed sessions in
`analysis/mnq-measure-12a.json` carries exactly 23 block2 hours: hour
21 UTC is absent from all of them, because it is the 16:00-17:00
Central daily break and the MNQ calendar never opens it. The preset's
`intensity_hour[21] = 1.0` is a stated convention, not a measurement.

Draft 1 then said two incompatible things. Section 3.1 declared
`HOURS = 0..24` and promised the gates "iterate the hours the block
records actually carry"; section 4 and gate B6 said an hour whose
observed reading is `None` is a REFUSAL and that any refusal fails the
gate. Under the second reading hour 21 refuses on every seed and B6
and B7 fail unconditionally, whatever the mechanism does. That would
have made the negative control incapable of the pass it exists to test
for.

The resolution, which changes no 12b threshold and no gate wording:

- The gate INDEX SET is the set of hours the MNQ calendar actually
  opens, derived from the calendar's own open minutes - not `0..24`,
  and not "whatever the generated side happened to populate". An hour
  with zero scheduled minutes in the calendar is OUTSIDE the index
  set: it is neither passed nor refused nor recorded as a gate cell,
  exactly as B8 is absent by inapplicability rather than recorded.
- Within that index set, draft 1's rule stands unchanged and unsoftened:
  a cell that cannot be evaluated is a recorded refusal and any
  refusal fails the gate. In particular a generated-side `None` at an
  hour the calendar DOES open is a refusal, not a skip - that is the
  omission 12b section 8 forbids, and the distinction is exposure, not
  convenience.
- Hour 20 is partially exposed (45 open minutes per session around the
  15:15-15:30 halt). Partial exposure is still exposure: hour 20 is IN
  the index set and is judged like any other hour, with its smaller
  denominator visible in the evidence.
- The artifact records the index set explicitly as
  `binding.gate_hours` together with `binding.unexposed_hours: [21]`,
  so a reader can see that 23 hours were judged and which one was not,
  rather than counting rows.

`HOURS` as a `0..24` constant is DELETED from section 3.1; it appeared
in no signature and its only effect was to license the wrong reading.
It is replaced by `pub fn gate_hours(profile: &InstrumentProfile) ->
LabResult<Vec<i64>>`, derived from the calendar, and N2 pins it with
`gate_hours_excludes_the_unexposed_hour` (MNQ yields 23 hours, hour 21
absent) and `an_unexposed_hour_is_not_a_refusal` (B6 and B7 over the
committed observed context and a synthetic matching generated context
pass, with no hour-21 cell in the evidence and no refusal).

### 2.9 The tree can move during the run

REVIEW. `require_clean_tree` runs before any input is read, and then
roughly six minutes of walking happen before the artifact is written.
A source edit or a HEAD move in that window would produce an artifact
binding a commit that is not the code that ran. `measure.rs` already
solves this: it reads `fresh_tree_state()` immediately before its
atomic write and bails with "the tree changed during the measure12a
run; the artifact is unbound" if HEAD moved or the tree went dirty.

This brick does the same, and the check happens BEFORE the B1 and B5
evidence and the verdict are serialized, so no partially bound record
reaches disk. `fresh_tree_state` moves to `mogwai_lab::ledger`
alongside `require_clean_tree` under brick N1, on the same
message-preserving terms, since it is now a second shared contract
rather than a `measure.rs` private. N3 pins it with
`arrival_control_refuses_a_tree_that_changed_during_the_run`, which
dirties the tree mid-run at a shortened window.

### 2.10 The median over four seeds needs a convention

REVIEW. 5.5 says "median over seeds" and `CONTROL_FIT_SEEDS` has four
entries. An even-count median is ambiguous - lower, upper, or the
average of the two middle readings - and draft 1 named no convention,
gave no function and pinned no test. Two implementers would then
produce two different `new_curve` values and two different committed
artifacts from the same inputs, which is exactly the reproducibility
this brick's whole binding block exists to guarantee.

The convention is the arithmetic median (average of the two middle
readings), fixed in `seed_median` in section 3.1 and pinned by the N2
test `the_seed_median_averages_the_two_middle_readings`. This is a
mechanism decision, not a threshold: 5.5 is silent, so the spec
chooses, records the choice, and the artifact carries all four
per-seed readings so the choice is auditable rather than asserted.

## 3. The target, as concrete artifacts

### 3.1 New file: `crates/mogwai-lab/src/arrival_control.rs`

Registered in `crates/mogwai-lab/src/lib.rs` as `pub mod
arrival_control;`. This module owns the correction, the walks and the
gates. It owns no I/O beyond reading the two committed artifacts it is
handed by path.

```rust
//! Protocol 12b brick N: the deterministic hourly re-centring negative
//! control. The spec is `notes/protocol-12b-arrival-composition-spec.md`
//! section 5.5 and brick N; gates B1 to B7 are 10.2, verbatim.

/// 12b section 7 and 16, verbatim. Pairwise disjoint from every other
/// 12b seed set, and in particular from CONFIRMATION_SEEDS = 1..8.
pub const CONTROL_FIT_SEEDS: [u64; 4] = [301, 302, 303, 304];
pub const CONTROL_TEST_SEEDS: [u64; 4] = [305, 306, 307, 308];

/// 12b section 16, verbatim.
pub const MEAN_RATE_BAND: (f64, f64) = (0.98, 1.02);
pub const ZERO_COUNT_BAND: (f64, f64) = (0.8, 1.25);
/// 12b section 1.2 obligation 2 and 10.2 B3. Inherited, not relaxable.
pub const WALLTIME_BAND: (f64, f64) = (0.8, 1.25);
pub const WALLTIME_HORIZONS_S: [i64; 2] = [60, 300];

/// The gate index set: the hours the instrument's calendar actually
/// opens, in ascending order. An hour with zero scheduled minutes is
/// OUTSIDE the set - not refused, not skipped, absent (section 2.8).
/// MNQ yields 23 hours; hour 21 UTC is the daily break.
pub fn gate_hours(profile: &InstrumentProfile) -> LabResult<Vec<i64>>;

/// One hour's rate reading on one context.
pub struct HourRate {
    pub hour: i64,
    /// Sum over block1 rows of `n * count`: every parent at this hour.
    pub parents: i64,
    /// Sum over sessions of the block2 `(hour, 60)` `scheduled_windows`.
    pub scheduled_minutes: i64,
    /// `parents / scheduled_minutes`, or `None` where the denominator
    /// is zero (an hour the calendar never opens).
    pub mean: Option<f64>,
}

/// Mean parents per SCHEDULED minute, per hour, over a whole context.
/// Scheduled, not populated: an empty minute is a zero, not an absence.
pub fn hourly_mean_parents(ctx: &ObsContext) -> BTreeMap<i64, HourRate>;

/// The 1 s zero-count fraction per hour: the block2 `(hour, 1)`
/// `zero_windows` summed over sessions, over the summed
/// `scheduled_windows`. `None` where no session serialized the cell.
pub fn hourly_zero_second_fraction(ctx: &ObsContext) -> BTreeMap<i64, Option<f64>>;

/// The median over the four fit seeds, per hour. 5.5 says "median over
/// seeds" and the seed count is EVEN, so the convention is pinned here
/// rather than left to the implementer (section 2.10): sort the finite
/// readings ascending and average the two middle ones - the ordinary
/// arithmetic median, not lower-median, not upper-median. With fewer
/// than four finite readings at an hour the result is `None`, which
/// leaves that hour's curve unchanged in `recentred_curve`. NaN and
/// infinite readings are dropped before the sort and their count is
/// recorded in the artifact's per-hour `ratios` entry.
pub fn seed_median(readings: &[Option<f64>; 4]) -> Option<f64>;

/// The section 5.5 correction, closed form, one pass.
/// `ratios[h] = generated median / observed`, absent for an hour with
/// no reading, which leaves that hour's curve value UNCHANGED - the
/// control corrects what it measured and invents nothing where it did
/// not.
/// The returned curve is rescaled to sum to exactly 1 by `py_sum`
/// division; that rescale is canonical serialization for the
/// `SessionProfile` schema and changes no generated rate (5.5).
pub fn recentred_curve(old: &[f64; 24], ratios: &BTreeMap<i64, f64>) -> [f64; 24];

/// `K` of section 2.4: the exposure-weighted normalizer ratio between
/// the shipped and the re-centred curve, over the MNQ calendar's open
/// minutes of one week, computed the way `SessionModulator::new`
/// computes its normalizer. Reported, never applied.
pub fn normalizer_drift(
    old: &[f64; 24],
    new: &[f64; 24],
    profile: &InstrumentProfile,
) -> LabResult<f64>;

/// One control walk's two passes over one tape.
pub struct ControlWalk {
    pub seed: u64,
    /// The 12a block records, for B2, B3, B6 and B7.
    pub ctx: ObsContext,
    /// `summary::summarize`'s record, for B4.
    pub summary: Value,
    /// Wall seconds of both passes together.
    pub cost_s: f64,
}

/// One seed, one curve, both passes. `curve` is `None` for the SHIPPED
/// curve (the ratio measurement) and `Some(new_curve)` for the
/// corrected one (the judgement). The exposure is 12b section 8: MNQ
/// resolved through `Config::load`, no divergence, regime neutral,
/// window and warmup READ FROM the 12a artifact's `binding.generated`
/// rather than restated, all 23 traded hours, nanosecond rounding
/// unchanged.
pub fn control_walk(
    scratch_dir: &Path,
    binding: &GeneratedBinding,
    curve: Option<&[f64; 24]>,
    seed: u64,
) -> LabResult<ControlWalk>;

/// The exposure contract, parsed from `binding.generated` of the 12a
/// artifact. Refuses rather than defaulting on any missing field.
pub struct GeneratedBinding {
    pub window_start_ns: u64,
    pub window_length_ns: u64,
    pub warmup: String,
}
impl GeneratedBinding {
    pub fn from_measure12a(artifact: &Value) -> LabResult<Self>;
    /// `window_length_ns` as the duration STRING `run_summary_walk`
    /// takes, `"<n>s"`, refusing a length that is not a whole number
    /// of seconds (section 2.7).
    pub fn length_arg(&self) -> LabResult<String>;
}

/// One gate's verdict, in the artifact's own shape.
pub struct GateRec {
    pub name: &'static str,
    pub passed: bool,
    pub evidence: Value,
    pub refusals: Vec<RefusalRec>,
}

/// The five statistical gates, each over the four test walks against
/// the committed observed context, each restricted to `hours` (section
/// 2.8). Signatures pinned so the driver cannot pass the wrong side of
/// a ratio. REVIEW: all five return `LabResult<GateRec>` uniformly.
/// Draft 1 had three of them return a bare `GateRec`, which drew a
/// line between "gate cannot be computed" and "gate did not pass" that
/// the callers do not honour and the artifact does not record; a gate
/// that cannot be computed at all is a section 8 stop, and `Err` is
/// how it reaches the operator. A refusal WITHIN a computable gate is
/// still `Ok(GateRec { passed: false, refusals })`.
pub fn gate_b2(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec>;
pub fn gate_b3(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec>;
pub fn gate_b4(envelope: &Value, tests: &[ControlWalk]) -> LabResult<GateRec>;
pub fn gate_b6(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec>;
pub fn gate_b7(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec>;
```

`gate_b1` and `gate_b5` take no context and live in the CLI driver:
B1 is a byte comparison over files on disk and B5 is a build gate.
Both are recorded in the artifact by the driver from evidence the
operator supplies on the command line (section 4).

### 3.2 New file: `crates/mogwai-cli/src/arrival_control.rs`

Registered in `crates/mogwai-cli/src/lib.rs` (`pub mod
arrival_control;`) and dispatched from `main.rs` as
`Command::ArrivalControl(arrival_control::ArrivalControlArgs)` with the
doc comment "Protocol 12b brick N: the deterministic hourly
re-centring negative control."

It lives in `lib.rs` rather than as a `main.rs`-private module for the
same reason `measure` and `minute_range_envelope` do: the brick's own
scratch-path regression test drives `run` directly.

```rust
#[derive(Args)]
pub struct ArrivalControlArgs {
    /// The committed protocol-12a artifact: the observed side, the
    /// exposure binding and the input hash.
    #[arg(long, value_name = "PATH")]     // default analysis/mnq-measure-12a.json
    measure: Option<PathBuf>,
    /// Brick B4's committed bound.
    #[arg(long, value_name = "PATH")]     // default analysis/mnq-minute-range-envelope.json
    envelope: Option<PathBuf>,
    /// The directory holding the five PRE-LANDING legacy tapes gate B1
    /// compares against (section 4.1), produced by the shipped binary
    /// at the parent commit.
    #[arg(long, value_name = "DIR")]      // default analysis/out/arrival-control-b1-baseline
    b1_baseline: Option<PathBuf>,
    /// Where the driver writes the five AFTER tapes it generates by
    /// exec'ing the shipped binary, before comparing them byte for
    /// byte against the baseline (section 4.1).
    #[arg(long, value_name = "DIR")]      // default analysis/out/arrival-control-b1-after
    b1_after: Option<PathBuf>,
    /// Where to write the artifact.
    #[arg(long, value_name = "PATH")]     // default analysis/mnq-arrival-control.json
    out: Option<PathBuf>,
}

/// Refuses a dirty tree BEFORE reading any input, exactly as
/// `minute_range_envelope::run` does, and for the same reason: an
/// artifact may only bind a commit that is exactly the code that ran.
/// Re-attests the tree from git immediately BEFORE the atomic write
/// (section 2.9), so a HEAD move or an edit during the six minutes of
/// walking unbinds the artifact rather than being recorded as clean.
pub fn run(args: ArrivalControlArgs) -> anyhow::Result<Value>;
```

### 3.3 Data flow, end to end

```text
1  require_clean_tree()                          -> harness_tree_commit
2  B5: spawn `brokkr check --gate`, record exit + output digests
   B1: spawn the shipped binary for the five section 16 tapes into
       --b1-after, compare byte for byte against --b1-baseline
   (both run BEFORE the walks: a red gate costs seconds, not minutes)
3  read + sha256 analysis/mnq-measure-12a.json   -> observed per_session, binding
   read + sha256 analysis/mnq-minute-range-envelope.json
4  obs   = ObsContext::new(artifact.observed.per_session)
   hours = gate_hours(profile)                  (23 for MNQ; no hour 21)
   o[h]  = hourly_mean_parents(obs)
5  for seed in CONTROL_FIT_SEEDS:                 (shipped curve)
       control_walk(.., None, seed) -> g_seed[h] = hourly_mean_parents
   g[h] = seed_median over the four seeds                   (5.5: median)
   ratio[h] = g[h] / o[h]
6  new_curve = recentred_curve(shipped_curve, ratio)
   drift     = normalizer_drift(shipped_curve, new_curve, profile)
7  for seed in CONTROL_TEST_SEEDS:                (corrected curve)
       control_walk(.., Some(&new_curve), seed) -> ControlWalk
8  gates B2, B3, B4, B6, B7 over those four walks, over `hours`
9  verdict = all(B1..B7) ? negative-control-passed : negative-control-failed
10 fresh_tree_state(): HEAD unmoved and tree still clean, or bail
   unbound - BEFORE anything is serialized (section 2.9)
11 write analysis/mnq-arrival-control.json atomically
```

Step 5 uses the SHIPPED curve deliberately: `ratio` is defined in 5.5
against the shipped generator, and measuring it under any other curve
would make the correction a function of itself.

## 4. The gates, defined exactly

Each gate is judged PER HOUR and PER SEED where 10.2 says per hour and
per seed, over the index set `gate_hours(profile)` of section 2.8 and
no other hour, and a gate fails if any cell fails. A cell that cannot be
evaluated is a RECORDED REFUSAL under 12a semantics
(`RefusalRec`), never an omission and never a silent pass: a gate with
any refusal FAILS, because 12b section 8 forbids dropping an
inconvenient hour and a refusal is exactly the state a dropped hour
would hide.

### B1 - legacy byte identity

Verbatim from 10.2: "Every shipped preset that does not declare the
arrival seam produces a byte-identical tape to the pre-landing binary,
by `cmp` over the fixed walks of section 16. Not statistical."

Section 16's walks, verbatim: symbol in BTCUSDT, ETHUSDT, SOLUSDT,
MES, MNQ; seed 7; length 2d; `gen --type trades` (the raw tape,
byte-complete - bars cannot prove tape identity); the committed anchor
(`FINAL_START_NS = 1782856800000000000`).

Procedure. The five baseline tapes are produced from the PRE-LANDING
commit before this brick's code exists, and compared after it lands:

```text
git stash --include-untracked           # or check out the parent commit
brokkr run mogwai -- gen --type trades --symbol BTCUSDT --seed 7 --length 2d --start 1782856800000000000 --out analysis/out/arrival-control-b1-baseline/BTCUSDT.csv
   ... the same for ETHUSDT, SOLUSDT, MES, MNQ ...
# then, on the brick-N tree:
brokkr run mogwai -- gen --type trades --symbol BTCUSDT --seed 7 --length 2d --start 1782856800000000000 --out analysis/out/arrival-control-b1-after/BTCUSDT.csv
   ... the same for the other four ...
cmp analysis/out/arrival-control-b1-baseline/BTCUSDT.csv analysis/out/arrival-control-b1-after/BTCUSDT.csv
   ... the same for the other four ...
```

`analysis/out/` is the scratch/cache root and these files are NOT
committed; the artifact records their SHA-256 digests, which is what
makes the comparison auditable after the files are gone.

REVIEW, and this replaces draft 1's mechanism. Draft 1 gave the shell
`cmp` procedure above and ALSO said the driver "regenerates each of
the five tapes in-process through the same profile path `gen` uses".
Those are not the same gate. 10.2's frozen wording is byte identity of
`gen --type trades` OUTPUT, and a freshly written in-process serializer
proves nothing about it: it could differ from `gen`'s CSV while the
tape is identical, or match the baseline while the real CLI path has
drifted. Nothing in the codebase exposes `gen`'s exact byte stream as
a reusable writer, and extracting one would be a `gen` refactor this
brick has no mandate for.

Resolved by exec'ing the shipped binary. The driver, for each of the
five symbols, runs `target/release/mogwai gen --type trades` with the
section 16 arguments as a SUBPROCESS, writing into `--b1-after`, then
compares the resulting file to the corresponding `--b1-baseline` file
byte for byte and records per symbol
`{argv, baseline_sha256, after_sha256, identical}`. The bytes compared
are therefore the CLI's own bytes on both sides, which is what 10.2
asks for. The binary is located as
`std::env::current_exe()`, since the driver IS the shipped binary and
so cannot disagree with itself about which build ran; a `current_exe`
that is not the release `mogwai` REFUSES rather than falling back to a
path guess.

A missing, unreadable or zero-length baseline file REFUSES the gate
rather than passing it, as does a non-zero subprocess exit. This is a
real gate and not a formality even though this brick touches no
generator: the point of B1 is that it is run, and 12b section 18
records the ruling that every gate needs a command that actually runs.

Supporting check, recorded alongside but not substituted for the
comparison: `git diff --stat <parent>..HEAD` touches no path under
`crates/mogwai-data/`, `crates/mogwai-protocol/`,
`crates/mogwai-server/presets/` or `analysis/fingerprint.json`, and
`TAPE_PROTOCOL_VERSION` is 11.

### B2 - support and conditional adequacy (contains A1)

Verbatim from 10.2: "The 12a count substitution runs unamended and
produces support for every implicated hour: no
observed-support-without-generated-support refusal at any of the 24
hours, and the 5.2 conditional adequacy guard evaluates rather than
refuses."

Mechanism:

```text
obs_shares = obs_shares_under(obs, &obs.ones())
for each test walk:
    gen_hist = pool_session_hists(walk.ctx.per_session())
    rec      = count_substitution(&gen_hist, &obs_shares)
    refusals = support_refusals_of(&rec)
cond = conditional_adequacy_bins(obs, &[test contexts])
```

PASSES iff every seed's `refusals` is empty AND every `CondBin` with
`required == true` has `supported == true`. Evidence records, per
seed, the refused hours and the substitution closure value, and the
full `CondBin` list once.

This is the gate 12b exists to move: `analysis/mnq-measure-12a.json`
records the shipped generator refusing 22 of 24 hours here.

### B3 - wall-time contour

Verbatim from 10.2: "Hourly 60 s and 300 s robust_scale
generated/observed ratios inside [0.8, 1.25] at every hour,
protocol-11 estimator. Inherited, not relaxable." The estimator is the
one-maximum-trimmed mean absolute fixed-horizon return, which is 12a
block 3's `robust_scale` and is what `ObsContext::b3_robust_strict`
reads.

Mechanism: for each hour the block records carry and each `h` in
`WALLTIME_HORIZONS_S`, `obs.b3_robust_strict(hour, h, &obs.ones())`
and the same on each test walk's context; the ratio
`generated / observed` must lie in `WALLTIME_BAND` for every hour and
every seed. `b3_robust_strict` returning `None` on either side is a
refusal (it refuses on any missing or non-finite session vote, which
is the 12a strictness rule and is not softened here).

### B4 - minute-range envelope, two-sided

Verbatim from 10.2: "p99 minute range inside a two-sided band whose
LOWER bound comes from the lower tail of the same resampled envelope
machinery that supplies the existing upper bound. The existing p99.9
and per-seed-max upper bounds continue to apply."

Bounds come from `analysis/mnq-minute-range-envelope.json`'s
`envelope` object and are NOT recomputed (brick B4 amendment 2):
`p99_lower = 210`, `p99 = 250`, `p99.9 = 399`, `max = 968`, in ticks,
under corpus job GLBX-20260805-HAPEWPABKG.

Per test seed, from that seed's `summary`:

```text
p99   = nearest_rank_of(minute_range_ticks_hist, 0.99)
p99.9 = nearest_rank_of(minute_range_ticks_hist, 0.999)
max   = minute_range_max_ticks
```

using `mogwai_lab::fit::observe::nearest_rank_of`, the same function
the protocol-11 driver uses, so the generated side of the comparison
is computed by the code that already computes it. It and
`mogwai_lab::fit::solve::SLACK` are both already `pub`; this brick
needs no visibility change anywhere. PASSES iff for every seed
`p99_lower <= p99 <= envelope.p99`, `p99.9 <= envelope["p99.9"]` and
`max <= envelope.max`, each with the protocol-11 `SLACK` on the upper
side and the same `SLACK` on the new lower side, so the two sides of
one band are not judged with different arithmetic.

REVIEW, recorded so the tolerance is not misread: `SLACK` is `1e-12`
ABSOLUTE and every quantity here is an integer tick count, so applying
it changes no verdict and is not a tolerance in any practical sense.
It is applied anyway, on both sides, for symmetry with the protocol-11
comparison path - but the artifact's B4 evidence records the raw
integer comparisons, and no reader should treat `SLACK` as slack.

### B5 - the standing generator gates

Verbatim from 10.2: "Every existing realism, rail, truncation and
preset-provenance gate stays green and `brokkr check --gate` is
green."

REVIEW, and this replaces draft 1's mechanism. Draft 1 took the
operator's `--b5-green-at <commit>` and passed B5 when that string
equalled `harness_tree_commit`. That proves only that the operator
typed HEAD. An artifact could record B5 passed - and reach
`negative-control-passed`, the verdict that ENDS the 12b landing -
after a red check or after no check at all. A hard gate that a typo
satisfies is not a gate.

The driver produces the evidence itself. It runs `brokkr check --gate`
as a subprocess on the clean tree before the walks begin, and records
`{command: "brokkr check --gate", commit: harness_tree_commit,
exit_status, stdout_sha256, stderr_sha256, duration_s}`. A non-zero
exit fails B5 with the captured status in the evidence; a `brokkr`
that cannot be spawned REFUSES B5. `--b5-green-at` is DELETED, which
also removes draft 1's deviation from brick N's frozen command in 12b
section 14 - the command is again exactly what 12b specifies plus
input and output paths.

Two facts about this that the implementer must not discover the hard
way. First, B5 runs FIRST, before the six minutes of walking, so a red
check costs seconds rather than minutes. Second, `brokkr check --gate`
builds, so the driver must not be invoked from inside a cargo
invocation that already holds the target-directory lock; the N3 test
`arrival_control_refuses_a_b5_that_cannot_be_spawned` exercises the
refusal path with a `PATH` that has no `brokkr` on it, and no test
runs the real check.

### B6 - mean-rate preservation (identical to A2)

Verbatim from 10.2: "Per hour AND per seed, generated / observed mean
parents per scheduled minute in MEAN_RATE_BAND."
`MEAN_RATE_BAND = [0.98, 1.02]`.

Mechanism: `hourly_mean_parents` on each test context against the same
on `obs`, over `gate_hours(profile)` and no other hour. Within that
index set, an hour whose observed `mean` is `None` or zero is a
refusal, not a skip; an hour outside it (hour 21 for MNQ) is absent
from the gate entirely and is neither judged nor refused (section
2.8). Evidence records the per-hour per-seed ratio
grid, plus `normalizer_drift` and the spread of the per-hour ratios,
so a uniform offset (section 2.4) is readable directly off the
artifact.

### B7 - sub-second composition (identical to A3)

Verbatim from 10.2: "Per hour AND per seed, the 1 s zero-count
fraction ratio in ZERO_COUNT_BAND." `ZERO_COUNT_BAND = [0.8, 1.25]`.

Mechanism: `hourly_zero_second_fraction` on each test context against
the same on `obs`; ratio per hour per seed inside the band, over
`gate_hours(profile)` only, with the same index-set rule as B6
(section 2.8). `None` on either side, or an observed fraction of
exactly zero, is a refusal.

REVIEW: block2 already serializes `zero_windows` and `zero_fraction`
directly at each `(hour, window)` cell, so `hourly_zero_second_fraction`
reads `zero_windows` over `scheduled_windows` at `(hour, 1)` rather
than summing `count_hist["0"]` as draft 1 said. Same number, one fewer
place for a histogram-key convention to drift, and it makes the "a
session that did not serialize the cell is a refusal" case a direct
field-absence test. The `count_hist["0"]` route is retained only as
the N2 test's cross-check: `the_zero_fraction_matches_the_count_hist`
asserts the two agree over the committed observed context.

## 5. The artifact

`analysis/mnq-arrival-control.json`, committed. The schema is brick
N's own, lifted verbatim and then pinned field by field; the `binding`
block follows the 12a style required by 12b section 13.

```text
{
  binding: {
    harness_tree_commit,          the clean commit this ran at
    clean_tree: true,
    input_hashes: {               path -> sha256, both inputs
      "analysis/mnq-measure-12a.json",
      "analysis/mnq-minute-range-envelope.json" },
    exposure: {                   READ from the 12a binding, not restated
      instrument: "MNQ", preset: "crates/mogwai-server/presets/mnq.toml",
      window_start_ns, window_length_ns, warmup,
      divergence: null, regime: "neutral" },
    control_fit_seeds:  [301,302,303,304],
    control_test_seeds: [305,306,307,308],
    gate_hours: [23 ints],        the index set actually judged (2.8)
    unexposed_hours: [21],        in the calendar, outside every gate
    tape_protocol_version: 11,
    spec: "notes/protocol-12b-arrival-composition-spec.md section 5.5, brick N"
  },
  ratios:   { "<hour>": { generated_mean, observed_mean, ratio,
                          generated_per_seed: [4 floats],
                          dropped_nonfinite: int } },
  old_curve: [24 floats],         the shipped mean-1 curve, sum 23.862306
  new_curve: [24 floats, summing to 1],   NOT mean-1; see section 2.4
  normalizer_drift: float,        section 2.4's K, computed
                                  scale-invariantly; reported, never
                                  applied
  gates: { "B1".."B7": { passed: bool, evidence: <per-gate record>,
                         refusals: [RefusalRec] } },
  verdict: "negative-control-passed" | "negative-control-failed",
  failing_gates: [names],
  cost: { fit_walk_s, test_walk_s, b1_s, b5_s, total_s, peak_rss_bytes }
}
```

`B8` is absent by inapplicability, not recorded as passed or refused
(5.5). `failing_gates` is `[]` on a pass. `ratios` carries the
per-seed readings as well as the median so the median is auditable
rather than asserted.

## 6. Bricks

The whole item is ONE landing, split into three commits so the suite
is green at every boundary. Each commit's gates are exact commands.

### Brick N1: the clean-tree helper moves to the lab

`mogwai_lab::ledger::require_clean_tree` and
`mogwai_lab::ledger::fresh_tree_state`, with
`crates/mogwai-cli/src/measure.rs`, `crates/mogwai-cli/src/fit.rs` and
`crates/mogwai-cli/src/minute_range_envelope.rs` rewritten to call
them and their private copies deleted (section 2.6, section 2.9).
Behavior-preserving, but NOT type-preserving: the error type becomes
`LabResult` and each call site maps it back to `anyhow` with the
message text unchanged byte for byte.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-cli minute_range_envelope_refuses_a_dirty_tree_before_reading_inputs
brokkr test -p mogwai-cli fit_refuses_a_dirty_tree
brokkr test -p mogwai-cli measure
```

### Brick N2: the control module

`crates/mogwai-lab/src/arrival_control.rs` and its `lib.rs`
registration: the constants, `hourly_mean_parents`,
`hourly_zero_second_fraction`, `recentred_curve`, `normalizer_drift`,
`GeneratedBinding`, `control_walk`, and the five statistical gates.
No CLI yet, so nothing is reachable from a command and nothing is
written.

New tests, each pinning a claim this spec makes:

- `hourly_mean_parents_divides_by_scheduled_not_populated_minutes` -
  over a hand-built two-session context carrying an empty minute, the
  denominator is the block2 `(hour, 60)` `scheduled_windows` sum and
  not the block1 row count.
- `the_recentred_curve_divides_by_the_ratio_and_sums_to_one` - the
  ratio division is elementwise, an hour absent from `ratios` is
  carried through unchanged, and the result sums to 1 within one ulp
  of the `py_sum` reduction.
- `the_recentred_curve_is_scale_invariant_before_the_rescale` -
  multiplying every input by a constant leaves the output identical,
  which is the 5.5 neutrality claim as a test.
- `normalizer_drift_is_one_for_a_constant_ratio` - and is the
  exposure-weighted ratio otherwise, checked against a directly summed
  week of open minutes.
- `the_control_walk_pair_replays_one_tape` - for one seed and a fixed
  curve, the `GeneratedAcc` pass and the `summarize` pass see the same
  parent count and the same first and last event timestamps, which is
  section 2.5's determinism claim. Run at a SHORT window (one day, no
  warmup) so it fits the test ceiling; the exposure contract governs
  the artifact run, not this pin.
- `a_gate_with_a_refusal_fails_rather_than_passing_vacuously` - each
  of B2, B3, B4, B6 and B7 over a context with one missing cell
  reports `passed: false` with a non-empty `refusals`.
- `gate_b4_is_two_sided` - a synthetic summary whose p99 sits below
  `p99_lower` fails, and the same summary at `p99_lower` exactly
  passes.
- `gate_hours_excludes_the_unexposed_hour` - MNQ yields 23 hours and
  hour 21 is not among them (section 2.8).
- `an_unexposed_hour_is_not_a_refusal` - B6 and B7 over a generated
  context matching the observed one report `passed: true` with no
  hour-21 cell and no refusal.
- `the_seed_median_averages_the_two_middle_readings` - and returns
  `None` below four finite readings (section 2.10).
- `the_zero_fraction_matches_the_count_hist` - the `zero_windows`
  route and the `count_hist["0"]` route agree on the committed
  observed context.
- `the_binding_renders_its_window_length_as_whole_seconds` - and
  refuses a sub-second remainder (section 2.7).

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-lab hourly_mean_parents_divides_by_scheduled_not_populated_minutes
brokkr test -p mogwai-lab gate_hours_excludes_the_unexposed_hour
brokkr test -p mogwai-lab an_unexposed_hour_is_not_a_refusal
brokkr test -p mogwai-lab the_seed_median_averages_the_two_middle_readings
brokkr test -p mogwai-lab the_zero_fraction_matches_the_count_hist
brokkr test -p mogwai-lab the_binding_renders_its_window_length_as_whole_seconds
brokkr test -p mogwai-lab the_recentred_curve_divides_by_the_ratio_and_sums_to_one
brokkr test -p mogwai-lab the_recentred_curve_is_scale_invariant_before_the_rescale
brokkr test -p mogwai-lab normalizer_drift_is_one_for_a_constant_ratio
brokkr test -p mogwai-lab the_control_walk_pair_replays_one_tape
brokkr test -p mogwai-lab a_gate_with_a_refusal_fails_rather_than_passing_vacuously
brokkr test -p mogwai-lab gate_b4_is_two_sided
```

### Brick N3: the command and the artifact

`crates/mogwai-cli/src/arrival_control.rs`, its `lib.rs` registration,
the `main.rs` dispatch, the B1 tape comparison and the B5 attestation,
the artifact writer, and the man/CLI documentation update
(`docs/cli.md` gains the subcommand alongside `minute-range-envelope`;
markdown rides this commit rather than travelling alone).

New tests:

- `arrival_control_refuses_a_dirty_tree_before_reading_inputs` - the
  sibling of the `minute_range_envelope` test, and the reason `run` is
  public.
- `arrival_control_refuses_a_tree_that_changed_during_the_run` -
  section 2.9; the tree is dirtied mid-run at a shortened window.
- `arrival_control_refuses_a_b5_that_cannot_be_spawned` - a `PATH`
  with no `brokkr` on it refuses rather than passing B5.
- `arrival_control_refuses_a_missing_b1_baseline_rather_than_passing_b1`.
- `the_control_artifact_carries_no_b8_field` - inapplicability is
  absence, not a recorded pass (5.5).
- `the_lab_walk_matches_the_measure_exposure_contract` - the section
  2.7 equality pin between `control_walk`'s `GeneratedAcc` pass and
  `measure::run_final_walk`. It lives here because only this crate can
  see both.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-cli arrival_control_refuses_a_dirty_tree_before_reading_inputs
brokkr test -p mogwai-cli arrival_control_refuses_a_tree_that_changed_during_the_run
brokkr test -p mogwai-cli arrival_control_refuses_a_b5_that_cannot_be_spawned
brokkr test -p mogwai-cli arrival_control_refuses_a_missing_b1_baseline_rather_than_passing_b1
brokkr test -p mogwai-cli the_control_artifact_carries_no_b8_field
brokkr test -p mogwai-cli the_lab_walk_matches_the_measure_exposure_contract
```

### The artifact run

After N3 is committed and the tree is clean, on a machine with no
corpus required:

```text
brokkr run mogwai -- arrival-control --out analysis/mnq-arrival-control.json
```

which is brick N's command from 12b section 14 exactly, all other
paths defaulted. The B1 baselines are produced first, per section 4.1,
from the PARENT commit's binary; the driver then runs B5 and B1 itself
before any walking (section 3.3).

Then read the verdict and stop:

- `negative-control-passed` - the 12b landing closes here. Report to
  the owner: the premise that a new stochastic shape is required has
  been falsified.
- `negative-control-failed` - record the failing gates and stop.
  Item 2 is a separate spec.

## 7. Cost

Twelve month-long in-process walks: four fit walks
(`GeneratedAcc` only), four test walks doubled for the two
accumulators. At the phase-2a measured ~26 s per generated month-walk,
that is roughly 5 to 6 minutes of walking plus the gate arithmetic,
which is seconds. No corpus pass, no observed pass, no resampling.

REVIEW, added: that is not the whole bill. B1 generates five two-day
`gen --type trades` tapes in-process-adjacent subprocesses (the five
baselines are produced separately, at the parent commit, and are not
charged to the run), and B5 runs `brokkr check --gate`, which BUILDS.
Budget a further one to two minutes for the five tapes and whatever
the gate check costs on a warm target directory - several minutes cold.
The artifact's `cost` block gains `b1_s` and `b5_s` so both are
measured rather than estimated, and `total_s` covers all of it.
Peak RSS is one walk plus five `ObsContext`s over 22 sessions each,
far under the 8 GiB `STAGE_A_RSS_BYTES` scale the later bricks work
at. There is no cost probe for this brick and none is owed: 12b gives
cost probes to A and S, not N, and this budget is small enough that a
miss costs one re-run rather than a re-freeze.

The artifact records the measured cost, so the estimate above is
checked by the run rather than trusted.

## 8. Keep/revert

12b section 15: bricks K, N, B4, A0 and A are additive and
independently revertible; none changes a tape byte and each is kept or
reverted on its own gate.

Concretely for this item: N1, N2 and N3 each stand alone and each is
reverted on its own gate list. The artifact is a committed derivative
of N3 plus the committed inputs; if it is ever regenerated and
disagrees, the RECOMPUTATION is authoritative and the committed file
is stale, exactly as brick B4 amendment 2 rules for its own artifact.

A gate FAILURE in the artifact is not a brick failure and reverts
nothing: `negative-control-failed` is a recorded measurement and the
predicted outcome (5.5). What would revert this brick is a gate that
cannot be EVALUATED - a statistic the committed inputs cannot supply -
and that is a 12b section 17 stop, not a workaround.

## 9. Stopping rule

Out of scope, named and excluded rather than deferred:

- Everything in 12b section 17's list, which applies unchanged.
- Stage A and Stage B in every part: no screen driver, no
  `arrival-screen` or `arrival-solve` command, no `--cost-probe` mode,
  no fidelity layers, no conformance vectors, no grid. Those are items
  2 and 3.
- `crates/mogwai-lab/src/arrival_screen.rs` is NOT touched by this
  brick. It is item 2's file.
- `crates/mogwai-data/src/generated/arrival.rs` and the kernel are not
  touched: the control has no cadence kernel and never reaches one.
- The `dow_weight` axis. 5.5 scopes the control to the HOUR axis and
  states the consequence: a day-of-week-shaped error survives the
  control, so the control's failure falsifies less than "the curve is
  right". Correcting `dow_weight` here would be an unamended widening
  of a frozen mechanism.
- Any second iteration of the correction, including dividing out
  `normalizer_drift` (section 2.4).
- Any preset edit. The corrected curve reaches the generator through
  an in-memory scratch override and is written to no preset, ever.

If implementation proves a constant, gate or statistic here
unmeasurable, this brick FAILS and stops; a reviewed amendment to
`notes/protocol-12b-arrival-composition-spec.md`, dated, restarts
Brick F before implementation resumes. No artifact may be produced
under a partially amended contract.

## 10. Frozen constants, lifted verbatim

```text
CONTROL_FIT_SEEDS   = 301..304        the ratios are measured here
CONTROL_TEST_SEEDS  = 305..308        the corrected curve is judged here
CONFIRMATION_SEEDS  = 1..8            NOT touched by this brick at all
MEAN_RATE_BAND      = [0.98, 1.02]    B6
ZERO_COUNT_BAND     = [0.8, 1.25]     B7
wall-time band      = [0.8, 1.25]     B3, inherited, not relaxable
WALL horizons       = 60 s, 300 s     B3
FINAL_START_NS      = 1782856800000000000   the committed anchor, B1
B1 walks            = BTCUSDT, ETHUSDT, SOLUSDT, MES, MNQ; seed 7;
                      length 2d; gen --type trades
minute-range bounds = p99_lower 210, p99 250, p99.9 399, max 968
                      (analysis/mnq-minute-range-envelope.json, B4)
TAPE_PROTOCOL_VERSION stays 11
```

Derived, not lifted, and therefore this spec's own (section 2.8): the
MNQ gate index set is the 23 calendar-open hours, hour 21 UTC excluded
as unexposed. It is computed from the preset calendar at runtime, not
written down as a constant, so a calendar change moves it rather than
silently contradicting it.

## 11. The predicted outcome, recorded before the run

12b section 5.5 predicts failure and this spec records the prediction
so a failure is not later mistaken for a surprise: protocol 11 already
fitted the hourly marginal parent counts to a worst-hour error of 0.63
percent, so the hourly MEANS are not what is wrong, and a
deterministic hourly rate cannot produce a within-hour MIXTURE - at
hour 20 the observed distribution spans four bins and lowering the
curve to reach `65-256` cannot simultaneously produce the `1025-4096`
mass, and at hour 19 no mean shift explains the `4097+` mass.

The gate this bites is B2. B6 and B3 are expected to pass or nearly
pass, since the correction targets exactly the hourly mean and leaves
wall-time shape alone. If B2 passes, that is the falsification the
control exists to make possible, and the landing stops for the owner.

The control runs anyway, because a predicted failure that is not run
is an assumption.

## 12. Review ledger

Draft 1 was reviewed twice independently. Every finding below was
checked against the code and the committed artifacts before being
folded or rejected; the two reports agreed on three items, which are
merged.

Folded, with where each landed:

| Finding | Verified by | Landed in |
|---|---|---|
| Hour 21 makes B6 and B7 fail by construction | all 22 sessions carry 23 block2 hours, hour 21 absent | 2.8, 3.1, 4 preamble, B6, B7, schema |
| `normalizer_drift` measures the wrong thing | `mnq.toml` `intensity_hour` sums to 23.862306, not 1 | 2.4, schema |
| `control_walk` cannot call `run_final_walk` | `mogwai-cli` depends on `mogwai-lab`, not the reverse | 2.7, 2.1, N3 tests |
| `run_summary_walk` takes duration strings, not ns | its signature | 2.7, `GeneratedBinding::length_arg` |
| The four-seed median has no convention | 5.5 is silent; draft 1 named none | 2.10, `seed_median` |
| B1's two mechanisms are different gates (both reports) | no reusable `gen` CSV writer exists | 4, B1 |
| B5 is operator self-attestation (both reports) | draft 1 compared a string to HEAD | 4, B5, CLI args |
| `require_clean_tree` has three copies, not two (both reports) | `measure.rs`, `fit.rs`, `minute_range_envelope.rs` | 2.6, N1 |
| N1 is not a pure refactor: the error type changes | `anyhow::Result` today, `LabResult` proposed | 2.6, N1 |
| The tree can move during the six-minute run | `measure.rs` re-attests before its write; draft 1 did not | 2.9, 3.3, N3 test |
| `SLACK` is decorative on integer ticks | `SLACK = 1e-12`, absolute | B4 |
| B7 should read `zero_windows`, not `count_hist` | block2 serializes both | B7, 3.1, N2 test |
| `HOURS` is declared and used nowhere | draft 1 section 3.1 | deleted, replaced by `gate_hours` |
| Gate return types are inconsistent | three bare, two `LabResult` | 3.1, now uniform |
| Cost omits the B1 tapes and the B5 check | draft 1 counted only the walks | 7, `cost` block |

Rejected, and why:

- "12b 5.5's sum-to-1 rescale is out of step with the shipped preset
  convention, so fix it here." The observation is correct and is now
  recorded in 2.4, but 5.5 is FROZEN and section 17 makes an amendment
  a Brick-F restart. The rescale moves no generated rate, so nothing
  is at stake in the artifact beyond readability, which 2.4 handles by
  telling the reader what scale each curve is on. Flagged upward, not
  amended.
- "Keep B5 entirely outside the command." One review's preferred
  remedy. Rejected in favour of the driver running `brokkr check
  --gate` itself: an out-of-band check reintroduces exactly the
  operator-attestation gap the finding raised, whereas a subprocess
  with a recorded exit status and output digests is machine-checkable
  and keeps the artifact self-contained. The finding itself is
  accepted in full; only the remedy differs.
- "The extra artifact fields deviate from brick N's frozen schema."
  Rejected. 12b section 13 fixes what a binding block must CONTAIN,
  not what it may not add, and every added field
  (`gate_hours`, `unexposed_hours`, `normalizer_drift`, the per-seed
  ratios, `b1_s`, `b5_s`) is evidence for a gate the frozen schema
  already requires. The command-surface half of the same finding IS
  accepted and is now moot: `--b5-green-at` is gone, so the command
  matches 12b section 14 exactly.

No finding was rejected as factually wrong. Both reviews checked out
against the code on every claim tested.
