# TODO

Open work only. How the built system works lives in `reference/architecture.md`;
the landing-by-landing history is in git; the per-crate mechanics are in code
comments.

**Not the live arc.** Work that is actively being done belongs to its own
track's document, not here - see `notes/README.md` for the map. This file is for
what is parked, deferred, unresolved or owed by someone else.

Once an item here is completed, it gets removed entirely. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new `../reference/` document.
Or both. There are no exceptions - a ruling recorded only here is a ruling the
next bug hunter re-derives from scratch.

**Where an entry here also appears in `notes/bugs.md`, bugs.md is the source of
truth.** The 2026-08-24 extraction copied entries rather than moving them, the
two copies drifted, and the reconciliation ruling (2026-08-26) is that every
correction lands in bugs.md and only there: a copy surviving here is
unmaintained, and a section verified as fully duplicated is deleted here rather
than kept in parallel. The former "Tests and tooling" section was the first
such deletion - its thirteen entries all live in bugs.md's G section, with
corrections this file's copies never received.

## The gate that blocks other work

- **Segment-sampler gate: failed 2026-08-18, still failing.** The owner viewed
  the two Asia charts and rejected both as unusable - 300-point moves inside the
  session body over one-to-twenty-minute spans, which happen at an open and never
  in Asia session body. Both arms failed, including the gaps-off control, so the
  reopen-gap injection is not the cause. The verdict and its measurements are in
  `notes/segment-sampler.md`; the probe is `analysis/asia_jump_probe.py` (the
  owner's untracked work in progress - never sweep it into a commit, and never
  put a number out of it beside a Rust-computed one, since its percentile
  convention differs from `mogwai_lab::kernel::nearest_rank_list`).

  Two repairs are owed before a re-render is worth the owner's eye:
  - the seam level discontinuity that survives `--no-reopen-gaps` and
    contaminates the control - self-contained and ours;
  - whatever the cut admits at Asia bars 1112-1113 - carried in from the segment
    data, and possibly a cut-criteria question for the owner rather than a bug.

  Nothing further is built on the composed tape until a re-render passes. In
  particular the serving wiring is a real refactor: `CheckpointIndex` is typed on
  `GeneratedSource`, so a composed river means generalizing the checkpoint and
  resume path.

  London and the two NY charts are unviewed and unprobed, so whether the defect
  is Asia-specific or general is open. Regenerating one end to end (substitute
  the window name and library path for the others):

  ```text
  brokkr run mogwai -- segments cut --symbol MNQ --month 2026-04 --window asia --out analysis/out/asia-mnq-2026-04.json
  brokkr run mogwai -- segments compose --library analysis/out/asia-mnq-2026-04.json --type bars --interval-s 60 --ticks 3000000 --seed 42 --out analysis/out/asia-endless.csv
  python3 analysis/plot_tape.py --csv analysis/out/asia-endless.csv --out analysis/out/asia-endless.html --title "Endless Asia, MNQ 2026-04 segments, seed 42"
  ```

  The cut needs the delivered corpus at
  `research/market-data/databento/mnqv/2026-04.manifest.tbbo`, which is out of
  git. April yielded: asia 22 segments and 2,976,377 ticks, london 22 and
  2,492,576, ny-morning 21 and 8,396,328, ny-afternoon 21 and 9,572,450. Times in
  these charts are composed tape time, not a calendar: a composed tape starts at
  unix ns 0 and elides the hours between sessions.

## The shared-exchange mode

The default mode - one venue owned by one run, serving N tapes to one account at
one placement - is complete. The optional shared-exchange mode has one axis
still open, removed by the one-venue-per-run rewrite and not since undone. Not
urgent; both modes must eventually be supported.

- **Named tape windows.** `SocketQuery` carries no start or end: every cursor is
  placed at the fixed `run_start_ns` origin and `duration_ms` is
  length-from-boarding, so there is no wire for naming a window at all. This is
  the half most strategies will use, because a named window is what makes a
  forward-test claim bindable: a run becomes a pure function of `(seed, config,
  symbol, start, end)` with no boarding instant and no wall-clock input anywhere,
  and replication pairs dealt the same window trade identical water by
  construction rather than approximately.

  A named window always gets its own river even against an identical request
  already running, because the first requester is by then some N of sim-time
  ahead and a window means being served from its start. Sharing therefore only
  happens for the unnamed form - a preset plus a duration - which is the request
  that says "wherever you are is fine".

  One constraint to design for up front: a strategy needs warmup before its
  requested start, so `[T1, T2]` asks for materialization from `T1 - warmup_ns`,
  and that floor must sit at or above `TAPE_ORIGIN_NS`. A window requested too
  near the tape origin cannot carry its own warmup. Better as a named refusal at
  request time than a short warmup nobody notices.

## Venue and protocol

- A passenger whose own duration ends is sent a `RunComplete` frame before the
  `close::DURATION_COMPLETE` close, identically to the whole-run arm
  (`crates/mogwai-venue/src/ws.rs`). A client classifying on the text frame reads
  a duration end as a run completion, and the close behind it is the only
  accurate statement. Nothing breaks today, since both readings stop the client,
  and the imprecision is documented at all three sites. The fix is either a
  distinct per-passenger completion frame or dropping the frame on that arm and
  letting the close carry it - both protocol changes with consumers to consider.
  Nothing detects it regressing further, because the two arms are correct in
  isolation.

