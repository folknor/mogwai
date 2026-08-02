# SPEC: stop-market and stop-limit, first class on the venue

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/problem-refused-order-types.md` (resolved as to its central questions)
and the PROBLEM STATEMENTS entry in `notes/todo.md` that owns it.

Revised after two independent reviews (`notes/spec-conditional-orders-review-1.md`,
`notes/spec-conditional-orders-review-2.md`); section 11 records what was folded
in and what was rejected.

**Build path.** As every spec touching these APIs must state: the implementer
READS the nautilus and broadarrow APIs from the in-tree copies under
`research/`, and BUILDS against the SIBLING checkout `../nautilus_trader`, which
`mogwai-adapter/Cargo.toml` path-depends on with default features off. The two
are kept in sync; `research/` is never a build input.

This is a capability rewrite of the venue's order surface. It replaces a
two-variant `OrderType` and a resting-order model that can only express "limit
waiting for a print through a price" with a four-variant type and a resting
model that distinguishes a LIVE order from an UNTRIGGERED conditional from an
INERT market remainder. The adapter stops refusing conditionals at conversion,
so a strategy whose protective leg is a stop becomes forward testable on the
only keyless venue `ba forward` can use.

It is not a bracket, not a trailing stop, and not an order list. The user ruled
those out; section 9 states exactly where the teardown stops.

## 1. The model, stated exactly

### 1.1 The four types

| Type | Carries `price` | Carries `trigger_price` | Rests as |
|---|---|---|---|
| `Market` | stamped by the server | never | never (fills or dies on arrival) |
| `Limit` | required | never | live limit under the fill band |
| `StopMarket` | never | required | untriggered conditional |
| `StopLimit` | required | required | untriggered conditional |

`price` on a `StopLimit` is the limit price the order takes AFTER it triggers.
A `StopMarket` carries no price at all: the server's price stamp is keyed on
`OrderType::Market` and stays keyed on it, because a stamped last print on a
stop would be a number nothing reads - the fill price comes from the print that
triggered it, and the reservation comes from the trigger price (section 1.6).
An order carrying a price the venue never consults is a field two implementers
will disagree about, so the venue refuses it.

### 1.2 The trigger predicate is TOUCH, not THROUGH

    touches_trigger(Buy,  trigger, traded) = traded >= trigger
    touches_trigger(Sell, trigger, traded) = traded <= trigger

A buy stop rests ABOVE the market and triggers when the tape reaches or passes
it upward; a sell stop rests BELOW and triggers when the tape reaches or passes
it downward. Note the sides are the mirror image of `trades_through`, which is
correct rather than confusing: a buy LIMIT is below the market and waits for the
tape to come DOWN to it, a buy STOP is above and waits for the tape to come UP.

Stated precisely, because `ScanKind::hit` puts the two side by side in one
match: for the SAME side and the SAME price argument the two predicates are
exact logical complements - `trades_through(Buy, p, t) = t < p` and
`touches_trigger(Buy, p, t) = t >= p`. They are never handed the same price in
practice (a limit is scanned against its DRAWN band trigger, a conditional
against its STATED stop), which is exactly why the two must not be collapsed
into one function with a strictness flag.

The venue already owns one predicate, `trades_through`, and it is deliberately
STRICT - "a print AT the trigger is touching, not trading through". That
strictness is a QUEUE argument: at your own limit price you are behind the
resting queue, so the tape merely reaching your price is not evidence that flow
reached YOU. A stop is not queued anywhere. Its trigger is a pure price
predicate the venue evaluates on its own book, and every real venue fires it on
touch. Reusing the strict predicate here would make a stop placed exactly at a
round number silently harder to trigger than the same stop one tick away, for a
reason that has no counterpart in the mechanism. So the venue carries TWO
predicates, each with one stated meaning, in one file, next to each other.

Both are trade predicates. There is no quote and there is not going to be one
(`problem-refused-order-types.md` decision 2), so `trigger_type` on the wire
would be a field with one legal value; the adapter refuses any nautilus
`TriggerType` other than `NoTrigger`, `Default` and `LastPrice` rather than
accepting a mark-price or bid-ask trigger and quietly serving a last-price one.
Silently honoring the wrong reference price is the class of lie the queue-ahead
refusal already rejected.

### 1.3 What a trigger produces

On a hit, the venue emits `OrderTriggered` and then, in the same batch:

- **StopMarket** becomes a market execution at the TRIGGERING PRINT slipped
  adversely by a draw from the same seeded band a market order uses:
  `fill_px = hit.px + increment * u` for a buy, `- ` for a sell, `u` drawn on
  `0 ..= band_ticks` from the order's own key with `band_draw` incremented. The
  triggering print, not a fresh `read_market` reading: the print is what made
  the order live, taking a second reading inside the sweep pass would cost a
  second synchronous walk per triggered order, and the two numbers would differ
  only by whatever the tape did between the print and the end of the pass -
  which is precisely the look-ahead the venue refuses everywhere else.
- **StopLimit** becomes a LIVE LIMIT at its stated price. It draws a fresh band
  trigger around that price (`band_draw` incremented, `band_ticks` unchanged -
  it is the band the order was accepted under, or the one its last price amend
  re-read), and is then judged EXACTLY as a limit submitted at that instant: if
  the triggering print is strictly through the drawn trigger it fills in full at
  its stated price right there; otherwise it rests and the sweep decides it.

  Judging it against the triggering print is not an optimization, it closes a
  hole. If a triggered stop-limit simply rested with its frontier at the end of
  the pass, the print that made it live would never be offered to it: a
  protective sell-stop at 100 with a limit at 99 that is triggered by a print at
  99.5 has, in that very print, a price its limit would have taken, and resting
  it with a frontier past that print discards the fill.

  **What this rule does NOT do, stated because a reviewer of an earlier draft of
  this document was misled by the earlier draft's own example: it does not fill a
  gapped stop-limit.** A sell stop at 100 with a limit at 99, triggered by a
  print at 95, does NOT fill - `trades_through(Sell, 99, 95)` is `95 > 99`,
  false - and it rests until the tape comes back to above 99, possibly forever.
  That is correct venue behavior and it is the textbook reason a stop-LIMIT is
  the dangerous protective leg through a gap; the venue must not "fix" it.
  Filling there would manufacture a price the market never offered, which is
  precisely the free money the fill band exists to remove.

The band's magnitude for a triggered stop is the LIMIT BAND, unchanged and
unrefitted. `problem-refused-order-types.md` records that price span per
inferred match event has never been measured and that the slippage magnitude is
therefore an unquantified mechanism. That is true and it is not a blocker here,
for a reason that must be stated rather than assumed: the identical objection
applies to the market-order slippage that ALREADY shipped on this same borrowed
multiplier (`a214996` and follow-ons). Introducing a second, separately fitted
number for triggered stops would add an unmeasured constant; reusing the one
already in the venue adds none. When the span measurement lands it moves one
config field and moves both paths together. The measurement stays a `notes/`
item, named in section 9.

### 1.4 Triggering on arrival

A stop whose `trigger_price` is already touched by the acceptance-time reading
triggers inside its own submit: `OrderAccepted`, `OrderTriggered`, then the fill
or the rest, in one batch. It is NOT rejected.

**The synthesized hit.** `on_trigger` takes a `Hit`, and at submit there is no
print - only a `MarketReading`. The arrival path therefore synthesizes
`Hit { px: reading.last_px, ts_ns: reading.ts_ns }`, the instant and price of
the reading's own last print, which IS a real print off the canonical tape and
not an invention. So the arrival-triggered stop-market fills off the
acceptance-time last price, slipped: that is the only evidence the venue has.
The landing-2 gate is worded accordingly (section 5) - the "never the
acceptance-time last price" clause belongs to the SWEPT case only, where the
triggering print is a later one, and asserting it on the arrival path would be
unsatisfiable.

**Which timestamp.** `OrderTriggered.ts_event` and `OpenOrder.ts_triggered` are
the APPLICATION instant `ts`, not `hit.ts_ns`. Every `ts_event` the engine emits
today - accepts, fills, cancels alike - is the `ts` handed to the call, and a
triggered fill and the trigger that produced it must not disagree about when
they happened. `hit.ts_ns` is carried for pricing and for the walk's own
ordering, and is deliberately not published; publishing it would make
`OrderTriggered` the one event in the protocol stamped on a different clock
reading from the fill it precedes in the same batch. The pass instant remains
what `scanned_ns` advances to.

nautilus' own simulated matching engine rejects a stop that is already in the
market. mogwai deliberately diverges, and the divergence is the point of the
venue: a strategy that submitted its protective leg a beat late, or whose leg
was held by an armed `DelayAcks` until the market had already run through it,
must end up protected-and-filled rather than unprotected-and-rejected. Rejecting
would hand the strategy the worst of both worlds - no position protection and no
fill - which no live venue does and which would make the fault injection this
venue exists for unreadable. Recorded here because an implementer comparing
against `research/nautilus_trader`'s backtest engine will otherwise "fix" it.

If the venue has NO reading at the submit instant (`read_market` refused - a
cold estimator or a truncated walk, which the standing `notes/todo.md` item
measures at roughly 30% of instants on the default profile) the stop simply
rests untriggered with `band_ticks = 0`, and the sweep decides it from its
acceptance instant forward. It cannot trigger on arrival because the venue has
no evidence it should, and the first sweep pass covers the span anyway. No stop
is lost by a refused reading.

### 1.5 Time in force

A stop must be `Gtc`. `Ioc` and `Fok` stops are rejected at submit with
"conditional orders are good-till-cancel only: a now-or-never order cannot
wait for a trigger". This is not a refused strategy shape - nautilus emits GTC
stops - it is the removal of a state with no meaning: an IOC stop would be
accepted and cancelled in the same breath, having never been capable of
triggering, and its `OrderCanceled` would read as a venue fault.

The TIF of the order a trigger PRODUCES is the stop's own GTC, which is why a
triggered stop-limit rests and a triggered stop-market's unfilled remainder
rests inert (section 1.7).

### 1.6 Reservations

An untriggered stop is an `OpenOrder` and therefore contributes to
`locked_balances` like any other resting order. The reservation is:

- **Buy**: `leaves_qty * price` for a `StopLimit`, `leaves_qty * trigger_price`
  for a `StopMarket`. The `expect("resting order price is always Some")` in
  `locked_balances` becomes `price.or(trigger_price)`, and the invariant it
  asserts becomes "a resting order carries a price or a trigger price", which
  `validate_submit` enforces.
- **Sell**: `leaves_qty` of the base, unchanged.

A buy stop-market reserving against its trigger price is an under-reservation by
exactly the slippage - the fill can land above the trigger. That is accepted and
stated rather than padded: the same one-sided exposure already exists for a
market order (`validate_fill_funds` re-checks the SLIPPED price at fill time and
rejects if the account cannot cover it), and the same re-check runs at trigger
time here. The reservation bounds the ordinary case; the fill-time check is what
keeps the ledger honest.

**`validate_fill_funds` must be re-shaped before it can be called at trigger
time, and this is not optional.** Today it is only ever called from the submit
path, where the order is not yet in `open`, so `free_balance` - which is total
minus `locked_balances`, i.e. minus every RESTING order's hold - does not
include the order's own reservation. At trigger time the order IS resting, so
calling it unchanged compares the full notional against a free balance that has
already had this same order's hold subtracted: a fully funded buy stop would
fail its own trigger at zero slippage. It also requires
`order.quantity * fill_px` rather than the quantity actually about to fill,
which over-requires on any partial. Both are fixed by one signature change:

```rust
/// `held` is the reservation this order itself already contributes to
/// `locked_balances`, added back before the comparison; zero on the submit
/// path, where the order is not yet resting. `qty` is the quantity about to
/// fill, not the order's total.
fn validate_fill_funds(
    &self,
    order: &SubmitOrder,
    qty: Decimal,
    fill_px: Decimal,
    held: Decimal,
) -> Result<(), String>;
```

with `free_balance(quote) + held >= qty * fill_px` as the test. The submit-path
call sites pass `held = Decimal::ZERO` and `qty = order.quantity`, which is
today's behavior exactly.

**When the check fails at trigger time**, the order is CANCELED, not rejected:
it was accepted, it triggered, and only then did the slipped price outrun the
account. The batch is `OrderTriggered`, then `OrderCanceled` with the reason
logged, the order removed from `open`, its reservation freed, `record_closed`
called with `Canceled`, and the account snapshot that any reservation-freeing
transition owes (section 3.4). Nautilus' FSM would ACCEPT a rejection here
(`(Triggered, Rejected) -> Rejected` exists, and that is the transition
post-only uses in section 1.9), so this is a choice rather than a constraint:
running out of money at the moment of execution is an economic outcome on a live
order, not the venue refusing a request, and `Canceled` is what the strategy's
own risk logic reads correctly.

### 1.7 Resting states

The engine's resting-order model becomes explicit:

    Resting::Limit { fill_trigger_px }  - live; a print strictly through fills it
    Resting::Conditional { stop_px }    - untriggered; a print touching it triggers
    Resting::Inert                      - never scanned; ends only on cancel

`Inert` is not new behavior, it is the naming of behavior the code currently
carries as a filter comment: `pending_scans` today excludes non-Limit and
non-GTC orders because "an armed `PartialFillNext` can leave one RESTING with a
stamped price, and without this filter that remainder would be handed to the
tape walk and held until the market traded through the price the venue itself
synthesized for it". With three types of resting order that filter would have to
grow a second special case; making the state explicit removes both.

### 1.8 reduce-only

`reduce_only` is a first-class wire flag, accepted on every order type and
ENFORCED at fill time, not at submit time:

    cap = |position(symbol)| if position's sign opposes the order's side, else 0

At every fill decision (arrival, sweep, trigger) a reduce-only order's
`last_qty` is `min(planned_qty, cap)`. A reduce-only order whose `cap` is zero
at the moment it would fill is CANCELED instead - `OrderCanceled`, recorded
`Canceled` in the truth store, reservation freed - with the reason logged.

It is enforced rather than echoed because ignoring it is a lie with teeth. The
user's shape is a position plus a protective stop; the stop is emitted
`reduce_only` by nautilus. A venue that accepts the flag and then fills the stop
after the position is already flat opens a NEW position in the opposite
direction, and the strategy's own state diverges from the venue's for the rest
of the run. Refusing the flag instead would refuse the very shape this spec
exists to enable. Enforcement is the only remaining option, and against a
netting ledger it is four lines.

It is enforced at FILL rather than at SUBMIT because a reduce-only order is
routinely submitted while flat and is expected to rest: the position it will
close may not exist yet when the protective leg is placed.

**Admission and reservation for a reduce-only order.** The "submitted while
flat" case collides with the funded ledger and the collision must be resolved
rather than left to the implementer. `validate_submit` today requires a funded
SELL to hold `quantity` of the BASE currency, and `locked_balances` reserves it
while the order rests. A protective sell-stop placed on a funded cash account
while flat holds no base, so it would be refused at the door and fill-time
enforcement would never be reached - the exact shape this spec exists to enable
would be unreachable on the only account mode that checks anything. The rule:

- A `reduce_only` order is EXEMPT from the `validate_submit` funds check and
  contributes NOTHING to `locked_balances`, on either side. It is by
  construction an order that can only shrink an existing exposure, so reserving
  against it would double-reserve the asset the position itself already
  represents.
- Its funds are checked at fill time like every other fill, against the CAPPED
  quantity, through the re-shaped `validate_fill_funds` above. A fill capped at
  the position it closes cannot overdraw, because closing a position returns
  the asset rather than spending it - the check is retained for the buy-side
  slippage case and for uniformity, not because it is expected to bite.

This is stated as its own rule because it is the one place reduce-only touches
the ledger's invariants rather than just the fill decision.

**The capped remainder is canceled, never stranded.** If `cap` is positive but
smaller than the planned quantity, the fill flattens the position and leaves a
remainder that can never again have a non-zero cap. For a stop-market that
remainder would become `Resting::Inert`, which by definition reaches no further
fill decision, so the "cancel when the cap is zero" rule would never fire and
the order would sit open until the client noticed. Therefore: a reduce-only
order whose fill was CLAMPED by the cap is closed in the same batch - fill, then
`OrderCanceled`, reservation math settled, `record_closed` with `Canceled`, one
account snapshot. This holds for both stop-market and stop-limit remainders and
for the arrival path; the `Inert` state is never entered by a reduce-only order.

### 1.9 post-only

`post_only` is a wire flag, legal on `Limit` and `StopLimit` only, and it means
one thing: an order that would take liquidity is rejected instead of filling.
Concretely, a post-only limit that is marketable on arrival is rejected with
"post-only order would take liquidity", and a post-only stop-limit that is
marketable against its own triggering print is rejected at trigger time with the
same reason (after `OrderTriggered`, which did happen).

REJECTED is the word, everywhere, and the `on_trigger` doc comment in section
3.3 says the same - an earlier draft said "cancels" in that one place and the
contradiction is resolved in favor of rejection. Nautilus' FSM permits it on
both paths (`(Accepted, Rejected)`, annotated "StopLimit order", and
`(Triggered, Rejected)`), and a post-only violation is the venue refusing the
order's own stated terms, which is a rejection and not a cancellation.

Because a rejection at TRIGGER time happens to an order that is already
`Accepted` and already in `open`, the engine must, in one batch: emit
`OrderTriggered`, emit `OrderRejected` with "post-only order would take
liquidity", remove the order from `open`, free its reservation, call
`record_closed`, and emit the account snapshot the freed reservation owes. This
also means the truth store needs a terminal status the wire can spell:
`WireOrderStatus` gains `Rejected` alongside `Triggered` (section 3.1), because
a rejection that happened AFTER acceptance is a closed row a `QueryOrders` must
be able to report. The submit-time post-only rejection is unchanged - the order
never entered `open`, so it is the existing pre-acceptance refusal path.

Included rather than excluded because under this venue's model it is one branch
at a decision the code already makes - `marketable` is computed on the submit
path today - and because refusing the flag would refuse another whole class of
nautilus strategy for no mechanism the venue lacks.

### 1.10 The lifecycle of a resting conditional

`problem-refused-order-types.md` decision 6, settled here:

- **Amend.** `ModifyOrder` gains `trigger_price`. Amending the trigger of an
  UNTRIGGERED conditional is legal and restarts its trigger window
  (`scanned_ns = ts`, `revision += 1`), exactly as a price amend restarts a
  limit's band window. Amending the trigger of an order that has ALREADY
  triggered is rejected with "order has already triggered": there is nothing
  left to trigger, and silently ignoring the field would make the amend a lie.
  A price amend on an untriggered stop-limit is legal (it changes the limit the
  order will take) and does NOT restart the trigger window, because the price
  the tape must touch has not moved; it does re-read the band, matching the
  existing price-amend rule.

  The ACK has to carry the new trigger or the amend is unverifiable:
  `ServerMessage::OrderUpdated` gains `trigger_price: Option<Decimal>` next to
  its existing `price`, the adapter forwards it into nautilus' `OrderUpdated`
  (which takes a trigger price the adapter passes `None` for today), and the
  adapter's own update arm must stop recomputing a zero-filled order's status as
  `Accepted` - see section 3.5. Without the wire field the venue would accept a
  trigger amend and answer with a message that still shows the old trigger,
  which is the silent-degrade class this document refuses elsewhere.
- **Cancel.** An untriggered conditional cancels like any resting order. After
  triggering, the cancel targets whatever the trigger produced: a live limit
  cancels normally; an inert market remainder cancels normally; a stop-market
  that already filled in full is terminal and answers
  `OrderCancelRejected: order already terminal`, which is the existing path.
- **QueryOrders.** An untriggered conditional reports `Accepted` with its
  `trigger_price` and `ts_triggered: None`. A triggered one with nothing filled
  reports the new `Triggered` status and its `ts_triggered`; with something
  filled it reports `PartiallyFilled`, because a partial fill is the more
  specific truth and nautilus' own status ladder orders it that way. Every row
  now also carries `price`, `reduce_only` and `post_only`.
- **Death of the instance.** Nothing, deliberately. There is no restart and no
  resume (`notes/todo.md`, settled premises); a resting stop dies with its
  process like every other piece of run state, and the fire-and-forget parent
  observes a dead PID. No reaping path exists to write.

### 1.11 Havoc, per arm, on the longer lifecycle

`problem-refused-order-types.md` decision 5 settled that ALL arms reach a
conditional. A conditional has three lifecycle points a plain order does not
(submit, trigger, and the order the trigger produces), so this is where each arm
lands. No new arm is added.

| Arm | Where it lands on a conditional |
|---|---|
| `RejectNextSubmit` | The SUBMIT. The conditional never exists, so nothing can trigger. |
| `PartialFillNext` | The FILL the trigger produces, never the trigger itself. An untriggered stop consumes no arm - it is targeted by client order id and `plan_fill` is the only consumer. A triggered stop-market's partial leaves an `Inert` remainder; a triggered stop-limit's partial leaves a live limit with a fresh band draw. |
| `DuplicateNextFill` | The fill event only. `OrderTriggered` is never duplicated: it is not a fill, and a duplicated trigger would be a state transition the client's FSM has no arm for. |
| `DropNextAccountUpdate` | The account snapshot that follows the triggered fill, on the same rule as any other fill. A trigger that produces no fill (stop-limit that rests) emits no snapshot and consumes no arm. |
| `CommandLatency` submit act/ack | The submit only. There is deliberately no trigger-act or trigger-ack knob: the trigger is a venue-internal event with no client command behind it, the sweep interval already governs how late a trigger can be, and adding a per-trigger delay knob would be a new arm rather than an extension of an existing one (out of scope, section 9). |
| `DelayAcks` / `GoDark` / `StallData` | Transport, unchanged. `OrderTriggered` classifies `EventKind::Exec`, so `DelayAcks` holds it and `GoDark` drops it; `StallData` never touches it. |
| `CancelOpenOrderSilently` | An untriggered conditional is a resting order, so it works today's way and this is the highest-value new coverage the landing buys: the venue silently kills the protective leg and only a `QueryOrders` poll reveals it. |
| `MarketRegime` / `VolStorm` / `LiquidityDrought` / `ReopenGap` | Per subscription, on the DATA feed, and therefore never on the trigger decision - the sweep walks the clean tape (`fills::scan_triggers` doc). A drought silences a client's view while its stops still trigger off the canonical tape. Same property acceptance-time readings already have; stated so nobody reports it as a bug. |

**Composition.** Arms are consumed at the instant their subject occurs, so a
submit-time arm and a trigger-time arm compose without ordering rules: a
`RejectNextSubmit` armed with a `PartialFillNext` targeted at the same id leaves
the partial armed (the order never existed), while a `PartialFillNext` plus a
`DuplicateNextFill` produce one partial fill delivered twice, at trigger time,
exactly as they do for a swept limit today. The one composition worth pinning
with a test is `CancelOpenOrderSilently` racing a trigger in the same sweep
pass: the silent cancel takes the engine lock and bumps nothing (it removes the
order), so the in-flight `ScanResult` fails its `client_order_id` lookup and is
dropped. The order is canceled and no fill is booked - which is the existing
revision-guard contract, not new machinery.

### 1.12 Determinism

Unchanged from the fill band's claim and no stronger. The trigger PRICE is
client-supplied, so triggering is a pure function of (seed, config) and the
client's own order: no draw, no estimator, no wall clock. The fill that FOLLOWS
a trigger inherits the band's existing caveats verbatim - `band_ticks` depends
on a wall-derived reading instant under a wall-paced clock, and the sweeper's
off-lock walk makes fill OUTCOME wall-dependent. This spec neither improves nor
worsens that, and does not claim otherwise.

Positively: the trigger draws nothing from any RNG, so adding stops cannot
perturb the trigger any other order draws.

## 2. Survey of the ground

Everything the teardown touches.

- `crates/mogwai-protocol/src/messages.rs`
  - `trades_through` - stays, gains a sibling.
  - `OrderType { Market, Limit }`, `TimeInForce`, `Side`.
  - `SubmitOrder`, `validate_submit_order` (the PRE-stamp gate; its doc comment
    explains the two-phase split with the engine and must be extended, not
    replaced).
  - `ClientMessage::ModifyOrder { client_order_id, price, quantity }` and
    `validate_modify_order`.
  - `WireOrderStatus` (including `is_open`, which the new `Triggered` status
    must join), `OrderStatusInfo`, `OrderStatusSnapshot`.
  - `ServerMessage::OrderUpdated`, which gains `trigger_price`.
  - `ServerMessage` (the variant list around `OrderAccepted` ..
    `HavocDiagnostic`) and `ServerMessage::category`, which maps each variant to
    an `EventKind`.
- `crates/mogwai-protocol/src/sizing.rs` - `ORDER_EVENT_MAX_BYTES`,
  `ORDER_STATUS_ROW_MAX_BYTES`, `worst_case_output_bytes`'s `SubmitOrder` arm
  (4 order events today), `swept_fill_max_bytes` (2 per emitted order today).
- `crates/mogwai-protocol/src/havoc.rs` - `EventKind`, `is_execution`.
- `crates/mogwai-data/src/trigger.rs` - `TriggerScan { side, trigger_px,
  from_ns }`, `Walk { triggered: Vec<bool>, reached_ns, drained }`,
  `scan_triggers` (including its early-stop once every scan has triggered),
  `vol_reading`, `VolReading`, and the module's unit tests.
- `crates/mogwai-data/src/lib.rs` - the re-exports of the above.
- `crates/mogwai-data/examples/fill_walk_bench.rs` - constructs `TriggerScan`s
  and reads `Walk.triggered`; moves with the type.
- `crates/mogwai-engine/src/lib.rs` - `OpenOrder` (`trigger_px`, `band_ticks`,
  `band_draw`, `scanned_ns`, `revision`), `MarketReading`, `PendingScan`,
  `ScanResult`, `Engine::pending_scans` (the Limit+GTC filter),
  `order_status_snapshot`, `open_order_status`, `cancel_open_order_silently`,
  `book_shape`.
- `crates/mogwai-engine/src/orders.rs` - `on_submit` (validation, the band draw,
  the marketable decision, the FOK gates, the rest/close routing),
  `apply_scans`, `plan_fill`, `commit_fill`, `validate_submit`,
  `validate_fill_funds`, `on_cancel`, `on_modify`, `draw_key`, `draw_offset`,
  `draw_trigger`, `draw_market_price`, `safe_price`.
- `crates/mogwai-engine/src/account.rs` - `locked_balances` (the
  `expect("resting order price is always Some")`), `positions`,
  `apply_position`, `next_position`.
- `crates/mogwai-server/src/fills.rs` - `scan_triggers` composition (the
  `PendingScan` -> `TriggerScan` map), `read_market`, `read_last`, and its unit
  tests, several of which assert on `walk.triggered`.
- `crates/mogwai-server/src/sweeper.rs` - the pass loop's `zip(walk.triggered)`
  into `ScanResult`, `deliver`'s `swept_fill_max_bytes` reservation (keyed on
  `apply_scans`'s fill-only `emitted` count, which must become an event count)
  and `deliver`'s `AdmissionSubject` finder (keyed on `OrderFilled` only).
- `crates/mogwai-server/src/http.rs` - `boundary_error`, `admission_subject`,
  `process_order_cmd` (the price-less MARKET refusal, keyed on
  `OrderType::Market`), `market_reading` (the stamp, keyed the same way, and the
  amend arm keyed on `ModifyOrder { price: Some(_), .. }`).
- `crates/mogwai-server/src/ws.rs` - decodes `ClientMessage`; type-agnostic, but
  it is the carrier the new smoke mode drives.
- `crates/mogwai-adapter/src/convert.rs` - `wire_order_type` (the permanent-set
  comment and the refusal message), `wire_time_in_force`,
  `nautilus_order_type`, `nautilus_order_status`.
- `crates/mogwai-adapter/src/client/exec.rs` - `submit_order` (builds the wire
  order from `cmd.order_init`), `modify_order`, `ExecWsCommand`,
  `exec_command_to_client_message`, `reject_for`, `OrderRecord`,
  `handle_exec_message` (the `OrderAccepted`/`OrderUpdated`/fill arms and their
  terminal-state guards), `order_status_report_from_info` (which sets NEITHER
  price nor trigger price today).
- `crates/mogwai-adapter/tests/` - `adapter_smoke.rs`, `havoc.rs`,
  `reconciliation.rs` (socket-backed, `#[ignore]`d, invisible to plain
  `brokkr check`).
