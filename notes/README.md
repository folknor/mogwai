# Start here

The entry point. Read this, then the ONE document that drives the track you
have been asked to work. It exists because the alternative is what keeps
happening: an agent reconstructs the shape of the work from `todo.md` plus
whichever notes it happens to open, gets it three-quarters right, and writes
the quarter that is wrong into three more documents.

Notes-class, like everything else here: transient, no truth guarantee, nothing
durable may cite it. Its own end state is deletion - see the last section.

**Keep it short.** `DATA-PURCHASE-REPORT.md` was meant to live three hours and
is now 2,676 lines. This file is a map. If it starts explaining things instead
of pointing at them, it has failed.

## The arc

One continuous piece of work, not a set of projects: make mogwai generate a
REALISTIC tape for whatever instrument gets traded next, and serve it over the
live path so a strategy can be forward-tested against it.

Everything below is a phase of that. The current instrument set - MNQ, MES,
BTCUSDT, ETHUSDT, SOLUSDT - is where the work has reached, not where it stops.
That premise is load-bearing and is stated durably in `AGENTS.md` and
`reference/architecture.md`; several assessments have been made wrong by
assuming the corpus is closed.

## Where the work stands, 2026-08-08

Three tracks. No document sits above them, which is what this file is for.

| Track | Driven from | State |
|---|---|---|
| Evidence and purchase | `DATA-PURCHASE-REPORT.md` (root) | Idle. The ~30 dollar ES/MES purchase is the named next marginal dollar, owner-gated. Nothing to buy for 12b. |
| Generator fidelity, protocols 10 to 12b | the four protocol specs below | 12a landed `no-family-eligible`; owner ruled 12b targets ARRIVAL COMPOSITION, repair-until-measurable. 12b spec undrafted, gated on codex capacity. |
| Python-to-Rust rewrite | `rust-rewrite-phases.md` | TWO review passes, both REFUSED, 2026-08-08. The first pass's five blockers and the second's four are all closed; gate at 682 passed, 0 orphaned. Both judgement calls - the lateness quarantine and the version exemption - were accepted by the second pass. Awaiting a third. |

`todo.md` is the catch-all beneath all three: open work items, parked
investigations, the hardcoded-value inventory.

## Which document drives what

- **`rust-rewrite-phases.md`** - the rewrite program, phase by phase, with
  every landing record. 4b's scope lives here.
- **`rust-rewrite-review-dossier.md`** - the map the codex review pass is
  pointed at. Gates, pinned cross-language conventions, the four ruled owner
  decisions. NOT a plan; a review packet.
- **`python-script-triage.md`** - per-script scope rulings. Its inventory is
  STALE by design: commit `9170f45` already executed its DEAD verdicts.
- **`protocol-12a-measurement-spec.md`** - 12b's contract. The ladder re-runs
  unamended against it. Binding.
- **`protocol-11-session-repair-spec.md`** - landed, but its Brick V amendment
  makes the hourly 60s/300s wall-time bands HARD GATES for 12b. Binding.
- **`sampling-frame-preregistration.md`** - the rejected sampling-frame method.
  Section 7.1 is the part people miss: only the `rv`-rank association was
  tested; the five-feature farthest-point selection was never on trial.
- **`pair-test-preregistration.md`** - `pair_harness.py` loads its frozen JSON
  twin and is still the live judge for delivered pair data.
- **`mnq-tbbo-fit-spec.md`**, **`mnq-generator-successor-spec.md`**,
  **`mnq-session-fit.md`**, **`mnq-session-refit-report.md`** - landed. Their
  spec bodies are SPENT; only their RESULT sections still carry weight, as the
  evidence behind values currently shipping.
- **`bugs-*.md`** - six per-crate bug inventories.

## What blocks what, right now

1. **4b's remaining work is unblocked**: per-mode subcontract scoping, the
   `select_windows.py` and `tick_composition_ratios.py` absorptions, and the
   TBBO short-row fix. The five signature blockers the review raised - the
   `characterize` CLI gap, the session-profile CLI gap, the `cadence-feasible`
   Markov gate and its fail-open decoding, the fingerprint
   float/ordering/fail-open trio, and the red `--gate` - all closed
   2026-08-08. Full 4b order in `rust-rewrite-phases.md`.
2. **12b is blocked on codex capacity**, and may not be drafted outside the
   freeze protocol.
3. **The codex program-level review** still gates 4b's retirement half. Two
   passes have refused. The lesson both times, and worth reading before
   assuming a third will pass: every finding was a closure that held on the
   committed corpus and failed one layer below where its gate looked, so
   verifying a fix against the artifacts is not verifying it against the
   contract. The Python originals must stay runnable until it passes; every
   parity gate loses its reference the moment they move.

## Traps this repo has actually fallen into

Cheap to state, expensive to rediscover.

- **Assuming the corpus is closed.** It made three separate script assessments
  wrong. Every new instrument re-runs the whole intake sequence.
- **Trusting an inventory instead of the tree.** The triage's script list, the
  hardcoded-value catalogue, and this file's own tables are all point-in-time.
  Verify against the tree before acting on a list.
- **Citing a frozen artifact by vibe.** `targets-frozen.json` was described as
  `select_windows.py`'s gate in three documents; it is the BTCUSDT
  microstructure target set and that script never touches it.
- **Reading a summary and calling it the source.** `DATA-PURCHASE-REPORT.md`
  section 7.2 summarizes the sampling-frame verdict; the preregistration's
  section 7.1 and 8 are what actually scope it, and they say something the
  summary does not.

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
