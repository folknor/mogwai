# Technical implementation spec: one venue per run

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. It descends from
`notes/problem-server-lifecycle.md` (RESOLVED), which is the sole source of the
decisions below; where this spec appears to decide something, it is applying
that document's ruling to concrete artifacts, and where the two disagree the
problem statement wins and this spec is wrong. The `notes/todo.md` entry naming
the item is the PROBLEM STATEMENTS bullet, first sub-bullet.

This is a FULL REWRITE of the server's lifecycle, tenancy and subscription
surface, plus the wire protocol and adapter changes that follow. It is not a
local change and it is not additive: more code is deleted than written.

## Summary of the target

One venue process serves ONE run, ONE instrument, ONE account, for an optional
declared duration, on an ephemeral port it reports back to whoever started it,
in the foreground, logging to stderr, owning no files. It is started by the run
launcher, killed by the run launcher, or it stops itself when its declared sim
duration elapses. Nothing looks it up, nothing subscribes to it, nothing
arbitrates between consumers of it, because there is only ever one.

## Survey of the ground

Reconciled against the sibling problem statements as required by point 8. The
relevant sibling facts: `notes/problem-order-book.md` establishes there is no
book, so nothing here needs to preserve matching state across a run;
`notes/problem-seeds-and-paths.md` owns `data_origin_ns` and the wall anchor, so
this spec touches the anchor only where the lifecycle forces it (the readiness
record reports the seed) and does not decide what sets the origin;
`notes/problem-instrument-model.md` owns the CONTENT of the instrument table,
so this spec changes only its CARDINALITY (list to single) and must not be read
as fixing its fields.

### What exists today, by artifact

`crates/mogwai-server/src/main.rs` (6517 lines) carries the CLI, the daemon, the
router and the bulk of the test suite.

- `Cli` / `Command` with `serve`, `stop`, `gen`, `man`. `ServeArgs` carries
  `--addr` (default `127.0.0.1:8787`), `--log-file` (default `mogwai.log`),
  `--pid-file` (default `mogwai.pid`), `--config`, `-f`. `StopArgs` carries
  `--pid-file`.
- The daemonization machinery: `PidLock`, `PidLockStatus`, `LockAttempt`,
  `WaitLock`, `PipeReady`, `Ready`, `ReadyByte`, `acquire_pid_lock`,
  `locked_inode_still_at_path`, `remove_pid_file_if_owned`, `try_lock_pid_file`,
  `is_lock_held`, `clear_pid_file`, `write_pid_into_locked_file`,
  `read_pid_from_file`, `open_existing_pid_file`, `stop`, `read_pid_when_ready`,
  `wait_for_lock_release`, `signal_pid`, `await_ready`, `read_ready_byte`,
  `redirect_stdio_to_devnull`, `print_banner`, `resolve_paths`, `resolve_path`,
  and the consts `READY_TIMEOUT`, `SHUTDOWN_GRACE`, `STOP_TIMEOUT`,
  `STOP_KILL_GRACE`, `PID_POLL_INTERVAL`, `PID_LOCK_ACQUIRE_ATTEMPTS`.
- The router: `/health`, `/account`, `/accounts`, `/accounts/{id}` (DELETE),
  `/instruments`, `/trades`, `/quotes`, `/clock`, `/orders`, `/ws`,
  `/control/divergence`.
- `init_logging` writing to a cwd-relative `mogwai.log`.

`crates/mogwai-server/src/accounts.rs` (466 lines) is the multi-tenant registry:
`AccountRegistry` with `acquire` (implicit creation on unknown id), `get`,
`destroy`, `reap_idle`, `len`, `summaries`, `wait_for_teardown`, plus
`AccountSlot` (generation, tombstone, session counting, per-account divergence
atomics, session lanes, `last_seen_ns`), `SessionLease`, `AccountTemplate`,
`AccountSummary`, `RegistryError`.

`crates/mogwai-server/src/tape.rs` (762 lines) is the sharing layer: `TapeKey`
(symbol, `data_origin_ns`, `RegimeKey`), `TapeRegistry` with refcounted
`attach`/`attach_inert` under a `Semaphore` sized by `max_concurrent_tapes`,
`TapeLease`, `TapeCapacity`, `TapeFrame`, `CursorState`, `TapeSpawn`, and the
bounded broadcast fanout ring.

`crates/mogwai-server/src/ws.rs` (1769 lines) carries `ws_upgrade`,
`AccountParams`, `handle_socket`, `Subscription`, `spawn_fanout`, `FanoutSpawn`,
`quiesce_subscription`, `quiesce_and_resume_floor`, `resume_seek_target`,
`reconcile_entry_start_ts`, `coalesce_issues`, `dedup_symbols`,
`spend_diagnostic`, plus the exec pump and heartbeat.

`crates/mogwai-server/src/config.rs` (705 lines) is `Config` with the knob set
listed in `mogwai.toml`, plus `validate_account_lifecycle`,
`validate_admission_limits`, `validate_balances`, `validate_penetration`,
`warn_unfunded_quotes`, `build_admission_limits`.

`crates/mogwai-server/src/source.rs` (973 lines) carries
`MAX_HISTORY_SEEK_TICKS = 190_000`, `CHECKPOINT_K = 8192`, and the lazy
checkpointed seek that serves history per request.

`crates/mogwai-protocol/src/messages.rs` (1578 lines) carries
`ClientMessage::Subscribe`/`Unsubscribe`, `SubscriptionEntry`,
`SubscriptionIssue` (9 variants), `SubscriptionIssues`, the symbol-list and
entry-list boundary guards, `MAX_SUBSCRIBE_SYMBOLS`, and the frame-size
reasoning anchored on the widest `SubscriptionIssues` frame.
`crates/mogwai-protocol/src/lib.rs` carries the `x-mogwai-account` header name.
`crates/mogwai-protocol/src/transport.rs` carries `TransportProfile`.

`crates/mogwai-data/src/lib.rs` carries `MergeSource` (the k-way merge), used by
`crates/mogwai-server/src/source.rs`, by
`crates/mogwai-data/examples/fill_walk_bench.rs`, and by `mogwai-data`'s own
unit tests. The example matters: it is a non-test construction site OUTSIDE the
server, and it also mirrors `MAX_HISTORY_SEEK_TICKS` in its own constants, so it
is fallout of the warmup ruling as well.

`crates/mogwai-protocol/src/transport.rs` carries `TransportProfile` with three
variants: `WsStreaming` (default), `HttpOrders` (order entry over HTTP, market
data still pushed over WS), `HttpPolling` (both over HTTP).

`crates/mogwai-adapter/src/config.rs` carries `DEFAULT_BASE_URL =
"ws://127.0.0.1:8787"`, `UNSET_ACCOUNT_ID`, and both client configs.
`client/data.rs` (3391 lines) and `client/exec.rs` (4473 lines) carry the
subscribe/unsubscribe call paths, the WS reconnect loop, and the HTTP poller.

`scripts/smoke.py` hardcodes `HOST, PORT = "127.0.0.1", 8787`, does not start a
server, and exercises `/accounts`, `/accounts/{id}` DELETE and the
`x-mogwai-account` header directly. `scripts/probe_arm_eviction.py` and
`scripts/probe_warmup_window.py` share that assumption.

Docs that assert the current shape and must move with it: `reference/cli.md`,
`reference/config.md`, `reference/architecture.md`, `reference/glossary.md`,
`reference/havoc.md`, `reference/clock.md`.

### The load-bearing parts that must NOT be dropped in the rip

Stated explicitly because the tenancy machinery and the fidelity machinery are
physically interleaved in `tape.rs` and `ws.rs`, and the problem statement flags
this as the mistake an implementer is most likely to make.

- The bounded fanout ring, the lag policy, `FeedLagged`, `fanout_depth`,
  `zero_speed_stall_ms`, and killing a connection that falls behind. All STAY.
  One client still holds two sockets onto one tape.
- The admission lanes (`exec_held_budget_bytes`, `admission_lane_frames`,
  `pending_command_acts`, `global_pending_command_acts`) STAY: they bound one
  connection's memory, not one tenant's share.
- The divergence control plane, the havoc filter, `CommandLatency`, the exec
  pump and its delay path STAY unchanged in behaviour; only their OWNER moves
  from `AccountSlot` to the process-global run state.
