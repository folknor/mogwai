# Bug hunt: mogwai-adapter

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-adapter`: the factories and their configs, the DataClient and
ExecutionClient pair, the socket upgrade and reconnection path, order and
order-group submission, history pagination, reconciliation, and the four
socket-backed test binaries.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter read the full crate plus the nautilus emitter source in `research/`,
and skimmed the four socket test binaries. Nothing was built or run.

## 1. The exec pump can hold an emitter with no sender: every venue-pushed order event silently dropped (high)

`MogwaiExecutionClient::start()` (exec.rs ~859) is the only place
`self.emitter.set_sender(..)` happens. `connect()` builds
`pump_ctx = self.exec_context()`, which CLONES the emitter
(`ExecContext.emitter: ExecutionEventEmitter`, a `#[derive(Clone)]` struct
owning `sender: Option<...>` by value - see
`research/nautilus_trader/crates/live/src/execution/emitter.rs:60-92`). The
clone freezes whatever sender state existed at `connect()` time.

- `connect()`'s own doc comment explicitly says "hosts may connect without
  calling start, and reconnect after stop". If a host does that, the pump's
  emitter has `sender: None` for the whole life of the connection, and
  `send_order_event` just does
  `log::warn!("Cannot send order event: sender not initialized")` - no error, no
  return value. Fills, accepts, cancels, rejects all vanish while `submit_order`
  (which uses the live `self.emitter`) keeps working. The asymmetry is the worst
  part: nautilus sees Submitted and nothing else, forever.
- `start()` also does `if let Some(sender) = try_get_exec_event_sender()` - a
  `None` there is swallowed entirely. There is no assertion anywhere that the
  emitter is initialized before `connect()` returns;
  `ExecutionEventEmitter::is_initialized()` exists and is never called.
- All four socket test binaries call `client.start()` before `connect()`
  (`tests/common/mod.rs:732`), so this ordering is untested by construction.

Fix direction: don't clone the emitter into `ExecContext` at all. Share it
(`Arc<Mutex<..>>` or resolve the sender lazily per send), or make `connect()`
hard-fail when `self.emitter.is_initialized()` is false. The hunter would take
the second as a minimum and the first as the right shape - the emitter-by-value
clone is the same "snapshot of mutable state captured into a long-lived task"
pattern that `retire_connected_flag` exists to fight, just unrecognised here.

## 2. `submit_order_list` can announce half a bracket and dispatch none (high)

exec.rs:1154-1202. The conversion loop is correctly all-or-nothing before
anything is dispatched, and the doc comment says exactly that. But the SECOND
loop calls `self.announce_submitted(..)?` per leg, and `announce_submitted` is
fallible (`self.core.get_order(client_order_id)?`, plus the mutex-poison arm).
If leg 3 fails, legs 1 and 2 have already had `OrderSubmitted` emitted and
mirror records inserted, and the `?` returns before `dispatch_order` - so no
`SubmitGroup` frame goes out and no reject is synthesized for the legs already
announced. Those orders sit `Submitted` in both nautilus and the mirror forever,
exactly the "half a bracket is worse than none" outcome the doc claims to
prevent.

Fix: build all `OrderSubmitted` events first (or pre-resolve every `get_order`),
and only then emit and dispatch; or synthesize `OrderRejected` for the
already-announced prefix on the error path, the way
`synthesize_transport_reject`'s `SubmitGroup` arm already does.

## 3. `subscribe_bars` leaks a bar ref when the bound-symbol check refuses (medium)

data.rs:606-626. `bars.entry(cmd.bar_type).or_default().refs += 1` happens
BEFORE `self.subscribe_symbol(..)`, which can return `Err` on the `config.symbol`
mismatch (`ensure!` at data.rs:154-159). The per-`BarType` ref is now
incremented while the per-symbol `SubState.bars` count is not.

This is precisely the cross-counter desync AD10's comment in `unsubscribe_bars`
describes, arriving from the subscribe side that the AD10 fix did not cover: a
later `unsubscribe_bars` for that bar type finds `refs > 0`, so
`matched == true`, and it decrements `SubState.bars` - a decrement belonging to
a DIFFERENT bar type's live subscription. Drop the shared count to zero and the
surviving feed stops forwarding bars.

