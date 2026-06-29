# Technical implementation spec: coherent simulated clock for accelerated forward testing

Written against `reference/technical-implementation-spec.md` (the contract this
document must satisfy). Spawned from `docs/todo.md` section "Next / 1. Coherent
simulated clock for accelerated forward testing", whose upstream blocker
(nautilus issue 4304, the live-node clock-injection seam) has now landed as
`ClockFactory` + `LiveNodeBuilder::with_clock_factory` (PR 4331, commit
338b64b, refined by 66dcfd5). This spec is the design that section cites as
owed.

## Goal

Run the actual live trading path - real async, real reconnects, real WS frames,
real nautilus components - while compressing time, e.g. a simulated trading day
per wall-clock second. A real exchange forces 1x; mogwai's clock is fake, so it
has no lower bound. This is a run mode, not havoc: it perturbs nothing about the
content of the stream, only the rate at which the one shared simulated time axis
advances against the wall.

The defining requirement is COHERENCE: market-data `ts_event`, execution-event
and account-state timestamps, the adapter's `ts_init` stamps, and broadarrow's
nautilus node clock (its `timestamp_ns()` reads AND its timer firings) must all
sit on ONE simulated time axis that advances at the acceleration factor. The
rejected cheap version (data-only `speed`) leaves execution/account stamps and
the node's `LiveClock` on wall time while data races ahead - silently wrong.

### Everything temporal rides the simulated axis

Coherence is total, not selective. EVERY time quantity in the system is on the
one simulated axis - there is no "venue behavior is sim, transport is wall"
split. Concretely:

- Every absolute instant (a stamp, a timer alert, a window deadline) is a
  simulated instant, mapped to/from wall by `sim_ns` / `wall_ns`.
- Every relative duration that the system sleeps, intervals on, or measures
  elapsed against (replay pacing, `DelayAcks`, `GoDark`/`StallData` windows,
  client + baseline inbound latency, the server heartbeat cadence, the adapter's
  reconnect backoff / idle timeout / heartbeat interval, the HTTP request
  timeout, and the node clock's timer fires) is a SIMULATED duration, realized
  in wall time as `sim_duration / speed`.

So a 1-second reconnect backoff is one SIMULATED second (compressed to
`1s / speed` wall); a 30 ms baseline latency is 30 simulated ms; a 5-minute
blackout is 5 simulated minutes. The single mechanism is `SimClock::wall_span`
(brick 1): every wall sleep/interval/elapsed-threshold derived from a configured
(sim-intended) duration passes through it. At `speed == 1.0` it is the identity,
so today's behavior is byte-identical.

The only thing NOT on the sim axis is the irreducible floor: the tokio scheduler
and timer granularity (~1 ms) and the real local-IO round trip. Nothing can
sleep below that floor, so at high `speed` a configured sim duration that scales
below the floor is clamped to it. That floor, multiplied by `speed`, is the
coherence error budget and sets the maximum usable speed (see Cross-process
coherence and the brick 2 gate).

## The model: one affine clock

Define simulated time as an affine function of wall-clock time:

```
sim_ns(wall_ns) = sim_epoch_ns + (wall_ns - wall_anchor_ns) * speed
```

Three parameters define a run's timeline:

- `sim_epoch_ns` - the simulated instant the run starts from (the subscription
  `start_ts` and the generator anchor are this value).
- `wall_anchor_ns` - the wall-clock instant the run's clock was anchored.
- `speed` - the acceleration factor (`1.0` = honest live; `86_400.0` = a
  simulated day per wall second; `0.0` is NOT valid here - it means "unthrottled
  firehose" for the legacy data-only path and is rejected when a sim epoch is
  configured, see the stopping rule).

Every time consumer computes `sim_ns` from these same three numbers, so they
agree by construction. At `speed == 1.0` and `sim_epoch_ns == wall_anchor_ns`
the map is the identity and behavior is byte-identical to today.

Why affine and not data-driven: a data-driven clock (sim time = the `ts_event`
of the last emitted tick) makes timer firing intractable - a nautilus timer set
for sim instant T has no wall deadline until a future tick happens to reach T.
The affine map gives every sim instant a definite wall deadline
(`wall = wall_anchor + (T - sim_epoch) / speed`), which is exactly what timer
sleeps and deadline-paced replay both need. Coherence between the affine clock
and the data stream is then GUARANTEED by pacing the data to the clock (see
brick 2), not hoped for.

### The two-knob trap and the guard against it

`sim_epoch_ns != 0` is the switch that selects accelerated mode; `speed` is the
factor. The dangerous middle is `speed != 1.0` with `sim_epoch_ns == 0`: the
SimClock would be the identity (stamps stay on wall time) while the legacy
gap-based replay pacing still divides gaps by `speed` - i.e. EXACTLY the
data-only acceleration this whole spec rejects as silently wrong. The server
therefore rejects `sim_epoch_ns == 0 && speed != 1.0 && speed != 0.0` at startup
with a clear error (`0.0` stays the legacy firehose; `1.0` stays honest live).
Acceleration is only ever expressed by setting `sim_epoch_ns` to the run's start
instant together with `speed > 1.0`.

### Cross-process coherence and the speed ceiling

The server process and broadarrow's worker process (which hosts the adapter and
the nautilus node) are co-located on one host. Each computes `sim_ns` from its
own read of `CLOCK_REALTIME`. Same-host realtime skew between two processes is
sub-microsecond (same NTP-disciplined source), contributing `wall_skew * speed`
of sim-time skew.

