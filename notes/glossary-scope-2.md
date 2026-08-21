# Glossary scope pass 2: the venue's external surface

Inventory of what `mogwai-venue` presents to the outside world - HTTP routes,
query structs, JSON request and response bodies, status codes, refusal and error
bodies, close reasons, operator-facing config keys, and the log lines a human
reads - measured against `reference/glossary.md`. Nothing was edited but this
file.

**Independent reconciliation (2026-08-21).** The second inventory read the
glossary and the scoped server code before consulting this report. Its changes
are marked **[P2 ADDED]**, **[P2 CHANGED]**, and **[P2 REFUTED]**. Refuted rows
remain in place so the owner can see the disagreement rather than receiving a
silently cleaned consensus.

Scope note: the crate's internal domain model (`Run`, `Passenger`, `Boat`,
`Boatyard`, the lane and seat apparatus as such) belongs to a separate pass.
Where one of those words CROSSES onto the external surface - in a refusal body,
a JSON field, a config key or a log line a client or operator reads - it is a
row here, because at that point it has stopped being internal.

Direction key: **1** a glossary term doing a job that is not that term's;
**2** a job the glossary already names, under a different word; **3**
load-bearing and undefined; **inherited** vocabulary this project does not own.

Reach key: `wire` externally visible (route, query key, JSON field, status,
refusal/close text, config key, stdout/stderr line a consumer or operator
reads); `cross` across a crate boundary; `local` crate-local.

---

## Relationship to pass 1

Pass 1 catalogued `mogwai-protocol`. This pass does not re-derive its rows. It
CONTRADICTS pass 1 in one place and EXTENDS it in eleven:

