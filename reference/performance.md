# Performance record

Measured numbers across every operational surface, plus how to obtain and read
them. Every later change that moves a number APPENDS a row; rows are never
edited in place, so this file is a history and not a snapshot.

Numbers measured through `brokkr mogwai` carry their result UUID, so any claim
here can be re-derived - `brokkr results <uuid>` and `brokkr sidecar <uuid>`.
Numbers from the criterion harnesses do not, and are pinned by commit instead.

## The criterion harnesses

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

## The invocation surface

Settled 2026-08-10, replacing the two-layer setup that preceded it. There are no
layers and no frozen workloads: the argv is composed at the call site and
captured verbatim in the row, so pairing rows is a query rather than a name
lookup. The design and what it deliberately defers are in
`notes/benchmarking-design.md`.

Two kinds of surface, the same three modes over both. Recording a row needs a
clean tree.

**CLI surfaces, through the shipped bin.** No registration beyond the bin
itself, so benching measures what ships, startup and argument parsing included.

```
brokkr mogwai -- gen --type summary --symbol MNQ --seed 1
brokkr mogwai -- arrival-screen --cost-probe
```

**Harness surfaces, through an example target.** These are the loops with no
command line. They resolve by name against `[mogwai.targets.*]` in
`brokkr.toml`, which carries the feature shape each one needs.

```
brokkr mogwai arrival_walk --hotpath
brokkr mogwai screen_projection --alloc
```

`arrival_walk` (`mogwai-data`) runs the kernel draw with no projection attached;
`screen_projection` (`mogwai-lab`) runs one Stage A cell with the projection.
The gap between them is the measurement cost the Stage A round is spending
against, and it is read by SUBTRACTION rather than by annotation - see the
reading rules below.

Both need their crate's `hotpath` feature, which `required-features` enforces:
a plain `--examples` build skips them rather than producing an uninstrumented
binary that reports an empty profile. `hotpath-alloc` additionally arms the
allocation counting, and both harnesses declare the `#[global_allocator]`
themselves under it, because hotpath ships the allocator without installing it
and an alloc run without that line reports zero bytes everywhere. Registering
the feature shape is what makes `--hotpath` produce a profile at all; a target
registered without it records a row with an empty table.

`fill_walk_bench` and `fill_bench` are NOT registered targets. They are criterion
harnesses that parse `--bench` out of their own argv, which is what lets them
live in example targets, and which would collide with the mode flag. Run them as
in the section above.

The delivered July corpus is registered per host under
`[bygg.datasets.mnq-tbbo-july]` and pinned by an XXH128 digest over the whole
delivery DIRECTORY, not over its manifest. The two hashes on that corpus answer
different questions and neither replaces the other: this digest is a DRIFT check
- the delivery under that path not being the one it was last time - while
`measure`'s own SHA-256 verification against the ledger and the committed
preflight is about the CONTENTS being the ones the method was fitted to.

**The two output channels.** Every benched command emits its work size beside
its timing, unconditionally, on both channels
(`mogwai_lab::sidecar`): `key=value` scalars on stderr for the tracked
regression row, and timestamped phase markers plus counters on the marker FIFO
for the sampled `/proc` timeline. Absent the FIFO environment variable the
marker path is a load and a branch, so a plain interactive run pays nothing.

| command | markers | counters |
|---|---|---|
| `arrival-screen --cost-probe` | `probe` | `cells_evaluated`, `parents`, `prints`, `peak_rss_bytes` |
| `arrival-screen` | `coarse`, `refine`, per-family boundaries | as above, plus `coarse_s` / `refine_s` |
| `gen --type summary` | `walk` | `parents`, `rows` |
| `measure` | `observed`, `walks`, `bootstrap` | `elapsed_ms`, per-phase ms, `seeds`, `sessions`, `usable_sessions`, `peak_rss_bytes`, `scratch_bytes` |

Every surface is timed EXTERNALLY. `measure` briefly carried a self-reported
clock, on the reasoning that it verifies a multi-gigabyte corpus before any
measured work and an external wall folds that hash pass into every reading; that
was withdrawn 2026-08-10 because it redefined what the recorded elapsed MEANS
for one surface while the column said the same thing everywhere else. The
corpus pass is excluded the way every other phase boundary is - with a marker,
so `brokkr sidecar --durations` reports the measured phases alone and the
verification stays VISIBLE as its own phase rather than deleted from the record.
Timing everything externally is also what lets a history be back-filled to any
commit whose CLI still parses the invocation.

**Annotation discipline.** A small, stable set at phase boundaries, never a
function called millions of times per run - annotate its caller instead. Today:
`arrival_screen::project_stream` (once per seed walk) and
`SessionAcc::close_reduced` (once per session rotation). The value of a profile
is the same names appearing run after run.

