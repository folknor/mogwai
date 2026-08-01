# TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

Once an item here is completed, it GETS REMOVED ENTIRELY. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new ../reference/ document.

Or both. There are no exceptions.

## Open issues

- DECIDE: are the fitted tape's arrival droughts desirable realism? The oldest
  open question here, and now the highest-severity one. Surfaced 2026-07-16
  while fixing the accelerated smoke, re-measured twice since; this entry
  absorbs the former sweep item D16, which tracked the same
  decision separately.

  MECHANISM. The ACD duration process (persistence 0.9935, Weibull shape 0.60,
  dispersion band 131.7 to 4608.9 s) is heavy-tailed. Psi decays per TICK, so a
  high-psi state self-prolongs in WALL time - at 30 min/tick a hundred ticks of
  decay is days of tape - and the per-tick fit never constrained wall-clock
  dwell. Per-tick mean cadence stays ~7 s throughout, which is exactly why a
  per-tick view of the fit is blind to this: the deserts hide in the wall-clock
  projection, not in the duration distribution.

  MEASURED, two independent ways. Counted per simulated hour off `/trades` with
  `scripts/probe_warmup_window.py`, the default 24 h-horizon tape opens at ~660
  trades/hour then sits at 3-16 trades/hour for eighteen straight hours; over a
  7-day horizon the pattern repeats with droughts of 15+ simulated hours.
  Independently, `mogwai gen --type bars --length 4d --interval 1h` on the
  default BTCUSDT profile at anchor 0 shows hours 0-5 dense (806, 641, 501, 749,
  864, 738 trades/hour), hours 6-25 collapsing to 2-85 with a single hour holding
  2 trades, a milder patch at hours 30-36, then dense for the remaining ~60. No
  hour is completely empty, so this is near-silence rather than a true gap - but
  an hour holding 2 trades means inter-trade gaps in the tens of minutes, which
  no real BTCUSD tape does. Because `data_origin` and the seed are fixed by the
  boot config, a pinned `sim_epoch_ns` lands EVERY boot of that config on the
  same stretch, so it reproduces per-config rather than intermittently.

  WHY IT IS NOT SEPARABLE. The three ACD constants are hand-tuned in
  `generated/consts.rs` to land two committed fingerprint duration bands, and
  `realism()` measures BOTH on the REALIZED tick-to-tick gaps - `measure` pushes
  `trade.ts_event` into `timestamps` and derives the dispersion index and the
  duration ACF from consecutive differences of that vector. So the gate reads
  exactly the wall-clock gaps any cap would shorten; it never sees the
  generator's internal `duration_s`. The deserts are not an artifact bolted onto
  the fit, they ARE the realized mechanism that achieves the committed duration
  realism. Sharper still: the DISPERSION band only needs big gaps to EXIST, but
  the duration-ACF band demands they CLUMP, and clumped big gaps are the
  deserts. If the ACF target is faithful to Kraken, the deserts are faithful in
  KIND and wrong only in wall-clock SCALE, because neither committed target
  constrains dwell. That wall-clock-blindness is the actual root gap.

  WHY IT IS NOW PIPELINE-BLOCKING, not a fidelity blemish. The venue is the
  forward-validation gate, and the gate exists to validate what a backtest
  cannot: resting-order exit behavior. A strategy whose validation window lands
  in a desert gets no resting-order fills, so the very property being measured is
  never exercised - and it fails SILENTLY, the run completing and reporting
  nothing wrong. At batch scale that is a page of null results indistinguishable
  from strategies that genuinely never triggered. Acceleration shortens the
  wall-clock cost of a desert but not its effect. Consequences already seen: a
  fresh subscribe can stay silent for tens of wall-seconds at high speed
  (deadline pacing ignores `gap_cap_ms` by design), tripping a consumer's
  feed-stale watchdog on an honestly healthy venue; a short historical window
  inside a drought returns zero trades, so a nautilus bar warmup gets an empty
  batch and times out fatally; and density-sensitive tests are inherently flaky.
  Mitigations already landed: the accelerated smoke anchors on a real tape tick,
  the adapter names tape sparsity when an on-tape warmup window yields no bars,
  `trades_window_is_clamped_at_sim_now` was widened to 6 h, and the mechanism is
  written up in `reference/architecture.md` ("Tape arrival droughts").

  THE DECISION, four options with honest costs. (A) Accept and document: zero
  code risk, the unrealistic 18 h magnitude stays. (B) Cap the realized
  inter-tick gap while feeding the ACD the uncapped duration: looks surgical, is
  a TRAP - the gate measures realized gaps, so capping lowers the very statistics
  it validates. (C) Re-derive the duration model against a wall-clock-aware
  target: the only real fix, a genuine offline-analysis project touching
  `analysis/build_fingerprint.py`, the committed fingerprint targets,
  `consts.rs`, the golden stream, and every coupled realism anchor, and it partly
  relitigates whether the committed duration bands are the right targets. (D)
  Split the concern: gate `realism()` on the pure fitted dynamics and apply a
  wall-clock dwell governor only on the SERVING path - cheap, keeps
  byte-identity, but then the validated tape is no longer the served tape, which
  is where the byte-identical discipline and the usable-venue goal collide.

  Current lean: the purpose framing pushes off (A), since the fingerprint
  duration bands are a CREDIBILITY PROXY for a believable backdrop rather than
  the product, and a band that produces an unusable venue is a proxy that
  overshot. (B) is a trap. The live question is (C) vs (D) - whether the tape
  that ships must stay the tape `realism()` validates. Downstream of this
  decision: the AD12 dead-feed watchdog below (a threshold distinguishing "asleep"
  from "dead" depends on it), and closing a live in-progress bar window on a
  clock timer, deliberately not built for the same reason.

