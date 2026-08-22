# Glossary scope 8 - the durable corpus against the glossary

Inventory of the DURABLE prose - `reference/` in full (`architecture.md`,
`clock.md`, `performance.md`, `technical-implementation-spec.md`), `docs/`
except `cli.md`/`config.md`/`presets.md` (covered by earlier scopes), plus
`AGENTS.md` and `CLAUDE.md` - held against `reference/glossary.md` as revised
2026-08-21 (Boarding is an entry, Client became Consumer, River / Divergence /
Session / Eviction clarified). Nothing was edited but this file.

This scope differs from 1-7 in kind: both sides are BINDING. Where two durable
documents disagree neither can be cited, so direction 5 leads and is reported in
full with both quotations side by side.

Terminology per the standing rulings: `client` is retired for everything this
project owns (surviving only as nautilus's `MogwaiDataClient`/
`MogwaiExecutionClient` and the wire field `client_order_id`); `server` is
retired as a name for the process, its clock, its messages or its crate, and
survives only as Server mode.

---

## Direction 5 - durable contradictions. Read these first.

### 5.1 An account is on at most one river - the premise the owner retired 2026-08-20, stated three times in `reference/architecture.md` and contradicted by the SAME FILE [changed in independent reconciliation]

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Seat / River | `reference/architecture.md`: "**AN ACCOUNT IS ON AT MOST ONE RIVER, WITH ONE READER.** A second socket presenting a seated account id EVICTS the first, because a ledger read and written from two places is one ledger with two notions of its own state." | rule statement, section-opening capital | one account = one river = one socket | every downstream claim about seats, freeze, retirement, risk exactness | 5 | **Delete the sentence and rewrite the paragraph.** The glossary's Seat entry says the exact opposite: "An account holds as many seats as the distinct boats its sockets have bound, so one account trades many rivers at once (many strategies, one ledger). The one refusal is a second speed of a river the account is already riding." And Eviction is keyed on SESSION, not on seating. The eviction rationale quoted here ("a ledger read and written from two places") is also the wrong rationale under the current Eviction entry, which admits many coexisting sockets on one ledger under one session. |
| Seat / River | `reference/architecture.md`: "Equity is LINEAR in the price of the one instrument an account can hold - **an account is on at most one river**, strategies are single-instrument - so its extreme over the span sits at a price extreme." | rule statement inside the tick-resolution argument | the load-bearing premise of the "exact rather than approximate" risk claim | the whole trailing-stop / risk-policy exactness argument; `reference/performance.md` sweeper rows | 5 | **This is the expensive one.** The exactness proof for tick-resolution risk evaluation RESTS on the retired premise. Under the glossary an account holds many seats, so equity is a sum of linear terms across several rivers and the span argument does not close. The document already knows this - see the next row - and the two paragraphs contradict each other. The paragraph must be rewritten to state the bound honestly: exact for a single-river account, mark-cadence for the rest. |
| Seat / River | `reference/architecture.md`: "WHAT THE SPAN DOES NOT COVER ... an account holding MORE THAN ONE marked symbol ... That costs nothing under the model the venue enforces (**an account is on at most one river**) and is a bound to state rather than a defect to hide." | rule statement | the carve-out, dismissed as unreachable | risk claims over multi-river accounts | 5 | The carve-out is not unreachable: under the glossary it is the SUPPORTED SHAPE. Strike the parenthetical and promote the carve-out from "costs nothing" to "this is what a multi-seat account gets". |
| Consumer / Session | `reference/architecture.md`: "**An account is on at most one client at a time** and a second claim evicts the incumbent - which is how a reconnect works, since the venue cannot distinguish a returning client from a stranger - but one client legitimately holds several sockets on one ledger" | doc prose | `client` doing the job of Session | adapter session minting; eviction semantics | 5 + 1 | **[changed] This is not a fourth single-river assertion.** It is still defective, but on a different axis: `client` is retired, this is Session, and the venue cannot evaluate Consumer identity. Rewrite over Session: an account is claimed by at most one session at a time. The single-river count is three, at lines 34, 340 and 474 of the current file. |
| Seat / River | `reference/architecture.md`: "One ledger still carries one cadence - **two sockets on the default account may ride two rivers**, but a second speed on a river that account is already seated on is refused" | doc prose, boatyard section | the CORRECT rule, matching the glossary Seat entry exactly | boatyard, seat counting | 5 | **This paragraph is right and contradicts the four rows above, in the same file.** Keep this one; every other statement of the rule in `architecture.md` has to be deleted or rewritten to agree with it. That a binding document states a rule and its negation ~500 lines apart is the finding: a reader cites whichever half they opened. |

