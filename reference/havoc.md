# mogwai havoc model

How mogwai produces the messy, realistic divergences that are its reason to
exist, by surface. This is the durable companion to the wire-type doc comments
(which carry the local field semantics) and to git history (which carries the
landing-by-landing changelog). The open work lives in `docs/todo.md`;
`reference/architecture.md` describes the system as a whole and gives the havoc
model a one-page summary - this document is the long form.

mogwai is a fake broker/exchange that plugs into broadarrow to exercise the
*live* trading path. An in-process backtest sandbox cannot structurally produce
partial fills, rejects, ack delays, duplicate fills, dropped account updates,
venue blackouts, or a venue that acks a subscription and then silently delivers
nothing while the socket stays healthy. mogwai can, because it is an external
process speaking the native JSON-over-WS protocol over a real transport. The
havoc model is the catalog of those pathologies and the machinery that arms and
applies them.

## Honest by default

The first principle is that **nothing perturbs the stream unless broadarrow asks
for it.** By default the running server serves a realistic, honest live feed:

- Market data is the clean synthetic stream `mogwai_data::GeneratedSource`
  produces off the committed fingerprint, with no regime perturbation - a `None`
  regime is a byte-identical clean draw (pinned by the data crate's
  `clean_regime_is_byte_identical` test).
- Replay is **paced to wall-clock** at `speed = 1.0` (the built-in `Config`
  default and the committed `mogwai.toml`): the server sleeps the real
  inter-arrival gap between two ticks, capped by `gap_cap_ms` (default 1000 ms).
  `speed = 0.0` is the explicit unthrottled firehose for fast local iteration;
  the smoke test needs 1.0 and races under 0.0.
- Every inbound event still carries a modest **baseline network latency**:
  `mogwai_protocol::BASELINE_LATENCY` is 30 ms one-way (`base_nanos =
  30_000_000`), applied by the adapter's `HavocFilter` to every inbound frame
  regardless of armed havoc. The no-havoc path is not a zero-latency path - a
  real network always has delay.
- The connection lifecycle defaults (`ConnHavoc::default`) are a
  production-shaped reconnecting transport: 1 s initial reconnect backoff, 10 s
  ceiling, factor 2.0, no jitter, unlimited attempts, no idle-death detection,
  no client heartbeat, no HTTP quota, and the 30 s request timeout
  (`request_timeout_secs == 0` documents "keeps 30s", sourced from
  `DEFAULT_REQUEST_TIMEOUT_SECS`).
- The server heartbeat is off (`server_heartbeat_ms = 0`).

Every havoc surface layers **on top of** this honest floor. Armed
`ClientHavoc.latency` *adds* to the 30 ms baseline rather than replacing it;
drop, duplicate, and reorder are opt-in and default off; an unset `data` regime
is a clean draw; the `conn` default is a working transport. The desirable
direction (per the `docs/todo.md` Direction note) is richer, more realistic
venue pathologies - not making the fake venue more correct - and the lodestar is
a real venue misbehaving in a way the client cannot detect.

## One HavocSpec, four surfaces

