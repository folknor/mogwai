# Bug hunt: mogwai-engine

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Round 1 landed findings 1 through 6, each with a regression test verified to
fail against the reverted production change. Finding 6 was closed by making
`held_for` and `locked_balances` derive from one `Engine::order_reservation`,
which consults the instrument class before the margin map, rather than by
resting on the server-config guard that rejects a margin table on a spot
instrument (that guard is real, but `Engine::set_margin_policy` is public and
bypasses it). Two further defects found while reviewing that work - a venue
liquidation paying and expiring a client-armed `FeeSurcharge`, and an amend
whose funded-account check omitted commission - were fixed in the same pass.
Findings 7 through 9 below are untouched.

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

Findings 1 through 6 were confirmed and are landed. Finding 7 rests
on `rust_decimal`'s `serialize` being scale-preserving, which it is. Finding 8 is a
design judgment, not a bug - but it is the item with the largest payoff.
