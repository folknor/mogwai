# mogwai architecture: accounts, risk, instruments and the order path

Part of the architecture reference. The map and the reading order are in
`architecture.md`; this file was split out of the one long document on
2026-08-27 as a contiguous slice, with nothing moved, cut or reworded.

## Risk policy, and why the venue enforces it

An account may carry a risk policy, which the venue enforces rather than
reports. This is a risk-policy layer and not a funded-account feature: a live
venue has the same machinery, where an operator sets a daily loss limit that
behaves like a liquidation except that it lifts at the next session. A rule is a
triple - what it measures, on what basis, and what it does on breach - and the
breach action is what spans both worlds: `lock_until_reset` flattens and refuses
to open until the next reset, `terminate` flattens and ends the account. A
trailing drawdown ratchets on intraday peak equity including unrealized, or on
end-of-day balance only, which is the single largest difference between two
accounts advertising the same number. A static overall drawdown measures from
opening equity and never ratchets, which is the other common funded-account
form. A max-position cap is refused at entry: it is the largest |qty| the book
can reach after this order, given worst-case fill order of the working book
(worse extreme net under netting, larger side under hedging). Working orders
count; reduce-only does not. An oversized submit is a consumer error rather than
a liquidation.
The account defines its own day: the reset
is a minute of the UTC day named by the policy, not the instrument's calendar,
and it fires whenever sim time crosses it.

A policy resolves the way a symbol does: total, three steps, step three never
fails. Knobs stated inline win; otherwise a policy registered by name under
`[account_policies]` in the venue config, or one this build ships under that
name, with registered shadowing shipped; otherwise unpoliced. A name nobody has
is an error rather than a silent fall to unpoliced, because a run that believes
it is enforced and is not is the worst of the three outcomes. Registration is a
runtime path, unlike instrument presets, which are compiled in: funded-account
programmes number in the hundreds and change their terms without notice, so the
shipped set is illustrative rather than authoritative.

A breach flattens - cancel every resting order, then close every position as
reduce-only IOC market orders at the mark, each crossing its position's last
marked market state (the book the sweep read beside that mark), with the
configured liquidation band as the fallback book where no pair was recorded -
and then locks. A terminating breach on a venue serving one
account ends the run, since its only account is dead and there is nothing left
to serve; on a shared exchange it does not, because one subagent breaching must
not take down the batch. Enforcement without the flatten would be a
report; the flatten is what makes a forward claim mean anything, because a
strategy that would have been liquidated actually is.

One equity reading may cross several configured floors. A terminating rule
wins over a lock on that reading, so a softer rule earlier in evaluation order
cannot permanently hide a hard floor. Rules with the same action keep their
evaluation order. A lock is acted on once and remains inert until the reset
lifts it.

The thresholds, the ratcheted peak and the remaining budget are published on
`GET /account` for the evaluator rather than for the strategy. mogwai presents
no dashboard, so a run that ended flat having spent most of its budget would be
indistinguishable from one that never came close.

## The order-type surface

The order-type surface is complete rather than curated, and as of 2026-08-18 it
is complete in fact and not only in intent: Market, Limit, StopMarket,
StopLimit, TrailingStopMarket, TrailingStopLimit, MarketIfTouched,
LimitIfTouched and MarketToLimit are served, which is every order type nautilus
expresses. `wire_order_type` has no refusal arm left, so a type nautilus adds
later is a compile error in the adapter rather than a runtime refusal a strategy
discovers mid-run.

A trailing stop limit carries two distances, which is what distinguishes it from
every other conditional. `trail_offset` holds the trigger away from the extreme
the tape has reached, exactly as on a trailing stop market. `limit_offset` holds
the limit away from that trigger, on the side the order can fill from: a sell
rests at `trigger - limit_offset`, a buy at `trigger + limit_offset`. The limit
price is derived rather than consumer-stated, at acceptance, on every ratchet
and on an amended trigger, all through one function, so the two can never
disagree about which side of the trigger the limit belongs on and the limit can
never drift out of reach as the trigger advances. A consumer-stated price is
refused on this type for the same reason - the first ratchet would overwrite it -
and refused on the amend path as well as at submit, or a consumer could state
through a modify what the front door will not let it state directly.

