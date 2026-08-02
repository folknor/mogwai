# Trades-only queue-ahead (RFC 4631 phase B) - implementation spec

Written against `reference/technical-implementation-spec.md`. Spawned from the
`notes/todo.md` entry "DECIDE, then write up and delete this entry: how much of
RFC 4631's phase B (shared queue position) mogwai can honestly build". The
landed penetration gate this spec extends is described in
`reference/architecture.md` ("Fills are synthetic - there is no matching and no
order book") and in the module docs of `crates/mogwai-server/src/fills.rs` and
`crates/mogwai-server/src/sweeper.rs`; this spec does not restate them beyond
what a brick needs.

## 1. The item and the decision

Phase B asks for a FIFO per price level accounting for all resting liquidity,
public book included. The TODO already refuses that shape and this spec keeps
the refusal verbatim: there is no L2 anywhere in this project's lineage - the
offline corpus is trades only, the committed fingerprint is fitted to trades,
`/quotes` is always empty, and the engine holds no book - so a synthesized depth
ladder would be invented microstructure shipped under a banner of fitted
realism.

What the TODO left open is the salvage: a trades-only queue-AHEAD model, where a
resting order fills only once the tape has traded enough volume at its price to
have consumed the quantity modeled as queued in front of it. The open question
was whether that quantity can be grounded in the fingerprint or would be a free
parameter, with the standing instruction that a free parameter is declined on
the same credibility test that kills the ladder.

**Decision: the quantity is grounded, conditionally, and the condition is
measured before any engine code is written.**

The grounding is this. A price LEVEL VISIT - a maximal run of consecutive prints
at one price on the tape - has a total traded volume, and that volume is
directly observable in a trades-only corpus: it is how much the market traded at
a price before the price moved off it. That is a measured scale for "how much
has to print at your price before your turn comes", and it is measurable with no
book, no quotes, and no aggressor side. So the model's parameter is not chosen
by judgement; it is a draw from a distribution fitted offline to the corpus, in
the same pipeline, committed to the same `analysis/fingerprint.json`, on exactly
the terms the dwell bound landed on.

What that quantity IS NOT, stated once and carried into every artifact that
repeats it: it is not the depth of the book at the level, and it is not a queue
position. An earlier draft of this spec called visit volume a LOWER BOUND on
resting depth. That claim is withdrawn as unsound - the biases run in both
directions and one of them is not a bias at all but a different quantity (see
the three limits below). The estimand this spec commits to is AT-TOUCH TRADED
VOLUME PER LEVEL VISIT, and the model built on it is an at-touch volume delay
before a resting limit fills: a fitted, aggressor-blind stand-in for queue
position, not a measurement of one. Every doc string, the architecture bullet
and the `QueueAheadModel` comment use that wording, not "depth" and not "the
book". The feature keeps the name QUEUE-AHEAD because that is what it stands in
for, and the name is always accompanied by the estimand.

The grounding claim is falsifiable and is therefore MEASURED FIRST (landing L1),
against an explicit proceed/close threshold (section 5.1). If the corpus says
the per-visit volume distribution is degenerate - most level visits are a single
print, and the distribution carries no information beyond "one typical trade" -
then the queue-ahead quantity IS a free parameter dressed in a histogram, the
decision flips to decline, and the item closes on the write-up in section 8
without L2 or L3 ever landing. The estimate motivates the spec; only the
measurement justifies the landing.

THREE honesty limits are stated in the model's own doc comment rather than
papered over, because together they are why the estimand is at-touch traded
volume and nothing stronger:

- DEFLATING: liquidity that rested and was CANCELLED rather than traded is
  invisible in a trades-only corpus, so visit volume understates what was
  actually resting.
- INFLATING: liquidity that JOINS the level during the visit is traded and
  counted, though it was never queued ahead of an order that rested at the
  visit's start. Replenishment therefore pushes visit volume above the opening
  queue. With biases in both directions and no way to net them in a trades-only
  corpus, no bound - upper or lower - is claimed. The number is an estimate of
  level throughput.
- SIDE-BLIND: the corpus has no aggressor side, so a level visit mixes buy- and
  sell-initiated consumption. For a resting buy, sell-aggressed volume at its
  price consumes the bid queue it sits in, while buy-aggressed volume at the
  same price consumes the OTHER side and says nothing about it. The measurement
  cannot separate the two, so it measures both. The MODEL therefore also ignores
  aggressor side when it consumes the queue, even though the synthetic tape
  carries a native `Buyer`/`Seller` flag. Estimator and consumer share one
  definition on purpose: the failure the `analysis/` harness TODO entry warns
  about (two hour-bucket conventions drifting apart across the Python/Rust seam)
  is exactly what an aggressor-filtered consumer against an aggressor-blind
  estimator would reproduce. Using the generator's synthesized aggressor flag
  would also re-import an ungrounded input into the one number this spec exists
  to ground. The cost is real and is the sharpest of the three: the model's
  delay is driven by flow that a real venue would have charged to the opposite
  queue, so the delay is systematically too short in units of same-side volume.
  That is the price of a trades-only corpus, and it is the reason the feature is
  described as a fitted volume delay rather than a queue.

Done means:

1. Level-visit volume is measured over the corpus by `analysis/characterize.py`
   in its existing single O(1)-memory streaming pass, committed as a golden
   target band in `analysis/fingerprint.json`, and read by the Rust side.
2. `mogwai-engine` gains a queue-ahead admission alongside the penetration
   count: a RESTING GTC limit fills when the tape has traded through its
   price `penetration_ticks` times (today's rule, unchanged) OR when volume
   printed AT its price since it rested exceeds its drawn queue-ahead quantity.
3. The queue-ahead quantity per order is a deterministic function of the fitted
   distribution and the order's identity (symbol, client order id, and a
   window counter the engine owns) - no RNG state, no clock, no tape
   inside the engine, so `Engine` stays the pure synchronous core the
   architecture promises.
4. The venue at `queue_ahead = off` (the default) is byte-identical to today,
   including the golden synthetic tape, which this spec does not touch at all.
5. `reference/architecture.md`, `reference/config.md` and `mogwai.toml` describe
   the model, the refusal of the L2 ladder, and all three honesty limits; the TODO
   entry is deleted and its successor entry reworded (section 8).

Explicitly excluded, named as separate items, not deferral:

- Criterion benches on the fill path and golden fill CDFs - the next
  `notes/todo.md` entry (RFC 4631 phase D). This spec is what unblocks the CDFs;
  it does not build them.
- The dead-feed watchdog and the `next_position` accumulation cap are untouched.
- The `analysis/` TEST-HARNESS DECISION - which framework, what the standing
  layout is, whether existing analysis code gets retrofitted - stays its own
  TODO entry. This spec does not settle it. It does require unit tests for the
  code IT adds (4.10, gated in L1), written in whatever the smallest runnable
  form is at landing time, because L1's whole job is to produce a number that a
  proceed/decline verdict is read off, and an untested estimator cannot support
  a verdict. If the harness entry has landed by then, these tests go in it; if
  not, they go in a single self-contained file that the harness entry later
  absorbs.
- No L2 book, no depth surface, no `/quotes` content, no per-side queue, no
  cancellation modeling, no queueing for MARKET or FOK orders (both are decided
  at submit and are never gated today).

## 2. Survey of the ground

### 2.1 The penetration gate as landed

`crates/mogwai-engine/src/lib.rs`:

- `OpenOrder` carries `penetration_count: u32`, `penetration_scanned_ns: u64`
  and `revision: u64`. A reprice and a FILL both restart the window; a quantity
  amend does not.
- `EngineConfig { account_id, instruments, balances, penetration_ticks }` is the
  one construction path. The doc comment states the invariant this spec must
  preserve: "The core receives observations, never a tape or clock;
  `penetration_ticks` is therefore a pure model knob."
- `PendingScan { client_order_id, symbol, side, price, from_ns, remaining,
  revision }` and `ScanResult { client_order_id, from_ns, revision, counted,
  scanned_to_ns }` are the whole seam. `pending_scans()` returns nothing at all
  when `penetration_ticks == 0`.

`crates/mogwai-engine/src/orders.rs`:

- `on_submit` gates only `OrderType::Limit` with `Gtc`/`Ioc`, seeds one
  penetration when the server's `market_px` reading is already through the
  limit, and short-circuits into the resting/canceled path before `plan_fill`
  runs - so no divergence is consumed under the gate.
- `apply_scans` re-matches each result on `client_order_id` AND `revision` AND
  `penetration_scanned_ns == result.from_ns`, then accumulates `counted`,
  advances the frontier to `scanned_to_ns`, bumps `revision`, and executes if
  the threshold is met. It returns `(events, emitted)` where `emitted` is the
  count of orders that actually produced a fill, which is what the caller
  reserves wire bytes against.
- `commit_fill` is the single shared execution path for both `on_submit` and
  `apply_scans`; the fill is at the ORDER'S price, never the penetrating trade's.

`crates/mogwai-server/src/fills.rs`:

- `count_penetrations(symbol, scans, to_ns, profiles, data_origin) ->
  Option<Walk>` walks ONE clean-tape source per symbol, at most
  `SWEEP_DRAIN_BUDGET = 20_000` ticks, counting per scan into `Walk.counted` and
  reporting `Walk.reached_ns` / `Walk.drained`.
- `through(side, limit, traded)` is strict (`traded < limit` for a buy). The
  same predicate is duplicated in `orders.rs` for the acceptance-time seed; both
  copies must keep agreeing.
- The early exit fires when every scan has reached its `remaining`.

`crates/mogwai-server/src/sweeper.rs`:

- `spawn_account_sweeper` is spawned by `AccountRegistry::acquire` only when
  `penetration_ticks > 0`. Per pass: take `pending_scans()` under the engine
  lock, sample `to_ns` ONCE, group by symbol, run each walk on `spawn_blocking`,
  then apply under the lock. `MIN_SWEEP_WALL = 5ms` floors the converted
  interval.

`crates/mogwai-server/src/config.rs`:

- `penetration_ticks` (`MAX_PENETRATION_TICKS = 1_000`) and
  `fill_sweep_interval_ms` (default 100, `SLOW_SWEEP_WARN_MS = 60_000`).
  `validate_penetration` refuses `penetration_ticks > 0` with
  `fill_sweep_interval_ms == 0`, on the stated principle that a venue must not
  accept limits it can never execute. This spec adds one refusal in the same
  place and on the same principle.
- `Config` is `#[serde(default, deny_unknown_fields)]`, as is
  `ConfiguredInstrument`. A new top-level knob must therefore be added to the
  struct, to `Default`, and to `mogwai.toml`'s documented set.

### 2.2 The size and volume material that already exists

- The corpus CSV is `ts,price,size,...`; `analysis/characterize.py` reads it in
  ONE streaming pass with O(1) memory (the multi-GB constraint), already parsing
  `sz` for `size_log_hist`, `size_dec_hist` and `round_frac`.
- Two properties of that existing size material disqualify it as-is for this
  spec's normalizer, and both are fixed in 4.1 rather than inherited:
  - `size_log_hist` is accumulated for EVERY parsed trade, outside the
    `ts >= DWELL_ERA_START_TS` guard that the dwell block sits behind. Its
    median is therefore a full-corpus statistic. Dividing an era-windowed visit
    volume by a full-corpus median would mix two conventions in one ratio -
    precisely the Python/Rust convention drift the aggressor argument above
    invokes. This spec adds a SEPARATE era-windowed size accumulator for the
    normalizer and leaves `size_log_hist` untouched (an existing committed
    target reads from it; 4.3 forbids perturbing it).
  - The binning is `int(math.log10(sz)) + 9`, and `int()` truncates TOWARD ZERO,
    so `0.5` and `5.0` both land in bin 9 while `0.05` and `0.5` do not share
    one. The bins are not regular decades around 1 and the resulting quantiles
    are not reproducible from the bin index alone. The visit-volume histogram
    therefore does NOT reuse this binning; 4.1 defines its own with `floor`
    semantics, explicit edges, and finer-than-decade resolution.
- `char_<PAIR>.json` carries `size.log10_hist` (30 log-decade bins),
  `size.decimals_used_hist`, `size.round_frac`.
- `analysis/build_fingerprint.py` promotes anchor values and cross-pair
  min/median/max bands into `fingerprint.json` under `golden_targets` and
  `scalar_ranges`. Only `size_round_frac` of the size material is promoted
  today; the size DISTRIBUTION is not a committed target.
- `crates/mogwai-data/src/generated/fingerprint.rs` deserializes
  `fingerprint.json` WITHOUT `deny_unknown_fields`, so a new JSON key parses
  cleanly before any Rust reads it. This is what lets L1 land alone and green.
- The generator's trade sizes come from `LogNormal(ln(typical_size),
  SIZE_LOG_SIGMA = 1.15)` in `generated/source.rs`, where `typical_size` is a
  per-instrument `GeneratorScalars` field (default `0.1`, operator-settable per
  `[[instrument]]` table via `generator.typical_size`). So the tape's size SCALE
  is a declared per-instrument scalar, not a fitted one. This is why the fitted
  queue-ahead distribution must be committed DIMENSIONLESS - as a multiple of
  the pair's own median trade size - and scaled at use by that instrument's
  `typical_size`. A queue-ahead quantity committed in absolute base units would
  be meaningless against any instrument the operator configures.
- `TickEvent::Trade(TradeTick { symbol, price, size, aggressor, ts_event })` -
  the walk in `fills.rs` already has `size` in hand and currently ignores it.
- `InstrumentProfiles` in `crates/mogwai-server/src/source.rs` exposes only
  `get()` and `instrument_defs()`. There is no way to iterate profiles, so 4.9's
  "the `typical_size` of every configured instrument" needs a NEW accessor; it
  is named as a target artifact there rather than left implicit.

### 2.3 What pins the current behavior (the re-bless / must-still-pass set)

- `mogwai-engine` unit tests in `lib.rs`:
  `zero_penetration_ticks_fills_on_submit_exactly_as_before`,
  `a_price_amend_restarts_the_penetration_window_and_quantity_amend_preserves_it`,
  `an_executed_order_restarts_its_penetration_window`, and the `result()`/
  `engine_with(ticks)` helpers around line 638.
- `mogwai-server` `fills.rs` unit tests, including the two COST gates -
  `a_pass_costs_one_walk_per_symbol_not_per_order` and
  `sweep_pass_walks_only_the_new_span` - plus
  `counts_only_prints_strictly_through_the_limit`,
  `counting_stops_at_the_threshold`, `a_truncated_drain_reports_where_it_stopped`
  and `the_counted_prints_are_the_prints_trades_serves`.
- `mogwai-server/src/main.rs` integration tests that build engines with
  `penetration_ticks: ticks` (found by that field, not by line number - the
  file's line numbers drift between the writing of this spec and its landing).
- `scripts/smoke.py --penetration` / `--penetration-swept` against
  `scripts/smoke-penetration.toml` and `scripts/smoke-penetration-two.toml`.
- `mogwai-data`'s realism gate and `clean_regime_is_byte_identical`. This spec
  changes NO generator code and no fingerprint value the generator reads, so
  these must pass unchanged and un-re-blessed. That is a gate, not a hope: L1
  adds only new JSON keys, and section 4.3 forbids touching an existing one.

### 2.4 Sibling-survey reconciliation

The only sibling spec covering this ground is the arrival-drought elimination
work landed in commit fea62b5, which named penetration-gated fills as
unblocked-and-later. Its spec file lived under `docs/` and was deleted on
landing per the transient-docs rule, so the COMMIT is the citation and there is
no path to cite. Its survey establishes two facts
this spec depends on and does not re-derive: the fingerprint JSON tolerates new
keys ahead of the Rust reader, and `characterize.py`'s single pass must stay
O(1) in memory. Nothing in it contradicts this spec's premise.

## 3. Design decisions, resolved here

### 3.1 Disjunction, not a second sequential gate

An order fills at the first instant EITHER condition holds:

1. `penetration_count >= penetration_ticks` - a print strictly through the limit
   happened often enough. Unchanged.
2. queue-ahead enabled AND `volume_at > queue_ahead` - prints AT the limit price
   since the window opened have traded more than the modeled queue in front of
   the order.

Disjunction is the correct composition, not a convenience. Each is
independently sufficient in book terms: a print strictly THROUGH your price
means everything resting at your price was consumed and the aggressor kept
going, so you were filled; enough volume AT your price means the queue ahead of
you was consumed even though the level held. Requiring both would model a venue
where clearing the level is not enough to fill you, which is wrong. Making
queue-ahead an alternative way to earn "one penetration" would be shoehorning
the volume model into the count's units and would misbehave for
`penetration_ticks > 1`.

Consequence, pinned by a test: with queue-ahead enabled, an order can fill on a
span in which `counted == 0`.

### 3.2 The gate is AT the price, the clear is THROUGH it

`volume_at` accumulates the `size` of prints whose price EQUALS the limit
(`Decimal` comparison on the instrument's price grid; both the tape and the
order are rounded to `price_increment`, and `validate_submit` already refuses an
off-grid price, so equality is well defined and not a float hazard). Prints
strictly through the limit feed the existing count and do NOT feed `volume_at` -
they are the other disjunct, and double-counting them would make a fast market
fill an order twice as easily through two paths that describe the same event.

`through()` in `fills.rs` and its twin in `orders.rs` are unchanged.

### 3.3 The draw is a pure function of order identity, computed in the engine

The queue-ahead quantity must be computable INSIDE `apply_scans` (a reprice
arriving between passes must face a new queue), so it cannot be a value the
server injects once at submit time the way `market_px` is.

It must also not put RNG state, a clock, or a tape into `Engine`.

Both hold if the draw is a pure inverse-CDF sample indexed by a hash of the
order's identity:

```
queue_ahead(symbol, client_order_id, window)
  = typical_size(symbol) * quantile(u01(fnv1a64(symbol, client_order_id, window)))
