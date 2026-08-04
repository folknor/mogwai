# Technical implementation spec: the observable top-of-book layer

Written against `reference/technical-implementation-spec.md`, which defines what
this document must contain. Spawned from `DATA-PURCHASE-REPORT.md` section 14.4,
item B, and step 5c of section 11.

Status: IMPLEMENTED 2026-08-04.

Post-implementation correction: the measured 24-hour reach is enforced by
`MAX_WARMUP_MATERIALIZATION_TICKS` across lock-releasing chunks.
`MAX_EXTEND_TICKS` retains its original per-lock runaway-backstop purpose. The
sweep refusal ceiling was resized, but its latency warning remains at the old
2,500,000-tick signal rather than scaling with that ceiling.

Revision 5, after five review rounds. Revision 1's survey claimed the blast
radius was small because "downstream consumers already filter"; that claim was
generalized from two call sites and was wrong at three others, two of which fail
SILENTLY. Section 1 now carries the corrected survey, and section 9 records what
each revision got wrong so the same shortcut is not taken again.

## 0. What this closes, and what it deliberately does not

Before this landing, `mogwai` published no market. Nothing in the workspace
constructed a `QuoteTick`: the type existed in `mogwai-protocol`,
`TickEvent::Quote` existed in `mogwai-data`,
the server's tape loop relays that variant, `mogwai-adapter` converts it to a
nautilus `QuoteTick` and dispatches it to the host - and no source has ever
produced one. A subscribed host therefore never sees a bid or an ask, and
`/quotes` returns an empty vector unconditionally.

This spec builds the layer that makes a spread exist at the protocol boundary.

**In scope.** Quote synthesis in `GeneratedSource`; the emission ordering
contract; the trade displacement re-referenced to the published book midpoint;
BOTH calibration seams as validated per-instrument scalars; the `/quotes` history
scan; the connect-time BBO snapshot and its delivery through the adapter; the
tape-order invariants; the tick-budget resize; one `TAPE_PROTOCOL_VERSION` bump;
the golden re-bless; the repair of every site that assumes a trades-only stream.

**Named and excluded, because it belongs to a genuinely separate item.** Fitting
`P(width_ticks | causal_return_scale)` and the trade-displacement response from
CME TBBO. That data is not purchased, and section 11 of the report puts the
purchase AFTER this landing precisely because a fit needs somewhere to land. This
spec builds both seams and holds both quantities at explicitly uncalibrated
constants. It is not deferral of this item's work: the calibration was never part
of item B.

**The consequence of that split, stated so it is not discovered later.** This
landing takes the tape to protocol 7 with width and displacement constant. The
joint calibration of quoted width and trade displacement - which the report
requires be fitted as two separate response functions against one causal
volatility state - is one further landing at protocol 8. Two bumps, not one,
because the two landings are separated by a purchase.

## 1. Survey of the ground

### 1.1 What exists, and what breaks

| Artifact | State under a quote-carrying stream |
|---|---|
| `mogwai_protocol::QuoteTick` | defined: `symbol`, `bid_px`, `ask_px`, `bid_sz`, `ask_sz`, `ts_event`. Never constructed. |
| `mogwai_data::TickEvent::Quote` | defined, matched in `ts_event()`. Never constructed. |
| `mogwai-server` `tape.rs` | already maps `TickEvent::Quote` to `ServerMessage::Quote`. Relay needs no change; the snapshot of section 4 does. |
| `mogwai-server` `http.rs` `quotes()` | returns `Json(Vec::new())` unconditionally, with a comment anticipating this work. |
| `mogwai-server` `http.rs` `bounded_trades()` | skips non-trade ticks via `continue`. SAFE. |
| `mogwai_data::trigger::scan_triggers` | guards with `if let TickEvent::Trade`. SAFE. |
| `mogwai_data::bars` | folds trades directly, never sees a `TickEvent`. SAFE. |
| `mogwai-server` `source.rs:276` `last_trade_at_or_before` | **BREAKS SILENTLY.** `while let Some(TickEvent::Trade(trade)) = source.next_tick()` is a refutable pattern in a `while let`: the loop ENDS at the first quote. Returns `None` or a stale price. |
| `mogwai-server` `gen.rs:130` | **BREAKS SILENTLY.** `std::iter::from_fn` with `_ => None` ends the iterator at the first quote, so `mogwai gen --type trades` and `--type bars` emit empty output. |
| `mogwai-server` `fills.rs:267` | **BREAKS LOUDLY.** `TickEvent::Quote(_) => panic!("generated tape is trades-only")` in a test helper. |
| `mogwai-adapter` `convert::quote_tick` | written and wired, DEAD CODE: never run against a real quote. Uses the panicking `NautilusQuoteTick::new`. |
| `mogwai-adapter` `client/data.rs:1076` | gates quote dispatch on `s.quotes > 0`. Defeats the connect-time snapshot - see section 4.3. |
| `source::CHECKPOINT_K`, `fills::SWEEP_DRAIN_BUDGET`, `source::MAX_WARMUP_MATERIALIZATION_TICKS` | counted in TICKS. Their sim-time reach shrinks - see section 5.2. |

**`last_trade_at_or_before` is the one that matters most.** It backs `read_last`
and `stamp_market_price`, which is the order-admission and stale-print path. A
silent `None` there is a venue that stops knowing its own last price. Revision 1
of this spec asserted in its stopping rule that the engine's fill path was not
touched; that was false, and it was false because the survey stopped at two call
sites.

**The lesson, recorded because it is the second instance in this workstream.**
The `HALF_SPREAD_TICKS` error was an inference from a name that was never checked
against behavior. This was an inference from two samples that was never checked
against the population. Both entered as a confident sentence in a survey. The
mechanical remedy is in brick 0.

### 1.2 The two facts that genuinely shrink this spec

**`build_live_source` IS `build_history_source`.** `source.rs` defines the live
constructor as a direct call to the history one with `start = Some(sim_now)`.
Both resume the same `CheckpointIndex` chain from the same boot seed. The
requirement that bounded `/quotes` regeneration reproduce the live quote sequence
for the same seed and interval is therefore satisfied BY CONSTRUCTION, and needs
a test that proves it rather than machinery that establishes it.

**`MergeSource` preserves intra-symbol emission order.** `next_tick` picks by
`min_by_key(ts_event)`, which returns the first minimum on ties, and a symbol's
quote and its trades come from ONE source that emits them sequentially. A quote
emitted before a same-timestamp trade stays before it. The merge cannot reorder
within a symbol.

### 1.3 The draw order, which is load-bearing

`GeneratedSource::begin_event` consumes randomness in this order, verified
against the code rather than assumed:

1. `next_latent_mid` - GARCH innovation.
2. `next_price` calls `BounceState::next_side` (aggressor draw), then
   `next_drift` (drift step).
3. The child-count draw, from a per-event `SweepShape`.
4. The repeat draw, `rng.random_bool(EVENT_PRICE_REPEAT_PROB)`, taken only when
   `count == 1 && !high_regime`.

