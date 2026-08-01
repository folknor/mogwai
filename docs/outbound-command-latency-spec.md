# Outbound per-command latency - implementation spec

Written against `reference/technical-implementation-spec.md`. Spawned from the
`docs/todo.md` entry "BUILD: outbound per-command latency, the venue acting late
rather than the client learning late" (RFC 4631 phase C).

## 1. The item

mogwai can make a client learn late and can make the venue's whole execution
channel late, but it cannot make the venue take a DIFFERENT amount of time to
ACT on a submit than on a cancel. That per-command split is what this spec
builds, and it is what turns "a cancel racing a fill" from a phrase into a
reproducible outcome: today a cancel sent one millisecond after a submit always
finds the order already accepted (and, at the default fill fraction, already
filled), because the venue acts on both instantaneously and in arrival order.

Two halves, both per command class:

- **ACT latency** - how long the venue takes to touch the book after the command
  reaches it. Applied where the engine processes the command, so the mutation
  itself happens late and other commands can get in front of it.
- **ACK latency** - how long the venue takes to report what it did, on top of
  the existing `DelayAcks` shift. Applied in the outbound path, per command
  class.

Command classes: `SubmitOrder`, `ModifyOrder`, `CancelOrder`. Queries
(`QueryOrders` / `QueryFills`) are deliberately excluded - see the stopping rule.

## 2. Survey of the ground

### 2.1 What exists inbound (adapter side, NOT the target)

`crates/mogwai-protocol/src/havoc.rs`:

- `HavocLatency { base_nanos, exec_event_nanos, fill_nanos, data_nanos }`, with
  `delay_for(EventKind)` composing base into the category extra.
