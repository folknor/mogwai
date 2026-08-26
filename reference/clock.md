# mogwai clock

`SimClock` maps wall time to simulated time for one boat. The tape's origin is
the fixed constant `TAPE_ORIGIN_NS = 0` - not derived from wall time - so
`data_origin_ns` is the same for every run of a given config. The run clock is
built during readiness preparation, anchoring `sim_epoch_ns` at
`TAPE_ORIGIN_NS + warmup_ns` and the wall anchor at that instant; that anchor
decides only when a tick is delivered, never which tick it is.

Materializing a river consumes declared run duration, and no river escapes it.
The clock was once built after one river had been warmed, so that river's warmup
was free and every other river's was not. Nothing is warmed before readiness now,
so a run that first names a river pays for generating it out of the duration it
declared - uniformly, whichever label that river carries.

The venue retains one wall-to-sim reference, and it is the only axis any request
that owns no boat is answered on: history, the venue deadline, the venue-scoped
account ledger, `/clock`. It is deliberately not the now of any placed boat -
a boat's delivery frontier belongs to the passengers riding it, and reporting it
to a caller that owns no boat told one passenger about another.

`speed` is the only clock key left in config, and it is a default rather than
the run's one pacing rate: a `/ws` upgrade may name its own `speed`, and the
boat it boards is keyed by river and by that speed quantized to micro-multiples.
An unserved speed is a second boat on the same river, not a refusal. An
absent `speed` means the configured one and is what every consumer that predates
the carrier sends. `/clock` reports the run's clock and nothing about any boat:
it took `symbol` and `speed` and answered on a named river's boat until that was
found to be a boat-discovery channel on a route that cannot tell who is asking,
and both parameters are now refused rather than ignored. A passenger's own
delivery instant reaches it where it always did, stamped on the frames its boat
publishes.
`speed = 0.0` is unpaced
delivery, not a stopped clock: the sim axis still advances at wall rate so the
deadline task, the fill sweeper and the trailing-volatility window keep
working, which is why `/clock` reports speed `1.0` for a firehose run - that is
the rate the axis genuinely advances at. A non-zero speed
paces delivery to `sim.wall_ns(ts)`.

The consequence, stated because it has already been reasoned about backwards
twice: on a `speed = 0.0` venue the two axes come apart. Delivery is unpaced, so
the tape's `ts_event` runs as far ahead as the generator can carry it, while
`venue_now_ns` walks at wall rate from the boat's origin. Nothing on such a
venue supports the reading "sim time is roughly wall time since boot", and a
condition that anchors a `/clock` target on a tape stamp is satisfied the instant
it is asked. Neither is the reverse true - the clock does not race the wall; it
simply is the wall, at rate 1. A test or a tool that needs "some wall time has passed
for the sweeper" gets nothing from the clock on this config, because the sweeper
is on a wall cadence the clock cannot distinguish itself from.

## What acceleration costs a consumer that stops reading

The adapter scales `request_timeout_secs` from the simulation axis onto the wall
clock, then floors the result at one wall second. Local I/O does not compress
with simulated time, so without that floor a high enough `speed` would hand a
sound HTTP round trip a sub-second budget it cannot make, and every order would
time out for no reason but the accelerator.

What the floor does is worth stating precisely, because it was written down
backwards once. It imposes no ceiling on usable acceleration. The wall budget is
`max(ceil(configured / speed), 1)`, which never falls below a wall second however
fast the run goes, so acceleration cannot squeeze a request out. Nor can raising
the configured timeout ever shorten the wall wait - the expression is monotone in
both arguments, upward in `configured` and downward in `speed`.

What it does cost is the meaning of the number. Past `speed == configured`, the
floor takes over and the effective budget stops being the simulated span the
consumer asked for: one wall second is `speed` simulated seconds, so a
`ConnHavoc` arm meaning "give up after two simulated seconds" is silently more
generous than that on any run faster than speed two, and the divergence grows
with the speed. That is deliberate and is not owed a fix from the simulation
axis - the whole point of the floor is that a wall round trip is a wall cost.
Tightening it wants a measured local round-trip bound, which nothing has needed
enough to buy.

The venue is coherent. Once live, no reader can advance the canonical lead past
the tape worker, so published tape, history, sweeps and market readings are one
deterministic realization. The worker does advance the lead before pacing and
publishing, but during that pace it sleeps until that same tick's own wall deadline,
so the unpublished tick lies in the future of sim-now and market readings are
filtered to at-or-before sim-now. The engine cannot price from it, and that
window cannot bite.

So the only way a connection sees a market older than the one it is filled against
is that it has not drained its socket. Everything the engine can price from was
published already; unread frames are the connection's own backlog, every real venue
behaves this way, and `FeedLagged` is the signal for it - the venue names the span
the connection lost and goes on serving it, because being behind is the
connection's condition to act on rather than a fault of the venue's.

What is worth knowing is the acceleration multiplier. At speed 100, every 10 ms
a connection spends not reading - aggregating bars, evaluating a strategy - is a
full simulated second of tape it never saw. Forward tests always run accelerated, so
an accelerated run is systematically further behind the tape than a real one
would be, in proportion to the speed factor and to how long the connection waits
between reads. That is a property of accelerating past the connection, not a
venue defect, and a reader of forward results should know it. Under N
instruments it is the connection's read budget that splits N ways.

No measurement is owed here: the backlog is unbounded by construction, and a
number for one host on one day would change no decision. What would give a
measurement a decision to serve is a specific forward result somebody doubts,
where the question becomes whether that run was valid.

`warmup_ns` is the required servable simulated interval before a placement.
Every river is materialized on first read; none is warmed before readiness.
`run_start_ns` is every unnamed boat's placement origin. A named window instead
places its private boat at `window_start_ns` and exposes history no earlier than
`window_start_ns - warmup_ns`; admission refuses a floor below
`TAPE_ORIGIN_NS`. Per-boat clocks vary
in wall anchor and speed but never in sim epoch. The consequence for a declared
duration is worth stating, because it reads as a venue defect otherwise: the
run deadline is judged on the venue clock, while a socket's `RunComplete` is
re-derived on its own boat's clock, which is anchored at that boat's placement.
A boat placed a moment after the venue clock was anchored therefore trails it
by that gap times `speed` - at speed 100, 180 us of placement gap is 18 ms of
simulated time - so the announcement one socket reads can carry slightly less
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
