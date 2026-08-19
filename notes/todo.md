# TODO

Open work only. How the built system works lives in
`reference/architecture.md`; the landing-by-landing history is in git; the
per-crate mechanics are in code comments.

**NOT the live arc.** Work that is actively being DONE belongs to its own
track's document, not here - see `notes/README.md` for the map. This file is
for what is parked, deferred, unresolved or simply elsewhere: adapter and
engine items, investigations nobody is running, decisions nobody is taking,
the value inventory. If an item is on the critical path of the work in flight,
it is in the wrong file.

Once an item here is completed, it GETS REMOVED ENTIRELY. If the prose contains
any relevant information that must endure, it gets either (a) added as an inline
comment in the code, or (b) added to an existing or new ../reference/ document.

Or both. There are no exceptions.

## The grand design, and mogwai's place in it

Recorded 2026-08-15 with the owner, after walking the whole toolchain. This
is the context every item below serves. A mogwai developer does not need to
operate the other tools, but must know what mogwai is FOR, because several
design decisions only make sense against it.

THE ECOSYSTEM. One human runs a handful of orchestrator agents. Each
orchestrator deals a batch of `wyrd` assignments - deterministic,
stratified strategy-design slates (a behavioral thesis plus rarity-weighted
entry/exit/sizing components, with replication pairing across the batch) -
and launches on the order of 20-50 subagents, one slate each. Each subagent
authors ONE Pine strategy: it consults the Pine oracle, lints, hand-rolls
its dealt components as libraries, then establishes its EDGE on real
historical data through piners (backtest, optimize, Monte Carlo,
walkforward), passes the broadarrow backtest parity witness, and finally
FORWARD TESTS through broadarrow against mogwai. The goal per strategy:
minimum drawdown, roughly 20-50 dollars of income per day, reported as a
scope-qualified claim ("works during $session on $symbol under $conditions")
back to the orchestrator. The claim pipeline - collecting results,
adjudicating replication pairs, deciding what deploys - is the ORCHESTRATOR'S
job, human plus Claude, deliberately not software. Nothing in mogwai should
grow toward owning it.

MOGWAI'S ROLE, stated exactly. Mogwai is the ONLY forward-test venue the
ecosystem has, and the only model of venue timing either project has. What a
mogwai forward run validates is EXECUTION ROBUSTNESS: resting-order and
conditional-order timing, fills under havoc, survival of the messy live
path - the things a bar-close backtest structurally cannot see (wyrd's own
doctrine: resting-order exits are validated in forward, against an
accelerated synthetic tape). The EDGE was established upstream on real
history; dollars earned on an exogenous synthetic tape are a statement about
the fitted distribution of worlds, not about next month's market. Keep those
two claims distinct in anything mogwai reports or documents. The
distribution is also the point: one seed is one path, and a claim wants many
seeds - which is why fire-and-forget, seed-reproducible instances and cheap
tape identity matter more than any single run.

WHAT MOGWAI MUST BECOME for this to work, which is what the items below
build:

1. SERVE ANY SYMBOL, symbol-as-label, total resolution, many rivers at once.
   The venue must never be the reason a dealt strategy cannot be forward tested.
   LANDED 2026-08-15/16. A second clause - MANY PASSENGERS ON ONE RIVER, meaning
   N traders each with its own account, ledger and view of one deterministic
   tape - belongs to the SHARED-EXCHANGE mode only, is not needed by the default
   per-run mode. IT HAS SINCE LARGELY LANDED, and this entry said otherwise until
   the `notes/bugs-server.md` close pass corrected it 2026-08-19: the engine and
   the risk ledger moved onto `Passenger`, so each account has its own book, and
   delivery is attributed - an order event reaches its owner's lanes and an
   `AccountState` the account it names, rather than every connection. What is
   still open under this clause is the per-account TAPE WINDOW rather than the
   ledger.
   See "An exchange serves many accounts and many tape windows" below.
2. REALISTIC TAPES ACROSS SESSION CLASSES. The wyrd doctrine holds session
   structure to be the one non-fractal thing bars do not normalize away, so
   a session-bound thesis forward-tested against the wrong session class
   tests a DIFFERENT claim. The envisioned preset set is on the order of
   five, spanning three classes: 24/7 crypto (BTCUSDT, a perp like
   ETHUSDT.P), CME futures with genuine closure (MNQ, plus another such as
   MGC), and cash-equity hours (AAPL). The segment-sampler track (see
   `notes/README.md`) is what makes session-composable tapes; the intake
   sequence makes each preset honest; neither ever gates serving.
3. THE DATA SPIGOT IS `dbnget` (Databento). The account already holds about
   twelve months of MNQ/ES/MES tbbo plus mbp-1 server-side, re-fetchable by
   job id at no new cost. The missing session classes are cheap: a year of
   AAPL trades quoted at about 10 dollars list, a year of continuous MGC
   tbbo at about 2, and the subscription zeroes covered pulls. Corpus
   acquisition is never the bottleneck; fitting effort is.

The 200-agent end-state paragraph in the PROBLEM STATEMENTS block below and
the settled premises it records (always accelerated, single-instrument
strategies, one MOGWAI venue, resource cost shapes nothing) are this design's
mogwai-side constraints. TWO OF THEM MOVED ON 2026-08-16 - the venue's scope
and what "no restart" implies - and the amendment is recorded in that block
rather than here.

THE VENUE'S SCOPE, stated here because everything above reads differently
without it. A venue is scoped to an ORCHESTRATOR'S BATCH, not to one run: the
orchestrator starts one `mogwai serve`, takes the bound address, and hands it
to its 20-50 subagents, each connecting with its OWN ACCOUNT and asking for
whatever tape its strategy needs. A consumer given no address spawns its own
ephemeral transient venue - the dev, CI and fallback path. Both shapes are
supported; the shared exchange is the primary one.

## Landing the grand design

Counted 2026-08-15 as a fourteen-piece inventory, EXCLUDING tape realism
(the segment-sampler and preset-fitting track, which is separate). All
fourteen pieces are now landed: preset selection by symbol lookup, config
overlays split from the boot symbol, the derived `InstrumentDef`, the
keyed `Rivers` registry replacing `RunIndex`/`BOOT`, the `/ws` symbol
carrier, the boatyard (sharing key, placement/join/wind-down), one clock
per boat, the seed's symbol dimension, the venue-scoped readiness record,
and last the consumer surface - `/instruments` as configured-plus-
materialized, the adapter's unconfigured-symbol session, and the funding
and funds-rejection wording it forced - landed 2026-08-16. Piece 14, the
durable prose owed by each of the above, rode with every landing rather
than closing as a separate step. Detail is git history, not this file.
Broadarrow's item 4 (consuming the multi-instrument venue) was excluded
throughout - it is theirs, and their build breaking loudly when the
consumer surface landed was the designed handoff.

THE INVENTORY IS COMPLETE FOR THE DEFAULT MODE, and that is the claim to read it
as making (clarified 2026-08-16). The one-venue-per-run rewrite had removed symbol
and ACCOUNT from the request under one premise, and had made tape PLACEMENT a run
property; undoing symbol alone leaves a venue serving N tapes to ONE account at
ONE placement. That is exactly right for a venue owned by one run, which is the
default. It is not enough for the optional SHARED-EXCHANGE mode, where those two
axes are still open - see the section named below. Neither is urgent; both are
required eventually, since both modes must be supported.

## Two more consumer residuals landed 2026-08-18, both marked "and it is mogwai's"

Filed in `../broadarrow/notes/todo.md` under the dep-drift entries 5 and 6, as
the two residuals of eight landings that were NOT theirs to close.

**THE HISTORY GATE NOW WAITS.** Entry 6, which they called the worst of the eight
because it is silent. Four concurrent `/trades` or `/quotes` syntheses, and a
fifth was REFUSED - but nautilus's historical response types carry no error
channel, so their adapter's only alternative to an unresolvable hang is to
resolve the request empty and log why. A refused warmup therefore reached a
strategy as a quiet window, indistinguishable from a tape that printed nothing.
The cap bounds RESIDENT pages and a waiter holds no page, so the refusal bought
the bound nothing: it is now a bounded wait, with a fail-fast queue bound behind
it so the queue cannot become a way to accept everything and answer nothing.
Their other suggested option - carry the refusal as something other than
emptiness - is not available to us, because the missing channel is nautilus's.

**BACKPRESSURE IS NOW A FIELD.** Entry 5b. `AdmissionRejected` carries
`retryable`, and `mogwai-adapter` carries the distinction across the nautilus
boundary as `RETRYABLE_REJECT_PREFIX` on the reason - because `OrderRejected`
has one field an adapter may set, so an admission refusal and a business
rejection reached a strategy in the same shape. They refused to substring-match
our prose, correctly; the prefix is a public constant with a test on it, so what
they match is a contract rather than a sentence.

WHAT BROADARROW OWES: deciding whether to make a marked refusal retryable at
all. Their standing reasoning - a rejection wrongly treated as retryable is
worse than a run that stops when the venue said no - is still sound, and the
marker changes only what the decision RESTS on. Nothing here pushes them either
way.

STILL OPEN AND NOT OURS, recorded so it is not re-filed: the boot-storm PACING
half of entry 6 is broadarrow's own item, because their daemon decides when
workers spawn. The wait makes staggering unnecessary for ordinary paging rather
than a precondition of correctness, which is the change worth telling them
about.

## Two consumer asks landed 2026-08-18, and what broadarrow owes back

Both were filed in `../broadarrow/notes/todo.md` against the 2026-08-18 survey of
the "be an exchange" landing. Both are DONE here; the entries stay only for what
the consumer side still owes and for the one design decision that outran the ask.

**THE ACCOUNT CARRIER.** `mogwai-adapter` built `{base}/ws?symbol={symbol}` and
never sent `?account=`, so every broadarrow socket seated the venue's DEFAULT
ledger whatever the host's config said - silently, because account resolution is
total. Both client configs now name their configured ledger. THE ASK DID NOT
ANTICIPATE THE CONSEQUENCE and it is worth knowing: a nautilus host holds TWO
sockets on one account, and a claimed account EVICTS its incumbent, so naming the
account alone would have made every host disconnect its own data leg on its exec
dial. `/ws?session=` is the answer - one client's sockets coexist, a different or
absent session evicts - and the adapter mints one per process from the pid and
start instant, so a host configures nothing. Durable: `docs/config.md`,
`reference/architecture.md`.

WHAT BROADARROW OWES: `POST /accounts` at run-prep preflight, so each worker
opens its own ledger with its own balances before the node is built. Nothing here
blocks it.

**ATOMIC GROUP ADMISSION.** `ClientMessage::SubmitOrderGroup` landed, which is
the ask's preferred option 1 rather than the deferred-activation fallback. The
two smaller asks landed with it: `apply_linkage_after_fill`'s silent skip past an
absent sibling is now a `debug` line naming the hazard, and a linked bare
`SubmitOrder` is REFUSED at the wire boundary. `docs/order-lists.md` states the
admission guarantee explicitly, which is the sentence their capability bit was
waiting to cite, and `a_group_shrinks_a_sibling_admitted_after_the_fill_that_shrinks_it`
is the test they asked for - it fills leg one on the same call that admits leg
two and shows the aggregate bounded at one bracket quantity. Its negative twin,
`per_leg_dispatch_lets_an_entry_fill_before_its_stop_is_admitted`, keeps the
hazard measured rather than merely described.

WHAT BROADARROW OWES: their profile row becomes `AtomicOuo` and brick 3 of
`notes/venue-order-list-oco-spec.md` lands. Note the one carve-out they must read
before citing the guarantee - a member whose funds an earlier member's fill
consumed is REJECTED on the second pass with its earlier siblings already
accepted, so admission is atomic for everything the venue can decide in advance
and not for a balance the group's own fills moved. Whether their own exits meet
it depends on the run's `oms_type` and is theirs to know, not ours to assert: a
reduce-only exit reserves nothing and never meets it, while one submitted
without the flag takes initial margin per resting contract at admission and is
not clamped to a position. Two earlier drafts here were wrong and are recorded
so they do not come back: that their exits are reduce-only and reserve nothing
was a guess about a consumer's order shape, and that the unfunded member is
CANCELLED was a claim about an event the submit path does not emit - the cancel
reading belongs to a TRIGGERED order that outruns its account, which is a
different path.

NOT DONE, and it is theirs to decide rather than ours: the `submit_order_list`
path is the only thing that emits a group frame. A consumer wanting an atomic
group by any other route has no API for it, and none is owed until one is wanted.

## Open issues

- THREE COMMANDS DEFAULT AN ARTIFACT PATH TO A REPO-SHAPED RELATIVE PATH, AND
  ONE OF THEM NAMES IT `target/`. `fit`'s `DEFAULT_OUT`
  (`target/mogwai-fit/mnq-fit.json`) and both `synth` outputs
  (`target/mogwai-synth/fingerprint.json`, `.../cadence.json`) go through
  `mogwai_lab::storage::artifact_path`, which resolves a bare default against
  the WORKING DIRECTORY by design - an artifact is the user's file and is
  deliberately never cached. So a fit run from anywhere but the repository root
  creates a directory called `target` under the operator's feet and writes into
  it, and the name reads as a build directory rather than as output.
  `preflight` has the same shape with `analysis/out/...`, which at least is
  honestly repo-scoped. The `bugs-cli` round-1 cold review flagged `fit`'s;
  it was left because the behaviour is the policy's and the convention is
  crate-wide, so fixing one in isolation makes the set less coherent, not more.
  The question is whether `artifact_path` should refuse a default that is not a
  bare file name, or whether these commands should name a directory the way
  `bugs-cli` finding 8 argues the whole repo-scoped toolbox should. Decide it
  with finding 8, not before.