What the second distance buys is a floor on the exit, and it bites in the gap
case rather than the ordinary one. A sell's limit sits below its trigger, so a
print that reaches the trigger is normally through the limit too and fills at
once. When the tape gaps past both, the trigger fires and the limit is not
reachable, so the order rests instead of dumping into the hole - where a
trailing stop market would have taken whatever the gap offered. Which of those
a strategy wants is a real choice and the venue makes it for nobody.

## Order lists, linkage, and atomic admission

Order lists are a primitive, not a workaround. A linkage is a group id plus a
rule that each member carries - one-cancels-the-other, one-triggers-the-other,
one-updates-the-other - and the venue holds no list object, only what each order
says about the orders it names. The rule is applied where the fill is committed,
between sweep results and never after the batch: a tape span can cross both legs
of a bracket at once, so a sibling reaped on a later pass would already have
filled against the same prints. A child (`parent_order_id`) rests held -
accepted, answerable, scanned by nothing and placing no hold - until its
parent's first fill releases it into the state it would have been submitted
into, drawing a fresh band trigger and taking its hold then. A parent that goes
terminal without filling reaps its held children in the same batch.

The depth rule - a child may not itself be a parent, and a parent carries at
most `MAX_LINKED_ORDERS` children - is what keeps a cancel's byte reservation
computable: reaping is one generation, so `sizing::LINKAGE_MAX_BYTES` bounds it
in advance. See `docs/order-lists.md` for the consumer-facing rules.

Admission is atomic, and a linked order may not travel alone. A group arrives as
one `Command::SubmitOrderGroup` and a linked bare `SubmitOrder` is refused
at the protocol boundary. That refusal is the load-bearing half: applying a rule
where the fill is committed bounds nothing if a sibling has not yet been
admitted, which is exactly what per-leg dispatch produces - the entry fills, the
shrink adjusts an order that is not on the book, and the stop then arrives at
full size beside an open position, so a two-leg `Ouo` pair's aggregate fill is
twice the bracket quantity. The group runs in two passes under one lock: a dry
validation of every member against the book and against the group's own ids,
which mutates nothing and refuses the whole frame on one bad member, then the
ordinary submit path per member at one instant against one reading. A closing
linkage pass then applies the rule of every member that filled, against the
whole group, before the call returns - which is what covers the siblings
admitted after the fill that adjusts them. That pass is the sole application of
a member's linkage: the submit path suppresses its own, because `Ouo` subtracts
the filled quantity rather than setting a target, so applying it at the fill and
again at the close would shrink an already-resting sibling twice and cancel a
stop-first bracket's stop outright. The quantity that closing pass shrinks by is
the booked fill, returned out of the submit path by reference, never a quantity
summed back out of the emitted events: the wire is the layer this venue
deliberately corrupts, and `DuplicateNextFill` emits the same fill twice with
`last_qty` and all. Reconstructing the number from that stream produced exactly
the naked position the group frame exists to prevent, so no engine control flow
is derived from a `Vec<VenueMessage>` a divergence has touched. The only two
production readers of such a batch that remain are `account_changed`, which asks
presence rather than magnitude, and the group's own pass-two refusal check,
which reads a rejection no divergence fabricates per member.

The residual is funds. The dry pass reads the book as it is before the group
runs, so a member the venue can no longer fund once an earlier member's fill has
spent the balance is rejected on the second pass, with its earlier siblings
already accepted. Atomic admission therefore covers everything the venue can
decide in advance and not a balance the group's own fills moved; a group whose
members are jointly affordable at submission never meets it.

