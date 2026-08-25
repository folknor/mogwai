# Bug hunt: mogwai-venue serving path

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: `serve.rs`, `ws.rs`,
`registry.rs`, `admission.rs`, `run.rs` and `http.rs` serving halves, `config.rs`,
`lib.rs`, plus `boatyard::board` and the clock and config knobs they ride on.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies. Ordered by severity.

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
