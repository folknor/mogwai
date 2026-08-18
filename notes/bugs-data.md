# Bug hunt: mogwai-data

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-data`: the `TickSource` seam and `MergeSource`, `GeneratedSource`,
`KrakenCsvSource`, seed derivation, the arrival clock and GARCH machinery, the
fill band, and `TAPE_PROTOCOL_VERSION`.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter read the whole crate (`lib.rs`, `segment.rs`, `trigger.rs`, `bars.rs`
and all of `generated/`) plus the call sites in `mogwai-server` and `mogwai-lab`
the findings depend on. No edits made.

## 1. `GeneratedSource::seek_to` spins forever when the arrival kernel refuses (real infinite loop)

`crates/mogwai-data/src/generated/source.rs`, the `seek_to` override:

```rust
if self.pending_quote.is_none() && self.burst.remaining == 0 {
    let mut advanced = self.clone();
    let parent = advanced.advance_parent();
    let parent_end = parent.parent_ts_ns.saturating_add(...);
    if parent_end < start_ts { *self = advanced; continue; }
}
let tick = self.next_tick()?;
```

`advance_parent` calls `begin_event(false)`. On the integrated path,
`begin_integrated_event` handles an `ArrivalRefusal` by setting `self.fault` and
RETURNING WITHOUT TOUCHING `burst`. So `advance_parent` then builds
`ParentSummary` from the STALE burst: `parent_ts_ns` is the previous parent's
timestamp (or `0` on a fresh source, since `SweepBurst::empty()` has
`parent_ts_ns: 0`), and `child_count == 0`. The `while self.burst.remaining > 0`
loop body never runs.

Consequently `parent_end` is a stale or zero timestamp, which is `< start_ts`
for any forward seek, so the loop adopts the faulted clone and `continue`s -
WITHOUT ever reaching `self.next_tick()`, which is the only place `self.fault`
is checked. Every subsequent iteration repeats identically. This is an unbounded
spin holding whatever lock the caller took, which is exactly the failure mode
the `TickSource::seek_to` doc warns callers about and which this override exists
to avoid.

Reachability today is narrow but not zero: `mogwai-server`'s `place_cursor` and
`history_source` both call `seek_to` on a checkpoint-positioned
`GeneratedSource`, and both are preceded by `extend_toward`, which would itself
have stopped short on a faulting lead and returned `None`. So the
deterministic-walk argument mostly saves it. But nothing in `mogwai-data`
enforces that ordering, `seek_to` is a public trait method on a public type, and
the guard costs one line (`if self.fault.is_some() { return None; }` at the top
of the loop, or better - see finding 2).

## 2. `advance_parent` reports a parent that was never produced (frontier-family defect)

The root cause of finding 1, and it bites independently.

`ParentSummary` has no representation for "no parent was drawn".
`advance_parent` is `#[must_use]` and infallible, so on a refusal it hands back
a summary describing a phantom parent - stale timestamp, zero children. Three
consumers act on it:

- `seek_to` - infinite loop (above).
- `CheckpointIndex::extend_toward` - adopts the faulted clone as `self.lead`,
  credits `walked += 1` and `since_snapshot += 1` for a tick that does not
  exist, and can take a `snapshot(false)` of a faulted source. Bounded by
  `max_extend`, so it burns the whole budget and then reports "unreachable"
  rather than "faulted". `CheckpointIndex::fault()` exists and is never
  consulted on that path.
- `mogwai-lab/src/arrival_screen.rs`, `ParentWalk::next`:
  `Self::Generator(source) => Ok(source.advance_parent())`. The `Kernel` arm
  maps `ArrivalRefusal` into a `ScreenRefusal` with the clock as evidence; THE
  GENERATOR ARM STRUCTURALLY CANNOT REPORT A REFUSAL. A screen driven through
  `GeneratedSource` over a config that hits `IntensityCeiling` or
  `NonFiniteState` produces an endless run of phantom zero-child parents at a
  frozen timestamp and calls it data. `arrival_envelope.rs:671` has the same
  shape.

This is the frontier family from `AGENTS.md` verbatim: a cursor advanced over
work whose success the same expression never checked, with the "lookup that
legitimately returns nothing" variant. The fix is structural and pre-1.0-cheap:
`advance_parent` should return `Result<ParentSummary, TickFault>` (or `Option`),
and the three call sites should stop guessing. The hunter would not paper over
it with a fault check in `seek_to` alone - the lab asymmetry is the more
damaging instance and a local guard does not touch it.

## 3. `SegmentSource`'s running price has no floor, no ceiling, and a silent failure mode