The two passes are only as good as their agreement, and asserting that they
agree is what keeps them honest. The standing invariant is that no refusal may
reach a submit from outside `Engine::dry_refusal` - the dry pass and the real
path ask that one function, so a refusal added to only one of them is the whole
defect family in a single line. Five atomicity bugs came from it being
unwritten: a hedging `position_id` rule that lived outside the validator, a link
validated without the group's own ids on the second pass, a non-idempotent
`Ouo` applied twice, and then two the invariant did not catch because the two
passes called the same function with different arguments - the dry pass asked
`validate_submit` with `apply_divergences` false, and that one flag also gates
the venue-reserved id-prefix refusal and the armed fee surcharge, so a member
carrying an `LQ-` id or priced between the two commission thresholds was
admitted dry and refused real. The dry pass now asks with the flag true, which
is correct because a group is always consumer-submitted; the flag remains
overloaded, and splitting the arm-spending meaning from the two order-property
meanings is the durable fix. Because nothing can detect the next such mismatch by
construction, the group checks itself instead: a member refused on pass two
re-asks the dry question against the state as it now stands, and a dry pass that
would admit what the real path just refused is a defect rather than the funds
carve-out, reported as an error and asserted on in debug builds. The
discrimination is deliberately not a match on the reason text, which would pin
the check to today's wording and stay silent about a refusal added tomorrow.

## Conditionals, trails, and tick resolution without per-tick evaluation

A stop and a touched order are the same machinery with opposite comparisons. A
stop protects - buy above the market, sell below - and fires when price runs
away. A touched order enters - buy below, sell above - and fires when price comes
toward it. Both fire on touch rather than through, because a conditional holds no
queue position. The two predicates are separate functions and separate
`ScanKind`s rather than one function with a flag, since they are the two most
easily confused behaviours in the venue.

A trailing stop's trigger ratchets with the tape and never retreats: a sell trail
rises with the high and stays put when price falls back. It is advanced against
the span's extreme rather than its closing mark, so a spike between two sweep
passes drags it exactly as a tick-resolution venue would.

Tick resolution without per-tick evaluation, and this is one mechanism serving
both the trail and the risk policy. The tape thread records the high and the low
its river reached since the sweeper last looked, with the instant of each; the
sweeper takes that span once per pass. A trail is a monotone function of its own
river's tape, so the maximum over a span's ticks is exactly the span's high, and
that holds per symbol however many rivers the account rides. Equity is linear in
each price it depends on, so for an account holding one marked symbol its extreme
over the span sits at that symbol's price extreme and the two readings carry the
whole answer. An account riding several rivers is judged once per due boat, with
that boat's span carrying its river's extremes while every other symbol
contributes at its last mark, so the cross-river component of equity is evaluated
at mark cadence rather than at tick resolution. That is a stated bound of the
model, not a hidden defect, and it is written out again below where the risk
policy is described.

The policy observes the two extremes in the order the tape reached them and then
the close: a spike that opened and closed between passes spends drawdown budget,
and a collapse that recovered before the pass breaches. Order matters and is not
a detail - replaying favourable-first would invent breaches that never happened.
The tape thread's cost is two comparisons and two atomic loads per tick, and it
takes the mutex only when it has something to hand over - an extreme that
moved, or a print that found the span already taken.

The handoff between the two threads is an epoch the reader bumps as it takes.
Every print belongs to whichever span was open when it happened, including a
print that raced the take and one that moved neither of the old span's
extremes: the writer re-reads the epoch before it publishes, and a print that
finds the epoch moved opens the new span from itself rather than being dropped
under a stamp the reader will no longer accept. Nothing else in the venue may
assume the epoch read at the top of a record is still current at its end.

An unpaced tape is the exception to where those extremes are accumulated, not
to what the policy observes. Its publisher can run ahead of the boat clock, so
the sweeper regenerates the clock-bounded span from the same river instead of
consuming the publisher's future extremes. The regeneration reads the same
realization the tape thread published, so a surged river's extremes are the
surged ones.

## Time in force, and why an expiry is not a cancel

Time-in-force covers Gtc, Ioc, Fok, Day and Gtd. A conditional may be Day or Gtd
- both can wait for a trigger - but never Ioc or Fok, which cannot wait for
anything. Expiry is a time-driven pass with nothing to do with triggers: a Gtd
order stops resting at its instant whether or not the tape came near it, and a
Day order stops when its own instrument's session closes, which the sweeper
detects by asking the calendar whether the span it swept crossed from open to
shut. An instrument with no calendar supplies no such instant, so a day order on
a 24/7 symbol rests like a Gtc - the honest answer, since inventing midnight UTC
would expire orders at a time that market has never heard of.

