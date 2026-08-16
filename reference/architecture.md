# mogwai architecture

Mogwai is a fake venue. A direct launcher starts one foreground process and
receives a versioned readiness record as one JSON line on the child's stdout.
The process binds one endpoint and owns an open set of resolved instruments,
generated river tapes, and one ledger PER ACCOUNT.

A RIVER is a tape and is shared; a PASSENGER is one connected trader - its own
account, its own ledger, its own orders - and is never shared. The engine is
per passenger for that reason: one engine per process meant every client's
fills moved every other client's net, which is right for a venue owned by one
run and wrong for an exchange serving a batch. A `Passenger` is created on
demand, keyed by account id, and the id is the CLIENT'S: it outlives the
connection, so a socket presenting it again resumes that ledger rather than
opening a fresh one. The venue cannot distinguish a reconnect from a stranger
claiming the id and does not try, so an account id is effectively a bearer
token.

AN ACCOUNT IS ON AT MOST ONE RIVER, WITH ONE READER. A second socket presenting
a seated account id EVICTS the first, because a ledger read and written from two
places is one ledger with two notions of its own state. The evicted socket is
closed with `1000`, normally rather than as a fault: the venue cannot tell a
returning client from a stranger claiming the id, and treating an eviction as a
failure would make a consumer's reconnect ladder evict whatever evicted it. By
default the newcomer RESUMES that account - positions, order history and risk
state intact - which is what makes a killed worker able to come back to its own
book. `reset_account_on_reconnect` hands it a clean ledger instead, and the
readiness record reports which way the venue is set so a launcher never has to
infer it.

AN UNATTENDED ACCOUNT FREEZES. The moment its last connection goes away it is
not swept, not marked, not funded and not judged against its policy, and a
socket returning with the same id resumes it. This is a deliberate departure
from a real venue, where being away is no defence against liquidation: mogwai
exists to exercise a client's live path rather than to simulate an account
nobody is trading. THE CONSEQUENCE TO STATE IN ANY CLAIM is that a run spanning
a disconnect has a gap in its risk history.

RESUMING RE-BASES THE BOOK, because a returning boat is not the one that left. A
cursor is placed at its river's origin, so a frozen order's scan frontier - the
instant the departed boat had reached - sits in the new boat's FUTURE, and an
order left carrying it would wait for the new cursor to cover ground the old one
had already covered. Every surviving order therefore resumes scanning from the
returning boat's own clock. Nothing is owed for the span in between: nobody was
reading the account, which is the same statement the freeze makes.

WHAT THE ACCOUNT HELD OFF THE JOINED RIVER IS RETIRED at that moment - resting
orders cancelled, positions closed at their last mark. A returning socket may
name a different symbol than the account was trading, and carrying that forward
would leave it holding something the new session can neither see nor close.

AN ORDER ON A RIVER NOBODY READS IS CANCELLED rather than left, and this is the
other half of the same rule. An attached account's order on a symbol no cursor
is walking cannot fill, cannot expire, and cannot be told apart from one the
tape has not reached; the client is attached, so it is told. A frozen account is
exempt because it is skipped wholesale - its book is being kept for the socket
that comes back for it. Between the two, no resting order can sit indefinitely
on water nothing is reading.

A TTL BOUNDS THE FREEZE. `account_ttl_ms` collects an account nobody reclaims,
in WALL time because a frozen account has no simulated clock - the boat that
carried one wound down with the last socket. Zero, the default, keeps accounts
for the life of the process, which is what a consumer restarting a worker needs.
The setting is on the readiness record, so a consumer whose restart takes longer
than the TTL can assert on the fact rather than discover it as a clean ledger.

A connection that names no account is served under the venue's DEFAULT account.
That exists for the ephemeral single-client venue, where making the one client
name an id would be ceremony; it is not a venue-wide account every connection
shares.

An account may carry a RISK POLICY, which the venue ENFORCES rather than
reports. This is a risk-policy layer and not a funded-account feature: a live
venue has the same machinery, where an operator sets a daily loss limit that
behaves like a liquidation except that it lifts at the next session. A rule is a
triple - what it measures, on what basis, and what it does on breach - and the
breach action is what spans both worlds: `lock_until_reset` flattens and refuses
to open until the next reset, `terminate` flattens and ends the account. A
trailing drawdown ratchets on intraday peak equity including unrealized, or on
end-of-day balance only, which is the single largest difference between two
accounts advertising the same number. THE ACCOUNT DEFINES ITS OWN DAY: the reset
is a minute of the UTC day named by the policy, not the instrument's calendar,
and it fires whenever sim time crosses it.

A POLICY RESOLVES THE WAY A SYMBOL DOES: total, three steps, step three never
fails. Knobs stated inline win; otherwise a policy registered by name under
`[account_policies]` in the venue config, or one this build ships under that
name, with registered shadowing shipped; otherwise unpoliced. A name NOBODY has
is an error rather than a silent fall to unpoliced, because a run that believes
it is enforced and is not is the worst of the three outcomes. Registration is a
RUNTIME path, unlike instrument presets, which are compiled in: funded-account
programmes number in the hundreds and change their terms without notice, so the
shipped set is illustrative rather than authoritative.