- NOTHING GATES A TEST WRITING TO A RELATIVE `target/`. The `bugs-cli` round-1
  fix pass closed three instances (two in `mogwai-cli`'s `arrival_control`
  unit tests, one in `mogwai-lab`'s `the_control_walk_pair_replays_one_tape`)
  and swept the two shadow directories they had accumulated in
  `crates/mogwai-cli/target/` and `crates/mogwai-lab/target/`. A unit test's
  working directory is its crate, so such a path is invisible to `git status`
  (the root `.gitignore`'s bare `target` matches at any depth) and untouched by
  `cargo clean` (cargo's target dir is at the workspace root). The helpers are
  in place - `mogwai_lab::storage::unit_test_scratch` and
  `mogwai_cli::test_paths::scratch_dir` - but nothing detects a fourth
  instance, so it is found by looking or not at all. The
  `no_test_declines_to_assert_on_a_missing_input` scanner beside
  `no_test_binary_writes_a_committed_fixture` in
  `crates/mogwai-cli/tests/gate_skip_list.rs` is where such a check would go;
  the hard part is distinguishing a relative `target/` in a test from an
  `--out` default a production function legitimately carries, which is why it
  was not written blind.

- `try_reserve_boundary_frames` TAKES A `usize` AND DOES `frames.max(1)`, which
  is an unreachable state made harmless rather than unrepresentable. Round 3 of
  `notes/bugs-server.md` refused it as a defect - every `refuse_all` call site
  is guarded by a `submitted_orders(...).first()`, so the zero-frame case cannot
  arrive, and an unspent reservation is returned when the `Reservation` drops.
  Turning the count into a `NonZeroUsize` moves the guarantee from the call
  sites to the signature, which is worth doing and is a change across the
  admission boundary rather than a bug fix.

- A REFUSED `/ws` UPGRADE CAN STILL MINT A LEDGER, and it is now the only
  allocating read left. `ws_upgrade` calls `Run::passenger` on the
  non-resetting path in order to take the seat before the eviction, and the
  cadence refusal comes AFTER it, so a request refused with the 400 leaves a
  fresh account behind. Round 3 closed the sibling case - `GET /account` no
  longer mints, resolving through `peek_passenger` and previewing an unopened
  ledger with `Run::unopened_ledger` - and deliberately did not touch this one,
  because the ordering it stands on is the round-1 ruling that every refusal is
  decided before the seat. Whether a refusal may allocate an account is the
  question; `peek_passenger`'s doc states the limit in place.

- `/control/divergence` ALLOCATES A PENDING ARM RECORD PER NAMED ACCOUNT that
  has not connected yet, capped at `MAX_PENDING_ACCOUNT_ARMS` (64) and shed from
  the oldest end. It does NOT mint a ledger - a draft of the round-2 fix did,
  and that locked the named client out of its own `POST /account` with a `409`
  while handing it default balances - so the control plane is no longer a second
  account-creating surface, and finding 8 of `notes/bugs-server.md` can be closed
  without reference to it. What is left here is the shed: an operator who arms 65
  distinct names before any of them connects loses the first one's arm silently.
  The `202` says nothing about it. Whether the control plane should report a shed
  pending record the way it reports a shed engine divergence is open.

- A SOCKET'S `RunComplete` REPORTS SLIGHTLY LESS THAN THE DECLARED DURATION, and
  nothing on the wire lets a consumer tell that from a short run. Filed
  2026-08-19 by the `notes/bugs-server.md` close pass, which reproduced it: 2
  failures in 40 rounds of the `completion` binary at 32 threads under 64 busy
  processes, short by 1.7 ms of a declared 2 s and by 18 ms of a declared 30 s.
  The deadline is judged on the VENUE clock; `ws.rs` re-derives every
  announcement on the receiving socket's BOAT clock, which is anchored at that
  boat's placement, so the announcement trails by the placement gap times
  `speed`. Both halves are deliberate - a boat-stamped frame keeps per-socket
  monotonicity, and waiting for every boat's clock would let a socket connecting
  near the deadline extend the run by another whole duration - so this is a
  CONSUMER-VISIBLE property rather than a bug, now stated in
  `reference/clock.md`. What is open is whether a consumer should be able to
  tell: the frame carries no boat epoch, so a strategy cannot distinguish "the
  run served its whole duration and my boat was placed late" from "the run was
  cut short". Shipping the boat's epoch, or the venue's own elapsed alongside
  the socket's, would close it and is a wire change nobody has asked for.
  The tests that tripped over it now bound the skew through
  `completion.rs`'s `boat_skew_floor`; the run-clock half they used to imply is
  pinned by `mogwai-server`'s
  `the_deadline_wait_never_reports_done_before_the_sim_clock_arrives`.

- `ClearDivergences` STILL DOES NOT DRAIN THE ENGINE'S ARMED QUEUE, so an
  operator has no wire control that disarms a `PartialFillNext`. That is the
  documented split - `Engine::clear_armed` is crate-local precisely so the wire
  variant's contract is not widened by accident - and the round-2 venue-arm
  record MIRRORS the omission rather than fixing it, so a ledger minted after a
  clear holds exactly what a seated one holds. Whether the wire should get a
  full reset is a product call nobody has taken; if it does, `ArmRecord::engine`
  is cleared in the same change or the two halves diverge.

- ENGINE-ARM APPLICATION ORDER IS UNORDERED ACROSS CONCURRENT CONTROL REQUESTS.
  `Run::arm` records under the passenger map lock, but the engine half is applied
  after both locks drop because the engine sits behind an async mutex - so two
  `POST /control/divergence` requests in flight at once can land on two seated
  ledgers in opposite orders, and a ledger minted between them holds the
  record's order rather than either. `Run::arm`'s doc states this limit rather
  than papering over it. Unreachable in practice: the control plane is an
  operator surface and is serialized in every scenario the venue is driven from.
  Closing it means serializing the arming path against itself, which costs a
  second lock on a path that has never contended.

- THE VENUE-WIDE FEE SURCHARGE REPLAY HAS NO DIRECT ASSERTION. It is applied by
  the line beside the engine-queue replay in `ArmRecord::open_engine` and is
  covered only by that neighbour biting, because `mogwai-engine` exposes no
  reader for `fee_surcharge` outside its own crate.
  `fee_surcharge_multiplier_for` is `pub(crate)`. Closing this means either a
  public accessor or a socket-level test that fills on a late-connecting
  account and reads the commission; neither was worth a public API in round 2.

- OWNER CALL: SHOULD AN EVICTION-RECONNECT RETIRE THE BOOK IT TAKES OVER? The
  `notes/bugs-server.md` round-1 fixes count a newcomer onto an account BEFORE
  the incumbent is evicted, so the account never freezes in that window and
  `resume` sees `returning == false`: an eviction-reconnect now gets no
  `retire_off_river` and no `rebase_scans`, deterministically. That is right for
  the case it was aimed at - a client's own reconnect, where retiring would
  discard a live book - and it silently changes the STRANGER case, where a
  claimer connecting on a different symbol inherits the previous session's
  off-river position rather than having it retired. Before the fix the outcome
  was a race, so no behaviour was lost; what is owed is a ruling on whether the
  stranger case wants the retirement back, which would need eviction to be
  distinguishable from reconnection at `resume` rather than inferred from the
  freeze. Stated in `reference/architecture.md` as it stands.

- THE ABANDONED-UPGRADE PATH HAS NO SOCKET-LEVEL TEST, and no client behaviour
  found so far reaches it. `Passenger::admitted` exists for the upgrade a client
  walks away from before `handle_socket` runs - no lane bound, no lane released
  - and that branch is pinned only by `run.rs` unit tests that drop an
  `Admission` directly. Sixteen connections writing a well-formed upgrade
  request and then resetting with `SO_LINGER` at zero all landed on the handled
  path instead: on loopback the venue has read the request, written the 101 and
  started the handler before the reset arrives. The race is inside hyper's
  upgrade handoff, so parameterizing an interval - the arc's usual remedy - has
  nothing to take hold of. Closing it needs a seam the venue does not have, most
  plausibly a test-only delay or counter between the response and the handoff.

- THE CONNECTION LIFECYCLE IS STILL FOUR MUTABLE STRUCTURES rather than one
  derived registry. `Run::lanes`, `Passenger::frozen_since`,
  `Passenger::seated_on` and now `Passenger::admitted` each carry part of the
  answer to "is anybody reading this account", with the consistency rules in
  prose. The `notes/bugs-server.md` round-1 fixes closed the two live holes -
  eviction now happens after every refusal, and the freeze is decided by one
  predicate over the lane table and the admission count - but they closed them
  by adding a fourth structure, not by removing the possibility. NOTHING
  DETECTS the next lifecycle path that updates three of the four. The hunter's
  proposal stands: one registry keyed by account holding the live connections,
  with `is_frozen` and `is_seated_on` as derived queries. Not attempted in
  round 1 because it is a rewrite of `run.rs` rather than a fix, and the two
  holes were live.

- ONE `/ws` REFUSAL STILL SITS AFTER THE EVICTION, and only one: the cadence
  check re-run on a ledger the seat MINTED OR RESET, which can lose to another
  upgrade racing the same account inside that window. The pre-seat check that
  covers every other case cannot cover this one, because the ledger it would
  ask about does not exist yet. Closing it means making eviction and admission
  one transaction under the lane lock - which is the registry item above, not a
  local fix - so it is filed rather than papered over. It needs two upgrades on
  one account interleaved inside a few microseconds to fire.

- PRODUCT CALL: IS A ZERO PRICE LEGAL ON AN INVERSE INSTRUMENT? Raised by the
  `notes/bugs-engine.md` round-2 cold review and answered locally rather than
  ruled on. An `Inverse` contract's value is `1/price`, which has no value at
  zero, and `InstrumentDef::unrealized` reports that as `None` - the same answer
  it gives for arithmetic overflow. Two live paths reach it:
  `Engine::settle(&[(symbol, px)], ts)` passes the caller's settlement price
  straight through with no zero guard, and a ZERO-PRICE FILL is explicitly
  warned about and then BOOKED (`warn_zero_px`), so a position can carry
  `mark_px == 0`. `position_unrealized_checked` now answers ZERO for both cases,
  which stops a zero settlement price crediting `Decimal::MAX` to the balance
  and is the conservative reading, but it is a choice: the alternative is
  REFUSING a zero price on an inverse instrument upstream, at `settle` and at
  the fill, so an unpriceable position cannot exist at all. That is a statement
  about what the venue permits rather than about arithmetic, so it wants an
  owner ruling before either is called settled.

- `projected_qty` TAKES A BARE QUANTITY, so an incoming order that is itself one
  leg of an `Oco` pair is counted ADDITIVELY against the exclusive group it
  belongs to. Noticed while closing the round-2 regressions. The resting book is
  now counted correctly by `Engine::worst_case_leaves` - held children
  contribute nothing, an exclusive group contributes its max - but the
  `additional` argument cannot be, because the caller does not pass the order.
  The effect is a conservative over-projection in `mogwai-server`'s optional
  `max_position` cap: it can refuse an order the book could not actually have
  reached, never admit one it could. Fixing it means giving `projected_qty` the
  `SubmitOrder` rather than a `Decimal`, which is a signature change through
  `http.rs`; stated at the site and left alone until the cap matters enough.

- AN EQUITY SELL'S RESERVATION STILL HANDS THE SAME HELD SHARES TO EVERY RESTING
  SELL, filed by the `notes/bugs-engine.md` round-2 fix pass as the residual of
  finding 9. `Engine::order_reservation`'s margin-equity sell arm computes
  `uncovered = leaves - max(0, net_position)`, so a margin account holding 100
  shares with two resting sells of 100 posts collateral for neither, where the
  worst fill order leaves it short 100. ADMISSION IS NOW CORRECT -
  `validate_submit`'s short check reads `Engine::worst_case_leaves`, so nothing
  is accepted that the account cannot post for - and what is left is
  the HOLD carried between acceptance and fill.
  NOT FIXED BECAUSE THE OBVIOUS FIX BREAKS A DIFFERENT INVARIANT.
  `order_reservation` is per-order BY CONSTRUCTION: `Engine`'s incremental
  `order_locked` cache adds and removes one order's entry at a time, and
  `reconcile_order_locked` panics on any drift from a fresh fold. Any formula
  that reads the OTHER resting sells makes one order's reservation a function of
  the book, so removing one order silently changes another's entry and the debug
  reconciliation fires. Closing this properly means moving the cover allocation
  out of the per-order derivation and into the aggregate, which is a redesign of
  the cache rather than an edit to the formula - and the report's own suggested
  expression does not do it either, since summing
  `leaves - max(0, net - other_sells)` over both sells reserves for 200.
  There is also a product call inside it: what a venue SHOULD hold against a
  covered sell that another resting sell might consume first is a policy
  question, not only an arithmetic one.

- TWO ARRIVAL KNOBS HAVE NO UPPER BOUND, and both let an operator config buy a
  half-second parent draw. `ArrivalConfig::LogOuCox`'s `sigma_y` is validated
  only as finite and non-negative, and `x = exp(y - sigma^2 / 2)` is unbounded
  BELOW, so a thin latent stretches the budget traversal in
  `ArrivalKernel::next_parent` over its 366-day limit one second at a time.
  MEASURED 2026-08-19: `sigma_y` 8 at `thin` 1000 costs 3.6 ms per draw on
  average, peaking at 115 ms, and KEEPS SUCCEEDING - so unlike the cliff it
  recurs draw after draw rather than terminating. `sigma_y` 12 walks the full
  31.6M cells in 460 to 660 ms and
  then refuses `NoOpenExposure`. `GeneratorScalars::mean_event_duration_s` has
  the same gap - validated strictly-positive-finite only - and reaches the same
  traversal at 1e6. Every other arrival family's parameters carry two-sided
  ranges and every other family's latent has a floor; these two are the odd ones
  out. The fix is an ADMISSION bound in `ArrivalConfig::is_valid` and
  `GeneratorScalars::validate`, which moves no byte of any tape a bounded config
  produces and needs no chart gate. NOT LANDED because refusing a config that
  today works is a product decision. Note the blast radius is small: no shipped
  preset declares an arrival family at all, so this is reachable only from an
  operator `generator.arrival` override and from the lab's screen. Filed
  2026-08-19 by the `bugs-data` round-3 fix pass; the measured table and the
  recommendation are `reference/performance.md`, "The arrival kernel's cost
  cliff".

- `SegmentSource` OVERRIDES NEITHER `seek_to` NOR `fault`, so an effectively
  infinite source inherits the O(distance) default walk `mogwai-data`'s own
  `TickSource` doc warns about - the same shape `GeneratedSource` needed
  `CheckpointIndex` for. Harmless today because the only consumer,
  `mogwai segments tape`, walks forward from the origin and never seeks; it
  becomes a hang the moment anything serves a composed river or asks it for a
  window. `fault` is the harder half: the composer's one terminal condition,
  clock exhaustion, has no `TickFault` variant, and adding one ripples into
  `mogwai-server`'s `http.rs` fault rendering. Today it is reported through the
  inherent `SegmentSource::clock_exhausted`, which a `dyn TickSource` consumer
  cannot see. Filed 2026-08-19 by the `bugs-data` round-2 fix pass; the residual
  sub-item of that document's finding 4.
  THE SAME ITEM OWNS `emit_price`'s PANIC. It replaced a silent one-tick print
  with a named panic, which is right for a today-unreachable band breach in an
  offline dump - but it panics inside `TickSource::next_tick`, in a library
  crate, where `GeneratedSource`'s equivalent failures go through `TickFault`.
  The moment a composed river is served, that panic is a serving-path abort.
  Giving the composer a `TickFault` closes both halves at once.
- COMPOSED PRICES CAN SIT ON A RAIL WITH NOBODY WATCHING. The composer now
  clamps its running level to `[tick, MID_CEILING]` and counts the hits
  (`SegmentSource::clamps`, printed as `composed_price_clamps=` by
  `mogwai segments tape`), but nothing REFUSES a tape that spent most of its
  length pinned - and past the first clamp the printed prices are the rail
  rather than the integrated walk, which is a different tape than the one the
  library describes. The counter is the observable; the policy question -
  whether a long clamped run is a bad library, a bad `start_price`, or a
  legitimate deep drawdown - is an owner call and has not been made. Filed
  2026-08-19 alongside the clamp itself.
- NOTHING ROUTES A NEW WALL-CLOCK BUDGET INTO THE `timing` SWEEP, and the
  workspace has already paid for this shape once. `brokkr.toml` states the
  standing policy for a latency assertion - `#[ignore]` at the source, an entry
  in the gate's `skip`, and a name in the `timing` sweep's `only`, because "a
  latency assertion in a dev lane does not produce a weaker result: it produces a
  meaningless one" - and `tape_lateness_under_acceleration` was deleted for
  violating it. The enforcement that exists,
  `every_release_only_filter_is_skipped_by_the_gate`, runs the check in ONE
  DIRECTION: every test the `only` filter catches must be gate-skipped. The
  converse - every test carrying a wall-clock budget appears in some `only`
  filter - is not checkable from the outside, because "carries a budget" is not a
  syntactic property, and a plain `#[test]` asserting 50 ms in the parallel dev
  lane is therefore admitted silently. Filed 2026-08-19 by the `bugs-protocol`
  round-3 fix pass, whose cold review caught exactly that test on the way in;
  that instance was closed by INFLATING THE INTERVAL under test rather than by
  routing it, which is the better answer whenever the quantity is a parameter and
  no answer at all when it is not. Same family as the standing item on durable
  prose asserting a live fact: caught by habit or not at all. An owner-level
  question if anyone wants it mechanised - a source scan for `Duration::from_`
  inside an `assert!` would be the crude form, and would have to justify its
  false-positive rate against a repository full of legitimate loose bounds.

- A CONFIGURED INSTRUMENT MAY CARRY A SYMBOL ORDER ENTRY NOW REFUSES. Filed
  2026-08-19 by the `bugs-protocol` round-4 fix-and-commit pass, which closed the
  order-entry half and stopped there. `validate_submit_order` runs
  `validate_wire_symbol` as of that commit, so a client-inbound symbol is 1 to 32
  bytes of the URL-safe alphabet; `mogwai-server`'s `config.rs` checks an
  instrument's own `symbol` for non-empty and `MAX_SYMBOL_LEN` ONLY (it does run
  `validate_wire_symbol` on `index_symbol`), so a config naming `MNQ!` loads, is
  served, and cannot be ordered. Nothing in this tree does that - every shipped
  preset and test config is inside the alphabet - so this is a latent
  inconsistency rather than a live defect.
  IT IS AN OWNER-LEVEL QUESTION BECAUSE THE ANSWER IS A POLICY: the alphabet
  exists so a symbol can be concatenated into a URL without percent encoding, and
  `AGENTS.md` says the instrument set is OPEN and the venue does not gate on it.
  Tightening config to the same alphabet makes one sentence true everywhere and
  narrows what an operator may list; leaving it means the venue can serve a tape
  under a symbol no client can trade or fetch over HTTP. Whichever way it goes,
  the two validators should stop being able to disagree silently.
  THE SURFACE WIDENED ON 2026-08-19 and the defect did not change: the engine's
  `on_submit_group` now CALLS `mogwai_protocol::validate_submit_group`, which
  reaches the same wire-symbol alphabet, so a group naming such a symbol is now
  refused WHOLE at the engine boundary rather than member by member at the
  server's. Same policy question, one more caller; nothing here is a reason to
  answer it differently.

- THE LAUNCHER KILLS ONE PROCESS, NOT A PROCESS GROUP, so a venue with any
  descendant leaks a reader thread on the readiness-timeout path. Filed
  2026-08-19 by the `bugs-protocol` round-5 fix pass, carrying forward what
  finding D of that report left explicitly open.

  `launch`'s timeout arm issues `child.kill()` and then does NOT join the
  readiness reader, which is what makes the readiness bound unconditional. The
  kill closes stdout only while the CHILD holds the write end: a `binary`
  naming a wrapper script that starts the venue without `exec`, or a venue that
  ever grows a helper subprocess, leaves a grandchild holding an inherited copy,
  so `read_until` never returns and that thread is stranded for the life of the
  process. One leaked thread per timed-out launch is the deliberate trade -
  reporting the timeout beats hanging inside it - but it is a trade, not a fix.

  The robust form is putting the child in its own process group and `killpg`ing
  it, which also collects a helper the venue itself spawned. It does NOT help
  against a wrapper's grandchild that has left the group, so it narrows the hole
  rather than closing it, and it is a real change to the launcher's process
  model rather than a line. Nothing in this tree forks today - `mogwai serve`
  spawns nothing - so this is latent, and `docs/cli.md` states the supported
  shape (name the binary, or a wrapper that `exec`s it) and what it costs when a
  caller does not.

- FOUR REFUSAL MESSAGES SPELL THE DIVERGENCE CEILING AS A LITERAL. Filed
  2026-08-19 by the `bugs-protocol` round-5 fix-and-commit pass, which found it
  while spot-checking that report's cleared items and did not fix it - the
  `bugs-*` arc has no remaining document scoped to `mogwai-protocol`, so it has
  nowhere else to land.

  `havoc::validate_divergence` writes "3600000" into the text of four refusals
  (the `DelayAcks`/`GoDark`/`StallData` arm, `CommandLatency`, `FlowSurge` and
  `FeeSurcharge`) while `control::MAX_DIVERGENCE_MS` is the constant the check
  itself compares against. The constant IS 3_600_000 today, so nothing is wrong
  and there is no bug to close - the point is that changing the constant leaves
  four operator-facing messages naming the old ceiling, and NOTHING DETECTS IT:
  a live fact asserted inside a string literal is invisible to the prose gate
  and to the compiler alike. Cheap fix whenever anyone next edits those arms -
  interpolate the constant - and the same shape as the `bugs-tests-lab-cli`
  item about a refusal text hardcoding its cap.

  COUNT THEM AT THE PRODUCTION SITES. The first filing said five, from a raw
  match over the file: the module's tests carry the same four strings as
  expected values, so grep reports eight.

  A SECOND INSTANCE, found by the close pass over the same arc and worth fixing
  in the same edit: `messages::validate_wire_symbol` refuses with "symbols are 1
  to 32 characters" while the bound it compares against is `MAX_SYMBOL_LEN`.
  It also says CHARACTERS where the check is `symbol.len()`, which is BYTES -
  harmless only because the alphabet arm below it admits ASCII alone, so the two
  cannot disagree today. This one is worth more than the divergence texts
  because round 4 of `bugs-protocol` routed ORDER ENTRY through this function,
  so the message is now what a client sees when its symbol is refused at the
  venue's front door. Both refusals return `&'static str`, so interpolating the
  constant means changing the return type or reaching for a `const` formatter -
  which is why neither was fixed in passing.

- NOTHING ON THE WIRE SAYS WHETHER A SUBMIT TOOK A MARKET READING, which forces
  one integration test to read the venue's LOG instead. Filed 2026-08-18 by the
  lifecycle-test fix pass.

  `market_reading` in `mogwai-server`'s command path takes a `MarketReading` for
  every submit and passes it to the engine. When `read_market` refuses - a cold
  volatility estimator, a truncated walk - the engine falls back to the order's
  stated price with no slippage and logs a WARN. Whether the reading happened is
  therefore invisible on the wire in one specific and important case: a
  PRICE-LESS market order, where the venue stamps the order with the last print
  either way, so the fill lands on the tape whether a reading was taken or not.
  That is exactly the path the venue used to get wrong (it returned early with a
  stamped price and no reading at all), and
  `serving::a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths`
  is the gate against it.

  WHAT THE VENUE WOULD HAVE TO SHIP: the reading's own instant, or a bare
  "reading taken" flag, on `OrderFilled`. Two things follow at once. The gate
  stops reading logs, and the adverse-slippage invariant - a market buy fills at
  or above the print the venue read - becomes an EXACT per-fill statement instead
  of the bracket it is now. The bracket exists only because the reading instant
  is unidentifiable from outside: it is neither the acceptance instant nor the
  fill instant, and `MarketReadingCache` buckets it further.

  WHAT WAS DONE INSTEAD. The test reads the venue's stderr through
  `common::Venue::log` - every venue the harness spawns is captured, under a
  PINNED `RUST_LOG`, and a conclusion drawn from an absence in that buffer owes
  `CapturedLog::await_positive_control` first - and scores each attempt on
  whether the WARN named its client order id. On the priced arm the log and the
  fill price are
  cross-checked against each other, so neither observable is trusted alone. It
  works and it bites, but it keys a gate on a log line, which is not a contract.

- THE VENUE COUNTS SWEEP PASSES FOR NOBODY, which is the missing observable
  behind the last fixed sleeps in the item below. Filed 2026-08-18 by the
  lifecycle-test fix pass.

  Two integration tests need to wait until the fill sweeper has walked the book,
  and neither can state it.
  `serving::the_tape_is_identical_with_and_without_a_resting_stop` puts a hundred
  resting conditionals in the book and needs several passes to walk them, because
  the WALK - not the submit path - is what its purity claim is about;
  `serving::a_perpetual_position_pays_funding_across_an_interval` needs a pass to
  cross a funding instant and charge the position. Both waited on a bare
  `tokio::time::sleep`, which is a bet on the host rather than a condition: a
  venue that had stalled satisfies the sleep and is judged as if it had swept.

  WHAT THE VENUE WOULD HAVE TO SHIP: a monotonic count of completed sweep passes
  per river, readable from a route that already exists - a field on `/clock` or
  `/health` would do. A test could then read it, wait for it to advance by N, and
  say exactly what it waited for. Nothing on the wire carries it today; the
  sweeper is internal to `mogwai-server`'s fill path and emits no frame, no
  counter and no log a test can consume.

  WHAT WAS DONE INSTEAD. The resting-stop test polls `/clock?symbol=` until the
  boat's sim clock has advanced fifty simulated seconds, which on its `band.toml`
  venue at speed 100 is about half a wall second and roughly fifty opportunities
  at the 10 ms sweep cadence - and a stalled venue now FAILS there rather than
  passing. The funding test keeps its wall sleep: the same poll was implemented,
  run, and found vacuous on `perpetual.toml`, and the reason is a property of
  `speed = 0.0` now written down in `reference/clock.md`. Neither test can state
  "a sweep pass ran"; the first waits on a proxy that is only sound because that
  config's clock is wall-affine, and the second still bets on the host. Both are
  one server-side counter away from being conditions.

- TRIAGE EVERY TEST FOR PARALLEL SAFETY, AND KILL EVERY FIXED DURATION AND
  WAIT. Filed 2026-08-18, after `[test.profiles.gate]` took `test_threads = 8`
  and cut `brokkr check --gate` from 3m01s to about 1m00s. That setting is a
  measured compromise, not a resolution, and this item is the resolution.

  WHAT THE MEASUREMENT ACTUALLY FOUND. Serial, the gate spent 164s executing
  1,608 tests, and the top 20 of them were 54% of it while the other 1,451 came
  to 3.8s combined. Almost none of that concentration is computation. The
  lifecycle gates spend a declared `--duration` in WALL TIME - three of them
  cost 2.2s each because they ask for `2s` and then wait for it - and the
  reconnect ladders spend their attempts the same way. The genuinely CPU-bound
  tests are the tape walks, and they are the minority.

  WHY 8 AND NOT MORE, WHICH IS THE WHOLE REASON THIS ITEM EXISTS. At 16 the run
  goes RED, and not as a watchdog timeout on a starved test:
  `venue_announces_run_complete_and_exits_zero_at_the_declared_sim_deadline`
  fails on "the run announces its completion on the wire" at 2.016s against its
  usual 2.215s - it finished EARLY having never seen the frame. A test that
  asserts a timing contract fails as a WRONG ANSWER when the host is crowded, so
  the ceiling is set by our least robust test rather than by the machine. Every
  fixed wall-clock wait in the suite is a piece of that ceiling.

  THAT PARTICULAR CLIFF IS GONE as of 2026-08-19 - the declared-deadline family
  below no longer bets on a margin at all - but `test_threads` STAYS AT 8 until
  someone measures 16 again. One removed cause is not evidence that the cliff had
  only one, and this item's own closing note is that three green runs are not
  evidence about an intermittent race.

  THE WORK, and it is triage before it is repair. Go through every test and ask
  two things: can it run beside its siblings, and does it wait on a duration
  rather than on a CONDITION. A test that waits for a state to be reached, with
  a generous deadline as the failure path, is both parallel-safe and fast; a
  test that sleeps a fixed span is neither, and it silently prices the whole
  gate. The lifecycle family is the obvious start - `completion.rs`,
  `serving.rs`, `lifecycle.rs`, and the adapter's four socket binaries - but the
  sweep is EVERY test, because the point is a property of the suite rather than
  a fix to the tests we happen to have caught.

  WHAT THIS UNLOCKS. `test_threads` can then go to 0 (num_cpus) with the cliff
  gone rather than merely avoided, and the floor stops being the sum of a few
  tests' patience. Note the shape that is NOT the answer, already measured and
  rejected in `brokkr.toml`: a serial lane for the socket-backed tests has a
  floor of 74s, worse than the 53s the flat setting costs, because those tests
  are the best parallel citizens in the suite precisely BECAUSE they are idle.
  Fixing the waits is the only thing that actually moves the floor.

  THE PARKED LIST IS EMPTY, as of 2026-08-19. Both entries were
  `crates/mogwai-cli/tests/completion.rs` gates and both are un-parked: they are
  out of `brokkr.toml`'s `skip` list, they carry the ordinary
  `#[ignore = "binds a loopback listener"]` their neighbours do, and the gate
  runs them. Tests caught racing under parallel execution still get parked the
  same way while they are being fixed; there is simply nothing on the list today.

  THEY ALL HAD ONE SHAPE. The venue gets a fixed `--duration` measured from
  readiness - `serve.rs` sleeps `sim.wall_duration(remaining)` and then completes
  the run - and the launcher returns AT readiness, so the test connects into a
  span already running down. Under parallel execution the connect can lose, the
  run completes and closes before the socket exists, and the assertion fails on
  not having seen the frame: a WRONG ANSWER, not a timeout, which is why each one
  read like a real regression when it fired.

  WHAT ACTUALLY FIXED IT, because "wait for a condition instead of a fixed span"
  - what this item said before - does not apply here. There is no condition to
  wait for: the frame the test wants may already have been sent to nobody by the
  time the test exists, and no amount of patience recovers it. Two other answers
  are also wrong. A longer declared duration is a bigger margin, and a margin is
  precisely what a crowded host takes away. A passenger-scoped `?duration_ms=`
  does remove the race - that deadline starts at UPGRADE - but it closes one
  socket and leaves the run going, so it cannot express either property here:
  one gate is about the VENUE exiting 0 at its deadline, the other about the
  run-wide announcement reaching EVERY socket.

  What works is asserting the PREMISE and discarding the runs where it fails,
  and the premise is "this socket was a LIVE SESSION" rather than "this socket
  attached in time". The obvious phrasing was built first and is wrong: `ws.rs`
  evaluates `already_complete` when a session starts and announces to a socket
  that arrived after the run finished, so attaching late is served. What produces
  nothing is a connection accepted by a venue already tearing down, which never
  becomes a session at all, and the only evidence either way is the venue having
  written something on that socket. `watch_a_bounded_run` therefore launches,
  connects, drains every socket to completion, and throws the whole run away
  unless each saw at least one frame - within a retry budget that gives up naming
  the loss rather than blaming the venue. The test can then only ever make a
  statement about a run it was actually watching. The same treatment went to
  `run_complete_is_stamped_on_the_receiving_sockets_clock` and
  `a_short_accelerated_run_is_not_over_before_it_is_ready`, which had the
  identical shape and had simply not lost yet - the second of them is the
  tightest window in the family, 30 declared simulated seconds at speed 100
  being 0.3 s of wall.

  THE LOSERS ARE KEPT ALIVE rather than dropped as they are discarded, and that
  is not tidiness: `common`'s wall budget re-anchors when the last live venue
  goes away, so dropping one mid-test would restart the budget and push the
  ceiling past the hang watchdog.

  AND A SECOND, DEEPER CAUSE SURFACED THE MOMENT THE FAILURES BECAME TRUTHFUL,
  which is the argument for making them truthful. With the premise right the gate
  failed again, now saying the run announced nothing on a socket the venue had
  already served 1,475,111 frames on. `fast.toml` is `speed = 0.0`, so delivery
  is UNPACED and the run generates flat out for its whole span; `RunComplete` is
  written at the deadline and queued BEHIND that backlog, and a client draining
  about 111,000 frames a second cannot clear 1.4 million inside its wall budget.
  It had been passing only because two of the four family members were skipped.
  The family now runs on `tests/configs/bounded-run.toml`, which is `fast.toml`
  with `speed = 1.0`: nothing about `RunComplete` is a claim about unpaced
  delivery, so the firehose was pure cost. THIS IS THE GENERAL FORM AND IT IS NOT
  CONFINED TO THIS FAMILY: any test waiting for something the venue writes at the
  TAIL of a span of unpaced tape is on the same trap.

  AND NOTE HOW THESE WERE FOUND, because the method matters more than the two
  names. `test_threads = 8` went red after three green runs at 8, having already
  gone red at 16. Three passes are not evidence about an intermittent race; a
  failure RATE is. Anything parked or un-parked here needs repeated runs before
  it is called settled, and "the gate went green" is the weakest possible
  evidence for this class of defect.

  Check the fixed-path unit tests while triaging, too. Nothing collides today -
  every one writes a distinct `target/...` name and ports are kernel-assigned,
  which is why the flat setting was viable at all - but that is an unstated
  property currently held by convention, and one duplicated literal breaks it in
  a way that only shows up under load.

- OWNER GATE JUDGED 2026-08-18: SLICE 1 FAILED. The owner viewed the two Asia
  charts and rejected both as unusable - 300-point moves inside the session
  body over one-to-twenty-minute spans, which happen at an open and never in
  Asia session body. BOTH ARMS FAILED, including the gaps-OFF control, so the
  reopen-gap injection slice 1 built is not the cause. The full verdict, the
  measurements behind it and the two repairs it owes are in
  `notes/segment-sampler.md`; the probe is `analysis/asia_jump_probe.py`.

  THE GATE STANDS: nothing further is built on the composed tape - in
  particular the serving wiring, which is a real refactor (`CheckpointIndex` is
  typed on `GeneratedSource`, so a composed river means generalizing the
  checkpoint and resume path) and must not go ahead of a re-render that passes.

  Two repairs are owed before a re-render is worth the owner's eye: the seam
  LEVEL DISCONTINUITY that survives `--no-reopen-gaps` and contaminates the
  control, which is self-contained and ours; and whatever the cut admits at
  bars 1112-1113 of the Asia tape, which is carried in from the segment data
  and may be a cut-criteria question for the owner rather than a bug.

  FIVE CHARTS were rendered for the gate, all MNQ 2026-04 at seed 42. They are
  gitignored and live on the build host only, so they either get viewed there
  or regenerated locally. The three not yet judged - London and the two NY
  windows - are still unviewed, and the probe has not been run against them
  either, so whether the defect is Asia-specific or general is OPEN.

  | chart | what it is | bars |
  |---|---|---|
  | `analysis/out/asia-endless.html` | endless Asia, reopen gaps ON | 11,889 |
  | `analysis/out/asia-endless-nogaps.html` | the same tape, gaps OFF - the A/B | 11,889 |
  | `analysis/out/london-endless.html` | endless London | 8,237 |
  | `analysis/out/ny-morning-endless.html` | endless NY morning, 09:00 to lunch | 1,681 |
  | `analysis/out/ny-afternoon-endless.html` | endless NY afternoon, 10:30 to close | 2,824 |

  START WITH THE FIRST TWO SIDE BY SIDE. They are the same segments, the same
  seed, differing only in whether the measured reopen gap lands at each seam,
  so the difference between them IS the feature this slice was built for.

  Regenerating one, end to end - substitute the window name and the library
  path for the others:

  ```text
  brokkr run mogwai -- segments cut --symbol MNQ --month 2026-04 --window asia --out analysis/out/asia-mnq-2026-04.json
  brokkr run mogwai -- segments tape --library analysis/out/asia-mnq-2026-04.json --type bars --interval-s 60 --ticks 3000000 --seed 42 --out analysis/out/asia-endless.csv
  python3 analysis/plot_tape.py --csv analysis/out/asia-endless.csv --out analysis/out/asia-endless.html --title "Endless Asia, MNQ 2026-04 segments, seed 42"
  ```

  The cut needs the delivered corpus at
  `research/market-data/databento/mnqv/2026-04.manifest.tbbo`, which is out of
  git. What April yielded per window: asia 22 segments and 2,976,377 ticks,
  london 22 and 2,492,576, ny-morning 21 and 8,396,328, ny-afternoon 21 and
  9,572,450.

  WHAT TO LOOK FOR, against the observed defect list in
  `notes/segment-sampler.md`. Defect 2 is the one this slice was meant to fix:
  reopen gaps should now be VISIBLE at the segment seams, drawn from real
  measurements rather than a model - the largest measured gap in April is
  -1.31 %, about 261 points at a 20,000 level, which is the same scale as the
  owner's 2026-04-10 example. The A/B is the same compose with
  `--no-reopen-gaps`, which yields a continuous tape with no gaps at all, the
  way the fitted generator behaves today. Worth judging at the same time:
  whether the within-session texture reads as real, since carrying that texture
  from real data is the whole argument for resampled segments over generated
  ones.

  Times in these charts are NOT wall-clock UTC: a composed tape starts at unix
  ns 0 and elides the hours between one session and the next, so the x axis is
  composed tape time, not a calendar. One Asia segment is about nine hours of
  it, one NY-morning segment three.

  A LATERAL FINDING WORTH THE OWNER'S EYE, surfaced by cutting all four
  windows. The per-hour trade density across windows is enormously uneven in
  the real data: NY afternoon carries 9,572,450 ticks in 5.5 hours against
  Asia's 2,976,377 in 9, which is roughly a 5x ratio per hour. That is the
  observed shape of defect 3 - the suspicion that generated volume looks
  uniform across all sessions. Measuring the generator against this was
  explicitly waived by the owner on 2026-08-18 and is NOT being carried as
  work; it is recorded only because the segment cut produced the observed side
  of the comparison for free, and a future decision about the session profile
  would otherwise have to buy it again.

- NAUTILUS HAS NO CHANNEL FOR A DECLARED FEED GAP, so mogwai's `FeedLagged`
  can only reach a host as a log line. CROSS-REPO, and written for a reader who
  has not seen the bug loop.
  The MOGWAI venue tells a client, explicitly and with a count, when it dropped
  market-data or execution frames for that client: `ServerMessage::FeedLagged`
  carries `skipped` and `sim_now_ns`. That is a strictly better signal than
  most real venues give, and the adapter has nowhere to put it. Nautilus's
  `DataEvent` enum (`common/src/messages/mod.rs`) is `Response`, `Data`,
  `Instrument`, `FundingRate`, `InstrumentStatus`, `OptionGreeks` and an
  optional DeFi variant - none of them means "the stream you are aggregating
  has a hole". The client itself is handed to the host boxed as
  `dyn DataClient`, so an adapter-owned counter or health accessor is
  unreachable, and `is_connected` is true throughout because the socket never
  broke. Fabricating an `InstrumentStatus` would report a venue halt that did
  not happen, and tearing the socket down to force a visible failure would turn
  a recoverable gap into an outage.
  WHY IT MATTERS HERE: bar aggregation over the missing span is silently wrong,
  and the polling cursor resumes past it, so a strategy cannot distinguish a
  quiet market from a dropped one. On the execution socket the same drop can
  take order events, leaving the nautilus order state disagreeing with venue
  truth until something calls the reconciliation generators.
  WHY THE EXECUTION SOCKET CANNOT SELF-HEAL, which is the part that is NOT
  inherited from the market-data case. `ExecutionEvent` does carry a
  `Report(ExecutionReport)` variant, so a client CAN push a report at the
  engine without waiting to be asked - the missing channel is not the emitter.
  What is missing is a truthful report to push. Every truthful order, fill and
  position set in this adapter comes from an ASYNCHRONOUS venue-truth query
  (`QueryOrders` / `QueryFills` over the same socket, or `GET /account`), and
  the frame translator that sees `FeedLagged` runs as `handler(msg).await`
  INSIDE the reader's own frame loop (`lifecycle.rs`): the reply to a query
  issued there can only be read by the loop that is awaiting the handler, so
  it deadlocks by construction. Spawning the query off the handler is
  unavailable too - the client owns `Rc<RefCell<Cache>>` through
  `ExecutionClientCore` and is `!Send`, so it cannot be moved into a task, and
  the reader task holds no `&self`. Fabricating a report from the local mirror
  would be the exact falsehood the venue-truth move (AE10) removed: the mirror
  is built from the very frames the venue just said it dropped.
  LOCAL MITIGATION SHIPPED: both message translators log at ERROR with the
  skipped count and the simulated instant, in the words a host can alert on,
  and the execution socket's whole-batch admission refusal
  (`AdmissionSubject::Frame`, an outbound batch the venue discarded) logs at
  ERROR with the same reconcile wording.
  THE UPSTREAM HALF, stated as what nautilus would have to ship: (1) a
  degradation signal on the DATA side - a `DataEvent` variant or a
  `DataClient` health callback the engine surfaces - so a gap is an event
  rather than a log line; and (2) on the EXECUTION side, a client-initiated
  reconciliation request the ExecutionEngine services on the client's behalf -
  something like a `RequestReconciliation` message, or an `ExecutionClient`
  callback the engine polls - so the adapter can say "my mirror is suspect,
  re-run mass status" without owning an async handle to itself. Either one
  alone closes half of this. Until they exist, a host driving MOGWAI should
  treat an ERROR from `mogwai-adapter` mentioning a feed gap or a refused
  frame as a reconcile-and-distrust-the-window signal. broadarrow is the known
  consumer.

- THE SYMBOL IS A REQUEST PARAMETER, NOT AN IDENTITY THE VENUE OWNS. Converged
  with the owner 2026-08-15, and it supersedes every earlier framing of this
  item, including "serve N instruments from one venue", which described the
  wrong shape of work.
  THE MODEL. A client asks for a symbol. Mogwai accepts ANY STRING and serves a
  tape for it. Nothing is admitted, declared, registered or added; the symbol is
  transient, ephemeral, and inconsequential to the machinery. If the string
  HAPPENS TO MATCH a preset, that preset supplies the tape knobs. If it does not,
  the default knobs are used. A preset is nothing but a NAMED BUNDLE of knobs a
  user could set by hand, carrying no authority and conferring no status.
  THE PRECEDENCE, all three layers over the same knob set:
  default knobs < preset knobs, if the requested symbol names one < knobs the
  operator set explicitly. So `FOOBAR` configured with MNQ's values IS an
  MNQ-shaped tape called FOOBAR, and nothing distinguishes them; `MNQ` with two
  knobs set gets the preset for the rest and the operator's values for those two.
  WHAT THIS MAKES THE WORK. Mostly DELETION of a singular assumption, plus one
  new mechanism. What already exists and is shaped right: the preset resolution
  machinery with nesting (MES over MNQ), an override layer and provenance
  validation; `Engine::build` taking `instruments: Vec`; margin and fees keyed by
  symbol; `InstrumentProfiles` as a map; `/trades` and `/quotes` taking `symbol`.
  What has to change (items 1 through 3 landed in slice 1 - symbol-lookup preset
  selection, config overlays split from the boot symbol, and `InstrumentDef`
  derived through one resolution path; items 5, 7 and 8 landed with piece 7
  2026-08-15 - lazy engine registration, the keyed `Rivers` registry replacing
  `RunIndex`/`BOOT`, and `build_instrument_profiles` returning every configured
  shape instead of collapsing with `.next()`; item 4, seed derivation gains a
  symbol dimension, landed with piece 8 2026-08-15 - detail is git history,
  numbering left as assigned):
  NOT ON THE LIST, corrected 2026-08-15: THE SWEEPER IS ALREADY SYMBOL-KEYED and
  slice 1 needs to touch none of it. It groups pending scans into a map by symbol
  and walks once per symbol, and marks and settlements look each symbol up in
  `profiles`. An earlier draft called this the lowest-confidence item and the
  likeliest place a hidden assumption would bite; the survey found the opposite.
  LANDED in piece 11, 2026-08-16: the fill path no longer retains this
  single-symbol seam. Each boat owns its river's `MarketReadingCache`, so two
  symbols neither evict one another nor serialize their unrelated walks.
  `last_swept_ns` no longer describes this - piece 9 moved the sweep
  watermark onto the boat (`Boat::last_swept_ns`) and piece 10 confirmed it,
  so settlement liveness is per boat, not process-wide; this paragraph's
  earlier claim that it was one all-or-nothing watermark coupling every
  symbol's settlement frontier is stale.
  SEQUENCING, two slices that fail differently. SLICE 1, symbol selects the
  preset, still one symbol per run: proves the lookup, the derived
  `InstrumentDef` and the default-shape path, with no concurrency and no version
  bump. SLICE 2, many tapes at once: lifecycle, fanout topology and the seed
  dimension, where the bump and the real risk live.
  THE SLICE 1 ORDERING PROBLEM, and it is structural rather than a detail:
  `INDEX` is initialized inside `materialize_warmup` AT BOOT, from config, BEFORE
  ANY REQUEST EXISTS. If the symbol arrives per-request, either warmup moves or a
  boot symbol must still be chosen up front. RULED 2026-08-15: slice 1 keeps
  a boot symbol in config; warmup and `INDEX` initialize from it unchanged;
  moving warmup is slice-2 work. See piece 4 of the fourteen-piece inventory.
  CONSUMER-SIDE, unchanged and still theirs: `run_prep::mogwai_facts` refuses a
  `/instruments` answer of anything but exactly one instrument, so a venue
  serving many BREAKS their build loudly by design, closed on their side by
  selecting on the strategy's frontmatter symbol.
  PRIORITY: broadarrow has landed both of its halves of the route to the
  strategy-search end state, so this is the sole remaining blocker rather than
  one ask among many. See the consumer-context section below.

- THE RIVER AND THE PASSENGER: how one tape serves N traders. SETTLED with the
  owner 2026-08-15, and CORRECTED 2026-08-16 after the metaphor was found to
  have been misapplied in this file - see the noun correction below, which
  supersedes every "boat" in the earlier drafts of this bullet.
  A RIVER is a deterministic path, fully determined by seed, symbol and the
  resolved knob bundle INCLUDING COMPOSITION. It exists whether or not anyone is
  on it. A PASSENGER is one connected trader: its own account, its own ledger,
  its own orders, its own view of the river. Mogwai is the EXCHANGE, serving
  many passengers across many rivers.
  THE BOAT IS NOT A DOMAIN NOUN. It is a shared cursor - one position on a
  river, one clock, one pacing thread, one materialization of the tape - and its
  only purpose is amortization: generate and pace once rather than N times.
  Semantically it is void, because the tape is deterministic and exogenous, so
  two passengers at the same sim-time on the same river see identical water
  whether or not they share a hull. Nothing a client can measure reveals whether
  it has company. Keep the boat in `boatyard.rs` and out of any durable prose:
  every place this file previously reasoned about which passengers may share a
  boat was reasoning about a cache as though it carried meaning.
  RIVER IDENTITY is the resolved knob bundle plus the seed plus GENERATOR-level
  havoc - everything that mutates the water. Composition is IN it: two agents
  asking for MNQ want different rivers if one wants the Asia loop and the other
  wants post-lunch, so the identity is the resolved bundle and never the symbol
  string. Speed is NOT in it, and never was: it changes delivery cadence, never
  a single generated value. Speed is a property of the shared-cursor cache key,
  which is why a passenger asking for a speed nobody is serving is a CACHE MISS
  and must never be a refusal.
  THE TWO PROPERTIES A PASSENGER IS OWED, and they are distinct - one holds
  today and one does not.
  NON-INTERFERENCE: no passenger's orders can affect another's. The tape moves
  as it moves; order flow does not feed back into it. HOLDS TODAY, because the
  tape is EXOGENOUS - generated, never order-driven. This is load-bearing and
  easy to destroy by accident: if mogwai ever modelled market impact, passengers
  would reach one another through the water and nothing else in this design
  would save it. The consequence is a CONTRACT rather than a limitation: there
  is no queue competition, so fifty agents submitting the same buy at the same
  instant all get the same fill and their aggregate moves nothing.
  INVISIBILITY: no passenger can observe that another exists. DOES NOT HOLD
  TODAY, and it is a defect against the end state rather than a property to
  document. Verified in this checkout on 2026-08-16, three channels: the fill
  sweeper walks `run.bound_lanes()` with no submitting-connection filter, so
  every connection receives `OrderFilled` for orders it never submitted; the
  engine holds ONE `account_id` and ONE `Account` for the whole process, so one
  passenger's fills move another's balance and net position and `GET /account`
  shows it; and the order-query surface answers over the same unscoped book.
  Non-interference does not imply invisibility, and conflating them is what let
  this stand - the water is clean while the ledger and the mailbox are common.
  Under the default per-run mode this costs nothing, since one connection has no
  second account to be confused with, so the defect is latent rather than
  harmful today.
  HAVOC SPLITS ALONG THE SAME LINE. `VolStorm`, `FlowSurge` and
  `LiquidityDrought` reach the generator, so they FORK THE RIVER - that agent
  needs its own water, not merely its own view. Drop, duplicate, reorder and
  latency are applied at the socket, so they are the passenger's own EYESIGHT:
  same river, same water, blurry glasses, no isolation needed. Since forward
  testers overwhelmingly want transport havoc, sharing is the common case rather
  than the exotic one.
  ONE CLOCK PER RIVER, not one per run. A cursor launches at its river's origin
  and walks forward; it never seeks. The objection that a client would see two
  symbols at different simulated times is void, because STRATEGIES ARE
  SINGLE-INSTRUMENT by settled premise, so no observer ever holds two clocks.
  This also deletes the catch-up burst a late-anchored worker performs against
  an already-advancing clock.
  WHERE A PASSENGER IS PLACED. A client may ask for a DURATION or for infinite,
  and MAY NOT ask for a start or end time. Where it enters the tape is the PHASE
  question ruled open below; the interim posture is that every passenger gets
  its own river from the origin.
  THE COMPOSITION IS PART OF THE REQUEST AND PART OF THE KEY. A request naming
  only a symbol and a duration - "give me MNQ, speed 60, for 30 minutes" - does
  not say WHICH THIRTY MINUTES of what tape, so it leaves the venue to choose
  the composition on the client's behalf. The answerable form names what the
  water is: the tape resolved from the MNQ PRESET, looping the Asia footprint
  from 8pm EST to London open, at speed 60, for 30 days. Duration stays OUTSIDE
  the key: it is how long you ride, not what you ride.
  WHETHER JOINING MID-TAPE IS SANE IS A PROPERTY OF THE COMPOSITION, not a
  venue rule. A looping session footprint - Asia, 10:30 to lunch, post-lunch to
  close, BTCUSD Monday to Friday on infinite loop - is the shape where joining
  anywhere is fine. These are what get requested overwhelmingly, and the
  segment-sampler track is what builds them. A full-calendar linear tape with
  real overnight structure and NY-open bursts is not, and nobody joins one
  mid-tape; they request a fresh river. An earlier draft of this bullet argued
  from that second kind that mid-tape placement was hazardous in general. That
  was arguing from a tape model the segment sampler is replacing, and it does
  not generalize.
  WHAT MAKES A LOOP JOINABLE IS PHASE-OVER-PERIOD, NOT HOMOGENEITY, and this is
  worth stating because "homogeneous" overstates it. A loop is not homogeneous
  instant by instant: join halfway through an Asia session and your first
  session is truncated. It is joinable when the LOOP PERIOD IS SMALL RELATIVE TO
  THE RIDE - thirty days of a one-day loop makes the entry phase a rounding
  error, thirty minutes of a one-week loop makes it the whole experiment. That
  ratio is computable from things the venue already holds, since the sampler
  DECLARES the footprint's period and the request carries the duration, so the
  venue never needs to know that an NY-open burst matters to a strategy. The
  non-looping tape is the degenerate case rather than a special rule: infinite
  period, ratio zero, never joinable.
  OPEN, AND DELIBERATELY NOT ANSWERED (owner, 2026-08-16). Whose call phase is -
  the user's as a request knob, the venue's from the ratio, or the
  COMPOSITION'S, declared by whoever authored the footprint and therefore
  knows what it is for - cannot be settled until the tapes mature enough for
  the question to have a real answer. Until then the interim posture is ONE
  RIVER PER PASSENGER: no mid-tape joining, no sharing, no threshold anybody
  has to defend.
  THIS DOES NOT PARK THE PASSENGER WORK, and the distinction is the useful one
  to carry: SHARING IS AN OPTIMIZATION, THE PASSENGER IS THE MODEL. A
  per-passenger ledger, the fill filter, per-passenger transport havoc and the
  order-query scoping are identical work whether a river carries one passenger
  or fifty. All of it can land under the interim posture and none of it depends
  on how phase is eventually ruled.
  LANDED in piece 9, narrowing this design in two owner-adjudicated ways: one
  shared cursor per river, with a loud refusal on a differing key rather than a
  second cursor (a subscriber naming a sitting river's speed differently is a
  400 naming the sitting speed); and runtime generator-level havoc (`VolStorm`,
  `FlowSurge`, `LiquidityDrought`) REFUSED on a river carrying a seated cursor,
  in favor of forking the river at PLACEMENT - the design already said
  generator havoc forks the river, and this landing makes river identity, not
  a runtime mutation of shared water, the sole carrier of that fork. Also
  landed: one OS pacing thread and one resized ring per cursor; no river
  eviction; and ticket-owned last-passenger teardown. Both narrowings are
  reversible: distinct-speed cohabitation by keying seats by cursor and adding
  per-cursor temporal ownership of orders and marks to the engine (the real
  prerequisite); mid-run generator havoc on shared water by replaying
  immutable control history from a boundary at or before every live cursor,
  never by mutating the lead. See the history ledger for the adjudication's
  full grounds; the spec that recorded it is retired.
  THE SPEED REFUSAL WAS AN ARTIFACT and is gone: seats are keyed by
  (river, speed), an unserved speed is a second cursor, and the sweeper
  applies a boat's walk only to passengers seated on it. One ledger still
  carries one cadence, so a second socket on the same account asking for a
  different speed of a river it is already riding is refused. That is the
  ledger constraint, not the cache showing through.
  CLOSED 2026-08-16, as the second of those two: the venue refuses to leave
  orders resting on a river nobody is reading. An ATTACHED account's order on a
  symbol no cursor walks is cancelled at the sweep pass; a FROZEN account is
  skipped wholesale and its book kept for the socket expected back. See
  `reference/architecture.md`.
  LANDED in piece 10, 2026-08-16: every remaining run-level singleton that
  assumed one notion of now - engine event stamps, `/clock`, the history
  ceilings on `/trades` and `/quotes`, the pulled `/account` snapshot, the
  `RunComplete` stamps, and the fill sweeper's cadence - now resolves through
  a boated river's own boat clock, with a labeled venue clock kept for the
  answers that have no boat (a boatless river, the venue deadline, the
  venue-scoped account ledger). Detail is git history; the durable statement
  is `reference/clock.md`, `reference/architecture.md`, `docs/havoc.md` and
  `docs/cli.md`. Piece 11 has since moved `MarketReadingCache` onto each boat;
  piece 12 has since resolved the readiness record as venue-scoped, and the
  boatless-river sweep gap above is explicitly still open too.

- SYMBOL RESOLUTION IS TOTAL, AND THE DEFAULT PRESET IS THE SHAPE CONTRACT.
  Settled 2026-08-15. A requested symbol resolves in three steps and step three
  never fails:
  1. Knobs the operator explicitly configured for that symbol win.
  2. Otherwise, a preset whose NAME MATCHES the symbol supplies the shape - this
     is the `MNQ` case, where asking for MNQ gets the MNQ preset with no config
     saying so.
  3. Otherwise, the DESIGNATED DEFAULT TAPE PRESET supplies the shape and the
     result is labelled with the requested symbol - the `FOOBAR` case.
  The default tape is ITSELF ONE OF THE PRESETS, not an invented fallback shape,
  and it is explicit about currency, price grid and class like any other. Which
  preset it will be is undecided - BTCUSD, BTCUSDT or BTCUSDT.P - and the choice
  changes nothing structural.
  THE SYMBOL NEVER CONTRIBUTES A CURRENCY. It is a label. Currencies, class,
  grid, multiplier and margin come from the resolved shape, so `FOOBAR` is the
  default preset's shape wearing a different name. An earlier draft here
  reasoned that an arbitrary string had no derivable quote currency, invented a
  default shape to supply one, and then proposed making it a FUTURE so that
  spot-sell base reservations would not make arbitrary symbols unshortable.
  All of that followed from the false premise and is void.
  CONSEQUENCE WORTH STATING WHERE THE DESIGNATION IS MADE: the default preset
  stops being merely the tape you get when you do not pick one and becomes the
  SHAPE CONTRACT for every unnamed symbol. Swapping it for tape reasons silently
  moves the currency, grid and class of every unmatched symbol, and therefore
  what the funding check below demands of the ledger.

- THE PASSENGER CARRIES AN ACCOUNT POLICY, AND MOGWAI ENFORCES IT. Ruled by the
  owner 2026-08-16, and it supersedes the funding item below: the venue-level
  `[balances]` seed DIES, and a connecting client must NAME its account. This is
  the largest consequence of the passenger becoming an object, and it is a much
  bigger surface than "add an opening balance to the request".
  WHAT A CLIENT MUST NAME, at minimum: its ACCOUNT ID, its starting balance, its
  daily loss limit, its autoliquidation threshold, and its DRAWDOWN MECHANICS -
  which variant, and how the number is computed.
  THE ACCOUNT ID IS THE CLIENT'S, NOT MINTED, and the reason is RECONNECTION.
  Ruled 2026-08-16 after minting per connection was considered and refused. From
  the venue's side a reconnect is a socket it was serving going away and a new
  socket arriving that claims to be the same client, and the venue CANNOT
  distinguish the two causes: a dropped socket the adapter redialed - which the
  adapter does today, and which an armed `GoDark` produces deliberately - or a
  client process that died and restarted against a still-running venue, which is
  exactly what broadarrow's restart run stages. If the id were born with the
  socket, case one would silently open a NEW account with a fresh balance, no
  positions and a reset peak equity, so a hiccup would wipe a run's P and L. A
  stable client-supplied id is what makes a returning socket a continuation.
  This is also what the restart run and the `go_live` restart de-duplication
  scenarios already assume: the account outlives the connection.
  A DEFAULT ID SURVIVES, and its scope is stated so it is not mistaken for the
  old venue-wide account: it exists for the EPHEMERAL TRANSIENT instance where
  exactly one client ever connects, which is the default per-run mode. One
  connection has nobody to collide with, so making it name an id would be
  ceremony. `DEFAULT_ACCOUNT_ID = "MOGWAI-001"` and the `ISSUER-NUMBER`
  validation carry over; what dies is the assumption that the config's id is THE
  account rather than a default for the unnamed case.
  AN ACCOUNT IS ON AT MOST ONE RIVER, ENFORCED BY EVICTION. Ruled 2026-08-16. A
  second socket presenting an id that is already seated KILLS THE INCUMBENT and
  then proceeds under the ledger-reset knob above - resuming the frozen account
  or starting it clean. This unifies reconnection and account-stealing into one
  mechanism, which is correct because the venue cannot distinguish them anyway:
  a second connection claiming an id IS a reconnect from the venue's side.
  The consequence worth stating: a trailing drawdown is therefore always
  computed over ONE instrument, and no ledger ever spans two rivers.
  ON RESUME, POSITIONS OFF THE JOINED RIVER ARE FLATTENED. The edge is a
  returning socket that names a DIFFERENT symbol than the frozen account was
  trading: carrying the old position forward would leave the account holding
  something the new session can neither see nor close. Flattening at resume is
  the ruling.
  ACCOUNT RESOLUTION IS TOTAL, EXACTLY LIKE SYMBOL RESOLUTION. Ruled 2026-08-16
  to close the hole the two rulings above left between them - if balances only
  ever come from the client, even the ephemeral client must POST before it can
  trade, and the default id buys it nothing. Three steps, and step three never
  fails:
  1. Knobs the client explicitly named win.
  2. Otherwise a POLICY PRESET whose name matches supplies the shape - the
     `apex-50k` case, asking for it by name and getting its rules with nothing
     else said.
  3. Otherwise the DESIGNATED DEFAULT ACCOUNT PRESET supplies the shape, under
     whatever account id was requested.
  So POSTING IS OPTIONAL rather than mandatory: a client that names nothing gets
  the default policy under the default id, and the ephemeral case is
  ceremony-free as intended.
  IT INHERITS THE SAME CONSEQUENCE the default tape preset carries, and it
  belongs wherever the designation is made: the default account preset stops
  being merely what you get when you do not pick and becomes the CONTRACT FOR
  EVERY UNNAMED ACCOUNT. Swapping it silently moves the opening balance and the
  risk rules of every run that did not name one.
  THE TWO DEFAULTS ARE COUPLED BY CURRENCY, which is not obvious and will bite
  whoever designates either one alone. The default TAPE preset fixes the
  settlement currency of every unmatched symbol; the default ACCOUNT preset
  fixes what an unnamed account is funded in. If they disagree, the wholly
  unnamed request - default symbol shape, default account - fails its own
  connect-time funding check, which is the one path that must never fail.
  Designating either is therefore a joint decision.
  THE TARGET IS PROP-FIRM FIDELITY. Funded-account rules are what a real
  deployment of these strategies has to survive, so a forward test that ignores
  them tests a different account than the one the strategy will trade. The
  variants that must be expressible, because they are exactly what firms
  advertise and differ on:
  - WHAT RATCHETS THE TRAILING THRESHOLD: intraday PEAK EQUITY including
    unrealized, which is the harsh and common form, or END-OF-DAY BALANCE only,
    which is much softer because an intraday spike that is given back never
    counts.
  - WHETHER THE TRAIL STOPS. Many firms trail only until the threshold reaches
    the starting balance plus a buffer and then LOCK it there; others trail for
    the life of the account.
  - THE DAILY LOSS LIMIT, a separate non-ratcheting floor measured from the
    day's opening equity and reset each session - not the same mechanism as the
    trailing drawdown and not derivable from it.
  - WHAT A DAY IS. THE ACCOUNT POLICY DEFINES IT, not the instrument. Ruled
    2026-08-16. A prop firm's reset is its own instant - 17:00 or 18:00 ET
    typically - and is a property of the account rather than of whatever is
    being traded, so it does not come from `[instrument.calendar]` even though
    that carries a settlement minute and real open windows.
    THE APPARENT COMPLICATION RESOLVES ITSELF. The policy names an instant on
    the tape's civil clock and the reset fires whenever SIM TIME CROSSES IT; a
    one-session loop crosses it once per loop, a multi-day loop as often as it
    contains it. No rule about loops is needed.
    THE EDGE THAT DOES NOT RESOLVE ITSELF, flagged rather than solved: a
    footprint that NEVER CONTAINS the named instant. An Asia-only loop, 8pm to
    3am ET, under a 17:00 ET reset never crosses it, so the daily budget never
    resets and a daily limit silently becomes a run-lifetime limit.
  THE MECHANIC WORTH RECORDING, because it is the case that motivated the
  ruling and it is not obvious: a passenger can end a day up 3,000 dollars and
  still have SPENT 700 dollars of drawdown budget. On a 50k account with a 2k
  trailing drawdown the threshold starts at 48k; if intraday equity peaks at
  53.7k the threshold ratchets to 51.7k; closing at 53.0k leaves 1.3k of room
  rather than 2k. The budget was spent on a peak that was touched and not held.
  Nothing about that is expressible without keeping PEAK EQUITY as durable
  per-passenger state, which is the point.
  DERIVED STATE THIS IMPLIES, per passenger, evolving on every mark: peak
  equity, day-open equity, and the current threshold. That sits directly beside
  the mark-to-market the futures margin ledger already performs, so the natural
  home is the same place - but it is per PASSENGER, where today's margin ledger
  is per process.
  PEAK EQUITY TRACKS EVERY TICK, not the sweeper's cadence. Ruled 2026-08-16.
  The ratchet is effectively tick-by-tick at a real venue, so a spike lasting
  200 ms still spends budget; sampled at the sweeper's sim-second cadence that
  spike is invisible and the account keeps room it should have lost, which is
  the difference between a run being liquidated and not. The cost objection
  raised against this was WRONG and is recorded so it is not raised again: an
  account holds ONE position, because strategies are single-instrument and an
  account is on at most one river, so per-tick marking is one multiply per tick
  rather than a walk over a book.
  ENFORCEMENT, not reporting. The venue flattens and locks a passenger that
  breaches, because a strategy that would have been liquidated must actually be
  liquidated or the forward claim is worth nothing.
  THIS IS A RISK-POLICY LAYER, NOT A PROP-FIRM FEATURE, and reading it the other
  way would build the wrong thing. A LIVE account has the same machinery: on
  Tradovate an operator sets "if I lose 200 dollars today, allow no further
  positions", which behaves exactly like a liquidation except that it lifts at
  the next session. A prop firm is that engine with stricter numbers and less
  forgiving breach actions, so there is ONE mechanism here and not two.
  A RULE IS THEREFORE A TRIPLE: what it measures, on what basis, and WHAT IT
  DOES ON BREACH. The breach action is the parameter that spans both worlds, and
  two values cover the known cases:
  - FLATTEN AND LOCK UNTIL THE NEXT SESSION BOUNDARY - Tradovate's daily loss
    limit, and most firms' daily limit. The passenger stays connected and
    unable to open, and resumes with a fresh daily budget.
  - FLATTEN AND TERMINATE - the prop trailing-drawdown breach, where the
    account is dead and there is no tomorrow.
  The existing `breach_action = "liquidate"` on the futures margin ledger is a
  third instance of the same triple, which is evidence the abstraction is right;
  reconciling the two liquidation paths means expressing margin breach as one
  more rule rather than keeping a parallel mechanism with its own arithmetic.
  A breach that TERMINATES gives a run an outcome that is neither a completed
  duration nor a venue fault, and it must reach the client as such on the wire.
  IN THE EPHEMERAL MODE A TERMINATING BREACH ENDS THE VENUE, since the run's one
  account is dead and there is nothing left to serve - the same "no client, no
  job" rule that governs disconnection. The terminal frame carries the final
  state, so nothing is lost by not staying up for a post-mortem pull.
  ACCOUNT POLICIES ARE PRESETS, and the argument is the instrument preset's
  argument unchanged: a named bundle of knobs a user could set by hand, carrying
  no authority and conferring no status. There are on the order of 300 prop
  firms, the large ones ship several account sizes with different trailing
  bases, and they change the rules without notice.
  SO REGISTRATION MUST BE A RUNTIME PATH, and this is the part that is NOT a
  copy of the instrument preset machinery. Instrument presets are COMPILED IN -
  `include_str!` against a fixed table in `config.rs` - so an operator can
  override a symbol's knobs in config but cannot add a NAMED preset without
  rebuilding the binary. For account policies that shape is wrong by
  construction, because nobody can track 300 firms in a release cycle. The
  shipped policies are embedded defaults; a user registers its own through a
  directory or config section read at boot, and a user-registered name shadows a
  shipped one.
  HOW A CLIENT NAMES IT: over the JSON HTTP CONTROL PLANE, following the
  precedent `POST /control/divergence` already sets - structured config goes
  over HTTP and is validated at its boundary, while `/ws` carries the streaming
  session. The owner has no preference on mechanism; this is the shape that
  fits. An account and its policy are a nested document that cannot go in a
  query string - `SocketQuery` is three scalar fields under
  `deny_unknown_fields` - so the account is POSTed and the socket carries only
  the ID, which the existing query string holds unchanged.
  A NOTE THAT COMES FREE: `arm_divergence`'s own comment today says a divergence
  "is armed against the RUN, so it reaches every open connection: there is no
  account to divert it onto." Once accounts exist that sentence stops being
  true, and transport havoc gains the target it was missing - the per-passenger
  havoc scoping recorded elsewhere in this file needs no separate mechanism.

  WHAT HAPPENS WHILE NOBODY IS CONNECTED. Ruled 2026-08-16, and it splits by
  mode because the two modes can do different things.
  MOGWAI'S JOB IS SERVING A CLIENT, so an unattended account is NOT kept
  running. Orders do not rest, the position does not mark, and the risk policy
  cannot liquidate somebody who is not there. This is a DELIBERATE DEPARTURE
  from a real venue, where being away is no defense against liquidation, and it
  is the right one here: mogwai exists to exercise a client's live path, not to
  simulate an account nobody is trading. The consequence to state in any claim
  is that a run spanning a disconnect has a GAP IN ITS RISK HISTORY.
  IN THE EPHEMERAL MODE THE QUESTION IS MOOT: the venue is owned by the run, so
  a client that goes away takes the venue with it and there is nothing left to
  resume.
  IN THE DURABLE `mogwai serve` MODE THE ACCOUNT FREEZES AND IS RESUMABLE by a
  client returning with the same id. That is what makes the two owed restart
  runs work at all - broadarrow's realized-PnL baseline leg and the `go_live`
  restart de-duplication both require the book to outlive the worker - and it
  means one subagent of fifty dying to a hiccup does not lose its work.
  A TTL BOUNDS IT, because a frozen account nobody reclaims is state with no
  lifecycle. Collected after a configured span; the span is a venue knob.
  THIS MUST BE LOUD, and stdout is the stated place. `serve` already emits ONE
  JSON READINESS LINE that the shipped launcher parses, so the persistence
  policy belongs as a FIELD IN THE READINESS RECORD rather than as a free-text
  log line a human has to notice: a launcher can then surface it and a consumer
  can assert on it. That is a `ReadyRecord::VERSION` bump. Pair it with a config
  key that opts out, so an operator wanting clean-slate reconnects says so
  explicitly and the record reports which way it is set.
  THE ACCOUNT ID IS A BEARER TOKEN under this, and the note is deliberate rather
  than an oversight: anyone who knows an id can claim that account. On a
  loopback venue serving one orchestrator's subagents that is acceptable and no
  auth is worth building, but it should be written down rather than assumed.
  NOT ANSWERED HERE: whether a HAVOC-INDUCED disconnect behaves differently. The
  venue does know the difference, since it armed the blackout itself and can
  infer the client is alive and merely blinded, and an argument exists that
  `GoDark` is toothless if the world stops while it is armed. Left open
  deliberately - the freeze-and-resume answer above covers it adequately for now
  and nothing is blocked on refining it.

  THE CONSUMER BREAKS, DELIBERATELY. Ruled 2026-08-16. broadarrow sets no
  `account_type`, inherits `MOGWAI-001` and POSTs no account, so every part of
  this lands as a breaking change on their side, including a run that can now
  end by LIQUIDATION rather than by duration or fault. No compatible default is
  built to keep an unchanged consumer working. This is the same handoff as the
  `/instruments` widening: a loud break is the designed signal, and a silent
  compatibility shim would hide from them that the account they are trading is
  no longer the one they thought.

  THE LEDGER REBUILD IS GENERAL, ruled 2026-08-16 in the same sitting. The
  account policy above is not the only thing the one-ledger-per-process model
  blocks, and since it forces a ledger rewrite anyway, the rewrite is scoped to
  what an exchange's ledger has to hold rather than to what spot and futures
  need today. THE TAPE SIDE IS A NON-ISSUE and should not be confused for the
  work: any symbol already gets a tape, so AAPL is a tape labelled AAPL. Every
  gap is in the LEDGER.
  WHAT IT MUST HOLD:
  - SHARES, not a currency balance. Modelling equity as `Spot { base: "AAPL",
    quote: "USD" }` puts AAPL in the ledger as MONEY, on the same footing as
    USD. That is right for crypto spot, where you genuinely hold the base
    asset, and wrong for equity. Downstream of it: share and round-lot
    conventions, settlement periods, short-sale locate or borrow.
  - LEVERAGE OUTSIDE FUTURES. Margin is per-contract on `Future` only, so a
    currency pair cannot post collateral and a forex strategy's position sizing
    - the part most likely to be wrong in a way that costs money - cannot be
    exercised. What is wanted is a genuine leveraged margin account: notional
    against posted collateral at a ratio. Forex, crypto margin, perpetuals and
    Reg-T equity margin all need the same one.
  - RECURRING FUNDING PAYMENTS. A perpetual pays funding between long and short
    at intervals, and mogwai has no mechanism for a periodic cash flow at all.
    A strategy holding a perp across funding instants has real cash movement the
    venue cannot produce, so its forward P and L is wrong by construction
    rather than by approximation. Inverse and coin-margined arithmetic rides
    with this.
  OPTIONS ARE OUT, on the owner's stated ground that they are not understood
  well enough to specify. Recorded as an exclusion rather than an oversight, and
  flagged as REVISITABLE for the same reason the order-type exclusions were
  reversed: "the owner does not need it" stopped being the scoping rule when
  mogwai went public. It stands until someone who understands them argues it.
  ALL THREE LANDED 2026-08-16, plus inverse. `InstrumentClass` gained `Equity`,
  `Perpetual` and `Inverse` beside `Spot` and `Future`, split by SETTLEMENT
  SHAPE rather than asset class because the shape is what decides how holding
  one moves the ledger. An equity credits a POSITION and never a balance, so its
  cash leg is spot's quote leg without the base credit, and an equity account is
  therefore statable in one currency where a spot account needs its base priced.
  A perpetual pays funding on notional AT THE MARK, on instants sitting at
  multiples of the interval from the unix epoch - a property of the CLOCK, so
  the schedule cannot depend on when a run booted or how the sweep passes were
  cut, and abutting spans never double-count. An inverse contract's value is
  `multiplier * qty / px`; `InstrumentDef::notional` and `::unrealized` are the
  ONE implementation of both forms, so realized and unrealized cannot disagree
  and a position's value cannot jump the moment it closes. `MarginBasis::Notional`
  is the leveraged account: a fraction of notional rather than a fixed amount per
  contract, which is what forex, crypto margin and Reg-T equity margin do.
  `is_marked` replaced `is_future` wherever the question was really "does this
  carry a marked position", which is now a wider set than "is this a derivative".
  ALL FIVE ARE CONFIGURABLE, not merely modelled: `[symbols.X.class]` takes
  `kind = "equity" | "perpetual" | "inverse"` beside the two it had, and
  `[symbols.X.margin]` takes `basis = "notional"`. `SizeGrid::from_def` now
  derives from the def's OWN sizing rather than from its class, which is what
  lets a perpetual size fractionally the way a real one does; that touched the
  generation path so `TAPE_PROTOCOL_VERSION` went to 19 and the Stage A manifest
  was re-blessed, though the change provably moves no existing tape (a future at
  increment 1 lands on the integral grid it had, spot at 1e-8 on `spot()`).
  THE EQUITY CONVENTIONS LANDED 2026-08-16: `lot_size`, `borrowable` and
  `settlement_ns` on the class, plus the cash-versus-margin distinction wired
  onto `MarginBasis::Notional` as Reg-T. Two live defects went with it - a funded
  equity account could not sell at all, and the maintenance walk read a
  notional-basis fraction as a per-contract amount. See `docs/config.md`.
  STILL OPEN ON THIS AXIS, none blocking:
  funding is paid on the fill sweeper's cadence so an instant is honoured on the
  pass that crosses it rather than at the instant itself; and nothing has been
  FITTED for any of the new classes, so a symbol configured as one is served the
  default tape wearing a different shape - the intake sequence is what makes a
  preset honest, and none has been run for equity, perps or inverse.
  LANDED 2026-08-16: the funding rate responds to the mark-versus-index basis
  when the class names an `index_symbol` whose river is already materialized;
  otherwise the configured `funding_rate` is the whole rate. Also landed: a
  static overall drawdown and a max-position cap on the risk policy, with two
  new shipped rulesets (`static-drawdown`, `intraday-trail-sized`) that use
  them. See `docs/config.md`.
  THE RISK STATE IS PUBLISHED, and the reason is EVALUATION rather than
  strategy consumption. Ruled 2026-08-16. A real trader reads its remaining
  drawdown off the firm's dashboard; mogwai presents no dashboard, so if the
  numbers are not on the wire nobody can judge the run afterwards - a strategy
  that ended flat having spent 90 percent of its trailing budget is a different
  result from one that never came close, and the two are indistinguishable from
  fills alone. Publish peak equity, the current threshold, remaining budget and
  the day's remaining loss allowance.
  THIS IS NOT `FeedLagged`-SHAPED, which is worth stating because it looks like
  it at first. That item is blocked because a signal must reach a RUNNING
  STRATEGY and nautilus has no typed channel for it. Risk state is for the
  EVALUATOR, so it needs no nautilus event type at all: `/account` is already an
  HTTP pull the consumer can poll directly. Mogwai can close this alone, and it
  does not owe broadarrow a nautilus-side change to do it.
  WHAT THAT LEAVES OPEN, and it is a smaller question: whether a strategy should
  ALSO be able to see its own budget in order to size against it. A strategy
  knows its own policy, since it named it at connect, so it can in principle
  derive peak and threshold from its own fills and marks - which means blind
  trading is workable and the venue publishing to the evaluator is not a
  half-measure. Decide it when a strategy actually wants it.
  INSTRUMENT PRESETS COULD GAIN THE SAME RUNTIME REGISTRATION, and the argument
  for it is identical - the compile-time table is only defensible while there
  are three of them. NOT RELEVANT NOW, recorded 2026-08-16 so the symmetry is
  not rediscovered later: it is not a precondition of anything above, and the
  account-policy path can land first and set the pattern.

- FUNDING: was CLOSED, REOPENED by the account-policy ruling, RE-CLOSED
  2026-08-16. The boot-checks-plus-bind-refusal design below assumed a
  venue-level `[balances]` that a client can now override per account, so the
  check MOVED rather than went away: a socket binding a symbol its own account
  holds no balance line for is refused before the upgrade, naming the account
  and the currency. Still knowable with no order at all, so still a
  CONFIGURATION error; and still PRESENCE rather than sufficiency, so a funds
  rejection on a served shape keeps meaning DEPLETION and only depletion. The
  boot-time barred set survives for the venue's configured shapes, which is what
  an unnamed account opens with. The original text follows.
  The set of
  shapes is still closed at boot: configured shapes, every embedded preset,
  and the default bundle under the instrument overlay.
  An arbitrary symbol does not open that set, because it contributes no
  currency.
  Configured shapes and the default bundle retain the existing boot refusal.
  Other embedded preset shapes are resolved at boot and marked FUNDING-BARRED
  when their currency has no ledger line; a request selecting one refuses at
  bind as a configuration error naming the symbol, currency, and `[balances]`
  key. A funds rejection on a served shape then means DEPLETION and only
  depletion. Collapsing the two would make a typo look
  like a trading outcome and waste an agent's whole run, which is the reason to
  keep them apart.
  The concern that this had to move to order time came from believing the
  currency set was runtime-discovered; it is not. And "the operator will just
  see it on their first order" is wrong
  for the mismatch case: an unfunded currency is knowable with no order at all,
  and only genuine depletion needs an order to discover. A user funds their
  ledger deliberately - nobody sets it to zero - so the case worth catching is
  incongruence, not absence.
  Confirmed: every runtime funds rejection names the currency, including the
  neighboring margin-breach refusal.

- EVERY DECISION IN THIS BLOCK OWES `reference/` AND `docs/` PROSE, and that is
  not a tidy-up at the end. These notes carry no truth guarantee and nothing
  durable may cite them, so a design that lands with its reasoning only here
  leaves a user blind to it: the symbol being a label rather than an identity,
  the three-step resolution and its total third step, river identity and what
  forks a river, one clock per river, the exogeneity that gives passengers
  non-interference and the no-queue-competition contract that follows, and the
  boot-versus-runtime split on funding. Durable prose states RIVER and
  PASSENGER and never the boat, which is a cache with no semantics; the two
  properties a passenger is owed are stated separately, since only one of them
  holds today. `docs/presets.md` and `docs/config.md` are where a user
  looks; `reference/architecture.md` is where the why belongs. Write the prose
  with the code that implements each decision, not in a documentation pass
  afterwards.

- `/instruments` RETURNS THE RESOLVED CONFIGURATION. Settled 2026-08-15,
  AMENDED 2026-08-16 by the owner's delegated ruling that subscribing to an
  unconfigured symbol is a supported client session, not merely a property
  of the generator. Not the servable set, which is unbounded and cannot be
  enumerated; not the presets. It reports the shapes THE OPERATOR CONFIGURED
  for this venue UNIONED WITH every shape a socket bind or a history poll
  has MATERIALIZED so far this run - a resolve that materializes nothing
  does not advertise. That still gives nautilus's `request_instruments` and
  `subscribe_instruments` a real, finite-per-instant answer; it grows
  exactly when the capped river resource is spent, which `docs/cli.md`
  states in those terms.
  LANDED: the adapter's `client/data.rs` no longer refuses a subscription
  for a symbol absent from its seeded set - that guard was right for a typo
  and wrong for a servable-but-unconfigured symbol, where the adapter would
  refuse on the venue's behalf what the venue would happily serve. Both
  clients instead reseed from `/instruments` after their socket binds,
  behind a readiness barrier that holds inbound delivery until the reseed
  completes, so no frame can reach a handler before the def it needs is
  cached. Socket and history resolution on the server are correspondingly
  TOTAL: an unconfigured symbol resolves to the default bundle wearing the
  requested label, and the `RiverKey` widens to that label in the same
  change, which is what makes sharing sound under it. Detail is git history.

- DECIDE whether the protocol-12b Stage A refinement pass should run at
  all. Deferred by the owner on 2026-08-09 rather than settled, so the
  frozen pass stands and the budgets were raised to fund it.
  The case for cutting it: refinement is 29,200 s of the 35,526 s Stage A
  cost model, 82 percent, and its entire product is a finer loss ORDERING
  over cells that Stage B then truncates to `STAGE_B_CELL_CAP = 24` per
  family. It also cannot rescue a family whose coarse admissible region is
  empty - which is the outcome that would close the landing - because it
  subdivides around that region's own boundary cells, so an empty region
  has nothing to subdivide around. And `SELECTION_INDIFFERENCE = 0.01`
  already declares losses inside that margin as not separating candidates,
  so a half-spacing lattice buys precision the selection is defined not to
  use. Cutting it drops Stage A to about 6,326 s, roughly 1.8 hours, and
  the budget question disappears rather than being negotiated.
  The case against: the selected parameter point would sit on the coarse
  lattice, and nobody has shown the coarse spacing is fine enough for the
  mechanism to be found at all. That is a real risk and it is why this is
  a ruling rather than an obvious cut.
  Note this is NOT the same question as `STAGE_B_CELL_CAP`, which earns
  its place: a Stage B cell is a full month-scale walk per seed at about
  250 s, so an uncapped 1,508-cell region genuinely is tens of hours.
  Changing `REFINEMENT_DEPTH` or `REFINEMENT_CELL_CAP` is a section 17
  amendment against the contract of record.

- RECONCILE the protocol-12b section 5.5 rescale with the shipped preset
  convention. That section freezes the negative control's re-centring as
  "rescale the 24 values to sum to 1, which the `SessionProfile` schema
  requires", and the schema requires no such thing: nothing in `config.rs`
  or `session.rs` enforces sum-to-one, and the shipped MNQ
  `intensity_hour` sums to 23.862306 - a mean-one curve, not a
  sum-to-one one. Found 2026-08-09 while writing the brick N spec, and
  the brick was implemented against the frozen text rather than around
  it. It moves no generated rate either way: `SessionModulator::new`
  divides by an exposure-weighted normalizer, so a common factor on
  every hour cancels at every instant, and the control's committed
  `new_curve` is therefore a correct re-centring on a different scale
  from its own `old_curve`. What it cost is readability - the drift
  figure the spec introduced so a uniform B6 offset would be
  interpretable had to be redefined scale-invariantly before it measured
  anything - and it will cost the same again at any later reader who
  compares the two curves elementwise. Fixing the frozen sentence is a
  section 17 amendment through review, not an edit, so it is recorded
  here rather than taken.

- ONE STRUCTURAL OBSERVATION from that diagnosis. Its sibling - publication
  order is not mutation order - was investigated 2026-08-18 and closed to a code
  comment: the invariant is real and unenforced, but it is unreachable by a
  margin the structure explains, so the finding is a NOTE AT THE SITE rather
  than a fix. Detail is the comment on the `submit_produced` call in
  `ws::dispatch_command`; the short version is that publication order is enqueue
  order, the sweeper must walk the tape before it can enqueue anything, and the
  competing stretch is three synchronous calls with no yield point - so the
  ordering is protected by timing rather than by design, and one `.await` added
  there would open it. `DelayAcks` cannot open it: the exec pump sleeps in-line
  and is head-of-line, so a delayed accept holds the fill behind it.
  (b) SWEEP BATCHES REACHED EVERY BOUND LANE with no submitting-connection
  ownership check, so a socket received `OrderFilled` for orders another
  connection submitted. Was recorded as a decision - contract to state, or
  filter to add - and DECIDED 2026-08-16 as a filter, since a passenger must be
  unable to observe that another exists.
  LANDED 2026-08-16: `ExecLanes` carries its own minted id, the run keys live
  orders to the connection that submitted them, and `deliver` attributes each
  order-scoped frame rather than broadcasting it. The ORDER-QUERY surface landed
  with it: `QueryOrders` and `QueryFills` are answered from the one book, as
  they must be while there is one ledger, and their rows are scoped to the
  asking connection on the way out. Venue-wide frames still reach every lane,
  and an unattributed order - one the VENUE originated, such as a margin
  liquidation - is still delivered and reported to all of them, which is the
  conservative direction: a stray row is the defect being closed, a MISSING fill
  would be worse.
  ONE DESIGN NOTE WORTH KEEPING, because the first shape of this was wrong: an
  ownership claim survives its order's TERMINAL state and is dropped only when
  the connection is released. Retiring on the ending frame bounds the table more
  tightly, but it makes a closed order unattributed, and the query surface
  reports terminal rows BY DESIGN - so every connection's finished history would
  have gone to everyone.
  ALL THREE CHANNELS ARE CLOSED as of 2026-08-16. The third was the one no
  filter could close - the single process-wide ledger - and it went with the
  PASSENGER OBJECT landing: an account id plus its own engine, created on demand
  and keyed by id, named with `ws?account=` and `account=` on the pulled
  snapshot. A connection naming none gets the default account, which exists for
  the ephemeral single-client venue rather than as a venue-wide account everyone
  shares.
  ATTRIBUTION IS BY ACCOUNT, NOT BY CONNECTION, corrected during that landing
  and worth keeping: two sockets presenting one id are the SAME TRADER, so
  hiding one socket's resting order from the other's query is not invisibility,
  it is a client losing sight of its own book. The socket suite pins both
  halves - same id shares a ledger, different ids do not.
  THE SWEEPER STILL WALKS EACH RIVER ONCE. Pending scans are gathered across
  every passenger BEFORE the walk and the results applied back per ledger
  against the same prices, which is what keeps the water common and the money
  private. Settlement marks are cloned per passenger, since a settlement instant
  belongs to the calendar.
  THE CLIENT-NAMED OPENING BALANCE LANDED TOO, 2026-08-16: `POST /accounts`
  takes an id and its balances over the JSON control plane, following the
  `/control/divergence` precedent, and only the id crosses the socket upgrade.
  OPTIONAL by design, since account resolution is total - a connection that
  never calls it is served under the default account on the venue's
  `[balances]`, which stops being the balance of one ledger and becomes the
  OPENING balance of every unnamed one. Re-opening a live account is a `409`
  rather than a reset: an account outlives its connections, so the request
  cannot be told apart from a reconnecting client re-sending its config, and
  the reset reading would silently wipe a position book.
  THE RISK POLICY LANDED, 2026-08-16. `POST /accounts` carries an optional
  `policy`; `mogwai_protocol::risk` is the wire type and
  `mogwai-server/src/risk.rs` the evaluator. A rule is the TRIPLE the design
  named - measure, basis, breach action - with `lock_until_reset` and
  `terminate` as the two actions, a trailing drawdown ratcheting on either
  intraday peak equity or end-of-day balance with an optional lock level, and a
  daily loss limit measured from the day's opening equity. The account defines
  its own day as a minute of the UTC day, not the instrument's calendar. A
  breach FLATTENS through `Engine::liquidate_all` - resting orders cancelled
  first, then positions closed reduce-only at the mark against the configured
  liquidation band - and then locks; a locked account is refused at order entry
  by name, while its cancels and queries are still served so it can tidy its
  own book. Thresholds, the ratcheted peak and remaining budget publish on
  `GET /account` for the evaluator.
  TWO GAPS CAME OUT OF THAT LANDING and are their own items below: the
  mark-cadence evaluation, and the single-currency confinement of a policed
  account.
  EVICTION AND RESUMPTION LANDED, 2026-08-16. A second socket presenting a
  seated account id evicts the first with a NORMAL close (`1000`, not a fault -
  a consumer treating it as failure would redial and evict whatever evicted it)
  and inherits that account's ledger, orders and risk state. That is the whole
  reconnection story: the venue cannot tell a returning client from a stranger
  claiming the id, so handing the account over is the only behaviour that lets a
  killed worker come back to its own book, and it is what the two owed restart
  runs need. `reset_account_on_reconnect` opts into a clean ledger instead, and
  `ReadyRecord::VERSION` is 7 carrying that setting, so a launcher reads the
  policy rather than inferring it.
  POLICY PRESETS AND THE TERMINAL BREACH LANDED, 2026-08-16, completing the
  account-policy design bar the residue below. Resolution is now TOTAL and
  three-step like a symbol's: inline knobs win, else a name registered under
  `[account_policies]` in the venue config or one this build ships, with
  registered shadowing shipped, else unpoliced. A name nobody has is an ERROR
  rather than a silent fall to unpoliced - a run that believes it is enforced
  and is not is the worst outcome available. Registration is a RUNTIME path
  because funded-account programmes number in the hundreds and change without
  notice; the three shipped names are illustrative shapes, not any firm's terms.
  A TERMINATING breach on a venue serving ONE account now ends the run, and
  deliberately does not on a shared exchange, where one subagent breaching must
  not take down the batch. The account count is what distinguishes the modes at
  runtime.
  ALL TRANSPORT HAVOC RIDES THE PASSENGER, 2026-08-16. `GoDark`, `StallData`,
  `DelayAcks` and every `CommandLatency` field moved from the run onto the
  account, and `/control/divergence` takes an optional `account` naming whose
  view to corrupt; absent still means every account, which is what an operator
  on a single-account venue wants and what every existing scenario file already
  writes. A clear still clears everyone, since a clear is an operator saying
  stop everything. Generator arms are untouched: they change the WATER, which
  belongs to the river and reaches everyone reading it whatever account they
  trade. The water/view test the design named is now the actual code structure.
  THE FREEZE AND ITS TTL LANDED 2026-08-16. A `Passenger` carries its
  attachment, the sweeper skips a frozen account entirely, resuming re-bases
  every surviving order onto the returning boat's clock and retires what the
  account held off the newly joined river, and `account_ttl_ms` collects an
  account nobody reclaims. `ReadyRecord::VERSION` is 8, carrying the TTL beside
  the reset setting.

- PROBLEM STATEMENTS. **This was the solvable set of problems believed to get
  mogwai to the end state the user needs.** That was a claim rather than an
  inventory: each entry was believed NECESSARY, and the set was believed
  SUFFICIENT FOR MOGWAI TO STOP BEING THE BLOCKER. All seven have now resolved
  into landed code, which is the point at which the claim becomes checkable
  rather than assumed - and whether the end state is in fact reached is an
  observation to make going forward, not a re-litigation to hold here. If it
  turns out not to be, that is a finding worth having, and the reason the claim
  was stated as a claim rather than as a list.

  ZERO, DOWN FROM SEVEN. `problem-order-book.md` is deleted: the user's fill
  model needed no book, and what remained open after that ruling - the
  volatility estimator, the band's scale and shape, the derived RNG stream,
  self-trade impossibility - is landed code (`a214996` and follow-on commits),
  pinned by the tests and docs the landing itself cites. `problem-fees.md`
  dissolved into the instrument model (an exchange charges fees, so the
  schedule is one more config knob) and was deleted earlier.
  `problem-refused-order-types.md` is deleted the same way: the venue now
  accepts `StopMarket` and `StopLimit`, with reduce-only and post-only as
  first-class flags and the touch-versus-through trigger distinction, and the
  adapter stopped refusing them at conversion - landed code, pinned by the
  engine, server and adapter test suites the landing itself added. The
  MECHANISM half of `problem-instrument-profiles.md` went the same way, and the
  document's one surviving question - whether the arrival and volatility
  process constants are genuinely per-instrument - was answered by the same
  parameterization ruling that closed `problem-instrument-model.md` below: the
  model is a complete parameterization, so those constants are per-instrument
  because everything is. Last of the seven, `problem-instrument-model.md` and
  its spec, `spec-instrument-model.md`, are deleted with it: the venue now
  models an instrument as a bundle of knobs rather than one hardcoded spot
  shape - instrument identity and class, a multiplier-aware contract size grid,
  a futures margin ledger with mark-to-market and settlement, a session
  calendar with genuine closure, a fee schedule reaching the consumer as booked
  commission, `position_id` end to end under both netting and hedging, and a
  preset layer with mandatory provenance - landed code, pinned by the test
  suites and gates each landing added. `reference/architecture.md`,
  `docs/config.md`, `reference/glossary.md`, `docs/cli.md`,
  `reference/performance.md` and `docs/presets.md` /
  `docs/oms-types.md` carry what must endure; the landing history is git's, not
  this file's, to keep. `notes/` now holds no problem statement or spec files
  at all - only this one.

  PREMISES THE USER HAS SETTLED, which every document below inherits and none
  previously stated. Forward tests always run ACCELERATED, never at speed 1.0 -
  which is a correctness bound rather than a cost one, since the adapter's
  one-second minimum wall request timeout caps usable sim speed and a timed-out
  request is a failed run. A run has an OPTIONAL DURATION in sim time,
  defaulting to indefinite. There is NO RESTART and NO RESUME; mogwai is fire
  and forget, and reproducing a path means a fresh instance with the same seed
  and config. WARMUP is declared config, so the venue generates it eagerly at
  boot and `MAX_HISTORY_SEEK_TICKS` dies with the lazy history it existed to
  bound. Strategies are SINGLE-INSTRUMENT, which is why independent per-symbol
  tapes carrying no cross-instrument correlation is correct rather than a defect.
  There is ONE `MOGWAI` venue, not one per asset class.

  AMENDED 2026-08-16 with the owner. Three of the premises above now read
  differently, and they are amended rather than rewritten because what changed
  and when is worth keeping.

  A VENUE IS SCOPED TO ONE RUN BY DEFAULT, AND TO AN ORCHESTRATOR'S BATCH IN A
  SECOND MODE. The per-run instance every premise above assumes is unchanged and
  remains the default: a consumer given no address spawns its own venue and owns
  it. What is added is an optional shared exchange - one `mogwai serve` whose
  address an orchestrator hands to its subagents, each connecting with its OWN
  ACCOUNT - motivated by amortizing tape generation across a batch. "One MOGWAI
  venue, not one per asset class" survives either way, and is strengthened in the
  second mode: one venue across asset classes AND across the batch's agents.

  NO RESTART AND NO RESUME survives for the PROCESS in both modes and is
  unchanged: a venue is never restarted in place. "Reproducing a path means a
  fresh instance with the same seed and config" stays exactly right for the
  default mode. In the shared mode it becomes "requesting the same WINDOW on the
  same tape", which is why placement has to become a request parameter THERE and
  can stay config here.

  WARMUP GENERATED EAGERLY AT BOOT is already only half true - non-boot rivers
  materialize on first read - and would go fully wrong under the shared mode's
  per-window placement, where a request for `[T1, T2]` needs materialization from
  `T1 - warmup_ns` and cannot be served by whatever one span was generated at
  boot. In the default mode, eager-at-boot is correct and is what you want, since
  the boot river IS the run's river. Warmup stays DECLARED CONFIG in both.
  `MAX_HISTORY_SEEK_TICKS` staying dead is unaffected.

  SINGLE-INSTRUMENT STRATEGIES is untouched, and note it does not imply a
  single-instrument VENUE: one strategy trades one symbol, while the exchange
  serving those strategies serves many.

  THE SUFFICIENCY CLAIM HAS NO EVIDENCE AND IS NOT MEANT TO. Two review passes
  have now flagged that, correctly as a matter of fact and beside the point as a
  matter of genre: the first paragraph says outright that this is a claim rather
  than an inventory, and that being wrong about it is the finding it exists to
  produce. It cannot be evidenced in advance without already having built the
  thing. Do not raise it a third time; raise a MISSING ENTRY instead, which is
  the falsifiable form of the same objection.

  Three things are deliberately outside that claim. THROUGHPUT - whether N
  instances fit on the machine - is excluded by the user's standing instruction
  that resource cost shapes no decision here. The CLAIM PIPELINE - how a seed
  becomes provenance attached to a result, how many paths make a claim, how they
  are allocated - belongs to whatever consumes the venue; mogwai's obligation
  ends at generating a path and reporting which. And the open items elsewhere in
  this file, notably the dead-feed watchdog and the terminal-venue-fault
  decision, both bear on whether a forward result is VALID and are not part of
  this set.

  They are ordinary todo items that outgrew a bullet, so they live in their own
  files; they carry the evidence, the decisions to be made, and what is
  explicitly out of scope, but no implementation plan. A spec is written against
  `reference/technical-implementation-spec.md` only once the problem statement
  it descends from has been resolved.

  ORDERING WAS A GRAPH, NOT A LINE, while the set was open - recorded here as
  a historical note rather than left to imply an active dependency structure,
  since there is nothing left in the set to sequence. Two independent reviews
  found an earlier total-order draft circular; the graph that replaced it ran
  `lifecycle` and `seeds` into everything else, and `cadence` and
  `instrument-model` both into `profiles`, with `order-types` gaining no
  inbound edge once the fill band replaced the order book it would otherwise
  have waited on. All of it resolved in landing order without the graph ever
  needing to be redrawn again.

  The end state they served: on the order of 200 agents running concurrently,
  each developing a strategy through broadarrow - backtest, optimize, Monte
  Carlo - and then FORWARD TESTING it against mogwai. Whether that many
  fit on the machine is explicitly not a design input; resource cost does not
  shape any decision in these documents.

  READ "INSTANCES" CAREFULLY HERE (amended 2026-08-16). In the DEFAULT mode 200
  agents really does mean 200 venue processes, and the exclusion says plainly
  that whether they fit is not a design input. The SHARED mode exists to make
  that number smaller by amortizing tape generation, and there the agents are
  CONNECTIONS on a handful of exchanges rather than processes. The exclusion
  holds in both readings - it was always about not letting cost shape the design,
  and the shared mode is cost motivating a second mode rather than bending the
  first one. Note the two counts the axes must scale in differ accordingly:
  processes per machine in the default mode, connections per venue in the shared
  one.

  WHO DECIDES: the repository owner, on every product and architecture question
  in every one of these documents. There is one user, and the operator of the
  venue is an agent acting for them. broadarrow is a consumer, not an authority -
  mogwai is a nautilus adapter, so where a standing broadarrow note conflicts
  with what nautilus strategies emit, the note is a preference and loses.
  Consulting them is courtesy, not process.

  ACCEPTANCE was previously listed here as the largest defect these documents
  share - that none names a measurable form of "done". That paragraph was wrong
  at the layer it applied: gates are a
  `reference/technical-implementation-spec.md` concern, stated there as exact
  copy-pasteable commands, and a problem statement that carried them would be
  doing the spec's job. The documents are correct to omit them. Two things
  survive the removal. The SET needs no acceptance criterion at all - the
  repository owner is the gate and will know. But the cadence document does
  invalidate a currently-green gate, the 0.1603 duration ACF anchor, without
  naming a successor, and that debt is real and belongs to whichever spec
  descends from it.

  DELETED, not archived: `notes/problem-instrument-profiles.md`. Its mechanism
  half had already moved to the instrument model under the parameterization
  ruling. Its one surviving question - whether the arrival and volatility
  PROCESS constants become per-instrument at all - is answered by that same
  ruling and needed no separate document: the model is a COMPLETE
  parameterization and a preset is a named bundle of otherwise-tunable knobs, so
  those constants are per-instrument because EVERYTHING is per-instrument. The
  arrival constants, the GARCH parameters and `SIZE_LOG_SIGMA` get slots like any
  other knob.

  What survives is not a design question but a FITTING one, and it belongs to
  whoever builds each preset: whether BTC and ETH genuinely differ enough to
  warrant different values, which the measured 2.8x dispersion spread across
  three crypto majors suggests but one month of one venue cannot settle. That is
  answered when the data arrives - trade-level or 1-second archives spanning
  years are expected - and it gates nothing in the meantime, because the venue
  can already EXPRESS a difference whether or not one is fitted. The evidence
  asymmetry the document recorded stays true and stays relevant to preset
  authors: BTC, ETH and SOL have trade-level archives, MNQ and MES have
  15-second bars and nothing else, so a CME preset's cadence is derived
  arithmetic and its clustering comes from nowhere at all. Each preset says
  where its numbers came from.

  DELETED, not archived: `notes/problem-fees.md`. The engine books zero
  commission on every fill, which biases every claim optimistically and
  systematically - but an exchange charges fees, so under the parameterization
  ruling the schedule is one more config knob and the problem belongs to the
  instrument model, which now carries it. Its "declare fee-free and push cost
  onto the consumer" exit was independently closed and the reason is recorded
  there: nautilus computes commission client-side only in its SIMULATED matching
  engine, so on the live path a venue reporting no commission is
  indistinguishable from one that charges none, and nothing downstream can
  correct for it. Also deleted, its problem fully landed: `notes/problem-
  refused-order-types.md`. The venue was refusing `StopMarket` and
  `StopLimit` at conversion; it now serves both, first class - a four-variant
  `OrderType`, a `Resting` state machine distinguishing a live limit from an
  untriggered conditional from an inert market remainder, a stop that
  triggers on TOUCH rather than THROUGH, reduce-only and post-only as wire
  flags enforced at fill time, and the adapter's `wire_order_type` no longer
  refuses the two types. Trailing stops and two-leg brackets remained refused
  by name under a ruling that they were excluded rather than deferred; that
  ruling is REVERSED 2026-08-16 by the order-type completeness ruling below.

  RAISED IN REVIEW AND RULED ON, recorded so they are not raised a third time.
  (a) Three documents each partly re-scope the realism gate - cadence
  invalidates its anchors, profiles moves the arrival constants out from under it,
  and the parameterization ruling lets config move the tape anywhere - and it
  was argued that nobody owns the result. The owner is the repository owner, the
  same answer as ACCEPTANCE above. (b) Three documents each want to rewrite part
  of `mogwai-engine` (per-run state, matching, a margin ledger) and nothing
  sequences the REWRITES as opposed to the decisions. That is spec-level, the
  same layer error the acceptance paragraph made. (c) The dead-feed watchdog and
  the terminal-venue-fault item stay OUTSIDE the set. A venue fault is
  mogwai failing to do its job and is obviously terminal, and mogwai surfaces it
  as such where it can tell - but in most cases it cannot, because a real
  failure shows up as a crashed or stalled PID rather than as a protocol event.
  Under fire-and-forget instances tied to a parent process that is exactly what
  the owner observes, so the silent-but-socket-alive failure the watchdog was
  designed for was a property of the long-lived shared daemon being deleted. The
  watchdog is not worthless; it is not structural.

  Also relevant and not a problem statement: `reference/glossary.md` defines the
  identity chain the code builds - now just run, tape and ledger, since the
  lifecycle landing collapsed account, session and subscription out of it.

- `an_armed_divergence_reaches_every_connection` FLAKED ONCE at the piece-7
  landing gate, 2026-08-15: "market data generated after an armed StallData
  window arrived", in a full `--gate` sweep under machine load, then passed
  3 of 3 focused runs and the full gate rerun on the identical tree (only a
  doc comment differed from the prior green gate). Wall-clock window
  assertion on a socket test, so load can stretch the stall boundary. One
  observation only; if it recurs, the fix direction is the same family as
  the smoke snapshot item below - assert on the divergence window's own
  clock, not on wall-time arrival order.

- THREE SOCKET-LEVEL CLOCK TESTS deliberately not built at the piece-10
  landing, 2026-08-16: an armed stall window lasting its declared span on a
  late boat, a window armed before boarding still opening, and two boats
  swept at their own cadence. Each rule is pinned at unit level (the
  HavocWindow suite, the sweeper schedule tests); the socket forms are
  latency-bounded assertions judged more likely to land as flakes than
  gates, per the standing lessons on wall-clock socket tests. If a
  socket-level demonstration is ever wanted, design it on the divergence
  window's own clock rather than wall arrival order.

- ACCOUNT VALUATION: what a holding is worth in the currency its policy is
  stated in. LARGELY CLOSED 2026-08-16, with the residue stated at the end.
  THE MECHANISM, worth keeping because it is not obvious: a SPOT fill credits
  the base asset as a CURRENCY BALANCE and debits the quote - `apply_fill` in
  `mogwai-engine/src/account.rs` - so buying one BTC at 60,000 leaves the ledger
  holding `BTC: 1` beside `USDT: -60,000`. Equity cannot be stated without
  valuing the base. A future moves only its settlement currency and carries its
  own unrealized, which is why futures never had the problem.
  THE DEFECT, recorded because the wrong version shipped for one commit: equity
  summed every balance total, valuing one unit of any asset at one unit of any
  other. On the DEFAULT preset shape - BTCUSDT spot - that reads a 59,999 loss
  on a purchase that changed nothing, so a trailing drawdown fired on the first
  buy. Not an exotic case; the common one.
  WHAT LANDED. `Engine` keeps a LAST MARK per symbol for every class rather than
  only the ones posting margin, `valuation_symbols` asks the sweeper to price
  every pair whose base the account holds, and `valuation_in` answers what the
  account is worth in one currency: that currency's balance, plus each other
  balance valued through an instrument quoting it in that currency, plus the
  unrealized on futures settling in it. A policy must NAME its currency; an
  order whose shape would leave a holding nothing prices is refused at ENTRY by
  name; and an account that reaches an unvaluable state some other way is warned
  about and NOT enforced, on the grounds that enforcing against a wrong number
  is worse than not enforcing because it looks enforced.
  SO A POLICED SPOT ACCOUNT WORKS, which the default tape shape needed.
  THE RESIDUE, none of it blocking:
  - ONE HOP ONLY. An asset is valued through an instrument quoting it DIRECTLY
    in the policy currency. Hold ETH under a USD policy with only ETHUSDT and
    BTCUSD listed and nothing prices it, so the account is unvaluable rather
    than valued through a chain. A rate surface would fix it and buys
    cross-currency accounts too; nothing needs it yet.
  - THE MARK IS AS STALE AS THE LAST SWEEP, inherited from the margin ledger
    rather than new, and the same gap the mark-cadence item above describes.
  - THE LEDGER-GENERALITY RULING still wants shares, leverage and funding
    payments. Every one needs a holding valued in a currency it is not
    denominated in, so this machinery is the part of that which now exists.

- THE ORDER-TYPE SURFACE IS COMPLETE, NOT CURATED. Ruled by the owner
  2026-08-16: mogwai is an exchange, so there is no axis on which it limits
  order-type support. An exchange serves the types that go with the instruments
  it lists, and a shape mogwai refuses is a strategy family that has NO forward
  test anywhere - not a worse one, none - because the forward leg is the only
  place execution behaviour can be validated at all.
  TWO PRIOR RULINGS ARE REVERSED, and WHAT CHANGED IS THE AUDIENCE, not the
  argument - recorded precisely, because the earlier rulings were correct when
  made and reading them as mistakes would teach the wrong lesson. (a) Trailing
  stops and two-leg brackets were excluded rather than deferred. (b)
  `MarketIfTouched` was dead unless re-argued, on broadarrow's evidence that
  TradingView rejects the offset-absent exit-at-activation shape. Both rested on
  the owner not needing the shape, which was a sufficient reason while the
  owner was the only user. MOGWAI IS NOW PUBLIC, and others might. The scoping
  rule that replaces "what does the owner need" is the definitional one already
  stated for instrument classes: an exchange serves the order types that go with
  the instruments it lists, and the venue's surface is not sized against any
  particular consumer's current catalog.
  WHAT THIS PULLS IN: `MarketIfTouched`, `LimitIfTouched`, `MarketToLimit`,
  `TrailingStopMarket`, ORDER LISTS with real linkage (OCO and OTO), and the
  `Day` and `Gtd` time-in-force values. `Day` is not trivia - it is the DEFAULT
  on equity venues, so an equity surface offering only Gtc, Ioc and Fok is not
  an equity surface.
  MOST OF IT IS TRIGGER VARIANTS on machinery that already exists: the fill
  sweep walks prints against per-order trigger prices, and the `Resting` state
  machine already distinguishes a live limit from an untriggered conditional.
  `TrailingStopMarket` is a per-order high-water mark plus an offset, which is
  the SAME ratchet the account-policy trailing drawdown needs, so the two share
  a mechanism if they are built in either order.
  ABSORBS the former standalone GTD item, whose content follows: `Gtd` needs an
  `expire_time` on the wire plus a time-driven expiry pass on the sweeper that
  has nothing to do with triggers. The conditional-order-type landing carried a
  GTC-only rule for stops for exactly this reason.
  LANDED 2026-08-16. `TrailingStopMarket`, `MarketIfTouched`, `LimitIfTouched`
  and `MarketToLimit` are served, along with `Day` and `Gtd`. ORDER LISTS landed
  with them: a linkage is a group id plus a rule each member carries, applied
  where the fill is COMMITTED. See `docs/order-lists.md` and the architecture
  note.
  COMPLETE 2026-08-18: `TrailingStopLimit` was the last refusal and is served,
  carrying `limit_offset` beside `trail_offset` with its limit price DERIVED and
  re-derived on every ratchet. `wire_order_type` now has no refusal arm at all,
  so a type nautilus adds later is a compile error rather than a runtime refusal
  a strategy meets mid-run. The surface is complete in fact, not only in intent.
  THE TOUCHED FAMILY is a third `ScanKind`, `TriggerToward`, rather than a flag
  on the stop predicate: a stop fires when price runs AWAY from what it protects
  and a touched order when price comes TOWARD its level, and putting the two
  most easily confused behaviours in the venue behind one boolean is how they
  end up swapped.
  THE TRAIL ratchets on the MARK - never retreating, which is the whole
  mechanism - and moves the trigger on both the resting state and the submit,
  because the reservation derives from the submit. It carries the same
  resolution bound as the risk policy and for the same reason.
  EXPIRY is its own time-driven pass. `Gtd` expires at its stated instant; `Day`
  expires when its own instrument's SESSION CLOSES, which the sweeper detects by
  asking the calendar whether the swept span crossed from open to shut. An
  instrument with no calendar supplies no instant, so a day order on a 24/7
  symbol rests like a Gtc rather than expiring at an invented hour.
  LANDED 2026-08-18: an expiry reports `OrderExpired` with a terminal `Expired`
  status, and the adapter maps it to nautilus's own `OrderStatus::Expired` and
  `OrderExpired` event. It reported `Canceled` until then, on the argument that
  nothing downstream matched the difference - the argument the order-type
  completeness ruling overturned. See `reference/architecture.md`.

- A trigger-act latency havoc arm, if a scenario ever needs a trigger fired
  later than the sweep interval already allows. Deliberately not built with
  the rest of the conditional-order-type surface: the sweep interval already
  bounds how late a trigger can be, and a per-trigger delay knob would be a
  new arm rather than an extension of an existing one.

- The price-SPAN-per-inferred-match-event measurement, still owed. The
  triggered-stop fill (like the plain market order before it) slips by the
  existing fill band, reused rather than separately fitted; how wide that
  band SHOULD be for a triggered stop is a scale question this measurement
  would answer, and it has never been computed - the sweep tail quoted
  elsewhere (up to 2,213 aggTrade rows in one inferred event on BTC) counts
  rows rather than distinct prices, so it does not establish how far a
  marketable order actually walks. One probe extension over archives already
  on disk would settle it; until then the slippage magnitude stays an
  unquantified mechanism shared by every order type that slips.

- RE-SCOPE the acceptance-time market reading, or accept 9.8 ms inside a
  submit. Re-measured 2026-08-14 after the checkpoint stride repair: miss median
  9.782 ms, p99 9.987 ms, hit 0.096 ms, on host `bygg` in release. The stride
  repair cut checkpoint positioning by 53x and moved this by under 3 ms, which
  settles where the cost lives: the 300 s `VOL_WINDOW_NS` walk, not the restore.
  Everything below still stands with 12.6 read as 9.8. Originally measured
  2026-08-03 by `read_market_latency_stays_within_submit_budget`
  after that instrument was corrected to time the cache MISS rather than a
  warmed hit. The cadence landing applied lever two of that gate's own
  KEEP/REVERT rule (memoize per symbol per sweep interval, `MarketReadingCache`)
  and not lever one (a shorter `VOL_WINDOW_NS` or an otherwise re-scoped
  reading), so the 5 ms budget is met on the hit path (~0.13 ms) and missed by
  2.5x on the miss path. Lever one moves the estimator's identity and re-blesses
  the fill golden, which is why the cadence spec put it out of scope; it is
  still owed. Two prices are being paid for that: the 12.6 ms itself, and the
  loss of an exactly-stated slippage contract (the reading instant is not on the
  wire, so both end-to-end gates now assert a bracket - see the doc comment on
  `MarketReadingCache`). Putting the reading instant on `OrderFilled` would buy
  the contract back cheaply and independently of the re-scoping.

- The reconciliation exposure is a CLASS, not one method: every report path
  mogwai relies on shares the silent-degrade property. The socket-backed guard
  in `crates/mogwai-adapter/tests/reconciliation.rs` seeds venue truth and pins
  each granular generator, `query_order`, and their mass-status composition over
  both query carriers. Known limitation: it proves the adapter WOULD answer when
  asked, not that the node asks. Related upstream, queued in the maintainer's PR
  tracker and NOT a substitute for this guard (mogwai overrides the method, so a
  better trait default protects the next adapter author, not this repo): give
  the Rust trait default the same composing behavior as the Python base.

- BUILD: a positive dead-feed watchdog (formerly sweep item AD12). No liveness
  timer, tick counter, or "0 ticks in N s" log exists on either transport. The
  negative diagnostics are all in place - the server emits a `ProtocolError` on
  an unservable subscribe, the adapter's data drain warns rather than swallowing
  it, and the poll loop self-heals after a server restart - but nothing
  positively proves a subscribed feed is alive rather than genuinely quiet. The
  WS idle timeout does not cover it: `idle_timeout_ms` defaults to 0, and even
  armed the idle clock resets on ANY application frame, so a
  data-silent-but-frame-active socket never trips it, deliberately, because that
  is what reproduces the 4255 case. The landed default-tape dwell bound is what
  supplies the threshold separating "the venue is asleep" from "the subscription
  is dead": honest silence on the dense default tape now has a gated upper
  bound (the realism gate's era-windowed p999 gap, empty-hour fraction and
  longest empty-hour run), and an armed LiquidityDrought legitimately
  silences the feed but is visible via the control plane, so the watchdog can
  account for it.

- NUMERICAL STABILITY in `AutoCorr`, and it needs cadence-impact analysis before
  anyone touches it. Surfaced 2026-08-05 by the F3-F6 conformance fixtures. Its
  `acf()` guards zero variance with `if var <= 0: return [0.0] * k`, and that
  guard fires only when the variance is EXACTLY zero. A series constant at an
  irrational value - the fixture case is `abs(log return)` constant at ln2 -
  leaves a tiny positive float residue from `sumsq / n - mean * mean`, so the
  guard misses and the returned ACF comes out of catastrophic cancellation
  rather than measurement. Both branches are wrong in the same way the report
  keeps recording: a number is substituted where the honest answer is that the
  quantity is undefined for a constant series.

  NOT fixed, deliberately, and not during the sampling-frame experiment.
  `AutoCorr` computes the F1 duration ACFs as well as the return ACFs, and its
  output is bit-exact against `analysis/cadence.json` - `duration_acf_lag1`
  0.32204142581620676 and `duration_acf_lag5` 0.22388204486699373 - which is the
  lineage the fingerprint's cadence half rests on. Changing the estimator
  invalidates that equivalence, which is a stronger guarantee than the fix buys.

  What a fix would need: return an explicit unavailable rather than zeros, a
  relative rather than absolute variance floor, and possibly a two-pass or
  Welford accumulation to stop the cancellation at source. Each changes numbers,
  so the work is the ANALYSIS of what moves in the cadence targets and whether
  the fingerprint must be refitted, not the code change. Real monthly series
  carry positive return variance and come nowhere near the degenerate case, so
  nothing currently depends on this being fixed.


## Consumer context: every MOGWAI item in broadarrow's todo

Copied 2026-08-15 from the sibling checkout `../broadarrow/notes/todo.md`. Our
file carried exactly one of these (serve N instruments) and carried it without
the fact that reprioritizes it, so the rest were invisible from this side.

CONSUMER CLAIMS, NOT VERIFIED VENUE TRUTH. Every statement below is
broadarrow's, written from its side of the wire and dated to when they wrote it.
Where one asserts something about this venue's behaviour, treat it as a LEAD to
check against the source - the same standing rule the hardcoded-value inventory
carries, and for the same reason. Several may already have been overtaken by a
mogwai landing.

OWED: TELL THEM, and expect their build to break. Nobody has, and the whole
account surface moved under them - they set no `account_type`, inherit
`MOGWAI-001`, POST no account, and have no handling for a run that ends by
LIQUIDATION.
THE ACCOUNT-ID CONTRACT RIDES WITH THAT MESSAGE, ruled by the owner 2026-08-18
and stated in `docs/config.md`: a client on a SHARED venue must name its own
account, and a client that spawns its own ephemeral venue owes nothing. It is a
usage contract, not a venue problem - the venue cannot tell the two modes apart,
and two clients presenting one id ARE one trader from its side, which is the
same mechanism that makes reconnection work. It matters to them because their
orchestrator runs the shared shape: 50 subagents inheriting `MOGWAI-001` would
take each other's ledger in turn. Their per-subagent account TOMLs are where the
id belongs. That break is DESIGNED (see the account-policy item's consumer
paragraph), but designed-to-break only works if the other side is told it is
coming. Several entries below are now stale IN THEIR FAVOUR and should go in the
same message: trailing stops are served, the full order-type surface is served,
ORDER LISTS are served so the two-independent-legs workaround is no longer
required, and `RejectNextCancel` exists - so their three unrun scenario files can
now be written against a venue that produces the shapes they need.
ALSO FOR THE SAME MESSAGE, 2026-08-18 and this one is purely in their favour:
`TrailingStopLimit` is served, so NO order type is refused any more. Their
`translate_trailing_exit` can emit the limit form as well as the market one, and
the halt that a native Pine trailing leg used to cause is gone for both shapes.
The venue derives the limit price from a `limit_offset`, so they must send that
offset and NOT a price.
ONE MORE BREAK FOR THE SAME MESSAGE, 2026-08-18: an expired order now reports
`ServerMessage::OrderExpired` with a terminal `Expired` status where it reported
`OrderCanceled` before. A consumer matching the wire enum exhaustively stops
compiling, and one matching loosely stops seeing its `Day` and `Gtd` orders end
at all - the second is the dangerous reading and the reason this belongs in the
message rather than in a changelog nobody diffs.

### MOGWAI is the test venue, by decision (broadarrow, 2026-08-14)

They stopped shopping for a third-party paper venue. Every owed forward run that
was blocked on "which venue" is now a MOGWAI question. The eliminations, which
are theirs to own but explain why the load lands here: Kraken streams no futures
bars at all (`subscribe_bars` is a silent `Ok(())` for futures, so a demo that
loaded instruments cleanly would subscribe successfully-looking and receive zero
bars forever), Kraken demo futures is their ONLY server-side paper account,
Bybit spot's wallet position reports invent phantom positions on every cached
pair sharing a base currency, and keyless Kraken spot is a local sandbox fill
sim with no venue book.

A consequence they state plainly and we should not argue with: REAL-VENUE FILL
TIMING NOW HAS NO PROVING GROUND in that project, and they accept it as a
boundary rather than a pending choice, on the grounds that MOGWAI's havoc knobs
model latency and adversarial sequencing DELIBERATELY where a paper venue
supplied one vendor's incidental timing. That is a compliment with an
obligation attached: the havoc surface is now the only model of venue timing
either project has.

### The strategy-search end state, and the four items on the route

Adjudicated with the owner 2026-08-14. One human starts N orchestrator agents;
each ensures the shared broadarrow daemon is up, starts ONE durable
`mogwai serve`, mints per-subagent account TOMLs, and launches ~50 subagents
writing and forward-testing Pine strategies against the attached venue.

Four work items. TWO ARE BROADARROW'S AND BOTH HAVE LANDED:

- Item 1, carry the scenario path on the deploy wire and in the durable
  topology. Landed: `accounts` gained `mogwai_scenario` and `deployment_mode`
  columns.
- Item 2, convert `ba forward` from an embedded daemon to a thin-client deploy.
  Landed: forward is now an ordinary detached fleet member.

Their summary of the result is the line that matters here: "With 1 and 2 landed
the scenario WORKS - N orchestrators, ~50 subagents each, every run attached to
one durable `mogwai serve`, all one fleet. On one instrument. Items 3 and 4 buy
breadth, not function."

- Item 3, SERVE N INSTRUMENTS FROM ONE VENUE. Ours, and they call it the largest
  of the four in real engineering.
- Item 4, consume the multi-instrument venue. Theirs, and it is a DESIGNED break
  point rather than an accident: `run_prep::mogwai_facts` refuses a
  `/instruments` answer of anything but exactly one instrument, precisely so a
  relaxed mogwai breaks their build loudly instead of having broadarrow pick an
  instrument arbitrarily. Closing it means selecting by the strategy's
  frontmatter symbol (the `MOGWAI:<symbol>` identity that already must match),
  per-worker rather than per-venue, after which the readiness record's `symbol`
  field needs its one-venue-one-symbol meaning reconciled. They expect their
  adapter's subscription and warmup paths, already per-instrument-id, to mostly
  follow.

So the sequencing is: item 3 lands here, their build breaks by design, item 4
lands there.

### The `RejectNextCancel` ask, stated as a MOGWAI-repo item

LANDED 2026-08-16. `control::Divergence::RejectNextCancel { reason }` refuses a
cancel for a RESTING order and leaves the order where it was, which is the whole
arm: a client that published its replacement before the cancel was acknowledged
now rests two orders where its script rests one. It is deliberately NOT spent on
an unknown or already-terminal id, since those are refused anyway and spending
it there would look, to a scenario author, exactly like the arm failing to fire.
Their three unrun scenario files can now be written against a venue that can
produce the shape.

Their acknowledgement-sequencing landing (2026-08-12) closed a real live-path
defect: a rejected cancel with its replacement already published leaves two live
orders where the script rests one. NOTHING AT ANY VENUE CAN CURRENTLY PROVOKE
THAT SHAPE, because `control::Divergence` has `RejectNextSubmit` and no
cancel-rejection member. The defect is pinned only by in-process tests driving
the bridge callback directly.

Three scenario files ship UNRUN in their `examples/mogwai`, with what a hand-run
established about each - useful to us because each names a venue behaviour:

- `ack-delay.toml` reaches staging, but MOGWAI rejects the cancel as `unknown
  order`, so it produces `OrderCancelRejected` rather than the resume it was
  meant to witness.
- `ack-famine.toml` correctly produces `CommandAckTimeout`.
- `ack-dark.toml`: `GoDark` drops the startup mass-status query, so their worker
  never becomes ready and never reaches command sequencing at all.

The third is worth a look from this side regardless of the ask: an armed
`GoDark` swallowing the startup query means a client can never complete boot,
which may be correct-by-design (it is a blackout) or may be an arm that is
too broad to be useful.

### Let the launcher name the boot symbol (broadarrow, 2026-08-16)

`LaunchSpec` carries `binary`, `config`, `duration`, `ready_timeout` and
`stderr`, and `mogwai serve` takes `--config`, `--duration` and `--launcher-pid`.
Neither carries a SYMBOL, so the boot river is whatever the config file's
top-level `symbol` key names, or the BTCUSDT default when it names none.

That is a real cost under one-venue-per-run, which is the topology the end state
actually uses. The boot river is the one exception to placement on demand: it is
materialized BEFORE the readiness line and boarded at the configured speed, and
the run retains that ticket for process life. So a launcher that wants symbol X
gets this sequence today:

1. the venue synthesizes a full `warmup_ns` of tape for the BOOT river - the
   expensive part of boot, paid before readiness, for a river nobody will trade;
2. that river keeps a boat, a pacing thread and a fill-sweeper slot alive for the
   whole run;
3. X's river is then materialized COLD, on demand, when the consumer first polls
   or binds it - so the warmup synthesis that actually matters happens at first
   read rather than before ready, inside the consumer's boot path.

Two rivers where one was wanted, and the eagerly-warmed one is the wrong one.
Resource cost is explicitly not a design input for this project, granted - but
LATENCY TO FIRST BAR is, and so is which river is warm when a strategy starts.
Multiply by a few hundred concurrent one-venue-per-run instances and it is the
whole boot cost, doubled and misdirected.

THE ASK: a `LaunchSpec.symbol` rendering as `serve --symbol <SYM>`, overriding
the config's boot symbol exactly as `duration` already overrides
`run_duration_ns`. The precedent is the point - this is the same shape as a knob
that already exists, for the same reason: a launcher knows something the config
file cannot, because one config serves many runs.

The second benefit is arguably larger than the first: it moves the FUNDING
refusal to venue boot. Today a config funding only USDT boots happily and then
refuses `MNQ` at first bind or poll, because only CONFIGURED shapes are
funding-checked at boot while the presets and the fallback are merely recorded as
barred. With the boot symbol supplied at launch, a venue that cannot serve the
one symbol its run exists for fails to start - a far better failure than starting
and then refusing the only thing anyone wanted.

broadarrow cannot do this from its side without authoring or rewriting the
operator's venue config, which is exactly the re-parse its design refuses: it
passes `[launch].config` through untouched on purpose. Until the flag exists the
workaround is that every venue config must name its run's symbol, which means a
config per symbol rather than a config per venue shape.

RETRACTED SAME DAY, and kept only for the narrow case it still covers. The above
argues from a one-venue-per-run topology that is the FALLBACK rather than the
main path - see the next section. On a shared exchange serving N symbols on
demand, the venue should not eagerly warm any particular river at all, so the
boot river is vestigial rather than mis-pointed, and the requirement is that
materialization is demand-driven per (symbol, window). The observation still
holds for the ephemeral spawn path, where a venue really does know its one symbol
up front.

### An exchange serves many accounts and many tape windows (broadarrow, 2026-08-16)

TWO MODES, BOTH REQUIRED. Settled with the owner 2026-08-16. Read the priority
carefully, because an earlier draft of this very section had it backwards.

THE DEFAULT MODE IS THE PER-RUN VENUE, and it is what the grand-design section
above already describes. A consumer given no address spawns its own ephemeral
transient venue, owns it for the run, and takes it down with the worker. N
subagents means N venue processes, each with one river, one passenger and one
ledger. MOGWAI ALREADY SERVES THIS MODE CORRECTLY - the single-ledger engine and
the one-cursor-per-river boatyard are exactly right for it, and nothing in this
section is a defect against it. Note the invisibility defect is latent here for
the same reason: with one passenger there is nobody to be visible to.

THE SECOND MODE IS THE SHARED EXCHANGE, and it exists for PERFORMANCE. An
orchestrator runs one `mogwai serve`, takes the bound address, and hands it to its
subagents; each connects with its own account and asks for whatever tape its
strategy needs. The motivation is amortization: tape generation is the expensive
part, and under the default mode a batch of agents pays for N generations of what
are often identical or near-identical tapes. One river per distinct tape identity
with N cheap passengers reading it collapses that. The owner's day-to-day intent
is to run this mode; it is currently THEORETICAL and its performance case is
unmeasured.

NOTE THIS IS COST MOTIVATING A SECOND MODE, NOT COST SHAPING THE MODEL, which is
why it does not conflict with the standing premise that resource cost shapes no
decision here. The venue's semantics are identical either way; what differs is
how many processes carry them.

Everything below is what the SECOND mode needs and the first does not. None of it
blocks the default mode, so none of it is urgent - but both modes must be
supported, so none of it is optional either.

WHAT LANDED AND WHAT DID NOT. The open-instrument landing (2026-08-15/16) moved
SYMBOL from a run property back to a request parameter, and that half is done -
one venue genuinely serves N tapes now. But the earlier one-venue-per-run rewrite
had removed BOTH symbol and account from the request, under the single premise
"one venue is one run is one ledger". Symbol was undone. ACCOUNT WAS NOT, and
neither was tape PLACEMENT. Verified in this checkout, not inferred from the docs.

#### The two nouns, restated - because the current ones are wrong

This is one axis, not several, and it is easiest to state as a correction to the
boatyard's vocabulary. Settled with the owner 2026-08-16 by worked example, and
CORRECTED the same day: an earlier draft of this subsection made the boat the
per-connection noun, which is the same conflation running the other way. There
are two nouns and the boat is not one of them.

A RIVER IS A TAPE, and it is shared. Its identity is EVERYTHING THAT MUTATES THE
WATER: the symbol or preset, the session or window shape, the loop shape, the
seed, the resolved bundle, the market regime, GENERATOR havoc, and the tape
protocol version. Any two requests agreeing on all of that get one river. Speed
is NOT in this list - it changes delivery cadence and no generated value.

A PASSENGER IS ONE CONNECTED TRADER: its own account, its own ledger, its own
orders, its own view. Never shared, one per connection. Passengers on a river
owe each other two things - NON-INTERFERENCE, which exogeneity already gives,
and INVISIBILITY, which the venue does not give today.

THE BOAT IS AN IMPLEMENTATION CACHE and belongs in `boatyard.rs` rather than in
any statement of the model. It is a shared cursor keyed by (river, speed), and
its whole purpose is to generate and pace one river once rather than N times.
It carries no semantics: nothing a passenger can observe depends on whether it
is served from a cursor of its own or one it shares.

Two things are passenger-local and therefore do NOT split a river: DURATION, and
TRANSPORT havoc (`GoDark`, `DelayAcks`, `StallData`, `CommandLatency`), which
corrupt what one connection RECEIVES rather than what the generator produces.

THE WORKED EXAMPLE, kept because it settles every case that confused this
consumer. Banana asks for MNQ asia session looped 30 days at x60, clean, and a
river is spawned. Coconut asks for the same thing looped 100 days - identical
river identity, since duration mutates nothing - so Coconut is a second
PASSENGER on Banana's river, with its own account and ledger, seeing the water
from wherever the river has already reached. Whether the venue serves the two of
them from one cursor or two is an amortization question with no observable
consequence. Kiwi asks for BTCUSDT.P Mon-Fri looped at x20 with a generator
havoc knob armed; Pear asks for that same tape clean. Those are TWO rivers, not
one, because generator havoc changes the water. Three rivers, four passengers.

WHY THE CURRENT MODEL CANNOT BE PATCHED INTO THIS. The passenger has no object
to hang an account on. In the tree a passenger is a `u32` refcount
(`Seat.passengers`) and a `Ticket` of two shared handles; every piece of state
one would expect it to own - the ledger, the position book, the order book, the
outbound fill stream - is either on the shared cursor or on the process-wide
`Engine`, which holds one `account_id` and one `Account`. So the work is to make
the passenger exist as a thing, not to add `?account=` to a query struct. That
is also why this is a single axis: the account is not adjacent to any existing
noun, it is the missing one.

A NAMED WINDOW ALWAYS GETS ITS OWN RIVER, even against an identical request
already running, because the first requester is by then some N of sim-time ahead
and a window means being served from its start. Sharing therefore only happens
for the unnamed-window form - a preset plus a duration - which is exactly the
request that says "wherever you are is fine".

SPEED IS SERVED, NOT REFUSED. LANDED: seats are keyed by (river, speed) and
an unserved speed is a second cursor on the same water. The remaining 400 is
one ledger, one cadence - a second socket on an account already riding that
river at another speed. An earlier draft called this "speed splits the river",
which overstated it - river identity never contained speed, and two cadences
over one river share the whole checkpoint chain underneath.

#### What that costs, measured against this checkout

THE PASSENGER SIDE. `SocketQuery` is exactly `symbol`, `speed`, `duration_ms` under
`deny_unknown_fields`, so `?account=` is not ignored - it is a hard `400` that
refuses the connection outright. `SocketSession` carries no account. `Engine`
holds one `account_id` and one `Account`, with one balances map and one positions
map for the whole process. So N subagents on one exchange today share one balance
and one position book, and every subagent's fills move every other subagent's
net. For broadarrow that is not merely untidy: an account move it cannot
attribute to one of its own strategies is exactly what the per-bar attribution
guard HALTS on, so the shared exchange currently fails closed rather than
producing wrong numbers. That is the right failure and it costs nothing today,
since the default mode never reaches it - one connection per venue has no second
account to be confused with.

FUNDING IS NAMED BY THE CLIENT. Recorded here as the consumer-side view; the
ruling and its full surface are in the account-policy item above. `[balances]`
seeds the one ledger today and is being deleted outright rather than converted
into a per-passenger template: a connecting client names its own opening
balance, and with it a whole ACCOUNT POLICY - daily loss limit, autoliquidation
threshold and trailing-drawdown mechanics - which the venue ENFORCES. A strategy
sized for a 25k account and one sized for 100k are different experiments, and
under prop-firm rules so are two 50k accounts trailing on different bases.

THE RIVER SIDE. Every cursor is placed at the fixed `run_start_ns` origin and one
river carries at most one - `SocketQuery`'s own doc says it "never places a
second boat on the same water". `duration_ms` is length-from-connecting rather
than a window, so there is no wire for naming a start and an end at all. That is the
half most strategies will actually use, since a named window is what makes a
forward-test claim bindable to something reproducible.

ONE CONSTRAINT TO DESIGN FOR UP FRONT. A strategy needs warmup BEFORE its
requested start, so `[T1, T2]` really asks for materialization from
`T1 - warmup_ns`, and that floor must sit at or above `TAPE_ORIGIN_NS`. A window
requested too near the tape origin cannot carry its own warmup. Better as a named
refusal at request time than as a short warmup nobody notices.

THE REPRODUCIBILITY ARGUMENT IS THE STRONGEST ONE FOR NAMED WINDOWS, stronger
than resource sharing. With an explicit window a run is a pure function of
`(seed, config, symbol, start, end)` - no boarding instant, no wall-clock input
anywhere. Under placement-at-a-fixed-origin-whenever-you-board, reproducing a run
means reproducing WHEN IT CONNECTED, so "a run is a pure function of (seed,
config)" is true of the tape but not of what any client actually saw. Named
windows also make replication pairs exact instead of approximate: deal both
halves the same window and they trade identical water by construction, so the
paired comparison is not confounded by two different draws.

#### Transport havoc stops being a scoping problem

An earlier draft of this section asked for transport havoc to gain an account or
connection scope, on the grounds that `GoDark`, `DelayAcks`, `StallData` and
`CommandLatency` are run-wide today, so one subagent arming a blackout would
black out every other subagent on the exchange. That is a real defect today and
it needs no separate design: those windows corrupt what one connection RECEIVES
rather than what the generator produces, so under the model above they simply
RIDE THE PASSENGER. Passenger-local by construction, river untouched, nothing to
scope. Note this is the same defect family as the fill fanout - a per-connection
concern implemented process-wide because no per-connection object exists to hold
it - so both close with the passenger rather than with two separate mechanisms.

Which is the useful test for any future havoc knob, and worth stating once:
ask whether it changes the WATER or the VIEW. Water changes go into river
identity, so clients wanting different answers get different rivers. View
changes ride the passenger, and leave the river shareable.

### Be an exchange: the instrument, order and account surface (broadarrow, 2026-08-16)

READ THIS FIRST, BECAUSE THE REST OF THE SECTION IS UNREADABLE WITHOUT IT.

Strip every havoc knob off mogwai and what is left is a venue. An exchange.
That is not a metaphor and it is not a reduction - it is the product. The havoc
is an adversarial dial ON TOP of an exchange, and it can only be a dial on top
of the parts of an exchange that exist. Every gap below is a place where the
thing the dial sits on is missing, so no amount of havoc reaches it.

WHY IT MATTERS BEYOND TIDINESS. mogwai is the only model of venue timing either
this project or broadarrow has, and the forward leg of broadarrow's pipeline is
the only place a strategy's EXECUTION behaviour can be validated at all - a
bar-close backtest structurally cannot see resting-order timing, conditional
fills, partial fills, or latency. So for any strategy shape whose instrument,
order type, account type or time-in-force mogwai does not model, there is no
forward test anywhere. Not a worse one: none. The venue is not merely
inconvenient for those shapes, it is the reason they are untestable.

THE ITEMS BELOW ARE NOT DERIVED FROM ANY DOWNSTREAM CATALOG'S CURRENT CONTENTS,
and that is deliberate - an earlier draft of this section justified each item by
what `wyrd` happens to deal today, which is backwards. Those tools are in
development and their catalogs grow; sizing a venue against a snapshot of them
would guarantee re-deriving this list every time one lands a component. The
requirement is definitional and upstream: an exchange serves stocks, futures,
forex and crypto, and serves the order types, time-in-force values and account
types that go with each. mogwai currently serves a strict subset, and the subset
boundary is what this section names.

WHAT MOGWAI MODELS TODAY, so the gaps read against something concrete.
`InstrumentClass` has exactly two arms - `Spot { base, quote }` and
`Future { underlying, settlement_currency, multiplier, asset_class }`, the future
being cash-settled and continuous. Order types are Market, Limit, StopMarket and
StopLimit. `TimeInForce` is `Gtc | Ioc | Fok`. The ledger is one funds-checked
cash account, with per-contract initial and maintenance margin on futures only.
Everything an exchange does that is not in that list is either absent or gets
flattened into one of those two classes.

#### Instrument classes

**Equity.** A stock is not a currency pair, and modelling it as
`Spot { base: "AAPL", quote: "USD" }` is not a near-enough approximation - it
makes the ledger hold AAPL as a CURRENCY BALANCE, on the same footing as USD or
BTC. That is right for crypto spot, where you genuinely hold the base asset as
money, and wrong for equity, where you hold shares. Downstream of that one
choice: no share or round-lot conventions, no settlement period, no short-sale
locate or borrow, and no way to express the cash-versus-margin account
distinction that decides what an equity account may even do. Nautilus carries a
distinct `Equity` instrument type, so a consumer has somewhere to put it.

**Forex.** Expressible as a `Spot` pair only by discarding the thing that makes
it forex: LEVERAGE. mogwai has margin exclusively on `Future`, so a currency
pair cannot post collateral, and a forex strategy's position sizing - the part
most likely to be wrong in a way that costs money - cannot be exercised. Also
missing with it: pip and point conventions, and rollover or swap charges for a
position held across the daily boundary. The 24/5 session the existing
`[instrument.calendar]` can already express, so that half is done.

**Crypto perpetuals.** Absent entirely, and they are the dominant crypto
instrument rather than an exotic one. A perp is close to the existing cash
settled future - no expiry, a multiplier - but it has a FUNDING RATE, a
recurring payment between long and short that mogwai has no mechanism for at
all. A strategy holding a perp across funding instants has a real cash flow the
venue cannot produce, so its forward P&L is wrong by construction rather than by
approximation.

**Inverse / coin-margined.** Also standard, also absent. Worth noting because
this one is nearly free on the consumer side: broadarrow's `VenueProduct`
already carries an `Inverse` arm, and the only reason a MOGWAI inverse run
refuses is that we have nothing to map it to.

**Futures expiry and roll.** The lowest-priority item here, recorded for
completeness rather than pressed. The continuous cash-settled future is a
defensible simplification for most strategy work; it does foreclose anything
keyed to expiry itself, and any strategy whose horizon crosses a roll is being
tested against a contract that never rolls.

#### Order types and time-in-force

Missing order types: `MarketIfTouched`, `LimitIfTouched`, `MarketToLimit`, and
ORDER LISTS (OCO and OTO). The order-list gap is the structural one - mogwai
models no linkage between orders, so a genuine two-leg bracket where one fill
cancels the sibling cannot exist. broadarrow works around it by placing two
independent reduce-only legs and relying on reduce-only plus stale-cancel
reconciliation to reap the loser, which is a real technique that real venues
also permit, but it is a workaround for a missing primitive rather than the
primitive. ALL SERVED as of 2026-08-16, order lists included; the workaround is
no longer required, though it keeps working.

`TrailingStopMarket` is the same gap and is written up separately below, since
it was found first and has its own argument.

Missing time-in-force: `Day` and `Gtd`. `Day` is not optional trivia - it is the
DEFAULT on equity venues, so an equity surface that only offers Gtc, Ioc and Fok
is not an equity surface. `Gtd` is common enough across all four classes to
belong beside it.

ALL OF THIS IS ACCEPTED, 2026-08-16, by the order-type completeness ruling in
the open-issues section: the venue serves the full surface and curates nothing.
This subsection's inventory stands as the gap list; what it no longer needs is
its justification, which the ruling supplies.

#### Account types

One funds-checked cash ledger plus per-contract futures margin is two points in
a space that needs at least three. What is missing is a genuine LEVERAGED MARGIN
ACCOUNT - notional against posted collateral at a leverage ratio, rather than
fixed currency per contract. Forex needs it, crypto margin and perpetuals need
it, and Reg-T equity margin needs it. Without it, "account type" is not really a
dimension the venue has, which is why a consumer configuring one has nothing to
configure it against.

The consumer half of this is broadarrow's and is tracked there: we currently
never set `account_type` at all, so a nautilus `CashAccount` silently discards
the margin rows mogwai already reports correctly on futures. That is our defect,
not yours, and it is worth stating here only so the two halves are not confused
for one.

ACCEPTED 2026-08-16: the ledger rebuild forced by the per-passenger account
policy is scoped to hold shares, leverage and recurring funding payments, so
this subsection and the instrument-class one above are answered together. See
the account-policy item in the open-issues section for the scope and for the one
exclusion, options.

### Trailing stops block a whole dealt exit family (broadarrow, 2026-08-16)

`wire_order_type` in `mogwai-adapter/src/convert.rs` refuses everything outside
Market, Limit, StopMarket and StopLimit, telling the caller that "a trailing
stop or a bracket leg must be expressed as a fixed stop that the strategy
re-places itself". For a bracket that advice is fine, and broadarrow already
takes it - `SubmitBracket` is two independent reduce-only submits, not an order
list. For TRAILING it is not advice broadarrow can take at the venue seam: a
Pine `strategy.exit` carrying a native trailing leg compiles through
`core::translate_trailing_exit` into a reduce-only `TrailingStopMarket`, the
adapter refuses it at submit, and the bridge HALTS rather than run the position
unprotected. Correct fail-closed behaviour on their side; the run is over.

WHY THIS IS NOW A PIPELINE ITEM RATHER THAN A CAVEAT. The strategy-search end
state deals slates with `wyrd`, whose exit catalog carries a
`exit-winner/trailing-doctrines/*` family - `keltner-trail`, `kase-devstop`,
`yoyo-exit` among them. A sample 12-slate batch dealt three of them. Those
slates are exactly the ones whose exits a bar-close backtest cannot validate,
so forward against this venue is the ONLY place they can be tested - and it is
the one place they cannot run. The family that most needs the venue is the
family the venue refuses.

A dealt trailing doctrine can sometimes be authored as a stop the script
recomputes and amends each bar, which mogwai serves today. That is a real
workaround and it covers the doctrines whose trail level is an indicator value.
It does NOT cover Pine's native `trail_points`/`trail_offset`, and leaving the
choice to whichever agent drew the slate makes forward-testability a property
of authoring style rather than of the venue.

The ask, in preference order:

1. Model trailing state on a resting order: a `TrailingStopMarket` whose trigger
   ratchets with the tape's extreme and fires on touch like any other stop. The
   fill sweep already walks prints against per-order trigger prices, so this is
   a per-order high-water mark plus an offset, not a new execution path.
2. Failing that, say so durably in `docs/oms-types.md` or `docs/havoc.md` as a
   named permanent exclusion, so the pipeline can drop trailing doctrines from
   the dealt catalog rather than discovering the halt per run.

GRANTED AS OPTION 1, 2026-08-16, under the order-type completeness ruling. The
ratchet is also the mechanism the account-policy trailing drawdown needs, so the
two share a high-water-mark implementation whichever lands first.

Not asked for: `MarketIfTouched`. TradingView rejects the offset-absent
exit-at-activation shape as invalid Pine, so broadarrow retired that build and
refuses the shape statically at preflight. Nothing on either side wants MIT.
SERVED ANYWAY, per the same ruling: the venue's surface is not sized against a
consumer's catalog, and mogwai is public now, so a shape this consumer cannot
emit is still a shape another might. Their preflight refusal is theirs to keep.

### Runs owed against MOGWAI, all stageable today

These are theirs to run, not ours to build, but each is a venue exercise that
would surface mogwai defects, and several have been owed for weeks.

- THE RESTART RUN, the realized-PnL baseline, legs 1 to 3. Serve durably, trade
  to a non-zero realized figure, SIGKILL the worker, re-run against the same
  `[attach]` scenario, verify the carried baseline, the brake mark, and no
  duplicate booking. Leg 3 is load-bearing and rests on a verdict reached BY
  READING the dependency rather than by observing a reconciliation, and it
  landed as an explicit operator override of its own gate - a known-unrun
  verification on a capital bound.
- `go_live` RESTART DE-DUPLICATION: kill a non-flat worker with orders resting
  at the durable venue, restart, verify the batch de-duplicates against the
  surviving book.
- THE FUTURES RUN. Their static wiring landed (they take the served instrument
  from `/instruments` as authority, admit cash-settled linear futures, refuse a
  `leverage` key they used to drop silently, and size percent/cash orders by
  `price * multiplier`). What is owed is a forward run against a
  futures-configured venue (`preset = "MNQ"`) proving warmup, fed fills, a
  resting stop triggering on the multiplied instrument, a settlement-currency
  commission actually charged, and the brakes marking in that currency.
- THE CONDITIONAL HALF of the fed-fill path. At their 2026-07-31 QA runs mogwai
  filled everything immediately at the order's own price (measured 56 accepted /
  56 filled / 0 resting for a protective limit 20 percent away) and refused
  `StopMarket` outright. That incapacity is gone - the fill model
  penetration-gates resting limits and serves both conditionals first class - so
  what remains is a fed fill from an order that GENUINELY RESTED and then filled
  at venue timing, ideally under havoc.
