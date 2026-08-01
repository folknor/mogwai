# Implementation spec: exec-pump admission control (workstream A)

Written against `reference/technical-implementation-spec.md`. Spawned from
`docs/protocol-problem.md`, section "Workstream A: the exec pump (lands first)"
- that file states the problem, the measurement, and the five commitments this
document turns into code. It is not restated here except where a commitment is
being SETTLED (the "Open for the spec to settle" list).

Scope: `mogwai-protocol`, `mogwai-server`, `mogwai-adapter`. Workstream B (the
subscription wire redesign) is explicitly out of scope; see "Stopping rule".

## 1. What is being built, in one paragraph

The session's execution path stops being a single bounded channel that both the
engine and the read loop push into. It becomes two lanes with explicit
admission accounting: a HELD lane (unbounded channel guarded by a per-connection
BYTE budget) carrying engine output through the `DelayAcks` shift, and a
PRIORITY lane (unbounded channel guarded by a per-connection FRAME budget)
carrying admission truth, exempt from `DelayAcks`, delivered ahead of held
traffic. Every producer that runs on the socket read loop reserves worst-case
capacity BEFORE the engine is allowed to mutate, refuses visibly with a new
`AdmissionRejected` wire variant if the reservation fails, and never awaits a
full channel. If the priority lane itself cannot take the refusal, the
connection is closed with an explicit overload close. This is a full rewrite of
the buffering model, not a resize.

## 2. Survey of the ground

Facts established by reading the current tree; each is load-bearing below.

**`crates/mogwai-server/src/ws.rs`.**

- `handle_socket` creates `tx/rx: mpsc::channel::<ServerMessage>(1024)` (the
  writer channel) and `exec_tx/exec_rx: mpsc::channel::<(Instant,
  ServerMessage)>(1024)` (the pump channel).
- The writer task loops `rx.recv()`, applies the `GoDark` gate to every frame
  and the `StallData` gate to `msg.is_market_data()`, then `serde_json::to_string`
  and `sink.send`. Serialization happens IN the writer.
- `spawn_exec_pump` dequeues `(arrived, msg)`, sleeps
  `wall_duration(delay_ms) - arrived.elapsed()`, forwards into `tx`. It is a
  time SHIFT: an already-elapsed deadline forwards immediately.
- Read-loop producers that `.await` into `exec_tx` (these are the stall):
  1. the undecodable-frame arm, via `send_exec_protocol_error`;
  2. `strip_unfireable_reopen_gap`'s diagnostic;
  3. `reconcile_subscribe_start_ts`'s two diagnostics (below-origin,
     beyond-sim-now);
  4. the unknown-symbol refusal in the per-symbol loop;
  5. the `replay_permits` capacity refusal in the same loop;
  6. the `order_cmd` arm, one `exec_tx.send((arrived, ev)).await` per event
     returned by `process_order_cmd`.
- Replay threads emit two diagnostics (unknown symbol - unreachable in
  production, and the dead-seek) via `send_cancellable`, from their own OS
  thread. They cannot block the reader and are NOT part of the defect.
- `quiesce_and_resume_floor` / `quiesce_replay` await a `spawn_blocking` join
  from the read loop. Bounded by `REPLAY_SEND_POLL` / `REPLAY_SLEEP_POLL`.
- Teardown order is: cancel + join replays, abort heartbeat, abort pump, drop
  `tx`, await writer.

**`crates/mogwai-server/src/http.rs`.**

- `process_order_cmd` validates at the protocol boundary, stamps a market
  price, then `state.engine.lock().await.process(order_cmd, ts)`. The engine
  lock is taken ONLY at that final line; nothing reads engine shape before it.
- `submit_order_http` calls the same function and returns the events in the
  HTTP response body. That path has no pump and no lane - it is unaffected by
  this spec and must stay unaffected.
- `AppState` holds `delay_ms`, `dark_until_ns`, `stall_until_ns`,
  `replay_permits`.

**`crates/mogwai-engine`.** Worst-case output per command, read off
`orders.rs`/`lib.rs`:

| command | worst case |
| --- | --- |
| `SubmitOrder` | `OrderAccepted` + `OrderFilled` (duplicate) + `OrderFilled` + `OrderCanceled` (IOC remainder) + `AccountState` = 5 frames |
| `CancelOrder` | `OrderCanceled` + `AccountState` = 2 frames |
| `ModifyOrder` | `OrderUpdated` + `AccountState` = 2 frames |
| `QueryOrders` | 1 `OrderStatusSnapshot`, rows = open + closed |
| `QueryFills` | 1 `FillSnapshot`, rows = recorded fills |

`AccountState` size scales with balances + positions; the two snapshots scale
with book size. This is why commitment 1 says an event count is not a memory
bound, and why the reservation must consult engine shape.

**`crates/mogwai-protocol`.**

- `ServerMessage::category()` is the single classifier; `ProtocolError` is
  currently `EventKind::Exec`, so `DelayAcks` holds it - the documented reason
  the diagnostics ride the pump at all.
- `EventKind` is `Exec | Fill | Data`; `is_execution()` is `!matches!(Data)`.
  `LatencyModel::delay_for` matches all three arms.
- `validate_submit_order` / `validate_modify_order` bound numerics only. NO
  length bound exists on `client_order_id`, `symbol`, or `request_id`. Those
  strings are client-controlled and echoed into server output, so today no
  upper bound on a produced frame's size exists at all.
- `control::MAX_DIVERGENCE_MS` is 3 600 000 (one hour), the ceiling that makes
  an unbounded buffer a memory-exhaustion vector.

**`crates/mogwai-adapter`.**

- `client/exec.rs::handle_exec_message` matches every `ServerMessage` variant
  exhaustively; `ProtocolError` logs a warning (line ~2060). `OrderRejected`
  requires the local mirror to know the order (else the A.11 log-and-drop).
  A refused CANCEL must become `OrderCancelRejected`, never `OrderRejected` -
  that distinction is already documented on the wire type and is why the
  refusal variant needs a command discriminator.
- `client/data.rs` also matches `ProtocolError` (~line 1264).
- `client/shared.rs::delay_for` buckets inbound frames by `msg.category()`.
- Adding a `ServerMessage` variant or an `EventKind` variant breaks both
  crates' exhaustive matches - that is the cross-crate landing
  `docs/protocol-problem.md` warns about.

**Tests already in place.** `mogwai-server/src/main.rs` carries
`delayed_acks_must_not_stall_the_socket_read_loop` (`#[ignore]`d, currently
failing = the defect) and `saturation_witness_control_is_sound` (green). They
share `reader_survives_saturation(delay_ms)`.

**Sibling reconciliation.** `docs/gen-cli-spec.md`,
`docs/gen-havoc-file-spec.md` and `docs/shared-bar-aggregator-spec.md` are the
other open specs. None of them touches `ws.rs`'s channel topology,
`ServerMessage`'s variant set, or `EventKind`; the bar-aggregator spec produces
market-data frames only, which never enter either lane. No sibling survey
refutes anything above.

## 3. Settling the open questions

`docs/protocol-problem.md` leaves six items open for workstream A's spec. Each
is decided here, with the reasoning, so it is not re-derived.

**3.1 Which lane subscribe diagnostics ride: the PRIORITY lane.** The category
argument that exempts an admission refusal applies verbatim to a degraded or
refused `Subscribe`: that frame reports what the venue's request-handling
refused or clamped, it is not something the matching engine produced. Applying
the argument once and consistently means the pump's ONLY producer becomes
engine output, which is exactly what makes commitment 2's "reserve before
producing" tractable - there is precisely one production site to gate.
Consequence: `ProtocolError` is reclassified out of `EventKind::Exec`, so
`DelayAcks` no longer holds it. That is a deliberate, documented behavior
change and it dissolves problem 2's feedback loop as a side effect (a
retrying client now gets its "unservable" answer promptly, so retries stop
being fed by the venue's own latency).

**3.2 The undecodable-frame diagnostic: PRIORITY lane, unattributed.** It is
the purest transport truth there is - the venue could not even decode the
frame. Carried as `AdmissionRejected { subject: AdmissionSubject::Frame, .. }`.

**3.3 The bound and its unit.**

- HELD lane: BYTES. `EXEC_HELD_BUDGET_BYTES = 8 * 1024 * 1024` per connection.
  Bytes, not events, because `AccountState` and the two snapshots have no
  per-frame size ceiling that a count could stand in for.
- PRIORITY lane: FRAMES. `ADMISSION_LANE_FRAMES = 64` per connection. A count
  is a legitimate memory bound HERE, and only here, because every frame on this
  lane is an `AdmissionRejected` or `ProtocolError` whose serialized size is
  provably under `ADMISSION_FRAME_MAX_BYTES = 4096` (section 5.3): reasons are
  server-generated and truncated (including the reasons on the EXISTING
  `ProtocolError` sites, which must route through `truncate_reason` - see
  5.1), and the only client-controlled strings are length-capped ids. 64
  frames x 4 KiB = 256 KiB, a real bound.
- The reservation is sized against the worst-case table in section 5.4.

