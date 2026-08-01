# Problem statement: the exec pump couples output latency to input liveness

Problem 1 is confirmed by measurement; problems 2 to 4 are established by
reading the code. The direction at the end is agreed but unbuilt. Nothing here
is a spec - a spec built from this file is written against
`reference/technical-implementation-spec.md`.

## Where this came from

`docs/todo.md` carried an item reading, in full: *"ProtocolError diagnostics
have no per-connection rate limit. Every refused or clamped subscribe emits one
diagnostic; ba's watchdog-driven resubscribe churn turned that into a
~1,700-line storm during a venue restart (QA finding, mechanism now removed by
the pinned `wall_anchor_ns`). Any future repeated-subscribe pathology will storm
the same way - a per-connection dedup or rate limit on identical diagnostic
reasons would cap it."*

That framing was examined on 2026-08-01 and rejected. Three steps got us here:

1. **Dedup is the wrong fix, and not merely inelegant.** A `ProtocolError` is
   the *response* to a degraded `Subscribe` - one truthful diagnostic per
   detected degradation or refusal, which is not the same as one per request: a
   single multi-symbol `Subscribe` can produce a request-wide `start_ts` or
   regime degradation AND a per-symbol unknown-symbol or capacity refusal. What
   makes the current traffic look one-per-request is problem 4, the adapter
   sending one symbol per frame on reconnect. Suppressing an identical repeat
   makes the wire answer depend on hidden per-connection history, so a consumer
   cannot predict it and a conformance test cannot assert it. It also collides with the honest-content invariant `reference/havoc.md`
   states in full - havoc may delay or drop *delivery*, never alter *content*,
   and the server never silently withholds truth. A dedup is the server
   injecting a drop divergence nobody armed, in the one component whose job is
   making injected faults legible.

2. **But the venue is not a bystander either.** The first reading - "1,700
   diagnostics for 1,700 degraded subscribes is correct, the client is at fault,
   close the item" - ignores WHEN those answers arrived. A watchdog resubscribes
   because it has heard nothing. If the diagnostic that would tell it "this
   subscribe is unservable" is itself queued behind a delay, the client keeps
   retrying, and each retry enqueues more. The venue's own answer latency
   becomes the input to the client's retry decision.

3. **Following that through the code found something bigger**, which is not
   about `ProtocolError` at all, and which the original item's framing would
   never have surfaced. That is problem 1 below.

## Why mogwai exists, and how much latitude a fix has

mogwai exists so broadarrow's *live* trading path can be exercised against a
venue that misbehaves the way real ones do. A backtest sandbox runs the matching
engine in-process, so it structurally cannot produce the failures that actually
break live systems: partial fills, rejects, delayed or duplicated acks, dropped
account updates, silently cancelled resting orders, venue blackouts. Those all
live in the seam between a strategy and an exchange it does not control, and
mogwai is a real process on the other side of a real socket that injects them
deliberately. Two commitments follow from that and explain most of the design.
First, the market data has to be plausible rather than synthetic-looking, hence
the fingerprint fitted offline to Kraken history - including its uncomfortable
properties, like the multi-hour arrival droughts measured in
`reference/architecture.md` under "Tape arrival droughts", because a venue that
is quiet in realistic ways is part of the point. Second, and more importantly,
the *content* the venue reports is always truthful: havoc may delay or drop
delivery, but the engine's answer about what actually happened is never a lie.
That is what makes the whole thing useful as an instrument - a consumer can
distinguish an injected fault from a real defect, because everything mogwai does
wrong, it does on purpose and admits to.

- Be explicit about which ideas are full coherent rewrites versus local changes.
- Pre-1.0: breaking internal API is acceptable.
- Aggressive internal rewrites are acceptable if the payoff is real.
- Correctness and maintainability matter; preserving old abstractions because
  they already exist does not.
- Do not preserve structure because it exists today.
- Do not preserve abstractions because they are shared.
- Do not optimize for minimal or least-invasive change by default.
- Do not assume public or internal API stability matters pre-1.0.
- Do not assume engineering time or resource constraints. Resources are
  unlimited.

