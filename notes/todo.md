# TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

Once an item here is completed, it GETS REMOVED ENTIRELY. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new ../reference/ document.

Or both. There are no exceptions.

## Open issues

- OWNER DECISION, rewrite leftovers (assessments in the phase-3b
  report and notes/rust-rewrite-phases.md): select_windows.py -
  absorb with a targets-frozen gate (one small slice; the
  session-profile port already landed its infrastructure) or
  re-sentence to KEEP until a sampling-frame purchase question
  returns; tick_composition_ratios.py - reading it settled the triage
  ambiguity, it is a report generator over Rust-produced fixtures
  with no independent estimator, so ABSORB as a --report mode on the
  tick-composition subcommand is the honest verdict, with the frozen
  baseline tables moving as data, never re-derived.

- OWNER DECISION: the committed `analysis/fingerprint.json` has drifted
  from its gitignored `analysis/char_*.json` inputs - regeneration
  (Python and the Rust port agree) produces
  `empirical_ranges.modal_tick.max = 0.1` where the committed artifact
  carries `0.25`, the exact ceiling MNQ's tick sits on. The fingerprint
  is compiled into the generator via include_str, so re-committing it is
  a `TAPE_PROTOCOL_VERSION` decision, not a hygiene fix; the ranges are
  warnings-not-admission since the 3.3 resolution, but the shipped-preset
  test makes unaccepted warnings fatal for committed presets. Found
  2026-08-06 by the rewrite phase-3a fingerprint parity gate. Do nothing
  until ruled.

- The TBBO stream contract has an unguarded conversion class, in BOTH
  implementations: `parse_stream` (analysis/mnq_fit.py and its
  phase-1 port crates/mogwai-lab/src/stream.rs) converts
  price/size/bid_sz/ask_sz without a named Refusal, so a malformed
  non-integer field crashes instead of refusing cleanly. The Rust
  port mirrors the Python deliberately (parity first); once the
  Python retires at rewrite phase 4, decide whether these joins the
  refusal contract. Found 2026-08-06 by the phase-1 port.

- INVESTIGATE the fanout-capacity accept-before-fill failure. A CORRECTNESS
  investigation, not capacity tuning: with `fanout_depth = 16_777_216`,
  `a_banded_limit_fills_from_the_run_sweep` fails DETERMINISTICALLY - the
  fill reaches the socket without a prior `OrderAccepted` satisfying
  `fill.ts_event > accepted_ts` - and passes just as deterministically at
  the shipped 4,194,304. Exact reproduction, measured 2026-08-06 on the
  protocol-11 landing tree: `tests/configs/band.toml` (default BTCUSDT
  venue, speed 100), the other three protocol-11 ceiling resizes present
  in BOTH trees, and
  `brokkr test -p mogwai-server a_banded_limit_fills_from_the_run_sweep -N 5`
  reading 0 of 5 passing at 16,777,216 against 5 of 5 at 4,194,304.
  The assertion CANNOT yet distinguish wire reordering (the fill frame
  arriving before `OrderAccepted`) from timestamp inversion -
  `accepted_ts.is_some_and(..)` fails identically for both - so the first
  diagnostic must capture separately: whether `OrderAccepted` arrived
  before the fill; both timestamps when both exist; the sweep pass's
  `to_ns`; the run-clock anchor and the tape-worker anchor; and the
  broadcast-channel construction duration (or RSS) if the eager
  allocation is to be blamed rather than estimated. Ring depth is not
  consulted after construction at nonzero speed, so the suspected channel
  is the allocation shifting boot phase relative to the anchored run
  clock - suspected, not established. The shipped default carries the
  reviewed protocol-11 policy exception and is pinned by
  `the_fanout_default_carries_the_protocol_11_exception`; this item is
  about the latent serving defect the rejected capacity exposes.

