# Performance record

Measured numbers for the fill path. Every later change that moves one APPENDS a
row; rows are never edited in place, so this file is a history and not a
snapshot.

## How to run

```
brokkr run fill_bench -- --bench
brokkr run fill_walk_bench -- --bench
```

Both are criterion benchmarks shipped as EXAMPLE targets, not `[[bench]]`
targets: `criterion_main!` parses `--bench` out of its own argv, `brokkr run`
forwards everything after `--` raw and defaults to release, and examples link
dev-dependencies so criterion never enters the shipped dependency graph.

Read a verdict from `target/criterion/<bench_id>/new/estimates.json`:
`std_dev.point_estimate` divided by `mean.point_estimate`, both in nanoseconds.
Not from the console line - criterion leads with a confidence interval on the
mean, which is a different and much narrower quantity, and eyeballing it in
place of the deviation passes benchmarks the rule below is meant to reject.

**Usability rule.** A benchmark whose standard deviation exceeds 5 percent of
its mean is not a regression gate: a later 3 percent regression is invisible in
it. Re-shape it (longer span, larger batch, smaller timed region) and re-measure
before recording the number.

**There is no automatic throughput gate, deliberately.** Criterion's saved
baselines live under `target/`, which is gitignored and not portable between
machines, and a wall-clock assertion inside `brokkr check` would be flaky on any
shared machine. The benches are operator-run before and after a change that
touches the fill path, and this file is where the two readings are compared.
Fill BEHAVIOUR is gated automatically, by
`crates/mogwai-server/tests/golden/fill_distribution.json`, which runs in
`brokkr check`.

## What each id measures

`fill_bench` (`crates/mogwai-engine/examples/fill_bench.rs`):

- `submit_immediate` - one accepted limit at `fill_band_vol_mult = 0.0`, so the
  drawn band is zero-width and the order fills at submit exactly as the old
  ungated default did: validation, `plan_fill`, `commit_fill`, ledger apply,
  `record_fill`, account snapshot.
- `submit_banded_rest` - the same submit with a nonzero band and no market
  reading, so the order draws its trigger and rests. The per-submit cost the
  banded venue pays.
- `submit_banded_marketable` - the same with a marketable reading, so
  `trades_through` holds against the drawn trigger and the submit fills
  synchronously. `Engine::process` supplies no market price, so only this
  benchmark reaches the branch the real path takes on every banded submit.
- `apply_scans_50` / `apply_scans_200` - one `apply_scans` batch over 50 and 200
  resting orders with no result at threshold: the common pass.
- `apply_scans_50_all_fill` / `apply_scans_200_all_fill` - the same batches with
  every result at threshold: the worst case, N fills plus one snapshot.

`fill_walk_bench` (`crates/mogwai-data/examples/fill_walk_bench.rs`):

- `walk_one_pass_1_scan` / `_50_scans` / `_500_scans` - one
  `mogwai_data::scan_triggers` over a one-second span of the fitted BTCUSDT
  tape, at three scan counts. The scans are far from market, so no walk exits
  early on a satisfied scan.
- `scan_mapping_50` - the `PendingScan` to `TriggerScan` mapping the server
  wrapper builds once per symbol per pass, allocation included.
- `source_positioning` - the sweeper's fixed per-pass cost: a checkpoint restore
  out of a long-lived index taken under a lock, then the residual drain through
  `MergeSource::starting_at` behind a `Box<dyn TickSource>`.
- `mark_pass_1_future` / `mark_pass_4_futures` - the engine-owned portion of
  one mark pass over one and four open futures positions: mark application,
  unrealized P&L, maintenance-equity evaluation, and the authoritative account
  snapshot. Tape positioning is excluded and remains priced by
  `source_positioning` plus the uncached market-reading number below.

