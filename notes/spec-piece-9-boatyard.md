# Implementation spec: piece 9, the boatyard

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. Spawned from `notes/todo.md`: the
fourteen-piece inventory item 9 under "Landing the grand design", and the design
bullets it points at under Open issues - THE RIVER AND THE BOAT (settled
2026-08-15), THE SYMBOL IS A REQUEST PARAMETER, SYMBOL RESOLUTION IS TOTAL, and
the durable-prose obligation.

REVISION 2, 2026-08-15. Revised after two independent reviews and the
orchestrator's adjudication of the design questions they raised (the review
reports and the adjudication document were consumed into this revision and
deleted per the loop's convention; section 10 preserves what they ruled). The
adjudication is BINDING on this document. Four of its five rulings changed the design rather
than the prose: one boat per river with a loud refusal on a differing sharing
key, runtime generator-level havoc REFUSED rather than re-derived, a boat placed
at the river's FIXED origin, and the ring resize decided rather than measured
into. Section 10 records every finding this revision rejected and why.

Notes-class document. Nothing durable may cite it; what must endure moves into
`reference/architecture.md`, `docs/config.md` and code comments WITH the code,
per the standing todo item.

## 0. What this spec is for, in one paragraph

Today one venue process places exactly ONE paced boat, at boot, on the river
named by the config's boot symbol. Every other configured river is servable for
history and refused for `/ws`. This spec builds the BOATYARD: a keyed registry
of live boats, placed on demand by the first subscriber whose request resolves
to a sharing key, joined by every later subscriber resolving to the same key,
and wound down when the last passenger leaves. It carries the three mechanics
the todo left open - idle-river retirement, pacing threads versus one scheduler,
per-boat rings versus one filtered ring - and it makes a concrete
recommendation on each, with grounds, in section 3, so a reviewer has something
to attack rather than a menu.

## 1. Survey of the ground

Read against the tree, not from memory. Paths are relative to the repository
root.

### 1.1 What exists and is shaped right

- `crates/mogwai-server/src/source.rs` - `Rivers`, a keyed registry of
  `River { checkpoints: Mutex<CheckpointIndex> }` created lazily on first use,
  with a stated lock ordering (registry, then release, then river; never both).
  `RiverKey(Symbol)` already exists and already carries a
  `#[expect(dead_code, reason = "piece 9 widens and inspects the river key")]`
  accessor. `TapeIdentity { seeds, regime }` is the per-run half of river
  identity. `concurrent_first_readers_share_one_river` pins the
  create-under-the-registry-mutex property this spec depends on.
- `crates/mogwai-server/src/tape.rs` - `Tape`, a `broadcast::Sender<TapeFrame>`
  plus a `last_quote` snapshot, driven by one OS thread that paces against a
  `SimClock` and publishes serialized frames. Already parameterized on
  `TapeSpawn { rivers, sim, speed, fanout_depth, zero_speed_stall_ms, fault_tx }`
  - every value a boat needs is already an argument rather than a global.
- `crates/mogwai-server/src/ws.rs` - `SocketQuery` with `deny_unknown_fields`
  and a doc comment stating outright that "later pieces add `speed` and
  `duration_ms` here"; `SocketSession`, which exists as a struct rather than a
  bare `Symbol` explicitly because "boat placement and the per-boat clock attach
  here". Piece 6's carrier is half-built and its extension points are named.
- `crates/mogwai-adapter/src/config.rs` already emits `/ws?symbol=<S>` when a
  symbol is configured, so the consumer side of the carrier exists.
- `Run::bind_lanes` / `release_lanes` with a monotonic lane id, so "a
  reconnecting client cannot retire the lanes of the connection that replaced
  it". The owner ruled passenger identity NOT open; this is the code that
  answers it, and the boatyard reuses the same shape rather than inventing a
  second one.

### 1.2 What is singular and must change

- `Run::tape: Arc<Tape>` - one tape, constructed inside `Run::new` from
  `instrument.symbol`. `Run::boot_symbol` is the only symbol that can own it.
- `http::resolve_socket_symbol` refuses a configured non-boot symbol with
  "configured but is not the river this run booted". That refusal exists solely
  because no second boat can be placed; it is deleted by this spec. Its OTHER
  branch - `unserved_symbol_refusal`, for a symbol absent from the profile map -
  SURVIVES this spec and is piece 13's to total; see section 8.
- `Run::sim: SimClock` - one clock per run, read by `AppState::sim()`, by the
  engine's order stamping through `sim_now_ns`, by `/clock`, by `/account`, by
  the writer's `GoDark`/`StallData` gates, by the exec pump's ACK holds, by the
  heartbeat period and by the sweeper.
- `sweeper::spawn_fill_sweeper` samples ONE `to_ns` per pass from `run.sim` and
  carries ONE `last_swept_ns` settlement watermark for every symbol. The engine
  underneath it exposes `pending_scans()` keyed by SYMBOL and carries no boat
  identity on an order - which is why section 3.0's one-boat-per-river ruling is
  load-bearing rather than conservative.
- `Tape::arm_flow_surge` / `clear_flow_surge` reach `Rivers` by
  `self.symbol` - a run-wide control-plane arm that can only ever name the boot
  river.
