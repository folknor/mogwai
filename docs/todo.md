# TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

Once an item here is completed, it GETS REMOVED ENTIRELY. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new ../reference/ document.

Or both. There are no exceptions.

## Open issues

- Decide whether the fitted tape's multi-minute lulls are desirable realism
  (surfaced 2026-07-16 while fixing the accelerated smoke). The ACD duration
  process (persistence 0.9935, Weibull shape 0.60, dispersion band 131.7 to
  4608.9 s) is heavy-tailed: per-tick mean cadence stays ~7 s, but slow
  excursions last ~150 ticks - hours of tape time at minutes per tick - so a
  subscriber landing at a random sim instant sits mid-lull with high
  probability (the gap straddling an instant is length-biased toward the
  dispersion index, i.e. minutes). Consequence: at high speed a fresh
  subscribe can legitimately stay silent for tens of wall-seconds (deadline
  pacing ignores gap_cap_ms by design), which will trip broadarrow's
  feed-stale watchdog on an honestly healthy venue. The accelerated smoke now
  anchors on a real tape tick so it no longer trips on this; bounding lulls in
  the generator would break the committed byte-identical golden stream, so
  this is a fingerprint refit decision, not a local fix.

- mogwai-engine `next_position` unbounded accumulation. The per-fill weighted-
  average is now overflow-guarded (a single oversized order is rejected before
  it reaches the arithmetic), but `current.qty` still accumulates across many
  individually-valid orders on one symbol/side, so a long-lived engine can
  overflow the `current_abs * avg_px + delta_abs * px` computation over time.
  Closing it means introducing a position-size or notional cap - a design
  decision, not a local fix.

- ProtocolError diagnostics have no per-connection rate limit. Every refused or
  clamped subscribe emits one diagnostic; ba's watchdog-driven resubscribe
  churn turned that into a ~1,700-line storm during a venue restart (QA
  finding, mechanism now removed by the pinned `wall_anchor_ns`). Any future
  repeated-subscribe pathology will storm the same way - a per-connection
  dedup or rate limit on identical diagnostic reasons would cap it.

- Position status reports are still rebuilt from the adapter's account-snapshot
  mirror, unlike the order/fill reports (flipped to the venue-truth
  QueryOrders/QueryFills surface, 2026-07-31). A dropped account snapshot can
  therefore still make position reconciliation confirm a stale position. The
  truthful source already exists (`GET /account`); flipping the generator is a
  small change, deferred until broadarrow asks for it.

- The adapter integration-test stub (`crates/mogwai-adapter/tests/common`) does
  not answer `QueryOrders`/`QueryFills`; the venue-truth report generators are
  covered by unit tests with an in-process fake venue instead. Extend the stub
  when an integration test needs to drive reconciliation end to end.

## Notes / gotchas

- broadarrow standing notes (2026-07-31, their request that landed the
  order-status query surface): (a) the ack-delay havoc band above their ~25 s
  INFLIGHT_TIMEOUT is deliberately unserved - they permanently declined a
  per-venue ceiling on that safety timeout, so do not invest in DelayAcks/
  GoDark scenarios past it (also recorded in reference/havoc.md's operator
  note); (b) the once-floated MarketIfTouched order-type extension is dead
  (the triggering Pine shape is invalid on TradingView and nautilus cannot
  rest an MIT faithfully) - the protocol owes no order-type growth beyond
  Market and Limit.

- broadarrow-side follow-ups from the 2026-07-15 QA findings (their repo, listed
  here so the coordination is not lost): (a) the feed-stale message hard-codes
  the issue-4255 hypothesis ("the connection looks healthy...") as fact even
  when the venue process is dead; (b) `reference/mogwai.md` / `ba man mogwai`
  still describe the venue as unfundable - stale once the `[balances]` seed
  lands; (c) any stored scenario TOMLs arming `GoDark`/`DelayAcks` under an
  HTTP transport profile now fail scenario load by design (create-time
  deliverability refusal) and need a sweep. (The data-path WARN template that
  named three wrong causes turned out to live in mogwai-adapter, not ba - fixed
  here: it now defers to the venue's `reason`, and the WS lifecycle logs
  disconnect/backoff/reconnect/exhaustion per socket.)
