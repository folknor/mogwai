# Implementation spec: the perpetual funding publisher

Written against `reference/technical-implementation-spec.md`. Spawned from the
`notes/todo.md` entry under Adapter: "`perpetual`'s four funding fields are
still dropped silently at `convert::instrument_any`", whose stated shape is a
publisher on nautilus's `DataEvent::FundingRate` channel rather than a bail.
`reference/north-star.md` and `reference/glossary.md` bind this spec; the work
serves the north-star claim directly - a venue that charges funding cash a
consumer can never see the rate of is an exchange hiding one of its own
prices. This document is transient: it is deleted in the commit that lands
its final brick.

## The problem, precisely

Two related losses, one seam:

1. `mogwai_protocol::InstrumentClass::Perpetual` carries four funding fields
   (`funding_interval_ns`, `funding_rate`, `index_symbol`, `funding_clamp`).
   `convert::instrument_any` builds a nautilus `CryptoPerpetual`, which has no
   fields for them, so they are dropped silently. Nothing warns.
2. The venue genuinely exchanges funding cash - `Engine::apply_funding`,
   called from the sweeper pass in `crates/mogwai-venue/src/sweeper.rs`
   (`apply_engine_pass_on_clock`), debits longs and credits shorts at every
   funding instant, at the computed rate `FundingTerms::rate(mark, index)`.
   The consumer sees the balance move in `AccountState` and can never learn
   the price the venue charged it at - the instrument's funding rate is a
   price of the instrument, and this venue publishes it nowhere. (Read the
   adjudication below before restoring the stronger claim this sentence used
   to make: the frame publishes the price, and does not let a consumer
   reconstruct the cash.) Nautilus has the exact channel for this fact:
   `Data::FundingRate(FundingRateUpdate)` delivered as
   `DataEvent::Data(..)`, with `DataClient::subscribe_funding_rates` /
   `unsubscribe_funding_rates` hooks whose defaults today just log
   "not implemented".

The deliverable is therefore venue-to-consumer, end to end: a `FundingRate`
wire frame the venue pushes on the data socket, an adapter subscription that
forwards it as `Data::FundingRate`, and the four instrument fields preserved
in the nautilus instrument's `info` metadata so the def-level drop is closed
too.

## Survey of the ground

All facts below were read from the tree on 2026-08-27.

- `FundingTerms` (`crates/mogwai-protocol/src/instruments.rs`): `interval_ns:
  u64`, `interest: Decimal`, `index_symbol: Option<String>`, `clamp:
  Decimal`, with `rate(mark, index) -> Decimal` implementing
  `clamp(interest + (mark - index)/index)`. An absent or zero index leaves
  the premium at zero, by documented ruling. `InstrumentClass::funding()`
  yields the terms for `Perpetual` and nothing else (`Inverse` has no funding
  fields today; the publisher below keys on `funding()`, so if `Inverse` ever
  grows terms it is covered without edits).