- PROBLEM STATEMENTS. **This was the solvable set of problems believed to get
  mogwai to the end state the user needs.** That was a claim rather than an
  inventory: each entry was believed NECESSARY, and the set was believed
  SUFFICIENT FOR MOGWAI TO STOP BEING THE BLOCKER. All seven have now resolved
  into landed code, which is the point at which the claim becomes checkable
  rather than assumed - and whether the end state is in fact reached is an
  observation to make going forward, not a re-litigation to hold here. If it
  turns out not to be, that is a finding worth having, and the reason the claim
  was stated as a claim rather than as a list.

  ZERO, DOWN FROM SEVEN. `problem-order-book.md` is deleted: the user's fill
  model needed no book, and what remained open after that ruling - the
  volatility estimator, the band's scale and shape, the derived RNG stream,
  self-trade impossibility - is landed code (`a214996` and follow-on commits),
  pinned by the tests and docs the landing itself cites. `problem-fees.md`
  dissolved into the instrument model (an exchange charges fees, so the
  schedule is one more config knob) and was deleted earlier.
  `problem-refused-order-types.md` is deleted the same way: the venue now
  accepts `StopMarket` and `StopLimit`, with reduce-only and post-only as
  first-class flags and the touch-versus-through trigger distinction, and the
  adapter stopped refusing them at conversion - landed code, pinned by the
  engine, server and adapter test suites the landing itself added. The
  MECHANISM half of `problem-instrument-profiles.md` went the same way, and the
  document's one surviving question - whether the arrival and volatility
  process constants are genuinely per-instrument - was answered by the same
  parameterization ruling that closed `problem-instrument-model.md` below: the
  model is a complete parameterization, so those constants are per-instrument
  because everything is. Last of the seven, `problem-instrument-model.md` and
  its spec, `spec-instrument-model.md`, are deleted with it: the venue now
  models an instrument as a bundle of knobs rather than one hardcoded spot
  shape - instrument identity and class, a multiplier-aware contract size grid,
  a futures margin ledger with mark-to-market and settlement, a session
  calendar with genuine closure, a fee schedule reaching the consumer as booked
  commission, `position_id` end to end under both netting and hedging, and a
  preset layer with mandatory provenance - landed code, pinned by the test
  suites and gates each landing added. `reference/architecture.md`,
  `docs/config.md`, `reference/glossary.md`, `docs/cli.md`,
  `reference/performance.md` and `docs/presets.md` /
  `docs/oms-types.md` carry what must endure; the landing history is git's, not
  this file's, to keep. `notes/` now holds no problem statement or spec files
  at all - only this one.

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

  ORDERING WAS A GRAPH, NOT A LINE, while the set was open - recorded here as
  a historical note rather than left to imply an active dependency structure,
  since there is nothing left in the set to sequence. Two independent reviews
  found an earlier total-order draft circular; the graph that replaced it ran
  `lifecycle` and `seeds` into everything else, and `cadence` and
  `instrument-model` both into `profiles`, with `order-types` gaining no
  inbound edge once the fill band replaced the order book it would otherwise
  have waited on. All of it resolved in landing order without the graph ever
  needing to be redrawn again.

  The end state they served: on the order of 200 agents running concurrently,
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

  DELETED, not archived: `notes/problem-instrument-profiles.md`. Its mechanism
  half had already moved to the instrument model under the parameterization
  ruling. Its one surviving question - whether the arrival and volatility
  PROCESS constants become per-instrument at all - is answered by that same
  ruling and needed no separate document: the model is a COMPLETE
  parameterization and a preset is a named bundle of otherwise-tunable knobs, so
  those constants are per-instrument because EVERYTHING is per-instrument. The
  arrival constants, the GARCH parameters and `SIZE_LOG_SIGMA` get slots like any
  other knob.

  What survives is not a design question but a FITTING one, and it belongs to
  whoever builds each preset: whether BTC and ETH genuinely differ enough to
  warrant different values, which the measured 2.8x dispersion spread across
  three crypto majors suggests but one month of one venue cannot settle. That is
  answered when the data arrives - trade-level or 1-second archives spanning
  years are expected - and it gates nothing in the meantime, because the venue
  can already EXPRESS a difference whether or not one is fitted. The evidence
  asymmetry the document recorded stays true and stays relevant to preset
  authors: BTC, ETH and SOL have trade-level archives, MNQ and MES have
  15-second bars and nothing else, so a CME preset's cadence is derived
  arithmetic and its clustering comes from nowhere at all. Each preset says
  where its numbers came from.

  DELETED, not archived: `notes/problem-fees.md`. The engine books zero
  commission on every fill, which biases every claim optimistically and
  systematically - but an exchange charges fees, so under the parameterization
  ruling the schedule is one more config knob and the problem belongs to the
  instrument model, which now carries it. Its "declare fee-free and push cost
  onto the consumer" exit was independently closed and the reason is recorded
  there: nautilus computes commission client-side only in its SIMULATED matching
  engine, so on the live path a venue reporting no commission is
  indistinguishable from one that charges none, and nothing downstream can
  correct for it. Also deleted, its problem fully landed: `notes/problem-
  refused-order-types.md`. The venue was refusing `StopMarket` and
  `StopLimit` at conversion; it now serves both, first class - a four-variant
  `OrderType`, a `Resting` state machine distinguishing a live limit from an
  untriggered conditional from an inert market remainder, a stop that
  triggers on TOUCH rather than THROUGH, reduce-only and post-only as wire
  flags enforced at fill time, and the adapter's `wire_order_type` no longer
  refuses the two types. Trailing stops and two-leg brackets remain refused
  by name, and stay refused - the user ruled them out rather than deferred
  them.

  RAISED IN REVIEW AND RULED ON, recorded so they are not raised a third time.
  (a) Three documents each partly re-scope the realism gate - cadence
  invalidates its anchors, profiles moves the arrival constants out from under it,
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
  identity chain the code builds - now just run, tape and ledger, since the
  lifecycle landing collapsed account, session and subscription out of it.

