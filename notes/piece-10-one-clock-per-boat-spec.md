# Piece 10: one clock per boat - the remainder

Revision 2, 2026-08-15. Consolidates two independent reviews and implements
the orchestrator adjudication of the design questions they raised (the review
reports and the adjudication document were consumed into this revision and
deleted per the loop's convention), whose three rulings are BINDING on this
document. Section 9 is the finding-by-finding ledger,
including what was rejected and why.

Implementation specification, written against
`reference/technical-implementation-spec.md`. Spawned from
`notes/todo.md`: piece 10 of the "Landing the grand design: fourteen
pieces" inventory ("One clock per boat: every run-level singleton
assuming one now - engine event stamps, `/clock`, `AccountState`
timestamps, `run_duration_ns` completion, derived `sim_epoch_ns`. None
hard, all scattered."), and the design bullets it points at under Open
issues: "THE RIVER AND THE BOAT: how N tapes are shared" (the ONE CLOCK
PER BOAT paragraph and the piece-9 LANDED narrowings), "EVERY DECISION IN
THIS BLOCK OWES `reference/` AND `docs/` PROSE", and item 2 of "STILL
OPEN" (`ReadyRecord` under per-boat clocks - explicitly piece 12, and
excluded here).

Notes-class document: transient, no truth guarantee, nothing durable
cites it.

## 0. What piece 9 already absorbed - do not rebuild it

The boatyard landing took more of piece 10 than the inventory line
suggests. Verified against the tree on 2026-08-15, not from the
inventory:

1. **A boat owns a `SimClock`.** `Boat::sim` is built at placement in
   `Boatyard::board`: `sim_epoch_ns = self.origin_ns` (the yard's
   `origin_ns`, which `Run::new` passes as `started_ns`),
   `wall_anchor_ns = now_ns()` at placement, `speed` the quantized
   boarding speed. Boats therefore already differ from the run clock in
   BOTH wall anchor and speed, and differ from each other in either.
2. **The whole `/ws` socket path is on the boat clock.** `handle_socket`
   reads `boat_sim = session.ticket.boat().sim` once and threads it into
   the command dispatcher, `run_writer`, `spawn_exec_pump`, the
   heartbeat, the `FeedLagged` stamp, `ProtocolError` / `AdmissionRejected`
   stamps, and the passenger duration timer. So ENGINE EVENT STAMPS -
   the first named singleton - are done: `process_order_cmd` takes `sim`
   as a parameter and every `ts` it samples (`boundary_outcome`, the
   act-delay re-sample, the post-synthesis re-sample) is the boat's.
   `AccountState` rides those same engine events, so the PUSHED account
   snapshots are already boat-stamped.
3. **`/clock` already answers per boat.** `ClockQuery::symbol` resolves
   through `Boatyard::boat_for_symbol`; a boated river answers with the
   boat's `SimClock` and `server_now_ns = boat.published_ns`, and
   `ServerClock::boat_clock` tells the caller which answer it got.
   `reference/clock.md` states this and `crates/mogwai-cli/tests/serving.rs`
   `clock_answers_per_boat_when_a_symbol_is_named` pins it.
4. **The fill sweeper is per boat.** `spawn_fill_sweeper` iterates
   `run.boatyard.boats()`, takes `to_ns = sim_now_ns(boat.sim)`, and the
   settlement watermark `last_swept_ns` LIVES ON THE BOAT
   (`Boat::last_swept_ns`), not on the run. The todo's piece-11 sentence
   calling `last_swept_ns` "one process-wide settlement watermark" is
   stale; what remains of piece 11 is `MarketReadingCache`'s single entry
   behind one mutex, and that is not this spec's.
5. **Per-passenger completion exists.** `SocketQuery::duration_ms` is
   measured on the boat clock from boarding, and each passenger emits its
   own `RunComplete` and closes. So `run_duration_ns` is no longer the
   only completion notion; it is the VENUE deadline, which is a different
   thing and is treated as such below.

What that leaves is a smaller, sharper item than the inventory line: the
run clock still exists and is still read at NINE `state.sim()` call sites
in `http.rs` (plus the `serve.rs` deadline task and the sweeper's interval
conversion), where it now means something different from what the socket
path means by "now". Revision 1 of this spec said "five" in two places
while its own table listed seven rows; the count verified against the tree
on 2026-08-15 is nine reads in `http.rs` - two `window_until_ns` arms
(`GoDark`, `StallData`), the `FlowSurge`-family stamp, the `FeeSurcharge`
arm, the `CancelOpenOrderSilently` stamp, `account`, `clock`'s boatless
fallback, and the two history ceilings in `trades` and `quotes`.

## 1. The defect the remainder actually is

`build_run_clock` anchors `sim_epoch_ns = TAPE_ORIGIN_NS + warmup_ns` and
`wall_anchor_ns = now_ns()` at boot. `Boatyard::board` anchors the boat at
the SAME epoch (`origin_ns` is `run_start_ns`, which `serve.rs` computes as
`TAPE_ORIGIN_NS + cfg.warmup_ns` - identical expression) but at the
PLACEMENT wall. A boat placed `T` wall-nanoseconds after boot is therefore
behind the run clock by `T * speed` simulated nanoseconds, permanently and
by construction. Two consequences, both live:

- **The run clock is a look-ahead oracle for every boated river.** It
  reports a sim instant strictly later than anything that river's boat has
  published.