Every benchmark builds its state in the setup closure and hands it back as part
of the output. Both matter: an engine reused across iterations grows without
bound (retained client order ids, closed orders, fill history, resting orders)
and would report the average of a ramp as a latency; a `TickSource` reused
across iterations is already drained past `to_ns` and would report one
`next_tick` as the cost of a pass. And because `iter_batched` drops OUTPUTS
after stopping the timer but drops a consumed INPUT inside the timed region,
returning the state is what keeps a teardown out of the reading - it was worth
roughly 45 percent of `apply_scans_50` and took its deviation from 15.3 percent
(unusable) to 2.8 percent.

## Readings

2026-08-02, Linux x86-64 workstation (kernel 7.1.4-x64v3-xanmod1), release
profile, workspace at `cc54799` plus the RFC 4631 phase D working tree.

| id | mean | std dev |
|---|---|---|
| `submit_full_fill` | 631.6 ns | 2.80 % |
| `submit_gated_rest` | 302.9 ns | 1.76 % |
| `submit_gated_seeded` | 634.4 ns | 1.76 % |
| `apply_scans_50` | 2.464 us | 2.77 % |
| `apply_scans_200` | 19.02 us | 0.66 % |
| `apply_scans_50_all_fill` | 19.62 us | 1.54 % |
| `apply_scans_200_all_fill` | 85.18 us | 1.93 % |
| `walk_one_pass_1_scan` | 291.4 ns | 0.80 % |
| `walk_one_pass_50_scans` | 297.0 ns | 1.08 % |
| `walk_one_pass_500_scans` | 404.1 ns | 1.15 % |
| `scan_mapping_50` | 31.4 ns | 1.71 % |
| `source_positioning` | 328.1 ns | 1.65 % |

Every id is under the 5 percent bar, so none was deleted and none is recorded
as unmeasurable.

### The scaling verdict

`apply_scans_200 / apply_scans_50 = 7.72`. The two sizes differ by 4x, so a
purely linear cost would read 4x and a quadratic one 16x; 7.7x says the linear
scan of `open` per result is real and dominant at these sizes but has not yet
overtaken the per-result constant. The all-fill pair is `85.18 / 19.62 = 4.34`,
essentially linear, because there the per-fill work (ledger apply, fill record,
message construction) swamps the matching scan.

### L2's accepted cost bound

`scan_mapping_50 / walk_one_pass_50_scans = 31.4 / 297.0 = 0.106`. L2 accepted
one mapping allocation per symbol per pass on the condition that it stays at
least an order of magnitude under the walk. It lands at 10.6 percent - at the
bound rather than comfortably inside it, and worth restating honestly: the walk
figure is for a ONE-SECOND span, while the default `fill_sweep_interval_ms` is
100 ms, so a default-interval pass walks less tape and the true ratio is worse
than this. In absolute terms the mapping is 0.6 ns per scan against a pass that
also pays a checkpoint restore and a mutex acquisition (`source_positioning`,
328 ns), so it is not the thing to optimize first; if it ever becomes one, the
answer is to have `pending_scans` produce the tape-shaped scan directly, not to
reunite the two crates.

### What `source_positioning` does NOT include

It is built from `mogwai-data` and so omits one ingredient of the server's
`build_history_source`: the `InstrumentProfiles` lookup, a constant-time table
read outside any loop. It also omits the server-private `BoundedSeek` wrapper,
whose cost is one counter comparison per drained tick. So this number is close
to, but strictly below, the sweeper's true per-pass fixed cost.

## 2026-08-02 raw-fill cadence L2

| id | mean | std dev |
|---|---:|---:|
| `walk_one_pass_1_scan` | 8.314 us | 0.35 % |
| `walk_one_pass_50_scans` | 8.667 us | 0.42 % |
| `walk_one_pass_500_scans` | 11.788 us | 0.48 % |
| `scan_mapping_50` | 30.06 ns | 1.93 % |
| `source_positioning` | 8.162 us | 0.16 % |

