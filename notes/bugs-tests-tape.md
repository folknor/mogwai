# Test hunt: the tape, generator and golden tests

Reconnaissance report, 2026-08-18. One Opus hunter, scope: every `#[cfg(test)]`
module and test file under `crates/mogwai-data/src/`, the crate's `tests/`
directory, and `crates/mogwai-server/tests/`.

This hunt looks for defects in the TESTS, not in the code they test: tests that
do not survive parallel execution, tests that wait on fixed durations rather than
conditions, tests that assume they are the only test in the process, tests that
cannot fail, fixtures that cannot represent their shape, and anything else weird.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

UNLIKE THE OTHER TWO HUNTS, THIS ONE RAN THINGS - focused `brokkr test --debug`
invocations, including the fixture-writing test in finding 1. The wall times
below are therefore measured rather than estimated. The tree was verified clean
afterwards.

## What is in scope, and how it behaves

Everything in this scope is pure computation - no sockets, no threads, no sleeps,
no ports, no shared temp paths. THERE IS NOT A SINGLE FIXED-DURATION WAIT OR
WALL-CLOCK BUDGET ASSERTION ANYWHERE IN `mogwai-data`, so the whole "waits on a
duration instead of a condition" family is absent here. Only two tests touched
the filesystem at runtime, and one of them was a serious defect (finding 1,
closed in round 1: no test in the tree writes a committed fixture any more, and
nothing may again).

## 2. `session_modulation_reproduces_curves` - closed in round 2

Closed by giving the bare `#[ignore]` a COST reason that also states what the
attribute buys - exclusion from the FAST lane, not from the gate, which sets
`include_ignored`.

BOTH OF THE FINDING'S PROPOSED CUTS WERE REFUSED, with measurement.

- Cutting `SESSION_DRAW`: 15M parent events is about 30 simulated days, and the
  seven `dow_weight` assertions need whole weeks to separate a weekend from a
  weekday. Halving it does not fail sooner, it passes on less evidence. The
  harness already discards every child, so the remaining cost is the arrival
  draw itself.
- Un-ignoring it: 7.5 s onto every plain `brokkr check` for no gate value, the
  gate already running it.

THE WATCHDOG RISK DID NOT REPRODUCE. 7.51 s serial, and `brokkr.toml`'s own
record has the same walk at 7.622 s serial against 7.768 s at eight threads -
this walk barely notices the parallel lane and sits 2.6x from the 20 s kill.
The finding's 1.6x inflation figure was borrowed from a sibling project.

## 3. `dwell_is_bounded_across_run_seeds` - closed in round 2

The finding was right and the fix is the one it proposed:
`run_seeded_tape_dwell_is_bounded` is deleted, this test is un-ignored and out
of `skip`, and it runs the arms 0, 1, 2, 3, 4, 5, 6 and 42 - seed 42 kept
because it is the default run seed and therefore the shipped realization, seed
7 displaced so the arm count stays eight and the total stays two million parent
events. The per-bound discrimination arithmetic is in
`reference/performance.md` and beside the test.

## What round 2 changed, and what is left

The full gate went 58.3 s to 41.4 s and 50.4 s on two post-change runs - a
noisy figure, and all of whatever it saved comes from the `instrumented`
sweep; the focused `brokkr test -p mogwai-data "" --debug` went
133.66 s to 86.15 s over its three sweeps. The numbers, the lane split and the
one measured loss are in `reference/performance.md`. The two skip entries this
cluster left behind are closed, so nothing here is open:

- CLOSED IN ROUND 4, with finding 6. Both entries are out of `skip` and both
  tests are un-ignored. `synthetic_spread_decomposition_at_protocol_seven` at
  6.46 s is the only entry left that the cost heading describes even loosely,
  and it is untouched.

## 4. Ignore reasons are free text, so no scan can classify them

The rest of this finding closed in round 1: the proposed "every ignored test owes
a skip entry" rule was refused with evidence and the one genuinely silent
sub-case was enforced instead. The reasoning lives on
`gate_skip_list::no_test_binary_writes_a_committed_fixture` and in the
carry-forward; it is not repeated here.

THE RESIDUE IS REFUSED IN ROUND 5, not carried further. It was stated as "an
ignore REASON is free text, so nothing can tell a COST ignore from an
ENVIRONMENT one, and that classification is what any stronger rule in this area
would need". The classification is only worth having if some rule gates on it,
and after rounds 1 through 4 there is no such rule left to write: the one
silent case is enforced by `no_test_binary_writes_a_committed_fixture`, the
cost half of the `skip` list was audited against measurement in round 2 and
now carries per-entry numbers, and every remaining ignore reason is prose
addressed to a human reader. Introducing a machine-readable taxonomy to
classify reasons nothing dispatches on would be a scanner whose verdict no
gate consumes. There is no longer a reasonless `#[ignore]` anywhere in the
tree: round 2 gave
`session_modulation_reproduces_curves` a COST reason that also states what the
attribute buys, which is exclusion from the FAST lane rather than from the gate.

`dwell_is_bounded_across_run_seeds`'s skip entry is gone and the test runs.

## 5. Conformance "vectors" V4-V8 are green by construction - closed in round 3

The finding reproduced in full and every vector it named now runs against
production. The per-vector verdict on whether the fixture was an independent
derivation or a re-blessing, the five bite-checks, and the two things the
bite-checks found that nobody had proposed are in
`notes/bug-loop-carry-forward.md`. Nothing is left open from this cluster.