- **A window armed on the run clock and evaluated on a boat clock is the
  wrong length**, and for a late-placed or slow boat can be a window the
  boat never enters at all.

The surviving run-clock reads, each with the consequence:

| Site | Reads | Consequence |
|---|---|---|
| `http::trades` / `http::quotes` | `sim_now_ns(state.sim())` for the `history_start_refusal` ceiling and the `end` clamp | serves ticks LATER than the boat has published for that symbol; a client warming up a bar aggregator gets history that overlaps the future of its own live feed |
| `http::account` | `sim_now_ns(state.sim())` as `account_snapshot(ts)` | the PULLED account snapshot is on a different axis from every PUSHED one, and nothing in the response says so. NOT a defect to be fixed by picking a boat - see ruling 1 in section 3.3: the ledger is venue-scoped, so the venue stamp is correct and what is missing is the LABEL |
| `http::clock` | `state.sim()` as the boatless fallback | correct as it stands, and already labeled by `ServerClock::boat_clock`. Listed because the rewrite onto the resolver must preserve it |
| `arm_divergence`, `GoDark` / `StallData` | `window_until_ns(sim_now_ns(state.sim()), ms)` | consumed in `run_writer` against `sim_now_ns(boat_sim)`, so the observed window is `ms + (boat lag)` rather than `ms` |
| `arm_divergence`, `FeeSurcharge` and `CancelOpenOrderSilently` | `sim_now_ns(state.sim())` into engine state | the surcharge window `start_ns..end_ns` is compared against order `ts` values sampled on boat clocks; same skew |
| `arm_divergence`, `FlowSurge` (and `VolStorm` / `LiquidityDrought` by the same path) | `sim_now_ns(state.sim())` into `Rivers::arm_flow_surge` | worse in kind: this is only reachable on a river with NO seated boat, and a boat placed afterwards starts at the river ORIGIN. A window stamped at wall-derived sim-now is in that boat's far future and may never be entered |
| `serve.rs` deadline task | `sim.sim_ns(now_ns())` for the venue `RunComplete` | the terminal frame every socket receives is stamped on a clock none of them uses |
| `sweeper.rs` interval conversion | `run.sim.wall_duration(...)` | not a stamp, a cadence. Its comment says "every boat on it shares the configured speed", which STOPPED BEING TRUE when `/ws?speed=` landed |

## 2. The decision this spec takes

**The venue keeps exactly one wall-to-sim reference, and it stops being
called "now" for anything a boat owns.**

Rejected alternative, recorded so it is not re-walked: deleting `Run::sim`
outright. It cannot go. Four answers have no boat to ask - `/clock` with
no `?symbol=`, `/trades` on a configured-but-boatless river, the venue
deadline, and, per ruling 1, the venue-scoped account ledger - and history
synthesis is unbounded on demand, so without a
venue-level ceiling a boatless `/trades` would happily synthesize
arbitrarily far forward. The run clock is the honest ceiling for water
nobody is carrying.

So the rule, in four lines, and it is what the durable prose must say:

1. **A boated river's now is its boat's `published_ns`.** Not the boat's
   affine projection either - the boat is deliberately behind its own map
   (`/clock` already says this), and a ceiling above what was published is
   the same look-ahead in a smaller costume.
2. **A boatless river's now is the venue clock.** Nothing has been
   published there, so the only bound available is the venue's own
   elapsed simulated time. `boat_clock: false` already names this case on
   `/clock`; the same flag's reasoning now governs history.
3. **A window carries no clock at all; it is JUDGED on the reader's.**
   Revision 1 said "armed on the clock it will be judged against", which
   presumes the armer can know who will read it - and it cannot, since a
   passenger may board afterwards. A window is stored as a wall instant
   plus a simulated SPAN, and each reader opens it on its own clock,
   at its own anchor if that anchor is later than the arm. See 3.2; this
   is the rule the durable prose must carry.
4. **A venue-scoped answer keeps the venue clock and is LABELED.** The
   account ledger is the one such answer today. Labeling is what makes a
   venue stamp honest rather than a look-ahead in disguise; `/clock`'s
   `boat_clock: false` is the existing precedent and `/account`'s `clock`
   field is the new one.

## 3. Concrete artifacts

### 3.1 One resolver, in `http.rs`, replacing the ad-hoc reads

The resolver carries the whole `SimClock`, not just an instant. R1 finding
2 is accepted: `http::clock` returns `sim_epoch_ns`, `wall_anchor_ns` and
`speed`, so a resolver that yielded only `ns` could not carry that
endpoint and L1's "rewritten on top of `river_now`" would have been
unimplementable. R3's directions make this binding.

```rust
/// How a river's now was resolved, and on what clock.
///
/// A boated river answers with what its boat has PUBLISHED; a boatless
/// river answers with the venue clock, which is the only ceiling water
/// nobody is carrying has. Never the boat's affine projection: a boat is
/// deliberately behind its own map, and a ceiling above the published
/// tape is a look-ahead.
pub(crate) struct RiverNow {
    /// The ceiling a request about this symbol may be answered as of.
    /// Boated: `boat.published_ns`. Boatless: `sim_now_ns(venue_sim)`.
    pub(crate) ns: u64,
    /// The clock that instant lives on - the boat's `SimClock` when
    /// boated, the venue's otherwise. `/clock` renders this whole value;
    /// `HavocWindow::open_at` judges against it.
    pub(crate) sim: SimClock,
    /// True when `sim` is a boat's. Renders as `ServerClock::boat_clock`
    /// and as the `/account` clock label's sibling.
    pub(crate) from_boat: bool,
}

impl AppState {
    /// Awaits placement rather than falling through: see below.
    pub(crate) async fn river_now(&self, symbol: &str) -> RiverNow { .. }
    /// The venue clock. The ONLY callers left are the boatless answers,
    /// `/account` (ruling 1), and the venue deadline; anything about a
    /// symbol calls `river_now`.
    pub(crate) fn venue_sim(&self) -> SimClock { self.run.sim }
}
```