Clock skew is NOT the dominant coherence error, though. Every stamp is computed
from `sim_ns(now)` at the instant of its event, so two stamps are coherent to
within the WALL latency between the two reads, multiplied by `speed`. That
latency is the irreducible floor described above - tokio scheduler wakeups, mpsc
hand-off, the WS/HTTP round trip, and per-message processing - none of which
compresses with `speed` (they are real wall costs). So:

```
realized sim-coherence error ~= (irreducible wall delivery latency) * speed
```

At `speed == 1e5`, 1 ms of un-compressible wall latency is 100 simulated
seconds. This is the explicit accepted bound, and it sets a MAXIMUM USABLE
SPEED: above the speed at which the floor-times-speed budget exceeds the
tolerance a test cares about, even a perfectly correct affine clock fails the
coherence gate. The brick 2 gate (assertion c) is read against this realized
budget, not against an impossible zero-latency ideal, and the accelerated smoke
instrument measures where the budget is blown so the ceiling is a number, not a
guess. A real venue's own feed carries network jitter of the same order, so a
nonzero budget is honest, not a defect.

## Survey of the ground

The current time surfaces, traced through their call sites:

- `mogwai_protocol::now_unix_nanos() -> u64` (`crates/mogwai-protocol/src/
  lib.rs`): the shared saturating wall reader. The single source of "now" today.
- `mogwai-server` (`crates/mogwai-server/src/main.rs`): `now_ns()` wraps the
  protocol reader. It feeds `engine.process(order_cmd, now_ns())` (the WS order
  arm and the `/orders` HTTP arm), the heartbeat `ts_event` (`spawn_heartbeat`),
  and the temporal divergence windows (`window_until_ns(now_ns(), ms)` for
  `GoDark`/`StallData`, and the `now_ns() <` guards in the writer). The writer
  also sleeps `delay_ms` wall for `DelayAcks`. Replay pacing is in
  `spawn_replay`: for each tick it sleeps `(tick.ts_event - prev) / speed` wall,
  clamped to `gap_cap_ms`. The server heartbeat ticks a wall
  `interval(server_heartbeat_ms)`. `speed`, `gap_cap_ms` and `server_heartbeat_
  ms` come from `mogwai.toml` via `Config`.
- `mogwai-server` history/polling path: `GET /trades` (`trades` handler) ignores
  `AppState` entirely (`State(_state)`) and calls `bounded_trades` ->
  `source::build_history_source`, which anchors a fresh `GeneratedSource` at the
  COMPILED-IN `ORIGIN_TS` (`crates/mogwai-server/src/source.rs`) and SEEKS
  forward to the requested `start` under a bounded `MAX_HISTORY_SEEK_TICKS` cap.
  This is a different model from the live path (which anchors the generator AT
  `start_ts`): history is a seek-from-fixed-origin pagination model whose cursor
  determinism the `/trades` replay-cursor test depends on. It is NOT on the
  accelerated axis and does not deadline-pace (it is request/response pull).
- `mogwai-engine` (`crates/mogwai-engine/src/lib.rs`): `process(&mut self, msg,
  ts: u64)` and `account_snapshot(ts)` already take the timestamp as a parameter
  and stamp every emitted `ts_event` from it. The engine reads no clock of its
  own. THIS IS A SEAM ALREADY: feeding it sim-time needs zero engine change.
- `mogwai-data` (`crates/mogwai-data/src/generated.rs`): `GeneratedSource::new`
  takes an explicit `start_ts` epoch; `clock_ns` starts there and advances by
  simulated inter-arrival durations. `next_tick` stamps `ts_event: self.
  clock_ns`. The data axis IS already simulated time anchored at `start_ts`. The
  `SessionModulator` keys off `utc_hour_dow(clock_ns)` - i.e. simulated time,
  not wall time - so non-stationarity already rides the sim axis and needs no
  change. (The architecture doc's phrase "non-stationary in wall-clock time" is
  imprecise; the code is keyed on `ts_event`.)