```

`quantile` is the fitted dimensionless distribution's inverse CDF at a
`[0, 1)` point derived from the hash. No mutable state, no ordering dependence,
reproducible across restarts, and independent per order and per window.

The index is a NEW `window: u32` field on `OpenOrder`, NOT `revision`. This is
load-bearing and an earlier draft got it wrong. `revision` is bumped on every
frontier advance in `apply_scans`, not only at window starts, so its value at
the moment of a fill is a function of how many sweep passes have run - which is
a function of `fill_sweep_interval_ms`, of drain-budget truncation, and of pass
timing. Indexing the draw on `revision` would make a repriced or resized order's
new queue depend on wall-clock scheduling - two runs at different
`fill_sweep_interval_ms`, or a restart mid-order, would give different queues -
contradicting the reproducibility claim this whole subsection rests on. `window`
starts at 0 at acceptance and is incremented ONLY at a reprice or a quantity
increase (3.7 explains why the post-fill remainder is not a redraw at all).
Nothing else writes it.

Encoding of the hash input is fixed here so two implementations agree: FNV-1a
64-bit over the concatenation of the symbol's UTF-8 bytes, a single `0x00`
separator, the client order id's UTF-8 bytes, another `0x00`, and the eight
big-endian bytes of `window`. The `[0, 1)` point is `(h >> 11) as f64 /
(1u64 << 53) as f64`, which is exact in `f64` and never reaches 1.0.

### 3.4 Where the model type lives

`QueueAheadModel` is defined in `mogwai-engine`, not in `mogwai-protocol` and
not in `mogwai-data`:

- It is execution semantics, which is the engine's charter, and it is never
  serialized onto the wire, so `mogwai-protocol` would be the wrong home
  (`mogwai-protocol` is "the wire types ... the single source of truth both ends
  serialize against").
- The engine must not depend on `mogwai-data`; it depends on `mogwai-protocol`,
  `rust_decimal` and `tracing` only, and this spec adds no dependency.

The SERVER constructs the model from the parsed fingerprint plus the per-symbol
`typical_size` it already holds in `InstrumentProfiles`, and hands it to
`EngineConfig`. Fitted numbers flow from `mogwai-data` into the engine as plain
values, exactly as instrument definitions and balances already do.

### 3.5 One knob, and it requires the penetration gate

`queue_ahead_enabled: bool`, default `false`. Rejected alternatives:

- A queue-ahead multiplier knob - that is the free parameter this spec exists to
  avoid. The whole point is that the operator does not get to set the queue
  size; the corpus does.
- Reusing `penetration_ticks > 0` alone to imply queue-ahead - it would change
  the behavior of every existing gated config and every landed penetration test
  silently.

Boot refuses `queue_ahead_enabled = true` with `penetration_ticks == 0`, in
`validate_penetration`, with the same justification as the existing sweep
refusal: with `penetration_ticks == 0` an order fills on submit and never rests,
so `pending_scans()` returns nothing, no walk runs, and the queue-ahead model
would be configured but structurally unreachable. A venue must not silently
ignore a knob the operator set.

### 3.6 The walk stays one pass per symbol

`count_penetrations` gains a per-scan `Decimal` accumulator in the SAME loop
over the SAME source. No second walk, no second checkpoint restore, no extra
acquisition of the process-wide checkpoint mutex. The early exit becomes "every
scan has met its count threshold OR its volume threshold". The two cost gates in
section 2.3 are extended rather than replaced.

Precisely what those gates pin, and what they do not: they pin WALK COUNT - one
walk per symbol per pass, over only the new span. They do not pin PER-TICK COST,
and the per-tick cost does rise. Each tick now costs up to `scans.len()`
additional `Decimal` equality comparisons plus a `Decimal` add, inside a loop
already bounded by `SWEEP_DRAIN_BUDGET = 20_000`, and `Decimal` compare and add
are not free relative to the existing `through()` compare. This spec does not
measure that; the criterion benches on the fill path are the phase D entry and
are explicitly excluded (section 1). The claim made here is therefore the narrow
one - no new walk, no new source acquisition, no new lock - and the constant
factor inside the existing walk is knowingly unmeasured. Do not read the two
extended cost tests as evidence about it.

### 3.7 The queue lifecycle is derived from queue semantics, not copied from the count

The penetration count is a counter of events and resets wholesale on every
window restart. A queue is not a counter, and the three lifecycle transitions
are decided here on queue semantics rather than by mirroring `penetration_count`
- an earlier draft mirrored it and got two of the three wrong.

- POST-FILL REMAINDER: the remainder KEEPS ITS PRIORITY. It does not redraw. If
  volume at your price reached the front of your order, everything that was
  queued ahead of you is gone by definition; a fresh random queue would put
  liquidity in front of an order that just traded, which no venue does. So on a
  remainder, `queue_ahead` is set to `Some(Decimal::ZERO)` and `volume_at` is
  reset to zero, meaning the next at-price print of any size completes the
  remainder (the comparison is strictly greater, so a zero-size print does not).
  `penetration_count` still resets exactly as today - that rule is unchanged and
  this spec does not touch it. Blast radius is small: `plan_fill` produces a
  remainder only under an armed `PartialFillNext`, so the clean path never
  reaches this transition.
- REPRICE: full redraw. A repriced order is a new order at a new level and goes
  to the back of that level's queue. `window` increments, `queue_ahead` is
  redrawn, `volume_at` resets. Same sites where `penetration_count` already
  resets on a reprice.
- QUANTITY AMEND: a DECREASE keeps the queue (`queue_ahead`, `volume_at` and
  `window` all untouched), matching the existing count-preserving rule and
  matching real venues, which do not penalize a reduction. An INCREASE redraws
  `queue_ahead` and resets `volume_at`, bumping `window`: added quantity must
  not inherit the priority the original quantity earned. This is the one place
  the queue's lifecycle deliberately DIVERGES from `penetration_count`, whose
  reset rule on any quantity amend stays exactly as landed. The divergence is
  justified because a count of penetrating prints is a property of the price and
  the amend does not move the price, whereas queue position is a property of the
  order's place in a line and new quantity joins that line at the end. Pinned by
  a test that a decrease and an increase behave differently.

One consequence is accepted rather than fixed: draws are independent per order,
so two mogwai orders resting at the same price can fill out of submission order.
A single shared FIFO across accounts and orders would be phase B's real shape,
which section 1 refuses for lack of an L2 lineage. The model gives each order an
INDEPENDENT delay drawn from the same fitted distribution; it does not model
relative priority between two mogwai orders. That is stated in the
`QueueAheadModel` doc comment alongside the three biases.

### 3.8 Partial fills, IOC, and the divergence surface are untouched

`plan_fill` still sizes off leaves, `PartialFillNext` still applies, the fill is
still at the order's price, and the account snapshot is still one per batch.

A gated IOC is untouched for a STRUCTURAL reason, which is the one to assert.
Under the gate `on_submit` emits `OrderCanceled` and calls `record_closed`
immediately; the record never enters `self.open`. So by the time any sweep pass
runs there is no order for `pending_scans()` to emit, no scan, and no walk. The
volume argument (no elapsed span means no volume at its price, so the disjunct
is trivially false) is true but second-order, and a test asserting only that
would still pass if the order were wrongly rested. `an_ioc_is_never_gated_into_the_book`
therefore asserts the structural fact: with queue-ahead on, a gated IOC produces
exactly today's accepted-then-canceled sequence and `pending_scans()` is empty
afterwards. IOC behavior is bit-for-bit what it is today. That is a property,
not an omission.

## 4. Target artifacts

### 4.1 `analysis/characterize.py`

A dedicated log-binning, NOT the `size_log_hist` one (2.2 explains why that one
is unusable here: truncation toward zero makes its bins irregular around 1, and
whole decades quantize the model to powers of ten). Defined once as module
constants and reused by both the visit-volume histogram and the new era-windowed
size accumulator, so numerator and denominator share a definition:

```
LVL_LOG_LO   = 1e-6     # inclusive lower edge, base units
LVL_LOG_HI   = 1e6      # exclusive upper edge
LVL_PER_DEC  = 10       # bins per decade
LVL_BINS     = 120      # 12 decades * 10
def lvl_bin(x):
    # floor, not int(): regular bins across the whole support, including x < 1
    if x < LVL_LOG_LO: return 0
    b = int(math.floor(math.log10(x / LVL_LOG_LO) * LVL_PER_DEC))
    return min(LVL_BINS - 1, max(0, b))