`AppState::sim` is RENAMED to `venue_sim` in the same landing. The rename
is the point: the current name is what let nine call sites read a venue
clock while believing they read "the clock", and a compiler error at every
one of them is the cheapest audit available.

`http::clock` is rewritten on top of `river_now` rather than keeping its
own copy of the boat lookup; `ServerClock::boat_clock` becomes
`RiverNow::from_boat`, so the endpoint and the history ceiling can no
longer disagree about whether a river is boated.

**The placement race is closed inside the resolver.** R1's smaller finding
on `Slot::Placing` is accepted: `Boatyard::boat_for_symbol` matches only
`Slot::Seated`, so a request arriving while a boat is being placed sees
`None` and silently receives the venue clock - precisely the look-ahead
this spec exists to remove, and it is INVISIBLE because the answer is
well-formed. Per R3, `river_now` WAITS on the placement handoff: the
`Slot::Placing(Arc<Semaphore>)` the boatyard already installs is the
handoff `board` itself awaits, so the resolver acquires the same permit
and re-reads the slot. This is why `river_now` is `async`. A river with
no slot at all is boatless and answers immediately - the wait is only for
a placement already in flight, and placement failure re-reads to `None`
and falls through to the venue clock, which is then the truthful answer.
`Boatyard` gains one method for this (`boat_for_symbol_awaiting_placement`
or equivalent); `boat_for_symbol` keeps its non-blocking form for
`health`, which must never block on a placement.

### 3.2 One clock-neutral window shape for ALL havoc spans

Binding: R3 ruling 2. Revision 1 gave the transport windows a
duration-plus-wall-instant shape and explicitly EXCLUDED `FeeSurcharge`
from it, proposing instead to stamp the surcharge from a named boat. R2
finding 2 proved that insufficient and it is accepted: the engine stores
ONE `FeeSurchargeWindow` as an absolute `start_ns..end_ns`
(`mogwai-engine/src/lib.rs`, `arm_fee_surcharge` /
`fee_surcharge_multiplier_at`), and every order tests its own
BOAT-STAMPED `ts` against that one interval. Choosing whose clock to arm
from cannot repair that: an interval on boat A's axis is applied verbatim
to orders on boat B's, so the surcharge fires for the wrong span, or never.

So there is ONE representation, used by the transport windows and by the
fee surcharge alike:

```rust
/// A havoc window, armed at a WALL instant for a SIMULATED span, judged
/// on whatever clock the reader owns. Stored as (wall_armed_ns,
/// sim_span_ns) rather than as an absolute sim deadline, because the
/// venue has no single sim axis to express a deadline on: the same
/// window must mean `ms` simulated milliseconds to a passenger on a fast
/// boat and to one on a slow one.
///
/// Behind a `Mutex`, not two atomics. Arming is a cold path - an
/// operator control - and two independent `AtomicU64`s are a TORN READ:
/// a concurrent reader can pair the new wall instant with the old span,
/// and a clear can race a re-arm and erase the new span. The
/// `AtomicU64` this replaces was tear-free by construction, so the
/// atomic pair would have been a regression introduced by the fix. No
/// packed encoding: two independent nanosecond quantities do not fit
/// one u64 without a range limit nobody can audit later.
pub(crate) struct HavocWindow(Mutex<Option<ArmedSpan>>);

#[derive(Clone, Copy)]
struct ArmedSpan {
    wall_armed_ns: u64,
    sim_span_ns: u64,
}

impl HavocWindow {
    pub(crate) fn arm(&self, wall_now_ns: u64, sim_span_ns: u64);
    pub(crate) fn clear(&self);
    /// Judged on the reader's own clock. The opening instant on that
    /// clock is `max(sim.sim_ns(wall_armed_ns), sim.sim_epoch_ns)` - the
    /// LATE-BOARDER RULE - and the window is open while `sim_at_ns` is
    /// inside `[opening, opening + sim_span_ns)`.
    pub(crate) fn open_at(&self, sim: SimClock, sim_at_ns: u64) -> bool;
}
```

**The late-boarder rule is the load-bearing half.** R1 finding 4 is
accepted: projecting `wall_armed_ns` through the clock of a boat whose
`wall_anchor_ns` is LATER than the arm puts the window in that boat's
past, so it never opens - the identical defect section 1's table records
for `FlowSurge`, which revision 1 fixed for generator havoc while leaving
transport windows exposed to it (arm a `GoDark`, a client connects 50 ms
later, the blackout silently does not happen). Under the rule, a reader
whose anchor is after the arm treats ITS OWN ANCHOR as the opening instant
and consumes the FULL span on its own clock. One rule, applied uniformly,
and the generator-havoc origin stamp of revision 1 becomes a special case
of it rather than a second mechanism.

