# Benchmarking design

The addressing scheme for measuring mogwai. Supersedes the two-layer design
that governed the Stage A round, and the `[mogwai.workloads.*]` registry that
design put in `brokkr.toml`.

Notes-class: transient, no truth guarantee, nothing durable cites it. Its death
condition is at the bottom.

## The premise

Everything in mogwai gets benchmarked thousands of times, across every
operational surface, for as long as the project exists. Stage A is the problem
directly in front of us; it is not what the infrastructure is for.

The end state is on the order of 200 venue instances on one host. That is a
MULTIPLIER on every instance-level cost, not a workload of its own - a
megabyte of steady-state RSS is 200 MB, a percent of the draw loop is two
cores. Measuring one instance well is measuring the end state. It is why the
grinding is justified, and why the measuring apparatus has to outlive the
round it was built during.

## What is brokkr's problem, not ours

Capture. Wall clock, peak anon RSS, allocation churn, the /proc timeline,
markers, counters, the results and sidecar stores, the query surface, the
comparison flags, how `brokkr <project>` parses its own arguments. All of it
exists, is shared across projects, and mogwai inherits it. None of it is
designed here.

## What mogwai owes

ADDRESSING. What is the name of a thing you can bench, such that every
operational surface has one.

The `[mogwai.workloads.*]` registry answered this for four hand-written
invocations. The surface it needs to cover is the whole product of commands,
presets, windows, seeds, cells and flag axes, plus every library-level loop
that has no command at all. That product cannot be enumerated, and a registry
that must be edited before a new question can be asked will not survive a
decade of asking them.

pbfhogg is the precedent: roughly 25 commands across several flag axes and ten
datasets, and its `brokkr.toml` registers ZERO workloads. It registers inputs.
Invocations are composed at the call site and captured verbatim, and pairing
rows is a query rather than a name lookup.

## The surfaces

Two kinds, and the split is what the two-layer design was groping at.

ARGV-SHAPED, through the shipped bin: `gen` and its `--type` variants,
`tick-composition`, `preflight`, `measure`, `fit`, `cache`, `synth`,
`arrival-screen`. A process, an argv, a wall. Benching these through
`target/release/mogwai` measures what ships, startup and argument parsing
included, which is the honest end-to-end number.

HARNESS-SHAPED, through an example target: the engine's matching loop and
divergence seam, the `TickSource` implementations, the arrival draw, the
screen's projection, and eventually the serving path and the adapter. These
have no command line, so there is nothing for an argv registry to hold. The
harness is the addressable thing.

The second kind is the MAJORITY of the eventual surface. Registering only the
first kind is what forced everything else into "layer 2", which was never an
architecture - it was an escape hatch with a name.

## The design

The registry holds targets and their feature shapes, plus out-of-git inputs.
Nothing else.

```toml
[mogwai]
package = "mogwai-cli"
bin = "mogwai"

[mogwai.targets.arrival_walk]
package  = "mogwai-data"
example  = "arrival_walk_bench"
features = ["hotpath"]

[mogwai.targets.screen_projection]
package  = "mogwai-lab"
example  = "screen_projection_bench"
features = ["hotpath"]

[bygg.datasets.mnq-tbbo-july]
path   = "research/market-data/databento/mnqv/2026-07.full.tbbo"
xxh128 = "280ade40376bd49f50c579bb127f3fbd"
```

- CLI surfaces need no registration. The bin is registered once; the argv is
  composed at the call site.
- Harness surfaces resolve by name against `[mogwai.targets.*]`. Adding a
  surface to the measurable set is registering a target, which is the work you
  were going to do anyway the moment you wanted to optimize it.
- Harnesses TAKE AN ARGV, like the bin. Every surface here is config-shaped -
  preset, window, seed, cell - so an argument-free harness becomes a new
  registry entry per shape, which is the enumeration trap again at one remove.
- Datasets record out-of-git inputs per host: which delivery, and whether the
  file under that path drifted. Not a substitute for the run's own content
  verification, which asks a different question.