### 5.2 The venue serves four order types / nine order types - `reference/architecture.md` against itself and against `docs/oms-types.md`

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - (order-type surface) | `reference/architecture.md`: "**THE ORDER-TYPE SURFACE IS COMPLETE RATHER THAN CURATED** ... Market, Limit, StopMarket, StopLimit, TrailingStopMarket, TrailingStopLimit, MarketIfTouched, LimitIfTouched and MarketToLimit are served, which is every order type nautilus expresses." | rule statement | nine types, exhaustively | `docs/oms-types.md`, the adapter's `wire_order_type` compile-error argument | 5 | Correct half. |
| - (order-type surface) | `reference/architecture.md`, ~350 lines later: "Execution output that no command asked for reaches every open socket. **The venue serves four order types - `Market`, `Limit`, `StopMarket`, `StopLimit`** - and a resting order is one of three explicit states" | doc prose | four types | same | 5 | **Stale prose asserting a live fact, in the file that argues hardest against exactly that failure.** `docs/oms-types.md` states nine ("That is every order type nautilus expresses; none is refused"). Delete the "four order types" clause; the surrounding paragraph about resting-order states survives untouched. |
| Instrument class | `reference/architecture.md`: "**THE LEDGER MODELS FIVE INSTRUMENT CLASSES**, split by SETTLEMENT SHAPE" (Spot, Equity, Future, Perpetual, Inverse) | rule statement | five, matching glossary Instrument class | glossary, config, presets | 5 | Correct half. |
| Instrument class | `reference/architecture.md`: "An instrument is a bundle of knobs, not one fixed shape. **Two classes are selectable: a spot currency pair, and a cash-settled continuous future** with a contract multiplier" | doc prose | two classes | same | 5 | Same defect shape as the order types: an older paragraph survived a capability landing and now contradicts both the glossary and its own file. Rewrite to name the five and keep the multiplier/size-grid argument, which is what the paragraph is actually for. |

### 5.3 `POST /account` versus `POST /accounts` - `docs/havoc.md` against `reference/architecture.md` and `docs/oms-types.md`

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - (account-open endpoint) | `docs/havoc.md`: "the account's first ledger - whether it is minted by a socket or by **the client's own `POST /account`** - opens carrying it"; and "the client still states its own opening balances and policy on **`POST /account`**, and finds the arm standing on the ledger that call returns." | doc prose, twice | names the account-OPEN endpoint | anyone wiring a scenario file / control plane | 5 | **The endpoint does not exist under that spelling.** `reference/architecture.md`: "`POST /accounts` opens an account on terms the client states" and "`GET /account` names whose ledger with `?account=`". `docs/oms-types.md` agrees: "the opening balances in a `POST /accounts` body". A reader of `havoc.md` will POST to the read endpoint. Fix `havoc.md` (two sites), and while there, `client` -> `consumer`. |

### 5.4 "There is no venue-wide notion of now" - `docs/havoc.md` against `reference/clock.md` and `reference/architecture.md`

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - (venue clock) | `docs/havoc.md`: "the boat carries the clock every answer about that symbol is dated on. **There is no venue-wide notion of now**, so a havoc window cannot be an interval on one clock." | rule statement | there is exactly one clock species, the boat's | havoc window semantics; `/clock`; `/account` | 5 | Stated absolutely and it is false. `reference/clock.md`: "The venue retains one wall-to-sim reference for answers that have no boat: a boatless river, the venue deadline, and the venue-scoped account ledger." `reference/architecture.md`: "`GET /account` therefore keeps the venue stamp and LABELS it, adding a `clock: \"venue\"` field". The venue-wide now EXISTS, is on the wire, and is what the run deadline is judged on. The havoc conclusion survives on a weaker true premise: no venue-wide clock is authoritative for a SEATED river, so a window is stored as a wall instant plus a simulated span. Rewrite the premise; keep the conclusion. |

### 5.5 `speed` is run-level - `docs/oms-types.md` against `reference/clock.md`

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Connection / Boat | `docs/oms-types.md`: "This is a **run-level** choice, not an instrument one - it applies to the whole venue for the run, **the same way `seed` or `speed` does**" | doc prose, analogy | asserts `speed` is a run-wide constant | any consumer reasoning about per-socket pacing | 5 | `reference/clock.md` states the opposite in capitals: "`speed` is the only clock key left in config, and it is a **DEFAULT** rather than the run's one pacing rate: a `/ws` upgrade may name its own `speed`". The glossary's Boat entry says the same ("a different quantized speed places a second boat on the same water"). The analogy is load-bearing here - it is how the reader is told what "run-level" means - so it teaches the wrong model of the venue. Drop `speed` from the analogy; `seed` alone carries it. |

### 5.6 `FeedLagged`: a signal you keep trading through, or a close - `docs/havoc.md` against `reference/architecture.md`

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - (`FeedLagged`) | `docs/havoc.md`: "The venue declares the hole on the wire: `FeedLagged` carries the skipped count, **so a client reading the protocol directly can tell a quiet feed from a lossy one** rather than inferring it from bar shape." | doc prose | a diagnostic identifying why delivery ended | consumers writing gap handling; `mogwai-adapter`'s ERROR log | 5 | **[changed, refuted] No contradiction is established.** `reference/architecture.md` says the consumer receives `FeedLagged` and is then closed with WS 1011. The frame still distinguishes a lossy feed from a quiet one before the fatal close; `havoc.md` never says delivery continues. `docs/adapter-lifecycle.md` then consistently treats the close as reconnectable transport failure. The wording could say "before the connection closes" for clarity, but weakening or escalating the behavior is not owed. |

