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

## Close pass over the server-tape arc (commits af5434f, 4410e4f)

A whole-arc review of both rounds, weighted toward the never-cold-reviewed
second half of each commit. The frontier machinery, the canonical FlowSurge
change, the pinned-snapshot design, the no-bump verdicts and the durable prose
all held up under enumeration. ONE liveness defect found and fixed - a fifth
frontier door, the inverse shape of the previous four:

- THE WALK-BACK FENCE COULD STALL THE SETTLEMENT FRONTIER FOREVER. The fence
  at a `FlowSurge` control boundary is correct (crossing it replays the span
  unsurged), but it had no recovery: a settlement instant landing between a
  boundary pin's clock and the pin's next trade has its last print CONSUMED by
  the pin, unreachable from any resumable snapshot. `last_trade_at_or_before`
  then answered `None` for that instant deterministically, every pass, forever
  - the exact permanent stall its own comment named as the thing to avoid.
  Where rounds 1 through 4 of this defect advanced a watermark past
  unperformed work, this one wedged the watermark so no settlement could ever
  book again. The recovery is `GeneratedSource::last_trade_price`: the
  boundary snapshot's own walk state carries the print it consumed (mid-burst
  it is `burst.price_ticks`, at a boundary `last_event_price_ticks`),
  materialized through the exact `next_child` arithmetic, consuming no draw.
  The server lookup consults it only when the chain reports itself exhausted
  and the residual held nothing.
  `a_fenced_control_boundary_still_answers_for_the_print_it_consumed` pins the
  fence flag, the empty residual and the recovered price together.

Verified rather than accepted, each against the code and not the claim:

- Every frontier: `last_swept_ns` is assigned at exactly one site through
  `frontier_after`, whose `read` argument is the success of both reading
  halves; a partial settlement answer cannot reach it because `read_marks`
  collects into an `Option`. Scan frontiers advance only off a completed walk
  (`walked == None` skips the symbol), and the engine re-validates revisions.
  Settlement spans are start-exclusive, end-inclusive, so a held watermark
  re-asks its instants and an advanced one never double-books them.
- The no-bump verdicts, both rounds. An unarmed run never constructs a
  `SurgeWindow`; pinning, coarsening and the walk-back select only WHICH
  already-walked snapshot a clone resumes from; the stride and the compact
  seek are pinned draw-identical by goldens. The close pass's own fix is a
  read-only accessor. 14 stays reserved for protocol-12b.
- The pinned-snapshot memory bound. `coarsen`'s overflow tail removes index 1
  regardless of pin, so `MAX_CHECKPOINTS` is a hard ceiling and the disclosed
  residual (a run arming more than ~4096 surges loses replay fidelity for its
  most ancient windows, arm and clear each costing one pin) is accurately
  stated. `k` doubles only when the every-other pass removed something, so pin
  pressure cannot inflate the residual drain. `since_snapshot` can never
  exceed `k`, so the parent-skip arithmetic cannot underflow.
- `bugs-data.md` finding 1's closure is honest. In the same pass its findings
  2 and 4 were reconciled with what this arc actually landed: every
  `BoundedSeek` reference is gone and the trait doc's bound is now real
  (finding 2 closed), and the compact `seek_to` plus the 8,192 stride resolve
  finding 4's drain half, leaving only the per-tick `symbol` clone open.
- The fill-golden `is_multiple_of` removal is benign: `ACCEPT_STRIDE_NS` and
  `SWEEP_INTERVAL_NS` are both one second, so each sweep step maps to a unique
  order index and no duplicate submits are possible.

Residuals accepted as ruled: the FlowSurge alive-check race (the arm lands on
the canonical tape while only the feed stops - observable, bounded, disclosed),
the jointly-observable commission checks, and the dev/release profile split
(no test in this arc rests on `debug_assert`). One weakness noted, not fixed:
`swept_batch_max_bytes`'s five-frame originated bound has no worst-case pinning
test of its own; the engine's cascade test bounds a real cascade under the
tighter four-frame helper, so the server's wider reservation is safe but
unpinned. `docs/havoc.md`'s conditional-order table now says the sweep walks
the CANONICAL tape (which per-subscription regimes never touch) rather than
the "clean" tape, matching the FlowSurge paragraph above it.

## notes/bugs-data.md, round 1 (findings 3, 5, 6 and 7)

Machinery introduced:

- Calendar closure jumps are part of `begin_event`'s crossing frontier before
  `ReopenGap` is consumed. The calendar is checked again after the unscheduled
  halt, so fixing the swallowed arm cannot create a print inside a later
  scheduled closure.
- `GeneratedSource::try_build` validates an optional `SessionCalendar` before
  either `SessionModulator` or the walk can consume it, and returns the new
  typed `GeneratedSourceError::Calendar` guard on invalid input.
- The low-intensity gap documentation names strict advancement between parent
  events, not between wire ticks. Quote/first-child and sibling timestamp ties
  are intentional. The existing `fully_closed_profile_caps_each_gap_and_never_freezes_the_clock`
  test is the biting parent-level check the old `monotonic_clock` citation was
  not.

Finding state:

- Finding 3 reproduced exactly. Its regression test places the arm inside the
  CME weekend closure and observes the halt after the Sunday calendar jump.
- Findings 5 and 6 reproduced exactly. Finding 5 needed truthful invariant
  prose rather than a new mechanism; finding 6 needed the construction guard.
- Finding 7 is dead, but its premise had become live shipped wrongness before
  it was fixed: MNQ now ships `children_mean = 1.1711127211559897`, well below
  five. Commit `3a48f322` already added a floor-aware quiet/active solve,
  construction-time feasibility validation, and near-one plus boundary tests.
  This round did not alter that arithmetic.

Protocol and verification:

- The calendar-aware crossing changes generated bytes for a calendar plus
  `ReopenGap` scenario, so the unconditional rule moved
  `TAPE_PROTOCOL_VERSION` from 13 to 14. The pre-existing protocol-12b
  mechanism reservation moves to 15 rather than colliding with the live fix.
  The version-bound stage-A manifest and arrival-screen artifact were
  re-blessed in the same change.
- Both new tests were bite-checked by reverting their production guard. The
  calendar-closure test returned the bare Sunday reopen instead of the reopen
  plus halt; the invalid-calendar test returned `Ok` instead of
  `GeneratedSourceError::Calendar`. Both failed in the ordinary and
  instrumented debug sweeps before their production changes were restored.

## notes/bugs-data.md, round 1 audit pass

The cold read of the round-1 diff found nothing, and this audit confirms the
three classifications and the death verdict. It found ONE live defect of the
family the round was closing, at the boundary the fix did not reach, plus one
piece of prose the round's own finding had named and not corrected.

- THE CROSSING FRONTIER SKIPPED EVERY INTRA-BURST SPAN. The round made the
  calendar jump part of the tested span, but the span still STARTED at
  `clock_ns` - and `step_child` moves `clock_ns` from the parent to the last
  child of the burst, `INTRA_EVENT_STEP_NS` per child. So the span
  `(parent_ts, last_child_ts]` was tested by nobody: the burst's own event
  tested up to the parent, and the next event started from the last child,
  already past any arm inside it. `old_clock < at_ts` could then never hold
  again and the halt stayed armed forever - finding 3's exact failure mode on
  a different boundary. The window is narrow (one microsecond per extra child)
  and unreachable for a single-child parent, which is why nothing had hit it,
  but an arm is operator-supplied nanoseconds and the loss is permanent and
  silent. Fixed by CONSTRUCTION rather than by another check: `GeneratedSource`
  now carries `reopen_frontier_ns`, the upper bound of the last span actually
  tested, and both walk branches test from it. Contiguity is now a property of
  the field rather than of every clock mutation remembering to be included.
  The integrated branch needed the matching half, because the kernel tests the
  crossing against its own walk start: a pending arm that already sits behind
  that start is presented at `from_ns + 1`, which the kernel's first candidate
  necessarily reaches. A still-pending arm is always strictly ahead of the
  frontier, so that clamp can only move an arm the walk would otherwise strand.
  `a_reopen_gap_armed_inside_a_burst_still_fires` places an arm one nanosecond
  after a multi-child parent and counts the halts; it reports zero against the
  round-1 code in both the ordinary and instrumented release sweeps.
- The floor-branch condition is now STATED where the identity is claimed.
  Finding 7's real residual was `consts.rs` asserting
  `quiet_frac * QUIET + (1 - quiet_frac) * ACTIVE == 1` unconditionally when it
  is the above-floor statement only. The comment now names the condition, says
  which branch runs below it, and points at the construction-time feasibility
  refusal.

