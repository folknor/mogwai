# Bug hunt: mogwai-data

CITATIONS BELOW TO `notes/bug-loop-carry-forward.md` ARE DEAD LINKS. That file
held the eleven-document arc's round-by-round state and was DELETED when the arc
closed on 2026-08-20, per `AGENTS.md`'s rule that a closed arc folds what still
binds into `AGENTS.md` and deletes its carry-forward. The standing lessons are
there; the deleted text resolves to git history.

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-data`: the `TickSource` seam and `MergeSource`, `GeneratedSource`,
`KrakenCsvSource`, seed derivation, the arrival clock and GARCH machinery, the
fill band, and `TAPE_PROTOCOL_VERSION`.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter read the whole crate (`lib.rs`, `segment.rs`, `trigger.rs`, `bars.rs`
and all of `generated/`) plus the call sites in `mogwai-server` and `mogwai-lab`
the findings depend on. No edits made.

EVERY FINDING IN THIS DOCUMENT IS NOW CLOSED. 1 through 5 had their text removed
by their fix passes, 6 was answered with a measurement rather than a fix - its
section is the evidence, and it carries one FILED item - and 7's seven bullets
were worked in round 3. The sections keep their ORIGINAL numbers, so this
document starts at 6 by design: later briefs and
`notes/bug-loop-carry-forward.md` cite these findings by number, and renumbering
would silently break those citations. What 1 through 5 were, and the corrections
the fix passes made to them:

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
  both `validate`s now refuse it, and both refuse the `side` alphabet too - the
  reader from round 2, the writer from the close pass, which found the alphabet
  enforced on one side only under an argument that applied equally to both.
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

## 6. `ArrivalKernel::next_parent`'s cost cliff, MEASURED

The round-3 fix pass was asked to price the hunter's estimate rather than act on
it, because a jump-ahead rewrite moves RNG consumption and therefore trips the
owner's standing chart gate. THE DECISION THIS MEASUREMENT NAMES: whether to
spend that gate on an arrival-kernel rewrite. The answer from the numbers below
is NO, not yet, and the cheap fix is somewhere else entirely.

Measured 2026-08-19 on the owner's host with a throwaway probe over
`CadenceWalk::next` - the shipped `ArrivalKernel::next_parent` with nothing
attached - at fingerprint-median scalars, a flat session profile and
`mean_event_duration_s` 0.171. Debug and release agreed to within noise on every
row, which is itself informative: the loop is bound by its per-cell RNG draw, not
by anything the optimizer touches.

### The mechanism is real and the arithmetic is right

`limit = from_ns + MAX_SESSION_GAP_NS` is 366 days, `next_segment_end` returns
the next one-second cell while the venue is open, so the budget traversal can run
31.6 million iterations for one parent draw. The hunter's count is correct.

### What it COSTS is 0.66 seconds, not hours

| case | per draw |
|---|---|
| healthy walk, any family, thin 1 | 45 to 50 ns |
| `LiquidityDrought` at its `thin_factor` CEILING of 1000 | about 3 us |
| `LogOuCox` `sigma_y` 8 with thin 1000 | 3.6 ms mean, 115 ms peak, NO refusal |
| full 31.6M-cell traversal | 460 to 660 ms, then `NoOpenExposure` |

`AGENTS.md`'s "a multi-hour computation is presumptively a defect" does not
apply. The cliff is two-thirds of a second, and it is TERMINAL - the traversal
that reaches the limit refuses, the fault latches, and the source is done. It
cannot recur.

### What is reachable, and from what

- NO SHIPPED PRESET DECLARES AN ARRIVAL FAMILY. MNQ, MES and BTCUSDT carry no
  `[generator.arrival]` table, `GeneratorScalars::arrival` is therefore `None`,
  `ArrivalConfig::kernel` is never called, and the integrated path this finding
  is about is not on the serving path of any default run. `mogwai-server`'s
  config loader has a dedicated branch admitting `generator.arrival` as an
  operator OVERRIDE precisely because the seam is absent from every preset. The
  reachable population is operator configs and the lab's own screen.
- `LiquidityDrought` IS NOT THE CAUSE. Its validator caps `thin_factor` at 1000,
  and at that cap a draw costs about 3 microseconds. The hunter listed it as a
  trigger; it is not one.
- THE TWO UNBOUNDED KNOBS ARE THE CAUSE, and both are validator gaps rather than
  kernel defects:
  - `ArrivalConfig::LogOuCox`'s `sigma_y` is checked only for finiteness and
    non-negativity. `x = exp(y - sigma^2 / 2)`, so the latent is unbounded BELOW
    and the exposure per cell collapses. `sigma_y` 8 costs 3.6 ms per draw on
    average, peaking at 115 ms, and keeps succeeding, so unlike the cliff it
    recurs draw after draw; `sigma_y` 12 hits the full traversal and refuses.
    Every
    other family's latent has a floor - MMPP by construction, `SelfExciting` at
    `1 - phi` with `phi <= 0.98`, `ShotNoise` at `1 - m` with `m <= 0.8` - and
    every other family's parameters carry two-sided ranges. `sigma_y` is the odd
    one out.
  - `GeneratorScalars::mean_event_duration_s` is checked only as
    strictly-positive-finite. At 1e4 one draw costs 10 ms; at 1e6 it hits the
    full traversal and refuses.

### The weekend crossing is real and costs about one millisecond

Measured on an MNQ-shaped calendar (one weekly window, minutes 1020 to 8160, so
a 49-hour weekend closure = 176,400 one-second cells): the single draw that
spans it costs 650 us to 1.19 ms depending on family, and three weeks of
walking (7.4 to 7.5 MILLION parent draws) costs about 500 ms in total. The
hunter's
mechanism is right and its magnitude is a rounding error. It is paid once a
week under the river mutex and is not worth a line of code.

### Is there a fix that bounds the cost without moving a byte? Not for the cliff

Stated plainly, because a "no" is the useful answer here. The per-cell RNG draw
inside `advance_state_to` IS the tape: skipping it, batching it, or replacing it
with the closed-form `n`-step transition all change how many values come off the
`ChaCha12Rng` and therefore change every later draw. There is no byte-preserving
O(1) jump-ahead. The one genuinely free win inside the loop is
`baseline_integral`, which the `SelfExciting` arm recomputes per cell and which
consumes no randomness - hoisting it is a constant-factor improvement on one
family and does not change the shape.

### Recommendation

DO NOT spend the chart gate on the jump-ahead. Close the two validator gaps
instead, which is the fix that costs nothing:

- give `LogOuCox`'s `sigma_y` an upper bound, in the two-sided style every other
  family's parameters already use;
- give `mean_event_duration_s` an upper bound in `GeneratorScalars::validate`.

Both are ADMISSION changes. They move no byte of any tape a bounded config
produces - they only refuse, at boot, configs that today produce a 115 ms draw or
a half-second terminal traversal. Filed as an open item rather than landed by
this pass, because refusing a config that currently works is a product decision
and this round's remit was to measure.

If the rewrite is ever revisited, the number to beat is 45 ns per parent on a
healthy walk. The kernel is not slow; it is unbounded at one end of a parameter
range nothing validates.

## 7. Smaller and lower-confidence - CLOSED

All seven bullets were worked in round 3. Six were fixed and one refused; the
resolved text is removed rather than annotated, per the loop's convention. Three
things a later round should know:

- THE `RuntimeModifiers::rate_mult` BULLET WAS THE SERIOUS ONE AND THE SECTION
  HEADING UNDERSOLD IT. The hunter offered it as "may well be intended". It was
  not: measured on the unfixed code, an armed `FlowSurge` at `rate_mult` 8 left
  the self-exciting kernel's mean latent at 123, because the excitement raises
  `x`, a raised `x` draws more parents into the next cell, and those inflate the
  ratio again. Fifteen times what the operator asked for, plus a `tau_s`-decayed
  tail after the divergence cleared, all below `ARRIVAL_X_CEILING` and so refused
  by nothing. `advance_state_to` now takes the multiplier and scales the baseline
  expectation with it. `TAPE_PROTOCOL_VERSION` took 22 for this; no CLEAN tape
  moves, because the new factor is exactly 1.0 under neutral modifiers.
- THE `TriggerToward` BULLET IS REFUSED. The hunter checked the code and found it
  exactly right, which is correct, but the comment is right too. "The TOUCHED
  family" names `ScanKind::TriggerToward` - the touched-ORDER family - and every
  clause after it describes `TriggerToward` accurately. The confusion is that
  `TriggerTouch` is the STOP family despite its name, so "TOUCHED" and "Touch"
  read as the same word. Nothing was wrong; the comment now names the variant
  explicitly so the next reader does not spend the same hour.
- `ScalarError` NOW CARRIES `field` AND `detail`. `field` is a BARE config
  identifier a consumer can match on and `mogwai-server` renders as
  `generator.{field}`; `detail` carries the discriminating prose where one field
  has several ways to fail. Constructed through `ScalarError::field` and
  `ScalarError::detailed`, never as a literal.
  `every_scalar_refusal_names_a_bare_config_field` walks every refusal
  `validate`, `validate_size_grid` and `try_new` can produce - the last of those
  being `top_sizes`, an operator-visible refusal outside either validator's
  reach - and asserts EACH case's field BY NAME. The fix pass first guarded it
  with a `refusals.len() >= 12` floor, which is this arc's signature defect in
  the round's own new test: three mutations could stop refusing, or refuse under
  a different field, and the floor still held. Nothing checked any of this
  before, which is why the floor-branch sentence shipped in a `&'static str`
  field for as long as it did. `mogwai-server` renders the detail AFTER the
  verb rather than inline, so the parenthetical cannot be read as part of the
  config path an operator is about to go edit.

The other four: `MergeSource::starting_at` now positions every child before it
latches a fault, pinned by
`a_faulting_child_does_not_leave_its_siblings_un_seeked`, which asserts on the
CHILD because a merge whose heads were cleared reports `None` either way;
`scan_triggers`' empty-scan branch carries a comment stating exactly what its
asserted `reached_ns` does and does not claim, and what would make it false;
`MIN_VOL_SAMPLES` says it is a sample COUNT; and `CheckpointIndex::coarsen`
drains its excess in one pass instead of `remove(1)` in a loop.

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
At the time of the hunt the live tape identity was 20. Round 1 owed no bump and
took none, round 2 spent 21 on the composer's price and clock rails in findings
3 through 5, and round 3 spent 22 on the FlowSurge repair in finding 7, so
`TAPE_PROTOCOL_VERSION` is 22 and
`TAPE_PROTOCOL_VERSION` next takes 23. `bars.rs` is correct and its out-of-order
contract is deliberate and tested.
