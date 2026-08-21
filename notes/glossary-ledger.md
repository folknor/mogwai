# The glossary ledger

The merge of eight scope inventories, two passes each, organised by term family
rather than by scope. This is the document phase 4 executes from and phase 3
rules against. The scope reports stay as provenance; nothing below re-derives
them, and where two scopes reached one finding from different sides that
agreement is recorded, because it is evidence.

Procedure and rulings: `notes/glossary-reconciliation.md`. Sources:
`notes/glossary-scope-1.md` through `-8.md`.

## How to read this

Five rulings are already made and are not re-opened here; they appear only where
a family's work depends on one.

Everything else sorts into six kinds:

- **Collisions** - one word doing several jobs. Ranked by blast radius.
- **Orphans** - one job wearing several words.
- **Undefined vocabularies** - whole contract vocabularies with no durable home.
  This is the largest category and it is not a rename problem.
- **Durable contradictions** - two binding documents that disagree, so neither
  can be cited. These are the most urgent thing in the ledger.
- **The roadmap** - entries the code has not caught up with. The glossary is the
  end state, so these are gaps the code owes.
- **Inherited** - vocabulary this project does not own. Never renamed.

A row's cost is dominated by its reach. A word an operator types or a consumer
parses is expensive to move and expensive to leave wrong; a crate-local
identifier is neither.

## What the arc actually found

The arc was scoped to reconcile names. Names are the smaller half of what came
back. Three things outrank the renames:

1. **The durable corpus contradicts itself.** `reference/architecture.md` states
   a rule and its negation about 500 lines apart, says the venue serves four
   order types 350 lines after saying it serves nine, and says two instrument
   classes are selectable after stating there are five. `docs/havoc.md` names an
   endpoint that does not exist, twice. Nine contradiction clusters in total,
   every one of them between documents whose whole purpose is to be citable.
2. **A premise the owner retired on 2026-08-20 is still load-bearing in nine
   places**, including the exactness proof for tick-resolution risk evaluation
   and one method that flattens positions.
3. **The two crates that make the water share no noun with the six glossary
   entries that describe it.** `river` appears once in `mogwai-data` and
   `mogwai-lab` combined; `boat`, `boarding`, `passenger`, `boatyard` and `seat`
   appear zero times. The generator's word for a river is `realization`.

---

# A. Ruled, and not re-opened

| ruling | what it settles |
|---|---|
| `venue` beats `server` for the process | `server` survives only as the name of Server mode |
| `consumer` for the counterparty, as a classification | `client` is retired for everything this project owns |
| `callsign` for the socket identity, wire parameter following | `session` is freed for the trading day |
| a passenger is one per account | duration and tickets move onto Connection and Seat; boarding is per connection |
| `FeedLagged` is advisory | the venue reports the gap and keeps serving |

Grounds are in the carry-forward. Two of them overturned a coherent agent
position, which is why the grounds are recorded and not only the verdict.

---

# B. Collisions, ranked by blast radius

## B1. `session` - seven jobs, ruled

Ruled: the socket identity becomes `callsign`, the wire parameter follows, and
`session` is left to the trading day. Recorded here because the sweep is larger
than the ruling: the word is doing seven jobs, and only two of them move.

| sense | where | disposition |
|---|---|---|
| the socket identity | `/ws?session=`, `SocketQuery::session`, adapter `process_session_id` | becomes `callsign` |
| a socket's served tenure | `SocketSession`, `sessions_drained`, `session_guard`, `serve_until_drained` | ours, internal, needs its own word - candidate: `conn` |
| the trading day | `session-profile`, `[instrument.session]`, ~63 operator-visible sites | keeps `session` |
| the CME trade date | `mogwai-lab::session`, `SessionSegment`, `assign_session` | inherited exchange vocabulary, quarantined |
| a named slice of a trade date | `mogwai-lab::segments::SessionWindow`, `ASIA`/`LONDON`/... | ours; candidate `TradingWindow` |
| the weekly intensity curve | `SessionProfile`, `SessionModulator` | ours; candidate `IntensityProfile` |
| the risk reset instant | `LockUntilReset` doc "next session boundary", `DailyLossLimit` | wrong on its face - it is `reset_minute_utc`, an account clock |