`next_price` computes `mid_ticks = mid / tick + drift_ticks`, so the price the
generator prints today is placed off the DRIFTED mid, not the raw latent mid.

This spec does not reorder any of it. Section 3 shows how the quote fits without
a single draw moving.

### 1.4 There is no `Subscribe` frame

The subscription model was retired; the venue pushes the run's one tape unbidden,
and `messages.rs` asserts that a client sending `Subscribe` is REFUSED by the
decoder rather than ignored. The tape thread starts at boot and broadcasts into a
bounded ring; `handle_socket` subscribes mid-flight and relays what comes next.

A host connecting between two trades therefore receives a trade before it has
ever seen a bid or an ask. The generator cannot fix this. It is a server
obligation, and section 4 lays it.

## 2. The quote model

### 2.1 Width is an integer number of ticks

```rust
/// Quoted width in whole ticks. Never zero: a crossed or locked synthetic book
/// is not a market condition this generator models.
///
/// Carries its provenance for the same reason `TopOfBookSizes` and
/// `TradeDisplacement` do - it is the third quantity this landing ships
/// uncalibrated, and a width that could only be described as fitted in PROSE is
/// a width that will eventually be described as fitted by someone who did not
/// read the prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuotedWidth {
    ticks: NonZeroU32,
    provenance: CalibrationProvenance,
}
```

A continuous half-width rounded OUTWARD independently on each side is what
produced the phase-dependent separation the synthetic decomposition reports
(`f=0.000:2t ... f=0.500:1t ... f=0.875:2t`): two independent roundings of one
continuous quantity do not preserve their difference. Real quoted spreads live on
integer ticks. The schema follows the market.

**This replaces existing code, and the replacement is named here because
revision 1 criticised the pattern without saying where it lived.** `next_price`
currently uses `.ceil()` for a buy and `.floor()` for a sell (`source.rs:489`).
That IS the independent outward rounding. Brick 3 removes it. `tests.rs`
reconstructs a counterfactual book with the same `ceil`/`floor` pair and changes
with it.

### 2.2 One centering rule, one rounding, off the drifted mid

Let `m` be the latent mid in price, `t` the modal tick, `w` the width in ticks,
`k` the accumulated `drift_ticks`, and

```
mt = m / t + k                     // the DRIFTED mid, in ticks
bid_ticks = round(mt - w / 2)      // the ONLY rounding in quote construction
ask_ticks = bid_ticks + w          // exact, by construction
```

**Why the drifted mid.** `drift_ticks` is a per-event accumulated walk of the
tradable level away from the latent mid, and `DRIFT_RECENTER_FRAC` recentres it
on the residual between a sweep's last print and the latent mid. That is a
description of a book moving, not of a print offset. Placing the book off the
undrifted mid would make the book and the print disagree by the whole drift and
would strand the recentring logic as dead code.

`round` is `f64::round`, half away from zero; every quantity is positive, so that
is half-up. The tie is documented rather than avoided because it is reachable.

Four properties follow, each asserted in section 6:

- `ask_ticks - bid_ticks == w` exactly, for every `w` and every phase.
- `book_mid_ticks = bid_ticks + w / 2`. Odd `w` gives a half-integer tick
  midpoint - the book straddles two tradable levels, which is what a one-tick
  market does. Even `w` gives an integer-tick midpoint.
- `|book_mid_ticks - mt| <= 0.5`, immediately from `|round(x) - x| <= 0.5`.
  The bound is against the DRIFTED mid; the distance to the latent mid is the
  drift plus this, and is not bounded.
- The width survives the one-tick floor.

**The one-tick floor.** `next_price` already floors the printed tick count at
1.0. The book needs the same fence, applied to the bid and PROPAGATED:

```
bid_ticks = bid_ticks.max(1.0)
ask_ticks = bid_ticks + w
```

Flooring each side independently would narrow the book at the floor - the same
bug class as independent rounding. When the floor binds, the half-tick bound no
longer holds; that is correct, because the alternative is a non-positive price.
Section 6 asserts the floor is NOT reached on any shipped preset rather than
asserting a bound that accommodates it.

**Width admission.** Revision 1 proposed a bound of "small enough that the bid
cannot underflow at the start price". That was incoherent: width does not cause
bid underflow under this formula, and the start price does not bound later mids.
The rule is structural only - integral and non-zero, enforced by the type - plus
checked price construction at the boundary, so an `ask_px` that cannot be
represented as a `Decimal` at the instrument's `price_decimals` is an error and
not a panic.

### 2.3 The trade displacement is measured from the book, not the latent mid

```
price_ticks = round(book_mid_ticks + sign * d)
```

`d` is the displacement in ticks, `sign` is `+1` for a buyer-initiated print and
`-1` for a seller-initiated one. `book_mid_ticks` already contains the drift, so
the drift is not added a second time.

At the shipped constants (`w = 1`, `d = 0.5`) this is exact:
`book_mid_ticks = bid_ticks + 0.5`, so a buy lands on `bid_ticks + 1 = ask_ticks`
and a sell on `bid_ticks`. Prints sit at the touch on both sides at every grid
phase. The phase-dependent separation disappears because the quantity that
produced it no longer exists.

**`d` does not map injectively to a print level, and the tests must not assume it
does.** At `w = 1`, every `d` in `(0, 0.5]` prints at the touch: `round` collapses
the interval. An independent-settability test that varied `d` by a small amount
and expected a different print would pass or fail for the wrong reason. Section 6
therefore reasons about REALIZED grid displacement - the set of distinct printed
levels a given `(w, d)` pair produces - and asserts the boundaries where the
realized level changes, not that the continuous parameter is visible.

### 2.4 Top-of-book sizes are a structural input, uncalibrated

No defensible size model exists before TBBO inspection, and deriving book sizes
from trade sizes would fabricate a relationship the data has not been asked
about. The seam is built; the values are labeled.

```rust
/// Whether a quote-layer quantity was measured, and against what. ONE type for
/// every such quantity - sizes, width, displacement - because they are all
/// answering the same question about the same landing, and three parallel enums
/// would drift. It is NOT shared with the trade-layer scalars, whose provenance
/// predates this work and means something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalibrationProvenance {
    /// A placeholder chosen so the wire type has a value. It means nothing.
    Uncalibrated,
    /// Fitted from a measured corpus, which is carried so the type can enforce
    /// the contract its own name makes: revision 1 documented a corpus and then
    /// had nowhere to put one.
    Fitted { corpus: String },
}

#[derive(Debug, Clone)]
pub struct TopOfBookSizes {
    pub bid: Decimal,
    pub ask: Decimal,
    pub provenance: CalibrationProvenance,
}
```

`TradeDisplacement` carries the same type. It does not "share the shape" of a
size provenance informally - displacement is not size, and an implementer needs
the actual type name rather than a resemblance.

**The placeholder is the instrument's minimum tradable size, not
`latent_size_median`.** Revision 1 claimed the median "cannot produce a malformed
`QuoteTick`" because it is in native units. That was wrong for the same reason
the whole `typical_notional` episode was wrong: `latent_size_median` is the
PRE-QUANTIZATION latent parameter of the size distribution, not a published size.
A futures instrument may legitimately carry a latent median of 0.5 while its
published sizes must be integral and at least one contract. The minimum size is
on the grid by definition.

