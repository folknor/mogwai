# Implementation spec: the per-entry subscription protocol (workstream B)

Written against `reference/technical-implementation-spec.md`. Spawned from the
`docs/todo.md` item "The subscription protocol redesign (workstream B in
`docs/protocol-problem.md`)" and from `docs/protocol-problem.md`'s workstream B
section, which records the agreed shape and lists what this document must
settle. Workstream A (the exec pump rewrite, `AdmissionRejected`, the priority
lane) has landed and is not re-opened here.

This is a full coherent rewrite of the subscription request/response surface,
not a local change: `ClientMessage::Subscribe` changes shape, one server-to-
client variant is added, one `AdmissionSubject` variant is deleted, and the
adapter's reconnect fan-out is replaced. Pre-1.0, the wire break is legal and
both ends of it live in this workspace.

## 1. What is being fixed

Two defects, one landing, because they need the same wire change (the argument
is in `docs/protocol-problem.md` problems 3 and 4 and is not re-derived here).

- A subscription diagnostic names no subscription. `ProtocolError { reason,
  ts_event }` carries the symbol only as prose, and nothing at all identifying
  WHICH subscription generation it describes. The `Subscribe` handler quiesces
  and replaces a symbol's in-flight replay on every resubscribe, so a
  diagnostic discovered asynchronously (the dead seek, emitted from the replay
  thread) can describe a generation that no longer exists, and the client has
  no field with which to discard it.
- The adapter sends one `Subscribe` frame per symbol on every reconnect
  (`subscribe_commands`), because the wire carries ONE request-wide `start_ts`
  while each symbol's resume cursor advances independently. Batching without a
  per-entry cursor would clobber cursors.

## 2. Survey of the ground

Everything that touches the subscribe path today. This is the teardown
inventory; nothing outside it is in scope.

### mogwai-protocol

- `messages.rs` `ClientMessage::Subscribe { symbols: Vec<Symbol>, start_ts:
  Option<u64>, regime: Option<MarketRegime> }`, with `serde(default,
  skip_serializing_if)` on both options.
- `messages.rs` `AdmissionSubject::Subscribe { symbols: Vec<Symbol>,
  refused_total: usize }` - used by exactly one construction site (the server's
  coalesced subscribe refusal) and by the round-trip and ceiling tests.
- `messages.rs` `ServerMessage::ProtocolError { reason, ts_event }`, classified
  `EventKind::Admission`, whose doc comment enumerates the subscribe
  degradations it carries and states the untargetedness as deliberate. Its
  construction sites are not only `ws.rs`: `http.rs` builds one for the
  unsupported-carrier path, so the narrowing in section 4.1 is to frame-level
  faults ON EITHER CARRIER, not on the WS carrier alone.
- `messages.rs` `ServerMessage` is matched exhaustively in more places than the
  `EventKind` classifier: `admission.rs`'s `truncate_reason` remap match,
  `http.rs`'s `matches!(event, ServerMessage::ProtocolError { .. })` filter, the
  adapter's `client/data.rs` market handler AND `client/exec.rs`'s own
  `ProtocolError` arm plus its `AdmissionSubject::Subscribe` arm. Adding a
  variant and deleting a subject touches all of them.
- Constants: `MAX_SUBSCRIBE_SYMBOLS` (256), `MAX_REFUSED_SYMBOLS_LISTED` (16),
  `MAX_SYMBOL_LEN` (32), `MAX_REASON_LEN` (512), `JSON_ESCAPE_FACTOR` (6),
  `ADMISSION_FRAME_MAX_BYTES` (8192, proven by
  `admission_frames_fit_their_ceiling` against the widest subject, which today
  IS `AdmissionSubject::Subscribe`).
- `validate_symbols` - cardinality plus per-symbol length.
- `sizing.rs` `worst_case_output_bytes`: `Subscribe`/`Unsubscribe` reserve 0
  bytes of the held byte budget, because a subscribe produces no engine output
  and its diagnostics are priority-lane frames drawn from a different pool.
- Tests: `subscribe_start_ts_round_trips_and_legacy_payloads_default` (asserts
  the exact JSON `{"type":"Subscribe","symbols":["X"]}` and legacy decode), plus
  the two `AdmissionSubject::Subscribe` construction sites inside
  `messages.rs`'s own test module (the round-trip and the ceiling proof).

### mogwai-engine

- `lib.rs` `process` matches `ClientMessage::Subscribe { .. } |
  ClientMessage::Unsubscribe { .. } => Vec::new()`. Pattern-only; the engine
  never reads a subscribe's fields and is not otherwise affected.

### mogwai-server

- `ws.rs` `handle_socket`: `replays: HashMap<String, Replay>`, one replay
  thread per symbol per connection.
- `ws.rs` `ClientMessage::Subscribe` arm, in order: `validate_symbols` (frame
  refusal via `send_exec_protocol_error`), `validate_regime_or_clean`,
  `strip_unfireable_reopen_gap` (request-wide diagnostic),
  `reconcile_subscribe_start_ts` (request-wide, two diagnostics: below origin,
  beyond sim-now), then a per-symbol loop over `dedup_symbols(symbols)` doing
  unknown-symbol refusal, `lanes.reserve_promise()`,
  `quiesce_and_resume_floor`, `state.replay_permits.try_acquire_owned()`
  (capacity refusal), `spawn_replay`, `replays.insert`. Refusals accumulate
  into `refused` / `refused_total` / `refused_capacity` and emit ONE
  `AdmissionRejected { subject: AdmissionSubject::Subscribe }` at the end.
- `ws.rs` `reconcile_subscribe_start_ts(start_ts, &state, &lanes)` - the
  request-wide reconciliation. Note precisely what it returns today, because
  section 3.5 turns on it: below the origin it passes `start_ts` through
  UNCHANGED (its comment says so - the generator merely happens to emit its
  first tick at or after the origin, so nothing is actually clamped); beyond
  sim-now it returns `None`, deliberately, so the replay seeks sim-now at THREAD
  START and seeds its pacer there. Neither branch computes a concrete
  "effective start" u64.
- `ws.rs` `ReplaySpawn { symbol, start_ts, regime, speed, gap_cap_ms, profiles,
  sim, data_origin, tx, lanes, diag_ticket, cancel, resume_floor, last_sent_ts,
  permit }`, `spawn_replay`, `spend_diagnostic`, `resume_seek_target`,
  `quiesce_and_resume_floor`, `dedup_symbols`.