Verified rather than accepted:

- THE BUMP WAS OWED. The calendar-aware crossing is not an error path: with a
  calendar and an armed `ReopenGap`, the halt now fires where it previously
  did not, so the clock, the mid and every subsequent draw move. It reaches
  only that configuration - a calendar-free run and an unarmed run both step
  through the identical arithmetic - but the rule is unconditional and the
  bump is free. The frontier fix above lands in the same commit and is covered
  by the same identity.
- THE RE-BLESS IS A REGENERATION, not a literal typed to match.
  `committed_manifest_is_self_consistent` re-derives the WHOLE manifest from
  the committed pilot with `build_manifest`, which reads the live constant, and
  asserts `rebuilt == manifest`; `parse_manifest` separately refuses a manifest
  whose version is not the live constant. A hand-edited hash could not survive
  either. The screen artifact's binding assertion still compares against
  `mogwai_data::TAPE_PROTOCOL_VERSION` and nothing was widened into a tolerance.
- FINDING 7 IS GENUINELY DEAD. `3a48f322` (protocol 10) put the floor-aware
  branch behind exactly the predicate finding 7 identified -
  `children_mean * ARRIVAL_QUIET_CHILDREN_MULT <= 1.0`, i.e. `children_mean` at
  or below 5 - so the clamp the finding described is unreachable in that whole
  range. The branch re-solves the active mean from the unconditional target, so
  the declared mean is preserved by construction rather than approximately, and
  `GeneratorScalars::validate` refuses a configuration whose active solve is
  not expressible as the one-plus-geometric mixture. MNQ's shipped 1.1711 is
  inside the branch and feasible.
- EVERY CONSTRUCTION PATH IS COVERED by the calendar validation, not just
  `try_build`'s own callers: `new`, `new_with_session_profile`, `try_new`,
  `try_new_with_size_grid` and `try_new_with_session_profile` all funnel
  through `build`/`try_build`, the `calendar` field is private, and nothing
  attaches or replaces a calendar after construction. This is the shape the
  engine arc got wrong with `set_margin_policy`, so it was enumerated rather
  than assumed. `mogwai-server`'s config validation already refused an invalid
  calendar at boot, so the server's infallible `generator()` call cannot reach
  the new panic; a consumer constructing an `InstrumentProfile` by hand gets a
  loud panic where it previously got a clock pinned at `u64::MAX`.
- Both of the round's own tests bite. Removing the pre-crossing calendar jump
  makes `reopen_gap_inside_calendar_closure_is_consumed_at_the_jump` return the
  bare Sunday reopen instead of reopen plus halt; removing the `validate` call
  makes `try_new_accepts_valid_input_and_surfaces_bad_input` see `None` where it
  wants `Calendar(CalendarError("open_windows"))`. Checked in the release
  profile, where `brokkr test` runs, so neither rests on a `debug_assert`.
- The reservation prose reached further than the durable folders. The
  protocol-12a spec states the identity the 12b mechanism landing consumes in
  two places, and both still said 13 - stale since the PREVIOUS bump. Both are
  amended to 15, `notes/bugs-engine.md`'s present-tense "now takes 14" is
  corrected, and `notes/todo.md`'s prose-gate item records that the gate must
  find its document set by grep rather than by a hardcoded list of folders.

## notes/bugs-data.md, round 2 and document close

Classification:

- Finding 4 REPRODUCED. `mogwai_protocol::Symbol` is now `Arc<str>`, with
  serde's `rc` support, and `GeneratedSource` owns one interned value shared by
  every materialized trade and quote. The wire JSON is unchanged. The type
  change was followed through engine, server, adapter, CLI and tests rather
  than stopped at the generator. `TickRuleAggressor` keys directly on the same
  `Symbol`, so its state keeps a reference-counted clone rather than allocating
  another string.
- Finding 8 REPRODUCED AS FALSE DOCUMENTATION, but its deletion proposal was
  rejected. `MergeSource` is still the real multi-symbol merge for the reusable
  offline CSV intake, latches child faults, preserves deterministic source-index
  ties, and provides the inclusive one-tick buffer for production history.
  Production k=1 does not erase those contracts. Its documentation now says it
  performs a linear head scan and names both usage shapes; no heap is claimed.
- Finding 9 IS DEAD and was deleted rather than acted on. Its version-9 and
  zero-Rust-change premises describe an old tree. This round's own verdict is
  also no bump: ownership, settlement lookup complexity and runtime metadata
  layout change no draw, seed, event value, event ordering or tape origin.
  Version 14 remains live and 15 remains reserved for the protocol-12b
  mechanism.
- The settlement bullet REPRODUCED IN ALTERED FORM. The old loop did not call
  `is_open` every minute as claimed, but it did compute civil-time modulo for
  every minute. It now solves the first UTC settlement minute once and advances
  by 1,440 minutes. The offset test counts 10 candidates over 10 days; restoring
  the minute scan makes it count 14,400 and fail.
- The `PublishedBook` provenance bullet REPRODUCED conditionally. Shipped
  presets remain uncalibrated, but a fitted provenance string was cloned into
  every placed book and compatible repeat. Runtime books now copy only bid and
  ask decimal values. Calibration provenance remains on config. A fitted-corpus
  test pins the separation and the 48-byte runtime shape.
- The `SweepShape::new` bullet IS DEAD AS WRITTEN. `new` performs no logarithms;
  the two logarithms are in `next_count`. Caching the invariant denominator was
  implemented and measured on `arrival_walk`; identical work stayed at 300 ms
  before and after, so the unmeasured micro-optimization was reverted.
- The checkpoint bullet REPRODUCED IN ALTERED FORM. `CHECKPOINT_K` is 8,192,
  but `MAX_CHECKPOINTS` is still 4,096, not 8,192. Pinned control snapshots are
  included inside that hard cap and can displace old pins; they do not grow it.
  The committed warmup retains roughly 520 clones. `GeneratorScalars`, the
  calendar and the emitted symbol allocation are now shared across the lead and
  snapshots. Positioning measured 202.27 us before and 199.80 us after, which
  criterion called no detectable throughput change; the retained result is the
  eliminated heap duplication, not a wall-clock claim.

Verification:

- `clones_share_immutable_config_and_emitted_symbol_storage` fails when the
  three materialization sites are reverted to allocate fresh `Arc<str>` values.
- `settlement_day_step_respects_local_offset_and_open_filter` fails at 14,400
  candidates versus 10 when the minute scan is restored. Both negative controls
  were run in release, not left behind a debug assertion.
- `published_book_carries_values_without_calibration_metadata` and
  `tick_rule_reuses_the_trade_symbol_allocation` pass in both release sweeps.

## notes/bugs-data.md, round 2 audit and document close

The round's classifications all held. The audit found NO defect in the round's
own work and ONE PRE-EXISTING defect that the round's mandatory `--gate` run
surfaced, which had shipped red through exactly the ignored-test gap the
project rules warn about.

- THE TAPE WORKER REFUSED TO START ON A ZERO-WARMUP VENUE. `Tape::start`'s
  worker opens with a positioning probe, and it targeted the SIMULATED NOW -
  a few milliseconds of boot latency past `run_start_ns`. `activate_live` has
  already run by then, and a live index will not extend for a reader, so the
  probe asks whether the frontier ALREADY covers an instant the worker itself
  has not reached. Whether it did was an accident of how far the warmup walk
  had overshot `run_start_ns`: with a five-minute warmup the next tick usually
  sits a second or more past it and the probe passed, while with `warmup_ns`
  of 0 there is no overshoot at all and the probe refused on EVERY boot. The
  worker then returned before publishing one frame and the venue served an
  empty tape and exited 0. The probe now targets the tape ORIGIN, which is
  checkpoint zero and always reachable, so a refusal there means what its
  message says. Note the shape: the probe's positioned source is DISCARDED -
  the worker reads through `next_live_tick` - so this was a liveness check
  whose target had drifted into a race with the thing it was checking.
- The gate's two failures were both this. `a_faulted_venue_exits_nonzero...`
  failed deterministically because the fault fixture declares `warmup_ns = 0`,
  so it never reached the fault at all, and
  `a_short_accelerated_run_is_not_over_before_it_is_ready` failed one run in
  three because a loaded host stretches boot latency past the accelerated
  warmup's overshoot. Both were confirmed PRE-EXISTING by running them against
  the round's base commit, and both are green after the fix; the flaky one
  passed 10 of 10.
- `a_venue_without_warmup_still_publishes_its_tape` and the `no-warmup.toml`
  fixture pin the boot shape directly rather than through the fault fixture's
  side door. Bite-checked: restoring the simulated-now target makes it fail
  because the venue is gone by the time the socket opens.

