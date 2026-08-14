# AGENTS.md

## Project

mogwai is a fake broker/exchange that plugs into a nautilus trading system to
exercise the *live* trading path. It synthesizes market data from a committed fingerprint
fitted offline to Kraken trade history (the running server opens no CSV) and
injects the messy, realistic execution divergences (partial fills, rejects,
delays,
duplicate fills, dropped account updates, venue blackouts) that an in-process
backtest sandbox structurally cannot produce. The broker core never imports
nautilus; the `mogwai-adapter` crate is the lone, deliberate exception - it
depends on the published nautilus crates to ship the
`ExecutionClient`/`DataClient` pair a host constructs to drive the `MOGWAI`
venue over this workspace's native JSON-over-WS protocol.

The INSTRUMENT SET IS OPEN. The three shipped presets - MNQ, MES and
BTCUSDT - are the
current state, not the end state - whatever gets traded next, the venue owes
it a realistic tape, which means a corpus, a measurement, a fit and a preset
for that symbol. (ETHUSDT and SOLUSDT shipped for a while as BTCUSDT aliases -
identical generator paths, tapes differing only in the symbol identity - and
were retired 2026-08-09 by owner ruling; MES remains a stated stopgap
borrowing the MNQ fit.) The offline toolbox (`mogwai-lab` and what survives in
`analysis/`) is therefore reusable intake machinery, not one instrument's
history: treat a component as spent only when its QUESTION cannot recur, and
keep per-instrument knowledge in config or a preset rather than hardcoded in
the method. See the architecture note on the intake sequence for what this
binds.

## Workspace

A Cargo workspace, seven crates under `crates/`:

- `mogwai-protocol` - the wire types (`ClientMessage`, `ServerMessage`) plus
  `control::Divergence`. The single source of truth both ends serialize against;
  it never imports nautilus. Also carries `launch`, the SHIPPED launcher: it
  lives here rather than in `mogwai-adapter` because the server's own test
  binaries drive the venue through it and cannot depend on the adapter, so a
  launcher shipped from there would leave the contract hand-rolled on both sides.
  `mogwai-adapter` re-exports it for consumers that already depend on it.
- `mogwai-engine` - the venue-agnostic exchange core, with the seam that injects
  armed divergences into the event stream.
- `mogwai-data` - the `TickSource` seam and the k-way `MergeSource`. Carries the
  `GeneratedSource` synthetic generator the running server uses (fitted to the
  committed fingerprint) plus the `KrakenCsvSource` streaming loader kept as the
  offline-analysis lineage.
- `mogwai-server` - the axum LIBRARY that owns the sockets, the clock and replay
  pacing, synthesizing market data per subscription; exposes `/health`, `/ws`,
  `/control/divergence`, `/instruments`, `/trades`, `/quotes`, `/account` and
  `/clock`. Order entry is websocket-only - the `POST /orders` carrier went with
  the HTTP transport profiles. `serve` runs
  ONE venue in the foreground for one run and owns no PID, log or config files:
  it reports its bound address as a single JSON readiness line on stdout,
  and `PR_SET_PDEATHSIG` makes the kernel kill it when its launcher dies. There
  is no daemon mode and no `stop` subcommand - lifecycle is the launcher's job.
  See `docs/cli.md`. It ships no binary: `serve`, `config` and `source` are its
  public surface and `mogwai-cli` calls them.
- `mogwai-cli` - the `mogwai` BINARY. A clap dispatcher plus the offline
  subcommands that never bind a socket: `gen` (the tape generator and its
  measurement consumers, including `measure12a`), `tick-composition`, `presets`
  and `man`. `serve` does no work here - it hands its three arguments to
  `mogwai_server::serve`. The bin TARGET name is `mogwai`, not the package name,
  and that is load-bearing: `brokkr run mogwai` and the shipped launcher both
  exec `target/release/mogwai` by that name. The Python measurement harness
  did too, until phase 4b retired it; renaming the target no longer breaks it,
  because it is gone, but the launcher and every doc still key on the name. The
  socket-backed lifecycle/serving/completion integration tests live here too,
  because only this crate's tests get `CARGO_BIN_EXE_mogwai`.
