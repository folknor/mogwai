# Bug-loop carry-forward

State the bug-hunt loop's agents cannot see, because each one arrives with only
its own round. Every brief carries the relevant slice forward. Current through
the close pass over the now-exhausted `notes/bugs-protocol-cli.md` arc, whose
decisions are directly below. The bugs-protocol-cli, bugs-venue-serving,
bugs-venue-mechanics and bugs-engine arcs are all closed and judged sound; the
close-pass records are at the bottom. `notes/bugs-adapter.md` has not been
worked at all, which is why this carry-forward stays live. The adapter arc is
next, and the adapter is the one crate that touches nautilus.

What the adapter arc should carry from the protocol/CLI arc: the round-2 fix
pass closed a finding by writing a sentence into `reference/architecture.md`,
and the sentence was false about the very path the finding named. Both halves
of that matter - a durable claim is owed the same verification as a code
change, and closing a "the ordering happens to be safe" finding with prose
closes nothing.

## Machinery agents may build on and must not break, from bugs-protocol-cli round 2

- **`http::BoundaryCleared` is a witness, and it is the enforcement of finding
  7.** `boundary_outcome` returns `Result<BoundaryCleared, OrderOutcome>` and
  mints the witness on the arm where its own validation found no fault;
  `ExecLanes::reserve` demands one. The struct's field is private to `http`, so
  the boundary is the only code in the crate that can produce it and no future
  call site can size engine output for a command nobody validated. Round 2's
  first attempt closed finding 7 by writing the ordering into
  `reference/architecture.md`, and the sentence it wrote was false; a sentence
  in a reference document is not enforcement, which is the general lesson.
- **There are two output-byte admissions on the order path and they are not the
  same contract.** Engine-output admission (`worst_case_output_bytes` under the
  engine lock) is unreachable without the witness, so a malformed command never
  reaches it and never reads as capacity. Boundary-refusal admission exists
  *because* the command is malformed - a refusal is a produced frame and is
  charged - so a boundary with no budget to state its refusal answers a
  retryable `AdmissionRejected` instead, and the malformed verdict is deferred
  to the retry rather than lost. Both are stated in `reference/architecture.md`
  and the second is pinned by
  `a_boundary_refusal_it_cannot_afford_is_answered_as_capacity`, bitten on all
  three of its assertions.
- **Neither reservation clamps its member count at `MAX_GROUP_ORDERS`, and the
  reasons differ.** `worst_case_output_bytes` charges the actual count because a
  clamp would stop being an upper bound the moment an invalid command did reach
  the engine; the count is exact, so it can never undercount, and it is the
  ordering rather than a clamp that keeps the number operationally small.
  `boundary_frame_count` charges the actual count because it answers a command
  that already failed validation - an over-long group is one of the things
  validation fails it for - so a clamp there would reserve for fewer frames than
  `boundary_refusal` writes. What bounds that one is
  `MAX_INBOUND_MESSAGE_BYTES`, not the validator. The old claim on
  `try_reserve_boundary_frames` that `MAX_GROUP_ORDERS` bounds it was false and
  is corrected.

## Decisions from bugs-protocol-cli round 2

- **The per-order-type field table is refused as an implementation and built as
  a test, and this is not to be relitigated.** The decisive argument is that the
  cell finding 1 was missing - `Market` against `price` - inverts across
  `stamp_market_price`, so a static per-type table would have to state one of
  the two values and be wrong on the other side of the stamp. A table that could
  not have held the defect it was proposed for is not the implementation's
  shape. Nor are the protocol and engine matches two copies of one lattice: the
  protocol owns the full wire shape including phase-specific pricing, trailing
  offsets, expiry and linkage, while the engine deliberately repeats a narrower
  defensive subset and then adds instrument and book rules, and the one recorded
  drift between them (the post-only list) was closed by a shared predicate,
  which is the shape already in use. What the proposal was right about is
  exhaustiveness, and that half is built:
  `every_order_type_owes_or_bars_each_price_field` states all nine rows of
  required-or-forbidden over `price`, `trigger_price`, `trail_offset` and
  `limit_offset` as data and asserts every cell in both directions, so a new
  `OrderType` fails to compile until its row is written and a wrong row fails
  the test. `the_market_price_cell_is_the_one_the_phase_decides` covers the cell
  the table cannot hold. Both bitten by deleting the corresponding guard arm.