- `RunComplete` reports slightly less than the declared duration and nothing on
  the wire lets a consumer tell that from a short run. The deadline is judged on
  the venue clock while `ws.rs` re-derives every announcement on the receiving
  socket's boat clock, so the announcement trails by the placement gap times
  `speed`. Both halves are deliberate and stated in `reference/clock.md`. What is
  open is whether a consumer should be able to distinguish "the run served its
  whole duration and my boat was placed late" from "the run was cut short":
  shipping the boat's epoch, or the venue's own elapsed alongside the socket's,
  would close it. A wire change nobody has asked for, and the same missing field
  as the item above.

- Refusal texts spell their bounds out instead of naming the constant.
  Re-verified 2026-08-24 and half of the original finding is closed:
  `messages::validate_wire_symbol` now says bytes and a test pins the refusal
  text against `MAX_SYMBOL_LEN`, so moving the constant fails loudly. What is
  left is cosmetic: the bound is still spelled `32` inline on the refusal a
  client sees at the venue's front door, and four divergence texts in the same
  module have the shape too - count at the production sites, since the module's
  tests carry the same strings as expected values. Both refusals return
  `&'static str`, so fixing means changing the return type or reaching for a
  `const` formatter, which is why neither was fixed in passing.

## Engine

- A zero-price fill is still warned about and booked by `warn_zero_px`, so a
  position can carry `mark_px == 0` if the tape produces one.
  `position_unrealized_checked`'s zero answer is the backstop for exactly that
  case. Refusing at the fill was considered and rejected in the 2026-08-20
  ruling, because by then the tape has already produced the print and aborting
  the serving path over it is the one thing no venue does. Open only as a
  known-covered case.

- Account valuation residue, none of it blocking:
  - One hop only. An asset is valued through an instrument quoting it directly in
    the policy currency, so holding ETH under a USD policy with only ETHUSDT and
    BTCUSD listed leaves the account unvaluable rather than valued through a
    chain. A rate surface would fix it and buys cross-currency accounts too.
  - The mark is as stale as the last sweep, inherited from the margin ledger.
  - The ledger-generality ruling still wants shares, leverage and funding
    payments; each needs a holding valued in a currency it is not denominated in,
    so this machinery is the part of that which now exists.

## Instruments and account policy

- Account policy and the new instrument classes, still open:
  - Nothing has been fitted for equity, perpetual or inverse. A symbol configured
    as one is served the default tape wearing a different shape; the intake
    sequence is what makes a preset honest and none has been run.
  - Not answered: whether a havoc-induced disconnect should behave differently
    from a real one. The venue armed the blackout so it knows the client is
    merely blinded, and `GoDark` is arguably toothless if the world stops while
    it is armed. Freeze-and-resume covers it for now and nothing is blocked.
  - Open and small: whether a strategy should see its own remaining budget in
    order to size against it. It can derive peak and threshold from its own fills
    and marks, so blind trading is workable. Decide it when one asks.

- Owner call: should an order-list release carry a market reading? A standalone
  `MarketToLimit` takes the market, while the same type released as an order-list
  child rests at its stated price, because `Engine::release_child` runs inside
  `apply_linkage_after_fill` with no `MarketReading` to price against. Resting is
  the consistent behaviour and the carve-out is stated in `docs/oms-types.md`, so
  nothing is broken. Deciding yes would make a released `MarketToLimit` execute
  on arrival and would reopen whether a `Market` child can be admitted too.

- A trigger-act latency havoc arm, if a scenario ever needs a trigger fired later
  than the sweep interval already allows. Deliberately not built: the sweep
  interval already bounds trigger lateness, and a per-trigger delay knob is a new
  arm rather than an extension of an existing one.

## Data and generator

- **Generator defects inherited from the closed measure-and-fit arc.** Recorded
  2026-08-23 as a transcription from that arc's deleted documents, with the
  evidence in `notes/tape-research-v1.md`. Re-verified against the code
  2026-08-24: of the three named, two are closed and one is live but was
  mischaracterized. Treat anything else inherited from that arc with the same
  suspicion.

  Still unverified, and all that survives of the `ARRIVAL_MEAN_CAL = 0.944`
  half: the claim that the shipped path carries a 5.5 to 7.0 percent
  absolute-rate conflict against the observed July month. That is a question
  about the shipped sampling scheme's rate, not about the calibration leaking
  into the integrated frame, which
  `the_arrival_mean_calibration_stays_off_the_integrated_frame` now gates on
  both sides. A Jensen-gap explanation for it was refuted in closed form, so
  establish whether it reproduces before acting on it.

  Live: the calendar has no daylight rule. Not hardcoded - `utc_offset_minutes`
  is a validated calendar field and the MNQ preset declares `-300`, a CDT summer
  offset, with the model carrying one scalar and no transition. Twelve of
  twenty-four Stage M control walks, exactly the winter rotation, collapsed the
  local-hour-22 stratum to zero variance, and November put 840,315 rows outside
  declared sessions, 3.8 percent against September's 0.5. Any regression suite
  needs one daylight, one standard and one transition month. It is disclosed:
  the preset's provenance carries `calendar.utc_offset_minutes` as declared, with
  the rationale naming the unmodelled transitions. Giving the calendar a daylight
  rule is a schema change reaching every preset, which is the decision this
  actually carries.

