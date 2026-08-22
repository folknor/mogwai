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
not the now of a placed boat. A boated river's now is the last instant its boat
published, not the boat clock's affine projection. Every symbol-bearing HTTP
endpoint uses that same resolution rule and waits for an in-flight placement
before choosing its answer.

`speed` is the only clock key left in config, and it is a DEFAULT rather than
the run's one pacing rate: a `/ws` upgrade may name its own `speed`, and the
boat it boards is keyed by river and by that speed quantized to micro-multiples.
An unserved speed is a second boat on the same river, not a refusal. An
absent `speed` means the configured one and is what every consumer that predates
the carrier sends. `/clock` reports the RUN's clock and nothing about any boat:
it took `symbol` and `speed` and answered on a named river's boat until that was
found to be a boat-discovery channel on a route that cannot tell who is asking,
and both parameters are now refused rather than ignored. A passenger's own
delivery instant reaches it where it always did, stamped on the frames its boat
publishes.
`speed = 0.0` is unpaced
delivery, not a stopped clock: the sim axis still advances at wall rate so the
deadline task, the fill sweeper and the trailing-volatility window keep
working: `/clock` reports speed `1.0` for a firehose run. A non-zero speed
paces delivery to `sim.wall_ns(ts)`.

THE CONSEQUENCE, stated because it has already been reasoned about backwards
twice: on a `speed = 0.0` venue the TWO AXES COME APART. Delivery is unpaced, so
the tape's `ts_event` runs as far ahead as the generator can carry it, while
`venue_now_ns` walks at wall rate from the boat's origin. Nothing on such a
venue supports the reading "sim time is roughly wall time since boot", and a
condition that anchors a `/clock` target on a TAPE STAMP is satisfied the instant
it is asked. Neither is the reverse true - the clock does not race the wall; it
IS the wall, at rate 1. A test or a tool that needs "some wall time has passed
for the sweeper" gets nothing from the clock on this config, because the sweeper
is on a wall cadence the clock cannot distinguish itself from.

## What acceleration costs a consumer that stops reading

The venue is COHERENT. Once live, no reader can advance the canonical lead past
the tape worker, so published tape, history, sweeps and market readings are one
deterministic realization. The worker does advance the lead before pacing and
publishing, but during that pace it sleeps until THAT tick's own wall deadline,
so the unpublished tick lies in the future of sim-now and market readings are
filtered to at-or-before sim-now. The engine cannot price from it, and that
window cannot bite.

So the only way a connection sees a market older than the one it is filled against
is that it has not drained its socket. Everything the engine can price from was
published already; unread frames are the connection's own backlog, every real venue
behaves this way, and `FeedLagged` is the signal for it - the venue names the span
the connection lost and goes on serving it, because being behind is the
connection's condition to act on rather than a fault of the venue's.

What is worth knowing is the ACCELERATION MULTIPLIER. At speed 100, every 10 ms
a connection spends not reading - aggregating bars, evaluating a strategy - is a
simulated SECOND of tape it never saw. Forward tests always run accelerated, so
an accelerated run is systematically further behind the tape than a real one
would be, in proportion to the speed factor and to how long the connection waits
between reads. That is a property of accelerating past the connection, not a
venue defect, and a reader of forward results should know it. Under N
instruments it is the connection's read budget that splits N ways.

No measurement is owed here: the backlog is unbounded by construction, and a
number for one host on one day would change no decision. What would give a
measurement a decision to serve is a specific forward result somebody doubts,
where the question becomes whether that run was valid.

`warmup_ns` is the uniform servable simulated interval before `run_start_ns`.
The boot river is materialized before readiness and every other river on first
read. `run_start_ns` is every boat's placement origin, so per-boat clocks vary
in wall anchor and speed but never in sim epoch. THE CONSEQUENCE FOR A DECLARED
DURATION IS WORTH STATING, because it reads as a venue defect otherwise: the
run deadline is judged on the venue clock, while a socket's `RunComplete` is
re-derived on ITS boat's clock, which is anchored at that boat's placement.
A boat placed a moment after the venue clock was anchored therefore trails it
by that gap times `speed` - at speed 100, 180 us of placement gap is 18 ms of
simulated time - so the announcement one socket reads can carry slightly LESS
elapsed sim time than the run declared, permanently and by design. The venue
serves the whole declared duration on its own clock; no socket's clock is that
clock. Holding the run open until every boat's affine clock had also passed the
deadline would let a socket connecting near the deadline extend the run by
another whole duration, which is why nothing does.
`data_origin_ns` is its earliest servable instant, always `TAPE_ORIGIN_NS`.
`/clock` reports the run's clock and the current simulated time, and takes no
parameters at all. It is also the axis anonymous history is answered on, so a
consumer paginating `/trades` or `/quotes` reads it once and passes that instant
as `end` on every page, which is what stops one logical window from growing
while it is being read. A clock-neutral havoc window stores its wall arming instant
and simulated span. Each reader judges the span on its own clock; a boat placed
after the arm opens the window at its own epoch and receives the full span.
The pulled `/account` snapshot is venue-scoped and labels its axis as
`"clock":"venue"`; pushed account events remain boat-stamped and the two axes
must be ordered by sequence, not timestamp comparison. A process is
not restarted in place: launchers create a new run and obtain a new readiness
record with a fresh (or configured) seed.