An expiry is not a cancel, and the wire says so: expiry ends an order with
`VenueMessage::OrderExpired` and a terminal `Expired` status, never
`OrderCanceled`. A cancel is an actor's decision - a consumer's, or the venue's
under havoc or a risk breach - while an expiry is the clock reaching a lifetime
the consumer itself stated at submit. A host reconciling the two acts on them
differently, and nautilus carries the same distinction as `OrderStatus::Expired`
and an `OrderExpired` event, so the adapter maps it straight through rather than
collapsing it at the last seam that could keep it. This reported `Canceled`
until 2026-08-18, on the argument that no consumer matched the difference; that
is the argument the order-type completeness ruling overturned, since the venue's
surface is not sized against a consumer's current catalog.

## The six instrument classes

The ledger models six instrument classes, split by settlement shape rather
than by asset class, because the shape is what decides how holding one moves the
ledger.

- `Spot` credits the base asset as a currency balance. Right for crypto spot,
  where the base genuinely is money you can spend on the next pair.
- `Forex` carries a marked leveraged position and pays the configured long or
  short swap when its daily rollover boundary is crossed.
- `Equity` credits a position and never a balance. A share is not money, and
  modelling it as `Spot { base: "AAPL" }` put it on the same footing as USD -
  which is what made short sales, settlement periods and round lots
  inexpressible. Cash moves by the full notional on both sides; the shares are
  the position. All three conventions are now expressed on the class, and the
  next paragraph is what they mean.
- `Future` moves only settlement cash, with exposure carried as a marked
  position.
- `Perpetual` is a future that pays funding between long and short at an
  interval. With no expiry to converge at, funding is the only thing tying it to
  spot, so a perpetual without it reports P and L that is wrong by construction.
  Funding is paid on notional at the mark standing at each instant, on instants
  that sit on multiples of the interval from the unix epoch - a property of the
  clock, so the schedule cannot depend on when a run booted or how the sweep
  passes were cut. The configured `funding_rate` is the zero-premium interest.
  When the class names an `index_symbol` and that symbol's river is
  materialized, the live rate at an instant is
  `clamp(interest + (mark - index) / index, +/- funding_clamp)`, both prices
  read at that instant. No index mark means a zero premium: reading an index
  never spends a river nobody asked for, so a perp-only venue keeps the
  configured rate. An instant is still honoured on the sweep pass that crosses
  it, but it is priced at its own instant, through the same enumerator and the
  same rate rule the published `FundingRate` frames use - so the cash the
  ledger moves reconciles with the frames on the wire. The sweep scopes
  funding to the swept boat's own symbol, exactly as it scopes marks and
  settlements, which is what keeps an account seated on several boats from
  being charged one instant once per boat.
- `Inverse` is coin-margined: quoted in one currency, settled in another. Value
  is `multiplier * qty / price` rather than `multiplier * qty * price`, so P and
  L is non-linear and a long is not the mirror of a short. `InstrumentDef`
  carries the one implementation of both forms, so realized and unrealized can
  never disagree.

## Equity accounts, margin, and holds

An equity is a cash account or a margin account, and the margin policy is which.
That distinction is what decides what an equity account may do, and it is
enforced rather than reported:

- A cash account (no margin policy on the symbol) pays the whole notional on a
  buy and may never sell short at any price. The refusal names the reason rather
  than reading as a funding shortfall, because shorting is not something a
  larger balance would buy.
- A margin account posts the Reg-T requirement - `basis = "notional"`, `initial
  = 0.5`, `maintenance = 0.25` - and borrows the rest, so the settlement balance
  goes negative by the loan while the shares sit on the other side of it. The
  account is worth what it was; `valuation_in` counts an equity at its market
  value rather than its unrealized, because the cash already moved by the whole
  notional. The maintenance walk measures the same way, which is what makes a
  margin call reachable at all.

The locate is `borrowable`: absent means the venue models no borrow market, `0`
states a name nobody will lend, and any other value caps the account's net short.
The settlement period is `settlement_ns`: a sale's proceeds are credited at once
and held unspendable until the span has run, appearing as `locked` on the balance
row - which is what a `T+N` convention actually is to a strategy. It is a fixed
sim span rather than N sessions, and that simplification is stated rather than
hidden. The round lot is `lot_size`, and it governs what may be submitted or
amended to rather than what the size grid can represent: a partial fill
legitimately leaves an odd-lot remainder, so `size_increment` stays at one
share. Submit and modify enforce the same grid, so a resting order cannot drift
off it into a state a fresh submit would have refused.

