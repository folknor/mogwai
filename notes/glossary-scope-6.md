# Glossary scope 6 - `mogwai-adapter`'s public surface

Inventory only. Nothing in the crate was edited.

**Second-pass reconciliation (2026-08-21).** This pass read the revised
glossary and formed an independent inventory from the adapter source before
opening this report, specifically to resist anchoring on the first pass. Row
annotations use **[second pass added]**, **[second pass changed]**, and
**[second pass refuted]**. Refuted rows remain in place with the reading that
refutes them.

Read against `reference/glossary.md` as revised 2026-08-21 (Boarding is an entry,
Client became Consumer, River and Divergence clarified). Read alongside
`notes/glossary-scope-1.md` through `-5.md` and the carry-forward in
`notes/glossary-reconciliation.md`.

Surface covered: `lib.rs`, `factories.rs`, `config.rs`, `client.rs` and
`client/{data,exec,shared}.rs`, `clock.rs`, `convert.rs`, `lifecycle.rs` -
every `pub` item, every doc comment, and every `tracing::*` / `anyhow::bail!` /
`ensure!` string a host operator can read. Test bodies are not public surface
and are inventoried only where a test name or fixture asserts a vocabulary fact.

## Where this scope sits relative to the others

Scopes 1-5 barely touched this crate. The only prior row naming it is
scope 5's `bars.rs` row, which observed that `mogwai-data` prose says "the
client `HavocFilter`" about a type that lives here. This scope confirms that
row from the other side and extends it: `HavocFilter`, `client_havoc`,
`HavocFilter::from_client` and the whole inbound-havoc pipeline are
adapter-owned and carry the ruled-wrong `client` sense.

This scope also **contradicts nothing** in scopes 1-5. It extends scope 1's
admission/refusal cluster across the boundary (the wire's `retryable` becomes
`RETRYABLE_REJECT_PREFIX`, a public string contract this crate owns), and it
extends the `client` classification with two adapter-only senses the ruling
does not yet cover: **the socket leg** (`client = label` where the value is
`"data"` / `"exec"`) and **the transport generation**.

## The boundary, stated before the rows

Nautilus's names are inherited and are never findings: `DataClient`,
`ExecutionClient`, `ClientId`, `ClientConfig`, `DataClientFactory`,
`ExecutionClientFactory`, `ClientOrderId`, `VenueOrderId`, `TradeId`,
`AccountId`, `TraderId`, `StrategyId`, `PositionId`, `OrderStatus`,
`OrderSide`, `OrderType`, `TimeInForce`, `TriggerType`, `LiquiditySide`,
`OmsType`, `AccountType`, `BarType`, `BarAggregation`, `UnixNanos`,
`OrderStatusReport`, `FillReport`, `PositionStatusReport`,
`ExecutionMassStatus`, `TimeEvent`, `Clock`, and the `MogwaiDataClient` /
`MogwaiExecutionClient` pair the glossary's Consumer entry names explicitly.

`MogwaiDataClientFactory`, `MogwaiExecutionClientFactory`,
`MogwaiDataClientConfig`, `MogwaiExecClientConfig` and `MogwaiClock` are ours
but named for the inherited object each produces or implements. I do **not**
propose renaming them: the quarantine works precisely because our type is named
after the trait it satisfies. What follows is the quarantine LEAKING - our
vocabulary bent to nautilus's shape, or nautilus's word adopted into prose and
log fields we own.

---

# Direction 4 - entries the code has not caught up with