- FLIP PLUS PYRAMID PLUS PARTIAL in one bar, end to end.
- GATE B, the anchored-warmup overlap drop. Their in-tree `handoff.rs` suite
  covers BINANCE, KRAKEN and BYBIT but NOT MOGWAI, and is a consistency test
  rather than ground truth.
- THE POLL-HEAL END-TO-END TEST, the payoff of our order-status query surface.
  External QA (2026-08-14) settled that their open-order poll DETECTED a
  silently-cancelled resting order every time and REACTED never; root cause was
  theirs (a dep default) and is fixed. What they still owe is the heal
  assertion, and it drives our control plane directly: rest a far-from-market
  limit, POST `CancelOpenOrderSilently`, assert the local order converges to
  Canceled within the retry ladder's bound. Their fixture notes that still
  hold: carry NO protective exits, and census the whole rotated log family.

### Landings they consumed with no mogwai change needed

Recorded so we do not go looking for owed work that is already closed.
CONDITIONAL ORDERS needed no broadarrow change at all - the old denial was our
own adapter's `wire_order_type` refusal, which moved with the venue, so
`strategy.exit(stop = ...)` trades end to end. THE FILL MODEL replacement is
reconciled into their `reference/mogwai.md`. THE ATTACH CAPABILITY landed
entirely on their side; they state explicitly that no mogwai-side change was
needed. Trailing stops were the one refused shape when this was written; both
forms are served now, so NOTHING is refused and that sentence is history.

