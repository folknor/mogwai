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

A planned run completion is not havoc: it emits `RunComplete` and closes
normally. That announcement is exempt from both suppression windows - a venue
that reached its declared duration says so even mid-blackout, because dropping
the frame would make a planned completion look like exactly the death `GoDark`
is imitating, which is the confusion `RunComplete` exists to end.

There is no deliverability refusal any more. It existed to reject divergences an
HTTP transport profile could not carry, and with the profiles gone every
divergence is deliverable over the one websocket carrier.
