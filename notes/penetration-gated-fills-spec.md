# Penetration-gated fills - implementation spec

Written against `reference/technical-implementation-spec.md`. Spawned from the
`notes/todo.md` entry "BUILD, unblocked now that the default tape carries a
gated dwell bound: penetration-gated fills" (RFC 4631 phase A).

Revision 2. Two independent reviews (`notes/penetration-review-r1.md`,
`notes/penetration-review-r2.md`) were validated against the source and folded
in; section 8 records what was taken and what was refused. The largest change
is ownership: the sweeper is ACCOUNT-owned, not session-owned, because a
session-owned one cannot fill an HTTP-submitted order and stops the account
trading on disconnect.

## 1. The item

mogwai fills a resting limit at the order's own price the instant it is
submitted. A maker strategy therefore never pays for the one thing a real book
charges it: waiting for the market to come to it, and sometimes never getting
there. That is the specific lie this spec removes.

The gate: a LIMIT order fills only once the tape has printed `N` trades THROUGH
its price since the venue accepted it. `N = 0` reproduces today's behaviour
byte for byte and is the default, so nothing about the shipped venue changes
until a scenario asks for it.

Two deviations from RFC 4631 phase A are forced and are stated wherever this
lands (code comment, `reference/architecture.md`, RFC thread):

1. **The predicate is a traded price, not a quote.** The RFC gates on the best
   QUOTE moving through the limit. mogwai has no quotes to gate on: the offline
   Kraken corpus is trades-only, `GeneratedSource` synthesizes trades, and
   `/quotes` is always empty (`notes/todo.md`, "Notes / gotchas"). Gating on
   traded prices is what bar and L1 backtesting does anyway and is defensible on
   its own terms, but it is NOT the predicate nautilus would implement upstream.
   If both ship they will disagree about fills for a reason that is invisible at
   the call site.
2. **The fill still prints at the ORDER'S price, not the penetrating trade's.**
   That is the existing no-book contract and this spec does not touch it: the
   gate decides WHEN, not at what price. Price improvement is a book property
   and mogwai has no book.

## 2. Survey of the ground

### 2.1 The engine's fill path (`crates/mogwai-engine/src/orders.rs`)

`Engine::on_submit` runs, in this exact order, and the order is load-bearing:

1. `validate_submit` - id, instrument, quantity/price sign, size and price
   grid, notional `checked_mul`, and on a funded account the free-balance
   check.
2. `RejectNextSubmit` consumption.
3. `fill_fraction` - consumes a `PartialFillNext` TARGETED at this
   `client_order_id`.
4. `fill_quantity(order, fraction, size_increment)` - clamps the fraction to
   `[0, 1]`, multiplies `order.quantity`, floors to the size grid, and carries
   the two documented degenerate fallbacks (sub-increment partial fills ZERO; a
   non-positive candidate full-fills with a warn).
5. FOK all-or-nothing gate, judged against the (possibly diverged) fill size.
6. `next_id("V")` and the `seen_client_order_ids` reservation - only an
   ACCEPTED order reserves its id.
7. `OrderAccepted`.
8. If `last_qty > 0`: build `OrderFilled` at `order.price`, `apply_fill`,
   `record_fill`, consume `DuplicateNextFill`, push the fill (twice if
   duplicated).
9. Route the `OpenOrder`: GTC remainder rests in `self.open`; IOC remainder
   emits `OrderCanceled` and `record_closed`; a zero remainder is
   `record_closed(Filled)`.
10. Consume `DropNextAccountUpdate`, else push `AccountState`.

Steps 3-5 sit BEFORE the id reservation on purpose, and the comment above them
says why: FOK is judged against the diverged size, and a rejected FOK must not
burn its `client_order_id`. Any refactor that needs a `VenueOrderId` to size a
fill has to respect that ordering (3.11).

`OpenOrder { venue_order_id, submit, leaves_qty, ts_accepted, ts_last }` is the
resting record. `Engine::process(msg, ts)` is the only mutation entry point;
`ts` is supplied by the caller so the engine never reads a clock. The engine
is synchronous, side-effect free, and has no way to observe market data at all -
that is the seam this spec has to open.

`on_modify` amends a resting order in place (price and/or total quantity),
re-derives `leaves_qty = new_total - filled`, sets `ts_last`, and emits
`OrderUpdated` + `AccountState`. It deliberately never touches the armed queue.

`cancel_open_order_silently` removes a resting order with no event, and
`record_closed` freezes terminal records into the `QueryOrders` truth store.

### 2.2 The server's order-entry choke point (`crates/mogwai-server/src/http.rs`)

`process_order_cmd` is the single path for BOTH carriers (`POST /orders` and the
`/ws` read loop):

1. `ts = sim_now_ns(state.sim)`, `boundary_outcome` refusal.
2. The `CommandLatency` ACT sleep, when the caller passes `ActDelay::PayHere`
   (the HTTP surface) rather than having already paid it off the read loop.
3. `stamp_market_price(order_cmd, state).await` - for a price-less MARKET order
   only, via `source::current_price`, pushed onto `spawn_blocking` "so a burst
   ... cannot stall the runtime's worker pool".
4. `ts` re-sampled after the stamp.
5. price-less-MARKET reject (`"venue could not synthesize a market price at
   sim-now"`).
6. `slot.engine.lock().await`, tombstone recheck UNDER the lock.
7. `lanes.reserve(&order_cmd, &engine.book_shape())` - the worst-case held-lane
   BYTE reservation, taken before the engine is allowed to mutate. `None` means
   `AdmissionRejected` and NOTHING mutates.
8. `engine.process(order_cmd, ts)`.

`POST /orders` reaches this without any websocket session existing. An order
carrier that is not `/ws` is therefore a first-class path, which is what makes
a session-owned sweeper wrong (3.4).

### 2.3 The tape, and what the server can already ask it

`crates/mogwai-server/src/source.rs`:

- A symbol's whole tape is a pure function of `(scalars, seed, start_ts,
  session_profile, regime)`. `seed_for(symbol)` and the boot-derived
  `data_origin_ns` fix the first two; the CLEAN (no-regime) tape is the one the
  order path cares about.
- `positioned_generator` restores the nearest checkpoint at or before a target
  from a process-wide, mutex-guarded `CheckpointStore` (`CHECKPOINT_K = 8192`),
  so seeking to an arbitrary instant is O(K), not O(span).
- `BoundedSeek { inner, cap: MAX_HISTORY_SEEK_TICKS }` (190,000) caps ONLY
  `seek_to`. Its `next_tick` delegates straight to the inner source with no cap
  at all, and `GeneratedSource::next_tick` never returns `None`. So a DRAIN
  after positioning is unbounded, and any walk this spec builds must carry its
  own drain budget rather than claiming `BoundedSeek`'s (3.10, 4.5).
- `current_price(symbol, profiles, data_origin, sim_now) -> Option<Decimal>`
  returns `seek_to(sim_now)`, which is the first tick AT OR AFTER `sim_now` -
  its own gate `current_price_matches_live_subscriber_at_same_instant` asserts
  `first.ts_event >= sim_now`. That is correct for stamping a MARKET order (it
  matches what a live subscriber sees) but it is a LOOK-AHEAD relative to the
  acceptance instant, so it cannot be the marketable-on-arrival predicate
  (3.3).
- `build_history_source(symbol, start, regime, profiles, data_origin)` yields a
  `MergeSource` over the same checkpointed walk, seeked to `start`. This is what
  `GET /trades` pages through and what the penetration count will walk.
- `GET /trades` with no `start` means "from ORIGIN", not "latest": the handler
  serves from `data_origin` and clamps `end` at sim-now. A client that wants the
  current level must ask for a window ending at sim-now, not `limit = 1` from
  the default start (4.9 step 1).
- Both existing synthesis callers (`stamp_market_price`, the `/trades` handler)
  run the walk on `spawn_blocking`. This spec's walk is the same shape and does
  the same (4.6).

`TradeTick { symbol, price, size, aggressor, ts_event }` carries everything the
predicate needs. `TickEvent::Quote` never occurs on the generated tape.

### 2.4 Where an unsolicited execution frame can come from

`crates/mogwai-server/src/tape.rs` runs one thread per `(symbol, data_origin,
regime)` and broadcasts pre-serialized frames; it is the market-data plane and
touches no engine. `crates/mogwai-server/src/ws.rs::handle_socket` already
spawns two long-lived per-session tasks alongside the read loop - `run_writer`
and `spawn_exec_pump` - and already has a path for execution frames that no
inbound command asked for, because `delayed_act` produces a batch from a
detached task and hands it to `lanes.submit_produced(...)`.

`ExecLanes` (`crates/mogwai-server/src/admission.rs`) is cloneable, non-blocking
on every method, and exposes `reserve` / `try_reserve_boundary` /
`reserve_admission` / `emit_admission` / `send_close` / `submit_produced(
reservation, arrived, class, events)`. Frames carry their `HeldCharge` and
release it on drop wherever the writer ends. Its byte budget
(`exec_held_budget_bytes`) is PER SESSION and fixed, so anything that makes a
mutation conditional on a reservation whose size grows with the open-order count
can wedge permanently (3.11).

`AccountSlot` (`crates/mogwai-server/src/accounts.rs`) owns `engine:
AsyncMutex<Engine>`, the havoc atomics, `sessions: AtomicUsize`, `tombstoned`
and `closed: Notify`. It does NOT today know about any session's `ExecLanes`;
`SessionLease` only counts sessions. Delivering an account-owned batch to the
account's sessions therefore needs a new registry on the slot (4.8).

`mogwai_protocol::sizing` bounds worst-case output per `ClientMessage`, sizing a
`SubmitOrder`'s `AccountState` against a shape widened by `+2` balances and `+1`
position because one fill in a new pair introduces two currencies and a
position. There is no bound today for a batch no `ClientMessage` produced;
`BOUNDARY_REFUSAL_BYTES` is the precedent for a command-less constant.

### 2.5 The sequencing precondition, now satisfied

A penetration gate makes resting fills strictly rarer, so a tape that goes quiet
for hours starves them silently. `default_symbol_tape_dwell_is_bounded` in
`crates/mogwai-data/src/generated/tests.rs` now bounds the default tape's
era-windowed p999 gap, empty-hour fraction, and longest empty-hour run against
the committed fingerprint's `golden_targets.dwell`. An armed
`MarketRegime::LiquidityDrought` still legitimately silences a tape, but it is
visible on the control plane, so a scenario can account for it.

### 2.6 The price scale, which is a trap

`GeneratedSource` anchors BTCUSDT at `START_PRICE_USD = 60_000` with
`MAX_ABS_RETURN = 2e-5` per tick. Every existing fixture order in the repo
(`scripts/smoke.py`, the engine tests, the adapter tests) uses price 100 and
quantity 10, which the bookless venue fills happily because it never compares
the order price to the tape. Under a gate, a BUY limit at 100 against a tape at
60,000 is never penetrated and rests forever. Any fixture that arms the gate
must derive its prices from the live tape, and the funded seed (1,000,000 USDT)
bounds the quantity: a BUY at ~60,000 must be for well under 16 units.

The same ceiling bounds what a fixture may WAIT for. At `MAX_ABS_RETURN = 2e-5`
per tick a price target more than a few basis points away is not reachable in a
bounded wall interval at `speed = 1.0`, so a smoke assertion must construct a
crossing it can prove rather than hope for one (4.9).

## 3. Design decisions, resolved here

### 3.1 The predicate

Counting starts at the instant the venue ACCEPTED the order and covers
`(scanned_from_ns, now_ns]`. A trade `t` counts when it is STRICTLY through the
limit:

- `Side::Buy`: `t.price < order.price`
- `Side::Sell`: `t.price > order.price`

Strict, not `<=`. A print AT the limit is the market touching, not trading
through, and at-touch filling is the exact lie being removed. `t.aggressor` is
ignored: it is a property of who crossed the spread, not of whether the price
level was consumed, and a trades-only tape cannot say whether the aggressor took
from a queue this order sits in. `t.size` is likewise ignored - queue-ahead
volume is phase B, explicitly out of scope here (7).

### 3.2 Where the gate is evaluated

The engine gets the COUNT, never the tape. `mogwai-engine` must keep building
without `mogwai-data` and must stay clock-free and side-effect free; a `dyn`
callback into the server would make it neither. The server owns the tape, so the
server counts and hands the engine numbers.

### 3.3 The marketable-on-arrival case

A limit priced through the market must fill AT ONCE, not one sweep later, or the
gate would tax every aggressive limit with a latency it never asked for. So the
submit path takes a market READING for gated LIMIT orders and the engine seeds
the order's penetration count with 1 when that print is through the limit.

The reading is NOT `source::current_price`. That function returns the first tick
at or after sim-now (2.3), so seeding from it would grant a penetration from a
trade that has not printed yet - a look-ahead leak, and a direct contradiction
of 3.1's `(from, now]` window. The reading is a new
`fills::last_trade_at_or_before(symbol, ts)` (4.5): the last print with
`ts_event <= ts` on the clean tape. If no print exists at or before `ts` within
the walk's budget, the reading is `None` and the order simply rests - a missing
reading never grants a penetration.

This does mean an aggressive limit is judged against a print that may be some
sim-milliseconds stale on a quiet tape. That is the honest direction to err: the
alternative grants a fill from the future. The staleness is bounded by the same
dwell bound 2.5 pins.

Consequences, all intended: at `N = 1` an aggressive limit fills on submit,
which is what a real venue does. At `N >= 2` even an aggressive limit rests
until the market prints through it again. At `N = 0` no reading is taken at all
and the whole mechanism is inert. No reading is taken for MARKET or FOK orders
either (3.9), so the gate never pays for a seek whose result it discards.

### 3.4 The trigger for a late fill, and who owns it

**An ACCOUNT-owned sweeper task, spawned by `AccountRegistry::acquire` and torn
down with the slot.** Not per session. A session-owned sweeper is wrong in four
distinct ways, all of which are consequences of `POST /orders` being a
first-class carrier (2.2):

- An order submitted over HTTP by an account with no websocket would never fill.
- An account whose socket drops stops trading entirely, and its resting orders
  freeze mid-window.
- Reconciliation over `QueryOrders`/`QueryFills` would honestly report an order
  that can never fill, so the venue-truth stores would be truthful about a
  venue that is broken.
- Two sockets on one account would each walk the same tape every interval, with
  one side's results discarded - double cost and a race over who sees the fill.

Per pass the sweeper:

1. Takes the engine lock, asks for the pending scans, releases the lock.
2. Walks the tape OFF the lock, on `spawn_blocking` (the walk is a checkpoint
   restore plus a bounded drain against a process-wide mutex, and both existing
   synthesis callers do the same - 2.3). Holding the engine mutex across it
   would serialize every order command in the account behind it.
3. Takes the lock again and executes whatever crossed its threshold.

Step 3 re-validates under the lock against a per-order REVISION token, not just
liveness: an order that was cancelled, amended, filled, or already advanced by
another pass between steps 1 and 3 is skipped. The engine is the arbiter; the
sweeper is a courier.

Rejected alternative: evaluating the gate lazily on the next client touch
(command or query). It is cheaper and it is dishonest - fills would materialize
only when observed, and a strategy that submits and then waits would see nothing
at all on a streaming socket.

### 3.5 Delivering a fill nobody asked for

Execution is account-scoped; DELIVERY stays per session, because `ExecLanes` is
per session and this spec does not rebuild event topology (7).

The sweeper executes under the engine lock and then, for each session currently
registered on the slot, reserves on THAT session's lanes and submits the batch.
A session whose reservation is refused gets the existing `AdmissionRejected`
refusal on its priority lane and learns the real state from
`QueryOrders`/`QueryFills`. It does NOT roll the execution back: the fill
happened at the venue, and a client's byte budget is not allowed to decide
whether the market traded through a price. That is the inversion of 3.11's
earlier draft and it is what makes the permanent-wedge failure impossible.

An account with zero sessions still sweeps and still books fills into the truth
stores. That is the whole point of account ownership: an HTTP-carrier client
sees them on its next reconciliation query, which is now a true statement rather
than the one revision 1 made (7).

### 3.6 When the divergences apply

`PartialFillNext`, `DuplicateNextFill` and `DropNextAccountUpdate` move from
submit time to EXECUTION time, because that is where the fill now happens. At
`N = 0` execution IS submit, so nothing observable changes. Concretely, on the
gated-rest branch the acceptance `AccountState` must NOT consume
`DropNextAccountUpdate`: the divergence is armed against the fill's account
update, and letting the accept snapshot eat it would both contradict 3.6 and
make the G2 test that asserts it survives unpassable.

The FOK gate stays at submit time and cannot move: FOK never rests, so a FOK
order is executed or rejected within `on_submit` regardless of the gate (3.9).

This forces the plan/commit split in 3.11.

### 3.7 What execution does to the count

A fill RESETS the order's penetration state: `penetration_count = 0`,
`penetration_scanned_ns = ts`, `revision += 1`. Without that reset a partially
filled order (an armed `PartialFillNext`, or a quantity amend upward after a
fill) rests at exactly `penetration_count == penetration_ticks` and the next
pass fills the remainder with zero further penetrations - the gate would leak
open on precisely the orders it was most supposed to hold.

The remainder is therefore gated afresh, which is also the honest model: each
tranche has to be traded through on its own.

### 3.8 What an amend does to the count

- A PRICE amend RESETS the count to zero and restarts the window at the amend
  instant. The order is at a new price; penetrations of the old one are not
  evidence about the new one.
- A QUANTITY-only amend PRESERVES the count. The price the market has to trade
  through has not moved.
- Either way `revision += 1`, so a walk in flight over the old state is
  discarded (3.4).

That split is what `on_modify` already knows (it distinguishes `price.is_some()`
from a quantity-only amend), so no new plumbing is needed - only the reset.

### 3.9 Order types and TIF under the gate

- MARKET: never gated. It is marketable by definition and already fills at the
  synthesized market price. No penetration reading is taken.
- FOK: never gated. All-or-nothing is judged at submit; an order that cannot
  fill NOW is rejected, and "now" is not something a gate can defer. No reading
  is taken.
- IOC: GATED, evaluated exactly once at submit against the seeded count from
  3.3, then resolved immediately - filled if the seed meets
  `penetration_ticks`, otherwise `OrderCanceled` for the whole quantity. An IOC
  never rests, so it is never swept. This is the resolution of the revision-1
  contradiction between 3.9 and the target predicate: an IOC that ignored the
  gate would fill at its own price against a market that never reached it,
  which is the exact lie the spec exists to remove, and it would make an
  unmarketable IOC fill rather than cancel.
- GTC LIMIT: the gated case, and the only one that reaches the sweeper.

So `gated = penetration_ticks > 0 && order_type == Limit && matches!(tif, Gtc |
Ioc)`, and a market reading is taken exactly for that set.

### 3.10 The bound on sweep work

Three separate bounds, because revision 1 claimed one that does not exist:

1. **Per order per pass, the SPAN is `(scanned_from_ns, now_ns]`** - the span
   since the previous pass, not since the order rested - because the engine
   persists the scan frontier.
2. **The DRAIN carries its own budget, `SWEEP_DRAIN_BUDGET` (a new const in
   `fills.rs`, 20,000 ticks).** `BoundedSeek` caps only its `seek_to`; its
   `next_tick` is uncapped and `GeneratedSource` never ends (2.3), so a walk
   that relied on it would grind forever on a far-away order or a long
   reconnect gap. The budget makes the walk terminate and REPORT where it got
   to, so the frontier advances over exactly what was examined (4.5).
3. **Per pass, work is per SYMBOL, not per order.** Every pending order on a
   symbol shares one span and one clean tape, so one walk evaluates all of
   their predicates. Fifty resting limits on BTCUSDT cost one checkpoint
   restore per pass, not fifty, and the process-wide checkpoint mutex is taken
   once per symbol rather than once per order.

At the default 100 ms interval and the fitted BTCUSDT cadence the per-pass drain
is single-digit ticks. G5 measures it rather than asserting it.

The sweep interval is a SIM window converted through `SimClock::wall_duration`
like every other window here, but the wall sleep is floored at
`MIN_SWEEP_WALL_MS` (5 ms). Under an accelerated clock `wall_duration` shrinks
linearly while the per-pass fixed cost (checkpoint restore, lock round-trip)
does not, so an unfloored sweep at `speed = 100` is a 1 ms hot loop - and G3
proposes exactly that configuration. The floor costs sim-time resolution the
gate does not need and buys a cost that stays bounded in wall time.

### 3.11 The fill executor, and the FOK ordering it must not break

Revision 1 specified one `execute_fill(submit, venue_order_id, ts)` and also
said steps 3-5 move inside it and that FOK still runs before the id reservation.
Those are mutually exclusive: step 5 IS the FOK gate, and an executor taking a
`VenueOrderId` can only run after `next_id("V")` and the `seen_client_order_ids`
insert - so a rejected FOK would burn its client id and break the documented
"only an ACCEPTED order reserves its id" invariant, which is an N=0 behaviour
change and exactly the class of regression L2's keep/revert verdict exists to
catch. FOK also cannot be judged before sizing, because it is judged against the
post-`PartialFillNext` size, and `fill_fraction` must not be consumed twice.

The split: `plan_fill` (pure sizing, consumes the targeted `PartialFillNext`,
needs no venue id, mutates nothing else) then `commit_fill` (needs the venue id,
emits, books, consumes `DuplicateNextFill`). `on_submit` runs
`plan_fill` -> FOK gate -> id reservation -> `OrderAccepted` -> `commit_fill`,
which is today's order verbatim. The sweep path runs `plan_fill` ->
`commit_fill` back to back on an order that already has its venue id.

`plan_fill` sizes against the REMAINING quantity, not `submit.quantity`. On the
submit path those are equal; from the sweep path the resting order may already
be partly filled (3.7) or have been amended, and multiplying the fraction by the
original quantity would over-fill. The emitted `OrderFilled.leaves_qty` and the
`OpenOrder.leaves_qty` update are both derived from that remaining quantity.

### 3.12 Composition with `CommandLatency`

Free, and it is the compounding the TODO calls for: an armed `submit_act_ms`
delays the moment the venue ACTS, `ts_accepted` is stamped at act time, and the
penetration window opens there. The order was late AND has to be traded through.
No code in this spec is aware of `CommandLatency`; the ordering in
`process_order_cmd` (act sleep, then reading, then `ts` re-sample, then engine)
already produces it.

### 3.13 The knob lives in `mogwai.toml`, not on the control plane

`penetration_ticks` is a model parameter of the venue, not a fault injected into
a run. Faults go through `control::Divergence`; run configuration goes through
`mogwai.toml` (the workspace is deliberately env-var-free for runtime knobs).
Making it a divergence would also mean it could change mid-run, which would
leave already-resting orders gated by a threshold they were never accepted
under - a state with no honest reading.

## 4. Target artifacts

### 4.1 `crates/mogwai-server/src/config.rs`

Two new `Config` fields (flat, `#[serde(default, deny_unknown_fields)]` like
every other):

