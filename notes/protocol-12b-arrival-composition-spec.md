# Protocol 12b: the arrival-composition repair

REVISION 10, drafted 2026-08-08. FROZEN BY OWNER DECISION, not by
signature - see section 19. The freeze protocol of
`notes/protocol-12a-measurement-spec.md` section 9 (Brick F) applies in
full: this document is frozen only when a reviewing codex session signs
it and that sign-off is recorded here. Until then no brick below may be
implemented.

Written against `reference/technical-implementation-spec.md`. Spawned
from `notes/protocol-12a-measurement-spec.md` section 11 (the RESULT
record and the owner ruling it carries), whose section 1.2, section 8
and frozen constants bind this document.

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it.

Revision 1 was REFUSED with nine blockers by codex session 019fe29f.
Two were outright errors of mine: the wall-time MMPP's grid transition
did not have the stationary law its normalization assumed, and the
Hawkes recursion was not mean-preserving under a time-varying baseline
at all. Both were repaired by derivation rather than by tightening a
tolerance, and the MMPP repair was confirmed correct at the next
review.

Revision 2 was REFUSED with eight blockers by codex session 019fe2a7.
Three of its claims were false as written and are the ones to check
hardest here: the shared-kernel claim (the kernel did not own the
calendar snapping that happens after a gap resolves), the
necessary-screen claim (A1 was strictly stronger than the frozen 12a
support rule it claimed to be contained by, so it could reject a cell
Stage B would accept), and the holdout claim (the negative control both
fitted and judged itself on the confirmation seeds). The full
disposition of all rounds is section 18.

Revision 3 was REFUSED with six blockers by codex session 019fe2af.
The deepest was that the shared-kernel claim was STILL false after two
attempts to repair it: the generator carries four runtime
transformations around the gap, and the next gap opens from the last
CHILD's timestamp, so an arrival-only simulation is not arrival-only at
all. Revision 4 scoped the claim instead of widening the surface again.

Revision 4 was REFUSED with four blocking contradictions and one
verification gap by codex session 019fe2b6. The sharpest was an
off-by-one that would have desynchronized every Stage A gap: the child
burst ends at `(count - 1)` strides past the parent, not `count`,
because `step_child` gives the FIRST child the parent's own timestamp.

Design rulings that preceded the draft, from codex sessions 019fe28a
(shape) and 019fe28e (stage boundary), are written into the body rather
than appended.

---

## 0. Work items for the orchestration loop

ADDED 2026-08-09, after bricks F, K and B4 landed at `f75156f`. This
section is an INDEX, not an amendment: it changes no constant, gate,
family, verdict or brick, and partitions the REMAINING bricks into
three work items for the spec-loop. Each item is the X a step-1 spec
writer reads; the sections it names are that item's binding contract.
A completed item is removed from this index; the sections it pointed
at stay, because later items and the artifacts' binding blocks still
cite them.

Standing constraints on every sub-spec written from an item:

- This document remains the contract of record. A sub-spec LIFTS its
  constants, seed sets, gate definitions and gate commands VERBATIM
  from the sections its item names - it never paraphrases them, never
  tightens or loosens them, and never sends the implementer back here
  to cross-reference mid-build.
- A sub-spec may not amend frozen content. If writing it exposes a
  contradiction or an unmeasurable requirement, that is a section 17
  stop: the brick fails and the amendment goes through review, dated,
  in THIS document, before implementation resumes.
- The stop points below are owner decisions. A sub-spec states its
  item's stop point and the implementer halts there; no sub-spec may
  define work past its own item's verdict.

State when this index was added: bricks F (freeze), K (kernel,
dormant) and B4 (the two-sided bound instrument) are DONE. The Stage A
screen core (`mogwai_lab::arrival_screen`: observed projections, grid
rules, loss) exists without its driver or CLI. `TAPE_PROTOCOL_VERSION`
is 11. Not yet run: the B4 artifact command (an orchestrator run
against landed code, precondition of item 1, no spec needed).

UPDATE 2026-08-09: brick N is DONE and item 1 is removed above. The
control ran on `CONTROL_TEST_SEEDS` against gates B1 to B7 and returned
`negative-control-failed`: B1 and B5 pass, B2, B3, B4, B6 and B7 fail.
The landing therefore does NOT stop, and the loop proceeds to item 2 as
5.5 provides for. The evidence is `analysis/mnq-arrival-control.json`,
bound to commit `671d193`.

This is the outcome 5.5 predicted, and the prediction's REASONING is
confirmed rather than merely its verdict: the hourly means were never
what was wrong, so re-centring them cannot buy the within-hour mixture
the observed tape carries. The measured `normalizer_drift` is
1.0026855, so the corrected curve moves the overall level by about a
quarter of a percent - the correction really is a reshaping of the hour
axis and not a level change in disguise. The premise that a new
stochastic shape is required survives.

### Item 2: bricks A0 and A, the Stage A screen

The screen driver, the `arrival-screen` CLI with its `--cost-probe`
mode, the fidelity layers, and `analysis/mnq-arrival-screen.json`.
One item because A0 is a mode of A's binary and its whole job is to
fail brick A before the grid runs.

- Binding sections: 9 (projections, admissibility A1 to A4, loss,
  output), 6 (fidelity layers; the layer-1 oracle test is BLOCKING),
  3 (the stage boundary and what Stage A may not evaluate), 12
  (procedure, budgets, the two-tier cost probe), 16 (grids and
  constants), 7 and 8, bricks A0 and A (gate commands), 13.
- Stage A is corpus-free and consumes neither the corpus nor the B4
  artifact (brick B4 amendment 2).
- Stop points: a cost-probe miss fails brick A and stops for a grid
  re-freeze; an empty admissible region closes the landing with
  `no-arrival-admissible-candidate-in-frozen-search-space` and goes
  to the owner.

### Item 3: bricks S0 and S, the Stage B landing

`arrival-solve` with its `--cost-probe` mode, the seam declaration in
`presets/mnq.toml`, `presets emit --omit`, the legacy byte-identity
procedure, confirmation, `TAPE_PROTOCOL_VERSION = 12`, and
`analysis/mnq-arrival-selection.json`.

- Binding sections: 10 (gates, ordered selection, confirmation), 9.4
  (grid sensitivity), 12 (Stage B budgets and the cap's consequence),
  11 (anti-gaming), 15 (brick S is kept or reverted WHOLE), 16, 7 and
  8, bricks S0 and S (procedures and exact commands), 13, and 1.2
  (the inherited hard gates).
- Stop points: every closing verdict of 1.1 that Stage B can reach,
  each an owner report; `confirmation-failed` reverts the whole
  brick.
- Brick R follows a `mechanism-landed` close and is orchestrator runs
  against existing code (`mogwai measure`, `parity12a_i`), not an
  item.

## 1. The goal

Repair the generated parent-count COMPOSITION until the frozen
protocol-12a arrival counterfactual has support, then re-run the
UNAMENDED 12a ladder and record what it measures.

The success criterion is deliberately not "the arrival rung fires".
Reading it that way inverts the mechanism: a repaired arrival process
moves the arrival family's metrics toward the materiality band, which
is the state in which rung 2a cannot fire. 12b removes an UNMEASURABLE
state; it does not manufacture an eligible one. Whatever the re-run
ladder then says is a measurement, and it goes to the owner as one.

This is also why no gate in this spec requires the arrival metrics to
land inside the band. Section 9.2 records that argument where it bites.

### 1.1 Proceed/close threshold

The landing closes with exactly one of:

- `mechanism-landed` - a candidate family and parameter point passes
  every hard gate of section 10, the generator change lands
  instrument-resolved with `TAPE_PROTOCOL_VERSION` at 12, and the
  unamended 12a ladder re-runs against the repaired tape. The ladder
  verdict is recorded, not required to be any particular value.
- `negative-control-passed` - the deterministic hourly re-centring of
  section 5.5 clears gates B1 to B7 (B8 being inapplicable to a
  candidate with no cadence grid). The landing STOPS for an owner
  ruling: the premise that a new stochastic shape is
  required has been falsified.
- `no-arrival-admissible-candidate-in-frozen-search-space` - Stage A
  admits no family-region pair IN THE FROZEN GRID. No generator change
  lands. This is never stated as a claim about the continuous parameter
  space; this spec supplies no enclosure argument and will not pretend
  to one.
- `no-feasible-cell-among-evaluated-cells` - Stage A admits survivors,
  Stage B evaluates as many as `STAGE_B_CELL_CAP` allows per family in
  Stage A loss order, and none clears the full-generator constraint
  set. This is the outcome an UNCAPPED search cannot be claimed to
  have reached, and revision 3 wrongly called it
  `no-jointly-feasible-mechanism` in every binding section while
  admitting the cap only in prose. The artifact records exactly which
  admissible cells went unevaluated.
- `no-jointly-feasible-mechanism` - reserved for the case where every
  admissible cell of every survivor was actually evaluated and none
  cleared. Only reachable when no family's admissible region exceeds
  the cap.
- `confirmation-failed` - a selected cell cleared every gate on the
  search seeds and failed on `CONFIRMATION_SEEDS`. What this
  establishes is narrow and is stated as such: the SELECTED cell is
  disproved on the holdout. It says nothing about the other capped
  cells and nothing about the unevaluated ones. No generator change
  lands and the frozen run closes; it does not return to tuning.
- `not-identified` - two or more candidates pass everything and the
  frozen tie-break of 10.3 does not separate them.

There is no "best of the failures" outcome. A confirmation failure
closes the frozen run rather than returning to tuning.

### 1.2 Inherited obligations, restated as hard gates

1. `notes/protocol-12a-measurement-spec.md` is the contract. Its
   ladder, bins, floors, estimators and refusal semantics re-run
   UNAMENDED. If a 12a definition proves inconvenient here, this spec
   fails; the ladder is the judge and 12b is the defendant.
2. The hourly 60 s and 300 s wall-time bands `[0.8, 1.25]` on the
   protocol-11 `robust_scale` estimator are HARD gates (the Brick V
   amendment, 12a section 1.2). Not relaxable after a miss.
3. The standing instrument-resolution decision (12a section 8): MNQ
   receives an instrument-resolved override, the legacy branch is
   preserved BYTE for BYTE with no re-bless of the crypto tapes, and a
   mechanism that cannot preserve the legacy branch exactly is
   ineligible.
4. `TAPE_PROTOCOL_VERSION` moves 11 to 12 at the landing that changes
   the generator, and at no earlier commit.

12a section 1.2 also inherits a two-sided minute-range body gate here:
the p99 minute-range statistic gets a LOWER acceptance bound from the
lower tail of the same resampled envelope machinery, so an over-damped
model cannot pass by making every minute too small. That instrument is
built as brick B4, before the gate that consumes it.

## 2. Survey of the ground

### 2.1 The defect, measured

From `analysis/mnq-measure-12a.json` (binding commit `1e9506c`, corpus
job GLBX-20260805-HAPEWPABKG, 22 usable observed sessions, 8 generated
seeds), parent-count bin shares of populated minutes, observed monthly
against generated seed 1, at the three `FAIL_HOURS_300`:

```text
hour 19  observed  257-1024 0.273  1025-4096 0.699  4097+ 0.028
         generated 257-1024 0.001  1025-4096 0.999
hour 20  observed  1-64 0.019  65-256 0.513  257-1024 0.364
                   1025-4096 0.100  4097+ 0.004
         generated 65-256 0.018  257-1024 0.981  1025-4096 0.001
hour 23  observed  65-256 0.371  257-1024 0.601  1025-4096 0.029
         generated 65-256 0.140  257-1024 0.859  1025-4096 0.001
```

Seeds 1 and 2 produce IDENTICAL bin counts at hours 19 and 20: the
seed-to-seed variation does not reach a bin edge. The generated
per-minute parent count is effectively degenerate around a
deterministic hourly rate.

`fano_60` log ratios are -2.325, -3.067, -1.219 at hours 19, 20, 23 and
`count_p99_60` log ratios -0.639, -1.326, -0.430, each with 8-of-8 seed
agreement. The count substitution refused 22 of 24 hours on
observed-support-without-generated-support; the arrival family's
`inventory_complete` is false with a null critical value.

### 2.2 The mechanism as built

`crates/mogwai-data/src/generated/source.rs` and `consts.rs`. Per
parent event: a Weibull innovation at shape 1.0 (exponential)
normalized by `ARRIVAL_WEIBULL_MEAN`; a two-state mean selection
(`active_mean_s`, or `quiet_mean_s = active_mean_s *
ARRIVAL_QUIET_ACTIVE_RATIO`) solved from
`mean_event_duration_s * ARRIVAL_MEAN_CAL` so the UNCONDITIONAL mean
gap is preserved; a state flip drawn AFTER the gap, per EVENT, at
switch rate `1 - ARRIVAL_STATE_PERSISTENCE = 0.10`; division by the
deterministic piecewise session multiplier and the regime thin factor
on the open-market path, or the hour-by-hour budget integration of
`low_intensity_gap_ns` when the gap opens below
`LOW_INTENSITY_ARR_MULT`; and a child count from `SweepShape` under
state-conditioned multipliers pinned to preserve the declared
`children_mean` exactly.

The time-scale coupling is structural. The chain is indexed by EVENT,
so at the several hundred to several thousand parents per minute the
MNQ curve carries in the cash hours, a switch rate of 0.10 gives a
state-run correlation length of about ten parents and one minute
averages over many effectively independent runs. What survives into the
minute count is the deterministic hourly rate plus Poisson noise, which
is Fano near 1 by construction.

It is NOT established that no per-event chain can produce minute-scale
over-dispersion: persistence can be moved arbitrarily close to one,
lengthening state runs in event time without disturbing the stationary
one-gap mixture. That is a measurement, and family 1 makes it.

### 2.3 What already exists, and is NOT built here

- `GeneratedSource::advance_parent()` returns
  `ParentSummary { parent_ts_ns, child_count, child_stride_ns }` while
  running every state transition and random draw of the real walk,
  skipping only protocol-object construction. Its compact path is
  already pinned against the wire path by the tick-composition tests.
  This is the exact shipped-generator arrival oracle, and it is what
  fidelity layer 1 uses.
- The observed side of every Stage A target is ALREADY COMMITTED in
  `analysis/mnq-measure-12a.json`: `observed.monthly.block1.hist` is
  the exact sparse joint histogram keyed on exact `N`, 29,971 rows
  across all 23 traded hours, and `observed.monthly.block2` carries,
  per hour and per window in `COUNT_WINDOWS_S = {1, 5, 60}`, the exact
  count histogram, run-length histogram and lag-1 sufficient moments.
  Stage A therefore needs NO corpus pass and no delivered TBBO data on
  disk, and its gate runs on any clone.
- `mogwai_lab::measure12a` computes both sides of Blocks 1 to 5;
  `mogwai_lab::fit` carries the protocol-11 walk cache and its
  deterministic grid, endpoint-reuse and tie-break conventions;
  `mogwai_lab::kernel` carries `splitmix64`, nearest-rank quantiles,
  `py_sum`, `py_fsum` and the typed-canonical comparator.
- `GeneratorScalars` already carries three defaulted nested seams
  (`quoted_width`, `top_sizes`, `trade_displacement_ticks`) and a
  defaulted scalar (`size_log_sigma`) whose documented contract is
  exactly what 12a section 8 demands: an instrument that omits the
  field draws the shared shape byte for byte. The protocol-12 arrival
  seam takes that shape.
- An existing realism gate already bounds the one-second-scale count
  behavior of the shipped tape: `zero_second_frac` against the
  fingerprint's `density.zero_frac` within 0.05 absolute
  (`crates/mogwai-data/src/generated/tests.rs`). Section 10.2's B7 is
  its sibling, not a new invention.

### 2.4 What `fit::solve` does and does not supply

`mogwai_lab::fit::solve` supplies deterministic grids, cache keying,
endpoint reuse and tie-breaking, and its invariants are unit-tested.
Its solver itself is ONE-DIMENSIONAL scalar trisection. It is not a
multidimensional search and cannot cross the discontinuous support gate
of section 9.2. Section 12 freezes its own procedure and reuses only
`solve`'s caching and tie-breaking conventions. Claiming the
protocol-11 solver applies here unchanged would be a survey error of
the kind `reference/technical-implementation-spec.md` item 8 warns
about.

### 2.5 The public API change this spec requires

`splitmix64` in `crates/mogwai-protocol/src/seeds.rs` is a PRIVATE
`const fn` today. Section 7's seed derivation needs it from
`mogwai-data`, so brick K makes it `pub` and lands a stable-vector test
pinning its output against the values `mogwai_lab::kernel`'s copy
already asserts. Naming another crate's private function and hoping is
not a specification.

## 3. The two stages and the boundary

> Stage A is a corpus-free, no-running-generator-change screen of
> NECESSARY conditions. It advances every admissible family-region pair
> and selects none. Stage B evaluates every survivor through the real
> generator, in Stage A loss order and up to a frozen per-family cell
> cap, and performs the only family selection, under simultaneous hard
> gates.

| Stage | Authority | Output |
|---|---|---|
| A | Parent timestamps and cadence only | Every family-region pair satisfying the necessary conditions |
| B | The complete real generator | The single jointly feasible family and parameter point, or a no-feasible verdict |

Binding consequences:

- Stage A may NOT evaluate the whole Block 1 histogram: it also carries
  `trade_range_ticks` and `quote_range_half_ticks`, which need the
  price and book paths. Stage A sees the parent-count marginal and
  nothing else from Block 1. The count substitution and its conditional
  range-law guard are Stage B statistics.
- Stage A may NOT discard an admissible family because another scores
  better. Loss ranks evaluation ORDER only. Every family with a
  nonempty admissible region advances. If exactly one survives it is
  named the sole arrival-admissible survivor, never the selected
  mechanism.
- **Every Stage A condition is also a Stage B hard gate.** This is the
  nesting property revision 1 got wrong: a screen whose predicates are
  not implied by the landing contract can reject a jointly feasible
  mechanism. Section 9.2 admits a cell only on conditions that reappear
  verbatim in section 10.2, and the mapping is stated in both places.
- Stage A failure is named
  `no-arrival-admissible-candidate-in-frozen-search-space`, never the
  stronger verdicts, which only Stage B reaches - and of those, only an
  UNCAPPED Stage B can reach `no-jointly-feasible-mechanism`; a capped
  one reaches `no-feasible-cell-among-evaluated-cells` (1.1).

## 4. The cadence kernel, and why there is exactly one of it

The obvious design has Stage A simulate candidates in `mogwai-lab` and
Stage B re-implement them inside `GeneratedSource`. Rejected: two
implementations of one stochastic contract is the cross-language twin
defect the Python-to-Rust rewrite spent nine review passes killing, in
a new costume. The reviewing session allowed either one frozen kernel
executed by both sides or a bit-exact transcript matched by both; this
spec takes the stronger option and keeps the transcript as well.

Committing this module is NOT a tape-protocol event and owes no version
bump: no shipped preset declares the arrival seam, so `begin_event`
takes the shipped path for every committed instrument, no kernel
variant is reachable from the generator at all, and the commit carries
no changed generated artifact. Brick K's gate pins that. Brick S is the bump point. (Ruled
explicitly by session 019fe29f.)