The walk ratios against the prior readings are 28.5x, 29.2x, and 29.2x,
inside the cadence landing's 42x ceiling after adding a constant-time price
envelope rejection before the per-scan loop. At speed 100, the paced-tape gate
delivered 43,121 frames in 10 wall seconds with p99 lateness 19.34 ms and a
43.77 ms maximum, below the 50 ms threshold, so burst-batched pacing was not
needed. The fill-band probe read 32 day-spanning windows with zero refusals.
The uncached 300-second market reading measured 13.86 ms median on the dense
tape, above the 5 ms submit budget. Caching one reading per symbol per sweep
interval reduced the repeated-path median and p99 to 30 ns; the first command
in an interval still pays the synthesis on the blocking worker.

## 2026-08-03 futures mark ledger

Release profile, current instrument-model working tree.

| id | mean | std dev |
|---|---:|---:|
| `mark_pass_1_future` | 446.4 ns | 0.17 % |
| `mark_pass_4_futures` | 2.378 us | 0.07 % |

Both readings satisfy the 5 percent usability rule. The four-position engine
work is 5.33x the one-position work: super-linear, because the breach check is
quadratic in the symbols that share a settlement currency - it evaluates the
whole currency's equity once per symbol. At four symbols that is 2.4 us and
irrelevant; a run carrying dozens of futures symbols in one currency would want
the equity hoisted out of the per-symbol loop, which nothing today needs.

An earlier draft of this table read 396.9 ns and 1.677 us. Those were measured
with a per-POSITION margin row, which under-reported the work and, worse,
under-reserved the admission budget for a hedged book; the row is now
aggregated per symbol over positions AND resting orders, which is what the
current numbers price. These numbers do
not hide the dominant server cost: a cache miss still pays the previously
measured 13.86 ms tape walk per symbol, and the cache is single-entry, so a
multi-symbol pass can evict itself. The landing shares the HTTP cache with the
sweeper to remove duplicate same-bucket walks when the keys coincide, but it
does not close the separate market-reading performance item.

## 2026-08-03 warmup materialization throughput

Not a criterion benchmark: measured from a real `mogwai serve` boot, as the
interval between the `projected warmup synthesis cost` and `eager warmup
materialized` log lines, on the committed default config (24 h `warmup_ns`,
BTCUSDT, 4,288,935 projected ticks, 19 checkpoints retained). Release binary.

| run | elapsed | implied rate |
|---|---:|---:|
| 1 | 1.4883 s | 2.88 M ticks/s |
| 2 | 1.4920 s | 2.87 M ticks/s |
| 3 | 1.4790 s | 2.90 M ticks/s |

**2.9 M ticks/s**, spread 0.9 percent across the three. This is the number
`SYNTHESIS_TICKS_PER_SEC` in `mogwai-server/src/main.rs` exists to hold, and it
had been carrying 5 M - so the boot projection ran 1.7x optimistic, and the
60-second WARN threshold it gates was really firing at about 104 seconds of
actual cost. Corrected to 2.9 M.

Measuring the whole boot interval rather than tick synthesis alone is
deliberate: the constant predicts what an operator waits through, which includes
checkpoint retention and the frontier draw, not just the walk. Re-measure the
same way after any change to the generator, the checkpoint stride, or the tape
protocol - the previous value's stated provenance was a `fill_bench` row, and no
such row has ever existed in this document.

## 2026-08-04 protocol 7 BBO composition

The `mogwai tick-composition` fixtures measure each of the five presets
independently over 2,000,000 parent events, eight seeds, and four arrival modes.
Each preset is resolved through `config::profile_from_preset`, the same path the
venue boots from, so preset inheritance, scalar defaulting, the size grid, the
session profile and the calendar are the venue's own: MNQ and MES are measured
on the whole-contract grid under CME hours rather than the crypto fractional
grid with no calendar. Every stream starts at the fingerprint's highest-intensity session
hour. Protocol 6 is the
trade-frame projection of the same protocol 7 realization; quote placement
draws no randomness and changes neither timestamps nor child counts. The
fanout arm continues to 6,000 simulated seconds so both speed 1 and speed 10
carry the required 600-wall-second sampling horizon even under maximum surge.
Every simulated second between the first and last measured frame is represented,
including zero-frame seconds. Wall-rate tails use exactly 600 bins per speed.
The reported p99.9 is the linearly interpolated empirical quantile at rank
`(n - 1) * 0.999`, so a 600-bin horizon does not collapse p99.9 to the maximum.

