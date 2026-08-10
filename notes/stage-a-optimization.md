# Stage A performance round, preceding brick A

Owner-defined step, 2026-08-09: the full Stage A screen run does not start
until this optimization round has run and the owner is satisfied with the
cost. Grounds: the standing runtime-cost ruling in `AGENTS.md` (a
multi-hour computation is presumptively a defect to optimize before it is
run), against a screen priced at ~9.9 h when the round opened and at
~5.8 h once step 0 corrected the refinement double-count (see the
baseline table).

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
| Stage A total, cost model (coarse 6,326 s + refinement 14,600 s) | ~20,926 s = 5.8 h | 39,600 s |

CORRECTED 2026-08-10, during step 0. The model above read 35,526 s
(9.9 h) until the batch estimator forced the question of what the
refinement caps mean. They are TOTALS shared by both rounds, not
per-round allowances: the driver accumulates `used` and passes
`cap - used` to round 2, so a family spends its cap once. The old model
spent it in both rounds, double-counting refinement - 29,200 s where the
driver permits 14,600 s. The `STAGE_A_BUDGET_S` doc comment carried the
same error and is corrected with it. The 39,600 s ceiling is deliberately
UNCHANGED: correcting an overestimate is not grounds for tightening a
frozen ceiling mid-round.

Every "9.9 h" below is the superseded figure, kept where it records what
was believed at the time. The round's premise is unaffected in kind -
5.8 h is still a multi-hour computation and still presumptively a defect
to optimize - but any ratio computed against 9.9 h now overstates the
win by about 1.7x.

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

The profile is now instrumented rather than hypothetical.
`crates/mogwai-lab/examples/screen_projection_bench.rs` runs one probe cell
per family, cache bypassed, under `hotpath`;
`crates/mogwai-data/examples/arrival_walk_bench.rs` runs the kernel draw with
nothing attached. The gap between them is the measurement cost this round is
attacking. The probe itself now reports its work size on both output channels
- the first post-amendment reading is `cells_evaluated=4 parents=252704639
prints=295958882 peak_rss_bytes=487493632` - so a later claim of a speedup can
be checked against whether the work changed. `reference/performance.md` has
the invocations.

The pre-profile hypothesis was that `SessionAcc` bookkeeping the screen
discards dominates, with per-child segment arithmetic and cross-cell
parallelism as secondary candidates. The profile has now run and largely
confirms it, with one correction recorded below. What follows replaces
that hypothesis.

## The measured profile

Numbers from the stored profiles, four seed walks:

| frame | time | share |
|---|---:|---:|
| `project_stream` | 14.61 s | 97.18 % |
| `close_reduced` | 455 ms | 3.03 % |

Work size for those four walks: 126,143,060 parents and 147,738,385
prints. Cumulative allocations: `project_stream` 5.9 GB,
`close_reduced` 1.2 GB, `main` 459.8 MB. Peak RSS about 488 MB.

The bare kernel benchmark walks 6,000,000 parents in 214.7 ms, about
35.8 ns per parent. It is NOT the identical calendar workload, so it is
not a precise bound, but it confirms the draw is far cheaper than the
attached projection: roughly 3x cheaper per parent.

The host exposes 32 logical CPUs. The screen driver evaluates every
family, cell, seed and refinement candidate serially.

The decisive reading: session closure and final histogram calculation are
NOT the problem. The hot loop is.

## Where the cost actually sits in the hot loop

Before step 1, each `ParentSummary` made `project_stream` perform:

- a loop over all children,
- a `session_segment_at` call for every in-window child,
- a second `session_segment_at` when opening the parent, through
  `open_parent_at`,
- a `BTreeMap` operation per print minute (`trade_min`),
- a `BTreeMap` operation per parent minute (`n_min`),
- a retained `u64` timestamp in a per-segment vector (`parent_ts`),
- a generic JSON session record at close.

For the profiled four walks that is roughly 274 million session
assignments: one for each of 147.7 million prints and one for each of
126.1 million parents. Each assignment performs 128-bit division and
calendar arithmetic in `crates/mogwai-lab/src/session.rs`.

`SessionAcc` is doing exactly what it was designed to do; it is the wrong
engine for this screen. Its price and range fields are populated with
zeroes, its generic session model retains data the screen never reads,
and its output representation is far richer than the verdict surface.

**The sharpest single finding.** `push_print` is called with
`price_nanos: 0`, always. Every minute's trade range is therefore
`(0, 0)`, and the only thing the per-child loop contributes to the output
is (a) the SET of populated minutes that seeds `block1`'s minute
iteration and (b) the `prints` counter. The per-child data is literally
constant. That is what makes the child loop collapsible as an exact
equivalence transformation rather than an approximation.

### A correction to the first profile report

The report's claim of "three later scans over those retained timestamps
for 1-second, 5-second and 60-second windows" overstated the cost. There
is one linear pass per horizon, not a repeated scan: `block2` is already
a two-pointer merge of `parent_ts` against `window_schedule`. More
importantly it lives inside `close_reduced`, the measured 3.03 % frame.

Removing `parent_ts` retention is therefore a MEMORY-enabling change, not
a CPU win, and block-2 closure is not a defensible justification for a
rewrite. The single-core estimate of 2x to 3x is a HYPOTHESIS resting on
exactly four things:

- collapsing 148 million child iterations,
- removing almost all child-side `session_segment_at` calls,
- removing almost all child-side `trade_min` tree operations,
- replacing the parent-side `n_min` tree operations.

The same caveat applies to the linear-Wasserstein item below: it also
sits in the 3 % frame.

## What the layer-1 oracle pins, and what it does not

`arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks`
compares the parent-count marginal of generated block 1 exactly against
the committed marginal. It therefore DOES pin the aggregate
populated-minute semantics Stage A consumes:

- the exact parent count N,
- the number of populated minutes carrying each N,
- the UTC hour,
- zero-parent populated minutes created by a burst crossing a minute
  boundary.

A collapsed child path that dropped or added an N = 0 populated minute,
or placed one in another hour, fails the oracle.

It does NOT pin:

