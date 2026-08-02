# PROBLEM: mogwai is one long-lived service, and the workload is hundreds of disposable instances

**This is a PROBLEM STATEMENT, not an implementation spec.** It is what the
author of a `reference/technical-implementation-spec.md` document reads BEFORE
writing one: the observed defect and its evidence, the decisions still open and
who settles them, and what is deliberately out of scope. It contains no
implementation plan, names no target artifacts, and pins no gates - if it reads
as under-specified, that is the genre rather than an omission. One resolved
problem statement yields one or more specs.

Expanded from what would otherwise be a `notes/todo.md` entry. This is the ROOT
document of the current set: it is resolved first, and it supersedes an earlier
draft (`problem-per-consumer-scope.md`, deleted) that framed the same ground as
a question about which config knobs move per consumer. That framing was wrong.
The knobs are not the problem; the number of venues is.

## What the user wants

mogwai should be fire-and-forget per client: fire up two strategies at once and
nothing whatsoever is shared between them. Whether the venue is started
explicitly or spawned by the adapter is explicitly NOT important. What matters
is that instances are independent, that one dies when its owner dies, and that
nothing about starting one is ceremony.

The end state that makes this urgent: on the order of 200 agents running
concurrently, each developing a strategy - backtesting, optimizing and running
Monte Carlo through broadarrow, then FORWARD TESTING it against mogwai. Each
of those forward tests needs a venue.

Explicitly out of scope by the user's instruction: whether 200 instances fit on
the machine. If the answer turns out to be 20, that is a hardware problem to be
solved later and must not shape any decision here.

## What the user has since settled

Four rulings that this document previously left open or did not ask. They are
premises for every spec descending from here.

- **Forward tests always run ACCELERATED.** Never speed 1.0. This is a premise
  no document previously stated, and it is a correctness constraint rather than
  a cost one: the adapter's `MIN_WALL_REQUEST_TIMEOUT_SECS` of 1 is already
  flagged in its own comment as the tightest cap on usable sim speed, and a
  request that times out at speed N is a failed run rather than a slow one. It
  therefore sits outside the resource-cost exclusion above.
- **A run has an optional duration, defaulting to indefinite.** The adapter can
  be told to run for N seconds, minutes, hours or days; told nothing, the
  instance runs until its owner dies. The duration is in SIM time, not wall
  time - under mandatory acceleration those differ by the speed factor, and the
  adapter is where the ambiguity would land, so the spec states it.
- **There is no restart and no resume.** mogwai is fire and forget. An instance
  that dies is gone; nothing resumes its path. This CLOSES decision 6 of
  `notes/problem-seeds-and-paths.md` rather than deferring it. Reproducing a
  path means launching a new instance with the same seed and the same config,
  which reproduces from the origin because the tape is a pure function of
  (seed, config) once the wall-clock anchor is removed.
- **Everything is on the sim clock, and REAL latency is not modelled.** Recorded
  as a ruling because it keeps being re-raised: under mandatory acceleration a
  sim-axis latency figure and physical wall latency appear to move in opposite
  directions (a 30 ms modelled latency is 0.3 ms of wall at 100x, while a real
  microsecond of wall reads as 100 microseconds of sim), and this looks like a
  conflict. It is not one mogwai can or should resolve. The venue models latency
  on the sim axis only, it does not measure or compensate for physical latency,
  and it runs over loopback on the same machine as its client, where physical
  latency is negligible by construction. Do not reopen this without a measured
  case where it actually distorts a result.
- **Warmup duration is declared config.** How much history a run needs before
  the strategy trades is the user's decision, not the venue's. Two consequences.
  The venue knows its horizon at boot, so it generates warmup EAGERLY rather
  than serving it lazily per request - which removes the silent failure where an
  exhausted seek returns an empty SUCCESSFUL HTTP response, and lets a request
  reaching beyond the declared horizon be refused precisely. And
  `MAX_HISTORY_SEEK_TICKS` dies with that: it is a latency bound on the request
  path (190,000 ticks sized against ~1.9M ticks/sec synthesis to fit a ~100 ms
  request budget), not a memory bound, and once no request needs a long seek it
  protects nothing. The user's position is that requesting a two-year warmup and
  the consequences of doing so are theirs to own. See the correctness section of
  `notes/problem-trade-cadence.md` for the surviving numbers.

## Who the operator is

Not a human. The entity that starts venues, runs smokes, reads failures and
kills processes is an agent, and at fleet scale it is hundreds of them. Every
piece of operator ergonomics should be read that way: a readiness pipe is the
right shape, a startup banner has an audience of nobody, and "check the log
file" is not a diagnostic.

This is not hypothetical. Every venue started during the 2026-08-02 session was
started by an agent, and the friction below was hit by one.

