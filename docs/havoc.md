# mogwai havoc

Havoc is armed per account or per run, never per connection, and which of the
two a given arm takes is the arm's own property rather than its family's. The
four transport controls take the request's `account` and corrupt only that
account's view; a silent cancel searches the named account's book. The engine
one-shots and `FeeSurcharge` are recorded on the run whatever the request names,
because they are statements about the venue's matching and its fees - which is
also what makes a ledger opened later carry them. `FaultTape` refuses an account
scope outright. The per-arm detail is below. Naming an account on an arm the
venue records against the run is accepted and then ignored, which is worth
knowing before writing a scenario that believes it scoped one: read the arm's own
entry rather than assuming the field applies because the request carries it.
Order-path divergences operate on the account's ledger; data-path divergences
operate on the selected river or on the account's view. Admission and execution
lanes remain connection-local memory bounds. Order-path arms apply only to
consumer-originated orders, which reach the venue over the websocket carrier and
nowhere else. Venue-originated maintenance, including forced
liquidation, bypasses them and leaves them armed for the next matching consumer
action.

Two nouns run through what follows and `reference/glossary.md` defines both. A
river is the generated sequence for one resolved instrument shape, keyed by the
requested symbol plus that shape's knobs - not by the symbol alone, which
matters here because a generator arm is part of that key. A boat is the paced
reader sitting on a river, placed when the first connection boards it at a given
speed, and it carries the clock every answer about that river is dated on.

The venue keeps one wall-to-sim reference, and it answers every request that owns
no boat: history, the venue deadline, the venue-scoped account ledger, `/clock`.
It is deliberately not any boat's, because a boat's own instant belongs to the
passengers riding it. What a passenger receives is still dated on its boat, so a
havoc window cannot be an interval on one clock.

An armed `CommandLatency` act delay is head-of-line on its socket. Each
connection feeds one sequential dispatcher, so a delayed submit holds every
later command from the same socket behind it - which is what stops a cancel
from overtaking the submit it cancels. Concurrent in-flight commands under an
armed latency need several sockets.

`DelayAcks`, `CommandLatency`, `GoDark`, `StallData`, partial fills, rejects,
duplicate fills, and blackouts keep their existing fidelity meanings. `GoDark`
suppresses a connection's output wholesale; `StallData` suppresses market data
only, so a venue heartbeat still arrives and a stalled feed stays
distinguishable from a dead venue.

Transport havoc reshapes bars rather than dropping them, and that is deliberate.
The venue ships no bars: every bar a nautilus host receives is fabricated by the
adapter by folding the trades it was delivered. Dropping or duplicating a trade
therefore changes a bar's open, high, low, close or volume rather than removing
or duplicating a whole bar frame, because the fold happens downstream of the
filter. That is what a real consumer-side aggregator on a lossy feed experiences,
so it is the honest simulation of this venue; modelling a dropped bar would be
modelling a venue that ships bars natively, which mogwai is not. It follows the
same principle as the rest of the surface: mogwai injects faults and declines to
repair them downstream.

An overrun ring is not havoc, and the venue keeps the two apart on the wire. A
window you armed withholds frames deliberately and is never reported as loss; a
passenger that falls behind the bounded fanout ring has lost frames nobody armed,
and that is declared with `FeedLagged` before the next market frame it receives.
A hole discovered while a blackout is open waits for the blackout to lift rather
than being announced into it. The venue does not close the socket for either.

What a consumer can do about it depends on the consumer, and the nautilus case is
the constrained one. The venue declares the hole on the wire: `FeedLagged`
carries the skipped count and the two boundaries of the affected span, so a
consumer reading the protocol directly can tell a
quiet feed from a lossy one rather than inferring it from bar shape. A nautilus
host cannot: `DataEvent` has no gap or degradation variant, the adapter object
reaches the host as a `dyn DataClient` with no downcast, and fabricating an
`InstrumentStatus` would report a venue halt that did not happen - so
`mogwai-adapter` logs the frame at the `error` level, the level a host alerts on, and
that log line is the only channel the signal has. A bar-folding strategy is
therefore not in a position to read it, and a run that needs the distinction
programmatically wants a consumer on the raw protocol. The real fix is a declared
feed-gap event upstream, which is owed rather than landed.

