# Bug hunt: mogwai-venue market mechanics

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: `fills.rs`,
`fill_golden.rs`, `tape.rs`, `extremes.rs`, `vol_window.rs`, `risk.rs`,
`sweeper.rs`, `source.rs`, `history.rs`, `boatyard.rs`, following load-bearing
calls out into `mogwai-engine` and `mogwai-data` where the invariant lives.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies. Ordered by the hunter's confidence.

## 1. The delivered `AccountState` is taken before funding and expiry are applied, and their own snapshots are then deleted

`sweeper.rs`. High confidence. Bites on ordinary perp and forex runs.

`apply_engine_pass_on_clock` calls the engine in one order and appends its
events in a different one:

```rust
let marked = engine.mark_over(marks, extremes, to_ns);     // snapshot taken HERE
events.extend(engine.expire_orders(to_ns, session_closed, to_ns));
let funded = engine.apply_funding(from_ns, to_ns, to_ns);
events.extend(funded.events);
originated += marked.originated_orders;
events.extend(marked.events);                              // appended LAST
```

Then the dedup keeps only the last `AccountState` in the vector:

```rust
let last_state = events.iter().rposition(|e| matches!(e, VenueMessage::AccountState(_)));
```

Confirmed at the callee side: `Engine::mark_over` pushes
`AccountState(self.snapshot(ts))` when the mark moved or a margin breach
originated; `Engine::apply_funding` pushes one when `paid`;
`Orders::expire_orders` calls `push_account_snapshot` when anything expired
("an expiry frees a hold, so this batch moves the ledger").

So on any pass where the mark moved and funding was paid - a perpetual with a
moving mark crossing a funding instant, or forex crossing a rollover minute -
the sequence is: mark snapshot computed pre-funding, funding debits or credits
the balance, funding's own correct snapshot appended, mark's stale snapshot
appended after it, `rposition` selects the stale one, funding's snapshot is
dropped by the `retain`.

The consumer is handed balances that do not include the funding payment it was
just charged. Same shape for expiry: the retained snapshot still shows the hold
that `expire_orders` just released. The error persists until the next pass
produces a snapshot, and on a quiet symbol - mark not moving - that can be
several passes.

The comment above the dedup is itself part of the defect: it asserts "every
snapshot but the final one reports a stale `mark_px` and `unrealized_pnl`",
which is only true if the final one is the last one computed. It is the last
one appended.

`a_pass_emits_exactly_one_account_state_after_marking` is structurally blind to
this - its fixture is a linear future with no funding terms and no expiring
order, so neither of the two competing snapshots ever exists. The vacuous-gate
shape, both halves green.

The fix is either to move `events.extend(marked.events)` up to where
`mark_over` is called, so vector order equals call order and `rposition` means
what the comment says, or to stop selecting an existing snapshot and take one
fresh `engine.account_snapshot(to_ns)` at the very end of the pass. The hunter
argues for the second: the "pick the last one" rule is derived state that has
now been wrong once, and a terminal snapshot is unconditionally correct by
construction.

Second consequence of the same rule: `enforce_policy` runs after this and
extends `events` with `flattened.events`, which can contain further
`AccountState` frames. So the "exactly one `AccountState` per pass" invariant
the comment claims is already false on any breaching pass, and nothing asserts
it there.

## 2. `PriceExtremes::record` can lose a published span to a racing `take`, and the doc claims the opposite

`extremes.rs`. High confidence on the mechanism. Precisely the hole the module
exists to close, reopened on a race.

`record` loads the epoch, folds, and only then takes the `published` lock:

```rust
let epoch = self.epoch.load(Ordering::Acquire);
let moved = /* fold against writer.span */;
if !moved { return; }
*self.published.lock()... = Some((epoch, span));
```

`take` bumps the epoch first, then reads:

```rust
let closing = self.epoch.fetch_add(1, Ordering::AcqRel);
match *published { Some((epoch, span)) if epoch == closing => { *published = None; Some(span) } _ => None }
```

Interleaving:

1. Tape thread prints a 140 spike. Loads `epoch = 0`. Folds; `moved = true`.
   Preempted before taking the `published` lock.
2. Sweeper calls `take()`. `fetch_add` returns `closing = 0`, epoch is now 1.
   It locks `published`, finds the previous value stamped 0, which does not
   contain the spike, returns it, sets `published = None`.