```rust
/// Trades that must print THROUGH a resting limit's price before the venue
/// fills it. `0` - the default - is the bookless venue's historical
/// behaviour: a limit fills on submit at its own price, untouched by the
/// tape. Any positive value turns resting limits into orders the market has
/// to come to, gated on TRADED prices rather than quotes, because this
/// venue's corpus, generator and `/quotes` surface are all trades-only.
pub(crate) penetration_ticks: u32,

/// How often, in sim milliseconds, an ACCOUNT re-checks its resting limits
/// against the tape. Read only when `penetration_ticks > 0`. `0` disables
/// the sweep, and with the sweep off a gated resting order can NEVER fill:
/// a submit seeds only its own order, so nothing else ever advances a
/// penetration count. Boot refuses that combination rather than shipping a
/// venue that accepts limits it will never execute.
pub(crate) fill_sweep_interval_ms: u64,
```

`Config::default()`: `penetration_ticks: 0`, `fill_sweep_interval_ms: 100`.

New `validate_penetration(cfg) -> anyhow::Result<()>`, called from the same boot
sequence as `validate_admission_limits` / `validate_account_lifecycle`:

- `penetration_ticks <= MAX_PENETRATION_TICKS` (1_000, a new named const in this
  module). Above that no realistic tape ever fills a resting order and the
  venue would silently be a black hole; refusing at boot says so.
- `fill_sweep_interval_ms <= control::MAX_DIVERGENCE_MS`, reusing the one-hour
  ceiling every other sim-ms window in this workspace is bounded by.
- ERROR (not a warn) when `penetration_ticks > 0 && fill_sweep_interval_ms ==
  0`: that pair accepts gated limits nothing can ever fill.
- WARN when `penetration_ticks > 0 && fill_sweep_interval_ms >
  SLOW_SWEEP_WARN_MS` (60_000). An hour-long interval is functionally the `0`
  case and deserves to be named as such rather than passing silently under the
  `MAX_DIVERGENCE_MS` ceiling.

Root `mogwai.toml` gains both keys at their defaults with a comment pointing at
`reference/config.md`.

### 4.2 `crates/mogwai-protocol/src/sizing.rs`

```rust
/// Upper bound on one penetration sweep's output: per executed order, the
/// fill plus its possible `DuplicateNextFill` twin, and ONE `AccountState`
/// for the whole batch (the sweep snapshots once, after every fill it
/// booked).
///
/// The account is sized against a shape widened PER ORDER, not per batch:
/// a sweep can execute `orders` fills across `orders` distinct pairs, and
/// each first fill in a new pair introduces up to two currencies and one
/// position the pre-sweep snapshot never had. Widening by a flat `+2/+1`
/// (the single-command `SubmitOrder` case) under-reserves any multi-symbol
/// batch, which is exactly the domination failure the held-byte budget
/// exists to prevent.
#[must_use]
pub fn penetrated_fill_max_bytes(shape: &BookShape, orders: usize) -> usize {
    orders * 2 * ORDER_EVENT_MAX_BYTES
        + account_state_max_bytes(&BookShape {
            balances: shape.balances + 2 * orders,
            positions: shape.positions + orders,
            ..*shape
        })
}
```