- GTD / `expire_time` on the wire, with a time-driven expiry pass on the
  sweeper. Refused today for limits and conditionals alike - the conditional-
  order-type landing carried a GTC-only rule for stops for exactly this
  reason, and closing the gap needs a wire expiry field plus an expiry pass
  that has nothing to do with triggers, so it is its own item rather than
  bundled with anything else that touches order lifecycle.

- A trigger-act latency havoc arm, if a scenario ever needs a trigger fired
  later than the sweep interval already allows. Deliberately not built with
  the rest of the conditional-order-type surface: the sweep interval already
  bounds how late a trigger can be, and a per-trigger delay knob would be a
  new arm rather than an extension of an existing one.

- The price-SPAN-per-inferred-match-event measurement, still owed. The
  triggered-stop fill (like the plain market order before it) slips by the
  existing fill band, reused rather than separately fitted; how wide that
  band SHOULD be for a triggered stop is a scale question this measurement
  would answer, and it has never been computed - the sweep tail quoted
  elsewhere (up to 2,213 aggTrade rows in one inferred event on BTC) counts
  rows rather than distinct prices, so it does not establish how far a
  marketable order actually walks. One probe extension over archives already
  on disk would settle it; until then the slippage magnitude stays an
  unquantified mechanism shared by every order type that slips.

- RESTORE DISCRIMINATION to the fill golden's banded half. Found 2026-08-03 while
  re-calibrating `fill_band_vol_mult` from `0.5` to `0.005`: the re-blessed
  `crates/mogwai-server/tests/golden/fill_distribution.json` now has its five
  banded cells BYTE-IDENTICAL to its five unbanded ones - same fill counts, same
  latency vectors, same pass counts. The banded half therefore certifies that the
  band pipeline RUNS, not that the band BITES, and a regression that silently
  zeroed the band would still pass this golden.
  The cause is resolution rather than calibration. Latency is quantized to the
  harness's one-second `SWEEP_INTERVAL_NS`, and one second of raw-fill tape
  carries roughly fifty prints travelling much further than the 0-to-4 ticks -
  about 0.1 basis points on a 37,000 tape - that a `0.005` band displaces a
  trigger by, so the tape crosses the displacement inside the same sweep pass.
  The old `0.5` discriminated only because it was clamp-saturated at 200 ticks,
  which is the defect the re-calibration removed; this is the bill for fixing it,
  not a new regression.
  Two knobs restore it, both costing runtime in a harness whose coverage was
  deliberately cut for runtime: a finer `SWEEP_INTERVAL_NS`, so sub-second
  latency differences are representable, and a tighter offset ladder, so the band
  is a large fraction of the distance to the limit rather than a rounding error
  against it. Neither was taken. A third option is to stop asking this artifact
  the question and add a direct assertion that a banded trigger differs from its
  stated price, which is cheap but proves much less.

