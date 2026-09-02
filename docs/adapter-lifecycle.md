# Adapter lifecycle: the order a host must call the clients in

This page uses `client` only for nautilus's inherited adapter objects.

`mogwai-adapter` ships an `ExecutionClient`/`DataClient` pair a nautilus host
registers for the `MOGWAI` venue. Both are ordinary nautilus components, and a
host that drives them through the standard kernel path already satisfies
everything below. This page exists because exactly one of these requirements is a
refusal rather than a warning, and a host assembling its clients by hand can
trip it.

## Getting the execution event sink

**A host using a nautilus node has nothing to arrange.** The node binds its
event senders and then calls every registered client factory on the same
thread, so `MogwaiExecutionClientFactory` resolves the sink at creation and
installs it before it hands the client back. `start()` and `connect()` can then
run whenever the host runs them.

That is the only site where the sink is resolved from ambient state, and
deliberately so. `MogwaiExecutionClient::new` does **not** read the runner's
thread-local, even though it could. The last runner to bind a thread owns the
slot, so a constructor reading it would capture whichever runner happened to be
bound at that moment; a client that later ran under a second runner would pass
every "is a sender installed" check and deliver its whole event stream to the
first. Absence is detectable and this document tells you how to detect it -
misrouting is not. The factory is the one place where the owning runner is
known, so it is the one place that guesses nothing.

A host that builds the client itself, rather than through a node, has to supply
the sink some other way: call `start()` on the runner's thread. The refusal
below is what tells you if that never happened.

### The refusal, for a client built outside a node

**The execution client refuses to connect until it has a sink.**
`connect()` returns an error naming the missing sink:

    execution event sender not initialized: call start() on the runner's thread
    before connect(), or every order event this connection receives would be
    dropped silently

This is not a style preference, and it is not a guard that can be relaxed. The
sink is nautilus's own `ExecutionEventEmitter`, and two facts combine badly:

- The sender is resolved from a thread-local set on the runner's thread
  (`EXEC_EVENT_SENDER` in `nautilus_common::live::runner`). A client whose
  `start()` ran on any other thread cannot obtain one at all - there is no
  process-wide slot to fall back to.
- `send_order_event` on an emitter with no sender writes a `log::warn!` and
  returns. There is no error and no return value to check.

So a client connected without a sender reports success, holds a healthy socket,
and drops every `OrderAccepted`, `OrderFilled`, `OrderCanceled` and
`OrderRejected` the venue pushes, for the whole run, with nothing in the strategy
path observing it. Refusing the connection is the only way this adapter can make
that condition visible, so it refuses.

That is also why the adapter cannot simply resolve the sender lazily inside
`connect()`: an async fn may already be polled on a different thread, where the
slot is empty. `connect()` retries the lookup once - free when it really is on
the runner's thread - and then refuses.

One thing this guard used to also cover no longer applies, and a host reading an
older version of this page should know it. Until nautilus 0.63 the emitter owned
its sender by value on a `Clone` type, so an execution context cloned into the
websocket pump froze whatever sender state existed at clone time and a later
installation never reached it. Nautilus 0.63 moved the sender into a shared cell
that every clone observes, so that narrower ordering hazard is gone. The refusal
above is unaffected: it fires when a sender is never obtainable at all, which the
shared cell does not address.

**What a host must do:** nothing, if it registers the factory with a node. If it
constructs the client itself, call `start()` on the execution client from the
runner's thread before `connect()`.

**This is not a licence to drive the client from another thread.** Nautilus
declares `ExecutionClient` as `#[async_trait(?Send)]` and its adapter guide says
clients do not move across threads. Seeding the sink at creation fixes where
*events* go; it says nothing about where the client's own lifecycle methods may
be called. Create it, start it, connect it and stop it on one thread.

**Reconnecting is unaffected.** `connect()` after a `stop()` is supported and
does not need another `start()`; the sender, once installed, stays installed on
the emitter. The refusal only fires when a sender was never resolved at all.

## What ends a connection for good, and what does not

Both clients reconnect on their own, with backoff, for the whole life of the
client. Four things stop that permanently, and each writes one log line:

- the venue's **run completing** - the `RunComplete` frame, or a WS 1000 close
  reasoned `run complete`,
