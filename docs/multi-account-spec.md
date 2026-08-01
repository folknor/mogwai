# Multi-account venue: implementation spec

Written against `reference/technical-implementation-spec.md` (the contract this
document is judged by). Spawned from `docs/bug-sweep.md` item **X11, "The venue
is single-account and must become multi-account (200+ concurrent)"**, which is
the requirement statement and the source of every deployment fact cited here.

## 1. Goal

One `mogwai-server` process serves N independent trading accounts concurrently,
each with its own ledger, its own order book state, its own fills and its own
armed divergences. Two accounts trading the same symbol at the same time see
nothing of each other in `/account`, `QueryOrders`, `QueryFills` or their
execution streams.

The deployment target from X11: a `wyrd` batch of 200 strategies across up to
ten symbol scopes forward-validates concurrently, one account per
`(strategy, symbol)` pair, so the working ceiling is ~2000 live accounts.
Accounts are TRANSIENT - created for a validation run, destroyed when it ends -
so create/destroy and reclaim-on-teardown are part of this spec, not a later
one: without reclaim the unbounded `seen_client_order_ids` / `closed` / `fills`
retention inside `Engine` loses its in-code justification and a long-lived
daemon accumulates every order of every batch it has ever run.

## 2. Survey of the ground

### 2.1 What is single-account today, exactly

`crates/mogwai-server/src/main.rs`, in `serve_async`, builds ONE engine and
hands it to axum:

```rust
let state = AppState {
    engine: Arc::new(Mutex::new(Engine::with_instruments_and_balances(
        profiles.instrument_defs(),
        cfg.balances.clone(),
    ))),
    ...
};
let app = Router::new()...with_state(state);
```

`AppState` (`crates/mogwai-server/src/http.rs`) is `Clone` and its `engine` is
an `Arc<Mutex<Engine>>`, so every HTTP request and every websocket session
clones a handle to the SAME engine. There is no `Engine::new` anywhere on the
connection path. The five call sites that touch it:

| Site | File | What it does |
| --- | --- | --- |
| `process_order_cmd` | `http.rs` | `state.engine.lock().await`, `book_shape()`, `process(cmd, ts)` - the shared order-entry gate for both `POST /orders` and `/ws` |
| `arm_divergence` | `http.rs` | `engine.arm(div)` for engine-side variants, `engine.cancel_open_order_silently(..)` for the immediate one |
| `instruments` | `http.rs` | `engine.instrument_defs()` |
| `account` | `http.rs` | `engine.account_snapshot(ts)` (takes `&mut self`) |
| test helper `state()` | `main.rs` | builds an `AppState` for the server test suite |

Three further pieces of per-account state do NOT live in `Engine` at all; they
live as process-global atomics on `AppState` and are read by every connection's
writer and exec pump:

- `delay_ms` (`DelayAcks`)
- `dark_until_ns` (`GoDark`)
- `stall_until_ns` (`StallData`)

`handle_socket` (`ws.rs`) clones `dark_until_ns` / `stall_until_ns` out of
`AppState` once at connection setup and moves the clones into the writer task;
`delay_ms` is read by the exec pump. So arming `GoDark` today blacks out the
whole fleet.

`Engine` itself (`crates/mogwai-engine/src/lib.rs`) is ALREADY a self-contained
per-account unit: `open`, `account`, `seen_client_order_ids`, `closed`, `fills`,
`armed`, `seq` are all private fields of the one struct, and every public method
is `&self` / `&mut self` on it. Nothing in `mogwai-engine` is global, static, or
cross-account. This is the load-bearing survey finding: the engine's ISOLATION
needs no work for multi-account - relocating the struct into a per-account slot
is the whole of it. The changes are in `mogwai-server`, `mogwai-protocol`
(transport surface and the admission size model) and `mogwai-adapter` (send the
identity).

One qualification, because an earlier draft of this spec overclaimed it: the
engine is not literally untouched. `Account::snapshot` in
`crates/mogwai-engine/src/account.rs` constructs every `AccountState` literal
directly and has no account identity to put in the new required field. Section
3.1 pins how that is resolved; it is a small, mechanical engine change, not zero.

### 2.2 What is genuinely shared and must stay shared

- `profiles: Arc<InstrumentProfiles>` - instrument defs and generator scalars.
- `sim: SimClock` and `data_origin_ns` - one venue clock, one tape origin.
- `replay_permits: Arc<Semaphore>` - the global replay ration.
- The generated tape itself.

None of these become per-account. An account is a LEDGER, not a world.

### 2.3 The client side needs no work

A nautilus `ExecutionClient` carries exactly one `account_id`
(`crates/mogwai-adapter/src/config.rs`, `MogwaiExecClientConfig::account_id`,
default `MOGWAI-001`), and N accounts means N clients in N broadarrow workers.
The identity already exists on the client; it simply never leaves the adapter.
The adapter work in this spec is only "put the configured id on the wire".

The adapter's outbound surfaces that must carry it:
`fetch_account`, `post_order`, `ship_server_havoc`,
`fetch_clock_or_identity` (exempt, see 3.2) in `client/exec.rs` and
`client/shared.rs`; the history/quote pulls in `client/data.rs`; and the
websocket connect URL in both clients.

### 2.4 What is NOT wrong today

`max_concurrent_replays` (default 1024) and the per-symbol replay map in
`handle_socket` are keyed per CONNECTION and per SYMBOL, not per account. X11
predicts 2000 accounts implies 2000 replay threads. That is true only because
2000 accounts implies 2000 connections. Multi-account does not by itself
multiply replays beyond what 2000 connections already cost today, so tape
multiplexing is a separable item (see the stopping rule, section 7) - but this
spec must not make it worse, and section 5.2 pins the test that proves it does
not.

What IS wrong today, and stays wrong after this spec, must be said plainly
rather than left for the deployment to discover: `max_concurrent_replays`
defaults to **1024** (`crates/mogwai-server/src/config.rs`), and the X11 target
is ~2000 concurrent workers each subscribing at least one symbol. A default
build therefore refuses subscriptions somewhere past the halfway mark of the
target batch. This spec does not fix that - identity and ledger isolation are
orthogonal to replay accounting - but it does two things about it:

- the deployment must raise `max_concurrent_replays` past the batch's total
  subscription count (2000 workers times their symbol count), and
  `reference/config.md` gains that sentence in L3;
