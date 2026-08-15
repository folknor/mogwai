# mogwai clock

`SimClock` maps wall time to simulated time for one run. The tape's origin is
the fixed constant `TAPE_ORIGIN_NS = 0` - not derived from wall time - so
`data_origin_ns` is the same for every run of a given config. The run clock is
built AFTER warmup is materialized, anchoring `sim_epoch_ns` at
`TAPE_ORIGIN_NS + warmup_ns` and the wall anchor at the post-warmup boot
instant; that anchor decides only WHEN a tick is delivered, never which tick
it is. Warmup generation does not consume declared run duration.

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
a boat's own time. A process is
not restarted in place: launchers create a new run and obtain a new readiness
record with a fresh (or configured) seed.