The last one is the dangerous one: three clocks under one bare word, so a reader
can implement the wrong one. Never leave `session` bare in a doc.

## B2. `client` - eight jobs, ruled as a classification

Ruled. Recorded here because the classification grew two senses after the
ruling, both found in the adapter.

| sense | where | disposition |
|---|---|---|
| the driving program | pervasive in prose, ~99 sites in the durable corpus alone | becomes `consumer` |
| nautilus's adapter objects | `MogwaiDataClient`, `MogwaiExecutionClient` | inherited, quarantined |
| the submitting side's id namespace | `client_order_id` | inherited from nautilus and FIX |
| the adapter's own inbound havoc | `ClientHavoc`, `HavocSpec.client`, `HavocFilter::from_client` | not the consumer sense; `inbound_havoc` |
| any consumer-supplied echoed id | `MAX_CLIENT_ID_LEN`, `truncate_client_id` | `MAX_ECHOED_ID_LEN` |
| **the socket leg** | `tracing::warn!(client = label)` where label is `"data"`/`"exec"` | new; resolves to `socket`, which `lifecycle.rs` already uses for the same value |
| **the transport generation** | adapter reconnect vocabulary | new; a real distinct concept, see D14 |
| one socket's view | `reference/clock.md`'s backlog argument, several `architecture.md` sites | `callsign` or "connection", not `consumer` - a consumer may hold several sockets with different backlogs |

The last row is why this sweep cannot be mechanical, and scope 8 supplies the
worked example: "unread frames are the client's own backlog" is a property of
one connection, and `consumer` would make it false in the other direction.

## B3. `admission` - three unrelated mechanisms

Found by scope 2, confirmed internally by scope 3. Two of the three are
observable by one consumer in one run.

| sense | where | disposition |
|---|---|---|
| the per-connection outbound byte and frame budget | `admission.rs`, `AdmissionRejected`, `admission_lane_frames` | keep the word here - the protocol already publishes it |
| an inbound concurrency gate on `/trades` and `/quotes` | `admit_history`, `MAX_CONCURRENT_HISTORY_REQUESTS` | rename: slots |
| an attach refcount on an account | `Run::admit`, `Admission`, `Passenger::admitted` | rename: attach - the crate already says `attach` in three places |

`Run::admit` reads as capacity and means the opposite.

## B4. `seat` / `seated` - four senses, two shadowing each other in one loop

The single largest readability win available in `mogwai-venue`, and the
glossary's own sense is implemented correctly in exactly one place.

| sense | where | disposition |
|---|---|---|
| an account's riding of one boat | `Passenger::seated_on`, `try_sit`, `unsit` | correct; the model implementation |
| a boat exists in the yard | `boatyard::Slot::Seated(Seat)`, `seated_symbols` | `Berth` / placed |
| a passenger exists in the map | `Run::passenger`'s `seated` bindings, ~12 doc sites | existing / minted |
| a river has a boat | `"a seated river is skipped"` | boated |
| **takes no seat at all** | `Run::seat(...)` | `claim_account` - a reader tracing "where is the seat taken" lands here and is wrong |

`sweeper.rs` binds `seated` twice in one loop body, once for the live boat set
and once for the attached passenger set, and the second shadows the first.

## B5. `reservation` - a byte budget and a funds hold

Scope 1 found the two senses; scope 4 found five nouns for the funds sense
inside one crate.

- Byte sense: `sizing`, `BOUNDARY_REFUSAL_BYTES`, `worst_case_output_bytes`.
- Funds sense: `order_reservation`, `held_for`, `order_locked`, `locked_balances`, `Reservation::None`.

Disposition: `hold` for the funds object, `held` for the verb, `locked` reserved
for the wire field it names, and `reservation` kept for the byte budget only.

Riding with it: `Balance.locked` carries two economically different quantities -
order holds and unsettled equity proceeds - with opposite remedies and no way to
tell them apart. The split exists internally (`UnsettledCredit`) and is
deliberately collapsed on the wire. Scopes 1 and 4 disagree with the collapsing
argument; I agree with them.