- `mogwai-adapter` free reads (`crates/mogwai-adapter/src/client.rs`): a free
  `now_unix_nanos() -> UnixNanos` wraps the protocol reader and is called at ~15
  sites to stamp `ts_init` on built events and `ts_event` on synthesized
  rejects. These appear in the data client's spawned request/subscribe tasks, in
  the free functions `handle_market_message` / `emit_trade` / `active_to_bar` /
  `aggregate_bars`, in `reject_for`, and in the exec client's inline-built
  `OrderAccepted` / `OrderCanceled` events and the `generate_*_reports`
  builders. Many are inside `tokio::spawn` / `get_runtime().spawn` closures and
  free functions with no `self`, so threading sim-time there means passing the
  (Copy) `SimClock` down each call chain, not a one-line substitution.
- `mogwai-adapter` EMITTER reads (the wall surface the free reader hides):
  `MogwaiExecutionClient::new` builds `ExecutionEventEmitter::new(get_atomic_
  clock_realtime(), ...)` (`client.rs`), and the emitter stamps `ts_init`
  internally from that `&'static AtomicTime` via `self.clock.get_time_ns()`
  (`research/nautilus_trader/crates/live/src/execution/emitter.rs`). The events
  built INSIDE the emitter - `emit_order_submitted`, `emit_order_rejected_event`,
  `emit_account_state` (and the modify-rejected emit) - therefore carry WALL
  `ts_init` even after every free `now_unix_nanos()` is moved to sim. Replacing
  the free reader alone leaves these four event classes off-axis; they need an
  explicit fix (brick 3).
- `mogwai-adapter` lifecycle / latency durations: `lifecycle.rs` arms reconnect
  backoff, idle-timeout detection and the heartbeat interval from `ConnHavoc`
  (ms knobs) using tokio sleeps/intervals and `Instant::now()` elapsed measures;
  `client.rs` sleeps client + `BASELINE_LATENCY` inbound latency
  (`delay_for` / `sleep_havoc_delay`) and bounds HTTP order entry by
  `request_timeout_secs`. Under "everything scales" these are SIMULATED
  durations realized as `sim.wall_span(...)`, not wall-bound (brick 3).
- nautilus seam (`research/nautilus_trader/crates/common/src/clock.rs`, depend
  via `../nautilus_trader`): the `Clock` trait owns `timestamp_ns()`,
  `set_time_alert_ns`, `set_timer_ns`, `cancel_timer`, the `timer_*` queries,
  default-handler and callback registration, and `reset`. `LiveClock`
  (`common/src/live/clock.rs`) reads the process-global
  `get_atomic_clock_realtime()`, holds a `CallbackRegistry` for named per-timer
  callbacks, and creates `LiveTimer`s; `LiveTimer::start`
  (`common/src/live/timer.rs`) spawns a tokio task that sleeps a REAL
  `next_time_ns - now_ns` duration and pushes `TimeEvent`s into the runner's
  `TimeEventSender` (obtained via `try_get_time_event_sender() ->
  Option<Arc<dyn TimeEventSender>>`). The sender drains in the runner's
  `select!` loop with no wall assumption, so an accelerated clock that pushes
  into the same sender is fully supported - the fire AXIS is ours to set.
- nautilus build site (`research/nautilus_trader/crates/live/src/node/
  builder.rs`): `LiveNodeBuilder::with_clock_factory<F>(factory: F) where F: Fn()
  -> Rc<RefCell<dyn Clock>> + 'static` takes a CLOSURE and wraps it in
  `ClockFactory::new` itself. It does NOT take a `ClockFactory`. The exported
  mogwai constructor must therefore hand back the closure, not a `ClockFactory`
  (brick 3).
- broadarrow build site (`research/broadarrow/crates/worker/src/bin/
  ba-worker.rs`): `LiveNode::builder(trader_id, environment(args.mode))?` then
  `.build()`; the MOGWAI exec path routes through
  `run_prep::venue::register_mogwai_forward`. `with_clock_factory` is the
  injection point (downstream, excluded).

Nothing else reads a clock on the order/data/account path.

## Target artifacts

### 1. `mogwai-protocol`: the `SimClock` value and its wire form

