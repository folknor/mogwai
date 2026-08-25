# Bug hunt: mogwai-venue serving path

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: `serve.rs`, `ws.rs`,
`registry.rs`, `admission.rs`, `run.rs` and `http.rs` serving halves, `config.rs`,
`lib.rs`, plus `boatyard::board` and the clock and config knobs they ride on.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies. Ordered by severity.

## 1. `reset_account_on_reconnect` deletes the connection record the admission just committed

Serious, high confidence.

`ws.rs` runs the admission in this order:

```
line 511  let (attach, committed) = state.run.commit_admission(&mut reservation, callsign, Some(ticket.boat().key()), claimed);
line 521  let account_state = state.run.claim_account(&account_id, claimed, callsign, resetting);
```

`registry::commit` pushes a `ConnectionRecord { id, phase: Committed, ride,
lanes: None }` into the account's `AccountEntry` and sets `handoffs += 1`.

Then, when `resetting` is true, `Run::claim_account` calls `reopen` calls
`discard_account`, and `discard_account` ends with:

```rust
self.registry.forget(account_id);   // run.rs, discard_account
```

`ConnectionRegistry::forget` does `self.locked().remove(account_id)`: it
removes the whole `AccountEntry`, including the connection record commit
installed one line earlier, its ride, and its handoff. The very next call,
`self.account(account_id)`, mints the fresh ledger and calls
`registry.ensure(...)`, which `or_insert`s a new, empty entry - frozen, zero
connections.

Consequences, all silent:

- `bind_lanes` becomes a no-op. `registry::begin_reading` looks up
  `entry.connections.iter_mut().find(|c| c.id == connection_id)`, finds
  nothing, and returns early - the `lanes` argument is dropped on the floor.
  This connection is therefore never in `ConnectionRegistry::bound_lanes()`,
  which is the only channel the fill sweeper delivers on (`sweeper.rs:782`). A
  resting limit order fills in the engine and the consumer is never told: it
  receives its `OrderAccepted` on the direct dispatch path and then nothing.
  This is exactly the shape the venue exists to test.
- The account reads as permanently unattended. `is_frozen` is true and
  `frozen_for` runs from the `ensure` timestamp, so with `account_ttl_ms`
  greater than zero the TTL reaper will `discard_account` a live, trading
  connection's ledger mid-run.
- The ride is unrecorded. `is_seated_on` returns false, so the sweeper's seat
  checks (`sweeper.rs:215`, `:378`) skip this account's boat, and the
  cadence-conflict rule in `registry::reserve` no longer sees the seat.
- `release_lanes` at teardown is a no-op, and `Attach::drop` finds nothing.
  Harmless, but it means every teardown path is silently inert too.

This is not only a reconnect. `claim_discards_ledger` is `claimed &&
reset_account_on_reconnect && !has_matching_identity_on(...)`, which is true on
the first connection of any named account when the knob is on. So with the knob
enabled, essentially every named-account socket is affected.

Why it survived: `grep -rn reset_account_on_reconnect` over the whole workspace
shows the knob is `false` by default and no test anywhere sets it true. The
registry lifecycle tests (`run.rs:2487`, `:2524`, `:2564`, `:2655` and kin) all
go through `claim_account` with `resetting = false`.

