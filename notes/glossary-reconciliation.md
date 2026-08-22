# The glossary reconciliation arc

The standing procedure for reconciling this repository's vocabulary with
`reference/glossary.md`, repo-wide, down to landed commits.

It is `orchestrate hunt` and `orchestrate bugs` recombined, with one phase
neither of them has and two of their rules deliberately reversed. Read this
document rather than either template when running this arc.

## What this arc is for

The glossary states what each word means here. The code does not always agree,
and where it does not, the cure is almost always to rename the code rather than
to lengthen the entry. The standing ruling, recorded in `notes/todo.md`
2026-08-20: the glossary's end state is uniform usage, one purpose per entry,
across the entire codebase. An entry that has to disambiguate is a stopgap
documenting a disease rather than curing it.

Three known standing cases seed the arc - `client`, `connection`, `session` -
and the working assumption is that they are a small fraction of what is there.

## What makes this arc different from a bug hunt

A bug arc inherits its verifier: revert the fix, watch the named assertion fire.
This arc has the compiler for identifiers and nothing at all for the thing that
actually matters, which is whether a word and its job agree. Prose is the only
artifact in this repository with no compiler, and this arc is mostly prose and
identifiers. The verifier has to be built rather than inherited, which is what
phase 5 is for.

## Reconciliation runs in three directions

Scoping this as "find where the code disagrees with the glossary" throws away
most of the yield. Every scope answers all three:

1. **The code is wrong.** A glossary term names something that is not that
   term's job. Cure: rename the identifier, sweep the prose.
2. **The glossary is wrong**, and this direction is narrower than it looks -
   see the section below, which is the single most important thing in this
   document. It covers vocabulary defects only: an entry that disambiguates
   rather than defines, one that contradicts itself, one that is silent where
   it must not be. Cure: fix the entry.
3. **Neither exists.** A load-bearing noun in the code that the glossary does
   not define at all - `BoundLane`, `Ticket`, `Admission`, `ArmRecord`,
   `RiverKey`, `SocketSession`, frontier, cursor, sweep, and whatever else the
   inventory turns up. Cure: an entry, a rename onto an existing entry, or a
   ruling that it is implementation detail and stays out. This direction is
   expected to be the largest, and the wire scope showed why: it decomposes
   into whole vocabularies missing together, not into isolated nouns.

## The glossary is the end state, and the code is not evidence against it

The single rule this arc keeps getting wrong, stated by the owner three times
before it stuck.

The glossary describes the venue we are building, not the venue that compiles
today. So when an entry states behaviour and the code does something else, the
finding is that the code owes the behaviour. It is not a reason to correct the
entry, and "the implementation deliberately does otherwise, with machinery built
for it" is not a counterargument - it is a description of the gap, and possibly
of work that needs undoing.

The worked example, because three passes across two crates got it backwards.
The Divergence entry says generator arms are part of river identity and fork the
river. The tape machinery deliberately mutates a canonical boatless river
instead, with a named snapshot mechanism built to make that correct. Two server
passes read the missing fork as a defect against the design and were right; one
generator pass read the mechanism as proof the entry was stale and was wrong.
The entry stands. The fork is owed.

What this leaves as a genuine glossary defect is narrow and is direction 2
above: vocabulary problems, not behavioural mismatches. An entry that
disambiguates instead of defining. One that contradicts another entry. One that
leans on a word it never defines. Those are real and this arc has already fixed
several.

And it makes the most valuable output of the whole inventory a category the
first briefs did not ask for: **an entry the code has not caught up with**.
That is the roadmap, and it is worth more than any rename in the ledger. Report
it as its own direction rather than burying it as a glossary defect.

A fourth category is not a defect and must not be renamed: vocabulary this
project does not own. Nautilus's API names (`DataClient`, `ExecutionClient` and
kin), and domain-standard trading terms that are immovable because the industry
uses them, the trading sense of `session` being the glossary's own worked
example. These are quarantined behind their APIs and named as inherited in the
glossary. An inventory row that proposes renaming one is a mistake.

A wire field is not in that category merely for being on the wire. Renaming one
is a designed break with a version bump behind it, and it is in scope. Phase 4
gives those their own rounds so an internal cleanup cannot hide inside one.

## Roles

| Role | Who | Sandbox |
|---|---|---|
| Orchestrator | Main session | Full |
| Inventory pass A | Claude subagents, foreground | Read-only in practice |
| Inventory pass B | `review bugs --profile deep` | read-only |
| Merge | Main session | Full |
| Ruling | The owner | - |
| Rename pass | `review bugs --profile build` | workspace-write |
| Cold review | `review bugs --profile deep` | read-only |
| Fix-and-commit | Claude Opus subagent, foreground | Full |
| Close | Claude Fable subagent, foreground | Full, carte blanche |

**There is no adjudicator role.** `orchestrate bugs` sends forks to a Fable
adjudicator and never to the human. That rule is reversed here: naming is the
owner's gate, and a consensus gate over what a word means converges to the
verifier's utility function rather than to the right word. Every naming fork
goes to the owner.

## Phase 1: inventory

Read-only throughout. Nothing is renamed in this phase. An inventory that starts
editing has stopped inventorying.

### The scopes

Eight, cut by contract surface rather than by crate. What matters is what one
part of the system promises another and what the venue promises the outside: the
wire, the routes, each crate's public API, the operator contract, and the
durable prose. A word that is wrong on a contract is wrong everywhere the
contract reaches.

A scope is a surface, and any list of types inside it is examples. State that in
the fill. Pass B on scope 1 read an enumeration as a fence and refused about ten
rows as out of scope that were never meant to be outside it, which cost a whole
cluster its second reading. Where a boundary really is exhaustive, say so in the
words "and nothing else".

| # | Scope |
|---|---|
| 1 | The wire: the whole of `mogwai-protocol`'s public surface. `Command`, `ServerMessage`, `control::Divergence`, `close`, `risk`, `launch` and `ReadyRecord` are the load-bearing parts, not the boundary |
| 2 | The venue's external surface: `mogwai-venue`'s routes, query structs, JSON bodies, status codes, operator config keys, and every refusal, error and log string a consumer or operator reads |
| 3 | The venue's internal domain model: `Run`, `Passenger`, `Boat`, `Boatyard`, the lane and seat apparatus, the sweeper, and the names they use among themselves |
| 4 | `mogwai-engine`'s public API |
| 5 | `mogwai-data` and `mogwai-lab` public APIs - the `TickSource` seam, the sources, `segment`, the measurement and fit surfaces |
| 6 | `mogwai-adapter`'s public surface - configs, factories, the client pair - and the inherited-vocabulary boundary |
| 7 | The operator contract: `mogwai-cli` subcommands and flags, `mogwai.toml` keys, preset and policy names, and `docs/cli.md`, `docs/config.md`, `docs/presets.md` |
| 8 | The durable prose: `reference/`, the rest of `docs/`, `AGENTS.md`, `CLAUDE.md` |

Out of scope for the inventory, deliberately: tests, benches and examples, which
follow the compiler mechanically in phase 4; and `notes/`, `analysis/` and
`scripts/`, which promise nothing and which nothing durable may cite. A hunter
that finds a naming defect there anyway files it as a lateral finding rather
than a row.

Scope 3 carries a question the others do not. The glossary makes structural
claims about those objects - what a boat does not reveal, what a passenger is
one of, what a river's identity contains. Those are checkable against the code,
and where one is false it is a finding about the system rather than about a
name. Ask for it explicitly in that fill.

### Two passes, sequential, never simultaneous

**Pass A** produces a per-scope report, foreground.

**Pass B** runs over the same scope after pass A's report exists, by a different
agent, with that report in hand. It is not a critique of the report: it does the
same scoped work independently and then reconciles, so its output is a revision,
with rows added, corrected and refuted. Refuting a row is worth exactly as much
as adding one, and the brief says so, because the cost of handing pass B the
first report is anchoring and that is the only mitigation available.

Pass B goes out as `review bugs --profile deep`, which gets a genuinely
different reading rather than the same model twice. It has earned its place: on
scope 1 it added a whole missing taxonomy and refuted six rows with arguments,
and on scope 3 it confirmed three structural claims with worked readings and
narrowed two more.

### The ledger row

Uniform rows are what makes the merge possible. Prose reports do not merge.
Every site is one row:

    term | site | kind | what it means there | reach | direction | verdict

- **term**: the glossary word, or `-` for an undefined noun.
- **site**: file and identifier or the quoted phrase. No line numbers, they drift.
- **kind**: identifier, type, variant, field, module, constant, route, query
  parameter, status code, doc prose, refusal text, log text.
- **what it means there**: the job the word is actually doing at that site. This
  field is what later makes a classification possible instead of a substitution.
- **reach**: crate-local, across a crate boundary, or externally visible. This
  is what orders the work.
- **direction**: 1, 2, 3, structural or inherited.
- **verdict**: the hunter's proposal, stated plainly.

### The brief

Carried from `orchestrate hunt` and binding here:

- Scope the investigation, not the report. No caps, no maximum row counts.
- Do not prescribe how to report beyond the row format, which exists only so the
  merge is possible.
- Invite lateral findings up front - a bug, a smell, anything surprising, even
  outside the naming question.
- Name the question, not the method. Do not prescribe tools.
- Do not restate rules the agent inherits from `AGENTS.md` and `CLAUDE.md`.
- State verbatim in substance: do not default to conservative or minimal
  suggestions; if the right move is a big rewrite, say so; do not preserve
  structure or abstractions because they exist; assume pre-1.0, breaking
  internal API is acceptable; prefer structural opportunities over
  micro-optimizations.

And specific to this arc:

- `reference/glossary.md` is a mandatory read, in full.
- All three directions are in scope, and direction 3 is expected to be the
  largest. Report at the cluster level as well as the row level.
- Inherited nautilus and industry vocabulary is not a defect.
- The glossary is the end state. Where an entry states behaviour the code does
  not have, the code owes it: report that as a gap, and never as a reason to
  correct the entry. A glossary defect means a vocabulary defect - an entry that
  disambiguates instead of defining, contradicts another entry, or leans on a
  word it never defines.
- What is being reconciled is the contract: the names one part of the system
  promises another, and the names the venue promises the outside.

### Filing

Reports land as `notes/glossary-scope-<n>.md`, transient by definition. Pass B
revises the scope's document in place rather than writing a second one.

## Phase 2: the merge

One pass, in the main session, not delegated: it is the cross-cutting
reconciliation that the bug arcs found indispensable and it needs the whole
picture in one head.

Output is `notes/glossary-ledger.md`, organised by term family rather than by
scope, holding the collisions (one word doing several jobs), the orphans (one
job wearing several words), the undefined nouns grouped by whether they look
like domain vocabulary or implementation detail, the glossary's own defects, and
the inherited list, which becomes glossary prose rather than work.

Deduplicate across scope boundaries, keeping the fuller writeup. When one defect
appears in three scopes, say so as a fact about the ledger. Do not rank, soften,
agree or disagree: the hunter's confidence is data, the orchestrator's is not.

## Phase 3: the ruling

One document, term families ordered by blast radius, each carrying its sites,
its proposed word, and its cost. The owner goes down it.

The orchestrator's job here is to make most rows confirmations and to surface
the genuine forks at the top. No agent rules on any of them. Every ruling is
recorded in the carry-forward before phase 4 opens, because a half-ruled term
family is worse than an unswept one.

A ruling is recorded with its grounds, not only its verdict. Two of the rulings
below overturned an agent position that was internally coherent, and the grounds
are what stop it being re-argued in a later round by an agent that cannot see
this conversation.

## Phase 4: execution

**Serial, one term family per round.** This is the second reversal from the
templates: a rename cross-cuts every file in the workspace, so the strict file
ownership that makes parallel agents safe is structurally unavailable here. Two
agents renaming two term families at once collide by construction.

The round, per term family:

1. **Rename pass.** `echo '<brief>' | review bugs --profile build`, background.
   The brief carries the ruling, the ledger rows for that family, the inherited
   and untouchable list, the carry-forward slice, and: do not commit.
2. **Orchestrator gate.** `brokkr fmt`, then `brokkr check --gate` - the gated
   form, not plain `check`, whenever the round touched `mogwai-adapter`. Note
   the test count.
3. **Cold review.** `echo 'Please read AGENTS.md and reference/glossary.md, plus
   any other reference docs you judge relevant. Then please critically review
   all unstaged changes.' | review bugs --profile deep`. Keep it impoverished.
   Do not name the term, do not describe the ruling, do not prime it. A cold
   reader is the only check on whether the new word reads better than the old.
4. **Fix-and-commit.** Foreground Opus subagent. Cold-review findings restated in
   full, the rename pass's claims flagged as claims, docs kept honest, full
   unscoped gate, one commit on master, no push.

A family whose ruling was a classification rather than a substitution cannot be
handed over as a single instruction. The brief carries the row list with each
site's job already recorded, and a site fitting none of the recorded jobs is
escalated rather than defaulted.

Wire-visible renames get their own rounds at the end of the phase, each with its
version bump, so an internal cleanup can never hide inside one.

## Phase 5: the conformance gate

