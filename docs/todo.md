# mogwai - TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

mogwai is a fake broker/exchange that plugs into broadarrow to exercise the
*live* trading path: it synthesizes market data from a committed fingerprint
fitted offline to Kraken trade history (the running server opens no CSV) and
injects the messy execution divergences a backtest sandbox cannot produce. Four
nautilus-free broker crates plus the `mogwai-adapter` venue adapter.

## Direction (scope - read before triaging anything as a "bug")

mogwai is a disposable test fixture, NOT a production exchange. It does not need
economically-correct accounting or anti-adversarial hardening: its inputs are
author-controlled (the committed fingerprint plus bounded, validated havoc
knobs), and the messy behavior is the *feature*. Two standing principles:

- **Honest by default.** By default mogwai serves a realistic, honest live feed
  with normal simulated network latency. Every havoc surface is opt-in and driven
  from broadarrow - the per-venue `HavocSpec` (client / data / conn) plus the
  out-of-band `/control/divergence` plane. Nothing perturbs the stream unless
  broadarrow asks.
- **Lean toward more havoc.** The desirable direction is richer, more realistic
  venue pathologies - not making the fake venue more correct. The lodestar is a
  real venue misbehaving in a way the client cannot detect: e.g. nautilus_trader
  issue 4255, where Kraken acked an `ohlc` subscription and then silently
  delivered nothing while the socket stayed healthy. Reproducing that class of
  failure is in-scope and welcome; hardening mogwai toward a correct ledger is
  not.

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
the summary). A project-wide bug/duplication hunt ran as six fix waves (see git
log); the clearly-correct bugs and safe consolidations all landed.

## Next

### 1. broadarrow integration: point a MOGWAI venue at the server

The adapter, its `transport_profile` selector, and its full four-surface
`HavocSpec` (client / server / data / connection-lifecycle) are landed (see
`reference/architecture.md`). What remains lives in broadarrow, not this repo:

- [ ] broadarrow side (lives in broadarrow, not here): a `MOGWAI` arm in
      `run-prep/src/venue.rs` and a `core::venue` PROFILES row, with a
      profile-guard test enforcing that every wired venue has a PROFILES row.
      This is where broadarrow sets the per-venue `HavocSpec` (including its
      `data` market-regime and `conn` connection-lifecycle fields), the
      `oms_type` (the adapter exposes it as a `MogwaiExecClientConfig` field
      defaulting to `Netting`), and picks the `transport_profile`.

### 2. Coherent simulated clock for accelerated forward testing (NEEDS A SPEC)

The headline capability a synthetic venue can offer that a real exchange cannot:
run the *actual live trading path* (real async, real reconnects, real WS frames)
while compressing time - e.g. a simulated day of OHLCV per wall-clock second. A
real exchange forces 1x; mogwai's clock is fake, so it has no lower bound. This
is a run-mode, NOT havoc.

The cheap version - data-only acceleration via the existing server `speed`
multiplier - is REJECTED: it leaves execution-event and account-state timestamps
on wall-clock while data races ahead on simulated time, and broadarrow's node
still runs on nautilus `LiveClock` (wall time), so a strategy's timers,
bar-aggregation windows and time-in-force expiries tick at real time while the
feed is a simulated-day ahead. Silently wrong, not loudly broken.

The real thing is a COHERENT simulated clock: mogwai stamps *everything* -
market-data `ts_event`, fills, account state - on one simulated time axis that
advances at the acceleration factor, AND broadarrow's node advances on that same
clock. This spans:

- `mogwai-protocol` / `-engine` / `-server` / `-data`: today the server stamps
  execution events with wall-clock `now_ns()` (now `mogwai_protocol::`
  `now_unix_nanos`) while market data carries the generator's simulated
  `ts_event` - those two time bases must unify onto one accelerated axis.
- `mogwai-adapter`: its time source (currently the shared wall-clock reader).
- broadarrow's node clock seam.

**Feasibility crux - RESOLVED, and the answer is DEFERRED to nautilus.** A
nautilus `LiveNode` cannot today be driven by a non-wall `Clock` without an
upstream change. Three independent walls in the in-tree `research/nautilus_trader`
copy:

- The kernel hardwires the clock by environment with no injection seam:
  `crates/system/src/kernel.rs` `initialize_clock` is a bare match - `Backtest`
  gets a `TestClock`, `Live | Sandbox` get `LiveClock::default()`. Neither the
  config nor the builder accepts a caller-supplied `Clock` (the builder only
  hands the kernel's own clock to factories, read-only).
- `LiveClock`'s time-read axis is the process-global realtime singleton:
  `crates/common/src/live/clock.rs` takes `get_atomic_clock_realtime()` and
  `timestamp_ns()` just reads it.
- The timer-fire axis is independently wall-bound: `LiveClock` timers are Tokio
  `LiveTimer`s that sleep a real duration, so time-in-force expiries and
  bar-aggregation windows tick at 1x even if the time-read axis were scaled.

So coherent acceleration is NOT a mogwai+adapter-only change - it needs a
nautilus seam (a scalable "accelerated live" `Clock` whose time source AND timers
scale, plus a kernel/builder injection point or a new `Environment` variant),
after which broadarrow constructs its node in that mode. The user is filing the
request upstream with nautilus. This item is parked until that lands; no mogwai
spec or code until the nautilus seam exists.

- [ ] Upstream: nautilus accepts a non-wall `Clock` for a live node (user-filed).
- [ ] Then write the spec per `reference/technical-implementation-spec.md`
      before any mogwai/adapter code (this item is the TODO source it cites).

### 3. Honest default: a realistically-paced live feed

Per the Direction note the honest default should *be* a realistic live feed; it
currently is not (a zero-latency firehose). This is NOT a new non-havoc surface -
`speed` and `ClientHavoc.latency` already ARE the knobs; the work is choosing
honest *default values* for them, on top of which the havoc layers extra:

- [ ] Server pacing defaults to `speed = 0.0` (unthrottled - dumps ticks as fast
      as the client drains). Flip the honest default to wall-clock pacing, i.e.
      respect the generator's inter-arrival gaps in real time (the `speed = 1.0`
      baseline). Until the coherent clock (item 2) lands this is a `mogwai.toml`
      default change; afterwards it is the 1x point of the acceleration axis.
- [ ] `ClientHavoc.latency` defaults to `null` (zero injected delay). Give it a
      modest non-null default so the honest baseline carries realistic network
      latency; arming additional havoc latency then layers on top of that
      default. Same knob, honest default value - not a separate surface.

### 4. Havoc expansion: acked-but-silent data stall (4255-shaped)

Direction: more havoc. `GoDark` is all-or-nothing and time-bounded - it stops
*everything*, so a frame-level idle timeout catches it (the easy-to-detect
blackout). It cannot express the nastier, realistic nautilus_trader issue 4255
variant: subscription acked, socket healthy, liveness signal still flowing, only
the channel data withheld - undetectable by a frame-level idle timeout, which is
exactly why that issue argues for a per-subscription data watchdog.

- [ ] New `control::Divergence` (e.g. `StallData` / `GoQuiet { ms }`) that
      suppresses only market-data (`Trade` / `Quote`) frames while leaving
      execution frames alive. Cleanly additive: a 7th `Divergence` variant plus a
      writer rule, and it needs `validate_divergence` coverage.
- [ ] Optional server-originated heartbeat so the connection has a liveness
      signal that survives the stall - the full 4255 reproduction. mogwai's
      server has none today (the `heartbeat_interval_ms` knob is the *client*
      pinging). With a server heartbeat flowing through a `StallData` window, the
      stall is invisible to any frame-level idle timeout and only a data watchdog
      can catch it.

### 5. Author `reference/havoc.md`

Durable reference doc for the divergence/havoc surfaces. Covers the four-surface
`HavocSpec` (client / server / data / connection-lifecycle), every `Divergence`
variant and its trigger/semantics, the `MarketRegime` axis, how the server arms
and applies them vs what the engine owns, the validation boundaries
(`validate_divergence`, `validate_market_regime`, `validate_conn_havoc`), and -
per the Direction note - the honest default vs the opt-in havoc surfaces. Read
`reference/technical-implementation-spec.md` first for what such a doc must
contain. Best written after items 3 and 4 settle the default and the stall
divergence, so the doc describes the real surface.

### 6. Remove remaining runtime env vars - knobs belong in TOML / `HavocSpec`

Run knobs should be explicit input (TOML or havoc knobs), not ambient
environment. The replay-speed / gap-cap env vars were already moved to
`mogwai.toml` (commit ca7bc66). Remaining audit:

- `RUST_LOG` (server only, `mogwai-server/src/main.rs`
  `EnvFilter::try_from_default_env`) stays env-driven - DECIDED. It is the one
  standard, universally-expected logging env var; the adapter reads no `RUST_LOG`
  at all. This is the deliberate exception to the TOML-knobs rule, not an open
  item.
