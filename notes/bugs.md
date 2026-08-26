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

### C7. Refusal texts spell their bounds out instead of naming the constant

Re-verified against the code 2026-08-24 and the entry as filed was half stale.
`messages::validate_wire_symbol` refuses with "symbols are 1 to 32 bytes" and
carries a comment saying exactly why bytes and not characters, so the
units half of the finding is closed. A test in the same module already pins the
refusal text against `MAX_SYMBOL_LEN` by asserting the refusal contains the
constant's value, so moving the constant fails loudly rather than silently.

What is left is narrower than filed: the bound is still spelled `32` inline
rather than named, on the refusal a client sees at the venue's front door since
order entry routes through it. Cosmetic now that the test stands behind it.

Two clauses of this entry were corrected 2026-08-26. The divergence texts are
not in `messages.rs` and there are not four of them: roughly nine live in
`havoc.rs`, spelling their bounds inline (`vol_mult`, `thin_factor`,
`extra_vol_mult`, the `PartialFillNext` fraction, the `FeeSurcharge` mult,
`REFUSE_HALT_SECS`, `start_hour`, `rate_mult`, `children_mult`). Count at the
production sites, since the module's tests carry the same strings as expected
values. `messages.rs` itself has a second instance beside the symbol one -
`validate_callsign`'s "callsigns are 1 to 64 bytes" against `MAX_CALLSIGN_LEN` -
which likewise already has a `contains` test behind it.

The other corrected clause: the entry claimed fixing this needs a changed return
type or a `const` formatter that does not exist. `havoc.rs` already does the
latter, in `format!("DelayAcks/GoDark/StallData ms must be <= {bound} (one
hour)")`, so there is a shipped precedent to follow rather than a design to
invent. The `&'static str` return is what blocks the two in `messages.rs`, not
the absence of a pattern.

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

Adjacent, unfiled until now: `RunComplete` is also emitted on the
already-complete-at-boarding path, where `elapsed_ns` measures from the boat
epoch, so a passenger boarding a finished run reports the boat's whole span as
its own elapsed. The variant doc calls this intended, but it is the same class of
consumer confusion C10 was filed for and nothing tracks it.

### C13. Named tape windows have no wire at all

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

### C17. C10's fix left a doc comment describing the behaviour it removed

Filed 2026-08-26 by the verification pass. `SocketQuery::duration_ms`'s doc in
`ws.rs` still says each passenger "announces `RunComplete` and closes at its own
deadline", which is exactly what shipping `PassengerDurationComplete` stopped
being true. It was one of the three sites the old C10 entry credited with
documenting the imprecision, and the fix moved the code without it.

Small, but it is a durable comment asserting retired behaviour on the frame a
consumer classifies on, which is the same reading error C10 existed to prevent.

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

### D2. Nine deleted Python scripts are still referenced about forty times

`analysis/mnq_fit.py` alone has roughly thirty references across `mogwai-lab` and
`mogwai-cli`, ten in `mogwai-lab/src/subcontract.rs` alone; `characterize.py`,
`build_fingerprint.py`, `select_windows.py`, `build_cadence.py`, `run_corpus.py`,
`fit_session_profile.py`, `check_cadence_feasible.py` and
`tick_composition_ratios.py` account for the rest, across doc comments,
`docs/cli.md`, `notes/` and `Cargo.toml`.

Corrected 2026-08-26: `AGENTS.md` was in that list and is now clean, so the sweep
is crates, `docs/cli.md`, `notes/` and `Cargo.toml`. All nine scripts are
confirmed absent from `analysis/`.

`scripts/retire_note_citations.py` is the existing tool for this sweep but is
scoped to `crates/` and `brokkr.toml`.

### D2a. The deleted-script instruction a user can actually hit

Split out of D2 on 2026-08-26 because it is the one part with user-visible blast
radius and it should not wait on a forty-citation prose sweep.
`mogwai-lab/src/fingerprint.rs` emits the runtime error "analysis/cadence.json is
required; run build_cadence.py first", instructing the user to run a script
deleted in the Rust port. A one-line fix, independent of everything else in D2.

### D4. `SegmentSource` overrides neither `seek_to` nor `fault`

An effectively infinite source inherits the O(distance) default walk that
`mogwai-data`'s own `TickSource` doc warns about - the shape `GeneratedSource`
needed `CheckpointIndex` for - harmless today only because
`mogwai segments compose` walks forward from the origin and never seeks. It
becomes a hang the moment anything serves a composed river or asks it for a
window.