These are the roadmap. Every one of them is a gap in the code, never a case
against the entry.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boat, Connection | `config.rs` `ws_url` | fn (builds the only URL either client dials) | the upgrade URL carries `account`, `symbol` and `session` and nothing else | externally visible | 4 | **THE ADAPTER CANNOT NAME A SPEED.** The Connection entry defines a connection as "one WebSocket under an account, bound to one river **at one speed**", and the Boat entry makes speed the second half of a boat key. `mogwai-server`'s `ws.rs` accepts `?speed=` and documents that an unserved speed places a second boat on the same water. `ws_url` emits no `speed` parameter and `MogwaiDataClientConfig` / `MogwaiExecClientConfig` carry no speed field, so every adapter-driven connection silently takes `state.cfg.speed`. Add `speed: Option<f64>` to both configs, carry it on the upgrade, and validate it the way the venue does. Note the coupling: both legs must carry the SAME speed or the venue refuses the second with "a ledger carries one cadence". |
| RunComplete | `config.rs` `ws_url`; `lifecycle.rs` `Terminal::DurationComplete` arm | fn, log text | the adapter handles a `DurationComplete` close it can never cause | externally visible | 4 | **THE ADAPTER CANNOT NAME A DURATION.** The RunComplete entry says "A socket may carry its own `duration_ms`, measured in simulated milliseconds on its boat's clock from its own boarding instant, so passengers on one boat complete independently." `ws.rs` reads `?duration_ms=`; `ws_url` never writes one. The lifecycle already has a log line for the resulting close (`"this connection's configured duration elapsed"`) that no adapter-built connection can reach, which is exactly the "control that is itself vacuous" shape. Add `duration_ms` to both configs. |
| Boat, Warmup | `clock.rs` `fetch_clock`, `mogwai_clock_factory`; `shared.rs` `fetch_clock_or_identity` | fn | fetches bare `GET /clock`, with neither `symbol` nor `speed`, then keeps only `.sim` | externally visible | 4 | **[second pass changed] The adapter reads a clock that is explicitly not its boat's.** Each boat carries its own `SimClock`, and `mogwai-server` selects one with `/clock?symbol=&speed=`. A symbol alone is insufficient once several boats read one river at different speeds. Both client connections bind through a URL that currently carries neither speed nor duration, while every `ts_init`, havoc deadline, quota interval, backoff, timeout, and `MogwaiClock` timer is derived from this bare run-level answer. Pass the same resolved symbol and quantized speed used at Boarding to `/clock`; for the boot river, the adapter still needs an explicit way to identify the boat rather than treating an omitted symbol as sufficient. `mogwai_clock_factory(http_base)` has no config argument at all, so fixing only the two client connect paths leaves the host clock on the wrong axis. |
| Boat | `mogwai_protocol::ServerClock::boat_clock`, as consumed here | field | the venue's own flag saying "this answer is a boat's" | across a crate boundary | 4 | **A FLAG WITH NO READER.** `boat_clock` is written by the venue, is asserted on by four `serving.rs` tests, and is read by nothing in this crate. `shared.rs`'s identity fallback sets it `false` under the comment "A synthesized fallback is not a boat's answer either" - so the crate constructs the field and then discards the distinction it exists to carry. It reads as gated and is not. Once the clock fetch names a symbol, this flag is the check that the venue actually answered for the boat, and it should refuse or warn when it comes back `false` for a named river. |
| Seat | `client/data.rs` `subscribe_symbol`; both config types' singular `symbol` | method, fields, error text `"subscription symbol {symbol} does not match the symbol this connection is bound to ({bound})"` | one data client serves exactly one river | externally visible | 4 | **[second pass changed]** The Seat entry's many-rivers shape is not expressible through one registered client pair: both inherited Nautilus objects are configured around one singular symbol, and the data client refuses every other subscription. Several client pairs under one account and session can technically create several seats, but no builder or public doc exposes that as the supported composition, and factory registration normally presents the pair as the venue integration. Treat this as an API-shape gap, not merely missing documentation: either make multi-pair composition an explicit public builder with shared account/session invariants, or give the registered pair a multi-connection, multi-river shape. |
| Freeze | `mogwai_protocol::ReadyRecord::account_ttl_ms`, `reset_account_on_reconnect`, as consumed by `config.rs` `for_run` | fn | `for_run` reads `record.addr` and `record.run_seed` and discards the rest | externally visible | 4 | The Freeze entry makes `account_ttl_ms` the bound on a frozen account's survival, and Ledger says "a ledger outlives the connection that named it, which is what makes a reconnect a continuation". The adapter's `ReconnectPolicy` can back off arbitrarily far past that TTL and has no idea the ledger it is redialling may have been collected - and `reset_account_on_reconnect` is a run-level fact the adapter's mirror would want to know before it treats a reattach as a continuation. `for_run` should carry both, and the reconnect loop should say so when its backoff crosses the TTL. |
| Ledger, Freeze | `lifecycle.rs` reattach comment; `client/exec.rs` `connect` "Scope:" comment | doc prose | an internal redial replays WS commands only and never re-pulls `GET /account` | crate-local, externally observable | 4 | **[second pass refuted]** The first-pass inference crosses the venue/adapter boundary. The Ledger entry promises that the venue's authoritative ledger survives a connection loss; the code does preserve that continuation. It does not promise that the adapter's event-derived mirror is refreshed on reconnect, and the Divergence entry expressly permits `DropNextAccountUpdate` to corrupt that view. Re-pulling would be a reconciliation policy question, not evidence that the ledger failed to continue. The separate TTL/reset row remains valid because collection or configured reset can destroy the ledger continuation itself. |
| Instrument class | `convert.rs` `instrument_any`, `InstrumentClass::Equity` arm | fn | builds `nautilus_model::instruments::Equity` with `None` for lot size and every optional | across a crate boundary | 4 | The Instrument class entry defines `equity` as "a share - held as a position, paid for in one currency, carrying **lot size, borrowability and a settlement period**". The conversion passes `None` for all of them, so the three facts the entry says an equity carries do not survive the seam. Either the wire `InstrumentClass::Equity` gains them and the conversion forwards them, or the entry is describing an instrument class the venue does not yet model - and per this arc, the first reading is the operative one. |
| Venue, Run, Ledger | `client/exec.rs` `note_account_label` doc: "One venue is now one run is one LEDGER: the connection carries the only account there is" | doc prose attached to a live conversion seam | treats the whole run as one account and one ledger | crate-local, maintainer-facing | 4 | **[second pass added]** This is retired architecture stated as current fact. The Venue entry says one run has many accounts, and Ledger says one engine is owned by one account. The function's present behavior, retaining the configured account id rather than adopting a returned one, may still be correct, but its proof is not: a mismatched snapshot can once again mean the wrong account was fetched or routed. Rebuild the check around the current per-account endpoint and bearer account id instead of preserving a proof whose premise is false. |
| Divergence, Boarding | `client/exec.rs` `ship_server_havoc`, called once from `connect`; `HavocSpec.server` as consumed here | fn, config conversion | serializes each `Divergence` bare and POSTs it before the execution connection boards | externally visible | 4 | **[second pass changed] The adapter discards the boarding scope.** The venue request envelope accepts `account` for passenger-view arms and `symbol` for water-changing arms, but `ship_server_havoc` serializes only the flattened divergence. Transport arms therefore default venue-wide instead of riding the configured account's passenger, while generator arms default to the boot symbol or are refused once boats exist instead of contributing to the configured river key. Only the execution object posts, even though both configs accept the same `HavocSpec`, and an internal redial does not resolve a fresh constant-per-connection set. The carrier arriving before Boarding is correct under the glossary; dropping the account, symbol, and connection association is not. Replace the bare post loop with one boarding-spec path that carries the resolved account, river inputs, speed, duration, and classified divergences together. |
| Session, Consumer, Eviction | `config.rs` `process_session_id`, `default_session`, `with_session`; glossary Consumer, Session, and Eviction entries | public config behavior and glossary contract | defaults every adapter object in one process to one session, while callers may override it | externally visible | glossary defect | **[second pass added] This is a contradiction between glossary entries, not a code gap against one coherent end state.** Consumer says one consumer may be several processes and that the venue never perceives it. Session says the asserted identity is minted once per adapter process. Eviction then equates a different session with a different consumer and promises the same consumer's sockets coexist. Two processes belonging to one consumer receive different default sessions and evict each other unless an external coordinator overrides both with the same value. The code already exposes that escape hatch, although `with_session` documents only the opposite use. The glossary must choose an operational identity: either coexistence means same session, not same consumer, or a consumer-wide session must be supplied and shared across processes. The venue cannot enforce an identity it expressly cannot perceive. |

