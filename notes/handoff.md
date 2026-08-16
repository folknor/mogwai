# Handoff: what to work on next

Written 2026-08-16 at the end of the passenger/ledger stretch, for whoever picks
this up next. `notes/todo.md` is still the standing record and carries the
reasoning behind every ruling; this file is only an ORDERED list of what is
actually next, with enough context to start each one without re-reading
everything.

Read `AGENTS.md` and `CLAUDE.md` first. The two that will bite you soonest: any
change to the tape generation path owes a `TAPE_PROTOCOL_VERSION` bump, and
every new regression test must be BITE-CHECKED by reverting the fix as a text
edit and watching the named assertion fail.

## What just landed, so you know what you are standing on

A venue serves RIVERS (shared deterministic tapes) to PASSENGERS (one connected
trader each, never shared). A passenger owns its account id, ledger, orders,
risk policy and transport havoc. Accounts are client-named, outlive their
connections, and resume on reconnect. The ledger models spot, equity, futures,
perpetuals and inverse contracts, with per-contract or notional margin. The
order-type surface is complete except order lists. Risk policies are enforced,
not reported.

`brokkr check` and `brokkr check --gate` are both green at the time of writing.

---

## 1. Order lists: OCO and OTO - LANDED 2026-08-16

Served. A linkage is a group id plus a rule each member carries
(`SubmitOrder.link`), applied where the fill is COMMITTED - between sweep
results, so two legs swept in one batch produce one fill and one cancel. `Oco`
reaps its siblings, `Ouo` shrinks them by the filled quantity, and a child
(`parent_order_id`) rests HELD - accepted, scanned by nothing, reserving
nothing - until its parent's first fill releases it. A parent that goes terminal
without filling reaps its held children. Depth is one generation, which is what
keeps a cancel's byte reservation computable.

`docs/order-lists.md` is the consumer-facing statement and
`reference/architecture.md` carries the why. The adapter implements
`submit_order_list`, which silently no-opped through the nautilus trait default
before, and `wire_order_type` now refuses only `TrailingStopLimit`.

## 2. Per-tick risk evaluation and trail ratcheting - LANDED 2026-08-16

Closed WITHOUT a per-tick evaluation, because neither quantity needs one. A
trail is monotone in the tape, so the maximum over a span's ticks IS the span's
high; equity is linear in the price of the one instrument an account can hold,
so its extreme over a span sits at a price extreme. The tape thread records only
the high and the low it reached, with the instant of each (`extremes.rs`), and
the sweeper replays those two readings IN THE ORDER THE TAPE REACHED THEM before
observing the close. Replaying favourable-first would have invented breaches
that never happened, which is why the instants are carried.

Cost on the hot path is one relaxed load and two `Decimal` comparisons per tick;
the shared slot is written only when an extreme moves.

## 3. The boatless-river sweep gap - LANDED 2026-08-16

Closed as "refuse to leave orders resting on a river nobody is reading", in two
halves that meet:

- An ATTACHED account's order on a symbol no cursor is walking is CANCELLED at
  the sweep pass. It could never fill or expire, and the client is there to be
  told.
- A FROZEN account is skipped wholesale and its book kept, because a socket is
  expected back for it - which is what the owed restart scenarios need.

## 4. Freeze and TTL for unattended accounts - LANDED 2026-08-16

The freeze is now by construction rather than by accident: `Passenger` carries
its attachment, the sweeper skips a frozen account entirely, and the behaviour
is tested and stated in `reference/architecture.md` and `docs/config.md`.

Resuming does two things a returning socket needs. Every surviving order is
RE-BASED onto the returning boat's clock - a cursor is placed at its river's
origin, so a frozen order's frontier sits in the new boat's future and would
have wedged it for as long as the previous session ran. And whatever the account
held off the newly joined river is retired, per the standing ruling.

`account_ttl_ms` collects an account nobody reclaims, in WALL time because a
frozen account has no simulated clock. Zero (never) is the default, and the
setting rides the readiness record - `ReadyRecord::VERSION` is 8.