**Validation is SPLIT, and each half is limited to what its layer can actually
see.** An earlier revision said `mogwai-data` rejects sizes "off the grid it was
given" while also saying `SizeGrid` lacks what it would need to know that. Both
cannot be true. The split:

- **`mogwai-data`, from `SizeGrid`**, guarantees exactly what `SizeGrid` knows:
  `multiplier`, `integral` and `min_size`. So the `try_*` constructors reject a
  quote size below the minimum, a non-integral size on an integral grid, and a
  size that is not a whole multiple of the multiplier. That is the whole
  data-layer guarantee and the spec claims nothing beyond it.
- **The server config boundary, from `InstrumentDef`**, validates what
  `SizeGrid` cannot see: `size_precision`, and the price-side
  `price_decimals` for the book's own prices.

Expanding `SizeGrid` to carry precision was considered and rejected for this
landing: it is a shared type on the trade path, widening it would put a new field
in front of every existing construction site, and the config boundary already
holds the `InstrumentDef` that answers the question. If a later item needs
precision inside `mogwai-data` for another reason, that is when `SizeGrid` grows.

Section 6 asserts no golden target and no realism-gate band reads a quote-derived
quantity while any preset is `Uncalibrated`, so a placeholder cannot leak into a
fidelity claim.

### 2.5 Both calibration seams are real scalars

Revision 1 added `quoted_width` and `top_sizes` and left the displacement as a
module constant. That would have shipped a landing that claims to provide the
fitting seam for two observables while providing it for one.

`GeneratorScalars` gains all three:

```rust
pub quoted_width: QuotedWidth,
pub top_sizes: TopOfBookSizes,
pub trade_displacement_ticks: TradeDisplacement,
```

`TradeDisplacement` wraps an `f64`, validated non-negative and finite, and
carries a `CalibrationProvenance`. `BounceState::trade_bounce_ticks`
initializes from the scalar instead of from `TRADE_BOUNCE_HALF_WIDTH_TICKS`, and
the constant becomes the documented DEFAULT that presets start from rather than
the value the generator reads. Shipping it at 0.5 does not make it calibrated,
which is what the provenance says.

### 2.6 Quote construction draws no randomness

Once the state transitions of section 1.3 have occurred, placing the book is a
pure function of `(drifted mid, tick, width, sizes)`.

This is a hard requirement. The checkpointed seek, the byte-identical golden and
every cross-run comparison rest on the RNG stream being a function of the event
sequence alone. The future width-transition model WILL need draws, and it has to
be introduced knowing it is changing this.

**The test must point at the emission path, not at the pure function.** A test
that hands `place_book` no RNG and observes no draw is vacuous - it could not
have drawn. Section 6 pins it at the source level instead.

### 2.7 A repeat parent freezes the book

`EVENT_PRICE_REPEAT_PROB = 0.8` means a single-child low-regime event re-prints
the previous event's last price. Its own documentation says what it models: a
venue whose top of book did not move between two small takes.

Publishing a freshly placed book while repeating only the old trade would
contradict that mechanism outright, and would do something worse than widen a
spread - it can put a buyer-initiated print BELOW the published bid, or a
seller-initiated print ABOVE the ask. A trade outside the quote explicitly
declared to govern it is not a wide effective spread; it is an inconsistent
market.

So on a repeat parent:

- Reuse the previous bid, ask and sizes exactly.
- Emit that book again, with the NEW parent timestamp.
- Repeat the prior parent level as today.
- Let the latent mid and the drift advance internally, so the next non-repeat
  event catches up to wherever the state has walked.

**Freezing the book is necessary but NOT sufficient, and this is the subtlety
that would otherwise be discovered during implementation.** The repeat branch
reuses the previous BURST's last price, while the aggressor side for the new
event has already been drawn independently and may have flipped. The previous
burst may also have carried several children and walked several ticks from its
own governing book. So:

```
previous seller sweep ends below the bid
new event draws Buyer
repeat reuses that low price under the frozen book
```

is a buyer-initiated print below the published bid - exactly the inconsistency
the freeze exists to prevent. The freeze fixes the book; it does not fix the
label.

Repeat acceptance is therefore conditional, evaluated AFTER the repeat draw so
the draw order of section 1.3 is untouched:

```
repeat = repeat_draw && previous_price_is_compatible_with(new_aggressor, previous_book)

Buyer  => repeated_price >= book_mid_ticks(previous_book)
Seller => repeated_price <= book_mid_ticks(previous_book)
```

Compatibility is defined against the book MIDPOINT rather than the opposite
touch. The midpoint is the referent every other quantity in this spec is measured
from, and it is the weaker of the two conditions, so it rejects only repetitions
whose aggressor label actually contradicts their price rather than also rejecting
legitimate prints inside the spread.

When compatibility fails the event prices normally off the fresh book, and the
freshly placed book is published rather than discarded.

**This changes realized statistics and the change is not estimated here.** The
realized repeat rate falls below `EVENT_PRICE_REPEAT_PROB`, and `zero_change_frac`
moves with it. Both are MEASURED at the protocol-7 re-bless (brick 3) and the
realism-gate band for `zero_change_frac` is re-read against the measurement, not
adjusted to preserve the old number.

Some coupling between repetition and the observable book is unavoidable once the
previously implicit market state becomes observable, and the later joint
calibration decides whether repetition survives as a separate mechanism.

**The keep/revert rule, so the open question does not sit across a landing
boundary.** Every boundary must be green, so "a finding awaiting a decision"
cannot be a state landing B is allowed to be in:

- `zero_change_frac` outside the committed band STOPS and reverts landing B.
- The measurement is recorded regardless.
- The band is NOT re-blessed and the mechanism is NOT tuned inside this landing.
  Either would be fitting the model to keep a gate green, which is the failure
  mode the band exists to prevent.
- Whether the band or the mechanism is wrong is decided separately, and whatever
  that decision produces lands as its own change with its own evidence.

**This does not reorder a single draw.** The repeat decision is drawn after the
count, which is after the side and drift - exactly as today (section 1.3). The
compatibility test consumes no randomness and is evaluated after the draw. Book
placement is PURE, so it is computed at its natural point and simply DISCARDED
when the repeat is both drawn AND accepted, with the frozen prior book published
instead. Draw order: side, drift, count, repeat. Wire order: quote, then burst.

The generator carries `last_book: Option<PublishedBook>` for this, and it is a
plain `Clone` field - see section 5.1.

## 3. The emission contract

Per parent event:

1. **Advance latent state.** `next_latent_mid` steps the GARCH recursion and the
   session and regime multipliers.
2. **Draw the aggressor side**, then **step the drift**. Unchanged.
3. **Place the book** from `latent_mid / tick + drift_ticks`, per section 2.2.
   Pure; no draw.
4. **Draw the child count**, then **the repeat decision** if it applies.
   Unchanged.