Fix: increment `refs` only after `subscribe_symbol` succeeds, or roll it back on
error.

## 4. The subscription resume cursor is entirely dead state, and its documentation asserts a contract nothing keeps (medium, structural)

`SubState.start_ts` is written in three places (`subscribe_symbol` seeding,
`advance_sub_start_ts` on every delivered trade) and READ IN ZERO. The hunter
grepped the whole crate: no reader.

The surrounding prose is confidently wrong about live behaviour:

- data.rs:1381 - "On the WS path a reconnect re-issues `Subscribe { start_ts }`".
  It does not: `connect()` passes `Vec::new` as the `on_connect` reattach hook
  and `None` as the command receiver (data.rs:467-473), and the crate's own
  comment three lines below says "a reattach has no subscribe frames to replay".
- data.rs:205 - "The seeded `start_ts` survives as the resume cursor the
  historical request paths read." No historical request path reads it;
  `request_trades`, `request_bars` and `request_quotes` take their `start` from
  `request.start` only.

So `advance_sub_start_ts` runs a mutex acquisition per delivered trade on the
hot data path to maintain a value nobody consumes, and AD7's fix-comment
documents a hazard that can no longer exist. Given the local-only subscription
model, the hunter would delete `start_ts`, `advance_sub_start_ts`,
`start_ts_param` and the `start_ts` plumbing through `subscribe_symbol` and
`subscribe_quotes_inner` outright rather than preserve it. If a resume cursor is
genuinely wanted for a future reattach, it should be reintroduced with a reader
in the same change.

Note also (minor, and moot once the above is deleted): `advance_sub_start_ts` is
called at data.rs:1388 BEFORE `convert::trade_tick` may fail and drop the tick -
a watermark advancing over work whose success the same expression did not check,
the frontier family verbatim.

## 5. A command accepted by `send_command` is not a command that reached the venue (medium)

lifecycle.rs. `send_command` pushes onto the unbounded `out_tx` and returns
`Ok`; a separate writer task drains it into the socket. On disconnect the inner
loop breaks and `writer_handle.abort_and_join()` kills the writer with whatever
is still queued. Anything in flight - a submit, a cancel, a modify, a
`QueryOrders` - is dropped with no error and no synthesized reject.

`dispatch_order` only synthesizes a reject when `tx.send(cmd)` on the CHANNEL
fails, i.e. when `ws_cmd` is gone. It cannot see this case. An order submitted
in the millisecond before a socket drop reaches nautilus as `Submitted`, never
reaches the venue, and never gets a terminal event - the same wedge AE9 fixed
for the channel-closed case, still open for the writer-aborted case. Same for
`VenueQuery`: the waiter is registered, the send "succeeds", and the requester
waits out its full timeout.

This one is inherent to the enqueue-then-forget writer split. The honest fixes
are either to have the writer own the pending frames and re-queue unsent ones
onto the next generation's `out_tx` (the reattach hook is the natural place), or
to drain `out_rx` on teardown and report each undelivered command back to the
client so it can synthesize the reject. The hunter would take the second -
replaying orders across a reconnect is a policy call the adapter shouldn't make
silently, but TELLING the caller is not optional.

## 6. `Close(Normal)` is read as run completion and permanently disables reconnect (medium, half-confident)

