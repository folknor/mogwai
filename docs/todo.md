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

### 4. Bug-hunt follow-ups - remaining open items

A project-wide bug/duplication hunt ran as six fix waves (see git log: the
findings catalogue itself was a transient doc, now deleted). The clearly-correct
bugs and safe consolidations landed; what remains is recorded here in full so
nothing is lost. Tags like `B.3` / `E.5` / `dup #12` are the original finding
ids, kept only as a cross-reference to the commit messages.

**Open design decisions (deliberately not changed - they alter intended havoc /
account semantics, so they are yours to call):**

- [ ] **Market-order fill price (B.3).** A market order fills at price zero,
      crediting base for free and booking zero quote notional
      (`mogwai-engine`, the fill path). Decide: synthesize a mid from the
      generator, or reject market orders outright.
- [ ] **`GoDark` semantics (E.1 / E.2 / E.15).** Process-global (one arm blacks
      out every connected client), it *drops* frames rather than holding them
      (execution events are lost, not delayed), and there is no clear/reset path
      (an absurd `ms` bricks all output until restart). `mogwai-server`
      `arm_divergence` / the writer dark-window check. Decide per-session
      scoping, hold-and-replay vs drop, and a clear control.
- [ ] **`AccountState` exec-vs-data classification (E.8 / A.14 / dup #3).** The
      server treats `AccountState` as an execution event (delayed by `DelayAcks`)
      while the adapter buckets it as data latency - opposite views of one enum.
      A shared `ServerMessage::category()` in `mogwai-protocol` consumed by both
      ends would consolidate it, but forces deciding which view is correct.
- [ ] **`/quotes` stub (E.12 / A.10 / G.bug9).** The server `/quotes` route and
      the adapter `request_quotes` both return empty unconditionally. Leave as the
      documented trades-only stub, or signal "unsupported" instead of empty-OK.
- [ ] **Surfacing rejects for unknown orders (A.11).** `OrderRejected` /
      `OrderModifyRejected` for an order the adapter mirror does not know are now
      logged but still dropped, because nautilus validates the event's
      `strategy_id`/`instrument_id` against its own cache and silently drops a
      mismatch - so synthesized identifiers cannot surface the reject. Correctly
      surfacing it needs a non-mirror identifier source (the nautilus cache, which
      `ExecContext` does not hold). Decide how to source the real identifiers.
- [ ] **`AccountBalance::new` panic.** It `assert!`s `total == locked + free`,
      which a hostile wire snapshot can violate independently of the (fixed) money
      conversion (`mogwai-adapter` `handle_account_state`). Fixing it is a
      behavior call: drop the balance, or clamp.
- [ ] **`RejectNextSubmit` targeting (B.8).** It rejects whatever order is
      submitted next, while `PartialFillNext` is targeted by `client_order_id`.
      Giving it a `client_order_id` is a protocol + engine change and a
      targeting-semantics decision.
- [ ] **Configurable `OmsType` (D.7).** The exec factory hard-codes
      `OmsType::Netting`. Making it a config knob changes position accounting and
      may need to match what broadarrow expects for the MOGWAI venue.
- [ ] **Lognormal size: median vs mean (C.2).** `typical_size` is the lognormal
      *median*, not the mean (`mean ~= 0.194` for `typical_size = 0.1`). Changing
      `mu` to make the mean match would shift the size distribution and break the
      byte-exact golden test, so it is a deliberate calibration call.
- [ ] **Reopen-gap halt origin (C.6).** `take_reopen_crossed` measures the halt
      from the crossing trade, not from `at_ts`, so the realized halt can be up to
      one inter-arrival longer than `halt_secs`. Semantic - decide if it matters.

**Deferred engineering (clear-cut, just larger than a one-wave fix):**

- [ ] **Seeded-RNG determinism test (F.6).** All probabilistic havoc tests use
      `prob` 1.0/0.0, so the seed is never load-bearing. Add a `draw`-level test
      at an intermediate probability with a pinned seed.
- [ ] **Generic havoc dispatch/flush (dup #12).** Four near-identical
      `dispatch_*`/`flush_*` wrappers in `mogwai-adapter` `client.rs` (market vs
      exec) could collapse into one generic pair (~50 lines).
- [ ] **Lanczos `gamma` -> literal constant (C.8, partial).** The dead reflection
      branch is gone and the value is pinned by a tolerance test; replacing the
      series with a hard-coded constant needs a one-time `clean_regime_is_byte_-`
      `identical` run to confirm bit-equality.
- [ ] **Fallible `http_base_url` (D.13).** Currently infallible with an
      http-prefix fallback; a stricter `Result` contract would ripple through ~13
      `client.rs` call sites. Low priority now that `validate` rejects non-ws
      `base_url` up front.

**Wontfix / accepted as-is (recorded so they are not re-flagged):**

- `seq: u64` overflow is unguarded but unreachable in practice (B.11).
- Modify rejects `new_total == filled` exactly (B.12) - the reject is defensible.
- `HavocSpec.data` vs `conn` `skip_serializing_if` asymmetry is intentional
  (B.13, per its comment).
- `wait_connected` is racy against a fast socket flap (A.17) - minor.
- `convert::instrument_id` takes `&InstrumentDef` but reads only `symbol` (D.10) -
  cosmetic. `TradeId` from `symbol-ts` is not collision-free (D.11) - would need a
  real wire id.
- The reorder filter holds a message until the next one or stream-close (A.5),
  and reorder is a no-op over the HTTP-orders transport (A.13) - both are part of
  the current havoc model; revisit only if the model changes.
- The adapter test suite is `#[ignore]`d so CI only compiles it (F.20) - a
  deliberate socket-sandbox tradeoff.

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
