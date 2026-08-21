# Glossary scope pass 4: the engine's public surface

Inventory of `mogwai-engine` - the `Engine` itself and everything a caller
outside the crate can reach or is described by: the ledger and account
machinery, orders and their states, fills, positions, margin, reservations and
holds, the linkage apparatus, liquidation, the divergence seam, and every
refusal and error a caller can receive - measured against
`reference/glossary.md` as it stands 2026-08-21. Nothing was edited but this
file.

**[P4b reconciliation, 2026-08-21]** A second pass was performed independently:
the glossary and engine surface were read before this report was opened, to
resist anchoring on its first-pass conclusions. Rows added or changed by that
pass carry **[P4b added]** or **[P4b changed]**. Refuted first-pass rows remain
in place with the worked reading that refutes them.

Scope split: `notes/glossary-scope-1.md` covers `mogwai-protocol`,
`notes/glossary-scope-2.md` the venue's external surface,
`notes/glossary-scope-3.md` the venue's internal domain model. Their rows are
not re-derived. Where this surface CONTRADICTS or EXTENDS one of them it is
marked **[extends P1/P2/P3]** or **[contradicts]** and says how.

Direction key: **1** a glossary term doing a job that is not that term's; **2**
a job the glossary already names, under a different word; **3** load-bearing
and undefined; **structural** a glossary claim about the ledger or the account
checked against this code, whether confirmed or falsified; **inherited**
vocabulary this project does not own.

