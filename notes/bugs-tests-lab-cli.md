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
ROUND 5 CLOSED SECTIONS D AND E and VERIFIED SECTION F, so this document has
no open findings left. COUNTED RATHER THAN ESTIMATED, because the three places
that stated it disagreed: THREE FULL REFUSALS - B2, D1 and D4 - PLUS ONE
PARTIAL, D2, refused as a fixture candidate and closed by anchoring its inputs
instead. That is the phrasing every summary of this document uses. What else
remains below is C2's for-the-record list and F's verified praise. Sections A
and the closed halves of B, D and E are gone from this document; what the
rounds decided that a later reader would otherwise re-derive wrong is in
`notes/bug-loop-carry-forward.md`.

ROUND 5'S OWN COLD REVIEW FOUND THE ARC'S SIGNATURE DEFECT INSIDE THE FIX FOR
IT, which is worth stating at the top rather than burying in D2: the new
premise test asserted the window length against `FINAL_END_NS -
FINAL_START_NS` while `run_final_walk` derives the window it walks by parsing
`FINAL_LENGTH`, a SECOND ENCODING of the same quantity that nothing compared
against the first. So the test written to catch a drifting window could not
see the one edit its own docstring named. Both halves are closed - the premise
test reads what the walk reads, and `subcontract.rs` now gates the two
encodings against each other - and both bite.

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

## D. Shared-fixture convention - CLOSED, and no third fixture was built

**THE HUNTER'S CENSUS IS ACCURATE AND ITS GRADING IS NOT.** Every row was
re-derived against the code. The three "legitimate differential tests" are
exactly that and were not touched. Of the four remaining, the reasoned
verdicts, none of which is "write a `*_conformance.json`":

**D1. `the_zero_fraction_matches_the_count_hist` - REFUSED, and the report's
own table refutes its prose.** The table row says "test recomputes the ratio
from the committed artifact", which is the DIFFERENTIAL shape the report
praises three rows above; the prose then grades it as "two production copies
with no external anchor". The prose is wrong. `hourly_zero_second_fraction` is
the sole implementation of the quantity (four call sites, all consumers), and
the test recomputes it from a DIFFERENT FIELD of the committed artifact:
production sums `block2[hour]["1"]["zero_windows"]`, the test sums
`count_hist["0"]`. Two independent producer fields agreeing is an external
anchor, not a second copy of one convention. A fixture would state the ratio's
contract in neutral units and buy nothing the count histogram does not already
buy, because the count histogram is not written by the code under test.