- **The 86 MB and 57 MB build tax, and whether the dead protocol code goes.**
  `analysis/mnq-measure-12a.json` is 86 MB and is `include_str!`d at five sites,
  two of them outside `cfg(test)`, so it is baked into the shipped binary.
  `analysis/mnq-arrival-screen.json` is 57 MB and is parsed in full by
  `arrival_envelope_diagnostic.rs`'s test, which is not ignored, so every
  `brokkr check` reads it. Both are terminal outputs of the closed 12b protocol.
  They cannot be removed without deciding the larger question they sit inside:
  roughly 25,000 lines across `mogwai-lab` and `mogwai-cli` are the compiled
  machinery of the closed arc (the arrival screen, control and envelope family,
  `measure12a`, `aggregate`, `stage_m` and its Tier 2 limb, `count_curve`,
  `ordered_counts`, `slow_geometry`, `tick_composition`, `select_windows`), and
  the binary still advertises them as supported subcommands. Owner call, deferred
  until v2's shape is known, since a successor may want some of the corpus-side
  machinery.

- **Nine deleted Python scripts are still referenced about forty times.**
  `analysis/mnq_fit.py` alone has roughly thirty references across `mogwai-lab`
  and `mogwai-cli`; `characterize.py`, `build_fingerprint.py`, `select_windows.py`,
  `build_cadence.py`, `run_corpus.py`, `fit_session_profile.py`,
  `check_cadence_feasible.py` and `tick_composition_ratios.py` account for the
  rest, across doc comments, `docs/cli.md`, `crates/mogwai-venue/presets/mnq.toml`
  and `mogwai-lab`'s `Cargo.toml`. Re-checked 2026-08-24: `AGENTS.md` is clean
  now, the rest are not. One is
  not prose: `mogwai-lab/src/fingerprint.rs` emits the runtime error
  "analysis/cadence.json is required; run build_cadence.py first", instructing the
  user to run a script deleted in the Rust port. `scripts/retire_note_citations.py`
  is the existing tool for this sweep but is scoped to `crates/` and `brokkr.toml`.

- `SegmentSource` overrides neither `seek_to` nor `fault`. An effectively
  infinite source inherits the O(distance) default walk that `mogwai-data`'s own
  `TickSource` doc warns about - the shape `GeneratedSource` needed
  `CheckpointIndex` for - harmless today only because `mogwai segments compose`
  walks forward from the origin and never seeks. It becomes a hang the moment
  anything serves a composed river or asks it for a window.

  `fault` is the harder half: the composer's one terminal condition, clock
  exhaustion, has no `TickFault` variant and is reported only through the
  inherent `SegmentSource::clock_exhausted`, which a `dyn TickSource` consumer
  cannot see, and adding a variant ripples into `mogwai-venue`'s `http.rs` fault
  rendering. The same item owns `emit_price`'s panic: a named panic inside
  `TickSource::next_tick` in a library crate, which becomes a serving-path abort
  the moment a composed river is served, where `GeneratedSource`'s equivalent
  failures go through `TickFault`. Giving the composer a `TickFault` closes both
  halves at once.

- Generator havoc must fork the river. The tape machinery deliberately mutates a
  canonical boatless river instead, with the pinned control-boundary snapshot,
  the coarsen exemption and the walk-back floor built to make non-forking
  correct, so the gap has known size and known work to undo. The seated-boat
  refusal standing in for the fork names a remedy no route exposes, and its gate
  reads boat presence in the non-awaiting form, so it is vacuous against a
  concurrent board.

- The synthetic top of book is uncalibrated: quoted width, top sizes and trade
  displacement are placeholder constants pending CME TBBO. The layer exists since
  tape protocol 7; what is absent is the calibration.

- Numerical stability in `AutoCorr`
  (`crates/mogwai-lab/src/characterize/mod.rs`), needing cadence-impact analysis
  before anyone touches it. Its `acf()` guards zero variance with `if var <= 0.0
  { return vec![0.0; self.k] }`, which catches zero and any negative residue but
  not a positive one, so a series constant at an irrational value - the fixture
  case is `abs(log return)` constant at ln2 - can leave a tiny positive residue
  from `sumsq / n - mean * mean`, slip the guard, and return an ACF that came out
  of catastrophic cancellation rather than measurement. Which side of zero the
  residue lands on is not something the caller controls. Both branches substitute a number where the honest
  answer is that the quantity is undefined for a constant series.

  Deliberately not fixed. `AutoCorr` also computes the F1 duration ACFs and is
  bit-exact against `analysis/cadence.json` (`duration_acf_lag1`
  0.32204142581620676, `duration_acf_lag5` 0.22388204486699373), which is the
  lineage the fingerprint's cadence half rests on, and changing the estimator
  invalidates that equivalence. A fix returns an explicit unavailable rather than
  zeros, uses a relative rather than absolute variance floor, and probably a
  two-pass or Welford accumulation - all of which move numbers, so the work is
  the analysis of what moves in the cadence targets and whether the fingerprint
  must be refitted. Real monthly series carry positive return variance and come
  nowhere near the degenerate case.

