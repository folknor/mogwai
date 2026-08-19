# Test hunt: mogwai-adapter socket suites

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-adapter/tests` (the four socket-backed binaries and their shared
`common/` harness) plus the crate's own `#[cfg(test)]` modules.

This hunt looks for defects in the TESTS, not in the code they test: tests that
do not survive parallel execution, tests that wait on fixed durations rather than
conditions, tests that assume they are the only test in the process, tests that
cannot fail, fixtures that cannot represent their shape, and anything else weird.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

THIS DOCUMENT IS CLOSED. Five rounds, no open findings left - every section
below records what was fixed or, in one case, a refusal with its measurement.
It is kept until the arc's close pass reads it.

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

CLOSED IN ROUND 5, and the coverage hole it named was real and is closed.
`adapter_smoke::both_legs_disclose_one_process_session_on_the_upgrade` builds
one process's exec leg and data leg against one stub, reads the two `/ws`
request lines off `ws_requests`, and asserts a session is PRESENT on both,
EQUAL across them, wire-legal by `mogwai_protocol::validate_session_id`, and
minted from this pid. Bite-checked by making `default_session` return `None`:
it fails naming the bare request line, at 0.94 s. Both consequences the report
drew - that a two-distinct-clients test must state `session:` explicitly, and
that a "two legs, no eviction" test asserts a constant unless it reads the wire
- are now written on `tests/common`'s module doc rather than living here.

The thread-local claim was verified and is TRUE: `DATA_EVENT_SENDER` and
`EXEC_EVENT_SENDER` are `thread_local!` in the pinned nautilus release's
`common/src/live/runner.rs`. The doc comments calling them "global" are
nautilus's own and `research/` is read-only, so the correction is stated on
this workspace's harness instead, next to the negative-assertion windows whose
soundness depends on it.

## Binary-level timing

CLOSED IN ROUND 5 BY MEASUREMENT, and the section's every figure was stale, so
it is replaced rather than corrected. THE ~37 s WAS NOT A DISTRIBUTION OF SLOW
TESTS AT ALL - it was a ~420 ms FLOOR under every test that calls `connect()`,
paid because the stub answered `GET /clock` with an undecodable `[]` and the
client retried three times with 200 ms wall sleeps before falling back. The
serial sweep is 12.1 s now, from 39.71 s, with no test removed and two added.
The instrument that found it (`scripts/adapter_test_walls.py`), the per-test
shape, and the second repair the clean distribution then exposed are recorded
durably in `reference/performance.md` under the 2026-08-19 entry, because a
measured number belongs there rather than in a transient note.

## Assertions that cannot fail

CLOSED IN ROUND 5. The class was swept across all four binaries and the crate's
`#[cfg(test)]` modules. THE INSTANCE THE REPORT NAMED IS REFUSED ON EVIDENCE
and two others were found and fixed:

- REFUSED: `havoc::a_venue_serving_another_run_is_refused_terminally`'s
  `is_disconnected()`. Round 2's reasoning does not transfer to this fixture.
  The flag is vacuous only where the stub REFUSES THE UPGRADE, because nothing
  can then set `connected` true; this stub serves a perfectly good websocket and
  the refusal happens after the dial succeeds, with `connected.store(true)` the
  very next statement on the non-refusing path. Bite-checked as a text edit -
  `IdentityOutcome::Mismatch` returning `Ok(())` fails exactly this line, and
  the `handshakes <= 2` bound does not move, so it is the ONLY assertion
  carrying the property. Deleting it would have unpinned the whole refusal. The
  reasoning is now written beside the assertion. The cold review then found a
  narrow vacuous-pass WINDOW in it - the `health_hits` gate moves when the stub
  SERVES the probe, before the client has classified the response and reached
  `connected.store(true)` - so it is held as a 250 ms window rather than
  snapshotted once. Two further couplings the same review named are now written
  down beside it: the 500 ms connect bound only has headroom BECAUSE the stub's
  clock default changed in the same commit, and cancelling `connect()`
  mid-flight skips its own `abort_tasks`/`retire_connected_flag` cleanup, which
  is safe here only because the identity mismatch is TERMINAL.
- FIXED: `reconciliation::mass_status_reports_all_three_sets_over_the_single_ws
  _transport` asserted `ws_hits >= 1`. `fixture()` connects and `expect`s it,
  and a connect that returns has completed an upgrade the stub counted, so the
  value is at least one before the test's first line. Deleted; the two query
  counters beside it are the real wire evidence.
