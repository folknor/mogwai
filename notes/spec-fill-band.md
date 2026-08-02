# SPEC: the seeded volatility-scaled fill band

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/problem-order-book.md` (resolved as to its central question) and the
PROBLEM STATEMENTS entry in `notes/todo.md` that owns it.

This is a full rewrite of the venue's fill decision for resting limits. It
replaces the integer `penetration_ticks` counter with a per-order trigger price
drawn from a seeded, volatility-scaled band, and makes that model the venue's
only fill model rather than an off-by-default knob.

## 1. What the model is, stated exactly

A limit order gets a TRIGGER PRICE, drawn once at submit, held for the life of
the order's current tranche and price:

    trigger = price - increment * u   (Buy)
    trigger = price + increment * u   (Sell)

where `increment` is the instrument's `price_increment`, and `u` is an integer
number of ticks drawn uniformly from `0 ..= band_ticks`. The order fills IN FULL
at its own stated `price` the moment a print on the clean tape is strictly
through `trigger`, judged by the one existing predicate
`mogwai_protocol::trades_through`. Until then it rests.

`band_ticks` is the band's half width, in ticks, computed by the SERVER at
submit from the tape the order was submitted against:

    band_ticks = clamp(floor(mult * horizon_return * last_px / increment),
                       0, fill_band_max_ticks)

`horizon_return` is the trailing realized volatility SCALED TO A NAMED HORIZON,
described in section 3. The horizon matters: a per-print RMS return carries no
time dimension, so two configurations with the same per-print move and a tenfold
difference in arrival rate would draw identical bands while their tapes travel
tenfold different distances within one sweep. The band is a claim about how far
the tape moves before flow reaches you, so the estimator is defined per unit sim
time and multiplied up to `FILL_HORIZON_NS`.

### 1.1 The band is one-sided, and why

`notes/problem-order-book.md` says "a band around the order's stated price". The
band this spec builds runs from the stated price AWAY from the market, never
through it. A symmetric band would fill a buy limit while the market is trading
ABOVE it, which is a fill better than any price the market ever offered: free
money manufactured by the venue, and a strictly worse forward test than
today's over-optimistic instant fill. The behaviour the user specified - orders
rest and are consumed by arriving flow - is a statement about orders being
HARDER to fill than a naive touch, not easier. So `u >= 0` moves the trigger
away from the market and the band's randomness expresses queue position and
adverse selection: sometimes the tape reaching your price is not enough.

`u = 0` is the front-of-queue draw and reduces exactly to today's
`penetration_ticks = 1` behaviour: one print strictly through the stated price.
A print AT the price is still the market touching rather than trading through,
and still does not fill. That preserves the single predicate and the fidelity
property the penetration gate was landed for.

### 1.2 Why a band rather than a single fixed offset

A fixed offset is a claim about queue depth the trade-only corpus cannot
support, and it makes every order at a given price behave identically. The draw
gives a population of orders a distribution of outcomes with no invented
microstructure: the SCALE is fitted (to trailing volatility), the SHAPE is
declared uniform, which is the maximum-entropy choice on a bounded support and
carries no claim beyond the scale.

### 1.3 Jumps

The trigger is a threshold, not an equality, so a tape that steps from above the
trigger to below it in one print still fills the order. Nothing is missed
because of tick granularity, which is the property `notes/problem-order-book.md`
asks the band to have.

### 1.4 Determinism

The draw comes from a stream derived from the run seed and the order's identity,
never from the generator's `ChaCha12Rng`. The tape stays a pure function of
(seed, config): no client action advances any generator state, no client order
is visible to the market, and market impact stays excluded by construction. The
key is

    band_key = mix(fill_seed, symbol, client_order_id, side, price, band_draw)

where `band_draw` is a per-order counter incremented on a price amend and on a
partial fill, and on nothing else. It deliberately does NOT include the order's
`revision`, which is bumped by sweep passes: a key that moved with sweep timing
would make the trigger a function of how often the sweeper ran.

What is reproducible, stated precisely because an earlier draft of this section
overstated it: given (seed, config, symbol, client order id, side, price,
`band_draw`), the DRAWN OFFSET `u` is a pure function - the same inputs always
produce the same offset. That is the whole determinism claim of the draw.

What is NOT reproducible, and must not be read into the above:

- `band_ticks` itself, under a wall-paced clock. It is a function of the tape
  reading taken at the submit's timestamp, and that timestamp is wall-derived,
  so a live client replaying identical actions gets a different band and hence a
  different trigger price. Only under a fixed sim clock - which is what the
  engine unit tests and the golden use - is the trigger reproducible end to end.
- The fill OUTCOME, not merely its wall instant. The sweeper walks off-lock and
  then discards a `ScanResult` whose `revision` moved while it walked, so a
  cancel or a modify that wins the lock first turns what would have been a fill
  into a cancellation. Identical client actions at identical SIM times can
  therefore end in different terminal states depending on wall scheduling. This
  spec does not redesign that ordering - it is the existing off-lock contract -
  but it stops claiming reproducibility the venue does not have.
- The `the_tape_is_identical_with_and_without_order_flow` test in landing 2 pins
  the TAPE, which is the property that actually matters (no client action
  advances any generator state). It does not pin triggers, and is not offered as
  if it did.

A consequence worth naming rather than leaving to be discovered: the key is a
pure function of client-supplied fields plus `fill_seed`, so a client that
dislikes its trigger can cancel and resubmit under a fresh `client_order_id` to
re-roll it. For a test venue whose clients are strategies, not adversaries, that
is accepted; it is recorded here so nobody later reports it as a leak.

### 1.5 What falls out and needs no work

- Self-trade within one account is IMPOSSIBLE rather than prevented. Orders
  never interact; each is judged only against the tape. Stated in a test and in
  `reference/architecture.md`, implemented by nothing.
- The queue-ahead quantity is not needed and stays refused.
- Partial fills remain havoc-only (`PartialFillNext`); the honest path fills in
  full.
- `/quotes` staying empty stops being a deviation: the traded-price predicate is
  the right one under this model.

## 2. Survey of the ground

Everything the teardown touches, and what depends on it.

- `crates/mogwai-engine/src/lib.rs`
  - `EngineConfig.penetration_ticks`, `Engine.penetration_ticks`.
  - `OpenOrder.penetration_count`, `OpenOrder.penetration_scanned_ns`,
    `OpenOrder.revision`.
  - `PendingScan { client_order_id, symbol, side, price, from_ns, remaining,
    revision }`, `ScanResult { client_order_id, from_ns, revision, counted,
    scanned_to_ns }`, `Engine::pending_scans`.
  - `process_with_market(msg, ts, Option<Decimal>)` and its `process` wrapper.
  - Unit tests naming the gate:
    `zero_penetration_ticks_fills_on_submit_exactly_as_before`,
    `a_price_amend_restarts_the_penetration_window_and_quantity_amend_preserves_it`,
    `an_executed_order_restarts_its_penetration_window`, plus every submit test
    that relies on a bare `process` filling a limit immediately.
- `crates/mogwai-engine/src/orders.rs` - the `gated`/`seeded` block in
  `on_submit`, `apply_scans`, `commit_fill`, `on_modify`'s window restart.
- `crates/mogwai-engine/examples/fill_bench.rs` - `engine(penetration_ticks)`,
  the `submit_ungated` / `submit_gated_rest` / `submit_gated_seeded` cases
  recorded in `reference/performance.md`.
- `crates/mogwai-data/src/penetration.rs` - `PenetrationScan`, `Walk`,
  `count_penetrations`, and its five unit tests.
- `crates/mogwai-data/src/lib.rs` - the `mod penetration;` declaration, the
  `pub use penetration::{PenetrationScan, Walk, count_penetrations};` re-export,
  and the crate-level doc paragraph naming `count_penetrations` as the walk
  behind "the venue's penetration gate". All three move with the rename.
- `crates/mogwai-data/examples/fill_walk_bench.rs` - the SECOND criterion bench,
  on the data side. It imports `count_penetrations` and constructs
  `PenetrationScan`s directly, so the rename breaks it; `reference/performance.md`
  records its numbers too, so landing 2 re-measures BOTH benches, not just
  `fill_bench.rs`.
- `crates/mogwai-protocol/src/sizing.rs::penetrated_fill_max_bytes` and its
  caller `crates/mogwai-server/src/admission.rs::reserve_penetrated`. The bound
  is unchanged in substance - a sweep batch still emits at most one fill plus
  one account state per executed order - but both items are named after the
  deleted concept, and `apply_scans`'s own comment cites the sizing function as
  the bound it satisfies. Landing 2 renames them to `swept_fill_max_bytes` /
  `reserve_swept` and updates that comment. This is a rename of a public item in
  `mogwai-protocol`, which section 4.1's "no change" is about the WIRE only.
- `crates/mogwai-protocol/src/messages.rs` - `trades_through` survives
  unchanged, but its doc comment is written wholly in penetration-gate terms
  ("`mogwai-engine` applies it to seed one penetration"). It is restated in
  trigger terms in landing 2.
- `crates/mogwai-server/src/fills.rs` - the composition wrapper, the
  `SWEEP_DRAIN_BUDGET`, `last_trade_at_or_before` re-export, and ten tests that
  drive the walk against the real generated tape.
- `crates/mogwai-server/src/source.rs` - `seed_for`, `last_trade_at_or_before`,
  `current_price`, `build_history_source`, the checkpoint chain.
- `crates/mogwai-server/src/sweeper.rs` - the three-phase pass, spawned only
  when `penetration_ticks > 0`.
- `crates/mogwai-server/src/http.rs` - `market_reading` and its `gated_limit`
  condition; the price-less-market stamping path.
- `crates/mogwai-server/src/config.rs` - `penetration_ticks`,
  `MAX_PENETRATION_TICKS`, `fill_sweep_interval_ms`, `SLOW_SWEEP_WARN_MS` and
  the two boot validations coupling them.
- `crates/mogwai-server/src/main.rs` - engine construction and the conditional
  sweeper spawn.
- `crates/mogwai-server/src/run.rs` - `Run::new` currently TAKES
  `penetration_ticks: u32` and threads it into `EngineConfig`; that parameter is
  removed in the same edit that adds `fill_seed`. The file also carries a doc
  comment about "a penetration fill nobody commanded", as does
  `crates/mogwai-server/src/ws.rs` on the venue-originated output path. Both are
  restated; the path they describe is exactly what this model preserves.
- `crates/mogwai-server/src/fill_golden.rs` plus
  `crates/mogwai-server/tests/golden/fill_distribution.json` - the committed
  fill-timing distribution, keyed on `penetration_ticks` 1 and 3.
- `crates/mogwai-server/tests/configs/gated.toml`,
  `tests/common/mod.rs::gated_config`, `tests/serving.rs::
  a_gated_limit_fills_from_the_run_sweep`.
- `scripts/smoke-penetration.toml`, `scripts/smoke-penetration-two.toml`,
  `scripts/smoke.py` modes `penetration` and `penetration-swept`.
- Root `mogwai.toml` (`penetration_ticks = 0`).
- Docs: `reference/architecture.md` (the founding fill assumption and the
  penetration paragraph), `reference/config.md` (the fill gate section),
  `reference/glossary.md`, `reference/performance.md`, `notes/todo.md`.

Two things the survey clears rather than finds:

- `mogwai-adapter` is NOT affected. Its four socket-backed test binaries drive a
  STUB server replaying canned frames, not a real venue, so no adapter test
  depends on when a limit fills. No wire type changes, so no serde surface
  moves, and the adapter needs no rebuild reasoning. `brokkr check --gate` is
  therefore not required by this spec (it is still what a commit touching the
  adapter runs; this one does not).
- `scripts/smoke.py`'s `default` and other modes submit MARKET orders, so only
  the two penetration modes are affected by the resting-limit change.

## 3. The volatility estimator

Trailing realized volatility over a sim-time window of recent trades, computed
on the clean tape. Chosen over ATR because ATR needs bars this venue does not
ship, and over the generator's internal GARCH sigma because `mogwai-engine` is
venue-agnostic and must not learn how the tape was made; the tape seam already
carries prints, and prints are all this needs.

Definition, pinned so two implementers compute the same number:

- Window: `VOL_WINDOW_NS = 300_000_000_000` (300 sim seconds) ending inclusively
  at the submit instant `ts`.
- Sample: every `TradeTick` with `ts_event <= ts` in that window, in tape order.
- Returns: `r_i = (p_i - p_{i-1}) / p_{i-1}` as `f64`, over consecutive prints
  in the window.
- Per-print estimator: `rms_return = sqrt(mean(r_i^2))`. Uncentered on purpose:
  the mean return over a 300 s window is noise, and estimating it costs a degree
  of freedom for nothing.
- HORIZON SCALING, the step that gives the number its units. `rms_return` is the
  RMS of ONE print's move and carries no time dimension. Under the usual
  square-root-of-time aggregation of independent increments,

      horizon_return = rms_return * sqrt(n * FILL_HORIZON_NS / span_ns)

  where `n` is the number of returns actually collected and `span_ns` is the sim
  time they span (`ts_last - ts_first`, the observed span, not the nominal
  window, so a truncated or sparse window is scaled by what it really saw).
  `n / span_ns` is the measured print arrival rate, so the product is the
  expected number of prints in one horizon and the square root converts it to a
  price move. `FILL_HORIZON_NS = 60_000_000_000` (60 sim seconds): the horizon a
  resting order is plausibly exposed to between arrival and the flow that could
  consume it, and comfortably above any `fill_sweep_interval_ms` the config
  validator permits. It is a const for the same reason `VOL_WINDOW_NS` is.
  Degenerate `span_ns == 0` (all prints at one instant) reports no reading.

  This is the fix for the estimator having previously been a per-print quantity
  used as if it were a per-horizon one. It is also what makes one `mult` value
  meaningful across instruments and cadences, which is the inherited requirement
  the unscaled form could not satisfy, and it is what makes section 5's
  tick-window threshold a comparison between two quantities in the same units.
- Minimum: `MIN_VOL_SAMPLES = 8` returns. Below that the reading is REFUSED -
  `vol_reading` returns `None`, the submit gets no `MarketReading`, and the
  order rests untriggerable until a later walk has evidence. It does NOT fall
  back to `rms_return = 0.0`. An earlier draft called the zero band "the most
  conservative available answer"; that is backwards, and the spec's own golden
  inspection property says so - the zero band fills at least as many orders as
  any banded scenario, so it is the most PERMISSIVE outcome available. Falling
  back to it means the venue answers "I have no evidence" with its easiest-fill
  regime, which is the exact defect this rewrite exists to remove. Refusing is
  the conservative answer. Only reachable within a few hundred sim seconds of
  `data_origin`, which is inside warmup, so no live order sees it either way.
- Drain bound: the same `SWEEP_DRAIN_BUDGET = 20_000` ticks. A walk that
  EXHAUSTS the budget before reaching `to_ns` returns `None`. It does not return
  what it collected. This matters more than it looks: the walk starts at a
  checkpoint up to `CHECKPOINT_K = 8_192` ticks BEFORE the window opens, pays
  that residual first, and collects forward, so an exhausted walk holds the
  OLDEST part of the window. Its `last_px` would then be a print from well
  before `ts` while `MarketReading` documents that field as the last print at or
  before the submit instant, and the submit path decides marketable-on-arrival
  against exactly that number. Stale is not a look-ahead, but it is a lie, and
  the predecessor `last_trade_at_or_before` had no budget and so could not
  produce it. `Walk.drained == budget` with `reached_ns < to_ns` is the
  detection; `None` is the answer. The engine's no-reading path (order rests,
  section 4.3 step 5) already handles it, and landing 1's probe reports how
  often it fires so the budget can be raised if it fires at all.

Positioning: the run's checkpoint chain, `source_at_or_before(ts -
VOL_WINDOW_NS)`, then a forward walk to `ts`. That is the same resume-plus-
residual cost `last_trade_at_or_before` already pays, plus the window's own
prints. It runs on `spawn_blocking`, exactly as the current reading does.

The last print of the same walk IS `last_px`, so one walk produces the whole
market reading and the separate `last_trade_at_or_before` walk disappears.

### 3.1 Arithmetic: exactness, ordering, and totality

Prices are `Decimal` and the estimator is `f64`, so the crossing points are
pinned rather than left to the implementer:

- `Decimal -> f64` happens exactly twice, both inside `vol_reading`: on each
  print price when forming `r_i`, and on `last_px` when forming `band_ticks`.
  `rust_decimal::prelude::ToPrimitive::to_f64` returning `None`, or a
  non-finite intermediate, makes the whole reading `None`.
- `band_ticks` is computed in `f64`, floored, then converted: a non-finite or
  negative product is `0`, a product above `fill_band_max_ticks` is the clamp.
  The clamp is applied BEFORE the `u32` conversion, so the conversion cannot
  wrap. Order of operations is exactly the section 1 formula: multiply, divide
  by the increment, floor, clamp.
- Every price arithmetic that produces a trigger or a fill price uses
  `checked_mul` / `checked_add` / `checked_sub`. On `None` the offset is treated
  as zero and the order gets its stated price - total, and degenerate in the
  safe direction. This covers the paths the earlier buy-trigger clamp missed:
  sell trigger and buy market fill can OVERFLOW `Decimal` at a wide band, and a
  sell market fill can slip to zero or negative. A resulting price at or below
  zero is likewise replaced by the stated price.
- `u` is drawn as `u32` and converted with `Decimal::from`, which is total.

## 4. Target artifacts

### 4.1 `mogwai-protocol`

No WIRE change. There is no new field and no serde surface moves: `band_ticks`
and the trigger are venue internals, and a client learns the model's effect by
watching fills. This is a deliberate refusal, not an oversight - see section 7.

Two non-wire items in that crate DO move, and are called out here because an
earlier draft's bare "no change" read as a claim the survey had not checked:
`sizing::penetrated_fill_max_bytes` is renamed `swept_fill_max_bytes` with its
doc restated (the bound itself is unchanged), and `trades_through` keeps its
body while its doc comment is restated in trigger terms.

### 4.2 `mogwai-data`

`src/penetration.rs` is renamed `src/trigger.rs` and rewritten:

```rust
pub struct TriggerScan {
    pub side: Side,
    /// The drawn trigger price. A print strictly through this fills the order.
    pub trigger_px: Decimal,
    /// Exclusive lower bound of the span still to walk.
    pub from_ns: u64,
}