Built in the last round, and the reason the arc is not merely a snapshot.

A test in the shape of `crates/mogwai-data/tests/tape_version_prose.rs`: a scan
over sources and markdown for the words this arc retired, with an explicit
allowlist for inherited vocabulary. Nothing else detects the next drift, and
this repository has watched durable prose rot before for exactly this reason.

The allowlist is the load-bearing part. A scan with no allowlist is a scan
somebody will disable.

## Phase 6: the close pass

One Fable subagent over the whole commit arc, carte blanche. Brief per
`orchestrate bugs`, plus the fact that binds hardest here: the second half of
every commit was never cold-reviewed, and across eleven documents of the 2026-08
arc the defect was in that half eleven times out of eleven. Point it at the
durable prose the arc touched and at the call sites of whatever the last round
installed.

Ask for: what it found, what it fixed, what it left, and whether the glossary
and the code now say the same thing.

## Carry-forward

Maintained in this document, updated after every round, because no agent in the
loop sees any round but its own.

### Ruled

**`server` loses to `venue` for the process. Owner, 2026-08-21.**

The grounds are the glossary's own. Server mode and Transient mode are both
defined as "the venue launched by X", and the entries state that the venue's
semantics are identical in both. So the process is a venue in both modes, and
`server` names only one of the two modes it can run in. A transient venue is not
in server mode, yet it sends `ServerMessage` and reports `server_now_ns`. The
word is doing two jobs.

This overrules the recurring pass-B position that `client` and `server` are
endpoint-polarity vocabulary legitimately coexisting with Venue. That argument
treats `server` as a free word available for protocol direction, and it is not
free: it is a mode name.

**The counterparty is a `consumer`, and it is not a process. Owner, 2026-08-21.**

A consumer is the program or system driving the venue. It may be one process,
several sharing nothing but the wire, or the very process the venue is embedded
in, since `mogwai-venue` is a library. So there is no process-shaped object at
the other end to name, and the venue never perceives a consumer at all: what it
perceives is a session id, an account and its connections.

`host` was considered and rejected. It is already taken in this repository for
the machine - `.review.toml` resolves profiles per host, `AGENTS.md` states that
rule, `reference/performance.md` records measurements on host `bygg` - so
renaming onto it would import a fresh collision to cure an old one. It is also
over-specific: the glossary says "typically a nautilus host", and the protocol
is native JSON over websockets, so a non-nautilus consumer is a supported shape.

Two consequences follow, and the second is easy to miss.

The word for the party on one socket is Session, never Consumer. A frame does
not arrive from a consumer, it arrives from one connection under one session.

So the inbound wire type is named for what it carries rather than for who sent
it. There is no singular party to name it after, and `ConsumerMessage` would be
wrong for the same reason `Command` is. The crate already classifies its
inbound traffic as commands and its outbound as events, which is the naming that
depends on nothing unobservable.

**The `client` sweep is a classification, not a substitution.** A global rename
would move the overload onto a new word rather than curing it. Every site is
classified by the job it does, and the jobs visible in the inventory so far:

- The consuming program or system: becomes `consumer`.
- Nautilus's adapter objects inside it, `MogwaiDataClient` and
  `MogwaiExecutionClient`: inherited, quarantined behind their API, never
  renamed.
- The submitting side's id namespace, `client_order_id`: inherited from nautilus
  and FIX. This is not a separate sense of ours - the submitting side is the
  consumer - and it stays only because the spelling is inherited.
- Havoc the adapter applies to its own inbound stream, `InboundHavoc` and
  `HavocSpec.client`: not the consumer sense at all, and wrong under any
  reading. Takes an adapter or inbound word, not `consumer`.
- Any consumer-supplied echoed id, `MAX_ECHOED_ID_LEN` and `truncate_echoed_id`,
  covering `client_order_id`, `request_id`, `order_list_id` and `position_id` at
  once. Takes an echoed-id word, not `consumer`.

A site fitting none of these is escalated, never defaulted to `consumer`.

### Glossary edits already landed

Made during the rulings above, so later scopes read the corrected document:

- **Boarding** is now an entry. It is the act, at connect, by which a
  passenger's resolved config selects its water, and it is the moment identity
  is decided. Two things it fixes: the carrier of a knob decides nothing, so a
  divergence posted to the control plane and one read from config are the same
  input by the time it matters; and the river key is whatever in that resolved
  config can mutate the water, so a key naming an existing river boards the
  passenger onto it and a key naming none creates one. The absence of this entry
  is what let a reader conclude the glossary was internally inconsistent about
  divergences and river identity. It is not.
- **Venue** no longer says "one running mogwai process". Whether it runs as its
  own PID or embedded in the consumer's program is a deployment detail.
- **Client** became **Consumer**, per the ruling above.
- **River** states that identity is resolved at boarding.
- **Divergence** states that the carrier decides nothing, and that a generator
  arm forking the river means only that a passenger carrying one boards a
  different river.
- The boot river entry no longer says the run boards a boat. A run places a
  boat; boarding is a passenger's act and a run takes no seat.
- **Session and Eviction are now stated over the session, not the consumer.**
  The first `client` to `consumer` sweep put "a different consumer" into the
  Eviction entry, which made the venue discriminate on something the Consumer
  entry says it cannot perceive - a contradiction introduced by the sweep
  itself, found by the adapter scope. Both entries now say that session is the
  only identity the venue has, and that a consumer spread over several
  processes is several sessions unless it deliberately supplies one, which is
  the consumer's act rather than an inference the venue makes.

  The lesson binds the rename rounds. A classification sweep fails not only by
  putting the new word on the wrong job, but by putting it on a sentence the new
  word's own definition forbids. The check at each site is not "is this the
  ruled sense" but "does the new word's definition permit this sentence".

**The socket identity is a `callsign`. Owner, 2026-08-21.**

The grounds are a count. The operator surface uses `session` for the market's
trading day at roughly 34 document occurrences and 29 printed-preset
occurrences, plus config tables, provenance keys and command names, while the
socket sense appears there about six times. The trading sense is domain-standard
futures vocabulary and is not ours to rename, so the glossary entry moved rather
than the operator surface.

`callsign` was chosen over `session id`, `pass`, `tag` and `badge` because it
names what the thing is: an identity announced by the party itself on connect,
conventionally honoured, with nothing standing behind it. That is the entry's
own content rather than a label on top of it. `pass` was rejected for colliding
with the boatyard's `Ticket`, and `tag` and `badge` for reading as "some string"
rather than as a claim of identity.

The wire parameter follows: `/ws?callsign=`, not `?session=`. Leaving the old
spelling would reinstate the collision at the one place a consumer types it.
This makes the ruling a designed break rather than a prose change, which is
accepted.

Closes the second of the three standing cases in `notes/todo.md`.

**A passenger is one per account. Owner, 2026-08-21.**

The glossary already said so; the durable prose was split, with `docs/havoc.md`
and one line of `reference/architecture.md` explicitly per connection, and the
code counts connections in at least four places. Under the end-state rule the
entry stands and the code owes it.

The alternative was rejected on the ground that it renames the problem rather
than solving it. Something has to hold the ledger, the risk state and the freeze
stamp across a reconnect; that object is real, it is what makes a reconnect a
continuation, and Passenger is already its name in the glossary and in `run.rs`.
Making Passenger mean a connection would leave the account-lifetime object
unnamed and need a new noun for it.

What moves instead: the boatyard's per-ticket refcount becomes riders or
tickets, a socket's declared duration becomes a Connection property, and the
`RunComplete` entry stops being evidence about Passenger at all. A count of
riders on a boat is a count of connections.

Consequence, already applied to the glossary: **boarding is per connection, not
per passenger.** An account's connections may each want different water, so they
board separately; the passenger is what their orders and money land on, never
what selects their river. The Boarding entry written earlier the same day said
"a passenger's resolved config" and was wrong under this ruling.

**`FeedLagged` is advisory. Owner, 2026-08-21.**

The venue reports the gap and keeps serving; the consumer decides what to do
about it. The frame's own payload argues for this - it carries a skipped count
and a simulated instant, which is data you publish only if the reader is
expected to continue - and the venue should not decide a consumer's risk
posture for it. A gap a consumer can measure is worth more than a dead socket.

The consequence reaches further than the one contradiction that surfaced it.
`reference/architecture.md` has the frame followed by a close with WS 1011, and
`mogwai-protocol`'s own doc comment says the venue closes 1011 after delivering
it. Both are now wrong and owe correction. If the serving path really does close
after sending it, that is a code gap against this ruling, not a licence to
restore the fatal reading.

What does not change: a consumer receiving one is working from a book it cannot
fully trust until it reconciles, and nautilus has no typed channel to carry that
- so the adapter's error-level logging stays the mitigation, and the upstream
half recorded in `notes/todo.md` stays owed.

**A passenger is one per connection. Owner, 2026-08-22. Supersedes the
2026-08-21 passenger ruling above.**

