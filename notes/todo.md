# TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

Once an item here is completed, it GETS REMOVED ENTIRELY. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new ../reference/ document.

Or both. There are no exceptions.

## Open issues

- PROBLEM STATEMENTS. **This is the solvable set of problems we believe will
  get mogwai to the end state the user needs.** That is a claim rather than an
  inventory: each entry is believed NECESSARY, and the set is believed
  SUFFICIENT FOR MOGWAI TO STOP BEING THE BLOCKER. If all seven resolve and the
  end state is still out of reach, the claim was wrong - which is a finding
  worth having, and the reason it is stated as a claim rather than as a list.

  SEVEN, DOWN FROM EIGHT, as of the 2026-08-02 review pass. `problem-fees.md`
  dissolved into the instrument model (an exchange charges fees, so the schedule
  is one more config knob) and was deleted. The MECHANISM half of
  `problem-instrument-profiles.md` went the same way, but that document SURVIVES
  and still counts: its empirical question - whether the arrival and volatility
  process constants are per-instrument - is untouched by any ruling. An earlier
  version of this paragraph said six and was wrong; count the files in `notes/`.

  PREMISES THE USER HAS SETTLED, which every document below inherits and none
  previously stated. Forward tests always run ACCELERATED, never at speed 1.0 -
  which is a correctness bound rather than a cost one, since the adapter's
  one-second minimum wall request timeout caps usable sim speed and a timed-out
  request is a failed run. A run has an OPTIONAL DURATION in sim time,
  defaulting to indefinite. There is NO RESTART and NO RESUME; mogwai is fire
  and forget, and reproducing a path means a fresh instance with the same seed
  and config. WARMUP is declared config, so the venue generates it eagerly at
  boot and `MAX_HISTORY_SEEK_TICKS` dies with the lazy history it existed to
  bound. Strategies are SINGLE-INSTRUMENT, which is why independent per-symbol
  tapes carrying no cross-instrument correlation is correct rather than a defect.
  There is ONE `MOGWAI` venue, not one per asset class.

  THE SUFFICIENCY CLAIM HAS NO EVIDENCE AND IS NOT MEANT TO. Two review passes
  have now flagged that, correctly as a matter of fact and beside the point as a
  matter of genre: the first paragraph says outright that this is a claim rather
  than an inventory, and that being wrong about it is the finding it exists to
  produce. It cannot be evidenced in advance without already having built the
  thing. Do not raise it a third time; raise a MISSING ENTRY instead, which is
  the falsifiable form of the same objection.

  Three things are deliberately outside that claim. THROUGHPUT - whether N
  instances fit on the machine - is excluded by the user's standing instruction
  that resource cost shapes no decision here. The CLAIM PIPELINE - how a seed
  becomes provenance attached to a result, how many paths make a claim, how they
  are allocated - belongs to whatever consumes the venue; mogwai's obligation
  ends at generating a path and reporting which. And the open items elsewhere in
  this file, notably the dead-feed watchdog and the terminal-venue-fault
  decision, both bear on whether a forward result is VALID and are not part of
  this set.

  They are ordinary todo items that outgrew a bullet, so they live in their own
  files; they carry the evidence, the decisions to be made, and what is
  explicitly out of scope, but no implementation plan. A spec is written against
  `reference/technical-implementation-spec.md` only once the problem statement
  it descends from has been resolved.

  ORDERING IS A GRAPH, NOT A LINE. An earlier draft asserted a total order and
  two independent reviews found it circular. The dependencies below are
  technical, and where none exists the order is free:

      lifecycle ─┬─> everything (it decides what a run IS)
      seeds ─────┘

      order-types ──requirements──> book ──> cadence ──> profiles
                                     ▲                     ▲
      instrument-model ──────────────┴─────────────────────┘

  The one non-obvious edge: order-type REQUIREMENTS precede the book, because a
  market-state model cannot be chosen before it is known what triggers, fills
  and reduce-only behaviour it must support - even though implementing those
  types lands after the book resolves.

  The instrument model has grown two inbound absorptions (fees, and the profile
  mechanism) without gaining an inbound dependency, so its position in the graph
  is unchanged. `profiles` is now a thin node: only the per-instrument
  clustering question.

  The end state they serve: on the order of 200 agents running concurrently,
  each developing a strategy through broadarrow - backtest, optimize, Monte
  Carlo - and then FORWARD TESTING it against mogwai. Whether that many
  instances fit on the machine is explicitly not a design input; resource cost
  does not shape any decision in these documents.

  WHO DECIDES: the repository owner, on every product and architecture question
  in every one of these documents. There is one user, and the operator of the
  venue is an agent acting for them. broadarrow is a consumer, not an authority -
  mogwai is a nautilus adapter, so where a standing broadarrow note conflicts
  with what nautilus strategies emit, the note is a preference and loses.
  Consulting them is courtesy, not process.

  ACCEPTANCE was previously listed here as the largest defect these documents
  share - that none names a measurable form of "done". That paragraph was wrong
  at the layer it applied: gates are a
  `reference/technical-implementation-spec.md` concern, stated there as exact
  copy-pasteable commands, and a problem statement that carried them would be
  doing the spec's job. The documents are correct to omit them. Two things
  survive the removal. The SET needs no acceptance criterion at all - the
  repository owner is the gate and will know. But the cadence document does
  invalidate a currently-green gate, the 0.1603 duration ACF anchor, without
  naming a successor, and that debt is real and belongs to whichever spec
  descends from it.

  - `notes/problem-server-lifecycle.md` - mogwai is one long-lived service and
    the workload is hundreds of disposable instances. Nothing allocates a port,
    discovers one, or cleans one up, so concurrent forward tests collide today
    and were hand-serialized during the 2026-08-02 session. Also the root of the
    account namespace that produced that session's adapter defect. Decides what
    a RUN is, so everything else inherits from it.
  - `notes/problem-seeds-and-paths.md` - the tape already varies per launch,
    because tape identity includes a `data_origin` derived from wall time, but
    the variation is accidental, unsampled and unrecorded. Decided by the user:
    one axis, a random seed per launch, deterministic given that seed, wall
    anchor removed, seed reported. What sets the origin instead is open. Its
    restart question is CLOSED - there is no restart.
  - `notes/problem-order-book.md` - the founding no-book assumption. Two
    independent axes: what exists to match against, and what happens to a client
    order. The user has answered the second - orders rest and are consumed by
    arriving flow, accounts never match each other - and argued that a
    probabilistic fill model can supply allocation without modelled depth, which
    keeps the cheapest market-state option alive. Resolves before cadence,
    because under matching the generator emits parent arrivals and wire prints
    fall out of it. Now carries a bound the earlier draft asserted its way past:
    trade data constrains an UPPER BOUND on fill, not a fill, because
    queue-ahead volume is unrecoverable from trades - so the probabilistic model
    randomizes the queue-ahead objection rather than escaping it, and the
    residual parameter is declared. A fifth market-state option (A5,
    bookTicker-fitted top of book) was added as the cheapest way to shrink that
    declared surface. NEEDS ITS OWN RESEARCH SESSION before any spec.
  - `notes/problem-trade-cadence.md` - the tape runs orders of magnitude slower
    than a real active pair, and "trades per second" has three values differing
    by 8.5x because raw fills, aggregated prints and match events are LAYERS of
    one process rather than alternatives. Also carries the market-data
    provenance for the whole set: where the archives live, how to fetch more,
    which committed probe reproduces each figure, and why none of it is
    reproducible from a fresh clone yet.
  - `notes/problem-instrument-model.md` - the venue models spot currency pairs
    only, so MNQ and MES cannot be futures: no multiplier, no tick value, no
    expiry, no margin, and a session envelope that can thin an hour but not
    close it, with no calendar and no exchange-local time. Precedes profiles,
    because a profile cannot be fitted for an instrument that cannot exist. NOW
    THE LARGEST DOCUMENT IN THE SET: the user ruled first-class, and that the
    model is a complete PARAMETERIZATION rather than an enum of supported
    instruments - presets are named bundles of otherwise-tunable knobs, and a
    user who picks none must be able to invent MCL, MBT or AAPL. Fees and the
    profile mechanism both land here as a result, as do margin and the general
    config-versus-havoc line (config is the instrument's identity; havoc is a
    deviation from it, so a scheduled CME close is config and `ReopenGap` stays
    havoc for unscheduled halts only). Its decision list was renumbered - it
    previously ran 1 to 6 then repeated 4 and 5, so older citations by number
    are ambiguous.
  - `notes/problem-instrument-profiles.md` - REDUCED to one question. Its
    mechanism half (named presets, overlay, provenance, selection) moved to the
    instrument model under the parameterization ruling. What remains is
    empirical and untouched by any decision: whether the arrival and volatility
    PROCESS constants become per-instrument at all, which measured clustering
    differing 2.8x across three crypto majors leaves genuinely open. Confirming
    that spread across months is an ACQUISITION, not a probe run: aggTrades are
    held for June only, and the April and May archives are 1-second klines that
    cannot describe sub-second structure at all.
  - `notes/problem-refused-order-types.md` - every conditional type is refused,
    so a strategy with a protective stop cannot be forward-tested at all. The
    owed surface is what nautilus expresses, not what today's consumer happens
    to emit. Its REQUIREMENTS precede the book; its implementation follows it.
  DELETED, not archived: `notes/problem-fees.md`. The engine books zero
  commission on every fill, which biases every claim optimistically and
  systematically - but an exchange charges fees, so under the parameterization
  ruling the schedule is one more config knob and the problem belongs to the
  instrument model, which now carries it. Its "declare fee-free and push cost
  onto the consumer" exit was independently closed and the reason is recorded
  there: nautilus computes commission client-side only in its SIMULATED matching
  engine, so on the live path a venue reporting no commission is
  indistinguishable from one that charges none, and nothing downstream can
  correct for it.

  RAISED IN REVIEW AND RULED ON, recorded so they are not raised a third time.
  (a) Three documents each partly re-scope the realism gate - cadence
  invalidates its anchors, profiles moves the ACD constants out from under it,
  and the parameterization ruling lets config move the tape anywhere - and it
  was argued that nobody owns the result. The owner is the repository owner, the
  same answer as ACCEPTANCE above. (b) Three documents each want to rewrite part
  of `mogwai-engine` (per-run state, matching, a margin ledger) and nothing
  sequences the REWRITES as opposed to the decisions. That is spec-level, the
  same layer error the acceptance paragraph made. (c) The dead-feed watchdog and
  the terminal-venue-fault item stay OUTSIDE the set. A venue fault is
  mogwai failing to do its job and is obviously terminal, and mogwai surfaces it
  as such where it can tell - but in most cases it cannot, because a real
  failure shows up as a crashed or stalled PID rather than as a protocol event.
  Under fire-and-forget instances tied to a parent process that is exactly what
  the owner observes, so the silent-but-socket-alive failure the watchdog was
  designed for was a property of the long-lived shared daemon being deleted. The
  watchdog is not worthless; it is not structural.

  Also relevant and not a problem statement: `reference/glossary.md` defines the
  identity chain the code builds - process, account, session, subscription, tape
  - and carries a numbered register of discrepancies found while writing it,
  several of which are work items in their own right.

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

