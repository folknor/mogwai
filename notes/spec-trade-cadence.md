# SPEC: the tape prints raw fills, in parent sweeps, at a real venue's cadence

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/problem-trade-cadence.md` (resolved: four rulings and one withdrawal) and
the PROBLEM STATEMENTS entry in `notes/todo.md` that owns it.

**Build path.** As every spec touching these APIs must state: the implementer
READS the nautilus and broadarrow APIs from the in-tree copies under
`research/`, and BUILDS against the SIBLING checkout `../nautilus_trader`, which
`mogwai-adapter/Cargo.toml` path-depends on with default features off. The two
are kept in sync; `research/` is never a build input.

This is a full rewrite of the arrival process, not a rescale of it. The
generator today draws one independent arrival per print at a 7.19 s mean gap
and one price update per print. It will draw one PARENT arrival per taker
event, materialize a BURST of raw-fill children under it, and publish the
children - which is the publication contract the problem statement settled and
the only layer at which "49.6 trades per second" is a statement about the same
quantity mogwai emits. Cadence, per-fill size, the dispersion form, the
round-lot grid, the history page size and the fanout depth all move with it,
because each is either denominated in the old cadence or fitted at the old size
scale.

It is not a book, not a level model, and not per-instrument profiles. Section 10
states exactly where the teardown stops.

## 1. The survey: what exists, and everything that depends on the cadence

### 1.1 The generator

`crates/mogwai-data/src/generated/` is the whole arrival process.

- `source.rs` - `GeneratedSource::next_tick` runs exactly one cycle per emitted
  print: `next_duration_ns` (ACD clock, session envelope, regime thinning),
  clock advance, `next_latent_mid` (GARCH + Student-t), `next_price`
  (bounce/drift, on-grid snap), `next_size` (lognormal + round-lot). One print,
  one arrival, one price shock. There is no notion of an event above the print.
- `dynamics.rs` - `AcdClock`, `GarchVol`, `BounceState`.
- `consts.rs` - the fitted block. `ACD_PERSISTENCE 0.9935`,
  `ACD_FEEDBACK_SHARE 0.08`, `ACD_WEIBULL_SHAPE 0.60`, `ACD_WALL_RELAX_TAU_S
  7200`, `ACD_RELAX_MEAN_CAL 1.2293`, the GARCH and bounce constants,
  `SIZE_LOG_SIGMA 1.15`, `START_PRICE_USD 60_000`, `TYPICAL_SIZE_MANTISSA/SCALE`
  (0.1), `VOL_SCALAR 5e-8`.
- `fingerprint.rs` - `GeneratorScalars` (the per-instrument knobs), `ScalarRanges`
  (their committed bands), `GoldenTargets` (the realism gate's anchors),
  `SessionProfile`.
- `numeric.rs` - `round_lot_size`, whose grid is the absolute literals 1.0 and
  0.1.
- `checkpoint.rs` - `CheckpointIndex`, `MAX_CHECKPOINTS 4096`, coarsening.
- `tests.rs` - `realism`, `run_seeded_tape_dwell_is_bounded`,
  `dwell_is_bounded_across_run_seeds`, `clean_regime_is_byte_identical`
  (the byte-for-byte golden), `monotonic_clock`, `on_grid_prices_and_native_aggressor`,
  `measure`, `empty_hour_stats`, `is_round_lot`, and the slack constants
  `DWELL_P999_SLACK 2.0`, `MEAN_GAP_REL_TOL 0.10`, `DURATION_ACF_ABS_TOL 0.14`.

### 1.2 Everything denominated in the cadence

Traced through the actual callers, not merely through what the structures admit:

| site | today | at 49.6 raw fills/sec |
|---|---|---|
| `CHECKPOINT_K = 8192` ticks (`mogwai-server/src/source.rs`) | 16 sim hours per snapshot | 165 sim seconds per snapshot |
| `MAX_CHECKPOINTS = 4096` before coarsening | first coarsening after ~7.5 sim years | after ~7.5 sim days |
| `MAX_HISTORY_LIMIT = 1000` (`mogwai-protocol`) | ~2 sim hours per `/trades` page | ~20 sim seconds per page |
| adapter `fetch_trades_windowed` | ONE fetch, no paging | silently answers a multi-hour `request_bars` with 20 seconds of tape |
| `fanout_depth = 4096` (`mogwai.toml`) | ~8 sim hours of ring slack | ~83 sim seconds, and at speed 100 that is 0.83 wall seconds before `FeedLagged` |
| `SWEEP_DRAIN_BUDGET = 20_000` (`mogwai-server/src/fills.rs`) | ~40 sim hours per pass | ~7 sim minutes per pass |
| `VOL_WINDOW_NS = 300e9` / `MIN_VOL_SAMPLES = 8` (`mogwai-data/src/trigger.rs`) | median 32 prints per window, 30% of instants refuse | ~15,000 prints per window |
| `round_lot_size` grid (1.0 / 0.1 absolute) | median draw 0.1 BTC, grid is the right scale | median draw 0.00268 BTC, so EVERY round-lot draw snaps to 0.1 - a 37x size spike on ~24% of trades |
| `warmup_ns` default 24 h | ~12,000 ticks at boot | ~4.29M ticks at boot |
| tape pacing (`mogwai-server/src/tape.rs`) | one `thread::sleep` and one JSON serialize per 7.19 sim seconds | per 20 sim milliseconds |
| `SESSION_DRAW = 5_000_000` ticks (`mogwai-data/src/generated/tests.rs`) | ~416 sim days, so a day-of-week assertion is well sampled | ~28 sim hours, so `session_modulation_reproduces_curves` asserts a weekly curve it never sees |
| tape lead `(fanout_depth / 2).max(1)` (`tape.rs`) | 2048 ticks, ~4 sim hours of pre-buffer | 41 sim seconds, and at the new default 11 sim minutes |
| literal `CHECKPOINT_K` / `SWEEP_DRAIN_BUDGET` mirrors in `mogwai-data/examples/fill_walk_bench.rs` | agree with the server | silently diverge unless moved in the same landing |
| `TAPE_PROTOCOL_VERSION = 1` (`mogwai-data/src/lib.rs`) | describes the current stream | describes a stream this spec replaces wholesale |

The last row is the only one whose verdict this spec cannot state in advance;
section 8.3 makes it a measurement with a threshold.

### 1.3 The gate, and which of its anchors survive

`realism` asserts, in order: `duration_dispersion_index` (as `var/mean`, in
SECONDS - not scale free), `duration_acf` at lags 1 and 5 against
`duration_acf_anchor` (the retired 0.1603 vector), `return_acf_lag1`,
`abs_return_acf` at lags 1/10/50, `zero_change_frac`, `round_lot_frac`,
`size_cv`, the on-grid and native-aggressor invariants, and then the dwell
block (p999 cadence-scaled, empty-hour fraction, longest empty-hour run,
declared-mean-gap agreement).

Every one of the return-shape anchors is computed PER PRINT, over a Kraken
corpus in which 61.1% of consecutive prints share a whole-second stamp. Section
6 rules on each individually rather than treating them as one bloc, because
they do not share a fate.

### 1.4 The sibling surveys this reconciles against

`notes/problem-instrument-model.md` owns the parameterization ruling: the model
is a complete parameterization and presets are named bundles of otherwise
tunable knobs. Consequence for this spec, applied throughout section 5: every
new arrival constant is a per-instrument SCALAR on `GeneratorScalars` with a
committed band, never a module const - so the instrument-model spec has
something to bundle. The four constants that stay module-level
(`SIZE_LOG_SIGMA`, the ACD/GARCH/bounce block,
`INTRA_EVENT_STEP_NS`, `CHILD_CAP`) are process SHAPE rather than instrument
identity, and `notes/problem-instrument-profiles.md` owns the open question of
whether the first of those becomes per-instrument. Nothing here forecloses it.
Section 10 restates the same boundary and must be read as agreeing with this
paragraph: the NEW cadence and size scalars get per-instrument slots; the ACD,
GARCH and bounce constants explicitly do NOT, and stay module-level here.

## 2. The model, stated exactly

### 2.1 Three layers, one process

A taker order arrives (a PARENT match event). It consumes one or more resting
makers, producing raw fills (CHILDREN) at one or more price levels. A feed then
publishes the children raw or aggregated. mogwai publishes them RAW - the
settled ruling - so the generator must own the parent rate and the child
multiplicity, or it would emit the right count with no clustering at all.

Measured, Binance BTCUSDT spot, June 2026:

| quantity | value | source |
|---|---|---|
| raw fills per second | 49.6 mean, 4 median, 257 p95, 13.4% empty seconds | 1s klines, `Number of trades` |
| aggTrades per second | 13.2 | aggTrades archive |
| match events per second | 5.84 | aggTrades collapsed to distinct timestamps |
| children (raw fills) per event | 8.49 (derived, 49.6 / 5.84) | two sources, hence section 4 |
| price levels per event | 2.26 (derived, 13.2 / 5.84) | as above |
| mean raw-fill size | 0.00492 BTC, $311 notional | klines |
| match-event gap | mean 0.171 s, `var/mean` 0.79 s, `var/mean^2` 4.62 | aggTrades collapsed |

### 2.2 The generation rule

Per PARENT event, in exactly this order (the order is the RNG contract; two
implementers who follow it produce the same stream and the golden in section
7.1 is stable):

1. **Parent gap.** The existing `next_duration_ns` unchanged in mechanism - ACD
   draw, wall relaxation, session envelope, regime thinning - but its declared
   mean is now `mean_event_duration_s`, the PARENT gap. The gap is measured from
   the LAST CHILD of the previous event, not from its parent instant. This
   introduces a mean bias of `(children_mean - 1) * INTRA_EVENT_STEP_NS`, which
   at the default is 7.5 microseconds against a 171 ms gap - 0.004%, four orders
   of magnitude inside `MEAN_GAP_REL_TOL`. Measuring from the parent instant
   instead would let a long burst overrun the next parent and force a clamp; the
   bias is the cheaper honesty.
2. **One latent-mid update.** `next_latent_mid` exactly as today, once. A sweep
   is ONE shock to the market, not eight. This is the load-bearing structural
   claim of the whole spec: every constant fitted at the per-print layer today
   (ACD shape, GARCH, bounce/drift, `zero_change_frac`) keeps its meaning at the
   per-EVENT layer, so only the cadence scalar, the size scalar and the new
   child block are refitted.
3. **Parent side and first price.** `next_price(mid)` as today. The returned
   side is the PARENT's taker side and every child shares it.
4. **Child count** `C`, section 2.3.
5. **Per child k in 0..C:** if `k > 0`, one Bernoulli draw at `level_step_prob`
   deciding whether this child advances one tick in the take direction (up for
   `Buyer`, down for `Seller`) from the previous child's price; then one
   `next_size` draw. The child is stamped at `parent_ts + k *
   INTRA_EVENT_STEP_NS`.

`INTRA_EVENT_STEP_NS = 1_000` (one microsecond). This is the resolution the
source archive carries, and it is CONFIRMED for the raw-trades archive rather
than assumed from the aggTrades sibling: `BTCUSDT-trades-2026-06.csv` stamps
rows with 16-digit values (`1780272000140314`), microseconds since epoch, with
no header row and the column order section 4.2 states. Had the raw archive been
millisecond-stamped, section 4.2's event inference would have been coarser than
the model and `children_mean` would have come back biased upward; it is not, so
that risk is closed rather than carried.

### 2.3 Child count: a two-component mixture with no free parameter

`C` is drawn as: with probability `q`, exactly 1; otherwise Geometric on
{1, 2, ...} with mean `m`. `q` and `m` are NOT fitted knobs - they are solved in
closed form at construction from the two measured statistics
`children_mean = C_bar` and `children_single_frac = s`:

    m = (C_bar - 1) / (1 - s)
    x = (C_bar - 1) / (m - 1)          // = 1 - q
    q = 1 - x

Derivation: `q + (1-q)/m = s` (a geometric with mean m puts 1/m on the value 1)
and `q + (1-q)m = C_bar`. Both solve to the above.

**The sampler is specified exactly, not left to the implementer.** The RNG
contract of section 2.2 is only a contract if two implementers draw the same
`C`. `next_count` is therefore:

1. Draw `u0 = rng.gen::<f64>()`, a uniform in `[0, 1)`. If `u0 < q`, return 1
   and consume nothing further.
2. Otherwise draw `u1 = rng.gen::<f64>()` and return
   `C = min(CHILD_CAP, 1 + floor(ln(1 - u1) / ln(1 - 1/m)))`.

That is the inverse CDF of a Geometric on {1, 2, ...} with mean `m` and success
probability `1/m`, with the half-open convention `u1` in `[0, 1)` so
`ln(1 - u1)` is finite and `C >= 1` always. `m = 1` cannot reach step 2 (it is
the `C_bar = 1` case `validate` already refuses). When the `min` binds, a
`truncated: u64` counter on `SweepShape` is incremented - the cap is otherwise
invisible to the gate in section 6.4, which asserts a truncation FRACTION and
cannot recover it from capped counts alone.

**Validity, and the fallback, stated rather than discovered.** The mixture can
only ADD single-child mass above the geometric baseline, so it exists iff
`s >= 1 / C_bar`. If the L0 measurement returns `s < 1 / C_bar`, the shape is
`q = 0, m = C_bar` - a pure geometric - and `children_single_frac` is recorded
in the fingerprint as documentation rather than as a target. `GeneratorScalars::validate`
enforces `s` in `[0, 1)` and `C_bar > 1`, and `SweepShape::new` selects the branch;
which branch was taken is stated in the fingerprint's `_doc` string so a reader
is never guessing.

`CHILD_CAP = 4096` truncates the geometric tail so one draw cannot produce an
unbounded burst. The largest inferred sweep in the corpus is 2,213 aggTrades on
BTC; the realism gate asserts the cap truncates fewer than 1e-5 of events, so a
future cadence that made the cap bite fails loudly instead of quietly clipping
the tail.

### 2.4 Levels: one Bernoulli, solved from one measured mean

With `C` children and per-child advance probability `p`, the expected number of
distinct price levels is `1 + (C_bar - 1) * p`. So

    level_step_prob = (levels_mean - 1) / (children_mean - 1)

At the defaults, `(2.26 - 1) / (8.49 - 1) = 0.1682`. `GeneratorScalars::validate`
requires `1 <= levels_mean <= children_mean`, which is exactly the condition for
`level_step_prob` to land in `[0, 1]`.

A sweep therefore walks the grid monotonically away from the mid in the take
direction and never back - which is what a taker consuming a book does, and it
is the same walk the venue's own fill band already narrates for a market order.
The walked price feeds the mid only through the existing `next_price` snap on
the NEXT event; the intra-burst walk does not perturb `GarchVol`.

### 2.5 Timestamps stay strictly monotone, deliberately

Binance stamps every child of one event with a single `transact_time`. mogwai
does not, and the reason is not aesthetic. Strict monotonicity is load bearing
in five places: `CheckpointIndex`'s binary search, `MergeSource::starting_at`,
the `/trades` window bound, the adapter's WS resume cursor (which advances to
`ts_event + 1` EXCLUSIVE, so same-nanosecond siblings not yet delivered would be
lost across a reconnect), and the paging rule this spec adds in section 7.1.
Sharing a stamp would require relaxing all five to non-decreasing and would make
the resume cursor lossy by construction. One microsecond apart preserves every
one of them, costs nothing at the resolution the source data has, and is a
truthful statement about a venue that timestamps each fill.

It also settles the WITHDRAWN decision 5 outright: distinct `ts_event` per child
makes the adapter's derived `TradeId` distinct per child, so no wire trade id or
sequence number is owed. And it is safe even if that reasoning were wrong -
`Cache::add_trade` in `research/nautilus_trader/crates/common/src/cache/mod.rs`
pushes onto a `BoundedVecDeque` with no key check, so nothing downstream
collapses byte-identical ticks and the density decision 1 selects for cannot be
silently undone.

### 2.6 Sizing is notional, per raw fill

    typical_size_median = typical_notional / start_price / exp(SIZE_LOG_SIGMA^2 / 2)

`typical_notional` is the MEAN notional of one RAW FILL, in quote currency
($311 for BTCUSDT). The `exp(sigma^2/2)` term (1.93715 at `SIZE_LOG_SIGMA = 1.15`)
converts the mean the archives report into the median `LogNormal::new` takes -
the 1.9x error the problem statement flagged, closed at the one site that can
close it.

**Which price divides it: `start_price`, not the live mid.** `start_price` is
config, so the size scale is part of the instrument's identity and is fixed for
the run rather than drifting with the walk. The consequence is stated plainly: a
run whose mid drifts far from `start_price` keeps its per-fill BASE size and
lets per-fill notional drift with the price, which is what a real venue does -
size is quoted in base units, not in dollars. Dividing by the live mid would
make per-fill notional constant, which no venue exhibits.

At the defaults: `311 / 60000 / 1.93715 = 0.0026757 BTC` median, `0.005183 BTC`
mean, against a measured mean of `0.00492`. Realized volume `49.6 * 0.005183 =
0.257 BTC/sec` against a measured `0.244`.

`typical_size` leaves `GeneratorScalars` entirely. Keeping both would let a
config state two incompatible size scales.

### 2.7 The round-lot grid becomes relative

`round_lot_size` currently snaps to whole units above 1.0 and to 0.1 below it,
with a 0.1 floor. At a 0.00268 median, that turns every round-lot draw into
exactly 0.1 - 37x the median - on the ~24% of trades `size_round_frac`
selects. The fix is one rule, decade-relative to the derived median:

    lot = 10^floor(log10(typical_size_median))
    round_lot_size(base) = max(lot, lot * round(base / lot))

At a 0.1 median this yields a 0.1 grid, reproducing today's sub-unit behaviour;
above 1.0 it yields multiples of 0.1 where today yields whole units, which is a
deliberate difference and re-blesses with the golden. `is_round_lot` in the test
module becomes the same predicate (a multiple of `lot`), because the two must
not drift.

## 3. Havoc: one new arm, and one clarified one

The problem statement records that the user expects cadence havoc knobs as well
as an honest default. The venue can already only THIN the tape
(`LiquidityDrought.thin_factor`, which multiplies gaps). This spec adds the
mirror and stops there.

    Divergence::FlowSurge { rate_mult: f64, children_mult: f64, duration_ms: u64 }

- `rate_mult` in `(1.0, 1000.0]` DIVIDES the parent gap, exactly as
  `thin_factor` multiplies it.
- `children_mult` in `[1.0, 100.0]` scales `children_mean` for the surge's
  duration; the mixture is re-solved from the scaled mean and the unchanged
  single fraction.
- `duration_ms` bounded by the existing `control::MAX_DIVERGENCE_MS`.

### 3.1 Ownership and the data path, because this is a new class of arm

Every existing `Divergence` acts in the SERVER's order and event path. None
touches the generator, and `MarketRegime` deliberately does not travel through
`/control/divergence` at all - it is boot config, handed to `GeneratedSource` at
construction. `FlowSurge` is therefore the first control-plane arm that reaches
into generator state, and the spec must say how, or the implementer invents it.

**It is not a mutation of live generator state.** It is an absolute SIM-TIME
WINDOW carried in the source, the same shape `SessionEdgeSpike` already uses:

- The arm records `surge: Option<SurgeWindow { start_ns, end_ns, rate_mult,
  children_mult }>` on `GeneratedSource`, where `start_ns` is the source's
  current `clock_ns` at the instant the arm is applied and `end_ns` is
  `start_ns + duration_ms * 1e6`. Both are SIM instants, so the window is a
  pure function of the tape's own clock and not of wall time.
- The owner is the per-symbol source handle the server already holds behind its
  mutex in `mogwai-server/src/source.rs`; the control endpoint applies the arm
  through that handle. The tape thread observes it implicitly, by pulling ticks
  from the same source - there is no second channel and no shared atomic.
- `begin_event` consults the window and nowhere else does. Both multipliers
  therefore take effect only at PARENT BOUNDARIES; a burst in flight when the
  window opens or closes finishes under the shape it started with. A window
  boundary landing mid-burst is not a state a venue produces, and the same
  argument section 5.3 makes for `ReopenGap` applies unchanged.
- `rate_mult` divides the parent gap at the point `next_duration_ns` returns it.
  It does NOT flow through `regime.arrival_thin`. That field is validated in
  `[1, 1000]` and `closed_window_gap_ns` reasons FROM that bound - `rate =
  arrival_mult / arrival_thin`, positive because the divisor is at least 1 - to
  conclude its integration walk cannot saturate `MAX_SESSION_GAP_NS`. Making the
  field sub-unit would silently change the closed-window integration and void
  that argument. Keeping the surge out of the field keeps the invariant intact
  and needs no re-derivation. (An earlier draft called `arrival_thin` "a signed
  multiplier"; nothing about it is signed and it does not change at all.)
- `ClearDivergences` clears the window. Re-arming replaces it outright rather
  than composing - two surges are one surge, the last one armed.
- **Historical resynthesis is deliberately CLEAN.** A `CheckpointIndex`
  snapshot is a clone taken before the arm existed, so a seek or a `/trades`
  replay through the surge window re-walks without it and returns a different
  tape than was streamed. Rather than invalidate checkpoints on arm, the venue
  states the contract: havoc is a LIVE-STREAM phenomenon, history is the clean
  process. That matches every other divergence, which perturbs the live order
  path and leaves the tape's history untouched, and it is the reason the window
  lives on the source rather than in the checkpointed generator state.
  `reference/havoc.md` says so in one sentence.
- Engine routing: `FlowSurge` is dispatched to the SOURCE, not armed on the
  engine. The engine's divergence match arm must reject or ignore it explicitly,
  so a future variant added to the enum cannot fall through into engine
  behaviour by accident.

`LiquidityDrought`'s doc comment gains one sentence: it thins EVENTS, never
children, so a drought makes sweeps rarer and leaves each sweep's shape intact -
which is what a venue losing takers looks like.

Gate: a `mogwai-protocol` serde round-trip test for the new variant, a
`validate_flow_surge` bounds test alongside the existing validator tests, and a
`mogwai-data` test asserting an armed surge raises realized raw-fill rate by
approximately `rate_mult * children_mult` inside the window.

**The identity claim outside the window is stated correctly, because the obvious
one is false.** An armed surge cannot leave the post-window tape byte-identical
to clean under a single RNG stream: the extra children consume extra Bernoulli
and lognormal draws, permanently shifting every later draw. The assertion is
therefore the achievable one - the tape BEFORE `start_ns` is byte-identical to
clean, and after `end_ns` the tape is statistically clean (the same realism
bands, not the same bytes). Buying byte-identity after the window would require
a separate RNG stream per surge, which is a complexity this spec refuses; the
weaker claim is the honest one and the test's comment says which claim it is
making.

## 4. Landing 0: the measurement, with a proceed/close threshold

The contract requires the instrument before the rewrite it justifies, and here
two of the three new scalars are DERIVED from a ratio between two different
sources (klines' raw counts over aggTrades' collapsed events) rather than
measured. `children_single_frac` is not measured at all. So L0 measures them
directly and can close the spec.

### 4.1 Acquisition

**BTCUSDT is ALREADY DONE.** `research/market-data/BTCUSDT-trades-2026-06.zip`
is on disk: 914 MB compressed, 9.77 GB as the single member
`BTCUSDT-trades-2026-06.csv`. It was fetched directly from the published URL
with the standard library, because
`research/binance-public-data/python/download-trade.py` (note the `python/`
segment; an earlier draft named the parent directory) needs `pandas`, which is
not installed here. Do not re-fetch it.

Still to fetch, the same way, into `research/market-data/`:

    https://data.binance.vision/data/spot/monthly/trades/ETHUSDT/ETHUSDT-trades-2026-06.zip
    https://data.binance.vision/data/spot/monthly/trades/SOLUSDT/SOLUSDT-trades-2026-06.zip

Budget for them on the order of the BTC archive - roughly a gigabyte
compressed and ten uncompressed apiece, which is why every probe in section 4.2
streams the zip member rather than extracting it. `research/market-data/` stays
gitignored, and by the repository owner's ruling that is fine: gates in this
spec MAY cite those archives directly. See section 4.3.

### 4.2 The probe

New: `analysis/probe_binance_trades.py`, streaming and O(1) in memory, matching
the discipline of its two siblings. Raw-trade columns are `id, price, qty,
quote_qty, time, is_buyer_maker, is_best_match`, no header row, `time` in
microseconds. Events are INFERRED, not identified, and the probe says so in its
docstring exactly as the aggTrades probe does.

**Two grouping rules, both reported, because they are not the same rule.**
`analysis/probe_binance_aggtrades.py` groups by DISTINCT TIMESTAMP ONLY. This
probe's primary rule adds the taker side: consecutive rows sharing both `time`
and `is_buyer_maker` are one event, because two takers on opposite sides landing
in the same microsecond are two events and the aggTrades rule cannot see that.
The probe reports `children_mean` and `children_single_frac` under BOTH rules,
and `cadence.json` records both. This matters directly: section 2.1's 8.49 is a
ratio taken ACROSS the two rules (kline raw counts over timestamp-collapsed
aggTrades), which is exactly the composition L0 exists to replace.

The "fraction of events whose children share one stamp" statistic an earlier
draft asked for is DELETED: it is 1.0 by construction of a rule that groups by
shared stamp. What replaces it is the statistic that actually carries
information - the gap distribution BETWEEN consecutive distinct stamps within a
one-second window, which is what tells us whether the microsecond spacing this
spec imposes in section 2.5 is finer than, coarser than, or comparable to the
venue's own.

It reports, per file and per grouping rule:

    children per event: mean, single-child fraction, p95, max
    price levels per event: mean, single-level fraction
    raw fills/sec, mean notional per raw fill
    parent gap: mean, var/mean, var/mean^2, ACF lag1 and lag5
    per-second raw-fill counts: mean, median, p95, zero-second fraction

The last line is new and is what section 4.4's feasibility pre-check reads.

**A preliminary reading already exists, and it says PROCEED.** Grouping the
first 2,000,000 rows of the BTC archive (the opening hours of 2026-06-01) by
`(time, is_buyer_maker)` gives 349,508 events: `children_mean = 5.72`,
`children_single_frac = 0.728`. Both land inside section 4.4's PROCEED band, and
`s = 0.728 >= 1 / 5.72 = 0.175`, so the section 2.3 mixture exists and the pure-
geometric fallback is not the branch taken. This is one partial day of one
symbol read with a throwaway loop, NOT the probe's output, and it does not
discharge L0 - the committed probe over the full month is still the evidence.
It does mean the spec is very unlikely to hit its own CLOSE branch, and it
retires the largest of the section 11 risks from "unmeasured" to "measured
once".

### 4.3 The committed artifact

**The reproducibility obligation is WAIVED by the repository owner.** Gates in
this spec MAY cite `research/market-data/` directly even though it is gitignored
and unchecksummed. There is no requirement that the acquisition be reproducible
from a fresh clone, that the archives be checksummed, or that a derived artifact
be committed IN ORDER FOR gates to stop citing gitignored data. Nothing below
should be read as re-imposing it, and a future reviewer raising it is raising a
settled question.

`analysis/cadence.json` is still committed, but for a different and smaller
reason: the generator reads its scalars from a fingerprint, and re-deriving them
from a ten-gigabyte archive on every build is absurd. It is a cache of a
measurement, not a substitute for evidence.

New: `analysis/build_cadence.py`, which runs the three probes over whatever
archives are present and writes `analysis/cadence.json`.

**The probes must return structured results, not print them.** All three
existing probes today print human-readable reports and return nothing.
`build_cadence.py` cannot parse stdout and must not try. Part of L0 is therefore
a small refactor: each probe grows a function that returns a `dict` of its
statistics, and its `main` becomes a formatter over that dict. The printed
output is unchanged, so nothing that reads these probes by eye is disturbed, and
`analysis/test_characterize.py` gains a case per probe pinning the dict's keys.

```json
{
  "provenance": {
    "generated_utc": "...",
    "archives": [{"name": "BTCUSDT-trades-2026-06.zip",
                  "bytes": 0, "rows": 0, "span_days": 0.0}]
  },
  "anchor": "BTCUSDT",
  "pairs": {"BTCUSDT": {"...": 0.0}},
  "targets": {
    "mean_event_duration_s": {"anchor": 0.0, "range": {"min": 0.0, "median": 0.0, "max": 0.0}},
    "children_mean": {"...": 0.0},
    "children_single_frac": {"...": 0.0},
    "levels_mean": {"...": 0.0},
    "typical_notional": {"...": 0.0},
    "duration_dispersion_cv2": {"...": 0.0},
    "per_second_counts": {"mean": 0.0, "median": 0.0, "p95": 0.0, "zero_frac": 0.0}
  },
  "shape": {"q": 0.0, "m": 0.0, "level_step_prob": 0.0, "fallback_pure_geometric": false}
}
```

`build_fingerprint.py` then MERGES `cadence.json` into `fingerprint.json` under
a new top-level `cadence` key rather than recomputing it, so the Kraken corpus
pass and the Binance cadence pass stay independent and neither needs the other's
disk. `build_fingerprint.py` FAILS if `cadence.json` is absent, on the same
principle its `level_queue` already applies to a stale `char_*.json`: a
fingerprint missing a block the generator reads is worse than no fingerprint.

The band rule, applied by `build_cadence.py` and stated so a re-run re-derives
it rather than copying a literal: for each target, `min` and `max` are the
per-pair extremes widened by a factor of 1.5 on each side, and `anchor` is
BTCUSDT. `duration_dispersion_cv2` additionally carries a hard floor of 1.0
(Poisson), so a de-clustered tape fails the gate no matter what the widening
produced.

### 4.4 The proceed/close threshold

Read off `analysis/cadence.json` for BTCUSDT:

- **PROCEED** if `children_mean` is in `[3.0, 20.0]` and
  `single_child_fraction < 0.90`. The parent/child model is then carrying real
  structure and section 5 lands as written.
- **CLOSE** if `children_mean < 1.5`. The 8.49 factor was then an artifact of
  comparing a kline count against a collapsed aggTrade count, and there is no
  parent layer to model. **CLOSE means STOP, not "land a different spec."** An
  earlier draft said sections 5.2 through 5.4 are deleted and everything else
  lands unchanged; that is not true, because most of what remains is written in
  terms of parents and children - `children_mean`, `levels_mean`,
  `children_mult`, the event-layer restatement in 6.3, the burst-batched pacing
  fix in 8.3, the microsecond child spacing in 2.5, the event-boundary detector
  the gate needs. A single-layer landing is a different specification with its
  own types, gates and exclusions, and it does not exist in this document. On
  CLOSE the implementer commits the L0 evidence, records the verdict in
  `notes/todo.md`, and stops; the single-layer rewrite is spawned as its own
  spec if the user wants it.
- **STOP AND ASK** in the gap between them, or if `children_mean > 20.0`. Either
  reading contradicts both sources and the discrepancy is the finding.

### 4.4b The feasibility pre-check, which is the harder question

`children_mean` is not the only thing that can sink section 5, and it is not the
one most likely to. The density gate in 6.4 asks the model to reproduce mean
49.6 fills/sec, MEDIAN 4, and 13.4% empty seconds simultaneously. The child
mixture cannot supply that spread: a geometric is exponential-tailed, so
essentially the whole mean/median gap and the whole empty-second mass has to
come from the PARENT clock alone - a median second holding well under one event
while the mean holds 5.84, and 13.4% of seconds holding zero against a 0.171 s
mean gap, where a Poisson process would give `e^-5.84 = 0.003`. That is a severe
demand on a clock whose only permitted tuning (section 5.4) is one mean-
calibration constant, since 5.4 explicitly refuses to re-run the
persistence/feedback grid.

So L0 answers it before L1 is written, at a cost of minutes:

1. `probe_binance_trades.py` reports the measured per-second count
   distribution (mean, median, p95, zero fraction) - the section 6.4 targets,
   read off the corpus rather than off klines.
2. `analysis/check_cadence_feasible.py` (new, pure Python, no corpus) simulates
   the SPEC'S OWN parent process - the ACD clock with its committed
   `ACD_PERSISTENCE`, `ACD_FEEDBACK_SHARE` and `ACD_WEIBULL_SHAPE`, at the
   measured `mean_event_duration_s` - crossed with the section 2.3 mixture at
   the measured `q` and `m`, for a few million events, and prints the same four
   per-second statistics.

### 4.4b-RESULT: the check has been RUN, it FAILED, and the owner has ruled

This is no longer a pre-check to perform. It was performed, and the numbers are
recorded here so nobody re-derives them:

| | measured BTCUSDT | ACD clock as committed |
|---|---|---|
| mean fills/sec | 49.64 | 41.87 |
| median | 4 | 12 |
| p95 | 257 | 177 |
| empty seconds | 13.35% | 30.73% |

The parent/child layer PASSED - 8.49 children per event, 55.87% single-child, so
section 2.3's mixture is sound and section 5.2 stands. What failed is the ACD
ARRIVAL CLOCK, and the shape of the miss is the useful part: the simulated tape
carries THREE TIMES the median and MORE THAN TWICE the empty seconds. It is not
uniformly too slow. It alternates between dead seconds and steady moderate
activity where the real tape alternates between dead seconds and violent bursts.
That is a process-shape failure, which is exactly why one mean-calibration
constant cannot reach it.

THE OWNER HAS RULED THAT THIS DOES NOT STOP THE WORK. The end state gets
implemented; residuals are fixed afterwards. The gate advises, it does not
decide, and it must NOT be treated as a CLOSE verdict or as a reason to halt
before L1.

The permitted response, in order:

1. **REFIT the ACD clock against Binance.** Section 5.4's refusal to re-run the
   persistence and feedback grid is LIFTED. `ACD_PERSISTENCE`,
   `ACD_FEEDBACK_SHARE`, `ACD_WEIBULL_SHAPE` and `ACD_RELAX_MEAN_CAL` are all
   refittable against the raw-trades corpus. This is the first move because the
   committed values were fitted to a corpus of whole-second Kraken stamps that
   cannot describe sub-second arrival structure at all, so they were never
   evidence for behaviour at this cadence; refitting them against data that CAN
   describe it is what should have happened regardless.
2. **REPLACE the arrival process** if a refitted ACD still cannot reach the
   shape. A refit that leaves the median and empty-second fraction outside their
   bands is evidence that the family is inadequate rather than the constants,
   and a self-exciting process is the standard answer for arrivals that cluster
   this hard. Take this step only on that evidence, and state in the landing
   which constants were tried and what they produced.

Re-run `analysis/check_cadence_feasible.py` after a refit; it is the cheap
oracle for whether the family can reach the shape, and it stays useful precisely
because it costs minutes. Its verdict is now INFORMATIVE, not blocking.

The 6.4 bands still do NOT get widened until they pass. Refitting or replacing
the process to meet the measured distribution is the work; moving the target to
meet the process is not.

### 4.5 L0 gates

    python3 analysis/probe_binance_trades.py research/market-data/BTCUSDT-trades-2026-06.zip
    python3 analysis/build_cadence.py
    python3 analysis/check_cadence_feasible.py
    python3 -m unittest discover -s analysis -t .

`analysis/test_characterize.py` gains cases pinning BOTH of the probe's
event-grouping rules, the probes' new structured-result dicts, the geometric
sampler's inverse-CDF convention against a fixed uniform sequence, and the
closed-form mixture solve against hand-built fixtures - including the
`s < 1 / C_bar` fallback branch, which is the one path a real corpus may never
exercise.

## 5. Landing 1: the generator rewrite

One coherent, fully intrusive change. It re-blesses every golden, so it cannot
be split without landing a knowingly-red boundary.

### 5.1 `GeneratorScalars`, exactly

```rust
pub struct GeneratorScalars {
    pub symbol: String,
    pub modal_tick: Decimal,
    pub price_decimals: u32,
    /// Mean gap between PARENT match events, seconds. Was `mean_duration_s`,
    /// which meant the gap between prints; at the raw-fill layer those are
    /// different quantities by `children_mean`.
    pub mean_event_duration_s: f64,
    /// Mean raw fills per parent event. Must exceed 1.0.
    pub children_mean: f64,
    /// Fraction of parent events carrying exactly one raw fill.
    pub children_single_frac: f64,
    /// Mean distinct price levels per parent event. In `[1.0, children_mean]`.
    pub levels_mean: f64,
    pub size_round_frac: f64,
    pub start_price: Decimal,
    /// MEAN notional of one raw fill, quote currency. Replaces `typical_size`.
    pub typical_notional: Decimal,
    pub vol_scalar: f64,
}
```

`ScalarRanges` gains `mean_event_duration_s`, `children_mean`,
`children_single_frac`, `levels_mean` and `typical_notional`, and drops
`mean_duration_s`. `validate` gains, beyond the range checks: `children_mean >
1.0`, `children_single_frac` in `[0.0, 1.0)`, `1.0 <= levels_mean <=
children_mean`, `typical_notional > 0`, and the derived median size positive and
representable at `SIZE_DECIMALS` (a notional so small that
`typical_notional / start_price / 1.937` rounds to zero at 8 decimals is a
config that would emit nothing but the size floor, and is refused by name).

`from_fingerprint_medians` and `xbtusd_anchor` both read the new fields from
`fp.cadence.targets`.

`typical_size` leaving `GeneratorScalars` is not a Rust-only edit. It also
appears in `analysis/build_fingerprint.py`, `analysis/fingerprint.json` and
`analysis/decode_dwell_bins.py`, and all three move with it in the same landing
or the fingerprint states a size scale nothing reads. Same for
`mean_duration_s` becoming `mean_event_duration_s`.

### 5.2 The burst state

New in `dynamics.rs`:

```rust
#[derive(Clone)]
pub(super) struct SweepShape {
    /// Probability the event is a single fill.
    pub(super) q: f64,
    /// Mean of the geometric component.
    pub(super) m: f64,
    pub(super) level_step_prob: f64,
    /// Count of draws clipped by `CHILD_CAP`, and of draws total. Read by the
    /// realism gate's truncation-fraction assertion.
    pub(super) truncated: u64,
    pub(super) drawn: u64,
}