---

# Direction 1 - a glossary term doing a job that is not its own

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Consumer / Session | `config.rs` `process_session_id` doc: "This PROCESS's **client** identity", "The venue seats one **client** per ledger and evicts a second one", "one worker process is one **client**", "that **client** reconnecting", "a RESTARTED worker gets a fresh one" | doc prose | the self-asserted identity on `/ws?session=` | externally visible (rustdoc) | 1 | This is the Session entry's subject, described entirely in the retired word. Five occurrences in one doc block. Rewrite in Session and Consumer: "this process's session id", "the venue seats one session per account". |
| Consumer / Session | `config.rs` `MogwaiDataClientConfig.session` doc "The CLIENT identity presented on `/ws?session=`, so this process's several sockets on one ledger are not read as several **clients**"; `MogwaiExecClientConfig.session` doc "The CLIENT identity" | field doc | same | externally visible | 1 | Same substitution. "CLIENT" in shouting caps is the emphasis of the retired word. |
| Consumer / Session | `config.rs` `with_session` doc (both configs): "Override the **client** identity... set it only to make two **clients** in ONE process deliberately distinct **clients** of one ledger" | method doc | two sessions inside one process | externally visible | 1 | "two sessions in one process, which is a request to have them evict each other". |
| Consumer | `config.rs` `MogwaiDataClientConfig.symbol` doc: "`None` takes the server's boot symbol for compatibility with **clients** predating the carrier" | field doc | older builds of THIS adapter | externally visible | 1 | Neither inherited sense. The word wanted is "adapter builds"; and see the Boot-symbol row below for the second defect in the same sentence. |
| Consumer | `client/shared.rs` `run_identity_check`: `tracing::warn!(client = label, ...)` and the message "this **client** cannot tell its venue from another holding the same address. Build the config with `for_run`..." | log field, log text | `label` is `"data"` or `"exec"` - a socket leg | externally visible (operator log) | 1 | **A NEW `client` SENSE the ruling does not cover: the socket leg.** Worse, it disagrees with itself: `lifecycle.rs` emits the SAME value as `socket = label`, and `WsConnectionConfig.label`'s own doc says "Names this **socket** in the connection-lifecycle log lines". One value, two field names, in log lines an operator correlates. Make it `socket = label` everywhere. |
| Consumer | `lifecycle.rs` `verify_run_identity`, `Unidentified` arm: "this **client** cannot verify it is the one it was launched against"; `Unreachable` arm text | log text | the connection | externally visible | 1 | Same substitution; "this connection". |
| Consumer | `lifecycle.rs` identity-mismatch ERROR: "so this **client** is giving up rather than trading against it"; `run_ws_connection` doc "Drives one **client** socket for the life of the **client**" | log text, doc | the connection and its owning adapter object | externally visible | 1 | The doc sentence uses the word twice for two different things in eight words. |
| Consumer | `client/exec.rs` `note_account_label`: "the venue labels its ledger differently from this **client**; using the configured id" | log text | the adapter | externally visible | 1 | "this adapter". |
| Consumer | `client/exec.rs` `handle_account_state` comment: "a venue and a **client** that named the account differently produced a **client** whose balances silently stopped updating" | doc prose | the adapter | crate-local | 1 | Same. |
| Consumer | `client/exec.rs` `AdmissionSubject::SubmitGroup` ERROR: "venue refused an order group this **client** cannot attribute to its legs" | log text | the adapter's mirror | externally visible | 1 | "this adapter". |
| Consumer | `client/shared.rs` `warn_missing_instrument_once` doc: "The set belongs to one data **client**, so another venue **client** still reports its own first miss" | doc prose | the `MogwaiDataClient` object | crate-local | 1 | This one IS the inherited sense (the nautilus adapter object) and is fine. Recorded so the sweep does not over-apply. |
| Consumer | `mogwai_protocol::ClientHavoc`, `HavocSpec.client`, consumed here as `client_havoc(&spec)`, `HavocFilter::from_client(client: &ClientHavoc)`, `let client_havoc = ...`, comment "the data client accepts the same config object but only applies its **client**-side transport half" | fn, param, local, doc | havoc the ADAPTER applies to its own inbound stream | across a crate boundary | 1 | Already ruled: not the consumer sense, takes an adapter or inbound word. **This scope adds that the adapter is where the wrongness is worst**, because here the same paragraph uses `client` for the adapter object AND for this havoc bucket. `inbound_havoc` / `HavocFilter::from_inbound`. Extends scope 5's `bars.rs` row and scope 1's row 38. |
| Venue | `config.rs` `base_url` doc (both configs): "Base URL of the running **mogwai-server**" | field doc | the venue's address | externally visible | 1 | `server` is retired for the process. "Base URL of the running venue." (The crate name `mogwai-server` is a separate, already-recorded question.) |
| Venue | `config.rs` `MogwaiExecClientConfig.symbol`: "`None` takes the **server default**"; `MogwaiDataClientConfig.symbol`: "`None` takes the **server's boot symbol**"; `ws_url` doc "An absent symbol takes the **server's** boot river" | field docs | binding the boot river | externally visible | 1 | Three spellings of one fact, two of them wrong. The glossary names it: **boot river**. Also note the exec config's version says only "server default", which does not even say which default. |
| Venue | `client/shared.rs` `fetch_clock_or_identity` ERROR: "if the **server** runs at speed != 1..."; `warn_missing_instrument_once`: "(**server** config change or later-added instrument)"; `seed_instruments` doc; `client/exec.rs` "**server** predates GET /account" (twice); `client/data.rs` "the **server** may have refused an off-tape window" (twice), "a **server** that kept answering full pages" | log text, doc prose | the venue | externally visible | 1 | Systematic. Every one is `venue`. The `"server predates GET /account"` pair is operator-facing and appears in both `connect` and `generate_position_status_reports`. |
| Venue | `client/data.rs` `subscribe_bars` / `request_bars` errors: `"mogwai only supports time based external bars"`, `"mogwai does not support Week/Month/Year bars"`; `client/shared.rs` `ensure_on_tape`: `"the mogwai tape cannot serve before its origin"` | error text | the venue's refusal | externally visible | 1 | The refusing party is the venue - and in these three cases it is actually the ADAPTER refusing on the venue's behalf, which the wording hides. Say which: "this adapter aggregates only time-based bars". Note also the lowercase `mogwai` against the `MOGWAI` used in `convert.rs`'s refusals. |
| Tape | `client/shared.rs` `ensure_on_tape`, `"requested start ... precedes data_origin_ns ...; the mogwai **tape** cannot serve before its origin"`; `client/data.rs` `"refusing an off-**tape** window"` (x3), `"mogwai's synthetic **tape** has multi-hour arrival droughts"` | fn name, error text, log text | the generated data of one river, queried over HTTP history | externally visible | 1 | The Tape entry defines tape as **what a boat publishes** - the paced frame stream broadcast to that boat's passengers. History reads a **river** directly and nothing has to be boarded for it (River entry). So every one of these names the river, not the tape, and `ensure_on_tape` is checking a river's origin. `ensure_on_river` / "off-river window" reads oddly; the honest fix is probably to name the bound directly: `ensure_within_river_history`, "requested start precedes the river's origin". The one CORRECT use in the crate is `client/data.rs`'s and `client/exec.rs`'s delivery-barrier comment "the frames are real tape", which is exactly a boat's broadcast. |
| Warmup | `client/data.rs` `request_bars`: "refusing an off-tape **warmup** window", "the **warmup** may not splice contiguously into live", "let the venue run further past its epoch before starting the **warmup**" | log text | the consumer's historical bar request | externally visible | 1 | Warmup is the venue's `data_origin_ns .. run_start_ns` span, a property of a river. A `request_bars` window can sit anywhere and is not that span. The adapter is borrowing the venue's word for the strategy's own bootstrap. "history request" / "history window". The Warmup entry does say "where a strategy's warmup bars come from", which is what makes this borrowing tempting and still wrong: the bars come from the warmup, they are not it. |
| Seat, Eviction | `config.rs` `process_session_id` doc: "The venue **seats one client per ledger** and evicts a second one" | doc prose | why both legs must share a session | externally visible | 1 | **Contradicts two entries.** Seats are per BOAT, not per ledger - "an account holds as many seats as the distinct boats its sockets have bound". And the venue does not evict "a second one": it evicts a socket presenting a DIFFERENT session, and the same session's sockets coexist, which is the very property this doc is explaining. The doc states the rule it exists to explain incorrectly and then relies on the correct rule two sentences later. Rewrite against the Session and Eviction entries. |
| Boarding | `config.rs` `with_symbol` doc (both configs): "Name the river this client's socket **binds**"; `client/data.rs` and `client/exec.rs` "**Binding** is what REGISTERS an unconfigured symbol server-side"; `client/exec.rs` "the post-**bind** reseed" | method doc, comments | the connect-time act by which the configured connection selects its water | externally visible | 1 and 2 | **[second pass changed]** `bind` is the adapter's word for Boarding, the one moment water identity is resolved. The first pass's claim that the adapter is the only workspace component that performs it is too broad: the venue actually resolves the river key and calls `boatyard.board`; the adapter initiates that act by constructing the upgrade. The vocabulary finding survives intact on this boundary: use "the river this connection boards", "boarding registers an unconfigured symbol", and "post-boarding reseed" so the adapter's public builder names the glossary job it initiates. |
| Account / Ledger | `config.rs` `DEFAULT_ACCOUNT_ID` doc "Default account **label**"; `validate_account_id` doc "the account **label**"; `client/exec.rs` `note_account_label` | const doc, fn, fn name | the account id | externally visible | 1 | The Account entry calls it an **id** and makes the point that "the id is the consumer's, not minted" and is a bearer token. `label` is used here for the deliberate reason that the venue's reported id is decorative to this client - but three sites say `label` for the id the client itself CONFIGURES, where it is not decorative at all: it names the ledger. Keep `label` only in `note_account_label`, where the distinction is the whole point, and say `id` everywhere else. |