```

Ten bins per decade puts adjacent quantiles about 26 percent apart instead of a
factor of ten, which is the resolution the 5.1 dispersion gate and the inverse
CDF both need. The edges are constants, so the histogram is independently
reproducible from the committed array plus these three numbers - and 4.2 commits
them alongside the array rather than leaving the support implicit.

Inside the existing per-line loop, an O(1) level-visit accumulator:

```
visit_px       = None    # price of the level currently being visited
visit_vol      = 0.0     # summed size within the visit
visit_n        = 0       # prints within the visit
visit_open_ok  = False   # the visit opened in-era, so it may be closed and kept
visit_vol_hist = [0] * LVL_BINS
visit_n_hist   = [0] * 12   # 1,2,...,10,11-20,21+ prints per visit
visit_count    = 0
visit_single   = 0       # visits closed with exactly one print
lvl_size_hist  = [0] * LVL_BINS   # era-windowed trade sizes, the normalizer
lvl_size_n     = 0
```

On each accepted trade: if `px == visit_px`, accumulate; otherwise close the
open visit (bin it, count it, increment `visit_single` when `visit_n == 1`) and
open a new one at `px`. Close the final visit after the loop.

Era handling, mechanized rather than asserted. Both accumulators are era-windowed
on `DWELL_ERA_START_TS` exactly like the dwell statistics, so the queue target
and its normalizer are judged over the same modern era the tape claims, and the
two share one window. Streaming makes the boundary explicit: `lvl_size_hist`
takes only trades with `ts >= DWELL_ERA_START_TS`; the visit accumulator sets
`visit_open_ok = (ts >= DWELL_ERA_START_TS)` at the instant a visit OPENS, and a
visit is binned on close only when `visit_open_ok` is true. A visit opened before
the boundary and closed after it therefore falls on the floor entirely rather
than contributing a truncated volume, which is the "dropped rather than
half-counted" rule made executable. The final visit at end of file is closed and
kept under the same rule.

Straddling is the only boundary case; the file is time-ordered, so a visit can
cross the boundary at most once.

Emitted as a new top-level `"level"` dict in `char_<PAIR>.json`:

```
"level": {
  "era_start_ts": DWELL_ERA_START_TS,
  "n_visits": int,
  "single_print_frac": float,
  "bin_lo": LVL_LOG_LO,
  "bin_hi": LVL_LOG_HI,
  "bins_per_decade": LVL_PER_DEC,
  "vol_hist": [120 ints],
  "n_hist": [12 ints],
  "size_median": float,    # ERA-WINDOWED median trade size, from lvl_size_hist
  "vol_p50_norm": float,   # visit volume / era-windowed median trade size
  "vol_p90_norm": float,
  "vol_dispersion": float,   # vol p90 / vol p50, from vol_hist
  "size_dispersion": float   # trade-size p90 / p50, from lvl_size_hist
}
```

The two dispersion ratios exist for gate 5.1 conditions 2 and 3; both come out
of histograms already accumulated, so neither costs a pass or an array.

`vol_p50_norm` / `vol_p90_norm` are derived at emit time from `vol_hist` and the
`lvl_size_hist` median, so no second pass and no retained per-trade array. Both
are histogram-quantile estimates over 10-bins-per-decade support, so a quantile
carries roughly 26 percent resolution rather than a factor of ten - which is
what makes the 5.1 dispersion gate a real test instead of a one-bit one. The
normalizer is the era-windowed `size_median`, never `size.log10_hist`'s
full-corpus median (2.2).

Memory added: 232 ints and six scalars per pair. O(1) holds.

### 4.2 `analysis/build_fingerprint.py`

Adds to `golden_targets`:

```
"level_queue": {
  "era_start_ts": anchor level era_start_ts,
  "_doc": "AT-TOUCH TRADED VOLUME per level visit, era-windowed, expressed as a
           multiple of the same era's median trade size. NOT book depth and NOT
           queue position: cancelled liquidity is invisible so the number is
           deflated, liquidity that joined the level mid-visit is counted so it
           is also inflated, and the corpus has no aggressor side so buy- and
           sell-initiated flow at one price are summed together. The
           queue-ahead fill model samples support_norm/pmf as an inverse CDF;
           single_print_frac is the credibility reading the model's landing was
           judged against.",
  "single_print_frac": {anchor, range{min,median,max}},
  "vol_p50_norm":      {anchor, range{min,median,max}},
  "vol_p90_norm":      {anchor, range{min,median,max}},
  "vol_dispersion":    {anchor, range{min,median,max}},
  "size_dispersion":   {anchor, range{min,median,max}},
  "support_norm": [120 floats],
  "pmf":          [120 floats summing to 1.0]
}
```

The pair `support_norm` / `pmf` is the whole sampled artifact and it is
SELF-DESCRIBING - an earlier draft committed a bare 30-float array and told the
Rust side to reconstruct quantities from bin indices it could not see, which is
not reconstructible and is the reason the support is now serialized.
`support_norm[i]` is the anchor pair's bin-`i` REPRESENTATIVE QUANTITY divided
by the anchor's era-windowed `size_median`: the geometric centre of bin `i`,
i.e. `LVL_LOG_LO * 10**((i + 0.5) / LVL_PER_DEC) / size_median`. It is a
re-labelling of bin centres in dimensionless units, not a re-binning - no mass
moves between bins, and dividing by a median (a shift of `log10(size_median)` in
log space, generically non-integer) could not be a bin shift anyway.
`support_norm` is strictly increasing by construction. `pmf[i]` is the anchor's
`vol_hist[i]` normalized to sum to 1.0. Trailing and leading all-zero bins are
KEPT rather than trimmed, so the two arrays stay index-aligned with `vol_hist`
and the array length is a constant of the binning.

The three bands are the credibility reading and the documentation; the anchor's
`bin_lo`/`bin_hi`/`bins_per_decade` are copied in as a sibling `binning` dict so
the file records how `support_norm` was derived.

`build_fingerprint.py` refuses to emit the block if any pair's `level.bin_lo`,
`bin_hi` or `bins_per_decade` disagree with the anchor's, since the cross-pair
bands would then be comparing incomparable histograms.

The existing `rng()`/anchor helpers are reused; no existing key changes value.

### 4.3 `analysis/fingerprint.json`

Regenerated by L1. The DIFF MUST BE ADDITIVE: the only committed change is the
new `golden_targets.level_queue` subtree. If regeneration perturbs any existing
value, L1 stops and the perturbation is investigated before anything else lands
- an existing target moving would re-bless the generator's golden stream, which
this spec has no business doing.

`analysis/char_*.json` is GITIGNORED (`.gitignore` line 15). The new `level`
dicts in those files are intermediates, not landing artifacts, and no earlier
listing of them as committed output is correct. Only `analysis/fingerprint.json`
lands. This also means the additivity check cannot be a `git diff` on the char
files; it is a diff on `fingerprint.json` alone, and 5's gate mechanizes it
rather than eyeballing `--stat` (a `--stat` line count cannot distinguish an
added key from a changed one, so the gate compares the pre-image explicitly).

### 4.4 `crates/mogwai-data/src/generated/fingerprint.rs`

New serde structs mirroring 4.2, hung off `GoldenTargets` as
`pub level_queue: LevelQueue`.

There is NO `Fingerprint::validate` today - only `SessionProfile::validate` and
`GeneratorScalars::validate` exist, and `Fingerprint::from_repo_json` calls the
former directly. So this landing adds `LevelQueue::validate` in that same
per-struct style and calls it from `from_repo_json` next to the existing
`session_profile.validate()` call, with the same `expect` convention. Arms:
`support_norm` and `pmf` must be the same length and non-empty;
`support_norm` strictly increasing and all entries positive; `pmf` non-negative
and summing to 1.0 within the existing tolerance convention; `single_print_frac`
in `[0, 1]`; `vol_p50_norm` and `vol_p90_norm` positive with
`vol_p90_norm >= vol_p50_norm`. Lengths are checked against each other, not
against a hardcoded 30 or 120, so a later change to the binning constants does
not require a Rust edit.

No generator code reads any of this - `GeneratedSource` is untouched.

### 4.5 `crates/mogwai-engine/src/lib.rs`

```rust
/// The fitted at-touch volume distribution plus the per-instrument size scale
/// it is expressed in. A pure value: no RNG state, no clock, no tape - the
/// draw is an inverse-CDF sample indexed by a hash of the order's identity, so
/// the engine stays the synchronous observation-driven core.
///
/// The sampled quantity is AT-TOUCH TRADED VOLUME PER LEVEL VISIT, not book
/// depth and not a queue position: see `golden_targets.level_queue._doc` in
/// `analysis/fingerprint.json` for the estimand and its three biases
/// (cancellation deflates it, replenishment inflates it, and it is
/// aggressor-blind). Draws are independent per order, so this models a delay,
/// not relative priority between two mogwai orders at one price.
#[derive(Debug, Clone)]
pub struct QueueAheadModel {
    /// Inverse CDF as (quantity, cumulative probability) pairs, ascending in
    /// both, quantity expressed as a MULTIPLE of the instrument's typical
    /// trade size. Built from `support_norm` and the running sum of `pmf`.
    cdf: Vec<(Decimal, f64)>,
    /// Per-symbol trade-size scale, from each instrument profile's
    /// `generator.typical_size`. A symbol absent here is never gated by
    /// volume - `draw` returns `None` and the order falls back to the
    /// penetration count alone.
    typical_size: HashMap<Symbol, Decimal>,
}

