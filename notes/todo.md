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
than kept in parallel.

That sweep is finished as of 2026-08-26. Every section was compared entry by
entry against bugs.md; nine were full duplicates and are gone, four were mixed
and keep only the entries the extraction never took, and the handful of clauses
a copy here carried that bugs.md did not - the shared-exchange provenance behind
C13, the upstream emitter's remedy, and five broadarrow items whose closing path
had been summarized away - were moved into bugs.md before the copy was deleted.
What remains below is what bugs.md does not carry. Anything added here later
that also belongs there is a new drift, not a survivor of the extraction.

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

## Venue and protocol

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

## Data and generator

- Generator havoc must fork the river. The tape machinery deliberately mutates a
  canonical boatless river instead, with the pinned control-boundary snapshot,
  the coarsen exemption and the walk-back floor built to make non-forking
  correct, so the gap has known size and known work to undo. The seated-boat
  refusal standing in for the fork names a remedy no route exposes, and its gate
  reads boat presence in the non-awaiting form, so it is vacuous against a
  concurrent board.

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

- `HavocSpec.data` (`Option<MarketRegime>`) appears to have no reader on the
  adapter side now that the `Subscribe` carrier is retired, so an operator
  setting `[havoc.data]` may be arming a field nothing consumes - the
  looks-armed-and-is-not shape. Wants a verdict: route it or refuse it.