A breach FLATTENS - cancel every resting order, then close every position as
reduce-only IOC market orders at the mark, judged against the configured
liquidation band - and then locks. A TERMINATING breach on a venue serving ONE
account ends the run, since its only account is dead and there is nothing left
to serve; on a shared exchange it does not, because one subagent breaching must
not take down the batch. Enforcement without the flatten would be a
report; the flatten is what makes a forward claim mean anything, because a
strategy that would have been liquidated actually is.

The thresholds, the ratcheted peak and the remaining budget are PUBLISHED on
`GET /account` for the EVALUATOR rather than for the strategy. mogwai presents
no dashboard, so a run that ended flat having spent most of its budget would be
indistinguishable from one that never came close.

THE ORDER-TYPE SURFACE IS COMPLETE RATHER THAN CURATED. Market, Limit,
StopMarket, StopLimit, TrailingStopMarket, MarketIfTouched, LimitIfTouched and
MarketToLimit are all served. The one nautilus type still refused is
`TrailingStopLimit`, whose trail rests as a limit where this venue's trail
resolves to a market close.

ORDER LISTS ARE A PRIMITIVE, not a workaround. A linkage is a GROUP ID plus a
RULE that each member carries - one-cancels-the-other, one-triggers-the-other,
one-updates-the-other - and the venue holds no list object, only what each order
says about the orders it names. THE RULE IS APPLIED WHERE THE FILL IS COMMITTED,
between sweep results and never after the batch: a tape span can cross both legs
of a bracket at once, so a sibling reaped on a later pass would already have
filled against the same prints. A CHILD (`parent_order_id`) rests HELD -
accepted, answerable, scanned by nothing and holding no reservation - until its
parent's first fill releases it into the state it would have been submitted
into, drawing a fresh band trigger and taking its hold then. A parent that goes
terminal without filling reaps its held children in the same batch.

THE DEPTH RULE - a child may not itself be a parent, and a parent carries at
most `MAX_LINKED_ORDERS` children - is what keeps a cancel's byte reservation
computable: reaping is one generation, so `sizing::LINKAGE_MAX_BYTES` bounds it
in advance. See `docs/order-lists.md` for the consumer-facing rules.

A STOP and a TOUCHED order are the same machinery with opposite comparisons. A
stop protects - buy above the market, sell below - and fires when price runs
AWAY. A touched order enters - buy below, sell above - and fires when price comes
TOWARD it. Both fire on TOUCH rather than through, because a conditional holds no
queue position. The two predicates are separate functions and separate
`ScanKind`s rather than one function with a flag, since they are the two most
easily confused behaviours in the venue.

A TRAILING STOP's trigger ratchets with the tape and never retreats: a sell trail
rises with the high and stays put when price falls back. It is advanced against
the SPAN'S EXTREME rather than its closing mark, so a spike between two sweep
passes drags it exactly as a tick-resolution venue would.

TICK RESOLUTION WITHOUT PER-TICK EVALUATION, and this is one mechanism serving
both the trail and the risk policy. The tape thread records the HIGH and the LOW
its river reached since the sweeper last looked, with the instant of each; the
sweeper takes that span once per pass. Two properties make it exact rather than
approximate. A trail is a MONOTONE function of the tape, so the maximum over a
span's ticks IS the span's high. Equity is LINEAR in the price of the one
instrument an account can hold - an account is on at most one river, strategies
are single-instrument - so its extreme over the span sits at a price extreme.
The policy therefore observes the two extremes IN THE ORDER THE TAPE REACHED
THEM and then the close: a spike that opened and closed between passes spends
drawdown budget, and a collapse that recovered before the pass breaches. Order
matters and is not a detail - replaying favourable-first would invent breaches
that never happened. The tape thread's cost is two comparisons and one relaxed
load per tick, and it publishes only when an extreme actually moves.

TIME-IN-FORCE covers Gtc, Ioc, Fok, Day and Gtd. A conditional may be Day or Gtd
- both can wait for a trigger - but never Ioc or Fok, which cannot wait for
anything. Expiry is a TIME-DRIVEN pass with nothing to do with triggers: a Gtd
order stops resting at its instant whether or not the tape came near it, and a
Day order stops when its own instrument's SESSION CLOSES, which the sweeper
detects by asking the calendar whether the span it swept crossed from open to
shut. An instrument with no calendar supplies no such instant, so a day order on
a 24/7 symbol rests like a Gtc - the honest answer, since inventing midnight UTC
would expire orders at a time that market has never heard of.

THE LEDGER MODELS FIVE INSTRUMENT CLASSES, split by SETTLEMENT SHAPE rather
than by asset class, because the shape is what decides how holding one moves the
ledger.

- `Spot` credits the BASE ASSET as a currency balance. Right for crypto spot,
  where the base genuinely is money you can spend on the next pair.
- `Equity` credits a POSITION and never a balance. A share is not money, and
  modelling it as `Spot { base: "AAPL" }` put it on the same footing as USD -
  which is what made short sales, settlement periods and round lots
  inexpressible. Cash moves by the full notional on both sides; the shares are
  the position. All three conventions are now expressed on the class, and the
  next paragraph is what they mean.
- `Future` moves only settlement cash, with exposure carried as a marked
  position.
- `Perpetual` is a future that pays FUNDING between long and short at an
  interval. With no expiry to converge at, funding is the only thing tying it to
  spot, so a perpetual without it reports P and L that is wrong by construction.
  Funding is paid on notional AT THE MARK, on instants that sit on multiples of
  the interval from the unix epoch - a property of the clock, so the schedule
  cannot depend on when a run booted or how the sweep passes were cut.
