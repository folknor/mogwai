# Test hunt: mogwai-lab and mogwai-cli unit tests

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope: every
`#[cfg(test)]` module under `crates/mogwai-lab/src/` and
`crates/mogwai-cli/src/`. `mogwai-lab` had never been covered by any previous
hunt; this is the first time anything has looked at it.

This hunt looks for defects in the TESTS, not in the code they test.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

ROUND 1 CLOSED A1, C1 AND C3 on 2026-08-19; ROUND 2 CLOSED A2 AND A3 the same
day. Those sections are gone from this
document; what the rounds decided that a later reader would otherwise re-derive
wrong is in `notes/bug-loop-carry-forward.md`.

## A. Tests that cannot fail

**A4. `frozen_12a_path_is_byte_identical_through_the_parameterized_seam` - a
tautology.** `count_curve.rs` (~line 665)

It compares `GeneratedAcc::new(a,b,c,d,e).finish()` against
`GeneratedAcc::new_with_count_windows(a,b,c,d,e, COUNT_WINDOWS_S).finish()`. But
`new` is LITERALLY
`Self::new_with_count_windows(seed, start, end, offset, tick, crate::subcontract::COUNT_WINDOWS_S)`
(`measure12a/generated.rs:97-106`). This is `f(x) == f(x)`. No edit to the seam
can make it red. Only the trailing `assert_eq!(COUNT_WINDOWS_S, &[1,5,60])`
carries signal - keep that, delete the rest, or rewrite it to assert that a
DIFFERENT window list produces a DIFFERENT artifact (which is the property that
would actually catch the seam being mis-plumbed).

**A5. `excess_draw_is_session_order_independent` - degenerate fixture.**
`stage_m_tier2.rs:1435`

Every cell is constructed with `residual: 0.0`. It then asserts
`excess(...) == 0` on both orderings and that the residuals are pairwise equal -
a comparison of all-zeros against all-zeros. No order-dependent bug in `excess`
could be detected. It needs distinct per-cell residuals, and the assertion should
be on the multiset of residuals per hour after a genuine shuffle (the sibling
`no_slow_uses_independent_hour_streams_deterministically` right below it gets
this right and is the model to copy).

**A6. `stamps_run_from_the_pinned_origin` - a type-level tautology.**
`mogwai-lab/src/sidecar.rs:167`

`assert!(second >= first)` over two successive `stamp_us()` calls backed by
`Instant`. `Instant` is monotonic by construction in std; this cannot fail. The
doc comment says "monotonic AND SMALL" but nothing checks smallness, which is the
half that could actually catch a wrong epoch (e.g. `stamp_us` returning
wall-clock micros since 1970 instead of since `init`). Assert the bound:
`stamp_us()` right after `init()` is under, say, a second.

**A7. `emission_without_a_fifo_is_inert` - no assertions, and ambient-env
dependent.** `mogwai-lab/src/sidecar.rs:155`