- `gen` STAYS, and grows in importance: the repository owner is the realism gate
  and generated tapes are what they read.
- The checkpointed seek in `source.rs` STAYS as the mechanism that materializes
  warmup; only its LAZY, per-request, budget-capped invocation goes.

## The decisions this spec makes that the problem statement left to spec time

Each is forced by a ruling above it; none is a new product decision.

1. **The readiness record is one line of JSON on an inherited fd**, selected by
   `--ready-fd <N>`. Chosen over stdout because stdout is also where a launcher
   would tee logs, and over a file because file artifacts are exactly what this
   spec removes.
2. **Both HTTP transport profiles are REMOVED**, not accelerated, and
   `TransportProfile` dies with them. The ruling was "accelerable or removed".
   `HttpPolling` is a 250 ms wall loop against a 1,000-trade page cap, it is
   structurally outrun by an accelerated dense tape, and every fix makes it a
   stream imitation that degrades further as speed rises. `HttpOrders` goes with
   it rather than surviving alone: it is the carrier the unordered-dispatch
   `notes/todo.md` item describes, nothing selects either
   (`TransportProfile::default()` is `WsStreaming` and no scenario overrides),
   and a three-variant selector reduced to one variant is not a selector.
   Removing them closes that todo item: with no HTTP order carrier there is no
   dispatch race to explain. `POST /orders` and the `GET /trades` history
   request survive as REQUEST endpoints the smoke and the history path use
   directly; it is the client-side PROFILES that die.
3. **Planned completion is announced on the wire**, as a new
   `ServerMessage::RunComplete`, followed by a WS 1000 close and process exit 0.
   The problem statement created a terminal state and required it be
   distinguishable from a crash; the adapter's reconnect loop cannot tell a
   clean exit from a death by socket behaviour alone, so the venue says so
   before it goes.
4. **Config has no cwd fallback.** `--config <path>` or built-in defaults.
   Reading `mogwai.toml` from the process cwd is the third cwd-relative artifact
   and it goes with the other two.
5. **Logs go to stderr.** The launcher owns the child's pipes and therefore owns
   where its output lands. `--log-file` is removed; choosing a per-run home is
   the operator's problem, exactly as the problem statement says.
6. **`man` is removed** with `stop`, per decision 5 of the problem statement.
   Its audience reads `reference/` and `docs/` directly. A consequence worth
   naming because it is a benefit rather than a loss: `man.rs` is the only thing
   that `include_str!`s `reference/cli.md`, `config.md`, `architecture.md`,
   `havoc.md` and `clock.md` into the binary. Deleting it decouples the durable
   docs from the build, so a doc edit stops forcing a server recompile.
7. **The earliest servable instant is named `tape_origin_ns`, and
   `data_origin_ns` keeps its current meaning.** Today `data_origin_ns =
   sim_now_at_boot - backfill_horizon_ns` (`main.rs`), which is the FLOOR of
   available history, not the start of the run. The warmup section as first
   drafted said warmup ends AT `data_origin_ns` and the floor is
   `data_origin_ns - warmup_ns`; both cannot be true of one field. Resolved by
   keeping `data_origin_ns` as the floor and introducing `run_start_ns` for the
   sim instant at which the run begins serving live. The relation is
   `data_origin_ns = run_start_ns - warmup_ns`, computed once at boot. Nothing
   here decides what sets `run_start_ns`; that is
   `notes/problem-seeds-and-paths.md`'s wall-anchor question, and this spec only
   renames a derivation that already exists.
8. **The run deadline is measured from `run_start_ns`, not from boot.** Warmup
   generation is unbounded wall work that happens BEFORE readiness, and under
   acceleration a 30 s sim run could otherwise be over before the launcher
   connects. `Run::started_ns` is set to `run_start_ns` after warmup completes
   and immediately before the readiness record is written; `deadline_ns =
   started_ns + run_duration_ns`. Sim time does not advance during warmup
   generation; the sim clock is anchored at the same moment `started_ns` is set.
9. **`--addr` with port 0 and no `--ready-fd` is a startup error.** The default
   `--addr` is `127.0.0.1:0` and `--ready-fd` is optional, so a bare
   `mogwai serve` would bind a port it reports nowhere and serve nobody. `serve`
   refuses that combination with a message naming both flags. Any explicit
   non-zero port without a ready fd is fine; the caller already knows the
   endpoint.
10. **The venue dies with its owner via an explicit parent-death watch, not by
    trusting SIGTERM.** The problem statement requires that an instance dies
    when its owner dies, and a launcher that is SIGKILLed sends nothing. The run
    installs `prctl(PR_SET_PDEATHSIG, SIGTERM)` at startup on Linux, which the
    kernel delivers when the parent thread exits regardless of how it died. This
    is a Linux-only mechanism and that is acceptable: it is the platform the
    project builds and tests on, and it is a belt to SIGTERM's braces rather
    than a replacement for it. A launcher that re-parents the child (double
    fork) defeats it, which is documented in the launcher contract as a thing
    not to do.
11. **`backfill_horizon_ns` is a wire field, and its removal is a wire change.**
    It is not only a config knob: it sits on `ClockSnapshot` in
    `mogwai-protocol/src/clock.rs`, is served by `/clock`, is read by
    `mogwai-adapter/src/client/shared.rs`, and is pinned by
    `crates/mogwai-adapter/tests/data_client_transport.rs`. It is replaced on
    the snapshot by `warmup_ns`, which carries exactly the same meaning under
    the new model (the sim span between `data_origin_ns` and `run_start_ns`), so
    the adapter's consumer changes name only. The serde round-trip for
    `ClockSnapshot` is re-pinned in the same landing.

## Target artifacts

### `mogwai-server` CLI

```rust
#[derive(Parser)]
#[command(name = "mogwai", version, long_version = LONG_VERSION)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve(ServeArgs),
    Gen(GenArgs),
}

#[derive(Args)]
struct ServeArgs {
    /// Address to bind. Port 0 (the default) takes an ephemeral port and the
    /// endpoint is reported on the ready fd.
    #[arg(long, value_name = "ADDR", default_value = "127.0.0.1:0")]
    addr: SocketAddr,

    /// Run configuration. Omitted, built-in defaults apply; there is no
    /// implicit read of a file in the working directory.
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Inherited writable file descriptor the readiness record is written to.
    /// Omitted, no record is written and the launcher is responsible for
    /// knowing the endpoint some other way (it must then pass an explicit
    /// --addr).
    #[arg(long, value_name = "FD")]
    ready_fd: Option<RawFd>,

    /// Overrides run_duration_ns from config. Accepts a humantime duration
    /// ("30s", "10m", "2h"), measured on the SIM axis.
    #[arg(long, value_name = "DURATION")]
    duration: Option<humantime::Duration>,
}
```

`serve` never forks, never writes a pid file, never redirects stdio, never
prints a banner. `-f` is removed because it is now the only mode; a launcher
passing it gets a clap error, which is the correct loud failure.

### The readiness record

Written exactly once, as one line terminated by `\n`, then the fd is closed:

```rust
/// The venue's report to whoever started it. Written to --ready-fd the instant
/// the listener is bound and the eager warmup is materialized, which is also
/// the point at which a connect will succeed and be served in full.
#[derive(Serialize, Deserialize)]
pub struct ReadyRecord {
    /// Wire schema version of this record. Bumped by any landing that adds or
    /// changes a field; see the staging note below.
    pub version: u32,
    /// The bound endpoint, host and port, e.g. "127.0.0.1:41235".
    pub addr: SocketAddr,
    /// Process id, so a launcher that loses its child handle can still signal.
    pub pid: u32,
    /// The instruments this instance serves. Exactly one from L4 onward; a
    /// list until then, because the config still carries [[instrument]].
    pub symbols: Vec<String>,
    /// The seed the tape was drawn from. See the staging note: at L1 this is
    /// the existing per-symbol FNV derivation reported honestly, NOT a run
    /// seed, because no run seed exists in the tree and
    /// notes/problem-seeds-and-paths.md has not decided one.
    pub seed: SeedReport,
    /// Sim-time floor of available history. Equals run_start_ns - warmup_ns.
    pub data_origin_ns: u64,
    /// Sim instant at which the run begins serving live, and the epoch the run
    /// duration is measured from.
    pub run_start_ns: u64,
    /// Declared sim duration, or None for indefinite.
    pub run_duration_ns: Option<u64>,
    /// Declared warmup horizon, already generated when this line is written.
    pub warmup_ns: u64,
    /// Long version string, so a launcher can record which binary produced the
    /// path without shelling out again.
    pub version_string: String,
}
```