## B6. `warmup` - three jobs

| sense | where | disposition |
|---|---|---|
| the servable history span | glossary, `warmup_ns`, `tick_composition_ratios` | correct |
| an estimator burn-in prefix, discarded | `fit::walk`, `arrival_control`, `arrival_screen`, `gen --warmup` | `burn_in` |
| a consumer's history request window | adapter `request_bars` logs | "history window" |

The burn-in sense is load-bearing three ways: a refusal, a component of a
content-addressed cache key, and a key in a committed artifact. The rename is an
offline re-bless.

## B7. `divergence` - armed havoc and float disagreement

Ten durable sites in `mogwai-lab` and `mogwai-data` use `divergence` for two
numbers disagreeing, in the same crates where it also means an armed arm. None
of the ten resists substitution - `disagreement`, `drift`, `mismatch` are all
free. Cheapest large collision in the workspace.

## B8. `ledger` - an account's engine and a purchase manifest

`mogwai_lab::ledger` is the Databento delivery manifest, with `LEDGER_KEY`,
`verify_input` and the `--ledger` operator flag. It collides head-on with the
glossary's Ledger, and both are load-bearing. The glossary term is older and
richer, so the flag and module move: `--jobs-manifest`, `delivery`.

The same module also carries the git-cleanliness gate, which is a third
unrelated job under one file name.

`LedgerTemplate` in `mogwai-venue` is a fourth sense - it is neither a ledger
nor a template of one, and its own doc admits the drift.

## B9. `tape` - four jobs

| sense | where | disposition |
|---|---|---|
| what a boat publishes | glossary, the delivery barrier comments | correct |
| the generated sequence | `SegmentSource` doc, `ensure_on_tape`, "off-tape window" | this is a river |
| an offline CSV dump | `mogwai segments tape`, `gen` output | this is a river's realization; `segments compose` |
| the identity of the generation process | `TAPE_PROTOCOL_VERSION` | a third concept with no word at all |

The last is the interesting one: River is the sequence, Tape is the delivery, and
the process that manufactures the sequence is unnamed while carrying the
workspace's most load-bearing constant.

## B10. The rest, in one table

Each is real, each is smaller, and each was found by at least one scope with a
worked reading.

| word | jobs | note |
|---|---|---|
| `ticket` | boatyard rider handle; admission frame slot; `ADMISSION_PROMISE_TICKETS` | the admission one is already called `slot` at its other site |
| `budget` | arrival exposure; wall-clock screening seconds; outbound bytes | three load-bearing senses |
| `segment` | a piece of a trade date; a cut session slice; an integration grid step | two of them are public types whose names invert each other |
| `cell` | a parameter point; a measurement cell | eight cell types across two modules |
| `provenance` | evidence standing; a cache key; a delivery record | two of the three are printed by `mogwai` subcommands |
| `breach_action` | two types, disjoint variants, both ours | see D1 - the worst collision found |
| `fault` | armed; organic; the health report; the exit string; the close | five senses in one crate |
| `pass` | sweeper cycle; per-passenger engine step; boat tape walk; two group-admission phases | five granularities, two crates |
| `frame` | websocket message; serialized `ServerMessage`; RFC 6455 control frame | most-used undefined noun in `mogwai-protocol` |
| `heartbeat` | venue text frame; adapter WS ping | two operator-visible knobs, opposite directions |
| `claim` | order attribution; account-id claim | split them: an order is attributed, an account is claimed |
| `latent` | mid; intensity; size median | three latents make "the latent" ambiguous |
| `level` | price grid step in a sweep; corpus statistic; book level | |
| `seam` | engine/server split; arrival attachment; the `TickSource` trait; a segment boundary | four seams |
| `preset` | instrument bundle; policy bundle | registered as `[account_policies]`, requested as `policy_preset`, called "a policy preset" |
| `cadence` | delivery speed; generator arrival rate | the Seat entry uses the first, the presets use the second |
| `venue` | mogwai; a real exchange | the water crates slide to the real sense, sometimes in one paragraph |
| `consumer` | the driving program; a `TickSource` caller | `mogwai-data` lands on the wrong glossary word while avoiding the retired one |
| `cursor` | a boat; a `TickSource`; a history cursor | three, one of them operator-visible in `docs/cli.md` |
| `terminal` | a close's finality; an order's finality | |
| `bind` / `unbound` | a connection binding a river; a placeholder account id | |
| `open` | opening an account; applying an arm to a ledger | collides inside one file |
| `trader` | what an account id identifies; nautilus `TraderId`; "one connected trader" | undefined, three referents |
| inbound / outbound | flips meaning depending on whose stream you stand in | fix by naming the ends, not the direction |