impl SweepShape {
    pub(super) fn new(children_mean: f64, single_frac: f64, levels_mean: f64) -> Self;
    /// Draws C in 1..=CHILD_CAP by the inverse CDF of section 2.3. Consumes
    /// exactly one uniform, plus one more when the geometric branch is taken -
    /// the RNG contract of section 2.2. Takes `&mut self` so a truncation at
    /// `CHILD_CAP` can increment `truncated`; the gate in 6.4 asserts a
    /// truncation FRACTION and cannot recover it from the capped counts.
    pub(super) fn next_count(&mut self, rng: &mut ChaCha12Rng) -> u32;
}

/// The children of the current parent still owed to `next_tick`. Part of
/// `GeneratedSource` state, so a `CheckpointIndex` snapshot taken mid-burst
/// resumes mid-burst and the chain stays byte-identical.
#[derive(Clone)]
pub(super) struct SweepBurst {
    pub(super) remaining: u32,
    pub(super) emitted: u32,
    pub(super) parent_ts_ns: u64,
    pub(super) side: AggressorSide,
    pub(super) price_ticks: f64,
}
```

`GeneratedSource` gains `shape: SweepShape` and `burst: SweepBurst` (empty at
construction) and drops nothing.

### 5.3 `next_tick`

```rust
fn next_tick(&mut self) -> Option<TickEvent> {
    if self.burst.remaining == 0 {
        self.begin_event();          // steps 1-4 of section 2.2
    }
    Some(TickEvent::Trade(self.next_child()))   // step 5
}
```

`begin_event` is the current body of `next_tick` up to and including
`next_price`, plus the count draw; it sets `burst` and leaves `clock_ns` at the
parent instant. `next_child` advances the level with one Bernoulli when
`emitted > 0`, draws the size, sets `clock_ns = parent_ts_ns + emitted *
INTRA_EVENT_STEP_NS`, decrements `remaining`, and returns the tick.

`clock_ns()` keeps its documented meaning - the `ts_event` of the last emitted
tick - and the next parent gap is added to it, which is the "measure from the
last child" rule of section 2.2 falling out of the structure rather than being
imposed on it.

The `ReopenGap` crossing check stays where it is, in `begin_event`, so a halt
lands between events and never inside a burst. A burst is one atomic exchange
action; halting halfway through one would be a state no venue produces.

### 5.4 Refitting `ACD_RELAX_MEAN_CAL`

`ACD_RELAX_MEAN_CAL = 1.2293` was bisected to cancel a Jensen term whose size
depends on `exp(-gap / ACD_WALL_RELAX_TAU_S)`, and the relaxation weight moves
from ~0.999 at a 7.19 s cadence to ~0.99998 at 0.171 s. The constant is
therefore INVALID at the new cadence and must be re-derived by the procedure its
own comment documents: a 10-step bisection on `[1.0, 1.8]` driving the seed-42
realized mean PARENT gap of the 2M-event realism draw onto
`scalars.mean_event_duration_s`, committed as the final bracket midpoint rounded
to four decimals.

`ACD_WALL_RELAX_TAU_S` stays 7200. It is a WALL horizon and the wall behaviour
it bounds - how long an excursion persists in simulated seconds - is exactly
what must not move when the cadence does. That is the property the drought
landing bought and this spec must not spend.

The grid over `ACD_PERSISTENCE` and `ACD_FEEDBACK_SHARE` is NOT re-run.
Re-running it would refit the clustering shape against the same Kraken corpus
section 6.2 disqualifies for sub-second work. If the re-derived `RELAX_MEAN_CAL`
cannot land the mean-gap gate at any value in `[1.0, 1.8]`, that is a finding
about the ACD block at sub-second cadence and it comes back to the user rather
than being absorbed by widening the grid.

### 5.5 What moves in `mogwai-server`

- `CHECKPOINT_K` 8192 -> 262_144. **The invariant being preserved is stated
  explicitly, because the old value did not buy 90 minutes.** At 7.19 s/print
  8192 ticks was 16.4 SIM HOURS per snapshot; 262,144 ticks at 49.6 fills/sec is
  88 sim MINUTES. That is an 11x REDUCTION in sim tape per snapshot, not a
  restoration of it, and it is chosen deliberately: the quantity that must not
  regress is the RESIDUAL DRAIN COST - the ticks a seek must replay after the
  nearest snapshot, which is bounded by `CHECKPOINT_K` and is what the walk
  actually pays. Holding sim hours constant instead would mean 2.9M ticks of
  residual replay per seek, a 32x cost regression on the seek path the
  lifecycle landing bought. Holding ticks constant would mean a snapshot every
  165 sim seconds and `MAX_CHECKPOINTS` coarsening after 7.5 sim days. 262,144
  is a 32x tick increase against a 42x cadence increase: residual drain grows
  32x in ticks but the index spans 88 minutes per snapshot and reaches its
  coarsening ceiling after ~250 sim days. The doc comment states all three
  denominations - ticks, sim time at the default cadence, and residual replay
  cost - so the next cadence change finds the coupling instead of rediscovering
  it.
- `SWEEP_DRAIN_BUDGET` 20_000 -> 5_000_000, and **the property it preserves is
  restated, because the old one no longer holds.** The old comment claimed "two
  orders of magnitude above the default 100 ms interval's expected handful of
  ticks, and still terminates a multi-hour gap in bounded work". At 49.6/sec a
  100 ms pass expects ~5 ticks, so 5M is six orders above the expectation, not
  two. The half that survives is termination: a multi-hour gap is ~700k ticks
  and stays inside the budget. The half that must be replaced is the cost bound,
  because a budget is now a WALL-TIME bound on a blocking pass - at the measured
  synthesis throughput, 5M ticks is on the order of seconds against a 100 ms
  sweep interval. The doc comment therefore states the budget as a wall bound
  (`SWEEP_DRAIN_BUDGET / measured ticks-per-sec`), L2 records the measured
  number, and the sweep's existing "raise SWEEP_DRAIN_BUDGET before reading this
  table" diagnostic gains a companion WARN when a single pass actually consumes
  more than half the budget - which is the signal that the venue is spending
  seconds inside a 100 ms tick.
- `fanout_depth` default 4096 -> 65_536 in `Config::default` and `mogwai.toml`,
  and a boot-time WARN when `fanout_depth` is below one wall second of projected
  frames (`children_mean / mean_event_duration_s * speed`). The ring is the
  difference between a slow client and a FAILED RUN - `FeedLagged` closes the
  socket with WS 1011 - so a default sized for the old cadence is a correctness
  problem, not a tuning one. **Second consequence, stated because it is not
  obvious:** `tape.rs` derives its pacing lead as `(fanout_depth / 2).max(1)`,
  so this also moves the tape's lead from 2048 to 32,768 ticks. At the new
  cadence 32,768 ticks is ~11 sim minutes of pre-buffered tape against the 2048
  ticks that were ~4 sim hours - so the lead SHRINKS in sim time even as it
  grows in ticks, which is the direction that matters and is why the derivation
  is left alone rather than re-based.
- **The benchmark's mirrors move too.** `crates/mogwai-data/examples/fill_walk_bench.rs`
  carries literal copies of both `SWEEP_DRAIN_BUDGET` and `CHECKPOINT_K`, with
  comments saying they mirror the server. Changing only the server copies leaves
  the benchmark measuring a configuration the venue no longer runs. Both move in
  the same landing.
- **`SESSION_DRAW` moves.** `crates/mogwai-data/src/generated/tests.rs` draws
  5,000,000 ticks for `session_modulation_reproduces_curves`, which asserts a
  DAY-OF-WEEK distribution and therefore needs at least a full week of sim tape.
  At 7.19 s/print 5M ticks was ~416 sim days; at 49.6 fills/sec it is ~28 sim
  hours, so the day-of-week assertions become invalid or fail outright. The
  constant is re-denominated to hold SIM SPAN rather than tick count: at least
  four sim weeks, which is ~150M raw fills and is too expensive as a per-run
  draw. So the test is additionally re-pointed at the PARENT series - one
  observation per parent event, using the same event-boundary accessor section
  6.3 introduces - which restores a four-week span at ~20M events. If that is
  still intolerable in wall time the honest lever is a shorter asserted span
  with the curve tolerances re-derived, not a shorter draw with the old
  tolerances.
- **`TAPE_PROTOCOL_VERSION` 1 -> 2.** `crates/mogwai-data/src/lib.rs` declares
  it, `mogwai-server/src/main.rs` prints it in the long version string, and
  `mogwai-server/tests/lifecycle.rs` asserts the banner carries it. This landing
  changes the generator's constants, its RNG consumption, its timestamps, its
  size scale and the fingerprint's schema: a client that cached a tape from
  version 1 and resumes against version 2 gets a different stream at the same
  seed and offset. That is exactly what the version exists to announce, so the
  bump is an explicit L1 artifact and the banner assertion is a gate on it, not
  an afterthought at release time.
- A boot INFO line projecting warmup cost: expected ticks
  (`warmup_ns / 1e9 / mean_event_duration_s * children_mean`) and projected
  synthesis seconds at the throughput measured in section 8.2, escalating to
  WARN above 60 projected seconds. No refusal: the user ruled warmup length is
  theirs. This is the "extreme warmup fails loudly rather than mysteriously"
  obligation the problem statement left to the spec.

## 6. Landing 1, second half: the gate

### 6.1 The dispersion index is restated dimensionlessly

`duration_dispersion_index` (`var/mean`, seconds) becomes
`duration_dispersion_cv2` (`var/mean^2`, dimensionless) everywhere: in
`characterize.py`'s output, in `build_fingerprint.py`, in `GoldenTargets`, and
in `Measured`. The Kraken side is derivable from the committed `char_*.json`
with no corpus access - `cv2 = dispersion_index / mean_s` - so this needs no
re-run of `run_corpus.py`.

The BAND the gate asserts comes from `cadence.json` (Binance match events,
`4.62` on BTC and `3.57` on SOL as measured so far), not from the Kraken band:
the generator's parent gaps are now targeting the Binance process, so gating
them against the Kraken spread would assert a shape the tape is deliberately not
producing. The Kraken `cv2` stays in `fingerprint.json` as recorded corpus fact.

### 6.2 The Kraken duration ACF anchor is retired, and re-anchored on Binance

Per the ruling: `duration_acf_anchor`, `DURATION_ACF_ABS_TOL` and both
`assert_near` calls in `realism` are DELETED. 0.1603 is an artefact of
whole-second stamps - collapsing same-second Kraken prints takes it to 0.0012 -
so it certifies a property it never measured, and no interim replacement is
invented. `duration_acf_anchor` also stops being written by
`build_fingerprint.py`, because a committed number nothing reads is a number
someone will later read.

**But the anchor does need a successor, and `cv2` is not it.** An earlier draft
claimed `cv2` with its Poisson floor catches a de-clustered tape. It does not:
`cv2` describes the MARGINAL gap distribution, and an IID heavy-tailed process
has `cv2` well above 1 with exactly zero serial dependence. Retiring the ACF
anchor while leaning on `cv2` would leave an ACD regression free to flatten all
clustering and still pass the gate - the ACD block is the one piece of the
generator whose entire purpose is serial dependence, and it would be the one
piece nothing asserts.

So the successor is the same statistic against a defensible reference. L0
already measures the parent-gap ACF at lags 1 and 5 on the Binance corpus, at
the layer mogwai now generates. `cadence.json` records it as a TARGET with a
cross-pair band (widened per section 4.3's band rule, with a hard floor: lag-1
ACF strictly above zero), and `realism` asserts the generated parent-gap ACF at
both lags against it. This is not the retired 0.1603: that number was a per-
print artefact of whole-second Kraken stamps measuring a layer that does not
exist here, whereas this is measured on microsecond-stamped parent events of the
same process the generator targets. The ruling that retires 0.1603 "with no
successor" is honored in the sense that mattered - no interim replacement is
INVENTED - and the new anchor is measured rather than invented.

If L0's measured lag-1 parent-gap ACF comes back at or below zero, the ACD block
has nothing to reproduce at this cadence and that is a finding for the user, not
a reason to ship an unguarded gate.

Consequence recorded for the todo owner: `liquidity_drought_imitates_dying_symbol`
currently asserts against `duration_acf_anchor[0]`. It is re-pointed at the
drought's realized-versus-clean parent-gap RATIO, which is what that test is
actually about, and its comment says so.

### 6.3 The return-shape anchors move to the EVENT layer

`return_acf_lag1`, the three `abs_return_acf` lags and `zero_change_frac` are
computed per PRINT today. At the raw-fill layer a print is no longer a price
update - at `level_step_prob = 0.1682` a mean event advances the price 1.26 times
across 8.49 children, so roughly 6.2 of every 8.5 prints repeat the previous
child's price by construction - so per-print `zero_change_frac` would jump toward
0.73 for a reason that says nothing about the market, and the abs-return ACF would be
measuring burst geometry.

Ruling: all five are measured over the EVENT series, one observation per parent
event, taking the event's LAST child price as the event's price. `measure` gains
an event-boundary detector (a child with `emitted == 0`, exposed for the test
module through a `#[cfg(test)]` accessor on `GeneratedSource` rather than
re-derived from timestamps, so the gate and the generator cannot disagree about
where an event starts).

