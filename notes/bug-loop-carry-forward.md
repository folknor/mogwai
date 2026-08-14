# Bug-loop carry-forward

Machinery the bug-hunt loop has put in place, which later rounds may build on
and must not break. Each brief carries the relevant slice forward, because no
agent in the loop can see any round but its own. Transient by folder, but it is
live state for as long as the arc is running.

## notes/bugs-engine.md, round 1 (commit 53dc693, findings 1 through 6)

Machinery introduced:

- `apply_divergences` is threaded through `commit_fill`, `validate_fill_funds`,
  `maximum_commission` and `validate_submit`. When it is false the caller is
  venue-originated (liquidations), the fee surcharge multiplier is
  `Decimal::ONE`, and the surcharge window is NOT mutated. Any new path that
  reads a divergence must decide, explicitly, which side of this flag it is on.
- `Engine::order_reservation`, returning a new `Reservation` enum in
  `account.rs`, is the SINGLE derivation of an order hold. It consults the
  instrument class before the margin map. `held_for` and the `locked_balances`
  order loop are both thin wrappers over it, and the `locked_balances` position
  loop is now gated on `is_future` too. Do not reintroduce a second
  hold calculation; this helper is what closed finding 6, whose whole shape was
  two implementations of the same branch drifting apart.
- `fee_surcharge_multiplier_at` is the PURE reader of the surcharge window and
  is what every check path uses. `commit_fill` is now the only mutating reader
  left, which is half of finding 9's last bullet.
- Funded accounts cover commission at three check layers: submit, amend and
  fill. The amend requirement adds the maximum of the maker and taker
  commission; a spot sell carries a separate settlement-side commission check
  because its hold is base currency.

Decisions already ruled on, not to be silently relitigated:

- Findings 1 through 6 are closed. Finding 6 was closed by CONSTRUCTION (the
  shared helper), not merely by the observation that `mogwai-server`'s
  `validate_instrument_options` rejects Spot-plus-margin at boot - that
  observation was too weak, because `Engine::set_margin_policy` is `pub` and
  bypasses config validation entirely.
- Round 1 owed NO `TAPE_PROTOCOL_VERSION` bump. Nothing it touched reaches
  `mogwai-data`, the fill-band draw or seed derivation; the new post-only amend
  check computes a trigger from a local `band_draw + 1` and stores nothing, and
  the amend's own redraw afterwards uses identical inputs.

Disclosed residual, deliberately left:

- The submit-side commission charge is not independently observable by test.
  `on_submit` runs `validate_fill_funds` on every submit before an order rests,
  and both layers emit the identical `insufficient <currency> balance` message,
  so the two checks are mutually redundant. Reverting either one alone leaves
  the suite green; reverting both together breaks it. The fill-time check is
  what actually holds the invariant, and the submit-side charge is
  belt-and-braces. A later round that wants these independently pinned needs
  distinguishable refusal reasons first, not a cleverer test.

Known live wrongness fixed in passing, worth not undoing:

- `held_for` used to return a base-currency quantity for spot sells and hand it
  to a settlement-currency comparison. `Reservation::Base` types that away.

## notes/bugs-engine.md, round 2 (findings 7 and 9)

Machinery introduced:

- Fill-band draw keys normalize decimal prices before hashing. Economically
  identical prices with different trailing-zero scales now share a trigger and
  slippage draw. This moved the fill-draw identity, bumped
  `TAPE_PROTOCOL_VERSION` from 12 to 13, and re-blessed committed artifacts
  carrying that identity.
- Venue-order, trade, hedging-position and liquidation ids have independent
  saturating counters. Client order ids beginning with the venue-reserved
  `LQ-` liquidation prefix are rejected, while venue-originated liquidation
  submits bypass that client-only rule.
- A successful cancel consumes `DropNextAccountUpdate`, because freeing its
  reservation changes the account snapshot. A resting acceptance still leaves
  the arm untouched for the later fill, preserving round 1's rule.
- A hedging reduce-only submit without `position_id` is rejected. Hedging can
  hold several independently keyed and opposing positions, so there is no
  unambiguous "reduce whatever I have" target. `docs/oms-types.md` states the
  contract.
- Fee-surcharge applicability is a pure simulated-time lookup in validation
  and booking. A fill after the window cannot erase the answer for a replayed
  timestamp inside it; venue-originated fills still bypass the surcharge.
