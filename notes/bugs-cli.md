# Bug hunt: mogwai-cli

CITATIONS BELOW TO `notes/bug-loop-carry-forward.md` ARE DEAD LINKS. That file
held the eleven-document arc's round-by-round state and was DELETED when the arc
closed on 2026-08-20, per `AGENTS.md`'s rule that a closed arc folds what still
binds into `AGENTS.md` and deletes its carry-forward. The standing lessons are
there; the deleted text resolves to git history.

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-cli`: the clap dispatcher and argument handling, the offline
subcommands, the thin `serve` hand-off, and the socket-backed integration tests
that live in this crate.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own, and they were worth discounting: FOUR
OF THE EIGHT FINDINGS WERE WHOLLY OR PARTLY STALE - already fixed in the tree
the hunter read - and finding 8 argued from a subcommand count, a set of line
counts and a reading of `run_b1` that none of them survived checking.

EXHAUSTED as of round 3, 2026-08-20. All eight findings are closed: seven
fixed, finding 8's binary split refused with its evidence recorded in place so
a later reader does not re-derive it.

CLOSE PASS, 2026-08-20. Gate green at 1316 + 466, 1839 pairs, 1782 run, 57
ignored, 0 orphaned. No code defect found across the three commits. Every
load-bearing claim was re-derived against the source rather than the record:
THE BINARY-SPLIT REFUSAL STANDS (`run_b1` does exec `gen` on `current_exe`, the
dispatcher does carry 22 subcommands, the method does live in `mogwai-lab`);
the class gate's ten-hit triage re-derives clean, with zero genuine let-else
bindings anywhere in `serving.rs` and no test in the tree using a third
absence-probe spelling; the re-attestation roster sees all five entry-gated
writers; and the four moved `--help` texts were verified against the BUILT
BINARY, not the source, since that is the only check operator-visible clap
prose has. Two record defects were corrected - the "seventh writer" line below,
and a missing round-3 entry in `notes/bug-loop-carry-forward.md`.

The hunter reports editing nothing, and verified its claims against the tape
constant, the committed artifacts' recorded versions (11 and 14), and cargo's
test CWD semantics. Its transcription of the constant said 20; the tree it read
carried 22, which is also the number finding 1 argues from, so the slip was in
the preamble alone.

## 1. `arrival-control` gate B1 can never pass: hardcoded tape version is three bumps stale (high confidence, real bug) - FIXED 2026-08-18

`crates/mogwai-cli/src/arrival_control.rs:210`

```rust
let version_ok = mogwai_data::TAPE_PROTOCOL_VERSION == 18;
```

`TAPE_PROTOCOL_VERSION` is 22. `version_ok` is therefore `false`, and it is
ANDed into B1's verdict (`"passed": shipping.is_empty() && version_ok`), which
is ANDed into the run's `failing_gates`. So `mogwai arrival-control` on this
tree spends its full multi-minute walk set and then writes
`verdict: negative-control-failed` with `B1` in `failing_gates`, for a reason
that has nothing to do with tape identity. The per-symbol byte comparisons can
all report `identical: true` and the gate still reads red - precisely the
"supporting check that contradicts the evidence it supports is worse than no
supporting check" failure the `is_non_shipping` doc comment above it says was
already found once.

This is the standing hazard `AGENTS.md` names: a durable statement of a live
tape identity that nothing checks. It is worse here than in prose because it is
EXECUTABLE prose. Three places state the identity and all three disagree with
reality:

- the code says 18
- its own doc comment (lines 165-178) narrates a re-baseline "from 16 to 17" and
  never mentions 18
- `docs/cli.md` (durable, must-be-true) says "the accepted identity is 18 since
  boat placement moved to an independent cursor...", and the arrival-screen
  section says "records the live tape protocol version (18...)"

None of these are in the two phrasings `tape_version_prose.rs` checks, so the
guard was structurally unable to catch it.

Structural view, not a patch: an accepted-identity constant that must be
hand-edited at every bump is a ratchet that will be forgotten every time - it
already has been, three times running. Either B1's supporting check should
record the version and NOT gate on it (the byte comparison is the actual
evidence; the version equality is decoration that can only produce false reds),
or the accepted baseline should be an argument like `--b1-baseline-commit`
already is, defaulting to "whatever the baseline commit's constant was", which
is derivable from git. The hunter would delete the equality outright.

FIXED by the second of those, not the deletion: `tape_version_at` reads
`TAPE_PROTOCOL_VERSION` out of the baseline commit and the check compares the
running binary against it, so the gate asserts that no bump landed inside the
range the baseline tapes came from - which is what the literal was trying and
failing to say, and which cannot go stale. An unreachable commit or an
unparseable declaration refuses. The equality was kept rather than deleted
because a bump between the baseline and HEAD genuinely does void the byte
comparison's premise; what was wrong was stating the accepted value by hand.
`docs/cli.md` stopped naming a number in both places the hunter listed.

## 2-5. RESOLVED 2026-08-19 (fix pass, round 1)

Findings 2, 3, 4 and 5 are closed. Four reported findings, five real defects
plus one already-fixed half; what each turned out to be, so a later round does
not re-derive it:

- FINDING 2 WAS HALF STALE. `the_control_artifact_carries_no_b8_field` had
  ALREADY been fixed by commit `4a1e18e` ("Make a vacuous artifact pin
  execute"), which is in the tree the hunter read; it joins `repo_root()` and
  panics on a missing artifact. REFUSED as reported. The screen half was real
  and is fixed: `the_screen_artifact_carries_every_evaluated_cell_and_its
  _verdict` now joins the repository root, panics rather than skipping, and its
  `tape_protocol_version` assertion no longer holds a FROZEN artifact against
  the LIVE constant - the hunter was right that the dead path was hiding a
  false invariant. It asserts the field is an identity, which is what a frozen
  artifact can be held to. A non-vacuity assertion was added beside it: the
  per-cell loop is the bulk of the test and an empty `cells` array would have
  walked it in zero iterations, which is the same vacuity relocated inward.
  The resurrected test PASSED immediately, which is only trustworthy because
  the class gate below was bite-checked against the exact skip that was there.
  THE CLASS IS NOW GATED, not the three instances patched.
  `no_test_declines_to_assert_on_a_missing_input` in
  `crates/mogwai-cli/tests/gate_skip_list.rs` scans every `#[test]` and
  `#[tokio::test]` body under `crates/*/src`, `crates/*/tests`,
  `crates/*/examples` and `crates/*/benches` for a body that probes for an
  absent input and returns, on the existing literal-aware stripper and
  function-span machinery.
  THE FIRST CUT OF IT WAS BLIND, and the cold review caught it: its span walk
  matched `#[test]` alone, and `#[test]` is not a substring of
  `#[tokio::test]`, so it read no async test in the workspace - roughly sixty
  in `serving.rs` alone, which is where the socket suites live. Its claim of
  "zero further instances workspace-wide" was therefore worth nothing, and a
  full green gate was no evidence against it: a scanner that sees nothing
  passes trivially. THE CORRECTED ANSWER, with both spellings scanned: TEN
  HITS, ALL IN `crates/mogwai-cli/tests/serving.rs`, ALL FALSE POSITIVES, AND
  ZERO REAL INSTANCES. Every one is a socket DRAIN loop - `while let
  Ok(Some(Ok(message)))` or the let-chain `&& let Ok(msg) = ..` - where the
  `return` is the SUCCESS exit and the loop falling through panics, so nothing
  declines to assert. They were closed by tightening the probe rather than by
  exempting them: the `let Ok(` probe now recognizes the LET-ELSE form
  structurally (an `else` reached before the statement's `;`), which excludes
  `while let`, `if let` and let-chains by what they are rather than by where
  they sit. A gate producing false positives is as useless as one producing
  none. Two parser fixtures pin both halves -
  `an_async_test_that_skips_is_convicted_too` and
  `a_socket_drain_loop_is_not_a_missing_input_skip` - and both were
  bite-checked as text edits, reading the named assertion: reverting the
  `#[tokio::test` match failed the first on `left: 0, right: 1` with an empty
  offender list, and reverting the let-else recognition failed the second on
  the `while let` arm. Two lesser scope gaps went with it: `examples/` and
  `benches/` are scanned now (kept OUT of the shared `source_files`, because
  an example target is the sanctioned home for a fixture generator and the
  fixture-write gate would otherwise refuse its own remedy), and both
  self-exemptions match `root.join(file!())` rather than a bare file name,
  which would have exempted any other `gate_skip_list.rs` anywhere.
  `crates/mogwai-cli/src/test_paths.rs` is the one place a unit test in this
  crate resolves the repository root or a scratch directory from.
- FINDING 3 WAS REAL AND IS FIXED, by refusal rather than by a better guess.
  `cache clean --stale` now requires `--keep <TOKEN>` and refuses without it;
  `cache stats --entries` prints the tokens present. A cache entry's provenance
  token binds the PRODUCING command's inputs, which a `cache` invocation cannot
  derive, so the only correct options were "name it" or "refuse" - and the
  synthesized token was destroying an hours-long screen cache on the safe-
  looking invocation. `docs/cli.md` was wrong here (it described the intended
  behaviour, not the code's) and was corrected.
- FINDING 4 WAS REAL, AND THERE WERE TWO OF IT. `fit` now re-attests with
  `fresh_tree_state` and refuses "the artifact is unbound", and writes through
  `write_json_atomic`. Its module doc was RIGHT and needed no edit. A SECOND
  WRITER HAD THE SAME DEFECT, `minute_range_envelope` - an entry gate, a full
  corpus pass, then `clean_tree: true` written as a constant - and got the same
  fix. (An earlier draft of this line called it "the seventh writer nobody had
  counted"; that was wrong and the close pass corrected it. The guard roster is
  SIX and `attestation.rs`'s module doc has named all six, `minute_range_envelope`
  among them, since before this arc. What went uncounted was not the writer but
  the RE-ATTESTATION half of the contract, missing in two writers of six.)
  THE CLASS IS GATED:
  `every_tree_attested_writer_re_attests_before_its_write` in
  `crates/mogwai-cli/src/attestation.rs` is a second source roster beside the
  existing one, keyed on the GUARD CALL rather than on the binding key, because
  `fit.rs` never names `harness_tree_commit` (the lab driver serializes it) and
  the older roster is structurally blind to it. `arrival_envelope_diagnostic`
  is exempt by construction and the exemption is stated in place: it takes no
  entry gate and RECORDS the reading rather than asserting a constant, so it
  has no stale claim to make.
- FINDING 5 WAS REAL AND IS FIXED IN BOTH HALVES. The two unit tests write to
  the WORKSPACE target directory through `test_paths::scratch_dir`
  (`CARGO_TARGET_TMPDIR` is unavailable in a unit test - cargo sets it for
  integration targets and benches only, which `mogwai-lab`'s
  `unit_test_scratch` already records). Production's
  `PathBuf::from("target/arrival-control")` is now a `ScratchDir` under the
  storage policy's cache root: unique per process, removed on drop, and no
  longer dependent on where the operator was standing. THE BINDING IS THE
  GUARD there - the tidy-looking `ScratchDir::new(..)?.path().to_path_buf()`
  compiles and deletes the directory before the first walk, and the first cut
  of this fix wrote exactly that.

Bite-checks, all as text edits, all reading the named assertion: restoring the
skip shape failed the class gate on `arrival_screen.rs:746` by name; gutting
`fit`'s re-attestation (imports included, so it compiled) failed the roster on
`["fit.rs"]`; restoring the synthesized `--stale` token failed
`stale_without_a_named_token_refuses_and_deletes_nothing` on `aaaa was
deleted` - the RESOURCE assertion, which is why that test asserts the disk
BEFORE the refusal. Asserted the other way round it went red on the
`expect_err` without ever looking at the cache, which is a bite-check that
proves the wrong thing.

No tape version bump is owed: nothing on the tape generation path moved, and
the scratch relocation changes where walk temporaries live, not a drawn byte.

### Cold review of the fix pass, closed 2026-08-19

Six findings, all real, all closed in the same round. Beyond the class gate's
blindness above:

- `test_paths::scratch_dir` REINTRODUCED THE SHARED-PATH SHAPE the arc had just
  recorded a burn from - a FIXED `target/<name>` per name, opening with
  `remove_dir_all`, in a gate profile that runs two sweeps CONCURRENTLY. It
  returns a `mogwai_lab::storage::ScratchDir` now, under
  `target/cli-unit-scratch/<name>/`, with the pid-plus-nanosecond leaf that
  makes a second concurrent test under one name unrepresentable rather than
  merely unlikely, and drop-cleanup so the directories stop persisting. All
  four call sites bind the guard rather than taking `.path().to_path_buf()`,
  which is the guard-scope defect in the tidiest-looking line.
- `cache clean --stale --keep <TOKEN>` STILL DELETED EVERYTHING ON A TYPO.
  `clean_stale` only COMPARES names, so `--keep bbb` for `bbbb` kept nothing
  and cleared the lot - the exact data loss `--keep` exists to prevent, moved
  from unconditional to one keystroke away. An unknown token now refuses
  against `cache_entry_tokens` and names the candidates.
  `an_unknown_token_refuses_rather_than_pruning_everything` asserts the two
  seeded directories still exist BEFORE it reads the error, for the same reason
  its sibling does.
- THE ARRIVAL-CONTROL SCRATCH PROSE OVERSTATED THE HARM. The comment claimed
  the CWD-relative scratch dropped "multi-GB walk scratch" wherever the
  operator stood; `control_generated_pass` writes one small
  `arrival-control-<seed>.toml` there and deletes it immediately, the walk
  being in memory. The relocation is right, the magnitude was invented, and a
  durable comment asserting a fact the code contradicts is the family this
  pass is closing. Corrected in place.
- `reference/architecture.md` still described `clean --stale` as a working
  prune. It names `clean --stale --keep TOKEN`, the two refusals and
  `stats --entries` now, and its SCRATCH sentence states the per-process leaf.
- `fit`'s CWD-relative half was only half swept: `scratch_dir:
  PathBuf::from("target/mogwai-fit/scratch")` is a `ScratchDir` under the cache
  root now, like `arrival-control`'s, and `resolve` takes the directory rather
  than naming one. `DEFAULT_OUT` was deliberately LEFT: an ARTIFACT defaulting
  to the working directory is the storage policy's stated behaviour, and
  `preflight` and both `synth` outputs carry the identical convention, so it is
  a crate-wide question rather than a defect in `fit`. Filed in `notes/todo.md`.

## 6-7. RESOLVED 2026-08-20 (fix pass, round 2)

Finding 6 was REAL and is fixed. Finding 7 named four tests: TWO WERE ALREADY
FIXED IN THE TREE THE HUNTER READ and are refused, two were real and are fixed.
What each turned out to be, with the measurement, so a later round does not
redo it:

- FINDING 6 REPRODUCED, and the reproduction is the interesting part. The code
  admits it exactly as reported - `write_trace`'s loop broke on `ts >= end` for
  a TRADE as readily as for a quote, and `--trace-until == start + length` is
  legal, so the last in-window parent's brood was cut at the walk's stopping
  instant and the short record emitted anyway. BUT IT IS INVISIBLE TO ANY
  ROUND-NUMBER WINDOW: a brood spans microseconds while the parents are seconds
  apart, so a `--trace-until` on a whole second falls between broods. 599
  round-second closes were probed against a walk an hour longer and not one
  differed. The regression test therefore DERIVES its window close from the
  tape - it walks the source for the first parent past the origin owning a child
  strictly after it, counts that parent's whole brood as an independent oracle,
  and closes the window one nanosecond past the parent.
  `the_last_parents_child_count_does_not_depend_on_where_the_walk_stops` in
  `gen.rs`. Bite-checked by restoring the trade-side break: red on `the last
  in-window parent's brood was truncated at the window close`,
  `left: Some(1), right: Some(4)` - the named assertion, and the same 1-versus-4
  the differential form had shown before the oracle replaced it.
  THE PERTURBATION WAS NOT A PURE REVERSION, and the round-2 cold review was
  right to say so. The fix DELETED the `end` parameter, so restoring the defect
  restores a parameter: the signature moves, and the test's own `write_trace`
  call site and the production call in `run_into` had to be edited alongside it.
  Every edit was a text edit and nothing was restored with `git checkout`, but
  this is not the single-hunk reversion `AGENTS.md` describes, and the record
  says so rather than describing a cleaner check than was run. The general
  point: a fix that changes a SIGNATURE cannot be bite-checked by a local
  reversion, and the honest move is to name the extra edits, not to prefer a
  weaker fix for the sake of a tidier check.
  THE FIX REMOVES `end` FROM `write_trace` ENTIRELY. Since the validator already
  enforces `until <= end`, the quote-side `>= until` break is the only stopping
  point the record shape admits: a brood belongs to its parent, so the walk must
  run to the NEXT PARENT and no further. `end` still bounds the WINDOW at the
  call site, which is where it belongs.
  NO TAPE VERSION BUMP IS OWED. `write_trace` is a consumer - it draws nothing,
  perturbs nothing, and `trace_consumes_no_draws_and_leaves_the_tape_byte_identical`
  in `mogwai-data` pins that. What changed is how many ticks the trace reader
  consumes before it stops, which moves a REPORTED count and not a generated
  byte. `TAPE_PROTOCOL_VERSION` is 22.
- FINDING 7, `presets_cli.rs`: REFUSED, STALE. `every_listed_preset_is_fetchable_by_name`
  already asserts `output.status.success()` AND non-emptiness of the listing,
  with a comment naming the exact vacuity the hunter describes. Landed in
  `8a782fa` on 2026-08-18, in the tree the hunter read.
- FINDING 7, `completion.rs`: REFUSED, STALE. The healthy venue takes
  `StderrSink::Discard` and its comment records the shadowed `Arc<Mutex<Vec<_>>>`
  as a defect already removed - "a buffer filling for no reader". There is one
  `diagnostics` pair in the file and it is read.
- FINDING 7, THE STRICT DRAIN SHAPE: HALF STALE, HALF REAL.
  `unconfigured_symbol.rs` was already converted to the ending-recording form,
  and its comment quotes `while let Ok(Some(Ok(Message::Text(_))))` as the shape
  it replaced. `serving.rs`'s `a_ws_upgrade_for_a_configured_non_boot_symbol_is_served`
  was the LAST INSTANCE workspace-wide and is converted to match. NOT
  BITE-CHECKED, AND IT CANNOT BE: the venue emits no Ping, Pong or Binary frame,
  so no production edit makes the old shape produce the wrong answer - which is
  precisely why it survived. The change is brittleness removal, and what it buys
  is that the panic now names the ending (`the venue CLOSED the socket`,
  `the socket failed`, `the deadline expired`) instead of reporting all of them
  as "produced no named market frame". The audit was widened rather than taken
  on faith: `Ok(Some(Ok(Message::` appears eleven times in `crates/`, and every
  other instance is a `match` arm with a live catch-all beside it.
- FINDING 7, THE POPULATION GATE: REAL AND FIXED, AND IT WAS WORSE THAN
  REPORTED. The hunter said the gate "says nothing about the actual ordering".
  It cannot say anything about it: it sorted `sorted` and compared it against
  the sorted-deduped `unique`, and TWO SORTED VECTORS OF THE SAME MULTISET ARE
  EQUAL BY CONSTRUCTION, so the comparison had exactly one reachable failure
  mode - a duplicate - while raising a refusal that reads "not 23 sorted
  unique". The dates are compared in arrival order now. The gate was EXTRACTED
  to `session_dates_are_23_sorted_unique` to be testable at all: it sat mid-way
  through a multi-minute walk driver behind a Brick G cache, with no reachable
  seam, which is why it had no test in the first place.
  `the_population_gate_refuses_a_calendar_that_is_out_of_order` in `measure.rs`
  covers all four refusals. Bite-checked as a text edit restoring the
  sort-before-compare: red on `a calendar out of ascending order must be
  refused`, the ordering assertion by name, with the duplicate, count and
  non-string arms untouched - so the perturbation selected the half that was
  decoration and left the halves that already bit alone.
  FILED, because the consumer is unreachable from here: no sweep in this
  workspace populates the walk cache, so the first real `measure` run is the
  first execution of the tightened comparison. `notes/todo.md`.

Also filed: `--type trace` still has no end-to-end coverage through the shipped
binary - the argv, the window validation and the legality of `until == end` are
unstated by any test.

### Cold review of the round-2 fix pass, closed 2026-08-20

No blockers. The review confirmed the `write_trace` fix (one caller, `end`
still validated at that call site, `analysis/out/` untracked so no committed
artifact needed re-blessing), the `measure.rs` fix and its diagnostic chain, and
that the `serving.rs` drain conversion is honestly labelled as unbitten
brittleness removal. Four minor findings, all closed:

- THE BITE-CHECK RECORD OVERSTATED ITS OWN PURITY. Corrected in place above: the
  perturbation was sound but it moved a signature and touched two call sites,
  which the record now says.
- `assert!(children > 1)` WAS STRICTER THAN THE PRECONDITION IT STATED. The
  oracle's `straddles` flag already guarantees a child past the parent, and a
  brood of exactly one such child exhibits the truncation as 0 versus 1 - a
  perfectly good witness the assertion would have thrown away as unusable. It is
  `> 0` now, restating the precondition rather than narrowing it, with the
  reasoning in place. This matters because the fingerprint is a committed
  artifact this arc has already re-blessed twice.
- THE TEST CITED A DOC COMMENT THAT DID NOT SAY WHAT IT CLAIMED. The test's doc
  asserted `write_trace` "says" `child_count` is the parent's WHOLE brood; the
  production doc said only "its child count". The DOC was tightened, not the
  citation - it is the durable statement the fix's correctness rests on, so it
  now states the contract and states why the next parent is the only legal
  stopping point.
- THE REFUSAL MESSAGE WAS AMBIGUOUS NOW THAT ALL THREE HALVES ARE REACHABLE. An
  out-of-order calendar of 23 distinct dates reported "carries 23 sessions, not
  23 sorted unique", which reads as a contradiction. Count, duplicate and order
  are three separate refusals now (`carries N session dates, not 23`, `carries
  duplicate session dates`, `carries session dates out of ascending order`), and
  `the_population_gate_refuses_a_calendar_that_is_out_of_order` ASSERTS EACH ARM
  BY ITS OWN MESSAGE rather than by a substring two of them share - which is the
  hazard this arc's instance 32 was.

Re-bitten after the split, as a text edit: deleting the `as_read != ascending`
refusal alone fails the test on `a calendar out of ascending order must be
refused`, with the duplicate, count and type arms still green - so the ordering
half is selected by the perturbation and by the message both.

## 8. RESOLVED 2026-08-20 (fix pass, round 3): the split is REFUSED, the four smaller notes are FIXED

THE BINARY SPLIT IS REFUSED, with the evidence below so a later reader does not
re-derive it. The four smaller structural notes it carried are independent of
the split and are all fixed. The deferred `notes/todo.md` question about
repo-shaped artifact defaults, which was explicitly parked "decide it with
finding 8", is decided and closed.

THE REFUSAL RESTS ON THREE THINGS, and a re-proposal has to answer all three:
the finding READS `run_b1` BACKWARDS and its strongest concrete cost is an
argument against the split; the size benefit is ALREADY BANKED, because the
method is `mogwai-lab` and the lab stays linked into the venue binary whatever
moves; and the cost is two hundred path rewrites plus a NEW build-identity
mechanism to replace the one the split destroys. Two things that could have
sunk the refusal independently were checked and did NOT - there is no
dependency cycle and the `test-seam` mechanism survives a move - so the
refusal is not a build obstacle wearing an argument's clothes. All three, and
both checks, are written out below.

### What reproduced, and what did not

- THE SUBCOMMAND COUNT IS 22, NOT 25, and one of the five the proposal wants
  the venue half to keep - `config` - IS NOT A SUBCOMMAND AT ALL. `mogwai`
  dispatches Serve, Gen, Presets, TickComposition, Man, Preflight, Measure,
  CountCurve, StageM, Fit, MinuteRangeEnvelope, ArrivalControl, ArrivalScreen,
  ArrivalEnvelopeDiagnostic, Cache, Characterize, SelectWindows,
  TickCompositionRatios, SessionProfile, Synth, CadenceFeasible and Segments.
  `mogwai_server::config` is a MODULE the `presets` arm calls, not a command
  line. Several line counts are understated too: count-curve is 817 not 691,
  arrival-screen 831 not 742, arrival-control 812 not 636. `serve` being a
  three-line hand-off reproduces exactly.
- THE CWD-RELATIVE DEFAULTS REPRODUCE, and there are more than the finding
  implies: `research/market-data/...` or `analysis/...` defaults appear in
  `preflight`, `measure`, `fit`, `minute_range_envelope`, `arrival_control`,
  `arrival_screen`, `arrival_envelope_diagnostic`, `select_windows`,
  `session_profile`, `segments` and `synth`.
- THE `current_exe` REFUSAL REPRODUCES VERBATIM, AND THE FINDING READS IT
  BACKWARDS. `run_b1` bails unless `current_exe()` ends with
  `target/release/mogwai`, and the finding calls this "the driver having to
  defend against being itself". Being itself is the DESIGN, and the doc comment
  above it says why: B1 execs `gen --type trades` and compares the bytes
  against pre-landing tapes, so the binary that generates the comparison must
  be the binary under test, "since the driver IS the shipped binary and so
  cannot disagree with itself about which build ran". SPLITTING BREAKS THIS
  OUTRIGHT: `gen` is a venue-half subcommand, so a lab binary running B1 has no
  `gen` to exec and must locate the venue binary by path or by argument -
  reintroducing precisely the build-identity ambiguity the current form
  forecloses. The finding's strongest concrete cost is an argument AGAINST the
  split.
- THE SIZE ARGUMENT IS MOSTLY ALREADY BANKED, which the finding does not
  account for. The METHOD is already a separate crate: `mogwai-lab` carries
  `measure12a`, `arrival_screen`, `arrival_control`, `fit`, `characterize`,
  `select_windows`, `session_profile`, `segments`, `stage_a_batch` and the rest.
  What sits in `mogwai-cli` is the DRIVER layer over it - argv parsing, artifact
  assembly, JSON writing. And `mogwai-lab` stays a dependency of the venue
  binary whatever moves, because `gen` reaches into it eighteen times and
  `main` calls `sidecar::init` before the argv parse. So the split relocates
  drivers while the intake method it is meant to unship remains linked.
- THE COST IS NOT "a rename in `brokkr.toml`'s bench targets and a doc pass".
  It is roughly two hundred `mogwai_lab::` paths rewritten to `crate::` across
  about twelve thousand lines of one-shot brick drivers, several of which have
  no test at all; six integration test files relocated; the `attestation.rs`
  source roster and its keying moved with them; a NEW build-identity mechanism
  for B1 to replace the one the split destroys; and the durable prose in
  `AGENTS.md`, `docs/cli.md` and `reference/architecture.md`. The arc's
  signature defect, forty instances deep, is A THING THAT READS AS GATED AND IS
  NOT, and this is the largest single opportunity in the arc to manufacture one,
  bought with a benefit that is largely already banked and paid for with a
  regression in the one gate the finding cites as its motivation.

Two things the fix pass checked because they could have sunk the split
independently, and neither did - recorded so the refusal is not read as resting
on them. `mogwai-lab` CAN host a binary: it already depends on `mogwai-data`,
`mogwai-protocol` and `mogwai-server` and none of the three depends on it, so
the direction holds. And the `test-seam` mechanism SURVIVES a move: the lab's
seam is `#[cfg(any(test, feature = "test-seam"))]`, so moved unit tests would
reach it through `cfg(test)` within the crate while a lab BINARY target, which
sets neither, would still have no installation point. The refusal rests on the
B1 inversion, the already-banked size, and the cost, not on a build obstacle.

### The four smaller notes, all fixed

- `session_profile --alignment` IS A `ValueEnum` NOW. A CLI-side `AlignmentArg`
  rather than a derive on the lab's `Alignment`, because `mogwai-lab` carries no
  clap dependency and must not grow one to describe an argument. The two
  spellings are pinned by `every_alignment_has_a_command_line_spelling`, whose
  gate is a match EXHAUSTIVE over `Alignment`: an alignment added to the lab
  fails this test's compile rather than becoming silently unreachable from the
  command line. ITS BITE IS THEREFORE A COMPILE FAILURE, not a red assertion,
  and no run-time perturbation demonstrates it - said plainly rather than
  described as a cleaner check than was run.
- `segments cut --window` IS A `ValueEnum` NOW, AND IT WAS WORSE THAN REPORTED.
  The hand-rolled `String` plus `window_by_name` meant `--help` enumerated
  nothing, and the argument's OWN help text named only `asia` and `london` when
  `WINDOWS` carries four - so `ny-morning` and `ny-afternoon` were cuttable and
  undocumented on the command line. Two gates, both bite-checked as text edits:
  deleting the `NyAfternoon` variant and its `resolve` arm failed
  `every_cuttable_window_has_a_command_line_spelling` on `the cuttable window
  ny-afternoon has no --window spelling, so nothing can ask for it` - the
  unreachable-window direction by name, which is the one that matters, since
  the other direction is a run-time refusal rather than a silent hole. And
  editing the lab's `NY_MORNING.name` to `ny_morning` failed
  `the_command_line_spelling_is_the_labs_own_name` on `clap must accept
  --window ny_morning: invalid variant`, which pins that the kebab-case rename
  and the lab's own name are one string rather than two.
- `man.rs` NO LONGER CLAIMS `reference/*.md`. Both the `content()` doc and the
  module doc said it; five of the seven topics are `docs/*.md`, which is the
  folder documenting how the venue is USED and therefore exactly the right
  source for a bundled operator manual. Corrected in both places, stating the
  five-and-two split the `ManTopic` doc already described.
- `presets <name>` NORMALIZES ITS TRAILING NEWLINE. The `print!` made the
  ending a property of the included FILE rather than of the command: a preset
  saved without a final newline left the prompt mid-line, one saved with a
  blank line printed two. THE GATE ON THE DISPATCHER IS THE UNIT TEST ALONE.
  `a_fetched_preset_ends_in_exactly_one_newline` in `presets_cli.rs` is an
  end-to-end test through the shipped binary and IT CANNOT BITE TODAY: every
  shipped preset happens to end in exactly one newline, so it passed unchanged
  against the broken dispatcher when it was run against it. Do not read it as
  the gate - it is a FORWARD GUARD on the preset FILES, which will bite the day
  a preset is saved with the wrong ending. What actually gates the dispatcher
  is the `preset_output` helper with `a_preset_document_prints_with_exactly_one
  _trailing_newline` beside it, bite-checked as a text edit: reverting the
  helper to the identity failed it on `a document with no trailing newline must
  not leave the prompt mid-line`, `left: "symbol = \"MNQ\"", right: "symbol =
  \"MNQ\"\n"`.

### The deferred artifact-path question, decided

`notes/todo.md` parked "whether `artifact_path` should refuse a non-bare default
or whether these commands should name a directory" explicitly to be decided with
this finding. DECIDED: the commands name a directory, and it is
`analysis/out/`. `fit`'s `DEFAULT_OUT` and both `synth` outputs named
`target/mogwai-fit/` and `target/mogwai-synth/`, and since `artifact_path`
resolves a bare default against the WORKING DIRECTORY by design, a run from
anywhere but the repository root created a directory literally called `target`
under the operator's feet - reading as a build directory that a `cargo clean`
would take. `analysis/out/` is this repository's gitignored output directory and
is already where `preflight` and `measure` default, so the whole repo-scoped
toolbox now writes unblessed output to one honestly repo-shaped place. The
defaults' actual purpose - not clobbering the committed `analysis/mnq-fit.json`
or `analysis/fingerprint.json` - is preserved, and it is stated in place on both
constants. `artifact_path` is NOT changed: refusing a non-bare default would
break `preflight`, `measure`, `arrival_control`, `arrival_screen` and
`minute_range_envelope`, every one of which is honestly repo-scoped already.
The item is removed from `notes/todo.md`. `docs/cli.md` stopped naming
`target/` in both places.

NO TAPE VERSION BUMP IS OWED. Nothing on the tape generation path moved: the
`ValueEnum` conversions resolve to the identical `SessionWindow` constants and
the identical `Alignment`, the preset newline is terminal output, and the three
artifact defaults name where an OFFLINE artifact lands rather than what a walk
draws. `TAPE_PROTOCOL_VERSION` is 22.

### Cold review of the round-3 fix pass, closed 2026-08-20

The review confirmed the refusal's central argument independently, and
confirmed the mechanics of all four smaller fixes - all four `WindowArg` names
match `mogwai_lab::segments::WINDOWS` with help text matching the lab's offsets
and lengths, both `--window` gates bite, the `AlignmentArg` exhaustive match is
the compile-time gate it claims to be, `analysis/out/` is gitignored and both
writers `create_dir_all` their parent so the new defaults work from a fresh
clone, `man.rs`'s five-and-two split is right, and `docs/cli.md` already said
`--window` "is one of four" so there was no doc drift to fix. Two findings, both
real, both closed:

- THE FIX REPRODUCED ITS OWN DEFECT CLASS TWO FILES OVER. Finding 8's list
  included "a doc comment claiming the wrong folder"; the constants moved to
  `analysis/out/` and THREE clap/module doc comments did not, so `mogwai fit
  --help` and `mogwai synth fingerprint --help` named `target/` and lied to the
  operator. `CadenceArgs::out` in the same file had been updated, which is what
  makes the others oversights rather than intent. All three corrected:
  `fit.rs`'s `out` field, `synth.rs`'s `FingerprintArgs::out`, and `synth.rs`'s
  MODULE doc, which the review did not name and which said the same thing.
  THE `fit.rs` SENTENCE'S CLAIM SURVIVES THE MOVE and was checked rather than
  reworded around: the claim is that a bare invocation cannot clobber the
  committed `analysis/mnq-fit.json`, and the default is now
  `analysis/out/mnq-fit.json` - a DIFFERENT path, in a directory `.gitignore`
  covers as `/analysis/out/`, while `analysis/mnq-fit.json` is tracked. The
  reasoning is no longer "under `target/`" but "a distinct path in the
  gitignored output directory", and the comment says that now.
- `MODEL_CLOCK_ALIGNMENT_DEFAULT` WAS A DEAD THIRD ENCODING, instance 41 of the
  arc's signature defect and the sharpest form of it yet: a test whose doc
  claimed to pin two encodings "where neither side can be derived from the
  other" while a THIRD, derivable, zero-reader encoding sat in the lab. Closed
  by WIRING rather than deleting, because the constant is where the fact
  belongs - the alignment the model runs on is the lab's, not the CLI's. The
  exhaustive match moved out of the test and into production as
  `AlignmentArg::of`, and the argument is now
  `default_value_t = AlignmentArg::of(MODEL_CLOCK_ALIGNMENT_DEFAULT)`, so the
  default has ONE encoding and `of` is both the compile gate and the carrier.
  The test's doc no longer claims what it does not do: it says the VARIANTS are
  the underivable half it asserts, and that the DEFAULT is deliberately not
  asserted BECAUSE it is derived. NOT BITE-CHECKED, AND IT CANNOT BE by a
  run-time perturbation - `of`'s bite is a compile failure, exactly as
  `every_alignment_has_a_command_line_spelling`'s was before it, and changing
  the lab constant now changes the CLI default by construction rather than
  producing a red assertion. Said plainly rather than described as a cleaner
  check than was run.

The review's three minor notes were judged and LEFT. `#[clap(rename_all =
"kebab-case")]` is a no-op on both new enums, but it is pinned deliberately on
`WindowArg` by `the_command_line_spelling_is_the_labs_own_name`, and carrying
it on `AlignmentArg` too keeps the two new enums stating the same thing the
same way; `segments.rs`'s `TapeType` lacking it is pre-existing cosmetics.
`fit.rs`'s `create_dir_all` ahead of `write_json_atomic` is redundant and
pre-existing, and removing it would couple the fit path to an implementation
detail of the writer for no gain.