```rust
/// Affine wall-to-simulated time map. Cheap to copy; carries no clock of its
/// own - callers pass the wall read in, so it is pure and testable.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SimClock {
    pub sim_epoch_ns: u64,
    pub wall_anchor_ns: u64,
    pub speed: f64,
}

impl SimClock {
    /// The identity map: sim == wall. The default when no acceleration is
    /// configured, so every `sim_ns(now_unix_nanos())` equals today's `now`
    /// and every `wall_span(d)` equals `d`.
    #[must_use]
    pub fn identity() -> Self {
        Self { sim_epoch_ns: 0, wall_anchor_ns: 0, speed: 1.0 }
    }

    /// Map a wall-clock nanos reading onto the simulated axis. Saturating in
    /// both directions: a `wall_ns` before the anchor clamps to `sim_epoch_ns`
    /// (never underflows), and the scaled offset clamps to `u64::MAX` rather
    /// than wrapping. Mirrors the saturating contract of `now_unix_nanos`.
    ///
    /// PRECISION: the offset `(wall_ns - wall_anchor_ns)` is run-duration scale
    /// (small), so scaling it as `f64` is exact enough for runs up to ~years.
    /// `sim_epoch_ns` is epoch scale (~1.9e18), which EXCEEDS f64's 2^53 exact
    /// integer range, so it MUST be kept out of the float: compute the scaled
    /// offset in `f64` (or u128), then add `sim_epoch_ns` as a `u64`. Folding
    /// the epoch into the float quantizes every stamp to >=256 ns and is a
    /// coherence defect; the unit test pins nanosecond fidelity.
    #[must_use]
    pub fn sim_ns(&self, wall_ns: u64) -> u64 { /* saturating affine, epoch added as u64 */ }

    /// Inverse of `sim_ns`: the wall instant at which the clock reaches the
    /// simulated instant `sim_ns`. Used for absolute deadlines (timer alerts,
    /// deadline-paced replay). Saturating, same precision discipline.
    #[must_use]
    pub fn wall_ns(&self, sim_ns: u64) -> u64 { /* saturating inverse */ }

    /// Wall duration that elapses while the simulated clock advances
    /// `sim_dur_ns`: `sim_dur_ns / speed`, saturating. The ONE mechanism for
    /// relative durations - every wall sleep, tokio interval, and
    /// elapsed-threshold derived from a configured (sim-intended) duration goes
    /// through this, so all of them compress by `speed`. At identity returns
    /// `sim_dur_ns` unchanged. Result is clamped UP to the scheduler floor by
    /// the caller's own timer, not here (this stays a pure map).
    #[must_use]
    pub fn wall_span(&self, sim_dur_ns: u64) -> u64 { /* sim_dur_ns / speed */ }
}
```

`SimClock` is the identity-mapped default everywhere, so it is a pure addition:
no existing serialized message changes shape. It is serde-derived because the
adapter fetches it over `GET /clock` (brick 3).

Verification: `brokkr test -p mogwai-protocol sim_clock` - unit tests pinning
`identity()` round-trips wall unchanged and `wall_span` returns its argument;
`sim_ns`/`wall_ns` are inverse to within rounding AT EPOCH-SCALE inputs (the
precision test: a `sim_epoch_ns` near 1.9e18 with a small offset must preserve
nanosecond fidelity, which fails if the epoch enters an `f64`); `wall_span`
scales by `speed`; all three saturate (anchor-relative underflow ->
`sim_epoch_ns`; overflow -> `u64::MAX`); and a serde round-trip proves both ends
serialize identical bytes. This is the wire-protocol gate the contract names.

### 2. `mogwai-server`: feed sim-time to the engine, scale every duration, deadline-pace the data, expose `/clock`

Config (`Config` in `main.rs`, surfaced in `reference/config.md`): add

- `sim_epoch_ns: u64` (default `0`) - the simulated start instant. `0` means
  "honest live, epoch == wall anchor" (the SimClock stays identity).
- `speed` already exists; its meaning is unchanged at the wire level but is now
  the affine factor when `sim_epoch_ns != 0`.

Startup:

- Reject the two-knob trap: `sim_epoch_ns == 0 && speed != 1.0 && speed != 0.0`
  is a hard startup error (it would silently re-enable data-only acceleration -
  see the model). `sim_epoch_ns != 0 && speed <= 0.0` is also rejected (zero
  speed = no sim progress).
- If `sim_epoch_ns == 0`: `SimClock::identity()`. Every existing path is
  byte-identical (sim == wall, `wall_span` is the identity).
- Else: `SimClock { sim_epoch_ns, wall_anchor_ns: now_unix_nanos(), speed }`,
  captured ONCE at boot and stored in `AppState` behind an `Arc`. Capturing once
  is what makes the three independent `/clock` fetches (data client, exec
  client, clock factory) return identical params, so all consumers agree on the
  timeline. (`wall_anchor_ns` is the moment the server boots; the run is
  expected to connect promptly - the start-gap is coherent and documented, see
  the pacing note on the connect catch-up burst.)

Threading - move every stamp and every duration onto the sim axis via the stored
`state.sim`:

- Stamps -> `state.sim.sim_ns(now_unix_nanos())`: the two `engine.process(order_
  cmd, ...)` calls, the heartbeat `ts_event`, the `window_until_ns(...)`
  deadlines for `GoDark`/`StallData`, and the `dark_until_ns`/`stall_until_ns`
  guard comparisons in the writer. Because the windows are now sim-axis, a
  `GoDark { ms }` blackout lasts `ms` of SIMULATED time (the `window_until_ns`
  arithmetic is unchanged; it just operates on sim nanos), and the guard
  comparison reads sim-now.
- Durations -> `state.sim.wall_span(...)`: the writer's `DelayAcks` sleep
  (`from_millis(delay)` becomes `from_nanos(state.sim.wall_span(delay *
  1_000_000))`, so a `DelayAcks { ms }` is `ms` SIMULATED ms), and the server
  heartbeat cadence (`interval(server_heartbeat_ms)` becomes an interval of
  `wall_span(server_heartbeat_ms * 1_000_000)`, so the liveness cadence is on
  the sim axis like everything it interleaves with).