- the exact minute identity,
- session-date attribution,
- since-open or until-close labels,
- full block-1 rows before marginalization,
- the `prints` work counter,
- every rare boundary case, merely by running eight committed seeds.

So it is a strong aggregate gate and not by itself sufficient for the
projection lifecycle.

Existing focused tests in `crates/mogwai-lab/src/arrival_screen.rs`
already cover several cases:
`the_screen_projection_places_a_straddling_burst_in_two_minutes`,
`a_burst_straddling_a_session_boundary_files_its_parent_in_the_old_session`,
`a_child_with_no_segment_is_pushed_not_refused`, and
`a_projection_gap_refuses_rather_than_dropping_a_boundary_minute`.

**The test seam permits arbitrary strides, and one green test uses them.**
The `one_parent(parent_ts_ns, child_count, child_stride_ns)` helper is
called with a ONE MINUTE stride in
`a_child_with_no_segment_is_pushed_not_refused`. So "a burst spans at
most 4.095 ms, therefore first and last child describe the populated
minutes" is true of the shipped sources (1 microsecond stride, 4,096
child cap) but false as a general rule the seam allows. The collapse must
use a distinct-minute iterator, whose cost is O(populated minutes) rather
than O(children): at most two minutes in production, exact under
artificial fixtures, with no slow production path.

That same test also constrains the collapse more tightly than the
marginal does. A child landing in the 15:15 to 15:30 halt resolves to no
segment, must still be PUSHED into the open session, and the refusal must
come from `block1` at close naming the minute - never a push-time A4
projection refusal. Resolving per distinct populated minute makes that
fall out naturally instead of needing a special branch.

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

## The agreed plan: an instrument, then four attributable steps

The first report recommended one intrusive replacement landing the
projector and the scheduler together. That was revised. The scheduler is
the larger win and the semantically SAFER one, since it touches no
measurement semantics and cannot perturb the layer-1 oracle; the
projector is the smaller win and rewrites frozen-contract-adjacent
behavior. Landing them together yields one number and no way to
attribute it. The agreed order is:

0. design, freeze, register and baseline the batch instrument,
1. remove per-child work,
2. remove retained per-parent state and the trees,
3. exploit the independent work across cores,
4. clean up the typed boundary afterward.

Each step gets its own cost-probe and allocation reading, so every
semantic change is separately falsifiable, followed by a separately
attributable scheduler result.

STATUS 2026-08-10: steps 0 through 4 are complete. Step 0 rows are
`fbd03346` quick and `66d4797d` full. Step 1 rows are `a0921513` quick
and `5c012131` full. Step 2 is implemented in `ddd5284`; its decisive
pre-commit quick and full verification runs were deliberately not stored
in `results.db`. Step 3 is implemented in `8564bc6`, with the seed-task
estimator correction in `c87fe2f` and the measured default-worker cap in
`211d096`. Its final full row is `0b861338`. Step 4 is implemented in
`924c000`; its focused allocation row is `29c19b0e`.

### Step 0 - freeze the instrument before any optimization

Step 0 completes BEFORE optimization begins. Otherwise step 1 is measured
against an instrument whose workload may still move afterward, and every
later performance claim becomes attributable to workload selection rather
than to code. The instrument's shape is specified under "The measurement
instrument this round needs" above; step 0 is the work of producing it.

**Sample allocation and estimator weights are different things, and
conflating them corrupts the estimator.**

- SAMPLE ALLOCATION decides which 72 cells are worth spending benchmark
  time on. It may freely favor expensive or variable regions.
- ESTIMATOR WEIGHTS describe how much frozen Stage A population each
  sampled cell represents. They come from population and inclusion
  probability, NEVER from estimated CPU contribution.

Weighting by estimated CPU contribution would bake the estimator's
expected answer into its own weights.

Step 0 produces: a precisely defined sampling population; a deterministic
selection procedure; a committed quick/full manifest; exact extrapolation
weights; a stable plan hash; baseline quick and full rows; and structural
validation of the manifest.

#### The sampling frame

The coarse population is easy: every coarse cell is known and every one
runs.

Refinement is the real design problem. Its evaluated population depends
on prior verdicts, while the CAP is known, so the manifest cannot pretend
to sample actual survivors. Define the refinement sampling frame as the
complete set of VALID depth-1 and depth-2 lattice points the refinement
mechanism could propose, partitioned by `family x level x
parameter-region`. The maximum-cap estimator then uses the frozen
per-family caps without claiming to know which cells survive.

The split of a family's cap between refinement rounds is also unknown.
The conservative scheduled estimate should therefore evaluate every
feasible allocation of the family cap between rounds and report the
allocation producing the GREATEST modeled wall time. That preserves the
real refinement barriers without guessing a survivor count or a round
split.

#### The frozen populations, verified against `coarse_grid`

Coarse axis cardinalities, recomputed from the grid constructors rather
than taken on trust (the endpoint push in `log_grid` is what makes three
of these land one above the decade ladder):

| family | axis lengths | coarse cells |
|---|---|---:|
| EventMarkov | 19 | 19 |
| WallMmpp | 6 x 7 x 12 | 504 |
| LogOuCox | 10 x 12 | 120 |
| SelfExciting | 16 x 9 | 144 |

787 coarse cells in total. The structurally proposable refinement frames
- depth 1 being one midpoint axis, depth 2 the quarter-axis points plus
the intersections carrying midpoints on two axes:

| family | depth 1 | depth 2 |
|---|---:|---:|
| EventMarkov | 18 | 36 |
| WallMmpp | 1,314 | 3,769 |
| LogOuCox | 218 | 535 |
| SelfExciting | 263 | 646 |

#### Strata: binary product quantiles, 54 strata

Two empirical quantile bins per axis, product partition within each
family and level:

- EventMarkov: 2 regions,
- WallMmpp: 2 x 2 x 2 = 8 regions,
- LogOuCox: 2 x 2 = 4 regions,
- SelfExciting: 2 x 2 = 4 regions.

18 parameter regions per lattice level, three levels sampled separately
(coarse, depth 1, depth 2), so **54 probability strata**.

Quantile boundaries come from the ACTUAL ENUMERATED coordinate
population for that family and level, which handles the irregular depth-1
and depth-2 coordinate distributions without inventing continuous
parameter ranges.

