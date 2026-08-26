# The order path: implementation spec

Written against `reference/technical-implementation-spec.md`, from the order
path section of `notes/todo.md` (the single entry plus its three folded-in
former entries and the margin-refold rider). Bound by
`reference/north-star.md` and `reference/glossary.md`; where the tree and
either of those disagree, this spec moves the tree toward them. This document
is transient: it is deleted in the commit that lands its final brick, and
every fact the tree must keep is named below with the durable document that
will carry it.

## What this builds

Market-taking execution that crosses the quoted book instead of slipping the
last print by a draw. After the last brick:

- A market buy fills against the offer side of the synthetic book, walking
  depth level by level; a market sell fills against the bid side. Slippage is
  the arithmetic consequence of depth against order size, never a draw.
- A marketable limit crosses the same way, bounded by its stated price, and
  its remainder rests or cancels according to its time in force.
- Marketability itself is judged against the touch, not against the last
  print plus a drawn trigger.
- The book a fill crossed is the book at the submit's own instant, so the
  fill is checkable against `/quotes` by anyone holding the tape.
- A triggered stop becomes a market (or limit) order crossing the book that
  prevails at the trigger instant.
- The quiet-market sign is structurally correct: there is no estimator on the
  market-taking path that can refuse and thereby produce a perfect fill. The
  volatility band survives only in the one role it is genuinely good for -
  the queue-position trigger offset on resting limits.

The north-star constraints that shape everything here: fills must be adverse
or fair, never favourable ("free money the venue manufactured" is the defect
class); passengers are non-interfering by construction, so no order ever
consumes depth that another order could observe missing; determinism per
binary holds - same seed, config and binary, same fills.

## Survey of the ground

What exists, what depends on it, and what the teardown may not drop.

### The engine (`crates/mogwai-engine/src/orders.rs`)

- `on_submit_from` is the arrival path. It computes `stated_px = risk_px(&order)`,
  takes `band_ticks` from the optional `MarketReading`, draws a trigger via
  `draw_trigger`, and for `Market | MarketToLimit` prices the fill via
  `draw_market_price` - the last print slipped adversely by a uniform draw on
  `0 ..= band_ticks`. With no reading it warns and fills at `stated_px`.
  Marketability is `order_type == Market || trades_through(side, trigger_px, reading.last_px)`.
- `MarketReading` (`lib.rs`) carries `last_px`, `ts_ns`, `band_ticks`. It is
  the whole of what the engine knows about the market at a command instant.
- `on_trigger` re-prices a triggered stop through `draw_market_price` against
  the hit's print, with the band the order was accepted under.
- `draw_trigger` / `draw_offset` implement the resting-limit queue-position
  model: stated price moved away from the market by a `ChaCha8Rng` draw keyed
  on the order's own identity. One-sided on purpose; `u = 0` is
  front-of-queue. This mechanism is not the defect and survives.
- The group path (`on_submit_group` / `process_with_market`) applies one
  reading to every member at one instant, deliberately.
- `validate_fill_funds` checks funds at the computed `fill_px` before the
  fill books. The refold rider lives behind this (`rest_open`, `take_open`,
  `refresh_open_hold`, `rebuild_order_holds_excluding`).

### The venue readers (`crates/mogwai-venue/src/fills.rs`, `vol_window.rs`)

- `read_market` walks `VOL_WINDOW_NS` of tape, produces a `MarketReading`
  through `band_reading`; refuses (`None`) on a cold estimator - fewer than
  `MIN_VOL_SAMPLES` (8) returns - which is the sign inversion: refusal means
  zero band means the most permissive fill regime the venue has, and
  `serving.rs` records that the fitted BTCUSDT tape is under the floor at a
  substantial fraction of instants.
- `read_last` supplies the price a price-less market submit is stamped with,
  and the sweeper's mark and settlement prices.
