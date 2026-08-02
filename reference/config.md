# mogwai run config

The `mogwai` binary takes its run knobs from a TOML file, not from the
ambient environment. This is the reference for that file; see `reference/cli.md`
for how the binary finds it (`serve --config <path>`, default `mogwai.toml` in
the working directory) and the rest of the command-line surface.

These are server run knobs: the simulated clock, replay pacing, optional server
heartbeat, and optional instrument profiles. They are distinct from the
per-venue havoc configuration broadarrow constructs on the adapter side
(`HavocSpec`, including `ConnHavoc.heartbeat_interval_ms`, which is a *client*
ping, not this server heartbeat); see `reference/havoc.md` for that. For the
affine simulated-clock model these `sim_epoch_ns` / `speed` knobs drive, and the
full table of every temporal quantity that scales with `speed` and its wall
lower bound, see `reference/clock.md`.

## Loading

The server reads the config once at startup:

- The path is `--config <path>` when passed, otherwise `mogwai.toml` in the
  working directory.
- A missing file yields built-in defaults, so the server still starts with no
  config present.
- A malformed file is a hard error, not a silent fallback.
- Unset keys fall back to their defaults individually (the struct is
  `#[serde(default)]`), so a partial file only overrides the keys it names.

The built-in defaults equal the committed `mogwai.toml`, so a checkout serves a
realistic paced live feed out of the box.

## Keys

