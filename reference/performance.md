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
