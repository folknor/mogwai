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
   per-run mode, and has not landed: the venue still holds one ledger for the
   whole process and fans every fill out to every connection.
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

## Open issues

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
  THE SPEED REFUSAL IS AN ARTIFACT, not a semantic, and it is recorded here so
  nobody later defends it as intended behaviour. It exists because a cache was
  promoted to a concept: sharing one cursor forces everyone served from it onto
  one cadence, and the refusal is that constraint surfacing on the wire. Speed
  mutates no generated value, so a passenger asking for an unserved speed is
  asking for a second cursor on the same water. The end state serves it.
  OPEN, and created by that landing: the fill sweeper now iterates over seated
  CURSORS, so a resting order on a river whose cursor wound down is no longer
  swept until someone connects to that river again. It cannot be swept without
  one - there is no clock to sample a `to_ns` from - so the honest fixes are
  either to keep the venue clock as the sweep instant for unseated rivers or to
  refuse to leave orders resting on a river nobody is reading. Neither is piece
  9's, and piece 10 (below) left it explicitly untouched too.
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

- FUNDING: was CLOSED, REOPENED 2026-08-16 by the account-policy ruling above -
  the boot-checks-plus-bind-refusal design below assumed a venue-level
  `[balances]`, which is being deleted. What survives is the SHAPE half: the set
  of shapes is still closed at boot, so a symbol still contributes no currency
  and a request whose named balances do not cover its shape's currency is still
  knowable without an order. What moves is WHERE: from a boot check against
  operator config to a CONNECT-TIME check against what the client named, which
  makes it a bad REQUEST rather than a bad configuration. The
  mismatch-versus-depletion distinction below is unaffected and still the
  reason to keep the two refusals apart. The original text follows.
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

- GATE DURABLE PROSE THAT ASSERTS A LIVE FACT. The tape-version half of this
  item is CLOSED, 2026-08-16: `crates/mogwai-data/tests/tape_version_prose.rs`
  walks every markdown file in the repository - the set from a walk, never a
  hardcoded list, because the third occurrence was in `notes/` - and checks two
  claim forms against the live constant. The convention is stated in AGENTS.md
  under Tape protocol version. It bit on its first run, finding the 12b spec's
  index preamble still claiming the live identity was 11.
  WHAT REMAINS OPEN is the general kind, and it is why a narrow version-only
  gate was only a partial fix. `reference/architecture.md` carried a 12.6 ms market-reading cost that a
  stride change had superseded; `checkpoint.rs` argued its safety from a
  `BoundedSeek` type that had been deleted from the tree, in three separate
  comments; `tick_composition_ratios.rs` claimed to decide `CHECKPOINT_K`,
  which stopped being true when that constant became a latency tradeoff rather
  than a reach ceiling. A gate on a named constant catches the first kind. The
  general kind - durable prose asserting a measured number or a live type -
  wants either a citation convention that can be checked (a symbol name a test
  can resolve) or a review habit of grepping the identifier before deleting it.
  Neither is built. The version gate's shape is the available precedent: a
  claim form a test can recognize, applied to prose that means to assert a live
  fact, leaving every historical record alone.

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