Verified rather than accepted:

- THE WIRE FORM DID NOT MOVE, and it is pinned rather than argued. `Symbol` is
  a serde `rc` `Arc<str>`, whose impls forward to the inner `str`, and
  `messages.rs` carries an EXACT-EQUALITY frame table that both serializes a
  `QuoteTick` to a literal JSON string and re-serializes the decode. That test
  is untouched by the diff and green, so the JSON a nautilus consumer parses is
  byte-identical. `Arc<str>` is `Send + Sync` exactly as `String` was, so no
  thread-boundary obligation changed; the cost that did change is that a symbol
  clone is now an atomic increment rather than a heap copy.
- NO `TAPE_PROTOCOL_VERSION` BUMP IS OWED, and the exact-equality artifacts are
  the argument rather than a reading of the diff. No committed artifact under
  `analysis/` or `crates/mogwai-server/tests/golden/` is modified by this round,
  and the goldens that bind them - the fill golden, the arrival transcripts and
  the checkpoint continuation golden - are green. A change that moved a draw,
  a seed derivation, an emitted value or the tape origin could not leave all of
  them untouched and passing. The three changed values are structural: an
  `Arc<str>` symbol carries the same characters, `PublishedBook` drops metadata
  the generator never read (only `sizes.bid` and `sizes.ask` ever reached a
  quote), and the settlement day-step returns the same instant set because
  `MINUTES_PER_WEEK` is a whole multiple of `MINUTES_PER_DAY`, so the old
  per-minute civil-time predicate matches exactly one minute per day - the one
  the new arithmetic solves for directly. 14 stays live, 15 stays reserved for
  protocol-12b.
- THE `MergeSource` PROSE IS TRUE, checked claim by claim against the code
  rather than against the round's report. The scan is a linear `min_by_key`
  over per-source heads; `latch_fault` really does latch a child fault and
  blank every head; `starting_at` seeks each child and BUFFERS the returned
  tick, which is the inclusive-start contract `source.rs` relies on. Only the
  tie rule was asserted without a test - `min_by_key` returning the first
  minimum is a documented guarantee, but the doc now states the behaviour, so
  `merge_breaks_timestamp_ties_by_source_index` pins it with three sources and
  a tie run.
- THE `Arc<str>` REFACTOR EARNS ITS BLAST RADIUS ON STRUCTURAL GROUNDS ONLY,
  and `reference/performance.md` says so in those words. The measurements found
  no throughput win (300 ms unchanged; a 0.38 percent positioning point
  estimate criterion called undetectable), and the durable record states the
  retained result as eliminated heap duplication rather than a wall. It is kept
  because the reported defect WAS an allocation defect and the fix removes the
  crate's most frequent allocation from the feed, history and key paths at no
  measured cost - not because it made anything faster.
- Both of the round's negative controls bite, checked by reverting the
  production change and observing the named failure in RELEASE. Reverting
  `next_child`'s shared symbol fails
  `clones_share_immutable_config_and_emitted_symbol_storage` on its `ptr_eq`;
  restoring the minute scan fails the settlement test on its candidate
  counter (14,400 versus 10). The instant list is the SAME under both
  arithmetics - that identity is the no-bump argument - so the counter is
  the only assertion that can bite, which is why the test carries one.

Consumer-visible surface, recorded in `notes/todo.md`: `mogwai_protocol::Symbol`
is no longer `String`, which is a source-breaking change for anything
constructing wire types by hand. broadarrow is the known consumer.

## Standing process note: never undo a bite-check revert with git

A bite check reverts a production change, confirms the test fails, and restores
it. TWICE in this loop an agent restored it with `git checkout -- <path>` and
destroyed the whole file back to HEAD, wiping uncommitted work an EARLIER stage
had left in the tree. Both times the work was reconstructed and verified, and
both times it was luck rather than process.

The tree in this loop routinely carries a prior stage's uncommitted work, so
`git checkout -- <path>` is never the right undo: it discards everything in the
file, not the one edit under test. Revert and restore a bite check as a TEXT
EDIT, the same way it was made. If a bite check genuinely needs a wholesale
revert, stash or copy the file first and restore from that copy.

## notes/bugs-server-transport.md, round 1 (findings 1 through 5)

Classification:

- Findings 1 through 5 all REPRODUCED as written. The admission subject defect
  also closes finding 1 of `notes/bugs-protocol.md`: `AdmissionSubject` now
  bounds every client id during serialization in the protocol crate, so even a
  caller constructing a raw public variant cannot put an oversized subject on
  the wire.
- Websocket frames and reassembled messages have an explicit 64 KiB protocol
  ceiling, replacing tungstenite's dependency defaults.
- Every socket owns one bounded command queue and one sequential dispatcher.
  A process-wide permit covers every queued or executing command, including
  the ordinary zero-act-latency path. Capacity refusal is a priority-lane
  `AdmissionRejected`; the engine never sees the refused command.
- The dispatcher is the construction proof for arrival order. It holds each
  command through act latency, market reading and engine processing before it
  receives the next, so neither the blocking pool nor different act delays can
  reorder admitted commands.
- A completion receiver checks its current value before waiting for a change.
  A socket upgraded after the terminal transition therefore receives
  `RunComplete` and WS 1000 instead of waiting on a change already seen.

Protocol and verification:

- No `TAPE_PROTOCOL_VERSION` bump is owed. The tape worker, generator, seed
  derivation and emitted tape bytes are untouched. Arrival order can change
  ENGINE outcomes relative to the buggy binary, including whether a cancel
  rejects or succeeds, but it does not change tape generation or fill draws;
  fill draws remain keyed by order fields and seed. Version 14 stays live and
  15 stays reserved for the protocol-12b mechanism landing.
- The ordering negative control replaced the sequential receive with detached
  dispatch. `websocket_commands_cannot_overtake_each_other` then failed with
  `unknown order` when a zero-latency cancel overtook its delayed submit.
  Removing the websocket size configuration made
  `websocket_rejects_messages_over_the_protocol_ceiling` fail by accepting the
  oversized payload and returning `ProtocolError`. Both production changes
  were restored by text edit. Making the late-completion reader return no
  current value made `receiver_created_after_completion_observes_terminal_state`
  fail in release; that production change was likewise restored by text edit.
- `admission_subject_serialization_bounds_raw_client_ids` constructs the public
  enum with an id more than 10 KiB over its cap, serializes the whole refusal,
  and verifies both the 4 KiB frame ceiling and the decoded 64-byte subject.
  `websocket_command_work_is_bounded_without_an_act_delay` drives the original
  unbounded configuration directly: with both command capacities set to one
  and no armed latency, a burst receives a visible capacity refusal.

## notes/bugs-server-transport.md, round 1 review and close pass

A cold read of the uncommitted round-1 diff found ONE issue, and it was a
FAILURE THIS MACHINE'S GREEN GATE DID NOT REPRODUCE. It is a test bug, not a
production bug, and the reasoning matters more than the verdict:

- `websocket_rejects_messages_over_the_protocol_ceiling` assumed the first
  frame after an oversized send is the close. The socket is a LIVE market-data
  feed, so a `Quote` or `Trade` the venue had already written is legitimately
  in flight and arrives first; the reviewer saw exactly that. The production
  path is prompt and deterministic in the only sense available: the very next
  poll of `stream.next()` returns `Err`, the read loop breaks, teardown is
  bounded by `CLOSE_GRACE`, and TCP guarantees only frames written BEFORE that
  turn precede the close. There is no unbounded interval to paper over, so the
  test drains until close, error or deadline - and the DEADLINE is now the real
  assertion, so a venue that kept serving the connection would time out rather
  than pass. It additionally asserts no `ProtocolError` ever appears, which is
  the honest statement of the finding: the venue must never REASSEMBLE AND
  PARSE a message over its own ceiling. That rewrite made the test STRONGER,
  not more tolerant: it fails 3 of 3 with the size configuration removed, where
  the original depended on frame interleaving to fail at all.
- The class was checked, not just the instance. The other two new socket tests
  already drain in a loop and ignore unrelated frames. THE STANDING RULE: a
  test on a `/ws` socket may never assert on THE NEXT frame, because every
  socket is attached to the tape on upgrade.

Claims audited, and where they did not hold:

- `websocket_command_work_is_bounded_without_an_act_delay` DID NOT PIN WHAT IT
  CLAIMED. Its fixture set both `pending_command_acts` and
  `global_pending_command_acts` to 1, and the refusal it observed came from the
  process-wide semaphore: raising the per-connection queue to 4096 left the
  test green. The fixture now leaves the global bound at its default, so the
  refusal can only come from the socket's own queue. Bite-checked in that
  form - it times out with the queue widened.
