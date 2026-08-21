# Performance record

Measured numbers across every operational surface, plus how to obtain and read
them. Every later change that moves a number APPENDS a row; rows are never
edited in place, so this file is a history and not a snapshot.

Numbers measured through `brokkr mogwai` carry their result UUID, so any claim
here can be re-derived - `brokkr results <uuid>` and `brokkr sidecar <uuid>`.
Numbers from the criterion harnesses do not, and are pinned by commit instead.

## The arrival kernel's cost cliff, 2026-08-19

Priced to answer one question - whether an `ArrivalKernel::next_parent`
jump-ahead rewrite is worth the owner's chart gate - and recorded here because
the answer turns on the numbers rather than on the mechanism. THE ANSWER IS NO.
The measured record, the reachability analysis and the recommendation follow.

NOT A BENCHED ROW, so it carries no UUID. A throwaway integration probe over
`CadenceWalk::next` - the shipped kernel draw with nothing attached - at
fingerprint-median BTCUSDT scalars, a flat session profile and
`mean_event_duration_s` 0.171, on the owner's host. Dev and release agreed to
within noise on every row, which is the reading: the traversal is bound by its
per-cell RNG draw, not by anything the optimizer reaches.

| case | per parent draw |
|---|---|
| healthy walk, all four families, `thin` 1 | 45 to 50 ns |
| `LiquidityDrought` at its `thin_factor` ceiling of 1000 | about 3 us |
| `LogOuCox` `sigma_y` 8, `thin` 1000 - succeeds, so it repeats | 3.6 ms mean, 115 ms peak |
| the full 31.6M-cell traversal to `MAX_SESSION_GAP_NS` | 460 to 660 ms, then a terminal refusal |
| the one draw spanning a 49-hour weekend, 176,400 cells | 650 us to 1.19 ms |
| three weeks of calendar walking, 7.4M parent draws | about 500 ms total |

The weekend crossing is the row worth remembering, because it was the half of
the report expected to matter most: 176,400 one-second cells of latent evolution,
paid once a week under the river mutex, for about a millisecond. Real mechanism,
rounding-error magnitude.

The 31.6M cliff is real and reachable, but only from a config no preset ships -
no shipped preset declares an arrival family at all, so `ArrivalConfig::kernel`
is never called on any default serving path, and the reachable population is
operator `generator.arrival` overrides plus the lab's own screen. It also
TERMINATES rather than recurring: the traversal that reaches
`MAX_SESSION_GAP_NS` refuses with `NoOpenExposure`, the fault latches, and the
source is done. `AGENTS.md`'s "a multi-hour computation is presumptively a
defect" does not apply to two-thirds of a second that cannot repeat.

`LiquidityDrought` is not the cause; capped at `thin_factor` 1000 it buys 3
microseconds. What reaches the cliff is two knobs with no upper bound -
`LogOuCox`'s `sigma_y`, whose `x = exp(y - sigma^2 / 2)` latent is unbounded
BELOW, and `GeneratorScalars::mean_event_duration_s` - and both are validator
gaps rather than kernel defects.

THE RECOMMENDATION IS TO BOUND THOSE TWO KNOBS AND LEAVE THE KERNEL ALONE. The
per-cell RNG draw inside `advance_state_to` IS the tape: skipping it, batching
it, or substituting the closed-form n-step transition changes how many values
come off the `ChaCha12Rng` and therefore every later draw, so no byte-preserving
O(1) jump-ahead exists and any rewrite spends the chart gate. Upper bounds cost
nothing by comparison - they are ADMISSION changes that move no byte of any tape
a bounded config produces - but refusing a config that works today is a product
decision, so they are filed rather than landed. The number to beat if the
rewrite is ever revisited is 45 ns per parent on a healthy walk. The kernel is
not slow; it is unbounded at one end of a parameter range nothing validates.

### Placing the two bounds, 2026-08-20

The owner ruled the bounds in, which left a number to choose. Three points -
`sigma_y` 1, 8 and 12 - cannot place a ceiling, so the region between them was
measured by `mogwai-data`'s `examples/arrival_sigma_sweep`, 2000 to 4000 draws
per point at the `LiquidityDrought` thinning ceiling, release, on host `bygg`.
Two runs at different draw counts agreed row for row. The example is committed
because the question recurs whenever either knob is revisited.

THE MEDIAN DRAW IS 50 TO 70 NS AT EVERY `sigma_y` SETTING. All of the cost is
in the tail, so the mean and the max are the readings:

| `sigma_y` | mean | max |
|---|---|---|
| 1.0 to 5.5 | 70 to 190 ns | under 70 us |
| 6.0 | 1.7 us | 256 us |
| 6.5 | 3.8 us | 369 us |
| 7.0 | 6.2 us | 1.31 ms |
| 7.5 | 25 us | 2.38 ms |
| 8.0 | 24 us | 10.6 ms |
| 9.0 | 336 us | 40.7 ms |

SIX IS A KNEE and the ceiling is placed there: the mean is flat at healthy-walk
cost through 5.5 and departs by an order of magnitude at 6.0, while the max
crosses a millisecond at 7.0. The bound admits the knee and excludes the whole
millisecond region with a point of margin.