## 5. Equity is a class with no equity conventions

`InstrumentClass::Equity` holds shares as a position and pays cash, which is the
part that was structurally wrong before. What it still has none of: a SETTLEMENT
PERIOD, a short-sale LOCATE or BORROW, and any round-lot rule beyond whole
shares.

A cash-versus-margin account distinction also does not exist, and it is what
decides what an equity account may even do. `MarginBasis::Notional` is the
mechanism; nothing wires Reg-T semantics onto it.

## 6. A perpetual's funding rate is a constant

Real venues compute funding from the mark-versus-index BASIS, so the rate
responds to how far the perp has drifted from spot - which is the feedback loop
that makes a perpetual track spot at all. Mogwai's is a configured constant, so
a perp drifts without correction and its funding never tells a strategy
anything.

This needs an index price beside the mark, which the tape does not currently
produce.

## 7. Presets for the new classes

Nothing has been FITTED for equity, perpetuals or inverse contracts, so a symbol
configured as one is served the DEFAULT TAPE wearing a different shape. The
intake sequence - corpus, measurement, fit, preset - is what makes a preset
honest, and it has never been run for any of them.

`AGENTS.md` is emphatic that this gates nothing: the venue serves any symbol
whether or not a fit exists. But a forward test on an unfitted equity preset is
a test against crypto-spot dynamics with a share's label on it, and that should
be stated wherever such a preset is offered.

## 8. Session-composable tapes: the segment-sampler track

THE LARGEST REMAINING PIECE OF THE GRAND DESIGN and the one everything about
tape realism waits on. The wyrd doctrine holds session structure to be the one
non-fractal thing bars do not normalize away, so a session-bound thesis
forward-tested against the wrong session class tests a different claim.

The envisioned preset set is about five, spanning 24/7 crypto, CME futures with
genuine closure, and cash-equity hours. The segment sampler is what makes
looping session footprints, which is also what makes the PHASE question below
answerable.

## 9. Phase and joinability (BLOCKED, deliberately)

Whether a passenger may join a river mid-tape, and who decides. The owner ruled
this cannot be settled until the tapes mature - see the river-and-passenger item
in `todo.md` for the phase-over-period analysis and the three candidate owners
of the decision.

INTERIM POSTURE: one river per passenger. Do not quietly implement sharing
before this is ruled. Sharing is an optimization; the passenger is the model.

## 10. Named windows and placement as a request parameter

The shared-exchange mode needs a request to name `[T1, T2]` rather than taking
whatever the river has reached. The reproducibility argument is the strong one:
with an explicit window a run is a pure function of `(seed, config, symbol,
start, end)` with no wall-clock input anywhere.

One constraint to design for up front: a strategy needs warmup BEFORE its
requested start, so `[T1, T2]` really asks for materialization from
`T1 - warmup_ns`, and that floor must sit at or above `TAPE_ORIGIN_NS`. Better a
named refusal than a short warmup nobody notices.

## 11. Designate the default tape preset, jointly with the default account

Still undecided between BTCUSD, BTCUSDT and BTCUSDT.P. It is now the SHAPE
CONTRACT for every unmatched symbol, and it is COUPLED to the default account
preset by currency: if the two disagree, the wholly unnamed request fails its
own connect-time funding check, which is the one path that must never fail.

`.P` is now expressible - perpetuals exist - but its funding rate would be a
constant, per item 6.

## 12. Tell broadarrow, and expect their build to break

Everything about the account surface changed and their build breaks by design:
they set no `account_type`, inherit `MOGWAI-001`, POST no account, and have no
handling for a run that ends by LIQUIDATION.

`notes/todo.md` carries their whole item list under "Consumer context". Several
entries there are now stale in their favour - trailing stops are served, the
full order-type surface is served, `RejectNextCancel` exists - and their three
unrun scenario files can now be written against a venue that can produce the
shapes they need.

## 13. `FeedLagged` has nowhere to go (CROSS-REPO)