lifecycle.rs:534-538: any close frame with code 1000 sets `run_complete = true`
and returns from `run_ws_connection` for good. `RunComplete` the MESSAGE is the
real terminal signal; the 1000 close is documented as "its socket-level fallback
for a reader that loses the final text frame while the server drains". But 1000
is also the ordinary, correct code for any graceful close - a venue restarting,
a proxy closing an idle socket, and (per `config.rs`'s own eviction discussion)
the venue evicting a socket. Any of those kills the client's transport
permanently with an INFO line reading "venue run completed". The hunter is not
certain the server ever closes 1000 for a non-completion reason - that is
another hunter's scope - but the adapter is trusting a two-byte code to carry a
semantic the protocol has a dedicated message for. A close reason string, or
requiring the `RunComplete` message, would remove the ambiguity.

## 7. Smaller things

- Account watermark advances over a snapshot that produced nothing.
  `handle_account_state` (exec.rs:2911-2929) sets `mirror.account_ts_last =
  ts_event` unconditionally, after the balance and margin loops may have dropped
  every row (unknown currency, `locked + free != total`). A subsequent OLDER but
  well-formed snapshot is then refused as stale, and the account row keeps the
  degraded state. Frontier family again, mild version.
- Group attribution is keyed off leg 0 only. `submit_order_list`
  (exec.rs:1195-1200) reads `orders.first().and_then(|o| o.link...)`. A list
  whose first leg carries no link but whose later legs do is never remembered,
  so an `AdmissionSubject::SubmitGroup` refusal degrades to the
  `tracing::error!` "cannot attribute" path and no leg gets a rejection. Also
  `take_group` REMOVES the entry, so a duplicated `AdmissionRejected` (which
  `duplicate_prob` deliberately produces) hits the same unattributable path.
- `stop()` does not clear `ExecState.groups`. `reset()` does (via
  `ExecState::default()`), `stop()` does not. Stale group rows survive a stop
  and connect cycle and can be attributed to a refusal from the NEXT generation
  if a list id repeats.
- `ExecState::prune` cannot bound a mirror full of open orders. The
  `orders.len() <= MAX_TERMINAL_ORDERS` gate returns early, and the
  terminal-only prune means a run that accumulates more than 10k open or
  permanently-`Submitted` records grows without bound. The AE6 comment claims
  the map is bounded; it is bounded only in the terminal-heavy case. Finding 5
  above is one way `Submitted` strays get created.
- `fetch_quotes_windowed` returns `truncated: false` when a short page arrives,
  but `fetch_trades_windowed` returns `stop(&out)`. Not a bug today (the quote
  path has no stop closure), but the two paginators are near-duplicates that
  have already drifted in their truncation semantics. They should be one generic
  function over `ts_of` - `final_ts_group_start` is already extracted; the loop
  is not.
- `await_account_registered` polls the cache on a 10 ms sleep for up to 5 s
  inside `connect()`, and `wait_connected` does the same for the socket flag.
  Both are busy-wait shims around state that could carry a notifier. Minor, but
  it is roughly 500 wakeups on a slow boot and it is the kind of thing that
  makes `connect()` latency unpredictable under a scaled sim clock (neither is
  sim-scaled, unlike everything else in the lifecycle).

## Relayed from broadarrow, 2026-08-18 - both FIXED, both documentation

Not this hunter's findings. broadarrow filed them in its own `notes/todo.md`
after consuming the "be an exchange" landing, and both are cases of durable
prose stating something the code does not do.

- `docs/havoc.md` told a client wanting to tell a quiet feed from a lossy one to
  read `FeedLagged` and its skipped count. A nautilus host cannot, and this
  crate's own data-message arm says so at length: `DataEvent` carries no gap
  variant, the client reaches the host as a `dyn DataClient`, and the ERROR log
  line is the only channel the signal has. So the two sites disagreed and the
  adapter was the one telling the truth. The doc now splits the raw-protocol
  client from the nautilus one and names the log as the nautilus channel.
- `process_session_id`'s doc comment derived the id from "the process's start
  instant"; the `OnceLock` captures the wall clock at first call, which is the
  first client this process builds. The collision argument is unaffected - a
  reused pid still arrives with a later instant - so this was prose only, but the
  argument should rest on what the code does.

## What the hunter checked and found sound

The venue-truth report generators (the argument for querying the venue rather
than the mirror is correct and consistently applied), the terminal-state guards
and forward-only `ts_last` discipline across every `handle_exec_message` arm,
`final_ts_group_start`'s timestamp-cursor rule (the `AGENTS.md` cursor invariant
genuinely holds on both paginators), the `retire_connected_flag` Arc-swap, the
drop-and-warn discipline in `convert.rs` (every panicking nautilus constructor
is routed through a `*_checked` twin - checked for `Price`, `Quantity`, `Money`,
`TradeId`, `ClientOrderId`, `VenueOrderId`, `Symbol`, `AccountBalance`,
`MarginBalance`, `TradeTick`, `QuoteTick`), and the identity-check three-outcome
classification.
