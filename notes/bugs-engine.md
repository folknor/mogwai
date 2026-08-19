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

FINDINGS 5 THROUGH 9 ARE FIXED AND REMOVED. All five reproduced; they were FOUR
defects, because 7 and 8 were one rule with two drifted copies and were closed by
one collapse. What landed, in the shape the structural note asked for rather than
five site patches:

- `position_unrealized_checked` in `lib.rs` is now the ONE unrealized
  expression, with `position_unrealized` as its saturating wrapper; both
  delegate to `InstrumentDef::unrealized`. FOUR readers, not three:
  `unrealized_pnl` (and thence the margin breach test and forced liquidation),
  the `positions()` wire rows, `settle`'s realized credit and `valuation_at`'s
  derivative contribution - the last one was missed by the first pass and closed
  by the cold review, and until it was, one FLAT inverse position made
  `valuation_at` answer `None` for the whole account and the tick-resolution risk
  sweep silently declined to value it.
  The split exists because `InstrumentDef::unrealized` answers `None` for two
  unrelated reasons and the first pass conflated them: an inverse at a ZERO
  PRICE is undefined rather than overflowed, and saturating it credited
  `Decimal::MAX` to the balance on any zero settlement price. The checked form
  answers zero for both defined-but-degenerate cases (flat, and inverse at zero)
  and reserves `None` for genuine overflow.
  `valuation_at`'s doc said equity is LINEAR in the price of a held instrument,
  which is false for `Inverse`; it now says MONOTONE, which is both true and
  what the two-point extreme evaluation actually needs.
  Correction to the report's finding 5,
  so a later round does not re-derive it wrong: the linear form is wrong in
  MAGNITUDE only, never in sign. `1/avg - 1/mark` and `mark - avg` always carry
  the same sign, so no inverse position was ever liquidated in the wrong
  direction - it was liquidated on a number that can be four orders of magnitude
  too large. The regression fixtures pick a 20,000-times separation for exactly
  that reason.
- `margin_requirement` reads `policy.maintenance` for its positions and the
  order's OWN `order_reservation_entry` for its resting orders, so no `initial`
  row can disagree with the hold it reports. Its doc's "sum is exactly what
  `locked_balances` reserves" was still overstated after that, and now names
  three carve-outs that make the general statement a `<=`: `locked_balances`
  also folds `account.unsettled` credits, holds on unmargined or unmarked
  symbols, and `Reservation::Base` holds this function must skip because a
  margin row is denominated in settlement currency.
  `the_reported_margin_reconciles_with_the_reported_locked` sits inside all
  three, so its doc now says it pins the EQUALITY CASE and not the wider claim.
- `order_reservation` uses its `price` ARGUMENT. `on_modify`'s hand-rolled
  fourth copy is gone: it builds the amended `SubmitOrder` and calls
  `order_reservation` for both sides of the comparison, the way `held_for`
  already did. Three follow-ups from the cold review landed with it: the block
  now short-circuits for a `Resting::Held` child (`order_reservation` has no
  Held rule - that lives one level up in `order_reservation_entry` - so the two
  consumers had disagreed about the same question and an amend of a bracket's
  unreleased exit leg was checked against a hold the venue never takes); the
  spot-sell misconfiguration refusal no longer emits the FUTURES reason string;
  and the rework's incidental fix of the equity-sell amend - every equity sell
  amend used to hit the `base_currency()` else arm and be refused with "cash-
  settled futures require the margin ledger" - now has `an_equity_sell_can_be_amended`
  standing over it. The dropped `clipped` flag was checked and left as it is:
  every CHECK-TIME reservation site drops it, `held_for` included, and the
  RECORDING path re-derives it when the amend lands. That is now stated at the
  site rather than left to be re-discovered.
