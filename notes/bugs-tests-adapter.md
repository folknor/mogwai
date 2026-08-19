# Test hunt: mogwai-adapter socket suites

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-adapter/tests` (the four socket-backed binaries and their shared
`common/` harness) plus the crate's own `#[cfg(test)]` modules.

This hunt looks for defects in the TESTS, not in the code they test: tests that
do not survive parallel execution, tests that wait on fixed durations rather than
conditions, tests that assume they are the only test in the process, tests that
cannot fail, fixtures that cannot represent their shape, and anything else weird.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

The hunter reports running nothing, and verified its two load-bearing claims
against source: `replace_data_event_sender` is a `thread_local` in
`research/nautilus_trader/crates/common/src/live/runner.rs`, and the 100 ms
pre-push sleep is unconditional in `serve_ws` before `ws_trades` is drained.

## Fixed durations on the success path (the parked-completion-test family)

- `common::serve_ws` line ~484: `sleep(100ms)` with the comment "give the client
  a scheduling turn ... to record its local Nautilus subscription before the
  first tape frame arrives." THIS IS THE FAMILY'S CANONICAL SHAPE, sitting in the
  shared harness and therefore under every data test in three binaries. It is a
  bet that connect plus subscribe finishes in 100 ms of wall time; under
  `--test-threads=8` on a loaded box that bet can lose, and when it loses the
  failure is "expected a trade data event, got Instrument" or a timeout - a wrong
  answer, not a clean timeout. It also adds a flat 100 ms to every socket test.
  The fix is a condition, not a longer sleep: have the stub wait for the client
  to signal readiness, or (better) accept that the tests must tolerate frames
  arriving before the subscription by having the client buffer, or drive the push
  explicitly from the test after subscribe (e.g. a `tokio::sync::Notify` in
  `StubState` the test fires).
- `havoc::dialing_blind_establishes_a_full_session_with_a_stranger`:
  `sleep(600ms)` then `assert!(!client.is_disconnected())`. Should poll for
  `ws_requests` non-empty / connected, with 600 ms as the failure deadline.
- `havoc::an_unanswerable_identity_probe_does_not_refuse`: same 600 ms sleep,
  same fix.
- `havoc::conn_reconnect_respects_max_attempts`: `sleep(300ms)` "to give any
  erroneous extra dial time to land". Justified as a NEGATIVE window, but note
  the `(3..=4)` tolerance already admits the timing is loose; a 4th dial arriving
  at 310 ms passes.
- `data_client_transport::a_host_subscribing_quotes_after_connect_receives_the_book_immediately`:
  `sleep(50ms)` after connect before subscribing, to let the cached quote land.
  That 50 ms is racing the stub's own 100 ms sleep - it is *always* too short,
  and the test only passes because the client caches the quote whenever it
  arrives. Harmless today, meaningless as written; drop it or make it a
  condition.

## Wall-clock upper bounds that will bite under parallelism

- `lifecycle.rs::reconnect_backoff_throttles_accept_then_die_and_trips_attempt_cap`:
  `(200ms..300ms).contains(&elapsed)` across three real TCP dials plus three WS
  handshakes, dev profile. 100 ms of headroom for three round trips and three
  task spawns. This is the tightest budget in the crate and the most likely first
  flake. The INTENT is "three dials span two backoffs, not three" - assert that
  structurally (count the backoffs, or assert `elapsed < 250ms` with a 100 ms
  initial and drop the lower bound, which is already implied by `dials == 3`).
- `clock.rs::alert_timer_fires_with_sim_event_timestamp`: the poll loop tolerates
  200 ms, then the test asserts `started.elapsed() < 50ms`. Two contradictory
  budgets; the 50 ms one is the real gate and it is a 2 ms-wall timer measured in
  a dev build under 8-way load. Not `#[ignore]`d - this runs in every
  `brokkr check`.
- `client/shared.rs::latency_pump_pipelines_a_burst_instead_of_serializing`:
  `elapsed < per_msg * 3` where `per_msg` is the default baseline (~30 ms) ->
  ~90 ms for 40 messages through an unbounded channel. Probably fine, but it is a
  wall budget in a non-ignored test; the property (pipelined, not serialized)
  would be more honestly asserted as `elapsed < per_msg * (N/4)` or similar, far
  from the boundary.
- `havoc::havoc_reaches_the_order_a_trigger_produces`: upper bound `< 3s` on a
  clock started *before* `connect()`, with a 400 ms hold and a 4 s data bucket as
  the discriminator. The comment acknowledges the clock placement. It is the
  widest gap of the timing tests, but it is also the slowest test in the slowest
  binary (~1.5-2 s minimum) and therefore sets `havoc`'s floor.
- `havoc::havoc_reaches_the_order...` lower bound `triggered_at >= 400ms` is
  measured from before connect, so ~100 ms of stub sleep plus setup already
  contributes; it would still fail with latency off, but the margin is not what
  it reads as.

## Harness defects

### `common::read_request` cannot handle a segmented request head

It does one `read()` into 4096 bytes and returns `None` if `\r\n\r\n` is not in
it - dropping the connection silently. The body loop exists, the header loop does
not. On loopback with small requests this practically always works, but a WS
upgrade split across two segments (or a header set over 4096 B, which a future
`Authorization`/cookie would produce) turns into an unexplained connect failure.
Loop the header read too.