That is tight for 64 cells once the certainty anchors are added. Do NOT
weaken the WallMmpp partition to preserve a round number. **The full
panel is 72 cells**:

- 54 cells, one probability sample per stratum,
- 8 certainty anchors, which are also the quick panel,
- 10 cells allocated from pilot variance.

At current costs that should still land near 13 to 15 minutes serially.

#### Deterministic probability selection

Hand-picked space filling makes bootstrap intervals hard to defend,
because inclusion probabilities are unknown. The procedure instead:

1. partition each family and level into parameter-region strata using
   axis quantiles,
2. treat required anchors - probe cells, centers, selected corners - as
   certainty strata,
3. within every remaining stratum, hash the canonical cell identity
   together with a fixed selection seed,
4. select the lowest K hashes,
5. give every non-anchor cell in that stratum the KNOWN inclusion
   probability K / N.

That is reproducible, uniform within each stratum, space covering
because the strata enforce coverage, auditable without executing a
stochastic walk, and compatible with inverse-probability weights and
bootstrap estimation.

The selection hash input uses EXACT PARAMETER BITS and lattice
coordinates, never formatted floats.

For every stratum CONTAINING ANCHORS: remove the anchors from the
probability population, give each anchor certainty weight 1, select at
least one non-anchor by hash, and give selected non-anchors inclusion
probability K / (N - A). That preserves complete population coverage. An
anchor cannot replace the probability sample unless it exhausts the
stratum.

#### Quick and full membership

`quick` is exactly eight cells: four coarse probe cells, one per family,
supplying the centers, plus four refinement anchors, one per family. They
remain certainty anchors in `full`, which retains all eight tasks
unchanged.

Both halves are DERIVED, with no implementation discretion left. The
coarse four are already: `probe_cell` takes the per-axis midpoint index
scaled by `LATTICE_SCALE`, so naming a family names the cell.

The refinement four are derived by the rule below.

##### The refinement anchor rule

Define a corner-adjacent depth-2 candidate frame per family, in lattice
coordinates:

- exactly one axis is a QUARTER coordinate,
- that coordinate is either 1 or `axis_max - 1`,
- every other axis sits at either 0 or `axis_max`,
- the resulting cell must belong to the valid depth-2 proposable frame.

Candidate counts follow from the axis counts - choose which axis carries
the quarter coordinate, times two ends, times the corner assignments of
the remaining axes:

| family | axes | candidates |
|---|---:|---:|
| EventMarkov | 1 | 2 |
| WallMmpp | 3 | 24 |
| LogOuCox | 2 | 8 |
| SelfExciting | 2 | 8 |

Then: hash each candidate's canonical identity with a fixed
ANCHOR-SPECIFIC selection seed, select the lowest hash independently per
family, materialize the four resulting exact cells in the manifest, give
them certainty weight 1, and remove them from their probability
populations before the lowest-K selection runs.

The anchor seed is DISTINCT from both the pilot seed and the final
probability-selection seed, so neither influences the certainty cells.

**Quantile boundaries are computed from the complete enumerated
population BEFORE anchor removal.** Otherwise selecting an anchor could
move a stratum boundary and thereby alter the very sampling frame the
anchor is later removed from.

`quick` need NOT cover both refinement depths, because both execute the
same stage path; the full panel covers both depths through its
probability strata.

`quick` stays a development stopwatch. Its sample is too thin for an
authoritative refinement envelope, so it must not print a budget-facing
estimate that could be mistaken for the full-panel result.

#### Allocating the ten adaptive cells

After every stratum has its minimum sample, the remaining ten cells are
allocated by expected contribution to ESTIMATOR VARIANCE, approximately
proportional to

    population_size * cost_standard_deviation / sqrt(task_cost)

This beats permanently allocating by current mean CPU contribution: a
region that is expensive but nearly CONSTANT needs fewer samples than a
cheaper region whose cost varies widely.

#### The pilot: three cells per stratum, one full-window seed

**Do not shorten the measurement window.** A shortened window distorts
exactly the properties the panel must characterize: persistence at large
`tau_s`, session and halt exposure, refusal timing, long-run parent rate,
and amortized session rotation and closure. Reduce SEED MULTIPLICITY
instead - the pilot estimates cell-to-cell cost variance, so one fixed
seed over the full window suffices.

Three pilot cells per stratum gives 54 x 3 = 162 seed walks. At about
6 s per EventMarkov seed and 3 s per kernel seed: 18 EventMarkov walks at
about 108 s, 144 kernel walks at about 432 s, roughly **540 s or 9
minutes** total. Ample room under the 20-minute ceiling.

Use the SAME seed for every pilot cell, so seed variability does not
contaminate the parameter-region variance estimate. The quick anchors,
which run their real two-seed and four-seed shapes, give a secondary
reading on seed variability.

Pilot and final selection use SEPARATE fixed hash seeds: one selects
pilot cells, the pilot results determine K per stratum, and a different
hash seed selects the final panel cells. That stops the cells used to
estimate variance from also determining which measured costs enter the
final estimator. Conditional on the resulting K, final inclusion
probabilities stay known and uniform.

For strata with fewer than three non-anchor candidates the pilot takes a
CENSUS. The current frame sizes suggest this will not bind, but the rule
is structural rather than contingent.

Three observations give a noisy standard deviation, so shrink each
stratum estimate toward its family-and-level pooled variance, or at
minimum apply a variance floor. Otherwise one noisy three-cell stratum
consumes the entire ten-cell adaptive allocation.

The implemented version makes that rule exact: variance is the equal
weight mean of the stratum sample variance and the family-and-level
pooled variance, with a floating-point variance floor. The ten integer
allocations use Hamilton largest remainders over the resulting scores.
The measured pilot assigned one extra cell to ten distinct strata, so no
stratum captured more than one adaptive slot.

The pilot exists only to freeze panel version 1: its results select the
manifest and never become benchmark rows of record.

The exact pilot artifact is committed beside the manifest. The manifest
hash includes the canonical pilot hash, and a test rebuilds the complete
manifest from that committed pilot. A pilot retained only under `target/`
would make the adaptive selection self-consistent but unreproducible.