pub struct Walk {
    /// Per scan, in the input's order: whether a print strictly through the
    /// trigger was seen in `(scan.from_ns, reached_ns]`.
    pub triggered: Vec<bool>,
    pub reached_ns: u64,
    pub drained: usize,
}

#[must_use]
pub fn scan_triggers(
    source: &mut dyn TickSource,
    scans: &[TriggerScan],
    to_ns: u64,
    budget: usize,
) -> Walk;
```

`scan_triggers` keeps every structural property of `count_penetrations`: one
walk per tape shared by every scan, per-scan `from_ns`, early return once every
scan has triggered, `reached_ns` reporting where the drain actually got to,
`drained` for the cost gates, and the total-function empty-scan branch.

New in the same module, because it is the other thing a fill decision reads off
a tape:

```rust
pub struct VolReading {
    /// Last print at or before `to_ns`. Only ever produced by a walk that
    /// actually REACHED `to_ns`.
    pub last_px: Decimal,
    /// RMS of one print's return, unitless.
    pub rms_return: f64,
    /// `rms_return` scaled to `FILL_HORIZON_NS` by the observed arrival rate.
    /// This is the number the band formula multiplies.
    pub horizon_return: f64,
    pub samples: usize,
    /// Sim-time span the samples actually covered. Reported so the probe can
    /// show the scaling's inputs rather than only its output.
    pub span_ns: u64,
}

