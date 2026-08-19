# Bug hunt: mogwai-cli

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-cli`: the clap dispatcher and argument handling, the offline
subcommands, the thin `serve` hand-off, and the socket-backed integration tests
that live in this crate.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter reports editing nothing, and verified its claims against the tape
constant (`crates/mogwai-data/src/lib.rs:109` = 20), the committed artifacts'
recorded versions (11 and 14), and cargo's test CWD semantics.

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
  `write_json_atomic`. Its module doc was RIGHT and needed no edit. The
  seventh writer nobody had counted, `minute_range_envelope`, had the same
  defect - an entry gate, a full corpus pass, then `clean_tree: true` written
  as a constant - and got the same fix. THE CLASS IS GATED:
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

## 6. `--type trace` under-counts children of the last parent when the window ends at `end` (medium confidence)

`gen.rs:378-407` (`write_trace`). The loop breaks on `ts >= end` for either
event kind, then emits `pending`. A parent whose `parent_ts < until <= end` can
own child trades whose timestamps land at or past `end`; those trades break the
loop before `record.child_count += 1` runs, and the record is emitted anyway
with a truncated `child_count`. Since the constraint is only `until <= end`, the
common case `until == end` puts the last in-window parent exactly on this
boundary. The quote-side path handles this correctly (a quote at `>= until`
emits the previous parent THEN breaks), so the asymmetry looks unintentional
rather than a documented truncation. The hunter has not confirmed a real parent
straddles the boundary in practice - it depends on child spacing - hence medium
confidence, but the code admits it.

## 7. Non-biting and brittle tests

- `tests/presets_cli.rs:47` `every_listed_preset_is_fetchable_by_name` never
  asserts the listing command succeeded. If `presets` exits nonzero with empty
  stdout, the `for name in listing.lines()` body never executes and the test
  passes green. Its sibling three lines up does assert status; this one dropped
  it.
- `tests/completion.rs:105-122`: the first `diagnostics` and
  `diagnostics_for_sink` pair is constructed, wired into the first launch's
  `StderrSink::Lines`, and then immediately shadowed by a second pair. The first
  venue's captured stderr is unreachable dead state. Harmless, but it reads as
  if the healthy-venue diagnostics were being checked, and they are not.
- `tests/serving.rs:121-127` and `tests/unconfigured_symbol.rs:61-63` use
  `while let Ok(Some(Ok(Message::Text(frame))))`. Any non-Text frame - a Ping, a
  Close - terminates the loop and falls straight into
  `panic!("... produced no frames")`, reporting a false failure. Today the
  server sends no pings so it does not bite; the sibling loops at
  `serving.rs:105` and `completion.rs:47` use the safe shape (bind `message`,
  match Text, `_ => {}`). The strict shape should not survive.
- `measure.rs:322-334`: the population gate's comment says "23 sorted unique",
  but it sorts a copy before comparing, so it verifies uniqueness and count and
  says nothing about the actual ordering of `per_session`. If sorted order is
  load-bearing downstream, the gate does not check it.

## 8. Structural: the shipped venue binary carries roughly 13,000 lines of retired-protocol evidence tooling

`main.rs` dispatches 25 subcommands. `serve` - the only one the launcher, the
adapter, and every consumer ever invokes - is three lines of hand-off. The other
24 include `stage-m` (1604 lines plus 1550 for tier2), `ordered-counts` (1298),
`count-curve` (691), `arrival-screen` (742), `arrival-control` (636),
`slow-geometry` (701): one-shot drivers for specific numbered bricks of
protocols 11, 12a and 12b, several of which exist to reproduce a Python script
that no longer exists. Every one of them is linked into
`target/release/mogwai`, the binary `PR_SET_PDEATHSIG` and the shipped launcher
exec, and into every consumer's `cargo install mogwai-cli`.

This has concrete costs beyond size. `arrival_control::run_b1` refuses unless
`current_exe()` ends with `target/release/mogwai` - the research driver and the
venue binary are the same executable, so B1's "exec the shipped binary" trick
works only in a release build from the repo root, and the driver has to defend
against being itself. `mogwai man` exists specifically because an installed
binary has no source tree beside it, yet half the subcommands hardcode
`research/market-data/databento/mnqv/2026-07.full.tbbo` and `analysis/...` as
CWD-relative defaults and are meaningless anywhere but this checkout.

The hunter would split this: `mogwai` keeps `serve`, `gen`, `presets`, `man`,
`config` - the things a consumer of the venue uses - and a second binary
(`mogwai-lab`, in the crate that already owns the method library) takes the
whole intake and measurement and brick toolbox. That also puts the
repo-relative default paths in a binary that is honestly repo-scoped, and lets
the lab binary depend on git and on the corpus without the venue doing so.
Pre-1.0, this costs a rename in `brokkr.toml`'s bench targets and a doc pass;
the bin TARGET name `mogwai` that `AGENTS.md` flags as load-bearing stays on the
venue half, so nothing that keys on it breaks.

Smaller structural notes in the same direction: `session_profile --alignment`
and `segments cut --window` are stringly-typed with hand-rolled `match`
validation where every other enum-shaped flag in the crate is a clap
`ValueEnum` (so they get no `--help` enumeration and no shell completion);
`man.rs`'s `content()` doc says "Each topic maps to one `reference/*.md`" when
five of the seven are `docs/*.md`; and `presets <name>` uses `print!` rather
than `println!`, so a preset document without a trailing newline leaves the
shell prompt mid-line.
