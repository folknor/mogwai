# Bug report: the actionable backlog

Extracted from `notes/todo.md` on 2026-08-24 for the bug-hunt orchestration loop.
Every entry below is work that can be done in this tree. Items owed by another
repository, or that are pure owner rulings with no code to write, are collected
in the appendix at the end and are out of scope for the loop.

## Mandatory reads, every agent, every round

Before touching anything in this document, read `reference/north-star.md` and
`reference/glossary.md`, then whichever other reference and docs pages the round
rides on.

Both of those two are exempt from the must-be-true rule and state the end state
rather than the present. Where either and the code disagree, the code owes the
change: the entry is not stale, the tree is behind. Correcting either to match
current behaviour is the one edit that is always wrong there, and only the owner
changes what they aim at. Glossary entries are admitted by the owner alone, so an
agent meeting an undefined load-bearing word escalates and never adds one.

Each finding carries the symptom, where it lives, and what closing it means.
Confidence and hedging from the source document is preserved verbatim in spirit:
where the source says a thing is unreproduced, unruled, or merely suspected, so
does this document. Verify before fixing.

Ordering inside a section is roughly by dependency, not by severity. Where an
entry cannot start until another lands, it says so under Blocked by.

## How much to trust an entry

Eleven of the entries transcribed into this document proved stale on first
contact with the code during the 2026-08-24 arc: already fixed, already gated, or
describing an API that had been retired. That is roughly a quarter of what was
transcribed, and it is the single most useful thing to know before starting.

So the first move on any finding is to read the code it names and decide whether
it still describes the tree. If it does not, correct the entry to what actually
remains and leave the code alone. Several entries have already been narrowed once
and say so; that does not mean they cannot be narrower again.

A full read-only verification pass ran on 2026-08-26 over sections B, C, D, E, F,
G and I, and this document is the corrected result. Two entries were deleted as
fully overtaken by landed work, nine were narrowed clause by clause, and the
count is again about a quarter - the decay rate is a property of the document,
not of one bad transcription, so assume it applies to whatever has been added
since. Two of the machinery bullets above were wrong in a way that mattered more
than any single entry, because a round is told it may build on them.

The pass did not cover H, J, K or L, which are owner rulings and prose debts with
no code to verify against. Those carry their original confidence.

Some entries carry an explicit instruction about how not to close them, because a
plausible-looking fix would undo something deliberate. Those are not suggestions.

## Machinery this document rests on

Six rounds of fixes landed on 2026-08-24. An agent working any finding may build
on these and must not break them:

- The incremental `order_holds` cache and a fresh fold must stay in exact
  agreement. `reconcile_order_holds` panics on any drift, but it is a debug-only
  check: it is called under `cfg!(debug_assertions)`, and the doc at the site
  says running it in release would reinstate a per-command cost on the funded
  venue. So the panic is how a drift is caught in development, and release
  safety rests on construction instead - `OpenBook`'s private storage and its
  three mutation paths. Do not reason about a release build as though the
  assert were standing behind it.
- The divergence request is a `kind` plus `args` shape that refuses unknown
  fields. A post-test script check named `control-plane-shapes` boots a real venue
  and posts a body for every divergence kind; if what the venue accepts changes,
  that check moves in the same commit, and so do the senders in `scripts/`.
- The closed-market guard asks the engine for a band-drawn marketability verdict
  over every closed-session member of a group in one pass. Do not split the
  calendar lookup from the engine query, and do not let the guard become more
  permissive than it is.
- The delivery-speed bound and its refusal wording live once, in
  `mogwai_protocol::control::validate_delivery_speed`, read by both the venue
  boatyard and the adapter validator. Follow that precedent rather than
  duplicating a bound across crates.
- The adapter retries a cadence conflict rather than treating it as terminal,
  because the venue names the seated speed but never who is seated at it. The
  refusal lifts when the last of the account's passengers leaves that river.
- One data client that *names* a river binds it and refuses any other
  subscription. That is the single-instrument strategy premise being enforced at
  the only layer that can see it, not a limitation to remove. The qualifier is
  load-bearing and was missing from this bullet until 2026-08-26:
  `subscribe_symbol` runs the check only under `if let Some(bound) =
  self.config.symbol.as_deref()`, and `MogwaiDataClientConfig::for_run`
  deliberately leaves `symbol` at `None`, so the config a host builds straight
  from a readiness record is exactly the one where nothing is enforced. Whether
  that is the intended hole or an unfiled finding is undecided.
- The adapter test stub mirrors the venue's real refusals. A stub that answers
  success to everything makes every test built on it confidently wrong, which is
  how one round shipped vacuous coverage.

## What went wrong last time

The cold review found a real correctness defect in every one of the six rounds,
and every one had already passed a full unscoped green gate. The recurring
shapes, worth checking your own work against:

- a value assigned where it should have been accumulated;
- a migration that left callers behind, in a layer no test imports;
- a restructured search that stopped visiting every element it used to;
- a conditional refusal treated as permanent;
- a rate applied to a signed quantity that wanted a magnitude;
- a test that passed only because its double accepted everything.

A green suite is not evidence that a fix is correct. Ask what each fix does to
its callers, and whether the invariant you are relying on is asserted anywhere at
all.

---

## B. Engine correctness

### B7. Account valuation residue

None of it blocking:

- One hop only. An asset is valued through an instrument quoting it directly in
  the policy currency, so holding ETH under a USD policy with only ETHUSDT and
  BTCUSD listed leaves the account unvaluable rather than valued through a chain.
  A rate surface would fix it and buys cross-currency accounts too.
- The mark is as stale as the last sweep, inherited from the margin ledger.
- The ledger-generality ruling still wants shares, leverage and funding payments;
  each needs a holding valued in a currency it is not denominated in, so this
  machinery is the part of that which now exists.

### B9. The order path refolds all holds whenever a margin-equity sell is involved

Filed 2026-08-24 from round 1. `rest_open`, `take_open` and `refresh_open_hold`
each trigger a full `rebuild_order_holds_excluding(None)` refold when a
margin-equity sell is in play, so those accounts went from O(1) to O(open
orders) on the order path. Correct, and the price of moving the cover
allocation into the aggregate, but it is a hot path.

Whatever is done here must not break what round 1 established: the incremental
`order_holds` cache and a fresh fold must stay in exact agreement, which
`reconcile_order_holds` enforces by panicking on drift under
`cfg!(debug_assertions)` and not in release.

Re-verified 2026-08-26, and the constraint is now wider than when filed. The
engine hunt's round-2 finding-7 fix made `margin_equity_sell_holds` deliberately
count a price-less resting sell for its quantity while contributing no price, so
the two folds cannot disagree even on an order no wire path can currently rest.
An O(1) incrementalization has to reproduce that, plus the exclusive-group
max-leg fold and the max-resting-price rule. All three `rebuild_order_holds_excluding(None)`
sites named above are unchanged.

---

## C. Venue and protocol

### C11. `RunComplete` reports slightly less than the declared duration

Nothing on the wire lets a consumer tell that from a short run. The deadline is
judged on the venue clock while `ws.rs` re-derives every announcement on the
receiving socket's boat clock, so the announcement trails by the placement gap
times `speed`. Both halves are deliberate and stated in `reference/clock.md`.
`RunComplete` still carries only `sim_now_ns` and `elapsed_ns`, and the variant's
own doc now states the consequence: `elapsed_ns` is the span that boat covered,
not the run's declared duration.

What is open is whether a consumer should be able to distinguish "the run served
its whole duration and my boat was placed late" from "the run was cut short":
shipping the boat's epoch, or the venue's own elapsed alongside the socket's,
would close it. A wire change nobody has asked for.

Narrowed 2026-08-26. This entry used to end "the same missing field as C10", and
C10 has since been closed by shipping `VenueMessage::PassengerDurationComplete`,
which carries `declared_duration_ns`. So the field is now missing on one frame
rather than two, and there is a landed precedent for how to add it - including
the reasoning recorded on that variant for why a new tag beats a new field: an
old decoder ignores an unknown field and commits the same false transition, where
an unknown tag fails loudly.

