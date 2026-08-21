# Glossary scope pass 3: the venue's internal domain model

Inventory of `mogwai-venue`'s internal model - the run and its passengers, the
boatyard and its boats and tickets, the rivers, the lanes and seats and the
delivery apparatus, the sweeper, the account lifecycle machinery, and the words
these use among themselves - measured against `reference/glossary.md`. Nothing
was edited but this file.

Scope split: `notes/glossary-scope-1.md` covers `mogwai-protocol`,
`notes/glossary-scope-2.md` covers this crate's externally visible surface
(routes, query structs, JSON bodies, statuses, refusal and log text). Rows there
are not re-derived here. Where a finding in one of those passes has an internal
half that changes the verdict, it is marked **[extends P2]** and says how.

**Independent reconciliation (2026-08-21).** The second inventory read the
glossary in full and traced the scoped code before opening this report. This
matters because the first report's rows are unusually plausible anchors. The
second pass treated confirmation and refutation symmetrically. Its annotations
use **[P2 ADDED]**, **[P2 CHANGED]**, **[P2 CONFIRMED]**, and **[P2 REFUTED]**;
refuted reasoning remains visible rather than being silently cleaned away.

Direction key: **1** a glossary term doing a job that is not that term's; **2** a
job the glossary already names, under a different word; **3** load-bearing and
undefined; **structural** a glossary claim about these objects that this code
falsifies; **inherited** vocabulary this project does not own.

Reach key: `local` crate-local; `cross` across a crate boundary; `wire`
externally visible.

---

## The headline

Four findings are worth more than the rest of the inventory combined, and three
of them are structural rather than nominal.

1. **"An account is on at most one river" is asserted in four places and is
   false.** Pass 2 found two of them and settled the naming half. The half it
   did not reach is that `extremes.rs` and `risk.rs` derive a CORRECTNESS
   ARGUMENT from the false premise: two prices per span are claimed to carry the
   whole tick-resolution answer BECAUSE equity is linear in the price of the one
   instrument the account can hold. A two-river account breaks the linearity and
   the sweeper judges it twice per interval, each time on one river's extremes
   and the other river's stale mark. This is a risk-enforcement defect, not a
   word.
2. **"Passenger" has two live meanings; the first pass's "about as often" is
   unsupported and unnecessary.** The named `Passenger` type and most run and
   sweeper uses consistently mean the account object. But the boatyard's
   `passengers` count and WebSocket duration prose use it for a connection or
   ticket, and the glossary's own `RunComplete` entry does too. One conflicting
   load-bearing use is enough to falsify "one per account, not per connection";
   no frequency claim is needed.
3. **"Seated" carries four unrelated senses**, two of them bound to local
   variables named `seated` inside one loop body in `sweeper.rs`.
4. **Generator havoc does not fork the river.** The glossary says a river's
   identity includes generator-level havoc and that generator arms "fork the
   river at placement rather than mutating shared water". `RiverKey` carries the
   symbol, the seeds, the boot regime and the resolved profile - and nothing an
   operator can arm at runtime. `Rivers::arm_flow_surge` mutates the shared
   checkpoint chain in place, under a refusal whose stated remedy names a
   mechanism that does not exist.

---

