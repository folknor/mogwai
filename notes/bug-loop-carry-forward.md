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