## The structure

`mogwai-server`'s `ws.rs` gives each session an exec delay pump: a bounded
channel (1024) drained by `spawn_exec_pump`, which holds each execution frame
until `arrival + armed DelayAcks window` and then hands it to the writer.
`reference/havoc.md` documents the contract this implements, under
"Server-owned (temporal windows and the clear control)": `DelayAcks { ms }`
*"holds every outbound execution event by `ms` before sending it; market data is
untouched"*, with each frame's deadline anchored at its production instant so a
batch lands together rather than serialized a window apart.

Two kinds of producer feed that channel from the socket's READ LOOP, both with
an `.await`ed send:

- order-entry events, after `process_order_cmd`, and
- `ProtocolError` diagnostics, via `send_exec_protocol_error`, from the
  `Subscribe` arm (below-origin `start_ts`, beyond-sim-now `start_ts`,
  unfireable `ReopenGap`, unknown symbol, replay capacity exhausted) and from
  the undecodable-frame arm.

Replay threads also emit diagnostics (the dead-seek case), but they do so from
their own threads via `send_cancellable`, so they cannot block the reader. Only
the two above can.

`ProtocolError` reaches the pump because `ServerMessage::category` classifies it
as execution and `havoc.md` says `DelayAcks` holds every execution event - so
routing it through the pump is what makes the route match the classification.
That decision is deliberate and documented. It is also the coupling that makes
problems 1 and 2 interact.

## Problem 1: an armed `DelayAcks` can become an input stall

**The claim.** When the pump is full, both producers block. Because both sit in
the read loop, a full pump stops the server reading client frames on that socket
entirely. `havoc.md` documents `DelayAcks` as holding outbound execution events
by `ms`. It does not say the venue stops accepting your commands. An injected
output-latency fault silently becoming an input-refusal fault is a fidelity
break in the component that exists to make injected faults distinguishable from
real ones - a consumer diagnosing "the venue stopped responding" cannot tell it
from the blackout `GoDark` models.

**The fill condition, stated precisely.** An earlier reading of this called the
pump a throttle - roughly one event per `ms` - which is WRONG and worth
recording so it is not re-derived. `spawn_exec_pump` anchors each deadline at
the event's own arrival instant, and an event whose deadline already elapsed
while a predecessor slept forwards immediately. So in steady state the pump is a
fixed time SHIFT of the stream, not a rate limiter: drain rate equals arrival
rate. What it holds is every event that arrived within the last `ms` window.
The channel therefore fills when

> events arriving within any window of length `ms` exceed 1024

and not before. Two ways to reach that: sustained rate (rate x delay > 1024,
e.g. 25 s of ack delay at >41 events/sec) or a single burst larger than 1024
inside one window. `GoDark` and `StallData` do NOT contribute - the writer
drops those frames downstream of the pump, so they never back it up. This is
specifically a `DelayAcks` interaction.

**Status: MEASURED AND CONFIRMED, 2026-08-01.** The stall was reasoned from the
code first; it is now reproduced.

`delayed_acks_must_not_stall_the_socket_read_loop` in `mogwai-server`'s test
module arms a 30 s `DelayAcks`, submits 400 limit orders on one socket, then
sends a valid `Subscribe` and waits 3 s for a trade.

The layers are worth keeping distinct, because only the first is observed:

- **Observed.** With the delay armed, no witness trade arrives within 3 s. With
  no delay armed, the same harness produces one promptly.
- **Code-supported premise.** Each submit yields three execution events
  (OrderAccepted, OrderFilled, AccountState), so 400 submits is ~1200, past the
  ~1025 the pump holds. That comes from reading the engine, not from counting
  frames in this test - the test never reads them.
- **Causal inference.** The reader never reached the `Subscribe`. This follows
  from the observation plus the inspected channel topology - both producers
  `.await` into a bounded channel from the read loop - and not from direct
  instrumentation of the read loop, which the test does not have.