- `MarketReadingCache` memoizes one reading per river per sweep-interval
  bucket, served memo-first, then the boat's resident `VolWindow`, then the
  tape walk. The latency gate is
  `read_market_latency_stays_within_submit_budget` (5 ms resident budget,
  25/50 ms walk ceilings). The memo keys on `bucket_ns = ts / width_ns *
  width_ns`, so the reading a submit is decided against is the reading at the
  start of its sweep-interval bucket - stale by up to `fill_sweep_interval_ms`
  (100 ms by default). The cache's own doc comment states this and states the
  consequence: the exact per-fill reading is recoverable only by putting the
  reading instant on the wire or by dropping the bucketing. That staleness is
  load-bearing for this spec and is resolved below ("The reading instant"),
  not inherited.
- The only two places `fills.rs` touches a `QuoteTick` read
  `TickEvent::Quote(quote) => (quote.ts_event, None)`: timestamp taken,
  payload discarded. `vol_window.rs` folds the same shape. The book the
  consumer is quoted is invisible to the fill path.

### The synthetic book (`crates/mogwai-data/src/generated/quote.rs`)

- `QuotedWidth` (ticks, `NonZeroU32`), `TopOfBookSizes` (bid, ask,
  `Decimal`), `TradeDisplacement` - each carrying `CalibrationProvenance`,
  all currently `Uncalibrated` placeholders (width 1 tick, sizes at the
  minimum size). `place_book` places a `PublishedBook` around the drifted
  mid. The tape publishes this as `QuoteTick` (`bid_px`, `ask_px`, `bid_sz`,
  `ask_sz`) and `/quotes` answers from it. The layer has existed since tape
  protocol 7; only the calibration is absent.
- One level. There is no depth ladder anywhere in the tree.

### Tests and goldens that will move

- `crates/mogwai-venue/src/fill_golden.rs` - exact-equality fill transcripts;
  re-blessed knowingly wherever a fill price legitimately moves.
- `mogwai-engine` unit tests around `on_submit_from`, `on_trigger`,
  post-only, market-to-limit bounding, FOK/IOC planning.
- `crates/mogwai-cli/tests/serving.rs` - the socket-path gates, including
  `a_market_submit_takes_a_reading_on_the_priceless_wire_path`, which keys on
  warn text because nothing on the wire says a reading was taken. With a
  book, that question dissolves; the test is rewritten to assert the fill
  lands on or beyond the quoted touch, a wire-checkable claim.
- `scripts/smoke.py` - asserts the bracketed fill form end to end.
- The frontier discipline binds: the sweep `Walk` advances `reached_ns` only
  over spans it actually walked. Brick 4 extends what the walk carries; it
  must not change what the frontier advances over.

### Reconciled against the sibling surveys in `notes/todo.md`

The refold entry's round-8 refusal (no cache without a release-checkable
reconstruction) stands and is untouched here; brick 7 only prices it. The
retired price-span measurement stays retired: the band it would size is
retired from market pricing by brick 3, and the reduced role it survives in
(queue offset) is sized by the existing `vol_probe` instrument, not by that
measurement.

## The lifecycle model

The definitional half the owner ruling asked for. This table is the contract
every brick implements; when it lands it moves, condensed, into
`reference/architecture.md` (the durable carrier).

**The book at an instant.** The synthetic book is one quoted level plus a
parametric ladder extending it: level `k` (k = 0 is the touch) sits `k`
ticks beyond the touch on each side, with size
`touch_size * depth_growth^k`, for `depth_levels` levels. All three knobs -
`depth_levels`, `depth_growth`, and the existing width and touch sizes - are
per-instrument preset fields carrying `CalibrationProvenance`. The ladder is
a pure function of the published `QuoteTick` and the preset, computed at
read time, never on the tape: the tape's bytes do not move, and every
consumer of `/quotes` still sees exactly the top of book a real feed shows.
Beyond the last ladder level the book is deemed exhausted.