### 4.1 Ownership: the kernel owns the whole time change

Revision 1's `next_gap(state, clock, rng) -> GapDraw` was refused, and
correctly: it returned a completed budget while the caller would have
had to own grid traversal, integration against the session curve, the
resolved timestamp and the self-excitation feedback - which is exactly
the part that must not be able to diverge between stages. The kernel
owns all of it:

Two rounds of review took this apart, and the scope of the claim has
had to shrink twice to become true. Revision 2 said the kernel owned
the whole time change; revision 3 added `resolve_clock` for the
calendar snap and still claimed "no caller-side clock policy". Both
were false, because `begin_event` carries FOUR runtime transformations
around the gap, not one:

- `FlowSurge` computes a `rate_mult` from `self.surge` and divides the
  duration before the clock advances;
- `ReopenGap` mutates `RegimeState` through `take_reopen_crossed`, so
  it is not a pure function of the clock;
- reopen ALSO moves `self.vol.mid` by `gap_frac`, so it is a coupled
  PRICE transformation and not a timestamp transformation at all;
- `step_child` advances `self.clock_ns` across the whole child burst,
  so the next gap opens from the LAST CHILD's timestamp, never from
  the previous parent's.

The fourth is the one that matters most and that revisions 1 to 3 all
missed: an "arrival-only" simulation is not arrival-only, because the
clock advance depends on the child COUNT.

The claim is therefore SCOPED rather than repaired by adding more
surface, and the scope is stated as a limit:

> The kernel owns the cadence: the gap, the latent state, the child
> count and the resulting clock advance. The runtime divergences enter
> it as INPUT (`RuntimeModifiers` below) rather than being applied
> around it, because a surge multiplier cannot scale a count the kernel
> has already drawn and a reopen shift cannot move a parent whose cell
> and successor clock are already computed. The caller retains only
> what is not cadence: querying and mutating `RegimeState`, and the
> coupled `vol.mid` price move. Stage A passes
> `RuntimeModifiers::NEUTRAL` and reproduces Stage B exactly under the
> frozen exposure of section 8.

Revision 5 said instead that surge and reopen were adapter work
"outside the parity claim", and that was refused: scoping a claim does
not rescue a production API that cannot express the behavior it must
carry. The repair is to pass the modifiers in, which makes the claim
unscoped as well as true.

Three consequences are frozen with it:

1. **The child-count draw moves to the cadence stream** for the
   INTEGRATED families 2 to 4 only. Family 1 is a protocol-12 arrival
   seam too, and deliberately keeps that draw on the main stream under
   stochastic contract A (section 7); revision 8 said "for the
   protocol-12 branch", which would have swept family 1 in and undone
   its byte identity. Revision 2 put it on the main stream to avoid a
   second difference between branches; that is REVERSED, because the
   clock advance depends on the child count and a Stage A driver that
   cannot draw it cannot predict the next gap's start. With the count
   on the cadence stream, Stage A reproduces every timestamp exactly
   while simulating no price, size or level draw at all. The new
   argument dominates the old one and the reversal is recorded rather
   than quietly made.
2. **`next_parent` takes `from_ns` and returns `next_from_ns`.**
   Revision 4 had the caller advance by `count * INTRA_EVENT_STEP_NS`,
   which is WRONG by one stride: `step_child` assigns the first child
   the parent's own timestamp because `emitted` starts at zero, so the
   burst ends at `parent_ts_ns + (count - 1) * INTRA_EVENT_STEP_NS`
   and every subsequent Stage A gap would have opened 1,000 ns late.
   Rather than restate the corrected formula and hope both callers read
   it the same way, the kernel RETURNS the resulting clock, so there is
   no caller-side arithmetic left to get wrong. The `(count - 1)`
   identity is pinned by its own test against the existing
   child-stride tests.
3. **The calendar moves INSIDE the intensity** (4.2), so the snap that
   revision 3 tried to own never fires on the protocol-12 branch, and
   `resolve_clock` is deleted rather than kept as a half-owned seam.

```rust
/// Everything the cadence contract needs from the instrument: the
/// session curves, the calendar and the static thin factor.
/// Constructed by ONE public function that both `GeneratedSource` and
/// `mogwai-lab` call, so neither can assemble a different
/// environment. Immutable: it holds no live divergence state, which
/// is what keeps it constructible identically on both sides.
pub struct ArrivalEnv {
    session: SessionModulator,
    calendar: Option<SessionCalendar>,
    thin: f64,
    origin_ns: u64,          // the cadence grid's phase anchor (4.2)
}

impl ArrivalEnv {
    pub fn for_profile(
        profile: &InstrumentProfile,
        thin: f64,
        start_ts_ns: u64,
    ) -> Self;

    /// The rate multiplier at a wall instant: the session arrival
    /// multiplier divided by the thin factor, and EXACTLY ZERO when
    /// `calendar.is_open(t)` is false. That zero is what puts the
    /// calendar inside the integrated intensity (4.2) instead of
    /// leaving it to a post-hoc snap.
    pub fn rate_at(&self, clock_ns: u64) -> f64;
}

/// THREE variants, not five. Revision 6 put `Legacy` and
/// `EventMarkov` in here too and that was refused, on a fact about the
/// shipped draw order: `begin_event` draws the gap innovation and the
/// arrival flip, THEN the latent-mid price and side/book draws, and
/// only THEN the child count. A kernel that owns the child count
/// necessarily draws it before the caller can execute the intervening
/// price and book work, which destroys byte identity for any branch
/// that must share the main RNG - and section 7 requires exactly that
/// of `Legacy`, which never constructs a cadence stream.
///
/// So the boundary follows the actual structure: `Legacy` and family 1
/// are PARAMETERIZATIONS OF THE SHIPPED PATH and stay in
/// `begin_event`; only the three integrated wall-time families, which
/// have no byte-identity obligation and draw children on their own
/// stream, go through the kernel.
pub enum ArrivalKernel {
    WallMmpp(WallMmppParams),
    LogOuCox(LogOuParams),
    SelfExciting(SelfExcitingParams),
}

/// Parameter structs, all `Copy`, all deserialized from the preset's
/// `[instrument.generator.arrival]` table, all validated on load
/// against the domains of section 16 and refused outside them.
pub struct EventMarkovParams  { pub quiet_share: f64, pub switch_rate: f64,
                                pub rate_ratio: f64 }
pub struct WallMmppParams     { pub occupancy: f64, pub rate_ratio: f64,
                                pub tau_s: f64 }
pub struct LogOuParams        { pub sigma_y: f64, pub tau_s: f64 }
pub struct SelfExcitingParams { pub phi: f64, pub tau_s: f64 }

/// Every field pinned, because "per-kernel latent state" is not a
/// specification. `new` is the only constructor and performs each
/// family's frozen initialization (5.1 to 5.4), consuming exactly the
/// draws that initialization owns: none for Legacy, EventMarkov and
/// SelfExciting, one Bernoulli for WallMmpp, one normal for LogOuCox.
/// `cell_index` is the grid cell of the last state update, so a gap
/// spanning many cells advances the latent state deterministically
/// from wherever it was left.
pub enum ArrivalState {
    WallMmpp     { quiet: bool, cell_index: u64 },
    LogOuCox     { y: f64, cell_index: u64 },
    SelfExciting { a: f64, cell_index: u64, cell_count: u32 },
}

impl ArrivalState {
    pub fn new(
        kernel: &ArrivalKernel,
        origin_ns: u64,
        rng: &mut ChaCha12Rng,
    ) -> Self;
}

pub struct ParentDraw {
    /// The parent's timestamp under the neutral-regime cadence. No
    /// snap is applied or needed: `rate_at` is exactly zero in closed
    /// windows, so no candidate can land in one.
    pub parent_ts_ns: u64,
    /// Drawn on the cadence stream, because the clock advance depends
    /// on it and a driver that cannot draw it cannot predict the next
    /// gap.
    pub child_count: u32,
    /// The clock the NEXT `next_parent` advances from:
    /// `parent_ts_ns + (child_count - 1) * INTRA_EVENT_STEP_NS`, the
    /// last child's timestamp. Returned rather than left to the caller
    /// precisely because revision 4 got this arithmetic wrong.
    pub next_from_ns: u64,
    /// The latent multiplier in force at `parent_ts_ns`. Transcripts
    /// pin this, so a latent-state divergence cannot hide behind an
    /// agreeing timestamp.
    pub latent_x: f64,
    /// True when the resolved candidate crossed `pending_reopen` and
    /// the shift was applied BEFORE cell assignment and
    /// `next_from_ns`. The caller consumes the matching `RegimeState`
    /// entry and applies the coupled `vol.mid` move only when this is
    /// true, so the two never disagree about whether a reopen fired.
    pub reopen_applied: bool,
}

/// The runtime divergence state, passed IN rather than left as
/// caller-side work around the kernel. Revision 5 called surge and
/// reopen "adapter work outside the parity claim"; that was refused,
/// and correctly, because `children_mult` cannot scale a count the
/// kernel has already drawn and a `ReopenGap` shift cannot move a
/// parent after the kernel has already computed `next_from_ns` and
/// assigned the self-exciting cell. The kernel applies each modifier
/// at the only point where it is applicable, so the production API
/// CARRIES the divergences instead of contradicting them.
///
/// The CALLER still owns querying `RegimeState` - including
/// `take_reopen_crossed`, which mutates - and owns the coupled
/// `vol.mid` move, which is a PRICE transformation and no business of
/// a cadence contract. `ArrivalEnv` therefore stays immutable and
/// both stages can still construct it identically.
pub struct RuntimeModifiers {
    /// FlowSurge: divides the gap, applied BEFORE resolution.
    pub rate_mult: f64,
    /// FlowSurge: scales the child-count law, applied BEFORE the draw.
    pub children_mult: f64,
    /// ReopenGap: the PENDING reopen, described immutably, not an
    /// already-computed shift. Revision 6 passed `reopen_shift_ns` in,
    /// which cannot work: `RegimeState::take_reopen_crossed(old, new)`
    /// discovers a crossing only once the NEW clock is known, and
    /// `next_parent` is the operation that computes it, so the caller
    /// cannot know the shift before calling. The kernel is given the
    /// armed reopen and decides whether the resolved candidate crosses
    /// it.
    pub pending_reopen: Option<PendingReopen>,
}

pub struct PendingReopen {
    /// The armed instant; a crossing is `from_ns < at_ts_ns` and
    /// `at_ts_ns <= candidate_ns`, matching `take_reopen_crossed`.
    pub at_ts_ns: u64,
    pub shift_ns: u64,
}

THE REOPEN TRANSFORMATION, frozen step by step. Revision 7 said only
that the shift is applied "before cell assignment", which named a
vector case without defining its semantics. The order below reproduces
the production sequence in `begin_event` exactly, and the kernel
performs steps 2, 3, 5 and 6 while the caller performs 4:

```text
1. resolve the gap candidate from the integrated intensity
2. test the crossing: from_ns < at_ts_ns && at_ts_ns <= candidate_ns
3. apply the shift with SATURATING addition:
   shifted = candidate.saturating_add(shift_ns)
