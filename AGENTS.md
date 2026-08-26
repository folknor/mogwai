# AGENTS.md

## Project

mogwai is a fake broker/exchange that plugs into a nautilus trading system to
exercise the *live* trading path. It synthesizes market data from a committed
fingerprint fitted offline to real trade history (the running venue opens no
CSV) and injects the messy, realistic execution divergences (partial fills,
rejects, delays, duplicate fills, dropped account updates, venue blackouts)
that an in-process backtest sandbox structurally cannot produce. The broker
core never imports nautilus; the `mogwai-adapter` crate is the lone,
deliberate exception - it depends on the published nautilus crates to ship
the `ExecutionClient`/`DataClient` pair a host constructs to drive the
`MOGWAI` venue over this workspace's native JSON-over-WS protocol.

The instrument set is open and the venue gates on nothing: a symbol arrives
and is served, with the preset's shape when the name matches one and the
default shape under the requested label when none does. The intake sequence -
corpus, measurement, fit, preset - makes a river better, never decides
whether it is served. The three shipped presets - MNQ, MES and BTCUSDT, with
MES a stated stopgap borrowing the MNQ fit - are the current state, not the
end state; several assessments have been made wrong by assuming the corpus is
closed. The offline toolbox (`mogwai-lab` and what survives in `analysis/`)
is reusable intake machinery, not one instrument's history: treat a component
as spent only when its question cannot recur, and keep per-instrument
knowledge in config or a preset rather than hardcoded in the method.

## Workspace

A Cargo workspace, seven crates under `crates/`. `reference/architecture.md`
is the full map; what follows is each crate's identity and the one fact that
binds work on it.

- `mogwai-protocol` - the wire types both ends serialize against; never
  imports nautilus. Also ships `launch`, the venue launcher, kept here because
  this workspace's own test binaries drive the venue through it and cannot
  depend on the adapter; `mogwai-adapter` re-exports it.
- `mogwai-engine` - the venue-agnostic exchange core, with the seam that
  injects armed divergences into the event stream.
- `mogwai-data` - the `TickSource` seam and the `GeneratedSource` synthetic
  generator the running venue uses, fitted to the committed fingerprint.
- `mogwai-venue` - the axum library that owns the sockets, the clocks and the
  pacing. It ships no binary and has no daemon mode: `serve` runs one
  foreground venue, reports its bound address as one JSON readiness line on
  stdout, and `PR_SET_PDEATHSIG` kills it with its launcher. Order entry is
  websocket-only. See `docs/cli.md`.
- `mogwai-cli` - the `mogwai` binary: a clap dispatcher over `serve` and the
  offline subcommands. The bin target name is `mogwai`, not the package name,
  and that is load-bearing: `brokkr run mogwai` and the shipped launcher both
  exec `target/release/mogwai` by that name. The socket-backed
  lifecycle/serving/completion integration tests live here, because only this
  crate's tests get `CARGO_BIN_EXE_mogwai`.
- `mogwai-lab` - the corpus-to-fingerprint method library (corpus parsing,
  the 12a measurement engine, fingerprint synthesis, the fit) plus `sidecar`,
  the observation-only benchmarking channels. It depends on `mogwai-data`,
  `mogwai-protocol` and `mogwai-venue`; nothing depends on it back, so there
  is no cycle.
- `mogwai-adapter` - the nautilus venue adapter and the only crate that
  touches nautilus: published crates.io releases pinned in its `Cargo.toml`,
  default-features off, no pyo3. The other six build nautilus-free.

`scripts/` holds the end-to-end smoke test and the harness-bug flush;
`analysis/` is the offline Python that fits the fingerprint; `notes/` holds
the transient work items. Durable documentation splits by subject: `docs/` is
how the venue is used, `reference/` how it is built and why. See the Document
folders section below for what each folder may and may not contain.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- No all-caps. Not for emphasis, not for a term of art, not for a warning.
  Write the sentence so the emphasis is carried by the words and where they sit.
  A document that shouts every important thing has no way left to mark the one
  thing that matters, and prose full of capitals is read by nobody twice.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.

### Tape protocol version

Any change to the tape generation path must bump
`mogwai_data::TAPE_PROTOCOL_VERSION` - no exceptions. This includes a generator
constant, an
arrival-clock or GARCH parameter, the committed fingerprint, seed derivation,
the fill band's draw, or the tape origin. Nothing can detect that a
determinism-affecting change should have bumped the version and did not.