*Ladder arithmetic, exactly.* Sizes are `Decimal` end to end, because the
touch sizes and every order quantity are, and because `config.rs` already
refuses a configured `top_sizes` that is off `size_increment` or
unrepresentable at `size_precision`. `depth_growth` is therefore a `Decimal`,
not an `f64`, and level size is computed by repeated `checked_mul` from the
level below rather than by a float `powi`. Each level's size is then floored
onto `size_increment` and clamped to at least one increment, so a ladder
level is always a quantity the instrument can express and a growth factor
below the increment grid degrades to a flat ladder rather than to fractional
futures depth. A `checked_mul` overflow truncates the ladder at the last good
level, which is the exhaustion case already defined. Boot validation is
`levels >= 1`, `growth >= 1`, and every derived level representable; the
provenance sweep at the exhaustive-destructuring site in `config.rs` covers
the new scalars like any other.

**The reading instant.** The book half of a reading is taken at the submit's
own instant `ts`, never at `bucket_ns`. This is a change to
`MarketReadingCache`, and it is required rather than optional: the whole
claim of this spec is that a fill price is the arithmetic consequence of the
book, and a book stale by up to a sweep interval is a fabricated price that
can be favourable - the north-star defect class named in the opening. It also
makes brick 3's replacement gate ("the fill lands on or beyond the quoted
touch, checkable against `/quotes`") true rather than flaky in exactly the
fast spans where it matters.

The vol band keeps the bucketing. It is a trailing estimator over
`VOL_WINDOW_NS` feeding a queue-position offset on resting limits; a
bucket's staleness there is immaterial, and the memo's cost saving is
preserved for the expensive half. So the cache holds a bucketed band and
composes it with a per-`ts` book read. The book read must stay cheap enough
for that to be free, which is what the resident quote series below is for.
The cache's doc comment is rewritten in brick 2 to say this; the sentence
naming the wire field and the bucketing removal as the only two escapes goes
with it, because the second escape is the one taken.

**Crossing.** An aggressor of size `q` walks its taking side from the touch
outward, consuming `min(level size, remaining)` at each level's price, until
`q` is exhausted, the ladder is exhausted, or (for limit-bounded types) the
next level's price is beyond the stated limit. The fill is reported at the
volume-weighted average price of the consumed levels, snapped adversely to
the instrument's increment (buy rounds up, sell rounds down). No draw
anywhere in this arithmetic. Depth is not consumed statefully: the next
order, this passenger's or any other's, sees a fresh ladder - exogenous
water, non-interference by construction. Members of one atomic group share
the same fresh book for the same reason.

*The accepted limit that follows from it, named rather than left to be
found.* A stateless ladder makes size costless by slicing: one 10,000-lot
order pays the ladder, a hundred sequential 100-lot orders pay the touch a
hundred times, and that is the standard way a real algo would exploit this
venue. It is the same objection decision 3 raises against an unbounded
ladder, one level up. It is accepted anyway, because exogenous water is a
north-star constraint and the alternative - stateful depletion - makes one
passenger's fills depend on another's, which is the thing the constraint
forbids. Brick 5's chart carries a sliced-order arm so the owner sees the
magnitude rather than reads the argument. If a mitigation is ever wanted, the
place for it is a per-account transient impact term, not stateful depth, and
it is out of scope here.

**No book at the instant.** `book_reading` returns an `Option`, and the
refusal is reachable for reasons that have nothing to do with quotes: a river
read failure, a walk budget exhausted, an instant before the tape's first
quote, an unresolvable instrument profile, a price increment that will not
convert. The taking path does not carry an arm for this. Instead the engine's
taking path is made total by construction - `on_submit_from` and `on_trigger`
take a non-optional reading on that path - and the refusal is handled at the
venue boundary, which rejects the submit with the named reason "no market
data available" before the engine sees it. A triggered order whose
hit-instant book cannot be read is cancelled with the same named reason
rather than filled at any price. That is a loud, wire-visible outcome on a
path that is rare by construction, and it is not the routine refusal the todo
entry forbade: the routine refusal was the cold *volatility* estimator, which
after brick 3 no longer gates any fill.

**Per type, on arrival:**

- **Market**: always marketable. Walks the ladder. If the ladder exhausts
  before `q` does, the fill stops at the last level and the remainder is
  cancelled with a named reason ("insufficient displayed depth") - the
  exchange-realistic protection-band behaviour, and the one place a market
  order can partially fill. Depth exhaustion alone never rejects the order;
  validation, funds and risk rejections are untouched and still apply first.