Executable margin-equity sells share one pool of long shares, so their hold is
an aggregate over the symbol rather than a sum of per-order holds, and that
aggregate is computed twice - once for the `locked` balance and once for the
`initial` margin row the same snapshot reports. The two folds must count the
same orders. In particular a sell carrying neither a price nor a trigger price
contributes its quantity to both and a price to neither, so an order with no
price cannot raise the reported requirement above the balance actually held
against it. Nothing on the wire rests such an order today - every order type is
validated into carrying one of the two, and a modify replaces a price rather
than removing it - and the rule is stated over the folds anyway, because a
reconciliation that holds only for the inputs validation happens to admit is
one validation change away from being false.

Margin has two bases. `per_contract` is a fixed amount of settlement currency
however the price moves, which is what CME publishes and what every shipped
preset states. `notional` is a fraction of notional, so the requirement moves
with the price - that is what forex, crypto margin and Reg-T equity margin
actually do, and it is the leveraged account the venue previously had no way to
express: ten-times leverage is `initial = 0.1`. The maintenance walk asks the
policy rather than multiplying `maintenance_per_contract` by a contract count,
which read a notional-basis fraction as a per-contract amount and left a
leveraged account unable to breach at any price.

## Funding, valuation and the policy currency

Funding is checked per account at bind. The venue's `[balances]` is only what an
unnamed account opens with, so a consumer that named its own funding cannot be
checked at boot. It is still knowable with no order at all, so a socket binding
a symbol its account holds no balance line for is refused before the upgrade,
naming the account and the currency. Presence, never sufficiency: running out is
depletion, and a funds rejection on a served shape has to keep meaning that and
only that.

A policy names the currency its thresholds are stated in, and the account is
valued in it. A spot fill credits the base asset as a currency balance and
debits the quote, so an account trading spot holds an asset that has to be
priced before its equity means anything. The engine keeps a last mark per
symbol for every class, the sweeper prices every pair whose base the account
holds, and `Engine::valuation_in` sums that currency's balance, each other
balance valued through an instrument quoting it in that currency, and the
unrealized on futures settling in it. Opening refuses foreign balances on a
policed account. Boarding then refuses a shape that does not settle in the
policy currency before claiming the account, and order entry applies the same
one-hop predicate. The sweeper warn remains a backstop for a spot fill swept
before its first mark and for the base-asset line valued through the admitted
pair's one hop; it never guesses a rate.

When several priced instruments quote the same held currency into the policy
currency, valuation uses the lexically first symbol. The choice is a stable
tie-breaker for an ambiguous request, not a claim that the pairs share a market.

Valuation is one hop: an asset is priced only through an instrument quoting it
directly in the policy currency, never through a chain. There is no rate
surface.

The policy is evaluated at tick resolution, through the span of extremes the
tape thread records rather than through a per-tick evaluation. See the trailing
stop above for the mechanism: a spike lasting a fraction of a sweep interval
spends drawdown budget, and a collapse that recovers before the pass still
breaches.

Where the exactness argument stops, stated rather than hidden: an account holding
more than one marked symbol. An account rides as many rivers as its passengers
have boarded, and the sweeper judges its policy once per due boat, so the span in
hand covers that boat's river alone. Equity is then a sum of linear terms whose
extremes need not coincide and need not sit at either river's extreme, and only
the swept river's symbol carries a span - every other symbol contributes at its
last mark. So a multi-river account's peak-equity ratchet sees a partial
reconstruction of the interval, ordered by whichever boat came due first, and its
cross-river component moves at mark cadence. The per-river half is unaffected: a
trailing stop is monotone in its own river's tape, so it remains exact per
symbol. Evaluation is therefore exact for an account on one river and bounded by
mark cadence across rivers, which is a property of the model to know about rather
than a defect to hide.

## Symbol resolution, the river cap, and what `/instruments` advertises

