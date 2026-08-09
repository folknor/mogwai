# Protocol 12b item 2: the Stage A screen (bricks A0 and A)

DRAFT 2, written 2026-08-09. Draft 1 was reviewed twice (an Opus
cross-check against the landed code and a codex read-only review); every
valid finding of both is folded in below, and section 10 records what was
rejected and why.

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/protocol-12b-arrival-composition-spec.md` section 0, work item 2,
which names the binding sections: 9 (projections, admissibility A1 to A4,
loss, output), 6 (fidelity layers; the layer-1 oracle test is BLOCKING), 3
(the stage boundary and what Stage A may not evaluate), 12 (procedure,
budgets, the two-tier cost probe), 16 (grids and constants), 7 and 8,
bricks A0 and A (gate commands), and 13 (artifacts).

This is a `notes/`-class document: transient, no truth guarantee, nothing
durable may cite it.

The 12b document remains the CONTRACT OF RECORD. Every constant, seed set,
gate definition and gate command below is lifted from it verbatim. This
spec amends nothing. Where writing it exposed a question the 12b document
does not answer, that question is answered here as an IMPLEMENTATION
DECISION over ground the 12b document leaves to the implementer, and each
one is labelled as such with the reasoning; where a question could only be
answered by changing a frozen constant, family, gate or statistic, this
spec would instead be a section 17 stop, and there are none.

---

## 1. The goal, and where this item stops

Build the Stage A screen: the corpus-free, no-running-generator-change
screen of NECESSARY conditions that advances every admissible
family-region pair and SELECTS NONE. Deliver `mogwai arrival-screen` with
its `--cost-probe` mode, the fidelity layers, and the committed artifact
`analysis/mnq-arrival-screen.json`.

Stage A's authority is parent timestamps and cadence only (12b section 3).
It may not evaluate the whole Block 1 histogram, because that carries
`trade_range_ticks` and `quote_range_half_ticks`, which need the price and
book paths; it sees the parent-count marginal and nothing else from Block
1. The count substitution and its conditional range-law guard are Stage B
statistics. It may not discard an admissible family because another scores
better: loss ranks evaluation ORDER only.

Stage A is corpus-free and consumes neither the corpus nor the brick-B4
envelope artifact (brick B4 amendment 2). It runs on any clone.

### 1.1 Stop points, which are owner decisions

- A cost-probe miss FAILS brick A and stops. A PER-CELL miss stops for an
  owner ruling on the per-cell price, which a grid re-freeze cannot fix; a
  TOTAL-budget miss stops for a grid re-freeze. The two are separated in
  5.1 because draft 1 conflated them. The budget is never met by trimming
  the grids silently, and never by moving a band.
- A full run that crosses `STAGE_A_BUDGET_S` or `STAGE_A_RSS_BYTES` stops
  without writing an artifact (5.1.1) and goes to the owner.
- An empty admissible region across every family closes the landing with
  `no-arrival-admissible-candidate-in-frozen-search-space` and goes to the
  owner. Stage A failure is named that and never the stronger verdicts,
  which only Stage B reaches.
- A pass produces the verdict `arrival-admissible: <families>` and the
  loop proceeds to item 3. No work past that verdict belongs to this item.

### 1.2 Out of scope, named and excluded rather than deferred

Everything in 12b section 17, plus, for this item specifically: Stage B
and every gate of 12b section 10.2 (B1 to B8), the `arrival-solve`
binary, the `presets emit --omit` mode, the seam declaration in
`presets/mnq.toml`, `TAPE_PROTOCOL_VERSION = 12`, the legacy byte-identity
procedure, confirmation, and the grid-sensitivity re-run of 9.4. Those are
item 3. This item lands no generator change and moves no tape byte:
`TAPE_PROTOCOL_VERSION` stays 11 and the assertion that it does is part of
the artifact's binding block.

## 2. Survey of the ground

### 2.1 What brick K already landed, and what it means for this item

`crates/mogwai-data/src/generated/arrival.rs` carries the full seam and it
is REACHABLE from the generator, not a dormant library:

- `ArrivalConfig`, a serde-tagged enum over `event_markov`, `wall_mmpp`,
  `log_ou_cox` and `self_exciting`, deserialized from the preset's
  `[instrument.generator.arrival]` table into `GeneratorScalars::arrival`
  (an `Option`, defaulted absent), with `is_valid` enforcing the section
  16 domains and `kernel()` returning `None` for `EventMarkov` and the
  matching `ArrivalKernel` for the three integrated families.
- `ArrivalEnv::for_profile(&SessionProfile, Option<&SessionCalendar>,
  thin, origin_ns)`, `rate_at` returning exactly zero in a closure,
  `ArrivalState::new`, `RuntimeModifiers::NEUTRAL`, `ParentDraw` with its
  `parent_ts_ns`, `child_count`, `next_from_ns`, `latent_x` and
  `reopen_applied`, `ArrivalRefusal`, and
  `ArrivalKernel::next_parent(state, from_ns, base_mean_s, shape, env,
  modifiers, rng) -> Result<ParentDraw, ArrivalRefusal>`.
- `GeneratedSource::try_new_with_session_profile` reads
  `scalars.arrival`: the `EventMarkov` variant overrides the shipped
  `(quiet_fraction, quiet_active_ratio, switch_rate)` triple on the
  SHIPPED path (contract A, no cadence stream constructed), and the three
  kernel variants build a cadence `ChaCha12Rng` seeded
  `splitmix64(seed ^ CADENCE_STREAM_TAG)`, an `ArrivalState` and an
  `ArrivalEnv`, routing `begin_event` into `begin_integrated_event`.
- `GeneratedSource::advance_parent()` returns
  `ParentSummary { parent_ts_ns, child_count, child_stride_ns }` after
  running every draw of the real walk, and asserts it is called on a
  parent boundary.

Two consequences this item depends on, both good news the 12b document
could not assume when it was frozen: family 1 needs no new generator work
at all, only a `GeneratorScalars::arrival` set to `EventMarkov` before the
walk; and the three kernel families are already executable in isolation
through `ArrivalKernel::next_parent`.

### 2.2 What is MISSING for a cadence-only Stage A walk

`ArrivalKernel::next_parent` is public but the surrounding assembly is
not. `CADENCE_STREAM_TAG` is a private constant of
`crates/mogwai-data/src/generated/source.rs`; `ARRIVAL_MEAN_CAL` (0.944)
and `INTRA_EVENT_STEP_NS` (1,000) are `pub(super)` in `consts.rs`; the
neutral `RegimeState::arrival_thin` (1.0) is `pub(super)`; and
`SweepShape` is public but the mapping from `GeneratorScalars` to it lives
inside the source constructor. A `mogwai-lab` driver that re-derived any
of those would be a SECOND implementation of the cadence environment,
which is exactly the defect 12b section 4 spent three revisions killing.
Section 3.1 below closes this with one public constructor both sides call,
which is what section 4.1 already demands in prose.

### 2.3 What `mogwai-lab` already carries

- `mogwai_lab::arrival_screen` (landed with brick B4's sibling work):
  `PARENT_COUNT_BINS`, `MIN_MINUTES_CELL = 30`, `FAIL_HOURS_300 =
  {19, 20, 23}`, `bin_name`, `parent_count_marginal` (marginalizes a
  `block1.hist` down to the count axis, refusing a malformed row rather
  than skipping it), `bin_totals`, `wasserstein_log1p` (the exact
  1-Wasserstein distance over `log1p(N)`, `None` when exactly one side is
  empty), `linear_grid` and `log_grid` with their point counts pinned.
  There is no driver, no CLI and no artifact.
- `mogwai_lab::arrival_control` (brick N): `GeneratedBinding`, which parses
  `binding.generated`'s `window_start_ns`, `window_length_ns` and `warmup`
  out of the committed 12a artifact and refuses on any missing field;
  `gate_hours(profile)`, the calendar-exposed hour index set (MNQ yields
  23; hour 21 UTC is the daily break); `hourly_mean_parents(ctx)`, mean
  parents per SCHEDULED minute per hour, denominated on the block2
  `(hour, 60)` `scheduled_windows` sum; `hourly_zero_second_fraction(ctx)`,
  block2 `(hour, 1)` `zero_windows` over `scheduled_windows`; and the
  landed gates `gate_b2`, `gate_b6`, `gate_b7`, whose predicates A1, A2 and
  A3 must reappear verbatim in.
- `mogwai_lab::measure12a::SessionAcc` with `push_print(ts, price_nanos)`,
  `push_parent(segment_index, first_ts, bid, ask, book_normal)` and
  `close(scope)`, which emits `block1_hist` and the whole `block2` record.
  Block 2 reads `seg.parent_ts` and the segment schedule ONLY; block 1's
  count axis reads `n_min`, keyed by first-child timestamp, over a minute
  universe that is the union of print minutes and parent minutes.
- `mogwai_lab::aggregate::context::ObsContext`, the per-session view every
  landed gate consumes; `RefusalRec`; `mogwai_lab::ledger`'s
  `require_clean_tree`, `fresh_tree_state`, `sha256_file`;
  `mogwai_lab::sampler::ResourceSampler` for wall time and peak RSS;
  `mogwai_lab::storage`'s `ProvenanceToken`, `CacheStore` and
  `artifact_path`.
- `mogwai_lab::fit::walk`'s `WalkCache` conventions (`get`, `put`,
  `CacheStats`) and `parse_duration`.

### 2.4 What the 12a artifact supplies, and what it does not

`analysis/mnq-measure-12a.json` carries `observed.monthly.block1.hist`
(29,971 rows over 23 traded hours), `observed.monthly.block2` per hour and
per window in `COUNT_WINDOWS_S = {1, 5, 60}`, `observed.per_session` (the
22 usable sessions the landed gate helpers consume), the eight committed
seeds' `generated.per_seed[*].blocks`, and `binding.generated`. Nothing in
this item reads a corpus, a TBBO file or the brick-B4 envelope.

The observed side of A2 and A3 is computed from `observed.per_session`
through the SAME two helpers the landed B6 and B7 gates use. That is what
makes the nesting exact rather than asserted.

### 2.5 The generated-side minute universe, pinned from 12a

12a section 3.1: a populated minute on the GENERATED side is one
containing at least one child trade; parent count `N` is the number of
sided inferred parents attributed to the minute by FIRST-CHILD timestamp;
a minute with prints but zero sided parents occupies the parent-count-zero
bin. Every generated parent is sided (the bounce emits buyer or seller
only) and its first child carries the parent's own timestamp, so the
generated minute universe is exactly the set of minutes touched by a child
timestamp `parent_ts_ns + i * INTRA_EVENT_STEP_NS` for `i < child_count`,
and `N` per minute counts parents by `parent_ts_ns`. A burst that straddles
a minute boundary therefore contributes a trailing `N = 0` minute. This is
a projection of cadence data alone, which is why Stage A can compute it,
and it is the fact the whole corpus-free property rests on.

## 3. The target, as concrete artifacts

### 3.1 `mogwai-data`: one public cadence-walk constructor

The environment assembly moves out of `GeneratedSource::try_new_with_session_profile`
into a public type both callers construct, so there is exactly one
assembly and no constant needs exporting.

```rust
// crates/mogwai-data/src/generated/arrival.rs