impl QueueAheadModel {
    pub fn new(cdf: Vec<(Decimal, f64)>, typical_size: HashMap<Symbol, Decimal>) -> Self;

    /// The volume this order must see print at its price before it fills.
    /// Deterministic in the three arguments and nothing else.
    pub fn draw(&self, symbol: &Symbol, id: &ClientOrderId, window: u32) -> Option<Decimal>;
}
```

Inverse-CDF boundary rules, fixed here because "inverse CDF" alone does not
determine them: let `u` be the `[0, 1)` point of 3.3. `draw` returns the
quantity of the FIRST entry whose cumulative probability is strictly greater
than `u` (equivalently, `partition_point` on `cum <= u`), scaled by
`typical_size`. Bins with zero mass are therefore unreachable. Since the final
cumulative probability is 1.0 within tolerance and `u < 1.0`, the search always
finds an entry; if floating-point summation leaves the last entry marginally
below `u`, the last entry is returned. An empty `cdf` is refused at construction
rather than handled here.

`OpenOrder` gains:

```rust
/// The volume that must print at this order's price before it fills, fixed at
/// window start (acceptance or reprice) and set to ZERO on a post-fill
/// remainder, which keeps its priority rather than redrawing. `None` when
/// queue-ahead is off or the symbol has no size scale, in which case only
/// `penetration_count` can fill the order. See 3.7 for the lifecycle.
pub queue_ahead: Option<Decimal>,
/// Volume printed AT `submit.price` since the window opened.
pub volume_at: Decimal,
/// Window generation, incremented ONLY at a reprice or a quantity INCREASE.
/// Indexes the draw. Deliberately not `revision`, which also advances on
/// frontier advances and would make the draw depend on sweep timing (3.3).
pub window: u32,
```

`EngineConfig` gains `pub queue_ahead: Option<QueueAheadModel>` (`None` in
`unbound`). `ScanResult` gains `pub volume_at: Decimal` - volume printed at the
price in `(from_ns, scanned_to_ns]`.

`PendingScan` gains `pub queue_remaining: Option<Decimal>`, NOT the raw stored
draw. It is `queue_ahead - order.volume_at` clamped at zero, computed fresh each
time `pending_scans()` runs. This is the fix for a cross-pass defect: `volume_at`
on the order is CUMULATIVE across passes, so a scan carrying the full original
draw would make the walk compare one pass's delta against the whole queue and
never early-exit for an order that is one unit short. The walk's own accumulator
stays per-pass (the engine adds the delta), and only the THRESHOLD it is
compared against is what shrinks. A cross-pass test pins this (4.10).

`pending_scans()` is UNCHANGED in which orders it emits - it still returns
nothing when `penetration_ticks == 0`, and it still emits every open gated
order. An earlier draft said it "now emits a scan for an order that is short on
EITHER disjunct"; that is a no-op condition, because an order that met either
disjunct has already filled and is no longer open, and an implementer coding it
would write a branch that cannot differ. The only change is the new
`queue_remaining` field carried through.

### 4.6 `crates/mogwai-engine/src/orders.rs`

- `on_submit`: at BOTH `OpenOrder` construction sites (the gated one and the
  rest-after-fill one), `window = 0`, `volume_at = Decimal::ZERO`, and
  `queue_ahead = self.queue_ahead.as_ref().and_then(|m| m.draw(&symbol, &id, 0))`.
  The gate's short-circuit condition is unchanged (`seeded < penetration_ticks`)
  - a GTC cannot have consumed any volume at acceptance, and a gated IOC never
  rests at all, per 3.8.
- `apply_scans`: accumulate `order.volume_at += result.volume_at` next to
  `penetration_count`, and replace the single threshold test with the
  disjunction of 3.1. On a REMAINDER, set `volume_at = Decimal::ZERO` and
  `queue_ahead = Some(Decimal::ZERO)` when queue-ahead is on for the symbol
  (`None` otherwise); do NOT bump `window` and do NOT redraw - the remainder
  keeps its priority (3.7).
- In the amend path, at the site where `penetration_count` is already reset on a
  reprice: on a REPRICE, bump `window`, redraw `queue_ahead` at the new window,
  reset `volume_at`. On a QUANTITY INCREASE, do exactly the same three things
  even though `penetration_count` is preserved there. On a QUANTITY DECREASE,
  touch none of the three. 3.7 carries the reasoning; the amend path must
  therefore distinguish increase from decrease, which it does not need to do
  today for the count.
- Those are the only places `queue_ahead`, `volume_at` or `window` are written
  after acceptance.

### 4.7 `crates/mogwai-server/src/fills.rs`

`Walk` gains `pub(crate) volume_at: Vec<Decimal>`, parallel to `counted`. Inside
the existing loop, for each scan whose `from_ns` is passed: if
`trade.price == scan.price` AND that scan has not already met its volume
threshold, `volume_at[index] += trade.size`. The early exit tests, per scan,
`counted[i] >= scan.remaining || met_volume(i)` where `met_volume` is
`scan.queue_remaining.is_some_and(|q| volume_at[i] > q)`.

The "has not already met" guard mirrors the existing `counted[index] <
scan.remaining` guard. It is not needed for correctness within a single 20k-tick
walk, but leaving the accumulator running past its threshold while the count
next to it stops is an asymmetry a reader will trip over, and it keeps the
reported `volume_at` bounded at "just past the threshold" rather than "whatever
the rest of the span happened to print".

Strictly greater, not greater-or-equal: consuming EXACTLY the volume queued
ahead of you leaves you at the front of the queue, not filled.

### 4.8 `crates/mogwai-server/src/sweeper.rs`

Threads `walk.volume_at` into each `ScanResult`. No structural change: same one
walk per symbol per pass, same `spawn_blocking`, same lock discipline.

### 4.9 `crates/mogwai-server/src/config.rs`, `accounts.rs`, `main.rs`

- `Config` gains `pub(crate) queue_ahead_enabled: bool` (default `false`) with a
  doc comment stating what it turns on, that it needs `penetration_ticks > 0`,
  and that the queue quantity itself is fitted and not configurable.
- `validate_penetration` gains the refusal of 3.5.
- `source.rs` gains the accessor 2.2 names as missing: `InstrumentProfiles`
  exposes only `get()` and `instrument_defs()` today, so add
  `pub(crate) fn profiles(&self) -> impl Iterator<Item = &InstrumentProfile>`
  (or a `typical_sizes()` returning the map directly, if nothing else needs the
  profiles). Without it 4.9's next bullet has nothing to iterate.
- `main.rs` builds `QueueAheadModel` from `source::fingerprint()` plus the
  `typical_size` of every configured `InstrumentProfile`, when the knob is on,
  and passes it into the engine template. Any configured instrument whose
  `typical_size` is missing or non-positive is `tracing::warn`ed AT CONSTRUCTION,
  naming the symbol and saying it will fill on the penetration count alone.
  `draw` returning `None` is the right default but it is a silent one, and 3.5
  refuses to let a venue silently ignore a knob the operator set; the same
  posture applies to silently exempting one instrument from it.
- `accounts.rs`'s `EngineTemplate` carries the `Option<QueueAheadModel>`
  alongside `penetration_ticks`; the sweeper spawn condition
  (`penetration_ticks > 0`) is unchanged, since 3.5 makes queue-ahead a subset.

### 4.10 Tests

`mogwai-engine` unit tests (new):

- `queue_ahead_fills_an_order_the_tape_never_traded_through` - a scan with
  `counted == 0` and volume above the draw fills. This is the disjunction.
- `consuming_exactly_the_queue_does_not_fill` - strictly-greater boundary.
- `a_filled_tranche_keeps_its_priority` - after a partial fill the remainder has
  `queue_ahead == Some(ZERO)`, `volume_at == ZERO`, and an unchanged `window`;
  the next at-price print of any size completes it. Asserts EXACT values, not
  "differs from before". An earlier draft specified
  `a_filled_tranche_redraws_its_queue_ahead` asserting the new draw differs from
  the old, which is not a valid assertion against a discrete distribution: two
  legitimate independent samples may land in the same bin, so the test would be
  flaky by construction. No test in this spec asserts that two draws differ.
- `a_reprice_redraws_the_queue`, `a_quantity_increase_redraws_the_queue`, and
  `a_quantity_decrease_preserves_the_queue` - three separate tests, because 3.7
  makes the two quantity directions behave differently and a single test named
  for "a quantity amend" would hide it. Each asserts `window` and `volume_at`
  alongside `queue_ahead`, and each asserts `penetration_count` still follows
  its landed rule.
- `an_ioc_is_never_gated_into_the_book` (3.8) - structural: the gated IOC's
  event sequence is today's, and `pending_scans()` is empty afterwards.
- `queue_ahead_disabled_is_indistinguishable_from_today` - drives the existing
  gated fixtures with `queue_ahead: None` and asserts the same event sequence.
- `the_draw_is_deterministic_in_symbol_id_and_window` - same three inputs give
  the same quantity across two independently constructed models, and the
  quantity is unchanged by how many `ScanResult`s (hence `revision` bumps) the
  order absorbed in between. The second half is the regression test for the
  `revision`-indexed draw 3.3 rejects.
- `the_inverse_cdf_hits_its_boundaries` - `u` just below a cumulative boundary
  and just above it select adjacent support entries, a zero-mass bin is never
  selected, and `u` at its maximum selects the last non-zero-mass entry.

`mogwai-server` `fills.rs` unit tests (new/extended):

- `volume_accumulates_only_at_the_limit_price` - a print through the limit feeds
  `counted`, not `volume_at`.
- `the_walk_stops_once_every_scan_met_either_threshold`.
- `volume_stops_accumulating_once_the_threshold_is_met` - the mirrored guard of
  4.7.
- `a_scan_carries_the_remaining_queue_not_the_original_draw` - a cross-pass
  test: an order whose earlier passes already consumed most of its queue is
  filled by, and the walk stops on, a second pass whose OWN delta is smaller
  than the original draw. This is the defect 4.5 names; without it the walk
  silently never early-exits for a partially consumed order.
- `a_pass_costs_one_walk_per_symbol_not_per_order` and
  `sweep_pass_walks_only_the_new_span` EXTENDED to carry `queue_remaining`
  scans, so the "adds an accumulator to an existing walk, not a new walk" claim
  is pinned. Note 3.6: these pin walk count, not per-tick cost.

`mogwai-server` config tests: boot refuses `queue_ahead_enabled` with
`penetration_ticks = 0`; boot accepts the valid pair.

Python-side unit tests for the L1 estimator, in whatever form `analysis/`
settles on (the analysis test-harness decision is a separate open TODO entry, so
this spec does not pick the framework - but it does REQUIRE the tests, because
L1's gate is otherwise "the script produced a number", which cannot distinguish
the intended computation from a plausible wrong one). Each runs against a
handful of synthetic rows, not the corpus:
- a visit closes on a price change and not on a size change; consecutive equal
  prices at non-adjacent timestamps are ONE visit.
- a single-print visit increments `visit_single`; a two-print visit does not.
- a visit straddling `DWELL_ERA_START_TS` is dropped entirely; a visit opening
  exactly at the boundary is kept; the final open visit at end of input is
  closed and binned.
- `lvl_bin` is monotone, puts `LVL_LOG_LO` in bin 0 and anything at or above
  `LVL_LOG_HI` in the last bin, and puts `0.5` and `5.0` in DIFFERENT bins -
  the regression test for the `int()`-truncation defect this binning replaces.
- `vol_p50_norm` on a hand-built histogram equals the hand-computed value, and
  the normalizer used is the era-windowed `size_median`.
- `support_norm` is strictly increasing and `pmf` sums to 1.0 on a fixture.

`scripts/smoke-queue-ahead.toml` (`penetration_ticks = 1000`,
`fill_sweep_interval_ms = 50`, `queue_ahead_enabled = true`) plus
`scripts/smoke.py --queue-ahead`: rest a limit AT the live tape's current price
with a penetration threshold high enough that the count disjunct cannot fire
within the test, and assert the fill arrives anyway, at the order's own price,
over the live WS and control-plane path. That is the end-to-end proof that the
second disjunct is wired through config, engine, walk, sweeper and delivery.

That smoke is the ONLY gate in this spec that depends on tape luck, and it is
made deterministic rather than left to it. Three requirements, all on the test:

- FIX THE DRAW. The `client_order_id` is not generated; it is a constant chosen
  offline so that `fnv1a64` of it against the committed distribution lands in a
  LOW quantile - the smoke asserts the resulting `queue_ahead` is at most a
  small stated multiple of the instrument's `typical_size`, so a change to the
  fitted distribution that would make the test hang fails it loudly instead. The
  chosen id and the quantile it selects are recorded in a comment next to it.
- FIX THE TAPE. `smoke-queue-ahead.toml` pins the generator seed and the
  instrument's `typical_size` the same way the other smoke fixtures pin theirs,
  so the volume printed at a price in a given wall-clock window is not a
  different number on every run.
- BOUND THE WAIT. The smoke waits a stated number of seconds, computed from the
  fixture's tick rate, `typical_size` and the asserted draw ceiling, and FAILS
  on timeout rather than blocking. It also asserts the fill's `counted` disjunct
  did not fire, so a passing run cannot be a penetration fill in disguise.

### 4.11 Documentation

- `reference/architecture.md`, in the "Fills are synthetic" bullet: the
  disjunction, the refusal of the L2 ladder and WHY (no L2 in the lineage), the
  fitted origin of the volume threshold, the estimand named as at-touch traded
  volume rather than depth, the three honesty limits, and the independence of
  draws across orders (3.7).
- `reference/config.md`: a `queue_ahead_enabled` row alongside
  `penetration_ticks` / `fill_sweep_interval_ms`, naming the boot refusal.
- `mogwai.toml`: the knob, commented, defaulted off.
- `notes/todo.md`: section 8.

## 5. Landings

Each landing is one coherent intrusive change, kept or reverted on its gate.

### L1 - measure the corpus, and price the decision

Scope: `analysis/characterize.py`, `analysis/build_fingerprint.py`,
`analysis/fingerprint.json`, and the estimator unit tests of 4.10. The
`analysis/char_*.json` files are regenerated but are gitignored intermediates,
not landed artifacts (4.3). No Rust.

Gate, exactly:

```
python3 -m pytest analysis/
python3 analysis/run_corpus.py
python3 analysis/build_fingerprint.py
python3 analysis/check_additive.py
brokkr check
```

The estimator unit tests run FIRST and against synthetic rows, so the corpus run
is only ever asked to scale a computation already shown to be the intended one.
A corpus run alone answers "what number came out" and cannot answer "is the
instrument correct", which is the whole reason a boundary-sensitive run-closure
and histogram-quantile computation gets tests before it gets a verdict. (The
`pytest` invocation is illustrative: the analysis test-harness choice is its own
open TODO entry. Whatever it lands as, these tests run here.)

`analysis/check_additive.py` is a new script in this landing, ~30 lines: it
loads the pre-image of `analysis/fingerprint.json` from git and the regenerated
file, and exits non-zero unless the ONLY difference is added keys, with every
pre-existing key present and bit-identical in value. `git diff --stat`, which an
earlier draft used, cannot enforce this - a `--stat` line count does not
distinguish an added key from a changed one, and a reordered or reformatted
value can move the count either way. 4.3's additive requirement is a real gate
or it is nothing.

`brokkr check` must pass unchanged: the Rust side reads none of the new keys
yet, `fingerprint.rs` has no `deny_unknown_fields`, and the realism gate and
`clean_regime_is_byte_identical` must be green WITHOUT a re-bless. A re-bless
requirement here means 4.3 was violated.

The corpus lives under `MOGWAI_DATA_DIR`; L1 requires that disk mounted and
nothing else in this spec does.

#### 5.1 The proceed/close threshold

Read from the anchor (XBTUSD) `level_queue` block. PROCEED to L2 only if ALL
four hold:

1. `single_print_frac <= 0.50` - a strict MAJORITY of level visits carry more
   than one print, so "how much trades at a price before it moves" is a real
   quantity rather than a restatement of the trade-size distribution. An earlier
   draft wrote `<= 0.60` while describing it as "a majority-plus", which it is
   not: 0.60 admits a corpus where 60 percent of visits are a lone print, i.e.
   exactly the degenerate case section 1 says must flip the decision. The
   threshold now matches the sentence.
2. `vol_p90_norm / vol_p50_norm >= 3.0` - the distribution has genuine
   dispersion, so a draw from it is not a constant in disguise. Meaningful only
   because 4.1 bins at ten per decade; against the old whole-decade binning this
   ratio could take essentially two values and the test carried one bit.
3. That dispersion EXCEEDS the trade-size dispersion it could be inherited from.
   Condition 2 alone is satisfiable by a corpus of single-print visits whose
   spread is just the spread of trade sizes, which would prove nothing about
   levels. So: the visit-volume p90/p50 ratio must be at least 1.5x the p90/p50
   ratio of the era-windowed trade-size histogram (`lvl_size_hist`, already
   computed as the normalizer, so this costs nothing). Both ratios are emitted
   into `level_queue` so the reading is on the record.
4. The anchor's `single_print_frac` is within 1.5x of the CROSS-PAIR MEDIAN of
   `single_print_frac`, in both directions. An earlier draft tested "the anchor
   lies within the cross-pair `range.min .. range.max` band", which is vacuous:
   `rng()` in `build_fingerprint.py` computes min/median/max over all pairs and
   the anchor is one of them, so the anchor is inside its own band by
   construction and the condition can never fail. Comparing against the median
   is a real test of "the anchor is not the outlier the model would be fitted
   to". The median is still computed over all pairs including the anchor, which
   is fine - one pair cannot drag a median past a 1.5x tolerance unless the
   corpus is tiny, and if the corpus is that small the whole exercise fails
   condition 1 first.

If ANY fails, the queue-ahead quantity is not grounded in the corpus after all;
the decision flips to DECLINE, L2 and L3 are never laid, and the item closes on
L-DECLINE below. L1 stays landed either way: the measurement is a durable corpus
fact, and the `level_queue` block documents it whether or not anything samples
it.

Every threshold above is a reading of committed JSON, not a judgement call made
during implementation.

### L-decline - the close-out, laid ONLY if 5.1 fails

An earlier draft assigned the documentation and the TODO closure to L3 and to
section 8, then said L2 and L3 are skipped on decline - which left the decline
branch with no landing, no scope and no gate, i.e. the outcome that section 1
says is a live possibility was the one outcome with no plan. This landing is
that plan. It is mutually exclusive with L2 and L3: exactly one of "L2 then L3"
and "L-decline" is laid.

Scope: `reference/architecture.md` and `notes/todo.md` only. No Rust, no config,
no smoke, no further analysis.

- `reference/architecture.md`'s "Fills are synthetic" bullet gains a sentence
  naming RFC 4631 phase B as REFUSED IN FULL, with the measured
  `single_print_frac`, the two dispersion ratios, and which of the four
  conditions failed - so the refusal cites numbers and is not re-proposed from
  intuition.
- The `level_queue._doc` string in `analysis/fingerprint.json` gains the verdict
  in one sentence, next to the numbers it was read from. This is the only edit
  to a file L1 landed.
- `notes/todo.md` loses the phase B entry entirely and the successor entry is
  reworded per section 8's decline paragraph.

Gate, exactly:

```
brokkr check
python3 analysis/check_additive.py --allow golden_targets.level_queue._doc
```

`check_additive.py` re-runs because the `_doc` edit touches `fingerprint.json`,
and it must still show that no key the generator reads moved. The `--allow`
flag naming exactly one key path is the whole extent of the exemption; the
script takes it as a repeatable argument so the same mechanism serves any later
doc-only edit without weakening the default.

### L2 - the model in the engine

Scope: `crates/mogwai-data/src/generated/fingerprint.rs` (4.4),
`crates/mogwai-engine` (4.5, 4.6), and the engine tests of 4.10. The server does
not construct a model yet, so `EngineConfig::queue_ahead` is `None` everywhere
in production and the venue's behavior is unchanged.

Gate, exactly:

```
brokkr check
brokkr test -p mogwai-engine queue_ahead
brokkr test -p mogwai-data realism
```

The suite is green at this boundary because every existing caller passes `None`
and every existing gated test keeps its exact expectations.

### L3 - the walk, the wiring, the knob, the smoke, the docs

Scope: `crates/mogwai-server` (4.7, 4.8, 4.9), the server tests and the smoke
fixture of 4.10, and the documentation of 4.11.

Gate, exactly:

```
brokkr check --gate
brokkr test -p mogwai-server volume_accumulates_only_at_the_limit_price
brokkr test -p mogwai-server a_scan_carries_the_remaining_queue_not_the_original_draw
brokkr test -p mogwai-server a_pass_costs_one_walk_per_symbol_not_per_order
brokkr run mogwai -- serve -c scripts/smoke-queue-ahead.toml
python3 scripts/smoke.py --queue-ahead
brokkr run mogwai -- stop
brokkr run mogwai -- serve -c scripts/smoke-penetration.toml
python3 scripts/smoke.py --penetration
brokkr run mogwai -- stop
```

The two server runs use the DAEMONIZING `serve` with an explicit `stop` between
them, not `serve -f`. An earlier draft listed two `-f` invocations back to back,
which cannot work as a gate script: the first blocks the terminal so the smoke
after it never runs, and if it were backgrounded the second would collide with
the first on the listening port with no teardown in between. `serve` daemonizes
by default and `stop` ends it via the PID-file lock, which is exactly the
lifecycle this sequence needs. Each `stop` must precede the next `serve`.

(`--gate` because the landing touches the workspace the four socket-backed
adapter binaries are checked in; the adapter itself is not modified, and no wire
type changes, so no adapter-side work exists in this spec.)

The `--penetration` re-run is the keep/revert reading for "queue-ahead off is
today's venue": if it needs a single expectation edited, the default path was
perturbed and L3 reverts.

## 6. Ordering argument

L1 is additive JSON that no Rust reads - green alone. L2 adds a type and two
`OpenOrder` fields whose only production value is `None`/zero, and extends a
seam whose sole caller is updated in the same landing - green alone. L3 turns
the knob on and is the first landing whose behavior differs, behind a config
default of `false`. No landing depends on a later one, and each is separately
revertible: reverting L3 leaves an unreachable model, reverting L2 leaves unread
JSON keys, and neither is a broken state.

The decision point sits at the L1/L2 boundary rather than at the end, which is
what keeps a refuted premise from costing the engine rewrite. L-decline is the
other arm of that branch and is the same shape: documentation only, gated, and
revertible on its own.

Both arms terminate. PROCEED runs L1, L2, L3 and the close-out folded into L3
and section 8. DECLINE runs L1 then L-decline. There is no path where the item
is left open with the measurement landed and nothing said about it.

## 7. Stopping rule

The teardown stops at the walk and the resting-order record. Out of scope, and
not to be added under this item:

- Any book, ladder, depth surface, or `/quotes` content.
- Per-side queues, aggressor-filtered consumption, and cancellation modeling -
  all three refused in section 1 with reasons, not deferred.
- Queueing for MARKET or FOK orders, and for the acceptance-time IOC evaluation.
- The `through()` predicate, the fill price (still the order's), `plan_fill`,
  the divergence surface, the admission/sizing budgets, and every generator
  input. The synthetic tape is not touched by this spec at any landing.
- Benches and golden fill CDFs (the phase D TODO entry).

## 8. Closing the TODO

On landing, `notes/todo.md` loses the "DECIDE, then write up and delete this
entry: how much of RFC 4631's phase B" entry entirely. On the PROCEED outcome
that edit rides in L3; on the DECLINE outcome it rides in L-decline. The
enduring content moves as follows:

- The refusal of the L2 ladder, the disjunction, the estimand (at-touch traded
  volume, not depth), the fitted origin of the volume threshold, the three
  honesty limits, and the independence of draws across orders go to
  `reference/architecture.md` (4.11) - durable, and citable from code comments.
- The measured credibility reading (`single_print_frac`, the normalized
  quantiles, the two dispersion ratios, and the proceed/close verdict against
  5.1) goes into the `level_queue._doc` string in `analysis/fingerprint.json`
  and into the doc comment on `QueueAheadModel`, so the number that justified
  the model sits next to the model.
- On the DECLINE outcome, the same two homes carry the refusal instead, as
  L-decline specifies: the architecture bullet gains a sentence naming phase B
  as refused in full, with the measured numbers and the failed condition, so it
  is never re-proposed.
- The successor entry ("After the queue-ahead decision above lands ... RFC
  4631's phase D") loses its "if it survives its decision" hedge and is reworded
  to state the outcome, since the golden fill CDF's meaningfulness argument
  turns on it.

## 9. Review disposition

Two independent reviews of the first draft are folded into the text above. What
was NOT folded, and why, so it is not re-raised:

- "Resolve the decision as DECLINE now, before measuring." Rejected as a
  recommendation, accepted as a reframing. The reviewer's substantive point -
  that the estimand is at-touch traded volume rather than queue position - is
  folded throughout (section 1, the `_doc` string, the `QueueAheadModel`
  comment, the architecture bullet). The recommendation itself is refused
  because deciding before measuring is exactly what section 1 sets up the L1
  gate to avoid, and 5.1 now has four real conditions rather than the vacuous
  and mislabelled ones the reviewer correctly attacked. If the corpus fails
  them, the decline is a reading; if it passes them, a decline would be the free
  judgement call this spec exists to refuse.
- "The spec lives in `notes/` while conventions point at `docs/` for transient
  specs." Rejected as stated. `docs/` is empty and `todo.md` - which this spec
  is spawned from and closes - lives in `notes/`, so `notes/` is where the spec
  belongs. AGENTS.md's sentence calling `docs/` the transient TODO is what is
  stale; correcting it is a separate edit and not this item's business.
- "The two extended cost tests oversell what they pin." Accepted as true and
  folded into 3.6 as an explicit statement of what is unmeasured, but NOT fixed:
  measuring per-tick cost needs the criterion benches, which are the phase D
  entry and are excluded by section 1. The gap is documented, not closed.