---

# C. Orphans - one job, several words

| job | words | disposition |
|---|---|---|
| a river's water | river, realization, walk, stream, tape, "one symbol's tape" | River reaches down into the generator |
| the paced reader | boat, cursor, lead, paced reader | Boat; keep `lead` for the checkpoint index's position |
| boarding | board, bind, place, materialize, join mid-stream | Boarding is the act; `materialize` is the better word for creating the river and the glossary should adopt it |
| the freeze | freeze, unattended, frozen, attach, resume, thaw | Freeze once; the inverse has no name at all |
| `data_origin_ns` | `TAPE_ORIGIN_NS`, `data_origin_ns`, "tape anchor", "tape origin" | one name; the rename deletes a `&self` method that exists only to translate |
| the broadcast ring | `fanout_depth`, "broadcast ring", "tape ring" | the operator is sizing a ring, not a topology |
| an order nobody reads | stranded, off-river, unreadable | two adjacent methods, two different conditions described as one |
| losing a ledger | discard, collect, reopen, reset | one verb, three triggers; `reopen` discards and its name says the opposite |
| the clock axis | `ClockAxis` enum; `boat_clock` bool | one concept, two incompatible wire encodings |
| attribution | originated, produced, claimed | engine says one, server says the others |
| decimal saturation | clip, clamp, saturate, "the stored value is clipped" | four phrasings, one module |
| the tape interval | span, reach, window, extend | `MaterializeRefusal::Reach` and `extend_toward` are the same quantity |
| a refusal | five refusal vocabularies (venue, lab, arrival, screen, segment) | one concept - the system declining rather than failing |

---

# D. Undefined vocabularies

The largest category, and the one that is not a rename problem. Every scope
independently reached the same structural conclusion: these are contracts whose
definitions live in doc comments on individual types, which is the wrong home
because a consumer reading `reference/` cannot find them and a doc comment on a
variant cannot state a contract spanning six types.

Four scopes converged on the same proposal: a `reference/wire-vocabulary.md`
lifting them out, with the glossary carrying one-line entries pointing at it.

Ordered by how much depends on them.

**D1. Admission, refusal and rejection.** Admission is capacity, rejection is
business, and they are different frames on purpose. `AdmissionSubject`,
`retryable`, backpressure, the `RETRYABLE_REJECT_PREFIX` public string contract,
six spellings of "the venue is full" across three carriers, thirty-odd
hand-written `OrderRejected.reason` literals with no shared vocabulary. Also the
`BreachAction` collision: two of this project's own types, one name, disjoint
variants, both reachable from one account, and the glossary defines only one.

**D2. The frontier.** `AGENTS.md` names this defect family first and the
glossary has no Frontier, Sweep or Watermark entry. Four scopes found it
undefined: per-boat (`last_swept_ns`, `frontier_after`, `rebase_scans`),
per-order (`scanned_ns`, `revision`, the `PendingScan`/`ScanResult` seam), and
`trigger.rs`'s `reached_ns`, which scope 5 calls the cleanest statement of the
rule anywhere in the workspace - and which sits in a comment above a `for` loop.

**D3. Lane and byte budget.** The glossary uses `lane` in the Connection entry
and defines it nowhere. It is structural: the account-freeze rule is stated over
lane binding and release, so a reader cannot evaluate the freeze rule without it.
`exec_held_budget_bytes` and `admission_lane_frames` are operator config keys.
"Held" names the lane by what `DelayAcks` does to it rather than what it carries.

