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

`speed` is the only clock key left in config, and it is a DEFAULT rather than
the run's one pacing rate: a `/ws` upgrade may name its own `speed`, and the
boat it boards is keyed by river and by that speed quantized to micro-multiples.
An unserved speed is a second boat on the same river, not a refusal. An
absent `speed` means the configured one and is what every client that predates
the carrier sends. `/clock?symbol=` answers the lead boat when a river carries
more than one cadence; `/clock?symbol=&speed=` names one.
`speed = 0.0` is unpaced
delivery, not a stopped clock: the sim axis still advances at wall rate so the
deadline task, the fill sweeper and the trailing-volatility window keep
working: `/clock` reports speed `1.0` for a firehose run. A non-zero speed
paces delivery to `sim.wall_ns(ts)`.

## What acceleration costs a consumer that stops reading

The venue is COHERENT. Once live, no reader can advance the canonical lead past
the tape worker, so published tape, history, sweeps and market readings are one
deterministic realization. The worker does advance the lead before pacing and
publishing, but during that pace it sleeps until THAT tick's own wall deadline,
so the unpublished tick lies in the future of sim-now and market readings are
filtered to at-or-before sim-now. The engine cannot price from it, and that
window cannot bite.

So the only way a client sees a market older than the one it is filled against
is that it HAS NOT DRAINED ITS SOCKET. Everything the engine can price from was
published already; unread frames are the client's own backlog, every real venue
behaves this way, and `FeedLagged` is the signal for it.

What is worth knowing is the ACCELERATION MULTIPLIER. At speed 100, every 10 ms
a consumer spends not reading - aggregating bars, evaluating a strategy - is a
simulated SECOND of tape it never saw. Forward tests always run accelerated, so
an accelerated run is systematically further behind the tape than a real one
would be, in proportion to the speed factor and to how long the consumer thinks
between reads. That is a property of accelerating past the consumer, not a
venue defect, and a reader of forward results should know it. Under N
instruments it is the consumer's read budget that splits N ways.

No measurement is owed here: the backlog is unbounded by construction, and a
number for one host on one day would change no decision. What would give a
measurement a decision to serve is a specific forward result somebody doubts,
where the question becomes whether that run was valid.

`warmup_ns` is the uniform servable simulated interval before `run_start_ns`.
The boot river is materialized before readiness and every other river on first
read. `run_start_ns` is every boat's placement origin, so per-boat clocks vary
in wall anchor and speed but never in sim epoch.
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
