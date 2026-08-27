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

`notes/bugs.md` and `notes/bugs-engine.md` were adjudicated entry by entry on
2026-08-26 and deleted. Everything that survived that pass is below; everything
that did not was either closed by a ruling, already answered at its own code
site, or was never work in the first place. This file is now the only backlog.

## How to read an entry here

Two rules earned the hard way during that adjudication, both of which cost
real owner attention before they were written down.

**Read the code site before the entry's prose.** Four entries were still asking
a question the code had already answered, in a doc comment at the exact site the
entry named: the launcher's Ctrl-C trade, the adapter's account-label mismatch,
`havoc.data` being refused rather than dropped, and the reconciliation test's
scope. An entry that names a symbol is a pointer to that symbol's own
documentation first and a claim second.

**An entry earns its place only if something in this tree could change to close
it.** A true observation about where our repository ends is not a defect. Three
entries were deleted for this: we cannot prove a third-party framework calls our
code, we cannot detect a widened conformance tolerance without new measurement,
and a lint cannot separate an assertion message from a wire payload. Each was
correct and none was work. Where such a limit is worth recording, it is recorded
at the site it constrains, not here.

## Priority order across the slates

Ruled by the owner 2026-08-26. Tape research v1 was rejected and v2 has not
started, so the tree currently holds one or two tapes, neither in a good
state - and that is irrelevant to the backend: mogwai must work like an
exchange, and everything below is implementable against the tapes we have.
The ranking is by how directly a slate serves the north-star claim, and by
what unblocks what.

1. **The order path** (the section below). Was the largest open correctness
   item: systematically optimistic fills made a forward test worse than
   useless because it reported success. Crossing has now landed - a market
   or marketable-limit order walks the opposing quoted touch and a
   parametric ladder rather than slipping the last print by a draw. What
   remains under this heading is the synthetic-book calibration that would
   make the ladder's constants real. The margin-refold question closed
   2026-08-26 as priced-and-acceptable - the measurement and the reasoning
   that binds any future index live in `reference/performance.md`, row
   27d8f088. No chart verdict is owed on the crossing itself, by owner
   ruling 2026-08-26: the standing chart gate covers tape generation, and
   the crossing moved no tape byte. The calibration landing does owe one,
   because its preset constants move generated quote bytes.
2. **The adapter and consumer surface**: a test pinning the `MarketToLimit`
   refusal makes that gap loud.

Excluded as tape-gated: the segment-sampler gate, the composed-river
checkpoint chain behind it, and the whole tape-research-v2 cluster. The
documentation and CLA items are small or owner-only; the architecture.md
headings step is filler between slates.

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

  Nothing further is built on the composed tape until a re-render passes.

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

- **A composed river has no checkpoint chain, so a distant seek is linear in
  ticks.** The residue of the composed-source work, and blocked behind the gate
  above rather than independent of it. `SegmentSource::seek_to` no longer caps
  the walk - a cap there turned distance into a latched terminal fault, which
  would have made a window on a composed river fail silently - so a far target is
  reachable and simply costs the whole walk. The composed level and the sampling
  draw are both path-dependent, so no segment can be skipped without composing a
  different river.

  `GeneratedSource` had the same shape and `CheckpointIndex` fixed it: snapshot
  every K ticks, resume from the snapshot before the target, replay the residual.
  The composer wants the same thing, and `Rivers::place_cursor` shows how a
  caller consumes it.

  Currently unreachable rather than fixed, and deliberately fenced.
  `crates/mogwai-venue/tests/composed_source_guard.rs` fails the build if any
  source under `mogwai-venue/src` names `SegmentSource` or the
  `mogwai_data::segment` module, and its message states what is owed before the
  wiring may land - a checkpoint chain for the composer, or a bounded placement -
  and says to delete the guard in the same change. Do not delete the guard
  without paying what its message asks. The type-level barrier is real but is not
  the rule: `CheckpointIndex` holds a `GeneratedSource` concretely, so
  generalizing it to a boxed `TickSource` would remove the guarantee without
  removing anything that reads as one.

## The order path

Crossing landed 2026-08-26: a market or marketable-limit order now walks the
opposing quoted touch and a parametric ladder instead of slipping the last
print by a volatility-scaled draw, per `TAPE_PROTOCOL_VERSION` 27. What
remains is what the mechanism exposed rather than what it was built to fix,
plus the calibration intake the spec deferred, now carried here since the
spec is deleted.

- **Calibration intake for the book constants.** Measure `quoted_width`,
  `top_sizes`, `depth_levels`, `depth_growth` and `trade_displacement_ticks`
  from real book data, landing as preset values with fitted provenance
  through the normal intake sequence - `mogwai-lab` gains whatever
  measurement the corpus format needs, and the method stays
  instrument-agnostic per the north star.

  Corpus state, established during implementation: the delivered MNQ corpus
  is TBBO only, which can fit spread, touch sizes and trade displacement but
  cannot honestly fit `depth_levels` or `depth_growth` - those need mbp-1,
  which the Databento account holds server-side, re-fetchable by job id at
  no new cost (see the intake entry under Tape research v2). No crypto book
  archive is on disk; the crypto trade archives fit the displacement half
  only.

  The landing rules, carried from the spec so they are not re-derived:
  committing changed preset artifacts moves generated quote bytes, so the
  landing owes the next unspent tape protocol identity - and being exact
  about which knobs owe it, `quoted_width`, `top_sizes` and
  `trade_displacement_ticks` are generator-read and move tape bytes, while
  `depth_levels` and `depth_growth` are read-time only and move none. The
  landing re-blesses every fill and tape golden it moves, re-runs the fit
  tolerances, and owes a rendered chart under the owner's eye - the standing
  tape-generation gate applies here because tape bytes move, unlike the
  crossing landing, which moved none.