- `config.fanout_depth` (default `4_194_304`) sizes ONE eagerly allocated ring.
  Its doc comment derives the value as a per-boat quantity ("holds about 0.466
  wall seconds at the worst measured p99.9 rate"), and at that depth the eager
  allocation is already mispriced at N = 1. Section 3.3 resizes it.

### 1.3 Two live defects the survey found, which this landing must close

Neither is reachable today, because today there is exactly one boat that is
never wound down. Both become reachable the instant a second boat exists.

**D1. `activate_live` is one-way and has no inverse.**
`mogwai-data/src/generated/checkpoint.rs` sets `self.live = true` and nothing
ever clears it. `Rivers::activate_live` is called from `Tape::start`;
`extend_toward_unless_live` then refuses every reader extension for the life of
the process. A boat that winds down therefore leaves its river permanently
frozen at whatever frontier the dead worker reached: `/trades` and `/quotes`
for that symbol are capped there forever, and `reach_river` returns
`Ok(checkpoints)` rather than an error, so the refusal surfaces as a positioning
failure with no mention of the wound-down boat. This is the frontier family
inverted - a fence with no recovery that wedges the watermark forever.

**D2. Boat placement is a function of unrelated history reads.**
The paced worker consumes ticks from the river's SHARED lead
(`Rivers::next_live_tick` -> `CheckpointIndex::next_tick`). Any reader that
walked that river before the boat was placed - one `/trades` request, one
sweeper mark read, one price-less market order - has already advanced the lead.
A boat placed afterwards starts at the frontier, not at the placement origin,
and the span between is never delivered to anyone. The todo's design says "a
boat is placed at the river's origin"; with a shared cursor that sentence is not
implementable. Today the single boat is placed before the router binds, so
nothing can have read the river first, and the bug is unreachable.

### 1.4 Reconciliation against sibling scope

Pieces 6, 10, 11, 12 and 13 cover overlapping ground. Section 8 states exactly
which of their work this spec takes (because it is forced) and which it excludes
(because it is genuinely separable), with a paragraph for EACH of the five.
That section is the stopping rule and is the first thing a reviewer should read
after this one.

## 2. The target, as concrete artifacts

### 2.1 River identity

```rust
// mogwai-server/src/source.rs
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct RiverKey {
    symbol: Symbol,
    /// Digest of everything that determines the water: the resolved knob
    /// bundle (scalars, session profile, calendar, size grid), the tape seed
    /// derived for this symbol, and the boot regime. A `u64` and not the
    /// bundle itself so the key is cheap to hash and cheap to log.
    bundle: u64,
}
impl RiverKey {
    pub(crate) fn resolve(profile: &InstrumentProfile, identity: TapeIdentity) -> Self;
    pub(crate) fn symbol(&self) -> &str;
    pub(crate) fn bundle_digest(&self) -> u64;
}
```

The digest is computed with `std::hash::Hash` over a canonical, explicitly
enumerated field list - never over a `Debug` rendering, and never over a
`HashMap` iteration order. Today the symbol alone determines the bundle within
one process, so widening the key changes no behaviour; it is done NOW because
the todo says composition is in the key ("two agents asking for MNQ want
different rivers if one wants the Asia loop and the other wants post-lunch"),
and because a key widened later is a key every stored boat has to be re-hashed
against.

`RiverKey::resolve` takes an `&InstrumentProfile`, NOT a profile map and not a
symbol string. That signature is the piece-13 shaping obligation ruling 4
imposes: nothing in the boatyard may treat the configured profile map as the
universe of servable symbols. There is exactly ONE lookup that consults the map
- the caller that produces the `&InstrumentProfile` handed to `resolve` - and
piece 13 totals resolution by making that one lookup fall back to the default
shape. Every other boatyard surface is already total.

### 2.2 The boat and the sharing key

```rust
// mogwai-server/src/boatyard.rs  (NEW module)

/// River identity PLUS speed. Generator-level havoc chosen at placement is
/// already inside `RiverKey` through `TapeIdentity`'s regime.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct BoatKey {
    river: RiverKey,
    /// Speed quantized to micro-multiples. A raw `f64` is not `Hash` or `Eq`,
    /// and two clients writing `100` and `100.0000001` must share a boat.
    /// Quantization is stated on the wire in `docs/config.md`.
    speed_micros: u64,
}

pub(crate) struct Boat {
    key: BoatKey,
    /// This boat's clock. `sim_epoch_ns` is the RIVER'S FIXED ORIGIN (the
    /// warmup boundary), never sim-now-at-placement; `wall_anchor_ns` is the
    /// instant this boat was placed, which is operational metadata and not
    /// identity; `speed` is the key's speed. A joiner adopts the clock
    /// unchanged - that is what "join where the boat is" means, and it means
    /// boarding MID-STREAM. See section 3.5.
    sim: SimClock,
    tape: Arc<Tape>,
    /// Sim instant of the last tick this boat published, for `/clock`.
    published_ns: AtomicU64,
    /// Settlement watermark, per boat because it is denominated in this boat's
    /// clock. See section 8 on why piece 11's other half is excluded.
    last_swept_ns: AtomicU64,
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
    cancel: Arc<AtomicBool>,
}

pub(crate) struct Boatyard {
    rivers: Arc<Rivers>,
    /// Keyed by RIVER, not by boat: at most one boat sits on a river at a
    /// time (section 3.0). The passenger count lives here, under this mutex,
    /// rather than inside `Boat`, so registration and wind-down are one
    /// decision.
    boats: Mutex<HashMap<RiverKey, Seat>>,
    fanout_depth: usize,
    zero_speed_stall_ms: u64,
    fault_tx: mpsc::Sender<TickFault>,
}

/// The sitting boat and its live passenger count.
struct Seat {
    boat: Arc<Boat>,
    passengers: u32,
}

/// A boarding pass. OWNED by the connection task - never wrapped in an `Arc`,
/// never cloned. Its drop is what deregisters, so a socket task that is
/// dropped or aborted cannot leak a passenger.
pub(crate) struct Ticket {
    yard: Arc<Boatyard>,
    boat: Arc<Boat>,
}

pub(crate) enum BoardRefusal {
    /// A boat is already sitting on this river at a different speed. Names the
    /// sitting speed. Rendered as a `400` before the 101.
    SpeedInUse { sitting_speed: f64 },
    /// Placement failed (a reach error, a fault). Carries the cause.
    Placement(anyhow::Error),
}

impl Boatyard {
    /// Place, join, or refuse. Blocking work (the placement reach) runs on
    /// `spawn_blocking` OUTSIDE the registry mutex; see section 4, brick 4.
    pub(crate) async fn board(&self, req: &BoardRequest) -> Result<Ticket, BoardRefusal>;
}

impl Drop for Ticket {
    /// Decrements under the boatyard mutex. If it was the last passenger, the
    /// seat is REMOVED from the map and the cancel flag set while the mutex is
    /// held; the worker `JoinHandle` is then joined AFTER the mutex is
    /// released, on a detached blocking task. See section 4, brick 4, for why
    /// the join cannot happen under the lock or on a runtime worker.
    fn drop(&mut self);
}
```

`duration_ms` is deliberately NOT in `BoatKey`. A duration is a property of a
PASSENGER, not of the water: two agents wanting the same river at the same speed
for different spans must share one boat, and each one's socket announces
`RunComplete` and closes at its own deadline. The boat winds down when the last
passenger leaves, whatever their reasons for leaving.

`SocketSession` holds the `Ticket` BY VALUE. There is no `Arc<Ticket>` anywhere
in the tree after this landing; a clippy-visible `Arc` around it would defeat
the ownership property the whole wind-down argument rests on.

### 2.3 What replaces the shared live cursor

`CheckpointIndex::activate_live`, `Rivers::activate_live`,
`Rivers::next_live_tick` and `extend_toward_unless_live` are DELETED, together
with the `live` flag itself and the third guard site, the `if !self.live` branch
inside `CheckpointIndex::try_source_before_target`. In their place:

```rust
// mogwai-server/src/source.rs
impl Rivers {
    /// A boat's own positioned cursor over this river, at the river's fixed
    /// `origin_ns`. Extends the shared chain to `origin_ns` first, so every
    /// boat and every reader are positioned from the SAME pinned snapshots.
    ///
    /// TIE RULE, and it is the frontier family's cursor form: the returned
    /// cursor yields EVERY row whose `ts_event` is at or after `origin_ns`,
    /// including all rows exactly AT `origin_ns`. A cursor positioned by
    /// timestamp alone may never be advanced onto an instant whose rows have
    /// not all been yielded, and burst quotes and trades share instants
    /// routinely.
    pub(crate) fn place_cursor(
        &self,
        key: &RiverKey,
        origin_ns: u64,
    ) -> anyhow::Result<Box<dyn TickSource>>;
}
```

This is the fix for D2 and the enabling change for everything else: a boat's
path becomes a pure function of `(river identity, placement origin)`, and since
the origin is the river's FIXED origin (ruling 3), a pure function of the key
alone. No reader can move it, and no wall instant enters it.

There is no `ControlGeneration` and no re-derivation. Revision 1 of this spec
proposed that a boat compare a control generation at every tick boundary and
re-derive its cursor across a runtime `FlowSurge`; both reviews showed the
mechanism cannot work (the arm pins a boundary at the LEAD's position, which is
generally ahead of the boat, so the boat re-derives from a pre-arm unsurged
snapshot and the re-derivation is a literal no-op), and ruling 2 removes the
need for it entirely by REFUSING the runtime arm. See section 3.4.

### 2.4 The wire

```rust
// mogwai-server/src/ws.rs
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SocketQuery {
    #[serde(default)] symbol: Option<String>,
    /// Absent means the venue's configured `speed`. Validated by the same
    /// `validate_speed` rule the config uses: finite and non-negative.
    /// A speed that differs from the speed of the boat already sitting on the
    /// requested river is REFUSED, naming that speed - it does not place a
    /// second boat. Section 3.0.
    #[serde(default)] speed: Option<f64>,
    /// Absent means indefinite. Sim milliseconds, measured from this
    /// passenger's boarding instant on the BOAT's clock, never from boot.
    #[serde(default)] duration_ms: Option<u64>,
}
```

`deny_unknown_fields` stays, and adding these two keys is exactly what its doc
comment reserved. A `speed` or `duration_ms` that fails validation, and a
`speed` that collides with a sitting boat, is a `400` before the 101, in the
same shape as the symbol refusal, for the reason already recorded there: a
refusal is a status, not a close code.

`SocketSession` gains the ticket, by value:

```rust
pub(crate) struct SocketSession {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) ticket: Ticket,
}
```

Everything downstream that reads `state.sim()` for a per-connection decision
reads `session.ticket.boat().sim` instead. Section 8 enumerates the sites and
which ones are excluded.

## 3. The open mechanics: rulings and grounds

The todo leaves three of these open and says so; the adjudication settled two
more that the reviews forced. Each is decided here with its grounds and its
falsifier, so a reviewer can attack the reasoning rather than guess it.

### 3.0 ONE BOAT PER RIVER, with a loud refusal on a colliding key

**Ruling** (adjudication ruling 1, binding). At most one boat sits on a river at
a time. The sharing key KEEPS speed and placement-time generator havoc as
members, and that membership is what makes the refusal precise: a subscriber
whose resolved `BoatKey` equals the sitting boat's key BOARDS it; a subscriber
naming the same river with a different speed is REFUSED with a `400` naming the
sitting boat's speed. It is never given a second boat on the same water.

**Grounds.**
1. The engine's ledger, its resting orders and its settlement are per SYMBOL by
   settled design, and `pending_scans()` carries no boat identity. Two boats on
   one river with two incomparable clocks means each sweep pass sees the other's
   resting orders: duplicate settlement, or a slow passenger's order filled
   against a fast passenger's future water. Revision checks can suppress a
   duplicate APPLICATION but cannot decide which clock should have won. Both
   reviews found this independently.
2. Symbol therefore uniquely names the boat again, which collapses the `/clock`
   and control-plane ambiguity: `/clock?symbol=` has one boat to answer for, and
   a river-scoped arm has one clock to derive an instant from.
3. Forward testers overwhelmingly share transport-havoc boats at one standing
   accelerated speed. Distinct-speed cohabitation on one river is the exotic
   case; refusing it loudly is honest, and widening later stays open because the
   key already carries the speed.

**Falsifier / reversal path.** If distinct-speed cohabitation is genuinely
wanted, the boatyard map re-keys from `RiverKey` to `BoatKey` (a one-line change
this shape preserves) AND the engine grows per-boat temporal ownership of orders
and marks. The second half is the real cost and is why the narrowing landed.
The todo's river-and-boat bullet is amended in brick 7 to record this as the
landed narrowing, explicitly reversible.

### 3.1 Idle-river retirement: NOTHING retires a river beyond the empty-boat rule

**Ruling.** A `Boat` is dropped when its last passenger leaves. The `River`
underneath it - the `CheckpointIndex` and its retained snapshots - survives for
the life of the process. There is no LRU, no idle timer and no eviction.

**Grounds.**
1. A river is not idle in the sense an eviction policy assumes. It is the ONLY
   thing that makes history cheap: `/trades`, `/quotes`, the sweeper's mark and
   settlement reads, and every price-less market order position themselves from
   its snapshots. Retiring the chain converts a bounded memory cost into an
   unbounded, synchronous, lock-held walk from the tape origin on the next
   reader - which the source's own `ensure_reach` comment already calls out as
   the first-touch cost. Eviction moves that cost from boot, where an operator
   waits for it knowingly, onto an arbitrary later request.
2. The retained set is already bounded per river by `MAX_CHECKPOINTS`, so the
   growth an eviction policy would fight is in the NUMBER OF RIVERS, and that
   number is bounded by the distinct `RiverKey`s one fire-and-forget instance
   sees. Strategies are single-instrument by settled premise and a venue serves
   one forward-test run, so the expected count is one or two.
3. Resource cost shapes no decision here - the standing owner ruling recorded in
   the todo's PROBLEM STATEMENTS block. A cache eviction policy is a pure
   resource-cost mechanism, and it would be the first one in the tree.

**Falsifier, stated so this can be reopened honestly.** If a single venue
instance is ever observed carrying more than a handful of distinct river keys -
which would mean the single-instrument premise has broken, or that request-
carried composition is being swept over - the fix is an LRU over RETAINED
SNAPSHOTS within a river (coarsening the grid, which `CheckpointIndex` already
does for long runs), never over the river's existence. A river that can vanish
is a river whose history answers depend on when you asked.

### 3.2 Pacing: ONE OS THREAD PER BOAT, not one multiplexing scheduler

**Ruling.** Keep the current shape: placement spawns one `std::thread` that
owns that boat's cursor, paces it and publishes. No shared scheduler, no tokio
task, no timer wheel.

**Grounds.**
1. The per-tick work is a BLOCKING, path-dependent generator step, plus a
   `serde_json::to_string`. On a shared scheduler every boat's pacing jitter
   becomes the sum of the other boats' generator costs at that instant. Pacing
   fidelity is the thing this venue exists to model; a topology that couples it
   across unrelated rivers trades away the product to save threads.
2. A `speed == 0.0` boat has no wall deadline at all - it runs the generator
   flat out and throttles only on ring headroom (`await_headroom`). On a shared
   scheduler one firehose boat monopolizes the run queue and starves every paced
   boat. Special-casing it back out is rebuilding per-boat threads with extra
   steps.
3. The thread is almost entirely ASLEEP: `sleep_until_wall_cancellable` parks in
   20 ms slices against a wall deadline. The marginal cost of a boat is one
   stack and a periodic wakeup, at an expected boat count of one or two.
4. Three existing invariants are expressed as "the thread that owns this river's
   lead": the cancel-flag teardown, the `await_headroom` park, and (until this
   spec deletes it) the live-frontier ownership. A scheduler re-derives all
   three, and the second one - a park that blocks the producer on subscriber
   drain - has no coherent meaning when the producer is shared.
5. Putting the pacer on a tokio task instead is worse than either: a blocking
   generator step on a runtime worker starves the sockets, which is the exact
   reason `market_reading` and the sweeper already push their walks onto
   `spawn_blocking`.

**Falsifier.** If a venue is ever asked to carry tens of boats, the answer is a
bounded thread POOL with one boat pinned per worker and a refusal when the pool
is full - a placement refusal an operator can see, not silent jitter. It is not
a multiplexing scheduler.

### 3.3 Fanout: ONE RING PER BOAT, at a DECIDED smaller depth

**Ruling** (adjudication ruling 5, binding). Each boat owns its own
`broadcast::Sender<TapeFrame>`, and `fanout_depth` becomes a PER-BOAT depth. The
shipped default of `4_194_304` DROPS, as a decided change in brick 1, to the
smallest power of two that still holds the worst measured p99.9 wall second of
frame work - the knob's own stated derivation rule, applied honestly rather than
inherited. Brick 1 then VERIFIES the chosen size's resident cost; it is not a
decision gate.

**Grounds for the topology.**
1. A shared filtered ring destroys the exact property the ring exists to
   provide. `fanout_depth`'s own doc comment derives it as a span of WALL TIME
   at a measured frame rate. In a shared ring that span becomes a function of
   the AGGREGATE rate across every boat, so a subscriber on a quiet river is
   evicted by traffic on a river it never asked for.
2. Eviction here is not a slowdown, it is a KILL. `ws.rs` treats
   `RecvError::Lagged` as `CLOSE_VENUE_FAULT` and terminates the connection,
   deliberately. A shared ring makes one busy boat able to kill unrelated
   passengers' forward-test runs, and the killed run looks like a venue fault
   rather than like contention. That is a liveness coupling across strangers -
   the same family as the process-wide settlement watermark the todo already
   flags as a defect to remove.
3. The filtering itself is not free either: every subscriber wakes on every
   frame of every boat and discards the ones that are not its own, so the
   per-connection wakeup rate is the aggregate rate, at exactly the moment the
   venue is busiest.

**Grounds for the resize being decided rather than measured into.** Revision 1
presented the depth as an open question with a 64 MiB proceed threshold. R1's
arithmetic closed it: a tokio broadcast ring is EAGERLY allocated at roughly
40-64 bytes per slot once the per-slot lock and counters are counted, so
4,194,304 slots is on the order of 170-270 MB. No sane per-boat budget is met at
that depth, the "retain the default" branch was arithmetically dead, and
presenting a decided question as open would have hidden a real timing
perturbation inside the brick that claimed nothing could break. This is a
SIZING defect that already exists at N = 1; the answer was always resize, never
share.

**Consequence, named rather than discovered.** The resize perturbs boot timing,
and `a_banded_limit_fills_from_the_run_sweep` is known fragile to exactly that
(section 7). Brick 1 is therefore no longer a no-risk brick and its gate says so.

### 3.4 Runtime generator-level havoc: REFUSED on a boated river

**Ruling** (adjudication ruling 2, binding). `POST /control/divergence` gains an
optional `symbol` selector. Divergences split by class:

- GENERATOR-LEVEL (`FlowSurge`, and `VolStorm` / `LiquidityDrought` when they
  land) armed against a river that has a SEATED BOAT are REFUSED with a `400`
  naming the forking alternative: place a boat whose key carries the havoc. On a
  river with no seated boat the arm is accepted, mutates the lead and pins its
  boundary as today, and takes effect for the next placement and for history
  alike - coherent because no passenger straddles it. With no `symbol` given,
  the arm is refused with a `400` naming the rivers that have boats, rather than
  fanning out to all of them.
- TRANSPORT-CLASS (`GoDark`, `StallData`, `DelayAcks`, `CommandLatency`, and the
  drop/dup/reorder/latency family) stay run-wide, per-socket and
  runtime-armable, unchanged. They are the passenger's eyesight, not the water.

**Grounds.** The design already says VolStorm, FlowSurge and LiquidityDrought
FORK THE RIVER. That is a statement about river IDENTITY, so the sound carrier
for generator-level havoc is the sharing key at PLACEMENT time, not a runtime
mutation of shared water. Revision 1 tried to keep the runtime arm and reconcile
it with per-boat cursors through a control generation and a re-derivation; both
reviews demolished it on the same mechanism. `CheckpointIndex::arm_flow_surge`
mutates the shared LEAD and pins a boundary snapshot at the LEAD's current
position. A boat generally sits BEHIND the lead, because history readers push
the lead to sim-now. Re-deriving "at the boat's current position" therefore
selects a snapshot strictly before the arm - pre-arm and unsurged - so the
re-derivation is a no-op and the divergence it was invented to close stays open.
The arm also cannot retroactively rewrite snapshots history has already
materialized past it. No generation counter repairs an already-materialized
interval.

**What this deletes.** Brick 6's re-derivation mechanism, `ControlGeneration`,
the per-tick generation comparison, and with them R1's re-seek tie hazard - a
re-seek that does not exist cannot replay or skip an instant. The tie rule
survives as a constraint on `place_cursor`'s ONE positioning (section 2.3),
which is where it belongs.

**Documented consequence**, and it goes in `docs/havoc.md` with the code: an
operator who wants generator-level havoc mid-run must place a boat carrying it,
which is a different river by identity. A generator arm accepted on an unboated
river is visible to every later passenger of that river. This does not violate
"passengers cannot see each other", because the arming party is the operator and
not a passenger, and because nothing an ORDER does reaches the water. The
exogeneity contract is untouched.

**Falsifier.** If a scenario genuinely needs mid-run generator havoc on shared
water, that is a future design with its own spec - and its sound shape is
immutable control history replayed from a boundary at or before every live
cursor, not a mutation of the lead. Nothing in the current consumer inventory
needs it.

### 3.5 Placement origin: the RIVER'S FIXED ORIGIN, and joiners board mid-stream

**Ruling** (adjudication ruling 3, binding). A boat is placed at the river's
fixed origin - the warmup boundary - always, never at sim-now-at-placement. The
wall instant of placement is recorded as operational metadata (`wall_anchor_ns`)
and is NOT part of identity. A later subscriber joins the boat WHERE IT IS.

**Grounds.** Revision 1 was self-contradictory here, as R1 found: it called
`sim_epoch_ns` the "placement origin" while section 8 argued to piece 12 that a
boat's path does not depend on when it was placed. Only a fixed origin makes
that second sentence true, so the fixed origin is what this spec takes. The
consequence - a boat placed an hour into a run is an hour of sim behind, and at
speed 1 never catches up - is accepted, not worked around: mid-stream boarding
is the design's explicit contract, and it is sane because the composition is
homogeneous by the operator's own choice. A boat's path is then a pure function
of its key and the run seed, and the piece-12 argument in section 8 stands as
written.

**Falsifier.** If mid-stream staleness is ever observed to matter, the repair is
a request-carried origin in the SHARING KEY (a joiner naming a different origin
gets a different boat), which is the composition track, not a wall-anchored
epoch. An origin that depends on the wall clock makes two boats with equal keys
carry different water, which breaks the key's whole meaning.

## 4. The bricks

Each brick is one coherent landing that leaves the suite green, ordered so the
keep/revert verdict on each is readable on its own gate.

### Brick 1 - resize the ring, and verify the size

Drop `fanout_depth`'s default from `4_194_304` to the depth re-derived by the
knob's own rule (section 3.3), updating the doc comment's derivation in the same
edit so the number and its justification stay together.

Then VERIFY the chosen size's resident cost. `--alloc` measures per-function
allocation bytes and is NOT a measure of process RSS - R2 is right that the two
are different measurements - so the verification is a `ring_resident_bytes=<N>`
sidecar counter read from `/proc` around ring construction, and `--alloc` is a
secondary cross-check on the allocation itself. If no surface reports it, build
one: a `mogwai-server` example registered as a `[mogwai.targets.*]` harness in
`brokkr.toml`.

Gate:

    brokkr check
    brokkr mogwai ring_sizing --alloc 3
    brokkr results --command ring_sizing --mode alloc

Record the reading in `reference/performance.md` with the host and date, per the
annotation discipline.

THIS BRICK IS NOT RISK-FREE, contrary to revision 1's table. The depth change
perturbs boot timing, and `a_banded_limit_fills_from_the_run_sweep` bets a fixed
price headroom against sim-time drift (section 7). If it fails here, take the
todo's honest fix - assert the fill's liquidity side - inside this brick rather
than arguing about a red gate. The depth moves no generated byte, so nothing is
re-blessed.

### Brick 2 - widen `RiverKey` to `(symbol, bundle digest)`

Pure refactor. `Rivers::river` takes a `&RiverKey` resolved by the caller from
an `&InstrumentProfile`; the `#[expect(dead_code)]` on `RiverKey::symbol` is
removed. No behaviour changes: within one process the symbol determines the
bundle, so every key that was equal stays equal.

The single map lookup that produces the `&InstrumentProfile` is isolated behind
one named function in this brick, and its doc comment states that piece 13
totals resolution HERE and nowhere else (section 2.1). That isolation is the
whole piece-13 shaping obligation and it costs nothing to take now.

Gate: `brokkr check`. This is a change that claims to move nothing, so it is
proven cheaply by the existing exact-equality gates - the fill golden and
`two_runs_with_the_same_configured_seed_serve_the_same_first_trades` must both
pass unmodified.

    brokkr check
    brokkr test -p mogwai-cli two_runs_with_the_same_configured_seed

### Brick 3 - give a boat its own cursor; delete the live flag (closes D1 and D2)

Add `Rivers::place_cursor`, with the tie rule of section 2.3. Delete
`CheckpointIndex::activate_live`, its `live` field, `extend_toward_unless_live`,
`Rivers::activate_live`, `Rivers::next_live_tick`, AND the `if !self.live` guard
inside `CheckpointIndex::try_source_before_target` - three deletion sites, not
two. `Tape::start` takes a positioned cursor instead of a symbol and drives it
directly.

Deletions this forces, named because a spec that leaves them to be discovered is
short a brick - all exist only to police the shared live cursor:

- `source.rs::the_live_river_is_not_extended_by_a_reader`
- `source.rs::activation_racing_a_cold_reach_never_moves_the_live_frontier`
- the `extend_toward_unless_live` unit tests in
  `mogwai-data/src/generated/checkpoint.rs`

They are replaced, not merely dropped, by the three tests below. That
replacement is the brick's load.

New tests (they are the gate, and all three must be BITE-CHECKED by reverting
the production change as a text edit):

- `a_boat_placed_after_a_history_read_still_starts_at_the_river_origin`
  (mogwai-server, in-crate): read `/trades`-shaped history far past the origin
  through `Rivers`, THEN place a cursor, and assert the first tick is the first
  tick at or after the river's fixed origin. This is D2's regression test;
  against the current shared-lead code it fails with a first tick at the
  reader's frontier.
- `a_wound_down_boat_leaves_its_river_extendable` (mogwai-server, in-crate):
  place a cursor, drop it, then `ensure_reach` well past the frontier and assert
  it succeeds. This is D1's regression test; against the current code the reach
  silently returns the unchanged checkpoint count.
- `a_placed_cursor_yields_every_row_at_the_origin_instant` (mogwai-data or
  mogwai-server, wherever the tie is decidable): construct or find an origin
  instant carrying more than one row and assert every one of them is yielded.
  This is the frontier family's cursor form and it is cheap to pin now, when
  there is exactly one positioning to get right.

    brokkr check
    brokkr test -p mogwai-server a_boat_placed_after_a_history_read
    brokkr test -p mogwai-server a_wound_down_boat_leaves_its_river
    brokkr test -p mogwai-server a_placed_cursor_yields_every_row

This brick MOVES THE FIRST DELIVERED TICK of the boot tape: today the boat
starts wherever the warmup walk left the shared lead (an overshoot of up to
`CHECKPOINT_K` ticks past `run_start_ns`); afterwards it starts at exactly the
first tick at or after the river origin. That is a change to the tape ORIGIN and
therefore owes an unconditional `TAPE_PROTOCOL_VERSION` bump.

READ THE CONSTANT, DO NOT TRUST THE PROSE. `mogwai_data::TAPE_PROTOCOL_VERSION`
is **17** as of 2026-08-15, while `AGENTS.md` still narrates 14 as the latest
bump and reserves 15 for the protocol-12b mechanism landing. That is the
tape-version-prose defect the todo has now recorded three times, caught a fourth
time by this spec's survey and confirmed by review R1. So: this landing takes
**18**, `AGENTS.md`'s reservation prose is corrected to name 19 in the same
commit, and the durable statements naming the constant are found by GREP across
`docs/`, `reference/` and `notes/` - not from a list - because the reservation
prose in `notes/protocol-12a-measurement-spec.md` is exactly the kind of
out-of-folder reader a hardcoded list has already missed. The fill golden and
every transcript that pins first-tick identity are RE-BLESSED knowingly here,
never widened into a tolerance.

### Brick 4 - the boatyard registry and the ticket

New `mogwai-server/src/boatyard.rs` with the types in section 2.2.

The concurrency rule, which is the whole brick: **registration and wind-down are
decided under the same mutex, and the passenger count is incremented before that
mutex is released.** A joiner therefore either finds a seat it has already
joined, or finds none and places one, or finds one at a different speed and is
REFUSED. There is no `Weak` upgrade, no resurrect path and no observable window
in which a seat exists with zero passengers. This is the guard-scope family
stated positively: the ticket is OWNED by the connection task, by value, so a
socket task that is dropped or aborted deregisters, rather than being
alive-while-the-work-runs.

Blocking work stays outside the mutex. `board` resolves the key and takes the
registry lock; on a MISS it inserts a placeholder the joiners await, releases
the lock, runs the placement reach on `spawn_blocking`, then re-takes the lock
to publish or to remove the placeholder on failure. A placement failure must
never leave a placeholder that later joiners wait on forever.

WIND-DOWN, and this is where revision 1 was wrong twice. The drop path, under
the boatyard mutex, decrements, and on reaching zero removes the seat and sets
the cancel flag. It then RELEASES the mutex and hands the `JoinHandle` to a
detached `spawn_blocking` that joins the worker. Two reasons, both from R1:
joining under the mutex serializes every concurrent `board()` behind a thread
teardown, and joining on a tokio worker blocks the runtime inside a destructor.
It also matters for the lock order: the worker thread takes the RIVER lock, so
waiting for it while holding the REGISTRY lock introduces a registry-then-river
wait edge that the source's stated ordering does not sanction. Joining off the
mutex keeps the stated order intact.

The join is NOT bounded by one `TAPE_SLEEP_POLL` slice, and revision 1's claim
that it was is withdrawn (R2). The poll bounds the PACING SLEEP only; a worker
may additionally be inside a generator step, a serialization, or waiting on the
river mutex, so the join is bounded by one poll slice PLUS one tick's work plus
lock acquisition. That is why it is detached rather than awaited on the
departing connection's path.

Cancellation is still not "gone": a cancelled-but-running worker still holds the
river's cursor and may still publish, which is exactly why the seat is removed
from the map BEFORE the join, so no joiner can ever reach a boat that is
winding down.

Dead code between bricks 4 and 5: the module lands before `/ws` can ask for a
second boat, so the public surface carries
`#[expect(dead_code, reason = "brick 5 wires the boatyard to the /ws carrier")]`
on exactly the items its own tests do not exercise, removed in brick 5. Naming
the reason is the tree's existing convention (`RiverKey::symbol` carries the same
shape today).