#### Step 0 decisions, as recorded

- three lattice levels sampled separately,
- two empirical quantile bins per axis,
- product partition within each family and level,
- 54 probability strata,
- full panel size 72,
- quick panel size 8, all certainty anchors,
- ten adaptive full-panel cells,
- pilot size up to 162 cells,
- one fixed seed per pilot cell,
- full measurement window,
- three independent fixed hash seeds: anchor, pilot, final selection,
- refinement anchors derived by the corner-adjacent depth-2 rule,
- quantiles computed before anchor removal,
- worst-case refinement round split retained.

#### Manifest validation

Structural only. These tests establish that the stopwatch measures the
declared panel; they establish nothing about generated statistics.

- every `quick` task appears unchanged in `full`,
- every cell belongs to the frozen domain and its claimed lattice level,
- coarse cells use exactly two seeds,
- refinement cells use exactly four seeds,
- inclusion probabilities are positive and valid,
- extrapolation weights sum to the represented population,
- certainty strata carry weight one per represented task,
- anchors are excluded from the probability population of their own
  stratum,
- every stratum carries at least one non-anchor probability sample,
  unless the anchors exhaust it,
- every refinement anchor is level 2, lies in the valid proposable
  frame, satisfies the corner-adjacent coordinate rule, is excluded from
  the non-anchor population, and appears unchanged in both `quick` and
  `full`,
- every required family and stage shape is present,
- plan serialization and hash are stable,
- the estimator version is included in the hash,
- refinement cap accounting never exceeds the frozen caps.

#### The plan hash

The hash covers both the executed tasks AND their interpretation:
manifest schema version; selection seed and algorithm version; quick or
full membership; exact cell bits and lattice coordinates; family and
refinement level; seeds; stratum; inclusion probability; extrapolation
weight; estimator version; refinement cap model; anchor rule version;
anchor selection seed; the candidate-frame definition; and the selected
anchors' exact parameter bits and lattice coordinates.

That prevents a row from keeping its identity after only its
extrapolation logic changed.

#### The implementation boundary

The order of work inside step 0:

1. enumerate the frozen coarse and refinement populations,
2. compute quantile boundaries, before anchor removal,
3. derive the coarse probes and the hashed refinement anchors,
4. run the independent pilot selection,
5. allocate the adaptive ten,
6. perform the final lowest-K selection,
7. validate and hash the plan,
8. commit the exact manifest,
9. record the quick and full baseline rows.

**The three selection seeds are named and domain-specific**, so they can
never be confused with the Stage A STOCHASTIC seeds. Those already exist
and are frozen - `STAGE_A_SEEDS = [201, 202, 203, 204]`, with
`CONTROL_FIT_SEEDS` and `CONTROL_TEST_SEEDS` at 301 to 308 - and they
drive generators. The panel's three drive a HASH and select cells; a
name collision between the two kinds would be an ugly class of bug,
because either would run and only one would be right.

**The first implementation change contains NO optimization.** It ends
with the reproducible manifest, the structural tests, the plan hash and
the baseline rows, and it touches nothing on the hot path. That is what
freezes the measuring instrument before step 1 goes near
`project_stream`.

#### Step 0 exit

Step 0 ends only when there is a quick baseline row, a full baseline row,
a reproducible plan hash, a maximum-cap estimate against 39,600 s, and a
defensible validation reading.

IMPLEMENTATION REVIEW CORRECTION: the 72-cell design has only one
probability sample in 44 of its 54 strata. It therefore cannot produce
two independent estimates of every stratum, and an ordinary within-stratum
bootstrap gives those 44 strata exactly zero variance. The batch target
must not print such an interval or describe a family-level split as two
stratified halves. It reports the 10 comparable strata, the 44 singleton
strata and `design_based_interval_available=0`. Mapping validation remains
open until a separate validation expansion or a larger panel supplies at
least two independent probability samples per stratum. This does not
invalidate the panel as a fixed optimization stopwatch; it prevents the
panel from overstating its extrapolation certainty.

Step 0 is complete. The one-worker full baseline is 790.2 s, with
787.1 task CPU seconds over 72 cells and 242 seed walks. Its maximum-cap
p90 serial estimate is 27,448.7 s, higher than the rough 20,926 s model
but below the frozen 39,600 s ceiling. The exact work is 7,571,686,367
parents and 8,868,542,328 prints.

### Step 1 - collapse the child loop

`SessionAcc`, the reduced session JSON and the verdict path stay
unchanged. Inside `project_stream`:

- algebraic in-window child index bounds from `parent_ts_ns`,
  `child_count`, `child_stride_ns`, `start` and `end`, with `end`
  EXCLUSIVE, matching the current `(start..end).contains(&ts)`,
- a distinct-minute iterator over that index range, jumping minute to
  minute, yielding `(minute, representative_ts, child_count_in_minute)`,
- per yielded minute: the existing `session_segment_at`, rotation and
  `push_print(ts, 0)` logic, run ONCE instead of per child,
- `prints` incremented by `child_count_in_minute` AFTER the rotation
  logic and only when `acc.is_some()`,
- the parent still opening on its first in-window child, preserving the
  ordering the `IMPLEMENTATION DECISION` comment defends.

The third iterator field is load-bearing. `prints` is not unconditionally
the count of in-window child indices: the current loop increments it only
inside `if let Some(active) = acc.as_mut()`, so a segmentless child with
no previously opened accumulator is not counted. Adding
`child_count_in_minute` under the same condition keeps the closed form
while preserving the exact definition - prints PUSHED INTO an
accumulator.

The equivalence argument:

- `push_print(ts, 0)` is idempotent within one minute for block 1,
- `session_segment_at` is constant within a minute, because every
  calendar boundary is minute-aligned,
- rotation therefore happens at most once per populated minute,
- child multiplicity within a minute affects only `prints`,
- the first yielded representative IS the actual first in-window child,
  so parent-opening order is unchanged,
- a segmentless minute is still pushed into an existing accumulator and
  is still allowed to fail later at block-1 closure.