This is a HARD limit, not a preference, and it decides how attribution is done.
`hotpath` queues one event per instrumented call and drains at roughly 1.3M
events per second. `project_stream` is a single loop over ~147M children per
cell, so annotating anything inside it - `session_segment_at`, `push_print`,
`close_parent` - would backlog tens of gigabytes and price the instrument rather
than the code. Attribution inside such a loop therefore comes from HARNESS
SUBTRACTION (run the same loop with one layer absent and difference the
per-parent cost) or from the allocation profile, never from a finer annotation.

## Reading rules

How a verdict is read off this file and `.brokkr/results.db`. Seeded 2026-08-10
from the first stored rows; this section grows as the ground is learned, and
every rule here should be traceable to a run.

- **This workload is CPU and RNG bound, not I/O bound.** The screen cell reads
  one committed JSON artifact and touches no disk thereafter (0 kB read and
  written across a 15 s cell, `d3fa0b0a`). So the error model of an I/O-bound
  project - drive state, trim debt, page-cache warmth, header-walk cache swings
  - does not apply here, and neither do its remedies. What moves a reading
  instead is host quiet, CPU frequency and thermal state, core count, and
  allocator behaviour.
- **The Stage A cell is single-threaded.** Avg cores 1.0, peak threads 9 but
  only one running (`d3fa0b0a`). A hotpath percentage over 100% would therefore
  be a finding here, not the routine cross-thread artifact it is in a pooled
  workload.
- **First noise reading: two runs of an identical cell measured 15.2 s and
  15.1 s** (`d3fa0b0a`, `3f82ed37`), with byte-identical counters. That is a
  ~0.7% spread on a single sample and the only variance datum on file. It is NOT
  yet an error bar: two runs minutes apart on a quiet host is the best case, and
  nothing has yet tested the same cell across a day, a thermal state, or a
  loaded host.
- **A wall-clock contract on this host is not yet trustworthy.**
  `tape_lateness_under_acceleration` asserts a 50 ms p99 pacing bound and failed
  at 311 ms p99 on 2026-08-08 with a load average of 1.46 across 32 visible
  CPUs, so load average alone is not a sufficient admission test. The bound is
  unchanged and authoritative; what is unresolved is the environment under which
  it can be evaluated. Open work in `notes/todo.md`.
- **Read counters before crediting a wall.** Every surface emits its work size
  beside its timing. A cell whose wall moved while `parents` or `prints` moved
  did different work, and the wall comparison is void rather than interesting.
- **`--hotpath` costs nothing at the current annotation density, and that is a
  property of the density.** The screen cell measured 15.04 s under hotpath
  against a 15.1 s clean wall; the alloc run measured 14.66 s against 14.61 s.
  Both are inside the noise above. That holds only while the annotation set
  stays at phase boundaries - see the discipline above.
- **Neither `--hotpath` nor `--alloc` reports peak RSS.** It comes from the
  sidecar timeline on a `--bench` or plain run. Peak anon is a headline quantity
  here rather than a footnote: the end state runs on the order of 200 venue
  instances on one host, so every instance-level cost is multiplied.

## Stage A baseline, 2026-08-09

Host `bygg` (AMD Ryzen 9 9950X3D2, 16c/32t, governor performance, kernel
7.1.4-x64v3-xanmod1), release, at commit `93c4a9d`. The harness pair, both modes.

| surface | uuid | mode | parents | measured | ns/parent | exclusive alloc |
|---|---|---|---:|---:|---:|---:|
| `arrival_walk` | `835d8c15` | hotpath | 6,000,000 | 214.7 ms | **35.8** | - |
| `arrival_walk` | `931336d5` | alloc | 6,000,000 | 216.0 ms | 36.0 | **3.8 KB** |
| `screen_projection` | `3f82ed37` | hotpath | 126,143,060 | 14.61 s | **115.8** | - |
| `screen_projection` | `3161fd34` | alloc | 126,143,060 | 14.66 s | 116.2 | **5.9 GB** |

`screen_projection` peak anon RSS 560 MB, avg cores 1.0 (`d3fa0b0a`).

**The draw is roughly a third of the screen's per-parent cost, and allocates
essentially nothing** - 3.8 KB total across 6M parents and 50.9M children. The
remaining two thirds, and all 5.9 GB, are the projection layer: about 47 bytes
allocated per parent, on a path whose per-child work is a `BTreeMap` traversal.

`close_reduced` is 3.03% of wall, so the session-CLOSE path is not where the
time is. This CORRECTS the round's entering hypothesis, which named `SessionAcc`
bookkeeping generally: the cost is the per-child and per-parent accumulation,
not the rotation.

