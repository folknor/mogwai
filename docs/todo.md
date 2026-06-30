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
log); the clearly-correct bugs and safe consolidations all landed. The `mogwai`
binary exposes a clap command line: `serve` runs the gateway (with `--config`
and `--addr`), `man` renders the `reference/*.md` docs compiled into the binary,
and `--version` reports the git-stamped build. Every havoc knob is now
range-checked at its boundary; the client-latency cap (`MAX_LATENCY_NANOS`,
60 s) closed the last unbounded one.

## Next

### 1. Coherent simulated clock for accelerated forward testing (NEEDS A SPEC)

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

**Feasibility crux - RESOLVED, and the upstream seam has LANDED.** A nautilus
`LiveNode` previously could not be driven by a non-wall `Clock` without an
upstream change. Three independent walls existed:

- The kernel hardwired the clock by environment with no injection seam:
  `crates/system/src/kernel.rs` `initialize_clock` was a bare match - `Backtest`
  got a `TestClock`, `Live | Sandbox` got `LiveClock::default()`. Neither the
  config nor the builder accepted a caller-supplied `Clock`.
- `LiveClock`'s time-read axis is the process-global realtime singleton:
  `crates/common/src/live/clock.rs` takes `get_atomic_clock_realtime()` and
  `timestamp_ns()` just reads it.
- The timer-fire axis is independently wall-bound: `LiveClock` timers are Tokio
  `LiveTimer`s that sleep a real duration, so time-in-force expiries and
  bar-aggregation windows tick at 1x even if the time-read axis were scaled.

The first wall - the missing clock-injection point - is now closed upstream.
Issue nautechsystems/nautilus_trader#4304 was resolved by PR #4331 (commit
`338b64b`) and refined by `66dcfd5`. A live/sandbox node can now be constructed
on a caller-supplied clock instead of hardwiring `LiveClock::default()`. The
landed API (`crates/system/src/clock_factory.rs`):

- `ClockFactory` - a cloneable struct wrapping a re-invocable
  `Rc<dyn Fn() -> Rc<RefCell<dyn Clock>>>`. It memoizes ONE *primary* clock via
  an inner `OnceCell` and mints *fresh* component clocks on demand:
  - `ClockFactory::new(closure)` - build from a clock constructor.
  - `clock()` - the memoized primary clock, shared by the kernel and the trader
    timestamps (lazily created on first call, same `Rc` thereafter).
  - `create_component_clock()` - a brand-new clock instance per call, so each
    component's timer/callback registration stays isolated.
  - `for_environment(Environment)` / `test_default()` - defaults: `TestClock`
    for `Backtest`, `LiveClock` for `Live | Sandbox` (the live default is
    feature-gated behind `live`; without it the factory panics with guidance).
- Threaded through `NautilusKernelBuilder`, `LiveNodeBuilder::with_clock_factory`,
  `NautilusKernel::new_with`, and `Trader::new`. Supplying no factory is
  byte-identical to the old `LiveClock::default()` behavior; the backtest path
  (`TestClock`) is unchanged.

The remaining two walls (the realtime-singleton read axis and the Tokio
`LiveTimer` fire axis) are ours to solve, NOT upstream's: the `Clock` trait
already owns both timer creation AND firing (`LiveTimer` pushes `TimeEvent`s
into a `TimeEventSender` the runner only drains in its `select!` loop, with no
wall-clock assumption), so the accelerated `Clock` is a mogwai implementation
we hand to the node via `with_clock_factory`. This item is now UNPARKED: the
spec can be written.

- [x] Upstream: nautilus accepts a non-wall `Clock` for a live node -
      nautechsystems/nautilus_trader#4304, landed as PR #4331 (`338b64b`,
      refined by `66dcfd5`). The seam is `ClockFactory` +
      `LiveNodeBuilder::with_clock_factory`.
- [x] Spec written: `docs/coherent-clock-spec.md`, per
      `reference/technical-implementation-spec.md`. Design: one affine
      `SimClock` (`sim = sim_epoch + (wall - wall_anchor) * speed`) shared by
      server and adapter; deadline-paced replay binds the data stream to the
      clock; the adapter implements `MogwaiClock` (sim reads, scaled-wall timer
      fires into the runner `TimeEventSender`) and exports a `ClockFactory` via
      `mogwai_clock_factory`, closing both non-upstream walls. `mogwai-engine`
      needs no change (its `process(msg, ts)` already abstracts the clock).
- [ ] Implement the spec in three keep/revert landings (protocol SimClock ->
      server sim-time + `/clock` + deadline pacing + accelerated smoke ->
      adapter MogwaiClock + factory). Then the downstream broadarrow wiring
      (`with_clock_factory` in `ba-worker.rs`), which is a separate repo's item
      named and excluded by the spec.

### 2. Bug-hunt follow-ups - the residual tail

