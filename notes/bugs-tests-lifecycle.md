# Test hunt: mogwai-cli venue lifecycle and serving integration tests

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-cli/tests/` - the seven in-scope binaries, the `common/` launcher
harness and the seventeen `configs/` fixtures. The hunter also read
`brokkr.toml`'s gate profile for the parallelism and skip context.

This hunt looks for defects in the TESTS, not in the code they test: tests that
do not survive parallel execution, tests that wait on fixed durations rather than
conditions, tests that assume they are the only test in the process, tests that
cannot fail, fixtures that cannot represent their shape, and anything else weird.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.

## A. The unenumerated family - fixed spans on the success path

Findings 1-4 were fixed on 2026-08-18 and are removed. What landed, so a later
round does not re-derive it: every one of them now drains its sockets
concurrently with whatever else it is doing, gives each phase its own deadline,
and - the part that matters - reports a venue-side CLOSE as a close rather than
folding it into the property under test. `goes_quiet` was replaced by a
three-valued observer (`Serving` / `Quiet` / `Closed`), because the two-valued
one returned "still receiving" for a socket that no longer existed, which is how
the untargeted account passed its assertion while dead. The two tape-purity
tests now `split()` their socket and send while draining. Each fix was
bite-checked by injecting a real close and observing the new, truthful message.

THE FIRST PASS AT THAT LEFT THREE HOLES, all found by cold review of its own
diff and closed the same day. They are worth keeping as a shape rather than as
history, because the shape recurs: A DRAIN THAT DOES NOT RECORD HOW THE STREAM
ENDED IS NOT A DRAIN. The pass introduced background drains spelled
`tokio::spawn(async move { while s.next().await.is_some() {} })` and only ever
`abort()`ed the handle, so `None`, a close frame and a transport error ended it
identically and unobservably - reintroducing, in the background, exactly the
misdiagnosis it had just removed from the foreground. It also left the
resting-stop test's socket unread across the clock poll it had just added,
immediately after splitting the socket to avoid that, and left the other socket
unread across `await_acceptance` in both ledger tests. The file now carries two
shared helpers for this and nothing hand-rolls it: `BackgroundDrain`, which
remembers the ending and is checked with `assert_still_serving` BEFORE the
property is asserted, and `while_draining`, which drains one socket for the
duration of a future on another.

One correction to the report's own reading, worth keeping: it called the
`two_connections_share_one_ledger` pair harmless "because the query is retried by
the drain loop". It is not retried - the query is sent once - and the converse
test was the more dangerous of the two, since an order the venue had not booked
yet satisfies its `all(id != PRIVATE-1)` assertion VACUOUSLY. Both now wait for
the venue's own `OrderAccepted` before querying.

`a_passenger_duration_closes_one_socket_and_leaves_the_boat_running` was rebuilt
rather than patched, because neither cheap shape establishes its property. A
"discard whatever is queued, then take the next frame" drain empties only what
the reader task has FORWARDED, so a frame that reached the socket before the
exit and had not been polled yet is accepted as post-exit evidence; and a
comparison of tape stamps against the venue clock is meaningless on a
`speed = 0.0` venue. It now stages a ROUND TRIP - a query sent after the close,
then a print counted after the answer - which is post-exit by the stream's own
ordering. Its unbounded channel, which had been accumulating one entry per frame
across 1.5 s of unpaced firehose, is a `watch` of monotone counts.

Remaining, with reasons:

- `history_is_bounded_by_the_rivers_own_boat_not_the_venue_clock` -
  `sleep(250ms)` to "construct a late boat" (serving.rs:142). REFUSED, on the
  report's own grounds: it asserts the premise afterwards (`venue_clock > boat`),
  so a sleep that was too short fails the premise rather than producing a wrong
  answer about the property. That is already the correct pattern and the change
  would be cosmetic.
- `a_perpetual_position_pays_funding_across_an_interval` keeps its
  `sleep(3_000)`. The obvious replacement - poll the boat's clock across three
  one-second funding intervals - was implemented, run, and found VACUOUS: that
  venue is `speed = 0.0`, where the sim axis is not wall-rated and races ahead,
  so the poll returns immediately, before any wall-cadenced sweep pass has
  charged funding, and the test then fails on an unmoved balance. The binding
  resource is the sweeper's WALL cadence, and nothing the venue serves counts
  sweep passes. Filed in `notes/todo.md`; the test's own comment records the
  measurement so nobody retries it blind. Its `sleep(500)` is gone - it now waits
  for `OrderFilled`.

  The fix pass's EXPLANATION of that measurement was wrong and is corrected here
  so a later round does not inherit it: a zero speed does NOT make the clock race
  the wall. `build_run_clock` and the boatyard both substitute 1.0 for a
  configured 0.0, so the clock IS the wall, at rate 1. What comes apart is
  delivery, which is unpaced and runs the tape's `ts_event` far ahead of
  `server_now_ns` - so a clock target anchored on a tape stamp is satisfied at
  once. Written down durably in `reference/clock.md`, because round 3 reasons
  about wall-clock budgets and would otherwise re-derive it wrong.

## B. Tests that cannot fail, or fail for free

Findings 5-10 were fixed on 2026-08-18 and are removed. Every one reproduced;
none was refused. What landed, and the parts a later round would otherwise
re-derive:

- THE SUBSTRING FAMILY (8, 9, 10) is closed by parsing structure, never by a
  better substring. The two websocket refusals now destructure
  `tungstenite::Error::Http`, assert `status() == 400` AND read the body -
  tungstenite does carry the refusal body through the failed handshake, which
  was the open question. Each asserts the REASON that names it (`already
  seated` plus the sitting speed; the whole phrase `not funded in USDT`), so
  a 400 raised by any other check on the same route no longer satisfies it.
  The divergence ack is PROSE, not JSON as the report assumed, so `1000` is read
  out of it by splitting the sentence and parsing the token, and the trailing
  origin is compared against the readiness record's `run_start_ns`.

  THE FIRST PASS AT THAT SHIPPED TWO SUBSTRING DEFECTS OF ITS OWN, both found by
  cold review of its own diff and closed the same day, and both are the family
  they were fixing, re-entering through the repair. `body.contains("USDT")` was
  commented as the currency asserted AS A LITERAL - but the boot river is the
  default preset BTCUSDT, `USDT` is a substring of it, and the symbol assertion
  standing beside it therefore IMPLIED the currency one. A venue echoing the
  ACCOUNT's own currency back - `not funded in JPY, which is what BTCUSDT
  settles in`, a plausible real bug - satisfied it. That text was injected into
  `ws.rs`: the old pair passed it, the whole-phrase assertion refuses it. And
  the surge origin was BRACKETED against `[data_origin_ns, server_now_ns]` when
  it is an exact number the test already holds - `run.started_ns` is
  `data_origin + warmup`, and the readiness record reports it as `run_start_ns`,
  so a regression arming anywhere inside the warmup sat in the bracket. Arming
  at the raw data origin was injected: it passed the bracket and fails the
  equality, 0 against 300000000000.
- FINDING 5 WAS WORSE THAN FILED - not "likely" green by construction but
  provably so, and the bite-check demonstrated the asymmetry directly. Both
  market paths go through ONE `market_reading`; the price-less arm differs only
  in that the order is stamped with the last print, which
  `fills::read_last` supplies when `read_market` refuses. Both outcomes land on
  the tape, so `fill.last_px < last * 2` cannot separate them. The gate now
  scores each attempt on the engine's `market order has no market reading` WARN,
  captured through a new `common::spawn_capturing_stderr`, and cross-checks the
  log against the fill price on the priced arm so neither is trusted alone.
  Keying a gate on a log line is not a contract; the wire observable it wants -
  a reading instant on `OrderFilled` - is filed in `notes/todo.md`.

  READING A LOG BUYS A NEW WAY TO BE GREEN BY CONSTRUCTION, and the first pass
  walked into it: the score is an ABSENCE of the WARN, and the venue INHERITED
  the test process's environment, where `init_stderr_logging` falls back to
  `mogwai=info` only when `RUST_LOG` is unset. Under `RUST_LOG=mogwai=error` no
  WARN can be emitted, every attempt reads as having taken a reading, and
  `warned.len() < attempts.len()` is `0 < 6` - the price-less arm, the entire
  reason for the rewrite, unfalsifiable again. Closed on both sides: `LaunchSpec`
  grew an `env` field and `spawn_capturing_stderr` PINS `RUST_LOG=mogwai=info`
  on the venue, and `CapturedLog::await_positive_control` refuses to let any
  conclusion be drawn from an absence until a line the venue is known to emit
  has actually arrived in the buffer. Bite-checked by pinning the hostile filter
  instead: the test refuses with "no absence in it means anything" rather than
  passing, which also proves the env reaches the child, since an ambient-free
  venue would have logged at info.

  The attempt count and the inter-attempt sleep are the FLAKE MARGIN on that
  assertion, whose false-failure mode is every attempt legitimately refusing a
  reading. The first pass cut them from 8 and 500 ms to 6 and 300 ms while
  making every attempt scored rather than one; nothing compensated, so both are
  restored. The test runs in about 10 s at 8 attempts, inside the gate's budget.
- FINDING 6 IS REAL IN DIRECTION AND SMALL IN MAGNITUDE, and the fix pass
  measured it rather than assuming: on `paced.toml` the venue clock ran
  40-73 ms of SIM time ahead of the boot boat, so the old ceiling was generous
  by that much and a total suppression failure (which overshoots by two sim
  MINUTES in the two-second window) would have been caught anyway. It is fixed
  because the quantity was wrong, not because the slack was dangerous: the boat
  lag it admits is unbounded in principle - it grows with how late the boat was
  placed - while the new ceiling's slack is one arming round trip. The read is
  now `/clock?symbol=`, asserts `boat_clock`, and happens AFTER the ack.

  ITS RATIONALE FOR READING AFTER THE ACK WAS WRONG and is corrected here, since
  the comment would otherwise teach the next reader to preserve slack for a
  reason that does not exist: `ws.rs` gates at SEND time, not at publish time, so
  a frame published before the arm and still queued when it lands is dropped too.
  Nothing is legitimately in flight past an armed blackout. The read is after the
  ack for the CEILING'S freshness instead - the boat moves during the round trip,
  and a ceiling read before it sits below the last instant the venue was still
  entitled to publish at.
- FINDING 7 was exactly as filed and is a two-line fix.

ONE DEFECT FOUND WHILE FIXING 5, worth keeping because it is a measurement
artifact nothing else in the tree guards: the adverse-slippage floor in that
test was computed over a `/trades` page that was TRUNCATED. `bounded_trades`
fills its page from the START of the window and breaks at the limit, so a
300 s lookback at `limit=10000` returned the window's OLDEST prints - and the
floor was then taken over stale water the market had since fallen through, and
asserted as favourable slippage that never happened. It had never fired because
the assertion ran on at most one attempt per arm before the loop broke; running
it on every attempt exposed it immediately, deterministically, at seed 42. The
window is now 60 s anchored on a PRE-SUBMIT boat clock (the acceptance instant
is not a lower bound on the reading instant either).

THE FIRST REPAIR GUARDED IT WITH `trades.len() < MAX_HISTORY_LIMIT`, which is a
self-inflicted flake rather than a fix: the window is not 60 s but
`[before - 60s, reading_ts]`, and `reading_ts` derives from the acceptance
instant, so at speed 100 a slow round trip widens it without bound and the test
then fails naming stale water on a run where nothing is wrong. The window is now
PAGED instead, by `trade_window`, so truncation is not a condition to detect at
all. Its cursor obeys the frontier rule - a full page's last instant may be cut
in half, so that instant is dropped and the next query resumes AT it, never past
it - and `a_paged_tape_window_equals_the_same_window_read_in_one_query` pins the
walk against a single unlimited query at page size 3. That test is honest about
what it does not cover: this tape stamps every print at a distinct nanosecond
(it asserts so), so the colliding half of the cursor rule is unreachable on it,
and advancing past the boundary was tried as a bite-check and PASSED. Dropping
the boundary row bites hard - 13780 prints became 9187.

## C. Wall-clock budgets

Findings 11-14 were fixed on 2026-08-18 and are removed, along with the
deadline sweep the round owned. What landed, so a later round does not
re-derive it:

- THE 20 SECOND FIGURE IS CONFIRMED, from the tool's own reference rather than
  from the comments in `brokkr.toml`: the parallel lane keeps "the same per-test
  20s hang watchdog" as the serial one, attributes it by name and kills the
  process group. The before/after was demonstrated on one test with an injected
  never-satisfied condition: at the old 30 s deadline the run reported
  `exceeding the 20s per-test timeout ... killed cargo process group`, with no
  message, no file and no line from the test at all; under the budget it
  reported `the venue pushes its tape unbidden: Elapsed(())` at serving.rs:494
  in 16.2 s.
- The sweep is a HARNESS MECHANISM, not 36 retuned constants, because a test
  with several sequential phases overruns by summing generous per-phase bounds
  even when each one is legal. `common` carries `HANG_WATCHDOG`,
  `TEST_WALL_BUDGET` (16 s) and `deadline` / `wall_deadline`, which clamp any
  requested cap to a ceiling anchored at the test's FIRST LAUNCH.
  `TEST_WALL_BUDGET` is DERIVED from `HANG_WATCHDOG` minus a
  `WATCHDOG_HEADROOM` of four seconds rather than written down beside it, so the
  stated relationship cannot drift; four rather than one because the panic, the
  unwind and the venue teardowns all run after the deadline fires.
  `Venue::wait_for_exit` and the harness's blocking HTTP reads clamp the same
  way; the slowest gate in these files measures 9.7 s.
  TWO SITES ARE DELIBERATELY NOT CLAMPED and say so in place: the armed-blackout
  quiet window and the three-valued `observe` window are OBSERVATION LENGTHS,
  where shortening does not fail sooner, it passes on less evidence.

  THREE HOLES IN THE FIRST PASS AT THAT, found by cold review of its own diff
  and closed the same day:

  - SEQUENTIAL PHASES SUMMED AGAINST ONE BUDGET WITH NO RESERVATION, so the LAST
    phase absorbed every shortfall - a drain legitimately running to 15 s left
    `wait_for_exit` a second and the test then reported "venue did not exit
    within 20s", which is a false statement about the VENUE manufactured by the
    phase before it. `TEARDOWN_RESERVE` is three of the sixteen seconds:
    `wall_deadline` clamps to the budget minus it, `teardown_deadline` clamps to
    the whole budget, so the wait that reports on an exit always has its own
    wall. And the clamp now REFUSES rather than handing back an instant in the
    past - a spent ceiling used to make every `timeout_at` below it fire on its
    first poll, and each one then reported whatever it was named for.
    Bite-checked by widening the reserve to the whole budget: every bound
    refuses with "this test spent its 6s wall budget before this 10s bound was
    even taken ... an earlier phase used the whole budget", which names the
    budget rather than the venue.
  - THE HTTP READ TIMEOUT WAS NOT THE BOUND IT CLAIMED. `set_read_timeout`
    bounds ONE `read` syscall and the body was consumed by `read_to_end`, which
    issues many, so a venue dribbling a byte at a time reset it forever and the
    route was unbounded. It is a deadline loop now, and it names the route:
    bite-checked by forcing the deadline to `Instant::now()`, which reports "the
    test's wall budget expired while reading /health; 0 bytes arrived first"
    where the old shape would have said `read response: Resource temporarily
    unavailable`, naming neither the route nor the budget.
  - `Venue::Drop` COULD HAVE ABORTED THE PROCESS. It reads the captured log only
    when the thread is panicking, and `snapshot` took the lock with `expect`, so
    a capture thread that had panicked while holding it would turn the triage
    dump into a panic during unwinding - an abort, replacing the real assertion
    failure with an unattributed crash. All three readers go through one
    `read()` that takes `PoisonError::into_inner`. Not bite-checkable without
    poisoning the lock on purpose; it is closed by construction instead, since
    that path can no longer panic, and a poisoned buffer still holds every line
    captured before the poisoning, which is what triage wants.

  ONE COLD-REVIEW FINDING WAS REFUSED, and the refusal is the round's most
  useful fact. It held that `BUDGET_ANCHOR` is shared across every test in a
  binary, because libtest's `run_tests` special-cases `concurrency == 1` and
  runs each test INLINE on the calling thread - which would make the anchor
  stale from the second test onward and fail the whole suite wholesale under
  the very invocation AGENTS.md prescribes. THAT IS OLD LIBTEST. Today's
  `run_test` spawns a named thread per test whenever the platform supports
  threads, regardless of the concurrency level, and the measurement says so
  directly: with the reset deliberately disabled and the budget shrunk to six
  seconds, all fourteen tests of a 5.96 s serial run passed, and the anchors
  they reported were 140 ns and 200 ns old rather than seconds. The prescribed
  serial invocation was run before and after and is green.

  THE MECHANISM WAS BUILT ANYWAY, because the premise is a libtest
  implementation detail that has already changed once and nothing in this tree
  would detect it changing back - the failure mode is silent and wholesale.
  `spawn` re-anchors when no venue is live and `Venue`'s `Drop` clears the
  anchor when the last one goes, so the budget is per-test by the harness's own
  bookkeeping. No test in these files drops a venue and carries on, which is
  what keeps a mid-test re-anchor - which would push the ceiling PAST the
  watchdog - out of reach. Bite-checked by disabling both halves under a
  six-second budget, where a genuinely shared anchor fails from the fourth test
  on.
- 11 WAS MOVED RATHER THAN RETUNED. It is now `examples/tape_lateness_bench.rs`
  and the `tape_lateness` target, launching the shipped binary through the
  shipped launcher and reporting `frames`, `sample_ms`, `p50/p99/max_lateness_ns`
  as scraped counters. It is gone from the `timing` sweep's `only` and from the
  gate's `skip`, and `reference/performance.md` carries the first reading under
  the new shape: 11,893 frames, p99 42.9 ms on a quiet host - which is how
  little room the retired 50 ms threshold had even when it passed.

  ITS READ LOOP INHERITED THE RETIRED TEST'S TRUNCATION, found by cold review
  and closed: `while let Ok(Some(Ok(Message::Text(_))))` ENDS on a Ping, a Pong
  or a Binary frame, so `frames` - the declared WORK SIZE - would silently
  understate the sample and p99 would be taken over a prefix. It is the same
  rule finding 13 imposed on the drains one section up, and it matters more here
  because a recorded number is compared against later ones: the loop now
  `continue`s on a control frame, counts `non_trade_text` and `control_frames`
  apart, and REPORTS `ending` - `sample_complete`, `stream_ended` or
  `transport_error` - so two frame counts are only ever compared when both loops
  ended the same way. Measured rather than assumed: at 3 s on `accelerated.toml`
  the venue sends NO control frames, so the truncation was latent and the
  recorded reading stands. What the same runs did show is a large run-to-run
  spread - 9,186 against 11,659 frames minutes apart - which is on the record
  now, because it is the thing a single reading would have hidden.
- 12 was TIGHTENED, not left. The bound is now `serve.rs`'s own five-second
  `SHUTDOWN_GRACE`, which is the sentence the docstring already made, rather
  than an arbitrary ten; the wait that backstops it is ten seconds, so a venue
  that never exits is reported by the wait rather than by the watchdog.
  Bite-checked with a six-second sleep injected into `shutdown_signal`: the old
  ten-second bound would have passed that regression, the new one fails at
  6.2 s naming the grace.
- 13's inner timeout is now the outer deadline, and the loop RECORDS HOW IT
  ENDED - timeout, clean end and transport error are three separate messages,
  and none of them is allowed to reach the old `expect` that blamed the venue
  for serving a hole. Bite-checked by suppressing the `FeedLagged` capture: the
  failure now says "either the venue served a hole ... or this reader never fell
  behind the ring at all" instead of asserting the first.
- 14 was rebuilt on SUSTAINED PRESSURE rather than a bigger burst. One task
  sends continuously while another drains - the old shape fired all 50 sends
  before reading a byte, so the venue's writer could be backpressured by the
  test that was waiting to hear from it - and the loop ends on the refusal, the
  deadline or a blast-radius cap, reporting which. Bite-checked by widening
  `pending_command_acts` to 8192 in the fixture: 5,000 commands sent, no refusal,
  and the failure names both counts instead of timing out anonymously.

## D. Harness

15. FIXED on 2026-08-18: `spawn` now captures every venue's stderr into
    `Venue::log` and PRINTS THE TAIL ON PANIC, `spawn_capturing_stderr` is gone
    into it rather than left as a second path, and both of round 2's guards came
    across - `RUST_LOG=mogwai=info` is pinned on every venue, and
    `CapturedLog::await_positive_control` is unchanged and still owed by any
    caller concluding something from an absence. The pin is now load-bearing for
    triage as well as for scoring: an ambient `mogwai=error` would hand a failing
    test an EMPTY diagnostic dump, which is the same vacuity in another costume.
    `common::spec` keeps `Discard` and its doc comment now says so - it is the
    launcher-contract spec for the two gates that read `LaunchError` themselves,
    where `NoRecord` carries the stderr independently. Kept as a shape: the
    triage dump was confirmed by a real failure, which printed the venue's last
    23 lines through `mogwai listening` and `socket bound to river`.

    THE FOLD LEFT TWO DANGLING REFERENCES, both closed the same day. A comment
    in `serving.rs` still credited `spawn_capturing_stderr` for pinning
    `RUST_LOG` - a function that no longer exists - and `Venue::ready_at`
    survived with NO READERS AT ALL, kept alive only by the module's
    `#![allow(dead_code)]`, still documented as the instant "the acceleration
    gate measures the served run from" when that gate had been deleted in the
    same diff. The comment names `common::spawn` and the field is gone. Worth
    keeping as a shape: a blanket `allow(dead_code)` on a shared test module
    means nothing detects a field that lost its last reader, so a deletion has
    to carry the sweep with it.