- `spend_diagnostic` has TWO call sites, both inside the replay thread: the
  dead-seek diagnostic the promise is nominally reserved for, and the defensive
  unknown-symbol branch where `build_live_source` returns `None` (the guard that
  asserts the handler's pre-filter still holds). It returns `()`, not a
  `Result`: it runs on an OS thread with no path back to the socket owner, so
  every failure is best-effort logging.
- `validate_regime_or_clean` (defined in `http.rs`, used by both carriers)
  SILENTLY converts an out-of-range regime to a clean replay, warning in the
  log only. Nothing about that degradation reaches the wire today.
- `admission.rs`: `ADMISSION_PROMISE_TICKETS = MAX_SUBSCRIBE_SYMBOLS`,
  `ADMISSION_LANE_FRAMES = 64`, `reserve_promise`, `reserve_admission`,
  `emit_admission` (truncates reasons at the lane), `CloseSpec::overload`,
  `CLOSE_ADMISSION_OVERLOAD`. Unchanged in mechanism by this spec; only its
  callers and one test (`the_promise_pool_covers_a_full_subscribe`) are
  touched.
- `http.rs` two match arms refusing `Subscribe`/`Unsubscribe` on the HTTP
  carrier (pattern-only).
- `main.rs` test module: ten `ClientMessage::Subscribe` constructions;
  `tests/daemon.rs`: one; one test sends the raw literal
  `{"type":"Subscribe"}` to prove the undecodable-frame path.
- `main.rs` tests whose ASSERTIONS are about `ProtocolError` for exactly the
  degradations this spec moves onto `SubscriptionIssues`, and which therefore
  change in substance, not merely in constructor:
  `subscribe_below_data_origin_reports_protocol_error_then_streams`,
  `subscribe_beyond_sim_now_clamps_to_a_live_stream`,
  `unknown_symbol_subscribe_reports_protocol_error`,
  `dead_subscribe_reports_protocol_error_instead_of_silence`,
  `coalesced_subscribe_refusal_keeps_the_connection_up`,
  `replay_cap_refuses_subscribe_across_connections`,
  `subscribe_reserves_diagnostic_capacity_before_quiescing`,
  `protocol_error_reasons_are_truncated`,
  `reconcile_subscribe_start_ts_clamps_future_to_live`, and
  `dedup_symbols_sorts_and_dedups` (whose subject leaves the `Subscribe` path).
  Section 6 states what happens to each; the blanket "no assertion changes"
  claim does not hold for this list and never did.

### mogwai-adapter

- `client/data.rs` `SubState { trades, quotes, bars, start_ts }`, the per-symbol
  refcount table under `subs: Arc<Mutex<HashMap<Symbol, SubState>>>`.
- `subscribe_symbol`: seeds `state.start_ts` only on the 0->1 transition (AD7),
  and sends a single-symbol `WsCommand::Subscribe` only when connected (AD5).
- `subscribe_commands(subs, regime)`: the `on_connect` callback, ONE
  `WsCommand::Subscribe` per symbol - the fan-out this spec removes.
- `WsCommand::Subscribe { symbols, start_ts, regime }` and
  `ws_command_to_client_message`.
- `advance_sub_start_ts` - forward-only cursor advance on every delivered
  trade, the source of the per-symbol cursor divergence.
- `handle_market_message`'s `ServerMessage::ProtocolError` arm - warns with the
  venue's reason verbatim, no correlation, no action.
- `PollCursor` and the HTTP polling path: a SEPARATE cursor mechanism that does
  not ride `Subscribe` at all. Out of scope (section 9).
- Tests named in section 7 that assert today's shape.
- `tests/common/mod.rs` - the integration stub matches
  `ClientMessage::Subscribe { .. }` and streams; pattern-only.
- `client/exec.rs` - an exhaustive `ServerMessage` match with its own
  `ProtocolError` arm and an `AdmissionSubject::Subscribe` arm. Both move.

Access path for these APIs, per AGENTS.md: the nautilus and broadarrow surfaces
this crate sits on are READ from the in-tree copies `research/nautilus_trader`
and `research/broadarrow`, and BUILT against the published crates.io nautilus
version pinned in `mogwai-adapter/Cargo.toml` with default features off.
broadarrow is never a build input. Where the vendored snapshot and the pinned
version disagree, the pinned version is what compiles.

### scripts

- `scripts/smoke.py` - twelve `{"type": "Subscribe", ...}` literals, eight of
  them carrying `start_ts` (recounted against the file; an earlier draft of this
  survey said eleven and four, and a miscount here leaves a dead literal after a
  mechanical migration).

### Reconciliation against siblings

No sibling spec is in flight. `docs/shared-bar-aggregator-spec.md` and
`docs/gen-*.md` do not touch the subscribe surface; the bar aggregator consumes
trades downstream of it.

## 3. The open questions, settled

`docs/protocol-problem.md` leaves four decisions to this spec (3.1 to 3.4).
Review of the first draft surfaced five more that the typed per-entry shape
forces and that an implementer cannot resolve on their own (3.5 to 3.9). Each is
settled here with its reason, because an unsettled one is a missing brick.

### 3.1 Generation ids are u64, monotonic per connection - not opaque strings

The problem statement's illustrative shape had `generation_id: String`. It is a
`u64` instead, strictly increasing within a connection, chosen by the client.

- It settles "OLDER versus UNKNOWN" by construction, which an opaque id cannot:
  for a symbol whose current generation the receiver knows, a smaller value is
  OLDER and a larger value is one the receiver has not issued. No retained
  issuance history, no ordered-id convention layered on top of a string.
- It makes the client's uniqueness obligation unfalsifiable rather than merely
  documented. `docs/protocol-problem.md` records that collisions are NOT
  harmless (a reused id makes a stale diagnostic look current); a monotonic
  counter cannot collide, so the failure mode is removed rather than
  documented.
- It keeps the echo bound provable at fixed cost: 20 serialized bytes worst
  case, no `MAX_CLIENT_ID_LEN` cap, no `JSON_ESCAPE_FACTOR` multiplier, no
  truncation rule for an echoed identifier.

The server NEVER interprets a generation beyond comparing it to the one it
recorded for that symbol on that connection, and echoing it. Generation space
is per `(connection, symbol)`; a reconnect is a new connection with fresh
server-side state, so a client counter that simply never resets is conforming.

### 3.2 `regime` moves per entry

`MarketRegime` is already applied per replay thread (`ReplaySpawn.regime`); the
request-wide field is an artifact of the one-`start_ts` shape. With per-entry
resubscribe, a request-wide regime forces a client resubscribing one symbol to
restate a regime for symbols it is not touching, and makes the
`strip_unfireable_reopen_gap` diagnostic un-attributable in exactly the way
this spec exists to fix. Each `SubscriptionRequest` now fully describes one
symbol's stream: symbol, cursor, regime.

### 3.3 `start_ts` stays `Option<u64>`

`None` is load-bearing and means "position at sim-now" - the fix for the
catch-up dump documented on `subscribe_symbol`. It is the value a first
subscribe sends; `Some(cursor)` is what a resume sends. Making it required
would force the adapter to invent a sim-now value it does not authoritatively
know. It stays optional and per entry.

### 3.4 No success-result frames

Successful subscriptions gain NO acknowledgment frame. The decision table in
section 5.4 needs only the client's own record of the last generation it issued
per symbol, which it has by construction at issue time; a success frame would
be a wire frame no rule consumes. It would also cost a priority-lane frame per
subscribe on the healthy path - the lane whose 64-frame budget the coalescing
in section 4.3 exists to protect. The absence is recorded in
`reference/architecture.md` with this reason, so it reads as a decision rather
than an oversight. What would change the answer: a client needing positive
confirmation that a generation became current DURING a tape arrival drought
(`reference/architecture.md`, "Tape arrival droughts"), where the first tick -
today's implicit success signal - can be hours of sim time away. That is a
separate TODO if it ever arises, not deferral of anything agreed here.

### 3.5 The two clamp issues carry different payloads, because only one clamps

The first draft gave both `StartBeforeOrigin` and `StartAfterSimNow` an
`effective_start_ts: u64` and told the adapter to write it into the resume
cursor. Neither value exists today (section 2): below-origin passes `start_ts`
through unchanged, and beyond-sim-now returns `None` precisely so the replay
seeks sim-now at thread start. Reporting a synthesized number the venue does not
use, and then having the client pin a cursor to it, would be a payload the wire
asserts and the venue contradicts. Settled:

- `StartBeforeOrigin { effective_start_ts: u64 }` keeps its payload, and
  `reconcile_entry_start_ts` genuinely CLAMPS: it returns
  `Some(data_origin_ns)`. This is a real behavior change from today's
  pass-through, stated here so it is implemented deliberately. It is
  observationally equivalent for the generator (whose first tick already lands
  at or after the origin) and it makes the reported value true, which is what
  lets the adapter act on it.
- `StartAfterSimNow { sim_now: u64 }` carries the clock reading AT ADMISSION and
  nothing else. The venue returns `None` internally, exactly as today, so the
  replay seeks sim-now on its own thread; the number on the wire is a diagnostic
  observation, not a position the venue promises. The adapter therefore does NOT
  write it into the cursor - the cursor advances forward-only off delivered
  ticks, which is the only value that was ever authoritative. The field name
  says what it is so no later reader mistakes it for an anchor.

### 3.6 Monotonicity is enforced against a high-water map, not against `replays`

Checking the incoming generation against `replays.get(&symbol)` forgets
everything: an `Unsubscribe`, or an entry removed on quiesce-then-refusal, drops
the record, and a LOWER generation is then accepted - which is precisely the
reuse that makes a stale diagnostic look current, the failure section 3.1 claims
to have removed. The server therefore keeps

```rust
generations: HashMap<Symbol, u64>, // per connection, per socket task
```

a high-water mark written for EVERY entry that names a listed symbol, whatever
the entry's outcome (accepted, capacity-refused, seek-dead), and never removed
for the life of the connection. `Replay.generation` still exists, because the
replay thread must carry the generation its asynchronous diagnostic names, but
it is no longer the authority for the monotonicity check. The map is bounded by
the venue's instrument count because the unknown-symbol check runs FIRST (see
the reordered flow in section 4.2), so an unlisted symbol never creates an
entry.