### Tape sparsity, settled on both sides

Their warmup could not always be satisfied from a window holding no trades, and
the mechanism turned out to be ours and correct: the fitted ACD arrival process
is persistent and heavy-tailed, so a short historical window can legitimately
hold zero trades and `/trades` correctly answers `200 []`. Their "fresh server"
correlation was a proxy - a deterministically seeded generator puts every boot of
a pinned epoch on the SAME stretch of tape, so a config whose epoch lands in a
drought reproduces per-config rather than intermittently. Both sides shipped
diagnosis improvements. What stays open is THEIR policy question, not our
mechanism: a legitimately empty window still costs them a fatal halt, and one of
the two things that would help is blocked on the same nautilus gap as our
`FeedLagged` item - an empty historical response carries no feed identity, so it
cannot be attributed.

## Notes / gotchas

- The CLA check is NOT yet a required status check. cla-assistant.io is wired up
  and its webhook delivers (verified `200 OK` on a real PR), but nothing blocks
  an unsigned merge until a repository ruleset requires the check by name. The
  trap: an owner-authored PR produces no status at all - the CLA assigns
  copyright TO the owner, so the bot correctly has nothing to ask - which means
  the check cannot be picked from the suggestion list and cannot be validated
  against a real run. Type the context name in by hand and leave the rule in
  EVALUATE mode until an outside PR confirms it, because a required check that
  never reports blocks every merge with no visible cause.