- `receiver_created_after_completion_observes_terminal_state` was a near
  tautology over `watch::Receiver::borrow_and_update`. It now asserts the
  DIFFERENTIAL - a late receiver's `changed()` does not resolve, which is the
  bug's whole shape - alongside the current-value read. Disclosed residual: it
  pins the helper, not the call site. An integration test cannot reach this
  honestly, because the venue stops accepting connections at completion, which
  is why the round wrote a unit test in the first place.
- The ordering test and the admission-cap test both bite. Reverting the
  sequential receive to a detached spawn fails
  `websocket_commands_cannot_overtake_each_other` with `cancel overtook submit
  and was rejected: unknown order`; removing the char-boundary truncation fails
  `admission_subject_serialization_bounds_raw_client_ids` on the 4 KiB ceiling.
  All four bite checks ran in RELEASE and none rests on a `debug_assert`.

Verified rather than accepted:

- THE DISPATCHER IS DETERMINISTIC AND WEDGE-PROOF. Its total order is a
  function of arrival alone: one socket has one read loop, `try_send` preserves
  that order into one mpsc queue, and one task awaits each command through act
  latency, market read and engine processing before receiving the next. Nothing
  downstream reintroduces a choice - there is no `select!` over ready commands,
  no map iteration and no per-command spawn left. It cannot wedge: the read
  loop never awaits the queue (`try_send` refuses instead of blocking), so a
  dispatcher stuck on a market read that never completes costs a visible
  capacity refusal rather than a stalled socket, and teardown aborts it. The
  abort is safe because `process_order_cmd` has exactly ONE await, before the
  engine lock; `process_with_market` runs to completion under the lock, so no
  cancellation can tear engine state.
- THE BOUND REFUSES, IT DOES NOT DROP AND IT DOES NOT BLOCK, and the client is
  told: a full queue or an exhausted process permit emits `AdmissionRejected`
  with `venue command capacity exhausted` on the priority lane, and the engine
  never sees the command. Both permits are released on the refusal path,
  because the `QueuedCommand` carrying the permit is dropped by the failed
  `try_send`.
- THE NO-BUMP VERDICT HOLDS, checked rather than relayed. Command arrival order
  reaches the ENGINE, and the engine reaches no tape generation path at all;
  the market read clones a checkpoint source and cannot extend the live index,
  so no ordering of reads can move a generated byte, a draw, a seed or the
  origin. Fill draws are keyed by order fields and band draw. 14 stays live and
  15 stays reserved for protocol-12b.
- `bugs-protocol.md` FINDING 1 IS CLOSED and struck from that document in the
  same commit; nothing else there was touched. Both halves of its argument are
  now real bounds - the subject is truncated inside `Serialize`, so every wire
  path passes it, and the inbound 64 MiB tungstenite default is replaced by
  `MAX_CLIENT_MESSAGE_BYTES`. The finding's preferred `BoundedId` newtype was
  NOT built: the variants stay constructible from a raw `String`, so the
  invariant holds at serialization rather than construction. That is a weaker
  place than the type, and a later round wanting the stronger one has the
  finding's own sketch.

Consumer-visible surface, recorded in `notes/todo.md` as three numbered limits
with what a consumer must do about each: the 64 KiB inbound cap, the bounded
command queue and its refusal frame, and the new HEAD-OF-LINE property - an
armed `CommandLatency` now delays every later command on the same socket, which
is the price of arrival order and is stated in `docs/havoc.md`.

## notes/bugs-server-transport.md, round 2

- Finding 6 REPRODUCED. Duration resolution now carries three states, so an
  explicit indefinite override beats a finite config. This ALSO CLOSES finding
  2 of `notes/bugs-protocol.md`: `LaunchSpec` already preserved zero in argv,
  and the server now gives it the documented meaning.
- Finding 7 was DEAD. Round 1 had already corrected the admission-lane comment
  to 256 KiB.
- Finding 8 REPRODUCED. `/quotes` now refuses below the tape origin exactly as
  `/trades` does. No tape protocol bump is owed: this only changes an HTTP
  refusal outside the tape and moves no byte, draw, seed, origin or generated
  event. Version 15 remains reserved for protocol-12b.
- Finding 9 REPRODUCED IN ALTERED FORM. Checkpoint positioning still shares one
  per-run lock, but round 1's 8,192-tick stride means it covers only the short
  positioning step, not 50,000-tick response synthesis. The live wedge was
  uncapped history work sharing Tokio's blocking pool with command readings. A
  four-request fail-fast gate caps worst-case response construction near 28 MiB
  and prevents pool saturation; excess requests receive 503. Admission cost
  measured 22 to 23 ns over five release runs of one million iterations.
- Speed validation REPRODUCED and moved into `Config::load`, before warmup.
  Unsigned duration and cadence fields did not receive arbitrary ceilings:
  zero is meaningful, warmup already has a synthesis cap, and a zero unpaced
  stall is an intentional immediate release used by a socket fixture.
- Binary-frame silence REPRODUCED. Binary frames now receive a priority-lane
  `ProtocolError`. Forced-drain success REPRODUCED; exhausting the five-second
  grace now returns an error and exits nonzero.
- Signal completion was reviewed and DECLINED. `RunComplete` means declared
  simulated duration elapsed; a launcher signal is an interrupted or
  undeclared run. Publishing completion would erase that distinction.
- The single-instrument settlement loop and websocket-only `ActDelay` marker
  both REPRODUCED as dead generality and were removed.
- The rewrite proposal was already BUILT by round 1: one bounded sequential
  dispatcher, no per-command spawn, refusal on capacity, and serialization-time
  subject truncation. This round retained that design unchanged.

Bite checks: restoring two-state duration fallback failed the new test with
the configured 600 seconds instead of `None`; restoring binary silence failed
the socket test at its ten-second deadline, while restored code passed 5 of 5;
restoring a 512-request history ceiling failed the pinned-cap test; disabling
the quote origin-floor branch failed its artificial-nonzero-origin test.

## notes/bugs-server-transport.md, round 2 review and document close

A cold read of the round-2 diff found ONE correctness defect, and it was the
family this whole loop keeps meeting: A GUARD RELEASED BEFORE THE WORK IT
GUARDS IS FINISHED. Where five earlier instances advanced a watermark past
work nobody performed, this one returned a capacity slot while the work it
admitted was still resident.

- THE HISTORY BOUND DID NOT BOUND ANYTHING. The permit was a handler local and
  the handler returned `Json(Vec<Tick>)`. Axum serializes a returned `Json`
  value AFTER the handler future resolves, so all four permits were released
  while four multi-megabyte responses were still being built - readmitting four
  more requests, and with them unbounded accumulation of exactly the buffers
  the ceiling advertised as capped. Fixed by making the permit outlive the
  RESPONSE, not the handler: serialization moved onto the synthesis's own
  blocking task, and `HistoryPage` is a one-shot body stream owning both the
  finished bytes and the permit, so the slot returns when hyper drops the body.
  THE RULE, and it generalizes past this loop: a permit, lock or guard whose
  scope ends before the work it protects is the same defect as a watermark
  advanced past unperformed work, and both are visible by asking WHAT IS STILL
  RESIDENT when the guard drops.
- THE 28 MiB FIGURE WAS INVENTED and is replaced by a measurement.
  `worst_case_history_page_bytes` is a release-mode instrument over a full
  `MAX_HISTORY_LIMIT` page: `/quotes` is 4.40 MB of vector plus 5.90 MB of
  JSON, `/trades` 3.20 MB plus 5.05 MB. Vector and bytes are resident together
  while serde runs, so an admitted quote page peaks near 10.3 MB and the four
  slots near 41 MB. `reference/performance.md`, `docs/config.md` and the
  constant's own doc all carry that number now, and the doc comment says which
  test derives it.
- THE OTHER GUARDS IN THIS ARC WERE ENUMERATED, not assumed. Round 1's
  process-wide command permit is CORRECT and its correctness rests on a
  variable NAME: `while let Some(QueuedCommand { cmd, _global_slot })` binds
  the permit for the whole loop body, so it is returned after
  `dispatch_command` has been awaited to completion. A bare `_` pattern would
  drop it at the destructure and reduce the bound to counting acceptances. That
  is now stated in a comment at the site, because nothing else would catch it.
  The per-connection queue slot is the mpsc capacity itself, released on
  `recv`, which is deliberate and unchanged: it bounds what is QUEUED while the
  process permit bounds what is IN FLIGHT.