**Staging note - this record cannot be born whole at L1, and pretending
otherwise would make L1 unbuildable.** Every field above depends on a ruling
some later landing lands: `run_duration_ns` arrives with L2, `warmup_ns` and
`data_origin_ns`'s new floor meaning with L3, the single `symbol` with L4, and a
run seed does not exist anywhere in the tree - `source.rs::seed_for(symbol)` is
an FNV hash of the symbol and `notes/problem-seeds-and-paths.md` explicitly
leaves seed granularity open as its unresolved decision 2. The resolution:

- L1 ships `version: 1` with `addr`, `pid`, `symbols`, `data_origin_ns` (its
  CURRENT meaning, which L3 does not change), `version_string`, and
  `seed: SeedReport::PerSymbolFnv(Vec<(String, u64)>)` - the truth about what
  the tape was actually drawn from today. No `warmup_ns`, no `run_duration_ns`,
  no `run_start_ns`.
- L2 adds `run_duration_ns` and bumps to `version: 2`.
- L3 adds `warmup_ns` and `run_start_ns` and bumps to `version: 3`.
- L4 replaces `symbols` with `symbol: String` and bumps to `version: 4`.
- `SeedReport` becomes a single `run_seed: u64` only when
  `notes/problem-seeds-and-paths.md` decides granularity. This spec does not
  decide it and does not block on it; reporting the derivation that actually
  exists is provenance, and reporting a run seed that does not exist is a lie.

The version field is what makes the staging safe: a launcher reads `version`
first and refuses a record it does not understand. There is one consumer and it
is developed in lockstep, so no compatibility window is owed.

This lives in `mogwai-protocol` (new module `ready.rs`, re-exported from
`lib.rs`), not in the server binary, because the launcher and any Rust consumer
must deserialize it against the same definition. It is the fourth wire contract
this crate owns and it obeys the same round-trip rule as the other three.

Failure to write it is fatal: the venue exits nonzero rather than serving a run
nobody can reach.

### Run state replaces the registries

`AccountRegistry` and `TapeRegistry` collapse into one process-global struct.

```rust
/// Everything one run owns. Constructed once at boot, never keyed, never
/// looked up, dropped when the process exits.
pub(crate) struct Run {
    /// The single instrument. Its content is owned by
    /// notes/problem-instrument-model.md; this spec fixes only that there is
    /// exactly one.
    instrument: InstrumentDef,
    profile: source::InstrumentProfile,

    /// The single ledger. No id, no generation, no tombstone, no idle clock.
    engine: AsyncMutex<Engine>,

    /// The single tape and its fanout ring. Not refcounted, not keyed: the
    /// process owns it for its whole life.
    tape: Arc<Tape>,

    /// Armed divergences, formerly per-AccountSlot atomics, now per-run.
    delay_ms: AtomicU64,
    submit_act_ms: AtomicU64,
    modify_act_ms: AtomicU64,
    cancel_act_ms: AtomicU64,
    submit_ack_ms: AtomicU64,
    modify_ack_ms: AtomicU64,
    cancel_ack_ms: AtomicU64,
    dark_until_ns: AtomicU64,
    stall_until_ns: AtomicU64,

    sim: SimClock,
    /// Sim instant the run begins serving live, set after warmup completes.
    /// The epoch every duration is measured from, per decision 8.
    started_ns: u64,
    /// Sim instant at which the run stops itself, or None for indefinite.
    /// Equals started_ns + run_duration_ns.
    deadline_ns: Option<u64>,
    /// Sim span of history generated eagerly at boot and held. The history
    /// floor is started_ns - warmup_ns, which is data_origin_ns.
    warmup_ns: u64,
    /// Fires once when the run reaches deadline_ns. Every connection task
    /// selects on its own subscriber alongside its normal work: that is how
    /// RunComplete reaches every open socket without the run holding a
    /// registry of connections. Broadcast rather than a notify because each
    /// task must see it, and a watch rather than a oneshot because tasks
    /// subscribe at arbitrary times.
    complete: tokio::sync::watch::Sender<Option<RunComplete>>,
    /// Cancels the accept loop. Signalled at the same instant as `complete`,
    /// so no connection is accepted after the announcement.
    accept_shutdown: tokio_util::sync::CancellationToken,
    /// Counts live connection tasks so the completion sequence can wait for
    /// them to drain before the process exits, bounded by SHUTDOWN_GRACE.
    live_connections: Arc<tokio::sync::Semaphore>,
}
```

The completion sequence is therefore explicit and owned: the deadline timer task
sends on `complete`, cancels `accept_shutdown`, and waits for `live_connections`
to quiesce or for `SHUTDOWN_GRACE` to expire, whichever comes first, then exits
0. Each connection task, on seeing `complete` fire, writes `RunComplete` as its
next frame, closes with WS 1000, and drops. The same path runs under SIGTERM
with `RunComplete` omitted - a signal is not a planned completion and must not
be reported as one.

`AccountSlot`, `SessionLease`, `AccountTemplate`, `AccountSummary`,
`RegistryError`, `AccountRegistry` are deleted outright, along with
`accounts.rs`. The per-connection exec lanes that `session_lanes` tracked move
onto the connection task itself, which is where they were always logically
owned; the vector existed so a divergence armed against an ACCOUNT could reach
every session on it, and with one run there is one broadcast target.

`TapeKey`, `RegimeKey`, `TapeLease`, `TapeCapacity`, `TapeRegistry`,
`attach_inert` and the tape semaphore are deleted. `Tape`, `TapeFrame`,
`CursorState`, `TapeSpawn`, the ring and `sleep_until_wall_cancellable` survive
unchanged.

### Wire protocol changes (`mogwai-protocol`)

Deleted from `ClientMessage`: `Subscribe`, `Unsubscribe`. Deleted types:
`SubscriptionEntry`, `SubscriptionIssue` and all nine variants,
`SubscriptionIssues`, `MAX_SUBSCRIBE_SYMBOLS`, `dedup_symbols`'s callers, and
the boundary guards `validate_subscribe_entries` / the symbol-list guard.

The frame-size reasoning in `messages.rs` is anchored on `SubscriptionIssues`
being the widest admission frame, and `MAX_SUBSCRIPTION_ISSUES_LISTED` is what
made that bound provable. With both gone the anchor must be re-derived:
`AdmissionRejected` becomes the widest, and `ADMISSION_FRAME_MAX_BYTES` is
recomputed and its doc comment rewritten to name the new widest frame. Leaving
the old number with a dangling justification is not acceptable.

The recomputation is not left to the implementer's judgement, because two
implementers would otherwise produce two artifacts. `ADMISSION_FRAME_MAX_BYTES`
is 8192 today, sized by a list of `MAX_SUBSCRIPTION_ISSUES_LISTED` rows. Once
the widest frame is a single `AdmissionRejected` - one client id at
`JSON_ESCAPE_FACTOR * MAX_CLIENT_ID_LEN`, one reason at `JSON_ESCAPE_FACTOR *
MAX_REASON_LEN`, one symbol at `JSON_ESCAPE_FACTOR * MAX_SYMBOL_LEN`, plus
fixed envelope - the bound drops by more than an order of magnitude. The
implementer computes the exact figure from those caps in the existing
`admission_frames_fit_their_ceiling` style, rounds UP to the next power of two,
and the test proves it. What is fixed here is the METHOD and the requirement
that the resulting constant be proven by that test rather than asserted; what is
not fixed is a number this document would be guessing at.

Deleting `MAX_SUBSCRIBE_SYMBOLS` has fallout outside `mogwai-protocol` that must
land in the same commit or the workspace does not compile:

- `mogwai-server/src/admission.rs` defines
  `ADMISSION_PROMISE_TICKETS = mogwai_protocol::MAX_SUBSCRIBE_SYMBOLS` and uses
  it again in its own test. With one replay per connection the pool is one
  ticket, so `ADMISSION_PROMISE_TICKETS` becomes a literal `1` with a doc
  comment saying why, and `config.rs`'s `admission_promise_tickets` knob and its
  non-zero validator are deleted with it.