- RE-SCOPE the acceptance-time market reading, or accept 12.6 ms inside a
  submit. Measured 2026-08-03 by `read_market_latency_stays_within_submit_budget`
  after that instrument was corrected to time the cache MISS rather than a
  warmed hit. The cadence landing applied lever two of that gate's own
  KEEP/REVERT rule (memoize per symbol per sweep interval, `MarketReadingCache`)
  and not lever one (a shorter `VOL_WINDOW_NS` or an otherwise re-scoped
  reading), so the 5 ms budget is met on the hit path (~0.13 ms) and missed by
  2.5x on the miss path. Lever one moves the estimator's identity and re-blesses
  the fill golden, which is why the cadence spec put it out of scope; it is
  still owed. Two prices are being paid for that: the 12.6 ms itself, and the
  loss of an exactly-stated slippage contract (the reading instant is not on the
  wire, so both end-to-end gates now assert a bracket - see the doc comment on
  `MarketReadingCache`). Putting the reading instant on `OrderFilled` would buy
  the contract back cheaply and independently of the re-scoping.

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

- BUILD, eventually: a CLI binary in `mogwai-data` that becomes the standing
  home for the offline data machinery, in Rust. The repo now carries 30+
  Python scripts across `analysis/` and `scripts/`, which is too much surface
  to keep correct by convention; the corpus-scale work (archive preflight,
  fidelity probes, fingerprint characterization) is exactly the streaming-
  parse-and-count shape Rust is good at. The 2026-08-05 preflight optimization
  measured the Python ceiling honestly: a bytes-mode hot loop plus a process
  pool gets a 128.7M-row month to ~58 s, which is 2.21M rows/sec.

  Do NOT justify this item with the 2.9M ticks/sec figure. That is
  `SYNTHESIS_TICKS_PER_SEC`, the rate at which the generator MANUFACTURES a tick
  - GARCH recursion, RNG draws, checkpoint retention, measured over a whole boot
  interval - and manufacturing a tick is far heavier per item than splitting a
  CSV line. Set beside Python's 2.21M rows/sec it looks like near-parity and so
  argues AGAINST the migration, which is the opposite of the truth. The real
  comparator is a Rust byte-level parse, which should run far faster than
  either, and NOBODY HAS MEASURED IT. The expected win is large but currently
  unquantified; a one-month Rust parse prototype timed against the same
  128.7M-row archive would settle it for the cost of an afternoon, and this item
  should carry that number before it is scheduled on performance grounds.

  Prior art that cuts against the stated home: `tick-composition` started as
  `mogwai-data/examples/tick_composition.rs` and was MOVED to
  `mogwai-server/src/tick_composition.rs` as a subcommand of the `mogwai` bin
  (494f00f). It needed the server's preset resolution, which is a real reason
  and may not apply here - archive preflight only parses files and wants nothing
  from the server. So `mogwai-data` may still be right, but the precedent points
  the other way and should be distinguished deliberately rather than overlooked.

  Consolidating also collapses the twice-computed-definition risk the
  `analysis/` test-harness item below records (`dwell_stats` versus
  `empty_hour_stats`), since one implementation would serve both the corpus
  measurement and the generator gate. Scope decisions when picked up: which
  scripts migrate versus die, whether the bin reads ZIP archives directly
  (a new dependency), and how the per-month JSON result contract carries over.

