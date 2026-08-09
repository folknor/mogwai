# Start here

The entry point. Read this, then the ONE document that drives the track you
have been asked to work. It exists because the alternative is what keeps
happening: an agent reconstructs the shape of the work from `todo.md` plus
whichever notes it happens to open, gets it three-quarters right, and writes
the quarter that is wrong into three more documents.

Notes-class, like everything else here: transient, no truth guarantee, nothing
durable may cite it. Its own end state is deletion - see the last section.

**Keep it short.** `DATA-PURCHASE-REPORT.md` was meant to live three hours,
grew to 2,687 lines, and was consolidated back to one page on 2026-08-09.
This file is a map. If it starts explaining things instead of pointing at
them, it has failed.

## The arc, and the end goal

One continuous piece of work, not a set of projects: make mogwai generate a
REALISTIC tape for whatever instrument gets traded next, and serve it over the
live path so a strategy can be forward-tested against it.

THE END STATE, stated here because `reference/` documents what IS while this
is what is INTENDED: on the order of 200 agents running concurrently, each
developing a strategy through broadarrow - backtest, optimize, Monte Carlo -
and then FORWARD-TESTING it against mogwai, accelerated, fire-and-forget,
one venue instance per run, reproducible by seed. Resource cost is
explicitly not a design input. The settled premises behind that (always
accelerated, no restart or resume, single-instrument strategies, one MOGWAI
venue) are recorded in `todo.md`'s PROBLEM STATEMENTS block.

WHY THE TAPES: a backtest optimizes against the one path that actually
happened, so its edge is always suspect of being memorized. A fitted
generative tape supplies unlimited counterfactual months drawn from the same
measured process - a DISTRIBUTION of realistic worlds, a fresh path per
seed, each reproducible and bindable to a claim. Every un-fitted quantity is
a direction in which the sampled worlds silently stop resembling the real
one, which is why realism is a measured, gated property and why fake tapes
(the ETH/SOL aliases) get cut rather than kept.

WHY THE PRESET KNOBS: the generator's end state is pure instrument-agnostic
method, with every instrument-specific fact living in a preset as a named,
provenance-carrying value. Three jobs, per the settled parameterization
ruling (recorded in `todo.md`): COMPLETE PARAMETERIZATION - everything is
per-instrument in principle, so onboarding instrument N is the intake
sequence terminating in a TOML file, zero method edits; LANDING SITES -
each knob is where a measurement lands, and evidence is only bought when a
knob exists to receive it; AUDITABLE HONESTY - every knob carries
fitted/derived/declared provenance, so a preset is a claim ledger stating
which parts of its tape rest on measurement and which on assertion.

Everything below is a phase of that. The current instrument set - MNQ, MES,
BTCUSDT - is where the work has reached, not where it stops.
That premise is load-bearing and is stated durably in `AGENTS.md` and
`reference/architecture.md`; several assessments have been made wrong by
assuming the corpus is closed.

## Where the work stands, 2026-08-09

Two live tracks and one completed. No document sits above them, which is what
this file is for.

| Track | Driven from | State |
|---|---|---|
| Evidence and purchase | `DATA-PURCHASE-REPORT.md` (root) | Idle. The ~30 dollar ES/MES purchase is the named next marginal dollar, owner-gated. Nothing to buy for 12b. |
| Generator fidelity, protocol 12b | `protocol-12b-arrival-composition-spec.md` (frozen contract of record) plus `protocol-12a-measurement-spec.md` (the binding ladder it re-runs) | Bricks F, K, B4, N, A0 and A are landed (through `9e074d4`). The negative control FAILED as predicted, so the loop proceeds. Item 2's full Stage A run is outstanding, currently STOPPED on the A0/A probe's finding that the integrated frame runs ~6 percent hot on mean parent rate (a structural gap-vs-rate convention mismatch), which voids the kernel families against gate A2 - a section 17 amendment awaiting the owner's ruling. |
| Python-to-Rust rewrite | git history (its phase records, dossier and script triage are retired) | **DONE.** Signature renewed 2026-08-08 at `c783d5f`; the Python retired to the gitignored `research/dead/`; the Rust is the reference. Nine review passes, eight refusals. What survives durably: the parity contract in `reference/architecture.md`, the version exemption in `AGENTS.md`, the two approved numerical deviations pinned by discriminating tests in the code. |

`todo.md` is the catch-all beneath the tracks: open work items, parked
investigations, the hardcoded-value inventory.

## Which document drives what

- **`protocol-12b-arrival-composition-spec.md`** - the contract of record for
  the live work. Its section 0 index partitions the remaining bricks; its
  frozen constants, gates and verdicts bind every sub-spec. Amendments go
  through section 17, never edits.
- **`protocol-12a-measurement-spec.md`** - 12b's judge. The ladder re-runs
  unamended against it. Binding. (It cites the retired protocol-11 spec as its
  spawn point; that citation resolves to git history.)
- **`stage-a-optimization.md`** - the performance round preceding brick A:
  baseline, call chain, the free-lane/amendment-lane constraints, gates,
  exit criterion.
  (Its one surviving benchmarking obligation is the ordering rule in item 2
  below. The two-layer design that governed this round was RETIRED 2026-08-10 -
  see `benchmarking-design.md`.)