- `scripts/smoke.py` - `order()` payload builder, `MODES`, `MODE_CONFIGS`,
  `mode_band`, `mode_band_swept`.
- `reference/architecture.md` - the execution paragraph; `reference/havoc.md`;
  `reference/config.md` if a knob moves (none does).
- `notes/todo.md` - the broadarrow standing note whose "no order-type growth"
  half this spec discharges, and the stop-MARKET coverage-hole note.

### 2.1 Reconciled against siblings

`notes/problem-instrument-model.md` will re-parameterize instruments and add
fees; nothing here fixes an instrument shape (the venue reads
`price_increment`/`size_increment` off the `InstrumentDef` it already has), and
the one new numeric - the trigger price - is validated against the same grid the
limit price is. `notes/problem-trade-cadence.md` will change tape density; the
trigger predicate is density-independent, and the one place this spec inherits a
density dependency is the shared `read_market` band, which is that document's
open item and not this one's. The standing "the fill band is INERT about 30% of
the time" item is inherited unchanged: a stop accepted at a refused-reading
instant rests untriggered (section 1.4) and triggers correctly, but the FILL its
trigger produces slips by `band_ticks = 0`. That is the same defect already
shipped for limits and markets, it is ordered after cadence, and this spec adds
no new instance of it.

## 3. Target artifacts