/// The cadence half of a generated walk, assembled from the same inputs
/// `GeneratedSource` assembles it from and driven independently.
///
/// This is the ONLY public way to run an integrated arrival family
/// outside the generator. `GeneratedSource` builds its own cadence state
/// through `CadenceWalk::assemble`, so the two cannot diverge: there is
/// one assembly, not two.
pub struct CadenceWalk {
    kernel: ArrivalKernel,
    state: ArrivalState,
    env: ArrivalEnv,
    shape: SweepShape,
    rng: ChaCha12Rng,
    base_mean_s: f64,
    clock_ns: u64,
}

/// What both callers need out of one assembly. Returned as a struct
/// rather than a tuple because `GeneratedSource` stores the parts in
/// separate fields and a five-tuple at a call site is unreadable.
pub struct CadenceParts {
    pub kernel: Option<ArrivalKernel>,
    pub state: Option<ArrivalState>,
    pub env: Option<ArrivalEnv>,
    pub rng: Option<ChaCha12Rng>,
    pub base_mean_s: f64,
}

impl CadenceWalk {
    /// The single assembly. `None` kernel for a seam-absent instrument or
    /// for `EventMarkov`, which is a parameterization of the shipped path
    /// and never constructs a cadence stream (12b section 7, contract A).
    ///
    /// `thin` is the regime's arrival thin factor: exactly 1.0 under the
    /// neutral exposure of 12b section 8.
    pub fn assemble(
        scalars: &GeneratorScalars,
        session: &SessionProfile,
        calendar: Option<&SessionCalendar>,
        thin: f64,
        seed: u64,
        start_ts_ns: u64,
    ) -> CadenceParts;

    /// A standalone cadence walk for an integrated family. `None` when
    /// `scalars.arrival` names no kernel family, which is a caller error
    /// Stage A turns into a refusal rather than a panic.
    pub fn new(
        scalars: &GeneratorScalars,
        session: &SessionProfile,
        calendar: Option<&SessionCalendar>,
        thin: f64,
        seed: u64,
        start_ts_ns: u64,
    ) -> Option<Self>;

    /// The next parent under `RuntimeModifiers::NEUTRAL`, advancing the
    /// internal clock to `draw.next_from_ns` exactly as `step_child`
    /// advances the generator's. No caller-side arithmetic.
    pub fn next(&mut self) -> Result<ParentDraw, ArrivalRefusal>;

    /// The child stride, so a projection can enumerate child timestamps
    /// without importing a `pub(super)` constant.
    pub const fn child_stride_ns(&self) -> u64;
}
```

`GeneratedSource::try_new_with_session_profile` calls
`CadenceWalk::assemble` and stores its parts. The behavior is unchanged by
construction; the byte-identity pin in 3.4 proves it.

`CADENCE_STREAM_TAG`, `ARRIVAL_MEAN_CAL`, `INTRA_EVENT_STEP_NS` and
`arrival_thin` stay private. Nothing leaks.

### 3.2 `mogwai-lab::arrival_screen`: the driver

Added to the existing module (which keeps its landed contents unchanged):

```rust
/// The four families Stage A screens, by their artifact spelling. Of the
/// FIVE frozen families of 12b section 5, family 5 (Legacy, the negative
/// control) is absent by construction: it is not a Stage A survivor and
/// never enters the family ranking (12b section 5.5). Four variants, not
/// five, and deliberately.
pub enum Family { EventMarkov, WallMmpp, LogOuCox, SelfExciting }

/// One grid cell: the family and its fitted coordinates, named as the
/// SEAM names them so a cross-check against a preset's
/// `[instrument.generator.arrival]` table is mechanical rather than a
/// translation. Family 1 carries only `switch_rate`; `quiet_share = 0.35`
/// and `rate_ratio = 150` are DECLARED-HELD in both stages. The 12b
/// section 16 shorthands `w`, `q`, `r`, `sigma_Y`, `phi` and `tau` map
/// onto these one-for-one and appear nowhere in the code or the artifact.
pub enum Cell {
    EventMarkov  { switch_rate: f64 },
    WallMmpp     { occupancy: f64, rate_ratio: f64, tau_s: f64 },
    LogOuCox     { sigma_y: f64, tau_s: f64 },
    SelfExciting { phi: f64, tau_s: f64 },
}