/// One walk of `(from_ns, to_ns]` producing the trailing realized volatility
/// and the last print.
///
/// `None` in every case where the reading would be untrue rather than merely
/// imprecise: the span carries no trade, it carries fewer than
/// `MIN_VOL_SAMPLES` returns, its samples span zero sim time, the walk
/// exhausted `budget` before reaching `to_ns`, or a price failed to convert to
/// a finite `f64`. A caller never receives a partial or stale reading.
#[must_use]
pub fn vol_reading(
    source: &mut dyn TickSource,
    from_ns: u64,
    to_ns: u64,
    budget: usize,
) -> Option<VolReading>;
```

### 4.3 `mogwai-engine`

```rust
/// What the venue read off its own tape at the instant a submit arrived.
pub struct MarketReading {
    /// Last print at or before the submit instant. Never a look-ahead.
    pub last_px: Decimal,
    /// Band half width in TICKS, already scaled by trailing realized
    /// volatility and clamped by the server. The engine multiplies it by the
    /// instrument's price increment, because the instrument table lives here.
    pub band_ticks: u32,
}

pub struct EngineConfig {
    pub account_id: AccountId,
    pub instruments: Vec<InstrumentDef>,
    pub balances: HashMap<String, Decimal>,
    /// Root of the fill-band RNG stream. Never the generator's stream.
    pub fill_seed: u64,
}

pub struct PendingScan {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub trigger_px: Decimal,
    pub from_ns: u64,
    pub revision: u64,
}

pub struct ScanResult {
    pub client_order_id: ClientOrderId,
    pub from_ns: u64,
    pub revision: u64,
    pub triggered: bool,
    pub scanned_to_ns: u64,
}

impl Engine {
    pub fn process_with_market(
        &mut self,
        msg: ClientMessage,
        ts: u64,
        reading: Option<MarketReading>,
    ) -> Vec<ServerMessage>;
}
```

`OpenOrder` loses `penetration_count` and gains:

```rust
    /// Band half width this order was accepted under, in ticks. Held so a
    /// re-draw (reprice, partial-fill remainder) does not need a fresh tape
    /// reading it has no way to take.
    pub band_ticks: u32,
    /// The drawn trigger price for the CURRENT tranche and price.
    pub trigger_px: Decimal,
    /// Number of draws this order has made. Part of the RNG key, so a reprice
    /// or a partial-fill remainder draws a fresh offset rather than reusing
    /// the one the previous tranche got.
    pub band_draw: u32,