Generator havoc is not a control post at all. It rides the websocket upgrade,
in four query keys, and it selects which river the passenger boards:

    /ws?symbol=MNQ&surge_start_ms=0&surge_duration_ms=60000&surge_rate_mult=4&surge_children_mult=2

A passenger carrying an arm boards a different river than one without it. That
is the whole mechanism, and it is why generator havoc no longer needs an empty
boatyard: nothing is mutated, so nothing that anyone is already reading can
change under them. Two accounts can run a clean strategy and a surged one on one
exchange, at the same time, on the same symbol, and neither sees the other's
weather. Ask for the same arm twice and you get the same river, which is what
keeps sharing intact - the boat, the checkpoint chain and the warmup are all
paid once.

It rides the upgrade rather than a control post for a reason worth stating,
because the control post is the obvious design and it is wrong here. A posted
arm is run-wide state: on a server-mode venue it would let one consumer decide
what every other account's next boarding resolves to, which is cross-account
interference arriving through the control plane rather than through the water.
And a registry of named water shapes in the venue's config would be worse for
the attached case, where the config file belongs to whoever launched the
exchange and the consumer is precisely the party that does not own it.

`surge_start_ms` is an offset from the run origin, not from the moment you
connect. That is what lets two passengers share one river: "starting when I
connect" names a different window for every boarding instant, so it would fork a
river per connection and share nothing. The consequence to expect, and it is
deliberate: boarding late with a zero offset boards water whose surge is already
over. The river had its weather whether or not anyone was aboard, the same way
it had a Tuesday. A harness that wants a surge beginning at its own connection
computes the offset against the run origin first and reuses that one arm for
every leg of the passenger - which a nautilus host must do anyway, since its
data and execution legs have to carry identical water or the strategy prices
against one market and is filled in another.

The multipliers are canonicalized to parts per million before they become
identity, so two spellings of one multiplier are one river. Without that, a
scenario that accumulates `1.1 + 2.2` where another writes `3.3` would strand
two rivers instead of sharing one, and the river cap does not evict.

A surge is never lifted: it ends when its window expires. There is no control
that takes it back off the water, because there is no water to take it off - a
different arm is a different river, and you leave one by leaving the boat.

What this costs, stated because the cap is real. Every distinct arm materializes
a river, rivers are never evicted, and the run's cap is 256. A scenario sweep
across a hundred multiplier values spends a hundred rivers for the run's whole
life, not for the life of the socket that asked. Sweeps larger than the cap want
one venue process per batch.

Transport controls remain runtime-armable and are armed per account. `GoDark`,
`StallData`, `DelayAcks` and `CommandLatency` all take an optional `account` on
the request and corrupt only that account's view; naming none arms every
account, which is what a single-account venue wants and what an existing
scenario file already writes, so nothing on the wire breaks. They ride the
account rather than the venue because they change what one connection receives,
or when it hears about its own commands, rather than what the generator
produces - so on a shared exchange, blacking out or slowing one subagent leaves
the rest of the batch untouched.

Every control post has one request shape: `kind` names the divergence, `args`
contains only that kind's fields, and the optional `account` and `symbol` scope
sit beside them. For example:

    {"kind":"GoDark","args":{"ms":60000},"account":"WYRD-500"}

Unknown top-level fields and unknown fields inside `args` are refused. Every
successful post answers `202` with a JSON object whose `status` is `accepted`;
`detail` and `evicted` appear only when there is collateral information to
report. A scenario driver never has to distinguish an empty body, prose and a
debug rendering.

The control plane arms and never disarms. There is no clear: the route off an
armed window is to re-arm it with a zero span, which is closed on every reader's
clock, and it is scoped exactly like any other arm - naming an account lifts
that account's window and naming none lifts every account's. `GoDark { ms: 0 }`
and `StallData { ms: 0 }` lift a blackout early, `DelayAcks { ms: 0 }` releases
acknowledgements already queued because the writer reads that window per event
at dequeue, and a `CommandLatency` arm with every field omitted zeroes all six.
What no re-arm reaches is an act delay the venue has already begun serving: that
command sleeps out its full window and then mutates, because a venue that has
begun acting does not un-begin.