16. **`common::boot_symbol` pins the venue against itself.** It resolves
    `venue.symbol` by calling `mogwai_server::config::Config::load` +
    `build_instrument_profiles` + `boot_symbol_def` IN THE TEST PROCESS - the
    same code the venue runs. Every test that asserts something about
    `venue.symbol` is therefore comparing the server's answer to the server's
    answer. This is the AGENTS.md "two implementations pinned by one shared
    fixture" rule violated in the cheapest possible way.
    `preset_only_config_resolves_the_boot_river` is the only test that pins it
    against a literal (`"MNQ"`). The right shape is for the readiness record to
    carry the boot symbol - the harness comment even notes "a venue reports no
    symbol", which is the actual defect.

    REFUSED on 2026-08-18, on the ground that the recommendation reverses a
    recorded ruling rather than fixing an oversight, and that direction is not
    this loop's to take. Commit `0f12796` removed `symbol` from the readiness
    record two days before this report was written, as slice 2 of the grand
    design, and stated the reason: "a venue has no symbol under the boatyard
    model, so the readiness record's symbol field could only lie", with
    `ReadyRecord::VERSION` 6 as the designed loud break for consumers still
    parsing it. The same commit put `common::boot_symbol` here ON PURPOSE, and
    created `preset_only_config_resolves_the_boot_river` as its literal pin,
    "whose bite was proven". Putting the field back is a program decision for
    the owner, and it would cost a schema version and every consumer of the
    record; the fix pass does not get to make it from a test-hygiene finding.

    The complaint is also weaker than filed. The AGENTS.md rule is about a GATE
    that COMPARES two computations of one quantity, and no test here gates on
    the boot symbol: they use it to address requests. The one place the
    resolution is asserted is the preset-only test, against a literal, which is
    exactly the shared-fixture shape the rule asks for. What was missing was the
    RECORD of any of that at the harness, which is why the resolution looked
    like an accident to a reader; `Venue::symbol` and `boot_symbol` now carry
    the ruling, the reason the raw config key cannot be used, and the
    instruction not to remove the pin. Reopening this needs an owner decision,
    not another reading of the same code.