## Structural - glossary claims about these objects, checked against the code

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boat ("an implementation cache with no semantics of its own... nothing a client can measure reveals whether it shares a hull") | `http.rs` `AppState::river_now` | method | the history ceiling for a symbol is `max(published_ns)` over EVERY seated boat on that symbol, any speed, whoever placed it | wire | structural | **[P2 CHANGED] The claim is false, but the first-pass repair is not defined for HTTP.** `/trades` and `/quotes` have no requesting connection whose "own boat" can supply a ceiling. The worked reading holds: without `speed`, `river_now` takes every boat for the symbol and chooses the greatest `published_ns`, so another account boarding the same river at a faster cadence advances this caller's history ceiling. Make the ceiling river-owned and independent of boats, or expose and define a cadence selector on history. Do not pretend an HTTP poll owns a boat. |
| Boat (same claim) | `boatyard.rs` `boat_for_symbol` (min speed) vs `boats_for_symbol` + `max_by_key(published_ns)` in `river_now` | methods | two different tie-break rules over the same multi-boat set | local | structural | Two reducers over one set, one taking the SLOWEST and one the LEAD, each with a comment defending itself and neither aware of the other. The FlowSurge gate and `/health` take the slowest; the history ceiling takes the lead. Pick one rule and name it, or make each caller state which boat it means. |
| River ("a river's identity is everything that mutates the water - the resolved bundle, the seed, generator-level havoc") | `source.rs` `RiverKey::resolve`, `TapeIdentity` | method, type | the key hashes the symbol, the tape seed, the boot `MarketRegime`, the whole `InstrumentDef`, the scalars, the arrival config, the session curves and the calendar - and nothing armed after boot | local | structural | **[P2 CONFIRMED]** Only BOOT-TIME generator havoc (`TapeIdentity.regime`) is in the identity. `RiverKey::resolve` has no input representing a runtime arm. The glossary reads as though `FlowSurge` were too. Correct the entry to say the identity carries the run's regime, and state separately what a runtime generator arm does. |
| Divergence ("generator arms... fork the river at placement rather than mutating shared water") | `source.rs` `arm_flow_surge` / `clear_flow_surge` | methods | takes the river's `checkpoints` mutex and arms the surge ON THE EXISTING CHAIN | cross | structural | **[P2 CONFIRMED] Flatly false.** `arm_flow_surge` resolves the ordinary cached key, obtains the existing `Arc<River>`, locks `river.checkpoints`, and mutates that `CheckpointIndex`. It returns no new key, river or boat. There is no fork anywhere in the crate. What actually stands in for the fork is a refusal in `http.rs` when the river has a seated boat. The glossary describes a design that is not implemented. |
| Divergence / Boat | `http.rs` `arm_divergence`, `FlowSurge` arm: `if run.boatyard.boat_for_symbol(symbol).is_some()` then refuse `"place a boat whose sharing key carries generator havoc"` | refusal text + gate | the substitute for the fork | wire | structural | **[extends P2]** Pass 2 called this a client-vocabulary leak. The internal half is worse: the remedy is unperformable. A boat's sharing key is `(RiverKey, speed_micros)` and `RiverKey` cannot carry a runtime arm, so no request a client or operator can make produces the boat this sentence describes. Also see lateral 1: the gate is vacuous against a concurrent board. |
| Passenger ("one per account, not per connection") | `boatyard.rs` `Seat { boat, passengers: u32 }` | field | a count of live `Ticket`s, i.e. CONNECTIONS | local | structural | **[P2 CONFIRMED, frequency claim narrowed.]** `board` increments once per returned `Ticket`, and `Ticket::drop` decrements once, so one account with two sockets on one boat counts as two "passengers" here. The `Passenger` type itself remains account-scoped. Rename this refcount to `riders` or `tickets`, matching `Passenger::unsit`'s own `riders` binding. |
| Passenger (same) | `ws.rs` module doc `"A websocket passenger on one boat"`; `SocketQuery::duration_ms` doc `"from this passenger's boarding instant"`; `admission.rs` `ExecLanes::id` doc `"the dispatcher, the reader and the fault task are all the same passenger"` | doc prose | the CONNECTION | local | 1 | Three durable statements using the glossary's account-noun for the connection-noun the glossary already has (Connection). Rewrite all three to "connection". |
| Passenger (same) | `reference/glossary.md` RunComplete: `"passengers on one boat complete independently"` | doc prose | the CONNECTION | wire | 1 | **The glossary contradicts itself.** `duration_ms` is a `SocketQuery` field, so two connections of one account on one boat complete at different instants - which under the glossary's own Passenger entry is impossible. Fix the RunComplete entry, not the code. |
| Freeze ("the state of an account whose last connection went away") | `run.rs` `Run::passenger`, `Run::open_account` | constructors | a newly created account starts with `frozen_since = Some(now)` before it has ever had a connection | local | structural | **[P2 ADDED] The glossary is too narrow.** This is deliberate and load-bearing: a POSTed account nobody ever connects to must be TTL-collectable, and create-on-first-sight accounts share the same initial state. Define Freeze as the state in which no connection is reading the account, including before the first connection, rather than only a transition after the last one leaves. |
| Seat ("an account holds as many seats as the distinct boats its sockets have bound") | `run.rs` `Passenger::seated_on`, `try_sit`, `unsit`, `is_seated_on` | field, methods | exactly that, counted per connection, vacated by the last rider | local | - | **No defect. This is the model implementation of the entry**, comments included. Every other use of "seat"/"seated" in the crate should be measured against this one. |
| Run ("one foreground mogwai process, many ledgers") | `run.rs` module doc: `"State owned by one venue process: one ledger, and keyed paced boats over many rivers"` | doc prose | claims one ledger per run | local | 1 | **[extends P2]** Pass 2 found this premise in `config.rs` and `serve.rs`. It is also the FIRST LINE of the module that owns the `passengers: HashMap<String, Arc<Passenger>>` map. Same fix, third site. |
| Seat / Ledger | `risk.rs` module doc; `extremes.rs` module doc; `admission.rs` `CLOSE_EVICTED` doc - all three assert some form of `"an account is on at most one river"` | doc prose | the single-instrument premise | local | structural | Four sites total with pass 2's `risk.rs` row. See lateral 2: in `extremes.rs` the premise is not decoration, it is the proof obligation for the whole two-prices-per-span design. |

