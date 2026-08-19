# Bug hunt: mogwai-engine

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-engine`: order state machines, the matching loop, fills and
partial fills, order groups and their linkage, account and position state, and
the divergence-injection seam.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

FINDINGS 1 THROUGH 3 ARE FIXED AND REMOVED; FINDING 4 IS WITHDRAWN, see section
11 below for why and for the process lesson it cost. The three that landed
reproduced as described and were three defects rather than symptoms of one; the
two that touched `Resting::Held` (the orphaned children and the amend that
promoted a child) were independent. The reap rule now has one home,
`Engine::close_out` with its unrested twin `close_unrested`, which every terminal
path routes through except `reap_children_of` itself. Section numbering is
unchanged: later briefs and the carry-forward cite these by their original
numbers.

The hunter read all four source files (`orders.rs`, `account.rs`, `lib.rs`
non-test region, `divergence.rs`) plus the protocol-side helpers the engine
leans on (`InstrumentDef::unrealized`, `validate_submit_group`,
`InstrumentClass`). No files modified.

## 5. Inverse contracts: two P and L formulas, and the wrong one is used at the decision points

`InstrumentDef::unrealized` carries the inverse arithmetic
(`multiplier * qty * (1/entry - 1/exit)`), and `apply_fill` deliberately routes
realized P and L through it - its comment says "Realized and unrealized must
come from the same expression or a position's value would jump the moment it
closed." Three places compute the linear form by hand instead, and all three see
`Inverse` because `is_future()` includes it:

- `Engine::unrealized_pnl` (lib.rs) - `(mark - avg) * qty * multiplier`. This
  feeds `collateral_contribution`, which feeds `apply_margin_breaches`. MARGIN
  BREACH AND FORCED LIQUIDATION FOR AN INVERSE CONTRACT ARE DECIDED ON THE WRONG
  NUMBER, and it is wrong in sign as well as magnitude for a move of any size.
- `Engine::positions()` - same hand-rolled expression, so the wire
  `Position.unrealized_pnl` is wrong for inverses, and contradicts what
  `valuation_at` reports for the same position.
- `Engine::settle` - `(settle_px - avg_px) * qty * multiplier` credited to the
  balance. An inverse future's REALIZED SETTLEMENT is booked with the linear
  formula while an inverse fill's realized P and L is booked with the inverse
  one.

All three should call `def.unrealized(...)`. The duplication is the bug; there
should be one expression, as `apply_fill` already says.

Confidence: high.

## 6. `margin_requirement` ignores `MarginBasis`, contradicting its own doc

lib.rs `margin_requirement` computes rows as `maintenance_per_contract * |qty|`
and `initial_per_contract * leaves_qty` - raw multiplication, not
`policy.maintenance(...)` and `policy.initial(...)`. Under
`MarginBasis::Notional` those fields are FRACTIONS, so a 40 percent requirement
on two contracts is reported as `0.8`.

Its own doc says "Their sum is exactly what `locked_balances` reserves, so the
reported margin reconciles with the reported `locked` by construction" - and
`locked_balances` and `order_reservation` both go through the policy methods,
honouring `basis`. So for any notional-basis account the invariant the comment
asserts is simply false, and the wire `AccountState.margins` disagrees with
`AccountState.balances[].locked`.

This is the exact defect `apply_margin_breaches` was already fixed for - that
function carries a comment about a notional policy having "read a 40 percent
requirement on two contracts as eighty cents". The same fix was not applied
here.

Confidence: high.

## 7. `order_reservation` reads `submit.price` while its caller passes a `price` it then ignores

account.rs `order_reservation(submit, leaves, price, clipped)` takes a `price`
argument, and the futures and equity branches DISCARD it, using
`submit.price.unwrap_or_default()` instead. Its caller `order_reservation_entry`
computes `price = submit.price.or(submit.trigger_price)` precisely so a
price-less `StopMarket` has one - and that work is thrown away. Consequences:

- A `StopMarket` ON A FUTURE OR MARGINED EQUITY UNDER `MarginBasis::Notional`
  RESERVES NOTHING AT ALL while resting (`initial(qty, 0)` = 0). It holds no
  collateral until it triggers.
- `held_for` (which routes through the same function) correspondingly returns 0,
  so the two agree - but `on_modify`'s hand-rolled futures branch does NOT: it
  adds back `policy.initial(instrument, leaves, submit.price.or(submit.trigger_price))`,
  i.e. the full trigger-priced requirement the order never held. An amend is
  checked against free balance plus money that was never locked.

Either the argument should be used (`price`, not `submit.price`), or it should
be removed. Having it present and ignored is what let the caller believe the
trigger fallback was in effect.

Confidence: high on the code path; the bite is confined to
`MarginBasis::Notional`, since `PerContract` ignores price by construction.

## 8. `on_modify`'s futures funds check uses the amend's `price`, not `effective_price`

Same block, orders.rs ~2708:

```rust
policy.initial(instrument, new_leaves, price.unwrap_or_default())
    .saturating_add(commission)