What is armed before a consumer dials is one-way. An arm recorded against an
account that does not exist yet is spent when that account opens, or when the
run ends, and nothing retracts it in between - pre-boarding havoc setup is run
construction, so a setup that fails partway is rolled back by discarding the run
and starting another. Two consequences worth stating because a harness will meet
both. Engine arms append rather than replace, so a retried setup leaves the
eventual account carrying one-shots from every abandoned attempt rather than
just the intended ones. And a pending record is retained, never guaranteed: the
venue holds arms for a bounded number of unopened names and sheds the oldest, so
arms posted for unrelated accounts can drop one that was already waiting.

An arm does not wait for a connection, in either spelling - with the lede's
caveat that the named spelling reaches only the arms the venue routes by
account. Naming an account that has not connected yet records a transport arm
against that name, and the account's first ledger - whether it is minted by a
socket or by the consumer's own `POST /accounts` - opens carrying it. Naming
none records the arm on the run itself, so every ledger opened afterwards
carries it too, engine divergences and the fee surcharge included - and since
the venue records those against the run whatever the request names, naming none
is also the only spelling that arms one for a future account. Both used to reach only the accounts that happened to exist
at the instant of the request while answering `202` either way, so arming a
subagent before starting it did nothing and said nothing. An `account` the venue
cannot parse as an id is now a `400` rather than a `202` that arms nothing.

Recording an arm does not open an account, deliberately: the consumer still
states its own opening balances and policy on `POST /accounts`, and finds the
arm standing on the ledger that call returns.

The market regimes - `VolStorm`, `LiquidityDrought` and `ReopenGap` - are the
same mechanism at a different scope. They are a boot choice made by whoever
launches the run, apply to the whole run's water, and enter every river's
identity, so a regime run serves different rivers rather than mutating one. A
surge is now that idea per passenger rather than per run, which is why neither
needs a control post and neither can reach water anyone is reading.
`LiquidityDrought` is the inverse
rate control to a surge: it stretches parent gaps while leaving sweep shape
unchanged. `mogwai gen` takes the same regimes offline.

Every timed havoc window, including transport windows and `FeeSurcharge`, is
measured in simulated milliseconds on the receiving passenger's clock. The
window is stored as the wall instant it was armed at plus a simulated span, so
each reader judges the span on its own boat's clock: a passenger that boards
after the arm receives the full declared span from its own boarding instant,
and so does a boat placed after the arm, which opens the window at its own
epoch. Re-arming a window replaces it outright rather than extending it, so a
smaller `ms` shortens a blackout already running.

`FeeSurcharge { mult, window_ms }` multiplies the configured maker or taker
charge for fills inside one simulated-time window. The multiplier is restricted
to `(0, 100]`, the duration to one hour, and a later arm replaces the earlier
window outright. Whether it applies is a pure function of the fill's simulated
timestamp on the clock of the boat that booked it, so a later fill cannot erase
the window for a replayed earlier timestamp, and two boats running at different
speeds each pay over the same number of simulated milliseconds. A
venue-originated fill does not pay the surcharge.

`CancelOpenOrderSilently` takes its clock and symbol from the targeted resting
order. If the control request also supplies `symbol`, a mismatch is refused
with HTTP 400 rather than using the supplied symbol to choose a clock.

`DropNextAccountUpdate` swallows the next account snapshot that follows an
order executing or leaving the book - a fill, a cancel that frees a resting
order's hold, a funds-check eviction during a sweep, a stop trigger that booked
any of these. It is deliberately not spent on an order joining the book, even
though the hold that joining places does move `locked`: acceptance
necessarily precedes the fill, so an arm consumed there could never reach the
event a scenario author aimed it at. That one carve-out is the whole of the
asymmetry - everywhere else the question is whether an order's state actually
transitioned, not whether the transition was a fill.

