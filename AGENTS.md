# AGENTS.md

## Project

mogwai is a fake broker/exchange that plugs into broadarrow to exercise the
*live* trading path. It synthesizes market data from a committed fingerprint
fitted offline to Kraken trade history (the running server opens no CSV) and
injects the messy, realistic execution divergences (partial fills, rejects,
delays,
duplicate fills, dropped account updates, venue blackouts) that an in-process
backtest sandbox structurally cannot produce. The broker core never imports
nautilus; the `mogwai-adapter` crate is the lone, deliberate exception - it
depends on the published nautilus crates to ship the `ExecutionClient`/`DataClient` pair broadarrow
constructs to drive the `MOGWAI` venue over this workspace's native JSON-over-WS
protocol.

## Workspace

A Cargo workspace, five crates under `crates/`:

- `mogwai-protocol` - the wire types (`ClientMessage`, `ServerMessage`) plus
  `control::Divergence`. The single source of truth both ends serialize against;
  it never imports nautilus.
- `mogwai-engine` - the venue-agnostic exchange core, with the seam that injects
  armed divergences into the event stream.
- `mogwai-data` - the `TickSource` seam and the k-way `MergeSource`. Carries the
  `GeneratedSource` synthetic generator the running server uses (fitted to the
  committed fingerprint) plus the `KrakenCsvSource` streaming loader kept as the
  offline-analysis lineage.
- `mogwai-server` - the axum binary that owns the sockets, the clock, replay
  pacing, and its own daemon lifecycle (`serve` daemonizes by default, `-f` stays
  foreground, `stop` ends it via a PID-file lock), synthesizing market data per
  subscription; exposes `/health`, `/ws`, `/control/divergence`, `/orders`,
  `/instruments` and `/trades`.
- `mogwai-adapter` - the nautilus venue adapter: the `MogwaiDataClientFactory` /
  `MogwaiExecutionClientFactory`, their configs, and the client pair broadarrow
  registers for the `MOGWAI` venue. The only crate that depends on nautilus -
  the published crates.io crates pinned in its `Cargo.toml`, default-features
  off, no pyo3; the other four build nautilus-free.

`scripts/` holds the end-to-end smoke test and the harness-bug flush the
orchestration loop uses (codex is now driven by the `review` tool, configured
from `.review.toml`, not by wrapper scripts);
`analysis/` is the offline Python that fits the fingerprint; `docs/` is the
transient TODO; `reference/` is durable docs - `architecture.md` describes how
the system works, the others are process docs.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.
- ./docs/* are transient, do not reference them from code comments. Code comments should contain the full context - it will outlive the docs.
- ./reference/* is durable and lives on: Code comments may reference these.
- In general ./docs/**/*, try to refrain from referencing direct line numbers in the rust source. You can use line numbers, but they drift fast.
- When asked to write a plan or a specification, read `reference/technical-implementation-spec.md` first; it defines what such a document must contain.

### Reading vs depending on nautilus_trader and broadarrow

The broadarrow adapter (and any spec or implementation that touches the nautilus
or broadarrow APIs) has two distinct access paths - never conflate them:

- Read the source from the in-tree copies `research/nautilus_trader` and
  `research/broadarrow`. Agents cannot read anything outside this repo, so these
  copies are the only place to study those APIs.
- Build against the SIBLING CHECKOUT `../nautilus_trader`, path-depended from
  `mogwai-adapter/Cargo.toml` with default-features off so no pyo3 or Python
  linkage is pulled in. A build therefore REQUIRES that checkout to exist
  alongside this repo; it is not optional and it is not the `research/` copy.
  broadarrow is never a build input at all - it is the consumer that depends on
  this workspace, not the reverse. `research/` is read-only reference, never a
  build input; `members = ["crates/*"]` already excludes it, so no workspace
  `exclude` is needed.

The path dependency is deliberate and temporary. The published nautilus release
still carries bugs this project hits, which are being fixed upstream; once the
queued fixes land in a release, the manifest moves to pinned crates.io versions
and this section changes with it. Until then a path dep is the honest
description: it carries no version requirement and no checksum, so `Cargo.lock`
cannot pin it and whatever sits in that checkout at build time is what compiles.

Every implementation spec that references these APIs states both paths: the
implementer reads from `research/` and builds against the sibling checkout. The
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