A single `mogwai_protocol::HavocSpec` value is carried on both adapter client
configs (`MogwaiDataClientConfig` and `MogwaiExecClientConfig`, in
`mogwai-adapter`'s `config.rs`, as `havoc: Option<HavocSpec>`). The per-venue
value broadarrow constructs lives in broadarrow, not in this repo; broadarrow
sets it on the `MOGWAI` venue arm. `None` is a clean adapter (but still the
honest baseline latency and a working transport, per above).

`HavocSpec` has four fields, one per surface:

```text
pub struct HavocSpec {
    pub client: ClientHavoc,            // transport corruption, in the adapter
    pub server: Vec<control::Divergence>, // execution divergences, relayed to the server
    pub data: Option<MarketRegime>,     // generator perturbation, before ticks exist
    pub conn: ConnHavoc,                // connection-lifecycle corruption, in the adapter
}
```

The whole spec is range-checked once, at adapter factory `create` time, by
`Mogwai*Config::validate` -> the shared `validate_havoc` helper, which runs
`validate_client_havoc`, `validate_conn_havoc`, `validate_market_regime` (on
`data`), and `validate_divergence` on each entry of `server`. A spec that fails
any boundary never constructs a client. The four surfaces then reach the running
system by different routes, described below.

### Client surface - transport corruption in flight

`HavocSpec.client` is a `ClientHavoc`: a seeded `HavocFilter` (in the adapter's
`client.rs`) that corrupts the inbound `ServerMessage` stream after it is
received but before it reaches the nautilus runner's egress sink. It is threaded
through every drain path - the data and exec WS readers, the HTTP polling loop,
and the per-dispatch exec-over-HTTP path - so the same corruption model applies
regardless of transport profile. Knobs:

- `latency: Option<HavocLatency>` - static added delay. `HavocLatency` carries
  `base_nanos` plus per-category extras (`exec_event_nanos`, `fill_nanos`,
  `data_nanos`); `delay_for(kind)` composes `base + extra` for the event's
  category, mirroring nautilus's `StaticLatencyModel` base-plus-op composition.
  The filter computes the effective delay as `BASELINE_LATENCY.delay_for(cat) +
  armed.delay_for(cat)`, so the armed delay always rides on top of the 30 ms
  floor. Each of the four fields is bounded to `[0, MAX_LATENCY_NANOS]` (60 s)
  by `validate_client_havoc`, so an armed latency stays in the
  pathological-but-plausible network band rather than wedging the stream with a
  multi-century delay.
- `drop_prob`, `duplicate_prob`, `reorder_prob` - per-event probabilities in
  `[0.0, 1.0]` (range-checked by `validate_client_havoc`). Drop discards an
  inbound event; duplicate emits it twice; reorder holds one event back and
  transposes it with the next adjacent event (the held event is released when
  the next one arrives, or flushed on stream-close).
- `seed: Option<u64>` - optional deterministic RNG seed for the three
  probabilistic knobs; absent, the filter draws from entropy.

The added delay is delivered as **parallel pipeline latency**, not inter-message
spacing. Each inbound frame's release deadline is anchored at its own arrival
(`arrival + delay`) and paced off the reader loop: the streaming drains (the
data and exec WS readers) enqueue into a per-connection latency pump, and the
HTTP poll drain anchors a whole fetched page at its fetch instant. Both mirror
the mogwai server's own `spawn_exec_pump` deadline discipline. A burst therefore
drains at full throughput with every frame delayed by `delay` - the way a real
network delays frames in parallel - rather than serializing into a `1/delay`
message-rate ceiling that would also head-of-line-block Ping/Pong and outbound
commands behind the backlog. The reorder and duplicate expansions ride the same
pump, so their timing composes with the latency. (The short per-order
HTTP-dispatch response drain keeps a simple inline sleep: a handful of
causally-ordered events on a dedicated task, with no select loop or ping to
block.)

The event-category split is the protocol's single source of truth:
`ServerMessage::category()` returns `EventKind::{Exec, Fill, Data}`, and
`EventKind::is_execution()` is the two-way exec-vs-data fold. `AccountState` is
classified `Exec` (it reports balances and positions moved by fills), trades and
quotes are `Data`, fills are `Fill`, and the server heartbeat rides the `Data`
latency bucket but is *not* market data (`is_market_data()` is narrower than
`category() == Data`). Both ends consult this one classifier so a frame can
never be data on one end and execution on the other.

### Server surface - execution divergences relayed to the venue

`HavocSpec.server` is a `Vec<control::Divergence>`. The **execution** client
ships them to the running server on connect: `ship_server_havoc` POSTs one
divergence per entry to `/control/divergence` (so broadarrow makes no separate
control-plane call - arming is folded into connect). The **data** client carries
the same field but ships nothing server-side: divergences are execution-owned.
The divergence catalog and its server/engine split are the subject of the next
two sections.

### Data surface - the market before it is produced

`HavocSpec.data` is an `Option<MarketRegime>` that corrupts the *market before
the ticks exist*: it perturbs the generator's parameters at
source-construction time, rather than corrupting events after they have been
produced. It does not travel the `/control/divergence` plane (that plane arms
global, connection-scoped state; a regime must instead be baked into the
`GeneratedSource` a subscription spins up). It rides per subscription on the
`Subscribe` entry (`SubscriptionRequest { regime, .. }`) and per request on
`GET /trades` (the `regime` query parameter). The `MarketRegime` axis is the
subject of its own section below.

### Connection-lifecycle surface - the transport machinery itself

`HavocSpec.conn` is a `ConnHavoc` that perturbs the adapter's own transport
machinery rather than any event payload: reconnect backoff and attempt caps,
client heartbeat pings, idle-timeout death detection, HTTP request quotas, and
per-request order timeouts. Every field mirrors a nautilus adapter config knob
(`WebSocketConfig` reconnect/idle/heartbeat fields plus per-adapter quota and
timeout fields). It is consumed entirely inside the adapter's `lifecycle.rs`:

- `reconnect_delay_initial_ms` / `reconnect_delay_max_ms` /
  `reconnect_backoff_factor` / `reconnect_jitter_ms` -> `ReconnectPolicy`. The
  backoff grows exponentially from the initial, clamps at the ceiling (a `max ==
  0` means "no clamp", not "clamp to zero"), and adds uniform jitter seeded from
  `ClientHavoc.seed` for reproducibility.
- `reconnect_max_attempts: Option<u32>` -> `ReconnectPolicy::exhausted`; `None`
  is unlimited. The attempt counter counts CONSECUTIVE UNPROVEN connection
  cycles: it resets only once a connection proves itself by delivering an
  inbound application frame (Text/Binary - the same liveness criterion the
  idle timeout uses; Ping/Pong do not count), not on a successful dial. The
  backoff sleep applies after ANY disconnect - a failed dial, a peer close, a
  read error, an idle-timeout death - so an accept-then-die venue walks the
  exponential ladder and eventually trips the cap instead of re-dialing in a
  hot loop, while a proven connection that later drops re-dials after the
  initial backoff with a fresh count.
- `heartbeat_interval_ms` -> a Tokio interval that fires a WS Ping each period
  (first ping one interval after connect, not at `t=0`). `0` disables it. This
  is the *client*'s ping cadence, distinct from the *server*'s
  `server_heartbeat_ms` liveness frame.
- `idle_timeout_ms` -> an idle read deadline reset only by inbound application
  data; Ping/Pong frames do not reset it. If it fires, the socket is declared
  dead and reconnected. `0` disables detection.
- `max_requests_per_second: Option<u32>` -> `HttpQuota`, ceil-dividing one
  second into a minimum per-request spacing so the effective rate stays at or
  below the cap. `None` is unlimited.
- `request_timeout_secs` -> per-request timeout for HTTP order entry; `0` keeps
  the 30 s default.

A clean `ConnHavoc::default` is a real reconnecting transport; hostile values
make the adapter behave like a venue connection with broken lifecycle settings.

Every lifecycle transition logs, tagged with a `socket` field naming the
connection (`data` / `exec`): a failed dial and the disconnect cause (peer
close, read error, idle timeout, writer death) at WARN, each backoff before a
re-dial and every successful connect at INFO (a nonzero `attempt` on the
connect line marks a re-attach after an outage), and attempt-cap exhaustion -
which permanently stops the socket - at ERROR. A venue outage and recovery is
therefore readable from the log after the fact instead of only inferable from
bars resuming. Deliberately NOT logged: individual heartbeat pings and pongs
(at a 1 s cadence they would out-shout the events that matter).

## The divergence catalog

`control::Divergence` (in `mogwai-protocol`'s `control` module) is the catalog
the `server` surface arms. There are nine variants, split by *who owns them*:
the engine owns the single-shot, order-triggered divergences; the server owns
the temporal, connection-scoped windows plus the clear control; and one
immediate-action variant (`CancelOpenOrderSilently`) acts on the book the
moment it is posted rather than arming anything. Every variant, its trigger,
and its semantics:

### Engine-owned (single-shot, order-triggered)

These fire synchronously inside `mogwai_engine::Engine::process` as orders flow,
consuming themselves from an armed queue on their trigger. The engine's
`take_armed` scans for the first *applicable* armed entry (it does not just peek
the queue front), so a divergence whose trigger has not yet arrived does not
head-of-line-block the ones behind it.

- **`PartialFillNext { client_order_id, fraction }`** - fills only `fraction` of
  the named order on its next submit, leaving the remainder resting open as a
  partial fill. Targeted: it applies only to the order whose id it names, so a
  partial armed for a not-yet-seen order stays armed without blocking others.
  `fraction` is validated to `(0, 1]`; the engine additionally clamps
  defensively at runtime (a `fraction > 1` becomes a full fill, a
  `fraction <= 0` becomes a full fill with a warning).
- **`RejectNextSubmit { reason }`** - rejects the next submitted order outright
  with `reason`, emitting an `OrderRejected` and nothing else. Untargeted: it
  applies to whatever submit arrives next. The operator-supplied `reason` is
  TRUNCATED to `MAX_REASON_LEN` (512 bytes, on a char boundary) by the
  `/control/divergence` handler before the divergence is stored, so an armed
  reason that comes back clipped is doing what it says here. The truncation is
  not cosmetic: the engine echoes the reason verbatim into `OrderRejected`, and
  a connection's admission reservation sizes that frame against
  `sizing::ORDER_EVENT_MAX_BYTES`, which charges exactly `MAX_REASON_LEN` for
  it. An uncapped reason would make produced output exceed its own reservation
  and void the size model.
- **`DuplicateNextFill`** - emits the next fill event twice on the wire (same
  `trade_id`, `last_qty`, `last_px`, `leaves_qty`), so a naive downstream
  client would double-count. Untargeted: applies to the next fill produced.
  Operator note: against the nautilus adapter stack this can never corrupt
  state - the adapter's mirror dedups on `trade_id` and nautilus itself
  warn-drops the duplicate before the portfolio sees it - so what it exercises
  is the dedup path and its logging, not an account divergence. Corrupting a
  robust OMS would need distinct trade ids, which is a genuine double-fill
  (overfill), a different divergence. The same applies to the client-side
  `duplicate_prob` when the duplicated event is a fill.
- **`DropNextAccountUpdate`** - swallows the next fill-driven `AccountState`
  snapshot, so the account drifts out of sync with the fills the client did see.
  Untargeted: applies to the next submit's snapshot.

  The drift is DURABLE across a transport reconnect, deliberately. The adapter
  pulls `GET /account` when the execution client connects (and again on any
  nautilus-driven reset or reconnect, which calls `connect` afresh), but the
  transport's OWN internal reconnect replays websocket subscriptions only and
  does not re-pull the account. A dropped snapshot therefore stays missing from
  the pushed account row until the next fill produces one, rather than being
  quietly healed by a socket bounce - which is the point, since auto-healing on
  reconnect would undo the divergence the operator armed. It also matches a real
  venue, which does not hand you a fresh account snapshot just because your
  socket came back.

  Reconciliation is NOT fooled by this: `generate_position_status_reports` pulls
  `GET /account` fresh on every call and reads venue truth regardless of how
  stale the pushed row is. The staleness this divergence creates lives in the
  consumer's account/balance view between reconciliations, which is exactly the
  surface it exists to perturb.

The engine never mutates wall-clock state and is deterministic; the server
stamps the real timestamps and owns the sockets. An amend (`ModifyOrder`)
deliberately never touches the armed queue, so an interleaved modify cannot
consume a divergence armed for a fill.

**The armed queue is BOUNDED at 1024 entries** (`MAX_ARMED_DIVERGENCES`), and
"stays armed" above is bounded by that cap, not a promise of durability. A
targeted `PartialFillNext` whose order never arrives has no trigger and would
otherwise sit armed forever, so an unbounded queue is a harness DoS; at the cap
each new arm sheds the OLDEST entry, which is exactly the accumulated
never-triggered leftovers. `clear_armed` between scenarios is the explicit
flush.

An arm that displaces an entry still answers `202 Accepted` - the arm that was
just posted really was accepted - but the response BODY names the evicted
divergence:

```
armed; the engine queue was at its 1024-entry cap, so the oldest armed
divergence was discarded to make room: PartialFillNext { ... }
```

An empty body means nothing was displaced. Read the body: an armer that posts a
large bracket of targeted partials (a coid sweep, say) and ignores it will see
its early arms silently discarded and then measure a full fill where it expected
a partial - the failure looks exactly like "armed divergences do not fire".

### Server-owned (temporal windows and the clear control)

These have no synchronous engine-side trigger; they arm atomic state on the
account's registry slot and are applied in the server's outbound writer task (in
`mogwai-server`'s `ws.rs`), gating frames as they pass through. They are
*per-account*: a `/control/divergence` POST carries the `x-mogwai-account`
header like every other stateful request, and the window it arms applies to
every live session bound to THAT account and to no other. Arming a blackout on
one fleet worker leaves the rest running clean.

- **`DelayAcks { ms }`** - holds every outbound **execution** event by `ms`
  before sending it; market data is untouched. Implemented as the arming
  account's `delay_ms` atomic, read per execution frame (`category().is_execution()`) by
  the session's exec delay pump, which anchors each frame's deadline at its
  production instant - so a whole order-entry batch, produced at one instant,
  shares one deadline and lands together `ms` late rather than serialized a
  window apart - without head-of-line-blocking market data behind the sleep.
  Arm with `ms: 0` to clear the delay, or post `ClearDivergences`. The
  heartbeat is explicitly exempt - `DelayAcks` must not perturb its cadence.

  `DelayAcks` holds **delivery of engine output**. It never holds, delays or
  gates **admission**, and it never touches the priority lane. An
  `AdmissionRejected` (and a `ProtocolError`) classifies `EventKind::Admission`,
  which is not engine output at all - it reports what the venue's request
  handling refused, clamped or could not decode - so the knob that holds engine
  output legitimately does not reach it. That exemption lives in exactly one
  place, `EventKind::is_execution()`, so the two ends cannot disagree about it.
  Concretely: with a 30 s `DelayAcks` armed and a saturated held lane, a refused
  submit's `AdmissionRejected` arrives immediately while the held acks stay
  held. `ProtocolError` used to ride the pump and be held by this window; it no
  longer is.
- **`GoDark { ms }`** - drops **everything** (market data and execution) for a
  `ms` blackout window: a total venue blackout. Implemented as a `dark_until_ns`
  absolute wall-clock deadline; the writer drops every frame while `now_ns() <
  dark_until_ns`. Frames produced during the window are **dropped, not
  buffered** - there is no backlog to replay when the window lifts. This is
  faithful to a real venue blackout (cf. issue 4255).
- **`StallData { ms }`** - drops **only market-data frames** (`Trade` / `Quote`)
  for `ms`, while every execution frame continues to flow. Implemented as a
  `stall_until_ns` deadline the writer consults only for `is_market_data()`
  frames; execution traffic and the heartbeat are unaffected. Like `GoDark`,
  stalled frames are dropped, not buffered. This is the surface that reproduces
  the issue-4255 class of failure - see the heartbeat section.
- **`ClearDivergences`** - lifts all three server-owned temporal windows of the
  arming account at once: stores `0` into its `delay_ms`, `dark_until_ns`, and
  `stall_until_ns`
  (`delay_ms == 0` skips the delay sleep; `now_ns() < 0` is never true, so the
  dark and stall guards are off). It does **not** flush the engine-side
  single-shot divergences - those self-disarm on their own trigger. There is no
  backlog to replay because gated frames were dropped.

### Admission control, and the one honest exception to honest content

A `/ws` session's outbound path is two lanes with explicit accounting. The HELD
lane carries engine output through the `DelayAcks` shift under a per-connection
BYTE budget (`EXEC_HELD_BUDGET_BYTES`, 8 MiB); the PRIORITY lane carries
admission truth under a per-connection FRAME budget (`ADMISSION_LANE_FRAMES`,
64) and is drained first. Both sizes are run config
(`exec_held_budget_bytes`, `admission_lane_frames` in `reference/config.md`);
the constants above are the defaults, and lowering them makes the venue refuse
sooner without changing what it does when it refuses - which is how
`scripts/smoke.py --admission` exercises a refusal over a live socket. Both channels are unbounded, because the bound is the
budget: that is what lets every producer on the socket read loop reserve
worst-case output BEFORE the engine mutates, and never await a full channel. A
command whose worst case cannot be reserved is refused with
`ServerMessage::AdmissionRejected { subject, reason, ts_event }` and the engine
is never asked, so nothing mutated - no venue order id burned, no order resting.
`subject` names what was refused (submit, cancel, modify, query, a coalesced
subscribe, or an undecodable frame) so a consumer can translate it per command;
a refused cancel must not read as a rejected order.

If the priority lane itself cannot take a refusal, the connection is closed with
`CLOSE_ADMISSION_OVERLOAD` (WS 1013, "Try Again Later") and a stated reason -
never a silent stall. That close is best-effort by nature: the writer may
already be parked against a peer that stopped reading, so the teardown waits
`CLOSE_GRACE` (2 s of WALL time - a transport deadline, not a sim one) and then
aborts the writer and drops the socket. The reasoned close is attempted; the
release of the connection's resources is not optional.

Admission control never drops an execution event the engine has produced.
Refusal happens BEFORE production; after production the frame is delivered,
dropped by an ARMED HAVOC WINDOW, or the connection dies. The havoc exception is
explicit and pre-existing: `GoDark` still drops produced execution frames in the
writer and `StallData` still gates market data, and admission control does not
change that. What is ruled out is a silent, capacity-driven drop.

The `ms` windows of `DelayAcks`, `GoDark`, and `StallData` are all bounded by
`control::MAX_DIVERGENCE_MS` (3 600 000 ms = one hour), so a single request
cannot arm an effectively permanent window or saturate a writer deadline.

Re-arming a window **replaces** it, it does not extend it: a second POST with a
smaller `ms` SHORTENS an in-flight `GoDark`/`StallData` rather than adding to
it (the writer stores the new absolute deadline over the old). Store-not-extend
is deliberate so a test can shorten a window it armed; arm `ms: 0` (or
`ClearDivergences`) to lift one immediately.

**These windows live only in the `/ws` outbound writer, so they apply only to
legs carried over WS.** Under `HttpOrders` the exec client's order events are
returned synchronously in the `POST /orders` response, and under `HttpPolling`
market data is fetched over `GET /trades` - neither path passes through the
writer, so an armed `DelayAcks`/`GoDark`/`StallData` has NO effect on an HTTP
carrier. Admission control is scoped the same way: the HTTP order path gets
lanes constructed fresh per request and dropped with it, so it has no pump, no
socket and no backlog for admission control to protect, and it can never refuse
a command. Refusing there would be inventing a fault. This is faithful to the connection-scoped framing of these divergences
(they model a sick socket, and the HTTP carriers do not hold one), but it used
to be a real operator trap: arming `GoDark` against a `transport_profile =
"HttpOrders"` client exercised a clean order path while the operator believed a
blackout was running. The adapter configs now REFUSE the undeliverable
combinations at create time, exactly like every other impossible spec:

- `DelayAcks` under `HttpOrders` or `HttpPolling` - order events return in the
  `POST /orders` response; there is no WS exec leg to delay.
- `GoDark` under `HttpOrders` or `HttpPolling` - the order path rides HTTP and
  stays live through the window, so the documented TOTAL blackout can never be
  total.
- `StallData` under `HttpPolling` - market data never passes the writer. It
  stays LEGAL under `HttpOrders`, whose market data still streams over WS: that
  is the deliverable data-only stall on a mixed carrier.

The refusal guards the adapter config path (`validate`, which broadarrow's
scenario loader runs before any run starts). A raw `POST /control/divergence`
still arms whatever it is sent: the windows are venue-global and the server
cannot know which transport each connected client rides, so an external armer
remains responsible for matching windows to carriers.

Operator note on long delay windows: any nautilus consumer runs an
execution-manager inflight check that re-queries a still-unacked order and,
after a bounded retry budget, synthesizes a local reject. The adapter now
answers that re-query from venue truth (`query_order` and the singular report
generator ride the `QueryOrders` wire surface - see the reconciliation-witness
section below), so an inflight order whose ack is merely delayed can resolve
instead of being locally rejected - but the query REPLY rides the execution
channel too, so the same `DelayAcks`/`GoDark` window that holds the ack also
holds the answer. A window longer than the consumer's inflight threshold
therefore still ends in the consumer's local reject, with the later real ack
landing on an already-rejected order. Arm ack-delay havoc inside the consumer's
inflight threshold to exercise the delay path; longer windows exercise the
consumer's brake (and its query-timeout handling). The specific threshold is
the consumer's to document - and per broadarrow's standing note (2026-07-31),
the band above their ~25 s inflight threshold is deliberately unserved: they
declined a per-venue ceiling on a safety timeout, so mogwai does not invest in
exercising ack delays past it.

### Immediate action - the out-of-band silent cancel

- **`CancelOpenOrderSilently { client_order_id }`** - cancels a RESTING order
  server-side the moment it is posted, emitting **no lifecycle event**: the
  lost `OrderCanceled` IS the injected fault. The engine removes the order
  from its book and frees its reservation; the client keeps believing the
  order rests until it reconciles. This is the fault class nautilus'
  continuous open-order poll exists to catch - a protective order cancelled
  out-of-band with its event dropped, a position the operator believes
  protected and is not - and it was previously unprovokable anywhere. Unlike
  the armed single-shots it has no trigger to wait for (no client action is
  involved), so the `/control/divergence` handler routes it straight to
  `Engine::cancel_open_order_silently` instead of `arm`; a post naming an id
  that is not currently resting (unknown, or already terminal) is refused
  with a 404 so a scenario cannot silently arm a no-op. The lookup is scoped
  to the arming account's book, so an id belonging to a DIFFERENT account is
  also a 404 - correct, since the arming account has no such order. The
  account snapshot
  is not pushed either - the freed reservation surfaces on the next
  fill-driven snapshot or `GET /account` pull, which is honest drift, and a
  `QueryOrders` reply reports the order `Canceled` from the instant of the
  cancel.

## The reconciliation witness - QueryOrders and QueryFills

The wire carries two reconciliation queries (`ClientMessage::QueryOrders`,
`ClientMessage::QueryFills`) answered by `ServerMessage::OrderStatusSnapshot` /
`FillSnapshot`, correlated by a client-chosen `request_id` echoed verbatim.
The engine answers them from its own book and its booking-order fill store -
each fill is recorded once as it books, so a `DuplicateNextFill`'s doubled
wire event never doubles the truth. The adapter's report generators (the
plural set that startup mass-status and the continuous open-order poll
consume, the singular one behind the inflight re-query, and `query_order`)
all ride this surface; they no longer read the adapter's client-side mirror,
which is fed by the same lifecycle stream havoc corrupts and so could only
confirm a corrupted belief.

**The design invariant: honest content, havoc-able delivery.** The query path
is the reconciliation channel, and its whole value is being a second,
independent witness while the event channel lies. No divergence may ever
alter what a reply SAYS - a havoc knob on the witness would collapse the two
channels into one adversary and make any poll-heal scenario unprovable ("the
heal works" vs "both channels agreed on the same lie" become
indistinguishable). Delivery, though, is fair game: the snapshot replies
classify as execution (`ServerMessage::category`), so `DelayAcks` holds them,
`GoDark` drops them, and client-side inbound latency/drop applies - a lost or
late reply surfaces in the adapter as a query timeout, exercising the
consumer's own query-timeout behavior. If someone later genuinely wants a
lying reconciliation source (venue-truth corruption - on a real venue that
means venue bugs), that is a different fault class deserving its own
explicitly-named havoc, never a side effect of the query transport.

The payoff scenario this enables end to end: post
`CancelOpenOrderSilently` against a resting protective order, and assert the
consumer's reconciliation poll detects the vanished order within one cycle -
a real proof of the poll heal, previously impossible against any venue.

Under acceleration these `ms` are **simulated** milliseconds. The deadlines and
the `DelayAcks` sleep are computed on the one sim axis (the `wall-clock` wording
above is exact only at `speed = 1.0`): a `GoDark { ms }` blackout lasts `ms`
simulated ms, realized in `ms / speed` wall, and the writer's drop guards compare
against sim-now. So every temporal divergence here - plus the client inbound
latency and the connection-lifecycle knobs in the surfaces above - scales with
`speed` and stays coherent with the data stream. The full knob-by-knob table and
each duration's wall lower bound live in `reference/clock.md`.

## The server/engine split and why it matters

The catalog is split deliberately, and the split is enforced at two points:

- **The engine refuses the non-armable variants.** `Engine::arm` matches
  `DelayAcks`, `GoDark`, `StallData`, `ClearDivergences`, and
  `CancelOpenOrderSilently` and **drops them** rather than queueing them. The
  temporal windows have no engine-side trigger and the silent cancel is
  immediate-action (the server routes it to `cancel_open_order_silently` at
  post time), so `take_armed` would never consume any of them - queueing them
  would leak dead entries that could silently disarm real engine divergences
  behind them. Only the four single-shot variants ever enter the armed queue.
- **The server routes them and never forwards them to `arm`.** The
  `/control/divergence` handler (`arm_divergence`) intercepts `DelayAcks`,
  `GoDark`, `StallData`, `ClearDivergences`, and `CancelOpenOrderSilently`
  before the catch-all and applies each (the windows to the shared atomics,
  the silent cancel to the engine book directly); only an engine-side armed
  variant reaches `engine.arm()`. A `debug_assert` in the catch-all makes any
  future refactor that forwarded a whole `HavocSpec.server` vec straight to
  `engine.arm()` fail loudly rather than silently lose these knobs.

The reason for the split is structural. The single-shot divergences are
deterministic functions of order flow - "the next fill", "this named order" -
and belong with the order/account state the engine owns and unit-tests
synchronously. The temporal windows are wall-clock effects on the *outbound
byte stream* - delaying, dropping, withholding frames - which the engine (clock-
free and side-effect-free by design) cannot express. The server owns the
sockets, the clock, and the writer, so the windows live there. `ClearDivergences`
and the delay/dark/stall knobs are therefore server-owned and never reach
`engine.arm()`; that is not an accident of layering but the load-bearing
boundary that keeps the engine pure.

## The heartbeat and the issue-4255 reproduction

The server can emit its own liveness frames, configured by the run-config
`server_heartbeat_ms` (in `mogwai.toml`; `0`, the honest default, disables it).
When enabled, each WS session spawns a heartbeat task (`spawn_heartbeat`) that
pushes a `ServerMessage::Heartbeat { ts_event }` into the same channel the
writer drains, on the configured cadence. The frame carries the server wall
clock (so it is non-empty and timestamp-comparable) but no market or execution
payload; clients may ignore it. It rides the channel through the single writer
so socket writes stay single-owned and ordered.

The heartbeat is distinct from the adapter's *client* ping
(`ConnHavoc.heartbeat_interval_ms`): the client ping is a WS Ping the adapter
sends; the server heartbeat is an application-data `ServerMessage` the server
sends. They serve opposite ends of the same liveness question.

How the three compose to reproduce **nautilus_trader issue 4255** (Kraken acked
an `ohlc` subscription and then silently delivered nothing while the socket
stayed healthy):

- The heartbeat is classified `EventKind::Data` by `category()` (so `DelayAcks`
  does not perturb its cadence) but is deliberately **not** `is_market_data()`.
- `StallData` gates only `is_market_data()` frames. So a `StallData` window
  withholds every `Trade`/`Quote` while the heartbeat continues to flow.
- From the client's transport, the socket stays **frame-active**: heartbeat
  frames arrive on schedule, the idle-timeout clock keeps resetting, no
  reconnect fires. Yet the *channel* the strategy subscribed to delivers
  nothing. A frame-level idle timeout structurally cannot catch this - the
  socket is healthy; only the data is missing. That is exactly the 4255 failure:
  liveness survives the stall, the data does not.

`GoDark` is the contrasting blunt instrument: it drops the heartbeat too (it
drops *everything*), so a sufficiently long `GoDark` would eventually trip an
idle-timeout reconnect. `StallData` plus the heartbeat is the subtle case the
client cannot detect at the transport layer - which is the whole point of mogwai
existing as an external process.

## The MarketRegime axis (the data surface)

`HavocSpec.data` is an `Option<MarketRegime>` carried per subscription and per
history request. It perturbs the generator *before* ticks exist, by being
decomposed at `GeneratedSource::new` into a private `RegimeState` that layers its
perturbation onto the realized output at the same point the session envelope
attaches - after the ACD arrival-clock and GARCH volatility recursions have
consumed their un-modulated feedback. So the clustering dynamics survive the
perturbation, and a `None` regime is a byte-identical clean draw. Crucially the
regime does **not** mutate the validated `GeneratorScalars` (the per-instrument
price/tick/vol knobs): a regime is a deliberately out-of-band perturbation
layered on top, while `GeneratorScalars::validate` keeps gating the in-band
scalars. The four regimes:

- **`VolStorm { vol_mult }`** - multiplies the GARCH return RMS by `vol_mult`,
  and lifts the per-update and sigma2 clamps with it (`clamp_mult = vol_mult`),
  so the storm is realized rather than clipped back by the fitted bounds. A bare
  multiply without the clamp lift would be silently clamped; the data crate pins
  this with a clamp-override test.
- **`LiquidityDrought { thin_factor }`** - divides the arrival intensity by
  `thin_factor` (stretches inter-arrival durations), producing a thin tape.
- **`SessionEdgeSpike { start_hour, end_hour, extra_vol_mult }`** - inside the
  UTC half-open hour window `[start_hour, end_hour)`, amplifies the session vol
  curve. The extra is *additive* within the regime envelope (neutral 1.0, out of
  window the edge contributes 0.0), and that result composes *multiplicatively*
  with the session envelope - so the spike amplifies the fitted session curve
  rather than additively shifting it. The window invariant (`start_hour <
  end_hour <= 24`) is enforced only by the validator and trusted at runtime.
- **`ReopenGap { at_ts, halt_secs, gap_frac }`** - the one stylized fact a 24/7
  spot tape structurally never shows. When the simulated clock crosses `at_ts`
  once, the generator advances its clock by `halt_secs` (a silent halt) and then
  resumes at a mid gapped by a signed latent log-return `gap_frac` (the resumed
  mid is the pre-halt mid times `exp(gap_frac)`). `take_reopen_crossed` fires it
  exactly once, on the crossing.

`RegimeState`'s decomposition arms are in **lockstep** with
`validate_market_regime`: they destructure the same `MarketRegime` variants the
validator range-checks, one crate away, with no compiler link forcing agreement.
A new variant or a renamed field must be mirrored in both. On the server's
ingress paths an out-of-band regime is dropped to a clean replay rather than
panicking: `validate_regime_or_clean` (the `Subscribe` path) and
`parse_history_regime` (the `/trades` path) both validate and fail closed to
`None` on an out-of-range or unparseable regime. On the `Subscribe` path the
drop is now a reported `SubscriptionIssue::InvalidRegime` degradation rather
than a silent one; the stream still runs clean, unhavocked.

The wire carries a regime per SUBSCRIPTION ENTRY (`SubscriptionRequest.regime`),
so a per-symbol regime is expressible. The adapter today sends the same value for
every entry, so behavior is unchanged and the regime is effectively venue-wide; a
genuinely per-symbol regime is now a client-side change only.

Market-data tapes are shared only when their regimes are bit-identical. The
regime is an input to the generated walk, not a post-generation filter, so a
different armed regime receives a distinct tape and cannot perturb clean or
other-regime subscribers.

## Validation boundaries

Four free validators in `mogwai-protocol` gate the havoc surfaces, one per
field of `HavocSpec`. The adapter runs all of them at config-`validate` time
(via `validate_havoc`), and the server re-runs the relevant two
(`validate_market_regime` and `validate_divergence`) on its ingress paths. They
share a finite-range idiom (`finite_in` for inclusive `[lo, hi]`,
`finite_in_excl_lo` for half-open `(lo, hi]`) so a `NaN`/inf input can never slip
a forgotten finiteness check.

### `validate_client_havoc`

Run by the adapter on `HavocSpec.client`; the client surface is adapter-applied
only, so the server never sees these knobs.

- `drop_prob`, `duplicate_prob`, and `reorder_prob` must each be finite in
  `[0.0, 1.0]` - a probability outside the unit interval (or a `NaN`/inf) is
  meaningless.
- The four `HavocLatency` delay fields (`base_nanos`, `exec_event_nanos`,
  `fill_nanos`, `data_nanos`) must each be in `[0, MAX_LATENCY_NANOS]` (60 s).
  The ceiling keeps an armed latency inside the plausible-network band - the
  honest baseline is 30 ms, a badly degraded link reaches seconds, and a frame a
  full minute late already reads as a dead connection - and stays well below the
  one-hour `MAX_DIVERGENCE_MS` window cap, since an in-flight per-event delay
  belongs below a total blackout.
- `seed` (`Option<u64>`) carries no range.

### `validate_divergence`

The authoritative gate for an armed divergence; the server runs it in
`arm_divergence` before storing or arming anything, so an out-of-range knob is
rejected with a `400` rather than surfacing as a degenerate fill downstream.

- `PartialFillNext.fraction` must be in `(0, 1]` - a fill must move some quantity
  and cannot exceed the order. (The engine's runtime clamp is a last-line net
  below this gate, not a substitute for it.)
- `DelayAcks.ms`, `GoDark.ms`, `StallData.ms` must be in
  `[0, MAX_DIVERGENCE_MS]` (`0` is the valid disarm value, `MAX_DIVERGENCE_MS`
  is one hour), so a control-plane request cannot arm an effectively permanent
  window.
- `RejectNextSubmit`, `DuplicateNextFill`, `DropNextAccountUpdate`,
  `ClearDivergences` are otherwise unconstrained.

### `validate_market_regime`

Run by the adapter on `HavocSpec.data` and by the server on both the `Subscribe`
and `/trades` regime inputs.

- `VolStorm.vol_mult` must be finite in `(0.0, 100.0]` - a `0` (or non-finite)
  multiplier is degenerate.
- `LiquidityDrought.thin_factor` must be finite in `[1.0, 1000.0]` - thinning
  cannot increase intensity below 1.0.
- `SessionEdgeSpike` - the window must satisfy `start_hour <= 23`,
  `start_hour < end_hour`, and `end_hour <= 24` (a valid half-open window over a
  24-element session curve, never empty, never indexing past the curve), and
  `extra_vol_mult` must be finite in `[0.0, 100.0]`.
- `ReopenGap` - `at_ts` must be `> 0` (the epoch is a halt that has already
  passed before the first forward-replay tick, so it would arm a silently inert
  divergence; there is no upper bound - it is a forward-replay instant),
  `halt_secs` in `[0, 86_400]` (up to one day), and `gap_frac` finite in
  `[-1.0, 1.0]`.

### `validate_conn_havoc`

Run by the adapter on `HavocSpec.conn`.

- `reconnect_backoff_factor` must be finite and `>= 1.0` (a shrinking backoff is
  meaningless).
- `reconnect_delay_max_ms` must be `> 0` whenever `reconnect_delay_initial_ms >
  0` (a zero ceiling with a real initial backoff is ambiguous - `max == 0` is not
  a documented "unlimited" sentinel, and it would collapse the lifecycle backoff
  into a CPU-spinning zero-delay reconnect loop), and `>=`
  `reconnect_delay_initial_ms` when both are positive.
- Symmetrically, `reconnect_delay_initial_ms` must be `> 0` whenever
  `reconnect_delay_max_ms > 0`: the lifecycle backoff is
  `initial * factor^attempt`, so a zero initial stays zero on every attempt
  regardless of the ceiling - the same CPU-spinning reconnect loop from the
  other direction. Disabling backoff requires BOTH bounds zero, which is fine
  (backoff disabled). Note the partial-table implication: a `[havoc.conn]`
  arming only `reconnect_delay_initial_ms = 0` inherits the default
  `reconnect_delay_max_ms = 10_000` and is rejected - set both to zero to
  disable backoff.
- `max_requests_per_second`, when present, must be `> 0` (`None` is the
  documented "unlimited"; `Some(0)` has no defined meaning).

The remaining `ConnHavoc` fields (idle timeout, heartbeat interval, jitter,
attempt cap, request timeout) carry no cross-field invariant: `0` / `None`
disable the respective behavior, and any other value is honored as-is.
