# Glossary scope pass 1: the wire contract

Inventory of `mogwai-protocol`'s public surface against `reference/glossary.md`.
No edits were made to anything but this file. Rows are grouped by direction;
within a group they run roughly in order of how much a rename would cost.

**Pass 2 reconciliation (2026-08-21).** The second pass read the glossary and
the scoped code before opening this report. Its row annotations use **[P2
ADDED]**, **[P2 CHANGED]**, and **[P2 REFUTED]**. A refuted row remains in place
so the disagreement is visible. Pass 2 also enforced the brief's narrower
surface: `ClientMessage`, `ServerMessage`, `control::Divergence`, `close`,
`risk`, `launch`, `ReadyRecord`, and validators/constants attached to those.
Rows about the rest of `havoc`, `instruments`, `sizing`, `clock`, or `seeds`
are retained as first-pass provenance but are not findings in this inventory.

Direction key: **1** a glossary term used for something that is not that term's
job; **2** something whose job the glossary already names, under a different
word; **3** load-bearing and undefined; **inherited** vocabulary this project
does not own (nautilus, or immovable industry usage) - recorded so it can be
quarantined, never renamed.

Reach key: `local` crate-local, `cross` across a crate boundary, `wire` on the
wire (JSON field or variant name, close-frame reason, or `ReadyRecord` key).

---

## Direction 1 - a glossary term doing a job that is not its own

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Divergence | `reference/glossary.md`, "Divergence" entry: "Generator arms (`VolStorm`, `FlowSurge`, `LiquidityDrought` and kin)" | doc prose | claims three named generator arms are Divergences | - | 1 | **The glossary is wrong.** `VolStorm` and `LiquidityDrought` are `MarketRegime` variants, not `Divergence` variants, and `havoc.rs`'s own module doc says a regime "never travels the `/control/divergence` control plane". Only `FlowSurge` of the three is a `Divergence`. Fix the entry: Divergence is what `POST /control/divergence` accepts; MarketRegime is the generator-side sibling carried per request. |
| Divergence | `reference/glossary.md`, "Divergence" entry: arm list | doc prose | enumerates examples inside a classification | - | 1 | **[P2 CHANGED]** The first-pass verdict contradicts itself: `RejectNextCancel` is present in the glossary and was not omitted. `SessionEdgeSpike` and `ReopenGap` are not `Divergence` variants, so their omission from a Divergence entry is correct. The surviving defect is narrower: `CancelOpenOrderSilently` is the one account-side Divergence family member the classification does not place, while `CommandLatency` cannot be placed wholly in the transport bucket because its act and ack halves have different jobs. Do not turn an example list into an exhaustive enum catalogue; correct the category boundaries instead. |
| Divergence / Transport arm | `reference/glossary.md`: "Transport arms (`GoDark`, `StallData`, `DelayAcks`, `CommandLatency`) corrupt what one account's connections receive" | doc prose | classifies `CommandLatency` as corrupting reception | - | 1 | Half wrong. `CommandLatency`'s `*_act_ms` fields delay when the venue **acts** on a command - that is engine-side timing, not what a connection receives - and `control.rs` says the mutation happens after the sleep. Only the `*_ack_ms` half is transport. Either split the arm in the glossary or drop the "corrupt what is received" test for it. |
| Passenger | `close::DURATION_COMPLETE` = `"passenger duration complete"` | constant / wire (close reason) | this **socket's** configured `duration_ms` elapsed | wire | 1 | Glossary Passenger is "one per account, not per connection". The duration this reason announces is per socket - `ServerMessage::RunComplete`'s own doc says "a socket may carry its own `duration_ms` ... so passengers on one boat complete independently", which uses the word the same wrong way. Rename the reason to `"connection duration complete"` (a designed wire break, in scope) or redefine Passenger. Do not leave both readings alive: this string is a protocol discriminator. |
| Run / RunComplete | `ServerMessage::RunComplete`; `reference/glossary.md`, `RunComplete` | variant / wire | announces either the run's declared duration or one connection's independently declared duration | wire | 1 | **[P2 ADDED]** `Run` is one foreground venue process, but the same `RunComplete` frame is emitted when a single socket's duration elapses while the run continues. The glossary accurately admits both behaviours yet calls both `RunComplete`, so its entry records rather than resolves the overload. Split the wire vocabulary, for example `RunComplete` for venue-wide completion and `ConnectionComplete` for the per-connection deadline. Merely renaming the close reason leaves the primary text-frame discriminator wrong. |
| Account policy | `risk::AccountPolicy` | type | risk rules only - no opening balance field exists | wire, cross | 1 | Glossary: "Account policy: the rules an account is enforced under - **opening balance plus risk rules**". `AccountPolicy` carries no balance; the opening balances live in `mogwai-venue`'s `OpenAccountRequest.balances`. The glossary describes a type that does not exist. Either fold the balances in or correct the entry. |
| Seat / river | `risk.rs` `MaxPosition` doc: "An account is on at most one river, so one number is enough" | doc prose | justifies a single scalar position cap | wire, cross | 1 | **Direct contradiction with the glossary's Seat entry**, which says "An account holds as many seats as the distinct boats its sockets have bound, so one account trades many rivers at once (many strategies, one ledger)". One of the two is false. If the glossary is right, `MaxPosition { quantity }` is unsound as a per-account cap across symbols and the type owes a per-symbol shape. Flagged again in the lateral section - this is the most consequential row in the file. |
| Client | `ClientHavoc`, `HavocSpec.client` | type, field | havoc the **adapter applies to its own inbound stream** - it never crosses the native WS protocol | wire (config), cross | 1 | **[P2 REFUTED: out of scope.]** `ClientHavoc` and `HavocSpec` are not among the public surfaces named by the brief, except insofar as the Divergence validator touches `control::Divergence`. The naming concern may be real in a broader glossary pass, but it is not a finding in this inventory. |
| Client | `MAX_CLIENT_ID_LEN`, `truncate_client_id` | constant, fn | the cap on **any** client-supplied echoed identifier: `client_order_id`, `request_id`, `order_list_id`, `position_id`, linked ids | wire, cross | 1 | The name says "client id" - a thing that does not exist on this protocol (a client is identified by `session`, and that has its own `MAX_SESSION_LEN`). The constant's own doc has to enumerate what it covers because the name does not. `MAX_ECHOED_ID_LEN` / `truncate_echoed_id`. |
| Connection | `ConnHavoc`, `HavocSpec.conn` | type, field | adapter transport-machinery knobs: reconnect backoff, idle timeout, WS ping interval, HTTP quota, HTTP request timeout | wire (config), cross | 1 | **[P2 REFUTED: out of scope.]** Neither type is in the surface assigned by the brief. Preserve for a later whole-crate pass, not this report's verdict set. |
| Lane | `reference/glossary.md`, Connection entry: "holds no lane and no seat" | doc prose | uses an undefined queueing term | - | 3 | **[P2 CHANGED]** This is not direction 1: Connection still means a WebSocket. It is direction 3 because `lane` is load-bearing and undefined. Keep the Lane cluster finding below; no rename of Connection follows from this site. |
| Client | `reference/glossary.md`, Client entry | doc prose | names three senses and picks none | - | 1 | The entry **disambiguates rather than defines**, which the brief calls a finding and I agree it is. It is the right instinct in the wrong artifact: a glossary that admits a three-way overload licenses every future site to pick a sense. Pick one (the counterparty process), give the other two their own words (`session` for the identity; the nautilus client objects are inherited and stay quarantined), and let `client_order_id` be a recorded inherited exception rather than a third sense. |
| Freeze | `ReadyRecord.account_ttl_ms` doc: "How long an **UNATTENDED** account survives" | doc prose | the freeze TTL | wire | 1 | The glossary's word for this state is Freeze; the field's own next paragraph then says "is FROZEN, not liquidated". "Unattended" is a fourth word for the same state in the same doc comment. Use Freeze once. |
| Divergence | `havoc::MarketRegime` doc: "It is carried per subscription on `Subscribe`" | doc prose | claims a wire message that was retired | wire, cross | 1 | **[P2 REFUTED: out of scope as a row, lateral finding retained.]** `MarketRegime` is deliberately distinct from `control::Divergence` and is outside the assigned surface. The stale `Subscribe` reference is still a real lateral documentation bug discovered while checking the glossary's false Divergence entry. |
| Divergence / clearing | `control::Divergence::ClearDivergences` | variant / wire | clears only server-owned temporal windows, not all divergences | wire | 1 | **[P2 ADDED]** The plural, unqualified wire name promises a complete clear, but its own contract excludes engine-side one-shots and cannot undo an act delay already being served. Rename to the positive scope it actually clears, such as `ClearTemporalWindows`; do not make it clear queued one-shots merely to satisfy the existing name. |
| Session | `risk::BreachAction::LockUntilReset` and `DailyLossLimit` docs | doc prose / wire-adjacent | the account's UTC reset instant | cross | 1 | **[P2 ADDED]** The risk docs say "next session boundary" and "reset each session". In the glossary, Session is the client identity, while Session calendar is the instrument's weekly market windows. Risk resets at `reset_minute_utc`, an account-policy clock explicitly independent of the instrument calendar. Replace both uses with `account reset` or `daily reset`; otherwise a reader can implement the wrong clock. |