## 6. `garch_second_moment_instrumentation` reported a retired process - closed in round 4

Reproduced in full, including both printed numbers, and closed by REPOINTING
rather than deleting: the shipped arm now runs at `VOL_SCALAR` with the
standardized innovation, the second arm runs the same shipped `a1` and `b1`
with the standardization switched off and is labelled as exactly that - it
cannot reconstruct the pre-standardization era, because `GarchVol::new` reads
the shipped `a1` and `b1` and offers no override - and the shipped arm carries
three assertions a constant change can break. The entangled skip entries went
with it
- `standardized_candidate_rail_sizing` is un-ignored, renamed
`shipped_garch_rails_sit_above_the_clean_tail`, pointed at the shipped
constants and running, and so is
`realized_return_envelope_under_regime_scaling`. THE COLD REVIEW OF THAT FIX
PASS FOUND THREE MORE OF THE SAME DISEASE IN THE REPOINTED INSTRUMENT - a
stated 10 percent window that was really under 1 percent, a "retired process"
label on a parameter triple that never shipped, and two rail assertions that
could not fail - and all three are closed. The bite-checks, the numbers, the
seed ensemble behind the widened window and the one assertion deliberately left
ungated are in `notes/bug-loop-carry-forward.md`. Nothing is left open from this
cluster.

## 7. `tape_version_prose` panicked on a byte-index slice - closed in round 4

The `claims()` half reproduced exactly and is fixed, with a pin that builds its
multi-byte character rather than committing one. Of the two smaller notes, the
symlink one is REAL BUT NOT AS DESCRIBED - a cycle does not recurse forever,
Linux bounds it at 40 levels - and is fixed and pinned; the `read_dir` panic is
REFUSED as fail-closed behaviour a gate certifying complete coverage should
have. The measurements are in `notes/bug-loop-carry-forward.md`. Nothing is
left open from this cluster.

## 8. Smaller things - closed in round 5

All six items are resolved. The measurements and the bite-checks are in
`notes/bug-loop-carry-forward.md`; the verdicts, because two of the six were
praise rather than defects:

- BOTH PRAISED PATTERNS DESERVE THEIR PRAISE, verified rather than taken on
  trust, with one asymmetry found and closed in the Roll pair - the Python half
  PRINTED the fixture version where the Rust half asserts it. The integral-floor
  test's reasoning holds and its ratio has not drifted, but its parenthetical
  claim about what a truncating grid reads was false and is now the measured
  number.
- The three brittle-assertion items are converted rather than deleted, because
  each was the only thing asserting something real: a returned candidate count
  replaces the `#[cfg(test)]` thread-local, a sum over the four field types
  replaces the `== 48` layout pin, and a `ptr_eq` on the permuter's retained map
  key replaces the `strong_count == 3`.
- The drought window is an ensemble now, and the finding's own claim about it
  was half wrong: at 0.5x-to-2x a multiplier off by 50 percent DOWNWARD was
  caught and one off by 50 percent upward was not.

TWO THINGS ARE CARRIED, NOT CLOSED, and they are named here rather than left to
the carry-forward alone, because a section that says "nothing is open" over
residue recorded elsewhere in the same change is the exact defect this round
spent itself correcting.

- CARRIED: the Roll conformance fixture's Python half is manual. It runs as
  `python3 analysis/roll_estimator.py conformance` and NO LANE RUNS IT, so the
  version guard fixed above only fires for a human who thinks to invoke it. The
  dwell pair has automated tests on both sides; this pair does not. Not fixed
  because a Rust test may not spawn Python.
- CARRIED, and it is a hole in a rule `AGENTS.md` states as binding: NEITHER
  shared fixture detects a quietly WIDENED `tolerance`. The version is a SCHEMA
  version, and a tolerance edit weakens both implementations at once, so the
  second implementation - whose whole purpose is to catch a one-sided drift -
  is structurally blind to it. There is no re-derivation to compare against,
  the way the arrival vectors have one. Naming it is all this round can do.

## Structural recommendation - closed in round 2

The premise held and the fix landed: the five walks over ~2 s carry
`#[cfg(not(feature = "hotpath"))]` and the instrumented sweep of this crate went
44.61 s to 9.96 s, which is where ALL of the gate's saving comes from. Three
corrections to how it was stated, for the record.

- The heavy set is SEVEN tests, not four, and the report's fourth
  (`arrival_families_match_their_stationary_derivations`) is 472 ms - not in it
  at all. The two it missed are `session_edge_spike_lifts_realized_clamp`
  (5.28 s) and `session_edge_spike_localizes` (2.72 s).
- The `cfg` and the filter are NOT interchangeable alternatives. The gate
  certifies complete coverage, so a filtered-out test is an orphaned pair and an
  error, while a test absent from a build shape is no pair at all. There is also
  no per-sweep filter to reach for: `skip` lives on the profile and would apply
  to both sweeps.
- The justification is stronger than "the annotations only need to compile":
  `crates/mogwai-data/src` carries no `hotpath` annotation at ALL, so the two
  build shapes of this crate's lib differ in the dependency graph and in one
  example target and in nothing a test can observe. Measured, the same test runs
  7.65 s in one shape and 7.67 s in the other.