---

# Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boarding | see the `bind` row above | - | - | - | 2 | Cross-listed; it is both a wrong word for a named job and a named job under a different word. |
| Connection | `lifecycle.rs` `label`, `socket = label`; `run_ws_connection` "one client socket"; `client/data.rs` "one transport **generation**"; `client/exec.rs` `retire_connected_flag` doc "the current transport generation"; `config.rs` "this process's several **sockets**"; "both **legs**" (config.rs x4, exec.rs) | field, doc prose, log field | one WebSocket under an account | externally visible | 2 | The Connection entry names this exactly, and the crate has four words for it: socket, leg, transport generation, and (per direction 1) client. `leg` is genuinely useful and means something the glossary does not: **one of the two connections a nautilus consumer necessarily holds on one account**, data and exec. That is worth an entry of its own rather than a rename. `socket` and `connection` should collapse to Connection; `transport generation` is a real and distinct concept (see the cluster below). |
| Divergence | `client/exec.rs` `ship_server_havoc`; `shared.rs` `client_havoc` / `conn_havoc`; `config.rs` `validate_havoc` doc "the **client-side** probabilities, the **connection-lifecycle** knobs, the optional market regime, and every armed **server** `Divergence`" | fn, doc | the adapter's three-way split of a `HavocSpec` | across a crate boundary | 2 and 3 | The Divergence entry classifies arms by **what they change**: generator arms change the water, transport arms corrupt what one account's connections receive, engine arms queue one-shot execution divergences, windowed account-side arms apply for their span, `FaultTape` stands alone. The adapter's split is by **where the knob is enforced** (adapter-side / connection-lifecycle / venue-side). Neither is wrong, but they are two taxonomies of one thing with no stated relation, and only one is in the glossary. At minimum the adapter's three buckets should be named against the entry's axis; better, the glossary should state that the wire's three-way split is an enforcement split and how it maps. |
| Run | `lifecycle.rs` `IdentityOutcome`, `RunIdentityCheck`, `IDENTITY_UNREACHABLE`, `IDENTITY_NOT_REPORTED`, `run_identity_check`, "which run it is serving" | type, const, fn | proving the venue at an address is the same Run | externally visible | 2 | Conformant, and worth recording as such: this machinery uses **Run** correctly and consistently, and `for_run` / `expected_run_seed` are the best-named things on the surface. The only stray is `verify_run_identity`'s "venue identity mismatch" ERROR headline, which says *venue* where the mismatch is between *runs* - the body gets it right ("this address is serving a different run"). Fix the headline to "run identity mismatch". |
| Eviction | `lifecycle.rs` `Terminal::Evicted` arm: "another connection claimed this account and the venue evicted this socket; reconnect disabled, because redialling would evict the claimant in turn" | log text | eviction | externally visible | 2 | **Fully conformant with the Eviction entry**, including the entry's closing rule that "a consumer must not treat it as a reason to redial, or it evicts whatever evicted it" - the code disables reconnect and the log states the reason. Recorded because it is the one place the adapter reads as if it were written from the glossary. |