New focused tests are owed for: the exact `prints` count; children
clipped at measurement start; children clipped at measurement end; a
parent before `start` with later children inside the window; a child
exactly AT `end`, which must be excluded; a parent with no in-window
child; and several non-contiguous populated minutes under an artificial
large stride.

Plus one sharp fixture pinning `open_parent_at` to the ORIGINAL parent
timestamp: put `parent_ts_ns` inside the 15:15 to 15:30 halt and before
`start`, give it a large stride so its second child lands at or after the
15:30 reopen and inside the measurement window, and make that second
child the only in-window child. The correct path calls
`open_parent_at(parent_ts_ns)`, finds the original instant segmentless
and returns the projection refusal. An implementation keying on the first
surviving child instead maps it to the reopened segment and wrongly
succeeds. A parent merely before `start` but in a valid segment does not
pin this, because `close_parent` skips its parent record before it ever
checks the stored session invariant.

Step 1 acceptance surface:

- exact layer-1 marginal and block 2 for all committed seeds,
- exact existing session, halt and rotation fixtures,
- exact print multiplicity,
- exact start and end clipping,
- exact non-contiguous minute coverage under arbitrary stride,
- exact parent-before-start behavior,
- explicit refusal when the original parent is segmentless even though
  its surviving child is not.

That collapses the per-child loop to O(distinct populated minutes)
without narrowing the existing projection contract.

#### Step 1 measured result

Implemented in `e4c8c24`. All focused boundary fixtures, the complete
check, the eight-seed layer-1 oracle and both draw-identity gates pass.
The tape and kernel versions are unchanged because the edit is entirely
on the measurement side of the draw.

The result is real but small. The quick row moved from 87.100 s to
85.101 s best wall. The full row moved from 790.200 s to 780.201 s,
1.27%, while task CPU moved from 787.144 s to 777.564 s, 1.22%.
The maximum-cap p90 estimate moved from 27,448.707 s to 27,121.840 s,
a 326.9 s reduction. Parent, print, cell and seed-walk counters are exact
across both full rows.

The reason the entering hypothesis missed is now measured rather than
speculative: the full panel has 8,868,542,328 prints over 7,571,686,367
parents, only 1.171 prints per parent. The collapsed path still performs
roughly one populated-minute operation per parent, so it removes at most
the roughly 14.6% of print-loop iterations in excess of one per parent.
The distinct-minute primitive remains useful substrate for step 2, but
step 1 is not the projected 2x to 3x single-core win.

### Step 2 - a lean screen accumulator keeping its current output

Narrower than the final typed redesign. A screen-specific accumulator
still emits the current reduced session JSON, while internally using:

- dense per-session minute slots for populated state and parent count,
- dense second counts or streaming window counters for block 2,
- no `trade_min` and no `n_min` trees,
- no retained `parent_ts`.

A dense 1-second session array is only a few hundred kilobytes and is a
simple exact substrate for the 1 s, 5 s and 60 s block-2 reduction. Much
less risky than replacing the verdict representation at the same time.

This is the step that establishes whether 32 workers fit beneath the RSS
ceiling. Replacing only the two maps may not be enough: `parent_ts` must
also leave the retained path before wide parallelism is safe.

#### Step 2 measured result

Implemented in `ddd5284`. `ScreenSessionAcc` keeps dense populated-minute
and parent-count slots plus dense per-segment second counts. It emits the
same reduced session JSON while removing `trade_min`, `n_min` and
`parent_ts` from the Stage A path. A direct differential fixture compares
its complete block-1 and block-2 output against `SessionAcc` exactly. The
complete check, eight-seed layer-1 oracle and both draw-identity gates pass.

The three-run quick verification moved from the step-1 best of 85.101 s
to 70.100 s, with task CPU moving from 83.740 s to 68.919 s. The one-run
full verification moved from the step-1 best of 780.201 s to 620.200 s,
20.51%, while task CPU moved from 777.564 s to 617.991 s, 20.52%. The
maximum-cap p90 estimate moved from 27,121.840 s to 21,218.767 s, a
5,903.1 s or 1 h 38 min reduction. All work counters are exact.

Those batch readings were intentionally dirty-tree verification runs and
have no `results.db` UUID. Do not invent one. The committed focused rows
are `c9927a73` hotpath and `0d874ef3` allocation: `project_stream` moved
from the step-0 14.61 s to 11.65 s, and exclusive accumulator-path
allocation moved from roughly 7.1 GB across `project_stream` plus
`close_reduced` to 3.4 GB in the replacement path. The one-run full peak
RSS reading moved from 610,578,432 bytes at step 1 to 486,887,424 bytes;
it is directional evidence for scheduler sizing, not a repeated bound.

### Step 3 - the scheduler

The full run is roughly 9,000 independent month-scale seed walks. The
only real dependencies: all required seeds for a cell must finish before
its verdict is reduced, and a family's previous refinement round must
finish before the next proposal is generated. Nothing requires cells
within a pass to be serial.

The landed task unit is `(cell, seed)`, not a whole family or whole cell.
The coordinator generates coarse tasks for all families, places results
in deterministic cell and seed slots, reduces each cell as its seeds
complete, and schedules every family's candidates together within each
refinement round. The implementation retains one global barrier between
coarse, refinement round 1 and refinement round 2. That is more
conservative than the sketched per-family dependency scheduler, but the
measured 14x full-panel result leaves no performance reason to add its
coordination complexity now.

Workers receive a cloneable immutable `ProjectionContext` carrying the
profile, binding and cache handle, not `ScreenContext`; the latter keeps
its `Rc<RefCell<...>>`-backed `ObsContext` on the coordinator. Verdict
reduction, budget sampling and cancellation remain central. Per-cell
`cost_s` is worker execution plus verdict reduction and excludes queue
wait. The artifact is assembled in canonical family/lattice order rather
than completion order.

Cache pruning is hoisted to one preparation before any worker starts.
Each prepared write uses a process-and-sequence-unique staging file and
an atomic rename, so concurrent writers cannot expose partial JSON.
Budget crossing flips a central cancellation flag; workers already in a
walk may finish and report, but no new task begins after observing it.

