# mogwai architecture

How the venue is built and why. Four subjects in order: the venue's runtime
shape and its account model; accounts, risk, instruments and valuation; the
boatyard, clocks, delivery and history; and the generator, the tape identity and
the fingerprint. The workspace map is last.

The headings were added on 2026-08-26 to a document that had carried two in
1,400 lines. They are section markers over prose that was written to be read
straight through, so a section is an entry point rather than a self-contained
unit: several state a rule and then spend paragraphs on the case that made the
rule necessary, and that second half is usually the part worth reading. Nothing
was moved, cut or reworded in that pass.

Mogwai is a fake venue. A direct launcher starts one foreground process and
receives a versioned readiness record as one JSON line on the child's stdout.
The process binds one endpoint and owns an open set of resolved instruments,
generated rivers, and one ledger per account.

## Two topologies

Two topologies are supported, and both are required. The default is the per-run
venue: a consumer given no address spawns its own ephemeral venue, owns it for
the run, and takes it down with the worker, so N subagents means N venue
processes, each with one river, one passenger and one ledger. The second is the
shared exchange, and it exists for amortization: one `mogwai serve`, N
subagents connecting under their own accounts, one river per distinct tape
identity rather than N generations of near-identical tape. That is cost
motivating a second mode, not cost shaping the model, so it does not conflict
with the standing premise that resource cost shapes no decision here. The
venue's semantics are identical either way; what differs is how many processes
carry them.

## Rivers, accounts and passengers

A river is a generated sequence and is shared; an account owns its ledger,
risk state and freeze stamp; a passenger is one connected trader riding one
boat and dying with its socket. The
engine is per account: one engine per process meant every consumer's
fills moved every other consumer's net, which is right for a venue owned by one
run and wrong for an exchange serving a batch. An `Account` is created on
demand, keyed by account id, and the id is the consumer's: it outlives every
passenger, so a socket presenting it again resumes that ledger rather than
opening a fresh one. The venue cannot distinguish a reconnect from a stranger
claiming the id and does not try, so an account id is effectively a bearer
token.

## Delivery is attributed, not broadcast

Delivery is attributed, not broadcast, which is what makes the per-account
ledger worth having on the wire as well as in memory. A sweep executes one engine
pass per account, and each frame it produces reaches only the passengers of the
account it is
about: an order-scoped frame goes to the account that submitted the order, and an
`AccountState` goes to the account it names. What reaches every connection is
what is genuinely about the venue - a fault, a run completion, a feed gap.

The account snapshot was the last frame to get this, and until it did, an
N-account venue sent every socket all N snapshots on every pass. A consumer that
believes them sizes against a stranger's equity, and a consumer has no reason to
suspect it: the snapshot carries its own account id, but a consumer told the venue
serves one ledger per run has no reason to read it. If a consumer's own adapter
skips that comparison on the strength of the one-ledger-per-run premise, the
shared-venue topology breaks the premise, so the venue is what has to be right.

## Callsigns and eviction

An account is read under one callsign. A second socket presenting an existing
account id under a different or absent callsign evicts the incumbent, because a
ledger read and written by two unrelated parties is one ledger with two notions
of its own state. Sockets presenting the same callsign coexist instead, which is
what lets one consumer hold a data leg and an exec leg - and, through them, as
many rivers as those sockets bound - on one account. The evicted socket is
closed with `1000`, normally rather than as a fault: the venue cannot tell a
returning consumer from a stranger claiming the id, and treating an eviction as a
failure would make a consumer's reconnect ladder evict whatever evicted it. By
default the newcomer resumes that account - positions, order history and risk
state intact - which is what makes a killed worker able to come back to its own
book. `reset_account_on_reconnect` hands it a clean ledger instead, and the
readiness record reports which way the venue is set so a launcher never has to
infer it.

## Close codes carry no meaning; close reasons do

The close code does not carry the meaning; the reason does. `1000` is the
ordinary code for any graceful close, and this venue sends it for three
different things - a completed run, a passenger whose configured duration
elapsed, and the eviction above - while a proxy or a load balancer sends it for
reasons of its own. A consumer that read the code alone would have to treat all
four alike, and the adapter did: it read every `1000` as run completion and
permanently disabled its reconnect. The reason strings are therefore a protocol
contract rather than log text, and they live in `mogwai_protocol::close`, which
the venue writes and `close::classify` reads. A reason that module does not
recognize is never terminal, which is the safe default in both directions: a
needless redial is recoverable, and a run silently declared over is not.

A close reason is therefore bounded by the frame rather than by the ordinary
reason cap. RFC 6455 allows a control frame 125 payload bytes and the status
code takes two of them, so `close::MAX_REASON_BYTES` is 123 - a quarter of the
512 that bounds a reason inside a text frame. The eviction sentence interpolates
an account id and reaches 135 bytes at `MAX_ACCOUNT_ID_LEN`, which a conforming
peer fails the connection over: the close carrying the discriminator would never
arrive, and a consumer that sees a bare EOF instead classifies nothing and redials
into the loop. Every `CloseSpec` constructor trims to that budget, after
composing its prefix, so the terminal survives and only the detail is spent.

## What order an upgrade does its work in

Eviction is the last thing an upgrade does. Every refusal `/ws` can make - an
unresolvable shape, an account unfunded in the settlement currency, a
non-finite speed, a boat that could not be placed, a second cadence on one
ledger - is decided before the claim, because claiming closes the incumbent's
sockets and, under `reset_account_on_reconnect`, discards its ledger. A refused
upgrade must cost the incumbent nothing; the alternative made a single
unauthenticated request a way to disconnect a live consumer and wipe its book
without ever connecting.

Under that knob the ledger the checks were taken against can also be replaced by
somebody else, so each account carries a ledger incarnation and an upgrade
samples it before it reads that ledger. The reservation refuses with a
retryable `409` if the identity has moved since, and the replacement itself is
performed while the reservation is outstanding, immediately before the commit
that advances the identity. Those two facts are what make the incarnation a
boundary rather than a decoration: every admission's checks lie wholly before
the exclusive window or wholly after it, so a funding or calendar answer taken
against a ledger that no longer exists can never be carried into a commit.
Sampling the identity inside the reservation instead, or replacing the ledger
after the commit, restores a window in which the check agrees with its own
mutation.

That whole tail - the ledger replacement, the commit, the displaced closes and
the instrument registration - runs in a task of its own, spawned and then
awaited before the upgrade response is built. Two properties have to hold at
once and each one alone is a defect. The work is owned by the task doing it,
because hyper drops the request future when a client goes away and a tail
cancelled midway has already evicted somebody: a spawned task instead runs to
completion, and the passenger it yields is dropped by the runtime when nobody
takes it, releasing the attach, the ticket and the liveness guard in order. And
the task is awaited before the 101, because the handshake is the consumer's
only proof that its admission committed. The supported two-socket
shared-callsign topology rests on that: a client opening its execution leg the
instant its data leg upgrades must not find its own account still reserved and
be answered a conflict. Returning the 101 with the tail still pending satisfies
the first property and breaks the second.

## Freeze, resume, retirement and the TTL

An unattended account freezes. The moment its last connection goes away it is
not swept, not marked, not funded and not judged against its policy, and a
socket returning with the same id resumes it. This is a deliberate departure
from a real venue, where being away is no defence against liquidation: mogwai
exists to exercise a consumer's live path rather than to simulate an account
nobody is trading. The consequence to state in any claim is that a run spanning
a disconnect has a gap in its risk history.

