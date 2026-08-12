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
| Arrival successor | `successor-contract.md` | **LIVE, signed 2026-08-12.** The staged contract toward a plausible MNQ tape at all intervals: Stage M design measurement, Stage F freeze, Stage I implementation, Stage C single confirmation on sealed June 2026 (May 2026 reserve). Four-arm intervention study; acceptance belongs to the joint arm. Blocked on the data pull below. |
| Evidence and purchase | `DATA-PURCHASE-REPORT.md` (root) | **ACTIVE.** Owner authorized one Databento Standard subscription month (199 USD, 2026-08-12), superseding credit-first for this purchase. The successor contract's manifest and seal ledger govern the pull; the ES/MES months ride along sealed for the MES-borrow track, which needs its own contract. |
| Generator fidelity, protocol 12b | `protocol-12b-arrival-composition-spec.md` (its section 16.1 carries the RESULT) | **CLOSED 2026-08-11** with `no-arrival-admissible-candidate-in-frozen-search-space`. Unlike the 2026-08-10 run of the same verdict string, this one is a measurement: A4 and the A2 level limb were defective and are repaired, A2 now admits 618 cells and A1 57. A3 fails all 1,402 cells of all five families, including the right-skewed shot-noise family added for exactly that reason. TWENTY cells fail A3 and nothing else. Section 11 forbids amending A3 having seen that, so the finding is exploratory evidence for a successor, not a 12b repair. |
| Python-to-Rust rewrite | git history (its phase records, dossier and script triage are retired) | **DONE.** Signature renewed 2026-08-08 at `c783d5f`; the Python retired to the gitignored `research/dead/`; the Rust is the reference. Nine review passes, eight refusals. What survives durably: the parity contract in `reference/architecture.md`, the version exemption in `AGENTS.md`, the two approved numerical deviations pinned by discriminating tests in the code. |

`todo.md` is the catch-all beneath the tracks: open work items, parked
investigations, the hardcoded-value inventory.

## Which document drives what

- **`successor-contract.md`** - the contract of record for the live work:
  staging, immutable data roles, seal ledger, gate shapes, outcome and
  amendment rules. Each stage writes its own dated preregistration under it.
  Signed by codex session 019ff4db.
- **`stage-m-preregistration.md`** - Stage M frozen and signed (same session,
  four rounds): the per-month measurements bound by reference, the
  calendar-adjusted exchangeability test with its pre-read power analysis,
  and the Tier 2 projection feasibility program with its numeric admission
  hurdle. Runs once the design months are delivered and sealed.
- **`count-curve-preregistration.md`**, **`ordered-counts-preregistration.md`**,
  **`slow-geometry-preregistration.md`** - the three post-12b measurements the
  successor contract binds as established evidence, each carrying its RESULT.
- **`protocol-12b-arrival-composition-spec.md`** - CLOSED; section 16.1 carries
  the result the successor inherits. Its section 0 index partitions the
  retired bricks; amendments went through section 17, never edits.
- **`protocol-12a-measurement-spec.md`** - 12b's judge. The ladder re-runs
  unamended against it. Binding. (It cites the retired protocol-11 spec as its
  spawn point; that citation resolves to git history.)
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

## What blocks what, right now

1. **The successor contract is signed and Stage M is blocked on the data
   pull.** The owner subscribes, the manifest is pulled while every month
   is inside the rolling L1 window, roles bind at delivery per the seal
   ledger, and only then may a Stage M preregistration freeze and run.
   The calendar-adjusted persistence test freezes BEFORE any design month
   is read.
2. **12b is CLOSED**; its full result is that spec's section 16.1, and
   the twenty-cell A3 residue is exploratory evidence the successor may
   read qualitatively but never derive a constant from. Stage B and the
   protocol-13 landing never ran and never will under the frozen 12b
   contract. The performance round is retired to git history; its durable
   measurements are in `reference/performance.md`.
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
