# mogwai - TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

mogwai is a fake broker/exchange that plugs into broadarrow to exercise the
*live* trading path: it synthesizes market data from a committed fingerprint
fitted offline to Kraken trade history (the running server opens no CSV) and
injects the messy execution divergences a backtest sandbox cannot produce. Four
nautilus-free broker crates plus the `mogwai-adapter` venue adapter.

## State

The four broker crates and the nautilus adapter are built and verified end to
end (`scripts/smoke.py` plus the workspace unit and integration tests). The
running server opens no Kraken CSV: market data is synthesized by
`mogwai_data::GeneratedSource` from the committed `analysis/fingerprint.json`,
with the UTC session modulator layered on. The adapter ships the `DataClient` /
`ExecutionClient` pair, the `transport_profile` selector, and the full
four-surface `HavocSpec` (client / server / data / connection-lifecycle).
Subsystem detail is in `reference/architecture.md`; the offline analysis
that produced the fingerprint is under `analysis/` (`analysis/findings.md` is
the summary).

## Next

### 1. broadarrow integration: point a MOGWAI venue at the server

The adapter, its `transport_profile` selector, and its full four-surface
`HavocSpec` (client / server / data / connection-lifecycle) are landed (see
`reference/architecture.md`). What remains lives in broadarrow, not this repo:

- [ ] broadarrow side (lives in broadarrow, not here): a `MOGWAI` arm in
      `run-prep/src/venue.rs` and a `core::venue` PROFILES row, with a
      profile-guard test enforcing that every wired venue has a PROFILES row.
      This is where broadarrow sets the per-venue `HavocSpec` (including its
      `data` market-regime and `conn` connection-lifecycle fields) and picks the
      `transport_profile`.

### 2. Author `reference/havoc.md`

Durable reference doc for the divergence/havoc surfaces. Covers the four-surface
`HavocSpec` (client / server / data / connection-lifecycle), every `Divergence`
variant and its trigger/semantics, the `MarketRegime` axis, how the server arms
and applies them vs what the engine owns, and the validation boundaries
(`validate_divergence`, `validate_market_regime`, `validate_conn_havoc`). Read
`reference/technical-implementation-spec.md` first for what such a doc must
contain.

### 3. Remove remaining runtime env vars - knobs belong in TOML / `HavocSpec`

Run knobs should be explicit input (TOML or havoc knobs), not ambient
environment. The replay-speed / gap-cap env vars were already moved to
`mogwai.toml` (commit ca7bc66). Remaining audit:

- [ ] `tracing_subscriber::EnvFilter::try_from_default_env()` in
      `mogwai-server/src/main.rs` reads `RUST_LOG` - decide whether log level
      stays env-driven (standard) or moves to config.
- [ ] `scripts/smoke.py` and any shell scripts - audit for `os.environ` /
      `$VAR` runtime knobs (the Rust-side `grep` is clean: only CLI `args()` and
      the compile-time `env!("CARGO_MANIFEST_DIR")` fingerprint-path macro
      remain, neither a runtime env var).
- [ ] `MOGWAI_DATA_DIR` is offline-analysis-only (see gotcha below) - confirm it
      is not read anywhere on the server runtime path before considering it out
      of scope.

## Notes / gotchas

- The offline Kraken corpus is trades only - no quotes, no L2, no aggressor side.
  This shapes the offline analysis only; the running server synthesizes trades
  with a native `Buyer`/`Seller` aggressor and serves no quotes (`/quotes` is
  always empty). `KrakenCsvSource` and `TickRuleAggressor` survive in
  `mogwai-data` for the offline lineage and its unit tests.
- `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`) is an
  offline-analysis input only (`analysis/`), never a server runtime knob.
- `research/` (the nautilus and broadarrow clones) is gitignored; read those APIs
  from there, depend on the sibling `../` checkouts.