---

# Direction 3 - load-bearing and undefined

Reported at cluster level first, as the brief asks. These are whole vocabularies,
not isolated nouns, and every one of them is operator-facing at least in part.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Mirror / venue truth cluster | `client/exec.rs`: `ExecState`, `OrderRecord`, `VenueQuery`, `mirrored_order_status`, reconciliation generators and logs | types, methods, operator prose | the adapter's fallible event-derived belief versus authoritative ledger queries | externally visible | 3 | **[second pass added at cluster level]** Load-bearing across order dispatch, query, and reconciliation, but undefined. Define Mirror and state that Ledger is the authority it is reconciled against. |
| Snapshot staleness cluster | `client/exec.rs`: `account_ts_last`, `admit_account_snapshot`, whole/degraded, watermark, duplicate-fill guards | fields, methods, logs | which account and order observations may advance local belief | externally visible | 3 | **[second pass added at cluster level]** Define whole and degraded snapshots and the success-guarded watermark rule. |
| Connection-generation cluster | `lifecycle.rs`; both clients' `retire_connected_flag` | lifecycle types and prose | one dial-to-disconnect incarnation of a Connection | cross-module | 3 | **[second pass added at cluster level]** A generation is neither a Connection nor a Run. Define it, then reserve connect/disconnect for the Connection and dial/redial for generation changes. |
| Receipt-book cluster | `lifecycle.rs`: `unwritten`, `on_undelivered`, `send_reattach_commands`, residue logs | transport mechanism and operator prose | proof of commands accepted locally but not proven written | externally visible | 3 | **[second pass added at cluster level]** Define receipt and undelivered. The mechanism synthesizes host-visible rejection events, so this is contract vocabulary rather than implementation color. |
| Delivery-pipeline cluster | `client/shared.rs`; both client connect paths | types, tasks, barriers and logs | adapter-side filtering, delay, reordering, and gated dispatch of received frames | externally visible | 3 | **[second pass added at cluster level]** `pump`, `barrier`, `drain`, `sink`, `held`, and `black-holed` carry correctness distinctions with no shared definition. |
| History-paging cluster | `client/data.rs`: windowed fetchers, cursors, limits, and truncation logs | query machinery and operator prose | complete timestamp-group pagination and its bounded failure modes | externally visible | 3 | **[second pass added at cluster level]** Define timestamp group, cursor frontier, truncation, and the condition currently called a `same-ts wedge`; the current log makes an operator decode implementation slang. |
| Retryable-refusal cluster | public `RETRYABLE_REJECT_PREFIX`, `mark_retryable`, transport rejection paths | public string contract, methods, logs | distinguishes capacity backpressure from business refusal and transport undelivery | externally visible | 3 | **[second pass added at cluster level]** Extends scope 1's refusal taxonomy. A public prefix consumers branch on needs a glossary definition and stable mapping to each event path. |
| Instrument-cache cluster | `client/shared.rs`; data and execution post-boarding reseeds | cache machinery and prose | the adapter definition map, Nautilus cache, and unknown-symbol holding areas | cross-module | 3 | **[second pass added at cluster level]** Two caches and an orphan quote store are called `cache`, while seed/reseed sounds like served-symbol admission despite explicit prose denying that reading. Define or rename each store by ownership and job. |