- Plumbing: `spawn_heartbeat` and the writer task currently capture only the
  divergence atomics / interval, not `AppState`; they must also carry the
  (Copy) `SimClock` so the stamps and `wall_span` calls above are reachable.
- Replay pacing (`spawn_replay`): when accelerated (`sim_epoch_ns != 0`),
  replace the per-gap `(gap_ns / speed)` sleep + `gap_cap_ms` clamp with
  DEADLINE PACING: before emitting a tick with `ts_event == T`, sleep until wall
  reaches `state.sim.wall_ns(T)`. This releases each tick exactly when the
  affine clock reaches its `ts_event`, so the realized data stream tracks the
  clock by construction - no drift, no cap needed. When NOT accelerated
  (`sim_epoch_ns == 0`), the existing gap-based paced sleep and `gap_cap_ms`
  clamp are kept UNCHANGED, so the honest 1x feed and every current smoke timing
  assertion are byte-identical. The two modes are selected by whether the
  SimClock is the identity, not by an env switch.
  - CONNECT CATCH-UP BURST: `wall_anchor_ns` is fixed at boot, so if the
    subscription arrives `delta` wall-seconds after boot, every tick whose
    `ts_event` falls in the first `delta * speed` sim-window already has a wall
    deadline in the past and is released immediately - a burst of up to
    `delta * speed` sim-time worth of ticks at connect. At `speed == 86_400` a
    one-second-late subscribe dumps ~one simulated day at once. This is honest
    (a real venue snapshots history on subscribe) and bounded by connect
    promptness; it is documented here so the burst is not read as a pacing bug.
- History/polling path: left UNCHANGED and explicitly NOT on the accelerated
  axis. `build_history_source` anchors the generator at `ORIGIN_TS` and seeks to
  the requested `start` under a bounded cap; relocating that anchor to
  `sim_epoch_ns` would break the `/trades` cursor determinism the pagination
  contract and its replay-cursor test depend on (a fresh anchor generates a
  different sequence, so page N+1 would not continue page N). For accelerated
  COHERENCE the live WS push path is the vehicle (its generator anchors at the
  subscription `start_ts == sim_epoch_ns`, coherent for any epoch). Setting
  `sim_epoch_ns == ORIGIN_TS` additionally lines the history path up (the
  seek-to-epoch is then immediate and on-axis); a `sim_epoch_ns` far from
  `ORIGIN_TS` makes accelerated `HttpPolling` exhaust the seek cap and return
  empty pages - so full accelerated `HttpPolling` coherence is a NAMED
  downstream item, not in this spec's bricks. The accelerated smoke exercises
  the WS path, which this spec makes coherent.
- New route `GET /clock` -> `Json(SimClock)` returning the run's stored clock.
  Honest default returns the identity, so a non-accelerated server answers
  truthfully and the adapter builds an identity clock (wall == sim).

`gap_cap_ms` under acceleration: it is a wall-clock comfort cap meaningful only
at 1x. Under deadline pacing it is structurally irrelevant (the deadline is
absolute), so it is ignored when accelerated. The server logs this at startup
when `sim_epoch_ns != 0 && gap_cap_ms != 0` so the operator is not surprised; it
is not an error.

Verification:
- `brokkr run -p mogwai-server -- serve` + `python3 scripts/smoke.py` - the
  existing honest-path suite, which MUST stay green unchanged (proves the
  identity-clock path is byte-identical). Same for `--heartbeat`.
- New `scripts/smoke.py --accelerated` mode driven by a committed
  `scripts/smoke-accelerated.toml` (`sim_epoch_ns` set to a fixed epoch at or
  near `ORIGIN_TS`, `speed` high, e.g. `3600.0`). This is a NEW INSTRUMENT and
  is itself a brick (the contract requires building the gate that prices the
  change). Launch it with the EXACT command
  `brokkr run -p mogwai-server -- serve --config scripts/smoke-accelerated.toml`
  then `python3 scripts/smoke.py --accelerated`. It asserts:
  (a) after submitting an order, the `OrderFilled.ts_event` and the following
  `AccountState.ts_event` are `>= sim_epoch_ns` and advance with the simulated
  axis (not wall) - i.e. they jump far past wall-now at high speed;
  (b) a windowed `Subscribe { start_ts: sim_epoch_ns }` delivers trades whose
  `ts_event >= sim_epoch_ns` and whose first few arrive within a wall budget
  consistent with `speed` (deadline pacing released them on the sim axis);
  (c) execution `ts_event` and market-data `ts_event` for the same wall instant
  agree to within the documented coherence budget - `(irreducible wall delivery
  latency) * speed`, NOT a zero-skew ideal (the coherence assertion - the whole
  point);
  (d) a `DelayAcks { ms }` armed mid-run delays the next execution event by `ms`
  of SIMULATED time (i.e. `~ ms / speed` wall), confirming the duration scaled.
  The proceed/close threshold: pick the test's tolerance, then read assertion
  (c) against `(measured wall delivery latency) * speed`. If (c) cannot hold
  within that budget at the chosen `speed`, the run is above the speed ceiling -
  lower `speed` or close the item as mispriced at that speed; the measurement,
  not the estimate, is the justification, and it also reports the ceiling.