- `Inverse` is coin-margined: quoted in one currency, settled in another. Value
  is `multiplier * qty / price` rather than `multiplier * qty * price`, so P and
  L is non-linear and a long is not the mirror of a short. `InstrumentDef`
  carries the one implementation of both forms, so realized and unrealized can
  never disagree.

AN EQUITY IS A CASH ACCOUNT OR A MARGIN ACCOUNT, and the margin policy is which.
That distinction is what decides what an equity account may DO, and it is
enforced rather than reported:

- A CASH account (no margin policy on the symbol) pays the whole notional on a
  buy and MAY NOT SELL SHORT at any price. The refusal names the reason rather
  than reading as a funding shortfall, because shorting is not something a
  larger balance would buy.
- A MARGIN account posts the Reg-T requirement - `basis = "notional"`, `initial
  = 0.5`, `maintenance = 0.25` - and BORROWS the rest, so the settlement balance
  goes negative by the loan while the shares sit on the other side of it. The
  account is worth what it was; `valuation_in` counts an equity at its MARKET
  VALUE rather than its unrealized, because the cash already moved by the whole
  notional. The maintenance walk measures the same way, which is what makes a
  margin call reachable at all.

THE LOCATE is `borrowable`: absent means the venue models no borrow market, `0`
states a name nobody will lend, and any other value caps the account's net short.
THE SETTLEMENT PERIOD is `settlement_ns`: a sale's proceeds are credited at once
and held unspendable until the span has run, appearing as `locked` on the balance
row - which is what a `T+N` convention actually is to a strategy. It is a fixed
sim span rather than N sessions, and that simplification is stated rather than
hidden. THE ROUND LOT is `lot_size`, and it governs what may be SUBMITTED rather
than what the size grid can represent: a partial fill legitimately leaves an
odd-lot remainder, so `size_increment` stays at one share.

MARGIN HAS TWO BASES. `per_contract` is a fixed amount of settlement currency
however the price moves, which is what CME publishes and what every shipped
preset states. `notional` is a fraction of notional, so the requirement moves
with the price - that is what forex, crypto margin and Reg-T equity margin
actually do, and it is the leveraged account the venue previously had no way to
express: ten-times leverage is `initial = 0.1`. THE MAINTENANCE WALK ASKS THE
POLICY rather than multiplying `maintenance_per_contract` by a contract count,
which read a notional-basis fraction as a per-contract amount and left a
leveraged account unable to breach at any price.

FUNDING IS CHECKED PER ACCOUNT AT BIND. The venue's `[balances]` is only what an
unnamed account opens with, so a client that named its own funding cannot be
checked at boot. It is still knowable with no order at all, so a socket binding
a symbol its account holds no balance line for is refused before the upgrade,
naming the account and the currency. PRESENCE, never sufficiency: running out is
DEPLETION, and a funds rejection on a served shape has to keep meaning that and
only that.

A POLICY NAMES THE CURRENCY its thresholds are stated in, and the account is
VALUED in it. A spot fill credits the base asset as a currency balance and
debits the quote, so an account trading spot holds an asset that has to be
priced before its equity means anything. The engine keeps a last mark per
symbol for every class, the sweeper prices every pair whose base the account
holds, and `Engine::valuation_in` sums that currency's balance, each other
balance valued through an instrument quoting it in that currency, and the
unrealized on futures settling in it. An order whose shape would leave a holding
nothing prices is refused at entry by name, and an account that reaches an
unvaluable state some other way is warned about and left unenforced rather than
judged against a wrong number.

Valuation is ONE HOP: an asset is priced only through an instrument quoting it
directly in the policy currency, never through a chain. There is no rate
surface.

The policy is evaluated at TICK RESOLUTION, through the span of extremes the
tape thread records rather than through a per-tick evaluation. See the trailing
stop above for the mechanism and why it is exact: a spike lasting a fraction of a
sweep interval spends drawdown budget, and a collapse that recovers before the
pass still breaches.

WHAT THE SPAN DOES NOT COVER, stated because it is the one place the exactness
argument stops: an account holding MORE THAN ONE marked symbol. Equity is then a
sum of linear terms whose extremes need not coincide, and only the swept river's
symbol carries a span - every other symbol contributes at its last mark, which is
the mark-cadence behaviour. That costs nothing under the model the venue enforces
(an account is on at most one river) and is a bound to state rather than a defect
to hide.

Symbol resolution is total over wire-legal labels. Configured profiles are
held directly and other profiles are memoized without a cap. The permanent,
expensive checkpoint chains are capped instead: creation of the 257th river is
refused atomically, with no eviction. A `RiverKey` includes the exact requested
label, its per-label tape seed, and the resolved bundle digest, so two labels
wearing the same default shape still own independent water.

`/instruments` therefore answers the union of the configured shapes and every
shape this run has MATERIALIZED a river for - materialized, not merely
resolved, because resolution is total and a memo-shaped list would advertise
labels nothing had registered. A socket bind and a history poll spend the same
river budget, so the advertised set grows exactly when the capped resource
does. The ENGINE's instrument set grows on the same demand: `Run::ensure_instrument`
registers a def and installs its margin policy and fee schedule the first time
a socket binds that symbol or an order names it, guarded on the registration
having been new so re-binding never resets a live configuration.