Consequences, per call site:

- `Run::dark_until_ns` and `Run::stall_until_ns` become
  `Run::dark: HavocWindow` and `Run::stall: HavocWindow`. `arm_divergence`
  stores `(now_ns(), sim_duration_from_millis(ms))`; `run_writer` asks
  `state.run.dark.open_at(boat_sim, sim_now_ns(boat_sim))`.
  `ClearDivergences` calls `clear()`, which stores `None` - `open_at` is
  then false for every clock, preserving the existing cleared-sentinel
  property. STORE-not-extend survives because the whole `ArmedSpan` is
  replaced under the lock, which is now true rather than asserted.
- `FeeSurcharge` moves to the SAME shape inside the engine.
  `arm_fee_surcharge(mult, window_ms, now_ns)` becomes
  `arm_fee_surcharge(mult, wall_armed_ns, sim_span_ns)`, and
  `fee_surcharge_multiplier_at(ts)` becomes
  `fee_surcharge_multiplier_for(sim, ts)` - the order's own boat clock and
  its own boat-stamped `ts`. The engine is venue-wide and stays so; what
  changes is that the stored window no longer names an axis. All three
  order-path call sites (`orders.rs` maker, taker and the liquidation
  guard) pass the clock they already have. The existing engine unit tests
  `a_fee_surcharge_bills_above_the_advertised_schedule_and_expires_on_sim_time`
  and `a_re_armed_fee_surcharge_replaces_the_earlier_window` are updated
  to the new signature and must keep asserting the same behavior for a
  single-clock run.
- `CancelOpenOrderSilently` derives its clock FROM THE TARGETED ORDER, not
  from a request field. R2's point stands: `client_order_id` already
  determines the order and therefore its symbol and its river, and a
  request-supplied `symbol` can disagree with it. The protocol variant
  carries no symbol of its own; the surrounding `DivergenceRequest::symbol`
  is, for this variant, VALIDATED against the resolved order's symbol and a
  mismatch is refused with a 400 naming both. It is not used to pick a
  clock.
- `FlowSurge` / `VolStorm` / `LiquidityDrought` keep their existing refusal
  on a seated river and change their stamp from `sim_now_ns(state.sim())`
  to the late-boarder rule above, which on a boatless river means the
  eventual boat opens the window at its own epoch and gets the full span.
  The arming response carries the armed span so an operator can see what
  was armed against what.

### 3.3 `/account` stays on the venue clock, and says so

Binding: R3 ruling 1. Both reviews found revision 1's largest hole - the
endpoint takes no query (`pub(crate) async fn account(State(state))`), so
`river_now(symbol)` has no symbol, and the engine behind it is one
venue-wide `Mutex<Engine>` serving every river. R1's proof is accepted:
the L2 monotonicity test was unsatisfiable under EVERY candidate. Stamp
from the boot boat and a push from a later-placed boat on another river is
AHEAD of the pull; stamp from the newest boat and it is behind. There is
no boat axis for a venue-scoped ledger, and inventing one manufactures a
monotonicity promise no choice can keep.

So: the pulled snapshot keeps the venue stamp and gains a LABEL, mirroring
`/clock`'s `boat_clock` fallback labeling.

```rust
/// The HTTP shape of a pulled account snapshot. `AccountState` itself is
/// UNCHANGED - it is also the pushed frame's payload, and the pushed path
/// is per-boat and correct. The label is added by the HTTP response only.
#[derive(Serialize)]
pub(crate) struct AccountSnapshot {
    /// Always `"venue"` today. Present so a consumer can never mistake
    /// `ts_event` here for boat time.
    clock: ClockAxis,
    #[serde(flatten)]
    account: AccountState,
}
```

`#[serde(flatten)]` keeps every existing field at the same position in the
object, so an existing consumer that ignores unknown fields - including
`mogwai-adapter`'s `client/shared.rs` - parses it unchanged. That
compatibility is asserted, not assumed: `brokkr check --gate` covers the
adapter binaries and L2 names the check.

The prose owed (section 7) states it plainly: PULLED ACCOUNT TIME IS VENUE
TIME. Order a pulled snapshot against pushed events by sequence, never by
comparing the two clocks.

### 3.4 The venue deadline stops lying about whose clock it is

`serve.rs`'s deadline task keeps the run clock - the venue's lifetime is a
venue property and there is no boat to ask when the last one has left. What
changes is the frame: `ServerMessage::RunComplete` currently carries
`sim_now_ns` and `elapsed_ns` computed on the run clock and is broadcast
to sockets whose every other stamp is a boat's.

The fix is local to `ws.rs`: the venue-completion arm RE-STAMPS on the
socket's own clock, exactly as the passenger-duration arm already does
(`let now = sim_now_ns(boat_sim);`). `Run::complete` keeps broadcasting the
venue instant as the SIGNAL; the per-socket arm converts it. `elapsed_ns`
for the venue case is `sim_now_ns(boat_sim) - boat.sim.sim_epoch_ns`
clamped at zero - how much tape THIS passenger's boat covered, which is
the only elapsed number meaningful to the reader.