Findings 17-19 were fixed on 2026-08-19 and finding 20 was refused; the detail
each one still binds is below, and the cold review of that pass is folded in
rather than kept as a second layer.

17. FIXED on 2026-08-19, and THE LEAK WAS WORSE THAN FILED. The unbounded
    `read_line` is now `read_line_within`, a bounded read on its own thread
    (there is no portable way to bound a blocking pipe read in place), reported
    at 10.22 s by the test itself rather than at 20 s by the watchdog. The
    20 s poll round 3 left unclamped is `teardown_deadline(10s)` - that wait
    runs last and its message blames the venue, which is what the reserve is
    for.

    THE `sleep 3600` LEAKS ON THE SUCCESS PATH, not only on a panic, and a drop
    guard around the `Child` does not fix it. The shell FORKS the sleep rather
    than exec'ing it, so killing the shell orphans a grandchild the test never
    learns the pid of; the machine this was fixed on was carrying five of them
    from earlier green runs, and each further run added one. The guard is a
    PROCESS GROUP - `process_group(0)` on the shell, `killpg` on drop - and the
    explicit kill below it still signals the SHELL ALONE, because a group kill
    would have killed the venue directly and the PDEATHSIG property under test
    would have proven nothing. Bite-checked in both directions: with the venue
    removed from the script the bounded read reports at 10.22 s naming what it
    waited for, and `pgrep -P 1 sleep` grew by one per sweep before the group
    guard and by zero across every run after it, the full gate included.

    TWO THINGS THE GUARD DEPENDS ON, raised by cold review and now stated at the
    guard rather than left implicit. The pgid is signalled AFTER the shell has
    been killed and reaped, so the pid it names has been released - and that is
    safe only because `sleep 3600` keeps the group non-empty and therefore pins
    the pgid against reuse. Change the script so nothing outlives the shell and
    the `killpg` becomes a SIGKILL aimed at whatever group inherited a recycled
    pid. And whether the sleep is forked at all is SHELL-DEPENDENT: a shell may
    exec the last command of `A & B` in place, in which case there is no
    grandchild and nothing leaks. The nine orphans say this machine's `/bin/sh`
    forks; the guard is safe either way, it is its NECESSITY that varies.
    `read_line_within`'s thread reclamation carries the matching ordering
    dependency - on the panic path the pipe is closed by the guard's drop, so the
    guard must already be constructed when the read is taken - and now says so.