- A swept partial that floors below one size increment advances its scan
  frontier without incrementing `band_draw` or redrawing its trigger. Only a
  nonzero execution starts a new tranche and queue-position draw.

Decisions already ruled on, not to be silently relitigated:

- Finding 8 remains wholly fenced for round 3. Round 2 did not change the open
  book representation, reservation structure, lookup strategy or funds-path
  complexity.
- Missing `position_id` on a hedging reduce-only order is an invalid ambiguous
  request, not shorthand for reducing an arbitrary or aggregate position.
- `DropNextAccountUpdate` is defined by ORDER TRANSITION, not by fill. It is
  spent on the snapshot after an order executes or leaves the book - fill,
  client cancel, funds-check eviction, a stop trigger that booked either - and
  is deliberately NOT spent on an order coming to rest, even though the
  reservation moves `locked`. That single carve-out exists because acceptance
  always precedes the fill, so an arm consumed there could never reach what the
  scenario author aimed it at. `control.rs`, `docs/havoc.md` (both the prose
  and the conditional-order table row) and `on_cancel`'s comment all state this
  rule; they used to say "fill", which had been false of `on_trigger` and
  `apply_scans` since before this loop began. If a later round moves a
  consumption site, move all four statements with it.

Re-blessing, verified rather than assumed:

- `analysis/stage-a-batch-manifest.json` is BOUND: `stage_a_batch.rs` refuses a
  manifest whose `tape_protocol_version` is not the live constant, and the
  committed-manifest test both recomputes `plan_sha256` and re-derives the
  whole manifest from the committed pilot with `build_manifest`. The new hash
  is therefore a regeneration, not a literal typed to match.
- `analysis/mnq-arrival-screen.json` is bound by
  `the_screen_artifact_carries_every_evaluated_cell_and_its_verdict`, which
  asserts the artifact's binding block equals the live constant. Its search
  content is untouched and unaffected: the fill band is engine-side and the
  screen evaluates arrival kernels. Nothing was widened into a tolerance
  anywhere to keep an old blessing alive.
- Residual, cosmetic: the round-2 edit of the screen artifact added a trailing
  newline the generator does not write. One byte, no gate reads it.

## notes/bugs-engine.md, round 2 review pass (P1 through P3 of the cold read)

- `apply_scans` no longer resets `scanned_ns` to `ts` on a zero-quantity
  result. The frontier set from `result.scanned_to_ns` earlier in the loop
  stands, so a drain budget that truncated the walk loses no span. Only a real
  execution earns the reset, because only a real execution opens a tranche that
  covers from `ts`. Both directions are pinned by test.
- The `DropNextAccountUpdate` widening above is prose only; no consumption site
  moved. The cancel behaviour round 2 introduced was already the engine's rule
  everywhere else and is kept.
- The bump's durable prose is now consistent across
  `mogwai-data/src/lib.rs`, `arrival_control.rs`, `docs/cli.md` (two places),
  `reference/architecture.md` and `AGENTS.md`. `architecture.md` had been stale
  at 11 since before the loop started. Every one of them now says the
  protocol-12b MECHANISM landing takes 14.

## notes/bugs-engine.md, round 3 (finding 8)

Machinery introduced:

- Resting orders keep vector storage and a client-id index resolves cancel,
  modify and scan-result lookups in constant time. Removal uses `swap_remove`.
  Wire event and snapshot orderings sort explicitly, and pending scans recover
  acceptance order from `ts_accepted` and the numeric venue-order sequence.
- `order_locked` incrementally aggregates only the resting-order component of
  locked funds. `free_balance` reads that currency directly and folds the much
  smaller position-maintenance component without allocating a map. Snapshots
  clone the aggregate once and add position maintenance. Every cache add,
  remove, amend refresh and reconciliation still calls
  `Engine::order_reservation`, the single hold derivation from round 1.
- All book mutations go through `rest_open`, `take_open` or
  `refresh_open_reservation`. This covers ordinary rests and cancels, partial
  leaves changes, reprices, trigger promotion and cancellation, funds-check
  eviction, and venue-originated liquidation remainders. Replacing a public
  margin policy rebuilds the aggregate through the same derivation.
- A fresh full-book fold reconciles the aggregate at every command and sweep
  boundary under `cfg!(debug_assertions)`, and PANICS on drift. A saturated
  currency is rebuilt on removal because subtracting from `Decimal::MAX`
  cannot recover an overflowed sum.