- **The placeholder ladder displays eight size increments, which is a usability
  cliff on every preset without fitted `top_sizes`.** Depth is `top_sizes`
  scaled over `depth_levels` levels, and only MNQ has a fitted touch (3
  contracts, so 24 displayed). Every other instrument falls back to
  `TopOfBookSizes::uncalibrated(min_size)`, so BTCUSDT displays one satoshi at
  the touch and eight across the whole placeholder ladder. Any order larger than
  that partially fills and cancels the remainder for insufficient displayed
  depth - which is the crossing path working exactly as specified, against a
  book that is not the instrument's.

  Not a defect in the mechanism, and deliberately not worked around in it: the
  fix is the calibration brick, and until it lands the gates that cross a
  BTCUSDT book (`serving::a_market_submit_takes_a_reading_on_the_priceless_wire_path`
  and `scripts/smoke.py`) size their orders to the placeholder and say so at the
  call site. Recorded here so the next reader meets it as a known state rather
  than as a venue that refuses to fill anything.

## Tape research v2

Several parked items are waiting on one thing: what tape research v2 turns out
to be. Recording that here so each stops looking independently stalled.

The standing owner ruling that governs this whole cluster, because it keeps
being re-filed by fresh readers: **tape fidelity is not a prerequisite for
exchange machinery.** All the machinery of a real exchange can be built against
the tapes we have. Better tapes are gated on v2 regardless, so an unfitted
instrument class is not a finding and does not need re-reporting.

- **Nothing has been fitted for equity, perpetual or inverse.** A symbol
  configured as one is served the default tape wearing a different shape. The
  intake sequence is what makes a preset honest and none has been run. Note that
  MES is already a stated stopgap borrowing the MNQ fit, so this is not the only
  preset in borrowed clothes.

  The intake half, formerly a separate entry: candidate symbols for the missing
  session classes are a perp like ETHUSDT.P, a second CME future like MGC, and
  AAPL for cash-equity hours. Terabytes of DBN data are already downloaded on
  another host, and the Databento account holds about twelve months of MNQ, ES
  and MES tbbo plus mbp-1 server-side, re-fetchable by job id at no new cost.
  Whether BTC and ETH genuinely differ enough to warrant different values is
  unsettled - the measured 2.8x dispersion across three crypto majors suggests
  so, and one month of one venue cannot settle it. The evidence asymmetry stays
  relevant to preset authors: BTC, ETH and SOL have trade-level archives while
  MNQ and MES had 15-second bars and nothing else, so a CME preset's cadence is
  derived arithmetic and its clustering comes from nowhere. Re-derive the
  asymmetry from the DBN bulk download now on disk before repeating it.

- **The 86 MB and 57 MB build tax, and the dead protocol code.**
  `analysis/mnq-measure-12a.json` is 86,147,079 bytes and is `include_str!`d at
  six sites, three of them outside `cfg(test)`, so three copies are baked into
  the shipped binary. The three non-test sites are
  `mogwai-cli/src/ordered_counts.rs` (`run_with` and `run_with_rows`) and
  `mogwai-cli/src/count_curve.rs::reference`; the test-only three are in
  `mogwai-lab`'s `arrival_control.rs` and `arrival_screen.rs`.
  `analysis/mnq-arrival-screen.json` is 57,044,526 bytes and is parsed in full by
  `arrival_envelope_diagnostic.rs`'s
  `committed_screen_selects_the_twenty_a3_only_failures`, which is not ignored,
  so every `brokkr check` reads it.

  Both are terminal outputs of the closed 12b protocol. They cannot be removed
  without deciding the larger question they sit inside: roughly 25,000 lines
  across `mogwai-lab` and `mogwai-cli` are the compiled machinery of the closed
  arc (the arrival screen, control and envelope family, `measure12a`,
  `aggregate`, `stage_m` and its Tier 2 limb, `count_curve`, `ordered_counts`,
  `slow_geometry`, `tick_composition`, `select_windows`), and the binary still
  advertises them as supported subcommands.

  Owner call, deferred until v2's shape is known, since a successor may want some
  of the corpus-side machinery. Do not delete without a ruling. A split into a
  runtime read rather than `include_str!` was proposed and declined on the same
  grounds: it is a separate decision from the one being deferred, and taking it
  now prejudges nothing usefully.

- **The absolute-rate conflict, unverified.** The claim that the shipped arrival
  path carries a 5.5 to 7.0 percent absolute-rate conflict against the observed
  July month. It exists only as prose in `notes/tape-research-v1.md`; no test or
  artifact in the tree bears on it, `ARRIVAL_MEAN_CAL` is unchanged, and a
  Jensen-gap explanation was refuted in closed form. It also cannot name the
  decision a measurement would change, which is the standing bar for running one.
  Very likely moot under v2. Treat as unverified prose, not an established
  defect - it has been read as the latter more than once.

  The other two thirds of the original finding are closed and should not be
  re-filed from the old note: the `children_mean` clamp is repaired with the fix
  documented at the branch in `begin_event`, and the `ARRIVAL_MEAN_CAL` leak onto
  the integrated frame is gated by
  `the_arrival_mean_calibration_stays_off_the_integrated_frame`, with
  `GeneratedSource::active_mean_s` giving the calibrated side an observable so
  both halves are stated as exactly as each other.

