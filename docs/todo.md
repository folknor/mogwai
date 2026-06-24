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
nautilus seam. The single upstream ask narrowed to just a *clock-injection
point*: the `Clock` trait already owns timer creation AND firing (`LiveTimer`
pushes `TimeEvent`s into a `TimeEventSender` the runner only drains in its
`select!` loop, with no wall-clock assumption), so the accelerated `Clock` is
ours to implement - nautilus only needs to let a live/sandbox node construct on
a caller-supplied clock factory instead of hardwiring `LiveClock::default()` at
`kernel.rs initialize_clock` and `trader.rs create_component_clock`. Filed
upstream as nautechsystems/nautilus_trader#4304. This item is parked until that
lands; no mogwai spec or code until the nautilus seam exists.

- [ ] Upstream: nautilus accepts a non-wall `Clock` for a live node - filed as
      nautechsystems/nautilus_trader#4304.
- [ ] Then write the spec per `reference/technical-implementation-spec.md`
      before any mogwai/adapter code (this item is the TODO source it cites).

### 2. Bug-hunt follow-ups - the residual tail

Closed - nothing here is open. The project-wide bug/duplication hunt ran as six
fix waves; every follow-up has landed or been decided (closed-as-correct or
wontfix). The per-item dispositions and their rationale live in the fix-wave
commit messages, keyed by the `B.8` / `C.2` / `D.10` / etc. tags - consult git
log rather than re-deriving them, so a settled bug is not re-flagged.

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
