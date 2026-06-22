# AGENTS.md

## Project

mogwai is a fake broker/exchange that plugs into broadarrow to exercise the
*live* trading path. It replays Kraken trade history as market data and injects
the messy, realistic execution divergences (partial fills, rejects, delays,
duplicate fills, dropped account updates, venue blackouts) that an in-process
backtest sandbox structurally cannot produce. mogwai never imports nautilus; the
client-side `ExecutionClient`/`DataClient` adapters live in broadarrow and speak
this workspace's native JSON-over-WS protocol over the wire.

## Workspace

A Cargo workspace, four crates under `crates/`:

- `mogwai-protocol` - the wire types (`ClientMessage`, `ServerMessage`) plus
  `control::Divergence`. The single source of truth both ends serialize against;
  it never imports nautilus.
- `mogwai-engine` - the venue-agnostic exchange core, with the seam that injects
  armed divergences into the event stream.
- `mogwai-data` - the streaming Kraken CSV loader (O(1) memory over multi-GB
  files) and the k-way `MergeSource` that merges several pairs into one
  time-ordered stream.
- `mogwai-server` - the axum binary that owns the sockets, the clock and replay
  pacing, exposing `/health`, `/ws` and `/control/divergence`.

`scripts/` holds the end-to-end smoke test and the orchestration codex wrappers;
`docs/` is transient TODO and notes; `reference/` is durable process docs.

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
- Cargo path-dependencies point at the sibling checkouts `../nautilus_trader` and
  `../broadarrow` (nautilus with default-features off, so no pyo3 or Python
  linkage is pulled in), mirroring the split broadarrow itself uses. `research/`
  is read-only reference, never a build input; `members = ["crates/*"]` already
  excludes it, so no workspace `exclude` is needed.

Every implementation spec that references these APIs states both paths, so the
implementer reads from `research/` and depends on `../`.

### Bash rules

- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`.
- Never capture stdout into env vars (`UUID=$(...)`).
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.

### git commit rules

- Always run `brokkr fmt` before a commit.
- Never commit markdown changes alone. Bundle them with upcoming code commits.
- When committing other changes: always tag along markdown files if dirty.
- Write substantive engineering-focused commit messages.
- Hard-wrap the message body at ~72 columns, matching the existing history; the
  subject stays one concise line. The wall-of-text we keep producing comes from
  `git commit -m "<whole paragraph>"`: a single `-m` is recorded as ONE unwrapped
  line. Embed real line breaks so every body line wraps at ~72 (one `-m` per
  paragraph is fine only when each paragraph already carries its own newlines).
  Newlines are not metacharacters, so this composes with the no-metacharacters-in
  `-m` rule (CLAUDE.md Bash rules) - wrap with literal newlines while still
  avoiding braces, brackets, parens, angle brackets and the hash sign.
- Has `Cargo.lock` changed? Commit it.
- Never `git push` unless the user explicitly asks. Stop after the commit.

## Commands

Use `brokkr` (not `cargo`) for check/test. By default output is filtered to changed files and capped at 20 diagnostics per phase.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope)
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
- `brokkr run [ARGS]...` - thin wrapper over `cargo run`; forwards all arguments raw. Use instead of `cargo run` for the same reason as `brokkr check`/`brokkr test`.
