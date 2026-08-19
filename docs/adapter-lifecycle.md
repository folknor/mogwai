# Adapter lifecycle: the order a host must call the clients in

`mogwai-adapter` ships an `ExecutionClient`/`DataClient` pair a nautilus host
registers for the `MOGWAI` venue. Both are ordinary nautilus components, and a
host that drives them through the standard kernel path already satisfies
everything below. This page exists because ONE of these requirements is a
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

- The emitter derives `Clone` and owns its sender BY VALUE. The client clones
  its execution context into the websocket pump at connect time, so whatever
  sender state exists at that instant is FROZEN into the pump for the life of
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
THREAD-LOCAL on the runner's thread (`EXEC_EVENT_SENDER` in
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

## The data client has the same shape, without the guard

`MogwaiDataClient`'s sink is resolved the same way, from the same kind of
thread-local, and it is unguarded today - a data client connected without
starting goes quiet rather than refusing. The ordering requirement is therefore
the same for both clients even though only one of them enforces it. Start both,
then connect both.