Exact shapes. Everything else is derived.

### 3.1 `mogwai-protocol`

```rust
/// True when a traded price has reached or passed a conditional order's
/// trigger. TOUCH, not through - see the spec's section 1.2: a stop holds no
/// queue position, so the strictness `trades_through` needs has no counterpart
/// here, and every real venue fires a stop on touch.
#[must_use]
pub fn touches_trigger(side: Side, trigger: Decimal, traded: Decimal) -> bool {
    match side {
        Side::Buy => traded >= trigger,
        Side::Sell => traded <= trigger,
    }
}

/// Which predicate a tape walk applies to one resting order. The engine
/// classifies, the data walk evaluates, and neither owns the enum - it lives
/// with the two predicate functions so the classification and the predicates
/// cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanKind {
    /// `trades_through` against a live limit's drawn band trigger.
    FillThrough,
    /// `touches_trigger` against an untriggered conditional's stop price.
    TriggerTouch,
}

impl ScanKind {
    #[must_use]
    pub fn hit(self, side: Side, px: Decimal, traded: Decimal) -> bool {
        match self {
            Self::FillThrough => trades_through(side, px, traded),
            Self::TriggerTouch => touches_trigger(side, px, traded),
        }
    }
}

/// The print that satisfied a scan: both its instant and its price, because a
/// triggered stop-market prices its fill off exactly this print.
#[derive(Debug, Clone, Copy)]
pub struct Hit {
    pub ts_ns: u64,
    pub px: Decimal,
}

pub enum OrderType { Market, Limit, StopMarket, StopLimit }

pub struct SubmitOrder {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub order_type: OrderType,
    pub quantity: Decimal,
    pub price: Option<Decimal>,
    /// The price the tape must touch for a conditional to become live.
    /// REQUIRED on StopMarket/StopLimit, refused on Market/Limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<Decimal>,
    pub time_in_force: TimeInForce,
    /// Fills are clamped to the position this order would close, and the order
    /// is canceled rather than filled when that position is gone.
    #[serde(default)]
    pub reduce_only: bool,
    /// An order that would take liquidity is rejected rather than filled.
    /// Legal on Limit and StopLimit only.
    #[serde(default)]
    pub post_only: bool,
}

pub enum WireOrderStatus {
    Accepted,
    /// A conditional whose trigger has fired, with nothing filled yet. A
    /// triggered order with a partial fill reports `PartiallyFilled`.
    Triggered,
    PartiallyFilled,
    Filled,
    Canceled,
    /// Refused AFTER acceptance: today only a post-only stop-limit that would
    /// take liquidity against its own triggering print (section 1.9). A
    /// pre-acceptance refusal never becomes a truth-store row at all.
    Rejected,
}

impl WireOrderStatus {
    pub fn is_open(self) -> bool {
        // `Triggered` is OPEN. A triggered stop-limit is resting and fillable,
        // and omitting it here would make it vanish from open-order
        // reconciliation between its trigger and its fill - a hole exactly the
        // width of the window this spec adds.
        matches!(self, Self::Accepted | Self::Triggered | Self::PartiallyFilled)
    }
}

pub struct OrderStatusInfo {
    // ... existing fields ...
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_price: Option<Decimal>,
    /// Sim unix-ns the trigger fired, `None` while untriggered or for a
    /// non-conditional order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ts_triggered: Option<u64>,
    #[serde(default)]
    pub reduce_only: bool,
    #[serde(default)]
    pub post_only: bool,
}

pub enum ClientMessage {
    // ...
    ModifyOrder {
        client_order_id: ClientOrderId,
        price: Option<Decimal>,
        quantity: Option<Decimal>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_price: Option<Decimal>,
    },
    // ...
}

pub enum ServerMessage {
    // ...
    /// A conditional order's trigger fired. Always precedes whatever the
    /// trigger produced (a fill, or the order resting as a live limit), in the
    /// same batch. Never duplicated by `DuplicateNextFill`.
    OrderTriggered {
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        ts_event: u64,
    },
    OrderUpdated {
        // ... existing fields ...
        /// New trigger price after the amend. `None` for a non-conditional
        /// order, and for an amend that did not touch the trigger.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_price: Option<Decimal>,
    },
    // ...
}
```

