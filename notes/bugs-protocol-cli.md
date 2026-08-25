# Bug hunt: mogwai-protocol and mogwai-cli

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: the wire types and their
validators, `launch`, and the `mogwai` binary's clap dispatcher plus its
socket-backed lifecycle, serving and completion tests. The headline finding was
traced end to end - protocol validator, the venue's `stamp_market_price`, the
engine's fill-price draw - rather than inferred.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies.

## 1. High - `validate_submit_order` admits a `Market` order carrying a consumer-supplied `price`, and its own doc says that is impossible

`crates/mogwai-protocol/src/messages.rs`, `validate_submit_order`, the big
`match order.order_type` block.

Walk the arms for `OrderType::Market { price: Some(x), trigger_price: None,
trail_offset: None, limit_offset: None, post_only: false, time_in_force: Gtc }`:

- arm 1 (`Market if trigger_price.is_some()`) - no
- arms 2 and 3 (`Limit`) - no
- arm 4 (`StopMarket|MarketIfTouched|TrailingStopMarket if price.is_some()`) -
  `Market` is deliberately not in that set
- arm 5 (`is_conditional() && trigger_price.is_none()`) - `Market` is not
  conditional
- arms 6 to 18 - all no

So `Ok(())`. There is no arm anywhere that refuses a price on a plain `Market`
order.

The doc block directly above the function - the "deliberate two-phase split"
paragraph - asserts:

> "The venue then stamps a synthetic execution price onto every Market order (on
> both the WS and HTTP carriers, failing loudly if synthesis fails) before the
> engine ever sees it, so by the time `validate_submit` runs the order always
> carries a price and a still-priceless one is a genuine post-stamp bug."

The stamp is `crates/mogwai-venue/src/http.rs`, and it reads:

```rust
let stamp = |order: &mut mogwai_protocol::SubmitOrder| {
    if order.order_type == OrderType::Market && order.price.is_none() {
        order.price = last_px;
    }
};
```

It stamps only priceless market orders. A market order that arrived with a
price is passed through untouched, and the "venue could not synthesize a market
price at sim-now" refusal a few lines earlier is keyed on the order still being
priceless, so a priced market order bypasses that gate too.

In the engine (`crates/mogwai-engine/src/orders.rs`, the `takes_the_market`
branch), the consumer's value lands in `stated_px`, and:

```rust
let fill_px = if takes_the_market {
    reading.map_or_else(
        || {
            tracing::warn!(..., "market order has no market reading; using its stated price");
            stated_px
        },
        |value| draw_market_price(self.fill_seed, &order, stated_px, value.last_px, ...),
    )
} else { ... };
```

Two consequences:

- When there is no market reading, the fill price is the consumer's own number,
  with nothing but a `tracing::warn!`. A consumer sends
  `{"order_type":"Market","price":"1.00","side":"Buy",...}` at a moment the
  reading is absent and buys at 1.00. `validate_fill_funds` is then run against
  that price, so the funds check does not catch it either - it agrees the trade
  is affordable.
- When there is a reading, `stated_px` is still fed into `draw_market_price`
  alongside `last_px`. The hunter did not chase how far `stated_px` steers that
  draw, but a consumer-controlled input reaching the fill-price draw of a market
  order is worth an audit on its own.

This is the vacuous-gate family in its purest sub-shape from
`test-doctrine.md`: "a doc, comment or help text describing a gate wider or
narrower than the gate", plus "a two-phase split where the second phase does not
do what the first phase's doc says it does". Both halves are green because
nothing tests a priced market order.

The fix that matches the stated design is one arm in `validate_submit_order` -
`OrderType::Market if order.price.is_some() => Err("Market order must not carry
a price")` - mirroring arm 4, which already says exactly that for the three
market-on-trigger types. Note the engine's `validate_submit` (`orders.rs` around
2347) requires a price on `Market` post-stamp, so the refusal has to go in the
wire gate, not the engine one.

Confidence: high on the gap; high on the no-reading exploit; medium on whether
the with-reading path is also exploitable.