```

and `penetration_scanned_ns` is renamed `scanned_ns` (same meaning: the
exclusive lower bound the next walk resumes from, advanced only by the engine).

The draw, private to `orders.rs`:

```rust
fn draw_trigger(
    fill_seed: u64,
    order: &SubmitOrder,
    price: Decimal,
    increment: Decimal,
    band_ticks: u32,
    band_draw: u32,
) -> Decimal
```

- Key: FNV-1a over `fill_seed.to_le_bytes()`, the symbol bytes, `0x00`, the
  client order id bytes, `0x00`, one side byte, the price's
  `Decimal::serialize()` bytes, and `band_draw.to_le_bytes()`.
- Stream: `rand_chacha::ChaCha8Rng::seed_from_u64(key)`, one
  `random_range(0..=band_ticks)`. `rand_chacha` and `rand` are already workspace
  dependencies; `mogwai-engine` adds both.
- Result: `price -/+ increment * Decimal::from(u)`, by side. A buy whose
  trigger would go non-positive clamps the offset down so the trigger stays
  above zero; unreachable at any sane band but the arithmetic is total.

Submit path in `on_submit`, replacing the `gated`/`seeded` block:

1. Validate as today (unchanged, including the funds and grid checks).
2. `RejectNextSubmit` as today.
3. For a LIMIT of any TIF, draw the trigger from the reading's `band_ticks`
   (`0` when there is no reading) and store it.
4. Marketable on arrival iff a reading exists AND
   `trades_through(side, trigger_px, reading.last_px)`. If so, fall through to
   the existing `plan_fill` / FOK / `commit_fill` path unchanged.
5. Otherwise: GTC rests carrying `trigger_px`, `band_ticks`, `band_draw = 0`,
   `scanned_ns = ts`; IOC emits `OrderCanceled`; FOK is REJECTED with
   `"fill-or-kill could not fill at its trigger"`. FOK stops being a free fill:
   it is decided now or never, and now means against the trigger like everything
   else.
6. MARKET orders never draw and never rest; see landing 3 for their pricing.

Two things about that step ordering, both of which an earlier draft left
implicit:

**The FOK rejection must NOT move ahead of `plan_fill`.** Today `on_submit`
deliberately runs `plan_fill` BEFORE deciding a FOK, so that a targeted
`PartialFillNext` which is the very reason the FOK cannot fully fill is CONSUMED
by that FOK rather than left armed to ambush a resubmit of the same
`client_order_id`. A long comment in `orders.rs` records that as load-bearing,
and `plan_fill` needs no venue id precisely so the ordering is possible. A
short-of-trigger FOK rejected at step 5 would sail past `plan_fill` and leave
the arm standing, silently changing a divergence contract this spec does not own.
So step 5's FOK branch draws the arm first - it calls `plan_fill` for its
consuming effect, discards the plan, and then rejects. Pinned by a named test:
`a_fok_rejected_at_its_trigger_still_consumes_a_targeted_partial_fill_arm`.

**Why an AGGRESSOR gets the same band, stated because the queue-position
justification does not transfer.** Section 1.2 motivates the band as queue
position and adverse selection, and an aggressive marketable order is exactly
what does not wait in a queue. The band still applies to IOC and FOK, on a
different and narrower ground: what the venue lacks is not queue depth but any
knowledge of the price a marketable order would actually get, and filling it at
its own stated price is the same lie for an IOC as for a market order. The band
is the venue's one available estimate of "further away than you asked for", so
it is applied uniformly rather than switched off for the order types where the
motivating story is weaker. The consequence is real and is accepted: at a median
band of several ticks, an IOC or FOK priced AT the touch now usually cancels or
rejects instead of filling. Consumers that want a fill from an aggressor price
through the band, which is what an aggressor does on a real venue anyway. This
is a behaviour change large enough that `reference/architecture.md` states it in
the fill model section, and it is pinned by
`an_ioc_short_of_its_trigger_cancels_and_a_fok_short_of_its_trigger_is_rejected`.

`pending_scans` yields one `PendingScan` per resting GTC LIMIT, carrying
`trigger_px` and `scanned_ns`, sorted by `from_ns` as today. The `order_type`
and TIF filters stay for the same load-bearing reason recorded there.

`apply_scans` matches on `(client_order_id, revision, from_ns)` as today,
advances `scanned_ns` to `result.scanned_to_ns`, bumps `revision`, and executes
when `result.triggered`. On a remainder (only reachable under an armed
`PartialFillNext`) it bumps `band_draw`, re-draws `trigger_px` around the
unchanged price with the stored `band_ticks`, resets `scanned_ns = ts` and bumps
`revision` again. The "each tranche must be traded through on its own" property
is preserved and strengthened: the new tranche gets a fresh queue position.

A marketable-on-arrival LIMIT that partially fills (again only under an armed
`PartialFillNext`) takes the same path as any other partial: the remainder bumps
`band_draw`, re-draws around the unchanged price with the stored `band_ticks`,
sets `scanned_ns = ts`, and rests if the TIF is GTC. IOC cancels the remainder
and FOK cannot reach this state at all. Stated explicitly because the global
invariant is "every partial increments `band_draw`" and the sweep path is not
the only place a partial happens.

A MARKET order that partially fills leaves a GTC remainder resting under today's
code, and `pending_scans` filters it out by `order_type == Limit` for a reason
recorded there: a market remainder has no meaningful trigger to walk against.
That stays true and unchanged - the remainder rests, is never scanned, and is
terminated only by a client cancel. Section 4.3's "MARKET orders never draw and
never rest" is therefore about the HONEST path; the havoc path can still leave
one resting, and this spec does not change what happens to it. Landing 3's
slippage applies to the tranche that fills, not to the remainder.

`on_modify` on a PRICE amend bumps `band_draw`, re-draws around the new price,
resets `scanned_ns` and bumps `revision`. A quantity-only amend touches none of
it, exactly as today.

The amend re-draw uses a FRESH `band_ticks`, not the stored one. The stored
value exists for `apply_scans`, which runs inside the sweeper with no way to
take a tape reading; `on_modify` arrives over the same HTTP/WS path that section
4.4 already teaches to take a reading on every limit submit, so a fresh reading
is available and cheap by comparison to the amend itself. An order repriced
hours after acceptance otherwise keeps a band fitted to a regime that is gone.
`process_with_market` therefore accepts a reading for a modify as well as a
submit, and the engine uses `reading.band_ticks` when one is present, falling
back to the stored value when it is not. `OpenOrder.band_ticks` is updated to
whatever the re-draw used, so a subsequent partial re-draw inherits the current
regime rather than the acceptance one. Pinned by
`a_price_amend_adopts_a_fresh_band_when_the_server_supplies_one`.

### 4.4 `mogwai-server`

The two functions in this crate that produce a reading get DISTINCT names. An
earlier draft called both `market_reading`, which for a document whose bar is
"two implementers produce the same artifact" is not acceptable. The blocking
tape walk in `fills.rs` is `read_market`; the async wrapper in `http.rs` keeps
`market_reading`. Every reference below uses the qualified name.

- `src/fills.rs`: `count_penetrations` becomes `scan_triggers`, mapping
  `PendingScan -> mogwai_data::TriggerScan`. `last_trade_at_or_before` is
  replaced by:

```rust
pub(crate) fn read_market(
    symbol: &str,
    ts: u64,
    profiles: &InstrumentProfiles,
    data_origin: u64,
    mult: f64,
    max_ticks: u32,
) -> Option<mogwai_engine::MarketReading>;
```

  which positions the chain at `ts - VOL_WINDOW_NS`, calls
  `mogwai_data::vol_reading`, and converts to `band_ticks` with the formula in
  section 1, using the profile's `price_increment`.
- `src/source.rs`: `last_trade_at_or_before` is deleted (its only callers move
  to `fills::read_market`); `seed_for` stays and additionally sources the run's
  `fill_seed`.
- `src/run.rs`: `Run::new` LOSES its `penetration_ticks: u32` parameter (it
  currently takes one and threads it into `EngineConfig`) and `Run` gains
  `fill_seed: u64`, set at construction to `source::seed_for(&symbol)`. ONE
  line. Its comment states the full rationale in place - that the fill band's
  RNG root is derived from the tape seed so a run is reproducible from its
  symbol alone, and that deriving it here rather than in the engine is what
  keeps the engine venue-agnostic. It does NOT cite
  `notes/problem-seeds-and-paths.md`: durable source may not depend on a
  transient note, and a comment outlives the note it would point at. The note
  still owns replacing that derivation with a reported per-launch random seed,
  and will change that line and nothing else here.
- `src/http.rs`: `market_reading` (the async wrapper) takes a reading for EVERY
  limit submit regardless of TIF, since every limit is now banded, and for every
  price AMEND, and returns `Option<MarketReading>`. The price-less market
  stamping path is unchanged in landing 2 and is rewritten in landing 3.
- `src/sweeper.rs`: spawned unconditionally. The `interval_ms` guard, the
  `MIN_SWEEP_WALL` floor, the three phases, the off-lock walk and the delivery
  path are unchanged. A pass with nothing resting is still one lock acquisition
  and a `continue`.
- `src/config.rs`:
  - REMOVED: `penetration_ticks`, `MAX_PENETRATION_TICKS`, and the boot
    validation coupling them to the sweep interval.
  - ADDED: `fill_band_vol_mult: f64` (default from landing 1) and
    `fill_band_max_ticks: u32` (default 200). Validation: `mult` finite and in
    `[0.0, 1_000.0]`; `max_ticks` in `[1, 10_000]`. The default was 5_000 in an
    earlier draft, which is fifty times the 100-tick ceiling landing 1 uses to
    CLOSE the spec for being mispriced - a clamp above the stated ceiling of
    usefulness is not a guard, it is a no-op. 200 sits just above that ceiling,
    so it truncates only readings the calibration would already have rejected
    while leaving room for a genuine volatility spike to widen the band beyond
    its median. The upper validation bound stays generous because an operator
    deliberately exploring a pathological band is a legitimate experiment; the
    DEFAULT is what must be defensible.
  - CHANGED: `fill_sweep_interval_ms` must be `> 0` unconditionally, since the
    sweep is no longer optional. `SLOW_SWEEP_WARN_MS` keeps its warning.
  - `VOL_WINDOW_NS`, `FILL_HORIZON_NS` and `MIN_VOL_SAMPLES` are consts, not
    knobs. They set the
    estimator's identity, not an operator's policy, and a venue whose fill model
    changes shape per deployment is not a venue anyone can compare runs across.

## 5. Landings

Three, in order, each keeping the suite green at its boundary.

### Landing 1 - the instrument, and the number it prices

Nothing about the fill model changes. This landing measures.

Lands: `mogwai_data::vol_reading`, `VolReading`, and `fills::read_market`;
plus `mogwai-server::fills::vol_probe`, an `#[ignore]`d test that prints a
calibration table.