The server exposes `/health`, `/account`, `/accounts`, `/instruments`, `/clock`,
`/trades`, `/quotes`, `/control/divergence`, and `/ws`. `POST /accounts` opens
an account on terms the client states - an id, its opening balances, and
optionally the risk policy the venue enforces against it - and is OPTIONAL:
account resolution is total, so a connection that never calls it is served under
the default account, unpoliced. A policy the venue cannot enforce is refused
where it enters rather than hours later. Structured account config goes over HTTP for
the same reason a divergence does, and only the id crosses the socket upgrade.
Re-opening an account that already exists is a `409` rather than a reset,
because an account outlives its connections and the request cannot be told
apart from a reconnecting client re-sending its config.
`GET /account` names whose ledger with `?account=`, defaulting the same way.
Order entry is WebSocket-only: the
`POST /orders` carrier went with the HTTP transport profiles. Each socket feeds
one bounded sequential dispatcher, so admitted commands reach the market read
and engine in socket arrival order even when their modeled act latencies differ.
The queue and a process-wide permit bound parsed command work before it reaches
the blocking pool or engine mutex, and a full bound is a visible
`AdmissionRejected` the engine never sees. Inbound frames and reassembled
messages are capped at `MAX_CLIENT_MESSAGE_BYTES`, 64 KiB, so a dependency
default no longer sets the venue's memory bound; an oversized frame ends the
connection. A WebSocket carries its whole binding in the upgrade query string,
which `deny_unknown_fields` rejects any other key on: the optional, case-exact
`symbol` names its one river, the optional `speed` names the pacing multiple,
the optional `duration_ms` names a passenger-local simulated deadline, and the
optional `account` names the ledger it trades.
Absent, they default to the run's boot symbol and the configured `speed`, and
to an indefinite passenger. The key is known before any tasks or bytes exist,
a refusal - an illegal label, a shape that does not validate, a funding-barred
one, an exhausted river cap, a non-finite or negative speed, or a speed
differing from the one this river's sitting boat carries - is an HTTP 400
rather than an
ambiguous WebSocket close, and one connection still owns exactly one replay. A
frame carrier would permit multiple replays and create an unbound interval
before the first frame. The query carrier is the seam where river-keyed state,
boat placement, and per-boat clocks attach. `ws_upgrade` resolves the query
symbol, registers its instrument on the engine, resolves its `RiverKey`, and
boards a boat on that river, all before the 101; `handle_socket` then owns the
already-bound session. Every resolved shape owns a lazily created checkpoint
chain, keyed and locked independently, and is servable through history. Clients
do not send subscribe frames or an account identity. The bounded fanout
ring remains; a lagging client receives
`FeedLagged` on the priority lane and is closed with WS 1011.

A river's tape root is derived from the run seed and the REQUESTED symbol
label, not from the shape the label resolves to, so a run serving several
symbols serves several genuinely different tapes. A run stays a pure function
of `(seed, config)`; a river is a pure function of `(seed, label, resolved
bundle)`. The seeding rules are set out with the run seed below.

Execution output that no command asked for reaches every open socket. The
venue serves four order types - `Market`, `Limit`, `StopMarket`, `StopLimit` -
and a resting order is one of three explicit states: a live limit, an
untriggered conditional, or an inert market remainder left by a partial fill
that is never scanned again and ends only on cancel. Every resting limit
carries a trigger price drawn once at submit from a seeded, volatility-scaled
band around its stated price (`fill_band_vol_mult = 0.0` degenerates to a
strict through-at-the-stated-price fill), and it fills only when the run's
fill sweep walks a print STRICTLY THROUGH that trigger; the fill is delivered
to each connection's lanes from the run, not from a command response. A
market order slips the same way, adverse to its side, off the same seeded
band. The trailing-volatility reading is cached once per symbol and fill-sweep
interval, using the interval's simulated-time floor, so a command burst shares
one coherent coarse band instead of repeating the full 300-second synthesis.
Sweep marks and settlement prices are separate exact-instant last-print reads;
the coarse band cache never supplies unrealized P&L.
A conditional (`StopMarket`/`StopLimit`) rests untriggered until the
same sweep walks a print that TOUCHES its stop price - the mirror-image
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
A passenger owns an uncloneable ticket for one websocket connection. The first
passenger places the boat at the river's fixed warmup origin; later passengers
with the same speed join it mid-stream. Speed is quantized to micro-multiples
in the sharing key. Duration is passenger-local and is therefore not in that
key. At most one boat sits on a river, and a differing speed is refused loudly.
The last ticket removes the seat, cancels the worker and joins it away from the
registry mutex. Rivers and their bounded checkpoint sets remain for process
life so later history does not depend on eviction timing.

The BOOT river is the exception to placement on demand: `serve` boards it
before it writes the readiness line, at the configured `speed`, and the run
retains that ticket for process life. So the boot river always has a boat, it
never winds down, and a socket asking for a different speed on the boot symbol
is refused. Every other river is boatless until someone binds it.

Concurrent first boarders share one placement through a semaphore handoff
rather than each placing a boat, and a reader asking a river's now while a
placement is in flight WAITS for it instead of falling through to the venue
clock - falling through would hand back a well-formed answer off a clock ahead
of the boat about to be seated, which is the look-ahead per-boat clocks exist
to remove. `/health` keeps the non-blocking form, because it must never block
on a placement.

