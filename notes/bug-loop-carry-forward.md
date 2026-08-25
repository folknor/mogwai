# Bug-loop carry-forward

State the bug-hunt loop's agents cannot see, because each one arrives with only
its own round. Every brief carries the relevant slice forward. Current as of the
`notes/bugs-engine.md` arc, round 2, which closed that document. What follows is
also the handover to the close pass.

## Machinery agents may build on and must not break

- **Group closing linkage uses the booked fill quantity**, never quantities
  reconstructed from the emitted `VenueMessage` stream. `on_submit_from` returns
  the booked quantity for this reason. The general invariant, now written into
  `reference/architecture.md`: no engine control flow may be derived from a
  `Vec<VenueMessage>` that divergences have already touched.
- The two surviving production readers of that vector are audited and safe:
  `orders.rs` `account_changed` asks presence rather than magnitude, and
  `on_submit_group`'s pass-two refusal scan runs after `RejectNextSubmit` is
  consumed group-wide.
- The group dry pass now sees reserved id prefixes and active fee surcharges.
- `on_modify` enforces the round-lot rule, refuses price amendments on
  market-on-trigger and trailing-stop-market remainders, and rederives a
  `TrailingStopLimit`'s limit from an amended trigger.
- `on_modify`'s `OrderUpdated` frame publishes `order.submit.price`. It must not
  become `price.or(derived_price)`: that publishes a null price on every
  quantity-only amend of a limit order.
- **Venue-originated liquidations are minted in sorted order.**
  `apply_margin_breaches`, `liquidate_all` and `retire_off_river` each sort by
  `(symbol, position_id)` before minting, because `liquidation_seq`, the `LQ-` /
  `RISK-` ids, the venue and trade ids behind them and the event order all
  follow from it, and the positions live in a `HashMap`. Written into
  `reference/architecture.md`. Any new venue-originated batch owes the same
  sort.
- **Ambiguous balance valuation is a `min_by` over the symbol**, not a
  `find_map` over `self.instruments`. Documented in `reference/architecture.md`
  as a stable tie-breaker, not a claim the pairs share a market.
- **The two margin-equity sell folds count the same orders.**
  `margin_equity_sell_holds` (the `locked` side) and `worst_case_leaves` via
  `margin_equity_sell_hold_with_pending` (the `initial` margin row) are two
  implementations of one quantity with no shared fixture - the doctrine's named
  hazard. A price-less sell now contributes its quantity to both and a price to
  neither. Do not reintroduce a `continue` that drops it from one side.
- **Saturation warnings are keyed by `account::SaturationKey`**, a namespaced
  enum, because a symbol and a currency may legally share a name on an open
  instrument set. `Warned::saturated` is a `RefCell` so `free_balance`, which
  takes `&self`, can warn on its own clipping.

## Hazards this arc has already paid for

- Group admission atomicity is backed by a `debug_assert`, so a group regression
  test is structurally prone to passing vacuously in release. Assert the
  whole-group rejection wording, not the rejection count.
- Twelve commits running, the expensive defect has been in the half nobody cold
  reviews. The round-1 wire regression sat in the fix pass's own production diff
  and passed both the cold review and a green 1419-test gate.
- A determinism test over a two-entry `HashMap` is a coin flip against its own
  defect, not a test. Round 2's four ordering tests all reverted green about
  half the time; the fix pass read that as "failed nondeterministically" and
  moved on. The witness has to be derived: rebuild the engine until the map
  hands out the non-lexical key, then exercise the path. `mogwai-engine`'s
  `engine_iterating_non_lexically` is the worked example.
- And the predicate must name the map the path under test actually walks.
  Pinning `account.positions` left `apply_margin_breaches` a coin flip anyway,
  because that function takes its order from `self.margin.keys()` and only the
  later sort makes the output lexical. One helper, two different predicates.
- The cold review can be right about a discrepancy and wrong about its
  reachability at the same time. Round 2's finding 7 was a genuine disagreement
  between two folds, provable with a hand-built witness, and unreachable through
  any wire path. Neither "it reproduces" nor "it does not reproduce" was the
  answer; closing it by making the two folds agree by construction was.

## Decisions already ruled on, not to be relitigated

- Splitting the overloaded `apply_divergences` flag is the durable fix for the
  dry-pass blindness. It is recorded as owed in `reference/architecture.md`, and
  the local guards that landed in round 1 are the interim.
- `clear_armed` stays as it is. Shed-oldest is the intended live bound on the
  armed queue and explicit clearing is test machinery; the absence of a live
  escape hatch is the design, not a gap.
- `MAX_GROUP_ORDERS` stays unasserted against `MAX_LINKED_ORDERS`. It is already
  defined as `MAX_LINKED_ORDERS + 1`, so the identity is in the definition and a
  second assertion of it would restate rather than gate.
- The engine's execution paths are not the tape generation path.
  `TAPE_PROTOCOL_VERSION` was not bumped in either round of this arc, matching
  the round-1 precedent, which moved fill and linkage behaviour far more than
  round 2 did. `mogwai-data`, `analysis/` and the fingerprint were untouched
  throughout.

## The close pass, and how the arc ended

The close pass reviewed the whole `d58ff1e..HEAD` arc and closed it. What it
found and did:

- The `SaturationKey` / `RefCell<HashSet<..>>` conversion in `account.rs`, its
  call sites, and the `BTreeSet` currency union in `snapshot` were re-read in
  full and are sound: every borrow of the `RefCell` is confined to
  `warn_saturated`, `free_balance`'s warn fires after its folds, and the union
  preserves the sorted-unique contract the old `Vec` sort provided.
- The `MarketToLimit` residual was settled the way round 2 suspected: the
  protocol doc was the stale half. Its "broken in both halves" paragraph
  described the pre-2026-08-19 engine; `orders.rs` prices the executing part
  off the tape bounded by the stated limit and rests a kept remainder as a
  scannable limit. The doc comment in `mogwai_protocol::messages` now states
  the implemented model and names the old text as the closed defect record.
- The four round-1 residual rulings (the overloaded `apply_divergences` flag
  with its owed split, `clear_armed`, `MAX_GROUP_ORDERS`, and the unbumped
  `TAPE_PROTOCOL_VERSION`) were re-verified and stand. Nothing in the arc
  touched `mogwai-data`, `analysis/` or the fingerprint, so no bump was owed.