What the accumulator is doing for the screen, stated because it is the shape of
the opportunity rather than a claim about a fix: `project_stream` drives the
full protocol-12a `SessionAcc`, calling `push_print(ts, 0)` per child and
`push_parent(index, ts, 0, 0, false)` per parent. With a literal zero price the
per-minute trade range it maintains is constant by construction, and with
`book_normal: false` the quote-mid arrays are never touched at all. What the
screen reads back is `block1.hist` marginalized to the parent-count axis, and
the same counts again for gate A1.

**Negative result, recorded so it is not re-derived.** The 5.9 GB was first
attributed to `walk.next()`: it is a `dyn ParentSource` call and nothing under
it is annotated, so its allocations land in `project_stream`'s exclusive bucket
and the arithmetic was consistent with the draw allocating ~30 bytes per parent.
`931336d5` refutes it outright at 3.8 KB. The generator's walk allocates
nothing; do not re-open that line.

## Predictive-envelope month price, 2026-08-11

Host `bygg`, release, one run per family through
`brokkr run --release envelope_evaluation_bench -- <family> <months>`
(registered as `[mogwai.targets.envelope_evaluation]`). The measured unit is
ONE replicate month of the protocol-12b section 9.7 envelope over the frozen
section 8 exposure, 2,674,800 grid cells. An evaluation is exactly
`500 * (1 + K)` months, so the unit multiplies out to every K tier; the work
count is exactly linear, the wall only estimated so, because fixed overhead,
host noise and stochastic draw counts all intrude.

| family | before | after | gain |
|---|---:|---:|---:|
| `shot_noise` | 0.4964 s | 0.4346 s | 12% |
| `event_markov` | 0.4128 s | 0.3725 s | 10% |
| `log_ou_cox` | 0.1258 s | 0.1016 s | 19% |
| `wall_mmpp` | 0.0880 s | 0.0796 s | 9% |
| `self_exciting` | 0.0838 s | 0.0730 s | 13% |

The optimization hoisted per-walk constants out of a 2.67-million-iteration
loop: `Poisson` and `Exp` objects whose parameters are fixed for the walk were
being CONSTRUCTED per grid cell, and `exp(-dt/tau)`, its square and the log-OU
spread were recomputed per cell. Every row is BIT-IDENTICAL - the harness work
sink reproduces its pre-optimization value exactly - so no envelope number,
gate verdict or conformance figure moved and nothing was re-blessed.

**The floor is the count draw.** `wall_mmpp` and `self_exciting` at 0.073 to
0.080 s are one Poisson draw plus loop overhead per grid cell. `event_markov`
at 0.372 s draws about sixteen gaps per cell and `shot_noise` at 0.435 s draws
ten jumps per cell, each jump costing an `Exp`, a uniform and a transcendental
decay factor. Both are the LAW at their probe cells - which are deliberately
the worst-cost corners, maximum jump rate and most state flips - and not an
implementation artifact. Reaching the fast families' price would mean drawing
fewer variates than the law specifies.

**A recorded pessimization.** Replacing the `event_markov` gap draw with
inverse CDF (`-ln(U) / rate`) measured 10% SLOWER than `rand_distr`'s ziggurat
on this toolchain and RNG: the ziggurat takes its rejection branch rarely
enough to beat an unconditional logarithm. It was measured, reverted, and only
the construction removal kept, reproducing `Exp`'s own reciprocal-multiply
arithmetic so the result stays bit-identical. This is a reading on one
toolchain, not a claim about every future one.

## Stage A batch instrument and Steps 1-4, 2026-08-10

Host `bygg`. Batch rows through Step 2 use release without profiling features
and one worker; Step 3 records its worker count explicitly, and the focused
rows name their instrumentation mode. The committed panel hash is
`81b5325fc18758c77b033b68ffe086a0f807b7d9b3d81321cb751d2609ae932d`.
The stored Step 0 and Step 1 rows and the Step 2 quick reading are the best
of three runs. The Step 2 full reading is one verification run.

| state | tier | uuid | wall | task CPU | maximum-cap p90 serial CPU |
|---|---|---|---:|---:|---:|
| Step 0 | quick | `fbd03346` | 87.100 s | 86.217 s | - |
| Step 1 | quick | `a0921513` | 85.101 s | 83.740 s | - |
| Step 0 | full | `66d4797d` | 790.200 s | 787.144 s | 27,448.707 s |
| Step 1 | full | `5c012131` | 780.201 s | 777.564 s | 27,121.840 s |
| Step 2 | quick | not stored | 70.100 s | 68.919 s | - |
| Step 2 | full | not stored | 620.200 s | 617.991 s | 21,218.767 s |

The full-panel wall improves 1.27%, task CPU 1.22%, and the maximum-cap p90
estimate 1.19%, or 326.9 seconds. Work is identical across the full rows:
72 cells, 242 seed walks, 7,571,686,367 parents and 8,868,542,328 prints.
The quick rows likewise retain exactly 8 cells, 24 seed walks, 752,083,142
parents and 880,798,950 prints.