`crates/mogwai-data/src/segment.rs`. The composer integrates `price *= ret.exp()`
in `f64` forever, with NO counterpart to the generator's
`(mid * r.exp()).max(tick_f64).min(MID_CEILING)`. Over an endless tape (the
stated design: "never returns `None`... effectively infinite") a run of negative
drift walks the level toward zero and a run of positive drift walks it toward
infinity. Nothing re-anchors it.

Two concrete consequences:

- `emit_price` computes `(level / tick_size).round() * tick_size`. Once the
  running price falls below half a tick, every emitted price is EXACTLY ZERO - a
  non-positive price on the tape. `lib.rs`'s own Kraken parser explicitly
  rejects non-positive prices because "a zero/negative price would poison
  downstream ln-return math with -inf/NaN"; the composer can manufacture them.
  `vol_reading`'s `pair[0] == 0.0` guard turns a whole window into a refusal at
  that point.
- On overflow, `Decimal::from_f64_retain(self.price)` returns `None` and the
  `let else` SILENTLY RETURNS `self.tick_size` as the price - a one-tick print in
  the middle of a runaway tape, with no error, no fault, and no log. The comment
  calls this "unreachable for a finite positive price, which the constructor
  established and multiplication by a finite factor preserves", which is only
  true if you ignore overflow to `inf` (finite times finite can be `inf`) and
  `Decimal`'s roughly 7.9e28 range, far below `f64`'s.

The generator got a floor and a ceiling on exactly this reasoning; the second
tape origin shipped without them.

## 4. `SegmentSource` does not validate the invariants its own conformance fixture declares

`analysis/segment_library_conformance.json` states five `rules`.
`SegmentLibrary::validate` checks three of them (version, non-empty,
parallel-array lengths) and ignores two:

- "`ret[0]` is always 0" - unchecked. This is the rule that makes a seam
  level-continuous: the incoming segment's first return must not move the price,
  because its displacement lives in `open_gap_ret`. A library with a nonzero
  `ret[0]` produces a silent extra jump at every seam, on top of (or instead of)
  the reopen gap, and `reopen_gaps: false` no longer means "no gap at the seam".
  The test `a_seam_without_a_reopen_gap_moves_no_price` only passes because its
  hand-built fixture happens to set `ret[0] = 0`.
- `dt_ns` positivity - unchecked, and the doc comment on `seam_gap_ns` claims
  "one second is enough to keep timestamps strictly increasing". That is only
  true across seams. Within a segment, `dt_ns[i] == 0` yields two ticks with an
  identical `ts_event`. That may be fine for `MergeSource`, but the type's own
  stated property is wrong as written, and
  `timestamps_are_strictly_increasing_across_seams` cannot catch it (all its
  `dt_ns` are 1 ms).

The module's whole justification for the shared fixture is "if the shapes drift,
one side fails on the fixture rather than both staying green" - but a rule the
fixture STATES and neither side CHECKS is precisely the "nothing detects a
missing fixture" hole `AGENTS.md` flags, one level down.

Smaller in the same file: `at_seam` is `true` at construction, so the very first
segment's `open_gap_ret` is applied at tape origin - a gap measured against a
session that isn't in the tape. `side` chars other than `B` or `A` fall to
`NoAggressor` silently rather than being refused at load (`N` is legitimate per
the fixture, but so is a typo). `SegmentSource` overrides neither `seek_to` nor
`fault`, so an endless source inherits the O(distance) default walk `lib.rs`
warns about.

## 5. `SegmentCompose::seam_gap_ns` and `saturating_add` can freeze the clock

