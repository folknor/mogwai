# Optimizations

Transient. Findings, not commitments - nothing durable cites this file, and an
item that lands gets deleted from here and its surviving reasoning moved into a
code comment or into `reference/`.

## tick-composition: where the hour goes

Scope: `crates/mogwai-server/src/tick_composition.rs` and the generator emit
path it drives. This consolidates two independent analytical passes. Both are
reasoned from the code and from the run shape `reference/performance.md`
records; NEITHER ran a profiler, and no cargo or brokkr command informed the
sizing. Treat every multiplier below as a hypothesis, and see the measurement
named at the end.

The two passes agree on the finding that matters: the dominant cost is not in
the histogram code. `tick-composition` materializes the complete production wire
event stream while retaining only timestamps, frame kinds, and child counts.

`reference/performance.md` records the shape under its protocol 7 BBO
composition entry: about an hour, nearly all of it the surged arm. Surged arms
40 of the 160 combinations at `rate_mult` 1000 and `children_mult` 100, with a
duration of `u64::MAX`, so the surge never expires. Two million parents span
roughly 340 simulated seconds, but `measure` has to keep pulling to the 6,000
simulated-second fanout horizon. That continuation is about seventeen times more
parents than the measurement proper, and every event in it is consumed by
exactly one statement: the fanout bin increment. Everything else in the loop
body is already gated behind `recorded < parents`, which is false for the whole
continuation.

So the dominant workload of this command is: generate billions of fully
materialized `TickEvent`s, read `ts_event` and the discriminant, increment a
bin, drop the event.

## The rewrite: a stochastic parent kernel with two sinks

Items 1, 2 and 4 below are one architecture, not three independent changes. They
are listed separately because they can land incrementally, but the shape they
converge on is:

```
stochastic parent kernel
    |
    +-- wire sink
    |     materializes QuoteTick and TradeTick
    |
    +-- composition sink
          consumes parent metadata
          advances child state without protocol objects
          aggregates timestamp runs into bins
```

One stochastic implementation, two consumers - NOT a second "approximately
equivalent" analytical generator running beside the real one. That distinction
is the whole point. A standalone `next_measurement_tick()` bolted beside
`next_tick()` would be faster to land and would leave two stochastic paths free
to drift.

It also removes an inversion: today the core appears to generate wire objects
and the analysis has to reverse them back into parent structure.

### 1. Make the parent sweep, not the wire tick, the unit of advancement

FULL COHERENT REWRITE of the generator's emit path. Biggest expected win,
plausibly several-fold.

`measure` calls `next_tick()` once per frame, which forces the production path
to construct every `QuoteTick` and `TradeTick`. Per trade tick,
`GeneratedSource::next_child` builds:

- `self.scalars.symbol.clone()`. `Symbol` is `String`, so this is a malloc, a
  memcpy and a free per tick.
- `decimal_from_f64(price_ticks * tick_f64).round_dp(price_decimals)`.
- `next_size`, which is another `decimal_from_f64` plus `round_dp`, or
  `round_lot_size`, then a `max` against the grid minimum.

Quotes cost four `Decimal`s plus a `String`. On top of that, every frame pays a
`TickEvent` enum construction, a return across the `TickSource` abstraction, and
the measurement's own bounds checks and bin updates. `tick-composition` reads
none of the payload - it reads a `u64` timestamp and one bit of discriminant.
The surged arm's 100x children multiplier is precisely where per-frame
materialization hurts most.

The design:

1. Make a parent sweep the generator's fundamental advancement unit.
2. Have that kernel expose a compact description: `parent_ts`, `child_count`,
   and the child timestamp progression.
3. Let the production `TickSource` adapter materialize quotes and trades from
   that parent.
4. Let composition consume the parent description directly and update its
   sufficient statistics without constructing protocol objects.

Order of magnitude: a `String` clone is roughly 20 to 30ns, and three `round_dp`
calls perhaps 20 to 40ns each, against ChaCha12 draws and float math that all
remain. Plausibly half to two thirds of per-child cost, so roughly two to three
times on the whole run.

This pays twice. `mogwai-server` currently clones a symbol `String` per tick per
subscriber on the live serving path, and that clone goes away for the same
reason.

### 2. A true discard/count sink inside child generation

FULL COHERENT REWRITE, complementary to item 1 and the mechanism that makes it
safe.

Even with parent-level traversal the generator must advance every child, because
each child consumes randomness and can affect the closing price and bounce
state. Advancing a child does not require constructing a `TradeTick`.

Split each emit into a STEP half and a MATERIALIZE half:

- STEP: every `self.rng` draw, every f64 state evolution, level movement, size
  draw consumption, closing state, and the clock advance.
- MATERIALIZE: the `String` clone and the `Decimal` construction and rounding,
  nothing else.