`serde(default)` on every added field is load-bearing, not politeness: it keeps
`scripts/smoke.py`'s existing payloads and every stored control-plane fixture
decodable without a sweep.

`validate_submit_order` (PRE-stamp) gains, keeping its existing two-phase doc:

```text
Market    : price optional (server stamps), trigger_price must be absent
Limit     : price required and > 0,         trigger_price must be absent
StopMarket: price must be ABSENT,           trigger_price required and > 0
StopLimit : price required and > 0,         trigger_price required and > 0
post_only : legal only on Limit / StopLimit
stop types: time_in_force must be Gtc
```

`validate_modify_order` accepts a trigger-only amend: at least one of the three
must be present, each present one strictly positive.

**The RISK PRICE: the one substitution a price-less `StopMarket` needs, pinned.**
The engine assumes a price everywhere, and the reviews found three distinct
places it does so. All three take the same replacement, named once:

    risk_px(order) = order.price.or(order.trigger_price)
                       .expect("validated submit carries a price or a trigger")

which is total for exactly the four rows of the table above, and is the same
`price.or(trigger_price)` the `locked_balances` invariant of section 1.6 adopts.
The sites:

1. `orders.rs`'s `let stated_px = order.price.expect("validated submit carries a
   price");` at the top of the band section, which runs BEFORE any type
   dispatch and would panic the engine on the first stop-market submit. It
   becomes `risk_px(order)`.
2. `validate_submit`'s `let Some(price) = order.price else { return Err("submit
   price required") }` and everything derived from it - the increment check, the
   `checked_mul` overflow guard that keeps `apply_fill`'s `last_qty * last_px`
   from panicking, and the funded-account requirement. The presence rule splits
   by type per the table; the DERIVED checks all run against `risk_px`. A
   `StopMarket`'s notional is therefore `quantity * trigger_price`, matching the
   reservation it will take in section 1.6.
3. `safe_price(stated, candidate)`'s degenerate fallback, reached when the band
   arithmetic overflows or slips to zero. For a triggered stop-market the
   fallback is `hit.px`, not `risk_px`: the triggering print is the best price
   the venue actually saw, and falling back to the trigger price would answer
   "what did this trade at" with the client's own number, which is the defect
   the fill band exists to remove. `draw_market_price` therefore passes `hit.px`
   as its `stated` argument at trigger time.

The RNG key price is separate and is the one place the client's number is
deliberately used: `draw_key`'s price component for a `StopMarket` is its
`trigger_price` (section 3.3), because the key must be stable across the order's
life and the trigger price is the only client-stated price it has.

Sizing:

```rust
// UNCHANGED at 4. The widest arrival shape is still four order events: today's
// IOC limit is Accepted, the duplicated fill, the fill, and the canceled
// remainder; an arrival-triggered conditional is Accepted, Triggered, the
// duplicated fill, the fill - and it cannot also carry an IOC remainder,
// because section 1.5 forbids IOC on a conditional. The other conditional
// arrival shapes are shorter: cap-zero reduce-only cancel and post-only
// rejection are three events each.
ClientMessage::SubmitOrder(_) => 4 * ORDER_EVENT_MAX_BYTES + account_state_max_bytes(widened)

// +1 per emitted order: the sweep now emits OrderTriggered alongside the fill
// and its possible duplicate.
orders * 3 * ORDER_EVENT_MAX_BYTES + account_state_max_bytes(widened)

// +128: ONE new decimal (trigger_price - `price` is already counted among the
// three decimals in the existing 320-byte accounting), one u64 (ts_triggered),
// two bools and their key names, on a row that already rounded 320 up to 384.
pub const ORDER_STATUS_ROW_MAX_BYTES: usize = 512 + ESC * (2 * MAX_CLIENT_ID_LEN + MAX_SYMBOL_LEN);
```

The `orders * 3` multiplier alone is NOT sufficient, and this is the sharpest
correctness hole the reviews found. `apply_scans` returns `emitted`, incremented
only inside `if last_qty > Decimal::ZERO`; `sweeper::deliver` hands that count
straight to `lane.reserve_swept(shape, emitted)`. A pass in which a stop-limit
triggers and RESTS - the ordinary case of section 1.3 - emits an
`OrderTriggered` frame with `emitted == 0`, writing an unreserved frame against a
zero-order reservation. So:

- `apply_scans` counts every order that produced ANY event, not every order that
  produced a fill. The engine's own bound test asserting `emitted == 3` moves
  with it.