`fault` is the harder half: the composer's one terminal condition, clock
exhaustion, has no `TickFault` variant and is reported only through the inherent
`SegmentSource::clock_exhausted`, which a `dyn TickSource` consumer cannot see,
and adding a variant ripples into `mogwai-venue`'s `http.rs` fault rendering.

The same item owns `emit_price`'s panic: a named panic inside
`TickSource::next_tick` in a library crate, which becomes a serving-path abort
the moment a composed river is served, where `GeneratedSource`'s equivalent
failures go through `TickFault`. Giving the composer a `TickFault` closes both
halves at once.

Re-verified 2026-08-26, accurate in full, with one addition: there is a second
library panic on the same feature, `mogwai-lab/src/segments.rs`'s
`panic!("shipped window {} must be cuttable")`. It sits on a test-fixture helper
path so the risk is lower, but it is the same family and should be swept with
the first.

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

### E9. A preset cycle test written the obvious way passes vacuously

Filed 2026-08-24 from round 5. The provenance check runs before the parent is
resolved, so a preset missing `[provenance]` is refused for that reason rather
than for the cycle it declares. The cycle guard itself is real and does refuse,
but a fixture written the natural way never reaches it and goes green without
testing anything - the vacuous-gate family, in a guard landed the same day.

Either resolve the parent chain before validating provenance, or make the
refusal name which check fired so a fixture cannot pass for the wrong reason.

Narrowed 2026-08-26. The ordering defect is intact - the "has no provenance
table" bail still sits above the `effective_preset_walk` recursion - but the
test is no longer the hazard: `a_runtime_preset_inheritance_cycle_refuses_boot`
emits `[provenance]` deliberately, with a comment saying why. So what remains is
the ordering itself and the next fixture someone writes naively, not a green test
proving nothing today.

Related, and the reason an operator would not notice: the cycle detail lands in
the anyhow context chain rather than the top line, so what is seen is "instrument
preset X is invalid" unless the renderer walks the chain.

### E10. `effective_preset_walk` does not pop its stack on an error return

Filed 2026-08-24 from round 5. The frontier shape, and harmless today only
because the stack is abandoned along with the error. It stops being harmless the
moment the walk is reused across attempts.

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

### G16. The scripts hand-copy Rust constants, and only exercise proves them

Filed 2026-08-24 from round 2, as the residue of the hole that round found.
`scripts/smoke.py` had been pinning `READY_VERSION = 6` against a
`ReadyRecord::VERSION` of 8 for two schema bumps, which killed every mode of the
script at boot, and nothing noticed because nothing ran it. That pin now reads 8
and matches, so the stale instance is repaired; what survives is the shape. The
same hand copy is still there in two places: that pin, and `DIVERGENCE_KINDS`,
the list of divergence tags the control-plane helper will build a body for.

The new `control-shapes` gate closes the half that matters most - it boots a
venue on every gate run, so a stale pin or a body the venue would refuse now
fails immediately. What it cannot do is prove either list complete. A divergence
kind added in Rust and not added to `DIVERGENCE_KINDS` is simply untested, and
the gate stays green while it is. Closing that means the venue publishing its
own kind list on a route the script can read, which is a wire addition and wants
a ruling before it is built rather than being done in passing.

The general lesson is worth more than either constant: a green workspace suite
says nothing about the layer above the code the tests import. Two rounds running
have now put a hole there.

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

**Blocked by:** G2 for the last of the fixed sleeps.

### G2. The venue counts sweep passes for nobody

This is the missing observable behind the last fixed sleeps. Wanted: a monotonic
count of completed sweep passes per river, readable from a route that already
exists - a field on `/clock` or `/health` would do - so a test can read it, wait
for it to advance by N, and say exactly what it waited for.

Nothing on the wire carries it today; the sweeper is internal to `mogwai-venue`'s
fill path and emits no frame, counter or log a test can consume.

Blocked on it:
`serving::the_tape_is_identical_with_and_without_a_resting_stop` polls sim-clock
advance as a proxy, sound only because that config's clock is wall-affine, and
`serving::a_perpetual_position_pays_funding_across_an_interval` still bets on a
wall sleep, the same poll having been found vacuous at `speed = 0.0`.

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

The stale name is still cited from two other places, one of them durable source:
`serving.rs`'s module docstring and `mogwai-venue/src/fills.rs`'s doc comment. A
doc comment naming a test that no longer exists under that name is caught by
nothing, so fix both while here. When `read_market` refuses - a cold volatility
estimator, a truncated walk - the engine falls back to the order's stated price
and logs a warn, and on a price-less market order the venue stamps the last print
either way, so the fill lands on the tape whether a reading was taken or not.
That is exactly the path the venue used to get wrong.