- Arming havoc via raw `POST /control/divergence` bypasses the adapter's
  create-time deliverability check by construction: the windows are
  venue-global and the server cannot know which transport each connected
  client rides. External armers (the QA probes do this) remain responsible for
  matching windows to carriers; `reference/havoc.md` says so explicitly.
- The offline Kraken corpus is trades only - no quotes, no L2, no aggressor side.
  This shapes the offline analysis only; the running server synthesizes trades
  with a native `Buyer`/`Seller` aggressor and serves no quotes (`/quotes` is
  always empty). `KrakenCsvSource` and `TickRuleAggressor` survive in
  `mogwai-data` for the offline lineage and its unit tests.
- `MOGWAI_DATA_DIR` (default `/media/folk/Banan/Kraken_Trading_History`) is an
  offline-analysis input only (`analysis/`), never a server runtime knob.
- `research/` (the nautilus and broadarrow clones) is gitignored; read those APIs
  from there. mogwai builds against the pinned crates.io nautilus, not a `../`
  checkout.

## Hardcoded-value and env-var inventory (read-only sweep, 2026-07-01)

Catalogue only, for later evaluation of what deserves to become a knob - nothing
here was changed. Pervasive test-fixture literals (repeated `BTCUSDT`/`BTC`/
`USDT`, golden seed 42, per-assertion timing tolerances) are summarised rather
than enumerated line-by-line; production and config-relevant values are listed in
full.

### Environment variables (whole workspace)

The Rust crates are deliberately env-var-free for runtime knobs; run config lives
in `mogwai.toml`. The only reads:

- `RUST_LOG` - `mogwai-server` via `EnvFilter::try_from_default_env`, falls back
  to `mogwai=info`. The one documented, deliberate ambient exception; a prior
  `MOGWAI_REPLAY_SPEED`/`MOGWAI_GAP_CAP_MS` pair was removed in favour of
  `mogwai.toml`.
- `NO_COLOR` - `mogwai-server/src/man.rs`, standard convention, `man`-output only.
- `MOGWAI_DATA_DIR` - `analysis/characterize.py` and `analysis/recon.py`, default
  `/media/folk/Banan/Kraken_Trading_History`. Offline-analysis input only, never a
  server runtime knob. The default path string is duplicated verbatim in both
  files (`recon.py` re-reads the env var instead of importing
  `characterize.DATA_DIR` the way `run_corpus.py` does).
- Compile-time only (not runtime): `env!("CARGO_MANIFEST_DIR")` in
  `mogwai-data/src/generated.rs` locates the baked-in `analysis/fingerprint.json`;
  the server build script bakes `MOGWAI_LONG_VERSION` from `CARGO_PKG_VERSION`;
  `CARGO_TARGET_TMPDIR`/`CARGO_BIN_EXE_mogwai` in server integration tests.

### Cross-crate couplings worth reconciling

- Default server address `127.0.0.1:8787` is hardcoded independently in three
  places with no shared constant: the server's `--addr` default, the adapter's
  `DEFAULT_BASE_URL = "ws://127.0.0.1:8787"`, and the smoke test's `HOST, PORT`.
  Consistent today, but a port change needs three coordinated edits and nothing
  flags a drift.
- Correctly single-sourced from `mogwai-protocol` (the pattern to follow):
  `DEFAULT_REQUEST_TIMEOUT_SECS` (30) and `MAX_HISTORY_LIMIT` (1000) - the adapter
  references these rather than re-hardcoding them.
- `default_instruments()` BTCUSDT seed lives in `mogwai-protocol` but its seven
  literals are duplicated verbatim in two of that crate's own tests, and the smoke
  test's fixed order shape implicitly depends on it.

### mogwai-protocol (canonical wire defaults)

Named consts, canonical: `DEFAULT_REQUEST_TIMEOUT_SECS = 30`, `MAX_HISTORY_LIMIT
= 1000`, `BASELINE_LATENCY.base_nanos = 30_000_000` (30ms honest-feed latency
floor), `MAX_LATENCY_NANOS = 60_000_000_000` (60s per-field ceiling),
`control::MAX_DIVERGENCE_MS = 3_600_000` (1h DelayAcks/GoDark/StallData ceiling).

