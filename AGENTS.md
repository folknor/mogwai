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
  and that is load-bearing: `brokkr run mogwai`, the shipped launcher and
  `analysis/mnq_fit.py` all exec `target/release/mogwai` by that name. The
  socket-backed lifecycle/serving/completion integration tests live here too,
  because only this crate's tests get `CARGO_BIN_EXE_mogwai`.
- `mogwai-lab` - the corpus-to-fingerprint method library the 2026-08
  Python-to-Rust rewrite absorbed from `analysis/` (`notes/rust-rewrite-phases.md`):
  TBBO/Binance-trades corpus parsing, the protocol-12a measurement engine,
  fingerprint/cadence synthesis and the protocol-11 fit. Depends on
  `mogwai-data`, `mogwai-protocol` and `mogwai-server` (session-summary work
  resolves an `InstrumentProfile` through `Config::load`); `mogwai-server`
  depends on none of it, so there is no cycle. `mogwai-cli` calls it for
  `preflight`, `measure`, `fit`, `cache` and `synth`.
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
- When asked to write a plan or a specification, read `reference/technical-implementation-spec.md` first; it defines what such a document must contain.

### Tape protocol version

Any change to the tape generation path MUST bump
`mogwai_data::TAPE_PROTOCOL_VERSION`. This includes a generator constant, an
arrival-clock or GARCH parameter, the committed fingerprint, seed derivation,
the fill band's draw, or the tape origin. Nothing can detect that a
determinism-affecting change should have bumped the version and did not.

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