- Move the adapter off the `../nautilus_trader` path dependency onto a pinned
  crates.io release. `crates/mogwai-adapter/Cargo.toml` path-depends five
  nautilus crates from the sibling checkout, which is deliberate: the published
  release still carries bugs this project hits. Those are being fixed upstream
  (60+ PRs merged as of 2026-08-01, roughly 15-20 more queued), and once they
  land in a release the manifest pins that version instead. Until then the
  build is not reproducible - a path dep has no version requirement and no
  checksum, so `Cargo.lock` records the crates with no `source` and cannot pin
  them, and whatever sits in the sibling checkout at build time is what
  compiles. A fresh clone also cannot build `mogwai-adapter` without that
  checkout present. Blocked on upstream, not on a decision here; `AGENTS.md`
  describes the current path-dep arrangement rather than the intended one.

- mogwai-engine `next_position` unbounded accumulation. The per-fill weighted-
  average is now overflow-guarded (a single oversized order is rejected before
  it reaches the arithmetic), but `current.qty` still accumulates across many
  individually-valid orders on one symbol/side, so a long-lived engine can
  overflow the `current_abs * avg_px + delta_abs * px` computation over time.
  Closing it means introducing a position-size or notional cap - a design
  decision, not a local fix.

- BUILD: an end-to-end guard that startup reconciliation actually runs
  (`crates/mogwai-adapter/src/client/exec.rs`, formerly sweep item F16). The
  nautilus Rust `ExecutionClient` trait defaults are silent no-ops -
  `generate_mass_status` returns `Ok(None)`, the three granular generators
  return empty vecs - and the live node's `Ok(None)` arm only warns "likely
  adapter error" and reconciles nothing. Python's `LiveExecutionClient` base
  composes the three for you, so a Python adapter cannot fall into this; the
  Rust trait does not, which is why mogwai hit it. mogwai overrides the method
  (unit-tested by `generate_mass_status_composes_the_three_report_sets`), so the
  instance is fixed - but nothing proves a seeded venue produces a NON-EMPTY
  mass status through the real transport, so a refactor that drops the override,
  breaks the compose, or leaves one generator returning empty reverts startup
  reconciliation to zero with only a warn line and no test going red.

  DECIDED, shape agreed: extend the self-contained stub in
  `crates/mogwai-adapter/tests/common` to answer `QueryOrders`/`QueryFills` over
  both carriers, then assert all THREE report sets non-empty - and assert each
  generator INDIVIDUALLY, not just the composite, which is what turns the
  class-of-risk observation below into a gate. Rejected as disproportionate:
  booting a real `mogwai-server` from the adapter crate, or a full nautilus
  `LiveNode`. The only thing a live node catches beyond this is "nautilus stopped
  calling mass status at startup", which is upstream behavior with its own tests.
  Known limitation to keep recorded: the guard proves the adapter WOULD answer
  when asked, not that the node asks.

  The exposure is a CLASS, not one method: every report path mogwai relies on
  shares the silent-degrade property, and this got MORE important since filing,
  because those paths are now the venue-truth reconciliation surface rather than
  a mirror echo. Related upstream, queued in the maintainer's PR tracker and NOT
  a substitute for this guard (mogwai overrides the method, so a better trait
  default protects the next adapter author, not this repo): give the Rust trait
  default the same composing behavior as the Python base.

- BUILD: a positive dead-feed watchdog (formerly sweep item AD12). No liveness
  timer, tick counter, or "0 ticks in N s" log exists on either transport. The
  negative diagnostics are all in place - the server emits a `ProtocolError` on
  an unservable subscribe, the adapter's data drain warns rather than swallowing
  it, and the poll loop self-heals after a server restart - but nothing
  positively proves a subscribed feed is alive rather than genuinely quiet. The
  WS idle timeout does not cover it: `idle_timeout_ms` defaults to 0, and even
  armed the idle clock resets on ANY application frame, so a
  data-silent-but-frame-active socket never trips it, deliberately, because that
  is what reproduces the 4255 case. Blocked in practice on the drought decision
  above: on a desert tape a silent feed is often CORRECT, so the threshold that
  separates "the venue is asleep" from "the subscription is dead" depends on
  which option lands.

