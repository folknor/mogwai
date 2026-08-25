# Bug hunt: mogwai-venue market mechanics

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: `fills.rs`,
`fill_golden.rs`, `tape.rs`, `extremes.rs`, `vol_window.rs`, `risk.rs`,
`sweeper.rs`, `source.rs`, `history.rs`, `boatyard.rs`, following load-bearing
calls out into `mogwai-engine` and `mogwai-data` where the invariant lives.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies. Ordered by the hunter's confidence.

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