- NUMERICAL STABILITY in `AutoCorr`, and it needs cadence-impact analysis before
  anyone touches it. Surfaced 2026-08-05 by the F3-F6 conformance fixtures. Its
  `acf()` guards zero variance with `if var <= 0: return [0.0] * k`, and that
  guard fires only when the variance is EXACTLY zero. A series constant at an
  irrational value - the fixture case is `abs(log return)` constant at ln2 -
  leaves a tiny positive float residue from `sumsq / n - mean * mean`, so the
  guard misses and the returned ACF comes out of catastrophic cancellation
  rather than measurement. Both branches are wrong in the same way the report
  keeps recording: a number is substituted where the honest answer is that the
  quantity is undefined for a constant series.

  NOT fixed, deliberately, and not during the sampling-frame experiment.
  `AutoCorr` computes the F1 duration ACFs as well as the return ACFs, and its
  output is bit-exact against `analysis/cadence.json` - `duration_acf_lag1`
  0.32204142581620676 and `duration_acf_lag5` 0.22388204486699373 - which is the
  lineage the fingerprint's cadence half rests on. Changing the estimator
  invalidates that equivalence, which is a stronger guarantee than the fix buys.

  What a fix would need: return an explicit unavailable rather than zeros, a
  relative rather than absolute variance floor, and possibly a two-pass or
  Welford accumulation to stop the cancellation at source. Each changes numbers,
  so the work is the ANALYSIS of what moves in the cadence targets and whether
  the fingerprint must be refitted, not the code change. Real monthly series
  carry positive return variance and come nowhere near the degenerate case, so
  nothing currently depends on this being fixed.

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

  Read this WITH the Rust-consolidation item above, which may moot part of it.
  If `dwell_stats` migrates to Rust it stops needing a Python harness and stops
  being a second definition at all, so investing in a Python test toolchain for
  it first would be work with a known expiry. The part that survives either way
  is the surviving Python that does NOT migrate, and deciding which scripts
  those are is the migration item's scope question. Sequence accordingly: that
  decision comes first, this one is downstream of it.

- REPAIR the `brokkr check --gate` profile mismatch for
  `tape_lateness_under_acceleration`. The gate runs the workspace test pass in
  DEBUG profile, and that test asserts a 50ms p99 WALL-CLOCK pacing bound the
  debug server cannot reliably meet (measured 2026-08-05: debug p99 106-330ms
  across trees whose release builds pass at 38-43ms on a quiet box). The test
  is also load-sensitive even in release: with a second workspace building
  (load average 1.0), 4 of 5 release repetitions failed at ~250ms on BOTH the
  protocol-9 parent and the protocol-10 candidate - indistinguishable paired
  distributions, recorded in `notes/mnq-generator-successor-spec.md` as a
  reviewed gate exception. The 50ms release threshold stays authoritative and
  unrelaxed. Options when picked up: exclude the test from the gate's debug
  lane the way the other perf-bound tests are already `--skip`ped, run it in
  a release lane, or gate it on a load check. Until then `check --gate`
  cannot be read as green/red on this one test without a quiet-box release
  rerun, which is exactly the ambiguity the gate exists to remove.

- UNPROVEN, and it decides whether the venue-identity check needs to stop being
  opt-in: can a full session be established against a stranger holding a reused
  port, and this client's account id stamped onto its state, inside the window
  before anyone notices? An external QA pass forced the port reuse and showed
  the adapter DOES dial a dead venue's address and a stranger DOES accept the
  connections, but their stranger was a bare TCP listener that accepted and
  closed, so the stamping half was never demonstrated. Their bound on the window
  (about 160 ms) came entirely from the consumer's own child-exit poll, which
  covers nothing for a consumer that does not own the venue as a child, and
  nothing at all when the venue is wedged rather than exited.
  `expected_run_seed` closes it for anyone who sets one; what is undecided is
  whether a config WITHOUT one should keep dialling blind. Answering it needs a
  stub venue that speaks enough of the wire to complete a handshake.

- The venue frees its port BEFORE it exits: a declared completion stops the
  accept loop, then drains live connections for up to `SHUTDOWN_GRACE`. So the
  address is reusable while the process is still alive, which is why a consumer
  watching for child exit sees nothing during that window. Not a defect - the
  drain is deliberate - but it is the mechanism behind the item above, and worth
  keeping in view before anyone shortens or lengthens the grace.

## Notes / gotchas

- The CLA check is NOT yet a required status check. cla-assistant.io is wired up
  and its webhook delivers (verified `200 OK` on a real PR), but nothing blocks
  an unsigned merge until a repository ruleset requires the check by name. The
  trap: an owner-authored PR produces no status at all - the CLA assigns
  copyright TO the owner, so the bot correctly has nothing to ask - which means
  the check cannot be picked from the suggestion list and cannot be validated
  against a real run. Type the context name in by hand and leave the rule in
  EVALUATE mode until an outside PR confirms it, because a required check that
  never reports blocks every merge with no visible cause.