impl Cell {
    /// The seam value a walk is configured with.
    pub fn config(&self) -> mogwai_data::ArrivalConfig;
    /// The fitted-parameter count, counted rather than assumed: 1, 3, 2, 2.
    ///
    /// Stage A CONSUMES THIS NOWHERE - the Stage A loss carries no
    /// parameter penalty. It is recorded per cell in the artifact because
    /// it is a Stage B input: 12b section 10.3 breaks a family tie on
    /// fewest fitted parameters, and counting them here, next to the grid
    /// that defines them, is what keeps that count from being re-derived
    /// by hand later. It is declared surface with a downstream consumer,
    /// not dead API.
    pub const fn fitted_params(&self) -> u8;
    /// The canonical cache and artifact key: family name plus each
    /// coordinate at 17 significant digits, so two f64s that differ in the
    /// last bit are two keys. Coordinates appear in the declaration order
    /// above, which is also the artifact's `params` key order.
    pub fn key(&self) -> String;
}

/// The coarse tensor grids of section 16, exactly.
pub fn coarse_grid(family: Family) -> Vec<Cell>;

/// The refinement pass: half spacing around the admissible region's
/// boundary cells, to REFINEMENT_DEPTH, capped per family.
pub fn refine(family: Family, admissible: &[Cell], depth: u8, cap: usize) -> Vec<Cell>;

/// One cell's per-seed walk product: the reduced session records the
/// admissibility conditions and the loss consume, plus validity evidence.
pub struct SeedWalk {
    pub seed: u64,
    pub sessions: Vec<serde_json::Value>,  // ScreenSession records, 3.3
    pub parents: u64,
    pub realized_mean_gap_s: f64,
    pub refusal: Option<ScreenRefusal>,
    pub cost_s: f64,
}