Stated for a close pass, 2026-08-26. The change is a wire change and there are
two shapes of it: a new field on `RunComplete` carrying the venue's own elapsed
beside the socket's, or a second tag the way C10 took. The cost of the field
shape is that an old decoder ignores it and commits the same false transition,
which is the reasoning C10 already recorded for preferring a tag; the cost of
the tag shape is a third terminal frame every consumer must classify, on top of
`RunComplete` and `PassengerDurationComplete`. The alternative is to ship
nothing and let the variant's doc carry it, which is the state today: the doc
says `elapsed_ns` is the span that boat covered and not the run's declared
duration, so a consumer that reads it is not misled - only one that assumes.
Nobody has asked for the distinction.

Adjacent, unfiled until now: `RunComplete` is also emitted on the
already-complete-at-boarding path, where `elapsed_ns` measures from the boat
epoch, so a passenger boarding a finished run reports the boat's whole span as
its own elapsed. The variant doc calls this intended, but it is the same class of
consumer confusion C10 was filed for and nothing tracks it.

### C13. Named tape windows have no wire at all

Provenance, folded in from `notes/todo.md` on 2026-08-26 when that file's
shared-exchange section was reconciled away: this is the one axis of the
optional shared-exchange mode still open, and it is open because the
one-venue-per-run rewrite removed it and nothing has since undone that. The
default mode - one venue owned by one run, serving several tapes to one account
at one placement - is complete. Both modes must eventually be supported, so the
axis is deferred rather than dropped.

Owed, not optional. `reference/north-star.md`'s settled premises make a named
window on a shared exchange one of the two ways a path is reproduced, so this is
an end-state commitment the tree is behind on rather than a feature request.

`SocketQuery` carries no start or end: every cursor is placed at the fixed
`run_start_ns` origin and `duration_ms` is length-from-boarding. This is the half
most strategies will use, because a named window is what makes a forward-test
claim bindable: a run becomes a pure function of `(seed, config, symbol, start,
end)` with no boarding instant and no wall-clock input anywhere, and replication
pairs dealt the same window trade identical water by construction rather than
approximately.

A named window always gets its own river even against an identical request
already running, because the first requester is by then some N of sim-time ahead
and a window means being served from its start. Sharing therefore only happens
for the unnamed form - a preset plus a duration - which is the request that says
"wherever you are is fine".

One constraint to design for up front: a strategy needs warmup before its
requested start, so `[T1, T2]` asks for materialization from `T1 - warmup_ns`,
and that floor must sit at or above `TAPE_ORIGIN_NS`. A window requested too near
the tape origin cannot carry its own warmup. Better as a named refusal at request
time than a short warmup nobody notices.

Re-verified 2026-08-26, unstarted and accurate: `SocketQuery` is
`deny_unknown_fields` over `symbol`, `speed`, `duration_ms`, `account`, the four
`surge_*` keys and the presented identity, so a client cannot even smuggle a
start in, and every cursor placement is against `state.run.started_ns`.

One thing to fix while here, and a documentation defect on its own terms:
`SocketQuery::speed`'s doc comment already asserts the named-window rule as
though it were shipped - "A named window always gets its own river even against
an identical request already running". No named window exists on the wire. Under
the folder rules only `north-star.md` and `glossary.md` may state the end state
as fact; everywhere else says so in as many words and names it as owed. Whoever
lands C13 makes that comment true; whoever does not should make it say it is
owed.

---

### C16. The venue no longer receives a terminal interrupt

Filed 2026-08-24 from round 4, as the stated cost of the C8 process-group fix.
`launch.rs` puts the child in its own process group with `command.process_group(0)`
under `cfg(unix)`, which took the venue out of the launcher's group, so a
terminal Ctrl-C reaches the launcher alone. `PR_SET_PDEATHSIG` still kills it
with its launcher and the SIGTERM path is unaffected, so nothing is leaked; what
is lost is the interactive stop.

Wording corrected 2026-08-26, because the entry as filed sends a reader to test
the wrong thing. What lost Ctrl-C is a venue *spawned by a launcher* from an
interactive terminal. A `mogwai serve` typed directly at a shell never goes
through `launch.rs` and still takes Ctrl-C normally.

Recorded as a deliberate trade rather than an oversight, and documented at the
site. Open only as the question of whether the interactive case deserves a
forwarded signal.

Stated for a close pass, 2026-08-26. No wire change is involved - this is a
launcher-side signal question. The change would be a SIGINT handler in the
launching process that forwards to the child's process group, which is the same
`killpg` call `launch.rs` already makes on timeout, so the mechanism exists and
is one handler away. The cost is that installing a process-wide signal handler
is a decision the launcher makes on behalf of its host, and this crate is
embedded in consumers - broadarrow, and a nautilus host - that may install
their own; a library that claims SIGINT unasked is a worse defect than the one
it fixes. The alternative is the status quo, which leaks nothing: the venue
still dies with its launcher through `PR_SET_PDEATHSIG`, so what Ctrl-C costs
is only latency to teardown in an interactive session, and the SIGTERM path is
untouched. If a forwarding handler ships, it should be opt-in on `LaunchSpec`
rather than a default the host cannot decline.

## D. Data and generator

### D1. Generator defects inherited from the closed measure-and-fit arc

Recorded 2026-08-23 as a transcription from that arc's deleted documents, with
the evidence in `notes/tape-research-v1.md`. Re-verified against the code
2026-08-24, and the entry as filed was substantially stale: of the three defects
it named, one was already repaired, one is now gated with a stated residual, and
one is live but was mischaracterized. What follows is the verified state. Treat
anything else inherited from that arc with the same suspicion.

**Closed. The `children_mean` clamp.** The mechanism was real: sweep size per
parent event is `children_mean`, scaled per state by `ARRIVAL_QUIET_CHILDREN_MULT`
0.20 and `ARRIVAL_ACTIVE_CHILDREN_MULT` 1.4308, the pair chosen so the weighted
mean is preserved exactly. A sweep cannot hold fewer than one child, so at MNQ's
observed mean of 1.1711 the quiet state wants 0.234 children, clamps to one, the
identity collapses, and realized mean lands near 1.44 whatever is configured.

`begin_event` now branches on exactly that condition and switches to a
floor-aware solve that re-derives both active parameters from the unconditional
targets at the current effective mean. The branch selects on the base configured
mean rather than the surge-effective one, so no runtime path crosses between the
two arithmetics, and `GeneratorScalars::validate` refuses a configuration whose
floor-branch solve is not expressible. The comment at the branch names the July
MNQ fit failure and the 1.44 figure - it is this defect and its repair. Nothing
is owed here; it is recorded so the finding is not re-filed from the old note.

**Gated, with a residual. `ARRIVAL_MEAN_CAL = 0.944` reaching the integrated
frame.** The constant corrects the shipped sampling scheme's realized-mean
inflation and is derived by bisection, not fitted. The integrated frame draws
from an exact time change and has no such inflation, so a correction leaking into
it would be a uniform 1/0.944 = 1.0593 rate excess on every integrated river.

`the_arrival_mean_calibration_stays_off_the_integrated_frame` pins the
integrated mean to the declared mean bit for bit, and the 2026-08-09 calibration
amendment rests on the frame staying bare.

Updated 2026-08-26: this paragraph used to end by asking for a source-side
accessor so the corrected half could be asserted too, and that work has landed.
`GeneratedSource::active_mean_s` exists as a `cfg(test)` accessor documented as
the one observable of the calibrated side, and the test now states both halves
as exactly as each other. The gate is whole; nothing is owed here.

Unverified, and the part of the original finding that still stands: the claim
that the shipped path carries a 5.5 to 7.0 percent absolute-rate conflict against
the observed July month. That is a question about the shipped scheme's rate, not
about the leak, and the leak's gate says nothing about it. A Jensen-gap
explanation for it was refuted in closed form. Establish whether it reproduces
before acting on it.

Re-checked 2026-08-26 and it has not moved: the claim exists only as prose in
`notes/tape-research-v1.md`, no test or artifact in the tree bears on it, and
`ARRIVAL_MEAN_CAL` is unchanged. This is the live half of D1 and the only one
that needs a measurement rather than a reading.