- **`benchmarking-design.md`** - the addressing scheme for measuring mogwai,
  replacing the two-layer design and the frozen `[mogwai.workloads.*]` registry.
  Written for every operational surface rather than for Stage A, because the
  200-instance end state makes every instance-level cost a multiplier. Dies when
  the registry, the reading rules and the document split have landed.
- **`protocol-landings.md`** - the consolidated record of protocols 8, 10 and
  11: what landed, the verdicts, and the obligations later work inherited.
  Replaces five retired per-protocol documents whose full text is in git
  history.
- **`sampling-frame-preregistration.md`** - the rejected sampling-frame method,
  kept as a frozen record. Section 7.1 is the part people miss: only the
  `rv`-rank association was tested; the five-feature farthest-point selection
  was never on trial.
- **`pair-test-preregistration.md`** - `pair_harness.py` loads its frozen JSON
  twin and is still the live judge for delivered pair data.
- **`bugs-*.md`** - six per-crate bug inventories.

## What blocks what, right now

1. **12b item 2 (the Stage A screen run) is UNBLOCKED, 2026-08-09.** The
   frame-calibration finding was diagnosed (`ARRIVAL_MEAN_CAL`
   double-application in the integrated frame), amended through the freeze
   protocol (signed, codex session 019fe781), and landed: bare mean for the
   kernel families, `ARRIVAL_KERNEL_VERSION` 2, `TAPE_PROTOCOL_VERSION` 12
   (Brick S renumbered to 13, with the coordinated 12a sections 8/11
   amendment). The A0 probe on the amended tree passes its budgets with the
   excess eliminated. Before the full screen run: the owner-defined
   OPTIMIZATION ROUND below.
2. **A Stage A performance round precedes brick A** (owner step,
   2026-08-09): the screen is priced at ~9.9 h and the measured hotspot is
   the `SessionAcc` projection, not the draw. `stage-a-optimization.md` is
   the work item - entry points, the free-lane/amendment-lane constraint
   split, gates and exit criterion. The round needs almost none of the
   benchmarking scheme: profile one kernel screen cell (the cost probe needs
   no clean tree, so it is the benchmark loop as-is, and
   `screen_projection_bench` is the instrument) and CONFIRM the hypothesis
   that `SessionAcc` bookkeeping dominates `project_stream`. Only then
   optimize, in the free lane - a lean screen-side accumulator producing
   exactly the fields the screen reads, held honest by the layer-1 oracle
   test's exact integer reproduction - staying out of `arrival.rs`'s draw
   path unless the profile forces it. Targets: kernel cells at ~6.0-6.4 s,
   family 1 at ~11.8 s, ~9.9 h implied total; anything that gets cells
   under ~2 s makes the refinement-budget debate in `todo.md` moot. Brick
   A runs when the owner is satisfied with a fresh cost probe.
3. **12b item 3 (Stage B, the landing, protocol 13) waits on item 2's
   verdict.**
3. **Standing owner rulings that reshape the ground, 2026-08-09**: the
   bit-exactness era toward Python-era artifacts is CLOSED (re-bless instead
   of preserving CPython arithmetic; determinism per binary plus green
   statistical gates is the contract), and RUNTIME COST is a first-class
   concern - a multi-hour computation is presumptively something to optimize
   before it is run. This bears directly on the Stage A budget question in
   `todo.md`.

## Traps this repo has actually fallen into

Cheap to state, expensive to rediscover.

- **Assuming the corpus is closed.** It made three separate script assessments
  wrong. Every new instrument re-runs the whole intake sequence.
- **Trusting an inventory instead of the tree.** Point-in-time lists drift.
  Verify against the tree before acting on a list.
- **Citing a frozen artifact by vibe.** `targets-frozen.json` was described as
  `select_windows.py`'s gate in three documents; it is the BTCUSDT
  microstructure target set and that script never touches it.
- **Reading a summary and calling it the source.** The purchase report's
  summary of the sampling-frame verdict got its scope wrong for days; the
  preregistration's sections 7.1 and 8 are what actually scope it, and they
  say something no summary carried. Read the deciding document, not the
  document that cites it.
- **Verifying a fix against the artifacts instead of the contract.** The
  rewrite's recurring failure shape: a closure that held on the committed
  corpus and failed one layer below where its gate looked. A bound established
  over the fixtures you happen to have is not a bound; a test that passes is
  not evidence until its reference is independently established; a claim about
  what a user sees is checked against the built binary, not the source.

## The end state of these documents

All of `notes/` resolves to deletion. When the arc lands, the implemented work
and the procedures speak for themselves, with one or two `reference/` documents
explaining the procedure - the intake sequence a new instrument walks, which
`reference/architecture.md` already carries in outline.

So the test for anything written here is: does it die cleanly? A note that has
to survive is a note that belongs in `reference/` or in a code comment, and
`todo.md`'s standing rule already says so - when an item completes it is
REMOVED, and whatever must endure moves to `reference/` or the code first.

This file dies with the rest.