- **this connection's configured duration elapsing** - the
  `PassengerDurationComplete` frame, or a WS 1000 close reasoned
  `passenger duration complete`,
- **eviction** - a WS 1000 close whose reason begins `evicted: `, meaning a
  newer connection presented this client's account id under a different or
  absent callsign. Redialling would evict the claimant in turn, forever, so the
  client stops.
- **the consumer going away** - the execution client only, and the one entry
  here the venue does not announce. It keeps a clone of the sender it emits
  through, and when that channel's receiver is dropped - the nautilus runner
  gone or shutting down - the events this socket translates reach nobody. It
  retires the connection rather than redialling into a dead sink.

  This one is recoverable in a way the other three are not: it retires the
  transport generation, not the client. A host that installs a live sender by
  calling `start()` on the runner's thread again and then `connect()` gets a
  working client back. What it must not expect is self-healing, because the
  adapter never redials on its own after this.

  It is observed at emission boundaries, not continuously, so a receiver that
  closes while the socket is quiet is noticed at the next event rather than
  immediately. Events translated before the loss was observed are gone: the
  venue is not an authority that can be asked for them again, which is why the
  connection ends rather than continuing on a best-effort basis.

Everything else is a transport event and is redialled, including any other WS
1000. That matters because 1000 is the ordinary code for any graceful close: a
proxy retiring an idle socket sends it, and so does a venue restarting. The
adapter classifies on the close reason (`mogwai_protocol::close::classify`), not
on the code, and a reason it does not recognize is never read as completion.

**The log line distinguishes all of them.** The venue announces a finished run
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

## An order the venue would refuse is refused here first

Every order command the execution client builds is run through the venue's own
protocol-boundary verdict - `mogwai_protocol::validate_submit_order` at
`SubmitPhase::PreStamp`, the same function the venue's decode boundary calls -
before the frame leaves the process. A frame that fails it never reaches the
socket, and `submit_order` (or `submit_order_list`, for the whole list) returns
an error naming the venue's own reason, which nautilus turns into an
`OrderDenied` rather than an `OrderRejected`.

This is a difference a host can observe, and it is the one worth stating: the
verdict is the venue's either way, but it arrives without a round trip and
without depending on a live socket. Nothing legal is refused by it, because the
adapter and the venue are running one rule table rather than two.

What mostly reaches it in practice is an `OrderInitialized` assembled by hand.
A `MarketOrder` built through nautilus's own constructors cannot carry a price,
but `SubmitOrder::new` takes any `OrderInitialized`, every field of which is
public, so `order_type = Market` beside a price is reachable through nautilus's
public API. Such an event is a host defect; the adapter neither drops the price
nor forwards it, it names it.

One factory-built shape reaches it too, and it is not a host defect but a
model mismatch: a `MarketToLimit` built through nautilus's own constructor
carries no price - nautilus determines the limit at fill - while this venue's
wire requires the consumer to state the limit the remainder rests at. The two
models disagree about who names that number, so a factory-built market-to-limit
is denied with `MarketToLimit order must carry a price`. Stating one means
setting `price` on the `OrderInitialized` yourself before `SubmitOrder::new`,
which for this one type is the contract rather than the defect the previous
paragraph describes. The venue refused the same frame at its boundary before
this local check existed; the denial only moved closer.

Configuration is refused the same way and at the same distance from the venue:
`validate()` on either client config rejects `dial_timeout_secs = 0`, which
would otherwise expire every dial before a socket could open and report a local
config error as an unreachable venue.

## The data client has the same shape, without the guard

The data client implements nautilus funding-rate subscriptions for perpetuals.
Each epoch-aligned venue funding frame is forwarded as `Data::FundingRate`, and
the most recent rate is replayed to a subscriber joining between instants. The
instrument metadata also preserves the interval, interest, index symbol and
clamp under `mogwai_` keys. These frames are market prices, not cash receipts:
the ledger charges a multi-instant sweep at one pass-end rate, so a balance
cannot be reconciled from the published per-instant rates.

`MogwaiDataClient`'s sink is resolved the same way, from the same kind of
thread-local, and it is unguarded today - a data client connected without
starting goes quiet rather than refusing. The ordering requirement is therefore
the same for both clients even though only one of them enforces it. Start both,
then connect both.