5. **Select the book to publish**: the freshly placed one, or - on a repeat
   parent - the frozen previous one, per section 2.7. Emit `TickEvent::Quote`
   with `ts_event` equal to the parent event's timestamp.
6. **Emit the burst's trades** against that published book. Every print in the
   burst, parent and all sweep children, is displaced from the SAME published
   book midpoint. Children walk the price grid as today; the walk is a property
   of the prints and does not republish the book.

Equal timestamps keep quote-before-trade order: the quote's `ts_event` equals the
parent print's, and `INTRA_EVENT_STEP_NS` spacing puts children strictly after. A
consumer sorting only on timestamp must still see the quote first, which is why
section 6 asserts EMISSION order rather than timestamp order.

**Children do not republish the book**, and the consequence is stated rather than
tuned away: during a sweep, later children print several ticks from a book that
is no longer updating, so measured against the published BBO they carry a large
effective spread. That is a faithful consequence of a per-event quote layer and
exactly the kind of divergence this venue exists to express. Brick 6 reports the
within-burst distribution; it does not bound it.

## 4. The connect-time snapshot

### 4.1 The race revision 1 missed

Revision 1 specified: subscribe, then read `last_quote`, then send. It argued the
snapshot precedes every frame the subscriber will receive. That is false. The
read happens an unbounded time after the subscribe, and the tape can publish in
between:

```
publish T1 ... [subscribe] ... publish T2 ... publish Q2 ... [read last_quote]
```

The receiver has `T2` and `Q2` queued; the read returns `Q2`; the feed sends `Q2`
as a snapshot and then relays `T2`, whose `ts_event` is OLDER. That is a backward
step in a stream the venue promises ascending - the condition `ws.rs` already
kills a connection over. Revision 1's nondecreasing argument was wrong.

Compounding it: `TapeFrame` is `{ payload: Arc<str> }` and carries no timestamp,
so a `Mutex<Option<TapeFrame>>` as specified could not even compare.

### 4.2 The fix: make subscription and snapshot capture atomic

```rust
pub(crate) struct TapeFrame {
    pub(crate) payload: Arc<str>,
    /// The frame's `ts_event`, so ordering decisions never re-parse the JSON.
    pub(crate) ts_event: u64,
}

impl Tape {
    /// Subscribe and capture the current book in one critical section, so the
    /// snapshot cannot advance past frames already queued for this receiver.
    pub(crate) fn subscribe_with_snapshot(
        &self,
    ) -> (broadcast::Receiver<TapeFrame>, Option<TapeFrame>) { .. }
}
```

One mutex guards the published-market state. The subscriber takes the lock,
creates the receiver, clones the current quote frame, and releases. The tape
thread takes the SAME lock while it updates `last_quote` and broadcasts a quote.
Trades do not take it, so the hot path is untouched for the common frame.

With publication and subscription serialized on one lock, no quote can be
published between the receiver's creation and the snapshot's capture, so the
snapshot is the last quote at or before every frame this receiver will see.

**The test must force the interleaving.** A test named
`the_snapshot_never_precedes_the_subscribe` asserts the order of two statements
in a source file and would have passed against the broken design. Brick 5's test
is `a_concurrent_publish_cannot_advance_the_snapshot_past_a_queued_frame`, using
barriers to park a subscriber mid-operation while the tape publishes, and
asserting the delivered sequence is nondecreasing in `ts_event`.

At most one duplicate remains possible - the snapshot and the same frame from the
ring - and a duplicate BBO is idempotent because it carries absolute prices and
sizes, not a delta.

### 4.3 The snapshot must survive the adapter

`client/data.rs:1076` dispatches a quote only when `s.quotes > 0`. A nautilus
host's `subscribe_quotes` normally arrives AFTER the WS connect, so the
connect-time snapshot is silently discarded and the host waits for the next
generated quote anyway. A wire-level test would pass while the actual requirement
fails end to end.

The adapter therefore retains the most recent `ServerMessage::Quote` per symbol
and replays it on `subscribe_quotes`, before any live quote. This is the same
argument as section 4.2 one layer up: the guarantee is "a subscriber sees a book
before its first trade", and the subscriber that matters is the host.

## 5. Determinism, budgets and the protocol version

### 5.1 Version and state

`TAPE_PROTOCOL_VERSION` goes 6 to 7. Three independent reasons, any sufficient:
the stream gains quote events; the displacement is re-referenced to the book
midpoint; `ceil`/`floor` becomes a single `round`.

**The pending-event state machine.** `next_tick` returns exactly one event per
call, so emitting a quote before a burst requires the source to hold a pending
quote and return it first. This is a new field:

```rust
pending_quote: Option<QuoteTick>,
last_book: Option<PublishedBook>,
```

Both are plain `Clone` fields, so `CheckpointIndex` needs no new snapshot state:
it snapshots the whole source by `Clone`. Its extension API did gain a bounded
chunk operation for the post-implementation reach/backstop separation above.
Section 6 tests a resumed stream rather than resting on that.

**Golden re-bless.** `clean_regime_is_byte_identical` pins a Debug-formatted
prefix. Quotes interleave and some prices shift, so it is regenerated - with the
new stream read and sanity-checked (a quote before the first trade, one quote per
parent event, prints on the touch at `w = 1`, a frozen book across a repeat), not
mechanically pasted.

### 5.2 The tick budgets

`CHECKPOINT_K` (262144), `SWEEP_DRAIN_BUDGET` (5000000) and the cumulative warmup reach
are counted in TICKS. Adding one quote per parent event changes mean ticks per
parent from `C` to `C + 1`, an inflation factor of

```
(C + 1) / C = 1 + 1/C
```

so every budget's reach in SIM TIME shrinks by that factor: each checkpoint spans
less time, `vol_reading` and `scan_triggers` cover a shorter window before
refusing on budget, and the "drained more than half its budget" warning fires at
a lower real load. The tape ring carries proportionally more frames, so
`FeedLagged` - a connection-killing venue fault - becomes likelier at the same
`fanout_depth`.

**The resize is measured, not derived.** `children_mean` is a declared
unconditional mean, and realized child counts are state-conditioned (quiet versus
active arrival) and can be scaled by an armed `FlowSurge`. Brick 7 measures the
protocol-7 quote-to-trade composition across every shipped preset and sizes the
budgets from a conservative tail of the worst preset, not from `children_mean`.
The measurement lands before the constants change.

## 6. The bricks, in landing order

Each brick names its gate and the exact command.

**The landing sequence, explicitly.** Brick 0 is its own landing precisely
because its repairs are behavior-preserving and must be reviewable against a
green suite with no tape change confounding them.

| Landing | Bricks | Protocol | Character |
|---|---|---|---|
| A | 0 | stays 6 | behavior-preserving repair of the trades-only assumption, plus brick 7's instrument and its committed protocol-6 baseline |
| B | 1 - 5 | 6 to 7 | the tape change and everything that keeps it honest |
| C | 6 | 7 | the decomposition report at the new referent |
| D | 7 | 7 | the measured budget resize |
| E | 8 | 7 | documents, riding with C or D per the commit rules |

