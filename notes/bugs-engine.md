# Bug hunt: mogwai-engine

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-engine`: order state machines, the matching loop, fills and
partial fills, order groups and their linkage, account and position state, and
the divergence-injection seam.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter read all four source files (`orders.rs`, `account.rs`, `lib.rs`
non-test region, `divergence.rs`) plus the protocol-side helpers the engine
leans on (`InstrumentDef::unrealized`, `validate_submit_group`,
`InstrumentClass`). No files modified.

## 1. Venue-originated cancels consume client-armed divergences (serious)

`Engine::liquidate_all`, `retire_off_river` and `cancel_unreadable_orders`
(lib.rs) all cancel through `self.on_cancel(...)`. `on_cancel` consumes two
client arms:

- `RejectNextCancel` - if armed, the FIRST order the venue tries to flatten
  comes back `OrderCancelRejected` and STAYS RESTING. A risk-breach
  `liquidate_all` then proceeds to close the positions while leaving a live
  resting order behind - precisely the hazard its own doc comment ("RESTING
  ORDERS GO FIRST... or the account could re-open the position it was just
  closed out of") says it exists to prevent.
- `DropNextAccountUpdate` - spent on a venue action the scenario author never
  aimed it at.

Contrast the care taken on the fill side: every venue-originated submit
(`close_at_mark`, `apply_margin_breaches`, `liquidate_all`) passes
`apply_divergences: false` explicitly, and `validate_fill_funds` even documents
"a venue-originated order pays no client-armed FeeSurcharge, so checking it
against one could cancel a forced liquidation." The cancel path has no
equivalent. `on_cancel` needs an `apply_divergences` parameter (or a
`cancel_silently`-style internal twin) exactly as `on_submit_from` has.

Confidence: high. This is a straight asymmetry in the same invariant the crate
already states.

## 2. Terminal conditional paths orphan their held children (frontier/guard family)

`reap_children_of` is called from exactly four sites: `expire_orders`, the OCO
cancel, the OUO zero-leaves cancel, and `on_cancel`. It is NOT called from:

- `on_trigger`'s reduce-only cap-zero cancel (orders.rs ~1545)
- `on_trigger`'s post-only rejection (~1663)
- `on_trigger`'s `cancel_triggered` funds-check cancel
- `on_trigger`'s cap-clamped remainder cancel (both arms)
- `apply_scans_on_clock`'s cap-zero cancel and its resting funds-check cancel
- `cancel_open_order_silently`

Every one of these takes an order terminal WITHOUT it having filled. Any
`Resting::Held` child of that order is now waiting for a release that can never
come: it is never scanned (`pending_scans` skips `Held`), holds no reservation,
is not covered by `expire_orders` unless it happens to be GTD or Day, and only a
client cancel ends it. `on_cancel`'s own comment states the rule - "a held child
left behind would rest for the life of the run holding a promise nothing can
keep" - and six other terminal paths violate it.

The structural fix is not six more call sites. The rule is "an order leaving the
book without having filled reaps its children", and it belongs inside `take_open`
or `record_closed` (or a single
`close_order(pos, status, ts) -> Vec<ServerMessage>` that every terminal path
routes through). Right now the invariant is maintained by remembering, at ten
sites, which is how it got missed at six of them.

Confidence: high on the mechanism; the hunter did not construct a failing test.

## 3. `on_modify` promotes a `Held` child into a live limit

orders.rs, the price-amend block:

```rust
if !matches!(order.resting, Resting::Conditional { .. }) {
    order.resting = Resting::Limit { fill_trigger_px: draw_trigger(...) };
    order.scanned_ns = ts;
}
```

`Resting::Held` is not `Conditional`, so a price amend on an order-list child
that is still waiting for its parent TURNS IT INTO A SCANNABLE LIVE LIMIT, takes
its reservation via `refresh_open_reservation` (the `Held` short-circuit in
`order_reservation_entry` no longer applies), and lets it fill before its parent
ever executes. That is one-triggers-the-other defeated by a client amend - the
exact hazard `Resting::Held` exists to prevent.

The condition wanted is `matches!(order.resting, Resting::Limit { .. })`, or the
amend of a `Held` child should mutate `submit.price` only and leave the resting
state alone.

Related, cosmetic: a `trigger_price` amend on a held CONDITIONAL child is
refused with "order has already triggered", which is false - it has not
triggered, it has not been released. Wrong reason on the wire.

Confidence: high.

## 4. An IOC or FOK order with a parent rests forever

In `on_submit_from`, the held-child branch is placed AHEAD of every
marketability, TIF and post-only decision (deliberately, per its comment). But
it is also ahead of the FOK rejection and the IOC cancel. So a standalone
`SubmitOrder` with `time_in_force: Ioc` (or `Fok`) and a
`link.parent_order_id` naming an unfilled parent is accepted and rests `Held`;
`release_child` later promotes it to `Resting::Limit`, where it behaves as a
GTC. `expire_orders` handles only `Gtd` and `Day`, so nothing ever ends it.

Now-or-never orders resting indefinitely is a contract break. Note the wire
layer already knows this is nonsense - `validate_submit_group` refuses an
IOC/FOK GROUP MEMBER with "a now-or-never order's fate is not decided by
admission" - but the per-leg route (`ClientMessage::SubmitOrder` with a link)
reaches the engine unfiltered, and `validate_submit` refuses IOC/FOK only for
CONDITIONALS. The rule belongs in `validate_submit`: a child (any order carrying
`link.parent_order_id`) may not be IOC or FOK.

Confidence: high on reachability, given `validate_submit_order` does not itself
refuse this (the hunter checked the group validator; it did not exhaustively
read `validate_submit_order`, so treat the single-order path as roughly 85
percent).

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
HAND-ROLLED COPIES. The reservation formula exists in `order_reservation`, and
again in `on_modify`'s futures branch, and again in its buy/sell branch, and
`margin_requirement` has a fourth. The P and L formula exists in
`InstrumentDef::unrealized` and again in three places in lib.rs. The "reap
children on non-filling terminal" rule exists at four of ten call sites. The
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