- `deliver`'s `subject` finder currently matches only `ServerMessage::OrderFilled`
  when naming an `AdmissionSubject`; a trigger-only pass that hits the refusal
  branch would therefore have no subject to name. It must fall back to
  `OrderTriggered`, `OrderCanceled` and `OrderRejected`.
- Every no-fill transition that FREES a reservation - the cap-zero reduce-only
  cancel, the post-only trigger rejection, the trigger-time funds cancel - emits
  an account snapshot, so the per-pass `account_state_max_bytes` term is load
  bearing for passes that book no fill at all.

`ORDER_EVENT_MAX_BYTES` is unchanged: `OrderTriggered` is strictly narrower than
`OrderFilled`, which is the shape that bound already sizes.

`ServerMessage::category` maps `OrderTriggered` to `EventKind::Exec`.

### 3.2 `mogwai-data`

```rust
pub struct TriggerScan {
    pub side: Side,
    /// The price the predicate is applied against: a live limit's drawn band
    /// trigger, or an untriggered conditional's stop price.
    pub px: Decimal,
    pub kind: ScanKind,
    pub from_ns: u64,
}

pub struct Walk {
    /// Per scan, in the input's order: the print that satisfied it, or `None`.
    pub hits: Vec<Option<Hit>>,
    pub reached_ns: u64,
    pub drained: usize,
}

pub fn scan_triggers(
    source: &mut dyn TickSource,
    scans: &[TriggerScan],
    to_ns: u64,
    budget: usize,
) -> Walk;
```

The walk's early stop (it returns as soon as every scan has hit) and its
frontier reporting are unchanged; only what it records per scan changes. The
field rename `trigger_px -> px` is deliberate: the field now carries two
different quantities and the old name would name one of them.

### 3.3 `mogwai-engine`

```rust
/// What the venue is waiting for on one resting order.
#[derive(Debug, Clone, Copy)]
pub enum Resting {
    /// Live limit. A print strictly through `fill_trigger_px` fills it at its
    /// own stated price.
    Limit { fill_trigger_px: Decimal },
    /// Untriggered conditional. A print touching `stop_px` triggers it.
    Conditional { stop_px: Decimal },
    /// Never scanned: a market remainder left by a partial fill, which has no
    /// meaningful price for the tape to reach. Ends only on a client cancel.
    Inert,
}

pub struct OpenOrder {
    pub venue_order_id: VenueOrderId,
    pub submit: SubmitOrder,
    pub leaves_qty: Decimal,
    pub ts_accepted: u64,
    pub ts_last: u64,
    /// Sim unix-ns the conditional's trigger fired.
    pub ts_triggered: Option<u64>,
    pub resting: Resting,
    pub band_ticks: u32,
    pub band_draw: u32,
    pub scanned_ns: u64,
    pub revision: u64,
}

pub struct PendingScan {
    pub client_order_id: ClientOrderId,
    pub symbol: Symbol,
    pub side: Side,
    pub kind: ScanKind,
    pub px: Decimal,
    pub from_ns: u64,
    pub revision: u64,
}

pub struct ScanResult {
    pub client_order_id: ClientOrderId,
    pub from_ns: u64,
    pub revision: u64,
    /// The print that satisfied the scan. `None` means the span held nothing.
    pub hit: Option<Hit>,
    pub scanned_to_ns: u64,
}
```

`OpenOrder.trigger_px` is GONE, absorbed into `Resting::Limit`. `pending_scans`
loses its `order_type`/`time_in_force` filter entirely and becomes a match on
`resting`: `Limit` yields a `FillThrough` scan, `Conditional` a `TriggerTouch`
scan, `Inert` yields nothing.

New private engine helpers:

```rust
/// The quantity a reduce-only order may fill: the netted position it opposes,
/// or zero. `None` for an order that is not reduce-only.
fn reduce_only_cap(&self, order: &SubmitOrder) -> Option<Decimal>;

/// The triggered conditional's transition, under the engine lock: emits
/// OrderTriggered and then either commits the market fill, promotes the
/// stop-limit to a live banded limit (filling it at once if the triggering
/// print is already through its drawn trigger), REJECTS a post-only stop-limit
/// that would take liquidity, or cancels the order when the slipped price
/// outruns the account or a reduce-only cap has gone to zero. Every terminal
/// branch removes the order, frees its reservation, calls `record_closed` and
/// owes an account snapshot.
///
/// `hit` is a real print on the sweep path and the reading's own last print on
/// the arrival path (section 1.4); `ts` is the application instant and is what
/// every emitted `ts_event` and `ts_triggered` carries.
fn on_trigger(&mut self, pos: usize, hit: Hit, ts: u64) -> Vec<ServerMessage>;
```

`draw_market_price` gains a `band_draw` argument (it hardcodes `0` today) so a
triggered stop's slippage draw is distinct from the submit-time draw of the same
key. `draw_key`'s price component for a `StopMarket` is its `trigger_price`,
which is the only price it has; the key derivation is otherwise untouched.

### 3.4 `mogwai-server`

`fills::scan_triggers` maps `PendingScan.kind`/`px` straight onto `TriggerScan`;
`sweeper` zips `walk.hits` into `ScanResult.hit`. No new walk, no second
reading, no new config knob. `http::market_reading`'s amend arm stays keyed on
`ModifyOrder { price: Some(_), .. }` and is NOT widened: a trigger-only amend
draws no band and must not pay a synchronous `read_market` walk for a reading
nothing will read, while a stop-limit amend that carries both a price and a
trigger already matches on its price. The price-less MARKET stamp and its
refusal stay keyed on `OrderType::Market` exactly as they are.

### 3.5 `mogwai-adapter`

```rust
pub(crate) fn wire_order_type(order_type: OrderType) -> anyhow::Result<mogwai_protocol::OrderType> {
    match order_type {
        OrderType::Market => Ok(..Market),
        OrderType::Limit => Ok(..Limit),
        OrderType::StopMarket => Ok(..StopMarket),
        OrderType::StopLimit => Ok(..StopLimit),
        // Trailing stops need venue-side per-tick high-water state and
        // if-touched types need a resting non-protective trigger the venue
        // does not model; both were ruled out of scope by the repository
        // owner rather than deferred. Name that in the refusal.
        other => anyhow::bail!(
            "unsupported order type {other:?}: the MOGWAI venue serves Market, \
             Limit, StopMarket and StopLimit. It models no trailing state and \
             no order lists, so a trailing stop or a bracket leg must be \
             expressed as a fixed stop that the strategy re-places itself"
        ),
    }
}

/// The venue triggers off traded prices only - it has no quote and no mark
/// price - so a trigger reference it cannot honor is refused rather than
/// silently served as last-price.
///
/// `NoTrigger` and `Default` are ACCEPTED and normalize to last-price, which is
/// the one thing the venue can do; the report the adapter builds back therefore
/// says `LastPrice` for an order whose init said `Default`. That substitution
/// is stated here rather than left implicit, because it is the same shape as
/// the substitutions this module refuses - the difference is that `Default`
/// asks the venue to choose and `MarkPrice` asks for something specific the
/// venue does not have.
pub(crate) fn wire_trigger_type(t: Option<TriggerType>) -> anyhow::Result<()>;

/// The order-init shapes the venue cannot honor, refused BEFORE any
/// `OrderSubmitted` is emitted, alongside the trigger-type check:
///
/// - `trigger_instrument_id` naming an instrument other than the order's own.
///   The venue triggers off the order symbol's tape and has no cross-instrument
///   trigger; serving one silently would be the same lie as serving a mark-price
///   trigger as last-price.
/// - `contingency_type` other than `NoContingency`, and any `order_list_id` or
///   non-empty `linked_order_ids`. Section 9 rules out order lists, and this is
///   where that ruling has to be enforced: accepting one leg of a bracket
///   without the linkage would leave the venue filling a stop whose sibling
///   nothing will cancel, which is worse than refusing the shape by name.
///
/// Each refusal names the ruling and what to do instead, exactly as
/// `wire_order_type`'s does.
pub(crate) fn check_unsupported_init(init: &OrderInitialized) -> anyhow::Result<()>;
```

`submit_order` fills `trigger_price` from `cmd.order_init.trigger_price`,
`reduce_only` and `post_only` from the same init, and calls `wire_trigger_type`
before it builds the wire order - inside the existing convert-first block, whose
whole point is that a conversion failure returns before any `OrderSubmitted` is
emitted.

`modify_order` forwards `cmd.trigger_price`; `ExecWsCommand::Modify` and
`exec_command_to_client_message` carry it, and the `OrderUpdated` arm of
`handle_exec_message` passes the wire's new `trigger_price` into nautilus'
`OrderUpdated` (which takes one, and which the adapter fills with `None`
today).

