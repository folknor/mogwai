# Bug hunt: mogwai-engine

Hunter: Claude (Opus), read-only, 2026-08-25. Scope: the mogwai-engine crate -
order lifecycle (`orders.rs`), account state (`account.rs`), the divergence
seam (`divergence.rs`), `lib.rs` and the crate's own tests.

A hunt report is a work document, not a contract. Findings may be wrong; the
fix pass verifies.

Hunter's note on method: read all of the engine's production code (lib.rs
1-2635, orders.rs, account.rs, divergence.rs), the test doctrine, glossary and
north-star, and the protocol validators the findings hang on
(`validate_submit_group`, `MAX_LINKED_ORDERS` / `MAX_GROUP_ORDERS`). Confirmed
by grep that `RESERVED_ID_PREFIXES` has exactly one enforcement site and it is
`apply_divergences`-gated, and that no existing test pairs `DuplicateNextFill`
with a `SubmitOrderGroup`.

## 1. `DuplicateNextFill` double-counts a group member's fill, and an `Ouo` sibling is shrunk by twice the real quantity

`orders.rs`, `on_submit_group`, around 246-259. High confidence, serious.

`on_submit_group` derives each member's filled quantity by summing the emitted
`OrderFilled` events:

```rust
let member_filled: Decimal = events.iter().filter_map(|event| match event {
    VenueMessage::OrderFilled(fill) if fill.client_order_id == order.client_order_id => Some(fill.last_qty),
    _ => None,
}).sum();
```

`commit_fill` emits the fill twice when `DuplicateNextFill` is armed
(`out.push(fill.clone()); out.push(fill)`), byte-identical, `last_qty`
included. So `member_filled == 2 * last_qty`, and the closing pass calls
`apply_linkage_after_fill(&order, member_filled, ts)`.

Concrete sequence: arm `DuplicateNextFill`; submit a two-leg `Ouo` group
(entry 100, stop 100). Entry fills 100 and emits two identical fills. The
closing pass shrinks the stop by 200, so `new_leaves = 100 - 200 <= 0` and the
stop is cancelled outright, leaving the position naked. That is verbatim the
hazard the function's own doc says the group frame exists to close ("a two-leg
bracket sent stop-first would have its stop driven to zero leaves and
cancelled outright... leaving the position naked"), reintroduced through a
different door.

The two non-group paths are immune because they pass the scalar `last_qty`
(`on_submit_from` line 872, `apply_scans_on_clock` line 1308). Only the group
path reconstructs the quantity from the wire event stream - which is precisely
the stream the venue is designed to corrupt. Structurally: the divergence
layer's whole contract is that the truth store keeps the truth and the wire
carries the lie, and this is engine logic reading the lie. The fix direction is
to have `on_submit_from` return the booked quantity rather than have the caller
infer it from events; more broadly, no engine control flow should ever be
derived from a `Vec<VenueMessage>` that divergences have already touched.

## 2. Two refusals are invisible to the group's dry pass, both because `apply_divergences` is overloaded

`orders.rs`, `dry_refusal` 322, `validate_submit` 2289 and 2452. High
confidence.

`dry_refusal` calls `validate_submit(&candidate, ts, /* apply_divergences */
false, group)`. Pass two calls it with `true`. Two rules read that flag.

