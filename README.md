# mogwai

A fake broker/exchange that plugs into broadarrow to exercise the *live*
trading path. It synthesizes market data from a committed fingerprint fitted
offline to Kraken trade history (the running server opens no CSV) and injects
the messy, realistic execution divergences - partial fills, rejects, ack
delays, duplicate fills, dropped account updates, venue blackouts, silent data
stalls - that an in-process backtest sandbox structurally cannot produce.

A Cargo workspace of five crates under `crates/`:

- `mogwai-protocol` - the JSON-over-WS wire types and the divergence catalog.
- `mogwai-engine` - the venue-agnostic exchange core and divergence seam.
- `mogwai-data` - the synthetic generator and the k-way tick merge.
- `mogwai-server` - the axum binary owning sockets, clock and replay pacing.
- `mogwai-adapter` - the nautilus venue adapter broadarrow registers for the
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
cargo run -p mogwai-server -- serve -f   # foreground gateway on 127.0.0.1:8787
```

To put a `mogwai` binary on your `PATH` instead, install it from the crate path
- it is never published to crates.io, so the `--path` form is the only one that
resolves (the package is `mogwai-server`; the binary it installs is `mogwai`):

```sh
cargo install --path crates/mogwai-server
```

This build graph excludes `mogwai-adapter`, so it needs no `../nautilus_trader`
sibling, and the fingerprint is embedded at compile time - the binary is
self-contained. `mogwai serve` daemonizes by default, writes `mogwai.pid`, and
keeps reading `mogwai.toml` from its working directory (or `serve --config
<path>`), so an installed server needs a config alongside wherever you run it.
Use `mogwai serve -f` for a foreground process, and `mogwai stop` to stop a
daemon; see [`reference/cli.md`](reference/cli.md).

## Smoke test

With a server running (`cargo run -p mogwai-server -- serve -f`, which paces
correctly at the default `speed = 1.0`):

```sh
python3 scripts/smoke.py
```

It arms divergences over the control plane and submits orders over the native
WS gateway, asserting the resulting execution events. Stdlib only - nothing to
install.

## Documentation

- [`reference/architecture.md`](reference/architecture.md) - how the system
  works, subsystem by subsystem, including the HTTP/WS routes.
- [`reference/havoc.md`](reference/havoc.md) - the havoc model: every divergence
  variant, the four havoc surfaces, and the validation boundaries.
- [`reference/config.md`](reference/config.md) - the `mogwai.toml` run knobs.
- [`reference/cli.md`](reference/cli.md) - the `mogwai` command line.

These four are bundled into the binary: `mogwai man <topic>` (`cli`, `config`,
`architecture`, `havoc`) renders them in the terminal, and `mogwai man` lists
them.

Codebase conventions and build rules live in `AGENTS.md`. The transient TODO is
`docs/todo.md`; the offline fingerprint-fitting pipeline is `analysis/`.

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