Symbol resolution is total over wire-legal labels. Configured profiles are
held directly and other profiles are memoized without a cap. The permanent,
expensive checkpoint chains are capped instead: creation of the 257th river is
refused atomically, with no eviction. A `RiverKey` includes the exact requested
label, its per-label tape seed, and the resolved bundle digest, so two labels
wearing the same default shape still own independent water.

`/instruments` therefore answers the union of the configured shapes and every
shape this run has materialized a river for - materialized, not merely
resolved, because resolution is total and a memo-shaped list would advertise
labels nothing had registered. A socket bind and a history poll spend the same
river budget, so the advertised set grows exactly when the capped resource
does. The engine's instrument set grows on the same demand: `Run::ensure_instrument`
registers a def and installs its margin policy and fee schedule the first time
a socket binds that symbol or an order names it, guarded on the registration
having been new so re-binding never resets a live configuration.

## The route surface

The venue exposes `/health`, `/account`, `/accounts`, `/instruments`, `/clock`,
`/operator/trades`, `/operator/quotes`, `/control/divergence`, and `/ws`. A
consumer reads its own history over `/ws` with `QueryHistory`; the two operator
routes serve the unarmed river of a label on the run clock, and are namespaced
so that difference is visible on the wire rather than only in prose.
`POST /accounts` opens
an account on terms the consumer states - an id, its opening balances, and
optionally the risk policy the venue enforces against it - and is optional:
account resolution is total, so a connection that never calls it is served under
the default account, unpoliced. A policy the venue cannot enforce is refused
where it enters rather than hours later. Structured account config goes over HTTP for
the same reason a divergence does, and only the id crosses the socket upgrade.
Re-opening an account that already exists is a `409` rather than a reset,
because an account outlives its connections and the request cannot be told
apart from a reconnecting consumer re-sending its config.
`GET /account` names whose ledger with `?account=`, defaulting the same way.
Order entry is WebSocket-only: the
`POST /orders` carrier went with the HTTP transport profiles. Each socket feeds
one bounded sequential dispatcher, so admitted commands reach the market read
and engine in socket arrival order even when their modeled act latencies differ.
The queue and a process-wide permit bound parsed command work before it reaches
the blocking pool or engine mutex, and a full bound is a visible
`AdmissionRejected` the engine never sees.

## The two output-byte admissions

There are two output-byte admissions on the order path, they answer different
questions, and only one of them is the malformed-versus-capacity contract.

Engine-output admission is `worst_case_output_bytes` against the book shape,
taken under the engine lock just before the engine is allowed to mutate. It is
reached only after protocol validation has cleared the command, so a malformed
group never reaches it and cannot be reported as venue capacity. That ordering
is part of the refusal contract rather than an optimization, and it is now
enforced rather than remembered: `boundary_outcome` mints a `BoundaryCleared`
witness on the arm where its own validation found no fault, `ExecLanes::reserve`
demands one, and the witness's field is private to the boundary module, so no
future call site can size engine output for a command nobody validated. The
sizing function still charges the actual member count rather than clamping it at
`MAX_GROUP_ORDERS` - the count is exact, so the bound cannot undercount whatever
it is handed, and a clamp would stop being an upper bound the moment an invalid
command did get through. The ordering, not a clamp, is what keeps the number
operationally small.

Boundary-refusal admission is the other one, and it exists because the command
is malformed. A refusal is a produced frame, so it is charged too: one
`BOUNDARY_REFUSAL_BYTES` per frame the refusal will write, which for a group is
one per member that arrived. That count is deliberately not clamped either -
this path has no validated member count to use, since an over-long group is one
of the things validation refuses, and clamping would reserve for fewer frames
than the refusal writes. `MAX_INBOUND_MESSAGE_BYTES` is what bounds it: a 64 KiB
decoded frame cannot carry an unbounded member list. If even that small
reservation cannot be met, the consumer is answered a retryable
`AdmissionRejected` rather than the malformed refusal, because the venue has no
output budget in which to state the refusal. So capacity can preempt a malformed
verdict here, and the verdict is deferred rather than lost - the retry that
finds budget gets it. The stronger claim, that a malformed request is never
reported as capacity, is true of engine-output admission and of that alone.