**Step 1's entering 2x to 3x single-core hypothesis is false.** The full panel
has only 1.171 prints per parent. Collapsing each burst to its distinct
populated minutes therefore removes at most the roughly 14.6% of child-loop
iterations in excess of one per parent, while one session assignment and one
minute insertion generally remain. The implementation still earns a small
measured reduction and supplies the exact populated-minute primitive needed by
the lean accumulator, but it is not the structural CPU win originally priced.
Do not attribute any RSS movement to Step 1: retained parent timestamps and the
session accumulator are unchanged.

The full rows report 10 comparable strata, 44 singleton strata and
`design_based_interval_available=0`. They are a paired optimization
measurement, not a design-based confidence interval for the unsampled Stage A
population.

Step 2, commit `ddd5284`, replaces the Stage A use of the generic
`SessionAcc` with dense populated-minute, parent-minute and segment-second
state. A complete differential fixture pins its reduced JSON against the
generic accumulator, and the eight-seed layer-1 oracle remains exact. Its
batch readings above are pre-commit dirty-tree verification runs, so
brokkr correctly stored no `results.db` row. The values are retained here
without a fabricated UUID.

Against Step 1, the full-panel wall improves 20.51%, task CPU 20.52%, and
the maximum-cap p90 estimate 21.77%, or 5,903.1 seconds. Cumulatively from
Step 0, task CPU improves 21.49% and the maximum-cap p90 estimate 22.70%.
All batch work counters remain exactly 72 cells, 242 seed walks,
7,571,686,367 parents and 8,868,542,328 prints.

The committed focused rows are:

| state | uuid | mode | `project_stream` | accumulator-path allocation |
|---|---|---|---:|---:|
| Step 0 | `3f82ed37` | hotpath | 14.61 s | - |
| Step 0 | `3161fd34` | alloc | 14.66 s wall | about 7.1 GB |
| Step 2 | `c9927a73` | hotpath | 11.65 s | - |
| Step 2 | `0d874ef3` | alloc | 11.74 s | 3.4 GB |
| Step 4 | `29c19b0e` | alloc | 11.41 s | 3.0 GB |

The allocation comparison combines Step 0's 5.9 GB `project_stream` and
1.2 GB `close_reduced` frames because Step 2 folds closure into the new
screen-specific frame. The roughly 52% reduction removes the retained
parent timestamps and tree nodes needed before wide parallelism. One full
Step 2 run reported 486,887,424 peak RSS bytes against Step 1's
610,578,432; treat that as directional sizing evidence rather than a
repeated memory bound.

Step 3 lands the production `(cell, seed)` scheduler in commits `8564bc6`,
`c87fe2f` and `211d096`. Workers receive a cloneable immutable projection
context; the non-`Send` observed context, verdict reduction, budget checks and
deterministic result slots remain on the coordinator. Cache pruning runs once
before the worker pool, and prepared entries publish through unique staging
files and atomic renames. The batch harness calls the same scheduler as the
shipped `arrival-screen` command.

The quick panel's one-run scaling curve is below. Every row retains exactly 8
cells, 24 seed walks, 752,083,142 parents and 880,798,950 prints.

| jobs | uuid | external wall | measured wall | summed task time | effective concurrency | peak RSS |
|---:|---|---:|---:|---:|---:|---:|
| 1 | `de39aad9` | 69.200 s | 68.706 s | 69.077 s | 1.005 | 699,817,984 B |
| 2 | `edcf03a2` | 36.100 s | 35.220 s | 70.137 s | 1.991 | 696,905,728 B |
| 4 | `b3f89ff9` | 20.107 s | 19.082 s | 71.527 s | 3.748 | 819,929,088 B |
| 6 | `6b2a99d3` | 14.100 s | 13.087 s | 72.250 s | 5.521 | 844,824,576 B |
| 8 | `b5a1e13b` | 11.108 s | 10.225 s | 72.859 s | 7.126 | 1,033,072,640 B |
| 12 | `b43f10a2` | 8.100 s | 7.795 s | 73.350 s | 9.409 | 1,068,077,056 B |
| 16 | `4c87556a` | 6.100 s | 5.868 s | 74.027 s | 12.615 | 1,303,625,728 B |
| 24 | `f3d9809c` | 7.100 s | 6.257 s | 87.691 s | 14.014 | 1,219,899,392 B |

The knee on this 16-core, 32-thread Ryzen 9 9950X3D2 is 16 workers. Moving to
24 makes external wall 16.4% worse and summed task time 18.5% higher. The
shipped default is therefore machine parallelism capped at 16, with explicit
`--jobs` left as the host-specific override. Row `4a6d17ed` is deliberately
excluded: the first harness revision incorrectly capped its requested 12
workers at the quick panel's 8 cells instead of its 24 seed walks, and its own
captured `jobs=8` metadata exposes the mistake.