4. the CALLER, seeing reopen_applied, calls
   RegimeState::take_reopen_crossed(from_ns, candidate_ns) to consume
   the armed entry and applies the coupled vol.mid jump. The kernel
   never mutates RegimeState.
5. if the SHIFTED timestamp lands in a closed window, snap it to
   calendar.next_open_ns - the one place an integrated family can
   snap, because an armed shift can jump a candidate across a closure
6. cell assignment and next_from_ns both use the SHIFTED-AND-SNAPPED
   timestamp, never the raw candidate
```

If the descriptor and the live `RegimeState` disagree - the caller
passed a `PendingReopen` that `take_reopen_crossed` then declines to
return - that is a CALLER BUG, not a recoverable state: the kernel has
already applied a shift the regime does not agree happened. The caller
asserts the agreement and panics on mismatch, because continuing would
serve a tape whose timestamps no state explains. Vector V8 pins all
three cases: no crossing, a crossing, and a crossing whose shift lands
inside a closure.

THE ZERO-SNAP CLAIM IS THEREFORE SCOPED, and 4.2's flat statement that
integrated families never snap is corrected here: they never snap under
NEUTRAL exposure, which is what the frozen exposure of section 8 and
every gate in this document use. Under an armed `ReopenGap` a shift can
land inside a closure and step 5 fires. Brick K's zero-snap gate is a
neutral-exposure gate and says so.

impl RuntimeModifiers {
    /// rate_mult 1.0, children_mult 1.0, pending_reopen None - what the
    /// frozen neutral-regime exposure of section 8 always passes, and
    /// what makes the Stage A / Stage B parity claim exact rather than
    /// scoped by assertion.
    pub const NEUTRAL: Self;
}

/// Refusals are a RETURN VALUE, not a panic and not a silent cap.
/// Revision 5 specified two refusal conditions while pinning an
/// infallible signature - a contradiction an implementer would have
/// resolved by panicking in a library the venue runs in production.
pub enum ArrivalRefusal {
    /// 4.2: the budget could not be spent within MAX_SESSION_GAP_NS
    /// because no future open exposure exists.
    NoOpenExposure { from_ns: u64 },
    /// 5.4: the latent intensity exceeded SELF_EXCITING_X_CEILING.
    IntensityCeiling { clock_ns: u64, x: f64 },
    /// Any non-finite latent state.
    NonFiniteState { clock_ns: u64 },
}

impl ArrivalKernel {
    /// Resolve the NEXT parent from `from_ns` - the caller's clock
    /// AFTER the previous parent's child burst - advancing all latent
    /// state, consuming every draw the mechanism owns including the
    /// child count, and applying every runtime modifier at its own
    /// point.
    pub fn next_parent(
        &self,
        state: &mut ArrivalState,
        from_ns: u64,
        base_mean_s: f64,
        shape: &SweepShape,
        env: &ArrivalEnv,
        modifiers: RuntimeModifiers,
        rng: &mut ChaCha12Rng,
    ) -> Result<ParentDraw, ArrivalRefusal>;
}
```

REFUSAL PROPAGATION, pinned end to end so the error path is as
specified as the happy one:

- `GeneratedSource` treats any `ArrivalRefusal` as a FATAL walk error.
  Revision 6 said it "surfaces through the existing tick-source error
  path", and no such path exists: `TickSource::next_tick` returns
  `Option<TickEvent>` and the server reads `None` as normal end of
  stream, so a refusal would have been indistinguishable from a clean
  finish - the silent-degrade class this repository keeps finding.

  Revision 7 proposed changing the trait to
  `Result<Option<TickEvent>, TickSourceError>` and claimed a blast
  radius of four implementors. That survey was WRONG, and checking it
  properly is what changed the design: the consumers, not the
  implementors, are the cost. `while let Some(tick) = source.next_tick()`
  appears in `mogwai-server`'s `tape.rs`, `source.rs` and `http.rs`, in
  `mogwai-lab`'s `summary.rs`, in `mogwai-cli`'s `measure.rs` and
  `gen.rs` at seven sites, plus `.expect(...)` and `?` uses in
  `fills.rs`, `fill_golden.rs` and `tick_composition.rs`, plus the
  trait's own default `seek_to`, `MergeSource`'s initialization and
  head replenishment, the `mogwai-data` example and many tests. A
  signature change rewrites all of it to carry an error that only one
  implementor can ever produce.

  FROZEN DESIGN, the smaller and better one - a defaulted query method,
  so no existing implementor or consumer changes at all:

  ```rust
  pub trait TickSource {
      fn next_tick(&mut self) -> Option<TickEvent>;

      /// The terminal fault that ENDED this source, if it ended
      /// because of one. `None` means ordinary exhaustion. The default
      /// is `None`, so every existing implementor is correct
      /// unchanged.
      fn fault(&self) -> Option<TickFault> { None }
  }

  pub enum TickFault {
      Arrival(ArrivalRefusal),
  }
  ```

  - `GeneratedSource` records the refusal, returns `None` from
    `next_tick` thereafter, and reports it from `fault()`.
  - `MergeSource` FAILS FAST, which revision 8 left open and which was
    a silent-degrade risk in new clothing: reporting "the first faulted
    child" while continuing to serve a surviving infinite child means
    `next_tick` never returns `None`, so the server never queries
    `fault()` and the fault is never observed. Frozen instead: on
    detecting a child fault at initialization or at head
    replenishment, `MergeSource` LATCHES the fault, DISCARDS its
    buffered heads, and returns `None` from that call onward - so it
    becomes terminal no later than the `next_tick` that discovers the
    fault, and `fault()` never reports a fault that has not ended the
    source. Serving a truncated merge after one leg died would be a
    tape nobody could interpret.
  - Every other implementor inherits the default and is untouched.

  HONEST LIMIT OF THE DEFAULTED METHOD, stated because it is the
  design's real weakness: it is an opt-in terminal-status side channel.
  It does not make unchecked `next_tick` consumers fault-aware, and it
  distinguishes fault from exhaustion only for a consumer that queries
  `fault()` at every terminal `None`. That is acceptable here because
  the offline consumers (`gen`, `measure`, `summary`) run a bounded
  window and a fault there surfaces as a short walk the operator sees,
  while the ONE consumer where silence would be harmful is the serving
  path, which is specified below and gated.

  SERVER DISPOSITION, concrete types and owners, because "logs and ends
  the run" is not buildable. The tape worker is a detached
  `thread::spawn` holding `Arc<Tape>` with a `cancel` flag, it simply
  `break`s on `None` today, `/health` is statically ok, and the
  shutdown channel is created later inside serving setup and is not
  reachable from the worker. The frozen changes:

  ```text
  Tape           gains  fault: Mutex<Option<TickFault>>
  TapeSpawn      gains  fault_tx: mpsc::Sender<TickFault>
  serve.rs       MOVES the shutdown channel construction ahead of
                 Tape::start so the sender can be handed to TapeSpawn
  /health        gains  "fault": null | { "kind": ..., "clock_ns": ... }
  ```

  On `None` from `next_tick` the worker calls `source.fault()`:

  - `None` - ordinary exhaustion, the existing `break`, unchanged, and
    the run completes normally with exit status 0.
  - `Some(fault)` - the worker stores it in `Tape::fault`, logs the
    variant and clock at ERROR, sends it on `fault_tx` and breaks.
    `serve` receives it, returns `Err`, and the BINARY EXITS NONZERO.

  Exit status is the load-bearing signal, not a new protocol surface,
  and that is deliberate: `notes/todo.md` already records the owner's
  position that a venue fault is mogwai failing to do its job, is
  obviously terminal, and is observed by the parent process watching a
  fire-and-forget child. A nonzero exit is exactly what that consumer
  already watches for, and it cannot be confused with the declared
  completion that exits 0.

  WHAT A WEBSOCKET-ONLY CONSUMER SEES, stated as a LIMIT rather than
  claimed as a feature. Nonzero exit is sufficient for the parent
  process watching the child PID, which is the consumer this venue is
  built for. It is NOT an in-band explanation: a websocket-only client
  sees its stream terminate with no fault reason and must correlate
  that with the supervisor or with a successfully sampled `/health`.
  Revision 9 claimed client frames and refused subscriptions
  distinguished a fault from exhaustion; they do not, because this
  design deliberately adds no client fault frame and a closing stream
  looks the same either way. `subscribe_with_snapshot` stays
  INFALLIBLE.

  `/health` still carries the fault field, because a consumer that
  polls it may catch the reason, but it is NOT gated: the tape can
  refuse before the listener is even bound, and once the fault reaches
  `serve` the accept loop stops, so no launched-binary test can
  reliably sample `/health` in the window between publication and exit.
  Asserting it would be a flaky gate pretending to be an exact one.

  GATE, exact command, asserting only what is deterministic:

  ```text
  brokkr test -p mogwai-cli a_faulted_venue_exits_nonzero_and_an_exhausted_one_does_not
  ```

  It asserts, for a venue whose seam is configured to refuse: the fault
  recorded in `Tape::fault` before shutdown, the ERROR diagnostic
  emitted, and a NONZERO exit; and for a venue with a bounded
  `run_duration_ns`: exit zero. It lives in `mogwai-cli` because only
  that crate's tests get `CARGO_BIN_EXE_mogwai`.

- Stage A records the refusal against the cell with its variant and
  clock, marks the cell inadmissible under A4 validity, and moves on. A
  refused cell never enters the loss.
- Stage B does the same and the cell is infeasible.
- Both artifacts carry the refusal records, so a family refusing across
  a whole region reads as that rather than as an empty admissible set.

`mogwai-lab` depends on `mogwai-data` already and calls `next_parent`
with an `ArrivalEnv::for_profile` built from the same
`InstrumentProfile` the server resolves and `RuntimeModifiers::NEUTRAL`;
`GeneratedSource::begin_event` calls the identical function with the
identical environment and its live modifiers. There is no second
implementation of the cadence, and under the frozen neutral-regime
exposure the two stages agree bit for bit.

### 4.2 The common frame for the three new wall-time families

`Legacy` and `EventMarkov` keep the SHIPPED frame exactly (once-sampled
multiplier on the open-market path, `low_intensity_gap_ns` otherwise).
`WallMmpp`, `LogOuCox` and `SelfExciting` use an exact time change:

- Each defines a latent multiplier `X(t) >= 0`, piecewise constant on a
  grid of `CADENCE_STEP_NS`, evolving in wall time across hour
  boundaries, closures and reopenings without reset.
- The grid is aligned to the TAPE ORIGIN, not to the UTC second. This
  costs nothing and removes the phase coincidence with the 12a
  one-second count window, which would otherwise let a family's update
  boundary sit exactly on the measurement boundary.
- The next parent solves, exactly,

  ```text
  integral from t to t' of  baseline.rate_at(u) * X(u) / base_mean_s du
      = g,        g ~ Exp(1) drawn as -ln(U)
  ```

  consumed segment by segment, where a segment ends at the earliest of
  the next grid boundary, the next UTC hour boundary and the next
  calendar open or close instant (the baseline is piecewise constant
  between those, `X` per grid step, so each segment is exactly
  integrable and the residual inside the final segment is exact).

  THE CALENDAR IS INSIDE THIS INTEGRAL, via `rate_at` returning exactly
  zero while the venue is shut, and that closes a defect revision 3
  argued around instead of repairing. The shipped `SessionProfile`
  validates every hour share as strictly POSITIVE, so a closed hour
  carries a tiny positive rate and the calendar does its real work
  afterwards by SNAPPING a timestamp that landed in a closed window
  forward to `calendar.next_open_ns` - which concentrates every
  closed-window candidate onto the reopen instant. Revision 3 claimed
  such parents land in cells the self-exciting update does not read;
  that was FALSE, since the update counts by resolved timestamp and the
  reopened cell sits well above the expected-count floor. With the
  calendar inside the intensity the situation cannot arise at all: no
  candidate is generated in a closed window, no snap fires, and the
  budget is simply not consumed while the venue is shut, which is the
  trading-hours semantics the low-intensity budget path was always
  reaching for. Brick K gates it directly - zero snaps on the
  protocol-12 branch under the frozen NEUTRAL exposure. The claim is
  scoped to neutral exposure deliberately: an armed `ReopenGap` shift
  can jump a candidate across a closure, and 4.1 step 5 handles that
  case rather than pretending it cannot arise.

  PROGRESS ACROSS ZERO-RATE SEGMENTS, which an exactly-zero rate makes
  a real obligation rather than a detail: a closed segment consumes no
  budget, so the traversal must SKIP it rather than divide by zero or
  spin. The frozen rule is that a segment whose rate is exactly zero
  advances the cursor to the segment's end and consumes nothing, and
  that the traversal is bounded by `MAX_SESSION_GAP_NS` exactly as
  `low_intensity_gap_ns` already bounds itself. If the cursor reaches
  that bound without finding enough open exposure - a calendar with no
  future open segment, which the frozen exposure does not contain but a
  misconfigured profile could - the kernel REFUSES rather than
  returning a saturated timestamp, and the refusal is recorded against
  the cell. That is the same failure the shipped low-intensity path
  handles by capping; here it fails loudly instead, because a silently
  capped gap in a candidate search would look like a feasible cell.