- A HISTORY REQUEST CANNOT STALL A COMMAND, checked rather than reasoned about.
  The per-symbol index mutex is taken by `build_history_source` for
  `try_source_at_or_before` ONLY - the positioning step, bounded by round 1's
  8,192-tick stride - and released before the up-to-50,000-tick synthesis,
  which walks a private clone. So the worst a command's `market_reading` waits
  behind is four positioning steps, not four page syntheses. The blocking-pool
  half is what the gate closes: four of Tokio's 512 blocking threads, so
  history can no longer crowd out command market readings there either.

Verified rather than relayed:

- `bugs-protocol.md` FINDING 2 IS GENUINELY CLOSED and struck from that
  document in the same commit; nothing else there was touched. It was checked
  at the layer that owns it rather than accepted from the server fix. The
  server's three-state resolution gives `--duration 0s` the meaning
  `docs/cli.md` always claimed, and `LaunchSpec::duration` - the protocol type
  the finding actually names - now DOCUMENTS that `Some(ZERO)` is an explicit
  indefinite override rather than "end immediately" and rather than a typed
  guarantee that hides it. Refusing zero was considered and rejected: `0s` is a
  documented spelling the venue must be able to be told.
- THE DELETIONS ARE SOUND, but one of them is a behaviour change the finding
  mis-stated. `ActDelay` was a single-variant marker enum in `http.rs`, never a
  divergence and never armable by a scenario author - command latency is
  `CommandLatency` in `control.rs` and is untouched. The identifier now appears
  in no crate and no document. `refuse_unfunded_settlement` is the behaviour
  change: `instrument_defs()` is NOT always one element, because a checkout
  with no `[instrument]` table builds all three shipped presets while `serve`
  picks the first and serves only it. Checking the whole table could refuse
  boot over a currency no order in the run could quote; the check now takes THE
  served instrument, and the function's doc states that distinction rather than
  repeating the finding's "always one element".
- No `TAPE_PROTOCOL_VERSION` bump. Response framing, an admission gate, a
  duration resolution, an exit code and a `ProtocolError` frame reach no
  generator, draw, seed or origin. 14 live, 15 reserved for protocol-12b.

Bite checks, each by reverting the production change as a TEXT EDIT and
observing the named failure in RELEASE:

- `a_history_page_holds_its_slot_until_its_body_is_written` fails on its first
  assertion when `history_page` drops the permit at handler exit - which is
  precisely the shipped-round behaviour, so it is a regression test for the
  defect above and not a tautology.
- `history_concurrency_refuses_instead_of_queueing` was REWRITTEN because it
  did not bite: it built its own semaphore and asserted tokio's behaviour, so
  no change to the server could have failed it. It now drives `admit_history`,
  the one decision both handlers make, and fails when the cap is widened to
  512. Disclosed residual: it pins the decision, not that a handler still calls
  it; no in-crate test can build an `AppState` cheaply enough for the call
  site.
- `quote_history_refuses_below_a_nonzero_origin` fails when the origin-floor
  branch is disabled, and `an_explicit_zero_duration_overrides_a_finite_config`
  fails when duration resolution is flattened back to two states.

Consumer-visible surface, recorded in `notes/todo.md` as two entries with what
a consumer must do about each: the `503` on `/trades` and `/quotes` under
concurrency (retryable backpressure, and a slow reader holds its slot until the
bytes are written), and the NONZERO exit when connections do not drain within
the shutdown grace (close sockets on `RunComplete` rather than waiting to be
dropped). `docs/cli.md` carries the exit-code change and `docs/config.md` the
admission gate.

## Close pass over the server-transport arc (commits 437f3a6, 55936a2)

A whole-arc review weighted toward the never-cold-reviewed second halves of
both rounds: the rewritten tests, the streaming `HistoryPage` body, the
`LaunchSpec::duration` semantics and the deletions. The sequential dispatcher,
the completion read, the `AdmissionSubject` bound, the duration tri-state, the
no-bump verdicts, the `bugs-protocol` strikes and the durable prose all held
up under enumeration. ONE correctness gap found and fixed, the SEVENTH member
of the guard-scope family and the second inside the same history gate:

- THE HISTORY PERMIT DID NOT SURVIVE A CLIENT DISCONNECT. Round 2 made the
  permit outlive the RESPONSE, but on the way there it was still a handler
  local across the `spawn_blocking` await - and hyper DROPS the handler future
  the moment the client disconnects, while a running blocking task cannot be
  cancelled. So a client that requested and immediately hung up released the
  slot while its orphaned synthesis and multi-megabyte serialization were
  still resident; repeated, that accumulates unbounded orphaned syntheses,
  which is exactly what the four-slot ceiling advertises as impossible. The
  permit now rides INSIDE the blocking closure and is handed back with the
  finished bytes: on success it flows into `HistoryPage` as before, and on
  error, panic or a dropped handler it is released only when the blocking
  work actually ends. The lesson sharpens the standing rule: a guard is not
  scoped to the work by being ALIVE while the work runs - it must be OWNED by
  the task doing the work, because the awaiting future can die first.
- THE COMMAND PERMIT'S NAME-HELD LIFETIME WAS HARDENED. Round 2 disclosed that
  `while let Some(QueuedCommand { cmd, _global_slot })` kept the process-wide
  slot alive for the loop body only because the binding was named rather than
  `_`. The dispatcher now moves the whole `QueuedCommand` in and drops
  `queued.global_slot` EXPLICITLY after the awaited dispatch, so releasing it
  early requires deleting a visible `drop`, not renaming a pattern binding.
  Behaviorally identical; the correctness property moved from a naming
  convention into sequenced code.

Verified rather than accepted, each against the code:

- THE DISPATCHER'S ORDER AND TEARDOWN. One read loop, `try_send` into one
  bounded mpsc, one task awaiting each command to completion: total order is a
  function of arrival alone. The teardown `abort()` is safe because every
  await in `dispatch_command`/`process_order_cmd` precedes the engine
  mutation, and `process_with_market` runs synchronously under the lock with
  no await between mutation and the (synchronous) lane submit - a cancellation
  cannot tear engine state, only lose frames a closed socket could not receive
  anyway. Refusal paths return both permits: a failed `try_send` drops the
  `QueuedCommand` inside the closure, permit and all.
- HISTORY CANNOT STALL A COMMAND. `build_history_source` takes the per-symbol
  index mutex for the positioning step only (bounded by the 8,192 stride) and
  the up-to-50,000-tick synthesis walks a private clone; the gate caps history
  at four of the blocking pool's 512 threads.
- THE 10.3/41 MB MEASUREMENT IS REAL. `worst_case_history_page_bytes` re-run
  in release reproduced 4,400,000 + 5,900,001 bytes for `/quotes` and
  3,200,000 + 5,050,001 for `/trades`, exactly the figures in the constant's
  doc, `docs/config.md` and `reference/performance.md`. No stale 28 MiB
  statement survives outside this file's own historical round-2 record.
- BOTH `bugs-protocol` STRIKES ARE HONEST. Finding 1: the truncation lives in
  the protocol crate's `Serialize`, so every wire path passes it, and the
  64 KiB inbound cap is set on the upgrade; the declined `BoundedId` newtype
  is recorded as the residual. Finding 2: `LaunchSpec::duration` documents the
  `Some(ZERO)` meaning at the protocol layer, `serve_argv` preserves `0s`, and
  `resolve_run_duration` pins both directions server-side.
- THE NO-BUMP VERDICTS HOLD, including round 1's admission that engine command
  outcomes can differ from the buggy reordered binary: the engine reaches no
  tape generation path, the market read clones a checkpoint source and cannot
  extend the live index, and fill draws are keyed by order fields and band
  draw - determinism per binary is the contract and the tape bytes are
  untouched. 14 stays live, 15 stays reserved for protocol-12b.
- THE DELETIONS. `ActDelay` was a single-variant marker enum, never armable -
  `CommandLatency` in `control.rs` is untouched and the identifier appears
  nowhere. `refuse_unfunded_settlement`'s narrowing is the disclosed behaviour
  change (defaults carry three presets, serve serves the first) and its test
  covers the served-instrument form.

Verification of this pass: `brokkr fmt` and `brokkr check --gate` green (902 +
416 tests), and the four socket-backed transport tests - command ordering,
capacity refusal, size ceiling, binary frames - each passed 5 of 5 repeated
runs.