**Contradiction.** Pass 1 (row on `risk.rs` `MaxPosition`, lateral finding 2)
recorded a standoff between `MaxPosition`'s "an account is on at most one river"
premise and the glossary's Seat entry. The server settles it: the glossary is
right and the doc is wrong. `ws_upgrade` seats an account per `(river, speed)`
and refuses only a SECOND SPEED OF THE SAME RIVER, so a multi-river account is
the supported shape. But it also shows the cap is not the venue-wide scalar the
`risk.rs` doc describes: `process_order_cmd` compares it against
`engine.projected_qty(&order.symbol, additional)` - a PER-SYMBOL projection - so
today the one number is applied independently to each symbol the account trades.
That is neither what the type's doc says nor what a reader of `MaxPosition
{ quantity }` would guess. See lateral finding 2.

**Extensions.** Pass 1 rows whose reach this pass upgrades from crate-local or
doc-prose to externally visible:

| pass 1 row | how this surface extends it |
|---|---|
| Lane (undefined, doc prose) | `exec_held_budget_bytes` and `admission_lane_frames` are OPERATOR CONFIG KEYS. "Lane" is now a thing an operator tunes and still cannot look up. |
| byte budget (undefined) | same two keys, plus the refusal "execution output admission budget exhausted" on the wire. |
| act / ack (undefined) | `pending_command_acts` and `global_pending_command_acts` are config keys spelled in the act half of a distinction nothing defines. |
| band / sweep (tape-walk cluster) | `fill_band_vol_mult`, `fill_band_max_ticks`, `fill_sweep_interval_ms` are config keys. |
| `Heartbeat` collision | `server_heartbeat_ms` is a config key; it names the TEXT-FRAME heartbeat, not the WS ping the adapter's `heartbeat_interval_ms` names. Two operator-visible knobs, one word, different mechanisms. |
| Freeze vs "unattended" | `Config::account_ttl_ms`'s doc says "UNATTENDED account" - the same fourth word as `ReadyRecord`'s, in the file an operator actually reads. |
| `reset_account_on_reconnect` | it is a CONFIG KEY, not only a `ReadyRecord` field, so the rename cost is higher and the reasoning identical. |
| Account policy = "balance plus rules" | `OpenAccountRequest` proves the glossary wrong on the wire: `balances` and `policy` are siblings, and `policy_preset` is a third. |
| `ClearDivergences` scope | confirmed at the routing site: it clears venue arms and flow surges only; engine one-shots stay armed. |
| `RunComplete` / "passenger duration complete" | confirmed: `handle_socket` emits the same `RunComplete` variant for the venue deadline and for `SocketQuery::duration_ms`, and closes 1000 with two different reasons. The overload is on the primary text frame, as pass 1 said. |
| `MAX_CLIENT_ID_LEN` naming | the server adds a SECOND echo cap, `MAX_ECHOED_SYMBOL = 64`, under its own name for the same rule. Pass 1's `MAX_ECHOED_ID_LEN` proposal should absorb both. |
| Fault undefined | this crate spends "fault" five ways; see the fault cluster below. |

---

## Direction 1 - a glossary term doing a job that is not its own

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boat | `http.rs` `arm_divergence`: `"river {symbol} has a seated boat; place a boat whose sharing key carries generator havoc"` | refusal text (400) | tells a CLIENT that boats exist, that they are seated, and that they have a sharing key | wire | 1 | The glossary says a boat "is an implementation cache with no semantics of its own... nothing a client can measure reveals whether it shares a hull". This body measures it directly, and worse, instructs the operator to perform an action ("place a boat whose sharing key carries generator havoc") that no route exposes. Either the glossary's unobservability claim is false, or this refusal must be rewritten in client vocabulary: "generator havoc cannot be armed on a symbol that already has a live subscriber". I think the glossary is describing an intent the code no longer honours. |
| Boat | `ServerClock::boat_clock` as populated by `/clock`; `RiverNow::from_boat` | JSON field | publishes whether the answer uses a paced tape's clock rather than the venue clock | wire | - | **[P2 REFUTED]** This does not contradict the glossary's claim that sharing a hull is unobservable. The bit exposes the clock axis, which is semantically observable whether the paced reader is shared or private; it reveals neither another connection nor cache sharing. The incompatible encoding beside `AccountSnapshot.clock` remains a direction-3 clock-axis design defect, but using `boat` here is not evidence that Boat is doing the wrong job. |
| Boat | `http.rs` `arm_divergence`: `"generator divergence requires symbol; seated boats: {joined}"` | refusal text (400) | enumerates internal objects to a client as if they were the served set | wire | 1 | Same. What the client can act on is the set of symbols with live subscribers; say that. |
| Boat | `ws.rs` `ws_upgrade`: `"could not place boat for {symbol}: {err}"` | refusal text (400) | a river could not be materialized or paced | wire | 1 | Same. The client asked for a symbol and gets told about hull placement. |
| River | `http.rs` `history_start_refusal`: `"requested start {start} exceeds this river's now {river_now} - what its boat has published, or venue sim-now if it carries none"` | refusal text (400) | the per-symbol ceiling | wire | 1 | A refusal body that has to teach the reader two internal nouns to be intelligible is not a refusal body. The client knows `symbol` and `/clock?symbol=`; say "the tape for {symbol} has only been produced through {ns}; see /clock?symbol=". |
| River | `ws.rs` `handle_socket`: `tracing::info!("socket bound to river")` | log text | a socket bound a symbol | wire (operator) | 1 | Low cost, but it is the first line an operator reads per connection and it names neither the account nor the speed, which are the two things that decide what happens next. |
| Run / Ledger | `config.rs` `Config::account_id` doc: "One venue is one run is one ledger, so this NAMES the account rather than selecting one - there is nothing to look up and nothing to refuse." | doc prose (operator-facing config reference) | claims the venue holds exactly one ledger | wire | 1 | **Flatly false today and the falsehood is load-bearing.** The glossary's Run entry says "many ledgers"; `POST /accounts`, `/ws?account=`, `/account?account=`, `account_ttl_ms` and `AccountRefusal::AlreadyOpen` all exist precisely because there are many. Rewrite: this names the DEFAULT account, the one a connection that names none is served under. |
| Run | `serve.rs`: `tracing::info!(account_id, "run ledger account fixed")` | log text | the default account id was resolved | wire (operator) | 1 | Same stale premise, in the boot log. "default account id fixed". |
| Freeze | `config.rs` `account_ttl_ms` doc: "How long an UNATTENDED account survives" | doc prose | the freeze TTL | wire | 1 | Fourth word for Freeze, same as pass 1's `ReadyRecord` row, now in the operator's config reference. Use Freeze once, in both. |
| Divergence / StallData | `config.rs` `zero_speed_stall_ms` | config key | how long a `speed = 0` tape parks waiting for ring headroom | wire | 1 | `StallData` is an armed transport divergence; this stall is an unarmed backpressure park inside the tape. Two mechanisms, one word, and the config key is the one an operator reads first. Rename the knob (`zero_speed_ring_wait_ms`) - it is the newer and cheaper of the two. |
| Session | `serve.rs` `serve_until_drained` / `Run::sessions_drained`, and `SocketSession` | type, fn, doc prose | one live WebSocket connection | cross, wire (indirect) | 1 | The glossary's Session is the CLIENT IDENTITY on `?session=`, and this crate uses the same word for the connection - the glossary's Connection. Both appear in one struct: `SocketSession.client_session` is the glossary's Session, and the struct it sits in is not. Rename the connection sense (`SocketConn` / `conns_drained`) and keep Session for the identity. Mostly internal, but `SocketQuery::session` is on the wire and the collision is one field away from a reader. |
| Account policy | `http.rs` `OpenAccountRequest` (`balances` + `policy` + `policy_preset`) | JSON fields | balances and rules as SEPARATE fields | wire | 1 | Confirms pass 1: the glossary's "opening balance plus risk rules" describes no type on either side. Correct the entry to say the policy is the RULES, and that opening balances are stated beside it. |
| Session calendar | `http.rs` `refuse_all(..., "market closed", ts)` | refusal text (`OrderRejected.reason`) | the resolved shape's session calendar says the market is shut | wire | - | **[P2 REFUTED]** The code emits this reason only from `calendar.is_open(ts) == false`; `ReopenGap` is not an alternate branch producing the same refusal. "Market closed" is also quarantined below as standard trading vocabulary. The message could be more diagnostic, but it neither misuses Session calendar nor collapses two code paths. |

---

## Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boot symbol | `config.rs` `Config::symbol` (top-level TOML key `symbol`) | config key | the boot river, AND the default any request naming no symbol resolves to | wire | 2 | The glossary names this exactly - Boot symbol / boot river - and the key does not. A bare `symbol` in a config that also has `[symbols.*]` and an `[instrument] preset` reads as "the instrument this venue serves", which is precisely the closed-set misreading `AGENTS.md` spends three paragraphs killing. Rename to `boot_symbol`; the accessor `cfg.boot_symbol()` already exists, so only the serde name moves. |
| Tape / Boat's ring | `config.rs` `fanout_depth` | config key | depth of each tape's bounded broadcast ring | wire | 2 | "Fanout" appears nowhere in the glossary; the thing is the Boat's ring publishing the Tape. Pair this rename with pass 1's `FeedLagged` -> `TapeLagged`: one word for the paced stream, one for its ring. `tape_ring_frames` says what it bounds and in what units. |
| Warmup / data origin | `source::TAPE_ORIGIN_NS` vs the wire field `data_origin_ns` and the refusal `"precedes data_origin_ns {n}"` | constant vs JSON field | one instant, two names | wire, cross | 2 | The glossary's Warmup entry spells it `data_origin_ns`. The constant that IS that value is `TAPE_ORIGIN_NS`. Two names for one number, and the refusal text quotes the wire spelling while the code reads the other. Rename the constant. |
| Venue / Server | `Health.status: "ok"`; the `mogwai-venue` crate; `ServerClock.server_now_ns` on `/clock` | field, crate, JSON field | the server side of the external protocol | wire | - | **[P2 REFUTED]** `server_now_ns` is conventional endpoint-polarity vocabulary just like `ServerMessage`; the absence of a neighbouring `client_now_ns` field does not turn it into a competing name for Venue. `Health.status` does not use the word at all. Keep Server for protocol direction and Venue for the modeled exchange/process. |
| Eviction | `admission.rs` `CLOSE_EVICTED` / `CloseSpec::evicted` and the reason `"another connection claimed account {id}; a ledger is never read from two clients at once"` | constant, close text | the glossary's Eviction, exactly | wire | - | **No defect. Recorded as the model case.** Constructor owns the prefix, prefix is the contract, reason names the rule, code is 1000. This is what every other refusal on this surface should look like. |

---

## Direction 3 - load-bearing and undefined

The clusters matter more than the rows. Five of them, and one of the five - the
admission cluster - is one word spent on three unrelated mechanisms, all
reachable by a client in one run.

### The admission cluster - ONE WORD, THREE MECHANISMS

This is the worst finding in the pass. "Admission" names three unrelated
capacity systems inside one crate, two of them observable by the same client on
the same run, and the glossary defines none of them.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `admission.rs` `ExecLanes`, `AdmissionLimits`, `ADMISSION_LANE_FRAMES`, `reserve_admission`, `emit_admission`; `admission_lane_frames` config key | module, types, constants, config | the per-connection OUTBOUND byte and frame budget, and the priority lane refusals ride | wire, cross | 3 | Sense one. This is the sense `mogwai_protocol::AdmissionRejected` and pass 1's admission cluster are about. |
| - | `http.rs` `admit_history`, `HISTORY_ADMISSION_WAIT`, `MAX_CONCURRENT_HISTORY_REQUESTS`, `MAX_QUEUED_HISTORY_REQUESTS`, "the endpoints' own admission decision" | fn, constants, doc prose | an INBOUND concurrency gate on `/trades` and `/quotes`, bounding resident page memory | wire | 3 | Sense two. Unrelated to sense one: different direction, different resource, different refusal (503 with a plain body, no `retryable`, no `AdmissionSubject`). |
| - | `run.rs` `Run::admit`, `Admission`, `SocketSession::admission` | fn, type, field | a COUNT of live readers on an account, whose absence freezes it | cross | 3 | Sense three. Nothing to do with capacity at all - it is an attach refcount. This one is internal, but it shares `SocketSession` with sense one's `ExecLanes`. |
| - | `"execution output admission budget exhausted"` (AdmissionRejected), `"venue command capacity exhausted"` (AdmissionRejected), `"history request capacity exhausted"` (503), `"priority admission lane saturated: the peer is not reading"` (WS 1013), `"outbound writer is gone"` (WS 1013), `"execution admission lane saturated"` (WS 1013) | refusal and close texts | six spellings of "the venue is full" across three carriers | wire | 3 | The client-facing vocabulary of backpressure is six phrases, three transports (text frame, HTTP status, close code) and two machine-readable hints (`retryable`, nothing). A consumer writing one backoff policy has to pattern-match prose. Owed: one taxonomy in `reference/`, and the `retryable` idea extended to the HTTP refusals (a `Retry-After` header costs nothing and is the HTTP spelling of the same claim). |
| - | 503 vs `AdmissionRejected` vs WS 1013 | status codes | which carrier a capacity refusal takes | wire | 3 | The choice is principled (pre-upgrade HTTP, in-band frame, connection-terminal) and written down nowhere. One paragraph. |

**Verdict for the cluster: rename two of the three.** Keep "admission" for the
outbound lane contract the protocol crate already publishes as
`AdmissionRejected`. The history gate is a CONCURRENCY GATE (`history_slots`,
`HISTORY_SLOT_WAIT`). The account counter is an ATTACHMENT
(`Run::attach` / `Attachment`), which also fixes its own readability - "admit an
account" reads as capacity and means the opposite.

### The clock-axis cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `ClockAxis` (`Venue`) on `AccountSnapshot.clock`, one variant, always `"venue"` | JSON field, enum | which time axis this stamp lives on | wire | 3 | The concept - venue axis versus paced-tape axis - is real, load-bearing, and stated twice in two incompatible encodings: a single-variant string enum here, a bool (`boat_clock`) on `/clock`. Define the axis once in the glossary and encode it once on the wire. |
| - | `RiverNow` (`ns`, `sim`, `from_boat`), `AppState::river_now`, "this river's now" | type, fn, refusal text | the per-symbol ceiling a request may be answered as of, plus which clock it is on | wire, cross | 3 | This is the single most important quantity on the history surface - it is why an `end` is clamped and a `start` refused - and the glossary has no word for it. "River now" is also the one place a client is told the answer depends on whether anyone is subscribed. Owed: an entry, and probably a wire name (`served_through_ns`). |
| - | the `start` refusal / `end` clamp asymmetry (`history_start_refusal` vs the inline clamp) | status/behaviour | a future start is a caller error, a future end is "everything through now" | wire | 3 | Excellent reasoning, living in two comments at two call sites. It is a documented consumer contract with no durable home. |
| - | `server_now_ns`, `data_origin_ns`, `warmup_ns`, `sim` on `/clock` | JSON fields | the affine map plus the tape's floor and ceiling | wire | 3 | Warmup and data origin ARE glossed. `sim` (a `SimClock`) is not; pass 1 deferred it as out of scope. It is on the wire here, so somebody owes it an entry. |
| - | `/clock?speed=` without `symbol`; `ClockQuery.speed` | query key / behaviour | appears to select a cadence but is ignored unless `symbol` is also present | wire | 3 | **[P2 ADDED]** This is not merely undefined clock vocabulary. It is an accepted-and-ignored request on the same surface whose `/ws` query explicitly calls that failure mode unacceptable. Either refuse `speed` without `symbol` as a 400, or define and implement what cadence of the unnamed venue clock it selects. Today `/clock?speed=17` and `/clock` are indistinguishable successes. |

### The fault cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `Health.fault` / `HealthFault { symbol, kind, clock_ns }` | JSON field, object | a seated river whose tape gave out, smallest symbol wins | wire | 3 | The only pre-mortem fault signal a fleet poller has, and "fault" is undefined. The selection rule (smallest symbol, one object, N boats) is a real contract stated only in a doc comment. |
| - | `kind` values `"arrival.no_open_exposure"`, `"arrival.intensity_ceiling"`, `"arrival.non_finite_state"`, `"injected"` | JSON string values | a machine-readable fault taxonomy | wire | 3 | A dotted namespace with one member that is not dotted. `"injected"` should be `"injected.fault_tape"` or the namespace should be dropped; as it stands a consumer cannot split on `.` safely. And the vocabulary is nowhere durable - a consumer matching these strings is matching undocumented literals. |
| - | `CLOSE_VENUE_FAULT` (1011) and its two reasons: `"venue fault: lost {n} ticks; the tape ring turned over and this feed has an unarmed gap"`, `"venue fault: tape event time moved backward from {a} to {b}"` | close texts | the venue lost data it promised | wire | 3 | Good texts. Note they carry an ad-hoc `"venue fault: "` prefix that, unlike `EVICTED_PREFIX`, is a call-site literal rather than a constructor's contract - and `close::classify` returns `None` for 1011 anyway, so nothing reads it. If the prefix means anything it should be in `mogwai_protocol::close`; if it means nothing it should go. |
| - | `serve.rs` exit `anyhow::bail!("tape source fault: {fault:?}")` | error text (process exit, nonzero) | the terminal fault path | wire (operator) | 3 | Fourth "fault". Also renders a `TickFault` through `Debug` into the last thing an operator ever sees from the process; see lateral 5. |
| - | `Divergence::FaultTape` acks: `"the venue is faulting and will exit nonzero"`, `"the venue was already tearing down when this arrived"` (both 202) | refusal/ack text | fifth sense, the armed one | wire | 3 | Pass 1 already owes Fault an entry. This crate is the evidence that it needs to distinguish ARMED fault (FaultTape), ORGANIC fault (ring turnover, arrival refusal) and the health-poll REPORT of one. |
| - | `Health.status: "ok"` | JSON field | a constant | wire | 3 | Never anything but `"ok"`; `fault` carries the whole signal. A field that cannot vary is a field a consumer will eventually gate on. Either make it vary (`"ok"` / `"faulted"`) or drop it. |

### The resolution cluster - shape, profile, bundle, def, preset, overlay

The words for "what a symbol resolves to" are the most numerous undefined set on
this surface, and several reach the client in refusal bodies.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `ResolveRefusal::Invalid` -> `"invalid resolved shape: {e}"`; `"symbol {s} has no configured shape"`; `InstrumentProfile`, `InstrumentDef`, `profile_for_symbol`, `configured_symbols`, `materialized_symbols`, `resolve_profile`, `resolve_key` | refusal texts, types, fns | one concept - the instrument the venue serves under a label - under five names | wire, cross | 3 | Extends pass 1's `InstrumentDef` row onto the wire. A client reading "invalid resolved shape" has never been told what a shape is; `/instruments` returns `InstrumentDef`. Pick one noun (I would take *shape*, since the glossary's Served symbol entry already uses it) and use it in the refusal, the type and the entry. |
| - | `ResolveRefusal::FundingBarred` -> `"symbol {s} resolves to a shape settling in {c}, which this run does not fund; add {c} to [balances]"`; `funding_barred`; `Run::funded_in`; `"account {a} is not funded in {c}, which is what {s} settles in"` | refusal texts | a shape whose settlement currency the account or run holds no balance in | wire | 3 | "Funding-barred" is a coined term appearing in three refusals and no durable document, and it COLLIDES with the perpetual's funding rate (glossary Instrument class), which is a completely different thing. Rename: *unfunded settlement currency*. The refusal texts themselves are excellent - remedy-shaped, naming the config key. |
| - | `MaterializeRefusal::CapacityExhausted` -> `"this run has materialized {n} rivers and its cap is {cap}; no further symbol can be served"`; `MaterializeRefusal::KeyMismatch` -> `"the river key passed for symbol {s} is not the key this run resolved for it"` | refusal texts (400) | the river cap; an internal invariant violation | wire | 3 | The first is fine and the cap deserves a glossary line, since it is the one thing that makes the open instrument set finite. The second is an INTERNAL INVARIANT rendered to a client as a 400 - a client cannot pass a river key, has never heard of one, and cannot act on it. That is a 500, and it should say so. |
| - | `"illegal symbol: {reason}"` vs `resolve_socket_symbol`'s `"requested symbol {s} is not a legal symbol; symbols are 1 to 32 characters of ASCII letters, digits, dot, dash or underscore"` | refusal texts | the same rule, two messages, one of which spells out the alphabet and one of which does not | wire | 3 | Extends pass 1's alphabet finding: there are now FOUR statements of the URL-safe alphabet (`validate_wire_symbol`, `validate_session_id`, `AccountId::parse`, and this hand-written sentence). The sentence is the only one a client ever sees and it is the one not checked against the validator. |
| - | `MaterializeRefusal::Reach` -> `"{e:#}"` | refusal text (400) | a synthesis failure | wire | 3 | An anyhow chain rendered raw into a 400 body. Unlike its siblings it names no rule and no remedy, and a synthesis failure is not a client error at all. |
| - | "materialize", "reach", "spend the river", "advertise through /instruments" | doc prose, fn names | first synthesis of a river; walking it to an instant; consuming cap | cross | 3 | Four verbs for the lifecycle the glossary's Tape entry describes as a two-moment split. The entry is right; the code's verbs are not in it. |
| - | `GET /instruments`; `Rivers::instrument_defs` | route / JSON body | configured shapes UNION symbols materialized so far, not the open set of instruments the venue will serve | wire | 3 | **[P2 ADDED]** The route name reads as the venue's instrument set, while the glossary says the set is open and every legal, fundable label is a Served symbol. Its response is instead a growing observation set: requesting history changes the next answer. Name and define that concept (`known_instruments`, `resolved_instruments`, or equivalent), or make `/instruments` report the total resolution rule rather than pretending a finite list is exhaustive. This is the route-level version of the configured/materialized/served vocabulary split. |

### The account-lifecycle cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `POST /accounts` "open"; `AccountRefusal::AlreadyOpen` -> `"this account is already open; an account outlives its connections, so it is never re-opened with new terms - name a different account id for a fresh ledger"` (409) | route, refusal | stating an account's terms before it trades | wire | 3 | Glossary Account says "created on first sight of the id", which is the `/ws` path. OPEN is a second, earlier creation act with its own refusal and its own status code, and the glossary does not name it. The refusal text is a model one. |
| - | `seat` / `claim` / `admit` / `attach` / `resume` / `freeze` / `collect` / `reset` / `evict` / `unsit` / `discard` (`Run`, `ws_upgrade`) | fns, doc prose | the states an account passes through | cross, wire (partly) | 3 | Eleven verbs. The glossary names three (Seat, Freeze, Eviction). `resume`, `claim`, `open`, `collect` and `reset` are load-bearing and undefined, and `reset` vs `discard` vs `collect` are three different ways to lose a ledger with three different triggers. This is the vocabulary a consumer reasoning about restart windows needs, and it does not exist. |
| - | `"account {a} is already seated on {s} at speed {sp}; a ledger carries one cadence"` | refusal text (400) | the one-cadence-per-ledger rule | wire | 3 | Matches the glossary's Seat entry exactly, and is the only externally visible use of "seated" that reads correctly, because the client actually chose the speed. Keep. But note it is a 400 for a CONFLICT with existing state - 409 is the honest status. |
| - | `"account {a} is policed in {c} and {s} would make it hold another currency; the venue has no rate to state its equity with..."`; `tracing::info!(policed, "opened an account")` | refusal text, log field | an account with a policy currency | wire | 3 | Extends pass 1's `is_unpoliced` row from crate-local to WIRE: "policed" is now a word a client reads in a rejection reason. It is a fifth verb in the enforcement vocabulary (breach, lock, terminate, reset, police) and the only one on the wire. |
| - | `AccountRefusal::UnknownPolicy` -> `"no account policy is registered or shipped under {n}; shipped names are {list}, and an operator registers more under [account_policies]"` | refusal text (400) | resolution failure on `policy_preset` | wire | 3 | Registered vs shipped is a real two-tier distinction (config shadows build) that the glossary's Account policy entry does not mention. Model refusal otherwise - it enumerates the legal set and names the config table. |
| - | `"an account must open with at least one funded currency"` | refusal text (400) | empty `balances` | wire | 3 | Good. The reasoning behind it (a configuration mistake must stay distinguishable from depletion) is the same principle as the funding-barred refusal and belongs with it in one place. |

### The control-plane cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `DivergenceRequest.account` | JSON field | which passengers a TRANSPORT arm corrupts; `None` means THE VENUE (recorded on the run, so late-connecting accounts inherit it) - and it is ignored outright by `FeeSurcharge` and the five engine arms, and REFUSED by `FaultTape` | wire | 3 | One field, four behaviours, decided per variant with no schema expressing it. `None` meaning "the venue, including accounts that do not exist yet" rather than "everyone currently here" is a genuinely subtle and correct design that a client cannot discover from the field name or from any document. |
| - | `DivergenceRequest.symbol` | JSON field | the river for `FlowSurge` and `ClearDivergences`; a cross-CHECK against the resting order for `CancelOpenOrderSilently`; SILENTLY IGNORED for the other nine variants | wire | 3 | Same shape, worse: nine variants accept and discard it. See lateral 1. |
| - | `Run::arm` / `VenueArm` / "the venue record" / "arming boundary" / "server-owned arm" | fns, types, doc prose | where a divergence lives before it fires, and who owns each variant | cross | 3 | The eight-server-owned / five-engine-armed split is a real contract, enumerated exhaustively at the routing site with an excellent comment explaining why a catch-all would be a silent control loss. It exists nowhere durable, and the glossary's Divergence entry - already wrong per pass 1 - offers a THIRD, incompatible classification (generator / transport / engine / windowed / terminal). Two taxonomies of one set. Reconcile them. |
| - | 202 with a prose body (`"armed a {n} ms simulated surge on {s}, opening at the river origin {o}"`, `"armed; the engine queue was at its {n}-entry cap, so the oldest armed divergence was discarded to make room: {shed:?}"`) | status + body | accepted, with collateral damage named | wire | 3 | The eviction report is a genuinely good design (a bare 202 cost a QA run a misdiagnosis, per the comment) rendered as unparseable prose containing a Rust `{:?}`. If the collateral matters enough to report, it matters enough to be JSON. |

---

## Inherited - quarantine, do not rename

| term | site | source |
|---|---|---|
| HTTP status codes and their meanings (400/404/409/201/202/500/503) | throughout | HTTP |
| `Content-Type`, `Content-Length`, chunked transfer encoding | `history_page` | HTTP/1.1 |
| WS 1000 / 1011 / 1013, RFC 6455 control-frame 125-byte cap | `admission.rs` | RFC 6455 |
| `oms_type` and `OmsType::Netting` on `/health` | `http.rs` | nautilus |
| `AccountState`, `InstrumentDef` field spellings, `TradeTick` / `QuoteTick` shapes | `/account`, `/instruments`, `/trades`, `/quotes` | nautilus / pass 1's quarantine |
| `MOGWAI-001` `ISSUER-NUMBER` account id shape | `config.rs` `account_id` | nautilus `AccountId` |
| "settlement currency", "notional", "reduce-only", "market closed" as a concept, "equity", "drawdown" | refusal texts | universal trading vocabulary |
| `PR_SET_PDEATHSIG`, SIGTERM, reparenting, subreaper | `serve.rs` | POSIX / Linux |
| `preset`, `override` as TOML overlay verbs | `config.rs` | configuration-language convention |

Note the route names themselves are NOT inherited and are in scope: `/health`,
`/instruments`, `/trades`, `/quotes`, `/clock`, `/account`, `/accounts`,
`/control/divergence`, `/ws`. Only one of them has a naming defect, below.

---

## Lateral findings

Ordered by how much I think they matter.

**1. `DivergenceRequest` accepts and ignores, which is the exact failure mode
`SocketQuery` was built to prevent.** `SocketQuery` carries
`#[serde(deny_unknown_fields)]` with a long comment arguing that
accepted-and-ignored is the failure mode the carrier exists to prevent, and
that a `400` on an unknown key is worth the cost. `DivergenceRequest` carries no
such attribute and uses `#[serde(flatten)]` for the divergence, which makes
`deny_unknown_fields` unavailable anyway (serde cannot deny unknown fields
alongside a flattened map). So on the control plane: a typo'd knob
(`"fraction_"` instead of `"fraction"`) deserializes into a `PartialFillNext`
with the DEFAULT fraction and answers `202`, and `symbol` is silently discarded
by nine of the thirteen variants. An operator arming a scenario gets "accepted"
for an arm that is not what they wrote. This is the vacuous-gate family: the
validator (`validate_divergence`) is real and runs, and it cannot see a field
that never arrived. The fix is structural, not an attribute: stop flattening -
`{"kind": "...", "args": {...}}` - and deny unknown fields on both levels.

