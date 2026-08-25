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
vacuous without it.

Round 2 closed findings 6, 7 and 8, and the document is now empty of open
findings. Finding 6 reproduced on all four of its paths and is fixed by sorting.
Finding 7 was adjudicated rather than dismissed: the two folds really did
disagree - a price-less resting margin-equity sell was counted by
`worst_case_leaves` and skipped by `margin_equity_sell_holds`, and a witness
built by stripping the price off a rested order shows the `initial` margin row
reporting 10,000 against a `locked` of 2,500 - but no wire path can rest such an
order, so it was latent. It is closed by making the two folds count the same
orders rather than by an argument that the input cannot arrive. Finding 8's four
actionable items landed; its two observations about `clear_armed` and
`MAX_GROUP_ORDERS` were refused with reasons, and those refusals stand.