- `base_mean_s = mean_event_duration_s * ARRIVAL_MEAN_CAL`, the shipped
  calibration, unchanged.
- Child counts: the kernel draws them on the cadence stream (4.1) with
  a state multiplier of exactly 1.0 for these three, so the parent
  draws `children_mean` unconditionally and the declared mean is
  preserved identically. Only `EventMarkov` keeps state-conditioned
  multipliers, under the same mean-preserving identity that pins them
  today; its active multiplier is DERIVED from that identity and never
  independently tuned.

Two deliberate consequences, stated rather than discovered:

- These three families do not inherit the documented legacy artifact
  whereby a gap opening in an open hour crosses a later closed window
  at its open-hour rate. That is a behavior difference by construction,
  confined to the protocol-12 branch, moving no legacy byte.
- `EventMarkov` is evaluated in the SHIPPED frame while the other three
  are evaluated in the integrated frame, so a cross-family comparison
  is partly a comparison of frames. This is deliberate: re-framing the
  nested family would confound a frame change with a persistence change
  and destroy the one clean nested test available. The grid-sensitivity
  gate of 9.4 bounds the frame's own contribution for the other three,
  and section 10.3's parameter-count tie-break never compares across
  frames without every hard gate having passed on both.

### 4.3 The grid, defended

`CADENCE_STEP_NS = 1_000_000_000` (one second) is a MODEL DEFINITION,
not an approximation to a continuous process: each family below is
defined as the grid process, so there is nothing to converge to and no
discretization error to bound in principle. Three specific hazards the
review raised are answered concretely:

- Multiple state transitions inside one cell. Answered exactly for
  `WallMmpp` by using the sampled continuous-time transition matrix
  (5.2), which is exact over any interval however short the dwell.
- Dynamics faster than the grid. Answered by domain: `SelfExciting`'s
  decay time is floored at `2 * CADENCE_STEP_NS` and `WallMmpp`'s and
  `LogOuCox`'s correlation times at `1 s`, so no frozen cell asks the
  grid to resolve something it cannot represent.
- Phase coincidence with the measurement window. Answered by aligning
  the grid to the tape origin (4.2).

What remains is that the grid is a modelling choice, and choices get
measured here rather than asserted: 9.4 freezes a grid-sensitivity gate
re-running the selected point at 250 ms with stated equivalence bounds.

## 5. Candidate families

Four active families and one negative control, frozen. Every active
family satisfies `E[X] = 1` exactly under its own stationary law -
DERIVED below, not asserted - so the fitted hourly session arrival curve
is preserved analytically rather than by fitting. Only the negative
control touches that curve.

### 5.1 Family 1: event-time two-state Markov renewal (the nested shipped family)

The shipped mechanism with its three constants instrument-resolved.
State flips per PARENT EVENT at switch rate `w = 1 - persistence`;
stationary quiet share `q`; `r` is the quiet-to-active mean-gap ratio.
Base means solve from the declared mean exactly as today, so the
unconditional mean gap is preserved for every `(q, w, r)`, which is the
existing identity in `consts.rs` and needs no new derivation.

Fitted: `w` ONLY. HELD: `q = 0.35` and `r = 150`, the shipped values,
in both stages - see the reduction argument below, which revision 8
made while leaving this line saying all three were fitted. Fixed:
Weibull shape 1.0 - the nested test must not confound a shape change
with a persistence change. Child
multipliers instrument-resolved, active one derived. `w = 0` is
EXCLUDED as absorbing.

THIS FAMILY DOES NOT GO THROUGH THE KERNEL. It is a parameterization
of the shipped path, so it stays in `begin_event` under stochastic
contract A (section 7) with `(q, w, r)` read from the
instrument-resolved seam, and BOTH stages evaluate it by driving the
REAL generator through `advance_parent()` rather than by simulating
it. That is forced by the draw-order fact in 4.1 and it has
two consequences, both stated rather than absorbed:

- Stage A's family-1 cells cost a full generator walk (price and size
  draws included) instead of a cadence-only walk, roughly an order of
  magnitude more per cell.
- Its grid is therefore REDUCED, and reduced on an argument rather
  than to fit a budget: the nested question is whether PERSISTENCE
  alone can produce minute-scale over-dispersion, so `w` is gridded
  across its full range while `q` and `r` are HELD at their shipped
  fitted values. Freeing all three would be re-fitting the incumbent
  rather than testing it.

  THIS HOLDS IN BOTH STAGES. Revision 7 said Stage B would explore `q`
  and `r`, which was an undefined search - Stage A would have produced
  an admissible list containing only `(0.35, 150, w)` coordinates, so
  Stage B would have been evaluating unscreened points and breaking the
  nesting rule of section 3. Family 1 is therefore a ONE-PARAMETER
  candidate throughout, and the verdict it can support is stated as
  narrowly as it deserves:

  > A family-1 failure falsifies PERSISTENCE VARIATION AT THE SHIPPED
  > `q` AND `r`. It does not falsify the event-time family in general.
  > Widening it to `(q, w, r)` is a new frozen search space and a new
  > owner decision, not a continuation of this one.

  Its fitted-parameter count for the 10.3 tie-break is therefore ONE,
  which means it wins the simplicity ordering against every other
  family. That is the correct ordering for a nested test of the
  incumbent at its own fitted values, and it is recorded here so the
  tie-break is not later read as a surprise.

The shipped point `(0.35, 0.10, 150)` is simply the `w = 0.10` cell of
the reduced grid, since `q` and `r` are held at their shipped values:
`0.10` lies on the switch-rate grid exactly, at `1e-6 * 10^(15/3)`. It
is reported as the incumbent reference. It must reproduce the Legacy tape BYTE for BYTE
(brick K gate), which is the strongest available check that the
instrument-resolved plumbing changed nothing.

### 5.2 Family 2: wall-time two-state MMPP

Two states with WALL-CLOCK dwell, independent of how many parents
arrive. This family exists because it is the minimal change that
distinguishes persistence in elapsed time from persistence in event
index, which is the unresolved structural question.

Revision 1 wrote the grid transition as `1 - exp(-step/D_s)` per state
and normalized against a continuous-time occupancy those probabilities
do not produce. Repaired by using the exactly sampled CTMC transition
over a grid step of length `dt`:

```text
P(quiet -> active) = (1 - q) * (1 - exp(-dt / tau))
P(active -> quiet) =      q  * (1 - exp(-dt / tau))
```

Stationarity check, which is why this form is chosen: detailed balance
gives `pi_q * (1-q) = pi_a * q`, hence `pi_q = q` exactly, for every
`dt` and every `tau`. Because this is the exact sampled transition of
the underlying CTMC, it also accounts for arbitrarily many transitions
inside one cell, which is what removes the short-dwell hazard.

Levels, from `q * x_quiet + (1 - q) * x_active = 1` and
`x_active / x_quiet = r`:

```text
x_quiet  = 1     / (q + (1 - q) * r)
x_active = r     / (q + (1 - q) * r)
```

so `E[X] = 1` exactly.

Fitted: `q`, `r`, `tau`. Initialization: the state is drawn from the
stationary law at the tape origin (quiet with probability `q`), one
draw. Evaluation order within a step is frozen: the transition is
applied AT the grid boundary, and the interval `[t_k, t_k+1)` uses the
POST-transition state.

### 5.3 Family 3: log-OU Cox intensity

`X(t) = exp(Y(t) - sigma_Y^2 / 2)` with `Y` a stationary OU process of
zero mean, stationary standard deviation `sigma_Y` and correlation time
`tau`. `E[X] = exp(-sigma_Y^2/2) * E[exp(Y)] = 1` exactly at stationary
grid points. Updated by the EXACT OU transition, never an Euler step:

```text
a      = exp(-dt / tau)
Y_next = a * Y + sigma_Y * sqrt(1 - a^2) * Z,   Z ~ N(0, 1)
```

Initialized from the stationary law `Y_0 ~ N(0, sigma_Y^2)` at the tape
origin, one draw, never reset. Fitted: `sigma_Y`, `tau`.

Log-OU is chosen over gamma/CIR NOW, before any histogram is seen.
Choosing between lognormal and gamma mixing after seeing results is
forbidden by section 11.

### 5.4 Family 4: discrete self-exciting intensity

Revision 1 called this a Hawkes process and wrote a recursion that was
not one: it added an absolute parent RATE to a dimensionless
multiplier, so its mean moved with the hourly curve and the `(1 - phi)`
prefactor restored nothing. The `(1 - n)` normalization is valid for a
continuous Hawkes intensity with kernel mass `n` and a CONSTANT
baseline, which is not this. Repaired by defining a discrete
self-exciting process directly and deriving its stationary mean under
the time-varying baseline mogwai actually uses. The name changes with
it: this is not a Hawkes process and will not be called one.

Per grid step `k` spanning `[t_k, t_k+1)`:

```text
E_k   = (integral over the step of baseline.rate_at(u) du) / base_mean_s
        -- the EXPECTED parent count from the baseline ALONE, with the
           latent multiplier excluded. Deterministic.
n_k   = parents whose resolved timestamp lies in step k.
a_k   = n_k / E_k                       when E_k >= EXPECTED_COUNT_FLOOR
      = 1                               otherwise (closed or near-closed
                                        steps carry no information and
                                        relax toward the stationary mean)
decay = exp(-dt / tau)
A_(k+1) = decay * A_k + (1 - decay) * a_k
X_(k+1) = (1 - phi) + phi * A_(k+1)
```

Mean preservation, derived BY INDUCTION rather than by assuming
stationarity - revision 2 invoked a stationary law under a
time-varying baseline, which is precisely where such an argument is
not entitled to stand. With `E[n_k | X_k] = E_k * X_k` giving
`E[a_k] = E[X_k]`:

```text
E[A_(k+1)] = decay * E[A_k] + (1 - decay) * E[X_k]
E[X_k]     = (1 - phi) + phi * E[A_k]
```

Start at `A_0 = 1`. If `E[A_k] = 1` then `E[X_k] = (1-phi) + phi = 1`,
so `E[A_(k+1)] = decay + (1 - decay) = 1`. By induction
`E[A_k] = E[X_k] = 1` for EVERY step and every baseline path, with no
stationarity assumption anywhere.

The floored steps take a second, piecewise induction: when
`E_k < EXPECTED_COUNT_FLOOR` the update substitutes `a_k = 1`, and
`E[a_k] = E[X_k]` is then FALSE in general - but the substitution maps
a mean-one state to a mean-one state, so `E[A_k] = 1` is carried
through unchanged and the induction closes on both branches. What the
floor cannot do is preserve the mean from a state that has already
drifted, which is why the claim is stated as an invariant from a
mean-one start rather than as a restoring force.

`E[n_k | X_k] = E_k * X_k` is exact in the CONTINUOUS-TIME model, and
is exact in the implementation only under four conditions, stated
because three of them are real:

1. resolved timestamps follow the integrated intensity - true by
   construction of 4.2;
2. calendar snapping does not move parents between cells. Revision 3
   argued this was satisfied because snapped parents land in floored
   cells, and that argument was FALSE: a snap moves a candidate to
   `next_open_ns`, which is the first instant of an OPEN cell, well
   above the floor, and several closed-window candidates can pile onto
   the same reopened cell and enter `a_k` together. The repair is
   structural rather than argumentative - with the calendar inside the
   intensity (4.2) no candidate is ever generated in a closed window,
   so no snap fires on this branch and the condition holds by
   construction rather than by a claim about where snapped parents
   land. Brick K's zero-snap gate is what makes it checkable;
3. nanosecond rounding and the one-nanosecond minimum gap do not move a
   parent across a cell boundary. This is NOT exactly true: the
   generator rounds to integer nanoseconds, and a parent within one
   nanosecond of a grid boundary can land on the wrong side of it;
4. the residual budget across a grid boundary keeps the conditional
   process Poisson - true, because the exponential is memoryless and
   4.2 consumes the budget rather than redrawing it.

A FIFTH condition, and it is the one that finally decides the claim.
Revision 4 established that the next gap opens from the LAST CHILD's
timestamp, and did not carry that fact back into this derivation. It
has to: during each child burst the arrival process integrates no
intensity at all, so resolved parents are not a Poisson process with
compensator `E_k * X_k` but a renewal process with count-dependent DEAD
TIME. The equality above is therefore structurally false, not merely
imprecise, and no amount of measurement can rescue a claim of
exactness.

The claim is downgraded accordingly, with a bound rather than a shrug.
Writing `d_k` for the fraction of step `k` consumed by child bursts,

```text
E[n_k | X_k] = E_k * X_k * (1 - d_k) + O(d_k^2)
d_k          = (E[child_count] - 1) * INTRA_EVENT_STEP_NS / mean_gap_k
```

Its size, computed from the actual frozen constants rather than
estimated - revision 5 guessed `children_mean` as "a small single-digit
number" and landed one to two orders out:

```text
children_mean         = 1.1711127211559897   (presets/mnq.toml)
mean_event_duration_s = 0.060859305487494256 (presets/mnq.toml)
INTRA_EVENT_STEP_NS   = 1_000 ns = 1e-6 s    (consts.rs)

at the declared mean gap:      d = 0.1711127 * 1e-6 / 0.0608593 = 2.8e-6
at a 2,000 parent/minute hour: d = 0.1711127 * 1e-6 / 0.03      = 5.7e-6
at a 4,000 parent/minute hour: d = 0.1711127 * 1e-6 / 0.015     = 1.1e-5
```