- **Limit**: marketable iff its stated price is at or through the opposing
  touch (buy: `limit >= ask`; sell: `limit <= bid`). A marketable limit
  crosses like a market order but stops at levels priced beyond its limit;
  the remainder rests at the stated price. A non-marketable limit rests
  whole. Resting fills keep today's sweep model: a print strictly through
  the drawn trigger (`draw_trigger`, the queue-position band) fills at the
  stated price. The two judgments now use different prices - arrival tests
  the touch, the sweep tests the drawn trigger - so they can disagree: an
  order non-marketable against the touch may already sit through its drawn
  trigger. The arrival judgment wins for acceptance, and the order rests; the
  first sweep after acceptance then judges it like any other resting order
  and may fill it immediately at its stated price. That is correct rather
  than a leak - the fill is at the order's own limit, never better - and it
  is stated here so the first golden re-bless is not a puzzle.
- **Market-to-limit**: crosses like a market order; the stated price bounds
  the walk exactly as a limit's does; the remainder rests at the stated
  price. The current min/max clamp against `stated_px` is subsumed - the
  walk simply never consumes a level beyond the limit.
- **Post-only**: rejected iff marketable under the touch test above. No
  draw in the judgment.
- **Stop-market / market-if-touched**: rest `Conditional` as today. At
  trigger, the order crosses the book prevailing at the hit instant. The
  sweep walk already sees `TickEvent::Quote`; brick 4 makes the walk carry
  the last book at or before each hit, so the trigger and the book it
  crosses come from one pass of one river.
- **Stop-limit**: at trigger, becomes a limit judged and crossed as above
  against the hit-instant book.
- **Trailing types**: unchanged mechanics; their derived limit or market leg
  takes the corresponding path above at trigger.

**Time in force, interacting with the above:**

- **GTC / Day / GTD**: the remainder rests (and expires by the existing
  `OrderExpired` path) - for every type that has a resting price. A market
  order has none, so it is exempt: its depth-exhausted remainder is always
  cancelled with the named reason, under every time in force. No time in
  force converts a market remainder into a resting order at a derived price;
  a price the venue invented is exactly what this spec removes.
- **IOC**: the crossed part fills, the remainder cancels immediately - for
  every type, including the market order's depth-exhausted remainder.
- **FOK**: the walk is planned first; if the full quantity is not fillable
  within the type's price bound, nothing fills and the order is killed.
  `plan_fill`'s divergence-consuming contract is preserved, in the order the
  havoc paragraph below fixes.

**Havoc.** The dial perturbs the wire and the sequencing, never the pricing
*rule* - but it does change the quantity, and price is a function of
quantity, so the sequence has to be stated rather than waved at. Today
`on_trigger` computes the price and only then calls `plan_fill`; under a
walk, that would report the price of a walk that did not happen. The order,
binding on both the arrival and trigger paths:

1. Requested quantity is whatever the order states (`order.quantity`, or
   `leaves_qty` on a trigger).
2. `plan_fill` runs first, consuming any targeted `PartialFillNext` for this
   id, and `reduce_only_cap` clamps its result. This yields the diverged
   quantity. It runs before the FOK decision, exactly as the existing comment
   at that site requires, so a targeted divergence is spent whether or not
   the order goes on to fill.
3. FOK is judged against the *requested* quantity and the book's capacity
   within the type's price bound. A `PartialFillNext` cannot turn a kill into
   a fill, and a killed FOK still consumed its divergence.
4. The walk is then computed for the diverged quantity. The reported VWAP is
   the price of the levels that quantity actually consumes, so a cut fill is
   priced better than the uncut one would have been. That is correct and
   realistic - a smaller order walks less depth - and it is the reason the
   walk may not be computed once for the requested quantity and reused.
5. The remainder (requested minus filled) follows the type's remainder rule
   above.

Delays, rejects, duplicate fills and dropped updates sit on top of this
unchanged; a duplicate fill repeats a computed fill verbatim rather than
recomputing a walk. No havoc arm invents a price no level quoted.