**3.3a Two kinds of priority capacity, accounted separately.** A first draft of
this spec drew both queued frames and reserved-for-later diagnostics from the
same 64-frame pool. That is wrong twice over, and the fix is structural:

- A replay's dead-seek ticket is held for the replay's WHOLE life (5.9), not
  for the microseconds a queued frame lives. Drawn from the same pool, 64 live
  replays leave zero capacity for any actual refusal and the 65th subscribe
  closes a connection whose priority lane is completely empty. Reserving future
  promises against a queue-depth budget conflates a bound on memory with a
  bound on concurrency.
- `MAX_SUBSCRIBE_SYMBOLS = 256` (5.1) against 64 frames means one `Subscribe`
  naming 65 unknown symbols deterministically closes the connection, which
  directly contradicts the documented `S22a` contract on
  `mogwai-server/src/config.rs`: an over-cap or unservable subscribe refuses the
  symbol and "the connection stays up and its already-running streams are
  untouched".

Settled:

- `ADMISSION_LANE_FRAMES = 64` bounds QUEUED priority frames only.
- `ADMISSION_PROMISE_TICKETS` is a SEPARATE per-connection pool sized at
  `MAX_SUBSCRIBE_SYMBOLS` (256) x `ADMISSION_FRAME_MAX_BYTES` = 1 MiB worth of
  outstanding promises, one ticket per live replay. A promise is not a queued
  frame; it is capacity that is only ever spent by converting the ticket into a
  queued frame, and the conversion re-checks the queue budget (a ticket-holder
  that finds the queue full takes the overload close, as any producer does).
  The pool cannot be exhausted by healthy replays because the replay-thread cap
  (`max_concurrent_replays`) already bounds live replays per process, and per
  connection `MAX_SUBSCRIBE_SYMBOLS` bounds them exactly.
- A `Subscribe` COALESCES its per-symbol refusals: the per-symbol loop
  accumulates refused symbols and emits ONE `AdmissionRejected { subject:
  Subscribe { symbols, refused_total } }` at the end of the frame, so a
  256-symbol subscribe of unknown symbols costs one queued priority frame, not
  256. `symbols` lists at most `MAX_REFUSED_SYMBOLS_LISTED = 16` of them and
  `refused_total` carries the true count, so the frame stays under
  `ADMISSION_FRAME_MAX_BYTES` (5.3) while still being honest about how many
  were refused. This is why `AdmissionSubject::Subscribe` carries a bounded
  LIST rather than one symbol (5.3), and why the ceiling proof takes the
  coalesced frame as its worst case. The
  connection therefore stays up for every case `S22a` says it stays up for
  today; the overload close is reached only when the client is not reading at
  all.

**3.4 The refusal taxonomy: ONE variant with a subject enum.**
`docs/protocol-problem.md` proposed `OrderAdmissionRejected { client_order_id }`
and noted it cannot represent a refused cancel, modify, query, or subscribe.
Settled as a single `ServerMessage::AdmissionRejected { subject, reason,
ts_event }` where `subject: AdmissionSubject` names what was refused. Rationale
for deviating from the proposed name and shape:

- The adapter's translation is not uniform - a refused submit must become
  nautilus `OrderRejected`, a refused cancel `OrderCancelRejected`, a refused
  modify `OrderModifyRejected`. A bare `client_order_id` cannot select between
  them, so a discriminator is required whatever the variant is named.
- One variant means one `category()` arm, one `DelayAcks` exemption, one
  serde round-trip test, one adapter match arm with an inner dispatch. Three or
  six sibling variants would replicate the category rationale N times, which is
  precisely the "reinvented category logic" the source document warns against.

`AdmissionSubject::Submit { client_order_id }` is the agreed
`OrderAdmissionRejected` under a different spelling; nothing about its
semantics changes.

**3.5 Successful subscriptions do NOT gain result frames.** New design, not a
consequence of anything agreed; and workstream B may want to attach any such
acknowledgment to a per-entry generation id. Deferring it here is not a hole -
it is work that belongs to a different item, named and excluded.

**3.6 Older-vs-unknown generation, and per-entry `start_ts`/`regime`.**
Workstream B. Out of scope.

## 4. Invariants the rewrite must satisfy

These are the acceptance criteria; every brick below exists to serve one.

- **I1.** No producer running on the connection's READ LOOP may ever await or
  block on a full channel. Producers on their own OS thread or task (replay
  threads, the pump, the heartbeat) may block - blocking them cannot stall
  input.
- **I2.** Engine state is never mutated for a command whose worst-case output
  could not be reserved.
- **I3.** ADMISSION never drops an execution event the engine has produced.
  Refusal happens before production; after production the frame is delivered,
  dropped by an ARMED HAVOC WINDOW, or the connection dies. The havoc exception
  is explicit and pre-existing: `GoDark` deliberately drops produced execution
  frames in the writer and `StallData` gates market data, both documented in
  `reference/havoc.md`; an invariant phrased as "delivered or the connection
  dies" would read as a contradiction of that contract to the next implementer.
  What I3 forbids is a SILENT, capacity-driven drop - the failure mode this
  spec exists to prevent. Two corollaries the implementation must honor:
  serialization failure at the producer (5.7) is log-and-skip only because
  `OutboundFrame` makes it unreachable by construction; if it ever becomes
  reachable it is a connection-fatal error, not a skip. And an unbounded-lane
  `send` returning `Err` means the receiver is gone, i.e. the connection is
  already tearing down - the producer treats it as terminal rather than calling
  the path infallible.
- **I4.** `DelayAcks` holds DELIVERY of engine output. It never holds, delays,
  or gates ADMISSION, and it never touches the priority lane.
- **I5.** Every refusal is visible on the wire, attributed to what it refused.
- **I6.** The last resort is a close with a stated reason, never silence.

## 5. Target artifacts

### 5.1 `mogwai-protocol`: identifier length caps

New in `messages.rs`:

```rust
/// Maximum byte length of any client-supplied identifier the venue echoes
/// back into its own output: `client_order_id`, `request_id`. The cap exists
/// so a produced frame has a computable upper bound - the admission
/// reservation in `mogwai-server` sizes worst-case output against it, and an
/// unbounded id would make that bound unprovable (and let one 8 MiB order id
/// exhaust a connection's whole execution budget).
pub const MAX_CLIENT_ID_LEN: usize = 64;

/// Maximum byte length of a symbol on the wire, same reasoning.
pub const MAX_SYMBOL_LEN: usize = 32;

/// Maximum byte length of a server-generated `reason` string. Constructors
/// truncate to this on a char boundary rather than rejecting - a reason is
/// diagnostic prose, and a truncated diagnostic is still truthful about what
/// happened, whereas a refused frame would not be.
pub const MAX_REASON_LEN: usize = 512;

/// Truncate a server-generated reason to `MAX_REASON_LEN` bytes on a char
/// boundary, appending nothing (the truncation is visible as an abrupt end).
#[must_use]
pub fn truncate_reason(reason: String) -> String;

/// Maximum byte length of a currency code, an instrument base or an
/// instrument quote as configured. Operator-supplied config strings reach the
/// wire through `AccountState`'s balance rows and every position row, so
/// `BALANCE_ROW_MAX_BYTES` / `POSITION_ROW_MAX_BYTES` (5.4) are only upper
/// bounds if these are capped too. Validated where the config is loaded
/// (`mogwai-server/src/config.rs`, alongside the existing balance
/// validation), which fails startup rather than the connection.
pub const MAX_CURRENCY_LEN: usize = 16;

/// Worst-case expansion factor `serde_json` applies to an arbitrary string of
/// N bytes: a byte that must be escaped as `\uXXXX` costs six output bytes.
/// Every `*_MAX_BYTES` constant in `sizing` is stated in SERIALIZED bytes, so
/// each embedded string contributes `JSON_ESCAPE_FACTOR * cap`, never its raw
/// cap. Sizing against raw lengths - which an implementer measuring with
/// ordinary ASCII test strings would never catch - makes the reservation a
/// typical case rather than an upper bound.
pub const JSON_ESCAPE_FACTOR: usize = 6;
```

**The control-plane reason.** `control::Divergence::RejectNextSubmit { reason }`
is operator-supplied, uncapped today, and `mogwai-engine/src/orders.rs` echoes
it verbatim into `ServerMessage::OrderRejected.reason`. Left alone it defeats
the whole size model: a 1 MiB armed reason makes a produced batch exceed its
reservation, so `submit_produced`'s "infallible by construction" is false and
`Reservation::charge` is handed more than was reserved. Settled: the
`/control/divergence` handler runs every operator-supplied `reason` through
`truncate_reason` AT THE BOUNDARY, before the divergence is stored, so the
engine can only ever echo an already-bounded string and no engine change is
needed. The truncation is documented in `reference/havoc.md` with the arming
control.

`validate_submit_order` gains, before the numeric checks:

```rust
if order.client_order_id.len() > MAX_CLIENT_ID_LEN {
    return Err("client_order_id exceeds MAX_CLIENT_ID_LEN");
}
if order.symbol.len() > MAX_SYMBOL_LEN {
    return Err("symbol exceeds MAX_SYMBOL_LEN");
}
```