**D2. `the_lab_walk_matches_the_measure_exposure_contract` - REFUSED as a
fixture candidate, STRENGTHENED where it was actually thin.** This one is a
genuine two-copy gate: `mogwai_lab::arrival_control::control_generated_pass`
and `mogwai_cli::measure::run_final_walk` are the same walk loop written
twice, because the lab cannot depend on the CLI. But the convention does not
fit it. A `*_conformance.json` states a QUANTITY in a form neither side's
units privilege; what these two produce is a 20 KB `GeneratedAcc` record of a
month-long walk, so the only "fixture" expressible is a golden of that record
- a re-blessed artifact, which is a different mechanism with a ten-minute
production cost, not this convention.

  WHAT A DRIFT WOULD ACTUALLY COST, enumerated rather than asserted, because
  three of the four drift-prone inputs turn out to be anchored already. The
  window START, its LENGTH and the WARMUP: the lab side reads all three from
  the committed 12a artifact, `run_final_walk` from `mogwai_lab::subcontract`
  constants, so those are committed bytes against code and a divergence
  REDDENS the gate. The count-window list: `GeneratedAcc::new` versus
  `new_with_count_windows(COUNT_WINDOWS_S)`, whose agreement round 3's A4
  rewrite pins directly. What is left unanchored is the walk loop itself being
  edited identically in both files, and a JSON fixture cannot express a loop.

  THE THIN PART WAS THE PREMISE, AND IT IS FIXED. Only the window START was
  checked, and it was checked against a bare literal inside a test `#[ignore]`d
  at ten minutes - so the one place the two sides' inputs are genuinely
  cross-checked never ran in a check lane, and a mismatch in the other two
  would have surfaced as an opaque diff of two accumulator records blaming the
  exposure contract. All three inputs are now asserted against the constants,
  in a new unignored
  `the_committed_binding_carries_the_window_run_final_walk_measures`.
  Bite-checked by moving `SUMMARY_WARMUP` to `"4d"`: fails by name in
  microseconds, `"3d"` against `"4d"`.

  AND THE FIRST CUT OF THAT TEST CARRIED THE VERY SHAPE SECTION D WAS HUNTING,
  found by the round's cold review. It asserted the length against
  `FINAL_END_NS - FINAL_START_NS`, which is what `measure.rs` writes INTO the
  artifact; `run_final_walk` derives the window it actually walks from
  `FINAL_LENGTH`, a seconds string. Two constants, one quantity, no gate - the
  reviewer grepped every use of both and found nothing comparing them - so
  `FINAL_LENGTH = "2674801s"` would have moved the measured window with the new
  test green, and the ten-minute pin would then have failed as the opaque 20 KB
  accumulator diff the new test exists to pre-empt. The `SUMMARY_WARMUP`
  bite-check hit the one input where the test did read what the walk reads,
  which is why the gap survived it.

  BOTH HALVES CLOSED, and the identity was owed regardless of the first. The
  premise test parses `FINAL_LENGTH` exactly as `run_final_walk` parses it, and
  `subcontract.rs`'s own tests gained
  `the_final_windows_two_encodings_of_its_length_agree`. Neither encoding can
  be deleted - the fit driver needs a duration string, the artifact needs
  nanoseconds - so the identity assertion is the only close available.
  Bite-checked TOGETHER at `FINAL_LENGTH = "2674801s"`: the lab gate fails
  naming both encodings, the premise test fails naming the walk's window, both
  in well under a second.

  ONE CORRECTION TO THE REVIEW, measured rather than argued.
  `hash_matches_the_python_reference` DOES redden on that edit - `FINAL_LENGTH`
  is in `subcontract_hash`'s tree - so the workspace was not fully blind to a
  bare edit. It was blind to the SANCTIONED one: moving a sub-contract constant
  re-blesses `EXPECTED_HASH` in the same change, and the re-blessed hash says
  nothing about whether the two encodings still agree. A frozen-snapshot hash
  catches an unintended edit; only the identity catches an intended edit to one
  encoding of a quantity written twice. That distinction is now on the test and
  in `AGENTS.md`, as its own bullet: two constants encoding one quantity are
  the two-copy defect without the gate, and easier to miss because neither
  looks like an implementation.

**D3. `session_segment_at_agrees_with_mogwai_lab` - THE REWIRE HAD LANDED, so
the test is deleted and so is the copy it guarded.** The hunter asked whether
the rewire had happened. It had, in a direction its comment did not anticipate:
`gen.rs` carries no session math at all any more - it imports
`mogwai_lab::summary::session_segment_at` - so the sweep had quietly stopped
comparing two crates and become a permanent two-copy gate INSIDE `mogwai-lab`,
between `summary` and `session`, duplicated branch structure and duplicated
session-minute constants included. `summary`'s copy is now a field mapping over
`session`'s, verified equal over 207,360 instants (six UTC offsets, three eras,
30 s resolution, 196,560 open and 10,800 closed) by a throwaway probe holding
the deleted body, and the sweep is gone as `f(x) == f(x)`. Surviving coverage
bite-checked: transposing the post-halt segment origin fails
`summary_matches_an_independent_tick_walk` on its hand-computed instant.
Two durable sentences in `session.rs` that said `gen.rs` "carries it
independently today" were corrected in the same change.