```

`price` is the amend's optional new price. Every other branch in this function
uses `effective_price` (`price.or(submit.price)`, or the trigger for a
`StopMarket`), and `commission` two lines above is computed against
`effective_price`. So a QUANTITY-ONLY amend of a futures order under a notional
policy computes `required = initial(new_leaves, 0) + commission = commission`,
against `held` = the full old requirement - the funds check cannot fail, and an
amend that doubles the size of a leveraged position is admitted unconditionally.

Confidence: high. Note this whole block is a fourth hand-rolled copy of the
reservation formula; findings 7 and 8 are both symptoms of that. `on_modify`
should compute the prospective reservation by building the amended
`SubmitOrder` and calling `order_reservation` - one expression, as `held_for`
already does.

## 9. Equity short reservation double-counts held shares across resting sells

`order_reservation`, equity sell with a margin policy:

```rust
let uncovered = leaves - self.net_position(&submit.symbol).max(Decimal::ZERO);
```

and `validate_submit`'s short check:

```rust
let short = (self.net_position(&order.symbol) - order.quantity).min(ZERO).abs();
```

Neither subtracts the quantity already committed by OTHER resting sells. Holding
100 shares, two resting sells of 100 each both compute `uncovered = 0` and
`short = 0`, reserve nothing, and pass the cash-account short refusal - so a
cash equity account can end the run short 100 shares through an order path that
refuses shorting by name. The locate check (`borrowable`) is bypassed the same
way.

The quantity that matters is
`leaves - max(0, net_position - other_resting_sell_leaves)`, i.e. the same
"worst-case fill order" reasoning `projected_qty` already implements carefully
for position caps. That function exists and states the argument; the reservation
path does not use it.

Confidence: medium-high. The hunter has not checked whether an upstream layer
applies `projected_qty` to equity sells; nothing in this crate does.

## 10. Smaller and lower-confidence

- Group closing pass ignores `DropNextAccountUpdate`. `on_submit_group`
  unconditionally pushes an `AccountState` when the closing linkage produced
  anything, while every other snapshot site consults the arm. It also emits that
  snapshot IN ADDITION TO the per-member snapshots pass two already emitted, so
  one group can produce N+1 account states.
- `account_changed` omits `OrderExpired` and `OrderUpdated`. Today no reachable
  batch emits either without an accompanying fill or cancel, but the predicate
  claims to answer "did anything move the ledger" and an OUO shrink
  (`OrderUpdated`) plainly does. A latent gap of the same shape as the ones
  above.
- Liquidation id prefixes are half-guarded. `validate_submit` reserves `"LQ-"`,
  but `liquidate_all` mints `"RISK-..."`, which no rule reserves. A client can
  submit `RISK-MNQ-1` and collide with a risk-breach close.
- `on_trigger`'s `StopMarket` remainder rests with a stale `scanned_ns` and an
  unbumped `revision`. It rests `Inert` so nothing scans it today, but the
  frontier is left pointing at pre-trigger tape - the frontier family's shape,
  currently harmless only because of a property of a different field.
- `draw_key` collision surface. Documented as accepted, so not a finding, but
  worth restating: the trigger is a pure function of client-controlled fields,
  so a strategy can re-roll its queue position by cancel-and-resubmit under a
  new id. Fine for strategies, not fine if this venue is ever used to score
  anything adversarial.
- The group's atomicity guarantee is contingent on an external validator.
  `on_submit_group` never calls `validate_submit_group`; the
  duplicate-id-within-group and IOC/FOK-member rules live only in
  `mogwai-server/src/http.rs`. The dry pass cannot catch an intra-group
  duplicate on its own (no member is in `seen_client_order_ids` yet), so a
  caller reaching `process_with_market` directly - tests, benches, a future
  gateway - breaks the group open on pass two. Given how much documentation in
  this file is devoted to "no refusal may reach a submit from outside
  `dry_refusal`", having a whole class of refusals live in another crate is a
  structural weak point. `on_submit_group` should call the validator itself.

## Structural note

Six of the ten findings (1 aside: findings 2, 3, 6, 7, 8, and the
`account_changed` gap) share one cause: A RULE WITH ONE STATED HOME AND SEVERAL
HAND-ROLLED COPIES. The reap half of that is now closed - `Engine::close_out` is
the single home and every terminal path routes through it - and the reservation
and P and L halves are still open below. The reservation formula exists in `order_reservation`, and
again in `on_modify`'s futures branch, and again in its buy/sell branch, and
`margin_requirement` has a fourth. The P and L formula exists in
`InstrumentDef::unrealized` and again in three places in lib.rs. The "reap
children on non-filling terminal" rule existed at four of ten call sites. The
prose in this crate is unusually good at STATING each invariant - and in each
case the statement sits next to one implementation while the others drift.

Given pre-1.0, the hunter would spend the next change on collapsing those three,
not on patching the ten sites: a single
`close_order(pos, status, ts) -> Vec<ServerMessage>` funnel, a single
prospective-reservation entry point that `on_modify` and `validate_submit` both
call by constructing the candidate `SubmitOrder`, and deleting every hand-rolled
P and L expression in favour of `def.unrealized`. `reconcile_order_locked`
already proves the aggregate-cache half of this is worth doing; nothing plays
that role for the other two.

## 11. WITHDRAWN, and finding 4 with it: both rested on a misreading

Filed by the round-1 fix pass, refuted by the round-1 cold review, and settled
here by reading the code. `mogwai_protocol::validate_order_link` ALREADY refuses
both shapes, and has for longer than this hunt: a `Market` child with
"a Market order cannot be an order-list child...", and an `Ioc`/`Fok` child with
"an order-list child cannot be immediate-or-cancel...". It is called from
`validate_submit_order`, which `mogwai-server`'s `boundary_error` runs on every
inbound `SubmitOrder` and, through `validate_submit_group`, on every member of
every inbound `SubmitOrderGroup`. Those two frames are the whole client-facing
order-entry surface - the `POST /orders` carrier is gone - and `boundary_error`
additionally refuses a LINKED bare `SubmitOrder` outright, so the "per-leg route"
finding 4 said reached the engine unfiltered does not exist over the wire at all.
The pre-existing test `linkage_shapes_the_venue_cannot_honour_are_refused` was
asserting both refusals the whole time, in the same file the fix pass edited.

So: `docs/order-lists.md` was RIGHT before the round-1 edit and has been restored
to what it said. The `Engine::validate_submit` copy of the IOC rule has been
removed rather than kept as defence in depth, because a second home for a rule
that has one is the exact defect the structural note names, and the engine's real
exposure - a caller that reaches `process_with_market` without the protocol
validator - is finding 10's group-validation gap, closed by having
`on_submit_group` call `validate_submit_group` rather than by copying one arm of
it into the engine. A later round taking finding 10 should not re-derive any of
this.

Two process notes, since this cost a round. The fix pass self-rated finding 4 at
85 percent for not having read `validate_submit_order`, and that reservation was
the correct one: an "is this already refused" question is answered by reading the
validator, not by reading the path that would have needed it. And its regression
test asserted `reason.contains("order-list child cannot be immediate-or-cancel")`
while driving `process_with_market` directly - a substring the protocol crate's
own message also carries. The test has been deleted, and
`linkage_shapes_the_venue_cannot_honour_are_refused` now asserts the FULL refusal
string for both shapes plus the group route, so deleting either arm of
`validate_order_link` fails it by name.
