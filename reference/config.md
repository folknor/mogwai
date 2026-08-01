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
| `server_heartbeat_ms` | integer ms | `0` | Server-originated websocket heartbeat cadence in simulated milliseconds. `0` keeps it off. When set, each `/ws` session receives liveness frames that survive `StallData` but not `GoDark` - enable it to test liveness frames that outlive a data stall (the issue-4255 reproduction). |
| `backfill_horizon_ns` | integer ns | `86_400_000_000_000` (24h) | How far before sim-now-at-boot the synthetic tape begins. The boot `data_origin = sim_now_at_boot - backfill_horizon_ns` is the earliest instant any source can serve, shared by every symbol's generator so the timeline tracks the advertised clock. A request straddling the floor is refused loudly (a `/trades` 422, a WS `ProtocolError` clamp) rather than served short, so the default need not be exact - 24h covers a day's warmup. |
| `max_concurrent_replays` | integer | `1024` | Global ceiling on concurrently-live per-symbol replay streams, summed across every `/ws` connection. Each subscribed symbol runs on its own OS thread, so without a ceiling the aggregate thread count is `connections * subscribed-symbols` - entirely client-driven, and a fleet of connections each subscribing the whole catalog can exhaust the process thread limit. A subscribe that would exceed the cap is refused for the over-limit symbols with a `ProtocolError` on the wire, exactly like an unservable subscribe; the connection stays up and its already-running streams are untouched. `0` disables the cap (unbounded). A multi-account fleet must raise this past the batch's TOTAL subscription count (workers times symbols per worker), not past its account count: replay permits are charged per subscription, and the default `1024` refuses subscriptions somewhere past the halfway mark of a 2000-worker batch. |
| `max_accounts` | integer | `4096` | Hard ceiling on simultaneously-live account ledgers. Accounts are created implicitly, on the first request carrying an unknown `x-mogwai-account` id, so a runaway or typo-prone batch would otherwise be free to exhaust memory; a request that would create the `max_accounts + 1`-th account is refused with `429 Too Many Requests` and creates nothing, while every EXISTING account keeps trading. `0` disables the cap. |
| `account_idle_timeout_ms` | integer ms | `3_600_000` (1h) | WALL-clock idleness after which an account with no live websocket session is destroyed, dropping its engine and with it the `seen_client_order_ids` / closed-order / fill retention a long-lived daemon would otherwise accumulate for every batch it has ever run. Wall time, not sim time, deliberately: `speed` is a free multiplier and `0.0` is unthrottled, so a sim-time budget could elapse in seconds of real time and reap a driver that merely paused to compute. Idleness is a property of the DRIVER's real-world liveness. Every identity-bearing request (`POST /orders`, `GET /account`, `POST /control/divergence`) restamps the clock, and an account holding a live session is never reaped no matter how quiet the tape. A session-less driver - the `HttpPolling` transport owns no websocket at all - keeps its account alive by polling `GET /account`, which is the documented keepalive for that case; the accountless `/trades` does not count. `0` disables reaping entirely. |
| `account_reap_interval_ms` | integer ms | `60_000` | Tick period of the idle reaper. Must be greater than zero and is refused at load otherwise: read as "never" it would silently disable the only teardown that fires when no driver calls `DELETE /accounts/<id>`, and read as "every tick" it is a busy loop. Disabling the reaper has exactly one spelling, `account_idle_timeout_ms = 0`. |
| `exec_held_budget_bytes` | integer bytes | `8_388_608` (8 MiB) | Per-`/ws`-connection ceiling on execution output the venue has produced but not yet written to the socket - the HELD lane's budget. Bytes rather than a frame count because an `AccountState` or a venue-truth snapshot has no per-frame size ceiling a count could stand in for. A command whose worst-case output does not fit is refused with an `AdmissionRejected` BEFORE the engine sees it, so nothing mutates; the bytes come back as the writer writes each frame. The ceiling is what stops an armed hour-long `DelayAcks` on one stalled connection from becoming process-wide memory exhaustion, so the process-wide exposure is this value times the live connection count. Must be at least one boundary refusal's worst case, or every order-entry command would be refused; that floor is checked at startup. Lower it to make the venue refuse sooner - which is also how `scripts/smoke.py --admission` reaches the refusal without twelve thousand orders. |
| `admission_lane_frames` | integer | `64` | Per-connection ceiling on QUEUED priority frames: `AdmissionRejected` and `ProtocolError`, the admission truth that is exempt from `DelayAcks` and delivered ahead of held traffic. A frame COUNT is a legitimate memory bound here and nowhere else, because every frame on this lane is under `ADMISSION_FRAME_MAX_BYTES` (64 x 8 KiB = 512 KiB). A slot is held until the frame is written, so the lane only fills against a peer that has stopped reading; when it does, the connection is closed with WS 1013 and a stated reason rather than stalling silently. |
| `admission_promise_tickets` | integer | `256` | Per-connection pool of outstanding PROMISES of a future priority frame, one per live replay, accounted separately from `admission_lane_frames`. A promise lives as long as its replay (it covers that replay's one possible dead-seek diagnostic), so drawn from queue depth a handful of healthy streams would leave no room for an actual refusal and an ordinary subscribe would close the connection. Sized at `MAX_SUBSCRIBE_SYMBOLS`, which is exactly how many replays one connection can have. |
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