`mean_event_duration_s` HAS NO KNEE. Its cost is LINEAR in the knob, measured
against a fixed healthy `LogOuCox`:

| `mean_event_duration_s` | mean | max | outcome |
|---|---|---|---|
| 0.171, the fitted median | 78 ns | 590 ns | ok |
| 10 | 1.2 us | 12.3 us | ok |
| 100 | 11.8 us | 95 us | ok |
| 1e3 | 123 us | 975 us | ok |
| 1e4 | 1.18 ms | 13.1 ms | ok |
| 1e5 | 12.0 ms | 88.4 ms | ok |
| 1e6 | 122 ms | 638 ms | refuses `NoOpenExposure` |

So its ceiling is a statement about acceptable per-draw cost rather than a
boundary the data picks out, and 1e3 is chosen as the last decade whose mean
stays in microseconds - some 6000 times the fitted median, and generous past any
duration a market event has. Both bounds landed with the constants carrying
their own tables, so the next reader does not have to find this section to know
why the numbers are what they are.

## The `mogwai-data` test binary's wall, 2026-08-19

TWO DIFFERENT LANES ARE MEASURED HERE AND THEY DO NOT PAIR. Read the command
attached to each number; an earlier draft of this entry attributed both sweep
walls to one invocation that cannot produce them, and the correction is the
reason the split is spelled out.

THE FOCUSED RUNNER, `brokkr test -p mogwai-data "" --debug` on host `bygg`.
It runs the crate's suite once per sweep and there are THREE sweeps -
`workspace`, `instrumented` and `timing` - not two, and it does NOT apply the
gate profile's `skip` list, which is why both dwell twins ran in the baseline.
Serial, one run each side, re-measured on the round's own tree:

| sweep | before | after |
|---|---|---|
| `workspace` (dev) | 44.62 s | 37.97 s |
| `instrumented` (dev, `hotpath-alloc`) | 44.61 s | 9.96 s |
| `timing` (release) | 44.43 s | 38.22 s |
| total | **133.66 s** | **86.15 s** |

The three deltas decompose exactly. `workspace` and `timing` each lose only the
deleted twin, about 6.5 s. `instrumented` loses the twin AND the five walks
that are now absent from that build shape, 6.5 + 28.4 s. `timing` does not
enable `hotpath`, so the `cfg` leaves it alone by construction.

THE GATE, `brokkr check --gate`, full and unscoped: **58.3 s -> 41.4 s and
50.4 s**, two runs on the same host after the change. Both are quoted because
one is not a number: the gate runs eight threads against a loaded desktop and
its wall is the noisiest figure in this file, so treat it as "faster, by
something between 8 and 17 seconds" and re-derive it before quoting it. The
coverage counts are NOT noisy and are the ones to check: 1191 + 436 = 1627
run across the two sweeps, 61 ignored, so 1688 pairs, 0 orphaned, 16 skips.
The two summands are the SWEEPS, and the pair count is run plus ignored - an
earlier draft wrote the sum as the pair count, which is the one arithmetic in
this entry a reader would otherwise carry forward wrong. The gate runs
`workspace` and `instrumented` only, and it DOES apply `skip`, so its
arithmetic is not the table's. IN THE GATE'S `workspace` SWEEP THE TWO DWELL
CHANGES CANCEL - the twin's deletion is about -6.5 s and un-skipping
`dwell_is_bounded_across_run_seeds` is about +6.5 s - and all five `cfg`'d
walks still run there. THE ENTIRE GATE SAVING COMES FROM THE `instrumented`
SWEEP. The dwell change bought coverage, not wall clock, on the default lane;
only the focused runner sees it as time.

THE DISTRIBUTION CAME FIRST, through the same instrument the adapter round
built - `scripts/adapter_test_walls.py`, which is generic over libtest
binaries. It showed the OPPOSITE shape to the adapter's: not a floor but seven
genuine walks carrying 93% of the wall, with a tail of 168 tests at a
millisecond apiece.

| test | dev wall, one per process |
|---|---|
| `session_modulation_reproduces_curves` | 7.51 s |
| `dwell_is_bounded_across_run_seeds` | 6.54 s |
| `run_seeded_tape_dwell_is_bounded` | 6.53 s |
| `synthetic_spread_decomposition_at_protocol_seven` | 6.46 s |
| `realism` | 6.35 s |
| `session_edge_spike_lifts_realized_clamp` | 5.28 s |
| `session_edge_spike_localizes` | 2.72 s |
| the other 168 | 3.29 s combined |

TWO CUTS, and neither weakened a gate.

THE BINARY RAN TWICE FOR NOTHING. The `instrumented` sweep builds this crate
with `hotpath-alloc` so that a feature nothing compiles cannot rot; that is a
COMPILE-time property, and `crates/mogwai-data/src` carries no `hotpath`
annotation at all - the crate's only one is in `examples/arrival_walk_bench.rs`.
So the second sweep re-executed every million-tick walk to learn nothing: the
same test measures 7.65 s in the default shape and 7.67 s in the instrumented
one. The five walks over ~2 s now carry `#[cfg(not(feature = "hotpath"))]`, so
they are absent from that build shape rather than filtered out of the run -
which matters, because the gate certifies complete coverage and a filtered test
is an ORPHANED pair while a test that does not exist in a shape is no pair at
all. The audit agrees: 1688 pairs, 0 orphaned. Instrumented 44.61 -> 9.96 s,
and that sweep is where all of the gate's saving lives.