"Its last passenger" is counted from the attach, not from the outbound lane a
socket binds after its upgrade completes. The lane table alone cannot answer
whether anybody is reading an account: an eviction retires the incumbent's lane
immediately, while the newcomer binds its own only once its handler runs - and
never at all if it abandons the upgrade. A socket is therefore counted onto its
account before the 101 and off it when its lane is released - or when its
passenger is dropped, if it never bound one, which is what covers the abandoned
upgrade. The two are deliberately not the same instant: the writer's close
frame outlives the lane, and holding the account counted-in for that grace would
keep it in the sweep after nothing is reading it. The freeze fires when neither
a lane nor an attach is left. Without that count the evicted incumbent's
teardown found no lane, resolved no account and simply returned, leaving the
ledger attached with zero passengers: never TTL-collected, and still swept
while riding no boat, which cancelled the very resting orders the freeze
exists to preserve.

Resuming re-bases the book, because a returning boat is not the one that left. A
cursor is placed at its river's origin, so a frozen order's scan frontier - the
instant the departed boat had reached - sits in the new boat's future, and an
order left carrying it would wait for the new cursor to cover ground the old one
had already covered. Every surviving order therefore resumes scanning from the
returning boat's own clock. Nothing is owed for the span in between: nobody was
reading the account, which is the same statement the freeze makes.

Retirement does not apply to an eviction, and the boundary is the freeze rather
than the reconnection. Retirement runs only for a returning account - one the
venue found frozen - and a newcomer that claims an existing account is counted onto
it before the incumbent is closed, so the account never freezes in that window
and the newcomer resumes a live ledger. That is deliberate: retiring off it
would cancel resting orders and close positions the incumbent connection had every
reason to expect to survive its own reconnect. The alternative was worse than a
rule: before the count existed the incumbent's teardown could win the race and
freeze the account first, so whether the newcomer's book was retired depended on
which task got there first.

Re-basing is not bounded by the freeze, and this document previously said it
was, on the reasoning that a book taken over by eviction carries "the frontiers
of a river something is still reading". That is false when the claimer boards a
different river. The newcomer takes a different `BoatKey`, the incumbent's
ticket drops, and the departed river's boat is torn down with its worker; a boat
placed over that river again starts at the yard's origin, under the same key,
because `BoatKey` carries no placement nonce. The surviving orders' frontiers
then name a cursor that no longer exists, and every scan window they are judged
on is empty until the new cursor has covered the whole of the first session
again - silently, and indistinguishable from an order the tape has not reached.

So the rule is stated on the frontier itself rather than on the freeze. A
frontier may trail the cursor serving it and may never lead it: it is set either
at an order's acceptance or by how far a sweep has walked, both sampled on the
cursor in front of it. `resume` therefore re-bases exactly the leading frontiers
on every bind, and leaves trailing ones alone - a trailing frontier names water
this account is genuinely owed a scan over. Asking the state directly needs no
new identity and closes the case whatever produced it; a placement nonce on the
boat would instead repair the freeze proxy, and was not taken for that reason.

The remaining identity-shaped use is teardown, and it is closed by ownership
ordering rather than by widening `BoatKey`. A passenger releases its registry
ride before its ticket can remove the last boat. Until that release it still
owns the ticket, so another placement under the same key cannot exist; after
the release there is no stale seat to match. `Passenger::drop` states and
enforces that order for an upgrade abandoned before its socket handler runs.
Reversing the order, or adding a path that removes a boat without first retiring
every registry ride holding its ticket, invalidates the argument and would make
a placement nonce necessary.

What the account held off the joined river is retired at that moment - resting
orders cancelled, positions closed at their last mark. A returning socket may
name a different symbol than the account was trading, and carrying that forward
would leave it holding something the new connection can neither see nor close.

An order on a river nobody reads is cancelled rather than left, and this is the
other half of the same rule. An attached account's order on a symbol no cursor
is walking cannot fill, cannot expire, and cannot be told apart from one the
tape has not reached; the consumer is attached, so it is told. A frozen account is
exempt because it is skipped wholesale - its book is being kept for the socket
that comes back for it. Between the two, no resting order can sit indefinitely
on water nothing is reading.

A TTL bounds the freeze. `account_ttl_ms` collects an account nobody reclaims,
in wall time because a frozen account has no simulated clock - the boat that
carried one wound down with the last socket. Zero, the default, keeps accounts
for the life of the process, which is what a consumer restarting a worker needs.
The setting is on the readiness record, so a consumer whose restart takes longer
than the TTL can assert on the fact rather than discover it as a clean ledger.
Collection races the very reconnect it exists to give up on, so the removal
re-derives "unattended, and no admission pending" under the registry lock
rather than acting on the sweep's earlier read: an account reclaimed between
its expiry and its collection is spared, and an admission that sampled the
ledger's identity before a collection is refused at its reservation - ledger
incarnations are minted from one registry-wide counter, so a collected and
recreated account can never wear an identity an in-flight admission already
observed.

A connection that names no account is served under the venue's default account.
That exists for the ephemeral single-consumer venue, where making the one consumer
name an id would be ceremony; it is not a venue-wide account every connection
shares.

## The account id on a snapshot is a label

The account id on a snapshot is a label, and a consumer keeps its own. A venue
may hold several ledgers, and `/ws?account=` names one, but one connection sees
exactly one of them. The account a connection carries is the only account on that socket, so nothing
can be misrouted onto it, and the id the venue writes on an `AccountState`
therefore identifies nothing a consumer has to resolve. The
adapter reads it exactly once, at connect, where `note_account_label` logs the
mismatch if the venue's name for the ledger differs from the configured one -
and then stamps the configured id onto every snapshot it publishes
(`handle_account_state`). Both halves of this used to be an equality check, and
both were per-account-slot invariants that outlived the slots: the connect-time
one killed the consumer outright, and one release of the venue reported a bare
`MOGWAI`, which is a legal `mogwai_protocol::AccountId` and an unconstructable
nautilus one, so no configured value could satisfy it and every run died on
connect. The push-path one silently dropped a differently-labelled snapshot, so
a consumer took every fill while its balances quietly stopped moving.

