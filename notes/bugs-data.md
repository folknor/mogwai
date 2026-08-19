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

FINDINGS 1 THROUGH 5 ARE CLOSED and their text removed; the surviving sections
keep their ORIGINAL numbers, so this document starts at 6 by design. Later rounds'
briefs and `notes/bug-loop-carry-forward.md` cite these findings by number, and
renumbering would silently break those citations. What they were, and the
corrections the fix passes made to them:

- 1 was `GeneratedSource::seek_to` spinning forever on a refused arrival draw,
  and 2 its root cause - `advance_parent` was infallible, so a refusal returned
  a PHANTOM parent (the stale burst's timestamp, zero children) that three
  consumers then advanced over. `advance_parent` now returns
  `Result<ParentSummary, TickFault>` and every consumer reports the refusal.
- THE REPORT OVERSTATED THE LAB HALF OF 2. It said the two `mogwai-lab`
  consumers produced "an endless run of phantom zero-child parents". Neither
  looped: both walk under a stall guard that refuses on a non-advancing
  timestamp. The real damage there was MISATTRIBUTION - a refused cell reported
  as "candidate walk stalled", aborting the whole run instead of naming the
  refusal and its clock. The structural fix stands on that ground; the
  infinite-loop claim was true of `seek_to` only.
- 3 was the composer's unbounded running price. Both halves reproduced; the
  running level now clamps to the generator's `[tick, MID_CEILING]` band from
  the same constant, and `emit_price` panics rather than silently printing one
  tick. THE REPORT HAD THE CEILING'S MECHANISM WRONG: a rising walk never
  reaches `from_f64_retain`'s `None`, because `level / tick_size` overflows
  `Decimal` around 1.98e28 first and PANICS there. The silent one-tick print is
  real code and unreachable that way; the reachable damage was the panic.
- 4 was HALF RIGHT. `ret[0] == 0` is a fixture rule neither side checked, and
  both `validate`s now refuse it (plus the `side` alphabet, on the reader side).
  `dt_ns` POSITIVITY IS NOT A FIXTURE RULE and must not become one: two prints
  at one nanosecond are ordinary in a swept book, `mogwai_lab::segments` records
  the difference verbatim, and refusing a zero would throw away real sessions.
  The defect there was the `seam_gap_ns` doc comment claiming strict increase;
  it now says non-decreasing and says why. The origin-is-a-seam sub-item was
  real and is fixed.
- 5 was real and UNDERSTATED. The report priced it at 580 years of sim time, but
  `--start` is a raw `u64` an operator types, so a near-max value froze the
  clock on the first command. The composer refuses instead of saturating, and
  `SegmentSource::clock_exhausted` names the one terminal condition it has.
  The `seek_to` / `fault` sub-item of 4 is NOT closed; see `notes/todo.md`.

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
`TAPE_PROTOCOL_VERSION` is 21, `AGENTS.md` says next takes 22, and no markdown
carries a stale live-identity claim. `bars.rs` is correct and its out-of-order
contract is deliberate and tested.