- `mogwai-adapter/src/client/data.rs` defines
  `GENERATION_HISTORY = 4 * MAX_SUBSCRIBE_SYMBOLS` and batches with
  `.chunks(MAX_SUBSCRIBE_SYMBOLS)`. The chunking dies with the wire subscribe
  frames; `GENERATION_HISTORY` becomes a standalone `1024` with its own
  justification, since it bounds a local ring and never depended on the wire cap
  for anything but a convenient number. The two tests pinning the chunk boundary
  (`subscribe_variants_emit_subscribe_then_refcount_suppresses` and the
  1024-symbol chunking assertion) are deleted, not ported: they pin a frame that
  no longer exists.
- `mogwai-server/src/main.rs` has three tests building
  `MAX_SUBSCRIBE_SYMBOLS`-sized subscribes. Deleted with the frame.

Two `SubscriptionIssue` variants carry meaning that survives the type, and each
gets a home:

- `FeedLagged { skipped }` is the LAG policy and stays. It becomes
  `ServerMessage::FeedLagged { skipped, sim_now_ns }`, a top-level frame, since
  there is no subscription to attribute it to. The WS 1011 venue-fault close and
  `is_venue_fault()` semantics move with it.
- `ReopenGapUnfireable { at_ts }` is a havoc diagnostic and becomes
  `ServerMessage::HavocDiagnostic { .. }` alongside the existing diagnostic
  spend accounting.

The other seven (`UnknownSymbol`, `ReplayCapacity`, `StaleGeneration`,
`SeekBudgetExhausted`, `StartBeforeOrigin`, `StartAfterSimNow`,
`InvalidRegime`) describe conditions that cannot arise: there is nothing to
name, no capacity to exceed, no generation to stale, no seek budget, and the
regime is boot config. They are deleted, not relocated. `StartBeforeOrigin` and
`StartAfterSimNow` DO have a surviving analogue on the history REQUEST path,
which is covered below and is an HTTP error rather than a wire frame.

Added:

```rust
/// The run reached its declared sim duration and is stopping itself. Sent on
/// every open socket immediately before a WS 1000 close, and immediately
/// before the process stops accepting connections and exits 0. This exists so
/// a client can tell a finished run from a dead one: the adapter's reconnect
/// loop cannot distinguish them from socket behaviour alone, and a venue that
/// finished cleanly would otherwise be reported as a failed run.
RunComplete {
    sim_now_ns: u64,
    /// Sim nanos actually served, which equals the declared duration except
    /// under a signal.
    elapsed_ns: u64,
},
```

`x-mogwai-account` is deleted from `mogwai-protocol/src/lib.rs`. The `account`
query parameter on `/ws` is deleted with `AccountParams`. Requests carrying
either are served normally, ignoring them, for exactly zero releases - the
header is removed and an unknown query parameter is already ignored by axum,
which is the honest behaviour for a pre-1.0 protocol with one consumer.

`TransportProfile::HttpPolling` and `TransportProfile::HttpOrders` are deleted,
which leaves `WsStreaming` alone; a one-variant selector is not a selector, so
`TransportProfile` and `transport.rs` are deleted outright and every field
holding one is removed from both adapter configs.

`TransportProfile` lives in `mogwai-protocol`, not in the adapter, so deleting
it is a WIRE-CRATE change subject to the round-trip clause: its existing
round-trip test is deleted with the type, and the landing states that deletion
rather than leaving an orphan test to fail. Its consumers are wider than the two
configs - `mogwai-adapter/src/factories.rs`, `tests/common/mod.rs`,
`tests/adapter_smoke.rs`, `tests/havoc.rs`, `tests/data_client_transport.rs`,
and `tests/reconciliation.rs`, whose whole `fixture(transport_profile)`
parameterization collapses to a single unparameterized fixture. Any test that
existed ONLY to cover an HTTP profile is deleted; any test parameterized over
all three keeps its `WsStreaming` case as an ordinary test.

`config.rs::validate_window_deliverable` is deleted entirely, and
`validate_havoc`'s signature loses its `transport_profile` argument. Note it
refuses THREE divergences, not the two named earlier in this spec:
`DelayAcks` and `GoDark` under `orders_over_http()`, and `StallData` under
`data_by_polling()`. All three refusals go, because with only `WsStreaming` left
every one of them is deliverable. `reference/havoc.md`'s note on that refusal is
rewritten rather than left asserting a check that no longer exists.

**The client-selected `MarketRegime` loses its carrier, and that is a deliberate
capability deletion rather than an oversight.** Today the adapter arms a regime
through `HavocSpec.data` -> `shared.rs::data_regime()` ->
`SubscriptionEntry.regime`, validated at config time by the adapter's
`validate_market_regime`. Deleting `SubscriptionEntry` removes the only wire
path a client had for choosing its own regime, and making the regime boot config
is the ruling that replaces it: the regime is now chosen by whoever launches the
run, in `[regime]`, which is strictly more coherent than a per-subscription
choice under one tape. Concretely: `HavocSpec.data` is deleted from the adapter
config, `data_regime()` is deleted, the `validate_market_regime` call in
`config.rs` goes with the field, and `MarketRegime` itself STAYS in
`mogwai-protocol` because the server still parses it from `[regime]` and
`validate_market_regime` is still the validator the server uses. The adapter
tests naming `HavocSpec.data` (including
`subscribe_command_carries_data_regime`) are deleted. A run wanting a chosen
regime sets it in the venue config the launcher passes, which the launcher
writes anyway.

### Config changes (`mogwai-server/src/config.rs`, `mogwai.toml`)

Deleted knobs: `max_concurrent_tapes`, `max_subscriptions_per_connection`,
`max_accounts`, `account_idle_timeout_ms`, `account_reap_interval_ms`. Deleted
validator: `validate_account_lifecycle`.

Changed: `[[instrument]]` becomes `[instrument]`, a single table. `Config::
instruments: Vec<ConfiguredInstrument>` becomes `instrument:
Option<ConfiguredInstrument>`, defaulting to the built-in BTCUSDT profile.
`warn_unfunded_quotes` stops looping and checks one quote currency - and is
promoted from a WARN to a hard boot ERROR, because with one instrument an
unfunded quote currency means every order in the run is rejected for
insufficient balance, which is a misconfigured run rather than a caution.

Added:

```toml
# Sim-time duration of this run. 0 runs indefinitely, until the launcher kills
# the process. Non-zero: the venue serves exactly this much sim time, announces
# RunComplete, and exits 0.
run_duration_ns = 0

# Sim-time history generated eagerly at boot, before the readiness record is
# written. A history request reaching further back than this is refused with a
# precise error rather than silently served short. Requesting a two-year warmup
# and paying for it at boot is the operator's decision.
warmup_ns = 86400000000000

# Market regime for this run. Formerly per-subscription, the one knob a
# consumer picked for itself; with no subscriptions it is boot config chosen by
# whoever launches the run.
[regime]
# ... existing MarketRegime fields
```

`backfill_horizon_ns` is subsumed by `warmup_ns` and RENAMED rather than simply
deleted: it existed to bound what a client was PERMITTED to ask for against
history that did not exist yet, and with warmup declared and materialized those
are the same number. Per decision 11 this is a wire change, not a config-only
one. Concretely, in the landing that builds warmup:

- `Config::backfill_horizon_ns` becomes `Config::warmup_ns`, same default
  (86_400_000_000_000), and `scripts/probe-warmup-long.toml` is updated or
  retired with its probe.
- `ClockSnapshot::backfill_horizon_ns` (`mogwai-protocol/src/clock.rs`) becomes
  `warmup_ns`, its doc comment rewritten to say the floor is materialized rather
  than merely permitted, and the `ClockSnapshot` serde round-trip is re-pinned.
- `http.rs`'s `/clock` handler and its floor comment follow the rename.
- `mogwai-adapter/src/client/shared.rs` and
  `crates/mogwai-adapter/tests/data_client_transport.rs` (which pins the JSON
  text of the snapshot) follow the rename.
- `main.rs`'s `data_origin_ns = sim.sim_ns(now) - backfill_horizon_ns` becomes
  `data_origin_ns = run_start_ns - warmup_ns`. This is a rename of an existing
  derivation, not a new policy for the origin: the stopping rule's exclusion of
  `data_origin_ns` means this spec does not decide what anchors `run_start_ns`,
  and it must keep whatever `notes/problem-seeds-and-paths.md` decides.

