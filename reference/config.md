# mogwai run config

The `mogwai` binary takes its run knobs from a TOML file, not from the
ambient environment. This is the reference for that file; see `reference/cli.md`
for how the binary finds it (`--config <path>`, default `mogwai.toml` in the
working directory) and the rest of the command-line surface.

These are *run* knobs - replay pacing and the optional server heartbeat. They are
distinct from the per-venue havoc configuration broadarrow constructs on the
adapter side (`HavocSpec`, including `ConnHavoc.heartbeat_interval_ms`, which is a
*client* ping, not this server heartbeat); see `reference/havoc.md` for that.

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
| `speed` | float | `1.0` | Replay speed multiplier. `1.0` paces inter-tick wall delay to the real gap between ticks - the honest live-venue default, and what `scripts/smoke.py` needs. `0.0` streams unthrottled (a firehose for fast local iteration; the paced smoke steps race and fail under it). Otherwise the inter-tick sleep is `(tick gap) / speed`. |
| `gap_cap_ms` | integer ms | `1000` | Maximum wall-clock sleep between two ticks under paced replay. Bounds the longest pause a sparse stretch of the generated tape can produce. `0` disables the cap. |
| `server_heartbeat_ms` | integer ms | `0` | Server-originated websocket heartbeat cadence. `0` (the honest default) keeps it off. When set, each `/ws` session receives liveness frames that survive `StallData` but not `GoDark` - enable it to test liveness frames that outlive a data stall (the issue-4255 reproduction). |

## Examples

The committed default - a paced, honest live feed:

```toml
speed = 1.0
gap_cap_ms = 1000
server_heartbeat_ms = 0
```

The heartbeat reproduction config (`scripts/smoke-heartbeat.toml`), driving
`scripts/smoke.py --heartbeat`:

```toml
speed = 1.0
gap_cap_ms = 1000
server_heartbeat_ms = 100
```

An unthrottled firehose for fast local iteration (note: the paced smoke steps
race under this):

```toml
speed = 0.0
```