---

## Direction 1 - a glossary term doing a job that is not its own

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Seat | `boatyard.rs` `enum Slot { Placing, Seated(Seat) }`, `struct Seat { boat, passengers }` | type, variant | A BOAT EXISTS IN THE YARD and how many tickets hold it | local | 1 | Nothing here is an account's seat. This is the boat's own registry entry. `Slot::Placed(Berth { boat, riders })` says it; anything says it better than reusing the account word. |
| Seat | `boatyard.rs` `seated_symbols`, `boats_for_symbol` doc `"Every seated boat"`, `boat_at` doc `"if one is seated"` | methods, doc prose | boat EXISTENCE | local, wire (the string reaches a refusal) | 1 | Same sense. "Placed" is the verb `board` already implies and is free of the account meaning. |
| Seat | `run.rs` `Run::seat(account_id, claimed, session, resetting)` | method | evict whoever else holds the id, optionally discard the ledger, then return the passenger - IT TAKES NO SEAT AT ALL | local | 1 | **The worst name in the crate.** The real seat is taken by `try_sit` at two other places in `ws_upgrade`, one of them BEFORE this call and one AFTER. A reader tracing "where is the seat taken" lands here and is wrong. Rename to `claim_account` or `resolve_for_connection`. |
| Seat | `run.rs` `seat_discards_ledger` | method | whether the CLAIM will reset the ledger | local | 1 | Follows `seat`. Rename with it (`claim_discards_ledger`). |
| Seat / seated | `run.rs` `Run::passenger`: `if let Some(seated) = passengers.get(...)`, `let seated = Arc::new(Passenger {...})`, `arms.all.open_transport(&seated)` | local bindings | A PASSENGER EXISTS IN THE MAP | local | 1 | Third sense, and the value is BORN FROZEN AND HOLDS NO SEAT four lines below where it is called `seated`. Rename to `passenger`. |
| Seat / seated | `run.rs` docs: `"every seated ledger"`, `"a seated one"`, `"ledgers that were already seated"`, `"one that was seated when the arm arrived"`, `"a seated account"` (log), `"the seated set"` | doc prose, log text | a ledger EXISTS in the passenger map | local, wire (one log line) | 1 | Same third sense, roughly a dozen sites in `clear_venue_arms`, `arm` and their tests. The word wanted is "existing", "minted" or "open". |
| Seat / seated | `sweeper.rs`: `let seated: HashSet<BoatKey> = boats.iter()...` and, ~50 lines later in the same loop body, `let seated: Vec<Arc<Passenger>> = ...filter(|p| !p.is_frozen())` | local bindings | the live BOAT set, then the attached PASSENGER set | local | 1 | **Two different senses shadowed under one name in one scope.** The second shadows the first for the rest of the pass, and `seated[*index].is_seated_on(&boat_key)` on the next screen reads as a set membership test and is an index into the other one. Rename to `live_boats` and `attached`. |
| Seat / seated | `run.rs` `"The VENUE clock, and not the now of any seated river"`; `http.rs` `"A seated river is SKIPPED"`, `"a fault on any seated river"` | doc prose | a river THAT HAS A BOAT | local, wire | 1 | Fourth sense: rivers do not sit, boats do. "A river carrying a boat" / "a boated river" / "a read river". |
| Passenger | `ws.rs` `SocketSession.passenger` field doc: `"The ledger this connection trades on"` | field + doc | correct object, and the doc calls it a ledger | local | 1 | Minor but symptomatic: the field is a `Passenger`, the doc calls it a Ledger, and the glossary distinguishes them (a Passenger HAS a ledger). One word either way. |
| Ticket | `boatyard.rs` `Ticket { yard, boat }` vs `admission.rs` `Ticket { _permit }` | two types, one crate | a boat's rider handle; a priority-lane frame slot | local | 1 | The glossary names only the boatyard sense ("the tickets that keep them alive"). Two `Ticket` types in one crate, both `pub(crate)`, imported into `ws.rs` side by side. Rename the admission one to `FrameSlot` - `OutboundFrame.slot` already calls it that. |
| Ticket | `admission.rs` `ADMISSION_PROMISE_TICKETS` | constant | outstanding PROMISES of a future priority frame | wire (config key) | 1 | Third sense of ticket, and the only one an operator reads. `promise_slots`. |
| Session | `ws.rs` `SocketSession`, `Run::sessions_tx` / `session_guard` / `sessions_drained`, `serve.rs` `serve_until_drained` | types, fields, methods | ONE WEBSOCKET CONNECTION | cross | 1 | **[extends P2]** Pass 2 named the collision from the `SocketQuery::session` side. Internally it is larger than one struct: the whole shutdown-drain vocabulary is built on the connection sense while `SocketSession.presented_identity` carries the glossary sense in the same value. `conns_tx` / `conns_drained` / `SocketConn`. |
| Freeze | `run.rs` `frozen_for` doc `"how long this account has been unattended"`, `freeze_if_unattended`, `collect_expired_accounts` doc `"UNATTENDED IS THE SAME TWO-PART QUESTION"` | methods, doc prose | the freeze state | local | 1 | **[extends P2]** Pass 2 found "unattended" in `ReadyRecord` and `account_ttl_ms`. Internally it is the PRIMARY word - three method names and a capitalized doc heading - and "freeze" is the secondary one. Whichever wins, the two-part predicate (no lane AND no admission) deserves the definition, because that is the load-bearing part and neither word carries it. |
| Ledger | `run.rs` `LedgerTemplate` | type | opening balances, fill seed, oms type, liquidation band | local | 1 | Not a ledger and not a template of one: a ledger is a `mogwai-engine` instance and this is the four venue-wide settings every engine is built from. Its own doc admits the drift ("the same value doing a different job"). `EngineDefaults` or `AccountOpeningTerms`. |
| Ledger | `run.rs` `unopened_ledger` | method | a throwaway `Engine` built to answer a read | local | 2/1 | Honest name for what it returns, but it returns an Engine and the crate's word for an Engine-per-account is Ledger via Passenger. Keep, but note the trio `passenger` / `peek_passenger` / `unopened_ledger` are three return shapes over one concept. |
| Client | `run.rs` `has_matching_identity_on`, `evict_account`'s `same_client` closure, `"a ledger is never read from two clients at once"` | method, closure, close text | the SESSION STRING | local, wire | - | Correct per the glossary's Client entry (the counterparty process, identified only by its session id). No defect; recorded because it is the one place the overloaded word is used precisely. |
| Client | `ws.rs` `ws_upgrade` comment `"a ledger is never read from two sockets at once"` | comment | contradicts the close reason two files away, which says "two clients" | local | 1 | One client's two sockets DO read one ledger - that is the entire point of `session`. The comment states the opposite of the behaviour it sits above. Fix the comment. |