**D4. `close_reduced_agrees_with_close_on_block1_and_block2` - REFUSED, the
table miscategorises it.** It is not two conventions, it is one optimization:
`close_reduced` drops blocks 3 and 4 to buy Stage A budget, and the property is
that it changes nothing else. If the block-2 definition moves, both sides
moving together is CORRECT - that is the whole claim. There is no drift for a
fixture to catch.

**AND THE CONVENTION'S OWN COUNT IS STALE.** `AGENTS.md` says "two instances so
far". There are three: `analysis/segment_library_conformance.json` carries the
same `_doc`/`units`/`rules`/`version` blocks and is read by both
`mogwai-data`'s `segment.rs` and `mogwai-lab`'s `segments.rs` - by PATH rather
than `include_str!`d, which the sentence also asserted universally. Corrected
there, since a convention that miscounts its own instances is the
durable-prose-asserting-a-live-fact shape aimed at itself.

---

The hunter's original census, kept because the grading above is only readable
against it. In this scope NONE of these use a shared fixture; every
two-implementation comparison derives one side inside the test:

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

**E1. CLOSED, and the recommended shape was itself the second instance.** The
finding is real: `cadence.rs` wrote to `std::env::temp_dir()`, keyed on the pid
alone, and leaked the directory whenever `probe` failed, because the cleanup
sat below the call on the success path only.

But "use the `storage.rs` helper's shape" would have reproduced the defect.
`CARGO_TARGET_TMPDIR` IS DEFINED FOR INTEGRATION-TEST TARGETS ONLY, and both
call sites are LIB UNIT tests - so `storage.rs`'s `std::env::var` lookup always
fails and its `env::temp_dir()` fallback is the branch that always runs, under
a comment claiming the opposite ("keeps test scratch data inside the project
tree"). There were two instances of the finding, not one, and the second was
wearing the fix. `characterize/tests.rs` had it right and says why in place:
`CARGO_MANIFEST_DIR` is defined for every compilation.

Both now go through one `storage::unit_test_scratch(name)`, which returns the
existing `ScratchDir` guard rooted at `target/lab-unit-scratch/<name>` - inside
the tree, removed on drop including the panic path, and keyed on pid plus a
nanosecond stamp so a second cadence test is unrepresentable rather than
merely unlikely. FIVE call sites were converted, not two - `cadence.rs`'s
probe fixture plus the four in `storage.rs` that went through
`scratch_test_root` - and five `remove_dir_all` lines that ran only on the
success path are gone with them. Verified by listing: every unique leaf
removed, nothing under `/tmp`. What the guard does NOT remove is the empty
`target/lab-unit-scratch/<name>/scratch/` spine above the leaf, which is a
fixed set of empty directories rather than an accumulation; the doc comment
says so exactly, because "removed on drop" would have a reader expect the root
to disappear. No assertion pins this - it is a convention, and manufacturing a gate
for it would be the wall-clock-threshold mistake in another costume.

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

**E3. VERIFIED AFTER ROUNDS 2-4, AND IT NEEDS AMENDING RATHER THAN CONFIRMING.**
The three literal claims all still hold, checked rather than re-read: no
`set_var` or `remove_var` anywhere under either crate's `src/`, no
`tracing_subscriber` / `init_stderr_logging` / `log::set_logger`, and the only
memory-budget assertions are the four `BudgetGuard::scripted` sites in
`arrival_screen.rs`.

What has changed is the reason. In round 1 the claim was "these tests happen
not to touch ambient state"; it is now a property the code enforces, by two
different mechanisms, and a later round should reach for whichever fits:

- PURE, where the rule can take its readings as data. Round 3's
  `cache_root_from(Option<&Path>, CacheEnv)` and
  `sidecar::resolve_fifo(Option<OsString>)` mean the environment-precedence
  and FIFO-attachment rules are asserted in full without a process
  environment existing. Nothing to install, so nothing to spoof - prefer this.
- INJECTED AND GUARDED, where the reading is a subprocess or a clock.
  `BudgetGuard::scripted` (pre-existing) and round 2's `TreeOracle`, which is
  additionally cargo-feature-gated because its output feeds a provenance
  claim.

So E3's "genuinely good design" is now the workspace's stated preference
order, and the one residual ambient coupling in scope is named in the
carry-forward rather than here: `sidecar`'s `OnceLock<CHANNEL>` is fixed by
whichever test calls `init` first, which binds the epoch and is why A6's
process-origin bound is the SWEEP's runtime rather than the test's.

## F. Praise - VERIFIED BY PERTURBING, five for five deserved

The arc had found four praised tests carrying false or vacuous claims, so none
of this was taken on trust. Test by test:

- `arch_coefficients_match_the_shipped_recursion` - DESERVED, and better than
  the report says. It is not only self-sensitive; it is the EXTERNAL ANCHOR for
  a genuine two-copy situation the report's own section D missed.
  `measure12a::generated`'s `ARCH_12A` / `GARCH_12A` are deliberate duplicates
  of `mogwai-data`'s coefficients (spec 2.3 forbids touching that crate in
  12a), and this test recovers their values from the SHIPPED recursion's own
  `VolTrace` output by least squares - a third party neither copy writes.
  Bite-checked by moving `ARCH_12A` to 0.0201: fails naming the recovered
  0.019999999999999435 against the local constant. The intercept is fitted
  rather than pinned, which is correct - no omega is duplicated.