## Adapter

- **Nautilus has no instrument type for mogwai's leveraged Forex class.** On
  this side the symptom was silent publication as `CurrencyPair`: that type is
  spot/cash, so it discarded the marked-position settlement model, rollover,
  swaps, pip and point conventions, and originally the contract multiplier.
  Nautilus would need to ship a distinct leveraged-FX instrument whose notional
  and P&L use the multiplier and whose account model can represent daily swap
  settlement. An info-bag workaround of the kind `equity` uses for
  `mogwai_borrowable` does not close it: nautilus computes notional itself, at
  an implicit multiplier of 1, so a preserved multiplier in `Params` would sit
  beside a wrong number rather than correcting it.

  What was done instead: `convert::instrument_any` refuses the `forex` class
  with a named error rather than flattening it onto `CurrencyPair`. Its one
  production caller is `instrument_any_or_warn`, so the observable behaviour is
  a `warn` naming the symbol and the reason, and the instrument is absent from
  nautilus's cache - which makes a host refuse every bar for that symbol. That
  is loud and wrong-shaped rather than quiet and wrong-valued.

  What remains wrong, and is merely disclosed: a mogwai venue serving a `forex`
  instrument is unusable from a nautilus host. The venue, the config surface and
  the native protocol all still support the class; only this adapter cannot
  carry it. It becomes available the day nautilus ships that instrument shape,
  and the fix at that point is a new arm in `instrument_any`, not a change to
  the venue.

- **Nautilus's own `MarketToLimit` constructor cannot express the mogwai
  wire's market-to-limit.** The two models disagree about who names the limit:
  nautilus's `MarketToLimitOrder::new_checked` builds its `OrderInitialized`
  with `price: None` and a comment saying the price is determined on fill,
  while `mogwai_protocol::validate_submit_order` requires a `MarketToLimit`
  submit to carry a price, because on this venue the limit the remainder rests
  at is the consumer's to state. So a factory-built market-to-limit is refused
  at `SubmitPhase::PreStamp` - since the bugs-adapter arc that refusal lands
  locally as an `OrderDenied` before any event is emitted; before it, the venue
  refused the identical frame at its decode boundary, so no verdict changed.
  The workaround is host-side: set `price` on the `OrderInitialized` by hand
  before `SubmitOrder::new`, which for this one type is the contract rather
  than a defect (documented in `docs/adapter-lifecycle.md`). Closing it
  properly is a cross-repository question: either nautilus grows a stated-limit
  form of the type, or the adapter would need a limit it cannot invent - it has
  no reading to price one from, and guessing would name the number the venue
  exists to own. No adapter test submits a `MarketToLimit` today, which is how
  the gap stayed invisible; a test pinning the refusal's reason would at least
  make it loud.

- **`perpetual`'s four funding fields are still dropped silently at
  `convert::instrument_any`.** `funding_interval_ns`, `funding_rate`,
  `index_symbol` and `funding_clamp` have nowhere to go on nautilus's
  `CryptoPerpetual`. Lower impact than the forex loss and deliberately not
  given the same refusal: no arithmetic result is wrong, and nautilus exposes
  funding through the separate `DataEvent::FundingRate` channel, so the shape
  to build is a publisher on that channel rather than a bail. Nothing has been
  built for it and nothing warns, which is the part worth remembering.

- **Confirmed, and now an owner call: `DuplicateNextFill` certifies nothing
  against a nautilus host.** Verified 2026-08-23 against the pinned version -
  `mogwai-adapter` pins nautilus 0.62.0 (the earlier note said 0.61) and
  `research/nautilus_trader` is at 0.62.0, so the read is at the pin rather than
  at HEAD. `commit_fill` emits `fill.clone()`, `trade_id` included.
  `ExecutionEngine::validate_fill_for_order` calls `Order::is_duplicate_fill`,
  which matches `trade_id`, `order_side`, `last_qty` and `last_px` together -
  all four identical - and bails with a warning before `Position::apply`, whose
  own `check_predicate_true` on a repeated trade id would panic. The suppression
  is not keyed on `causation_id`, as the original note had it.

  What is left is not a bug report but a choice, and the two options inject
  different lies. Keeping the shared id models a retransmitting venue and makes
  the arm a test of the consumer's deduplication, which nautilus passes in
  silence and which nothing observes. Minting a fresh `trade_id` per emitted
  fill models a phantom execution, which a correct consumer books twice - the
  divergence the arm's own doc comments describe ("doubles the wire event, not
  the truth"). Minting also shifts every subsequent venue trade id, so it owes a
  re-bless of the exact-equality transcripts. The verdict and both readings are
  recorded at the emission site in `mogwai-engine`'s `commit_fill`.