- TWO STRUCTURAL OBSERVATIONS from that diagnosis, neither with a known
  reproduction, both worth a decision before the serving path grows.
  (a) PUBLICATION ORDER IS NOT MUTATION ORDER. `dispatch_command` releases the
  engine lock inside `process_order_cmd` and only afterwards calls
  `lanes.submit_produced`, so between an order becoming visible to the sweeper
  and its `OrderAccepted` reaching the connection's outbound sequence there is
  a window in which a sweep can commit a causally dependent fill first. The
  engine mutex establishes mutation order and not publication order. The
  invariant it wants: for every connection, execution-event publication
  preserves the causal order of committed engine transitions, so if B can
  occur only because A committed, A's complete batch is committed to that
  connection's stream before B's. LATENT - the test above was its only cited
  evidence and does not in fact show it, so nothing currently demonstrates the
  window is reachable.
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
  TRANSPORT HAVOC RIDES THE PASSENGER, 2026-08-16. `GoDark` and `StallData`
  windows moved from the run onto the account, and `/control/divergence` takes
  an optional `account` naming whose view to corrupt; absent still means every
  account, which is what an operator on a single-account venue wants and what
  every existing scenario file already writes. A clear still clears everyone,
  since a clear is an operator saying stop everything. Generator arms are
  untouched: they change the WATER, which belongs to the river and reaches
  everyone reading it whatever account they trade.
  STILL OPEN, none of it blocking: nothing FREEZES an unattended account, so an
  account whose connection drops is simply not marked until someone returns -
  the intended behaviour, but untested and not stated anywhere a consumer reads;
  there is no TTL collecting an account nobody reclaims; and the ack/act latency
  knobs (`DelayAcks`, `CommandLatency`) are still run-wide, which is the same
  defect family as the blackout was and now has the same fix available.

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

- THE RISK POLICY IS EVALUATED ON THE MARK, NOT PER TICK, which contradicts the
  ruling that peak equity tracks every tick. Landed knowingly 2026-08-16 with
  the account-policy work; recorded here because nothing else will surface it.
  WHY IT MATTERS. The trailing threshold ratchets on peak equity, and at a real
  venue that ratchet is effectively tick-by-tick: a spike lasting 200 ms still
  spends budget. `enforce_policy` runs once per fill-sweeper pass per boat, so a
  spike that opens and closes entirely between two passes never happened as far
  as the account is concerned, and it keeps room it should have lost. At the
  default one-second sweep interval under acceleration that window is short in
  wall time and a full second in SIM time, which is the axis the policy is
  stated on.
  THE GAP IS RESOLUTION AND NEVER DIRECTION, which is the reason it was
  acceptable to land: every peak the evaluation DOES see ratchets exactly as it
  should, so enforcement is uniformly LENIENT rather than wrong. An account is
  never liquidated for a spike that did not happen; it is sometimes not
  liquidated for one that did.
  WHAT CLOSING IT NEEDS, and it is why this was not just done: the evaluation
  would have to run in the tape thread, which cannot take the engine lock - the
  whole sweeper design exists because the tape walk must stay off it. So it
  wants the equity INPUTS published out of the engine (position quantity, avg
  price, balance) into something the tape thread can read lock-free, with the
  policy evaluated against the tick price there. That is a real piece of work
  and it touches the hottest path in the venue, so it wants a measurement
  naming what it costs before it is taken.
  NOT THE SAME as tightening `SWEEP_INTERVAL_NS`, which would buy resolution
  everywhere at a cost the fill golden's own item already describes.

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
  ORDER LISTS ARE THE STRUCTURAL ONE. mogwai models no linkage between orders at
  all, so a genuine bracket where one fill cancels its sibling cannot exist.
  broadarrow works around it with two independent reduce-only legs plus
  stale-cancel reconciliation, which real venues also permit - but that is a
  workaround for a missing primitive, not the primitive.
  ABSORBS the former standalone GTD item, whose content follows: `Gtd` needs an
  `expire_time` on the wire plus a time-driven expiry pass on the sweeper that
  has nothing to do with triggers. The conditional-order-type landing carried a
  GTC-only rule for stops for exactly this reason.

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