That frame carries `retryable`, as data rather than as prose, and the reason is
what happens to it downstream. A consumer's adapter must map it onto its own
stack's event for the same subject, and nautilus's `OrderRejected` has one field
an adapter may set - the reason string - so a refused submit reaches a strategy
in the same shape as "insufficient balance": terminal, and separable only by
reading the venue's wording. No consumer should hang a quarantine decision on
our prose, and one correctly refused to. So the wire states it, and
`mogwai-adapter` carries it across the boundary as its public
`RETRYABLE_REJECT_PREFIX` on the reason - an identifier this repo versions and
tests, not a sentence. Every admission refusal the venue issues today is
retryable, which is the contract rather than a redundancy: an admission refusal
means the venue was full, not that it said no. Absent decodes `false`, so a
consumer reading an older venue takes the safe reading. The claim is scoped to
admission: `HistoryRejected` carries the same field and does say no, setting it
`false` for a malformed request, an unreadable continuation, and a river cap
already spent for the life of the process.

## The upgrade query string is the whole binding

Inbound frames and reassembled
messages are capped at `MAX_INBOUND_MESSAGE_BYTES`, 64 KiB, so a dependency
default no longer sets the venue's memory bound; an oversized frame ends the
connection. A WebSocket carries its whole binding in the upgrade query string,
which `deny_unknown_fields` rejects any other key on: the optional, case-exact
`symbol` names its one river, the optional `speed` names the pacing multiple,
the optional `duration_ms` names a passenger-local simulated deadline, the
optional `account` names the ledger it trades, and the optional `callsign`
names the identity the socket presents. `window_start_ns` and `window_end_ns`
form one absolute named window and must appear together; that form is mutually
exclusive with `duration_ms`.

That last one lets several sockets present one identity. A second identity
claiming the account evicts the incumbent connections, while sockets presenting
the same callsign coexist. A nautilus host relies on that rule because its data
and execution legs name one account by construction. A different callsign evicts
every incumbent socket, and so
does an absent one, which keeps silence meaning what it always meant. The venue
reads nothing into the string beyond equality, and `mogwai-adapter` mints one per
process from the pid and start instant so a host configures nothing and a
restarted worker still reclaims its ledger from the sockets of the dead one.
Absent, they default to the run's default symbol and the configured `speed`, and
to an indefinite passenger. The key is known before any tasks or bytes exist,
a refusal - an illegal label, a shape that does not validate, a funding-barred
one, an exhausted river cap, a non-finite or negative speed, or a second
cadence on a river this account is already riding - is an HTTP 400
rather than an
ambiguous WebSocket close, and one connection still owns exactly one replay. A
frame carrier would permit multiple replays and create an unbound interval
before the first frame. The query carrier is the seam where river-keyed state,
boat placement, and per-boat clocks attach. `ws_upgrade` resolves the query
symbol, registers its instrument on the engine, resolves its `RiverKey`, and
boards a boat on that river, all before the 101; `handle_socket` then owns the
already-bound passenger. Every resolved shape owns a lazily created checkpoint
chain, keyed and locked independently, and is servable through history. Consumers
do not send subscribe frames or an account identity. The bounded fanout ring remains, and it is the only delivery slack a passenger
has. A passenger that falls behind it is told what it missed and goes on being
served: `FeedLagged` carries an episode counter, the skipped count, a cumulative
total and the two event-time boundaries of the hole, written immediately before
the first market frame delivered after it. The declaration is positional, which
is why it rides the market stream rather than the priority lane - a diagnostic
that overtook the backlog would name a boundary the reader had not reached.

## Declared feed gaps, and what is not one

Only unarmed loss is declared. A frame withheld by an armed `GoDark` or
`StallData` window is not loss, so a hole discovered while one is open is held
back until delivery resumes rather than announced into a blackout. The venue does
not claim the converse: it cannot know whether a frame the ring overwrote would
have been suppressed, because suppression is a question about a delivery attempt
that never happened, so a hole spanning a blackout is declared rather than
assumed away. A sink failure can leave an owed declaration undeliverable, and
socket termination is then the only observable.

The venue never ends a connection for lag. What to do about a lossy view is the
consumer's decision, and a rising episode count with no close is how a passenger
that fell behind once is told apart from one whose sustainable read rate is below
its boat's publish rate.