- The engine's funding clock: `fn funding_instants(from_ns, to_ns,
  interval_ns) -> u64` in `crates/mogwai-engine/src/lib.rs`. Two facts about
  it the first draft of this spec got wrong, both confirmed at the site: it is
  **private** to `mogwai-engine` (no `pub`, no `pub(crate)`), and it returns a
  **count**, `(to_ns / interval_ns) - (from_ns / interval_ns)`, not the
  instants themselves. So "call it per instant" is impossible twice over and
  the publisher has exactly one option: lift the alignment arithmetic into the
  venue. The lift is two lines - an instant is a multiple of `interval_ns`
  from the unix epoch, and the span is half-open with `from_ns` exclusive - and
  it owes a tie test (brick 2) that walks a span with the lifted enumerator and
  asserts the number of instants it yields equals what the engine's counter
  returns for the same three arguments, over a table of spans including a zero
  interval, an abutting span, and a span whose ends both land exactly on
  instants. Without that test the two clocks are free to drift apart silently,
  which is the vacuous-gate shape: both halves green, neither checking the
  other.
- The engine's index read is **not** gated on materialization. `apply_funding`
  reads `self.last_marks.get(index_symbol)` - whatever the last pass put
  there. The materialization gate lives one layer out, in the sweeper's
  per-pass symbol selection ("reading an index must never spend a river nobody
  asked for", at the `materialized_symbols` filter), which decides whether the
  index symbol is handed to `read_marks` at all. Mark is a last-print read
  (`last_trade_at_or_before` through the rivers registry, see `read_marks`),
  taken at the pass's `to_ns`. The publisher reproduces the sweeper's gate, not
  the engine's read, and the two are on different threads at different
  instants - see the divergence bullet below.
- The data socket's market stream is produced by the boat-owned tape thread,
  `crates/mogwai-venue/src/tape.rs`. It walks `TickEvent`s in `ts_event`
  order, paces them to the boat clock, serializes each as a `VenueMessage`
  (`Trade`/`Quote`) into a `TapeFrame`, and publishes into a broadcast ring.
  It is constructed in `crates/mogwai-venue/src/boatyard.rs` (`Tape::start`
  with a `TapeSpawn`), at a site that holds `self.rivers` (the registry) and
  `req.river` - everything needed to resolve the profile and its funding
  terms. This thread is the only place a funding frame can be interleaved
  into the data stream in tape order: the sweeper runs on the sweep clock,
  and on an unpaced boat the tape is arbitrarily far ahead of it, so a
  sweeper-injected frame would arrive out of `ts_event` order.
- `crate::run::audience` (`crates/mogwai-venue/src/run.rs`) exhaustively
  classifies every `VenueMessage` variant with no catch-all, so a new variant
  is a compile error there until classified. `Trade` and `Quote` classify
  `Audience::Venue`.
- The adapter's data reader dispatches `VenueMessage` in
  `handle_market_message` (`crates/mogwai-adapter/src/client/data.rs`), with
  a `_ => {}` catch-all, so an unhandled new variant is silently dropped
  rather than a compile error - the adapter brick must be verified by test,
  not by the compiler. Subscriptions are satisfied entirely locally through
  the `SubState` table (`SubKind::{Trades, Quotes, Bars}` counts plus a
  `cached_quote` replay slot); nothing is sent to the venue on subscribe.
  The quote cache is **not** an unconditional `entry().or_default()`: it goes
  through `retain_quote`, which refuses to allocate a row for a symbol no
  subscription refers to once 64 such orphan rows exist, logs the drop, and
  never evicts an existing row. The funding cache is subject to the same
  hazard and inherits the same bound - see brick 3.
- The exec client's `VenueMessage` handler in
  `crates/mogwai-adapter/src/client/exec.rs` is **exhaustive with no
  catch-all**: `Trade | Quote | HistoryPage | HistoryRejected |
  HavocDiagnostic | RunComplete | PassengerDurationComplete => {}` is a
  spelled-out arm list. An earlier draft of this spec asserted the opposite and
  concluded brick 1 would compile with the exec leg untouched. It will not.
  Adding the variant is a compile error in `exec.rs` until `FundingRate` joins
  that ignore list, which is good news: the guard is by construction on both
  legs, and only `handle_market_message` in `data.rs` needs test coverage in
  place of compiler coverage.
- Nautilus (read from `research/nautilus_trader`, built against the pinned
  crates.io release - both paths per AGENTS.md): `FundingRateUpdate` in
  `crates/model/src/data/funding.rs` carries `instrument_id`, `rate:
  Decimal`, `interval: Option<u16>` (minutes), `next_funding_ns:
  Option<UnixNanos>`, `ts_event`, `ts_init`. `Data::FundingRate` exists and
  `From<FundingRateUpdate> for Data` is provided. `SubscribeFundingRates` /
  `UnsubscribeFundingRates` carry `instrument_id` plus the usual command
  envelope. The two hooks are **not symmetric in how they take the command**:
  `fn subscribe_funding_rates(&mut self, cmd: SubscribeFundingRates)` takes by
  value, `fn unsubscribe_funding_rates(&mut self, cmd:
  &UnsubscribeFundingRates)` takes by reference. Write the impls to that,
  rather than to the mirror-image shape the prose below suggests. The
  `DataClient` defaults log-and-return-Ok, so a host
  subscribing today gets silence with a log line - implementing the hooks is
  a pure widening.
- `CryptoPerpetual::new_checked` takes an `info: Option<Params>` parameter.
  Its position is settled here rather than left to the implementer: the
  signature has thirteen `Option` parameters after `multiplier` (`lot_size`,
  `max_quantity`, `min_quantity`, `max_notional`, `min_notional`, `max_price`,
  `min_price`, `margin_init`, `margin_maint`, `maker_fee`, `taker_fee`,
  `tick_scheme`, `info`), and `convert::crypto_perpetual` passes exactly
  thirteen `None`s after its `multiplier` argument, so `info` is the last of
  them. The Equity arm already uses this mechanism for `mogwai_borrowable` /
  `mogwai_settlement_ns`, which is the precedent to follow.
- `convert::crypto_perpetual` has **two** call sites - the `Perpetual` arm and
  the `Inverse` arm, distinguished by the `is_inverse` flag. `Inverse` is the
  only class that reaches this function with no funding terms, so the
  "non-funding class leaves `info` as `None`" test in brick 3 must be written
  with an `Inverse` def. A `Spot` def would exercise a different converter
  entirely and pass vacuously.
- **What the ledger actually charges, read at `apply_funding` before anything
  below was decided.** For each symbol in the swept span it computes
  `instants = funding_instants(from_ns, to_ns, interval_ns)` - a count - then
  reads `index` once from `self.last_marks`, then for each position computes
  a single `rate = terms.rate(position.mark_px, index)` and books `notional *
  rate * Decimal::from(instants)`. So the cash charged over a span crossing N
  instants is

      N * rate(mark at the pass end, index at the pass end)

  The individual instants have no price at all on the ledger side. They are a
  multiplier. This is sharper than the earlier draft's "different read
  instant" phrasing and it changes what the deliverable can honestly claim -
  see "What the frame means" below, which is now a binding section of this
  spec rather than an aside.

No watermark, cursor, frontier, permit or guard is moved by this spec. The
one cursor touched - the tape thread's walk - only gains a derived read
(`prev_ts`, the previous published tick's `ts_event`) that advances with the
publication it covers, in the same straight-line loop; it gates emission
only, never skipping.

## What the frame means (adjudicated 2026-08-27)

Two reviews converged on this from different sides - one asking which instants
exist on each side, one asking whether the published number equals the charged
number. Establishing the ledger's arithmetic first (the survey bullet above)
settles both, and the answer is not the comfortable one the first draft
assumed.

Three divergences between the ledger and the publisher, all real:

1. **Price.** The ledger charges `N * rate(pass-end mark, pass-end index)`.
   The publisher prices each instant at the mark standing at that instant.
   When the mark moves within a multi-instant sweep, no published rate need
   equal the charged one, and their sum need not equal `N *` the charged one
   either.
2. **Which instants exist.** The publisher is tick-driven: an instant is
   emitted only when a later tick arrives to cross it. The sweeper runs on
   `sim_now_ns(boat.sim)`, which on an unpaced boat can be well ahead of the
   tape cursor, and at cursor exhaustion the tape thread breaks out of its
   loop with no flush at all. So the ledger can charge instants the publisher
   never emits. This one is fixed below; it is a defect, not a semantic.
3. **The index gate.** The sweeper resolves `materialized_symbols()`
   containment once per pass; the publisher resolves it per instant on the
   tape thread. A river materializing between the two reads gives one side the
   premium and the other the bare interest.

The ruling. **The frame is market truth, not a receipt.** It states the price
of the instrument at a funding instant, computed from the venue's own terms
and the venue's own tape, and it is emitted whether or not any account holds a
position. It does not, and after this landing still will not, let a consumer
reconstruct the cash that moved their balance. Divergence 1 is therefore kept
and stated, not hidden: the first draft's "both are truthful reads of the same
terms" was true and useless, because it let the document keep an opening claim
- that the consumer will be able to learn the rate that moved the balance -
which the mechanism does not support. Publishing a plausible wrong explanation
is worse than publishing none, so the claim goes and the divergence is written
at every site a reader could form the wrong belief.

What that costs and why it is still right: expanding scope to per-instant
ledger charging is a cash-behaviour change with its own account-state gates,
and it is the *ledger* that is approximating here, not the publisher. A real
exchange prices each instant and charges at it; `apply_funding`'s
`N * rate(late mark)` is the thing that would have to change, and it should -
just not inside a spec whose keep/revert unit is a data frame. So it is filed
rather than excluded (see the stopping rule), and the publisher ships honest.

Divergence 2 is fixed, because it is the one that is straightforwardly a lie:
a balance moving with no frame behind it at all. Divergence 3 is bounded and
stated, and gets a test that shows the gate bites in both directions.

Consequences that bind the sections below:

- The opening claim in "The problem, precisely" is narrowed: the venue charges
  funding cash whose *price* the consumer can never see. It is the price that
  is being published.
- The wire doc comment, the `apply_funding` doc comment, and the durable
  adapter doc each carry one sentence naming divergence 1 in the same terms.
- Brick 2 gains a test that *pins the divergence* rather than asserting
  agreement - a scripted multi-instant span with a moving mark where the
  published rates and the ledger's single charged rate are asserted to differ
  as computed. A test that accidentally passes because the mark never moved is
  exactly the vacuous gate this repository keeps finding.

## Target artifacts

### 1. Wire frame (`mogwai-protocol`)

A new `VenueMessage` variant in `crates/mogwai-protocol/src/messages.rs`:

```rust
/// The funding rate this river's perpetual exchanges at a funding instant,
/// published on the data stream in tape order at the instant it binds.
/// Market truth, not account truth: it is emitted whether or not any
/// position pays, because the rate is a price of the instrument.
///
/// This is not a receipt for the cash. The ledger charges a whole swept
/// span at one rate taken from the pass-end mark, multiplied by the
/// number of instants the span crossed; this frame prices one instant at
/// the mark standing at that instant. Where the mark moves inside a
/// multi-instant sweep the two numbers differ, by design and not by
/// defect. Do not reconcile a balance against these frames.
FundingRate {
    symbol: Symbol,
    /// The computed rate for the instant: clamp(interest + premium), the
    /// same `FundingTerms::rate` the ledger charges with.
    #[serde(with = "rust_decimal::serde::str")]
    rate: Decimal,
    /// The interval the venue exchanges funding on, in nanoseconds.
    interval_ns: u64,
    /// The next epoch-aligned funding instant after this one.
    next_funding_ns: u64,
    /// The funding instant itself, on this boat's tape clock.
    ts_event: u64,
},
```

A new tagged variant fails outright on an older decoder rather than being
silently ignored, which is the documented preference
(`PassengerDurationComplete`'s note); both ends live in this workspace.

### 2. Venue emission (`mogwai-venue`)

`TapeSpawn` gains one field:

```rust
/// The funding terms of this boat's river, resolved at placement, plus the
/// registry handle an index read needs. `None` for a river whose class
/// exchanges no funding, which prices every non-perp boat at zero cost.
pub(crate) funding: Option<TapeFunding>,
```

```rust
pub(crate) struct TapeFunding {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) terms: mogwai_protocol::FundingTerms,
    /// `crate::source::Rivers`, not `crate::registry::Rivers` - the latter
    /// does not exist. `registry.rs` is the connection registry (who is
    /// reading an account); the river store lives in `source.rs` and is
    /// `pub(crate)`, which is what the boatyard already holds.
    pub(crate) rivers: Arc<crate::source::Rivers>,
}
```

Populated in `boatyard.rs` at the `Tape::start` call site:
`self.rivers.resolve_profile(..)` for the boat's river (the same resolve the
sweeper does per pass), then `profile.def.class.funding()`. A
`terms.interval_ns == 0` also yields `None` - the engine skips it too.

A resolve failure also yields `None`, and the earlier draft justified that
with "the boat placement is already failing elsewhere", which is asserted and
not shown. Do not ship it as an assumption: the failure mode is a publisher
that reads as gated on instrument class and is in fact gated on a resolve, so
a perpetual silently publishes nothing and the code looks correct. The rule
stands, with a `tracing::warn!` on that arm naming the symbol and saying the
boat will publish no funding frames. If the assumption is right the log never
fires; if it is wrong, the next reader is told instead of guessing.

Tape-thread logic, in the publish loop of `tape.rs`, trades only:

- Track `last_mark: Option<Decimal>` (this boat's last published trade price)
  and `prev_ts: Option<u64>` (the previous published tick's `ts_event`,
  trades and quotes alike, so the span walked matches the stream order).
- **Where in the loop.** `tape.rs`'s loop is: pull tick, fold `vol_window`,
  `pace(..)`, check cancel, record extremes, serialize, `publish`. The funding
  emission goes **after `pace(..)` and the cancel check, immediately before the
  crossing tick's own serialize-and-publish**. This is a pinned decision, not
  an implementation detail: placing it before `pace` would deliver the instant
  early in wall time by a whole inter-tick gap on a paced boat, and placing it
  after the crossing tick's publish would put the stream out of `ts_event`
  order. The consequence of the chosen spot, stated so nobody rediscovers it
  as a bug: a funding frame arrives in wall time when the crossing tick would
  have, up to one inter-tick gap after its own `ts_event`. That is the same
  latency any tape frame carries relative to its `ts_event` on a paced boat.
- The instant enumerator is **lifted** into `mogwai-venue`, because
  `funding_instants` is private to `mogwai-engine` and returns a count. Given
  `prev` and `ts`, the instants are `t = k * interval_ns` for `k` in
  `(prev / interval_ns) .. (ts / interval_ns)`, taking the upper end
  inclusive of its own product - the half-open `(prev, ts]` convention, with a
  zero `interval_ns` yielding nothing and `ts <= prev` yielding nothing. The
  tie test named in the survey is mandatory and lands with this brick.
- For each such instant `t`, when `funding` is `Some` and `last_mark` is
  `Some` and `prev_ts` is `Some(prev)`, compute:
  - `index`: `terms.index_symbol` filtered on
    `rivers.materialized_symbols()` containment (the sweeper's exact gate),
    then read as the index river's last trade at or before `t` via the same
    registry read `read_marks` uses. Unmaterialized, unresolvable or
    unpriced index leaves `index = None`. The materialization check is made
    **once per crossing, not once per instant**, and only when
    `terms.index_symbol` is `Some` - `materialized_symbols()` takes a mutex
    and allocates, which is the exact reason the sweeper guards it the same
    way. Divergence 3 in the adjudication above is not removed by this; it is
    the reason the check is cheap enough to leave where it is.
  - `rate = terms.rate(mark, index)` with `mark = last_mark`.
  - Publish `VenueMessage::FundingRate { symbol, rate, interval_ns,
    next_funding_ns: t + interval_ns, ts_event: t }` as a `TapeFrame` with
    `ts_event: t`, before the tick that crossed it - so the stream stays
    nondecreasing in `ts_event`.
- **Flush at cursor exhaustion.** When `cursor.next_tick()` returns `None`,
  the loop today closes `vol_window` and breaks. Before it does, and only when
  `funding` and `last_mark` are both `Some`, emit every instant in
  `(prev_ts, sim_now_ns(spawn.sim)]` at `last_mark`, then break as before.
  Without this, an instant the sweeper has already charged - the sweeper runs
  on the boat's sim clock, which outruns an exhausted cursor - is never
  published at all, and the consumer sees a balance move with nothing behind
  it. `spawn.sim` is already on `TapeSpawn`, so the clock read costs nothing
  new. This closes divergence 2. It does not touch the fault arm's ordering:
  the flush runs on exhaustion whether or not `cursor.fault()` is `Some`,
  because a faulted cursor's already-crossed instants are as charged as a
  clean one's.
- No instants are emitted before the boat's first trade: an unpriced mark is
  a tape that has not yet spoken, mirroring the "not yet priced" ruling on
  the class doc. Instants crossed inside the first trade's own arrival are
  therefore skipped, deliberately: there was no mark to price them at. The
  flush above inherits this - `last_mark` is `None` for a boat that never
  traded, and it emits nothing.
- The frame does not update `last_quote` and is not a quote
  (`is_quote = false`).

**Venue-side snapshot.** `Tape` gains `last_funding: Mutex<Option<TapeFrame>>`
beside `last_quote`, written in `publish` on the funding path and returned by
`subscribe_with_snapshot`, whose return becomes a pair of options (or a small
struct - the callers are few and in-crate). Without this the whole replay
story is adapter-local: `SubState::cached_funding` lives in one adapter
process, so a fresh socket, or a reconnect after `FeedLagged`, sees no funding
rate until the next instant - up to eight simulated hours of exactly the
silence this document opens by condemning. The quote path already solved this
at the boat; mirroring it half way, at the adapter only, was the first draft's
error. `subscribe_with_snapshot`'s doc comment already warns callers not to
turn its `Option` into a snapshot-first promise, and the same warning applies
verbatim to the funding slot: a boat that has crossed no instant has nothing
to replay, and that is the contract rather than a gap.

`audience()` in `run.rs` gains the arm `M::FundingRate { .. } =>
Audience::Venue`, beside `Trade`/`Quote` - the compiler forces this arm the
moment the variant lands, which is the guard-by-construction the audience
doc promises. In practice the frame never enters a sweep batch (it is born on
the tape thread), so the arm is a classification of what the frame is, not a
delivery path.

Cost, argued rather than asserted. Per tick a funding boat pays one `Option`
check and two integer divisions (`ts / interval_ns` against `prev /
interval_ns`); a non-funding boat pays one `Option` check. At an instant it
additionally pays one `materialized_symbols()` call - a mutex acquisition plus
an allocation of the river name list - and one `last_trade_at_or_before` seek
per index, which is the same registry read the sweeper takes every pass
anyway. On a paced boat instants arrive every eight simulated hours, so this
is nothing. On an unpaced boat walking a long window the instants come back to
back and the per-instant cost is what matters: it is one mutex and one seek,
bounded by window length over interval, against a sweeper already taking the
same read on its own clock. No measurement is owed, and the reason is the
standing rule rather than the smallness: no decision would change on the
result. If one ever would - if the unpaced instant walk showed up in a tape
throughput series - the fix is hoisting the materialization check out of the
crossing loop, which is a local change.

### 3. Adapter: instrument metadata (`mogwai-adapter`)

In `convert::crypto_perpetual`, when `def.class.funding()` is `Some`, build a
`Params` and pass it as the `info` argument of
`CryptoPerpetual::new_checked` - the **last** of the thirteen `None`s after
`multiplier`, established in the survey above rather than left to the
implementer:

- `"mogwai_funding_interval_ns"`: number (u64, exact in JSON).
- `"mogwai_funding_rate"`: string (Decimal as string, the Equity precedent -
  no precision loss through JSON numerics).
- `"mogwai_index_symbol"`: string or JSON null.
- `"mogwai_funding_clamp"`: string.

`None` funding terms keep `info` as `None`. This closes the "dropped
silently" half at the def seam: every field now survives to the consumer.

### 4. Adapter: the publisher (`mogwai-adapter`)

In `client/data.rs`:

- `SubKind` gains `Funding`; `SubState` gains `funding: usize` and
  `cached_funding: Option<CachedFunding>` where `CachedFunding` holds the
  last received frame's fields - the exact mirror of `cached_quote`, so a
  subscriber arriving mid-interval gets the standing rate immediately
  instead of up to a full interval of silence. `total()` includes the new
  count; the retention rule in `unsubscribe_symbol` keeps a row with either
  cache resident, matching the quote behaviour.
- `subscribe_funding_rates(cmd: SubscribeFundingRates)` - **by value** - and
  `unsubscribe_funding_rates(cmd: &UnsubscribeFundingRates)` - **by
  reference**; the trait is asymmetric here and the impls must match it or
  they do not compile. Subscribe derives the symbol via
  `symbol_from_instrument`, calls `subscribe_symbol(symbol,
  SubKind::Funding)` (which applies the bound-symbol check for free), then
  replays `cached_funding` through the same delivery-ordering guard shape
  `subscribe_quotes_inner` uses if a cached frame exists. Unsubscribe mirrors
  `unsubscribe_quotes`.
- `handle_market_message` gains a `VenueMessage::FundingRate { .. }` arm:
  cache the frame in the symbol's `SubState`, then when the symbol's
  `funding` count is non-zero, convert and send
  `DataEvent::Data(Data::FundingRate(update))`.
- **The cache write is bounded, and "like quotes" is what bounds it.** The
  first draft said "cache unconditionally (like quotes)", which misreads the
  quote path: quotes go through `retain_quote`, which refuses to allocate a
  row for a symbol no subscription refers to once 64 orphan rows exist, warns
  on the drop, and never evicts a row that already exists. A plain
  `entry(symbol).or_default()` on the funding path would reintroduce
  unbounded growth from malformed wire symbols - the exact defect that bound
  was added to close - and would do it through a map the quote path is also
  bounding, so the funding path could push the shared orphan count past what
  the quote path expects. Generalize `retain_quote` into one shared helper
  that takes the symbol and the field to write (or add `retain_funding`
  beside it with the same constant and the same orphan accounting over the
  same `total() == 0` predicate). Either way `SubState::total()` counts
  `funding` too, so a funding subscription keeps its row out of the orphan
  set. This owes a saturation test: 64 orphan funding symbols cached, the
  65th refused and not allocated, and an existing row still updated after
  saturation.
- Conversion, a new `convert::funding_rate_update(..)` beside the tick
  converters:
  - `instrument_id`: from the subscribed instrument (resolved the same way
    the trade path resolves it).
  - `rate`: the wire `Decimal`, passed through - both sides are
    `rust_decimal`.
  - `interval`: `Some(u16)` only when `interval_ns` is an exact whole number
    of minutes that fits `u16` (`interval_ns % 60_000_000_000 == 0` and the
    quotient converts); otherwise `None`. Never round - a wrong minute count
    is a lie, an absent one is honest.
  - `next_funding_ns`: `Some(UnixNanos::from(next_funding_ns))`.
  - `ts_event`: the frame's; `ts_init`: `now_unix_nanos(sim)`.
- The exec client's `VenueMessage` handler has no catch-all (see the survey).
  `FundingRate` joins the spelled-out ignore arm beside `Trade | Quote |
  HistoryPage | ...`, with a clause on that arm's existing comment saying a
  funding rate is data-path truth the execution leg has no event to publish
  for. This is a compile requirement of brick 1, not an optional check here.
- `next_funding_ns` stays on the wire even though it is exactly `ts_event +
  interval_ns` and both are on the frame. It is redundant, and it is kept
  deliberately: it maps one-to-one onto nautilus's `next_funding_ns` field,
  and deriving it adapter-side would put the epoch-alignment convention in a
  second place, where it can drift from the venue's. One redundant `u64` is
  cheaper than two copies of a clock.

### 5. Prose that moves in the landing commit

- `reference/architecture.md`: wherever the wire's `VenueMessage` surface or
  the data client's capabilities are described, add the funding frame and
  the subscription. Find the sites by grepping the durable docs for
  `VenueMessage` and `subscribe`; `docs/adapter-lifecycle.md` describes the
  adapter's consumer surface and gains the funding subscription there.
- Delete the originating entry from `notes/todo.md` (and this spec file).
- The durable fact this spec must not lose, now stated exactly: the ledger
  charges `N * rate(pass-end mark, pass-end index)` for a span crossing `N`
  instants, while the publisher prices each instant at the mark standing at
  that instant, so the published rates and the charged rate need not agree and
  a balance cannot be reconciled from the frames. That sentence lives with the
  `apply_funding` doc comment (the mechanism's home, per the
  backlog-adjudication rule), added in brick 2's landing, and in shorter form
  on the wire variant's doc comment and in `docs/adapter-lifecycle.md` where
  the subscription is described. Three sites, because a consumer can form the
  wrong belief at any of them.
- `notes/todo.md` gains the per-instant-ledger entry described in the
  stopping rule, in the same commit that deletes the originating entry.

## TAPE_PROTOCOL_VERSION

**Ruling: brick 2 bumps `TAPE_PROTOCOL_VERSION` to 29.** The two reviews
disagreed outright on this and it was decided at the rule's wording and the
glossary's definition rather than by vote. Both arguments are recorded because
the losing one is not silly.

The no-bump argument. The rule enumerates "a generator constant, an
arrival-clock or GARCH parameter, the committed fingerprint, seed derivation,
the fill band's draw, or the tape origin" - every item generator-internal. The
funding frame consumes no draw, reads no artifact, and leaves the generated
tick sequence byte-identical. And the rule narrows itself further: "the bump
is owed by the commit of a changed artifact, not by a change to the code that
could produce one." No artifact changes here.

Why it loses. Three reasons, in order of weight.

1. **The enumeration is introduced with "This includes", not "this is limited
   to".** It is a list of things people kept forgetting, not a definition of
   the path. Reading an open list as closed is exactly the move the rule's own
   justification forbids: "nothing can detect that a determinism-affecting
   change should have bumped the version and did not."
2. **The glossary defines the tape as what a boat publishes** - "the paced
   frame stream broadcast to that boat's passengers only". Not the generated
   tick sequence. Under the repository's own vocabulary, brick 2 changes the
   tape: same seed, same config, previous binary gives one frame stream; same
   seed, same config, this binary gives a different one. That is the precise
   thing the version identifies, since determinism is promised per binary and
   the version is how a binary's stream identity is named.
3. **The "changed artifact" clause is a narrowing for a specific hazard, and
   not this one.** It exists because `mogwai-lab` depends on `mogwai-data` and
   the generator reads `fingerprint.json` through `include_str!`, so editing
   the synthesis code cannot move a tape byte until a regenerated artifact
   differs. That reasoning is about code that *cannot* move a byte. Brick 2's
   code moves bytes in the published stream on the very next run.

The tie-break, if the three above were somehow balanced: AGENTS.md states that
bumps are free and that no consumer has ever depended on a tape identity, so
the rule "forecloses nothing". The cost of an unnecessary bump is zero; the
cost of a missed one is two different streams sharing one identity, with
nothing able to detect it. Under a rule whose whole justification is
undetectability, an unforced tie breaks toward bumping every time.

Mechanics. `TAPE_PROTOCOL_VERSION` is 28 today and the next unspent identity
is 29, per AGENTS.md. Brick 2 - the brick that actually changes what a boat
publishes - carries the bump, not brick 1 (a variant nothing emits changes no
stream) and not brick 3 (the adapter reads, it does not publish). The prose
claim in AGENTS.md moves in the same commit, and
`crates/mogwai-data/tests/tape_version_prose.rs` enforces that it does.
Bricks 1, 3 and 4 owe no further bump.

If the implementation additionally touches any draw or any committed artifact,
that is a deviation from this spec and is re-adjudicated, not absorbed under
this bump.

## Bricks, in landing order

Each landing leaves `brokkr check --gate` green (the adapter is touched from
brick 3 on, and the gate invocation is mandatory for it; run it from brick 1
anyway - it is the complete answer).

**Brick 1 - the wire variant.** `VenueMessage::FundingRate` in
`mogwai-protocol`, plus the `audience()` arm in `mogwai-venue` (the variant
does not compile into the venue without it - one landing, two crates,
ordered internally by the compiler). Tests: a serde round-trip in the
protocol crate proving tag, field names and the string-Decimal rate
serialize and reparse identically (the wire-protocol gate class); extend the
existing `audience` pin test with the new variant's `Audience::Venue`
verdict. Gate: `brokkr check --gate`.

Also in brick 1, because the compiler demands it: the exec client's
`VenueMessage` match in `mogwai-adapter/src/client/exec.rs` is exhaustive, so
`FundingRate` joins its ignore arm here or nothing compiles. Only
`handle_market_message` in `data.rs` has a `_ => {}`, and that is the one arm
brick 3 must prove by test. With the exec arm added the frame is unemitted and
unread and everything stays green. No version bump in this brick: a variant
nothing emits changes no stream.

**Brick 2 - venue emission.** `TapeFunding`, the `TapeSpawn` field, the
boatyard resolution with its warn-on-resolve-failure, the lifted instant
enumerator, the tape-thread emission logic, the exhaustion flush, the
`Tape::last_funding` snapshot slot, the `apply_funding` doc-comment sentence,
and the `TAPE_PROTOCOL_VERSION` bump to 29 with its AGENTS.md prose. Tests, in
`mogwai-venue` beside `tape.rs`'s existing unit tests, driving `Tape::start`
with a scripted cursor (the crate's test tick sources):
- trades straddling one instant produce exactly one `FundingRate` frame,
  with `ts_event` on the epoch-aligned instant, `next_funding_ns` one
  interval later, `rate` equal to `terms.rate(last trade before the
  instant, None)`, and the frame ordered before the crossing tick;
- a span crossing two instants produces two frames, each priced at the mark
  standing when it is emitted;
- no frame before the first trade even when instants were crossed;
- a non-funding river (`funding: None`) and a zero interval produce no
  frames on the same input;
- an index case: with a registry whose index river is materialized, the rate
  reflects the premium; with the same terms and no materialized index, the
  rate is the bare interest (this is the vacuous-gate hazard in this brick -
  the gate must be shown to bite in both directions);
- the enumerator tie test: over a table of spans - zero interval, abutting
  spans, both ends on instants, a span crossing none, a span crossing many -
  the lifted enumerator yields exactly `funding_instants(from, to, interval)`
  many instants, all epoch-aligned multiples in `(from, to]`. The engine's
  counter is private, so this test lives where it can see both: either a
  `pub(crate)` test-only re-export from `mogwai-engine`, or the counter's
  formula restated in the test itself with a comment naming the site it
  mirrors. Prefer the re-export; a restated formula tests nothing about the
  engine;
- the exhaustion flush: a cursor that ends after crossing an instant, with the
  boat's sim clock past that instant, publishes it before the tape thread
  leaves its loop, and publishes nothing when the boat never traded;
- the snapshot: a subscriber attaching after a funding frame receives it from
  `subscribe_with_snapshot`, and one attaching before any instant receives
  `None`;
- the divergence pin (see the adjudication): a scripted multi-instant span
  with a moving mark, where the published per-instant rates are asserted
  against the ledger's `N * rate(pass-end mark)` and shown to differ. This
  test exists to stop a later reader "fixing" the divergence into agreement
  by accident, and it must be written so that a flat mark would fail it.
Each new test is bite-checked per the doctrine: revert the emission, the
index gate, the flush and the snapshot write as text edits, observe the named
failure, restore as a text edit. Gates: `brokkr check --gate`, and the serving
path is touched, so also the live end-to-end pass: `brokkr run mogwai --
serve` plus `python3 scripts/smoke.py`. The smoke question is settled rather
than left conditional: `scripts/smoke.py` never matches on frame type
exhaustively - every read is `ws.until(predicate)` over `frame.get("type")` -
so an unrecognised frame is skipped by construction and no smoke change is
owed. Note it does exercise a perpetual (`BTCUSDT`), so funding frames will
genuinely appear in that run; the two "first market frame" reads there filter
on `type in ("Trade", "Quote")` and a funding frame cannot precede the first
trade anyway. No throughput measurement, for the reason argued in the cost
paragraph above.

**Brick 3 - adapter metadata and publisher.** Bricks 3 and 4 of the target
land together as one coherent adapter change: the `info` metadata in
`convert::crypto_perpetual`, the conversion function, `SubKind::Funding`,
the subscribe/unsubscribe hooks (asymmetric signatures), the
`handle_market_message` arm, and the bounded cached replay. The exec arm
landed in brick 1. Tests, in the adapter crate:
- a convert unit test: a `Perpetual` def's four fields appear in the built
  instrument's `info` under the `mogwai_` keys, Decimals as strings; an
  `Inverse` def - the only other class reaching `crypto_perpetual`, and the
  only one there without funding terms - leaves `info` as `None`. A `Spot`
  def would take a different converter and pass vacuously;
- the orphan saturation test: 64 orphan funding symbols cached, the 65th
  refused without allocating a row, an already-resident row still updated
  after saturation, and `total()` counting `funding` so a subscribed symbol
  is never in the orphan set;
- a convert unit test for `funding_rate_update`: exact minutes convert,
  non-minute and oversized intervals yield `interval: None`, the rate
  passes through bit-identically;
- the socket-backed data-client suite (`data_client_transport` is the
  existing home for pushed-frame handling) gains: a pushed `FundingRate`
  frame reaches the sink as `Data::FundingRate` when subscribed, is dropped
  when not, and a subscribe after a frame replays the cached rate; the
  bound-symbol refusal applies to the funding subscription like any other.
Bite-check each: revert the dispatch arm, the subscription gate, and the
cache write as text edits and watch the named assertions fail for the
stated reason. Gate: `brokkr check --gate` (the only invocation that runs
the four socket-backed binaries - plain `brokkr check` is blind to this
brick's most important tests).

**Brick 4 - prose and closure.** The durable-doc updates named above (the
`apply_funding` doc-comment sentence lands earlier, with brick 2, at the
mechanism it describes), the new `notes/todo.md` entry for per-instant ledger
charging, deletion of the originating todo entry and of this spec. Markdown
rides the final code landing per the commit rules; nothing here lands alone.

## Keep/revert

Each brick is one coherent landing kept or reverted whole on its gates. The
riskiest is brick 2 (a new frame in every consumer's data stream); its
revert is deleting the emission, the flush, the snapshot slot and the
`TapeSpawn` field, which returns the stream byte-for-byte to today's, because
nothing else consumes the frame until brick 3 - which is exactly why brick 3
lands after it and not with it. The version bump does not revert with it: a
spent identity stays spent, which is what "bumps are free" buys.

## Stopping rule and exclusions

In scope: everything above, and nothing else. Named exclusions:

- Per-instant ledger charging - a cash-behaviour change with its own
  account-state gates, excluded here and **filed** in brick 4 rather than left
  as a shrug, because after this landing it is consumer-visible. The entry to
  write, in the terms established above: `apply_funding` charges
  `N * rate(pass-end mark, pass-end index)` for a span crossing `N` funding
  instants, so on a multi-instant sweep with a moving mark the cash charged
  matches no instant's actual price. The ledger is the side approximating, not
  the publisher. Closing it means walking the instants in `apply_funding` and
  pricing each at a mark read for that instant, which needs marks the engine
  does not currently receive - so it is real work, not a rename.
- A funding history request (`RequestFundingRates` exists in nautilus) - the
  venue keeps no funding-rate history and synthesizing one is not asked for
  by the originating item; the default log-and-Ok hook stays for requests.
- Funding terms for `Inverse`, predictive/next-rate publication between
  instants, and any change to how `MarkOutcome` or the sweeper deliver
  events.
- The forex swap fields: they fund through the daily-rollover arm, carry no
  wire-visible rate today, and the originating item names the perpetual
  channel only.

## Review ledger (2026-08-27)

Two independent reviews, consolidated here. Every claim below was re-checked
against the tree rather than taken from the report; the reports are superseded
by this section and by the edits above, and are deleted with this spec.

Folded in, all valid:

| Claim | Where it landed |
|---|---|
| `funding_instants` is private and returns a count, so "call it per instant" is impossible | Survey; brick 2 enumerator and tie test |
| `crate::registry::Rivers` does not exist; the type is `crate::source::Rivers` | `TapeFunding` sketch |
| `subscribe_funding_rates` takes by value, `unsubscribe_funding_rates` by reference | Survey; adapter section 4 |
| No venue-side snapshot: `subscribe_with_snapshot` replays `last_quote` only, so a reconnecting socket waits up to an interval | New `Tape::last_funding` slot in section 2 |
| Emission is tick-driven, so an instant crossed before cursor exhaustion is never published while the sweeper may already have charged it | Exhaustion flush in section 2; divergence 2 in the adjudication |
| The index materialization gate is resolved per pass by the sweeper and per crossing by the publisher, and can disagree | Divergence 3; the both-directions test in brick 2 |
| Ordering relative to `pace()` was unspecified | Pinned: after `pace`, before the crossing tick's publish, with the wall-time consequence stated |
| The `None`-on-resolve-failure rule was asserted, not shown | Kept with a mandatory `warn!` |
| The cost claim was asserted, not argued | Rewritten as an argument with the unpaced case named |
| The non-funding `info` test needs an `Inverse` def, not a `Spot` one | Brick 3 |
| The smoke-script question should be settled, not left conditional inside a brick | Settled: predicate-filtered, tolerant, no change owed |
| The published rate is not the rate that moved the balance | The whole "What the frame means" adjudication |
| The funding cache would bypass `retain_quote`'s 64-orphan bound | Adapter section 4 and its saturation test |

Found during this validation, in neither report: **the spec's claim that the
exec client has a catch-all is false.** `exec.rs`'s `VenueMessage` match is
exhaustive with a spelled-out ignore arm, so brick 1 does not compile without
adding `FundingRate` to it. Corrected in the survey and in brick 1.

Rejected, with reasons:

- **"`next_funding_ns` is redundant on the wire and could be derived
  adapter-side."** Correct that it is redundant, rejected as a change. It maps
  one-to-one onto nautilus's field, and deriving it adapter-side puts the
  epoch-alignment convention in a second place where it can drift from the
  venue's. Recorded at the field.
- **"No `TAPE_PROTOCOL_VERSION` bump is owed."** Adjudicated against; the full
  argument, including why the no-bump reading is reasonable, is in the
  `TAPE_PROTOCOL_VERSION` section. The ruling is 29, carried by brick 2.
- **"Per-instant ledger charging must be in scope."** The problem is real and
  the report is right that the deliverable was untruthful as written. Rejected
  as a scope expansion and answered instead by making the frame's meaning
  honest and filing the ledger defect: the fix belongs at `apply_funding` with
  account-state gates, not inside a spec whose keep/revert unit is a data
  frame. See the adjudication and the stopping rule.
- **"The `TAPE_PROTOCOL_VERSION` reasoning is correct"** (the opposing
  report's affirmative finding). Rejected on the same adjudication.