`/health`'s tape fault reads EVERY seated river on those same non-blocking
terms, not the boot river alone. It read only the boot river until 2026-08-16,
which was right when a run had one paced tape and became a hole under the open
instrument set: a client bound to any other river got a healthy answer while
its own tape was stuck, and the boot river is the one a strategy under test is
least likely to have bound. One optional object over N boats forces a choice,
and it is the faulted river with the smallest symbol - deterministic across
polls, unchanged in wire shape, and enough to answer whether any river faulted.
`docs/cli.md` states what a poller should do with it.

The venue also retains a wall-to-sim reference, but it is not a seated boat's
clock. It bounds history for a boatless river, drives the venue deadline, and
stamps the venue-scoped pulled account ledger. A seated river instead answers
only through the instant its own boat has published.

The ledger stays venue-scoped because one engine serves every river, so a
pulled `/account` snapshot has no boat axis to sit on: stamping it from any one
boat makes it ahead of or behind a push from another. `GET /account` therefore
keeps the venue stamp and LABELS it, adding a `clock: "venue"` field beside the
otherwise unchanged `AccountState` so a consumer can never mistake that
`ts_event` for boat time; pushes are ordered against pulls by sequence.
`/clock` answers the same way - `?symbol=` renders that river's boat clock and
sets `boat_clock`, and without a symbol it renders the venue's.

Each boat has its own settlement watermark and its own ring. Market water is
exogenous: orders never move it and there is no queue competition. Fifty agents
submitting the same buy against the same water receive the same fill without
changing one another's result. Generator-level havoc belongs to river identity
and cannot mutate a seated boat; transport havoc remains a property of what a
passenger sees. So `FlowSurge` and the generator half of `ClearDivergences` are
refused with a 400 on a river that has a seated boat, and unqualified
`FlowSurge` is refused outright while any boat sits, naming the seated symbols;
an unqualified clear reaches every materialized river that is boatless and
SKIPS the seated ones, because the transport half of that control is run-wide
and must stay reachable while a boat is sitting. A timed havoc window carries a
wall arming instant and a
simulated span rather than one boat's absolute deadline, and every passenger
judges it on its own clock.

The fill sweeper is ONE task on an earliest-deadline schedule over the seated
boats, keyed by boat identity rather than by allocation address, each boat
re-armed on its own clock and floored in wall time so an accelerated run cannot
turn the pass into a hot loop. The consequence to know: a river with no seated
boat is not swept, because a sweep needs a clock to sample, so resting orders
on a wound-down river stay unscanned until someone boards again.

An instrument is a bundle of knobs, not one fixed shape. Two classes are
selectable: a spot currency pair, and a cash-settled continuous future with a
contract multiplier, whole-contract quantities on the order path AND on the
tape, and no expiry or roll. The generator's size grid is multiplier-aware -
notional per unit is `multiplier * price`, and a contract draw is rounded half
away from zero and floored at one contract, so no print becomes the zero
quantity nautilus drops. `latent_size_median` is stated directly in the
instrument's native size unit and names the continuous lognormal center before
that grid is applied. The floor truncates its lower tail, so it is deliberately
not called the observed size median. `TAPE_PROTOCOL_VERSION` is 19; version 5
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
been fitted at protocol 8 as a PER-MINUTE quantity from NQ one-minute bars but
is applied PER PARENT EVENT, and minute-scale volatility carries the per-parent
scale times the square root of the arrivals in that minute - so the fitted 3.4x
hourly swing compounded with the 27.5x arrival swing and left the generated
Asia and London sessions roughly five times too quiet at bar scale. Both
session arrays were refitted from the July MNQ TBBO corpus as one atomic group:
`intensity_hour` from inferred-parent counts conditional on the frozen
`dow_weight`, landing 14.5x peak-to-trough against the volume proxy's 27.51x
upper bound, and `vol_hour` as a per-parent robust scale, which comes out
nearly flat and slightly INVERTED - overnight parents individually move a
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
the band a function of how the client spelled its price - `100` and `100.00`
drew different triggers and different slippage for the same order. Every
seeded fill trigger and market-slippage offset therefore moves at 13. Version
14 makes a scheduled calendar jump part of the `ReopenGap` crossing frontier,
so an arm inside a closure cannot be skipped forever. The protocol-12b
MECHANISM landing, long pencilled in for 13, now takes 15. Version 16 retired
the second unnamed default knob bundle without moving generated bytes. Version
17 keys each server river's tape root by the requested symbol label, moving
every server tape while leaving offline generation seeds untouched. Version 18
is the boatyard landing: placement, pacing and the per-boat clock moved off the
one venue-wide replay, so what a socket receives is a function of its boat
rather than of the run.

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

That snapshot is CONDITIONAL, and a consumer must not read it as a
snapshot-first wire contract. `Tape::subscribe_with_snapshot` hands back an
option: the boat retains the last quote it PUBLISHED, so a socket binding in
the window between a boat's first trade and its first quote is handed no
snapshot and sees that trade as its first market frame. Nothing is hidden by
the absence - the snapshot is missing only when no quote has been published on
that river yet, and the tape's own first quote follows immediately, so there is
no case where a bound socket holds a stale BBO or none at all for long. A test
or a consumer that requires the FIRST frame to be a quote is asserting
something stronger than the venue promises, and it will lose that bet at boot;
drain to a deadline instead. `scripts/smoke.py` did exactly that and flaked
twice in one day before it was corrected.