**There are TWO arms, not one.** R1 finding 6 is accepted and verified:
`ws.rs` also has an `already_complete` path that fires when a socket
connects AFTER the deadline has passed, and it sends a `RunComplete`
carrying the venue-clock `(sim_now_ns, elapsed_ns)` pair straight out of
`current_completion`. Revision 1 named only the live-broadcast arm, and
the L4 test as written would not have reached the other one. Both arms
re-stamp identically, and L4 covers both - the post-deadline connect is
the easier of the two to construct, since it needs no timing race.

### 3.5 The sweeper gets a schedule, not a comment fix

Binding: R3 ruling 3. Revision 1 said the duration conversion moves
"inside the boat loop". Both reviews showed that is not implementable as
described, and it is accepted: the sleep is the OUTER `'passes` loop's
select against `completion.changed()`, and boats are swept sequentially
inside one pass. Sleeping per boat inside the loop would ADD their delays
together; keeping one outer venue sleep leaves some boats at the wrong
cadence. R1 is also right that the severity was overstated - per-boat
`to_ns` and per-boat `last_swept_ns` already make SETTLEMENT correct, and
what a shared cadence costs is granularity and trigger latency, not
correctness.

The ruling: ONE task, earliest-deadline scheduling. Rejected explicitly,
per R3: per-boat sweeper tasks. They buy cadence granularity at the price
of contending the one engine lock N ways and fanning out the completion
watch, and the thing bought is not load-bearing for settlement.

```
loop {
    // Re-derived every pass: boats appear and leave under this task.
    for boat in run.boatyard.boats() {
        next_due.entry(boat.key).or_insert(now + wall_interval(boat));
    }
    next_due.retain(|key, _| still_seated(key));

    // Boatless: nothing to sweep, but the loop must still tick to
    // observe completion, so fall back to the venue-clock interval.
    let deadline = next_due.values().min().copied()
        .unwrap_or(now + venue_wall_interval());

    select! {
        _ = sleep_until(deadline) => {}
        _ = completion.changed() => break,
    }

    for (key, due) in next_due.iter_mut().filter(|(_, d)| **d <= now()) {
        sweep(boat_for(key));               // as today
        *due = now() + wall_interval(boat); // floored at MIN_SWEEP_WALL
    }
}
```

`wall_interval(boat)` converts the configured sweep interval through THAT
BOAT's `SimClock`, floored at `MIN_SWEEP_WALL` as today - which is the
substance of the original complaint: a `?speed=1` boat and a `?speed=100`
boat share a cadence derived from the CONFIGURED speed, so one is swept at
the wrong simulated interval. A boat seated mid-pass is due immediately on
the next pass, which is the same latency it has today. The stale comment
goes with the mechanism, and unlike revision 1 this section names its test
(L4, section 6), which the spec's own closing rule requires.

### 3.6 What is deleted

- `AppState::sim` (renamed, not kept as an alias - an alias is how the
  five call sites got here).
- `Run::dark_until_ns`, `Run::stall_until_ns` as bare `AtomicU64`.
- `Engine::fee_surcharge_multiplier_at`'s clock-blind `ts`-only form, and
  `FeeSurchargeWindow`'s absolute `start_ns..end_ns`.
- The `end`-clamp comment in `trades` reasoning that a consumer stamps its
  own clock "a hair ahead of the venue's". Under the new ceiling the gap
  is `T * speed`, not a hair, and the comment must be rewritten WITH the
  clamp rather than left to mislead the next reader.
- The `/instruments` doc comment claiming "exactly one paced boat is
  placed, at boot" - false since piece 7/9.
- The sweeper comment "every boat on it shares the configured speed".

## 4. Stopping rule - what is explicitly NOT in this spec

- **Piece 11.** `MarketReadingCache`'s single entry behind one mutex. It
  is passed a `ts` and is clock-agnostic; nothing here touches it.
- **Piece 12.** `ReadyRecord`'s `symbol` drop and the `VERSION` bump to 6,
  and what `data_origin_ns` / `run_start_ns` / `warmup_ns` mean as river
  properties. This spec deliberately leaves `ReadyRecord` byte-identical
  so the two landings do not collide on `mogwai-protocol`.
- **Piece 13.** `/instruments` returning the resolved configuration and
  the adapter subscription guard. The `/instruments` change here is one
  stale doc comment, nothing behavioral.
- **The boatless-river sweep gap** recorded under the piece-9 bullet in
  `todo.md` (a resting order on a river whose boat wound down is never
  swept). It is a lifecycle question, not a clock question, and both
  candidate answers there are decisions this spec has no standing to
  take.
- **`TAPE_PROTOCOL_VERSION`.** Nothing here reaches the tape generation
  path: no generator constant, no arrival-clock or GARCH parameter, no
  fingerprint, no seed derivation, no fill-band draw, no tape origin.
  Clocks decide WHEN a tick is delivered and which instant an event is
  stamped at, never which tick it is. No bump is owed.

## 5. Landing order

Four landings, each green at its boundary, each independently
revertible.

**L1 - the resolver and the rename.** Introduce `RiverNow`,
`AppState::river_now`, rename `sim` to `venue_sim`, and rewrite
`http::clock` on top of it, including the placement wait. Mechanical at
every call site except `clock` itself; no CEILING changes yet, because
every other caller still passes the venue clock through. The one behavior
change L1 does carry is the placement wait, which turns a request racing a
placement from a wrong-but-instant answer into a right-but-briefly-blocked
one - and it is `/clock`'s answer that changes, which is why the
`clock_answers_per_boat_when_a_symbol_is_named` gate below is not a pure
refactoring check for that case. This is the landing that makes the
remaining three small.