- **Numerical stability in `AutoCorr`.**
  `crates/mogwai-lab/src/characterize/mod.rs`. Its `acf()` guards zero variance
  with `if var <= 0.0`, which catches zero and any negative residue but not a
  positive one, so a series constant at an irrational value (the fixture case is
  `abs(log return)` constant at ln2) can leave a tiny positive residue from
  `sumsq / n - mean * mean`, slip the guard, and return an ACF that came out of
  catastrophic cancellation rather than measurement. Both branches substitute a
  number where the honest answer is that the quantity is undefined for a constant
  series.

  Deliberately not fixed. `AutoCorr` also computes the F1 duration ACFs and is
  bit-exact against `analysis/cadence.json` (`duration_acf_lag1`
  0.32204142581620676, `duration_acf_lag5` 0.22388204486699373), the lineage the
  fingerprint's cadence half rests on. A fix returns an explicit unavailable
  rather than zeros, uses a relative rather than absolute variance floor, and
  probably Welford or two-pass accumulation - all of which move numbers, so the
  real work is the cadence-impact analysis and possibly a refit. Since a refit is
  exactly what v2 does anyway, fixing it there costs nothing extra; fixing it now
  buys a refit for a case no real corpus produces.

- **Whether the protocol-12b Stage A refinement pass should run at all.**
  Deferred by the owner 2026-08-09 rather than settled, so the frozen pass stands
  and the budgets were raised to fund it.

  For cutting: refinement is 29,200 s of the 35,526 s Stage A cost model, 82
  percent, and its entire product is a finer loss ordering over cells that Stage
  B then truncates to `STAGE_B_CELL_CAP = 24` per family. It cannot rescue a
  family whose coarse admissible region is empty, because it subdivides around
  that region's own boundary cells. And `SELECTION_INDIFFERENCE = 0.01` already
  declares losses inside that margin as not separating candidates, so a
  half-spacing lattice buys precision the selection is defined not to use.
  Cutting drops Stage A to about 6,326 s. Against: the selected point would sit
  on the coarse lattice, and nobody has shown the coarse spacing is fine enough
  for the mechanism to be found at all.

  Not the same question as `STAGE_B_CELL_CAP`, which earns its place: a Stage B
  cell is a full month-scale walk per seed at about 250 s, so an uncapped
  1,508-cell region genuinely is tens of hours. Changing `REFINEMENT_DEPTH` or
  `REFINEMENT_CELL_CAP` is a section 17 amendment against the contract of record.

- **The protocol-12b section 5.5 rescale contradicts the shipped preset
  convention.** That section freezes the negative control's re-centring as
  "rescale the 24 values to sum to 1, which the `SessionProfile` schema
  requires", and the schema requires no such thing: nothing in `config.rs` or
  `session.rs` enforces sum-to-one, and the shipped MNQ `intensity_hour` sums to
  23.862306, a mean-one curve. It moves no generated rate either way, since
  `SessionModulator::new` divides by an exposure-weighted normalizer so a common
  factor cancels at every instant. What it cost is readability, and it will cost
  the same again at any later reader comparing the two curves elementwise. Fixing
  the frozen sentence is a section 17 amendment through review, not an edit.

## Venue and protocol

- **One residual divergence between the boat implementation and the glossary's
  Boat entry.** The two concrete divergences the cold review raised landed on
  2026-08-27: the owner discriminator left the boat key, so identical named
  placements share one hull across accounts, and the window end moved to the
  passenger - the hull runs unbounded and each passenger's writer cuts its own
  delivery at its own `window_end_ns`, exactly as a duration works. What
  remains is the literal reading of "passengers asking for the same river and
  the same speed share one boat": named windows at different starts still
  place separate hulls, because the boat carries one `SimClock` anchored at
  its placement start. Satisfying the sentence outright means a hull that
  caches and publishes river data while each passenger owns its clock
  projection, delivery frontier, history cutoff and completion - a larger
  clock/view refactor, sketched in the 2026-08-27 spar, and not urgent by the
  2026-08-26 ruling. The analysis behind the window itself is settled: a named
  window is a placement and not a river, provably, because the window never
  enters `RiverKey`.

## Engine

- A zero-price fill is still warned about and booked by `warn_zero_px`, so a
  position can carry `mark_px == 0` if the tape produces one.
  `position_unrealized_checked`'s zero answer is the backstop for exactly that
  case. Refusing at the fill was considered and rejected in the 2026-08-20
  ruling, because by then the tape has already produced the print and aborting
  the serving path over it is the one thing no venue does. Open only as a
  known-covered case.

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
  at `SubmitPhase::PreStamp` - the refusal lands locally as an `OrderDenied`
  before any event is emitted. The workaround is host-side: set `price` on the
  `OrderInitialized` by hand before `SubmitOrder::new`, which for this one type
  is the contract rather than a defect (documented in
  `docs/adapter-lifecycle.md`). Closing it properly is a cross-repository
  question: either nautilus grows a stated-limit form of the type, or the
  adapter would need a limit it cannot invent - it has no reading to price one
  from, and guessing would name the number the venue exists to own.

  The pin this entry asked for already exists, and the ask is withdrawn
  2026-08-27. `unsupported_init_shapes_are_refused_before_submitted` in
  `adapter_smoke.rs` builds a market-to-limit exactly as nautilus' factory does
  - order type set, price and both trigger fields cleared - submits it, and
  requires the error to name both "malformed for MOGWAI" and "MarketToLimit
  order must carry a price", then asserts no execution event was emitted, which
  is the before-any-event half of the claim. It landed in `e908ee1` on
  2026-08-26, and the entry was not updated to notice.

  Worth knowing why it stayed invisible to a reader who looked: `adapter_smoke`
  is one of the four socket-backed binaries that plain `brokkr check` does not
  run, so the pin is real but only executes under `brokkr check --gate`. A
  search of test names would have found it; a green check never mentions it.

  What remains open is only the cross-repository question above, which no test
  in this tree can close.

