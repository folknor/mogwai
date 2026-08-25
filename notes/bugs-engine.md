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

Round 1 closed findings 1 through 5; all five reproduced and all five fixes are
bite-checked, two of them in the release lane as well because the group's
atomicity `debug_assert` is compiled out there and the obvious assertions were
vacuous without it. Findings 6 through 8 below are untouched and open.

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
