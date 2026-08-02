# PROBLEM: the tape varies by accident, and nothing records which one you got

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. Sibling of the
now-landed server-lifecycle work (see git history): that one was about how an
instance starts and dies, this one is about which market it generates and what
a consumer can honestly claim from having traded against it.

## What the user wants

Hundreds of agents forward-testing dealt strategies, each producing a claim
about whether a strategy works. The user's position on determinism is explicit:
nobody cares whether the tape is reproducible as long as it is realistic, and
they would EXPECT a different tape on every launch - anything else invites the
end user to overfit to one realization. The variation should come from a random
SEED, and the wall-clock anchor should go, so that the tape is deterministic
GIVEN a seed and the seed is the only axis that moves.

The strategy-dealing doctrine those agents work under demands the same thing
from the other direction: a scope-qualified success is a terminal state, but
"adequate" means honest evaluation within the scope - out-of-sample, and
MARGINALIZED OVER PATHS and over interchangeable symbols of the same session
class. It also names forward testing against an accelerated synthetic tape as
the instrument for validating resting-order exits, because a bar-close backtest
of those is two simulators agreeing.

## The observation

An earlier draft of this document claimed every instance generates the identical
tape because the seed is an FNV hash of the symbol. That is wrong, and the truth
is worse.

Tape identity is `(symbol, data_origin_ns, regime)`, and `data_origin` is
derived from WALL TIME at boot. So two launches already produce different tapes
- the seed is fixed by symbol, but the origin moves, and the generator restarts
its RNG and price state at whatever anchor it is given. Runs vary.

They vary by accident. Nothing samples the variation, nothing bounds it, and
nothing records it. A run cannot say which market it traded against, two runs
cannot be shown to be independent draws, and a failure cannot be re-examined
because the thing that produced it is gone with the clock that made it.

A related consequence for a claim: because the anchor moves with wall time and
session modulation reads absolute UTC hour and day, two runs launched at
different times of day start in different session shapes. That is a real
difference in market conditions arriving through an axis nobody chose.

## The decision the user has already made

One variation axis: the seed, drawn randomly at launch, fixed for the run's
lifetime. The wall-clock anchor is removed, so the tape is a pure function of
(seed, config). Different every launch, reproducible on demand when someone asks
for that seed.

Two things fall out of that and are not in tension with it:

- **The seed must be reported.** Different-every-time and knowing-which are
  independent properties. Recording it is what lets a strategy that blew up on
  one path be re-run on that path, and lets a claim say it survived forty
  DISTINCT paths rather than forty launches that were probably distinct.
- **Within-run determinism survives untouched.** The checkpointed seek requires
  the tape to be a pure function of its state; one fixed seed per run gives that
  completely. So does the realism gate, which pins the generator's correctness at
  seed 42 and is testing the process rather than any served path.

## What must be decided

ALL FIVE ARE SETTLED by the user. They are recorded as rulings rather than as
questions, and a spec descending from this document implements them rather than
re-deriving them. Where a ruling made a question meaningless rather than
answering it, that is said plainly.

1. ~~**What sets `data_origin` once it is off the wall clock.**~~ SETTLED: a
   FIXED EPOCH AT ZERO. The tape begins at `0 - warmup_ns`, reaches 0 at the
   instant the run proper begins, and proceeds either indefinitely or until the
   configured duration is met. `sim_epoch_ns` already defaults to 0, so this is
   the DELETION of the wall-derived origin rather than the addition of a new
   mechanism, and `wall_anchor_ns` goes with it.

   The drafting concern that a fixed epoch starts every run in the same session
   shape was raised and dismissed, correctly: forward tests always run
   accelerated, so a run of any useful length sweeps every hour of the day
   regardless of where it starts.
2. ~~**Seed granularity.**~~ SETTLED: ONE SEED PER RUN, and at the end state
   per-run is synonymous with per-boot and per-instance. Everything derives from
   it WITHOUT EXCEPTION - the tape generator and the fill band's draw stream
   alike. It is global, random per launch, and overridable in config.

   Two thirds of the original question died with the lifecycle landing rather
   than being answered: one instance serves one instrument and subscriptions no
   longer exist, so there is nothing to decorrelate and the per-symbol FNV
   derivation now hashes a constant. What survives is the derivation discipline:
   the fill band's stream must stay separate from the generator's, because
   drawing the band from the generator's stream would advance it and make the
   tape a function of how many orders the client placed. Separate STREAMS, one
   SEED.
3. ~~**What counts as an independent path**, and whether independence is checked
   or asserted.~~ SETTLED, and the question was largely meaningless once 1 and 2
   landed: with the origin constant and one seed feeding every stream, two runs
   differing only in seed are two draws from the same process. Independence is
   ASSERTED by construction and nothing verifies it. Nothing could: fire and
   forget means no state outlives a run, so a duplicate seed across two
   concurrent instances is undetectable by design, and ensuring distinct seeds
   across a fleet belongs to whatever launches them.
4. ~~**Where the run reports itself.**~~ SETTLED: the READINESS RECORD ONLY. No
   `/run` endpoint, no field on `/clock`, no propagation into nautilus. The
   launcher started the run and allocated its endpoint, so it is both the thing
   that needs the seed and the thing attaching provenance to a result; a wire
   surface would let a client read a value it has no use for, since the strategy
   is not what makes the claim. The record already exists and already carries a
   staged seed field.