**What retires.** `draw_market_price` and the band's role in market-taking
pricing. The reading-refusal permissive regime. The warn-text fallback
("market order has no market reading") - the taking path takes a
non-optional reading (see "No book at the instant"), so the arm has no
caller to serve and is deleted rather than kept as a quiet fallback. The
engine's `on_submit`/`on_submit_from` signatures change accordingly, and
every test passing `None` there is rewritten to the boundary behaviour;
"unreachable" is not claimed for a path an `Option` still admits.

**What survives, demoted.** The volatility band as the resting-limit queue
offset only. A refused vol reading there yields a zero offset -
front-of-queue, mildly optimistic, second-order against the arrival-path fix,
and sized by the existing `vol_probe` instrument. Recorded in
`reference/architecture.md` when brick 3 lands so nobody re-files the band as
the market model.

## Target artifacts

- `mogwai-engine/src/lib.rs`: `MarketReading` becomes

  ```rust
  pub struct MarketReading {
      pub bid_px: Decimal,
      pub ask_px: Decimal,
      pub bid_sz: Decimal,
      pub ask_sz: Decimal,
      /// Instant of the quote these four came from. Never a look-ahead.
      pub ts_ns: u64,
      /// Queue-position band half width in ticks, for resting-limit
      /// trigger draws only. Zero on a refused vol reading.
      pub band_ticks: u32,
      /// Last print at or before `ts_ns`. Retained, not removed: the
      /// conditional predicates below are print tests and stay print tests.
      pub last_px: Decimal,
      /// Ladder parameters resolved from the instrument's preset.
      pub depth: DepthLadder,
  }

  /// Resolved ladder shape. `Decimal` for the same reason `top_sizes` is:
  /// level sizes must land on the instrument's size grid. Provenance lives
  /// on the preset fields these are resolved from, not on this struct -
  /// the engine consumes resolved numbers, and the provenance sweep in
  /// `config.rs` is what audits their origin.
  pub struct DepthLadder {
      pub levels: u16,
      pub growth: Decimal,
  }
  ```

  `last_px` stays, contrary to an earlier draft of this spec, because it has
  five consumers in `on_submit_from` and its neighbourhood, not the two that
  draft counted: the price-less stamping, the marketability test, the
  conditional touch predicates (`touches_toward` / `touches_trigger`), the
  synthesized arrival-path `Hit { ts_ns, px: last_px }` fed to `on_trigger`,
  and the post-only amend guard. Only the first two move to the touch. The
  conditional predicates keep testing the print, because the lifecycle table
  says stops "rest `Conditional` as today" and swapping their input from a
  print to a quote would be a silent semantic change to stop triggering
  smuggled in as plumbing. The synthesized arrival-path `Hit` gains the book
  from the same reading, so a stop that triggers on arrival crosses a book
  and not a price. The sweeper's mark/settlement reads keep using
  `read_last`, which is untouched.

- `mogwai-engine/src/orders.rs`: a new pure function

  ```rust
  /// Walk `qty` against one side of the reading's ladder, bounded by
  /// `limit` when the type states one. Deterministic; no rng.
  fn cross_book(
      side: Side,
      qty: Decimal,
      limit: Option<Decimal>,
      reading: &MarketReading,
      increment: Decimal,
  ) -> CrossOutcome; // { filled_qty, vwap_px, exhausted: bool }
  ```

  called from `on_submit_from` (Market, MarketToLimit, marketable Limit)
  and from `on_trigger` (against the hit-carried book).

- `mogwai-protocol/src/messages.rs`: `Hit` gains the last quote at or before
  the hit (`bid_px`, `ask_px`, `bid_sz`, `ask_sz`). `Hit` lives here, not in
  `mogwai-data/src/trigger.rs`, which only imports it. It is not
  `Serialize`, so the stopping rule's "no wire change ships" survives and
  `sizing`'s serialized-byte bounds are unaffected - but `Walk` carries
  `Vec<Option<Hit>>`, so four `Decimal`s per hit is a real memory widening on
  a sweep-sized vector and brick 1 states the resulting per-sweep footprint
  rather than assuming it is free.