- The claims in `docs/cli.md` about the launcher were re-verified against
  `launch.rs` and `serve.rs` rather than taken from the fix pass: the owner
  loop's `try_wait` on both arms with `reaped` read from the `exit` slot before
  the teardown kill, `read_ready`'s `take` wrapping the raw stream rather than
  the `BufReader`, `record_stderr_line`'s single elision marker, and
  `arm_parent_death_signal` comparing a zero or negative `--launcher-pid`
  against `getppid()` and bailing before any signal target exists. All hold.
- `validate_modify_order`'s corrected doc was verified against the engine half
  it defers to: `on_modify` is where a trigger amend is refused on anything but
  an untriggered conditional, and where a price amend is refused on a market
  remainder. The two-phase split the doc now describes is the code.
- **A flat test count was a finding again, and the answer this time was
  benign.** The round-2 fix pass reported a multi-currency regression added and
  the gate count did not move, because its only test edit rewrote
  `policed_opening_balances_may_not_leave_the_policy_currency` in place, from a
  two-currency `contains` pair to a three-currency full-message equality. The
  rewrite is a real strengthening and both of its halves bite - reversing the
  sort fires the stable-order assertion, truncating the list to one fires the
  names-every-currency one - but "added" was the wrong word for it. Read the
  count after every fix pass; twice in this session the count was right and the
  summary was not.
- No `TAPE_PROTOCOL_VERSION` bump is owed, verified rather than assumed.
  Nothing under `mogwai-data`, `analysis/` or the committed fingerprint changed,
  and no edit in this round reaches a generator constant, an arrival clock, a
  seed derivation, the fill band's draw or the tape origin. Order validation and
  output admission are not the tape generation path.
  `TAPE_PROTOCOL_VERSION` is 24.
- Least examined by this round: whether a boundary refusal that loses its race
  for budget is observable to a consumer as anything other than a retry - the
  new regression drives `boundary_outcome` directly rather than a socket, and no
  socket test starves the held budget at the boundary.

## Machinery agents may build on and must not break, from bugs-protocol-cli round 1

- **`SubmitPhase` is a required argument on both submit validators, and there
  is no default.** `mogwai_protocol::validate_submit_order` and
  `validate_submit_group` take `SubmitPhase::PreStamp` or
  `SubmitPhase::PostStamp`. The market-price rule is the one rule that differs:
  pre-stamp a `Market` order must carry no price, post-stamp it must carry the
  one the venue put on it. Every other rule is phase-independent. The two
  production call sites that matter are `mogwai-venue`'s `boundary_error`
  (pre-stamp, both frames) and `mogwai-engine`'s `on_submit_group` (post-stamp,
  the only production caller on the far side of the stamp). Do not add a
  defaulted or inferred phase: guessing pre-stamp post-stamp rejects every
  market order the venue itself priced, and guessing post-stamp pre-stamp lets
  a consumer name the price its own market order fills at.
- **`mogwai-venue`'s `stamp_market_price` is a named function, not a closure**,
  precisely so a test can cross the phase boundary through the production code
  instead of spelling the stamp rule a second time. It is the whole of the
  transition between the two phases. A test that re-implements it pins the two
  validators against a third opinion of what sits between them.
- The parent-acyclicity walk in `validate_submit_group` starts from **every**
  member, not from the first. A cycle hanging below a member with a legal
  parentless root is invisible to a walk rooted at the frame's first order, and
  the regression covers exactly that shape.
