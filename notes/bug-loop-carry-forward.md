# Bug-loop carry-forward

State the bug-hunt loop's agents cannot see, because each one arrives with only
its own round. Every brief carries the relevant slice forward. Current as of the
`notes/bugs-venue-mechanics.md` arc, round 1, which closed findings 1 through 5
of that document; findings 6 through 9 and the structural recommendation are a
later round. The `notes/bugs-engine.md` arc below it is closed and judged sound.

## Machinery agents may build on and must not break, from bugs-venue-mechanics round 1

- **A sweep pass emits at most one `AccountState` from the engine phase, and it
  is recomputed at the end of the phase**, never selected by vector position.
  Marking is computed before expiry and funding but its events are appended
  after both, so `rposition` picked the oldest ledger state. The presence of a
  surviving snapshot still decides whether one is emitted at all, which is what
  keeps `DropNextAccountUpdate` working - do not turn the recompute into an
  unconditional push. The invariant is scoped to that phase: `enforce_policy`
  legitimately appends more `AccountState` frames afterwards, and the comment
  now says so rather than claiming one per pass.
- **`PriceExtremes` epoch handling is a two-sided contract.** The epoch read at
  the top of `record_with` may be stale by the time the print is published, so
  the writer re-reads it under the `published` lock and, on a moved epoch,
  opens the new span from this print. A print that moved *neither* extreme of
  the old span takes the same path: it re-reads the epoch off the mutex, and
  only returns early when the epoch is genuinely unchanged. Both halves are
  needed and both have their own barrier-pinned regression. Never reintroduce
  an early return that skips the recheck.
- **An unpaced boat (`speed_micros() == 0`) does not use the tape thread's
  extremes accumulator at all.** Its publisher runs ahead of the boat clock, so
  `fills::price_span` regenerates the clock-bounded span from the same river
  and `commit_pass` takes the accumulator only on a paced boat. `tape.rs`
  correspondingly stops recording when `spawn.speed == 0.0`. The two predicates
  agree exactly because `BoardRequest`'s speed is quantized through
  `BoatKey::speed()` before `TapeSpawn` sees it - if either side stops going
  through the quantized value they diverge silently.
- **Risk precedence: a terminating rule wins over a lock on the same reading**,
  and same-action rules keep evaluation order. **A lock is acted on once**:
  `observe` returns `Clear` while `locked`, after the day-boundary lift and
  after the peak ratchet. The ratchet deliberately still runs while locked -
  the peak is a property of the tape, not of the account's permission to trade.
- **`readable` is computed per account seat**, from the boats that account is
  seated on, because the predicate that decides cancellation must be the
  predicate that decides sweeping. It is sourced from a fresh
  `boatyard.boats()` read taken *after* the freeze filter, not from `next_due`:
  an upgrade places its boat before it attaches the connection, so an account
  observed attached has its boat in that list, while `next_due` was sampled
  before the pass slept and can miss a boat boarded during the sleep. Reusing
  `next_due` there would cancel the orders of an account that had just sat
  down.

## Hazards this arc has already paid for, from bugs-venue-mechanics round 1

- A race regression must pin the interleaving with barriers, and the barrier
  hook has to sit at the point the branch under test reaches. Round 1's
  finding-2 test was correctly barrier-pinned and still could not reach the
  non-moving-print branch, because its single print always sets `moved = true`
  and the hook fired after the `!moved` early return. A race test that only
  covers the easy branch reads exactly like one that covers both.
- Three of round 1's five regressions test a pure helper (`readable_symbols`,
  `commit_pass`) rather than the call site that feeds it. They bite, but they
  would still pass if the sweeper passed `|_| true` or a wrong `unpaced`. The
  wiring is one readable line in each case and was audited by reading; a later
  round wanting stronger coverage needs a two-account, two-boat sweeper
  fixture, which does not exist yet.
- `Rivers::history_source` yields the same realization the tape thread
  publishes - `FlowSurge` mutates the live river so prints, history and trigger
  decisions agree - which is what makes regenerating a span off the river
  equivalent to reading the accumulator. If that ever stops being true, the
  unpaced path silently observes different water than the passengers saw.

## Decisions already ruled on, from bugs-venue-mechanics round 1

- **No `TAPE_PROTOCOL_VERSION` bump is owed by this round.** `mogwai-data`,
  `analysis/` and the fingerprint were untouched. `mogwai-venue`'s `tape.rs` is
  the publication thread, not the generator: gating the extremes recording
  changes no tick, no seed derivation, no arrival clock and no fill band draw.
  `fills::price_span` builds a fresh `history_source` per call exactly as
  `read_marks` and `scan_triggers` already do, so it consumes the generator
  rather than advancing shared state.
- The one-sweep-interval window in which a boat placed during the pass's sleep
  is missing from `next_due` still exists for the *sweep* itself. It is closed
  for cancellation by the fresh `boats()` read above; closing it for the sweep
  would mean re-deriving `next_due` after the sleep, which changes the cadence
  schedule. Left alone deliberately.

## Machinery agents may build on and must not break, from the bugs-engine arc

- **Group closing linkage uses the booked fill quantity**, never quantities
  reconstructed from the emitted `VenueMessage` stream. `on_submit_from` returns
  the booked quantity for this reason. The general invariant, now written into
  `reference/architecture.md`: no engine control flow may be derived from a
  `Vec<VenueMessage>` that divergences have already touched.
- The two surviving production readers of that vector are audited and safe:
  `orders.rs` `account_changed` asks presence rather than magnitude, and
  `on_submit_group`'s pass-two refusal scan runs after `RejectNextSubmit` is
  consumed group-wide.
