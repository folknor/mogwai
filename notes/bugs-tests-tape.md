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
duration instead of a condition" family is absent here. Only two tests touch the
filesystem at runtime, and one of them is a serious defect.

Measured in dev, serial:

| test | wall |
|---|---|
| `session_modulation_reproduces_curves` (`#[ignore]`d, NOT skipped) | 7.71 s |
| `run_seeded_tape_dwell_is_bounded` | 6.69 s |
| `dwell_is_bounded_across_run_seeds` (`#[ignore]`d AND skipped) | 6.71 s |
| `realism` | 6.47 s |

## 1. `regenerate_arrival_transcripts_amendment_only` runs in the gate and silently re-blesses a committed pin

`crates/mogwai-data/src/generated/arrival.rs:1792`. It is `#[ignore]`d for an
ENVIRONMENT/POLICY reason ("sanctioned only by a signed amendment"), and it is
NOT in the `skip` list in `brokkr.toml`. The gate sets `include_ignored`, so it
runs on every `brokkr check --gate`. It does:

```rust
let path = format!("{}/tests/fixtures/arrival-transcript-{file_stem}.json", env!("CARGO_MANIFEST_DIR"));
std::fs::write(&path, ...)
```

The hunter ran it and diffed: the fixture is byte-identical today, so the tree
stays clean and nobody has noticed. The failure mode is exactly the one the pin
exists to prevent:

- A kernel change lands. Run 1: `arrival_transcripts_replay_bit_exact` fails
  (correct) - AND IN THE SAME RUN the regenerate test overwrites
  `arrival-transcript-shot_noise.json` with the new output.
- Run 2 recompiles, `include_str!` picks up the rewritten fixture, and the pin is
  green. One re-run converts a real regression into a blessing.