The volatility innovation is standardized to unit variance before it reaches
that recursion. The `a0` derivation has always assumed this, but the innovation
was a raw Student-t whose variance is `df / (df - 2)`, so the true condition was
`a1 * E[z^2] + b1 = 1.115` and the process had no finite stationary variance: it
stayed bounded only by its own rails, ran 8.17x hotter than `vol_scalar`
claimed, and sat pinned at the variance cap 12.96 percent of the time. `a1`,
`b1` and `vol_scalar` were re-solved against the corrected condition.

Three rails are named separately because they answer to different things. The
GARCH state cap and the feedback-return ceiling bound the base process and are
NEVER scaled by a regime, so an armed divergence cannot raise the process's own
ceiling and change what it does after the divergence ends. The realized-return
ceiling is absolute and applies after session and regime scaling, so a
divergence is an output envelope. That ceiling is a stated product policy sized
against a measured maximum-strength envelope, not a fitted market quantity: as a
log return it permits about +5.13 and -4.88 percent in a SINGLE event, and it
does not bound cumulative movement over many events.

Two structural fidelity limits of the generated futures tape are stated here
because no parameter can remove them. First, the calendar-driven baseline has
no automatic reopen jump: a real session reopen prints a discrete gap where
the closed-hours information arrives at once, while the generated mid resumes
its random walk from where it halted. An explicitly armed `ReopenGap`
divergence can inject such a jump on a subscription's view, but the clean
baseline tape never produces one, so on it overnight gaps are absent and any
large single-minute range the tape does produce is a volatility-cluster tail
inside a session, not a reopen - a different phenomenology occurring at a
different time of day. Second, the session profile modulates intensity and
volatility by HOURLY factors, so within-hour structure (the opening minutes'
concentration at the cash open, the settlement flurry) is smeared uniformly
across each hour; the profile reproduces hour-scale contour, not
minute-scale texture.

Fingerprint ranges are corpus-labelled observations. They select defaults and
produce operator diagnostics, but never admit or reject an instrument. A
shipped preset must either produce no diagnostic or accept its stable code on
the matching provenance entry; exact-set validation also rejects stale
acceptances. Hard
generator validation is mechanism-derived instead. In particular, the latent
size center must not sit two orders of magnitude below the minimum tradable
quantity, volatility must sit below the GARCH sigma cap so the process is not
born clipped, and the tick must be representable at the declared precision.
Rail HEADROOM is diagnosed rather than gated: a universal ratio of scale to rail
would repeat the same mistake in another dimension, denying a legitimately
higher-volatility instrument. Return ceilings remain shared module-level process
shape: a coarse truthful grid is allowed to produce a stickier latent mid rather
than receiving an uncalibrated stress-tail lift.
The dimensionless `tick_return / vol_scalar` and its squared random-walk crossing
estimate are exposed as diagnostics for deriving event-price repetition; they
do not pretend to model sweep stepping, bounce, recentering, or explicit repeat
draws by themselves.

A future's ledger is single-currency and collateralized. There is no base leg:
a fill moves the position and the VWAP, a quantity-reducing fill books
`(fill - avg) * closed * multiplier` of realized P&L straight into the
settlement balance, and margin - `maintenance_per_contract` per open contract
plus `initial_per_contract` per resting non-reduce-only contract - is what the
account locks, reported per symbol as posted margin the adapter forwards as
nautilus `MarginBalance` rows. Reduce-only orders reserve nothing, which is
what makes two bracket legs against one position exclusive rather than
additive. The sweep pass marks every open futures position to the tape, strikes
every settlement instant the calendar crossed AT ITS OWN INSTANT rather than at
the sweep boundary, and emits exactly one account snapshot per pass, after the
mark, so no consumer sees a stale `mark_px`. Settlement moves the accumulated
difference into actual cash and resets the VWAP to the settlement price, which
is why a losing futures position drains an account rather than merely carrying
a worse unrealized number. A breach is `total_balance + unrealized <
maintenance` - the TOTAL balance, because the locked amount already IS the
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
is charged the configured schedule but never a client-armed `FeeSurcharge`.

Margin and fees are treated here as instrument identity, which is not how
markets work: real schedules vary by account tier and real CME margin varies by
product, volatility, portfolio and time. A fixed per-contract margin and a
fixed schedule are declared simplifications of the venue's model, not
descriptions of the market's.

A run draws one 64-bit seed at launch, or takes it from config, and every
random stream in the run - each river's tape generator and the fill band -
derives from it by domain-separated derivation; nothing else in a run is
random. The fill root is run-level, because the band's draw key already carries
the order's symbol; a tape root is per-river, keyed by the REQUESTED symbol
label as described above, so two labels resolving to the same shape never share
a path. The tape's origin is the fixed constant `TAPE_ORIGIN_NS = 0`; the boot river's
warmup is generated before readiness, every other river's on first read, and the
run proper begins at `run_start_ns = TAPE_ORIGIN_NS + warmup_ns` on the same
axis, so `data_origin_ns` is always `TAPE_ORIGIN_NS` and history outside
`[data_origin_ns, sim_now]` is refused. The venue has one tape origin, one
placement origin and one warmup span, but N rivers, each carrying at most one
boat with its own wall anchor and speed. This is why the readiness record
carries those three time facts and no symbol. A run is therefore a
pure function of `(seed, config)` for a given build and fingerprint - with the
limit that a new seed only draws a new path from the one fitted model behind
the fingerprint, so marginalizing over seeds reduces variance conditional on
that model rather than adding out-of-sample market evidence.