`orders` is the count of orders the sweep will actually EMIT for, never the
count of pending scans: an incomplete scan and a scan below its threshold
produce no bytes, and reserving for them would inflate the batch without bound
(4.6 step 5).

### 4.3 `crates/mogwai-engine/src/lib.rs`

`OpenOrder` gains three fields:

```rust
/// Trades that have printed THROUGH `submit.price` since this order was
/// accepted, since its last REPRICE, or since its last FILL - a price
/// amend and an execution both restart the window, a quantity amend does
/// not. Compared against the engine's `penetration_ticks`; at the default
/// 0 it is written once and never read.
pub penetration_count: u32,
/// Sim unix-ns instant the penetration walk has already covered, exclusive
/// lower bound for the next pass. Advanced by the ENGINE when it accepts a
/// count, never by the counter: a walk whose result is discarded must
/// re-cover the same span rather than lose it.
pub penetration_scanned_ns: u64,
/// Bumped on every mutation of this order's identity for gating purposes -
/// reprice, quantity amend, fill, frontier advance. A `ScanResult` carries
/// the revision its walk was planned against, so a result computed against
/// state that has since moved is DROPPED rather than applied. Liveness
/// alone is not enough: two overlapping walks (a slow one and a fast one)
/// can both name a still-resting order, and applying both double-counts
/// the span they share and fills early.
pub revision: u64,
```

Construction replaces the ad-hoc constructors with one config value, so the gate
cannot be forgotten at a call site:

```rust
pub struct EngineConfig {
    pub account_id: AccountId,
    pub instruments: Vec<InstrumentDef>,
    pub balances: HashMap<String, Decimal>,
    pub penetration_ticks: u32,
}
```

- `EngineConfig::unbound(instruments)` - `UNBOUND_ACCOUNT_ID`, no balances, gate
  off.
- `Engine::new()` stays, delegating to `EngineConfig::unbound(default_instruments())`,
  so the ~40 engine tests that call it are untouched.
- `Engine::build(EngineConfig) -> Engine`.
- `with_instruments` and `with_instruments_and_balances` are DELETED, not kept
  as shims - a shim is a second way to build an engine with the gate off by
  accident, which is the failure this consolidation exists to prevent. Their
  call sites, all of which are updated in the same landing:
  `AccountRegistry::acquire`, `AccountSlot::detached_for_tests`, and the engine
  and server tests that name either constructor directly (enumerate with
  `rg with_instruments` before starting; the count is small and pre-1.0 breakage
  is legal).

New engine surface:

```rust
/// One resting order the caller must count penetrations for, and how many
/// are still owed. Ordered by `ts_accepted` so a sweep's work is stable.
pub struct PendingScan {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    /// The resting limit's price. Always present: a resting LIMIT is
    /// validated to carry one, so the scan surface hands the counter a
    /// `Decimal` rather than re-litigating `submit.price`'s `Option`.
    pub price: Decimal,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
    /// Penetrations still required. Never zero: an order at its threshold
    /// is executed, not returned.
    pub remaining: u32,
    /// The order state this scan was planned against. Echoed back on the
    /// `ScanResult` and checked under the lock.
    pub revision: u64,
}

/// Every resting GTC limit whose gate is not yet satisfied. Empty when
/// `penetration_ticks == 0`, which is what makes the sweeper free to not
/// even spawn on a default venue.
pub fn pending_scans(&self) -> Vec<PendingScan>;

/// Result of one walk, handed back for the engine to apply under the lock.
pub struct ScanResult {
    pub client_order_id: ClientOrderId,
    /// Echoed from the `PendingScan`: the span's exclusive lower bound and
    /// the order revision the walk assumed. Both are checked before the
    /// result is applied.
    pub from_ns: u64,
    pub revision: u64,
    /// Penetrations observed in `(from_ns, scanned_to_ns]`.
    pub counted: u32,
    /// The instant the walk ACTUALLY reached, which its drain budget may
    /// have cut short of the pass's `to_ns`. The frontier advances to
    /// exactly this, never past it.
    pub scanned_to_ns: u64,
}

/// Apply a batch of walk results and execute whatever the gate now admits.
///
/// Each result is matched back to a still-resting order whose `revision`
/// and `penetration_scanned_ns` both still equal what the walk was planned
/// against; anything cancelled, filled, repriced, amended or already
/// advanced since is dropped, which is what makes the off-lock walk safe.
/// Returns the batch's events and the count of orders it actually emitted
/// for, which is what the caller reserves delivery bytes against.
pub fn apply_scans(&mut self, results: &[ScanResult], ts: u64)
    -> (Vec<ServerMessage>, usize);
```

### 4.4 `crates/mogwai-engine/src/orders.rs`

`on_submit` is restructured around a plan/commit pair (3.11):

```rust
/// The size this fill WOULD be, and nothing else. Consumes the targeted
/// `PartialFillNext`, clamps and floors onto the size grid against
/// `remaining` (the order's leaves, not its original quantity - a swept
/// order may already be partly filled). Mutates no ledger and needs no
/// venue id, so `on_submit` can judge FOK against its answer while a
/// rejected FOK still leaves the client order id unreserved.
fn plan_fill(&mut self, submit: &SubmitOrder, remaining: Decimal) -> Decimal;

/// Emit and book a planned fill: the `OrderFilled` at the ORDER'S price,
/// `apply_fill`, `record_fill`, and the `DuplicateNextFill` consumption.
/// Called from `on_submit` for an ungated (or already-penetrated) order and
/// from `apply_scans` for one the tape has now traded through, so the two
/// paths cannot diverge in WHAT they produce, only in when.
fn commit_fill(
    &mut self,
    submit: &SubmitOrder,
    venue_order_id: &VenueOrderId,
    last_qty: Decimal,
    leaves_qty: Decimal,
    ts: u64,
) -> Vec<ServerMessage>;
```

`Engine::process` grows a sibling that carries the tape's opinion:

```rust
/// `process`, plus the venue's last traded price at or before `ts` for the
/// command's symbol. The server passes `Some` only when the penetration
/// gate is armed and the command is a LIMIT submit with a resting-capable
/// or IOC time in force; the engine uses it to seed `penetration_count`
/// for an order already marketable on arrival, so an aggressive limit does
/// not have to wait for a sweep. `process` delegates here with `None`,
/// which is exactly the ungated venue.
pub fn process_with_market(
    &mut self,
    msg: ClientMessage,
    ts: u64,
    market_px: Option<Decimal>,
) -> Vec<ServerMessage>;
```

`on_submit(order, ts, market_px)` then reads:

- Steps 1 and 2 (validate, `RejectNextSubmit`) unchanged.
- `let gated = self.penetration_ticks > 0
      && order.order_type == OrderType::Limit
      && matches!(order.time_in_force, TimeInForce::Gtc | TimeInForce::Ioc);`
- `let limit = order.price.expect("validated limit carries a price");`
  `let seeded = if gated { u32::from(market_px.is_some_and(|px| through(order.side, limit, px))) } else { 0 };`
- If `gated && seeded < self.penetration_ticks`:
  - Reserve the id, emit `OrderAccepted`.
  - GTC: rest the order with `penetration_count: seeded`,
    `penetration_scanned_ns: ts`, `revision: 0`.
  - IOC: emit `OrderCanceled` for the whole quantity and `record_closed`
    (3.9). It never rests and is never swept.
  - Emit `AccountState` WITHOUT consuming `DropNextAccountUpdate` - that
    divergence belongs to the eventual fill (3.6).
  - No fill event, and no `PartialFillNext` consumption: `plan_fill` is not
    called, so the divergence stays armed for the execution it targets.
- Otherwise: today's path verbatim - `plan_fill`, FOK gate, id reservation,
  `OrderAccepted`, `commit_fill`, routing, `DropNextAccountUpdate` /
  `AccountState`.

The FOK branch keeps running before the id reservation, which the plan/commit
split is what makes possible.

`on_modify` adds to the mutation block it already has:

```rust
order.revision += 1;
if price.is_some() {
    order.submit.price = price;
    // A reprice restarts the window: penetrations of the OLD price are
    // not evidence about the new one. A quantity-only amend keeps the
    // count, because the price the market must trade through has not
    // moved. Either way the revision bump discards any walk already in
    // flight against the pre-amend state.
    order.penetration_count = 0;
    order.penetration_scanned_ns = ts;
}
```

