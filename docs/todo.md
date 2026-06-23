# mogwai - TODO

A fake broker/exchange that plugs into **broadarrow** to exercise the *live*
trading path: it replays Kraken trade history as market data and emits the messy,
realistic execution divergences (partials, rejects, delays, drops, blackouts) an
in-process sandbox structurally cannot produce. The four broker crates never
import nautilus; the lone exception is the `mogwai-adapter` crate, which path-deps
nautilus to ship the `ExecutionClient`/`DataClient` pair that lets broadarrow
drive the `MOGWAI` venue over this workspace's native protocol.

## Status

Done and verified end-to-end (`scripts/smoke.py` plus the workspace unit and
integration tests):

- `mogwai-protocol` - native JSON-over-WS wire types + `control::Divergence`,
  including the `Position` type and an `AccountState` that carries balances
  *and* positions. The amend lifecycle has its own acknowledgement
  (`OrderUpdated`, carrying the post-amend price, total quantity and leaves) and
  its own rejection (`OrderModifyRejected`, whose `venue_order_id` is absent only
  when the order id is unknown), kept distinct from the submit-side
  `OrderAccepted`/`OrderRejected` so the adapter can tell a submit reject from a
  modify reject (see git history for the landing).
- `mogwai-engine` - venue-agnostic core with the divergence-injection seam.
  The two engine-side divergences (`DuplicateNextFill`, `DropNextAccountUpdate`)
  extend the `armed`-queue seam alongside `PartialFillNext`/`RejectNextSubmit`;
  the two temporal, connection-scoped divergences (`DelayAcks`, `GoDark`) are
  filtered out of the engine queue and applied in the server's outbound writer
  (see git history for the landing). The instrument table now carries precision
  and increments (price/size precision plus tick/lot size, exposed as the shared
  `InstrumentDef` wire type) so the adapter can translate mogwai `Decimal` ticks
  into nautilus `Price`/`Quantity` at a stable per-symbol precision and the
  server can serve the table over `GET /instruments` (see git history for the
  landing). Maintains per-currency
  balances and per-symbol VWAP positions off an instrument-decomposition table,
  and pushes `AccountState` after fills and reservation-freeing cancels
  (free/locked derived from resting-order reservations; pure delta ledger off
  zero, so an unfunded buy drives the quote leg negative). `ModifyOrder` is a
  real amend of a resting order: it reprices and/or resizes the open order in
  place (the new wire `quantity` is the order's total, so leaves is re-derived as
  total minus already-filled), re-derives the reservation that backs it, and
  emits `OrderUpdated` plus a fresh `AccountState`; an amend of an unknown order,
  an empty amend, a non-positive price, or a quantity at or below the filled
  amount is rejected on the wire and leaves the order untouched. The amend never
  touches the armed-divergence queue, so an interleaved modify cannot consume a
  divergence armed for a fill (see git history for the landing).
- `mogwai-data` - streaming Kraken CSV loader (O(1) memory over multi-GB files),
  seconds→ns, k-way `MergeSource`; verified on the real 43GB dump.