---

## Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boat | `source.rs` `place_cursor`, `BoatCursor`; `sweeper.rs` `"the clock is a property of the cursor"`, `"a cursor may have been torn down"`; `ws.rs` `"the newcomer's binding cursor"` | fn, type, doc prose | the boat's paced reader, and by extension the boat | local | 2 | "Cursor" is the crate's second word for the paced reading of a river. Sometimes it is the `Box<dyn TickSource>` inside the boat and sometimes it IS the boat ("the departed cursor" in `an_eviction_reconnect_rebases_a_frontier_from_the_departed_cursor`). Keep Cursor for the `TickSource` and never for the boat, or drop it. |
| Boatyard | `boatyard.rs` `Boatyard::board` / `Ticket` / `Slot::Placing` vs `source.rs` `place_cursor` | methods | "place" is used for placing a boat AND for constructing the cursor a boat is placed with | local | 2 | Two verbs, `board` and `place`, for one act, split across two crates' worth of module boundary. The glossary uses "placed" for the boat and "boards" for the socket, which is the right split; `place_cursor` is the odd one out. |
| Freeze | `run.rs` `Passenger::attach`, `Run::resume` | methods | clearing the freeze | local | 2 | Two verbs for the un-freeze (`attach` sets the stamp to `None`; `resume` calls it and does two more things). Nothing is named `thaw` or `unfreeze`, so the inverse of the glossary's Freeze has no name. The test `an_account_freezes_when_its_last_connection_goes_and_thaws_when_one_returns` invents a fourth. |
| Warmup / data origin | `run.rs` `Run::data_origin_ns()` returning `source::TAPE_ORIGIN_NS` | method | one constant, wrapped under the wire spelling | local | 2 | **[extends P2]** Pass 2 asked for the constant to be renamed. The internal half is this method, which exists ONLY to translate one name to the other and takes `&self` while reading a `const`. Renaming the constant deletes the method. |
| Tape | `boatyard.rs` `fanout_depth`; `tape.rs` `"its bounded broadcast fanout"` | field, doc prose | the tape's broadcast ring depth | wire (config key) | 2 | **[extends P2]** Same rename pass 2 asked for; recorded because the word also lives on the `Boatyard` and `TapeSpawn` structs, so the rename is four sites and not one. |
| Ledger | `sweeper.rs` `commit_pass`, `apply_engine_pass`, `apply_engine_pass_on_clock`, `deliver_produced` | fns | one engine-mutating step per passenger | local | - | No defect. Recorded because "pass" is used consistently for both the sweeper's whole cycle and the per-passenger engine step, which is a real ambiguity in an otherwise disciplined module. |