A river's tape root is derived from the run seed and the requested symbol
label, not from the shape the label resolves to, so a run serving several
symbols serves several genuinely different rivers. A run stays a pure function
of `(seed, config)`; a river is a pure function of `(seed, label, resolved
bundle)`. The seeding rules are set out with the run seed below.

## How an order rests, triggers and fills

Execution output that no command asked for is delivered to the account it
concerns, not broadcast: a submitting connection claims its order at acceptance,
and a venue-originated order - a risk or margin liquidation the venue mints - is
claimed for the account whose ledger produced it. An order absent from that table
is a bug in whoever built the batch, and the fallback of delivering it everywhere
with a warning naming the id is the conservative direction to fail in while such
a bug lives, not the ordinary path. Where the venue mints several such orders at
once - a margin cascade, a risk flatten, an off-river retirement - it sorts the
positions by symbol and position id before minting, because the sequence number
in the `LQ-` or `RISK-` id, the venue order ids behind them, the trade ids and
the event order all follow from that order, and the positions themselves are
held in a `HashMap`. Determinism per binary is a contract over everything the
same seed and config produce, and it is not satisfied by a set the run happened
to iterate one way. A
resting order is one of two explicit states: a live limit or an untriggered
conditional. Every resting limit
carries a trigger price drawn once at submit from a seeded, volatility-scaled
band around its stated price (`fill_band_vol_mult = 0.0` degenerates to a
strict through-at-the-stated-price fill), and it fills only when the run's
fill sweep walks a print strictly through that trigger; the fill is delivered
to each connection's lanes from the run, not from a command response. A market
order crosses the opposing quoted touch and a parametric ladder. Level zero has
the published quote size; later levels are one price increment farther away and
grow by the preset's decimal factor. The result is an adversely snapped
volume-weighted average. A marketable limit walks the same ladder only through
its stated price and rests its remainder. Exhausted market quantity is canceled
and logged as `insufficient displayed depth`; it never rests. FOK plans the
bounded walk before committing a fill, while IOC cancels its remainder.

The ladder is exogenous water. Each order sees it fresh, so no passenger can
consume depth another passenger would otherwise observe. This makes slicing
cheaper than one order of the same total size; the accepted alternative would
be stateful depletion and passenger interference. Any later mitigation belongs
in a per-account transient impact term.

The trigger walk carries the quote with each optional hit. On the current
64-bit build, `Option<Hit>` is 120 bytes, up from 32 bytes before the quote and
ladder fields were added. The widening is 88 bytes per pending scan for one
sweep pass; it is transient and scales with the scans gathered for that pass.

The trailing-volatility band is only a resting-limit queue-position offset. It
is cached per river and sweep interval, while the quote half of a reading comes
from the boat's retained quote series at the command's exact instant. A cold
volatility estimator yields a zero offset, not a perfect taking fill. A taking
submit without a book is rejected as `no market data available`; a triggered
order without its hit-instant book is canceled with the same diagnostic.
Sweep marks and settlement prices are separate exact-instant last-print reads;
the coarse band cache never supplies unrealized P&L.
A conditional (`StopMarket`/`StopLimit`) rests untriggered until the
same sweep walks a print that touches its stop price - the mirror-image
predicate, since a stop holds no queue position and every real venue fires one
on touch rather than through. On trigger the venue emits `OrderTriggered` and,
in the same batch, either crosses a stop-market against the book at the
triggering instant, or promotes a stop-limit to a live limit judged against that
same book (filling at once if already marketable, resting with a fresh band
otherwise - it does not manufacture a fill through a gapped print). Reduce-only
and post-only are first-class wire flags: reduce-only clamps every fill to the
position it would close and cancels the order once that position is gone;
post-only rejects an order, at submit, at a price amend or at trigger, that
would take liquidity rather than filling it. Trailing stops carry per-order
trailing state and two-leg brackets carry a linkage each member holds, both
described above. No order book
exists: orders never interact, so self-trade within one account is impossible
rather than prevented, and every fill is judged only against the tape - and a
linked sibling is reaped by the venue's own rule rather than by one order
meeting another.