### The mirror / venue-truth cluster - the biggest gap on this surface

`ExecState`, `OrderRecord`, "the mirror", "the reconciliation mirror",
"venue truth", "venue-truth query", `VenueQuery`, `mirrored_order_status`,
`with_order_record`, `order_record`, "the mirror does not know this order",
"stale mirror", "terminal mirror record", "the mirror should be reconciled
against venue truth", `PendingQueries`, "waiter", "straggler", "unsolicited
snapshot", "correlation id".

The whole execution half of this crate is organised around a two-witness
scheme - the adapter's local belief about orders versus what the venue's engine
book says - and the glossary has no word for either witness. "Venue truth"
appears in eleven doc blocks and two operator-facing ERROR strings as if it
were defined vocabulary. The Ledger entry is the closest thing (the venue's
authoritative per-account book) but it names the venue's side only, and the
mirror is exactly the thing that can disagree with a ledger.

This is the cluster most worth an entry. Suggested shape: **Mirror** - the
adapter's own belief about an account's orders and account state, populated by
the same event stream havoc corrupts, never authoritative; and a sentence in
the Ledger entry naming the ledger as what a mirror is reconciled against.

### The staleness / watermark cluster

`account_ts_last`, `admit_account_snapshot`, "the staleness watermark",
"whole" vs "degraded snapshot", "forwarding a degraded account snapshot without
advancing the staleness watermark", "forward-only `ts_last`", "terminal-state
guard", "regress", `seen_trades`, "duplicate fill".

The frontier rule from `AGENTS.md` is implemented here about as carefully as
anywhere in the workspace, and none of its vocabulary is defined. "Whole" and
"degraded" in particular are a two-valued classification of an account snapshot
with an operator-facing WARN and no definition anywhere.

### The transport-generation cluster

"transport generation", `retire_connected_flag`, "retire", "the retired
reader", `AbortOnDrop`, "abort then join", "reattach", "redial", "dial",
"proven connection", "unproven cycle", "the attempt counter", `ReconnectPolicy`,
"backoff", "jitter", "the idle timeout", "heartbeat".

`generation` is genuinely load-bearing: it is what makes the swapped `Arc` in
`retire_connected_flag` correct, and it is neither a Connection nor a Run. It
deserves a definition or a rename onto Connection with the reconnect loop's
generations made explicit.

### The receipt-book / undelivered cluster

"THE RECEIPT BOOK", `unwritten`, `lock_unwritten`, `on_undelivered`,
`send_reattach_commands`, "residue", "the untried remainder", "accepted for
writing", "a command the socket swallowed", "double-reject",
"the venue websocket dropped with commands still queued; reporting them
undelivered rather than replaying them onto the next connection".

An excellent mechanism with entirely invented vocabulary, none of it shared with
the venue side (which has its own `spawn_exec_pump` and its own words for the
same hazard). Worth checking at the merge whether the venue's outbound
equivalent has a second vocabulary for one concept - that is the
two-implementations-one-quantity shape `AGENTS.md` warns about.

### The delivery-pipeline cluster

`HavocFilter`, "the latency pump", `spawn_latency_pump`, `HavocDelivery`,
`havoc_deadline`, "arrival-anchored deadline", "the delivery barrier",
`delivery_ready` / `pump_ready`, `enqueue_havoc`, `flush_havoc_into_pump`,
"held" (the reorder slot), "the drain", "the sink", "black-holed".

"Black-holed" appears in an operator WARN
(`"its data is black-holed until the instrument is seeded"`). "The delivery
barrier" is a real invariant with a two-paragraph comment in each client and no
name outside them.

### The history-paging cluster