18. FIXED on 2026-08-19, with one CORRECTION to the finding. The dead
    `diagnostics`/`diagnostics_for_sink` pair is gone and the first launch now
    says `StderrSink::Discard`, which is what it always was in effect - a buffer
    filling for no reader, which reads to a maintainer as though the assertion
    beside it were scoring something.

    THE COMMENT WAS NOT DESCRIBING NOTHING, so it was rewritten rather than
    deleted. The race it names is real and the structure IS there: the faulted
    config is launched after the healthy assertion, and it has to be, because a
    venue whose source faults may be gone before any client can poll `/health`
    - so "`fault` is null before the fault" is not observable on that process.
    What was wrong is that the comment sat on a launch named `faulted` that is
    the HEALTHY one; the variable is `healthy` now and the comment states the
    reason rather than the ordering.

19. FIXED on 2026-08-19, in `common::scratch`, and used by
    `characterize_cli.rs` and `arrival_control_exposure.rs` alike. Two
    collisions, closed differently because they are different: ACROSS PROCESSES
    the directory name carries the pid, so a collision is unrepresentable rather
    than detected; WITHIN a process two tests share a pid and nothing about the
    path can separate them, so the names are claimed in a registry and the
    second claim PANICS naming the shared key. The finding's own suggestion
    (`line!()`) would have made the names unique without making a reused one
    loud, which is the weaker half of the same job. Bite-checked by pointing two
    of the three characterize tests at one name: the second refuses with the
    shared key rather than deleting the first's output.

    THE PID SUFFIX TRADED A COLLISION FOR A LEAK, found by cold review of that
    diff and closed the same day. Reuse was the only thing bounding the growth of
    `target/tmp`: a fixed name was one directory rewritten every run, while
    `<name>-<pid>` is a fresh directory per run that nothing ever revisits, so
    three of them - each holding a synthesized corpus and a written report -
    accumulated per invocation, forever. `scratch` returns a `Scratch` GUARD now,
    on the pattern `mogwai-lab`'s `storage::ScratchDir` already uses, and the one
    call site that passed the path straight into a callee binds it to a name so
    the temporary cannot drop first. The guard KEEPS the directory when the
    thread is panicking and says where it is: that directory is the whole
    evidence of a failing characterize run, and removing it would leave a
    maintainer the assertion message and nothing to read. Bite-checked by
    counting: two further runs of the characterize binary added zero directories
    where each had previously added one.

    LATERAL, NOT FIXED, because it is another owner's file: `target/tmp` on this
    machine also holds 503 `*d-*.log`, 180 `bad-*.toml` and 168 `bad-*.log`
    entries, and twelve `stale-*.pid`. Whatever writes those is leaking the same
    way and is outside this document's scope.