Closed - nothing here is open. The project-wide bug/duplication hunt ran as six
fix waves; every follow-up has landed or been decided (closed-as-correct or
wontfix). The per-item dispositions and their rationale live in the fix-wave
commit messages, keyed by the `B.8` / `C.2` / `D.10` / etc. tags - consult git
log rather than re-deriving them, so a settled bug is not re-flagged.

### 3. Coherent forward-test data origin (SPEC WRITTEN)

`../freedom/FINDINGS.md` #13: a forward warmup never receives historical bars
and dies on a fatal timeout. Root cause is a coherence bug - the tape is frozen
at `ORIGIN_TS` (2023) while the identity clock advertises wall-now (2026), so
the warmup window lands years past the tape start, blows the 50k seek cap, and
returns an empty page. Fix: delete the frozen origin, derive
`data_origin = sim_now_at_boot - backfill_horizon` from the clock, unify the
live path to seek the shared tape (closing the warmup/live discontinuity and the
reconnect price-reset), refuse off-tape warmups loudly, and publish the tape
boundary on `/clock` so broadarrow can guard its own warmup. Spec:
`docs/forward-origin-spec.md`, per `reference/technical-implementation-spec.md`.

- [x] Spec written: `docs/forward-origin-spec.md`. Four landings - measure the
      seek; boot-derived origin + unified seek + refuse-straddle (server);
      publish `ServerClock` + adapter consumption; checkpointed seek (conditional
      on the measurement). broadarrow's count-based warmup guard + honest message,
      #12, the O(1) block-addressable generator, and per-symbol horizon are named
      and excluded.
- [x] Landing 1 (measurement): `seek_throughput_measurement` in
      `crates/mogwai-server/src/source.rs` prices the from-origin `BoundedSeek`.
      Reading at ~1.9M ticks per second synthesis: a 24h fast-cadence warmup is
      ~22k ticks (11.7 ms), comfortably under the 100 ms request budget B. But
      the budget-affordable cap C_B is ~190k ticks while honoring the 2h
      fresh-subscribe floor F at speed 120 demands C_F ~244k ticks, so no single
      cap satisfies both B and F -> Landing 4 (checkpointed seek) PROCEEDS. A
      from-origin seek cannot decouple per-request cost from session length; the
      verdict is throughput-sensitive near the boundary but structurally PROCEEDS.
- [x] Landing 2 (boot-derived origin + unified seek + refuse-straddle): the
      frozen `ORIGIN_TS` is gone; `Config.backfill_horizon_ns` and
      `AppState.data_origin_ns` derive the tape origin from the boot clock; both
      builders anchor there; `build_live_source` wraps `BoundedSeek` and seeks to
      `start_ts.unwrap_or(sim_now)`; `/trades` refuses a pre-origin start with a
      422; `MAX_HISTORY_SEEK_TICKS` is 190k. Default smoke green.
- [x] Landing 3 (publish the affordance + adapter consumption): `ServerClock`
      in `mogwai-protocol` (sim plus server_now_ns, data_origin_ns,
      backfill_horizon_ns); `/clock` returns it; the adapter `fetch_clock`
      decodes it, the node clock takes `.sim`, the data client stores
      data_origin_ns and refuses an off-tape warmup at the request boundary,
      surfaces a fetch error instead of dropping it, and a live subscribe sends
      `start_ts = None`. The cross-repo `ba forward` confirmation is the
      operator's, not a mogwai gate.
- [ ] Landing 4 (checkpointed seek), suite green at the boundary.

### 4. Self-detaching `mogwai serve --daemon` (DEFERRED)

`serve` logs to a file (`--log-file`, default `mogwai.log`, written per event so a
backgrounded server stays greppable) and prints a flushed `Listening.` banner to
stdout. It is still a foreground process: it holds the terminal until killed,
which is the right shape for systemd / containers / CI and is fine for the dev
loop when launched in the background by the caller.

Deferred is making the binary detach itself so a bare `mogwai serve` returns the
prompt while the server keeps running. That is daemonization, and it drags a tail
of machinery behind it, so it stays behind an explicit opt-in rather than
becoming the default:

- A `--daemon` flag that forks, `setsid`, redirects stdio, and writes a PID file;
  the default stays foreground-clean.
- The fork MUST happen before the Tokio runtime starts (forking a multithreaded
  process and then using those threads in the child is undefined behavior), so
  `main` would drop `#[tokio::main]` and build the runtime by hand in the child.
- A `mogwai stop` subcommand that reads the PID file and signals the daemon, plus
  a graceful-shutdown handler (axum `with_graceful_shutdown`, not wired today) so
  the stop is clean.
- It also fights `cargo run` / `brokkr run`, which are foreground wrappers: a
  self-detaching child makes cargo report "finished" while the daemon lives on.

Until a workflow actually needs the bare-`serve`-returns behavior, the
file-logging plus disciplined backgrounding covers the need at far lower cost.

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