`next_tick` becomes step plus materialize. The composition sink runs step alone
and receives a timestamp or a compact frame run. The same split applies to
quotes: book state must be retained because repeat compatibility depends on it,
but converting book prices into protocol `Decimal` fields and cloning the symbol
are not needed for composition.

The hard constraint, worth stating because it is the easy thing to get wrong:
the analysis path may omit output CONSTRUCTION, but it cannot omit `next_size()`
or price evolution. Those consume randomness, and skipping them would alter
every later draw and all path-dependent state.

Determinism is then preserved BY CONSTRUCTION rather than by assertion, because
no draw lives on the materialize side. That property is the reason the split is
safe, and it belongs in the doc comment on the seam rather than in a test that
merely notices it holds.

### 3. Two counter sets where one and a half will do

LOCAL to `tick_composition.rs`.

`measure` maintains `all` and `trades` side by side, updating `all` on every
event and `trades` on non-quotes: two bin updates per trade, one per quote. But
the file already knows, and `protocol_six_is_protocol_seven_less_one_quote_per_parent`
already asserts, that protocol 7 is protocol 6 plus exactly one quote per
parent.

Keep `trades` and a `quotes` counter set instead, and derive `all` at `finish`
time:

- `per_parent`: the protocol-7 child count is always the protocol-6 one plus
  one. Drop the `all` histogram entirely and shift the `Tail` by 1.0 in both
  fields. Exact for the interpolated p999 too, since a uniform shift commutes
  with linear interpolation.
- `per_second` and `fanout_second`: protocol 7 is the elementwise sum of the two
  arrays, computed once per task.

Halves hot-loop bin traffic, halves the L1 footprint of the two 48KB fanout
arrays, and drops a `Histogram::add` and a counter increment per parent.

Worth doing on shape alone, independently of speed: it turns the test's
assertion into a structural identity rather than an agreement two independently
maintained counters happen to reach.

### 4. Aggregate frame timestamps as runs

REWRITE of `measure`'s loop structure plus a burst-level generator API. Belongs
to the parent-kernel rewrite; implemented alone as a second traversal API it
just duplicates the abstraction.

Child timestamps are a parent timestamp plus a fixed intra-event step, and
composition needs counts per simulated second, not each timestamp
independently. `INTRA_EVENT_STEP_NS` is 1,000 and `CHILD_CAP` is 4,096, so a
full sweep spans at most 4.096ms: it lands in one second bin, occasionally two.
Yet the loop today charges a `u64` division, two bounds-checked `get_mut` calls
and two increments PER CHILD.

Have the sink report a run - quote timestamp, child start timestamp, child
count, child stride - and distribute it into second bins arithmetically. The
fanout arm becomes one add per PARENT, with a split only when a sweep crosses a
second boundary. Under surge with about a hundred children per parent, that
removes something like 99 percent of the bin bookkeeping from the phase that is
most of the run.

The draws inside the sweep still have to happen - one `ChaCha12Rng` stream,
order-dependent, so nothing can be skipped. This removes the CONSUMER cost only,
which is why it needs items 1 and 2 underneath it to be worth anything.

### 5. Schedule the long tasks first

LOCAL, a few lines, and the highest-value change that can be made entirely
inside this file. No reduction in CPU work; potentially major wall-time
reduction on high-core machines.

`tasks` emits seed, then mode, then preset, and the workers claim off a shared
`AtomicUsize`. Surged is last within every seed, INCLUDING the final seed, so
the run ends with only the five most expensive preset tasks available: once the
other workers finish, most cores sit idle through that final cohort. A work
queue only balances well if the expensive work is not concentrated at its end,
and a surged combination costs perhaps twenty times a natural one.

`results` is indexed by task position, so execution order is ALREADY fully
decoupled from emission order - reordering the claim sequence is free and
changes no output byte. Split the task list into immutable indexed work items
sorted by a stable deterministic cost class: surged first, then whichever modes
and presets historical measurement shows are slowest. Dynamic atomic claiming is
adequate once that is done; a work-stealing runtime would add little. Classic
longest-processing-time scheduling. Fixture entry order is untouched, which is
the property `task_order_is_seed_then_mode_then_preset` guards and which stays
true.

### 6. Replace full sorting with selection

LOCAL. Modest overall, potentially useful for long quiet streams.

`tail_of` sorts every sample although it needs only the two adjacent p99.9 order
statistics and the maximum. `rolling_tails` allocates two `Vec<u64>` of the full
simulated span and sorts each: six sorts per task at N log N.

Use `select_nth_unstable` for the lower and upper ranks and track the maximum
during collection - O(n log n) to expected O(n). Alternatively both windows
could feed the same counting-sort `Histogram` path `per_parent` already uses,
which drops the sorts and the allocations together; a sparse frequency map is
another option but should be justified from measured second-count distributions
first.

