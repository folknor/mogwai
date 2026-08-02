# mogwai havoc

Havoc is armed against the one run, not against an account or subscription.
Order-path divergences operate on the run ledger; data-path divergences operate
on the run's single tape and its connected sockets. Admission and execution
lanes remain connection-local memory bounds.

`DelayAcks`, `CommandLatency`, `GoDark`, `StallData`, partial fills, rejects,
duplicate fills, and blackouts keep their existing fidelity meanings. `GoDark`
suppresses a connection's output wholesale; `StallData` suppresses market data
only, so a server heartbeat still arrives and a stalled feed stays
distinguishable from a dead venue.

`FlowSurge { rate_mult, children_mult, duration_ms }` acts on the live tape at
parent boundaries. It divides parent gaps and increases sweep size for an
absolute simulated-time window. Historical checkpoint reads remain clean, so
seeking back across a live surge intentionally does not replay the havoc.
`LiquidityDrought` remains the inverse rate control: it stretches parent gaps
while leaving sweep shape unchanged.

A planned run completion is not havoc: it emits `RunComplete` and closes
normally. That announcement is exempt from both suppression windows - a venue
that reached its declared duration says so even mid-blackout, because dropping
the frame would make a planned completion look like exactly the death `GoDark`
is imitating, which is the confusion `RunComplete` exists to end.

There is no deliverability refusal any more. It existed to reject divergences an
HTTP transport profile could not carry, and with the profiles gone every
divergence is deliverable over the one websocket carrier.

## Havoc against a conditional order

A conditional (`StopMarket`/`StopLimit`) has three lifecycle points a plain
order does not - its submit, its trigger, and the order the trigger produces -
and every arm reaches it somewhere on that longer lifecycle. No arm is
carved out and no new arm exists for the trigger itself.

| Arm | Where it lands on a conditional |
|---|---|
| `RejectNextSubmit` | The submit. The conditional never exists, so nothing can trigger. |
| `PartialFillNext` | The fill the trigger produces, never the trigger itself. An untriggered stop consumes no arm - only a fill targets one by client order id. |
| `DuplicateNextFill` | The fill event only. `OrderTriggered` is never duplicated - it is not a fill, and a duplicated trigger has no client FSM transition to land on. |
| `DropNextAccountUpdate` | The account snapshot that follows the triggered fill, on the same rule as any other fill. A trigger that rests with no fill emits no snapshot and consumes no arm. |
| `CommandLatency` submit act/ack | The submit only. There is no trigger-act or trigger-ack knob - the trigger is venue-internal with no client command behind it, and the sweep interval already bounds how late it can fire. |
| `DelayAcks` / `GoDark` / `StallData` | Transport, unchanged. `OrderTriggered` classifies as execution, so `DelayAcks` holds it and `GoDark` drops it; `StallData` never touches it. |
| `CancelOpenOrderSilently` | An untriggered conditional is a resting order, so it works today's way - the venue silently kills the protective leg and only a `QueryOrders` poll reveals it. A silent cancel racing a trigger in the same sweep pass leaves the order canceled: the cancel takes the lock first and removes the order, so the in-flight trigger fails its lookup and is dropped. |
| `MarketRegime` / `VolStorm` / `LiquidityDrought` / `ReopenGap` | Per subscription, on the data feed, never on the trigger decision - the sweep always walks the clean tape. A drought silences a client's view while its stops still trigger off the canonical tape, the same property acceptance-time readings already have. |
