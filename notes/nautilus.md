# Wanted upstream nautilus_trader PRs

Moved out of `notes/todo.md` on 2026-09-15: nautilus moves far more slowly than
the backlog does, so these are re-read at a pin bump rather than daily. Nothing
here can be fixed from this tree. Revisit the whole file on every nautilus bump.

Read the source from `research/nautilus_trader`; build against the pinned
crates.io release. Each of these names what the other side would have to ship,
which is what makes it a writable patch rather than a grievance.

Every entry was re-verified 2026-09-15 against the 0.64 pin: the cash-account
guard and the `try_send_account_state` request closed, and the rest stand
unchanged. Before that, every entry was re-verified 2026-09-02 against the 0.63
pin rather than against memory. That is worth doing for a reason this file
should keep in view: the checkout used to sit on `develop`, which is neither
what we link nor what this file claims to describe, and a maintainer may
reshape or reject what we file, so an entry here can go stale without anyone
touching this repository. Of the five carried into 0.63, two still stood and
two were struck - the mass status default, closed at 0.62, and the execution
event sender, closed at 0.63 by #4874 plus the binding-order answer recorded
below. One new entry, fractional-share equity, was filed at the bottom.

Two lessons from closing the sender entry, both worth keeping. Reading #4874's
title as closing it outright would have been wrong - the entry offered two
acceptable patches and only one landed. But so was assuming the other patch was
therefore owed: the second half turned out to be unnecessary rather than
outstanding, and only asking upstream surfaced that. An entry here names a
problem, not a solution, and it closes when the problem goes away by any route.

- ~~**The execution event sender cannot be obtained off the runner's thread.**~~
  **Closed, verified at the 0.63 pin.** Half by #4874 and half by an answer that
  showed the second half was never the problem.
  This entry originally named two acceptable PRs - a shared cell for the
  emitter's sender, or resolving it from a process-wide rather than thread-local
  slot. #4874 shipped the first: `live/src/execution/emitter.rs` now holds
  `sender: Arc<ArcSwapOption<UnboundedSender<ExecutionEvent>>>`, documented as
  "Clones share the sender slot and observe later sender installations and
  replacements", with `test_clone_before_set_sender_observes_sender` pinning it.

  The rest is closed too, by an answer rather than a change. Upstream declined
  both shapes this entry asked for, with reasons that hold: a process-wide
  fallback cannot tell which runner an unbound thread belongs to when two
  runners live in one process, and their own live test binary binds runners on
  spawned threads, so a static would be order-dependent. Passing the sender
  through `create` adds nothing, because it is already bound by the time
  `create` runs.

  What we had missed is that the sender is already reachable where it matters.
  `LiveNodeBuilder::build` calls `runner.bind_senders()` and then every
  `ExecutionClientFactory::create` in the same function on the same thread;
  `LiveNode::build` does the same. So our factory resolves it and installs it
  before returning the client, and #4874's shared slot carries it to every later
  clone. Landed. Upstream also corrected their adapter guide, which had told
  adapters to install in `start()` - the exact convention that failed here - in
  nautechsystems/nautilus_trader#4906, and fixed the "global" wording on the
  sender accessors to say thread-local.

  Deliberately not done in `MogwaiExecutionClient::new`, though it would have
  been less code. The last runner to bind a thread wins the slot, so a public
  constructor reading it captures whichever runner is bound at that instant; a
  client later run under a second runner would pass every sender-installed check
  and deliver its entire stream to the first. That trades a loud total loss for
  silent cross-runner misrouting, and only the factory has upstream's proof of
  which runner owns the thread.

  The deaf-client refusal stays, narrowed to what it now guards: a client built
  outside a node that found no sink at construction, at `start()`, or at
  `connect()`.

  The stale comment at the guard is fixed. The sink-loss question it raised is
  answered and built: a retained sender clone is a generation-scoped liveness
  witness, checked after every emission, and closure retires the transport
  generation rather than the client. `try_send_order_event` was considered and
  not adopted - it answers whether one enqueue saw a live receiver, which is not
  the terminal question, and at 0.63 it had no usable sibling for account state,
  since `try_send_account_state` was private and `try_emit_account_state`
  regenerates the event off the emitter's realtime clock and would discard this
  venue's simulated `ts_init`.

  The one-line interface request that trailed this entry, making
  `try_send_account_state` public, landed upstream in #4907 and ships at 0.64.
  It is the cleaner per-event interface for an already-built `AccountState` -
  fallible dispatch that keeps this venue's simulated stamp - and would remove
  the ambiguity about whether one particular account enqueue failed. Not
  required, since the witness answers the generation-liveness question without
  it, so adopting it is an option rather than a blocker.

  The original entry, kept for provenance: the emitter derived `Clone` and owned
  `sender: Option<UnboundedSender<ExecutionEvent>>` by value, installed once from
  `try_get_exec_event_sender()`, which reads a `thread_local!` in
  `nautilus_common::live::runner` set on the runner's thread. Every clone taken
  after that point freezes the sender state of the instant it was taken, and
  `send_order_event` on a sender-less clone only logs a warning. Our workaround
  is a refusal, not a repair: a host that starts its clients on one thread and
  connects them on another gets a named error rather than a working client.
  The PR asked for an emitter holding its sender behind a shared cell, or
  resolving it per send from a process-wide rather than thread-local slot, so a
  clone taken before `set_sender` still emits. The first of those is what landed.