- `mogwai-lab` - the corpus-to-fingerprint method library the 2026-08
  Python-to-Rust rewrite absorbed from `analysis/` (the rewrite's phase
  records are retired to git history):
  TBBO/Binance-trades corpus parsing, the protocol-12a measurement engine,
  fingerprint/cadence synthesis and the protocol-11 fit. Depends on
  `mogwai-data`, `mogwai-protocol` and `mogwai-server` (session-summary work
  resolves an `InstrumentProfile` through `Config::load`); `mogwai-server`
  depends on none of it, so there is no cycle. `mogwai-cli` calls it for
  `preflight`, `measure`, `fit`, `cache` and `synth`. It also carries
  `sidecar`, the observation-only benchmarking channels (stderr `key=value`
  scalars and the marker/counter FIFO) the benched commands report through;
  see `reference/performance.md`.
- `mogwai-adapter` - the nautilus venue adapter: the `MogwaiDataClientFactory` /
  `MogwaiExecutionClientFactory`, their configs, and the client pair a host
  registers for the `MOGWAI` venue. The only crate that depends on nautilus -
  the published crates.io crates pinned in its `Cargo.toml`, default-features
  off, no pyo3; the other six build nautilus-free.

`scripts/` holds the end-to-end smoke test and the harness-bug flush the
orchestration loop uses (codex is now driven by the `review` tool, configured
from `.review.toml`, not by wrapper scripts);
`analysis/` is the offline Python that fits the fingerprint; `notes/` holds the
transient work items and plans. The durable documentation is split by subject:
`docs/` is how the venue is USED (`cli`, `config`, `havoc`, `presets`,
`oms-types`) and `reference/` is how it is BUILT and why (`architecture`,
`clock`, `glossary`, `performance`, `technical-implementation-spec`). See the
Document folders section below for what each folder may and may not contain.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.

### Tape protocol version

Any change to the tape generation path MUST bump
`mogwai_data::TAPE_PROTOCOL_VERSION`. This includes a generator constant, an
arrival-clock or GARCH parameter, the committed fingerprint, seed derivation,
the fill band's draw, or the tape origin. Nothing can detect that a
determinism-affecting change should have bumped the version and did not.

The bump is owed by the COMMIT of a changed artifact, not by a change to the
code that could produce one. `mogwai-lab` depends on `mogwai-data` and never
the reverse, and the generator reads `analysis/fingerprint.json` through
`include_str!` rather than calling the synthesis - so editing the synthesis
cannot move a tape byte until a regenerated artifact actually differs.

ONE RECORDED EXEMPTION, granted 2026-08-08 and deliberately narrow. The
correction of `analysis/fingerprint.json` at
`empirical_ranges.modal_tick.max` from 0.25 to 0.1 landed WITHOUT a bump.
Grounds: exactly one JSON leaf changed, an exhaustive reader audit found its
sole consumer to be `Scalars::empirical_diagnostics`, which emits advisory
diagnostics and never reaches the generator, and fresh Python and Rust
synthesis agree on every other leaf. Version 12 was at that time reserved for
the protocol-12b mechanism change, so bumping here would have spent a tape
identity on a change that provably moves no generated byte. Since then 12 went
to the arrival-frame calibration repair and 13 to the fill-band decimal
normalization, so the mechanism landing now takes 14; the reservation moves,
the exemption's grounds do not. This exemption does NOT generalize to any
other fingerprint leaf:
the rule stays unconditional, and the next leaf needs its own audit and its
own ruling. If a non-tape artifact revision ever needs an identity of its own,
give it a content hash or a method version rather than overloading this
constant.

### The bit-exactness era is closed (owner ruling, 2026-08-09)