**D4. The fill band.** `AGENTS.md`'s unconditional tape-version bump rule names
"the fill band's draw" by name, so the term is load-bearing in a rule the
glossary does not define. Band, draw, trigger, tranche, hit, through versus
touch, print. Three config keys an operator must set rest on it. Scopes 1, 4, 5
and 7 all reached it.

**D5. Generation.** Parent, child, sweep, burst, print, level, latent mid, bounce,
drift, `SweepShape` (public, with a closed-form mixture solve in its doc). This
is what a tape is made of. A reader of `/trades` sees prints; a reader of the
generator sees parents with children; nothing connects them. The intra-event
stride is what makes the trade stream strictly increasing, which a timestamp-only
history cursor depends on - a consumer-visible contract stated in a private
function's doc.

**D6. Reconciliation and the mirror.** Venue truth, witness, mirror, truth store,
snapshot (three kinds), correlation id, whole versus degraded. "Venue truth"
appears in eleven doc blocks and two operator-facing errors as if it were defined
vocabulary. The Ledger entry names the venue's side only, and the mirror is
exactly the thing that can disagree with a ledger.

**D7. Linkage.** Group, sibling, parent/child, release, reap, close-out, the
atomic-admission guarantee and its funds carve-out. Release emits no wire frame,
which is a consumer-visible fact: a consumer watching for a status change on its
bracket's exit legs waits forever.

**D8. Resolution.** Shape, bundle, def, profile, preset, overlay, materialize -
five names for one concept, several of which reach a consumer in refusal bodies.
The River entry hangs identity on "the resolved bundle" and never defines bundle.

**D9. Account lifecycle.** Eleven verbs from outside, more from inside: seat,
claim, admit, attach, resume, freeze, collect, reset, evict, unsit, discard,
open. The glossary names three.

**D10. Risk enforcement.** Equity (a precise, restrictive definition a consumer
cannot guess), peak, day-open, drawdown, trail, ratchet, floor, high-water mark,
breach, lock, terminate, policed. The type docs are excellent; none is durable.

**D11. The arrival kernel.** Seven public types, three admission ceilings with
exemplary measured docs, `ARRIVAL_KERNEL_VERSION` as a fourth identity constant
nobody enumerates, and `ArrivalRefusal` - the only non-injected route to a
terminal tape fault in the product, whose vocabulary appears in no document.

**D12. The intake sequence.** Corpus, measurement, fit, fingerprint, artifact,
binding, provenance, preflight, verdict. `AGENTS.md` names the sequence and
defines no step. Half of `mogwai --help` is protocol jargon whose referents are
retired `notes/` documents an operator cannot resolve.

**D13. Storage.** `docs/cli.md` cites "the storage policy" three times as a named
authority and no document defines it. Artifact, cache and scratch classes,
provenance token, sub-contract hash.

**D14. The adapter's own vocabularies.** Transport generation (neither a
Connection nor a Run, and what makes a swapped `Arc` correct), the receipt book
(`unwritten`, `on_undelivered`, residue), the delivery pipeline (pump, barrier,
drain, sink, held, black-holed), history paging (the "same-ts wedge" appears in
three operator-facing warnings and is defined nowhere), the instrument caches
(two, both called "the cache" in adjacent sentences).

**D15. Delivery and invisibility.** `Audience` and its five variants, the lane
table, the order-ownership table. The Passenger entry asserts invisibility and
names no mechanism, so nobody can check the claim. `Audience`'s own doc is most
of the document that is owed.

**D16. Smaller, still owed.** Checkpoint chain (the glossary leans on it - a
glossary defect in the strict sense). Mark. Leg - one of the two connections a
nautilus consumer necessarily holds, a real concept scope 6 argues should get an
entry rather than a rename. The late-boarder rule, open-coded twice in two
crates with nothing shared. Print. The `Wire` prefix convention. Redial.
Terminal. The money-is-a-string decimal contract, whose exhaustive exception list
lives in a test's doc comment.