New sibling validators. `validate_client_order_id` and `validate_request_id`
are called from `process_order_cmd` before anything else; `validate_symbols` is
called from the `Subscribe`/`Unsubscribe` arm of the READ LOOP, NOT from
`process_order_cmd` - subscribe messages never reach that function (it
`unreachable!`s on them, see `http.rs`), so wiring the symbol validator there
would leave it dead code and the cardinality cap unenforced.

```rust
pub fn validate_client_order_id(id: &ClientOrderId) -> Result<(), &'static str>;
pub fn validate_request_id(id: &str) -> Result<(), &'static str>;
pub fn validate_symbols(symbols: &[Symbol]) -> Result<(), &'static str>;
```

`validate_symbols` also caps cardinality at `MAX_SUBSCRIBE_SYMBOLS = 256`, so a
single `Subscribe` cannot demand 100k per-symbol reservations in one read-loop
iteration.

Over-length ids are refused at the protocol boundary with the EXISTING
mechanism (`OrderRejected` / `OrderModifyRejected` / `OrderCancelRejected` from
`process_order_cmd`, `ProtocolError` for a `Subscribe`), not with
`AdmissionRejected`: they are malformed requests, not capacity refusals, and
conflating them would make an admission refusal unreadable as a load signal.

The refusal must NOT echo the offending identifier as-is - an 8 MiB
`client_order_id` would otherwise produce an 8 MiB `OrderRejected`, recreating
the exact unbounded frame the cap exists to prevent. The rejection frame
carries the id truncated to `MAX_CLIENT_ID_LEN` bytes on a char boundary, and
its `reason` states that the id was truncated for display and that no order was
created under either spelling. A truncated echo cannot be mistaken for a valid
correlation because the id is, by construction, not one the venue would ever
accept; a client matching on it finds no order, which is the truth.

### 5.2 `mogwai-protocol`: the admission category

```rust
pub enum EventKind {
    Exec,
    Fill,
    Data,
    /// Transport/admission truth: what the venue's request handling refused,
    /// clamped, or could not decode. Never engine output, so the knob that
    /// holds engine output (`DelayAcks`) legitimately does not reach it -
    /// see reference/havoc.md. `is_execution()` is FALSE for this kind,
    /// which is what implements that exemption in one place.
    Admission,
}

impl EventKind {
    pub fn is_execution(self) -> bool {
        matches!(self, EventKind::Exec | EventKind::Fill)
    }
    /// Whether this kind rides the priority lane on the server and bypasses
    /// the execution delay pump.
    pub fn is_admission(self) -> bool {
        matches!(self, EventKind::Admission)
    }
}
```

`is_execution` is rewritten from `!matches!(Data)` to a positive list - a new
kind must now opt IN to being delayed, rather than being delayed by default.

`LatencyModel::delay_for` gains `EventKind::Admission => self.exec_event_nanos`.
The adapter's simulated inbound latency is an adapter-side consumer knob, not a
venue contract, and an admission frame is exec-adjacent from the consumer's
point of view; giving it its own configured field would be a new knob nobody
asked for.

### 5.3 `mogwai-protocol`: the refusal variant

```rust
/// What an `AdmissionRejected` refers to. Present because the refusal must be
/// translatable: the adapter turns a refused submit into nautilus
/// `OrderRejected` but a refused cancel into `OrderCancelRejected` - flipping
/// a live order to Rejected because its CANCEL was refused would be an invalid
/// transition (see `ServerMessage::OrderCancelRejected`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind")]
pub enum AdmissionSubject {
    Submit { client_order_id: ClientOrderId },
    Cancel { client_order_id: ClientOrderId },
    Modify { client_order_id: ClientOrderId },
    /// A `QueryOrders` or `QueryFills`; the id is the one that would have been
    /// echoed on the reply (bounded by `validate_request_id` at
    /// `MAX_CLIENT_ID_LEN`, which is what makes this subject's contribution to
    /// `ADMISSION_FRAME_MAX_BYTES` computable), so a waiting requester can fail
    /// its own wait instead of timing out. `query` names WHICH query, because
    /// the adapter keeps two separate waiter maps keyed by request id
    /// (`client/exec.rs`'s `pending.orders` / `pending.fills`) and the
    /// protocol nowhere requires ids to be unique across the two. Without the
    /// discriminator the adapter must probe both maps and could wake the wrong
    /// waiter on a collision.
    Query { request_id: String, query: QueryKind },
    /// Subscriptions the venue would not admit, COALESCED into one frame per
    /// `Subscribe` (3.3a): the per-symbol loop accumulates and emits once, so
    /// a 256-symbol subscribe of unknown symbols costs one priority frame
    /// rather than 256 and cannot close a connection that `S22a` says stays
    /// up. `symbols` lists at most `MAX_REFUSED_SYMBOLS_LISTED` of them - that
    /// truncation is what keeps `ADMISSION_FRAME_MAX_BYTES` provable -
    /// and `refused_total` is the true count. An empty `symbols` with a
    /// non-zero `refused_total` is impossible; `refused_total == 0` means the
    /// whole frame was refused before any symbol was reached.
    Subscribe {
        symbols: Vec<Symbol>,
        refused_total: usize,
    },
    /// A frame the venue could not decode, or could not attribute at all.
    Frame,
}

/// Which venue-truth query a refused `Query` subject refers to. Mirrors the
/// adapter's two waiter maps one-for-one.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum QueryKind {
    Orders,
    Fills,
}

ServerMessage::AdmissionRejected {
    subject: AdmissionSubject,
    reason: String,
    ts_event: u64,
},
```

`category()`:

```rust
ServerMessage::AdmissionRejected { .. } | ServerMessage::ProtocolError { .. }
    => EventKind::Admission,
```

`ProtocolError` moves out of `Exec` in the same edit (section 3.1). Its
doc comment gains the lane/category rationale. Its "untargetedness is
deliberate" paragraph is NOT rewritten here - that inversion belongs to
workstream B, which adds the correlation field. This spec only records that
the untargetedness argument now rests on a NEW footing: with diagnostics on
the priority lane they are no longer subject to `DelayAcks`, which removes the
lateness that problem 3 says breaks the argument for the `DelayAcks` case
specifically. Network reordering and the quiesce-and-replace race remain, so
problem 3 is narrowed, not closed, and workstream B still lands.

Provable per-frame ceiling:

