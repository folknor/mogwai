# mogwai clock

`SimClock` maps wall time to simulated time for one boat. The tape's origin is
the fixed constant `TAPE_ORIGIN_NS = 0` - not derived from wall time - so
`data_origin_ns` is the same for every run of a given config. The run clock is
built AFTER warmup is materialized, anchoring `sim_epoch_ns` at
`TAPE_ORIGIN_NS + warmup_ns` and the wall anchor at the post-warmup boot
instant; that anchor decides only WHEN a tick is delivered, never which tick
it is. Warmup generation does not consume declared run duration.

The venue retains one wall-to-sim reference for answers that have no boat: a
boatless river, the venue deadline, and the venue-scoped account ledger. It is
not the now of a seated boat. A seated river's now is the last instant its boat
published, not the boat clock's affine projection. Every symbol-bearing HTTP
endpoint uses that same resolution rule and waits for an in-flight placement
before choosing its answer.

`speed` is the only clock key left in config. `speed = 0.0` is unpaced
delivery, not a stopped clock: the sim axis still advances at wall rate so the
deadline task, the fill sweeper and the trailing-volatility window keep
working: `/clock` reports speed `1.0` for a firehose run. A non-zero speed
paces delivery to `sim.wall_ns(ts)`.

`warmup_ns` is the materialized simulated interval before `run_start_ns`.
`data_origin_ns` is its earliest servable instant, always `TAPE_ORIGIN_NS`.
`/clock` reports both the clock and the current simulated time. It takes an
optional `?symbol=`, and answers for that river's boat: a boat carries its own
clock, anchored at ITS placement, and `server_now_ns` is then the sim instant of
the last tick that boat published rather than the affine map read at the wall.
With no symbol, or for a river carrying no boat, the venue clock answers
instead and `boat_clock` is `false`, so a caller cannot mistake the fallback for
a boat's own time. A clock-neutral havoc window stores its wall arming instant
and simulated span. Each reader judges the span on its own clock; a boat placed
after the arm opens the window at its own epoch and receives the full span.
The pulled `/account` snapshot is venue-scoped and labels its axis as
`"clock":"venue"`; pushed account events remain boat-stamped and the two axes
must be ordered by sequence, not timestamp comparison. A process is
not restarted in place: launchers create a new run and obtain a new readiness
record with a fresh (or configured) seed.
