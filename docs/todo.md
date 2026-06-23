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

### 4. Bug-hunt follow-ups - open decisions and deferred work

The project-wide bug/duplication hunt landed four fix waves (see git log and
`docs/bug-hunt-findings.md` for the full catalogue with line refs). What remains
is recorded here so it survives that transient doc.

Open design decisions (deliberately not changed - they alter intended havoc
semantics, so they are yours to call):

- [ ] **Market-order fill price (B.3).** A market order currently fills at price
      zero, crediting base for free and booking zero quote notional. Decide:
      synthesize a mid from the generator, or reject market orders outright.
- [ ] **`GoDark` semantics (E.1 / E.2 / E.15).** Today it is process-global (one
      arm blacks out every connected client), it *drops* frames rather than
      holding them (execution events are lost, not delayed), and there is no
      clear/reset path (an absurd `ms` bricks all output until restart). Decide
      per-session scoping, hold-and-replay vs drop, and a clear control.
- [ ] **`ServerMessage::category()` consolidation (Wave-3 item 3).** The server
      classifies `AccountState` as an execution event (delayed by `DelayAcks`)
      while the adapter buckets it as data latency - opposite views of one enum.
      Consolidating into a shared classifier forces deciding which is correct.
- [ ] **`/quotes` stub (E.12 / G.bug9).** Always returns `200 []`. Leave as the
      documented trades-only stub, or signal "unsupported" instead of empty-OK.

Deferred engineering (clear-cut, just larger than a one-wave fix):

- [ ] **`Price::new` / `Quantity::new` hardening (D.1 / D.2).** The `to_f64`
      panic was closed via the saturating `decimal_to_f64`, but the nautilus
      constructors still `assert!` on out-of-range / negative / over-precise
      values. Move to `new_checked` and drop/log the offending tick; this ripples
      through the `convert` signatures, so it wants its own pass.
- [ ] **Seeded-RNG determinism test (F.6).** All probabilistic havoc tests use
      `prob` 1.0/0.0, so the seed is never load-bearing. Add a `draw`-level test
      at an intermediate probability with a pinned seed.
- [ ] **Remaining smells/nits.** The findings doc still lists assorted
      `smell`/`nit` items across all five crates that were not worth a dedicated
      wave (e.g. the additive-vs-multiplicative vol composition, the reorder
      no-op-over-HTTP asymmetry, `is_execution_event` defined by exclusion).

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