On the standing of that evidence, since two review passes have raised it: the
frictions below are SESSION OBSERVATIONS, not regression cases. No logs were
retained and no test reproduces them. They are recorded because they happened,
and the two that are structural rather than anecdotal - a fixed port permitting
one concurrent run, and cwd-relative artifacts colliding - are verifiable from
the code without reference to the session at all. The orphaned-venue item is
the weakest of them and is inferred rather than observed. None of this changes
the document's argument, which rests on the fixed port.

## The observation

mogwai is shaped like a service an operator runs once. `serve` daemonizes by
default, `-f` keeps it in the foreground, `stop` ends it through a PID-file
lock, and it binds a fixed `127.0.0.1:8787`. Clients are expected to find it
already running.

Concretely, the friction that shape produced in one session:

- **The lock deadlock.** `brokkr run mogwai -- serve -f` holds the system-wide
  brokkr lock for the venue's entire lifetime, so `brokkr run mogwai -- stop`
  queues behind the process it is trying to stop and never acquires. The
  documented way to stop a venue cannot work while that venue is running under
  the documented way to start it.
- **The kill escape hatch does not match.** `pkill -9 -f mogwai-server` is
  allowlisted; the binary is `mogwai`, so it matches nothing.
- **Hand serialization.** Four smoke scenarios in one session, each requiring
  start, run, stop before the next could begin, because the port is fixed. Two
  agents doing this concurrently would silently connect to each other's venue.
- **Abnormal death is routine.** Agents are killed by their harness mid-run. An
  orphaned venue holding 8787 poisons the next run and nothing prevents it.

At fleet scale each of those stops being friction and becomes a wall. A
hardcoded port permits exactly one concurrent forward test per machine.

## What one shared service forces that hundreds of instances would not

- **A multi-account namespace.** Accounts are created implicitly on the first
  request carrying an unknown `x-mogwai-account`, bounded by `max_accounts`,
  reaped on an idle timer, deletable over HTTP, and funded from a template that
  `reference/config.md` states is explicitly NOT per-account. All of it exists
  so one process can serve strangers. The 2026-08-02 defect - a data client
  silently defaulting to `MOGWAI-001`, auto-creating a slot, holding a session
  on it and diverting every server-armed divergence away from the market-data
  feed - was only possible because accounts are a namespace inside a shared
  server.
- **Cross-consumer tape arbitration.** `TapeKey` identity and refcounted
  sharing, the bounded fanout ring, `FeedLagged` promoted to a connection-killing
  venue fault, `zero_speed_stall_ms` for a tape blocked on its slowest
  subscriber. Note this does not disappear entirely: one client still has two
  sockets and many subscriptions, so sharing WITHIN an instance survives.
- **Config as a process-global boot artifact.** One `mogwai.toml` read once from
  the process cwd, applying to everyone. Under one instance per run this stops
  being a scope problem at all - the process IS the consumer.
- **State that outlives a run.** Accepted order ids, closed orders and fill
  history accumulate for as long as the daemon lives, which is the opposite of
  what a reproducible fixture wants.

For the record, since the superseded document inventoried it: the knobs are
server-global (`speed`, `gap_cap_ms`, `sim_epoch_ns`, `wall_anchor_ns`,
`backfill_horizon_ns`, `penetration_ticks`, `fill_sweep_interval_ms`,
`max_concurrent_tapes`, `fanout_depth`, `zero_speed_stall_ms`, the
`[[instrument]]` set), per-connection with an operator-chosen limit
(`exec_held_budget_bytes`, `admission_lane_frames`, `admission_promise_tickets`,
`pending_command_acts`, `max_subscriptions_per_connection`), per-account
(armed divergences, but NOT `[balances]`), or per-subscription (`MarketRegime`,
the one knob a consumer picks for itself).

## Correctness consequences, as distinct from cost

Resource cost is out of scope per the user. These are not cost:

- **Two concurrent runs on one port produce wrong data, not slow data.** One
  connects to the other's venue, with the other's config, clock and instrument
  set.
- **`MAX_HISTORY_SEEK_TICKS` (190,000) against a 24 h `backfill_horizon_ns`.**
  At any cadence worth targeting the seek budget stops reaching the start of the
  window a client is permitted to request, so the venue silently serves less
  history than it advertises. Closed by the declared-warmup ruling above: the
  cap is a latency bound on LAZY history, and eager generation at boot leaves it
  nothing to protect.
- **`mogwai.log`, `mogwai.pid` and `mogwai.toml` are cwd-relative.** Concurrent
  runs in one tree collide on all three. The PID file and the config read stop
  existing under the resolution below - the launcher holds the handle and
  supplies the config - so the log is the only artifact that still needs a
  per-run home, and choosing it is the operator's problem.
- **Attributability**, if the venue is ever embedded in the consumer's process:
  a venue-side stall and a strategy-side stall sharing one worker pool become
  indistinguishable, and the fixture stops measuring what it exists to measure.
  This is a fidelity property, not a performance one, and it is the argument
  that settled decision 1.