- The reconciliation exposure is a CLASS, not one method: every report path
  mogwai relies on shares the silent-degrade property. The socket-backed guard
  in `crates/mogwai-adapter/tests/reconciliation.rs` seeds venue truth and pins
  each granular generator, `query_order`, and their mass-status composition over
  both query carriers. Known limitation: it proves the adapter WOULD answer when
  asked, not that the node asks. Related upstream, queued in the maintainer's PR
  tracker and NOT a substitute for this guard (mogwai overrides the method, so a
  better trait default protects the next adapter author, not this repo): give
  the Rust trait default the same composing behavior as the Python base.

- BUILD: a positive dead-feed watchdog (formerly sweep item AD12). No liveness
  timer, tick counter, or "0 ticks in N s" log exists on either transport. The
  negative diagnostics are all in place - the server emits a `ProtocolError` on
  an unservable subscribe, the adapter's data drain warns rather than swallowing
  it, and the poll loop self-heals after a server restart - but nothing
  positively proves a subscribed feed is alive rather than genuinely quiet. The
  WS idle timeout does not cover it: `idle_timeout_ms` defaults to 0, and even
  armed the idle clock resets on ANY application frame, so a
  data-silent-but-frame-active socket never trips it, deliberately, because that
  is what reproduces the 4255 case. The landed default-tape dwell bound is what
  supplies the threshold separating "the venue is asleep" from "the subscription
  is dead": honest silence on the dense default tape now has a gated upper
  bound (the realism gate's era-windowed p999 gap, empty-hour fraction and
  longest empty-hour run), and an armed LiquidityDrought legitimately
  silences the feed but is visible via the control plane, so the watchdog can
  account for it.

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