- the residual - one OS thread and one tape cursor per subscription at that
  scale - is the tape-multiplexing item in section 7.1, which is the thing that
  actually makes 2000 workers cheap. Until it lands, "2000 accounts" is proven
  only for the ledger dimension, not as a running fleet. This document is the
  identity/ledger brick; it does not claim the deployment target is met.

## 3. The target: concrete artifacts

### 3.1 Account identity is a TRANSPORT attribute, not a message field

X11's design question 1 offers "an `account_id` on the relevant `ClientMessage`
variants plus an HTTP header" versus "a connect-time handshake minting a
session". Neither is taken as written. The decision:

**Identity travels out-of-band on every request: an HTTP header on every HTTP
call, a query parameter on the websocket upgrade.** `ClientMessage` gains no
account field.

Reasoning, since two implementers must reach the same artifact:

- The handshake option is refused because `POST /orders`, `GET /account` and
  `POST /control/divergence` are stateless and are used WITHOUT a websocket at
  all (`submit_order_http` is a first-class order-entry surface, and
  `scripts/smoke.py` drives the control plane over plain HTTP). A
  WS-session-scoped session token does not generalize to them.
- Putting `account_id` inside `ClientMessage` is refused because it would have
  to be added to seven variants, made mandatory (a defaulted field silently
  routes to a wrong account), and then validated for agreement against the
  socket it arrived on - a second source of truth for the same fact. Real
  venues put identity in the auth header of every request; that is the shape
  taken here.
- Consequence, stated so it is not discovered later: a websocket connection is
  bound to ONE account for its lifetime, fixed at upgrade. A fleet worker owns
  one account, so this costs nothing.

Exact surface, in `mogwai-protocol/src/lib.rs`:

```rust
/// HTTP header carrying the acting account on every request to the venue.
pub const ACCOUNT_HEADER: &str = "x-mogwai-account";
/// Query parameter carrying the acting account on the `/ws` upgrade.
pub const ACCOUNT_QUERY_PARAM: &str = "account";
```

and a newtype in `mogwai-protocol/src/messages.rs` beside the other id types:

```rust
/// Cap on an account id, joining `MAX_CLIENT_ID_LEN` and friends in the
/// protocol's cap set. `sizing.rs` budgets against this constant, so it is a
/// wire-model input, not merely a validation nicety.
pub const MAX_ACCOUNT_ID_LEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

impl AccountId {
    /// Venue-side validity. Rejects empty, over-`MAX_ACCOUNT_ID_LEN`, and
    /// anything outside `[A-Za-z0-9._:-]`, so an account id is safe to log, to
    /// use as a map key, and to echo into an error body without escaping.
    pub fn parse(raw: &str) -> Result<Self, AccountIdError>;
    pub fn as_str(&self) -> &str;
}

/// `Display` and `std::error::Error` are REQUIRED, not optional: 3.2 mandates
/// that a 400 body name both the header and the reason it was refused, and the
/// adapter's construction-time check (3.7) surfaces the same value through
/// `anyhow`. A bare enum with only `Debug` forces every call site to hand-roll
/// prose and they will diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountIdError { Empty, TooLong, IllegalChar(char) }
```

`AccountState` (the `/account` body) gains `pub account_id: AccountId` so a
snapshot is self-describing and a misrouted response is detectable by the
client rather than silently adopted.

**Who produces that field.** `Account::snapshot` (`mogwai-engine/src/account.rs`)
builds the `AccountState` literal and has no identity of its own. The resolution
is that the `Engine` STORES its account id: `Engine::with_instruments_and_balances`
gains an `AccountId` first parameter, the id lands in a private field, and
`snapshot` stamps it. Rejected alternatives, so this is not relitigated:
threading the id through `snapshot(ts, id)` pushes the same fact through every
caller and lets two call sites disagree; returning an account-neutral internal
struct that the server wraps duplicates the type and means the engine's own
tests no longer exercise the shape that goes on the wire. A stored field is one
source of truth and matches how the slot already owns the pair. Consequence:
every `Engine` constructor call in the engine's own test module takes an id, so
L1 touches `mogwai-engine`, and the L1 gate is `brokkr check`, not
`brokkr check -p mogwai-protocol`.

**The admission size model must move with it.** This is the least obvious
coupling in the whole change. `crates/mogwai-protocol/src/sizing.rs` bounds a
snapshot at

```rust
pub fn account_state_max_bytes(shape: &BookShape) -> usize {
    128 + shape.balances * BALANCE_ROW_MAX_BYTES + shape.positions * POSITION_ROW_MAX_BYTES
}
```

and `process_order_cmd` reserves that bound BEFORE the engine mutates; the model
must dominate real output or the reservation voids (`admission.rs` asserts
exactly this, and `worst_case_reservation_covers_actual_output` in
`mogwai-engine/src/lib.rs` samples it). A 64-byte account id plus its JSON key
and quoting can consume the entire 128-byte envelope constant on its own, so
adding the field WITHOUT touching sizing turns a proven bound into a false one -
the failure mode is a voided reservation under a long id, not a compile error,
which is why it must be pinned here. The envelope becomes

```rust
128 + 16 + ESC * MAX_ACCOUNT_ID_LEN
```

(the 16 covers the `"account_id":` key and its punctuation, matching how
`SNAPSHOT_ENVELOPE_MAX_BYTES` and the row constants already account for keys),
`MAX_ACCOUNT_ID_LEN` is imported into `sizing.rs` alongside `MAX_CLIENT_ID_LEN`,
and the domination test is extended to sample a MAX-LENGTH account id rather
than the short ids the fixtures use today. L1 is therefore additive on the wire
but NOT "green by construction"; the sizing edit is part of it.

### 3.2 Which endpoints require identity

| Endpoint | Identity | Rationale |
| --- | --- | --- |
| `GET /health` | none | liveness, no ledger |
| `GET /clock` | none | one venue clock |
| `GET /instruments` | none | venue-global listing; served from `profiles`, NOT from an engine (behavior change, see 3.6) |
| `GET /trades`, `GET /quotes` | none | market data is shared |
| `GET /account` | REQUIRED | per-account ledger |
| `POST /orders` | REQUIRED | per-account book |
| `POST /control/divergence` | REQUIRED | per-account havoc (3.5) |
| `GET /ws` | REQUIRED (query param) | binds the session |
| `DELETE /accounts/<id>` | n/a - id is in the path | teardown (3.4). The path segment is parsed with `AccountId::parse`; a malformed segment is `400`, an unknown one `404`. No header is read, and none is required - a control-plane operator is not acting AS the account. |
| `GET /accounts` | none | operator listing (3.4). Unguarded, like `/health`: section 7.3 excludes authentication entirely, so a header here would be theater. |

