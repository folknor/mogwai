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

Round 1 closed findings 1, 2 and 3: all three were real defects, all three are
fixed, each with a bite-checked regression test. They are removed rather than
annotated, per the arc's convention. Numbering is unchanged. Two things the
round learned that the removed text would otherwise have carried:

- The hunter's claim that finding 1's ordering was "untested by construction"
  because every socket binary starts before connecting was HALF right. The
  ordering is untested, but `havoc.rs`'s `ships_server_havoc` installed no
  execution sink at all, so its client's `start()` found nothing on the
  thread-local and it connected DEAF - the new guard failed it on the first
  run. The test now installs a sink. That is the defect the finding describes,
  standing live in the crate's own suite.
- The fix is the REFUSAL, not the shared emitter. `try_get_exec_event_sender`
  reads a `thread_local!` set on the runner's thread, so resolving the sender
  lazily from `exec_context()` - the hunter's preferred shape - is not reliably
  possible from a spawned task. `connect()` retries the lookup (free when it
  really is on that thread) and then refuses. The nautilus-side fix would be an
  emitter that shares its sender; `research/` is read-only, so this is the
  local fail-loud mitigation. Filed in `notes/todo.md`; the HOST-FACING
  contract it creates is durable and now lives in `docs/adapter-lifecycle.md`,
  because a refusal a shipped consumer can hit is documentation, not a note.
- THE COLD REVIEW'S HEADLINE FINDING WAS EMPIRICALLY REFUTED, AND THE MECHANISM
  IS WORTH KEEPING. The reviewer argued that
  `connect_refuses_a_client_with_no_execution_event_sink` cannot pass under
  `brokkr test`, on the model that libtest at `--test-threads=1` runs tests
  INLINE on the main thread, which would share one `EXEC_EVENT_SENDER` across a
  whole binary and let an alphabetically-earlier test install a sender this one
  would then find. THE MODEL IS WRONG. libtest spawns a fresh named thread per
  test unconditionally on any threaded target - the thread name is how a panic
  is attributed to a test - and `--test-threads` caps how many run AT ONCE, not
  whether a thread is created. Measured, not argued: an `eprintln!` probe under
  `brokkr test -p mogwai-adapter "" --debug` reported distinct `ThreadId`s named
  for each test and an EMPTY sender slot on entry to both new tests. So the
  test's doc comment is literally true, and `tests/common/mod.rs`'s isolation
  claim - which every negative sink window in these binaries rests on - is
  sound. The claim now says so with the mechanism spelled out, and it is PINNED
  rather than asserted: `common::owns_a_fresh_exec_sink_on_every_lane` runs in
  all three binaries and in all lanes, and `assert_owns_a_fresh_exec_sink` lets
  an individual test restate the premise it depends on so a libtest change
  fails on the premise rather than on whatever the test was really asserting.
  Both new tests call it. The close pass bite-checked the refusal in the
  `brokkr test` lane specifically: with the `ensure!` removed as a text edit,
  the test goes red on the ERROR-TEXT assertion, `connect()` having failed with
  the transport error `fetch instruments` instead of the named refusal - which
  is the one assertion that distinguishes "refused for the right reason" from
  "failed for any reason", and no other site emits that string.

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