---

# E. Durable contradictions - the most urgent section

Two binding documents that disagree mean neither can be cited, and every reader
downstream is working from whichever they opened. Scope 8 found nine clusters;
these are the ones with a worked reading on both sides.

**E1. The retired one-river premise, nine sites.** Owner-retired 2026-08-20.
Still asserted in `reference/architecture.md` three times, including as the
proof that tick-resolution risk evaluation is exact - and contradicted by the
same file about 500 lines later, which states the rule correctly. Also in
`risk.rs`'s `MaxPosition`, `extremes.rs`, `admission.rs`, and
`retire_off_river`. The `extremes.rs` instance is the expensive one: the
two-prices-per-span argument is a correctness proof resting on it, and for a
multi-river account the peak-equity ratchet is fed a partial, arbitrarily
ordered reconstruction. Scope 2 established what the code actually does, which
is a third thing neither document describes: the position cap is applied per
symbol.

**E2. Four order types versus nine.** `reference/architecture.md` says the
surface is complete at nine, then says the venue serves four about 350 lines
later. `docs/oms-types.md` says nine.

**E3. Two instrument classes versus five.** Same file, same shape.

**E4. `POST /account` versus `POST /accounts`.** `docs/havoc.md` names the
account-open endpoint twice under the read endpoint's spelling. A reader wiring a
scenario posts to the wrong URL.

**E5. "There is no venue-wide notion of now."** `docs/havoc.md`, stated
absolutely, false against `reference/clock.md` and against the `clock: "venue"`
field on the wire. The conclusion survives on a weaker true premise: no
venue-wide clock is authoritative for a seated river.

**E6. `speed` taught as run-level.** `docs/oms-types.md` uses it as the analogy
that explains what run-level means, while `reference/clock.md` says in terms it
is a default a `/ws` upgrade may override.

**E7. Havoc scope.** `docs/havoc.md`'s opening line says havoc is armed against
the one run and operates on "the run ledger" - two retired premises in the first
sentence - and the same page says sixty lines later that transport controls are
armed per account.

**E8. Attributed delivery versus every open socket.** `reference/architecture.md`
states "delivery is attributed, not broadcast" and, elsewhere, "execution output
that no command asked for reaches every open socket". The second negates the
first and negates the invisibility property.

**E9. The boot river is boarded.** `reference/architecture.md` says `serve`
boards it; the glossary says the run boards nothing because boarding is a
passenger's act. Fixed in the glossary already; the architecture paragraph owes
the correction.

**E10. `server` for the process, in binding prose.** Including `AGENTS.md`
itself, three times - the file that teaches every agent the vocabulary. Also the
project-owned wire field `server_now_ns`, which the Consumer entry's exhaustive
exemption list does not cover.

**E11. The Served symbol entry's refusal list reads as exhaustive and is not.**
At least five grounds exist; the entry names two and excludes a third.

---

# F. The roadmap - what the code owes the glossary

The glossary is the end state. These are gaps, never cases against an entry.

**F1. Generator havoc must fork the river.** Settled: the entry stands, the code
owes it. Scope 5 found the machinery that deliberately removed the fork - the
pinned control-boundary snapshot, the coarsen exemption, the walk-back floor -
so the gap has a known size and known work to undo. The seated-boat refusal that
stands in for it names a remedy no route exposes, and its gate is vacuous
against a concurrent board.

**F2. The adapter cannot name a speed or a duration.** `ws_url` emits neither,
though the venue reads both and the Connection, Boat and RunComplete entries all
define them. `lifecycle.rs` already carries a log line for a close no
adapter-built connection can cause.

**F3. The adapter reads the wrong clock.** `GET /clock` is fetched with neither
symbol nor speed, so every timestamp, havoc deadline, quota interval and backoff
sits on an axis the venue itself declines to call a boat's. `boat_clock` is the
flag that would catch it and has no reader in the crate.

**F4. Boat unobservability.** A river's history ceiling is computed across every
seated boat, so another account boarding at a different cadence moves this
caller's ceiling.

