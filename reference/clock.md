# mogwai simulated clock

How mogwai compresses time for accelerated forward testing, and - the part this
document exists to pin down - exactly which temporal knobs ride the simulated
axis and what their wall-clock lower bounds are. This is the durable companion
to the coherent-clock design (its transient spec lives under `docs/`) and to the
`SimClock` doc comments in `mogwai-protocol`. `reference/config.md` covers the
`mogwai.toml` knobs that turn it on; `reference/havoc.md` covers the divergence
durations that scale with it.

## The one affine axis

mogwai runs the real live trading path - real async, real reconnects, real WS
frames - while advancing one shared simulated time axis faster than the wall.
A real exchange forces 1x; mogwai's clock is fake, so it has no lower bound.

Simulated time is an affine function of wall-clock time, defined by three
numbers fixed once at server boot and served to every consumer over
`GET /clock`:

```
sim_ns(wall_ns) = sim_epoch_ns + (wall_ns - wall_anchor_ns) * speed
```

- `sim_epoch_ns` - the simulated instant the run starts from. `0` selects the
  identity clock (sim == wall); a nonzero value selects accelerated mode.
- `wall_anchor_ns` - the wall instant the clock is anchored. By default this is
  captured at server boot; the `wall_anchor_ns` config knob pins it to a fixed
  past instant instead. Which one you choose decides what a RESTART means -
  see "Restarts" below.
- `speed` - the acceleration factor (`1.0` honest live; `3600.0` a simulated
  hour per wall second).

The `SimClock` value (`mogwai-protocol`) is the single implementation of this
map. Every time consumer - the server engine stamps, the adapter `ts_init`
stamps, and the `MogwaiClock` injected into broadarrow's nautilus node - computes
`sim_ns` from the same three numbers, so they agree by construction. Coherence is
the defining requirement: market-data `ts_event`, execution/account stamps, the
adapter's `ts_init`, and the node clock's reads AND timer firings all sit on this
one axis.

