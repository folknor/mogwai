# mogwai - TODO

A fake broker/exchange that plugs into **broadarrow** to exercise the *live*
trading path: it replays Kraken trade history as market data and emits the messy,
realistic execution divergences (partials, rejects, delays, drops, blackouts) an
in-process sandbox structurally cannot produce. mogwai never imports nautilus; the
client-side `ExecutionClient`/`DataClient` impls live in broadarrow and speak this
crate's protocol over the wire.

## Status

Done and verified end-to-end (`scripts/smoke.py`, 43 unit tests):

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
  (see git history for the landing). Maintains per-currency
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
  `/control/divergence`. Replay runs on a blocking thread with backpressure;
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
  heartbeat, rate-limit quotas for free); mogwai can reuse the same.

#### Chosen shape: lean standard-CEX, with a transport selector
The survey found four archetypes (heavy CEX = binance pool/SBE; standard CEX =
kraken/bybit WS-primary with HTTP fallback; data-only streaming = databento with
built-in historical replay; in-process sim = sandbox). mogwai follows the
standard-CEX shape stripped to the bone: it owns the protocol, so no SBE, no
product-type split, no auth signing, no rate-limit honoring, no connection pool.
WS carries streaming (market data, order events, order commands); a small HTTP
surface on mogwai-server answers the request/response calls (`request_instruments`,
the reconciliation reports, account snapshot) and fits its existing axum routes.

- [ ] `mogwai-adapter` crate: `MogwaiDataClientFactory` + `MogwaiExecutionClient`
      `Factory` and their `ClientConfig`s, returning the client pair.
- [ ] `DataClient` impl: required lifecycle (`client_id`, `venue`, `start` /
      `stop` / `reset` / `dispose`, `is_connected`) plus only the `subscribe_` /
      `request_` handlers mogwai serves (trades, quotes, bars, instruments); the
      rest keep the trait defaults. Borrow databento's replay idiom (a start
      timestamp replays history then switches to live), passing the start
      instant on the already-landed `Subscribe.start_ts` wire field.
- [ ] `ExecutionClient` impl: required identity + lifecycle, `submit` / `modify` /
      `cancel` mapped onto `mogwai-protocol` order commands, and the
      reconciliation report generators (`generate_order_status_reports`,
      `generate_fill_reports`, `generate_position_status_reports`).
      `generate_account_state` is REQUIRED here; its hard dependency on engine
      account state is now satisfied - the engine tracks balances/positions and
      pushes `AccountState` (with positions) on fills and reservation-freeing
      cancels (see git history for the landing). The adapter consumes that
      pushed snapshot; account state is push-only, with no client-driven query.
- [ ] `transport_profile` knob: because mogwai owns both ends, the adapter can
      behave like different archetypes (orders over WS vs HTTP-only the bybit-demo
      way, push-stream vs request/response data) so broadarrow exercises each of
      its own integration code paths against one backend. A selector, not cosmetic.
- [ ] broadarrow side (must live in broadarrow, not here): a `MOGWAI` arm in
      `run-prep/src/venue.rs` and a `core::venue` PROFILES row; a profile-guard
      test enforces that every wired venue has a PROFILES row.

#### Havoc knobs: one `HavocSpec`, two surfaces
The point of mogwai. broadarrow sets a single `HavocSpec` in the `MogwaiClientConfig`
at adapter-build time (in `venue.rs`); the adapter splits it:
- Client-side (the adapter itself, per connection): latency, drop, duplicate,
  reorder, injected in the spawned task between `transport.stream()` and the sink.
  Tests broadarrow's resilience with the server none the wiser. Mirror nautilus's
  own `StaticLatencyModel` shape (`base` / `insert` / `update` / `delete` latency
  nanos in `execution/src/models/latency.rs`, a backtest construct never wired
  live) for the latency field.
- Server-side (mogwai-server's `control::Divergence` engine): the execution
  divergences (partial, reject, duplicate fill, drop account update, blackout,
  delayed acks), all six now landed and verified end-to-end. The adapter
  forwards the server-side part of the `HavocSpec` to mogwai-server on connect,
  so broadarrow never makes a separate control-plane call - one config object,
  the adapter relays it.

- [ ] `HavocSpec` type in `mogwai-protocol` (so both ends share it), carrying the
      client-side knobs and the server-side `Divergence` arming set.
- [ ] Adapter applies client-side knobs locally and ships the server-side set over
      the existing `/control/divergence` channel on connect.
- [ ] Prior art to mirror for knob vocabulary: existing nautilus config that
      already behaves like havoc (`ws_idle_timeout_ms`, `ws_request_timeout_secs`,
      `heartbeat_interval_secs`, retry/backoff/jitter, rate-limit quotas).

## Notes / gotchas
- Kraken history is **trades only** - no quotes, no L2, no aggressor side
  (`AggressorSide::NoAggressor`). Symbol comes from the filename (`XBT` = BTC).
  Set `MOGWAI_INFER_AGGRESSOR=1` to infer it via the tick rule at replay time.
- Data dir: `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`).
- `research/` (nautilus clone, ~413MB) is gitignored.