A missing or malformed header on a REQUIRED endpoint is `400 Bad Request` with
a body naming the header AND the `AccountIdError` reason (via its `Display`).
It is never defaulted to a fallback account: a silently-defaulted identity is
exactly the fleet-contamination bug being fixed.

On the `/ws` upgrade the same failure is still `400`, but the body names the
QUERY PARAMETER, not the header - `account=`, per `ACCOUNT_QUERY_PARAM`. The
distinction matters because the test in 5.2 asserts on that body.

The full status set for an identity-bearing request, so no code is left to
invent one:

| Condition | Status |
| --- | --- |
| header/param absent | `400`, body names the header (or the param, on `/ws`) |
| present but fails `AccountId::parse` | `400`, body names it plus the reason |
| well-formed, account exists | proceed |
| well-formed, account absent, room under `max_accounts` | auto-create, proceed |
| well-formed, account absent, at `max_accounts` | `429 Too Many Requests` |

**Identity is resolved before anything else, and a rejected request creates
nothing.** `acquire` is called only after parse succeeds and only on the path
that will actually serve the request, so no 4xx path can leave a slot behind.
This is a stronger rule than "a malformed id creates no account": a WELL-FORMED
id on a request that is refused for any other reason (unparseable body, unknown
symbol, an at-capacity refusal) equally must not have minted a slot, or a
malformed batch quietly fills the registry to `max_accounts` with ghosts. 5.2
pins the malformed case and 5.3 pins the at-capacity case.

### 3.3 The registry

New module `crates/mogwai-server/src/accounts.rs`:

```rust
/// One account's private venue state. Everything here is per-account by
/// construction; anything shared (clock, instruments, tape) stays on AppState.
pub(crate) struct AccountSlot {
    pub(crate) id: AccountId,
    /// Monotonic per-id incarnation. `ACC-A` destroyed and re-created is a NEW
    /// generation with a NEW slot; the old one keeps serving nothing while its
    /// last Arc drains. Every removal in the registry is generation-checked, so
    /// a reap cannot delete the incarnation that replaced the one it decided
    /// about (see the TOCTOU note below).
    pub(crate) generation: u64,
    /// Set by `destroy` / the reaper the instant the slot leaves the map.
    /// Every request path checks it AFTER acquiring the slot and refuses with
    /// `410 Gone`; every session's writer selects on `closed` below. Without
    /// this a websocket that holds an `Arc<AccountSlot>` keeps trading against
    /// a removed engine indefinitely.
    pub(crate) tombstoned: AtomicBool,
    /// Fired once at tombstoning. Bound sessions wake on it and close - they do
    /// NOT wait to discover teardown on their next engine access, because a
    /// market-data-only session may never make one, which would both defeat the
    /// abort path and pin the engine's retention forever.
    pub(crate) closed: tokio::sync::Notify,
    /// The per-account exchange core. Unchanged from the single-account build -
    /// `Engine` was already self-contained, so this is a relocation, not a rewrite.
    pub(crate) engine: Mutex<Engine>,
    /// Execution-event delay, GoDark and StallData windows: per-account now, so
    /// arming a blackout on one worker leaves the other 1999 running clean.
    /// Relaxed for the reason documented on the old AppState fields.
    pub(crate) delay_ms: AtomicU64,
    pub(crate) dark_until_ns: AtomicU64,
    pub(crate) stall_until_ns: AtomicU64,
    /// Live websocket sessions bound to this account. A slot with sessions > 0
    /// is never idle-reaped. Only ever mutated through `SessionLease` below -
    /// never incremented or decremented by hand.
    pub(crate) sessions: AtomicUsize,
    /// WALL-clock ns of the last request on this account, for the idle reaper.
    /// See 3.4 for why this is wall time and not sim time.
    pub(crate) last_seen_ns: AtomicU64,
}

/// RAII counter for `AccountSlot::sessions`. `handle_socket` takes one at entry
/// and holds it for the socket's life; `Drop` decrements. Hand-rolled
/// increment/decrement pairs are FORBIDDEN, because every abnormal exit -
/// a panic in the writer, the quiesce path, close-on-overload, an early return
/// on a failed subscribe - skips the decrement, and a leaked increment makes
/// the account permanently unreapable. That is the same unbounded-retention
/// leak this spec exists to fix, wearing a different hat.
pub(crate) struct SessionLease {
    slot: Arc<AccountSlot>,
}

impl SessionLease {
    /// Increments `sessions` and stamps `last_seen_ns`.
    pub(crate) fn acquire(slot: Arc<AccountSlot>) -> Self;
    pub(crate) fn slot(&self) -> &Arc<AccountSlot>;
}

impl Drop for SessionLease {
    /// Decrements `sessions`. Panic-safe: unwinding runs it.
    fn drop(&mut self);
}

pub(crate) struct AccountRegistry {
    slots: std::sync::Mutex<HashMap<AccountId, Arc<AccountSlot>>>,
    /// Next generation to hand out, bumped on every creation.
    next_generation: AtomicU64,
    template: AccountTemplate,
    max_accounts: usize,
}

/// What a freshly auto-created account starts with, cloned from run config.
/// `instruments` is an `Arc` because section 2.2 says instrument defs are
/// SHARED, and at 2000 accounts a per-account `Vec<InstrumentDef>` copy
/// contradicts that for no benefit - the defs are immutable after boot.
pub(crate) struct AccountTemplate {
    pub(crate) instruments: Arc<[InstrumentDef]>,
    pub(crate) balances: HashMap<String, Decimal>,
}

impl AccountRegistry {
    pub(crate) fn new(template: AccountTemplate, max_accounts: usize) -> Self;

    /// Get-or-create. The ONE lookup on the request path.
    /// Errors with `AtCapacity` when a NEW account would exceed `max_accounts`;
    /// an existing account is always served, capacity or not.
    pub(crate) fn acquire(&self, id: &AccountId, now_ns: u64)
        -> Result<Arc<AccountSlot>, RegistryError>;

    /// Existing only - no creation. Used by teardown and by the operator listing.
    pub(crate) fn get(&self, id: &AccountId) -> Option<Arc<AccountSlot>>;

    /// Remove the slot, tombstone it and notify its sessions. Returns false if
    /// it was not present. Tombstone-then-remove happens under the `slots`
    /// lock so no acquirer can hand out a slot that is already condemned.
    pub(crate) fn destroy(&self, id: &AccountId) -> bool;

    /// ONE atomic pass: under a single `slots` lock, find every slot with
    /// `sessions == 0` and `last_seen_ns + idle_ns <= now_ns`, remove it,
    /// tombstone it, and return what was reaped. Deliberately NOT
    /// `idle_since()` followed by `destroy()`: between those two calls a
    /// request can acquire the slot, bump `last_seen_ns` and start trading, and
    /// the reaper would delete it anyway. Doing the eligibility recheck and the
    /// removal under one lock closes that window; the generation field closes
    /// the remaining one, where an id is re-created between the two and the
    /// reaper removes the fresh incarnation.
    pub(crate) fn reap_idle(&self, now_ns: u64, idle_ns: u64) -> Vec<AccountId>;

    pub(crate) fn len(&self) -> usize;

    /// Async, because `open_orders` lives behind each slot's
    /// `tokio::Mutex<Engine>`. The body clones the `Arc<AccountSlot>` list out
    /// from under the `std::sync::Mutex`, RELEASES it, and only then awaits the
    /// engine locks - a synchronous signature would force `blocking_lock` on a
    /// runtime worker thread, which is a stall the `await_holding_lock` rule in
    /// this section exists to prevent.
    pub(crate) async fn summaries(&self) -> Vec<AccountSummary>;
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct AccountSummary {
    pub(crate) account_id: AccountId,
    pub(crate) generation: u64,
    pub(crate) sessions: usize,
    pub(crate) open_orders: usize,
    pub(crate) last_seen_ns: u64,
}
```