An earlier measurement predated those corrections: it measured BTCUSDT once per
seed and mode and relabeled four cloned rows as other presets, omitted empty
simulated seconds, used a ceiling rank that made every 600-bin p99.9 equal the
maximum, and ran every preset on the spot size grid with no calendar. Its
figures are gone from this document; the ones below are the corrected
measurement. The correction moved one constant, the cumulative warmup ceiling,
by 70,000,000 frames out of 81 billion. That the other three survived it intact
is a property of the two-times headroom and the power-of-two rounding, which are
coarse enough to absorb the whole error - not evidence that the defects were
harmless.

The run costs about an hour on a workstation, nearly all of it the surged arm:
maximum surge compresses arrivals 1000-fold, so 2,000,000 parent events span
roughly 340 simulated seconds and the traversal must then continue to the
6,000-second fanout horizon on tape that feeds the wall-rate bins alone.

The maximum paired p99.9 protocol-7/protocol-6 ratios were:

| budget denominator | ratio | resulting constant |
|---|---:|---:|
| simulated-second checkpoint work | 1.388889 | 1,048,576 |
| 300-second sweep work | 1.583145 | 282,000,000 |
| 24-hour cumulative warmup reach | 1.589436 | 81,124,000,000 |
| wall-second fanout work | 1.546711 | 262,144 |

Every ratio exceeds the 1.05 materiality threshold. Each resulting constant is
the old constant multiplied by the ratio and by two for headroom, then rounded
up as specified by the BBO layer measurement protocol: powers of two for the
checkpoint and fanout, and the next million for sweep and warmup reach. The sweep
formula alone produced 16,000,000. The required-reach rule applies the worst
measured p99.9 rate of 938,928.666 frames per simulated second over the whole
horizon, then rounds up. That raises the sweep budget above 281,678,599 frames
and the cumulative warmup ceiling above 81,123,436,742 frames. The directly observed
partial-window counts, 258,678,966 and 1,364,836,628, are lower and therefore
do not set either result. All four are fractional now, because the p99.9 is an
interpolated quantile rather than an element of the sample.

The warmup reach is not the per-lock runaway backstop. `MAX_EXTEND_TICKS`
remains 1,073,741,824 ticks per index-lock acquisition; boot materialization may
use multiple such chunks, releasing the global lock between them, up to the
81,124,000,000 cumulative measured-reach ceiling. The sweep's refusal ceiling
is likewise separate from its 2,500,000-tick latency warning threshold.

The checkpoint calculation starts from the prior approximately 88
simulated-minute spacing contract. The two-times headroom and power-of-two
rounding make the final bound deliberately longer than an exact preservation;
it cannot shorten the baseline horizon. At the worst measured wall rate, the
old 65,536-frame fanout held 0.007398 wall seconds; the resized fanout holds
0.029553 wall seconds, so its horizon does not shrink. Regenerate both fixtures
with
`brokkr run mogwai -- tick-composition --out-6 analysis/tick-composition-protocol-6.json --out-7 analysis/tick-composition-protocol-7.json`,
then run `python3 analysis/tick_composition_ratios.py` after any
event-composition change. One invocation emits both: protocol 6 is a count
projection of the protocol-7 tape, so the two fixtures are counted off a single
traversal and are paired by construction rather than by two runs agreeing. Both
documents carry the same `pairing_id`, which the ratio script asserts on, so a
fixture paired with a stale partner is refused rather than silently ratioed.
Both are serialized in full and staged beside their destinations before either
is touched, so a serialization failure or a full disk cannot consume a finished
run. The two renames remain two operations - a crash between them leaves a new
protocol 6 beside an old protocol 7, and two paths cannot be replaced atomically
as a pair - so what is guaranteed is DETECTION of that mismatch, not its
prevention. `--jobs N` sets worker count, defaulting to the machine's
parallelism.