This is written here because the design is counter-intuitive from the outside
and has been re-derived backwards twice under review: the natural-looking
assertion - that a published snapshot carries the id that arrived on the wire -
is the exact inverse of the contract, and pinning it would restore the drop.
The assertion that belongs on this path is the configured id, and it bites;
`adapter_smoke::an_account_labelled_differently_is_still_served` is where it
lives. The scope is the connection, and that is where the argument would break:
if a socket ever carried several ledgers, the id becomes a key and this whole
paragraph is what has to change first.

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
reduce-only IOC market orders at the mark, judged against the configured
liquidation band - and then locks. A terminating breach on a venue serving one
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
  Funding is paid on notional at the mark, on instants that sit on multiples of
  the interval from the unix epoch - a property of the clock, so the schedule
  cannot depend on when a run booted or how the sweep passes were cut. The
  configured `funding_rate` is the zero-premium interest. When the class names
  an `index_symbol` and that symbol already has a last mark, the live rate is
  `clamp(interest + (mark - index) / index, +/- funding_clamp)`. No index mark
  means a zero premium: reading an index never spends a river nobody asked for,
  so a perp-only venue keeps the configured rate. An instant is still honoured
  on the sweep pass that crosses it rather than at the instant itself.
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
unrealized on futures settling in it. An order whose shape would leave a holding
nothing prices is refused at entry by name, and an account that reaches an
unvaluable state some other way is warned about and left unenforced rather than
judged against a wrong number.

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
resting order is one of three explicit states: a live limit, an
untriggered conditional, or an inert market remainder left by a partial fill
that is never scanned again and ends only on cancel. Every resting limit
carries a trigger price drawn once at submit from a seeded, volatility-scaled
band around its stated price (`fill_band_vol_mult = 0.0` degenerates to a
strict through-at-the-stated-price fill), and it fills only when the run's
fill sweep walks a print strictly through that trigger; the fill is delivered
to each connection's lanes from the run, not from a command response. A
market order slips the same way, adverse to its side, off the same seeded
band. The trailing-volatility reading is cached once per symbol and fill-sweep
interval, using the interval's simulated-time floor, so a command burst shares
one coherent coarse band instead of repeating the full 300-second synthesis.
Sweep marks and settlement prices are separate exact-instant last-print reads;
the coarse band cache never supplies unrealized P&L.
A conditional (`StopMarket`/`StopLimit`) rests untriggered until the
same sweep walks a print that touches its stop price - the mirror-image
predicate, since a stop holds no queue position and every real venue fires one
on touch rather than through. On trigger the venue emits `OrderTriggered` and,
in the same batch, either fills a stop-market at the triggering print slipped
by the same band, or promotes a stop-limit to a live limit judged against that
same print (filling at once if already marketable, resting with a fresh band
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

## The boatyard: rivers, boats, tickets and placement

Live pacing is owned by the boatyard. A river is deterministic checkpointed
water keyed by symbol and its resolved bundle digest. A boat is one positioned
cursor, one OS pacing thread, one clock and one broadcast ring on that river.
A boat's per-river state is:

- `sim`, its affine simulated clock;
- `tape`, its paced broadcast source;
- `published_ns`, the history frontier visible to passengers;
- `last_swept_ns`, its settlement watermark; and
- `market_readings`, its acceptance-time reading memo.

The memo belongs here because its bucket is a function of the boat's clock and
the walk it saves is a walk of this river only.
A passenger owns an uncloneable ticket for one websocket connection. An
unnamed passenger places or joins a boat at the run's fixed origin. A named
window places a private boat at its stated start, except that the data and
execution legs presenting one account and callsign share that placement. Two
accounts dealt identical bounds therefore read separate cursors over
byte-identical water from the same start. Speed is quantized to micro-multiples
in the sharing key. Duration is passenger-local and is therefore not in that
key. An unserved speed is a second cursor on the same water, not a refusal:
speed mutates no generated value. One ledger still carries one clock per river,
and a clock is its rate and its epoch together. Two sockets on the default
account may ride two rivers, but on a river that account is already riding, a
second speed is refused as a cadence conflict and a second placement - a
different named window, or the run's shared origin against a named one - is
refused as a placement conflict, because either would be two clocks judging one
book. Admission decides both from the seat, before any boat is placed, so the
check compares exactly what the boatyard keys on minus the owner that decides
hull sharing. The account counts its passengers per boat, and the
count falls when a passenger ends rather than when the account freezes: an
account riding two rivers never freezes on losing one socket, and a boat key
carries no connection identity, so a ride left behind would be
indistinguishable from a live one as soon as anybody boarded that cadence again. Dropping the last
ticket of a given cadence winds that boat down: it
cancels its worker and joins it away from the registry mutex. Other cadences
on the same river stay. Rivers and their bounded checkpoint sets remain for
process life so later history does not depend on eviction timing.

## Placement on demand, and who pays for a cold river

There is no exception to placement on demand. Every river is boatless until a
passenger boards it, and no river is materialized before readiness. `serve` used
to warm the default label's whole span before writing its readiness line and then
retain a ticket on it for process life, which made one river permanently warm and
permanently boated while every other was cold - a privilege rather than a rule.

Removing it makes the venue uniform in a way worth stating, because it looks like
a cost and is mostly the disappearance of an exception. Placement reaches its
river inline, so every river has always been synthesized inside the request that
first named it, and that synthesis has always been paid out of the declared run
duration. Only the default label escaped, and only because it was reached before
the run clock was built. Now nothing escapes: a run that materializes a cold
river spends part of itself doing so, whichever label that river carries.

The first requester pays, and that requester can be a history poll as easily as a
boarding - so a history request reaches its river to the run start before serving
a window, rather than generating only as far as its own `end` and leaving the
river owing most of its warmup to whoever came next.

Concurrent first boarders share one placement through a semaphore handoff
rather than each placing a boat. `/health` reads the boatyard on non-blocking
terms, because it must never block on a placement.

`/health`'s tape fault reads every boated river on those same non-blocking
terms, not one river alone. It read only the default river until 2026-08-16,
which was right when a run had one paced tape and became a hole under the open
instrument set: a consumer bound to any other river got a healthy answer while
its own tape was stuck, and the default river is the one a strategy under test is
least likely to have bound. One optional object over N boats forces a choice,
and it is the faulted river with the smallest symbol - deterministic across
polls, unchanged in wire shape, and enough to answer whether any river faulted.
`docs/cli.md` states what a poller should do with it.

## Clocks: the venue's reference against a boat's

The venue also retains a wall-to-sim reference, but it is not a placed boat's
clock. It bounds history for a boatless river, drives the venue deadline, and
stamps the venue-scoped pulled account ledger. A boated river instead answers
only through the instant its own boat has published.

The ledger stays venue-scoped because one engine serves every river, so a
pulled `/account` snapshot has no boat axis to sit on: stamping it from any one
boat makes it ahead of or behind a push from another. `GET /account` therefore
keeps the venue stamp and labels it, adding a `clock: "venue"` field beside the
otherwise unchanged `AccountState` so a consumer can never mistake that
`ts_event` for boat time; pushes are ordered against pulls by sequence.
`/clock` goes further and renders only the venue's: it named a river and rendered
that river's boat clock until that made an anonymous route a boat-discovery
surface, so the parameters are refused now and the per-boat instant reaches a
passenger the only way it honestly can, stamped on the frames its own boat
publishes.

Each boat has its own settlement watermark and its own ring. Market water is
exogenous: orders never move it and there is no queue competition. Fifty agents
submitting the same buy against the same water receive the same fill without
changing one another's result. Transport havoc remains a property of what a
passenger sees.

## Generator havoc forks the river

Generator havoc is river identity. A passenger whose resolved config carries a
generator arm boards a different river than one without it, and nothing mutates
water someone is already reading. The arm arrives on the `/ws` upgrade as four
query keys, is normalized into a `GeneratorArm` before the account is claimed,
and enters `RiverKey` structurally beside the bundle digest. The generator
receives it through `with_surge`, which consumes a source that has drawn
nothing, so the window is installed before the first draw and every checkpoint
of that river carries it.

That is what removed the checkpoint machinery the mutation needed. The pinned
control boundary, its exemption from coarsening, the walk-back fence and the
fence's own `last_trade_price` recovery all existed to stop a snapshot taken
before an arm from replaying the span after it unsurged. With the window present
from the origin there is nothing to fence: resuming from any snapshot, or from
the origin itself, stays on one realization across the surge's opening and its
expiry alike.

Two asymmetries in the key are deliberate. The bundle digest covers the
operator-owned half - instrument shape, tape seed, boot regime - and the arm is
held exactly, because a passenger chooses the arm and a digest collision would
hand it water belonging to another key rather than merely mislabelling one. And
the multipliers are canonicalized to parts per million before they become
identity, because equivalent human inputs do not produce equal floats and the
river cap does not evict.

`Rivers::river` validates by issuance rather than by re-derivation. It used to
ask whether a key equalled the key the registry resolves for its symbol, and
that question has only one answer, the armless one - so after the fork it would
refuse every armed key, while deleting it would admit a key nothing minted.

What the fork owes, recorded because it is larger than adding a field to a key.
Every water read now takes a `RiverKey` rather than a symbol - history, the
order-time market reading, the trigger scans, marks and settlement - so a fork
cannot land halfway and leave execution reading a river the passenger is not on.
What remained unanswered was ownership rather than plumbing, and the owner ruled
on it 2026-08-22. Two of the four rulings have landed and are described above:
the arm rides the upgrade rather than a registered bundle or a posted default,
on the grounds that in server mode the config file belongs to whoever launched
the exchange and a posted default is run-wide state; and the arm carries a
coordinate rather than opening at the boarding instant, because two passengers
carrying the same arm a second apart would otherwise ask for different water and
every late boarding would fork a river of its own. The base is the run origin,
which is where the old boatless handler already stamped its window.

## History is read over the socket

History is read over the socket. A poll names a symbol and no passenger, so once
a label names several rivers it names none of them - and every proposed selector
restates at the history call what the upgrade already settled, which is a second
place for identity to be stated and therefore to drift. A passenger's socket
already names its boat and so its river, so a `QueryHistory` carried there names
nothing and cannot name it wrong. This follows order entry, which became
websocket-only for the same reason. The premise it rests on, recorded because it
is the load-bearing one: every history read comes from a party that has already
boarded. `/operator/trades` and `/operator/quotes` serve the unarmed river of a
label on the run clock, and the path carries the demotion because prose cannot -
a route that kept its old spelling would have gone on answering a consumer
plausibly while its meaning changed underneath.

The operator view still materializes the river it is asked for, and that was
argued the other way before it was checked. A read that creates has real costs -
a typo permanently spends one of 256 never-evicted rivers, and `/instruments`
changes as a result of being looked at - but the glossary settles it: nothing
has to be boarded for history to answer, so refusing a label no passenger had
boarded would make cold history unservable.

A page's cutoff for a shared placement is the tighter of the run clock and the
asking passenger's own boat clock. The run bound keeps any caller from reading
past the venue's present; the boat bound keeps this one passenger from reading
past its own, which on an unpaced or slow-boat run is earlier. Without the
second, a strategy warming from its own history would read water it had not
been delivered - the look-ahead the first bound exists to prevent, arriving one
level down. A named placement is bounded by its own boat clock and its window
end alone, with no run-clock term: its boat is anchored at `window_start_ns`
and delivers from there whatever the run clock reads, so its frontier can
legitimately lead the venue's, and clamping it to the run clock starved the
warmup backfill the window's floor promises - answered as an empty complete
page a consumer cannot tell from a quiet market - while making the answer a
function of the venue's boot instant, the wall-clock input a named run is
defined not to carry. No look-ahead opens either way, because history never
crosses the asking boat's own delivery frontier.

Still owed:

One account riding two rivers of one symbol is unpoliced operator error. A
resting order and a position are recorded per instrument, so such an account has
orders that match both boats and one `last_marks` entry that two boats race to
write - which decides unrealized PnL, the peak-equity ratchet and therefore
whether a risk rule fires. The venue neither refuses it nor rekeys the ledger to
express it. It is unreachable through a nautilus host, whose two legs must carry
identical water, and a consumer that genuinely wants clean and surged water side
by side uses two accounts. The glossary's non-interference and invisibility
properties are unaffected, because both are stated over passengers of different
accounts and this is one account's passengers colliding with each other.

A perpetual's funding index has the history problem and no passenger to ask,
and is not settled by the above. A timed havoc window carries a
wall arming instant and a
simulated span rather than one boat's absolute deadline, and every passenger
judges it on its own clock.

## The fill sweeper

The fill sweeper is one task on an earliest-deadline schedule over the placed
boats, keyed by boat identity rather than by allocation address, each boat
re-armed on its own clock and floored in wall time so an accelerated run cannot
turn the pass into a hot loop. The consequence to know: a river with no placed
boat is not swept, because a sweep needs a clock to sample, so resting orders
on a wound-down river stay unscanned until someone boards again.
Each boat also carries a monotonic completed-pass count, advanced after the
whole pass including every seated account's engine and delivery work. It is
observation only: neither scheduling nor engine behavior reads it. It is
published on `GET /account`, one row per boat the named account is seated on,
and deliberately not on `/health`: `/health` answers without an identity, so a
per-boat list there is an anonymous boat-discovery surface enumerating every
other account's symbols and cadences, which is what `/clock` was cut back to
remove. The account-scoped row carries no speed, because one ledger carries one
cadence per river and the symbol therefore names the seat on its own.

## The tape identity, and what each bump changed

An instrument is a bundle of knobs, not one fixed shape. Five classes are
selectable, split by settlement shape and set out in full above: spot, equity,
future, perpetual and inverse. The two that this paragraph's size-grid argument
is drawn from are a spot currency pair, and a cash-settled continuous future
with a contract multiplier, whole-contract quantities on the order path and on
the tape, and no expiry or roll. The generator's size grid is multiplier-aware -
notional per unit is `multiplier * price`, and a contract draw is rounded half
away from zero and floored at one contract, so no print becomes the zero
quantity nautilus drops. `latent_size_median` is stated directly in the
instrument's native size unit and names the continuous lognormal center before
that grid is applied. The floor truncates its lower tail, so it is deliberately
not called the observed size median. `TAPE_PROTOCOL_VERSION` is 26; version 5
removed the quote-notional proxy whose value was actually arithmetic mean
notional and made the latent size distribution explicit, and version 6 repaired
the GARCH recursion's second moment. Version 7 added the observable top of book,
version 8 added the instrument session profile, and version 9 split stochastic
parent advancement from wire-object materialization. Version 9 deliberately
preserves the version-8 tape byte for byte, but changes the generation path and
therefore advances the process version under the unconditional versioning rule.
Version 10 landed the July 2026 MNQ TBBO fit: floor-aware child-count
conditioning (below the point where the quiet-hour multiplier would push the
mean child count under one, the conditional parameters are re-solved so the
configured unconditional targets survive the one-child floor; above it the
legacy path is byte-identical), a per-instrument `size_log_sigma` whose default
reproduces the shared crypto shape byte for byte, and the fitted MNQ preset
values with MES inheriting them loudly.

Version 11 repaired a unit mismatch in the session calibration. `vol_hour` had
been fitted at protocol 8 as a per-minute quantity from NQ one-minute bars but
is applied per parent event, and minute-scale volatility carries the per-parent
scale times the square root of the arrivals in that minute - so the fitted 3.4x
hourly swing compounded with the 27.5x arrival swing and left the generated
Asia and London sessions roughly five times too quiet at bar scale. Both
session arrays were refitted from the July MNQ TBBO corpus as one atomic group:
`intensity_hour` from inferred-parent counts conditional on the frozen
`dow_weight`, landing 14.5x peak-to-trough against the volume proxy's 27.51x
upper bound, and `vol_hour` as a per-parent robust scale, which comes out
nearly flat and slightly inverted - overnight parents individually move a
little more than cash-session ones, and the old curve's swing was almost
entirely the arrival-density double count. `vol_scalar` was re-solved under the
corrected arrays and ships declared rather than fitted: its pooled scale gate
passes while the per-seed minute-range envelope still fails, which is the
standing tail-shape evidence.

Version 12 repaired the protocol-12b arrival-frame calibration: integrated
families take the bare mean. No shipped preset declares the arrival seam, but
it moves outputs for `(config, seed)` pairs already expressible under 11.
Version 13 normalizes the decimal price before it is hashed into the fill-band
draw key, because `rust_decimal`'s serialized form carries the scale and made
the band a function of how the consumer spelled its price - `100` and `100.00`
drew different triggers and different slippage for the same order. Every
seeded fill trigger and market-slippage offset therefore moves at 13. Version
14 makes a scheduled calendar jump part of the `ReopenGap` crossing frontier,
so an arm inside a closure cannot be skipped forever. The protocol-12b
mechanism landing, long pencilled in for 13, now takes 15. Version 16 retired
the second unnamed default knob bundle without moving generated bytes. Version
17 keys each served river's tape root by the requested symbol label, moving
every served tape while leaving offline generation seeds untouched. Version 18
is the boatyard landing: placement, pacing and the per-boat clock moved off the
one venue-wide replay, so what a socket receives is a function of its boat
rather than of the run.

Versions 19 through 26 continue the same process record. Version 19 added the
configurable equity, forex, perpetual and inverse classes. Version 20 added the
session-segment composer, and 21 railed its price and its clock, so an endless
integration cannot walk to zero or overflow its nanosecond axis.
Version 22 corrected the interaction between a flow surge and the arrival
kernel. Version 23 removed the control-plane clear operation. Version 24 moved
generator arms into river identity, so an arm forks water instead of mutating
water already being read. Version 25 made composer termination a typed source
fault while centralizing the route and divergence-kind registries. Version 26
added named tape-window placement without putting the window coordinates into
river identity.

Not every bump moves every tape, and the record for the crypto lineage is
specific enough to be worth stating, because a reader who knows the bumps are
unconditional will assume the opposite. `analysis/protocol9-tape-hashes.json`
freezes FNV-1a hashes of six simulated hours of canonical BTCUSDT trade and
quote lines, at two seeds plus a flow-surge arm, taken when the tape identity
was 9. `protocol9_tape_oracle` in `mogwai-cli`'s `gen` module asserts equality
against it, and at the current tape identity it still matches - the assertion
is live in the gate, so that sentence is enforced rather than dated, and it
carries no version number of its own for a later bump to falsify. The identity
itself is stated once above, in the phrasing
`crates/mogwai-data/tests/tape_version_prose.rs` checks; a second statement of
it here in any other phrasing would be exactly the durable-claim defect that
test exists to prevent.
So none of the intervening bumps - 12's arrival-frame calibration, 13's
fill-band decimal normalization, 14's `ReopenGap` crossing repair, and onward -
moved any of the bytes this oracle observes (15 is not among them: it remains
the arrival-mechanism reservation, held for a protocol-12b mechanism landing
that has not happened, so no commit has ever set the constant to it): the first six simulated hours of the offline generation path, at
those three arms. It does not walk MNQ, it does not walk the venue's river
placement, and it does not run past six hours, so it is evidence about the
crypto generator's core draw rather than a blanket identity. That is consistent
with each bump's own scope: 13 is a fill-band key and 14 a calendar crossing,
neither of which the crypto preset's generation reaches, and 17's per-symbol
tape root keys the venue's rivers while leaving offline generation seeds
untouched. The
version constant is a process identity that advances whenever a change could
move a tape; it is not a claim that every tape moved.

## The synthetic top of book

Each generated parent event publishes one BBO before its first trade. The book
has an exact positive integer-tick width and is centered, with one rounding, on
the drifted latent mid. Every child in the parent sweep shares that book. Parent
trades are displaced from the published midpoint, so the default one-tick width
and half-tick displacement print at the touch. Width, top sizes, and trade
displacement are separate per-instrument calibration seams; a measured TBBO
corpus supplies them for MNQ (and, by stated inheritance, MES) as of protocol
10, while the BTCUSDT preset remains explicitly uncalibrated because no quote
evidence covers spot. Displacement is not
capped by width: the published BBO is only the top level, so an aggressive
parent may print beyond the touch without making the book malformed. A connecting WebSocket
receives the current BBO snapshot before later tape frames, and the adapter
retains that snapshot until its host activates quote delivery.

That snapshot is conditional, and a consumer must not read it as a
snapshot-first wire contract. `Tape::subscribe_with_snapshot` hands back an
option: the boat retains the last quote it published, so a socket binding in
the window between a boat's first trade and its first quote is handed no
snapshot and sees that trade as its first market frame. Nothing is hidden by
the absence - the snapshot is missing only when no quote has been published on
that river yet, and the tape's own first quote follows immediately, so there is
no case where a bound socket holds a stale BBO or none at all for long. A test
or a consumer that requires the first frame to be a quote is asserting
something stronger than the venue promises, and it will lose that bet at boot;
drain to a deadline instead. `scripts/smoke.py` did exactly that and flaked
twice in one day before it was corrected.

## The generator's volatility process and its three rails

The volatility innovation is standardized to unit variance before it reaches
that recursion. The `a0` derivation has always assumed this, but the innovation
was a raw Student-t whose variance is `df / (df - 2)`, so the true condition was
`a1 * E[z^2] + b1 = 1.115` and the process had no finite stationary variance: it
stayed bounded only by its own rails, ran 8.17x hotter than `vol_scalar`
claimed, and sat pinned at the variance cap 12.96 percent of the time. `a1`,
`b1` and `vol_scalar` were re-solved against the corrected condition.

Three rails are named separately because they answer to different things. The
GARCH state cap and the feedback-return ceiling bound the base process and are
never scaled by a regime, so an armed divergence cannot raise the process's own
ceiling and change what it does after the divergence ends. The realized-return
ceiling is absolute and applies after session and regime scaling, so a
divergence is an output envelope. That ceiling is a stated product policy sized
against a measured maximum-strength envelope, not a fitted market quantity: as a
log return it permits about +5.13 and -4.88 percent in a single event, and it
does not bound cumulative movement over many events.

## Two structural fidelity limits no parameter can remove

Two structural fidelity limits of the generated futures river are stated here
because no parameter can remove them. First, the calendar-driven baseline has
no automatic reopen jump: a real session reopen prints a discrete gap where
the closed-hours information arrives at once, while the generated mid resumes
its random walk from where it halted. An explicitly armed `ReopenGap`
divergence can inject such a jump on a subscription's view, but the clean
baseline river never produces one, so on it overnight gaps are absent and any
large single-minute range the river does produce is a volatility-cluster tail
inside a session, not a reopen - a different phenomenology occurring at a
different time of day. Second, the session profile modulates intensity and
volatility by hourly factors, so within-hour structure (the opening minutes'
concentration at the cash open, the settlement flurry) is smeared uniformly
across each hour; the profile reproduces hour-scale contour, not
minute-scale texture.

## Fingerprint ranges diagnose; mechanism validation gates

Fingerprint ranges are corpus-labelled observations. They select defaults and
produce operator diagnostics, but never admit or reject an instrument. A
shipped preset must either produce no diagnostic or accept its stable code on
the matching provenance entry; exact-set validation also rejects stale
acceptances. Hard
generator validation is mechanism-derived instead. In particular, the latent
size center must not sit two orders of magnitude below the minimum tradable
quantity, volatility must sit below the GARCH sigma cap so the process is not
born clipped, and the tick must be representable at the declared precision.
Rail headroom is diagnosed rather than gated: a universal ratio of scale to rail
would repeat the same mistake in another dimension, denying a legitimately
higher-volatility instrument. Return ceilings remain shared module-level process
shape: a coarse truthful grid is allowed to produce a stickier latent mid rather
than receiving an uncalibrated stress-tail lift.
The dimensionless `tick_return / vol_scalar` and its squared random-walk crossing
estimate are exposed as diagnostics for deriving event-price repetition; they
do not pretend to model sweep stepping, bounce, recentering, or explicit repeat
draws by themselves.

## A future's ledger, calendars and fees

A future's ledger is single-currency and collateralized. There is no base leg:
a fill moves the position and the VWAP, a quantity-reducing fill books
`(fill - avg) * closed * multiplier` of realized P&L straight into the
settlement balance, and margin - `maintenance_per_contract` per open contract
plus `initial_per_contract` per resting non-reduce-only contract - is what the
account locks, reported per symbol as posted margin the adapter forwards as
nautilus `MarginBalance` rows. Reduce-only orders place no hold, which is
what makes two bracket legs against one position exclusive rather than
additive. The sweep pass marks every open futures position to the tape, strikes
every settlement instant the calendar crossed at its own instant rather than at
the sweep boundary, and its engine phase emits at most one account snapshot,
recomputed after the mark, expiry and funding it covers, so no consumer sees a
stale `mark_px`. At most one, not exactly one: `DropNextAccountUpdate` works by
that phase emitting none, and risk enforcement may append its own snapshots
afterwards. Settlement moves the accumulated
difference into actual cash and resets the VWAP to the settlement price, which
is why a losing futures position drains an account rather than merely carrying
a worse unrealized number. A breach is `total_balance + unrealized <
maintenance` - the total balance, because the locked amount already is the
maintenance requirement and subtracting it twice liquidates solvent accounts.

A scheduled close is configuration, not havoc: `[instrument.calendar]` names
the open windows of the week in exchange-local time, the generator jumps a
closure whole rather than emitting inside it, and a consumer can know about it
in advance. `ReopenGap` remains havoc and remains unscheduled. Fees are a
per-instrument maker/taker schedule; liquidity side is decided where the fill
is produced, so a resting limit the sweep fills is a maker fill even though the
same order would have been a taker fill had it been marketable on arrival. A
funded account must cover the fee as well as the notional or the margin: the
submit check, the amend check and the fill check all add commission to the
requirement, the first two against the worse of the two rates since which side
the order provides is not known until it fills. A venue-originated liquidation
is charged the configured schedule but never a consumer-armed `FeeSurcharge`.

Margin and fees are treated here as instrument identity, which is not how
markets work: real schedules vary by account tier and real CME margin varies by
product, volatility, portfolio and time. A fixed per-contract margin and a
fixed schedule are declared simplifications of the venue's model, not
descriptions of the market's.

## Seeding, the tape origin, and the two durations

A run draws one 64-bit seed at launch, or takes it from config, and every
random stream in the run - each river's tape generator and the fill band -
derives from it by domain-separated derivation; nothing else in a run is
random. The fill root is run-level, because the band's draw key already carries
the order's symbol; a tape root is per-river, keyed by the requested symbol
label as described above, so two labels resolving to the same shape never share
a path. The tape's origin is the fixed constant `TAPE_ORIGIN_NS = 0`; every river's
warmup is generated on first read, and the
run proper begins at `run_start_ns = TAPE_ORIGIN_NS + warmup_ns` on the same
axis, so `data_origin_ns` is always `TAPE_ORIGIN_NS` and history outside
`[data_origin_ns, sim_now]` is refused. The venue has one tape origin, one
default placement origin and one warmup span, but N rivers and as many named
placements as their passengers request. A named `[start, end)` placement has
its own history floor at `start - warmup_ns`; admission refuses a start whose
floor precedes `TAPE_ORIGIN_NS`. This is why the readiness record
carries those three time facts and no symbol. A run is therefore a
pure function of `(seed, config)` for a given build and fingerprint - with the
limit that a new seed only draws a new path from the one fitted model behind
the fingerprint, so marginalizing over seeds reduces variance conditional on
that model rather than adding out-of-sample market evidence.

Two durations exist and they are not the same object. The run duration is
configured, starts at `run_start_ns` rather than boot, and is measured on the
venue clock; at its deadline the venue announces `RunComplete`, closes
WebSockets normally, drains, and exits zero. A passenger duration is the
socket's own `duration_ms`, simulated milliseconds measured on its boat's clock
from its boarding instant, so passengers with different durations still share
one boat and each closes at its own deadline while the boat winds down only
when the last of them leaves. A named window replaces that passenger duration:
its boat clock starts at `window_start_ns`, stops publishing at
`window_end_ns`, and announces the same passenger-scoped completion with the
declared span `end - start`. The deadline is absolute where a duration is
relative. A socket joining a placement its paired leg already opened waits only
`window_end_ns - sim_now`, so both legs end at the window's end and neither
reports an instant outside it - which is what keeps a named run a pure function
of seed, config, symbol, start and end, with the boarding instant nowhere in the
observable. Its `elapsed_ns` is still the ride it actually took, so a late leg
reports less than the span it declared. The venue's completion instant is the signal that
crosses to a socket; the numbers on the `RunComplete` frame are always
re-derived on that socket's boat clock, and `elapsed_ns` is how much tape that
boat actually covered.

The two endings are different frames, `RunComplete` and
`PassengerDurationComplete`, and the close reason behind each agrees with it.
They were one frame until the split, so a consumer classifying on frames called
its own deadline a finished run and the close reason that could have told them
apart was documented as unreachable. A passenger's frame carries the span it
observed since it itself boarded - a shared boat can predate its passenger, so the
boat's own epoch is somebody else's boarding - alongside the deadline that
fired. When both deadlines fall in the same instant the run wins, because a
finished run is the stronger fact and the one that stays true for a consumer
deciding whether to redial.

## History refusals, and what bounds a page

The history endpoints refuse rather than return an empty page on every
impossible request, so a refusal is never mistaken for a span nothing traded
in. A start before the tape origin or past the ceiling is a 400; so is a
shape-class refusal decided before the synthesis task runs - an illegal label,
a shape that does not validate, a funding-barred one, or an exhausted river
cap. A synthesis failure is a 500 naming the symbol and the window. There is no
"symbol this run does not serve" axis any more: resolution is total, and a poll
materializes the river it names, which is also what makes it advertise through
`/instruments`.

The ceiling is the run clock, one snapshot per request, and it consults no boat.
It was the furthest-published boat on the named river, which made one
passenger's delivery frontier decide another's window: board the same water at a
faster cadence and you moved somebody else's ceiling, which they could watch
move. That is the observation the Boat entry forbids, and it was not a property
of the river in any case - speed belongs to a boat's identity, a tape is what one
boat publishes, so a maximum over boats is a maximum across different tapes. A
river has no present of its own; it is deterministic water with no cursor.

An `end` past the ceiling is clamped rather than refused, deliberately asymmetric
with the start, and it is clamped whether it was stated or not: an explicit end
bounds a window and never authorizes crossing the run present. A consumer paging
a window pins its own end by reading `/clock` once before its first page.

The surrender is stated because it is real. A boat-free bound and a
delivery-shaped bound cannot both exist - a firehose that has delivered through
some instant and a river nobody has generated past are indistinguishable to
anything that does not consult a boat - so on an unpaced run history trails what
a socket already holds, and that water cannot be re-fetched here. Preventing
look-ahead is worth more than reproducing delivery: this venue exists so a
forward claim is worth something, and a run that read its own future would look
clean and not be. Repairing one passenger's own gap would need tape identity an
anonymous route does not carry.

## The adapter's side of the wire

The protocol crate owns every JSON type shared by venue and adapter. The adapter
uses its websocket for market data, execution and history alike: warmup and backfill
are pulled with `QueryHistory` on the socket that boarded, in bounded pages
resumed by an opaque continuation. It makes no HTTP history call at all, which
is what stops it reading a label's water instead of its own. Each adapter
consumer names its river with an optional `symbol` in its own config, which
becomes `/ws?symbol=`, and optional `speed` and `duration_ms` alongside it,
which become the rest of the upgrade query. All three default to absent, which
takes the venue's own defaults, so a host that configures none has its data and
execution consumers board the same boat at the venue's configured speed - and a
host that configures them must configure both legs alike, because one ledger
carries one cadence and the second leg's upgrade would otherwise be refused.
`speed` is judged before the dial against
`mogwai_protocol::control::validate_delivery_speed`, the same function the
venue's boat quantization judges it with, so a value the venue would refuse is a
config error rather than a permanent dial failure. The adapter holds no served-symbol guard
of its own any more: since resolution became total there is no set to guard
against, so both clients re-read `/instruments` after binding - binding is what
registers an unconfigured symbol, so only a read taken after the socket is up
can see it - behind a watch-gated readiness barrier that black-holes delivery
until the reseed says go, and a failed reseed tears the connection down rather
than wedging the delivery pump.

## Raw fills, and how the fill band was calibrated

The generated river carries BBO updates and raw fills, not aggregated trades. One parent
match event updates the latent market once and emits a same-side sweep of one
or more children, one microsecond apart, walking monotonically in the take
direction. Its BBO is emitted first at the parent timestamp and remains the
governing book for every child. Consumers that count trade ticks therefore count raw fills. At the new
cadence a 300-second fill-band window carries about 15,700 returns; the L2
probe observed zero cold or budget refusals in 128 readings spanning ~21
simulated hours, closing the former cold-reading defect without changing the
estimator.

The same probe reports the other side of that shift, and the fill band was
re-calibrated against it. The estimator's 300-second window now carries ~15,700
returns where it carried ~32, so the horizon return it reads rose by two orders
of magnitude: at the former `fill_band_vol_mult = 0.5` the implied band came out
a median 439 ticks with a p90 of 703, above the 200-tick `fill_band_max_ticks`
clamp at nearly every instant. A clamp-saturated band is drawn uniformly across
the whole clamp range no matter what the tape is doing, which is the mirror
image of the inert `0.0` band - in neither case does the tape decide the fill.
The shipped default is now `0.005`, the smallest multiplier whose median implied
band falls inside the probe's usable 3-to-100-tick window (median 4 ticks, p90
8). The probe is the provenance: re-run it if the fingerprint or the cadence
moves again, and re-bless the fill golden, whose banded scenario reads the
default rather than restating it.