Tests:

- `two_requests_with_one_sharing_key_share_one_boat`
- `a_second_speed_on_a_boated_river_is_refused_naming_the_sitting_speed`
  (this is the ruling-1 narrowing's regression test; it replaces revision 1's
  `two_speeds_on_one_river_place_two_boats`, which asserted the opposite)
- `the_last_passenger_leaving_winds_the_boat_down_and_joins_its_worker`
- `a_joiner_racing_the_last_departure_never_boards_a_wound_down_boat`
  (loop, in the shape of `activation_racing_a_cold_reach`, which is the tree's
  existing precedent for a race test that a sequential test cannot catch)
- `a_failed_placement_leaves_no_placeholder_for_the_next_joiner`

    brokkr check
    brokkr test -p mogwai-server boat -N 16

### Brick 5 - the `/ws` carrier, per-boat clocks, and the refusal deletion

`SocketQuery` gains `speed` and `duration_ms`. `resolve_socket_symbol` loses its
configured-but-not-the-boot-river branch entirely - the refusal it emits is now
false, and leaving a stale refusal in place is worse than the feature being
absent. Its `unserved_symbol_refusal` branch STAYS: an unconfigured symbol still
gets a `400` after this landing, by ruling 4, and totaling that is piece 13's.
`SocketSession` carries the ticket by value. Every per-connection read of
`state.sim()` moves to `session.ticket.boat().sim`; section 8 lists them.
`/clock` gains the same `?symbol=` carrier and answers for that river's boat -
unambiguously, since a river carries at most one - falling back to the venue
clock when none is placed, which the response labels as such so a caller cannot
mistake a venue clock for a boat's.