**Live. The calendar has no daylight rule.** Not hardcoded, which is what the old
note said: `utc_offset_minutes` is a validated calendar field, and the MNQ preset
declares `-300`, a CDT summer offset. The defect is that the model carries one
scalar with no daylight transition, so a river fitted in July and walked through
November is an hour out of place.

The measured symptoms: twelve of twenty-four Stage M control walks, exactly the
winter rotation, collapsed the local-hour-22 stratum to zero variance, and
November put 840,315 rows outside declared sessions, 3.8 percent against
September's 0.5. Any regression suite over this needs one daylight, one standard
and one transition month, because single-month validation is structurally blind
to it.

It is disclosed rather than hidden: the preset's provenance table carries
`calendar.utc_offset_minutes` as `kind = "declared"` with the rationale
"permanent CDT model clock; DST transitions unmodelled, so CST sessions sit one
UTC hour later than this table places them". So the cost is not a constant edit.
Giving the calendar a daylight rule is a schema change reaching every preset, and
that is the decision this finding actually carries.

### D8. A composed river has no checkpoint chain, so a distant seek is O(ticks)

Filed 2026-08-26 as the residue of D4, and named because C13's tape windows are
what will meet it. `SegmentSource::seek_to` no longer caps the walk - a cap
there turned distance into a latched terminal fault, which would have made a
window on a composed river fail silently - so a far target is now reachable and
simply costs the walk. What it costs is the whole walk: the composed level and
the sampling draw are both path-dependent, so no segment can be skipped without
composing a different river, and the seek is linear in ticks from wherever the
source stands.

`GeneratedSource` had the same shape and `CheckpointIndex` is what fixed it -
snapshot the walk every K ticks, resume from the snapshot before the target,
replay only the residual. The composer wants the same thing, and the venue's
`Rivers::place_cursor` shows how a caller consumes it. Until then a consumer
opening a window far from the composed origin pays for every tick between, which
is correct but not fast, and nothing bounds how long it holds whatever lock it
took to get there.

### D5. The 86 MB and 57 MB build tax, and the dead protocol code

`analysis/mnq-measure-12a.json` is 86,147,079 bytes and is `include_str!`d at six
sites, three of them outside `cfg(test)`, so three copies are baked into the
shipped binary. The three non-test sites are `mogwai-cli/src/ordered_counts.rs`
(`run_with` and `run_with_rows`) and `mogwai-cli/src/count_curve.rs::reference`;
the test-only three are in `mogwai-lab`'s `arrival_control.rs` and
`arrival_screen.rs`. The counts were five and two as filed, corrected 2026-08-26 -
`count_curve.rs::reference` was missed, and since the count is the argument, the
case is stronger than filed rather than weaker.

`analysis/mnq-arrival-screen.json` is 57,044,526 bytes and is parsed in full by
`arrival_envelope_diagnostic.rs`'s `committed_screen_selects_the_twenty_a3_only_failures`,
which is not ignored, so every `brokkr check` reads it.

Both are terminal outputs of the closed 12b protocol. They cannot be removed
without deciding the larger question they sit inside: roughly 25,000 lines across
`mogwai-lab` and `mogwai-cli` are the compiled machinery of the closed arc (the
arrival screen, control and envelope family, `measure12a`, `aggregate`,
`stage_m` and its Tier 2 limb, `count_curve`, `ordered_counts`, `slow_geometry`,
`tick_composition`, `select_windows`), and the binary still advertises them as
supported subcommands.

Owner call, deferred until v2's shape is known, since a successor may want some
of the corpus-side machinery. Do not delete without a ruling.

### D6. The synthetic top of book is uncalibrated

Quoted width, top sizes and trade displacement are placeholder constants pending
CME TBBO. The layer exists since tape protocol 7; what is absent is the
calibration.

### D7. Numerical stability in `AutoCorr`

`crates/mogwai-lab/src/characterize/mod.rs`. Its `acf()` guards zero variance
with `if var <= 0.0 { return vec![0.0; self.k] }`, which catches zero and any
negative residue but not a positive one, so a series constant at an irrational
value - the fixture case is `abs(log return)` constant at ln2 - can leave a tiny
positive residue from `sumsq / n - mean * mean`, slip the guard, and return an
ACF that came out of catastrophic cancellation rather than measurement. Which
side of zero the residue lands on is not something the caller controls. Both
branches substitute a number where the honest answer is that the quantity is
undefined for a constant series.

Deliberately not fixed. `AutoCorr` also computes the F1 duration ACFs and is
bit-exact against `analysis/cadence.json` (`duration_acf_lag1`
0.32204142581620676, `duration_acf_lag5` 0.22388204486699373), which is the
lineage the fingerprint's cadence half rests on, and changing the estimator
invalidates that equivalence.

A fix returns an explicit unavailable rather than zeros, uses a relative rather
than absolute variance floor, and probably a two-pass or Welford accumulation -
all of which move numbers, so the work is the analysis of what moves in the
cadence targets and whether the fingerprint must be refitted. Real monthly series
carry positive return variance and come nowhere near the degenerate case.
Cadence-impact analysis is required before anyone touches it.

---

## E. Instruments and account policy

### E3. Nothing has been fitted for equity, perpetual or inverse

A symbol configured as one is served the default tape wearing a different shape;
the intake sequence is what makes a preset honest and none has been run.

### E8. Undecided, listed so they are not re-derived

- Whether a havoc-induced disconnect should behave differently from a real one.
  The venue armed the blackout so it knows the client is merely blinded, and
  `GoDark` is arguably toothless if the world stops while it is armed. The
  venue's split is unchanged: `GoDark` gates the writer wholesale, `StallData`
  gates market data only. Nothing is blocked.

  Owner question, raised 2026-08-26. This item used to end "freeze-and-resume
  covers it for now", and that phrase names nothing in the tree: there is no
  freeze, pause or resume control in `mogwai-protocol`, and the only `Freeze` in
  the codebase is unrelated engine prose about a removed order entering the
  terminal truth store. Either it refers to a mechanism under another name, or
  the premise has evaporated and this item is less covered than it reads.
- Whether a strategy should see its own remaining budget in order to size against
  it. It can derive peak and threshold from its own fills and marks, so blind
  trading is workable. Decide it when one asks.
- Whether an order-list release should carry a market reading. A standalone
  `MarketToLimit` takes the market, while the same type released as an order-list
  child rests at its stated price, because `Engine::release_child` runs inside
  `apply_linkage_after_fill` with no `MarketReading` to price against. Resting is
  the consistent behaviour and the carve-out is stated in `docs/oms-types.md`, so
  nothing is broken. Deciding yes would make a released `MarketToLimit` execute
  on arrival and would reopen whether a `Market` child can be admitted too.
- A trigger-act latency havoc arm, if a scenario ever needs a trigger fired later
  than the sweep interval already allows. Deliberately not built: the sweep
  interval already bounds trigger lateness, and a per-trigger delay knob is a new
  arm rather than an extension of an existing one.

---

## F. Adapter

### F3b. A cadence conflict that will never clear is indistinguishable from one that will

Filed 2026-08-24 from round 4, as the residue of F3. The adapter now retries the
venue's second-cadence refusal, which is right: the rule lifts when the
incumbent passenger leaves, and the incumbent need not be ours. What it cannot
do is recognize the one case that really is permanent - this client's own two
legs configured with different `speed` values - because the venue names the
seated speed but not who is seated at it, and the data and exec clients are
separate objects with no handle on each other. So a misconfigured pair dials
its cap out behind a repeated `warn` line rather than failing at construction.

The cheap close is upstream of the dial: a host builds both configs, so a
constructor that took the pair, or a shared cadence value both legs read, could
refuse the mismatch before either socket exists. That is a public API shape
change on `mogwai-adapter` and wants a decision, not a patch.

Round-1 verdict, 2026-08-26: take the public API change and make the normal
construction path return the data and execution configs as one validated pair.
An optional pair validator leaves every existing caller exposed, while a
process-global registry mistakes unrelated clients in one process for a pair.
The migration cost is every host that currently constructs the two public
configs independently, including broadarrow's two helpers in the pinned
read-only snapshot. It is not landed in this round.

### F5. `for_run` discards `account_ttl_ms` and `reset_account_on_reconnect`

The adapter's reconnect loop can back off past a freeze TTL blind.