The whole doc comment ("running this outside an amendment defeats the pin's whole
purpose; that is why it is `#[ignore]`d") is built on the assumption that
`#[ignore]` keeps it out of the suite. It does not, in this workspace, by design.
Contrast `mogwai-server/src/fill_golden.rs`, which gets this right: it writes
only when the file is ABSENT, and panics after writing so the run can never be
green on a fresh bless.

Fix: this should not be a `#[test]` at all. Make it an example target or a CLI
subcommand (`mogwai gen --type arrival-transcript`). A regeneration tool living
in the test binary is a landmine that the gate's own `include_ignored` steps on.
Minimum stopgap is adding the name to `skip`, but that leaves a file-writing test
one config edit away from arming again.

## 2. `session_modulation_reproduces_curves` - the slowest test in the workspace, `#[ignore]`d for nothing, and closest to the watchdog

`tests.rs:2155`, a bare `#[ignore]` with NO REASON STRING, not in the `skip`
list. It walks `SESSION_DRAW = 15_000_000` parent events and takes 7.71 s in dev
- it is the 7.6 s test named in the brief. So:

- The `#[ignore]` buys nothing: the gate runs it anyway.
- It is the binary's floor. `mogwai-data`'s lib test binary is run twice
  (workspace plus `instrumented`), so this test alone is about 15 s of gate wall.
- It has the least headroom of anything in scope: 7.7 s serial against a 20 s
  watchdog, and it is compute-bound, which is precisely the class the sibling
  project saw inflate 1.7 s to 2.7 s (about 1.6x) at 8 threads. 7.7 x 1.6 is
  about 12 s. One slower host or one more parallel compute-bound sibling and this
  is the test that kills the gate.

Either drop the `#[ignore]` (it is a real gate and should be honest about it) or
cut `SESSION_DRAW`. Note the harness already throws away every child
(`src.burst.remaining = 0`) to afford the span; the remaining cost is the arrival
draw itself.

## 3. `dwell_is_bounded_across_run_seeds` is skipped on a cost claim that is false

`brokkr.toml` skips it, and both the source comment and the config imply it
"outlives the 20-second per-test hang watchdog by design". It runs in 6.71 s -
cheaper than `session_modulation`, which is not skipped. Its `#[ignore]` reason
says "walks eight two-million-tick run realizations", but it calls
`assert_run_seed_dwell_is_bounded_with_draw(run_seed, DRAW / 8)` - eight 250k
walks, the same 2M total as `realism`.

Consequence: the only multi-seed dwell gate the repo has is never run, while
`run_seeded_tape_dwell_is_bounded` (`tests.rs:2012`) runs the seed-42 arm at full
`DRAW` for the same 6.7 s. The right move is to delete
`run_seeded_tape_dwell_is_bounded`, un-ignore `dwell_is_bounded_across_run_seeds`,
remove it from `skip`, and get eight seeds of coverage for the same wall clock.
This directly addresses "a single seed passing is not evidence the band holds".

## 4. The `skip`-list invariant needs the OTHER direction checked too

`brokkr.toml` states the invariant as "every pattern must match ONLY `#[ignore]`d
tests" and warns that nothing detects a missing entry. Both halves are currently
violated in the same file:

- IGNORED BUT ABSENT FROM `skip`: `session_modulation_reproduces_curves`,
  `regenerate_arrival_transcripts_amendment_only` (mogwai-data);
  `worst_case_history_page_bytes`, `history_admission_overhead`
  (mogwai-server/src/http.rs - another hunter's crate, same class).
- SKIPPED ON A FALSE COST: `dwell_is_bounded_across_run_seeds`.

This is enumerable and should be enforced mechanically rather than by comment. A
test in `mogwai-data` (or a brokkr check) can parse every `#[ignore]` in the tree,
parse the `skip` list, and assert the two agree - one side listing a non-ignored
test, the other holding an ignored test with no entry. The config comment already
predicts the failure ("nothing detects the omission"); three live instances say
the prediction came true.

## 5. Conformance "vectors" V4-V8 are green by construction

`arrival.rs`, `arrival_conformance_vectors_v1_through_v9`. The vectors are
described as "independently-derived inputs" that "must be able to catch a
plausible but wrong kernel result." Four of the nine cannot:

- `execute_v4_event_markov_contract` asserts
  `vector["expected"][0]["main_stream_order"] == json!(["gap","flip","latent_mid",...])`
  - that is the FIXTURE COMPARED AGAINST A LITERAL IN THE TEST. No production
  code is consulted. Same for `contract_a_order` / `contract_b_order` in
  `execute_v5`.
- `execute_v6_budget_traversal` re-implements the budget/segment traversal inline
  in the test (`let spent = if open { remaining.min(seconds) } else { 0.0 }; ...`)
  and checks it against the fixture. The kernel's real traversal is never called.
- `execute_v7_degenerate_budget` computes `-uniform.ln()` and
  `(budget * 1e9).ceil().max(1.0) as u64` in the test.
- `execute_v8_reopen_seam` recomputes the crossing rule
  (`from < at && at <= candidate`) and the shift locally; only
  `CADENCE_STEP_NS`/`INTRA_EVENT_STEP_NS` are borrowed from production.

Break the real `next_parent` budget traversal, the reopen-seam crossing
condition, or the child-draw ordering and every one of these stays green. This is
the "two implementations pinned by a fixture built on one side" failure the
AGENTS.md lesson names, in its purest form: the fixture and the test-local
arithmetic were derived from each other, and the production code is not in the
loop. V1/V2/V3/V9 are fine - they drive `params.transition`, `params.level`,
`transition_from_jumps` directly, and V5's probability arm does use `SweepShape`.

Fix: route V6/V7/V8 through the kernel entry points, and for V4/V5's ordering
claims, assert the order against something observable in production (a draw-tag
trace, or the `arrival_transcripts` mechanism) rather than against a JSON array
the test also hardcodes.

## 6. `garch_second_moment_instrumentation` reports on a process that has not shipped for two repairs

`tests.rs:4671` and the block comment above it at 4488. The comment states
`a1 * E[z^2] + b1 = 0.12 * 2 + 0.875 = 1.115`. The shipped constants are
`GARCH_ARCH = 0.02`, `GARCH_GARCH = 0.979`. The test hardcodes
`vol_scalar = 1e-6`; the shipped `VOL_SCALAR` is `1.2e-5`. And it labels the RAW
t(4) arm "AS SHIPPED" and the standardized arm "COUNTERFACTUAL" - but the shipped
generator standardizes (`trace_consumes_no_draws_and_leaves_the_tape_byte_identical`
asserts `innovation_raw / innovation_std == STUDENT_T_UNIT_SCALE`), so the labels
are exactly inverted relative to today's code.

Its printed output, captured by the hunter, says
`sqrt(E[sigma2]) / vol_scalar 968.71x` and `cap occupancy 17.19%` under a banner
reading "AS SHIPPED". Neither number describes anything the venue produces. The
two assertions (`raw.effective_persistence() > 1.0` at 1.019, `standardized < 1.0`
at 0.9991) are arithmetic identities over two constants and a distribution mean -
no change to the generator, the tape, or the rails can make them red.

This is a stale report presented as a finding, and it is dangerous precisely
because it is legible and quotable. Either re-point it at `VOL_SCALAR` and swap
the labels, or delete it - `standardized_candidate_rail_sizing` (which does use
1.2e-5 and does check the shipped rails against the measured tail) is the test
that carries the real claim, and it is `#[ignore]`d and skipped, i.e. never run.

## 7. `tape_version_prose` can panic on a byte-index slice

`crates/mogwai-data/tests/tape_version_prose.rs`, `claims()`:

```rust
let end = (at + pattern.len() + digits.len() + 40).min(rest.len());
let start = at.saturating_sub(40);
found.push((value, rest[start..end].to_string()));
```

Those are byte offsets into a `String` that may contain multi-byte UTF-8. Any
non-ASCII character within 40 bytes either side of a
`` `TAPE_PROTOCOL_VERSION` is N `` claim panics the gate with "byte index is not
a char boundary" - and the panic looks nothing like a stale-prose failure. The
repo's no-gremlins rule makes this unlikely rather than impossible (a name, a
currency symbol, a pasted quote in `notes/` all suffice; `notes/` is deliberately
in scope). Use `char_indices` or `rest.get(start..end)` with a fallback.

Two smaller notes on the same file: `markdown_files` follows symlinks via
`path.is_dir()`, so a symlinked directory cycle recurses forever; and it
`panic!`s on any `read_dir` error, so one unreadable directory anywhere in the
tree fails a test about prose.

## 8. Smaller things

- **`empty_hour_stats_match_the_shared_conformance_fixture` is the good pattern**
  and worth keeping in view as the model: versioned JSON under `analysis/`,
  `assert_eq!(spec.version, 1)` so a fixture bump forces a re-read, both
  implementations kept separate. Same for
  `stratified_roll_matches_the_shared_conformance_fixture`. These two are the only
  cross-implementation gates in scope that actually satisfy the shared-fixture
  rule.
- **`SETTLEMENT_CANDIDATES`** (`calendar.rs:13`) is a `#[cfg(test)]`
  `thread_local!` mutated by production code and asserted in
  `settlement_day_step_respects_local_offset_and_open_filter`. It survives
  `--test-threads=8` (libtest gives each test its own thread) AND
  `--test-threads=1` (the test resets to 0 first), so it is safe today - but it is
  process-global state reachable from a production path, and its safety rests on
  a `.set(0)` one line above the assertion. If a second test ever asserts on it
  without resetting, or if libtest ever pools threads, it breaks silently. A
  returned count would be strictly better than a thread-local counter.
- **`published_book_carries_values_without_calibration_metadata`** asserts
  `size_of::<PublishedBook>() == 48`. That is a layout pin with no `#[repr]`
  guarantee behind it; it is a struct-reordering tripwire dressed as a size test.
  The second assertion in the same test (size is less than the naive sum) is the
  one carrying the actual claim.
- **`tick_rule_reuses_the_trade_symbol_allocation`** asserts
  `Arc::strong_count == 3`. Correct today, but strong-count assertions are
  brittle against any change in how the tick is moved through `apply`.
  `Arc::ptr_eq` on the line above already carries the claim.
- **`liquidity_drought_imitates_dying_symbol`** accepts `mean_gap` anywhere in
  `[0.5x, 2x]` of expected - a 4x window. Combined with the fixed seed this is
  deterministic, so it is not flaky, but it will not catch a drought multiplier
  that is off by 50%. The comment acknowledges the band is slack rather than
  fitted; worth a note that it is wide enough that only a broken-outright drought
  fails.
- **`the_integral_floor_lifts_the_realized_mean_above_the_notional_target`**
  deserves credit: its comment explicitly identifies that the obvious assertion
  (`realized > target`) is green by construction and pins the measured ratio
  2.326 plus or minus 0.05 instead. That is the reasoning the V4-V8 vectors above
  are missing.

## Structural recommendation

The `mogwai-data` lib test binary carries roughly 20 s of dev-profile compute in
four tests (`session_modulation` 7.7, `run_seeded_tape_dwell` 6.7, `realism` 6.5,
plus `arrival_families_match_their_stationary_derivations` at 3 x 30 simulated
days), and the whole binary runs TWICE because of the `instrumented` sweep. That
sweep exists to prove the `hotpath` annotations still compile - it does not need
to re-execute million-tick statistical gates to do that. Gating the four heavy
walks behind a `cfg(not(feature = "hotpath"))`, or giving the `instrumented`
sweep a filter, halves the gate's compute bill and buys back the watchdog
headroom that finding 2 is spending.
