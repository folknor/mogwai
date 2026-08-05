# Bug hunt: mogwai-engine

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

## 1. `mark_px` defaults to zero, so every fresh or flipped futures position reports a catastrophic phantom unrealized P&L - and can trigger a spurious margin breach

`crates/mogwai-engine/src/account.rs`, `next_position`.

Three of the four branches that construct a `PositionState` set
`mark_px: Decimal::ZERO`:

- new position (`current.qty == 0`) -> `mark_px: ZERO`
- flat close -> `ZERO` (harmless, entry is removed)
- flip (`delta_abs > current_abs`) -> `ZERO`, discarding an already-marked
  position's mark

Only `mark()` and `settle()` ever write a real `mark_px` (confirmed by grep -
nothing else in the workspace touches it). So between a fill and the next mark
tick:

```rust
unrealized_pnl = (mark_px - avg_px) * qty * multiplier
               = (0 - 20_000) * 1 * 2 = -40_000
```

Consequences:

- Wire lie. `Engine::positions()` is called by `snapshot()`, which is emitted on
  the same `on_submit` that created the position. The very `AccountState`
  acknowledging a futures fill reports a fabricated loss of the full notional. A
  nautilus consumer sees it.
- Spurious breach. `apply_margin_breaches` computes
  `breached = total + unrealized < maintenance` where `unrealized` folds
  `unrealized_pnl` over every symbol settling in that currency - including symbols
  not in this `marks` batch and therefore still unmarked. So a `mark()` call for
  symbol A can liquidate or refuse symbol B purely because B has never been marked.
  Under `BreachAction::Liquidate` this originates a real venue order against a real
  position on a fabricated number.
- The flip case is worse because it destroys correct state: a long that flips short
  zeroes its mark even though the position has been marked for hours.

Fix: `mark_px` should initialize to the fill price (`px`) on the new-position and
flip branches, and the flip branch should carry `current.mark_px` forward.
Confidence: high. This is the single clearest silent-wrongness in the account
arithmetic.

## 2. `account.rs` documents "commission is always zero on this venue" - the fee engine has since been wired, and the funds checks were never updated

`crates/mogwai-engine/src/account.rs`, `apply_fill`:

> "`fill.commission` is always `Decimal::ZERO` on this venue: mogwai models
> execution DIVERGENCES (partials, rejects, delays, drops), not fees, so no
> commission source exists and none is wired."

This is false. `Engine::set_fee_schedule` exists, `mogwai-server/src/run.rs` calls
it from config, `FeeRate::{BasisPoints, PerContract}` are implemented, `commit_fill`
computes a real commission, and `Divergence::FeeSurcharge` multiplies it. The
default config even ships a `PerContract` schedule.

The load-bearing consequence is that no funds check accounts for commission:

- `validate_submit` requires `notional` (spot buy) or `initial_per_contract * qty`
  (futures).
- `validate_fill_funds` requires `qty * fill_px` or the same margin - never
  `+ commission`.
- `apply_fill` then books `spend = notional + commission`.

So a spot buy that exactly exhausts the free balance passes both gates and drives
the quote balance negative by the commission. The `enforce_funds` doc comment on
`Engine` says exactly why that matters: "the ledger goes negative and a nautilus
cash-account consumer refuses every snapshot after it, silently desyncing." The
venue can enter that state by design, on a funded account, on a legal order.

Related, smaller: `commit_fill`'s bps path does
`def.notional(...).unwrap_or(Decimal::MAX).saturating_mul(rate).checked_div(10_000).unwrap_or(Decimal::MAX)`
- an overflowing notional silently books a commission of `Decimal::MAX`, with none
of the `warn_saturated` discipline the rest of the ledger is careful about.

## 3. Venue-originated liquidation orders consume client-armed divergences

`crates/mogwai-engine/src/lib.rs`, `apply_margin_breaches` -> `on_submit`.

A liquidation submit goes through the ordinary `on_submit`, which unconditionally
scans the armed queue:

- `take_armed(RejectNextSubmit)` fires on the liquidation order. The liquidation is
  rejected, the position is not closed, the breach does not clear, and the client's
  armed reject is spent on an order it never sent. The next client submit goes
  through cleanly and the scenario's whole premise is broken.