### The `ClientMessage::ModifyOrder` arm in `serve_ws` carries a large block of dead code

Inside it: the `close_after_trades` re-serve guard, the `dark_ms` sleep, the
server-ping probe, and the close-and-return. All four are also implemented at the
top of `serve_ws` (post-handshake), which is the path every data test actually
takes since `Subscribe` stopped being a wire frame. No test sends a `ModifyOrder`
AND sets `dark_ms`/`close_after_trades`/`ws_server_pings`, so that whole block is
unreachable - and it is written as though it were the data path, which is
actively misleading to the next reader. `served_once` is only touched there, so
it is dead too. Rip it out; the `ModifyOrder` arm should push `ws_modify_frames`
and nothing else.

### `StubState` is one flat god-object with 30 fields, ~4 of which any given test sets

It is why the dead branch above survived: nothing localizes which fields belong
to which leg. Given pre-1.0 latitude, the hunter would split it into `HttpStub`
(instruments/trades/quotes/account/clock/control plus the tapes and cursors) and
`WsStub` (frames, counters, refusal/close/dark switches), with the data-leg and
exec-leg WS behaviours as two separate handlers rather than one function
branching on a parsed message. The exec leg's semantics (reply to
submit/modify/query) and the data leg's (push tape at upgrade) share nothing but
the handshake.

### The negative-assertion windows are sound, and should stay that way

`assert_no_exec_event` (250 ms) and the several `timeout(250ms/400ms,
rx.recv())` tail drains are fine as failure paths - they are negative assertions
with a bounded window. Note the sibling-project finding: a 250 ms window on a
SHARED channel would be leaky. Here each test owns its own thread-local sink, so
they are sound. Do not consolidate these onto a shared sender.

## Process-global state

`process_session_id()`'s `OnceLock` is genuinely process-wide, and every exec
client built through `MogwaiExecClientConfig::default()` (i.e. all of
`adapter_smoke` and `reconciliation`) presents the identical session string. This
is harmless today only because every test binds its own stub, so no two clients
ever meet on one venue. Two consequences:

- A SESSION-EVICTION TEST CANNOT BE WRITTEN THE OBVIOUS WAY in these binaries.
  Any test that wants two DISTINCT clients on one venue must set `session:`
  explicitly (as `havoc` does with `session: None`), and any test wanting "same
  process, two legs, no eviction" is testing a constant.
- THERE IS NO TEST AT ALL FOR THE `session=` QUERY PARAMETER.
  `StubState::ws_requests` records the full request line specifically so
  disclosure is assertable, and exactly one test uses it - for `account=`.
  Nothing asserts `session=` is present, well-formed, or shared between a
  process's data and exec legs, which is the ENTIRE stated reason the `OnceLock`
  exists ("without a shared identity the second dial would evict the first and
  the host would disconnect itself before it ever traded"). Delete
  `default_session` and no adapter test goes red. That is the largest coverage
  hole the hunter found.

`replace_data_event_sender` / `replace_exec_event_sender` are thread-locals
(`DATA_EVENT_SENDER.with(...)` in nautilus `common/src/live/runner.rs`), and each
`#[tokio::test]` gets its own libtest thread, so these are safe under
`--test-threads=8`. Worth stating because the module doc calls them "global" and
the naming invites the opposite conclusion. No global loggers, capture buffers,
env vars, or fixed ports anywhere in scope - all listeners are `127.0.0.1:0`.

## Binary-level timing

Cargo runs the four binaries sequentially, so each binary's wall time is its own
longest test. Distribution is lopsided:

- `havoc` (17 tests) holds essentially all the slow ones: two 600 ms sleeps, a
  300 ms sleep, a 400 ms dark window, two 2 s `wait_for_at_least` calls, and the
  ~2 s trigger-latency test. Floor about 2 s, total about 6-8 s serial content.
- `reconciliation` (14 tests) is all fast request/response - floor about 300 ms.
- `adapter_smoke` (10) and `data_client_transport` (12) are fast except the two
  `MAX_HISTORY_LIMIT` paging tests, which build large tapes and serialize them
  through the stub twice.

Nothing is near the 20 s watchdog. But if the 100 ms harness sleep is removed,
roughly 40 tests get 100 ms faster each - the single largest cheap win available
here.

## Smaller notes

- `data_client_transport::trade_history_pages_without_duplicates_at_the_seam`
  asserts `trades_hits == 2` INSIDE the response loop while the `/trades` handler
  increments that counter from a different task; it is ordered correctly (the
  response can only exist after both fetches) but the coupling is implicit.
- `request_whole_tape` builds a NEW stub per call while taking
  `&Arc<StubState>` - so `trades_hits`/`trades_starts` accumulate across any
  second call on the same state. Only used once per test today; a landmine if
  anyone calls it twice.
- `next_exec_event`'s panic message ("execution event arrives") names neither the
  test's expectation nor the elapsed time, so a timeout in any of the ~20 call
  sites reads identically.
- `adapter_smoke::a_submitted_position_id_reaches_the_wire` reads
  `ws_client_messages` after draining two events rather than draining to a
  deadline like its sibling `an_order_list_reaches_the_wire_as_linked_legs` does.
  It happens to be safe (the events imply the submit crossed), but the two tests
  use different disciplines for the same question.