Two durations exist and they are not the same object. The RUN duration is
configured, starts at `run_start_ns` rather than boot, and is measured on the
venue clock; at its deadline the server announces `RunComplete`, closes
WebSockets normally, drains, and exits zero. A PASSENGER duration is the
socket's own `duration_ms`, simulated milliseconds measured on its boat's clock
from its boarding instant, so passengers with different durations still share
one boat and each closes at its own deadline while the boat winds down only
when the last of them leaves. The venue's completion instant is the SIGNAL that
crosses to a socket; the numbers on the `RunComplete` frame are always
re-derived on that socket's boat clock, and `elapsed_ns` is how much tape that
boat actually covered.

The history endpoints refuse rather than return an empty page on every
impossible request, so a refusal is never mistaken for a span nothing traded
in. A START before the tape origin or past the ceiling is a 400; so is a
shape-class refusal decided before the synthesis task runs - an illegal label,
a shape that does not validate, a funding-barred one, or an exhausted river
cap. A synthesis failure is a 500 naming the symbol and the window. There is no
"symbol this run does not serve" axis any more: resolution is total, and a poll
MATERIALIZES the river it names, which is also what makes it advertise through
`/instruments`.

The ceiling is the NAMED RIVER's now - what its boat has published - and only a
boatless river answers with venue sim-now. An `end` past that ceiling is
CLAMPED rather than refused, deliberately asymmetric with the start: a boat
placed `T` wall-nanoseconds after boot sits `T * speed` simulated nanoseconds
behind the venue clock by construction, so a client stamping its `end` from
`/clock` with no `?symbol=` is routinely ahead of this answer, and refusing it
would fail every honest warmup fetch. A client that needs to know where the
tail actually is reads `/clock?symbol=`.

The protocol crate owns every JSON type shared by server and adapter. The
adapter uses WebSocket streaming only for market data and execution; `/trades`
and `/quotes` remain request endpoints, which is how history and warmup are
fetched. Each adapter client names its river with an optional `symbol` in its
own config, which becomes `/ws?symbol=`; it carries no `speed` or
`duration_ms`, so the data and execution clients of one host board the same
boat at the venue's configured speed. The adapter holds NO served-symbol guard
of its own any more: since resolution became total there is no set to guard
against, so both clients re-read `/instruments` AFTER binding - binding is what
registers an unconfigured symbol, so only a read taken after the socket is up
can see it - behind a watch-gated readiness barrier that black-holes delivery
until the reseed says go, and a failed reseed tears the connection down rather
than wedging the delivery pump.

The generated tape publishes BBO updates and raw fills, not aggregated trades. One parent
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

Reading the market at a submit is correspondingly more expensive: the walk costs
about 9.8 ms as of 2026-08-14 (median over 100 distinct buckets, release, host
`bygg`, measured by the ignored `read_market_latency_stays_within_submit_budget`
instrument, versus the 12.6 ms recorded before the checkpoint stride was
repaired). The residual replay is now a small part of that: the 300 s
volatility window is the walk, which is why cutting checkpoint positioning by
53x moved this number by only a few milliseconds. So acceptance-time readings
are memoized ON THE BOAT - one memo per boat, bucketed by fill-sweep interval
on that boat's own clock - and a submit sees a reading that may be up to one
interval stale. A market order therefore fills at or beyond the market as of
that reading, not as of the fill instant. The memo lives on the boat rather
than on the run because the bucket is a function of the boat's clock and the
walk it saves is a walk of one river: a run-level memo held a single entry, so
two symbols evicted each other into a guaranteed miss and then serialized on
the walk behind one mutex. The lock is held ACROSS the walk deliberately, so
two passengers landing in the same bucket pay for one walk rather than two.

The exact-instant mark and settlement reads that never come from this memo fail
differently from each other on purpose:
an unreadable ordinary mark costs one pass of unrealized P&L freshness and is
dropped, while an unreadable SETTLEMENT price refuses the whole read and leaves
the watermark where it stands, because nothing looks back past a watermark that
has moved.

## The workspace and the offline evidence toolbox

Seven crates. `mogwai-protocol` owns the wire types and the shipped launcher and
imports nothing else in the workspace. `mogwai-engine` is the venue-agnostic
exchange core. `mogwai-data` owns `TickSource`, the k-way merge and the
`GeneratedSource` synthetic generator fitted to the committed fingerprint.
`mogwai-server` is a library - it owns the sockets, the clock and the replay
pacing, and ships no binary of its own. `mogwai-cli` is the `mogwai` BINARY: a
clap dispatcher over `serve` (which does no work itself, just forwards to
`mogwai_server::serve`) plus every offline subcommand. `serve` is the only one
that binds a socket; the rest are the intake and measurement surface - `gen`,
`tick-composition`, `presets`, `man`, `preflight`, `measure`, `fit`, `cache`,
`synth`, `cadence-feasible`, `characterize`, `select-windows`,
`session-profile`, and the protocol-12 instruments (`count-curve`, `stage-m`,
`minute-range-envelope`, `arrival-control`, `arrival-screen`,
`arrival-envelope-diagnostic`, `tick-composition-ratios`). `mogwai --help` is
the authority on the current set. `mogwai-adapter` is
the lone nautilus-dependent crate, unchanged by anything below.