**Post-acquire tombstone check.** Every handler that acquires a slot rechecks
`tombstoned` after taking the engine lock and before mutating, refusing with
`410 Gone`. `acquire` returning a live slot is not a guarantee that it is still
live by the time the engine lock is won; without the recheck, a `DELETE`
concurrent with an in-flight order lets that order fill into an engine nobody
will ever read again, and the caller is told it succeeded.

**Recreation while old sessions drain.** A `DELETE` followed immediately by a
request under the same id yields a NEW slot at generation N+1, funded from the
template, with an empty ledger. The old slot lives on only as long as its
condemned sessions hold Arcs, serves nothing (every path is tombstoned), and
drops when the last one closes. The two never share state and the old sessions
are never adopted by the new incarnation - a socket bound to generation N stays
bound to generation N until it closes.

Locking discipline, pinned because it is the one place this design can deadlock
or serialize:

- `slots` is a **`std::sync::Mutex`, held only across a `HashMap` get/insert**.
  No `.await` occurs while it is held, so it never crosses a yield point and
  cannot be held during engine work. Clippy's `await_holding_lock` enforces
  this and `brokkr check` runs clippy.
- `AccountSlot::engine` is a **`tokio::sync::Mutex`**, exactly as `AppState`'s
  engine is today, because `process_order_cmd` holds it across the
  `book_shape` read and `process` call inside an async fn.
- Net effect X11 predicted and this design delivers: the process-global engine
  mutex becomes per-account, so unrelated workers stop serializing.

`AppState` after the change:

```rust
pub(crate) struct AppState {
    pub(crate) accounts: Arc<AccountRegistry>,   // replaces `engine`
    pub(crate) cfg: Config,
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    pub(crate) sim: SimClock,
    pub(crate) data_origin_ns: u64,
    pub(crate) replay_permits: Arc<Semaphore>,
    // delay_ms / dark_until_ns / stall_until_ns are GONE from here - they moved
    // into AccountSlot.
}
```

### 3.4 Lifecycle: implicit create, explicit destroy, idle reap

X11's design question 2 is settled as **implicit auto-creation** - a batch turns
over constantly and nobody edits `mogwai.toml` per batch. The typo risk that
pre-declaration would have caught is answered instead by `max_accounts` plus
`GET /accounts` (a typo shows up as an extra account in the listing) and by the
adapter refusing to send an id its own config did not set.

Question 4 (how a run declares itself finished) is settled as **both**
mechanisms, because either alone leaks:

- `DELETE /accounts/<id>` - explicit teardown, the normal path. `200` with an
  empty body on success, `404` if unknown. Destroying an account with live
  websocket sessions is allowed and is the abort path: the slot is tombstoned
  and removed under the registry lock, and every bound session wakes on
  `AccountSlot::closed` and closes with a `ProtocolError` naming the teardown.
  It does NOT wait to discover teardown on its next engine access: a
  market-data-only session makes none, so lazy discovery would leave it trading
  a phantom and pinning the engine's `seen_client_order_ids` / `closed` /
  `fills` for the daemon's life - the exact retention leak section 1 cites as
  the reason this spec exists. This is deliberate - a driver aborting a run
  must not have to unwind sockets first.
- **Idle reaper** - a tokio task spawned in `serve_async`, ticking every
  `account_reap_interval_ms`, calling `reap_idle(wall_now, account_idle_timeout_ns)`.
  This is the policy for the abandoned run (a driver that dies without tearing
  down), which X11 names as required. A slot with `sessions > 0` is never
  reaped, so a live-but-quiet validation run is safe regardless of tape deserts
  (see `docs/bug-sweep.md` D16 - a multi-hour trade desert means a live account
  can go hours without order activity, which is precisely why the reaper keys
  on SESSIONS, not on last order).

**The reaper uses WALL time, not sim time.** This is a correction of an earlier
draft and the reasoning is load-bearing. `Config::speed` is a free multiplier
and `0.0` means unthrottled; `sim_epoch_ns` can anchor the run anywhere. Under
an accelerated or unthrottled clock a "one hour" sim-time idle budget can elapse
in seconds of real time, so a driver that pauses to compute for ten real
seconds can return to find its account reaped and silently re-auto-created
EMPTY - a wrong-answer failure with no error anywhere. Idleness is a property of
the DRIVER's real-world liveness, which has nothing to do with the tape's clock,
so `last_seen_ns` and the timeout are both wall-clock nanoseconds. `sim_now` is
reserved for anything that must line up with the tape; account lifetime is not
one of those things.