Widened 2026-08-26, and it is worse than "discards". Both `for_run`s lift exactly
one field over `for_addr`, the expected run seed. Neither name appears anywhere
in `mogwai-adapter` outside a test fixture, so the adapter has no field to hold
either value and no code path that could consult one. Closing this is adding the
plumbing, not un-dropping an argument.

Round-1 verification, 2026-08-26: copying the two readiness fields into each
independent config is not enough. One leg cannot tell whether the other still
keeps the account attended, so it cannot know when the TTL began; treating its
own disconnect as that instant would terminate recoverable connections. The
same paired lifecycle boundary F3b needs must own this interpretation. Keep this
filed until that boundary lands, then carry both readiness facts through it and
gate reconnect against the account's shared attendance state.

### F7. `await_account_registered` is a busy-wait shim

`wait_connected` now sleeps on an adapter-owned notification, with a 250 ms
backstop re-read of the flag and its wall-time bound unchanged. The backstop is
not a leftover poll: bite-checking the notification by deleting
`notify_waiters` hung every socket test for the full dial timeout rather than
failing on anything that named the cause, so a latch with one publisher and no
fallback was trading five hundred cheap wakeups for a wedge. The cache half
remains: `await_account_registered` polls every 10 ms until nautilus's
runner has consumed the forwarded account event and inserted the row, with its
own 5 s wall bound. The pinned nautilus cache exposes no registration
notification, and notifying when the adapter forwards the event would be too
early: forwarding only queues it. Closing this residue needs a signal at the
nautilus cache insertion boundary rather than another adapter-side latch.

### F10. `fetch_account` id mismatch is only a cosmetic log line

The adapter's `fetch_account` names the configured account, but what a
reported-id mismatch should mean under the per-account venue is still only
`note_account_label`. Whether it should ever be treated as an error is undecided.

### F11. Reconciliation proves the adapter would answer, not that the node asks

`crates/mogwai-adapter/tests/reconciliation.rs` seeds venue truth and pins each
granular generator, `query_order` and their mass-status composition over both
query carriers. The silent-degrade property is a class, shared by every report
path mogwai relies on. Known limitation, recorded rather than closed.

### F12. Owner call: `DuplicateNextFill` certifies nothing against a nautilus host

Verified 2026-08-23 against the pinned version - `mogwai-adapter` pins nautilus
0.62.0 (an earlier note said 0.61) and `research/nautilus_trader` is at 0.62.0,
so the read is at the pin rather than at HEAD. `commit_fill` emits
`fill.clone()`, `trade_id` included. `ExecutionEngine::validate_fill_for_order`
calls `Order::is_duplicate_fill`, which matches `trade_id`, `order_side`,
`last_qty` and `last_px` together - all four identical - and bails with a warning
before `Position::apply`, whose own `check_predicate_true` on a repeated trade id
would panic. The suppression is not keyed on `causation_id`, as the original note
had it.

What is left is not a bug report but a choice, and the two options inject
different lies:

- Keeping the shared id models a retransmitting venue and makes the arm a test of
  the consumer's deduplication, which nautilus passes in silence and which
  nothing observes.
- Minting a fresh `trade_id` per emitted fill models a phantom execution, which a
  correct consumer books twice - the divergence the arm's own doc comments
  describe ("doubles the wire event, not the truth"). Minting also shifts every
  subsequent venue trade id, so it owes a re-bless of the exact-equality
  transcripts.

The verdict and both readings are recorded at the emission site in
`mogwai-engine`'s `commit_fill`. Re-verified 2026-08-26 in every clause,
including the pin: `Cargo.toml`, `Cargo.lock` and `research/nautilus_trader` all
agree at 0.62.0, so the parenthetical correcting the earlier "0.61" note stands.

---

## G. Tests and tooling

### G1. Triage every test for parallel safety, and kill every fixed duration and wait

`[test.profiles.gate]` sits at `test_threads = 8`, which is a measured compromise
rather than a resolution: at 16 the run goes red as a wrong answer rather than a
watchdog timeout, so the ceiling is set by our least robust test rather than by
the machine, and every fixed wall-clock wait in the suite is a piece of that
ceiling.

What the measurement found. Serial, the gate spent 164s executing 1,608 tests,
and the top 20 of them were 54 percent of it while the other 1,451 came to 3.8s
combined. Almost none of that concentration is computation: the lifecycle gates
spend a declared `--duration` in wall time and the reconnect ladders spend their
attempts the same way. The genuinely CPU-bound tests are the tape walks, and they
are the minority.

The work is triage before repair. For every test ask two things: can it run
beside its siblings, and does it wait on a duration rather than on a condition. A
test that waits for a state to be reached, with a generous deadline as the
failure path, is both parallel-safe and fast; a test that sleeps a fixed span is
neither, and it silently prices the whole gate. The lifecycle family is the
obvious start - `completion.rs`, `serving.rs`, `lifecycle.rs` and the adapter's
four socket binaries - but the sweep is every test, because the point is a
property of the suite.

Check the fixed-path unit tests while triaging too: nothing collides today, since
every one writes a distinct `target/...` name and ports are kernel-assigned, but
that is convention rather than structure and one duplicated literal breaks it
only under load.

What it unlocks: `test_threads` can go to 0 (num_cpus) with the cliff gone rather
than merely avoided. What is believed not to be the answer: a serial lane for the
socket-backed tests floors at 74s against the flat setting's 53s, because those
tests are the best parallel citizens in the suite precisely by being idle.

Provenance correction, 2026-08-26. This entry said that comparison was "already
measured and rejected in `brokkr.toml`", and it is not written down there - the
file records the serial-versus-8 story and the cliff at 16, but the 74s/53s pair
appears nowhere in it. The numbers may well be real and simply unrecorded, but
nobody should cite them as settled from the config. Anyone acting on this should
re-run that comparison rather than inherit it. The rest of the measured figures
here - 164s, 1,608 tests, the top-20 concentration - do appear in the gate
profile's own comment; only the serial-lane result is unsupported.

Both `[test.profiles.gate]` and `[test.profiles.dev]` sit at `test_threads = 8`,
which the entry does not mention and which doubles the blast radius of any
change.

Anything called settled here needs repeated runs. `test_threads = 8` went red
after three green runs at 8, having already gone red at 16; three passes are not
evidence about an intermittent race, a failure rate is. The parked list is empty
as of 2026-08-19.

Triaged 2026-08-26, and the raw count was hiding the shape. The often-quoted
"73 sleep sites" is a count over the whole of `crates/`, and about forty-four of
them are production pacing - the launcher's owner loop, the adapter clock and
reconnect ladder, the boat and sweeper cadences - which price no test and belong
to no part of this entry. The test and test-support sites number thirty-one:
sixteen under `mogwai-cli/tests` and fifteen under `mogwai-adapter/tests`. What
follows is the classification, so a later round finishes this without redoing
it.

- **Poll intervals inside a deadline-bounded loop, eighteen sites.** The
  wanted shape already: the loop ends on a condition, the deadline is the
  failure path, and the sleep only decides how often the condition is asked
  about. These are not a gate cost - a passing run leaves them as soon as the
  condition holds - and converting one buys nothing. Leave them.
- **Negative-observation windows, six sites.** The adapter's blackout watch, its
  stranger-socket and dial-cap watches, `havoc.rs`'s "the data client must never
  ship divergences" window, its bound-to-another-run disconnect watch, and
  `serving.rs`'s settle before reading the absence of a market-reading warn. The
  assertion here is that something does not happen for a span, and there is no
  condition to wait on because the property is an absence. The duration is the
  subject rather than a bet on it, and shortening one weakens the test rather
  than speeding the suite. Leave them, and do not let a later sweep mistake them
  for the convertible class.
- **The duration is the thing under test, three sites.** `lifecycle.rs`'s
  `a_venue_launched_after_untracked_work_inherits_that_works_budget` sleeps to
  spend wall budget, which is precisely what it asserts about; `serving.rs`'s
  slow-connection gate reads on a deliberate stall, which is what makes it fall
  behind; and the adapter harness sleeps the armed blackout's own window.