- `mogwai-data/src/trigger.rs`: a new
  `book_reading(source, ts, budget) -> Option<BookState>` walk answers "the
  last quote at or before `ts`" the way `vol_reading` answers the vol
  question, with the same never-look-ahead contract, and the sweep `Walk`
  populates the new `Hit` payload from the same pass.
- `mogwai-venue/src/fills.rs`: `read_market` composes `book_reading` plus the
  vol reading into the new `MarketReading`; `band_reading` shrinks to the
  band computation; `MarketReadingCache` holds the band at `bucket_ns` and
  the book at `ts`, per "The reading instant", and its doc comment is
  rewritten to say so.
- `mogwai-venue/src/vol_window.rs`: the resident window retains quotes rather
  than discarding their payload. It must be a retained series with the same
  coverage refusal `read(from, to)` already applies, not a "last quote seen"
  scalar: a scalar is bounded by the fold frontier rather than by `to`, so on
  a boat that has folded past the reading instant it would answer with a
  quote from *after* it - precisely the look-ahead `read_market`'s doc
  refuses, and a favourable one whenever the market moved. `fold(ts_ns, px:
  Option<Decimal>)` widens to carry the quote payload it currently drops.
  This is the one place the latency picture can move, so brick 2's gate is a
  real check rather than a formality.
- Preset config (`mogwai-venue/src/config.rs`, `generated/fingerprint.rs`):
  `depth_levels`, `depth_growth` beside `quoted_width` and `top_sizes`, each
  with provenance, validated (levels >= 1, growth >= 1, every derived level
  representable on the size grid). The scalars are enumerated in several
  places besides the struct: the typo-key tables (`QUOTED_WIDTH_KEYS` and
  its neighbours plus the table that lists them), the exhaustive
  destructuring test, and the provenance sweep. All of them gain entries, and
  brick 1 is not done until they do.

## Decisions resolved inline

1. **Ladder at read time, not on the tape.** Emitting depth on the tape
   would move every generated byte and every consumer for no observable
   gain - the wire's `QuoteTick` is top-of-book, like a real L1 feed. Read
   -time derivation keeps tape identity untouched until calibration lands.
2. **Deterministic crossing, no draw.** Slippage as arithmetic is the whole
   point of the entry; a drawn component on top would re-introduce the thing
   being removed. Determinism per binary is preserved trivially.
3. **Depth exhaustion cancels the remainder rather than rejecting the
   order.** Refusal-as-routine was named the non-escape; a partial fill plus
   a named cancel is what a protection band does on a real venue, and it is
   loud on the wire.
4. **The band survives only as the resting-limit queue offset.** Its refusal
   there degrades to front-of-queue, which is second-order and measured, not
   a fill-price fabrication.
5. **No new wire field, because the bucketing goes instead.** The
   `OrderFilled` reading-instant field proposed by the folded entry stays
   unbuilt, as the todo directs. That is only sound once the book half of the
   reading is taken at the submit instant: the cache's doc comment names the
   wire field and the bucketing removal as the only two ways to make a fill's
   reading nameable, and this spec takes the second. Against a bucketed book
   the `/quotes` check would be neither necessary nor sufficient - it could
   reject a correct fill or pass one taken against a different book - so the
   two halves of this decision stand or fall together.
6. **The engine's taking path takes a non-optional reading.** The refusal
   moves to the venue boundary as a named reject. An `Option` on the taking
   path is what made the previous draft claim unreachability for a state its
   own API kept representable.
7. **Calibration is blocking for the realism claim, not for the mechanism.**
   The mechanism lands and is gated on placeholder constants with
   `Uncalibrated` provenance stated loudly; the claim "slippage is realistic"
   is made only by the calibration brick. This is the todo's own framing:
   the mechanism can be specified exactly while its calibration is
   placeholder constants.

## Bricks, in landing order

The suite is green at every boundary. Each landing is one coherent change,
kept or reverted on its gates.