The bump is owed by the commit of a changed artifact, not by a change to the
code that could produce one. `mogwai-lab` depends on `mogwai-data` and never
the reverse, and the generator reads `analysis/fingerprint.json` through
`include_str!` rather than calling the synthesis - so editing the synthesis
cannot move a tape byte until a regenerated artifact actually differs.

Prose stating a live identity is gated, because three bumps in a row left
durable statements naming a superseded one. Two phrasings, and only these two,
are checked against the constant by
`crates/mogwai-data/tests/tape_version_prose.rs`, over every markdown file in
the repository:

- ``TAPE_PROTOCOL_VERSION` is N` - N is the identity right now.
- ``TAPE_PROTOCOL_VERSION` next takes N` - N is the next unspent identity.

Write a live claim in one of those forms and the next bump cannot forget it.
Every other phrasing is read as a historical record of a past landing and is
left alone, which is what keeps a frozen spec's "stays 11" from being rewritten
out from under the contract it froze.

There is one recorded exemption, granted 2026-08-08 and deliberately narrow: a
one-leaf correction of `analysis/fingerprint.json`
(`empirical_ranges.modal_tick.max`, 0.25 to 0.1) landed without a bump, after
an exhaustive reader audit proved its sole consumer advisory and never on the
generator's path. It does not generalize to any other leaf: the rule stays
unconditional, and the next leaf needs its own audit and its own ruling.
`TAPE_PROTOCOL_VERSION` next takes 27. If a non-tape artifact revision ever
needs an identity of its own, give it a content hash or a method version
rather than overloading this constant.

### The correctness contract (owner ruling, 2026-08-09)

The bit-exactness era is closed: the pinned CPython conventions in the tree
(`py_sum`/`py_fsum`, the ported Mersenne Twister and kin) are the record of
how the Python-to-Rust port was proven, and bind nothing. The standing rules:

- Determinism per binary: the same seed, config and binary produce the same
  tape and the same measurement, always. Cross-version, cross-toolchain and
  cross-platform bit reproducibility is explicitly not promised.
- Statistical gates are the correctness story: a change that moves generated
  behavior re-runs the realism gates and fit tolerances rather than being
  forbidden. Floats may drift; a change that moves bits in a committed
  artifact is closed by re-blessing the artifact.
- Exact-equality gates (goldens, transcripts) are free refactoring checks. A
  change that legitimately moves output re-blesses them knowingly in the same
  change, never widens them into a tolerance to keep an old blessing alive.
- `TAPE_PROTOCOL_VERSION` bumps are free - no consumer has ever depended on a
  tape identity - so the unconditional bump rule above forecloses nothing.
- Runtime cost is first-class: a multi-hour computation is presumptively a
  defect to optimize before it is run. Algebraic float operations and
  `target-cpu` SIMD are sanctioned wherever a serial recursion does not
  forbid reordering, with the re-bless and gate re-runs they imply.

### Standing lessons from the 2026-08 bug-hunt arcs

The full distillation is `reference/test-doctrine.md` - the defect families
with their sub-shapes, the bite-check hazards, the lane and fixture
disciplines, the diagnostic rules, and the process facts about what a green
anything is worth. It is binding: read it before laying, bite-checking or
judging any test, and before closing any finding. What must never be skipped
in any session:

- The frontier family: a watermark, cursor or frontier may only advance over
  work whose success the same expression checked. A lookup that legitimately
  returns nothing is exactly as dangerous as a panic; treat any watermark
  assignment not guarded by the success of the work it covers as a defect on
  sight.
- The guard-scope family: a permit, lock or guard whose scope ends before the
  work it protects, visible by asking what is still resident when the guard
  drops. A guard must be owned by the task doing the work, because the
  awaiting future can be dropped first.
- The vacuous-gate family, by far the largest: a thing that reads as gated
  and is not, invisible because both halves are green. When a comment says a
  function guarantees something, either the function guarantees it or the
  comment is a defect; there is no third reading.
- Bite-check every new regression test: revert the production fix as a text
  edit, observe the named failure, restore it as a text edit - never with
  `git checkout -- <path>`, which has destroyed uncommitted work in the same
  file twice. Read which assertion fired and check it can fire only for the
  reason you mean.
- Run the socket suites after any change to the serving path: `brokkr check
  --gate` is the invocation that covers them; plain `brokkr check` is blind
  to roughly thirty loopback-binding tests and a real regression has shipped
  red through that gap.
- Commit or stash before reading a `brokkr test -p mogwai-cli ""` result:
  that filter catches a test that refuses a dirty tree by design and fails
  rather than skips.