THE ONLY MULTI-SEED DWELL GATE WAS THE ONE NOBODY RAN. It was `#[ignore]`d and
in the runner's skip list on the claim that it "outlives the 20-second per-test
hang watchdog by design"; measured, it is 6.54 s, and its eight arms are
`DRAW / 8` apiece - the same two million parent events in total as `realism`.
Meanwhile a single-seed twin at the full draw ran on every lane for 6.53 s. The
twin is deleted, seed 42 (the default run seed, so the shipped realization)
moved into the loop, and the eight-arm version now runs everywhere. The arms
are 0, 1, 2, 3, 4, 5, 6 and 42: seed 7 is displaced by 42 rather than joined by
it, because eight arms at `DRAW / 8` is what keeps the total at two million
parent events. Eight realizations for the wall clock of one - and on the gate's
default sweep that is exactly what it cost, nothing, since the deletion and the
un-skipping cancel there.

WHAT THE SHORTER PER-SEED DRAW COSTS, measured against each bound rather than
argued: `mean_gap_s` 0.1743-0.1785 short against 0.17426 full, declared
0.17104 with a 10% window, so the short arms sit further from the declared mean
and the band did not soften; `gap_p999_s` 2.92-3.03 short against 3.18 full,
bound 10.65, so a tail defect must reach 3.55x rather than 3.35x - a 6% loss on
one of four assertions, bought with seven extra seeds; `empty_hour_frac` and
`max_empty_hour_run_h` are exactly 0 at both draws, being one-sided guards
against silence that a 0.17 s mean gap never approaches. The silence guard was
bite-checked at the short draw by injecting a 30,000x `LiquidityDrought`:
`empty_hour_frac` fails at 0.765 against 0.0105. State that guard honestly: at
~11 complete hour buckets its resolution is 1/11 = 0.09 against a 0.0105 bound,
so it is a BINARY "no empty hour at all" and not a measurement of a fraction.
That was already true at the full draw, where ~96 buckets give 0.0104 against
the same 0.0105, so the short draw gives up nothing here - but the two-sided
reading the assertion's form suggests was never available.

REFUSED: cutting `SESSION_DRAW`. Its 15M parent events are about 30 simulated
days and the seven `dow_weight` assertions need whole weeks to separate a
weekend from a weekday, so halving it does not fail sooner, it passes on less
evidence. The reported watchdog risk did not reproduce either: 7.51 s serial,
and the runner config's own record has the same walk at 7.622 s serial against
7.768 s at eight threads, so the 20 s per-test kill sits 2.6x away rather than
the 1.6x-inflation estimate's 1.7x.

## The two GARCH instruments the skip list was hiding, 2026-08-19

The same shape as the dwell entry above, found by finishing the audit that one
started. `standardized_candidate_rail_sizing` and
`realized_return_envelope_under_regime_scaling` were `#[ignore]`d and in the
gate profile's `skip` list under the heading claiming every entry there outlives
the 20-second per-test hang watchdog. Measured one test per process in dev on
host `bygg`: **0.43 s** and **0.20 s**, three sweeps agreeing to within 0.15 s.

A review pass argued that 0.43 s reads like a RELEASE number and that dev would
land at 8 to 30 s, straddling the watchdog. IT DOES NOT. Re-measured with
`--debug`, which is the profile the argument was about, the rail sizing runs
**0.45 s to 0.56 s** across the three sweeps, and the whole `garch` filter -
both instruments plus the second-moment harness - runs **0.63 s to 0.77 s**.
Record the spread rather than a single figure: this crate's gate wall is noisy
and a lone number invites exactly that objection.

WHAT THE WRONG CLAIM COST, and it is the dwell lesson verbatim: those two are
the ONLY measurements behind `GARCH_SIGMA_CAP`, `FEEDBACK_RETURN_CEILING` and
`REALIZED_RETURN_CEILING`. The numbers `consts.rs` cites in prose - a sigma
reaching 57.2x its unconditional scale, a largest clean return of 3.33e-3, an
unclipped return RMS of 1.2393e-5 over 16M updates, a clean realized maximum of
0.82 percent at `vol_mult` 1 - all come from tests nothing had run since they
were written. Both are un-ignored, out of `skip`, and read
`GARCH_ARCH`/`GARCH_GARCH`/`VOL_SCALAR` rather than a frozen candidate triple
that happened to equal them. Every figure above re-measured unchanged on the
round's tree.

THE GATE, `brokkr check --gate`, on the round's tree: **49.0 s** on the fix
pass and **52.4 s** after the review repairs, which changed no test count.
Against the 41.4 s / 50.4 s pair recorded above this is inside the noise that
entry warns about; the counts are the readable part. 1195 + 440 = 1635 run,
57 ignored, so 1692 pairs, 0 orphaned, 14 skips. The deltas from
1191 + 436 = 1627 run / 61 ignored / 1688 pairs / 16 skips decompose exactly: two new `mogwai-data` tests in
`tests/tape_version_prose.rs` are +2 in each sweep and +4 pairs, and the two
un-ignored instruments are +2 run in each sweep, -4 ignored and -2 skips while
adding no pair, having already been pairs.