- `await_account_registered` still polls the nautilus cache every 10 ms inside
  `connect()`, with its own 5 s wall bound. The `wait_connected` half was closed
  2026-08-24 with an adapter-owned notification plus a 250 ms backstop re-read;
  the cache half remains because the pinned nautilus cache exposes no
  registration notification, and notifying when the adapter forwards the event
  would be too early - forwarding only queues it. Closing the residue needs a
  signal at the nautilus cache insertion boundary.

- A cadence conflict that will never clear is indistinguishable from one that
  will. The adapter now retries the venue's second-cadence refusal, which is
  right - the rule lifts when the incumbent passenger leaves, and the incumbent
  need not be ours - but the one case that really is permanent, this client's
  own two legs configured with different `speed` values, dials its cap out
  behind a repeated `warn` line rather than failing at construction. The cheap
  close is upstream of the dial: a constructor that takes the pair, or a shared
  cadence value both legs read. A public API shape change on `mogwai-adapter`,
  wanting a decision rather than a patch.

- `for_run` discards `account_ttl_ms` and `reset_account_on_reconnect`, so the
  adapter's reconnect loop can back off past a freeze TTL blind.

- `HavocSpec.data` (`Option<MarketRegime>`) appears to have no reader on the
  adapter side now that the `Subscribe` carrier is retired, so an operator
  setting `[havoc.data]` may be arming a field nothing consumes - the
  looks-armed-and-is-not shape. Wants a verdict: route it or refuse it.

- The adapter's `fetch_account` names the configured account, but what a
  reported-id mismatch should mean under the per-account venue is still only a
  cosmetic log line (`note_account_label`). Whether it should ever be treated as
  an error is undecided.

- Reconciliation: the silent-degrade property is a class, shared by every report
  path mogwai relies on. `crates/mogwai-adapter/tests/reconciliation.rs` seeds
  venue truth and pins each granular generator, `query_order` and their
  mass-status composition over both query carriers. Known limitation: it proves
  the adapter would answer when asked, not that the node asks.

## Measurement and method still owed

- The price-span-per-inferred-match-event measurement. Triggered stops and plain
  market orders slip by the existing fill band, reused rather than separately
  fitted, and how wide that band should be for a triggered stop is a scale
  question that has never been computed. The sweep tail quoted elsewhere - up to
  2,213 aggTrade rows in one inferred event on BTC - counts rows rather than
  distinct prices, so it does not establish how far a marketable order actually
  walks. One probe extension over archives already on disk would settle it; until
  then the slippage magnitude is an unquantified mechanism shared by every order
  type that slips.

- Build a positive dead-feed watchdog (formerly sweep item AD12). No liveness
  timer, tick counter or "0 ticks in N s" log exists on either transport. The
  negative diagnostics are all in place, but nothing positively proves a
  subscribed feed is alive rather than genuinely quiet. `idle_timeout_ms` does
  not cover it: it defaults to 0, and even armed the idle clock resets on any
  application frame, so a data-silent-but-frame-active socket never trips it -
  deliberately, since that is what reproduces the 4255 case. The threshold comes
  from the landed default-tape dwell bound (the realism gate's era-windowed p999
  gap, empty-hour fraction and longest empty-hour run), and an armed
  LiquidityDrought legitimately silences the feed while remaining visible via the
  control plane, so the watchdog can account for it.

  Priority note: a real venue failure mostly shows up as a crashed or stalled PID
  rather than a protocol event, and the silent-but-socket-alive failure this was
  designed for was largely a property of the deleted long-lived daemon. Useful,
  not structural.

- The cadence document invalidated a currently-green gate, the 0.1603 duration
  ACF anchor, without naming a successor. That debt belongs to whichever spec
  descends from it.

- Per-instrument fitting belongs to whoever builds each preset: whether BTC and
  ETH genuinely differ enough to warrant different values - the measured 2.8x
  dispersion across three crypto majors suggests so, and one month of one venue
  cannot settle it. The evidence asymmetry stays relevant to preset authors: BTC,
  ETH and SOL have trade-level archives while MNQ and MES had 15-second bars and
  nothing else, so a CME preset's cadence is derived arithmetic and its
  clustering comes from nowhere. Each preset says where its numbers came from.
  Re-derive the asymmetry from the DBN bulk download now on disk before repeating
  it.

- Candidate symbols for the missing session classes: a perp like ETHUSDT.P, a
  second CME future like MGC, and AAPL for cash-equity hours. Terabytes of DBN
  data are already downloaded on another host, and the Databento account
  additionally holds about twelve months of MNQ/ES/MES tbbo plus mbp-1
  server-side, re-fetchable by job id at no new cost.