## 2. Medium - `validate_submit_group` admits parent cycles, producing a group that is legal, admitted, and permanently inert

`messages.rs`, `validate_submit_group`.

The self-containment loop checks that every `parent_order_id` names a member:

```rust
if link.parent_order_id.as_ref().is_some_and(|parent| !names_member(parent)) {
    return Err("an order group's parent must be a member of the group");
}
```

`validate_order_link` separately refuses `parent == self`. Nothing checks for a
cycle of length two or more. Submit a two-member group where A's
`parent_order_id` is B and B's is A: both are members, neither is itself, both
carry legal contingencies. The group is admitted atomically. Per the
`OrderLink::parent_order_id` doc, a child "rests inert - unscanned, holding no
hold - until its parent fills." Neither can ever fill, because each waits on the
other. The consumer sees two `OrderAccepted` frames for orders that are
structurally dead for the life of the run, with no event ever explaining it.

The same shape applies to longer cycles (A to B to C to A) up to
`MAX_GROUP_ORDERS`. The group frame's whole selling point is the three numbered
guarantees in the `SubmitOrderGroup` doc; a cycle satisfies all three vacuously.

A depth or acyclicity check is cheap at `MAX_GROUP_ORDERS = 9`. Note
`sizing.rs`'s `worst_case_output_bytes` comment mentions "the depth rule in
`Engine::validate_link`", so a depth rule exists engine-side; whether it catches
a cycle or only bounds generations was not verified, and it is worth checking,
because the wire gate is the one that claims self-containment.

Confidence: high that the wire gate does not check it; medium on whether
anything downstream does.

## 3. Medium - a configured instrument symbol outside the wire alphabet is unreachable by order entry, with no gate anywhere saying so

`messages.rs`, `validate_wire_symbol` doc:

> "`config.rs` does not apply it to an instrument's own `symbol`, which is a
> recorded asymmetry rather than an oversight."

The asymmetry is recorded, but its consequence is not, and the consequence is a
dead instrument. `validate_submit_order` calls `validate_wire_symbol(&order.symbol)`,
and `/trades`, `/quotes` and `source` all apply the same rule. So an operator
who configures an instrument named `MNQ Z5` (space), `BTC/USDT` (slash), or
anything non-ASCII gets a venue that boots fine, builds the profile fine, serves
it as a default river if it is the default - and refuses every order-entry frame
naming it, and every history request for it, at the wire gate, with "symbols use
only ASCII letters, digits, dot, dash or underscore". The operator learns hours
later from a run where nothing fills.

The whole argument for `validate_currency_code` two hundred lines below is
precisely this failure mode - "a code that no balance can ever equal ... freezes
a policed account's equity at zero, and it does so silently" - and it concludes
that the right place to catch it is at config load, failing startup. The same
argument applies verbatim to an instrument symbol and is not applied. Either
apply the alphabet at config load, or the asymmetry note needs to say "such an
instrument cannot be traded" rather than leaving it as a neutral fact.
Confidence: high.

## 4. Low-medium - `SimClock::window_opening`'s guard cannot fire

`crates/mogwai-protocol/src/clock.rs`:

```rust
pub fn window_opening(&self, wall_armed_ns: u64) -> u64 {
    self.sim_ns(wall_armed_ns).max(self.sim_epoch_ns)
}
```

with the doc: "A reader whose epoch is later than the arm receives the full
window from its own epoch instead of inheriting a window in its past."

`sim_ns` has exactly three exits: `sim_epoch_ns` (wall at or below anchor),
`sim_epoch_ns` (degenerate speed), and `self.sim_epoch_ns.saturating_add(scaled)`.
All three are at least `sim_epoch_ns`. The `.max()` is a no-op on every input,
so the function is `sim_ns` with a doc claiming a behaviour it does not
implement.

Harmless today, but it is the shape doctrine names: a comment stating a
guarantee, where either the function guarantees it or the comment is a defect.
The live risk is the inverse - someone reading this concludes `window_opening`
protects against a `sim_ns` below the epoch and relies on it after `sim_ns`
grows a fourth exit. Confidence: high that it is vacuous; low on impact.

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