## The adapter socket suites' wall, 2026-08-19

`brokkr test -p mogwai-adapter "" --debug`, the serial sweep of the four
socket-backed test binaries, on host `bygg`: **39.71 s -> 12.14 s**, no test
removed and two added. For four rounds roughly 37 s of that was recorded as
UNEXPLAINED and was the crate's largest single cost; this is where it went.

COUNTS, because two figures below are ratios and a stale denominator makes them
unreadable. The four binaries held 58 tests when the work started and hold 60
now: `adapter_smoke::both_legs_disclose_one_process_session_on_the_upgrade` and
`data_client_transport::an_undecodable_clock_is_retried_then_falls_back_without
_refusing` were added, in that order. The distribution quoted next was taken
between those two additions, so its denominator is 59.

THE INSTRUMENT CAME FIRST, because nobody had a per-test distribution and every
proposal was therefore a guess. libtest's `--report-time` is nightly-only, so
`scripts/adapter_test_walls.py` runs the already-built test binaries directly,
one test per process, and times each. The shape it showed was not a few
outliers but a FLOOR: 55 of those 59 sat in a 419-892 ms band with a hard
~420 ms bottom, while the only two that never call `connect()` came in at 15 ms
and 23 ms.

A floor that flat is a fixed cost inside `connect()`, and it was one. The test
stub answered `GET /clock` with the catch-all `[]`, which the client cannot
decode, so every connecting test walked `fetch_clock_or_identity`'s full ladder
- three attempts with a 200 ms wall sleep between them - before falling back to
the identity clock. 57 of the 59 connect, at ~400 ms each: about 24 s.

The fix is in the harness, not the client. The stub now serves a real identity
envelope (`common::IDENTITY_CLOCK_JSON`, speed 1, zero floor), which is
behaviourally identical downstream - `ensure_on_tape` receives `Some(0)`
instead of `None` and no start can precede zero - and is a more honest fixture,
since the real venue answers that route. 39.71 -> 15.93 s. The retry ladder
kept its coverage deliberately, moving from 57 accidental traversals asserting
nothing to one test in `data_client_transport`,
`an_undecodable_clock_is_retried_then_falls_back_without_refusing`, which arms
the new `fail_clock` switch and counts the attempts.

With the floor gone the distribution became legible and named a real outlier:
`havoc::a_venue_serving_another_run_is_refused_terminally` at 5.0 s, which is
`wait_connected`'s five-second readiness bound spent in full because readiness
never arrives on a terminal refusal. That is the standing shape - A BOUND ON A
FUTURE THAT CANNOT SUCCEED IS ON THE PASSING PATH, NOT THE FAILING ONE - and
the same repair applied there as to `conn_reconnect_respects_max_attempts` a
round earlier: bound the connect at 500 ms, then poll the observable. 15.93 ->
11.83 s.

The final distribution over all 60, one test per process, totals 11.38 s and has
a long flat tail: only four exceed half a second, and each is deliberate - the
close-and-reconnect replay pin at 1.42 s, the reconnect-attempt ladder at
1.31 s, the unsupported-init table at 0.78 s, and the identity refusal at
0.76 s. The retry-ladder test is the one remaining ~420 ms entry, which is the
cost the other 57 used to pay each.

## Session-segment cut and compose, 2026-08-18

The session-segment sampler's two offline surfaces, measured on host `bygg`
against the delivered MNQ 2026-04 TBBO month. Not through `brokkr mogwai`, and
the reason is worth recording: that tool resolves the first token after `--` as
a HARNESS TARGET unless it is one of the CLI subcommands it already knows, and
`segments` is new, so an argv-shaped bench of it is refused with
`unknown target "segments"` despite the documented rule that CLI surfaces need
no registration. The commands therefore emit their own elapsed as stderr
counters - `cut_seconds`, `library_load_seconds`, `compose_seconds` - beside
the work-size counters they already carried, which makes the surface
self-measuring rather than dependent on that resolution being fixed.

Worst case of the four windows, `ny-afternoon`, 9,572,450 ticks over 21
sessions: `cut_seconds=45.9`. Composing 4,000,000 ticks from the resulting
165 MB library: `library_load_seconds=0.4`, `compose_seconds=0.7`.

WHAT THIS DECIDES, and it was measured to decide it: whether cutting the full
eleven-month corpus needs optimizing before it is run, per the standing rule
that a multi-hour computation is presumptively a defect rather than a budget.
It does not. Eleven months across four windows is on the order of 44 cuts at
roughly three quarters of a minute each, so about half an hour - a coffee
break, not an overnight job, and not worth optimizing ahead of a need.

THE LEVER IF IT EVER MATTERS, recorded so it need not be rediscovered: cut cost
is dominated by streaming the month's TBBO, which is the same work whichever
window is being cut, so cutting four windows re-reads the same month four
times. One pass emitting all four libraries would be close to four times
cheaper. The window bounds are already resolved per trade date up front and the
stream is classified against them as it flows, so the change is to carry a
table of windows rather than one - not a restructuring.