`FaultTape` kills the venue. The run reports a source fault, tears down, and
the process exits nonzero, with an error line on stderr naming the operator as
the cause. It is the one arm a consumer is least likely to have exercised: a
strategy that survives a blackout, a duplicate fill and a dropped account update
may still have no answer for the venue simply going away mid-run, which a real
broker does and which no in-process backtest can produce.

Three things follow from it being terminal. It cannot be scoped to an account -
a request naming one is refused with HTTP 400 rather than silently widened,
because killing a whole run when a scenario meant to perturb one ledger is not a
generous reading. Nothing takes it back, which is true of every arm here but
differently so: the others are spent or expire, while this one has no venue left
to be spent on. And it is never queued or replayed onto a later ledger, unlike
the engine-armed set. Posting it is the last thing a scenario does.

A second `FaultTape` arriving while the venue is already tearing down answers
`202` and says so in the body, rather than failing: the state it asked for is
the state the venue is in.

It exists partly because the alternatives closed. A venue fault used to be
reachable only by configuring an arrival family out of its usable range, and
both knobs that allowed that are bounded at admission now - so without this arm
the venue's fault-exit path would have no door at all.

A planned run completion is not havoc: it emits `RunComplete` and closes
normally. That announcement is exempt from both suppression windows - a venue
that reached its declared duration says so even mid-blackout, because dropping
the frame would make a planned completion look like exactly the death `GoDark`
is imitating, which is the confusion `RunComplete` exists to end.

There is no deliverability refusal any more. It existed to reject divergences an
HTTP transport profile could not carry, and with the profiles gone every
divergence is deliverable over the one websocket carrier.

The protocol validator is the arming boundary for divergence payloads.
Single-shot client-order-id targets must be valid submit ids, so an unmatchable
arm cannot remain queued forever. `RejectNextSubmit.reason` is refused above
`MAX_REASON_LEN`; the engine echoes it into an order event whose byte reservation
depends on that cap.

## Havoc against a conditional order

A conditional (`StopMarket`/`StopLimit`) has three lifecycle points a plain
order does not - its submit, its trigger, and the order the trigger produces -
and every arm reaches it somewhere on that longer lifecycle. No arm is
carved out and no new arm exists for the trigger itself.

| Arm | Where it lands on a conditional |
|---|---|
| `RejectNextSubmit` | The submit. The conditional never exists, so nothing can trigger. |
| `RejectNextCancel` | A cancel for a resting order, refusing it and leaving the order where it was. Not spent on an unknown or already-terminal id, which would be refused anyway and would look like the arm failing to fire. The point is what the consumer is left believing: publish a replacement before the cancel is acknowledged, have the cancel refused, and two orders rest where the script rests one. |
| `PartialFillNext` | The fill the trigger produces, never the trigger itself. An untriggered stop consumes no arm - only a fill targets one by client order id. |
| `DuplicateNextFill` | The fill event only. `OrderTriggered` is never duplicated - it is not a fill, and a duplicated trigger has no consumer FSM transition to land on. |
| `DropNextAccountUpdate` | The account snapshot that follows the triggered fill, or the cancel a trigger's funds check produced, on the same rule as anywhere else. A trigger that only comes to rest still emits its snapshot, and consumes no arm. |
| `CommandLatency` submit act/ack | The submit only. There is no trigger-act or trigger-ack knob - the trigger is venue-internal with no consumer command behind it, and the sweep interval already bounds how late it can fire. |
| `DelayAcks` / `GoDark` / `StallData` | Transport, unchanged. `OrderTriggered` classifies as execution, so `DelayAcks` holds it and `GoDark` drops it; `StallData` never touches it. |
| `CancelOpenOrderSilently` | An untriggered conditional is a resting order, so it works today's way - the venue silently kills the protective leg and only a `QueryOrders` poll reveals it. A silent cancel racing a trigger in the same sweep pass leaves the order canceled: the cancel takes the lock first and removes the order, so the in-flight trigger fails its lookup and is dropped. |
| `VolStorm` / `LiquidityDrought` / `ReopenGap` | Not an arm at all - a boot regime baked into the run's rivers, so the sweep and the consumer see the same water. A drought thins what a stop has to trigger on rather than hiding prints from the consumer. |