3. Tape thread resumes and stores `Some((0, span_with_140))`.
4. Next `take()`: `closing = 1`. `published` is stamped 0. Falls to `_ =>
   None`. The spike is discarded.
5. Meanwhile on the tape thread's next `record`, `epoch (1) != writer.epoch
   (0)`, so it resets `writer.span` to that new print. The 140 is gone from
   both sides.

The doc on `take` states: "The epoch is bumped before the take, so a print
racing this call is attributed to the new span rather than silently
discarded." It is silently discarded. Either the function guarantees it or the
comment is a defect.

The window is small - between an atomic load and a mutex acquisition on the
tape thread - but it is on the hottest path in the venue, hit once per
print-that-moves-an-extreme, and the loss is exactly a spike, the class of
event the whole module was built for. The consequence is an unspent drawdown
budget or an unratcheted trailing stop, silently.

The natural fix is to make the publish atomic with respect to the epoch:
re-read the epoch under the `published` lock and, if it has moved, restart the
writer's span at this print with the new epoch and publish that, so the print
lands in the new span rather than being dropped. Alternatively fold the whole
thing into one mutex and drop the epoch trick; the lock is uncontended in the
common case anyway, since it is only taken when an extreme moves.

Neither existing test can see this. `taking_a_span_opens_a_new_one` is
single-threaded, and `an_unread_pass_leaves_its_price_extremes_owed` in
`sweeper.rs` is too. There is no concurrency test on this type at all.

## 3. An unpaced boat (`speed = 0`) feeds the sweeper price extremes from the future

`boatyard.rs` plus `tape.rs`. Medium-high confidence.

`Boatyard::board` builds the clock as:

```rust
let sim = SimClock { sim_epoch_ns: origin, wall_anchor_ns: now_ns(), speed: if speed == 0.0 { 1.0 } else { speed } };
```

so a `speed = 0` boat's `sim_now_ns` advances at wall rate, while `tape::pace`
returns immediately for `speed == 0.0` and the tape thread publishes as fast as
it can synthesize, roughly 2.9M ticks per second per the constant docs. Within
one 5 ms sweep interval the tape can print hours of simulated time.

The sweeper is careful about this for the walk - `scan_triggers(..., to_ns =
sim_now_ns(boat.sim), ...)` bounds the trigger scan to the clock. It is not
careful about the extremes. `PriceExtremes::record` is called for every trade
with no bound at all, and `commit_pass` calls `take()` with no comparison
against `to_ns`. So `enforce_policy` receives a `PriceSpan` whose `high_ns` or
`low_ns` can be far past `to_ns`, and calls:

```rust
ledger.observe(equity_at_that_price, ts)   // ts = span.high_ns, a future instant
```

Two things go wrong. First, this is a lookahead leak of exactly the kind
`history::PageRequest::present` was written to prevent one level down: an
account gets liquidated, or ratchets its peak, on a price its own clock has not
reached. Second, `RiskLedger::observe` uses `ts` for `day_index`, so it can be
handed a far-future instant and then a much earlier `to_ns` on the very next
call in the same `enforce_policy` invocation: the day index jumps forward,
resets `day_open_equity` and clears `locked`, then jumps back and resets it
again. A daily lock can be laundered off by a single unpaced pass.

The trigger walk and the extremes are supposed to describe the same span. On a
paced boat they do, because `record` runs after `pace`. On an unpaced boat they
diverge without bound. Either the extremes need a `to_ns` bound
(`take_up_to(to_ns)`), or `speed = 0` needs to stop pretending its sim clock is
1.0.

## 4. A non-terminating breach permanently masks a terminating rule below it

`risk.rs`. High confidence on the mechanism; medium on whether it is considered
intended.

`observe` checks rules in fixed order - trailing, daily, overall - and returns
on the first that fires. `fire` sets `self.locked = true` unconditionally but
records `self.breach` only for `BreachAction::Terminate`.

So with a policy carrying `trailing_drawdown { on_breach: LockUntilReset }` and
`overall_drawdown { on_breach: Terminate }`, an equity reading below both
thresholds fires the trailing rule, returns, and never evaluates `overall`.
Because the trailing breach is not recorded in `self.breach`, the early-out at
the top of `observe` does not engage, and every subsequent pass re-fires the
trailing rule and returns before `overall` again. The terminating rule is
unreachable for as long as the trailing rule keeps firing - which, since the
floor is ratcheted and the account is now flat, is forever.

The account also never terminates, so `holds_one_ledger` never ends the run,
and `state().breached` reports `None` while the account has in fact blown
through its hard floor.

Secondary consequence of the same code: for a `LockUntilReset` breach,
`enforce_policy` calls `engine.liquidate_all(to_ns)` and logs `warn!("an
account breached its risk policy")` on every single sweep pass for the rest of
the day, because `observe` keeps returning `Breached`. After the first flatten
the liquidation is a no-op, so nothing is corrupted, but the log is a
warn-per-5-ms firehose and `enforce_policy` returns a grown `emitted` count each
pass. A breach that has already been acted on should be idempotent; today only
`Terminate` is.

Suggested shape: record every breach in `self.breach`, or a separate `acted`
flag, and let the reset clear it for `LockUntilReset` rather than relying on
`locked` alone. That makes the early-out cover both actions and lets a
lower-priority terminal rule be reached the pass after the lock is lifted.

## 5. `readable` is venue-wide but scan application is seat-scoped, so an order can rest forever in the gap

`sweeper.rs`. Medium confidence - the hunter could not find a submit-time gate
that forecloses it, but did not exhaustively read the admission path, which was
out of scope.

`readable` is built from every placed boat on the venue:

```rust
let readable: Vec<Symbol> = next_due.values().map(|(boat, _)| Symbol::from(boat.symbol())).collect();
...
let cancelled = engine.cancel_unreadable_orders(&readable, venue_now);
```

but results are only applied to accounts seated on the specific boat:

```rust
scan.symbol.as_ref() == symbol && attached_accounts[*index].is_seated_on(&boat_key)
```

and `Registry::is_seated_on` is true only if one of this account's own
connections has `conn.ride == Some(key)`.

So consider account A riding BTCUSDT and account B riding MNQ. A holds a
resting limit in MNQ - its engine carries the instrument; nothing in the sweep
requires A to ride it. MNQ is in `readable` because B's boat is placed, so
`cancel_unreadable_orders` leaves A's order alone. But when the MNQ boat comes
due, `is_seated_on(mnq_boat_key)` is false for A, so A's MNQ scan is filtered
out of `boat_scans` and never decided. The order rests indefinitely - never
filled, never expired, never cancelled, and indistinguishable from an order the
tape has not reached.

The comment justifying `readable` says "An attached account's order outside
this set rests on a river with no clock: nothing can sweep it" - but the set it
computes is "rivers with a clock", not "rivers with this account's clock", and
those are different sets the moment two accounts ride different symbols. The
predicate that decides cancellation and the predicate that decides sweeping
must be the same predicate; today they are two.

The cheap correct form is to compute `readable` per account from the boats that
account is seated on, rather than once from the whole `next_due` map. Worth
confirming first whether order entry can even accept a symbol the connection is
not riding: if it cannot, this is latent rather than live, but it is still two
predicates where there should be one.

## 6. `last_trade_at_or_before` swallows a materialization refusal as "no print"

`source.rs`. High confidence. Silent, and it wedges the settlement frontier with
zero diagnostics.

```rust
let Ok(river) = self.river(key) else { return Ok(None); };
```

`Rivers::river` returns `KeyMismatch` - an internal routing bug, classified
`is_venue_fault() == true` - and `CapacityExhausted`, a real operational limit.
Both become `Ok(None)`.

`fills::read_last` then does:

```rust
rivers.last_trade_at_or_before(river, ts).unwrap_or_else(|error| { warn!(...); None })
```

so the `Err` path logs and the `Ok(None)` path does not. A `KeyMismatch` -
which by its own doc "no caller can produce" and which is supposed to latch a
terminal fault - produces exactly the same observable as a quiet market: `None`,
no log, no fault.

Downstream, `read_marks` treats a `None` settlement price as a hard refusal, so
`frontier_after` holds `last_swept_ns` and the next pass re-asks the same
unanswerable question. The venue does the safe thing - never retires an unpriced
settlement - and then does it forever, silently, with nothing anywhere saying
why. This is the frontier family's fence-with-no-recovery inverse: correctly
refusing to advance, with no path back and no signal.

Two smaller swallows in the same function:

- The budget check sits after the pull and returns `Ok(None)` even when `last`
  is already `Some`: a walk that found the print and then ran out of budget
  discards a correct answer.
- Both budget exhaustion (`budget == 0`) and a chain that cannot position
  (`try_source_before_target` returning `None`) also collapse into `Ok(None)`,
  indistinguishable from "the tape is quiet here".

The doc directly above the function says "Callers treat `None` as 'the tape
could not be read', and the sweeper's settlement frontier refuses to retire a
span on it, so answering `None` where a print exists would stall the frontier
forever rather than merely losing one reading." That is exactly what these four
paths do. The return type wants to be `Result<Option<Decimal>, _>` with the
refusals as `Err`, and `read_last`'s `warn` wants to cover all of them.

## 7. The banded half of the fill golden certifies nothing, by the module's own admission

`fill_golden.rs`. Confirmed by the source; flagged because it is a
doctrine-named vacuous gate sitting in a file whose whole purpose is to be a
gate.

The module doc states plainly: "At `0.005` the five banded cells came out
byte-identical to the five unbanded ones - same fill counts, same latency
vectors, same pass counts... the banded half currently certifies only that the
band pipeline runs, not that the band bites."

And `assert_shape`'s only band property is:

```rust
assert!(unbanded >= banded, "a banded trigger cannot be easier than an unbanded trigger");
```

which equality satisfies. So half the artifact and the one band-specific
assertion are both vacuous. The cause is correctly diagnosed in the doc -
latency quantized to `SWEEP_INTERVAL_NS = 1 s`, and one second of raw-fill tape
crosses far more than a 0-to-4-tick band displacement - and the remedy is named
as owed work in `notes/todo.md`.

Reported rather than treated as closed because the doctrine's rule is that an
honest "this cannot bite" belongs in the record, and the record here is a module
comment nobody reads before trusting a green golden. The artifact is still a
real byte-exact regression detector for the tape, the predicate and the frontier
arithmetic; it just does not detect band regressions at all. If the band is
worth certifying, the fix is structural: sub-interval latency resolution, or
record the drawn trigger price per order in the artifact rather than only the
quantized latency. The second is cheap and would make the band bite immediately
without touching runtime.

## 8. `MaterializeRefusal` classification is lost, so a cap exhaustion is reported as retryable

`history.rs`. Low-medium severity, high confidence.

```rust
rivers.ensure_reach(key, run_start_ns).map_err(|error| Refusal {
    reason: format!("history could not be produced: {error:#}"),
    retryable: true,
})?;
```

`ensure_reach` flattens `MaterializeRefusal` into `anyhow::Error`
(`self.river(key).map_err(anyhow::Error::new)`), so the caller cannot tell a
`Reach` failure - genuinely transient-ish, venue fault - from
`CapacityExhausted` or `IllegalSymbol`, which are permanent and the request's
fault. Everything is reported `retryable: true`, so a consumer that has
exhausted the 256-river cap will poll forever against a refusal that can never
change.

`MaterializeRefusal::is_venue_fault` exists precisely to make this distinction
and its doc argues at length that getting it wrong in either direction is worse
than the defect it closes. This call site throws it away. The typed refusal
should survive to here.

## 9. Smaller things and observations

- `sweeper.rs`, `enforce_policy` doc is wider than the code. The doc says "The
  closing equity is observed last regardless, because that is the reading the
  published risk state has to agree with." It is not: a breach found at a span
  extreme breaks out and the closing `observe(equity, to_ns)` is skipped. For a
  `Terminate` breach this is harmless, since `observe` early-outs on a recorded
  breach anyway, but for `LockUntilReset` the closing reading is genuinely lost
  and the published `RiskState` disagrees with the last equity the ledger
  folded.
- `sweeper.rs`, `enforce_policy` silently skips unvaluable extremes. `let
  Some(equity) = engine.valuation_at(...) else { continue; }` - an extreme the
  account cannot be valued at is dropped, so a span can be judged on one of its
  two readings with nothing recording that the other was skipped. Everywhere
  else in this file an unanswerable price refuses the whole unit of work; here
  it partially proceeds.
- `boatyard.rs`, `Ticket::drop` decrements whatever is under the key.
  `boats.get_mut(&self.boat.key)` matches by `BoatKey`, not by boat identity,
  and the `_ => false` arm silently no-ops on a `Placing` or `Failed` slot.
  Today this is unreachable - a boat is only removed at `passengers == 0`, when
  no ticket remains - and `Boat::key`'s own doc warns that the key is a sharing
  key rather than a lifetime identity. But nothing enforces the premise, and the
  failure if it ever breaks is a leaked boat whose worker never cancels. An
  `Arc::ptr_eq(&placed.boat, &self.boat)` guard costs nothing and turns the
  premise into a check.
- `source.rs`, `RiverKey::resolve` does not hash `TAPE_PROTOCOL_VERSION`.
  `boatyard.rs`'s module doc says river identity is "symbol or preset, session
  or window shape, loop shape, seed, resolved bundle, market regime, generator
  havoc, and the tape protocol version." The digest covers everything on that
  list except the last. Within one process the version is constant so nothing is
  currently wrong, but the doc states a set wider than the code. Either hash it
  or strike it from the list.
- On the `TAPE_PROTOCOL_VERSION` evasion question specifically: the obvious
  candidate holds up. `CHECKPOINT_K`, `MAX_EXTEND_TICKS` and the sequence of
  `extend_toward` targets all live venue-side, outside the version constant's
  crate, and `CheckpointIndex::extend_toward` selects between two different
  advancement implementations - `advance_parent` whole-parent jumps versus
  per-tick `next_tick` - based on `k`, the remaining budget and the target. If
  those two were not bit-equivalent, the tape would depend on venue-side
  constants and on call patterns, with no version bump possible to catch it.
  They are gated: `mogwai-data`'s
  `compact_parent_advancement_matches_wire_frames_and_continuation` walks 512
  parents through both paths and then 1,000 further ticks asserting
  continuation. `coarsen`'s correctness argument is separately stated and its
  no-exemption rule is explicit. So this one is closed - but it is closed by a
  single test in another crate, and nothing in `mogwai-venue` records that its
  `CHECKPOINT_K` is only safe because of it. A one-line comment at
  `CHECKPOINT_K` naming that test would keep the next person from re-deriving
  the whole chain, or worse, not deriving it.
- `fills.rs` `MarketReadingCache` is sound but its identity argument rests on
  one thing worth stating. The resident-window path passes `SWEEP_DRAIN_BUDGET`
  as the window's budget, the same value `read_market` passes to `vol_reading`,
  so `vol_window`'s "the window refuses first" argument holds only because
  those two are literally the same constant. They are, today, at both call
  sites. If the walk's budget were ever parameterized per caller, the window
  could serve a read the walk would have refused, and the two implementations
  would silently diverge with both halves green. That is the
  two-constants-encoding-one-quantity shape; it is currently one constant, which
  is the right answer, and it is worth a comment at the `window.read(...)` call
  site saying so.

## Structural recommendation

Findings 1 and 2 are the same underlying disease: derived selection where a
direct statement was available. The pass picks its outgoing snapshot by scanning
a heterogeneous event vector for the last `AccountState` instead of taking one
at the end; the extremes channel reconstructs "which span does this print belong
to" from an epoch stamp read outside the lock instead of deciding it under the
lock. Both are clever, both are cheaper than the obvious thing by an amount that
does not matter, and both have now produced a silent wrong answer.

Given the pre-1.0 posture, the hunter would rewrite both outright rather than
patch:

- `apply_engine_pass_on_clock` returns the batch plus one snapshot taken after
  every mutation, and the `retain` dance is deleted. `enforce_policy`'s flatten
  then appends before that snapshot is taken, which also fixes the
  one-snapshot-per-pass invariant on breaching passes for free.
- `PriceExtremes` collapses to a single `Mutex<Option<PriceSpan>>` with the span
  reset inside `take`. The lock is only touched when an extreme moves, which
  after the first few ticks of a span is rare, so the atomic-epoch optimization
  is buying approximately nothing and costing a correctness argument that turned
  out to be wrong.

Both are small, contained, and remove a category rather than an instance.
Nothing here needs a `TAPE_PROTOCOL_VERSION` bump - none of it touches tape
generation.