Decisions:

- The proposed combined order-plus-position cache was narrowed deliberately.
  Position maintenance is not book-depth-dependent, while caching it would add
  every fill and policy mutation to the silent-drift surface. The implemented
  split removes the O(results * open-orders) funds path without taking that
  extra invariant risk.
- A plain unordered keyed map was rejected because internal iteration can
  reach scan and event order. The vector plus index keeps deterministic
  metadata explicit; O(1) swap removal is safe only because consumers sort
  before emitting.
- No `TAPE_PROTOCOL_VERSION` bump is owed. Tape generation, fill draws, ids and
  event ordering are unchanged; the container swap is hidden behind explicit
  deterministic ordering.

Verification:

- The drift test deliberately corrupts the cached USDT hold and proves the
  next funded command's reconciliation panics in debug. Removing command
  reconciliation made that test fail because no panic occurred.
- The keyed-book test removes the first of three orders, amends the order moved
  into its slot and verifies the deterministic query snapshot. Removing amend
  cache refresh made it fail on the reconciliation's 200-versus-300 USDT
  mismatch.

## notes/bugs-engine.md, round 3 audit pass

The cold read of the round-3 diff found no correctness bug. The audit found
one, plus three smaller items, all fixed in the same commit.

- THE RELEASE RECONCILIATION IS GONE. It reinstated the exact cost finding 8
  removed: a full fold of the book, deriving every reservation and allocating
  a currency `String` and a `HashMap` per order, twice per command and twice
  per sweep batch, on any engine with `enforce_funds` set - which is every
  funded venue, the only configuration that ships. No benchmark could see it,
  because `fill_bench`'s `scans` engine seeds no balances and is therefore
  unfunded; the blessed table measured a path the shipped venue does not take.
  Reconciliation is now `cfg!(debug_assertions)`-only and PANICS rather than
  repairing, per the standing line that an invariant wants an assert, a type
  or a guard and not a silent verification. Release correctness rests on
  construction: `OpenBook`'s storage is private and the three cache-aware
  mutators are the only way to move a reservation input.
- The drift test FAILED under `brokkr test`, which is release. Verified by
  running it: `test did not panic as expected`. `brokkr check` runs the suite
  in dev, so the gate was green and the hole invisible. The test now carries
  `#[cfg(debug_assertions)]`, so the release sweep skips it instead of failing
  it. THE GENERAL LESSON, worth carrying past this loop: a test that pins a
  `debug_assertions` behaviour must be gated to that profile, because the two
  brokkr entry points disagree on profile.
- `pending_scans` sorted with a comparator that did two hash lookups and two
  integer parses PER COMPARISON, on the sweeper's hottest call. The acceptance
  key is now decorated once per order and `sort_by_key`ed. `venue_order_sequence`
  is a total order in practice: every venue order id is `V-{n}` from one
  monotonic counter, so the `u64::MAX` fallback is unreachable and no tie can
  fall through to the unstable slot order.
- The stop-limit not-marketable arm wrote `self.open[pos] = order` directly,
  bypassing the cache. It moves no hold today - the arm touches `resting`,
  `scanned_ns` and `revision`, none of which the reservation derives from -
  so it was not a live defect, but it was the one book write not going through
  a cache-aware helper. It now refreshes.
- `locked_balances` handed the saturation warning an unordered `HashSet` drain.
  Sorted.

Verified rather than assumed:

- Mutation sites enumerated from the code, not the claim. `self.open` is
  written at exactly the sites above plus `rest_open` / `take_open` /
  `refresh_open_reservation`; the two remaining `IndexMut` writes touch only
  `scanned_ns` and `revision`. `self.instruments` is never mutated after
  construction and `self.margin` only through `set_margin_policy`, which
  rebuilds - so no reservation input moves behind the cache's back.
- NO `TAPE_PROTOCOL_VERSION` BUMP IS OWED, and the reason is stronger than
  "ordering is sorted": `mogwai-engine` does not reference the constant and
  reaches no tape generation path at all. Fill draws are keyed by order and
  band draw, not by sweep sequence, and venue order ids are assigned at submit.
  14 stays reserved for protocol-12b.
- Bite-checked both negative controls rather than trusting them. Removing the
  amend refresh fails `keyed_book_index...` on the reconciliation assert;
  the drift test passes in dev and does not exist in release.