- `brokkr check` - gremlins, clippy, and all server unit tests, including new
  unit tests that `GET /clock` returns the configured params, that an
  identity-configured server stamps engine events identically to `now_ns()`,
  that the two-knob trap is rejected at startup, and that `wall_span`-scaled
  `DelayAcks`/heartbeat durations divide by `speed`.

### 3. `mogwai-adapter`: `MogwaiClock`, the exported clock closure, sim `ts_init` everywhere, scaled durations

The adapter gains a sim clock, replaces ALL its wall reads (free AND emitter),
and scales every wall duration.

```rust
/// A nautilus `Clock` on mogwai's simulated axis. Reads sim-time via an affine
/// `SimClock`; fires timers by sleeping the SCALED wall delay and pushing
/// `TimeEvent`s into the runner's `TimeEventSender` (the same sink `LiveClock`
/// uses), so the runner's select loop drains them unmodified.
#[derive(Debug)]
pub struct MogwaiClock {
    sim: SimClock,
    timers: BTreeMap<Ustr, MogwaiTimer>,
    callbacks: CallbackRegistry,
    sender: Option<Arc<dyn TimeEventSender>>,
}
```

- `callbacks: CallbackRegistry` (the `pub` registry from nautilus `clock.rs`),
  NOT a single `Option<TimeEventCallback>`: `set_time_alert_ns` / `set_timer_ns`
  register per-NAME callbacks and `get_handler` looks them up, so a lone default
  handler would drop the named-callback path `LiveClock` provides. The struct
  mirrors `LiveClock`'s fields with `time` replaced by `sim`.
- `timestamp_ns()` -> `UnixNanos::from(self.sim.sim_ns(now_unix_nanos()))`. All
  the trait's `timestamp_*` helpers derive from it.
- `set_time_alert_ns` / `set_timer_ns`: a timer is requested at sim instant `T`
  (or interval `I` sim-ns). The fire task computes the WALL deadline
  `self.sim.wall_ns(T)` (and wall interval `self.sim.wall_span(I)`), sleeps that
  real duration via `tokio::time::interval_at`, and on each fire builds a
  `TimeEvent` stamped with the SIM instant `T` (not wall) and pushes it into
  `sender`. This is a near-copy of `LiveTimer::start` with two substitutions:
  the deadline is `sim.wall_ns(target)` instead of `target - wall_now`, and the
  event's `ts_event` is the sim instant. The TODO's two remaining "walls" (the
  realtime-singleton read axis and the wall-bound `LiveTimer`) are closed here.
  The tokio ~1 ms granularity is the floor: a sim interval whose `wall_span`
  falls below it coalesces, which is the per-timer face of the speed ceiling.
- `cancel_timer`/`cancel_timers`/`reset`/`timer_*`/default-handler and callback
  registration mirror `LiveClock` mechanically (a `BTreeMap<Ustr, MogwaiTimer>`
  of join handles + next-fire atomics, plus the `CallbackRegistry`).

Public constructor broadarrow calls. `LiveNodeBuilder::with_clock_factory` takes
a CLOSURE `Fn() -> Rc<RefCell<dyn Clock>> + 'static` (it wraps it in
`ClockFactory` itself), so the exported constructor fetches the clock ONCE
(async) and returns that closure - returning a `ClockFactory` would not compile
at the call site:

```rust
/// Fetch the server's run clock from `GET {http_base}/clock`, then return a
/// clock constructor closure for `LiveNodeBuilder::with_clock_factory`. The
/// fetched `SimClock` is the identity for a non-accelerated server, so this is
/// safe to call unconditionally - it degrades to wall-time clocks. The builder
/// wraps the closure in its own `ClockFactory`, whose memoized primary clock
/// (kernel + trader timestamps) and fresh per-component clocks then all share
/// the one fetched `SimClock`.
pub async fn mogwai_clock_factory(
    http_base: &str,
) -> anyhow::Result<impl Fn() -> Rc<RefCell<dyn Clock>> + 'static> {
    let sim: SimClock = fetch_clock(http_base).await?;
    Ok(move || Rc::new(RefCell::new(MogwaiClock::new(sim, try_get_time_event_sender()))))
}
```

The closure captures `sim` (Copy, `'static`) and re-reads
`try_get_time_event_sender()` per invocation, exactly as `LiveClock::default`
does. Downstream (excluded): `builder.with_clock_factory(mogwai_clock_factory(
http_base).await?)`.