**The session-less driver.** `sessions > 0` is not a sufficient liveness signal
for every supported client. `TransportProfile::HttpPolling`
(`mogwai-protocol/src/transport.rs`) polls `GET /trades` for market data and
posts orders over HTTP, so it owns NO websocket at all; `scripts/smoke.py`
drives the control plane the same way, and 3.1 calls `submit_order_http` a
first-class order-entry surface. Such a driver sits at `sessions == 0`
permanently. It is protected by the OTHER half of the rule rather than by a
special case: `last_seen_ns` is stamped on EVERY identity-bearing request -
`POST /orders`, `GET /account`, `POST /control/divergence` - so a polling
driver that touches its ledger even once per idle window is live by the same
test a websocket is. The residual hole is a polling driver that holds a funded
account while doing nothing but reading the accountless `/trades` for longer
than the timeout; `GET /account` is the documented keepalive for that case and
`reference/config.md` says so next to `account_idle_timeout_ms`. Making
`/trades` identity-bearing was rejected: market data is genuinely shared (2.2)
and requiring an account to read it would make the clock/tape probes account-
creating, which 3.7 explicitly forbids.

**Who calls `DELETE`.** Stated because "the normal path" was asserted without
an owner: no artifact in this workspace or in broadarrow's
`register_mogwai_forward` calls it today, and L4 of this spec only stamps
identity. The teardown caller is the RUN DRIVER - whatever broadarrow component
owns the lifetime of a `(strategy, symbol)` worker - and wiring it is a
broadarrow-side change this spec does not contain (section 7.7). Until that
lands, the idle reaper is the only mechanism that actually fires, which is why
it is specified as a safety net that must work standalone rather than as a
backstop for a path that is assumed present. A reconnect is NOT a teardown: a
worker dropping and re-opening its socket keeps its ledger, because only an
explicit `DELETE` or the idle timeout destroys a slot.

New config keys in `crates/mogwai-server/src/config.rs`, with defaults:

```rust
/// Hard ceiling on concurrently-live accounts. A new account beyond this is
/// refused with 429 rather than admitted, so a runaway batch degrades visibly
/// instead of exhausting memory. Default sized for the 2000-account deployment
/// target with headroom.
pub(crate) max_accounts: usize,              // default 4096, 0 == unlimited
/// WALL-clock idleness after which a session-less account is destroyed.
pub(crate) account_idle_timeout_ms: u64,     // default 3_600_000 (1h), 0 == never
/// Reaper tick period. 0 is REJECTED at config validation (alongside the
/// existing `speed`/`sim_epoch_ns` checks in `config.rs`) rather than given a
/// meaning: read as "never" it silently disables the only teardown mechanism
/// that fires today, and read as "every tick" it is a busy loop. Disabling the
/// reaper has exactly one spelling, `account_idle_timeout_ms = 0`.
pub(crate) account_reap_interval_ms: u64,    // default 60_000, 0 == config error
```

`[balances]` and `[[instruments]]` keep their exact current meaning and become
the account TEMPLATE: every auto-created account starts funded identically.

Reclaim is by drop: destroying the slot drops the `Engine`, which drops
`seen_client_order_ids`, `closed` and `fills`. That is the whole of X11's
reclaim requirement - but only once the LAST `Arc<AccountSlot>` goes, which is
why teardown notifies sessions rather than waiting for them to notice.
Section 5.3 pins the actual drop with a `Weak<AccountSlot>` upgrade check, not
merely with the observation that a subsequent `GET /account` returns a fresh
template: a fresh template proves the map entry is gone, which is compatible
with the old engine still being fully resident behind a leaked Arc.

### 3.5 Divergences become per-account

X11's design question 3 is settled as **per-account**. With a fleet, arming
`DelayAcks` on one worker while the rest run clean IS the point.

`POST /control/divergence` requires the account header and routes every variant
to that account's slot:

- `DelayAcks` / `GoDark` / `StallData` / `ClearDivergences` - write the slot's
  atomics instead of `AppState`'s. The store-not-extend semantics of the two
  window variants are unchanged.
- engine-side variants - `slot.engine.lock().await.arm(div)`, including the
  eviction-relay body, unchanged.
- `CancelOpenOrderSilently` - `slot.engine.lock().await
  .cancel_open_order_silently(..)`, unchanged, and the `404` on a miss now also
  fires when the id belongs to a DIFFERENT account, which is correct: the
  arming account has no such order.

The reason-truncation gate at the arming boundary is untouched.

`handle_socket` clones `Arc<AccountSlot>` (not the individual atomics) into the
writer and the exec pump, so both read the account's current windows. `ws.rs`'s
writer signature changes from three `Arc<AtomicU64>` to one `Arc<AccountSlot>`.

Data-surface havoc is unaffected by tape sharing today because each connection
spawns its own replay and its own `HavocFilter`; X11 flags that a future shared
tape would need data havoc to stay per-subscriber. Recorded here as a
constraint the tape-multiplexing item inherits, not solved here.

### 3.6 `GET /instruments` stops reading an engine

Today it answers from `engine.instrument_defs()`. With a registry that would
force an arbitrary account choice for a venue-global fact. It is re-pointed at
`state.profiles.instrument_defs()`, which is the SAME data - `serve_async`
builds the engine's instruments from exactly that call. This is a refactor with
no observable wire change, and section 5.2 names the test that proves the two
lists are byte-identical.

### 3.7 Adapter changes

Per the workspace rule these APIs have two access paths and this section uses
both: the nautilus and broadarrow shapes cited below were READ from the in-tree
copies `research/nautilus_trader` and `research/broadarrow`, while the adapter
BUILDS against the sibling checkout `../nautilus_trader`, default-features off.
`research/` is never a build input.

`MogwaiExecClientConfig::account_id` (a nautilus `AccountId`) and a NEW
`MogwaiDataClientConfig::account_id` are stringified once at client
construction into a `mogwai_protocol::AccountId` and stamped on every outbound
request:

- every `http.get(..)` / `http.post(..)` call gains the header
  `x-mogwai-account: <id>` in its header map (the `post_order` path already
  builds a `HashMap` of headers; the `get` paths currently pass `None` and now
  pass `Some(headers)`);
- the websocket URL gains `?account=<percent-encoded id>`;
- `fetch_clock_or_identity` and the `/health` probe deliberately send no header
  and must keep working without one - they are used before an account is
  meaningful, and a header requirement on them would make a clock probe create
  an account.