The witness is a TRADE, not an order event, and that choice is what makes the
result readable: market data is written to the writer channel directly by the
replay thread and never touches the exec pump, so a trade arriving would prove
the `Subscribe` behind it was read and processed while execution frames sat
held. An order ack proves nothing - it is delayed by design.

The innocent explanations are excluded by a control that ships alongside it and
is NOT ignored, so it stays green in `brokkr check`:
`saturation_witness_control_is_sound` runs the byte-identical saturation with no
delay armed, and the trade arrives in ~0.2 s.

What the control proves is precisely this: the identical workload and the
identical witness function without a delay armed. So the `Subscribe` is
well-formed, the 3 s window is ample, and the tape has data to send - the
documented arrival droughts are not hiding the witness. The only difference
between pass and fail is the armed delay. It does NOT independently prove the
submits were accepted; neither test reads their responses, so acceptance stays a
code-supported premise alongside the three-events-per-submit figure.

Run them with:

```
brokkr test -p mogwai-server saturation_witness_control_is_sound --debug
brokkr test -p mogwai-server delayed_acks_must_not_stall --debug
```

The gate was `#[ignore]`d deliberately while problem 1 stood: it FAILED, and
that failure was the reading, not a broken suite.

**Status, 2026-08-01: fixed.** Workstream A below landed - the pump is now a
two-lane, admission-controlled path that never awaits a full channel from the
read loop. `delayed_acks_must_not_stall_the_socket_read_loop` lost its
`#[ignore]` and is the standing regression test; see git history for the
landing sequence.

**Reachability prices urgency; it does not decide correctness.** An earlier
draft of this section made the pump repair conditional on first showing that a
real consumer clears ~1025 events inside its armed window. That was the wrong
test. mogwai documents no input-rate ceiling anywhere - not in `havoc.md`, not
in the wire types - so 400 submits inside a delay window is a load the protocol
accepts, and the venue breaking its own documented `DelayAcks` semantics under
an accepted load is a contract violation whether or not broadarrow currently
produces that load. Measuring their real order rate against their armed delay
(bounded by their ~25 s `INFLIGHT_TIMEOUT`) is still worth doing, because it
says how urgent this is. It does not get to say whether it is broken. The other
way to close it honestly would be to DOCUMENT an input-rate ceiling and enforce
it visibly - which is a protocol decision, not a silent behavior.

## Problem 2: `ProtocolError` volume scales with client retries

Every other pump filler scales with the client's ORDER FLOW. Diagnostics scale
with the client's REACTION TO LATENCY, which is what closes a loop: slow answers
generate retries, retries generate frames, frames deepen the queue, the queue
slows the answers. The adapter's per-symbol resubscribe fan-out (problem 4)
multiplies each round.

This is not a defect on its own - it is a defect only in the presence of
problem 1. If the pump cannot stall the reader, a retry storm is just the venue
correctly answering a badly-behaved client, and back-pressure against a client
hammering a venue is honest behavior a real exchange also exhibits.

**Status, 2026-08-01: dissolved as a side effect of workstream A.** With
`ProtocolError` reclassified onto the admission-controlled priority lane
(exempt from `DelayAcks`, delivered ahead of held traffic), a retrying client
now gets its "unservable" answer promptly instead of after the pump's delay, so
the answer-latency-feeds-retries loop this problem describes has no lever left
to close. The volume itself - one diagnostic per resubscribe fan-out - is
unchanged; that is problem 4, still open.

## Problem 3: `ProtocolError` is untargeted, so a late one is unattributable

`ServerMessage::ProtocolError { reason, ts_event }` carries no correlation
field; the symbol appears only as prose inside `reason`. `mogwai-protocol`
documents the untargetedness as deliberate, on the grounds that the offending
frame carries no `client_order_id` to echo, unlike `OrderRejected`.

That reasoning holds while diagnostics are prompt. It stops holding once they
can be late: the `Subscribe` handler quiesces and REPLACES a symbol's in-flight
replay on every resubscribe, so a delayed diagnostic describes a subscription
generation that no longer exists, and the client has no field with which to
discard it. `ts_event` is stamped at emission, before the pump delay, so it
dates the complaint without identifying its subject.