**2. The position cap is applied per symbol, and nothing says so.**
`process_order_cmd` reads one scalar `max_position()` off the account's risk
ledger and compares it against `engine.projected_qty(&order.symbol, additional)`.
For an account riding two rivers that is two independent caps of the same
number, over two incomparable size units (contracts and base units). The
`risk.rs` doc says the opposite ("an account is on at most one river, so one
number is enough"), the glossary says the opposite of the doc, and the code
does a third thing that neither describes. Whichever way the owner rules, the
refusal text `"account {a} may not carry more than {cap} of {symbol}"` is
currently the ONLY honest statement of the behaviour in the tree - it names the
symbol, so it is already per-symbol-shaped. My read: the code is right, the
doc is wrong, and `MaxPosition` should be documented (and probably typed) as a
per-instrument cap.

**3. `/account` and `/accounts` are one letter apart and unrelated.** `GET
/account?account=` reads a snapshot; `POST /accounts` opens one. Different
methods so nothing collides, but a typo'd path silently changes the operation
being described in every log, script and doc, and neither name says which is
which. Pre-1.0: make them `GET /account` and `POST /account/open`, or make the
pair RESTful (`GET|POST /accounts`, `GET /accounts/{id}`). Also note `GET
/account` takes the account in the QUERY STRING while `POST /accounts` takes it
in the BODY as `account_id` - two spellings (`account`, `account_id`) of one
identifier on two routes of one surface, plus a third on `/ws?account=` and a
fourth on `DivergenceRequest.account` where it means a TARGET rather than an
identity.

**4. `MaterializeRefusal::KeyMismatch` renders an internal invariant as a client
error.** `"the river key passed for symbol {s} is not the key this run resolved
for it"` reaches a client as a `400` from `/trades` and `/quotes`. A client
cannot pass a river key. If this is reachable it is a venue bug and owes a 500;
if it is unreachable from HTTP the variant should not be in the arm that maps to
`BAD_REQUEST`. Same family: `MaterializeRefusal::Reach` (a synthesis failure) is
also mapped to `400` at the `rivers.materialize` call site in both handlers,
while the very same failure raised later in the blocking task correctly becomes a
`500` with a hand-written body. One failure, two status codes, depending on
which line it happens on.

**5. The process's last words are a `Debug` render.** `anyhow::bail!("tape source
fault: {fault:?}")` puts `TickFault`'s derived `Debug` into the venue's exit
error - the string a launcher captures and an operator reads when a run dies.
`/health` already has a hand-written taxonomy for exactly these values
(`health_fault`, producing `"arrival.intensity_ceiling"` and friends). Two
renderings of one enum, and the operator-facing one is the derived one. Reuse
`health_fault`'s mapping, or give `TickFault` a `Display`.

**6. Three "capacity exhausted" refusals, one `retryable` field.**
`AdmissionRejected` carries `retryable: true` and the protocol crate states that
every admission refusal is backpressure. The HTTP twin - `503 "history request
capacity exhausted"` - carries no machine-readable equivalent and no
`Retry-After`, and the WS 1013 close carries only the code. A consumer writing
one backoff policy across the three has to key on prose. Cheap fix:
`Retry-After` on the 503; the code 1013 already means "try again later" by RFC.

**7. `Health.status` cannot vary.** Serialized as `&'static str` and set to
`"ok"` unconditionally, including when `fault` is `Some`. A consumer that
health-checks on `status == "ok"` - which is the obvious reading of a field
named `status` next to a field named `fault` - is reading a constant. The
`fault` field's own doc explains at length why a fleet poller depends on this
endpoint to score a run keepable. That is a vacuous gate with a well-documented
consumer.

**8. Two echo caps, one rule, and only one of them is tested against its
message.** `MAX_ECHOED_SYMBOL = 64` (this crate) and
`MAX_CLIENT_ID_LEN` (protocol) both exist so a caller-supplied string cannot
amplify a refusal body. The symbol refusal hard-codes `1 to 32 characters` in
its message while validating through `validate_wire_symbol`, and truncates at
64 - so the message names 32, the cap is 64, and the validator owns both
numbers. Same shape as pass 1's lateral 7 (four validator branches hard-coding
`3600000`). Format the constants into the message or assert containment.