- `mogwai-server` - axum `/health`, `/ws` (orders + market-data replay),
  `/control/divergence`, plus the request/response data surface the adapter's
  `request_` handlers hit: `GET /instruments` (the engine's precision-bearing
  instrument table as JSON), `GET /trades` and `GET /quotes` (a bounded,
  seek-and-scan historical fetch keyed by `symbol`/`start`/`end`/`limit`, with a
  hard `MAX_HISTORY_LIMIT` row ceiling so neither the response body nor the
  client's materialized vector grows unbounded over a multi-GB dump; `/quotes`
  is always empty because the Kraken dump is trades-only, and `/trades` mirrors
  the live path's optional aggressor inference), plus a `POST /orders` route that
  accepts an order-bearing `ClientMessage` and returns the engine's resulting
  `ServerMessage` events as a JSON array. That route drives the identical
  `engine.process` call the `/ws` order arm makes, so order semantics are
  byte-identical across the two carriers; only the connection-scoped temporal
  divergences (`DelayAcks`/`GoDark`, which model a streaming writer) do not apply
  to the request/response response. It rejects `Subscribe`/`Unsubscribe` bodies,
  which belong on `/ws`. Replay runs on a blocking thread with backpressure;
  speed via `MOGWAI_REPLAY_SPEED`. Per-subscription windowing (`start_ts` on
  `Subscribe`, seeking each source's prefix), `Unsubscribe` cancellation (a
  shared atomic flag the replay loop polls), and a paced inter-tick sleep cap
  (`MOGWAI_GAP_CAP_MS`, default 1000) are all wired and pinned by `smoke.py`.
  The outbound writer also owns the two temporal divergences: `DelayAcks` holds
  each execution event (market data untouched) and `GoDark` drops everything for
  the blackout window, both armed over `/control/divergence` via shared atomics.
  Optional tick-rule aggressor inference (the `TickRuleAggressor` `Permutation`,
  opt-in via `MOGWAI_INFER_AGGRESSOR`, applied over the merged stream so each
  symbol's rule sees its trades in replay order; `Identity` stays the default).
- `mogwai-adapter` - `MogwaiDataClientFactory` / `MogwaiExecutionClientFactory`,
  their `MogwaiDataClientConfig` / `MogwaiExecClientConfig` (serde,
  `ClientConfig`-downcastable, carrying the mogwai-server URL plus the exec
  identity the core needs; the shared default base now points at the server's
  real `8787` port, and the config derives an `http://` base from the `ws://`
  one so the WS and HTTP transports speak the right scheme off one field).
  The factories downcast, validate and construct (exec building an
  `ExecutionClientCore` at `OmsType::Netting`), reporting the `MOGWAI` name.
  This is the one crate that path-deps the sibling `../nautilus_trader` checkout
  (default-features off, no pyo3); the other four stay nautilus-free.
  The `DataClient` is live (see git history for the landing): `start` grabs the
  runner's thread-local egress sink, `connect` seeds the instrument cache off
  `GET /instruments`, opens the `/ws` socket and spawns the reader/writer drain
  pair, and `disconnect`/`stop` tear them down. `subscribe_trades`/`_quotes`/
  `_bars`/`_instruments`/`_instrument` and their `unsubscribe_` partners are
  implemented: an adapter-owned per-symbol refcount table only sends
  `Subscribe`/`Unsubscribe` on the 0<->1 transition (so trades/quotes/bars for
  one symbol no longer clobber the server's set-keyed replay), keeps the
  earliest `start_ts` on conflict, and the drain task filters each frame by the
  subscribed data type. Bars are venue-delivered: the adapter aggregates the
  drained trade stream into time-based `Bar` values (live and for `request_bars`)
  rather than relying on nautilus's internal aggregator, which never reaches an
  external-bar client. The `request_` handlers follow the databento spawn-and-
  respond idiom over the server's HTTP surface, with a mandatory limit ceiling
  bounding the materialized response vector. A pure `convert` module maps mogwai
  `Decimal` ticks to nautilus `Price`/`Quantity`/`TradeTick`/`QuoteTick`/
  `InstrumentAny` at the instrument's declared precision. The `ExecutionClient`
  is live (see git history for the landing): `submit`/`modify`/`cancel` translate
  the nautilus trading commands into `mogwai-protocol` order commands over an
  exec-owned `/ws` socket, a reader task drains the `ServerMessage` order-event
  stream and dispatches each variant through an `ExecutionEventEmitter` into the
  live runner's execution-event channel, and `generate_account_state` graduated
  from no-op to real emission of the pushed `AccountState` snapshots. Because the
  global runtime is multi-thread and the `CacheView` is not `Send`, the reader
  thread never touches the cache: it builds accepted / filled / updated / canceled
  events from raw wire fields plus an `Arc<Mutex<ExecState>>` reconciliation
  mirror seeded at `submit` time, and resolves a fill's quote currency and
  precision from the `GET /instruments` table rather than from an order lookup.
  The three reconciliation report generators
  (`generate_order_status_reports`/`_fill_reports`/`_position_status_reports`)
  reconstruct their reports from that mirror, filtered by the request's
  `start`/`end` bounds so open-order reconciliation sees no false conflicts.
  Both client configs now carry a `transport_profile` field (`WsStreaming`
  default, `HttpOrders`, `HttpPolling`) shared as the `TransportProfile` enum in
  `mogwai-protocol`, and `connect` branches on it (see git history for the
  landing). Under an orders-over-HTTP profile the exec client opens no `/ws`
  socket: the three order methods POST to `/orders` and drain the returned events
  through the same `handle_exec_message` dispatch the WS reader uses, and a failed
  POST synthesizes the matching reject so the order reaches a terminal state
  instead of wedging in Submitted. Under the polling profile the data client opens
  no `/ws` socket either: it polls `GET /trades` per subscribed symbol on a fixed
  interval, advancing an inclusive overlap-and-skip cursor (re-fetch the boundary
  `ts_event`, skip the already-emitted prefix) so a response capped mid-timestamp
  by `MAX_HISTORY_LIMIT` neither drops nor duplicates a trade, and emits through
  the same per-symbol `SubState` filter the WS drain uses. The default profile is
  byte-identical to the pre-selector adapter.
  The `HavocSpec` knobs are live (see git history for the landing): one
  `HavocSpec` (shared in `mogwai-protocol` as `HavocSpec`/`ClientHavoc`/
  `HavocLatency`/`EventKind`, with `delay_for` mirroring nautilus's
  `StaticLatencyModel` `base + op` composition) arms both halves of the havoc
  from a single `havoc: Option<HavocSpec>` field on both client configs, range-
  checked at factory `create` time. The exec client `connect` ships the server-
  side `Vec<control::Divergence>` to `/control/divergence` on connect (one POST
  per divergence, so broadarrow makes no separate control-plane call); the data
  client carries the same field but ships nothing server-side, applying only its
  client half. The client half is a seeded `HavocFilter` that corrupts the
  inbound stream in flight - latency, drop, duplicate and adjacent-reorder -
  threaded through every drain path (the data and exec WS readers, the polling
  loop, and the per-dispatch exec-over-HTTP path), with a clean `None` spec a
  byte-identical passthrough. Only the broadarrow-side value that constructs a
  per-venue `HavocSpec` remains, and it lives in broadarrow, not here.

## Next

### 1. Engine depth
- [ ] Real matching against an order book (today: immediate fill, no book).

### 2. broadarrow integration: the mogwai venue adapter
broadarrow drives strategies live through a nautilus `LiveNode`, registering one
venue adapter per exchange (the stock `nautilus-binance` / `-kraken` / `-bybit`
factories, picked by the symbol's exchange prefix in `run-prep/src/venue.rs`).
mogwai is just another venue: broadarrow points a `MOGWAI` venue at a running
mogwai-server instead of a real exchange. No protocol impersonation - mogwai is
queried on purpose, so it speaks its own native protocol and ships its own
nautilus adapter. Nautilus exposes this as a plugin point: `add_data_client` /
`add_exec_client` take a `Box<dyn DataClientFactory>` / `Box<dyn
ExecutionClientFactory>`, and adapters implement those traits in their own crate.

The adapter is the one deliberate exception to "the broker crates never import
nautilus": a `DataClient` + `ExecutionClient` pair plus their factories, each a
client of mogwai-server that translates nautilus commands to and from
`mogwai-protocol` messages. It lives here as a new crate so mogwai owns both ends
of its protocol; the cost is that the nautilus path-deps enter this workspace's
build graph (the broker crates themselves stay nautilus-free).

#### How events flow (resolved from the nautilus adapter survey)
Every stock adapter wires the same way, so mogwai copies it:
- Market data egress: the data client pushes `DataEvent` into a process-global
  `UnboundedSender<DataEvent>` fetched via
  `nautilus_common::live::runner::get_data_event_sender()` (grabbed in `start`,
  NOT passed at construction).
- Order-event egress: an `ExecutionEventEmitter` (built with the clock plus
  trader / account ids), or `msgbus::send_order_event`.
- Cache: the exec factory hands the client a read-only `CacheView`; only the
  in-process sandbox path gets a mutable `Rc<RefCell<Cache>>`, which mogwai does
  NOT use (mogwai is out-of-process, so it is a live adapter, not a sim client).
- The runtime shape is a spawned task draining `transport.stream()`, parsing into
  nautilus types, and pushing to the sink. The stock CEX adapters build on
  `nautilus_network`'s `HttpClient` / `WebSocketClient` (reconnect, backoff,
  heartbeat, rate-limit quotas for free). The landed data client reuses
  `nautilus_network::http::HttpClient` for the request/response surface but
  drives the `/ws` stream over `tokio-tungstenite` directly (a reader/writer task
  pair), since mogwai owns the protocol and the heavier WS client's quotas and
  reconnect policy buy little against a local server (see git history for the
  landing).

#### Chosen shape: lean standard-CEX, with a transport selector
The survey found four archetypes (heavy CEX = binance pool/SBE; standard CEX =
kraken/bybit WS-primary with HTTP fallback; data-only streaming = databento with
built-in historical replay; in-process sim = sandbox). mogwai follows the
standard-CEX shape stripped to the bone: it owns the protocol, so no SBE, no
product-type split, no auth signing, no rate-limit honoring, no connection pool.
Under the default profile WS carries streaming (market data, order events, order
commands); a small HTTP surface on mogwai-server answers the request/response
calls (`request_instruments`, the reconciliation reports, account snapshot) and
fits its existing axum routes. The transport selector (landed, see git history)
lets a non-default profile move order entry onto HTTP and market data onto a
polling loop, so one backend can drive each of broadarrow's integration paths.

The `transport_profile` selector that drives the adapter down each archetype is
landed (see git history): one mogwai-server can present as WS-for-everything,
HTTP-orders-with-pushed-data, or fully request/response. Only the broadarrow-side
wiring that picks a profile per venue remains:

- [ ] broadarrow side (must live in broadarrow, not here): a `MOGWAI` arm in
      `run-prep/src/venue.rs` and a `core::venue` PROFILES row; a profile-guard
      test enforces that every wired venue has a PROFILES row.

#### Havoc knobs: one `HavocSpec`, two surfaces
The point of mogwai. The `HavocSpec` type and the adapter's split/apply/ship are
landed (see the adapter Done entry above and git history): one config object
arms both halves of the havoc, the adapter applying its client half (latency,
drop, duplicate, reorder) to its own inbound stream and shipping the server half
to `/control/divergence` on connect. broadarrow sets a single `HavocSpec` in the
`MogwaiClientConfig` at adapter-build time (in `venue.rs`); only that
broadarrow-side value remains, and it lives in broadarrow, not here.

This landing mirrored only nautilus's `StaticLatencyModel` (the latency field).
The wider knob vocabulary stays open as a follow-up `HavocSpec` extension:

- [ ] Connection-lifecycle and quota knobs to mirror from existing nautilus
      config that already behaves like havoc (`ws_idle_timeout_ms`,
      `ws_request_timeout_secs`, `heartbeat_interval_secs`, retry/backoff/jitter,
      rate-limit quotas). These corrupt the transport's connect/reconnect
      behavior rather than the inbound event stream the first landing targeted.

## Notes / gotchas
- Kraken history is **trades only** - no quotes, no L2, no aggressor side
  (`AggressorSide::NoAggressor`). Symbol comes from the filename (`XBT` = BTC).
  Set `MOGWAI_INFER_AGGRESSOR=1` to infer it via the tick rule at replay time.
- Data dir: `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`).
- `research/` (nautilus clone, ~413MB) is gitignored.