Inline literals (no named const):
- `default_instruments()`: symbol `BTCUSDT`, base `BTC`, quote `USDT`,
  `price_precision 2`, `size_precision 8`, `price_increment 0.01`, `size_increment
  1e-8`. Doc comment signposts growth to multi-instrument - prime externalisation
  candidate.
- `ConnHavoc::default()` transport bundle: `reconnect_delay_initial_ms 1_000`,
  `reconnect_delay_max_ms 10_000`, `reconnect_backoff_factor 2.0`, idle/heartbeat/
  jitter 0, `request_timeout_secs 0` (sentinel for the 30s default). Cross-checked
  by the validator, so they move together.
- Validator bounds inline in `validate_*`: VolStorm `vol_mult (0, 100]`,
  LiquidityDrought `thin_factor [1, 1000]`, SessionEdgeSpike hour clamp and
  `extra_vol_mult [0, 100]`, ReopenGap `halt_secs > 86_400` (the one temporal
  bound NOT backed by a named const, unlike its sibling `MAX_DIVERGENCE_MS`),
  PartialFillNext `fraction (0, 1]`.

### mogwai-engine

- `commission: Decimal::ZERO` booked on every fill unconditionally - no fee policy
  or divergence path exists (notable for a crate whose stated purpose is injecting
  realistic execution divergences).
- Venue/trade id prefixes `V`/`T` as inline magic strings.
- Test fixtures repeat `BTCUSDT`/`BTC`/`USDT`, a base price of 100, and
  partial-fill fractions 0.3/0.4/0.5 across dozens of sites (no shared consts).

### mogwai-server

- Bind: `--addr` default `127.0.0.1:8787` (see coupling above); tests bind
  `127.0.0.1:0`.
- Filenames: `mogwai.log`; `mogwai.pid` (default duplicated on both `serve` and
  `stop` args); `mogwai.toml` (fallback duplicated in `Config::load` and
  `resolve_paths`).
- HTTP route strings (`/health`, `/account`, `/instruments`, `/trades`,
  `/quotes`, `/clock`, `/orders`, `/ws`, `/control/divergence`) as inline
  literals, no shared registry with the adapter's route segments.
- `Config::default()`: `speed 1.0`, `gap_cap_ms 1000`, `server_heartbeat_ms 0`,
  `backfill_horizon_ns 86_400_000_000_000` (24h), `sim_epoch_ns 0`.
- Lifecycle timeout consts: `READY_TIMEOUT 10s`, `SHUTDOWN_GRACE 2s`, `STOP_TIMEOUT
  5s`, `STOP_KILL_GRACE 2s` (same value as SHUTDOWN_GRACE but a distinct phase),
  `PID_POLL_INTERVAL 25ms`, `REPLAY_SEND_POLL 5ms`.
- Channel capacity `1024` duplicated inline for the writer channel and the
  exec-delay pump channel (different traffic classes, no shared const).
- Synthesis limits: `MAX_HISTORY_SEEK_TICKS 190_000`, `CHECKPOINT_K 8192`. The
  test-side `HORIZON_S 86_400.0` stands in for the production `backfill_horizon_ns`
  default as a plain literal and can silently drift from it.

### mogwai-adapter

- `DEFAULT_BASE_URL = "ws://127.0.0.1:8787"` (see coupling above).
- `MOGWAI_VENUE_STR = "MOGWAI"` (correctly single-sourced).
- Default identity `TraderId`/`AccountId` `MOGWAI-001` in the exec config.
- Timeout consts: HTTP `POLL_INTERVAL 250ms`, `ACCOUNT_REGISTRATION_TIMEOUT 5s`,
  `ACCOUNT_REGISTRATION_POLL 10ms`, `MIN_WALL_REQUEST_TIMEOUT_SECS 1` (flagged in
  its own comment as the tightest cap on usable sim speed). `wait_connected`
  re-hardcodes an independent 5s/10ms pair matching the registration consts by
  value but not sharing them.