| Key | Type | Default | Meaning |
| --- | --- | --- | --- |
| `sim_epoch_ns` | integer ns | `0` | Simulated start instant. `0` keeps the identity clock, where simulated time equals wall time. A nonzero value enables coherent accelerated mode and is returned to adapters by `GET /clock`. |
| `wall_anchor_ns` | integer ns | `0` | Wall instant the accelerated clock anchors to. `0` anchors at server boot, which REWINDS sim time to `sim_epoch_ns` on every restart - each boot is a fresh deterministic scenario, and a client that survives the restart lands on a rewound axis. A nonzero (past) wall instant pins the anchor so every boot computes the same affine map: a restarted venue resumes at the sim instant a monotonic exchange would have reached, which is what lets a surviving worker ride through a venue bounce. Requires `sim_epoch_ns`; a future instant is refused at startup. See `reference/clock.md`, "Restarts". |
| `speed` | float | `1.0` | Clock speed multiplier. With `sim_epoch_ns = 0`, only `1.0` and `0.0` are valid: `1.0` is honest paced replay, and `0.0` is the legacy unthrottled firehose. Any other speed requires a nonzero `sim_epoch_ns`, so market data, execution timestamps, adapter `ts_init`, and timers share one simulated axis. |
| `gap_cap_ms` | integer ms | `1000` | Maximum wall-clock sleep between two ticks under identity paced replay. Bounds the longest pause a sparse stretch of the generated tape can produce. `0` disables the cap. In accelerated mode replay is deadline-paced against the simulated clock and this cap is ignored. |
| `penetration_ticks` | integer | `0` | Strictly-through traded prints required before a resting limit fills. `0` preserves immediate synthetic fills. Values above 1000 are refused. |
| `fill_sweep_interval_ms` | integer ms | `100` | Simulated interval for account-owned penetration sweeps. Must be nonzero when `penetration_ticks` is enabled; values above one hour are refused. Gated scenarios must derive prices from the live tape rather than the old price-100 fixtures. |
| `server_heartbeat_ms` | integer ms | `0` | Server-originated websocket heartbeat cadence in simulated milliseconds. `0` keeps it off. When set, each `/ws` session receives liveness frames that survive `StallData` but not `GoDark` - enable it to test liveness frames that outlive a data stall (the issue-4255 reproduction). |
| `backfill_horizon_ns` | integer ns | `86_400_000_000_000` (24h) | How far before sim-now-at-boot the synthetic tape begins. The boot `data_origin = sim_now_at_boot - backfill_horizon_ns` is the earliest instant any source can serve, shared by every symbol's generator so the timeline tracks the advertised clock. A request straddling the floor is refused loudly (a `/trades` 422, a WS `ProtocolError` clamp) rather than served short, so the default need not be exact - 24h covers a day's warmup. |
| `max_concurrent_tapes` | integer | `256` | Global ceiling on REACHABLE synthesized tapes, summed across every `/ws` connection. A tape is identified by `(symbol, data_origin, regime)` - the regime is part of the identity because it is an input to the generated walk, not a filter over it - and every subscription whose triple matches an existing tape shares that tape for free. Only a DISTINCT tape costs an OS thread, so a 200-account forward-validation batch on one symbol costs one thread, while arming 200 distinct `VolStorm` multipliers costs 200. A subscribe that would need a new tape past the cap is refused for those symbols with a per-entry `ReplayCapacity` outcome; the connection stays up and its running streams are untouched. The cap counts tapes that are reachable, not threads that still exist: a reaped tape releases its permit at map removal while its cancelled thread finishes, so the transient overshoot is bounded by the reaper queue. `0` disables the cap. Replaces the removed `max_concurrent_replays`, whose unit was subscriptions; a config still carrying that key fails to load with a message naming it. |
| `max_subscriptions_per_connection` | integer | `256` | Live subscriptions allowed on ONE websocket, counted over the symbols currently streaming on it. The tape cap no longer bounds this incidentally, since many subscriptions share a tape. A resubscribe of an already-held symbol is free (the cap is checked after the predecessor is quiesced); the overflow is refused with `ReplayCapacity`. Equals `MAX_SUBSCRIBE_SYMBOLS`, so one maximal subscribe frame lands exactly at the limit and the 257th distinct symbol on that connection is refused. `0` disables the cap. |
| `fanout_depth` | integer | `4096` | Depth of each tape's bounded broadcast ring, in pre-serialized frames (~8 simulated hours at the default cadence, ~5 wall minutes at `speed = 100`). A subscriber that falls further behind has its CONNECTION KILLED with WS 1011 as a venue fault, because the venue has lost market data it already promised and a shared tape cannot be stalled by one slow subscriber the way a private replay could. This is a backstop against a wedged consumer, not a modeled pathology: at these depths it is unreachable on a loopback deployment short of a client stalling for hours, and if it ever fires the run should be treated as invalid rather than merely degraded. UNLIKE every neighbouring count knob, `0` is NOT unbounded here: a zero-capacity broadcast channel is invalid, so the key is rejected at load. |
| `zero_speed_stall_ms` | integer ms | `5000` | How long a `speed = 0` tape parks waiting for ring headroom before giving up on its slowest subscriber. Only consulted when `speed = 0`, whose meaning is unchanged ("as fast as the venue can produce") but whose THROTTLE moved: a private replay was paced by its own connection's backpressure, and a shared tape has none, so the tape now blocks on the slowest attached subscriber instead. A stopped client therefore costs one stall of this length and its connection is then killed as a venue fault, rather than stalling the tape forever. |
| `max_accounts` | integer | `4096` | Hard ceiling on simultaneously-live account ledgers. Accounts are created implicitly, on the first request carrying an unknown `x-mogwai-account` id, so a runaway or typo-prone batch would otherwise be free to exhaust memory; a request that would create the `max_accounts + 1`-th account is refused with `429 Too Many Requests` and creates nothing, while every EXISTING account keeps trading. `0` disables the cap. |
| `account_idle_timeout_ms` | integer ms | `3_600_000` (1h) | WALL-clock idleness after which an account with no live websocket session is destroyed, dropping its engine and with it the `seen_client_order_ids` / closed-order / fill retention a long-lived daemon would otherwise accumulate for every batch it has ever run. Wall time, not sim time, deliberately: `speed` is a free multiplier and `0.0` is unthrottled, so a sim-time budget could elapse in seconds of real time and reap a driver that merely paused to compute. Idleness is a property of the DRIVER's real-world liveness. Every identity-bearing request (`POST /orders`, `GET /account`, `POST /control/divergence`) restamps the clock, and an account holding a live session is never reaped no matter how quiet the tape. A session-less driver - the `HttpPolling` transport owns no websocket at all - keeps its account alive by polling `GET /account`, which is the documented keepalive for that case; the accountless `/trades` does not count. `0` disables reaping entirely. |
| `account_reap_interval_ms` | integer ms | `60_000` | Tick period of the idle reaper. Must be greater than zero and is refused at load otherwise: read as "never" it would silently disable the only teardown that fires when no driver calls `DELETE /accounts/<id>`, and read as "every tick" it is a busy loop. Disabling the reaper has exactly one spelling, `account_idle_timeout_ms = 0`. |
| `exec_held_budget_bytes` | integer bytes | `8_388_608` (8 MiB) | Per-`/ws`-connection ceiling on execution output the venue has produced but not yet written to the socket - the HELD lane's budget. Bytes rather than a frame count because an `AccountState` or a venue-truth snapshot has no per-frame size ceiling a count could stand in for. A command whose worst-case output does not fit is refused with an `AdmissionRejected` BEFORE the engine sees it, so nothing mutates; the bytes come back as the writer writes each frame. The ceiling is what stops an armed hour-long `DelayAcks` on one stalled connection from becoming process-wide memory exhaustion, so the process-wide exposure is this value times the live connection count. Must be at least one boundary refusal's worst case, or every order-entry command would be refused; that floor is checked at startup. Lower it to make the venue refuse sooner - which is also how `scripts/smoke.py --admission` reaches the refusal without twelve thousand orders. |
| `admission_lane_frames` | integer | `64` | Per-connection ceiling on QUEUED priority frames: `AdmissionRejected` and `ProtocolError`, the admission truth that is exempt from `DelayAcks` and delivered ahead of held traffic. A frame COUNT is a legitimate memory bound here and nowhere else, because every frame on this lane is under `ADMISSION_FRAME_MAX_BYTES` (64 x 8 KiB = 512 KiB). A slot is held until the frame is written, so the lane only fills against a peer that has stopped reading; when it does, the connection is closed with WS 1013 and a stated reason rather than stalling silently. |
| `admission_promise_tickets` | integer | `256` | Per-connection pool of outstanding PROMISES of a future priority frame, one per live subscription, accounted separately from `admission_lane_frames`. A promise lives as long as its subscription's fanout task (it covers that task's one possible `SeekBudgetExhausted`/`FeedLagged` diagnostic), so drawn from queue depth a handful of healthy streams would leave no room for an actual refusal and an ordinary subscribe would close the connection. Sized at `MAX_SUBSCRIBE_SYMBOLS`, which is exactly how many subscriptions one connection can have. |
| `pending_command_acts` | integer | `256` | Per-connection ceiling on order commands detached by an armed `CommandLatency` ACT delay and not yet acted on. A COMMAND is the right unit here, unlike the held lane's bytes: a pending act holds no frame, it holds future work of up to an hour, and what needs bounding is how many of them one connection can start. Over the budget the command is answered with a visible `AdmissionRejected` and the engine never sees it, so nothing mutates. Lowering it is how `scripts/smoke.py --command-latency` reaches that refusal without an armed hour and a flood. A zero budget is refused at startup: it would accept the control and serve none of it. |
| `global_pending_command_acts` | integer | `4096` | The same count across every connection AND `POST /orders`, which has no per-connection lane to draw on and is therefore covered by this ceiling alone. It is what stops an armed hour-long act delay plus a request flood from parking unbounded axum tasks, each holding a socket - `mogwai-server` installs no `tower` concurrency or timeout layer that would catch that. Must be at least `pending_command_acts`, checked at startup, or the per-connection budget would be unreachable and therefore a lie. |
| `[balances]` | table of decimal strings | `USDT = "1000000"` | Initial per-currency account funding, the venue's equivalent of a deposit made before the run. The ledger books only fill deltas on top of this seed, and the seed rides the first `GET /account` snapshot, so adapters register a funded account before any order is worked. Funding also selects the enforcement mode: a FUNDED account is an honest cash venue that rejects any submit or amend its free balance (total minus resting reservations) cannot cover, with `insufficient <currency> balance`. An explicitly EMPTY `[balances]` table runs the account unfunded and UNENFORCED - the permissive delta-off-zero ledger where the first buy goes negative, which a nautilus cash account (the adapter's default `AccountType::Cash`, borrowing forbidden) refuses to apply; use it only to exercise exactly that refusal. An absent table keeps the funded default. Fund the quote currency of every instrument the run trades (the server warns at boot for any it finds unfunded), and the base currency too if a strategy sells first. Negative amounts and blank currencies are refused at load. This table is the account TEMPLATE: every auto-created account starts funded identically, and funding is not per-account. |
| `[[instrument]]` | array of tables | built-in BTCUSDT | Optional authoritative instrument set. When present, each table carries the wire `InstrumentDef`, a `generator` table, and a `session` table. The server uses this same set for `GET /instruments`, order validation, live subscriptions, and historical `/trades`. Unknown keys are rejected here as at the top level, including in each `[[instrument]]` table. The `generator` and `session` sub-tables are the exception: their types are shared with the committed fingerprint parse, so a typo'd key inside them is still tolerated (its value falls back to the default, and the resulting profile is still value-validated at load). |