Adapter `ts_init` - BOTH surfaces:
- The free `now_unix_nanos()` in `client.rs` is replaced by a clock handle
  carried on the data and exec clients. Both clients fetch the `SimClock` at
  `connect` (alongside the existing `GET /instruments` seeding) and store it; the
  ~15 stamping sites read `self.sim.sim_ns(mogwai_protocol::now_unix_nanos())`.
  Because many sites are free functions inside spawned tasks
  (`handle_market_message` -> `emit_trade` -> `active_to_bar`, `aggregate_bars`,
  `reject_for`, the `request_*` and `subscribe_*` spawn closures), the (Copy)
  `SimClock` is threaded down those call chains as a parameter. Before connect
  (and for any client that never fetches) the handle is `SimClock::identity()`,
  so pre-acceleration behavior is preserved and a non-accelerated run is
  byte-identical.
- The EMITTER-built events bypass the free reader: `emit_order_submitted`,
  `emit_order_rejected_event`, `emit_account_state` (and the modify-rejected
  emit) stamp `ts_init` from the emitter's `&'static AtomicTime`. With no
  upstream change permitted, the fix is to stop using those self-stamping
  helpers for `ts_init` and instead BUILD those events explicitly with sim
  `ts_init` - exactly as `handle_exec_message` already builds `OrderAccepted` /
  `OrderCanceled` (set `ts_init` via the now-sim free reader) and pushes them
  through `emitter.send_order_event` / the account-state send, which take a
  pre-built event and do no internal stamping. The emitter is retained for the
  send path; only its `ts_init`-self-stamping constructors are sidestepped.
- The subscription `start_ts` the data client sends is set to the fetched
  `sim_epoch_ns` so the server anchors the live generator on the same epoch the
  node clock runs on.

Adapter durations - all scaled by `self.sim.wall_span(...)`:
- Inbound latency: `HavocFilter::delay_for` composes `BASELINE_LATENCY` +
  armed `HavocLatency`; the resulting duration is realized via
  `sleep_havoc_delay` as `wall_span` of the configured (sim) nanos, so a 30 ms
  baseline is 30 SIMULATED ms.
- Lifecycle (`lifecycle.rs`): reconnect backoff, idle-timeout detection, and the
  heartbeat interval are armed from `ConnHavoc` ms knobs; each tokio sleep /
  interval / elapsed-threshold is built from `wall_span(knob * 1_000_000)`, and
  the `Instant::now()` idle measure compares elapsed wall against the scaled
  threshold. The `SimClock` is threaded into `WsConnectionConfig`. The HTTP
  `request_timeout_secs` likewise becomes a sim timeout; note it is the tightest
  contributor to the speed ceiling (it guards a REAL local-IO round trip whose
  wall cost does not compress, so at extreme speed the scaled timeout hits the
  IO floor first - documented, and the smoke ceiling measurement covers it).

Verification:
- `brokkr test -p mogwai-adapter mogwai_clock` - unit tests: `timestamp_ns`
  returns the affine map of a fixed injected wall read; a timer set for sim `T`
  fires after `~ self.sim.wall_span(T - sim_now)` wall (measured against
  `tokio::time` with a generous tolerance) and its `TimeEvent.ts_event == T`; an
  interval timer fires on the sim cadence; `cancel_timer` stops it; a per-name
  callback registered via `set_time_alert_ns` is found by `get_handler`; an
  identity clock reproduces wall behavior. The timer-firing tests use a stub
  `TimeEventSender` (the pattern in nautilus's own `live/timer.rs` tests) so no
  runner is needed.
- `brokkr test -p mogwai-adapter connect_clock` - tests
  `connect_fetches_clock_and_stamps_ts_init_on_sim_axis`: a client built against
  a stub server serving `GET /clock` stamps a built event's `ts_init` on the sim
  axis; built against a server with no/identity clock, stamps wall. And
  `emitter_built_events_stamp_ts_init_on_sim_axis`: an order-submitted /
  order-rejected / account-state event carries sim `ts_init` (the emitter
  bypass), not wall.
- `brokkr test -p mogwai-adapter scaled_durations` - an inbound latency and a
  reconnect backoff configured in ms are realized as `~ ms / speed` wall under
  an accelerated `SimClock`, and unchanged at identity.
- `brokkr check` - gremlins, clippy, full adapter suite (including the existing
  transport/havoc integration tests, which run at identity and must stay green).

## Keep/revert landing order

Each landing is one coherent intrusive change, green at its boundary:

1. **`mogwai-protocol` SimClock** (brick 1): `sim_ns` / `wall_ns` / `wall_span`,
   epoch-out-of-float precision. Pure addition; protocol tests green.
2. **`mogwai-server` sim-time threading + duration scaling + `/clock` + deadline
   pacing + startup guard + new accelerated smoke instrument** (brick 2). At
   default config byte-identical; `scripts/smoke.py` and `--heartbeat` green; new
   `--accelerated` mode green and is the measurement that prices the feature.
   Kept iff the coherence assertion (c) holds within the realized budget at the
   chosen speed; reverted as mispriced otherwise.
3. **`mogwai-adapter` MogwaiClock + exported closure + sim `ts_init` (free AND
   emitter) + scaled latency/lifecycle durations** (brick 3). Adapter suite
   green at identity; new clock, connect-clock, emitter, and scaled-duration
   tests green. The adapter now EXPORTS `mogwai_clock_factory` (a closure
   constructor) and stamps every event and sleeps every duration on the sim
   axis.

Ordering rationale: 2 depends on 1 (uses `SimClock`); 3 depends on 1 (the wire
type and `wall_span`) and on 2 (the `/clock` route to fetch). The suite is green
at every boundary because each lower layer defaults to identity until the layer
above opts in.

## Stopping rule and out of scope

- **broadarrow wiring is a separate, named downstream landing, not deferral.**
  This spec delivers everything mogwai owns: the coherent server, the
  `MogwaiClock`, and the exported `mogwai_clock_factory` closure. Calling
  `builder.with_clock_factory(mogwai_clock_factory(http_base).await?)` in
  `research/broadarrow`'s `ba-worker.rs` MOGWAI path (and threading the scenario
  that turns acceleration on) is a change to a different repository with its own
  todo and spec. It is named here so the API contract is fixed, and explicitly
  excluded from this spec's bricks and gates. The mogwai side is fully
  verifiable without it (server via `smoke.py --accelerated`, adapter via its
  unit suite).
- **No nautilus changes.** The seam is landed; the two remaining axes (read,
  fire) are implemented entirely inside `MogwaiClock`, and the emitter `ts_init`
  problem is solved adapter-side by building events with sim `ts_init` rather
  than via the emitter's `&'static AtomicTime` self-stamp. If a gap in the
  landed `ClockFactory`/`Clock`/`with_clock_factory` surface is discovered
  during brick 3, that is a new upstream item, not in-scope teardown here.