- **No channel for a declared feed gap.** `VenueMessage::FeedLagged` carries
  `skipped` and `sim_now_ns` and the adapter has nowhere to put it. No
  `DataEvent` variant means "the stream you are aggregating has a hole", the
  client is handed to the host boxed as `dyn DataClient` so an adapter-owned
  counter or health accessor is unreachable, and `is_connected` is true
  throughout because the socket never broke. So bar aggregation over the
  missing span is silently wrong and the polling cursor resumes past it, and a
  strategy cannot distinguish a quiet market from a dropped one. Fabricating a
  report from the local mirror is not the escape: the mirror is built from the
  frames the venue just said it dropped. The execution socket cannot self-heal
  either: the frame translator that sees `FeedLagged` runs as `handler(msg).await`
  inside the reader's own frame loop, so a venue-truth query issued there
  deadlocks by construction, and the client is `!Send` so spawning it off is
  unavailable. The PR: a data-side degradation signal and a client-initiated
  reconciliation request. Until then, a host driving mogwai should treat an
  error from `mogwai-adapter` mentioning a feed gap or a refused frame as a
  reconcile-and-distrust-the-window signal.

  Still open at the 0.64 pin: `DataEvent` carries `Response`, `Data`,
  `Instrument`, `FundingRate`, `InstrumentStatus`, `OptionGreeks` and a
  `defi`-gated variant, and none of them means a hole in the stream - the
  enumeration in `client/data.rs`'s gap comment matches the pinned source
  exactly. `SystemEvent::SocketState` and 0.64's new
  `SystemCommand::ReconnectSocket` sit beside it, and neither helps: both are
  about the socket itself, and this gap happens while the socket never breaks.

- **No registration signal at the account cache insertion boundary.**
  `await_account_registered` polls every 10 ms until nautilus's runner has
  consumed the forwarded account event and inserted the row, with a 5 s wall
  bound. The pinned cache exposes no registration notification, and notifying
  when the adapter forwards the event would be too early, because forwarding only
  queues it. The PR: a signal at the cache insertion boundary. No adapter-side
  latch can substitute.

  Still open at the 0.64 pin: `Cache::add_account` writes the database, inserts
  into `accounts` and indexes `venue_account`, then returns. There is no notify,
  no watch and no subscriber hook on that path, so a waiter has nothing to sleep
  on.

  The connection half of this is already closed and should not be re-filed:
  `wait_connected` sleeps on an adapter-owned notification with a 250 ms backstop
  re-read, and that backstop is not a leftover poll - bite-checking the
  notification by deleting `notify_waiters` hung every socket test for the full
  dial timeout rather than failing on anything that named the cause, so a latch
  with one publisher and no fallback was trading five hundred cheap wakeups for a
  wedge.