20. REFUSED on 2026-08-19, and the reason matters for round 5. `FOOBAR` IS THE
    WORKSPACE'S IDIOM for an unconfigured label - `config.rs`, `source.rs`,
    `seeds.rs` and `configs/unmatched-symbol.toml` all use it - so renaming it
    in one test file buys nothing today and costs the idiom.

    THE LITERAL IS NOT THE HAZARD. What is fragile in `unconfigured_symbol.rs`
    is that both tests ASSERT AN ABSENCE FIRST: `!advertises(..)` is a statement
    about a venue on which nothing has materialized the label yet, and it is
    sound only while the venue belongs to that test alone. Consolidating
    binaries does not break it; SHARING A VENUE does, which is precisely round
    5's first move. Both tests are therefore recorded in place as belonging on
    the OWNED side of any shared-venue split, which is a durable statement about
    the property rather than a rename that would look like protection.

    ONE REAL DEFECT FOUND IN THAT FILE while judging this, and fixed: its drain
    took `tokio::time::Instant::now() + Duration::from_secs(30)`, a bound PAST
    the 20 s watchdog, so its "the bound river produced no market frame" panic
    could never have been printed. Round 3's sweep covered `lifecycle.rs`,
    `completion.rs` and `serving.rs` only, and nothing detects a fourth file
    holding the same shape - which is the open item on that mechanism, not a
    property of this file.

    ITS DRAIN WAS THE WRONG SHAPE TOO, and the first pass clamped the deadline
    while standing next to it. `while let Ok(Some(Ok(Message::Text(..))))` ends
    on a Ping, a Binary or a Close as well as on the deadline, so a venue that
    CLOSED the socket arrived as a venue that served an unlabelled river - a
    wrong answer, and the drain rule this document opens with, in the very file
    the sweep had just visited. The loop records its ending now, in four
    distinguishable forms, and the assertion carries it.