- `commit_fill` consumes `DuplicateNextFill`; the tail of `on_submit` consumes
  `DropNextAccountUpdate`. Both are untargeted and both get stolen by the venue's
  own order.

`DropNextAccountUpdate` is doubly pointless here: `mark()`/`settle()` do
`events.retain(|e| !matches!(AccountState))` and then push a snapshot
unconditionally, so the arm is consumed and has no effect at all.

Venue-originated orders should bypass the divergence seam entirely (a flag on the
submit path, or a separate internal entry point). Confidence: high on the reading;
no scenario was run to observe it.

## 4. `post_only` is unenforceable through `on_modify`

`on_submit` rejects a `post_only` order that is marketable on arrival, and
`on_trigger` rejects a `post_only` stop-limit that would take liquidity. `on_modify`
has no marketability gate. A client submits a resting post-only limit far from the
market (accepted), then amends the price through the market. The amend redraws the
trigger, sets `scanned_ns = ts`, and the order fills on the next sweep as
`LiquiditySide::Maker` - at maker fees, from a price the venue would have refused at
submit.

This is a straightforward invariant claim the code fails to hold. It is also
economically material now that fees are wired: taker-to-maker is a fee arbitrage the
client controls.

## 5. A price amend converts a resting market remainder into a live limit

`on_modify`, the price branch:

```rust
if !matches!(order.resting, Resting::Conditional { .. }) {
    order.resting = Resting::Limit { fill_trigger_px: draw_trigger(...) };
    order.scanned_ns = ts;
}
```