Reach key: `local` crate-local; `cross` across a crate boundary (the server and
the lab both build on this crate's public API); `wire` externally visible - a
refusal string or a field that reaches a client verbatim.

---

## The headline

Five findings outrank the renames. The second pass confirms two structural
contradictions, narrows two others, and refutes the first pass's reading of one.

1. **`BreachAction` is TWO DIFFERENT TYPES with disjoint variant sets, and the
   glossary defines only one of them.** `mogwai_engine::BreachAction` is
   `Refuse | Liquidate` (margin maintenance). `mogwai_protocol::risk::
   BreachAction` is `LockUntilReset | Terminate` (account policy). The
   glossary's Account policy entry says "what it does on breach - flatten and
   lock until the next session boundary, or flatten and terminate", i.e. it
   states that a breach action is one of exactly two things, and it names the
   WRONG two for the type a caller of this crate holds. Both types are in
   scope of one sentence in `MarginPolicy`'s own field list
   (`pub breach_action: BreachAction`). This is the worst collision found in
   any of the four passes: one word, two types, two enforcement systems, both
   reachable from one account, and the durable document that defines the word
   describes only the other one.

2. **"An account is on at most one river" appears here once as a false
   capitalized premise**, in `retire_off_river`'s doc. **[P4b changed]** The
   first pass also attributed the premise to `cancel_unreadable_orders`; that method
   does not state that premise: it cancels against the distinct, directly
   checkable condition that no boat is reading an order's symbol. Pass 1 found
   the one-river premise in `risk.rs`, pass 3
   found it in `extremes.rs`, `risk.rs` and `admission.rs`. That is now SIX
   reported sites, but only `retire_off_river` is an engine instance of it.
   That method does flatten positions and cancel orders, but its server call is
   guarded by `returning = passenger.is_frozen()`, and the glossary's Seat
   entry explicitly requires the same return-time retirement. The premise is
   false; the guarded behaviour is documented and is not evidence that a live
   two-instrument account loses half its book.

3. **`Engine`'s own `event_sim` field doc says "The engine is venue-wide".** It
   is not. The glossary's Ledger entry says a ledger is one engine instance
   owned by one account, and `run.rs` holds one per passenger. A durable
   comment on the field that carries the fee-surcharge clock states the
   opposite of the crate's own identity.

4. **`Engine::cancel_unreadable_orders` is a consumer contract the glossary
   leaves implicit, not one it forbids.** **[P4b changed]** The Ledger entry promises that "every socket a consumer opens
   under one account id acts on that account's ledger, whatever symbol each
   bound, so a consumer trading two instruments is trading one book." This
   method cancels every resting order whose symbol is not currently being read.
   For a two-instrument account whose second connection leaves and therefore
   lets its boat wind down, the remaining attached account receives ordinary
   `OrderCanceled` events for orders on that now-boatless symbol. The account
   still has one ledger while both sockets exist, so this does not falsify that
   claim. It does expose an omitted lifecycle rule: an order does not keep its
   river alive after the last socket leaves, and remaining attached sockets do
   not preserve such an order merely because they share its ledger.

5. **The glossary's Posted margin entry is one of the few entries this pass
   can confirm outright** - `margin_requirement` implements it exactly,
   including the reduce-only carve-out - and it is also the entry most likely
   to be misread, because the same function's doc spends thirty lines
   explaining that `sum(initial) + sum(maintenance)` is a `<=` of the reported
   `locked`, not an equality. The glossary says nothing about that gap and a
   reader reconciling the two numbers will assume equality.

---

## Structural - glossary claims about the ledger and the account, checked here

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Account policy ("what it does on breach ... one of exactly two things") | `lib.rs` `BreachAction { Refuse, Liquidate }`; `MarginPolicy::breach_action` | type, variants, field | what the MARGIN MAINTENANCE ledger does when collateral falls below requirement: refuse further entry, or force-close the position | cross | structural | **The glossary's "exactly two things" is true of a different type.** Two `BreachAction` enums exist, in two crates, with four distinct variants between them and no shared member. Rename this one `MarginBreachAction { Refuse, Liquidate }` (or `MaintenanceAction`), keep `risk::BreachAction` for the policy sense, and make the glossary entry say which is which. Nothing in the tree currently prevents a reader from importing the wrong one and matching on variants that do not exist. |
| Account policy ("enforced by flattening and locking, or terminating") | `lib.rs` `liquidate_all` doc: "This is what enforcing an ACCOUNT POLICY does on breach" | method, doc prose | flatten only - cancel resting orders, close positions at the mark | cross | structural | Accurate as far as it goes and the doc is good, but it claims to be the whole of policy enforcement while implementing only the FLATTEN half; the lock and the terminate live in `mogwai-venue`. A reader of this crate cannot find where "until the next session boundary" is decided. Say "the flatten half of account-policy enforcement; the lock and the terminate are the server's". |
| Ledger ("one `mogwai-engine` instance, owned by one account") | `lib.rs` `Engine::event_sim` doc: "The engine is venue-wide but every pass through it belongs to exactly ONE boat" | field doc prose | asserts the engine is a venue-scoped object | local | structural | **Flatly false and it is the load-bearing sentence of the crate's trickiest field.** One engine per account. Rewrite: "the engine is per-ledger and a ledger's sockets may sit on several boats, so a pass belongs to exactly one of them." The rest of the doc's reasoning survives the correction unchanged. |
| Ledger ("a consumer trading two instruments is trading one book") | `lib.rs` `cancel_unreadable_orders`; `sweeper.rs` global `readable` set | method, doc prose, call site | cancels resting orders whose symbols have no live boat anywhere in the run | cross, wire (client sees `OrderCanceled`) | structural | **[P4b changed, first-pass verdict refuted in part]** It does not assume one river and does not cancel half the book while both instrument sockets remain: `readable` is built globally from every live boat, so both symbols survive. It bites after the last rider of one boat leaves while the account remains attached elsewhere. The ordinary cancel events make it non-silent. This does not contradict "one book"; it is an omitted consequence of the glossary's Boatyard rule that a boat winds down with its last passenger. Add the consequence to Seat or Ledger: ledger ownership alone does not keep an order's river readable. |
| Ledger / Seat ("an account holds as many seats as the distinct boats its sockets have bound") | `lib.rs` `retire_off_river`; `run.rs` `Run::resume` | method, doc prose, guarded call site | on return from freeze, flattens positions and cancels orders not on the first returning socket's symbol | cross | structural | **[P4b changed, first-pass contradiction refuted]** The capitalized premise "AN ACCOUNT IS ON AT MOST ONE RIVER" is false and must be removed. The behavior is nevertheless exactly what the current glossary's Seat entry says: "when a frozen account returns, what its book holds off the river the returning socket joins is retired". `Run::resume` calls it only after observing `passenger.is_frozen()`; live second binds take the early return and retire nothing. Thus a frozen multi-instrument account can have the other legs retired when its first socket returns, but that is a documented return rule, not money moving on an undisclosed one-river premise. The real design question is whether that glossary rule should survive: it makes return order economically significant. |
| Ledger ("positions, balances, order history and armed divergences are all per ledger") | `lib.rs` `Engine` fields `account`, `closed`, `fills`, `seen_client_order_ids`, `armed` | fields | exactly that | cross | - | **No defect. The claim is TRUE and this is the site that makes it true.** Recorded because it is the only one of the Ledger entry's four claims that survives the pass intact, and because two retention decisions ride on it: `closed` and `fills` are unbounded on purpose. The glossary should say "order history is retained for the ledger's whole life" - a consumer sizing a long run needs that. |
| Divergence ("Engine arms queue one-shot execution divergences on the account's own ledger") | `divergence.rs` `Engine::arm` | method | classifies every variant as engine-armed or server-owned, exhaustively, no catch-all | cross | - | **No defect, and this is the model case for the whole workspace.** The exhaustive match with no `_` arm is the mechanism pass 1 asked `CommandClass::of` for. Note it produces a THIRD classification of the divergence set, alongside the glossary's five-way one and the routing site's eight/five split (pass 2). Three taxonomies, one enum. The reconciliation pass 2 asked for should adopt THIS one, because it is the only one the compiler enforces. |
| Divergence ("windowed account-side arms (`FeeSurcharge`) apply to the ledger for their span") | `lib.rs` `arm_fee_surcharge`, `fee_surcharge_multiplier_for` | methods | a WALL arming instant plus a SIMULATED span, opened per reader at `max(sim_ns(armed), sim_epoch_ns)` | cross | structural | **The glossary's "for their span" hides the whole mechanism.** There is no single span: the window has no axis of its own and resolves differently on every boat clock, under the LATE-BOARDER RULE. Pass 3 found the same rule open-coded a second time in `arm_divergence`'s FlowSurge branch. That is now two implementations of one rule in two crates with nothing shared - the exact shape `AGENTS.md` says to anchor with a fixture or a module. The glossary entry should state the rule, not the word "span". |
| Freeze ("a frozen account is not swept, not marked, not funded and not judged") | `lib.rs` `rebase_scans` / `rebase_future_scans` docs | methods, doc prose | the repair a returning socket owes the book the freeze left behind | cross | 3 | The Freeze entry states what does NOT happen and never states the debt. "Nothing is owed for the span in between" is a real, load-bearing consumer-visible ruling - a resting order that would have filled during the freeze does not - and it lives only in these two doc comments. It belongs in the Freeze entry. |

---

## Direction 1 - a glossary term doing a job that is not its own

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Eviction | `orders.rs` `apply_scans_on_clock` comment: "a swept fill or a funds-check eviction is an order executing or leaving the book" | comment | an order CANCELLED because its own fill failed the funds check | local | 1 | The glossary's Eviction is a socket claiming a seated account id from another consumer. Nothing is evicted here; an order is cancelled. One word, and it is the word a reader greps for when tracing account handover. "funds-check cancel". |
| Ledger | `orders.rs` `reduce_only_cap` doc: "Reads the ledger's own position map" | doc prose | the `Account.positions` map inside this one engine | local | - | Correct usage, recorded as the model: "ledger" here means this engine's state, which is the glossary's sense exactly. Contrast the `event_sim` row above. |
| Ledger | public `Engine` / `EngineConfig` / `Engine::build` vocabulary | type and constructor surface | the glossary's ledger: one account-owned execution and account-state machine | cross | 2 | **[P4b added]** The glossary names the job Ledger while the public API names the object Engine. This is not inherited vocabulary and the server exposes the mismatch at every `Passenger.engine` call site. Pre-1.0, prefer `Ledger` / `LedgerConfig`; if `Engine` stays, the glossary should define `Engine` as the implementation name rather than making callers infer the equivalence. |
| Session | `orders.rs` `expire_orders` doc: "a `Day` order must stop resting at the session boundary"; `closed_symbol` "names an instrument whose SESSION JUST CLOSED" | doc prose, parameter | the instrument's SESSION CALENDAR window | cross | 1 | **[extends P1's `LockUntilReset` row]** The glossary has Session (the consumer identity on `?session=`) and Session calendar (the instrument's weekly windows) as two entries and this uses the bare word for the second. Pass 1 found the same bare use in `risk.rs`, where it meant a THIRD clock (`reset_minute_utc`). Three clocks, one bare word. Spell it "calendar session" everywhere the instrument's window is meant, and never leave "session" bare in a doc. |
| Boat | `orders.rs` `apply_scans_on_clock`; `lib.rs` `fee_surcharge_multiplier_for` | doc prose and fee calculation | the paced reader whose clock stamps a pass and resolves a wall-armed simulated-duration surcharge window | cross | 1 | **[P4b changed, first-pass conclusion narrowed]** Confirmed: the surcharge is judged on the commanding or swept boat's clock. Refuted: there is no single shared simulated "fill instant" at which two differently paced boats can be compared, and the glossary's no-semantics sentence concerns whether a hull/cache is shared, not whether two requested speeds have different clocks. Each boat gets the same local simulated span, opened by the documented late-boarder rule. The real mismatch is narrower: the glossary calls `FeeSurcharge` ledger-side but does not state that its temporal membership is resolved per event's boat. State that rule, then separately decide whether a ledger-wide absolute window is desired. |
| Client | `orders.rs` `push_account_snapshot` doc: "the client is left to notice"; `on_submit_from`: "an arm the client aimed at its own next fill"; `expire_orders`: "the clock rather than a client drives it" | doc prose, pervasive | the party that sent the command | cross | 1 | **[extends P1's Client row]** The glossary says "`client` is not used for anything this project owns" and grants exactly two inherited exceptions. This crate uses it as its PRIMARY word for the commanding party, roughly a dozen durable sites. The glossary's own Consumer entry says the word for the party on one socket is Session. Whatever the ruling, this crate is where the cost is: it is the biggest single user of the forbidden word. |
| Venue | `lib.rs` module doc: "The venue-agnostic exchange core"; `Engine::event_sim` "venue-wide" | doc prose | "protocol-agnostic" in the first case, "not per-account" in the second | local | 1 | Two uses of the glossary's Venue in one crate, neither of which means one running instance of mogwai. The first means the core does not import nautilus and could sit behind a Binance facade - that is PROTOCOL-agnostic, and "venue-agnostic" reads as "does not know which exchange it is", which is not the claim. |
| Warmup | - | - | absent | - | - | Recorded as a NON-finding: the engine never uses the word, which is correct - it receives observations and owns no tape. |

---

## Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Posted margin | `lib.rs` `margin_requirement` -> `mogwai_protocol::PostedMargin` | method | the glossary's Posted margin, exactly | cross, wire | - | **No defect; the model case.** The entry, the type and the function agree. Recorded so the next pass does not re-litigate it. |
| Posted margin / reservation | `account.rs` `Reservation { None, Settlement, Base }`, `order_reservation`, `order_reservation_entry`, `order_locked`, `held_for`, `locked_balances`, `free_balance` | type, variants, methods, fields | the funds a resting order ties up | cross, wire (`Balance.locked`) | 2/3 | **[extends P1's reservation cluster]** Pass 1 found "reservation" carrying a BYTE sense and a FUNDS sense in one crate and proposed *hold* for the funds one. This crate is where the funds sense actually lives, and it spends FIVE nouns on it: reservation, hold (`held_for`), lock (`order_locked`, `locked_balances`), and `Reservation::None` vs "reserves nothing" vs "moves no number". Pick one. My read: `Hold` for the object, `held` for the verb, `locked` reserved for the wire field it names, and delete "reservation" from this crate entirely. |
| Variation margin | `lib.rs` `settle` | method | the glossary's Variation margin - realize at the settlement price, rewrite `avg_px` | cross | 2 | The glossary names the concept and the code calls it `settle`. Not a defect exactly, but the entry says "the VWAP resets to that price" and the code writes BOTH `avg_px` and `mark_px`, which is the same thing said twice. One of the two names should give: either the entry says "settlement" or the method says `apply_variation_margin`. |
| Divergence / clearing | `divergence.rs` `clear_armed` | method | flushes the engine-side one-shot queue - deliberately NOT `control::ClearDivergences` | cross | 2 | **[extends P1's `ClearDivergences` row]** Pass 1 asked for `ClearDivergences -> ClearTemporalWindows`. This method is the other half and its doc explains the split at length. If the wire variant is renamed, this method's doc is a call site of the rename. Also: `clear_armed` is `pub` and nothing outside the crate calls it - it is a harness escape hatch with no consumer. |
| Freeze / stranded | `lib.rs` `retire_off_river`, `cancel_unreadable_orders` local binding `stranded` | bindings, log text | an order or position on a river nobody reads | local, wire (operator log) | 2 | "Stranded" and "off-river" and "unreadable" are three words for one condition in two adjacent methods. `cancel_unreadable_orders`'s log says "on a river no cursor is reading" while `retire_off_river`'s says "off the river this account is bound to" - two different conditions described as if they were one. Pick the boatless condition, name it once, and use it in both. |

---

## Direction 3 - load-bearing and undefined

### The order-state cluster - resting, held, inert, terminal, open, live

The `Resting` enum is public and is the single most load-bearing undefined
vocabulary on this surface: it decides what the tape may act on, what holds
funds, and what a query reports.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `lib.rs` `Resting { Limit, Conditional, Inert, Held }` | type, variants | what the venue is waiting for on one resting order | cross | 3 | Four venue-side states with excellent docs and no durable home. `Held` in particular is a real consumer-visible contract - accepted, answerable to `QueryOrders`, holding NO reservation, scanned by nothing - and a client reading `WireOrderStatus::Accepted` cannot tell it from a live limit. That gap is the whole of one-triggers-the-other and it is invisible from the wire. |
| - | `Resting::Inert` | variant | a market remainder with no price the tape can reach; ends only on a client cancel | cross | 3 | An order that CAN NEVER FILL AND CAN NEVER EXPIRE, produced by an armed `PartialFillNext`. `mogwai-protocol`'s `MarketToLimit` doc names the same hazard (pass 1 lateral 11). A client holding one has no way to know. Owed a wire status or at least a glossary line. |
| - | "terminal" (`closed`, `record_closed`, `not_resting_reason` -> `"order already terminal (filled or canceled)"`) | field, method, refusal text | reached `Filled` or `Canceled` and frozen into the truth store | cross, wire | 3 | **[extends P1]** Pass 1 asked for `close::Terminal` to be defined. Here "terminal" is a SECOND, unrelated sense - an order's finality, not a close frame's. And the refusal text says "(filled or canceled)" while `expire_orders` produces `Expired`, which `record_closed` also stores. The parenthetical is narrower than the set. Small, wire-visible, and a client parsing it will conclude an expired order is unknown. |
| - | `open_order_status`'s status derivation (`PartiallyFilled` / `Triggered` / `Accepted`) | fn | the venue's own answer for a resting order | cross, wire | 3 | Three of `WireOrderStatus`'s seven variants are derived HERE from fill progress and `ts_triggered`, and the precedence (partial beats triggered) is stated nowhere. `is_open` including `Triggered` is pass 1's row; this is the site that produces it. |

### The band cluster - band, draw, trigger, tranche, hit, scan, frontier

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `OpenOrder.band_ticks`, `band_draw`, `MarketReading.band_ticks`, `draw_trigger`, `DEFAULT_LIQUIDATION_BAND_TICKS`, `set_liquidation_band_ticks` | fields, fns, constants | the randomized fill trigger and the half-width it is drawn within | cross, wire (config key `fill_band_max_ticks`) | 3 | **[extends P1]** Pass 1 flagged "band" as undefined and TAPE-VERSION-BEARING. Here it is a public constant, a public setter, a struct field a caller populates and a per-order draw counter. A caller of `Engine::build` must supply `fill_seed` and a caller of `process_with_market` must supply `band_ticks` with no document anywhere saying what either means. This is the single largest undefined vocabulary on the crate's constructor path. |
| - | `OpenOrder.band_draw` doc: "Deliberately NOT `revision`, which sweep passes bump" | field doc | the RNG key half that must not move with sweep timing | cross | 3 | A determinism contract - the trigger must not be a function of how often the sweeper ran - stated in one field comment. It is `TAPE_PROTOCOL_VERSION`-adjacent (the fill band's draw is named in the bump rule) and belongs in `reference/`. |
| - | "tranche" (`apply_scans_on_clock`: "An execution starts a NEW tranche ... Each tranche has to be traded through on its own, and gets a fresh queue position") | comment | one partial fill's remainder, re-drawn and re-queued | local | 3 | A real model of queue position, used nowhere else and defined nowhere. It is the reason a partial-filled limit does not free-fill on the next pass. One line. |
| - | `OpenOrder.scanned_ns`, `revision`, `PendingScan.from_ns`, `ScanResult.scanned_to_ns`, `rebase_scans`, `rebase_future_scans` | fields, methods | the per-order frontier and the staleness gate over off-lock walks | cross | 3 | **[extends P3's frontier cluster]** Pass 3 found the per-BOAT frontier undefined. The per-ORDER one is here, it is public, and it is the one `AGENTS.md`'s frontier family is actually about: `scanned_ns` advances only to `result.scanned_to_ns` (guarded), and `revision` is what makes two overlapping walks safe. Three names, one invariant, stated only in field docs. The whole seam - `pending_scans` out, `ScanResult` back - is a public contract with no durable description. |
| - | `Hit`, `ScanKind`, `trades_through`, `touches_trigger` (imported), `PendingScan.px` doc "the stated price a fill books at stays inside the engine" | types, fns | the tape-walk predicate vocabulary | cross | 3 | Pass 1's tape-walk cluster, confirmed from the consuming side. The load-bearing addition this crate makes: the TRIGGER price and the BOOKING price are different numbers and only the trigger crosses the seam. That distinction is what makes the band invisible to the walker and it is stated in one field doc. |

### The linkage cluster, engine half

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `apply_linkage_after_fill`, `release_child`, `held_children_of`, `reap_children_of`, `close_out`, `close_unrested`, `note_absent_sibling`, `has_filled` | methods | release, cancel-siblings, shrink-siblings, reap-orphans | cross | 3 | **[extends P1's linkage cluster]** Pass 1 asked the glossary for Linkage, Group, Sibling, Parent/Child and the atomic-admission guarantee. This crate adds four more verbs the wire never sees: RELEASE (a held child becoming live, emitting NO frame), REAP (cancelling orphaned children), CLOSE-OUT, and the `has_filled` predicate that decides which. "Release emits no wire frame" is a consumer-visible fact - a client watching for a status change on its bracket's exit legs will wait forever - and it is in a doc comment. |
| - | `on_submit_group`'s two-pass structure: "dry pass", "pass one", "pass two", "the closing linkage pass", `dry_refusal`, `report_group_member_refusal` | methods, doc prose | the atomic-admission mechanism | cross | 3 | The best-documented code in the crate and entirely absent from any durable document. The DISCLOSED FUNDS CARVE-OUT - a group member CAN be refused after admission when an earlier member's fill spent the balance - is a hole in the atomicity guarantee that a consumer must know about, and it is stated in a doc comment here and one on `Command::SubmitOrderGroup`. That is the contract, and it lives on the two ends of a seam rather than in the middle. |
| - | `MAX_LINKED_ORDERS` depth bound: "`validate_submit` refusing a child of a child" | doc prose | why the reap worklist terminates | cross | 3 | A termination argument for a `while let` loop that depends on a rule enforced in a different function three hundred lines away. True today. It is the "check the callee's unreachable!s" shape: a future relaxation of the child-of-a-child rule makes this loop unbounded and nothing says so at the loop. |

### The refusal vocabulary, engine half

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | thirty-odd `OrderRejected.reason` literals in `orders.rs` (`"unknown instrument"`, `"duplicate client_order_id"`, `"submit with non-positive quantity"`, `"quantity violates size increment"`, `"order notional exceeds maximum representable value"`, `"cash-settled futures require the margin ledger"`, `"a sell of this instrument reserves its base asset, which it ..."`, `insufficient {currency} balance`, ...) | refusal texts | every business refusal a client can receive | wire | 3 | **[extends P2's refusal-taxonomy verdict]** These are the venue's real order-entry documentation and they are hand-written literals with no shared vocabulary: some name a remedy, some name a rule, some name neither. `"unknown instrument"` appears twice (`validate_submit` and `on_modify`) as two independent literals. `"conditional orders cannot be immediate-or-cancel: a now-or-never order cannot wait for a trigger"` is the best of them and is the only one shaped like the server's remedy-shaped refusals. One taxonomy, in `reference/`, and constants for the ones stated twice. |
| - | `"client_order_id uses reserved liquidation prefix"` vs `RESERVED_ID_PREFIXES = ["LQ-", "RISK-"]` | refusal text, constant | the two venue-minted prefixes a client may not claim | wire, cross | 3 | The message says "liquidation prefix" (singular) while the constant holds two, one of which is a RISK FLATTEN and not a liquidation. Same shape as pass 2's lateral 8: the message is narrower than the constant it enforces, and the constant's own doc explains that `RISK-` was added precisely because it had been minted unreserved. Format the list into the message. |
| - | `"order group rejected whole: {blamed} was refused because {reason}"` | refusal text | the atomic-group refusal, one per member | wire | 3 | Good. Recorded because it is the only refusal on this surface that names a SIBLING'S id, which a client must parse to find the real cause. That is a machine-readable relationship rendered as prose. |
| - | `not_resting_reason` -> `"order already terminal (filled or canceled)"` / `"unknown order"`, shared by `silent_cancel_refusal` and `cancel_open_order_silently` | fn, refusal texts | why an id is not resting | cross | 3 | **[P4b changed] Defect in one otherwise good mechanism.** One wording serves both readers, but the wording is false for `Expired`, which `expire_orders` freezes through `record_closed` and which satisfies the only gate, `!status.is_open()`. Keep the shared helper and change the text to "order already terminal" or enumerate every terminal status from the type. The first-pass "No defect" verdict is refuted by its own later lateral finding. |
| - | `POST_ONLY_REFUSAL` used at three sites, but two of them spell the literal `"post-only order would take liquidity"` inline (`orders.rs` lines ~646 and ~1891 and ~2928) | refusal texts | the post-only refusal | wire | 3 | **[extends P1's `POST_ONLY_REFUSAL` row]** The protocol crate exports the constant precisely so the engine's admission-table test can parse it; three sites here write the string by hand instead. Two implementations of one contract, both green. Use the constant. |

### The fee, funding and settlement cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `FeeRate { BasisPoints, PerContract }`, `FeeSchedule { maker, taker }`, `set_fee_schedule`, `commission_for`, `maximum_commission` | types, methods | what a fill costs | cross, wire (`commission`) | 3 | Commission and fee are used interchangeably; the wire field is `commission`, the type is `FeeSchedule`, the divergence is `FeeSurcharge`. Three words. Maker/taker is inherited. |
| - | `MarginBasis { PerContract, Notional }` | type, variants | whether a margin figure is currency-per-contract or a fraction of notional | cross | 3 | Load-bearing enough that TWO separate defects are recorded in this crate's comments from reading the raw field instead of going through `policy.maintenance()`. The glossary's Posted margin entry mentions "the instrument's margin parameter" and never says it has two readings. Worth an entry: "10x leverage is `initial = 0.1` under `Notional`". |
| - | `UnsettledCredit`, `release_settled_cash`, `settles_at_ns`, `Account.unsettled` | type, method, field | T+N sale proceeds credited but unspendable | cross, wire (folded into `Balance.locked`) | 3 | **[extends P1 lateral 10]** Pass 1 noted that `Balance.locked` carries two economically different quantities. This is the second one, and here it is a separate `Vec` with its own release pass - the split EXISTS internally and is deliberately collapsed on the wire. The doc argues that is fine ("needs no new balance field for a consumer to understand"); a consumer cannot tell "cancel an order" from "wait two days". I think the doc is wrong and pass 1 is right. |
| - | `funding_instants`, `apply_funding`, "funding instant", epoch-multiple schedule | fn, method | how many eight-hour boundaries a span crossed | cross | 3 | The convention (instants on multiples of `interval_ns` FROM THE UNIX EPOCH, half-open with `from_ns` exclusive) is a real cross-pass invariant - "an instant is funded exactly once however the sweep passes are cut" - stated in one private function's doc. It is the same half-open convention `settle` uses and the two state it separately. |
| - | `position_unrealized` vs `position_unrealized_checked`, "UNDEFINED IS NOT OVERFLOW" | fns, doc prose | the one expression every unrealized reader uses, in saturating and refusing forms | cross | 3 | Excellent, and the doc records a real defect it fixed (a coin-margined book liquidated on a linear number). The distinction it draws - a valuation that saturated is a LIE, a margin number that saturated is a BOUND - is a genuine contract with no home. |

### Miscellaneous undefined nouns

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `EngineConfig::unbound`, `Engine::UNBOUND_ACCOUNT_ID = "UNBOUND"` | fn, constant | an engine with a placeholder identity, for tests and benches | cross | 3/1 | "Bound" is the glossary's word for a connection binding a river (`Connection`: "bound to one river at one speed"). Here "unbound" means "has no real account id", which is a different axis entirely, and the string `UNBOUND` can reach the wire as `AccountState.account_id` if anybody ships an `Engine::new()`. `PLACEHOLDER_ACCOUNT_ID` / `EngineConfig::placeholder`. |
| - | `enforce_funds` and the FUNDED / UNFUNDED account distinction | field, doc prose | whether submits are checked against free balance, decided once at construction from whether the balance seed was empty | cross | 3 | A whole account MODE, invisible from the wire, that changes whether an order is refused or the ledger goes negative. The glossary's Account entry says nothing about it, and `mogwai-venue` always funds - so the unfunded mode is a test-only behaviour reachable through a public constructor. Either name it in the glossary or make it explicit rather than inferred from an empty map. |
| - | `MarkOutcome { events, originated_orders }`, "venue-originated" | type, field, doc prose | how many orders the VENUE itself submitted in this batch, which the caller must attribute | cross | 3 | **[extends P3's delivery cluster]** Pass 3 found `Audience` and `claim_produced_orders` undefined; `originated_orders` is the number they consume. "Originated" is the engine's word, "produced" and "claimed" are the server's, for one act. Settle on one. |
| - | `apply_divergences: bool` threaded through `on_submit_from`, `push_account_snapshot`, `on_cancel`, `validate_submit` | parameter | FALSE for a venue-originated batch, so it cannot spend an arm the client aimed at its own fill | cross | 3 | A real and subtle rule - the venue's own acts must not burn the client's arms - carried as an unnamed bool through five functions. `Originator::Venue | Originator::Client` would make it unmisreadable and would also fix the `false, &[]` call sites, which currently read as nothing. |
| - | `Warned { missing_instrument, zero_px, saturated, unpriceable_settlement }` and the once-per-key warn discipline | type, fields | log-flood suppression for sticky conditions | local | 3 | Fine names. Recorded because the discipline ("saturation is sticky, so warn once per key") is a real operational contract for anyone reading venue logs and is stated four times in four field docs. |
| - | "clipped" / "saturated" / `add_clamped` / `mul_clamped` / `order_locked_clipped` | fns, fields, log text | Decimal-boundary saturation | cross, wire (operator log) | 3 | Three words - clip, clamp, saturate - for one condition, in one module, with the log line using the fourth phrasing ("saturated ... the stored value is clipped"). Pick one. |
| - | `dry_refusal` / "the dry pass" / "pass one" / "pass two" / "the closing pass" | fns, doc prose | the group admission mechanism's phases | cross | 3 | **[extends P3's sweep cluster]** "Pass" now names: a sweeper cycle, a per-passenger engine step, a boat's tape walk, and TWO of a group submit's three phases. Five granularities, one word, two crates. |
| - | "the seam" (`PendingScan` doc: "This is the whole seam - the engine never sees a tick") | doc prose | the engine/server division of labour | cross | 3 | The crate's own word for its central architectural fact, used once. `reference/architecture.md` should own it. |

---

## Inherited - quarantine, do not rename

| term | site | source |
|---|---|---|
| `AccountState`, `Balance`, `Position`, `OrderFilled`, `SubmitOrder`, `Command`, `ServerMessage`, `WireOrderStatus`, `OmsType`, `InstrumentDef`, `Symbol`, `ClientOrderId`, `VenueOrderId`, `SimClock`, `Hit`, `ScanKind`, `Contingency`, `LiquiditySide`, `TimeInForce`, `OrderType`, `Side` | throughout | `mogwai-protocol` / nautilus; passes 1-3's quarantine |
| `OmsType::Netting` / `Hedging` and the whole `position_id` keying | `position_key_id`, `apply_position` | nautilus |
| maintenance margin, initial margin, notional, VWAP, mark, unrealized/realized P and L, commission, maker/taker, drawdown, equity, liquidation, flatten, settlement, T+N, round lot, Reg-T, funding rate, basis points, coin-margined / inverse | throughout | universal derivatives and accounting vocabulary |
| "post-only", "reduce-only", "fill-or-kill", "immediate-or-cancel", "good-til-date", "day order", bracket / OCO / OTO / OUO | `orders.rs` | FIX / industry |
| `Decimal`, `saturating_*`, `checked_*`, `swap_remove`, `VecDeque` | throughout | Rust / `rust_decimal` |
| ChaCha8, `SeedableRng` | `orders.rs` | published algorithms |

Note `BreachAction` is NOT inherited from anywhere - both spellings are this
project's - which is why the collision in the headline is fixable.

---

## Lateral findings

Ordered by how much I think they matter.

**1. Two doc comments are attached to the wrong item in `account.rs`.** The
block beginning "The spendable amount of one currency: the booked total minus
every resting order's reservation ... (both derive from `locked_balances` with
clamped arithmetic)." runs straight into "The account's NET quantity in one
symbol ..." with no separator, so the whole `free_balance` doc is attached to
`net_position`, and `free_balance` - the function every funds check calls - has
NO doc at all. This is the identical defect pass 3 found twice in `run.rs`
(`evict_account`/`has_matching_identity_on` and `session_guard`/`fault_venue`). Three
instances now, in two crates, all reading like a misplaced merge, and rustdoc
renders every one of them happily. Nothing detects it. Worth a lint pass over
the whole workspace, not three individual fixes.

**2. [P4b changed] The fee surcharge is billed on the event's boat clock, but
the first pass's "same instant" conclusion is not established.** `fee_surcharge_multiplier_for`
opens the window at `max(sim.sim_ns(armed), sim.sim_epoch_ns)` on whichever
boat's clock the pass belongs to. An account with two sockets on two boats at
two speeds - which the glossary's Seat entry explicitly blesses - has one ledger
and one stored wall-arm-plus-duration record resolved on two axes. Each axis
receives the same simulated duration, including the late-boarder rule. Because
the two axes map wall time to different simulated timestamps, the first pass did
not identify a shared simulated instant at which the answers differ. What is
confirmed is that temporal membership is per boat even though the arm is stored
per ledger. The glossary must state that. Whether the product instead wants one
ledger-owned absolute axis remains an owner ruling, not a demonstrated double
price for one instant.

**3. `Engine::clear_armed` is public with no caller.** It is documented as "the
explicit escape hatch a harness calls between scenarios" and nothing in the
workspace calls it. Either a harness owes the call (in which case the
never-triggered targeted `PartialFillNext` leak the doc describes is live today
between scenarios) or the method is dead public surface. Cheap to check, and the
`MAX_ARMED_DIVERGENCES` doc rests on it being the remedy.

**4. `projected_qty`'s doc links `[Engine::worst_case_leaves]`, which is
`pub(crate)`.** `projected_qty` is public and the intra-doc link resolves to an
item an external reader cannot see. Same family as pass 1's lateral 3 (three doc
references to types that do not exist): if `broken_intra_doc_links` is not
enabled, nothing in this workspace catches a dangling or private doc link, and
this is a second, different way for that to bite - a link that resolves for the
compiler and 404s for the reader.

**5. The reserved-prefix admission is a genuine security-shaped contract with
one gap in the message.** `RESERVED_ID_PREFIXES` exists because a client
pre-claiming `LQ-...` could burn the id in `seen_client_order_ids` and make the
venue unable to liquidate it. The constant's doc says `RISK-` "was minted without
being reserved", i.e. the hole was live. The refusal message still says
"liquidation prefix" singular. A client refused for using `RISK-` is told it used
a liquidation prefix, which is false, and the fix is to format the constant into
the message. This is the same shape as pass 2's lateral 8.

**6. `not_resting_reason` says "(filled or canceled)" but `Expired` is also
stored terminal.** `expire_orders` calls `record_closed` with
`WireOrderStatus::Expired`, and `record_closed`'s only assertion is
`!status.is_open()`. So a client that queries a GTD order after its expiry and
then tries a silent cancel gets "order already terminal (filled or canceled)".
Narrow, wire-visible, and the parenthetical is the kind of doc-narrower-than-gate
prose `AGENTS.md` names.

**7. `Engine::new()` can put the literal `UNBOUND` on the wire.**
`account_snapshot` stamps `self.account_id`, and `EngineConfig::unbound` parses
`"UNBOUND"`. The doc argues production always builds a config by hand, which is
true of `mogwai-venue` today, but `new()` is public and un-deprecated and
`mogwai-lab` depends on this crate. A `#[cfg(test)]`-only constructor, or a type
that cannot be built without an id, removes the class.

**8. `set_margin_policy` rebuilds the whole reservation cache on every call.**
`rebuild_order_locked_excluding(None)` is an `O(open orders)` fold with a
per-order allocation, and the server calls the setter once per symbol at
account mint. For an account with many instruments that is quadratic in symbol
count at open. The doc explains why it rebuilds rather than teaching the setter
a second formula - correct reasoning - but a batch setter (`set_margin_policies`)
that rebuilds once would keep the reasoning and drop the quadratic.

**9. `MarketToLimit`'s known-broken behaviour (pass 1 lateral 11) is invisible
from this side.** The protocol crate's doc says the fill takes the whole quantity
at the order's own limit price with no reference to the tape, and that a
divergence-manufactured remainder rests INERT forever. `Resting::Inert`'s doc
here describes the mechanism without naming the order type, and nothing in this
crate flags the variant as unimplemented. Two halves of one known defect,
documented on two sides, neither pointing at the other.

**10. `liquidate_all` cancels resting orders one at a time through `on_cancel`
with `apply_divergences: false`, and `on_cancel` emits a wire frame per order.**
For a large book a risk breach produces one `OrderCanceled` per resting order
plus one close per position, in a single batch, and the byte reservation for
that batch is the server's problem. Recorded as a sizing question rather than a
defect: `sizing`'s `swept_fill_max_bytes` bounds a SWEEP pass, and this is not
one.

**11. `funding_instants` returns `0` when `interval_ns == 0` and `apply_funding`
also guards `interval_ns == 0` immediately before calling it.** Harmless
duplication, but the callee's guard is the one that makes the division safe and
the caller's reads as the guard. If the caller's is ever removed as redundant,
nothing changes; if the callee's is, the caller's does not protect the divide
inside. Keep the callee's; the caller's is the one that is actually redundant.

---

## What I would do with this

Four moves, in order.

1. **Rename one of the two `BreachAction`s, this week.** It is the only finding
   in four passes where two types in two crates share a name, share a domain,
   and have DISJOINT variant sets, so a reader who imports the wrong one gets a
   compile error at best and a wrong mental model at worst. The glossary
   sentence that defines the word describes only one of them. `MarginBreachAction`
   here, `risk::BreachAction` there, and one glossary entry naming both.

2. **Keep multi-river and rewrite the stale premises around its actual lifecycle.**
   **[P4b changed]** `retire_off_river` and `cancel_unreadable_orders` answer two
   different questions. The former is the glossary-prescribed return-from-freeze
   retirement and makes first-returning-symbol order economically significant;
   the latter cancels orders after their last live reader disappears while the
   account remains attached elsewhere. Preserve neither merely because it
   exists: decide both policies explicitly, but do not justify either with "an
   account is on at most one river" and do not report the live two-socket case as
   losing half its book.

3. **Give the scan seam and the fill band a durable home.** `PendingScan` /
   `ScanResult` / `Resting` / `band_ticks` / `fill_seed` are the crate's public
   constructor and sweep contract, they are `TAPE_PROTOCOL_VERSION`-bearing, and
   a caller cannot use them without reading four field docs. Passes 1, 2 and 3
   all converged on `reference/wire-vocabulary.md`; this is its third
   counterpart, and it should absorb the band, the frontier, the order-state
   vocabulary and the linkage verbs.

4. **Spend "reservation" once, and split `Balance.locked`.** Five nouns for one
   hold in one crate is the same shape as pass 3's "seat", and unlike seat it
   also reaches the wire, where the single `locked` number carries two
   economically opposite conditions with opposite remedies. Pre-1.0: the wire
   split is cheap and the internal rename is mechanical.