- Decide whether the protocol-12b Stage A refinement pass should run at all.
  Deferred by the owner 2026-08-09 rather than settled, so the frozen pass stands
  and the budgets were raised to fund it.

  For cutting: refinement is 29,200 s of the 35,526 s Stage A cost model, 82
  percent, and its entire product is a finer loss ordering over cells that Stage
  B then truncates to `STAGE_B_CELL_CAP = 24` per family. It cannot rescue a
  family whose coarse admissible region is empty - the outcome that would close
  the landing - because it subdivides around that region's own boundary cells.
  And `SELECTION_INDIFFERENCE = 0.01` already declares losses inside that margin
  as not separating candidates, so a half-spacing lattice buys precision the
  selection is defined not to use. Cutting drops Stage A to about 6,326 s.

  Against: the selected parameter point would sit on the coarse lattice, and
  nobody has shown the coarse spacing is fine enough for the mechanism to be
  found at all.

  Not the same question as `STAGE_B_CELL_CAP`, which earns its place: a Stage B
  cell is a full month-scale walk per seed at about 250 s, so an uncapped
  1,508-cell region genuinely is tens of hours. Changing `REFINEMENT_DEPTH` or
  `REFINEMENT_CELL_CAP` is a section 17 amendment against the contract of record.

- Reconcile the protocol-12b section 5.5 rescale with the shipped preset
  convention. That section freezes the negative control's re-centring as
  "rescale the 24 values to sum to 1, which the `SessionProfile` schema
  requires", and the schema requires no such thing: nothing in `config.rs` or
  `session.rs` enforces sum-to-one, and the shipped MNQ `intensity_hour` sums to
  23.862306, a mean-one curve. It moves no generated rate either way, since
  `SessionModulator::new` divides by an exposure-weighted normalizer so a common
  factor cancels at every instant, and the control's committed `new_curve` is
  therefore a correct re-centring on a different scale from its own `old_curve`.
  What it cost is readability, and it will cost the same again at any later
  reader who compares the two curves elementwise. Fixing the frozen sentence is a
  section 17 amendment through review, not an edit.

## Values that want to become named constants or knobs

- `mogwai-protocol`'s `default_instruments()` BTCUSDT seed is seven inline
  literals, duplicated verbatim in two of that crate's own tests, and the smoke
  test's fixed order shape depends on it implicitly. Its own doc comment
  signposts multi-instrument growth.
- `mogwai-venue`'s HTTP route strings are inline literals with no shared registry
  against the adapter's route segments, so a renamed route breaks the pair
  silently.
- `arrival_envelope_diagnostic` applies no job cap at all - it takes
  `available_parallelism` whole - while `arrival_screen` caps its default at 16
  for a measured SMT regression past which the run gets slower rather than
  faster. Whether the diagnostic should share the cap is open. The other half of
  this item is closed: `DEFAULT_MAX_JOBS` now carries a comment naming the
  measurement and citing `reference/performance.md`.
- `MIN_WALL_REQUEST_TIMEOUT_SECS` in the adapter is flagged in its own comment as
  the tightest cap on usable sim speed. If sim speed is ever pushed hard, that
  constant is the first wall.

## Owner rulings still open

- `Balance.locked` carries order holds, maintenance collateral and unsettled
  credits in one wire number with opposite remedies. Two scopes recommend a
  split; `Account::unsettled`'s doc in `mogwai-engine` argues the conflation is
  fine.
- Whether the evidence toolbox stays on the binary's top level - eighteen
  subcommands for one audience beside three for another - and the repeated leaf
  names inside it: `preflight` is three different commands and `fit` is two, all
  operator-typed, all producing plausible evidence output, with no collision
  warning in `docs/cli.md`.
- Whether `leg`, one of the two connections a nautilus consumer necessarily holds
  under one account, gets a glossary entry.
- The tape-identity vocabulary: river is the sequence, tape the delivery, and the
  process the version constant identifies has no name. Adjacent and real:
  `reference/architecture.md`'s version narrative walks 5 through 18 and then
  asserts the current identity, six unnarrated bumps.
- The `held` collision: `exec_held_budget_bytes` and the "held lane" use the word
  for the outbound byte-budget sense that kept `reservation`, at a
  consumer-visible config key.
- `mogwai-engine`'s fourth sense of admission - acceptance of an order onto the
  book, about 60 sites - is unruled and must not be swept as retired.
- `stage_m_tier2.rs`'s append-only candidate ledger is an unruled sixth sense of
  ledger, and `mogwai_lab::delivery` still owns the git-cleanliness oracle
  (`TreeOracle` and kin), a second unrelated job under a module named for the
  delivery manifest.

## Documentation owed

- Durable prose is owed for the account, river and passenger design. These notes
  carry no truth guarantee and nothing durable may cite them, so any part of the
  design whose reasoning lives only here is invisible to a user. Owed: the symbol
  as a label rather than an identity; the three-step resolution and its total
  third step; river identity and what forks a river; one clock per river; the
  exogeneity that gives passengers non-interference and the no-queue-competition
  contract that follows; and the boot-versus-runtime split on funding. Durable
  prose states river and passenger and never the boat, which is a cache with no
  semantics, and states the two properties a passenger is owed separately.
  `docs/presets.md` and `docs/config.md` are where a user looks;
  `reference/architecture.md` is where the why belongs.

- Half of `mogwai --help` is protocol jargon resolving to retired notes: "Brick
  B4", "Stage M", "Amendment 2" reach an operator who has no way to look them up.

- `docs/cli.md` cites "the storage policy" three times as a named authority no
  document defines.

