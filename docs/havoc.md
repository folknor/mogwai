# mogwai havoc

Transport and engine havoc is armed against the one run, not against an account
or a connection.
Order-path divergences operate on the run ledger; data-path divergences operate
on the selected river or on connected sockets. Admission and execution
lanes remain connection-local memory bounds. Order-path arms apply only to
client-originated orders, which reach the venue over the websocket carrier and
nowhere else. Venue-originated maintenance, including forced
liquidation, bypasses them and leaves them armed for the next matching client
action.

Two nouns run through what follows. A RIVER is one symbol's tape, materialized
the first time this run is asked for that symbol. A BOAT is the paced reader
sitting on a river, placed when the first websocket binds to that symbol at a
given speed; the connections sharing it are its PASSENGERS, and the boat carries
the clock every answer about that symbol is dated on. There is no venue-wide
notion of now, so a havoc window cannot be an interval on one clock.

An armed `CommandLatency` act delay is HEAD-OF-LINE on its socket. Each
connection feeds one sequential dispatcher, so a delayed submit holds every
later command from the same socket behind it - which is what stops a cancel
from overtaking the submit it cancels. Concurrent in-flight commands under an
armed latency need several sockets.

`DelayAcks`, `CommandLatency`, `GoDark`, `StallData`, partial fills, rejects,
duplicate fills, and blackouts keep their existing fidelity meanings. `GoDark`
suppresses a connection's output wholesale; `StallData` suppresses market data
only, so a server heartbeat still arrives and a stalled feed stays
distinguishable from a dead venue.

TRANSPORT HAVOC RESHAPES BARS RATHER THAN DROPPING THEM, and that is deliberate.
The venue ships no bars: every bar a nautilus host receives is FABRICATED by the
adapter by folding the trades it was delivered. Dropping or duplicating a trade
therefore changes a bar's open, high, low, close or volume rather than removing
or duplicating a whole bar frame, because the fold happens downstream of the
filter. That is what a real client-side aggregator on a lossy feed experiences,
so it is the honest simulation of THIS venue; modelling a dropped bar would be
modelling a venue that ships bars natively, which mogwai is not. It follows the
same principle as the rest of the surface: mogwai injects faults and declines to
repair them downstream. A strategy that needs to tell a quiet feed from a lossy
one reads `FeedLagged`, which carries the skipped count, rather than inferring it
from bar shape.

Generator havoc is river-scoped. The control payload accepts an optional
`symbol`. `FlowSurge { rate_mult, children_mult, duration_ms }` on a BOATLESS
river mutates its checkpointed water at parent boundaries and is visible to
history and to every passenger that boards later; its window opens at the
river's own origin, and the `202` body names that origin so you can see which
span was armed. A generator arm on a river that already has a boat is refused
with `400` naming that river: mid-run mutation of shared live water is not
supported, so arm the surge before any socket binds that symbol. An arm without
a symbol is refused too while any boat is seated, naming those rivers; with no
boat anywhere it falls to the run's boot symbol. `ClearDivergences` follows the
same rule from the other side - naming a seated river refuses, while an
unqualified clear lifts the transport windows run-wide and clears the surge on
every boatless river, skipping seated ones rather than refusing.

Transport controls remain runtime-armable and are ARMED PER ACCOUNT. `GoDark`,
`StallData`, `DelayAcks` and `CommandLatency` all take an optional `account` on
the request and corrupt only that account's view; naming none arms every
account, which is what a single-account venue wants and what an existing
scenario file already writes, so nothing on the wire breaks. They ride the
account rather than the venue because they change what one connection RECEIVES,
or when it hears about its own commands, rather than what the generator
produces - so on a shared exchange, blacking out or slowing one subagent leaves
the rest of the batch untouched. Clearing clears every account whatever the
request names, since a clear means stop everything.

The market REGIMES - `VolStorm`, `LiquidityDrought` and `ReopenGap` - are not
runtime arms. They are a boot choice made by whoever launches the run, apply to
the whole run's tape, and enter the tape identity, so a regime run is a
different tape rather than a mutation of one. `LiquidityDrought` is the inverse
rate control to a surge: it stretches parent gaps while leaving sweep shape
unchanged. `mogwai gen` takes the same regimes offline.

Every timed havoc window, including transport windows and `FeeSurcharge`, is
measured in simulated milliseconds on the receiving passenger's clock. The
window is stored as the WALL instant it was armed at plus a simulated span, so
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
order EXECUTING or LEAVING THE BOOK - a fill, a cancel that frees a resting
order's hold, a funds-check eviction during a sweep, a stop trigger that booked
any of these. It is deliberately NOT spent on an order JOINING the book, even
though the reservation that joining takes does move `locked`: acceptance
necessarily precedes the fill, so an arm consumed there could never reach the
event a scenario author aimed it at. That one carve-out is the whole of the
asymmetry - everywhere else the question is whether an order's state actually
transitioned, not whether the transition was a fill.

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
| `RejectNextCancel` | A cancel for a RESTING order, refusing it and leaving the order where it was. Not spent on an unknown or already-terminal id, which would be refused anyway and would look like the arm failing to fire. The point is what the client is left believing: publish a replacement before the cancel is acknowledged, have the cancel refused, and two orders rest where the script rests one. |
| `PartialFillNext` | The fill the trigger produces, never the trigger itself. An untriggered stop consumes no arm - only a fill targets one by client order id. |
| `DuplicateNextFill` | The fill event only. `OrderTriggered` is never duplicated - it is not a fill, and a duplicated trigger has no client FSM transition to land on. |
| `DropNextAccountUpdate` | The account snapshot that follows the triggered fill, or the cancel a trigger's funds check produced, on the same rule as anywhere else. A trigger that only comes to rest still emits its snapshot, and consumes no arm. |
| `CommandLatency` submit act/ack | The submit only. There is no trigger-act or trigger-ack knob - the trigger is venue-internal with no client command behind it, and the sweep interval already bounds how late it can fire. |
| `DelayAcks` / `GoDark` / `StallData` | Transport, unchanged. `OrderTriggered` classifies as execution, so `DelayAcks` holds it and `GoDark` drops it; `StallData` never touches it. |
| `CancelOpenOrderSilently` | An untriggered conditional is a resting order, so it works today's way - the venue silently kills the protective leg and only a `QueryOrders` poll reveals it. A silent cancel racing a trigger in the same sweep pass leaves the order canceled: the cancel takes the lock first and removes the order, so the in-flight trigger fails its lookup and is dropped. |
| `VolStorm` / `LiquidityDrought` / `ReopenGap` | Not an arm at all - a boot regime baked into the run's tape, so the sweep and the client see the same water. A drought thins what a stop has to trigger on rather than hiding prints from the client. |
