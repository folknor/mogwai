# Bug hunt: mogwai-server

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-server`: sockets, clock and replay pacing, the HTTP surface,
websocket-only order entry, subscription and account state, eviction, `serve`'s
foreground lifecycle and readiness line, config loading, and its tests.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

EXHAUSTED AS OF ROUND 3, 2026-08-19. Every finding is fixed or recorded as
refused, lead 9 is closed, and lead 10 is the one open item - not reproduced,
with what was measured and what it rules out written into that section. A close
pass follows.

THE COLD REVIEW OVER ROUND 3 FOUND THE ROUND'S OWN FIX REINTRODUCING ITS OWN
FINDING, closed in the fix-and-commit pass: scoping the silent cancel's SEARCH
by account left the miss path's "diagnose why it missed" fallback running
`cancel_open_order_silently` - which is not a query - unconditionally against
the DEFAULT account, so a request naming an account that did not hold the id
silently cancelled the default account's order and answered `404`. See finding
8 below and the carry-forward's fix-and-commit section.

The hunter read `ws`, `run`, `http`, `serve`, `sweeper`, `admission`,
`boatyard`, `extremes` plus the `SimClock` contract they ride on. Nothing was
built or run.

## 1. Every connection receives every other account's `AccountState` (high confidence, serious) - FIXED 2026-08-18

`sweeper.rs::deliver` filters by `run::addressed_order`, which returns `None`
for anything that is not order-scoped - including `ServerMessage::AccountState`.
An owner of `None` passes `is_none_or(...)` for EVERY bound lane, so the frame
goes to all of them.

The sweep loop calls `deliver` INSIDE the per-passenger loop
(`for (index, passenger) in seated.iter()...`), and `apply_engine_pass_on_clock`
guarantees exactly one `AccountState` per passenger per pass. So on a venue with
N attached accounts, each pass fans out N account snapshots - balances,
positions, margin, unrealized PnL - to all N sockets.

The doc comment on `deliver` states the old rule verbatim: "everything else -
the account snapshot, anything the venue says about itself - still reaches all
of them." That was true when the run had one ledger. It stopped being true when
the engine moved onto `Passenger`, and the comment was not revisited. This is
the same defect class the attribution work closed for `OrderFilled`: one channel
was fixed, the account channel was left open.

It is not merely an information leak. `mogwai-adapter` (and any nautilus host)
applies `AccountState` to its account row, so a second account on the same venue
actively corrupts the first one's reported equity. The three-way orchestrator
topology the crate is explicitly built for (fifty subagents on one exchange) is
exactly where it bites.

Fix direction: `deliver` should take the producing passenger's account id and
scope non-order-scoped, ledger-owned frames to it. Genuinely venue-wide frames
(a venue fault) need to be a distinguishable class, not "everything
`addressed_order` doesn't recognise". The hunter would make that an explicit
enum on the frame rather than a fallthrough, because the fallthrough is what
silently mis-classified `AccountState` in the first place.

FIXED, by the frame's own field rather than by the producing passenger's id.
`run::addressed_account` reads `AccountState.account_id`, and `deliver` consults
it before falling back to order ownership. That is deliberately not the "take the
producing passenger's account id" shape the hunter proposed: delivery stays a
pure function of the batch, so a frame is attributed by what it SAYS rather than
by which passenger the sweep happened to be iterating, and a later refactor of
the loop cannot silently re-point it. The hunter's deeper point stands and is not
closed here - "unattributed means everyone" is still a fallthrough rather than a
declared class, so the next ledger-owned frame joins the broadcast set exactly
the way this one did. Findings 6 and the second half of 12 keep that open.
`a_swept_account_snapshot_reaches_only_that_account` pins the fix and was
bite-checked against the reported symptom.

## 5. FIXED 2026-08-19: arms did not reach accounts that connect later

The report named `FeeSurcharge` and the engine divergences. The hole was wider
in two directions it did not reach. The four TRANSPORT arms - `GoDark`,
`StallData`, `DelayAcks`, `CommandLatency` - shared the same helper and had the
identical hole, which `docs/havoc.md` contradicts twice. And a NAMED account was
worse than an unqualified one: naming an account filtered the set that existed,
so arming a blackout on a subagent that had not connected yet matched nothing
and still answered `202`.

FIXED by `Run::arm`, which records the arm on the run and applies it to the
ledgers that exist IN ONE CALL, and by both mint sites replaying that record.
`ArmRecord` is the replayable form; `VenueArms` holds the venue-wide record plus
the per-account records for names that have not connected yet, which are
consumed by that account's first mint. `ClearDivergences` lifts the record along
with the seated ledgers.

TWO THINGS THE FIRST PASS GOT WRONG AND THE COLD REVIEW CAUGHT:

- `Run::open_account` - the SECOND mint site, reached from `POST /account` and
  from the sweeper's tests - did not replay anything, so finding 5 survived
  verbatim for any client that opens its own account. That is also instance 36
  of this arc's signature defect: the new record's doc asserted an invariant its
  own second call site broke.
- The first pass MINTED a ledger for a named arm. That closed the silent no-op
  by making the client's own `POST /account` answer `409 already open`, on a
  ledger carrying default balances and no policy. Recording without minting is
  the shape that keeps the two paths one mechanism and costs the client nothing.

A malformed `account` on the request is now a `400` rather than a `202` that
arms nothing. Pinned by five `run.rs` tests, each bite-checked one perturbation
at a time; what does and does not bite is stated on the tests themselves.

## 6. REFUSED 2026-08-19: `deliver`'s reservation is over-sized, not under-sized

Recorded rather than deleted so a later round does not re-derive it.

The finding's live claim was that `Reservation::split` would `debug_assert`-fail
or log "produced output exceeded its admission reservation", because the batch's
`shape` belongs to the producing passenger while the frames go to every lane.
Finding 1's fix dissolves it, and the reason is stronger than "mostly":

- Every term `swept_batch_max_bytes` derives from `shape` sits inside the ONE
  `account_state_max_bytes` summand. The per-order terms are
  `ORDER_EVENT_MAX_BYTES` and `LINKAGE_MAX_BYTES`, both shape-free constants.
- After finding 1, `run::addressed_account` attributes `AccountState` by the
  frame's own `account_id`, so the only lane that can receive it is the
  producing passenger's - whose shape this IS. Another account's lane receives
  at most the unattributed order-shaped frames, sized by the shape-free terms,
  and does not receive the account snapshot the shape-dependent term paid for.

So a foreign lane is reserved MORE than it can spend. The dominance argument
holds; there is no reachable assertion. What is left is an over-reservation
against a shape that is not the lane's, which is conservative in the only
direction that matters and is not worth a mechanism.

THE FINDING'S STRUCTURAL POINT IS NOT THIS, and it stays open where finding 1
already put it: "unattributed means everyone" is a FALLTHROUGH rather than a
declared frame class, so the next ledger-owned frame type joins the broadcast
set exactly the way `AccountState` did. That is filed in `notes/todo.md`.

## 7. REFUSED 2026-08-19: the venue never prints two trades at one instant

Recorded rather than deleted, because the finding's MECHANISM is real and a
later round reading `bounded_trades` will see the same mid-instant break.

The mechanism was filed by broadarrow on 2026-08-18 and ruled on the same day,
in commit b795d1e. It was measured before being concluded: 1.1 M BTCUSDT trades
over six simulated hours, zero ties, smallest gap 27 ns. Children are stamped
`parent + emitted * INTRA_EVENT_STEP_NS` at a 1 us stride and the arrival kernel
floors a parent's own advance at the same stride, so one river's trade stream is
strictly increasing until the u64 nanosecond epoch runs out. The report's
premise - "synthetic tapes produce many prints per nanosecond bucket in a burst"
- is the one thing that measurement overturned.

`Rivers::history_source` builds a `MergeSource` over exactly ONE child, so no
tie can enter across children either. A quote ties with its first child only,
and `/trades` and `/quotes` are separate pages, so that tie cannot cut either.

The property is load-bearing and now PINNED, by `mogwai-data`'s
`a_river_never_prints_two_trades_at_one_instant`, deliberately strict rather
than non-decreasing. This fix pass added the citation to `bounded_trades`, so
the serving side names the generator property its correctness rests on rather
than leaving it unstated - which is the gap the ruling identified. A change that
makes ties possible has to break that test first.

## 8. Smaller things

FIVE OF THE SEVEN BULLETS WERE DEFECTS AND ARE FIXED, 2026-08-19, and are
deleted rather than annotated. What each was and how it was closed is in
`notes/bug-loop-carry-forward.md` under the round-3 section. What is left below
is the one bullet refused and the two that are owner questions rather than
defects.

THE CROSS-ACCOUNT SILENT CANCEL TOOK TWO PASSES. Scoping
`Run::passenger_holding` closed the SEARCH; the miss path's diagnosis still ran
the mutating cancel against the default account, which reopened the same defect
on the miss path only. It now diagnoses off the TARGETED ledger through
`Engine::silent_cancel_refusal`, a query. The regression test that catches this
half has THE DEFAULT ACCOUNT as the holder, which is the case the first test
structurally could not express.

- `refuse_all`'s `frames.max(1)` REFUSED as a defect, kept as a shape note.
  The claim was that a non-submit command reaching that path would reserve one
  frame and produce zero events. Every `refuse_all` call site is guarded by a
  `submitted_orders(...).first()`, which the report itself says, so the branch
  is unreachable and the `max(1)` costs one unspent reservation that the
  `Reservation` returns on drop. Turning the count into a `NonZeroUsize` is a
  signature change across the admission boundary to make an unreachable state
  unrepresentable, which is worth doing and is not a bug fix; filed in
  `notes/todo.md` rather than done inside a fix pass.

- `reject_while_closed` JUDGES MARKETABILITY AGAINST THE STATED PRICE while the
  engine judges it against the BAND-DRAWN trigger, so the two can disagree by up
  to the fill band in either direction: an order the server admits as
  non-marketable can be marketable to the engine (and fills off the stale print
  the guard exists to refuse), and one the server refuses can be one the engine
  would have rested. The engine's `draw_trigger` needs the order's `band_ticks`
  and the run's `fill_seed`, neither of which the HTTP boundary holds, so
  closing this properly means asking the engine the question rather than
  re-deriving it - `Engine::worst_case_leaves` is the precedent for that shape.
  Filed 2026-08-19 by the `bugs-engine` round-3 fix pass, which found the gap
  while widening the guard to cover `MarketToLimit`; the widening did not create
  the gap, it doubled the number of types standing on it.

- A `MarketToLimit` ORDER-LIST CHILD IS A LIMIT CHILD, which is not what
  `docs/oms-types.md` describes the type as doing. CORRECTED 2026-08-19 BY THE
  CLOSE PASS; the filing as first written claimed the opposite and was wrong.
  What is actually true: `Engine::release_child` rests every released
  non-conditional child at `submit.price` as `Resting::Limit`, so a
  `MarketToLimit` child NEVER takes the market - the release runs inside
  `apply_linkage_after_fill`, which holds no `MarketReading` to price against.
  The standalone submit path took the market as of 2026-08-19; the release path
  was not enumerated with it, which is the one site the round-3
  `OrderType::` audit could not have found by grep, since `release_child` names
  no order type at all.
  NOT A DEFECT AS IT STANDS, and the reason is the rule the original filing
  reached for from the wrong end: `validate_order_link` refuses a `Market`
  child precisely because "a released child rests", so resting a released
  market-to-limit is the CONSISTENT behaviour and executing it on release would
  be the thing that contradicts the linkage contract. The carve-out is now
  stated in `docs/oms-types.md`.
  WHAT IS OWED, AND TO WHOM: an owner call on whether a release should carry a
  market reading at all - which would make a released `MarketToLimit` execute on
  arrival and would reopen whether a `Market` child can be admitted too. Until
  that is asked, the venue serves the type one way standalone and another way as
  a child, and says so in the durable doc rather than in a comment.

## 9. CLOSED 2026-08-19: the venue outran its own completion announcement

REPRODUCED AND FIXED. It is venue-side, it was never about the test, and the
mechanism is one sentence: `axum::serve`'s graceful shutdown tracks HYPER
CONNECTIONS, and an upgraded connection's hyper future resolves AT THE 101, not
when the websocket ends. Read it in `axum-0.8.9/src/serve/mod.rs` - the
per-connection task drops its `close_rx` the moment
`serve_connection_with_upgrades` returns. So the deadline task published
completion on a watch channel, stopped the accept loop in the very next
statement, the serve future resolved with every session still mid-frame, `main`
returned and the runtime dropped them - taking the `RunComplete` frame and the
WS 1000 close with it. The peer saw a reset.

THAT MATCHES THE FILED SYMPTOM EXACTLY, including the part that made it read as
a venue lead: the socket WAS a live session, the venue HAD served it content,
and the drain ended - on `Ending::Failed`, a reset without a closing handshake,
which `watch_a_bounded_run` accepts because it discards only `Ending::Deadline`.
The `serve.rs` comment claimed the ordering it did not have - "announces
completion on every open socket and only then stops the accept loop" - which is
this arc's signature defect once more.

HOW IT WAS REPRODUCED, since `brokkr test` never fires it: the runner always
passes `--test-threads=1`, and the failure needs a crowded host. Running the
built `completion` binary directly at `--test-threads=32` under 64 busy
processes (`scripts/flake_hunt.py`) produced 5 failures in 78 rounds - the filed
test, `run_complete_reaches_every_open_socket` and
`a_short_accelerated_run_is_not_over_before_it_is_ready`, all three the same
shape. The captured message names the mechanism: "after 15 content frames and a
Failed ending".

FIXED by `Run::session_guard` / `Run::sessions_drained`: every `/ws` session
holds a watch receiver from BEFORE THE 101 until after its writer has flushed,
and a PLANNED completion waits for that set to empty before the process leaves,
inside the existing `SHUTDOWN_GRACE`. A SIGNAL deliberately does not wait -
nothing tells a session to end on that path, so waiting would idle out the grace
- and `docs/cli.md` now states both halves. 80 rounds at the same load and
thread count, zero failures.

THE GUARD MOVED ONCE MORE IN THE FIX-AND-COMMIT PASS, and the reason is the same
mechanism one notch smaller. Taken at the top of `handle_socket` it sat inside
the task `on_upgrade` spawns, which is polled only AFTER hyper's connection
future has already resolved at the 101 - so a completion landing in that window
saw the session counted at zero and the runtime dropped the task before its
first poll. It is now a `SocketSession::alive` field, taken in `ws_upgrade`
before the 101 and dropped with the session after the writer flush. 80 rounds
after the move, zero failures.

WHAT THE ORIGINAL LEAD GOT RIGHT AND WHAT IT GOT WRONG, kept because the
reasoning is reusable. Right: it insisted the failure sat PAST every premise
`watch_a_bounded_run` checks, so the socket really was a live session the venue
really had served, and it named the writer's ordering against the deadline task
as the place to look. Wrong: it read a discarded run as impossible, when
`Ending::Failed` is accepted by that helper and is precisely what a reset
produces - the helper discards only `Ending::Deadline`. The general lesson is
that "the helper would have discarded it" is only as strong as the ENDINGS the
helper enumerates, and this one enumerated four and rejected one.

## 10. LEAD, STILL OPEN: a SECOND intermittent lifecycle test, on shutdown rather than completion

Filed 2026-08-19 from the `bugs-data` round-2 close pass. DISTINCT from lead 9 -
different test, different file, different failure mode - and recorded separately
so neither absorbs the other.

`crates/mogwai-cli/tests/lifecycle.rs`'s
`sigterm_stops_the_venue_within_the_shutdown_grace` failed the FIRST of that
pass's two full unscoped `brokkr check --gate` runs and passed the second, on
the IDENTICAL tree. The failure was at `crates/mogwai-cli/tests/common/mod.rs`
in the wait helper: "venue did not exit within 10s (or the test's remaining
budget)". The tree's changes were confined to `mogwai-data`, `mogwai-cli` and
markdown - nothing in the serving or shutdown path.

So the venue took longer than the shutdown grace to exit after a SIGTERM, once.
The candidate readings, none of them checked: a shutdown path that waits on a
task the signal does not interrupt; the grace being measured against a clock
that starts before the process is really up; or simple host contention under a
parallel gate sweep, which is the boring answer and the one to rule out first by
running the test alone under load.

IT COSTS THE SAME AS LEAD 9 WHEN IT FIRES. Aborting on this test kills the
instrumented sweep, and the gate then reports all of `mogwai-data`'s tests as
orphaned - the brokkr-coverage-bug lookalike `AGENTS.md` warns about. The tell
is that the orphan count equals the missing sweep's pass count. There are now
TWO tests known to produce that wall; check for a crashed test before suspecting
the tool.

NOT REPRODUCED BY ROUND 3, 2026-08-19, and here is exactly what was tried and
what it rules out. The built `lifecycle` binary was run directly at
`--test-threads=16` for 20 rounds under 64 busy processes on a 32-core host -
the same harness that DID reproduce lead 9 five times in 78 rounds, so the load
is not the reason nothing fired. Zero failures. That does not close it, and
"not seen since" is not a closure.

WHAT ROUND 3 DID CHANGE IS THE AMBIGUITY, which was the cheaper prize and is the
thing that was costing a cycle. The message the filing quotes - "venue did not
exit within 10s (or the test's remaining budget)" - is TWO OPPOSITE VERDICTS in
one sentence. `Venue::wait_for_exit` clamps its bound to the test's remaining
wall budget, so on a crowded 32-way sweep the wait may have been far shorter
than the 10 s the caller asked for, and the failure would then be a statement
about what the phases before it spent rather than about the venue's shutdown.
The helper now reports HOW LONG it actually waited and WHICH bound produced
that, so the next occurrence arrives as one verdict. Re-read the lead against
that output before theorising about the shutdown path; the boring reading is
still the one to rule out first, and it is now readable rather than inferable.

STILL UNRULED-OUT if the clamp turns out not to be it: a shutdown path that
waits on something the signal does not interrupt. The two concrete candidates a
later round should look at are `spawn_blocking` work in flight at signal time -
the sweeper's tape walk and the boatyard's `worker.join()` both run there, and a
tokio runtime being dropped WAITS for blocking tasks that have started - and the
boat worker's own responsiveness to its cancel flag. Note that round 3's
completion-path session wait does NOT touch this path: a signal deliberately
does not wait for sessions, so nothing here got slower.

## What the hunter would actually rewrite

Findings 1, 2 and 3 - all three now fixed - were the same underlying problem:
THE ACCOUNT AND CONNECTION LIFECYCLE IS ENCODED ACROSS SEVERAL MUTABLE
STRUCTURES - `Run::lanes`, `Passenger::frozen_since`, `Passenger::seated_on`,
and now `Passenger::admitted` - with the consistency rules living in prose. Each
has a path that skips one of the others. The hunter's proposal was to collapse
them: one registry keyed by account, holding the live connections; `is_frozen`
and `is_seated_on` become derived queries; eviction becomes a transition on that
registry taken at the last possible moment before the 101.

THE SECOND HALF OF THAT LANDED AND THE FIRST DID NOT, which is worth stating so
a later round does not read the fixes as the rewrite. Eviction IS now the last
step before the 101, and the freeze IS now decided by one predicate over the
lane table and the admission count rather than by whichever call site happened
to notice - but the state is still spread over four structures rather than
derived from one registry, and `admitted` is a FOURTH of them. It closes the
hole by making the missing reader visible instead of by removing the
possibility, so the next lifecycle path that forgets one of the four is not
detected by anything.

THE CLOSE PASS ADDED ONE THING TO THAT READING, 2026-08-19. `Passenger::admitted`
is reached ONLY through an `Admission` guard now - `Run::admit` returns one and
`Run::depart` is private - because the first shape of the fix raised the count
and constructed the guard as two statements, which leaks the count permanently
on a panic or on axum cancelling the handler. So the fourth structure at least
cannot be left inconsistent by a path that forgets to lower it; it can still be
left inconsistent by a path that never raises it, which is the item filed in
`notes/todo.md`. And the freeze on the ordinary teardown is now owed entirely by
the admission's departure: `handle_socket` releases its lane while still holding
its admission, so the lane release finds the account counted-in and declines.

Second, `deliver`'s "unattributed means everyone" default is the wrong direction
now that the ledger is per-passenger. It should be an exhaustive match on frame
class with no fallthrough, so the next per-ledger frame type cannot silently
join the broadcast set the way `AccountState` did.