- `SimClock::window_opening` is now `sim_ns` with no second clamp, and the
  late-reader guarantee is `sim_ns`'s own pre-anchor branch: `wall_ns <=
  wall_anchor_ns` returns `sim_epoch_ns`. Verified rather than substituted -
  all three of `sim_ns`'s exits are at or above `sim_epoch_ns`, so the old
  `.max(self.sim_epoch_ns)` could not fire on any input, while a reader
  anchored after the arm genuinely does take the pre-anchor branch and get its
  full window. If `sim_ns` ever grows an exit below its epoch, the guarantee
  moves back out into `window_opening`.

## Decisions and hazards from bugs-protocol-cli round 1

- **The round's fix pass shipped a high-severity regression that a green
  1448-test gate could not see, and the shape is worth remembering.** Finding
  1's fix - refuse a consumer-supplied price on a market order - was correct at
  the wire and was installed in a validator the engine calls again *after* the
  venue stamps that very price on. Every market-entry bracket was rejected
  whole, on the path `docs/order-lists.md` documents in as many words, and no
  test in the workspace held a market member in a group at all. A validator
  reached at two points in a message's life with different truths at each is
  the structural defect; `SubmitPhase` is the fix, and the reason it is a
  required argument rather than a comment.
- **Finding 3 did not survive contact with the code and is closed without a
  code change, on evidence rather than on the fix pass's word.**
  `mogwai-venue`'s `config.rs` has applied `validate_wire_symbol` to an
  instrument's own `symbol` since the 2026-08-20 ruling, and
  `a_configured_symbol_is_held_to_the_alphabet_the_wire_enforces` gates it
  against the wire validator's own verdict rather than a second copy of the
  alphabet, with a negative half. That is exactly the finding's scope - a
  configured symbol the venue serves and no consumer can trade or fetch - so
  restoring the finding would record a gap that is not there. What was real is
  the half the finding quoted: `validate_wire_symbol`'s own doc still said
  "config.rs does not apply it to an instrument's own symbol, which is a
  recorded asymmetry", eighteen months of a sentence describing a gate narrower
  than the gate. Corrected. The vacuous-gate family's prose sub-shape survives a
  fix to the code it describes, and nothing detects it.
- Two tests were added, and the gate count moved from 1448 to 1450 - which is
  what it should have moved by, and the round-2 lesson from the venue-serving
  arc applied cleanly.
  `mogwai-engine`'s `a_group_admits_the_market_member_the_venue_has_already_priced`
  goes through `process_with_market`, so it pins which phase the *caller* names
  rather than what the validator does when asked directly; `mogwai-venue`'s
  `a_market_entry_bracket_survives_both_sides_of_the_price_stamp` carries a
  bracket across the real `stamp_market_price` and asks both phases in turn.
  Both were bitten on each assertion separately.
- Bite-check hazard, seen twice in this round: a test that asserts a
  validator's verdict and then asserts the boundary dispatches to it cannot
  bite the second assertion by perturbing the validator - the first fires and
  masks it. Perturb the dispatch instead, and note that emptying a `boundary_error`
  arm outright fails to compile when the test module reaches the validator by
  its full path, because the crate-level import goes unused. Swapping the
  phase at the call site is the perturbation that reaches the dispatch
  assertion without touching the callee.
- **Coverage this round deliberately gave up.** `serving.rs`'s market-reading
  socket test carried two arms, and the priced one - a market order stating an
  absurd 9000000 - was the only place the venue's log and the wire were
  cross-checked against each other, since a fill away from 9000000 proved
  independently that a reading was taken. The wire now refuses that frame, so
  the arm is gone and only the log witnesses the reading. The remaining loop is
  one arm wide and says so at its head. Nobody has proposed a replacement
  witness; a future round wanting one needs an observable other than the fill
  price.
- No `TAPE_PROTOCOL_VERSION` bump is owed, verified rather than assumed.
  Nothing under `mogwai-data`, `analysis/` or the committed fingerprint changed,
  and no edit reaches a generator constant, an arrival clock, a seed derivation,
  the fill band's draw or the tape origin. Order validation is not the tape
  generation path. `TAPE_PROTOCOL_VERSION` is 24.
- Least examined by this round: `mogwai-adapter`'s `wire_submit` sets
  `price: init.price.map(..)` for every type but `TrailingStopLimit`, so a host
  that hands it an `OrderInitialized` with `order_type` Market and a price
  would now earn a boundary refusal where it previously earned a fill at that
  price. Nautilus's own `MarketOrder` cannot carry one, so this is unreachable
  through the supported constructor and was left alone rather than given a
  defensive arm - the venue's refusal is translated to `OrderRejected` and names
  the reason, which is the better answer than silently dropping the price.
  The close pass filed this as finding 10 in `notes/bugs-adapter.md`, so the
  adapter arc meets it in its own document.

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

## Rulings from the close pass over the venue-mechanics arc

- **The `http.rs` typed-refusal escape is settled, not open.** Round 2 left
  "a spent river cap still becomes one `500` at `/trades` and `/quotes`" as a
  live wire-surface question. It is not: both routes call `rivers.materialize`
  before the blocking task, and `materialize_refusal_response` already answers
  a `Resolve` refusal and a spent cap with a `400` there. By the time
  `ensure_reach` runs inside the task the river was materialized by this very
  request, so the only refusals reachable through the
  `.map_err(anyhow::Error::new)` escape are `Reach` and `KeyMismatch` - venue
  faults, whose correct status is the `500` they get. The escape now carries a
  comment saying so, which is what keeps the flattening from reading as the
  lost classification it is not.
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

## Machinery agents may build on and must not break, from bugs-venue-serving round 1

- **The reconnect reset replaces the ledger and nothing else.** `discard_account`
  drops the account's engine and the order claims attributing its frames, and
  deliberately leaves the connection registry alone; only `collect_account`, the
  TTL path, calls `registry.forget`, and only because it has already established
  that nothing is committed or reading. The old shared `forget` deleted the
  connection record the admission had committed one line earlier, so the
  connection bound no lanes, rode no boat, never reached `bound_lanes` - the fill
  sweeper's only delivery channel - and its live account read as unattended to
  the TTL collector.
- **The ledger identity boundary, and the order it depends on.** `/ws` samples
  `Run::ledger_incarnation` *before* it reads the ledger, carries that sample
  into `reserve_admission`, and the registry refuses `AdmissionRefusal::
  LedgerMoved` if the identity has moved since. `claim_account` then runs
  *before* `commit_admission`, so the ledger replacement happens while the
  reservation is outstanding and no other admission can reserve across it; the
  commit advances the incarnation inside that same exclusive window. All three
  are load-bearing together. Sample the identity inside `reserve` and the check
  reads the state after the reads it covers. Replace the ledger after the
  commit and a second socket can read the outgoing ledger, reserve the identity
  the commit already advanced to, and be admitted on checks nothing supports.
  Stated in the `crate::registry` module docs and in
  `reference/architecture.md`, and pinned by
  `a_check_taken_against_a_replaced_ledger_cannot_reserve`.
- The reordering does not disturb the previous arc's per-seat `readable` work.
  That rests on the ride being installed by the same `commit` transaction that
  makes the account attended, with the boat placed before the commit. `commit`
  is untouched and `claim_account` touches no registry state, so both halves
  still hold; only the ledger swap moved, and it moved earlier, not later.
- **A history request is resolved on every arm but one.** `finish_history_page`
  is split out of `spawn_history_page` precisely so a panicked blocking task can
  be exercised - the socket path cannot provoke one - and it answers a
  `JoinError` with a correlated, retryable `HistoryRejected` rather than only
  logging under a comment claiming it does. The single unresolved arm is a
  saturated lane, which means the peer is not reading, and it says so.

## Decisions and hazards from bugs-venue-serving round 1

- **Finding 4 stays closed, and is now closed by construction rather than by
  argument.** The fix pass and the cold reviewer both argued that a reset ledger
  installs no policy needing calendar validation, which was true and unenforced.
  `Run::minted_policy` is now the single source of the policy a fresh mint
  installs: `Run::account` mints with it, and `Run::daily_reset_minute` answers
  the resetting path with `risk::daily_reset_minute_of` over it instead of
  returning `None` unconditionally. The answer is still `None` today, because
  `AccountOpeningTerms` carries no policy - but the day it gains one the
  calendar refusal follows it rather than being silently skipped. No test can
  bite this today: with one policy value reachable, any assertion is `None ==
  None`. The construction is the gate, and it is named at both sites.
- The history-panic regression tests the helper, not the call site - the
  doctrine hazard this loop has now hit five times. `spawn_history_page` passing
  the wrong id to `finish_history_page` would still pass. The wiring is one
  line, `let panic_request_id = request_id.clone();` taken before the id moves
  into the closure, and it was audited by reading.
- The identity-boundary regression forces its interleaving by sequencing two
  registry calls by hand rather than by threading them. The window in `/ws` is a
  few instructions wide, so a threaded version would be the coin flip against
  its own defect that this loop has already paid for twice.
- Bite-check note worth keeping: adding a new assertion to an existing
  regression can mask the assertion the test was laid for. The reset regression
  was written with the identity assertion ahead of the finding-1 assertions, and
  the `registry.forget` perturbation then fired the identity one instead of "the
  reset did not erase the connection the admission committed". Ordering
  assertions so the older, more specific one fires first is the cheap fix.
- No `TAPE_PROTOCOL_VERSION` bump is owed, verified rather than trusted.
  `mogwai-data`, `analysis/` and the fingerprint are untouched; nothing in this
  round reaches a generator constant, an arrival clock, a seed derivation, the
  fill band's draw or the tape origin. `TAPE_PROTOCOL_VERSION` is 24.
- Least examined by this round: the `Ok(Ok(Ok(payload)))` arm's silent drop when
  `lanes.reserve_admission()` returns `None`. Finding 2 flagged it as the same
  unresolved-request-id outcome; it is argued (the peer is not reading) and now
  stated in `finish_history_page`'s doc, but it is not tested and nobody has
  asked whether a lane can be momentarily saturated by a peer that is reading.

## Machinery and decisions from bugs-venue-serving round 2

- Completion is a latched terminal state. `Run::complete` uses
  `watch::Sender::send_replace`, because `send` discards the value when no
  receiver exists. The regression drives the real `Run` with zero receivers
  and subscribes afterwards; its bite against `send` reaches the terminal-state
  equality, not the preceding timeout assertion.
- **The admission tail owes two properties at once, and the round's first fix
  bought one by breaking the other.** Moving the tail into axum's `on_upgrade`
  callback did make the work cancellation-proof, and it also returned the 101
  with the commit still pending - so a client's second leg could reach
  `reserve` while its own first admission was still `pending` and be answered
  `409 Busy`. That is a regression against the supported two-socket
  shared-callsign topology and against handshake linearization generally. The
  shape that holds both: `ws::admit` spawns the tail as its own task and awaits
  the `JoinHandle` before the upgrade response is built. Ownership sits with the
  task doing the work, so a dropped HTTP future detaches rather than cancels
  and the yielded `Passenger` is dropped by the runtime with its guards intact;
  and the 101 still cannot be observed before the commit. Do not move the await
  and do not remove the spawn - each alone is a defect.
- `ws_upgrade` is now a thin wrapper over `ws::admit`, which returns
  `Result<Passenger, Response>`. That split exists so the commit completes
  inside a function that returns, which is also the only seam a test can hold:
  `admit` returning IS the instant the 101 becomes available.
- `ConnectionRegistry::commit` returns `Option<Committed>`. Its missing-entry
  and stale arms are still unreachable while a live reservation exists, but
  they no longer answer a `Committed` that installed nothing - a caller cannot
  tell that apart from "installed, displacing nobody", and acting on it builds
  an `Attach` for a connection the registry never had. Both arms log at error
  and answer `None`; `commit_admission` passes it through, and the tail answers
  a `503`. Both still set `reservation.committed`, because the pending they
  found is not theirs to roll back.
- The tail can now fail in two ways before the 101 and both are answered rather
  than absorbed: a `JoinError` from a panicked tail is a `500`, and the
  registry invariant is the `503` above. Both leave nothing stranded, because
  everything the tail held is dropped as its task unwinds.
- No socket regression can force cancellation of axum's internal HTTP handler
  between the commit and response without a production scheduling hook. The
  earlier abandoned-upgrade test records the same limitation, so the ownership
  half of the fix is still gated by construction rather than by a test. The
  linearization half is gated by
  `a_second_leg_is_not_refused_by_the_first_legs_own_admission`.
- The run clock and boat clock now share `config::delivery_clock`, the one
  owner of the zero-speed substitution. This changes no clock value, and it is
  gated by `a_zero_delivery_speed_still_builds_a_wall_rate_clock`, which bites
  on both halves separately: dropping the substitution in `delivery_clock`
  fires the unpaced-axis assertion, and letting `build_run_clock` spell the
  rule itself fires the run-clock one. The hazard the gate names is that a
  speed-0 `SimClock` answers `u64::MAX` from `wall_duration`, so a forgotten
  substitution wedges every deadline on the venue and reports nothing.
- A terminal tape fault now takes precedence over a simultaneous drain timeout.
  The drain result is retained until the terminal fault latch has been read.
  Untested: reaching it wants a venue that faults and then fails to drain,
  which needs the socket-backed lifecycle rig rather than a unit test.
- **The divergence control route stays unauthenticated, and the convention is
  now written where an operator reads it** - `docs/havoc.md`, beside the
  control-post request shape - rather than only in a source comment. It names
  what an unauthenticated post can reach on somebody else's ledger and which
  other behaviours lose their premise if a non-cooperating party is ever put on
  the port.
- **Detached history synthesis is checked against the code, not the argument,
  and the refusal stands.** Both permits are moved into the `spawn_blocking`
  closure and dropped by it after the walk and the serialization; the outer
  spawned wrapper holds neither across an await, and there is no await between
  acquiring the synthesis slot and moving it in. Dropping the wrapper's handle
  does not cancel `spawn_blocking`, so the guard is owned by the task doing the
  work in the guard-scope family's sense. What remains is the bound already
  recorded: dead connections can occupy history slots until their synthesis
  finishes.
- No tape protocol bump is owed, verified rather than assumed. No file in
  `mogwai-data`, `analysis`, or the committed fingerprint changed, and the
  serving edits do not reach a generator constant, arrival clock, seed
  derivation, fill-band draw or tape origin. `TAPE_PROTOCOL_VERSION` is 24.
- **A flat test count is a finding, not a footnote.** The round-2 fix pass
  reported work across three findings and the gate's count did not move by one,
  because its only test edit rewrote an existing regression in place. Findings
  6 and 7 had shipped with no coverage at all. Two tests were added here for
  them. Read the count after any fix pass and ask what it should have moved by.
- Least examined by this round: `serve_until_drained`'s fault-precedence path,
  which is argued and unexercised; and whether an eviction that happens while
  the tail's client vanishes mid-handshake leaves an incumbent closed for a
  successor that never arrives. The second is unchanged by this round and is
  inherent - the client did disconnect - but nobody has stated it as a
  consumer-visible behaviour anywhere durable.

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

## The close pass, and how the bugs-protocol-cli arc ended

The close pass reviewed the whole two-commit arc (the SubmitPhase split and the
BoundaryCleared witness) and closed it. What it found and did:

- Every production and test call site of `validate_submit_order` and
  `validate_submit_group` was enumerated and checked against the stamp it
  stands on. The two production sites are exactly as recorded -
  `boundary_error` PreStamp on both frames, `on_submit_group` PostStamp - and
  no test asserts the wrong phase's rule: the engine tests that reuse the
  priced `order()` helper strip the price before asking PreStamp, and the
  linkage tests that hand a priced market child to PreStamp are safe because
  `validate_order_link` runs before the type match, so the child refusal fires
  first for the reason the assertion names.
- The parent-cycle walk was re-derived: bounded at `orders.len()` steps per
  member, it cannot flag an acyclic chain (depth is at most `len - 1`) and
  cannot miss a cycle (after `len` steps a walker in or above one still holds a
  parent), and duplicate ids are refused before `find` could pick between them.
- The finding-4 deletion was re-proven: all three of `sim_ns`'s exits are at or
  above `sim_epoch_ns`, so the deleted `.max` genuinely could not fire.
- The durable prose the arc touched was checked claim by claim against the
  code, which was this arc's characteristic hazard: the two-admissions section
  of `reference/architecture.md` (witness minting, the unclamped counts, the
  64 KiB bound, the deferred verdict), `docs/cli.md`'s launcher properties
  (owner-loop reap recording, the `take` under the `BufReader`, the single
  elision marker, the launcher-pid comparison against `getppid()`), and
  `docs/order-lists.md`'s two-validations paragraph all state what the code
  does. One prose defect found and fixed, in the arc's own family: the lattice
  test's doc claimed the market-price rule "cannot be expressed here" while
  the lattice does state that cell - as its pre-stamp value, which is the side
  the test asks. The comment now says exactly that.
- The wire_submit residual (a host-built `OrderInitialized` with type Market
  and a price now earns a named refusal instead of a fill at that price) is
  filed as finding 10 in `notes/bugs-adapter.md`, where the next arc will meet
  it, rather than left as a footnote here.
- The remaining residuals were accepted as recorded: the unclamped
  `worst_case_output_bytes` count (the ordering, enforced by the witness, is
  what keeps it small), the narrowed one-arm market-reading socket test (a
  replacement witness needs an observable other than the fill price, and none
  exists yet), and the untestable no-op clamp deletion.
- No `TAPE_PROTOCOL_VERSION` bump is owed, verified rather than trusted:
  nothing under `mogwai-data`, `analysis/` or the fingerprint moved in either
  commit, and order validation, admission sizing and the launcher are not the
  tape generation path.

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

## The close pass, and how the bugs-venue-mechanics arc ended

The close pass reviewed the whole two-commit arc and closed it. What it found
and did:

- The whole diff was re-read at its sites. The extremes epoch handoff was
  checked under every interleaving of the writer's three epoch loads against
  `take`'s bump-then-lock and is sound; the per-seat `readable` premise was
  verified in the registry - a connection's ride is installed in the same
  `commit` transaction that makes the account attended, and the boat is placed
  before the commit, so an attached account can never be observed seated on a
  boat the `boats()` read misses. The unpaced predicates agree because
  `TapeSpawn.speed` is taken from `key.speed()` after quantization.
- The pass-snapshot recompute was bite-checked by deleting the replacement
  block as a text edit: the funding regression fails on its named assertion,
  showing exactly the missing funding debit, and was restored the same way.
- Two durable-prose corrections in `reference/architecture.md`: the futures
  paragraph claimed "exactly one account snapshot per pass" where the
  invariant is at most one from the engine phase (the `DropNextAccountUpdate`
  suppression and `enforce_policy`'s own snapshots both contradict "exactly
  one per pass"), and the extremes paragraph still claimed "one relaxed load"
  and "publishes only when an extreme actually moves", both superseded by the
  round-1 race fix it sits directly above.
- The `TAPE_PROTOCOL_VERSION` non-bump was verified rather than trusted: the
  arc touches no `mogwai-data` code, no fill-band draw, and the tape edit only
  gates the extremes accumulator on the publication thread; the re-blessed
  golden is read by nothing but its own comparison. No bump owed, on the same
  grounds as both rounds recorded.
- The `http.rs` escape question round 2 left open is settled above. The other
  deliberate residuals - the helper-level regressions without a two-account
  sweeper fixture, the untested `Ticket::drop` guard, the sweep-side
  `next_due` staleness window, the `serve_page`-level classification test not
  laid - were re-read and stand as recorded.
- The retained-print ordering in `last_trade_at_or_before_with_budget` was
  questioned and accepted: returning a found print when the budget expires
  could in principle return a non-final print, but `SWEEP_DRAIN_BUDGET` is
  13.1e9 ticks - a runaway guard, not a tightness bound - so the case is
  unreachable in practice and either answer there is already a disaster.

## The close pass, and how the bugs-venue-serving arc ended

The close pass reviewed the whole two-commit arc and closed it. What it found
and did:

- The admission transaction was re-read at its sites against the three
  interlocking requirements in the `crate::registry` module docs, and it holds
  as documented: the incarnation is sampled in `/ws` before the first ledger
  read, `claim_account` and `commit_admission` run with no await between them
  inside the spawned tail, the tail is spawned and awaited before the 101, and
  the registry docs describe what the code does. The round-2 prose in
  `reference/architecture.md` and `docs/havoc.md` matches the code.
- **One real defect found and fixed: the TTL collection raced the very
  reconnect it exists to give up on.** `collect_expired_accounts` read
  `frozen_for` under the accounts lock and then called an unconditional
  `registry.forget` - so an admission that reserved, or fully committed, in
  the window between the sweep's read and its removal had its registry entry
  deleted. That was the finding-1 stranding through the TTL door, and it also
  made `commit`'s "unreachable while a live reservation exists" arms reachable
  by deleting an entry whose reservation was live. `forget` is now
  `ConnectionRegistry::collect`, which re-derives "unattended, and nothing
  pending" under the registry lock and answers whether the removal happened;
  `Run::collect_account` removes the ledger under the accounts lock taken
  before that registry call, in the same accounts-then-registry order the
  expiry filter already nests, and a refused collection waits out its next
  expiry.
- The ABA half of the same race was closed by minting ledger incarnations from
  a registry-wide counter instead of restarting each entry at 1: a collected
  and recreated entry can no longer wear the identity an in-flight admission
  sampled off its predecessor, so a stale sample is refused `LedgerMoved`
  however the entry was reborn. `incarnation` now creates the entry it samples,
  because a counter-minted identity cannot be predicted without creating it.
  Incarnation values are identities, not sequences: nothing may assert
  `observed + 1`, only that the identity moved. Both halves are pinned by
  `collection_refuses_an_account_an_admission_has_reached_first` and
  `a_sample_taken_before_collection_cannot_reserve`, both bitten.
- One prose defect: `discard_account`'s comment said reconnect reset is
  reached "after admission has committed the successor's ride and handoff",
  while the same round moved the reset before the commit. Corrected to what
  the code does; the TTL paragraph of `reference/architecture.md` now states
  the collection guard.
- The round-1 and round-2 residuals were re-read and stand as recorded: the
  saturated-lane silent drop, the helper-level history-panic regression, the
  untested minted-policy construction, the `result_large_err` expect, the
  unauthenticated control plane as a stated operator convention, and the
  detached history synthesis refusal.
- No `TAPE_PROTOCOL_VERSION` bump is owed by the arc or by this close pass,
  verified rather than trusted: nothing under `mogwai-data`, `analysis/` or
  the fingerprint moved, and the registry is not on the tape generation path.