The probe is specified exactly, because a calibration whose procedure is not
written down is not a measurement anyone can repeat:

- Config: the committed root `mogwai.toml` profile set, symbol `BTCUSDT`, and
  the default `data_origin` that config carries. No accelerated clock.
- Observation grid: readings are taken at `data_origin + WARMUP_NS + k *
  PROBE_STRIDE_NS` for `k` in `0..PROBE_SAMPLES`, with `WARMUP_NS =
  3_600_000_000_000` (one sim hour, clear of both `VOL_WINDOW_NS` and the
  generator's own warmup), `PROBE_STRIDE_NS = 60_000_000_000` and
  `PROBE_SAMPLES = 1_440` - one sim day of readings, one per sim minute. All
  three are consts in the probe.
- Per reading it records `rms_return`, `samples`, `span_ns`, `horizon_return`,
  and whether the reading was `None` and why.
- Quantiles: the nearest-rank definition on the sorted vector of non-`None`
  readings - `p(q)` is element `ceil(q * m) - 1` for `m` readings, zero-indexed,
  no interpolation. Stated because "median" and "p90" have three common
  definitions that disagree at these sample counts.
- The table has one row per multiplier in `[0.5, 1, 2, 4, 8, 16, 32]` and
  columns: median and p90 `horizon_return`, the implied `band_ticks` at each
  (computed through the real `read_market` conversion, not a reimplementation),
  and the implied band in basis points of `last_px`.
- It also prints, once, the refusal counts: how many of the `PROBE_SAMPLES`
  readings returned `None`, split by cause. A budget-exhaustion count above zero
  is a finding in itself and raises `SWEEP_DRAIN_BUDGET` before the table is
  read.

Landing 1 also lands the estimator's CORRECTNESS tests, not only its
calibration. A probe that prints numbers pins nothing; these are named unit
tests in `mogwai-data`:

- `a_window_excludes_prints_outside_its_bounds` - exclusive lower, inclusive
  upper, checked on the exact boundary instants.
- `a_reading_refuses_below_the_minimum_sample_count`
- `a_reading_refuses_when_the_walk_exhausts_its_budget`
- `a_reading_refuses_a_zero_span_window`
- `the_last_price_is_the_last_print_at_or_before_the_upper_bound`
- `horizon_scaling_is_the_square_root_of_the_observed_arrival_rate` - a
  synthetic tape with a known constant per-print move and a known cadence,
  asserted against the closed form.
- `doubling_the_arrival_rate_at_a_fixed_per_print_move_scales_the_band_by_sqrt_two`
  - the property the whole horizon scaling exists for, stated directly.

These also answer the dead-code concern: `vol_reading` has no production caller
until landing 2, but it has unit tests and `read_market` behind it, so `brokkr
check` sees no unused item. Should any item still warn, it is made
`pub(crate)`-visible to the tests rather than silenced with an attribute.

Gates:

    brokkr check
    brokkr test -p mogwai-data horizon_scaling_is_the_square_root_of_the_observed_arrival_rate
    brokkr test -p mogwai-data a_reading_refuses_when_the_walk_exhausts_its_budget
    brokkr test -p mogwai-server vol_probe --debug

PROCEED/CLOSE THRESHOLD, read off that table: the chosen multiplier is the
smallest one whose MEDIAN implied band is at least 3 ticks and at most 100 ticks
on the default BTCUSDT profile. Below 3 the band is indistinguishable from the
degenerate `u = 0` case and the whole rewrite buys nothing.

The 100-tick ceiling needs its provenance corrected. An earlier draft justified
it as "the offsets the fitted tape actually travels within a sweep horizon (see
the golden's `OFFSETS`, 1 to 100 ticks)". That is wrong: `OFFSETS` in
`fill_golden.rs` is a hardcoded array of TEST ORDER INPUTS, chosen to spread the
golden's cohorts across an interesting range. It is not a measurement of tape
travel and cannot license a threshold. The ceiling stands on its own footing
instead, and the probe supplies the evidence: 100 ticks is where the p90 band on
the default profile approaches the price move the tape makes over a full
`VOL_WINDOW_NS`, at which point a resting order's fill is decided by the band
draw rather than by the tape and every cohort becomes uncensorable noise. The
probe's basis-point column is what makes that check possible - it is read
against the observed `horizon_return` p90, and the threshold is confirmed or
adjusted IN landing 1 with the reading recorded next to it.

If NO multiplier in the sweep lands in that window, this spec closes as
mispriced: the estimator is not producing a usable scale and the runner-up
(GARCH sigma on the tape seam, recorded in `notes/problem-order-book.md`
decision 1) is what a successor spec builds. The chosen value is written into
`reference/config.md` together with the reading it came from.

### Landing 2 - the model swap

Everything in section 4 except market-order slippage. One coherent, intrusive
change: the counter is deleted, not deprecated, and there is no switch that
restores it. `fill_band_vol_mult = 0.0` remains a legal configuration and gives
the strict-through-at-the-stated-price venue, which is the degenerate case of
the model rather than a compatibility mode.

Tests this landing must add or rewrite, named, in `mogwai-engine`:

- `a_limit_rests_until_the_tape_reaches_its_drawn_trigger`
- `a_zero_band_reduces_to_a_strict_through_trigger_at_the_stated_price`
- `the_trigger_is_a_pure_function_of_seed_and_order_identity` - two engines
  built with the same `fill_seed`, fed the same submits in different
  interleavings with unrelated orders, draw identical triggers.

The next three tests are stated carefully, because the obvious formulation
asserts a property randomness does not provide. A different seed, a repriced
order and a fresh tranche can all legitimately redraw the SAME `u` - with a band
of a few ticks that happens a substantial fraction of the time - so a test
asserting the trigger price CHANGED is flaky by construction, and papering over
it by picking a lucky fixture is worse. Independence is tested two ways instead:
by fixed vectors (the draw is a pure function, so a committed
`(key inputs) -> u` table pins it exactly and any change to the key or the
stream breaks it), and by DRAW IDENTITY (`OpenOrder.band_draw` and the key bytes
are observable to the test, so "a fresh draw happened" is asserted directly
rather than inferred from an unequal price). Where a distributional claim is
what is meant, it is made over many draws with a stated tolerance, not over one.

- `a_different_fill_seed_produces_a_different_draw_distribution` - the same
  order id drawn under two seeds over the full band support; the two sequences
  of `u` differ in at least one position, asserted over a fixture of 64 order
  ids rather than one. A committed vector for one `(seed, id)` pair pins the
  exact value alongside it.
- `a_price_amend_redraws_the_trigger_and_a_quantity_amend_does_not` (replaces
  `a_price_amend_restarts_the_penetration_window_and_quantity_amend_preserves_it`)
  - asserted on `band_draw` incrementing and `scanned_ns` resetting for the
  price amend and on both being untouched for the quantity amend. The trigger
  PRICE is asserted only where the amended price makes it necessarily different.
- `a_price_amend_adopts_a_fresh_band_when_the_server_supplies_one` - amend with
  a reading whose `band_ticks` differs from the stored one; `OpenOrder.band_ticks`
  follows the reading, and follows the stored value when no reading is supplied.
- `a_partial_fill_remainder_draws_a_fresh_trigger` (replaces
  `an_executed_order_restarts_its_penetration_window`) - again asserted on
  `band_draw` and `scanned_ns`, not on trigger inequality.
- `a_marketable_on_arrival_partial_remainder_also_draws_a_fresh_trigger` - the
  non-sweep partial path, which the invariant covers and no other test reaches.
- `a_fok_rejected_at_its_trigger_still_consumes_a_targeted_partial_fill_arm` -
  the ordering guard of section 4.3: after the rejection, a resubmit under the
  same `client_order_id` is NOT ambushed by the arm.
- `a_market_remainder_left_resting_by_havoc_is_never_scanned` - pins that the
  `order_type == Limit` filter in `pending_scans` survives the rewrite.
- `a_marketable_on_arrival_limit_fills_only_when_the_reading_is_through_its_trigger`
- `an_ioc_short_of_its_trigger_cancels_and_a_fok_short_of_its_trigger_is_rejected`
- `orders_of_one_account_never_interact` - two crossing limits from the same
  account, no reading; both rest, nothing matches.
- `a_submit_with_no_reading_rests_rather_than_filling` (replaces
  `zero_penetration_ticks_fills_on_submit_exactly_as_before`, whose premise is
  the defect being removed)

In `mogwai-data`: `a_walk_reports_every_scan_that_triggered`,
`a_print_that_jumps_past_a_trigger_still_triggers`,
`a_walk_stops_once_every_scan_has_triggered`, plus the four surviving structural
tests ported (budget truncation, exact boundary, batching equivalence, empty
scan list).

In `mogwai-server`: `tests/configs/band.toml` replaces `gated.toml`,
`tests/common/mod.rs::band_config` replaces `gated_config`, and
`serving.rs::a_gated_limit_fills_from_the_run_sweep` becomes
`a_banded_limit_fills_from_the_run_sweep`. New in `serving.rs`:
`the_tape_is_identical_with_and_without_order_flow` - two runs on one config,
one submitting a hundred limits and one submitting none, produce byte-identical
`/trades` pages over the same window. That is the determinism claim of section
1.4, and nothing else in the suite pins it.

The golden moves: `fill_golden.rs` keys its cells on `band_vol_mult` instead of
`penetration_ticks`, with the scenario set `[0.0, <chosen mult>]`, `schema`
bumped to 2, and `assert_shape` updated for the new key.

Where the harness gets its `band_ticks` from, which an earlier draft left
unsaid: the golden constructs `PendingScan`s itself and calls into `fills`
directly, never through the HTTP submit path, so nothing hands it a
`MarketReading`. It takes one explicitly - a single `fills::read_market` call at
the scenario's ANCHOR INSTANT (the same instant the cohort's orders are priced
from), with the scenario's `band_vol_mult` and the default `max_ticks`, reused
for every order in that scenario. That keeps one tape walk per scenario, keeps
the band constant across a cohort so the offsets remain comparable, and is
recorded in the golden's header block along with the anchor instant so a later
reader can tell what band the cells were produced under.