Re-run against tape protocol 6, whose GARCH repair moved realized volatility by
roughly 1.3x in RMS, the selection is unchanged: `0.001` reads median 0 and p90
1, `0.002` median 1 and p90 3, `0.005` median 4 and p90 8, `0.010` median 9 and
p90 16. `0.005` is still the smallest multiplier satisfying the median rule, so
neither the band nor the fill golden's banded half needed to move.

## The three tiers of a market reading

Reading the market at a submit pays one of three tiers. Acceptance-time
readings are memoized on the boat - one memo per boat, bucketed by fill-sweep
interval on that boat's own clock - and a submit sees a reading that may be up
to one interval stale. A market order therefore fills at or beyond the market
as of that reading, not as of the fill instant. The memo lives on the boat
rather than on the run because the bucket is a function of the boat's clock
and the reading it saves is a reading of one river: a run-level memo held a
single entry, so two symbols evicted each other into a guaranteed miss and
then serialized behind one mutex. A memo miss is served from the boat's
resident vol window (`mogwai-venue`'s `vol_window`): the tape thread keeps the
trailing `VOL_WINDOW_NS` of prints it was already producing, and the miss
folds them through the same shared arithmetic the tape walk uses
(`mogwai_data::vol_reading_from_trades`), about 0.2 ms in release. Only where
the window cannot prove it would match the walk - a cold boat's first 300
simulated seconds, a bucket the pacing has not proven, `speed = 0.0` - does
the miss fall back to the full tape walk, about 8 to 10 ms as of 2026-08-23
(the 300 s volatility window is that walk's cost; the checkpoint stride repair
had already cut the positioning residual). The two paths are one estimator by
construction and are pinned bit-identical; `reference/performance.md` carries
the measurements. The miss lock is held across whichever path serves it, so
two passengers landing in the same bucket pay for one reading rather than two.

The exact-instant mark and settlement reads that never come from this memo fail
differently from each other on purpose:
an unreadable ordinary mark costs one pass of unrealized P&L freshness and is
dropped, while an unreadable settlement price refuses the whole read and leaves
the watermark where it stands, because nothing looks back past a watermark that
has moved.

## The workspace and the offline evidence toolbox

Seven crates. `mogwai-protocol` owns the wire types and the shipped launcher and
imports nothing else in the workspace. `mogwai-engine` is the venue-agnostic
exchange core. `mogwai-data` owns `TickSource`, the k-way merge and the
`GeneratedSource` synthetic generator fitted to the committed fingerprint.
`mogwai-venue` is a library - it owns the sockets, the clock and the replay
pacing, and ships no binary of its own. `mogwai-cli` is the `mogwai` binary: a
clap dispatcher over `serve` (which does no work itself, just forwards to
`mogwai_venue::serve`) plus every offline subcommand. `serve` is the only one
that binds a socket; the rest are the intake and measurement surface - `gen`,
`tick-composition`, `presets`, `man`, `preflight`, `measure`, `fit`, `cache`,
`synth`, `cadence-feasible`, `characterize`, `select-windows`,
`session-profile`, and the protocol-12 instruments (`count-curve`, `stage-m`,
`minute-range-envelope`, `arrival-control`, `arrival-screen`,
`arrival-envelope-diagnostic`, `tick-composition-ratios`). `mogwai --help` is
the authority on the current set. `mogwai-adapter` is
the lone nautilus-dependent crate, unchanged by anything below.

One binary is a standing decision, not an accident of growth. A split into a
venue binary and a lab binary was proposed and refused 2026-08-20, and a
re-proposal has to answer three things. First, `arrival-control`'s B1 gate
execs `gen` on `current_exe` so the binary generating the byte comparison is
the very binary under test - the driver cannot disagree with itself about which
build ran - and a split reintroduces exactly the build-identity ambiguity that
design forecloses. Second, the size benefit is already banked: the method
lives in `mogwai-lab`, which stays linked into the venue binary regardless,
because `gen` reaches into it and `main` calls `sidecar::init` before the argv
parse; a split relocates thin driver layers while the intake method remains
shipped. Third, the cost is on the order of two hundred path rewrites across
one-shot brick drivers, relocated integration suites, a moved attestation
roster, and a new build-identity mechanism to replace the one the split
destroys. Two potential hard blockers were checked and are not part of the
refusal: the crate direction admits a lab binary, and the `test-seam` cfg
survives a move - the refusal rests on the three arguments, not on a build
obstacle.

`mogwai-lab` is the fifth non-adapter crate: the corpus-to-fingerprint method
library the 2026-08 Python-to-Rust rewrite absorbed from `analysis/` (the
rewrite program's phase records and per-script scope rulings are retired to
git history) - streaming TBBO/Binance-trades
parsing, the protocol-12a measurement engine, aggregation and bootstrap,
fingerprint and cadence synthesis, and the protocol-11 session-calibration
fit. Its dependency direction is one-way and asymmetric: `mogwai-lab` depends
on `mogwai-data`, `mogwai-protocol` and also `mogwai-venue` (session-summary work
needs to resolve an `InstrumentProfile` through `Config::load` exactly as the
Python's `--config` scratch walks did), but `mogwai-venue` depends on none of
it - there is no cycle, and `mogwai-lab` stays out of the tape-generation path
`TAPE_PROTOCOL_VERSION` scopes, the same reason `measure12a.rs` was
consumer-only inside `mogwai-venue` before the rewrite moved it. `mogwai-cli`
depends on `mogwai-lab` for the pieces that need no `mogwai-venue` preset
resolution
(preflight, cache, most of measure/fit/synth) and calls straight into
`mogwai-venue` for the generated side of measurement.

The instrument set is open, and that is why `mogwai-lab` is a library rather
than a folder of scripts. A symbol is a request string, never an admission
identity. `InstrumentDef` is derived through one path from the symbol and the
operator overlay: an explicit preset, a matching preset, or the BTCUSDT default
bundle. No second hardcoded default bundle exists, and no symbol is refused for
wanting a fit. The three shipped presets - MNQ, MES and BTCUSDT - are the
current state, not the end state.

Config declares no closed instrument set. It supplies a default knob overlay
and optional case-insensitive per-symbol overlays for total symbol resolution.
The top-level default symbol is what a request carrying no symbol binds - a
carrier convenience for consumers that predate the parameter, not a privileged
river. It is materialized on first request like any other, and other request
symbols materialize their own rivers in the same run.

The intake sequence therefore makes a river better and gates nothing:
survey what cheap data exists, decide whether a paid corpus is worth buying
and which windows of it, buy, preflight, measure, characterize, fit, ship a
preset with its provenance. The offline toolbox is that sequence made
reusable, and the two consequences bind anything built on it. A component is
spent only when its question cannot recur, never merely because the MNQ pass
answered it - an archive inspector or a corpus driver is idle between
instruments, not dead. And per-instrument knowledge belongs in config or a
preset rather than a hardcoded list in the method: a preset tuple naming
today's three symbols is a defect the fourth exposes. The corollary for
evidence is that a finding measured on one instrument is one observation, not
a law, until a second instrument either reproduces it or does not - which is
why methods a preregistered test rejected are kept runnable rather than
deleted.

The second consequence is the direction of travel, not a met invariant, and
stating it as met would be false. The offline toolbox still fixes
per-instrument choices in source, faithfully mirroring the Python it was
ported from rather than introducing the debt: `cadence.rs` fixes the pair set
and the archive month and takes BTCUSDT as anchor, `fingerprint.rs` takes
`XBTUSD` as anchor, and both `session_profile.rs` entry points resolve the MNQ
preset. None of these is reachable as an input. Retiring the Python removes no
parameterization that exists today - it was equally hardcoded - so closing
this is forward work rather than a porting debt, and it is what a second
instrument will force.

The parity contract a port is held to, stated once because every case
otherwise gets argued from scratch: for every valid input, the Rust must
either produce output equivalent to the implementation it replaces or embody
an explicitly approved semantic change. It may additionally reject inputs
outside the declared input contract. It may never silently accept malformed
input, and it may never silently change results for valid input.

The line that follows from it, and the reason it is worth writing down: a
gate passing on the committed fixtures is evidence about those fixtures, not
proof of equivalence over the contract. So a Rust refusal where the original
proceeded is a loud narrowing and needs only to be recorded; a Rust result
that differs on some valid input the fixtures happen not to contain is a
silent mismatch and must be fixed or approved; and a Rust default where the
original raised is silent acceptance of malformed input, the worst of the
three, because it manufactures an answer. Fixing the third class by making
the committed artifact pass again is not a fix - the repair needs a fixture
chosen to distinguish the implementations, or the blind spot survives.

The rewrite's parity gates are the porting program's whole verification
story: every absorbed Python computation is checked against a committed JSON
artifact - `mnq-fit-preflight.json`, the observed and generated halves of
`mnq-measure-12a.json`, `cadence.json`, `fingerprint.json`, `mnq-fit.json` -
typed-canon-identically, with named, individually-verified exclusions for
genuinely live fields (wall-clock cost, the binding harness commit) rather
than a blanket tolerance. The gates live under
`crates/mogwai-lab/tests/parity3a*.rs`/`parity3b*.rs` and
`crates/mogwai-cli/tests/parity12a*.rs`/`parity3b.rs`, `#[ignore]`d because
they need local corpus or archive state on disk, and are excluded from
`brokkr.toml`'s complete profile by the shared `parity12a_`/`parity3a_`/
`parity3b_` naming prefix. The program-level review dossier - every gate,
every pinned cross-language convention (compensated float summation,
insertion-ordered accumulation, the ported CPython float repr and Mersenne
Twister, and the rest) and every owner decision the review adjudicated - is
retired to git history; the review signed and the program is complete.

The storage policy `mogwai_lab::storage` implements keeps three classes of
on-disk data apart, never mixed. Artifacts (preflight, measurement and fit
outputs) are the user's files: written to `--out` or a subcommand's own
working-directory default, never cached, never auto-deleted. Cache
(recomputable, keyed data - walk summaries, measure12a walk records) lives
under `$XDG_CACHE_HOME/mogwai/` (falling back to `~/.cache/mogwai/`),
overridable by `MOGWAI_CACHE_DIR` or `--cache-dir`, keyed by a
`ProvenanceToken` folding in the crate version, `TAPE_PROTOCOL_VERSION`, the
fingerprint hash, the full invoked command line, the measurement
sub-contract hash and (when built from a tree) the git sha; entries under a
stale token are unreachable by construction and pruned automatically on
write, with `mogwai cache stats`, `mogwai cache stats --entries`,
`mogwai cache clean` and `mogwai cache clean --stale --keep TOKEN` covering
the manual case. `--keep` is required with `--stale`, and the token must name
a directory that is actually present: a cache entry's provenance token binds
the command that produced it, which a `cache` invocation cannot derive, and a
token matching nothing keeps nothing - so both the missing and the mistyped
token refuse rather than pruning the lot. `stats --entries` prints the
candidates. Scratch (per-run temporaries) is a run-scoped directory under the
cache root with a leaf unique to the process, removed when its guard drops -
so two concurrent runs, or the two sweeps a full gate runs at once, cannot
share one. Repo development pins `MOGWAI_CACHE_DIR` to the Python-era
`analysis/out` layout so the phase 1-3b parity gates read the caches those
scripts already produced; that pin is not the installed default.
