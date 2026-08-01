# Implementation spec: one replay per tape, fanned out to N subscribers

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. The item it builds is the first open issue
in `docs/todo.md` ("Share one replay per `(symbol, data_origin)` across the
accounts subscribed to it, instead of one per subscription"), surfaced
2026-08-01 as the recorded sequel to the multi-account landing.

The built system this rewrites is described in `reference/architecture.md`
(the replay threads, the account registry, the checkpointed seek); the havoc
surface it must not break is `reference/havoc.md`.

## 1. What is being fixed

`spawn_replay` gives every SUBSCRIPTION its own OS thread carrying its own
`GeneratedSource`. `max_concurrent_replays` (default 1024) refuses subscribes
past that count with a per-entry `SubscriptionIssue::ReplayCapacity`.

A forward-validation batch is one account per strategy-symbol pair. A 200-slate
batch across ten symbol scopes is 2000 accounts and at least 2000
subscriptions - past the cap, so feeds are refused; and under the cap it is
still pure waste, because 200 strategies validating BTCUSDT subscribe the same
symbol, the same `data_origin`, the same FNV-derived seed and (almost always)
no regime. Those 200 generators are producing byte-identical streams in
parallel on 200 threads.

The end state: the number of OS threads doing synthesis is the number of
DISTINCT TAPES in flight - dozens - and a subscription is a cheap tokio task
attached to one. Per-subscriber observable output is byte-identical to today's
for any subscriber that keeps up with the feed. The two cases where it is not -
a subscriber slower than the shared ring, and a historical backfill that
structurally cannot catch up - are behavior changes this spec owns explicitly in
3.4 and 3.5 rather than promises it quietly breaks.

### The load-bearing constraint, resolved rather than discovered

`docs/todo.md` states it: sharing a tape couples the data-surface havoc, and
per-account divergence scoping just made that wrong. A `LiquidityDrought` armed
by one account must not perturb another account on the same symbol. The
perturbation is not a filter over ticks - `RegimeState` is consumed INSIDE the
walk (`arrival_thin` divides the ACD duration draw in
`GeneratedSource::next_duration_ns`; `vol_mult`/`realized_clamp_mult` scale and
clamp the GARCH return in `next_latent_mid`; a crossed `ReopenGap` advances
`clock_ns` and jumps `vol.mid` in `next_tick`), so a shared generator provably
cannot carry a per-account regime.

The resolution is that a regime does not need to be carried by a shared
generator: it is part of the tape's IDENTITY. A `GeneratedSource`'s entire
output is a pure function of `(scalars, seed, start_ts, session_profile,
regime)`. `scalars`/`session_profile` come from the symbol's
`InstrumentProfile`, `seed` from `seed_for(symbol)`, `start_ts` from
`data_origin`. So two subscriptions produce identical bytes exactly when their
`(symbol, data_origin, regime)` triples are equal, and the sharing key is that
triple - not `(symbol, data_origin)` as the TODO's first sentence proposes.

Consequences, stated plainly:

- A clean (regime-free) subscription shares with every other clean subscription
  on that symbol. This is the forward-validation batch case, which is the whole
  motivation: 200 accounts, one tape.
- A regime'd subscription gets its own tape, whose realization is byte-identical
  to the private generator it has today. Data havoc stays per-subscriber by
  construction, with no per-subscriber overlay to invent.
- Two subscriptions arming the IDENTICAL regime share a tape. That is correct,
  not a leak: identical `(symbol, data_origin, regime)` means each would have
  computed the same bytes privately.
- Havoc therefore costs a tape. A fleet arming 200 distinct `VolStorm`
  multipliers on one symbol still costs 200 tapes. That is the honest price of
  the regime being a generator input, it is bounded by the tape cap in
  section 4.6, and it is not the shape the item exists to fix.

## 2. Survey of the ground

### mogwai-server/src/ws.rs

- `handle_socket` owns `replays: HashMap<String, Replay>` and `generations:
  HashMap<String, u64>`, both per connection, keyed by symbol.
- `Replay { cancel: Arc<AtomicBool>, handle: JoinHandle<()>, last_sent_ts:
  Arc<AtomicU64> }`, with `NO_TICK_SENT = u64::MAX` as the never-sent sentinel.
- `Subscribe` per entry, in order: unknown-symbol pre-filter; generation
  high-water check against `generations`; `validate_regime_or_clean`;
  `strip_unfireable_reopen_gap`; `reconcile_entry_start_ts`;
  `lanes.reserve_promise()` (the single diagnostic ticket, reserved BEFORE the
  quiesce); `quiesce_and_resume_floor` of any in-flight stream for the symbol;
  `replay_permits.try_acquire_owned()`; `spawn_replay`. Per-entry outcomes
  coalesce into ONE `SubscriptionIssues` frame at the end.
- `spawn_replay` (OS thread): computes `sim_now`, `resume_seek_target(start_ts,
  resume_floor)`, calls `source::build_live_source`, treats a `None` first tick
  as `SeekBudgetExhausted`, takes one `(wall_anchor, instant_anchor)` pairing,
  then loops: pace (accelerated deadline pacing via `sim.wall_ns(ts)`, or
  identity gap pacing with `gap_cap_ms` and a chained `next_deadline_wall`),
  serialize, `send_cancellable` into the connection's `tx`, store
  `last_sent_ts`.
- `quiesce_replay` / `quiesce_and_resume_floor`: cancel, join on
  `spawn_blocking`, and read `last_sent_ts` ONLY after the join (the ordering is
  load-bearing - see the doc comment; a pre-join read can miss a tick already
  in the channel and produce duplicates on the replacement).
- `sleep_until_wall_cancellable` slices sleeps at `REPLAY_SLEEP_POLL` (20 ms) so
  cancel latency is bounded; `send_cancellable` retries `try_send` at
  `REPLAY_SEND_POLL` (5 ms) so a full connection channel parks the replay
  thread rather than dropping ticks.
- Disconnect: cancel every replay, then join every replay.

### mogwai-server/src/source.rs

- `checkpoint_store()`: process-global `Mutex<HashMap<(String, u64),
  Arc<Mutex<CheckpointIndex>>>>` over the CLEAN tape only, two-level locked.
  ALREADY shared across connections; regime'd realizations bypass it.
- `positioned_generator` uses the checkpoint index only when `regime.is_none()`
  and the seek target is past `data_origin`; otherwise a fresh from-origin
  generator inside `BoundedSeek { cap: MAX_HISTORY_SEEK_TICKS = 190_000 }`.
- `build_live_source(symbols, start_ts, regime, profiles, data_origin,
  sim_now)` and `build_history_source(symbol, start, regime, profiles,
  data_origin)`. `current_price` uses the same checkpointed walk for
  price-less market orders.
- `seek_throughput_measurement` reports ~1.9M ticks/sec synthesis;
  `checkpointed_seek_is_flat_in_k` pins the seek cost shape.

### mogwai-server/src/config.rs, main.rs

- `Config` is `#[serde(default, deny_unknown_fields)]`, so a removed key is
  already a hard load error with a serde message naming it.
- `max_concurrent_replays: usize` default 1024; `build_replay_permits` maps 0 to
  `Semaphore::MAX_PERMITS`. `AppState.replay_permits: Arc<Semaphore>`,
  `AppState.data_origin_ns`, `AppState.profiles: Arc<InstrumentProfiles>`.
- `data_origin_ns = sim.sim_ns(now_ns()) - cfg.backfill_horizon_ns`, computed
  ONCE at boot. It is therefore constant for a daemon's life, which is why it
  can sit in a tape key without a staleness story.
- Server tests touching the cap: `replay_permits_are_unchanged_by_account_scoping`,
  and the two tests at main.rs:2059 and main.rs:2640 that set
  `max_concurrent_replays = 1` plus a matching one-permit semaphore.

### mogwai-protocol

- `SubscriptionIssue` (messages.rs) with `is_refusal()` covering
  `UnknownSymbol | ReplayCapacity | StaleGeneration | SeekBudgetExhausted`.
- `MarketRegime` (havoc.rs) is `VolStorm { vol_mult: f64 }`, `LiquidityDrought
  { thin_factor: f64 }`, `SessionEdgeSpike { start_hour: u8, end_hour: u8,
  extra_vol_mult: f64 }`, `ReopenGap { at_ts: u64, halt_secs: u64, gap_frac:
  f64 }`. It derives `Serialize`/`Deserialize`/`Clone`/`PartialEq` but NOT
  `Eq`/`Hash` (it carries `f64`). `validate_market_regime` range-checks it.
- `SubscriptionRequest { symbol, generation, start_ts, regime }`.

### mogwai-adapter

- `client/data.rs` translates outcomes; the refusal arm is a catch-all
  `issue if issue.is_refusal() => ...`, so a NEW refusal variant needs no new
  match arm as long as `is_refusal` returns true for it. Only
  `StartBeforeOrigin` and `StartAfterSimNow` have bespoke arms.
- `client/exec.rs:2176` ignores `SubscriptionIssues` on the exec socket.

### Reconciliation against siblings

The two landed sibling specs (`exec-pump-admission`, `subscription-protocol`,
both in git history) own the priority lane, the promise pool, and the per-entry
generation/cursor. This spec consumes those structures unchanged: one promise
per subscription, one coalesced frame per subscribe, generations enforced
against the connection-lifetime high-water map. Neither sibling's survey states
anything that refutes the premise here; both describe per-subscription replay
threads as the status quo.

## 3. Open questions, settled

### 3.1 The sharing key includes the regime

Settled in section 1. `TapeKey { symbol: String, data_origin_ns: u64, regime:
RegimeKey }`.

`MarketRegime` cannot be a `HashMap` key (it carries `f64`, so no `Eq`/`Hash`).
`RegimeKey` is a canonical, hashable projection defined in section 4.1 that
maps each `f64` through `to_bits()`. Bit equality is the RIGHT comparison here:
two regimes share a tape only if they produce identical bytes, and the
generator's arithmetic is a pure function of the bit patterns. `NaN` never
reaches the key because `validate_market_regime` rejects non-finite fields, and
`strip_unfireable_reopen_gap` / `validate_regime_or_clean` run BEFORE the key is
built.

### 3.2 A subscriber attaches at the tape's cursor, and backfills the rest privately

A tape is one paced walk with one cursor. A subscriber joining it must be
handed everything from its own seek target onward, and that target can sit
behind the cursor (a resume floor, or an explicit historical `start_ts`) or
AHEAD of it (a resume floor inherited from a predecessor on a DIFFERENT tape -
any regime change on resubscribe - or a tape created after the predecessor had
already streamed past the new tape's first frame).

The cursor is therefore not a bare `u64` with a `u64::MAX` sentinel. It is a
three-state value (`Starting | Live(ts) | Poisoned`, encoded in section 4.1),
and each state has an explicit attach seam:

- `Starting`: the tape thread has committed no frame yet. There is nothing to
  backfill TOWARD, so phase 1 is skipped unconditionally and the delivery
  high-water is `target.saturating_sub(1)` (or 0 when `target` is `None`) -
  never the sentinel. This is the common case for the first subscriber to a new
  tape, and reading the sentinel as a timestamp is what would otherwise make a
  fresh subscriber either loop forever in phase 1 or drop every live frame.
- `Live(attach_ts)`: the seam below.
- `Poisoned`: the tape's first tick was `None`. Handled in 4.1/4.2 as
  `SeekBudgetExhausted`, spent from the fanout task's own promise.

With `Live(attach_ts)`, attach is two-phase:

1. **Backfill** `(target ..= attach_ts]` from a PRIVATE
   `source::build_history_source(symbol, Some(target), regime, ...)`, run only
   when `target` is `Some(t)` with `t <= attach_ts`. The clean case rides the
   existing shared `CheckpointIndex`, so POSITIONING is O(K), not O(span).
   `MAX_HISTORY_SEEK_TICKS` bounds only that positioning (`BoundedSeek::seek_to`
   caps the drain to the target); it does NOT bound the number of ticks emitted
   from `target` through `attach_ts`. That span is bounded only by how far
   behind the cursor the target sits, which is why the backfill pacing and its
   interaction with the bounded fanout ring get their own section (3.8).
2. **Live**, forwarding broadcast frames with `ts_event > high_water`, where

   `high_water = max(attach_ts_or_0, target.saturating_sub(1), last_backfilled_ts)`

   The `target - 1` term is load-bearing and is not implied by the other two:
   when `target > attach_ts` phase 1 is skipped, and filtering on `attach_ts`
   alone would deliver the frames in `(attach_ts, target)` that the predecessor
   already sent - a duplicated or regressed `ts_event` across exactly the
   E.5/E.6 resubscribe seam.

The two phases are the same deterministic walk, so the seam is exact: the
private backfill reproduces the tape's own bytes for that interval. No
inter-phase buffering is needed beyond the broadcast channel, which is already
accumulating the live frames while phase 1 runs.

A subscriber with no target behind the cursor (`start_ts: None`, no resume
floor - the common fresh subscribe) skips phase 1 entirely.

### 3.3 The cursor is stored BEFORE the frame is broadcast

The attach snapshot (`rx = tape.tx.subscribe(); attach_ts = tape.cursor.load()`)
is taken under the registry lock, but the tape thread runs free. Two orderings
are possible inside the tape thread for a frame at ts=100:

- store cursor, then send: an attacher can read `attach_ts = 100` and still
  receive frame 100 on `rx` (subscribe happened before the send). A DUPLICATE,
  which the `ts_event > attach_ts` filter drops.
- send, then store cursor: an attacher can read `attach_ts = 99` while frame 100
  went out before its `subscribe()`. A GAP, which nothing can recover, and gaps
  break the ascending-`ts_event` cursor contract the adapter's `PollCursor`
  relies on the same way duplicates do - worse, silently.

So the tape thread stores the cursor first and the fanout task filters. This is
not a micro-detail: it is the whole reason the attach is race-free without
holding the registry lock across a send.

### 3.4 A lagging subscriber loses its feed, loudly

Today a slow client parks its own private replay thread in `send_cancellable`
and nothing else is affected. With a shared tape that is no longer available:
parking the tape would stall every other subscriber on the symbol.

The broadcast channel is bounded (`fanout_depth`, section 4.6). A subscriber
that falls behind it gets `RecvError::Lagged(n)` and its subscription ENDS with
a new refusal outcome `SubscriptionIssue::FeedLagged { skipped: u64 }`, spent
from the diagnostic promise the subscribe already reserved. Ending the feed is
the honest answer: the alternative is a silent hole in a monotonic stream, and
the client can resubscribe with a higher generation and a cursor.

Rejected alternative: an unbounded broadcast queue per subscriber. It converts a
stalled client into unbounded server memory, and the venue exists to be driven
by clients that stall.

`FeedLagged` is a REAL behavior change, not merely a new name for an old
outcome, and section 1's "byte-identical per-subscriber output" promise is
scoped accordingly: it holds for subscribers that keep up. Two paths reach
`Lagged` that reached nothing today, and both are accepted deliberately:

- a slow-but-live subscriber, which today parks its private replay thread in
  `send_cancellable` and streams forever behind the clock,
- a long historical backfill, which today is a slow-but-correct feed (3.8).

### 3.5 Backfill under a bounded ring, and `speed = 0`

Phase 1 is paced and the ring keeps filling while it runs, so a subscriber whose
backfill is slower than the tape's live production is structurally guaranteed to
hit `Lagged` before phase 2 begins. Two configurations make that certain rather
than merely possible:

- **identity mode with `gap_cap_ms = 0`.** 3.8 records that this never catches
  up. Under a private replay that was slow-but-correct; under a shared ring it
  is a certain `FeedLagged`. This is a liveness change introduced HERE, and the
  spec owns it rather than calling it pre-existing.
- **`speed = 0`.** Today zero means "unthrottled firehose" - the identity branch
  is `else if speed > 0.0`, so a zero-speed replay never sleeps and is paced
  ONLY by its own connection's backpressure (`send_cancellable` on a full
  channel). `scripts/smoke.py` documents zero as the unthrottled mode. A tape
  has no such backpressure by construction (3.4), so a zero-speed tape would
  overwrite all `fanout_depth` slots as fast as it can synthesize and
  `FeedLagged` even subscribers that are reading as fast as they can.

The resolution, and it is a stated breaking change with its own gate:

- `speed = 0` remains supported and keeps its meaning ("as fast as the venue can
  produce"), but the throttle moves from the connection to the TAPE: when
  `speed == 0.0` the tape thread waits for ring headroom before committing the
  next frame, blocking on the SLOWEST attached subscriber the same way a private
  replay blocked on its own connection. Concretely the tape tracks
  `min(subscriber_cursor)` across live leases (each fanout task publishes the
  `ts_event` it last forwarded into an `AtomicU64` owned by its lease) and
  parks in `REPLAY_SEND_POLL`-sliced, cancel-aware sleeps while the lead exceeds
  `fanout_depth / 2`. A subscriber that is not merely slow but STOPPED still
  reaches `Lagged` once it stops advancing its cursor and the tape's park times
  out at `zero_speed_stall_ms` (section 4.4), so one dead client cannot stall a
  zero-speed tape forever - it is refused and the tape resumes.
- The backfill cases are not given a headroom mechanism. Phase 1 is per
  subscriber and the tape cannot wait on it without reintroducing the stall
  3.4 exists to prevent. Instead the fanout task detects the structurally
  doomed configuration up front: with `speed > 0`, identity mode,
  `gap_cap_ms == 0` and a `target` more than `fanout_depth` frames behind
  `attach_ts`, it refuses immediately with `FeedLagged { skipped: 0 }` rather
  than replaying for minutes and then being refused. The honest signal, spent
  from the same promise, at the moment it is knowable.

### 3.6 `max_concurrent_replays` is replaced, not reinterpreted

Its comment sizes it for "the intended single-broadarrow-node deployment", and
its unit was subscriptions because a subscription was a thread. After this
rewrite a subscription is a task and a THREAD is a tape, so silently
reinterpreting the same key would leave a 1024 that means something else.

The key is renamed `max_concurrent_tapes`, default 256, 0 still meaning
unbounded. `deny_unknown_fields` turns any config still carrying the old key
into a hard load failure naming it - the correct outcome, and the reason no
alias is added.

Per-connection subscription count is separately capped by
`max_subscriptions_per_connection` (default 256, matching
`MAX_SUBSCRIBE_SYMBOLS`), because the old cap was incidentally bounding that
too and nothing else now does. It is a real cap with a real enforcement point,
specified in full rather than merely named:

- **What it counts**: the number of entries LIVE in the connection's
  `subscriptions` map, i.e. one per `(connection, symbol)` currently streaming.
- **Where it is enforced**: in the `Subscribe` per-entry loop, AFTER the
  generation high-water check and AFTER the quiesce of any predecessor for that
  symbol. Placing it after the quiesce is what makes a RESUBSCRIBE free: the
  predecessor has already left the map, so a resubscribe at the cap replaces
  rather than being refused by its own predecessor.
- **The wire outcome**: refusal with `SubscriptionIssue::ReplayCapacity`,
  reusing the existing variant for the same reason `TapeCapacity` does - from
  the client's side the meaning is "the venue will not stream this, nothing
  further arrives for this entry". No new variant, no new adapter arm.
- **Zero**: means unbounded, matching `max_concurrent_tapes` and every other
  count knob in `Config`.
- Because the default equals `MAX_SUBSCRIBE_SYMBOLS`, one maximal subscribe
  frame lands exactly at the limit and the 257th distinct symbol on that
  connection is refused. That is intended: the frame limit and the connection
  limit are the same number so neither silently shadows the other.

### 3.7 Tape reaping is explicit, refcounted, and joined off the async worker

Relying on `broadcast::Sender::receiver_count()` to self-terminate races with an
attach in flight. The registry instead keeps an explicit refcount under the map
lock, and a `TapeLease` guard decrements it on drop. At zero the entry is
removed from the map, `cancel` is raised, and the `JoinHandle` is sent to a
single long-lived reaper task that joins threads on `spawn_blocking`. Joining
inside `Drop` would block a tokio worker; leaving the thread unjoined would
reproduce the detached-thread leak (E.7) the current code exists to avoid.

**Who owns the tape permit.** Asynchronous reaping and an exact cap are in
tension: if the permit is held until the thread has EXITED, a cap-of-one
resubscribe can be refused while its own predecessor is still stopping (the
deadlock-against-itself the current code sidesteps by acquiring after the
quiesce); if it is released at map removal, two tape threads briefly coexist
over one permit. The spec picks the second and states the consequence: the
`OwnedSemaphorePermit` lives in `Entry`, so it is released the moment the entry
leaves the map, and `max_concurrent_tapes` bounds tapes that are REACHABLE, not
threads that exist. The transient overshoot is at most the reaper's queue depth
and each overshooting thread is already cancelled, so it exits within one
`REPLAY_SLEEP_POLL` slice. The alternative - permit held to thread exit - trades
that bounded overshoot for a cap that refuses live work because of dead work,
which is the worse failure. `tape_capacity_refuses_a_new_tape_not_a_new_subscriber`
asserts the reachable-tape reading.

**Reap thrash on resubscribe, accepted explicitly.** Quiesce-then-attach means
the SOLE subscriber to a symbol drops the last lease, reaps the tape (and its
in-thread generator position), then immediately respawns it and re-seeks. It is
not a cost regression versus today - today the resubscribe also tears down and
respawns a thread, and the clean re-seek rides the shared `CheckpointIndex` - but
it does mean the resubscribe-heavy single-subscriber path gets none of the
sharing win. No grace period or linger timer is added: a linger would hold a
permit for a tape nobody reads, and the deployment this is sized for has many
subscribers per tape, which is exactly the case where the last lease does not
drop.

### 3.8 Live pacing moves to the tape; backfill pacing stays per subscriber

One pacer per tape, not per subscriber: that is where the CPU win beyond thread
count comes from. Backfill pacing stays in the fanout task because backfill is a
per-subscriber interval.

Backfill pacing rules are unchanged from `spawn_replay`'s explicit-`start_ts`
path: accelerated mode deadline-paces against `sim.wall_ns`, so already-elapsed
deadlines emit at full speed and catch-up terminates; identity mode gap-paces
with `gap_cap_ms`, which at the default 1000 ms against the tape's ~7 s mean
cadence catches up ~7x faster than the clock, and with `gap_cap_ms = 0` never
catches up. The never-catches-up case is a pre-existing property of the PACING,
but its consequence is not: under a private replay it produced a permanently
lagging feed, and under a shared bounded ring it produces a refusal. Section 3.5
owns that change and specifies the up-front refusal rather than leaving it to be
discovered minutes in.

Every sleep in the backfill pacer is cancel-aware on the same primitive as the
live loop (4.2), sliced at `REPLAY_SLEEP_POLL`, so a quiesce or disconnect during
a long backfill is bounded at 20 ms exactly as it is today.

## 4. Target artifacts

### 4.1 `mogwai-server/src/tape.rs` (new module)

```rust
/// The identity of a synthesized tape. Two subscriptions produce byte-identical
/// streams exactly when these are equal, because a `GeneratedSource`'s output is
/// a pure function of (scalars, seed, start_ts, session_profile, regime) and the
/// first three are derived from `symbol` and `data_origin_ns`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TapeKey {
    pub(crate) symbol: String,
    pub(crate) data_origin_ns: u64,
    pub(crate) regime: RegimeKey,
}

/// Hashable projection of `Option<MarketRegime>`. `f64` fields go through
/// `to_bits`, which is the correct comparison: the generator's arithmetic is a
/// pure function of the bit pattern, so bit-equal regimes yield identical bytes.
/// Non-finite values cannot reach here - `validate_market_regime` has already
/// rejected them - so `to_bits` has no NaN-payload ambiguity to worry about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RegimeKey {
    Clean,
    VolStorm { vol_mult: u64 },
    LiquidityDrought { thin_factor: u64 },
    SessionEdgeSpike { start_hour: u8, end_hour: u8, extra_vol_mult: u64 },
    ReopenGap { at_ts: u64, halt_secs: u64, gap_frac: u64 },
}

impl RegimeKey {
    pub(crate) fn from_regime(regime: Option<&MarketRegime>) -> Self;
}

/// One pre-serialized market-data frame, broadcast to every attached
/// subscriber. Serialization happens ONCE per tape tick rather than once per
/// subscriber - the second half of the win, after thread count. `Clone` is not
/// optional: `broadcast::Receiver::recv` clones out of the ring, which is
/// precisely why `payload` is an `Arc<str>` and not a `String`.
#[derive(Clone)]
pub(crate) struct TapeFrame {
    pub(crate) ts_event: u64,
    pub(crate) payload: Arc<str>,
}

/// What the tape thread has committed so far. A bare `u64` with a `u64::MAX`
/// sentinel is NOT sufficient: "no frame yet" and "poisoned" are distinct
/// attach seams (3.2), and reading either as a timestamp makes a fresh
/// subscriber either backfill forever or drop every live frame. Packed into one
/// `AtomicU64` (two reserved sentinels, everything else a real `ts_event`) so
/// the 3.3 store-before-send stays a single relaxed store.
pub(crate) enum CursorState {
    Starting,
    Live(u64),
    Poisoned,
}

pub(crate) struct Tape {
    key: TapeKey,
    tx: broadcast::Sender<TapeFrame>,
    /// Last frame the tape thread has COMMITTED to broadcasting, as a packed
    /// `CursorState`. Stored before the send (see 3.3), so an attacher may see
    /// a duplicate but never a gap. `Starting` until the first frame.
    cursor: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    /// Woken on cancel and on every commit, so a fanout task blocked in `recv`
    /// is interrupted promptly rather than at the next frame (4.2).
    wake: Arc<Notify>,
    /// Only consulted when `speed == 0.0` (3.5): the per-lease "last forwarded
    /// ts_event" cells the zero-speed tape throttles against. Registered at
    /// attach and removed at lease drop, under the registry lock.
    subscriber_cursors: Mutex<Vec<Arc<AtomicU64>>>,
}

pub(crate) struct TapeRegistry {
    inner: Mutex<HashMap<TapeKey, Entry>>,
    permits: Arc<Semaphore>,      // sized by `max_concurrent_tapes`
    fanout_depth: usize,
    reaper: mpsc::UnboundedSender<ReapRequest>,
}

struct Entry {
    tape: Arc<Tape>,
    refs: usize,
    handle: Option<JoinHandle<()>>,
    /// Released when the entry leaves the map, not when the thread exits: the
    /// cap counts REACHABLE tapes (3.7).
    _permit: OwnedSemaphorePermit,
}

/// A subscriber's hold on a tape. Dropping it decrements the refcount and, at
/// zero, cancels the tape and hands its thread to the reaper.
pub(crate) struct TapeLease {
    registry: Arc<TapeRegistry>,
    tape: Arc<Tape>,
    pub(crate) rx: broadcast::Receiver<TapeFrame>,
    /// Cursor snapshot taken under the registry lock at attach, as a state
    /// rather than a sentinel-bearing integer.
    pub(crate) attach: CursorState,
    /// This subscriber's own progress cell, published for the zero-speed
    /// throttle and deregistered on drop.
    pub(crate) progress: Arc<AtomicU64>,
}

impl TapeLease {
    /// Interrupts a `recv`/`reserve` await: cancel, or a fresh commit.
    pub(crate) fn wake(&self) -> &Notify;
}

impl TapeRegistry {
    pub(crate) fn new(cfg: &Config) -> (Arc<Self>, JoinHandle<()>);

    /// Attach to the tape for `key`, creating and starting it if absent.
    /// `Err(TapeCapacity)` when a NEW tape is needed and the permit pool is
    /// exhausted; attaching to an existing tape never consumes a permit and so
    /// can never be refused for capacity. A tape already `Poisoned` is returned
    /// as a lease whose `attach` is `Poisoned` - `attach` itself does NOT
    /// report `SeekBudgetExhausted`, because the seek runs on the tape thread
    /// and may still be in flight when `attach` returns (see below).
    pub(crate) fn attach(
        self: &Arc<Self>,
        key: TapeKey,
        spawn: TapeSpawn,
    ) -> Result<TapeLease, TapeCapacity>;

    pub(crate) fn live_tapes(&self) -> usize;
}

pub(crate) struct TapeSpawn {
    /// The full table, not one profile: the tape body calls
    /// `source::build_live_source`, which takes `&InstrumentProfiles`.
    pub(crate) profiles: Arc<InstrumentProfiles>,
    /// `symbol` and `data_origin_ns` are read back off the `TapeKey` passed
    /// alongside this, so they are deliberately absent here.
    pub(crate) regime: Option<MarketRegime>,
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) gap_cap_ms: u64,
    pub(crate) fanout_depth: usize,
    pub(crate) zero_speed_stall_ms: u64,
}
```

`live_tapes` is plain `pub(crate)`, not `#[cfg(test)]`: the section-6 tape tests
live in `main.rs`'s test module (5, L0), a different module from `tape.rs`, and
gating the accessor on `cfg(test)` couples the registry's API surface to test
configuration for no benefit. It is a one-line lock-and-count.

The tape thread body is `spawn_replay`'s loop with the connection-specific
parts removed: it seeks `build_live_source(&[symbol], None, regime, profiles,
data_origin, sim_now)` to `sim_now` at creation, paces with the identical
accelerated/identity branches and the identical `(wall_anchor, instant_anchor)`
pairing, serializes once, stores `cursor`, `tx.send(frame)`, and
`wake.notify_waiters()`. It has no `tx.try_send` backpressure loop and no
diagnostic promise: `broadcast::Sender::send` never blocks, and a tape with zero
receivers is already being reaped. `send` RETURNS `Err(SendError)` whenever the
receiver count is zero - which happens routinely in the window between the last
lease drop and the reap raising `cancel` - and the tape thread must IGNORE that
error and keep looping, not treat it as an exit condition. It exits on `cancel`
only. (Under `speed == 0.0` the loop additionally parks on ring headroom before
committing, per 3.5.)

**The dead seek needs a handshake, not a cursor read.** `attach` is synchronous
and returns before the tape thread has necessarily finished its seek, so it
cannot report `SeekBudgetExhausted` by inspecting a poisoned cursor. The
realizable flow:

- The tape thread's FIRST act is the seek. On `None` it stores `Poisoned`,
  calls `wake.notify_waiters()`, closes `tx` by dropping the sender, and exits.
- `attach` returns a lease in whatever state the cursor is in, including
  `Starting` while the seek is still running.
- The FANOUT task, not the registry, resolves it: before phase 1 it awaits
  either the first frame on `rx`, or `Closed`, or a cursor transition to
  `Poisoned`, raced against cancel. `Poisoned`-or-`Closed`-with-zero-frames
  spends the fanout task's OWN promise on `SeekBudgetExhausted` and ends. This
  is the only place that outcome is emitted, so a subscriber attaching during a
  seek in flight and a subscriber attaching after it failed get the identical
  answer.
- Because the seek is deterministic per key, every subscriber to that key gets
  the same answer - the same outcome each would have computed privately today.
- The entry is removed from the map when the tape poisons, so a later subscribe
  for the same key retries the seek rather than inheriting a dead entry.

### 4.2 `mogwai-server/src/ws.rs`

`Replay` is replaced by:

```rust
/// One subscription's live feed: a tokio task fanning one tape's frames into
/// this connection's outbound channel, after replaying whatever backfill the
/// subscriber's own seek target demands.
struct Subscription {
    /// The flag is retained for the cheap synchronous checks the loop bodies
    /// already do, but it CANNOT wake a parked task on its own.
    cancel: Arc<AtomicBool>,
    /// The wakeup half. Raising `cancel` is always followed by
    /// `cancel_wake.notify_waiters()`, and every await in the task is a
    /// `select!` against `cancel_wake.notified()`. Without this a fanout task
    /// parked in `recv` on an idle tape (mean cadence ~7 s in identity mode) or
    /// in `reserve` on a full connection channel would delay unsubscribe,
    /// resubscribe and disconnect by seconds or indefinitely, where today
    /// `sleep_until_wall_cancellable` bounds it at 20 ms. Disconnect teardown
    /// awaits every task, so the delay would compound across subscriptions.
    cancel_wake: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
    last_sent_ts: Arc<AtomicU64>,
}
```

`handle_socket` keeps `subscriptions: HashMap<String, Subscription>` in place of
`replays` and `generations` unchanged.

`quiesce_and_resume_floor` keeps its name, contract, and load-bearing ordering
(read `last_sent_ts` only after the task has ENDED), but joins a task instead of
an OS thread: raise `cancel`, then `.await` the `JoinHandle`. No
`spawn_blocking`, and no abort: the task must run to its own exit so the
`last_sent_ts` it may be mid-storing is final. Cancel latency is bounded because
EVERY await point in the task - the `recv`, the `tx.reserve()`, the backfill
pacing sleep, and the startup handshake - is a `select!` whose other arm is
`cancel_wake.notified()`, and the flag is checked after every wakeup. "Woken by
the next frame" is explicitly NOT the bound: an idle tape can be seconds away
from its next frame.

```rust
struct FanoutSpawn {
    symbol: String,
    generation: u64,
    lease: TapeLease,
    /// The seek target, from `resume_seek_target(start_ts, resume_floor)`.
    /// `None` means "start at the tape's attach point".
    target: Option<u64>,
    regime: Option<MarketRegime>,
    profiles: Arc<InstrumentProfiles>,
    data_origin: u64,
    sim: SimClock,
    speed: f64,
    gap_cap_ms: u64,
    tx: mpsc::Sender<Outbound>,
    lanes: ExecLanes,
    diag_ticket: Option<Ticket>,
    cancel: Arc<AtomicBool>,
    cancel_wake: Arc<Notify>,
    last_sent_ts: Arc<AtomicU64>,
    fanout_depth: usize,
}

fn spawn_fanout(spawn: FanoutSpawn) -> tokio::task::JoinHandle<()>;
```

Task body, in order:

0. **Resolve the tape's startup state** (4.1). If `lease.attach` is `Starting`,
   await the first frame / `Closed` / a `Poisoned` transition, raced against
   cancel. `Poisoned` or `Closed`-before-any-frame spends the promise on
   `SeekBudgetExhausted` and ends. Otherwise continue with a concrete
   `attach_ts`, or with "no frames yet" if the first frame arrives on `rx`
   (it is then delivered by phase 2 like any other, since the high-water is
   `target - 1` or 0, never a sentinel).
1. **Backfill**, only when `lease.attach` is `Live(attach_ts)` AND `target` is
   `Some(t)` with `t <= attach_ts`. Build `source::build_history_source(&symbol,
   Some(t), regime, &profiles, data_origin)`. A `None` source or a first tick
   that fails the bounded seek spends the promise on `SeekBudgetExhausted` and
   ends - identical to today. Before emitting anything, apply the 3.5 up-front
   refusal check (identity mode, `gap_cap_ms == 0`, span behind the cursor
   greater than `fanout_depth`): spend the promise on `FeedLagged { skipped: 0 }`
   and end. Otherwise emit ticks while `ts_event <= attach_ts`, paced by the same
   accelerated/identity rules `spawn_replay` used for an explicit `start_ts`,
   with every pacing sleep sliced at `REPLAY_SLEEP_POLL` inside a `select!`
   against `cancel_wake`, sending through the async `send_cancellable` twin
   below and storing `last_sent_ts`.
2. Loop on `lease.rx.recv()`, `select!`ed against `cancel_wake.notified()`:
   - `Ok(frame)` with `frame.ts_event <= high_water`: drop it (the
     3.3 duplicate window, the pre-`target` window, and any frame the backfill
     already covered).
   - `Ok(frame)`: send, store `last_sent_ts`, and store `ts_event` into
     `lease.progress` (the zero-speed throttle's input, 3.5).
   - `Err(Lagged(n))`: spend the promise on `FeedLagged { skipped: n }` and end.
   - `Err(Closed)`: the tape ended (poisoned seek, or process shutdown). If NO
     frame was ever delivered, this is the dead seek and the promise is spent on
     `SeekBudgetExhausted` here - nothing else has spent it. Otherwise log and
     end.
   - cancel notified: end.

   `high_water` is
   `max(attach_ts_or_0, target.saturating_sub(1), last_backfilled_ts)` - the
   full 3.2 formula. The `target - 1` term is not redundant: it is the only
   thing preventing a duplicate/regressed `ts_event` when `target > attach_ts`,
   which happens whenever a resubscribe's resume floor was inherited from a
   predecessor on a different tape (any regime change) or the tape was created
   after the predecessor had already streamed.

3. On exit, drop `lease` (refcount decrement, deregistration of `progress`,
   possible reap).

**One promise, two mutually exclusive outcomes.** A fanout task spends its
single diagnostic ticket on `SeekBudgetExhausted` (steps 0/1, before any frame
is delivered) or on `FeedLagged` (steps 1/2), never both: `SeekBudgetExhausted`
always ends the task, so no `FeedLagged` can follow it, and `FeedLagged` is only
reachable once phase 1 has committed to running. That exclusivity is what makes
one ticket sufficient and keeps `spend_diagnostic`'s second-spend `error!`
unreachable. It is stated here because it is exactly the invariant a future edit
that adds a third mid-stream diagnostic will break.

The `Subscribe` arm's ordering is unchanged except that
`replay_permits.try_acquire_owned()` disappears and `TapeRegistry::attach`
takes its place, AFTER the quiesce (same reason: a resubscribe of the same
symbol must release its predecessor's hold before the successor asks for one,
or a cap of one deadlocks against itself). `Err(TapeCapacity)` maps to the
existing `SubscriptionIssue::ReplayCapacity` refusal - the wire name stays,
because from the client's side the meaning is unchanged ("the venue's replay
pool is full, nothing streams"). The `max_subscriptions_per_connection` check
from 3.6 sits immediately after the quiesce and before the attach, refusing with
the same variant.

While that block is being rewritten, fix the stale comment it carries: the
current text says a capacity refusal emits a `ProtocolError`, but the code
already pushes a `SubscriptionOutcome`. The rewrite touches those exact lines,
so the comment is corrected in passing rather than left to mislead the next
reader.

`send_cancellable` gains an async twin `send_cancellable_async` used by the
fanout task: `tx.reserve()` raced against `cancel_wake.notified()`, so a full
connection channel applies backpressure to THIS subscriber only, never to the
tape, and a cancel while parked on a full channel returns promptly. The blocking
`send_cancellable` is deleted with `spawn_replay`.

**The outbound payload type changes with this, and that is part of the
landing.** `OutboundFrame.payload` is `String` today, and moving an
`Arc<str>` into it would allocate and copy a fresh `String` per subscriber -
which would give back most of the serialize-once win the tape exists for. So
`OutboundFrame.payload` becomes `Arc<str>`, and every producer changes with it:
`admission.rs` (the two construction sites), `ws.rs` (the heartbeat site and the
writer), and the `main.rs` test that builds one literally. The writer's
`Message::Text(frame.payload.into())` becomes a `Utf8Bytes` built from `&*payload`.
The honest performance claim, corrected from the earlier draft: fanout costs one
`Arc` clone per subscriber plus ONE memcpy of the payload at the socket write,
and saves a full `serde_json::to_string` per subscriber. The byte-charge
accounting is unaffected - it reads `payload.len()`, which `Arc<str>` provides
identically.

Disconnect teardown: raise every subscription's cancel AND notify its
`cancel_wake`, await every task, drop every lease.

### 4.3 `mogwai-protocol`

```rust
    /// REFUSED: the subscriber fell further behind the shared tape than the
    /// venue's fanout buffer, so `skipped` frames could not be delivered.
    /// Nothing further streams for this generation. A shared tape cannot be
    /// stalled by one slow subscriber the way a private replay could, so the
    /// venue ends the feed rather than leave a silent hole in a stream the
    /// client reads as strictly ascending.
    FeedLagged { skipped: u64 },
```

added to `SubscriptionIssue`, and to `is_refusal`'s match.

### 4.4 `mogwai-server/src/config.rs`

- `max_concurrent_replays` -> `max_concurrent_tapes`, default 256, doc comment
  rewritten to say the unit is DISTINCT TAPES and to name the regime as part of
  tape identity.
- new `max_subscriptions_per_connection: usize`, default 256, 0 meaning
  unbounded, semantics fully specified in 3.6.
- new `fanout_depth: usize`, default 4096, doc comment stating the slack in tape
  time (~8 simulated hours at the ~7 s mean cadence) and that exceeding it is
  `FeedLagged`, not a silent gap. Unlike the count caps, 0 is NOT "unbounded"
  here: `broadcast::channel(0)` panics, so `validate()` rejects
  `fanout_depth == 0` with a load error naming the key. That asymmetry is
  called out in the doc comment because every neighbouring knob reads 0 as
  unbounded.
- new `zero_speed_stall_ms: usize`, default 5000: how long a `speed = 0` tape
  parks waiting for ring headroom before giving up on the slowest subscriber and
  letting it lag (3.5). Only consulted when `speed == 0.0`.
- `build_replay_permits` -> `build_tape_permits`, same 0-means-unbounded rule.
- `AppState.replay_permits` is replaced by `AppState.tapes: Arc<TapeRegistry>`.

### 4.5 `mogwai-adapter`

No match arms change: the refusal catch-all covers `FeedLagged` once
`is_refusal` includes it. One test is added asserting the translation, because
"the catch-all covers it" is exactly the claim that silently rots.

### 4.6 Sizing

- `max_concurrent_tapes` 256: the deployment subscribes a handful of symbols
  clean, plus whatever distinct regimes QA arms. 256 is two orders of magnitude
  above the clean-tape need and still an OS-thread count a process trivially
  carries.
- `fanout_depth` 4096 frames: ~8 simulated hours of the default cadence; ~5 wall
  minutes at speed 100. A subscriber that cannot keep up with 4096 queued
  pre-serialized frames is not a subscriber whose feed is meaningful.
- `max_subscriptions_per_connection` 256: equals `MAX_SUBSCRIBE_SYMBOLS`, so one
  maximal subscribe frame is exactly at the limit.
- `zero_speed_stall_ms` 5000: long enough that a healthy in-process client is
  never the reason a firehose tape stalls, short enough that a dead client costs
  one stall and then gets refused.

### 4.7 Data flow, end to end

```
Subscribe entry (symbol, generation, start_ts, regime)
  -> validate / strip / reconcile          [unchanged]
  -> reserve promise                        [unchanged]
  -> quiesce predecessor -> resume_floor    [unchanged contract, task join]
  -> per-connection subscription cap        [new, 3.6]
  -> TapeKey { symbol, data_origin_ns, RegimeKey::from_regime(regime) }
  -> TapeRegistry::attach
       hit  -> refs += 1, rx = tx.subscribe(), attach = cursor.state()
       miss -> permit? -> spawn tape thread (seek to sim_now, pace, broadcast)
       full -> SubscriptionIssue::ReplayCapacity
  -> spawn_fanout
       phase 0: resolve Starting/Live/Poisoned -> SeekBudgetExhausted or on
       phase 1: private checkpointed backfill (target ..= attach_ts]
       phase 2: broadcast frames with ts_event > high_water
                high_water = max(attach_ts_or_0, target-1, last_backfilled)
  -> connection tx -> writer -> socket
```

## 5. Landing sequence

Two landings. The suite is green at both boundaries.

### L0 - the instrument that prices the item

The item is justified by an estimated volume win, so per the contract the
measurement lands first and carries an explicit proceed/close threshold.

**Where the test lives, settled.** `mogwai-server` has ONLY a `[[bin]]` target;
there is no `[lib]`, and the only file in `tests/` (`daemon.rs`) drives the
built binary as a subprocess. An integration test therefore cannot reach
`AppState`, the `state()` helper in `main.rs`'s `#[cfg(test)]` module, or
`TapeRegistry`. The choice - unit tests in the bin, or a new `[lib]` target - is
settled here rather than left to the implementer:

**The measurement, and the section-6 tape tests, are `#[cfg(test)]` unit tests
inside `main.rs`'s existing test module** - the same place the current
`max_concurrent_replays = 1` tests live, reusing the same `state()` helper and
the same in-process WS driving they already do. No `[lib]` target is added: a
lib split would re-export the entire server surface for the sake of a test, and
every existing server test already lives in `main.rs` for exactly that reason.
`TapeRegistry::live_tapes` is a plain `pub(crate)` method (4.1) reachable from
there.

The measurement is `#[tokio::test(flavor = "multi_thread")]`, named
`fanout_scaling_measurement`. It boots `state()`, opens `N`
in-process websocket sessions against one symbol with no regime, lets them
stream for a fixed wall window, and reports:

- OS thread count, read from `/proc/self/status` `Threads:`,
- process CPU time over the window, read from `/proc/self/stat` fields 14/15
  (utime/stime). No new dependency: the crate has no `libc` or `rustix` dep and
  a measurement is not a reason to add one. This is settled here, not left as
  an "if absent" branch.
- frames delivered per session, to prove the sessions were actually fed.

**The clients must not contaminate the thread metric.** Each session is an async
tokio task reading from an in-process channel - NOT a `tungstenite` blocking
client on its own thread, which would add N threads to the very number being
measured and make the L0 reading meaningless. The measurement subtracts the
runtime's own worker count (read once before any session is opened) so the
reported figure is threads ATTRIBUTABLE to streaming.

Reported for `N = 1` and `N = 128` on `BTCUSDT` at `speed = 100.0`. The test
PRINTS the table and asserts nothing about the ratio at L0 - it is an
instrument, not a gate, until L1 gives it a second reading.

**Proceed threshold**, judged on the L0 reading: at `N = 128`,
CPU-seconds-per-wall-second must exceed 4x the `N = 1` figure. That is the
load-bearing half. The thread-count reading (`> 128` attributable threads at
`N = 128`) is recorded as a sanity check on the harness rather than as a
decision criterion: it re-measures what the source already proves and cannot
realistically fail, so a failure there means the instrument is wrong, not the
premise. If the CPU ratio fails, the item is mispriced, L1 is never laid, and
the TODO entry is rewritten to record the measurement instead. This is the whole
point of L0: the "2000 threads producing byte-identical bytes" claim is a
reading of the source, not of a running process.

**Acceptance threshold** for L1, read from the same instrument: attributable
thread count at `N = 128` within 4 of the `N = 1` figure (one tape either way),
and CPU-seconds-per-wall-second within 1.5x of the `N = 1` figure.

The 1.5x figure is deliberately loose and the reason is stated so it is not
mistaken for a tight bound: the post-rewrite `N = 128` cost still includes 128
fanout tasks, 128 sets of websocket writes, and 128 in-process clients, none of
which the rewrite removes. The instrument therefore measures TOTAL process cost,
not isolated generator cost, and 1.5x is the envelope in which the removed
generators dominate the retained per-subscriber work. A reading between 1.5x and
the L0 baseline is a revert signal (section 7), not evidence the rewrite did
nothing - the diagnosis then requires the per-thread breakdown, which is out of
this instrument's scope.

### L1 - the tape registry, the fanout rewrite, and everything that moves with it

One coherent intrusive change, kept or reverted whole:

- `mogwai-protocol`: `FeedLagged`, `is_refusal`.
- `mogwai-server`: new `tape.rs`; `ws.rs` `spawn_replay`/`Replay`/
  `send_cancellable` deleted, `spawn_fanout`/`Subscription`/
  `send_cancellable_async` added, `Subscribe`/`Unsubscribe`/teardown rewired,
  the stale capacity-refusal comment corrected; `admission.rs`
  `OutboundFrame.payload` retyped to `Arc<str>` with its producers; `config.rs`
  key rename and three new knobs; `main.rs` `AppState` field swap, the literal
  `OutboundFrame` construction updated, and the three cap-touching tests
  rewritten in tape units.
- `mogwai-adapter`: the `FeedLagged` translation test.
- Root `mogwai.toml`: `max_concurrent_replays = 1024` becomes
  `max_concurrent_tapes = 256`, plus the three new knobs.
- `reference/config.md`, `reference/architecture.md`, `reference/havoc.md`
  updated in this same commit (markdown never lands alone).
- `docs/todo.md`: the item is REMOVED ENTIRELY, with the enduring facts moved
  into code comments (the 3.3 cursor-before-send ordering, the purity argument
  behind the tape key) and into `reference/architecture.md` (the fanout
  topology, the tape-identity rule, `FeedLagged`).

The protocol variant and the server that emits it land together because an
emitted variant the adapter cannot classify strands a subscription - the same
reason the exec-pump spec folded its adapter landing.

## 6. Gates

Per brick, the exact command.

### L0

- `brokkr test -p mogwai-server fanout_scaling_measurement`
  Reads the table. This is the proceed/close decision point.
- `brokkr check`
  Proves the instrument compiles clean and breaks nothing (it is additive).

### L1

Wire protocol (`FeedLagged` on both ends):

- `brokkr test -p mogwai-protocol subscription_issue`
  The existing serde round-trip module; extended with a `FeedLagged` case and an
  `is_refusal` assertion.
- `brokkr test -p mogwai-adapter subscription_issue`
  The adapter-side translation test added in 4.5.

Tape identity and sharing (behavior nothing today reaches - new tests, named):

- `brokkr test -p mogwai-server two_clean_subscribers_share_one_tape`
  Two sessions subscribe BTCUSDT clean; `TapeRegistry::live_tapes() == 1`; both
  receive the identical frame sequence over a fixed window.
- `brokkr test -p mogwai-server a_regimed_subscriber_gets_its_own_tape`
  One clean and one `LiquidityDrought` subscriber on BTCUSDT;
  `live_tapes() == 2`; the clean subscriber's frames are byte-identical to a
  solo clean run, i.e. the armed drought does not perturb it. This is the
  per-account-havoc-scoping invariant the TODO names as the substance of the
  item, and it is the test that must never be weakened.
- `brokkr test -p mogwai-server identical_regimes_share_one_tape`
  Two subscribers arming bit-equal `VolStorm { vol_mult: 2.0 }`;
  `live_tapes() == 1`.
- `brokkr test -p mogwai-server tape_is_reaped_when_its_last_subscriber_leaves`
  Subscribe, unsubscribe, and assert `live_tapes()` returns to 0 and the
  process thread count returns to its pre-subscribe value (the reaper actually
  joins).
- `brokkr test -p mogwai-server late_attacher_backfills_to_the_cursor`
  Subscriber A runs for a window; subscriber B attaches with an explicit
  `start_ts` behind A's first frame; B's delivered sequence is strictly
  ascending, contains no duplicate `ts_event`, and its overlap with A's frames
  is byte-identical.
- `brokkr test -p mogwai-server resubscribe_resumes_past_the_predecessors_last_tick`
  The E.5/E.6 seam, now across a shared tape: resubscribe the same symbol on the
  same connection and assert no `ts_event` repeats or regresses across the seam.
  Replaces whichever existing test pins this against private replays.
- `brokkr test -p mogwai-server a_lagging_subscriber_is_refused_with_feed_lagged`
  A session that never reads, with `fanout_depth` set small in the test's
  config, receives `FeedLagged` and no further market data, while a second
  healthy session on the same tape keeps receiving. This is the 3.4 contract and
  the one that proves a slow subscriber cannot stall the tape.
- `brokkr test -p mogwai-server tape_capacity_refuses_a_new_tape_not_a_new_subscriber`
  `max_concurrent_tapes = 1`: a second CLEAN subscriber on the same symbol
  succeeds; a subscriber on a second symbol is refused with `ReplayCapacity`.
  Replaces the two existing `max_concurrent_replays = 1` tests, whose
  assertions change by design (a second subscriber on the same symbol used to be
  refused and must now succeed - that inversion IS the item). Also asserts the
  3.7 reachable-tape reading: after the sole subscriber to symbol A unsubscribes,
  a subscribe to symbol B succeeds immediately, without waiting on the reaper.
- `brokkr test -p mogwai-server first_subscriber_to_a_new_tape_receives_frames`
  The `Starting` seam of 3.2, which a `u64::MAX` sentinel would have broken in
  the most common case there is: a single fresh subscriber, no regime, no
  `start_ts`, must receive ascending frames from the tape's first commit. Run
  also with an explicit `start_ts` so the phase-1 skip on `Starting` is covered.
- `brokkr test -p mogwai-server resume_floor_ahead_of_the_cursor_emits_no_duplicate`
  The `target > attach_ts` case: a resubscribe whose resume floor was inherited
  from a predecessor on a DIFFERENT tape (regime change) delivers nothing at or
  below `target - 1`.
- `brokkr test -p mogwai-server unsubscribe_returns_promptly_on_an_idle_tape`
  The 4.2 cancellation contract: with a tape whose next frame is seconds away,
  unsubscribe and full disconnect both complete well inside that gap. This is
  the test that fails if `cancel` is an `AtomicBool` with no `Notify`.
- `brokkr test -p mogwai-server a_dead_seek_reports_seek_budget_exhausted`
  A key whose seek cannot complete: BOTH a subscriber attaching while the seek
  is in flight and one attaching after the tape has poisoned receive
  `SeekBudgetExhausted`, and a later subscribe for the same key retries rather
  than inheriting the dead entry.
- `brokkr test -p mogwai-server per_connection_subscription_cap_refuses_the_overflow`
  `max_subscriptions_per_connection = 2`: a third distinct symbol on that
  connection is refused with `ReplayCapacity`, a RESUBSCRIBE of an already-held
  symbol succeeds, and a second connection is unaffected.
- `brokkr test -p mogwai-server zero_speed_tape_does_not_lag_a_reading_subscriber`
  `speed = 0`: a subscriber that reads continuously receives an unbroken
  ascending sequence and no `FeedLagged`, proving the 3.5 headroom throttle;
  a second, non-reading subscriber is refused after `zero_speed_stall_ms`
  without stalling the first.

Execution and engine semantics, plus the live control-plane path:

- `brokkr run mogwai -- serve` then `python3 scripts/smoke.py`
  Then `brokkr run mogwai -- stop`. The smoke drives the WS data path, the order
  path and `/control/divergence` end to end.
- `brokkr run mogwai -- serve -f --config scripts/smoke-accelerated.toml` then
  `python3 scripts/smoke.py --accelerated`
  Then `brokkr run mogwai -- stop`. The accelerated fixture is a SEPARATE
  config and a separate flag; the default invocation above does not exercise it.
  This is the gate for the deadline pacing that moved into the tape thread, so
  it is listed as its own command rather than implied by the first.

Whole-workspace regression:

- `brokkr check`
  Gremlins, clippy, and every test - including the engine unit tests, which this
  spec does not touch and which must be untouched by it.

Re-blessed expectations, stated up front so an implementer does not discover
them: the two `max_concurrent_replays = 1` tests invert as described;
`replay_permits_are_unchanged_by_account_scoping` is rewritten against
`AppState.tapes`; any test constructing `AppState` literally must swap the
`replay_permits` field. No golden generator stream changes - `mogwai-data` is
not touched, and `clean_regime_is_byte_identical` must still pass unmodified,
which is the cheapest proof that the tape rewrite did not perturb synthesis.

Also re-blessed: every construction of `OutboundFrame` in the test modules,
since `payload` becomes `Arc<str>`.

Throughput bound: this change trades a small per-frame cost (a broadcast hop, an
`Arc` clone per subscriber, and one memcpy of the payload at the socket write
where a `String` was previously moved into `Utf8Bytes`) for removing N-1
generators AND N-1 JSON serializations per tick. The accepted bound is the L0
acceptance threshold in section 5; a reading outside it is a revert, not a
tuning exercise.

## 7. Keep/revert

L1 is one commit and is reverted as one. The revert signal is any of:

- `fanout_scaling_measurement` outside the L1 acceptance thresholds,
- `a_regimed_subscriber_gets_its_own_tape` failing (havoc scoping broken - the
  item's own constraint),
- any ordering test showing a duplicate or regressed `ts_event`,
- `zero_speed_tape_does_not_lag_a_reading_subscriber` failing (the firehose mode
  the smoke fixtures document was silently broken),
- `scripts/smoke.py` failing on either fixture (default or `--accelerated`),
- `clean_regime_is_byte_identical` failing (synthesis perturbed).

There is no gated probe, no env-var switch, and no dual-path fallback to the
private replay: the old path is deleted in L1, which is what makes the revert
decision a `git revert` rather than a cleanup project.

## 8. Documentation that moves with the code

- `reference/architecture.md`: the "replay" section is rewritten as the tape
  topology - tape identity as `(symbol, data_origin, regime)`, the attach and
  backfill phases, the cursor-before-send ordering, reaping, and `FeedLagged`.
- `reference/config.md`: `max_concurrent_tapes`, `fanout_depth` (including its
  0-is-invalid asymmetry), `max_subscriptions_per_connection`,
  `zero_speed_stall_ms`, and the changed meaning of `speed = 0` (throttled by
  the slowest subscriber at the tape, not by one connection); the removed key
  named so an operator reading a `deny_unknown_fields` failure finds the answer.
- `reference/havoc.md`: the operator-facing statement that arming a data regime
  costs a tape and that a tape is shared only by bit-identical regimes.
- `docs/todo.md`: the item removed entirely.

## 9. Stopping rule

In scope: the live market-data fanout path and everything named in section 5.

Explicitly out of scope, and why:

- **The dead-feed watchdog** (`docs/bug-sweep.md` AD12). The TODO mentions it as
  the reason a capacity refusal is easy to miss, but it is a separate item with
  a separate design; this spec instead makes the refusal rarer and keeps its
  wire signal.
- **The arrival-drought decision** (the second `docs/todo.md` item). It is a
  fingerprint refit; nothing here changes generated bytes.
- **`/trades`, `/quotes` and `current_price`.** They already share the
  process-global `CheckpointIndex` and cost no threads. Untouched.
- **The checkpoint store's clean-only policy.** A regime'd tape still walks from
  origin on attach-time backfill. Caching regime'd realizations is a separate
  memory/complexity trade and the tape rewrite does not make it more urgent -
  it makes it less so, since a regime'd tape now walks once per key rather than
  once per subscription.
- **`mogwai-engine`, `mogwai-data`.** Neither is edited. The execution path,
  the divergence injection seam, and the generator are untouched.
- **The exec pump, the priority lane, the promise pool.** Consumed as-is from
  the sibling landings.

## References

- `reference/technical-implementation-spec.md` - the contract this document is
  written against.
- `docs/todo.md`, first open issue - the item this spec builds.
- `reference/architecture.md` - the account registry, replay pacing, and the
  checkpointed seek this rewrite sits on.
- `reference/havoc.md` - the data-surface divergence semantics section 1 must
  preserve per account.