- **Converted in this round, two sites.** `serving.rs`'s
  `history_is_bounded_by_the_run_clock_and_no_boat_moves_it` slept 250 ms to make
  its boat a late one, and now waits on the run clock reporting the lead through
  `wait_for_run_lead`; and
  `a_second_socket_claiming_an_account_evicts_the_first_and_resumes_its_ledger`
  slept 500 ms for its order to reach the ledger, and now waits on the venue's
  own acceptance through the existing `await_acceptance`. Both premises were
  previously unstated bets whose losing side is silent: an early boat makes the
  test pass under the rule it exists to reject, and an unaccepted order makes
  eviction look like a lost book.
- **Blocked, and on what, two sites.** `serving.rs`'s market-reading gate spaces
  its attempts 500 ms apart - the spacing is convertible to a run-clock
  condition, since fifty sim seconds is exactly what the comment says it wants,
  but that same comment names the gap as the whole flake margin of the assertion
  below it, so moving it is a change to the test's statistics and not a
  mechanical conversion. And
  `data_client_transport.rs`'s segmented-head test sleeps 20 ms so the reader
  observes two reads rather than one; the condition is "the reader has consumed
  the first segment", which no seam exposes, and losing the bet weakens the test
  silently rather than failing it. Both need a decision or a seam, not a sweep.

So the convertible class was two sites, not seventy-three, and it is now empty.
The gate's ceiling therefore does not come from fixed sleeps in the way this
entry assumed: the concentration the 164s profile found is declared `--duration`
runs and reconnect ladders spending their attempts, which are durations the test
asked the venue for rather than sleeps a test could stop taking. Whoever picks
this up next should aim at that - a lifecycle test's declared duration, and
whether the ladder's attempt spacing can be a parameter the test passes - rather
than at the sleep count.

### G3. Nothing on the wire says whether a submit took a market reading

Which forces
`serving::a_market_submit_takes_a_reading_on_the_priceless_wire_path` to key on
the venue's log.

Name corrected 2026-08-26. The test was
`..._on_both_the_priced_and_priceless_paths` when this entry was filed, and it
lost the priced arm for a stated reason recorded at the site: the wire now
refuses a consumer-stated price on a market order, so that arm is a `400` at the
boundary and there is no longer a submit to observe. The defect claim itself is
unchanged - `OrderFilled` carries no reading instant and no flag.

When `read_market` refuses - a cold volatility estimator, a truncated walk - the
engine falls back to the order's stated price and logs a warn, and on a
price-less market order the venue stamps the last print either way, so the fill
lands on the tape whether a reading was taken or not. That is exactly the path
the venue used to get wrong.

What the venue would have to ship: the reading's own instant, or a bare "reading
taken" flag, on `OrderFilled`. Two things follow. The gate stops reading logs,
and the adverse-slippage invariant - a market buy fills at or above the print the
venue read - becomes an exact per-fill statement instead of the bracket it is
now. The bracket exists only because the reading instant is unidentifiable from
outside: it is neither the acceptance instant nor the fill instant, and
`MarketReadingCache` buckets it further.

Half closed 2026-08-26. This entry carried two things: stale citations of the
renamed test, and the observability defect itself. The citation half is done -
`mogwai-venue/src/fills.rs` and `mogwai-cli/tests/serving.rs` name the live test
now, and `fills.rs` was the one that mattered, being durable source naming a test
that did not exist. The third site, `notes/todo.md`, went with the section the
G12b sweep deleted. Only the `OrderFilled`
observability half remains, and it is what the paragraphs above describe.

Stated for a close pass. The wire change is one optional field on
`OrderFilled`: either the reading's own `sim_now_ns` or a bare boolean saying a
reading was taken. The boolean is the cheaper of the two and is enough for the
gate, which only needs to stop reading logs; the instant is what the exact
per-fill slippage statement needs, and it is strictly more, so shipping the
instant subsumes the boolean. The cost is a field on the highest-volume frame
the venue emits - every fill carries it - which is a real byte-budget cost the
boolean does not escape either. The alternative is the status quo: the gate
keys on a warn log, which is not a wire contract and can be reworded by anyone
touching the engine's fallback path without a test noticing, and the
adverse-slippage invariant stays a bracket rather than an equality.

### G5. The abandoned-upgrade path has no socket-level test

No client behaviour found so far reaches it. The mechanism is the RAII guard
`Attach` in `run.rs`, whose doc states the same failure - an upgrade abandoned
after the 101 never reaches `handle_socket`, so no lane is bound and none
released - pinned only by that file's unit tests, which drop an `Attach`
directly.

Symbol corrected 2026-08-26: the entry named `Passenger::attachments`, which does
not exist anywhere in the tree and would send a reader hunting. The hole itself
is open, and confirmed by absence: no `SO_LINGER` or `set_linger` appears
anywhere under `crates/`.

Sixteen connections writing a well-formed upgrade request and then resetting with
`SO_LINGER` at zero all landed on the handled path instead: on loopback the venue
has read the request, written the 101 and started the handler before the reset
arrives. The race is inside hyper's upgrade handoff, so parameterizing an
interval has nothing to take hold of. Closing it needs a seam the venue does not
have, most plausibly a test-only delay or counter between the response and the
handoff.

### G9. Neither shared conformance fixture detects a quietly widened `tolerance`

The fixture version is a schema version, and a tolerance edit weakens both
implementations at once, so the second implementation - whose whole purpose is to
catch a one-sided drift - is structurally blind to it. Unlike the arrival vectors
there is no re-derivation to compare against; a fix needs an independently
derived bound on the tolerance itself.

Left filed 2026-08-26, with the dead end recorded so it is not re-walked. Every
shape that closes this without new measurement is circular. Pinning the current
tolerance in a test states the number twice and detects an edit that changes it,
which is a rename check rather than a contract check - the sanctioned way to move
a tolerance re-blesses the pin in the same change, exactly as for a frozen
snapshot hash. Deriving a bound from the fixture's own cases asks the fixture to
justify itself. Tightening the tolerance to whatever the current implementations
happen to achieve pins two implementations against their present agreement and
would fire on any legitimate float drift, which the correctness contract
explicitly permits. What is actually owed is a bound from the estimator's
sampling error at the fixture's sample sizes - a measurement nobody has run, and
one that would have to name the decision it changes before it is run. Until
then the standing mitigation is the doctrine's: review a tolerance change as a
contract change, because no gate will.

### G10. Nothing routes a new wall-clock budget into the `timing` sweep

`brokkr.toml` states the policy - a latency assertion is `#[ignore]`d at the
source, listed in the gate's `skip`, and named in the `timing` sweep's `only` -
and the tool enforces it in one direction only: an `only` entry the gate does not
skip is an orphaned pair, and a filter matching nothing is dead. The converse,
that every budget-carrying test appears in some `only` filter, is not a syntactic
property and is therefore not checkable, so a plain `#[test]` asserting 50 ms in
the parallel dev lane is admitted silently.

Where the quantity is a parameter, inflating the interval under test is the
better answer; where it is not, there is no answer. An owner-level question if
anyone wants it mechanised - a source scan for `Duration::from_` inside an
`assert!` is the crude form, and would have to justify its false-positive rate
against a repository full of legitimate loose bounds.

Left filed 2026-08-26, and the crude form was checked rather than assumed away.
The scan's false-positive rate is not marginal: the G1 triage counted thirty-one
test-side wall-clock durations, and eighteen of them are poll intervals inside a
deadline-bounded loop while six more are negative-observation windows - all of
them legitimately a `Duration::from_` sitting next to an assertion, none of them
a latency budget. A scan that flags twenty-four correct sites to find one
incorrect one gets suppressed on its first run and then means nothing, which is
the vacuous-control shape the doctrine names. The syntactic property the tool
does enforce - an `only` entry the gate does not skip is an orphaned pair, a
filter matching nothing is dead - stays the whole of the mechanism, and the
converse stays a review obligation on whoever writes a wall-clock assertion.
What would change this is a marker at the source rather than a scan of it: a
budget-carrying test declaring itself, so the tool has something to enumerate
instead of something to guess. That is a new convention and wants a ruling.

### G11. The no-shouting textlint is blind to several classes

Shouted words survive in four classes at once. Check the lint's coverage before
sweeping by hand, because a hand sweep will miss whatever the lint misses next
time - and this entry is itself the proof, having carried a false all-clear for
two days.