- The group dry pass now sees reserved id prefixes and active fee surcharges.
- `on_modify` enforces the round-lot rule, refuses price amendments on
  market-on-trigger and trailing-stop-market remainders, and rederives a
  `TrailingStopLimit`'s limit from an amended trigger.
- `on_modify`'s `OrderUpdated` frame publishes `order.submit.price`. It must not
  become `price.or(derived_price)`: that publishes a null price on every
  quantity-only amend of a limit order.
- **Venue-originated liquidations are minted in sorted order.**
  `apply_margin_breaches`, `liquidate_all` and `retire_off_river` each sort by
  `(symbol, position_id)` before minting, because `liquidation_seq`, the `LQ-` /
  `RISK-` ids, the venue and trade ids behind them and the event order all
  follow from it, and the positions live in a `HashMap`. Written into
  `reference/architecture.md`. Any new venue-originated batch owes the same
  sort.
- **Ambiguous balance valuation is a `min_by` over the symbol**, not a
  `find_map` over `self.instruments`. Documented in `reference/architecture.md`
  as a stable tie-breaker, not a claim the pairs share a market.
- **The two margin-equity sell folds count the same orders.**
  `margin_equity_sell_holds` (the `locked` side) and `worst_case_leaves` via
  `margin_equity_sell_hold_with_pending` (the `initial` margin row) are two
  implementations of one quantity with no shared fixture - the doctrine's named
  hazard. A price-less sell now contributes its quantity to both and a price to
  neither. Do not reintroduce a `continue` that drops it from one side.
- **Saturation warnings are keyed by `account::SaturationKey`**, a namespaced
  enum, because a symbol and a currency may legally share a name on an open
  instrument set. `Warned::saturated` is a `RefCell` so `free_balance`, which
  takes `&self`, can warn on its own clipping.

## Hazards the bugs-engine arc already paid for

- Group admission atomicity is backed by a `debug_assert`, so a group regression
  test is structurally prone to passing vacuously in release. Assert the
  whole-group rejection wording, not the rejection count.
- Twelve commits running, the expensive defect has been in the half nobody cold
  reviews. The round-1 wire regression sat in the fix pass's own production diff
  and passed both the cold review and a green 1419-test gate.
- A determinism test over a two-entry `HashMap` is a coin flip against its own
  defect, not a test. Round 2's four ordering tests all reverted green about
  half the time; the fix pass read that as "failed nondeterministically" and
  moved on. The witness has to be derived: rebuild the engine until the map
  hands out the non-lexical key, then exercise the path. `mogwai-engine`'s
  `engine_iterating_non_lexically` is the worked example.
- And the predicate must name the map the path under test actually walks.
  Pinning `account.positions` left `apply_margin_breaches` a coin flip anyway,
  because that function takes its order from `self.margin.keys()` and only the
  later sort makes the output lexical. One helper, two different predicates.
- The cold review can be right about a discrepancy and wrong about its
  reachability at the same time. Round 2's finding 7 was a genuine disagreement
  between two folds, provable with a hand-built witness, and unreachable through
  any wire path. Neither "it reproduces" nor "it does not reproduce" was the
  answer; closing it by making the two folds agree by construction was.

## Decisions already ruled on in the bugs-engine arc, not to be relitigated

- Splitting the overloaded `apply_divergences` flag is the durable fix for the
  dry-pass blindness. It is recorded as owed in `reference/architecture.md`, and
  the local guards that landed in round 1 are the interim.
- `clear_armed` stays as it is. Shed-oldest is the intended live bound on the
  armed queue and explicit clearing is test machinery; the absence of a live
  escape hatch is the design, not a gap.
- `MAX_GROUP_ORDERS` stays unasserted against `MAX_LINKED_ORDERS`. It is already
  defined as `MAX_LINKED_ORDERS + 1`, so the identity is in the definition and a
  second assertion of it would restate rather than gate.
- The engine's execution paths are not the tape generation path.
  `TAPE_PROTOCOL_VERSION` was not bumped in either round of this arc, matching
  the round-1 precedent, which moved fill and linkage behaviour far more than
  round 2 did. `mogwai-data`, `analysis/` and the fingerprint were untouched
  throughout.

## The close pass, and how the bugs-engine arc ended

The close pass reviewed the whole `d58ff1e..HEAD` arc and closed it. What it
found and did:

- The `SaturationKey` / `RefCell<HashSet<..>>` conversion in `account.rs`, its
  call sites, and the `BTreeSet` currency union in `snapshot` were re-read in
  full and are sound: every borrow of the `RefCell` is confined to
  `warn_saturated`, `free_balance`'s warn fires after its folds, and the union
  preserves the sorted-unique contract the old `Vec` sort provided.
- The `MarketToLimit` residual was settled the way round 2 suspected: the
  protocol doc was the stale half. Its "broken in both halves" paragraph
  described the pre-2026-08-19 engine; `orders.rs` prices the executing part
  off the tape bounded by the stated limit and rests a kept remainder as a
  scannable limit. The doc comment in `mogwai_protocol::messages` now states
  the implemented model and names the old text as the closed defect record.
- The four round-1 residual rulings (the overloaded `apply_divergences` flag
  with its owed split, `clear_armed`, `MAX_GROUP_ORDERS`, and the unbumped
  `TAPE_PROTOCOL_VERSION`) were re-verified and stand. Nothing in the arc
  touched `mogwai-data`, `analysis/` or the fingerprint, so no bump was owed.
