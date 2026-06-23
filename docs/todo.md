# mogwai - TODO

A fake broker/exchange that plugs into **broadarrow** to exercise the *live*
trading path: it synthesizes market data from a committed fingerprint fitted
offline to Kraken trade history (the running server opens no CSV) and emits the
messy, realistic execution divergences (partials, rejects, delays, drops,
blackouts) an in-process sandbox structurally cannot produce. The four broker
crates never
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
  seconds→ns, k-way `MergeSource`; verified on the real 43GB dump. Now also
  carries the `GeneratedSource` kernel (see git history for the landing): a new
  `TickSource` that loads the committed `analysis/fingerprint.json` (embedded at
  compile time via `include_str!`, so the network-isolated implementer and the
  running server open no file) and emits a deterministic, seedable, single-symbol
  trade stream. Four composed layers off one seeded `StdRng` reproduce the
  stylized facts: an ACD-style arrival clock (bursty, far-from-Poisson durations
  with clustering), a GARCH(1,1) stochastic-volatility latent mid (fat tails plus
  volatility clustering), a tick-grid bid-ask-bounce overlay (negative lag-1
  return ACF, flat-run zero-change fraction, on-grid prices) and a heavy-tailed
  lognormal size draw with round-lot mass. Because the bounce decides the side,
  every emitted trade carries a native `Buyer`/`Seller` aggressor, where the CSV
  path needs `TickRuleAggressor` to infer one. `GeneratorScalars` is the
  per-instrument knob vector (a mutable struct, leaving room for the later
  market-regime havoc axis) with `from_fingerprint_medians`, `xbtusd_anchor` and
  a range-checking `validate`; `new` takes an explicit `start_ts` epoch anchor so
  determinism is caller-set rather than wall-clock "now". Gated by a
  self-contained Rust realism test that draws a long stream and asserts each
  measured stylized fact lands inside the fingerprint's cross-pair tolerance band
  (never against live CSV), plus focused determinism, monotonic-clock, on-grid,
  native-aggressor, scalar-validate and fingerprint-parse unit tests. The
  fingerprint's `session_profile` block is now applied (see git history for the
  landing): a `SessionModulator` makes the generated stream non-stationary in
  wall-clock time, multiplying the arrival intensity by the UTC hour-of-day and
  day-of-week shares and the latent-mid volatility by the hour vol curve, so a
  generated stream reproduces the fingerprint's intraday intensity, intraday
  volatility and day-of-week split. The envelope is a deterministic outer factor
  applied to the realized duration and return after the ACD and GARCH recursions,
  which keep running on the un-modulated values so their clustering dynamics are
  preserved; the fingerprint loader now also fails loud on a non-positive session
  share. No constructor or fingerprint-schema change. An ignored 5M-tick test
  asserts the generated stream reproduces the session curves (intensity argmax in
  the London-NY overlap, the NY-open vol spike, weekend thinning, vol correlation
  above 0.9), and the realism gate gained a duration-ACF assertion confirming the
  envelope leaves duration clustering intact. `KrakenCsvSource`
  and the two server replay sites are untouched - the landing is purely additive.