**Brick 1 - the book walk in `mogwai-data` and the depth knobs.**
`book_reading`, the `Hit` quote payload, the preset fields with validation.
Nothing consumes them yet, so no behaviour moves.
The config surface is part of this brick, not a follow-up: the typo-key
tables, the exhaustive destructuring test and the provenance sweep all
enumerate the scalars.
Gate: `brokkr check` (new unit tests: `book_reading` never looks ahead;
`Hit` carries the quote at or before the hit; a ladder validates its bounds;
a ladder whose growth falls below the size grid degrades flat rather than
producing an off-grid size). No golden moves; no tape byte moves.

**Brick 2 - the composed `MarketReading` through the venue readers.**
`read_market` returns the new struct; the resident `VolWindow` retains a
quote series with its existing coverage refusal; `MarketReadingCache` splits
into a bucketed band and a per-`ts` book read. The engine consumes only what
it consumed before - `last_px` is still present and every current consumer
of it is untouched this brick - except that the stamping price for a
price-less market submit becomes the touch on the taking side rather than
the last print.
Gate: `brokkr check`; `brokkr test -p mogwai-venue read_market_latency_stays_within_submit_budget --timeout 280`.
That gate is doing real work here rather than confirming a formality: the
per-`ts` book read defeats the memo for the book half, and the retained
quote series is a new allocation on the fold path. If the 5 ms resident
budget or the 25/50 ms walk ceilings move, the fix is a cheaper resident
representation, not a widened ceiling and not a return to the bucketed book.
Then `fill_golden` re-blessed knowingly for the stamping change;
`brokkr run mogwai -- serve` plus `python3 scripts/smoke.py`.

**Brick 3 - crossing in the engine.** `cross_book`; `on_submit_from` and
`on_trigger` route every taking fill through it per the lifecycle table;
`draw_market_price` and the no-reading warn arm are deleted;
marketability, post-only and market-to-limit take the touch test. The
biggest brick, and deliberately one landing: a half state where some types
cross and others draw is exactly the incoherence the keep/revert rule
forbids.
Gate: `brokkr check --gate` (the serving-path rule binds); the engine unit
suite rewritten alongside - every new regression test bite-checked (revert
`cross_book`'s adverse rounding, or the limit bound, as a text edit and
name the assertion that fires); `fill_golden` re-blessed;
`serving.rs`'s warn-text test replaced by the wire-checkable
fill-lands-on-or-beyond-the-touch assertion; `scripts/smoke.py` updated to
the same claim. Named new tests, minimum set: a buy market order fills at
or above the ask and never inside the spread; a 1-lot and a
larger-than-touch order produce different average prices; depth exhaustion
partially fills and cancels the remainder with the named reason; a
marketable limit stops at its limit and rests the remainder; FOK kills when
the bounded walk cannot cover it; a triggered stop crosses the hit-instant
book, not the submit-instant one; two identical submits at one instant get
identical fills (non-interference); a `PartialFillNext` cut yields the price
of the cut walk, not of the requested one; an FOK is killed on requested
quantity even when a divergence would have cut it under the book's capacity;
a market remainder is cancelled rather than rested under GTC.
Brick 3 moves generated execution behaviour, and the todo entry that
commissioned this work states in as many words that this change owes a tape
protocol version bump. That ruling is carried here rather than deferred to
calibration: brick 3 bumps `TAPE_PROTOCOL_VERSION` to whatever identity is
unspent when it lands, even though no tape byte moves, because the entry
asked for it, bumps are free,
and the identity a consumer would key on is the venue's fill behaviour as
much as its tape. Brick 5 then takes the next identity after that for the
preset artifacts, which do move bytes.

**Brick 4 - struck by owner ruling, 2026-08-26.** An earlier draft imported
the standing rendered-chart gate here. That gate covers changes to tape
generation, and this work moves no tape byte: the ladder is read-time only
and the water is untouched. Execution against unchanged water is gated by
the fill goldens, the fill-distribution golden and the crossing tests, so
no chart verdict is owed and brick 3 is kept on those gates alone.