`--jobs` is explicit. It initially defaulted to
`available_parallelism()`, but the measured curve found a knee at 16 on
the 16-core/32-thread host: 24 workers regressed wall and increased summed
task time. The shipped default is therefore reported parallelism capped
at 16, with the flag retaining full override control.

Do NOT parallelize inside one stochastic walk. The arrival state is
recursive. Cells and seeds are the clean concurrency boundary.

The one-run quick scaling rows, all at exact 8 cells, 24 seed walks,
752,083,142 parents and 880,798,950 prints, are:

| jobs | uuid | external wall | measured wall | summed task time | peak RSS |
|---:|---|---:|---:|---:|---:|
| 1 | `de39aad9` | 69.200 s | 68.706 s | 69.077 s | 699,817,984 B |
| 2 | `edcf03a2` | 36.100 s | 35.220 s | 70.137 s | 696,905,728 B |
| 4 | `b3f89ff9` | 20.107 s | 19.082 s | 71.527 s | 819,929,088 B |
| 6 | `6b2a99d3` | 14.100 s | 13.087 s | 72.250 s | 844,824,576 B |
| 8 | `b5a1e13b` | 11.108 s | 10.225 s | 72.859 s | 1,033,072,640 B |
| 12 | `b43f10a2` | 8.100 s | 7.795 s | 73.350 s | 1,068,077,056 B |
| 16 | `4c87556a` | 6.100 s | 5.868 s | 74.027 s | 1,303,625,728 B |
| 24 | `f3d9809c` | 7.100 s | 6.257 s | 87.691 s | 1,219,899,392 B |

`4a6d17ed` is excluded: an initial harness cap converted its requested 12
workers to 8 because it counted cells rather than seed tasks. The row's
captured `jobs=8` exposes the mistake; `b43f10a2` is the corrected 12-worker
point.

The full row `0b861338`, one run at 16 workers, measures 44.200 seconds
external wall, 43.040 seconds internal wall, 676.239 seconds summed task
time, 15.712 effective concurrency and 1,765,822,464 bytes peak RSS. The
maximum-cap p90 scheduled-wall estimate is 1,465.749 seconds, or 24
minutes 26 seconds. Against Step 2 this is 14.03x faster externally and
14.48x lower on the maximum-cap p90 estimate. The full work remains exact
at 72 cells, 242 seed walks, 7,571,686,367 parents and 8,868,542,328
prints.

### Step 4 - replace the JSON verdict seam with typed values

Implemented in `924c000`. A projected seed now carries `ScreenReduced`,
the exact sufficient statistics Stage A reads: per-hour parent-count
histograms and per-hour/window scheduled counts, zero counts and count
histograms. `ScreenSessionAcc` closes directly into that type and merges
sessions without constructing generic session JSON. The observed artifact
is parsed into the same representation once when `ScreenContext` opens.
Production verdict evaluation no longer constructs an `ObsContext`, and
the cache stores the typed projection rather than `Vec<Value>`.

The implementation deliberately stopped short of the larger
`ProjectionPlan` sketch. Step 2 had already removed retained timestamps
and tree-shaped hot state; a second scheduling rewrite had no measured
justification. `ScreenReduced` is therefore a compact close-time boundary
over the existing dense accumulator, not another projector.

The old JSON and `ObsContext` route remains compiled only under
`cfg(test)` as a differential oracle. Exact verdict JSON matches it for a
passing walk and deliberate A1, A2/A3 and A4 failures. Direct typed close
matches the generic reduced session JSON exactly, including a two-session
rotation, and the eight-seed layer-1 oracle reproduces the committed 12a
generated blocks in both normal and instrumented builds. `brokkr check`
passes 727 normal and 385 instrumented tests.

The single focused allocation comparison is `0d874ef3` against
`29c19b0e`, with identical 126,143,060-parent and 147,738,385-print work.
Exclusive `project_stream` allocation moves from 3.4 GB to 3.0 GB, down
11.8%. Instrumented `project_stream` time moves from 11.74 s to 11.41 s,
down 2.8%; one reading is not used as a broader runtime claim. This is the
expected cleanup-scale result, and no further batch run is owed.

The whole of steps 1 to 4 alters no random draw and no parent timestamp.
It stays entirely in the free lane.

### The combined result after step 3

Steps 1 and 2 moved full-panel single-core task time from 787.144 seconds
to 617.991 seconds, 21.49%. Step 3 then moves external full-panel wall
from 620.200 seconds to 44.200 seconds, 14.03x, while summed task time
rises 9.43% under contention. From Step 0 to Step 3, external wall improves
17.88x and the maximum-cap p90 estimate improves 18.73x, from 27,448.707
seconds to 1,465.749 seconds. The entering 10x to 20x scheduler hypothesis
is confirmed on the real task shape.

### Boundary risks to carry through all four steps

The risk is semantic boundary behavior, not numerical arithmetic:

- a parent before the measurement window with a child inside it,
- a child burst crossing a minute,
- a child burst crossing an open or closed segment boundary,
- a window ending exactly on an hour boundary,
- run and lag-1 resets across hours and segments,
- populated zero-parent minutes,
- session rotation invariants and refusal timestamps.

## The measurement instrument this round needs

A multi-hour edit-measure loop is not an optimization loop. The round needs
two DISTINCT instruments. Step 0 supplied the second.

- A short profiler that says WHERE the time goes. The existing
  `screen_projection` target already does this, at about 15 s.
- A representative batch benchmark that predicts full Stage A cost. The
  committed `stage_a_batch` target now supplies it.

The permanent `stage_a_batch` target measures 790.2 s at the step-0
baseline, 780.2 s after step 1 and 620.2 s after step 2 on one worker. The
landed step-3 scheduler measures 44.2 s on the full panel at 16 workers.

### The workload

A committed, deterministic panel of 72 cells (sized in step 0 below; the
first sketch said roughly 64, which the 54-stratum partition outgrew)
that:

- covers all four families,
- includes both coarse and depth-1 / depth-2 lattice points,
- weights family representation by estimated full-run CPU contribution,
  NOT by raw cell count,
- includes domain corners, centers and a space-filling selection across
  each parameter domain,