- broadarrow standing notes (2026-07-31, their request that landed the
  order-status query surface): (a) the ack-delay havoc band above their ~25 s
  INFLIGHT_TIMEOUT is deliberately unserved - they permanently declined a
  per-venue ceiling on that safety timeout, so do not invest in DelayAcks/
  GoDark scenarios past it; (b) the once-floated MarketIfTouched order-type
  extension is dead
  (the triggering Pine shape is invalid on TradingView and nautilus cannot
  rest an MIT faithfully) - and their position was that the protocol owes no
  order-type growth beyond Market and Limit. SUPERSEDED as of 2026-08-02: that
  was a consumer's preference, and mogwai is a nautilus adapter whose owed
  surface follows what nautilus expresses. MarketIfTouched specifically stayed
  dead unless re-argued; RE-ARGUED AND SERVED as of 2026-08-16 under the
  order-type completeness ruling, mogwai being public now. The standing
  consequence for them is now RESOLVED: the
  venue serves `StopMarket` and `StopLimit` first class (`reference/
  architecture.md`), so a strategy whose protective leg is a stop-MARKET is
  forward-testable on MOGWAI. Nothing left to build here; their pre-deployment
  procedure no longer documents a shape their own tooling cannot exercise.

- Two broadarrow decisions their developer flagged, recorded so the mogwai-side
  residues read as connected rather than orphaned. (a) Enabling the continuous
  open-order poll closes the mid-run dropped-resting-cancel window for real
  venues, at REST-budget cost and needing a per-venue reconciliation override
  that does not exist; it was recorded as inert against mogwai because there was
  nothing for it to call, which is no longer true - the venue-truth order query
  exists, so mogwai would answer it. (b) Raising the inflight ceiling for mogwai
  only is largely moot now: the ceiling was a problem because mogwai could not
  answer `QueryOrder` and every inflight order escalated to a synthesized
  timeout, and it answers now, so the brake fires only when havoc actually
  withholds the reply - which is what the brake is for.