Section 3.1's claim narrows accordingly and is restated honestly: the ADAPTER's
counter cannot collide by construction; the SERVER enforces the rule against any
client, and this map is what makes that enforcement complete rather than
replay-lifetime-scoped.

Exhaustion: the counter is `u64` and monotone. At one generation per
subscribe-entry it cannot wrap in any run of this venue - a wrap needs 1.8e19
subscribe entries. The allocator does not handle wrap and does not need to; it
is named here so the omission is a decision rather than an oversight.

### 3.7 An invalid regime is a reported degradation

`validate_regime_or_clean` today drops an out-of-range regime and streams the
clean tape, telling the client nothing. That directly contradicts what
`SubscriptionIssues` promises - per-entry truth about every subscription not
served exactly as asked. A `SubscriptionIssue::InvalidRegime` DEGRADED variant
is added and the validator's caller reports it. It is a degradation and not a
refusal: the stream runs, unhavocked. The alternative (refusing the entry) would
change behavior clients rely on today for a fault that is already survivable.

### 3.8 The 16-entry cap is lossy, and refusals fill it first

`entries` is capped at `MAX_SUBSCRIPTION_ISSUES_LISTED` while `issues_total`
carries the true count, so a subscribe that produces more than 16 outcomes drops
the surplus and those entries get no `(symbol, generation, issue)` at all. That
is a genuine attribution hole, not a formatting detail: it can silently swallow
the news that a feed is dead. It is nonetheless kept, because the cap is what
makes `ADMISSION_FRAME_MAX_BYTES` provable and what keeps a 256-entry subscribe
to ONE priority frame, and both of those are load-bearing (section 4.2). Two
things pay for keeping it:

- Ordering: `entries` is filled with REFUSALS first (`is_refusal()` true), then
  degradations, so the outcomes that kill a feed are the ones that survive
  truncation. A degradation lost to the cap costs a log line; a refusal lost to
  the cap costs a feed.
- Counting: the frame carries `issues_total` AND `refusals_total`, so a client
  that sees `refusals_total > entries.iter().filter(is_refusal).count()` knows
  attributably that some feed it asked for is dead without knowing which, and
  can resubscribe the set rather than guess.

Rejected as the remedy: pagination or a larger frame. Pagination puts multi-frame
sequencing state on the priority lane the coalescing exists to protect, and a
larger frame re-opens a ceiling that is currently proven. Revisit only if a real
client is observed hitting the cap with refusals.

### 3.9 The symbol/generation cross-check needs issuance history on the client

`docs/protocol-problem.md` requires that an echoed symbol be checked against the
generation's recorded symbol, and that a mismatch be a protocol inconsistency
rather than log decoration. The first draft claimed this "falls out" of the
per-symbol comparison. It does not. Counterexample: BTC was issued generation 2
and ETH generation 3; a venue outcome naming `(ETH, 2)` is classified by the
per-symbol rule as an old ETH generation and discarded at `debug`, even though
generation 2 was never ETH's. The mismatch is invisible.

The adapter therefore keeps, per connection, a bounded issuance history:

```rust
/// Every generation this client has issued, mapped to the symbol it was issued
/// for. Consulted only to catch a venue naming a `(symbol, generation)` pair
/// this client never issued - the mismatch `docs/protocol-problem.md` requires
/// be treated as a protocol inconsistency. Pruned from the front once it
/// exceeds `GENERATION_HISTORY` entries; a generation below the retained floor
/// is unclassifiable and is discarded at `debug`, which is the same action the
/// superseded case takes, so the pruning costs no diagnosis.
issued: BTreeMap<u64, Symbol>,
```

`GENERATION_HISTORY` is `4 * MAX_SUBSCRIBE_SYMBOLS` (1024) - four full
reconnect rebuilds of a maximal subscription set, which comfortably outlives any
in-flight diagnostic from a dropped socket. The map is `BTreeMap` so pruning is
a `split_off` on the floor, not a scan.

## 4. Target artifacts

### 4.1 `mogwai-protocol` wire types

```rust
/// One symbol's subscription within a `Subscribe`. Self-contained: identity,
/// cursor and regime all per entry, so a client may resubscribe one symbol
/// without restating any other's state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionRequest {
    /// Client-chosen identity for THIS generation of this symbol's stream,
    /// strictly increasing within a connection. The venue never interprets it
    /// beyond ordering it against the generation it recorded for the symbol,
    /// and echoing it on every diagnostic about this subscription. Ordering is
    /// what lets a client tell a diagnostic about a superseded generation from
    /// one about a generation it never issued.
    pub generation: u64,
    pub symbol: Symbol,
    /// Replay from this unix-nanosecond instant forward. `None` positions the
    /// stream at sim-now (a live subscribe); `Some` is a historical window or
    /// a resume cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_ts: Option<u64>,
    /// Generator-level market regime for THIS symbol's stream only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regime: Option<MarketRegime>,
}

// in ClientMessage
Subscribe {
    subscriptions: Vec<SubscriptionRequest>,
},
```

`Unsubscribe { symbols: Vec<Symbol> }` is unchanged: an unsubscribe names no
generation because it targets whatever generation is current, and a stale
unsubscribe naming an old generation would be ambiguous rather than useful.

```rust
/// What the venue did to one requested subscription that it did not serve
/// exactly as asked. A closed set rather than prose: the client's handling is a
/// match, not a string search, and a fixed set is what keeps
/// `ADMISSION_FRAME_MAX_BYTES` provable now that issues are per entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SubscriptionIssue {
    /// REFUSED: the venue does not list this symbol. Nothing streams.
    UnknownSymbol,
    /// REFUSED: the global replay-thread pool is exhausted. Nothing streams;
    /// running streams are untouched.
    ReplayCapacity,
    /// REFUSED: `generation` is not strictly greater than the generation the
    /// venue currently records for this symbol on this connection. The live
    /// stream is untouched - this is a client-ordering fault, and destroying a
    /// healthy replay over one would be a worse answer than refusing it.
    StaleGeneration { current: u64 },
    /// REFUSED: the positioning seek exhausted its tick budget, so the stream
    /// could not be placed. Discovered asynchronously on the replay thread,
    /// which is why this issue in particular must carry a generation.
    SeekBudgetExhausted,
    /// DEGRADED: `start_ts` preceded the tape origin; the venue clamped the
    /// request to `effective_start_ts` (the tape origin) and the stream runs
    /// from there. This value IS the position the venue used, so a client may
    /// safely adopt it as a cursor (section 3.5).
    StartBeforeOrigin { effective_start_ts: u64 },
    /// DEGRADED: `start_ts` exceeded sim-now; the venue discarded the requested
    /// start and the stream is live from the clock. `sim_now` is the clock
    /// reading at admission - an OBSERVATION, not a position the venue
    /// promises, because the replay seeks sim-now again on its own thread. A
    /// client must not adopt it as a cursor (section 3.5).
    StartAfterSimNow { sim_now: u64 },
    /// DEGRADED: the entry's `regime` failed `validate_market_regime` and was
    /// dropped; the clean, unhavocked tape streams (section 3.7).
    InvalidRegime,
    /// DEGRADED: the entry's `ReopenGap` is anchored at or before the tape
    /// origin and can never fire; it was stripped and the clean tape streams.
    ReopenGapUnfireable { at_ts: u64 },
}

impl SubscriptionIssue {
    /// `true` when nothing streams for this entry; `false` when the stream
    /// runs, altered. The one bit a client needs to decide whether to keep
    /// waiting for data.
    #[must_use]
    pub fn is_refusal(self) -> bool { /* Unknown/Capacity/Stale/Seek */ }
}

/// One entry's outcome on a `SubscriptionIssues` frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubscriptionOutcome {
    pub generation: u64,
    pub symbol: Symbol,
    pub issue: SubscriptionIssue,
}

// in ServerMessage
/// Per-entry truth about a `Subscribe` the venue could not serve exactly as
/// asked. COALESCED: one frame per `Subscribe` for every synchronously
/// discovered issue, so a 256-entry subscribe of unknown symbols costs one
/// priority frame, not 256. The asynchronous `SeekBudgetExhausted` arrives
/// later in its own single-entry frame, paid for by the replay's promise
/// ticket.
///
/// `entries` lists at most `MAX_SUBSCRIPTION_ISSUES_LISTED`, REFUSALS FIRST;
/// `issues_total` is the true count and `refusals_total` the count of those
/// that killed a feed, so a client can tell "some feed I asked for is dead but
/// was truncated away" from "everything listed is what happened" (section 3.8).
/// Classifies `EventKind::Admission`, rides the priority lane, exempt from
/// `DelayAcks` - it reports what request handling did, never engine output.
///
/// One entry may produce more than one outcome (a clamped start on an entry
/// whose regime was also dropped), so `issues_total >= entries.len()` says
/// nothing about how many ENTRIES were affected.
SubscriptionIssues {
    entries: Vec<SubscriptionOutcome>,
    issues_total: usize,
    refusals_total: usize,
    ts_event: u64,
},
```

