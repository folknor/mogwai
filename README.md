# mogwai

A fake broker/exchange that plugs into a nautilus trading system to exercise
the *live* trading path. It synthesizes market data from a committed fingerprint fitted
offline to Kraken trade history (the running server opens no CSV) and injects
the messy, realistic execution divergences - partial fills, rejects, ack
delays, duplicate fills, dropped account updates, venue blackouts, silent data
stalls - that an in-process backtest sandbox structurally cannot produce.

A Cargo workspace of five crates under `crates/`:

- `mogwai-protocol` - the JSON-over-WS wire types and the divergence catalog.
- `mogwai-engine` - the venue-agnostic exchange core and divergence seam.
- `mogwai-data` - the synthetic generator and the k-way tick merge.
- `mogwai-server` - the axum binary owning sockets, clock and replay pacing.
- `mogwai-adapter` - the nautilus venue adapter a host registers for the
  `MOGWAI` venue; the only crate that imports nautilus.

## Building

The four broker crates build nautilus-free. `mogwai-adapter` depends on the
published nautilus crates from crates.io, pinned in its `Cargo.toml`
(default-features off, no pyo3), so a full build needs no sibling checkout -
cargo fetches nautilus like any other dependency.

Use `cargo` for check/test/run:

```sh
cargo clippy --all-targets               # lints
cargo test                               # the test suite
cargo run -p mogwai-server -- serve      # one venue, endpoint printed on stdout
```

`serve` takes no address: it always binds loopback on an ephemeral port, so two
of those commands running at once cannot collide and cannot be pointed at each
other. The bound address comes back as one line of JSON on stdout - a launcher
captures it, a human reads it off the terminal.

To put a `mogwai` binary on your `PATH` instead, install it from the crate path
- it is never published to crates.io, so the `--path` form is the only one that
resolves (the package is `mogwai-server`; the binary it installs is `mogwai`):

```sh
cargo install --path crates/mogwai-server
```

This build graph excludes `mogwai-adapter`, so it pulls no nautilus crates at
all, and the fingerprint and instrument presets are embedded at compile time -
the binary is self-contained, needing no data directory. `mogwai serve` runs one
venue in the FOREGROUND for one run and owns no PID, log or config files: it
never consults the working directory, so pass `serve --config <path>` to use
anything but the built-in defaults. There is no daemon mode and no `stop`
subcommand - the launcher owns the lifecycle, reading the bound address from the
readiness line on stdout; see [`reference/cli.md`](reference/cli.md).

Linux only, for now. The venue arms `PR_SET_PDEATHSIG` so the kernel terminates
it when its launcher dies, which is the whole of its cleanup story - there is no
PID file and no `stop` to fall back on - and that call is unconditional, so
`mogwai-server` does not build elsewhere.

## Usage

```sh
mogwai serve --config run.toml     # one venue, one run, foreground
mogwai presets MNQ                 # print a built-in instrument preset
mogwai gen --help                  # dump a tape offline, no venue involved
mogwai man cli                     # read a bundled doc; bare `man` lists topics
```

One venue is ONE RUN: one instrument, one account, one ledger, on an ephemeral
loopback port. It is not a service you start once and point many strategies at -
two independent forward tests are two processes, not two clients of one venue.
Sockets may still be many (the adapter alone opens two, data and execution), but
they all speak for the same account, so anything they submit lands in one shared
ledger.

Normally a launcher starts the venue, and normally that launcher is an agent
rather than a person. The contract is four steps:

1. Spawn `mogwai serve` as a **direct** child, capturing stdout.
2. Read one line of stdout: a JSON `ReadyRecord`. Check its `version`, then use
   its `addr`. Stdout closing without a line means the venue failed to boot, and
   its stderr says why. The read blocks for as long as warmup generation takes.
3. Drain stderr continuously, or send it to a file or the null device. Logs go
   to stderr by design, and a pipe nobody reads fills up and wedges the venue.
4. On `RunComplete` the venue exits 0 by itself; otherwise terminate it.

"Direct child" is load-bearing rather than stylistic: the venue arms
`PR_SET_PDEATHSIG` against its immediate parent, so a shell, a `cargo run`, or a
double fork in between wires the death watch to the wrapper and leaves a real
orphan behind. `scripts/smoke.py` is this contract executed, and is the reference
to copy from.

## Documentation

- [`reference/architecture.md`](reference/architecture.md) - how the system
  works, subsystem by subsystem, including the HTTP/WS routes.
- [`reference/havoc.md`](reference/havoc.md) - the havoc model: every divergence
  variant, the four havoc surfaces, and the validation boundaries.
- [`reference/config.md`](reference/config.md) - the `mogwai.toml` run knobs.
- [`reference/cli.md`](reference/cli.md) - the `mogwai` command line.
- [`docs/presets.md`](docs/presets.md) - choosing one of the five instrument
  presets shipped inside the binary, and reading its provenance.
- [`docs/oms-types.md`](docs/oms-types.md) - the run-level `oms_type` choice,
  and how netting and hedging differ on the wire.

All seven are compiled into the binary: `mogwai man <topic>` renders one in the
terminal and `mogwai man` lists them, so an installed `mogwai` carries its own
documentation with no source tree present. Colour is dropped when stdout is not
a terminal or `NO_COLOR` is set.

Codebase conventions and build rules live in `AGENTS.md`. Transient work items,
plans and analysis live in `notes/`; the offline fingerprint-fitting pipeline is
`analysis/`.

## License

mogwai is dual-licensed:

- **AGPL-3.0-only** for everyone - see [LICENSE](LICENSE). If you convey the
  software or offer it over a network, the AGPL's source-sharing obligations
  apply to your work.
- **Commercial licenses** for proprietary use without copyleft obligations -
  see [COMMERCIAL-LICENSE.md](COMMERCIAL-LICENSE.md).

All versions in this repository's history, including commits that predate the
LICENSE file, are licensed under the AGPL-3.0-only.

Contributions require agreeing to the [CLA](CLA.md), which assigns copyright to
the project owner and keeps dual licensing possible. You retain a full license
back to your own contributions.