```rust
/// Upper bound on the serialized bytes of any `EventKind::Admission` frame -
/// `AdmissionRejected` AND `ProtocolError`, since both ride the priority lane.
/// Provable, and proven by `admission_frames_fit_their_ceiling`: the widest
/// subject is `Subscribe` with `MAX_REFUSED_SYMBOLS_LISTED` symbols of
/// `MAX_SYMBOL_LEN`, every embedded string is charged at `JSON_ESCAPE_FACTOR`
/// times its cap, the reason is `truncate_reason`d to `MAX_REASON_LEN`, and
/// the remaining fields are a u64, a usize and fixed JSON scaffolding:
/// `JSON_ESCAPE_FACTOR * (MAX_REFUSED_SYMBOLS_LISTED * MAX_SYMBOL_LEN +
/// MAX_REASON_LEN)` plus scaffolding, rounded up to the constant below. The
/// old 4096 did not survive either the escape factor or the coalesced
/// subscribe subject.
///
/// The `ProtocolError` half of the bound is NOT free today: the undecodable
/// frame site passes `serde_json::Error::to_string()`, whose text echoes
/// client-controlled unknown-variant and unknown-field names, and the
/// unknown-symbol site interpolates the client's symbol. EVERY existing
/// `ProtocolError` construction site must be routed through `truncate_reason`
/// as part of L3; the ceiling is unproven otherwise, and with it the priority
/// lane's frame COUNT stops being a memory bound at all (3.3).
pub const ADMISSION_FRAME_MAX_BYTES: usize = 8192;

/// How many refused symbols a coalesced `Subscribe` subject names before it
/// falls back to `refused_total` alone (3.3a).
pub const MAX_REFUSED_SYMBOLS_LISTED: usize = 16;
```

64 queued frames x 8 KiB = 512 KiB per connection, still a real bound.

### 5.4 `mogwai-protocol`: the worst-case size model

New module `mogwai_protocol::sizing`. It lives in the protocol crate because it
is a statement about the wire format, and both the server (to reserve) and its
tests (to prove the bound) need it.

```rust
/// The engine-state facts a reservation must know to bound a command's output.
/// Read from the engine under the same lock that will then process the
/// command, so the shape cannot drift between the reservation and the
/// production it covers.
#[derive(Debug, Clone, Copy)]
pub struct BookShape {
    pub balances: usize,
    pub positions: usize,
    pub open_orders: usize,
    pub closed_orders: usize,
    pub recorded_fills: usize,
}
```

Every string cap below is multiplied by `JSON_ESCAPE_FACTOR`, because these are
SERIALIZED byte counts and a client may fill an id with characters serde
escapes to `\uXXXX`. The fixed addends are JSON scaffolding and numerics only.

```rust
const ESC: usize = JSON_ESCAPE_FACTOR;

pub const ORDER_EVENT_MAX_BYTES: usize =
    256 + ESC * (2 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN + MAX_REASON_LEN);
pub const BALANCE_ROW_MAX_BYTES: usize = 192 + ESC * MAX_CURRENCY_LEN;
pub const POSITION_ROW_MAX_BYTES: usize = 128 + ESC * MAX_SYMBOL_LEN;
pub const ORDER_STATUS_ROW_MAX_BYTES: usize =
    384 + ESC * (2 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN);
pub const FILL_ROW_MAX_BYTES: usize =
    320 + ESC * (3 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN);
pub const SNAPSHOT_ENVELOPE_MAX_BYTES: usize = 128 + ESC * MAX_CLIENT_ID_LEN;

#[must_use]
pub fn account_state_max_bytes(shape: &BookShape) -> usize;

/// Upper bound on the total serialized bytes `Engine::process` can produce for
/// `cmd` against a book of `shape`. The worst cases are enumerated from the
/// engine's own branches (see the table in docs/exec-pump-admission-spec.md);
/// `worst_case_reservation_covers_actual_output` pins the claim.
#[must_use]
pub fn worst_case_output_bytes(cmd: &ClientMessage, shape: &BookShape) -> usize;
```

Concretely:

- `SubmitOrder` -> `4 * ORDER_EVENT_MAX_BYTES + account_state_max_bytes(shape')`
  where `shape'` is `shape` with `positions + 1` AND `balances + 2`. Four
  order-shaped frames: accepted, duplicated fill, fill, canceled remainder.
  The balance widening is not cosmetic: `mogwai-engine/src/account.rs` mutates
  BOTH the base and the quote entry per fill via `entry(..).or_default()`, so a
  first fill in a new pair introduces up to two currencies the pre-command
  snapshot never had. Widening by one (or not at all) under-counts
  `account_state_max_bytes` by up to two `BALANCE_ROW_MAX_BYTES` and makes
  `submit_produced`'s domination claim false.
- `CancelOrder`, `ModifyOrder` ->
  `ORDER_EVENT_MAX_BYTES + account_state_max_bytes(shape)`.
- `QueryOrders` -> `SNAPSHOT_ENVELOPE_MAX_BYTES + (open_orders + closed_orders) * ORDER_STATUS_ROW_MAX_BYTES`.
- `QueryFills` -> `SNAPSHOT_ENVELOPE_MAX_BYTES + recorded_fills * FILL_ROW_MAX_BYTES`.
- `Subscribe`/`Unsubscribe` -> `0`; they produce no engine output (the engine
  returns an empty vec), and their diagnostics are priority-lane frames
  reserved separately.

**How the constants are justified.** A finite test matrix cannot PROVE an upper
bound - it samples it. Each constant above therefore carries, in its doc
comment, a field-by-field derivation from the serialized struct it bounds (key
names and punctuation counted, each numeric at its widest decimal form, each
string at `JSON_ESCAPE_FACTOR * cap`), and the matrix test is a CHECK on that
derivation, not its evidence. A constant without such a derivation is a guess
and must not land. `worst_case_reservation_covers_actual_output` gains, on top
of the shapes already listed, an adversarial-string case: every id and symbol
filled to its cap with characters serde escapes, plus a `RejectNextSubmit`
armed with a `MAX_REASON_LEN` reason (post-truncation, per 5.1) and one armed
with an over-length reason to pin that the boundary truncation happened.

### 5.5 `mogwai-engine`: shape accessor

```rust
impl Engine {
    /// The size facts an admission reservation needs. Cheap: five `len()`
    /// reads, no allocation, no iteration.
    #[must_use]
    pub fn book_shape(&self) -> BookShape;
}
```

Nothing else in the engine changes. The engine remains ignorant of lanes,
budgets and transports - it reports its own shape and is asked to process or
not asked at all.

### 5.6 `mogwai-server`: the budget types

New module `crates/mogwai-server/src/admission.rs`.

```rust
/// Per-connection byte ceiling on execution output that has been produced but
/// not yet written to the socket. The hard bound commitment 1 requires:
/// `MAX_DIVERGENCE_MS` permits a one-hour `DelayAcks`, so an unbounded held
/// lane turns a connection stall into process-wide memory exhaustion.
pub const EXEC_HELD_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Per-connection frame ceiling on the priority lane. A COUNT is a legitimate
/// memory bound here and nowhere else, because every frame on this lane is
/// bounded by `ADMISSION_FRAME_MAX_BYTES`.
pub const ADMISSION_LANE_FRAMES: usize = 64;

/// Byte budget for produced-but-unwritten execution output.
///
/// A `Semaphore` whose permits ARE bytes. Reservation is
/// `try_acquire_many_owned` + `forget()`, and the permits come back through
/// explicit `add_permits` calls at two points: the producer returns the unused
/// remainder as soon as the real batch is serialized, and the pump returns a
/// frame's charge as it hands that frame to the writer. Permits are forgotten
/// rather than held as RAII guards because a frame's charge outlives the
/// producer's stack frame - it belongs to the frame, and travels with it.
#[derive(Clone)]
pub struct ByteBudget {
    permits: Arc<Semaphore>,
}

impl ByteBudget {
    pub fn new(bytes: usize) -> Self;
    /// Reserve `bytes`, or fail. Never blocks - this runs on the read loop
    /// (I1). Fails immediately (rather than never succeeding) when `bytes`
    /// exceeds the whole budget.
    /// `bytes` is converted to the `u32` tokio's `try_acquire_many_owned`
    /// takes; a request that does not fit `u32` (or exceeds
    /// `Semaphore::MAX_PERMITS`) returns `None` rather than truncating - a
    /// truncating cast would grant a reservation smaller than the output it is
    /// supposed to dominate, which is the one failure mode this whole type
    /// exists to rule out. `EXEC_HELD_BUDGET_BYTES` is 8 MiB, far inside
    /// `u32`, so the conversion is total in practice and defensive in code.
    pub fn try_reserve(&self, bytes: usize) -> Option<Reservation>;
    pub fn release(&self, bytes: usize);
}

/// A granted reservation. `charge(actual)` keeps `actual` bytes charged and
/// releases the rest; `Drop` without a `charge` releases everything, so a
/// producer that panics or returns early cannot leak budget.
pub struct Reservation {
    budget: ByteBudget,
    reserved: usize,
}

impl Reservation {
    pub fn charge(self, actual: usize);
}

/// Frame budget for the priority lane, same discipline in units of frames.
/// A `Ticket` may outlive its producer: the subscribe path reserves one for a
/// replay's possible dead-seek diagnostic and hands it to the replay thread,
/// which releases it on exit if it never fires.
#[derive(Clone)]
pub struct FrameBudget { permits: Arc<Semaphore> }
pub struct Ticket { budget: FrameBudget }
```

### 5.7 `mogwai-server`: the outbound frame and the writer

Serialization moves OUT of the writer and into the producers. This is what
makes the byte budget exact rather than estimated: the charge is the true
length of the bytes that will go on the socket.

```rust
/// A frame already rendered to its wire bytes, plus the two facts the writer's
/// havoc gates need. Serializing at the producer - the replay thread, the
/// admission path, the order path - means the byte budget charges REAL bytes
/// and moves JSON cost off the single writer task.
pub struct OutboundFrame {
    pub payload: String,
    pub is_market_data: bool,
}

/// What the writer accepts. `Close` exists so the overload path (commitment 5)
/// can state a reason on the socket: the writer owns the sink, so no other
/// task can send a close frame.
pub enum Outbound {
    Frame(OutboundFrame),
    Close(CloseSpec),
}

pub struct CloseSpec {
    pub code: u16,
    pub reason: String,
}

/// WS 1013 "Try Again Later": the venue is refusing further work on this
/// connection because its admission path is saturated. Deliberately NOT a
/// silent stall - a close is a delivery failure, which the honest-content
/// invariant permits, and it is the least ambiguous signal available.
pub const CLOSE_ADMISSION_OVERLOAD: u16 = 1013;
```

The writer task becomes:

```rust
let mut prio_open = true;
let mut held_open = true;
while prio_open || held_open {
    let next = tokio::select! {
        biased;
        prio = prio_rx.recv(), if prio_open => match prio {
            Some(v) => Some(v),
            None => { prio_open = false; continue }
        },
        held = rx.recv(), if held_open => match held {
            Some(v) => Some(v),
            None => { held_open = false; continue }
        },
    };
    // ... GoDark gate, StallData gate on is_market_data, sink.send(payload)
    // ... release the frame's byte charge AFTER the send or the havoc drop
    // ... Outbound::Close => send the close frame, then break
}
```

`biased;` with the priority receiver first is what makes the lane a PRIORITY
lane: an admission refusal overtakes queued market data and queued
already-delayed execution output.

The per-branch disable flags are load-bearing, not style. A bare
`select! { prio = prio_rx.recv(), held = rx.recv() }` with `biased` busy-spins
the moment ONE side closes: a closed receiver returns `None` immediately and
forever, so with `prio_tx` still alive and `tx` dropped the loop burns a core
until teardown. "Both receivers returning `None` ends the loop" is a property
the disable flags create; it is not what the naive form does.

Serialization failure is handled at the producer (log + skip), as the writer
did; `OutboundFrame` cannot carry an unserializable value by construction (see
I3's corollary for why that construction, not the skip, is the guarantee).

**Every producer migrates, including the heartbeat.** `tx` changes type from
`Sender<ServerMessage>` to `Sender<Outbound>`, so every existing sender must
serialize at its own site. Besides the order path, the admission path and the
replay threads named elsewhere in this spec, that is `spawn_heartbeat`, which
today sends `ServerMessage::Heartbeat` directly into `tx`. It serializes its
own frame with `is_market_data: false` and takes no budget: the heartbeat is
server-generated at a bounded cadence, is not engine output, and predates the
lanes. An implementer who migrates only the producers this spec discusses will
find the heartbeat is the one that fails to compile - stated here so it is a
known step rather than a surprise.

### 5.8 `mogwai-server`: the two lanes

```rust
/// The session's outbound execution machinery. One value threaded through the
/// read loop, the replay spawns and the pump, replacing the bare
/// `exec_tx: mpsc::Sender<(Instant, ServerMessage)>` that both stalled the
/// reader and hid the two lanes behind one channel.
#[derive(Clone)]
pub struct ExecLanes {
    /// Engine output awaiting its `DelayAcks` deadline. UNBOUNDED by channel
    /// capacity ON PURPOSE: the bound is `held_budget`, in bytes, and a
    /// channel-capacity bound would either be redundant with it or (being a
    /// frame count) be the non-bound commitment 1 rejects. `send` on an
    /// unbounded sender never blocks, which is how the read loop satisfies I1.
    held_tx: mpsc::UnboundedSender<HeldFrame>,
    held_budget: ByteBudget,
    /// Admission truth. Also unbounded-by-channel, bounded by `prio_budget`
    /// in frames.
    prio_tx: mpsc::UnboundedSender<Outbound>,
    prio_budget: FrameBudget,
    /// Outstanding promises of future priority frames (one per live replay),
    /// accounted separately from queue depth - see 3.3a for why sharing one
    /// pool imposes a 64-stream cap and breaks the `S22a` contract.
    promise_budget: FrameBudget,
}

struct HeldFrame {
    arrived: Instant,
    bytes: usize,
    frame: OutboundFrame,
}
```

`ExecLanes` API, all non-blocking:

```rust
impl ExecLanes {
    /// Reserve worst-case output for `cmd` against `shape`. Returns `None` if
    /// the budget cannot cover it - the caller must then refuse WITHOUT
    /// letting the engine see the command (I2).
    pub fn reserve(&self, cmd: &ClientMessage, shape: &BookShape) -> Option<Reservation>;

    /// Hand the engine's real batch to the held lane under `reservation`,
    /// charging actual serialized bytes and releasing the remainder.
    /// Infallible by construction: the reservation dominates the batch.
    pub fn submit_produced(&self, reservation: Reservation, arrived: Instant, events: Vec<ServerMessage>);

    /// Reserve capacity for a boundary refusal (`BOUNDARY_REFUSAL_BYTES`),
    /// used by the two pre-engine refusal paths in `process_order_cmd` that
    /// have no `BookShape` to size against (5.9).
    pub fn try_reserve_boundary(&self) -> Option<Reservation>;

    /// Reserve one priority-lane QUEUE slot. `None` means the lane is full,
    /// which is the overload condition (commitment 5).
    pub fn reserve_admission(&self) -> Option<Ticket>;

    /// Reserve a PROMISE of future priority capacity from the separate
    /// `ADMISSION_PROMISE_TICKETS` pool (3.3a) - held for a replay's whole
    /// life, so it must not draw on the queue-depth budget.
    pub fn reserve_promise(&self) -> Option<Ticket>;

    /// Emit an admission frame against a ticket. Never blocks, never fails.
    pub fn emit_admission(&self, ticket: Ticket, msg: ServerMessage);
}
```

The pump becomes a receive-and-deadline loop over `held_tx`'s receiver. It
keeps the existing per-event-deadline contract verbatim (anchor at arrival,
already-elapsed forwards immediately, `delay_ms` re-read per frame so re-arming
and `ClearDivergences` reach queued frames). The only changes: it forwards
`Outbound::Frame` instead of `ServerMessage`, and it carries the frame's byte
charge along INTO `tx` rather than releasing it there (see below). It may block
on `tx` being full - it is its own task, so I1 is unaffected.

Note what did NOT need to change: intake no longer needs a `select!` between
"receive" and "the oldest frame's deadline", because intake is now an
unbounded-channel send that cannot block in the first place. The
receive-or-deadline loop `docs/protocol-problem.md` sketches was one way to
keep intake running; making intake non-blocking by construction is the simpler
way, and it is what makes the pump a pure time shift with no admission role at
all.

**Where the charge is released: at the WRITER, not at the pump.** A first draft
had the pump call `held_budget.release(frame.bytes)` as it forwarded into `tx`.
That silently voids the bound this spec is built on: `tx` is a 1024-slot
channel of frames with no per-frame size ceiling (a `QueryOrders` snapshot over
a 500-order book is arbitrarily large), so releasing on hand-off leaves up to
1024 unaccounted frames resident and the real bound becomes
`EXEC_HELD_BUDGET_BYTES + unbounded`. The byte charge therefore TRAVELS with
the frame:

```rust
pub struct OutboundFrame {
    pub payload: String,
    pub is_market_data: bool,
    /// Bytes charged against the connection's held budget, released by the
    /// writer once this frame has been written to the socket or dropped by an
    /// armed havoc window. `None` for frames that were never charged - market
    /// data and heartbeats.
    pub charge: Option<HeldCharge>,
}
```

`HeldCharge` owns a `ByteBudget` handle and the byte count, and releases in
`Drop`. Drop-on-release rather than an explicit call is what makes the two
non-write exits correct for free: a `GoDark` drop in the writer, and the writer
task dying with queued frames still in hand. The budget is then exactly what it
claims - a ceiling on produced-but-unwritten execution bytes, end to end.

**The overload close must terminate even against a peer that never reads.**
Queuing `Outbound::Close` on the priority lane and then awaiting the writer is
not by itself a terminal path: the writer may already be parked in
`sink.send()` on a full TCP window, in which case the close is never written
and teardown blocks behind it forever - the same hang, one layer down. The
close path is therefore: send `Outbound::Close` on the priority lane (it jumps
the held queue by the `biased` select), then await the writer's join under a
bounded timeout (`CLOSE_GRACE`, 2 s of WALL time - not sim time, this is a
transport deadline), and on timeout ABORT the writer task and drop the socket.
A reasoned close is best-effort by nature; what is not optional is that the
connection's resources are released either way.

### 5.9 `mogwai-server`: the read-loop rewrite

**Order commands.** `process_order_cmd` is restructured so admission and
production happen under one engine lock:

```rust
pub(crate) enum OrderOutcome {
    Produced(Vec<ServerMessage>),
    /// The protocol boundary refused it; these are engine-free frames and are
    /// charged against the reservation like any other output.
    Refused(Vec<ServerMessage>),
    /// Admission refused. Carries the frame to put on the PRIORITY lane.
    NotAdmitted(ServerMessage),
}

pub(crate) async fn process_order_cmd(
    order_cmd: ClientMessage,
    state: &AppState,
    lanes: &ExecLanes,
) -> OrderOutcome;
```

Body order, and every step of it is load-bearing:

1. Validate ids and lengths (5.1). A failure returns `Refused`, which CARRIES
   ITS OWN RESERVATION - see below.
2. `stamp_market_price` (may block on the checkpoint mutex; unchanged), then
   re-sample `ts` exactly as today.
3. The EXISTING post-stamp synthesis-failure branch, unchanged in behavior: a
   MARKET order still price-less after the stamp, for a symbol this venue does
   list, is rejected here with the venue-owns-this-failure reason rather than
   being handed to the engine. It is `Refused` and takes a boundary reservation
   exactly like step 1. This branch is easy to lose in the restructure - it
   sits between the re-sample and the engine lock in `http.rs` today, which is
   precisely where the new steps 4-7 are being inserted.
4. `let mut engine = state.engine.lock().await;`
5. `let shape = engine.book_shape();`
6. `let Some(reservation) = lanes.reserve(&cmd, &shape) else { return NotAdmitted(...) };`
   - the engine has NOT been asked to process anything, so nothing mutated (I2).
7. `let events = engine.process(cmd, ts); drop(engine);`
8. Return `Produced { events, reservation }`.

The reservation must travel with the events; the `OrderOutcome` sketch above is
simplified for readability - `Produced` and `Refused` both carry one. The lock
is held across steps 5-7 so `shape` cannot drift.

**Boundary refusals need a reservation too, and it can fail.** Steps 1 and 3
return before the engine lock is ever taken, so there is no `shape` to size
against and no reservation in hand - yet the caller hands `Refused` to
`submit_produced`, which requires one. The hole is closed by giving the
boundary path its own fixed-size reservation:

```rust
/// A boundary refusal produces exactly one order-shaped frame and no
/// `AccountState`, so its worst case is a constant.
pub const BOUNDARY_REFUSAL_BYTES: usize = ORDER_EVENT_MAX_BYTES;
```

Steps 1 and 3 do `lanes.try_reserve_boundary()` FIRST, and when even that
cannot be granted the outcome is `NotAdmitted` with an `AdmissionRejected`
carrying the appropriate subject - the same reserve-or-refuse-or-close ladder
every other producer walks. There is no path where a frame is produced against
no reservation.

The `handle_socket` order arm then becomes:

```rust
order_cmd => match process_order_cmd(order_cmd, &state, &lanes).await {
    OrderOutcome::Produced { events, reservation }
    | OrderOutcome::Refused { events, reservation } => {
        lanes.submit_produced(reservation, Instant::now(), events);
    }
    OrderOutcome::NotAdmitted(frame) => match lanes.reserve_admission() {
        Some(ticket) => lanes.emit_admission(ticket, frame),
        None => break CloseSpec::overload("execution admission lane saturated"),
    },
},
```

No `.await` on a full channel anywhere in that arm (I1).

`submit_order_http` calls the same function with a synthetic
`ExecLanes::for_http()`, constructed FRESH PER REQUEST and dropped with it.
Three details the name alone does not settle:

- Its budgets are sized at `EXEC_HELD_BUDGET_BYTES` and `ADMISSION_LANE_FRAMES`
  like any other, not at `usize::MAX >> 3`. A saturating sentinel would have to
  survive the `u32` conversion in 5.6 and would make the HTTP path the one
  place the reservation arithmetic is untested; a per-request budget can never
  refuse a single command anyway, because a fresh budget is empty.
- It holds a real receiver for both lanes, kept alive in the same scope, and
  the request drops the whole `ExecLanes` on return. Sending into a lane whose
  receiver was never created returns `Err` and would strand the forgotten
  permits forever - harmless in a per-request value, a monotonic leak in a
  process-wide one, which is exactly why per-request is specified rather than
  left to the implementer.
- `NotAdmitted` on this path is unreachable by construction; it is mapped to a
  500 with the reason, not silently unwrapped.

The events still come back in the response body as today. The HTTP order path has no pump, no socket and
no backlog, so there is nothing for admission control to protect there;
refusing on it would be inventing a fault. This is stated in `havoc.md`
alongside the existing "these windows live only in the /ws writer" paragraph.

**Subscribe.** The frame-level `send_exec_protocol_error` call sites (the
below-origin and beyond-sim-now `start_ts` reconciliations, the unfireable
`ReopenGap` strip) become:

```rust
let Some(ticket) = lanes.reserve_admission() else {
    break CloseSpec::overload(...);
};
lanes.emit_admission(ticket, ServerMessage::ProtocolError { .. });
```

The PER-SYMBOL sites (unknown symbol, `replay_permits` exhaustion) do NOT each
take a ticket. They push onto a per-frame `refused: Vec<Symbol>` accumulator and
`continue` as today; after the symbol loop, if `refused` is non-empty, ONE
ticket is spent on one coalesced `AdmissionRejected { subject: Subscribe {
symbols, refused_total } }` (3.3a). This is what keeps a 256-symbol subscribe
of unknown symbols from closing a connection the `S22a` contract in
`config.rs` promises stays up, and it is why the read loop cannot be driven
into overload by symbol count alone. Note the unknown-symbol arm `continue`s
BEFORE the quiesce and before the permit acquire, so a refused symbol never
consumes a promise ticket either.

And the clause `docs/protocol-problem.md` calls not-optional: **a `Subscribe`
must not quiesce and replace an existing replay until capacity for that
replay's possible diagnostics is reserved.** Implemented as:

```rust
// One PROMISE ticket per replay, drawn from `ADMISSION_PROMISE_TICKETS` (3.3a)
// - NOT from the 64-slot queued-frame budget, which a long-lived promise would
// starve: 64 healthy replays would otherwise leave no room for any actual
// refusal, and the 65th subscribe would close a connection whose priority lane
// is empty. Reserved BEFORE the quiesce, moved into the spawn, released by the
// thread on exit if it never fires. The replay can emit at most one diagnostic
// in its life (the dead-seek), so one ticket covers it exactly. Reserving
// after the quiesce would mean the old stream is already destroyed when the
// refusal is discovered - the client would have lost a live feed to a capacity
// problem it was never told about.
let Some(diag_ticket) = lanes.reserve_promise() else {
    break CloseSpec::overload("subscribe diagnostic capacity exhausted");
};
let resume_floor = if let Some(old) = replays.remove(&symbol) { ... };
```

`ReplaySpawn` swaps `exec_tx: mpsc::Sender<(Instant, ServerMessage)>` for
`lanes: ExecLanes` plus `diag_ticket: Option<Ticket>`. `send_cancellable` is no
longer used for diagnostics at all, only for market data into `tx`, where it
stays exactly as it is.

Spending a promise: `emit_admission` converts the promise into a queued frame,
which re-checks the 64-frame queue budget. If the queue is full the thread
takes the same overload path any producer takes (signal the read loop to close);
it does not block, and it does not drop the diagnostic silently. A promise
guarantees the venue has ACCOUNTED for the diagnostic, not that the queue can
never be full - conflating the two is what made the single-pool design wrong.

The one-diagnostic-per-replay claim carries the ticket's whole correctness, and
it rests on the unknown-symbol branch being pre-filtered by the handler before
the spawn - two distinct sites in `spawn_replay` can emit, and only the
dead-seek one is reachable in production. Pin it: `Ticket` is single-use by
type (spending it consumes it), and the second site carries a `debug_assert`
plus a `tracing::error!` fallback if it is ever reached with the ticket already
spent. A silent under-cover if that pre-filtering regresses is exactly the
class of bug this spec exists to eliminate.

**Undecodable frames.** Same reserve-or-close shape, emitting
`AdmissionRejected { subject: Frame, .. }` (3.2). Note this replaces the
current `ProtocolError` for the decode failure specifically: a frame that did
not decode is admission truth with a name for it now.

**Market data.** Replay threads serialize their own `TickEvent` into an
`OutboundFrame` and `send_cancellable` it into `tx` as before. No budget, no
reservation - market data never enters either lane, and its backpressure story
(bounded `tx` + cancel-aware retry on its own thread) is unchanged and correct.

**Teardown.** Unchanged in shape: cancel and join replays, abort the heartbeat,
abort the pump, drop `tx` and `prio_tx`, await the writer. The overload close
path breaks the read loop with a `CloseSpec`, sends `Outbound::Close` on the
priority lane (which is why the close does not need a lane slot - it is sent
after the read loop has already stopped taking new work, and the writer stops
on it), then falls into that same teardown.

### 5.10 `mogwai-adapter`

**How this crate is read and how it is built** (required by `AGENTS.md` of any
spec touching the nautilus or broadarrow APIs, and absent from the first draft
of this one). The implementer READS the nautilus and broadarrow APIs from the
in-tree snapshots `research/nautilus_trader` and `research/broadarrow`, which
are read-only reference and never a build input. The crate BUILDS against the
published nautilus crates pinned in `crates/mogwai-adapter/Cargo.toml` with
default features off, no pyo3. broadarrow is never a build input at all.

That statement does not currently describe the tree: the manifest pins
`nautilus-common`, `-core`, `-live`, `-model` and `-network` as PATH
dependencies on a sibling `../../../nautilus_trader` checkout, which every
other document in this repo says it must not. Reconciling that is NOT this
spec's work and must not be folded into L4 - it changes what the whole
workspace builds against and would confound every gate here. The landing
sequence assumes the manifest as it is; whoever converts it to crates.io pins
does so as its own change, and this paragraph is the note that the discrepancy
is known rather than overlooked. Where the vendored snapshot and the eventual
pinned version diverge, the pinned version is what compiles.

`client/exec.rs::handle_exec_message` gains:

```rust
ServerMessage::AdmissionRejected { subject, reason, ts_event } => match subject {
    AdmissionSubject::Submit { client_order_id } => // nautilus OrderRejected,
        // via the exact path the existing ServerMessage::OrderRejected arm
        // uses, including its A.11 unknown-mirror log-and-drop.
    AdmissionSubject::Cancel { client_order_id } => // nautilus OrderCancelRejected
    AdmissionSubject::Modify { client_order_id } => // nautilus OrderModifyRejected
    AdmissionSubject::Query { request_id, query } => // drop the pending waiter
        // for request_id from the map `query` names - `pending.orders` for
        // QueryKind::Orders, `pending.fills` for Fills - so the requester
        // fails fast instead of waiting out its query timeout for a reply the
        // venue said it will never send. The discriminator is why this cannot
        // wake the wrong waiter: the two maps are separate and the protocol
        // does not make request ids unique across them.
    AdmissionSubject::Subscribe { .. } | AdmissionSubject::Frame =>
        tracing::warn!(...),
},
```

The three order arms reuse the existing helpers (`wire_client_order_id`,
`with_order_record`) unchanged, so the mirror's terminal-state guards and the
A.11 limitation behave identically to a venue-originated rejection - which is
correct, because from nautilus's point of view a refused submit and a rejected
submit are the same thing: the order never reached the venue's book.

`client/data.rs` gains an `AdmissionRejected` arm mirroring its existing
`ProtocolError` arm (log; a data client has no order events to raise).

`client/shared.rs::delay_for` needs no change - it dispatches through
`category()`, which now answers `Admission`, and `LatencyModel::delay_for`
grew that arm in 5.2.

`Subscribe`'s per-symbol fan-out is NOT touched. That is workstream B.

## 6. Verification

Every brick's gate, with the exact command.

### 6.1 New tests this spec builds

| test | crate | what it pins |
| --- | --- | --- |
| `admission_frames_fit_their_ceiling` | `mogwai-protocol` | every `AdmissionSubject` shape - including the widest, a coalesced `Subscribe` carrying `MAX_REFUSED_SYMBOLS_LISTED` symbols - with ids, symbols and reason at their caps AND filled with characters serde escapes to `\uXXXX`, serializes under `ADMISSION_FRAME_MAX_BYTES`. Plain-ASCII fixtures would pass a bound that is 6x too small. This is what makes the priority lane's FRAME count a memory bound (3.3). |
| `admission_rejected_round_trips` | `mogwai-protocol` | serde round-trip of every subject variant, byte-identical both directions (the wire-protocol gate the contract requires). |
| `admission_is_not_execution_for_delay_purposes` | `mogwai-protocol` | `AdmissionRejected` and `ProtocolError` report `EventKind::Admission`, `is_execution()` is false, `is_market_data()` is false. Extends the existing `server_message_category_is_shared_source_of_truth`. |
| `worst_case_reservation_covers_actual_output` | `mogwai-engine` | for a matrix of books (empty, one open order, 500 open + 500 closed, 1000 fills, multi-currency accounts) crossed with every command shape including the divergence-armed worst cases (`DuplicateNextFill` + `PartialFillNext` + IOC, and an armed `RejectNextSubmit` at a full `MAX_REASON_LEN`), plus a first fill in a fresh pair (which adds TWO balance rows) and adversarially-escaped ids and symbols, the sum of `serde_json::to_string(ev).len()` over `Engine::process`'s real output is `<= worst_case_output_bytes(cmd, &shape)`. The bound is a claim about the engine that the per-constant derivations (5.4) argue and this instrument checks - it samples the bound, it does not by itself prove it. |
| `reservation_failure_leaves_engine_state_untouched` | `mogwai-server` | with a budget sized so a submit cannot be reserved, the submit produces an `AdmissionRejected` and a subsequent `QueryOrders` shows the book unchanged - no venue order id burned, no order resting (I2). |
| `admission_refusal_is_not_held_by_delay_acks` | `mogwai-server` | arm a 30 s `DelayAcks`, saturate the held budget, send one more submit: its `AdmissionRejected` arrives within 1 s while the held execution events stay held (I4). |
| `admission_lane_overload_closes_with_a_reason` | `mogwai-server` | a client that never reads, driven past `ADMISSION_LANE_FRAMES` refusals, receives a close frame with `CLOSE_ADMISSION_OVERLOAD` and a non-empty reason, rather than a silent stall (I6). |
| `subscribe_reserves_diagnostic_capacity_before_quiescing` | `mogwai-server` | with the PROMISE pool pre-exhausted, a resubscribe of a live symbol does NOT tear down the in-flight replay: the connection closes with the overload reason and the pre-existing stream is never quiesced (the not-optional clause). |
| `held_budget_is_returned_on_write_and_on_disconnect` | `mogwai-server` | after N submits fully drain, the budget is back to `EXEC_HELD_BUDGET_BYTES` (no leak); after an abort-teardown mid-delay, the connection's budget is dropped with it. |
| `oversized_client_ids_are_refused_at_the_boundary` | `mogwai-protocol` + `mogwai-server` | an over-length `client_order_id` comes back as `OrderRejected`, not `AdmissionRejected` (5.1's deliberate split). |
| `admission_rejected_translates_per_command` | `mogwai-adapter` | `Submit`/`Cancel`/`Modify` subjects raise nautilus `OrderRejected`/`OrderCancelRejected`/`OrderModifyRejected` respectively, and a `Query` subject wakes the waiter its `QueryKind` names (with the same request id registered in BOTH maps, so a wrong-map wake is a failure) rather than leaving it to time out. |
| `armed_reject_reason_is_truncated_at_the_control_boundary` | `mogwai-server` | a `RejectNextSubmit` armed with a 1 MiB reason is stored truncated to `MAX_REASON_LEN`, and the resulting `OrderRejected` fits `ORDER_EVENT_MAX_BYTES` (5.1). Without it the size model is not an upper bound. |
| `oversized_ids_do_not_echo_at_full_length` | `mogwai-protocol` + `mogwai-server` | an 8 MiB `client_order_id` yields a rejection frame under `ORDER_EVENT_MAX_BYTES`, not an 8 MiB echo. |
| `coalesced_subscribe_refusal_keeps_the_connection_up` | `mogwai-server` | a `Subscribe` naming `MAX_SUBSCRIBE_SYMBOLS` unknown symbols yields ONE `AdmissionRejected` listing at most `MAX_REFUSED_SYMBOLS_LISTED` with the true `refused_total`, the connection stays open, and pre-existing streams keep flowing (the `S22a` contract, 3.3a). |
| `many_live_replays_do_not_exhaust_the_priority_queue` | `mogwai-server` | with more than `ADMISSION_LANE_FRAMES` live replays on one connection, an admission refusal still queues and delivers - promises and queue depth are separate pools (3.3a). |
| `writer_does_not_spin_when_one_lane_closes` | `mogwai-server` | with `tx` dropped and `prio_tx` alive, the writer parks rather than looping; asserted by measuring the task's poll count (or CPU time) over a fixed wall window (5.7). |
| `overload_close_terminates_against_a_nonreading_peer` | `mogwai-server` | a peer that stops reading entirely still sees the connection torn down within `CLOSE_GRACE`, budgets released, threads joined - the reasoned close is attempted, the teardown does not depend on it (5.8). |
| `protocol_error_reasons_are_truncated` | `mogwai-server` | an undecodable frame crafted so serde's error text embeds a megabyte of client-controlled field names, and an unknown-symbol subscribe with a `MAX_SYMBOL_LEN` symbol, both produce priority frames under `ADMISSION_FRAME_MAX_BYTES`. |

### 6.2 The existing gate flips

`delayed_acks_must_not_stall_the_socket_read_loop` loses its `#[ignore]` and
becomes the standing regression test. `saturation_witness_control_is_sound`
stays exactly as it is - it is what makes a failure of the gate readable.

Note the gate's own premise shifts slightly and the test comment must say so:
with admission control, 400 submits inside a 30 s window may now be REFUSED
rather than queued (that is the point). The witness is unaffected - it asks
whether the `Subscribe` behind the backlog was READ, and a refusal is proof it
was. The comment is updated to state that both outcomes (all 400 held, or some
refused with `AdmissionRejected`) pass, and only a silent reader stall fails.

**Existing assertions that MUST change with the reclassification**, all in L3.
The first draft named only the two saturation tests; these are the rest, and an
implementer who misses them meets them as failures rather than as steps:

- `mogwai-protocol/src/messages.rs`'s category test asserts `ProtocolError` is
  `EventKind::Exec` and `is_execution()`. Both flip to `Admission` / false.
- `mogwai-server/src/main.rs`'s saturation test carries a comment that depends
  on the old routing in prose - "the diagnostic rides the exec pump
  (`ProtocolError` is execution-category, S10)". The routing it describes is
  exactly what this spec inverts; the comment is rewritten, not deleted, to say
  the diagnostic now rides the priority lane and why that is a stronger witness
  (it can no longer be late for the reason the test is probing).
- `ws.rs::is_execution_event` and its own tests: the helper delegates to
  `category().is_execution()`, so it silently changes answer for
  `ProtocolError`. Its call site is the assertion that order-entry events are
  never market data, which still holds; the helper's doc comment and tests are
  updated to state that admission frames are neither exec nor data.
- The comments at the undecodable-frame and per-symbol subscribe sites in
  `ws.rs` all narrate "routed through the exec pump ... honors DelayAcks (S10)".
  That narration becomes false in L3 and is rewritten with the lane rationale.

### 6.3 Commands

Per-landing gates, copy-pasteable:

```
brokkr check
brokkr test -p mogwai-protocol admission_frames_fit_their_ceiling
brokkr test -p mogwai-protocol admission_rejected_round_trips
brokkr test -p mogwai-engine worst_case_reservation_covers_actual_output
brokkr test -p mogwai-server delayed_acks_must_not_stall --debug
brokkr test -p mogwai-server saturation_witness_control_is_sound --debug
brokkr test -p mogwai-server admission_refusal_is_not_held_by_delay_acks --debug
brokkr test -p mogwai-server admission_lane_overload_closes_with_a_reason --debug
brokkr test -p mogwai-server subscribe_reserves_diagnostic_capacity_before_quiescing --debug
brokkr test -p mogwai-server coalesced_subscribe_refusal_keeps_the_connection_up --debug
brokkr test -p mogwai-server many_live_replays_do_not_exhaust_the_priority_queue --debug
brokkr test -p mogwai-server writer_does_not_spin_when_one_lane_closes --debug
brokkr test -p mogwai-server overload_close_terminates_against_a_nonreading_peer --debug
brokkr test -p mogwai-server protocol_error_reasons_are_truncated --debug
brokkr test -p mogwai-server armed_reject_reason_is_truncated_at_the_control_boundary --debug
brokkr test -p mogwai-adapter admission_rejected_translates_per_command
```

The `--debug` flag on the server tests is deliberate: they are
subprocess/socket lifecycle tests where release-LTO compile time dominates and
optimization level does not change the behavior under test.

End-to-end, for L3:

```
brokkr run -p mogwai-server -- serve
python3 scripts/smoke.py
```

`smoke.py` is run UNCHANGED first, as a no-regression gate on the live WS and
control-plane path: serialization moved from the writer to the producers and
the writer gained a `select!`, both of which could break ordering or the havoc
gates in ways no unit test sees. It then gains one new step, after the existing
`DelayAcks` step: arm a 30 s `DelayAcks`, submit until an `AdmissionRejected`
comes back, assert it arrived while order acks were still outstanding, then
`ClearDivergences` and assert the held acks all arrive. That step is the
end-to-end proof of I4 and I5 together.

### 6.4 Data-loader gates

None. This spec does not touch `mogwai-data`, the streaming Kraken reader, or
the k-way merge. The one adjacent change - replay threads now serialize their
own frames - is a CPU shift off the writer onto threads that already own a
core's worth of generation work, and it changes no memory profile. No
throughput measurement is required or claimed.

### 6.5 No proceed/close threshold applies

This spec is not justified by an estimated volume or throughput win, so the
"measurement first, threshold second" clause does not bind it. Its justifying
measurement already exists and is already recorded: the failing gate. Per
`docs/protocol-problem.md`, reachability prices urgency and does not decide
correctness - the venue breaking its own documented `DelayAcks` semantics under
a load the protocol nowhere forbids is a contract violation regardless of
whether broadarrow currently produces that load.

## 7. Landing sequence

Each landing is one coherent intrusive change, kept or reverted on its gates.
The suite is green at every boundary.

**L1 - protocol: identifier caps.** Section 5.1. Additive and self-contained:
new constants, new validators, length checks in the existing validators.
`process_order_cmd` adopts `validate_client_order_id` / `validate_request_id`;
the read loop's `Subscribe`/`Unsubscribe` arm adopts `validate_symbols` (5.1 -
subscribes never reach `process_order_cmd`). The `/control/divergence` handler
adopts `truncate_reason`, and `config.rs` gains the `MAX_CURRENCY_LEN` check on
configured currencies, bases and quotes. Gates: `brokkr check`,
`oversized_client_ids_are_refused_at_the_boundary`,
`oversized_ids_do_not_echo_at_full_length`,
`armed_reject_reason_is_truncated_at_the_control_boundary`.
Revert condition: an existing test or `scripts/smoke.py` uses an id longer than
`MAX_CLIENT_ID_LEN` - fix the fixture, do not raise the cap silently.

**L2 - protocol: the admission category and variant, additively.** Sections
5.2, 5.3 and 5.4, EXCEPT the `ProtocolError` reclassification. `EventKind`
gains `Admission`, `ServerMessage` gains `AdmissionRejected`,
`LatencyModel::delay_for` gains its arm, `sizing` lands whole. Server and
adapter gain compile-through match arms that log. `ProtocolError` stays
`EventKind::Exec` and stays on the pump for this landing, so no behavior
changes and no route/classification mismatch ever exists. `Engine::book_shape`
lands here too, ahead of its consumer: the sizing model is a claim ABOUT the
engine, and landing the formulas in one commit and their proof in the next
means the constants sit unverified across a boundary the suite is supposed to
be green at. Gates: `brokkr check`, `admission_frames_fit_their_ceiling`,
`admission_rejected_round_trips`, `admission_is_not_execution_for_delay_purposes`
(the `ProtocolError` half of that test is added in L3),
`worst_case_reservation_covers_actual_output`.

**L3 - server + adapter: the rewrite.** The whole of
`admission.rs`, `OutboundFrame`/`Outbound`/`CloseSpec`, the writer `select!`,
`ExecLanes`, the pump's new signature, `process_order_cmd`'s restructuring,
every read-loop call site, `ReplaySpawn`'s ticket, the `ProtocolError`
reclassification (which lands HERE, with the routing that makes it true), the
un-ignoring of the gate, AND the adapter translation of section 5.10. This is
the large one and it is deliberately not split: a half-migrated read loop with
one lane and one budget is not a state the suite can be green in.

The adapter translation was originally a separate L4, and that was a mistake
worth stating so it is not re-proposed. L3 is the landing where the SERVER
starts emitting `AdmissionRejected` in anger. With translation deferred, every
capacity refusal between the two landings is merely logged by the adapter: a
refused submit leaves a nautilus order stuck in SUBMITTED forever, a refused
query leaves its waiter to time out. That is a semantically broken boundary in
a sequence whose whole premise is "the suite is green at every boundary", and
green-compiles is not the standard - green-BEHAVES is. Emission and translation
land together.

Gates: everything in 6.3 including `admission_rejected_translates_per_command`,
plus `smoke.py` unchanged and then extended.
Revert condition: the gate still fails, or the control goes red, or `smoke.py`
regresses on ordering or the havoc windows.

Documentation (section 8) rides L3 - the `havoc.md`, `architecture.md` and
adapter-facing edits all land with the behavior they describe. No markdown
lands alone.

## 8. Documentation that moves with the code

Not optional; a venue that does something deliberate it has not admitted to is
the failure this project defines itself against.

- **`reference/havoc.md`**, "Server-owned (temporal windows and the clear
  control)": the `DelayAcks` entry must state what it currently says only by
  implication - it holds DELIVERY of engine output, never ADMISSION, and never
  the priority lane. Add the admission-control behavior, `AdmissionRejected`
  and the category rationale for its exemption (`EventKind::Admission` is not
  engine output, so the knob that holds engine output does not reach it), the
  `CLOSE_ADMISSION_OVERLOAD` close (including its `CLOSE_GRACE` bound and the
  forced teardown behind it, 5.8), and the note that `ProtocolError` is no
  longer held. Extend the existing "these windows live only in the /ws writer"
  paragraph with the HTTP order path's per-request lanes (5.9). Document
  alongside `RejectNextSubmit` that its operator-supplied `reason` is truncated
  to `MAX_REASON_LEN` at the arming boundary and why (5.1) - an operator whose
  long reason comes back clipped must be able to find that stated, not infer
  it. State also the one honest exception to I3: `GoDark` still drops produced
  execution frames, and admission control does not change that.
- **`reference/architecture.md`**, under "mogwai-server": the session diagram
  is now writer + two lanes + budgets, serialization at the producer, and the
  close path. Under "mogwai-protocol": `AdmissionRejected`, `EventKind::Admission`,
  the identifier caps, and `sizing`.
- **`mogwai-protocol`'s `ProtocolError` doc comment**: gains the lane and
  category rationale. Its "untargetedness is deliberate" paragraph is NARROWED,
  not inverted, per 5.3 - the inversion is workstream B's.
- **`docs/protocol-problem.md`**: workstream A's open questions are answered
  here, so that section is edited down to point at this spec rather than
  restating open items that are now closed. Problems 3 and 4 and workstream B
  stay untouched.
- **`docs/todo.md`**: the originating item is already superseded by
  `docs/protocol-problem.md`; no further edit.

## 9. Stopping rule

The teardown stops at the session's outbound machinery and the wire vocabulary
needed to describe a refusal. Explicitly OUT of scope:

- The `Subscribe` wire shape: `symbols`, one `start_ts`, one `regime` stay as
  they are. Per-entry generation ids and per-entry cursors are workstream B.
- The adapter's per-symbol subscribe fan-out (problem 4). Workstream B.
- `ProtocolError`'s untargetedness (problem 3). Narrowed here, closed by B.
- Success acknowledgments for subscriptions (3.5).
- The engine's matching, divergence injection, account model - untouched
  beyond the additive `book_shape` accessor.
- `mogwai-data` entirely.
- The HTTP order and history surfaces, other than threading the unbounded
  lanes through `submit_order_http`.
- Any input-rate ceiling on the protocol. `docs/protocol-problem.md` names
  documenting-and-enforcing a rate limit as the OTHER honest way to close
  problem 1; this spec takes the first way (make the pump not stall) and
  deliberately does not take the second. Admission refusal is a capacity
  answer, not a rate limit: it fires on produced-output backlog, never on
  request frequency.

## 10. References

- `reference/technical-implementation-spec.md` - the contract this document is
  written against.
- `docs/protocol-problem.md` - the source: problem 1's measurement, the five
  commitments of workstream A, the rejected options, and the open list this
  spec settles in section 3.
- `reference/havoc.md` - the `DelayAcks` contract, the per-event deadline
  behavior, and the honest-content invariant that rules out shedding produced
  execution truth.
- `reference/architecture.md` - the session/writer/pump structure and the wire
  types, both of which this spec rewrites.
