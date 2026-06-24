# mogwai command line

The workspace ships one binary, **`mogwai`** (the axum gateway, built from the
`mogwai-server` crate - the binary is named `mogwai`, the package
`mogwai-server`). The other crates are libraries: `mogwai-protocol`,
`mogwai-engine` and `mogwai-data` are broker internals, and `mogwai-adapter` is
the nautilus venue adapter broadarrow loads in-process - none has a `main`. This
document is the command-line surface of the server binary; `reference/config.md`
covers the TOML file it loads, and `reference/architecture.md` covers the
HTTP/WS routes it serves.

## Running

```sh
brokkr run -p mogwai-server
```

`-p mogwai-server` is the package (cargo scopes by package name); the binary it
runs is `mogwai`. An installed build (`cargo install --path crates/mogwai-server`)
is invoked directly as `mogwai`.

`brokkr run` is a thin wrapper over `cargo run`; arguments after a `--`
separator are forwarded to the binary, not to cargo:

```sh
brokkr run -p mogwai-server -- --config scripts/smoke-heartbeat.toml
```

## Flags

The argument grammar is clap-parsed: `--help`/`-h` and `--version`/`-V` work,
unknown flags and bad values are rejected with a usage error (not silently
ignored), and each subcommand carries its own `--help`.

| Flag | Argument | Default | Effect |
| --- | --- | --- | --- |
| `--config` | path | `mogwai.toml` in the working directory | Load the run config from this TOML file. A missing file falls back to built-in defaults; a malformed file is a hard error. See `reference/config.md`. |
| `--version`, `-V` | none | - | Print `mogwai <semver> (<git-hash> <build-time> UTC)` and exit. The hash carries a `-dirty` suffix when the tree had uncommitted changes at build time, and is `unknown` when built outside a checkout. Stamped at compile time by the crate's `build.rs`. |
| `--help`, `-h` | none | - | Print usage and exit. `mogwai man --help` prints the subcommand's help. |

With no subcommand, `mogwai` runs the server. Run knobs live in the config file
by design, not in flags or the environment.

## Subcommands

### `man [TOPIC]`

Render the bundled reference docs to the terminal. The `reference/*.md` contracts
are compiled into the binary with `include_str!`, so an installed `mogwai`
carries its own reference with nothing to ship alongside.

- `mogwai man` lists the available topics.
- `mogwai man <topic>` renders one as styled markdown. Topics: `cli`, `config`,
  `architecture`, `havoc` (the user-facing reference docs; the `orchestrate` and
  `technical-implementation-spec` process docs are deliberately not bundled).
- The topic is a clap value, so an unknown one is rejected with the valid set,
  and `mogwai man --help` lists the topics with a one-line description each.

Colour is auto-disabled when stdout is not a TTY or `NO_COLOR` is set, and a
closed downstream pipe (`mogwai man havoc | less`, quit early) is a clean exit.

## Environment

| Variable | Default | Effect |
| --- | --- | --- |
| `RUST_LOG` | `mogwai_server=info` | Standard `tracing` env-filter directive. The one deliberate exception to the no-ambient-environment rule for run knobs: log level is the universally-expected env var, not a run knob. An unset or unparseable value falls back to `mogwai_server=info`. |

## Listen address

The server binds `127.0.0.1:8787`, hardcoded. It is not yet configurable from
the command line or the config file; the adapter's default server URL targets
the same `8787` port.
