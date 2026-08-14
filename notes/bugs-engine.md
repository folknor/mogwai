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
Round 2 landed finding 7 and every bullet of finding 9. Finding 7 forced a
`TAPE_PROTOCOL_VERSION` bump from 12 to 13, so the protocol-12b mechanism
landing now takes 14. Reviewing that pass turned up three more defects, fixed
with it: a zero-quantity sweep pulled its scan frontier forward to the pass
time and could retire a span a truncated drain budget never walked; the
`DropNextAccountUpdate` doc comment and `docs/havoc.md` both defined the arm as
fill-only when the engine had already been spending it on any order transition
that moved the ledger; and the version bump left five durable statements of the
old number and the old 13 reservation standing.

Finding 8 below is the only finding left for round 3.

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

## Confidence summary

Findings 1 through 6 were confirmed and are landed. Finding 7 rests
on `rust_decimal`'s `serialize` being scale-preserving, which it is. Finding 8 is a
design judgment, not a bug - but it is the item with the largest payoff.