- `validate_submit`'s equity short check reads `Engine::worst_case_leaves`, the
  new single home of what "worst fill order" means, which `projected_qty` also
  reads. Verified reachable first: `projected_qty` has exactly one consumer,
  `mogwai-server`'s optional `max_position` cap in `http.rs`, which bounds
  magnitude and says nothing about shorting, so no upstream layer closed this.
  THE FIRST VERSION OF THIS FIX WAS A PLAIN SUM AND OPENED TWO REGRESSIONS the
  cold review caught; both are closed and both have tests:
  - It counted `Resting::Held` children and BOTH legs of an `Oco` exit pair, so
    an ordinary bracket over held shares read as a short and a cash equity
    account refused the second leg by name. The rule is now: a held child
    contributes nothing (the same rule `order_reservation_entry` applies), and
    an `Oco`/`Ouo` group contributes the MAX of its legs rather than their sum,
    keyed by `order_list_id`. `projected_qty` SHARED THE BLIND SPOT and is fixed
    by the same helper; there it was an over-refusal of a magnitude cap rather
    than a wrong admission, which is why nothing had surfaced it.
  - It made the group's two passes disagree, because it read `self.open` and the
    dry pass runs before any member rests. `report_group_member_refusal`
    re-asks the dry question AFTER the refusal, so the siblings are resting by
    then and it refuses again - landing on the benign "disclosed funds
    carve-out" warn branch and never tripping the `debug_assert` that commit
    `58a9557` installed for exactly this. The fix makes the number
    pass-invariant: the group's MEMBER ORDERS (not their ids) now travel through
    `dry_refusal`, `on_submit_from`, `validate_submit` and `validate_link`, and
    `worst_case_leaves` counts a member from that list instead of from the book
    whenever it appears in both. A group of two independent equity sells over
    one holding is now refused WHOLE on pass one, which is the atomic answer.

One residual is filed in `notes/todo.md` rather than fixed here: the equity sell
RESERVATION still hands the same held shares to every resting sell, because
`order_reservation` is per-order by construction and the incremental
`order_locked` cache depends on that independence. Admission is now correct; the
hold a margin equity account carries while several sells rest is not.

FINDING 10 IS CLOSED AND FINDING 12 - the `MarketToLimit` item routed here from
`notes/todo.md` - LANDED WITH IT, both in round 3. Five of finding 10's six
bullets reproduced and are fixed; one is refused with its reasoning recorded.
With that THE DOCUMENT IS EXHAUSTED: nothing here is open, and what remains is
one withdrawal (section 11), one refusal (inside section 10) and one residual
filed in `notes/todo.md`.

The hunter read all four source files (`orders.rs`, `account.rs`, `lib.rs`
non-test region, `divergence.rs`) plus the protocol-side helpers the engine
leans on (`InstrumentDef::unrealized`, `validate_submit_group`,
`InstrumentClass`). No files modified.

## 10. Smaller and lower-confidence - CLOSED

All six bullets settled by the round-3 fix pass. Five reproduced and are fixed;
one was never a finding and its refusal is recorded here so a later round does
not re-derive it.

- THE GROUP'S ATOMICITY GUARANTEE IS NO LONGER CONTINGENT ON AN EXTERNAL
  VALIDATOR, which is the one bullet section 11 routed here as the engine's real
  exposure. `on_submit_group` now calls `mogwai_protocol::validate_submit_group`
  itself, before the `RejectNextSubmit` arm, and refuses the group whole with
  that validator's own reason. Called, not copied: one arm re-spelled in the
  engine would read as the rule's home while implementing a fraction of it,
  which is what round 1 declined to do and why it left this open.
  MEASURED BLAST RADIUS BEFORE LANDING, at the call sites rather than by
  grepping. Two of the validator's rules are unreachable from the dry pass by
  construction - an intra-group DUPLICATE ID (`validate_submit` reads
  `seen_client_order_ids`, and on pass one no member is in it) and an
  `Ioc`/`Fok` MEMBER - and the duplicate case is a genuine atomicity break: the
  bite-check shows the first copy ACCEPTED and RESTING and the second refused.
  Everything else the validator asks was already true of every group the engine
  admits, verified by the suite: no test changed verdict.
  TWO ENGINE TESTS WERE WEAKENED BY IT AND WERE REPAIRED IN THE SAME CHANGE,
  which is the hazard of adding an earlier gate.
  `one_bad_member_rejects_a_whole_group_and_accepts_nothing` made its bad member
  bad by naming an unknown SYMBOL, which the group validator now refuses one
  step earlier for disagreeing with its siblings; it is now bad by an off-grid
  PRICE, which only the engine holds the increment to judge.
  `a_group_of_two_equity_sells_over_one_holding_is_refused_whole` sent UNLINKED
  members, which are now refused for the missing link rather than for the equity
  short check the test is named after; its members carry a `NoContingency` link
  and it asserts the refusal TEXT, since every wire-shape rule also refuses the
  group whole with the same two ids.