The honesty caveat, stated because the alternative is an unearned claim: the
committed Kraken anchors are per-print over a corpus where 61.1% of consecutive
prints share a whole second, which makes them approximately BUCKET-layer
statistics already. Comparing mogwai's event layer against them is CLOSER to
like-for-like than comparing the print layer would be. It is not an identity - a
Kraken bucket is one second, a mogwai event is 171 ms - so the gate asserts the
cross-pair BAND rather than the point anchor for all five, and the band's width
is doing real work rather than covering seed wobble.

### 6.4 New: the density gate

The settled ruling is that the model targets the DISTRIBUTION and the gate
asserts all four moments as bands. `measure` gains `per_second_counts` over the
draw's whole span, and `realism` asserts:

| statistic | target (BTC) | band | why this width |
|---|---|---|---|
| mean raw fills/sec | 49.6 | +/- 10% | a direct consequence of two declared scalars; anything wider would not detect a mis-derived `children_mean` |
| median raw fills/sec | 4 | `[target - 1, target + 1]` | integer per-second counts with a one-trade quantisation floor; the weakest of the four and the band says so |
| p95 raw fills/sec | 257 | `[0.5x, 2.0x]` | a tail quantile off a 2M-event draw carries real sampling noise |
| zero-trade second fraction | 0.134 | +/- 0.05 absolute | a fraction near a small value, so an absolute band, matching the existing `EMPTY_HOUR_FRAC_SLACK` convention |
| `CHILD_CAP` truncation fraction | 0 | `< 1e-5` | the cap must not be shaping the tail |

