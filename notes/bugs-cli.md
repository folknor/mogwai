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

`TAPE_PROTOCOL_VERSION` is 20. `version_ok` is therefore `false`, and it is
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

## 2. Three unit tests in this crate are structurally dead - they resolve repo-relative paths from the crate directory (high confidence)

A cargo integration or unit test runs with CWD = the PACKAGE root, i.e.
`crates/mogwai-cli`. These tests read repo-root-relative paths and silently
early-return:

- `arrival_control.rs:626` `the_control_artifact_carries_no_b8_field` reads
  `DEFAULT_OUT` = `"analysis/mnq-arrival-control.json"`, which resolves to
  `crates/mogwai-cli/analysis/...`, never exists,
  `let Ok(bytes) = ... else { return }`. It has never asserted anything. The
  same file's `repo_root()` helper exists two functions below and is used by the
  ignored mid-run pin, so the fix was known and not applied here.
- `arrival_screen.rs:660`
  `the_screen_artifact_carries_every_evaluated_cell_and_its_verdict` - same
  shape, `if !path.exists() { return }`.

And the arrival-screen one is ALSO wrong on its merits, which the dead path is
hiding. It asserts

```rust
artifact["binding"]["tape_protocol_version"] == json!(mogwai_data::TAPE_PROTOCOL_VERSION)
```

against the COMMITTED `analysis/mnq-arrival-screen.json`, which records 14,
against a live constant of 20. Fix the path and the test goes red immediately -
and correctly so, because the assertion encodes a false invariant: a frozen
artifact from an old tape identity must not be invalidated by a later, unrelated
bump. `TAPE_PROTOCOL_VERSION` bumps are declared FREE in `AGENTS.md`; this test
makes them cost a red gate. Same defect family as finding 1: a moving constant
pinned against a frozen artifact.

`analysis/mnq-arrival-control.json` records 11, incidentally, so the B8 test is
stale by nine identities too.

## 3. `mogwai cache clean --stale` is `clean` with extra steps - the token it computes can never match any producer's (high confidence)

`crates/mogwai-cli/src/cache.rs:44-58`. `current_token()` folds
`full_command = std::env::args().join(" ")` into the provenance hash. Invoked as
`mogwai cache clean --stale`, that string is `".../mogwai cache clean --stale"`.
The only real producer in the workspace (`mogwai-lab/src/arrival_screen.rs:687`)
computes its token with
`full_command = "arrival-screen:kernel-version=...:start=...:length=...:warmup=..."`
and `subcontract_hash = <measure hash>`, while `cache.rs` passes
`subcontract::subcontract_hash()`.

So the "CURRENT provenance token" the `--stale` path preserves is a token no
cache entry has ever carried. `clean --stale` deletes EVERYTHING, identically to
bare `clean`. `docs/cli.md` states it "removes only the ones that do not match
the CURRENT provenance token - the same pruning a cache write already does
automatically" - a durable doc asserting behaviour the code does not have. This
is a contract the code claims and does not keep, and it destroys data (an
hours-long screen's walk cache) on an invocation the operator reaches for
precisely because it is supposed to be the safe one.

Secondary in the same function: `fingerprint_hash` reads
`analysis/fingerprint.json` CWD-relative with `.ok()...unwrap_or_default()`, so
outside the repo root it silently becomes `""` - a third way the token diverges.
Folding the WHOLE argv into a cache key was probably never right; the key should
be the inputs, not the command line that asked for them.

## 4. `mogwai fit` writes non-atomically and never re-attests the tree, contradicting its own module doc (high confidence)

`crates/mogwai-cli/src/fit.rs:63-78`. The module doc says fit "carries the same
contract `mogwai measure` carries: `binding.harness_tree_commit` must name
exactly the code that ran". It does not. `measure` (`measure.rs:398`) and
`arrival_control` (`arrival_control.rs:445`) both re-read `fresh_tree_state()`
immediately before serializing and bail "the artifact is unbound" if HEAD moved
or the tree went dirty. `fit` takes `require_clean_tree()` at entry, runs a
multi-minute fit, and writes - binding a commit that may no longer be what is
checked out.

It also writes with a bare `std::fs::write` where every sibling uses
`write_json_atomic`, so an interrupted or failing fit truncates the artifact in
place rather than leaving the previous one intact. Given `DEFAULT_OUT` is under
`target/` this is currently low-blast-radius, but `--out` is documented as
pointing anywhere.

## 5. Test artifacts are written into a gitignore-shadowed `crates/mogwai-cli/target/` (medium-high confidence, real, small)

The unit tests in `arrival_control.rs` write to
`"target/arrival-control-b5/{green,red,truncated}.log"` and
`"target/arrival-control-empty-baseline/MNQ.csv"`, CWD-relative from the crate
dir. Those files exist on disk right now at `crates/mogwai-cli/target/...`. They
are invisible to `git status` because the root `.gitignore`'s bare `target`
pattern matches ANY directory of that name at any depth, and `cargo clean` never
touches them because cargo's target dir is at the workspace root. So this is an
untracked, unswept, permanently-growing scratch area that looks like a build
directory and is not one.

`arrival_control::run_with` has the same problem in production:
`let scratch = PathBuf::from("target/arrival-control")` (line 388) - same as the
module's default input paths, all CWD-relative. Run from anywhere but the repo
root and the command writes its multi-GB walk scratch wherever you happened to
be standing. Use `CARGO_TARGET_TMPDIR` in tests (as `characterize_cli.rs`
correctly does) and resolve the scratch root explicitly in production.

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