### 5.7 Havoc scope: "against the one run, not against an account" - `docs/havoc.md` against itself and against the glossary

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Divergence / Ledger | `docs/havoc.md`, opening sentence: "**Transport and engine havoc is armed against the one run, not against an account or a connection.** Order-path divergences operate on **the run ledger**" | rule statement, first line of the page | one ledger per run; havoc is run-scoped | every scenario author's first read | 5 | Two retired premises in the page's opening line. The glossary's Ledger entry: "one `mogwai-engine` instance, owned by one account ... A run holds as many as it has accounts and they share nothing." The Divergence entry: transport arms "corrupt what one account's connections receive, so they ride the passenger"; engine arms "queue one-shot execution divergences on **the account's own ledger**". And the same file contradicts itself sixty lines later: "**Transport controls remain runtime-armable and are ARMED PER ACCOUNT.** `GoDark`, `StallData`, `DelayAcks` and `CommandLatency` all take an optional `account`". The opening paragraph is pre-multi-account text. **Rewrite the whole opening section against the glossary's three-way classification (generator arms / transport arms / engine arms), which `havoc.md` re-derives worse further down.** |
| Passenger | `docs/havoc.md`: "A BOAT is the paced reader sitting on a river ... **the connections sharing it are its PASSENGERS**" | doc prose, the page's own two-noun primer | passenger = connection | the whole page's havoc-window reasoning | 5 | **[changed] Direct contradiction confirmed, but the first pass overstates how cleanly the corpus votes.** Clear per-connection sites are this sentence and `reference/architecture.md`'s "A passenger owns an uncloneable ticket for one websocket connection." The architecture opening's "one connected trader" leans connection-side, but the same paragraph immediately says Passenger is keyed by account id and outlives the connection, which leans account-side. The glossary Passenger entry is explicit per account; its RunComplete entry and `reference/architecture.md`'s passenger-local socket duration lean connection-side. See the adjudication section below for counts and migration cost. |
| River | `docs/havoc.md`: "A RIVER is **one symbol's tape**, materialized the first time this run is asked for that symbol." | doc prose | river keyed by symbol alone | havoc arming by symbol; generator-arm refusals | 5 / 1 | Glossary River: keyed by "the requested symbol plus that shape's knobs ... the resolved bundle, the seed, generator-level havoc". `architecture.md` agrees ("A `RiverKey` includes the exact requested label, its per-label tape seed, and the resolved bundle digest"). The simplification is not harmless ON THIS PAGE: the glossary's whole account of generator arms is that an arm CHANGES the key, and "one symbol's tape" makes that impossible to state. Fix the primer. |

### 5.8 Retired vocabulary asserted as the venue's own name - `server` for the process, in binding prose

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Venue | `reference/architecture.md`: "**The server exposes** `/health`, `/account`, `/accounts`, `/instruments`, `/clock`, `/trades`, `/quotes`, `/control/divergence`, and `/ws`." | doc prose, the HTTP surface roll-call | the process | every consumer's mental model of what serves | 5 / 1 | `server` is retired as a name for the process. This is the single most-cited sentence in the file. -> "The venue exposes". |
| Venue | `reference/architecture.md`: "at its deadline **the server announces** `RunComplete`, closes WebSockets normally, drains, and exits zero" | doc prose | the process | Run duration semantics | 5 / 1 | -> "the venue announces". |
| Venue | `reference/architecture.md`: "moving **every server tape** while leaving offline generation seeds untouched"; "it does not walk **the server's river** placement" | doc prose | the venue's rivers as opposed to offline `gen` | tape-version record | 5 / 1 | The distinction being drawn is real and useful (served rivers vs offline generation) and it deserves a WORD rather than the retired one. Proposed: "served tape" / "served river", against "offline tape". |
| Venue | `reference/performance.md`: "**The server now admits** at most four concurrent whole-page history syntheses"; "the mapping **the server wrapper** builds"; "one ingredient of **the server's** `build_history_source`"; "**The server never emits** that spelling" | doc prose, four sites | the process, and once the `mogwai-venue` crate layer | measurement record | 5 / 1 | Mixed: two of the four mean the process (rename to venue), two mean the `mogwai-venue` CRATE layer as opposed to `mogwai-data` and are legitimate crate references. Resolve site by site, do not sweep. |
| Venue | `AGENTS.md`: "synthesizes market data from a committed fingerprint ... (**the running server** opens no CSV)"; "**the server's** own test binaries drive the venue through it"; "the `GeneratedSource` synthetic generator **the running server** uses" | doc prose | the process | the root contract every agent reads | 5 / 1 | The rulings document itself uses the retired word for the process three times. Fix here first: this is the file that teaches every agent the vocabulary. |
| Venue | `docs/havoc.md`: "so a **server heartbeat** still arrives and a stalled feed stays distinguishable from a dead venue" | doc prose | a venue-originated keepalive frame | consumers writing liveness checks | 5 / 1 + 3 | Both retired-word AND undefined: no durable document and no glossary entry defines a heartbeat frame. The wire section of the glossary lists `ReadyRecord` and `RunComplete` only. Name the frame, define it, or say "venue output other than market data". |
| Venue | `reference/clock.md`: "`server_now_ns` walks at wall rate from the boat's origin"; "`server_now_ns` is then the sim instant of the last tick that boat published" | wire field name in doc prose | the `/clock` envelope's current-time field | `/clock` consumers, adapter clock fetch | 5 / 1 | A project-owned WIRE FIELD carrying the retired word. The Consumer entry's exemption list is exhaustive - nautilus's client objects and `client_order_id` - and does not cover this. Rename to `venue_now_ns` (or `now_ns`) with the field-rename landing in `mogwai-protocol`, `clock.rs`, the adapter and `reference/clock.md` together. |

### 5.9 The glossary's refusal grounds are narrower than the durable prose

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Served symbol | glossary: "Refusals are about the label or the run's balances, **never** about the absence of a preset." vs `reference/architecture.md`: "**creation of the 257th river is refused atomically**, with no eviction" and the `/ws` refusal roster "an illegal label, a shape that does not validate, a funding-barred one, **an exhausted river cap**, a non-finite or negative speed, **a second cadence on a river this account is already riding**" | doc prose vs rule statement | the complete set of grounds a symbol request can be refused on | consumers writing symbol-request error handling; `/trades` and `/quotes` 400s | 5 | The glossary's "never about the absence of a preset" is the right claim, but the two-item list around it reads as exhaustive and is not: there are at least five grounds. Rewrite the Served symbol entry to state the grounds as a set (label legality, shape validity, funding presence, river cap, cadence conflict) and keep "never a missing preset" as the exclusion it is. |