- `mogwai-server` - axum `/health`, `/ws` (orders + market-data replay),
  `/control/divergence`, plus the request/response data surface the adapter's
  `request_` handlers hit: `GET /instruments` (the engine's precision-bearing
  instrument table as JSON), `GET /trades` and `GET /quotes` (a bounded,
  seek-and-scan historical fetch keyed by `symbol`/`start`/`end`/`limit`, with a
  hard `MAX_HISTORY_LIMIT` row ceiling so neither the response body nor the
  client's materialized vector grows unbounded; `/quotes` is always empty because
  the generated history is trades-only), plus a `POST /orders` route that
  accepts an order-bearing `ClientMessage` and returns the engine's resulting
  `ServerMessage` events as a JSON array. That route drives the identical
  `engine.process` call the `/ws` order arm makes, so order semantics are
  byte-identical across the two carriers; only the connection-scoped temporal
  divergences (`DelayAcks`/`GoDark`, which model a streaming writer) do not apply
  to the request/response response. It rejects `Subscribe`/`Unsubscribe` bodies,
  which belong on `/ws`. Replay runs on a blocking thread with backpressure.
  Run config comes from a `mogwai.toml` read at startup (overridable with
  `--config <path>`, built-in defaults when absent) carrying the replay `speed`
  multiplier and the paced inter-tick sleep cap `gap_cap_ms` (default 1000),
  replacing the former environment variables. Per-subscription windowing
  (`start_ts` on `Subscribe`), `Unsubscribe` cancellation (a shared atomic flag
  the replay loop polls), and the gap cap are all wired and pinned by
  `smoke.py`. The outbound writer also owns the two
  temporal divergences: `DelayAcks` holds each execution event (market data
  untouched) and `GoDark` drops everything for the blackout window, both armed
  over `/control/divergence` via shared atomics.
  The running server now opens no Kraken CSV (see git history for the landing):
  every subscribed symbol's market data is synthesized by
  `mogwai_data::GeneratedSource`, seeded deterministically from the committed
  `analysis/fingerprint.json`, fed into the same `MergeSource` and the same
  outbound writer the CSV path used. A server-owned `source` module owns the two
  carriers - `build_live_source` (the `/ws` `Subscribe` path) anchors each
  generator directly at the requested window `start_ts`, so a windowed subscribe
  is O(1) with no draining `seek_to` over a multi-year prefix, while
  `build_history_source` (the `GET /trades` path) anchors every generator at a
  fixed per-symbol `ORIGIN_TS` and `seek_to(start)`s into that one append-only
  tape, so the same tick always lands at the same `ts_event` and the adapter's
  poll cursor slices one stable timeline rather than restarting a fresh prefix.
  The history seek is bounded by a server-side `BoundedSeek` wrapper
  (`MAX_HISTORY_SEEK_TICKS`), and `MAX_HISTORY_LIMIT` is `1_000`, so a
  default-`limit` `/trades` call synthesizes a bounded number of ticks and
  returns well inside the adapter's poll interval. Every generated trade carries
  a native `Buyer`/`Seller` aggressor, so the server no longer constructs a
  `Permutation`: the `MOGWAI_DATA_DIR` and `MOGWAI_INFER_AGGRESSOR` env knobs and
  the `data_dir`/`infer_aggressor` config fields are torn out. `KrakenCsvSource`
  and the tick-rule aggressor stay compiled and unit-tested in `mogwai-data` as
  the offline-analysis lineage; only the server's use of them is removed.
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

### 1. Synthetic tick generation: a generated tick source that looks real

#### The goal (achieved for the data path; the havoc axis remains)
mogwai stops reading Kraken CSVs at runtime entirely. Originally the only thing
that produced market data was `KrakenCsvSource`, which read a real CSV line by
line, and the running server opened those files and replayed them. That is now
reversed (see git history for the landing): no CSV is ever opened by the running
server. Every tick is generated by a self-contained stochastic model, and the
generated stream is realistic enough that broadarrow's live
classify/brake/quarantine layer cannot tell it from a real feed. The 45GB Kraken
dump is consumed exactly once, offline, to fit that model; the running server
ships only the fitted parameters, never the data. What remains under this
heading is the market-regime havoc axis below, which builds on the generator the
data-path landing wired in.

This is the point of mogwai restated for market data: just as the execution path
emits synthetic-but-realistic divergences instead of matching a real book, the
data path should emit synthetic-but-realistic ticks instead of replaying a real
tape. And the generated stream becomes the clean input the havoc knobs corrupt:
a realistic baseline tick stream, mutated by `HavocSpec` before it reaches the
listener.

#### The core decision: the model is symbol-agnostic
We do not model instruments individually. The microstructure shape of liquid
trade-tick streams is universal - the same stylized facts hold across
instruments, and what differs between, say, BTC and a thinner pair is a handful
of scalars (price level, tick size, typical trade size, overall activity and
volatility level), not the dynamics. mogwai is a fake venue; nothing downstream
cares whether a stream "is really" any particular pair. The symbol is a label on
the wire. So the corpus is analyzed pooled, as one body of evidence for the
universal shape and the plausible ranges of those scalars - never as a
per-symbol lookup table. A synthetic instrument is then just the generator called
with a chosen scalar vector and a seed, wearing whatever symbol name the
instrument table declares.

The model has two layers:
- A universal microstructure kernel (stationary): the shared shape, parameterized
  by the four scalars above.
- A session modulator (non-stationary, deterministic in wall-clock time): the one
  piece of genuinely structured, predictable variation, keyed to UTC time-of-day
  and day-of-week, encoding the financial-session cycle.