At a few hundred thousand seconds this is seconds per task against minutes of
generation, so it is NOT where the time is. It will not approach the benefit of
avoiding hundreds of millions of protocol-object constructions.

### 7. Fuse the rolling-window storage

LOCAL. Modest memory-bandwidth and peak-memory improvement, and mostly matters
when quiet modes cover very long simulated durations.

Composition holds dense per-second counts, a full 300-second rolling vector, a
full 24-hour rolling vector, and temporary wall-rate vectors. The two rolling
series exist only to compute tails. Absent an exact online quantile structure
their samples must be retained somehow, but they can be produced sequentially:
build one series, select its p99.9 and maximum, then reuse the allocation for
the other window. That removes one span-sized live allocation and some allocator
pressure.

### 8. Reduce hot-loop branching after the parent quota

LOCAL. Small to moderate, and currently masked entirely by generator
materialization cost.

The loop tests `recorded < parents`, the fanout horizon, a checked subtraction
and a checked index on every frame, but its behavior has two distinct phases:
record parent-duration statistics and fanout, then after the parent quota record
fanout only until the horizon. Split them into two loops. Known timestamp
invariants can also collapse the nested `checked_sub` and `get_mut` into one
range check and direct indexing.

Related, and removed by item 3 regardless: the
`[Some(&mut all), (!is_quote).then_some(&mut trades)]` array-of-options builds a
two-element array per event to iterate at most two things - the kind of
construct that blocks bounds-check elision in a loop that runs billions of
times. `SecondBins::add` likewise charges a `get_or_insert`, a compare, a
possible `resize` and a bounds-checked index per event; item 3 halves its call
count and item 4 stops the fanout phase calling it at all.

## Not worth prioritizing

- The per-result `Mutex` is acquired once per task, not per tick.
- The two relaxed atomics are touched once per task.
- JSON serialization handles 160 compact readings after an hour-long run.
- `Histogram` is already the right representation for bounded parent fanout.
- `BTreeMap` in the report, and profile resolution, do not affect throughput.
- A different thread pool fixes neither the final-task ordering nor the
  per-frame construction.

## Considered and rejected

Swapping `ChaCha12Rng` for a faster PRNG is a genuine win on paper - at two or
three f64 draws per child, a 64-byte ChaCha block is refilled every couple of
children. But it changes every tape, forces a `TAPE_PROTOCOL_VERSION` bump, and
invalidates both committed fixtures along with the four budget constants derived
from them. That is a real cost even pre-1.0, and items 1, 2 and 4 get most of
the way without it. Revisit only if a profile shows ChaCha dominating AFTER the
materialization is gone.

## Open question: does the kernel rewrite bump the tape protocol?

The two passes disagree and it needs deciding before item 1 starts.

The determinism-by-construction argument says the split cannot change output:
every draw stays on the step side, so the emitted tape is the same tape. The
conservative reading says the project rule is unconditional - any change to the
tape generation path bumps `TAPE_PROTOCOL_VERSION`, precisely because nothing
can detect that a determinism-affecting change should have bumped it and did
not, and "I reasoned that output is identical" is exactly the claim the rule
exists to distrust.

The conservative reading is the safe default and costs a fixture re-bless. Worth
noting the cost is not just the bump: it invalidates the committed fixtures and
the four budget constants derived from them, the same cost that got the PRNG
swap rejected above.

## The correctness gate

Stronger than comparing final composition reports. Run both sinks from identical
initial states across every preset, seed, mode, calendar boundary and surge
transition, and verify:

- Identical emitted production ticks when the materializing sink is used.
- Identical compact summaries derived from those ticks and reported directly by
  the counting sink.
- Identical continuation AFTER the measured interval, which is what proves the
  RNG and path-dependent state stayed aligned rather than merely agreeing over
  the sampled window.
- Golden tapes re-blessed alongside whatever the open question above decides
  about the version bump.

## Suggested order

Items 3 and 5 first: contained, single-file, and item 3 leaves the loop in the
shape the rest wants. Item 5 is pure wall-time for a few lines. Then items 1 and
2 together, which are the actual multiplier and which the live serving path
benefits from too. Then item 4 on top. Items 6, 7 and 8 are cleanup afterwards,
and item 8 is not measurable until the materialization is gone.

## The measurement that would confirm the premise

Before committing to items 1 and 2, run one surged combination with `--parents`
turned down under a sampling profiler, and check what fraction of samples sit in
`rust_decimal` and in the allocator versus in `ChaCha12Rng` generation. If
`Decimal` plus malloc is under roughly 30 percent, the sizing above is too
optimistic and item 4 becomes the lead item instead.