- uses two seeds for sampled coarse cells and four for sampled
  refinement cells, exactly as the real screen does,
- always bypasses existing caches, or uses a benchmark-owned cold cache
  under `target/`,
- runs the COMPLETE cell path, projection and verdict reduction
  included, never the arrival walk alone.

At the current 3 s kernel seed and 6 s `EventMarkov` seed costs, a
properly mixed 72-cell panel lands around 13 to 15 minutes serially.

**The panel is fixed.** Never change it because an implementation
performs badly on it.

### An estimator, not a stopwatch

Every sampled cell belongs to a stratum, `family x coarse-or-refinement x
parameter-region`. Each stratum knows how many real Stage A tasks it
represents, how many benchmark tasks were sampled, and the measured CPU
time, parents and prints of each sample. So the benchmark reports:

    estimated_full_serial_seconds =
        sum(stratum_population * mean_sample_task_seconds)

Refinement survivors are not known before the screen runs, so the two
uncertainties are named separately. Population uncertainty is represented
by the MAXIMUM-CAP assumption. Cost uncertainty is represented by the
sampled MEAN or sampled P90. The concrete readings are therefore
`maximum_cap_mean_serial_cpu_s`, `maximum_cap_p90_serial_cpu_s`,
`maximum_cap_mean_scheduled_wall_s` and
`maximum_cap_p90_scheduled_wall_s`. Calling one scalar merely
`maximum_cap_s` would hide which cost statistic it used.

### Parallel scaling, measured separately

The target takes a normal `--jobs`; the shipped command defaults to
available parallelism capped at 16 after the measured SMT knee. No
environment variables. It reports both the sum of individual task
execution times and the overall batch wall time, hence:

    effective_concurrency = task_cpu_seconds / batch_wall_seconds
    estimated_full_wall   = estimated_full_serial_seconds
                            / effective_concurrency

Because refinement has barriers, dividing by 32 is not good enough. The
estimator also REPLAYS the sampled task durations through the Stage A task
graph: populate the full coarse task counts with sampled durations,
simulate the configured worker count, apply the family refinement
barriers, and populate both refinement rounds while spending each
family's cap ONCE across the two rounds. It evaluates every feasible
per-family round split and retains the greatest modeled wall. The current
budget-facing replay preserves the driver's family serialization, making
it conservative relative to the landed cross-family pass scheduler, and
reports tail utilization. Step 3 changes its replay unit from whole cells
to seed walks, matching the production scheduler boundary.

### Required output

Per run, printed and stored: panel identity hash; worker count; cells and
seed walks completed; parents and prints by family and stage; wall time;
sum of task CPU time; effective concurrency; peak RSS; allocation totals
in allocation mode; estimated full serial time; estimated full wall time;
central and conservative estimates; per-family time per parent and time
per seed walk.

Parents and prints are ESSENTIAL, not decorative. A faster run that
accidentally does less work must be visibly invalid.

### How the instruments divide

- `screen_projection --hotpath` - locate hot functions, about 15 s.
- `screen_projection --alloc` - locate allocation volume, about 15 s.
- `stage_a_batch --jobs 1` - real per-core throughput, about 69 seconds
  after step 2.
- `stage_a_batch --jobs N` - scheduler scaling and full-run wall-time
  projection.
- `arrival-screen --cost-probe` - the final contract and budget sanity
  check, and the artifact the exit criterion is read from.

The batch target normally runs UNINSTRUMENTED. Hotpath instrumentation
and allocation counting perturb timing, so those modes are diagnostic and
never the canonical performance number. Registration detail to settle
when it is built: `screen_projection` and `arrival_walk` are registered
in `brokkr.toml` with `features = ["hotpath"]`, which would make the
canonical batch number an instrumented one. `stage_a_batch` should be
registered without it, with the mode axis supplying instrumentation on
demand.

### Validating that the estimate maps

Before the estimate is trusted as a mapping rather than a stopwatch:

1. run a separate validation expansion with at least two independent
   probability samples in every stratum,
2. compare its two per-stratum estimates and bootstrap within strata,
3. require parent and print counts to stay exact across implementation
   changes,
4. eventually compare the estimate against the first real Stage A run and
   retain that calibration.

The fixed 72-cell panel alone cannot perform items 1 and 2 because 44
strata are singletons. Its full row reports that fact explicitly. If the
expanded halves disagree substantially, increase sampling only in the
unstable family or parameter region. Do not lengthen every benchmark
equally.

### Benchmark invariants

Four properties, stated as invariants rather than open items.

#### 1. One panel definition, two tiers

- `quick`: about 8 cells, targeting roughly 2 minutes today.
- `full`: 72 cells, 13 to 15 minutes, containing every `quick`
  task UNCHANGED.

`quick` covers every family and both stage shapes, and reports itself as
a DEVELOPMENT READING rather than an authoritative Stage A estimate: its
sample is too sparse to characterize the refinement envelope reliably.
`full` is the number of record. One execution and estimator
implementation serves both tiers; there is no duplicated quick harness.

The normal optimization cycle: run `quick`, implement, repeat `quick`
while iterating, run `full` when the change looks real, run the
correctness gates independently, and run the cost probe for the
contract-facing number.

#### 2. Never a correctness gate and never a performance gate

The panel MAY fail because its plan is malformed, a task refuses
unexpectedly, the work counters are incomplete, or its recorded identity
is internally inconsistent.

It MUST NOT fail because runtime exceeded an expected number, a sampled
cell's output changed, a throughput threshold was missed, or the sampled
cells happened to agree or disagree with an old row.

Correctness remains exclusively with the layer-1 oracle, the explicit
boundary fixtures, the draw transcripts, parent-for-parent agreement and
the normal check suite. The panel measures an implementation AFTER
correctness has been established, and never establishes it. It equally
cannot certify that unsampled cells are correct: its only extrapolation
is COST.

#### 3. Identity-bearing rows

A cell-list hash is not enough. Define a benchmark-plan hash over: panel
schema version; quick or full membership; family; exact parameter bits;
lattice coordinate and level; coarse or refinement classification; seed
list; population stratum; extrapolation weight; estimator version.