- broadarrow standing notes (2026-07-31, their request that landed the
  order-status query surface): (a) the ack-delay havoc band above their ~25 s
  INFLIGHT_TIMEOUT is deliberately unserved - they permanently declined a
  per-venue ceiling on that safety timeout, so do not invest in DelayAcks/
  GoDark scenarios past it; (b) the once-floated MarketIfTouched order-type
  extension is dead
  (the triggering Pine shape is invalid on TradingView and nautilus cannot
  rest an MIT faithfully) - and their position was that the protocol owes no
  order-type growth beyond Market and Limit. SUPERSEDED as of 2026-08-02: that
  was a consumer's preference, and mogwai is a nautilus adapter whose owed
  surface follows what nautilus expresses. MarketIfTouched specifically stays
  dead unless re-argued. The standing consequence for them is now RESOLVED: the
  venue serves `StopMarket` and `StopLimit` first class (`reference/
  architecture.md`), so a strategy whose protective leg is a stop-MARKET is
  forward-testable on MOGWAI. Nothing left to build here; their pre-deployment
  procedure no longer documents a shape their own tooling cannot exercise.

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
  lands; (c) any stored scenario TOMLs setting a `transport_profile` on either
  adapter config now fail to parse, since the field is gone with
  `TransportProfile` itself (the lifecycle landing removed the HTTP transport
  entirely, not just its deliverability refusal), and need a sweep. (The
  data-path WARN template that named three wrong causes turned out to live in
  mogwai-adapter, not ba - fixed
  here: it now defers to the venue's `reason`, and the WS lifecycle logs
  disconnect/backoff/reconnect/exhaustion per socket.)
- The offline Kraken corpus is trades only - no quotes, no L2, no aggressor side.
  This shapes the offline analysis only; the running server synthesizes trades
  with a native `Buyer`/`Seller` aggressor AND, since tape protocol 7, publishes
  an observable top of book - one BBO before every parent burst, bounded history
  on `/quotes`, and a connect-time snapshot. This line asserted the opposite
  until 2026-08-05, and `DATA-PURCHASE-REPORT.md` section 12 records it as one of
  two existing records that contradicted that report and went unconsulted, so it
  is worth keeping accurate rather than deleting. The quoted width, top sizes and
  trade displacement remain explicitly uncalibrated placeholders pending CME
  TBBO; what is absent is the calibration, not the layer. `KrakenCsvSource` and
  `TickRuleAggressor` survive in `mogwai-data` for the offline lineage and its
  unit tests.
- `MOGWAI_DATA_DIR` (default `/home/folk/Kraken`) is an
  offline-analysis input only (`analysis/`), never a server runtime knob.
- `research/` is gitignored and holds read-only nautilus, broadarrow and piners
  clones plus `market-data/` (the Binance archives and TradingView exports) and
  `binance-public-data/` (the vendored downloader). Read those APIs from there.
  mogwai BUILDS against the pinned crates.io nautilus release (0.61), never
  against `research/` and no longer against a sibling checkout; see `AGENTS.md`.

## Hardcoded-value and env-var inventory (swept 2026-07-01, re-verified 2026-08-03)

STALE BY CONSTRUCTION between sweeps, and not covered by the removal rule at the
top of this file: it is a point-in-time catalogue rather than a set of work
items, so nothing here gets removed on completion and nothing re-sweeps it
automatically. The 2026-08-03 pass corrected six entries that had drifted -
`MAX_HISTORY_LIMIT` (1000 to 50_000), `CHECKPOINT_K` (8192 to 262_144), a
`/orders` route that no longer exists, `gap_cap_ms` which no longer exists
anywhere, `sim_epoch_ns` which became derived-and-refused rather than a knob,
and a `NO_COLOR` entry that briefly went stale with `man` and came back with it.
That hit rate is the reason for the standing instruction: treat every line as a
LEAD to verify against the source, never as a statement of fact.

Catalogue only, for later evaluation of what deserves to become a knob - nothing
here was changed. Pervasive test-fixture literals (repeated `BTCUSDT`/`BTC`/
`USDT`, golden seed 42, per-assertion timing tolerances) are summarised rather
than enumerated line-by-line; production and config-relevant values are listed in
full.

### Environment variables (whole workspace)

The Rust crates are deliberately env-var-free for runtime knobs; run config lives
in `mogwai.toml`. `RUST_LOG` is the only ambient read on the SERVING path. The
reads:

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