The truncation fraction is read off `SweepShape`'s `truncated / drawn` counters
(section 5.2), NOT inferred from the emitted counts. A capped draw is
indistinguishable from a draw that legitimately landed on `CHILD_CAP` once it
has been clipped, so without the counter the assertion cannot be computed at
all.

`DRAW` becomes 2,000,000 PARENT EVENTS (about 17M prints, about 4 sim days). Memory: the
existing `measure` retains four vectors per print; at 17M prints that is roughly
1 GB and is not acceptable. `measure` is rewritten to stream - running sums, a
ring buffer for the ACFs (the same `AutoCorr` shape `characterize.py` and the
probes already use, ported to Rust in the test module), and a per-second count
vector of `u32` - so its memory is O(lags) plus one integer per simulated
second. This is itself a brick and lands before the assertions that use it.

**`gap_p999_s` is the one statistic streaming cannot supply, and it is handled
rather than dropped.** Today `measure` sorts the full duration vector and
indexes it. Running sums and a ring buffer cannot recover a quantile. Three ways
out, and the spec picks the second:

1. Keep the full duration vector. At 2M PARENT events that is 16 MB of `f64` -
   the memory problem was 17M PRINTS, not 2M parents, and the dwell block is
   denominated in parent gaps. This works but re-introduces an O(n) allocation
   the rewrite exists to remove.