- Rust comments it does not reach. `mogwai-venue`'s `config.rs` still carries
  `RESOLVED`, `ASK`, `CASH`, `PER BOAT` and `NOT`; `mogwai-engine`'s
  `account.rs` carries `KEY`. All are single words that rewrite cleanly.

  Corrected 2026-08-26 on two counts. The claim that "the `run.rs` and `http.rs`
  survivors were swept 2026-08-23" is false: `http.rs` still shouts `ANY`, `IS`
  and `DOES` in `process_order_cmd` and its neighbours, and `arrival_control.rs`
  shouts `OWN`. And the `account.rs` named here is `mogwai-engine`'s, not
  `mogwai-venue`'s, which has none.
- The fourth class, undiagnosed until now, and the reason `RESOLVED` survives in
  a linted path: the preset's inline-code-span exemption matches on the physical
  line, so a line whose last backtick closes a span before the shouted word is
  exempted whole. `RESOLVED` sits after a closing `` ` `` on its line and is
  never examined. That is the false negative the preset's own comment warns
  about, and it silently exempts an unknown number of lines.
- Test fixture headers, which are `.toml` rather than Rust and were never in
  scope: `crates/mogwai-cli/tests/configs/` wants a sweep, one file having been
  de-shouted in passing. At least eleven files shout, including `perpetual.toml`,
  `account-ttl.toml`, `fast.toml` and `bounded-run.toml`.
- Assertion and panic message strings, which read to a human exactly like prose
  (`the venue CLOSED the perpetual socket`, `never fully WATCHED a run` in
  `serving.rs` and `completion.rs`, `TEST'S REMAINING BUDGET` in
  `tests/common/mod.rs`, `EXCLUDE` in `mogwai-data`'s generated tests). Excluded
  structurally, since the lint's region is `comment`. Whether the rule covers
  them is worth deciding once rather than per site, and the class is larger than
  the two examples originally given.

  Decided 2026-08-26, both halves separately, because the round-5 fix pass
  answered only the second and the entry asked for the first.

  The rule covers them. An assertion message is prose a human reads at the
  moment a test fails, which is the worst possible moment to be shouted at, and
  the AGENTS.md rule is about the writing rather than about which token the
  tokenizer calls a comment. So the sites named above were rewritten, along with
  the eight others in the same class, and a future one is a review nit like any
  other.

  The lint does not cover them, and will not. Structurally the region would have
  to become `string`, and a string region in this codebase is mostly wire
  payloads, protocol tags, symbols, account ids and close reasons - `MOGWAI`,
  `BTCUSDT`, `SubmitOrder`, `RETRYABLE_REJECT_PREFIX`'s text - so the rule would
  fire overwhelmingly on values the venue is contractually obliged to spell that
  way. Excepting them by shape is not available either: an assertion message and
  a close reason are both plain string literals and nothing in the syntax tells
  them apart. That leaves an allowlist per literal, which is the mechanism the
  preset's own comment already rejects as unfinishable. So this class is
  enforced by review and not by the tool, and the entry stays open only as that
  standing statement rather than as work.

The inline-code-span exemption was repaired 2026-08-26 and then repaired again
in the same round, because the first repair traded one blind spot for its mirror
image. Requiring a closing backtick stopped a line being exempted by a span that
closed before the shouted word, and exposed nine Rust-comment violations that
had been hidden; but the resulting pattern still matched from a span's close to
the next span's open, so a shout sitting between two code spans was exempted
just as silently. The pattern now counts complete span pairs from the start of
the line, so the backtick it reaches is an opener by construction. Coverage also
grew a TOML comment rule over the CLI test fixtures and the venue presets, which
is where the third class above lived. The known coarseness that remains is the
one the preset states: an exemption still applies to the whole physical line, so
a line carrying both a genuine code span and genuine shouting slips through, and
a shout built entirely of words under five letters never fires at all.

### G12. Open lead, not reproduced and not closed: the SIGTERM shutdown test

`sigterm_stops_the_venue_within_the_shutdown_grace` in
`crates/mogwai-cli/tests/lifecycle.rs` failed one full `brokkr check --gate` run
on 2026-08-19 and passed the identical tree's second, with the tree's changes
nowhere near the serving or shutdown path. Hunted twice on the harness that did
fire the completion-path race: 20 rounds at 16 threads and 30 at 32, both under
64 busy processes, zero failures.

Read the new output before theorising. The original failure message folded two
opposite verdicts into one sentence, because `Venue::wait_for_exit` clamps its
bound to the test's remaining wall budget, so the wait may have been far shorter
than the 10 s it reported; the helper now reports how long it waited and which
bound produced that. Host contention is the boring reading to rule out first.

If the clamp is not it, the two candidates are `spawn_blocking` work in flight at
signal time - the sweeper's tape walk and the boatyard's `worker.join()` both run
there, and a dropped tokio runtime waits for blocking tasks that have started -
and the boat worker's responsiveness to its cancel flag. The completion-path
session wait does not touch this: a signal deliberately does not wait for
sessions. When it fires it aborts the instrumented sweep and the gate then
reports every `mogwai-data` test as orphaned; the tell is the orphan count
equalling the missing sweep's pass count.

### G13. Watch the first live `mogwai measure` run

Against the tightened `session_dates_are_23_sorted_unique` gate in
`crates/mogwai-cli/src/measure.rs`. It has only ever executed under its unit
test, because the gate sits mid-way through a multi-minute walk driver behind a
Brick G cache that no test sweep in this workspace populates.

### G12b. Closed 2026-08-26: `notes/todo.md` no longer copies entries this file owns

Decided 2026-08-26 by the close pass: this document is the source of truth for
every entry both files carry, todo.md's preamble says so, and any correction
lands here and only here. The first execution took todo.md's "Tests and tooling"
section, verified entry by entry as a full duplicate of section G minus this
file's corrections.

The remaining sweep ran in round 5 and the entry is closed. Every section of
todo.md was compared entry by entry:

- Deleted as full duplicates, nine sections: shared-exchange mode (C13),
  instruments and account policy (E3 and all four E8 items), measurement and
  method (H1 through H6), values wanting constants (I1 through I4), owner
  rulings (K1 through K7), documentation owed (J1 through J5), the two appendix
  sections, and the CLA gotcha (L1). The venue-and-protocol section's C10 copy
  went with them - C10 is closed by `PassengerDurationComplete` and the copy
  described the behaviour before it - along with the pre-narrowing C11 copy, and
  the engine, data and adapter copies of B7, D1, D4, D5, D6, D7, F7, F10, F11,
  F12, F3b and F5.
- Kept in todo.md, never extracted and so not this document's to own: the
  segment-sampler gate, the refusal-texts-spell-their-bounds item, the
  zero-price fill, generator havoc forking the river, and four adapter items
  (the leveraged-forex instrument gap, nautilus's `MarketToLimit` constructor,
  the dropped perpetual funding fields, and `HavocSpec.data` having no reader).
- Moved here rather than deleted, being content only the copy carried: C13's
  shared-exchange provenance, the upstream emitter entry's statement of what
  nautilus would have to ship, and five broadarrow items whose closing path this
  document had summarized away.

Two dangling citations fell out of the sweep and were fixed in the same change:
`mogwai-adapter`'s `client/data.rs` and `client/exec.rs` each cited
`notes/todo.md` for the upstream gap they sit on, which is doubly wrong - a code
comment may not cite `notes/` at all, and the section it cited is now gone - so
both comments now carry the upstream ask themselves.

---

## H. Measurement and method owed

### H1. The price-span-per-inferred-match-event measurement

Triggered stops and plain market orders slip by the existing fill band, reused
rather than separately fitted, and how wide that band should be for a triggered
stop is a scale question that has never been computed. The sweep tail quoted
elsewhere - up to 2,213 aggTrade rows in one inferred event on BTC - counts rows
rather than distinct prices, so it does not establish how far a marketable order
actually walks. One probe extension over archives already on disk would settle
it; until then the slippage magnitude is an unquantified mechanism shared by
every order type that slips.

### H2. Build a positive dead-feed watchdog