- RESTORE DISCRIMINATION to the fill golden's banded half. Found 2026-08-03 while
  re-calibrating `fill_band_vol_mult` from `0.5` to `0.005`: the re-blessed
  `crates/mogwai-server/tests/golden/fill_distribution.json` now has its five
  banded cells BYTE-IDENTICAL to its five unbanded ones - same fill counts, same
  latency vectors, same pass counts. The banded half therefore certifies that the
  band pipeline RUNS, not that the band BITES, and a regression that silently
  zeroed the band would still pass this golden.
  The cause is resolution rather than calibration. Latency is quantized to the
  harness's one-second `SWEEP_INTERVAL_NS`, and one second of raw-fill tape
  carries roughly fifty prints travelling much further than the 0-to-4 ticks -
  about 0.1 basis points on a 37,000 tape - that a `0.005` band displaces a
  trigger by, so the tape crosses the displacement inside the same sweep pass.
  The old `0.5` discriminated only because it was clamp-saturated at 200 ticks,
  which is the defect the re-calibration removed; this is the bill for fixing it,
  not a new regression.
  Two knobs restore it, both costing runtime in a harness whose coverage was
  deliberately cut for runtime: a finer `SWEEP_INTERVAL_NS`, so sub-second
  latency differences are representable, and a tighter offset ladder, so the band
  is a large fraction of the distance to the limit rather than a rounding error
  against it. Neither was taken. A third option is to stop asking this artifact
  the question and add a direct assertion that a banded trigger differs from its
  stated price, which is cheap but proves much less.

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

- mogwai-engine `next_position` unbounded accumulation. The per-fill weighted-
  average is now overflow-guarded (a single oversized order is rejected before
  it reaches the arithmetic), but `current.qty` still accumulates across many
  individually-valid orders on one symbol/side, so a long-lived engine can
  overflow the `current_abs * avg_px + delta_abs * px` computation over time.
  Closing it means introducing a position-size or notional cap - a design
  decision, not a local fix.

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

- DECIDE: does dup/drop havoc reshaping fabricated bars model the right venue?
  (Formerly sweep item AD21a.) Bars are built AFTER the `HavocFilter` on both the
  WS and poll paths, so a dup or drop of one trade silently reshapes OHLCV rather
  than duplicating or dropping a whole bar frame. Bars here are FABRICATED by the
  adapter - the server never ships one - so deriving them from a corrupted trade
  feed is what a real client-side aggregator on a lossy feed experiences, and is
  arguably the honest simulation; the alternative models a venue that ships bars
  natively, which mogwai is not. Leaning accept-and-document, on the same
  principle that settled the reconnect account staleness: mogwai injects faults
  and declines to repair them downstream. (The reorder half of the original item
  was a different finding and is closed - `fold_trade` now documents an ordering
  EXPECTATION with a defined failure mode, names the adapter as a deliberate
  violator under `reorder_prob`, and is pinned by
  `an_out_of_order_trade_folds_into_the_open_window_without_wedging`.)

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

- The dwell definition is computed TWICE and the gate compares one against
  the other. `dwell_stats` (now `crates/mogwai-lab/src/characterize/mod.rs`,
  ported from `analysis/characterize.py`) measures the corpus;
  `empty_hour_stats_over` in `crates/mogwai-data/src/generated/tests.rs`
  measures the synthetic tape. If the two hour-bucket conventions ever drift -
  inclusive end boundary, the era-start ceiling, which trade closes a gap - the
  gate silently compares two different quantities and still passes. Surfaced
  2026-08-02 landing the drought elimination.

  The Python-test-harness half of this item is CLOSED by the rewrite: both
  implementations are Rust now, so there is no second toolchain to decide about
  and `analysis/test_characterize.py` dissolves at phase 4b. What survives is
  the twice-computed definition itself, which the port did NOT collapse - it
  moved one copy from Python to Rust and left the other where it was.

  Two ways to close it: have the generator test call
  `mogwai_lab::characterize::dwell_stats` directly (mogwai-data would gain a
  dev-dependency on mogwai-lab, which is the wrong dependency direction and may
  not be acceptable), or keep both and pin them against one shared fixture the
  way `roll_estimator`/`spread_conformance.json` does. The second is cheaper and
  matches existing precedent.

  STALE CITATION to fix whichever way it goes: `tests.rs` names
  `analysis/characterize.py` as the counterpart it must match, at its
  `empty_hour_stats_over` doc comment and again in
  `empty_hour_stats_use_complete_utc_buckets`. That file retires at 4b; the
  comment should name the Rust `dwell_stats` instead.