- **The two-knob trap is rejected, not supported.** `sim_epoch_ns == 0 && speed
  != 1.0 && speed != 0.0` is a hard startup error (it is the silently-wrong
  data-only mode). `speed == 0.0` (unthrottled firehose) stays a
  non-accelerated-only knob, incompatible with an affine clock; the server
  rejects `sim_epoch_ns != 0 && speed <= 0.0`. The firehose remains available
  for fast local iteration at `sim_epoch_ns == 0`.
- **Accelerated `HttpPolling` / history coherence is a named downstream item.**
  The history path is an `ORIGIN_TS`-anchored seek-from-fixed-origin pagination
  model whose cursor determinism the `/trades` contract depends on; relocating
  its anchor to an arbitrary `sim_epoch_ns` breaks pagination. This spec makes
  the live WS push path coherent for any epoch (the accelerated vehicle) and
  leaves the history path unchanged; setting `sim_epoch_ns == ORIGIN_TS` lines
  it up, while a distant epoch makes accelerated polling return empty pages.
  Full accelerated polling is out of this spec's bricks.
- **The `KrakenCsvSource` offline lineage and the analysis pipeline are
  untouched.** They never run in the server and carry no clock on the live path.
- **The speed ceiling is acknowledged, not engineered away.** Nothing sleeps
  below the tokio scheduler / timer granularity (~1 ms) or the real local-IO
  round trip. At high `speed`, configured sim durations that scale below that
  floor are clamped to it, and the coherence budget is `floor * speed`. The
  maximum usable speed is therefore a MEASURED property (the accelerated smoke
  reports it), not a target to push past in this spec.

## Documentation owed at landing (bundled with code, not alone)

- `reference/config.md`: document `sim_epoch_ns`, the affine `speed` meaning when
  set, the two-knob-trap rejection and the `speed <= 0.0` rejection, the
  `gap_cap_ms`-ignored-under-acceleration note, and the recommendation to set
  `sim_epoch_ns == ORIGIN_TS` for accelerated runs that also pull history.
- `reference/architecture.md`: a subsection under mogwai-server (clock) and
  mogwai-adapter (MogwaiClock) describing the single affine axis, that EVERY
  duration scales via `wall_span` (replay, divergence windows, `DelayAcks`,
  heartbeat, inbound latency, reconnect/idle/heartbeat lifecycle), deadline
  pacing and the connect catch-up burst, `/clock`, the emitter-`ts_init` bypass,
  and the cross-process budget + speed ceiling. Correct the "non-stationary in
  wall-clock time" phrasing to "in simulated time".
- `reference/havoc.md`: note that all temporal havoc durations - `DelayAcks`,
  `GoDark`, `StallData`, client latency, and the connection-lifecycle knobs -
  are on the simulated axis under acceleration (they scale by `speed`).
- `docs/todo.md`: check the spec checkbox in section 1; move the residual
  broadarrow-wiring line into a one-line downstream pointer, and add the
  accelerated-`HttpPolling` coherence item as a named downstream follow-up.
