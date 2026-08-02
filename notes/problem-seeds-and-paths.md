# PROBLEM: the tape varies by accident, and nothing records which one you got

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. Sibling of
`notes/problem-server-lifecycle.md`: that one is about how an instance starts
and dies, this one is about which market it generates and what a consumer can
honestly claim from having traded against it.

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

1. **What sets `data_origin` once it is off the wall clock.** It is part of tape
   identity and every history request is bounded by it. Candidates are a config
   value, a function of the seed, or a fixed epoch. `wall_anchor_ns` already
   exists to pin the anchor so restarts land on the same axis, so some machinery
   is there.
2. **Seed granularity.** One seed per run, per instrument, or per subscription.
   Today it is per symbol via FNV. If a run holds several instruments, they must
   be decorrelated from one run seed without collisions - deriving per-symbol
   seeds from (run seed, symbol) is the obvious approach and its independence
   properties should be stated rather than assumed.
3. **What counts as an independent path**, and whether independence is checked
   or asserted. Seed alone, or seed plus origin, or seed plus session phase.
4. **Where the run reports itself.** A `/run` endpoint, a field on `/clock`, a
   structured log line at startup - and whether the adapter propagates it into
   nautilus so a consumer captures it without scraping. The adapter half lives
   in this repo; the capture half does not.
5. **What else belongs in that record.** Seed and epoch at minimum. Arguably
   also the effective instrument profiles, the fingerprint version, the armed
   havoc and the binary version - at which point it is a run manifest rather
   than a seed report, and the question is whether to build one.
6. **Whether a restarted venue resumes its path.** If an instance dies mid-run
   and something restarts it, reusing the seed reproduces the tape from the
   origin but not the cursor. Whether that counts as the same run is a question
   the lifecycle document's restart policy also needs.

## What this document does not decide

How instances start (`notes/problem-server-lifecycle.md`). What the tape's
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