The sweeper is restructured to iterate over BOATS: one `to_ns` sampled per boat
from that boat's clock, one `last_swept_ns` per boat. `frontier_after` is
unchanged and stays the guard it is - it just guards a per-boat watermark now.
Because a symbol carries at most one boat, each pending scan is swept by exactly
one clock, which is the property ruling 1 exists to preserve.

Tests:

- `a_ws_upgrade_for_a_configured_non_boot_symbol_is_served` - this INVERTS
  `a_configured_but_unbooted_socket_symbol_names_the_bound_river` in
  `mogwai-server/src/http.rs` (revision 1 named this test wrongly; R1 caught it),
  and the `mogwai-cli/tests/serving.rs` assertion on the string "configured but
  is not the river this run booted" is deleted with it.
- `an_unconfigured_symbol_is_still_refused_by_the_unserved_branch` - pins that
  this landing did NOT accidentally total resolution, so piece 13's widening has
  a test to invert.
- `two_boats_on_different_rivers_stamp_their_own_orders` (different rivers, not
  different speeds on one - ruling 1)
- `a_passenger_duration_closes_one_socket_and_leaves_the_boat_running`
- `clock_answers_per_boat_when_a_symbol_is_named`

    brokkr check --gate
    brokkr test -p mogwai-cli two_boats_on_different_rivers
    brokkr run mogwai -- serve
    python3 scripts/smoke.py