### 5.10 Attributed execution output versus every open socket [added]

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Passenger / Account | `reference/architecture.md`: "**DELIVERY IS ATTRIBUTED, NOT BROADCAST** ... each frame it produces reaches only the connections it is about" versus the same file: "**Execution output that no command asked for reaches every open socket.**" | two rule statements | account-scoped delivery versus venue-wide delivery | account invisibility; fills, expiry, liquidation and sweep output | 5 | **[added] Direct internal contradiction.** The glossary Passenger and Ledger entries require every frame to reach the account it concerns, including venue-originated liquidation. "Every open socket" leaks one account's execution into every other account and negates the earlier section. Rewrite the later sentence to say unsolicited execution output reaches every connection of the account it concerns. This is behavior the code owes if it does not already attribute it. |

### 5.11 The boot river is placed, not boarded [added]

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boarding / Boot river | `reference/architecture.md`: "The BOOT river is the exception to placement on demand: `serve` **boards it** before it writes the readiness line ... and the run retains that ticket" versus glossary Boot symbol / boot river: "the run **boards nothing**, because boarding is a passenger's act and a run takes no seat" | rule statement | eager boat placement described as a passenger act | boatyard ownership, seat accounting, readiness | 5 + 1 | **[added] `boards` is used for the wrong job.** The behavior is eager placement and a run-owned keepalive ticket, not Boarding and not a Seat. Rewrite the architecture paragraph with `places`; preserve the lifetime fact. If the implementation represents the keepalive as a passenger ticket, that internal type owes separation because it makes the durable vocabulary false. |

---

## Direction 1 - a glossary term used for a job that is not that term's

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Consumer | `reference/architecture.md`: "one engine per process meant **every client's** fills moved every other client's net"; "an N-account venue sent **every client** all N snapshots"; "**A client that believes them** sizes against a stranger's equity" | doc prose | the counterparty program | the per-passenger-ledger argument | 1 | `client` retired -> `consumer`, or `session` where the sentence is about one socket's view. Note the second and third are precisely about what ONE CONNECTION sees, so `session` is the right word there, not `consumer`. |
| Consumer | `reference/architecture.md`: "**THE ACCOUNT ID ON A SNAPSHOT IS A LABEL, AND A CLIENT KEEPS ITS OWN** ... the id the venue writes on an `AccountState` therefore identifies nothing **a client** has to resolve" | rule statement, section heading | the adapter | `adapter_smoke::an_account_labelled_differently_is_still_served` | 1 | This whole section is about the ADAPTER's behaviour specifically (`note_account_label`, `handle_account_state`), which is one of the two sanctioned `client` survivals - but the prose says "a client" generically, which reads as the retired sense. Say "the adapter". |
| Consumer | `reference/architecture.md`: "A client that read the code alone would have to treat all four alike, **and the adapter did**"; "a client that sees a bare EOF instead classifies nothing and redials into the loop" | doc prose | any protocol peer | close-reason contract | 1 | -> "a consumer". |
| Consumer | `reference/architecture.md`: "**the client is attached, so it is told**" (order-on-an-unread-river rule); "An oversized submit is **a client error** rather than a liquidation"; "**A client-stated price is refused** on this type"; "so a **client** reading it does not have to infer" | doc prose, four+ sites | the submitting side | order-entry contract | 1 | -> `consumer` throughout, except where the point is genuinely per-socket, where `session` is more precise. |
| Consumer | `reference/architecture.md`: "That exists for the **ephemeral single-client venue**, where making the one client name an id would be ceremony" | doc prose | Transient mode | default-account semantics | 1 / 2 | The glossary NAMES this: Transient mode. Say "Transient mode" and the sentence gets shorter and more precise. |
| Consumer | `reference/architecture.md`: "**A client that needs to know where the tail actually is** reads `/clock?symbol=`"; "a client stamping its `end` from `/clock`" | doc prose | the history caller | history clamp contract | 1 | -> `consumer`. |
| Consumer | `reference/clock.md`: "the only way **a client** sees a market older than the one it is filled against is that it **HAS NOT DRAINED ITS SOCKET** ... unread frames are the client's own backlog" | rule statement | the reading side of one socket | the coherence argument | 1 | This is per-SOCKET (a backlog is a property of one connection), so `session` or "connection" is the precise word; `consumer` would be wrong here because a consumer may hold several sockets with different backlogs. Good example of why the blanket `client` -> `consumer` sweep must not be mechanical. |
| Consumer | `reference/clock.md`: "An absent `speed` means the configured one and is what **every client that predates the carrier** sends" | doc prose | older consumer builds | wire compat | 1 | -> "every consumer build that predates the carrier". |
| Consumer | `docs/havoc.md`: "Order-path arms apply only to **client-originated** orders"; "the next matching **client** action"; "**WHAT A CLIENT CAN DO ABOUT IT DEPENDS ON THE CLIENT**"; "a run that needs the distinction programmatically wants **a client on the raw protocol**"; "the client's own `POST /account`"; "the **client** still states its own opening balances" | doc prose + a section heading, ~14 sites | the counterparty | scenario authors | 1 | The venue-side distinction being drawn is consumer-originated vs venue-originated, which the glossary's Passenger entry already words as "a venue-originated liquidation". Sweep to `consumer`. |
| Consumer | `docs/oms-types.md`: "a **client** trading several symbols books every fill ... into its own account"; "a **client-supplied** position id"; "**a peer** sending `12345678901234567890.123`"; "a **client** reading it does not have to infer"; "a **client** testing against the old venue saw the divergence"; "The venue does not gate on **your client's** configuration"; "**a client** left on the default `account_type = \"cash\"`" | doc prose + a section heading, ~15 sites | the counterparty, and in the last two the nautilus exec client specifically | consumer-facing contract | 1 | Split the sweep: the `MogwaiExecClientConfig` paragraphs legitimately talk about the nautilus CLIENT OBJECT (sanctioned survival) and should say so explicitly ("the exec client"); everything before it is `consumer`. Note the heading "The venue does not gate on your client's configuration" is externally visible. |
| Consumer | `docs/order-lists.md`: "**a consumer** cannot build a safety argument on which path a client happened to take"; "which a **client** discovers only by watching a stop it thought was reaped go on to fill"; "a slice **a client** tracks locally"; "the **client's** own cancel" | doc prose | the counterparty | order-list contract | 1 | The first site uses BOTH words in one sentence for one referent, which is the clearest evidence the sweep is owed. -> `consumer`. |
| Consumer | `docs/adapter-lifecycle.md`: "**a client** connected without a sender reports success"; "**the client** stops"; "meaning a newer connection presented **this client's** account id"; "a data client connected without starting" | doc prose | the nautilus client objects | adapter hosts | - | **No defect.** This page is about `MogwaiDataClient` / `MogwaiExecutionClient`, the sanctioned inherited spelling. It is the one file where `client` is right, and it should stay. Worth a one-line note at the top saying so, since a reader coming from the sweep will otherwise "fix" it. |
| Session | `docs/adapter-lifecycle.md`: "**eviction** - a WS 1000 close whose reason begins `evicted: `, meaning **a newer connection presented this client's account id**. Redialling would evict the claimant in turn, forever, so the client stops." | doc prose | eviction as an account-id collision | adapter reconnect policy | 1 | Under the current Eviction entry the discriminator is the SESSION, not the account id: a newer connection presenting the same account AND session coexists. As written, a host reading this expects its own data and exec legs to evict each other. Add the session clause. |
| Ledger | `docs/havoc.md`: "Order-path divergences operate on **the run ledger**" | doc prose | one ledger for the run | see 5.7 | 1 | Covered above; listed here because "run ledger" is a compound the glossary does not admit at all - Ledger is per account, Run holds many. |

## Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Transient mode | `reference/architecture.md`, opening line: "**A direct launcher** starts one foreground process and receives a versioned readiness record" | doc prose, the file's first sentence | `mogwai_protocol::launch::launch(spec)`, i.e. Transient mode | every reader's first impression of the deployment model | 2 | "Direct launcher" appears once and is defined nowhere. The glossary has Transient mode and Server mode; the opening paragraph should name both, since a reader who meets only the launcher path will not know Server mode exists. |
| Consumer / Strategy | `reference/architecture.md`: "on **a shared exchange** it does not, because **one subagent** breaching must not take down **the batch**"; `docs/havoc.md`: "on **a shared exchange**, blacking out or slowing **one subagent** leaves the rest of **the batch** untouched"; "arming a subagent before starting it" | doc prose, ~5 sites across two binding files | Server mode; one Strategy; the set of strategies under one run | the whole multi-account rationale | 2 | Three undefined words doing three jobs the glossary already names. "Shared exchange" is **Server mode**. "Subagent" is a **Strategy** (or the account it drives) - and `subagent` is borrowed from the orchestration vocabulary in `CLAUDE.md`, where it means an LLM agent, so it is actively confusing in a venue document. "The batch" is the set of accounts on one run. Sweep all three to glossary words. |
| Passenger / Session | `reference/architecture.md`: "a PASSENGER is **one connected trader**" | doc prose | passenger as a person-shaped noun | see 5.7 | 2 | "Trader" appears once. Whatever the Passenger split resolves to, the word is either Account or Session; do not introduce a fourth. |
| Boarding | `reference/architecture.md`: "**Concurrent first boarders** share one placement through a semaphore handoff"; "The first passenger **places the boat** at the river's fixed warmup origin; later passengers with the same speed **join it mid-stream**" | doc prose | boarding, and the placement it triggers | boatyard | 2 | Close to the glossary but not on it: the Boarding entry now owns "a key naming none creates the river and places a boat for it". "First boarder" is fine as a derived form; "join it mid-stream" should be "boards it", so the one verb the glossary defines is the one the document uses. |
| Divergence | `docs/havoc.md`: "The market **REGIMES** - `VolStorm`, `LiquidityDrought` and `ReopenGap` - **are not runtime arms.** They are a boot choice ... and enter the tape identity" | rule statement | a carrier-based split between boot regime and runtime arm | glossary Divergence entry; boarding and river identity | 5 | **[changed] The first-pass verdict violates the audit premise.** The glossary is the end state and deliberately says carrier decides nothing: a divergence is resolved at Boarding, generator divergences enter River identity, and nothing mutates seated water. `havoc.md` instead makes config versus runtime carrier decide the taxonomy, then separately describes `FlowSurge` mutating boatless checkpointed water. The code may currently expose `MarketRegime`, but direction 4 runs from glossary to code. Rewrite this part of `havoc.md` around Boarding and make the code/config surface converge on it; do not weaken the glossary to preserve today's type split. |
| Warmup | `reference/architecture.md`: "the boot river's **warmup is generated before readiness**, every other river's **on first read**"; `reference/clock.md`: same, twice; glossary Tape entry: same | doc prose | the split | boot latency claims | - | **No defect - and this is the corpus's best-behaved statement.** Three durable sites and the glossary all agree, in the same words. Recorded as the counter-example: it is possible. |
| - (audience) | `reference/architecture.md`: "**DELIVERY IS ATTRIBUTED, NOT BROADCAST** ... an order-scoped frame goes to the account that submitted the order, and an `AccountState` goes to the account it NAMES. What reaches every connection is what is genuinely about the venue" | rule statement | the `Audience` classification scope 3 found in `run.rs` | delivery contract | 2 | The code has an exhaustive five-variant `Audience` enum (`Venue`, `Account`, `Order`, `Unattributable`, `Requester`) and this paragraph is its informal three-way summary. `architecture.md` should name the classification and its variants, since it is the venue's invisibility property expressed as a type. |