## E. Structural - the thing the hunter would actually fix first

**`serving.rs` calls `spawn(...)` 54 times.** Cargo runs test BINARIES
sequentially and `--test-threads=8` parallelizes only within one, so the gate's
wall floor is this single binary booting 54 venues - and `accelerated.toml`
materializes 6 simulated hours of warmup, `band.toml` 1 hour, and `band.toml` is
spawned 4 times. Eight of those generating tape concurrently is EXACTLY the load
that makes every timing assertion in sections A and C flake. The two problems are
the same problem.

Three moves, in order of payoff:

- **Share venues by config where the test is a read-only observer.** About 32 of
  the 54 spawns use `fast.toml`, and a large fraction of those only issue HTTP
  GETs (`a_pulled_account_snapshot_is_labeled_venue_clock`,
  `an_account_naming_no_policy_is_unpoliced`,
  `history_refuses_an_illegal_symbol_...`,
  `the_full_warmup_span_is_servable_at_readiness`,
  `trades_after_sim_now_are_refused_with_400`, the `/clock` tests). One leaked
  `OnceLock<Venue>` per config serves all of them. The tests that ARM RUN-SCOPED
  DIVERGENCES (`CommandLatency`, `StallData`, `GoDark`, `FlowSurge`) or MUTATE
  ACCOUNT STATE must keep owning a venue - that split is the design, and it is
  worth stating explicitly in the harness so the next test lands on the right
  side of it.