- `BASELINE_LATENCY` - a 30 ms honest floor that armed havoc **adds to** rather
  than replaces. This is the precedent this spec follows for composition (RFC
  4631's open question 4).
- `MAX_LATENCY_NANOS` - 60 s per field.
- `validate_client_havoc` - the boundary guard.

This is an adapter-side consumer knob applied to the inbound stream after the
venue has already acted. Nothing in this spec touches it.

### 2.2 What exists outbound (server side, the ground being rebuilt)

- `control::Divergence::DelayAcks { ms }` (`mogwai-protocol/src/control.rs`) -
  one uniform hold on EVERY outbound execution event, bounded by
  `control::MAX_DIVERGENCE_MS` (1 h) via `havoc::validate_divergence`.
- `AccountSlot` (`mogwai-server/src/accounts.rs`) carries the three server-owned
  temporal windows as per-account atomics: `delay_ms`, `dark_until_ns`,
  `stall_until_ns`.
- `http::arm_divergence` intercepts the server-owned variants (stores into those
  atomics), routes `CancelOpenOrderSilently` to an immediate book action, and
  forwards only the engine-side single-shots to `engine.arm()`. A
  `debug_assert!` in the catch-all pins that split.
- `engine::divergence::arm` drops the server-owned variants outright so they
  cannot accumulate as dead queue entries.
- `ws::spawn_exec_pump` reads `slot.delay_ms` **per event at dequeue** (so
  re-arming and `ClearDivergences` reach frames already queued) and sleeps
  `sim.wall_duration(...)` to a deadline anchored at the frame's arrival
  instant, so a whole order-entry batch lands together `ms` late.
- `Divergence::ClearDivergences` zeroes the three atomics and, by documented
  contract, leaves engine-side single-shots armed.

### 2.3 The order-entry path, both surfaces

`http::process_order_cmd` is the single choke point for BOTH order-entry
surfaces (`POST /orders` and the `/ws` read loop). In order:

1. `ts = sim_now_ns(state.sim)`; `boundary_outcome` refuses malformed commands
   before any account or engine work.
2. `stamp_market_price` (may block ~100 ms on the checkpoint mutex), then `ts`
   is re-sampled.
3. price-less MARKET reject.
4. engine lock; tombstone recheck under the lock.
5. `lanes.reserve(&order_cmd, &engine.book_shape())` - the worst-case held-lane
   BYTE reservation, taken before the engine is allowed to mutate. `None` means
   `AdmissionRejected` and nothing mutates.
6. `engine.process(order_cmd, ts)`.

`ws::handle_socket`'s read loop awaits that call INLINE (`ws.rs`, the
`order_cmd =>` arm) and then hands the batch to
`lanes.submit_produced(reservation, Instant::now(), events)` with one arrival
instant for the whole batch. The loop is strictly serial: one command is fully
processed before the next frame is read. **This is the load-bearing fact for
this spec** - an act delay applied inline would shift every subsequent command
by the same amount and produce no race at all. The premise ("a cancel racing a
fill can genuinely lose") is only reachable if a delayed command stops occupying
the read loop.

`http::submit_order_http` builds `ExecLanes::detached()` fresh per request, so
its budgets start empty and it cannot refuse a single command for capacity. Its
response IS the ack, and `MogwaiExecutionClient::dispatch_order` already fires
each command onto a bare `get_runtime().spawn` with no cross-command sequencing
(the unordered-HTTP-dispatch item in `docs/todo.md`), so the HTTP surface has no
ordering guarantee to preserve.

### 2.4 Admission and lanes

`mogwai-server/src/admission.rs`: `ExecLanes` holds a `ByteBudget` (held lane,
permits are bytes) plus two `FrameBudget`s (priority queue depth, replay
promises). `AdmissionLimits { held_budget_bytes, lane_frames, promise_tickets }`
is built from `Config` by `config::build_admission_limits` and validated by
`config::validate_admission_limits`. `HeldFrame { arrived, frame }` is what the
pump consumes. Every producer on the read loop reserves first, refuses visibly,
and never awaits a full channel.

### 2.5 Adapter and clock

- `mogwai-adapter/src/config.rs::validate_window_deliverable` refuses a server
  window the chosen `TransportProfile` provably cannot deliver (`DelayAcks` and
  `GoDark` under `orders_over_http()`, `StallData` under `data_by_polling()`).
- `reference/clock.md` carries the axis table; every ms window is a sim-axis
  span converted by `SimClock::wall_duration`.
- Standing constraint from `docs/todo.md` "Notes / gotchas": broadarrow
  permanently declined a per-venue ceiling on their ~25 s `INFLIGHT_TIMEOUT`, so
  the band above it is deliberately unserved. This spec does not invest in it,
  but it also does not clamp it away - see 3.7.

## 3. Design decisions, resolved here

**3.1 Composition is additive, and the inbound convention is matched.** An armed
per-command ack latency ADDS to whatever `DelayAcks` is armed, exactly as
`ClientHavoc.latency` adds to `BASELINE_LATENCY` rather than replacing it. RFC
4631's open question 4 is answered "compose", because mogwai already answered it
that way for the inbound side and a second convention would be a trap. There is
no baseline act latency: the honest venue acts immediately, and the 30 ms floor
that models the network already lives inbound.

**3.2 Act latency detaches the command from the read loop; ack latency does
not.** A nonzero act delay spawns the command onto its own task so the read loop
keeps reading. That is the entire mechanism by which a later command can
overtake an earlier one. Ack latency is a pure time shift in the existing pump
and needs no such thing.

**3.3 A zero act delay keeps today's synchronous path, byte for byte.** The
spawn is taken only when the effective act delay for that command class is
nonzero. With nothing armed, the `/ws` order path is unchanged code and
unchanged ordering; every existing gate keeps its current meaning. This is also
what makes the keep/revert verdict readable: a regression on the default path
cannot be blamed on this feature's steady-state.

**3.4 Concurrent acts are unordered, and that is fidelity, not a defect.** Two
commands both carrying a nonzero act delay race for the account's engine mutex,
so their relative act order is unspecified even when their delays are equal.
This is the same position `docs/todo.md` already takes for HTTP order dispatch
(nautilus's own Binance futures client spawns every order command with no
cross-command sequencing), and sequencing here would make mogwai more orderly
than the venues it stands in for. A per-connection ordered queue is explicitly
REJECTED so it is not re-proposed.

**3.5 Outbound WIRE order stays FIFO per connection.** `spawn_exec_pump` is one
serial loop; a frame with a large ack delay holds later frames behind it. It is
not turned into a reordering buffer. The race being modeled is at the ACT, not
at the write, and the wire is where a consumer's own reordering havoc
(`ClientHavoc.reorder_prob`) already lives. Stated explicitly in
`reference/havoc.md` so nobody reads per-command ack latency as per-frame
reordering.

**3.5a The consequence of 3.5: the per-class ACK split is observable only when
the earlier-arriving command carries the SMALLER ack delay.** The pump dequeues
serially and sleeps to a per-frame deadline, so a submit holding a 300 ms ack
window keeps a cancel's 100 ms frames behind it on the same socket - the cancel
lands at ~300 ms, not ~100 ms. Head-of-line blocking is the honest behavior of a
single ordered venue socket, and it is deliberately not engineered away. Two
things follow and are load-bearing for the tests in section 5: a per-class ack
difference is measured either across two connections (independent pumps) or with
the slow class arriving SECOND, and the feature's value on one connection is
"the whole tail after a slow class is late", not "classes overtake each other on
the wire". Overtaking is what the ACT half is for.

**3.6 The knob is server-owned, per-account, store-not-merge.** It joins
`DelayAcks`/`GoDark`/`StallData` as atomics on `AccountSlot`, armed over
`/control/divergence` with the `x-mogwai-account` header, applied to every
session bound to that account. One arm REPLACES all six values (an omitted field
is `0`), matching the store-not-extend rule the other windows already follow.
`ClearDivergences` zeroes all six alongside the existing three.

**3.6a `ClearDivergences` does NOT lift an act delay already being served.**
`spawn_delayed_act` reads `act_ms` once and sleeps it; a `ClearDivergences`
posted mid-sleep zeroes the atomic but the detached command still sleeps out its
full window. This is deliberate and it is the OPPOSITE of the ack half, which is
read per event at dequeue and therefore IS liftable. The asymmetry is honest:
the venue has already begun acting on that command, and `ClearDivergences`
already documents the same shape for engine-side single-shots, which it leaves
armed. Clearing changes what the venue does to commands it has not started on
yet. Do not "fix" this by making the sleep interruptible with a `Notify`: an
in-flight act completing on its original schedule is what a real venue does, and
an interruptible act would make the clear a time machine. Stated in
`reference/havoc.md` in exactly these terms, and section 4.9 must not claim the
two windows behave identically with respect to lifting - they do not.

**3.7 Bounds.** Each of the six fields is bounded by `control::MAX_DIVERGENCE_MS`
(1 h), the same ceiling as every other ms window, enforced in
`havoc::validate_divergence`. Not `MAX_LATENCY_NANOS` (60 s): that ceiling is
argued from a per-event network delay on the inbound stream, while these are
venue windows on the same axis as `DelayAcks`. Values above broadarrow's ~25 s
`INFLIGHT_TIMEOUT` are legal and unserved, per the standing note.

**3.7a The act sleep sits BETWEEN the protocol boundary and the market-price
stamp, on both carriers.** `process_order_cmd`'s body today is: boundary
validation (step 1), `stamp_market_price` plus `ts` re-sample (step 2),
price-less MARKET reject (step 3), engine lock and reservation (step 4-5). The
sleep goes after step 1 and BEFORE step 2, and the WS detach is taken at the same
seam rather than around the whole function. Both halves of that are load-bearing:

- *Before the price stamp.* A delayed submit must meet the tape as it is when the
  venue acts, not as it was when the command arrived. Sleeping after step 2
  would stamp a price synthesized at arrival onto an event dated at act time -
  the exact dishonesty the `ts` re-sample exists to prevent - and would make the
  L4 todo note "a delayed submit meets a tape that has moved further" false. It
  would also give the two carriers different market instants for the same
  command, contradicting 4.10's carrier-agnostic claim.
- *After the boundary.* A malformed command is refused by the protocol boundary,
  which is not a venue act and must not be delayed by an act latency, must not
  consume a pending-act slot, and must be equally prompt on both carriers. On WS
  the read-loop arm therefore consults `boundary_error(&order_cmd)` (which takes
  no reservation and touches no account) before deciding to detach: a command
  that fails the boundary takes the unchanged synchronous path.

So `process_order_cmd` keeps one body and one order of operations, and the WS
detach is "everything from the sleep onward", not "everything". The two surfaces
cannot drift in what they produce because they run the same code from the same
point.

**3.8 A pending-act command is admission-controlled, on BOTH surfaces, and
survives the connection that sent it.** A pending act is outstanding work with a
lifetime of up to an hour. Three things follow.

*Bounded twice.* Each detached WS command takes a ticket from a new
per-connection `FrameBudget` before it is spawned AND a permit from a new
process-wide pending-act budget on `AppState`; no ticket or no permit means
`AdmissionRejected` on the priority lane and the engine never sees the command.
`POST /orders` under a nonzero act delay takes the process-wide permit too (it
has no per-connection lane to draw on), and answers `503` with the same
`AdmissionRejected` body when the permit is refused. 3.10 does NOT exempt HTTP
from the resource question: an armed hour-long delay plus a flood of `POST
/orders` would otherwise park unbounded axum tasks, each holding a socket, and
`mogwai-server` installs no `tower` concurrency or timeout layer that would
catch it.

*Survives disconnect.* A command past the boundary has been RECEIVED by the
venue. Dropping it on disconnect models request loss, which is a different
divergence and one mogwai already has. A pending act therefore completes its
mutation even if the client is gone; only the acknowledgment is discarded,
because there is nowhere to send it. The mutation is safe to complete: the task
holds an `Arc<AccountSlot>` and `process_order_cmd` rechecks the tombstone under
the engine lock, so a deleted account refuses rather than resurrects.

*The connection does not own it.* An earlier draft justified abandoning pending
acts by saying the session no longer holds a lease on the account. That is
false: `SessionLease` only increments and decrements `AccountSlot.sessions`, the
registry keeps owning the slot, and other sessions may be bound to the same
account and watching that book. Pending acts are spawned onto an app-scoped
`JoinSet` (or a bare `tokio::spawn` holding the permit), NOT the connection's,
so teardown neither waits for them nor kills them, and a second session on the
same account sees the mutation land.

**3.9 The held-BYTE reservation is still taken immediately before the engine
mutates, i.e. AFTER the act sleep.** The reservation must dominate the output of
the command against the book shape as it is when the engine acts; taking it at
arrival would size it against a stale shape. The visible consequence is that an
`AdmissionRejected` for a delayed command arrives after the act delay rather
than at arrival, which is the honest report: the refusal is a fact about the
venue at the instant it tried to act.

**3.10 The HTTP surface applies both halves inline.** `POST /orders` has no read
loop to free and its response is the ack, so act and ack delay simply make the
response late. No spawn, no per-connection ticket - but it does take the
process-wide pending-act permit while it sleeps (3.8), because the socket it
holds is the resource the WS ticket protects.

**3.11 Additive composition with `DelayAcks` is a WS-only statement, and HTTP
boundary refusals carry no ack delay at all.** 3.1's "adds to whatever
`DelayAcks` is armed" describes the exec pump, which is the only place
`DelayAcks` is applied; `POST /orders` has never honored it, and
`validate_window_deliverable` already refuses `DelayAcks` under
`orders_over_http()` for exactly that reason. The HTTP surface therefore sleeps
`ack_ms` ALONE, and that is the composition rule stated in `reference/havoc.md`:
per-command ack latency is carrier-agnostic, its composition with `DelayAcks` is
not. Extending `DelayAcks` to HTTP is explicitly out of scope (section 7) -
doing so would invert an adapter refusal that consumers depend on.

Relatedly, `submit_order_http` answers boundary refusals before it resolves an
account at all, so no slot exists and no armed window applies: those responses
are prompt and undelayed. This matches 3.7a - a boundary refusal is not a venue
act - and it is consistent on WS, where the boundary check also runs before the
detach decision. The one residual asymmetry is that a WS boundary refusal rides
the held lane and therefore still picks up the class's ACK delay, while the HTTP
one cannot, having no account. Accepted and documented rather than papered over:
resolving an account for a request that is being refused is what
`boundary_outcome` was split out to avoid.

## 4. Target artifacts

### 4.1 `mogwai-protocol/src/control.rs`

```rust
/// Per-command venue latency: how long the venue takes to ACT on each order
/// command, and how long it then takes to ACK what it did.
///
/// Every field is milliseconds on the sim axis, bounded by
/// `MAX_DIVERGENCE_MS`, and ADDS to any armed `DelayAcks` rather than
/// replacing it (the same composition rule `BASELINE_LATENCY` states for the
/// adapter's inbound latency). An arm REPLACES all six values; an omitted
/// field is zero.
CommandLatency {
    #[serde(default)] submit_act_ms: u64,
    #[serde(default)] modify_act_ms: u64,
    #[serde(default)] cancel_act_ms: u64,
    #[serde(default)] submit_ack_ms: u64,
    #[serde(default)] modify_ack_ms: u64,
    #[serde(default)] cancel_ack_ms: u64,
},
```

A struct variant with per-field `#[serde(default)]`, not a newtype over a
separate struct: the wire shape stays flat like every sibling variant
(`{"type":"CommandLatency","submit_act_ms":800}`), and a partial body arms one
knob without spelling out the other five.

`ClearDivergences`' doc comment gains the six fields to its list.

### 4.2 `mogwai-protocol/src/havoc.rs`

`validate_divergence` gains:

```rust
control::Divergence::CommandLatency { submit_act_ms, modify_act_ms, cancel_act_ms,
                                      submit_ack_ms, modify_ack_ms, cancel_ack_ms } => {
    if [*submit_act_ms, *modify_act_ms, *cancel_act_ms,
        *submit_ack_ms, *modify_ack_ms, *cancel_ack_ms]
        .iter().any(|ms| *ms > control::MAX_DIVERGENCE_MS)
    {
        return Err("CommandLatency fields must each be <= 3600000 (one hour)");
    }
    Ok(())
}
```

### 4.3 `mogwai-protocol/src/messages.rs`

```rust
/// Which order command produced an execution frame, so the outbound path can
/// apply that command class's ack latency. `None` on the wire-diagnostic and
/// query paths, which carry no per-command latency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandClass { Submit, Modify, Cancel }

impl CommandClass {
    /// The class of an order-entry command, or `None` for anything else
    /// (queries, subscribe/unsubscribe).
    #[must_use]
    pub fn of(cmd: &ClientMessage) -> Option<Self>;
}
```

Server-internal in effect, but it lives in `mogwai-protocol` next to
`ClientMessage` and `EventKind` for the same reason `EventKind` does: the
classification of a wire type belongs with the wire type, so the two ends cannot
disagree about it. Not serialized.

### 4.4 `mogwai-engine/src/divergence.rs`

`Divergence::CommandLatency { .. }` joins the server-owned arm that `arm()`
drops, and the `debug_assert!` list in `http::arm_divergence`'s catch-all. Those
two land in the SAME commit as the 4.8 interception arm: the assert says "this
variant must never reach here", so shipping it without the arm that keeps it
from reaching here panics debug builds and silently discards the control in
release ones. The
engine is otherwise untouched: this feature never changes what the venue does,
only when.

### 4.5 `mogwai-server/src/accounts.rs`

`AccountSlot` gains six `AtomicU64`s, zero-initialized at both construction
sites:

```rust
pub(crate) submit_act_ms: AtomicU64,
pub(crate) modify_act_ms: AtomicU64,
pub(crate) cancel_act_ms: AtomicU64,
pub(crate) submit_ack_ms: AtomicU64,
pub(crate) modify_ack_ms: AtomicU64,
pub(crate) cancel_ack_ms: AtomicU64,
```

plus two readers so no call site open-codes the class match:

```rust
impl AccountSlot {
    /// Armed ACT delay in sim-ms for `class`, `0` when nothing is armed.
    pub(crate) fn act_ms(&self, class: CommandClass) -> u64;
    /// Armed ACK delay in sim-ms for `class`, `0` when nothing is armed.
    pub(crate) fn ack_ms(&self, class: CommandClass) -> u64;
}
```

### 4.6 `mogwai-server/src/admission.rs`

```rust
/// Per-connection ceiling on order commands detached by an armed act latency
/// and not yet acted on. An armed hour-long act delay would otherwise let one
/// connection spawn unbounded pending tasks.
pub(crate) const PENDING_ACT_SLOTS: usize = 256;
```

- `AdmissionLimits` gains `pending_act_slots: usize` (default `PENDING_ACT_SLOTS`).
- `ExecLanes` gains `act_budget: FrameBudget` and
  `pub(crate) fn reserve_act(&self) -> Option<Ticket>`.
- `HeldFrame` gains `pub(crate) class: Option<CommandClass>`. Every
  construction and every destructuring of `HeldFrame` moves with it - notably
  `spawn_exec_pump`'s `while let Some(HeldFrame { arrived, frame })` pattern and
  `send_exec_protocol_error`, which is not order-entry and passes `None`.
- `submit_produced` gains a `class: Option<CommandClass>` parameter, stamped onto
  every frame of the batch. Existing callers that are not order-entry pass `None`.

```rust
/// Process-wide ceiling on commands sleeping out an armed act delay, across
/// every connection and both order-entry surfaces. The per-connection
/// `PENDING_ACT_SLOTS` bounds one client; this bounds the box, and it is what
/// stops an armed hour-long act delay plus a `POST /orders` flood from parking
/// unbounded axum tasks (there is no tower concurrency or timeout layer).
pub(crate) const GLOBAL_PENDING_ACT_SLOTS: usize = 4096;
```

`AppState` gains `pending_acts: Arc<tokio::sync::Semaphore>` sized from it, with
`try_acquire_owned` at both surfaces - the permit is held for the sleep plus the
engine work and released when the task ends, wherever it ends.

### 4.7 `mogwai-server/src/config.rs`

- `Config` gains `pending_command_acts: usize`, defaulting to
  `admission::PENDING_ACT_SLOTS`, and `global_pending_command_acts: usize`,
  defaulting to `admission::GLOBAL_PENDING_ACT_SLOTS`.
- `validate_admission_limits` gains `pending_command_acts >= 1` and
  `global_pending_command_acts >= pending_command_acts` (a zero budget would
  refuse every delayed command; a global ceiling below the per-connection one
  would make the per-connection budget unreachable and therefore a lie).
- `build_admission_limits` passes both through.

### 4.7a `mogwai.toml`

The committed config documents and pins every admission budget
(`exec_held_budget_bytes`, `admission_lane_frames`, `admission_promise_tickets`).
Both new keys are added there with the same comment standard: what they bound,
that a pending act is one COMMAND rather than a payload, and that lowering
`pending_command_acts` is how the smoke test reaches the refusal. Omitting them
would leave the committed artifact silently out of step with `Config`.

### 4.8 `mogwai-server/src/http.rs`

`arm_divergence` gains an interception arm ahead of the engine catch-all:

```rust
Divergence::CommandLatency { submit_act_ms, modify_act_ms, cancel_act_ms,
                             submit_ack_ms, modify_ack_ms, cancel_ack_ms } => {
    slot.submit_act_ms.store(submit_act_ms, Ordering::Relaxed);
    // ... the other five, all six stored unconditionally (store-not-merge)
}
```

`ClearDivergences` zeroes all six alongside the existing three.

`process_order_cmd` keeps its body order and gains one insertion between step 1
(the boundary refusal) and step 2 (`stamp_market_price`), per 3.7a:

```rust
/// Whether the venue's ACT delay for this command has already been served.
///
/// Not a bare bool: the two call sites mean opposite things and a silent
/// mix-up reintroduces the head-of-line stall the whole feature exists to
/// avoid. `Paid` is what `spawn_delayed_act` passes AND what the WS inline
/// (zero-delay) arm passes - the read loop must NEVER sleep here, even if a
/// re-arm lands between the loop's `act_ms` load and this one.
pub(crate) enum ActDelay {
    /// The caller already slept it off the read loop, or there was none.
    Paid,
    /// Sleep it here, inline: `POST /orders` has no read loop to free.
    PayHere,
}
```

With `ActDelay::PayHere` and a nonzero `act_ms` for the class,
`process_order_cmd` sleeps `sim.wall_duration(sim_duration_from_millis(act_ms))`
BEFORE `stamp_market_price`, so the existing step-2 `ts` re-sample already dates
the engine's events at the instant the venue acted rather than the instant the
command arrived, and the synthesized market price is the one at act time. No
second re-sample is needed and none is added - the sleep is placed so the
existing one covers it.

`submit_order_http` acquires the process-wide pending-act permit before that
call whenever the class's `act_ms` is nonzero, answering `503` with an
`AdmissionRejected` body when the semaphore is exhausted (3.8), and holds the
permit until the response is rendered. It then sleeps the class's `ack_ms` after
`process_order_cmd` returns, so the HTTP response is late by act + ack. Per
3.11 it does NOT add `DelayAcks`: that window is the WS pump's alone.

### 4.9 `mogwai-server/src/ws.rs`

The `order_cmd =>` arm splits in two:

```rust
order_cmd => {
    let class = CommandClass::of(&order_cmd);
    let act_ms = class.map_or(0, |c| lease.slot().act_ms(c));
    // A malformed command is refused by the protocol boundary, which is not
    // a venue act: it must not be delayed and must not burn a pending-act
    // slot (3.7a). `boundary_error` takes no reservation and no account, so
    // consulting it here is free and `process_order_cmd` still runs the real
    // boundary gate on whichever path is taken.
    if act_ms == 0 || boundary_error(&order_cmd).is_some() {
        // Unchanged synchronous path: process inline with `ActDelay::Paid`,
        // submit_produced with `class`, dispatch refusals on the priority lane.
    } else {
        let Some(ticket) = lanes.reserve_act() else {
            // AdmissionRejected { subject: admission_subject(&order_cmd),
            //   reason: "pending command-latency queue saturated" }
            // The engine never sees the command; nothing mutated.
        };
        let Ok(permit) = state.pending_acts.clone().try_acquire_owned() else {
            // Same refusal, reason "venue pending-act capacity exhausted".
        };
        tokio::spawn(spawn_delayed_act(DelayedAct { .. }));
    }
}
```

Note the inline arm passes `ActDelay::Paid`, never `PayHere`. Passing `PayHere`
there would let a re-arm racing between the loop's `act_ms` load and
`process_order_cmd`'s own load make the READ LOOP sleep inline, which is exactly
the 3.2/3.3 violation this design is built to avoid. The enum exists so that
mistake is visible at the call site.

```rust
/// One order command the venue is taking `act_ms` to act on, detached from the
/// read loop so later commands can reach the engine first - which is the whole
/// point of an act latency and cannot happen while the command occupies the
/// serial read loop. Everything after the sleep is exactly what the inline
/// path does, so the two cannot drift in what they produce, only in when.
struct DelayedAct {
    cmd: ClientMessage,
    class: CommandClass,
    act_ms: u64,
    state: AppState,
    slot: Arc<AccountSlot>,
    lanes: ExecLanes,
    /// Released when the task ends, wherever it ends.
    ticket: Ticket,
    /// Process-wide pending-act permit, likewise.
    permit: tokio::sync::OwnedSemaphorePermit,
}

async fn spawn_delayed_act(act: DelayedAct);
```

Body: sleep `sim.wall_duration(sim_duration_from_millis(act_ms))`, call
`process_order_cmd(cmd, &state, &slot, &lanes, ActDelay::Paid)`, then run the
SAME outcome dispatch as the inline arm, with two differences forced by being off
the loop: a `LaneClosed` returns (the connection is already tearing down) and a
`CloseSpec` goes to `lanes.send_close(close)` directly, because the writer owns
the sink and no task can break another task's loop.

Per 3.8 the task is spawned with a bare `tokio::spawn`, NOT onto a
connection-owned `JoinSet`, and teardown neither awaits nor aborts it:

```rust
// A pending act OUTLIVES the socket that sent it. The command is past the
// protocol boundary, so the venue has received it; killing it on disconnect
// would model request loss, which is a separate divergence. The mutation
// therefore lands and only the acknowledgment is dropped - `lanes` is already
// closed, so the outcome dispatch takes its LaneClosed path and returns.
// Correctness after the client is gone rests on two existing facts: the task
// holds an Arc<AccountSlot> so the slot cannot be freed under it, and
// `process_order_cmd` rechecks the tombstone under the engine lock so a
// deleted account refuses rather than resurrects. Other sessions may be bound
// to the same account and MUST see this mutation - a SessionLease is only a
// counter on `AccountSlot.sessions`, never ownership of the account.
// Teardown does not wait: the task is detached and bounded by the process-wide
// pending-act semaphore, not by the connection's lifetime.
```

`Engine::process` is synchronous, so there is no await point inside the
mutation and a pending act is all-or-nothing with respect to the book: it either
has not taken the engine lock yet or it runs to completion.

`spawn_exec_pump` composes the two windows per event at dequeue:

```rust
let delay = slot.delay_ms.load(Ordering::Relaxed)
    .saturating_add(class.map_or(0, |c| slot.ack_ms(c)));
```

Read at DEQUEUE, not baked in at production, exactly as `delay_ms` already is -
so re-arming and `ClearDivergences` reach frames already queued, and the ack
window behaves exactly as the `DelayAcks` window it composes with. This is a
statement about the ACK half only: per 3.6a an act delay already being served is
NOT liftable, and the doc plan in section 5 must say so rather than claim
symmetry between the two halves.

### 4.10 `mogwai-adapter/src/config.rs`

`validate_window_deliverable` gains a documented pass-through: `CommandLatency`
is carrier-agnostic and is NOT refused under any profile. Both halves fire on
both carriers - the act half is applied where the engine processes the command
(shared by `/ws` and `POST /orders`), and the ack half either shifts the WS
frame or delays the HTTP response. This is stated in the function's doc comment
next to the three refusals, because the default there is "windows are WS-only"
and a reader must not generalize it.

The same doc comment states the 3.11 caveat, because it is the natural place a
reader will look for it: `CommandLatency` is deliverable under every profile,
but its ADDITIVE composition with `DelayAcks` only happens on the WS pump - the
neighbouring arm refuses `DelayAcks` under `orders_over_http()`, so under an
HTTP-orders profile a client gets `ack_ms` and nothing else. Leaving that
unsaid would let a reader infer that arming both under `HttpOrders` yields the
sum.

## 5. Landings

TWO landings, each independently green AND each coherent on its own terms.

An earlier draft split the server work three ways (wire type, then ack half,
then act half) and neither of the first two was a landing this repo's spec
contract permits:

- A wire-type-only landing adds `CommandLatency` to `arm_divergence`'s catch-all
  `debug_assert!` list WITHOUT the interception arm that keeps it out of the
  catch-all, so posting the control panics a debug build and is silently
  discarded by a release build. A control that is accepted and does nothing is
  not "inert", it is a lie with a `202`.
- An ack-only landing accepts all six fields while three of them are stored and
  never read, so a successful control response describes venue behavior that
  does not exist. "The ack half alone is a coherent venue" is true only if the
  act fields are REJECTED until they work, and a wire type whose accepted field
  set changes between landings is worse than one landing.

So the server work lands once, whole. Within it the order of work is still wire
type, then ack half, then act half - but there is one commit and one gate, and
nothing is observable to a client until all of it is.

### L1 - the control, both halves, both surfaces

`control::Divergence::CommandLatency`, `CommandClass` and `CommandClass::of`,
the `validate_divergence` arm, the `engine::arm` drop, the `arm_divergence`
interception arm AND its addition to the `debug_assert!` list (together, so the
catch-all is never reachable), the six `AccountSlot` atomics with
`act_ms`/`ack_ms`, `ClearDivergences` zeroing all six, `HeldFrame.class` and
every construction and destructuring that moves with it, `submit_produced`'s new
parameter, the pump's composition, `PENDING_ACT_SLOTS` and
`GLOBAL_PENDING_ACT_SLOTS` with `AdmissionLimits`, `ExecLanes::reserve_act`,
`AppState.pending_acts`, both `Config` keys with their validation and their
`mogwai.toml` entries, `ActDelay` and `process_order_cmd`'s new parameter with
the sleep placed per 3.7a, the HTTP permit and ack sleep, `DelayedAct` /
`spawn_delayed_act`, and the detached-task teardown contract of 3.8.

**The instrument comes first within this landing**, because the premise of the
whole item is a race no existing harness can observe. No test today can express
"a cancel got there first", so building that observation is a brick, and it is
laid before the behavior it gates is trusted.

New `scripts/smoke.py` step, `--command-latency`, with a committed
`scripts/smoke-command-latency.toml` alongside it (every existing smoke mode
ships one, and this one needs `pending_command_acts` lowered to reach the
refusal). `main()` is an exact-match `if`/`elif` chain plus a usage string -
BOTH are extended, along with the mode list in the file's header comment.

Over a live socket, and looped FIVE times inside the step rather than left to
the operator to repeat:

1. Arm `CommandLatency { submit_act_ms: 800 }`.
2. Submit `O-RACE`, then immediately send `CancelOrder { O-RACE }` on the same
   socket.
3. Assert the `OrderCancelRejected` for `O-RACE` arrives FIRST and names an
   unknown order (no `venue_order_id` - the field is
   `Option<VenueOrderId>` with exactly the "absent when the order id is unknown"
   contract), and only then the submit's `OrderAccepted`. This is the outcome
   that is impossible today: the cancel genuinely lost the race by winning it.
4. Arm `CommandLatency {}` (all zero), re-submit and re-cancel, assert the
   ordinary ordering returns - the cancel now finds the order. This is the
   disarm proof, in the shape the `DelayAcks` step already uses.

Proceed/close threshold for the reading: step 3 must produce the reversed order
on EVERY one of the five iterations at an 800 ms act delay against a 0 ms
cancel. If it does not - if the read loop is still serializing, or the spawn is
not actually detaching - the landing is REVERTED rather than tuned, because a
race that only sometimes happens at a 800:0 ratio is not a knob anyone can build
a scenario on. The loop lives in the script so the threshold is one
copy-pasteable command, not an instruction to run a gate five times.

Unit tests added in `mogwai-server/src/main.rs` (where the existing `DelayAcks`
pump tests live):

- `command_ack_latency_adds_to_delay_acks` - arm `DelayAcks { ms: 100 }` and
  `CommandLatency { submit_ack_ms: 200 }`; assert the submit's batch lands no
  earlier than ~300 ms. Per 3.5a the per-class SPLIT is measured separately and
  never behind a slower frame on the same socket: a second connection (its own
  pump) sends the cancel with `cancel_ack_ms: 0` and sees ~100 ms. Asserting a
  ~100 ms cancel behind a 300 ms submit on ONE socket cannot pass - the pump is
  serial and the cancel's frames sit behind the submit's in `exec_rx`.
- `ack_latency_is_per_class_on_one_socket` - the same-socket form that FIFO does
  permit: the fast class first. Cancel (0 ms) then submit (200 ms), assert the
  cancel's frames are not held by the submit's window.
- `clear_divergences_lifts_a_queued_ack_but_not_a_pending_act` - arm all six,
  detach a submit, post `ClearDivergences` mid-sleep; assert all six atomics are
  zero, a frame already queued is released promptly, AND the in-flight act still
  lands on its original schedule. Pins 3.6a in both directions so the
  `reference/havoc.md` claim cannot drift.
- `arming_command_latency_replaces_every_field` - arm all six nonzero, then arm
  `{ submit_act_ms: 5 }` alone, assert the other five are zero (store-not-merge).
- `command_latency_never_reaches_the_engine_queue` - the `debug_assert!` path,
  posted over the real control plane.
- `a_delayed_submit_does_not_stall_the_read_loop` - with `submit_act_ms` armed,
  a `QueryOrders` sent immediately after the submit is answered before the
  submit's events arrive. This is the head-of-line claim, pinned directly rather
  than inferred from the race.
- `a_delayed_act_is_stamped_at_act_time` - a submit with `submit_act_ms: 500`:
  assert its `OrderAccepted.ts_event` is at least 500 sim-ms after the sim-now
  read just before the send. Pins the 3.7a/4.8 re-sample, which is otherwise
  invisible to every other test here.
- `every_class_is_delayed_on_every_carrier` - one table-driven test over the
  cross product of `{Submit, Modify, Cancel}` and `{ws, POST /orders}`, arming
  that class's act and ack knobs alone and asserting BOTH that the outcome is
  late by act + ack and that the other two classes are prompt. MODIFY otherwise
  ships with no functional coverage at all, and the HTTP carrier otherwise rests
  on the 4.10 pass-through claim without a behavioral witness.
- `http_ack_latency_does_not_add_delay_acks` - arm both under HTTP, assert the
  response is late by `ack_ms` alone. Pins 3.11 rather than letting the additive
  wording be read as universal.
- `a_boundary_refusal_is_prompt_under_an_armed_act_delay` - a malformed submit
  on WS with `submit_act_ms: 800` armed: the `ProtocolError` arrives promptly
  and the pending-act budget is untouched (a second, well-formed submit still
  gets its slot under `pending_command_acts: 1`). Pins 3.7a's second bullet.
- `pending_act_budget_refuses_visibly` - a session built with
  `pending_command_acts: 1` (via `AdmissionLimits`), an armed act delay, two
  submits: the second answers `AdmissionRejected` with an
  `AdmissionSubject::Submit` naming its own id, and `QueryOrders` afterwards
  proves the second order never existed.
- `global_pending_act_budget_refuses_http` - `global_pending_command_acts: 1`,
  an armed act delay, two concurrent `POST /orders`: the second answers `503`
  with the `AdmissionRejected` body. Pins 3.8's HTTP half, which is the
  unbounded-task hole the earlier draft left open.
- `a_disconnect_completes_a_pending_act` - drop the socket mid-delay; assert the
  handler returns promptly (well under the armed delay, since teardown does not
  wait on the detached task) and that a SECOND session on the same account sees
  the order arrive after the delay elapses. The inverse of the earlier draft's
  assertion, and deliberately so: per 3.8 the venue already received the
  command.
- `zero_act_latency_keeps_the_synchronous_path` - with nothing armed, a submit
  followed immediately by a cancel behaves exactly as today (cancel finds the
  order). Guards 3.3.
- `config.rs`: `pending_command_acts: 0` is refused at startup, and
  `global_pending_command_acts` below `pending_command_acts` is refused too.

Gates:

- `brokkr check`
- `brokkr run mogwai -- serve` then `python3 scripts/smoke.py --command-latency`
- `brokkr test -p mogwai-server a_delayed_submit_does_not_stall_the_read_loop -N 5`
- `brokkr test -p mogwai-server pending_act_budget_refuses_visibly`
- `brokkr test -p mogwai-server every_class_is_delayed_on_every_carrier`

### L2 - adapter and documentation

`validate_window_deliverable`'s documented pass-through plus its unit test
(`CommandLatency` validates clean under `WsStreaming`, `HttpOrders` AND
`HttpPolling` - three assertions, because the point is that it is refused under
none of them).

Documentation, landed with this commit rather than alone:

- `reference/havoc.md` - a `CommandLatency` entry in the server-owned section
  covering: the act/ack split, composition with `DelayAcks` that is additive ON
  WS ONLY (3.11), the per-account scope, store-not-merge, the 1 h ceiling, the
  pending-act admission refusal on both surfaces, that concurrent delayed acts
  are unordered (3.4), that outbound wire order is still FIFO and what that
  costs the ack half's observability (3.5, 3.5a), that a pending act SURVIVES
  the disconnect of the socket that sent it (3.8), that `ClearDivergences` lifts
  a queued ACK but not an act already being served (3.6a), that a boundary
  refusal is never delayed (3.7a), and that it is the one server-owned control
  that is NOT WS-only. Extend the `ClearDivergences` bullet to name the six
  fields AND to say what it does not lift.
- `reference/clock.md` - a row in the axis table: `CommandLatency` act delay,
  "server order path", `sim.wall_duration` sleep; ack delay, "server writer",
  same conversion. Same granularity note as `DelayAcks`.
- `reference/config.md` - `pending_command_acts` and
  `global_pending_command_acts` rows, written to the same standard as the
  `exec_held_budget_bytes` row: what each bounds, why a FRAME count is the right
  unit here (a pending act is one command, not a payload), that the global one
  is what covers `POST /orders`, which has no per-connection lane, and that
  lowering the per-connection one is how the smoke test reaches the refusal.
- `reference/architecture.md` - the outbound-path description currently says the
  order path is serial on the read loop. It now has two paths, and the condition
  that selects between them is the armed act latency.
- `docs/todo.md` - delete the "BUILD: outbound per-command latency" entry. The
  penetration-gated-fills entry below it references RFC 4631 phase C as pending;
  update that reference to name this as landed, since the fill gate composes
  with act latency (a delayed submit meets a tape that has moved further).
- Delete this spec file.

Gate: `brokkr check --gate` (mandatory: this landing touches `mogwai-adapter`,
which plain `brokkr check` cannot see).

## 6. Ordering argument

L1 is the whole venue behavior, and it is coherent the moment it lands: every
field the control plane accepts is read, the catch-all `debug_assert!` is never
reachable because the interception arm arrives with it, and because a zero act
delay keeps the synchronous path (3.3) every pre-existing order-path test still
exercises exactly the code it did before. L2 touches no server behavior at all -
one adapter validation arm, its doc comment, and the reference docs. L1 does not
depend on L2 to be correct, neither is an env-var switch or a temporary route,
and neither leaves a control accepted-but-unimplemented at any point.

The rejected three-way split is documented at the top of section 5 so it is not
re-proposed as "smaller landings are always better": here they were smaller by
making the control plane dishonest between them.

## 7. Stopping rule

In scope: the six knobs, both order-entry surfaces, the per-connection AND
process-wide pending-act budgets, the pump composition, the adapter's validation
pass-through, and the docs above.

Explicitly out of scope, and not deferred work but separate items:

- **Query latency.** `QueryOrders` / `QueryFills` are the reconciliation witness.
  Delaying their delivery is already expressible with `DelayAcks`, and giving
  the witness its own act latency invites a scenario where the truth surface is
  the slowest thing on the venue - which reads as mogwai hiding the state it
  exists to expose. If a consumer ever asks for it, it is its own item.
- **Stochastic latency.** All six knobs are constants, like every existing
  window. A distribution (jitter, a heavy tail) is a different feature and would
  bring the golden-file question with it - which `docs/todo.md` already parks
  behind RFC 4631 phase D.
- **Penetration-gated fills** (RFC 4631 phase A), the queue-ahead decision
  (phase B) and the fill-path benches (phase D). Separate `docs/todo.md` items,
  and phase A is blocked on the arrival-drought decision while this is blocked
  on nothing.
- **Making the wire reorder.** 3.5. `ClientHavoc.reorder_prob` already exists on
  the consumer side.
- **Extending `DelayAcks` to `POST /orders`.** 3.11. It would invert
  `validate_window_deliverable`'s existing refusal under `orders_over_http()`,
  which consumers configure against; changing it is a control-semantics item
  with its own adapter and documentation blast radius, not a rider on this one.
- **An interruptible act sleep.** 3.6a. `ClearDivergences` governs commands the
  venue has not started acting on; making it reach into flight is a different
  control ("abort in-flight acts") and would need its own name.
- **A `tower` concurrency or timeout layer on the HTTP surface.** The
  process-wide pending-act semaphore bounds what THIS feature creates, which is
  the obstacle this spec is responsible for. A general request-concurrency or
  request-timeout policy for `mogwai-server` is a standing infrastructure
  decision that should not be made incidentally here.
- **Overflow hardening on the composed window.** Both operands are bounded by
  `MAX_DIVERGENCE_MS`, so the sum cannot overflow `u64`; the `saturating_add` in
  4.9 is belt-and-braces and nothing further is specified.
- **The inbound `HavocLatency`.** Untouched; it stays an adapter-side consumer
  knob and its doc comment stays true.