**2a. The reserved-prefix refusal.** `validate_submit` refuses `LQ-` / `RISK-`
prefixed ids only when `apply_divergences` is true. Nothing else in the
workspace enforces it: `RESERVED_ID_PREFIXES` has exactly one check site, and
`validate_submit_group` in the protocol does not know about it. So a
`SubmitOrderGroup` whose second member is `client_order_id: "LQ-anything"`
passes pass one whole, and pass two rejects it with its sibling already
accepted and possibly filled. `report_group_member_refusal` then re-runs the
dry question, which passes, so it hits `debug_assert!(false, "ATOMICITY
DEFECT...")` - a panic in every dev and test lane, and a silently broken-open
group in release. This is exactly the defect family the `dry_refusal` doc
claims to have closed ("no refusal may reach a submit from outside this
function"); the comment is wider than the function.

**2b. The fee surcharge.** `maximum_commission(order, qty, price, ts,
apply_divergences)` applies `fee_surcharge_multiplier_for` only when the flag
is true. So with an armed `FeeSurcharge` in its window, the dry pass computes a
smaller required commission than pass two. A group member sitting between the
two thresholds is admitted dry and refused real - same atomicity break, same
`debug_assert!(false)` panic.

Root cause: one boolean means three different things - "this is a consumer
order, not venue-originated" (the arm-spending rule), "count the surcharge",
and "enforce the reserved prefix". Those should be separate. The last two are
properties of the order and the venue's state, not of whose act it is. Pre-1.0
the hunter would split it: a `venue_originated: bool` for arm-spending, with
the prefix rule and the surcharge unconditional in `validate_submit` and the
venue's own orders exempted by construction - that exemption needs to be stated
as `is_venue_order`, not as `apply_divergences`.

## 3. `on_modify`'s price-amend guard is narrower than `validate_submit`'s, so a price can be pasted onto a market-on-trigger remainder

`orders.rs` 2883-2899. High confidence.

`validate_submit` refuses a price on `StopMarket | MarketIfTouched |
TrailingStopMarket` ("a market-on-trigger order must not carry a price").
`on_modify` refuses a price amend on `Market | StopMarket` only. Two homes for
one rule, drifted - the vacuous-gate family, sub-shape "a comment describing a
gate wider than the gate": the comment above the guard says "Neither kind of
market order has a live limit price... giving that remainder a price must not
turn it into a scannable limit", and then the guard misses two of the types it
is talking about.

Sequence: submit a `MarketIfTouched` (or `TrailingStopMarket`); it triggers; an
armed `PartialFillNext` leaves a remainder, which `on_trigger` rests as
`Resting::Inert` (line 1860). Send `ModifyOrder { price: Some(p) }`. The guard
passes. `effective_price` becomes `p`. At line 3250, `!matches!(order.resting,
Conditional | Held)` is true for `Inert`, so the remainder is promoted to
`Resting::Limit { fill_trigger_px: draw_trigger(...) }`, takes a hold priced at
`p`, and becomes scannable. `pending_scans` now offers it,
`apply_scans_on_clock` reads `submit.price.expect(...)` - which is now `Some` -
and fills it as a maker at a price the consumer invented after the fact, on an
order type the venue refuses a price for at the front door. Exactly the outcome
the comment forbids.

## 4. `on_modify` never re-derives a `TrailingStopLimit`'s limit from an amended trigger

`orders.rs` 3266-3273. Medium-high confidence.

`derive_trailing_limit` is the stated single home of "a trailing stop limit's
limit is derived, never stated", and it runs at submit and in
`ratchet_trailing_stops`. `on_modify` runs neither. A `ModifyOrder {
trigger_price: Some(t2) }` on a resting `TrailingStopLimit` writes the new
trigger and leaves `submit.price` at the limit derived from the old trigger.
Consequences: the hold and `effective_price` are taken against a stale limit;
`OrderUpdated` reports the new trigger beside the stale price; and if the amend
moves the trigger far enough, the limit ends up on the wrong side of it, so
`on_trigger`'s stop-limit arm promotes the order to a limit the consumer never
agreed to.

Related, probably the same fix: `on_modify` also does not refuse a price amend
on `TrailingStopLimit`, though submit refuses one on exactly the grounds that
the limit is derived - so a consumer can state the limit through modify that
submit will not let it state directly.

## 5. `on_modify` skips the round-lot rule that `validate_submit` enforces

`orders.rs` 2962 vs 2339. Medium confidence.

The comment at 2937 claims the mirror explicitly: "Submit enforces the
instrument's price/size grid; modify must too, or a resting order can drift to
an off-grid price/quantity that a fresh submit would have rejected outright
(and that off-grid state then goes out on the wire via `OrderUpdated`)."
`on_modify` checks `size_increment` but not `instrument.class.lot_size()`. An
equity with a 100-share lot accepts a submit of 200 and then a modify to 250,
which goes out on `OrderUpdated`. Small, but it is the exact drift the comment
says it is preventing, so the comment is currently a defect by the doctrine's
own reading.

## 6. Nondeterministic liquidation order and ids from `HashMap` iteration

`lib.rs` 1289-1298, 1380-1393, 1475-1490. Medium-high confidence,
contract-level.

`apply_margin_breaches` builds its `liquidate` list by iterating
`&self.account.positions`, a `HashMap` with `RandomState`; `liquidate_all` and
`retire_off_river` do the same. The order decides which position gets
`liquidation_seq` 1 vs 2, so the minted `LQ-{symbol}-{n}` / `RISK-{symbol}-{n}`
client order ids, the venue order ids assigned behind them, the trade ids, and
the order of the emitted `VenueMessage`s all vary run to run on the same seed,
config and binary. That contradicts the standing correctness contract
("Determinism per binary: the same seed, config and binary produce the same
tape and the same measurement, always"). It only bites with more than one
position under one symbol (hedging) or more than one position in the book, but
`liquidate_all` and `retire_off_river` are multi-position by construction. The
rest of the crate is careful about this - `positions()`, `margin_requirement()`,
`instrument_defs()`, `pending_scans()`, `order_status_snapshot()` all sort -
these three are the gap. Fix: collect and sort by `(symbol, position_id)`
before minting.

Same family, weaker: `valuation_at` (lib.rs 924-929) picks the pricing
instrument with `self.instruments.iter().find_map(...)`. If two registered
instruments share a base currency and settle in the same currency - which the
open instrument set permits, since a symbol arrives and is served - which one
marks the balance is nondeterministic, and a risk-policy breach decision rides
on it. Should be a deterministic choice (sorted by symbol, or refuse the
ambiguity loudly).

## 7. `margin_requirement`'s stated reconciliation can be false for a price-less margin-equity sell

`lib.rs` 1158 vs `account.rs` 262. Low-medium confidence, narrow.

`margin_equity_sell_holds`, the hold cache, skips an order with neither `price`
nor `trigger_price` (`let Some(price) = ... else { continue }`), so it
contributes nothing to `worst`. `margin_equity_sell_hold_with_pending` - which
`margin_requirement` calls for the equity row - computes `worst` via
`worst_case_leaves`, which counts every non-reduce-only, non-held sell
regardless of price. If such an order can rest (an inert remainder that lost
its price, or a future path that rests one), the reported `initial` margin row
exceeds the `locked` that `held_balances` folds, and the function's long "what
reconciles, exactly" doc becomes false in a case it does not carve out. The
hunter could not construct a live path that produces a price-less resting
equity sell today, so this may be latent rather than reachable; reported
because the two folds are two implementations of one quantity with no shared
fixture, which is the doctrine's named hazard.

## 8. Smaller observations

- `free_balance` drops its `clipped` flag (account.rs 675-711) while
  `snapshot` and `held_balances` warn on theirs. A saturation that only
  manifests on the funds path is silent, and the funds path is the one that
  refuses orders. Given the file's stated saturate-and-warn philosophy, this is
  an inconsistency worth closing.
- `warn_saturated` keys collide across namespaces. Its doc says "`key` is a
  symbol on the position path, a currency on the balance paths", but they share
  one `HashSet<String>`. `apply_fill`'s equity and spot branches warn with
  `&fill.symbol` while `snapshot` warns with a currency. A symbol whose name
  equals a currency name - the instrument set is open, nothing forbids `USD` -
  suppresses the other's warning permanently.
- `clear_armed` is dead outside tests and its doc says so at length. Fine as
  documented, but it means the `MAX_ARMED_DIVERGENCES` leak it names has no
  live escape hatch: the cap's shed-oldest is the only real mechanism.
- `on_cancel`'s divergence gate does a linear scan (`self.open.iter().find(...)`,
  orders.rs 2752) where `self.open.position(&client_order_id)` is the O(1)
  index that exists for exactly this. Trivial, but it is the only place in the
  file that walks the book to answer a by-id question.
- `snapshot`'s currency union is quadratic (`currencies.contains(currency)`
  over a `Vec`, account.rs 604). Trivial at test scale; a `BTreeSet` states the
  intent - sorted, unique - in one type.
- `MAX_GROUP_ORDERS == MAX_LINKED_ORDERS + 1` is load-bearing and unasserted.
  The hunter checked whether `validate_link`'s child cap can break a group open
  (dry pass counts 0 children, pass two counts them accruing) and it cannot -
  but only because a group's parent must be a group member, so it always starts
  with zero children, and 9 members means at most 8 children, exactly the cap.
  If either constant moves independently, pass one and pass two disagree and
  the group breaks open. That is the "two constants encoding one quantity"
  shape from the doctrine; it deserves an explicit identity assertion rather
  than an argument someone has to reconstruct.

Nothing found in `divergence.rs` itself is wrong: `take_armed`'s
first-applicable scan and `arm`'s exhaustive match are both sound, and the
exhaustive match is genuinely doing the work its comment claims.