#### What "real" means: the acceptance criteria
"Looks real" is not a matter of taste; it is a set of measurable stylized facts
that are simultaneously the offline measurement targets and the generator's
validation tests. A generated stream is realistic iff it reproduces:
- A bursty clock, not Poisson: inter-arrival times are heavy-tailed AND
  positively autocorrelated (activity clusters - quiet stretches then floods). A
  model that gets the marginal right but the clustering wrong looks fake in
  sequence.
- Returns that are fat-tailed with near-zero autocorrelation, but whose absolute
  value is strongly, slowly-decaying autocorrelated (volatility clustering).
- Bid-ask bounce: traded prices ping between bid and ask, giving a characteristic
  negative lag-1 autocorrelation in price changes and long runs of identical
  prices. This is the single most recognizable signature of real tick data.
- Prices on the tick grid, and trade sizes that are heavy-tailed with visible
  round-number clustering.

#### The session structure (the one real non-stationarity)
The structured, exploitable variation in a tick stream is not the instrument, it
is the session clock: London open, NY open, the London-NY overlap (the hot
window), the Asia session, and the edges where they hand off. Activity and
volatility ramp and spike around those transitions; mid-session is comparatively
stationary. The session modulator is therefore a function of absolute UTC
wall-clock time that multiplies the kernel's arrival intensity and volatility,
with the edges modeled with care - elevated vol and activity, and, where the data
supports it, a directional drift component right at the opens and closes. The
generator must carry a wall clock, not just an inter-arrival clock.

Two distinct edge phenomena exist and need different treatment; which one mogwai
presents is an open decision (see below):
- A 24/7 spot venue (what the Kraken dump is) never closes. Its session structure
  is inherited from the TradFi participant clock - a smooth intensity and vol
  ramp, no gap.
- A session-closing venue (CME-style futures) halts and gaps on reopen - Monday
  gaps, overnight gaps. A discontinuity, a different feature, and excellent havoc
  material.

#### Architecture: fit offline, generate online
The split that makes "never load a CSV at runtime" true:
- Offline (research, Python): stream the 45GB once - O(1) memory, mirroring the
  existing loader's discipline - measure the stylized facts and the UTC session
  profile, fit the model, and emit a small fingerprint (a parameter pack of a few
  KB to MB, plus whatever empirical histograms the generator samples from). This
  is the throwaway-able research code; it lives in a new analysis directory and
  is not a runtime build input. Practical handling: a cheap full streaming pass
  for marginals, counts, tick-grid inference and coarse seasonality over
  everything, then targeted windowed passes for the autocorrelation and clustering
  estimates on representative segments spanning the full date range.
- Online (runtime, Rust): a new `GeneratedSource` implementing the existing
  `TickSource` trait reads the fingerprint and generates ticks deterministically
  from a seed, with no file IO. It drops into `MergeSource` exactly where
  `KrakenCsvSource` does today, and is selected in `mogwai-server` in place of the
  CSV source. Because generation is deterministic and seedable it composes with
  the already-seeded havoc layer.

A bonus of generating rather than replaying: the generator knows the aggressor
side natively (the bounce overlay decides it), so `GeneratedSource` can emit a
real Buyer/Seller aggressor where the CSV path can only ever infer it via the
tick rule.

#### Validation is the deliverable that proves realism
Hold out corpus segments the fit never saw, generate a stream from the fitted
parameters, and compare the stylized facts and the intraday session profile -
marginal distances on durations/returns/sizes, return ACF near zero, absolute-
return ACF decay, negative lag-1 bounce, tail-index match, and the UTC
seasonality curve overlaid on the real one. Roll these into a single realism
score with an explicit accept threshold, and commit a small held-out fixture so
"the generator still looks real" is a regression test, not a judgment call. Per
the spec contract, this measurement instrument is itself a brick, built and
specified before the bricks it gates.

#### Havoc composition (the second surface)
The kernel's parameters are interpretable knobs, which opens a market-regime
havoc axis distinct from the existing transport-corruption havoc. Today
`HavocSpec` corrupts events after they are produced (latency, drop, duplicate,
reorder). A parametric generator lets havoc also corrupt the market before it is
produced: crank the volatility state, thin the arrival intensity (liquidity
drought), inject a session-edge spike or a reopen gap. Same `HavocSpec`
philosophy, new surface; the generator seed and the havoc seed sit side by side.
The first landing need not build this axis, but the generator parameterization
must leave room for it rather than bake the scalars in as constants.