- ~~**The Rust trait default for mass status does not compose** the way the
  Python base does.~~ **Closed upstream, verified at the pin 2026-08-27.**
  `ExecutionClient::generate_mass_status` in `common/src/clients/execution.rs`
  now carries a default that builds the three granular commands and composes
  their generators under `futures::try_join!`, with tests pinning both the
  composition and the error propagation. It ships with a stated caveat rather
  than silently: the default reads the realtime atomic clock, so a client on a
  mocked or backtest clock must still override to compose with its own. That
  does not reach us - mogwai overrides the method anyway, which is why this
  entry always said it protects the next adapter author rather than this repo.
  At 0.64 the default delegates to a public free `generate_mass_status` taking
  the caller's `ts_init`, so our override could compose through it on the
  simulated clock if that is ever wanted. Nothing to file, nothing to wait for.

The first three below came from the 2026-08-14 product-type plan and are ordered
by leverage - a fourth, the cash-account guard over dated futures, closed
upstream at 0.64 - and the last was filed later, from the 0.63 sweep. They are
what separates mogwai modelling a product from mogwai faking it the way every
other adapter fakes it. `reference/nautilus.md` carries the mechanism each one
names.

- **Lift `FundingSettlement` onto the live path.** The types exist, the
  semantics exist including rollback, and they are wired only into the backtest.
  Every shipped perp adapter currently launders funding through an unattributed
  balance delta. The highest-value change in this list: it is additive, its
  shape is obvious - an `ExecutionEvent` variant plus a live emitter method -
  and mogwai emitting it correctly is what makes the gap visible.

- **Make expiry act on the live path.** `InstrumentClose` carrying a
  contract-expired reason already arrives; the machinery that cancels orders and
  closes positions is matching-engine-only. Dated futures are not honestly
  forward-testable until this exists. At 0.64 the cache and database store an
  `InstrumentClose`, and still nothing acts on one.

- **Corporate actions.** Genuinely new - no dividend or split type exists
  anywhere. Splits are the hard half, because they rewrite an open position's
  quantity and average price retroactively. Only needed when equities become
  real.

- **`Equity` cannot express a fractional share.** Filed 2026-09-02, from the
  0.63 sweep of `reference/nautilus.md`, which recorded the constraint without
  anyone asking for it. The `Instrument` impl in `model/src/instruments/equity.rs`
  hardcodes `size_precision` to 0, `size_increment` to 1 and `multiplier` to 1 as
  trait method bodies rather than struct fields, so there is no declaration a
  venue can make that admits a fractional quantity. Unlike the `forex` case this
  has no named refusal on our side: `convert::instrument_any` publishes the
  equity, and a venue serving fractional lots would have its sizes silently
  rounded to whole shares by nautilus's own quantity handling rather than
  rejected.

  The PR: carry size precision, size increment and multiplier as fields on
  `Equity` the way `CurrencyPair` does, defaulting to today's 0, 1 and 1 so every
  existing declaration keeps its behaviour. An info-bag workaround does not close
  it, for the same reason it does not close `forex` - nautilus computes notional
  and normalizes quantity itself, so a preserved precision in `Params` would sit
  beside a wrong number rather than correct it.

  Not urgent, and honestly stated as such: no shipped preset is a fractional-lot
  equity, NVDA is whole-share, and this only bites when equities become real. It
  is filed because the constraint is verified and the fix is small and additive,
  which makes it writable now rather than rediscovered later.

- **Tape sparsity has no attribution channel.** An empty historical window is
  correct behaviour here - the fitted ACD arrival process is persistent and
  heavy-tailed, so a short window can legitimately hold zero trades and `/trades`
  correctly answers `200 []` - but it still costs the consumer a fatal halt, and
  one of the two fixes is blocked on the same gap as `FeedLagged`: an empty
  historical response carries no feed identity, so it cannot be attributed.