The final full-panel row is `0b861338`, one run at 16 workers:

| wall | measured wall | summed task time | effective concurrency | maximum-cap p90 scheduled wall | peak RSS |
|---:|---:|---:|---:|---:|---:|
| 44.200 s | 43.040 s | 676.239 s | 15.712 | 1,465.749 s | 1,765,822,464 B |

Against Step 2, full external wall improves 14.03x and measured wall 14.40x.
The maximum-cap p90 estimate moves from 21,218.767 seconds to 1,465.749
seconds, or 24 minutes 26 seconds, a 14.48x reduction. Summed task time rises
9.43% under contention, which is why wall scaling is measured rather than
inferred by dividing the one-worker row by a hardware-thread count. Exact full
work remains 72 cells, 242 seed walks, 7,571,686,367 parents and 8,868,542,328
prints. Peak RSS remains far below the 8 GiB ceiling.

Cumulatively from Step 0, full external wall improves 17.88x and the
maximum-cap p90 estimate improves 18.73x. Serial, one-worker and four-worker
real-projection verdicts are exactly equal after excluding timing fields; the
month-scale eight-seed layer-1 oracle also passes in both normal and
instrumented builds.

Step 4, commit `924c000`, removes generic session JSON and `ObsContext` from
the production verdict path. Generated and observed inputs meet at the same
typed sufficient-statistics boundary, and cached seed walks store that compact
projection. Against the Step 2 allocation row, exclusive `project_stream`
allocation falls 11.8% with identical 126,143,060-parent and
147,738,385-print work. Its instrumented time falls 2.8%; this single focused
reading supports the structural result but is not a general wall-time claim.
The legacy JSON path remains test-only as an exact differential oracle, and
the committed eight-seed layer-1 output oracle remains exact.

Brick A then ran from clean commit `2f512a6` with the shipped 16-worker
default. The frozen coarse grid evaluated all 787 cells in 241.105 seconds;
the complete command took 242.110 seconds and peaked at 547,155,968 bytes RSS.
It walked 48,987,759,956 parents and projected 57,368,361,183 prints. Summed
cell time was 3,835.378 seconds, about 15.9 effective workers. No coarse cell
was admissible, so the frozen refinement rule proposed no candidates and
`refine_s` was effectively zero. The resulting artifact verdict is
`no-arrival-admissible-candidate-in-frozen-search-space`. This is why the
actual run was about four minutes rather than the 24 minute 26 second
maximum-cap estimate: the estimate deliberately prices all available
refinement capacity. The post-artifact gate passed 805 normal and 387
instrumented tests.

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

### Finding 8 close reading, 2026-08-14

Host `bygg`, release, the uncommitted round-3 working tree after incremental
order reservations and keyed book lookup. One criterion run after the final
shape; every standard deviation is below the 5 percent usability limit.

| id | mean | std dev |
|---|---:|---:|
| `apply_scans_50` | 1.508 us | 3.34 % |
| `apply_scans_200` | 6.109 us | 3.90 % |
| `apply_scans_50_all_fill` | 29.84 us | 1.23 % |
| `apply_scans_200_all_fill` | 113.88 us | 1.31 % |

The common no-fill pair now scales `6.109 / 1.508 = 4.05` across a 4x book,
where the earlier reading was 7.72x. The all-fill pair is `113.88 / 29.84 =
3.82`, also linear. This is the structural verdict: scan-result lookup and
book removal no longer add a quadratic term. Do not read the absolute delta
against the 2026-08-02 table as a clean round-3 before/after comparison;
rounds 1 and 2 changed validation and booking work between those trees.

One rejected intermediate is recorded because it exposed the consumer risk.
The first indexed vector used shifting removal and rewrote every shifted index;
`apply_scans_200_all_fill` rose to 434 us. Replacing that with O(1)
`swap_remove`, while sorting all observable orderings explicitly, brought it
to the number above. The common path also stopped allocating a temporary
locked-balance map per funds query: it reads the cached order hold and folds
only the position-maintenance component.

What these four numbers do NOT cover, stated because the omission decided a
design question. `fill_bench`'s `scans` engine seeds no balances, so it is an
UNFUNDED engine and `enforce_funds` is false throughout the table above. The
round-3 working tree originally also ran a full reconciling fold of the book
in release whenever `enforce_funds` was set, repairing and logging on drift.
No benchmark here would ever have seen that fold, because none of them is
funded - and it is precisely the `O(open orders)` walk with a per-order
string and map allocation that finding 8 existed to remove, reinstated twice
per command and twice per sweep batch on the funded venue, which is the only
configuration that ships. The audit pass removed it: reconciliation is now a
`cfg!(debug_assertions)` assertion that panics, and release correctness rests
on construction (private book storage plus three cache-aware mutators)
instead. The table therefore stands as measured for both profiles.

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
read outside any loop. Positioning now refuses a target that the checkpoint
extension cap did not reach, before constructing the merge and entering its
unbounded source seek. So this number is close to the sweeper's true per-pass
fixed cost.

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
`SYNTHESIS_TICKS_PER_SEC` in `mogwai-server/src/serve.rs` exists to hold, and it
had been carrying 5 M - so the boot projection ran 1.7x optimistic, and the
60-second WARN threshold it gates was really firing at about 104 seconds of
actual cost. Corrected to 2.9 M.