Library size is the number more likely to bite than time: 408 MB of JSON for
one month across four windows, so an eleven-month corpus is roughly 4.5 GB and
a single-window eleven-month library about 1.8 GB, parsed whole into memory to
compose. The load stays linear and the 0.4 s above scales to a few seconds at
that size, which is tolerable; what would not be is holding several such
libraries at once. A compact binary encoding is the answer if that arrives, and
nothing about the format's contract prevents one - the parallel arrays are
already the storage shape.

## Per-boat ring sizing, 2026-08-15

Piece 9 reduced the shipped per-boat `fanout_depth` to 1,048,576, the smallest
power of two already measured to hold the protocol-8 worst p99.9 wall-second
frame work. On host `bygg`, `brokkr mogwai ring_sizing --alloc 3 --force`
reported `ring_resident_bytes=42213376` for one eagerly allocated Tokio
broadcast ring. It was measured from the dirty tree that preceded the boatyard
landing, so brokkr correctly stored no durable results row; the benchmark's
stderr counter is the measurement record for this landing. The depth itself is
`mogwai_server::config::DEFAULT_FANOUT_DEPTH`, public precisely so the
`ring_sizing` harness measures the shipped value rather than a copy of it, and
the ring is allocated PER BOAT - a run serving several rivers at once pays it
once per placed boat.

## History admission gate, 2026-08-15

The venue now admits at most four concurrent whole-page history syntheses,
bounding worst-case response construction and preventing history from filling
Tokio's blocking pool ahead of command market readings. The release-mode
`history_admission_overhead` instrument ran one million uncontended
acquire/drop pairs five times on host `bygg`: 23, 23, 23, 22 and 22 ns per
admission. This measures the added gate itself, not endpoint latency; history
synthesis and JSON response construction dominate it.

The bound the gate multiplies is MEASURED, by the release-mode
`worst_case_history_page_bytes` instrument, at a full `MAX_HISTORY_LIMIT` page
of MNQ-shaped ticks: `/quotes` is 4.40 MB of `QuoteTick` vector and 5.90 MB of
serialized JSON, `/trades` 3.20 MB and 5.05 MB. The vector and its bytes are
resident together while serde runs, so an admitted quote page peaks near 10.3
MB and four of them near 41 MB. That is the whole ceiling only because the
admission permit is carried by the response body rather than by the handler:
axum serializes a returned `Json` value after the handler future resolves, so
a handler-scoped permit would readmit four more requests while four
multi-megabyte responses were still being built. History serializes on its own
blocking task and hands the finished bytes and the permit to `HistoryPage`
together.

AMENDED 2026-08-18: the gate WAITS instead of refusing. The measurements above
stand unchanged - the concurrency, the per-page bytes and the ~41 MB ceiling are
all properties of what is RESIDENT, and a caller waiting for a slot holds no
page - but a fifth request now blocks for up to `HISTORY_ADMISSION_WAIT` rather
than taking a `503`. The reason is consumer-side and not a performance one: a
refusal reaches a nautilus host as an EMPTY window, because its historical
response types carry no error channel, so a refused warmup was indistinguishable
from a quiet tape. The gate's own overhead figure above is the uncontended path
and is unaffected; what a contended caller now pays is queueing latency bounded
by the deadline, and nothing has measured that under a real mass-attach boot
storm because nobody has run one. `MAX_QUEUED_HISTORY_REQUESTS` is the
fail-fast bound that keeps the queue itself from being unbounded, and it is
chosen rather than measured for the same reason.

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
lookup. The registry that carries the targets and their feature shapes, and the
reasoning for what it deliberately does NOT carry, is `brokkr.toml`.

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
- **Tape lateness is a MEASUREMENT, not a gate, and that is a ruling about what
  a wall-clock threshold can mean.** It was `tape_lateness_under_acceleration`,
  a test asserting a 50 ms p99 pacing bound, and it failed at 311 ms p99 on
  2026-08-08 with a load average of 1.46 across 32 visible CPUs - so load
  average alone is not a sufficient admission test, and no admission test yet
  distinguishes a host that can judge 50 ms from one that cannot. A threshold
  nobody can evaluate had to be excluded from the debug lane for being a latency
  budget and from the release lane for being unjudgeable under load, leaving a
  gate that no lane ran. It is now the `tape_lateness` target
  (`brokkr mogwai tape_lateness`, argv `[config] [sample_seconds]`), which
  records `frames`, `non_trade_text`, `control_frames`, `ending`,
  `p50_lateness_ns`, `p99_lateness_ns` and `max_lateness_ns`
  against the machine and the commit that produced them. A series of readings
  shows a regression; a portable threshold never existed to show one.
  - Baseline, host `bygg`, `brokkr mogwai tape_lateness -- "" 3` at `62c2501`,
    three consecutive 3 s samples on `accelerated.toml`, all
    `ending=sample_complete` and all `control_frames=0`:

    | frames | p50 | p99 | max |
    |---|---|---|---|
    | 15,718 | 0.407 ms | 9.36 ms | 42.8 ms |
    | 15,433 | 0.420 ms | 12.4 ms | 43.2 ms |
    | 16,987 | 0.301 ms | 28.3 ms | 41.5 ms |

    THE TAIL IS THE UNSTABLE PART AND THE BODY IS NOT. p50 holds inside a third
    of a millisecond across all three while p99 moves threefold, and the MAX is
    the steadiest figure of the four - about 42 ms every time, which is what the
    retired 50 ms p99 bound was really sitting against. A threshold placed on
    p99 was therefore being judged by the quantile with the widest spread here.
    Read one reading as a sample of a distribution, never as this host's number.
  - `ending` IS PART OF THE READING, not decoration. Only `sample_complete`
    means `frames` covers the whole `sample_ms`; a stream that ended or faulted
    early leaves a PREFIX, and two frame counts are not comparable unless both
    loops ended the same way. THAT IS NOT HYPOTHETICAL: an earlier reading
    recorded here - 11,893 frames, p99 42.9 ms - was taken with a draft loop
    that stopped on the first non-text frame, and its "p99" landed on the max
    because the truncated sample had too few points to separate them. It has been
    dropped rather than annotated, since the number the shipped instrument
    reports for that quantile is a third of it. `control_frames` is 0 in every
    sample above, so the truncation was latent rather than active - which is
    exactly why it survived to be recorded as a fact.
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
  here rather than a footnote: a venue is one process per run, hosts carry many
  of them at once, and every instance-level cost is multiplied by that count.

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