### Brick 6 - symbol-scoped havoc, with the generator class refused on a boated river

`POST /control/divergence` gains the optional `symbol` selector and the
generator/transport class split of section 3.4. No re-derivation, no control
generation, no per-tick comparison - ruling 2 deleted that mechanism, and with it
the highest-risk item in revision 1.

Tests:

- `a_generator_arm_on_a_boated_river_is_refused_naming_the_forking_alternative`
- `a_generator_arm_on_an_unboated_river_is_accepted_and_reaches_the_next_boat`:
  arm, then place, then assert the surge is visible on the socket AND in
  `/trades` over the same sim span. This is the coherence assertion revision 1
  could not honour; it holds here because the cursor is derived AFTER the pinned
  boundary rather than before it.
- `a_generator_arm_with_no_symbol_is_refused_naming_the_boated_rivers`
- `a_transport_divergence_still_reaches_every_connection`: the existing
  `an_armed_divergence_reaches_every_connection` must still pass, proving the
  run-wide/river-scoped split did not capture the transport arms. Note this test
  is a known one-time flake (todo, piece-7 gate); a failure here is re-run
  focused before it is believed.

    brokkr check --gate
    brokkr test -p mogwai-cli a_generator_arm_on_a_boated_river -N 5

### Brick 7 - the durable prose, with the code

