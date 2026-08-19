# Bug hunt: mogwai-server

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-server`: sockets, clock and replay pacing, the HTTP surface,
websocket-only order entry, subscription and account state, eviction, `serve`'s
foreground lifecycle and readiness line, config loading, and its tests.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

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

## 2. `/ws` evicts the incumbent before it has decided whether to admit the newcomer (high confidence)

`ws_upgrade` calls `state.run.seat(&account_id, true, session)` - which closes
every socket of the incumbent client and, under `reset_account_on_reconnect`,
discards the ledger - and only THEN runs:

- `ensure_instrument` (400 on an unresolvable shape),
- the settlement-currency funding check (400),
- `speed.is_finite() && speed >= 0.0` (400),
- `boatyard.board` (400 on placement failure),
- `passenger.try_sit` (400 on a second cadence).

Any of those returns a 400 with the incumbent already destroyed.
`GET /ws?account=X&speed=NaN` is a one-request, unauthenticated way to
disconnect a live client and, with the reset knob on, wipe its position book,
while itself never connecting. Eviction should be the LAST thing that happens
before the 101, after every refusal has been decided.

## 3. That same path leaves an account permanently un-frozen and un-collectable (high confidence)

Follow-on to finding 2, but independent enough to state separately.
`Passenger::attach()` is called from exactly one place (`Run::resume`, inside
`handle_socket`) and `freeze()` from exactly one (`Run::release_lanes`).
`evict_account` removes the incumbent's `BoundLane` EAGERLY, so when the evicted
socket finally tears down, `release_lanes(id)` finds no matching lane, resolves
`account` to `None`, and returns before `freeze()`.

Normally the newcomer binds and its own later release freezes the account. But
whenever the newcomer never reaches `bind_lanes` - any of the five refusals in
finding 2, or a client that abandons the upgrade after the 101 handshake
response - the account is left with `frozen_since = None` and zero connections.
Consequences:

- `collect_expired_accounts` filters on `frozen_for().is_some()`, so the account
  is NEVER TTL-collected. Unbounded ledger accumulation, driven by a client that
  only ever sends refused upgrades.
- The sweeper's `seated` filter (`!passenger.is_frozen()`) keeps sweeping it.
  Because it holds no seat, `cancel_unreadable_orders(&readable, venue_now)`
  will cancel its resting orders on the first pass where the river's boat is
  gone - which is precisely the "a frozen account's book survives for the socket
  that returns" contract, violated.

The structural fix is to make attachment a count derived from the lane table
rather than a separately-mutated flag: `is_frozen()` should be "no lane is bound
to this account id", computed, not stored. The current two-variable encoding
(lane list plus `frozen_since`) has exactly one consistency rule and one path
that skips it.

## 4. A failed mark read destroys the interval's price extremes (high confidence, frontier family)

In `spawn_fill_sweeper`:

```
let span = boat.extremes.take();          // destructive
let Some((marks, settlement_marks)) = reads else { continue; };
```

`PriceExtremes::take` bumps the epoch and clears the published span - it is a
CONSUMING read. `frontier_after` correctly refuses to advance `last_swept_ns`
when `reads` is `None`, and the comment above the `continue` explains that the
settlement instants must be retried. But `take()` already ran, so the high and
low the tape reached over that interval are gone permanently. The next pass's
span starts from its own first print.

`extremes.rs`'s entire reason for existing is that "a spike that opened and
closed between two passes never happened as far as the account was concerned,
and the account kept room it should have lost." On a failed-read pass, that is
exactly what happens again - for the interval that is then REPLAYED for
settlement, so the account is marked over a span whose extremes were silently
dropped. This is the guard-scope rule stated in `AGENTS.md` applied to a
destructive read rather than a watermark: the take must be sequenced after the
pass has committed, or the span must be pushed back on failure.

## 5. `FeeSurcharge` and the engine-armed divergences do not reach accounts that connect later (high confidence, contract vs code)

`arm_divergence`'s `FeeSurcharge` arm carries this comment:

> "a passenger connecting later gets it too - which is why this is stored on the
> template rather than only applied to the seated set."

It is not stored on the template. The code is
`for passenger in run.passengers() { ... arm_fee_surcharge(...) }` - a snapshot
of the accounts that exist NOW. `Run::passenger()` builds a fresh engine from
`LedgerTemplate`, which carries only balances, fill seed, OMS type and band
ticks. The same applies to the catch-all `engine_div` arm ("Armed on EVERY
ledger: the control plane names no account, so an engine divergence is a
statement about the venue"), and to the eviction-report reasoning that follows
it ("every ledger holds the same arms and hits the cap together") - which is
false the moment one ledger was created after an arm.

An operator arming a `PartialFillNext` and then starting a subagent gets a run
that believes it is perturbed and is not. Given the misdiagnosis this same code
path already cost (the eviction-reporting comment describes it), this is worth
closing properly: the arms belong on the run, applied at passenger construction
AND to the live set.

## 6. `deliver`'s worst-case reservation is computed from the producing passenger's book shape (medium confidence)

`deliver` receives one `shape` - the `book_shape()` of the passenger whose pass
produced the batch - and uses it to `reserve_swept` on EVERY bound lane,
including lanes belonging to other accounts. The reservation is supposed to
dominate the frames actually sent. Since finding 1 sends those other accounts
frames at all, and the shape is not theirs, the dominance argument does not
hold; `Reservation::split` will `debug_assert!`-fail or hit the release-mode
`tracing::error!("produced output exceeded its admission reservation")`. Fixing
finding 1 mostly dissolves this, but the coupling (a run-wide fan-out sized by
one ledger's shape) is worth removing deliberately rather than incidentally.

## 7. `/trades` and `/quotes` truncate at `limit` without regard to instant boundaries (medium confidence, structural)

`bounded_trades` and `bounded_quotes` break the moment `out.len() >= limit`,
mid-instant. The venue exposes no cursor, so any consumer paging this must
resume from a timestamp - and `AGENTS.md` states the rule the adapter's
pagination is already held to: "a timestamp-only cursor may advance onto an
instant only once every row at that instant has been seen." The server makes
that rule unsatisfiable: resuming at `last_ts` duplicates, resuming at
`last_ts + 1` drops the tail of that instant. Synthetic tapes produce many
prints per nanosecond bucket in a burst, so this is reachable, and a dropped
tail is invisible in the result.

Either truncate the page at the last COMPLETE instant (and say so), or ship a
real opaque cursor. The former is a few lines and is what the contract already
implies.

## 8. Smaller things

- `GET /account?account=<anything>` creates a passenger. `run.passenger(&account_id)`
  is create-on-first-sight, so an unauthenticated read endpoint mints ledgers.
  They are born frozen and TTL-collectable, so it is bounded ONLY IF
  `account_ttl_ms > 0`, and the default is "keep accounts forever"
  (`serve_async` skips the reaper entirely at 0). A read should not allocate;
  resolve-or-report-opening-balances without inserting.
- `Run::passenger_holding` first-match-wins. Already flagged in its own doc
  comment. `CancelOpenOrderSilently` names a client-chosen id with no account,
  so on a multi-account venue it silently cancels the wrong trader's order. That
  is a scenario control writing into an unrelated ledger - worth an account
  parameter rather than a comment.
- The run deadline over-runs by the boot interval. `serve_async` anchors `sim`
  at `build_run_clock(&cfg, now_ns())` right after warmup, then spawns the
  deadline task AFTER boat placement, listener bind and readiness write,
  sleeping the full `deadline_ns - started_ns`. The declared duration is
  therefore measured from an instant later than the sim epoch. Should sleep to
  `sim.wall_ns(deadline_ns)`, not for a span.
- An evicted socket that ignores its close frame holds its seat. `evict_account`
  sends `Outbound::Close`; `run_writer` writes it and breaks; but
  `handle_socket`'s read loop only exits on the peer's own close or EOF. Until
  then the `SocketSession` lives and `try_sit` still holds the seat, so the
  newcomer's DIFFERENT-SPEED reconnect is refused with "already seated". The
  eviction should also wake the read loop.
- Heartbeat period floors at 1 ns. `wall_duration` clamps to 1 ns, so at a high
  `speed` the heartbeat task becomes a timer-granularity loop (roughly 1 kHz)
  pushing uncharged frames into a 256-slot channel. `MIN_SWEEP_WALL` exists for
  exactly this reason on the sweep side; the heartbeat has no equivalent floor.
- `refuse_all` reserves `submitted_orders(cmd).len()` and
  `try_reserve_boundary_frames` does `frames.max(1)`. For a non-submit command
  that path would reserve one frame and produce zero events - harmless today
  because every `refuse_all` call site is guarded by a
  `submitted_orders(...).first()`, but the `max(1)` is papering over a shape
  that should be `NonZeroUsize`.

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

## 9. LEAD, not a finding: a completion announcement that reached one socket and not another

Filed 2026-08-19 from the `bugs-tests-lab-cli` close pass, and repeated here
because that document is CLOSED and its carry-forward section will be trimmed.
Nothing else is watching this.

`crates/mogwai-cli/tests/completion.rs`'s
`run_complete_is_stamped_on_the_receiving_sockets_clock` failed ONE of that
pass's two full `brokkr check --gate` runs and passed the other, on
`.expect("second boat receives completion")`.

WHY THAT IS A VENUE LEAD RATHER THAN A FLAKY TEST. The whole family of
attach-race failures in that file was closed by `watch_a_bounded_run`, and the
`expect` sits PAST every premise that helper checks: it launches, attaches a
socket per boat, drains each, and DISCARDS the whole run unless every socket
saw at least one `Message::Text` frame and its drain ended cleanly rather than
on the deadline. So reaching the `expect` at all establishes that the second
socket was a live session, that the venue served content on it, and that the
drain ran to a clean end - and the announcement still was not there. The
remaining explanations are venue-side: a bounded run whose completion frame is
not written to every attached socket before teardown closes them. Suspect the
per-socket writer's ordering against the deadline task rather than the test.

REPRODUCE BEFORE BELIEVING IT: one failure in two full gate runs on this host,
and it has not been seen since. A round that cannot reproduce it should say so
rather than close it.

IT REPRODUCED, 2026-08-19, on the `bugs-data` round-1 gate - same test, same
`.expect("second boat receives completion")` at `completion.rs:510`, on a tree
whose changes were confined to `mogwai-data`, one `mogwai-cli` comment and
markdown. It passed on the immediately following gate run of the identical tree.
So it is INTERMITTENT AND REAL, not an artifact of one pass, and the venue-side
reading above stands. Note what it costs when it fires: this test aborts the
parallel sweep, which reports every `mogwai-data` test as orphaned - the exact
ambiguity `AGENTS.md` warns reads as a brokkr coverage bug. Check for a crashed
test first, and this is the one to expect.

## 10. LEAD, not a finding: a SECOND intermittent lifecycle test, on shutdown rather than completion

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

Not chased by the filing round - out of its scope. Reproduce before believing a
verdict: one failure in two runs on one host.

## What the hunter would actually rewrite

Findings 1, 2 and 3 are all the same underlying problem: THE ACCOUNT AND
CONNECTION LIFECYCLE IS ENCODED ACROSS THREE MUTABLE STRUCTURES - `Run::lanes`,
`Passenger::frozen_since`, and `Passenger::seated_on` - with the consistency
rules living in prose. Each has a path that skips one of the others. Collapse
them: one registry keyed by account, holding the live connections; `is_frozen`
and `is_seated_on` become derived queries; eviction becomes a transition on that
registry taken at the last possible moment before the 101. That also gives
`deliver` the account scope it currently lacks, which is finding 1 for free.

Second, `deliver`'s "unattributed means everyone" default is the wrong direction
now that the ledger is per-passenger. It should be an exhaustive match on frame
class with no fallthrough, so the next per-ledger frame type cannot silently
join the broadcast set the way `AccountState` did.