- The position fold stays out of the cache and the boundary holds. Position
  count is bounded by instruments times position keys and is independent of
  book depth, so `free_balance`'s remaining loop is not where the next O(N)
  hides.

## Close pass over the whole arc (commits 53dc693, 4b86da4, 7135148)

A whole-arc review focused on the never-cold-reviewed second halves of each
round. The reservation cache's mutation discipline, the funds checks, the
sweep frontier handling, the id counters and the durable prose all held up.
Two corner defects found and fixed, each pinned by a test verified to fail
against the reverted fix:

- `order_reservation_entry` now treats a ZERO hold as no cache entry at all.
  A margin policy with `initial_per_contract` of zero made the reconciliation
  fold insert a zero-amount currency key per zero-hold order while the
  incremental remove deleted the key the moment its total hit zero, so
  cancelling one of two zero-hold orders left the cache and the fold
  disagreeing about key EXISTENCE while agreeing on every amount - a spurious
  debug reconciliation panic on economically identical states. Release
  behavior never differed.
- `on_submit`'s tail consumed `DropNextAccountUpdate` even when `last_qty`
  was zero (a `PartialFillNext` floored below one size increment on a
  marketable limit), i.e. on an order that merely came to rest - the exact
  carve-out the round-2 ruling and all four prose statements protect. The
  consumption is now gated on `last_qty > 0`. The zero-qty IOC cancel also
  stops spending the arm, matching the not-marketable IOC branch: an order
  that never held anything moves no ledger.

Residuals accepted as ruled: the submit-side commission charge stays
observable only jointly with the fill-time check (needs distinguishable
refusal reasons first), and the dev-vs-release profile split between the two
brokkr entry points stands recorded above. The full engine suite was re-run
in BOTH profiles after the fixes.

## notes/bugs-server-tape.md, round 1 (findings 1 through 5)

Classification:

- Finding 1 REPRODUCED. Trigger walks were offloaded, but futures mark and
  settlement reads still ran inline on a Tokio worker. The whole mark and
  settlement read phase now runs as one blocking job. Completion races that
  job and exits without booking late results; the blocking job itself may
  finish after cancellation, as Tokio cannot cancel a running blocking task.
  `MarketReadingCache` also recovers a poisoned lock like the other server
  mutexes.
- Finding 2 REPRODUCED IN ALTERED FORM. `BoundedSeek` was still gone and the
  checkpoint stride had grown to 67,108,864. `CheckpointIndex` now exposes a
  fallible positioning operation and refuses a target its bounded extension
  did not reach. The infallible API panics loudly instead of returning a short
  source that a downstream unbounded seek can walk forever. Server history,
  fill and last-print paths use the fallible operation.
- Finding 3 REPRODUCED IN ALTERED FORM. The density-derived checkpoint stride
  was 67,108,864, not the report's 4,194,304. Density continues to size the
  separate reach and drain ceilings; checkpoint spacing is 8,192 again and is
  documented as the latency/memory tradeoff it actually is. A reshaped
  one-hour `source_positioning` benchmark measured 115.85 ms at 67,108,864 and
  2.184 ms at 8,192, a 53.0x reduction.
- Finding 4 REPRODUCED AS A STRUCTURAL OBSERVATION, NOT AS AN INDEPENDENT
  CORRECTNESS DEFECT. The paced worker and canonical indexed source still walk
  separate clones. Combining them in this round would silently decide finding
  6, because the paced clone carries FlowSurge while history and fills are
  deliberately canonical in the current architecture. The actionable costs
  named by finding 4 were the inline worker stalls and oversized residual,
  fixed by findings 1 and 3. No shared live ring was introduced ahead of the
  fenced FlowSurge ruling.
- Finding 5 REPRODUCED. The process-global index now owns its symbol and
  refuses every other configured or unknown symbol after initialization.

Machinery introduced:

- `CheckpointIndex::try_source_at_or_before` is the refusal boundary for a
  capped extension. Do not replace it in server paths with the infallible
  wrapper or a bare default `seek_to`.
- The process-global checkpoint chain is a `RunIndex` carrying both symbol and
  checkpoints. Profile membership alone is insufficient after initialization:
  a config may know several symbols while one process still serves one tape.
- `run_blocking` is the sweeper's CPU boundary for mark and settlement tape
  reads. Run completion is selected while awaiting it so no result is applied
  after completion.

