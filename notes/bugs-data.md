# Bug hunt: mogwai-data

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope: findings 1 and 2 are extended from the server side in
`bugs-server-tape.md` findings 6 and 2. The `BoundedSeek` finding was reported
independently by both hunters.

## 1. CLOSED (`bugs-server-tape.md` round 2 plus its review pass)

`FlowSurge` is no longer per-clone state that only the tape worker sees. The
paced feed advances `CheckpointIndex`'s own lead, arming goes through the index
under its lock, and the arm and the clear each retain a PINNED control-boundary
snapshot, so every target after an arm resolves to a snapshot that carries the
surge and replays it. `coarsen` is forbidden from dropping a pinned snapshot,
and the last-print walk-back is fenced at one, so neither the memory ceiling nor
a lookup retry can reopen the fork. `checkpoint_flow_surge_is_visible_to_canonical_and_history_walks`
pins it through `try_source_at_or_before`, i.e. through the path a history
request actually takes, and fails when the boundary snapshot is removed.

Residual, bounded and documented rather than open: a run that arms more than
`MAX_CHECKPOINTS` surges drops its OLDEST boundaries to keep the memory ceiling,
so replay fidelity for the most ancient surge windows is what gives way rather
than the bound.

## 2. CLOSED (`bugs-server-tape.md` rounds 1 and 2, findings 2 and 8)

All four dangling `BoundedSeek` references are gone from the tree (the two in
`checkpoint.rs`, `reference/performance.md`, and `fills.rs`), and the safety
mechanism the docs now describe actually exists: `try_source_at_or_before`
REFUSES a target its capped extension did not reach, before any downstream seek
begins, so no unbounded `seek_to` can be entered against an unreachable target.
The server's last-print fallback additionally shares the sweep drain ceiling.
The `TickSource::seek_to` trait doc's "the server's checkpointed seek does
this" is now a true statement about that refusal boundary.

## 3. An armed `ReopenGap` is silently swallowed when the crossing lands in a calendar-closed window

`begin_event`:

```rust
let old_clock_ns = self.clock_ns;
self.clock_ns = self.clock_ns.saturating_add(dt_ns);
if let Some(reopen) = self.regime.take_reopen_crossed(old_clock_ns, self.clock_ns) { ... }
if let Some(calendar) = &self.calendar && !calendar.is_open(self.clock_ns) {
    self.clock_ns = calendar.next_open_ns(self.clock_ns);   // clock jumps AFTER the crossing test
}
```

`take_reopen_crossed` fires on `old_clock < at_ts && at_ts <= new_clock`. If `at_ts`
sits inside a closed window that the calendar jump steps over, the pre-jump
`new_clock` was short of `at_ts` so the test fails; on the next event `old_clock` is
already past `at_ts`, so the first conjunct can never be true again. The halt stays
armed forever and never fires - exactly the silent-inert failure mode
`RegimeState::new` goes to great length to prevent for an already-elapsed `at_ts`,
reproduced one layer down by the calendar. Nothing warns. Fix: run the crossing test
against the post-jump clock, or test the closed interval
`[old_clock, post_jump_clock]`.

## 4. `symbol: String` is cloned per materialized tick

MOSTLY CLOSED by `bugs-server-tape.md` rounds 1 and 2 (findings 3 and 4a): the
checkpoint stride is 8,192 again (not 4,194,304), so the per-request residual is
bounded by thousands of ticks rather than millions, and `GeneratedSource` now
overrides `seek_to` to skip whole parents through `advance_parent` without
materializing protocol objects, with a continuation golden pinning identical
draws. `CheckpointIndex::extend_toward` uses the same compact step.

What remains open is the allocation on the ticks that ARE materialized:
`next_child` does `symbol: self.scalars.symbol.clone()` per tick, and an
`Arc<str>` (or interned symbol) would remove the crate's most frequent heap
allocation on the feed and history paths.

## 5. `low_intensity_gap_ns` doc claims a strict-monotonicity invariant no test holds

The doc says a saturated cast pins the clock "breaking the strict monotonicity
`monotonic_clock` pins". `monotonic_clock` (`tests.rs`) asserts
`tick.ts_event() >= prior` - non-strict. A clock pinned at `u64::MAX` passes it. So
the guard's stated regression test would not catch the regression it describes.
Separately, a quote and its parent's first child legitimately share `ts_event` (both
at `burst.parent_ts_ns`), so strict monotonicity is not even the right invariant to
claim; `trigger.rs` is built around ties existing. Either tighten the test to
something real (e.g. strict monotonicity between parents) or stop asserting a
property nothing checks.

## 6. `try_build` never validates the `SessionCalendar` it accepts