- FIXED: `convert::tests::a_future_def_builds_a_futures_contract` asserted
  `!contract.expiration_ns.to_rfc3339().is_empty()`, which no value can
  falsify - a zeroed expiration renders as 1970 and passed. Replaced by the
  property it was reaching for: a mogwai future must not expire inside a run.
  Bite-checked by substituting `ts_init` for the sentinel.

- FIXED, AND FOUND BY THE ROUND'S OWN COLD REVIEW: the deliberate ladder test
  this round ADDED was itself half-vacuous, the eleventh instance of the arc's
  signature defect and introduced by the round whose subject was that defect.
  Its claim was that an UNKNOWN floor admits a window a known floor would
  refuse; it cannot discriminate, because the fallback yields `None`, the
  success path against the same stub yields `Some(0)`, and `ensure_on_tape`
  only bails on `start < data_origin` with `start` a `u64`. Confirmed by
  measurement, not argument: with `ensure_on_tape`'s comparison text-edited to
  `false`, the ladder test still PASSED. Only `clock_hits == 3` was live. The
  test is renamed
  `an_undecodable_clock_is_retried_then_falls_back_without_refusing` and now
  asserts only the ladder and the non-refusal.
- FIXED, EXPOSED BY THE SAME ANALYSIS: the pre-existing
  `off_tape_window_still_answers_the_request` was vacuous in the same way. It
  publishes a real nonzero floor and asserts the response is empty, but it left
  the stub's tape EMPTY, so a venue holding no rows answers empty too and the
  assertion passed with the guard disabled. It now stocks a row at the floor
  and asserts `trades_hits == 0`, which is what "refused at the CLIENT
  boundary" means. Bite-checked: the `false` edit above now fails it by name.
  A second test proposed for the contrast was written, bite-checked, and then
  DELETED as a duplicate of this one - strengthening the existing test was the
  smaller change and closed a live vacuity rather than adding a parallel one.

The sweep's other candidates were checked and stand: the `!is_disconnected()`
assertions in `an_unanswerable_identity_probe_does_not_refuse` and
`divergence_go_dark_within_the_idle_timeout_is_ridden_out` require the flag to
have been SET, `dialing_blind...` already holds it as a window, every
`wait_for_at_least` pair returns the observed count so the bound bites, and
`clock.rs`'s `is_empty()` is a real negative assertion behind a cancelled timer.

## Smaller notes

ALL CLOSED IN ROUND 5.

- `trade_history_pages_without_duplicates_at_the_seam` reads `trades_hits`
  after the response loop now, with the ordering argument written down: the
  count is settled because the response cannot exist until both fetches were
  served. The assertion was sound; what it lacked was any statement of why, so
  it read as a cross-task counter poked at an arbitrary moment.
- `request_whole_tape` resets `trades_hits` and `trades_starts` at entry, so the
  counters mean "SINCE ENTRY" - not "this run", which the reset cannot buy: each
  call binds a new stub over the SAME `Arc<StubState>` and never shuts the
  previous accept loop down, so a client still alive from an earlier call could
  contribute. None does today. The doc says which of the two it is.
- `next_exec_event` takes a `what: &str` and every one of its call sites names
  what it was waiting for. It also separates a closed sink from a timeout and
  reports the timeout duration. The old message named neither the expectation
  nor the wait and cost round 4 real time to attribute.
- `a_submitted_position_id_reaches_the_wire` drains to a deadline, matching its
  sibling `an_order_list_reaches_the_wire_as_linked_legs`. It was safe by an
  inference about the stub's reply rather than by observing the thing asserted
  on, and the two tests answered one question two ways.
- `an_account_labelled_differently_is_still_served` was closed in round 1. The
  design it rests on - the venue's account id is a LABEL and the client keeps
  its own - is now stated durably in `reference/architecture.md`, because a cold
  reviewer proposed the exact inverse assertion and only a measurement stopped
  it. The paragraph's scope was tightened after review: the venue CAN seat more
  than one ledger (`seat(&account_id, ..)` is keyed by account plus session), so
  the "nothing to be misrouted onto it" argument is a statement about ONE
  CONNECTION, and it now says so.

Also landed here, off this round's cold review and outside the adapter: the
`ETXTBSY` filed against `launch::tests::the_ready_bound_returns_on_time_against
_a_silent_venue` was misdiagnosed. `silent_venue()` has ONE caller and closes
its write handle before the exec, so there is no second writer in-process; the
race is CROSS-PROCESS, between the gate's dev and instrumented sweeps running
the same test at once. The filed fix - the test's own name in the filename -
would have changed nothing, since both processes compute the same name. The
path is PID-qualified now and unlinked after the launch, and the todo entry is
retired rather than corrected.