So `d_k` is of order 1e-6 to 1e-5, not the 1e-4 revision 5 claimed, and
the hour-conditioned realized rate is stated separately from the
declared mean gap rather than conflated with it.

TWO HONEST LABELS ON THAT NUMBER. It is a SCALE ESTIMATE, not a bound:
the first-order expression drops a term because `d_k` and `X_k` are
dependent - busy cells have both more feedback and more dead time - and
this spec does not derive the conservative inequality that would make
it a true bound. What actually carries the assurance is the empirical
gate: the realized count rate per open hour within 1 percent of the
baseline expectation, which sits five orders above the estimated
effect and would catch a dead-time error far larger than any of these
figures. The mean-preservation property is therefore stated as EXACT IN
THE CONTINUOUS MODEL WITHOUT DEAD TIME, as estimated at order 1e-5 in
the implementation, and as GATED at 1 percent.

Because conditions 3 and 5 both fail at their own scales, this spec
does NOT claim exact mean preservation in the implementation. It claims
exactness in the model and bounds the implementation's deviation by
derivation AND measurement.
Revision 3 leaned that bound entirely on a 30-day LATENT mean, which
was rightly called insufficient: a latent mean can sit at 1.0 while the
realized counts are redistributed. The layer-3 conformance gate for
this family therefore checks three things, not one (bands in section
16):

- the realized latent mean against 1.0;
- the realized COUNT rate per open hour against the baseline
  expectation, which is the quantity the identity is actually about;
- the reopen-cell count against its neighbours, so any residual
  concentration at a session open is visible rather than averaged away.

A miss on any of the three is a blocking defect, not a tolerance to
widen.

Every causal choice the review found unspecified is frozen here:
parents in step `k` affect `X` from step `k+1` onward and never their
own step, so the process is strictly causal; all parents in a cell
enter that cell's count with no sub-cell placement; the feedback is
dimensionless and carries no `dt` factor, so an hour-boundary change in
the baseline changes `E_k` and leaves `A` untouched; and the state
initializes at `A_0 = 1`, `X_0 = 1` at the tape origin.

Stability: `X >= 1 - phi > 0` always, so the intensity is bounded below
and cannot stall the walk. `phi` is gridded strictly below
`SELF_EXCITING_PHI_MAX = 0.90`. A run whose `X` exceeds
`SELF_EXCITING_X_CEILING = 1e4` at any grid step REFUSES that cell,
recorded, rather than clipping.

Fitted: `phi`, `tau`. This family's RECURSION consumes no random draws:
it is deterministic given the resolved parent times. It still draws the
budget and the child count like every other family, per section 7.

### 5.5 Family 5: hourly re-centring (the negative control)

Revision 1 named a 23-value curve refit with no objective, no search,
no seeds and no place in the sequence. That was a missing brick, not a
control. Repaired by making it a DETERMINISTIC one-pass re-centring
with no search at all, which is both fully specifiable and a sharper
test of the premise:

```text
For each traded hour h:
    ratio[h]     = generated mean parents per scheduled minute at h
                   (shipped generator, CONTROL_FIT_SEEDS, median over
                   seeds)
                   / observed mean parents per scheduled minute at h
                   (from the committed 12a block1 histogram)
    new_curve[h] = old_curve[h] / ratio[h]
Rescale the 24 values to sum to 1, which the `SessionProfile` schema
requires.
```

THE RESCALE IS NEUTRAL, and revision 2 mis-described it as preserving
exposure. Derived rather than assumed: `SessionModulator::new` stores
`arr_hour[h] = intensity_hour[h] * 24` and `arrival_mult` divides by
`arrival_normalizer`, the exposure-weighted mean of `arr_hour * arr_dow`
over the calendar's open minutes. Multiplying every `intensity_hour[h]`
by a common constant multiplies both the numerator and the normalizer
by that constant, so `arrival_mult` is unchanged at every instant. The
rescale is therefore CANONICAL SERIALIZATION to satisfy the schema's
sum-to-one contract, and it changes no generated rate. What moves the
tape is the per-hour shape, which is exactly what the control is for.

Scope limit, stated: `arrival_mult` is `arr_hour[hour] * arr_dow[dow]`,
so this control corrects the HOUR axis only and leaves
`session.dow_weight` untouched. That is the correct scope - the defect
under test is an hour-shaped one - but it means a day-of-week-shaped
error would survive the control, and the control's failure therefore
falsifies less than "the curve is right".

One iteration, closed form, no search and no objective. It is
nonetheless a FIT of 23 data-derived values, each from a noisy
generated estimate, so revision 2's claim that it had "no free
parameters to overfit" was wrong in kind: deterministic is not the same
as unfitted. It therefore gets its own disjoint seeds and is judged out
of sample:

```text
CONTROL_FIT_SEEDS  = 301..304   the ratios are measured here
CONTROL_TEST_SEEDS = 305..308   the corrected curve is judged here
```

`CONFIRMATION_SEEDS` are not touched by the control at all, which is
what lets section 7 keep calling them an untouched holdout.

Place in the sequence: it runs FIRST, as brick N, BEFORE Stage A,
because a pass ends the landing. It is evaluated on `CONTROL_TEST_SEEDS`
against gates B1 to B7 - NOT B8. Revision 3 said "the full Stage B set"
while B8 is grid sensitivity, defined for a selected grid-based
candidate and comparing quantities that exist only for the active
families; the control has no cadence grid to be sensitive to, so B8 is
inapplicable rather than waived, and saying which it is was the gap. A pass produces
`negative-control-passed` and stops for an owner ruling. A failure is
recorded with the failing gates and the landing proceeds to Stage A. It
is not a Stage A survivor and never enters the family ranking.

The evidence predicts failure, and the prediction is recorded so the
failure is not later mistaken for a surprise: protocol 11 already
fitted the hourly marginal parent counts to a worst-hour error of 0.63
percent (`crates/mogwai-server/presets/mnq.toml`), so the hourly means
are not what is wrong, and a deterministic hourly rate cannot produce a
within-hour MIXTURE - at hour 20 the observed distribution spans four
bins and lowering the curve to reach `65-256` cannot simultaneously
produce the `1025-4096` mass, and at hour 19 no mean shift explains the
`4097+` mass. The control runs anyway, because a predicted failure that
is not run is an assumption.

## 6. Fidelity: what makes Stage A believable

**Layer 1, the exact shipped-generator oracle.** `advance_parent()` on
the real `GeneratedSource` at shipped parameters, projected to parent
timestamps and child counts, must reproduce the committed
`generated.per_seed[*].blocks.block1` parent-count marginal and the
whole `block2` record of `analysis/mnq-measure-12a.json` EXACTLY, for
all eight committed seeds, under the exposure contract of section 8.
This validates the extraction and aggregation code, not the simulator.
Failure blocks. Replaying the committed seeds here is an ORACLE test,
not candidate screening, and does not compromise the seed holdout of
section 7.

**Layer 2, exact parity, stated per contract.** For families 2, 3 and
4, Stage A and the Stage B generator execute the same `arrival.rs`
`next_parent`, so a candidate's Stage A timestamps are an exact
prediction of its Stage B integration. Family 1 and Legacy use no
kernel at all (stochastic contract A): both stages drive the REAL
generator through `advance_parent`, so their parity is not a claim
about two implementations agreeing - there is only one. Transcripts
therefore exist for the three KERNEL families only, and family 1's
equivalent evidence is the byte-identity gate at the shipped point.
Guaranteed structurally, and pinned by a bit-exact TRANSCRIPT FIXTURE
per family, whose contract is frozen in section 16: the first 10,000
`ParentDraw` records - `parent_ts_ns`, `child_count` and `latent_x`,
so a latent divergence cannot hide behind an agreeing timestamp - at a
named parameter point, seed, initial state and exposure, with floats
serialized as raw `u64` bit patterns. Both the Stage A driver and a
Stage B integration test replay and compare them bit for bit.
Transcripts are committed data, generated once by brick K and never
regenerated to match a later change.

WHAT A TRANSCRIPT DOES AND DOES NOT PROVE. A transcript generated by
the implementation is a REGRESSION pin: it proves the two stages agree
and that neither drifts later. It is NOT evidence that the mechanism is
correct, and this spec does not use it as such.

Revision 4 tried to close that gap with a hand derivation at a
degenerate point, and the attempt was refused on two grounds, both
right. It conflicted with the fixture contract (section 16 puts each
transcript at the coarse cell nearest its domain centre, which is not
degenerate), and it did too little work: a degenerate point with a
constant baseline and frozen `X` collapses every family to the same
exponential-gap arithmetic, checking budget conversion and rounding
while checking nothing about the mechanisms most likely to be got
wrong. Both are fixed by separating the two artifacts instead of
overloading one:

- **Regression transcripts** stay exactly as section 16 defines them,
  at the domain-centre cell, and carry no correctness claim.
- **Conformance vectors**, new and committed as fixtures rather than
  as prose in a generated header, are independently tabulated - each
  expected value derived from the closed forms of section 5 by hand,
  recorded with its derivation, and never produced by running the
  implementation. One vector per item below, each at a parameter point
  chosen to make its own arithmetic tractable:

  ```text
  V1  family 2: the sampled CTMC transition probabilities at three
      (dt, tau) pairs, plus one grid-boundary traversal proving the
      transition applies AT the boundary and the interval uses the
      post-transition state
  V2  family 3: the OU stationary initialization and one exact
      transition at a known Z, checking a and sqrt(1 - a^2)
  V3  family 4: three feedback steps by hand from a known count
      sequence, including one floored step and one reopen
  V4  family 1: two state transitions at a known switch rate, the
      child draw reading the PRE-FLIP state, and the INTERVENING
      main-stream price and book draws written out in order - family 1
      shares the main RNG, so a vector that hid those draws would not
      exercise the ordering its byte identity depends on
  V5  every family: the child-count distribution's first four
      probabilities against SweepShape's own law, and the draw order
      of section 7, stated separately for the shipped-path families
      (main stream, child draw after the price and book work) and the
      kernel families (cadence stream, child draw inside next_parent)
  V6  every integrated family: one traversal crossing a grid boundary,
      a UTC hour boundary and a calendar close in a single gap, with
      the segment-by-segment budget consumption tabulated

  V7  every family: the degenerate constant-baseline case revision 4
      proposed, retained because it is a clean check of
      exponential-budget conversion and nanosecond rounding, and
      honest about being only that
  V8  the reopen seam, three cases: no crossing, a crossing, and a
      crossing whose shift lands inside a closure - each pinning the
      resolved timestamp, `reopen_applied`, the assigned cell and
      `next_from_ns` against the six-step order of 4.1, so the caller
      and kernel cannot disagree about whether a reopen fired
  ```

A transcript that disagrees with its family's vectors fails brick K,
and a vector may never be regenerated from the implementation.

VECTOR SCHEMA, frozen, one file per vector at the paths in section 13:

```text
{
  "vector": "V1",
  "what_it_checks": "<one line>",
  "derivation": "<the closed form, written out, with the section it
                  comes from and the arithmetic performed on the
                  stated inputs - reviewable without running code>",
  "params": { <the family's parameter struct, exact literals> },
  "inputs":  [ { <the call's arguments, floats as hex bit patterns> } ],
  "expected":[ { <the expected records, floats as hex bit patterns> } ]
}
```

The `derivation` field is what makes the vector independent: it is
prose plus arithmetic a reviewer can follow to the same numbers, and a
vector whose derivation does not reproduce its own `expected` is a
defect in the vector. Hex bit patterns keep a decimal parser off the
critical path of an arithmetic test, the convention
`scripts/gen_pvariance_cases.py` already established in this
repository.

**Layer 3, the distributional bridge at shipped parameters.** With
`EventMarkov` at the shipped point, the comparison against the layer-1
oracle is BIT-EXACT rather than distributional, because that family
keeps the shipped frame (4.2). This is stronger than revision 1's
equivalence-band bridge and replaces it.

For the three integrated-frame families there is no shipped counterpart
to bridge to, so their frame is validated differently and specifically:
an ANALYTIC conformance test per family, comparing the realized
long-run mean rate and the realized latent-multiplier distribution
against the closed-form stationary law derived in section 5, over a
frozen long walk, within the bands of section 16. This tests the
implementation against its own derivation, which is a real check with a
known right answer, rather than against a statistic it is also being
fitted to.

## 7. The stochastic contract

- **Stream separation, claimed precisely.** The protocol-12 arrival
  branch draws from a dedicated `ChaCha12Rng` seeded
  `cadence_seed = splitmix64(run_seed ^ CADENCE_STREAM_TAG)` using the
  now-public `splitmix64` of `crates/mogwai-protocol/src/seeds.rs`.
  What this guarantees is exactly this and no more: the cadence draws
  themselves are positionally stable across arrival parameter changes
  on a fixed seed. It does NOT keep every price, size, level and bounce
  draw in the same position - the main stream is consumed through
  branch-dependent work, child counts change later consumption, and
  timestamps move session-dependent branches, so two arrival points
  diverge on the main stream too. Revision 1 claimed semantic common
  random numbers on this basis and the claim was over-broad. Stage B is
  therefore a FIXED-SEED evaluation with an isolated cadence stream,
  and it controls seed variation the way 12a does: the eight-seed
  median, with the seed-agreement rule as evidence.
- **TWO STOCHASTIC CONTRACTS, not one.** Revision 7 split the
  architecture in 4.1 - family 1 and Legacy on the shipped path, the
  three integrated families in the kernel - and left this section
  describing the old single contract, which would have destroyed the
  very byte identity the split existed to protect. The contracts are
  now stated separately and neither borrows from the other.

  **Contract A - Legacy and family 1 (event-time), the shipped path.**
  Every draw stays on the MAIN stream in the existing `begin_event`
  order, which is not restated as a new rule because it is not a new
  rule:

  ```text
  1. the Weibull gap innovation          (next_duration_ns)
  2. the arrival state flip, recording last_quiet BEFORE it
  3. the latent-mid price draws          (caller, main stream)
  4. the side and book draws             (caller, main stream)
  5. the child-count draw, reading the PRE-FLIP last_quiet
  ```

  No cadence stream is constructed, `next_parent` is never called, and
  family 1 differs from Legacy only in that `(q, w, r)` come from the
  instrument-resolved seam instead of `consts.rs`. Steps 3 and 4 sit
  BETWEEN the flip and the child draw, which is exactly why this family
  cannot go through a kernel that owns the child count.

  **Contract B - families 2, 3 and 4, the cadence kernel.** The cadence
  stream owns the gap budget draw, the latent-state draws and the
  child-count draw, in that order inside `next_parent`:

  ```text
  1. the budget draw -ln(U)
  2. latent-state draws in grid-step order, one per traversed step
     (family 2's transition, family 3's normal; family 4 takes none)
  3. the child-count draw, unconditioned (children_mult 1.0)
  ```

  The main stream keeps every price, size, level and bounce draw, none
  of which moves a timestamp - which is what lets Stage A predict these
  families' timestamps exactly while simulating no price at all.

  Byte identity is structural under both contracts: a seam-absent
  instrument runs contract A unchanged, and contract B is unreachable
  without a declared seam.