The Python-to-Rust rewrite's parity contract and its pinned CPython
conventions existed to prove the port against the Python oracle. That program
completed and the Python retired, so they are HISTORY, not a standing
constraint on new or optimized code:

- Bit-exactness toward the Python-era committed artifacts is no longer an
  obligation. Floats may drift. A change that moves bits in a committed
  artifact is closed by RE-BLESSING the artifact, never by preserving CPython
  arithmetic. The pinned conventions (compensated `py_sum`/`py_fsum`, exact
  integer variance, insertion-ordered accumulation, the ported Mersenne
  Twister, the CPython float repr) remain in the tree as the record of how
  parity was proven; none of them binds future work, and replacing one for
  performance needs no ruling beyond the re-bless it forces.
- What replaces bit-exactness is a two-part contract. DETERMINISM PER BINARY:
  the same seed, config and binary produce the same tape and the same
  measurement, always. STATISTICAL GATES STAY GREEN: the realism gates, fit
  tolerances and representability verdicts are the correctness story, and a
  change that moves generated behavior re-runs them rather than being
  forbidden. Cross-version, cross-toolchain and cross-platform bit
  reproducibility is explicitly NOT promised.
- Exact-equality gates (goldens, transcripts, `cmp`-based identity checks)
  remain useful as free refactoring checks: a change that claims to move
  nothing can prove it cheaply. Where a change legitimately moves output, the
  gate is re-blessed knowingly in the same change, never widened into a
  tolerance to keep an old blessing alive.
- `TAPE_PROTOCOL_VERSION` bumps are FREE: no consumer has ever depended on a
  tape identity, so the unconditional bump rule above stays (it costs one
  integer edit) and forecloses nothing.
- RUNTIME COST IS A FIRST-CLASS CONCERN. A multi-hour computation is
  presumptively a defect to optimize before it is run, not a budget to
  provision for. Performance work may change generated bytes freely under the
  contract above; in particular, algebraic float operations (stable from Rust
  1.98) and `target-cpu` SIMD are sanctioned wherever a serial recursion does
  not forbid reordering, with the re-bless and gate re-runs they imply.

### Reading vs depending on nautilus_trader and broadarrow

The broadarrow adapter (and any spec or implementation that touches the nautilus
or broadarrow APIs) has two distinct access paths - never conflate them:

- Read the source from the in-tree copies `research/nautilus_trader` and
  `research/broadarrow`. Agents cannot read anything outside this repo, so these
  copies are the only place to study those APIs.
- Build against the PUBLISHED crates.io release, version-pinned in
  `mogwai-adapter/Cargo.toml` with default-features off so no pyo3 or Python
  linkage is pulled in. A build needs no sibling checkout: cargo fetches the
  five nautilus crates like any other dependency and `Cargo.lock` pins them by
  version and checksum. broadarrow is never a build input at all - it is the
  consumer that depends on this workspace, not the reverse. `research/` is
  read-only reference, never a build input; `members = ["crates/*"]` already
  excludes it, so no workspace `exclude` is needed.

The adapter previously path-depended a sibling `../nautilus_trader` checkout
because the published release carried bugs this project hits. Those fixes landed
in 0.61, so the manifest now pins that release and the build is reproducible
from a fresh clone.

Every implementation spec that references these APIs states both paths: the
implementer reads from `research/` and builds against the pinned release. The
two are kept in sync, so what you read in `research/` is what compiles.

### Bash rules

- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`.
- Never capture stdout into env vars (`UUID=$(...)`).
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.

## Commands

Use `brokkr` (not `cargo`) for check/test. By default output is filtered to changed files and capped at 20 diagnostics per phase.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope). Does NOT
  run the four socket-backed adapter test binaries (`adapter_smoke`,
  `data_client_transport`, `havoc`, `reconciliation`): they are `#[ignore]`d
  because they bind real loopback listeners, so an environment without sockets
  would fail them for reasons unrelated to the code. Fast and sandbox-safe, and
  blind to roughly 30 tests.