**L2 - history and the account label.** `trades` and `quotes` switch to
`river_now`; `account` keeps `venue_sim` and gains the `AccountSnapshot`
label. This is the first behavior change and the one a consumer can
observe. `bounded_quotes` versus `bounded_trades` on `start` (lateral
finding 3) is resolved HERE, before the ceiling moves, not after.

**L3 - the havoc windows.** `HavocWindow` with the late-boarder rule, the
transport windows and the fee surcharge moved onto it, the
order-derived `CancelOpenOrderSilently` clock, and the generator-havoc
family. This landing crosses into `mogwai-engine`, which the earlier ones
do not.

**L4 - completion and cadence.** Both `RunComplete` re-stamps and the
earliest-deadline sweeper. Plus the durable prose (section 7), which
lands WITH the code per the standing todo item, not after it.

## 6. Verification, per landing

Every command below is copy-pasteable as written.

### L1

- `brokkr check` - the rename is a compile-time audit; a call site this
  spec missed is a build error, which is the intended gate.
- `brokkr test -p mogwai-cli clock_answers_per_boat_when_a_symbol_is_named`
  - `/clock`'s per-boat answer must be unchanged by the rewrite onto
  `river_now`. Free refactoring check per the exact-equality-gates rule,
  for the already-seated case; the racing-placement case is new behavior
  and is pinned by the L2 test named below.

### L2

- **New test, and it is a brick of this spec**, in
  `crates/mogwai-cli/tests/serving.rs`:
  `history_is_bounded_by_the_rivers_own_boat_not_the_venue_clock`. Boot a
  venue, let the boot boat run, then open a `/ws` socket on a SECOND
  configured symbol so its boat is placed measurably later. Read
  `/clock?symbol=<second>` for the boat's `server_now_ns` and `/clock`
  with no symbol for the venue's, and assert the venue is measurably
  later - if it is not, the test has not yet built the condition it
  measures and must widen the delay rather than pass vacuously.
  The request is START-ANCHORED, per R2 finding 5 and R3's direction:
  `/trades?symbol=<second>&start=<just below the boat boundary>`.
  Revision 1's unanchored form could not be trusted to bite, because
  `bounded_trades` starts from the history source's default position and
  BREAKS at `limit`, so it can fill the page with old rows and never
  reach either ceiling. The pre-fix response must contain at least one
  row in `(boat_published_ns, venue_now_ns]` - the test asserts that on
  the bite-check run - and the post-fix response none.
  The existing `trades_after_sim_now_are_refused_with_400` must stay
  green, which requires `history_start_refusal` to be fed
  `river_now(symbol).ns` and its message to keep naming the instant it
  refused against.
  BITE-CHECK: revert `river_now` to `venue_sim` in `trades` as a TEXT
  EDIT, observe the named failure, restore it as a text edit. Never
  `git checkout -- <path>`.
- **New test**: `a_pulled_account_snapshot_is_labeled_venue_clock`, same
  file. Revision 1's
  `a_pulled_account_snapshot_is_not_stamped_ahead_of_a_pushed_one` is
  DELETED per ruling 1 - it is unsatisfiable in both directions and would
  have been a permanently red or permanently vacuous gate. What replaces
  it asserts the `clock` label is present and reads `venue`, and that
  every pre-existing `AccountState` field is still at the top level of
  the object (the `serde(flatten)` compatibility claim of section 3.3,
  asserted rather than assumed).
- **New test**: `history_answers_after_a_placement_rather_than_racing_it`.
  Issue `/trades?symbol=<second>` CONCURRENTLY with the `/ws` upgrade that
  places that river's boat, and assert the answer is bounded by the boat,
  never by the venue clock. This is the `Slot::Placing` hole of section
  3.1; without it the resolver's placement wait is unpinned. If the race
  proves too tight to construct reliably from the outside, it is pinned
  instead as a `mogwai-server` unit test driving `Boatyard` directly with
  a placement held open - a flaky socket test is worth less than a
  deterministic unit one.
- `brokkr check --gate` - `mogwai-adapter` consumes `/account` and
  `/trades` through `client/shared.rs`, and the four socket-backed
  adapter binaries are invisible to plain `brokkr check`.
- `brokkr run mogwai -- serve` then `python3 scripts/smoke.py` - the
  end-to-end live path. The smoke test reads history and account and is
  the cheapest check that neither ceiling went backwards far enough to
  return an empty warmup.

### L3

- **New test**: `an_armed_stall_window_lasts_its_declared_span_on_a_late_boat`.
  Place a boat late (open the socket after a measured delay), arm
  `StallData` with a short `ms`, and assert market data resumes within a
  bounded multiple of that span measured on the BOAT's clock. Today the
  observed window is `ms` plus the boat's lag, so a lag exceeding `ms`
  makes the window never open at all - assert on the resumption, not on
  an error, per the standing rule that a test observing only an error
  cannot distinguish a bound from a check performed after the damage.
  The existing serving test at line ~604 (`StallData` with 180000 ms)
  must stay green.
- **New test**: `a_window_armed_before_a_boat_boards_still_opens_for_it`.
  Arm `GoDark` on a river with no seated boat, THEN open the socket, and
  assert the blackout is observed for its declared span on that boat's
  clock. This is the late-boarder rule and it is the finding revision 1
  missed; without this test the rule is unpinned for the transport
  windows, which is exactly where the hole was.
