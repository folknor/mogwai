# mogwai run config

The `mogwai` binary takes its run knobs from a TOML file, not from the
ambient environment. This is the reference for that file; see `reference/cli.md`
for how the binary finds it (`serve --config <path>`, default `mogwai.toml` in
the working directory) and the rest of the command-line surface.

These are *run* knobs - the simulated clock, replay pacing, and the optional
server heartbeat. They are distinct from the per-venue havoc configuration
broadarrow constructs on the adapter side (`HavocSpec`, including
`ConnHavoc.heartbeat_interval_ms`, which is a *client* ping, not this server
heartbeat); see `reference/havoc.md` for that. For the affine simulated-clock
model these `sim_epoch_ns` / `speed` knobs drive - and the full table of every
temporal quantity that scales with `speed` and its wall lower bound - see
`reference/clock.md`.

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