2. **A fixed-width histogram of parent gaps** on a log grid from 1 microsecond
   to `MAX_SESSION_GAP_NS`, 64 bins per decade, with the p999 read as the bin
   edge containing the 0.999 rank. Memory is a few kilobytes. The quantile is
   then bounded, not exact - resolution is ~3.6% of the value at 64 bins per
   decade, against a `DWELL_P999_SLACK` of 2.0. A 3.6% read error against a 2x
   slack is not a real loss of gate power, and the constant's comment says so.
3. Reservoir sampling. Rejected: it makes the gate seed-dependent in a way the
   byte-identical golden is not, and a tail quantile is exactly where a
   reservoir is weakest.

The histogram's bin edges are module constants in the test module, so a future
reader can see the quantisation the bound carries rather than inferring it.

### 6.5 The dwell block

Survives unchanged in form. `cadence = mean_event_duration_s /
dwell.mean_s.anchor` becomes 0.0596, so the p999 bound tightens to ~53 s against
a realized value that will sit near a second: the gate stops being the binding
constraint it was and becomes a cheap backstop, which is correct - the
zero-second fraction in 6.4 is now the live silence gate and it is far stricter.
`MEAN_GAP_REL_TOL` applies to PARENT gaps against `mean_event_duration_s`, and a
second assertion of the same form pins realized raw fills per second against
`children_mean / mean_event_duration_s`.