What the venue would have to ship: the reading's own instant, or a bare "reading
taken" flag, on `OrderFilled`. Two things follow. The gate stops reading logs,
and the adverse-slippage invariant - a market buy fills at or above the print the
venue read - becomes an exact per-fill statement instead of the bracket it is
now. The bracket exists only because the reading instant is unidentifiable from
outside: it is neither the acceptance instant nor the fill instant, and
`MarketReadingCache` buckets it further.

### G4. History-splice clamp has no test for the case that motivated it

The cutoff is the tighter of the run clock and the asking passenger's own boat
clock, so a slow boat is no longer served its own future, but every socket test
runs at the venue's default speed where the two clocks agree, so nothing
exercises the branch where they differ. Wants a passenger on a deliberately slow
boat reading history past the point its own tape has reached, with the boat's
frontier as an observable rather than a sleep. Filed 2026-08-23: the code is
right, the coverage is a hole, and this is the shape that passes for the wrong
reason later.

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

### G6. `run_b1`'s build-identity guard has no test

`crates/mogwai-cli/src/arrival_control.rs`. The `bugs-cli` round-3 fix pass
refused the binary split partly on the strength of that guard. It bails unless
`current_exe()` ends with `target/release/mogwai`, so B1's byte comparison is
generated by the binary under test - the property the refusal record names as the
reason `gen` and `arrival-control` must stay in one executable.

Nothing asserts it: a test binary's `current_exe` is never
`target/release/mogwai`, so only the refusal arm is reachable. Wants a seam that
lets the accepted path be supplied. Low priority, since the guard is three lines
and its consequence is a refusal rather than a wrong answer.

### G7. `mogwai gen --type trace` has no end-to-end CLI coverage

The one test sits at the `write_trace` seam in `gen.rs`; nothing drives the argv
through the shipped binary the way `presets_cli.rs` does for `presets` -
`--trace-from` and `--trace-until` parsing, the four-part window validation, the
`--interval` and `--burn-in` rejections. The window validation admits
`until == end`, which is the case the truncated-`child_count` defect lived in,
and no test states that it is legal.

### G8. The roll conformance fixture's Python half is manual

`python3 analysis/roll_estimator.py conformance` is run by no lane, so its
fixture-version guard fires only for a human who thinks to invoke it. The dwell
pair has automated tests on both sides; this pair does not, because a Rust test
may not spawn Python.

### G9. Neither shared conformance fixture detects a quietly widened `tolerance`

The fixture version is a schema version, and a tolerance edit weakens both
implementations at once, so the second implementation - whose whole purpose is to
catch a one-sided drift - is structurally blind to it. Unlike the arrival vectors
there is no re-derivation to compare against; a fix needs an independently
derived bound on the tolerance itself.

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

### G12a. `brokkr.toml`'s gate comment points at the wrong document

Filed 2026-08-26. The gate profile's comment ends by sending the reader to "the
parallel item in `notes/todo.md`" for the rest of the story. That item is G1 in
this document, moved by the 2026-08-24 transcription. Both files still exist so
nothing errors, and a reader simply lands on the wrong page - which is the
failure mode that makes a pointer worse than no pointer. Fix it when G1 is
worked, since whoever is there is already reading both.

### G13. Watch the first live `mogwai measure` run

Against the tightened `session_dates_are_23_sorted_unique` gate in
`crates/mogwai-cli/src/measure.rs`. It has only ever executed under its unit
test, because the gate sits mid-way through a multi-minute walk driver behind a
Brick G cache that no test sweep in this workspace populates.

---

### G17. The closed-market refusal has no end-to-end socket coverage

Filed 2026-08-24 from round 3. Every assertion on this guard is unit-level,
against the predicate or the boundary helper. The path from `process_order_cmd`
through `refuse_all` to a consumer-visible market-closed frame is untested, which
is why a boundary-level regression in that guard landed green: the round 3 fix
pass admitted a whole order group whenever its first closed-session member was
non-marketable, and a full gate could not see it.

Wants a passenger submitting into a scheduled close over a real socket and
reading the refusal frame, including the group case with a marketable member
behind a non-marketable one.

### G19. Named account-side arm routing is proven at two layers, not through the wire