- broadarrow-side follow-ups from the 2026-07-15 QA findings (their repo, listed
  here so the coordination is not lost): (a) the feed-stale message hard-codes
  the issue-4255 hypothesis ("the connection looks healthy...") as fact even
  when the venue process is dead; (b) `reference/mogwai.md` / `ba man mogwai`
  still describe the venue as unfundable - stale once the `[balances]` seed
  lands; (c) any stored scenario TOMLs setting a `transport_profile` on either
  adapter config now fail to parse, since the field is gone with
  `TransportProfile` itself (the lifecycle landing removed the HTTP transport
  entirely, not just its deliverability refusal), and need a sweep. (The
  data-path WARN template that named three wrong causes turned out to live in
  mogwai-adapter, not ba - fixed
  here: it now defers to the venue's `reason`, and the WS lifecycle logs
  disconnect/backoff/reconnect/exhaustion per socket.)
- The offline Kraken corpus is trades only - no quotes, no L2, no aggressor side.
  This shapes the offline analysis only; the running server synthesizes trades
  with a native `Buyer`/`Seller` aggressor AND, since tape protocol 7, publishes
  an observable top of book - one BBO before every parent burst, bounded history
  on `/quotes`, and a connect-time snapshot. This line asserted the opposite
  until 2026-08-05, and `DATA-PURCHASE-REPORT.md` section 12 records it as one of
  two existing records that contradicted that report and went unconsulted, so it
  is worth keeping accurate rather than deleting. The quoted width, top sizes and
  trade displacement remain explicitly uncalibrated placeholders pending CME
  TBBO; what is absent is the calibration, not the layer. `KrakenCsvSource` and
  `TickRuleAggressor` survive in `mogwai-data` for the offline lineage and its
  unit tests.