- THE GROUP'S CLOSING SNAPSHOT NOW CONSULTS `DropNextAccountUpdate`, like every
  other snapshot site. It also consults `account_changed` first, so a closing
  pass that moved nothing does not spend an arm. The N+1 half of the bullet is
  NOT a defect and was not changed: the per-member snapshots report pass two and
  the closing snapshot reports the linkage, which is a later state.
  The test arms the divergence TWICE, because the filling member spends the
  first on its own snapshot - which is also why the gap never bit.
- `account_changed` IS NOW EXHAUSTIVE OVER `ServerMessage`, with `OrderUpdated`
  and `OrderExpired` counted. Confirmed latent rather than live before changing
  it: at both call sites an `Ouo` shrink only ever accompanies the fill that
  caused it, and no expiry reaches either. The match is exhaustive so the next
  variant must be classified rather than inherit `false`, which is how these two
  were lost. This closes the last member of the structural note's family.
- BOTH LIQUIDATION PREFIXES ARE RESERVED. `RESERVED_ID_PREFIXES` in
  `mogwai-engine`'s `lib.rs` is the one list, read by `validate_submit` and by
  the three minting sites. The bullet understated the consequence: a client
  claiming `RISK-MNQ-1` does not merely collide, it BURNS the id in
  `seen_client_order_ids`, so the account-policy flatten that later mints it is
  refused as a duplicate and the venue cannot liquidate the account that
  pre-claimed it.
- THE TRIGGERED `StopMarket` REMAINDER NOW INVALIDATES ITS OWN WALK. The bullet
  called it harmless-for-now; it was not. Keeping the pre-trigger `scanned_ns`
  and `revision` means the result that TRIGGERED the order still satisfies
  `apply_scans_on_clock`'s staleness guard, so a duplicate or late delivery of
  that result finds an order that is no longer `Conditional`, falls through to
  the resting-limit arm, and PANICS on a market-on-trigger order's absent price.
  The remainder now advances the frontier to where the walk reached and bumps
  the revision, exactly as the limit remainder beside it already did.
- REFUSED: the `draw_key` collision surface. Not a defect and the bullet says so
  itself - the behaviour is documented and accepted. A strategy re-rolling its
  queue position by cancel-and-resubmit under a new id is what a keyed draw
  means; making it adversarially sound would be a design change, and there is no
  claim in the tree that it is.

## Structural note

Six of the ten findings (1 aside: findings 2, 3, 6, 7, 8, and the
`account_changed` gap) shared one cause: A RULE WITH ONE STATED HOME AND SEVERAL
HAND-ROLLED COPIES. The reap half closed in round 1 - `Engine::close_out` is the
single home and every terminal path routes through it - and rounds 2 closed the
other two. The reservation formula lived in `order_reservation`, again in
`on_modify`'s futures branch, again in its buy/sell branch, and
`margin_requirement` had a fourth; all four now route to `order_reservation`,
`margin_requirement` through `order_reservation_entry` because it needs the
held-child and currency rules that wrap it. The P
and L formula lived in `InstrumentDef::unrealized` and again in FOUR places in
lib.rs; those four now read `position_unrealized_checked`, which delegates. THE PROSE
WAS RIGHT EVERY TIME - `apply_fill`'s "realized and unrealized must come from the
same expression", `margin_requirement`'s "reconciles by construction",
`architecture.md`'s "`InstrumentDef` carries the one implementation of both
forms, so realized and unrealized can never disagree". In each case the statement
sat next to one implementation while the others drifted, which is why the fixes
were collapses rather than corrections: correcting a copy leaves the next drift
undetectable, and collapsing it makes the stated invariant true by construction.