`through` is a free function next to `on_increment`:

```rust
/// Has the market traded THROUGH a resting limit at `limit`? Strict on
/// purpose: a print AT the limit is the market touching, not trading
/// through, and at-touch filling is the fidelity failure the gate exists
/// to remove.
fn through(side: Side, limit: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded < limit,
        Side::Sell => traded > limit,
    }
}
```

### 4.5 `crates/mogwai-server/src/fills.rs` (new module)

```rust
/// Ticks one sweep pass may drain per symbol before it reports where it
/// got to and stops. `BoundedSeek` caps only its `seek_to`; its
/// `next_tick` delegates uncapped and `GeneratedSource` never ends, so the
/// drain needs its own budget or a far-from-market order walks forever.
/// 20,000 is two orders of magnitude above the default 100 ms interval's
/// expected handful of ticks at the fitted BTCUSDT cadence, and still
/// terminates a multi-hour reconnect gap in bounded work across several
/// passes.
pub(crate) const SWEEP_DRAIN_BUDGET: usize = 20_000;

/// One order's predicate for a batched walk.
pub(crate) struct Probe {
    pub(crate) client_order_id: ClientOrderId,
    pub(crate) side: Side,
    pub(crate) price: Decimal,
    pub(crate) from_ns: u64,
    pub(crate) needed: u32,
}

/// What one symbol's walk found.
pub(crate) struct Walk {
    /// Per probe, in the input's order: penetrations counted in
    /// `(probe.from_ns, reached_ns]`.
    pub(crate) counted: Vec<u32>,
    /// The instant the drain ACTUALLY reached - `to_ns` when the span was
    /// covered, otherwise the `ts_event` of the last tick examined before
    /// `SWEEP_DRAIN_BUDGET` was spent. The caller advances each frontier
    /// to exactly this and never past it, so a truncated pass loses
    /// nothing and the next pass resumes where this one stopped.
    pub(crate) reached_ns: u64,
}

/// Count penetrations for every probe on one symbol in one walk of the
/// CLEAN tape.
///
/// ONE walk per symbol, not per order: every resting limit on a symbol
/// shares the same tape and the same pass span, so a per-order walk would
/// pay a checkpoint restore and a process-wide mutex acquisition per order
/// per interval - fifty resting limits at 100 ms would be five hundred
/// restores a second contending with `/trades` and market-price stamping.
/// The probes' `from_ns` may differ (orders rest at different instants);
/// the walk starts at the EARLIEST and each probe counts only ticks after
/// its own bound.
///
/// The clean tape, not a regime'd realization: an armed `MarketRegime` is
/// per-subscription (`TapeKey` carries it, see `tape.rs`) while an order
/// belongs to an account, so there is no single regime an order could be
/// gated under. A scenario that arms a drought silences its own DATA feed
/// and leaves its fills on the venue's canonical tape, which is stated in
/// `reference/havoc.md` alongside the same property for market-order price
/// stamping.
///
/// Composed from the same `build_history_source` the `/trades` cursor
/// pages through, so the prints gating a fill are the prints the client
/// can fetch and check. Returns `None` when the positioning seek could not
/// reach the earliest bound within `MAX_HISTORY_SEEK_TICKS`; the caller
/// then leaves every frontier unadvanced rather than treating an
/// unreachable span as zero penetrations.
///
/// Synchronous and CPU-bound. Callers run it on `spawn_blocking`, as
/// `stamp_market_price` and the `/trades` handler already do for the same
/// synthesis.
pub(crate) fn count_penetrations(
    symbol: &str,
    probes: &[Probe],
    to_ns: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Walk>;

/// The last trade printed at or before `ts` on the clean tape.
///
/// NOT `source::current_price`: that returns the first tick at or AFTER
/// sim-now (its own gate asserts `ts_event >= sim_now`), which is right for
/// stamping a MARKET order against what a live subscriber sees and wrong
/// for deciding whether a limit was marketable when the venue accepted it -
/// seeding a penetration from a trade that has not printed yet is a
/// look-ahead leak and contradicts the `(from, now]` window. Walks back
/// from the checkpoint at or before `ts` and returns the last tick with
/// `ts_event <= ts`, or `None` when the budget is spent first, in which
/// case no penetration is seeded.
pub(crate) fn last_trade_at_or_before(
    symbol: &str,
    ts: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
) -> Option<Decimal>;
```

Implementation of `count_penetrations`: `build_history_source(symbol,
Some(earliest_from.saturating_add(1)), None, profiles, data_origin)`, then drain
while `ts_event <= to_ns` and `drained < SWEEP_DRAIN_BUDGET`, for each
`TickEvent::Trade` incrementing every probe whose `from_ns < t.ts_event` and
`through(probe.side, probe.price, t.price)`, tracking `reached_ns`, and
returning early once every probe has hit its `needed`.

### 4.6 `crates/mogwai-server/src/sweeper.rs` (new module)

```rust
/// The venue re-checking one ACCOUNT's resting limits against the tape.
///
/// Spawned by `AccountRegistry::acquire`, and ONLY when
/// `penetration_ticks > 0`, so a default venue pays nothing at all - not a
/// task, not a timer, not a lock acquisition. Lives as long as the slot:
/// account-owned, not session-owned, because `POST /orders` is a
/// first-class carrier and a session-owned sweeper would leave an
/// HTTP-only account's orders unfillable and freeze a disconnected
/// account's book (3.4).
///
/// Three phases per pass, and the split is load-bearing: the tape walk in
/// phase two costs a checkpoint restore plus a bounded drain, so it runs
/// OFF the engine lock and on `spawn_blocking` or it stalls both this
/// account's commands and a runtime worker. The engine re-validates every
/// result against its order revision in phase three, which is what makes
/// the off-lock gap safe.
pub(crate) fn spawn_account_sweeper(sweep: FillSweep) -> tokio::task::JoinHandle<()>;

pub(crate) struct FillSweep {
    pub(crate) slot: Arc<AccountSlot>,
    pub(crate) sim: SimClock,
    pub(crate) profiles: Arc<InstrumentProfiles>,
    pub(crate) data_origin_ns: u64,
    pub(crate) interval_ms: u64,
}
```

Loop body:

1. `sleep(max(sim.wall_duration(sim_duration_from_millis(interval_ms)),
   MIN_SWEEP_WALL_MS))` - the floor is 3.10's protection against an accelerated
   clock turning the sweep into a hot loop.
2. Exit when `slot.tombstoned` is set or `slot.closed` fires.
3. `let scans = { slot.engine.lock().await.pending_scans() };` - lock released
   immediately. `continue` on empty.
4. `to_ns = sim_now_ns(sim)` sampled ONCE for the pass, so every order is judged
   against the same instant. Group `scans` by symbol, and for each symbol run
   one `fills::count_penetrations` on `spawn_blocking`. A `None` walk yields no
   `ScanResult` for that symbol at all (nothing advances). Otherwise each scan
   becomes a `ScanResult { from_ns, revision, counted, scanned_to_ns:
   walk.reached_ns }`.
5. Re-lock. Bail on `slot.tombstoned`. `let (events, emitted) =
   engine.apply_scans(&results, ts);` then `let shape = engine.book_shape();`
   and drop the lock. `continue` when `events` is empty.
6. Deliver per session: for each `ExecLanes` registered on the slot (4.8),
   `lanes.reserve_penetrated(&shape, emitted)` and
   `lanes.submit_produced(reservation, Instant::now(), None, events.clone())`.
   `class: None` because no order command produced this batch, so no per-command
   ACK latency applies; an armed `DelayAcks` still shifts it, which is correct -
   it is an execution frame. A session whose reservation is refused gets the
   existing admission refusal on its priority lane and reconciles; the
   EXECUTION is not rolled back (3.5). A `LaneClosed` deregisters that session
   and the sweep continues.

`emitted`, not `results.len()`, is what sizes the reservation: reserving for
every pending scan would grow the request with the open-order count while the
per-session budget stays fixed, and since the execution no longer depends on the
reservation there is nothing left to wedge - but an inflated request would still
refuse deliveries that comfortably fit.

`ExecLanes::reserve_penetrated(&self, shape: &BookShape, orders: usize) ->
Option<Reservation>` is added to `admission.rs`, delegating to
`held_budget.try_reserve(penetrated_fill_max_bytes(shape, orders))`.

### 4.7 `crates/mogwai-server/src/http.rs`

`stamp_market_price` grows the gated-limit case and returns the reading
alongside the command rather than only folding it in:

```rust
/// The venue's reading of the market for this command, sampled between the
/// protocol boundary and the engine, on `spawn_blocking`.
///
/// For a price-less MARKET order it is `source::current_price` and is
/// STAMPED onto the order (unchanged). For a gated LIMIT submit it is
/// `fills::last_trade_at_or_before(ts)` - a different function on purpose
/// (4.5) - and it is returned but NOT stamped: the order keeps its own
/// price, and the reading only tells the engine whether the order was
/// already marketable when the venue accepted it. When the gate is off, or
/// the order is MARKET or FOK, no reading is taken and no seek is paid for.
async fn market_reading(msg: ClientMessage, state: &AppState, ts: u64)
    -> (ClientMessage, Option<Decimal>);
```

`process_order_cmd` replaces its `stamp_market_price` call with this, keeps the
`ts` re-sample and the price-less-MARKET reject exactly as they are, and calls
`engine.process_with_market(order_cmd, ts, market_px)`.

### 4.8 `crates/mogwai-server/src/accounts.rs` and `ws.rs`

`AccountSlot` gains a session delivery registry and the sweeper's handle:

```rust
/// The `ExecLanes` of every session currently bound to this account, so an
/// account-owned producer (the fill sweeper) can deliver to all of them.
/// Sessions register in `handle_socket` and deregister on the same drop
/// that releases the `SessionLease`, so a dead socket's lanes cannot
/// accumulate. A `std::Mutex` (never held across an await) rather than an
/// async one: every `ExecLanes` method is non-blocking by construction.
pub(crate) sessions_lanes: std::sync::Mutex<Vec<(u64, ExecLanes)>>,
/// The account's fill sweeper, present only when the gate is armed.
/// Aborted and awaited by `AccountRegistry::destroy` / `reap_idle`
/// alongside the tombstone, so a removed account stops walking the tape.
pub(crate) sweeper: std::sync::Mutex<Option<JoinHandle<()>>>,
```

`SessionLease` grows the registration: `acquire` inserts `(session_id, lanes)`
and `Drop` removes that id, so the existing lifetime hook does the work and
there is no second thing to remember.

`AccountRegistry::acquire` builds its `Engine` through `EngineConfig` with
`penetration_ticks` carried on `AccountTemplate` from `Config`, and spawns
`spawn_account_sweeper` when `penetration_ticks > 0 && fill_sweep_interval_ms >
0`. `handle_socket` gains no sweeper task at all.

### 4.9 `scripts/smoke.py` and `scripts/smoke-penetration.toml`

New fixture `scripts/smoke-penetration.toml`:

```toml
# Penetration-gated fills: one print through the limit is enough, and the
# sweep runs fast so the scenario does not have to wait a human interval.
speed = 1.0
penetration_ticks = 1
fill_sweep_interval_ms = 50
```

New `main_penetration()` in `scripts/smoke.py`, wired into `main()` under
`--penetration` and documented in the module docstring alongside the other four
scenarios. It must NOT reuse `submit_order`'s fixed price-100 shape (2.6), and
it must anchor on a LIVE tick, not on the tape head:

1. Read `sim_now` from `GET /clock`, then
   `fetch_trades("BTCUSDT", start=sim_now - 60s, limit=200)` and take the LAST
   returned trade as `px`. `fetch_trades(..., None, 1)` is wrong: `start=None`
   means "from ORIGIN" in the `/trades` handler, so it returns the oldest tick
   on the tape, which at a 24 h backfill horizon is nowhere near the current
   level.
2. Submit a BUY limit at `px * 0.5`, quantity `0.01`, rounded onto the
   instrument grid from `GET /instruments`. Assert `OrderAccepted` and NO
   `OrderFilled` within 2 s, and that `QueryOrders` reports it `Accepted` with
   `filled_qty` zero. This is the assertion that could not be made before this
   spec: the venue refused to fill an order the market never reached.
3. Submit a BUY limit at `px * 1.5` (marketable on arrival). Assert an
   `OrderFilled` arrives in the SAME batch as the accept - the 3.3 path - and
   that its `last_px` is the ORDER'S price, `px * 1.5`, not the market's
   (deviation 2 of section 1).
4. The DETERMINISTIC late fill. Do not price a sell "a few increments above the
   market" and wait: at `MAX_ABS_RETURN = 2e-5` per tick, whether the random
   walk travels that far in any bounded interval is not a property the fixture
   controls (2.6). Construct a crossing that MUST happen instead: run this leg
   against a second fixture, `scripts/smoke-penetration-two.toml`, identical
   but for `penetration_ticks = 2`, and submit a BUY limit at `px * 1.5`. The
   3.3 seed grants one penetration on arrival, so the order rests one short of
   its threshold; every subsequent print is also below `px * 1.5`, because
   `MAX_ABS_RETURN = 2e-5` per tick cannot move the tape 50% in the interval,
   so the first sweep pass after the next print fills it. Neither the direction
   nor the magnitude of the walk can defeat the assertion. Assert the swept
   `OrderFilled` arrives unsolicited, with `last_px` equal to the ORDER'S
   price, and that it did NOT arrive in the accept's batch. Two fixtures are
   cheaper than one scenario that has to reboot the server: step 3's
   same-batch assertion stays on `smoke-penetration.toml`
   (`penetration_ticks = 1`), this leg runs under `--penetration-swept`.
5. Assert the order from step 2 is still resting at the end, so the run proves
   the gate both holds and releases.
6. Assert the HTTP carrier too, which is what makes 3.4's ownership claim
   testable end to end: submit the step-4 order over `POST /orders` with NO
   websocket bound to that account, wait past a sweep interval, and assert
   `GET /orders` (the `QueryOrders` truth store) reports it `Filled`. A
   session-owned sweeper fails exactly here.

### 4.10 Documentation