- **Seed sets, frozen and pairwise disjoint.** Revision 1 used the
  committed seeds for both Stage A screening and confirmation, which
  meant the selected candidate had already been chosen partly against
  the confirmation seeds. Repaired:

  ```text
  STAGE_A_SEEDS        = 201..204   (coarse pass 201..202)
  STAGE_B_SEARCH_SEEDS = 101..108
  CONTROL_FIT_SEEDS    = 301..304
  CONTROL_TEST_SEEDS   = 305..308
  CONFIRMATION_SEEDS   = 1..8       (the committed 12a seeds, touched by
                                     no screening, search, control-fit
                                     or control-test step)
  ```

  Confirmation and the 12a ladder re-run therefore land on seeds no
  parameter was chosen against and no decision was taken on. Layer 1
  replays 1..8 as an oracle over the SHIPPED mechanism, which fits and
  selects nothing.

## 8. The exposure contract

Frozen for every Stage A and Stage B walk: instrument MNQ resolved
through `Config::load` against `crates/mogwai-server/presets/mnq.toml`,
no divergence armed, regime neutral; the tape anchor, warmup and
generated window READ FROM `analysis/mnq-measure-12a.json`'s
`binding.generated` rather than restated here, so the two cannot drift;
all 23 traded hours, every scheduled session in the window, both
segment-label axes; timestamps at the generator's own nanosecond
rounding with `INTRA_EVENT_STEP_NS` unchanged. No partial window,
boundary cell or inconvenient hour is dropped anywhere in this spec; a
cell below floor is a recorded refusal under 12a semantics, never an
omission.

## 9. Stage A: the necessary-condition screen

Corpus-free. Inputs are `analysis/mnq-measure-12a.json` (bound by path
and SHA-256, recorded in the artifact) and the kernel.

### 9.1 The observed projections

- `P_obs[h]`, the distribution over populated minutes of the EXACT
  parent count `N` at hour `h`, by marginalizing
  `observed.monthly.block1.hist` over the two range axes and both
  segment-label axes. Exact `N` is retained; the six 12a bins are used
  only where 9.2 names them.
- `C_obs[h][W]`, the exact count histogram, run-length histogram and
  lag-1 moments at each `W` in `COUNT_WINDOWS_S`, from
  `observed.monthly.block2` unchanged.

### 9.2 Admissibility, checked BEFORE any loss

A cell is admissible iff ALL of the following hold for EVERY seed in
the pass's seed set. Any failure refuses the cell with the failing
condition, seed and cell recorded. No pooling across seeds, no
nearest-bin substitution, no pseudocount rescue.

```text
A1 SUPPORT   -> Stage B gate B2
   The exact conjunction of the two DIFFERENT frozen 12a rules, which
   revision 2 collapsed into one stronger rule and thereby broke the
   nesting it claimed:
   (a) count-substitution support, 12a 5.2: for every hour h in all 24
       and every parent-count bin b with OBSERVED share above zero, the
       generated count in (h, b) is NONZERO. (The frozen weight table
       refuses only the o > 0, g = 0 cell.)
   (b) conditional adequacy, 12a 5.2 rung 2c: for every hour h in
       FAIL_HOURS_300 = {19, 20, 23} and every REQUIRED bin b there
       (pooled OBSERVED populated-minute count at least
       MIN_MINUTES_CELL = 30), EVERY seed's generated count in (h, b)
       is at least MIN_MINUTES_CELL.
   Nothing stronger. A1 is exactly what B2 will demand.

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

Each of A1, A2 and A3 reappears verbatim as a Stage B hard gate, which
is what makes the Stage A set a NECESSARY-condition screen rather than
a preference. Revision 1 also screened on the 60 s Fano and count-p99
bands; that is REMOVED, on an argument worth recording because it is
not merely a nesting repair:

> Requiring the arrival family's own 12a metrics to land inside the
> materiality band would be 12b grading its own homework. Those metrics
> are ladder INPUTS; if support exists and they remain outside the band
> with envelope, rung 2 firing is a legitimate measured outcome that
> the owner ruling explicitly leaves open. A gate forbidding it would
> forbid a valid verdict.

That argument stands on its own. Revision 2 added a second one - that
A1 makes the Fano gate redundant because support across the observed
bins IS minute-scale over-dispersion - and that sentence is WITHDRAWN
as too strong: support is a statement about occupancy of bins, not
about the variance-to-mean ratio, and a distribution can satisfy the
one while missing the other. The removal needs only the first argument
and does not rest on the withdrawn claim.

The 60 s Fano and count-p99 log ratios are computed and REPORTED for
every cell, and feed the tie-break of 9.3, but gate nothing.

`MEAN_GAP_REL_TOL_12B` is this spec's own constant. The existing
`MEAN_GAP_REL_TOL = 0.10` in `mogwai-data`'s test module is a different
gate on a different walk and is untouched.

### 9.3 The loss, which ranks and never selects

For an admissible cell, per seed and hour, `L_comp[s,h]` is the exact
1-Wasserstein distance between the empirical distributions of
`log1p(N)` over populated minutes, generated against observed,
computed from the sorted empirical CDFs with no binning. The `log1p`
transform is frozen because parent counts span three orders of
magnitude and an untransformed distance would be dominated by the
busiest hour.

```text
L_comp[s] = sum over h of w[h] * L_comp[s,h]
w[h]      = observed populated minutes in h / total observed populated
            minutes
L_comp    = median over the pass's seeds of L_comp[s]
```

Tie-break, reported: the mean over hours and over `W in {1, 5, 60}` of
the absolute log ratio of the Fano factor.

The six-bin total variation distance is computed and REPORTED for every
cell because it is what the frozen counterfactual consumes, but it is
not the loss: it would let a family place exactly 30 minutes in a
required bin and score well. `L_comp` is a RANKING device and is never
cited as evidence that the raw count distribution agrees; A1 and the
Stage B gates carry that burden.

### 9.4 The grid-sensitivity gate

The PROVISIONAL Stage B winner - exactly one cell, never one per
family - is re-run with
`CADENCE_STEP_NS = 250_000_000` on `CONFIRMATION_SEEDS`, ONE run,
written to the `grid_sensitivity` block of
`analysis/mnq-arrival-selection.json`. The seed set is pinned here
because revision 5 left it implicit while 10.4 demanded B8 pass "on
CONFIRMATION_SEEDS" and the budget priced a single run: confirmation
seeds, once, is the reading both now state. Its cost is the one
sensitivity re-run `STAGE_B_BUDGET_S` already carries. Every Stage A statistic and every
Stage B gate statistic must stay inside `GRID_EQUIVALENCE` of section
16. A miss means the result is a property of the discretization rather
than of the mechanism, and the landing closes with
`confirmation-failed` rather than shipping a grid artifact.

### 9.5 Output

`analysis/mnq-arrival-screen.json`, committed: input binding (path,
SHA-256, schema version), the frozen search space in full, every
evaluated cell with per-condition per-seed verdicts, loss, reported
diagnostics and refusals, the measured cost, and the ADMISSIBLE REGION
per family as an explicit cell list. Verdict `arrival-admissible:
<families>` or
`no-arrival-admissible-candidate-in-frozen-search-space`.

## 10. Stage B: the real generator, and the only selection

### 10.1 The joint constrained solve

Over each surviving family's admissible region, taken in Stage A loss
order and truncated at `STAGE_B_CELL_CAP` cells per family, on
`STAGE_B_SEARCH_SEEDS`, evaluate the FULL constraint set of 10.2
simultaneously. Composition is never repaired first with wall time
inspected afterward; a cell failing any hard gate is infeasible, full
stop.

The cap makes this a RANKED SAMPLE of each admissible region rather
than an exhaustive search of it, and every outcome statement in this
document is written to that weaker claim (1.1). Where a family's
admissible region fits inside the cap, the search over that family IS
exhaustive and the artifact records which case applies per family.

### 10.2 The hard gates, all simultaneous

```text
B1 LEGACY BYTE IDENTITY
   Every shipped preset that does not declare the arrival seam produces
   a byte-identical tape to the pre-landing binary, by `cmp` over the
   fixed walks of section 16. Not statistical.

B2 SUPPORT AND CONDITIONAL ADEQUACY  (contains A1)
   The 12a count substitution runs unamended and produces support for
   every implicated hour: no observed-support-without-generated-support
   refusal at any of the 24 hours, and the 5.2 conditional adequacy
   guard evaluates rather than refuses.

B3 WALL-TIME CONTOUR
   Hourly 60 s and 300 s robust_scale generated/observed ratios inside
   [0.8, 1.25] at every hour, protocol-11 estimator. Inherited, not
   relaxable.

B4 MINUTE-RANGE ENVELOPE, TWO-SIDED
   p99 minute range inside a two-sided band whose LOWER bound comes
   from the lower tail of the same resampled envelope machinery that
   supplies the existing upper bound. The existing p99.9 and
   per-seed-max upper bounds continue to apply.

B5 THE STANDING GENERATOR GATES
   Every existing realism, rail, truncation and preset-provenance gate
   stays green and `brokkr check --gate` is green.

B6 MEAN-RATE PRESERVATION  (identical to A2)
   Per hour AND per seed, generated / observed mean parents per
   scheduled minute in MEAN_RATE_BAND. The seed aggregation is stated
   here in the same words as 9.2 deliberately: revision 2 wrote "every
   seed" in one place and "per hour" in the other, which is a nesting
   hole a reviewer found and an implementer would have inherited.
   Defended as a landing requirement rather than a screen: the hourly
   curve is protocol-11 fitted evidence, and a mechanism that moves it
   has silently refitted the one thing only the negative control may
   touch.

B7 SUB-SECOND COMPOSITION  (identical to A3)
   Per hour AND per seed, the 1 s zero-count-fraction ratio in
   ZERO_COUNT_BAND. Defended as a landing requirement: it is the
   sibling of the existing `zero_second_frac` realism gate (2.3), and
   without it a mechanism could buy minute-scale dispersion by bunching
   parents into pathological sub-second bursts.

B8 GRID SENSITIVITY
   Section 9.4, at the selected point.