Deleted: `AdmissionSubject::Subscribe { symbols, refused_total }`. Its only
construction site moves to `SubscriptionIssues`, and the move is a correctness
fix, not just a relocation: `AdmissionRejected`'s own doc comment says it
"reports what the TRANSPORT refused to admit, never was engine output". An
unknown symbol is not a transport refusal - it is a venue-semantic answer that
was mis-homed on the admission variant because no better one existed. After
this change `AdmissionRejected` has no subscribe-shaped subject at all, which
is correct: a subscribe whose diagnostic capacity cannot be reserved closes the
connection (`CloseSpec::overload`) rather than emitting a refusal it has no
room for, exactly as today.

`ProtocolError` survives, narrowed to FRAME-level failures on EITHER CARRIER
that cannot be attributed to any entry: an undecodable frame is already
`AdmissionRejected { subject: Frame }`, so `ProtocolError`'s remaining users are
the `validate_subscriptions` / `validate_symbols` boundary refusals of a whole
`Subscribe`/`Unsubscribe`, plus `http.rs`'s unsupported-carrier refusal, which
is frame-level by the same logic and is not touched. Its doc comment is
rewritten (section 8): the
untargetedness paragraph inverts for everything per-entry and stays only for
whole-frame faults, where there is genuinely no entry to name.

Constants:

- `MAX_SUBSCRIBE_SYMBOLS` (256) keeps its name and value and now caps
  `subscriptions.len()`.
- `MAX_REFUSED_SYMBOLS_LISTED` is renamed `MAX_SUBSCRIPTION_ISSUES_LISTED`,
  value 16, and caps `SubscriptionIssues.entries.len()`.
- `ADMISSION_FRAME_MAX_BYTES` stays 8192. Re-proof: the widest Admission frame
  is now `SubscriptionIssues` with 16 entries, each entry being a symbol
  charged at `JSON_ESCAPE_FACTOR * MAX_SYMBOL_LEN` = 192 bytes, two u64s at 20
  bytes each, an issue tag under 32 bytes and per-entry JSON scaffolding under
  80 bytes - under 350 bytes per entry, so under 5600 for the list, plus
  `issues_total`, `refusals_total`, `ts_event` and the envelope, comfortably
  under 8192. The
  previous widest case (`AdmissionSubject::Subscribe` at 6 * (16 * 32 + 512) =
  6144) is deleted along with the variant, and `AdmissionRejected`'s remaining
  widest subject is `Query`/`Frame` with a `MAX_REASON_LEN` reason at 6 * 512 =
  3072 plus scaffolding. The constant is unchanged; the proof test gains the
  new arm, and the constant's OWN doc comment - which today derives 6144
  explicitly from `AdmissionSubject::Subscribe` and `MAX_REFUSED_SYMBOLS_LISTED`
  - is rewritten to this derivation, or it is left describing a type that no
  longer exists (section 8).

`admission.rs`'s `truncate_reason` remap match gains a deliberate pass-through
arm for `SubscriptionIssues`: the frame carries no free-text `reason`, so there
is nothing to truncate, and the whole point of that function is that nothing
untruncated reaches the lane - an implicit `_ =>` would make the next
reason-carrying variant silently untruncated. State the "no reason field, by
design" argument in the arm's comment.

New validator, replacing the `Subscribe` use of `validate_symbols` (which stays
for `Unsubscribe`):

```rust
/// Boundary guard for a `Subscribe`'s entry list. Cardinality, per-symbol
/// length, and DUPLICATE SYMBOLS - two entries naming one symbol with
/// different generations have no defined meaning (which cursor wins, which
/// generation is current), so the whole frame is refused rather than silently
/// deduplicated. `dedup_symbols` was the old answer and it silently discarded a
/// cursor; a refusal is the honest one.
pub fn validate_subscriptions(subs: &[SubscriptionRequest]) -> Result<(), &'static str>;
```

Errors, in this order: `"subscriptions exceeds MAX_SUBSCRIBE_SYMBOLS"`,
`"symbol exceeds MAX_SYMBOL_LEN"`, `"subscriptions names a symbol twice"`.

`sizing.rs` keeps `Subscribe { .. } | Unsubscribe { .. } => 0` with its comment
updated: a subscribe still produces no engine output, and `SubscriptionIssues`
is a priority-lane frame drawn from the frame/promise pools, not the held byte
budget.

### 4.2 `mogwai-server`

`Replay` gains the generation that created it:

```rust
pub(crate) struct Replay {
    pub(crate) cancel: Arc<AtomicBool>,
    pub(crate) handle: JoinHandle<()>,
    pub(crate) last_sent_ts: Arc<AtomicU64>,
    /// The generation whose `SubscriptionRequest` spawned this stream. Read
    /// on the next `Subscribe` for the monotonicity check, and carried into
    /// the thread so the asynchronous dead-seek diagnostic names the
    /// generation it actually describes rather than whatever is current by
    /// the time it is discovered.
    pub(crate) generation: u64,
}
```

`handle_socket` gains, alongside `replays`, the connection-lifetime high-water
map settled in 3.6:

```rust
/// Highest generation this connection has ever named for each listed symbol,
/// whatever became of that entry. Independent of `replays` on purpose: an
/// `Unsubscribe`, or a quiesce followed by a capacity refusal, removes the
/// replay, and enforcing monotonicity off `replays` would then accept a REUSED
/// generation and let a delayed diagnostic about an old stream look current.
/// Bounded by the venue's instrument list because the unknown-symbol check runs
/// first, so an unlisted symbol never lands here.
generations: HashMap<String, u64>,
```

`ReplaySpawn` gains `generation: u64`. `spend_diagnostic` changes its payload
from a reason string to a typed issue and keeps its return type:

```rust
fn spend_diagnostic(
    lanes: &ExecLanes,
    ticket: &mut Option<Ticket>,
    generation: u64,
    symbol: &str,
    issue: SubscriptionIssue,
    ts_event: u64,
)
```

and emits a single-entry `SubscriptionIssues { entries: vec![outcome],
issues_total: 1, refusals_total: 1, ts_event }`.

It returns `()`, NOT `Result<(), CloseSpec>`. It runs on the replay OS thread,
`Replay` is a `JoinHandle<()>`, and there is no channel by which a close
decision could reach the socket owner; today's behavior is best-effort logging
on a full or closed lane and this spec does not change it. Making the signature
result-bearing would promise a close nothing can execute. Both of its call sites
are typed: the dead-seek branch passes `SeekBudgetExhausted`, and the defensive
`build_live_source` returned-`None` branch passes `UnknownSymbol` (that branch
is unreachable in production because the handler pre-filters, and its
`debug_assert!` on the unspent ticket stays).

`reconcile_subscribe_start_ts` is rewritten from an emitting function to a pure
one, because it now runs per entry and must contribute to a coalesced frame
rather than send its own:

```rust
/// Reconcile one entry's `start_ts` against the tape bounds. Returns the
/// effective start plus the issue to report, if any. Pure - the caller
/// accumulates issues and emits ONE frame per `Subscribe`.
pub(crate) fn reconcile_entry_start_ts(
    start_ts: Option<u64>,
    data_origin_ns: u64,
    sim_now: u64,
) -> (Option<u64>, Option<SubscriptionIssue>);
```

The two existing `tracing::warn!` lines move to the call site, one per entry,
keeping their current fields plus `generation`.

The `Subscribe` arm becomes, in order:

1. `validate_subscriptions(&subscriptions)`; on error a frame-level
   `ProtocolError` (unchanged mechanism, `send_exec_protocol_error`) and
   `continue`.
2. `let mut issues: Vec<SubscriptionOutcome> = Vec::new();` plus
   `issues_total: usize`, `refusals_total: usize` and
   `overload_close: Option<CloseSpec>`.