Residuals accepted as ruled: the history-concurrency test pins `admit_history`
rather than the call site; the late-completion test pins the helper because a
completed venue accepts no connections; `AdmissionSubject` stays constructible
from a raw `String` with the bound at serialization; the drain-grace bail can
shadow a tape-fault message when both occur (the fault already killed every
connection, so the drain succeeds in practice); and the `HistoryPage` permit
is released when hyper drops the body, which can precede the final socket
flush by whatever hyper has buffered - the dominant residency (vector plus
JSON during serde) is bounded, kernel-side buffering is not claimed to be.
One warning for anyone filtering tests: `brokkr test -p mogwai-server` with a
narrow filter LIES for the global-index tests -
`run::tests::the_history_floor_is_the_fixed_tape_origin` fails 5 of 5 in
isolation because the process-global chain is booted by `fills::test_profiles`
elsewhere in the suite. Pre-existing, green in the full gate; run the crate's
suite whole or include the `fills` tests in the filter.

## notes/bugs-protocol.md, single round

Classification:

- Finding 3 REPRODUCED. `validate_divergence` now rejects over-length targets
  for both targeted single-shots and rejects an over-length
  `RejectNextSubmit.reason`. The HTTP handler no longer owns a second
  post-validation truncation path, so the protocol guard is authoritative by
  construction for every current and future arming path.
- Finding 4 REPRODUCED. The readiness reader takes at most 4,097 bytes, refuses
  anything over the 4,096-byte record ceiling, and retains only a valid UTF-8
  prefix in `LaunchError::Malformed`.
- Finding 5 REPRODUCED WITH STALE DERIVATIONS. The constants remained safe but
  their field inventories were wrong. Order-fill and reason-bearing order
  events now have separate derivations whose maximum defines the reservation;
  `Position` lists all six fields; the admission derivation no longer charges
  a nonexistent symbol.
- Finding 6 REPRODUCED. `AccountId`'s field is private and its hand-written
  `Deserialize` routes through `parse`, so neither Rust construction nor serde
  can bypass the invariant.
- Finding 7 REPRODUCED AS A MEASURABLE ALLOCATION COST. A release probe measured
  244 ns and four allocations for an internally tagged `Trade`, versus 121 ns
  and two allocations for the same plain payload. The adapter now uses a tag
  probe plus direct `Trade`/`Quote` payload decode, measuring 245 ns and two
  allocations. This is an allocation win, not a claimed throughput win; cold
  variants retain the fully general decoder. Exact frame bytes did not move.
- Finding 8 REPRODUCED. Seed derivation remains in `mogwai-protocol` to preserve
  the workspace dependency direction, and now carries an adjacent explicit
  pointer to the downstream `TAPE_PROTOCOL_VERSION` obligation. Seed vectors
  and tape bytes did not move, so version 14 remains unchanged and 15 remains
  reserved.
- Finding 9 IS DEAD. Since commit `a6f57760`, `docs/cli.md` explicitly rules
  seed replay a written config act and says there is deliberately no `--seed`
  launcher override. The implementation matches that decided contract, so no
  second spelling was added.

Negative controls, all text-reverted and restored:

- Removing the three finding-3 bounds made
  `validate_divergence_rejects_unmatchable_targets_and_oversized_reason` fail.
- Removing the readiness cap made `the_readiness_line_is_size_bounded` retain
  104,096 bytes instead of 4,096. Its multibyte arm also caught the first fix
  reading a partial UTF-8 code point as `InvalidData`; the final byte reader
  retains the last complete character inside the cap.
- Restoring derived serde and the public `AccountId` field made
  `account_id_deserialization_enforces_the_type_invariant` accept the oversized
  id.
- Routing the hot decoder back through the internally tagged enum raised its
  probe from two allocations to four.

No tape-protocol bump is owed: no generator artifact, seed value, draw, origin
or generated byte changed. The exact-equality protocol frame table stays green.

## notes/bugs-protocol.md review and document close

A cold read of the round's uncommitted diff found ONE correctness defect, and
this pass found a second the round's own negative control had missed. The
document ends empty of open findings; every one of the nine carries a
disposition line, and the "Checked and found sound" section is preserved
verbatim.

- THE HOT DECODER REFUSED FRAMES THE OLD DECODER ACCEPTED. `from_json_str` and
  `from_json_slice` probed the `"type"` value as a BORROWED `&str`, and
  serde_json can only hand out a borrowed string when the JSON scalar contains
  no escape sequence. So a conforming peer spelling the tag with a `\uXXXX`
  escape - noncanonical, but valid JSON and accepted by the internally tagged
  decoder these helpers replace on the adapter's read path - was refused before
  the payload was ever looked at. THE SHAPE IS WHAT MATTERS: an optimization of
  a DECODER narrowed what it accepts, on the one crate both ends serialize
  against, and every gate stayed green because the server only ever emits the
  canonical spelling. The probe is now a `#[serde(borrow)] Cow<'de, str>`,
  which keeps the zero-copy path for the canonical form and owns a `String` only
  for an escaped one. `tag_probe_accepts_escaped_tags_exactly_as_the_general_decoder_does`
  pins acceptance parity in BOTH directions - what the general decoder takes and
  what it refuses - for a hot variant, a cold variant and three malformed
  frames.
- THE READINESS CAP TEST DID NOT BITE, and the fix was half a bound. The round
  wrote `BufReader::new(stdout).take(N + 1)`, which caps what is pulled OUT of
  the buffer while the buffer itself has already pulled a chunk from the child;
  and the test asserted only the refusal's shape, which the post-hoc truncation
  produces whether or not any cap exists. Deleting the `take` outright left the
  test GREEN while `read_ready` read all 104,096 hostile bytes. The `take` now
  wraps the CHILD stream, so the bound is on the read, and the test drives a
  counting reader and asserts at most 4,097 bytes were ever delivered. Verified
  to fail with the exact message `read 104096 of 104096 bytes` when the cap is
  widened. THE LESSON, the eighth non-biting test of this loop: a test that
  observes only an ERROR cannot distinguish a bound from a check performed after
  the damage, and the assertion has to be on the resource the finding named.
- THE ORDER-EVENT RESERVATION IS NOW PROVEN RATHER THAN ARGUED. Finding 5's
  corrected derivations SHRANK `ORDER_EVENT_MAX_BYTES` from 5,088 to 4,032,
  which is a tighter reservation on a live memory bound and cannot rest on a
  comment. `order_event_bound_covers_both_maximal_lifecycle_frames` constructs a
  maximal `OrderFilled` and a maximal `OrderRejected` with every string filled
  with U+0001 - the character `JSON_ESCAPE_FACTOR`'s six-byte charge exists for
  - and asserts domination. A future field added to either struct now fails here
  instead of silently voiding the bound.
- `Engine::arm` IS THE HOLE FINDING 3's GUARD CANNOT REACH, and it is closed at
  the other end. The validator is authoritative for every WIRE arming path, but
  `arm` is `pub` and takes a `Divergence` directly - the same shape as
  `set_margin_policy` in the engine arc. The engine now routes the echoed
  `RejectNextSubmit.reason` through `truncate_reason` at the point it builds the
  `OrderRejected`, so the byte reservation holds by construction for every
  arming path there will ever be. The unmatchable-ID half needs no such
  backstop: a dead queue entry is bounded by `MAX_ARMED_DIVERGENCES` and sheds.

Verified rather than accepted:

- FINDING 6 IS CLOSED BY CONSTRUCTION ON BOTH PATHS, enumerated rather than
  claimed. `AccountId`'s field is private; `parse` is the only constructor in
  the crate; there is no `From`, `FromStr` or `Default` impl and no other
  `Self(..)` site; `#[serde(transparent)]` now governs only `Serialize`, and
  `Deserialize` is hand-written through `parse`. Every construction site in the
  workspace - server config, engine, fill golden, sweeper, benches - already
  calls `parse`. The nautilus `AccountId` the adapter uses is a DIFFERENT type
  and is unaffected.
- FINDING 9 IS GENUINELY DEAD, checked against `docs/cli.md` and `a6f57760`
  rather than relayed. The doc states there is no `--seed` flag because a
  reproduced path is a written act and the config file's `seed` key is the one
  spelling; the commit is what established one seed per run and the readiness
  record as its sole report. The finding proposed a second spelling of a decided
  contract.
- FINDING 7 EARNS ITS CHANGE ON ALLOCATION GROUNDS ONLY, and
  `reference/performance.md` now says so in those words with a re-measurement:
  219 ns and 4 allocations for the internally tagged enum, 224 ns and 2 for the
  landed decoder, 103 ns and 2 for an unreachable plain-struct floor. IT IS
  5 ns SLOWER. It is kept because the reported defect was an allocation defect
  and the fix halves allocator calls on the adapter's per-tick path at no
  measurable throughput cost, with the wire bytes unmoved. The earlier
  244/121/245 reading is superseded and its plain-struct arm was not even the
  same parse - it carried a `trade_id` field `TradeTick` does not have. The
  probe now asserts field parity between its arms.