Verification and protocol ruling:

- Negative controls were exercised. Making `run_blocking` execute its closure
  inline fails `blocking_market_walk_does_not_occupy_the_runtime_worker`.
  Restoring the symbol-less global makes
  `initialized_run_index_refuses_every_other_symbol` fail. Restoring the old
  cache poison behavior makes `market_reading_cache_recovers_after_a_poisoned_lock`
  fail. Removing the short-frontier refusal makes the capped-extension gate
  fail its `None` assertion.
- No `TAPE_PROTOCOL_VERSION` bump is owed. Snapshot frequency and restore
  refusal do not change generator state, draws, emitted ticks, seed derivation,
  or tape origin. Version 14 remains reserved for the protocol-12b mechanism
  landing, and no artifact was re-blessed.

## notes/bugs-server-tape.md, round 1 review and close pass

A cold read of the round-1 diff plus an audit of its claims. The classification
above holds. One correctness defect was INTRODUCED by the round and is fixed
here, and the finding-4 close was too soft and has been reopened in part.

- THE SWEEP FRONTIER ADVANCED PAST WORK NOBODY DID. Moving the mark and
  settlement reads onto a blocking task made them fallible, and the failure was
  converted into a pair of empty vectors; the pass then applied empty marks and
  set `last_swept_ns = to_ns` anyway. `last_swept_ns` is the ONLY record that a
  span is owed settlement - the next pass asks the calendar for the instants in
  `last_swept_ns..to_ns` and nothing looks further back - so a panicking reader
  permanently and silently skipped every settlement its interval crossed. The
  loop now abandons the pass without moving the watermark, through
  `frontier_after`, whose whole job is to be the one place that decision is
  made. Scan results are safe to drop with it: the engine still holds every
  pending scan at its unadvanced `from_ns`. THIS IS THE THIRD TIME IN THE ARC
  that a frontier advanced past unperformed work; treat any watermark
  assignment that is not guarded by the success of the work it covers as a
  defect on sight.
- FINDING 4 IS REOPENED IN PART, as finding 4a in the document. The deferral
  of the live-ring rewrite is correct and belongs to fenced finding 6. But
  finding 4's other half - `GeneratedSource` has no `seek_to` override, so the
  trait default materializes a `Symbol` String and two `Decimal`s for every
  tick it skips - is independent of FlowSurge entirely, and shrinking
  `CHECKPOINT_K` did not touch it. It also governs the boot warmup walk, which
  is `extend_toward` and is not a restore at all. `advance_parent` is the
  mechanism; a golden proving the draw sequence is unmoved is the price.
- THE NO-BUMP VERDICT HOLDS, verified rather than accepted. `CHECKPOINT_K` is
  read at exactly one site, `CheckpointIndex::new`, and the index only decides
  WHICH already-walked snapshot a clone resumes from. `coarsen`'s own contract
  is that dropping snapshots lengthens the residual and never changes emitted
  ticks, and `checkpoint_resume_is_byte_identical` pins that. No seed, draw or
  origin reads the constant. A stride change is free.
- The 53x claim is properly recorded, and `reference/performance.md` now also
  carries what it bought the SUBMIT path, which is much less: the market
  reading went 12.6 ms to 9.782 ms median, because the 300 s volatility window
  walk, not the restore, is the cost. `reference/architecture.md` and
  `notes/todo.md` carried the 12.6 ms figure and are corrected.

Durable prose corrected in the same pass, all of it in the class the
`notes/todo.md` prose-gate item describes:

- `checkpoint.rs`'s `extend_toward` still argued its safety from the deleted
  `BoundedSeek`; that was the third of the three comments finding 2 named and
  the round fixed only two. The identifier now appears nowhere in the tree.
- `tick_composition_ratios.rs` claimed to decide FOUR shipped constants
  including `CHECKPOINT_K`. It decides the three reach ceilings plus
  `fanout_depth`. Checkpoint spacing bounds a residual replay, not a reach,
  which is exactly why sizing it by tick density grew it to 67,108,864. The
  `Baseline::checkpoint_k` field stays as historical data with its proposal
  marked advisory.

