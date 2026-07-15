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
| `speed` | float | `1.0` | Clock speed multiplier. With `sim_epoch_ns = 0`, only `1.0` and `0.0` are valid: `1.0` is honest paced replay, and `0.0` is the legacy unthrottled firehose. Any other speed requires a nonzero `sim_epoch_ns`, so market data, execution timestamps, adapter `ts_init`, and timers share one simulated axis. |
| `gap_cap_ms` | integer ms | `1000` | Maximum wall-clock sleep between two ticks under identity paced replay. Bounds the longest pause a sparse stretch of the generated tape can produce. `0` disables the cap. In accelerated mode replay is deadline-paced against the simulated clock and this cap is ignored. |
| `server_heartbeat_ms` | integer ms | `0` | Server-originated websocket heartbeat cadence in simulated milliseconds. `0` keeps it off. When set, each `/ws` session receives liveness frames that survive `StallData` but not `GoDark` - enable it to test liveness frames that outlive a data stall (the issue-4255 reproduction). |
| `backfill_horizon_ns` | integer ns | `86_400_000_000_000` (24h) | How far before sim-now-at-boot the synthetic tape begins. The boot `data_origin = sim_now_at_boot - backfill_horizon_ns` is the earliest instant any source can serve, shared by every symbol's generator so the timeline tracks the advertised clock. A request straddling the floor is refused loudly (a `/trades` 422, a WS `ProtocolError` clamp) rather than served short, so the default need not be exact - 24h covers a day's warmup. |
| `max_concurrent_replays` | integer | `1024` | Global ceiling on concurrently-live per-symbol replay streams, summed across every `/ws` connection. Each subscribed symbol runs on its own OS thread, so without a ceiling the aggregate thread count is `connections * subscribed-symbols` - entirely client-driven, and a fleet of connections each subscribing the whole catalog can exhaust the process thread limit. A subscribe that would exceed the cap is refused for the over-limit symbols with a `ProtocolError` on the wire, exactly like an unservable subscribe; the connection stays up and its already-running streams are untouched. `0` disables the cap (unbounded). |
| `[balances]` | table of decimal strings | `USDT = "1000000"` | Initial per-currency account funding, the venue's equivalent of a deposit made before the run. The ledger books only fill deltas on top of this seed, so an unfunded account goes negative on its first buy - which a nautilus cash account (the adapter's default `AccountType::Cash`, where borrowing is forbidden) refuses to apply, silently desyncing the consumer's account from the venue's. The seed rides the first `GET /account` snapshot, so adapters register a funded account before any order is worked. An absent table keeps the funded default; an explicitly empty `[balances]` table runs the account unfunded on purpose. Fund the quote currency of every instrument the run trades (and the base currency too if a strategy sells first). Negative amounts and blank currencies are refused at startup. |
| `[[instrument]]` | array of tables | built-in BTCUSDT | Optional authoritative instrument set. When present, each table carries the wire `InstrumentDef`, a `generator` table, and a `session` table. The server uses this same set for `GET /instruments`, order validation, live subscriptions, and historical `/trades`. Unknown top-level keys are rejected, but a typo'd sub-field inside an `[[instrument]]` table is still tolerated (serde cannot combine strict-unknown-field checking with the flattened instrument record). |

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