Coverage RETIRED, stated rather than silently dropped: the existing
`penetration_ticks = 3` scenario exists to cover ACCUMULATION of penetrations
across sweep boundaries - several separate walks each contributing to one
counter. The new model has no counter and no accumulation; one print through the
trigger fills. That behaviour is deleted, so its coverage is retired rather than
migrated, and the second scenario slot is reused for the banded multiplier.

RE-BLESS EXPECTATION:
this landing MUST move `crates/mogwai-server/tests/golden/fill_distribution.json`
in full. Procedure is the committed one and has no switch - delete the file, run
the test (it writes and then fails by design), inspect the diff, run again. The
diff is inspected against two properties before it is committed - both restated
here, because as an earlier draft phrased them the first was not a valid test of
a correct implementation:

1. **Censoring rises with `offset_ticks`, checked on PAIRED cohorts.** Strict
   monotonicity across the cohorts as they stand is not implied by a correct
   model: different offsets are accepted at different tape instants under
   different order identities, so finite-sample noise can invert two adjacent
   offsets even when the model is exactly right. The harness is changed to pair
   them - every offset in `OFFSETS` is submitted at the SAME set of acceptance
   instants with the same identity stem, so the cohorts differ only in offset -
   and monotonicity is asserted on those paired series with a stated tolerance:
   an inversion of more than `MONOTONE_TOL = 2` filled orders between adjacent
   offsets fails the inspection, a smaller one does not. If the pairing change
   does not land, this property is dropped rather than asserted loosely; a
   revert condition that fires on noise is worse than no revert condition.
2. **The `0.0` scenario fills at least as many orders at every offset as the
   banded scenario.** This one IS implied pathwise - `u >= 0` moves the trigger
   away from the market, so a banded order fills only on a subset of the tapes
   that fill an unbanded one at the same price - and it holds per order, not
   merely in aggregate, so it needs no tolerance.