Unchanged and still blocked upstream. Nautilus has no `DataEvent` variant
meaning "the stream you are aggregating has a hole", and the execution socket
cannot self-heal because the frame translator runs inside the reader's own loop
and the client is `!Send`.

The local mitigation is shipped: both translators log at ERROR in words a host
can alert on. What is needed is a nautilus change - a degradation signal on the
data side, or a client-initiated reconciliation request on the execution side.
Do not re-derive this; the analysis is in `todo.md`.

## 14. Valuation is one hop, with no rate surface

An asset is priced only through an instrument quoting it DIRECTLY in the policy
currency. Hold ETH under a USD policy with only ETHUSDT and BTCUSD listed and
nothing prices it, so the account is unvaluable rather than valued through a
chain.

A rate surface fixes it and buys cross-currency accounts too. Nothing needs it
yet, which is why it was left.

## 15. Expiry reports `Canceled`, not `Expired`

Nautilus models expiry as its own transition and the wire has no such status, so
a `Day` or `Gtd` order that runs out reports `Canceled`. Nothing downstream acts
on the difference today, but it is a real divergence from what a host would see
at a real venue, and adding a status is a wire change that breaks every
consumer's match.

## 16. `next_position` unbounded accumulation

A single oversized order is rejected before the arithmetic, but `current.qty`
still accumulates across many individually-valid orders on one symbol and side,
so a long-lived engine can overflow the weighted-average computation. Closing it
means a position-size or notional cap, which is a design decision rather than a
local fix.

## 17. Restore discrimination to the fill golden's banded half

The five banded cells are byte-identical to the five unbanded ones, so the
banded half certifies that the band pipeline RUNS rather than that the band
BITES. A regression that silently zeroed the band would still pass.

Two knobs restore it, both costing runtime in a harness whose coverage was
deliberately cut for runtime: a finer `SWEEP_INTERVAL_NS`, or a tighter offset
ladder. A third option is to stop asking this artifact the question and assert
directly that a banded trigger differs from its stated price.

## 18. Re-scope the acceptance-time market reading

Miss median 9.8 ms inside a submit, against a 5 ms budget. The cadence landing
took the memoization lever and not the re-scoping lever, so the hit path is fine
and the miss path is 2x over. Lever one moves the estimator's identity and
re-blesses the fill golden.

Putting the reading instant on `OrderFilled` would separately buy back the
exactly-stated slippage contract, cheaply and independently.

## 19. The smaller standing items

Each is written up in `todo.md` with its evidence:

- The DEAD-FEED WATCHDOG: nothing positively proves a subscribed feed is alive
  rather than genuinely quiet. The default-tape dwell bound now supplies the
  threshold it needed.
- DUP/DROP HAVOC RESHAPING FABRICATED BARS: decide whether deriving bars from a
  corrupted trade feed models the right venue. Leaning accept-and-document.
- `AutoCorr` NUMERICAL STABILITY: the zero-variance guard misses a constant
  series at an irrational value. Not to be touched without cadence-impact
  analysis - its output is bit-exact against `analysis/cadence.json`.
- THE DWELL DEFINITION IS COMPUTED TWICE and the gate compares one against the
  other. Cheapest fix is one shared fixture, matching existing precedent.
- `tape_lateness_under_acceleration` is load-sensitive in release and nobody has
  found the machine property that predicts the failure.
- The VENUE-IDENTITY question: can a full session be established against a
  stranger holding a reused port? Needs a stub venue that speaks enough of the
  wire to complete a handshake.

## 20. Two process notes worth keeping

RUN THE IGNORED SOCKET SUITES after any change to the serving path. `brokkr
check` skips roughly thirty tests that bind loopback listeners, and this session
shipped a real regression through that gap - eviction on the default account
closed a client's own second socket - which only `brokkr test -p mogwai-cli
socket` surfaced. It was green at session start and red four commits later.

TWO TESTS REFUSE A DIRTY TREE by design (`arrival_control_refuses_a_tree_that_
changed_during_the_run` and the measure pins). They fail rather than skip, which
is indistinguishable from a real regression at a glance. Commit or stash before
reading their result.