Independent of volume and of problem 1: this is true of a single late frame.

Whatever correlation is added has to identify BOTH the request generation and
the affected symbol. A batch-level request id alone is not enough: once one
`Subscribe` can carry several symbols, a client may resubscribe a single symbol
while the rest of an older batch stay current, so a diagnostic tagged only with
the batch it came from cannot be judged stale - it is stale for that one symbol
and live for the others.

## Problem 4: the adapter resubscribes one `Subscribe` per symbol

`subscribe_commands` in `mogwai-adapter`'s data client - the callback the WS
connection runs on every (re)connect - maps each subscribed symbol to its own
`WsCommand::Subscribe` rather than one command carrying the set. Every
reconnect round therefore costs N frames and, when the subscribes are degraded,
N diagnostics.

Independent of problems 1 and 2, but NOT independent of problem 3 - an earlier
draft of this file claimed it was, and that was wrong. The fan-out cannot simply
be batched away on the adapter side, because the wire shape
`Subscribe { symbols, start_ts, regime }` carries ONE `start_ts` for the whole
symbol set while each symbol's resume cursor advances independently. Batching
today would either clobber cursors or force grouping by `start_ts`, which is a
partial fix at best since cursors naturally diverge. Removing the fan-out
without losing resume precision requires a per-symbol `start_ts` on the wire -
the same breaking change to `Subscribe` that problem 3 needs for correlation.
They are one landing, not two.

## The converged direction

Not a spec. The shape the discussion converged on, recorded so the spec does not
re-derive it.

Provenance, stated precisely because it is the reason to trust this section:
TWO agents read this file cold, with no priming and no access to any earlier
list of options (the file deliberately carried none), and proposed plans
separately. They agreed on the shape and disagreed on two points. Those were
argued out between them and the author. That is two independent readings plus
the author's adjudication - the author is not an independent reader of their own
problem statement, so this is not three independent readings and an earlier
draft's claim of that was wrong.

No starting position survived intact, the author's included: the pump fix began
as "bound it" versus "unbound it" and ended as a two-lane design with admission
control that nobody proposed first. The two-lane structure and
`OrderAdmissionRejected` originated as one agent's completion of the
admission-control argument and were then explicitly accepted by the other, which
is what makes commitments 4 and 5 agreed rather than merely proposed.

### Workstream A: the exec pump - LANDED

**Status, 2026-08-01: landed.** Every commitment below is built: the pump is a
two-lane, byte/frame-budgeted, admission-controlled path (`AdmissionRejected`
replacing the `OrderAdmissionRejected` name floated here, generalized to a
`subject` so it can represent a refused cancel, modify, query, or subscribe,
not only a submit - see the full taxonomy note below), `ProtocolError` rides
the priority lane, and the overload close is `CLOSE_ADMISSION_OVERLOAD`. See
git history for the landing sequence and `reference/architecture.md` /
`reference/havoc.md` for how it behaves today. What follows is kept as the
design record - the commitments this landing was built to satisfy - not as
open work.

The confirmed defect, and independent of the subscription redesign - so it goes
first. It is NOT server-only: commitment 4 adds a variant to `ServerMessage` and
needs adapter translation, so this is a cross-crate pump, protocol and adapter
landing. An earlier draft called it server-internal with no wire break, which
was written before commitment 4 existed and is simply false with it.

A full coherent rewrite of the pump's buffering model, not a bigger channel. The
pump is a time SHIFT, not a rate limiter, so it must accept ALREADY-ADMITTED
events at arrival rate by construction; a receive-or-deadline loop keeps intake
running while the oldest frame waits out its deadline. Rate limiting happens at
admission, on commands, not on events the engine has already produced - that
distinction is what keeps commitment 2 from contradicting this one. Five
commitments:

1. **A hard bound.** Unbounded is not an option: `MAX_DIVERGENCE_MS` permits a
   one-hour `DelayAcks`, and rate x delay over an hour turns a connection stall
   into process-wide memory exhaustion. The existing 1024 cap is not the defect
   - blocking on it is. Note the bound's UNIT is open: an event count is not a
   memory bound, since `ServerMessage` sizes vary widely (snapshots and account
   states especially), so bytes, weighted permits, or a proven per-variant
   maximum all remain candidates.
2. **Reserve before producing or mutating.** Reserve worst-case output capacity
   for a command BEFORE the engine sees it; release the unused remainder once
   the real batch is known; if the reservation fails, do not mutate engine state
   at all. Checking after the engine has matched is too late - the events exist
   by then, and the only remaining choice is which fact to lose.
3. **Never shed produced execution truth.** The armed-divergence-queue eviction
   ack is NOT a precedent here, and the distinction is the load-bearing one: that
   queue holds INSTRUCTIONS not yet acted on, so naming what was displaced costs
   only a scenario's intent. The pump holds FACTS the engine already booked.
   Dropping a produced `OrderFilled` is an unarmed drop divergence against
   money-relevant truth, and "500 events discarded" is not something a consumer
   can reconcile from.
4. **Refuse through a bounded priority lane exempt from `DelayAcks`**, carrying
   a NEW wire variant, `OrderAdmissionRejected`, with the `client_order_id` the
   offending frame does carry. The adapter translates it to nautilus's ordinary
   rejected-order event. A distinct variant is REQUIRED rather than merely
   tidier: `ServerMessage::category` classifies the frame, so a special reason
   string on `OrderRejected` leaves it an execution event that `DelayAcks` is
   documented to hold, making any exemption a carve-out contradicting the
   contract. A separate variant is a genuine category difference - it reports
   what the TRANSPORT refused to admit, never was engine output, and so the knob
   that holds engine output legitimately does not reach it.
5. **Close the connection on sustained admission-path overload**, with an
   explicit overload close reason. If the priority lane fills too - peer not
   reading, or still hammering - this is the final hard bound. A close is a
   delivery failure, not content falsification, so it stays inside the
   honest-content invariant, and it is the least ambiguous signal available:
   the opposite of the `GoDark`-lookalike stall this document opens with.

Two clauses that are easy to miss and are not optional:

- The policy covers EVERY pump producer, not just `SubmitOrder`. Subscription
  diagnostics and query replies can recreate the stall on their own.
- A `Subscribe` must not quiesce and replace an existing replay until capacity
  for that replay's possible diagnostics is reserved.

What that second clause exposes: reserve-before-mutation and visible refusal
were agreed for ALL producers, but only the `SubmitOrder` refusal had a defined
shape at the time this was written. `OrderAdmissionRejected { client_order_id }`
could not faithfully represent a refused `CancelOrder`, `ModifyOrder`,
`QueryOrders`/`QueryFills`, or a subscribe whose diagnostic reservation failed -
settled at landing by the single `AdmissionRejected { subject, reason,
ts_event }` variant noted above.

`delayed_acks_must_not_stall_the_socket_read_loop` stops being `#[ignore]`d and
becomes the standing regression test. `saturation_witness_control_is_sound`
stays as it is.

### Workstream B: the subscription protocol (breaking)

Problems 3 and 4 in one landing, since per-symbol cursors and correlation are
the same wire change.

Note who actually sends this frame: `mogwai-adapter`'s data client constructs
and serializes `ClientMessage::Subscribe`. broadarrow constructs the adapter and
supplies its configuration - it does not build the wire message. So both ends of
this change live in this workspace. Whether a compatible deployment needs
coordination with broadarrow is a release and compatibility question, not a
consequence of the wire call site, and an earlier draft asserting that their
agreement is required was overstating it.

Shape, not types - field names and types below are ILLUSTRATIVE and settle
nothing. In particular the optionality of `start_ts` changes resume semantics
and is open:

```rust
Subscribe {
    subscriptions: Vec<SubscriptionRequest>,
    regime: Option<MarketRegime>,
}

SubscriptionRequest {
    generation_id: String,
    symbol: Symbol,
    start_ts: Option<u64>,
}
```