Not a documentation pass afterwards - the standing todo item says so explicitly.
Written in this landing:

- `reference/architecture.md`: the boatyard, the river/boat/passenger
  distinction, the sharing key and its components, why duration is not in the
  key, ONE BOAT PER RIVER and the loud speed refusal, the fixed placement origin
  and mid-stream boarding, one clock per boat, the exogeneity that makes sharing
  sound, and the no-queue-competition contract that follows from it (fifty
  agents submitting the same buy all get the same fill and move nothing). Also
  the new tape version number, which the todo records as having been missed
  across two prior bumps in this very file, and which brick 3 found stale for a
  THIRD time (`AGENTS.md` narrating 14 against a live constant of 17).
- `notes/todo.md`: the river-and-boat bullet is amended to record the
  one-boat-per-river narrowing as landed and explicitly reversible, with
  section 3.0's reversal path.
- `docs/config.md`: `?speed=` and `?duration_ms=`, the speed quantization rule,
  the speed-collision refusal, and `fanout_depth` restated as per-boat at its
  new default.
- `docs/havoc.md`: the generator-versus-transport split, the `symbol` selector,
  the refusal on a boated river and the forking alternative it names, and the
  "an operator's generator arm on an unboated river is visible to every later
  passenger of that river" consequence.

Gate: `brokkr check` (gremlins), plus a reading of section 3's rulings against
what the prose claims. Per the todo's tape-version-prose item, grep the live
constant across every folder including `notes/` before committing - a hardcoded
document list has missed it three times.