The structural fix is not to reorder the two calls: the ledger discard
genuinely needs to happen, and moving it before commit reintroduces the "refuse
after eviction" hazard the reserve/place/commit split closed. The right move is
that `discard_account` must not call `registry.forget` - the registry describes
connections, not ledgers, and the two lifecycles were conflated. Ledger discard
should reset the ledger and leave the connection table alone; only the TTL
collector, which has already established that nothing is reading, should forget
a registry entry. The comment at `discard_account` justifying `forget` ("a
collected account that came back would otherwise be refused for naming a
different world") is reasoning about the TTL path and was applied to the reset
path where it is wrong.

## 2. A panicked history synthesis leaves the consumer hanging, under a comment saying it does not

Vacuous gate, high confidence. `ws.rs::spawn_history_page`, the `JoinError` arm:

```rust
// A panicked synthesis is a venue fault, not an empty window, and
// the consumer is told so rather than left waiting out its timeout.
Err(error) => {
    tracing::error!(%error, "history synthesis task failed");
}
```

The comment states the guarantee; the code logs and returns without sending
anything. The consumer's `QueryHistory` `request_id` is never resolved - no
`HistoryPage`, no `HistoryRejected` - which is precisely "left waiting out its
timeout". The three sibling arms all emit a frame; this one does not. Textbook
"a comment says a function guarantees something and it does not".

Fix is one `send_admission(..., HistoryRejected { request_id, retryable: true,
.. })`, which needs `request_id` cloned before it is moved into the blocking
closure.

Related, same function: on the `Ok(Ok(Ok(payload)))` path, if
`lanes.reserve_admission()` returns `None` the page is dropped with no frame
and no log. That one is argued - the peer is not reading - but it is the same
unresolved-request-id outcome and is not stated in the doc comment.

## 3. `registry::reserve` mutates ledger identity on a proposal, and the commit's revalidation cannot fire

Medium.

```rust
if resetting {
    entry.incarnation += 1;
} else if let Some(sitting) = ... { return Err(CadenceConflict) }
```

Two problems:

- The field's own doc says "Bumped whenever the account is reset". It is bumped
  whenever a reset is proposed. A reservation that rolls back - placement
  failure, or the upgrade future cancelled between reserve and commit, both
  explicitly supported - leaves the incarnation advanced with no ledger reset
  having occurred. This is inert today, but it contradicts the module's stated
  discipline that a proposal changes no lifecycle state, and `rollback` does
  not undo it.
- The commit's staleness check `entry.incarnation != reservation.incarnation`
  is vacuous by construction: `pending` exclusivity already guarantees no other
  admission can run, and nothing else in the codebase writes `incarnation`.
  Worse, the actual ledger discard (`claim_account` calls `reopen`) happens
  after commit and never touches `incarnation`, so the field does not track
  ledger identity at all. The comment claims it catches "a commit onto a ledger
  incarnation other than the one every check upstream was taken against" - it
  cannot. It reads as gated and is not.

Given finding 1, the hunter would delete `incarnation` outright rather than
repair it; the pending-exclusivity is the real linearization and the second
mechanism is a decoy.

## 4. The daily-reset calendar refusal is skipped on exactly the path that installs a new policy

Medium confidence, possible bug.

`ws.rs` refuses an upgrade whose account resets its daily loss limit at a UTC
minute the symbol's footprint never contains:

```rust
if let Some(reset) = state.run.daily_reset_minute(&account_id, resetting)
    && let Some(reason) = daily_reset_refusal(...)
```

but `Run::daily_reset_minute` opens with `if resetting { return None; }`. So a
claim that discards the ledger skips the check entirely - and that is the path
that replaces the account's policy with the run's `account_opening_terms`. If
those terms carry a daily reset minute outside the bound symbol's calendar, the
account is admitted with a limit that never resets, which is the condition the
refusal exists to prevent. The non-resetting path checks the outgoing policy;
the resetting path checks nothing. If the boot config validates
`account_opening_terms` against every configured calendar somewhere the hunter
did not find, this is a non-issue; no such check was found.

## 5. `Run::complete` uses `watch::send`, which silently discards the completion on zero receivers

Latent, with the test pinned to a different API.

```rust
pub(crate) fn complete(&self, sim_now_ns: u64, elapsed_ns: u64) {
    if self.complete_tx.send(Some((sim_now_ns, elapsed_ns))).is_err() {
        tracing::debug!("run completed after all websocket receivers closed");
    }
}
```

`watch::Sender::send` returns `Err` and does not update the stored value when
every receiver is gone. If that ever happens, the run's terminal state stays
`None` forever: any socket that connects before the accept loop stops - there
is a real window between `complete()` and the graceful-shutdown drain - reads
`current_completion() == None`, never sends `RunComplete`, and waits on its own
duration instead.

Today it cannot fire, because `sweeper::spawn_fill_sweeper` is spawned
unconditionally and holds a `run.completion()` receiver until it observes the
change. That is an accidental invariant - it holds only because "there is no
configuration in which limits do not rest" - and nothing at `complete()` states
it.

The tell that this is drift rather than a considered choice: the regression
test for exactly this property,
`ws.rs::receiver_created_after_completion_observes_terminal_state`, builds its
own `watch::channel` and calls `send_replace`, not `send`. It pins the desired
behaviour against a hand-built channel rather than against `Run::complete`, so
it is structurally blind to which API production uses. One `send_replace` in
`Run::complete` closes it; better still, drive the test through a real `Run`.

## 6. An upgrade cancelled between commit and the 101 evicts the incumbent and leaves nobody

Medium, arguably by design but unstated.

`commit_admission` selects the displaced set, `close_displaced` sends the
1000/evicted closes, and only then does `ws_upgrade` await
`register_instrument(...)` before constructing the `Passenger` and returning
the upgrade response. Hyper drops the handler future when the client
disconnects. A client that dials `/ws?account=X`, gets past commit, and drops
during `register_instrument` has killed the incumbent's socket and its ledger
continuity for free: the successor never exists, and the `Attach` drop just
retires the empty record.

The module doc says "Nothing refuses after this point", which is true and is
not the same property as "nothing can fail after this point". Cancellation is
not a refusal but the incumbent pays identically. Either move
`register_instrument` before the commit - it is idempotent and needs only
`account_state`, which the reservation already pins - or state the window.

Same class, lower stakes: `commit`'s "unreachable" missing-entry arm and its
`stale` arm both return `Committed { displaced: vec![], connection_id, .. }`
and set `reservation.committed = true`, so the caller proceeds to build a
`Passenger` and bind lanes for a connection the registry never installed - the
same silently-unbound-lanes end state as finding 1. Given finding 1 makes that
arm reachable in a different way, the hunter would make both arms return a
refusal the upgrade can answer 503 to, rather than a lie shaped like a success.

## 7. Smaller findings and observations

- `registry::is_seated_on` doc/code mismatch. "Whether this account is riding
  `key`, derived from its reading connections" - the body filters on
  `conn.ride`, with no `phase == Reading` test, so a `Committed` connection
  counts. Including it is probably correct (the ride is held from commit), but
  the sentence is wrong and the sweeper's two `is_seated_on` call sites read it
  as a delivery statement.
- Two copies of the "speed 0 means unpaced, so the clock runs at 1.0"
  convention, at `boatyard.rs:273` and `config.rs:2135`, with no shared helper
  and no gate asserting they agree. This is the doctrine's "two constants
  encoding one quantity" shape: `validate_delivery_speed` and `validate_speed`
  both admit `0.0`, and every downstream `wall_duration` returns `u64::MAX`,
  roughly 584 years, for a speed-0 clock, so a third reader that forgets the
  substitution wedges the exec pump, the act delay, the passenger duration
  timer and the deadline task simultaneously, with no error anywhere. Worth a
  single `SimClock::for_speed(f64)` constructor that owns the rule.
- `dispatch_command`'s "no `.await` between here and `submit_produced`" comment
  gives only half the reason. It argues publication order versus the sweeper.
  The second reason is that `handle_socket` teardown calls
  `dispatcher.abort()`, and an `.await` inserted after
  `engine.process_with_market_on_clock` would let the abort land there: the
  engine has already mutated, the events are still in a local `Vec`, and the
  consumer is never told about an order the venue accepted. The current code is
  safe - abort can only land at existing await points, all of which precede the
  mutation - but the invariant is load-bearing and only half-documented.
- `serve_until_drained`'s grace-expiry bail masks a terminal fault. In
  `serve_async`, `serve_until_drained(...).await?` returns early on
  "connections did not drain", so the `terminal_fault` check below never runs
  and a venue that faulted and failed to drain reports only the drain message.
  Trivial, but the fault is the more important half.
- `settle_attendance` has a `clippy::collapsible_else_if` shape (`else { if
  frozen_since.is_none() { ... } }`); worth folding if clippy is not currently
  seeing it for some reason.
- `arm_divergence` is unauthenticated and can retarget or mutate any account
  mid-connection, including `FeeSurcharge` and engine arms applied venue-wide.
  This is stated at the route in `serve.rs` as a convention held by callers.
  Flagged only because the convention is what makes several other items - the
  pending-arm shed cap, the unordered application of concurrent arms - read as
  non-hazards, and nothing in the venue enforces it, so if the attach topology
  ever puts a non-cooperating consumer on the venue, several findings reactivate
  at once.
- Spawned history tasks outlive their socket. `spawn_history_page` spawns a
  detached task holding an `ExecLanes` clone; `handle_socket` never tracks or
  aborts it. After teardown the blocking synthesis runs to completion, still
  holding one of the four global history slots, and its result is emitted into a
  channel nobody reads. Bounded and not a correctness bug, but it means
  `MAX_CONCURRENT_HISTORY_SLOTS` can be occupied entirely by dead connections,
  and those `ExecLanes` clones also delay `run_writer`'s `priority_open` from
  ever going false - the `CLOSE_GRACE` abort is what actually ends the writer.