Measuring the whole boot interval rather than tick synthesis alone is
deliberate: the constant predicts what an operator waits through, which includes
checkpoint retention and the frontier draw, not just the walk. Re-measure the
same way after any change to the generator, the checkpoint stride, or the tape
protocol - the previous value's stated provenance was a `fill_bench` row, and no
such row has ever existed in this document.

DO NOT COMPARE THIS NUMBER AGAINST A CORPUS-PARSING RATE. It is the rate at
which the generator MANUFACTURES a tick - GARCH recursion, RNG draws, checkpoint
retention - and manufacturing a tick is far heavier per item than splitting a
CSV line. Set beside the measured Python archive-parse ceiling (2.21 M rows/s
for a 128.7 M-row month, bytes-mode hot loop plus a process pool, 2026-08-05)
the two look like near-parity, which reads as an argument against moving corpus
parsing to Rust - the opposite of the truth. The honest comparator for that
question is a Rust byte-level parse, which nobody has measured.

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
then run `mogwai tick-composition-ratios compare` after any
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

## 2026-08-05 protocol 8 session-profile composition

The four constants above were measured against a tape that no longer exists for
MNQ and MES. Protocol 8 fitted MNQ's session profile from the NQ 1-minute
archive and made MES inherit it, taking the hour curve from crypto's flat 1.78x
peak-to-trough to 27.51x. Every budget above is denominated in ticks per
simulated second, per volatility window, or per wall second, so concentrating
the same parent count into the cash session raises all of them.

Protocol 8 cannot be projected from protocol 7. The profile divides the duration
draw and scales the return, so timestamps and prices both move and the two
fixtures come from SEPARATE traversals. `mogwai tick-composition-ratios compare --mode
independent` therefore gates before computing any ratio: `ticks_per_parent` must
be bit-identical for all five presets, because the profile changes when events
happen and never how many, and every measured field must be identical for
BTCUSDT, ETHUSDT and SOLUSDT, whose calendar-free normalizer is the literal 1.0
and whose tape is byte-identical across the change. Both gates passed.

The maximum independent p99.9 protocol-8/protocol-7 ratios were:

| budget denominator | ratio | prior constant | resulting constant |
|---|---:|---:|---:|
| simulated-second checkpoint work | 1.640000 | 1,048,576 | 4,194,304 |
| 300-second sweep work | 2.541999 | 282,000,000 | 1,434,000,000 |
| 24-hour cumulative warmup reach | 1.000619 | 81,124,000,000 | 162,349,000,000 |
| wall-second fanout work | 1.595696 | 262,144 | 1,048,576 |

The same policy applies unchanged: prior constant times ratio times two for
headroom, rounded up to a power of two for checkpoint and fanout and to the next
million for sweep and warmup reach, then taken as the larger of that and the
required reach. Which candidate wins moved. At protocol 7 required reach set
both the sweep budget and the warmup ceiling; at protocol 8 the headroom formula
wins for both, at 1,434,000,000 against a required reach of 289,063,683 frames
and 162,349,000,000 against 83,250,340,704. The worst measured p99.9 rate is now
963,545.61 frames per simulated second. Directly observed partial-window counts,
270,931,449 and 1,364,836,628, remain lower and set neither result.

The warmup ratio is the one that reads oddly. At 1.000619 it is the only one of
the four that FAILS the 1.05 materiality threshold, yet its ceiling doubles. The
reason is that the 24-hour reach is dominated by the maximum-surge arm, where
arrival compression already saturates the horizon and a session profile has
almost nothing left to concentrate. The doubling is entirely the standing
two-times headroom rule reapplied to a constant that protocol 7 had set at bare
required reach with none. The old 81,124,000,000 was in genuine danger: the new
required reach of 83,250,340,704 exceeds it, so the ceiling had to move
regardless of the ratio.

`MAX_EXTEND_TICKS` is unchanged at 1,073,741,824 ticks per index-lock
acquisition, and the 2,500,000-tick sweep latency warning is likewise unchanged.
Both are deliberately not scaled with the reach ceilings: the first is a
per-lock runaway backstop and the second an operational signal, and scaling
either with a refusal ceiling would silence it.