- Correctly single-sourced from `mogwai-protocol` (the pattern to follow):
  `DEFAULT_REQUEST_TIMEOUT_SECS` (30) and `MAX_HISTORY_LIMIT` (50_000) - the
  adapter references these rather than re-hardcoding them.
- `default_instruments()` BTCUSDT seed lives in `mogwai-protocol` but its seven
  literals are duplicated verbatim in two of that crate's own tests, and the smoke
  test's fixed order shape implicitly depends on it.

### mogwai-protocol (canonical wire defaults)

Named consts, canonical: `DEFAULT_REQUEST_TIMEOUT_SECS = 30`, `MAX_HISTORY_LIMIT
= 50_000`, `BASELINE_LATENCY.base_nanos = 30_000_000` (30ms honest-feed latency
floor), `MAX_LATENCY_NANOS = 60_000_000_000` (60s per-field ceiling),
`control::MAX_DIVERGENCE_MS = 3_600_000` (1h DelayAcks/GoDark/StallData ceiling),
`ReadyRecord::VERSION = 5`.

The `launch` module (the shipped launcher) adds `DEFAULT_BINARY = "mogwai"`,
`DEFAULT_READY_TIMEOUT = 300s`, `STDERR_RING = 64` retained lines, and
`OWNER_POLL = 200ms` (how often the owning thread notices the venue ended on its
own). It also puts `serde_json` and `tracing` on this crate at RUNTIME rather
than dev-only: the launcher parses the readiness line and announces the run it
started.

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

- Venue/trade id prefixes `V`/`T` as inline magic strings.
- Test fixtures repeat `BTCUSDT`/`BTC`/`USDT`, a base price of 100, and
  partial-fill fractions 0.3/0.4/0.5 across dozens of sites (no shared consts).

### mogwai-server

- Bind: the `BIND_ADDR` const, `127.0.0.1:0`, not configurable at all - the
  `--addr` flag is gone, so ephemeral loopback is the only endpoint and it is
  reported on stdout as the readiness line, and on stderr as `mogwai listening`.
- HTTP route strings (`/health`, `/account`, `/instruments`, `/trades`,
  `/quotes`, `/clock`, `/ws`, `/control/divergence`) as inline
  literals, no shared registry with the adapter's route segments.
- `Config::default()`: `speed 1.0`, `server_heartbeat_ms 0`,
  `warmup_ns 86_400_000_000_000` (24h), `account_id` from
  `DEFAULT_ACCOUNT_ID = "MOGWAI-001"`. `gap_cap_ms` no longer exists anywhere in
  the workspace, and `sim_epoch_ns` is no longer a config key at all - it is
  DERIVED as `TAPE_ORIGIN_NS + warmup_ns`, and a config file stating it is
  refused by the parser.
- `account_id` is validated for the `ISSUER-NUMBER` shape at load, which is a
  NAUTILUS rule enforced by a crate that does not import nautilus. The venue's
  own wire type accepts a bare word; nautilus cannot construct an `AccountId`
  from one, so a venue reporting `MOGWAI` booted fine and was refused by every
  consumer.
- `SYNTHESIS_TICKS_PER_SEC = 2_900_000`, the boot projection's rate. MEASURED,
  not chosen - see the warmup section of `reference/performance.md` for the runs
  and the method. It read 5_000_000 for a while, making the projection 1.7x
  optimistic and the 60-second WARN threshold fire at about 104 seconds.
- Lifecycle timeout consts: `SHUTDOWN_GRACE 5s`, `TAPE_SLEEP_POLL 20ms`,
  `TAPE_HEADROOM_POLL 5ms`.
- Channel capacity `1024` duplicated inline for the writer channel and the
  exec-delay pump channel (different traffic classes, no shared const).
- Synthesis limits: `CHECKPOINT_K 262_144`. The test-side `HORIZON_S 86_400.0`
  stands in for the production `warmup_ns` default as a plain literal and can
  silently drift from it.

### mogwai-adapter

- `base_url` is now required on both configs (no default endpoint); a launcher
  learns it from the readiness record. `for_addr` builds a config from the
  reported address; `for_run` also captures `expected_run_seed` from the record,
  which is what binds a client to a RUN rather than to an address that may be
  reused. Builders cover havoc, oms type, account type and trader id.
