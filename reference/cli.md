# mogwai-server command line

The workspace ships one binary, `mogwai-server` (the axum gateway). The other
crates are libraries: `mogwai-protocol`, `mogwai-engine` and `mogwai-data` are
broker internals, and `mogwai-adapter` is the nautilus venue adapter broadarrow
loads in-process - none has a `main`. This document is the command-line surface
of the server binary; `reference/config.md` covers the TOML file it loads, and
`reference/architecture.md` covers the HTTP/WS routes it serves.

## Running

```sh
brokkr run -p mogwai-server
```

`brokkr run` is a thin wrapper over `cargo run`; arguments after a `--`
separator are forwarded to the binary, not to cargo:

```sh
brokkr run -p mogwai-server -- --config scripts/smoke-heartbeat.toml
```

## Flags

| Flag | Argument | Default | Effect |
| --- | --- | --- | --- |
| `--config` | path | `mogwai.toml` in the working directory | Load the run config from this TOML file. A missing file falls back to built-in defaults; a malformed file is a hard error. See `reference/config.md`. |

There is no `--help`, no subcommands, and the flag parser ignores anything it
does not recognize. Run knobs live in the config file by design, not in flags or
environment variables.

## Environment

| Variable | Default | Effect |
| --- | --- | --- |
| `RUST_LOG` | `mogwai_server=info` | Standard `tracing` env-filter directive. The one deliberate exception to the no-ambient-environment rule for run knobs: log level is the universally-expected env var, not a run knob. An unset or unparseable value falls back to `mogwai_server=info`. |

## Listen address

The server binds `127.0.0.1:8787`, hardcoded. It is not yet configurable from
the command line or the config file; the adapter's default server URL targets
the same `8787` port.
