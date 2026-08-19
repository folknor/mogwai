# Test hunt: mogwai-lab and mogwai-cli unit tests

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope: every
`#[cfg(test)]` module under `crates/mogwai-lab/src/` and
`crates/mogwai-cli/src/`. `mogwai-lab` had never been covered by any previous
hunt; this is the first time anything has looked at it.

This hunt looks for defects in the TESTS, not in the code they test.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

ROUND 1 CLOSED A1, C1 AND C3 on 2026-08-19; ROUND 2 CLOSED A2 AND A3 the same
day; ROUND 3 CLOSED A4, A5, A6, A7, A8 AND A9, so section A is empty and gone.
ROUND 4 CLOSED B1 AND B3 and REFUSED B2 on measurement, so what remains of
section B is that refusal. B1 TOOK TWO PASSES: the round's first cut of the
dispersion arm skipped it whenever EITHER sample's variance was zero, which
left the self-widening defect live in precisely the degenerate rows the round
had itself documented, and its self-proof probe was guarded by the same
condition so it could never have shown it. The cold review caught that; the
floor that closed it, and the cheap unignored predicate test that now pins it,
are in the carry-forward.
Those sections are gone from this
document; what the rounds decided that a later reader would otherwise re-derive
wrong is in `notes/bug-loop-carry-forward.md`.

## B. Self-widening tolerances (statistical gates that get MORE permissive as the code gets worse)

**B2. REFUSED, and the finding's own conditional is what refuses it.** The
report said: "if the plug-in SE at M=2000 is much tighter than that, the cap
never binds and the test is fine; if it does bind, the test is nearly useless.
MEASURE WHICH." Measured, over all 36 comparisons the gate makes: THE CAP NEVER
BINDS. The plug-in arm is the binding one in every cell and every hour, and it
sits between 0.15x and 0.59x of the `0.5 * closed` cap, so the cap would need
the plug-in SE to inflate by 1.7x before it could take effect at all. The
observed deviations use 0.01 to 0.43 of the tolerance actually applied.

Tightening the cap to `0.1 * closed`, as the report suggested, would therefore
NOT tighten a loose gate - it would REPLACE a healthy plug-in arm with a fixed
band in most cells, since `0.1 * closed` is below the plug-in arm nearly
everywhere (0.10 vs 0.157 of `closed` in the domain-centre `wall_mmpp` cell, for
instance). The gate would go from a tolerance sized by the run's own Monte Carlo
error to one that ignores it, and its margin against the observed deviation
would shrink from about 5x to about 3x for no gained sensitivity to any defect.
That is the tape document's round-4 lesson exactly: a principled-looking
correction that makes the gate strictly more fragile. The cap and the arm are
also frozen by the 12b spec section 9.7, which reasons them out explicitly.

The two arms are now PRINTED per comparison (`conformance_arms ...
binding=plugin slack_ratio=...`), so the claim "the cap never binds" is
re-checked by every run rather than resting on this note.

## C. Skip-list and `#[ignore]` reconciliation

**C2. Cost-reason `#[ignore]`s that are correctly listed.** For the record, these
check out: `the_envelope_matches_the_closed_forms_where_they_are_exact` (exact
name), the ten macro-generated
`the_envelope_simulator_is_faithful_to_the_candidate_walks_*` (covered by the
prefix, and the prefix invariant holds since every one is generated `#[ignore]`d
by the macro), `arrival_screen_layer1_reproduces_the_committed_12a_generated_blocks`,
and `arrival_control_refuses_a_tree_that_changed_during_the_run`.

## D. Shared-fixture convention

The brief asked whether cross-implementation gates use a shared,
independently-stated JSON fixture. In this scope: NONE DO. The convention
(`analysis/spread_conformance.json`, `analysis/dwell_conformance.json`) has zero
instances here. Every two-implementation comparison in scope derives one side
inside the test:

| Gate | Location | Shape |
|---|---|---|
| `summary_matches_an_independent_tick_walk` | `gen.rs:2207` | Test collects the same seeded stream into a `Vec` and recomputes every field with index scans |
| `minute_ranges_match_an_independent_bar_pass` | `gen.rs:1526` | Same |
| `measure12a_matches_independent_recompute` | `measure12a/tests.rs:277` | Same |
| `session_segment_at_agrees_with_mogwai_lab` | `gen.rs:2567` | Two production implementations against each other, no third party |
| `the_zero_fraction_matches_the_count_hist` | `arrival_control.rs:869` | Test recomputes the ratio from the committed artifact |
| `the_lab_walk_matches_the_measure_exposure_contract` | (cli tests, out of scope) | Two production copies against each other |
| `close_reduced_agrees_with_close_on_block1_and_block2` | `measure12a/tests.rs:654` | Two production paths against each other |