## The eleven envelope gate parts' wall, 2026-08-19

Host `bygg`, release, ONE PART PER INVOCATION - which is not a preference but
the only form that runs. `brokkr test --timeout` is honored only when the
filter matches exactly one test and errors out before running when it matches
more, so the ten fidelity parts have to be named individually by appending
their family and step suffix to the frozen prefix; `brokkr.toml` carries the
suffix list. Every part is `#[ignore]`d at the source and skipped by the gate
profile, so none of these seconds is ever paid by `brokkr check --gate`; this
is what the suite costs when it is run deliberately.

| part | wall |
|---|---:|
| `..._matches_the_closed_forms_where_they_are_exact` | 168.2 s |
| `..._faithful_to_the_candidate_walks_event_markov_250ms` | 142.5 s |
| `..._faithful_to_the_candidate_walks_event_markov_1s` | 138.2 s |
| `..._faithful_to_the_candidate_walks_shot_noise_250ms` | 106.2 s |
| `..._faithful_to_the_candidate_walks_shot_noise_1s` | 78.0 s |
| `..._faithful_to_the_candidate_walks_log_ou_cox_250ms` | 77.5 s |
| `..._faithful_to_the_candidate_walks_self_exciting_250ms` | 75.7 s |
| `..._faithful_to_the_candidate_walks_wall_mmpp_250ms` | 70.2 s |
| `..._faithful_to_the_candidate_walks_log_ou_cox_1s` | 58.5 s |
| `..._faithful_to_the_candidate_walks_self_exciting_1s` | 56.0 s |
| `..._faithful_to_the_candidate_walks_wall_mmpp_1s` | 54.2 s |

About sixteen minutes for the whole suite, every part inside the runner's 280 s
ceiling - which is the number that actually binds these, not the 900 s
`CONFORMANCE_BUDGET_S` the spec names. The conformance gate sits 5.3x inside
that budget, which is why its wall is now REPORTED
(`conformance_wall_s=... budget_s=900`) rather than asserted: an assertion with
that much headroom catches no cost regression and can only fire on host load.

THE GRID STEP IS NOT THE COST DRIVER FOR FAMILY 1. `event_markov` runs real
`advance_parent` walks rather than a grid sweep, so its two steps measure 138 s
and 142 s; every other family's 250 ms part costs 1.3x to 1.4x its 1 s twin
rather than the 4x the cell count would suggest, because the per-cell draw
dominates the cell count only for the jump-heavy laws. The per-replicate-month
unit prices behind all of this are the `envelope_evaluation` table above.

The degeneracy-floor repair to the dispersion arm, landed the same day, moves
none of this: re-runs of the two parts carrying degenerate rows came back at
79.5 s for `shot_noise_1s` and 140.2 s for `event_markov_1s`, inside the
run-to-run spread of the table. Both arms and both self-proof probes are
arithmetic over the 32 f64 values a part has already collected, so they cannot
show up against walks measured in minutes.

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
- `scan_mapping_50` - the `PendingScan` to `TriggerScan` mapping the venue
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

It is built from `mogwai-data` and so omits one ingredient of the venue's
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
not hide the dominant venue cost: a cache miss still pays the previously
measured 13.86 ms tape walk per symbol. That landing's memo was a single
run-level entry shared between the command path and the sweeper, so a
multi-symbol pass could evict itself; 2026-08-16 moved the memo ONTO THE BOAT,
one single-entry cache per river keyed by the boat's own sweep-interval bucket,
which removes the cross-symbol eviction. The sweeper no longer reads that memo
at all - its two reads are exact-instant last-print reads - so the only consumer
today is the command path. The tape walk a miss pays is unchanged and remains
the open cost; see the 2026-08-14 close reading below for the current levels.

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