**The `OrderUpdated` arm also has a status regression that this spec must fix,
not merely avoid.** It recomputes `record.status` from `filled_qty`/`leaves_qty`
as `Accepted` / `PartiallyFilled` / `Filled`, with no `Triggered` case. A
quantity or price amend on a triggered stop-limit - legal under section 1.10 -
would therefore walk the mirror back from `Triggered` to `Accepted`, desyncing it
from the engine and making every subsequent reconciliation report say `Accepted`
for a triggered order. Nautilus' own FSM keeps `(Triggered, Updated) =>
Triggered`. The zero-filled branch becomes: keep `Triggered` if the record is
already `Triggered`, else `Accepted`.

`handle_exec_message` gains an `OrderTriggered` arm following the
`OrderAccepted` arm's shape exactly: resolve ids, `with_order_record` under the
terminal-state guard (a triggered event for a terminal record keeps the terminal
status and warns), set `record.status = OrderStatus::Triggered`, forward-only
`ts_last`, emit `OrderEventAny::Triggered(OrderTriggered::new(..))`.

`order_status_report_from_info` gains `.with_price(..)` (a gap today, and a
stop-limit report without its limit price is unreconcilable),
`.with_trigger_price(..)`, `.with_trigger_type(TriggerType::LastPrice)` when a
trigger price is present, `.with_ts_triggered(..)`, `.with_reduce_only(..)` and
`.with_post_only(..)`, each under the same drop-and-warn discipline the
quantities use.

`nautilus_order_status` maps `WireOrderStatus::Triggered ->
OrderStatus::Triggered` and `WireOrderStatus::Rejected -> OrderStatus::Rejected`.

**`position_id` stays dropped, and that is now a stated position rather than an
omission.** mogwai's ledger NETS (`next_position` keeps one weighted-average
position per symbol), one run is one account and one instrument, and reduce-only
is enforced against that netted position. Reporting a venue position id would
imply the venue tracks per-position lots, which it does not. A hedging OMS is
therefore unsupported on `MOGWAI`; section 9 records it.

## 4. Landing 1: the walk reports hits

**Pure refactor, no behavior change.** Every existing test must pass unmodified
except where it names the renamed field.

Bricks:

1. `mogwai-protocol`: add `touches_trigger`, `ScanKind`, `Hit` beside
   `trades_through`, with the section-1.2 reasoning in the doc comments.
2. `mogwai-data/src/trigger.rs`: `TriggerScan.trigger_px -> px`, add `kind`,
   `Walk.triggered: Vec<bool> -> hits: Vec<Option<Hit>>`, route the predicate
   through `ScanKind::hit`, preserve the early stop (now "every scan has a
   hit"). Update the re-exports in `lib.rs` and `examples/fill_walk_bench.rs`.
3. `mogwai-engine`: `ScanResult.triggered: bool -> hit: Option<Hit>`,
   `PendingScan` gains `kind` (always `FillThrough` in this landing) and renames
   `trigger_px -> px`. `apply_scans` reads `result.hit.is_some()`.
4. `mogwai-server`: `fills::scan_triggers` maps the new fields; `sweeper` zips
   `walk.hits`; the `fills.rs` unit tests assert on
   `walk.hits.iter().map(Option::is_some)`.

Gates:

- `brokkr check` - green, with no test's EXPECTED BEHAVIOR changed. The gate is
  behavioral, not textual: a `Vec<bool> -> Vec<Option<Hit>>` rename forces
  literal rewrites, and `trigger.rs` already contains
  `assert_eq!(walk.triggered, vec![true, true])`, which cannot survive as
  written. Rewriting it to
  `assert_eq!(walk.hits.iter().map(Option::is_some).collect::<Vec<_>>(), vec![true, true])`
  is in bounds - the same tape, the same two hits. A test whose expected
  which-scans-hit or which-prints answer changed is out of bounds and means the
  refactor was not one.
- `brokkr test -p mogwai-data a_walk_reports_the_price_and_instant_of_each_hit`
  - new: a walk over a known tape returns, for each scan, the exact print that
  satisfied it, and `None` for one nothing satisfied.
- `brokkr test -p mogwai-data a_touch_scan_hits_at_the_price_and_a_through_scan_does_not`
  - new: one tape, one price, two scans differing only in `kind`; the touch
  scan hits on the print AT the price and the through scan does not. This is the
  single test that pins the section-1.2 distinction.

Keep/revert: revert if `brokkr check` needs any test's expected behavior
changed, or if the benchmark regresses. The benchmark is a Criterion EXAMPLE
target, so the command is `brokkr run fill_walk_bench -- --bench` - `brokkr test
-p mogwai-data` builds it and never runs it, and an earlier draft of this
document cited that non-measurement as the gate.

The walk now carries a `Decimal` per hit instead of a bool, which is 16 bytes
per scan instead of one. `Hit` is already `Copy` and the result vector is
already preallocated (`vec![false; scans.len()]` becomes `vec![None; n]`), so
there is no cheaper shape held in reserve: if the bench moves measurably, the
remedy is to keep `Walk.triggered: Vec<bool>` and carry the hit prices in a
SECOND, separately allocated `Vec<Option<Hit>>` populated only when at least one
scan is a `TriggerTouch` - the limit-only pass then pays exactly what it pays
today. That fallback is not the design because it is two vectors to keep in
step; it is the escape hatch, and naming it is what makes the revert criterion
actionable.

## 5. Landing 2: the venue speaks conditionals

The intrusive one: protocol surface, engine lifecycle, server threading, and the
Python smoke that drives it over the real socket.

Bricks, in dependency order:

1. **Protocol surface.** `OrderType`'s two new variants; `SubmitOrder`'s
   `trigger_price`/`reduce_only`/`post_only`; `ModifyOrder.trigger_price`;
   `WireOrderStatus::Triggered` and `::Rejected` plus the widened `is_open`;
   `ServerMessage::OrderUpdated`'s `trigger_price`; `OrderStatusInfo`'s four new fields;
   `ServerMessage::OrderTriggered` plus its `category` arm;
   `validate_submit_order` and `validate_modify_order` per section 3.1; the
   three sizing changes.
2. **`Resting`.** Introduce the enum, delete `OpenOrder.trigger_px`, add
   `ts_triggered`, rewrite `pending_scans` as a match. Every construction site
   of `OpenOrder` in `on_submit` sets `resting` explicitly. This brick alone
   turns the venue green again on limits.
3. **Submit-side validation.** `validate_submit` becomes type-aware per the
   table in 3.1: trigger price presence, grid alignment
   (`on_increment(trigger_price, price_increment)`) and positivity; the
   price-required rule split by type; the GTC-only rule for conditionals; the
   post-only type restriction. Each with its own reason string, each named in a
   test.
4. **Submit-side acceptance.** A conditional draws `band_ticks` from the
   reading and rests as `Resting::Conditional` unless the reading already
   touches its stop, in which case the submit runs `on_trigger` inline. The
   existing marketable-on-arrival path for limits is untouched.
5. **`on_trigger`.** Section 1.3 and 1.9, under the lock: emit
   `OrderTriggered`; stop-market -> `plan_fill` -> reduce-only clamp ->
   trigger-time funds re-check with the order's own hold added back ->
   `commit_fill` at `draw_market_price(hit.px, band_draw + 1)` ->
   `Resting::Inert` remainder or close; stop-limit -> fresh band draw ->
   post-only REJECTION if marketable -> immediate fill at the stated price if
   `trades_through(side, fill_trigger_px, hit.px)` -> otherwise
   `Resting::Limit` with `scanned_ns = ts`. Every terminal branch (reject,
   funds cancel, cap cancel, full fill) removes the order, frees its
   reservation, calls `record_closed` and emits the account snapshot.
6. **`apply_scans`.** Dispatch on the matched order's `resting`: a `Limit` hit
   fills as today, a `Conditional` hit calls `on_trigger`. The revision and
   frontier guards are untouched - they are what makes the off-lock walk safe
   and this landing does not relitigate them.
7. **reduce-only.** `reduce_only_cap`, applied at all three fill decisions;
   the cancel-instead-of-fill path when the cap is zero; the cancel-the-clamped-
   remainder path when the cap was positive but short; and the admission
   exemption - a reduce-only order skips the `validate_submit` funds check and
   contributes nothing to `locked_balances` (section 1.8).
8. **Reservations and the funds re-check.** `locked_balances`'s
   `price.or(trigger_price)`; `validate_fill_funds` re-shaped to take the
   filling quantity and the order's own held amount (section 1.6) so a resting
   order's trigger cannot fail against its own reservation; every submit-path
   call site passes `Decimal::ZERO` and the full quantity, preserving today's
   behavior exactly.
9. **`on_modify`.** The `trigger_price` arm: legal on an untriggered
   conditional, restarts the trigger window, rejected after triggering. The
   existing price/quantity arms keep their semantics.
10. **Truth store.** `open_order_status` reports the new status ladder and the
    four new fields; `record_closed` preserves `ts_triggered`.
11. **Server threading.** `fills`/`sweeper` pass `kind` through;
    `market_reading`'s amend arm; nothing else.
12. **Smoke.** `scripts/smoke-stop.toml` (accelerated, funded, the default
    BTCUSDT profile) and `mode_stop` in `scripts/smoke.py`: read the last print
    off `/trades`, submit a sell `StopMarket` a few ticks BELOW it, assert
    `OrderAccepted` and that a `QueryOrders` shows it resting untriggered with
    its `trigger_price` echoed; then wait for `OrderTriggered` followed by
    `OrderFilled` on the same socket, and assert the fill price is at or below
    the triggering print (adverse for a sell). Retry at a fresh anchor up to
    three times, exactly as `mode_band_swept` does, because a downward-moving
    tape is what makes this event. Register it in `MODES` and `MODE_CONFIGS`.
13. **Adapter compile-compatibility - part of L2, not L3.** `serde(default)`
    preserves JSON DECODING; it does nothing for Rust struct literals or
    exhaustive matches, and `mogwai-adapter` has both: `exec.rs::submit_order`
    builds a `mogwai_protocol::SubmitOrder` literal field by field, and
    `convert.rs` matches `OrderType` and `WireOrderStatus` exhaustively, as does
    `ServerMessage` handling. Adding variants and fields in L2 breaks that
    build, so L2 carries the MINIMUM adapter edit that restores compilation and
    nothing more: the literal gains `trigger_price: None`, `reduce_only: false`,
    `post_only: false`; `nautilus_order_type` gains `StopMarket`/`StopLimit`
    arms; `nautilus_order_status` gains `Triggered`/`Rejected` arms;
    `handle_exec_message` gains an `OrderTriggered` arm that logs and drops; and
    `wire_order_type` STILL REFUSES the two new types. That last point is what
    keeps L2 and L3 separable: after L2 the adapter compiles and behaves exactly
    as it does today - it refuses conditionals at conversion - while L3 is the
    landing that removes the refusal and wires the semantics.
14. **Inline documentation only.** The implementer writes doc comments on every
    new type, field and function this landing introduces, to the exact text this
    spec dictates where it dictates one.

    NOT part of this landing, and NOT to be attempted by the implementer:
    reconciling `reference/`, `docs/` or `notes/`. Document reconciliation and
    the removal of the originating item are owned by the orchestrator and happen
    after the implementation has been reviewed. Recorded here only so the later
    reconciliation pass knows where to look: `reference/architecture.md`'s
    execution paragraph needs the conditional surface and the touch-versus-
    through distinction, `reference/havoc.md` needs the section-1.11 table, and
    `notes/todo.md` carries both the stop-MARKET coverage-hole note and the
    superseded broadarrow no-growth entry, plus the two carve-outs section 9
    creates.

Gates:

- `brokkr check` - the whole workspace, including `mogwai-protocol`'s serde
  round-trips (the new fields must serialize identically on both ends) and
  `worst_case_reservation_covers_actual_output`, which gains an
  arrival-triggered conditional as a new CASE - not as a new widest submit: it
  ties today's IOC limit at four order events and does not exceed it, which is
  why the submit multiplier stays at 4.
- `brokkr test -p mogwai-engine a_stop_market_rests_untriggered_until_a_print_touches_its_stop`
- `brokkr test -p mogwai-engine a_stop_triggers_on_a_print_exactly_at_its_stop_price`
  - the touch semantics, at the engine layer this time.
- `brokkr test -p mogwai-engine a_triggered_stop_market_fills_slipped_off_the_triggering_print`
  - a SWEPT trigger: the fill price is the HIT price slipped adversely, never
  the stop price and never the acceptance-time last price. This is the test that
  would catch "filling at the trigger price", which is the lie the whole
  fill-band landing exists to prevent. The clause about the acceptance-time last
  price is scoped to the swept case deliberately: on the ARRIVAL path that
  reading IS the hit (section 1.4), and the companion assertion there is that
  the fill is the reading's last price SLIPPED, never the reading's last price
  raw and never the trigger price.
- `brokkr test -p mogwai-engine a_gapped_stop_limit_triggers_and_rests_without_filling`
  - the section-1.3 counter-case: a sell stop at 100 with a limit at 99,
  triggered by a print at 95, emits `OrderTriggered` and rests unfilled. The one
  test that pins the venue against manufacturing a fill through a gap.
- `brokkr test -p mogwai-engine a_trigger_only_sweep_pass_reserves_its_own_frame`
  - at the server layer too: a pass in which the only event is `OrderTriggered`
  is counted into `emitted` and reserved for, and its refusal path names an
  admission subject.
- `brokkr test -p mogwai-engine a_fully_funded_buy_stop_does_not_fail_its_own_trigger_on_its_own_reservation`
  - the section-1.6 double-count: an account funded to exactly the order's
  notional plus the band's worst slip triggers and fills.
- `brokkr test -p mogwai-engine a_reduce_only_order_rests_while_flat_on_a_funded_account`
  - the admission exemption: a protective sell-stop is accepted with no base
  balance held and reserves nothing.
- `brokkr test -p mogwai-engine a_cap_clamped_reduce_only_fill_cancels_its_remainder`
  - the remainder never becomes `Inert`.
- `brokkr test -p mogwai-engine query_orders_reports_a_triggered_stop_limit_as_open`
  - `WireOrderStatus::Triggered.is_open()`, and the row survives the open/closed
  partition.
- `brokkr test -p mogwai-engine a_triggered_stop_limit_rests_banded_and_does_not_fill_for_free`
  - after a trigger that does not clear the drawn band, the order rests with a
  fresh `band_draw` and a frontier at the pass instant, and the NEXT pass does
  not fill it on the prints the trigger pass already covered.
- `brokkr test -p mogwai-engine a_triggered_stop_limit_marketable_against_its_trigger_print_fills_at_once`
- `brokkr test -p mogwai-engine a_stop_already_through_the_market_triggers_on_arrival`
- `brokkr test -p mogwai-engine a_stop_with_ioc_or_fok_is_rejected`
- `brokkr test -p mogwai-engine conditional_field_shapes_are_rejected_by_type`
  - the whole 3.1 table in one table-driven test: a stop with no trigger, a
  limit WITH a trigger, a stop-market with a price, a stop-limit with no price,
  an off-grid trigger, a post-only market.
- `brokkr test -p mogwai-engine a_post_only_order_that_would_take_liquidity_is_rejected`
- `brokkr test -p mogwai-engine a_reduce_only_order_is_capped_by_the_position_and_cancels_when_flat`
- `brokkr test -p mogwai-engine an_untriggered_buy_stop_reserves_against_its_trigger_price`
- `brokkr test -p mogwai-engine a_trigger_amend_restarts_the_trigger_window_and_is_rejected_after_triggering`
- `brokkr test -p mogwai-engine query_orders_distinguishes_untriggered_triggered_and_partially_filled`
- `brokkr test -p mogwai-engine partial_fill_next_lands_on_the_fill_the_trigger_produces`
  - the havoc composition rule: an untriggered stop consumes no arm, and the
  arm fires on the fill the trigger produces.
- `brokkr test -p mogwai-engine a_silent_cancel_racing_a_trigger_leaves_the_order_canceled`
- `brokkr test -p mogwai-engine the_tape_is_identical_with_and_without_a_resting_stop`
  - the existing tape-purity property, extended: no client conditional advances
  any generator state.
- `brokkr test -p mogwai-server stop_touch_scans_and_limit_through_scans_share_one_walk`
  - the cost claim: a pass over a mixed book still drains one walk per symbol,
  the mixed-kind twin of `a_pass_costs_one_walk_per_symbol_not_per_order`.
- `brokkr run mogwai -- serve -f --config scripts/smoke-stop.toml` in one
  terminal is NOT how this is gated (the smoke spawns its own venue):
  `python3 scripts/smoke.py stop`, plus `python3 scripts/smoke.py default`,
  `python3 scripts/smoke.py band` and `python3 scripts/smoke.py band-swept` to
  prove the wire additions broke no existing carrier.

Cost, and why no new measurement gates this landing: no new tape walk, no new
`read_market` call, no new checkpoint restore. A resting conditional adds one
scan to a walk that is already per-symbol, and `scan_triggers` decides all scans
in one drain. The two standing instruments therefore keep their existing
thresholds and are re-read rather than re-derived:
`brokkr test -p mogwai-server read_market_latency_stays_within_submit_budget`
must still show median at or below 5 ms and p99 at or below 25 ms, and
`brokkr test -p mogwai-data --raw` must show `fill_walk_bench` unmoved.
Exceeding either is a revert, not a re-tune: this landing has no license to
spend fill-path budget.

Keep/revert: revert the whole landing on any red gate. It is one coherent
change - the wire, the engine and the smoke move together, because a protocol
that carries a trigger nothing evaluates is worse than one that refuses it.

## 6. Landing 3: the adapter stops refusing

Bricks:

1. `convert.rs`: `wire_order_type`'s four arms and the rewritten refusal
   message; `wire_trigger_type`; `nautilus_order_status`'s `Triggered` arm.
   Delete the "Market and Limit is the whole set, permanently" comment - it is
   the assertion this whole spec refutes.
2. `exec.rs::submit_order`: `trigger_price`, `reduce_only`, `post_only` and the
   trigger-type check, all inside the existing convert-first block.
3. `exec.rs::modify_order`, `ExecWsCommand::Modify`,
   `exec_command_to_client_message`, `reject_for`: the `trigger_price` field.
4. `handle_exec_message`: the `OrderTriggered` arm with its terminal-state
   guard and mirror update.
5. `order_status_report_from_info`: price, trigger price, trigger type,
   `ts_triggered`, `reduce_only`, `post_only`.
6. Inline documentation only, on what this landing adds. Reconciling
   `reference/architecture.md`'s adapter paragraph and discharging the
   broadarrow standing-note carve-out in `notes/todo.md` are the
   orchestrator's, after review; they are named here for that later pass, not
   for the implementer.

Gates:

- `brokkr check --gate` - not plain `brokkr check`. The four socket-backed
  adapter binaries are `#[ignore]`d and two regressions have already shipped red
  through that gap; this landing touches exactly those binaries' subject.