Three primitives realize it, all saturating and all keeping `sim_epoch_ns`
(epoch scale, ~1.9e18, beyond f64's exact-integer range) out of the float:

- `sim_ns(wall)` - map a wall read onto the sim axis (for stamps).
- `wall_ns(sim)` - the wall instant a sim instant is reached (for absolute
  deadlines: timer alerts, deadline-paced replay).
- `wall_span(sim_dur)` / `wall_duration(sim_dur)` - the wall duration that
  realizes a simulated duration, `sim_dur / speed` (for every relative sleep,
  interval, and elapsed threshold). `wall_duration` is the `Duration` form and
  is the single place the 1ns code floor is applied.

At `speed == 1.0` with `sim_epoch_ns == wall_anchor_ns` the map is the identity,
`wall_span` returns its argument, and behavior is byte-identical to a
non-accelerated run.

## The two-knob trap

`sim_epoch_ns != 0` is the switch; `speed` is the factor. The dangerous middle
is `speed != 1.0` with `sim_epoch_ns == 0`: the clock would stay the identity
(stamps on wall time) while legacy gap-based replay still divided gaps by
`speed` - data-only acceleration, which is silently incoherent. The server
rejects `sim_epoch_ns == 0 && speed != 1.0 && speed != 0.0` at startup, and also
`sim_epoch_ns != 0 && speed <= 0.0`. `speed == 0.0` stays the legacy unthrottled
firehose, valid only at `sim_epoch_ns == 0`.

## Restarts: boot-anchored rewind vs a pinned anchor

`sim_epoch_ns` is fixed config; the anchor is not, unless you pin it. That
asymmetry is the whole story of what a venue restart does to the axis:

- **Default (`wall_anchor_ns = 0`): every boot re-anchors, so a restart REWINDS
  sim-now back to `sim_epoch_ns`.** The rewind equals the previous instance's
  uptime times `speed` - the longer the venue ran, the further back it jumps.
  This makes each boot a fresh deterministic scenario (same epoch, same tape
  origin `sim_epoch - backfill_horizon`), which is the right semantic for
  repeatable single-run tests. It is the WRONG semantic for any client that
  survives the restart: a real exchange's clock is monotonic, and a surviving
  client's cursors and watermarks now sit in the restarted venue's FUTURE. The
  venue clamps such a subscribe to live-from-now ("start_ts exceeds sim-now")
  and streams from the rewound instant, but the client discards the
  backwards-stamped data until sim-now catches its old watermark - observed as
  a subscribe/diagnostic storm and a recovery lag of (rewind / speed) wall
  seconds.
- **Pinned (`wall_anchor_ns` set to a fixed past instant): every boot computes
  the SAME affine map, so sim time is monotonic across restarts.** A bounced
  venue resumes at the sim instant a monotonic exchange would have reached,
  as though it had kept running through the outage; a surviving worker's
  cursors stay in the past and are served normally. Note the tape origin
  follows sim-now at each boot (`data_origin = sim_now_at_boot -
  backfill_horizon`), so after a long outage a surviving cursor is servable
  only within the backfill horizon. This is the mode for exercising venue
  bounces, deploys, and any scenario where the client outlives the server.

### Why the worker cannot adopt a rewound axis (the nautilus side)

It is tempting to instead have the ADAPTER re-sync: detect the reconnect,
re-fetch `GET /clock`, and adopt the restarted server's rewound map. That is
not viable, and the reason is nautilus, not mogwai:

- The adapter's `mogwai_clock_factory` fetches `/clock` ONCE at worker boot
  and every `MogwaiClock` the node ever builds - the kernel clock and every
  component clock - captures that one map. There is no rebind surface: nautilus
  constructs clocks from the factory as components come up, and the clocks are
  the node's time authority for the life of the process.
- More fundamentally, nautilus requires time to be MONOTONIC. Its clocks drive
  timer deadlines, event ordering, cache staleness guards, and bar watermarks;
  `ts_event`/`ts_init` stamps are compared across the whole run. Stepping the
  node clock backward by the rewind would violate every one of those
  comparisons at once - resting timers would sit un-fireable for
  (rewind / speed) wall seconds, and freshly stamped events would sort BEFORE
  events already processed. A rewound axis is not a value a live nautilus node
  can adopt; the only consistent choices are "the worker keeps the old axis"
  (the default today, with the storm above) or "the venue never rewinds" (the
  pinned anchor).

So: restart the venue under a surviving worker only with a pinned
`wall_anchor_ns`. The boot-anchored default remains right for fresh-scenario
runs where venue and workers start together.

## What rides the axis, and its floor

Everything temporal is on the sim axis. The table is the contract: every row is
a configured (sim-intended) duration or instant, how it is realized, and its
hard lower bound. "~1ms tokio" is the tokio timer/scheduler granularity - the
irreducible floor below which no sleep or interval can resolve, regardless of
`speed`. A scaled duration that falls below it simply coalesces to it.

### Relative durations (realized as `sim_dur / speed` wall)

| Knob | Owner | Realized via | Wall floor |
| --- | --- | --- | --- |
| `DelayAcks { ms }` | server writer | `sim.wall_duration` sleep | 1ns code / ~1ms tokio |
| `CommandLatency { .. }` act / ack | server order path / writer | `sim.wall_duration` sleep | 1ns code / ~1ms tokio |
| `server_heartbeat_ms` | server heartbeat | `sim.wall_duration` interval | 1ns code / ~1ms tokio |
| `BASELINE_LATENCY` (30ms) + armed `HavocLatency` | adapter inbound | `sleep_havoc_delay` -> `sim.wall_duration` | 1ns code / ~1ms tokio |
| `reconnect_delay_initial_ms` / `reconnect_delay_max_ms` / `reconnect_jitter_ms` | adapter lifecycle | `ReconnectPolicy::backoff` -> `sim.wall_duration` | 1ns code / ~1ms tokio |
| `idle_timeout_ms` | adapter lifecycle | `idle_sleep` / `reset_idle` -> `sim.wall_duration` | 1ns code / ~1ms tokio |
| `heartbeat_interval_ms` (client ping) | adapter lifecycle | `sim.wall_duration` interval | 1ns code / ~1ms tokio |
| `max_requests_per_second` | adapter `HttpQuota` | per-request spacing -> `sim.wall_duration` | 1ns code / ~1ms tokio |
| `request_timeout_secs` | adapter HTTP order entry | `sim.wall_span` then ceil to seconds | **1 WALL second** (see below) |

### Absolute instants (mapped via `wall_ns`, fired at that wall deadline)

| Quantity | Owner | Realized via | Resolution floor |
| --- | --- | --- | --- |
| Tape tick release | server tape thread (`tape.rs`) | sleep until `sim.wall_ns(ts_event)` | ~1ms tokio |
| `GoDark { ms }` / `StallData { ms }` window | server writer | sim-time deadline, per-frame `sim_now` guard | per outbound frame |
| Timer alerts / interval fires | adapter `MogwaiClock` | sleep until `sim.wall_ns(target)`; event stamped at the sim instant | ~1ms tokio; sub-floor intervals coalesce |

### Deliberately NOT on the axis

- `POLL_INTERVAL` (250ms, adapter `HttpPolling`) stays a WALL duration. It paces
  the `/trades` pull, and that endpoint is the `ORIGIN_TS`-anchored history seek
  path, which this work leaves off the accelerated axis (relocating its anchor
  would break `/trades` cursor determinism). Scaling the poll would only spin the
  loop faster under acceleration without producing on-axis data. The accelerated
  vehicle is the WS push path, which the server deadline-paces. Full accelerated
  `HttpPolling` coherence is a named downstream item.
- `gap_cap_ms` is a 1x comfort cap on the legacy gap-based pace. Under deadline
  pacing the deadline is absolute, so the cap is structurally irrelevant and is
  ignored when accelerated (the server logs this once at startup).
- The history/polling path generally: `build_history_source` anchors at the
  compiled-in `ORIGIN_TS` and is request/response pull, not deadline-paced.

## The `request_timeout_secs` exception and the speed ceiling

Every floor above is the ~1ms tokio granularity except one: `request_timeout_secs`
is clamped UP to **one wall second** (`MIN_WALL_REQUEST_TIMEOUT_SECS` in the
adapter). It is the lone duration that guards a REAL local-IO round trip - an
actual HTTP request to the server - whose wall cost does NOT compress with
`speed`. Dividing a 30 sim-second timeout by `speed = 100` yields a 0.3 wall-
second budget that the real round trip would blow, spuriously timing out every
order. The one-second wall floor keeps the request survivable.

That makes `request_timeout_secs` the tightest contributor to the usable-speed
ceiling. More generally:

```
realized sim-coherence error ~= (irreducible wall delivery latency) * speed
```

Nothing compresses the tokio granularity, the mpsc hand-off, or the WS/HTTP
round trip - they are real wall costs. At `speed == 1e5`, 1ms of un-compressible
wall latency is 100 simulated seconds. So the maximum usable speed is a MEASURED
property, not a target: above the speed where `floor * speed` exceeds the
tolerance a test cares about, even a perfectly correct affine clock fails the
coherence gate. `scripts/smoke.py --accelerated` measures where the budget is
blown and reports it; a real venue's own feed carries jitter of the same order,
so a nonzero budget is honest, not a defect.