## 5. Verification summary

Per gate, the exact command, matched to what the change can break.

| Brick | What can break | Command |
|---|---|---|
| 1 | boot timing, and the banded-limit test through it | `brokkr check` then `brokkr mogwai ring_sizing --alloc 3` |
| 2 | tape identity, if the key digest is wrong | `brokkr check` then `brokkr test -p mogwai-cli two_runs_with_the_same_configured_seed` |
| 3 | tape origin, history reach, wind-down, instant ties | `brokkr check`, `brokkr test -p mogwai-server a_boat_placed_after_a_history_read`, `brokkr test -p mogwai-server a_wound_down_boat_leaves_its_river`, `brokkr test -p mogwai-server a_placed_cursor_yields_every_row` |
| 4 | placement/join concurrency, refusal on speed collision | `brokkr test -p mogwai-server boat -N 16` |
| 5 | serving, execution stamping, sweep ownership, adapter sockets | `brokkr check --gate`, `brokkr run mogwai -- serve` plus `python3 scripts/smoke.py` |
| 6 | havoc class split and its refusals | `brokkr check --gate`, `brokkr test -p mogwai-cli a_generator_arm_on_a_boated_river -N 5` |
| 7 | gremlins, stale constants | `brokkr check` |

`--gate` and not plain `check` from brick 5 onward: this touches `mogwai-adapter`
through the `/ws` carrier, and plain `check` cannot see the four socket-backed
adapter test binaries. Two regressions have shipped red through that gap.

Every new regression test above is BITE-CHECKED: revert the production fix as a
TEXT EDIT, observe the named failure, restore it as a text edit. Never
`git checkout -- <path>`; it has destroyed uncommitted work in this tree twice.

## 6. Keep/revert

Each brick is one intrusive landing, kept or reverted on its own gate. The
ordering is chosen so the suite is green at every boundary:

1 (the decided resize) -> 2 (key widening, provably inert) -> 3 (cursor
ownership, the version bump and the re-bless) -> 4 (registry, still only ever
one boat because nothing requests a second) -> 5 (the carrier, which is the
first commit where a second boat can exist) -> 6 (havoc scoping) -> 7 (prose).

The awkward boundary is 4-to-5 and it is deliberate: brick 4 lands a registry
that is exercised only by its own tests, because `/ws` cannot yet ask for a
second boat. That is the price of keeping the concurrency change separable from
the wire change, and it is worth paying - a failure after brick 5 is otherwise
ambiguous between the registry and the carrier.

Brick 3 is the one that cannot be reverted cheaply once brick 5 has landed on
top of it, because the version bump and the golden re-bless go with it. Judge it
on its own gate before proceeding.

## 7. Known-fragile tests this landing will disturb

Named up front so a failure is diagnosed rather than chased. All three are
recorded in `notes/todo.md`.

- `a_banded_limit_fills_from_the_run_sweep` bets a fixed 2.01 of price headroom
  against sim-time drift and MISREPORTS its failure as an ordering violation.
  Brick 1's decided depth change and brick 3's first-tick change both perturb
  boot timing, and at speed 100 a small wall shift is a large sim shift. If it
  fails, the diagnosis is the premise, not the venue: the todo names the honest
  fix (assert the fill's liquidity side - a swept fill is `Maker`, an immediate
  one `Taker`). Taking that fix inside brick 1 or brick 3 is permitted and
  preferable to arguing about a red gate.
- `scripts/smoke.py` is flaky on the first market frame - "the first market
  frame must be the BBO snapshot, got Trade", twice in one day. Brick 5's gate
  runs it; a single failure with an immediate clean rerun is the known race, not
  this landing.
- `an_armed_divergence_reaches_every_connection` flaked once under load at the
  piece-7 gate. Brick 6 touches its subject matter; re-run focused before
  believing a failure.

## 8. Stopping rule: what this spec does NOT do

The blast radius stops here. These are named and excluded, which the spec
contract distinguishes from deferral. There is a paragraph for each of the five
overlapping pieces, 6 and 13 included - revision 1 omitted both and R1 caught it.

**TAKEN from piece 6 (the `/ws` request carrier), wholesale, because brick 5 is
that carrier.** Piece 6 half-built `SocketQuery` and `SocketSession` and its doc
comments reserved the extension points by name. This spec finishes them:
`?speed=` and `?duration_ms=` on the query, the ticket on the session, the
deletion of the configured-but-not-booted refusal, and the `?symbol=` carrier on
`/clock`. Nothing of piece 6 is left over. What is NOT taken is any further
request-carried parameter - composition above all, which is explicitly out below.

**TAKEN from piece 10 (one clock per boat), because it is forced.** Piece 9
cannot place a second boat without a second clock - pacing IS
`sim.wall_ns(ts)`. So this spec takes: `Boat::sim`; the per-connection reads in
`ws.rs` (the writer's `GoDark`/`StallData` gates, the exec pump's ACK holds, the
heartbeat period, the admission-rejection stamps); the engine's order-event
stamping and `AccountState` timestamps, which reach the boat through
`SocketSession`; the sweeper's per-pass `to_ns`; and `/clock`, which gains the
symbol carrier so it has a boat in scope.

**LEFT to piece 10.** `run_duration_ns` completion stays VENUE-level and
wall-anchored, and the durable prose says so: a venue's declared duration is a
lifecycle bound on the process, not a statement about any river. `/health` and
the readiness record likewise.

**TAKEN from piece 11, the watermark half only, because it is forced.** A single
`last_swept_ns` across two boats with two clocks is not a coarse watermark, it is
a meaningless one - the boats' sim instants are incomparable. So the watermark
moves onto the boat, and `frontier_after` keeps its guard unchanged. Ruling 1 is
what makes this sufficient: with at most one boat per river, each symbol's
pending scans are swept by exactly one clock, so no order is ever contended
between two watermarks.