**Construction-time validation of the id.** The two charsets do not agree and
the mismatch is silent. Nautilus's `AccountId::new_checked`
(`research/nautilus_trader/crates/model/src/identifiers/account_id.rs`) demands
only a non-empty ASCII string containing a `-` with non-empty parts either side -
it accepts spaces and slashes that mogwai's `AccountId::parse` rejects. So a
perfectly legal nautilus config today would produce a client that `400`s on
every single request at runtime, with the first symptom appearing under live
trading. Both factories therefore run `mogwai_protocol::AccountId::parse` on the
stringified id at CONSTRUCTION and fail the client build with the
`AccountIdError` in the message. Noted while checking this: `WYRD-042:BTCUSDT`,
the deployment id shape in section 1, parses in nautilus as issuer `WYRD` -
implementers should confirm nothing on the nautilus side assumes
`get_issuer() == venue` before adopting that convention wholesale.

The data client needs an id because it opens its own `/ws`. Its default mirrors
the exec default (`MOGWAI-001`).

**Broadarrow does not currently set the data id, and must.** The claim that
broadarrow sets both to the same value per worker is FALSE in the surveyed
source: `register_mogwai_forward` in
`research/broadarrow/crates/run-prep/src/venue.rs` passes the supplied
`account_id` only to `applied_exec_config`, while `applied_data_config` builds
from `MogwaiDataClientConfig::default()` and never sees it. Left alone, every
data client in a 2000-worker fleet reports `MOGWAI-001`, which means all 2000
data sockets attach to ONE account slot: a per-account `GoDark` or `StallData`
armed by worker 47 either misses its own data socket entirely or blacks out
every worker's, depending on which id is armed. That is the fleet-contamination
bug re-entering through the data plane. The fix is a one-line broadarrow change
(thread `account_id` into `applied_data_config`), it lives in the CONSUMER repo
and so is not landed by this spec, and it is recorded as a coordination
obligation in section 7.7. Until it lands, per-account data havoc is not
delivered end to end, and L4's tests cover only the mogwai side of the contract.
A data client's account never trades; it exists so the upgrade can be attributed
and so its `GoDark` window is its own.