- THE EXACT-EQUALITY FRAME TABLE WAS NOT WEAKENED, it was WIDENED. It gained a
  `Trade` row, without which the table covered only one of the two variants the
  new fast path dispatches, and every row is now decoded through both public
  helpers and re-serialized against its literal.
- THE PROBE EXAMPLE IS REGISTERED, not stray and not deleted. `tag_decode_probe`
  is an `[[example]]` in `mogwai-protocol` and a `[mogwai.targets]` entry, so the
  performance table is re-runnable. Its registry comment records why it carries
  NO features and why the profiler modes do not apply: it wraps the global
  allocator itself, which `--alloc` would displace. The dead-code diagnostic is
  gone because the arms consume their fields through `black_box` rather than
  suppressing the lint.
- NO OTHER DECODE PATH NARROWED. The diff's decode-facing changes are the tag
  probe, `AccountId::deserialize` and `read_ready`; the latter two narrow
  DELIBERATELY and are recorded in `notes/todo.md` with what a consumer must do.
  No other borrowed deserialization was introduced anywhere in the round.
- NO `TAPE_PROTOCOL_VERSION` BUMP IS OWED. Nothing here reaches a generator,
  draw, seed value or tape origin; `seeds.rs` gained a comment and no
  arithmetic. 14 stays live, 15 stays reserved for protocol-12b.

Consumer-visible surface, recorded in `notes/todo.md`: the private `AccountId`
field and its validating deserialization, the three divergence payloads
`POST /control/divergence` now refuses with `400` where it used to arm or
silently truncate, and the launcher's 4,096-byte readiness ceiling. broadarrow
is the known consumer and nothing here can verify its build.

Bite checks, each reverted and restored as a TEXT EDIT: the `Cow` tag probe
(fails with `invalid type: string "Trade", expected a borrowed string`), the
`AccountId` deserialization guard (fails its `is_err` on the oversized id when
`parse` is bypassed), and the readiness cap (fails on the delivered-byte count).
All in RELEASE, and none rests on a `debug_assert`.
## notes/bugs-adapter.md, round 1 (findings 1 through 6)

Classification:

- Findings 1, 2, 5 and 6 REPRODUCED as written.
- Findings 3 and 4 REPRODUCED IN ALTERED FORM. Their named lifecycle failures
  were live, and the timeout cleanup also leaked the latency pump. More deeply,
  aborting the tracked outer connection task detached its bare split reader and
  writer JoinHandles, so even apparently correct task abortion left both socket
  halves alive.
- No finding in this round was dead.

Built:

- Spot instrument conversion now uses `CurrencyPair::new_checked`, so an
  invalid wire increment is a named conversion error and reaches the existing
  drop-and-warn boundary instead of panicking.
- Both client message translators warn with `skipped` and `sim_now_ns` on
  `FeedLagged`. The lifecycle still reconnects after WS 1011 and does not invent
  a downstream repair policy.
- Execution stop is unconditional. Both connect methods retire the previous
  transport generation, command sender and tasks before dialing, and timeout
  cleanup aborts the whole generation rather than only the reader.
- The lifecycle wraps each split socket child in abort-on-drop ownership. An
  aborted outer generation therefore cannot detach a reader or writer that
  keeps the socket alive.
- A failed venue-query send removes its registered oneshot waiter immediately.
- Order status reports carry a checked venue position id, matching fill reports
  and preserving hedging reconciliation identity.

Verification:

- Bite checks failed with each production fix textually reverted: invalid spot
  construction panicked; failed query send retained its waiter; order status
  lost its venue position id; reconnect-after-stop left its socket alive; and
  both double-connect tests observed that no replacement socket was opened.
  Restoring abort-on-drop ownership also changed all three lifecycle socket
  tests from red to green, proving the child-task guard is load-bearing.
- No tape generation path changed, so no tape protocol bump is owed. Version 14
  remains current and 15 remains reserved for protocol-12b.

## notes/bugs-adapter.md, round 1 audit pass

The cold read of the uncommitted round-1 diff found nothing. This audit
confirmed the four classifications and found TWO defects the round did not
reach, both of the loop's recurring families, plus one false claim in the
round's own report.

- THE CONNECTIVITY FLAG WAS A GUARD WHOSE CORRECTNESS RESTED ON DROP TIMING,
  and `abort_tasks` does not control drop timing. Cancellation is delivered at
  the aborted task's NEXT await point, so a reader caught between
  `connect_async(..).await` returning and its first select runs
  `connected.store(true)` to completion - AFTER the caller's `store(false)`.
  Both clients therefore had two live windows: a `stop()` racing a dialing
  reconnect leaves a dead client reporting `is_connected() == true` forever
  (nothing ever writes false again), and a second `connect()` has its
  `wait_connected` satisfied by the RETIRED generation's flag, so it returns
  success for a socket that never opened. Fixed by CONSTRUCTION, not by
  ordering: `retire_connected_flag` REPLACES the `Arc<AtomicBool>` rather than
  storing into it, at all six sites (both `stop`s, both `connect` heads, both
  `wait_connected` failure paths). The retired reader then writes to a cell
  nobody reads. The `connected` Arc is cloned into exactly one place per client
  - the reader task - which is what makes the swap sufficient; that was
  enumerated from the code, not assumed.
- AN EMPTY WIRE SYMBOL PANICKED THE UNSUPERVISED READER, and the round's claim
  that `CurrencyPair::new` was "the one panicking constructor left" is FALSE.
  `convert::instrument_id` built its id with `NautilusSymbol::from`, and every
  identifier `From<&str>`/`From<String>` impl in nautilus is generated by
  `impl_from_str_for_identifier!` to call `new`, which `expect_display`s past
  `check_valid_string_utf8` - a refusal for an empty OR all-whitespace string.
  `mogwai_protocol::Symbol` is an unvalidated `Arc<str>` since the `bugs-data`
  round 2 change, so a venue putting an empty `symbol` on the wire downed the
  spawned reader, poll or report task outright. `instrument_id` is now fallible
  and its seven call sites drop the frame or the report with a named warning,
  in the same shape the crate already used for `price`, `quantity`, `money` and
  `TradeId`. `instrument_any` reuses the validated `id.symbol` so the panicking
  `From` impl is off that path entirely.
  `an_empty_wire_symbol_is_refused_not_panicked` covers both the empty and the
  all-whitespace spelling.
- FEEDLAGGED STAYS A LOG LINE, AND THAT IS A RULING RATHER THAN AN OMISSION.
  An invariant wants an assert, a type or a guard; none of the three is
  reachable here. Nautilus's `DataEvent` has no gap or degradation variant, the
  client reaches the host as a `dyn DataClient` with no downcast,
  `is_connected` is true throughout because the socket never broke, a
  fabricated `InstrumentStatus` would report a halt that did not happen, and
  refusing the socket would convert a recoverable gap into an outage. Both arms
  are now `tracing::error!` rather than `warn!` - a data client declaring lost
  data is not warning-grade - with prose naming the consequence (wrong
  aggregation over the span; a possibly divergent order mirror on the exec
  socket). The cross-repo half is filed in `notes/todo.md` as a standalone
  writeup.

Verified rather than accepted, each against the code or the pinned nautilus
release read from `research/`:

- CONNECT AND STOP ARE DEFINED, not merely non-crashing.
  `ExecutionClientCore`'s `set_started`, `set_stopped`, `set_connected` and
  `set_disconnected` are plain atomic stores with no state machine and no
  panic, so `stop` without `start`, `stop` after a failed `connect`, and
  repeated `stop` are all well defined; the `is_stopped()` early return the
  round deleted was skipping TRANSPORT teardown on a component-flag
  technicality, which is what made it a bug. Nautilus wraps `connect` and
  `disconnect` with no idempotence guard of its own, so a double connect
  reaches the adapter and the adapter owes the definition.
- EVERY SPAWNED TASK AND GUARD WAS ENUMERATED. Tracked in `task_handles` and
  therefore covered by `abort_tasks`: the outer `run_ws_connection` task, the
  latency pump, and every request or instrument-seeding HTTP task. Owned by
  `AbortOnDrop` inside one connection generation: the split writer and the
  split reader, so aborting the outer task can no longer detach either socket
  half. No mutex guard is held across an await anywhere in the crate, and it
  cannot be: `tokio::spawn` demands `Send` and `std::sync::MutexGuard` is not,
  so the structural property is enforced by the compiler rather than by review.
  `abort_and_join` takes the handle by value out of the guard before awaiting,
  so the `Drop` impl cannot double-abort or detach.
