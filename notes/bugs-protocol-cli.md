# Bug hunt: mogwai-protocol and mogwai-cli

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: the wire types and their
validators, `launch`, and the `mogwai` binary's clap dispatcher plus its
socket-backed lifecycle, serving and completion tests. The headline finding was
traced end to end - protocol validator, the venue's `stamp_market_price`, the
engine's fill-price draw - rather than inferred.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies.

Round 1 is closed. Findings 1, 2 and 4 landed; finding 3 did not survive
contact with the code and its reasoning is recorded in
`notes/bug-loop-carry-forward.md` rather than left here as an open gap.
Findings 5 through 8 and the structural observation are round 2 and are
untouched.

## 5. Low - `validate_modify_order`'s doc describes a two-field gate that is now three

`messages.rs`:

> "API-boundary guard for a `Command::ModifyOrder`'s `price`/`quantity` pair ...
> At least one of the two must be present"

The signature takes three arguments and the refusal text names all three
("ModifyOrder must set price, quantity and/or trigger_price"). A reader taking
the doc at face value concludes a trigger-only amend is rejected as a no-op; it
is accepted. Same family as finding 4 - small, but the doctrine's rule is that
prose is the only artifact here with no compiler.

Separately worth noting: this validator has no order-type argument, so a
`trigger_price` amend on a non-conditional order passes the wire gate.
`Command::ModifyOrder`'s own doc says "on anything else it is rejected", which
is presumably the engine (`orders.rs` around 2886 shows "Market order must not
carry a price amend", so that family exists). Consistent as a two-phase split,
but the wire doc does not say so.

## 6. Low - `AccountPolicy::validate`'s cross-currency refusal names an arbitrary offender

`crates/mogwai-protocol/src/risk.rs`:

```rust
&& let Some(other) = self.opening_balances.keys()
        .find(|currency| *currency != policy_currency)
```

`opening_balances` is a `HashMap`, so with two or more offending currencies the
named one is `HashMap` iteration order - a coin flip per process. The error is
correct either way, but doctrine explicitly flags this shape ("a test resting on
`HashMap` iteration order passed against its defect half the time"): any test
asserting which currency the message names is a coin flip, and a diagnostic that
differs run to run for one config is a support cost. A `BTreeMap`, or sorting
the offenders, or naming all of them, removes it.

## 7. Note - `Command::SubmitOrderGroup`'s reservation is sized on unvalidated `orders.len()`; the ordering is currently safe

`sizing.rs`'s `worst_case_output_bytes` charges `members * (5 *
ORDER_EVENT_MAX_BYTES + LINKAGE_MAX_BYTES) + (members + 1) *
account_state_max_bytes(...)` from `orders.len()`, with no reference to
`MAX_GROUP_ORDERS`. `admission.rs`'s `reserve_swept` doc claims "the count is
bounded by `MAX_GROUP_ORDERS`, which is what keeps this a reservation rather
than an unbounded write" - a bound that is not enforced at the sizing site.

The hunter traced the HTTP path (`http.rs` around 253 `boundary_refusal`,
around 779 `lanes.reserve`) and validation runs first, so a 300-member group is
refused as a validation error before it ever reaches the reservation. Not a live
defect. Flagged because the safety is entirely an ordering property at the call
site, unstated at the sizing site, and the sizing function is `pub`. A
`min(orders.len(), MAX_GROUP_ORDERS)` or a debug assertion inside
`worst_case_output_bytes` would make it structural. With 64 KiB
`MAX_INBOUND_MESSAGE_BYTES`, the worst reachable reservation is roughly 25 MB,
so even if the ordering inverted it would be a wrong-shaped refusal -
`AdmissionRejected`, which the crate elsewhere insists "reads as a capacity
signal" and must not be used for malformed requests - rather than an overflow.

## 8. Note - launcher and CLI: nothing found wrong, and the reasons are worth recording

`launch.rs` is the most carefully worked file in either crate and the hunter
could not break it. Specifically checked and found correct:

- Unconditional reap. The owner loop's `try_wait` runs on both arms; the
  `reaped` flag is read from the `exit` slot before the teardown kill, so a
  double-kill of an already-reaped child cannot manufacture a false `Teardown`
  failure.
- `Timeout` and `Thread` paths both reach the unconditional kill and reap.
  `LaunchError::Thread`'s claim that "no venue is left running" holds at the
  reader-thread site because `serving == false` falls through to
  `kill_child_group` plus `child.wait()`.
- `record_stderr_line`. First eviction costs two real lines and inserts the
  marker (64 to 63 to 62 to 63 to 64); subsequent evictions cost the marker plus
  one line. Bounded, head preserved, one marker. Matches its test.
- `read_ready`'s ceiling. `take` wraps the raw stream, not the `BufReader`, so
  the byte bound is real and not a post-hoc truncation. The `CountingReader`
  test asserts on delivered bytes rather than on the error, which is the right
  assertion.
- `--launcher-pid` accepts a negative `i32` (`ServeArgs.launcher_pid:
  Option<i32>`) while `serve_argv` renders from `u32`. `arm_parent_death_signal`
  compares a negative or zero value against `getppid()` and bails, never
  reaching `kill(-1, ...)`. Not a bug - recorded because "clap takes `i32`, the
  renderer emits `u32`" is the shape that usually is one.
- `serve_argv_parses_in_the_venues_own_grammar` is a genuine cross-implementation
  gate - protocol renders, CLI parses - and its companion refusal test keeps it
  from reading as "any parser would do". Fixture discipline done right, in a
  module, exactly as doctrine prescribes.

The one launcher property not verifiable from inside the crate:
`format_duration`'s `u64::try_from(nanos)` fallback emits `format!("{}s",
duration.as_secs())` for durations past roughly 584 years. That renders as a
`u128` second count; `humantime` parses it, but the hunter did not confirm it
survives `serve`'s `u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)` as
anything meaningful. It saturates to `u64::MAX` ns, which is indefinite-ish, not
the requested span. Unreachable in practice; noted for completeness.

## Structural observation, outside the immediate ask

`validate_submit_order`'s match is eighteen guard arms whose correctness depends
entirely on their order, and the correctness argument is carried in comments -
one arm's comment explicitly says "It sits ahead of the conditional-IOC arm
deliberately, and `Engine::validate_submit` checks the two in the same order".
Finding 1 is precisely a hole in that arm lattice, the `Market`/`price` cell
that no arm covers, and it is invisible because reading eighteen guards and
computing their complement is not something a human does reliably.

Given the pre-1.0 posture: the honest structure here is a per-`OrderType` table
of required, forbidden and optional for each of `price`, `trigger_price`,
`trail_offset`, `limit_offset`, `expire_time`, plus the two cross-field rules
(`post_only`, conditional-versus-IOC) that genuinely are rules rather than table
cells. Nine order types times five fields is 45 cells, exhaustively written,
exhaustively testable, and a missing cell is a compile error rather than a
fall-through to `Ok(())`. The engine's `validate_submit` (`orders.rs` around
2347) is a second copy of the same lattice with the same ordering dependency,
and the two are kept in agreement today by a comment asking a future reader to
keep them in agreement. One shared table, read by both, is the version of this
that cannot drift - and it is exactly the fixture-discipline move
`test-doctrine.md` describes for two sides sharing a crate graph, which
`mogwai_protocol::close` already does for the WS close vocabulary. The hunter
would rewrite it.