The suite is green at every boundary. Landing A cannot be folded into B without
losing the property that makes it worth separating; landings C and D cannot
precede B, because both measure a protocol-7 stream.

### Brick 0: make the trades-only assumption findable

Before anything emits a quote, every site that consumes a `TickEvent` is
inventoried and repaired. This is a brick rather than a preamble because
revision 1's survey missed three sites and two fail silently.

- `source.rs:276` `last_trade_at_or_before`: `while let Some(tick) = ...` with an
  inner `let ... else { continue }`.
- `gen.rs:130`: `continue`-semantics, not a terminating match arm.
- `fills.rs:267`: consume quotes in the helper rather than panic.
- Every remaining `match` on `TickEvent`, and every loop that requests a fixed
  number of source events while expecting trades, audited and listed in the
  commit message.

The repairs are behavior-preserving TODAY - no quote exists yet - which is
exactly why they land first: the suite stays green at this boundary and the
change is reviewable in isolation.

**Landing A also carries brick 7's instrument**, the `mogwai tick-composition`
subcommand,
and commits its protocol-6 output as `analysis/tick-composition-protocol-6.json`.
It has to: after landing B the tape is protocol 7 and there is no switch back, so
a baseline not captured here cannot be captured at all. See brick 7.

Gate: `brokkr check`, plus a test that drives each repaired site against a
hand-built stream with quotes interleaved, so the repair is proven rather than
asserted. That test is written here and outlives the landing. Plus the baseline
run itself:

```
brokkr run mogwai -- tick-composition --out-6 analysis/tick-composition-protocol-6.json --out-7 analysis/tick-composition-protocol-7.json
```

### Brick 1: the quote model types

`crates/mogwai-data/src/generated/quote.rs`, new module: `QuotedWidth`,
`TopOfBookSizes`, `CalibrationProvenance`, `TradeDisplacement`, `PublishedBook`,
`place_book`, `book_mid_ticks`.

Gate: `brokkr check -p mogwai-data`, plus:

- `book_width_is_exact_at_every_phase` - `w` in 1..=8 over 64 phases.
- `book_mid_tracks_the_drifted_mid_within_half_a_tick`, which also asserts the
  floor is not reached.
- `odd_widths_straddle_a_tick_and_even_widths_sit_on_one`.
- `the_one_tick_floor_preserves_the_width`.

### Brick 2: per-instrument scalars

`GeneratorScalars` gains `quoted_width`, `top_sizes` and
`trade_displacement_ticks`. Presets gain the fields. Width admission is
structural per section 2.2; size validation lives at the server config boundary
per section 2.4.

Gate: `brokkr check`, plus:

- `every_shipped_preset_quotes_a_positive_integral_width`.
- `no_shipped_preset_claims_a_fitted_quote_quantity` - covers all THREE
  (`quoted_width`, `top_sizes`, `trade_displacement_ticks`), so adding a fourth
  calibrated quantity without a corpus fails here.
- `quote_sizes_are_on_the_instrument_grid` - drives a futures instrument whose
  latent median is fractional and asserts the placeholder is the minimum size and
  is integral, which is the case revision 1 got wrong.

### Brick 3: emission

Quote synthesis, the pending-event machine, the frozen book on repeat, the
displacement re-referenced, `ceil`/`floor` replaced by `round`,
`TAPE_PROTOCOL_VERSION` to 7, golden re-blessed.

Gate: `brokkr check`, plus:

- `clean_regime_is_byte_identical`, re-blessed and inspected.
- `a_quote_precedes_every_parent_burst` - 20,000 events, EMISSION order.
- `every_trade_has_a_governing_quote_at_or_before_it`.
- `quote_timestamps_are_nondecreasing`.
- `non_repeat_parents_print_at_the_touch_at_unit_width`.
- `a_repeat_parent_republishes_the_frozen_book` - asserts bid, ask and sizes are
  identical to the previous publication and the timestamp is the new one.
- `an_incompatible_repeat_is_rejected` - constructs the section 2.7 case (a
  seller sweep ending below the bid, followed by a Buyer draw) and asserts the
  event prices off the FRESH book and publishes it, rather than repeating.
- `the_realized_repeat_rate_and_zero_change_frac_are_recorded` - measures both at
  the re-bless and reports them against the fingerprint band, failing only if
  `zero_change_frac` leaves the band, so that a departure is a finding rather
  than a silent drift.
- `no_parent_print_crosses_the_wrong_side_of_its_governing_book` - the invariant
  section 2.7 exists to protect: no buyer-initiated PARENT print below the
  published bid, no seller-initiated parent print above the published ask. Named
  for what it actually asserts: sweep children are exempt by design (section 3
  admits them), so a name claiming every print would overclaim.
- `realized_displacement_changes_only_at_the_grid_boundaries` - the
  non-injectivity of section 2.3, asserted as the set of distinct printed levels
  across a sweep of `d`.
- `width_and_displacement_are_independently_settable`, written in terms of
  realized levels.
- `quote_construction_consumes_no_randomness` - at the SOURCE level: clone a
  source, drive extra book placements on the clone, assert byte-identical
  subsequent trade streams.
- `a_resumed_source_reproduces_quotes_identically`.

`the_generator_publishes_no_quotes` fails here by design and is rewritten into
`a_quote_precedes_every_parent_burst`. `the_trade_displacement_never_varies`
still passes: this re-references the displacement, it does not make it dynamic.

### Brick 4: the history route

`quotes()` grows the same seek-and-bound scan as `bounded_trades`.

**The window-boundary obstacle, resolved inline.** The tape-order invariant is a
property of the FULL stream and does not survive an arbitrary bounded slice: a
window starting mid-burst holds trades whose governing quote predates `start`.
The resolution is NOT to synthesize a leading quote, which would put a frame in
the response the tape never published at that timestamp. It is to pin the route's
semantics: `/quotes` returns quotes with `ts_event` in `[start, end]` and nothing
else. A consumer wanting the governing quote asks for an earlier `start`. This is
acceptable because the host does not use this route - it consumes the WS stream,
where section 4 guarantees a book before the first trade. `docs/` records it.

Gate: `brokkr check -p mogwai-server`, plus:

- `bounded_quotes_reproduce_the_live_quote_sequence` - proves section 1.2.
- `bounded_quotes_respects_the_window_and_the_limit`.
- `the_quotes_route_is_no_longer_empty`.
- The adapter's `request_quotes` / `QuotesResponse` path, live for the first
  time, gated by a test in the socket-backed adapter binaries.

### Brick 5: the snapshot, at both layers

`TapeFrame::ts_event`, `subscribe_with_snapshot` under one lock, the feed task,
and the adapter's activation and replay.

**The adapter is activating dead code, so the conversion changes with it.**
`convert::quote_tick` calls the panicking `NautilusQuoteTick::new`. The trade
path was deliberately moved to `new_checked` because the task running it has no
supervisor, and the same argument applies the moment this function becomes live.
Brick 5 switches it to `new_checked` and covers the rejection with a condition
that is ACTUALLY rejected.