The first three are LEGITIMATE DIFFERENTIAL TESTS - the recomputation is
structurally different from production (streaming vs collect-and-index), which is
the acceptable form, and they are good tests. The hunter would not rewrite them.

The last three are the shape the convention exists to guard: two production
implementations pinned only against each other. The `AGENTS.md` warning applies
verbatim - "if the two conventions drift the gate silently compares different
quantities and still passes." `session_segment_at_agrees_with_mogwai_lab` is
explicitly a temporary bridge ("phase 1 does NOT rewire this crate onto it yet"),
so it is fine and should die when the rewire lands. But
`the_zero_fraction_matches_the_count_hist` and the exposure-contract pin are
permanent two-copy gates with no external anchor. If either quantity's DEFINITION
moves, both sides move together and both stay green. These are the two candidates
for the third `analysis/*_conformance.json`, and per the standing note, NOTHING
WILL DETECT THAT THE FIXTURE IS MISSING - it is caught by this habit or not at
all.

## E. Fixed paths and parallel execution

At `test_threads = 8` the hunter checked every hardcoded path for collisions. NO
TWO TESTS SHARE A WRITE PATH. The naming is disciplined
(`target/stage-a-{dirty-tree,zero-jobs,cost-probe,wall-budget,rss-budget,budget-within}-test.json`,
`target/arrival-control-{test,b5,empty-baseline,midrun}`,
`target/{gen,walk}-scratch-configs/<distinct-name>`,
`target/char-loader-tests/<pair>`). Three notes:

**E1.** `mogwai-lab/src/cadence.rs:548` uses `std::env::temp_dir()` - the only
place in scope that writes outside the project tree, against the standing
convention, and it does not consult `CARGO_TARGET_TMPDIR` the way
`storage.rs:320` does. It is keyed on `process::id()` only, so it is safe today
but not safe against a second cadence test being added later. Should use the
`storage.rs` helper's shape.

**E2. CLOSED, and it was never open.** The finding read
`arrival_control_refuses_a_tree_that_changed_during_the_run`'s root-level probe
file as leaking on a panic and recommended a drop guard. The drop guard is
already there and was already in HEAD - `struct Sweep(PathBuf)` in
`arrival_control.rs` - so the cleanup is unconditional today. The
sibling-behaviour half is gone too: round 2 put the four dirty-tree tests behind
an injected tree reader, so a leaked probe could no longer change what any of
them assert even if one leaked. The test keeps its `#[ignore]`, its clean-tree
precondition and its drop guard, and it deliberately stays OFF the seam - the
LOCATION of the probe is its mechanism, so scripting the reading would pin it
against itself.

**E3.** No test in scope sets an env var, touches a global logger, or asserts a
memory budget outside the injected-reading `BudgetGuard::scripted` harness -
which is a genuinely good design and was the model for A2's fix.
`budgeted`/`BudgetGuard` showed this codebase already knew how to inject an
ambient reading; the tree-state checks got the same treatment in round 2.

## F. Things the hunter read that are worth saying are good

So the report is not read as uniformly negative:
`arch_coefficients_match_the_shipped_recursion` (`gen.rs:1806`) recovers three
parameters by least squares from a real walk and then PROVES ITS OWN SENSITIVITY
by perturbing each coefficient 0.1% and asserting the residual bound breaks -
that is the standard the rest of the statistical gates should be held to, and it
is the antidote to B1/B2. The `stage_a_batch` manifest suite re-derives the
committed plan from the committed pilot and mutates three separate fields to
prove each is load-bearing. `subcontract.rs` explicitly REFUSES to assert a
partition that would be "a tautology wearing the costume of a partition proof"
and documents why the only independent oracle died with the Python.
`measure12a/tests.rs` and `exact.rs` are dense and genuinely falsifiable
throughout. The `protocol9_tape_oracle` re-blessing guard is the correct pattern.

## The hunter's recommended order of work

1. ~~**B1/B2**~~ - closed and refused respectively by round 4; see above and the
   carry-forward.
2. **D** - a third `analysis/*_conformance.json` for the zero-fraction and
   exposure quantities, if the two-copy gates are meant to be permanent.