The sweep refusal ceiling is now roughly 8 minutes of blocking-worker time at
the measured 2.9 M ticks per second, up from 97 seconds. That is a ceiling
reached only by a far-from-market order under maximum surge, not a latency
anyone should see, and `SWEEP_DRAIN_WARN_TICKS` still fires three orders of
magnitude earlier. The resized fanout holds 0.114123 wall seconds at the worst
measured rate against the prior value's 0.029553, so no horizon shrinks. Note
this is still a fraction of a wall second under maximum surge; a surge-exposed
run should size `fanout_depth` deliberately rather than inherit the default.

Regenerate with
`brokkr run mogwai -- tick-composition --out analysis/tick-composition-protocol-8.json`,
then `mogwai tick-composition-ratios compare --mode independent`. The run
now costs about 90 minutes rather than an hour, and the increase IS the result.
Each mode carries its own baseline constant table; a shared one would resize the
current constants by the pre-protocol-7 baseline and under-propose checkpoint
and fanout by the factor protocol 7 had already absorbed.

## 2026-08-06 protocol 10 MNQ-fit composition

Protocol 10 landed the July 2026 MNQ TBBO fit: the two futures presets took
fitted generator scalars (mean event duration 0.0609 s against the
crypto-derived 0.171 s, near-single-child parents, the fitted quote seams), so
their cadence and fanout both move while the three crypto presets are untouched
by construction. The `independent_9_10` mode gates accordingly before any
ratio: every measured field must be byte-identical for BTCUSDT, ETHUSDT and
SOLUSDT, `parents` must match for all five, and every measurement entering a
ratio must be finite and positive on both sides. `ticks_per_parent` is NOT
frozen for the futures - the fit changes what a parent looks like, which the
7-to-8 session reshape could not. All gates passed; the protocol-9 side is the
Brick B0 fixture whose 8/9 byte-identity was separately verified.

The maximum independent p99.9 protocol-10/protocol-9 ratios were:

| budget denominator | ratio | prior constant | resulting constant |
|---|---:|---:|---:|
| simulated-second checkpoint work | 1.600000 | 4,194,304 | 16,777,216 |
| 300-second sweep work | 2.021814 | 1,434,000,000 | 5,799,000,000 |
| 24-hour cumulative warmup reach | 2.055137 | 162,349,000,000 | 667,299,000,000 |
| wall-second fanout work | 1.837312 | 1,048,576 | 4,194,304 |

The same standing policy: prior constant times ratio times two for headroom,
power-of-two rounding for checkpoint and fanout, next-million for sweep and
warmup reach, then the larger of that and required reach. The headroom formula
wins everywhere: required reach is 281,678,600 frames for a 300-second window
and 81,123,436,742 for the 24-hour warmup, both below their resized ceilings.
The worst measured p99.9 rate is 938,928.67 frames per simulated second;
directly observed partial-window counts, 258,678,966 and 1,364,836,628, set
neither result.

`MAX_EXTEND_TICKS` stays 1,073,741,824 per index-lock acquisition and the
2,500,000-tick sweep latency warning stays where it was, for the recorded
reasons: a per-lock runaway backstop and an operational signal must not scale
with refusal ceilings. The sweep refusal ceiling is now roughly 33 minutes of
blocking-worker time at the measured 2.9 M ticks per second - a ceiling only a
far-from-market order under maximum surge can reach, with the warning firing
three orders of magnitude earlier. The resized fanout holds 0.472841 wall
seconds at the worst measured rate against the prior 0.114123, so no horizon
shrinks; it remains a fraction of a wall second under maximum surge, and a
surge-exposed run should size `fanout_depth` deliberately rather than inherit
the default.

Regenerate with
`brokkr run mogwai -- tick-composition --out analysis/tick-composition-protocol-10.json`,
then `mogwai tick-composition-ratios compare --mode independent_9_10`.

## 2026-08-06 protocol 11 session-refit composition

Protocol 11 refit the two MNQ session arrays in the units the runtime
applies - arrival intensity from July MNQ inferred-parent counts,
per-parent volatility from quote-mid returns - and re-solved
`vol_scalar`; the fit artifact is `analysis/mnq-fit.json`. A session
reshape changes WHEN parents happen and how far returns reach, never how
many children a parent draws, so the `independent_10_11` mode gates
STRICTLY: `parents` and `ticks_per_parent` must be identical for every
pairing, the three crypto presets byte-identical, and every numeric leaf
finite and positive on both sides. All gates passed.

The maximum independent p99.9 protocol-11/protocol-10 ratios, the
standing policy's proposals, and what LANDED:

| budget denominator | ratio | prior | policy proposal | landed |
|---|---:|---:|---:|---:|
| simulated-second checkpoint work | 1.093750 | 16,777,216 | 67,108,864 | 67,108,864 |
| 300-second sweep work | 1.130315 | 5,799,000,000 | 13,110,000,000 | 13,110,000,000 |
| 24-hour cumulative warmup reach | 1.000360 | 667,299,000,000 | 1,335,079,000,000 | 1,335,079,000,000 |
| wall-second fanout work | 1.014216 | 4,194,304 | 16,777,216 | 4,194,304 |