Four calls, zero asserts; it only fails on panic. Worse, its stated premise ("Run
without the variable set") is unguarded: `BROKKR_MARKER_FIFO` is a real env var
this workspace's own benchmarking harness sets, and `CHANNEL` is a process-wide
`OnceLock`. Under `brokkr mogwai`-adjacent tooling or any lane that exports it,
this test silently becomes a different test - and since the FIFO open is the
thing being avoided, an accidental set turns a no-op test into an I/O test
against a nonexistent FIFO. The `OnceLock` also means whichever test in the
process calls `init()` first fixes the channel for all of them; the two sidecar
tests are order-coupled by construction, which now matters at
`test_threads = 8`. Fix: have the test assert
`std::env::var(MARKER_FIFO_ENV).is_err()` as a precondition, and make `channel()`
injectable so inertness is asserted rather than merely not-crashed.

**A8. `the_hour_normalization_centers_the_exposure_weighted_mean_on_one` - flat
input cancels the property.** `mogwai-lab/src/fit/curves.rs:357`

The raw curve is `2.5` at every one of 24 hours. Under a flat input, ANY
normalization that divides by ANY weighted average yields exactly 1.0 - the
exposure weights cancel identically. The test name claims the exposure-weighting
is pinned; it is not. Give the raw curve per-hour variation and a nonuniform
exposure table, then the weighting scheme is observable. Same objection, milder,
applies to `materialization_is_idempotent` immediately below (flat curve, so
idempotence is trivial).

**A9. `cache_root_prefers_override_then_env_then_xdg` - name promises three
limbs, body tests one.** `mogwai-lab/src/storage.rs:334`

The body is a single `assert_eq!(cache_root(Some(&cli)), cli)`. The env and XDG
limbs - the ones with real precedence logic and real failure modes - are
untested. Testing them requires env mutation, which is unsafe at
`test_threads = 8`; the right fix is to make `cache_root` take the resolved env
values as parameters (a pure function) and test all three orderings, rather than
reaching into the process environment at all. Meanwhile the name is a lie and
should at least be narrowed.

## B. Self-widening tolerances (statistical gates that get MORE permissive as the code gets worse)

**B1. `assert_fidelity` in the envelope gate.**
`mogwai-lab/src/arrival_envelope.rs:1180`

```rust
let combined_se = (idealized_variance / n + candidate_variance / n).sqrt();
assert!(difference <= 5.0 * combined_se);
```

The tolerance is derived from the CANDIDATE'S OWN SAMPLE VARIANCE, over
`FIDELITY_MONTHS = 32`. A regression that widens the candidate's dispersion
widens the acceptance band with it - the gate's sensitivity is inversely
proportional to the severity of the class of defect it exists to catch. At n=32
and 5 sigma this is already a very loose bound; the failure mode is that a
genuinely broken candidate walk passes BECAUSE it is broken. This gate is 10
tests times 32 idealized plus 32 candidate month-scale walks and is the most
expensive thing in scope; it deserves an absolute component - also require the
candidate variance itself to sit inside a band around the idealized variance - so
that inflated dispersion is itself a failure rather than a license.

**B2. `the_envelope_matches_the_closed_forms_where_they_are_exact`.**
`mogwai-lab/src/arrival_envelope.rs:1156`

```rust
let tolerance = (5.0 * se_plugin).min(0.5 * closed);
```

The `.min` caps the self-widening, which is the right instinct - but it caps it
at 50% OF THE CLOSED-FORM VALUE. A sample variance 1.49x the analytic truth
passes. For a gate whose whole premise is "where the closed forms are exact",
half is not a tolerance, it is a shrug. If the plug-in SE at M=2000 is much
tighter than that (it should be), the cap never binds and the test is fine; if it
does bind, the test is nearly useless. Worth printing the two terms and picking a
cap that reflects the actual Monte Carlo error, e.g. `0.1 * closed`.

**B3. Wall-clock assertion inside the same gate.**
`assert!(elapsed <= CONFORMANCE_BUDGET_S)` (900 s) at the end. This is the
`tape_lateness_under_acceleration` shape that `brokkr.toml` already documents at
length as environment-sensitive and unresolvable. It is less likely to bite at
900 s, but it is a host-property assertion sitting inside a correctness gate, so
a loaded machine reports a CORRECTNESS failure. It should be an `eprintln!`
counter (the `sidecar` module exists for exactly this) and not an assertion.

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

1. **A4, A5, A6, A8** - small, individually cheap, each currently counted as
   coverage it does not provide.
2. **B1/B2** - the envelope tolerances. This one needs a decision rather than a
   patch: a self-scaling 5-sigma band at n=32 is a choice, and tightening it will
   make the most expensive gate in the workspace occasionally red. Worth naming
   what a failure would change before touching it.
3. **D** - a third `analysis/*_conformance.json` for the zero-fraction and
   exposure quantities, if the two-copy gates are meant to be permanent.