- `reference/architecture.md`, the "Fills are synthetic" bullet: rewritten to
  state the default (immediate full fill at the order's own price, `N = 0`) and
  the gate, INCLUDING both deviations from the RFC in 1 - the traded-price
  predicate and the unchanged fill price - because that divergence is invisible
  at the call site and must not live only in a TODO. The "Time in force" bullet
  gains the 3.9 table. A new bullet under the server section describes the
  account-owned sweeper, that execution is account-scoped while delivery is
  per session, and that a fill is booked into venue truth whether or not any
  session could take the frame (3.5).
- `reference/config.md`: `penetration_ticks` and `fill_sweep_interval_ms`, the
  boot validation including the refused `ticks > 0, interval == 0` pair, and the
  price-scale trap from 2.6 (a gated scenario must price its orders off the
  tape, not off 100).
- `reference/havoc.md`: the sweep executes on the CLEAN tape regardless of an
  armed data regime, alongside the same note already made for market-order
  price stamping.
- `notes/todo.md`: the penetration entry is REMOVED ENTIRELY, in the same
  landing that ships the feature (L2), so no landing boundary leaves a TODO
  describing shipped code. The phase B entry is edited to note that the
  queue-ahead decision now has `count_penetrations` as its ready-made counting
  primitive; the phase D entry is edited to note that fills are now
  tape-dependent, so golden fill distributions have become meaningful.

## 5. Verification, per brick

Every gate below is a copy-pasteable command.

### G1 - the counting primitive

`crates/mogwai-server/src/fills.rs` unit tests, against the deterministic
generated tape at `TEST_ORIGIN` (the convention `source.rs` tests already use):

- `counts_only_prints_strictly_through_the_limit` - a limit placed exactly at a
  known print's price counts zero for both sides; a limit one tick ABOVE the
  print counts it for a BUY (`traded < limit`) and not for a SELL, and a limit
  one tick BELOW counts it for a SELL and not for a BUY. (Revision 1 stated
  this backwards, which is the kind of error a test written from the spec would
  have inherited.)
- `a_span_with_no_penetration_counts_zero` - a buy priced far below the tape
  over an hour of tape.
- `counting_stops_at_the_threshold` - `needed = 1` over a span containing many
  penetrations returns 1, proving the early exit.
- `an_unreachable_span_reports_none` - a `from_ns` beyond the seek budget.
- `a_truncated_drain_reports_where_it_stopped` - a span long enough to exhaust
  `SWEEP_DRAIN_BUDGET` returns `reached_ns < to_ns`, and the counts are exactly
  those of the prefix it covered. This is the gate on the `complete` bug: a
  primitive returning `Option<u32>` could not express this at all, so the
  frontier would advance over unwalked tape and silently drop penetrations.
- `one_walk_serves_every_probe_on_a_symbol` - three probes with different
  `from_ns` and prices in one call agree, order for order, with three
  single-probe calls.
- `the_counted_prints_are_the_prints_trades_serves` - the same span fetched
  through `build_history_source` and filtered by hand agrees. This is the gate
  on the honesty claim in 4.5.
- `last_trade_at_or_before_never_looks_ahead` - the returned price is the price
  of a tick with `ts_event <= ts`, and it differs from
  `source::current_price(ts)` on a span where a tick lands after `ts`. This is
  the gate on 3.3.

```
brokkr check -p mogwai-server
```

### G2 - engine semantics

New tests in `crates/mogwai-engine/src/lib.rs`:

- `zero_penetration_ticks_fills_on_submit_exactly_as_before` - the default
  engine's output for a plain submit is byte-identical to the pre-change
  expectation (accept, fill at the order price, account state).
- `a_rejected_fok_still_does_not_reserve_its_client_order_id` - submit a FOK
  the armed `PartialFillNext` shrinks below full, assert the reject, then
  resubmit the SAME `client_order_id` and assert it is accepted. This pins the
  invariant the plan/commit split exists to protect (3.11), and it is the
  regression a single `execute_fill` would have shipped.
- `a_gated_limit_rests_without_a_fill_event` - `penetration_ticks: 1`, no
  market reading: accept + account state only, order resting, `leaves_qty`
  whole, no `OrderFilled` on the wire.
- `a_marketable_limit_fills_on_arrival_under_a_gate_of_one`.
- `a_marketable_limit_still_rests_under_a_gate_of_two`.
- `apply_scans_fills_an_order_the_tape_traded_through` - a `ScanResult` meeting
  the threshold yields accept-free fill + account state, and the order leaves
  the book with a `Filled` terminal record a `QueryOrders` reports.
- `apply_scans_accumulates_below_the_threshold` - two passes of one
  penetration each fill at `penetration_ticks: 2` and not before.
- `a_truncated_scan_advances_only_what_it_covered` - a `ScanResult` whose
  `scanned_to_ns` is short of the pass's `to_ns` moves the frontier to exactly
  `scanned_to_ns` and credits exactly `counted`.
- `a_scan_against_a_stale_revision_is_dropped` - two `ScanResult`s over
  overlapping spans for one order: applying the first bumps the revision, and
  the second is discarded rather than double-counted into an early fill. Also
  the cancelled, repriced, and already-advanced cases.
- `an_executed_order_restarts_its_penetration_window` - arm `PartialFillNext`,
  sweep-fill the order, and assert the remainder rests at count 0 with the
  frontier at the fill instant, so it does not immediately re-fill (3.7).
- `a_swept_fill_sizes_off_the_remaining_quantity` - a partly filled order swept
  again fills at most its leaves, and the emitted `leaves_qty` matches the
  order's.
- `a_price_amend_restarts_the_penetration_window` /
  `a_quantity_amend_preserves_the_penetration_count` - 3.8.
- `a_partial_fill_divergence_survives_until_the_gated_order_executes` - arm
  `PartialFillNext`, submit gated, assert the queue still holds it after the
  submit, then assert the swept fill is the partial and the remainder rests.
- `a_duplicate_fill_divergence_applies_to_a_swept_fill`.
- `a_dropped_account_update_survives_a_gated_accept_and_applies_to_the_swept_fill` -
  the accept's `AccountState` is emitted and the divergence is still armed; the
  sweep's is the one dropped (3.6).
- `fok_is_never_gated`.
- `a_gated_ioc_cancels_when_the_seed_misses_and_fills_when_it_meets` - 3.9,
  both directions, and it never appears in `pending_scans`.
- `market_orders_are_never_gated`.
- `worst_case_reservation_covers_actual_output` (the existing test) extended to
  cover `penetrated_fill_max_bytes` against a sweep batch spanning THREE
  DISTINCT instrument pairs, none previously held. A single-pair batch cannot
  distinguish per-batch from per-order widening and would have passed against
  the under-reserving bound (4.2).

```
brokkr check -p mogwai-engine
brokkr test -p mogwai-engine penetration
```

### G3 - the server wiring

New tests in `crates/mogwai-server/src/main.rs` (where the tape and socket tests
already live):

- `a_gated_resting_order_is_filled_by_the_sweeper` - boot a server with
  `penetration_ticks = 1`, a fast sweep interval and an accelerated clock, rest
  a limit priced through the tape, and read the unsolicited `OrderFilled` off
  the socket.
- `an_http_only_account_still_fills` - submit over `POST /orders` with no
  websocket bound, then assert `GET /orders` reports `Filled`. The gate on 3.4.
- `a_disconnected_account_keeps_sweeping` - rest a gated order, drop the
  socket, wait past several intervals, reconnect, and assert the fill is in the
  truth store. Also asserts the frontier did not have to re-walk the whole
  disconnect gap (the sweeper never stopped, so there is no gap).
- `a_gated_order_the_tape_never_reaches_never_fills` - the negative, bounded by
  a wall timeout.
- `the_sweeper_is_not_spawned_on_a_default_venue` - asserted as a pure function
  of config: `spawn_account_sweeper` is called from exactly one place under
  exactly one condition, and a default-config `Engine::pending_scans()` is
  empty. No `#[cfg(test)]` field is added to `AppState` for this; a production
  struct carrying a test-only spawn flag to assert a negative is a worse trade
  than the two assertions above.
- `a_refused_delivery_still_books_the_fill` - a tiny `exec_held_budget_bytes`
  with a large resting book: the session sees an admission refusal, and
  `GET /orders` reports the order `Filled` anyway. This is the gate on 3.5, and
  the direct replacement for revision 1's `a_refused_sweep_reservation_mutates_nothing`,
  which specified the opposite behaviour and would have wedged the book
  permanently once the batch outgrew the fixed per-session budget.
- `a_cancel_racing_a_sweep_wins_or_loses_cleanly` - repeated submit/cancel
  against an active sweeper, asserting the client never sees both an
  `OrderCanceled` and a later `OrderFilled` for the same id.
- `two_sessions_on_one_account_both_see_a_swept_fill` - one execution, two
  deliveries, and exactly one fill in `QueryFills`.

```
brokkr check
```

### G4 - the cost of a sweep

The spec claims (3.10) that a pass costs one checkpoint restore per SYMBOL plus
a drain bounded by one sweep interval of tape. A claim about a hot path is not a
claim until it is measured, so the measurement is part of the landing:

`sweep_pass_walks_only_the_new_span` in `crates/mogwai-server/src/fills.rs`
counts ticks drained for two consecutive passes over a 60-minute-old order and
asserts the second pass drains fewer than 200 ticks (the default 100 ms interval
at the fitted BTCUSDT cadence is single digits, and 200 leaves two orders of
magnitude of headroom while still failing loudly if the frontier is ever ignored
and the walk becomes O(order age)).

`a_pass_costs_one_walk_per_symbol_not_per_order` asserts the same drain count
for a 1-probe and a 50-probe pass over the same span, which is the gate on
3.10's third bound - a per-order walk would multiply it by fifty.

```
brokkr test -p mogwai-server sweep_pass_walks_only_the_new_span
brokkr test -p mogwai-server a_pass_costs_one_walk_per_symbol_not_per_order
```

Proceed/close threshold: if the second pass drains at or above 200 ticks, or the
50-probe pass drains more than the 1-probe pass, the frontier or the batching is
not doing its job and L2 does not land until it is - the alternative is a
sweeper whose per-pass cost grows without bound over a long-lived run or with
the size of the book.

### G5 - the live end-to-end path

```
brokkr run mogwai -- serve -f --config scripts/smoke-penetration.toml
python3 scripts/smoke.py --penetration
```

```
brokkr run mogwai -- serve -f --config scripts/smoke-penetration-two.toml
python3 scripts/smoke.py --penetration-swept
```

and the unchanged default scenario, which is the regression gate on `N = 0`
being byte-identical:

```
brokkr run mogwai -- serve -f
python3 scripts/smoke.py
```

### G6 - the adapter

No adapter change is required and that is a claim to check, not assume:
`handle_order_filled` is dispatched from the generic inbound arm in
`crates/mogwai-adapter/src/client/exec.rs`, not from an in-flight request
table, so an unsolicited `OrderFilled` for an order nautilus believes is
resting is already handled. The gate is the socket-backed suite, which the
plain check cannot see:

```
brokkr check --gate
```

## 6. Landings and ordering

Two landings. The suite is green at the boundary between them, and each is
kept or reverted whole on its own gates.

### L1 - the instrument

`crates/mogwai-server/src/fills.rs` in full (both primitives, the drain budget,
the batched walk) plus G1 and G4. Nothing else in the tree calls it yet, so this
landing changes no venue behaviour at all; it exists so the thing that measures
and counts is laid, and its cost is read, BEFORE the feature that depends on it -
which is `reference/technical-implementation-spec.md`'s standing requirement
that an instrument is laid before the brick it gates, and the requirement
revision 1 violated by putting the live proof after the feature.

If G4's readings fail their proceed/close threshold, L2 is never laid and L1 is
reverted at the cost of one unused module.

Gates: `brokkr check -p mogwai-server`, the two G4 commands.

### L2 - the gate, its wiring, its proof, and its documentation

Everything else: 4.1-4.4 and 4.6-4.10, plus G2, G3, G5, G6.
`penetration_ticks` defaults to 0, so on the SHIPPED configuration this landing
is a refactor (the plan/commit split, the `EngineConfig` constructor, the
`process_with_market` sibling, the slot's session registry) plus dormant
machinery. The default smoke run and the whole existing suite must be unchanged,
which is exactly what makes the keep/revert verdict readable: any behavioural
difference on `N = 0` is a defect in this landing, not a consequence of the
feature.

The documentation and the `notes/todo.md` removal ride in this landing rather
than a third, so no landing boundary leaves `reference/architecture.md`
describing a venue that no longer exists or a TODO describing shipped code.

Gates: `brokkr check`, `brokkr check --gate`, all three G5 scenarios.

Ordering argument: L2 cannot precede L1 (it calls `fills.rs` on every path and
its cost claim would be unpriced), and L1 leaves the tree green on its own
because nothing calls it. An L2 revert returns the venue to today's behaviour
whole, since every new path is unreachable at the default.

## 7. Stopping rule

Explicitly OUT of scope, and named so they are not smuggled in:

- **Queue position / queue-ahead volume.** RFC 4631 phase B, and it has its own
  open TODO entry, including the refusal of a synthesized depth ladder. This
  spec counts prints, never their size.
- **Fill price improvement.** The fill prints at the order's price, as it does
  today. Filling at the penetrating trade's price is a book property.
- **Quote-based gating.** Impossible on this project's lineage; deviation 1 of
  section 1 documents why rather than building a synthetic quote to gate on.
- **Partial fills driven by the tape.** A penetrated order fills exactly as an
  ungated one does - fully, unless a `PartialFillNext` is armed. Tape-driven
  fill sizing is phase B territory.
- **Account-level event TOPOLOGY.** Execution is now account-scoped (3.4) and
  the sweeper delivers to every session currently bound to the account (3.5),
  which is as far as this spec goes. What it does NOT do is make
  command-triggered execution events fan out to sibling sessions: a fill
  produced by session A's `SubmitOrder` still reaches only session A. That
  asymmetry is pre-existing for commands and is deliberately not resolved here;
  closing it is a separate item about event topology.
- **The HTTP transport profile.** `TransportProfile::default()` is `WsStreaming`
  and broadarrow does not override it. An HTTP-carrier client has no channel for
  an unsolicited fill, so it learns of a swept fill from its reconciliation
  query - which is a TRUE statement only because the sweeper is account-owned
  and runs with no session at all (3.4). No new push surface is built for it.
- **Criterion benches and golden fill distributions.** RFC 4631 phase D, its own
  TODO entry. G4 measures the two costs this spec introduces; the standing
  benchmark suite is not this spec's to build.

## 8. Review disposition

Both reviews were validated against the source. Accepted findings and where they
landed:

| Finding | Where folded |
| --- | --- |
| R1 B1 / R2 6 - `execute_fill` cannot hold the FOK gate without burning the client order id | 2.1, 3.11, 4.4 (`plan_fill`/`commit_fill`), G2 `a_rejected_fok_still_does_not_reserve_its_client_order_id` |
| R1 B2 / R2 4 - `penetrated_fill_max_bytes` under-reserves a multi-symbol batch | 4.2 (per-order widening), G2 three-pair extension |
| R1 B3 - nothing resets the count after an execution, so a remainder re-fills free | 3.7, 4.3 `penetration_count` doc, G2 `an_executed_order_restarts_its_penetration_window` |
| R1 B4 - the executor sized off `submit.quantity`, not the leaves | 3.11, 4.4 `plan_fill(remaining)`, G2 `a_swept_fill_sizes_off_the_remaining_quantity` |
| R1 B5 / R2 7a - IOC contradiction between 3.9 and the predicate | 3.9 resolved in favour of gating IOC, 4.4 predicate, G2 IOC test |
| R1 B6 / R2 1 - a session-owned sweeper cannot fill an HTTP order and freezes a disconnected account | 3.4, 3.5, 4.6, 4.8, 4.9 step 6, G3 HTTP and disconnect tests, 7 |
| R1 B6 - the `interval == 0` doc comment claimed a submit-time evaluation that does not exist | 4.1 (doc corrected, pair refused at boot) |
| R1 G-a / R2 3 - the walk was not on `spawn_blocking` | 2.3, 3.4, 4.5 doc, 4.6 step 4 |
| R1 G-b - per-order walks instead of per-symbol | 3.10 bound 3, 4.5 batched `Probe`/`Walk`, G4 second measurement |
| R1 G-c - a reconnect gap is unbounded | resolved by account ownership (3.4); the sweeper never stops, and the drain budget (3.10 bound 2) bounds any gap that does open |
| R1 G-d / R2 3 - `Option<u32>` cannot express a truncated drain, so the frontier advanced over unwalked tape | 4.3 `scanned_to_ns`, 4.5 `Walk::reached_ns`, G1 and G2 truncation tests |
| R1 G-e - duplicate sweepers per session | resolved by account ownership (3.4) |
| R2 2 - `ScanResult` could not perform its stale-frontier check | 4.3 `revision` + echoed `from_ns`, 3.4, G2 `a_scan_against_a_stale_revision_is_dropped` |
| R2 3 - `BoundedSeek` caps only `seek_to`, so the claimed drain bound did not exist | 2.3, 3.10 bound 2, 4.5 `SWEEP_DRAIN_BUDGET` |
| R2 5 - admission refusal could freeze the book forever | 3.5 (execution no longer conditional on delivery), 4.6 steps 5-6 (`emitted` not `results.len()`), G3 `a_refused_delivery_still_books_the_fill` |
| R2 7b - the gated accept's `AccountState` would eat `DropNextAccountUpdate` | 3.6, 4.4 gated-rest branch, G2 renamed test |
| R2 8 - `current_price` looks AHEAD of sim-now, so the marketable seed used a future trade | 2.3, 3.3, 4.5 `last_trade_at_or_before`, 4.7, G1 `last_trade_at_or_before_never_looks_ahead` |
| R2 9a - the strict-predicate test was stated backwards | G1 `counts_only_prints_strictly_through_the_limit` |
| R2 9b - `fetch_trades(sym, None, 1)` returns the tape head, not the current level | 2.3, 4.9 step 1 |
| R2 9c / R1 nit - a 30 s wait for a random walk is not a gate | 2.6, 4.9 step 4 (deterministic crossing, second fixture) |
| R2 10 - the live instrument landed after the feature it gates | 6 (L1 is the instrument, L2 is the feature with its proof and docs) |
| R1 S2 - an accelerated clock turns the sweep into a hot loop | 3.10 `MIN_SWEEP_WALL_MS`, 4.6 step 1 |
| R1 S3 - a `#[cfg(test)]` field on `AppState` to assert a negative | G3 `the_sweeper_is_not_spawned_on_a_default_venue` restated without it |
| R1 S4 - `PendingScan.price` vs `submit.price: Option<Decimal>` | 4.3 doc, 4.4 `let limit = ...expect(...)` |
| R1 S5 - an hour-long sweep interval passes silently | 4.1 `SLOW_SWEEP_WARN_MS` |
| R1 nit - old constructors neither deleted nor kept | 4.3 (deleted, call sites enumerated) |
| R1 nit - `notes/todo.md` removal landed in a different commit than the feature | 4.10, 6 (both in L2) |

Rejected:

- **R1 S1 - "the marketable seed counts a print from BEFORE acceptance".** The
  direction is inverted: `source::current_price` calls `seek_to(sim_now)`, which
  returns the first tick AT OR AFTER `sim_now`, and
  `current_price_matches_live_subscriber_at_same_instant` asserts exactly that
  (`first.ts_event >= sim_now`). The reading is a look-AHEAD, not a stale one.
  The underlying worry - that the seed is not a print inside `(from, now]` - is
  real and is fixed, but under R2 8's correct diagnosis, not this one.
- **R2 1's "reconciliation cannot discover the claimed fill because no task
  mutated venue truth"** as a distinct defect. It is a restatement of the same
  session-ownership bug, not an additional one: nothing was ever claimed and
  then lost; the fill simply never happened. Folded as one finding.
- **R2's framing that account-level event distribution is "forced into
  scope".** Delivering an account-owned batch to the account's currently bound
  sessions (3.5, 4.8) is a delivery registry, not an event-topology rebuild.
  Command-triggered events remain session-scoped, and 7 now says so explicitly
  rather than implying the whole asymmetry was closed.
</content>
</invoke>
