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
pre-push sleep was unconditional in `serve_ws` before `ws_trades` was drained.
That sleep is gone as of round 2; `common::PushGate` stands where it was.

## Fixed durations on the success path (the parked-completion-test family)

CLOSED IN ROUND 2. All five bullets fixed; see `notes/bug-loop-carry-forward.md`
for the machinery (`common::PushGate`) later rounds must not break, and for the
one residual the fix could not close (the client half of the cached-quote
ordering is not observable from a test).

## Wall-clock upper bounds that will bite under parallelism

CLOSED IN ROUND 3. All five bullets settled, four by widening the SIGNAL rather
than the assertion and one by re-anchoring the clock; the fifth finding was
confirmed WORSE than reported (the lower bound could not fail at all). See
`notes/bug-loop-carry-forward.md` for the measured distributions, the new
`StubState::ws_first_exec_frame_at` anchor later rounds should reach for, and
the one thing this test set still cannot see.

## Harness defects

CLOSED IN ROUND 4. The segmented-head defect and the dead `ModifyOrder` block
are fixed, along with two defects the deletion exposed: a stub re-serve/close
loop running under a passing test, and a dead `hang_orders` field left by the
retired HTTP order carrier. The `HttpStub`/`WsStub` struct split is REFUSED ON
EVIDENCE and the part of the proposal with real content was done instead. See
`notes/bug-loop-carry-forward.md` for the facts round 5 would otherwise
re-derive wrong: that the two WS legs are INDISTINGUISHABLE at the handshake,
that the proposed split's axis does not separate the fields the defect
involved, and what `close_after_trades` now means. The negative-assertion
bullet below is a do-not-break item and stays.

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
  ROUNDS 1-3 HAVE INVALIDATED THIS BULLET'S FIGURES, and round 5 owns correcting
  it rather than trusting it: `havoc` holds 19 tests, not 17; the 100 ms pre-push
  sleep the section's last paragraph costs out is GONE, replaced by
  `common::PushGate`; and the trigger-latency test measures 0.94 s, not the
  1.5-2 s written here. The dead-socket test round 1 added can spend up to 3 s in
  `wait_for_at_least`, so the "floor about 2 s" is the more misleading of the two
  numbers left. Re-measure before quoting any of it.
- `reconciliation` (14 tests) is all fast request/response - floor about 300 ms.
- `adapter_smoke` (10) and `data_client_transport` (12) are fast except the two
  `MAX_HISTORY_LIMIT` paging tests, which build large tapes and serialize them
  through the stub twice.

Nothing is near the 20 s watchdog. But if the 100 ms harness sleep is removed,
roughly 40 tests get 100 ms faster each - the single largest cheap win available
here.

ROUND 2 MEASURED IT AND THE ESTIMATE HELD, but it is a tenth of the wall, not
the wall: `brokkr test -p mogwai-adapter "" --debug` went 42.46 s to 37.47 s per
sweep, and 37.61 s after the round's own review fixes. About 4 s of that is the
harness sleep (it ran on every `/ws` upgrade, including the exec legs that seed
no tape, which is why the count is ~40 sockets rather than the ~13 tests that
seed frames) and about 1 s is `conn_reconnect_respects_max_attempts` no longer
spending a 2 s connect bound on its passing path.

THE REMAINING ~37 s IS UNEXPLAINED, is not the sum of these tests' asserted
waits, and is now the largest single cost in the crate - larger than everything
this document's timing sections propose to save put together. NOBODY HAS
MEASURED WHERE IT GOES. That is the next timing pass's first job, before any
further structural-win claim is made about this crate: get a per-test
distribution first, then decide whether there is anything worth fixing.
Round 2 deliberately did not chase it.

## Assertions that cannot fail

FOUND IN ROUND 2 WHILE FIXING THE SAME SHAPE ELSEWHERE, not by the original
hunt, and left for a later round rather than swept in under a review fix.

- `havoc::a_venue_serving_another_run_is_refused_terminally` asserts
  `client.is_disconnected()` after a connect that was refused. That flag is
  `!connected`, it starts FALSE, and `lifecycle` stores `false` on every failed
  dial - so it is true from the first instant of the test and the assertion
  passes whatever the client does. The same expression was removed twice from
  `conn_reconnect_respects_max_attempts` in round 2 for exactly this reason.
  The test is NOT vacuous overall: its `handshakes <= 2` bound is real, and it
  is the assertion carrying the property. Either delete the dead line or
  replace it with something the client actually writes on the refusal path.
- SWEEP FOR THE WHOLE CLASS while you are there: every `is_disconnected()` in
  these binaries is meaningful only after a connect that SUCCEEDED, and even
  then only as a window rather than a snapshot (see `dialing_blind...`).

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