- Structural proposals recorded and unadopted: `reference/architecture.md` is
  about 1,300 lines doing four jobs, and its contradictions have all sat where
  one job's old text survived another's landing; `docs/havoc.md` was patched
  rather than rewritten.

- Glossary and vocabulary standing rules: glossary entries are admitted by the
  owner alone, so an agent that meets an undefined load-bearing word escalates
  and never adds an entry. Vocabularies that want definitions want them in
  `reference/`, not in the glossary.

## Open upstream: nautilus_trader

Read the source from `research/nautilus_trader`; build against the pinned
crates.io release. Nothing here can be fixed from this tree.

- **`ExecutionEventEmitter` cannot share its sender**, so this adapter can only
  refuse rather than heal. The emitter derives `Clone` and owns `sender:
  Option<UnboundedSender<ExecutionEvent>>` by value, installed once from
  `try_get_exec_event_sender()`, which reads a `thread_local!` in
  `nautilus_common::live::runner` set on the runner's thread. Every clone taken
  after that point freezes the sender state of the instant it was taken, and
  `send_order_event` on a sender-less clone only logs a warning.

  What the other side would have to ship: an emitter holding its sender behind a
  shared cell, or resolving it per send from a process-wide rather than
  thread-local slot, so a clone taken before `set_sender` still emits. Our
  workaround is a refusal, not a repair - a host that starts its clients on one
  thread and connects them on another gets a named error rather than a working
  client - and this item dies when nautilus ships a shareable emitter.

- **No channel for a declared feed gap.** Mogwai's `VenueMessage::FeedLagged`
  carries `skipped` and `sim_now_ns`, which is a strictly better signal than most
  real venues give, and the adapter has nowhere to put it. No `DataEvent` variant
  means "the stream you are aggregating has a hole", the client is handed to the
  host boxed as `dyn DataClient` so an adapter-owned counter or health accessor
  is unreachable, and `is_connected` is true throughout because the socket never
  broke. Bar aggregation over the missing span is silently wrong and the polling
  cursor resumes past it, so a strategy cannot distinguish a quiet market from a
  dropped one.

  The execution socket cannot self-heal either, and for a different reason.
  `ExecutionEvent::Report` exists, so the emitter is not the missing piece; what
  is missing is a truthful report to push. Every truthful set in this adapter
  comes from an asynchronous venue-truth query, and the frame translator that
  sees `FeedLagged` runs as `handler(msg).await` inside the reader's own frame
  loop, so a query issued there can only be read by the loop awaiting the
  handler - a deadlock by construction. Spawning it off is unavailable too, since
  the client owns `Rc<RefCell<Cache>>` through `ExecutionClientCore` and is
  `!Send`. Fabricating from the local mirror is the exact falsehood the
  venue-truth move removed, since the mirror is built from the frames the venue
  just said it dropped.

  What nautilus would have to ship, either half closing half of this: (1) a
  data-side degradation signal, a `DataEvent` variant or a `DataClient` health
  callback the engine surfaces, so a gap is an event rather than a log line; and
  (2) an execution-side client-initiated reconciliation request the
  ExecutionEngine services on the client's behalf, so the adapter can say "my
  mirror is suspect, re-run mass status" without owning an async handle to
  itself. Until they exist, a host driving mogwai should treat an error from
  `mogwai-adapter` mentioning a feed gap or a refused frame as a
  reconcile-and-distrust-the-window signal.

- **The Rust trait default for mass status does not compose** the way the Python
  base does. Queued in the maintainer's PR tracker. Not a substitute for our own
  reconciliation guard: mogwai overrides the method, so this protects the next
  adapter author rather than this repo.

- **Tape sparsity has no attribution channel.** An empty historical window is
  correct behaviour here - the fitted ACD arrival process is persistent and
  heavy-tailed, so a short window can legitimately hold zero trades and
  `/trades` correctly answers `200 []` - but it still costs the consumer a fatal
  halt, and one of the two fixes is blocked on the same gap as `FeedLagged`: an
  empty historical response carries no feed identity, so it cannot be attributed.

## Open at broadarrow

- **Owed: tell them, in one message. Nobody has.** Three breaking changes now,
  not two: the whole account surface moved under them, `OrderExpired` replaced
  `OrderCanceled` for expiries, and the divergence control plane changed request
  shape (`POST /control/divergence` now takes `{"kind": "<Tag>", "args":
  {<fields>}}` with `account` and `symbol` beside `kind`; unknown top-level
  fields are refused, so the old flattened body takes a `422` and arms nothing,
  and refusals and acks are JSON objects). Several entries below are stale in
  their favour.
  - The break: they set no `account_type`, inherit `MOGWAI-001`, POST no account,
    and have no handling for a run that ends by liquidation. Their orchestrator
    runs the shared shape, so 50 subagents inheriting `MOGWAI-001` would take
    each other's ledger in turn; the id belongs in their per-subagent account
    TOMLs. The account-id contract is in `docs/config.md`: a client on a shared
    venue names its own account, and a client that spawns its own ephemeral venue
    owes nothing. That break is designed, but designed-to-break only works if the
    other side is told it is coming.
  - The second break: an expired order now reports `VenueMessage::OrderExpired`
    with a terminal `Expired` status where it reported `OrderCanceled`. Exhaustive
    matching stops compiling; loose matching stops seeing `Day` and `Gtd` orders
    end at all, which is the dangerous reading and the reason this belongs in a
    message rather than a changelog nobody diffs.
  - In their favour, same message: trailing stops, the full order-type surface
    including `TrailingStopLimit`, order lists (so the two-independent-legs
    workaround is no longer required) and `RejectNextCancel` are all served, so
    their three unrun scenario files can now be written. `translate_trailing_exit`
    can emit the limit form as well as the market one; the venue derives the
    limit price from a `limit_offset`, so they send an offset and not a price.