`mogwai-lab` is the fifth non-adapter crate: the corpus-to-fingerprint method
library the 2026-08 Python-to-Rust rewrite absorbed from `analysis/` (the
rewrite program's phase records and per-script scope rulings are retired to
git history) - streaming TBBO/Binance-trades
parsing, the protocol-12a measurement engine, aggregation and bootstrap,
fingerprint and cadence synthesis, and the protocol-11 session-calibration
fit. Its dependency direction is one-way and asymmetric: `mogwai-lab` depends
on `mogwai-data`, `mogwai-protocol` AND `mogwai-server` (session-summary work
needs to resolve an `InstrumentProfile` through `Config::load` exactly as the
Python's `--config` scratch walks did), but `mogwai-server` depends on none of
it - there is no cycle, and `mogwai-lab` stays out of the tape-generation path
`TAPE_PROTOCOL_VERSION` scopes, the same reason `measure12a.rs` was
consumer-only inside `mogwai-server` before the rewrite moved it. `mogwai-cli`
depends on `mogwai-lab` for the pieces that need no server preset resolution
(preflight, cache, most of measure/fit/synth) and calls straight into
`mogwai-server` for the generated side of measurement.

THE INSTRUMENT SET IS OPEN, and that is why `mogwai-lab` is a library rather
than a folder of scripts. A symbol is a request string, never an admission
identity. `InstrumentDef` is derived through one path from the symbol and the
operator overlay: an explicit preset, a matching preset, or the BTCUSDT default
bundle. No second hardcoded default bundle exists, and no symbol is refused for
wanting a fit. The three shipped presets - MNQ, MES and BTCUSDT - are the
current state, not the end state.

Config declares no closed instrument set. It supplies a default knob overlay
and optional case-insensitive per-symbol overlays for total symbol resolution.
The top-level boot symbol selects the eagerly warmed boot river and remains the
default for a request that carries no symbol; other request symbols materialize
and board their own rivers in the same run.

The intake sequence therefore makes a tape better and gates nothing:
survey what cheap data exists, decide whether a paid corpus is worth buying
and which windows of it, buy, preflight, measure, characterize, fit, ship a
preset with its provenance. The offline toolbox is that sequence made
reusable, and the two consequences bind anything built on it. A component is
SPENT only when its QUESTION cannot recur, never merely because the MNQ pass
answered it - an archive inspector or a corpus driver is idle between
instruments, not dead. And per-instrument knowledge belongs in config or a
preset rather than a hardcoded list in the method: a preset tuple naming
today's three symbols is a defect the fourth exposes. The corollary for
evidence is that a finding measured on one instrument is one observation, not
a law, until a second instrument either reproduces it or does not - which is
why methods a preregistered test rejected are kept runnable rather than
deleted.

THE SECOND CONSEQUENCE IS THE DIRECTION OF TRAVEL, NOT A MET INVARIANT, and
stating it as met would be false. The offline toolbox still fixes
per-instrument choices in source, faithfully mirroring the Python it was
ported from rather than introducing the debt: `cadence.rs` fixes the pair set
and the archive month and takes BTCUSDT as anchor, `fingerprint.rs` takes
XBTUSD as anchor, and both `session_profile.rs` entry points resolve the MNQ
preset. None of these is reachable as an input. Retiring the Python removes no
parameterization that exists today - it was equally hardcoded - so closing
this is forward work rather than a porting debt, and it is what a second
instrument will force.

THE PARITY CONTRACT a port is held to, stated once because every case
otherwise gets argued from scratch: for every VALID input, the Rust must
either produce output equivalent to the implementation it replaces or embody
an explicitly approved semantic change. It MAY additionally reject inputs
outside the declared input contract. It may NOT silently accept malformed
input, and it may NOT silently change results for valid input.

The line that follows from it, and the reason it is worth writing down: a
gate passing on the committed fixtures is evidence about those fixtures, not
proof of equivalence over the contract. So a Rust refusal where the original
proceeded is a loud narrowing and needs only to be recorded; a Rust result
that differs on some valid input the fixtures happen not to contain is a
silent divergence and must be fixed or approved; and a Rust default where the
original raised is silent acceptance of malformed input, the worst of the
three, because it manufactures an answer. Fixing the third class by making
the committed artifact pass again is not a fix - the repair needs a fixture
chosen to DISTINGUISH the implementations, or the blind spot survives.

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
on-disk data apart, never mixed. ARTIFACTS (preflight, measurement and fit
outputs) are the user's files: written to `--out` or a subcommand's own
working-directory default, never cached, never auto-deleted. CACHE
(recomputable, keyed data - walk summaries, measure12a walk records) lives
under `$XDG_CACHE_HOME/mogwai/` (falling back to `~/.cache/mogwai/`),
overridable by `MOGWAI_CACHE_DIR` or `--cache-dir`, keyed by a
`ProvenanceToken` folding in the crate version, `TAPE_PROTOCOL_VERSION`, the
fingerprint hash, the full invoked command line, the measurement
sub-contract hash and (when built from a tree) the git sha; entries under a
stale token are unreachable by construction and pruned automatically on
write, with `mogwai cache stats`/`clean`/`clean --stale` covering the manual
case. SCRATCH (per-run temporaries) is a run-scoped directory under the
cache root, deleted on clean process exit and safe to leave behind on a
crash. Repo development pins `MOGWAI_CACHE_DIR` to the Python-era
`analysis/out` layout so the phase 1-3b parity gates read the caches those
scripts already produced; that pin is not the installed default.