```

### 10.3 Selection among feasible candidates

SELECTION IS ORDERED, because B8 is evaluated at "the selected point"
and requiring it for eligibility made the procedure circular - a point
had to be selected before it could become eligible for selection.
Frozen order:

1. PROVISIONAL selection over cells passing B1 to B7: fewest fitted
   parameters wins (counted per candidate, not assumed); ties break on
   `L_comp` recomputed on `STAGE_B_SEARCH_SEEDS`; candidates whose
   losses differ by less than `SELECTION_INDIFFERENCE = 0.01` produce
   `not-identified` and go to the owner.
2. B8, the grid-sensitivity re-run, on that provisional winner ALONE.
3. B8 failure CLOSES the landing with `no-feasible-cell-among-
   evaluated-cells`. It does NOT silently fall through to the next
   cell: a fallback that is not frozen is a fallback that gets
   invented under time pressure.
4. Confirmation (10.4) then re-runs B1 to B7 on `CONFIRMATION_SEEDS`
   plus the B8 result already established, rather than demanding B8
   again and reintroducing the circularity one layer down. A coin flip dressed as a
criterion is worse than saying the evidence does not separate them.

The negative control is evaluated separately (5.5) and never competes
here.

### 10.4 Confirmation

The selected cell re-runs on `CONFIRMATION_SEEDS`, which no parameter
was chosen against and no decision was taken on, and must pass every
gate of 10.2 again. A confirmation failure CLOSES the frozen run with
`confirmation-failed` and does not return to tuning. What that verdict
establishes is exactly one thing: the SELECTED cell is disproved out of
sample. It is not evidence about the other evaluated cells and not
evidence about the unevaluated admissible ones.

### 10.5 The ladder re-run

The UNAMENDED 12a measurement re-runs end to end and writes
`analysis/mnq-measure-12b.json` under both 12a validation gates. Its
verdict is recorded and taken to the owner. No 12a definition,
constant, bin, floor or predicate is touched. If the re-run reveals a
12a definition that cannot be evaluated against the repaired tape, this
landing FAILS and stops for an amendment, per the 12a stopping rule.

## 11. Anti-gaming constraints

- No seed-specific, hour-specific or result-specific parameters: one
  parameter point per family, every hour, every seed.
- The four active families keep the fitted session arrival curve FIXED
  and preserve its mean analytically. Only family 5 re-centres it.
- Price, GARCH, child-count, size and session-volatility parameters
  stay fixed. `vol_scalar` is NOT free: freeing it would open a
  compensation channel between arrival fidelity and volatility scale.
  If the joint solve is infeasible with it fixed, that is a
  `no-feasible-cell-among-evaluated-cells` finding and a new owner
  ruling, not a
  silent widening.
- No parameter-range expansion after a miss. No estimator, bin,
  threshold, tolerance, grid or seed change after seeing results.
- No family added, removed or re-specified after the freeze. Choosing
  between lognormal and gamma mixing after seeing a histogram is the
  named example.
- The section 16 conformance and equivalence bands are never tuned to
  make a candidate metric pass.

## 12. The search procedure and its budget

- Each fitted parameter carries a domain, transform and grid, frozen in
  section 16. Switch rates, correlation times and ratios are gridded
  LOGARITHMICALLY; occupancies and shares linearly.
- **Coarse pass**: the full tensor grid per family on
  `STAGE_A_SEEDS[0..2]` (two seeds). Using fewer seeds is deliberately
  CONSERVATIVE and not a budget dodge: every condition in 9.2 is
  per-seed and failure-monotone, so a two-seed pass can only ADMIT a
  superset of what four seeds would admit. It cannot falsely reject.
- **Refinement**: one subdivision pass at half spacing around the
  admissible region's boundary cells, on all four `STAGE_A_SEEDS`, to
  `REFINEMENT_DEPTH = 2`, capped at `REFINEMENT_CELL_CAP` cells per
  family. Grid endpoints are evaluated and admissible endpoints
  reported as such.
- Disconnected admissible regions are reported as multiple regions and
  every one advances. Nothing is smoothed or convex-hulled.
- Caching follows `mogwai_lab::fit::walk`'s convention, keyed by the
  full parameter point, seed, exposure contract and kernel version,
  under the existing storage policy's provenance token.
- **Stage A cost, measured before the grid runs.** Brick A0 is a cost
  probe on ONE cell per family, reporting wall time and peak RSS. The
  threshold DISPATCHES BY FAMILY, because the two evaluation paths cost
  an order of magnitude apart and revision 7 left a single 4-second
  bound that its own estimate would have failed family 1 against,
  stopping the run before the reduced grid could ever be used:
  `STAGE_A_GEN_CELL_BUDGET_S = 50.0` for family 1, which runs the real
  generator through `advance_parent`, and
  `STAGE_A_CELL_BUDGET_S = 4.0` for the kernel families 2 to 4, both at
  two seeds. Above it, brick A FAILS and stops for a re-freeze of the
  grids; the budget is never met by trimming the grids silently. Total
  frozen bounds: `STAGE_A_BUDGET_S = 28800` (8 h) and
  `STAGE_A_RSS_BYTES = 8 GiB`, recorded in the artifact and gated like
  12a's cost contract. Grid sizes are in section 16 so the arithmetic
  is checkable rather than asserted.
- **Stage B cost, which revision 2 did not price at all.** A Stage B
  cell is a full month-scale generator walk per seed plus its
  measurement, which the 12a Brick M record prices at roughly 25 s per
  walk, so eight search seeds cost order 250 s per cell. Evaluating a
  1,508-cell admissible region would take tens of hours, and revision 2
  simply advanced every survivor into that. Frozen instead:

  ```text
  STAGE_B_CELL_CAP  = 24 cells per surviving family
  STAGE_B_CELL_BUDGET_S = 320   (eight search seeds, one cell)
  STAGE_B_BUDGET_S  = 50400     (14 h, four families plus confirmation
                                 and the 250 ms sensitivity re-run)
  STAGE_B_RSS_BYTES = 8 GiB
  ```

  When a family's admissible region exceeds the cap, the cap applies in
  Stage A LOSS ORDER - which is what the loss is for, and the only role
  the stage boundary permits it. Brick S0 is the Stage B cost probe,
  one cell, measured against `STAGE_B_CELL_BUDGET_S` before the search
  runs; a miss FAILS brick S and stops for a re-freeze rather than a
  silent trim.

  THE HONEST CONSEQUENCE, stated because the cap changes what a
  negative verdict means: with a cap in force,
  the verdict is `no-feasible-cell-among-evaluated-cells` and asserts
  only that no feasible cell was found among the cells evaluated, with
  the artifact recording exactly which admissible cells went
  unevaluated. The stronger `no-jointly-feasible-mechanism` is
  unavailable whenever a cap bound the search. It is never reported as a
  claim about the whole admissible region, for the same reason section
  1.1 refuses to turn a finite grid into a claim about the continuum.

## 13. Artifacts

```text
analysis/mnq-minute-range-envelope.json  the two-sided B4 bound,
                                      committed, written by brick B4
analysis/mnq-arrival-control.json     the negative control, committed
analysis/mnq-arrival-screen.json      Stage A, committed
analysis/mnq-arrival-selection.json   Stage B search, confirmation and
                                      selection, committed
analysis/mnq-measure-12b.json         the unamended 12a ladder re-run,
                                      committed
crates/mogwai-data/tests/fixtures/arrival-transcript-<family>.json
                                      layer-2 regression transcripts,
                                      committed
crates/mogwai-data/tests/fixtures/arrival-vector-v1-mmpp-transition.json
crates/mogwai-data/tests/fixtures/arrival-vector-v2-logou-transition.json
crates/mogwai-data/tests/fixtures/arrival-vector-v3-selfexciting-feedback.json
crates/mogwai-data/tests/fixtures/arrival-vector-v4-eventmarkov-flip.json
crates/mogwai-data/tests/fixtures/arrival-vector-v5-child-law-and-order.json
crates/mogwai-data/tests/fixtures/arrival-vector-v6-triple-boundary.json
crates/mogwai-data/tests/fixtures/arrival-vector-v7-degenerate-budget.json
crates/mogwai-data/tests/fixtures/arrival-vector-v8-reopen-seam.json
                                      the independently tabulated
                                      conformance vectors of section 6,
                                      committed, never regenerated from
                                      the implementation
```

Each carries a `binding` block in the 12a style: harness tree commit,
clean-tree attestation, input hashes, the exposure contract, the kernel
version and the full frozen search space.

## 14. Bricks

### Brick F: freeze

This document argued to consensus with a reviewing codex session and
frozen, the sign-off recorded here. No implementation before that.

### Brick K: the kernel, dormant

`crates/mogwai-data/src/generated/arrival.rs` with the three kernel
variants, the instrument-resolved seam on `GeneratorScalars` carrying
family 1's `(q, w, r)` on the shipped path, the `TickSource` trait
change,
the frame of 4.2, the transcript fixtures, the layer-2 replay test, the
conformance vectors V1 to V8, and `splitmix64` made public with its
stable-vector test. No shipped preset declares the seam, so every
committed instrument takes the shipped path unchanged and
`TAPE_PROTOCOL_VERSION` stays 11.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-data arrival_transcripts_replay_bit_exact
brokkr test -p mogwai-data arrival_conformance_vectors_v1_through_v8
brokkr test -p mogwai-data arrival_families_match_their_stationary_derivations
brokkr test -p mogwai-data the_integrated_families_never_snap_a_closed_window_timestamp
brokkr test -p mogwai-data the_self_exciting_family_holds_its_count_rate_and_reopen_cell
brokkr test -p mogwai-data the_event_markov_family_at_the_shipped_point_is_byte_identical_to_legacy
brokkr test -p mogwai-protocol splitmix64_matches_its_stable_vectors
```

### Brick B4: the two-sided minute-range body gate

The lower acceptance bound from the lower tail of the existing
resampled envelope machinery, landed as its own instrument BEFORE the
gate that consumes it.

AMENDMENT 1, 2026-08-08. This brick MOVED ahead of brick N. Revision 10
ordered N first among the evaluation bricks while requiring N to be
judged against gates B1 to B7 - and B4 IS one of those gates, whose
lower bound does not exist until this brick builds it. The order was
therefore circular, and the implementer refused to proceed rather than
skip or weaken a gate, which is the correct reading of section 17. The
repair takes the spec's own governing rule, stated in this brick and in
`reference/technical-implementation-spec.md` item 5: an instrument
lands before the gate that consumes it. Nothing else changes; N still
runs first of the EVALUATION bricks, B4 being an instrument rather than
an evaluation.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-lab the_minute_range_envelope_supplies_a_lower_bound
brokkr run mogwai -- minute-range-envelope --out analysis/mnq-minute-range-envelope.json
```

AMENDMENT 2, 2026-08-09. This brick also WRITES ITS BOUND AS A COMMITTED
ARTIFACT, and the reason is a second gap the implementer found by
building: computing the lower bound is not the same as having one to
judge against. `analysis/mnq-measure-12a.json` carries no minute-range
resampling population, and the committed protocol-11 fit artifact
carries upper bounds only, so brick N had no committed source for B4 at
all - leaving only a fresh corpus pass, an edit to a frozen artifact, or
an invented input path, none of which is acceptable.

`analysis/mnq-minute-range-envelope.json` closes that. It is produced
ONCE here from the observed corpus through the existing protocol-11
`minute_range_envelope` machinery, and carries the p99 lower bound, the
p99, p99.9 and per-seed-max upper bounds, and a `binding` block naming
the corpus job, the file hashes, the resampling seed and the method, in
the 12a style. Every later consumer of B4 - brick N, Stage B and
confirmation - READS that artifact rather than recomputing it, so the
bound cannot drift between the bricks that share it and no later brick
needs the corpus on disk to evaluate B4.

STAGE A IS NOT A CONSUMER OF B4, and an earlier draft of this amendment
wrongly listed it as one. Section 9.2 is the binding admissibility list
and it is A1 to A4; B4 is a Stage B gate, named in 10.2 and nowhere in
Stage A. The screen therefore needs neither this artifact nor the
corpus, which is the corpus-free property section 3 rests on. The stray
mention cost an implementer a blocked run before it was caught.

The two-sided gate itself is unchanged; this pins where its numbers come
from. If the artifact and a recomputation ever disagree, the
RECOMPUTATION is authoritative and the artifact is stale - it is a
committed derivative, never an independent source of truth.

### Brick N: the negative control

Section 5.5, run FIRST of the EVALUATION bricks. Writes
`analysis/mnq-arrival-control.json`, whose schema is pinned here because
revision 10 named the path and nothing else:

```text
{
  binding:      { harness_tree_commit, clean_tree, input_hashes,
                  exposure, control_fit_seeds, control_test_seeds },
  ratios:       { hour: { generated_mean, observed_mean, ratio } },
  old_curve:    [24 floats],
  new_curve:    [24 floats, summing to 1],
  gates:        { B1..B7: { passed: bool, evidence: <per-gate record>,
                            refusals: [RefusalRec] } },
  verdict:      negative-control-passed | negative-control-failed,
  failing_gates:[names]
}
```

`B8` is absent by inapplicability, not recorded as passed or refused:
the control has no cadence grid to be sensitive to (5.5).

```text
brokkr fmt
brokkr check --gate
brokkr run mogwai -- arrival-control --out analysis/mnq-arrival-control.json
```

A pass STOPS the landing with `negative-control-passed`.

### Brick A0: the Stage A cost probe

One cell per family, two seeds, measured. Reports wall time and peak
RSS against the family's OWN bound: `STAGE_A_GEN_CELL_BUDGET_S` (50 s)
for family 1, which drives the real generator, and
`STAGE_A_CELL_BUDGET_S` (4 s) for the kernel families 2 to 4. A single
bound would fail family 1 by construction. A miss FAILS brick A and
stops for a grid re-freeze.

```text
brokkr run mogwai -- arrival-screen --cost-probe
```

### Brick A: the Stage A screen

`mogwai_lab::arrival_screen` plus `mogwai arrival-screen`, the fidelity
layers, the admissibility conditions, the loss and the artifact.
Corpus-free; runs on any clone.

```text
brokkr fmt
brokkr check --gate
brokkr test -p mogwai-lab arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks
brokkr run mogwai -- arrival-screen --out analysis/mnq-arrival-screen.json
```

The layer-1 test is blocking: it must reproduce the committed
artifact's generated `block1` parent-count marginal and whole `block2`
record for all eight committed seeds, exactly.

### Brick S0: the Stage B cost probe

ONE cell, eight search seeds, measured against
`STAGE_B_CELL_BUDGET_S = 320 s` and `STAGE_B_RSS_BYTES`, before the
search runs. A miss FAILS brick S and stops for a re-freeze of
`STAGE_B_CELL_CAP` or the budgets; it is never met by trimming
silently.

```text
brokkr run mogwai -- arrival-solve --cost-probe
```

### Brick S: Stage B, and the landing

One coherent history unit, because the version bump may not exist
before the changed tape artifact does. Contains: the
instrument-resolved arrival seam on `GeneratorScalars`; the legacy
byte-identical branch; the capped joint solve over every Stage A
survivor in loss order; the
confirmation run; the grid-sensitivity re-run; the selected mechanism
and parameters in `presets/mnq.toml` with full provenance;
`TAPE_PROTOCOL_VERSION = 12`; and
`analysis/mnq-arrival-selection.json`.

Legacy byte identity, exact procedure. Revision 2's command was
unrunnable (`--type bars` requires `--interval`, and the CLI takes
`--symbol` or `--config`, never `--preset`) and, worse, compared
AGGREGATED BARS, which cannot prove tape identity because different
tick streams can produce identical bars. Corrected on both counts:
`--type trades` is the raw tape and is byte-complete.

BEFORE the branch lands, from the pre-landing release binary, for each
of the four crypto and MES presets - the ones that must not move:

```text
brokkr run --release mogwai -- gen --type trades --symbol BTCUSDT --seed 7 --length 2d --out analysis/out/legacy-BTCUSDT-before.csv
brokkr run --release mogwai -- gen --type trades --symbol ETHUSDT --seed 7 --length 2d --out analysis/out/legacy-ETHUSDT-before.csv
brokkr run --release mogwai -- gen --type trades --symbol SOLUSDT --seed 7 --length 2d --out analysis/out/legacy-SOLUSDT-before.csv
brokkr run --release mogwai -- gen --type trades --symbol MES --seed 7 --length 2d --out analysis/out/legacy-MES-before.csv
brokkr run --release mogwai -- gen --type trades --symbol MNQ --seed 7 --length 2d --out analysis/out/legacy-MNQ-before.csv
```

AFTER, re-run each with `-after.csv` and compare:

```text
cmp analysis/out/legacy-BTCUSDT-before.csv analysis/out/legacy-BTCUSDT-after.csv
cmp analysis/out/legacy-ETHUSDT-before.csv analysis/out/legacy-ETHUSDT-after.csv
cmp analysis/out/legacy-SOLUSDT-before.csv analysis/out/legacy-SOLUSDT-after.csv
cmp analysis/out/legacy-MES-before.csv analysis/out/legacy-MES-after.csv
```

Those four must be byte-identical. MNQ is EXPECTED to differ once its
preset declares the seam, so its identity check needs a seam-absent
config - and revision 3 specified that as "copy the file and delete the
table", a manual edit inside a gate advertised as exact, and a fragile
one since deleting a TOML table must take exactly that table and its
contents without damaging what follows. Replaced by a deterministic CLI
mode, landed as part of brick S with its own test:

```text
brokkr run --release mogwai -- presets emit --symbol MNQ --omit generator.arrival --out analysis/out/mnq-seamless.toml
brokkr run --release mogwai -- gen --type trades --config analysis/out/mnq-seamless.toml --seed 7 --length 2d --out analysis/out/legacy-MNQ-seamless.csv
cmp analysis/out/legacy-MNQ-before.csv analysis/out/legacy-MNQ-seamless.csv
```

`presets emit --omit <dotted.path>` serializes the resolved preset with
exactly the named table removed, and refuses a path that is absent so a
typo cannot silently emit an unmodified config and turn the gate into a
tautology. Its test asserts both directions: the omission removes the
table, and a bad path refuses.

That is the actual section-8 property: an instrument that omits the
seam draws the legacy shape byte for byte. The captures are scratch
under `analysis/out/` and are not committed.

```text
brokkr fmt
brokkr check --gate
brokkr run mogwai -- arrival-solve --out analysis/mnq-arrival-selection.json
python3 scripts/smoke.py
```

### Brick R: the ladder re-run

```text
brokkr fmt
brokkr check --gate
brokkr run mogwai -- measure --out analysis/mnq-measure-12b.json
brokkr test -p mogwai-lab parity12a_i
```

`mogwai measure` runs both 12a validators (`measure12a_schema_errors`
and `measure12a_semantic_errors`) internally and refuses on either, so
the artifact cannot land unvalidated; `parity12a_i` re-proves the
assembly path itself is unchanged. The 12a Brick M cost contract
applies unchanged.

## 15. Keep/revert

Bricks K, N, B4, A0 and A are additive and independently revertible;
none changes a tape byte and each is kept or reverted on its own gate.
Brick S is the one intrusive landing and is kept or reverted WHOLE: if
any gate of 10.2 fails at confirmation, the entire brick reverts and
the landing closes with `confirmation-failed`. No partial
keep, no experiment switch, no env-var scaffolding: the arrival seam is
a legitimate instrument-resolved profile field under the standing
parameterization ruling, or it is nothing. The suite is green at every
boundary between bricks.

## 16. Frozen constants, search space and grid sizes

```text
CADENCE_STEP_NS         = 1_000_000_000     (grid aligned to tape origin)
CADENCE_STREAM_TAG      = 0x6D6F6777_61693132
STAGE_A_SEEDS           = 201..204   (coarse pass 201..202)
STAGE_B_SEARCH_SEEDS    = 101..108
CONFIRMATION_SEEDS      = 1..8
REFINEMENT_DEPTH        = 2
REFINEMENT_CELL_CAP     = 600 per family
SELECTION_INDIFFERENCE  = 0.01
SELF_EXCITING_PHI_MAX   = 0.90
SELF_EXCITING_X_CEILING = 1e4
EXPECTED_COUNT_FLOOR    = 0.01      (expected parents per grid step)
MEAN_RATE_BAND          = [0.98, 1.02]
ZERO_COUNT_BAND         = [0.8, 1.25]
MEAN_GAP_REL_TOL_12B    = 0.05
STAGE_A_CELL_BUDGET_S   = 4.0       (kernel cell, two seeds)
STAGE_A_GEN_CELL_BUDGET_S = 50.0    (family 1 real-generator cell)
STAGE_A_GEN_REFINE_CAP  = 40        (family 1 refinement cells)
STAGE_A_BUDGET_S        = 28800
STAGE_A_RSS_BYTES       = 8 GiB
STAGE_B_CELL_CAP        = 24 per surviving family
STAGE_B_CELL_BUDGET_S   = 320       (eight search seeds, one cell)
STAGE_B_BUDGET_S        = 50400
STAGE_B_RSS_BYTES       = 8 GiB