## Instrument Profiles

If no `[[instrument]]` tables are present, mogwai serves the built-in BTCUSDT
profile. Once any `[[instrument]]` table is present, that array is the full venue
instrument set. Orders for symbols outside the set are rejected, and market-data
requests for unknown symbols return no generated source.

Each configured instrument carries:

- `InstrumentDef` fields at the top of the table: `symbol`, `base`, `quote`,
  precisions, and increments.
- `generator`: the `GeneratorScalars` used by the synthetic source. Its
  `modal_tick` must equal `price_increment`, and `price_decimals` must equal
  `price_precision`, so generated trades are on the same grid orders validate
  against.
- `session`: a `SessionProfile` with 24 hourly arrival shares, 24 hourly
  volatility multipliers, and 7 day-of-week arrival weights. These are consumed
  directly by the generator's hour and day axes, so per-symbol exchange hours,
  maintenance breaks, and weekend shape belong here.

Startup rejects duplicate symbols, empty symbols/currencies, non-positive
increments, generator/grid mismatches, out-of-range generator scalars, and
non-positive or non-finite session entries.

Minimal custom profile shape:

```toml
[[instrument]]
symbol = "EURUSD"
base = "EUR"
quote = "USD"
price_precision = 4
size_precision = 8
price_increment = "0.0001"
size_increment = "0.00000001"

[instrument.generator]
modal_tick = "0.0001"
price_decimals = 4
mean_duration_s = 7.194349711185499
size_round_frac = 0.20856767610054022
start_price = "1.1000"
typical_size = "100000.0"
vol_scalar = 0.00000005

[instrument.session]
intensity_hour = [
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
]
vol_hour = [
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
  1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
]
dow_weight = [1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]
```

Startup rejects the dangerous middle ground: `sim_epoch_ns = 0` with a `speed`
other than `1.0` or `0.0`. That combination would accelerate only market-data
gaps while execution and adapter timestamps stayed on wall time.

## Examples

The committed default - a paced, honest live feed:

```toml
sim_epoch_ns = 0
speed = 1.0
gap_cap_ms = 1000
server_heartbeat_ms = 0

[balances]
USDT = "1000000"
```

The heartbeat reproduction config (`scripts/smoke-heartbeat.toml`), driving
`scripts/smoke.py --heartbeat`:

```toml
sim_epoch_ns = 0
speed = 1.0
gap_cap_ms = 1000
server_heartbeat_ms = 100
```

An unthrottled firehose for fast local iteration (note: the paced smoke steps
race under this):

```toml
speed = 0.0
```

An accelerated coherent-clock smoke config (`scripts/smoke-accelerated.toml`),
driving `scripts/smoke.py --accelerated`:

```toml
sim_epoch_ns = 1900000000000000000
speed = 100.0
gap_cap_ms = 1000
server_heartbeat_ms = 0
```