**F5. The many-rivers shape is not expressible through the adapter's public
API.** One data client binds one river and refuses every other subscription.

**F6. Equity conversion drops the three facts the entry says an equity carries** -
lot size, borrowability, settlement period.

**F7. `ship_server_havoc` discards the boarding scope**, so transport arms
default venue-wide instead of riding the configured account's passenger.

**F8. `[regime]` is run-wide**, so a per-passenger generator arm has no operator
expression. Under Boarding the carrier decides nothing, so this is a config-schema
gap.

**F9. `[balances]` and `[account_policies]` are separate tables** while the
Account policy entry defines a policy as opening balance plus risk rules. There
is no way to register a named policy that states its opening equity, which is
what a funded-account programme is.

**F10. Attributed delivery.** If unsolicited execution output really does reach
every open socket, the invisibility property is not held and the code owes it.

**F11. `for_run` discards `account_ttl_ms` and `reset_account_on_reconnect`**, so
the adapter's reconnect loop can back off past a freeze TTL blind.

---

# G. Inherited - quarantine, never renamed

Consolidated from all eight scopes so no later pass re-litigates it.

- Nautilus API names in full: the client and factory types, id types,
  `OrderStatus`, `OrderType`, `TimeInForce`, `OmsType`, `Contingency`,
  `AggressorSide`, `LiquiditySide`, the instrument field spellings, the asset
  classes, `AccountState`, `Balance`, `Position`, the tick shapes.
- FIX and industry: OCO/OTO/OUO, post-only, reduce-only, IOC/FOK/GTC/GTD/Day,
  bracket, maker/taker, `client_order_id`.
- Derivatives and accounting: notional, VWAP, variation margin, maintenance and
  initial margin, drawdown, equity, liquidation, flatten, settlement, T+N, round
  lot, Reg-T, funding rate, basis points, inverse and coin-margined.
- Microstructure and data: Roll estimator, effective spread, quoted width,
  aggressor, tick rule, top of book, BBO, TBBO, trade date, session open, halt,
  settlement, OHLCV.
- Statistics and probability: GARCH, Weibull, Student-t, ACF, Wasserstein,
  bootstrap, stratum, pilot sample, MMPP, Cox process, log-OU, Hawkes, time
  change.
- Published algorithms: ChaCha, splitmix64, Mersenne Twister, Fisher-Yates,
  sha256, XXH128, FNV.
- Platform and protocol: HTTP status codes, RFC 6455 close codes and framing,
  `PR_SET_PDEATHSIG`, SIGTERM, `RUST_LOG`, `NO_COLOR`, XDG.
- Real venues and instruments: Kraken, Binance, Databento, CME, MNQ, MES,
  BTCUSDT, XBTUSD.
- Trading-desk daypart names: asia, london, ny-morning, ny-afternoon.

Two notes carried from the scopes. Three "sides" coexist - `Side`,
`AggressorSide`, `LiquiditySide` - all inherited, so the collision stays and
wants one glossary entry naming the three rather than three renames. And
`BreachAction` is not inherited from anywhere, both spellings are ours, which is
why that collision is fixable.

---

# H. What still needs an owner ruling

Everything else in this ledger has an evident answer or is already ruled.

1. **`server_now_ns` on the wire.** The `server` ruling retires the word for the
   venue's clock; this is the wire field carrying it. Renaming is a designed
   break, which is in scope, but it is a break.
2. **`Balance.locked` carrying two quantities.** Splitting it is a wire change
   two scopes recommend and one type's doc argues against.
3. **`RunComplete` announcing two different events.** Both scope 1 passes reached
   it independently; `notes/todo.md` carries it as an open issue under a
   different framing.
4. **Whether the evidence toolbox stays on the binary's top level.** Eighteen
   subcommands for one audience beside three for another.
5. **`--ledger`.** The flag and the glossary term are unrelated and both
   load-bearing; renaming the flag is cheap, and someone has to say so out loud
   before the intake documents are rewritten around it.
6. **Whether `leg` gets an entry** rather than being renamed onto Connection.

---

# I. Cross-cutting, not naming