- **Split the binary along that same line.** `serving_readonly.rs` (shared venue,
  fast, safe at any thread count) vs `serving_owned.rs` (one venue each). Right
  now `serving.rs` is a 2351-line grab bag whose only organizing principle is
  "L3-L6", which no longer means anything.
- **Then re-examine the parked pair.** Both fail because the client's connect
  races a `--duration` measured from readiness. If the venue exposed a
  passenger-scoped duration for these (which
  `a_passenger_duration_closes_one_socket...` proves exists: `?duration_ms=`),
  the deadline starts at UPGRADE and the race is structurally gone rather than
  parked. That would unpark both without a fixed wait anywhere.

## F. Out of scope but noticed

Both items were fixed on 2026-08-19 and are removed. What landed:

- The fixed shared `target/arrival-control-exposure` path is
  `common::scratch("arrival-control-exposure")`, per finding 19 above. Worth
  keeping from the reading: under `curve: None` - which is what this call site
  passes - `control_generated_pass` never touches the directory at all, so the
  collision was latent rather than live. That is a property of the CALL SITE,
  not of the callee, which is why the fix went in anyway.
- THE SKIP-LIST INVARIANT IS ENFORCED, by
  `crates/mogwai-cli/tests/gate_skip_list.rs`. It reads the gate profile's skip
  list out of the project config as DATA, reconstructs every test name under
  `crates/*/src` and `crates/*/tests` from the source text, and fails naming
  both the pattern and the non-ignored test it swallowed - where the coverage
  audit could only report the downstream orphan. It also refuses a DEAD entry
  matching no test at all, and a second test holds the release sweep's `only`
  list against the same skip list, which is the other invariant the config
  states in prose and closes with "nothing checks that they do".

  THE SCANNER'S FIRST RUN FAILED, and the failure was its own blind spot rather
  than a defect in the list: ten of the envelope fidelity gates are emitted by a
  `fidelity_gate!` macro, so a `fn`-only scan called their skip entry dead. It
  reads `macro_rules!` generators now - a macro whose body carries `#[test]`,
  with the generated name taken from the invocation's first argument - which is
  the shape the tree uses. That blindness was the dangerous direction too: a
  non-ignored macro-generated test would have been invisible to the violation
  check as well. Bite-checked three ways: un-`#[ignore]`ing
  `parity3b_session_profile_reproduces_the_preset_dow_weight` names it with file
  and line under the `parity3b_` prefix; un-`#[ignore]`ing the macro body names
  all ten generated gates with their full `arrival_envelope::tests::` paths;
  and dropping `read_market_latency_stays_within_submit_budget` from the skip
  list fails the second test naming the release sweep.

  THAT FIRST VERSION SHIPPED FOUR HOLES OF THE KIND IT WAS BUILT TO DETECT, all
  found by cold review of its own diff and closed the same day. A parser-backed
  scanner that fails OPEN is worse than no scanner, because it reports green, so
  the shape of every fix is the same: REFUSE where the parser cannot see, and
  bound the refusal with a fixture.

  - COMMENT STRIPPING WAS ONE-SIDED AND WRONG IN THE DANGEROUS DIRECTION. It
    claimed a `//` cut inside a string literal "can only ever lose a brace and
    pop a module early"; losing a CLOSE does the opposite - `depth` stays too
    high, the `mod` is never popped, and every later test in the file gets a
    phantom prefix, which turns a live skip entry into a false DEAD one and,
    where the prefix was all that stood above the match, a live violation into a
    false negative. Block comments were not handled at all, so a commented-out
    `#[test] fn foo()` counted as a live test. It is a literal-aware whole-file
    stripper now - line comments, NESTED block comments, `"..."`, raw and byte
    strings at any hash count, and character literals distinguished from
    lifetimes - and it BLANKS string interiors rather than copying them, which
    also stops a `{` or a `[` inside a message from moving the module depth or
    an attribute's bracket count. An unterminated comment or string panics
    naming the file. Bite-checked by restoring the naive stripper as a text
    edit: `second`, which sits outside `mod inner`, is reported as
    `inner::second`.
  - ONE IGNORE STATUS WAS ATTRIBUTED TO EVERY TEST A GENERATOR EMITS, which is
    only sound while the body is uniform - and "all ignored" is exactly the
    answer that satisfies this file's invariant for free, so the hole came back
    one level down. Uniformity is MEASURED now, `#[test]` count against
    `#[ignore` count, and a mixed body panics rather than guessing; a
    per-invocation ignore attribute is refused separately. Bite-checked by
    adding a second, non-ignored arm to `fidelity_gate!`: it refuses naming the
    macro, the file and both counts.
  - GENERATORS WERE ONLY FOUND IN THE FILE THAT INVOKED THEM, so a
    `macro_rules!` exported from a shared module was invisible to both checks.
    The scan is two passes now, generators tree-wide first. The cost is an
    over-approximation, which is the safe direction for the violation check.
  - INTEGRATION SUBMODULES WERE UNDER-RECONSTRUCTED: only a TOP-LEVEL file in
    `tests/` is a binary, so `tests/foo/bar.rs` reports as `foo::bar::<test>`
    and the flat treatment produced false dead entries.

  AND THE SCANNER NOW REFUSES RATHER THAN DROPPING what it cannot parse. A
  `#[test]` attribute that does not land on a declaration it recognizes is
  collected and reported by name, line and attribute text - the `tests.len() >
  500` threshold was the only guard before, and it would not have caught losing
  one file or one declaration form, which is precisely how the first version
  failed. That refusal fired on its first run, on a real blind spot nobody had
  filed: a `#[ignore = "..."]` long enough that rustfmt WRAPPED it, whose
  continuation line was being read as the declaration. Attributes are
  accumulated across lines by bracket balance now. Five parser fixtures pin all
  of this in a `parser` module beside the checks, every one bite-checked by
  reverting its production half as a text edit.

  THE RELEASE-SWEEP AGREEMENT WAS A SUBSTRING COMPARISON, AND NEITHER DIRECTION
  OF IT IS THE INVARIANT. The review was right that `skip.contains(filter)` is
  pure slack - any unrelated long skip entry satisfies it - but the sound
  converse, `filter.contains(skip)`, REFUSES THE CONFIG AS IT STANDS: the live
  `only` filter `read_market_latency` is SHORTER than the skip entry
  `read_market_latency_stays_within_submit_budget`. That was measured, not
  argued - the first version of the fix was the converse and the full gate
  failed on it. So the check is resolved against the TESTS instead, which this
  file already reconstructs: every test the `only` filter catches must be
  matched by some skip entry, and an `only` filter matching no test at all is
  refused as a lane that evaluates nothing. That also catches what no substring
  form can see - a NEW test growing into the `only` filter without growing into
  any skip entry. Bite-checked by widening the filter to `market_reading`, which
  fails naming both tests it would have caught. A degenerate short skip entry is
  refused too: the empty string is a substring of every name and would satisfy
  every check here vacuously.

  IT COST ONE EXCLUSION IN THE `no-brokkr-in-rust-source` textlint rule, for
  that one file by name. The rule's stated purpose is that a source file must
  not be able to INVOKE the tool - the deadlock it was written for needs a
  subprocess - and this file spawns nothing; it opens the config with `std::fs`
  and parses it. Excluding one named path rather than relaxing the pattern keeps
  the rule absolute everywhere else.

  THAT EXEMPTION IS A PROPERTY OF THE FILE'S CONTENTS AND NOT OF ITS PATH, so it
  is bounded rather than permanent: `the_excluded_file_spawns_no_subprocess`
  reads this file's own source, strips it so the prose about spawning cannot
  trip a check about the code, and refuses any process-spawning construct in it.
  Bite-checked by adding a `std::process::id()` call, which it names. The
  config's prose was corrected in the same pass on both counts the review
  raised - the `only`/`skip` sentence that still ended "nothing checks that they
  do", and a comment placing the skip list BELOW itself when it is thirty lines
  above.