- `HavocSpec.data` was resolved and needs no entry: `config.rs`'s
  `validate_havoc` refuses the field outright with a named error telling the
  operator to use the offline `gen` command or configure the venue's river, and a
  test pins the refusal text. Recorded here only because the "may be arming a
  field nothing consumes" claim outlived its fix twice.

## Tests and tooling

- **Two timing sites remain deliberately blocked.** `serving.rs`'s
  market-reading gate spaces attempts 500 ms apart, and that spacing is the
  assertion's measured flake margin. `data_client_transport.rs`'s segmented-head
  test waits 20 ms for the reader to consume its first segment, and no seam
  exposes that condition. Neither duration can move without changing the test or
  adding the missing seam.

- **A budget-carrying test cannot be routed into the `timing` sweep
  automatically, and the fix is a brokkr-level feature.** `brokkr.toml` states
  the policy - a latency assertion is `#[ignore]`d at the source, listed in the
  gate's `skip`, and named in the `timing` sweep's `only` - and the tool enforces
  it in one direction: an `only` entry the gate does not skip is an orphaned
  pair, and a filter matching nothing is dead. The converse, that every
  budget-carrying test appears in some `only`, is not a syntactic property, so a
  plain `#[test]` asserting 50 ms in the parallel dev lane is admitted silently.

  The crude mechanization was checked rather than assumed away and does not work:
  a scan for `Duration::from_` inside an `assert!` would flag the eighteen poll
  intervals and six negative-observation windows above, so it would fire on
  twenty-four correct sites to catch one wrong one, get suppressed on its first
  run, and then mean nothing.

  What would change it is a marker at the source rather than a scan of it: a
  budget-carrying test declaring itself, so the tool has something to enumerate
  instead of something to guess. That is a brokkr feature and a new convention;
  mogwai's side is only adopting it once brokkr can enumerate it.

## Documentation

- **`reference/architecture.md` wants splitting; step one is done.** The entry
  used to open "about 1,400 lines with two headings" and that is stale,
  corrected 2026-08-27: the headings landed in `0ab8c4a` on 2026-08-26, and the
  file is now about 1,700 lines under roughly forty of them. It still does the
  same four jobs - the venue's runtime shape and account model; accounts, risk,
  instruments and valuation; clocks, boats, delivery and faults; the generator
  and fingerprint - plus a workspace section at the end, and its contradictions
  have all sat where one job's old text survived another's landing.
  `docs/havoc.md` was patched rather than rewritten and wants the same treatment
  eventually.

  What remains is step two: split into separate files with the boundaries
  visible. The seams the split needs now exist, which was the whole point of
  doing it in two steps.

  The blast radius for step two: 17 files cite `architecture.md`, and two of them
  are prose-scanning tests (`live_fact_prose.rs`, `tape_version_prose.rs`) that
  read every markdown file in the repository. The failure mode of step two is
  silently dropping a paragraph out of a durable must-be-true document while
  moving text, which nobody would notice.

## Infrastructure

- **The CLA check is not yet a required status check.** cla-assistant.io is wired
  up and its webhook delivers, but nothing blocks an unsigned merge until a
  repository ruleset requires the check by name.

  The trap: an owner-authored PR produces no status at all, since the CLA assigns
  copyright to the owner and the bot correctly has nothing to ask, which means
  the check cannot be picked from the suggestion list and cannot be validated
  against a real run. Type the context name in by hand and leave the rule in
  evaluate mode until an outside PR confirms it, because a required check that
  never reports blocks every merge with no visible cause.

  Not a code change - repository settings, owner-only.

---

# Owed by other repositories

Nothing below can be fixed from this tree. Kept so the ledger is complete.

## Wanted upstream nautilus_trader PRs

Read the source from `research/nautilus_trader`; build against the pinned
crates.io release. Each of these names what the other side would have to ship,
which is what makes it a writable patch rather than a grievance.

All five were re-verified 2026-08-27 against the pin rather than against
memory. That was worth doing for a reason this section should keep in view:
the checkout used to sit on `develop`, which is neither what we link nor what
this section claims to describe, and a maintainer may reshape or reject what we
file, so an entry here can go stale without anyone touching this repository.
Four still stand. One had closed and is struck below.

- **`ExecutionEventEmitter` cannot share its sender**, so this adapter can only
  refuse rather than heal. The emitter derives `Clone` and owns
  `sender: Option<UnboundedSender<ExecutionEvent>>` by value, installed once from
  `try_get_exec_event_sender()`, which reads a `thread_local!` in
  `nautilus_common::live::runner` set on the runner's thread. Every clone taken
  after that point freezes the sender state of the instant it was taken, and
  `send_order_event` on a sender-less clone only logs a warning. Our workaround
  is a refusal, not a repair: a host that starts its clients on one thread and
  connects them on another gets a named error rather than a working client.
  The PR: an emitter holding its sender behind a shared cell, or resolving it per
  send from a process-wide rather than thread-local slot, so a clone taken before
  `set_sender` still emits.

  Still open at the pin: `live/src/execution/emitter.rs` holds
  `sender: Option<UnboundedSender<ExecutionEvent>>` by value on a `Clone` type,
  unchanged. Upstream now documents the intended pattern - "call `set_sender` in
  the adapter's `start()`" - which is guidance rather than a fix, since a clone
  taken before that call still freezes with no sender.