- `expected_run_seed` unset dials blind, the historical behaviour. Set, every
  dial checks `/health` and a different run is refused TERMINALLY, logged as
  `venue identity mismatch`. Two non-answers are deliberately not mismatches and
  are reported as distinct categories: no usable answer is a transport failure,
  a well-formed answer carrying no `run_seed` is version skew.
- `MOGWAI_VENUE_STR = "MOGWAI"` (correctly single-sourced).
- Default `TraderId` `MOGWAI-001` in the exec config. `AccountId` no longer
  defaults to it on either config: both carry the `UNSET_ACCOUNT_ID` placeholder
  (`MOGWAI-UNSET`) and `validate_account_id` refuses a config that still states
  it, so an omitted account fails loudly instead of silently binding a slot.
  `TEST_ACCOUNT_ID` keeps `MOGWAI-001` for in-crate fixtures.
- Timeout consts: `ACCOUNT_REGISTRATION_TIMEOUT 5s`, `ACCOUNT_REGISTRATION_POLL
  10ms`, `MIN_WALL_REQUEST_TIMEOUT_SECS 1` (flagged in its own comment as the
  tightest cap on usable sim speed). `wait_connected` re-hardcodes an
  independent 5s/10ms pair matching the registration consts by value but not
  sharing them.
- `1_000_000_000` (nanos-per-second) repeated inline 5+ times across `client.rs`
  and `lifecycle.rs` - a `NANOS_PER_SEC` const would remove the repetition.
- Triplicated test `def()` instrument fixture (`price_precision 2`/`size_precision
  8`) across three test modules.

### mogwai-data (generator)

Fingerprint/distribution constants are named module consts, fitted-and-committed
by design (changing them re-shapes the synthetic market): quiet share 0.35,
state persistence 0.90, quiet/active mean ratio 150, Weibull shape 1.0,
GARCH 0.12 / 0.875, Student-t df 4.0, bounce and drift transition
probabilities, `SIZE_LOG_SIGMA 1.15`, `MAX_ABS_RETURN 2e-5`,
`GARCH_SIGMA_CAP 1e-5`, anchor `START_PRICE_USD 60_000`, and
`VOL_SCALAR 1e-6`. The real fingerprint numbers live in
`analysis/fingerprint.json` (embedded via `include_str!`), not in Rust.

Inline (not named): `xbtusd_anchor` fields `XBTUSD` / `modal_tick 0.1` /
`price_decimals 1` (deliberately per-pair, kept in the constructor); the `1e9`
mid-price runaway ceiling duplicated at two sites; `round_lot_size` thresholds
(1.0 / 10.0 / 0.1). `seed`, checkpoint `k`, and `max_extend` have no production
default here (caller-supplied by the server); seed `42` is the pervasive
golden-test seed.

### Non-crate (scripts, analysis, root config)

- `scripts/smoke.py`: spawns its own venue and learns the bound address from
  the readiness record read off the child's stdout (no hardcoded host/port). `WINDOW_LOOKBACK_NS 1h`, `ACCEL_DELAY_MS 1000`,
  `ACCEL_CLOCK_SLACK_WALL_NS 50ms`, `ACCEL_ANCHOR_TIMEOUT_S 120`, fixed order
  shape (`BTCUSDT`/`Limit`/qty 10/px 100), plus many inline per-assertion
  socket timeouts and latency tolerances (not centralised; first place to look
  if the smoke ever gets flaky).
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
  deps live in `mogwai-adapter/Cargo.toml`, not root, and are five crates.io
  dependencies pinned at 0.61 with default-features off. `brokkr.toml` only sets
  `project = "mogwai"`. Root `mogwai.toml` is an EXAMPLE run config, not one the
  server reads (nothing consults the working directory): `speed 1.0`,
  `server_heartbeat_ms 0`, `run_duration_ns 0`, `warmup_ns` 24h,
  `fanout_depth 65536`, `zero_speed_stall_ms 5000`, the fill-band and admission
  knobs, and the funded `balances` table. It states neither `sim_epoch_ns` nor
  `wall_anchor_ns` - both are derived at boot, and the former is refused as a
  key.