Found by the arc, belonging to no term family, and none of it should wait behind
a vocabulary sweep.

- **Doc comments attached to the wrong item: three instances, two crates.**
  `evict_account` and `session_guard` in `run.rs`, `free_balance` in
  `account.rs` - each losing its entire doc to the following item, and
  `evict_account` is the method both the glossary's Eviction entry and
  `CloseSpec::evicted` cite. Rustdoc renders all three happily. Nothing detects
  it.
- **Dangling intra-doc links.** Three references to `risk::RiskPolicy`, which
  does not exist; one to `instruments::InstrumentSpec`, which does not exist;
  one public doc linking a `pub(crate)` item; two citations of a deleted
  `PollCursor` type, one of which is the whole justification for the `trade_id`
  derivation. Worth checking whether `broken_intra_doc_links` is enabled.
- **Vacuous gates.** `CommandClass::of`'s wildcard arm, where its sibling in the
  same crate is deliberately exhaustive. `DivergenceRequest` accepting and
  ignoring, on the control plane, where `SocketQuery` carries a comment arguing
  that is the exact failure to prevent. `/clock?speed=` accepted and ignored.
  Three more query structs without `deny_unknown_fields`. `Health.status` is a
  constant `"ok"` even when `fault` is `Some`, with a documented fleet-poller
  consumer. `terminated && seated.len() == 1` documented as "the venue's only
  account" over the attached set, so one momentarily disconnected account lets a
  breach end the whole run.
- **Refusal texts hardcoding constants.** Four divergence branches spelling
  `3600000`, a symbol refusal naming 32 where the cap is 64, a reserved-prefix
  message naming one prefix where the constant holds two.
- **Stale module docs.** `generated/arrival.rs` says it is "deliberately not
  connected" to the generator it drives and is tape-version-bearing.
  `TRADE_BOUNCE_HALF_WIDTH_TICKS` rests on "the generator constructs no
  `QuoteTick` anywhere", which is false twice over. `default_instruments` says it
  returns one instrument and returns eight.
- **Ten sites describing a retired HTTP order carrier** in the adapter, two of
  which describe their only real caller as an exception.
- **Two binding documents cite `notes/`**, which `AGENTS.md` forbids outright.
- **The prose gate is narrower than the risk.** `tape_version_prose.rs` guards
  two phrasings of one constant; this arc found four other live facts stale in
  one file. The design generalizes cheaply, and phase 5 should take the
  order-type count and the class count as its first two subjects.
- **Structural proposals from the scopes**, recorded rather than adopted: split
  `reference/architecture.md`, which is 1174 lines doing four jobs with its
  contradictions sitting where one job's old text survived another's landing;
  rewrite `docs/havoc.md` rather than patching it, since its opening five
  paragraphs are pre-multi-account and the rest of the page argues against them;
  add `docs/accounts.md`, reached independently by scopes 7 and 8.

---

# J. Execution order

Phase 4 is serial, one term family per round. Suggested order, cheapest
verification first and highest blast radius early:

1. **The durable contradictions (E).** They are prose-only, they need no
   ruling, and while they stand no document in the corpus can be cited. E1's
   nine sites go first because a retired premise is still deriving behaviour.
2. **`server` to `venue` (B, ruled).** Mechanical, compiler-verified, and it
   unblocks rows in every remaining family.
3. **`client` to `consumer` (B2, ruled).** A classification, not a substitution;
   the brief carries the row list and escalates anything fitting no recorded
   sense.
4. **`session` to `callsign` (B1, ruled).** Wire break, its own round.
5. **`passenger` (B, ruled).** Duration and tickets onto Connection and Seat.
6. **`seat`, `admission`, `reservation`, `warmup`, `divergence`, `ledger`,
   `tape`** - the remaining large collisions, one round each.
7. **The undefined vocabularies (D).** Not rename rounds. This is writing
   `reference/wire-vocabulary.md` and its counterparts, which four scopes
   converged on independently.
8. **The conformance gate (phase 5).**

The roadmap (F) is engineering rather than vocabulary and does not belong in this
loop at all. It wants filing as work items once the ledger is accepted.