- `brokkr test -p mogwai-server` for the `HavocWindow` unit tests:
  `arming_replaces_rather_than_extends`, `a_cleared_window_is_open_for_no_clock`,
  `the_same_window_spans_equal_sim_time_on_two_different_speeds`,
  `a_reader_anchored_after_the_arm_opens_at_its_own_epoch`, and a
  concurrency test - arm, clear and read from separate tasks in a loop,
  asserting no observation ever pairs one arm's wall instant with
  another's span. The last one is what the mutex buys and the atomic
  pair would have failed.
- `brokkr test -p mogwai-engine fee_surcharge` for the migrated engine
  window, plus a new
  `a_surcharge_armed_once_spans_equal_sim_time_for_two_order_clocks`.
- **New test**: `a_silent_cancel_naming_the_wrong_symbol_is_refused`, for
  the order-versus-request symbol validation of section 3.2.
- A `/ws` test may never assert on THE NEXT frame - every socket is
  attached to the live tape on upgrade - so every new socket test drains
  to a deadline.

### L4

- `brokkr test -p mogwai-cli --` the completion suite:
  `crates/mogwai-cli/tests/completion.rs` already reads
  `venue.record.run_duration_ns` and pins the terminal sequence. Extend
  it with `run_complete_is_stamped_on_the_receiving_sockets_clock`:
  TWO RIVERS at different `?speed=`, one venue deadline, two
  `RunComplete` frames with DIFFERENT `sim_now_ns`. Equal values are the
  failure. Two sockets at different speeds on ONE river, as revision 1
  wrote it, is UNCONSTRUCTIBLE - `Boatyard::board` returns
  `BoardRefusal::SpeedInUse` when a seat exists at a different
  `speed_micros` (R1 finding 5, verified in `boatyard.rs`). R3's
  direction makes two rivers binding.
- **New test**:
  `a_socket_connecting_after_the_deadline_is_stamped_on_its_own_clock`,
  for the `already_complete` arm of section 3.4. Connect after the venue
  deadline has fired and assert the single terminal frame carries the
  boat's instant, not the venue's.
- **New test**: `two_boats_at_different_speeds_are_swept_at_their_own_cadence`,
  for section 3.5. Two rivers at materially different `?speed=`, resting
  orders on both, and an assertion on SWEEP TRIGGER LATENCY - the wall
  delay from a boat's tape crossing a resting order's price to the fill
  appearing - bounded per boat by that boat's own converted interval.
  Assert on the fills, not on an error: a test observing only an error
  cannot distinguish a bound from a check performed after the damage.
- `brokkr check --gate` and the smoke test again, as the closing gate.

If a gate above has no command because no test pins the behavior, the
test is named in this section and is itself a brick - laid before the
change it gates.

## 7. The durable prose owed (part of L4, not after it)

Per the standing `todo.md` item that every decision in that block owes
`reference/` and `docs/` writing WITH the code:

- `reference/clock.md` - currently says "`SimClock` maps wall time to
  simulated time for one run" and describes the run clock as THE clock.
  Rewritten around the four rules of section 2: a boat's clock, a
  boatless river's fallback, a window judged on the reader's clock, and a
  labeled venue-scoped answer - and where each is used. The existing
  `?symbol=` paragraph is correct and is generalized from `/clock` to
  every symbol-bearing endpoint.
- `reference/architecture.md` - the sentence on boat placement and
  per-boat clocks gains what a venue clock still IS and the three things
  it still answers - a boatless river, the venue deadline, and the
  venue-scoped account ledger.
- `docs/havoc.md` - EVERY havoc window, transport and fee surcharge
  alike, is declared in simulated milliseconds measured on the RECEIVING
  passenger's clock, and a passenger who boards after the arm gets the
  full span from its own boarding, not the remainder. A user arming a
  60-second blackout against a speed-100 passenger needs to know which 60
  seconds. `CancelOpenOrderSilently` takes its clock from the targeted
  order and refuses a mismatched `symbol`.
- `docs/cli.md` - `/trades` and `/quotes` state their ceiling: a boated
  river answers only as far as its boat has PUBLISHED, so a request with
  no `end` now returns materially less than before, by `T * speed` where
  `T` is how long after boot the boat was placed. `/account` states that
  its `ts_event` is VENUE time and carries the `clock` label saying so,
  and that a pulled snapshot is ordered against pushed events by
  sequence, never by comparing timestamps across the two clocks. One
  caveat gets its own sentence: a client that reads `/clock` WITHOUT
  `?symbol=` and passes the result as `start` to `/trades` will now get a
  400, because that instant is above the boated river's ceiling - read
  `/clock?symbol=` instead.

## 8. Lateral findings surfaced by the survey, not fixed here

Recorded because they were found while reading, and each wants a decision
rather than a silent fix:

1. **`Boatyard::boat_for_symbol` is a linear scan returning the FIRST
   match by symbol.** The boat registry is keyed by `RiverKey`, and the
   sharing key includes composition - so two rivers can carry the same
   SYMBOL with different bundles, and `/clock?symbol=`, the generator-
   havoc seated-boat refusal, and `health`'s fault lookup would all
   silently pick whichever hashed first. Not reachable today (one bundle
   per configured symbol), and it becomes reachable the moment the
   segment-sampler track lets a client name a composition. This spec's
   `river_now` inherits the same weakness by construction. The related
   `Slot::Placing` half of this - a request racing a placement silently
   receiving the venue clock - is NOT deferred; R1 raised it and R3 ruled
   it closed here, in section 3.1.