`fetch_trades_windowed`, `fetch_quotes_windowed`, `final_ts_group_start`,
"page", "short page", "full page", "the trailing group", "the cursor",
"truncated", **"same-ts wedge"**, `bar_span_reached`, `capped_limit`,
`MAX_TRADES_PER_REQUEST`, `MAX_TRADE_PAGES`, `MAX_HISTORY_LIMIT`.

"wedge" is the worst of these: it appears in three operator-facing WARN strings
(`"window truncated before its end (trade limit reached or same-ts wedge)"`)
and is defined in no artifact. It names the case where a whole page shares one
`ts_event`, so a timestamp-only cursor cannot advance without losing rows.
Either define it or say what it is in the log line.

### The refusal-marker cluster - extends scope 1's admission/refusal cluster

`RETRYABLE_REJECT_PREFIX` (`"[retryable] "`), `mark_retryable`, "a business
rejection", "backpressure", "the venue was FULL", "quarantine",
`AdmissionSubject`, `synthesize_transport_reject`, `reject_for`,
`report_undelivered_command`, "transport reject", "spurious-reject window".

This is where scope 1's cluster becomes an **externally visible string
contract**: the constant is `pub`, re-exported from `lib.rs`, and its doc
promises a consumer may match it with `starts_with`. That promise makes
"retryable" a glossary-grade word - it currently means exactly "the venue was
full, not that it said no" and nothing in `reference/` says so. Also note
`synthesize_transport_reject` and `emit_cancel_rejected` are the crate's own
words for the difference between a refusal that ends an order and one that does
not, which the Divergence and OMS vocabulary do not cover.

### The instrument-cache cluster

"seed", "reseed", "the post-bind reseed", `seed_instruments`,
`ensure_instrument`, `cache_instruments`, `emit_seeded_instruments`,
"the seeded instrument set is not an admission list", "orphan quote cache",
`MAX_ORPHAN_QUOTE_SYMBOLS`, `MAX_WARNED_SYMBOLS`, "the nautilus cache" vs "the
adapter's local `InstrumentDef` map".

Two distinct caches, both called "the cache" in adjacent sentences. The
Served-symbol entry covers the venue's side of this ("memoized per run") and
nothing covers the adapter's.

### Miscellaneous undefined nouns

`HttpQuota` / "metered" / "exempt"; `join_url`; `SubKind` / `SubState` /
`BarSubState` / `refs`; "the egress receiver"; "the executor's
instrument-presence guard"; "the shadow" (in `emit_seeded_instruments`'s doc:
"advancing the shadow with no real order" - a broadarrow concept, cited from
this crate's durable doc with no definition and no reference); "the fault
channel"; "the accept gate".

---

# Inherited - quarantine, do not rename

Recorded so the merge does not re-derive them.

- The whole nautilus name list at the top of this document.
- `client_order_id` on `mogwai_protocol::SubmitOrder` and every
  `ClientMessage`, as consumed here - already ruled inherited.
- `MogwaiDataClientConfig` / `MogwaiExecClientConfig` / the two factories /
  `MogwaiClock` - ours, named for the inherited object each serves, and correct
  for that reason.
- `client/shared.rs` `warn_missing_instrument_once`'s "one data client" - the
  inherited object sense, correct.
- `mogwai_clock_factory`'s "a live node", "the runner", "the kernel", "a
  `LocalSet`", `TimeEventSender`, `RustLocal` - nautilus internals, correctly
  named.
- "OMS type", "netting", "hedging", "mass status", "lookback" - nautilus and
  FIX.

---

# Lateral findings

Not naming defects. Flagged because the scope surfaced them.

1. **A whole family of durable prose describes a retired HTTP order carrier.**
   `AGENTS.md` states order entry is WebSocket-only and that `POST /orders`
   went with the HTTP transport profiles. The exec client still says, in
   durable doc comments a maintainer will believe: `client/exec.rs` module doc
   "the **HTTP-or-WS** order dispatch"; `task_handles` doc "every spawned
   **HTTP order dispatch**"; `dispatch_order` "unlike the **HTTP path**, which
   already synthesizes the matching reject"; `reject_for` doc "the **HTTP POST**
   for the command never got a venue reply"; `synthesize_transport_reject` doc
   "shared by the **HTTP POST error path** and the WS send-failure path";
   `emit_cancel_rejected` doc "a `Cancel` that failed at TRANSPORT (the **HTTP
   POST** never reached the venue)"; the `OrderRejected` arm's "the one
   reachable overwrite is the **HTTP carrier** synthesizing a reject";
   `generate_position_status_reports` doc "this works unchanged under the
   **HTTP order profiles that never open a `/ws` socket**"; and
   `lifecycle.rs` `HttpQuota`'s "an **HTTP order dispatch**" and `shared.rs`
   `track_task`'s same phrase. Ten sites. Two of them (`reject_for` and
   `synthesize_transport_reject`) describe the *only* caller as a path that no
   longer exists, which makes the surviving WS caller read as the exception.

2. **`PollCursor` does not exist and is cited twice as if it does.**
   `convert.rs` `trade_id`'s doc: "the adapter's own `PollCursor` explicitly
   tolerates multiple trades sharing one `ts_event` (**see its doc comment in
   client.rs**)" - and `client.rs` is now a twenty-line module declaration.
   `client/data.rs`'s `FeedLagged` arm: "a `PollCursor` resumes past the
   missing span". The `trade_id` derivation's whole justification rests on a
   type that was deleted.