Bite-checked rather than trusted, each by reverting the production change and
observing the named failure: the bounded-seek refusal
(`checkpoint_extension_is_capped`), the run-symbol refusal
(`initialized_run_index_refuses_every_other_symbol`) and the new frontier test
(`an_unread_pass_leaves_its_settlement_span_owed`). The symbol test was
REWRITTEN first: it asked for `MES`, which the shipped profile table does not
carry, so it would have passed against an implementation that only checked
profile membership. It now uses a fully resolvable second symbol, so the
refusal can only come from the run index owning its identity. No new test rests
on a `debug_assert`, so none of them vanish under `brokkr test`'s release
profile.

## notes/bugs-server-tape.md, round 2 (findings 4a, 6 through 10)

Classification:

- Finding 4a REPRODUCED. `GeneratedSource::seek_to` now skips whole parents
  through `advance_parent` when the complete parent precedes the target, then
  materializes only the boundary parent. `CheckpointIndex::extend_toward`
  uses the same compact step whenever it does not cross a checkpoint or its
  extension budget. The continuation golden proves identical subsequent ticks.
- Finding 6 REPRODUCED. The paced feed now advances the checkpoint index's
  canonical lead, and FlowSurge is armed on that lead synchronously. Feed,
  history and trigger scans derive from one state. This closes finding 1 of
  `notes/bugs-data.md` outright.
- Finding 7 REPRODUCED IN ALTERED FORM. The lossy channel and unconditional
  acceptance were live. Control now mutates the canonical source synchronously,
  a stopped worker refuses it, and HTTP reports 503. The closure-delay proposal
  was stale because the worker had already drawn the tick it was pacing.
- Finding 8 REPRODUCED. The last-print fallback now shares the sweep drain
  ceiling and refuses rather than walking without a bound.
- Finding 9 REPRODUCED. Admission sizes swept fills at four frames and
  venue-originated submits at five. A whole-batch refusal uses
  `AdmissionSubject::Frame`, so it does not blame an unrelated first order.
- Finding 10's fill-golden condition, underflow and CLI size-grid mismatch
  REPRODUCED and were fixed. The extra parent advance was DEAD as a correctness
  issue. The permanent maximum-surge fixture REPRODUCED and is now explicit in
  report metadata. The zero-stride risk REPRODUCED as a latent invariant and is
  checked before arithmetic. The unresolvable-symbol exit was DEAD in the serve
  path because validation and warmup resolve the symbol before worker spawn;
  the impossible exit now logs. The stale futures mark REPRODUCED and now uses
  an exact-instant last-print read rather than the coarse volatility cache.

Machinery introduced:

- The checkpoint index is the single mutable run-tape frontier. Do not restore
  a private paced clone or a fire-and-forget FlowSurge channel. Pacing calls
  `activate_live` before worker spawn; after that, history may clone the lead
  but cannot extend it and steal unpublished frames.
- `swept_batch_max_bytes` keeps venue-originated submit width separate from
  ordinary swept-fill width.
- Mark price and volatility-band freshness are intentionally separate. Marks
  are exact at the sweep instant; only acceptance-time volatility stays cached.

Protocol ruling:

- No `TAPE_PROTOCOL_VERSION` bump is owed. Compact seeking consumes identical
  draws and emits identical ticks, as pinned by the continuation golden.
  FlowSurge changes only the operator-requested realization; no baseline byte,
  seed derivation, draw law or origin moved. Version 14 remains reserved for
  the protocol-12b mechanism landing.

## notes/bugs-server-tape.md, round 2 review and close pass

A cold read of the round-2 diff found ONE correctness defect, and the audit
found that one of the round's own tests did not bite. The document ends empty
of open findings; nothing was deleted that is not resolved.

- THE FRONTIER ADVANCED PAST WORK NOBODY DID, AGAIN. This is the FOURTH time in
  the arc and the SECOND in this document. Round 1 closed the error-path door
  with `frontier_after`; round 2 opened a different one.
  `try_source_at_or_before` resumes AFTER the tick its checkpoint last consumed,
  so `last_trade_at_or_before` - which round 2 made the SOLE source of ordinary
  marks and settlement prices - can return `None` for a `ts` sitting between a
  checkpoint and the next trade even though the print exists. The sweeper's
  `filter_map` then dropped the missing settlement mark while `last_swept_ns`
  advanced over its instant, retiring that settlement permanently. Two fixes,
  and the second is the structural one:
  - `CheckpointIndex::try_source_before_target` takes a walk-back count and
    reports when it has reached the earliest snapshot it may use.
    `last_trade_at_or_before` resumes progressively earlier snapshots, doubling
    the step, until it finds the print or runs out of chain. `None` now means
    "the tape could not be read", which is what its callers already assumed.
  - The reading phase is the named `read_marks`, and it collects settlement
    prices into an `Option` rather than filtering. An unreadable ordinary mark
    is still dropped - one pass of stale unrealized P and L, asked again in
    five milliseconds - while an unreadable settlement price refuses the WHOLE
    read, so `frontier_after` sees `read == false` and leaves the span owed.
    THE RULE, now stated twice in the sweeper's own comments: a watermark may
    only move over work whose success the same expression checked, and a lookup
    that legitimately returns nothing is exactly as dangerous as a panic.