5. ~~**What else belongs in that record.**~~ SETTLED, and NOT as a manifest. The
   record carries the seed; what makes two runs comparable BEYOND the seed is a
   single TAPE GENERATOR PROTOCOL VERSION constant, exposed through the CLI's
   `--version` output. The operator records it, and a bump tells them new runs
   are not equivalent to their old ones.

   Rejected in favour of it: giving the fingerprint an identity (version, corpus,
   era, fit date) and reporting instrument configuration, binary version and
   armed havoc in the record. The version constant answers the same question -
   would this reproduce - at a fraction of the surface.

   The honest limit: nothing can detect that a generator constant changed while
   the version did not. Bumping it is a discipline, and a discipline needs to be
   visible where the change is made rather than only where the constant lives.

   So `AGENTS.md` MUST reference this constant by name and state the obligation
   outright: any code change that affects tape determinism has to bump it. A doc
   comment at the definition is not sufficient and is not the deliverable - the
   person changing an ACD constant, a GARCH parameter, the fingerprint, or any
   part of the generation path is not reading the version constant's own
   comment. `AGENTS.md` is what they do read, so that is where the rule binds.
   The comment at the definition stays as well, pointing back at it.
6. ~~**Whether a restarted venue resumes its path.**~~ CLOSED by the user. There
   is no restart and no resume - mogwai is fire and forget, and an instance that
   dies is gone. Reproducing a path means launching a NEW instance with the same
   seed and config, which reproduces from the origin because the tape is a pure
   function of (seed, config) once the wall anchor is removed. Recorded here
   rather than deleted because the question was reasonable and the answer
   constrains the run record in decision 5: nothing has to record a CURSOR,
   because there is no partial run to resume into.

   Note the narrower claim. An earlier version of this entry said "a seed plus
   config identifies a path completely", which overstates it and contradicts
   decision 5's own list. The tape is a pure function of (seed, config) FOR A
   GIVEN BUILD AND FINGERPRINT: change the generator's constants, the committed
   fingerprint, or the process itself, and the same seed draws a different path.
   So the reproducible unit is (seed, config, fingerprint version, binary
   version), which is precisely why decision 5 asks whether the record is a seed
   report or a run manifest.

## What a path is evidence OF, and a second variation axis

Raised in review and worth carrying into the spec, because it changes what the
record has to say rather than what the venue has to do.

A new seed draws a new path from ONE FITTED MODEL. Marginalizing over seeds
therefore reduces variance conditional on that model; it does not reduce model
error and it is not out-of-sample market evidence. Forty distinct paths are
forty draws from one fingerprint fitted to one corpus, so if the model is wrong
in some respect all forty are wrong in it identically. That is a real limit on
what a consumer may conclude, and mogwai is the only party positioned to state
it, since the consumer cannot see how the tape was made.

The user's response is a SECOND AXIS: support multiple fitted models, derived
from different corpora, so a claim can marginalize over models as well as paths.
Mechanically this is cheap - the fingerprint is already a JSON artifact the
generator reads, so another one is a file plus a selector. Two honest limits on
what it buys. It samples PARAMETER uncertainty, not STRUCTURAL model error:
refitting the same process family (ACD arrivals, GARCH volatility, Student-t
innovations) to new data gives different constants, not a different model, so a
flaw in the family survives every refit identically. And models fitted from the
same venue and era are correlated draws rather than independent ones, which
makes different ERAS the cheapest axis worth sampling - though not a
"genuinely independent" one, as an earlier draft called it: different eras of
one venue still share its market structure, overlapping regimes and the same
model family. They reduce shared sampling dependence, nothing more.

Two consequences for the spec. The realism gate has to assert per model rather
than once, which is the gate-scoping question the profiles document also raises.
And a fingerprint gains an identity - version, corpus, era, fit date - which
`notes/problem-instrument-model.md` decision 10 needs anyway for the
fitted-versus-declared distinction, so it is the same work.

The user is the realism gate and accepts a fingerprint by inspecting generated
tapes against real ones. That is a coherent stopping rule and the record should
say so plainly rather than implying more: a run's provenance should name which
seed, which model, and that the model was accepted by the owner on comparison
against a named corpus. One thing visual comparison cannot gate, and the record
should not imply it does: a tape can match every marginal moment - rate, size,
burstiness, session shape - and still be systematically easier or harder to
trade, because what pays a strategy is conditional structure rather than
marginal shape.

Related, and currently unenumerated in `notes/problem-trade-cadence.md`: making
a whole-second corpus "statistically sub-second" is a MODELLING choice, not a
preprocessing step. Kraken records 61% of consecutive prints in the same second,
so any sub-second structure is manufactured by whatever rule spreads them, and
the duration ACF that comes back out is a property of that rule. Defensible if
declared, per the discipline already in these documents.

## What this document does not decide

How instances start (settled by the now-landed server-lifecycle work; see git
history). What the tape's
cadence or shape is - a different seed draws a different path from the same
fitted process, and changing the process is the cadence, profile and book
documents. It also does not decide what a consumer does with the paths: how many
make a claim, how they are allocated across a fleet, and how provenance is
attached to a result are the consumer's pipeline, and the venue's obligation
ends at generating a path and saying which it generated.

## Known cost, explicitly not a decision input

Per the user's standing instruction, resource cost does not shape this. N paths
per strategy multiplies instance count by N; a fleet that wants both fair
comparison and marginalization needs both axes.