2. **`Run::boot_symbol` plus the retained boot ticket** means the boot
   river always carries a boat, so its lag against the venue clock is
   zero and every existing test that reads history for the boot symbol
   passes under both the old and the new ceiling. That is exactly why the
   L2 test above must use a SECOND symbol; a boot-symbol test would not
   bite.
3. **`bounded_quotes` filters on `start` while `bounded_trades` does
   not** - already recorded in `todo.md` as unresolved, and it sits
   directly under the ceiling this spec changes. Promoted from "worth
   resolving" to an L2 PRECONDITION: the biting history test of section 6
   is start-anchored, so the two functions disagreeing about what `start`
   means is now load-bearing for the gate, not just untidy.

## 9. Finding ledger - what each review raised and its disposition

Every finding from R1 and R2 was validated against the tree before
disposition. The two reports overlap on three findings; those are
consolidated into one row each.

### Accepted and folded

| # | Source | Finding | Where it landed |
|---|---|---|---|
| 1 | R1.1, R2.1 | `/account` has no symbol and the L2 monotonicity test is unsatisfiable | 3.3, and the test is deleted from section 6 |
| 2 | R1.2 | `RiverNow` cannot carry `/clock` - the endpoint needs the whole `SimClock` | 3.1 |
| 3 | R1.3, R2.3 | Two `AtomicU64`s are a torn read, a regression on what they replace | 3.2, mutex |
| 4 | R1.4 | Transport windows have the arm-then-board hole the spec fixes only for generator havoc | 3.2, the late-boarder rule, applied uniformly |
| 5 | R2.2 | A symbol-aware `FeeSurcharge` is still ONE run-wide absolute interval; naming a symbol repairs nothing | 3.2, the surcharge moves onto the same clock-neutral shape |
| 6 | R2.2b | `CancelOpenOrderSilently` must take its clock from the targeted order, not a request symbol | 3.2, with mismatch refusal and a test |
| 7 | R1.5 | The L4 test is unconstructible - two speeds on one river is `SpeedInUse` | 6/L4, two rivers |
| 8 | R1.6 | `ws.rs` has a second `already_complete` `RunComplete` arm | 3.4, and a test in L4 |
| 9 | R1.7, R2.4 | The sweeper section is not implementable and names no test | 3.5, earliest-deadline schedule plus a trigger-latency test |
| 10 | R2.5 | The history test is not guaranteed to bite - `bounded_trades` breaks at `limit` | 6/L2, start-anchored form with a demonstrated pre-fix row |
| 11 | R1 small | "Five reads" contradicts a seven-row table; the tree has nine in `http.rs` | 1 |
| 12 | R1 small | The `end`-clamp comment's "a hair ahead" is now `T * speed` | 3.6 and 7 |
| 13 | R1 small | `/clock` without `?symbol=` used as `start` now 400s | 7, `docs/cli.md` |
| 14 | R1 small | `Slot::Placing` returns `None`, so a placement race silently gets the venue clock | 3.1, the resolver awaits the handoff |

R1's smaller note that `boat_for_symbol`'s first-match weakness also
reaches `health` is folded into lateral finding 1, which already recorded
the weakness; `health` keeps the non-blocking lookup by design (3.1).

### Rejected

- **"Pack the window into one `AtomicU64`" (R1.3's parenthetical
  alternative).** Rejected per adjudication ruling 2, which directs a
  mutex and says plainly not to invent a packed encoding. Arming is a cold
  operator path; two independent nanosecond quantities do not fit one u64
  without a range limit that outlives the reader who could audit it. R2's
  "versioned/seqlock protocol" alternative is rejected on the same
  grounds. The finding itself is ACCEPTED - only the alternative is
  refused.
- **"Give `/account` a boat axis - a `symbol` parameter, min over seated
  boats, or session identity" (R1.1 and R2.1's proposed remedies).**
  Rejected per adjudication ruling 1: the ledger is venue-scoped by
  settled design, there is no boat axis for it, and every candidate
  manufactures a monotonicity promise no choice can keep - R1's own proof
  of unsatisfiability is what closes its own remedy. The venue stamp
  stays and is labeled instead. The finding is ACCEPTED; the remedies are
  not.
- **"Per-boat sweeper tasks" (R2.4's example remedy).** Rejected per
  adjudication ruling 3: N tasks contend the single engine lock and
  multiply the completion fan-out, to buy cadence granularity R1 correctly
  identifies as non-load-bearing for settlement correctness. R2's other
  suggestion in the same finding - per-boat next-due instants with one
  outer sleep to the earliest deadline - is what section 3.5 takes.
- **R1.7's implied "the sweeper item could be dropped as cosmetic".**
  Not taken. The severity WAS overstated in revision 1 and section 3.5
  now says so, but a `?speed=1` and a `?speed=100` boat sharing a cadence
  derived from the configured speed is still a wrong simulated interval,
  and the ruling requires the fix with a test.
- **R2's overall recommendation not to implement from the draft.**
  Superseded rather than rejected: the three blocking design questions it
  named are exactly what the adjudication ruled on, and this revision
  carries those rulings.