- `MOGWAI_DATA_DIR` (default `/home/folk/Kraken`) is an
  offline-analysis input only (`analysis/`), never a server runtime knob.
- `research/` is gitignored and holds read-only nautilus, broadarrow and piners
  clones plus `market-data/` (the Binance archives and TradingView exports) and
  `binance-public-data/` (the vendored downloader). Read those APIs from there.
  mogwai BUILDS against the pinned crates.io nautilus release (0.61), never
  against `research/` and no longer against a sibling checkout; see `AGENTS.md`.

## Hardcoded-value and env-var inventory (swept 2026-07-01, re-verified 2026-08-03)

STALE BY CONSTRUCTION between sweeps, and not covered by the removal rule at the
top of this file: it is a point-in-time catalogue rather than a set of work
items, so nothing here gets removed on completion and nothing re-sweeps it
automatically. The 2026-08-03 pass corrected six entries that had drifted -
`MAX_HISTORY_LIMIT` (1000 to 50_000), `CHECKPOINT_K` (8192 to 262_144), a
`/orders` route that no longer exists, `gap_cap_ms` which no longer exists
anywhere, `sim_epoch_ns` which became derived-and-refused rather than a knob,
and a `NO_COLOR` entry that briefly went stale with `man` and came back with it.
That hit rate is the reason for the standing instruction: treat every line as a
LEAD to verify against the source, never as a statement of fact.

