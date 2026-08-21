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
| 1 | The wire: the whole of `mogwai-protocol`'s public surface. `ClientMessage`, `ServerMessage`, `control::Divergence`, `close`, `risk`, `launch` and `ReadyRecord` are the load-bearing parts, not the boundary |
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
wrong for the same reason `ClientMessage` is. The crate already classifies its
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
- Havoc the adapter applies to its own inbound stream, `ClientHavoc` and
  `HavocSpec.client`: not the consumer sense at all, and wrong under any
  reading. Takes an adapter or inbound word, not `consumer`.
- Any consumer-supplied echoed id, `MAX_CLIENT_ID_LEN` and `truncate_client_id`,
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
- `session` as a name for the socket identity, on the wire and everywhere else.
  It survives only for the trading day, which is the operator surface's
  overwhelming majority usage and is not ours to rename.

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
- `passenger`: the glossary says one per account, and several sites count
  connections.

## Standing rules

- **The inventory does not fix.** Not even the one-line obvious ones.
- **A scope with no findings is a result.** Do not send a third pass at it.
- **Do not tune the brief between waves** to chase the rows you wanted.
- **Every naming fork goes to the owner.** No adjudicator.
- **The glossary is not presumptively right.** Direction 2 is real, and three of
  its entries have already been corrected rather than renamed toward.
- **Markdown never commits alone.** It rides with the code change it describes.
- **Subagents are foreground, never nested.** Never worktrees. The orchestrator
  validates between agents; agents do not run the build.
- **Commit on master, never a branch. Never push.**