- DECIDE: does dup/drop havoc reshaping fabricated bars model the right venue?
  (Formerly sweep item AD21a.) Bars are built AFTER the `HavocFilter` on both the
  WS and poll paths, so a dup or drop of one trade silently reshapes OHLCV rather
  than duplicating or dropping a whole bar frame. Bars here are FABRICATED by the
  adapter - the server never ships one - so deriving them from a corrupted trade
  feed is what a real client-side aggregator on a lossy feed experiences, and is
  arguably the honest simulation; the alternative models a venue that ships bars
  natively, which mogwai is not. Leaning accept-and-document, on the same
  principle that settled the reconnect account staleness: mogwai injects faults
  and declines to repair them downstream. (The reorder half of the original item
  was a different finding and is closed - `fold_trade` now documents an ordering
  EXPECTATION with a defined failure mode, names the adapter as a deliberate
  violator under `reorder_prob`, and is pinned by
  `an_out_of_order_trade_folds_into_the_open_window_without_wedging`.)

- WRITE UP, then delete this entry: why unordered HttpOrders dispatch is fidelity
  rather than a defect (formerly sweep item X6). `dispatch_order` hands each
  Submit/Modify/Cancel to `get_runtime().spawn` with no sequencing, so a submit
  followed immediately by a cancel can arrive at `/orders` reversed. That is
  exactly what nautilus's own production adapters do - the Binance futures client
  sends every order command through `spawn_task`, a bare `runtime.spawn` onto an
  abort list, with no cross-command sequencing anywhere - so real REST order
  entry has no ordering guarantee and sequencing mogwai would make its HTTP
  profile MORE orderly than the venues it stands in for. Blast radius is narrow:
  `TransportProfile::default()` is `WsStreaming` and broadarrow does not override
  it, so an HTTP profile is opt-in per scenario. `reference/architecture.md`
  already discloses the race; what it lacks is this REASON. The per-client
  ordered queue previously floated is explicitly REJECTED so it is not
  re-proposed. A genuinely separate feature, only if wanted in practice: an
  opt-in ordering mode to tell a strategy bug from a transport race while
  debugging.

- DECIDE (client side, spans two repos): should a venue fault be terminal for the
  consumer? mogwai now distinguishes failing from misbehaving on the wire -
  `SubscriptionIssue::is_venue_fault()` alongside `is_refusal()`, a WS 1011 close
  naming the fault, and an adapter error arm ahead of the refusal catch-all (see
  `reference/havoc.md`, "Misbehaving is not failing"). But the adapter's ordinary
  reconnect logic still fires on that close, so the client reconnects,
  resubscribes, and carries on with a hole in its history. Making the fault
  terminal end to end means the adapter treating 1011 differently from a routine
  disconnect AND broadarrow failing the run rather than resuming. Deliberately
  not taken here: the venue says clearly what happened, what the consumer does
  with that is the consumer's call.

## Notes / gotchas

- broadarrow standing notes (2026-07-31, their request that landed the
  order-status query surface): (a) the ack-delay havoc band above their ~25 s
  INFLIGHT_TIMEOUT is deliberately unserved - they permanently declined a
  per-venue ceiling on that safety timeout, so do not invest in DelayAcks/
  GoDark scenarios past it (also recorded in reference/havoc.md's operator
  note); (b) the once-floated MarketIfTouched order-type extension is dead
  (the triggering Pine shape is invalid on TradingView and nautilus cannot
  rest an MIT faithfully) - the protocol owes no order-type growth beyond
  Market and Limit. Standing consequence for them, surfaced by QA 2026-08-01
  and now written up in `reference/architecture.md`: a strategy whose
  protective leg is a stop-MARKET cannot be forward-tested on MOGWAI at all,
  because the adapter refuses the type and MOGWAI is the only keyless venue
  `ba forward` can use. Nothing to build here - the refusal message now names
  the limit replacement - but their pre-deployment procedure documents a shape
  their own tooling cannot exercise.

- Two broadarrow decisions their developer flagged, recorded so the mogwai-side
  residues read as connected rather than orphaned. (a) Enabling the continuous
  open-order poll closes the mid-run dropped-resting-cancel window for real
  venues, at REST-budget cost and needing a per-venue reconciliation override
  that does not exist; it was recorded as inert against mogwai because there was
  nothing for it to call, which is no longer true - the venue-truth order query
  exists, so mogwai would answer it. (b) Raising the inflight ceiling for mogwai
  only is largely moot now: the ceiling was a problem because mogwai could not
  answer `QueryOrder` and every inflight order escalated to a synthesized
  timeout, and it answers now, so the brake fires only when havoc actually
  withholds the reply - which is what the brake is for.

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
  `PID_POLL_INTERVAL 25ms`, `TAPE_SLEEP_POLL 20ms`, `TAPE_HEADROOM_POLL 5ms`.
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
  `max_concurrent_tapes 256`, `max_subscriptions_per_connection 256`,
  `fanout_depth 4096`, `zero_speed_stall_ms 5000`, and the funded `balances`
  table).
