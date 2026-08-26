# Adapter lifecycle: the order a host must call the clients in

This page uses `client` only for nautilus's inherited adapter objects.

`mogwai-adapter` ships an `ExecutionClient`/`DataClient` pair a nautilus host
registers for the `MOGWAI` venue. Both are ordinary nautilus components, and a
host that drives them through the standard kernel path already satisfies
everything below. This page exists because exactly one of these requirements is a
refusal rather than a warning, and a host assembling its clients by hand can
trip it.

## `start()` before `connect()`, on the runner's thread

**The execution client refuses to connect until it has been started.**
`connect()` returns an error naming the missing sink:

    execution event sender not initialized: call start() on the runner's thread
    before connect(), or every order event this connection receives would be
    dropped silently

This is not a style preference, and it is not a guard that can be relaxed. The
sink is nautilus's own `ExecutionEventEmitter`, and two of its properties
combine badly:

- The emitter derives `Clone` and owns its sender by value. The client clones
  its execution context into the websocket pump at connect time, so whatever
  sender state exists at that instant is frozen into the pump for the life of
  the connection. A sender installed afterwards never reaches it.
- `send_order_event` on an emitter with no sender writes a `log::warn!` and
  returns. There is no error and no return value to check.

So a client connected without a sender reports success, holds a healthy socket,
and drops every `OrderAccepted`, `OrderFilled`, `OrderCanceled` and
`OrderRejected` the venue pushes, for the whole run, with nothing in the strategy
path observing it. A strategy would see its orders acknowledged as `Submitted`
and then never hear another word. Refusing the connection is the only way this
adapter can make that condition visible, so it refuses.

`start()` is where the sender is resolved, because nautilus keeps it in a
thread-local on the runner's thread (`EXEC_EVENT_SENDER` in
`nautilus_common::live::runner`) rather than in a shared cell. That is also why
the adapter cannot simply resolve it lazily inside `connect()`: an async fn may
already be polled on a different thread, where the slot is empty. `connect()`
retries the lookup once - free when it really is on the runner's thread - and
then refuses.

**What a host must do:** call `start()` on the execution client from the
runner's thread, before `connect()`. The nautilus kernel already does: its
startup sequence runs `start_engines` ahead of `connect_*_clients`, so no
shipped host ordering hits this refusal. A host that constructs and connects
clients itself must preserve that order.

**Reconnecting is unaffected.** `connect()` after a `stop()` is supported and
does not need another `start()`; the sender, once installed, stays installed on
the emitter. The refusal only fires when a sender was never resolved at all.

## What ends a connection for good, and what does not

Both clients reconnect on their own, with backoff, for the whole life of the
client. Three things stop that permanently, and each writes one log line:

- the venue's **run completing** - the `RunComplete` frame, or a WS 1000 close
  reasoned `run complete`,
- **this connection's configured duration elapsing** - the
  `PassengerDurationComplete` frame, or a WS 1000 close reasoned
  `passenger duration complete`,
- **eviction** - a WS 1000 close whose reason begins `evicted: `, meaning a
  newer connection presented this client's account id under a different or
  absent callsign. Redialling would evict the claimant in turn, forever, so the
  client stops.
Everything else is a transport event and is redialled, including any other WS
1000. That matters because 1000 is the ordinary code for any graceful close: a
proxy retiring an idle socket sends it, and so does a venue restarting. The
adapter classifies on the close reason (`mogwai_protocol::close::classify`), not
on the code, and a reason it does not recognize is never read as completion.

**The log line distinguishes all three.** The venue announces a finished run
and an elapsed passenger duration as different frames, so the adapter classifies
each correctly from the frame and the close behind it agrees rather than
refining it. Until those frames split, both completions sent one frame and an
ordinary duration end was logged, and reported, as a finished run. Eviction has
no frame ahead of it and was always exact.

One refusal reads as if it belonged on that list and does not: the venue's
second-cadence refusal, an HTTP 400 at the upgrade whose body ends `a ledger
carries one cadence`. It is conditional. Per `docs/accounts.md` the rule holds
while any of the account's passengers is riding that river and lifts once the
last leaves, and the passenger holding it need not be this client's - any
process naming the same account id may be the incumbent. So the client keeps
dialling, and it writes its own `warn` line rather than a generic dial failure,
because a silent backoff loop against a cadence conflict looks exactly like a
transport outage and is nothing like one. If the line repeats forever, the cause
is almost always this client's two legs configured with different `speed`
values; configure them alike.

## A command your host submitted is not a command the venue saw

The order path enqueues onto a channel a writer task drains, so `submit_order`,
`cancel_order`, `modify_order` and the venue queries return before their frame
is on the socket. If the socket dies in that window, the frame is never written.

**The adapter does not replay those commands onto the next connection.**
Re-submitting orders across a reconnect is a policy call your host owns, not one
a venue adapter should make silently on your behalf. What the adapter guarantees
instead is that you are told: every accepted-but-unwritten order command is
reported through the same synthesized rejection as any other transport failure -
an `OrderRejected` for a submit (every leg, for a group), an
`OrderModifyRejected` for a modify, an `OrderCancelRejected` for a cancel - with
a reason naming the cause. An undelivered venue query fails its caller
immediately rather than waiting out its request timeout, and the same holds for
a history request in flight when the data client stops or reconnects.

The report is made whether the connection ends by dropping, by the run
finishing, or by your host calling `stop()` or `connect()` again. Those last two
abort the socket task outright, so the reporting is owned by that task's own
drop rather than by code after its loop; nothing queued is lost to a
cancellation.

So an order never sits in `Submitted` forever because its socket dropped. If
your strategy wants the order retried, it retries it on the rejection.

**The report errs toward saying less arrived than did, except where the adapter
can see otherwise.** A socket dying in the instant between the frame reaching
the wire and the write call returning leaves the adapter unable to tell that it
landed, so it reports the command undelivered though the venue may have seen
it. That direction is deliberate: a spurious rejection is recoverable by a host
that retries, while a silently swallowed order is not.

The one thing the adapter will not do is let that guess overrule evidence
against it. If the order's own mirror row has already seen the venue accept,
trigger, partially fill or begin amending it, the submit demonstrably did
arrive, and the synthesized rejection is dropped with a warning instead of
closing an order the venue still owns. A rejection you do receive for a submit
therefore means the adapter saw no venue acknowledgement for that order at all.
Treat it as "probably not accepted" rather than "certainly not accepted", and
let reconciliation settle the rest - the venue-truth reports on reconnect are
what resolve the remainder.

## The data client has the same shape, without the guard

`MogwaiDataClient`'s sink is resolved the same way, from the same kind of
thread-local, and it is unguarded today - a data client connected without
starting goes quiet rather than refusing. The ordering requirement is therefore
the same for both clients even though only one of them enforces it. Start both,
then connect both.