- `analysis/asia_jump_probe.py` is the owner's untracked work in progress.
  It is out of scope, stays untracked, and is never swept into a commit. Its
  percentile convention does not match
  `mogwai_lab::kernel::nearest_rank_list`, so a number out of it must never
  land in a durable document labelled "p95" beside a Rust-computed one.
- `reference/INVENTORY.md` does not exist in this repository. The generic
  orchestration workflow names it as a mandatory read; it is an
  unsubstituted variable from another repo. Do not put it in a brief. The
  real contracts are `AGENTS.md` and `CLAUDE.md`, then
  `reference/north-star.md`, `architecture.md`, `clock.md`, `glossary.md`,
  `performance.md`, `test-doctrine.md` and
  `technical-implementation-spec.md`.

### Standing lessons from the 2026-08-26 backlog adjudication

`notes/bugs.md` and `notes/bugs-engine.md` were adjudicated entry by entry and
deleted; `notes/todo.md` is now the only backlog. Roughly a third of what was in
them was not work at all, and the three shapes it took are worth recognising
before filing anything.

- **Read the code site before the entry's prose.** Four entries were still
  asking a question the code had already answered, in a doc comment at the exact
  site the entry named. An entry naming a symbol is a pointer to that symbol's
  own documentation first and a claim second.
- **A settled thing filed as an open question re-opens itself forever.** Writing
  "undecided" next to a decision that was in fact made is an invitation to every
  later reader to re-derive it. If it is ruled, say so and record the ruling
  where the mechanism lives; do not leave it phrased as a fork.
- **An entry earns its place only if something in this tree could change to
  close it.** A true observation about where this repository ends is not a
  defect. We cannot prove a third-party framework calls our code; we cannot
  detect a widened conformance tolerance without new measurement; a lint cannot
  separate an assertion message from a wire payload. All correct, none of them
  work. Record such a limit at the site it constrains.

One standing owner ruling belongs here because fresh readers keep re-filing it:
**tape fidelity is not a prerequisite for exchange machinery.** All the
machinery of a real exchange can be built against the tapes we have, and better
tapes are gated on tape research v2 regardless. An instrument class with no
fitted preset is a known and accepted state, not a finding.

### Reading vs depending on nautilus_trader and broadarrow

The broadarrow adapter (and any spec or implementation that touches the nautilus
or broadarrow APIs) has two distinct access paths - never conflate them:

- Read the source from the in-tree copies `research/nautilus_trader` and
  `research/broadarrow`. Agents cannot read anything outside this repo, so these
  copies are the only place to study those APIs.
- Build against the published crates.io release, version-pinned in
  `mogwai-adapter/Cargo.toml` with default-features off so no pyo3 or Python
  linkage is pulled in. A build needs no sibling checkout: cargo fetches the
  five nautilus crates like any other dependency and `Cargo.lock` pins them by
  version and checksum. broadarrow is never a build input at all - it is the
  consumer that depends on this workspace, not the reverse. `research/` is
  read-only reference, never a build input; `members = ["crates/*"]` already
  excludes it, so no workspace `exclude` is needed.

Every implementation spec that references these APIs states both paths: the
implementer reads from `research/` and builds against the pinned release. The
two are kept in sync, so what you read in `research/` is what compiles.

### Bash rules

- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.

## Commands

Use `brokkr` (not `cargo`) for check/test. By default output is filtered to changed files and capped at 20 diagnostics per phase.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope). Does not
  run the four socket-backed adapter test binaries (`adapter_smoke`,
  `data_client_transport`, `havoc`, `reconciliation`): they are `#[ignore]`d
  because they bind real loopback listeners, so an environment without sockets
  would fail them for reasons unrelated to the code. Fast and sandbox-safe, and
  blind to roughly 30 tests.
- `brokkr check --all` - show every diagnostic, no cap, no scope filter
- `brokkr check -p <crate>` - scope to one package (e.g. `-p mogwai-engine`). You generally do not want to run this; a single `brokkr check` is faster than 2-3 `-p` runs, and brokkr intelligently filters which warnings and errors to show you
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test -p <crate> <NAME>` - focused single-test runner. Always passes `--include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Builds dev by default (`[test] debug = true`); pass `--release` where optimization is what is measured. Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time. Defaults to `--all-features`.
  - `-p, --package <PKG>` - cargo package. Required in this workspace - no default package.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - Example: `brokkr test -p mogwai-data memory_source_replays_in_time_order` or `brokkr test -p mogwai-data parses_integer_and_fractional_timestamps -N 5`.