`empty_hour_stats` and its `dwell_stats` counterpart in `characterize.py` are
untouched, so the convention-drift exposure recorded in `notes/todo.md` is
neither worsened nor fixed here.

### 6.6 The parent/child structure itself is gated, not just its aggregates

Everything above asserts DISTRIBUTIONS. None of it would notice a generator that
produced the right marginal statistics with the wrong structure - children on
alternating sides, a non-monotone walk, eight latent updates per event, a burst
split by a reopen. The realism gate is a statistical instrument and structural
claims need structural assertions. `crates/mogwai-data/src/generated/tests.rs`
therefore gains a `sweep_structure` test, cheap (a few thousand events, not the
2M draw) and asserting per event, over the event-boundary accessor section 6.3
already introduces:

- every child of one parent carries the SAME `aggressor_side`;
- child prices are monotone in the take direction and never reverse;
- child `ts_event` values are exactly `parent_ts + k * INTRA_EVENT_STEP_NS`,
  and strictly increasing across the event boundary too;
- exactly ONE `next_latent_mid` call per parent (a call counter behind
  `#[cfg(test)]`, since this is the load-bearing structural claim of section 2.2
  and nothing else observes it);
- realized `children_mean`, `children_single_frac` and `levels_mean` over the
  draw agree with the declared scalars within a stated tolerance - the three
  numbers the whole spec is built on, currently asserted nowhere;
- a `ReopenGap` armed to fire mid-tape never splits an event: no event has
  children on both sides of the halt;
- a `CheckpointIndex` snapshot taken with `burst.remaining > 0` restores and
  resumes mid-burst, producing a byte-identical continuation - the property
  section 5.2 claims when it puts `SweepBurst` in generator state, and which
  today's checkpoint tests cannot exercise because no such state exists.

The last one is the one most likely to be silently wrong, because a burst that
does not survive a snapshot fails only on the seek path and only under load.

## 7. Landing 1, third half: the consumers

### 7.1 History paging - a hole this spec opens and must close

`MAX_HISTORY_LIMIT` is 1000 and `fetch_trades_windowed` performs exactly ONE
fetch. Today one page is ~2 sim hours, so a `request_bars` for 100 one-minute
bars is served. At 49.6/sec one page is 20 sim seconds, so the same request
returns a single bar and reports success. That is the venue silently serving
less history than it advertises - the exact failure class the lifecycle landing
removed from the seek path - re-entering through the page size.

Both halves are fixed, because either alone leaves a hole:

- `mogwai_protocol::MAX_HISTORY_LIMIT` 1000 -> 50_000. One page is then ~1000
  sim seconds and ~7 MB of JSON, synthesized in ~26 ms at the measured ~1.9M
  ticks/sec, comfortably inside `DEFAULT_REQUEST_TIMEOUT_SECS`.
  **This constant is also the DEFAULT, not only the ceiling:** `http.rs` computes
  the page size as `limit.unwrap_or(MAX_HISTORY_LIMIT).min(MAX_HISTORY_LIMIT)`,
  so raising it makes every `/trades` request that omits `limit` 50x more
  expensive - ~7 MB instead of ~140 KB, for a caller that expressed no opinion.
  The two roles are therefore split: `DEFAULT_HISTORY_LIMIT = 1_000` stays the
  no-opinion answer, `MAX_HISTORY_LIMIT = 50_000` becomes the ceiling an explicit
  caller may ask for, and the adapter (which always states a limit) asks for the
  ceiling. A default that grows 50x because a ceiling moved is a performance
  regression nobody requested.
- `fetch_trades_windowed` becomes a real loop: fetch, apply `stop`, and if the
  page came back FULL and `stop` did not fire, re-fetch with `start =
  last.ts_event + 1` and accumulate. It terminates on a short page (the window
  is exhausted), on `stop`, or on the request-wide budget below. The `truncated`
  flag it already returns keeps its meaning and now additionally covers the
  budget stop, which is the honest report the single-fetch version could not
  make.
- **The budget is request-wide in TRADES, not in pages.** An earlier draft set
  `MAX_TRADE_PAGES = 256`, which at 50,000 per page permits 12.8M `TradeTick`s
  accumulated in one adapter request - on the order of 1.8 GB of JSON parsed,
  plus the accumulated Rust vector and the converted nautilus objects, for a
  single `request_bars`. Risk 5 priced one page and not the accumulation. The
  ceiling is therefore `MAX_TRADES_PER_REQUEST = 1_000_000` (20 pages at the new
  page size, ~7 sim hours, and a bounded ~140 MB of transfer with a resident
  vector in the low hundreds of MB). That is a real ceiling on a real machine
  rather than a page count that happens to multiply badly. `MAX_TRADE_PAGES`
  survives only as a loop-safety backstop at 64.
- **Bar aggregation is not made incremental here, and that is a deliberate
  scope call.** `request_bars` needs the complete trade vector before it
  aggregates, and the helper returns a `Vec`. Streaming aggregation would be the
  structurally right answer and it is a separate change to the adapter's request
  path, not a rider on a cadence rewrite. What this spec owes is the bound, and
  `MAX_TRADES_PER_REQUEST` is it: the vector cannot exceed a stated size, and the
  constant's doc comment names incremental aggregation as the thing that would
  let it be raised.
- Stale comments in `crates/mogwai-adapter/src/client/data.rs` already DESCRIBE
  paging that does not exist - "Paging overshoots the trade ceiling by up to one
  page", the "same-ts wedge" note. The implementer should read them as the
  design this landing finally builds, not as documentation of current behaviour,
  and reconcile each one rather than leaving a file that claimed to page before
  it did.

`start = last.ts_event + 1` is exact and lossless precisely because section 2.5
kept timestamps strictly monotone. Under Binance-style shared stamps it would
drop every sibling of the last child on each page boundary.

Gate: `crates/mogwai-adapter/tests/data_client_transport.rs` gains a case
requesting a window spanning more than one page and asserting the full window
arrives, in order, with no duplicates at the seams.

### 7.2 The fill band's cold-reading defect

`notes/todo.md` records the fill band inert on ~425 of 1440 sampled instants
(29.5%) because a 300 s window carries too few returns, and orders that item
AFTER this one on the theory that it is a symptom. At 49.6/sec that window
carries ~15,000 prints against a `MIN_VOL_SAMPLES` of 8, so the theory predicts
the rate collapses to zero.

This spec does NOT pre-emptively touch `VOL_WINDOW_NS` or `MIN_VOL_SAMPLES` -
either would move the estimator's identity and force a second golden re-bless -
and instead re-measures with the instrument that produced the 425 figure:

    brokkr test -p mogwai-server vol_probe

Threshold: a refusal rate at or below 1% closes the todo item, and the closing
note goes into `reference/architecture.md` next to the band's description rather
than into the notes file that dies.

Above 1%, the estimator itself is at fault rather than the tape - and the fix an
earlier draft named was backwards. If a 300 s window carrying ~15,000 prints
still refuses, SHORTENING the window reduces its sample population and can only
make refusals more frequent; the sample count is manifestly not what is failing.
The diagnosis is then something else - the window is empty because the sweep
never advanced the source to cover it, or the returns inside it are degenerate
(the ~73% of prints that repeat the previous child's price by construction,
section 6.3, could leave a window with 15,000 samples and almost no distinct
returns). That last is the likely culprit and it is a REAL defect this rewrite
introduces: `MIN_VOL_SAMPLES` counts prints, and prints are no longer price
updates. So the fix, if the threshold is missed, is to re-denominate
`MIN_VOL_SAMPLES` in NON-ZERO returns rather than in prints, leaving
`VOL_WINDOW_NS` alone - landed as a separate change with its own golden
re-bless, since by then the tape is green. The measurement decides which of the
two diagnoses holds; the spec does not guess.

### 7.3 Bars, ticks and everything counting prints

Recorded, with no action, because each is a consequence the user weighed:
adapter-fabricated OHLCV now folds ~8.5x more trades per bar; tick bars and
trade-count indicators move by the full 8.5x; the venue's own sweep and vol
walks sample the same tape more finely, so resting limits fill more readily and
stops trigger sooner at the same underlying path. `reference/architecture.md`
gains one paragraph stating the publication contract - the tape publishes RAW
FILLS grouped into sweeps - because a consumer counting ticks needs to know
which layer it is counting.

## 8. Landing 2: the measurements the change invalidates

### 8.1 Golden re-bless - which lands in L1, not L2

**The landing boundary, stated once so 8.1 and section 9 agree.** The golden
re-bless is an L1 deliverable: L1 cannot be green without it, and section 9's
sequence is correct where this section's heading was misleading. 8.1 is
documented here, with the rest of the invalidated measurements, because the
PROCEDURE belongs with them; the WORK happens inside L1, last, after the gate
restatement, since a re-bless is read off a passing gate. Nothing in 8.1 may be
deferred past the L1 boundary - a knowingly-red golden is exactly the state
section 5's "one coherent landing" rule exists to prevent. 8.2 and 8.3 are the
real L2.

`clean_regime_is_byte_identical` holds twenty literal `TradeTick` debug strings.
Every one changes. The re-bless procedure is the one the file already documents:
run the test, confirm the diff is a whole-stream change rather than a drift in a
suffix, paste the new literals. A SUFFIX-ONLY diff would mean the burst
machinery only engaged partway through the sample and is a failure, not a pass;
the re-blessing implementer is told to check that explicitly.

`crates/mogwai-server/tests/golden/fill_distribution.json` re-blesses for the
same reason - the tape under the fills is a different realization.

### 8.2 Throughput

`reference/performance.md` gets an APPENDED row set (never edited in place):

    brokkr run fill_bench -- --bench
    brokkr run fill_walk_bench -- --bench

Expected direction, stated so a surprise is legible: per-PRINT synthesis cost
should FALL, because one GARCH/Student-t/duration cycle now amortizes over ~8.5
prints while a child costs one Bernoulli plus one lognormal draw.
`walk_one_pass_*` should RISE, because a one-second span now carries ~50 prints
instead of ~0.14.

**Directions are not thresholds, and section 9 promises thresholds.** So:

- per-print synthesis cost must not RISE at all. Any increase means the burst
  path is doing work the model does not describe, and it is a finding that stops
  L2 rather than a number that gets recorded.
- per-print synthesis cost is EXPECTED to fall by roughly the amortization
  factor. A fall of less than 2x against the ~8.5x amortization means the child
  path is far more expensive than "one Bernoulli plus one lognormal", which is
  also a finding - recorded and brought back, not silently accepted.
- `walk_one_pass_*` may rise by up to the cadence factor (~42x) and no more. A
  larger rise means the walk is paying something super-linear in tick count,
  which at this cadence is a scaling defect and not a constant-factor cost.

Each bound is against the pre-change numbers already in
`reference/performance.md`, which is why that file is appended to and never
edited in place.

The `~1.9M ticks/sec` figure cited in section 5.5's warmup projection is a
pre-change measurement; L2 replaces it with the re-measured one and updates the
projection's divisor.

### 8.3 Pacing lateness under acceleration

The one consequence this spec cannot rule on in advance. `tape.rs` pays one
`thread::sleep` and one `serde_json::to_string` per tick, and at 49.6/sec with
`speed = 100` that is 4,960 frames per wall second with a nominal 202
microsecond inter-tick sleep - below typical OS timer granularity, so the tape
may fall behind the sim clock. Falling behind is not a cost problem: it makes
`ts_event` systematically older than the clock the client stamps its own
requests with, which is a lateness the consumer cannot distinguish from a slow
venue, and acceleration is a settled premise rather than an optimization.

Instrument: a new `#[ignore]`d test in `crates/mogwai-server/tests/serving.rs`
that runs the paced tape at `speed = 100` for 30 wall seconds against the
accelerated config and reports the maximum and p99 of `wall_now - sim.wall_ns(ts)`
across delivered frames.

    brokkr test -p mogwai-server tape_lateness_under_acceleration

Threshold: p99 lateness at or below 50 ms (the same order as the protocol's
30 ms honest-feed latency floor) needs no change and the reading is recorded in
`reference/performance.md`. Above it, the fix is specified rather than
discovered: `pace` batches - it sleeps once for a whole BURST rather than once
per child, since a burst is 8.5 microseconds of sim time and the children are by
construction simultaneous to any observer, and it serializes the burst's frames
before sleeping rather than between them. That halves the syscall rate at
minimum and removes the sub-granularity sleep entirely.