- `--bench`, `--hotpath` and `--alloc` apply uniformly to both kinds. One row
  shape, one query surface, no layers.

The exposure this accepts: a harness with an argv can be invoked in a shape
nobody meant, and no registry entry prevents it. pbfhogg carries the same
exposure across every command it has. What answers it is not a config
constraint but the captured argv - an invocation that is not comparable is
VISIBLE in the row rather than prevented, and reading rules are what turn that
into a verdict.

## What is removed, and why

From `[mogwai.workloads.*]`:

- `timing` / `timing_reason`. Redefined what the `elapsed` column MEANS, per
  workload, so one row's elapsed was an external wall and another's was the sum
  of three internal phases with setup excluded, and nothing in the row said
  which. The stated goal - the measured phases and nothing else - is what
  markers and `brokkr sidecar --durations` already produce, with the excluded
  setup remaining visible as its own phase instead of being deleted from the
  record.
- `expect_seconds`. An estimate the first stored row supersedes. The history is
  the expectation.
- `runs`. Already a call-site flag everywhere else, because how much time to
  spend is a decision made on the day.
- `identity_counters`. The question is real - did the work size move under me -
  but undeclared counters are captured and diffed anyway, so the key only
  controlled fatality, and for the walk and screen surfaces a moved count on a
  frozen argv is a tape change, which owes a `TAPE_PROTOCOL_VERSION` bump
  unconditionally.
- The `{corpus}` token and its expansion. Datasets, under a different name and
  without the query surface.
- `successor` and name-as-promise. The DB pairs rows on the captured argv,
  which is stronger than a name because it cannot lie. `--grep` and `--grep-v`
  select arms, including the arm distinguished by an absent flag.
- The two-layer framing itself, and with it the claim that
  `--hotpath` / `--alloc` are broken. They recorded rows without profiles
  because the registry named a bin with no feature shape attached. A target
  plus its features is the fix.

`screen_projection_bench` existing as a separate ms-scale target is CORRECT and
survives. The arrival draw is precisely the annotation shape that drowns a
profiler - dellingr ships a second ms-scale file per workload for exactly this
reason, and pbfhogg keeps its instrumentation sparse to avoid it. That is a
property of the target and belongs in the reading rules; it was the profiler
constraint's promotion into an architectural layer that was wrong, not the
harness.

## Deferred, deliberately

- THE SERVING PATH. Designed for, not measured yet. It is harness-shaped and
  fits the scheme when it arrives; excluding it on principle, as the retired
  registry did, would have written off the class the entire end state lives in.
- MOGWAI'S READING RULES. pbfhogg's are load-bearing and almost entirely
  inapplicable here: it is I/O bound, so its error model is drive state, trim
  debt and page cache. The screen and the walk are CPU and RNG bound, so the
  variables are host quiet, frequency and thermal state, core count and
  allocator behaviour. The `tape_lateness_under_acceleration` failure at 311 ms
  p99 under a load average of 1.46, currently an open item in `todo.md`, is the
  first data point in that record rather than an annoyance.
- THE DOCUMENT SPLIT. pbfhogg carries current state and history separately,
  every number pinned to a UUID, with refuted experiments written down at the
  same weight as wins. Over a decade of grinding the most expensive thing is
  re-running a refuted experiment. Worth copying; not needed to unblock Stage A.

## Relationship to Stage A

Stage A needs almost none of this. It needs `screen_projection_bench` under
`--hotpath` to confirm the `SessionAcc` hypothesis, then a before-and-after
pair. Gutting the registry unblocks it immediately; the target registry, the
datasets and the document split land alongside the round rather than in front
of it.

## Death condition

This file dies when the scheme is implemented and the surviving statements have
moved: the registry into `brokkr.toml` beside the values it governs, the
invocation surface and the reading rules into `reference/performance.md`, and
the pointer in `CLAUDE.md` shrunk to name them. Nothing here needs to outlive
that.