- `brokkr run [NAME] [ARGS]...` - runs a bin or example by target name, discovered from cargo metadata across the whole workspace. This venue is `brokkr run mogwai -- serve` (`mogwai` is the bin target, not the package). A bare `brokkr run` lists what is runnable, or runs `[bin] default` if set. Arguments after `--` are forwarded raw to the program; brokkr's own `--debug`/`--release` go before the name. Use instead of `cargo run` for the same reason as `brokkr check`/`brokkr test`.

## Benchmarking

`brokkr mogwai` measures, `brokkr results` queries the durable record, and
`brokkr sidecar` queries the profiler store. The complete manual is bundled
with the tool - `brokkr man mogwai`, `brokkr man results`, `brokkr man
measure` and `brokkr man output-channels` - and `reference/performance.md`
carries the durable numbers and the annotation discipline; this section
deliberately repeats none of it. What binds every use:

- There are no layers and no frozen workloads. The argv is composed at the
  call site and captured verbatim in the row, so selecting an arm is a query
  rather than a name lookup. Adding a surface to the measurable set is
  registering a target under `[mogwai.targets.*]` in `brokkr.toml`.
- A wall alone cannot distinguish "the code got faster" from "the code did
  less": a benched surface emits its work size as stderr `key=value`
  counters, and `--compare` reports the ones that moved. Where a moved count
  invalidates a series - a seeded tape whose draw moved - that owes a
  `TAPE_PROTOCOL_VERSION` bump, which cannot be waived per comparison.
- Recording needs a clean tree; `--force` runs a dirty tree but stores no
  `results.db` row.
- A target registering `hotpath` has its `--bench` walls measured on an
  instrumented build. Register the feature on the target that needs it,
  never on every target.

## Document folders

The standing layout, across every project. Three live folders plus one retired,
split by durability first, subject second.

| Folder | Contents | Rule |
|---|---|---|
| `reference/` | Durable in-repo reference for anyone working on or with the code - how the thing is built and why: `architecture.md`, `technical-implementation-spec.md`, `performance.md` (the durable record of measured numbers over time), invariants, protocol contracts | Citable from source as a source of truth. What it says must be true. |
| `docs/` | Durable in-repo documentation of how the thing is used - guides, CLI reference, the consumer-facing API surface. Sometimes exposed as a hand-edited VitePress gh-pages site | Same must-be-true rule. |
| `notes/` | Transient - work items (`todo.md`), future plans, hypotheticals, bug reports, research, analysis. Things that will die | No truth guarantee. Nothing durable cites it. |
| `plans/` | Retired | Plan documents are transient: they go in `notes/`. |

`reference/` and `docs/` are both durable and both binding. The difference is
subject, not audience: `reference/` covers how the thing is built and why - what
you need in order to change it safely - while `docs/` covers how it is used. A
developer or library consumer reads both. Where a project publishes a site,
`docs/` is what gets published; the folder means the same thing either way.
`notes/` is neither durable nor binding, which is the whole point of keeping it
separate: a document that may be wrong must not sit where a document that must
be right is expected.

Two documents in `reference/` are exempt from the must-be-true rule, and they
are the most binding documents in the repository: `reference/north-star.md`
states what the whole system is when it is finished, and
`reference/glossary.md` states the end-state vocabulary. Both describe the end
state rather than the present. Where either and the code disagree, the code
owes the change - the entry is not stale, the tree is behind - and correcting
either to match current behaviour is the one edit that is always wrong there.
Their own preambles say so, and only the owner changes what they aim at.

The exemption is narrow and does not spread past those two. Every other file
under `reference/` and `docs/` describes what is true now, so a change that
moves behaviour moves them in the same commit. When such a file must record
something the code has not caught up with, it says so in as many words and
names it as owed, rather than quietly asserting the end state as though it had
landed.

The dependency direction is therefore one-way. `notes/` may cite `docs/` and
`reference/`; nothing durable may cite `notes/` - not a code comment, not
`docs/`, not `reference/`. A code comment must carry its full context, because
it outlives the note.

**Root-level convention files are exempt.** `AGENTS.md`, `CLAUDE.md`,
`README.md`, `LICENSE`, `CHANGELOG.md` and their kin are found by tooling and by
convention at the repository root, and stay there. These folders govern
documents we chose where to put, not files whose location is dictated.

In `notes/`, `docs/` and `reference/` alike, avoid citing source line numbers -
they drift fast.