- The `stage_a_batch` manifest suite - DESERVED. `committed_manifest_is_self
  _consistent` re-derives the whole manifest from the committed pilot, and each
  of the three mutation tests RE-BLESSES `plan_sha256` before validating, so
  the mutation is not caught for free by the identity hash. ONE WEAKNESS FIXED:
  all three asserted a bare `is_err()`, which the arc's own standing rule
  forbids - an unnamed error is satisfied by any other check tripping,
  including one broken by the re-hash itself. Each now names its refusal, and
  measurement showed they are three DIFFERENT refusals ("panel cell identity is
  inconsistent", "quick tasks are not an unchanged subset of the full panel",
  "probability sample is not the derived lowest-hash selection"), which the
  `is_err()` form could not have shown.
- `subcontract.rs` - DESERVED, and its refusal is well-founded rather than
  merely well-written: `Mode::Protocol11` really is `!is_12a`, so the partition
  assertion it declines really could never fail. It also scopes its surviving
  claims honestly ("they catch a typo or a stale name, not a
  misclassification").
- `exact.rs` - DESERVED. Every test asserts an exact bit pattern against an
  independently stated CPython value or a closed-form identity, and two carry
  explicit "the case only bites if..." premise assertions. ONE LINE ADDED:
  `negative_values_are_handled_through_the_magnitude` asserted only that two
  samples AGREE, which two zeros or two NaNs satisfy - the one shape in the
  file a broken implementation could pass. It now pins the shared value at 8/3.
- `measure12a/tests.rs` - DESERVED on the row section D flagged.
  `close_reduced_agrees_with_close_on_block1_and_block2` is an
  optimization-preserves-output gate, not a two-convention gate; see D4.
- `protocol9_tape_oracle` - verified in round 1, unchanged.

## F (original). Things the hunter read that are worth saying are good

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

## The hunter's recommended order of work - EXHAUSTED

1. ~~**B1/B2**~~ - closed and refused respectively by round 4; see above and the
   carry-forward.
2. ~~**D**~~ - worked by round 5 and answered NO on both. Neither candidate
   warrants a `*_conformance.json`: the zero-fraction gate already has an
   external anchor in a second artifact field, and the exposure record has no
   units-neutral statement to write down. The genuine two-copy gate in scope
   was the third one, `session_segment_at`, and it was closed by deleting a
   copy.

NO OPEN FINDINGS REMAIN IN THIS DOCUMENT. Three full refusals stand - B2, D1
and D4 - plus D2's partial refusal, which was closed by anchoring the gate's
inputs rather than its output. Four records in total, three of them whole.
