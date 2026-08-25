# Bug-loop carry-forward

State the bug-hunt loop's agents cannot see, because each one arrives with only
its own round. Every brief carries the relevant slice forward. Current as of the
`notes/bugs-venue-mechanics.md` arc, round 2, which closed findings 6 through 9
and ruled on the structural recommendation. That document is now deleted: every
finding in it is closed, and the two rewrites it proposed are refused with
reasons recorded below rather than deferred. The `notes/bugs-engine.md` arc
below it is closed and judged sound. What is owed next is the close pass over
the whole arc.

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

## Machinery agents may build on and must not break, from bugs-venue-mechanics round 2

- `Rivers::last_trade_at_or_before` reports every materialization, positioning
  and budget refusal as an error. A walk that finds a print on the last tick of
  its budget returns that print before judging exhaustion. `None` now means only
  that the reachable tape genuinely had no print at or before the instant.
- `Rivers::ensure_reach` preserves `MaterializeRefusal`. Socket history derives
  retryability from `is_venue_fault`, so a venue-owned synthesis failure is
  retryable and a permanent resolution or river-cap refusal is not. Do not
  flatten the type before that decision.
- The fill golden records every resting limit's drawn trigger distance. Its
  zero-width scenario must contain only zero offsets and its shipped-band
  scenario must contain at least one non-zero offset, before the committed
  artifact is compared or written. One-second fill latencies remain equal
  between the two scenarios and are no longer asked to prove the band moved.
- Risk enforcement resolves every extreme valuation before it mutates the
  ledger. One unvaluable extreme refuses the whole policy span. After a lock
  breach, the closing equity is still observed so the locked ledger's peak
  ratchet agrees with the close; the first breach remains the action returned.
- A ticket decrements a placed boat only when its `Arc` is the allocation the
  registry currently holds under that sharing key. Passenger teardown ordering
  still makes a mismatch unreachable, but a future violation cannot decrement
  or remove a replacement boat.

## Decisions from bugs-venue-mechanics round 2

- No `TAPE_PROTOCOL_VERSION` bump is owed, settled independently by the
  fix-and-commit pass rather than taken on the fix pass's word. Two things had
  to hold and both do. First, the re-blessed artifact is not on the generation
  path: `tests/golden/fill_distribution.json` is read only by its own
  comparison in `fill_golden.rs` and is `include_str!`d by nothing, so no
  generator can consume it. Second, the fill band's draw - which the bump rule
  names explicitly, and which finding 7 sits directly on - did not move:
  `draw_offset`, `draw_key` and `draw_trigger` are untouched, and the artifact
  diff proves it, since every pre-existing field is byte-identical and the only
  edits are the schema number and the added `trigger_offset_ticks` vector. Had
  the draw moved, the latency and fill vectors would have moved with it. The
  new observation calls `Engine::pending_scans`, which takes `&self`.
  Re-blessing a golden that only observes generated behaviour is not the same
  event as committing a changed generated artifact, and only the second owes a
  bump.
- `TAPE_PROTOCOL_VERSION` is not folded into `RiverKey`. It is a build identity,
  and one process cannot hold rivers from two builds; the boatyard comment now
  states that scope instead of claiming the compile-time constant is a key
  field.
- **The structural recommendation is refused, on the merits, and is not to be
  relitigated.** Both halves were re-read at their sites by the fix-and-commit
  pass and both proposals are wrong about the code they propose to replace.
  - `apply_engine_pass_on_clock` returning "one snapshot taken after every
    mutation" unconditionally would resurrect an account update that
    `DropNextAccountUpdate` exists to suppress. The divergence works by there
    being no `AccountState` in the phase's event vector at all, so the
    presence check the recommendation calls a `retain` dance is carrying the
    divergence semantics, not cleverness. Round 1 already took the half of the
    proposal that was right - the value is recomputed at the end of the phase
    rather than selected by vector position - and kept the half that is
    load-bearing.
  - `PriceExtremes` collapsing to a single `Mutex<Option<PriceSpan>>` rests on
    the claim that "the lock is only touched when an extreme moves, so the
    atomic-epoch optimization is buying approximately nothing". That claim is
    false of the proposed replacement. The epoch is what lets the tape thread
    hold its running extremes in its own `SpanWriter` stack and fold a print
    without any lock; with the span behind a mutex and no writer-local state, a
    print cannot know whether it moves an extreme without first locking to
    read the span. The proposal therefore adds a mutex acquisition to every
    trade on the venue's hottest path, and it would discard two barrier-pinned
    regressions covering a race that was a real silent wrong answer.

  The loop's standing rule is build rather than defer, and this is a refusal
  rather than a deferral: nothing here is owed later.