- The walk-back is FENCED at a `FlowSurge` control boundary. Resuming earlier
  than an arm and replaying across it would regenerate the span unsurged, which
  is the fork the round closed; the origin is pinned, so an index that never
  armed a surge walks back as far as it likes.
- COARSENING COULD HAVE REOPENED THE FORK. `checkpoint_control_boundary`
  retains a snapshot per arm and clear, and `coarsen`'s every-other rule would
  drop the odd-indexed ones - after which a target between an arm and the next
  ordinary snapshot resolves to a PRE-ARM snapshot and replays a different
  tape. Boundary snapshots are now pinned and exempt; `k` doubles only when the
  every-other pass actually removed something, so pin pressure cannot inflate
  the residual drain, and a run arming more than `MAX_CHECKPOINTS` surges drops
  its OLDEST boundaries rather than its memory ceiling.
- `bugs-data.md` FINDING 1 IS CLOSED, verified rather than relayed, and struck
  from that document in the same commit. Nothing else in it was touched.

Claims audited, and where they did not hold:

- `checkpoint_flow_surge_is_visible_to_canonical_and_history_walks` DID NOT
  BITE as round 2 wrote it. It compared two `frontier_source` clones, which is
  the lead against itself: it proved the lead is surged, which was never in
  question, and said nothing about the checkpoint chain, where the fork
  actually lived. It now reads through `try_source_at_or_before` at an instant
  well inside the surge window and compares against the canonical sequence.
  Verified to FAIL when the boundary snapshot is removed. The probe instant is
  chosen to be unshared, because a parent's quote and its first child print at
  the same `ts_event` and a seek to a shared instant compares tie order rather
  than tape identity.
- `live_history_cannot_advance_the_paced_frontier` bites: removing the `live`
  guard makes it fail its `None` assertion.
- FINDING 7 IS CLOSED BY CONSTRUCTION, not by verification. There is no channel
  and no deferral left: `arm_flow_surge` mutates the canonical index inside the
  request's own call path, so a `202` means armed and a dead worker is a `503`.
  The stale-window-across-a-closure half of the finding disappears with the
  channel. Residual, small and disclosed: the `alive` check races a worker
  dying immediately after it, in which case the arm still lands on the
  canonical tape and only the feed stops.
- THE NO-BUMP VERDICT HOLDS. FlowSurge moving the canonical tape sounds close
  to moving a generated byte and is not: it is an operator-armed runtime
  request, so the unarmed tape for a given seed and config is untouched.
  Pinning and the walk-back decide only WHICH already-walked snapshot a clone
  resumes from, which `coarsen`'s own contract already covers. The compact
  seek was checked rather than accepted: it advances through `advance_parent`,
  the primitive `compact_parent_advancement_matches_wire_frames_and_continuation`
  already pins, and the new continuation golden shows an identical head and
  1000 identical successors. Version 14 remains reserved for protocol-12b.
- Every finding 10 bullet was re-checked against the code. All are fixed or
  genuinely dead; none needed reopening or a move to `notes/todo.md`.

New machinery later rounds must not break:

- The `Snapshot` record and the pin exemption in `coarsen`. Any future
  per-source mutable state a control arms owes a pinned boundary, or history
  replays the span without it.
- `read_marks` is the sweep's ONE reading phase and the only place the
  ordinary-versus-settlement failure asymmetry is expressed. Do not inline it
  back into the loop, and do not turn its settlement map-and-collect into a
  `filter_map`.
- `fills::test_profiles` is the single boot tape every in-crate server test
  shares. The chain is process-global, so a second one is a named failure
  rather than a silent no-op.