### Eager warmup, and the death of `MAX_HISTORY_SEEK_TICKS`

At boot, before the readiness record is written, the run generates `warmup_ns`
of tape ending at `run_start_ns` and holds it; its earliest instant is
`data_origin_ns = run_start_ns - warmup_ns`. `source.rs` keeps the checkpointed
generator and `CHECKPOINT_K`; it loses `MAX_HISTORY_SEEK_TICKS` and the
per-request seek budget, and `SubscriptionIssue::SeekBudgetExhausted` and its
HTTP analogue go with them.

`crates/mogwai-data/examples/fill_walk_bench.rs` mirrors both constants in its
own `CHECKPOINT_K` and `MAX_SEEK_TICKS = 190_000`, with a comment saying it
mirrors the server. That comment goes stale the moment the server's budget dies,
so the example's constant is renamed to `BENCH_SEEK_TICKS` and its comment
rewritten to say it is the BENCH's own walk length with no server counterpart.
The example is not deleted: it exercises the offline fill-walk lineage, not the
server's history path.

`GET /trades?start=..&end=..` gains exact refusals in place of the silent short
serve: a start before `data_origin_ns` returns 400 with a body naming the
earliest servable instant, and an end after sim-now returns 400 naming sim-now.
The previous behaviour - an exhausted seek returning an empty 200 - is the
silent failure the ruling exists to remove, and a test pins that it is now a
400.

The seek-budget probes retire with the budget. `scripts/probe_warmup_window.py`
and `scripts/probe-warmup.toml` / `scripts/probe-warmup-long.toml` exist to
measure how far a lazy per-request seek could reach before exhausting its
budget; with warmup materialized eagerly there is no window to probe and no
budget to exhaust. They are DELETED in the warmup landing rather than ported,
along with `scripts/probe-warmup.log`. `scripts/probe_arm_eviction.py` survives
- it probes divergence arming, not history - and is updated for the endpoint
change only.

Memory is now proportional to `warmup_ns` times cadence, which is real and is
the operator's declared cost. It is not a resource-cost decision input per the
user's standing instruction; it is recorded so it is not rediscovered.

### Subscription retirement, server side

`handle_socket` no longer waits for a `Subscribe`. On upgrade it immediately
attaches a fanout cursor at sim-now to the single tape and begins streaming.
`Subscription`, `FanoutSpawn`, `quiesce_subscription`,
`quiesce_and_resume_floor`, `resume_seek_target`, `reconcile_entry_start_ts`,
`coalesce_issues`, `dedup_symbols` and `spend_diagnostic`'s per-subscription
accounting are deleted; `spawn_fanout` survives with its arguments reduced to
the connection's lanes and the tape cursor.

`admission_promise_tickets` was sized at `MAX_SUBSCRIBE_SYMBOLS` and pooled one
promise per live replay on a connection. With one replay per connection the pool
is one ticket and the knob is deleted; the promise itself STAYS, because it
still reserves room for a refusal frame behind a healthy stream.

### Subscription retirement, adapter side

`crates/mogwai-adapter/src/client/data.rs` still receives nautilus's subscribe
and unsubscribe calls and must keep implementing them - that is nautilus's
client model and it does not go away. It satisfies them LOCALLY: the subscribe
records the instrument in a local set and returns; the WS reader forwards
arriving data to the message bus if the corresponding set is non-empty and the
symbol matches the one instrument. No frame is sent to the venue. A subscribe
for an instrument other than the run's is an ERROR logged and refused locally,
because the venue cannot serve it and silence would be the 2026-08-02 misbinding
defect in a new place.

**This covers all three subscription kinds, not just trades.** The shared wire
path today serves quotes, trades AND bars (`data.rs`'s `subscribe_quotes`,
`subscribe_trades`, `subscribe_bars` and their three unsubscribes all funnel
through `subscribe_symbol` / `unsubscribe_symbol` with a `SubKind`). Each keeps
its local semantics:

- `subscribe_quotes` / `unsubscribe_quotes` - local `SubKind::Quote` set, gating
  quote forwarding. The venue pushes quotes unconditionally from L5 on.
- `subscribe_trades` / `unsubscribe_trades` - as above for `SubKind::Trade`.
- `subscribe_bars` / `unsubscribe_bars` - the bar aggregation and its REFCOUNT
  stay exactly as they are. The refcount was never about the wire: several bar
  specs over one symbol collapse to one underlying stream, and that is still
  true when the stream arrives unbidden. What goes is only the emission of a
  wire frame on the first acquire and the last release.
  `unsubscribe_bars_flushes_completed_window_but_not_in_progress` STAYS and must
  still pass; `unsubscribe_emits_only_on_last_release` is rewritten to assert
  the refcount reaches zero rather than that a frame was emitted.
- `subscribe_instruments` / `subscribe_instrument` and their unsubscribes are
  already local no-ops against the instrument cache and are unchanged.

The wrong-instrument refusal applies identically to all three kinds.

The reconnect loop changes in one place: on receiving `RunComplete`, or on a WS
1000 close, the client sets a `run_complete` flag, drains, and does NOT
reconnect. On any other close it reconnects exactly as today. Both clients
(`data.rs` and `exec.rs`) carry this independently, since they hold independent
sockets and each will see the frame.

`DEFAULT_BASE_URL` is deleted from `mogwai-adapter/src/config.rs`, and
`base_url` becomes REQUIRED on both configs: a default endpoint is precisely the
mechanism by which two runs silently share a venue, and the launcher always
knows the real one. `validate()` on both configs fails on an empty `base_url`
with a message naming the readiness record as the source. The three hardcoded
copies of `127.0.0.1:8787` all go: the server's `--addr` default is now
`127.0.0.1:0`, the adapter's default is deleted, and `smoke.py` learns the
endpoint from the readiness line.

`account_id` STAYS on both configs, including `UNSET_ACCOUNT_ID` and
`validate_account_id`. It is no longer a venue key - the venue has one ledger
and ignores it - but nautilus requires an `AccountId` to construct an account
and to route its own events, and the 2026-08-02 defect came from a config that
silently supplied one, so the loud refusal keeps earning its place. Its doc
comment is rewritten to say it is a nautilus-side label with no venue meaning.

### The launcher contract

mogwai does not ship the launcher; broadarrow does. What mogwai owes is a
documented, testable contract, which `scripts/smoke.py` becomes the reference
implementation of:

1. Create a pipe. Spawn `mogwai serve --config <path> --ready-fd 3` with the
   write end at fd 3 and stderr captured. Spawn it as a DIRECT child: the
   parent-death watch of decision 10 fires on the immediate parent, so a double
   fork or any re-parenting defeats it and re-creates the orphaned-venue defect
   this spec exists to remove.
2. Read one line from the read end. A closed pipe without a line means the
   venue failed to boot; the child's stderr and exit status say why. Note the
   read can block for as long as warmup generation takes, which is proportional
   to `warmup_ns` and cadence; a launcher wanting a bound sets its own timeout
   and treats expiry as a boot failure.
3. Parse `ReadyRecord`, checking `version` first and refusing a record it does
   not understand. Use `addr` for both clients' `base_url`.
4. Run. On `RunComplete` the child exits 0 on its own; on any other need to
   stop, SIGTERM the child and reap it. If the launcher dies without doing so,
   the venue's parent-death watch stops it anyway.

This goes in `reference/cli.md`, which is rewritten around it, and the four
steps are what the smoke test executes.

### `scripts/smoke.py` grows a real argument surface

The smoke test dispatches today on bare mode words in `sys.argv[1:]`
(`main_default`, `main_heartbeat`, `main_accelerated`, `main_admission`,
`main_command_latency`, `main_penetration`), with no flags at all. Every gate in
this spec that reads `python3 scripts/smoke.py --config <path>` or
`--duration 30s` therefore presumes a surface that must itself be built, and a
gate command that cannot be pasted into a shell is not a gate.

So L1 builds it: `argparse` with a positional MODE defaulting to `default`
(preserving every existing word), plus `--config PATH` forwarded to the spawned
venue and `--duration DURATION` forwarded as `--duration`. The existing
`scripts/smoke-*.toml` files become values for `--config` rather than
mode-implied constants, and the mode words that only existed to select a config
(`accelerated`, `heartbeat`, `admission`, `command-latency`) keep working by
each defaulting `--config` to its own file. This is stated as part of L1's build
rather than assumed, because L1's own live gates are written in the new form.