- The checkpoint stride and resident volatility window remain unchanged. Their
  cross-crate and equal-budget premises are now named at the constants and call
  site that rely on them.

## Hazards this arc paid for, from bugs-venue-mechanics round 2

- A budget check after a pull is not enough: it must return an answer found on
  the final admitted tick before reporting exhaustion. The regression pins both
  halves with the tape's opening quote and trade.
- A cadence-level golden can be byte-identical across two mechanisms that differ
  below its observation interval. Record the mechanism's direct output when the
  downstream observable quantizes the distinction away.
- A policy span must be valued before it is folded. Logging and skipping an
  unvaluable member leaves a partially mutated ledger, which is the same
  frontier defect as advancing over only the work that answered.
- To bite a golden that records a drawn value, perturb the draw in its callee
  and check which assertion fires. `draw_offset`'s `random_range(0..=band_ticks)`
  is the lever: `band_ticks.min(0)` fires the shipped-band assertion and
  `band_ticks.max(3)` fires the zero-width one, so both halves of the new gate
  are pinned. That perturbation is also the proof the old schema-2 golden was
  vacuous here - forcing every draw to zero leaves every schema-2 field of the
  committed artifact byte-identical, because the banded and unbanded fill
  outcomes already agreed.
- A two-assertion test needs each assertion bitten with the other left passing,
  or the first one masks the second. The budget regression was bitten twice:
  the old `return Ok(None)` inside the drain loop for the refusal half, and
  reordering the `drained == budget` bail ahead of the `last.is_some()` return
  for the retained-print half. Reverting the loop shape alone only ever fires
  the first assertion.

## Least-examined after round 2, for the close pass

Everything here was read and judged acceptable rather than missed, but it is
where the next look is worth most.

- **The typed refusal is checked but not classified, one layer out.** Making
  `Rivers::ensure_reach` return `MaterializeRefusal` forces every caller to
  handle the type, which is what closed finding 8 at the socket history site.
  It does not force a caller to *classify*: the cheap escape is
  `.map_err(anyhow::Error::new)`, and `http.rs` takes it at both `/trades` and
  `/quotes`, where every refusal - a spent river cap included - still becomes
  one `500`. That was already true before this round and the round did not
  widen it, so it was left alone; but the operator HTTP surface is the same
  lost classification finding 8 named, on a different route, and it is a real
  open item rather than a settled one.
- **The classification regression tests `Refusal::materialization`, not
  `serve_page`.** The doctrine hazard round 1 hit three times. The call site is
  one readable line and was audited by reading. A call-site regression was
  considered and not laid: the only refusal that discriminates the fix from the
  old unconditional `retryable: true` is `CapacityExhausted`, and reaching it
  through `serve_page` means materializing all 256 rivers of
  `MAX_MATERIALIZED_RIVERS`, each a real generator chain. `KeyMismatch` is a
  venue fault and so answers `true` either way.
- **The `Ticket::drop` identity guard ships with no regression, deliberately.**
  A boat is removed only at `passengers == 0`, when no ticket remains, so no
  live path can put a replacement `Arc` under a key a stale ticket still holds.
  The guard turns an unenforced premise into a check and logs on violation;
  constructing a witness would mean hand-building a `Slot::Placed` the registry
  could not otherwise reach. Recorded as "cannot bite today" rather than left
  looking gated.
- **`last_trade_at_or_before`'s positioning refusal has no test.** The
  `try_source_before_target` returning `None` branch now bails instead of
  answering `Ok(None)`, and it is covered by the type change alone.
- The `CHECKPOINT_K` cross-crate comment landed orphaned between that constant
  and `MAX_MATERIALIZED_RIVERS`'s doc, reading as though it described the river
  cap. Folded into `CHECKPOINT_K`'s own doc comment, with the reason the stride
  is outside `TAPE_PROTOCOL_VERSION`'s reach spelled out. Worth remembering
  that a bare `//` between two documented items attaches to neither.

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