**9. `arm_divergence` returns `(StatusCode, String)` with an empty string on
success.** Every success path answers `202` with `String::new()` except the two
that return prose. So the control plane's response body is: sometimes empty,
sometimes an English sentence, sometimes an English sentence containing a Rust
`Debug` render, and on failure sometimes a validator message and sometimes an
anyhow chain. Nothing on this route is parseable. It is the one route an
automated scenario driver uses most.

**10. The `/health` fault selection changes what a poller sees when a second
river faults.** Documented and deliberate ("one faulted river already condemns
the run"), and I agree with the ruling. Recording it because the field is
`Option<HealthFault>` and a poller that learns to expect a specific symbol will
see it change if the smallest-symbol river recovers. If the shape is ever
revisited, a count alongside the object would cost one integer.

**11. `resolve_socket_symbol`'s doc argues a case its own tests contradict in
spirit.** The doc says resolution is case-sensitive so a socket cannot bind
under one label while its history fetches name another - correct - and
`a_miscased_socket_symbol_is_a_distinct_resolved_label` pins it. But `[symbols.*]`
overlays match case-insensitively (`Config::symbols`, "Keyed case-insensitively").
So `mnq` gets a DISTINCT river that nevertheless picks up the `[symbols.MNQ]`
operator overlay. That is defensible, and it is two case rules meeting at one
label with nothing stating the interaction. Worth a sentence in `docs/config.md`.

**12. [P2 ADDED] `/clock?speed=` is accepted and ignored without `symbol`.**
`ClockQuery` deserializes both fields, but `clock` consults `query.speed` only
inside `if let Some(symbol)`. Thus `/clock?speed=2` returns 200 with precisely
the venue-clock answer `/clock` returns. This is the accepted-and-ignored defect
the `SocketQuery` comment explicitly forbids. Refuse the incomplete selector or
give it semantics; do not silently discard it.

**13. [P2 ADDED] The strict query contract stops at `/ws`.** `SocketQuery` has
`deny_unknown_fields` and explains why a typo must fail. `AccountQuery`,
`ClockQuery`, and `HistoryQuery` do not, so `/clock?speeed=2`,
`/account?acount=X`, and `/trades?...&limt=1` are successful requests whose
misspelled controls are ignored. The account typo is especially sharp: it
silently reads the default account, and the limit typo silently selects the
default page size. Apply one query-boundary policy across every route and pin
it with real extractor tests; the current split makes typo safety depend on
which endpoint received the same class of input.

**14. [P2 ADDED] Strict instrument config advertises a guard that ends one
level too early.** `ConfiguredInstrument` denies unknown fields, but its own
doc states that `generator` and `session` deserialize into shared types that do
not deny them, so a typo inside either sub-table is accepted and the omitted
knob defaults. This is a direct vacuous-gate instance on the operator surface:
the enclosing table reads as typo-safe and the most dynamics-sensitive nested
tables are not. Shared deserialization is not a reason to preserve the hole.
Introduce strict config-facing wrappers or validate the original TOML key set
before conversion.

---

## What I would do with this

Three renames I would push hardest for on this surface:

1. **Split the three admissions.** Keep the word for the outbound lane contract,
   rename the history gate to slots, rename `Run::admit` to attach. This is the
   only finding here where one word actively prevents a reader from forming a
   correct model, and two of the three senses are reachable by one client in one
   run.
2. **Get boats and rivers out of client-facing refusal bodies.** The generator
   control refusals genuinely expose whether a boat is seated, contradicting
   the glossary's unobservability claim. `boat_clock` is not part of that
   defect: it exposes a clock axis, not hull sharing, and belongs in the
   separate clock-axis wire cleanup.
3. **`Config::symbol` -> `boot_symbol`.** One serde rename, and it removes the
   single most likely misreading of the config file: that the venue serves one
   instrument.

The two glossary edits that are not optional because they are false rather than
thin: the Run/Ledger "one venue is one run is one ledger" premise that
`config.rs` still asserts (the glossary is right, the config doc is wrong), and
the Boat entry's unobservability claim as violated by generator-control
refusals that disclose and branch on seating (the clock-axis bit does not).

And the structural move, which is the same one pass 1 reached from the other
side: this crate's refusal bodies are the venue's real consumer documentation -
they are remedy-shaped, they name config keys, they explain rules - and they are
the only place several of these contracts are written down. That is the wrong
home for the same reason a doc comment on a wire variant is: a client cannot
read a refusal it has not triggered. `reference/wire-vocabulary.md` (pass 1's
proposal) should absorb the venue's refusal taxonomy, the status-code mapping
and the capacity vocabulary, and the bodies should then cite rules rather than
teach them.