Formerly sweep item AD12. No liveness timer, tick counter or "0 ticks in N s" log
exists on either transport. The negative diagnostics are all in place, but
nothing positively proves a subscribed feed is alive rather than genuinely quiet.

`idle_timeout_ms` does not cover it: it defaults to 0, and even armed the idle
clock resets on any application frame, so a data-silent-but-frame-active socket
never trips it - deliberately, since that is what reproduces the 4255 case. The
threshold comes from the landed default-tape dwell bound (the realism gate's
era-windowed p999 gap, empty-hour fraction and longest empty-hour run), and an
armed LiquidityDrought legitimately silences the feed while remaining visible via
the control plane, so the watchdog can account for it.

Priority note: a real venue failure mostly shows up as a crashed or stalled PID
rather than a protocol event, and the silent-but-socket-alive failure this was
designed for was largely a property of the deleted long-lived daemon. Useful, not
structural.

### H3. The cadence document invalidated a green gate without naming a successor

The 0.1603 duration ACF anchor. That debt belongs to whichever spec descends from
it.

### H4. Per-instrument fitting belongs to whoever builds each preset

Whether BTC and ETH genuinely differ enough to warrant different values - the
measured 2.8x dispersion across three crypto majors suggests so, and one month of
one venue cannot settle it. The evidence asymmetry stays relevant to preset
authors: BTC, ETH and SOL have trade-level archives while MNQ and MES had
15-second bars and nothing else, so a CME preset's cadence is derived arithmetic
and its clustering comes from nowhere. Each preset says where its numbers came
from. Re-derive the asymmetry from the DBN bulk download now on disk before
repeating it.

Candidate symbols for the missing session classes: a perp like ETHUSDT.P, a
second CME future like MGC, and AAPL for cash-equity hours. Terabytes of DBN data
are already downloaded on another host, and the Databento account additionally
holds about twelve months of MNQ/ES/MES tbbo plus mbp-1 server-side, re-fetchable
by job id at no new cost.

### H5. Decide whether the protocol-12b Stage A refinement pass should run at all

Deferred by the owner 2026-08-09 rather than settled, so the frozen pass stands
and the budgets were raised to fund it.

For cutting: refinement is 29,200 s of the 35,526 s Stage A cost model, 82
percent, and its entire product is a finer loss ordering over cells that Stage B
then truncates to `STAGE_B_CELL_CAP = 24` per family. It cannot rescue a family
whose coarse admissible region is empty - the outcome that would close the
landing - because it subdivides around that region's own boundary cells. And
`SELECTION_INDIFFERENCE = 0.01` already declares losses inside that margin as not
separating candidates, so a half-spacing lattice buys precision the selection is
defined not to use. Cutting drops Stage A to about 6,326 s.

Against: the selected parameter point would sit on the coarse lattice, and nobody
has shown the coarse spacing is fine enough for the mechanism to be found at all.

Not the same question as `STAGE_B_CELL_CAP`, which earns its place: a Stage B
cell is a full month-scale walk per seed at about 250 s, so an uncapped
1,508-cell region genuinely is tens of hours. Changing `REFINEMENT_DEPTH` or
`REFINEMENT_CELL_CAP` is a section 17 amendment against the contract of record.

### H6. Reconcile the protocol-12b section 5.5 rescale with the shipped preset convention

That section freezes the negative control's re-centring as "rescale the 24 values
to sum to 1, which the `SessionProfile` schema requires", and the schema requires
no such thing: nothing in `config.rs` or `session.rs` enforces sum-to-one, and the
shipped MNQ `intensity_hour` sums to 23.862306, a mean-one curve.

It moves no generated rate either way, since `SessionModulator::new` divides by an
exposure-weighted normalizer so a common factor cancels at every instant, and the
control's committed `new_curve` is therefore a correct re-centring on a different
scale from its own `old_curve`. What it cost is readability, and it will cost the
same again at any later reader who compares the two curves elementwise. Fixing
the frozen sentence is a section 17 amendment through review, not an edit.

---

## J. Documentation owed

Each of these is a durable-prose gap, not a note. All ride with a code commit.

### J1. Durable prose for the account, river and passenger design

These notes carry no truth guarantee and nothing durable may cite them, so any
part of the design whose reasoning lives only here is invisible to a user.

Owed: the symbol as a label rather than an identity; the three-step resolution
and its total third step; river identity and what forks a river; one clock per
river; the exogeneity that gives passengers non-interference and the
no-queue-competition contract that follows; and the boot-versus-runtime split on
funding.

Durable prose states river and passenger and never the boat, which is a cache
with no semantics, and states the two properties a passenger is owed separately.
`docs/presets.md` and `docs/config.md` are where a user looks;
`reference/architecture.md` is where the why belongs.

### J2. Half of `mogwai --help` is protocol jargon resolving to retired notes

"Brick B4", "Stage M", "Amendment 2" reach an operator who has no way to look them
up.

### J3. `docs/cli.md` cites "the storage policy" three times

A named authority no document defines.

### J4. Structural proposals recorded and unadopted

`reference/architecture.md` is about 1,300 lines doing four jobs, and its
contradictions have all sat where one job's old text survived another's landing;
`docs/havoc.md` was patched rather than rewritten.

Adjacent and real: that document's version narrative walks 5 through 18 and then
asserts the current identity, six unnarrated bumps.

### J5. Glossary and vocabulary standing rules

Glossary entries are admitted by the owner alone, so an agent that meets an
undefined load-bearing word escalates and never adds an entry. Vocabularies that
want definitions want them in `reference/`, not in the glossary. Listed here so
no agent in the loop edits the glossary.

---

## K. Vocabulary and naming, owner-ruled

These need a ruling before code moves. An agent may prepare the change and state
the recommendation, but must not land a rename the owner has not admitted.

### K1. `Balance.locked` conflates three things

Order holds, maintenance collateral and unsettled credits in one wire number with
opposite remedies. Two scopes recommend a split; `Account::unsettled`'s doc in
`mogwai-engine` argues the conflation is fine.

### K2. The evidence toolbox's place on the binary's top level

Eighteen subcommands for one audience beside three for another - and the repeated
leaf names inside it: `preflight` is three different commands and `fit` is two,
all operator-typed, all producing plausible evidence output, with no collision
warning in `docs/cli.md`.

### K3. Whether `leg` gets a glossary entry

One of the two connections a nautilus consumer necessarily holds under one
account.

### K4. The tape-identity vocabulary

River is the sequence, tape the delivery, and the process the version constant
identifies has no name.

### K5. The `held` collision

`exec_held_budget_bytes` and the "held lane" use the word for the outbound
byte-budget sense that kept `reservation`, at a consumer-visible config key.

### K6. `mogwai-engine`'s fourth sense of admission

Acceptance of an order onto the book, about 60 sites, is unruled and must not be
swept as retired.

### K7. A sixth sense of ledger, and a misfiled oracle

`stage_m_tier2.rs`'s append-only candidate ledger is an unruled sixth sense of
ledger, and `mogwai_lab::delivery` still owns the git-cleanliness oracle
(`TreeOracle` and kin), a second unrelated job under a module named for the
delivery manifest.

---

## L. Infrastructure

### L1. The CLA check is not yet a required status check

cla-assistant.io is wired up and its webhook delivers, but nothing blocks an
unsigned merge until a repository ruleset requires the check by name.

The trap: an owner-authored PR produces no status at all, since the CLA assigns
copyright to the owner and the bot correctly has nothing to ask, which means the
check cannot be picked from the suggestion list and cannot be validated against a
real run. Type the context name in by hand and leave the rule in evaluate mode
until an outside PR confirms it, because a required check that never reports
blocks every merge with no visible cause.

Not a code change - repository settings, owner-only.

---

# Appendix: not actionable in this tree

Kept so the ledger is complete. No round works these.

## Open upstream: nautilus_trader

Read the source from `research/nautilus_trader`; build against the pinned
crates.io release. Nothing here can be fixed from this tree.