Three proposals landed under the same standing policy as every prior
resize: prior times ratio times two for headroom, power-of-two rounding
for checkpoint, next-million for sweep and warmup, then the larger of
that and required reach (281,678,600 frames for a 300-second window and
81,123,436,742 for the 24-hour warmup, both far below the ceilings). The
worst measured p99.9 rate is 938,928.67 frames per simulated second,
unchanged from protocol 10: the refit moves density BETWEEN hours rather
than raising the peak, which is why the ratios sit barely above one
where the 9-to-10 cadence fit roughly doubled them.

The FANOUT proposal was REJECTED by joint review, the first recorded
policy exception. The reasoning, carried here in full: the resize
formula models costless refusal ceilings, but the fanout ring is eagerly
allocated state proportional to its depth, and compounding a fresh
two-times headroom onto an already headroom-sized allocation before
power-of-two rounding turns a 1.014x measured ratio into a 4x
allocation. The retained depth holds 0.466 wall seconds of ring at the
protocol-11 worst measured p99.9 frame rate against the 0.472 it held at
protocol 10 and the 0.114 the protocol-10 resize was justified over.
Decisively, the proposed 16,777,216 capacity DETERMINISTICALLY breaks
`a_banded_limit_fills_from_the_run_sweep` (5 of 5 failing against 5 of 5
passing at 4,194,304, with the other three resizes present in both
trees, on the default BTCUSDT venue at speed 100): an accept-before-fill
invariant failure whose mechanism is UNRESOLVED - the assertion cannot
yet distinguish wire reordering (the fill frame arriving before
`OrderAccepted`) from timestamp inversion, and ring depth is not
consulted after construction at nonzero speed, so the suspected channel
is the eager allocation shifting boot phase relative to the anchored run
clock. The investigation item in `notes/todo.md` carries the exact
reproduction; `the_fanout_default_carries_the_protocol_11_exception`
pins the default so a later mechanical application of the generated
proposal must be argued, not slipped through.

`MAX_EXTEND_TICKS` and `SWEEP_DRAIN_WARN_TICKS` stay unchanged for the
recorded standing reasons: a per-lock runaway backstop and an
operational signal must not scale with refusal ceilings.

Regenerate with
`brokkr run mogwai -- tick-composition --out analysis/tick-composition-protocol-11.json`,
then `mogwai tick-composition-ratios compare --mode independent_10_11`.

## 2026-08-14 checkpoint restore stride repair

Host `bygg`, release, current working tree. The `source_positioning` criterion
case was reshaped to a one-hour target so both compared strides reach an
interior checkpoint. Its former one-second target preceded the first checkpoint
and therefore could not price checkpoint spacing.

| checkpoint stride | mean | std dev / mean |
|---:|---:|---:|
| 67,108,864 | 115.85 ms | 0.1% |
| 8,192 | 2.184 ms | 0.1% |

The smaller stride cuts the measured steady-state positioning cost by 53.0x on
the identical target. This changes only snapshot frequency and residual replay.
Generated ticks, draws, seeds, and tape origin are unchanged, so no
`TAPE_PROTOCOL_VERSION` bump or artifact re-bless is owed.

### What that buys the submit path, which is less than the ratio suggests

Same host and tree, from the ignored latency instrument
`read_market_latency_stays_within_submit_budget` in `fills.rs`, run through
`brokkr test -p mogwai-server read_market_latency_stays_within_submit_budget`.
100 reads, each in its own memo bucket so none is served from the entry the
previous one left:

The "previously recorded" column is the number `architecture.md` and the
instrument's own comment carried, not a re-measurement on this host and tree, so
read the pair as a level check rather than a controlled delta.

| path | previously recorded | this run |
|---|---:|---:|
| miss, median | 12.6 ms | 9.782 ms |
| miss, p99 | not recorded | 9.987 ms |
| hit | ~0.13 ms | 0.096 ms |

The memory side of the same tradeoff, stated because the stride buys latency
with it: the committed default config warms 4,288,935 ticks, so the boot log's
retained-checkpoint count goes from tens to roughly 520 generator clones.
`MAX_CHECKPOINTS` of 4096 is still the hard ceiling, and coarsening only starts
past about 34M ticks - eight days of sim time at that cadence - so an ordinary
run keeps the base stride.

Positioning was never the whole of a market reading: the 300 s `VOL_WINDOW_NS`
walk is, and it is untouched by checkpoint spacing. So a 53x positioning win
shows up here as roughly 3 ms. Re-scoping the reading window remains the lever
that would move this number materially, and remains unapplied because it moves
the estimator's identity and re-blesses the fill golden.