/// A4's validity failures, recorded with variant and clock. A refused cell
/// never enters the loss.
pub enum ScreenRefusal {
    Arrival { variant: &'static str, clock_ns: u64, detail: String },
    NonFiniteState { clock_ns: u64 },
    MeanGap { realized_s: f64, declared_s: f64 },
    Projection { detail: String },
}

/// The per-condition per-seed verdicts for one cell.
pub struct CellVerdict {
    pub cell: Cell,
    pub a1: ConditionRec,
    pub a2: ConditionRec,
    pub a3: ConditionRec,
    pub a4: ConditionRec,
    pub admissible: bool,
    pub loss: Option<f64>,
    pub reported: Reported,      // Fano, count-p99, six-bin TV: never gates
    pub refusals: Vec<RefusalRec>,
    pub cost_s: f64,
}

/// Drives one cell across a seed set, projecting, evaluating A1 to A4 in
/// that order and stopping at the first failure for that seed - the
/// conditions are per-seed and failure-monotone, so an early stop changes
/// no verdict and is not a shortcut that could admit something.
pub fn evaluate_cell(
    ctx: &ScreenContext,
    cell: &Cell,
    seeds: &[u64],
) -> LabResult<CellVerdict>;

/// Everything a cell evaluation needs, built once: the resolved MNQ
/// profile, the exposure binding, the observed projections, the gate hour
/// set, the cache and the scratch directory.
pub struct ScreenContext { /* fields below */ }
impl ScreenContext {
    pub fn open(measure_path: &Path, cache: Option<&Path>) -> LabResult<Self>;
    pub fn observed_marginal(&self) -> &CountMarginal;
    pub fn gate_hours(&self) -> &[i64];
}
```

### 3.3 The projection, pinned step by step

The projection is the one place a Stage A implementation could
accidentally differ from the generated side of the 12a measurement, so it
is specified as a procedure rather than as an outcome, and it REUSES
`SessionAcc` rather than reimplementing block 1 or block 2.

For one cell and one seed:

```text
1. Resolve MNQ through `Config::load` against
   `crates/mogwai-server/presets/mnq.toml` (12b section 8). Set
   `scalars.arrival = cell.config()`. Nothing else is overridden.
2. walk_start = binding.window_start_ns - parse_duration(binding.warmup)
   start = binding.window_start_ns
   end   = start + binding.window_length_ns
   offset = calendar.utc_offset_minutes
   `offset` is widened to `i32` at this step: `SessionCalendar::
   utc_offset_minutes` is `i16` and `session_segment_at` takes `i32`.
3. FAMILY 1 (EventMarkov): build the real `GeneratedSource` at
   `walk_start` and call `advance_parent()` in a loop, taking
   `parent_ts_ns`, `child_count` and `child_stride_ns` from each
   `ParentSummary`.
   KERNEL FAMILIES: build a `CadenceWalk` at `walk_start` and call
   `next()` in a loop, taking `parent_ts_ns` and `child_count` from each
   `ParentDraw` and `child_stride_ns()` from the walk.
   Either loop ENDS at the first parent whose `parent_ts_ns >= end`. That
   TERMINAL parent is a lookahead only: it closes the walk and is never
   projected, counted or included in any statistic.
   TERMINATION GUARD, family 1. `advance_parent()` cannot report failure.
   On an arrival refusal `begin_integrated_event` sets the source's
   internal fault and returns WITHOUT touching `self.burst`, so the next
   `advance_parent()` hands back a stale summary with an unchanged
   `parent_ts_ns` and the loop never advances. The family-1 loop
   therefore refuses the cell under A4 `Projection` the moment a returned
   `parent_ts_ns` is not STRICTLY greater than the previous one, and the
   same guard runs in the layer-1 oracle. Family 1 at shipped parameters
   cannot fault today; the guard exists so that a future fault is a
   refusal and not an eight-hour hang.
4. Per parent the projection mirrors `GeneratedAcc`'s OPEN-PARENT
   LIFECYCLE, which is not the same thing as "children then parent":
   a. for i in 0..child_count, the child timestamp is
      parent_ts_ns + i * child_stride_ns. If it lies in [start, end) and
      `session_segment_at(child_ts, offset)` yields a segment whose
      `session_start_ns` differs from the open accumulator's, then, IN
      THIS ORDER: close the currently open parent (writing it into the
      session it belongs to), close the session, open the new one. Then
      `push_print(child_ts, 0)`. The price is a CONSTANT because Stage A
      has no price path; every minute's trade range is then exactly zero
      and is marginalized out before any condition reads the histogram
      (3.5).
   b. a child in [start, end) whose segment lookup returns `None` does
      NOT rotate and does NOT refuse: it is pushed into the currently
      open session, exactly as `GeneratedAcc::push_trade` does, and
      `block1` refuses the minute at close time if it must. Stage A
      copies the shipped behavior here rather than being stricter than
      it, because a stricter projection cannot pass layer 1.
   c. after its children, the parent becomes the OPEN parent, carrying
      `first_ts = parent_ts_ns`. It is not written yet. It is written by
      `close_open_parent` at exactly one of three moments: the next
      session rotation (4a), the arrival of the next parent, or the end
      of the walk (5).
   d. `close_open_parent` writes the parent only if `first_ts` lies in
      [start, end): resolve its segment from `session_segment_at(first_ts,
      offset)` and call `push_parent(index, first_ts, 0, 0, false)`.
      `book_normal` is false because Stage A simulates no book, which
      leaves the quote-mid axis null - the axis Stage A is forbidden to
      evaluate.
   e. a parent whose segment lookup fails, or whose segment's
      `session_start_ns` differs from the accumulator it would close
      into, REFUSES the cell under A4 `Projection` rather than being
      dropped or misfiled. Both are the refusals `GeneratedAcc::
      close_open_parent` already raises, including its rotation-invariant
      check; 12b section 8 forbids dropping an inconvenient boundary cell
      anywhere in this spec.
5. On the loop's end, close the open parent, then the open session, and
   emit each session's record REDUCED to
   `{ session_date, block1_hist, block2 }`. The reduced record is named a
   ScreenSession and is never presented as a 12a session record: blocks
   3, 4 and 5 and the permutations are absent because Stage A cannot
   compute them.
6. realized_mean_gap_s = (last first_ts - first first_ts) /
   ((measured_parents - 1) * 1e9), where measured_parents counts exactly
   the parents satisfying `start <= parent_ts_ns < end`: warmup parents
   and the terminal lookahead parent of step 3 are excluded, and
   `SeedWalk.parents` is that same measured count and no other. Fewer
   than two measured parents refuses the cell under A4 `MeanGap` with
   `realized_s` recorded as NaN rather than yielding a division by zero.
   This is the quantity A4 compares against `mean_event_duration_s`.
```

IMPLEMENTATION DECISION, and the correction draft 1 most needed. Draft 1
said "push the parent AFTER its children", which is right WITHIN a session
and wrong ACROSS one. `GeneratedAcc::push_trade` rotates by calling
`close_open_parent` BEFORE `close_session`, and `close_open_parent`
refuses outright if the parent's segment does not match the open
accumulator. A burst straddling a session boundary would, under draft 1's
order, rotate first and then file a previous-session parent into the new
session - tripping that very invariant. The lifecycle above is the
accumulator's own, transcribed; the layer-1 oracle test proves it rather
than assuming it.

IMPLEMENTATION DECISION, the reduced close. `SessionAcc::close`
unconditionally computes block3 (the `WALL_HORIZONS_S` boundary series and
permutation cells) and block4 (the lag-1 moments and standardizers), all
of which the ScreenSession discards. At Stage A volumes that is pure cost
against a budget section 5.1 already calls tight. This item therefore adds
`SessionAcc::close_reduced(scope) -> LabResult<Value>` in `mogwai-lab`,
emitting `{ session_date, block1_hist, block2 }` from the same
accumulators `close` uses and computing nothing else. `close` is not
changed, so no landed 12a caller moves; a test pins that the two agree on
the three keys they share, so `close_reduced` cannot drift into a second
block-1 or block-2 implementation.

### 3.4 The conditions, lifted verbatim

A cell is admissible iff ALL of the following hold for EVERY seed in the
pass's seed set. Any failure refuses the cell with the failing condition,
seed and cell recorded. No pooling across seeds, no nearest-bin
substitution, no pseudocount rescue.

```text
A1 SUPPORT   -> Stage B gate B2
   (a) count-substitution support, 12a 5.2: for every hour h in all 24
       and every parent-count bin b with OBSERVED share above zero, the
       generated count in (h, b) is NONZERO.
   (b) conditional adequacy, 12a 5.2 rung 2c: for every hour h in
       FAIL_HOURS_300 = {19, 20, 23} and every REQUIRED bin b there
       (pooled OBSERVED populated-minute count at least
       MIN_MINUTES_CELL = 30), EVERY seed's generated count in (h, b) is
       at least MIN_MINUTES_CELL.
   Nothing stronger.

A2 MEAN-RATE PRESERVATION -> Stage B gate B6
   Per hour AND per seed, generated / observed MEAN parents per
   scheduled minute lies in MEAN_RATE_BAND = [0.98, 1.02].

A3 SUB-SECOND COMPOSITION -> Stage B gate B7
   Per hour AND per seed, the 1 s zero-count-fraction ratio lies in
   ZERO_COUNT_BAND = [0.8, 1.25].

A4 VALIDITY (not a screen predicate; an invalid run is not a candidate)
   No non-finite intensity, no step exceeding SELF_EXCITING_X_CEILING,
   no refused draw, and a realized mean gap within
   MEAN_GAP_REL_TOL_12B = 0.05 of the declared mean_event_duration_s.
```

Binding implementation rules that make the nesting exact rather than
merely intended:

- A1's two limbs are evaluated by the SAME code the landed `gate_b2`
  calls, including the POOLING step: `aggregate::monthly::
  pool_session_hists` to reach the pooled hist the substitution consumes,
  then `aggregate::countsub::{count_substitution, obs_shares_under,
  support_refusals_of}` for limb (a) and
  `aggregate::family::conditional_adequacy_bins` for limb (b), against an
  `ObsContext` built from the ScreenSession records. Naming the pooling
  explicitly is the point: it is the step a reimplementation would most
  naturally grow a second copy of. A1 is exactly what B2 will demand, so
  it must be the same predicate and not a reimplementation of it.

  RECORDED CONTRACT RISK, not amended here. 12b's B2 says the 5.2
  conditional adequacy guard "evaluates rather than refuses", while A1(b)
  demands every seed reach `MIN_MINUTES_CELL` in every required bin.
  Those are not literally the same predicate, so the "A1 is exactly what
  B2 will demand" claim inherits a nesting hole from the contract of
  record. A1(b) as written is the STRONGER reading, which is the wrong
  direction for a containment claim. This spec cannot fix it: changing
  either predicate changes a frozen gate, which is a section 17 stop
  against `notes/protocol-12b-arrival-composition-spec.md`. It is flagged
  for the owner as the one place where Stage A may be strictly stronger
  than the gate it claims to be contained by - the same class of defect
  the hour-set decision below fixes, but one this spec has no authority
  to close. Brick A therefore records, per cell that fails A1(b) alone,
  the pooled generated minute counts in every required bin, so the owner
  can see exactly which cells the hole would have cost.
- A2 and A3 use `arrival_control::hourly_mean_parents` and
  `arrival_control::hourly_zero_second_fraction`, over the hour index set
  `arrival_control::gate_hours(&profile)`.

  IMPLEMENTATION DECISION, and the most consequential one in this
  document. 12b 9.2 says "per hour"; the landed B6 and B7 judge the
  calendar-EXPOSED hour set, because MNQ's hour 21 UTC is the daily break
  and no session ever exposes it, so judging it would fail those gates by
  construction whatever the mechanism does. Stage A must use the same set:
  a screen that judged 24 hours would be STRICTLY STRONGER than the gate
  it claims to be contained by, and could reject a cell Stage B would
  accept. That is the exact defect revision 2 of the 12b document was
  refused for. This changes no constant and no band; it applies the landed
  reading of the identical gate.
- A4's arrival refusals come from the walk itself: `ArrivalRefusal` from
  `CadenceWalk::next` for the kernel families, and for family 1 from the
  shipped path, which cannot refuse, so its A4 reduces to the mean-gap and
  non-finite checks.
- A refused or inadmissible cell has `loss = None` and never enters any
  ordering.

### 3.5 The loss, which ranks and never selects

For an admissible cell, per seed and hour, `L_comp[s,h]` is the exact
1-Wasserstein distance between the empirical distributions of `log1p(N)`
over populated minutes, generated against observed, computed from the
sorted empirical CDFs with no binning: `arrival_screen::wasserstein_log1p`
over `parent_count_marginal` of both sides.

```text
L_comp[s] = sum over h of w[h] * L_comp[s,h]
w[h]      = observed populated minutes in h / total observed populated
            minutes
L_comp    = median over the pass's seeds of L_comp[s]
```

Tie-break, reported: the mean over hours and over `W in {1, 5, 60}` of the
absolute log ratio of the Fano factor.

The six-bin total variation distance, the 60 s Fano and the count-p99 log
ratios are computed and REPORTED for every cell and GATE NOTHING. Requiring
the arrival family's own 12a metrics to land inside the materiality band
would be 12b grading its own homework.

The seed median follows the landed convention of
`arrival_control::seed_median`: with an even seed count, average the two
middle readings; any `None` or non-finite reading makes the median `None`.
On the coarse pass the seed set has two members, so the median is their
mean; the refinement pass has four.

The landed helper cannot be CALLED as it stands: its signature is
`seed_median(readings: &[Option<f64>; 4])`, fixed arity four, and the
coarse pass has two seeds. IMPLEMENTATION DECISION: this item generalizes
it to `seed_median(readings: &[Option<f64>]) -> Option<f64>` in
`mogwai_lab::arrival_control` and updates its one caller
(`crates/mogwai-cli/src/arrival_control.rs`, which passes a four-element
array and needs only a slice coercion). The landed test
`the_seed_median_averages_the_two_middle_readings` is kept unchanged and a
two-element case is added to it. Stage A owns that change; it lands with
brick A and is behavior-preserving for arity four, which is what the
unchanged test says.

### 3.6 `mogwai-cli`: the `arrival-screen` subcommand

```rust
// crates/mogwai-cli/src/arrival_screen.rs, dispatched from main.rs as
// Command::ArrivalScreen, mirroring the landed ArrivalControl arm.

#[derive(Args)]
pub struct ArrivalScreenArgs {
    /// The committed protocol-12a artifact: the observed side, the
    /// exposure binding and the input hash.
    #[arg(long, value_name = "PATH")]
    pub measure: Option<PathBuf>,      // analysis/mnq-measure-12a.json
    /// Where to write the artifact.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,          // analysis/mnq-arrival-screen.json
    /// Brick A0: one cell per family, two seeds, measured against the
    /// family's OWN budget. Writes no artifact and runs no grid.
    #[arg(long)]
    pub cost_probe: bool,
    /// The walk cache root, defaulting to the standing storage policy.
    #[arg(long, value_name = "DIR")]
    pub cache: Option<PathBuf>,
}

pub fn run(args: ArrivalScreenArgs) -> anyhow::Result<Value>;
```

The full run requires a clean tree before reading any input and
re-attests it immediately before serializing, bailing "the artifact is
unbound" if HEAD moved or the tree went dirty, exactly as the landed
`arrival-control` driver does. `--cost-probe` does NOT require a clean
tree: it writes no committed artifact and its whole purpose is to be run
before the work that produces one.

## 4. The fidelity layers, as this item builds them

**Layer 1, the exact shipped-generator oracle. BLOCKING.**
`advance_parent()` on the real `GeneratedSource` at SHIPPED parameters
(`scalars.arrival` absent), projected per 3.3, must reproduce the
committed `generated.per_seed[*].blocks.block1` parent-count marginal and
the whole `block2` record of `analysis/mnq-measure-12a.json` EXACTLY, for
all eight committed seeds, under the exposure contract of section 8.

This validates the extraction and aggregation code, not the simulator.
Replaying the committed seeds here is an ORACLE test, not candidate
screening, and does not compromise the seed holdout: CONFIRMATION_SEEDS =
1..8 remain untouched by any fitting or decision.

Comparison rules, pinned so a failure is unambiguous: `block1` is
compared as `parent_count_marginal` of both sides, hour by hour and exact
count by exact count, with no tolerance; `block2` is compared as the whole
`monthly` pooled record, key by key, with integer equality on
`scheduled_windows`, `zero_windows`, `count_hist`, the run-length
histogram and the lag-1 sufficient moments. A mismatch is a blocking
defect in the projection, never a tolerance to widen.

POOLING, named because it is the one place an exact comparison can drift
silently. `generated.per_seed[*].blocks` in the committed artifact is
MONTHLY-POOLED - the generated side carries no `per_session` - while the
projection emits per-session ScreenSessions. The test therefore pools its
ScreenSessions with the SAME functions `aggregate::assemble` used to build
the committed record: `aggregate::monthly::pool_session_hists` for
`block1`, and `blocks_from_sessions`'s block-2 accumulation for `block2`.
No bespoke summation. If a pooling helper cannot be called on a reduced
session record, the fix is to widen that helper's input, not to write a
second pooler in the test.

Cost: eight full-generator month-scale walks. The test is `#[ignore]`d
and named in brick A's gate list so it is run deliberately, for the same
reason the socket-backed adapter tests are.

**Layer 2, exact parity.** For families 2, 3 and 4, Stage A and the Stage
B generator execute the same `next_parent`. This item makes that
structural rather than claimed by routing both through
`CadenceWalk::assemble` (3.1), and pins it with a new test: a `CadenceWalk`
and a `GeneratedSource` built from the same scalars, profile, calendar,
seed and start produce IDENTICAL `parent_ts_ns` and `child_count` for the
first 10,000 parents, per kernel family, under neutral exposure. Family 1
and Legacy use no kernel: both stages drive the real generator, so their
parity is not a claim about two implementations agreeing.

Brick K's committed transcripts and conformance vectors V1 to V8 are NOT
regenerated, re-derived or amended by this item. They continue to gate
through brick K's own tests.

**Layer 3, the distributional bridge.** Brick K landed the analytic
conformance tests
(`arrival_families_match_their_stationary_derivations` and siblings). This
item adds nothing to layer 3 and changes none of its bands.

## 5. Procedure and budget

- Each fitted parameter carries the domain, transform and grid of section
  16. Switch rates, correlation times and ratios are gridded
  LOGARITHMICALLY; occupancies and shares linearly.
- **Coarse pass**: the full tensor grid per family on `STAGE_A_SEEDS[0..2]`
  (seeds 201 and 202). Using fewer seeds is CONSERVATIVE: every condition
  is per-seed and failure-monotone, so a two-seed pass can only ADMIT a
  superset of what four seeds would admit. It cannot falsely reject.
- **Refinement**: subdivision at half spacing around the admissible
  region's boundary cells, on all four `STAGE_A_SEEDS`, to
  `REFINEMENT_DEPTH = 2` - two sequential rounds, per 5.0 - capped at
  `REFINEMENT_CELL_CAP = 600` cells per family and at
  `STAGE_A_GEN_REFINE_CAP = 40` for family 1, over both rounds together.
  Grid endpoints are evaluated and admissible endpoints reported as such.

### 5.0 The refinement algorithm, stated precisely

Draft 1 said "one subdivision pass" and `REFINEMENT_DEPTH = 2` in the same
breath and left six choices open, any of which two implementers would
resolve differently and so report different admissible regions. It is
pinned here in full. This changes no constant: `REFINEMENT_DEPTH` and
`REFINEMENT_CELL_CAP` keep their frozen values and only their MEANING is
fixed.

```text
LATTICE. Every axis keeps a level-indexed lattice. Level 0 is the coarse
  grid of 5.2. Level d halves the spacing of level d-1 in the axis's own
  transform: arithmetic on a linear axis, geometric on a log axis. A cell
  is the tuple of its per-axis lattice coordinates, so a level-2 point is
  representable exactly and coordinates are compared as lattice indices,
  never as f64 - two rounding paths to "the same" midpoint are the same
  cell by construction.

ROUNDS. REFINEMENT_DEPTH = 2 means TWO SEQUENTIAL evaluate-and-refine
  rounds, d = 1 then d = 2, not two levels generated at once from the
  coarse boundary. Round d refines the boundary of the admissible set
  KNOWN AFTER round d-1, which is the union of every cell evaluated at
  levels 0..d-1. Both rounds run on all four STAGE_A_SEEDS.

ADJACENCY on the irregular grid. Two evaluated cells are neighbours iff
  they differ on exactly one axis and no evaluated cell lies strictly
  between them on that axis (all other coordinates equal). This reduces
  to grid adjacency on the uniform coarse lattice and stays well defined
  once midpoints are inserted. Both region components and boundary
  detection use this one relation and no other.

BOUNDARY. A boundary cell is an admissible evaluated cell with, on some
  axis, either an inadmissible or refused neighbour, or no neighbour at
  all on that side because the axis ends there (an endpoint).

CANDIDATES. For each boundary cell and each such axis side, the candidate
  is the level-d midpoint between it and that neighbour. An endpoint side
  yields NO candidate: the grid is frozen and refinement never extends
  past its own bounds. Candidates already evaluated at any level are
  dropped. Candidates proposed by more than one boundary cell are
  DEDUPLICATED by lattice coordinate; the survivor's rank is the BEST
  (lowest) parent loss among its proposers, which is also the
  parent-loss attribution rule the cap uses.

STOPPING. An inadmissible midpoint does not stop refinement globally, but
  it is itself never a parent: round d+1 refines only cells admissible
  after round d. That is what makes the recursion terminate at depth 2
  with or without the cap.

CAP. REFINEMENT_CELL_CAP = 600 per family and STAGE_A_GEN_REFINE_CAP = 40
  for family 1 are budgets over BOTH rounds combined, not per round. When
  a round's deduplicated candidate set would exceed the family's remaining
  allowance, it is sorted by parent loss ascending, ties broken by
  canonical cell ordering (family, then each coordinate's lattice index in
  the declaration order of `Cell`), and truncated. The artifact records
  `refinement_candidates_unevaluated` per family, per round.
```

- Disconnected admissible regions are reported as multiple regions and
  every one advances. Nothing is smoothed or convex-hulled. Regions are
  the connected components of the admissible set under the adjacency
  relation defined above, computed once over all evaluated levels
  together.
- Caching follows `mogwai_lab::fit::walk`'s convention, keyed by the full
  parameter point (`Cell::key`), seed, exposure contract and kernel
  version, under the existing storage policy's provenance token. The cached
  value is the cell's per-seed `SeedWalk` products, not its verdicts, so a
  condition change would invalidate no cache entry it should not.