**Brick 5 - calibration intake for the book constants.** Measure
`quoted_width`, `top_sizes`, `depth_levels`, `depth_growth`,
`trade_displacement_ticks` from CME TBBO/mbp-1 for MNQ (the Databento
holdings named in `notes/todo.md` cover it) and from the crypto trade
archives for BTCUSDT, landing as preset values with `Fitted { corpus }`
provenance through the normal intake sequence (`mogwai-lab` gains whatever
measurement the corpus format needs; the method stays instrument-agnostic
per the north star). Committing changed preset artifacts moves generated
quote bytes, so this brick owes a bump of its own - the identity after
brick 3's, whatever the constant holds when it lands - and the landing
re-blesses every fill and tape golden it moves, re-runs the fit tolerances,
and renders the chart again. Which of the five knobs owes it is worth being
exact about, so the rule does not look arbitrary later: `quoted_width`,
`top_sizes` and `trade_displacement_ticks` are read by the generator and
move tape bytes; `depth_levels` and `depth_growth` are read-time only and
move no byte. The bump is owed by the first three. This spec deliberately
does not carry the gated live-identity prose form, because the constant may
be bumped by unrelated work before these bricks land and a transient
document should not be the thing that has to be edited for it; the live
identity is the constant itself and the rule in `AGENTS.md`.
Gate: the realism gates, the chart, `brokkr check --gate`.
If the corpus work stalls, bricks 1-4 stand on placeholders with their
provenance saying so; this brick is the only one that may trail.

**Brick 6 - documentation settlement.** The lifecycle table lands in
`reference/architecture.md`; the band's demotion, the depth-exhaustion
cancel reason, the no-book reject reason, the reading-instant rule and the
accepted stateless-ladder limit are stated where they live. Rides in the
same commit as the last code brick it documents (markdown never alone).
This brick does not delete the spec: brick 7 is still outstanding and its
measurement contract lives here. The deletion happens in brick 7's commit,
which is the last brick, matching this document's own opening promise. If
brick 7 lands first, brick 6 deletes it.

**Brick 7 - the refold measurement.** With crossing landed, the order path
has a real workload. Register (or reuse) a `brokkr mogwai` target driving a
margin-equity account through a submit/amend/cancel mix with N resting
orders, counters emitting refolds performed and orders resting. The decision
the measurement changes, named per the standing bar: above an owner-agreed
share of submit latency at a realistic N, the logarithmic index from the
round-8 analysis gets specified (with its release-checkable reconstruction
from `OpenBook`, the condition round 8 set); below it, the todo entry closes
as priced-and-acceptable and the fold stays. Gate: `brokkr mogwai ... --bench`
against a recorded row, clean tree. Its commit deletes this spec file, the
last brick carrying out the opening promise.

## Stopping rule

Out of scope, named: the resting-limit sweep model and its trigger draw
(kept as is); `read_last` and the mark/settlement path; the tape generator's
arrival, GARCH and session machinery; every wire type, since no wire change
ships at all; the instrument-resolution slate (priority 2); tape research
v2 and anything the segment-sampler gate blocks; the adapter, beyond its
suites staying green under `brokkr check --gate`. The teardown stops at
`draw_market_price` and the reading plumbing - `draw_trigger`, `draw_offset`
and their keying are load-bearing and untouched.

## Decisions the owner is asked to make

Recommendations stated, not menus:

1. **Depth-exhaustion behaviour** - recommended: partial fill plus named
   cancel of the remainder (decision 3 above). The alternative (walk an
   unbounded ladder so market orders always fill whole) is rejected here
   because it makes size costless beyond the ladder and manufactures fills
   at prices no level quoted.
2. **Paying latency for a per-instant book** - recommended: yes, take the
   book at the submit instant and keep the band bucketed, accepting whatever
   the resident quote series costs inside the existing 5 ms budget. The
   alternative (keep the bucketed book, put the reading instant on
   `OrderFilled` so a fill is at least nameable) is rejected here because it
   ships a fill price that can be favourable and then documents it. If
   brick 2's latency gate cannot be held, this is the fork that reopens.
3. **The brick 7 threshold** - what share of submit latency the refold may
   cost before the index is built. Recommended: 20 percent at 50 resting
   orders, matching the sweep-scale example the todo entry itself uses.