## Landings

Each is one coherent, fully intrusive change, landed and then kept or reverted
on its gate. The order is chosen so the suite is green at every boundary. No
landing is a flagged probe and none leaves a switch behind.

### L1 - Foreground process, ephemeral port, readiness record

Rip: the entire daemon block listed in the survey, `stop`, `StopArgs`, `man`,
`man.rs`, `man/render.rs`, `NO_COLOR`, the pid consts, `print_banner`,
`init_logging`'s file sink and `--log-file`, `resolve_paths`, `resolve_path`,
the cwd `mogwai.toml` fallback in `Config::load`.

Build: the `ServeArgs` above, `ReadyRecord` version 1 in
`mogwai-protocol::ready` (per the staging note: `addr`, `pid`, `symbols`,
`data_origin_ns`, `version_string`, `SeedReport::PerSymbolFnv`), the
bind-then-report sequence, the port-0-without-ready-fd refusal of decision 9,
the `PR_SET_PDEATHSIG` parent-death watch of decision 10, stderr logging via
`EnvFilter` (`RUST_LOG` behaviour unchanged), SIGTERM handling that keeps
`SHUTDOWN_GRACE` and drops the rest, and `scripts/smoke.py`'s argparse surface.

Gates:

- `brokkr check --gate`
- `brokkr test -p mogwai-protocol ready_record_round_trips` - MANDATORY per the
  wire-protocol clause. `ReadyRecord` is a `mogwai-protocol` type and a launcher
  in another language parses it, so its byte form is pinned from birth, and the
  test asserts the exact JSON of a version-1 record rather than only
  `from(to(x)) == x`.
- `brokkr test -p mogwai-server ready_record_reports_the_bound_ephemeral_port --debug`
- `brokkr test -p mogwai-server two_concurrent_venues_bind_distinct_ports --debug`
- `brokkr test -p mogwai-server serve_exits_nonzero_when_the_ready_fd_is_unwritable --debug`
- `brokkr test -p mogwai-server serve_refuses_ephemeral_port_without_a_ready_fd --debug`
- `brokkr test -p mogwai-server sigterm_stops_the_venue_within_the_shutdown_grace --debug`
- `brokkr test -p mogwai-server venue_dies_when_its_launcher_is_killed_without_cleanup --debug`
  (spawns an intermediate process that spawns the venue, SIGKILLs the
  intermediate, and asserts the venue is reaped rather than orphaned - the
  decision 10 mechanism, tested the only way it can be)

None of these exist and all are bricks of this landing, laid before the code
they gate. `--debug` is correct for every subprocess test per the brokkr note.
The existing pid-file and daemonize tests in `main.rs` are deleted, not ported;
deleting a test for deleted behaviour is not a coverage loss and the spec says
so explicitly so a reviewer does not read it as one.

`scripts/smoke.py` is updated in this landing to spawn its own venue and read
the endpoint, because after L1 there is no fixed port for it to connect to. Its
`HOST, PORT` constants are replaced by the parsed record. Same for
`scripts/probe_arm_eviction.py` and `scripts/probe_warmup_window.py` - the
latter is updated here and DELETED at L3, which is deliberate: L3 is where its
subject disappears, and leaving it broken across two landings would break the
green-at-every-boundary rule.

Live gate: `python3 scripts/smoke.py` - which now starts its own server, so the
two-step "launch then smoke" instruction in `reference/cli.md` and in the
smoke's own docstring is replaced by this single command. Also
`python3 scripts/smoke.py --config scripts/smoke-accelerated.toml` and the
heartbeat, admission and command-latency variants, all of which stop needing a
separately started server.

### L2 - Declared run duration and clean completion

Depends on L1 (the process must be foreground-owned before it can own its exit).

Build: `run_duration_ns` in config, `--duration`, `Run::started_ns`,
`Run::deadline_ns`, `Run::complete`, `Run::accept_shutdown`,
`Run::live_connections`, `ServerMessage::RunComplete`, the completion sequence
(announce on every socket, WS 1000, stop accepting, drain, exit 0), and the
adapter's `run_complete` flag in both clients. `ReadyRecord` gains
`run_duration_ns` and bumps to version 2.

This is the landing that introduces `Run` itself, as a struct holding only the
completion state; L4, L5 and L6 move the instrument, the tape and the ledger
onto it. The earlier draft used `Run::deadline_ns` at L2 while claiming `Run` was
built at what is now L6, which was an ordering error: a struct that later
landings ADD FIELDS TO has to exist before they can.

At L2 there is no warmup, so `started_ns` is the sim instant at listener bind.
L3 moves it to the post-warmup instant per decision 8, and L3's slow-start gate
is what pins that move.

Gates:

- `brokkr check --gate`
- `brokkr test -p mogwai-protocol run_complete_round_trips` - MANDATORY per the
  wire-protocol clause; `RunComplete` is a new `ServerMessage` variant and its
  byte form is pinned here, not deferred to L5. The earlier draft parked all
  serde work in the subscription-retirement landing, which would have shipped a
  wire variant across three landings with no pinned form.
- `brokkr test -p mogwai-server venue_announces_run_complete_and_exits_zero_at_the_declared_sim_deadline --debug`
- `brokkr test -p mogwai-server run_complete_reaches_every_open_socket --debug`
  (two sockets open, both see the frame, both see WS 1000 - the `watch` fanout
  is the thing under test)
- `brokkr test -p mogwai-server sigterm_closes_without_announcing_run_complete --debug`
  (a signal is not a planned completion; this pins that the two paths stay
  distinguishable, which is the whole reason `RunComplete` exists)
- `brokkr test -p mogwai-adapter run_complete_stops_the_reconnect_loop --debug`
  (a new socket-backed test in `crates/mogwai-adapter/tests/`, therefore
  `#[ignore]`d like its siblings and therefore reachable only via
  `brokkr check --gate` or the focused runner - which is exactly why the gate
  form is mandatory here)
- `python3 scripts/smoke.py --duration 30s`, a new smoke mode asserting the
  child exits 0 and the adapter reports completion rather than failure.

Keep/revert reading: a run that completes must exit 0 with no reconnect attempt
logged. A reconnect attempt after `RunComplete` is a revert.

### L3 - Eager warmup, and the death of the seek budget

Depends on L2 (`started_ns` moves to the post-warmup instant, so the deadline
epoch must already exist to be moved).

This landing was missing entirely from the first draft: the eager-warmup ruling
was specified in prose and assigned to no landing, built by nothing and gated by
nothing, while being the ruling the problem statement singles out as removing a
silent failure. It is the largest brick in this spec.

Rip: `MAX_HISTORY_SEEK_TICKS` and every use of it in `source.rs`, `tape.rs`,
`http.rs` and `main.rs`; the per-request seek budget and the `BoundedSeek`
pricing built on it; `SubscriptionIssue::SeekBudgetExhausted` and its HTTP
analogue; the empty-200 short serve on `GET /trades`;
`scripts/probe_warmup_window.py`, `scripts/probe-warmup.toml`,
`scripts/probe-warmup-long.toml` and `scripts/probe-warmup.log`.

Build: `Config::warmup_ns` replacing `backfill_horizon_ns`;
`ClockSnapshot::warmup_ns` replacing `backfill_horizon_ns` with its consumers in
`http.rs` and `mogwai-adapter/src/client/shared.rs` renamed; `Run::warmup_ns`
and `Run::started_ns` set after generation; the boot-time generation of
`warmup_ns` of tape ending at `run_start_ns`, held for the process's life;
`data_origin_ns = run_start_ns - warmup_ns`; the two `GET /trades` 400s;
`ReadyRecord` gaining `warmup_ns` and `run_start_ns` at version 3; and
`fill_walk_bench.rs`'s constant renamed to `BENCH_SEEK_TICKS` with its comment
rewritten.

Gates:

- `brokkr check --gate`
- `brokkr test -p mogwai-protocol clock_snapshot_round_trips` - MANDATORY: the
  `backfill_horizon_ns` to `warmup_ns` rename is a wire change on
  `ClockSnapshot`, and `crates/mogwai-adapter/tests/data_client_transport.rs`
  pins the literal JSON text of that snapshot, so both move together or the
  adapter tests fail on a field name.