- **`ExecutionEventEmitter` cannot share its sender**, so this adapter can only
  refuse rather than heal. The emitter derives `Clone` and owns
  `sender: Option<UnboundedSender<ExecutionEvent>>` by value, installed once from
  `try_get_exec_event_sender()`, which reads a `thread_local!` in
  `nautilus_common::live::runner` set on the runner's thread. Every clone taken
  after that point freezes the sender state of the instant it was taken, and
  `send_order_event` on a sender-less clone only logs a warning. Our workaround
  is a refusal, not a repair: a host that starts its clients on one thread and
  connects them on another gets a named error rather than a working client.
  What the other side would have to ship, and what retires this entry: an
  emitter holding its sender behind a shared cell, or resolving it per send
  from a process-wide rather than thread-local slot, so a clone taken before
  `set_sender` still emits.

- **No channel for a declared feed gap.** `VenueMessage::FeedLagged` carries
  `skipped` and `sim_now_ns` and the adapter has nowhere to put it. No
  `DataEvent` variant means "the stream you are aggregating has a hole", the
  client is handed to the host boxed as `dyn DataClient` so an adapter-owned
  counter or health accessor is unreachable, and `is_connected` is true
  throughout because the socket never broke. So bar aggregation over the
  missing span is silently wrong and the polling cursor resumes past it, and a
  strategy cannot distinguish a quiet market from a dropped one. Fabricating a
  report from the local mirror is not the escape: the mirror is built from the
  frames the venue just said it dropped, which is the exact falsehood the
  venue-truth move removed. The execution socket cannot self-heal
  either: the frame translator that sees `FeedLagged` runs as `handler(msg).await`
  inside the reader's own frame loop, so a venue-truth query issued there
  deadlocks by construction, and the client is `!Send` so spawning it off is
  unavailable. Until nautilus ships a data-side degradation signal and a
  client-initiated reconciliation request, a host driving mogwai should treat an
  error from `mogwai-adapter` mentioning a feed gap or a refused frame as a
  reconcile-and-distrust-the-window signal.

- **The Rust trait default for mass status does not compose** the way the Python
  base does. Queued in the maintainer's PR tracker. Not a substitute for our own
  reconciliation guard: mogwai overrides the method, so this protects the next
  adapter author rather than this repo.

- **Tape sparsity has no attribution channel.** An empty historical window is
  correct behaviour here - the fitted ACD arrival process is persistent and
  heavy-tailed, so a short window can legitimately hold zero trades and `/trades`
  correctly answers `200 []` - but it still costs the consumer a fatal halt, and
  one of the two fixes is blocked on the same gap as `FeedLagged`: an empty
  historical response carries no feed identity, so it cannot be attributed.

## Open at broadarrow

Theirs, not ours. One item is genuinely ours and is called out first.

- **Owed by us: tell them, in one message. Nobody has.** Three breaking changes
  now, not two: the whole account surface moved under them, `OrderExpired`
  replaced `OrderCanceled` for expiries, and the divergence control plane
  changed request shape. Several entries below are stale in their favour. This
  is a message to write, not code to change.
  - The break: they set no `account_type`, inherit `MOGWAI-001`, POST no account,
    and have no handling for a run that ends by liquidation. Their orchestrator
    runs the shared shape, so 50 subagents inheriting `MOGWAI-001` would take each
    other's ledger in turn; the id belongs in their per-subagent account TOMLs.
    The account-id contract is in `docs/config.md`. That break is designed, but
    designed-to-break only works if the other side is told it is coming.
  - The second break: an expired order now reports `VenueMessage::OrderExpired`
    with a terminal `Expired` status where it reported `OrderCanceled`.
    Exhaustive matching stops compiling; loose matching stops seeing `Day` and
    `Gtd` orders end at all, which is the dangerous reading.
  - The third break, C4: `POST /control/divergence` no longer takes the
    divergence tag flattened into the request body. What they must send now is
    `{"kind": "<Tag>", "args": {<the tag's fields>}}`, with the optional
    `account` and `symbol` staying where they were, at the top level beside
    `kind`. Unknown top-level fields are refused rather than ignored, so the old
    `{"type": ..., <fields>}` body takes a `422` and arms nothing - it does not
    degrade quietly, and a scenario that posted it would run on believing a
    fault was armed. The refusals and the acks are JSON objects now too: a
    refusal is `{"error": "<reason>"}` and an ack is `{"status": "accepted"}`,
    carrying `detail` and the shed `evicted` divergence when an arm evicted one,
    where both used to be a bare text body. Their poll-heal end-to-end test
    drives this plane directly, so it is the run most likely to notice.
  - In their favour, same message: trailing stops, the full order-type surface
    including `TrailingStopLimit`, order lists and `RejectNextCancel` are all
    served, so their three unrun scenario files can now be written.
    `translate_trailing_exit` can emit the limit form as well as the market one;
    the venue derives the limit price from a `limit_offset`, so they send an
    offset and not a price.

- Item 4 of the strategy-search route, consuming the multi-instrument venue.
  `run_prep::mogwai_facts` refuses a `/instruments` answer of anything but exactly
  one instrument, precisely so a relaxed mogwai breaks their build loudly instead
  of having broadarrow pick an instrument arbitrarily. Closing it means selecting
  by the strategy's frontmatter `MOGWAI:<symbol>`, per worker rather than per
  venue, after which the readiness record's `symbol` field needs its
  one-venue-one-symbol meaning reconciled.
- `POST /accounts` at run-prep preflight, so each worker opens its own ledger with
  its own balances before the node is built. Nothing here blocks it.
- Their profile row becomes `AtomicOuo` and brick 3 of
  `notes/venue-order-list-oco-spec.md` lands. Carve-out they must read before
  citing the group-admission guarantee: a member whose funds an earlier member's
  fill consumed is rejected on the second pass with its earlier siblings already
  accepted.
- Whether a refusal marked `RETRYABLE_REJECT_PREFIX` should be treated as
  retryable at all. Their standing reasoning - a rejection wrongly treated as
  retryable is worse than a run that stops when the venue said no - is still
  sound, and the marker only changes what the decision rests on. Nothing here
  pushes them either way.
- Boot-storm pacing for concurrent `/trades` and `/quotes` warmup, because their
  daemon decides when workers spawn. Our bounded wait makes staggering an
  optimization for ordinary paging rather than a precondition of correctness,
  which is the change worth telling them about.
- `submit_order_list` is the only route that emits a group frame, so a consumer
  wanting an atomic group by any other route has no API for it. None is owed
  until one is wanted.
- Their own repo: the feed-stale message hard-codes the issue-4255 hypothesis
  ("the connection looks healthy...") as fact even when the venue process is
  dead; `reference/mogwai.md` and `ba man mogwai` still describe the venue as
  unfundable, stale since the `[balances]` seed landed; stored scenario TOMLs
  setting `transport_profile` on either adapter config no longer parse, since the
  field went with `TransportProfile` itself, and want a sweep.

### Runs owed against mogwai

Theirs to run, not ours to build, but each is a venue exercise that would surface
mogwai defects, and several have been owed for weeks.

- The restart run, the realized-PnL baseline, legs 1 to 3: serve durably, trade to
  a non-zero realized figure, SIGKILL the worker, re-run against the same
  `[attach]` scenario, verify the carried baseline, the brake mark, and no
  duplicate booking. Leg 3 is load-bearing and rests on a verdict reached by
  reading the dependency rather than by observing a reconciliation, landed as an
  explicit operator override of its own gate - a known-unrun verification on a
  capital bound.
- `go_live` restart de-duplication: kill a non-flat worker with orders resting at
  the durable venue, restart, verify the batch de-duplicates against the
  surviving book.
- The futures run against a `preset = "MNQ"` venue: warmup, fed fills, a resting
  stop triggering on the multiplied instrument, a settlement-currency commission
  actually charged, and the brakes marking in that currency.
- The conditional half of the fed-fill path: a fed fill from an order that
  genuinely rested and then filled at venue timing, ideally under havoc.
- Flip plus pyramid plus partial in one bar, end to end.
- Gate B, the anchored-warmup overlap drop. Their `handoff.rs` covers Binance,
  Kraken and Bybit but not mogwai, and is a consistency test rather than ground
  truth.
- The poll-heal end-to-end test, which drives our control plane directly: rest a
  far-from-market limit, POST `CancelOpenOrderSilently`, assert the local order
  converges to Canceled within the retry ladder's bound. Their fixture notes still
  hold: carry no protective exits, and census the whole rotated log family.