## Direction 3 - load-bearing and undefined

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `reference/architecture.md`: "counted onto its account before the 101 and off it when **its lane** is released"; "the **outbound lane** a socket binds"; "**the lane table**"; "`FeedLagged` on the **priority lane**"; "delivered to each connection's **lanes**"; "**Admission and execution lanes** remain connection-local memory bounds" (`docs/havoc.md`) | doc prose, ~8 sites in two binding files | the per-connection outbound queues, of which there are at least three kinds (priority, execution, admission) | freeze counting, backpressure, close codes | 3 | The glossary uses `lane` once, in the Connection entry ("holds no lane and no seat"), without defining it - scope 1 flagged that. Here it is doing structural work: the account-freeze rule is STATED over lane binding and release. A reader cannot evaluate the freeze rule without knowing what a lane is. **Owed: a Lane entry naming the kinds and what each bounds.** |
| - | `reference/architecture.md`: "an **inert market remainder** left by a partial fill that is never scanned again"; "the kept remainder rested **inert**" (`docs/oms-types.md`) | doc prose | a resting-order state that no sweep touches | order lifecycle | 3 | A third resting state with no name in the glossary, and `docs/oms-types.md` uses "inert" for a bug that was FIXED while `architecture.md` uses it for a live state. Name it or drop it. |
| - | `docs/order-lists.md`: "the children leave the book and **the truth store** records them cancelled"; "the **venue-truth reports** on reconnect are what resolve the ambiguity" (`docs/adapter-lifecycle.md`); "**writes the mirror**" (`docs/order-lists.md`) | doc prose | the venue's authoritative order record, and the adapter's local copy of it | reconciliation contract | 3 | Three undefined nouns for the reconciliation seam - "truth store", "venue-truth reports", "the mirror" - across two binding documents, and reconciliation is one of the four socket-backed test binaries. **Owed: one entry (Order truth / mirror) and one word per side.** |
| - | `reference/architecture.md`: "the **fill sweep**", "the **sweeper**", "a **sweep pass**", "a **scan**", "**scanned to**", "the **frontier**", "**`last_swept_ns`**, its settlement watermark" | doc prose, pervasive | three nested granularities: pass, walk, scan | the frontier defect family `AGENTS.md` names FIRST | 3 | **[extends scope 3 and scope 4]** Both prior scopes found the frontier vocabulary undefined on the code side and both said `reference/` should own it. It is not here either. `AGENTS.md` opens its standing-lessons section with the frontier family and the glossary has no Frontier entry, no Sweep entry and no Watermark entry. This is the largest single vocabulary hole in the durable corpus. |
| - | `reference/architecture.md`: "**a shape-class refusal**"; "a shape that does not validate"; "**the resolved bundle**"; "an **instrument bundle** of knobs"; "the **operator overlay**"; "the BTCUSDT **default bundle**" | doc prose | the resolved `InstrumentDef` and the config layering that produces it | River identity (the key IS the resolved bundle digest) | 3 | The glossary's River entry hangs identity on "the resolved bundle" and never defines bundle, shape, or overlay. Three words for two things. **Owed: Shape / Bundle / Overlay, or one word for all three.** |
| - | `reference/architecture.md`: "the **fill band**", "a seeded, volatility-scaled band around its stated price", "**band-drawn trigger**", "`fill_band_vol_mult`" | doc prose, ~10 sites; also `docs/oms-types.md` "the BAND-DRAWN trigger" | the per-order randomized fill trigger | `TAPE_PROTOCOL_VERSION` bump rule names it by name | 3 | `AGENTS.md`'s unconditional bump rule lists "the fill band's draw" as a determinism-affecting artifact, so the term is load-bearing in a RULE, and the glossary does not define it. **Owed a Fill band entry.** |
| - | `reference/architecture.md`: "a **checkpoint chain**", "bounded checkpoint sets", "rivers never serialize on each other's **checkpoint chain**" (glossary River entry) | doc prose | the deterministic-replay index a river is positioned through | the 256-river cap, history cost | 3 | The GLOSSARY leans on it too ("never serialize on each other's checkpoint chain") without defining it, which makes this a glossary defect in the strict sense: an entry leaning on a word it never defines. |
| - | `reference/architecture.md`: "**the mark**", "its last mark", "`mark_px`", "mark-cadence behaviour", "**variation margin**" (defined in glossary) | doc prose | the price a position is valued at, and the cadence it updates on | risk, valuation, funding | 3 | Glossary defines Variation margin and Posted margin but not Mark, which both depend on. "Mark-cadence behaviour" is used as a term of art in the multi-symbol carve-out and appears nowhere else. |
| - | `reference/architecture.md`: "a **retryable** reject ... `RETRYABLE_REJECT_PREFIX`"; "an admission refusal means the venue was **FULL**" | doc prose | the machine-readable backpressure class | consumer backoff policy | 3 | **[extends scope 2's refusal-taxonomy finding]** Scope 2 found six spellings of "the venue is full" across three carriers. `architecture.md` states the contract for one of them and no durable document holds the taxonomy. |
| - | `AGENTS.md`: "**the intake sequence** - corpus, measurement, fit, preset"; `reference/architecture.md`: "survey what cheap data exists ... buy, preflight, measure, characterize, fit, ship a preset with its provenance" | rule statement | the offline pipeline | `docs/cli.md`'s whole toolbox tree | 3 | **[extends scope 7]** Scope 7 asked for one entry defining Corpus, Measurement, Fit, Fingerprint, Artifact, Preset and Provenance. `AGENTS.md` and `architecture.md` both name the sequence and neither defines a step. The glossary has no water-side-offline section at all. |
| - | `reference/technical-implementation-spec.md`: "**a brick**", "**Every brick.**", "A brick whose load is unproven is not laid", "the first landing", "**a keep/revert path**" | rule statement, pervasive | one atomic step of a spec | every spec written against this document | 3 | Internally consistent and well used; it is the vocabulary of the SPEC PROCESS rather than of the venue, and it is defined in place. **No defect** - recorded because "brick" leaks into command help text per scope 7 ("Brick B4", "Brick G"), where it is undefined and refers to retired notes. The word is fine here and not fine there. |

## Direction 4 - the roadmap: statements the code has not caught up with

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Seat | glossary Seat entry: "An account holds as many seats as the distinct boats its sockets have bound, so one account trades many rivers at once" | doc prose | the end state | see 5.1 | 4 | Listed for completeness: the code and three `architecture.md` paragraphs owe this. The finding is on the prose, not the glossary. |
| Passenger | glossary Passenger entry: "One per account, not per connection." | doc prose | the end state | see 5.7 | 4 / evidence for owner | **[changed] Under the task premise, the code owes the per-account object.** The remaining owner question is migration vocabulary, not whether current code overrules the glossary. Durable prose is split and the apparent per-connection majority comes mostly from duration and ticket language that can move to Connection or Seat without changing behavior. Evidence and costs are below. |
| Consumer | glossary Consumer entry: "`client` is not used for anything this project owns." | doc prose | the end state | the whole sweep | 4 | The corpus in scope carries ~99 occurrences of `client` (architecture 38, adapter-lifecycle 16, oms-types 15, havoc 14, performance 9, order-lists 4, clock 3), of which roughly 20 (adapter-lifecycle in full, plus the nautilus-config paragraphs of oms-types) are sanctioned survivals. The statement is a roadmap item today. |
| Venue | glossary Venue entry: "Whether it runs as its own PID or embedded in the consumer's own program is a deployment detail and never part of its identity" | doc prose | the end state | architecture's launcher framing | 4 | `reference/architecture.md` opens by describing the venue AS a foreground process ("A direct launcher starts one foreground process"), which is the framing the Venue entry exists to retire. The opening paragraph owes a rewrite. |

---

## Lateral findings

### Passenger split: evidence for the owner [added in independent reconciliation]

The durable corpus does not support the first pass's simple "three to two"
count. It contains two clear definitions, several behavioral uses, and one
internally split paragraph:

| reading | site | weight | worked reading |
|---|---|---|---|
| per account | `reference/glossary.md`, Passenger: "the venue-side object for one account ... One per account, not per connection" | explicit definition | The clearest statement and, under this task's premise, the end state. |
| per account | `reference/architecture.md`, opening: "A `Passenger` is created on demand, keyed by account id, and the id ... outlives the connection" | structural ownership | This cannot describe a connection-lifetime object without contradicting itself. It places Passenger at account lifetime even though the paragraph's earlier "one connected trader" phrase leans the other way. |
| per connection | `docs/havoc.md`: "the connections sharing [a boat] are its PASSENGERS" | explicit definition | The clearest contrary statement. |
| per connection | `reference/architecture.md`: "A passenger owns an uncloneable ticket for one websocket connection" | explicit ownership | This is really the Connection's boat ticket under the glossary, but today it assigns that ticket to Passenger. |
| per connection | glossary RunComplete and `reference/architecture.md`: a socket carries its own `duration_ms`, measured from its boarding instant, so passengers on one boat complete independently | behavioral use, two documents | Independent completion is necessarily per connection when one account/session has several sockets. It does not require Passenger itself to be per connection if duration moves to Connection. |
| per account | glossary Seat: an account rides many boats and the seat is counted per connection | structural distinction | The glossary already has the vocabulary needed to avoid multiplying Passenger: one Passenger, many Seats, each Seat with connection riders. |

On raw sites, the connection-side reading has four strong appearances if the
two RunComplete descriptions are counted separately; the account-side has
three. On definitions and ownership, it is two to two, with the architecture
opening internally split. So per-connection is a narrow majority of current
durable usage, not a settled corpus model. The per-account reading has the
stronger ontology because Account, Connection and Seat already carry the three
lifetimes separately.

Cost to move to per account, the glossary end state:

- Rename the per-socket ticket owner and duration subject to Connection, and
  describe boat riders as connections or seat riders in `architecture.md`,
  `havoc.md`, the RunComplete entry and adapter close prose.
- Consolidate any code object actually named `Passenger` at account lifetime,
  or rename the current connection-lifetime type and introduce the account
  object if none exists. Preserve per-connection delivery, byte budgets,
  transport havoc and duration; this is an ownership and vocabulary migration,
  not a reason to merge those semantics.
- Audit boarding resolution carefully: the glossary currently says Passenger
  config is resolved at Boarding and is constant for that connection, while
  one Passenger may have several connections. Per-connection resolved inputs
  need a connection/seat-owned boarding record, with account-side engine arms
  applied to the Passenger's ledger.

Cost to move to per connection:

- Rewrite the glossary Passenger entry and every Account, Seat, Boarding,
  Divergence and Eviction sentence that relies on one Passenger per account.
  Invent a new name for the account-lifetime venue object, because Account is
  presently the id plus state while Passenger is the riding object.
- Rewrite the architecture opening's keyed-by-account, outlives-connection
  ownership claim and the glossary's invisibility argument over passengers.
- Reconcile RunComplete easily, but pay a much larger conceptual cost across
  multi-river accounts: several per-connection passengers would share one
  ledger and risk state, so "passengers owe each other non-interference" would
  become false unless narrowed to passengers of different accounts.

Evidence therefore favors keeping the glossary's per-account Passenger and
moving connection-lifetime jobs onto Connection and Seat. That is evidence,
not an owner ruling.

1. **Two binding documents cite `notes/`, which `AGENTS.md` forbids outright.**
   `docs/havoc.md`: "The real fix is a declared feed-gap event upstream; it is
   filed in `notes/todo.md`." `reference/performance.md`: "Filed as an
   owner-level item in `notes/todo.md` rather than tightened here." The rule is
   unambiguous - "nothing durable may cite `notes/` - not a code comment, not
   `docs/`, not `reference/`" - and `notes/todo.md` carries no truth guarantee,
   so both sentences point a reader at a document that may be wrong. Both are
   easy fixes: state the gap and stop, or state the decision.

2. **`reference/architecture.md`'s tape-version narrative stops at 18 and then
   asserts 23.** It walks versions 5 through 18 in prose, and separately states
   "`TAPE_PROTOCOL_VERSION` is 23". Five bumps landed with no durable record of
   what they changed - which is the exact gap the version-narrative exists to
   close, since the whole point of the section is that a reader can tell whether
   a given bump moved their tape. Either the narrative is completed or it is
   retired to git history and the section says so.

3. **The prose gate is narrower than the risk it was built for.**
   `tape_version_prose.rs` checks two phrasings of the tape identity. Every
   other live fact in the durable corpus - the order-type count, the instrument
   class count, the endpoint spellings, the account-river rule - is unguarded,
   and this scope found four of them stale in one file. The gate's design
   generalizes cheaply: a `docs/`-and-`reference/`-wide check that the order-type
   list and the class list match a constant would have caught 5.2 outright.

4. **`docs/havoc.md` needs a rewrite, not edits.** Its opening five paragraphs
   are pre-multi-account, pre-boatyard text (one run ledger, run-scoped havoc,
   passenger-as-connection, river-as-symbol) and the rest of the page argues
   against them. Four of this scope's direction-5 findings are in that page, and
   three of them are the page contradicting itself. Rewrite it against the
   glossary's three-way arm classification, and drop the two-noun primer
   entirely - the glossary is the primer now, and a second worse copy of it in a
   `docs/` page is precisely how the two drifted.

5. **`reference/architecture.md` is 1174 lines and carries at least six
   internal contradictions.** It is doing four jobs at once: the venue's design
   rationale, the ledger and instrument model, the tape-version history, and the
   workspace/offline-toolbox tour. The contradictions all sit where one job's
   old text survived another job's landing. It should be split - venue design,
   execution and ledger model, tape lineage, workspace - and the split is the
   cheapest structural fix available, because it makes each stale paragraph
   visible next to the paragraph that superseded it.

6. **`docs/` has no accounts page.** `POST /accounts`, the account policy shape,
   the bearer-token property of an account id, the freeze and the TTL are all
   documented only in `reference/architecture.md`, which is the how-it-is-built
   document. A consumer wiring an account reads the wrong folder. Scope 7
   proposed `docs/accounts.md` from the config side; this scope reaches the same
   conclusion from the prose side.

7. **The glossary has no water-side-offline section and no wire vocabulary
   beyond two entries.** Scopes 1, 2, 4 and 5 all converged on a
   `reference/wire-vocabulary.md`; scope 7 asked for an intake-sequence entry.
   The glossary's Wire section defines `ReadyRecord` and `RunComplete` and
   nothing else, while the durable corpus in this scope leans on `FeedLagged`,
   `AccountState`, `OrderExpired`, `AdmissionRejected`, `SubmitOrderGroup`,
   `OrderTriggered` and a heartbeat frame that is named nowhere else.

8. **Not a defect, recorded as the one clean cross-document contract.**
   `mogwai_protocol::close`'s reason vocabulary is stated in
   `reference/architecture.md` (the venue writes, `classify` reads, an
   unrecognized reason is not terminal) and consumed correctly in
   `docs/adapter-lifecycle.md` (three terminal reasons, everything else
   redialled), with no literal duplicated between them. It is the shape
   `AGENTS.md` prescribes and the only place in this scope where two binding
   documents describe one mechanism without drifting.