Weights are identity-bearing because changing a weight changes the
estimate even when the executed cells are identical.

Beside that hash, store `TAPE_PROTOCOL_VERSION`,
`ARRIVAL_KERNEL_VERSION`, the measurement input hash, parents, prints,
cells and seed walks. The repository commit and build configuration
already live in the brokkr row, but tape and kernel identity are
semantically independent and deserve explicit columns: a query must be
able to reject incomparable rows without recovering historical source
trees.

#### 4. Only the maximum-cap estimate faces the budget

The primary result reads, conceptually:

    maximum_cap_estimated_wall_s
    stage_a_budget_s
    estimated_budget_margin_s
    estimated_budget_fraction

The maximum-cap estimate assumes ALL frozen refinement capacity is
consumed and must not depend on an expected survivor count. Central and
p90 estimates stay useful for comparing optimization steps, detecting
parameter-dependent slowdowns, estimating the likely real run and
understanding scheduler tail behavior - but neither discharges a ceiling.
The headline budget statement comes from the full-cap task graph and
states which sampled cost statistic was used within each stratum.

Report BOTH a maximum-cap serial CPU estimate and a maximum-cap
scheduled wall estimate at the selected worker count. That separates code
efficiency from scheduler efficiency, and stops parallelism from making
an expensive implementation look intrinsically cheap.

#### Registration and instrumentation

The registry confirms the concern. `mogwai-lab/Cargo.toml` declares
`hotpath = [...]` and `hotpath-alloc = ["hotpath", ...]`, and
`screen_projection_bench` carries `required-features = ["hotpath"]`
precisely so a plain `--examples` build cannot produce a
profiling-named binary that measures nothing. Correct for a profile-only
harness; wrong for the batch target.

`stage_a_batch` should have NO cargo `required-features`, compile and run
with neither profiling feature enabled, be registered with an empty base
feature set, and produce its canonical row from the bare release binary.
The invariant matters more than the syntax:

    bench row:   release, no profiling features
    hotpath row: release, hotpath
    alloc row:   release, hotpath-alloc

A `--bench` row whose metadata says `features: hotpath` is MISCONFIGURED.
If the brokkr target schema cannot add the feature by mode from an empty
base, either invoke the features explicitly or extend the registration
schema.

The full batch is also too long and too broad for useful function-level
profiling. The `quick` tier is the natural `--hotpath` and `--alloc`
workload; the `full` tier stays the uninstrumented number of record.

#### The resulting instrument set

- `screen_projection` - seconds-scale focused profile.
- `stage_a_batch quick` - edit-cadence end-to-end measurement.
- `stage_a_batch full` - checkpoint-cadence number of record and
  stratified estimator.
- `arrival-screen --cost-probe` - the frozen contract-facing budget
  probe.
- the correctness tests - entirely separate.

One role each, so the 72-cell sample cannot slowly become an accidental
substitute for either correctness or the budget contract.

## Medium-value local changes

Worth doing only if they fall naturally out of the staged work, or after
the structural work is benchmarked.

1. Remove dynamic dispatch and heap boxing. Make projection generic over
   the concrete parent source; the boxed enum plus `dyn ParentSource` is
   unnecessary in a loop this hot. Likely low single digits, perhaps
   10 %.
2. Replace tree-shaped time buckets with dense or cursor-driven storage.
   Even an interim `Vec<u32>` indexed by minute beats
   `BTreeMap<u64, ...>`; the online projector is stronger. Subsumed by
   step 2.
3. Cache session resolution by minute. Children overwhelmingly share
   their parent's minute. A monotonic precomputed segment cursor is the
   better final design, so this is an interim measure only.
4. Use a linear Wasserstein merge. `composition_loss` rebuilds CDF totals
   by scanning each side for every support point; a two-pointer
   cumulative merge is O(n + m). Verdict time is small, so not first
   order.
5. Stop allocating in `ordered()`. `SessionAcc::ordered` builds a
   `Vec<&SegmentAcc>` repeatedly for exactly two segments. Disappears
   once Stage A leaves `SessionAcc`.
6. Move invariant setup out of each seed: warmup parsing, profile scalar
   cloning, size-grid construction, offset resolution and configuration
   validation are per-cell or per-context work.
7. Clean stale caches once per run rather than per cache write. Completed
   in step 3.
8. Use a compact typed cache encoding. Matters for cold-cache write
   volume and warm-cache startup, but only after the JSON session payload
   is gone.
9. Consider `target-cpu=native` codegen AFTER the rewrite. Release
   already uses opt-level 3, fat LTO and one codegen unit. It may help
   the numeric draw path, but the current hot loop is dominated by
   control flow, division, trees and allocation, so it must not lead.

## Draw-path opportunities that should wait

Real opportunities exist inside the arrival machinery, and this round
should not enter that lane:

- cache the neutral scaled `SweepShape` instead of reconstructing it per
  parent,
- reduce repeated calendar and session modulation checks in `ArrivalEnv`,
- build a specialized `EventMarkov` screen walk consuming only
  arrival-relevant state,
- separate arrival RNG state from price and size state so family 1 can
  skip irrelevant evolution,
- batch or SIMD several family cells.

The three kernel families dominate the grid and the profile says their
ATTACHED MEASUREMENT is the problem; `EventMarkov` is a minority of the
modeled total cost. Changing draw machinery also carries transcript,
kernel-version, tape-version and amendment consequences, per the
amendment lane above. Drive measurement overhead near the bare-walk
floor, parallelize the independent work, then profile again. Only if
draws then dominate should the arrival path be rewritten and re-blessed.

## Rider

`rust-version` is bumped to 1.99 (nightly toolchain in use), which puts
the stabilized algebraic float operators (`algebraic_add` and siblings,
stable from Rust 1.98: float ops the compiler may reorder and vectorize)
in reach for the free lane.

## Exit criterion

The owner looks at a fresh `--cost-probe` and the implied total and says
so. Then brick A runs on the optimized binary, per the 12b spec item 2.

The `--cost-probe` remains the contract-facing number. The batch panel's
maximum-cap estimate is how the round STEERS toward it; it does not
replace it, and the exit criterion is not restated in terms of the panel.