**LEFT to piece 11.** `MarketReadingCache`'s one entry behind one mutex. It is a
pure CONTENTION defect - two symbols thrash a single-entry cache and serialize on
unrelated work - and it is correct, if slow, under any number of boats. Fixing it
here would put an unrelated cache rewrite inside the highest-risk landing in the
inventory.

**LEFT to piece 12.** `ReadyRecord`'s `symbol` field and the meaning of
`data_origin_ns` / `run_start_ns` / `warmup_ns` as river rather than venue
properties, and the `VERSION` 6 bump they imply. This spec leaves the record
exactly as it is, still reporting the boot symbol, which stays TRUE - a boot boat
is still placed at boot. It becomes incomplete rather than false, and piece 12
closes it. The knock-on the todo names (a seed reproduces a venue, while a
river's path also depends on when its boat was placed) is worth restating here as
evidence FOR this spec's brick 3 AND ruling 3: with a placement-origin cursor at
the river's FIXED origin, a boat's path does NOT depend on when it was placed,
only on its key. Piece 12 inherits a simpler question than the todo assumed.

**LEFT to piece 13, and this is a ruling, not an oversight.** TOTAL SYMBOL
RESOLUTION. After brick 5, `/ws?symbol=FOOBAR` for a symbol absent from the
profile map still returns a `400` through `unserved_symbol_refusal`, and
`Rivers::river` still cannot manufacture a river for it. R2 demanded piece 9
implement this; adjudication ruling 4 declines, because the program's recorded
sequencing binds it in pieces 8 and 13 and the piece-8 landing recorded the
`RiverKey` obligation ON piece 13. What piece 9 owes instead is SHAPE: nothing
in the boatyard may assume the profile map is the universe of servable symbols
beyond the ONE lookup piece 13 will total (sections 2.1 and brick 2), and brick
5 lands `an_unconfigured_symbol_is_still_refused_by_the_unserved_branch` so
piece 13 has a test to invert rather than a behaviour to discover. Also left to
piece 13: `/instruments` returning the resolved configuration, the adapter's
subscription guard, and the runtime funds rejection naming its currency.

**EXPLICITLY OUT.** Request-carried COMPOSITION - an agent asking for "MNQ, Asia
loop". `RiverKey` is widened to carry the bundle digest so that arriving later
costs no re-keying, but nothing on the wire can set it. That belongs to the
segment-sampler track.

**EXPLICITLY OUT.** Distinct speeds cohabiting one river, and with them per-boat
temporal ownership of orders and marks in the engine. Ruling 1 refuses the
request loudly; section 3.0 states the reversal path.

**EXPLICITLY OUT.** Mid-run generator-level havoc on shared water. Ruling 2
refuses the arm; section 3.4 states what a future design would have to look
like.

**EXPLICITLY OUT.** Throughput. Whether N boats fit on the machine is not a
design input, by standing ruling. Brick 1's measurement is a VERIFICATION of a
decided size, not a budget.

## 9. The one thing a reviewer should attack first

Section 3.5 and brick 3: the fixed placement origin. It is what makes a boat's
path a pure function of its key, what deletes both survey defects, and what lets
section 8 hand piece 12 a simpler question - but it also means a passenger
boarding an hour into a run boards an hour of sim behind and, at speed 1, never
catches up. The spec accepts that on the grounds that mid-stream boarding is the
design's explicit contract and the composition is homogeneous by the operator's
choice. If that homogeneity premise is wrong for any real consumer, the fixed
origin becomes a liveness problem rather than a determinism win, and the repair
is a request-carried origin in the sharing key.

Revision 1 pointed a reviewer at the control-generation re-derivation instead.
Both reviews attacked it and it did not survive; ruling 2 deleted the mechanism
rather than repairing it, which is the largest single reduction in this spec's
risk. That is the right outcome to note when judging whether the remaining
highest-risk item has been named honestly.

## 10. Findings rejected, and why

Every finding from both reviews was validated against the tree. All were
factually sound except where noted. These are the ones whose PROPOSED REPAIR is
rejected; the underlying defect in each case is still closed, by the adjudicated
route instead.

- **R1 finding 1's repair, a sim-time `SurgeWindow` applied directly to a
  boat's cursor.** REJECTED per adjudication ruling 2. The finding itself is
  valid and confirmed against `CheckpointIndex::arm_flow_surge`, and it is what
  killed the re-derivation mechanism. But the repair arms the chain and the
  cursor by two different paths, which R1 itself flags as its own audit, and it
  keeps runtime generator mutation of shared water - the thing ruling 2 removes
  on the grounds that generator havoc forks the river and therefore belongs in
  the sharing key at placement.
- **R2's third P1 repair, "immutable control history replayed from an earlier
  boundary".** REJECTED per ruling 2, same grounds. It is recorded in section
  3.4 as the shape a FUTURE design would need if mid-run generator havoc on
  shared water is ever genuinely wanted; nothing in the current consumer
  inventory needs it.
- **R2's first P1, that piece 9 must implement total symbol resolution with a
  default-profile path and an unknown-symbol regression test.** REJECTED per
  ruling 4: it contradicts the program's recorded sequencing, where pieces 8 and
  13 bind it and the piece-8 landing put the `RiverKey` obligation on piece 13.
  The observation is correct and revision 1's header did overclaim; the spec now
  states plainly that resolution stays partial after this landing, takes the
  SHAPING obligation (one totalable lookup), and lands a test piece 13 inverts.
- **R1 finding 3's first two alternatives, a per-boat order book or a
  sweeper keyed on something other than the boat.** REJECTED per ruling 1, which
  takes the third alternative - constrain speed to one value per river - because
  it needs no engine change at all. A per-boat book is a rewrite of settled
  per-symbol design inside the riskiest landing in the inventory.
- **R2's fourth P1 repair, a boat/sharing-key selector on `/clock` and on the
  control plane.** REJECTED as unnecessary rather than wrong: under ruling 1 a
  symbol names at most one boat, so the ambiguity the finding identifies does not
  arise and a second selector on the wire would be dead surface. Its secondary
  point - that a venue-clock fallback must not be mistaken for a boat's - is
  ACCEPTED and folded into brick 5, which labels the fallback.
- **R1 finding 6's framing that the dead measurement branch is "fine as an
  outcome".** Half rejected: the finding is accepted and its arithmetic is what
  ruling 5 rests on, but leaving the resize in brick 1 while still calling that
  brick risk-free is not. The resize is now decided, and brick 1 carries the
  banded-limit fragility explicitly.

Everything else from both reviews is FOLDED, not rejected: R1's findings 2 (tie
hazard, now a constraint on `place_cursor`), 4 (origin contradiction, closed by
ruling 3), 5 (owned ticket, join off the mutex, lock-order edge), 7 (the missing
piece-6 and piece-13 paragraphs) and 8 (the real test name, the dead
`warmup_ns` field now deleted from `Boatyard`, the brick-4 dead-code plan, the
third `live` deletion site), and R2's two lower-severity items (the join-latency
claim withdrawn and restated, `--alloc` versus RSS separated in brick 1).