**The obvious test would not test anything.** `convert::price` and
`convert::quantity` build both sides at the instrument's DECLARED precision:
`decimal_to_f64` then `Price::new_checked(f64, precision)`. An over-scaled
`Decimal` is therefore quantized on the way through, not refused - the scale of
the wire value is never compared to the instrument's precision. And
`QuoteTick::new_checked` verifies that the bid and ask precisions AGREE, which
they do by construction here, since both come from `def.price_precision`.

So the adapter-level test uses a genuinely rejected condition: a magnitude past
the fixed-point range, or a declared `price_precision` the fixed-point
representation does not support - the same conditions the existing
`price_rejects_precision_beyond_fixed_precision` already pins for the trade path.
It asserts the quote is dropped with a warning and the data task survives.

**Off-grid prices and sizes are a different test in a different place.** Grid
compatibility is enforced at generation and at config validation (section 2.4),
not in the adapter, which sees an already-well-formed wire value. Brick 2 carries
`quote_sizes_are_on_the_instrument_grid`; brick 3 adds the price twin,
`published_book_prices_are_on_the_tick_grid`. Testing grid compatibility at the
adapter would assert a property the adapter does not enforce and cannot.

**The adapter cache has exact ownership and ordering, because otherwise the
server race of section 4.2 is repaired while the identical race one layer up
stays open:**

- Cache the WIRE `QuoteTick`, before subscription filtering. A quote arriving
  while the host is not subscribed is still the current book.
- Retaining a quote must NOT require an `InstrumentDef`. Today the handler
  returns early on a missing definition; the cache is written before that gate,
  so an instrument that resolves later still has a book to replay.
- `subscribe_quotes` atomically enables delivery AND takes the cached quote,
  under the same lock live dispatch takes, so a newer live quote cannot be
  delivered ahead of an older replay.
- The replay converts and emits through the SAME function as a live quote. A
  second conversion path is a second set of bugs, and it is how a replayed book
  would come to differ from a live one.

Gate: `brokkr check --gate` - the plain check cannot see the four socket-backed
adapter test binaries, and two regressions have already shipped red through that
gap - plus:

- `a_concurrent_publish_cannot_advance_the_snapshot_past_a_queued_frame`, with
  barriers, per section 4.2.
- `a_subscriber_sees_a_bbo_before_its_first_trade`.
- `a_host_subscribing_quotes_after_connect_receives_the_book_immediately` - the
  end-to-end requirement of section 4.3, which the wire-level test cannot see.
- `a_live_quote_cannot_overtake_the_replayed_book` - the adapter twin of the
  barrier test above, forcing a live quote to arrive concurrently with
  `subscribe_quotes`.
- `a_quote_cached_before_its_instrument_resolves_is_still_replayed`.
- `an_unrepresentable_quote_is_dropped_not_panicked` - the `new_checked` change,
  driven by an out-of-range magnitude or an unsupported declared precision, NOT
  by an over-scaled decimal, which is quantized rather than rejected.

And the live path, which exercises the adapter's never-run conversion:

```
brokkr run mogwai -- serve
python3 scripts/smoke.py
```

### Brick 6: the decomposition schema follows the referent

`synthetic_spread_decomposition_at_protocol_six` becomes `..._at_protocol_seven`:

- `mid_relative_displacement_at_*` measured against the PUBLISHED book midpoint
  is a genuine effective spread, comparable to the real-data matrix, and renamed.
- The same quantity against the latent mid is retained alongside, because the
  gap between them is now a measurable model property.
- `configured_quoted_width_ticks` joins the configured separation.
- The quote-age label goes from `no_model_quote` to
  `contemporaneous_model_quote`, which is now true. `tests.rs` carries a comment
  saying to make exactly this change at exactly this point.
- Within-burst effective spread against the frozen book, reported not bounded.
- Repeated versus non-repeated parents reported SEPARATELY, per section 2.7.

Gate: `brokkr test -p mogwai-data synthetic_spread_decomposition_at_protocol_seven`,
read for sanity rather than asserted - it is a report, and its numbers are input
to the purchase decision, not a gate on it.

### Brick 7: the budget resize

"A conservative tail of the worst preset" does not tell two implementers which
numbers to write. The measurement lands first, but the POLICY that converts its
output into constants is fixed here, before any number is read.

**Measurement.** For each of the five shipped presets, and for each seed in
`1..=8`, generate `2_000_000` parent events at protocol 7 under four
configurations:

- `quiet` - `ArrivalClock` forced to the quiet state for the whole run.
- `active` - forced active.
- `natural` - the unforced two-state chain.
- `surged` - `natural` with a `FlowSurge` armed for the whole run at the maximum
  `rate_mult` and `children_mult` the control plane admits.

**`ticks_per_parent` is the WRONG statistic for three of the four constants, and
sizing them off it would miss the case that actually exhausts them.** It captures
quote inflation and nothing else. A maximum `FlowSurge::rate_mult` shortens the
gaps BETWEEN parents without changing ticks per parent at all, so it can drain
`SWEEP_DRAIN_BUDGET`, exhaust the cumulative warmup reach and overrun `fanout_depth` while
the ratio reads 1.0. Every budget except the composition record is denominated in
time or in wall rate, so the measurement must be too:

| Quantity | Statistic |
|---|---|
| composition (the record, not a sizing input) | `ticks_per_parent` |
| `SWEEP_DRAIN_BUDGET` | ticks observed within one `VOL_WINDOW_NS` (300 s) window |
| `CHECKPOINT_K` | ticks per simulated second |
| `MAX_WARMUP_MATERIALIZATION_TICKS` | ticks within the longest warmup any preset configures |
| `fanout_depth` | peak frames per WALL second, per the named speed policy below |

`fanout_depth` is the one that is not a simulated-time quantity: the ring buffers
against a slow consumer in real time, so its measurement runs under a named speed
policy rather than at generation rate.

**Ratios are paired before they are maximised.** For each quantity, compute the
protocol-7 over protocol-6 ratio WITHIN each preset-seed-configuration
combination, then take the worst ratio across combinations. Taking independent
maxima of the two protocols would divide a protocol-7 reading from one
combination by a protocol-6 reading from another and call the quotient an
inflation factor. The statistic within a combination is the p99.9; the
aggregation across combinations is the maximum. A single pathological event must
not set a global constant, but a pathological PRESET must, because a shipped
preset is a supported configuration.

**One formula does not size four constants.** They serve different purposes and
their measurements are different quantities, so each is sized from ITS OWN paired
ratio `R_x`:

```
new_x = ceil_to_rule(old_x * R_x * 2.0)
```

with `2.0` the headroom multiplier. Only if the four measured `R_x` agree to
within the rounding step may a single ratio be used for all of them, and that
agreement is a RESULT to be reported, not an assumption to start from.

Required reach, checked against the computed result rather than merely assumed
by it:

- `SWEEP_DRAIN_BUDGET` must cover a full `VOL_WINDOW_NS` at the worst measured
  in-window tick count, since `vol_reading` refusing on budget is what makes the
  fill band fall back.
- `MAX_WARMUP_MATERIALIZATION_TICKS` must reach the longest warmup any shipped preset configures,
  at the worst measured rate.
- `fanout_depth` must hold at least the same number of WALL SECONDS of tape it
  holds today at 65,536 frames.

**`CHECKPOINT_K` preserves its EXISTING contract, and that contract is not a fill
horizon.** An earlier revision required it to span at least `FILL_HORIZON_NS`,
which was invented here and is wrong twice over: "at least 60 s" does not bound a
miss at 60 s, and the real contract in `source.rs` is different and already
written down - 262,144 ticks is about 88 simulated minutes at the default
raw-fill rate, it bounds residual seek replay to that many ticks, and it delays
checkpoint coarsening to roughly 250 simulated days.

So the requirement is to PRESERVE the protocol-6 simulated-time spacing under the
higher frame rate: scale `CHECKPOINT_K` by its own `R_x` so the interval stays at
about 88 simulated minutes. Doing so also preserves the coarsening horizon, since
both the spacing and the ticks-per-simulated-day scale by the same factor.

The accepted cost, stated as a bound rather than discovered: residual tick work
per checkpoint miss rises by exactly `R_checkpoint`, because the residual is
bounded by `CHECKPOINT_K` ticks and that number is what grew. Changing the
88-minute spacing is a separate decision with its own reasons, and this brick
does not make it.

**Rounding.** `CHECKPOINT_K` rounds up to the next power of two. Nothing in the
chain arithmetic requires that, but the constant has been a power of two since it
was introduced and keeping the property costs nothing. `SWEEP_DRAIN_BUDGET` and
`MAX_WARMUP_MATERIALIZATION_TICKS` round up to the next multiple of 1,000,000. `fanout_depth`
rounds up to the next power of two, matching its 65,536 default. Rounding is
always up; a budget rounded down is a refusal.

**`fanout_depth` preserves a WALL-TIME HORIZON, and its speed policy is named.**
The ring buffers against a slow consumer in real time, so what is held constant
is how many wall seconds of tape a subscriber may fall behind before `FeedLagged`
kills the connection. "The supported speed policy" is not an executable input, so:

- Speeds measured: `1.0` and `10.0`. Both are supported accelerations and the
  worst frames-per-wall-second across them is the sizing input.
- `speed == 0.0` is EXCLUDED, and the exclusion is a fact about the mechanism
  rather than a simplification: an unpaced tape is governed by `await_headroom`,
  which parks the producer while the ring is more than half full, so its
  occupancy is bounded by the park and not by the frame rate. Sizing the ring off
  a firehose would size it for a case the ring is not what protects.
- Simulated position: the preset's own session profile, with the run positioned
  at that preset's HIGHEST-intensity session hour, since a ring sized off a
  quiet hour is sized for the case that never overflows it.
- Wall-time sampling horizon: 600 wall seconds per speed, with the statistic the
  peak frames-per-wall-second over any one-second window in that run.

**Proceed/close threshold, evaluated PER CONSTANT.** There is no single `S`
any more, so there is no single threshold:

- A constant whose own `R_x >= 1.05` is resized.
- A constant whose own `R_x < 1.05` is left alone, and the measurement recorded.
- Brick 7 closes as mispriced only if EVERY `R_x < 1.05`.

The alternative - closing the whole brick when any one ratio is small - would
leave a genuinely inflated budget unresized because an unrelated one happened not
to move.

### Brick 7's instrument, which lands in Landing A

The specification contract requires the measurement to be a brick of its own,
built before the brick it gates. It also has to run on protocol 6, and after
landing B that is no longer possible by flipping a constant: the quote layer is
not switchable, by section 7's own rule.

So the instrument and its protocol-6 baseline land in **Landing A**, while the
tape is still protocol 6:

- `crates/mogwai-server/src/tick_composition.rs`, the `mogwai tick-composition`
  subcommand, driving all 160 preset-seed-configuration combinations and every
  statistic in the table above. It sits in the server beside `gen` rather than
  in a `mogwai-data` example because each preset must be measured through the
  profile the venue boots - preset inheritance, size grid, session profile and
  calendar - and that resolution is `config::profile_from_preset`, unreachable
  from a lower crate.
- Output: one JSON document per protocol version, keyed by
  `(preset, seed, configuration)`, each entry carrying `ticks_per_parent`,
  `ticks_per_vol_window`, `ticks_per_sim_second`, `ticks_per_warmup` and
  `frames_per_wall_second` at each measured speed, with the p99.9 and the max for
  every one.
- The protocol-6 run is committed as
  `analysis/tick-composition-protocol-6.json`. It is a FIXTURE, not a cache: it
  is the only way the ratio can be computed after landing B, and regenerating it
  later would silently measure a different tape.

Exact command:

```
brokkr run mogwai -- tick-composition --out-6 analysis/tick-composition-protocol-6.json --out-7 analysis/tick-composition-protocol-7.json
```

One invocation emits both fixtures. Protocol 6 is a count projection of the
protocol-7 tape - quote placement consumes no randomness and moves neither
timestamps nor child counts - so a separate protocol-6 pass would rebuild an
identical stream to count a subset of it. Both counter sets ride one traversal,
which pairs the fixtures by construction. `--jobs N` sets the worker count and
defaults to the machine's parallelism; `--parents N` sets the per-combination
sample size. The ratios are computed by pairing the two files on
`(preset, seed, configuration)` and taking the worst ratio per statistic, per
section above - never by comparing aggregates across files.

Gate: `brokkr check --gate`, plus `reference/performance.md` updated with the
measured composition, the per-constant ratios, and the held-constant horizons -
88 simulated minutes for `CHECKPOINT_K` and the measured wall seconds for
`fanout_depth` - since that file is the durable record of measured numbers over
time.

### Brick 8: the documents

Report sections 3.5, 11 step 5c and 14.4 move item B to landed.
`reference/architecture.md` gains the quote layer. `docs/` gains the `/quotes`
window semantics. Rides with brick 6 or 7 per the commit rules.

## 7. Keep/revert

Each landing is kept or reverted on its own gates, separately.

**Landing A (brick 0).** Revert signal: any behavior change at all. These repairs
are behavior-preserving by construction - no quote exists yet - so a moved test
result means the repair changed something it should not have. A is independently
valuable and stays landed even if B is reverted: `last_trade_at_or_before` and
`gen.rs` are latent bugs today, waiting for any future non-trade event.

**Landing B (bricks 1 - 5).** One coherent change, reverted whole. Revert signal:
a gate failure in brick 3 or brick 5. No gated probe and no environment-variable
switch: a venue that publishes quotes only when a flag is set is two venues, and
the divergence between them is exactly the class of thing this project exists to
catch in other people's systems.

**Landing C (brick 6).** Cannot be reverted for a surprising number - the
decomposition is a report, and a surprise in it is the report working. It is
reverted only if it fails to run or measures the wrong quantity.

