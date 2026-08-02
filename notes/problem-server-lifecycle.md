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

## Who the operator is

Not a human. The entity that starts venues, runs smokes, reads failures and
kills processes is an agent, and at fleet scale it is hundreds of them. Every
piece of operator ergonomics should be read that way: a readiness pipe is the
right shape, a startup banner has an audience of nobody, and "check the log
file" is not a diagnostic.

This is not hypothetical. Every venue started during the 2026-08-02 session was
started by an agent, and the friction below was hit by one.

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
  history than it advertises.
- **`mogwai.log`, `mogwai.pid` and `mogwai.toml` are cwd-relative.** Concurrent
  runs in one tree collide on all three.
- **Attributability**, if the venue is ever embedded in the consumer's process:
  a venue-side stall and a strategy-side stall sharing one worker pool become
  indistinguishable, and the fixture stops measuring what it exists to measure.
  This is a fidelity property, not a performance one.

## What must be decided

1. **Subprocess or embedded.** A subprocess spawned by the adapter keeps the
   process boundary that makes venue stalls attributable, needs no library
   extraction, and dies with its parent through a held pipe. Embedding removes
   the spawn entirely but requires `mogwai-server` to gain a lib target (it is
   bin-only today, ~758 non-test lines in `main.rs`, the rest being daemon
   plumbing and tests), and raises the attributability question above. An
   assessment of embedding exists in the 2026-08-02 session record; its
   conclusion was feasible and mechanical, with two genuinely open sub-questions
   - who owns the venue when two clients share it, and the runtime coupling.
2. **What identifies an instance**, given nautilus builds a DATA client and an
   EXECUTION client from two independent factories that share no state. One
   venue per PAIR is required; the pairing has no representation today, and the
   `account_id` both clients now must state (as of the 2026-08-02 fix) is the
   obvious candidate key.
3. **How the venue and its client find each other** without a fixed port.
   `127.0.0.1:8787` is currently hardcoded independently in three places: the
   server's `--addr` default, the adapter's `DEFAULT_BASE_URL`, and the smoke
   harness.
4. **What guarantees death.** A parent killed with SIGKILL leaves a child
   reparented and running on Linux. The venue's lifetime must be tied to the
   parent PROCESS rather than to connection state - the adapter reconnects by
   design, so exiting when the last socket closes would make a transport blip
   fatal.
5. **Which parts of the CLI survive.** `serve` and `stop` are the serving half.
   `gen` dumps the offline generator to CSV and is what `analysis/plot_tape.py`
   drives for charting; `man` renders bundled reference docs. Neither serves a
   client.
6. **What happens to the multi-consumer machinery** - accounts, reaping, caps,
   tape arbitration. Some is vestigial under one instance per run; some is still
   load-bearing because one client has two sockets and many subscriptions.
7. **How the test surface adapts.** Every smoke scenario starts a server out of
   band on a fixed port and says so in its module doc.

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