The `account_changed` gap was the last of this family and closed in round 3,
by making the predicate exhaustive over `ServerMessage` rather than by adding
the two missing variants to a hand-written list. The family is now empty.

## 12. `MarketToLimit`, routed here from `notes/todo.md` - FIXED

Not a finding of this report. Filed by the `bugs-protocol` round-2 fix pass as
ONE DEFECT WITH TWO SYMPTOMS and routed to this document; the round-3 fix pass
closed both, and the todo entry is removed.

WHAT THE VENUE DID, end to end. `wire_order_type` has no refusal arm, so the
type reaches the engine over `/ws` and through the adapter unimpeded;
`validate_submit_order` requires it to carry a price and no trigger; the submit
path drew a slipped market price ONLY for `OrderType::Market`, so a
`MarketToLimit` filled its whole quantity at `risk_px` - its own stated limit -
with no reference to the tape, and `marketable` was computed and then consulted
only for `Limit`. A remainder therefore never arose on the clean path, and where
an armed `PartialFillNext` manufactured one it rested `Resting::Inert`: on the
book with a positive `leaves_qty`, offered to no sweep, unable to fill or
expire.

WHY NO OWNER RULING WAS NEEDED, which is what the filing flagged as
owner-level. The product question - serve the type faithfully or refuse it - is
already answered in the tree, twice. The ORDER-TYPE COMPLETENESS RULING of
2026-08-16 says the venue serves the full nautilus surface and curates nothing,
and `docs/oms-types.md` and `reference/architecture.md` both state the type as
SERVED. Refusing it would contradict a standing ruling and two durable
documents; serving it properly contradicts nothing. So the honest close was to
implement it, and the fix pass did rather than sharpening the filing.

WHAT LANDED. `on_submit_from` now spells two predicates once each and reads them
at every decision that used to hand-roll `== OrderType::Limit`:
`takes_the_market` (`Market | MarketToLimit`) prices the executed part off the
last print through the same band draw a market order uses, and `rests_as_limit`
(`Limit | MarketToLimit`) governs the unmarketable-on-arrival rest, the
`Resting` the record is frozen with, and the partial remainder's tranche
redraw. The stated price BOUNDS the fill - a buy never above it, a sell never
below - because the band's adverse slip can otherwise carry the order past the
one price the client asked the venue to respect. A touch short of the limit
takes nothing and rests the whole quantity, which is `marketable` finally being
consulted for the type it was computed for.
`a_market_to_limit_remainder_is_governed_by_its_time_in_force` was UPDATED, not
deleted, as it asked to be: it pins the fill price at the market (99) rather
than the limit (100), the remainder resting as a limit and offered to the sweep,
and a new arm with the market ABOVE a buy limit that needs no divergence to
reach.

NO TAPE VERSION BUMP IS OWED, and the reasoning is recorded because the filing
named `draw_market_price` and `AGENTS.md` lists "the fill band's draw" among the
things that owe one. `mogwai-engine` does not depend on `mogwai-data` and cannot
see `TAPE_PROTOCOL_VERSION`; the fill draw is keyed on `fill_seed`, which the
generator's stream is deliberately kept separate from so that the tape is not a
function of client behaviour. No generated byte moves, and the clause names the
tape-side band model rather than the engine's per-order draw.
`TAPE_PROTOCOL_VERSION` is 22.

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
