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

`mogwai` has three verbs, `serve`, `stop`, and `man`; there is no default verb,
so a bare `mogwai` prints help. Start a daemon with `serve`:

```sh
brokkr run -p mogwai-server -- serve
```

`-p mogwai-server` is the package (cargo scopes by package name); the binary it
runs is `mogwai`. The command returns after the socket is bound and the daemon
keeps running. Stop it with `brokkr run -p mogwai-server -- stop`, or use
`serve -f` when a foreground process is wanted. An installed build (`cargo
install --path crates/mogwai-server`) is invoked directly, e.g. `mogwai serve`
and `mogwai stop`.

`brokkr run` is a thin wrapper over `cargo run`; arguments after a `--`
separator are forwarded to the binary, not to cargo:

```sh
brokkr run -p mogwai-server -- serve -f --config scripts/smoke-heartbeat.toml
```

## Global flags

The argument grammar is clap-parsed: `--help`/`-h` and `--version`/`-V` work,
unknown flags and bad values are rejected with a usage error (not silently
ignored), each verb carries its own `--help`, and a bare `mogwai` prints help
rather than doing anything.

| Flag | Effect |
| --- | --- |
| `--version`, `-V` | Print `mogwai <semver> (<git-hash> <build-time> UTC)` and exit. The hash carries a `-dirty` suffix when the tree had uncommitted changes at build time, and is `unknown` when built outside a checkout. Stamped at compile time by the crate's `build.rs`. |
| `--help`, `-h` | Print usage and exit. `mogwai <verb> --help` prints that verb's help. |

## Subcommands

### `serve [OPTIONS]`

Run the gateway: bind the sockets, replay synthesized market data, and serve the
HTTP/WS routes. A bare `serve` daemonizes by default, writes a PID file, and
returns once the listener is ready. Pass `-f` / `--foreground` for containers,
systemd Type=simple units, and local harnesses that need a foreground process.
Its run knobs live in `mogwai.toml`, not in flags; the flags here select paths,
the bind address, and foreground mode.

| Flag | Argument | Default | Effect |
| --- | --- | --- | --- |
| `--config` | path | `mogwai.toml` in the working directory | Load the run config from this TOML file. A missing file falls back to built-in defaults; a malformed file is a hard error. See `reference/config.md`. |
| `--addr` | `host:port` | `127.0.0.1:8787` | Address to bind the gateway to. The adapter's default server URL targets `8787`, so a non-default port also needs the adapter pointed at it. |
| `--log-file` | path | `mogwai.log` in the working directory | Append structured tracing logs to this file. The readiness banner still prints to stdout in foreground mode and from the launcher parent in daemon mode. |
| `--pid-file` | path | `mogwai.pid` in the working directory | PID-file path used by daemon mode for the single-instance lock and by `stop` to find the daemon. Ignored under `-f`. |
| `--foreground`, `-f` | none | off | Stay in the foreground instead of daemonizing. |

### `stop [OPTIONS]`

Stop a daemon started by `mogwai serve`. `stop` uses the PID-file lock to decide
whether a daemon is live before it signals anything, so a stale PID file is
removed without targeting a recycled PID.

| Flag | Argument | Default | Effect |
| --- | --- | --- | --- |
| `--pid-file` | path | `mogwai.pid` in the working directory | PID-file path for the daemon to stop. Use the same value that was passed to `serve --pid-file`. |

### `man [TOPIC]`

Render the bundled reference docs to the terminal. The `reference/*.md` contracts
are compiled into the binary with `include_str!`, so an installed `mogwai`
carries its own reference with nothing to ship alongside.

- `mogwai man` lists the available topics.
- `mogwai man <topic>` renders one as styled markdown. Topics: `cli`, `config`,
  `architecture`, `havoc`, `clock` (the user-facing reference docs; the
  `orchestrate` and `technical-implementation-spec` process docs are
  deliberately not bundled).
- The topic is a clap value, so an unknown one is rejected with the valid set,
  and `mogwai man --help` lists the topics with a one-line description each.

Colour is auto-disabled when stdout is not a TTY or `NO_COLOR` is set, and a
closed downstream pipe (`mogwai man havoc | less`, quit early) is a clean exit.

## Environment

| Variable | Default | Effect |
| --- | --- | --- |
| `RUST_LOG` | `mogwai=info` | Standard `tracing` env-filter directive. The one deliberate exception to the no-ambient-environment rule for run knobs: log level is the universally-expected env var, not a run knob. An unset or unparseable value falls back to `mogwai=info`. |