What is agreed is the SHAPE: a per-entry, client-supplied generation id plus a
per-entry cursor. The rationale is narrow and worth keeping narrow - per-entry
identity survives partial replacement of a prior batch, whereas
`(batch_id, symbol)` requires reconstructing that identity. A batch-level
request id may still exist for whole-frame diagnostics, but it must not be the
subscription identity.

Collisions are NOT harmless, and an earlier draft saying so was wrong: reusing
an id can make a stale diagnostic appear current and defeat the mechanism
outright. The client obligation is therefore positive and belongs in the
protocol docs - ids must be unique per `(connection, symbol)` across every
generation the client still cares about. The server never interprets the id
beyond echoing it, so the failure is confined to the client that caused it, but
confined is not harmless.

Every subscription diagnostic echoes `generation_id`, and the server stores it
alongside the replay so an asynchronously discovered failure (the dead seek)
still carries the right generation. Whether SUCCESSFUL subscriptions also gain
explicit result frames was not settled and is open. The echoed symbol is kept
for more than observability: it is checked against the generation's recorded
symbol, and a mismatch is a protocol inconsistency, not log decoration.

The adapter tracks the current generation per symbol and collapses
`subscribe_commands` into ONE frame carrying every symbol's own cursor. Beyond
"matching generation, apply" and "no longer subscribed, discard", the decision
table is NOT implied by an opaque id: telling an OLDER generation from an
UNKNOWN one requires either ordered ids or retained issuance history, and that
mechanism is open. Both reviewers flagged this independently.

### Rejected

- Per-connection diagnostic dedup, and rate-limiting identical reasons.
- Parsing symbols back out of diagnostic prose.
- Adapter-only batching, which loses resume precision without the wire change.
- A bigger fixed channel as the fix.
- An unbounded pump buffer.
- Shedding produced execution events, announced or not.

### Settled by workstream A's landing

The four items this list used to carry for workstream A - which lane subscribe
diagnostics ride, where the undecodable-frame diagnostic lives, the actual
bound and its unit, and the full refusal taxonomy - are all settled and built.
See git history for the landing sequence and `reference/architecture.md` /
`reference/havoc.md` for the resulting behavior; they are not restated here.

### Open for workstream B to settle

- **Whether successful subscriptions gain explicit result frames.** Adding
  success acknowledgments would be new design, not a consequence of anything
  agreed here. Workstream A's landing deliberately did not add them.
- **How an OLDER generation is distinguished from an UNKNOWN one** - ordered
  ids, retained issuance history, or neither, in which case the adapter's
  decision table shrinks accordingly.
- **Whether `regime` stays request-wide or moves per-entry**, and whether
  `start_ts` is optional.

### Documentation that must move with the code

Not optional: a venue that does something deliberate it has not admitted to is
the one failure this project defines itself against. Workstream A's share of
this (the `reference/havoc.md` admission-control writeup, the `ProtocolError`
doc-comment rewrite, `reference/architecture.md`'s pump structure) already
landed with it. What remains, for workstream B:

- `reference/architecture.md`: the `Subscribe` wire shape once it carries
  per-entry generation ids and cursors.
- `mogwai-protocol`: `ProtocolError`'s untargetedness paragraph inverts once
  per-entry correlation exists - narrowed by workstream A, not yet closed.

## References

- `reference/havoc.md` - the `DelayAcks` contract and the per-event deadline
  behavior (Server-owned temporal windows); the honest-content invariant that
  rules out silent suppression; the operator note on the unserved ack-delay band
  above broadarrow's `INFLIGHT_TIMEOUT`; the armed-queue eviction ack, which
  reads as a precedent for shedding-with-announcement and is argued above NOT to
  transfer to the pump.
- `reference/architecture.md` - the session/writer/pump structure under
  "mogwai-server", and `ProtocolError`'s place in the wire types under
  "mogwai-protocol".
- `reference/technical-implementation-spec.md` - the contract any spec built
  from this file is written against.
- `docs/todo.md` - the originating item, which this file supersedes.