`AccountState.account_id` is checked wherever a snapshot arrives, which is BOTH
paths, not just the pull: `fetch_account`, and the pushed
`ServerMessage::AccountState` frames that reach `handle_account_state` in
`client/exec.rs`. The pushed path is the one that actually matters and is the
one currently unguarded - `handle_account_state` stamps the CONFIGURED client id
onto the nautilus event regardless of what the wire said, so a misrouted
snapshot is adopted silently and relabelled as one's own. Both paths reject a
mismatch as an error, not a warning. That check is what makes the
fleet-contamination failure X11 describes (worker 112 seeing worker 47's fills)
impossible to reintroduce silently.

## 4. Landing sequence

Four landings. The suite is green at every boundary, and each is independently
keep/revert-able on its own gate. No feature flags, no env-var switches, no
compatibility shims left behind at the end.

**L1 - protocol surface and the size model.** `AccountId`, `AccountIdError`
(with `Display` / `Error`), `MAX_ACCOUNT_ID_LEN`, the two constants,
`AccountState.account_id`, the `Engine` id field that produces it, and the
`account_state_max_bytes` envelope bump with its extended domination test. It
is additive ON THE WIRE but it is not "green by construction": it touches
`mogwai-engine` (every `Engine` constructor call gains an id) and `sizing.rs`,
so its gate is a full `brokkr check`.

Note the one wire break inside L1: `AccountState` gains a required field, so a
mixed old-server/new-adapter pair fails to decode. Pre-1.0, both ends ship from
this workspace, and L4 lands the adapter side - acceptable and stated rather
than worked around.

**L2 - registry and per-account engine, identity required.** `accounts.rs`,
`AppState` swap, header/query extraction, the four handlers re-pointed,
`handle_socket` bound to a slot via a `SessionLease`, the atomics moved,
`/instruments` re-pointed, `scripts/smoke.py` updated to send the header.

`ws_upgrade` changes signature, and this is not incidental. It is today

```rust
pub(crate) async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse
```

and gains a `Query<AccountParams>` extractor plus a fallible return, so that a
missing, malformed or at-capacity id is answered as an HTTP `400`/`429` BEFORE
`on_upgrade` is called. Resolving identity after the upgrade instead would
force the failure to be reported as a post-handshake close frame, which a
client cannot distinguish from a venue restart and which 5.2 explicitly
asserts against.

After L2 the venue is
multi-account and every server test that constructs `AppState` is updated. This
is the intrusive landing; it is one coherent change because a half-moved
`delay_ms` (some readers on `AppState`, some on the slot) is a silently-wrong
intermediate state, which rule 6 forbids.

**L3 - lifecycle.** `DELETE /accounts/<id>`, `GET /accounts`, the three config
keys plus the `account_reap_interval_ms != 0` validation, the reaper task and
its atomic `reap_idle`, the capacity refusal, the tombstone/`Notify` close path.

L3 also lands the documentation, which is not optional cleanup: `reference/` is
compiled into the binary through `include_str!` in `mogwai-server/src/man.rs`,
so `mogwai man havoc` would otherwise ship prose describing divergences as
process-wide after they have become per-account. The set:

- `reference/havoc.md` - divergences are per-account, armed through the account
  header, and `GoDark`/`StallData`/`DelayAcks` scope to one slot;
- `reference/architecture.md` - the registry replaces the single engine on
  `AppState`, and the account lifecycle (auto-create, `DELETE`, idle reap);
- `reference/config.md` - the three new keys, the wall-clock basis of the
  timeout, `GET /account` as the keepalive for session-less pollers, and the
  `max_concurrent_replays` sizing note from 2.4;
- `mogwai.toml` - the new keys at their defaults, commented.

**L4 - adapter.** Stamp the header and the query param, add
`MogwaiDataClientConfig::account_id`, validate both ids with
`AccountId::parse` at client construction, enforce the `account_id` echo check
on the pulled AND pushed snapshot paths.

## 5. Verification per landing

Every command below is copy-pasteable as written.

### 5.1 L1

Gate: wire protocol serde round-trips PLUS the admission size model, which
spans protocol and engine - so the whole workspace, not one package.

    brokkr check

New tests in `crates/mogwai-protocol/src/messages.rs`:

- `account_id_round_trips_as_bare_string` - `AccountId` is `serde(transparent)`,
  so it must encode as `"ACC-1"`, not `{"0":"ACC-1"}`. This pins the byte shape
  both ends serialize against.
- `account_id_parse_rejects_empty_overlong_and_illegal` - table-driven over
  `""`, 65 bytes, `"a/b"`, `"a b"`, and accepts `"WYRD-042:BTCUSDT"` (the real
  deployment shape - strategy and symbol scope in one id).
- `account_state_carries_its_account_id` - decode a snapshot and assert the
  field survives.
- `account_state_bound_covers_a_max_length_account_id` - in `sizing.rs`: serialize
  an `AccountState` whose id is `MAX_ACCOUNT_ID_LEN` bytes of the widest
  escaping character the charset allows and assert
  `account_state_max_bytes(shape)` still dominates. This is the test that would
  catch the voided reservation; without it the envelope bump is an unchecked
  assertion.
- the existing `worst_case_reservation_covers_actual_output` in
  `mogwai-engine/src/lib.rs` is EXTENDED (not duplicated) to build its engine
  with a max-length account id, so the end-to-end domination claim samples the
  new field rather than the short fixture ids.

### 5.2 L2

Gates: execution and engine semantics (order handling, account state,
divergence injection) plus the live end-to-end path.

    brokkr check

then the live path, server first:

    brokkr run mogwai -- serve
    python3 scripts/smoke.py

New tests in `crates/mogwai-server/src/main.rs`'s test module (the existing home
of the server's axum-level tests, alongside the `state()` helper being updated):

- `two_accounts_have_independent_ledgers` - the central proof. Submit and fill a
  market order as `ACC-A`, then `GET /account` as `ACC-B`; B's balances equal the
  configured template exactly and B's positions are empty. This is the direct
  inverse of X11's "worker 112 sees worker 47's fill".
- `an_order_is_invisible_to_another_accounts_query` - `QueryOrders` and
  `QueryFills` as `ACC-B` do not report `ACC-A`'s order or fill.
- `same_client_order_id_in_two_accounts_both_accepted` - two accounts may reuse
  an id; `seen_client_order_ids` is per-engine, so neither is a duplicate. Pins
  that the dedup did not accidentally become global.
- `cancel_across_accounts_is_refused` - `ACC-B` cancelling `ACC-A`'s id gets the
  engine's unknown-order rejection, not a cancellation.
- `missing_account_header_is_rejected` - `POST /orders`, `GET /account`,
  `POST /control/divergence` each return `400` with no header and the body names
  the header.
- `ws_upgrade_without_the_account_param_is_rejected_before_handshake` - the
  `/ws` half, split out because its assertion is different in two ways: the
  failure must be an HTTP `400` with NO successful handshake (not a
  post-upgrade close), and the body must name the QUERY PARAMETER, since `/ws`
  carries no header.
- `malformed_account_header_is_rejected` - `400`, and no account is created
  (checked via `registry.len()`), so a typo cannot both fail and leak a slot.
- `a_rejected_request_with_a_valid_id_creates_no_account` - the stronger rule
  from 3.2: a well-formed id on a request refused for an unrelated reason
  leaves `registry.len()` unchanged.
- `a_failed_upgrade_leaks_no_session_count` - a `/ws` connection that fails
  after the slot is acquired leaves `sessions == 0`, pinning the `SessionLease`
  drop. A leaked count is invisible until the account proves unreapable hours
  later, so it needs its own gate.
- `replay_permits_are_unchanged_by_account_scoping` - two sockets under two
  accounts subscribe to one symbol; assert the semaphore's available permits
  match the pre-change accounting (one per subscription, not one per account).
  This is what backs 7.1's not-worse claim, which the ledger-isolation test
  does not touch.
- `go_dark_is_scoped_to_the_arming_account` - arm `GoDark` on `ACC-A`, assert
  `ACC-B`'s socket still receives its heartbeat and market data within the
  window. This is the test that would have caught the current global blackout.
- `delay_acks_is_scoped_to_the_arming_account` - same shape on the exec pump.
- `instruments_match_the_configured_profiles` - the `/instruments` re-point of
  3.6; asserts equality against `profiles.instrument_defs()`.

Focused runner for the central one while iterating:

    brokkr test -p mogwai-server two_accounts_have_independent_ledgers

`scripts/smoke.py` gains an end-to-end multi-account leg (the engine unit tests
cannot reach the real socket, and per the contract behavior neither the engine
tests nor the existing smoke test reach must be pinned by a named test): connect
two websocket sessions under distinct account ids, trade on the first, assert
the second's `/account` snapshot is untouched and that its stream carried no
execution event. The smoke test's existing single-account legs keep working by
sending the header with a fixed `SMOKE-001` id.

### 5.3 L3

Gate: execution and engine semantics plus lifecycle behavior no existing test
reaches.

    brokkr check -p mogwai-server

New tests, same module:

- `teardown_destroys_the_account_and_its_state` - trade on `ACC-A`, `DELETE`,
  then `GET /account` as `ACC-A`: the response is the fresh template, proving
  auto-creation produced a NEW slot rather than resurrecting the old ledger.
- `teardown_actually_drops_the_slot` - hold a `Weak<AccountSlot>` taken before
  the `DELETE`, close the bound session, and assert `Weak::upgrade` returns
  `None`. The test above proves the map entry is gone; this one proves the
  `Engine` and its `seen_client_order_ids` / `closed` / `fills` are actually
  reclaimed, which is the requirement from section 1 and is compatible with the
  previous test failing to hold.
- `teardown_of_unknown_account_is_404`, and `teardown_of_a_malformed_id_is_400`.
- `teardown_closes_a_data_only_session` - a socket that has subscribed to market
  data and never submitted an order receives a close whose reason names the
  teardown. Deliberately data-only: a test that happens to send an order would
  pass even under lazy discover-on-next-engine-access, which is the design this
  test exists to forbid.
- `teardown_during_an_in_flight_order_does_not_fill_a_phantom` - a `DELETE`
  racing an order returns either a normal result or `410 Gone`, never a fill
  booked into a removed engine.
- `a_reaped_account_is_not_the_one_that_was_reactivated` - the TOCTOU gate:
  reactivate an account concurrently with a reaper tick and assert either that
  it survives or that the surviving slot is a new generation with a clean
  ledger - never that a live, just-touched ledger silently vanishes.
- `idle_account_is_reaped` - with `account_idle_timeout_ms` set small in the
  test config, an account with no sessions disappears from `GET /accounts`
  after a reaper tick.
- `account_with_a_live_session_is_never_reaped` - the same wait with a socket
  held open leaves the account present. This is the D16-desert case: a quiet
  account is not a dead one.
- `new_account_beyond_max_accounts_is_refused` - `max_accounts = 2`, third id
  gets `429`; and the two existing accounts still trade fine, proving the cap
  refuses creation rather than degrading service. Also asserts the refused id
  left no slot behind.
- `an_http_only_account_stays_alive_while_polling` - the session-less-driver
  case from 3.4: with a short timeout and no websocket at all, an account whose
  driver calls `GET /account` each interval survives every reaper tick, while a
  sibling account that touches nothing is reaped. This is the HttpPolling
  transport's liveness contract and nothing else covers it.
- `zero_reap_interval_is_a_config_error` - alongside the existing `speed` /
  `sim_epoch_ns` validation tests in `config.rs`.
- `registry_holds_the_target_account_count` - create 2000 accounts through
  `acquire` and assert `len()` and a `summaries()` call both behave. Cheap
  (no sockets, no replay) and it is the only thing in this plan that touches
  the stated deployment scale at all - see 2.4 for what it does NOT prove.

`brokkr test -p mogwai-server idle_account_is_reaped --debug` is the loop to use
while iterating on the reaper: it is a timing/lifecycle test where release-LTO
build time dominates and optimization changes nothing under test.

### 5.4 L4

Gate: adapter behavior, plus the live path end to end.

    brokkr check -p mogwai-adapter

then

    brokkr run mogwai -- serve
    python3 scripts/smoke.py

New tests in `crates/mogwai-adapter/src/client/exec.rs` (which already hosts an
axum-backed fake-venue harness that binds an ephemeral port and repoints
`config.base_url` - these extend it rather than build a new instrument):

- `every_http_request_carries_the_account_header` - the fake venue records
  headers; assert the id appears on the order post, the account fetch and the
  havoc post.
- `websocket_upgrade_carries_the_account_query_param` - assert on the request
  URI the fake venue sees.
- `clock_probe_sends_no_account_header` - pins 3.7's exemption, so a clock probe
  cannot create an account.
- `account_snapshot_with_a_foreign_account_id_is_an_error` - the fake venue
  answers `fetch_account` with a different id; the client errors instead of
  adopting the balances.
- `pushed_account_state_with_a_foreign_account_id_is_rejected` - the same
  mismatch arriving as a `ServerMessage::AccountState` frame over the socket.
  Separate test because it is a separate code path (`handle_account_state`) and
  it is the one that currently stamps the configured id over whatever the wire
  said, i.e. the path where contamination would actually be silent.
- `a_config_id_illegal_to_the_venue_fails_client_construction` - a nautilus
  `AccountId` that is legal upstream but fails `AccountId::parse` (a space, a
  slash) is refused at factory time with the reason, rather than producing a
  client that 400s on every request under live trading.
- `data_client_config_round_trips_account_id` - alongside the existing config
  round-trip assertions in `factories.rs`.

### 5.5 Throughput

This spec is not justified by a throughput estimate, so no measurement gates
the landing. The one performance claim it makes is a REDUCTION in contention
(per-account mutex replacing a global one), which is structural and needs no
instrument. The data-loader path (streaming Kraken reader, k-way merge) is not
touched at all, so its O(1)-memory gate is not in scope.

## 6. Keep/revert

Each landing is reverted as a unit on its own gate. L2 is the one that can fail
non-obviously (a per-account atomic missed in the writer, contamination through
a path not enumerated in 2.1); its verdict is read off
`two_accounts_have_independent_ledgers`, the two havoc-scoping tests and the
smoke multi-account leg together. If any of those cannot be made green, L2
reverts wholesale - there is no partial keep, since a half-scoped divergence is
worse than the honest global one it replaced.

## 7. What this spec does NOT do (stopping rule)

Named and excluded, each with why:

1. **Tape multiplexing** - collapsing N subscriptions on one
   `(symbol, data_origin)` to one replay thread. X11 records it as the hard
   half. It is orthogonal: it is about SUBSCRIPTIONS and threads, not ledgers,
   and it is already reachable today by opening 2000 sockets. Its own item, and
   it inherits the per-subscriber data-havoc constraint from 3.5. This spec's
   obligation is only to not make it worse, gated by
   `replay_permits_are_unchanged_by_account_scoping` (5.2), which asserts on the
   semaphore directly - `two_accounts_have_independent_ledgers` was cited here
   in an earlier draft and proves only ledger isolation, nothing about replay
   accounting. Until multiplexing lands, the 2000-worker deployment needs
   `max_concurrent_replays` raised past its 1024 default (2.4).
2. **Enforcing the `(strategy, symbol)` account unit.** The venue takes an
   opaque `AccountId`. That the deployment encodes a strategy and a symbol scope
   in it (`WYRD-042:BTCUSDT`) is the CALLER's convention; a venue that parsed
   structure out of account ids would be a venue that breaks when the convention
   changes.
3. **Authentication.** The header is an identity, not a credential. Any account
   id may act as any account. This is a test venue on a trusted network, and
   adding auth would gate every test path behind key management for no fault
   coverage.
4. **Per-account instruments or balances.** Every account is created from one
   template. Heterogeneous funding is a real future want and a trivial follow-on
   (the template is already a struct on the registry), but nothing in the
   forward-validation gate needs it now.
5. **Cross-account matching.** Accounts do not trade against each other; each
   engine fills against the synthetic tape exactly as today.
6. **Authorization of teardown.** Anyone who can reach `DELETE /accounts/<id>`
   can destroy any account. Same reasoning as item 3: no auth in this venue.
7. **The broadarrow-side changes.** Two are required for the fleet to actually
   work end to end and neither is landed here, because they live in the
   consumer repo (which depends on this workspace, never the reverse): threading
   `account_id` into `applied_data_config` in `register_mogwai_forward` (3.7,
   without which every data client shares `MOGWAI-001`), and calling
   `DELETE /accounts/<id>` when a `(strategy, symbol)` run finishes (3.4,
   without which the idle reaper is the only teardown that ever fires). Both are
   recorded here as coordination obligations so that "multi-account works" is
   not claimed on the strength of the mogwai side alone.
8. **The D16 trade-desert fidelity problem** (`docs/bug-sweep.md` D16) and
   **AD12's missing dead-feed watchdog**. Both are aggravating context for the
   reaper design in 3.4 and are referenced there, but both are separate items.