The `mogwai tick-composition` fixtures measured each of the five presets then
shipped - ETHUSDT and SOLUSDT were retired 2026-08-09, leaving MNQ, MES and
BTCUSDT - independently over 2,000,000 parent events, eight seeds, and four
arrival modes.
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
0.029553 wall seconds, so its horizon does not shrink. This round regenerated
both fixtures from ONE invocation of `tick-composition`, which then took an
`--out-6` and an `--out-7` path and was followed by
`mogwai tick-composition-ratios compare`. The command takes a single `--out`
today and every later protocol is measured independently, so the paired form is
recorded here as history rather than as a runnable line. Protocol 6 was a count
projection of the protocol-7 tape, so the two fixtures are counted off a single
traversal and were paired by construction rather than by two runs agreeing. Both
documents carried the same `pairing_id`, which the ratio comparison asserts on,
so a fixture paired with a stale partner is refused rather than silently
ratioed. Both were serialized in full and staged beside their destinations
before either was touched, so a serialization failure or a full disk could not
consume a finished run; the two renames were still two operations, so what that
shape guaranteed was DETECTION of a mismatch rather than its prevention.
`--jobs N` sets worker count, defaulting to the machine's parallelism, and
still does.

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
clock. That mechanism is still unresolved.
`the_fanout_default_carries_the_protocol_11_exception`, in
`mogwai-server/src/config.rs`, pins the default so a later mechanical
application of the generated proposal must be argued, not slipped
through; it now pins 1,048,576, the per-boat depth the 2026-08-15
section above records, rather than the 4,194,304 that stood when this
exception was taken.

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

## 2026-08-15 data ownership and micro-optimization audit

Host `bygg`, release, current working tree against commit `0352f64`. The
registered `arrival_walk` surface ran five identical 6,000,000-parent,
50,914,229-child walks before and after the proposed cached geometric
denominator. Both read 300 ms externally. Baseline UUID `307adca9` records
300 / 300 / 301 / 301 / 300 ms; the changed dirty-tree run read 300 ms. The
cache therefore did not earn its landing and was reverted.

Checkpoint `source_positioning` measured 202.27 us before and 199.80 us after
sharing `GeneratorScalars` and `SessionCalendar` across generator clones.
Criterion reported no detectable change: 0.38% point estimate, p = 0.05, with
a 0.02% to 0.76% interval. The change is retained for allocation identity, not
claimed as a throughput win: one symbol string and one calendar window vector
now back the lead plus every retained snapshot, instead of one heap copy per
clone. The committed warmup retains roughly 520 snapshots and the hard cap
remains 4,096, including pinned control boundaries.

The per-event ownership reductions are pinned directly instead of inferred
from a wall dominated by stochastic generation. `PublishedBook` is 48 bytes
and contains only two tick prices and two decimal sizes; fitted corpus metadata
stays in `GeneratorScalars`. Materialized trade and quote symbols share one
`Arc<str>` allocation with the generator, and `TickRuleAggressor` retains that
same allocation as its key. Their JSON representation is unchanged.

## 2026-08-15 wire tag decode probe

Host `bygg`, release, current working tree. The registered `tag_decode_probe`
target decoded the same representative `Trade` frame 2,000,000 times per arm and
counted allocator calls through its own wrapping global allocator. Re-run it
with `brokkr mogwai tag_decode_probe` (an optional argv is the iteration count),
or `brokkr run --release tag_decode_probe` for a bare run that stores no row.

| decoder | ns / frame | allocations / frame |
|---|---:|---:|
| serde internally tagged enum | 219 | 4 |
| identical payload fields as a plain untagged struct | 103 | 2 |
| tag probe plus direct market payload, LANDED | 224 | 2 |
| tag probe, noncanonical escaped tag | 245 | 4 |

READ THIS AS AN ALLOCATION RESULT, NOT A THROUGHPUT ONE. The landed decoder is
5 ns per frame SLOWER than the internally tagged enum it replaces, because
probing the tag is a second pass over the same bytes; the plain-struct row is
the idealized single-parse floor and is not reachable on a tagged wire without
changing the wire. What the change buys is two fewer allocator calls per market
frame on the adapter's per-tick path, which is what the reported defect actually
was. It is kept on that ground alone.

The escaped-tag row is the `Cow` fallback in the probe: when the JSON tag
carries a `\uXXXX` escape, serde_json cannot borrow it, so the probe owns a
`String` and the frame costs its two allocations back. The venue never emits
that spelling; the row exists to show the fallback is a bounded cost rather than
a refusal, which is what a borrowed `&str` probe would have been.

An earlier measurement of the same arms on a busier host read 244 / 121 / 245
and is superseded here. The earlier plain-struct arm also carried an extra
`trade_id` field that `TradeTick` does not have; the probe now asserts field
parity between its arms.

## 2026-08-19 what an inline `Symbol` would buy, and why it was not taken

Host `bygg`, release, 2,000,000 iterations per arm, run three times.
`crates/mogwai-protocol/examples/symbol_decode_probe.rs` decodes the same
representative `Trade` frame the tag probe uses, counting allocator calls
through its own wrapping global allocator. Run it with
`brokkr run --release symbol_decode_probe -- 2000000`.