`GeneratedSource::try_build` exists specifically so a caller with untrusted config
gets a `GeneratedSourceError` rather than a panic. It validates `scalars` and
`session`, but takes `calendar: Option<SessionCalendar>` and never calls
`SessionCalendar::validate()`. `GeneratedSourceError` has no calendar variant at all.

A calendar with `open_windows: []` deserializes fine, makes `is_open` always false,
makes `next_open_ns` return `u64::MAX`, and pins `clock_ns` at `u64::MAX` on the
first event - every tick thereafter carries the same timestamp. `SessionModulator::new`
also degenerates (`open == 0.0` -> normalizers 1.0). This is the exact class the
`try_*` family was built to catch, and the calendar is the one input that walks
straight past it. Add a `GeneratedSourceError::Calendar` variant and call
`validate()`.

## 7. The declared child-count mean identity silently breaks for `children_mean < 5`

`consts.rs` asserts the identity `quiet_frac * QUIET + (1 - quiet_frac) * ACTIVE == 1`
"preserves the DECLARED unconditional mean exactly". `begin_event` builds the
per-event shape from `children_mean * ARRIVAL_QUIET_CHILDREN_MULT` (0.20), and
`SweepShape::new` clamps that to `1.0 + f64::EPSILON`. The clamp binds whenever
`children_mean < 5.0`.

At the shipped anchor (8.49) the quiet mean is 1.698 and the clamp is inert, as
`dynamics.rs` claims. But `GeneratorScalars::validate` only requires
`children_mean > 1.0`. An operator preset at, say, 2.0 gets: quiet mean clamped to
`1+eps`, `m` degenerate so every quiet event emits exactly one child, and a realized
unconditional mean of `0.35*1 + 0.65*2*1.4307 = 2.21` against a declared 2.0 - plus
`state_single_frac` for the active branch driven to the `.max(0.0)` floor. Nothing
errors, nothing diagnoses. Either validate
`children_mean * ARRIVAL_QUIET_CHILDREN_MULT > 1.0` as the mechanism bound it
actually is, or emit a `ScalarDiagnostic`. Currently the constant block's central
claim is conditionally false and the condition is unstated.

## 8. `MergeSource` documents a heap it does not have

`lib.rs` - "a heap of per-source heads yields a global ordering". `next_tick` is a
linear `min_by_key` scan over `heads`. Fine at k=1 (the only production shape), and
the tie-break is deterministic by index, which matters; but the doc is wrong. Given
production runs k=1 and the type exists solely as a one-tick pushback buffer (per the
essay in `source.rs`), delete `MergeSource` and name the thing what it is: a
`Peekable`-style one-tick pushback over a `TickSource`. That also removes a
`Vec<Box<dyn TickSource>>` and a `Vec<Option<TickEvent>>` from every history request.

## 9. `TAPE_PROTOCOL_VERSION`

No bump is owed. The working tree contains zero Rust changes and does not touch
`analysis/fingerprint.json`; the diffs are `.review.toml`, `mogwai.toml`
(`fanout_depth` 65536 -> 1048576, a broadcast-ring capacity that never reaches the
generator), two markdown files, and untracked `analysis/` Python.
`notes/sampling-frame-preregistration.md` already states this explicitly.
`reference/architecture.md` says version 9, matching `lib.rs`.

## Smaller items

- `SessionCalendar::settlement_instants` iterates every minute in `[from_ns, to_ns]`
  calling `is_open` per minute. Over a span of days that is fine; over a from-origin
  span it is millions of iterations. It should step day-by-day from the first
  local-midnight crossing rather than scanning minutes.
- `PublishedBook` carries `TopOfBookSizes`, which carries a `CalibrationProvenance`
  that can hold a `String`. `place_book` clones it on every parent event and
  `begin_event` clones `last_book` again on a compatible repeat. Today all presets
  are `Uncalibrated` so no allocation happens - the moment anyone fits `top_sizes`,
  this becomes two heap allocations per parent event on the hot path. Provenance is
  calibration metadata; it does not belong in a per-event runtime value.
- `SweepShape::new` is re-solved per event (five floats plus two `ln` calls). The
  comment justifies it by `FlowSurge` possibly scaling `children_mean`. It could be
  cached and invalidated on surge arm/clear; minor, but the `ln` pair is not free at
  50 events/s times multi-million-tick drains.
- `TickRuleAggressor` keys a `HashMap<String, _>` and clones the symbol on every
  trade. Unbounded growth is bounded in practice by symbol count, but it is another
  per-tick allocation.
- `checkpoint.rs` retains up to 4096 full `GeneratedSource` clones, each duplicating
  the immutable config (`GeneratorScalars` with its `String`s, `SessionCalendar` with
  its `Vec`, three distribution objects, three 24/7-element arrays). The mutable walk
  state is a few hundred bytes; everything else should sit behind one `Arc` shared by
  the lead and all snapshots.