  IMPLEMENTATION DECISION, `kernel_version`. Draft 1 required it in both
  the cache key and the artifact binding block, and it exists neither in
  the 12b contract nor anywhere in the workspace. It is defined HERE, as
  an implementation decision over ground 12b leaves open, and NOT as a
  section 17 stop: 12b freezes no such constant, so defining one amends
  nothing.

  `mogwai_data::ARRIVAL_KERNEL_VERSION: u32 = 1`, a public constant beside
  the kernel in `crates/mogwai-data/src/generated/arrival.rs`, with a
  doc comment stating the rule that gives it meaning: it is the identity
  of the CADENCE DRAW, and any change to `ArrivalKernel::next_parent`, to
  a family's parameterization, to `CADENCE_STREAM_TAG` or to the cadence
  seed derivation MUST bump it. It is deliberately NOT
  `TAPE_PROTOCOL_VERSION`: that constant is the identity of a shipped tape
  and stays 11 through this whole item, while this one is the identity of
  a cache entry and of a Stage A artifact, which is exactly the "content
  hash or method version rather than overloading that constant" the
  workspace rule already prescribes. Brick K's committed transcripts pin
  version 1, so the first bump is owed by whoever first changes the draw.

### 5.1 The two-tier cost probe, brick A0

One cell per family, two seeds, measured, reporting wall time and peak RSS
against the family's OWN bound:

```text
STAGE_A_CELL_BUDGET_S     = 4.0    kernel families 2, 3 and 4
STAGE_A_GEN_CELL_BUDGET_S = 50.0   family 1, the real-generator path
STAGE_A_BUDGET_S          = 28800  (8 h), total
STAGE_A_RSS_BYTES         = 8 GiB
```

The probed cell per family is the coarse cell nearest the family's domain
centre, which is the same cell brick K's transcript fixtures name, so the
probe measures a point whose behavior is already pinned.

Above the bound, brick A FAILS and stops. The budget is never met by
trimming the grids silently.

WHAT A PER-CELL MISS ACTUALLY STOPS FOR. Draft 1 said a miss "stops for a
grid re-freeze", which does not follow: a PER-CELL budget is independent
of how many cells the grid holds, so re-freezing the grid cannot fix it.
The stop points are separated here, and neither is a licence to move a
band:

- A `STAGE_A_CELL_BUDGET_S` or `STAGE_A_GEN_CELL_BUDGET_S` miss stops for
  an OWNER RULING on the per-cell price, whose only honest remedies are to
  make the walk cheaper without changing what it computes, to shorten the
  measured window in the exposure contract (a 12b section 8 change, hence
  a section 17 stop), or to raise the per-cell constant (also a section 17
  stop). It is NOT a grid question.
- A `STAGE_A_BUDGET_S` miss - total wall time over the whole run - IS the
  grid question, and stops for a grid re-freeze, because total cost is
  cells times per-cell price and the grid is the only factor this item may
  touch. Even then the re-freeze is an owner decision on a frozen search
  space, never a silent trim.

HONEST PREDICTION, recorded so a miss is not read as a surprise and not as
grounds to move a constant. The measured window is roughly 2.67e6 seconds
at a declared mean gap of 0.0609 s, so a cell walks order 4e7 parents per
seed. `STAGE_A_CELL_BUDGET_S = 4.0` at two seeds allows about 50 ns per
parent for the kernel path, and `STAGE_A_GEN_CELL_BUDGET_S = 50.0` allows
about 600 ns per parent for family 1 against the 12a Brick M price of
roughly 25 s per full walk. Both are tight, family 1 exactly at its
measured price.

FAMILY 1 IS PREDICTED TO MISS, and the prediction is now sharper than
draft 1's. `STAGE_A_GEN_CELL_BUDGET_S = 50.0` covers TWO seeds, so the
allowance is 25 s per walk against a measured 25 s per walk - zero
headroom before the projection has run at all. The projection is not free:
it feeds order 4.4e7 parents and their children through `SessionAcc`. The
`close_reduced` decision of 3.3 is the one cost this spec can remove
honestly, deleting block 3, block 4 and the permutation cells from every
session close for output the ScreenSession discards anyway. Whether that
is enough is exactly what A0 measures, which is why A0 runs BEFORE the
grid and why its failure goes to the owner as a ruling on the per-cell
price rather than as a band change. Recording the prediction is not
permission to pass a miss.

### 5.1.1 Enforcing the total budget and the RSS ceiling

12b requires `STAGE_A_BUDGET_S` and `STAGE_A_RSS_BYTES` to be recorded AND
gated. Draft 1 recorded them in the artifact and defined failure only for
the per-cell probe, which would let a compliant implementation burn twelve
hours and 20 GiB and still print `arrival-admissible`. The full-run driver
enforces both:

```text
WALL TIME. The driver samples elapsed wall time at every cell boundary
  against STAGE_A_BUDGET_S = 28800 s. On the first boundary at or past the
  bound it stops the run, writes NO artifact, and exits non-zero with
  verdict `stage-a-budget-exceeded`. The partial cell record is written to
  the cache, so a re-freeze re-run does not repay for cells already
  evaluated.

PEAK RSS. `mogwai_lab::sampler::ResourceSampler` samples peak RSS on the
  same cadence in BOTH the probe and the full run, against
  STAGE_A_RSS_BYTES = 8 GiB. Crossing it stops the run identically, with
  verdict `stage-a-rss-exceeded`. In `--cost-probe` mode a crossing fails
  brick A0 with the same wording and the same non-zero exit.

SERIALIZATION. An over-budget run MAY NOT serialize
  `analysis/mnq-arrival-screen.json`, and therefore may not commit one:
  the verdict field can only ever carry `arrival-admissible: <families>`
  or `no-arrival-admissible-candidate-in-frozen-search-space`, and neither
  is reachable from a stopped run. The two budget verdicts are exit
  conditions of the DRIVER, printed on stderr, not artifact states.
  Existing budget readings up to the stop are printed with them.

VERIFICATION. `brokkr run mogwai -- arrival-screen --cost-probe` is what
  independently verifies the RSS path: it exercises the same sampler
  against the same ceiling, in a mode that needs no clean tree and writes
  nothing, and it is brick A0's gate. Two `mogwai-lab` tests pin the
  arithmetic without burning eight hours - one drives the enforcement with
  an injected clock past the bound, one with an injected RSS reading past
  the ceiling, and both assert no artifact was written.
```

These verdict strings are new NAMES for a stop this item owns; they are
not gate verdicts of 12b section 10.2 and do not enter the Stage B
vocabulary. Stage A failure remains
`no-arrival-admissible-candidate-in-frozen-search-space` and nothing
stronger.

### 5.2 The frozen search space, verbatim

```text
STAGE_A_SEEDS   = 201..204   (coarse pass 201..202)
MEAN_RATE_BAND  = [0.98, 1.02]
ZERO_COUNT_BAND = [0.8, 1.25]
MEAN_GAP_REL_TOL_12B    = 0.05
SELF_EXCITING_X_CEILING = 1e4
REFINEMENT_DEPTH        = 2
REFINEMENT_CELL_CAP     = 600 per family
STAGE_A_GEN_REFINE_CAP  = 40  (family 1 refinement cells)

Family 1, event-time two-state Markov renewal  (1 fitted: w)
  q   quiet share   HELD at the shipped 0.35
  r   rate ratio    HELD at the shipped 150
  w   switch rate   log3(1e-6, 0.5)                         -> 19
  cells 19, the shipped point (0.35, 0.10, 150) being the w = 0.10 cell
  and counted once.

Family 2, wall-time two-state MMPP  (3 fitted)
  q   occupancy     linear(0.10, 0.60, 0.10)                ->  6
  r   rate ratio    log3(2, 200)                            ->  7
  tau seconds       log3(1, 3600)                           -> 12
  cells 504

Family 3, log-OU Cox  (2 fitted)
  sigma_Y           linear(0.2, 2.0, 0.2)                   -> 10
  tau seconds       log3(1, 3600)                           -> 12
  cells 120

Family 4, discrete self-exciting  (2 fitted)
  phi               linear(0.10, 0.85, 0.05)                -> 16
  tau seconds       log3(2, 600)                            ->  9
  cells 144
```

`linear_grid` and `log_grid` already implement section 16's generation
rule and their point counts are pinned by landed tests.

## 6. The artifact

`analysis/mnq-arrival-screen.json`, committed, written atomically through
a `.tmp` rename:

```text
{
  binding: { harness_tree_commit, clean_tree, schema_version,
             input_hashes: { "analysis/mnq-measure-12a.json": <sha256> },
             exposure: { instrument: "MNQ",
                         preset: "crates/mogwai-server/presets/mnq.toml",
                         window_start_ns, window_length_ns, warmup,
                         divergence: null, regime: "neutral" },
             stage_a_seeds, coarse_seeds, gate_hours, unexposed_hours,
             tape_protocol_version: 11,
             kernel_version, spec: "notes/protocol-12b-arrival-composition-spec.md
                                    section 9, bricks A0 and A" },
  search_space: { <every family's grid, in full, with its point counts> },
  cells: [ { family, params, lattice, level: 0 | 1 | 2,
             pass: "coarse" | "refine",
             a1: {passed, per_seed: [...], failing_cells: [...],
                  required_bin_counts: {...}},
             a2: {passed, per_seed_hour: [...]},
             a3: {passed, per_seed_hour: [...]},
             a4: {passed, refusal: null | {...}},
             admissible, loss, reported: { tv_six_bin, fano_60_log_ratio,
             count_p99_60_log_ratio, fano_tiebreak }, cost_s } ],
  admissible_region: { <family>: { regions: [[cell, ...], ...],
                                   cells: [cell, ...],
                                   endpoints_admissible: [cell, ...],
                                   refinement_candidates_unevaluated:
                                     { round_1: n, round_2: n } } },
  refusals: [ RefusalRec ],
  cost: { coarse_s, refine_s, total_s, peak_rss_bytes,
          per_family_cell_s: {...},
          budgets: { STAGE_A_CELL_BUDGET_S, STAGE_A_GEN_CELL_BUDGET_S,
                     STAGE_A_BUDGET_S, STAGE_A_RSS_BYTES } },
  verdict: "arrival-admissible: <families>"
           | "no-arrival-admissible-candidate-in-frozen-search-space"
}
```

Every evaluated cell appears, admissible or not, with its per-condition
per-seed verdicts, its loss, its reported diagnostics and its refusals. A
family refusing across a whole region reads as that rather than as an
empty admissible set.

Three spellings are pinned so a reader can cross-check the artifact
mechanically. `params` uses the SEAM names of 3.2 (`switch_rate`,
`occupancy`, `rate_ratio`, `sigma_y`, `phi`, `tau_s`), never 12b section
16's shorthands, so a cell can be pasted into a preset's
`[instrument.generator.arrival]` table unchanged. `lattice` carries the
integer per-axis coordinates of 5.0, which is what makes two refinement
rounds comparable without f64 equality. `kernel_version` is
`mogwai_data::ARRIVAL_KERNEL_VERSION`, defined in 5.
`fitted_params` rides along per cell as a Stage B input (3.2).

The cost probe prints its readings and writes no artifact. An over-budget
full run writes no artifact either (5.1.1).

## 7. Bricks

The suite is green at every boundary. Both bricks are additive and
independently revertible; neither changes a tape byte.

### Brick A-1: the cadence-walk constructor

`CadenceWalk`, `CadenceParts` and `ARRIVAL_KERNEL_VERSION` in
`mogwai-data`, with
`GeneratedSource::try_new_with_session_profile` rebuilt on
`CadenceWalk::assemble`, and the layer-2 parity test. Landed first because
Stage A cannot run a kernel family without it, and landed as its own unit
because it touches the generator's constructor and must be provable
byte-neutral on its own.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-data a_cadence_walk_and_the_generator_agree_parent_for_parent
brokkr test -p mogwai-data arrival_transcripts_replay_bit_exact
brokkr test -p mogwai-data the_integrated_families_never_snap_a_closed_window_timestamp
brokkr test -p mogwai-data the_event_markov_family_at_the_shipped_point_is_byte_identical_to_legacy
```

The last three are brick K's landed gates, re-run unchanged: they are what
proves the constructor refactor moved nothing. The first is new and
asserts, per kernel family, identical `parent_ts_ns` and `child_count` over
the first 10,000 parents.

### Brick A0: the Stage A cost probe

One cell per family, two seeds, measured. Reports wall time and peak RSS
against the family's OWN bound: `STAGE_A_GEN_CELL_BUDGET_S` (50 s) for
family 1, which drives the real generator, and `STAGE_A_CELL_BUDGET_S`
(4 s) for the kernel families 2 to 4. A single bound would fail family 1
by construction. A miss FAILS brick A and stops for a grid re-freeze.

A0 is a MODE of brick A's binary, so the code lands with brick A and this
gate is an orchestrator run against it.

```text
brokkr run mogwai -- arrival-screen --cost-probe
```

### Brick A: the Stage A screen

`mogwai_lab::arrival_screen`'s driver plus `mogwai arrival-screen`, the
fidelity layers, the admissibility conditions, the loss and the artifact.
Corpus-free; runs on any clone.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-lab arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks
brokkr run mogwai -- arrival-screen --out analysis/mnq-arrival-screen.json
```

The layer-1 test is blocking: it must reproduce the committed artifact's
generated `block1` parent-count marginal and whole `block2` record for all
eight committed seeds, exactly. It is `#[ignore]`d for cost and named
here so it is run deliberately.

Supporting tests, each pinning a property no gate above reaches, all run
by `brokkr check --gate`:

```text
mogwai-lab
  the_screen_projection_places_a_straddling_burst_in_two_minutes
      a burst crossing a minute boundary contributes its parent to the
      first minute and an N = 0 populated minute to the second (2.5).
  a_burst_straddling_a_session_boundary_files_its_parent_in_the_old_session
      the open-parent lifecycle of 3.3 step 4: rotation closes the parent
      BEFORE closing the session, so a parent whose children cross into
      the next session is still written into the session containing its
      first child. Draft 1's order would have tripped
      `close_open_parent`'s rotation invariant here.
  close_reduced_agrees_with_close_on_block1_and_block2
      the reduced session close emits byte-equal `block1_hist` and
      `block2` to `SessionAcc::close` on the same input, so dropping
      blocks 3 and 4 buys cost and nothing else.
  a_child_with_no_segment_is_pushed_not_refused
      3.3 step 4b: the projection copies `GeneratedAcc::push_trade`
      rather than being stricter than it.
  a_family_one_walk_that_stalls_refuses_instead_of_looping
      a `GeneratedSource` whose `advance_parent` repeats a
      `parent_ts_ns` refuses the cell under A4 `Projection` (3.3 step 3).
  the_mean_gap_counts_measured_parents_only
      warmup parents and the terminal lookahead parent are excluded, and
      fewer than two measured parents refuses under A4 `MeanGap`.
  the_refinement_is_two_rounds_over_a_lattice
      round 2 refines only what round 1 admitted, midpoints are lattice
      points compared as integers, endpoint sides yield no candidate,
      and duplicates take their best proposer's loss (5.0).
  the_total_budget_and_the_rss_ceiling_stop_the_run_without_an_artifact
      two cases, injected clock and injected RSS reading, each asserting
      the named verdict and that no file was written (5.1.1).
  a1_is_the_conjunction_of_the_two_frozen_12a_rules
      a generated side with a hole at an observed-positive bin fails limb
      (a); a side with 29 minutes in a required FAIL_HOURS_300 bin fails
      limb (b); 30 passes. Nothing stronger is demanded.
  the_screen_judges_the_same_hour_set_as_gates_b6_and_b7
      A2 and A3 evaluate `gate_hours` and never hour 21, so the screen
      cannot reject a cell Stage B would accept.
  an_arrival_refusal_records_the_cell_and_keeps_it_out_of_the_loss
      a cell whose walk refuses is inadmissible under A4 with the variant
      and clock recorded, and carries `loss: null`.
  a_projection_gap_refuses_rather_than_dropping_a_boundary_minute
      a parent that maps to no open segment refuses the cell.
  the_refinement_subdivides_boundary_cells_in_each_axis_transform
      geometric midpoints on log axes, arithmetic on linear ones, capped
      in loss order with the truncation recorded.
  disconnected_admissible_regions_are_reported_separately
      two non-adjacent admissible blocks yield two regions, unsmoothed.
  the_coarse_pass_admits_a_superset_of_the_four_seed_pass
      the failure-monotone argument, exercised rather than asserted.
mogwai-cli
  arrival_screen_refuses_a_dirty_tree_before_reading_inputs
  arrival_screen_cost_probe_needs_no_clean_tree_and_writes_no_artifact
  the_screen_artifact_carries_every_evaluated_cell_and_its_verdict
      run against the committed artifact once it exists; before then the
      test returns, exactly as the landed control's B8-absence pin does.
```

## 8. Keep/revert

Brick A-1 is kept or reverted on brick K's own gates plus its parity test:
it changes a constructor and must be byte-neutral, and the three re-run
brick K gates are what says so. Bricks A0 and A are additive, produce no
generator change, and are kept or reverted on the gates above. Nothing
here is a probe, an env-var switch or a temporary routing knob: the
`--cost-probe` flag is a declared mode of a shipped subcommand with its own
test, not scaffolding.

If brick A's grid run closes with
`no-arrival-admissible-candidate-in-frozen-search-space`, the CODE is
still kept - the screen is the instrument that produced the verdict - and
the landing closes for an owner ruling. A verdict is a result, not a
failure of the brick that measured it.

## 9. Stopping rule

The teardown stops at the generator's constructor. `CadenceWalk` reuses the
existing assembly and adds no behavior; `begin_event`, `begin_integrated_event`,
`step_child`, the kernel, the transcripts and the conformance vectors are
untouched. No preset declares the arrival seam at the end of this item, so
every committed instrument still takes the shipped path and
`TAPE_PROTOCOL_VERSION` stays 11.

If implementation proves a frozen constant, family, gate or statistic
unmeasurable, that brick FAILS and stops. The amendment goes through
review, dated, in `notes/protocol-12b-arrival-composition-spec.md`, before
implementation resumes. No artifact may be produced under a partially
amended contract.

The teardown also stops at `SessionAcc::close`, which is not modified:
`close_reduced` is an addition beside it (3.3), and `seed_median`'s
generalization (3.5) is behavior-preserving at arity four. Nothing landed
by 12a changes shape.

## 10. Review disposition

Draft 1 was reviewed twice, by an Opus session and a codex session
working independently from the same unprimed prompt. The reports
themselves are consumed and deleted once folded in - this table is what
survives them. Every finding of both is folded in above; this section
records where each landed, and the three that were NOT adopted as
written, with reasons.

### 10.1 Adopted

| Finding | Where it landed |
|---|---|
| Projection misfiles a parent across a session boundary | 3.3 step 4, rewritten as `GeneratedAcc`'s open-parent lifecycle, plus a new test |
| `SessionAcc::close` computes block 3 and block 4 the ScreenSession discards | 3.3, the `close_reduced` decision, and 5.1's sharpened family-1 prediction |
| A per-cell budget miss is not fixed by a grid re-freeze | 1.1 and 5.1, the two stop points separated |
| `STAGE_A_BUDGET_S` and `STAGE_A_RSS_BYTES` recorded but never enforced | 5.1.1, new |
| `kernel_version` names nothing that exists | 5, `ARRIVAL_KERNEL_VERSION` defined as an implementation decision |
| `seed_median` has fixed arity four and cannot be called at two seeds | 3.5, generalized to a slice with its one caller updated |
| A1 limb (a) omits `pool_session_hists` | 3.4 |
| A family-1 arrival refusal loops forever on a stale burst | 3.3 step 3, the termination guard |
| A child mapping to no open segment is unspecified | 3.3 step 4b |
| Layer 1 never names its pooling | 4, layer 1 |
| Refinement depth 2 is not deterministically implementable | 5.0, new: lattice, rounds, adjacency, boundary, dedup, cap |
| The mean-gap population is underspecified | 3.3 step 6 |
| `fitted_params` reads as dead API | 3.2, documented as the Stage B tie-break input |
| `Cell` coordinate names diverge from the seam's | 3.2 and 6, seam names pinned in code and artifact |
| `utc_offset_minutes` is `i16`, `session_segment_at` takes `i32` | 3.3 step 2 |
| "The five frozen families" over a four-variant enum | 3.2 |
| B2's "evaluates rather than refuses" versus A1(b)'s per-seed 30 | 3.4, recorded as a contract risk for the owner |

### 10.2 Not adopted as written

- **"Parent pushed after its children reproduces `GeneratedAcc`" was
  VALIDATED by one review and REFUTED by the other.** The refutation is
  right and the validation is right about the wrong scope. Within a
  session the parent does close after its children, which is what the
  first review checked; across a session boundary `push_trade` calls
  `close_open_parent` before `close_session`, so draft 1's order files a
  previous-session parent into the new accumulator and trips
  `close_open_parent`'s own rotation refusal. Section 3.3 now specifies
  the lifecycle rather than the ordering, which subsumes both readings.
- **The B2 nesting hole is not fixed here.** It is a real defect and it is
  recorded in 3.4, but both candidate fixes change a frozen Stage B gate
  predicate, which is a section 17 stop against the contract of record.
  This spec has no authority to close it, so it is escalated with the
  evidence a ruling needs rather than silently resolved.
- **The suggestion of a shared reduced accumulator EXTRACTED from
  `GeneratedAcc` is declined in favour of transcribing its lifecycle.**
  Extraction would refactor landed 12a measurement code that the committed
  artifact was produced by, which is a far larger blast radius than this
  item's stopping rule allows, and the layer-1 oracle already makes a
  divergent transcription a blocking failure rather than a silent one. If
  layer 1 fails on a lifecycle detail this spec got wrong, extraction is
  the right second move - and that decision then belongs to the owner,
  not to the implementer mid-brick.