`Resting::Inert` is not `Conditional`, so it falls into this arm. Reachable path: a
`Market`/GTC order cut short by an armed `PartialFillNext` leaves an `Inert`
remainder in `self.open` (documented at `pending_scans`: "a market remainder ...
rests, is never scanned, and ends only on a client cancel"). A
`ModifyOrder { price }` then makes it a scannable limit while `submit.order_type`
stays `Market`.

`on_modify` explicitly guards the analogous case for `StopMarket` ("StopMarket order
must not carry a price") but not for `Market`. The `Inert` arm should be excluded
from the promotion, or a plain-`Market` price amend should be rejected the same way.

## 6. `held_for` and `locked_balances` disagree for any non-future symbol carrying a margin policy

`orders.rs::held_for` checks `self.margin.get(symbol)` before it looks at the
instrument class:

```rust
if let Some(policy) = self.margin.get(&order.symbol) {
    return policy.initial_per_contract.saturating_mul(leaves);
}
match order.side { Side::Buy => leaves * price, Side::Sell => leaves }
```

`account.rs::locked_balances` gates the open-order loop on
`instrument.class.is_future()` first, and only then consults the margin map. So for
a spot symbol with a margin policy configured, the actual reservation is
`leaves * price` while `held_for` hands `validate_fill_funds` a margin-shaped
add-back. The funds check then compares against a number that is not the hold it is
trying to cancel out - under- or over-rejecting depending on the ratio.

(The positions loop in `locked_balances` has the opposite asymmetry: it reserves
maintenance margin for a spot position too, ungated on class.)

Whether this is currently reachable depends on whether the server config rejects
`margin` on a non-futures instrument; not chased down. Either way the two functions
should derive the hold from one shared helper rather than reimplementing the branch
order twice.

## 7. `draw_key` hashes `Decimal::serialize()`, which is scale-sensitive - the RNG is a function of the client's decimal formatting

`orders.rs::draw_key` feeds `price.serialize()` into FNV-1a. `rust_decimal`'s
serialized form carries the scale, so `100` and `100.00` hash differently. Meanwhile
`on_increment` uses `checked_div(...).fract() == 0`, which is scale-insensitive, so
both forms validate identically.

Two economically identical orders differing only in trailing zeros therefore draw
different fill triggers and different market slippage. For a project with a
`TAPE_PROTOCOL_VERSION` discipline and an explicit determinism-sensitive-replay
posture, "the fill band depends on how the client wrote its JSON" is a real
replay-fidelity hazard: a client library that normalizes scale differently across
versions silently changes every fill in the run.

Fix: normalize before hashing (`price.normalize().serialize()`, or hash the mantissa
at a fixed scale derived from the instrument's `price_increment`).

## 8. Structural: the funds path is O(open orders) per fill decision, and every command does three full book passes

Not a micro-optimization - this is the crate's load ceiling and it is
architectural.

`free_balance` calls `locked_balances()`, which walks every position and every open
order, allocating a `String` per entry (`settlement_currency().to_owned()`,
`currency.clone()`, `base_currency().map(str::to_owned)`) and building a fresh
`HashMap` - and then throws all of it away to read one currency.

- `apply_scans` calls `validate_fill_funds` (-> `free_balance` -> full walk) once
  per scan result. A batch of N results against M open orders is O(N*M) with ~2*N*M
  String allocations.
- `snapshot()` then does it a fourth time, plus `positions()` (clone and sort the
  whole map) plus `margin_requirement()` (two more full passes with its own
  HashMap).
- `on_cancel`/`on_modify`/`apply_scans` each locate an order by linear `position()`
  scan over `Vec<OpenOrder>`.

The right shape is an incrementally-maintained `locked: HashMap<Currency, Decimal>`
updated on the four events that move it (order rests, order leaves the book,
leaves_qty changes, price changes), with the current full walk kept only as a
`debug_assert` reconciliation. That turns per-fill funds checking into a hash lookup
and removes the allocation storm. `open` should be an
`IndexMap`/`HashMap<ClientOrderId, OpenOrder>` so lookups stop being linear. Given
pre-1.0, do both together rather than patch around them.

## 9. Smaller items

- `next_id` uses `self.seq += 1` - a plain `+=` on `u64` while every other `seq`
  bump in the crate is `saturating_add`. Panics in debug on overflow. Cosmetic in
  practice, but it is an inconsistency in a crate that otherwise audits every
  arithmetic op.
- `seq` is one counter shared across four namespaces: venue ids (`V-n`), trade ids
  (`T-n`), hedging position ids (`symbol-n`), and liquidation client order ids
  (`LQ-symbol-n`). Nothing collides today, but a client that submits a
  `client_order_id` of literally `LQ-MNQZ5-7` can pre-empt a future liquidation
  order's id (which would then be rejected as a duplicate by `validate_submit`,
  leaving the position un-liquidated). Cheap adversarial input, cheap fix: separate
  counters, or a reserved prefix rejected at the door.
- `on_cancel` emits `AccountState` unconditionally and never consults
  `DropNextAccountUpdate`, while `on_submit` and `apply_scans` both do. A cancel
  frees a reservation, so it does move `locked` - the divergence arguably applies
  and is inconsistently skipped.
- Reduce-only under `OmsType::Hedging`: `on_submit` auto-assigns a fresh
  `position_id` to any submit lacking one before validation. A reduce-only order
  submitted without a position id therefore gets a brand-new key, `reduce_only_cap`
  returns 0, and the order is instantly canceled. That may be intended, but it is
  not documented anywhere found, and it is a sharp edge for a client that reasonably
  expects "reduce whatever I have."
- `fee_surcharge_multiplier` expires lazily on `&mut self` inside `commit_fill`.
  With no fills, the window never clears; and it assumes `ts` is monotonic - a
  replay that rewinds `ts` after the window has been cleared cannot re-arm it. Given
  the determinism posture, expiry should be a pure function of `ts` with no
  mutation.
- `apply_scans` re-draws a trigger on a zero-quantity fill. When `plan_fill` floors a
  `PartialFillNext` below one size increment, `last_qty == 0`, no fill is emitted,
  but the `else if new_leaves > 0` arm still bumps `band_draw` and redraws
  `fill_trigger_px`. The order's queue position is re-rolled for free by a divergence
  that produced nothing. Small, but it is the band leaking in the direction the sweep
  comment says it must not.

## Confidence summary

Findings 1, 2, 3, 4, 5 the hunter is confident are real defects from the code as
written; 1 and 2 are the ones to fix first. Finding 6 is a definite inconsistency
whose reachability depends on server config validation not verified. Finding 7 rests
on `rust_decimal`'s `serialize` being scale-preserving, which it is. Finding 8 is a
design judgment, not a bug - but it is the item with the largest payoff.