## RESOLVED

All of it. This document is settled and ready for a spec. The list below was
seven open decisions; four of them DISSOLVED rather than being answered, which
is the characteristic result of reasoning from the end state instead of from the
shape of the current code. Read the dissolutions carefully - a spec author who
re-derives these questions from today's implementation will re-invent machinery
that has no reason to exist.

1. **A separate process, started by the run launcher.** Not embedded, and not
   spawned by the adapter either. Separate because the process boundary is what
   makes a venue-side stall distinguishable from a strategy-side one, and under
   mandatory acceleration a busy venue and a busy strategy sharing a tokio
   executor would contend for threads continuously - so embedding does not merely
   risk the attributability property, it forfeits it. Whoever launches the run
   launches the venue: it is not the adapter's job, and a library holding a child
   process inside someone else's node is a worse owner than the thing that owns
   the run.

2. ~~**What identifies an instance.**~~ DISSOLVED. Nothing does, because nothing
   is looking one up. An instance is a process with an endpoint; the launcher
   created it, so the launcher holds the handle and knows the endpoint. There is
   no namespace, no registry and no key to agree on. The account follows: one
   instance means one ledger, so the account name identifies nothing and every
   instance may use the same literal. It stops being a key and becomes a label.

   Recorded so it is not re-derived: this question existed because two nautilus
   factories build the DATA and EXECUTION clients independently, share no state,
   and both must reach the same venue - which is what produced the `MOGWAI-001`
   misbinding. That pairing is a coordination problem ONLY if the adapter spawns
   the venue. When the launcher creates it first and writes the endpoint into
   both configs, both clients are told rather than discovering, and the two
   nautilus clients remain two objects without being two decisions.

3. ~~**How the venue and its client find each other.**~~ DISSOLVED with 2. A
   launcher that allocates the port knows the port. What survives is not a
   rendezvous protocol but a readiness record: with an ephemeral port the venue
   must report its endpoint back to whoever started it, over an inherited
   channel, which is also the signal that it is ready to serve. The three
   independently hardcoded copies of `127.0.0.1:8787` all go.

4. **Death has exactly two causes.** The launcher kills it, or it reaches its
   declared run duration and stops itself. Nothing else ends a venue - not the
   last socket closing, since the adapter reconnects by design and a transport
   blip must not be fatal. Where the process artifacts live is the operator's
   problem, in the same class as choosing an extreme warmup.

5. **`serve` and `gen` survive; `stop` and `man` go.** `serve` runs in the
   foreground and does not daemonize, because a launcher wants a child it owns.
   `stop` has nothing to stop once the launcher holds the handle. `gen` survives
   on its own merits as the offline tape dump, and matters more than it used to,
   since the repository owner IS the realism gate and generated tapes are what
   they inspect. `man` renders bundled docs for an audience that reads files
   directly.

6. **The multi-TENANT machinery goes; the LAG machinery stays.** This split is
   the one an implementer is most likely to get wrong, because both halves look
   like the same "one process serving many" apparatus and they are not.

   Gone, because there are no strangers: accounts as a namespace, implicit
   creation on first unknown header, `max_accounts`, the idle reaper, account
   deletion over HTTP, and arbitration between consumers who do not know each
   other.

   STAYS, because it is FIDELITY rather than tenancy: the bounded fanout ring,
   the lag policy, and killing a connection that falls behind. A real venue's
   clock does not wait for anyone - the market runs whether or not you are
   watching the DOM, and real exchanges disconnect slow consumers. A venue that
   paced itself to its consumer would be an in-process backtest sandbox with
   extra steps, which is precisely what this project exists not to be. So the sim
   clock advances on wall clock times speed, unconditionally; the venue never
   pulls, never waits and never acknowledges; and a consumer that cannot keep up
   gets disconnected. That a strategy cannot keep up with the tape is a real
   finding a forward test SHOULD surface.

7. ~~**How the test surface adapts.**~~ NOT A DECISION AT THIS LAYER. It
   resolves per implementation commit, the same layer error the acceptance
   paragraph in `notes/todo.md` made.

## What this document does not decide

The content of any knob, the cadence, the book, or the order types. It removes
an obstacle from the book in particular: a per-run process can hold mutable
per-run state, and a book that client orders mutate is exactly the state a
long-lived shared daemon made dangerous.

Seed policy is a sibling document (`notes/problem-seeds-and-paths.md`) - it is
about what a run is, not how it starts.

## Known cost, explicitly not a decision input

Per the user's instruction, resource cost does not shape this. Recorded only so
it is not rediscovered as a surprise: N runs become N processes, each with its
own tapes, threads, checkpoint store and generator state, and per-run startup
becomes a real number where a warm shared venue had none.