Filed 2026-08-26 from round 1, as the residue of F13. What the round changed is
one identifier in `arm_divergence`: the engine one-shots and `FeeSurcharge` now
pass the request's `account` to `Run::arm` where they passed `None`. Two tests
stand on either side of that line and neither crosses it.
`a_named_account_side_arm_reaches_that_ledger_and_no_other` proves `Run::arm`
routes both kinds by name, and bites when the named branch is widened;
`ships_venue_havoc` proves the adapter puts the account on the body, and bites
when the engine variants leave its match. Nothing asserts that a body naming an
account reaches only that ledger's engine over a real socket, so re-passing
`None` at the route would take the whole gate green.

`crates/mogwai-cli/tests/serving.rs` already owns the harness - `post_divergence_body`
and the venue fixture - so what is missing is the scenario rather than the
machinery: two accounts on sockets, a `PartialFillNext` naming one, a submit on
each, one partial fill and one full. The same shape covers `FeeSurcharge` by
reading the two commissions.

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

## I. Values that want to become named constants or knobs

### I1. `default_instruments()` BTCUSDT seed is seven inline literals

`mogwai-protocol`. Duplicated verbatim in two of that crate's own tests, and the
smoke test's fixed order shape depends on it implicitly. Its own doc comment
signposts multi-instrument growth.

### I2. HTTP route strings have no shared registry

`mogwai-venue`'s route strings are inline literals with no shared registry against
the adapter's route segments, so a renamed route breaks the pair silently.

### I3. Two uncapped job counts beside `arrival_screen`'s measured 16

Half of this entry is closed, verified 2026-08-26. It was filed as
`DEFAULT_MAX_JOBS` carrying no comment naming its measurement, and it now carries
a four-line doc comment saying 16 is measured rather than chosen, that past it an
SMT regression eats the parallelism, and pointing at `reference/performance.md`
for the runs behind the number.

What is open is the other half, now with one more site than filed:
`arrival_envelope_diagnostic.rs` and `mogwai-cli/src/tick_composition.rs` both
take `thread::available_parallelism()` bare, with no cap at all. Whether they
should share `arrival_screen`'s 16 is open - but it should be settled for both at
once rather than one at a time.

### I4. `MIN_WALL_REQUEST_TIMEOUT_SECS` is the tightest cap on usable sim speed

Flagged in its own comment in the adapter. If sim speed is ever pushed hard, that
constant is the first wall.

---

### I5. `nix` is an unconditional dependency of `mogwai-protocol`

Filed 2026-08-24 from round 4. Every use of it is behind `cfg(unix)`, but it is
declared in plain `[dependencies]` rather than under
`[target.'cfg(unix)'.dependencies]`. Harmless while nothing builds this on
Windows, and exactly the shape that bites the first time something does.

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
  is a refusal, not a repair.

- **No channel for a declared feed gap.** `VenueMessage::FeedLagged` carries
  `skipped` and `sim_now_ns` and the adapter has nowhere to put it. No
  `DataEvent` variant means "the stream you are aggregating has a hole", the
  client is handed to the host boxed as `dyn DataClient` so an adapter-owned
  counter or health accessor is unreachable, and `is_connected` is true
  throughout because the socket never broke. The execution socket cannot self-heal
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
  one instrument, precisely so a relaxed mogwai breaks their build loudly.
- `POST /accounts` at run-prep preflight. Nothing here blocks it.
- Their profile row becomes `AtomicOuo` and brick 3 of
  `notes/venue-order-list-oco-spec.md` lands. Carve-out they must read before
  citing the group-admission guarantee: a member whose funds an earlier member's
  fill consumed is rejected on the second pass with its earlier siblings already
  accepted.
- Whether a refusal marked `RETRYABLE_REJECT_PREFIX` should be treated as
  retryable at all.
- Boot-storm pacing for concurrent `/trades` and `/quotes` warmup.
- `submit_order_list` is the only route that emits a group frame, so a consumer
  wanting an atomic group by any other route has no API for it. None is owed
  until one is wanted.
- Their own repo: the feed-stale message hard-codes the issue-4255 hypothesis as
  fact even when the venue process is dead; `reference/mogwai.md` and
  `ba man mogwai` still describe the venue as unfundable; stored scenario TOMLs
  setting `transport_profile` no longer parse and want a sweep.

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
- `go_live` restart de-duplication.
- The futures run against a `preset = "MNQ"` venue.
- The conditional half of the fed-fill path.
- Flip plus pyramid plus partial in one bar, end to end.
- Gate B, the anchored-warmup overlap drop.
- The poll-heal end-to-end test, which drives our control plane directly.