`fill_bench.rs` cases are renamed `submit_immediate`, `submit_banded_rest`,
`submit_banded_marketable` and re-measured. The `mogwai-data`-side
`examples/fill_walk_bench.rs` is ported to `scan_triggers` / `TriggerScan` and
re-measured too; `reference/performance.md` records BOTH benches' new numbers
alongside the old, per its convention.

**The submit path gets its own instrument, because nothing existing can see the
cost this landing adds.** Today `http::market_reading` walks the tape only when
`penetration_ticks > 0` and the TIF is GTC or IOC, and the walk is a single
`last_trade_at_or_before`. After this landing EVERY limit submit and every price
amend unconditionally pays a checkpoint restore plus up to `SWEEP_DRAIN_BUDGET`
ticks on `spawn_blocking`. `fill_bench.rs` lives in `mogwai-engine` and takes a
`MarketReading` as an ARGUMENT, so it is structurally blind to this. Landing 2
therefore adds `crates/mogwai-server/examples/submit_latency_bench.rs`: a
criterion bench over `fills::read_market` at a warm-clock instant on the default
profile, reporting median and p99, plus a second case measuring an end-to-end
`/orders` limit submit against a running run. Its numbers go into
`reference/performance.md` next to the fill benches.

KEEP/REVERT THRESHOLD on that instrument: median `read_market` at or below 5 ms
and p99 at or below 25 ms. Above that the reading is re-scoped before the model
ships - the first lever is a shorter `VOL_WINDOW_NS`, the second is caching one
reading per symbol per sweep interval and serving submits from it, which is
sound because the band is a coarse scale and not a per-microsecond quantity. The
threshold is stated now so the landing cannot quietly ship a submit path an
order of magnitude slower than the one it replaced.

Gates:

    brokkr fmt
    brokkr check
    brokkr test -p mogwai-engine a_limit_rests_until_the_tape_reaches_its_drawn_trigger
    brokkr test -p mogwai-engine the_trigger_is_a_pure_function_of_seed_and_order_identity
    brokkr test -p mogwai-engine a_partial_fill_remainder_draws_a_fresh_trigger
    brokkr test -p mogwai-server fill_distribution_matches_the_golden
    brokkr test -p mogwai-server a_banded_limit_fills_from_the_run_sweep
    brokkr test -p mogwai-server the_tape_is_identical_with_and_without_order_flow
    python3 scripts/smoke.py band
    python3 scripts/smoke.py band-swept

`scripts/smoke-penetration.toml` becomes `scripts/smoke-band.toml` (a band wide
enough that the anchor-priced buy rests: `fill_band_vol_mult` at the chosen
value, `fill_sweep_interval_ms = 50`), and `smoke-penetration-two.toml` becomes
`smoke-band-swept.toml`, keeping its accelerated clock and the comment
explaining why a wall-bounded assertion needs one. The two `smoke.py` modes are
renamed `band` and `band-swept` with their assertions restated against the
trigger rather than the counter.

`band-swept` MUST NOT keep its 1.5x-through buy. That order is priced far
through any trigger the clamp permits, so under submit step 4 it is marketable
on arrival and fills IMMEDIATELY, on the submit path, without the sweeper ever
running. The mode's whole claim is that a venue-originated sweep pass delivers
an unsolicited fill, and as inherited it would prove the opposite of what it
asserts while still passing. Under the old counter model the same order rested
because `penetration_ticks = 2` required two separate penetrations regardless of
how deep the price was; the band has no such counter, so the construction does
not survive the model change.

The mode is rebuilt around an order that is NOT through its trigger at
acceptance and is crossed by a later print:

- Submit a GTC buy priced AT the last print (`/trades` head at submit time), so
  any `u >= 0` puts the trigger strictly below the market and the order cannot
  be marketable on arrival. `u = 0` is the boundary case and still requires a
  print strictly THROUGH the price, which the submit instant by definition does
  not have.
- Assert `OrderAccepted` with no `OrderFilled` in the same response, which is
  the "did not fill on the submit path" half the old construction could not
  make.
- Then wait, on the accelerated clock, for the tape to print through and the
  sweeper to deliver `OrderFilled` unsolicited on the WS stream, with the same
  wall bound and the same comment explaining why the accelerated clock is what
  makes that bound honest.
- A downward-drifting tape is what makes this fill, so the wall bound is
  generous and the mode retries the submit at a fresh anchor price up to three
  times before failing, rather than asserting a single draw of the generator
  must cooperate.

Documents this landing makes stale, listed so the reconciliation pass can find
them. Reconciling them is NOT the implementer's job and must not be attempted
here: document reconciliation and the removal of the originating item are owned
by the orchestrator and happen after the implementation is reviewed. The
implementer changes code and its inline documentation only.

`reference/architecture.md` (the fill model section, the founding assumption
paragraph, and the self-trade impossibility), `reference/config.md` (the fill
gate section becomes the fill band, with the chosen multiplier and its
provenance), `reference/glossary.md` (penetration -> trigger, band),
`reference/performance.md`, `mogwai.toml`, and `notes/todo.md`. The surviving
content of the originating problem statement is either in this spec or belongs
in `reference/architecture.md`.

`notes/problem-refused-order-types.md` is NOT edited by this landing, but the
obstacle it names is gone: a triggered stop now has a defensible fill, namely
the same band applied to the market order the trigger produces.

### Landing 3 - market-order slippage

Today a MARKET order fills at whatever price the client stated, or at the
venue's `current_price` when the client stated none. That is a second instance
of the same defect: the venue is asked what price the trade happened at and
answers with the client's own number.

After this landing, a MARKET submit fills at

    fill_px = last_px + increment * u   (Buy)
    fill_px = last_px - increment * u   (Sell)

with `u` drawn from the same band and the same key (`band_draw = 0`), and
`last_px` from the same `MarketReading`. Slippage is adverse by construction, in
the same direction the band is adverse for limits. The client's stated price on
a market order is ignored for PRICING; it is still validated (positive, on
grid), because the wire contract still requires a price and dropping that
requirement is a protocol change this spec declines to make.

Funds: the fill price is known at submit, before the ledger moves, so the funded
account's balance check runs against `fill_px * quantity` rather than the stated
price. That closes the hole where a slipped buy could overdraw an account the
validator had cleared.

With no reading available, a market order falls back to today's behaviour (the
stated or stamped price, no slippage) and logs at WARN.

**The server plumbing is the substance of this landing, not the engine change.**
As landing 2 leaves it, `http::market_reading` obtains a reading only for
LIMITS, and the price-less market path stamps `source::current_price` and
returns early with no reading at all. An engine that slips perfectly would
therefore never receive a reading in production, and every gate an earlier draft
listed would still pass: the engine unit tests inject a `MarketReading`
directly, the golden places no market orders, and `smoke.py default` never
inspects a fill PRICE. That is a landing that can be declared done while
shipping nothing. So landing 3 explicitly includes:

- `http::market_reading` takes a reading for MARKET submits too, on both the
  priced and the price-less paths. The `current_price` stamping call is deleted
  outright rather than kept as a parallel path - `read_market` returns the last
  print at or before `ts`, which is the number `current_price` was approximating,
  and keeping two sources of "what is the market" is how they drift apart.
- The WARN-and-no-slippage fallback is exercised deliberately by a test, not
  left as the accidental production path.

Tests: `a_market_buy_slips_up_and_a_market_sell_slips_down`,
`market_slippage_is_drawn_from_the_same_seeded_stream`,
`a_funded_account_is_checked_against_the_slipped_price`,
`a_market_order_with_no_reading_fills_at_its_stated_price_and_warns`, and in
`mogwai-server`, `a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths`.
`smoke.py default` gains a LIVE assertion that closes the hole: it submits a
market buy, reads the `OrderFilled` price, reads the `/trades` head at the fill
instant, and asserts the fill price is greater than or equal to the last print -
adverse or equal, never better. The sell direction is asserted the same way in
the opposite sense. Equality is admitted because `u = 0` is a legitimate draw.