---

## Direction 3 - load-bearing and undefined

The clusters matter more than the rows.

### The delivery cluster - lane, bound lane, audience, claim, ownership

The venue's whole invisibility property lives in these five words and the
glossary defines none of them. Its Passenger entry ASSERTS the property
("invisibility, which attribution and per-account ledgers give") without naming
a single mechanism, so a reader who wants to check the claim has nowhere to
start.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `admission.rs` `ExecLanes`, held lane, priority lane; `run.rs` `BoundLane`, `bind_lanes`, `release_lanes`, `bound_lanes`, `locked_lanes` | types, methods | one connection's outbound machinery, and the run's table of them | cross, wire (config keys) | 3 | **[extends P1/P2]** Both prior passes flagged Lane as undefined. Internally it is the single most load-bearing undefined noun in the crate: the lane table is what `freeze_if_unattended` reads, what `evict_account` prunes, and what delivery walks. It owes a glossary entry stating that a lane is per CONNECTION while a claim is per ACCOUNT. |
| - | `run.rs` `Audience` and its five variants (`Venue`, `Account`, `Order`, `Unattributable`, `Requester`) | enum | who a swept frame is for | local | 3 | An exhaustive classification with an excellent doc explaining why it has no catch-all, and it is invisible from any durable document. This IS the invisibility property, expressed as a type. It belongs in `reference/`, and "audience" is a good word - adopt it. |
| - | `run.rs` `order_owners`, `claim_order`, `order_owner`, `track_ownership`, `claim_produced_orders`, `scope_query_rows` | field, methods | which ACCOUNT owns each live order | local | 3 | Six names for one table and its two claiming paths (dispatcher claims at acceptance, sweep claims at production). "Claim" also means an account-id claim in `Run::seat` and `Admission`'s doc ("one socket's claim to be reading an account"), so the word carries two jobs. Split them: an order is ATTRIBUTED, an account is CLAIMED. |
| - | `run.rs` `Audience::Order` doc: `"An order the table does not know is venue-originated (a liquidation) and goes to everyone"` | doc prose | the fallback | local | 3 | **Stale and contradicts its own file.** `claim_produced_orders`, twenty lines away, says venue-originated liquidations ARE claimed at production and that an unclaimed order "is therefore a BUG in whoever built the batch"; `scope_query_rows` says the same. The `Audience::Order` doc still describes the pre-fix world. This is the vacuous-gate prose shape: a comment describing a behaviour the function no longer has. |

### The admission cluster, internal half

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `run.rs` `Run::admit`, `Run::depart`, `Admission`, `Passenger::admitted` / `admissions` / `admit` / `depart` | methods, type, field | AN ATTACH REFCOUNT on an account | cross | 3 | **[extends P2, and I agree with its verdict]** Pass 2 called for `Run::attach` / `Attachment`. The internal half strengthens it: the crate ALREADY uses "attach" for this concept in three places - `Passenger::attach()`, the `resume` doc ("Attach an account to the river"), and `freeze_if_unattended` - so the rename is toward a word the module already speaks, not away from one. `Passenger::attach()` would need a different name (`thaw`). |
| - | `admission.rs` `ByteBudget`, `Reservation`, `HeldCharge`, `FrameBudget`, `Ticket`, `AdmissionLimits` | types | the outbound budget apparatus | wire (two config keys) | 3 | Six types, five nouns (budget, reservation, charge, ticket, slot) for one accounting scheme, and `OutboundFrame` names the same object `slot` that `FrameBudget` mints as `Ticket`. Settle on budget / reservation / charge / slot and delete "ticket" from this module. |
| - | `admission.rs` `held` vs `priority` lane | doc prose, fields | the two outbound lanes | local | 3 | "Held" names the lane by what `DelayAcks` does to it, not by what it carries (engine output). A connection with no `DelayAcks` armed still has a "held" lane holding nothing. `exec` / `priority` is the honest pair. |