A passenger is one connected trader: a single WebSocket under an account,
boarded onto one boat, holding its own lane, byte budgets, declared duration
and view of the water. It dies with its socket. The superseded ruling defended
per-account on the ground that the account-lifetime object would otherwise be
unnamed; it was never unnamed - it is the Account, whose entry already held the
ledger, the risk state and the freeze stamp. The Seat and Connection entries
are deleted with the re-cut: a seat's rules restate over Account (one cadence
per river ridden), Boatyard (wind-down when the last passenger leaves) and
Freeze (off-river retirement on a frozen account's return), and `connection`
recedes to ordinary English for the transport. Round 5's rider vocabulary
inverts: a count of riders on a boat is a count of passengers. The code sweep
for this re-cut is its own round and replaces the queued seat follow-ups; the
unruled tenure sense of `session` resolves inside it, taking passenger or
connection wording per the new entries.

**The boot river is deleted. Owner, 2026-08-22.**

No river is warmed at boot, in either mode; every river is synthesized when a
boarding or a history poll first names it, and its first requester pays the
warmup inside its own request. This extends the 2026-08-20 riverless-server
ruling to transient mode. Boot's remaining job is validation: a config that
cannot serve what its run exists for refuses to start. The glossary's boot
entry is gone; the riverless boot itself is engineering, filed as roadmap, not
a rename round. Open, recorded in `notes/todo.md`: what a request naming no
symbol binds once no boot river exists to fall to - presumptively the default
preset resolved on demand.

**Entries are admitted by the owner alone. Owner, 2026-08-22.**

The glossary's preamble now carries the admission test: a word earns an entry
when using it plainly would leave two people believing they agreed. Being
load-bearing is not the test, and neither is being undefined. The Admission,
History slot, Attach, Hold, Posted margin, Variation margin, ReadyRecord and
RunComplete entries were deleted under it; the renames those rounds landed
stand, only the entries are gone. No round adds an entry - a missing word
escalates to the owner. Entry prose states the meaning, never the change that
produced it, never an argument for a ruling, and never a rule that belongs to
another entry; a sentence legible only as a reply to this arc's conversation
does not belong in the document.

### Rounds landed

**Round 1, the `server` family, 2026-08-21.** `ServerMessage` to `VenueMessage`,
`ServerClock` to `VenueClock`, the wire field `server_now_ns` to `venue_now_ns`,
the config key `server_heartbeat_ms` to `venue_heartbeat_ms`, the operator-typed
`HavocSpec.server` to `HavocSpec.venue`, plus the prose, doc comments, log lines
and refusal text that carried the word. What the close pass found in the half no
cold reviewer reads:

- Two sites where the sweep put `venue` on a sentence whose subject was the
  crate rather than the running process - `mogwai-lab`'s manifest comment on
  its dependency, and the crate-graph paragraph in `reference/architecture.md`.
  Both were corrected to name the crate, which at that point was still
  `mogwai-server`. That is the round's characteristic failure and it recurred
  in round 2 with the polarity reversed.
- `.gitignore` carried `scripts/server.log`, which the sweep renamed to
  `scripts/venue.log`. Nothing has ever written either path - `smoke.py` drains
  the child's stderr into memory - so the rename turned a dead entry into a
  false claim. The entry is deleted. Three neighbours in that file are dead the
  same way and belong to no term family: `mogwai.log` and `mogwai.pid` name
  `mogwai serve --log-file` and `--pid-file`, which do not exist, and
  `scripts/probe-warmup.log` names a script that is gone.
- The retired config key is refused rather than ignored, because `Config`
  carries `deny_unknown_fields` - now pinned by a test beside the one for the
  removed `sim_epoch_ns`, since the doc comment claims the guarantee and prose
  has no compiler. The rename pass had already added `deny_unknown_fields` to
  `HavocSpec` with the matching test, which is what makes the JSON break loud
  too.

`VenueMessage` is `#[serde(tag = "type")]` over variant names, so the type
rename moves no byte on the wire; the only consumer-visible breaks are
`venue_now_ns` and `HavocSpec.venue`. No tape byte moves, so no
`TAPE_PROTOCOL_VERSION` bump is owed. `client` did not move: the workspace's
occurrence count is identical before and after.

**Round 2, the crate, 2026-08-21.** `mogwai-server` becomes `mogwai-venue`:
the package name, the directory under `crates/`, the workspace dependency
entry, the three dependents' manifests, and every prose, doc-comment,
manifest-comment, config-comment and tooling reference that spelled the old
crate or its `mogwai_server` module path. This closes the last surviving use
of the retired word, since round 1 deliberately protected prose whose subject
was the crate rather than the process.

The word `server` now appears in the workspace in exactly three places, all
of them correct: the glossary's Server mode entry, the two regression tests
pinning the retired `server_heartbeat_ms` config key and `HavocSpec.server`
JSON field as refused rather than ignored, and the substring inside
`observer`. There is no third reading to sweep later.

What the round did not touch, checked rather than assumed: the binary target
name stays `mogwai` in `mogwai-cli`, which the shipped launcher and every
document exec by that name, so nothing about `brokkr run mogwai` or
`target/release/mogwai` moves. No `client` site moved - the arc's own ruling
gives that family its own round, and the only `client` lines in the diff are
unchanged context inside ledger rows whose crate path was rewritten. No tape
byte moves and no wire byte moves, so no `TAPE_PROTOCOL_VERSION` bump is
owed. The gated workspace check reports 1339 workspace tests and 468
instrumented, unchanged from before the round, which is the expected outcome
for a pure rename - any movement in that count would itself have been a
finding.

Corrected in the close pass, the half no cold reviewer reads: the round-1
paragraph immediately above had been swept too, which turned a historical
record of round 1's characteristic failure into a self-contradiction, since
the crate it says was wrongly called `venue` is now called exactly that. It
is restored to describe what was true at the time. `notes/todo.md` carried
the retired `server_heartbeat_ms` spelling in three inventory entries and
named `man.rs` under the venue crate when it has always lived in
`mogwai-cli`; both are now correct.

A disclosed decision rather than an oversight.
`analysis/mnq-arrival-control.json` and
`analysis/mnq-arrival-screen.json` are committed measurement provenance bound
to specific commits, and the sweep rewrote the preset paths recorded inside
them, so the paths they now claim did not exist at the commits they name. The
cold review found this and it is correct. The owner ruled on 2026-08-21 that
it is out of scope and stays as it is: the artifacts are not reverted, not
regenerated and not annotated. A later round must not re-raise it as a new
finding.

**Round 3, the `client` family, 2026-08-21.** The ruled classification applied:
the driving program becomes `consumer`, the inbound wire type becomes `Command`,
`HavocSpec.client` becomes `inbound`, `MAX_CLIENT_ID_LEN` becomes
`MAX_ECHOED_ID_LEN`, the socket-leg log label resolves to `socket`, and the
nautilus adapter objects and the `client_order_id` namespace do not move at all.
`MAX_CLIENT_MESSAGE_BYTES` fitted the inbound sense and was the last `client`
identifier on the protocol crate's public surface, so it became
`MAX_INBOUND_MESSAGE_BYTES`; the remaining `client` identifiers this project
owned were test names and two locals, and they took the sense each was doing.
The two consumer-visible breaks are `HavocSpec.inbound` and that constant's
name. `deny_unknown_fields` makes the retired `client` key a hard load failure
rather than a silent ignore, pinned beside the round-1 `server` refusal in
`havoc_spec_defaults_from_empty_object`, and `HavocSpec`'s own doc now says so -
round 1 left that guarantee stated only in a commit message. No tape byte and no
`VenueMessage` byte moves, so no `TAPE_PROTOCOL_VERSION` bump is owed. The gated
check reports 1339 workspace and 468 instrumented tests, unchanged.

`ClientMessage` became `Command` rather than `ConsumerMessage`, which is the
ruling's own consequence and is deliberate asymmetry against `VenueMessage`:
the venue is singular and names its own frames, while the inbound side has no
singular party to be named after. It reads as an unfinished rename unless it is
stated, so `mogwai-protocol`'s crate doc now states it and `Command`'s own doc
points at that paragraph.

What the fix-and-commit pass found in the half no cold reviewer reads, and the
pattern is round 1's characteristic failure again with a third polarity - the
sweep moving a word off a job that was never the ruled one:

- Source paths rewritten into directories that do not exist. The adapter's
  `src/client/` is the inherited sense and is unchanged on disk, so
  `reference/performance.md`, `mogwai-venue`'s `http.rs` and
  `mogwai-protocol`'s `lib.rs` all named files nobody can open. Restored.
- The inherited wire field swept in PROSE while its identifier stood still:
  about twenty sites said "consumer order id" over `client_order_id` and
  `validate_client_order_id`. The spelling is inherited from nautilus and FIX
  and the prose form must not drift from it, so all of them are back, test
  fixture ids included.
- Half-swept emphasis, six sites: an upper-case CLIENT replaced by a lower-case
  `consumer` inside a sentence whose whole point was the emphasis. Cheap to
  find only once you look for the case mismatch.
- Sentences the new word's own definition forbids, which is the check the
  glossary edits already warned about. The eviction close reason said a ledger
  is never read "from two consumers at once", and the venue does not discriminate
  consumers - it discriminates the presented session, and two sockets of one
  consumer with different sessions do evict each other. It now says sessions, in
  both the venue's constructor and the frame-size test that hand-builds the same
  sentence. Same correction on `BoundLane::session`'s doc, on the eviction test's
  prose and assertion messages, and on the account-snapshot fanout paragraphs,
  where the count is per socket rather than per consuming program.
- `consumer` put on the INHERITED objects in durable prose - `README.md`,
  `AGENTS.md`, `docs/cli.md`, `docs/oms-types.md`, `reference/architecture.md`
  all called the nautilus client pair "consumers" - and on hyper's own
  documented "client disconnect", and on HTTP's own 4xx status class in
  `analysis/databento_download.py`, which is itself an HTTP client. Restored; a
  dropped socket is now described as a dropped connection, which is what the
  venue perceives.
- `mogwai gen --havoc`'s help text still named a `consumer` surface after the
  key became `inbound`, while the stderr note beside it already said `inbound`.

Raised by the cold review and correctly DECLINED, recorded so a later round does
not re-file it: the glossary specifies `/ws?callsign=` while the code still
accepts `?session=`, and the review offered reverting the glossary as one
option. The glossary is the end state, so that is a gap the code owes and never
a case against the entry - the arc has ruled this three times. The callsign
round is next and it is where `session` moves. Nothing about `session` moved in
this round; where a `client` site needed a word for the party on one socket it
took the code's existing `session`, which that round will carry.

**Round 4, the `callsign` family, 2026-08-21.** The socket identity takes
`callsign` and the wire parameter follows: `/ws?callsign=`, `SocketQuery::callsign`,
`BoundLane::callsign`, `validate_callsign`, `MAX_CALLSIGN_LEN`, the adapter's
`process_callsign` / `default_callsign` and both configs' `callsign` field with
its `with_callsign` setter, plus the doc comments, refusal text and test names
that carried the word. The glossary entry moved rather than the operator
surface, on the ruling's own ground: `session` is the market trading day at
roughly sixty visible operator sites against the socket sense's six, and the
trading sense is domain-standard futures vocabulary this project does not own.
`docs/config.md` moved its identity sites and kept its five trading-day ones,
`docs/presets.md` kept all seven of its, and nothing in `mogwai-lab`, the
session profiles, the instrument session table or the `session-profile`
subcommands moved - which is the shape a correct classification produces. No
tape byte moves, so no `TAPE_PROTOCOL_VERSION` bump is owed. The gated check
reports 1340 workspace and 468 instrumented tests, one more than round 3 and
that one is the refusal test below.

THE WIRE BREAK IS HONEST. `SocketQuery` carries `deny_unknown_fields`, so
`?session=` is a 400 naming the key rather than a socket silently seated with no
identity and the always-evict reading - which would disconnect the very peer leg
it was configured to coexist with. That was true but unpinned and undocumented:
the struct's doc now records the retired key where a reader meets it, and
`ws::tests::the_retired_session_query_key_is_refused` holds it, in the shape of
round 1's `a_config_naming_the_retired_heartbeat_key_is_refused`. Bite-checked
by deleting `deny_unknown_fields` as a text edit: the test fails naming the
silently-parsed query with `callsign: None`.

What the close pass found in the half no cold reviewer reads:

- The eviction close sentence was pinned on ONE SIDE ONLY, and it drifted the
  moment the wording changed. `run.rs` composed it and
  `admission.rs`'s frame-budget test hand-built its own copy, so the venue said
  "under a different callsign" while the test still measured "a ledger is never
  read from two sessions at once" - both halves green, and the test no longer
  measuring the reason that goes on the wire. `CloseSpec::evicted` now takes the
  account id and composes the whole sentence, so there is one copy and the test
  calls it. It also asserts the reason is EXACTLY `MAX_REASON_BYTES` at a
  maximal account id, because a shortened sentence would leave it passing
  without the trim ever running.
- The eviction sentence's byte length is durable prose asserting a live fact,
  and it was written in four places. 157 becomes 135 in `close.rs`,
  `admission.rs` twice and `reference/architecture.md`. Grep the number, not
  the file.
- Two adapter comments the sweep missed: `exec.rs` still said a venue seats
  ledgers "keyed by account plus session" - the same sentence
  `adapter_smoke.rs` had already moved - and `lifecycle.rs` described eviction
  as "another client claimed this account", which carried the retired `client`
  as well as the imprecision.
- `reference/architecture.md` opened its eviction section with "AN ACCOUNT IS
  ON AT MOST ONE RIVER, WITH ONE READER", which is false in both halves under
  coexistence and under the Seat entry. It now states the callsign rule and
  says what coexistence buys: one consumer holding a data leg and an exec leg,
  and through them as many rivers as those sockets bound.

THE TENURE SENSE DID NOT MOVE and is the round's open item: a socket's served
tenure is a third job whose word is unruled, so it was left and inventoried.
`mogwai-venue`: `SocketSession` and its doc, `Run::sessions_tx`,
`session_guard`, `sessions_drained`, `a_live_session_guard_holds_the_shutdown_open`,
`serve.rs`'s `sessions` binding and `serve_until_drained`'s doc, `admission.rs`'s
"a session's outbound machinery" prose, `run.rs`'s freeze and seat comments.
`mogwai-cli`: `completion.rs` throughout - "a live session", "never became a
session", "the venue's entire vocabulary for a session" - and `serving.rs`'s
teardown comments. `mogwai-adapter`: `exec.rs`'s `reset` comment on a prior
session's orders leaking into the next, `havoc.rs`'s
`dialing_blind_establishes_a_full_session_with_a_stranger`, `data.rs` and
`data_client_transport.rs`'s "the sessions piece 13 exists to support". Durable
prose: `docs/cli.md`'s drain paragraphs and `reference/architecture.md` at the
abandoned-upgrade, first-session and already-bound-session sentences. `conn` is
the ledger's recorded candidate; nothing is ruled.

Flagged, not acted on, because it belongs to another family: `mogwai-lab`'s CME
trade-date vocabulary is inherited and untouched, as intended.

**Round 5, the `seat` family, 2026-08-21.** A classification rather than a
substitution, and the only round so far whose ruling left the word in place. The
glossary sense keeps it: an account's riding of one boat, counted per
connection, vacated by its last rider - `Passenger::seated_on`, `try_sit`,
`unsit`, `is_seated_on`, the "already seated on {symbol} at speed" refusal and
the seat paragraphs of the glossary, `reference/architecture.md` and
`docs/config.md` are all untouched, because that one site is the model
implementation. The four other jobs moved:

- The boatyard's registry entry for a placed boat. `Slot::Seated(Seat)` becomes
  `Slot::Placed(PlacedBoat)` and its `passengers` field becomes `riders`, which
  is the ruling that a count of riders on a boat is a count of connections and
  never of passengers - the old field name asserted the opposite. `Berth` was
  the ledger's recorded candidate and was deliberately not taken: `place` is
  already the glossary's verb for putting a boat on a river, so `Placed` invents
  no noun where `Berth` would have.
- A passenger existing in the run's map. Locals and doc sites become `existing`
  and `minted`. The value at the site the pass renamed is born frozen and holds
  no seat, so nothing there may imply it is riding anything.
- A river carrying a boat: `boated`, against `boatless`, which the prose already
  had.
- `Run::seat`, which takes no seat at all and never did: `claim_account`, with
  `seat_discards_ledger` becoming `claim_discards_ledger`. A reader tracing
  "where is the seat taken" now lands on `try_sit` rather than here.

ONE OUTRIGHT DEFECT RODE WITH THE FAMILY, and it is a shadowed binding rather
than a naming problem. `sweeper.rs` bound `seated` twice inside one loop body -
once for the live boat key set, once about fifty lines later for the attached
passenger vector - and the second shadowed the first, so a membership test and
an index into a different collection read identically. They are now
`placed_boats` and `attached_passengers`. Nothing detected it because both
bindings type-checked and the shadow made the wrong read impossible to see.

No tape byte, no wire byte and no consumer-visible string a test reads moves:
the two refusal bodies the round reworded are asserted on by symbol and by
"sharing key", never by the swept phrase. The gated check reports 1340 workspace
and 468 instrumented tests, unchanged.

What the close pass found in the half no cold reviewer reads:

- Three sentences the substitution left FALSE OR UNGRAMMATICAL, all of them in
  the same shape - a word swapped without re-reading the sentence it landed in.
  `clear_venue_arms` came out promising that a clear "keeps a minted ledger
  identical to a minted one", which is vacuously true and states nothing; it now
  says a ledger minted after the clear is identical to one already open.
  `http.rs` twice produced "a existing", one of them inside a dash clause the
  swap had also broken open.
- The half-swept emphasis of round 3, in the other direction: the pass
  LOWERCASED emphasis whose contrast was the whole point of the sentence, and
  left the other half of the phrase capitalised. `docs/cli.md` read "a boated
  RIVER's tape", `/health`'s doc lost "A FAULTED TAPE ON ANY BOATED RIVER, not
  merely on the boot one", and `reference/architecture.md` lost the EVERY and
  the SKIPS that carried the two rules. Restored where the emphasis was the
  meaning.
- `ws.rs` mixed BOTH SENSES IN ONE SENTENCE and kept the retired one: "TAKEN ON
  THE EXISTING LEDGER, BEFORE THE SEAT ... a reset ledger holds no seat at all"
  used the claim sense and the glossary sense four words apart. The claim half
  is now spelled as a claim, and the "pre-seat check" beside it is the pre-claim
  check.
- THE SWEEP STOPPED AT THE VENUE CRATE, and the live siblings were in the other
  three. `mogwai-engine`'s `rebase_scans` doc, `mogwai-adapter`'s `config.rs`,
  `client/exec.rs`, `adapter_smoke.rs` and `havoc.rs`, and eight sites in
  `mogwai-cli`'s `serving.rs` all still carried a moved sense. Two of them were
  the same false sentence the round-4 close pass had corrected in
  `reference/architecture.md` and nowhere else: "a venue does seat several
  ledgers, KEYED BY ACCOUNT PLUS CALLSIGN". A ledger is keyed by account alone -
  the callsign keys the claim, not the book - so both copies now say the venue
  holds several ledgers, one per account id. Grep the sentence, not the file.

Raised and DECLINED, recorded so it is not re-filed: the glossary's Eviction
entry says "a socket claiming a seated account id", which reads at first like
the retired run-method sense surviving in the end-state document. It is not. An
account the venue holds was claimed by a connection, and every `/ws` connection
boards a boat, so the account genuinely does hold a seat - the adjective is the
glossary's own sense and the entry stands.

**Round 6, the `admission` family, 2026-08-21.** A classification, and the
second round whose ruling leaves the word on one of its senses. The three jobs
the word was doing in `mogwai-venue`, two of them observable by one consumer in
one run:

- The per-connection OUTBOUND byte and frame budget KEEPS the word. It is the
  only one of the three the protocol crate already publishes on the wire -
  `AdmissionRejected`, `AdmissionSubject`, `retryable` - and two operator config
  keys, `admission_lane_frames` and `exec_held_budget_bytes`, are stated over
  it. `admission.rs` is untouched by the round, which is the shape a correct
  classification produces.
- The INBOUND concurrency gate on `/trades` and `/quotes` becomes slots.
  `admit_history` is `acquire_history_slot`, `MAX_CONCURRENT_HISTORY_REQUESTS`
  is `MAX_CONCURRENT_HISTORY_SLOTS`, `HISTORY_ADMISSION_WAIT` is
  `HISTORY_SLOT_WAIT`, and `AppState`'s `history_requests` / `history_queue`
  are `history_slots` / `history_slot_waiters`. All crate-private: no route, no
  status code, no refusal body and no config key moves, so there is no operator
  or consumer break to document or pin.
- The ATTACH refcount on an account becomes attach, which the module already
  spoke for that concept in three places. `Run::admit` is `Run::attach`,
  `Admission` is `Attach`, `Passenger::admitted` / `admissions()` / `admit()`
  are `attachments` / `attachments()` / `attach()`, and `SocketSession::admission`
  is `attach`. The passenger method that HELD the name `attach` - it clears the
  freeze stamp and has nothing to do with the refcount - is `clear_freeze`, and
  it now carries a doc saying which of the two it is.

The point of the split is that `Run::admit` read as capacity and meant its
opposite: it does not decide whether a socket may in, it records that one is
already coming, and a reader who learned the wire sense first was actively
misled rather than merely confused. `Attach` also states the guard's job in its
own name, which is what the type is for - the count is reachable only by owning
the thing that lowers it.

Three glossary entries landed with it, because the word defined none of the
three: **Admission** under The wire, **History slot** beside it, and **Attach**
under The accounts next to Connection. No tape byte and no wire byte moves, so
no `TAPE_PROTOCOL_VERSION` bump is owed. The gated check reports 1340 workspace
and 468 instrumented tests, unchanged.

What the fix-and-commit pass found in the half no cold reviewer reads, which is
the three recurring failures again, all three of them:

- THE SWEEP STOPPED AT THE VENUE CRATE. `mogwai-cli`'s `serving.rs` carried the
  moved sense four times in one doc comment on
  `a_departing_socket_freezes_its_account_into_collection` - "the ADMISSION
  being given up rather than the lane", "still holding its admission", "the
  admission count was ADDED for", "drop an `Admission` directly" - which is the
  socket-level statement of exactly the rule the round renamed, and it named a
  type that no longer exists. `notes/todo.md` carried four more.
- EMPHASIS WAS HALF-SWEPT IN BOTH DIRECTIONS, eleven sites. The pass lowercased
  capitals it had no mandate to move while their contrast partners stayed
  capitalised: `freeze_if_unattended`'s "TWO CONDITIONS, AND BOTH ARE
  LOAD-BEARING" kept its lead and lost the AND that joined the two conditions;
  `Run::attach`'s "THE COUNT IS ONLY REACHABLE THROUGH THE GUARD" kept its
  shout while the sentence above it lost GUARD; `reference/performance.md` kept
  "properties of what is RESIDENT" and lost the AMENDED and the WAITS in the
  same paragraph; `reference/architecture.md` lost "ITS LAST CONNECTION" IS
  COUNTED FROM two lines under a paragraph that still shouts THE CONSEQUENCE TO
  STATE IN ANY CLAIM. Restored where the emphasis carried the meaning.
- SENTENCES THAT CAME OUT FALSE OR UNGRAMMATICAL around a correct substitution.
  `collect_expired_accounts` produced "refuses to freeze an account a socket
  attach is still held"; `attachments()`'s doc said "See `attachments`" and
  pointed at itself; `HistoryPage`'s docs kept "readmit", the retired sense's
  verb, twice.

And three defects the round exposed rather than caused, all in the sense that
kept the word or in the gate it renamed, fixed here because the split has failed
at the site it was protecting if that sense is incoherent:

- `MAX_CONCURRENT_HISTORY_SLOTS`'s doc still said a fifth request is REFUSED
  rather than queued. The 2026-08-18 amendment made it WAIT, and
  `HISTORY_SLOT_WAIT` four lines below said so - two docs on one mechanism
  disagreeing, with both halves green.
- `AppState::history_slot_waiters` and `MAX_QUEUED_HISTORY_REQUESTS` both said
  the semaphore counts callers "in the building at all - synthesizing or
  waiting". `acquire_history_slot` drops the queue permit the instant a slot is
  won, so it has never counted synthesis. The rename made the FIELD NAME assert
  the true thing while its own doc asserted the false one, which is how the
  disagreement became visible.
- `ADMISSION_PROMISE_TICKETS`'s doc was two clauses spliced together - "would
  leave zero capacity for any actual refusal would leave a connection whose
  priority lane is completely empty unable to state why it is closing". The
  wire-subscribe removal cut the middle of a sentence out and left both ends.
  Reconstructed from `d7b5f98`, which is the commit that wrote it whole.

RAISED AND NOT ACTED ON, recorded so a later round does not re-file them:

- **`admission` has a FOURTH sense this round did not rule on**, and it is the
  largest by site count: `mogwai-engine`'s acceptance of an order or a linked
  group onto the book - "atomic admission", `docs/order-lists.md`,
  `reference/architecture.md`, roughly sixty sites. It is the industry reading
  of the word and it is not a capacity statement, so it is plausibly inherited
  rather than a collision. The new glossary entry was drafted claiming the word
  "belongs only to that outbound backpressure surface", which that sense makes
  false; the entry now names the engine sense explicitly instead of asserting an
  exclusivity nobody ruled.
- **`admission.rs` carries the false eviction sentence round 4 corrected** -
  "an account is on at most one river at a time and is never shared", on
  `CLOSE_EVICTED`. That is the callsign family and the correction round 4 made
  in `reference/architecture.md` and round 5 made in three more files; this is
  the fourth copy. Grep the sentence, not the file.
- **`notes/todo.md` still says `client`** at the abandoned-upgrade item. Round 3's
  family.
- **`mogwai-cli`'s `completion.rs` says config knobs are "bounded at admission"**,
  a fifth generic-English use meaning validated at load. Unruled.

**THE BACKPRESSURE SPREAD, reported and deliberately not redesigned.** The pass
was asked to report the several spellings of "the venue is full" rather than to
unify them, and nothing about them moved in this round - no status code, no
refusal body, no `retryable`. There are three carriers and only one is
machine-readable:

| carrier | what it says | retry hint |
|---|---|---|
| `AdmissionRejected` on the priority lane | outbound budget or the global command semaphore is full, with an `AdmissionSubject` naming what was refused | `retryable: bool`, a typed field the adapter keys on |
| WS close 1013 `CLOSE_ADMISSION_OVERLOAD` | the connection's admission path is saturated and the venue is ending it | the code's own "Try Again Later" and nothing else; the connection is gone |
| HTTP `503` from `acquire_history_slot` | "history request capacity exhausted", for both the fail-fast waiter refusal and the expired slot wait | none - a plain-text body, no `Retry-After`, and the two very different conditions share one string |

The item for a later round: the HTTP half is the weak one. A consumer cannot
tell "the queue was already full" from "you waited thirty seconds and lost",
and nautilus's historical response types carry no error channel at all, so the
503 reaches a host as an empty window - the same defect the 2026-08-18 wait was
added to avoid. Whether the fix is two distinct bodies, a `Retry-After`, or
something the adapter can type is a design question this arc has no ruling on.

**Round 7, the `reservation` family, 2026-08-21.** A split rather than a
substitution. The word named two unrelated money-adjacent quantities that sat
two paragraphs apart in `mogwai-engine` and read identically: a byte
reservation against a connection's outbound budget, and the portion of a
balance tied up by a resting order. THE BYTE SENSE KEEPS THE WORD - the
protocol crate's `sizing` module, `mogwai-venue`'s `admission::Reservation`,
`try_reserve` and the sweeper's delivery reservations are untouched, which is
the shape a correct classification produces. The funds sense wore FIVE nouns in
one crate - reservation, hold, lock, locked, and an enum whose `None` variant
meant the order ties up nothing - and that was the defect rather than the
untidiness: a reader could not tell which of the two quantities a sentence was
about, and `held_for` next to `order_reservation` next to `order_locked` read
as three mechanisms where there is one. It collapses onto `hold` as the noun
and `held` as the verb: `Reservation` is `Hold`, `order_reservation` is
`order_hold`, `order_reservation_entry` is `order_hold_entry`,
`refresh_open_reservation` is `refresh_open_hold`, `add`/`remove` follow,
`order_locked` and `order_locked_clipped` are `order_holds` and
`order_holds_clipped`, `rebuild_order_locked_excluding` and
`reconcile_order_locked` follow those, `locked_balances` is `held_balances`,
and `held_for` is `hold_for`. The byte-sense test names in `mogwai-engine` that
said `reservation` say `byte_budget`, so the two senses no longer share a test
vocabulary either.

THE WIRE SPELLING DELIBERATELY DID NOT MOVE. `Balance.locked` is the
consumer-visible field, the adapter reads it as `balance.locked` into
nautilus's `AccountBalance`, and renaming it would be a designed break this
round has no ruling for. The engine now assigns `locked: held` at the one
construction site, which is the whole of the seam. No tape byte and no wire
byte moves, so no `TAPE_PROTOCOL_VERSION` bump is owed. The gated check reports
1340 workspace and 468 instrumented tests, unchanged.

ONE OBSERVABLE STRING MOVED, and it was checked rather than assumed. The
order-modification refusal for a sell of an instrument declaring no base
currency now says "places a hold on its base asset" instead of "reserves its
base asset". Nothing pins it: the phrase appears exactly once in the workspace,
no test, constant or consumer-side matcher reads it, and it is not one of the
refusal strings that ARE contracts - the post-only refusal lives as a shared
constant the engine's admission table parses to a fixed substring, and the
retryable prefix is a public string a consumer is invited to prefix-test.
Neither moved. The one two-sided literal the round did touch moved on both
sides in the same change: the debug reconciliation panic message
"resting-order hold cache drifted from the book" and the `should_panic`
expectation that reads it.

What the close pass found in the half no cold reviewer reads, which is the
recurring failures again:

- THE SWEEP STOPPED AT THE ENGINE. Live funds-sense siblings survived in
  `mogwai-protocol`'s `messages.rs` (the atomic-group doc's "a reduce-only
  member reserves nothing ... a member without that flag reserves like any
  other order"), in `docs/order-lists.md` twice, in `docs/config.md` ("posts
  collateral rather than reserving notional at every funds site") and in one
  engine test comment. Grep the sentence across the workspace, not the
  identifier inside the crate.
- TWO SENTENCES CAME OUT AWKWARD AROUND A CORRECT SUBSTITUTION - a resting
  remainder that "is held against it", and the same passive in the group doc.
  Both now take a hold rather than being held against.
- THE ROUND'S OWN NEW GLOSSARY ENTRY WAS THE FALSE ONE. The Hold entry as
  drafted said `locked` carries order holds and unsettled equity proceeds and
  does not distinguish "the two quantities". `held_balances` folds THREE: the
  order holds, the maintenance collateral of open marked positions, and the
  unsettled credits. The entry now names all three and distinguishes none of
  them, which is the true statement, and it points at Admission for the byte
  sense so the split is readable from the glossary.

RAISED AND NOT ACTED ON:

- **The `locked` split stays unruled and was not pre-empted.** The field
  carries economically different quantities with opposite remedies. But
  `Account::unsettled`'s doc in `mogwai-engine` already asserts the conflation
  is fine - "it appears on the wire as `locked` - which is exactly what it is,
  and needs no new balance field for a consumer to understand". That prose
  predates this round and nothing in it moved; it is recorded here because it
  argues one side of a question awaiting an owner ruling.
- **`held` is already taken by the byte sense at one operator-visible site.**
  `mogwai-venue`'s "held lane" and the config key `exec_held_budget_bytes` use
  the word for the outbound frame budget, which is the sense that KEPT
  `reservation`. That is a fresh collision the ruling created rather than
  cured, at a consumer-visible config key, so it needs its own ruling rather
  than a sweep.
- **`reserves its id`** in `mogwai-engine`'s `orders.rs`, and the venue-minted
  prefix paragraphs, are a third generic sense - an id namespace, neither bytes
  nor funds. Unruled and untouched, except where the rename pass replaced one
  awkward "the reservation is not cosmetic" with "the restriction is not
  cosmetic", which is the same claim in plainer words.

**Round 8, the `warmup` family, 2026-08-22.** A split, and the third round whose
ruling leaves the word on one of its senses. THE SERVABLE-HISTORY SENSE KEEPS
THE WORD - `warmup_ns`, the `/clock` envelope, the boot materialization, the
tick-composition ceilings and every venue-side doc are untouched, which is the
shape a correct classification produces. The estimator burn-in prefix takes
`burn_in`: `SUMMARY_BURN_IN`, `GeneratedBinding::burn_in`, `HourAcc::burn_in_excluded`,
the `walk_cache_key` / `run_summary_walk` / `WalkCache` parameters, the fit
driver's `summary_for`, the screen's and control's projection locals and
refusals, and the operator flag `mogwai gen --warmup`, which becomes
`--burn-in`. The adapter's third sense - a consumer's history request window,
which is neither servable history nor burn-in - becomes "history window" or
"history request" across `data.rs`, `shared.rs` and `clock.rs`. No tape byte and
no wire byte moves, so no `TAPE_PROTOCOL_VERSION` bump is owed. The gated check
reports 1341 workspace and 469 instrumented tests, one more than round 7, and
that one is the frozen-key test below.

THE SERIALIZED SPELLINGS DID NOT MOVE, AND THAT IS THE ROUND'S WHOLE RESOLUTION.
The rename pass respelled the burn-in sense inside four frozen formats at once,
and each break was invisible to `brokkr check`:

- the measurement artifact's `diagnostics.warmup_exclusions`, `block4[hour].warmup_excluded`
  and `binding.generated.warmup`. The release parity gate assembles
  `analysis/mnq-measure-12a.json` from the committed caches and compares it
  typed-canonically, so a respelled key is a hard red - and the summation in
  `aggregate/artifact.rs` was worse, because a missing `warmup_excluded` read as
  zero and the artifact came out silently wrong rather than refused.
- the walk cache key. `walk_cache_key` hashes a JSON object whose `warmup`
  member is a field of the Python-era serialization; respelling it missed all
  10,192 entries in the read-only cache directory and brought the multi-hour
  recompute back, with nothing but a slow run to show for it.
- the sub-contract tree. `subcontract_dumps` is a transcription of
  `analysis/mnq_fit.py`'s own constant names, and its bytes are the
  `subcontract_hash` recorded in every committed measurement and preflight
  artifact, so respelling the map key would have unbound them all.
- the arrival screen's provenance command string, which keys stored screen
  state.

TWO RESOLUTIONS WERE AVAILABLE and the conservative one was taken everywhere:
keep the old spelling in the serialized format and rename only the Rust
identifiers. The alternative - a frozen reader beside a live writer, two key
functions per format - buys nothing here, because none of these formats has a
second generation to read: there is one artifact family, one cache, one hash,
and the port is the only writer. Two key functions would have written native
entries under a key no reader wants, and dual-reading the artifact would have
left the parity gate comparing an artifact spelled one way against a committed
one spelled the other. So there is one `walk_cache_key`, one artifact spelling,
one hashed tree, each carrying a comment saying the field is inherited and
frozen while the identifiers around it are not. The cold review's proposed
two-key resolution and its re-blessed hash are both superseded by this: no hash
moved, so `hash_matches_the_python_reference` is restored intact rather than
weakened to an `assert_ne!`, which had accepted every accidental change to every
sub-contract constant. `the_hashed_tree_keeps_the_inherited_summary_warmup_key`
is added beside it, so the next pass that respells the map key fails on the
reason rather than on an opaque digest.

Also corrected in the pass, which is the recurring failures again:

- EMPHASIS HALF-SWEPT, eleven sites, all of them outside the rename's mandate.
  `data.rs` lowercased ANSWER, ALWAYS, RESOLVES, SHORT and the BAR-count /
  TRADE-page contrast, and turned a sentence-initial LOUD into a lower-case
  "loud" that no longer parses as a sentence; `shared.rs` lost BEFORE;
  `arrival_control_exposure.rs` lost five capitalised lead-ins whose whole job
  was to mark the premise; `docs/cli.md` lost PRODUCED, `docs/config.md` lost
  EMPTY and QUIET WINDOW, `reference/performance.md` lost RESIDENT and EMPTY.
  Restored.
- ARCH LOWERCASED TO "arch" in `measure12a/generated.rs`. It is an acronym, not
  emphasis, and the sweep read it as the latter.
- LIVE SIBLINGS THE SWEEP MISSED, in the crates it did not open:
  `mogwai-cli`'s `stage_m_tier2.rs` refused with "control warmup underflow",
  `mogwai-lab`'s `summary.rs` documented "a warm-up walk" and "a warm-up quote"
  for the discarded prefix, `measure12a/tests.rs` said "all warmup, no
  residuals", and `arrival_control_exposure.rs`'s module doc named the warmup
  twice. All of them the burn-in sense.

RAISED AND DECLINED: nothing in `docs/`, `reference/` or `notes/todo.md` spells
`gen --warmup` as an instruction - the only surviving occurrences are the
ledger's own B6 row and `notes/glossary-scope-7.md`'s inventory row, both of
which record the pre-rename state and are historical by construction.
`analysis/mnq-arrival-control.json` and `analysis/mnq-arrival-screen.json` stay
untouched under the 2026-08-21 ruling; the exposure keys the CLI writes were
kept at `warmup` partly so those records and any new one still agree.

**Round 9, the passenger re-cut, 2026-08-22.** The 2026-08-22 ruling applied to
the code: a passenger is one connected trader, one socket, one boat, dying with
its socket, and the account-lifetime object is the Account. The swap is the
round's whole shape and its whole risk - `Passenger` now names what
`SocketSession` was - so `run.rs`'s account-lifetime `Passenger` becomes
`Account` and `ws.rs`'s socket-owned `SocketSession` becomes `Passenger`. With
them: `Run::passenger` / `peek_passenger` / `passengers` / `passenger_holding`
become `account` / `peek_account` / `accounts` / `account_holding`, the map
field `passengers` becomes `accounts`, `Attach::passenger` becomes `account`,
and the tenure machinery takes the word the callsign round left unruled -
`sessions_tx`, `session_guard` and `sessions_drained` become `passengers_tx`,
`passenger_guard` and `passengers_drained`. The boatyard's rider vocabulary
inverts as the ruling says it must: `PlacedBoat::riders` becomes `passengers`,
because a count of riders on a boat is now a count of passengers.

THE ACCOUNT NAMING WAS CHECKED, NOT ASSUMED. `mogwai_engine::Account` is not in
scope in `mogwai-venue`'s `run.rs` - the engine type reached from there is
`Engine` - and the protocol snapshot type stays `mogwai_protocol::AccountState`,
which the venue names in full at its two use sites. So `Account` is free in
that module and collides with nothing a reader of `run.rs` meets.

The value bindings of the renamed type are spelled `account_state` where the
enclosing scope already binds `account` to an id or an `Option<&str>`, which is
most of `http.rs`, `ws.rs` and `run.rs`; that is a binding name and the prose
never uses it as an actor. THE COLD REVIEW FOUND EXACTLY THAT DEFECT and it was
the round's largest: the sweep had introduced `account_state` as a THIRD NOUN in
about twenty comment and doc sites, several of them stating lifecycle - "a
websocket account_state on one boat", "a account_state borrows this state for one
socket tenure and dies with that socket", "the last account_state has left", "a
account_state may board afterwards". Every one is now the account or the
passenger, whichever the sentence is true of.

Mechanism identifiers keep the retired word by design: `try_sit`, `unsit`,
`is_seated_on`, `seated_on` and the wire-visible "already seated on {symbol} at
speed" refusal are crate-local mechanism or a consumer-visible string, and no
round took a wire break here - the refusal body, its two tests, the close
reasons and every JSON field are byte-identical. The test for that choice is
whether the prose around them reads correctly without the deleted Seat entry,
and it did not: `reference/architecture.md` still said a run "takes no seat" and
"what it holds is a keepalive ticket rather than a seat", and `serving.rs`'s
eviction test still headed itself "AN EVICTED SOCKET RELEASES ITS SEAT". Both
now state the ride over Boatyard and Passenger. `docs/config.md` states the
one-cadence rule over the account and its passengers, `reference/architecture.md`
states wind-down as the last ticket of a cadence, and Freeze keeps its
off-river retirement wording, so no durable sentence presents Seat as
vocabulary any more.

No tape byte and no wire byte moves, so no `TAPE_PROTOCOL_VERSION` bump is
owed. The gated check reports 1341 workspace and 469 instrumented tests,
unchanged from round 8. The socket suites were run by name because the round
touched the serving path: `mogwai-adapter` all four binaries green in both
sweeps, `mogwai-cli socket` green in both.

What the close pass found in the half no cold reviewer reads, which is the four
recurring failures again:

- THE SWAP HAZARD BIT, in the crates the sweep did not re-read. Sentences that
  were true of the old per-account Passenger became false the moment the word
  changed hands: `sweeper.rs` bound `attached_passengers` to a vector of
  ACCOUNTS and documented "one acquisition PER PASSENGER" and "one pass PER
  PASSENGER" over it (now `attached_accounts`, per account); `run.rs` promised
  to make a symbol tradable "on ONE PASSENGER'S ledger" and described the sweep
  as "one engine pass PER PASSENGER"; `sweeper.rs`'s `policed_passenger` and
  `run.rs`'s `held_passenger` were bindings of the account type. A ledger is
  never a passenger's, and a sweep pass is never per passenger.
- SENTENCES LEFT FALSE OR UNGRAMMATICAL AROUND A CORRECT SUBSTITUTION.
  `exec.rs` came out with "so a prior a prior passenger's orders leak"; `run.rs`
  said an account's boat map counts "how many accounts ride each";
  `serving.rs`'s blackout test said transport havoc "rides the passenger", which
  the same ruling makes false - it is armed on the ACCOUNT and blurs each of its
  passengers alike, which is what `http.rs`'s own `DivergenceRequest` doc and
  `ws.rs`'s writer gate say. `docs/config.md` said the account may re-cadence
  "when the passenger on that cadence leaves", which is wrong for the two
  sockets sharing one cadence the count exists for; it is the last of them.
- A QUOTATION WAS SWEPT. `run.rs`'s `VenueArms` doc quotes `docs/havoc.md`
  verbatim on the late boarder, and the sweep rewrote the quote to "an account
  that boards" while the quoted document still says "a passenger that boards".
  A quoted sentence is a serialized field name in the sense round 8 named: the
  bytes belong to somebody else.
- EMPHASIS HALF-SWEPT, about twenty-five sites, in both directions and all of
  them outside the rename's mandate. `docs/cli.md` lost RUN, BEFORE, the whole
  of "THAT DRAIN COVERS WEBSOCKET SESSIONS", "A DECLARED COMPLETION", "A SIGNAL
  DOES NOT" and the NOT/NONZERO pair; `reference/architecture.md` lost the
  RIVER/PASSENGER contrast in its opening paragraph, "DELIVERY IS ATTRIBUTED,
  NOT BROADCAST" and "ITS LAST CONNECTION IS COUNTED FROM THE ATTACH";
  `completion.rs` lost PROVABLY, KEPT ALIVE and HOW THE DRAIN ENDED; `havoc.rs`
  lost FULL SESSION and SURVIVING; `run.rs` and `ws.rs` lost UNCONDITIONAL,
  TAKEN ON THE EXISTING LEDGER, COUNTED RATHER THAN A SET and the OPENING
  balance. Restored where the emphasis carried the meaning.

THE TENURE SENSE OF `session` IS CLOSED, which round 4 left open. Every site
that report inventoried has moved to passenger wording - `mogwai-venue`'s
`SocketSession` and the three tenure members, `mogwai-cli`'s `completion.rs`
and `serving.rs`, `mogwai-adapter`'s `exec.rs`, `havoc.rs`, `data.rs` and
`data_client_transport.rs`, and the drain paragraphs of `docs/cli.md` and
`reference/architecture.md`. `sweeper.rs`'s module doc, which no inventory
listed, said the sweep is owned by the run "rather than by an account or a
session"; it now says passenger. What survives the workspace-wide grep is the
trading day, the `SessionProfile` and `SessionCalendar` families, the
session-profile subcommands, and `ws.rs`'s `the_retired_session_query_key_is_refused`
with the two doc sentences that name the retired key - which is the shape a
correct classification produces.

AND ONE DEFECT THE ROUND EXPOSED RATHER THAN CAUSED, fixed here because the
round rewrote the prose it lives in. The doc block opening "A live-session
token" had been glued to the front of `fault_venue`'s own doc, so the rename
pass rewrote it in place and left it describing the wrong item - this is the
third `run.rs` instance of the doc-attached-to-the-wrong-item family the
cross-cutting section records, after `evict_account` and `session_guard`. It is
moved onto `passenger_guard`, which it has always been about.

**Round 10, the `divergence` split, 2026-08-22.** A split, and the fourth round
whose ruling leaves the word on one of its senses. THE ARMED-HAVOC SENSE KEEPS
THE WORD - everything reached from `mogwai_protocol::control::Divergence` is
untouched: the catalog, `MAX_DIVERGENCE_MS`, `mogwai-engine`'s `divergence.rs`
and its `apply_divergences` parameter, `POST /control/divergence`,
`validate_divergence`, `docs/havoc.md`, `README.md` and the glossary entry.
That is the shape a correct classification produces, and it is why the round is
almost entirely comments and doc prose. The float-disagreement sense - two
numbers, shapes or outputs disagreeing against a reference - becomes `mismatch`,
chosen over `drift` because drift implies gradual movement where these are
point comparisons. `first_divergence` became `first_mismatch` in
`mogwai-cli`'s `count_curve.rs`, and `mogwai-lab`'s `era_stability` gave up the
last identifiers in the retired sense: `divergent_share`, `divergent_cells` and
`divergent_exposure` are `mismatch_share`, `mismatch_cells` and
`mismatch_exposure`, which also stops the era-stability quantity from being
named differently than the separability quantity computed by the same rule two
functions above it. No tape byte and no wire byte moves, so no
`TAPE_PROTOCOL_VERSION` bump is owed. The gated check reports 1341 workspace and
469 instrumented tests, unchanged from round 9.

TWO SERIALIZED SPELLINGS DID NOT MOVE, and both were verified rather than
accepted as claimed. The exposure records' `"divergence": null` member is the
armed sense - it states that the measured window had no havoc armed - so it
keeps the word on its merits and not as an exemption, and the two committed
provenance artifacts `analysis/mnq-arrival-control.json` and
`analysis/mnq-arrival-screen.json` still carry it. The count-curve artifact's
`"first_divergence"` key stays as an inherited seam: the spelling is fixed by
the frozen preregistration the measurement answers, and the Rust identifier
beside it is not. The round-8 precedent of pinning such a key with a test is
NOT available here and the record says so rather than pretending otherwise -
the artifact is written to an untracked output directory, no committed artifact
and no hash carries the key, and a constant asserted equal to its own literal
would be a vacuous gate. The comment at both write sites is the whole of the
pin, and the round-8 comment claiming the key was "inherited from the committed
artifact format" was false in the word "committed" and now says what is true.

What the close pass found in the half no cold reviewer reads:

- EMPHASIS HALF-SWEPT, seventeen sites, all of them outside the rename's
  mandate and none of them on the moved word. `monthly.rs` lowercased
  IDENTICALLY SHAPED JSON - taking the acronym with it, which is round 8's ARCH
  failure exactly - and the ANY that carried the never-a-median-over-fewer-seeds
  rule; `kernel.rs` lost WITHOUT and turned ULP into "ulp"; `measure12a/tests.rs`
  lowercased the EXACT and POINTS whose contrast is the entire convention the
  test pins, while leaving its own capitalised lead-in standing;
  `parity_select_windows.rs` lost the NOT that marks the two accumulators that
  are deliberately uncompensated and the EXACTLY that makes the manifest a
  two-sided assertion; `measure.rs` lost LIVE and MANDATORY, `gen.rs` EXACT,
  `stream.rs` WELL-FORMED, `stream_refusals.rs` BEFORE, `docs/oms-types.md`
  WHOLE, and `reference/architecture.md` CONFIGURED, NEXT and ADMIT. Restored.
- ONE SENTENCE LEFT UNGRAMMATICAL AROUND A CORRECT SUBSTITUTION: the adapter
  test stub was "copy-pasted across the three files with subtle mismatch", where
  the retired word was a mass noun and the new one is not. Plural.
- A LIVE SIBLING IN THE CRATE THE SWEEP DID NOT OPEN, which is the recurring
  failure again: `session_profile.rs`'s era-stability trio above. It is the
  ruled sense by its own code - a `candidate` compared against a `reference`
  ratio outside a band - and it was the last of it in the workspace.

RAISED AND DELIBERATELY LEFT, so a later round does not re-file it: the VERB
`diverge` and the adjective `divergent` survive as ordinary English where no
quantity is named. `stream.rs` and `stream_refusals.rs` say the port "knowingly
diverges from" the Python reference, and `characterize`'s two tie-break
comments say a `HashMap` made the tie-break "not merely divergent from
`characterize.py` but nondeterministic". Only the NOUN naming a disagreement was
retired; forcing `mismatch` into those clauses produces worse prose and no
clearer word. The phase-5 conformance scan's allowlist owes them a line.
`notes/protocol-landings.md` records a past era-stability measurement as
"22.83 percent divergent"; it is a historical record of a landing and is not a
sweep target.

**Round 11, the `ledger` split, 2026-08-22.** A split, and the fifth round whose
ruling leaves the word on one of its senses. THE ACCOUNT SENSE KEEPS THE WORD -
the glossary's Ledger entry, `mogwai-engine`, the venue's ledger prose, the
adapter, `README.md`, `docs/config.md`, `docs/havoc.md`, `docs/oms-types.md`
and `reference/architecture.md` are untouched, which is the shape a correct
classification produces. The Databento delivery manifest gives the word up:
`mogwai_lab::ledger` is `mogwai_lab::delivery`, `LedgerFile` and
`LedgerJobEntry` are `JobsManifest` and `DeliveryEntry`, every `ledger_path`
parameter is `jobs_manifest`, every `ledger_key` parameter is `delivery_key`,
`subcontract::LEDGER_KEY` is `DELIVERY_KEY`, the `DEFAULT_LEDGER` / `LEDGER`
path constants across six `mogwai-cli` modules are `DEFAULT_JOBS_MANIFEST` /
`JOBS_MANIFEST`, and the refusal bodies of the verification path name the jobs
manifest and the delivered manifest rather than "the ledger" and "the
manifest", which were the two things a reader could not tell apart. The
operator flags follow: `--ledger` is `--jobs-manifest` and `--ledger-key` is
`--delivery-key`, on `preflight`, `measure`, `fit`, `minute-range-envelope` and
both `stage-m` subcommands. The fourth sense, `mogwai-venue`'s `LedgerTemplate`
- which the ledger row records as neither a ledger nor a template of one -
becomes `AccountOpeningTerms`, with `template_engine` becoming
`engine_from_account_opening_terms`. No tape byte and no wire byte moves, so no
`TAPE_PROTOCOL_VERSION` bump is owed. The gated check reports 1342 workspace and
470 instrumented tests, one more than round 10, and that one is the frozen-key
test below.

THE OPERATOR BREAK IS LOUD, and it was measured rather than reasoned about.
`target/debug/mogwai preflight --ledger analysis/databento-jobs.json` exits 2
with "error: unexpected argument '--ledger' found" and the usage line; clap
rejects an unknown long flag and no arg struct on the path carries an alias or
`allow_hyphen_values`, so there is no reading on which the retired spelling is
silently absorbed.

THE SERIALIZED SPELLINGS DID NOT MOVE, and each is anchored where a reader meets
it. The sub-contract tree keeps `"LEDGER_KEY"` under an inherited-format
comment, and `the_hashed_tree_keeps_the_inherited_ledger_key` now holds it
beside round 8's warmup sibling, in the same shape and for the same reason: a
pass respelling the map key otherwise fails only on an opaque digest.
Bite-checked by respelling the `tree()` insert as a text edit - the test fails on
its own named assertion, in all three sweeps. `analysis/databento-seal-ledger.json`,
its committed path references, `analysis/databento_seal_ledger.py` and the
seal-ledger vocabulary of the frozen purchase and preregistration documents are
untouched, which the frozen-document rule requires.

What the close pass found in the half no cold reviewer reads, which is the five
recurring failures again, four of them:

- THE SWEEP STOPPED AT THE CRATE IT OWNED. `mogwai-lab`'s own `Cargo.toml`
  named "the tree-state seam in `ledger`" and pointed at "the module comment in
  `ledger.rs`", a file that no longer exists, and its crate doc and manifest
  header both said "ledger-bound input verification"; `mogwai-cli`'s
  `count_curve.rs` still declared `const LEDGER` for the jobs manifest and
  passed it by that name; `.gitignore` called the lock's neighbour "the
  committed job ledger"; `brokkr.toml` and `reference/performance.md` both
  described `measure` as verifying "against the ledger", the same sentence in
  two files, which is the grep-the-sentence rule again.
- THE SWAP HAZARD, in the `template` half. The rename gave the venue a new noun
  and left three live sentences using the old one: `ws.rs` on "the venue
  template's balances", and `serving.rs` twice, once saying the preview "is
  built from the same template the mint uses" and once "the venue template a
  mint on the read path would have handed it". `http.rs`'s "stored on the
  template" is NOT one of them and was left: it quotes a comment that used to
  be there, in a sentence whose whole subject is that the claim was false.
- SENTENCES LEFT FALSE OR REPETITIVE AROUND A CORRECT SUBSTITUTION. `funded_in`
  came out saying a resetting claim "replaces the account's balances with the
  account opening terms", which replaces balances with a settings struct, and
  its `None` arm said the terms "are the honest answer" to a question about
  currencies; both now name the balances. The
  `engine_from_account_opening_terms` doc had the new phrase four times in one
  paragraph and was rewrapped to say it twice.
- EMPHASIS HALF-SWEPT, three sites. The pass lowercased `LedgerTemplate`'s
  OPENING, whose contrast with "the balance of one shared ledger" is the
  sentence's whole point; lowercased "THE ONE PLACE THE TEMPLATE IS APPLIED",
  the capitalised lead-in that marks the premise of the paragraph under it, in
  a file that still shouts CONSTRUCTION and READ six lines later; and turned
  `minute_range_envelope.rs`'s "names the tree and NOT the ledger" into a
  lower-case "not". Restored.

AND ONE DEFECT THE ROUND EXPOSED RATHER THAN CAUSED, which is round 10's
count-curve finding recurring on a different key. The rename pass anchored the
preflight artifact's `ledger_key` with a comment calling it the spelling of
"committed and on-disk preflight artifacts". No committed artifact carries it:
`analysis/mnq-measure-12a.json` does not, the twenty committed
`analysis/preflight/BTCUSDT-*.json` do not, and the field is
`skip_serializing_if = "Option::is_none"` on a path only the Databento months
take. The comment now says what is true - the artifacts on disk carry it,
nothing committed pins it, and the comment is the whole of the pin - and the
Stage M refusal artifact's `ledger_key` gets the same honest anchor, which it
had none of.

RAISED AND NOT ACTED ON, recorded so a later round does not re-file it:

- **THE GIT-CLEANLINESS ORACLE IS STILL IN `delivery.rs`**, and the ledger row
  named that as a third unrelated job under one file name. The rename pass
  declined the split as not cheap - `TreeOracle`, `TreeQuery`, `TreeReading`,
  `ScriptedTree`, `install_tree_oracle`, `require_clean_tree`,
  `fresh_tree_state` and `tree_readings_are_production` are a cfg-sensitive
  seam behind the lab's `test-seam` feature with consumers in five
  `mogwai-cli` modules plus `attestation.rs`'s runtime guard - and the coupling
  is confirmed as remaining. A module named for the delivery manifest still
  owns the reproducibility gate, which is a naming defect the round did not
  close.
- **`mogwai-cli`'s `stage_m_tier2.rs` has a sixth sense**: `fn ledger` and "an
  append-only candidate ledger", a record of evaluation entries that is neither
  an account's engine nor a delivery manifest. Nobody ruled on it and it was
  not swept.
- **The Stage M preregistration binds its inputs to the SEAL LEDGER while the
  code reads the jobs manifest.** `verify_input_entry` indexes
  `analysis/databento-jobs.json` by the key the frozen document calls a
  seal-ledger entry. Both docs now name the jobs-manifest key and mention the
  preregistration's word, rather than asserting a seal-ledger read the function
  does not perform, but whether the two artifacts should agree is a data
  question this arc has no ruling on.

**Round 12, the `tape` split, 2026-08-22.** A split, and the sixth round whose
ruling leaves the word on one of its senses. THE PACED-DELIVERY SENSE KEEPS THE
WORD - the glossary's Tape entry, `mogwai-venue`'s `tape.rs` broadcast ring, the
delivery-barrier comments in the adapter's `data.rs` and `exec.rs`, the backlog
and the `tape_lateness` bench target are untouched, which is the shape a correct
classification produces. The generated-sequence sense becomes river:
`ensure_on_tape` is `ensure_on_river`, the off-tape and on-tape history
diagnostics are off-river and on-river across `mogwai-adapter`,
`mogwai-protocol` and `mogwai-venue`, and `off_tape_window_still_answers_the_request`
follows. The offline dump becomes a composition: `mogwai segments tape` is
`mogwai segments compose`, with `TapeArgs` and `TapeType` becoming `ComposeArgs`
and `ComposeType`, and the composer prose in `mogwai-data`'s `segment.rs` and
`mogwai-cli`'s `segments.rs` says river where it named the sequence and
composition where it named the dump. `docs/cli.md`, `notes/segment-sampler.md`,
`notes/todo.md` and AGENTS.md's workspace overview moved with it. No tape byte
and no wire byte moves, so no `TAPE_PROTOCOL_VERSION` bump is owed. The gated
check reports 1342 workspace and 470 instrumented tests, unchanged from round
11.

THE OPERATOR BREAK IS LOUD, and it was measured rather than reasoned about.
`target/debug/mogwai segments tape --library /dev/null` exits 2 with
"error: unrecognized subcommand 'tape'" and the usage line; no alias, no
`allow_hyphen_values`, so there is no reading on which the retired spelling is
silently absorbed.

THE FOURTH SENSE WAS OUT OF SCOPE BY OWNER RULING and stayed out.
`TAPE_PROTOCOL_VERSION`, its value, its history, the bump rule, the version
narrative, `tape_version_prose.rs`, `TAPE_ORIGIN_NS` and every serialized
`tape_protocol_version` key are untouched, and the round's diff contains no
version prose at all.

What the close pass found in the half no cold reviewer reads, which is the
recurring failures again:

- THE COLD REVIEW'S OWN FINDING, and it was a two-sided contract left pinned on
  neither side. `http.rs`'s two history refusal bodies still told a consumer
  "the tape cannot serve" before its origin and past the clock, one line under a
  log message the same pass had moved to off-river, and the doc above them said
  the function refuses "a history start outside the tape that exists now".
  History reads a river directly, which is the glossary's own sentence. All
  three now say river. Nothing reads either body: the workspace's only assertion
  on them is `quote_history_refuses_below_a_nonzero_origin`, which matches
  "precedes data_origin_ns 10", so the two sides did not have to move together
  and the change is free.
- EMPHASIS HALF-SWEPT, fifteen sites, all of them outside the rename's mandate.
  `data.rs` lowercased the two ANSWER lead-ins that carry the whole reason the
  refusal answers rather than erroring; `common/mod.rs` lost KNOWN, whose
  contrast with "an unknown one" is the sentence; `data_client_transport.rs`
  lost RESOLVES and GUARD; `segments.rs` lost SAME and NOT and turned Slice 1
  into a lower-case slice; `segment.rs` lost ENDLESS and EXACTLY ZERO;
  `docs/cli.md` lost REAL, REFUSED and NO; AGENTS.md lost DEFAULT and BETTER.
  Restored.
- THE CASE-SENSITIVE GREP MISSED THE SHOUTED SPELLING. `data_client_transport.rs`
  headed the very test the round renamed with "An OFF-TAPE window must answer
  too" and documented its fixture as "THE TAPE IS STOCKED" over "the stub's
  default empty tape" - three live sites in the one file whose test name had
  already moved, invisible to a sweep grepping the lower-case word.
- HALF-SWEPT SENTENCES, where the new word and the old sat in one paragraph.
  `segment.rs` said "a session that is not in this river" two words after "the
  tape origin is not a seam", and repeated the retired phrase in a test name and
  an assertion message; `config.rs`'s default comment said "one day of warmup
  tape behind sim-now" on the line above its own off-river refusal note, and now
  says warmup history.
- A LOG LINE'S CONTINUATION LOST ITS INDENTATION in `data.rs`'s `request_bars`
  warning, four columns short of the block it belongs to. `rustfmt` does not
  reach inside the macro, so nothing would have moved it back.

RAISED AND DELIBERATELY LEFT, recorded so a later round does not re-file it:

- **THE `data_origin_ns` ORPHAN ROW IS NOT THIS ROUND'S.** "the tape boundary",
  "where the synthetic tape begins" and "the tape floor" in
  `mogwai-protocol`'s `clock.rs`, `mogwai-venue`'s `http.rs`, and the adapter's
  `clock.rs`, `exec.rs` and `data.rs` all name the same field the ledger files
  as its own orphan under section C, alongside `TAPE_ORIGIN_NS`, "tape anchor"
  and "tape origin". The constant is out of scope by owner ruling and the prose
  naming it is bound to it, so sweeping half that family here would pre-empt an
  unruled row.
- **The adapter stub's `trades_tape` and `quotes_tape` fixtures** are rows served
  over history rather than anything a boat publishes, so they are the
  generated-sequence sense by the entry's own reading. No ledger row names them,
  they are test-local, and moving them is a churn across three test files with
  no contract behind it.
- **`analysis/asia_jump_probe.py` still says `segments tape --type bars`.** It is
  the owner's untracked work in progress under the standing ruling, and it is
  never swept into a commit.
- **`analysis/plot_tape.py` keeps its filename**, which every doc and the
  composer's own header comment invoke, and it charts both a composed river and
  a generated one. The tool name is established and no round took a break on it.

**Phase 5, the conformance gate, 2026-08-22.**
`crates/mogwai-data/tests/glossary_vocabulary_prose.rs` walks the repository for
the spellings this arc retired and fails naming file, line, spelling and the
word that replaced it. It is built beside `tape_version_prose.rs` and inherits
that gate's shape: the document set is discovered by walking rather than listed,
symlinks are not followed, an unreadable directory fails the gate rather than
being skipped, and a walk that finds implausibly few files refuses instead of
certifying a partial scan.

It scans `.md`, `.rs`, `.toml`, `.py` and `.sh`, which is where every drift this
arc found actually landed. Extensionless files are NOT scanned, so `.gitignore`
- which carried a stale sentence in rounds 1 and 11 - is outside it; both of
those were bare words no gate of this shape could have caught anyway.

NEVER A BARE WORD. Every entry is an old identifier, an old operator spelling
or a phrase whose former meaning has no live use. The sense-scoped words this
arc deliberately left in place - `session`, `ledger`, `tape`, `warmup`,
`reservation`, `admission`, `seat`, `template` - are not banned, because each
still has a live sense and a bare-word ban would refuse the sense that won. For
the same reason the verb `diverge` and the adjective `divergent` are not
scanned: round 10 ruled they stay as ordinary English, and only the noun moved.

THE EXEMPTIONS ARE CUT IN TWO SHAPES, and that is the cold review's finding
rather than the first design. A whole-file exemption on a live production file
is the vacuous-gate shape - it reads as gated while any retired spelling
introduced anywhere in that file passes silently - and the first draft exempted
`config.rs`, `ws.rs` and `count_curve.rs` outright. An exemption is now a PAIR
of path and spelling, applied at match time: `config.rs` for
`server_heartbeat_ms`, `ws.rs` for the retired identity query key,
`count_curve.rs` for the frozen `first_divergence` artifact key, and for nothing
else. Whole-file exemptions survive only where nothing in the file is live
vocabulary: the three frozen documents the frozen-document rule names, the arc's
own records, the owner's untracked probe, this gate's own data table, and the
generated or vendored trees. `DATA-PURCHASE-REPORT.md` carries no retired
spelling and therefore gets no exemption; giving it one pre-emptively would have
been an exemption nobody could tell from a live one.

AND A SCOPED EXEMPTION MUST STILL MATCH SOMETHING. A carve-out that matches
nothing is indistinguishable from one that is load-bearing, and a mistyped path
exempts nothing while reading as though it exempts something, so the gate fails
on an exemption it never used.

THE WALK SKIPS EVERY DOT-DIRECTORY, which is not tidiness. `.gitignore` puts
`.agents`, `.brokkr`, `.claude`, `.codex` and `.plans` outside the repository
while leaving them squarely inside a filesystem walk, and agent scratch state is
exactly where a retired spelling lives legitimately.

BITE-CHECKED FOUR WAYS, all as text edits, all three sweeps red each time.
A `SocketSession` planted in `run.rs` fails on the drift assertion naming
`crates/mogwai-venue/src/run.rs:3`. A `ServerMessage` planted in `config.rs` -
a file exempt only for the heartbeat key, one line under the exempt site that
went on passing - fails the same way, which is the whole of the P2 repair
demonstrated. A scoped exemption pointed at a path that does not exist fails on
drift at the real file. An exemption naming a real file that carries no such
spelling fails on the unexercised-exemption assertion, which is the only one of
the four the drift assertion cannot reach.

THE GATE EARNED ITS PLACE ON DAY ONE: its first scan found six live drift sites,
all in `notes/todo.md`. Adjudicated as real drift rather than history, on the
ground that todo items are instructions to future workers and the file's own
rule removes completed items, so it must be current. Two `SocketSession` sites -
the session collision's sense-3 paragraph, which still called the tenure sense
open when round 9 closed it, and a broadarrow cost paragraph. Two
`order_reservation` sites in the equity-sell hold item, which took `order_hold`,
`order_holds` and `reconcile_order_holds` with them. Two `ServerMessage` sites,
one of them inside a message-to-broadarrow block: it states what the wire does
NOW rather than recording a past rename, so sending it spelled the old way would
hand a consumer a type that does not exist, and it took `VenueMessage`.

ONE SITE WAS JUDGED HISTORICAL and reworded rather than swept, which is the
option the brief allowed. The broadarrow cost paragraph said the socket state
"carries no account", and the account carrier has since landed - `SocketQuery`
takes an `account` and the socket's own state resolves one before the upgrade -
so renaming the type would have made the sentence MORE false, not less. It now
opens by saying the half has landed and states the measurement in the past
tense, without spelling the retired identifier. That is the swap hazard read
backwards: a sentence can be false about the new referent precisely because the
rename succeeded.

**Phase 6, the close pass, 2026-08-22.** One Fable subagent over the whole
thirteen-commit arc, `972e1af` through `6f276a3`, carte blanche. The phase-5
gate read clean: every retired spelling, exemption and walk rule matches its
recorded grounds, and every scoped exemption is exercised. What the pass found
is the recurring failures again:

- THE SWEEP STOPPED AT ONE FILE, round 12's family. AGENTS.md's overview took
  river while README.md kept the same sentences on tape - "that preset drives
  the tape", "the default tape is served", "draws the same tape", "dump a tape
  offline" - and `docs/presets.md` kept the whole family: "tape knobs", makes
  "a symbol's tape BETTER", "served the default tape", "run different tapes".
  `docs/config.md`'s seed paragraph said a served symbol's tape is a pure
  function of seed, config and label; `reference/architecture.md` opened with
  "a RIVER is a tape", kept "the generated futures tape", "the generated tape
  publishes", "serves several genuinely different tapes" and "makes a tape
  better" (the same sentence AGENTS.md had moved to river); `docs/cli.md` said
  a futures tape shows its closed weekend and warmup cost follows "the tape's
  cadence". In the venue crate, `fills.rs`'s clean-tape walk doc, `source.rs`'s
  "symbol labels select distinct tapes" assertion and `config.rs`'s "the
  earliest instant the tape can serve" were the generated-sequence sense by
  their own reading - the walk composes from `Rivers::history_source`. All
  moved to river or shape. Where the glossary itself is the sharper word, the
  fix took it: a presetless symbol is served the default SHAPE under its own
  label, never "the default river", because its label enters the seed and its
  river is its own - AGENTS.md's swept sentence said river and now says shape.
- SENTENCES LEFT FALSE around the account model. README.md still said one
  venue is one account and one ledger and "not a service you start once and
  point many strategies at", against the glossary's many-account Venue and the
  Server mode entry; it now states the account-id discriminator. `sweeper.rs`'s
  module doc justified run ownership with "one process is one ledger now",
  false since the per-account split; it now says the run holds every ledger.
  `messages.rs`'s `HavocDiagnostic` doc said the run-level diagnostic exists
  "because a run has one tape" - the exact pre-open-set claim
  `architecture.md`'s `/health` paragraph records as a former hole.
- A RULING'S OWED CORRECTIONS WERE NEVER MADE. The 2026-08-21 FeedLagged
  ruling named `reference/architecture.md`'s "closed with WS 1011" and the
  protocol doc's "the venue closes with WS 1011 after delivering it" as wrong
  and owing correction, and both stood unchanged through five later rounds.
  Both now state the advisory contract and record the close the serving path
  still performs as a standing code gap against the ruling, which keeps the
  reference true of the code without restating the fatal reading as the
  contract.
- THE SWAP HAZARD, in small instances: `architecture.md` had request symbols
  "materialize and board their own rivers" - boarding is a passenger's act -
  and `docs/config.md` had a boat placed at the origin "whenever it boards".
  And `docs/havoc.md`'s opening said transport and engine havoc is armed
  "against the one run, not against an account or a connection", contradicted
  by its own armed-per-account paragraph and by the Divergence entry; it now
  states the per-account arming, and the regime row's "baked into the run's
  tape" is the run's rivers.

RAISED AND DELIBERATELY LEFT, recorded so a later round does not re-file it:
the generated-sequence sense of `tape` survives where it is entangled with the
out-of-scope version and seed machinery - "tape root", "tape identity",
`TapeIdentity`, "tape byte", `tick-composition`'s measured tapes and the whole
version narrative - and untangling that family needs a ruling on the identity
vocabulary, not a sweep. `reference/performance.md`'s tape wording is the
durable record of past measurements and was not swept. `fills.rs`'s
"tape-shaped `TriggerScan`" and its `reference/performance.md` mirror were
left with it.

### Cross-cutting observations, recorded so they survive the merge

These belong to no single scope, so nothing else holds them.

- **Scopes 1 to 3 were inventoried against the pre-revision glossary.** The
  Venue, Client, River, Divergence and boot river entries changed afterwards,
  and Boarding did not exist when those passes ran. Several of their
  direction-2 rows argue that entries are wrong which have since been
  corrected, so those rows are already closed. Reconcile at the merge rather
  than re-running a pass.
- **The fork is owed by the code, and this is settled.** Scope 3 reported that
  generator havoc does not fork the river; scope 5 found the machinery that
  deliberately removed the fork and concluded the entry was stale. The entry is
  not stale. The glossary is the end state, so what scope 5 found is the size of
  the gap and the work that has to be undone to close it, not a correction to
  the entry. Nobody re-opens this as a naming question.
- **Doc comments attached to the wrong item are a family, not a coincidence.**
  Three instances across two scopes: two in `mogwai-venue`'s `run.rs`, where
  `evict_account` and `session_guard` both lost their doc to the following
  item, and one in `mogwai-engine`, where `free_balance`'s entire doc sits on
  `net_position` and `free_balance` has none. Nothing in the workspace detects
  this, and it is the kind of thing a lint or a rustdoc setting catches for
  free. Not a naming defect; recorded here because it falls between the two
  ledgers.
- **The late-boarder rule is open-coded twice, in two crates, with nothing
  shared** - the fee surcharge window in `mogwai-engine` and the FlowSurge
  branch of `arm_divergence` in `mogwai-venue`. That is the shape `AGENTS.md`
  says to anchor with a shared fixture or a common module.

### Retired

- `server` as a name for the process, its clock, its messages or its crate. It
  survives only as the name of Server mode.
- `client` as a name for anything this project owns. It survives only in
  inherited nautilus and FIX spellings.
- `session` as a name for the socket identity, on the wire and everywhere else,
  and - since the passenger re-cut - as a name for a socket's served tenure,
  which is a Passenger. It survives only for the trading day, which is the
  operator surface's overwhelming majority usage and is not ours to rename, and
  in the two regression sentences naming the retired `?session=` query key.
- `passenger` as a name for anything account-lifetime. A passenger is one
  connected trader on one socket, riding one boat and dying with that socket;
  the ledger, the risk state, the freeze stamp, the boat counts and the havoc
  arms are the Account's, and a per-account walk, a per-account engine pass and
  a per-account ledger are never per passenger. A count of riders on a boat is
  a count of passengers, which inverts the round-5 rider ruling.
- `seat` as defined vocabulary anywhere in durable prose. The rules it carried
  are stated over Account (one cadence per river ridden), Boatyard (wind-down
  when the last passenger leaves) and Freeze (off-river retirement on a frozen
  account's return). It survives as crate-local mechanism spelling -
  `try_sit`, `unsit`, `is_seated_on`, `seated_on` - and inside the
  consumer-visible "already seated on {symbol} at speed" refusal, neither of
  which any round has taken a break on.
- `admission` as a name for the history concurrency gate or for a connection's
  claim on an account. It survives for the per-connection outbound execution
  budget, which the wire publishes, and - unruled, and not to be swept as
  though it were retired - for `mogwai-engine`'s acceptance of an order onto
  the book.
- `reservation`, `lock` and `locked` as names for the funds a resting order
  ties up. That quantity is a Hold, and the verb is `held`. `reservation`
  survives for the per-connection outbound byte budget, which is the sense the
  wire and the operator config already publish; `locked` survives as the
  consumer-visible balance field and nowhere else in that sense.
- `warmup` as a name for an estimator's discarded burn-in prefix, or for a
  consumer's history request window. That prefix is a burn-in and the request
  window is a history window. The word survives for the servable simulated
  history from `data_origin_ns` through `run_start_ns`, which is the glossary's
  own entry - and, inherited and frozen, as a field name inside the walk cache
  key, the measurement artifact, the sub-contract tree and the screen's
  provenance string, where the identifiers around it carry the new word and the
  serialized bytes do not.
- `divergence` as a name for two numbers, shapes or outputs disagreeing against
  a reference. That is a mismatch. The word survives, undiminished, for one
  armed havoc injection - the whole of `mogwai_protocol::control::Divergence`
  and everything reached from it - and, inherited and frozen, as the
  `first_divergence` key of the count-curve artifact, whose spelling the
  preregistration fixed. The verb `diverge` and the adjective `divergent` stay
  available as ordinary English where they name no quantity.
- `ledger` as a name for the Databento delivery manifest, for one delivery in
  it, or for the operator flag naming either. The manifest is a jobs manifest,
  one entry in it is a delivery, and the flags are `--jobs-manifest` and
  `--delivery-key`. The word survives, undiminished, for the glossary's own
  sense - one `mogwai-engine` instance owned by one account - and, inherited
  and frozen, as the `LEDGER_KEY` member of the hashed sub-contract tree, the
  `ledger_key` member of the preflight and Stage M refusal artifact formats,
  and the whole seal-ledger vocabulary of `analysis/databento-seal-ledger.json`,
  `analysis/databento_seal_ledger.py` and the frozen purchase and
  preregistration documents. Unruled and not swept: `stage_m_tier2.rs`'s
  append-only candidate ledger.
- `template` as a name for what an account's engine is opened from. Those are
  the account opening terms - opening balances, fill seed, OMS type and
  fill-band limit - and a venue that holds them holds settings rather than a
  prototype ledger. The word has no surviving sense this project owns; it
  stands in durable prose only inside `http.rs`'s quotation of a comment that
  used to make the claim; `analysis/plot_tape.py`'s HTML page template and
  `notes/todo.md`'s proposed per-passenger config template are the ordinary
  English sense.
- `tape` as a name for the generated market-data sequence, and as a name for an
  offline CSV dump of one. The sequence is a river, a history window outside it
  is off-river, and the dump is a composition - `mogwai segments compose`. The
  word survives, undiminished, for what a boat publishes: the glossary's own
  entry, the broadcast ring, the delivery barrier, the backlog and the
  `tape_lateness` bench target. Out of scope rather than retired, by owner
  ruling: the version machinery - `TAPE_PROTOCOL_VERSION`, `TAPE_ORIGIN_NS`,
  the serialized `tape_protocol_version` keys, the bump rule and the version
  narrative - together with the `data_origin_ns` prose bound to it, and
  `analysis/plot_tape.py`'s established filename.

### Left as inherited

Recorded per scope in the scope reports until the merge collects them.

### Open, and not to be relitigated by an agent

- The Boat entry's claim that nothing a consumer can measure reveals whether it
  shares a hull is the end state, so the scope 3 finding - a history ceiling
  computed across every seated boat on a river - is a code gap against it, not a
  case for weakening the entry. What is unruled is only whether the gap is worth
  closing now.
- The `RunComplete` overload: one frame announcing both a run ending and one
  connection's own deadline elapsing.

## What the rename rounds keep getting wrong

Eleven rounds in, five failures recur often enough to belong in every brief.
Round 6 produced the first three at once, round 8 did it again and added the
fourth, round 9 produced all four, round 10 - a round of almost nothing but
comments - still produced two, and round 11 produced four of the five.

Round 9 adds a fifth, and it belongs to any round that MOVES A WORD FROM ONE
JOB TO ANOTHER rather than retiring it. **THE SWAP HAZARD: when a rename gives
an existing word a new referent, every sentence already using that word is a
site of the rename.** The compiler is no help at all - both meanings are the
same spelling - and the sentences read fluently either way. Round 9 handed
`Passenger` from the account-lifetime object to the socket, and the survivors
were not near the renamed types: a vector of accounts named
`attached_passengers` in another module, a doc promising a ledger "on ONE
PASSENGER'S ledger", a sweep documented "PER PASSENGER". Grep the word after
the swap and read every hit against the NEW entry, not the diff.

Round 8's fourth, and it is the one with teeth. **A SERIALIZED FIELD NAME
IS NOT AN IDENTIFIER.** A rename pass sweeps a word through a crate and takes
the string literals with it, and where one of those literals is a key in a
committed artifact, a content-addressed cache, a hashed constant tree or a
provenance token, the sweep has broken a format rather than renamed a thing.
Half of these fail silently: a missing key that reads as zero, a cache that
misses every entry and merely runs for hours. The check at a string literal is
"who else has already written bytes under this name", and the answer is almost
never in the crate being swept.

**The sweep stops at one crate.** A rename pass finds the family in the crate
that owns it and leaves live siblings in the others. Twice now a false sentence
corrected in one crate survived verbatim in two more, because the round that
fixed it grepped the file rather than the sentence. Grep the sentence across the
workspace, not the identifier inside the crate.

**The word moves and the sentence is never re-read.** The characteristic damage
is not a wrong identifier, which the compiler catches, but a sentence that came
out false, vacuous or ungrammatical around a correct substitution - a doc
promising that a minted ledger is identical to a minted one, an article left
disagreeing with its noun. The check at every site is "does the new word make
this sentence true", never "is this the ruled sense".

**Emphasis gets half-swept.** Where a sentence carried a contrast in capitals,
a pass will lowercase one half and leave the other, destroying the distinction
the sentence existed to draw. Whatever the house style for emphasis, both halves
move together or neither does.

The evidence for all of them is that the cold review found something in three
rounds of eleven and the close pass found something in eleven of eleven. That is not
a criticism of the cold review, which catches what a reader without the arc's
priors sees. It is that a rename arc's defects live in prose, and prose defects
compile, pass and read fluently.

## Standing rules

- **The inventory does not fix.** Not even the one-line obvious ones.
- **A scope with no findings is a result.** Do not send a third pass at it.
- **Do not tune the brief between waves** to chase the rows you wanted.
- **Every naming fork goes to the owner.** No adjudicator.
- **The glossary is not presumptively right.** Direction 2 is real, and three of
  its entries have already been corrected rather than renamed toward.
- **A frozen document is never a sweep target.** Preregistrations and closed
  specs record what was decided before the result was known, and a rename that
  edits one destroys the only thing it was for. Round 10's pass edited
  `notes/count-curve-preregistration.md`,
  `notes/pair-test-preregistration.md` and
  `notes/protocol-12b-arrival-composition-spec.md`; all three were reverted. The
  12b spec states its own form of the rule - amendments go in section 17, never
  edits to the body - and the two preregistrations state theirs by being
  preregistrations. A word retired by this arc stays spelled the old way inside
  them, and where such a document fixes a serialized key, that key is frozen for
  the code too.
- **Markdown never commits alone.** It rides with the code change it describes.
- **Subagents are foreground, never nested.** Never worktrees. The orchestrator
  validates between agents; agents do not run the build.
- **Commit on master, never a branch. Never push.**
- **The tape is not this arc's subject, and neither is its version.** Owner
  ruling, 2026-08-21. `TAPE_PROTOCOL_VERSION`, what it has or has not
  identified, the version narrative in `reference/architecture.md`, and the
  unconditional bump rule are all outside the scope of vocabulary
  reconciliation. This arc exists so that everyone uses the same word for the
  same job in future; it does not adjudicate anything about tape generation.
  Raised three times in the arc's first day and declined each time, so a round
  that finds itself reasoning about a tape identity has left its scope. The
  `tape` round renames the word where it does four jobs; it says nothing about
  the constant beyond the per-round statement of whether a byte moved, which
  every round already owes.
