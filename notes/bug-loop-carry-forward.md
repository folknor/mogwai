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