- `brokkr check --all` - show every diagnostic, no cap, no scope filter
- `brokkr check -p <crate>` - scope to one package (e.g. `-p mogwai-engine`). You generally do not want to run this; a single `brokkr check` is faster than 2-3 `-p` runs, and brokkr intelligently filters which warnings and errors to show you
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test -p <crate> <NAME>` - release-mode focused single-test runner. Always passes `--release --include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time. Defaults to `--all-features`.
  - `-p, --package <PKG>` - cargo package. Required in this workspace - no default package, and overrides `[test] default_package` in `brokkr.toml` if set.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - `--debug` - build and run the test in dev profile instead of release. Use this for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time (3-4 min for the full workspace) dominates wall time and the optimization level doesn't change the behavior under test. `BROKKR_TEST_BIN_DIR` points at `<target>/debug` accordingly.
  - Example: `brokkr test -p mogwai-data memory_source_replays_in_time_order` or `brokkr test -p mogwai-data parses_integer_and_fractional_timestamps -N 5`.
- `brokkr run [NAME] [ARGS]...` - runs a bin or example by TARGET NAME, discovered from cargo metadata across the whole workspace. This server is `brokkr run mogwai -- serve` (`mogwai` is the bin target, not the package). A bare `brokkr run` lists what is runnable, or runs `[bin] default` if set. Arguments after `--` are forwarded raw to the program; brokkr's own `--debug`/`--release` go before the name. Use instead of `cargo run` for the same reason as `brokkr check`/`brokkr test`.

## Benchmarking

`brokkr mogwai` measures, `brokkr results` queries the durable record, and
`brokkr sidecar` queries the profiler store. In depth: `brokkr man mogwai`
and `brokkr man results`. The design and what is deliberately deferred:
`notes/benchmarking-design.md`. What each surface emits and the annotation
discipline: `reference/performance.md`.

There are no layers and no frozen workloads. The argv is composed at the
CALL SITE and captured verbatim in the row, so selecting an arm is a
query rather than a name lookup. Recording needs a clean tree: `--force`
runs a dirty tree but stores no `results.db` row (its sidecar data
survives, reachable as the `dirty` pseudo-UUID).

### The two surfaces

**Argv-shaped**, through the shipped `target/release/mogwai`, needing no
registration - `gen` and its `--type` variants, `tick-composition`,
`preflight`, `measure`, `fit`, `cache`, `synth`, `arrival-screen`. The
argv goes after `--`, raw:

    brokkr mogwai --bench 3 -- gen --type summary --symbol MNQ

Benching the release binary measures what ships, startup and argument
parsing included, which is the honest end-to-end number.

**Harness-shaped**, through a cargo example, for the loops that have no
command line - the engine's matching loop and divergence seam, the
`TickSource` implementations, the arrival draw, the screen's projection,
and eventually the serving path and the adapter. These resolve by NAME
against `[mogwai.targets.*]` in `brokkr.toml`:

    brokkr mogwai screen_projection --hotpath
    brokkr mogwai arrival_walk --alloc

Harnesses take an argv too, after `--`, because every surface here is
config-shaped (preset, window, seed, cell). Bare `brokkr mogwai` lists
both kinds. Adding a surface to the measurable set is registering a
target.

Currently registered:

| target | example | features |
|---|---|---|
| `arrival_walk` | `mogwai-data/arrival_walk_bench` | `hotpath` |
| `screen_projection` | `mogwai-lab/screen_projection_bench` | `hotpath` |

### The three modes, uniform over both surfaces

- `--bench [N]` - N runs (default 3), lockfile, stores a row. A plain run
  stores nothing.
- `--hotpath [N]` - function-level timing.
- `--alloc [N]` - per-function allocation bytes, exclusive of nested
  calls.

