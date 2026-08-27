# mogwai architecture: boats, clocks, delivery and history

Part of the architecture reference. The map and the reading order are in
`architecture.md`; this file was split out of the one long document on
2026-08-27 as a contiguous slice, with nothing moved, cut or reworded.

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
unnamed passenger places or joins a boat at the run's fixed origin; a named
window places or joins one at its stated start. The sharing key is the river,
the quantized speed and the placement start, and nothing else: whoever asks
for the same water at the same cadence from the same epoch shares one hull,
because a boat is a cache with no identity of its own - the tape is exogenous
and broadcast frames carry no passenger. Two accounts replicating one window
therefore read one cursor. Speed is quantized to micro-multiples in that key.
Duration is passenger-local and is not in the key, and neither is a named
window's end: the end is each passenger's own delivery cutoff, enforced by
the writer that owns its socket, exactly as a duration is - the hull runs
unbounded and winds down when its last ticket drops. An unserved speed is a
second cursor on the same water, not a refusal: speed mutates no generated
value. One ledger still carries one clock per river, and a clock is its rate
and its epoch together. Two sockets on the default account may ride two
rivers, but on a river that account is already riding, a second speed is
refused as a cadence conflict and a second placement epoch - a named window
at a different start, or the run's shared origin against a named one - is
refused as a placement conflict, because either would be two clocks judging
one book. Two named rides sharing a start and differing on the end are one
clock and coexist. Admission decides both from the seat, before any boat is
placed, comparing exactly what the boatyard keys on. The account counts its passengers per boat, and the
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