- DECIDE (client side only now, and it is broadarrow's half): should a venue
  fault be terminal for the consumer? The MOGWAI-SIDE half is settled - a venue
  fault is mogwai failing to perform its duties, it is terminal, and the venue
  says so as clearly as it can. What remains is what the consumer does with
  that, which is theirs. mogwai now distinguishes failing from misbehaving on the wire -
  `SubscriptionIssue::is_venue_fault()` alongside `is_refusal()`, a WS 1011 close
  naming the fault, and an adapter error arm ahead of the refusal catch-all (see
  `reference/havoc.md`, "Misbehaving is not failing"). But the adapter's ordinary
  reconnect logic still fires on that close, so the client reconnects,
  resubscribes, and carries on with a hole in its history. Making the fault
  terminal end to end means the adapter treating 1011 differently from a routine
  disconnect AND broadarrow failing the run rather than resuming. Deliberately
  not taken here: the venue says clearly what happened, what the consumer does
  with that is the consumer's call.

- DECIDE: does `analysis/` deserve a test harness? Surfaced 2026-08-02 landing
  the drought elimination. The dwell statistics are computed TWICE against the
  same definition - `dwell_stats` in `analysis/characterize.py` measures the
  corpus, `empty_hour_stats` in `mogwai-data`'s generator tests measures the
  synthetic tape, and the gate compares one against the other. If the two hour-
  bucket conventions ever drift (inclusive end boundary, the era-start ceiling,
  which trade closes a gap) the gate silently compares two different quantities
  and still passes. The Rust side has a fixture pinning the convention; the
  dwell convention on the Python side still has none - the Rust fixture names
  `dwell_stats` as the counterpart it must match, which is the cheapest honest
  mitigation and not a real pin. The "no Python test runner at all" half of
  this is now stale: the queue-ahead measurement landed
  `analysis/test_characterize.py` plus an `analysis/__init__.py`, runnable as
  `python3 -m unittest discover -s analysis -t .` with no dependency beyond the
  stdlib, because a verdict was being read off an untested estimator. That is a
  bridgehead, not the decision. What is still open: whether that runner becomes
  the standing one (versus pytest), whether it joins `brokkr check` or stays a
  manual step, and whether the existing analysis code - `dwell_stats` first -
  gets retrofitted onto it. Adding a second test toolchain to a workspace whose
  gate is `brokkr check` is a project-shape call, not a local fix.

## Notes / gotchas

- broadarrow standing notes (2026-07-31, their request that landed the
  order-status query surface): (a) the ack-delay havoc band above their ~25 s
  INFLIGHT_TIMEOUT is deliberately unserved - they permanently declined a
  per-venue ceiling on that safety timeout, so do not invest in DelayAcks/
  GoDark scenarios past it (also recorded in reference/havoc.md's operator
  note); (b) the once-floated MarketIfTouched order-type extension is dead
  (the triggering Pine shape is invalid on TradingView and nautilus cannot
  rest an MIT faithfully) - and their position was that the protocol owes no
  order-type growth beyond Market and Limit. SUPERSEDED as of 2026-08-02 by
  `notes/problem-refused-order-types.md`: that was a consumer's preference, and
  mogwai is a nautilus adapter whose owed surface follows what nautilus
  expresses. MarketIfTouched specifically stays dead unless re-argued. Standing
  consequence for them, surfaced by QA 2026-08-01
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
- `MOGWAI_DATA_DIR` (default `/home/folk/Kraken`) is an
  offline-analysis input only (`analysis/`), never a server runtime knob.
- `research/` is gitignored and holds the read-only nautilus and broadarrow
  clones plus `market-data/` (the Binance archives and TradingView exports) and
  `binance-public-data/` (the vendored downloader). Read those APIs from there.
  mogwai BUILDS against the sibling `../nautilus_trader` checkout, per the open
  path-dependency item above and `AGENTS.md` - not against crates.io, and never
  against `research/`.

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
  `/home/folk/Kraken`. Offline-analysis input only, never a
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
- Default `TraderId` `MOGWAI-001` in the exec config. `AccountId` no longer
  defaults to it on either config: both carry the `UNSET_ACCOUNT_ID` placeholder
  (`MOGWAI-UNSET`) and `validate_account_id` refuses a config that still states
  it, so an omitted account fails loudly instead of silently binding a slot.
  `TEST_ACCOUNT_ID` keeps `MOGWAI-001` for in-crate fixtures.
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