- `brokkr test -p mogwai-adapter adapter_submits_a_stop_market_and_sees_triggered_then_filled`
  - new, socket-backed in `tests/adapter_smoke.rs`: a real venue, a real
  nautilus `StopMarketOrder` through the real `ExecutionClient`, and the event
  sequence `Submitted -> Accepted -> Triggered -> Filled` observed on the
  emitter. The one test that proves the refusal is actually gone end to end.
- `brokkr test -p mogwai-adapter a_stop_report_carries_its_trigger_price_and_status`
  - new, in `tests/reconciliation.rs`: venue truth for an untriggered and a
  triggered stop, over both query carriers, with the report's `trigger_price`,
  `price`, `ts_triggered` and `order_status` pinned. Without this the
  reconciliation class the existing guard covers has a hole exactly the shape of
  the new fields.
- `brokkr test -p mogwai-adapter havoc_reaches_the_order_a_trigger_produces`
  - new, in `tests/havoc.rs`: with `DelayAcks` armed, the `OrderTriggered` and
  the fill behind it are both held, proving `OrderTriggered` classifies as
  execution rather than slipping through as data.
- `brokkr test -p mogwai-adapter a_trailing_stop_is_refused_by_name`
  - the refusal that REMAINS must name what to do instead, and must not emit
  `OrderSubmitted` before it fails (the AE8 invariant the convert-first ordering
  exists for).
