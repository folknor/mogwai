# mogwai architecture: the venue's runtime shape and account model

Part of the architecture reference. The map and the reading order are in
`architecture.md`; this file was split out of the one long document on
2026-08-27 as a contiguous slice, with nothing moved, cut or reworded.

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

That change is load-bearing outside this repository, and the dependency is
named here so a future relaxation cannot be weighed as a local one. broadarrow's
percent and cash sizers read the account balance off `AccountState` and size
real orders against it, so under a socket carrying several ledgers the absent
wire-id check stops being a stamping convention and becomes a capital path: a
sibling ledger's equity would size a live order. Relaxing one-socket-one-ledger
therefore owes the re-check in `handle_account_state` in the same change, not
as a follow-up, and owes the consumer a breaking-change notice regardless.