- **No channel for a declared feed gap.** `VenueMessage::FeedLagged` carries
  `skipped` and `sim_now_ns` and the adapter has nowhere to put it. No
  `DataEvent` variant means "the stream you are aggregating has a hole", the
  client is handed to the host boxed as `dyn DataClient` so an adapter-owned
  counter or health accessor is unreachable, and `is_connected` is true
  throughout because the socket never broke. So bar aggregation over the
  missing span is silently wrong and the polling cursor resumes past it, and a
  strategy cannot distinguish a quiet market from a dropped one. Fabricating a
  report from the local mirror is not the escape: the mirror is built from the
  frames the venue just said it dropped. The execution socket cannot self-heal
  either: the frame translator that sees `FeedLagged` runs as `handler(msg).await`
  inside the reader's own frame loop, so a venue-truth query issued there
  deadlocks by construction, and the client is `!Send` so spawning it off is
  unavailable. The PR: a data-side degradation signal and a client-initiated
  reconciliation request. Until then, a host driving mogwai should treat an
  error from `mogwai-adapter` mentioning a feed gap or a refused frame as a
  reconcile-and-distrust-the-window signal.

  Still open at the pin: `DataEvent` carries `Response`, `Data`, `Instrument`,
  `FundingRate`, `InstrumentStatus`, `OptionGreeks` and a `defi`-gated variant,
  and none of them means a hole in the stream - the enumeration in
  `client/data.rs`'s gap comment matches the pinned source exactly. There is a
  new `SystemEvent::SocketState` beside it, and it does not help: it reports the
  socket changing state, and this gap happens while the socket never breaks.

- **No registration signal at the account cache insertion boundary.**
  `await_account_registered` polls every 10 ms until nautilus's runner has
  consumed the forwarded account event and inserted the row, with a 5 s wall
  bound. The pinned cache exposes no registration notification, and notifying
  when the adapter forwards the event would be too early, because forwarding only
  queues it. The PR: a signal at the cache insertion boundary. No adapter-side
  latch can substitute.

  Still open at the pin: `Cache::add_account` writes the database, inserts into
  `accounts` and indexes `venue_account`, then returns. There is no notify, no
  watch and no subscriber hook on that path, so a waiter has nothing to sleep on.

  The connection half of this is already closed and should not be re-filed:
  `wait_connected` sleeps on an adapter-owned notification with a 250 ms backstop
  re-read, and that backstop is not a leftover poll - bite-checking the
  notification by deleting `notify_waiters` hung every socket test for the full
  dial timeout rather than failing on anything that named the cause, so a latch
  with one publisher and no fallback was trading five hundred cheap wakeups for a
  wedge.

- ~~**The Rust trait default for mass status does not compose** the way the
  Python base does.~~ **Closed upstream, verified at the pin 2026-08-27.**
  `ExecutionClient::generate_mass_status` in `common/src/clients/execution.rs`
  now carries a default that builds the three granular commands and composes
  their generators under `futures::try_join!`, with tests pinning both the
  composition and the error propagation. It ships with a stated caveat rather
  than silently: the default reads the realtime atomic clock, so a client on a
  mocked or backtest clock must still override to compose with its own. That
  does not reach us - mogwai overrides the method anyway, which is why this
  entry always said it protects the next adapter author rather than this repo.
  Nothing to file, nothing to wait for.

- **Tape sparsity has no attribution channel.** An empty historical window is
  correct behaviour here - the fitted ACD arrival process is persistent and
  heavy-tailed, so a short window can legitimately hold zero trades and `/trades`
  correctly answers `200 []` - but it still costs the consumer a fatal halt, and
  one of the two fixes is blocked on the same gap as `FeedLagged`: an empty
  historical response carries no feed identity, so it cannot be attributed.

## Owed by us to broadarrow: one message, unsent

The framing here was wrong and is corrected 2026-08-27. "Nobody has written it"
was false: broadarrow's `reference/mogwai.md` tracks this repository's landings
by commit and date, and spot-checking it found `OrderExpired`, the USD equity
default with its `[balances]` currency edge, and the per-account divergence
scope all already recorded there - the last one dated and cited to `50e5c2d`.
They consume this tree continuously through the synced snapshot and through
direct exchanges, so most of what accumulated here as undelivered news had in
fact been delivered, and the section was bookkeeping debt rather than a backlog.

Worse, an entry was not merely stale but false - the paired adapter configs,
below, announce a breaking change that does not exist in this tree. So the
standing rule for this section: an entry is verified against the code before it
is sent, not just written down when a change is planned. A consumer restructuring
a call site against a boundary we never built pays for our bookkeeping, and this
list is read as authoritative precisely because it is written by the side that
made the changes.

What remains genuinely undelivered, per the same check, is small. Their doc has
no trace of the divergence request envelope, which the entry below now explains
does not bite them anyway.

One caveat on the method, which they supplied and which weakens some of the
conclusions below. Several entries are marked delivered on the evidence that
their `reference/mogwai.md` already records the fact. That doc is cited from
their code the way ours is, but they audited it in the same pass and found
three stale entries, one of which had inverted a capability outright. So "it is
in their doc" establishes that somebody wrote it down, not that it is currently
true there. Where an entry below rests on their prose rather than on our code,
the delivery claim is only as good as the paragraph it read, and the paragraphs
read for this audit were detailed and current rather than skeletal - which is
weak evidence and should be treated as such. A compiler probe settles a public
surface; nothing settles a semantic claim except one of us re-reading the code.