**Landing D (brick 7).** Governed by its own proceed/close threshold, stated in
the brick: below it, the resize is never laid and D closes as mispriced.

## 8. Stopping rule

The teardown stops at the generator's event loop, the server's feed task and the
adapter's quote dispatch.

Explicitly NOT touched:

- **The engine's fill SEMANTICS.** `fill_band_vol_mult` stays calibrated to its
  internal usability window; anchoring it to the now-published spread is a real
  question, recorded in report section 14.4, and is a different item. Note the
  correction to revision 1: the fill path IS touched, at
  `last_trade_at_or_before`, which brick 0 repairs. What is untouched is the
  band's calibration, not the code path.
- The divergence injection seam. No new divergence is armed against the book.
- `KrakenCsvSource` and the offline lineage. The corpus has no quotes.
- The width transition model and any volatility response, per section 0.

## 9. What earlier revisions got wrong

Kept because this workstream has now produced the same class of error at every
revision, and the record is the only thing that makes the pattern visible. Every
entry below corresponds to a concrete gate in section 6 rather than to a
retrospective narrative: the list is useful only because each line names
something a test now catches.

### Revision 1

1. **"Downstream consumers already filter."** Generalized from `trigger.rs` and
   `bounded_trades` to a population of five-plus sites. Three break, two
   silently, one of them in the order-admission path.
2. **The snapshot ordering argument.** Claimed the snapshot precedes every queued
   frame. The read is not atomic with the subscribe, so it does not. The proposed
   test would have passed against the broken design.
3. **"`latent_size_median` cannot produce a malformed `QuoteTick`."** Confused a
   pre-quantization latent parameter with a published size - the same confusion,
   in the same field, that the `typical_notional` work already corrected once.
4. **Only one calibration seam.** Claimed to provide the fitting seam for two
   observables while leaving the displacement a module constant.
5. **The drift was contradicted three ways** across sections 2.2, 3 and 2.3, and
   the contradiction was invisible because no section stated the existing draw
   order. Section 1.3 now does.
6. **The repeat branch was never mentioned**, so the emission contract's central
   claim was false for the majority of parent prints and brick 3's key gate could
   not have passed.
7. **`ceil`/`floor` was criticised as a pattern without naming the code that
   implements it**, leaving the replacement implicit.
8. **The tick budgets were not considered at all**, and the correction to the
   first attempt at the arithmetic was also wrong: the inflation factor is
   `1 + 1/C`, not `1/(C + 1)`, which is the quote fraction of the new stream.

### Revision 2

9. **Freezing the book was treated as sufficient for repeat consistency.** It
   fixes the book and not the aggressor label: the repeat reuses the previous
   BURST's last price while the side is drawn independently, so a seller sweep
   ending below the bid followed by a Buyer draw prints below the published bid.
   The invariant revision 2 introduced would have failed against revision 2's own
   mechanism. Section 2.7 now carries the compatibility condition.
10. **The landing table contradicted the brick 0 rationale.** Section 6 still said
    "bricks 0 through 5 land as one commit" after brick 0 had been separated out
    for a stated reason. Two adjacent paragraphs disagreeing is what a reader
    trusts least.
11. **The adapter was surveyed and then not specified.** Revision 2 correctly
    noted that `convert::quote_tick` uses the panicking constructor and that the
    subscription gate defeats the snapshot, then laid no brick that changed
    either - it repaired the server-side race and left the identical adapter race
    open one layer up.
12. **Brick 7 had no decision rule.** "A conservative tail of the worst preset"
    is a sentiment, not a policy: it names no horizon, no seeds, no statistic, no
    headroom, no rounding and no proceed threshold, so two implementers reading
    it produce two different sets of constants. The measurement was specified as
    landing before the resize while the policy converting one into the other was
    left to be invented at implementation time - which is the exact deferral this
    document's contract forbids.
13. **`TradeDisplacement` was specified by resemblance.** "Its own provenance
    sharing `SizeProvenance`'s shape" is not a type. Displacement is not size.

### Revision 3

14. **Two paragraphs of section 2.7 contradicted each other**, one correctly
    saying compatibility rejection moves `zero_change_frac` and the next
    retaining revision 2's claim that it is unchanged. The second was a survivor
    of an edit that should have removed it. A document that says both things says
    neither.
15. **The open `zero_change_frac` question was left straddling a landing
    boundary.** Brick 3 failed the gate while the prose called the same condition
    a finding awaiting a decision. Every boundary must be green, so "awaiting a
    decision" was not a state landing B could occupy. Section 2.7 now reverts.
16. **`CalibrationProvenance` claimed to cover width, and width could not carry
    it.** `QuotedWidth(NonZeroU32)` has nowhere to put a provenance, so a preset
    could present its width as fitted or uncalibrated only in prose - which is
    exactly the gap the type was introduced to close for the other two.
17. **Brick 7 measured quote inflation and sized time budgets with it.**
    `ticks_per_parent` is blind to `FlowSurge::rate_mult`, which shortens
    inter-parent gaps and can exhaust three of the four constants while the
    statistic reads no change at all. The measurement now matches each
    constant's own denomination, ratios are paired within a combination before
    being maximised, and one formula no longer sizes four different-purpose
    constants unless the measurements prove it may.
18. **The malformed-quote test named a condition that is not rejected.**
    `convert::price` builds at the instrument's DECLARED precision, so an
    over-scaled decimal is quantized rather than refused, and
    `QuoteTick::new_checked` compares bid and ask precisions that are equal by
    construction. The test would have passed while proving nothing.
19. **`no_print_falls_outside_its_governing_book` overclaimed in its name**, since
    sweep children are exempt by design. A test name is read far more often than
    its body.

### Revision 4

20. **Per-constant ratios were introduced and the single-ratio language was left
    behind.** `fanout_depth` and the proceed threshold still referred to an `S`
    that the same brick had just abolished. The same edit-survivor failure as
    entry 14, in the same brick, one revision later.
21. **The `CHECKPOINT_K` reach rule was invented rather than read.** It required
    spanning "at least `FILL_HORIZON_NS`", which does not bound a miss at that
    horizon, and which ignored the contract already documented at
    `source.rs:126`: about 88 simulated minutes, residual replay bounded by the
    tick count, coarsening delayed to roughly 250 simulated days. A constant with
    a written contract was re-specified without reading the contract - the same
    move as trusting a name over behavior, one layer along.
22. **"The supported speed policy" was not an input.** It named no speed, no
    exclusion, no simulated position and no sampling horizon, so two implementers
    following the prose would size the ring differently.
23. **Brick 7 had no runnable artifact and no obtainable baseline.** The
    measurement was required before the resize while nothing named what ran it,
    and its protocol-6 arm was unobtainable after landing B, since section 7
    forbids exactly the switch that would have produced it. The instrument and
    its committed baseline now land in A.
24. **Section 2.4 asserted a data-layer guarantee the data layer cannot make**,
    saying `mogwai-data` rejects sizes off "the grid it was given" two paragraphs
    after saying `SizeGrid` does not carry what that would require.