- [ ] `scripts/smoke.py` and any shell scripts - audit for `os.environ` /
      `$VAR` runtime knobs (the Rust-side `grep` is clean: only CLI `args()` and
      the compile-time `env!("CARGO_MANIFEST_DIR")` fingerprint-path macro
      remain, neither a runtime env var).
- [ ] `MOGWAI_DATA_DIR` is offline-analysis-only (see gotcha below) - confirm it
      is not read anywhere on the server runtime path before considering it out
      of scope.

### 7. Bug-hunt follow-ups - the residual tail

The fix waves are done; the scope conversation above resolved most of what was
parked here. Tags (`B.8`, `C.2`, etc.) cross-reference the fix-wave commit
messages. What genuinely remains open:

Deferred engineering (clear-cut, just larger than a one-wave fix):

- [ ] **Seeded-RNG determinism test (F.6).** All probabilistic havoc tests use
      `prob` 1.0/0.0, so the seed is never load-bearing. Add a `draw`-level test
      at an intermediate probability with a pinned seed.
- [ ] **Generic havoc dispatch/flush (dup #12).** Four near-identical
      `dispatch_*`/`flush_*` wrappers in `mogwai-adapter` `client.rs` (market vs
      exec) could collapse into one generic pair (~50 lines).
- [ ] **`GoDark` clear/reset control + bound `ms`.** The drop-not-hold and
      process-global semantics are correct and endorsed (they are faithful to a
      real blackout), but an absurd `ms` saturates `dark_until_ns` and bricks all
      output until restart, and there is no way to clear an armed delay/dark
      window. Add a clear/reset path and bound `ms`.
- [ ] **Lanczos `gamma` -> literal constant (C.8, partial).** Dead reflection
      branch is gone and the value is pinned by a tolerance test; replacing the
      series with a hard-coded constant needs a one-time
      `clean_regime_is_byte_identical` run to confirm bit-equality.
- [ ] **Fallible `http_base_url` (D.13).** Currently infallible with an
      http-prefix fallback; a stricter `Result` contract would ripple through ~13
      `client.rs` call sites. Low priority now that `validate` rejects non-ws
      `base_url` up front.

Resolved by the scope conversation - closed, NOT to be re-flagged as bugs:

- **Lognormal size: median vs mean (C.2)** - closed: correct as-is. `typical_size`
  is not fit from the corpus at all - it is a hand-set constant
  (`TYPICAL_SIZE_MANTISSA` / `TYPICAL_SIZE_SCALE` in `mogwai-data` `generated.rs`),
  like `start_price` and `vol_scalar`. Only `modal_tick`, `price_decimals`,
  `mean_duration_s` and `size_round_frac` come from fingerprint medians; the
  corpus `size_log10_hist` is emitted to the `char_*.json` files but never folded
  into the fingerprint. So there is no mean-fit to be ~2x off from: using the
  author-chosen typical size as the lognormal median is the natural reading and
  is correct.
- **Market-order fill price (B.3)** - fills at zero, "free base". Closed: the
  fake venue's balances are not consumed as truth, so the accounting need not be
  economically correct.
- **`GoDark` drops frames and is process-global (E.1 / E.2)** - endorsed as
  faithful to a real venue blackout (cf. issue 4255). Only the clear/reset + `ms`
  bound survives, listed under deferred above.
- **`AccountBalance::new` assert** - closed: no hostile input exists (mogwai
  emits its own snapshots); if it ever fires it is a mogwai-engine consistency
  bug to fix there, not an adapter-hardening surface.
- **`/quotes` empty-OK stub (E.12 / A.10)** - closed: the corpus is trades-only
  and it is documented; leave it.
- **Surfacing rejects for unknown orders (A.11)** - closed for now: the mirror
  records on submit so the normal path works; the unknown-order case is an edge
  the warn-log covers. Revisit only if it bites (it needs a non-mirror identifier
  source, which `ExecContext` does not hold).
- **`RejectNextSubmit` untargeted (B.8)** - closed unless broadarrow needs
  targeting: "reject the next order" is fine for test scenarios.
- **Reopen-gap halt origin (C.6)** - closed: the imprecision is bounded by one
  inter-arrival gap; not worth the complexity.

Wontfix / accepted as-is (recorded so they are not re-flagged):

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