#### Resolved decisions
The forks are settled (the codex implementer cannot ask mid-run, so none is left
open):
- Venue character: 24/7 spot, smooth session ramps. This matches the Kraken
  corpus and is the first landing. Session-closing reopen-gaps are out of scope
  here and belong to the later market-regime havoc axis.
- Model tier: a parametric stochastic model - a self-exciting (Hawkes) or ACD
  clock under the measured UTC session baseline, a tick-grid stochastic-volatility
  mid with a bid-ask-bounce overlay, and heavy-tailed sizes with round-lot mass.
  The fidelity bar is the measured stylized-fact suite (below). An ML sequence
  model is explicitly not pursued: heavier, harder to make deterministic and to
  validate.
- Language split: fitting lives in Python (Phase 0, done), the production
  generator is a Rust `TickSource`, and `analysis/fingerprint.json` is the
  contract between them.

#### Implementer constraint (load-bearing for the spec)
The codex implementer runs network-isolated and cannot read `MOGWAI_DATA_DIR`
(it is outside the repo, on a separate drive). Two consequences the spec must
honor:
- The fingerprint must live in the repo as a committed input the generator loads
  (or embeds); the running CSVs are never a build or test input.
- The realism gate is therefore a self-contained Rust test: generate a stream
  from the fingerprint and assert it reproduces the golden targets within the
  cross-pair tolerances. It never compares against live CSV data. The deeper
  synthetic-versus-real held-out comparison stays a main-session Python check,
  outside the codex loop and not a `brokkr check` gate.

#### Data schema (confirmed from the loader)
Each Kraken CSV line is exactly three fields: UTC unix timestamp in seconds
(optionally fractional), price, size. Trades only - no quotes, no L2, no aggressor
side. Symbol derives from the filename. Data dir is `MOGWAI_DATA_DIR` (default
`/media/folk/Banan/Kraken_Trading_History`).

#### Phase 0 (offline analysis): DONE
The corpus characterization is landed under `analysis/` (recon, the streaming
per-pair characterizer, the parallel corpus driver, and the fingerprint builder).
`analysis/fingerprint.json` is the contract artifact; `analysis/findings.md` is
the human-readable summary. Eight representative pairs, 298M trades, confirm the
stylized-fact shape is universal and bound the per-instrument scalars. Headline
golden targets (anchor XBTUSD, cross-pair range in parentheses):
- duration dispersion index 4609 (132 .. 4609) - the bursty, far-from-Poisson
  clock.
- return ACF lag1 -0.197 (-0.197 .. -0.057) - the bid-ask bounce.
- absolute-return ACF lag1 0.31 decaying to 0.12 at lag50 - slow-memory
  volatility clustering.
- zero-price-change fraction 0.47 (0.34 .. 0.75).
- scalar ranges: modal tick 1e-7 .. 0.1, price decimals 1 .. 7, mean inter-trade
  3.8s .. 12.6s, size round-fraction 0.12 .. 0.27.
The fingerprint also carries the pooled UTC session profile: a 24-hour intensity
curve peaking at the London-NY overlap (~14-16 UTC, ~0.055 share) and troughing
in the Asian small hours (~05 UTC, ~0.031), a 24-hour volatility curve with a
sharp NY-open edge (hour 14 at 2.46x the per-pair mean), and day-of-week weights
that thin on weekends (~0.11 vs ~0.15 weekday).

#### Remaining work items (ordered, one landing each)
The `GeneratedSource` kernel, the session modulator layered onto it, and the
server wiring that selects it in place of `KrakenCsvSource` are all landed (see
the `mogwai-data` and `mogwai-server` Done entries above and git history). The
follow-on landing, a coherent keep/revert, tree green at every boundary:
1. (Later, separate) the market-regime havoc axis over the generator parameters,
   including session-closing reopen-gaps.

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
  This is the shape of the offline corpus only; the running server no longer
  reads it. Every generated trade carries a native `Buyer`/`Seller` aggressor, so
  the server needs no tick-rule inference; `KrakenCsvSource` and
  `TickRuleAggressor` survive in `mogwai-data` for the offline-analysis lineage
  and its unit tests.
- `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`) is now
  an offline-analysis input only (`analysis/`), never a server runtime knob - the
  running server opens no CSV.
- `research/` (nautilus clone, ~413MB) is gitignored.