- A WAITER CANNOT LEAK ON ANY PATH. Registration and the send are adjacent with
  no await between them; a send failure unregisters explicitly; `stop` and
  `connect` both clear both maps; an `AdmissionRejected` for a query removes
  the keyed waiter; and a dropped sender wakes the requester with `RecvError`
  immediately rather than at the timeout. A late registration from a
  previous-generation `VenueQuery` is self-cleaning, because its snapshotted
  sender is closed and the send therefore fails.
- ADMISSION REFUSALS ARE NOT SWALLOWED. A submit, cancel or modify subject is
  re-entered as a real `OrderRejected`, `OrderCancelRejected` or
  `OrderModifyRejected` event, so the transport and protocol arcs' bounded
  command queue surfaces to nautilus as an order event rather than a lost
  command. None of the narrowings those arcs landed - the 64 KiB frame cap, the
  bounded `AccountId`, the capped readiness line, `Symbol` as `Arc<str>`, the
  503 history refusal - breaks a host driving this adapter, so none owed a new
  `notes/todo.md` entry; the ones with consumer impact are already recorded
  there from their own rounds.
- NO TAPE PROTOCOL BUMP IS OWED. The adapter reaches no generator, draw, seed
  derivation or tape origin, and nothing under `analysis/` or the goldens
  moved. 14 stays live, 15 stays reserved for protocol-12b.

Bite checks, each reverted and restored as a TEXT EDIT in release:

- `a_failed_query_send_unregisters_its_waiter` fails on "send failure must not
  retain a waiter" when the `unregister` call is removed.
- `connecting_twice_replaces_the_execution_socket` fails 1 versus 2 on
  `ws_hits` when the connect-head cleanup is removed. Worth knowing why it
  bites: the stub venue serves one connection at a time, so the second dial
  cannot be upgraded until the FIRST generation's socket is actually closed -
  the assertion is on the venue's observed behaviour, not on adapter
  bookkeeping. Both double-connect tests and the reconnect-after-stop test
  drain to a deadline on `active_ws` rather than assuming the next frame.

Disclosed residual, not fixed: an `AdmissionSubject::Frame` refusal on the
execution socket means the venue dropped a whole outbound batch, and the
adapter only logs it. Recovery exists (the venue-truth report generators) but
nothing here triggers it. Same shape as the `FeedLagged` ruling above and
covered by the same `notes/todo.md` entry.

## notes/bugs-adapter.md, round 2 close

- Mass status now includes every historical fill belonging to an open order,
  even when the fill predates the reconciliation lookback. This gives Nautilus
  the fill price and identity needed to pair a partially filled market
  remainder without relying on a nonexistent order-report `avg_px`.
- Timestamp-only history pagination no longer advances past rows it has not
  seen. The rule is stated once and applied on both paths: a cursor may only
  advance onto a timestamp whose rows have ALL been disclosed. See the review
  and close pass below, which replaced this round's page-wide check.
- `account_id` is explicitly a local Nautilus label. Both configs default it to
  `MOGWAI-001`; validation checks syntax only, and the stale account-query and
  server-slot documentation is gone.
- The execution mirror dropped its unused quantity, price, filled quantity,
  average price and acceptance timestamp. The unchecked fill accumulator and
  its dead notional arithmetic disappeared with those fields.
- Futures instrument conversion refuses any definition that is not precision
  zero with a one-contract increment. This matches both Mogwai's binding config
  contract and Nautilus 0.61's `FuturesContract::new_checked`, which constructs
  whole-contract sizing unconditionally.
- The data socket now uses a no-command lifecycle path, with no uninhabited
  command enum, sender or receiver. Pre-subscription quote retention is capped
  at 64 orphan symbols, and missing-instrument warning deduplication belongs to
  each data client rather than the process.
- Execution feed gaps remain blocked on Nautilus. `ExecutionEventEmitter` can
  publish a report but the synchronous frame handler cannot truthfully build
  one without asynchronous venue queries, and the client API exposes no
  request-reconciliation callback. The ERROR-level local mitigation and the
  standalone cross-repo item in `notes/todo.md` stand.
- No tape protocol bump is owed. All changes are inside `mogwai-adapter`, its
  tests and prose; no generator, draw, seed, fingerprint or tape origin is
  reachable. Version 14 remains live and 15 remains reserved.

## notes/bugs-adapter.md, round 2 review and close pass - THE LOOP ENDS HERE

The cold read found ONE correctness issue and it was the loop's recurring
disease in its purest form: a boundary that silently loses data, fixed for the
degenerate shape of the boundary and still losing data in the ordinary one.
That, plus four smaller corrections, closes the document and the arc.

- THE PAGINATION FIX ONLY COVERED THE DEGENERATE PAGE. `same_ts_wedge` fired
  only when a full page's FIRST and LAST timestamps matched, so a page of
  49,999 rows at instant A plus one row at instant B advanced to B + 1 and
  every remaining row at B vanished - finding 8's exact loss, in the shape a
  real tape actually produces. Replaced by the INVARIANT rather than another
  special case: a timestamp-only cursor may advance onto an instant only once
  every row at that instant has been seen, so a full page keeps its complete
  PREFIX, drops its trailing timestamp group, and re-requests from that
  group's own instant (the endpoint's start bound is inclusive, which
  `MergeSource::starting_at` buffers). The degenerate page has no complete
  prefix and so is the ONLY case that still reports truncation. Both paths use
  one helper, `final_ts_group_start`. Progress is structural: a page with two
  distinct timestamps has `group_ts` strictly greater than its own first row,
  so the cursor cannot stall, and the page cap still bounds the loop.
- THE STUB COULD NOT HAVE CAUGHT IT. `trades_pages` replays queued bodies
  whatever the client asks for, so no assertion against it can observe a
  skipped cursor - the round's own regression, and the older seam test, were
  both blind by construction. `StubState` now carries `trades_tape` and
  `quotes_tape`, served with the REAL inclusive-`start`, `end` and `limit`
  semantics, plus `trades_starts` / `quotes_starts` recording each requested
  cursor. The three pagination tests run against tapes and assert both the row
  set and the cursor sequence. Do not add another page-queue pagination test.
- MASS STATUS ASKS FOR FILLS ONCE, NOT TWICE. The round issued a second full
  `QueryFills` and APPENDED the older fills after the recent ones, so a
  partially filled order's group reached nautilus out of chronological order.
  It is now one unbounded query filtered locally, which preserves the venue's
  own ordering and halves the round trips. The test pins the trade-id sequence,
  not just the count.
- A REFUSED EXECUTION FRAME LOGS AT ERROR, matching the `FeedLagged` arm it
  shares a cause with. The two are the same event class - order events the
  host will never see - and one of them was `warn!`.
- FINDING 12 STANDS CLOSED BY DISCLOSURE, but the argument is now the
  EXECUTION socket's own rather than inherited. `ExecutionEvent::Report` does
  exist, so the emitter is not what is missing; what is missing is a truthful
  report. Every truthful set here comes from an async venue query, and the
  translator runs as `handler(msg).await` INSIDE the reader's frame loop, so a
  query issued there waits on a reply only that blocked loop can read. Nor can
  it be spawned: the client owns `Rc<RefCell<Cache>>` through
  `ExecutionClientCore` and is `!Send`. Rebuilding from the local mirror would
  assert as truth the frames the venue just said it dropped. The `notes/todo.md`
  entry now names both halves nautilus would have to ship - a data-side
  degradation event AND an execution-side client-initiated reconciliation
  request.
- Two bounded-state corrections. The orphan quote cap counts ORPHANS ONLY, so
  live subscriptions cannot crowd a client out of its own pre-subscription
  cache, and nothing ever evicts, so a symbol that has a row keeps it. The
  per-client missing-instrument warn set is capped at 256 symbols and, past the
  cap, warns on every miss rather than growing - the loud direction for a
  diagnostic.

Bite checks, each reverted and restored as a TEXT EDIT: the trade split-group
test fails 50,000 versus 50,002 against the old advance, the quote split-group
test fails identically, `mass_status_pairs_an_open_orders_fill_outside_the_lookback`
fails on "an open order's fills are reported" when the carve-out is dropped,
and the futures refusal fails on `unwrap_err` - which also proved the finding's
premise live, since nautilus happily built a `size_precision: 0` contract from
a `size_precision: 1` definition.

Consumer-visible, recorded in `notes/todo.md`: `UNSET_ACCOUNT_ID` is gone and
`account_id` no longer refuses a default, because the venue has one ledger and
carries no account identity on the wire.