---

## Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Venue | `HavocSpec.server` | field | the `Vec<Divergence>` relayed to the venue | wire (config), cross | 2 | **[P2 REFUTED: out of scope.]** `HavocSpec` is outside the assigned surface. Also, `server` in a client/server pair names a protocol role, not necessarily a synonym competing with the glossary's Venue object. |
| Venue | `ServerMessage`, `ServerClock`, `server_now_ns`, `mogwai-venue` | type, field, crate | frames from the server endpoint; its clock and reading | wire | 2 | **[P2 REFUTED in the scoped part.]** `ServerMessage` is conventional directional vocabulary paired with `ClientMessage`; it does not rename the running process or compete with Venue. Renaming only one side to `VenueMessage` would make polarity less regular, not clearer. `ServerClock` and the crate name are outside the assigned surface. Keep Client/Server for wire direction and Venue for the modeled exchange/process. |
| Tape | `ServerMessage::FeedLagged`, `is_market_data`'s "data watchdog" | variant, doc prose | the boat's broadcast ring overwrote frames for this connection | wire | 2 | The glossary word for the paced frame stream is Tape (and Boat for the ring's owner). "Feed" appears only here. `TapeLagged` costs one wire rename and removes a synonym. |
| Freeze / Session | `ReadyRecord.reset_account_on_reconnect` | field | whether a returning account id gets a clean ledger | wire | 2 | The mechanism the glossary describes is not reconnection - it is resolution on first sight of an account id (Account entry), moderated by Session and Freeze. "Reconnect" implies the venue tracks a prior connection, which it does not. `reset_account_on_reclaim` matches the Eviction/Freeze vocabulary. Low urgency; the field's own doc is accurate. |
| Account policy preset | `risk::SHIPPED_POLICIES`, `shipped_policy` | constant, fn | the named policies this build ships | cross | 2 | **[P2 REFUTED.]** "Shipped" qualifies provenance and availability; it is not an alternate noun for preset. `shipped_policy(name)` resolves the policy presets bundled with the build, exactly compatible with the glossary. Renaming it to `preset` would also collide with instrument presets without adding meaning. |
| Ledger / book | `sizing::BookShape` | type | counts of balances, positions, margins, open orders, closed orders, recorded fills in one engine | cross | 2 | **[P2 REFUTED: out of scope.]** `sizing::BookShape` is not attached specifically to the named contract surfaces. The broader Ledger/book synonym question remains legitimate for another pass. |
| Warmup / boat clock | `ServerClock.boat_clock: bool` | field | whether the reported clock is a boat's or the run's | wire | 2 | **[P2 REFUTED: out of scope and no contradiction shown.]** `ServerClock` is outside the named surface. A boat can remain an unobservable sharing cache while the paced stream and its clock are observable; publishing which clock envelope answered does not reveal whether another connection shares the boat. |

---

## Direction 3 - load-bearing and undefined

The largest group, as expected. The clusters are what matter more than the
individual rows. After pass 2 scope correction, **admission and
refusal/rejection**, **reservation**, **linkage**, **reconciliation**,
**frame/command**, **tape-walk predicates**, **risk enforcement**, and
**launch/readiness** are whole vocabularies the scoped contract runs on and the
glossary is silent about. The first-pass polarity cluster depends mostly on
out-of-scope havoc types and is retained below as provenance, not counted.

### The admission cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `ServerMessage::AdmissionRejected` | variant | the venue refused the work before touching engine state, on capacity grounds | wire | 3 | "Admission" is the contract's own name for a whole refusal class with its own lane, its own byte bound, its own havoc exemption and its own retry semantics. It needs a glossary entry stating: admission is capacity, rejection is business, and the two are different frames on purpose. |
| - | `AdmissionSubject` (+ `Submit`/`SubmitGroup`/`Cancel`/`Modify`/`Query`/`Frame`) | type, variants | what was refused, so the refusal is translatable per command | wire | 3 | Define Subject alongside Admission. `Frame` as a subject variant is the "we could not attribute this at all" case and deserves naming. |
| - | `EventKind::Admission`, `is_admission` | variant, fn | the priority-lane category | cross | 3 | Same entry. |
| - | `ADMISSION_FRAME_MAX_BYTES`, `ADMISSION_ENVELOPE_BYTES`, `JSON_ESCAPE_FACTOR` | constants | the bound that makes a lane frame count a memory bound | cross | 3 | Envelope / escape factor are derivation vocabulary, worth one entry covering the whole sizing method rather than three. |
| - | "priority lane" (`AdmissionRejected` doc, `ProtocolError` doc, `EventKind::Admission` doc), and the glossary's own Connection entry | doc prose | the per-connection outbound queue admission traffic rides ahead of held traffic | cross | 3 | **Lane is used by the glossary and defined nowhere.** Highest-value single entry in this list. |
| - | `retryable` field, "EVERY ADMISSION REFUSAL IS BACKPRESSURE" | field, doc prose | whether the same command later could succeed | wire | 3 | **[P2 CHANGED]** Backpressure is undefined, but the claim is correctly scoped to `AdmissionRejected`; it does not include `ProtocolError`. The glossary entry must preserve that distinction. |
| - | `ServerMessage::ProtocolError` | variant | a frame the venue could not decode or attribute | wire | 3 | **[P2 CHANGED]** `EventKind::Admission` names the priority-lane category, not a capacity verdict. The glossary must distinguish the Admission event category from the narrower `AdmissionRejected` backpressure frame; renaming or adding `retryable` to `ProtocolError` would conflate them. |

### The refusal and rejection cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `AdmissionRejected`, `OrderRejected`, `OrderModifyRejected`, `OrderCancelRejected`; `RejectNextSubmit`, `RejectNextCancel`; validator prose using "refused" | variants and prose | several distinct failures: capacity refusal, pre-acceptance business refusal, post-acceptance terminal rejection, and refusal of a modify or cancel | wire | 3 | **[P2 ADDED]** This is a whole missing vocabulary, not isolated names. The code carefully explains that a refused cancel must not reject the order, but the glossary defines neither Refusal nor Rejection and gives no map of which frame means which state transition. Some spellings are inherited from nautilus, but mogwai owns `AdmissionRejected` and the Divergence variant names and must define their relationship. Do not collapse the frames; add a contract-level taxonomy. |
| Reject / Refuse | `Divergence::RejectNextCancel` and its doc; `ServerMessage::OrderCancelRejected` | variant / wire | force the venue to refuse an otherwise valid cancel while leaving the order resting | wire | 3 | **[P2 ADDED]** The variant says Reject, its doc says Refuse, and the resulting inherited event says Rejected. This is acceptable only if the glossary defines rejection as the emitted event vocabulary and refusal as the venue action. Without that ruling, readers can infer the order itself becomes Rejected, the exact invalid transition `AdmissionSubject` exists to prevent. |

### The reservation cluster - one word, two jobs, both money-adjacent

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `sizing` module: "the server reserves against it before it lets the engine mutate"; `BOUNDARY_REFUSAL_BYTES`; `worst_case_output_bytes` | module, constants, fn | a **byte** reservation against a connection's outbound budget | cross | 3 | |
| - | `SubmitOrder.reduce_only` doc ("Exempt from the funded-admission check and from `locked_balances`"); `Divergence::CancelOpenOrderSilently` ("frees the order's reservation"); `OrderType::StopMarket` doc ("the reservation from the trigger") | doc prose | a **funds** reservation - the `locked` portion of a balance | wire, cross | 3 | **Two unrelated quantities share one word inside one crate**, and `StopMarket`'s doc uses the funds sense two paragraphs from where `sizing` uses the byte sense. Give them separate words - *budget reservation* vs *hold* (the engine already says "freeing a resting order's hold" in `DropNextAccountUpdate`) - and glossary both. |
| - | `Balance.locked` / `free` / `total` | fields | `locked` carries **both** the order hold and equity settlement proceeds (`InstrumentClass::Equity.settlement_ns` doc: unsettled cash "appears as `locked` on the balance row") | wire | 3 | Two economically different things in one wire number, with no way for a client to tell them apart. Glossary owes an entry; the contract arguably owes a split (`locked` vs `unsettled`). Lateral-adjacent. |
| - | "byte budget" (glossary Connection entry: "byte budgets are per connection") | doc prose | the connection's outbound capacity | cross | 3 | Used by the glossary, defined by nothing. Same shape as Lane. |

### The polarity cluster

**[P2 REFUTED AS A CLUSTER FOR THIS SCOPE.]** The polarity disagreement is
real across the full crate, but nearly all its evidence is `ClientHavoc`,
`HavocLatency`, and `ConnHavoc`, which the brief did not assign. Only
`validate_wire_symbol` is scoped, and "client-inbound" there is locally
unambiguous. Keep this cluster for a later whole-havoc inventory.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `ClientHavoc` / `HavocLatency` docs: "inbound event", "the inbound stream", `BASELINE_LATENCY` "inbound network latency" | doc prose, constant | venue -> adapter (frames the adapter receives) | cross | 3 | |
| - | `validate_wire_symbol` doc: "Validate a CLIENT-INBOUND symbol"; `ConnHavoc.idle_timeout_ms`: "no inbound application-data frame" | doc prose | **the opposite polarity in one case and the same in the other**: "client-inbound" means client -> venue, the adapter's "inbound" means venue -> adapter | wire, cross | 3 | The word flips meaning depending on whose stream you stand in. This is a live comprehension hazard on the one crate both ends read. Fix by naming the ends, not the direction: `to_venue` / `from_venue`, and glossary the pair. |

### The linkage cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `OrderLink`, `SubmitOrder.link` | type, field | one order's membership of an order list, as a group id plus a rule | wire | 3 | The whole atomic-group guarantee is stated in `ClientMessage::SubmitOrderGroup`'s doc and nowhere durable. Glossary owes: Linkage, Group, Sibling, Parent/Child, and the *atomic admission* guarantee itself. |
| - | `linked_order_ids`, `parent_order_id`, `order_list_id` | fields | siblings acted on; the order this one waits for; the list identity | wire | 3 | `order_list_id` is nautilus-shaped (`OrderList`) - see inherited. `parent`/`child` are ours. |
| - | "bracket" (`SubmitOrderGroup` doc, `Contingency::Ouo` doc, `MAX_LINKED_ORDERS` doc) | doc prose | the two-or-three-leg entry/stop/target shape | cross | 3 | Industry term, but load-bearing in the *justification* of `MAX_GROUP_ORDERS = MAX_LINKED_ORDERS + 1`. Worth an entry saying what mogwai means by it. |
| - | `MAX_LINKED_ORDERS`, `MAX_GROUP_ORDERS`, `LINKAGE_MAX_BYTES` | constants | caps that make the batch's output computable in advance | wire, cross | 3 | Fine as names; they inherit whatever Linkage ends up meaning. |

### The reconciliation cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `ClientMessage::QueryOrders` / `QueryFills` doc: "venue-truth", "the second, independent **witness**", "honest-content invariant" | doc prose | the guarantee that a query's content never lies even when its delivery is havoc'd | wire | 3 | This is one of the strongest contracts on the protocol and exists only as a doc comment on two variants. Glossary owes: Venue truth, Witness, and the content/delivery split. |
| - | `OrderStatusSnapshot`, `FillSnapshot`, `SNAPSHOT_ENVELOPE_MAX_BYTES` | types, constant | a point-in-time truthful read | wire | 3 | "Snapshot" is also used for `AccountState` in `HavocLatency.data_nanos`' doc ("Account-state snapshots are execution traffic") and for the server's account snapshot elsewhere. Three snapshot kinds, no entry. |
| - | `WireOrderStatus` (+ `Accepted`/`Triggered`/`PartiallyFilled`/`Filled`/`Canceled`/`Expired`/`Rejected`), `is_open` | type, variants, fn | states the venue itself can attest to | wire | 3 | The `Wire` prefix exists to separate it from nautilus's `OrderStatus`; the variant spellings are inherited. What is undefined is *open* - `is_open` includes `Triggered`, which is a real ruling with a real reason and lives only in a `//` comment. |
| - | `request_id` doc: "correlation id echoed verbatim" | field, doc prose | matching replies to requests on a shared socket | wire | 3 | One entry. |
| - | `QueryKind` (`Orders` / `Fills`) | type | which query a refused subject refers to | wire | 3 | Inherits Query's entry. |

### The frame/command cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | "frame" - `AdmissionSubject::Frame`, `ADMISSION_FRAME_MAX_BYTES`, `MAX_CLIENT_MESSAGE_BYTES` doc, `close.rs` "control frame", `Heartbeat` doc "frame-active" | variant, constants, doc prose | a websocket message; also a serialized `ServerMessage`; also an RFC 6455 control frame | wire, cross | 3 | Three senses. The glossary's Tape entry says "paced frame stream" and stops. This is the most-used undefined noun in the crate. Define it as the serialized message and use "websocket frame" / "control frame" explicitly for the transport senses. |
| - | `CommandClass` (`Submit`/`Modify`/`Cancel`), `CommandClass::of` | type, fn | which order-entry command produced an execution frame, for ack latency | cross | 3 | "Command" appears on the wire only as this classification and in `CommandLatency`. Undefined, and its boundary against "message" and "frame" is decided per site. |
| - | `Divergence::CommandLatency` `*_act_ms` / `*_ack_ms` | fields | act = how long the venue takes to mutate; ack = how long to report | wire | 3 | Act/ack is a genuinely useful distinction and appears nowhere else. Glossary it. |
| - | `ServerMessage::Heartbeat` | variant | server-originated liveness text frame, survives `StallData` | wire | 3 | **Collides with `ConnHavoc.heartbeat_interval_ms`**, which is a websocket PING on the adapter side. Two heartbeats, one word, one crate, opposite directions. Rename one (`Heartbeat` the frame vs `ping_interval_ms` the knob) and define the survivor. |
| - | `EventKind` (`Exec`/`Fill`/`Data`/`Admission`), `is_execution`, `ServerMessage::category` | type, fns | the single classification both ends key havoc off | cross | 3 | The classification is the load-bearing thing here and it is stated well in code. It is absent from the glossary, so "execution event" and "market data" are used in the Divergence entry with no definition behind them. |

### The tape-walk predicate cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `trades_through` | fn | a print strictly through a resting limit - the queue argument | cross | 3 | Ours entirely, shared by the engine and the data walk, and the whole fill model rests on the strictness split. Glossary owes: Through, Touch, and why they differ. |
| - | `touches_trigger`, `touches_toward` | fns | a print reaching a stop's trigger; reaching a touched order's trigger from the entry side | cross | 3 | Same entry. `toward` vs `touch` as opposite-direction twins is a distinction a reader cannot recover from the names alone. |
| - | `ScanKind` (`FillThrough`/`TriggerTouch`/`TriggerToward`), `ScanKind::hit` | type, variants, fn | which predicate a tape walk applies to one resting order | cross | 3 | "Scan" and "sweep" are both used (`swept_fill_max_bytes`, "a later sweep that a second fill could beat", "scanned by nothing"). Two words for the walk. Pick one. |
| - | `Hit` (`ts_ns`, `px`) | type | the print that satisfied a scan | cross | 3 | Inherits Scan's entry. |
| - | "band", "drawn band trigger" (`ScanKind::FillThrough` doc, `RunSeeds.fill` "fill band's draw stream") | doc prose, field | the per-order randomized fill trigger the generator draws | cross | 3 | Load-bearing for tape determinism (`RunSeeds.fill`, and a `TAPE_PROTOCOL_VERSION` subject) and defined nowhere. |
| - | "print" | doc prose, pervasive | one trade tick on the tape | cross | 3 | Industry-adjacent but used here as the atomic unit the whole scan vocabulary is defined over. One line in the glossary. |

### The risk cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `risk::RiskState` (`equity`, `peak_equity`, `day_open_equity`) | type, fields | what the venue is enforcing against this account right now | wire | 3 | Glossary's Account policy entry names the triple and defines none of its terms. Equity in particular has a precise, restrictive definition here (balance plus unrealized on positions settling in `currency`, no exchange rate) that a consumer cannot guess. |
| - | `Breach`, `BreachAction` (`LockUntilReset`/`Terminate`), `BreachedRule` | types, variants | a rule that fired and what it did | wire | 3 | "Breach", "lock", "terminate", "reset" all undefined. `LockUntilReset`'s reset is `reset_minute_utc`, which is an account fact, not a session-calendar fact - the glossary's Session calendar entry is a different clock and the two must not be conflated. |
| - | `TrailingDrawdown`, `OverallDrawdown`, `DailyLossLimit`, `TrailingBasis` (`PeakEquity`/`EndOfDayBalance`), `lock_at_equity` | types, variants, field | the three floor shapes and what a trail ratchets on | wire | 3 | Drawdown, trail, ratchet, floor, high-water mark - five undefined terms, each doing real work. The docs on these types are excellent; they are just not durable. |
| - | `MaxPosition` | type | a cap the venue refuses entry against rather than flattening after | wire | 3 | See the direction-1 row: its own doc contradicts the glossary's Seat entry. |
| - | `AccountPolicy::is_unpoliced` | fn | "no rule is set" | cross | 3 | "Policed"/"unpoliced" is a fourth word in the enforcement vocabulary. |
| - | `reset_minute_utc` | field | minute of the UTC day the daily budget resets | wire | 3 | The glossary Account policy entry says "the account defines its own day as a minute of the UTC day", which is right - but the *edge case the field's doc names* (a footprint that never contains the instant, so a daily limit silently becomes a run-lifetime limit) is a real behavioural gap and belongs where it can be cited. |

### The instrument cluster

**[P2 REFUTED AS A CLUSTER: out of scope.]** `FundingTerms`,
`InstrumentClass`, `InstrumentDef`, `WireAssetClass`, and `default_instruments`
are not among the surfaces assigned by the brief. The stale
`default_instruments` documentation remains a valid lateral finding, but these
rows do not count toward the scoped vocabulary inventory.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `FundingTerms` (`interval_ns`, `interest`, `index_symbol`, `clamp`), `FundingTerms::rate` | type, fields, fn | funding = interest plus mark-versus-index premium, clamped | wire, cross | 3 | Glossary Instrument class says a perpetual "pays funding between long and short at an interval" and stops. Funding, interest, premium, index, clamp are five undefined terms behind a computed cash flow. |
| - | `InstrumentClass::Perpetual.funding_rate` vs `FundingTerms.interest` | field vs field | **the same quantity under two names** | wire, cross | 3 | The config spells it `funding_rate`, the derived struct spells it `interest`, and `FundingTerms`' own doc has to say "`interest` is `funding_rate` on the class". Two words, one quantity, one crate. Pick one; `interest` is the more honest since it is explicitly the zero-premium term rather than the live rate. |
| - | `InstrumentClass::Equity.lot_size`, `borrowable`, `settlement_ns` | fields | round lot; short capacity; T+N in sim nanoseconds | wire | 3 | Glossary Instrument class names all three in passing ("carrying lot size, borrowability and a settlement period") without defining any. Locate, hard-to-borrow, unsettled are all in the docs and none is glossed. |
| - | `InstrumentDef` | type | the resolved instrument shape | wire, cross | 3 | Glossary talks about "the resolved bundle" (River entry) and "a legal, fundable shape" (Served symbol entry) - so *shape*, *bundle* and `InstrumentDef` are three names for one thing, and none is an entry. |
| - | `InstrumentDef::tick_value`, `notional`, `unrealized` | fns | derived quantities | cross | 3 | Tick value and Multiplier ARE glossed and the entries are good. Notional is glossed inside Multiplier; unrealized is not glossed anywhere though `Position.unrealized_pnl` is on the wire. |
| - | `WireAssetClass` (`Fx`/`Equity`/`Commodity`/`Index`/`Cryptocurrency`) | type | asset class, mirrored for the wire | wire | 3 (mostly inherited) | The variants are nautilus's; the `Wire` prefix is ours and means "our mirror of a nautilus type" - a convention shared only with `WireOrderStatus` and never stated. Worth one entry defining the `Wire` prefix as a naming rule. |
| - | `default_instruments` | fn | the seed instrument set | cross | 3 | Its doc says "Today this is the single BTCUSDT instrument" and the function visibly returns MNQ, MES, an Index, several Cryptocurrency instruments and more. **Stale doc; see lateral findings.** |

### The launch and readiness cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `ReadyRecord`, `ReadyRecord::VERSION`, "readiness line", "the readiness handshake" | type, constant, doc prose | the one JSON line on stdout and the protocol around reading it | wire | 3 | Glossary HAS a ReadyRecord entry (one sentence, accurate). It does not define the *handshake* - the four load-bearing properties in `launch`'s module doc (dedicated OS thread for `PR_SET_PDEATHSIG`, continuous stderr drain, unbounded ready read, version-first) are the actual contract and live only there. |
| - | `LaunchSpec`, `launch`, `LaunchedVenue`, `serve_argv` | type, fns | Transient mode's machinery | cross | 3 | Glossary Transient mode names `launch(spec)` correctly. `LaunchedVenue`'s ownership semantics (holding it keeps the venue alive; dropping blocks ~300us) are a real consumer contract with no durable home. |
| - | `StderrSink` (`Inherit`/`Discard`/`Lines`) | type | what to do with the venue log, all variants draining | cross | 3 | Local, low priority. |
| - | `LaunchError` variants (`Spawn`/`Thread`/`Read`/`NoRecord`/`Timeout`/`Malformed`/`Version`/`OwnerDied`/`Teardown`/`ZeroReadyTimeout`) | variants, error text | why a launch did not produce a serving venue | cross | 3 | Error text is excellent and remedy-shaped. "Owning thread", "readiness reader thread", "teardown" are undefined but crate-local. Low priority. |
| - | `VenueExit` | type | how a venue that ended on its own ended | cross | 3 | Inherits Venue and Run. Fine. |
| - | `RunSeeds` (`run`, `fill`, `tape_for`), `splitmix64`, domain constants | type, fields, fn | every random stream in one run, derived from one seed | cross | 3 | **[P2 REFUTED: out of scope.]** The label-versus-shape fact is important to River identity, but `RunSeeds` and its validators/constants were not assigned. |
| - | `SimClock`, `ServerClock`, `validate_sim_clock`, `speed` | types, fn, field | the affine wall-to-sim map; the `/clock` envelope | wire, cross | 3 | **[P2 REFUTED: out of scope.]** These are separate public exports, not validators/constants riding with the named surfaces. A clock-vocabulary pass should reconcile them with `reference/clock.md`. |

### Miscellaneous undefined nouns

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `Divergence::FaultTape` doc: "through its fault channel"; `FeedLagged` doc: "a venue fault" | variant, doc prose | terminal venue failure, WS 1011, nonzero exit | wire | 3 | Fault is the one terminal class `close` does NOT model (see lateral). Undefined, and it is the distinction between "stop" and "your venue died". |
| - | `close::Terminal` (`RunComplete`/`DurationComplete`/`Evicted`), `classify`, `EVICTED_PREFIX`, `fit_reason`, `MAX_REASON_BYTES` | type, fns, constants | what a graceful close means to a client deciding whether to redial | wire | 3 | Glossary defines Eviction well and RunComplete well. It does not define **Terminal** - the "a reason this module does not recognize is NOT terminal" default is the load-bearing safety rule and lives only in the module doc. |
| - | "redial" (`close` module doc, Eviction glossary entry) | doc prose | a client reconnecting after a close | cross | 3 | Used by the glossary ("A consumer must not treat it as a reason to redial") without definition, and it is the behaviour the whole `close` module exists to govern. |
| - | `POST_ONLY_REFUSAL` | constant / wire (refusal text) | the one refusal string both gates speak | wire | 3 | The convention it embodies - a refusal that *names both sets* rather than stating a rule, parsed by the engine's admission-table test up to the first `" orders"` - is a protocol contract encoded in a string literal's punctuation. Glossary or `reference/` owes it a home. |
| - | `MAX_CLIENT_MESSAGE_BYTES`, `MAX_SYMBOL_LEN`, `MAX_SESSION_LEN`, `MAX_ACCOUNT_ID_LEN`, `MAX_CURRENCY_LEN`, `MAX_REASON_LEN` | constants | the wire's length caps | wire | 3 | Individually fine. Collectively they encode "the URL-safe alphabet", which `validate_wire_symbol` and `validate_session_id` both spell out and `AccountId::parse` spells *differently* (it also admits `:`). Three alphabets, one described as "same reasoning as" another. Glossary the alphabet once, or better, share one predicate - see lateral. |
| - | `AccountId`, `AccountIdError` | type, error | a venue account identity, log-safe | wire | 3 | Glossary Account is good and covers the bearer-token property. The *newtype* is load-bearing beyond that: `sizing::account_state_max_bytes` charges the account id at its RAW cap precisely because the type's alphabet contains nothing serde escapes. That is a type invariant funding a memory bound and it deserves saying. |
| - | `decimal::str_option`, "string-spelled", `decimal_from_f64` / `decimal_to_f64` | module, doc prose, fns | the money-is-a-string rule and its exceptions | wire | 3 | "THE LINE IS MONEY, NOT Decimal everywhere" is a genuine protocol contract with an exhaustive-by-hand exception list living in a `#[test]` doc comment. It belongs in `reference/`. |
| - | `Symbol = Arc<str>` | type alias | a symbol on the wire | wire | 3 | Glossary has Served symbol and Boot symbol but no plain Symbol. Minor. |

---

## Inherited - quarantine, do not rename

Recorded so a later pass does not re-litigate them. Nautilus API names and
immovable industry terms.

| term | site | kind | source |
|---|---|---|---|
| `OmsType` (`Netting`/`Hedging`) | `instruments.rs` | type, variants | nautilus `OmsType` |
| `Contingency` (`NoContingency`/`Oco`/`Oto`/`Ouo`) | `messages.rs` | type, variants | nautilus `ContingencyType`; OCO/OTO/OUO are FIX-era industry |
| `OrderType` variants (`Market`, `Limit`, `StopMarket`, `StopLimit`, `MarketIfTouched`, `LimitIfTouched`, `MarketToLimit`, `TrailingStopMarket`, `TrailingStopLimit`) | `messages.rs` | variants | nautilus `OrderType` + universal exchange vocabulary |
| `TimeInForce` (`Gtc`/`Ioc`/`Fok`/`Day`/`Gtd`) | `messages.rs` | variants | FIX |
| `AggressorSide` (`NoAggressor`/`Buyer`/`Seller`), `LiquiditySide` (`Maker`/`Taker`) | `messages.rs` | types | nautilus; maker/taker is universal |
| `Side` (`Buy`/`Sell`) | `messages.rs` | type | universal |
| `leaves_qty`, `last_qty`, `last_px`, `avg_px`, `mark_px`, `unrealized_pnl`, `commission`, `commission_currency` | `messages.rs` | fields | nautilus / FIX |
| `client_order_id`, `venue_order_id`, `trade_id`, `position_id`, `order_list_id` | `messages.rs` | fields | nautilus id types; `client_order_id`'s "client" sense is the glossary's third sense and is inherited, not ours |
| `ClientOrderId` / `VenueOrderId` type aliases | `lib.rs` | aliases | nautilus |
| `post_only`, `reduce_only`, `expire_time`, `trigger_price`, `trail_offset`, `limit_offset` | `messages.rs` | fields | nautilus order fields |
| `bid_px`/`ask_px`/`bid_sz`/`ask_sz`, `ts_event` | `messages.rs` | fields | nautilus tick shape |
| `price_precision`, `size_precision`, `price_increment`, `size_increment` | `instruments.rs` | fields | nautilus `Instrument` |
| Asset-class spellings (`Fx`, `Equity`, `Commodity`, `Index`, `Cryptocurrency`) | `instruments.rs` | variants | nautilus `AssetClass` |
| `spot` / `future` / `perpetual` / `inverse`, "notional", "VWAP", "variation margin", "maintenance"/"initial" margin, "round lot", "T+N", "drawdown" | throughout | prose, variants | universal derivatives vocabulary |
| WS 1000 / 1011, RFC 6455 control-frame framing | `close.rs` | constants, prose | RFC 6455 |
| `splitmix64`, FNV constants | `seeds.rs` | fn, constants | published algorithms |

Note that three "sides" coexist - `Side`, `AggressorSide`, `LiquiditySide` -
and all three are inherited, so the collision stays. Worth one glossary entry
naming the three rather than three renames.

---

## Lateral findings

Ordered by how much I think they matter.

**1. `CommandClass::of` is a vacuous gate waiting to happen.** It matches
`SubmitOrder | SubmitOrderGroup`, `ModifyOrder`, `CancelOrder`, then `_ => None`.
A new order-entry variant added to `ClientMessage` compiles clean and is
silently classless, so it gets no per-command ack latency and `CommandLatency`
quietly stops covering it. This is precisely the shape `EventKind::is_execution`
in the sibling module goes out of its way to avoid - its doc says it is written
as an exhaustive match "so the compiler carries the claim: a new kind must opt
IN". Two classifiers, one crate, opposite disciplines. Make `of` exhaustive:
list `QueryOrders` and `QueryFills` explicitly and delete the wildcard.

**2. `MaxPosition`'s premise contradicts the glossary's Seat entry.** `risk.rs`
justifies a single scalar cap with "An account is on at most one river, so one
number is enough". The glossary says an account holds as many seats as the
distinct boats its sockets bound and "trades many rivers at once". If the
glossary is right, a policed account trading MNQ and BTCUSDT is capped by one
number over two incomparable size units - contracts and base units - and the
venue is enforcing a quantity that means nothing. This needs an owner ruling
before either document is edited, and it may be a live correctness bug rather
than a naming one.

**3. Three doc references to types that do not exist.** `risk::RiskPolicy` is
named in `risk.rs` (`[RiskPolicy] beside it stays number-tolerant`),
`decimal.rs` (`RiskPolicy inside it`) and `messages.rs`'s decimal-contract test
doc (`risk::RiskPolicy and risk::AccountPolicy around it`). There is no
`RiskPolicy` in the workspace - the type is `AccountPolicy`. The same
`messages.rs` sentence also names `instruments::InstrumentSpec`, which does not
exist either; the type is `InstrumentDef`. That sentence is the *exhaustive
tolerant-decimal list*, so it is durable prose asserting a live fact about types
it misnames. Two of the three sites are rustdoc intra-doc links that presumably
render as plain text rather than erroring - worth checking whether
`broken_intra_doc_links` is enabled, because if it is not, nothing in this
workspace catches a dangling doc link.

**4. `default_instruments`'s doc is flatly false.** "Today this is the single
BTCUSDT instrument." The function returns MNQ, MES, an index instrument,
multiple cryptocurrency instruments, a perpetual and an inverse - eight-plus
definitions across several hundred lines. A reader trusting the doc would
conclude the venue seeds nothing else. Same family as finding 3.

**5. [P2 REFUTED] "EVERY ADMISSION REFUSAL IS BACKPRESSURE" is not wider than
the code.**
That claim is stated on `AdmissionRejected`, and `retryable` encodes it as data.
`ProtocolError` also classifies as `EventKind::Admission`, but that enum is an
outbound-lane category, not a claim that every member is an
`AdmissionRejected`. The quoted sentence is inside the `AdmissionRejected`
variant doc and says "what `retryable` says as DATA"; `ProtocolError` has no
such field and cannot be read as its subject. Adding `retryable` to a malformed
frame error would conflate two contracts. The glossary still owes the admission
taxonomy, but the code claim is correctly scoped.

**6. [P2 REFUTED] `close` correctly models graceful stop decisions, not every
fault.** `FeedLagged`'s doc
says the server closes with WS 1011 after delivering it, and `FaultTape` tears
the venue down. `close::classify` returns `None` for every non-1000 code, and
the module explicitly defines `None` as a transport event whose redial policy
belongs to the client. That is the correct result for 1011: a venue fault did
not complete the run and must not become a stop-without-redial `Terminal`.
`FeedLagged` supplies a preceding typed message when that specific fault is
available; `FaultTape` may only yield transport failure. Adding `Fault` to this
enum would make its safety policy worse unless the enum is first redesigned to
encode retry actions rather than terminals.

**7. [P2 CHANGED] Four validator branches hardcode a bound the constant owns.**
`validate_divergence` returns `"DelayAcks/GoDark/StallData ms must be <= 3600000
(one hour)"` and `"CommandLatency fields must each be <= 3600000 (one hour)"`,
and the `FlowSurge.duration_ms` and `FeeSurcharge.window_ms` branches each embed
`3600000` too, all while checking against `control::MAX_DIVERGENCE_MS`. Move the
constant and four messages lie, and nothing notices. `validate_session_id`'s test explicitly
guards against exactly this ("THE MESSAGE AND THE CONSTANT ARE CHECKED AGAINST
EACH OTHER"), so the habit exists in this crate and these two sites missed it.
Cheap fix: format the constant into the message, or assert containment in a test.

**8. [P2 CHANGED] Two copied URL-alphabet validators and one deliberately
different account alphabet.**
`validate_wire_symbol` and `validate_session_id` are byte-for-byte the same
alphabet with different caps and different message text, and `validate_session_id`'s
doc says it "shares the URL alphabet with a wire symbol". `AccountId::parse`
implements a different rule, the same alphabet plus `:`, and its own comment
does not mention the relationship. The first-pass proposal to collapse all
three behind `url_safe(bytes, extra)` would obscure that account ids are not
described as URL values and have their own log-safety contract. Share the exact
symbol/session predicate if desired; keep AccountId's policy distinct and state
why `:` is admitted. This is a maintainability smell, not evidence that the
current account alphabet is accidental.

**9. `AdmissionRejected`'s doc comment appears to be two docs spliced.** It
opens with "The venue REFUSED to do the work, before any engine state was
touched...", runs through the admission-truth paragraph, and then starts over
with "The venue could not admit a command or a frame, and said so instead of
dropping it." followed by the `retryable` argument. The second opening is
orphaned mid-comment. Editorial, but it is the variant every consumer reads
first.

**10. `Balance.locked` carries two economically different quantities.** Order
reservations and unsettled equity sale proceeds both land in one wire number
(`InstrumentClass::Equity.settlement_ns` doc states the second explicitly). A
client cannot tell "I have money tied up in resting orders" from "my sale has
not settled", and those have opposite remedies - cancel an order versus wait.
Worth considering a third field on `Balance`. Pre-1.0, and `Balance` already
has a `sizing` row constant that would just need re-deriving.

**11. `MarketToLimit`'s own doc says the engine's behaviour is broken in both
halves.** Recorded here only because the note is inside the *wire contract* -
`OrderType::MarketToLimit`'s doc says its fill takes the whole quantity at the
order's own limit price with no reference to the tape, and that a
divergence-manufactured remainder rests INERT and can never fill or expire. That
is an open engine defect documented on the protocol type. It is honest and
well-written, and it means the wire admits a type the venue does not implement.
Not my scope to fix; flagging that the inventory found a known-broken variant on
the contract both ends serialize against.

**12. [P2 ADDED] `MarketRegime` documents a retired `Subscribe` carrier.** This
type is outside the assigned row inventory, but the stale statement was exposed
while checking the glossary's false claim that market regimes are Divergences.
`havoc.rs` says the regime is "carried per subscription on `Subscribe`" while
`ClientMessage` has no such variant and its regression test explicitly refuses
that tag. The actual live carriers need to replace this sentence. This is a
vacuous-documentation-family defect: the type-level comment promises a wire
path that cannot be constructed.

---

## What I would do with this

The three scoped renames I would push hardest for, if the owner only wants
three:

1. Split `ServerMessage::RunComplete` into venue-wide and connection-scoped
   completion variants, and rename `"passenger duration complete"` in the same
   designed wire break. Fixing only the close reason leaves the primary text
   frame overloaded.
2. `ClearDivergences` -> `ClearTemporalWindows`, because the current command
   explicitly leaves several divergences armed and cannot undo an act already
   sleeping.
3. `MAX_CLIENT_ID_LEN` -> `MAX_ECHOED_ID_LEN`, because the constant covers
   several client-supplied id namespaces while a client identity is `session`.

And the two glossary edits that are not optional, because they are false rather
than merely thin: the Divergence entry's generator-arm list, and whichever half
of the `MaxPosition` / Seat contradiction turns out to be wrong.

The direction-3 clusters are big enough that I would not treat them as a rename
loop at all. Admission/refusal, reservation, linkage, reconciliation,
frame/command, tape-walk, risk, and launch/readiness are vocabularies whose
definitions currently live in doc comments on individual wire variants and
types. That is the wrong home: a consumer reading
`reference/` cannot find them, and a doc comment on a variant cannot state a
contract that spans six types. The structural move is a
`reference/wire-vocabulary.md` that lifts them out, with the glossary carrying
one-line entries pointing at it - and the doc comments then shrinking to say
what the variant does rather than re-deriving the vocabulary each time.