- **The default shape moved to USD cash equity, 2026-08-26.** An unconfigured
  venue now resolves every unmatched symbol through a new NVDA preset - cash
  equity, USD-settled - and the default funding is 1,000,000 USD, not USDT.
  The sharp edge: BTCUSDT is funding-barred out of the box, since it settles
  USDT and the default account holds none. Any of their scenarios that spin a
  transient venue with no `[balances]` and trade BTCUSDT will be refused at
  bind as unfunded; the fix on their side is one `[balances]` line.

  Verified landed: `DEFAULT_PRESET` is `NVDA` and the default balance table is
  a single 1,000,000 USD entry. But the rider this entry used to carry - that
  their `reference/mogwai.md` prose was "doubly stale" - was false and is
  withdrawn. Their doc already describes these semantics precisely, including
  that boot validates only the shapes the config names, that an unfundable
  currency is merely recorded for the shipped presets so a BTCUSDT-only
  operator is not barred over an unfunded USD, and that the refusal lands at
  bind or at the history poll naming the currency to add. They meet it at the
  materializing poll and fail worker boot with our own text. Nothing here is
  news to them.
- **Market-taking fills cross a book now, 2026-08-26.** Slippage is the
  arithmetic consequence of walking displayed depth, not a draw: fills land
  at or beyond the quoted touch, never inside the spread, and a market order
  bigger than displayed depth partially fills and cancels the remainder with
  reason "insufficient displayed depth". A scenario asserting exact fill
  prices or assuming a market order always fills whole will read differently.
  What differs between presets is the touch size and not the ladder, and the
  earlier wording here had that wrong: the ladder is a placeholder everywhere.
  MNQ declares eight levels with flat growth, both stamped `uncalibrated` in
  its own preset, over a fitted touch of 3 by 3. Every other shape takes the
  default ladder over an uncalibrated touch of one size increment. So MNQ
  displays more depth than the rest, but no preset displays a calibrated book,
  and "fund MNQ" is a way to get a bigger placeholder rather than a real one.
  Until the calibration lands, size market orders to the touch on any preset.
- **Policed accounts are refused at boarding for unreachable currency,
  2026-08-26.** An account policed in currency X may bind only shapes
  settling in X; the bind is refused as a 400 with a named reason before the
  socket upgrade, not at order time. Their runs use unpoliced defaults today,
  so this bites only when they adopt policy presets - but the refusal text is
  new surface their error handling will meet first.
- **The account surface moved - and the collision warning in this entry was
  already wrong, corrected 2026-08-27.** It claimed they inherit `MOGWAI-001`,
  so fifty subagents would take each other's ledger in turn. They do not. Their
  `reference/mogwai.md` states that both client configs carry the worker's own
  account id and that broadarrow deliberately does not let the adapter default
  through, precisely because our default validates cleanly now and so no longer
  fails loudly for a consumer that forgot. They also track that the id selects a
  ledger on the wire as of 2026-08-18, and that it is the third thing both legs
  must agree on beside the run seed and the symbol. The warning was aimed at a
  hazard they had already closed, and by their own account they closed it
  before we wrote it down.

  What still stands: they set no `account_type` - the exec config leaves it at
  `Cash`, and a futures run wants otherwise - they POST no account, which they
  confirm and own as their work, and they have no handling for a run that ends
  by liquidation. The account-id contract is in `docs/config.md`.
- **`OrderExpired` replaced `OrderCanceled` for expiries**, with a terminal
  `Expired` status. Verified landed here - the frame and the `Expired` status
  both exist in `mogwai-protocol`. Already delivered: their `notes/todo.md`
  records it as a distinct wire frame, deliberately not a flagged
  `OrderCanceled`, and correctly notes they have no exposure because broadarrow
  places good-till-cancel only, so it goes live the day anything of theirs
  emits a `Day` order. They also named the dangerous reading themselves - an
  expiry read as a cancel is a resting order the bridge believes it still has.
  Nothing owed.
- **`POST /control/divergence` changed request shape - and this is not a break
  for them, corrected 2026-08-27.** The entry below stands as a wire fact and
  was wrong about who it bites. `ship_venue_havoc` in the adapter's exec client
  is the sole carrier: it serializes the `Divergence`, lifts the `type` tag out
  and rebuilds the `kind` and `args` envelope itself, so a consumer handing a
  `HavocSpec` to the client config never constructs that body and cannot send
  a stale one. broadarrow is exactly that consumer. What the change bites is a
  hand-written caller, which for them is one unwritten owed run - the poll-heal
  repro, which POSTs a `CancelOpenOrderSilently` by hand and will need the new
  shape from its first line, including the JSON `error` and `status` bodies in
  place of bare text, and the ack's shed `evicted` divergence, which is the
  thing a heal assertion actually wants to read.

  Also worth sending, because they reasoned from it: the `Divergence` enum
  still being `serde(tag = "type")` in the synced snapshot is not evidence that
  the snapshot is behind. That tag is the internal Rust representation and it
  did not change; the envelope is built at the adapter's HTTP boundary from it.
  They inferred a stale sibling from a tag that was never going to move.

  What they must send when hand-writing one:
  is `{"kind": "<Tag>", "args": {<the tag's fields>}}`, with the optional
  `account` and `symbol` staying at the top level beside `kind`. Unknown
  top-level fields are refused rather than ignored, so the old
  `{"type": ..., <fields>}` body takes a `422` and arms nothing - it does not
  degrade quietly, and a scenario that posted it would run believing a fault was
  armed. Refusals and acks are JSON objects now too: a refusal is
  `{"error": "<reason>"}` and an ack is `{"status": "accepted"}`, carrying
  `detail` and the shed `evicted` divergence when an arm evicted one, where both
  used to be a bare text body. Their poll-heal end-to-end test drives this plane
  directly, so it is the run most likely to notice.