**That fix needs an interface it does not have, and the artifact is named here
rather than discovered mid-landing.** `tape.rs` pulls one unmarked tick at a
time through `TickSource::next_tick`; nothing in the returned `TickEvent` says
whether it opened an event. Burst-batched pacing therefore requires either a
`next_event(&mut self) -> Option<Vec<TickEvent>>` on the `TickSource` seam - with
a default implementation returning a one-element batch, so `MergeSource` and
`KrakenCsvSource` need no change - or a boundary flag on the tick itself. This
spec prefers the former, because a flag on the tick leaks generator structure
into the wire types, and because `MergeSource` interleaving two symbols' bursts
must be able to keep each burst contiguous, which a flag cannot express. Either
way it is a NEW L2 artifact with its own gate, and it lands only if the
threshold above is missed - which is why it is specified but not scheduled.

## 9. Landing sequence and the gates per brick

Each landing is kept or reverted on its own gate results, and the suite is green
at every boundary between them.

**L0 - measurement** (section 4). No Rust changes, so nothing can go red.
Deliverables: the ETH and SOL raw-trade archives fetched (BTC is already on
disk, section 4.1), `analysis/probe_binance_trades.py`, the three probes
refactored to return structured dicts, `analysis/build_cadence.py`,
`analysis/check_cadence_feasible.py`, `analysis/cadence.json` committed, new
cases in `analysis/test_characterize.py`.

    python3 analysis/probe_binance_trades.py research/market-data/BTCUSDT-trades-2026-06.zip
    python3 analysis/build_cadence.py
    python3 analysis/check_cadence_feasible.py
    python3 -m unittest discover -s analysis -t .
    brokkr check

BOTH verdicts have already been read and L0's artifacts already exist; see
section 4.4b-RESULT. The parent/child threshold PASSED. The density feasibility
check FAILED, and the owner has ruled that it does not stop the work: L1
proceeds, with the ACD refit permitted as its first move and replacement of the
arrival process permitted if a refit proves the family inadequate. Do not halt
at this landing and do not treat the failed check as a CLOSE verdict.

**L1 - the rewrite** (sections 2, 3, 5, 6, 7, and the golden re-bless of 8.1).
One landing. In dependency order inside it: the streaming `measure` rewrite
(6.4) before any assertion that uses it; `SweepShape`/`SweepBurst` and
`next_tick` before the `RELAX_MEAN_CAL` bisection, which is measured against the
new walk; the structural gates (6.6) before the statistical ones, since a
structural failure explains a statistical one and not the reverse; the gate
restatement before the golden re-bless, since the re-bless is read off a passing
gate. `TAPE_PROTOCOL_VERSION` bumps here.

    brokkr check
    brokkr check --gate
    brokkr test -p mogwai-data realism
    brokkr test -p mogwai-data run_seeded_tape_dwell_is_bounded
    brokkr test -p mogwai-data dwell_is_bounded_across_run_seeds
    brokkr test -p mogwai-data clean_regime_is_byte_identical
    brokkr test -p mogwai-data monotonic_clock
    brokkr test -p mogwai-data sweep_structure
    brokkr test -p mogwai-data session_modulation_reproduces_curves
    brokkr test -p mogwai-server lifecycle
    brokkr test -p mogwai-adapter data_client_transport
    brokkr test -p mogwai-adapter havoc
    brokkr run mogwai -- serve
    python3 scripts/smoke.py

`--gate` rather than plain `check` because `mogwai-adapter` is touched (section
7.1) and the plain check cannot see the four socket-backed adapter binaries.
`session_modulation_reproduces_curves` and `dwell_is_bounded_across_run_seeds`
are `#[ignore]`d and therefore invisible to `brokkr check`; both read the
arrival process directly and must be run by name for this landing.

**L2 - the invalidated measurements** (section 8).

    brokkr test -p mogwai-server vol_probe
    brokkr test -p mogwai-server tape_lateness_under_acceleration
    brokkr run fill_bench -- --bench
    brokkr run fill_walk_bench -- --bench

Each has a stated threshold and a stated fix; none is a "look and see".

**Documentation, landed with the code and not alone.** `reference/architecture.md`
gains the publication-contract paragraph (7.3) and the closing note on the fill
band if 7.2 clears; `reference/config.md` gains `typical_notional`,
`mean_event_duration_s`, the three child scalars and the `fanout_depth` sizing
relation; `reference/havoc.md` gains `FlowSurge` and the clarified
`LiquidityDrought` semantics; `reference/performance.md` gains appended rows;
`notes/todo.md` loses the fill-band-inert entry if it closed, and loses
`notes/problem-trade-cadence.md` from the problem set with the surviving prose
moved into `reference/` per that file's own rule.

## 10. The stopping rule

Out of scope, named rather than deferred:

- **No book, no levels beyond the sweep walk.** The distinct-price structure
  here is a one-Bernoulli walk fitted to one measured mean. Real level depth,
  queue position and resting liquidity stay refused - `notes/problem-order-book.md`
  was deleted on that ruling and nothing here reopens it.
- **Per-instrument process constants.** Whether `children_mean`, the ACD block
  and the volatility constants cluster per instrument is
  `notes/problem-instrument-profiles.md`'s open empirical question. This spec
  gives the CADENCE and SIZE scalars a per-instrument slot and a cross-pair band
  (`mean_event_duration_s`, `children_mean`, `children_single_frac`,
  `levels_mean`, `typical_notional`). It deliberately does NOT give one to the
  ACD, GARCH or bounce constants, or to `SIZE_LOG_SIGMA`, `INTRA_EVENT_STEP_NS`
  and `CHILD_CAP` - those stay module-level process shape, exactly as section
  1.4 states. Nothing here forecloses promoting them later; this spec simply
  does not do it, and an earlier draft of this bullet claimed otherwise.
- **Contract instruments.** `typical_notional / price` yields a fractional
  contract, which is `notes/problem-instrument-model.md`'s territory by the
  problem statement's own ruling. Spot only here.
- **Aggregated publication.** The venue does not offer an aggTrade-layer feed
  toggle. One contract, stated in `architecture.md`.
- **GTD/`expire_time`, trigger-act latency havoc, the dead-feed watchdog, the
  terminal-venue-fault decision.** Separate todo items, untouched.
- **Multi-instrument correlation.** Strategies are single-instrument by settled
  premise; independent per-symbol tapes stay correct.
- **The `analysis/` test-runner decision.** L0 adds cases to the existing
  `python3 -m unittest discover` bridgehead and does not decide whether that
  becomes the standing runner or joins `brokkr check`. That is a project-shape
  call recorded in `notes/todo.md`.

## 11. Risks recorded honestly

1. **`children_mean` is derived, not measured** - substantially retired. The
   parent/child layer was justified by 49.6 / 5.84, a ratio between two
   archives measured by two probes under two different grouping rules. A
   preliminary direct reading over 2M rows of the BTC raw-trades archive
   (section 4.2) gives 5.72 with a 0.728 single-child fraction, which is inside
   the PROCEED band and admits the section 2.3 mixture. It is one partial day of
   one symbol, so L0's full-month probe is still owed; but the risk is now
   "the number may move within the band" rather than "the layer may not exist".
2. **The event-layer restatement of the return anchors is an argument, not a
   measurement.** Section 6.3 claims the Kraken per-print anchors are
   approximately bucket-layer statistics and therefore closer to mogwai's event
   layer than to its print layer. That is reasoning about a corpus that cannot
   adjudicate it - the same limit the problem statement records for 0.1603. The
   mitigation is asserting the wide cross-pair band rather than the anchor, and
   saying so in the test's comment so nobody later reads the pass as a
   verification of the argument.
3. **`RELAX_MEAN_CAL` may not converge in `[1.0, 1.8]` at 0.171 s.** Section 5.4
   states the escalation rather than widening the grid silently.
4. **The realism draw gets ~8.5x more expensive in prints even at constant
   events.** 2M events is ~17M prints. The streaming `measure` rewrite keeps
   memory flat but not time; if the test's wall time becomes intolerable in
   `brokkr check`, the honest lever is fewer EVENTS with the band widths
   re-derived for the smaller sample - not a quietly reduced draw with the old
   bands left in place.
5. **History requests get much larger, at two levels.** One page at
   `MAX_HISTORY_LIMIT = 50_000` is ~7 MB, and one paged REQUEST is bounded by
   `MAX_TRADES_PER_REQUEST = 1_000_000` at ~140 MB transferred with a resident
   vector in the low hundreds of MB, because `request_bars` aggregates from a
   complete vector (section 7.1). The venue is loopback-local and the adapter's
   timeout is 30 s, so both are comfortable here; neither would be over a real
   network, and both constants' doc comments say which assumption they are sized
   against. Incremental bar aggregation is the change that would lift the
   request-wide bound, and it is out of scope.

6. **The density distribution may be unreachable by the ACD clock at 0.171 s.**
   Mean 49.6, median 4 and 13.4% empty seconds together demand almost all the
   spread from the parent clock, and section 5.4 permits tuning only its mean
   calibration. Section 4.4b makes this a pre-check at L0 with a stated
   escalation, so it is answered in minutes of Python rather than discovered
   after the golden re-bless. It is nonetheless the single most likely reason
   this spec does not land as written.

7. **`FlowSurge` history is clean while the live stream is surged.** Section 3.1
   rules that deliberately - havoc is a live phenomenon, checkpoints are not
   invalidated on arm - but it means a client that seeks back across a surge
   window sees a tape that never surged. That is a real observable asymmetry, it
   matches every other divergence's behaviour, and it is written into
   `reference/havoc.md` so no consumer discovers it by surprise.

8. **The retired duration-ACF anchor gets a measured successor, which is itself
   new evidence.** Section 6.2 gates the parent-gap ACF against a Binance
   measurement that does not exist yet. If L0 returns a lag-1 ACF at or below
   zero, this spec has no clustering gate and the ACD block has nothing to
   reproduce - a finding that goes to the user rather than a gate that gets
   dropped.