Catalogue only, for later evaluation of what deserves to become a knob - nothing
here was changed. Pervasive test-fixture literals (repeated `BTCUSDT`/`BTC`/
`USDT`, golden seed 42, per-assertion timing tolerances) are summarised rather
than enumerated line-by-line; production and config-relevant values are listed in
full.

### Environment variables (whole workspace)

The Rust crates are deliberately env-var-free for runtime knobs; run config lives
in `mogwai.toml`. `RUST_LOG` is the only ambient read on the SERVING path. The
reads:

- `RUST_LOG` - `mogwai-server` via `EnvFilter::try_from_default_env`, falls back
  to `mogwai=info`. The one documented, deliberate ambient exception; a prior
  `MOGWAI_REPLAY_SPEED`/`MOGWAI_GAP_CAP_MS` pair was removed in favour of
  `mogwai.toml`.
- `NO_COLOR` - `mogwai-server/src/man.rs`, standard convention, `man`-output only.
- `MOGWAI_DATA_DIR` - `analysis/characterize.py` and `analysis/recon.py`, default
  `/home/folk/Kraken`. Offline-analysis input only, never a
  server runtime knob. The default path string is duplicated verbatim in both
  files (`recon.py` re-reads the env var instead of importing
  `characterize.DATA_DIR` the way `run_corpus.py` does).
- Compile-time only (not runtime): `env!("CARGO_MANIFEST_DIR")` in
  `mogwai-data/src/generated.rs` locates the baked-in `analysis/fingerprint.json`;
  the server build script bakes `MOGWAI_LONG_VERSION` from `CARGO_PKG_VERSION`;
  `CARGO_TARGET_TMPDIR`/`CARGO_BIN_EXE_mogwai` in server integration tests.

### Cross-crate couplings worth reconciling

- Correctly single-sourced from `mogwai-protocol` (the pattern to follow):
  `DEFAULT_REQUEST_TIMEOUT_SECS` (30) and `MAX_HISTORY_LIMIT` (50_000) - the
  adapter references these rather than re-hardcoding them.
- `default_instruments()` BTCUSDT seed lives in `mogwai-protocol` but its seven
  literals are duplicated verbatim in two of that crate's own tests, and the smoke
  test's fixed order shape implicitly depends on it.

### mogwai-protocol (canonical wire defaults)

Named consts, canonical: `DEFAULT_REQUEST_TIMEOUT_SECS = 30`, `MAX_HISTORY_LIMIT
= 50_000`, `BASELINE_LATENCY.base_nanos = 30_000_000` (30ms honest-feed latency
floor), `MAX_LATENCY_NANOS = 60_000_000_000` (60s per-field ceiling),
`control::MAX_DIVERGENCE_MS = 3_600_000` (1h DelayAcks/GoDark/StallData ceiling),
`ReadyRecord::VERSION = 6`.

The `launch` module (the shipped launcher) adds `DEFAULT_BINARY = "mogwai"`,
`DEFAULT_READY_TIMEOUT = 300s`, `STDERR_RING = 64` retained lines, and
`OWNER_POLL = 200ms` (how often the owning thread notices the venue ended on its
own). It also puts `serde_json` and `tracing` on this crate at RUNTIME rather
than dev-only: the launcher parses the readiness line and announces the run it
started.

Inline literals (no named const):
- `default_instruments()`: symbol `BTCUSDT`, base `BTC`, quote `USDT`,
  `price_precision 2`, `size_precision 8`, `price_increment 0.01`, `size_increment
  1e-8`. Doc comment signposts growth to multi-instrument - prime externalisation
  candidate.
- `ConnHavoc::default()` transport bundle: `reconnect_delay_initial_ms 1_000`,
  `reconnect_delay_max_ms 10_000`, `reconnect_backoff_factor 2.0`, idle/heartbeat/
  jitter 0, `request_timeout_secs 0` (sentinel for the 30s default). Cross-checked
  by the validator, so they move together.
- Validator bounds inline in `validate_*`: VolStorm `vol_mult (0, 100]`,
  LiquidityDrought `thin_factor [1, 1000]`, SessionEdgeSpike hour clamp and
  `extra_vol_mult [0, 100]`, ReopenGap `halt_secs > 86_400` (the one temporal
  bound NOT backed by a named const, unlike its sibling `MAX_DIVERGENCE_MS`),
  PartialFillNext `fraction (0, 1]`.

### mogwai-engine

- Venue/trade id prefixes `V`/`T` as inline magic strings.
- Test fixtures repeat `BTCUSDT`/`BTC`/`USDT`, a base price of 100, and
  partial-fill fractions 0.3/0.4/0.5 across dozens of sites (no shared consts).

### mogwai-server

- Bind: the `BIND_ADDR` const, `127.0.0.1:0`, not configurable at all - the
  `--addr` flag is gone, so ephemeral loopback is the only endpoint and it is
  reported on stdout as the readiness line, and on stderr as `mogwai listening`.
- HTTP route strings (`/health`, `/account`, `/instruments`, `/trades`,
  `/quotes`, `/clock`, `/ws`, `/control/divergence`) as inline
  literals, no shared registry with the adapter's route segments.
- `Config::default()`: `speed 1.0`, `server_heartbeat_ms 0`,
  `warmup_ns 86_400_000_000_000` (24h), `account_id` from
  `DEFAULT_ACCOUNT_ID = "MOGWAI-001"`. `gap_cap_ms` no longer exists anywhere in
  the workspace, and `sim_epoch_ns` is no longer a config key at all - it is
  DERIVED as `TAPE_ORIGIN_NS + warmup_ns`, and a config file stating it is
  refused by the parser.
- `account_id` is validated for the `ISSUER-NUMBER` shape at load, which is a
  NAUTILUS rule enforced by a crate that does not import nautilus. The venue's
  own wire type accepts a bare word; nautilus cannot construct an `AccountId`
  from one, so a venue reporting `MOGWAI` booted fine and was refused by every
  consumer.
- `SYNTHESIS_TICKS_PER_SEC = 2_900_000`, the boot projection's rate. MEASURED,
  not chosen - see the warmup section of `reference/performance.md` for the runs
  and the method. It read 5_000_000 for a while, making the projection 1.7x
  optimistic and the 60-second WARN threshold fire at about 104 seconds.
- Lifecycle timeout consts: `SHUTDOWN_GRACE 5s`, `TAPE_SLEEP_POLL 20ms`,
  `TAPE_HEADROOM_POLL 5ms`.
- Channel capacity `1024` duplicated inline for the writer channel and the
  exec-delay pump channel (different traffic classes, no shared const).
- Synthesis limits: `CHECKPOINT_K 262_144`. The test-side `HORIZON_S 86_400.0`
  stands in for the production `warmup_ns` default as a plain literal and can
  silently drift from it.

### mogwai-adapter

- `base_url` is now required on both configs (no default endpoint); a launcher
  learns it from the readiness record. `for_addr` builds a config from the
  reported address; `for_run` also captures `expected_run_seed` from the record,
  which is what binds a client to a RUN rather than to an address that may be
  reused. Builders cover havoc, oms type, account type and trader id.
- `expected_run_seed` unset dials blind, the historical behaviour. Set, every
  dial checks `/health` and a different run is refused TERMINALLY, logged as
  `venue identity mismatch`. Two non-answers are deliberately not mismatches and
  are reported as distinct categories: no usable answer is a transport failure,
  a well-formed answer carrying no `run_seed` is version skew.
- `MOGWAI_VENUE_STR = "MOGWAI"` (correctly single-sourced).
- Default `TraderId` and local Nautilus `AccountId` labels are `MOGWAI-001`.
  The account label is not sent to the one-ledger venue.
- Timeout consts: `ACCOUNT_REGISTRATION_TIMEOUT 5s`, `ACCOUNT_REGISTRATION_POLL
  10ms`, `MIN_WALL_REQUEST_TIMEOUT_SECS 1` (flagged in its own comment as the
  tightest cap on usable sim speed). `wait_connected` re-hardcodes an
  independent 5s/10ms pair matching the registration consts by value but not
  sharing them.
- `1_000_000_000` (nanos-per-second) repeated inline 5+ times across `client.rs`
  and `lifecycle.rs` - a `NANOS_PER_SEC` const would remove the repetition.
- Triplicated test `def()` instrument fixture (`price_precision 2`/`size_precision
  8`) across three test modules.

### mogwai-data (generator)

Fingerprint/distribution constants are named module consts, fitted-and-committed
by design (changing them re-shapes the synthetic market): quiet share 0.35,
state persistence 0.90, quiet/active mean ratio 150, Weibull shape 1.0,
GARCH 0.12 / 0.875, Student-t df 4.0, bounce and drift transition
probabilities, `SIZE_LOG_SIGMA 1.15`, `MAX_ABS_RETURN 2e-5`,
`GARCH_SIGMA_CAP 1e-5`, anchor `START_PRICE_USD 60_000`, and
`VOL_SCALAR 1e-6`. The real fingerprint numbers live in
`analysis/fingerprint.json` (embedded via `include_str!`), not in Rust.

Inline (not named): `xbtusd_anchor` fields `XBTUSD` / `modal_tick 0.1` /
`price_decimals 1` (deliberately per-pair, kept in the constructor); the `1e9`
mid-price runaway ceiling duplicated at two sites; `round_lot_size` thresholds
(1.0 / 10.0 / 0.1). `seed`, checkpoint `k`, and `max_extend` have no production
default here (caller-supplied by the server); seed `42` is the pervasive
golden-test seed.

### Non-crate (scripts, analysis, root config)

- `scripts/smoke.py`: spawns its own venue and learns the bound address from
  the readiness record read off the child's stdout (no hardcoded host/port). `WINDOW_LOOKBACK_NS 1h`, `ACCEL_DELAY_MS 1000`,
  `ACCEL_CLOCK_SLACK_WALL_NS 50ms`, `ACCEL_ANCHOR_TIMEOUT_S 120`, fixed order
  shape (`BTCUSDT`/`Limit`/qty 10/px 100), plus many inline per-assertion
  socket timeouts and latency tolerances (not centralised; first place to look
  if the smoke ever gets flaky).
- Orchestration: the `review` tool, configured from `.review.toml` - the codex
  wrapper scripts were removed in favour of it. Critique runs `review bare
  --profile deep` (gpt-5.6-sol, xhigh, read-only); implement runs `review goal
  --profile build` (gpt-5.6-terra, medium, workspace-write). `[_defaults]`
  pins the provider to `codex`. `prevent-harness-bug.sh` default sleep `60`.
- Smoke fixture configs `smoke-accelerated.toml` (`speed 100.0`) and
  `smoke-heartbeat.toml` (`server_heartbeat_ms 100`) - by-design knobs.
- `analysis/`: `MAX_LAG 50` in `characterize.py` with `build_fingerprint.py`
  hardcoding ACF indices `[9]`/`[49]` as lag10/lag50 (hidden coupling - changing
  MAX_LAG silently breaks the indices); `TICK_DICT_CAP 500_000`, histogram bin
  counts, `run_corpus.DEFAULT_PAIRS` (8-pair subset) with the worker pool capped at
  6, `recon.TAIL_BYTES 8192`, `ANCHOR "XBTUSD"`, and a day-of-week convention
  re-derived in three files instead of shared.
- Root `Cargo.toml`: workspace dep version pins (serde 1, tokio 1, axum 0.8,
  rust_decimal 1 with serde-with-str, rand 0.10, rand_distr 0.6, rand_chacha 0.10,
  and the rest) centralised as workspace deps; `[profile.release]` opt-level 3 /
  lto fat / codegen-units 1; `rust-version 1.96`, `resolver 3`. The nautilus
  deps live in `mogwai-adapter/Cargo.toml`, not root, and are five crates.io
  dependencies pinned at 0.61 with default-features off. `brokkr.toml` only sets
  `project = "mogwai"`. Root `mogwai.toml` is an EXAMPLE run config, not one the
  server reads (nothing consults the working directory): `speed 1.0`,
  `server_heartbeat_ms 0`, `run_duration_ns 0`, `warmup_ns` 24h,
  `fanout_depth 65536`, `zero_speed_stall_ms 5000`, the fill-band and admission
  knobs, and the funded `balances` table. It states neither `sim_epoch_ns` nor
  `wall_anchor_ns` - both are derived at boot, and the former is refused as a
  key.
