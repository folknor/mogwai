# Stage A performance round, preceding brick A

Owner-defined step, 2026-08-09: the full Stage A screen run does not start
until this optimization round has run and the owner is satisfied with the
cost. Grounds: the standing runtime-cost ruling in `AGENTS.md` (a
multi-hour computation is presumptively a defect to optimize before it is
run), against a screen priced at ~9.9 h.

This is a `notes/`-class document: transient, no truth guarantee, nothing
durable may cite it. It dies when the round ends and brick A runs.

## Background, so this reads without the spec open

Protocol 12b (`notes/protocol-12b-arrival-composition-spec.md`, the frozen
contract of record) repairs the generator's parent-count composition for
MNQ. Its **Stage A** is a corpus-free SCREEN: it walks a frozen grid of
candidate arrival mechanisms - four "families", each a small
parameterized stochastic model of how parent trade events arrive - and
keeps every parameter cell that satisfies a set of necessary conditions.
**Brick A** is the spec's name for implementing and running that screen;
its committed output is `analysis/mnq-arrival-screen.json`. **Brick A0**
is the screen's cost probe: it prices ONE cell per family (two seeds)
before the grid runs, so a budget blowout is discovered in seconds rather
than hours. Stage B (a later brick) then evaluates the survivors through
the real generator and does the only selection.

The four families: **family 1 (event_markov)** is the shipped arrival
mechanism with its persistence made tunable - it runs through the REAL
generator, price and size draws included. Families 2-4 (**wall_mmpp**,
**log_ou_cox**, **self_exciting**) are new wall-clock-time mechanisms that
run through a shared "cadence kernel"
(`crates/mogwai-data/src/generated/arrival.rs`), which draws only parent
timestamps and child counts - no prices, no sizes.

A cell is admissible only if it passes four per-seed conditions the spec
freezes as **A1-A4**: A1 support (the generated parent-count distribution
populates every bin the observed data populates), A2 mean-rate
preservation (generated mean parents per minute within [0.98, 1.02] of
observed, per hour), A3 sub-second composition (1-second zero-count
fraction within band), A4 validity (no refusals or runaway states).
Admissible cells are RANKED by a loss (a Wasserstein distance on
parent-count distributions), and `SELECTION_INDIFFERENCE = 0.01` declares
losses closer than that as not separating candidates - which is why
last-bit float drift in the loss is harmless.

The observed side of everything Stage A compares against is already
committed in `analysis/mnq-measure-12a.json`, the protocol-12a measurement
artifact - Stage A reads that file and needs no market data on disk.

Two pieces of frozen-contract vocabulary used below:

- **"Section 16 frozen constants"**: the 12b spec's section 16 freezes
  every grid (which parameter values each family is evaluated at), every
  seed set, every budget, every acceptance band and every cap as exact
  numbers. They may not be edited outside the spec's amendment process.