- **The adapter's data and execution configs are constructed as a validated
  pair - not landed, and this entry was false, corrected 2026-08-27.** There is
  no pair type in `mogwai-adapter/src/config.rs`, no paired constructor, and
  `factories.rs` still downcasts `MogwaiDataClientConfig` and
  `MogwaiExecClientConfig` independently. The entry announced a breaking change
  that does not exist, which is worse than announcing nothing: it invites a
  consumer to restructure a call site against a boundary that is not there.
  broadarrow reported the two configs had not reached them and were right;
  their `applied_data_config` and `applied_exec_config` in
  `run-prep/src/venue.rs` are the shape that would break, and they will break
  when it lands, which they have accepted. Keep it as owed work, never as
  delivered news, until a pair type actually exists.

  The reason it was catchable at all: their crates take `mogwai-adapter` and
  `mogwai-protocol` as unconditional path dependencies on this working tree, so
  their `brokkr check` compiles against whatever is on disk here. That makes
  their green build a live probe of our public surface, and it is why they
  could say with confidence that the pairing had not landed.

  The probe runs both ways, and they have offered it explicitly: we may ask
  them to run a `brokkr check` whenever we want to know whether a change of
  ours has actually landed downstream. That is a compiler answering the
  question instead of two documents agreeing with each other, and it is the
  cheapest verification either side has. It sees exactly what a compiler sees -
  a moved public surface - and nothing about semantics that still compile,
  which is the half that needs prose.

- **"Unattributed means everyone" is a declared class now, not a fallthrough**,
  and their note asking us to press for it is stale on both halves. Their entry
  reads the residual `b28fee8` recorded and asks for the declared class plus
  `handle_account_state` re-checking the wire id. The first landed:
  `mogwai_venue::run::audience` is an exhaustive match over `VenueMessage` with
  no catch-all, and every frame resolves to a named arm - `Venue`,
  `Account`, `Order` or `Requester` - each documenting why it routes where it
  does. The next ledger-owned frame is a compile error rather than a silent
  broadcast, which is exactly the loudness their note wanted. Their hunter's
  second half was right too, and it is now closed rather than merely conceded:
  the declared class was still broadcasting, which the glossary's Passenger
  entry and `north-star.md` both forbid, so by owner ruling on 2026-08-27 the
  `Unattributable` arm was collapsed into `Requester` outright rather than
  repointed. The two differed only in what they did on a path neither belongs
  on, and one arm removes that question instead of answering it. A submit
  rejection and an id-less modify or cancel rejection are the asker's, and
  swept delivery drops one loudly if a producer ever puts it there. If a
  variant ever turns up genuinely owned by nobody and genuinely owed to all, it
  gets an arm named `Everyone` - the old name described what the venue failed
  to know rather than who the frame was for, which is why it read as a
  fallthrough. No glossary entry, by the same ruling: this is internal delivery
  taxonomy, not venue vocabulary. They can retire the finding whole.
  The second is settled the other way, deliberately: `handle_account_state`
  does not compare the wire id, because a socket names exactly one account on
  its `/ws?account=` upgrade and only that ledger's state comes down it, so a
  dropped snapshot can only lose state that was correct - the drop is what the
  earlier defect was. The configured id is stamped on, and
  `note_account_label` says once at connect when the two names differ.
  `reference/architecture.md` carries the argument, including what would have
  to change first if a socket ever carried several ledgers. Their reply named
  themselves a stakeholder in that invariant - their percent and cash sizers
  size real orders off the `AccountState` balance, so a relaxation turns the
  absent re-check into their capital path rather than a style question. That
  is now recorded in `reference/architecture.md` beside the invariant, with
  the re-check named as owed in the same change as any relaxation.
- **The warmup boot storm is already solved venue-side, and they should not
  build daemon pacing for correctness.** Their question was whether the gate is
  cheaper from our side; it was built here on 2026-08-25. Four synthesis slots
  bound resident memory at the measured ceiling, and behind them sits a
  128-deep queue with a 30-second bounded wait, so the fifth caller is served
  late rather than refused. A `503` is reachable only past 128 concurrently
  queued history requests or a caller that queued and lost the whole deadline,
  and both carry `Retry-After` and distinct bodies naming which happened. Fifty
  workers paging sequentially never reach either. Their point about the
  refusal being invisible is right and sharpens the design rather than
  changing it: nautilus' historical response types carry no error channel, so
  a refusal arrives at their strategy as an empty page, which is why the wait
  exists at all. Pacing spawns stays a throughput optimization for them, not a
  precondition.