`--hotpath` and `--alloc` are INERT without the feature that compiles the
instrumentation in, which is what the registry's `features` field exists
for. The registered features are a UNION with what the mode and call site
add, never a replacement: `--hotpath` contributes `hotpath`, `--alloc`
contributes `hotpath-alloc`, and a call-site `--features` adds an arm.

The consequence worth knowing, and it bites: **a target registering
`hotpath` has its `--bench` walls measured on an instrumented build.**
Register the feature on the target that needs it, not on every target. A
harness whose canonical output is a wall rather than a profile should
carry no registered features and no cargo `required-features`.

Other flags that matter: `--commit <REF>` builds and benches an old
commit, `--dry-run` validates argv and path resolution without building,
and `--stop <MARKER>` kills the child when a sidecar marker fires, for
benching one phase.

### Reading the record

`brokkr results` prints the last `-n` rows (default 20), newest first.
Filters AND together: `--commit`, `--command`, `--mode` (`bench`,
`hotpath`, `alloc`), `--dataset`, and repeatable `--meta KEY=VALUE` /
`--env NAME=VALUE`. Both `--meta` and `--env` EXCLUDE rows missing the
key, so an arm defined by an unset variable needs an explicit baseline
value recorded on the off runs.

`--grep` / `--grep-v` match against the whole INVOCATION - subprocess
argv, brokkr argv, and each captured env var rendered as `NAME=VALUE`.
Repeatable, `git log --grep` style: every `--grep` must match, any
`--grep-v` hit excludes. That composition is the only way to select an
arm defined by an ABSENT flag.

`brokkr results <uuid-prefix>` resolves to one row and prints a labelled
block: full `cli_args`, the brokkr invocation, per-iteration walls for a
`--bench N` row, the scraped counters, the `prev.*` provenance of what
ran immediately before (often the explanation for an outlier), and the
hotpath and alloc tables when the mode captured them. `--top N` caps the
functions shown.

`brokkr results --compare A B` pairs rows on
`(command, mode, input_file, brokkr_args, env_fingerprint)`, with
`--commit` and `--verbose` stripped from `brokkr_args` first. Pairs whose
host conditions or captured env differed are ANNOTATED rather than
rendered as a clean delta.

### Counters, and why they are not optional

Every external run scrapes the winning run's stderr for `key=value`
counters. `--compare` reports the ones that moved:

    counters: cells_evaluated 5000 -> 4000, prints 1240 -> 1180

A wall alone cannot distinguish "the code got faster" from "the code did
less". This is what turns "12 % faster" into "12 % faster on 8 % fewer
cells", and it is why a benched surface should emit its WORK SIZE. It is
reported, never fatal. Where a moved count really does invalidate a
series - a seeded tape whose draw moved - that owes a
`TAPE_PROTOCOL_VERSION` bump, which is unconditional and cannot be waived
per comparison.

### The sidecar

`brokkr sidecar <uuid>` queries `.brokkr/sidecar.db` - the observation-only
channels the benched commands report through (`mogwai-lab`'s `sidecar`
module). A UUID prefix is required; find one with `brokkr results`. Views:
`--markers` and `--durations` (START/END pair timings), `--counters`
(with `--grep` to filter a noisy dump), `--samples` (the raw 100 ms /proc
stream, with `--phase`, `--range`, `--where`, `--fields`, `--every`,
`--head`, `--tail`), `--stat <FIELD>` for min/max/avg/p50/p95, `--stalls`
for `*_wait_ns` rollups, and `--compare A B` phase-aligned. `--human`
renders a table instead of JSONL.

### Datasets

`[<host>.datasets.<name>]` in `brokkr.toml` records an out-of-git input
by path and XXH128 digest - whether the bytes moved under a recorded row.
This is NOT a substitute for the run's own content verification, which
asks whether the data is what the ledger says. Register the path, run
`brokkr env`, and paste back the digest it computed from disk. A `path`
may name a directory, which digests as the sorted fold of
`<relative path>\0<file digest>`, so a rename or a layout change moves
it.

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