### The sweep cluster - pass, walk, scan, frontier, mark, due, readable

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `sweeper.rs` `'passes` loop, `commit_pass`, `apply_engine_pass`; `fills.rs` `scan_triggers`, `SWEEP_DRAIN_BUDGET` | labels, fns, constants | one sweeper cycle; one boat's tape walk; one order's trigger scan | local | 3 | Three nested granularities, all called some form of pass/walk/scan, and the nesting is not visible from any name. A "pass" contains N boat "walks", each of which resolves M "scans". Say so once, in `reference/`. |
| - | `sweeper.rs` `frontier_after`, `last_swept_ns`, `scanned_to_ns`, `from_ns`, `to_ns`, `rebase_scans` / `rebase_future_scans` | fns, fields | the per-boat watermark over swept tape | cross | 3 | The frontier is the defect family `AGENTS.md` names first and it has no glossary entry. `Boat.last_swept_ns` is the watermark, `frontier_after` is the guarded advance, and `rebase_scans` is the reconnect repair - three names, one invariant, and the invariant is stated only in `frontier_after`'s body. |
| - | `sweeper.rs` `readable` (symbols with a boat), `Engine::cancel_unreadable_orders` | binding, method | an order on a river nobody is reading is cancelled | cross | 3 | A REAL CONSUMER CONTRACT - an attached account's resting order on a boatless river is cancelled by the venue - stated in a comment inside a loop and nowhere else. A client cannot discover it. Owed a glossary line under Seat or Connection. |
| - | `sweeper.rs` `next_due`, `due_boats`, `MIN_SWEEP_WALL` | bindings, constant | the earliest-deadline schedule over boats | local | 3 | Fine names; recorded because per-boat cadence is a consequence of the Boat entry's "two boats at two speeds" and the glossary never says the sweeper is per boat. One sentence in the Boat entry. |
| - | `extremes.rs` `PriceExtremes`, `PriceSpan`, `SpanWriter`, "span" | module, types | the high and low one river reached between two sweep passes | local | 3 | "Span" here is a tape interval; `HavocWindow`'s `ArmedSpan.sim_span_ns` is a duration; `MaterializeRefusal::Reach` and `MAX_WARMUP_MATERIALIZATION_TICKS` call a tape interval a "reach". Three words for the interval concept in one crate. |

### The havoc-arming cluster, internal half

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `run.rs` `VenueArm`, `ArmRecord`, `VenueArms { all, pending }`, `HavocWindow`, `apply_transport_arm`, `record_pending` / `take_pending` | types, fns | where a divergence lives before and after it fires | cross | 3 | **[extends P2]** Pass 2 flagged the taxonomy conflict from the routing site. The internal half adds a whole lifecycle the glossary has no words for: an arm is RECORDED (venue-wide or pending-per-name), REPLAYED onto a ledger at mint, and OPENED on a reader's own clock. "Pending" is doing especially heavy lifting - an arm for an account that does not exist yet, shed from the oldest end at 64. |
| - | `run.rs` `HavocWindow::open_at` and THE LATE-BOARDER RULE | method, doc prose | a window armed on the wall clock, judged on each reader's sim clock, opening at `max(projected, epoch)` | local | 3 | One of the best-reasoned pieces of code in the crate and the rule exists only in this doc comment. It is also the rule `arm_divergence`'s FlowSurge branch open-codes at `run.started_ns`. Two implementations of one rule, no shared anything - the shape `AGENTS.md` says to anchor. |
| - | `run.rs` `LedgerTemplate` + `template_engine` + `open_engine` / `open_transport` | type, fns | the three-way split of what an arm can be applied to | local | 3 | "Open" here means "apply an arm record to", which collides with account OPENING (`POST /accounts`, `AccountRefusal::AlreadyOpen`, `open_account`) in the same file. `apply_to_engine` / `apply_to_transport`. |

### The account-lifecycle cluster, internal half

Pass 2 counted eleven verbs from the outside. From the inside the count is
higher and two of them are actively misleading.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `run.rs` `reopen` | method | **discards the account's ledger**, and its own doc says so | local | 3 | The name says the opposite of the body. `reopen` calls `discard_account` and nothing else. Its one caller is the `resetting` branch of `seat`. Delete it and call `discard_account` directly, or name it `reset_account`. |
| - | `run.rs` `discard_account` vs `collect_expired_accounts` vs `reopen` vs `seat_discards_ledger` | methods | three triggers for one destruction | cross | 3 | **[extends P2]** Pass 2 asked for these to be defined. Internally they all funnel into `discard_account`, so the concept is single and only the triggers differ: the reset knob, the TTL, and nothing else. Say that: one verb, three triggers. |
| - | `run.rs` `Passenger` born FROZEN (`frozen_since: Mutex::new(Some(now))`) | construction | an account exists before anything reads it | local | 3 | Load-bearing (it is what makes a POSTed-and-abandoned account TTL-collectable) and stated only in an inline comment. The glossary's Freeze entry says a frozen account is one "whose last connection went away", which does not cover an account that never had one. |
| - | `run.rs` `resolve_policy` steps 1-3, `is_unpoliced`, "policed" | method, doc prose | the total three-step policy resolution | wire | 3 | **[extends P2]** The internal half is that this MIRRORS symbol resolution deliberately ("Resolve a risk policy the way a symbol resolves: total, three steps, step three never fails") and the glossary describes neither resolution as a three-step total function. One shared shape, two undefined instances. |