- `brokkr test -p mogwai-server trades_before_the_history_floor_are_refused_with_400 --debug`
  (the named replacement for the empty-200 silent short serve, and the direct
  gate on the ruling)
- `brokkr test -p mogwai-server trades_after_sim_now_are_refused_with_400 --debug`
- `brokkr test -p mogwai-server the_full_warmup_span_is_servable_at_readiness --debug`
  (a request for the earliest servable instant returns data, not a refusal -
  this is what proves warmup was actually MATERIALIZED rather than merely
  declared)
- `brokkr test -p mogwai-server a_short_accelerated_run_is_not_over_before_it_is_ready --debug`
  (decision 8: a run with a large `warmup_ns`, a high speed and a small
  `run_duration_ns` must still serve its full declared duration after readiness.
  Without the post-warmup epoch this test fails, which is exactly the point)
- `python3 scripts/smoke.py` and
  `python3 scripts/smoke.py --config scripts/smoke-accelerated.toml`

Keep/revert reading: memory is now proportional to `warmup_ns` times cadence and
boot wall time rises with it. That is the operator's declared cost and not a
revert trigger. A revert trigger is a readiness line that arrives before the
declared warmup is servable.

### L4 - One instrument, one tape

Depends on L3 (there is one warmed tape to make singular, and `ReadyRecord`'s
`symbols` list collapses to a single `symbol` here at version 4).

Rip: `TapeKey`, `RegimeKey`, `TapeRegistry`, `TapeLease`, `TapeCapacity`,
`attach_inert`, the tape semaphore, `max_concurrent_tapes`, `MergeSource` from
`mogwai-data` and its `TickSource` composition, `[[instrument]]` as a list.

Build: `[instrument]` as a single table, `Run::tape` as a single owned tape,
`[regime]` as boot config, `warn_unfunded_quotes` promoted to a boot error.

`MergeSource` is NOT deleted from `mogwai-data`. Its construction sites are
`crates/mogwai-server/src/source.rs`, `crates/mogwai-data/examples/
fill_walk_bench.rs`, and `mogwai-data`'s own tests, the latter two being the
offline `TickSource` lineage alongside `KrakenCsvSource`. What goes is only the
SERVER's use of it - `source.rs` builds one tape from one `GeneratedSource` with
no merge - and the example keeps compiling and keeps its merge. Deleting an
offline-analysis path to satisfy a server change would be the wrong rip, and
this paragraph exists so nobody performs it. `cargo`'s example targets are built
by `brokkr check`, so a rip that broke the example would be caught, but the gate
below names it explicitly anyway.

Gates:

- `brokkr check --gate`
- `brokkr test -p mogwai-server a_config_naming_two_instruments_fails_to_parse --debug`
- `brokkr test -p mogwai-server an_unfunded_quote_currency_refuses_boot --debug`
- `brokkr test -p mogwai-data memory_source_replays_in_time_order` (existing;
  proves `mogwai-data` is untouched by the server's un-merging)
- `python3 scripts/smoke.py` and the accelerated variant.

The data-loader gate from the standing contract is discharged rather than run:
`MergeSource`, `KrakenCsvSource` and the streaming reader are not modified by
this landing, so the O(1)-memory-over-multi-GB-files property has nothing to
regress against, and the existing `mogwai-data` tests passing is the whole
evidence needed. This is stated rather than silently skipped because the
contract requires every gate be either run or explicitly discharged with a
reason.

### L5 - Subscription retirement

Depends on L4 (there must be exactly one tape before a connection can be
implicitly attached to it).

Rip: the protocol types listed above, `handle_socket`'s subscribe wait,
`Subscription`, `FanoutSpawn`'s subscription fields, the quiesce and
resume-floor machinery, `reconcile_entry_start_ts`, `coalesce_issues`,
`dedup_symbols`, `max_subscriptions_per_connection`,
`admission_promise_tickets`, `AccountParams`.

Build: implicit attach on upgrade, `ServerMessage::FeedLagged`,
`ServerMessage::HavocDiagnostic`, the recomputed `ADMISSION_FRAME_MAX_BYTES`
with its rewritten justification, and the adapter's local satisfaction of
`subscribe_trades`.

Gates:

- `brokkr check --gate`
- serde round-trip tests are MANDATORY here per the wire-protocol clause: a new
  `mogwai-protocol` test pinning the full post-rip `ClientMessage` and
  `ServerMessage` byte forms, and the deletion of the round-trips for the
  removed variants.
  `brokkr test -p mogwai-protocol client_and_server_messages_round_trip`
- `brokkr test -p mogwai-server a_connection_receives_the_tape_without_asking --debug`
- `brokkr test -p mogwai-server a_slow_connection_is_dropped_with_feed_lagged --debug`
  (the lag policy must be re-pinned against the new frame; this is the landing
  where the stays/goes split is verified rather than asserted)
- `brokkr test -p mogwai-adapter data_client_transport` variants via
  `brokkr check --gate`
- `python3 scripts/smoke.py` - which loses its subscribe frames entirely, a
  substantial rewrite of `ws_roundtrip` and its callers.

### L6 - Single ledger

Depends on L5 (`AccountParams` is on the upgrade path).

Rip: `accounts.rs` entirely, `/accounts`, `/accounts/{id}` DELETE, the
`x-mogwai-account` header and its constant, `max_accounts`,
`account_idle_timeout_ms`, `account_reap_interval_ms`,
`validate_account_lifecycle`, the reaper task, generation and tombstone
handling, `session_lanes`.

Build: `Run` as specified, with the divergence atomics moved onto it and the
exec lanes owned by the connection task. `/account` survives and reports the one
ledger.

Gates:

- `brokkr check --gate`
- `brokkr test -p mogwai-server two_connections_share_one_ledger --debug`
- `brokkr test -p mogwai-server an_armed_divergence_reaches_every_connection --debug`
  (this is the 2026-08-02 defect's regression test, stated positively: the
  divergence that was diverted onto an auto-created `MOGWAI-001` slot must now
  reach the market-data socket, because there is no other slot to divert to)
- `python3 scripts/smoke.py` - loses its `/accounts` listing, its DELETE and its
  header entirely; the account-eviction assertions are deleted, not ported.

### L7 - HTTP transport profile removal

Independent of L2-L6; ordered last because it is the smallest. It is NOT
adapter-only, which the first draft claimed: `TransportProfile` lives in
`mogwai-protocol`, so this is a wire-crate landing and the round-trip clause
applies to it.

Rip: `TransportProfile` and `transport.rs` (including its round-trip test), the
poller in `data.rs`, the HTTP order dispatch in `exec.rs` (`dispatch_order`'s
`get_runtime().spawn` path), `config.rs::validate_window_deliverable` with all
THREE of its refusals (`DelayAcks` and `GoDark` under `orders_over_http()`,
`StallData` under `data_by_polling()`), `validate_havoc`'s `transport_profile`
argument, `POLL_INTERVAL`, the `transport_profile` field on both adapter
configs, and the `fixture(transport_profile)` parameterization in
`tests/reconciliation.rs`. `POST /orders` and `GET /trades` survive as request
endpoints used by the smoke and by history.

Gates:

- `brokkr check --gate`
- `brokkr test -p mogwai-adapter havoc` via the gate form (the deliverability
  refusal tests are deleted with the check they gate)
- `brokkr test -p mogwai-adapter reconciliation` via the gate form (its fixture
  loses a parameter; this proves the surviving `WsStreaming` cases still pass
  rather than silently vanishing with the parameterization)
- `brokkr test -p mogwai-adapter adapter_smoke` via the gate form
- `python3 scripts/smoke.py`

On landing, delete the `notes/todo.md` entry beginning "WRITE UP, then delete
this entry: why unordered HttpOrders dispatch is fidelity rather than a defect".
Its subject no longer exists. Deleting it is part of this landing's diff, per
the standing rule that a completed item is removed entirely.

### L8 - Documentation reconciliation

Landed WITH L7, not after it: the standing rule forbids a markdown-only commit,
and every doc below asserts something that L1-L7 made false.

A note on why the durable docs are allowed to be stale for seven landings, since
it looks like a contract violation and is not. The no-markdown-only-commit rule
and the must-be-true rule for `reference/` pull in opposite directions, and the
resolution is that each landing carries the doc edits its OWN diff makes false,
in its own commit, whenever there are any: L1 takes `reference/cli.md`'s daemon,
`stop` and `man` sections, L3 takes `reference/clock.md`'s
`backfill_horizon_ns`, and so on. What L8 carries is the residue - the
cross-cutting rewrites (architecture's tenancy narrative, the glossary's
identity chain) that no single landing owns and that would be rewritten three
times if split. The list below is therefore the residue plus a final sweep, not
the whole of the doc work.

- `reference/cli.md` - rewritten around `serve` and `gen`, the readiness record,
  and the four-step launcher contract. `stop` and `man` sections deleted.
- `reference/config.md` - the deleted knobs removed, `run_duration_ns`,
  `warmup_ns` (including its `backfill_horizon_ns` lineage, so an operator with
  an old file knows what to rename), `[instrument]` and `[regime]` documented,
  `admission_promise_tickets` removed, and the "the balances template is
  explicitly NOT per-account" sentence deleted along with the concept.
- `reference/architecture.md` - the tenancy, subscription and tape-sharing
  sections rewritten; the HTTP-dispatch race disclosure deleted with the path;
  the "a stop-market protective leg cannot be forward-tested" note left intact
  (it belongs to `notes/problem-refused-order-types.md`, not here).
- `reference/glossary.md` - the identity chain loses account, session and
  subscription as distinct links; process and tape remain. Its discrepancy
  register is re-read and any entry this spec resolves is removed entirely.
- `reference/havoc.md` - the deliverability-refusal note and the operator note
  about arming per-account divergences are rewritten for one run.
- `reference/*.md` generally - none of these files is `include_str!`ed into the
  binary any more, because `man.rs` was the only thing doing that and it died at
  L1. A doc-only edit therefore no longer forces a server recompile, which is
  worth one sentence in `reference/cli.md` where the `man` section used to be.
- `reference/clock.md` - the `backfill_horizon_ns` field is renamed to
  `warmup_ns` throughout with its new materialized meaning (this part lands with
  L3, not here), and the "Restarts" section describing `wall_anchor_ns`
  letting a restarted venue resume is deleted: there is no restart.
  `wall_anchor_ns` itself is owned by `notes/problem-seeds-and-paths.md` and is
  NOT removed by this spec, only its restart justification.
- `notes/todo.md` - the `problem-server-lifecycle.md` sub-bullet is removed
  entirely and `notes/problem-server-lifecycle.md` is deleted, per the file's
  own rule. Anything in it that must endure is already in `reference/` by this
  landing. The `MAX_HISTORY_SEEK_TICKS` mention in the hardcoded-value
  inventory, the three-way `127.0.0.1:8787` coupling entry, and the
  `max_concurrent_tapes` / `max_subscriptions_per_connection` lines in the
  `mogwai.toml` inventory are corrected in the same edit.

Gate: `brokkr check --gate` (gremlins runs over markdown too), plus a read-back
that no surviving `reference/` sentence describes a deleted artifact - grep for
`mogwai.pid`, `mogwai stop`, `mogwai man`, `x-mogwai-account`, `Subscribe`,
`max_concurrent_tapes`, `max_accounts`, `max_subscriptions_per_connection`,
`admission_promise_tickets`, `backfill_horizon_ns`, `TransportProfile`,
`HttpPolling`, `MAX_HISTORY_SEEK_TICKS`, `--log-file`, `8787` across
`reference/`, `docs/` and `scripts/` and confirm every remaining hit is
deliberate.

## Stopping rule

This spec stops at the boundary of the run. Explicitly OUT of scope, and named
so the teardown does not drift into them:

- What anchors `run_start_ns`, and seed policy generally
  (`notes/problem-seeds-and-paths.md`). This spec renames the derivation of
  `data_origin_ns` from `backfill_horizon_ns` to `warmup_ns` without changing
  what it means or what anchors it, and REPORTS whatever seed the tape was
  actually drawn from - today the per-symbol FNV derivation, tomorrow whatever
  that document decides. It does not invent a run seed.
- The content of the instrument table - multiplier, tick value, expiry, margin,
  fees, session envelope (`notes/problem-instrument-model.md`). This spec
  changes cardinality only.
- The fill model, the fill band, and order types
  (`notes/problem-order-book.md`, `notes/problem-refused-order-types.md`).
- Trade cadence and the realism gate's anchors
  (`notes/problem-trade-cadence.md`). Note the interaction: eager warmup's
  memory cost is proportional to cadence, and if cadence rises by the order of
  magnitude that document contemplates, the warmup cost rises with it. That is a
  consequence to be priced there, not a reason to keep lazy history here.
- The dead-feed watchdog and the terminal-venue-fault decision, both outside the
  problem set by ruling.
- The launcher itself, which is broadarrow's. mogwai owes the contract and the
  reference implementation in `scripts/smoke.py`, nothing more.
- Whether 200 instances fit on the machine. Not a design input.

## Review disposition

Two independent reviews were run against the first draft of this document
(`notes/server-lifecycle-spec-review-1.md`, an Agent read, and
`notes/server-lifecycle-spec-review-2.md`, a codex gpt-5.6-sol read). Every
finding in both was validated against the tree before being folded in. What
follows records where each landed, so a later reader does not re-litigate them,
and names the two that were rejected with the reason.

### Folded in

R1-1 / R2-1, eager warmup had no landing - now L3, the largest single addition
to this document. R1-2 / R2-8, `ReadyRecord` demanded fields that do not exist
yet, notably a run seed - resolved by the staging note and `SeedReport`. R1-3 /
decision 11, `backfill_horizon_ns` is a wire field, not a config knob - its
rename is now specified across `clock.rs`, `/clock`, `shared.rs` and the
adapter's JSON-pinning test. R1-4 / R2-9, wire variants without serde gates -
`ReadyRecord` at L1, `RunComplete` at L2, `ClockSnapshot` at L3 and
`TransportProfile`'s deletion at L7 all now carry named round-trip gates. R1-5,
L7 is not adapter-only and the deliverability check refuses three divergences,
not two. R1-6, the client-selected `MarketRegime` loses its carrier - now an
explicit capability deletion with named artifacts. R1-7, `MAX_SUBSCRIBE_SYMBOLS`
fallout in `admission.rs`, `config.rs` and `data.rs`. R1-8, `MergeSource` has a
second non-test consumer in `fill_walk_bench.rs`, which also mirrors
`MAX_HISTORY_SEEK_TICKS`. R1-9, the warmup probes are retired at L3. R1-10, the
smoke test has no flag surface - L1 builds one. R1's three smaller notes: the
`include_str!` decoupling is now a stated benefit of removing `man`, and
`ADMISSION_FRAME_MAX_BYTES` now fixes the recomputation METHOD so two
implementers converge. R2-2, owner death - decision 10, `PR_SET_PDEATHSIG`, with
a test that SIGKILLs the launcher. R2-3, completion had no state to be
implemented with - `Run` now carries the watch, the accept token and the
connection counter, and is built at L2 rather than being used three landings
before it exists. R2-4, `data_origin_ns` carried two incompatible meanings -
decision 7 splits `run_start_ns` out and keeps the existing meaning. R2-5, the
duration epoch was undefined - decision 8, measured from post-warmup
`started_ns`, gated by an accelerated slow-start test. R2-6, a bare
`mogwai serve` was unreachable - decision 9 refuses it. R2-7, adapter
subscription retirement covered only trades - quotes and bars now have stated
local semantics and the bar refcount is explicitly preserved.

### Rejected

**R1's reading of the `x-mogwai-account` compatibility paragraph** as resting on
an implicit fact. It observed that "axum ignores unknown query parameters" holds
only because `AccountParams` is deleted rather than made deny-unknown. True, but
the paragraph already states that `AccountParams` is deleted in the sentence
before, so the fact is stated rather than implicit and nothing needs adding. No
edit made.

**R2-9's complaint that L8 defers documentation reconciliation** past the
landings that falsify the docs. The premise is right that a `reference/` file
must not assert something false, but the remedy it proposes - a doc commit per
landing - collides with the standing rule against markdown-only commits only if
the doc edits are split OUT of the landing commits, which was never the
proposal. The document now states the actual policy explicitly: each landing
carries the doc edits its own diff falsifies, in its own commit, and L8 carries
only the cross-cutting residue. So the finding produced a clarification rather
than the restructuring it asked for, and its literal recommendation is declined.