- REPAIR the `brokkr check --gate` profile mismatch for
  `tape_lateness_under_acceleration`. The gate runs the workspace test pass in
  DEBUG profile, and that test asserts a 50ms p99 WALL-CLOCK pacing bound the
  debug server cannot reliably meet (measured 2026-08-05: debug p99 106-330ms
  across trees whose release builds pass at 38-43ms on a quiet box). The test
  is also load-sensitive even in release: with a second workspace building
  (load average 1.0), 4 of 5 release repetitions failed at ~250ms on BOTH the
  protocol-9 parent and the protocol-10 candidate - indistinguishable paired
  distributions, recorded in `notes/protocol-landings.md` (protocol 10) as a
  reviewed gate exception. The 50ms release threshold stays authoritative and
  unrelaxed.

  HALF DONE, 2026-08-08. The debug-lane half is closed: the test is now
  excluded from the `gate` profile BY FULL NAME, on the argument that running
  a release wall-clock contract against a debug binary is already a changed
  measuring instrument, so that lane's red result was never evidence about
  the property. The assertion is untouched and the test stays directly
  runnable. Excluding it makes no claim that this host meets the budget.

  WHAT REMAINS is the environment sensitivity, and it is worse than the
  original note implies: a release rerun on 2026-08-08 failed at 311 ms p99
  with a load average of only 1.46 across 32 visible CPUs. So a load-average
  precheck is NOT a sufficient admission test - the earlier "load average 1.0"
  reading suggested one might be. Whatever admits this test needs to be a
  property of the machine that actually predicts the failure, and nobody has
  found it. Until then a release run of this test is informative when it
  passes and ambiguous when it fails.

- UNPROVEN, and it decides whether the venue-identity check needs to stop being
  opt-in: can a full session be established against a stranger holding a reused
  port, and this client's account id stamped onto its state, inside the window
  before anyone notices? An external QA pass forced the port reuse and showed
  the adapter DOES dial a dead venue's address and a stranger DOES accept the
  connections, but their stranger was a bare TCP listener that accepted and
  closed, so the stamping half was never demonstrated. Their bound on the window
  (about 160 ms) came entirely from the consumer's own child-exit poll, which
  covers nothing for a consumer that does not own the venue as a child, and
  nothing at all when the venue is wedged rather than exited.
  `expected_run_seed` closes it for anyone who sets one; what is undecided is
  whether a config WITHOUT one should keep dialling blind. Answering it needs a
  stub venue that speaks enough of the wire to complete a handshake.

- The venue frees its port BEFORE it exits: a declared completion stops the
  accept loop, then drains live connections for up to `SHUTDOWN_GRACE`. So the
  address is reusable while the process is still alive, which is why a consumer
  watching for child exit sees nothing during that window. Not a defect - the
  drain is deliberate - but it is the mechanism behind the item above, and worth
  keeping in view before anyone shortens or lengthens the grace.

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

ACCEPTED 2026-08-16, as a no-brainer rather than a considered fork: it is one
arm parallel to `RejectNextSubmit` on machinery that already exists, and it is
the only way a consumer's real live-path defect becomes testable anywhere.
Queued as a discrete implementation task.

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

SPEED IS SERVED, NOT REFUSED. Today a socket asking for a speed the seated
cursor does not carry gets a `400` before the 101. That refusal is the shared
cursor showing through onto the wire, not a statement about the model: speed
does not change the generated WATER, only its delivery cadence, so a passenger
asking for an unserved speed is asking for a second cursor on the SAME river.
An earlier draft called this "speed splits the river", which overstated it -
river identity never contained speed, and two cadences over one river share the
whole checkpoint chain underneath.

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
primitive.

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
needed. Trailing stops remain the one refused shape, by our ruling and theirs.

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