---

## Inherited - quarantine, do not rename

| term | site | source |
|---|---|---|
| `Engine`, `EngineConfig`, `PendingScan`, `ScanResult`, `book_shape`, `valuation_symbols`, `projected_qty` | `run.rs`, `sweeper.rs` | `mogwai-engine`'s own API |
| `AccountId`, `VenueOrderId`, `ServerMessage`, `Command`, `SimClock`, `OmsType`, `InstrumentDef`, `Symbol`, `CommandClass` | throughout | `mogwai-protocol` / nautilus; pass 1's quarantine |
| `TickSource`, `TickEvent`, `TickFault`, `GeneratedSource`, `MergeSource`, `CheckpointIndex` | `source.rs`, `tape.rs` | `mogwai-data` |
| Semaphore, permit, `forget`, RAII guard, watch/broadcast/mpsc channel, `spawn_blocking` | `admission.rs`, `boatyard.rs`, `tape.rs` | tokio / Rust |
| mark, VWAP, settlement, maintenance/initial margin, funding index, liquidation, drawdown, equity | `sweeper.rs`, `risk.rs` | universal trading vocabulary |
| watermark, frontier, cursor as CONCEPTS | `sweeper.rs` | general systems vocabulary - the mogwai-specific INSTANCES are rows above |

---

## Lateral findings

Ordered by how much I think they matter.

**1. The FlowSurge boat gate is vacuous against a concurrent board.**
`arm_divergence` refuses when `run.boatyard.boat_for_symbol(symbol).is_some()` -
the NON-awaiting form, which reads `Slot::Placing` as absence. `boatyard.rs`
documents this exact hazard on the awaiting variant ("`Slot::Placing` is not an
absence") and `river_now` uses the awaiting form for precisely this reason. So a
`POST /control/divergence` racing a `/ws` upgrade arms a generator surge onto a
river whose boat is mid-placement, which is the one thing the gate exists to
prevent, and the surge then mutates water a live tape is already reading. Both
halves are green. Use `boat_for_symbol_awaiting_placement`, or take the yard lock
across the arm.

**2. The two-prices-per-span risk derivation rests on a premise the venue
violates.** `extremes.rs` argues that recording only the high and low of a span
reproduces per-tick evaluation exactly, BECAUSE "equity is LINEAR in the price of
the one instrument an account can be holding - an account is on at most one
river". `ws_upgrade` supports an account riding many rivers (it refuses only a
second SPEED of the same river), and the sweeper calls `enforce_policy` once per
DUE BOAT with `span` carrying only that boat's river's extremes and `marks`
carrying the others' last reads. For a two-river account:
 - the trailing-stop argument still holds per symbol (monotone in that symbol's
   tape), but
 - the peak-equity argument does not: equity is a sum over two prices, its
   extreme over a span need not occur at either river's extreme, and the ratchet
   is fed a partial reconstruction whose ORDERING across rivers is arbitrary
   (whichever boat came due first).
This is a correctness finding, not a naming one. It is also the reason I would
push hardest for the glossary and the code to agree on multi-river accounts
before anything else here: three modules currently derive behaviour from the
single-river premise. If the owner rules that accounts really are single-river,
`try_sit` should ENFORCE it and the refusal should say so; if multi-river stays,
the peak-equity path owes a per-account extremes reconstruction.

**3. [P2 CONFIRMED] `terminated && seated.len() == 1` does not mean "the venue's only
account".** The comment above it says it does, twice, and reasons carefully about
shared-exchange behaviour. But `seated` at that point is the ATTACHED passengers
- `run.passengers()` filtered by `!is_frozen()`. A venue with two accounts, one
of which momentarily has no socket, terminates the WHOLE RUN when the other
breaches. This is a vacuous-gate instance of the "doc describes a gate wider or
narrower than the gate" shape, and the blast radius is the entire process.
`run.passengers().len() == 1` is the check the comment describes.