3. **`lifecycle.rs`'s `HttpQuota` doc points at a call-site file that no longer
   holds those call sites**: "is a `client.rs` call-site change, not something
   this type can enforce". The sites are now in `client/{data,exec,shared}.rs`.

4. **`client/data.rs`'s AD10 comment describes a wire frame the client never
   sends.** `unsubscribe_bars`: a stolen decrement "would ... **fire a wire
   Unsubscribe** that darkens the surviving feed". Forty lines earlier the same
   file states three times that subscriptions are satisfied entirely locally and
   nothing is sent to the venue. The hazard is real (the local table stops
   forwarding), the mechanism named is not. A comment describing a gate wider
   than the gate.

5. **`client/data.rs`'s connect teardown comment names a field the type does not
   have**: "Abort the task and clear the stale handle **and ws_cmd**". `ws_cmd`
   is the execution client's field; the data client has none. Copied prose.

6. **`MOGWAI_VENUE_STR`'s single-sourcing claim is wider than the gate.**
   `lib.rs`: "single-sourced so a future rename propagates to the `Venue`, the
   factory `name()` impls, and any test that names the venue." `convert.rs`
   writes the venue name as a bare literal, `Some(ustr::Ustr::from("MOGWAI"))`,
   as the exchange on every `FuturesContract` - in a file that already imports
   `MOGWAI_VENUE`. A rename does not propagate there, and nothing detects it.

7. **`MogwaiExecClientConfig::default()` hardcodes `TraderId::from("MOGWAI-001")`
   rather than reading `DEFAULT_ACCOUNT_ID`.** One string, two constants, two
   jobs (trader id and account id), able to drift silently. If they are meant to
   coincide, assert it; if not, give the trader default its own named constant.

8. **A hardcoded five-second connect deadline against a documented
   pay-warmup-on-first-request contract.** `shared.rs` `wait_connected` waits
   5 s, not sim-scaled and not configurable, and `client/exec.rs`'s
   `ACCOUNT_REGISTRATION_TIMEOUT` is another 5 s. The Tape entry says the first
   requester of a non-boot symbol "pays that river's warmup latency inside its
   own request". A consumer boarding a fresh symbol with a large warmup can
   therefore fail `connect` with `"connect websocket ... timed out"` for a venue
   that is working correctly - and the adapter then aborts the reader and
   returns, so the retry pays the cost again.

9. **The venue's seat refusal has no adapter-side handling.** `ws.rs` refuses a
   second speed on a river an account already rides with
   `"account {account} is already seated on {symbol} at speed {speed}; a ledger
   carries one cadence"`. That reaches this crate as a failed upgrade inside the
   reconnect loop, where `connect_async` errors are logged as
   `"venue websocket dial failed"` and retried forever with backoff. A
   configuration error that can never succeed is treated as a transient outage.
   Compare the identity-mismatch path, which correctly refuses terminally.

10. **`convert.rs`'s MNQ fixture contradicts the glossary's MNQ facts.**
    `a_future_def_builds_a_futures_contract` builds symbol `"MNQ"` with
    `multiplier: Decimal::new(25, 1)` (2.5) and `price_increment` 0.25, giving a
    tick value of 0.625. The Multiplier and Tick value entries state MNQ is
    multiplier 2 and tick value 0.50. It is only a fixture and nothing depends
    on it, but a test that labels its fixture with a real preset's symbol and
    then gives it another instrument's numbers is a trap for the next reader.

11. **A code comment cites transient work-item numbering.** `client/data.rs`
    `subscribe_symbol`: "refusing on the seed refused exactly the sessions
    **piece 13** exists to support". `AGENTS.md`'s document rule is that nothing
    durable cites `notes/`, and a code comment must carry its own context.
    (The `AD*` / `AE*` / `D.*` / `F*` / `A.11` bug-hunt tags are everywhere in
    this crate and are the same shape - roughly sixty citations of documents
    `AGENTS.md` records as retired to git history. Probably not worth a sweep,
    but `A.11` in particular is cited in six operator-facing WARN strings, where
    a host operator has no possible way to resolve it.)

12. **`fetch_quotes_windowed` silently drops its stop-condition asymmetry into a
    comment.** The comment is excellent and explicit about why the quote path
    returns `false` where the trade path calls `stop(&out)`. Recorded only
    because it is the exact "two near-duplicate loops, one line that must differ"
    shape, and nothing but the comment holds it. A shared helper parameterised on
    the stop closure would let the compiler hold it instead.

---

# Open, for the owner, not to be defaulted

- **The `client = label` sense (the socket leg) is a fourth `client` job** the
  ruling's five-way classification does not cover. It resolves trivially to
  `socket` because `lifecycle.rs` already spells the same value that way, but it
  should be recorded in the classification rather than swept.
- **`leg`** - one of the two connections a nautilus consumer necessarily holds
  under one account - is a real concept with no entry, used in six places, and I
  do not think it should be renamed onto Connection. It probably wants an entry.
- **[second pass resolved] Session versus Consumer.** This is adjudicated in the
  added Session/Consumer/Eviction row as a glossary contradiction. The code's
  configurable escape hatch does not make the three entries mutually true, and
  its rustdoc currently conceals the sharing use.
- **[second pass resolved] Control-plane scope.** The venue request is capable
  of account and symbol scoping, but the adapter serializes only the flattened
  divergence. The revised Divergence/Boarding roadmap row records the concrete
  adapter gap; this is no longer open.