3. For each entry, in the order given:
   - Unknown symbol FIRST: `state.profiles.get(&symbol).is_none()` - push
     `UnknownSymbol`, `continue`. Before any quiesce, reservation or generation
     bookkeeping, as today, and ahead of the monotonicity check so an unlisted
     symbol can never create an entry in `generations` (the boundedness argument
     in 3.6).
   - Monotonicity: if `generations.get(&entry.symbol)` exists and is
     `>= entry.generation`, push `StaleGeneration { current }`, leave any
     running replay alone, `continue`. No recorded generation means any
     generation is acceptable, so a first subscribe on a fresh connection never
     trips this. Otherwise record `generations.insert(symbol,
     entry.generation)` IMMEDIATELY - before the outcome of this entry is
     known - so a capacity refusal or a dead seek still burns the generation
     and a later reuse of it is refused.
   - `validate_regime_or_clean(entry.regime)`: it now reports whether it
     dropped anything (return `(Option<MarketRegime>, bool)`, or an
     `Option<SubscriptionIssue>` alongside, at the implementer's discretion -
     `http.rs`'s existing caller ignores the new half). If it dropped an
     out-of-range regime, push `InvalidRegime` and CONTINUE PROCESSING (3.7).
   - `strip_unfireable_reopen_gap`; if stripped, push `ReopenGapUnfireable {
     at_ts }` and CONTINUE PROCESSING this entry (it is a degradation, and the
     stream still starts).
   - `reconcile_entry_start_ts`; on `Some(issue)` push it and keep the
     effective start. Also a degradation; the entry proceeds.
   - `lanes.reserve_promise()`; on `None`, `overload_close =
     Some(CloseSpec::overload("subscribe diagnostic capacity exhausted"))` and
     `break`. Before the quiesce, as today, for the reason the current comment
     gives.
   - `quiesce_and_resume_floor` for any existing replay, then
     `replay_permits.try_acquire_owned()`; on failure push `ReplayCapacity` and
     `continue`. NOTE the ordering hazard preserved from today: the quiesce
     already destroyed the old stream by the time the permit is refused, and it
     released the old permit as it joined, so the acquire that follows a
     same-symbol quiesce reclaims it and cannot fail for that symbol.
   - `spawn_replay` with the entry's `start_ts`, `regime` and `generation`;
     `replays.insert(symbol, Replay { .., generation })`.
   - An entry that produced no issue pushes nothing.
4. If `issues_total > 0`, one `SubscriptionIssues` frame via
   `lanes.emit_admission`, with `entries` truncated to
   `MAX_SUBSCRIPTION_ISSUES_LISTED`. Truncation is REFUSALS FIRST (3.8), which
   the `if entries.len() < CAP` guard the refusal list uses today cannot do on
   its own: accumulate refusals and degradations in two vecs, then build
   `entries` as refusals followed by degradations, capped. `issues_total` and
   `refusals_total` count everything, truncated or not.
5. `if let Some(close) = overload_close { break Some(close) }`.

Note what this deletes: the request-wide `regime`/`start_ts` locals, the two
`send_exec_protocol_error` calls for the clamps, the `send_exec_protocol_error`
call for the ReopenGap strip, `dedup_symbols` on the `Subscribe` path (it stays
for `Unsubscribe`), and the `refused` / `refused_capacity` / `unknown` counters
whose only purpose was composing prose for the deleted subject.

Per-`Subscribe` priority-lane cost is unchanged at one frame in the degraded
case and zero in the healthy case, so `ADMISSION_LANE_FRAMES` and
`ADMISSION_PROMISE_TICKETS` keep their values. This is the load-bearing reason
issues are coalesced and typed rather than emitted per entry as prose: a
256-entry subscribe with a below-origin `start_ts` would otherwise emit 256
frames into a 64-frame lane and deterministically close a connection that S22a
promises stays up.

### 4.3 `mogwai-adapter`

```rust
struct SubState {
    trades: usize,
    quotes: usize,
    bars: usize,
    start_ts: Option<u64>,
    /// The generation of the `SubscriptionRequest` most recently SENT for this
    /// symbol. Stamped at send time on both paths (the live 0->1 subscribe and
    /// the `on_connect` rebuild), never on receipt: the client's own record of
    /// what it issued is the whole decision table, which is why no success
    /// frame is needed (section 3.4).
    generation: u64,
}
```

`MogwaiDataClient` gains `generation_seq: Arc<AtomicU64>`, seeded at 0 and never
reset - including across reconnects, which is conforming (a strictly increasing
sequence trivially satisfies a per-connection monotonicity rule) and avoids a
reset racing in-flight frames from the previous socket. With the `+ 1` below the
first issued generation is 1, so generation 0 is never issued and is a usable
"never sent" sentinel for `SubState::default()`. (Seed and sentinel must agree:
seeding at 1 with the same `+ 1` would start at 2 and leave both 0 and 1
meaning "never sent".)

```rust
/// Allocate the next generation. One counter, one name: the field is
/// `generation_seq`, this is the only allocator, and the free-function form
/// below takes `&AtomicU64` so `subscribe_commands` stays testable without a
/// client.
fn next_generation(seq: &AtomicU64) -> u64 { seq.fetch_add(1, Relaxed) + 1 }
```

Also gains the bounded issuance history settled in 3.9, `issued: BTreeMap<u64,
Symbol>`, written under the same lock as `subs` (or inside it) at every stamp
and pruned with `split_off` once it exceeds `GENERATION_HISTORY`.

`WsCommand::Subscribe` becomes `Subscribe { subscriptions:
Vec<SubscriptionRequest> }`; `ws_command_to_client_message` maps it straight
through. `data_regime(&self.config.havoc)` is now evaluated per entry (same
value for every entry today; the per-entry field is what makes a future
per-symbol regime expressible).

`subscribe_commands` collapses to ONE command:

```rust
/// One frame carrying every live symbol's own cursor, regime and a fresh
/// generation. Replaces the per-symbol fan-out: N frames and up to N
/// diagnostics per reconnect became one and one.
fn subscribe_commands(
    subs: &Arc<Mutex<HashMap<Symbol, SubState>>>,
    regime: Option<MarketRegime>,
    generation_seq: &AtomicU64,
) -> Vec<WsCommand>
```

It returns `vec![]` when nothing is subscribed (today it returns an empty vec
by construction; the collapsed version must not send an empty `Subscribe`),
otherwise a single-element vec. It stamps each entry's fresh generation into
the `SubState` under the same lock it reads the cursor under, so the record and
the frame cannot disagree. Entries are emitted in sorted symbol order so the
frame is deterministic and testable.

A subscription set larger than `MAX_SUBSCRIBE_SYMBOLS` is chunked into
successive `WsCommand::Subscribe` frames of at most `MAX_SUBSCRIBE_SYMBOLS`
entries each - the one case where more than one frame is correct, and it is
bounded by the server's own documented cap rather than by the old fan-out.

What chunking does NOT buy: more than `MAX_SUBSCRIBE_SYMBOLS` LIVE symbols on
one connection. `ADMISSION_PROMISE_TICKETS == MAX_SUBSCRIBE_SYMBOLS` (256) and
every live replay holds a promise for its whole life, so the 257th live
subscription trips `reserve_promise() -> None` and `CloseSpec::overload` -
deterministically, and identically under today's fan-out, so this spec neither
creates nor fixes it. Chunking exists only so that a set which happens to be
delivered in one call does not produce an over-cap FRAME. The gate in section 6
therefore asserts the chunking at the `subscribe_commands` level (a pure
function over the table) and does NOT drive 257 live symbols through a server,
which would prove the close rather than the chunking. Lifting the live ceiling
means repricing the promise pool, which is out of scope (section 9).

`subscribe_symbol` keeps AD5 (send only when connected) and AD7 (only the 0->1
subscriber seeds the cursor), and now stamps a fresh generation before sending
its single-entry `Subscribe`.

Stamping under the `subs` lock makes the TABLE and the frame agree; it does not
order the SENDS, because `subscribe_symbol` sends outside the lock exactly as it
does today. So a 0->1 subscribe holding G can be overtaken on the wire by an
`on_connect` rebuild holding G' > G, and the server then refuses the G entry as
stale. The outcome is benign and is exercised by the decision table below: the
adapter discards the `StaleGeneration` outcome at the `<` arm, and the symbol
streams at G' with the cursor the rebuild carried, which is the more recent one.
This is stated rather than left implied, so no reader concludes the lock closed
a race it does not close.

Diagnostic handling. `handle_market_message` gains a `SubscriptionIssues` arm
and the `ProtocolError` arm keeps its verbatim-reason warn for frame-level
faults. The decision table, per entry:

Every arm first consults `issued` (3.9): if `entry.generation` is at or above the
retained floor and maps to a symbol OTHER than `entry.symbol`, the outcome is a
`(symbol, generation)` pair this client never issued - `warn` as a protocol
inconsistency and take no state action, whatever the rest of the table would
say. Below the floor the pair is unclassifiable and is discarded at `debug`,
which is what the superseded arm would have done anyway.

| Condition | Action |
| --- | --- |
| `(symbol, generation)` contradicts `issued` | `warn` "venue named a subscription generation this client issued for another symbol" - protocol inconsistency, no state change |
| symbol absent from `subs` | discard silently at `debug` - no longer subscribed |
| `entry.generation < state.generation` | discard at `debug` - superseded generation, the exact frame the old wire could not classify |
| `entry.generation == state.generation` | act (below) |
| `entry.generation > state.generation` | `warn` "venue named a subscription generation this client never issued" - a protocol inconsistency, never silently applied |

Acting on a current-generation entry:

- `issue.is_refusal()` - `warn` naming symbol, generation and issue; the
  symbol's feed is dead until something resubscribes. No cursor change. The
  adapter does NOT itself resubscribe - see the non-goal in section 9.
- `StartBeforeOrigin { effective_start_ts }` - `warn`, and set this symbol's
  cursor to `effective_start_ts` so the NEXT reconnect resumes where the venue
  actually positioned rather than re-requesting a start the venue already told
  us it cannot honor. This is safe ONLY because 3.5 makes the venue genuinely
  clamp to that value. Written through the same forward-only rule
  `advance_sub_start_ts` uses, so a clamp can never rewind a cursor that
  delivered ticks in the interim.
- `StartAfterSimNow { sim_now }` - `warn` and NOTHING ELSE. `sim_now` is an
  observation, not the position the replay took (3.5); the cursor is advanced by
  delivered ticks, which is the only authoritative source. Adopting `sim_now` as
  a cursor would pin the resume to a stale clock snapshot.
- `ReopenGapUnfireable { at_ts }` / `InvalidRegime` - `warn`; the stream runs,
  clean or unhavocked, no state change.

The symbol/generation cross-check `docs/protocol-problem.md` requires ("the
echoed symbol is checked against the generation's recorded symbol, and a
mismatch is a protocol inconsistency, not log decoration") is the `issued`
consultation above, NOT a consequence of the per-symbol comparisons. 3.9 gives
the counterexample that killed the "it falls out" claim; the code comment should
carry that counterexample so a later reader does not delete `issued` as
redundant.

## 4.4 Data flow, end to end

1. nautilus calls `subscribe_trades`; `subscribe_symbol` takes the lock, sees
   the 0->1 transition, seeds `start_ts`, allocates generation G, writes it to
   `SubState`, and (if connected) sends `Subscribe { subscriptions: [{ G, sym,
   start_ts, regime }] }`.
2. The server validates the frame, checks G against the recorded generation for
   `sym`, spawns the replay with G, and records `Replay { generation: G }`.
3. Ticks flow; `emit_trade` advances the symbol's cursor forward-only.
4. The socket drops. `on_connect` fires; `subscribe_commands` takes the lock,
   allocates G' > G for every live symbol, writes them, and sends ONE
   `Subscribe` carrying every symbol's own advanced cursor.
5. A diagnostic about G arrives after the reconnect (a dead seek from the old
   connection's replay thread, in flight when the socket dropped). The adapter
   compares G against the recorded G' and discards it. This is the frame the
   whole redesign exists to make discardable.

## 5. Landing sequence

Two landings. Each is one coherent intrusive change, leaves the tree green, and
is kept or reverted whole on its gates.

### L1 - the wire reshape, both ends, no fan-out change

Everything in 4.1 and 4.2, plus the mechanical adapter adaptation: `SubState`
gains `generation`, the client gains `generation_seq` and `issued`,
`subscribe_symbol` stamps them, `subscribe_commands` still emits one command PER
SYMBOL (each now a one-entry `subscriptions` vec), and `handle_market_message`
gains the full `SubscriptionIssues` decision table from 4.3. The non-obvious
compile-breaks the first draft missed also land here: `client/exec.rs`'s
`ServerMessage` match (its `ProtocolError` arm stays, its
`AdmissionSubject::Subscribe` arm goes, and it gains an arm for the new variant
- exec has nothing to do with subscriptions, so that arm is an explicit
ignore with a comment saying why), `admission.rs`'s `truncate_reason` remap arm,
and `http.rs`'s `ProtocolError` filter. Docs from section 8 land here. Every
test and `scripts/smoke.py` literal in the survey is rewritten to the new shape.

Why the fan-out survives L1: it isolates the wire break from the reconnect
behavior change, so a red gate in L1 is a serialization or server-arm fault and
a red gate in L2 is a reconnect fault. The decision table lands in L1 rather
than L2 because L1 is where diagnostics start carrying generations, and shipping
a field nothing reads is exactly the unproven brick this contract forbids.

Note there is no compatibility shim and no legacy decode path. The existing
`subscribe_start_ts_round_trips_and_legacy_payloads_default` test asserts that
`{"type":"Subscribe","symbols":["X"]}` decodes; that assertion is DELETED, not
migrated. Both ends of this wire live in this workspace, the venue is a test
instrument rather than a deployed exchange, and a silently-accepted old-shape
frame would subscribe with generation 0 forever - the collision case section
3.1 exists to make impossible.

### L2 - the collapsed reconnect frame

`subscribe_commands` returns one command (chunked at
`MAX_SUBSCRIBE_SYMBOLS`), sorted, stamping generations under the lock.
Nothing in `mogwai-protocol` or `mogwai-server` changes; L2 is adapter-only,
which is possible only because L1 already made the server accept a multi-entry
frame with per-entry cursors.

Ordering constraint: L2 cannot precede L1 (the wire cannot carry per-symbol
cursors yet), and L1 does not depend on L2. There is no third ordering.

## 6. Gates

Copy-pasteable, per brick.

### L1

- Wire protocol, serde round trips (the `mogwai-protocol` gate this contract
  names for wire changes):

  ```
  brokkr test -p mogwai-protocol subscribe_round_trips_per_entry_generations
  brokkr test -p mogwai-protocol admission_frames_fit_their_ceiling
  ```

  `subscribe_round_trips_per_entry_generations` is NEW, replacing
  `subscribe_start_ts_round_trips_and_legacy_payloads_default`. It pins the
  exact serialized bytes of a two-entry `Subscribe` (one entry with neither
  `start_ts` nor `regime`, proving both still skip-serialize; one with both),
  round-trips it, and round-trips a `SubscriptionIssues` frame carrying one
  entry of each `SubscriptionIssue` variant (all eight, `InvalidRegime`
  included) so every tag and payload field is exercised in both directions.

  `admission_frames_fit_their_ceiling` is EXTENDED, not re-blessed: its
  existing arms keep asserting `<= ADMISSION_FRAME_MAX_BYTES`, the
  `AdmissionSubject::Subscribe` arm is deleted with the variant, and a new arm
  serializes the widest `SubscriptionIssues` - `MAX_SUBSCRIPTION_ISSUES_LISTED`
  entries, each with a `MAX_SYMBOL_LEN` symbol of a character that escapes to
  six bytes, `u64::MAX` generation and `u64::MAX` issue payload - and asserts
  the same ceiling. The constant does not move, so there is no re-bless.

- Server subscribe semantics, NEW tests in `mogwai-server`'s test module (the
  behavior neither the engine tests nor the smoke test reach):

  ```
  brokkr test -p mogwai-server subscribe_issue --debug
  ```

  matching, as a substring filter, these four:

  - `subscribe_issues_coalesce_into_one_frame` - one `Subscribe` naming 20
    unknown symbols yields exactly ONE `SubscriptionIssues` with
    `issues_total == 20`, `refusals_total == 20` and
    `entries.len() == MAX_SUBSCRIPTION_ISSUES_LISTED`, and the connection stays
    open (the S22a promise). This test blesses the truncation, deliberately and
    with 3.8's argument attached; it is not evidence the cap is harmless.
  - `subscribe_issues_list_refusals_before_degradations` - a `Subscribe` mixing
    20 below-origin degradations with 3 unknown symbols lists all 3 refusals in
    `entries` despite the cap, with `refusals_total == 3` and
    `issues_total == 23`. This is the assertion 3.8 pays for the cap with.
  - `subscribe_issue_reports_an_out_of_range_regime` - an entry whose regime
    fails validation yields `InvalidRegime` AND still streams trades (3.7).
  - `subscribe_issue_names_its_generation_and_symbol` - a below-origin
    `start_ts` on entry two of a three-entry subscribe produces one outcome
    carrying entry two's generation and symbol and
    `StartBeforeOrigin { effective_start_ts: data_origin_ns }`, and entries one
    and three stream normally.
  - `subscribe_issue_refuses_a_stale_generation_without_killing_the_stream` -
    subscribe G=5, then G=3 for the same symbol; the reply carries
    `StaleGeneration { current: 5 }` and the G=5 replay keeps delivering
    trades afterwards (the witness is a trade, for the reason
    `docs/protocol-problem.md` gives about trade witnesses).
  - `subscribe_issue_refuses_a_duplicated_symbol_frame_wide` - two entries
    naming one symbol yields a frame-level `ProtocolError` and NO replay is
    spawned for either.
  - `subscribe_issue_remembers_a_generation_across_unsubscribe` - subscribe
    G=5, `Unsubscribe`, then subscribe G=4 for the same symbol; the reply
    carries `StaleGeneration { current: 5 }`. This is the test that proves the
    high-water map of 3.6 exists at all - against `replays` alone the G=4 entry
    is accepted and the whole reuse hazard is back.

  Debug profile per the `brokkr test --debug` guidance: these are
  socket-lifecycle tests where release LTO dominates wall time.

- Per-entry positioning, NEW:

  ```
  brokkr test -p mogwai-server per_entry_start_ts_positions_each_symbol --debug
  ```

  Two symbols in ONE `Subscribe` with different `start_ts` values; each
  symbol's first delivered trade is at or after its own requested start and
  before the other's. This is the assertion that the wire change actually
  bought resume precision, and no existing test can make it.

- Adapter translation, NEW in `client/data.rs`'s test module:

  ```
  brokkr test -p mogwai-adapter subscription_issue
  ```

  - `subscription_issue_for_a_superseded_generation_is_discarded` - table entry
    two: a `SubscriptionIssues` naming generation G after the table holds G' >
    G leaves the `SubState` untouched.
  - `subscription_issue_for_the_current_generation_moves_the_cursor` - a
    `StartBeforeOrigin { effective_start_ts }` on the current generation sets
    the symbol's cursor to `effective_start_ts`, and a second one carrying a
    SMALLER effective start does not rewind it.
  - `subscription_issue_start_after_sim_now_leaves_the_cursor_alone` - a
    `StartAfterSimNow { sim_now }` on the current generation warns and changes
    no `SubState` field. The negative half of 3.5, and the reason the two clamp
    issues have different payload names.
  - `subscription_issue_for_a_foreign_generation_is_a_protocol_inconsistency` -
    with BTC at generation 2 and ETH at 3, an outcome naming `(ETH, 2)` is
    NOT treated as a superseded ETH generation: it hits the `issued` mismatch
    arm and changes nothing. Without `issued` this test cannot pass, which is
    the point (3.9).

- Existing suites:

  ```
  brokkr check
  ```

  These split into two groups, and the first draft's blanket "none of their
  assertions change" claim covered only the first. Stating the split is what
  keeps the re-bless honest.

  Constructor-only, assertions untouched, so any behavioral diff is a real
  regression: `main.rs`'s ten `Subscribe` constructions, `daemon.rs`,
  `tests/common/mod.rs`, the adapter's
  `subscribe_variants_emit_subscribe_then_refcount_suppresses`,
  `subscribe_command_carries_data_regime`,
  `subscribe_while_disconnected_defers_to_on_connect`,
  `later_subscriber_does_not_pull_start_ts_backward`, and the server's
  `the_promise_pool_covers_a_full_subscribe`.

  ASSERTION CHANGES, each a deliberate re-bless with a stated replacement -
  this is the largest single body of work in L1 and the survey now names it:

  - `subscribe_below_data_origin_reports_protocol_error_then_streams`,
    `subscribe_beyond_sim_now_clamps_to_a_live_stream`,
    `unknown_symbol_subscribe_reports_protocol_error`,
    `dead_subscribe_reports_protocol_error_instead_of_silence`,
    `coalesced_subscribe_refusal_keeps_the_connection_up`,
    `replay_cap_refuses_subscribe_across_connections`,
    `subscribe_reserves_diagnostic_capacity_before_quiescing` - each asserts a
    `ProtocolError` reason string for a degradation that is now a typed
    outcome. Each is rewritten IN PLACE to match the corresponding
    `SubscriptionIssue` and to additionally assert the generation, not deleted
    in favor of the new tests above: the new tests cover coalescing, ordering
    and staleness, while these cover the individual degradations end to end at
    the socket, and dropping them would lose that coverage.
    `subscribe_below_data_origin_...` additionally changes BEHAVIOR under 3.5
    (the start is now genuinely clamped to the origin) and must assert the
    clamp, not just the diagnostic.
  - `reconcile_subscribe_start_ts_clamps_future_to_live` becomes a unit test of
    the pure `reconcile_entry_start_ts`, asserting both the returned effective
    start and the returned issue for all three cases.
  - `protocol_error_reasons_are_truncated` loses its subject: both
    reason-carrying subscribe sites go away. It is re-pointed at a surviving
    `ProtocolError` construction site (the `validate_subscriptions` boundary
    refusal), because what it guards is the lane's truncation contract, not the
    subscribe path.
  - `dedup_symbols_sorts_and_dedups` keeps its subject only for `Unsubscribe`;
    its comment must say so, since `dedup_symbols` no longer runs on the
    `Subscribe` path (duplicates are now a frame refusal).

  Explicitly still green and NOT re-blessed:
  `delayed_acks_must_not_stall_the_socket_read_loop` and
  `saturation_witness_control_is_sound`, workstream A's standing regression
  pair, which this landing must not disturb.

- Live end-to-end path, after `scripts/smoke.py` is rewritten to the new
  shape:

  ```
  brokkr run -p mogwai-server -- serve
  python3 scripts/smoke.py
  ```

  The smoke's eleven subscribe literals become entry lists with explicit
  generations; the four carrying `start_ts` move it into their entry. One NEW
  smoke assertion: a two-symbol subscribe where the second symbol is unknown
  receives exactly one `SubscriptionIssues` frame naming the second entry's
  generation, while the first symbol's ticks keep arriving. That is the
  end-to-end proof that a diagnostic is now attributable on a live socket, and
  no unit test spans the whole path.

### L2

```
brokkr test -p mogwai-adapter reconnect_sends_one_subscribe_carrying_every_cursor
brokkr check
brokkr run -p mogwai-server -- serve
python3 scripts/smoke.py
```

`reconnect_sends_one_subscribe_carrying_every_cursor` is NEW and replaces the
`subscribe_commands` assertion inside
`subscribe_while_disconnected_defers_to_on_connect`: three symbols subscribed
with three different advanced cursors produce exactly ONE `WsCommand`, whose
entries are sorted by symbol, carry each symbol's own cursor, carry three
distinct generations all greater than those previously recorded, and match the
generations the `subs` table now holds. A second NEW case in the same test
module, `reconnect_chunks_at_the_subscribe_cap`, populates a `subs` table with
`MAX_SUBSCRIBE_SYMBOLS + 1` entries and asserts `subscribe_commands` returns two
commands of 256 and 1. It exercises the pure function against the table and
drives NO server: 257 live subscriptions exhaust the 256 promise tickets and
close the connection by design (section 4.3), so an end-to-end version of this
test would prove the close rather than the chunking.

`brokkr check` is the regression gate for L2's real risk, which is not the
frame shape but the lock discipline: generations are stamped under the same
lock that reads the cursors, and AD5/AD7 must still hold.

No measurement gate applies. This spec is not justified by an estimated
throughput or volume win - the fan-out reduction from N frames to 1 is a
correctness consequence of per-entry cursors, not the reason to build it - so
there is no proceed/close threshold to price. The data-loader throughput gate
this contract names does not apply either: no landing here touches
`mogwai-data`.

## 7. Keep/revert

L1 is kept if every gate in its list is green, including the smoke's new
attributability assertion. It is reverted whole if the coalescing test shows a
multi-entry subscribe costing more than one priority frame, or if
`delayed_acks_must_not_stall_the_socket_read_loop` goes red - either means the
per-entry design has reintroduced the lane pressure workstream A removed, and a
partial patch on top would be exactly the "which fact to lose" choice
`docs/protocol-problem.md` rules out.

L2 is kept on its own gates and is independently revertible: reverting it
restores the per-symbol fan-out over the new wire, which is correct, merely
wasteful. That independence is why it is a separate landing.

## 8. Documentation that moves with the code

Per `docs/protocol-problem.md`'s standing requirement, and bundled into the
landings rather than committed alone.

With L1:

- `reference/architecture.md` - the `Subscribe` wire shape under
  `mogwai-protocol` (per-entry generation, cursor and regime); the admission
  section's `AdmissionRejected` / `ProtocolError` sentence gains
  `SubscriptionIssues` and loses the subscribe subject; the `mogwai-server`
  session section notes that a replay records the generation that spawned it;
  the `mogwai-adapter` section's "keeping the earliest `start_ts` on conflict"
  line is now wrong twice over (AD7 already changed it, and cursors are now
  per entry) and is rewritten; and the section 3.4 decision - no success
  frames, and the drought caveat that would reopen it - is recorded there
  because that is where a future reader looks for it.
- `mogwai-protocol` doc comments - `ProtocolError`'s untargetedness paragraph
  is rewritten to cover only whole-frame faults on either carrier, closing the
  "a correlation field is future work" sentence workstream A left open;
  `SubscriptionRequest` carries the client's monotonicity obligation, stated as
  an obligation and not merely a description; and
  `ADMISSION_FRAME_MAX_BYTES`'s own doc comment, which today derives 6144 from
  `AdmissionSubject::Subscribe` and `MAX_REFUSED_SYMBOLS_LISTED`, is rewritten
  to the `SubscriptionIssues` derivation in 4.1 - otherwise the constant's
  proof text describes a deleted type. `MAX_SUBSCRIPTION_ISSUES_LISTED`'s
  comment carries 3.8's truncation argument, including what is lost.
- `reference/havoc.md` and `reference/architecture.md` - both state that the
  market regime is venue-wide and not per-symbol. Per-entry regimes make a
  per-symbol regime EXPRESSIBLE on the wire, so both sentences become wrong on
  L1 and both are rewritten: the wire carries a regime per subscription entry;
  the adapter today sends the same value for every entry, so behavior is
  unchanged, and a genuinely per-symbol regime is now a client-side change
  only. This corrects the first draft, which said `reference/havoc.md` needed
  no change.
- `docs/protocol-problem.md` - workstream B's section is marked LANDED in the
  same style workstream A's is, with the settled questions of section 3 named
  (its own four, plus the five the typed shape forced) and the
  "Open for workstream B to settle" list removed.
- `docs/todo.md` - the workstream B item is REMOVED ENTIRELY on L2, per that
  file's own rule, with nothing left behind: its durable content is the
  architecture entries above and the code comments named in section 4.

Nothing here alters what a divergence DOES; `SubscriptionIssues` inherits
`ProtocolError`'s admission classification and `DelayAcks` exemption unchanged.
`reference/havoc.md`'s change is the regime-scope sentence above and nothing
else.

## 9. Stopping rule

Out of scope, explicitly:

- **The HTTP polling data path.** `PollCursor`, `run_poll_loop` and the AD6
  self-heal do not ride `Subscribe` at all; they page `/trades` with their own
  cursor. They are untouched, and the `poll_cursor` map keeps its independent
  lifecycle. A future unification of the two cursor mechanisms is a separate
  TODO, named here so its absence is a decision.
- **`Unsubscribe`.** No generation, no per-entry shape; it targets whatever is
  current, and section 4.1 gives the reason.
- **Success acknowledgment frames.** Settled negative in section 3.4.
- **The adapter manifest / crates.io build-contract discrepancy** (its own
  `docs/todo.md` item). Changing what the workspace builds against would
  confound every gate here, exactly as workstream A concluded.
- **`next_position` overflow**, **the tape-drought fingerprint decision**, and
  **the integration stub's missing `QueryOrders`/`QueryFills`** - separate
  `docs/todo.md` items, none of which the subscribe path reaches.
- **Rate limiting or deduplicating diagnostics.** Rejected in
  `docs/protocol-problem.md` and not revisited: per-entry attribution is what
  makes the volume legible, and coalescing bounds it without hiding anything.
- **Refusal-triggered resubscribe.** A `ReplayCapacity` or
  `SeekBudgetExhausted` on the current generation leaves that symbol's feed
  dead until something outside the adapter resubscribes; the adapter warns and
  stops. This spec delivers ATTRIBUTABILITY, which is the precondition for
  acting, not the acting. Automatic retry needs a backoff policy, a bound on
  retries, and a decision about whether a retry storm against an exhausted
  replay pool is better or worse than a dead feed - none of which the wire
  change settles, and all of which would be designed against a diagnostic that
  did not exist until now. AD6's self-heal is HTTP-poll-only and does not cover
  this. Named here so the gap is a decision, and it is a `docs/todo.md` item
  the moment a refusal is observed in practice.
- **Raising the live-subscription ceiling.** `ADMISSION_PROMISE_TICKETS ==
  MAX_SUBSCRIBE_SYMBOLS` caps live replays per connection at 256 and the 257th
  closes the connection. Pre-existing, unchanged by this spec, and repricing
  the promise pool is its own analysis (section 4.3).
- **Pagination of `SubscriptionIssues`.** Rejected in 3.8 with its reason.

The teardown stops at the `Subscribe` request/response surface and the state
each end keeps for it. The replay threading model, the pacing, the admission
lanes, the byte budget and the engine are all read but not rebuilt.

## 10. Review disposition

Two independent reviews of the first draft (`docs/subscription-protocol-review-1.md`,
`docs/subscription-protocol-review-2.md`) were validated against the code. Every
defect either review raised was real and is folded in above; the two reviews
overlapped on three of them (the effective-start payload, the monotonicity
coupling, the under-surveyed test inventory), and the merged treatment is the
one recorded here. What was REJECTED is only ever a proposed remedy, never a
finding, and each rejection is recorded so it is not re-litigated:

- **Pagination or a larger `SubscriptionIssues` frame** as the fix for the
  16-entry truncation. Rejected in 3.8: pagination puts sequencing state on the
  priority lane the coalescing exists to protect, and a larger frame reopens a
  proven ceiling. The defect is instead bounded by refusals-first ordering and
  `refusals_total`, and the residual loss is stated rather than hidden.
- **Making `spend_diagnostic` result-bearing, or adding a result-carrying join
  path from the replay thread to the socket owner.** Rejected: it would promise
  a close that nothing on that thread can execute. Today's best-effort logging
  is retained and the spec's signature was corrected to match (4.2).
- **Repricing `ADMISSION_PROMISE_TICKETS` so >256 live symbols work.** Rejected
  as scope: the ceiling is pre-existing and identical under today's fan-out.
  The chunking gate was reshaped to test the pure function instead of proving
  the close (4.3, section 6 L2).
- **Deleting the old `ProtocolError`-asserting server tests as near-duplicates
  of the new ones.** Rejected: the new tests cover coalescing, ordering and
  staleness; the old ones cover each individual degradation end to end at the
  socket. They are rewritten in place, not dropped (section 6).
- **Giving `StartAfterSimNow` an `Option<u64>` effective start** to match the
  venue's internal `None`. Rejected as a half-fix that still invites the client
  to treat the value as a position. The variant carries `sim_now`, named as the
  observation it is, and the adapter is forbidden from adopting it (3.5).

## References

- `reference/technical-implementation-spec.md` - the contract this spec is
  written against.
- `docs/protocol-problem.md` - the problem statement this spec is spawned from;
  workstream B, its agreed shape, and the four questions settled in section 3.
- `docs/todo.md` - the originating open item.
- `reference/architecture.md` - the session/replay/admission structure surveyed
  in section 2, and the "Tape arrival droughts" measurement section 3.4 leans
  on.
- `reference/havoc.md` - the `DelayAcks` contract and honest-content invariant
  that `SubscriptionIssues` inherits unchanged.