- `1_000_000_000` (nanos-per-second) repeated inline 5+ times across `client.rs`
  and `lifecycle.rs` - a `NANOS_PER_SEC` const would remove the repetition.
- Triplicated test `def()` instrument fixture (`price_precision 2`/`size_precision
  8`) across three test modules.

### mogwai-data (generator)

Fingerprint/distribution constants are named module consts, fitted-and-committed
by design (changing them re-shapes the synthetic market): ACD 0.9935 / 0.08 /
Weibull shape 0.60, GARCH 0.06 / 0.935, Student-t df 4.0, bounce and drift
transition probabilities, `SIZE_LOG_SIGMA 1.15`, `MAX_ABS_RETURN 2e-5`,
`GARCH_SIGMA_CAP 1e-6`, anchor `START_PRICE_USD 60_000`, `VOL_SCALAR 5e-8`, and the
precomputed `WEIBULL_MEAN_SHAPE_060` gamma normaliser. The real fingerprint numbers
live in `analysis/fingerprint.json` (embedded via `include_str!`), not in Rust.

Inline (not named): `xbtusd_anchor` fields `XBTUSD` / `modal_tick 0.1` /
`price_decimals 1` (deliberately per-pair, kept in the constructor); the `1e9`
mid-price runaway ceiling duplicated at two sites; `round_lot_size` thresholds
(1.0 / 10.0 / 0.1). `seed`, checkpoint `k`, and `max_extend` have no production
default here (caller-supplied by the server); seed `42` is the pervasive
golden-test seed.

### Non-crate (scripts, analysis, root config)

- `scripts/smoke.py`: `HOST/PORT 127.0.0.1:8787` (no `--host`/`--port` override),
  `WINDOW_LOOKBACK_NS 1h`, `ACCEL_DELAY_MS 1000`, `ACCEL_CLOCK_SLACK_WALL_NS
  50ms`, `ACCEL_ANCHOR_TIMEOUT_S 120`, fixed order shape
  (`BTCUSDT`/`Limit`/qty 10/px 100), plus many inline per-assertion socket
  timeouts and latency tolerances (not centralised; first place to look if
  the smoke ever gets flaky).
- Orchestration: the `review` tool, configured from `.review.toml` - the codex
  wrapper scripts were removed in favour of it. Critique runs `review bare
  --profile deep` (gpt-5.6-sol, xhigh, read-only); implement runs `review goal
  --profile build` (gpt-5.6-terra, medium, workspace-write). `[_defaults]`
  pins the provider to `codex`. `prevent-harness-bug.sh` default sleep `60`.
- Smoke fixture configs `smoke-accelerated.toml` (`speed 100.0`) and
  `smoke-heartbeat.toml` (`server_heartbeat_ms 100`) - by-design knobs.
- `analysis/`: `MAX_LAG 50` in `characterize.py` with `build_fingerprint.py`
  hardcoding ACF indices `[9]`/`[49]` as lag10/lag50 (hidden coupling - changing
  MAX_LAG silently breaks the indices); `TICK_DICT_CAP 500_000`, histogram bin
  counts, `run_corpus.DEFAULT_PAIRS` (8-pair subset) with the worker pool capped at
  6, `recon.TAIL_BYTES 8192`, `ANCHOR "XBTUSD"`, and a day-of-week convention
  re-derived in three files instead of shared.
- Root `Cargo.toml`: workspace dep version pins (serde 1, tokio 1, axum 0.8,
  rust_decimal 1 with serde-with-str, rand 0.10, rand_distr 0.6, rand_chacha 0.10,
  and the rest) centralised as workspace deps; `[profile.release]` opt-level 3 /
  lto fat / codegen-units 1; `rust-version 1.96`, `resolver 3`. The nautilus
  crates.io dep (pinned) lives in `mogwai-adapter/Cargo.toml`, not root. `brokkr.toml` only sets
  `project = "mogwai"`. Root `mogwai.toml` carries the run knobs (`sim_epoch_ns 0`,
  `wall_anchor_ns 0`, `speed 1.0`, `gap_cap_ms 1000`, `server_heartbeat_ms 0`,
  `max_concurrent_replays 1024`, and the funded `balances` table).