- **Their reading of `account_ttl_ms` is exactly right**, and the sweeper's own
  comment states it in the same words: an unattended account is frozen - orders
  do not rest, positions do not mark, funding does not accrue, and a policy
  cannot liquidate somebody who is not there. It is a deliberate departure from
  a real venue. The stated consequence is theirs verbatim: a run spanning a
  disconnect has a gap in its risk history. Collection at the TTL leaves a
  clean ledger a boot adoption reads as a flat venue, which is why the setting
  is published on the readiness record - a restart slower than the TTL can
  assert on the fact rather than discover it. The default is `0`, meaning never
  collect, and that default is what a consumer restarting a worker wants.

  A direction caution was sent with this and then withdrawn, and the withdrawal
  is the part worth keeping. `north-star.md`'s "fire and forget: no restart, no
  resume" reads at first glance as excluding their restart runs. It does not.
  It scopes the venue resuming its own run after its own exit - a path is
  reproduced by a fresh instance with the same seed, never by resumption - and
  the same sentence's "or the same named window on a shared exchange", plus
  server mode's durable one-exchange-per-batch shape, contemplate exactly the
  topology they described. The glossary settles it outright: eviction hands the
  account over precisely so "a killed worker come back to its own book", and
  Freeze exists to hold that book "until a passenger returns". A client
  reconnecting to a still-running venue is designed for, not tolerated. Their
  three requirements are all met today.

  Three caveats belong with that confirmation, none of which changes the
  answer. `reset_account_on_reconnect` must stay `false`, its default, or the
  returning socket gets a fresh ledger from `[balances]`. Retirement on return
  is per-river: a frozen account that returns keeps what its returning
  passenger's river holds and loses the rest, which is invisible for their
  single-instrument workers and bites an account carrying strategies on several
  symbols. And their restart relies on eviction by a fresh callsign - the
  adapter mints one per process - which works and is deliberate, but means the
  venue cannot tell their restarted worker from a stranger, so the standing
  rule against redialling on eviction is load-bearing for the farm.

In their favour, same message: trailing stops, the full order-type surface
including `TrailingStopLimit`, order lists and `RejectNextCancel` are all served,
so their three unrun scenario files can now be written. `translate_trailing_exit`
can emit the limit form as well as the market one; the venue derives the limit
price from a `limit_offset`, so they send an offset and not a price.

## Open at broadarrow

- Item 4 of the strategy-search route, consuming the multi-instrument venue.
  `run_prep::mogwai_facts` refuses a `/instruments` answer of anything but exactly
  one instrument, precisely so a relaxed mogwai breaks their build loudly instead
  of having broadarrow pick an instrument arbitrarily. Closing it means selecting
  by the strategy's frontmatter `MOGWAI:<symbol>`, per worker rather than per
  venue, after which the readiness record's `symbol` field needs its
  one-venue-one-symbol meaning reconciled.
- `POST /accounts` at run-prep preflight, so each worker opens its own ledger with
  its own balances before the node is built. Nothing here blocks it.
- Their profile row becomes `AtomicOuo` and brick 3 of
  `notes/venue-order-list-oco-spec.md` lands. Carve-out they must read before
  citing the group-admission guarantee: a member whose funds an earlier member's
  fill consumed is rejected on the second pass with its earlier siblings already
  accepted.
- Whether a refusal marked `RETRYABLE_REJECT_PREFIX` should be treated as
  retryable at all. Their standing reasoning - a rejection wrongly treated as
  retryable is worse than a run that stops when the venue said no - is still
  sound, and the marker only changes what the decision rests on. Nothing here
  pushes them either way.
- Boot-storm pacing for concurrent `/trades` and `/quotes` warmup, because their
  daemon decides when workers spawn. Our bounded wait makes staggering an
  optimization for ordinary paging rather than a precondition of correctness,
  which is the change worth telling them about.
- `submit_order_list` is the only route that emits a group frame, so a consumer
  wanting an atomic group by any other route has no API for it. None is owed
  until one is wanted.
- Feature 3, venue-enforced account policies: blocked on them, not on us, and
  they have said so. They never `POST /accounts`, so every account they seat is
  auto-created unpoliced and enforced against nothing; closing it needs an
  account-file policy knob and a settled `409` story for restarts, both theirs.
  Nothing is held here for it. What is owed from us when it lands is the breach
  field on `GET /account`, so their classifier can name a flatten as the stated
  rule rather than as an unexplained venue move.
- Equity and inverse refusals are their translate layer's decision, not a gap in
  our landing, and the same for the leverage refusal - leverage is per-symbol
  venue config here rather than a client-settable knob, so the trigger to retire
  their refusal is client-settable leverage appearing, not our margin table.
  Nothing to build here.
- Trailing-exit parity against piners' shadow, unexamined on their side. Our
  trail ratchets off the tape's span extremes rather than a sweep mark, so it is
  tighter than a mark-based trail and can trigger where one would not; a shadow
  trailing off marks diverges on exactly the spikes, and it surfaces as a parity
  mismatch rather than an error. `docs/oms-types.md` now states the basis rather
  than leaving it to be inferred from "the extreme the tape has reached".
- Their own repo: the feed-stale message hard-codes the issue-4255 hypothesis
  ("the connection looks healthy...") as fact even when the venue process is
  dead; `reference/mogwai.md` and `ba man mogwai` still describe the venue as
  unfundable, stale since the `[balances]` seed landed; stored scenario TOMLs
  setting `transport_profile` on either adapter config no longer parse, since the
  field went with `TransportProfile` itself, and want a sweep.

## Runs owed against mogwai

Theirs to run, not ours to build, but each is a venue exercise that would surface
mogwai defects, and several have been owed for weeks.

- The restart run, the realized-PnL baseline, legs 1 to 3: serve durably, trade to
  a non-zero realized figure, SIGKILL the worker, re-run against the same
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
  converges to Canceled within the retry ladder's bound. Their fixture notes still
  hold: carry no protective exits, and census the whole rotated log family.