`take_seam` and `next_tick` both use `self.ts.saturating_add(...)`. On an endless
tape this is correct until `ts` reaches `u64::MAX`, after which every tick shares
that timestamp forever and the source silently becomes a constant-time stream.
The generator's `low_intensity_gap_ns` has a documented, deliberate answer to the
same question (`MAX_SESSION_GAP_NS` so the clock "advances strictly instead of
freezing"); the composer has none. Low severity - it needs roughly 580 years of
sim time - but it is the same defect the generator was explicitly repaired for,
and the asymmetry is worth knowing about.

## 6. `ArrivalKernel::next_parent` has a cost cliff of roughly 31.6M iterations per draw

`limit = from_ns + MAX_SESSION_GAP_NS` (366 days), and while the market is open
`next_segment_end` returns the next ONE-SECOND cell. So the budget-traversal
loop can run up to 31.6 million iterations for a single parent draw, each
calling `advance_state_to` (which itself walks cell by cell consuming RNG - for
`ShotNoise`, a `Poisson` sample plus per-jump draws each cell) and
`next_segment_end`. It is reached whenever `intensity` stays small relative to
the exponential budget: a thin session profile, a large `thin_factor` from
`LiquidityDrought`, or a latent `x` that has decayed.

Weekend crossings are the tamer version of the same thing: `intensity == 0.0`
jumps the cursor to the calendar boundary in one step, but the NEXT loop
iteration calls `advance_state_to` with the post-weekend cell index, which then
walks roughly 172,800 one-second cells consuming RNG for each. That is arguably
correct (the latent must evolve in wall time) but it is a per-weekend RNG burn
that nothing bounds or documents, and it is paid synchronously under the river
mutex on the serving path.

`AGENTS.md` says "a multi-hour computation is presumptively a defect to optimize
before it is run". The OU, MMPP and shot-noise transitions over a fixed step are
all closed-form over `n` steps (`decay^n`, a single Gaussian with the aggregated
variance, a thinned Poisson) - a jump-ahead would collapse both cases to O(1) at
the cost of a re-bless. The hunter would take that trade.

## 7. Smaller and lower-confidence

- `MergeSource::starting_at` breaks on the first faulting child, leaving later
  sources never seeked. Harmless today because `latch_fault` clears all heads,
  but the loop's `break` and the fill-with-`None` are two mechanisms doing one
  job; if anyone ever makes a fault recoverable, the un-seeked tail is a
  landmine.
- `scan_triggers`' empty-scan branch returns `reached_ns: to_ns, drained: 0` -
  it claims a fully drained horizon without reading a tick. Correct given no
  scans consume the frontier, but it is the only place in either walk where
  `reached_ns` is asserted rather than proved, and the two walks otherwise go to
  great lengths about exactly this. Worth a comment at minimum.
- The `TriggerToward` prune comment is wrong about itself. Lines around the
  `toward_buy_max` and `toward_sell_min` declarations say "The TOUCHED family
  opens in the opposite direction to the stop family" while describing the
  `TriggerToward` slots. The hunter checked all six (kind, side) bounds against
  `trades_through`, `touches_trigger` and `touches_toward` - THE CODE IS EXACTLY
  RIGHT, and the pre-filter is sound (`keeps_max` matches each predicate's open
  direction, and the `<=` vs `<` strictness split is genuinely why `toward_*`
  cannot share the `fill_*` slots). Only the prose mislabels the family.
- `MIN_VOL_SAMPLES`' doc comment is garbled: "Returns below which a reading is
  REFUSED rather than reported" - it is a minimum COUNT, not a return threshold.
- `ScalarError { field: "children_single_frac (floor-branch active solve infeasible)" }`
  stuffs a sentence into a `&'static str` field that every other variant uses as
  a bare field name. Any consumer switching on `field` will not match it.
- The self-exciting feedback ignores `RuntimeModifiers::rate_mult`.
  `advance_state_to` computes `expected` from `env.baseline_integral(..)`, which
  has no surge term, while the observed `cell_count` counts parents drawn WITH
  the surge applied. An armed `FlowSurge` therefore inflates `observed /
  expected` and the kernel amplifies itself on top of the operator's multiplier.
  That may well be intended (a surge is real flow, and real flow excites), but
  nothing says so, and it is the kind of thing the divergence-is-an-output-
  envelope principle in `consts.rs` argues against everywhere else.
- `CheckpointIndex::coarsen`'s `while len > MAX { remove(1) }` is O(n) per
  removal under pin pressure. Immaterial at 4096, noted only because it is the
  one unbounded-shift loop in the file.

## What the hunter checked and found sound

The two frontier walks in `trigger.rs` (`scan_triggers` and `vol_reading`) are
the best code in the crate - the "an instant is only drained once an event with
a later timestamp is seen" rule is applied consistently in both, the
budget-exhaustion and source-exhaustion cases are distinguished correctly, and
the tests bite on the real failure
(`walk_pulls_exactly_one_event_past_the_boundary_to_prove_it` explicitly trades
efficiency for the exclusive-frontier semantics). The `ReopenGap` frontier
invariant holds in both the integrated and non-integrated paths - the hunter
traced the `reopen_frontier_ns` and `at_ts_ns.max(clock+1)` interaction looking
for a way to trip the
`expect("arrival kernel and regime disagree about reopen crossing")` panic and
could not construct one, because `RegimeState::new` drops an already-elapsed arm
and every later frontier advance either consumes the arm or leaves it strictly
ahead. `SweepShape`'s `single_frac >= 1.0` division-by-zero is correctly
special-cased in `begin_event` and unreachable from `next_count_scaled`.
`TAPE_PROTOCOL_VERSION` is 20, `AGENTS.md` says next takes 21, and no markdown
carries a stale live-identity claim. `bars.rs` is correct and its out-of-order
contract is deliberate and tested.