GRID GENERATION, exact, because "3 per decade" does not specify
endpoint inclusion or rounding:
  linear(lo, hi, step): lo, lo+step, ... up to and including hi when
    hi - lo is an exact multiple of step, which it is at every use
    below; values are exact decimal literals, never accumulated sums.
  logk(lo, hi, k): the points lo * 10^(j/k) for j = 0, 1, ... while
    lo * 10^(j/k) <= hi * (1 + 1e-12); hi is appended as a final point
    when the last generated value is below hi by more than that
    tolerance. Values are computed in f64 from the literal lo, hi, j
    and k, never chained.

Family 1, event-time two-state Markov renewal  (1 fitted: w)
  REAL-GENERATOR family, stochastic contract A: evaluated through
  advance_parent in BOTH stages, not the kernel (5.1), at roughly ten
  times the per-cell cost. q and r are DECLARED-HELD, not fitted, in
  both stages; a failure falsifies persistence variation at the
  shipped q and r and nothing wider.
  q   quiet share   HELD at the shipped 0.35
  r   rate ratio    HELD at the shipped 150
  w   switch rate   log3(1e-6, 0.5)                         -> 19
  cells 19, PLUS 1 reference cell at the shipped point
  (0.35, 0.10, 150). Its w = 0.10 lies on the switch-rate grid
  exactly, at 1e-6 * 10^(15/3), so the reference cell coincides with a
  grid cell here and is counted once; q and r are held at shipped
  values by construction. Revision 2 called the point on-grid,
  revision 3 called it off-grid on all three axes, and revision 6
  called it off-grid on two - under the reduced grid it is simply the
  w = 0.10 cell.
  family total 19

KERNEL families, evaluated through arrival.rs at the cadence-only cost:

Family 2, wall-time two-state MMPP  (3 fitted)
  q   occupancy     linear(0.10, 0.60, 0.10)                ->  6
  r   rate ratio    log3(2, 200)                            ->  7
  tau seconds       log3(1, 3600)                           -> 12
  tau is the CTMC CORRELATION time 1 / (alpha + beta), not either
  state's mean dwell; 5.2's transition and level formulas are stated
  in exactly those terms.
  cells 504

Family 3, log-OU Cox  (2 fitted)
  sigma_Y           linear(0.2, 2.0, 0.2)                   -> 10
  tau seconds       log3(1, 3600)                           -> 12
  cells 120

Family 4, discrete self-exciting  (2 fitted)
  phi               linear(0.10, 0.85, 0.05)                -> 16
  tau seconds       log3(2, 600)                            ->  9
  cells 144

  Point counts recomputed against the logk rule above rather than
  estimated: log3(1e-6, 0.5) runs to j = 17 at 0.4641589 and then
  appends 0.5, giving 19; log3(2, 600) runs to j = 7 at 430.887 and
  appends 600, giving 9; log3(10, 1000) and log3(2, 200) land exactly
  on their upper endpoints at j = 6, giving 7 with no append; and
  log3(1, 3600) runs to j = 10 at 2154.43 and appends, giving 12.

  COST, under the two-tier model the kernel narrowing forces:
    kernel coarse   768 cells * STAGE_A_CELL_BUDGET_S 4.0 s  =  3,072 s
    family 1 coarse  19 cells * STAGE_A_GEN_CELL_BUDGET_S 50 s = 950 s
    coarse total                                             =  4,022 s
                                                             =  1.12 h
    kernel refinement    600/family cap, 1,800 cells at 4 seeds
                         and so twice the per-cell budget    = 14,400 s
    family 1 refinement  capped at STAGE_A_GEN_REFINE_CAP 40
                         cells at 4 seeds                    =  4,000 s
    refinement total                                         = 18,400 s
                                                             =  5.11 h
    TOTAL 22,422 s = 6.23 h against STAGE_A_BUDGET_S = 8 h.
  Stage B: at most 4 * 24 = 96 search cells at
  STAGE_B_CELL_BUDGET_S = 8.5 h, plus one confirmation and one 250 ms
  sensitivity re-run on CONFIRMATION_SEEDS, against STAGE_B_BUDGET_S.

Layer-3 analytic conformance, per family, over a frozen 30-day walk
  realized mean rate vs closed form      within 0.5 percent
  latent X mean vs 1.0                   within 0.5 percent
  latent X variance vs closed form       within 2 percent
  family 2 realized quiet occupancy vs q within 1 percent
  family 4 realized count rate per open hour vs baseline expectation
                                         within 1 percent
  family 4 first-cell-after-reopen count vs the mean of the next four
    cells                                within 5 percent
  every integrated family: calendar snaps EXACTLY ZERO

GRID_EQUIVALENCE (9.4, 1 s against 250 ms, selected point). Revision 2
required "every Stage A and Stage B gate statistic" to agree while
listing only four; the required comparisons are now enumerated and the
list IS the requirement:
  A1 support verdict, both limbs               IDENTICAL
  B2 conditional adequacy verdict per cell     IDENTICAL
  B2 count-substitution closure value          within 5 percent
  every hourly mean parents per minute (A2/B6) within 1 percent
  every hourly 1 s zero-count fraction (A3/B7) within 2 percent
  every hourly 60 s and 300 s robust_scale     within 2 percent
  B4 minute-range p99, p99.9 and per-seed max  within 2 percent
  L_comp                                       within 5 percent
  reported 60 s Fano and count-p99 log ratios  within 0.05 absolute

Transcript contract (layer 2), per family
  parameter point: the coarse grid cell nearest the family's domain
    centre, stated explicitly in the fixture's own header
  seed 201; initial state from the family's frozen initialization
  exposure: the section 8 contract, first 10,000 resolved parents
  serialization: parent_ts_ns as u64, child_count as u32, latent_x as
    the raw u64 bit pattern of its f64 value, never decimal

Legacy byte-identity walks (B1): symbol in
  BTCUSDT, ETHUSDT, SOLUSDT, MES, MNQ; seed 7; length 2d;
  gen --type trades (the raw tape, byte-complete - bars cannot prove
  tape identity); the committed anchor. Exact commands in brick S.
```

## 17. Stopping rule

Out of scope, named and excluded rather than deferred: any change to
`notes/protocol-12a-measurement-spec.md`, its ladder, bins, floors,
estimators or refusal semantics; any change to the price path, GARCH,
size, level, bounce or quote machinery; `vol_scalar` and the volatility
refit; the crypto presets and any re-bless of their tapes; the ES/MES
corpus and any purchase decision; the reopen-gap limitation and the
fanout-capacity investigation; the `--fit` and `--fit-markov` modes of
`cadence-feasible`; and multi-instrument generalization - this landing
resolves the arrival seam per instrument, which is what makes the next
instrument cheap, but it fits MNQ only.

If implementation proves a frozen constant, family, gate or statistic
unmeasurable, that brick FAILS and stops. A reviewed amendment restarts
Brick F before implementation resumes. No artifact may be produced
under a partially amended contract.

## 18. What the review rounds settled

Ten revisions went to codex; the first nine were refused, with 9, 8, 6,
5, 5, 3, 5, 3 and 3 findings. The per-round blocker lists are in git
history and in the session transcripts; carrying 500 lines of them
forward would be dead weight for an implementer. What survives is the
set of RULINGS that still bind, each of which corrected a real defect:

- **The MMPP must use the exactly sampled CTMC transition**
  (`P(q->a) = (1-q)(1-exp(-dt/tau))`), whose stationary quiet occupancy
  is exactly `q` for every `dt` and `tau`, and `tau` is the CORRELATION
  time `1/(alpha+beta)`, not either state's mean dwell. The naive
  per-state form does not have the stationary law its normalization
  assumes.
- **Family 4 is not a Hawkes process** and must not be called one. Its
  mean preservation is proved by INDUCTION from `A_0 = 1`, not by a
  stationarity argument under a time-varying baseline, and it is exact
  only in the continuous model: child-burst dead time (order 1e-5, from
  the real preset constants) and nanosecond rounding both break it in
  the implementation, so the assurance is the 1 percent realized-count
  conformance gate.
- **Stage A's predicates must be exactly the frozen 12a rules**, which
  are TWO different rules - nonzero generated support for every
  observed-positive bin at every hour, plus the 30-minutes-per-seed
  floor only at `FAIL_HOURS_300` required bins. A stronger screen
  rejects cells Stage B would accept.
- **Do not gate on the Fano or count-p99 bands.** They are ladder
  inputs; forcing them inside the band would forbid a legitimate
  unamended-ladder outcome.
- **Family 1 and Legacy cannot go through the kernel**, because
  `begin_event` draws price and book between the arrival flip and the
  child count, and a kernel owning the child count draws it too early
  for any branch sharing the main RNG. Hence the two stochastic
  contracts of section 7 and family 1's one-parameter scope.
- **The calendar belongs inside the integrated intensity**, not in a
  post-hoc snap. A snap concentrates closed-window candidates onto the
  reopen instant, which corrupts the self-exciting cell accounting.
- **Seed holdout is only real if nothing fits or decides on the
  confirmation seeds**, including the negative control, which is why it
  has its own fit and test seeds.
- **A capped search cannot claim a region-wide verdict**, hence
  `no-feasible-cell-among-evaluated-cells`.
- **Every gate needs a command that actually runs**, and a gate must
  test the property it claims: bars cannot prove tape identity, and a
  `/health` assertion in a shutdown race is not deterministic.

## 19. Freeze basis, and what it does not cover

FROZEN 2026-08-08 BY OWNER DECISION, not by reviewer signature. The
owner's standing practice is two review rounds before implementation;
this document had nine, which is over-investment the owner called out
directly. Rounds 1 to 3 earned it - they caught the MMPP law, the
Hawkes normalization and the false nesting claim. Rounds 6 to 9 were
largely propagation debt from mid-loop architectural churn, a cost
created by restructuring while drafting rather than one the spec
required.

The last review (session 019fe2cb) did not sign. Its three findings are
folded in above: the B8 selection circularity, the sensitivity-run
count, and the racy `/health` gate. No finding was left open.

WHAT THIS FREEZE DOES NOT COVER, carried forward as implementation-time
review items rather than pretended away:

- the exact fault-to-shutdown handoff and preservation of the `Err`
  through graceful shutdown;
- the startup-fault case, where the tape refuses before readiness is
  emitted;
- merge faults discovered through the default `seek_to`;
- websocket behavior with both reading and non-reading peers;
- the independently derived conformance vectors, especially the reopen
  ordering (V8) and family 1's main-stream draw identity (V4, V5).

Each is reviewed when the brick that lands it is implemented, which is
where the evidence to settle it will exist.