- **"Section 17 amendment"**: the 12b spec's stopping rule. Frozen
  content changes only through a reviewed, dated amendment argued to
  consensus with a codex review session (the "freeze protocol") - never
  by editing the spec or the constants directly. The 2026-08-09
  arrival-frame calibration amendment (recorded in the spec's section 0)
  is the worked example of the full procedure.

## The baseline, measured (A0 probe, 2026-08-09, post-calibration-amendment)

| cell | cost | budget |
|---|---:|---:|
| kernel family cell, 2 seeds (wall_mmpp / log_ou_cox / self_exciting) | 5.96-6.40 s | 7.0 s |
| family 1 cell (real generator), 2 seeds | 11.8 s | 50 s |
| Stage A total, cost model (coarse 6,326 s + refinement 29,200 s) | ~35,526 s = 9.9 h | 39,600 s |

The A0 finding that names the target: **the projection through
`SessionAcc` dominates a screen cell, not the draw**. The cadence-only
walk was assumed ~10x cheaper than a full generator walk; it measured ~2x,
because the cost is in measurement, not simulation.

## Where the time goes: the call chain

Entry point: `mogwai arrival-screen` in
`crates/mogwai-cli/src/arrival_screen.rs` (`run`), which calls per cell
into `crates/mogwai-lab/src/arrival_screen.rs`:

- `evaluate_cell` (line ~1021) - per seed, `project_seed` ->
  `project_walk` -> **`project_stream` (line ~848), the hot loop**.
- `project_stream` drives a parent walk (a `CadenceWalk` for the three
  kernel families; the real `GeneratedSource::advance_parent` for
  family 1) and, PER CHILD PRINT, calls `session_segment_at` (which UTC
  session and segment the print falls in) plus `SessionAcc::push_print`,
  with `push_parent` per parent and a session rotation on segment change.
  At MNQ cadence that is roughly 30 M parents and 35 M child prints per
  seed-month, per cell.
- `SessionAcc` (`crates/mogwai-lab/src/measure12a/`) is the FULL
  protocol-12a session measurement engine - built to compute the 12a
  spec's four evidence blocks, including joint histograms over quote and
  trade ranges the screen never reads. Stage A consumes only the
  parent-count marginal and the count/run-length/lag-1 statistics
  (12b spec sections 3 and 9.1 say exactly that).

So the first-order hypothesis, to be confirmed by a profile before
anything is rewritten: most of a screen cell is `SessionAcc` doing
bookkeeping whose outputs the screen discards, plus per-child segment
arithmetic that is recomputable-free (children share their parent's
minute except at stride boundaries). A lean screen-side accumulator that
produces EXACTLY the fields the screen reads is the obvious shape.
Secondary candidates once profiled: allocation churn in session rotation,
`session_segment_at` per child (memoize on the minute), and cross-cell
parallelism in the coarse pass (cells are independent; `evaluate_cell` is
currently driven serially per family).

## Constraints: the two lanes

The line between the lanes is the DRAW. A change that leaves every random
draw and every parent timestamp untouched only reshapes measurement and
is free. A change that moves even one draw changes the generated tape,
and the tape has identity machinery around it.

**Free lane - no amendment, optimize at will.** Anything on the
MEASUREMENT side of the draw: `project_stream`, `SessionAcc` or its
replacement, the A1-A4 evidence computation, the loss, allocation,
parallelism, `target-cpu` codegen (strict float semantics unchanged), and
algebraic float summation in the projections (sanctioned by the standing
`AGENTS.md` runtime-cost ruling; the loss is a ranking device and
`SELECTION_INDIFFERENCE` dwarfs ulp movement). The binding correctness
gates, all cheap:

- `brokkr test -p mogwai-lab arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks`
  - the screen's own oracle test: the projection must reproduce, for all
  eight committed seeds, the parent-count marginal and count statistics
  recorded in the committed `analysis/mnq-measure-12a.json` EXACTLY
  (integer counts, so exact equality is achievable and required). This is
  the gate that makes a lean accumulator honest.
- `brokkr test -p mogwai-data arrival_transcripts_replay_bit_exact` and
  `a_cadence_walk_and_the_generator_agree_parent_for_parent` - prove the
  draws did not move (the first replays 10,000 committed parent draws per
  family bit for bit; the second proves the kernel walk and the real
  generator agree parent for parent).
- `brokkr run --release mogwai -- arrival-screen --cost-probe` - the
  benchmark, and its per-hour evidence must be identical run to run
  (determinism per binary).
- `brokkr check --gate` green.

**Amendment lane - do not enter without a profile proving it binding.**
Anything in the kernel's draw path
(`crates/mogwai-data/src/generated/arrival.rs`): `next_parent`,
`baseline_integral`/`rate_at`, `advance_state_to`, the state transitions,
cell arithmetic, `cadence_base_mean_s`. A float reordering there can move
a timestamp one nanosecond, which changes every downstream draw. The
price is the full identity procedure: bump `ARRIVAL_KERNEL_VERSION` (the
kernel's own cache/fixture identity), regenerate the committed transcript
fixtures (`regenerate_arrival_transcripts_amendment_only`, an ignored
test sanctioned only by a signed amendment), bump `TAPE_PROTOCOL_VERSION`
(the workspace-wide tape identity in `crates/mogwai-data/src/lib.rs`;
`AGENTS.md` carries the unconditional bump rule), and record a section 17
amendment through the freeze protocol. The A0 evidence says the draw path
is NOT the bottleneck, so this lane should stay closed.

**What may not move at all:** the section 16 frozen constants - grids,
seed sets, budgets, bands, caps. The budgets are CEILINGS and coming in
under them needs nothing; editing them to flatter a result is a section 17
amendment. The screen's verdict semantics (A1-A4, the loss definition,
refusal behavior) are frozen contract. No Stage A artifact exists yet, so
there is nothing to re-bless; the post-optimization screen run is the
first artifact.

## Rider

`rust-version` is bumped to 1.99 (nightly toolchain in use), which puts
the stabilized algebraic float operators (`algebraic_add` and siblings,
stable from Rust 1.98: float ops the compiler may reorder and vectorize)
in reach for the free lane.

## Exit criterion

The owner looks at a fresh `--cost-probe` and the implied total and says
so. Then brick A runs on the optimized binary, per the 12b spec item 2.