- **Item 4 of the strategy-search route, consuming the multi-instrument venue.**
  `run_prep::mogwai_facts` refuses a `/instruments` answer of anything but
  exactly one instrument, precisely so a relaxed mogwai breaks their build loudly
  instead of having broadarrow pick an instrument arbitrarily. Closing it means
  selecting by the strategy's frontmatter `MOGWAI:<symbol>`, per worker rather
  than per venue, after which the readiness record's `symbol` field needs its
  one-venue-one-symbol meaning reconciled.

- `POST /accounts` at run-prep preflight, so each worker opens its own ledger
  with its own balances before the node is built. Nothing here blocks it.

- Their profile row becomes `AtomicOuo` and brick 3 of
  `notes/venue-order-list-oco-spec.md` lands. Note the carve-out they must read
  before citing the group-admission guarantee: a member whose funds an earlier
  member's fill consumed is rejected on the second pass with its earlier siblings
  already accepted, so admission is atomic for everything the venue can decide in
  advance and not for a balance the group's own fills moved.

- Decide whether a refusal marked `RETRYABLE_REJECT_PREFIX` should be treated as
  retryable at all. Their standing reasoning - a rejection wrongly treated as
  retryable is worse than a run that stops when the venue said no - is still
  sound, and the marker only changes what the decision rests on. Nothing here
  pushes them either way.

- Boot-storm pacing for concurrent `/trades` and `/quotes` warmup, because their
  daemon decides when workers spawn. Our bounded wait makes staggering an
  optimization for ordinary paging rather than a precondition of correctness,
  which is the change worth telling them about.

- Undecided and theirs: `submit_order_list` is the only route that emits a group
  frame, so a consumer wanting an atomic group by any other route has no API for
  it. None is owed until one is wanted.

- Their own repo, listed so the coordination is not lost: (a) the feed-stale
  message hard-codes the issue-4255 hypothesis ("the connection looks
  healthy...") as fact even when the venue process is dead; (b)
  `reference/mogwai.md` and `ba man mogwai` still describe the venue as
  unfundable, stale since the `[balances]` seed landed; (c) stored scenario TOMLs
  setting `transport_profile` on either adapter config no longer parse, since the
  field went with `TransportProfile` itself, and want a sweep.

### Runs owed against mogwai

Theirs to run, not ours to build, but each is a venue exercise that would surface
mogwai defects, and several have been owed for weeks.

- The restart run, the realized-PnL baseline, legs 1 to 3: serve durably, trade
  to a non-zero realized figure, SIGKILL the worker, re-run against the same
  `[attach]` scenario, verify the carried baseline, the brake mark, and no
  duplicate booking. Leg 3 is load-bearing and rests on a verdict reached by
  reading the dependency rather than by observing a reconciliation, landed as an
  explicit operator override of its own gate - a known-unrun verification on a
  capital bound.
- `go_live` restart de-duplication: kill a non-flat worker with orders resting at
  the durable venue, restart, verify the batch de-duplicates against the
  surviving book.
- The futures run against a `preset = "MNQ"` venue: warmup, fed fills, a resting
  stop triggering on the multiplied instrument, a settlement-currency commission
  actually charged, and the brakes marking in that currency.
- The conditional half of the fed-fill path: a fed fill from an order that
  genuinely rested and then filled at venue timing, ideally under havoc.
- Flip plus pyramid plus partial in one bar, end to end.
- Gate B, the anchored-warmup overlap drop. Their `handoff.rs` covers Binance,
  Kraken and Bybit but not mogwai, and is a consistency test rather than ground
  truth.
- The poll-heal end-to-end test, which drives our control plane directly: rest a
  far-from-market limit, POST `CancelOpenOrderSilently`, assert the local order
  converges to Canceled within the retry ladder's bound. Their fixture notes
  still hold: carry no protective exits, and census the whole rotated log family.

## Notes and gotchas

- The CLA check is not yet a required status check. cla-assistant.io is wired up
  and its webhook delivers, but nothing blocks an unsigned merge until a
  repository ruleset requires the check by name. The trap: an owner-authored PR
  produces no status at all, since the CLA assigns copyright to the owner and the
  bot correctly has nothing to ask, which means the check cannot be picked from
  the suggestion list and cannot be validated against a real run. Type the
  context name in by hand and leave the rule in evaluate mode until an outside PR
  confirms it, because a required check that never reports blocks every merge
  with no visible cause.