- `brokkr test -p mogwai-adapter unsupported_init_shapes_are_refused_before_submitted`
  - table-driven over a mark-price trigger, a foreign `trigger_instrument_id`,
  and a contingency/order-list leg: each refused, each naming the ruling, none
  emitting `OrderSubmitted` first.
- `brokkr test -p mogwai-adapter a_trigger_amend_on_a_triggered_stop_limit_keeps_it_triggered`
  - the mirror does not walk back to `Accepted`, and the emitted nautilus
  `OrderUpdated` carries the new trigger price.

Keep/revert: revert on any red gate. A partially converted adapter - one that
sends a trigger price the engine reads but cannot report it back - is exactly
the silent-degrade reconciliation class `notes/todo.md` already names as an open
exposure, so there is no half of this landing worth keeping alone.

## 7. Ordering and the green boundary

L1 -> L2 -> L3, and the suite is green at each boundary:

- After L1 the venue behaves identically and the walk carries strictly more
  information than anything reads.
- After L2 the venue serves conditionals over the wire and the Python smoke
  proves it; the adapter still refuses them at conversion, which is the state
  the repository has today and is therefore green. Green here REQUIRES L2's
  brick 13: new enum variants and new struct fields break the adapter's
  exhaustive matches and struct literals at compile time, so the boundary is
  green only because L2 carries the mechanical compile fix. It carries no
  behavior: `wire_order_type` still refuses.
- After L3 nautilus strategies reach them.

L2 is the only landing that can be called large, and it does not decompose: the
`Resting` refactor, the type-aware validation and `on_trigger` are one change to
one state machine, and any split leaves either a wire field nothing honors or an
engine path nothing can reach.

## 8. What this spec rejects, with reasons

- **Client-side emulation** (`emulation_trigger`/`OrderEmulator`). Rejected in
  writing, as `problem-refused-order-types.md` requires. It would work and cost
  nothing, and the loss is precisely stated: under emulation the protective leg
  never exists at the venue, so nothing can delay its acceptance, reject it on
  arrival, drop it while it rests, or fire it late - and exercising the live
  path under exactly those faults is the entire reason this venue exists. The
  overstated version of this argument ("no havoc can reach it") is false and is
  not the argument used: havoc does reach the market order the emulator
  releases.
- **A separately fitted slippage multiplier for triggered stops.** Rejected:
  it would introduce an unmeasured constant where reusing the existing band
  introduces none. Section 1.3.
- **A synthetic top-of-book so stops can trigger off quotes.** Rejected by
  decision 2 of the problem statement and not reopened here. The venue answers
  "where is the market" one way, everywhere.
- **Rejecting a stop that is already in the market**, as nautilus' own
  simulated exchange does. Rejected: section 1.4.
- **A venue-side OCO/order-list abstraction, even a minimal one.** Rejected by
  the user's ruling on decision 1. Reduce-only, which this spec DOES build, is
  the part of that surface that survives on its own merits: it is what a lone
  protective leg needs, not what a bracket needs.
- **A `trigger_type` field on the wire.** Rejected: one legal value. The
  adapter refuses the others rather than the protocol carrying a field that can
  only ever say the same thing.
- **A trigger-act latency havoc knob.** Rejected as a new arm rather than an
  extension of an existing one; the sweep interval already bounds how late a
  trigger can be, and `notes/` gets the item.

## 9. Stopping rule

The teardown stops at the engine's resting-order state machine, the wire types
that describe it, and the adapter's conversion of them. Explicitly out of scope,
each with the reason it is not deferral:

- **Trailing stops and two-leg brackets.** Ruled out by the user, not deferred.
  Naming the cost once more, since the refusal message now has to state it: a
  strategy whose protective structure is a bracket, or whose stop trails, is not
  forward testable on this venue.
- **MarketIfTouched / LimitIfTouched.** Killed and staying killed.
- **GTD and `expire_time`.** A separate `notes/` item, not part of this
  problem statement: a GTD LIMIT is refused today for the same reason a GTD stop
  would be, so the gap predates conditionals and closing it needs a wire expiry
  field plus a time-driven expiry pass that has nothing to do with triggers.
- **Hedging OMS / venue position ids.** The ledger nets; section 3.5.
- **The price-span-per-match measurement.** Named in the problem statement,
  owed, and not a blocker here; it prices the EXISTING band and moves both the
  market and the triggered-stop paths when it lands.
- **The 30%-inert-band item.** Inherited unchanged, ordered after
  `notes/problem-trade-cadence.md`; section 2.1.
- **The off-lock ordering that makes fill outcome wall-dependent.** The
  existing contract, untouched, and the determinism claim in section 1.12 is
  written to match what the venue actually offers rather than what would be
  nicer to say.
- **`mogwai-engine`'s unbounded `next_position` accumulation.** A standing
  `notes/todo.md` item that reduce-only enforcement reads but does not worsen:
  the cap is derived from the position, never added to it.

## 10. New `notes/todo.md` entries this spec creates

To be added when landing 2 touches that file, so the carve-outs above are
recorded rather than lost:

- GTD / `expire_time` on the wire, with a time-driven expiry pass on the
  sweeper. Refused today for limits and stops alike.
- A trigger-act latency havoc arm, if a scenario ever needs a late trigger the
  sweep interval cannot express.

## 11. Review disposition

Two independent reviews (`notes/spec-conditional-orders-review-1.md`, an
Agent(opus) read; `notes/spec-conditional-orders-review-2.md`, a codex
gpt-5.6-sol deep read) raised 23 findings between them, overlapping on five.
Each was checked against the source before being folded in. The design is
unchanged: the three-landing split, the `Resting` enum, touch-versus-through,
fill-time reduce-only and the rejection list in section 8 all survived both
reviews.

Folded in, by where they landed:

| Finding | Landed in |
|---|---|
| Trigger-only sweep passes write unreserved frames (both reviews) | 3.1 sizing, section 2 survey, L2 gate |
| `deliver`'s admission subject is `OrderFilled`-only | 3.1 sizing |
| No-fill reservation-freeing transitions owe an account snapshot | 3.1 sizing, 1.6, 1.8, 1.9 |
| `on_submit` unwraps a price a `StopMarket` does not carry (both reviews) | 3.1 risk-price rule |
| `validate_submit`'s notional, overflow and funds checks all derive from that price | 3.1 risk-price rule |
| `safe_price`'s degenerate fallback for a triggered stop-market | 3.1 risk-price rule |
| The gapped stop-limit example is financially wrong (both reviews) | 1.3, plus a new L2 gate |
| Post-only at trigger: rejected or canceled, and what the engine does | 1.9, 3.3 |
| L2 cannot compile: adapter struct literals and exhaustive matches | L2 brick 13, section 7 |
| Trigger-time funds check double-counts the order's own hold | 1.6, L2 brick 8 |
| That check uses the order quantity, not the filling quantity | 1.6 |
| Reduce-only while flat is refused by the funded-sell admission check | 1.8 |
| A cap-clamped remainder is stranded as `Inert` | 1.8 |
| `OrderUpdated` carries no `trigger_price` | 1.10, 3.1, 3.5 |
| The adapter's update arm walks a triggered order back to `Accepted` (both reviews) | 1.10, 3.5, L3 gate |
| `WireOrderStatus::Triggered` missing from `is_open` | 3.1 |
| Arrival trigger has no `Hit`, and the L2 gate contradicted it (both reviews) | 1.4, L2 gate |
| `ts_triggered` / `ts_event`: hit instant or application instant | 1.4 |
| `trigger_instrument_id`, contingency and order-list metadata ignored | 3.5 |
| `ORDER_STATUS_ROW_MAX_BYTES` comment double-counts `price` | 3.1 sizing |
| The 5-event submit rationale cites the forbidden IOC conditional (both reviews) | 3.1 sizing, L2 gate |
| L1's keep/revert gate trips on `assert_eq!(walk.triggered, ..)` | L1 gates |
| L1's revert remedy was already the design | L1 keep/revert |
| `fill_walk_bench` is a Criterion example, not a `cargo test` target | L1 keep/revert |
| `touches_trigger` versus `trades_through`, stated precisely | 1.2 |
| `market_reading`'s widened amend arm buys a walk nothing reads | 3.4 |
| The nautilus build path was never stated | Header |

Rejected, with reasons:

- **"`OrderRejected` after acceptance is not a legal nautilus transition."** Not
  a finding either review made, but the natural reading of R1's parenthetical.
  It is legal - `(Accepted, Rejected)` is annotated "StopLimit order" and
  `(Triggered, Rejected)` exists - so post-only rejection at trigger time needs
  no workaround, and this document uses it. Only the ENGINE-side bookkeeping was
  missing, and that is what got added.
- **Making the trigger-time funds failure a rejection.** R2 asked for "the
  terminal event and truth-store status" to be defined and did not prescribe
  one. `Canceled` is chosen over `Rejected` on the reasoning in section 1.6:
  running out of money at execution is an economic outcome on a live order, not
  a refusal of a request. Recorded as a choice so a later reader does not
  re-derive it as a bug.
- **Accepting contingency and linkage metadata with degraded semantics.** R2
  offered this as an alternative to refusing it. Refused: a bracket leg accepted
  without its linkage leaves the venue filling a stop whose sibling nothing
  cancels, which is a worse lie than the refusal and contradicts section 9's
  ruling.
- **Nothing else.** No finding from either review was rejected on the merits.
  The five overlaps were merged rather than counted twice, and R1's
  already-`Copy` observation and R2's no-op-fallback observation are the same
  finding, resolved once in L1's keep/revert.