**The slippage MAGNITUDE is inherited, and the measurement that would price it
independently is still owed.** `notes/problem-refused-order-types.md` records
that price SPAN per inferred match event has never been computed, so how far a
marketable order really walks is unquantified. This spec does not compute it.
What it does instead is refuse to invent a second scale: market slippage uses
the SAME band, the same `mult` and the same key as the limit trigger, so it
inherits landing 1's calibration and adds no unmeasured number. That is a weaker
claim than a fitted slippage model and is stated as such - the mechanism is
defensible, the magnitude is borrowed. When the span probe is run, it prices a
separate market multiplier and that is a successor change to one config field,
not to any type here.

Gates:

    brokkr fmt
    brokkr check
    brokkr test -p mogwai-engine a_market_buy_slips_up_and_a_market_sell_slips_down
    brokkr test -p mogwai-engine a_funded_account_is_checked_against_the_slipped_price
    brokkr test -p mogwai-engine a_market_order_with_no_reading_fills_at_its_stated_price_and_warns
    brokkr test -p mogwai-server a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths
    brokkr test -p mogwai-server fill_distribution_matches_the_golden
    python3 scripts/smoke.py default

The golden must NOT move in this landing (it places no market orders); if it
does, something in the limit path changed and the landing is wrong.

## 6. Keep/revert

Each landing is a single intrusive change kept or reverted whole on its gates.

- Landing 1 reverts if the calibration table lands no multiplier in the
  3-to-100-tick window, and this spec closes with it. It also reverts if any of
  the named estimator unit tests cannot be made to pass - a scale nobody can pin
  is not a scale.
- Landing 2 reverts if any named gate is red; if the re-blessed golden violates
  the banded-never-easier-than-unbanded property, or violates paired
  monotonicity beyond `MONOTONE_TOL`; or if `submit_latency_bench` exceeds the
  5 ms median / 25 ms p99 threshold and neither re-scoping lever brings it back.
  A partial revert is not available and is not wanted: half a fill model is a
  venue that fills some orders by counter and some by trigger.
- Landing 3 reverts independently. Landing 2 stands without it; the reverse is
  not true. It reverts in particular if the live `smoke.py default` price
  assertion cannot be made to hold, since that assertion is the only gate that
  observes slippage end to end.

## 7. Stopping rule and exclusions

The teardown stops at the fill DECISION. Named and excluded:

- **Order types.** Stops, stop-limits, trailing stops and every other
  conditional type stay refused. `notes/problem-refused-order-types.md` owns
  them, and this spec's only obligation to it is discharged: the fill a
  triggered conditional would get is now defined.
- **Fees and liquidity side.** `OrderFilled.commission` stays
  `Decimal::ZERO` and the adapter keeps hardcoding `LiquiditySide::Taker`.
  Maker/taker is a FEE input, fees belong to `notes/problem-instrument-model.md`
  under the parameterization ruling, and labelling a marketable-on-arrival limit
  correctly needs a decision (a wire field versus an order-type approximation)
  that the fee spec should make, not this one.
- **The run seed.** `fill_seed` is derived from today's symbol-derived tape seed
  through one named line. `notes/problem-seeds-and-paths.md` owns replacing it
  with a reported per-launch random seed and will change that line and nothing
  else here.
- **The instrument model, cadence, and profiles.** The band scales off whatever
  the tape does; none of those documents' decisions change any type in this
  spec.
- **Market impact.** Permanently excluded, and this model is what keeps it
  excluded: a client order that moved the tape would make the tape a function of
  client behaviour.
- **Partial-fill realism.** Partials stay havoc-only. The band produces full
  fills, which is the model the user specified.
- **Queue-ahead.** Refused on 2026-08-02 and not reopened; the band stands in
  for it without claiming to measure it.
- **The wire protocol.** No new field, no new message, no serde change. The
  band is a venue internal. Two internal items in `mogwai-protocol` are renamed
  (section 4.1); that is not a wire change.

## 8. Review disposition

Two independent reviews of the draft above (`notes/spec-fill-band-review-1.md`,
`notes/spec-fill-band-review-2.md`) are folded in. Every finding either changed
the text or is answered here. The two reports agreed on the survey gaps, on the
truncated-walk staleness, on the missing submit-path cost gate, and on the
determinism claim being overstated; those were folded once, not twice.

Folded, by where they landed: the estimator's missing time dimension and the
backwards zero-band fallback (section 3); the truncated-walk `None` contract and
the arithmetic totality rules (sections 3 and 3.1); the six survey gaps -
`fill_walk_bench.rs`, `mogwai-data/src/lib.rs`, `sizing.rs` with its server
caller, the `Run::new` parameter, the `trades_through` doc, the two penetration
comments (section 2, section 4.1); the `market_reading` name collision, the
`fill_band_max_ticks` default, the amend's stale band, the note-citing comment
(section 4.4, section 4.3); the FOK ordering against `plan_fill` and the
aggressor justification (section 4.3); the partial-fill and market-remainder
lifecycle gaps (section 4.3); the probe's unstated procedure, the OFFSETS-based
ceiling, the estimator's missing correctness tests, the dead-code concern
(landing 1); the golden's `band_ticks` source, the retired accumulation
coverage, the invalid monotonicity condition, the RNG tests that assumed unequal
draws, the swept smoke's self-contradiction, the missing latency instrument
(landing 2); landing 3's unprovable gates and the owed span measurement
(landing 3); the re-roll-by-resubmit surface and the overstated determinism
(section 1.4).

REJECTED, with reasons:

- **"Rename `sizing.rs` or say it survives untouched" read as a demand to leave
  the bound alone.** Neither review actually demanded that, but the option was
  live and is declined: the bound's SHAPE is unchanged, so an implementer might
  reasonably skip it. This spec renames it anyway. A public item named after a
  deleted concept is how the next reader learns a model that no longer exists.
- **"Refuse the submit when there is no volatility evidence."** Offered as one
  of three options for the `MIN_VOL_SAMPLES` fallback. The reading is refused;
  the SUBMIT is not. Rejecting a client order because the venue's own estimator
  is cold makes the venue's internal state a client-visible error, and the
  no-reading path already has a correct answer (the order rests untriggerable
  until a walk has evidence). The third option, holding the last good reading,
  is rejected too: a stale reading is exactly the failure mode the truncated
  walk was fixed to avoid, and caching it would reintroduce it deliberately.
- **"Redesign the sweeper's off-lock lifecycle ordering."** Out of scope. The
  off-lock walk with revision-checked reapplication is an existing contract this
  spec inherits and does not touch; the finding's real content is that section
  1.4 claimed more determinism than that contract provides, and section 1.4 is
  what changed. A redesign would be a separate spec with its own measurement.
- **"Landing 3 must run the price-span probe over the archive before shipping
  slippage."** Rejected as a blocking requirement, accepted as a stated debt.
  Landing 3 introduces no new scale - it reuses the limit band's multiplier - so
  there is nothing for that probe to price that landing 1 has not already
  priced. The probe becomes required the moment a SEPARATE market multiplier is
  proposed, and `notes/problem-refused-order-types.md` continues to own it.
- **"Section 1.1's one-sided band contradicts the problem note's wording."**
  Neither review pressed this, and both explicitly endorsed the reasoning. Kept
  as written: the note's "a band around the order's stated price" is loose
  wording, and manufacturing fills better than any price the market offered is
  not a defensible reading of it.