| arm | ns / frame | allocations / frame |
|---|---:|---:|
| landed `VenueMessage::from_json_str` | 239, 220, 219 | 2 |
| payload struct, `Symbol = Arc<str>` (today) | 110, 115, 109 | 2 |
| payload struct, inline 32-byte `Copy` symbol | 103, 111, 103 | 0 |
| `mogwai_adapter::convert::trade_id`, ONE trade | 162, 154, 153 | 5 |

`Symbol` IS `Arc<str>`, and deserializing one costs TWO allocations, not the one
a reader expects: serde takes a `String` off the wire and then copies it into a
fresh `Arc`. An inline fixed-capacity symbol removes both, and about 4 to 7 ns.

THE FIRST NUMBERS RECORDED HERE SAID 19 ns, AND THEY WERE BIASED. That table was
taken from a probe whose arms differed in two uncontrolled ways at once, both
found by the round's cold review, and it is worth knowing which because either
one can recur in the next probe someone writes here:

- THE INLINE ARM OBSERVED ONLY `symbol.as_str().len()`, a read of the `len: u8`
  field, so nothing observed `bytes` and the 32-byte `copy_from_slice` that is
  the inline representation's entire cost was free to be elided. Both arms now
  `black_box` the whole decoded tuple, symbol value included.
- THE INLINE ARM ALSO RAN `validate_wire_symbol` PER FRAME while the `Arc` arm
  validated nothing - the opposite bias, undisclosed. Neither arm validates now,
  so the delta is representation ONLY, and the alphabet check the proposal would
  add is a cost NOT counted here. The measured saving is therefore an upper
  bound on the real one.

THE FOURTH ROW IS WHY THE CHANGE WAS REFUSED, and it is the row to reach for
whenever a per-frame decode saving is proposed. The adapter's socket reader is
the only decoder of `VenueMessage` that exists - `from_json_str` and
`from_json_slice` have one call site each, both in `lifecycle.rs`'s read loop -
and the first thing it does with a decoded trade is `convert::trade_id`, which
`format!`s all five fields and costs ~155 ns and five allocations on its own,
before the nautilus event construction, the `handler().await`, the tungstenite
framing, and the `Message::Text` `String` the frame already arrived in. And five
is itself a floor: the probe replicates `trade_id` down to its 56-bit mask but
omits `TradeId::new_checked`, which interns the string. A ~5 ns saving inside a
~220 ns decode, at the frame rates a paced venue serves, is not observable. THE
ARC SHARING IS ALSO NOT WASTED, it is just not earned at DECODE: it pays inside
the process afterwards, which is where `GeneratedSource` and `TickRuleAggressor`
share one allocation (see the section above).

THAT `VenueMessage` INVENTORY IS EXACTLY AS NARROW AS IT READS. Symbols also
reach the adapter through five UNTAGGED HTTP decodes that are not
`VenueMessage` at all and validate nothing - `client/shared.rs` (instruments),
two in `client/data.rs` (trades and quotes), `client/exec.rs` and `clock.rs`.
They are the same deliberate posture `convert::instrument_id` takes below, not
an oversight, but a claim about "the decoders" has to name them.

THE OTHER HALF OF THE SAME PROPOSAL was making `MAX_SYMBOL_LEN` a property of
the type rather than a validator a caller must remember to call. Audited, it
named one live gap, now closed in the same commit as this table:
`validate_submit_order` checked ONLY `symbol.len() > MAX_SYMBOL_LEN`, so the
empty string and any byte outside the wire alphabet were admitted at order
entry - the one client-inbound symbol ingress - while this document asserted
every ingress validated. It runs `validate_wire_symbol` now, on both the
`SubmitOrder` and the `SubmitOrderGroup` carriers, pinned by
`an_order_entry_symbol_is_judged_by_the_wire_alphabet`. What each ingress checks
today:

- ORDER ENTRY: the full wire alphabet, through `validate_submit_order` at
  `http::boundary_error`, the one gate the websocket order carrier uses for
  every command.
- URL-CARRIED SYMBOLS: the full wire alphabet, in `http.rs` and `source.rs`.
- CONFIG INSTRUMENTS: non-empty and `MAX_SYMBOL_LEN` only. `config.rs` calls
  `validate_wire_symbol` on an instrument's `index_symbol` and NOT on its own
  `symbol`, so a configured instrument may carry a symbol order entry would now
  refuse. Filed as an owner-level item in `notes/todo.md` rather than tightened
  here: it is operator-supplied rather than client-supplied, and it is a
  `mogwai-server` decision.
- THE ADAPTER'S DECODE: unvalidated DELIBERATELY, with `convert::instrument_id`
  using `NautilusSymbol::new_checked` so a hostile symbol drops one frame rather
  than an unsupervised task. Validating at decode would move that refusal, not
  close a hole.

So the type-level bound would have caught the order-entry gap for free, which is
the honest point in its favour; it does not survive the cost of the edit, and a
one-line call to the validator that already existed closed the gap instead.

The probe is deliberately NOT a registered `brokkr mogwai` target: it settled one
decision rather than opening a series.