**4. [P2 CONFIRMED] Two doc comments are attached to the wrong item in `run.rs`.**
`evict_account`'s entire doc block - four paragraphs ending "Returns how many
were displaced, so the caller can say so" - runs straight into `has_matching_identity_on`'s
doc with no separator, so `has_matching_identity_on` carries it and `evict_account` (the
most consequential method in the file, and the one both the glossary's Eviction
entry and `CloseSpec::evicted` cite) has NO doc at all. The same defect repeats
at `session_guard`, whose doc is attached to `fault_venue`. Both read as a
misplaced merge. Nothing detects this - rustdoc renders both happily.

**5. `ADMISSION_PROMISE_TICKETS`' doc is a garbled sentence.** "64 healthy
replays would leave zero capacity for any actual refusal would leave a
connection whose priority lane is completely empty unable to state why it is
closing." Two sentences spliced. The reasoning is recoverable but the constant
guarding the venue's ability to say why it is closing deserves a readable one.

**6. `Run::seat`'s abandoned-seat path is documented as harmless by
unreachability.** In `ws_upgrade`, when the ledger moves out from under the
pre-seat check, the comment says the seat taken on `existing` "is left behind
deliberately... harmless because the ledger holding it is unreachable, not
because anything releases it". That is true today because `discard_account`
removes the map entry and the `Arc` dies with the request. It is exactly the
frontier-family shape in reverse: a write whose cleanup depends on a reachability
argument rather than on a guard. `SocketSession`'s `Drop` releases the seat for
every OTHER path; this one has no owner. A `SeatGuard` mirroring `Admission`
would make the window unrepresentable, which is the argument `Run::admit`'s own
doc makes for itself.

**7. `boat_for_symbol` and `river_now` disagree about which boat represents a
multi-cadence river** (slowest vs lead). Neither is wrong in isolation; together
they mean `/health`'s fault report, the FlowSurge gate and the history ceiling
are answering about different boats on the same river. If the boat set is to stay
observable at all (see the structural section), one reducer should serve all
three.

**8. `Passenger::depart`'s `debug_assert` cannot bite in the release sweep.**
Deliberate and documented - a release abort of the serving path would be worse.
Recorded only because `AGENTS.md`'s lane/profile note makes this the exact case
where a reader should check: the invariant "every admission has exactly one
departure" is asserted in dev and silently absorbed in release, and the saturating
loop below is what production actually gets. That is the right call; it is worth
one line in the doc saying the release behaviour is the floor, not the check.

**9. `LedgerTemplate.opening_balances` is cloned on every account mint.** Small,
but it is a `HashMap<String, Decimal>` cloned per passenger and per
`unopened_ledger` preview - and `unopened_ledger` runs on an UNAUTHENTICATED
`GET /account?account=<anything>`. An `Arc<HashMap<..>>` costs nothing and removes
a per-request allocation an unauthenticated caller controls the frequency of.

**10. `MAX_SPEED_MICROS` is documented as "one million times real time" and is
`1_000_000_000_000`.** The constant is in micro-multiples, so `1e12 / 1e6 = 1e6`
- the doc is right and the error message divides correctly. Recorded as a
non-finding so the next reader does not re-derive it; the value reads wrong at a
glance and the doc is the only thing that rescues it.

---

## What I would do with this

Three moves, in order.

1. **Settle the multi-river question, then propagate.** It is not a naming
   decision: `extremes.rs`'s correctness argument, `risk.rs`'s `MaxPosition`
   scalar, and `CLOSE_EVICTED`'s doc all rest on a premise `ws_upgrade`
   contradicts. Everything else in this report can wait behind it. My read is
   that multi-river is the real shape (the glossary, `seated_on` and
   `try_sit` are all built for it) and the three single-river derivations are the
   defects.

2. **Spend "seat" once.** Four senses, one of which is a method that takes no
   seat, and two of which shadow each other inside one loop. Keep the glossary's
   sense for `Passenger::seated_on` and its methods; rename `boatyard::Seat` to a
   berth, `Run::seat` to a claim, the passenger-map senses to "existing", and the
   river sense to "boated". This is mechanical and it is the single largest
   readability win available in the crate.

3. **Give the delivery apparatus a durable home.** `Audience`, the lane table and
   the order-ownership table are the invisibility property. The glossary ASSERTS
   the property in one clause of the Passenger entry and defines none of the
   three mechanisms, so nobody can check the claim. `Audience`'s own doc is
   already most of the document that is owed - it just lives where only a
   compiler reads it. Pass 1 and pass 2 both converged on
   `reference/wire-vocabulary.md`; this is its internal counterpart, and it
   should absorb the frontier vocabulary too, since `AGENTS.md` names the
   frontier family first among the defect families and the crate's own frontier
   has no definition anywhere.
