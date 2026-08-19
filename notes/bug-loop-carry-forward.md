# Bug-loop carry-forward

State the bug-hunt orchestration loop's agents cannot see. No agent in the loop
observes any round but its own, so every brief carries the relevant slice of
this forward. Not a history: when an entry stops binding future work, delete it.

Arc in progress: the eleven `notes/bugs-*.md` reports, worked one document at a
time in this order. `bugs-tests-lifecycle` CLOSED on 2026-08-19 after five
rounds plus a close pass over the whole commit arc, with no open findings;
`bugs-tests-adapter` CLOSED the same way on 2026-08-19, five rounds plus its own
close pass; `bugs-tests-tape` CLOSED on 2026-08-19 after its five rounds and
close pass; `bugs-tests-engine-protocol` CLOSED on 2026-08-19 the same way,
five rounds plus a close pass over the whole commit arc, with no open findings
apart from two recorded refusals and one carried residual.
`bugs-tests-lab-cli` CLOSED on 2026-08-19 after five rounds, with no open
findings apart from THREE FULL REFUSALS - B2, D1 and D4 - AND ONE PARTIAL, D2,
refused as a fixture candidate and closed by anchoring its inputs instead.
Counted rather than estimated: three summaries of that document had stated the
number three different ways. ITS CLOSE PASS RAN ON 2026-08-19 AND WAS THE LAST
OF THE TEST DOCUMENTS' - one finding, a runtime guard installed at two of six
call sites; see that section. `bugs-protocol`, THE FIRST PRODUCTION-CODE REPORT OF THE ARC, CLOSED on
2026-08-19 after five rounds, with no open findings apart from three recorded
refusals - C, L and M - and five deferrals filed in `notes/todo.md`, AND A CLOSE
PASS OVER ITS FIVE COMMITS on the same day - see that section near the end,
which also states plainly what is closed and what remains. `bugs-data`
starts next. From here the findings are about shipped behaviour rather than
about the gates that watch it, and "the test cannot fail" stops being the
default diagnosis.
WHAT STILL BINDS FROM THE TEST ARC is everything about the tests a production
fix WRITES: a fix whose regression test cannot fail has closed nothing, so the
vacuity rules, the bite-check discipline and the shared-fixture rule all carry
forward unchanged and apply to the new tests rather than to the reports. The order is
`bugs-tests-lifecycle`, `bugs-tests-adapter`,
`bugs-tests-tape`, `bugs-tests-engine-protocol`, `bugs-tests-lab-cli`, then
`bugs-protocol`, `bugs-data`, `bugs-engine`, `bugs-server`, `bugs-cli`,
`bugs-adapter`. Tests before production, because a report full of tests that
cannot fail means the production rounds after it are validated by a gate nobody
trusts yet.

## Machinery later rounds may build on and must not break

- `crates/mogwai-cli/tests/serving.rs` carries two shared helpers, and nothing
  in that file may hand-roll either one again. `BackgroundDrain` reads a socket
  in the background and REMEMBERS HOW THE STREAM ENDED; `assert_still_serving`
  checks that ending BEFORE the property under test is asserted.
  `while_draining` drains one socket for the duration of a future on another.
  Both exist because the round-1 fix pass introduced
  `tokio::spawn(async move { while s.next().await.is_some() {} })` with an
  `abort()`ed handle, where a close frame, a transport error and a clean end
  were indistinguishable and unobservable.
  - `stop` IS ASYNC AND YIELDS BEFORE IT CHECKS, found in round 6. The ending is
    written by ANOTHER TASK, so a silent return means "nothing recorded yet",
    not "the stream is alive" - and the last thing before a `stop` is typically a
    BLOCKING `http_get` holding the runtime thread, so a close that arrived
    during it was sitting unpolled and the guard reported success on exactly the
    branch it was built to catch. One scheduler turn removes the reachable half;
    nothing can make it proof. Pinned by
    `stopping_a_drain_sees_a_close_that_no_await_gave_it_a_chance_to_record`,
    which has NO await between the spawn and the stop and fails without the
    yield. CARRY THE SHAPE TO THE ADAPTER DOCUMENT: a guard reading a record
    another task writes owes that task a chance to write it.
- `crates/mogwai-cli/tests/common/mod.rs` CAPTURES EVERY VENUE'S STDERR. `spawn`
  sets `StderrSink::Lines` into `Venue::log`, a `CapturedLog`, and `Venue`'s
  `Drop` prints the last 40 lines WHEN THE THREAD IS PANICKING. There is no
  second launch path any more: round 2's `spawn_capturing_stderr` folded into
  this in round 3, carrying both of its guards, and nothing may reintroduce a
  separate capturing spawn.
  - IT PINS `RUST_LOG=mogwai=info` ON THE VENUE through `LaunchSpec::env`. The
    venue inherits the test process's environment otherwise, and
    `init_stderr_logging` falls back to `mogwai=info` only when `RUST_LOG` is
    ABSENT, so an ambient `mogwai=error` silences every line. That is vacuity
    twice over now: a caller scoring an ABSENCE passes for free, AND a failing
    test's triage dump comes back empty.
  - `CapturedLog::await_positive_control` polls for `mogwai listening` and
    panics if it never arrives. EVERY conclusion drawn from an absence in that
    buffer owes a call to it first; without one, a broken filter, a dead capture
    thread and a closed pipe all read as "the venue never said it". The panic
    dump does NOT owe one - it draws no conclusion.
  - `common::spec` still sets `Discard` and its doc comment now says so. It is
    the launcher-contract spec for the two gates that call `launch` themselves
    and read `LaunchError`, where `NoRecord` carries stderr independently of the
    sink. Every other caller wants `spawn`.
- `common` OWNS THE WALL BUDGET, and no test in these files may write
  `Instant::now() + <cap>` again. `HANG_WATCHDOG` is the tool's 20 s per-test
  kill; `TEST_WALL_BUDGET` is DERIVED from it as `HANG_WATCHDOG -
  WATCHDOG_HEADROOM`, 16 s, so the two cannot drift apart; `deadline(cap)` and
  `wall_deadline(cap)` return `min(now + cap, anchor + TEST_WALL_BUDGET -
  TEARDOWN_RESERVE)`, so several sequential phases cannot overrun by summing
  legal per-phase bounds. The harness's blocking HTTP reads clamp too.
  - `TEARDOWN_RESERVE` is 3 s of the 16 that no ordinary phase may spend, and
    `Venue::wait_for_exit` takes its bound from `teardown_deadline`, which
    draws on it. Without a reservation the LAST phase absorbs every overrun the
    ones before it left, and the test then reports "the venue did not exit" -
    a claim about the venue produced by the drain in front of it. A new wait
    that runs last, and whose message would blame the venue, belongs on
    `teardown_deadline`.
  - THE CLAMP REFUSES rather than returning an instant in the past. A spent
    ceiling made every `timeout_at` below it fire on its first poll, so each
    bound reported whatever IT was named for; the panic now names the budget and
    says an earlier phase spent it.
  - THE ONE EXCEPTION IS AN OBSERVATION WINDOW - the armed-blackout quiet window
    and `observe`'s window - where clamping does not fail sooner, it passes on
    less evidence. Both say so in place. Judge which kind a new wait is before
    reaching for the helper.
  - THE BUDGET IS PER-TEST BY THE HARNESS'S OWN BOOKKEEPING, not by the
    thread-local it lives in: `Venue::drop` CLEARS the anchor when the last live
    venue goes, and `spawn` SETS it when it finds none. See the libtest fact
    below for why that is not redundant.
    THE ASYMMETRY IS THE FIX AND IT COST A ROUND-6 FINDING. `spawn` used to
    re-anchor on a zero venue count, which is the watchdog overrun the mechanism
    exists to prevent wearing the costume of the reset that prevents it. The
    carry-forward's own rule for it - "a test must not drop a venue and then
    carry on" - covered only the TRACKED case, and
    `a_faulted_venue_exits_nonzero_and_an_exhausted_one_does_not` is an untracked
    one: it drives two venues through `launch` DIRECTLY, so the counter never
    leaves zero while up to ten seconds of budget is spent, and the `spawn` after
    them re-anchored a test already most of the way through its watchdog.
    BOOKKEEPING THAT COUNTS ONLY WHAT IT WAS TOLD ABOUT CANNOT BE THE SOLE GUARD;
    refusing to move an anchor at all does not need to be told. Pinned by
    `a_venue_launched_after_untracked_work_inherits_that_works_budget` in
    `lifecycle.rs`, bite-checked at 250 ms of ceiling drift.
- `serving.rs` also carries `trade_window` / `trade_window_paged`, and no test
  in the file may query `/trades` for a statistic without going through them.
  A single query returns the window's OLDEST prints once the page fills, silently
  (see the `/trades` fact below). `a_paged_tape_window_equals_the_same_window_read_in_one_query`
  pins the walk at page size 3 against a single unlimited query.
- `crates/mogwai-cli/examples/tape_lateness_bench.rs` and the `tape_lateness`
  target replaced `tape_lateness_under_acceleration`. Do not put it back as a
  test: its 50 ms p99 is a statement about the host, it was excluded from the
  debug lane for being a latency budget and unreliable in the release one under
  load, and a gate no lane runs measures nothing. THE GENERAL FORM, which is
  worth applying to the next one: a wall-clock threshold with no admission test
  is a measurement, not a gate.
  - ITS READ LOOP REPORTS `ending`, and a recorded frame count is only
    comparable against another taken with the same ending. The loop inherited
    the retired test's `while let Ok(Some(Ok(Message::Text(_))))`, which stops
    on a Ping and truncates the sample silently - the drain rule below, in the
    measurement's costume. Control frames are counted, not fatal; measured at
    zero in a 3 s sample, so the truncation was latent.
  - AND THE FIRST READING WAS TAKEN WITH THE PRE-FIX DRAFT, which round 6 caught
    in `reference/performance.md`: its recorded p99 of 42.9 ms had landed on the
    MAX, because a truncated sample has too few points to separate the two. The
    shipped instrument reports p99 9-28 ms over three consecutive 3 s samples,
    with max steady at 42-43 ms. A NUMBER RECORDED FROM A DRAFT OF THE
    INSTRUMENT IS NOT A READING, and the round's own second half is where to
    look for it: the fix and the number it invalidated landed in one commit.
- `common::scratch(name)` OWNS EVERY SCRATCH DIRECTORY THE `mogwai-cli` TEST
  BINARIES WRITE, and none of them may hand-roll a `CARGO_TARGET_TMPDIR` path
  again. SCOPED TO THOSE BINARIES DELIBERATELY: `mogwai-lab`'s
  `storage::ScratchDir` hand-rolls one too, correctly, and stating this
  workspace-wide would read as an audited invariant that nothing checks. The name
  carries the pid, so a cross-process collision is unrepresentable rather than
  detected, and the names are claimed in a process registry so a second claim
  PANICS instead of resolving the ambiguity by deleting the first test's output.
  Two callers: `characterize_cli.rs` and `arrival_control_exposure.rs`.
  - IT RETURNS A GUARD, and the guard is not optional. The pid suffix removed
    the REUSE that used to bound `target/tmp`: a fixed name was one directory
    rewritten per run, `<name>-<pid>` is a fresh one nothing revisits, so three
    corpora and three reports accumulated per invocation until `Drop` went in.
    Hold it for the test's lifetime - dropping it early removes the directory out
    from under the code still writing there, which is why the one call site that
    passed the path straight into a callee binds it to a name. It KEEPS the
    directory when the thread is panicking, because that directory is the
    evidence of the failure. `mogwai-lab`'s `ScratchDir` is the pattern.
  - LATERAL AND UNOWNED: `target/tmp` on this machine also carries 503
    `*d-*.log`, 180 `bad-*.toml`, 168 `bad-*.log` and twelve `stale-*.pid`.
    Something else leaks the same way, outside the lifecycle document's scope.
- `crates/mogwai-cli/tests/gate_skip_list.rs` ENFORCES THE PROJECT CONFIG'S OWN
  SKIP-LIST INVARIANT - a skip pattern may match only `#[ignore]`d tests - by
  reconstructing every test name from source text.
  - A PARSER-BACKED SCANNER THAT FAILS OPEN IS WORSE THAN NO SCANNER, and this
    one is built on that: it REFUSES wherever it cannot see. A `#[test]`
    attribute that does not land on a declaration it recognizes is reported by
    file, line and attribute text rather than dropped; a `macro_rules!`
    generator whose emitted tests are not uniformly `#[ignore]`d panics rather
    than being marked all-ignored (which would satisfy the invariant for free);
    an unterminated comment or string panics naming the file. A NEW WAY OF
    DECLARING A TEST therefore fails the scan loudly rather than shrinking it
    silently - teach the parser, never relax the refusal. Five fixtures in its
    `parser` module pin the comment stripping and the attribute handling.
  - ITS COMMENT STRIPPING IS LITERAL-AWARE AND BLANKS STRING INTERIORS. Both
    halves are load-bearing: a naive `//` cut loses a closing brace and gives
    every later test a phantom module prefix, and a `{` or `[` inside a message
    moves the module depth or an attribute's bracket count the same way.
  - THE `only`/`skip` AGREEMENT IS RESOLVED AGAINST THE TESTS, not by comparing
    the two strings. Both directions of the substring comparison were tried and
    both are wrong: `skip.contains(filter)` is satisfied by any unrelated long
    skip entry, and the sound-looking converse REFUSES THE LIVE CONFIG, where
    the `only` filter is the shorter of the two. What is owed is that every test
    the filter catches is excluded by some skip entry.
  - It is the single excluded file in the `no-brokkr-in-rust-source` textlint
    rule, named explicitly - it reads the config with `std::fs` and spawns
    nothing, which is the distinction the rule is actually about. THAT
    EXEMPTION IS BOUNDED BY A TEST IN THE FILE, `the_excluded_file_spawns_no
    _subprocess`, because an exemption justified by contents and keyed on a path
    decays into the hole the rule was written for.
- `crates/mogwai-cli/tests/completion.rs` carries `watch_a_bounded_run`, and
  every test that gives a venue a `--duration` and then wants a socket onto that
  run must go through it. THE DEFECT IT REMOVES IS THE ADAPTER DOCUMENT'S TOO,
  because the shape is the LAUNCHER'S rather than this file's: a declared
  duration is a WALL sleep started at readiness (`serve.rs` sleeps
  `sim.wall_duration(remaining)`, then completes the run) and `launch` returns AT
  readiness, so every such test connects into a span already running down.
  Losing that race is a WRONG ANSWER - the test fails on not having seen the
  announcement - which is why it reads like a regression.
  - THE PREMISE IS "THIS SOCKET WAS A LIVE SESSION", and getting this wrong cost
    a whole cycle, so take it as given rather than re-deriving it. ATTACHING LATE
    IS NOT THE DEFECT: `ws.rs` evaluates `already_complete` when a session
    STARTS and announces to a socket that arrived after the run finished. What
    produces nothing is a connection accepted by a venue already tearing down,
    which never becomes a session. The only sound evidence either way is the
    venue having WRITTEN SOMETHING on that socket, so the helper drains every
    socket and discards the whole run unless each saw at least one frame.
    A premise phrased as "attached before `run_start_ns + run_duration_ns`" was
    built first, passed locally and passed the binary at 8 and 16 threads, and
    then failed the full gate on the very test it was written to fix.
  - IT IS A PREMISE, NOT A MARGIN. A longer duration was refused explicitly: a
    margin is what a crowded host takes away, and the family had already been
    parked rather than retuned for that reason. A passenger-scoped
    `?duration_ms=` was refused too - it does start at upgrade and the race
    really is gone, but it closes ONE socket and leaves the run going, so it
    cannot express "the venue exits 0 at its deadline" or "the announcement
    reaches every socket".
  - THE DISCARDED VENUES ARE HELD, NOT DROPPED. `common` re-anchors the wall
    budget when the LAST live venue drops, so releasing a loser mid-test restarts
    the budget and pushes the ceiling PAST the watchdog. Any test that holds
    several venues owes the same care.
  - It covered FOUR tests, not the two that were parked:
    `run_complete_is_stamped_on_the_receiving_sockets_clock` and
    `a_short_accelerated_run_is_not_over_before_it_is_ready` had the same shape
    and had simply not lost yet, the latter with 0.3 s of wall - the tightest
    window in the family.
  - AND THE FAMILY IS PACED NOW, through `common::bounded_run_config` and
    `tests/configs/bounded-run.toml`, which is `fast.toml` with `speed = 1.0`.
    THIS IS THE ONE TO CARRY TO THE ADAPTER DOCUMENT, because it is a property of
    the venue rather than of these tests: on an unpaced venue the terminal frame
    is queued BEHIND the whole backlog the run generated flat out, and a client
    has to drain the backlog before it can see the frame. With the premise fixed
    the gate failed again, truthfully, naming 1,475,111 frames served on a socket
    that never got its announcement. Any test that waits for a frame the venue
    writes AFTER a span of unpaced tape is on this trap - a terminal frame, a
    late execution report, anything at the tail.
  - THE HELPER SHIPPED THE DISEASE IT TREATS, TWICE, and cold review caught both
    the same day. Carry the SHAPES, because the adapter's socket binaries will
    be written by the same hands:
    - A GUARD MAY NOT MEASURE SUCCESS AGAINST WHAT IT ACHIEVED. The first
      version compared the drained count against `sockets.len()`, so on the
      losing branch it exists to detect - the very first connect refused, so no
      sockets at all - the test reduced to `0 == 0` with `all` over an EMPTY
      ITERATOR, which is `true`. It returned success carrying nothing and every
      caller index-panicked on `seen[0]`, which is the unattributed failure the
      helper was written to remove, reachable on exactly the race its docstring
      describes. THE WANTED COUNT COMES FROM THE REQUEST, and an empty request
      is refused rather than passed. `all`/`any` over a collection whose
      emptiness is the failure mode is a defect on sight.
    - CONTROL FRAMES ARE NOT SESSION EVIDENCE. The counter incremented before
      the match, so a `Close` from a venue already tearing down - or a peer Ping
      - counted as one frame and the caller then asserted "the venue had already
      served 1 frames on, so this was a live session", the precise falsehood the
      counter exists to rule out. It counts `Message::Text` only. Bite-checked
      both ways: with Text frames suppressed the old counting reports "17 content
      frames ... live session" while the fixed counting discards every run and
      names host load.
    - A RETRY BUDGET CHECKED BETWEEN ATTEMPTS IS NOT A CEILING ON THE LAST
      ATTEMPT. The 8 s attach budget sits inside a 13 s wall clamp, and an
      attempt that re-boots a six-simulated-hour warmup costs seconds, so
      "still under budget, go again" admits an attempt the clamp then refuses -
      replacing this helper's message with the clamp's. The check carries the
      LAST ATTEMPT'S MEASURED COST.
    - AND A THIRD, FOUND BY THE lab/cli r2 GATE RUN RATHER THAN BY REVIEW: THE
      ACCEPTANCE TEST DID NOT ASK HOW THE DRAIN ENDED. `run_complete_reaches
      _every_open_socket` failed one gate run and passed the next with
      "the first socket saw the completion announcement, after 15 content
      frames" - a paced 2 s MNQ run prints about 89, so 15 is roughly 0.4 s of
      it. `drain_to_completion`'s deadline is CLAMPED to the test's remaining
      wall budget, so an attempt begun late enough is cut off mid-run and
      returns a reading that passes the `content_frames > 0` premise and
      carries no announcement, which the caller then reports as a venue defect.
      The discard condition covered a lost ATTACH and not a truncated DRAIN.
      This is the file's own standing rule - A DRAIN THAT DOES NOT RECORD HOW
      THE STREAM ENDED IS NOT A DRAIN - in the one place it had not been
      applied: `while let Ok(Some(Ok(m)))` folded a close, a stream end, a
      transport error and a timeout into "the loop finished". `Watched` now
      carries an `Ending`, a `Deadline` ending discards the run exactly like a
      lost attach, and the give-up message prints the endings. Bite-checked by
      capping the drain deadline at 400 ms: 19 launches, every one discarded,
      and the failure that arrives names host load instead of the venue.
      The sigterm caller asserts the ending is not `Deadline` BEFORE its
      absence assertion, because a truncated drain satisfies "no announcement"
      for free.
  - WHAT SEPARATES TWO BOATS IS THEIR WALL ANCHOR, NOT THEIR SPEED.
    `boatyard.rs` gives every boat the same `sim_epoch_ns = origin_ns` and a
    `wall_anchor_ns = now_ns()` taken AT BOAT CONSTRUCTION, so two boats built at
    different instants read different sim-now at one wall instant.
    `bounded-run.toml` had claimed its second river was "placed at a different
    speed", which was false on its face - both are 1.0.
  - PACING BUYS A LIVENESS REQUIREMENT, so it was measured. Six runs at seed 42,
    deterministic to the millisecond: MNQ prints 89 content frames in a declared
    2 s, the first 171 ms after attach, longest gap 519 ms; the BTCUSDT boot
    river is the SPARSER of the two at 16, the first 1.031 s after attach, which
    is also its longest gap. The thin margin is the boot river's, about a second,
    not the second river's - which is where it was suspected. Any paced fixture
    whose test needs a frame inside a declared window owes the same measurement.
- The rule those helpers encode, which recurred often enough to be the round's
  main lesson: A DRAIN THAT DOES NOT RECORD HOW THE STREAM ENDED IS NOT A DRAIN.
  It is the guard-scope family in a new costume - the drain outlives nothing it
  reports on.

## The adapter document, round 1: machinery and rulings

- `tests/common/StubState` GREW THREE FIELDS, and a later round reaching for the
  same shapes should use them rather than build a second mechanism.
  - `ws_first_frame_at` is the wall instant the WS leg put its FIRST `ws_trades`
    frame on the wire. ANY LATENCY MEASUREMENT IN THESE BINARIES STARTS THERE,
    never at the return of `connect()`. Measuring from connect charges the
    client for everything the harness does between the upgrade and the push, and
    the harness does a lot there - `havoc_latency_delays_inbound_event` passed
    with `HavocLatency` ZEROED, satisfied entirely by stub time. It is also
    where a "nothing leaked early" window should END, and the two blackout
    tests do exactly that: poll until the stamp appears rather than picking a
    fixed duration off the return of `connect()`, which races the harness's own
    delays. The stamp is set strictly before the send, so observing it set is a
    sound place to stop looking.
    (`assert_only_instrument_prologue`'s own fixed window is a different case
    and is fine: its callers drop the whole tape, so no stamp is coming.)
  - `fail_health` makes `GET /health` answer 500, which is the ONLY way to
    represent `IdentityOutcome::Unreachable` end to end - the stub could
    previously model only a venue that answers, so the unanswerable branch had
    no fixture. `health_hits` is its companion and is not optional: a test
    concluding "the client did not refuse" must first establish that it ASKED,
    or a client that skipped the probe passes for free.
- A BARE `>= 0` LOWER BOUND ON INBOUND LATENCY IS NEVER ZERO. `BASELINE_LATENCY`
  is always on at `base_nanos = 30_000_000`, and armed `ClientHavoc.latency` ADDS
  to it rather than replacing it. Measured, not read: with the armed latency
  zeroed the honest floor came out at 31.7 ms. A test wanting "no delay" cannot
  get one, and a test wanting a discriminating bound must clear 30 ms.
- THE IDLE TIMEOUT IS WHAT A CLIENT DECIDES A BLACKOUT WITH, and before this
  round `ConnHavoc::idle_timeout_ms` had NO end-to-end coverage at all: deleting
  the `break` from `WsAction::Idle` in `lifecycle.rs` left all 115 tests of this
  crate green. That is now a two-sided gate in `havoc.rs` -
  `divergence_go_dark_within_the_idle_timeout_is_ridden_out` (blackout under the
  timeout: one handshake, client up, held frame delivered) and
  `..._past_the_idle_timeout_is_read_as_a_dead_socket` (blackout over it: at
  least two handshakes, nothing delivered before the re-dial, and the tape
  served once the blackout lifts). Each catches an injection the other passes.
  - THE GENERAL RULING BEHIND IT, which the next "this test only tests the stub"
    finding should be closed the same way: where the stub models a VENUE-side
    divergence, the test is not worthless, it is aimed at the wrong end. Ask
    what the CLIENT must decide when the venue behaves that way, and pin that.
    Deleting was the cheaper close and the worse one.
- A REFUSAL PATH'S TEST DROPS THE CONNECT RESULT rather than unwrapping it.
  `client.connect().await.expect(...)` in a test whose property is "this venue
  is NOT refused" fails as `connect websocket ... timed out` when the property
  breaks - a message naming the socket, not the check. Both identity tests in
  `havoc.rs` now `drop(client.connect().await)` and assert on
  `is_disconnected()`.
- A BLACKOUT FIXTURE MUST SEED A FRAME, or `dark_ms` is not load-bearing and the
  test pins the trivial case. This is the arc's signature defect - a fixture that
  does not exclude the shape it claims - and it REAPPEARED INSIDE THE FIX for it:
  the first cut of `..._past_the_idle_timeout_is_read_as_a_dead_socket` set
  `dark_ms = 600` over an EMPTY `ws_trades`, and `serve_ws` only sleeps `dark_ms`
  before draining that list, so the socket was application-silent forever
  whatever the blackout said. Deleting the `dark_ms` line left the test passing
  identically. The repair is a seeded trade plus an idle timeout STRADDLING the
  harness's 100 ms pre-push delay: without the blackout the venue speaks inside
  the idle window and no socket is ever declared dead. The general form: after
  writing a divergence fixture, delete the divergence and confirm the test goes
  red. Nothing else detects this.
- THE VENUE'S ACCOUNT ID IS A LABEL AND THE CLIENT KEEPS ITS OWN. A round-1 cold
  review argued `an_account_labelled_differently_is_still_served` should assert
  the emitted `account_id` equals the WIRE's `SANDBOX-042`, on the reasoning that
  a regression relabelling the snapshot would otherwise pass. The measurement
  overturned it: `handle_account_state` stamps `ctx.account_id` deliberately, and
  `note_account_label` logs the difference once at connect and moves on - one
  venue is one run is one ledger, so the id is documentation, not a key. The
  assertion that belongs there is the OPPOSITE one, `MOGWAI-001`, and it bites:
  emitting the wire id instead fails it. The seeded snapshot is now selected by
  its `9900` USDT balance rather than by being the first `Account` event drained,
  which is what the reviewer's id-matching was really reaching for.
- ROUND 3 OWNS THE WALL-CLOCK BOUNDS AND INHERITS ONE TIGHT ONE.
  `havoc_latency_delays_inbound_event` now measures from `ws_first_frame_at` and
  asserts `>= 50 ms` against a composed `BASELINE_LATENCY + armed` of 30 + 50 ms.
  The bound is a LOWER one, so a slow machine does not flake it; what is thin is
  its BITE. Zeroing the armed latency leaves the ~30 ms baseline, which clears
  the bound by only ~20 ms - so the injection that proves this test alive is
  itself the tight measurement. Left at 50 ms deliberately: choosing the bounds
  is round 3's call to make across the whole set, not a fix to smuggle in under
  round 1. If the bite ever needs widening, widen the ARMED delay, which moves
  the signal, rather than the assertion, which moves the goalposts.

## The adapter document, round 2: the harness push gate

- `tests/common/PushGate` REPLACED THE 100 ms PRE-PUSH SLEEP IN `serve_ws`, and
  ROUNDS 3 AND 4 BOTH STAND ON IT. A data client's subscription is satisfied
  ENTIRELY LOCALLY - `subscribe_trades` sends no wire frame - so the stub cannot
  observe it and no condition on the venue side exists; the TEST has to say when
  the push may happen. `state.push_gate.open()` is that statement, and the leg
  waits for it only when `ws_trades` is non-empty, so a leg with nothing to push
  never waits at all. It LATCHES, because a reconnecting client re-enters
  `serve_ws` and a one-shot permit would strand the second socket.
  - A STALLED GATE IS REPORTED BY THE TEST, NOT BY THE STUB, and the first cut
    had this backwards. `PushGate::wait` asserted on timeout - inside the
    per-connection `tokio::spawn` in `run_stub` whose `JoinHandle` is dropped, so
    the runtime CAPTURED the panic and the test never failed on it; worse, the
    unwind dropped the socket, the client read a dead venue and re-dialled into a
    fresh leg that waited and panicked again, so a forgotten `open()` was a
    reconnect storm ending in a downstream timeout. Now `wait` returns a bool, a
    timeout sets a thread-local (every test in these binaries is
    `flavor = "current_thread"`, so the stub's tasks run on the test's own
    thread), the leg pushes nothing and stays up, and `common::assert_push_gate
    _opened()` panics naming the gate. It is called from `next_data_event` and
    from every poll helper in `havoc.rs`, which is what makes the gate the FIRST
    thing to speak. NOTHING DETECTS A WAIT SITE THAT FORGOT THE CALL: a `Drop`
    backstop on `StubState` was built to be that detector and MEASURED NOT TO
    WORK - tokio catches panics in task destructors too, and the bite-check
    printed the panic under a test libtest still reported as passing. It was
    deleted rather than kept as a verdict nobody delivers.
  - `PUSH_GATE_DEADLINE` IS THEREFORE ONE-SIDED, and 1 s is generous rather than
    squeezed. The legitimate wait is one `connect()` return plus a synchronous
    subscribe - single-digit ms - and the upper side is only how long a FAILING
    run spends before it records the stall. It does NOT have to beat any test's
    own deadline; the earlier note here claiming it did was written against the
    swallowed-panic design and was false in both directions - that design always
    named the data event.
  - THE COST OF THE OLD SLEEP WAS ~4 s OF THE BINARY'S 42 s, not the wall. It
    fired on every `/ws` upgrade including the exec legs, so ~40 sockets paid it
    while only ~13 tests seed frames. Measured before and after:
    `brokkr test -p mogwai-adapter "" --debug` 42.46 s -> 37.47 s per sweep,
    37.61 s once the round's review fixes landed.
    THE REMAINING ~37 s IS SOLVED - see the round-5 section below. It was a
    ~420 ms floor under every connecting test, not a distribution of slow ones.
    Do not re-open it as an open question; the sweep is 12 s.
  - IT MOVED THE GO-DARK TEST'S GROUND AND THE GROUND GOT FIRMER. Round 1 chose
    `idle_timeout_ms = 250` to STRADDLE the 100 ms pre-push delay; with the gate
    the pre-push interval is not a wall duration at all, so the seeded trade
    would land essentially at the subscribe and the separation is wider, not
    narrower. Round 1's bite-check was re-run after the change and still fails
    correctly: deleting the `dark_ms` line fails
    `divergence_go_dark_past_the_idle_timeout_is_read_as_a_dead_socket` on its
    own named assertion ("the blackout, not a served-then-idle socket, must be
    what cost the re-dial").
- ONE RESIDUAL THE FIX COULD NOT CLOSE, recorded rather than papered over.
  `a_host_subscribing_quotes_after_connect_receives_the_book_immediately` now
  establishes the WIRE half of its ordering (the quote is on the wire before the
  subscribe, polled on `ws_first_frame_at`), which the old `sleep(50ms)` never
  did - that sleep was racing the harness's own 100 ms and was ALWAYS too short.
  What no test can establish is the CLIENT half: whether the reader had filed the
  quote in the pre-subscription cache before the subscribe. The client exposes no
  observable for it, and a quote delivered LIVE to a subscription that beat it
  satisfies the same assertion. Closing this needs a client-side observable, not
  another wait.
- `is_disconnected()` IS `!connected`, IT STARTS FALSE, AND `lifecycle` STORES
  FALSE ON EVERY FAILED DIAL. So on any refuse-the-upgrade fixture it reads
  "disconnected" from the first instant of the test and no assertion built on it
  can fail. `conn_reconnect_respects_max_attempts` asserted it twice, the second
  time inside a `while !client.is_disconnected()` poll whose condition was
  therefore false on entry - loop body and named message unreachable in every
  run, a verdict the change ADVERTISED and never observed. The only observable
  separating "gave up" from "still trying" on that path is the dial counter.
  Where the flag IS meaningful is after a SUCCESSFUL connect, as in
  `dialing_blind...`, and even there it needs a window rather than a snapshot.
- `conn_reconnect_respects_max_attempts`'s `(3..=4)` TOLERANCE IS GONE and the
  300 ms negative window stays. ITS LOWER BOUND IS POLLED, not inherited from
  the connect timeout: the exact-3 window opens only after
  `wait_for_at_least(&ws_handshakes, 3, 2s)` returns, because the ladder in
  front of the third dial - a `/clock` fetch with retry sleeps, an instrument
  seed, three loopback dials, two 30 ms backoffs - can outlast the 1 s connect
  bound on a loaded box, and the window would then fire on `saw 1` and read as
  the defect. Bite-checked both sides: cap 2 fails "saw 2", cap 4 fails "saw 4".
  The tolerance made the window vacuous against the
  defect it was written for: a fourth dial passed whether it arrived at 10 ms or
  310 ms, so the wait bought nothing. Three is EXACT because the ladder admits no
  other count - `exhausted(0)` is false, the dial fails, `backoff_or_exhausted`
  bumps to 1 and 2 and returns true at 3, and the stub counts each dial before it
  drops the socket the client is waiting on. Measured 20/20 at exactly 3.
  - AND ITS 2 s CONNECT BOUND WAS A BUDGET IT ALWAYS SPENT. Readiness never
    arrives on this path, so the timeout WAS the runtime; it is 1 s now with the
    count polled for afterwards, and the test went 2.44 s to 1.44 s. THE SHAPE
    GENERALISES to any test that bounds a future which cannot succeed: that bound
    is on the passing path, not the failing one.
- A CONDITION POLL IS NOT AUTOMATICALLY AN IMPROVEMENT ON A SLEEP, and
  `dialing_blind_establishes_a_full_session_with_a_stranger` shows both ways it
  is not. `ws_requests` is pushed at the TOP of `serve_ws`, before the upgrade
  bytes are written, and `connect` returns only once the client reports
  connected - so polling for the upgrade record returns on its first look and
  establishes nothing the previous line had not. And the deleted `sleep(600ms)`
  WAS load-bearing: it made `!is_disconnected()` a claim about a session that
  had survived, and asserting it at t~0 let a stranger that upgrades and
  immediately drops the socket pass. The replacement is a 200 ms window over
  `active_ws == 1` and the client's connected state, which is the condition the
  sleep was standing in for. GENERAL FORM: when replacing a fixed wait, ask what
  the DURATION was buying, not only what the wait was waiting for.

## The adapter document, round 3: the wall-clock bounds

THE ROUND'S ONE MOVE, and it settled every bullet: WIDEN THE SIGNAL, NOT THE
ASSERTION. Four of the five bounds were not tight against the CLOCK at all -
measured, they sat 20x to 60x clear of their ceilings on an idle box - they were
tight against THE DEFECT THEY DISCRIMINATE, and three of them could not
discriminate it. Retuning a constant in that situation moves the goalposts
toward the defect. Separating the two outcomes further costs wall time and buys
headroom on both sides at once. THE WHOLE ROUND COST ~200 ms: the serial sweep
`brokkr test -p mogwai-adapter "" --debug` went 37.61 s to 37.80 s, with the two
sweeps of the same commit 70 ms apart, so the round's spend is at the edge of
what this measurement resolves.

- `StubState` GREW `ws_first_exec_frame_at`, the exec leg's twin of
  `ws_first_frame_at`, stamped before the first `ws_exec_frames` send. REACH FOR
  IT rather than building a third anchor.
  - THE REASON IT HAD TO EXIST IS A FACT ABOUT THE EXEC LEG THAT BITES ANY
    LATENCY TEST ON IT: `connect()` does not return until
    `await_account_registered` sees the seeded account snapshot, `AccountState`
    is `EventKind::Exec` (same bucket as `OrderAccepted`/`OrderTriggered`), so an
    armed exec delay IS PAID ONCE INSIDE CONNECT before an order is submitted.
    `havoc_reaches_the_order_a_trigger_produces` measured setup at 416.7-418.7 ms
    over 40 runs against its own `triggered_at >= 400ms` lower bound - the bound
    was satisfied by setup alone in every run and could not fail. The report
    called this "the margin is not what it reads as"; it was vacuous. Anchored at
    the send it is live, bite-checked at 72.8 ms with the exec hold zeroed, and
    its upper bound moved 3 s to 2 s (honest value ~473 ms, defect 4,073 ms,
    bite-checked by misfiling `OrderTriggered` as `EventKind::Data`).
- ROUND 1'S 50 ms BITE-MARGIN QUESTION IS SETTLED, AND NEITHER WAY IT WAS
  FRAMED. `havoc_latency_delays_inbound_event` asserted `>= 50 ms`, which is the
  ARMED half alone; the contract is `BASELINE_LATENCY + armed` = 80 ms. The
  assertion was under-stated, not the injection under-sized, so widening the
  armed delay (round 1's recommendation) would have bought margin by spending
  wall time on a bound that was still wrong. It now DERIVES the sum -
  `BASELINE_LATENCY.delay_for(EventKind::Data) + armed.delay_for(...)` - so
  neither half can drift a literal out from under it. Bite margin 20 ms -> 50 ms
  (zeroed armed delivers at 30.5 ms) at ZERO wall cost. It cannot flake low: the
  pump sleeps to an arrival-anchored deadline and the stamp precedes the send.
- `reconnect_backoff_throttles_...` HAS AN ESCALATING LADDER NOW (factor 2.0,
  initial 100, max 1000), so the two legal outcomes are 300 ms and ~700 ms rather
  than 200 and 300, and the window is `250..500`. Measured before: 202.9-204.6 ms
  over 40 runs, i.e. the three dials, three WS handshakes and three task spawns
  cost 0.3-4.5 ms, not the 100 ms the old window budgeted. Bite-checked BOTH
  sides as text edits in `backoff_or_exhausted`: sleeping the final backoff gives
  703 ms, deleting the sleep entirely gives 294 us.
- `alert_timer_fires_with_sim_event_timestamp` HAD TWO BUDGETS AND NEITHER COULD
  SEE THE PROPERTY. It armed 20 ms of sim at speed 10 - 2 ms of wall, measured
  4.3-8.0 ms over 60 runs - and asserted `< 50 ms`, but a timer that IGNORED
  `speed` and slept the sim interval raw lands at ~20 ms and passes. It arms
  500 ms of sim now (50 ms of wall) under ONE ceiling of 250 ms, which the poll
  loop itself carries; the trailing assertion is gone. Bite-checked by
  substituting `speed = 1.0` in `sleep_until_sim`: fails at 252 ms.
- `latency_pump_pipelines_a_burst_instead_of_serializing` is `< serial / 4`
  (300 ms) rather than `< per_msg * 3` (90 ms). Measured 31.1-31.6 ms over 60
  runs against a 1.2 s serial alternative, so the bound moved an order of
  magnitude away from the pipelined side and stayed 900 ms clear of the defect.
  - AND THE HOLE THE ROUND FLAGGED IN ITSELF DOES NOT EXIST. The fix pass
    reported that a pump re-anchoring each deadline at the PREVIOUS RELEASE would
    pass this test unchanged, because a burst enqueued at one instant makes the
    two anchors coincide, and asked a later round for a staggered-arrival twin.
    MEASURED, and the argument is wrong: the anchors coincide for the FIRST
    message only, chaining releases gives `i * per_msg`, and the defect installed
    as a text edit in `havoc_deadline` fails THIS test at 1.202 s against its
    300 ms bound with the burst test run alone. The staggered twin was built
    anyway before the measurement - N=20, 5 ms apart, honest 127-129 ms over ten
    runs against the same 600 ms compounded defect - and DELETED as strictly
    weaker: twenty windows of separation where the burst has forty, for 128 ms of
    added wall. Per-message spacing and compounding deadlines are one failure
    here, because the only way to space the output is to stop anchoring at
    arrival. The reasoning is now in the test beside the bound so it is not
    re-derived a third time.
- REFUSED: shrinking the 400 ms / 4 s havoc buckets to halve
  `havoc_reaches_the_order_a_trigger_produces` (measured 0.94 s, not the report's
  1.5-2 s). Halving them saves ~440 ms and cuts the lower bound's discrimination
  against the 30 ms baseline from 13x to 6.7x. The buckets ARE the signal; this
  round spent wall to widen signals, and spending it back here for the largest
  one would be incoherent.
- A LEAD ON THE UNEXPLAINED ~37 s, FILED HERE AND THEN OVERTAKEN: the
  account-snapshot wait puts at least one full inbound-latency window inside
  EVERY exec client's connect - the 30 ms baseline at minimum. Real, and NOT the
  answer; round 5 measured the distribution and the cost was elsewhere. Kept
  only because the lead is still true of the exec leg and a later latency test
  on it has to budget for the window; the accounting question it was raised
  against is closed.
- THE ROUND'S OWN COLD REVIEW FOUND NO DEFECT IN ITS CODE, the first time in the
  arc, and independently re-derived all four mechanisms against
  production source. It found three WORDING defects instead, all in the second
  half nobody else reads, and all of the same family the round had just closed -
  a comment that says something the code does not do:
  - `ws_first_exec_frame_at`'s doc did not say it is SINGLE-SHOT ACROSS THE WHOLE
    `StubState`, though `get_or_insert_with` makes it so and its data-leg twin's
    comment does say it. A later test submitting two orders, or letting the exec
    leg reconnect, would silently measure from the first submit. Now stated on
    the field, because the entry above invites later rounds to reach for it.
  - Both its doc and the stamp site said the stub "put the frame ON THE WIRE",
    while the stamp is taken BEFORE `ws.send`. Conservative for a `>=` bound - an
    earlier anchor only inflates the measured interval - but that makes the bound
    very slightly WEAKER, which is the opposite of what the phrasing implies.
  - `alert_timer_fires_with_sim_event_timestamp`'s new comment read "roughly 5x
    of headroom" as though it were an improvement. It is not: RELATIVE headroom
    SHRANK, 6-12x to ~4.6x, while ABSOLUTE slack grew 42 ms to 196 ms. The
    improvement is that the bound can see the property at all. Both figures and
    the trade are now written down, because the next reader tuning this will
    otherwise read the ratio as the thing that got better.

## The adapter document, round 4: the harness itself

- `common::read_request` LOOPS BOTH READS NOW. The head loop was missing: one
  `read` into a 4096-byte buffer, and `None` if `\r\n\r\n` was not in it, which
  DROPS THE CONNECTION WITH NO DIAGNOSTIC - so the symptom is a connect failure
  attributed to the adapter. Two shapes reach it and neither is exotic: a head
  straddling a segment boundary, and a head over 4 KiB (one `Authorization`
  header or a cookie jar). The rescan is from byte zero each pass, deliberately,
  because the terminator itself can straddle the boundary. A `MAX_HEAD_BYTES`
  cap of 64 KiB bounds a peer that never terminates.
  - PINNED BY `the_harness_reads_a_request_head_split_across_segments` in
    `data_client_transport.rs`, which writes a 6 KB head in two segments split
    INSIDE the terminator. It is in a test binary rather than beside the helper
    on purpose: `tests/common` compiles into four binaries and a pin there would
    run four times for one property.
  - ITS CLIENT-SIDE WRITES ARE DELIBERATELY NOT `expect`ed, and the first draft
    got this wrong. A reader that gave up has already dropped its socket, so the
    second write fails ECONNRESET and the test reports `second segment: Os {
    code: 104 }` - an error naming the write, not the property. Bite-checked
    both ways: with the writes `expect`ed the revert fails on ECONNRESET, with
    them dropped it fails on "a segmented head must be read, not dropped as an
    unparseable request". Same family as the standing rule that a test observing
    only an ERROR cannot distinguish a bound from a check.
- THE DEAD `ModifyOrder` TAIL IS GONE. The block held the
  `close_after_trades` re-serve guard, the
  `dark_ms` sleep, the server-ping probe and a close-and-return, written as
  though the read loop were the data path. TWO OF THE THREE UNREACHABILITY
  ARGUMENTS ARE STRUCTURAL rather than facts about today's tests, which is what
  made deleting it safe: `close_after_trades` returns from `serve_ws` BEFORE the
  read loop is entered, so its guard could not run under any fixture; and NO
  DATA CLIENT SENDS A `ClientMessage` AT ALL (only `ExecWsCommand`s become
  frames, `client/exec.rs`), so no socket with `dark_ms` or `ws_server_pings`
  armed reaches that code by any route a client can take. The three switches are
  set only in `havoc.rs`, all on data clients; `ws_modify_frames` only in
  `adapter_smoke.rs`.
- THE DELETION EXPOSED A LIVE DEFECT ONE LAYER UP, and the round's cold review
  caught it: `served_once` was the only code that even PURPORTED to implement
  `close_after_trades`'s documented "on any later reconnected subscribe the leg
  serves nothing", and deleting it left the claim standing over nothing. THE
  SURVIVING DATA LEG RE-READ `ws_trades` ON EVERY UPGRADE - and the close it had
  just sent is exactly what makes the client re-dial - so the stub sat in a
  re-serve/close loop for the whole test. `havoc_reorder_swaps_adjacent` passed
  over it because it stops reading after three trades. THE GATE IS RESTORED ON
  THE DATA LEG rather than the sentence deleted, and its meaning is now stated
  in one place: serve the batch and close ONCE, then serve nothing and STAY UP,
  so the reconnect is a live silent socket rather than another close. Staying
  up is the part that ends the loop; closing again would have kept it. Pinned by
  `havoc::a_close_after_trades_leg_does_not_replay_its_batch_on_the_reconnect`,
  which establishes the re-dial from `ws_handshakes` FIRST - without a reconnect
  there is nothing to replay and the silence assertion passes for free.
  Bite-checked by forcing the replay flag false: the batch is replayed and the
  test names it. THE SHAPE IS NEW FOR THIS ARC and worth carrying: not a guard
  that cannot fail, but a documented behaviour whose implementation was removed
  while its claim and its dependent test stayed. A green test is not evidence
  that the stub underneath it is quiet.
- `StubState::hang_orders` WAS DEAD AND IS GONE. It modelled the order POST
  handler accepting and never replying; `POST /orders` went with the HTTP
  transport profiles, so nothing sets it and nothing reads it. The
  `#![allow(dead_code)]` on `common/` is why the compiler never said so - the
  same decay the facts section below warns about, found by reading rather than
  by tooling. `http_request_times`'s doc claimed to time `/orders` too and now
  says what it actually holds.
- `MAX_HEAD_BYTES` IS AN EXACT BOUND ON WHAT IS ACCUMULATED, because the read is
  CLAMPED to the remaining room. The first cut checked the cap before the read
  that grows the buffer, making the true bound `MAX_HEAD_BYTES + 4096` and
  firing one pass late - cosmetic for a stub, but the comment stated the bound
  as exact, and a comment that overstates a bound is the same defect family this
  round spent itself on.
- THE SEGMENTED-HEAD PIN CARRIES ITS OWN TIMEOUT. A future regression could
  express "the reader gave up" as a BLOCK rather than a `None`, and an unbounded
  `server.await` would then hang to the libtest watchdog with no attributable
  verdict - the same reasoning the test already applies to its client-side
  writes. Bite-checked both ways: the single-read revert fails on "a segmented
  head must be read", a `pending()` in the loop fails on "the request reader
  must finish rather than block on a segmented head".
- THE `HttpStub` / `WsStub` SPLIT IS REFUSED, on two measured grounds rather
  than on cost. DO NOT REOPEN IT WITHOUT ANSWERING BOTH.
  - THE AXIS IS WRONG AND WOULD NOT HAVE CAUGHT THE DEFECT. Every field the dead
    block touched - `ws_trades`, `dark_ms`, `close_after_trades`,
    `ws_server_pings`, `ws_modify_frames` - is a WS field, so a transport split
    puts all of them in one bucket together. The causal claim ("the flat struct
    is why the dead branch survived") fails against its own remedy. The axis that
    DOES separate them is data-leg versus exec-leg, and it is now written on
    `StubState`'s doc comment as a three-way ownership table with the rule that a
    new field belongs in exactly one of them.
  - TWO HANDLERS CHOSEN AT THE HANDSHAKE ARE NOT IMPLEMENTABLE. Read from
    `config.rs`: `MogwaiDataClientConfig` and `MogwaiExecClientConfig` build the
    SAME `/ws?account=...&session=...` URL (`symbol=` is optional on the data
    side and absent by default), so the request line does not distinguish the
    legs and the stub has nothing to dispatch on until a `ClientMessage`
    arrives - which is what the code already did.
  - WHAT WAS DONE INSTEAD, the part of the proposal with real content: the exec
    leg's whole behaviour is extracted into `serve_exec_message`, generic over
    the socket, so the data leg is visibly ALL ABOVE the read loop and the read
    loop reads as "count control frames, record the text, hand it to the exec
    handler". Pure code move, no behaviour change; bite-checked by emptying the
    extracted `ModifyOrder` arm, which fails
    `a_trigger_amend_on_a_triggered_stop_limit_keeps_it_triggered`.
- THE ROUND COST ~1.9 s OF WALL, ALL OF IT ONE TEST, and the accounting is
  exact enough to state: the serial sweep `brokkr test -p mogwai-adapter ""
  --debug` was 37.80 s at the end of round 3, 37.81 s after the fix pass (whose
  extraction and head loop cost nothing measurable, as expected of a code move),
  and 39.71 s once the replay pin landed - which measures 2.03 s run alone,
  because it waits out a real client reconnect and then a 400 ms silence window.
  That is the price of seeing the loop at all; nothing cheaper observes it. The
  unexplained ~37 s is
  untouched and the handler split did NOT illuminate it - the extraction moved
  no await, and the exec leg's cost is the account-snapshot wait inside
  `connect()` that round 3 already identified, not the dispatch.
- LATERAL, AND IT IS ROUND 5'S ITEM ALREADY: the `ModifyOrder` bite-check failed
  as `execution event arrives: Elapsed(())` at `common/mod.rs`, naming neither
  the test nor what was expected - a live instance of the `next_exec_event`
  message bullet in the report's "Smaller notes". It was observed here, not
  fixed here.

## The adapter document, round 5: what carries to `bugs-tests-tape`

The adapter-specific mechanics (`PushGate`, the two frame anchors,
`close_after_trades`, `serve_exec_message`, `fail_clock`) die with that
document. What generalises:

- MEASURE THE DISTRIBUTION BEFORE PROPOSING A STRUCTURAL WIN. Four rounds
  carried "~37 s unexplained, the crate's largest cost" and every proposal
  against it was a guess. One instrument -
  `scripts/adapter_test_walls.py`, which runs already-built libtest binaries one
  test per process and times each - answered it in a single sweep's wall, and
  the answer was not a slow test anywhere. IT WAS A FLOOR: 55 of the 59 then in
  those binaries sat in a
  419-892 ms band with a hard ~420 ms bottom, and the only two that never
  `connect()` at 15 ms and 23 ms. A FLAT FLOOR IS A FIXED COST IN A SHARED
  SETUP PATH, and it is invisible in every aggregate. The script is generic over
  libtest binaries, so the tape document can point it at those.
  - The cause was a HARNESS FIXTURE MODELLING A BROKEN VENUE BY DEFAULT: the
    stub answered `GET /clock` with an undecodable `[]`, and the client's
    three-attempt, 200 ms-apart retry ladder ran inline in `connect()` on all 57
    connecting tests. Serving a real envelope is both faster and a MORE HONEST
    fixture - the real venue answers that route. 39.71 s -> 15.93 s. ASK WHAT
    THE DEFAULT FIXTURE CLAIMS THE WORLD LOOKS LIKE; a default that models a
    fault makes every test pay the fault's recovery path.
  - AND THE DELETION'S HOLE ONE LAYER UP WAS REAL: 57 accidental traversals of
    the fallback branch asserted nothing about it, but they were its only
    coverage. Removing the accident owes the branch ONE deliberate test. Here
    that is a `fail_clock` switch plus a test counting the attempts - which
    also pins the ladder itself, which nothing had.
  - WITH THE FLOOR GONE THE DISTRIBUTION NAMED A REAL OUTLIER, which it could
    not before: one test at 5.0 s, spending `wait_connected`'s whole readiness
    bound because readiness cannot arrive on a terminal refusal. Same shape as
    round 2's `conn_reconnect_respects_max_attempts` - A BOUND ON A FUTURE THAT
    CANNOT SUCCEED IS ON THE PASSING PATH - repaired the same way, bound the
    connect and poll the observable afterwards. 15.93 s -> 11.83 s.
- A VACUITY FINDING IS ABOUT A FIXTURE, NOT ABOUT AN EXPRESSION. Round 2 filed
  `is_disconnected()` as a dead assertion class and named a second instance;
  measured, that instance is the ONLY assertion in its test that catches the
  defect. The flag is vacuous where the stub refuses the upgrade, because
  nothing can set it; where the stub serves a good socket and the client refuses
  LATER, the same expression discriminates. Bite-check the named instance rather
  than sweeping by grep - deleting it would have unpinned the whole venue
  identity refusal.
  - The sweep did find two genuine ones elsewhere, both of the form "an
    assertion about a value the setup already guarantees": a counter asserted
    `>= 1` after a connect that must have moved it, and
    `assert!(!x.to_rfc3339().is_empty())`, which no timestamp can falsify. THE
    SHAPE TO GREP FOR IS A PREDICATE WHOSE FALSE BRANCH IS UNREACHABLE FOR THE
    TYPE, not a particular function name.
- A SHARED WAIT HELPER OWES ITS CALLER'S EXPECTATION IN ITS PANIC. One helper
  called from ~30 sites reported `execution event arrives: Elapsed(())` at its
  own file, so every one of those timeouts read identically; it cost round 4
  real time. libtest names the TEST for free (the thread carries the name), so
  what the helper has to be given is WHAT was awaited. Also separate a closed
  channel from a timeout, and print the duration.
- A DURABLE STATEMENT IS OWED WHERE A DESIGN IS COUNTER-INTUITIVE AND ONLY
  COMMENTS CARRY IT. The venue's account id is a LABEL the client does not
  adopt; a cold reviewer proposed the exact inverse assertion and only a
  measurement stopped it. It lived in two function doc comments and now lives in
  `reference/architecture.md`. The test to apply: could a competent reviewer
  reading only the durable docs derive the opposite? Then it is not documented.
  - AND SCOPE THE CLAIM TO WHAT IS ACTUALLY TRUE. The paragraph rested on "one
    venue is one run is one ledger", which reads as "only one ledger can exist"
    - and the venue in fact seats several, keyed by account plus session. The
    argument survives only at CONNECTION scope, and a durable paragraph written
    to stop a reviewer deriving the inverse has to be exactly right or it hands
    them the inverse premise instead.
- A ROUND WHOSE SUBJECT IS A DEFECT CLASS WILL COMMIT AN INSTANCE OF IT. The
  vacuity sweep's own new test was half-vacuous: it claimed an unknown floor
  admits what a known floor refuses, on a fixture where the only floor either
  path yields is ZERO, and `start < origin` over a `u64` makes zero and
  "unknown" observationally identical. It passed with the guard text-edited to
  `false`. So did the pre-existing test on the other side, which asserted an
  empty response over an EMPTY tape - an empty answer being exactly what a
  venue with no rows returns. TWO RULES CARRY:
  - A CONTRAST TEST MUST BE ABLE TO PRODUCE THE CONTRAST. Ask what values the
    fixture can actually put on both sides of the comparison; where one side is
    a type's floor, there is no other side.
  - BITE-CHECK BY BREAKING THE PRODUCTION GUARD AND RUNNING THE WHOLE
    NEIGHBOURHOOD, not only the test just written. Both defects here surfaced
    from one `&& false` edit, and the second was four rounds old.
- A COUNTER RESET AT ENTRY BUYS "SINCE ENTRY", NOT "THIS RUN", wherever the
  shared state outlives the reset. Say which one the doc means; the gap between
  them is where the next wrong verdict comes from.
- A FIXED FILESYSTEM PATH IN A TEST IS A CROSS-PROCESS RESOURCE, because the
  gate runs several sweeps of the same package at once. A write-then-exec on
  one intermittently fails `ETXTBSY`, and the obvious fix - the test's own name
  in the filename - changes NOTHING, since both processes compute it. Only a
  pid or a tempdir closes it. THE PID IS NECESSARY AND NOT SUFFICIENT, WHICH
  IS WHY THE FAILURE CAME BACK ON THE TAPE DOCUMENT: a write-then-exec has a
  SECOND, INTRA-PROCESS window that no unique path can touch. Rust opens files
  `O_CLOEXEC`, so a concurrently forking sibling test drops the inherited write
  descriptor at ITS exec - but not before it, and an exec of the script in that
  window is `ETXTBSY` all the same. `launch.rs` has three sibling tests that
  spawn children, running in parallel by default. The close pass RETRIES on
  that one errno rather than pretending the window is gone; the window is a
  fork-to-exec latency, so the retry budget dwarfs it and a genuine wedge still
  fails, because nothing else is retried. WRITE-THEN-EXEC IN A TEST OWES BOTH
  HALVES. Note also that a single crashed test aborts the
  sweep and makes brokkr report a whole package's tests as ORPHANED, which is
  indistinguishable in the output from the known tool bug: check for a crash
  before blaming the tool.

## The adapter document, close pass: what the five commits left

The arc is SOUND - the gate is green, the serial sweep reproduces at 11.96 s,
and the headline coverage claim was re-measured rather than taken on trust
(removing `default_session`'s default fails
`both_legs_disclose_one_process_session_on_the_upgrade` on its own message).
Three findings, all prose or fixture, none in production behaviour.

- THE TWELFTH COSTUME WAS A NEGATIVE ASSERTION.
  `havoc::assert_only_instrument_prologue` scored a 400 ms silence starting at
  the caller's own line, so a run where the stub had not yet pushed would have
  passed having observed nothing - "the client suppressed the tape" and "the
  tape was never sent" being the same silence. It now waits for
  `ws_first_frame_at` first and FAILS if no frame ever went out. Bite-checked by
  emptying the seeded tape: the old form passes, the new one fails by name.
  THE RULE, and it is the general one for this family: A NEGATIVE ASSERTION
  OWES A POSITIVE PRECONDITION. Round 1 wrote this exact advice about this exact
  helper and it was never applied; the entry above now describes the code.
- THE PREMISE THE ROUND-5 CORRECTION RETIRED SURVIVED IN TWO SOURCE COMMENTS.
  "One venue is one run is one ledger" is false - a venue seats several ledgers,
  keyed by account plus session - and round 5 rewrote
  `reference/architecture.md` to rest on CONNECTION scope instead. But
  `client/exec.rs`'s `handle_account_state` and
  `adapter_smoke::an_account_labelled_differently_is_still_served` still carried
  the retired premise, in the two places a reader arrives at when checking the
  architecture paragraph. A durable paragraph written to stop a reviewer
  deriving the inverse is worth nothing if the code it describes hands them the
  inverse premise; both now state the connection-scope argument and point at
  the doc. FIXING A DURABLE DOC IS HALF THE JOB - grep the claim.
- `common/mod.rs` CLAIMED THE SESSION PIN IS "THE ONLY THING IN THE CRATE THAT
  FAILS" when the default is removed. It is the only SOCKET test that does;
  `config`'s own unit test fails too, and being a unit test it aborts cargo
  before the four socket binaries run at all, so the naive bite-check reports a
  failure that says nothing about the wire. Corrected in place, with the
  invocation that actually works.

LATERAL, UNOWNED BY ANY DOCUMENT IN THE ARC:

- `crates/mogwai-server/src/config.rs`'s `account_id` doc still opens "One venue
  is one run is one ledger", the same retired premise, about the SERVER side.
  Out of every arc's scope so far. It is a one-line correction whenever
  something touches that file.
- `target/tmp`'s `d-*.log`, `bad-*.toml` and `stale-*.pid` debris is CLOSED by
  the tape close pass, and it was never a live leak: every one of those files
  is dated 2026-08-01 or 2026-08-02 and their writers went with the daemon era
  (the PID file, the log file, the `stop` subcommand). Nothing in the tree
  writes that shape any more, so the pile was stale build-tree debris rather
  than accumulation, and it was deleted. What DOES land there now is
  `common/mod.rs`'s `Scratch`, whose `Drop` removes it except on a panic (kept
  on purpose, for triage) or a kill - which is the whole explanation for the
  `characterize_*-<pid>` directories a reader will find. `cargo clean` collects
  those; do not read them as a leak.

## The tape document, round 1: the gate runs ignored tests, and one of them wrote

- `crates/mogwai-data/examples/regenerate_arrival_transcript.rs` IS THE
  AMENDMENT-ONLY TRANSCRIPT REGENERATOR, and it may not go back into a test
  binary. It was `regenerate_arrival_transcripts_amendment_only`, an
  `#[ignore]`d test whose own doc comment justified itself on `#[ignore]`
  keeping it out of the suite. IT DID NOT: the gate profile sets
  `include_ignored` deliberately, so the regenerator ran on every
  `brokkr check --gate` and rewrote
  `tests/fixtures/arrival-transcript-shot_noise.json` - the file
  `arrival_transcripts_replay_bit_exact` pins through `include_str!`. Today's
  output has not moved, which is the only reason nobody saw it. An example
  target is COMPILED by every lane and RUN by none, which is the property
  `#[ignore]` was mistakenly thought to give.
  - THE PORT WAS VERIFIED BYTE-FOR-BYTE before anything else: running the
    example with the kernel untouched leaves the fixture git-clean. The three
    bits that had to move for it to live outside the crate are
    `SweepShape::new` going `pub(super)` to `pub`, the shot-noise params
    becoming a `const`, and the cadence tag being spelled out; nothing else.
  - IT REFUSES WITHOUT `--amendment "<citation>"` and exits 2. A regeneration
    tool that runs on a bare invocation is one shell-history recall from doing
    the damage by accident.
  - BITE-CHECKED AS THE SCENARIO ITSELF, both halves. With
    `-rng.random::<f64>().ln()` text-edited to `* 1.000_001` in `next_parent`,
    `brokkr test -p mogwai-data arrival_transcripts_replay_bit_exact --debug`
    (which passes `--include-ignored`, i.e. the gate's own shape) FAILS on
    `wall_mmpp/stage-a timestamp 0` and the fixture stays clean; running the
    example under the same perturbation rewrites the transcript's 10,000
    records, which is what used to happen inside that failing run. THE RECORD
    COUNT IS 10,000, not the 17,861 first written here - that number was the
    diff's LINE count, and `RECORDS` in the example and
    `transcript.records.len()` in the pin both say 10,000.
- `gate_skip_list::no_test_binary_writes_a_committed_fixture` IS THE ENFORCER,
  and it is deliberately NOT the rule the report asked for. "Every ignored test
  owes a skip entry" is FALSE and was refused with measurement: the four
  socket-backed adapter binaries and `mogwai-server`'s two `/trades` sizing
  instruments are all ignored, unskipped, and are exactly what `include_ignored`
  is for. And the cases where a missing entry IS a defect - a walk past the
  watchdog, a corpus no clone carries - make the gate go RED naming the test, so
  that direction is self-detecting. THE ONLY SILENT CASE IS A TEST THAT MUTATES
  THE TREE IT IS BEING JUDGED AGAINST, and that is what the check forbids: a
  write construct under `crates/*/src` or `crates/*/tests` reached with a
  `tests/fixtures` path in scope. Bite-checked by planting the old writer back
  in `arrival.rs`; it fails naming file and line.
  - `tests/golden/` IS OUT OF SCOPE ON PURPOSE, not by exception.
    `mogwai-server`'s `fill_distribution_matches_the_golden` writes its golden
    only when the file is ABSENT and panics after writing, so a fresh bless can
    never be green. That shape is safe; a fixture regenerator that overwrites
    unconditionally is not. If a golden ever grows an unguarded writer it owes
    its own rule rather than a widening of this one.
  - `strip_comments` IN THAT FILE NOW HAS A LITERAL-KEEPING TWIN, and the two
    callers want opposite things: the name scan needs literals BLANKED (every
    counter it runs reads punctuation), the fixture scan needs them KEPT (the
    path it forbids only ever appears inside a string, so blanking would make
    the check pass for free). Comments are stripped either way, which is what
    lets the file's own prose name the path it forbids. THE TWIN IS THE
    THIRTEENTH GUARD-THAT-CAN-STOP-GUARDING IN THIS ARC: the two functions
    differ by one bool, collapsing them is an obvious simplification, and doing
    it makes the fixture scan pass unconditionally forever while the
    source-file floor stays green because it counts FILES rather than matches.
    `parser::the_literal_keeping_stripper_keeps_literals_and_still_drops_comments`
    is the pin, and it fails within milliseconds when the twin is collapsed.
  - SCOPE IS THE ENCLOSING FUNCTION, NOT A BYTE WINDOW. The first cut searched
    1200 bytes BEFORE the construct, which was wrong in both directions at
    once: a module-level `const FIXTURE: &str = ...` - the idiomatic way anyone
    would actually write this - was invisible, and `arrival.rs`'s dozen
    `include_str!` fixture READS would have convicted any unrelated
    `File::create` landing a few hundred bytes later, naming the wrong line.
    The window is gone; `fn_body_spans` walks braces (skipping literals, which
    are kept in this view) and a construct is judged against every enclosing
    span. There is no tunable left to be wrong.
  - THE SCANNER IS EXEMPT FROM ITSELF, by one file-name match, and
    structurally: a scanner has to name every construct it forbids and the
    directory it protects, so its own source and its own fixture block match
    its own rule by construction. That is why the scan lives in a pure
    `fixture_write_offenders(shown, code)` pinned on synthetic samples in
    `mod parser` - three of them, one positive, one module-level, one negative
    - rather than resting on what the tree happens to contain.
  - THE CONSTRUCT LIST covers the tokio spellings for free, which a cold review
    read as a miss: `tokio::fs::write(` CONTAINS `fs::write(` and
    `tokio::fs::File::create(` contains `File::create(`, and the match is a
    substring search. `File::options(`, the remove pair and `Command::new(`
    were added; `write!`/`writeln!` deliberately were not, because they are
    overwhelmingly used to format into a `String` and flagging them would be an
    exception list on day one. The doc comment used to claim a generic
    `std::fs::` fallback that was never implemented - deleted, not widened.
- THE SAME FAMILY, REACHED LATERALLY:
  `arrival_control_refuses_a_tree_that_changed_during_the_run` plants
  `arrival-control-midrun-probe.txt` at the REPOSITORY ROOT and used to remove
  it on the straight-line path only, so a panic inside `run_with` or inside the
  planting closure unwound past the cleanup and left the probe in the tree.
  That test is the one AGENTS.md documents as refusing a dirty tree by design,
  so a leaked probe poisons every later run of the suite and presents as an
  unrelated refusal. It now removes through a `Drop` guard.
  - THE PROBE MAY NOT MOVE TO `target/`, which the review proposed. `target/`
    is gitignored, so a probe planted there would not dirty the tree at all and
    the pin would stop biting - the location is the mechanism. Untracked at the
    root is the requirement; RAII cleanup is the fix.

## The tape document, round 2: the gate's compute bill

- `crates/mogwai-data/src` CARRIES NO `hotpath` ANNOTATION. The crate's only
  `cfg(feature = "hotpath")` is in `examples/arrival_walk_bench.rs`, so the
  `instrumented` sweep's build shape differs from the default one in the
  dependency graph and that example and in NOTHING a test in this crate can
  observe - the same walk measures 7.65 s in one shape and 7.67 s in the other.
  That is why five tests now carry `#[cfg(not(feature = "hotpath"))]` and why
  that costs the sweep nothing: the sweep exists to prove a feature nothing
  compiles cannot rot, which is a COMPILE property, satisfied by compiling.
  `mogwai-lab` is NOT in the same position - it has two annotations in `src` -
  so do not carry this cut across to it without re-deriving it there.
  - THE MECHANISM IS OPTING OUT OF THE BUILD SHAPE, NOT FILTERING THE RUN, and
    the two are not interchangeable. The gate `certifies = "complete"`, so a
    filtered test is an ORPHANED pair and an error; a test that does not exist
    in a shape is no pair at all. Verified: 1688 pairs, 0 orphaned. There is
    also no per-sweep filter available - `skip` lives on the profile and hits
    both sweeps.
  - IT STRANDS THE HELPERS THE GATED TESTS ALONE REACH, which is the hole one
    layer up: `measure`, `measure_session_curves`, `windowed_latent_returns`
    and their result structs become unreachable in that shape. The file carries
    `#![cfg_attr(feature = "hotpath", allow(dead_code))]` for it - scoped to
    that shape ONLY, so the default shape still reports a helper whose last
    caller was deleted. A blanket allow would have hidden exactly that decay.
    A field-level `#[expect(dead_code)]` inside a struct that becomes dead
    ALSO breaks: rustc reports the struct, the field expectation goes
    unfulfilled, and that is an error. `Measured::max_gap_s` is
    `#[cfg_attr(not(feature = "hotpath"), expect(...))]` for that reason.
  - THE LINE IS ROUGHLY TWO SECONDS, measured one test per process by
    `scripts/adapter_test_walls.py`, which is generic over libtest binaries and
    was the adapter round's instrument. `mogwai-data`'s shape is the OPPOSITE of
    the adapter's: not a floor but seven real walks carrying 93% of the wall
    over a tail of 168 tests at a millisecond each.
- THE DWELL PAIR IS ONE TEST NOW. `run_seeded_tape_dwell_is_bounded` is
  DELETED and `dwell_is_bounded_across_run_seeds` is un-ignored, out of `skip`,
  and runs the EIGHT ARMS 0, 1, 2, 3, 4, 5, 6 AND 42 at `DRAW / 8`. Seed 42 is
  in the loop deliberately - it is the default run seed, so a sweep of the
  first eight integers would have traded the shipped realization for eight
  unshipped ones - and SEED 7 IS DISPLACED RATHER THAN JOINED, because eight
  arms is what multiplies `DRAW / 8` back to the two million parent events the
  wall-clock argument rests on. The list is written out in the source for that
  reason: every range-plus-chain phrasing of it was read as nine arms. The skip
  entry's stated
  ground ("outlives the 20-second watchdog by design") was false at 6.54 s, and
  its cost was that the repo's ONLY multi-seed dwell evidence was the test
  nothing ran.
  - THE FOUR DWELL BOUNDS, MEASURED, so the next reader does not re-derive them:
    `mean_gap_s` 0.1743-0.1785 short vs 0.17426 full against declared 0.17104
    and a 10% window; `gap_p999_s` 2.92-3.03 short vs 3.18 full against a bound
    of 10.65; `empty_hour_frac` and `max_empty_hour_run_h` EXACTLY ZERO at both
    draws. The last two are not vacuous - they are one-sided guards against
    silence, bite-checked at the short draw with a 30,000x `LiquidityDrought`
    (`empty_hour_frac` fails at 0.765 against 0.0105). But
    `max_empty_hour_run_h` IS genuinely weaker short: its bound is 7 h and a
    `DRAW / 8` arm spans only ~12 h, so it can fire only on a near-total
    blackout. It costs the test nothing because `empty_hour_frac` is asserted
    first and dominates it at both draws, and that reasoning is written beside
    the test. THE UNSTATED COROLLARY, now stated: at ~11 complete hour buckets
    the RESOLUTION of `empty_hour_frac` is 1/11 = 0.09 against a 0.0105 bound,
    so that assertion is a BINARY "no empty hour at all" and not a two-sided
    measurement of a fraction. It was already binary at the full draw (~96
    buckets, 0.0104 against 0.0105), so the short draw gives up nothing - but
    do not read the assertion's form as more than it is.
- `brokkr test -p <crate> <NAME>` RUNS EVERY SWEEP OF THE RESOLVED PROFILE, not
  one. For `mogwai-data` that is `workspace`, `instrumented` and `timing`, and
  it reports each separately - a sweep whose filter matches nothing prints
  `SKIP ... - no tests matched (likely feature-gated out of this sweep)` rather
  than a green PASS. That is what makes a `#[cfg(not(feature = "hotpath"))]`
  test still runnable by name with no false green, and it is why a cold review
  of this round's cut - which assumed one all-features sweep and therefore an
  unrunnable, silently-passing test - did not survive contact with the runner.
  The focused runner also applies NO `skip` list; only the gate profile does.
- TWO SKIP ENTRIES WERE LEFT UNRESOLVED HERE AND ROUND 4 CLOSED THEM with
  finding 6; see that section. `synthetic_spread_decomposition_at_protocol_seven`
  at 6.46 s is the only entry the cost heading still describes even loosely.
- REFUSED: cutting `SESSION_DRAW`. 15M parent events is ~30 simulated days and
  the seven `dow_weight` assertions need whole weeks to separate a weekend from
  a weekday, so halving it passes on less evidence rather than failing sooner.
  The watchdog risk did not reproduce: 7.51 s serial, and `brokkr.toml`'s own
  record has the same walk at 7.622 s serial against 7.768 s at eight threads,
  so this walk BARELY NOTICES THE PARALLEL LANE and sits 2.6x from the 20 s
  kill. The report's 1.6x-inflation estimate was borrowed from a sibling
  project and does not describe this one.
- WHERE THE GATE'S SAVING ACTUALLY IS, because the first draft of this section
  got it wrong: entirely in the `instrumented` sweep. In the gate's `workspace`
  sweep the deletion and the un-skipping cancel and all five `cfg`'d walks
  still run, so the dwell change bought COVERAGE there and no wall clock. The
  full gate went 58.3 s to 41.4 s and 50.4 s on two post-change runs; the wall
  is noisy at eight threads on a loaded host and the coverage counts are not,
  so audit with the counts.

## The tape document, round 3: the conformance vectors now run production

- `arrival_conformance_vectors_v1_through_v9` HAS A SCHEMA VERSION NOW, and it
  is a SCHEMA version only. Every fixture carries `"version": 2` and the loop
  asserts it, so a rewrite of the layout - which fields exist, which of them
  are gated - cannot land without coming back through the test. IT DETECTS NO
  CONTENT EDIT: widening an expected value or retuning a param leaves `version`
  at 2 and the assertion green. The round's first cut claimed the opposite in
  a comment and here, and cold review caught it. Nothing in the tree detects a
  quietly widened vector, which is the same hole AGENTS.md records against the
  shared-fixture rule; the executors are what has to be strong enough not to
  need one.
- `crates/mogwai-data/tests/fixtures/` HOLDS THE VECTORS AND IS NOT THE
  `analysis/` CONVENTION, deliberately. The `analysis/` rule exists for gates
  where TWO IMPLEMENTATIONS IN DIFFERENT LANGUAGES compare a quantity; these
  vectors have one implementation and one language, and the failure they had
  was the other half of the same lesson - a fixture and a test-local
  re-implementation derived from each other with production outside the loop.
  Do not relocate them expecting the rule to apply.
- THE PER-VECTOR VERDICT, because a later round will otherwise re-litigate it.
  Every expected value in V4 through V8 traces to
  `notes/protocol-12b-arrival-composition-spec.md` - section 7's two contract
  orderings, 4.2's segment-by-segment integral with the calendar INSIDE it,
  4.1's crossing rule and `(count - 1)` stride identity. NONE of them was
  read off a run, so routing production through them is a real gate and not a
  re-bless. What was wrong was only where the comparison happened.
- `drive_next_parent` IS THE ROUTING MECHANISM AND ITS TRICK IS LOAD-BEARING.
  A vector states a budget; the kernel's budget is an RNG draw and cannot be
  chosen. The helper reads the budget the kernel is ABOUT to draw from a clone
  of the cadence stream and sets `RuntimeModifiers::rate_mult` to
  `budget / unit_budget_s`, so the drawn budget realizes the vector's stated
  seconds of open exposure exactly. Without it the only way to have an
  expected timestamp is to run the kernel and write down what came out, which
  is the change-detector failure. THE CLONE ALSO LEANS ON CONTRACT B: it is
  only the budget because the budget is the first cadence draw, so moving the
  child draw to the front fails V6, V7 and V8 as well as V5.
  - `unit_latent_kernel` is `WallMmpp` at `rate_ratio = 1.0`, where the level
    denominator is 1 and `level` returns 1.0 in BOTH states. That keeps the
    latent multiplier at the vectors' unit intensity while the walk still takes
    one cadence draw per traversed grid step, which is what leaves V5's
    contract-B ordering claim observable.
- TWO FIXTURES WERE UNREALIZABLE AND THE TEST-LOCAL ARITHMETIC IS WHY NOBODY
  NOTICED. V6 asserted a THREE-SECOND calendar closure and V8 a
  `next_open_ns` of 100 ns; `SessionCalendar` is MINUTE-granular, so neither
  could be produced by any calendar the venue can hold. A vector that only ever
  meets a re-implementation never finds out that it describes an impossible
  world. Both were re-derived from the same spec arithmetic at realizable
  scales - V6 three minutes and a 181.5 s offset, V8 a closure at clock minute
  1 reopening at 120 s.
  - AND CALENDAR WINDOWS ARE INDEXED IN MINUTES OF THE LOCAL WEEK, not from the
    clock: `UNIX_EPOCH_LOCAL_WEEK_MINUTE` is 5,760, so clock minute 60 is week
    minute 5,820. The first cut of both fixtures used clock minutes, and the
    symptom was a parent resolving one second into what was supposed to be a
    closure.
- `GeneratedSource` CARRIES `draw_stages`, a `#[cfg(test)]` STAGE TRACE of the
  shipped contract-A path, cleared at the top of `begin_event`. It records ONE
  TAG PER STAGE - `gap`, `flip`, `latent_mid`, `side_book`, `child` - and NOT
  one per RNG draw, because a stage's primitive-draw count belongs to the
  distribution it samples and the ziggurat's rejection loop makes even that
  variable. V4's `main_stream_order` still spells its multiplicities out; the
  test compares the sequence with consecutive repeats COLLAPSED, and the
  multiplicities are documented as ungated rather than left looking checked.
- THE FIVE BITE-CHECKS, all as text edits in production and all reverted:
  a `rate_at(cursor).max(0.5)` (the calendar not inside the intensity) moves
  V6's parent 179 s; dropping `at_ts_ns <= candidate` from the crossing guard
  fails V8's no-crossing case; replacing the calendar snap with the bare
  shifted instant fails V8's closed-window case; drawing the child count before
  the budget fails `V5 contract-B child-draw position` at 4 against 1 - it is
  the CHILD-COUNT comparison that catches it, and the round's first draft
  attributed the bite to an assertion labelled "the first cadence draw is the
  budget" that could not fail at all, see below; and reordering the Weibull
  sample after the flip in `next_duration_ns` fails V4's stage order.
- TWO THINGS THE BITE-CHECKS FOUND THAT THE REPORT DID NOT NAME, and both are
  the "your first design has a hole one layer up" rule again:
  - V7 AT THE SHIPPED ONE-SECOND GRID COULD NOT SEE THE CONVERSION IT EXISTS
    FOR. Its parent lands exactly on the segment end, where
    `candidate.min(end)` clamps it, so a kernel adding a nanosecond to every
    `delta_ns` still produced 1,000,000,000. The vector now runs on a
    ten-second cadence step so the parent is strictly interior. GENERAL FORM:
    a vector whose expected instant coincides with a clamp is measuring the
    clamp. Ask where the value is decided before believing the assertion.
  - `next_parent`'s `if intensity == 0.0 { cursor = end; continue; }` IS A
    FAST PATH, NOT THE MECHANISM. Deleting it changes no behaviour, because
    `available` is then zero, `remaining > available` holds and the loop
    advances the cursor identically. Its real job is keeping `0.0 / 0.0` out of
    `delta_ns` if `remaining` ever reaches exactly zero. The first V6
    bite-check was that deletion and it PASSED - a perturbation that proves
    nothing looks exactly like a test that cannot fail.
- NO `TAPE_PROTOCOL_VERSION` BUMP WAS OWED and it was checked rather than
  assumed: `git diff` on `arrival.rs` is entirely inside `mod tests`, and every
  `source.rs` edit is `#[cfg(test)]` - a field that does not exist in release
  and pushes that consume no RNG. The gate's counts are unchanged at 1191 + 436
  over 1688 pairs, 0 orphaned, which is the same evidence: no test was added or
  removed, five were rewritten.
- THE ROUND COMMITTED AN INSTANCE OF ITS OWN SUBJECT, the fourteenth in this
  arc and the second time a round has done it while the defect class was the
  round's whole topic. V5's contract-B replay carried a `"budget"` arm
  asserting `-ln(U)` from a clone of the cadence stream against `run.budget`,
  which the helper had produced from the SAME clone at the SAME offset by the
  SAME expression. Both sides were test-local, no production code was
  consulted, and the arm was green for a kernel that draws no budget at all -
  while its message said "the first cadence draw is the budget". IT IS GONE
  AND NOTHING REPLACED IT: `next_parent` returns no budget and exposes no
  observable of one, so there is nothing to compare against, and saying so in
  place beats an assertion that reads as a gate. The arm still CONSUMES one
  draw, which is what leaves the reference standing where the child draw must
  be - that position is the whole bite, and it is spent on the child-count
  comparison two lines down.
- THE FAMILY THE COLD REVIEW SWEPT: A THING THAT READS AS GATED AND IS NOT.
  Four more instances beyond the one above, all closed the same way - either
  the field is read by an assertion that observes production, or it moves to
  `derivation_intermediates` and says it is read by nothing.
  - V7's `assert_bits` on the budget was fixture-against-itself, verbatim the
    defect its own new `_doc` said version 2 existed to remove. `budget` is in
    `derivation_intermediates` now.
  - V6's `segments_s` / `open` were declared inputs that nothing read - the
    segmentation is a CONSEQUENCE of the origin, the cadence step and the
    calendar minutes, not something the kernel can be given. Both moved.
  - Every remaining declared param is READ AND ASSERTED. `base_mean_s` reaches
    `next_parent` (the driver carries it into `rate_mult` so the realized
    exposure is invariant to it); `latent_x` is checked against
    `ParentDraw::latent_x`; `baseline_rate` against `ArrivalEnv::rate_at`;
    `intra_event_step_ns` and `cadence_step_ns` against the shipped constants.
    Bite-checked by editing the fixture value: each edit now fails a named
    assertion, where before it changed nothing.
  - V8 got `cell` back, pinned in the fixture as its own parent instants
    divided by its own declared cadence step and asserted against
    `ArrivalEnv::cell_index` - the derivation's "that snapped time is also the
    cell" had been left asserting over nothing. Bite-checked by shifting
    `cell_index` one step: fails at 51 against 50.
  - `next_from_ns` STAYS UNPINNED AND SAYS WHY. The child count is an RNG
    draw, so a pinned successor instant would be a run written down; the
    identity check restates production's expression and is labelled a
    re-implementation check, not a fixture gate. What the fixture does gate is
    the STRIDE it multiplies by. Bite-checked at `INTRA_EVENT_STEP_NS = 1_001`.
- V7 DRIVES THREE FAMILIES NOW, and the fourth name was wrong rather than
  missing. The fixture listed `event_markov`, `wall_mmpp`, `log_ou_cox` and
  `self_exciting` while the executor drove `WallMmpp` alone. `event_markov`
  IS THE CONTRACT-A PATH AND NEVER ENTERS `next_parent`, so it could not have
  been driven by this vector under any rewrite; it is dropped from the list
  with the reason recorded in the derivation. The other three are driven, each
  parameterized to a latent multiplier of exactly 1.0 - `WallMmpp` at
  `rate_ratio = 1`, `LogOuCox` at `sigma_y = 0`, `SelfExciting` at `phi = 0` -
  which is the precondition the intensity-scaling trick needs. The families
  differ in what and how much they draw, so this is not one path run three
  times: bite-checked by adding 1.0 to `ArrivalState::new`'s `LogOuCox` arm,
  which fails `V7 log_ou_cox must reach the conversion at unit latent
  intensity` and NOTHING ELSE in the vector set - coverage no vector had.
- BOTH FIXTURE-BUILT CALENDARS ARE `validate()`d NOW. That is the cheapest
  possible guard against the round's own two-impossible-worlds finding: a
  vector describing a closure no `SessionCalendar` can hold used to be
  invisible because nothing built a calendar from it.
- `draw_stages` IS A SOFT GATE AND THE FIELD SAYS SO. The pushes are
  hand-placed statements ADJACENT TO the work, so what they pin is the order
  of the PUSHES; moving a draw past its own tag is undetected. It catches a
  reordering of the stages AS WRITTEN, which is how contract A is expressed
  here, and that is the whole of what V4 and V5 rest on it. Do not restate it
  as pinning the draw order. `begin_integrated_event` pushes nothing, so an
  anchor with an `arrival` kernel traces empty rather than wrong; the reader
  refuses on emptiness and its message now names that cause.
- `drive_next_parent`'s LATENT-STEP REPLAY IS NOT GENERAL. `cell_index(parent)`
  is the draw count only when the walk starts in cell zero AND resolves inside
  its first segment. V5 is the only caller that replays and is driven from
  `from_ns = 0` for exactly that reason; the helper's doc and the call site
  both say so, and the call site asserts the cell-zero start rather than
  trusting it.
- V6's `+0..4 ns` WINDOW IS A MAGNITUDE BOUND, not a statement about which way
  `ceil` rounds. The pre-ceiling value can sit either side of the ideal,
  because the intensity scaling is a float division; what makes the window
  safe is that `remaining` carries relative error of order 1e-16 over three
  segment subtractions against a 2.5e8 ns delta, so the absolute error is of
  order 1e-8 ns. The comment argued the direction of `ceil`, which is not
  sufficient, and now argues the magnitude.

## The tape document, round 4: the GARCH report and the prose gate

- `garch_second_moment_instrumentation` WAS REPOINTED, NOT DELETED, and the
  reasoning is the round's main ruling. Its two arms were labelled RAW =
  "AS SHIPPED" and standardized = "COUNTERFACTUAL", exactly inverted, at a
  `vol_scalar` of 1e-6 against a shipped `VOL_SCALAR` of 1.2e-5 - so its
  headline numbers, `sqrt(E[sigma2]) / vol_scalar 968.71x` and
  `cap occupancy 17.19%`, described the pre-standardization process under a
  banner saying otherwise. All of it reproduced verbatim. The question "what
  was it the only implementation of" answered itself: `consts.rs` CITES it as
  the ground for `GARCH_ARCH` and `GARCH_GARCH`, so deleting it would have left
  a durable comment pointing at nothing.
  - THE SHIPPED ARM NOW CARRIES THREE ASSERTIONS A CONSTANT CHANGE CAN BREAK,
    which is what the old pair could not. Stationarity
    (`GARCH_ARCH * E[z^2] + GARCH_GARCH < 1`, measured 0.9991), RAIL
    NON-PARTICIPATION (`at_cap` and `feedback_clamped` both exactly zero in a
    million clean updates), and `sqrt(E[sigma2]) / VOL_SCALAR` within a factor
    of two of one (measured 0.9571). Bite-checked as text edits in `consts.rs`:
    `GARCH_GARCH` 0.979 -> 0.99 fails stationarity by name at 1.0101, and
    `VOL_SCALAR` 1.2e-5 -> 1.2e-4 fails the rail assertion at 4 feedback clamps
    with stationarity still green - so the two are independent rather than one
    check wearing two hats. The third is bitten in `dynamics.rs` instead, and
    why is the next entry.
  - THE 10 PERCENT WINDOW THE FIX PASS SHIPPED WAS UNDER 1 PERCENT, and the
    cold review caught it. `a0 = VOL_SCALAR^2 * (1 - a1 - b1)` over a stationary
    variance `a0 / (1 - a1 * E[z^2] - b1)` puts a 0.001 denominator under a
    derivative of `-a1` = -0.02: A TWENTYFOLD AMPLIFICATION of `E[z^2]` into the
    ratio, so the window admitted `E[z^2]` in about [0.9905, 1.0083] while the
    sibling unit-variance assertion two lines above admitted [0.75, 1.25], 25x
    wider. Worse, `E[z^2]` is a mean of a squared standardized t(4), INFINITE
    VARIANCE at df 4, so no CLT error bar exists to justify any tight number.
  - THE OBVIOUS REPAIR - assert against the stationary variance computed FROM
    the measured `E[z^2]`, dividing the amplification out - WAS TRIED AND IS
    WORSE, which is why it was measured before being written down. Over harness
    seeds 42, 7 and 1: raw ratio 0.9571 / 0.9904 / 0.9530, corrected ratio
    0.9319 / 1.0085 / 0.9455. The correction WIDENS the spread from 3.7 points
    to 7.7, because it feeds `E[z^2]`'s own noise through the same 20x gain into
    the target. And driving `GARCH_GARCH` to 0.9799 - still stationary, still
    green everywhere else - takes the raw ratio to 0.685 and the corrected one
    to 0.474, BOTH DOWNWARD, opposite to what stationary theory predicts,
    because near the pole a million updates stop sampling the stationary mean.
    Either form at 10 percent is a red gate for a change that broke nothing.
  - SO THE WINDOW IS HALF TO DOUBLE AND ITS CATCH SET IS MEASURED, not tasted.
    The ratio is near an IDENTITY across the stationary interior by
    construction, so no band gates `a1` and `b1` here at all. What it does catch:
    a broken `a0` derivation - `a0 * 9` in `GarchVol::new` gives 2.87x and fails,
    which is the claim `VOL_SCALAR^2` is the unconditional variance, asserted
    nowhere else - and loss of unit variance, which misses by 80x on the
    unstandardized arm and missed by 968x pre-repair. Both ratios are still
    PRINTED, which is the part the tight assertion was pretending to do.
  - THE SECOND ARM'S LABEL WAS ALSO FALSE AND IS NOW TRUE. The fix pass drove
    it at the pre-standardization era's `vol_scalar` of 1e-6 and captioned it
    "the pre-standardization process", justified as "a counterfactual is only
    informative at the parameters it actually ran with". But `run_garch_harness`
    builds `GarchVol::new`, which reads the SHIPPED `GARCH_ARCH` and
    `GARCH_GARCH` and takes no override - the era ran 0.12 and 0.875 - so that
    arm was TWO of today's three parameters with one historical one: a triple
    that never shipped in any era, printed under a historical banner. The same
    defect the round was closing, one layer down, and the justification argued
    for something the code did not do. Both arms now run at `VOL_SCALAR` and the
    second is captioned "TODAY's parameters WITHOUT the standardization", which
    is what its assertions actually claim; `vol_scalar` reached only the printed
    report, since both are scale-free. Reconstructing the era needs an `a1`/`b1`
    override hook and nothing here asks a question that would justify one.
  - THAT ARM'S PERSISTENCE ASSERTION IS DELIBERATELY KEPT AND ITS MEANING IS
    WRITTEN ON IT. It reads TODAY's `a1` and `b1` against an unstandardized
    innovation, so what it says is "the standardization is what buys
    stationarity, not slack in the parameters" - true at 1.0191, and a future
    re-solve turning it red has made the standardization optional rather than
    regressed. That is a deliberate re-read, not a goalpost to move.
  - AND THE HARNESS MAKES ITS OWN STANDARDIZATION DECISION, so DE-STANDARDIZING
    PRODUCTION LEAVES EVERY ASSERTION HERE GREEN. Stated on the block comment
    rather than papered over: that property belongs to
    `trace_consumes_no_draws_and_leaves_the_tape_byte_identical`, which asserts
    `innovation_raw / innovation_std == STUDENT_T_UNIT_SCALE` against the
    shipped walk. This file owns the CONSEQUENCES - stationarity, rail
    occupancy, whether `VOL_SCALAR` means what it says.
- THE TWO SKIP ENTRIES ROUND 2 LEFT ARE RESOLVED, the same way the dwell pair
  was. `standardized_candidate_rail_sizing` measures 0.43 s and
  `realized_return_envelope_under_regime_scaling` 0.20 s against a heading
  claiming both outlive the 20 s watchdog; both are un-ignored, out of `skip`
  and running on every lane. THE COST OF THE WRONG CLAIM WAS THE DWELL COST
  AGAIN: those two are the only measurements behind `GARCH_SIGMA_CAP`,
  `FEEDBACK_RETURN_CEILING` and `REALIZED_RETURN_CEILING`, so every number
  `consts.rs` quotes in prose - 57.2x, 3.33e-3, RMS 1.2393e-5, 0.82 percent -
  came from a test nothing ran. All re-measured unchanged.
  - BOTH NOW READ THE SHIPPED CONSTANTS. Each had a frozen
    `(0.02, 0.979, 1.2e-5)` "stage-1 winner" written into it, and the envelope
    one also had the design-era "proposed" rails `1e-3` and `4e-3` as local
    literals. Those were what the sweep chose BETWEEN; left in place after the
    choice landed, a re-solve would have gone on measuring the old process,
    green, while `consts.rs` cited it. The rail test is renamed
    `shipped_garch_rails_sit_above_the_clean_tail` for the same reason - the
    word "candidate" was load-bearing and had stopped being true.
  - THE ENVELOPE TEST NEEDED A CLAIM OF ITS OWN, because pointing it at the
    shipped rails made its two assertions duplicates of the rail test's at half
    the horizon. It now asserts what only it can see: `REALIZED_RETURN_CEILING`
    is INERT IN CLEAN OPERATION - `base_max * session_peak` at `vol_mult` 1 is
    8.1771e-3 against a ceiling of 5e-2, which is `consts.rs`'s "0.82 percent
    against 5.13 percent" asserted rather than printed. Bite-checked by editing
    the ceiling to 4e-3, re-verified after this round's edits.
  - FOUR RAIL ASSERTIONS ACROSS THE TWO TESTS COULD NOT FAIL, and the round's
    own bite-check evidence was misattributed to them. The cold review named
    the sigma pair; MEASUREMENT FOUND THE FEEDBACK PAIR IS THE SAME. Lowering
    `GARCH_SIGMA_CAP` to 5e-4 and, separately, `FEEDBACK_RETURN_CEILING` to
    1e-3 fails BOTH tests on `measure_uncapped_tail`'s in-loop
    `!hit_variance_cap() && !hit_feedback_clamp()` guard, never on the named
    assertion after the loop - the guard aborts the run at the first offending
    update, so no later comparison against a maximum is ever reached. The sigma
    ones were dead twice over: `step` writes `sigma2 = candidate.min(cap)`, so
    a maximum over the capped value cannot exceed the cap even with the guard
    removed. All four are DELETED. The claim is stated once, on the guard,
    whose message now says what a failure means, and the guard is the stronger
    form anyway - every step rather than a top-k maximum.
  - AND `measure_uncapped_tail` NOW COLLECTS `sigma2_candidate`, the pre-cap
    value, which is what its own name says it measures. It was collecting the
    capped `sigma2`. The guard makes the two equal today, so every printed
    figure - 57.2x, 3.3252e-3, RMS 1.2393e-5 - is unchanged; the point is that
    they would silently stop being equal exactly when the measurement mattered.
- `tape_version_prose`'s `claims()` PANICKED ON A BYTE-INDEX SLICE and now
  snaps only the QUOTED window to char boundaries. WHAT IS MATCHED IS
  UNTOUCHED, and that was the constraint: the gate's whole design is two exact
  phrasings, everything else read as a HISTORICAL record, so a repair that
  widened matching would rewrite frozen specs.
  - PINNED BY A SYNTHETIC HAYSTACK, not by a file in the tree: the repository
    forbids the character in its own prose, so the hazard is unrepresentable as
    a fixture and the pin BUILDS the character from its scalar value. Placement
    is exact - it straddles the window edge on each side - because a multi-byte
    character merely NEAR a claim is harmless and a test that puts one in the
    neighbourhood passes for free. Bite-checked one edge at a time: reverting
    `start` alone fails on the leading character, reverting `end` alone fails on
    the trailing one.
  - THE SYMLINK NOTE IS REAL BUT NOT AS REPORTED, AND MEASUREMENT IS WHAT SAID
    SO. "A symlinked directory cycle recurses forever" is false on Linux: the
    kernel caps symlink resolution per pathname at 40, `is_dir` returns false
    there and the walk terminates on its own. What it actually cost, planted as
    `notes/x -> ..` and measured, was a 40-FOLD WALK - 0.27 s to 1.02 s - with
    every markdown file collected up to forty times, plus a reachable `read_dir`
    failure on the ELOOP boundary that would fail a prose gate with a symlink
    error. Fixed with `entry.file_type()` and pinned by
    `a_symlink_cycle_is_not_descended_into`, which builds the cycle in a
    pid-named `CARGO_TARGET_TMPDIR` directory rather than in the tree the suite
    is judged against. IT ASSERTS THE COUNT, NOT TERMINATION: the defect
    terminates too, and reports the same two files eighty times. Bite-checked
    by restoring `path.is_dir()`.
  - REFUSED: making the walk tolerate a `read_dir` error. The report reads the
    panic as "one unreadable directory fails a test about prose", and it does -
    correctly. This gate's claim is that it read every markdown file in the
    repository; skipping a directory it could not open keeps the green light
    while dropping the claim, which is the fail-open scanner the arc has
    refused twice already. The panic message now says that is what it is. A
  follow-up note - that a panic in `markdown_files` leaks the scratch tree,
  because `remove_dir_all` runs after the walk - is ACCEPTED AS STATED rather
  than fixed: the directory is inert, lives under `CARGO_TARGET_TMPDIR` rather
  than in the tree the suite judges, and `catch_unwind` around the walk is more
  machinery than a build-tree directory is worth. The cost is written beside
  the prologue that would collect it.
- REFUTED, BY MEASUREMENT: that the 0.43 s un-ignore cost rests on a
  release-profile number and would be 8 to 30 s in dev, straddling the
  watchdog. Re-measured with `--debug` it is 0.45 s to 0.56 s across the three
  sweeps. The lesson kept is the smaller one: record the SPREAD, not a single
  figure, on a crate whose gate wall is known to be noisy.
- NO `TAPE_PROTOCOL_VERSION` BUMP WAS OWED, checked rather than assumed:
  `git diff` on `consts.rs` is two lines of doc comment naming the renamed test,
  and every `tests.rs` edit is inside `mod tests`, which `generated/mod.rs`
  declares `#[cfg(test)]`. No constant, no fingerprint, no artifact moved.

## The tape document, round 5: what carries to `bugs-tests-engine-protocol`

The tape mechanics die with that document. What generalises, from the six
"smaller things" and the two praised patterns among them:

- VERIFY THE PRAISE. Two of the round's six items were commendations rather
  than defects, and BOTH held - but only after being run. The two checks that
  earned their keep: the praised Python half of the Roll conformance fixture
  only PRINTED `spec["version"]` where the Rust half asserts it, so a schema
  bump would have gone through one gate and past the other; and the
  integral-floor test's comment claimed "a truncating grid reads 1.00, all mass
  on the floor", which measurement refuted at 2.2642. THE REASONING IN A
  PRAISED COMMENT DECAYS LIKE ANY OTHER COMMENT, and a number written inside an
  argument for why an assertion is not vacuous is exactly the kind nobody
  re-runs.
  - The praise itself is deserved and the pattern is the model: a versioned
    language-neutral JSON under `analysis/` carrying `_doc`, `units`, `rules`
    and per-case `why`, `include_str!`d by two implementations kept
    deliberately separate. `dwell_conformance.json` is run by an automated test
    on both sides; `spread_conformance.json`'s Python side is a manual
    `python3 analysis/roll_estimator.py conformance` and no lane runs it.
    Recorded rather than fixed: a Rust test may not spawn Python, and the
    file's own docstring is where the invocation lives.
  - WHAT NEITHER FIXTURE DETECTS, and it is the standing AGENTS.md hole: the
    version is a SCHEMA version, so a quietly WIDENED `tolerance` weakens both
    halves at once and stays green on both. Unlike the arrival vectors there is
    no re-implementation to catch it, because catching it is precisely what
    having two implementations buys - and a tolerance edit moves both.
- A BRITTLE ASSERTION IS USUALLY A REAL CLAIM WEARING THE WRONG EXPRESSION, so
  CONVERT RATHER THAN DELETE. Three instances in one round, and in all three the
  proposal on the table was deletion:
  - `Arc::strong_count(&symbol) == 3` counts handles instead of naming them. The
    third handle was the permuter's HashMap KEY, and nothing else asserted that
    the key reuses the trade's allocation - so deleting the count on the ground
    that the neighbouring `ptr_eq` "already carries the claim" would have
    unpinned a real property. It is a `ptr_eq` on the key from
    `get_key_value` now, which says the same thing and does not move when the
    tick's route through `apply` does. Bite-checked by allocating a fresh
    `Symbol` for the key: the new assertion fires, the neighbouring one passes.
  - `size_of::<PublishedBook>() == 48` is a layout pin with no `#[repr]` behind
    it. Stated as `2 * size_of::<f64>() + 2 * size_of::<Decimal>()` it is the
    same check on today's layout, immune to reordering, and it reads as the
    claim. IT IS NOT IMMUNE TO A FIELD CHANGING WIDTH - see the review repair
    further down this section, which is the counterexample to the sentence
    that used to stand here. Bite-checked by adding a
    `CalibrationProvenance` field: fails 72 against 48 on the named assertion.
  - A `#[cfg(test)]` `thread_local!` counter mutated from a production loop is
    process-global state whose safety rests on a `.set(0)` one line above the
    read. A RETURNED COUNT CANNOT BE SHARED: `settlement_instants` delegates to
    a private `settlement_scan` returning `(Vec<u64>, usize)`, and production
    now carries no `cfg` at all. THE BITE-CHECK IS WHERE THE LESSON IS - the
    obvious perturbation (step one minute instead of one day) fails on the
    INSTANT LIST, not on the count, so it credits the wrong assertion. The
    perturbation the count alone catches is the honest alternative
    implementation: step a minute at a time AND filter on the minute-of-day,
    which returns the identical list from 14,371 candidates against 10.
- MEASURE THE CATCH SET BY SCALING THE FIXTURE'S OWN INPUT, not by perturbing
  production, wherever the quantity is linear in it. `liquidity_drought_imitates
  _dying_symbol` compared `mean_gap` against `thin_factor * mean_event_duration_s`
  in a 0.5x-to-2x window justified as "sampling slack". It is not slack: the
  ratio is systematic, 0.8898 to 0.9739 over run seeds 0-7 and 42, an 8.4
  percent spread, sitting below one because the gap mean is sampled PER EVENT
  and busy hours emit more events, so the sample over-represents large
  `arr_mult` and the mean of `1 / arr_mult` lands under one. Scaling
  `thin_factor` while
  holding the expectation fixed is arithmetically identical to a production
  multiplier off by that factor, so one test-local loop measured the whole catch
  set in one run: caught below 0.55x and above 2.25x. The window is 0.75 to 1.15
  now - caught below ~0.83x and above ~1.27x, ensemble clearing both edges by
  ~18 percent. Bite-checked at `thin_factor * 1.5` in `regime.rs`: fails the
  ratio assertion at 1.3508, which the old window passed, AND
  `liquidity_drought_stretches_durations` passes under the same perturbation, so
  no other test covered it.
  - THE REPORT'S OWN CLAIM ABOUT THE WINDOW WAS HALF WRONG in the direction
    nobody checks: "will not catch a multiplier off by 50%" is true upward
    (1.5x reads 1.351, inside 2) and false downward (0.5x reads 0.451, outside
    0.5). A window that is asymmetric ONLY because the honest value sits off
    centre is the easiest kind to describe wrongly.
- A DETERMINISTIC TEST IS NOT AUTOMATICALLY A TIGHT ONE, and the two get
  conflated. Every window in this round was on a fixed seed, so none of them
  could flake - and three of them were sized as though they could. Where the
  seed is fixed, the ONLY thing a wide window buys is tolerance of legitimate
  change, which under this workspace's rules already owes a
  `TAPE_PROTOCOL_VERSION` bump and a re-bless. Size the window against the
  seed ENSEMBLE and say so.
- THE ROUND THAT CORRECTS FALSE COMMENTS WRITES FALSE COMMENTS. Round 5's whole
  subject was claims that read as checked and were not, and its cold review
  found three of its own, all in prose it had just written. Two general shapes,
  both worth carrying to `bugs-tests-engine-protocol`:
  - A REPLACEMENT ASSERTION INHERITS A SCOPE CLAIM NOBODY RE-DERIVES. "Immune
    to the field types changing width" was written of
    `size_of::<PublishedBook>() == 2 * size_of::<f64>() + 2 * size_of::<Decimal>()`.
    It is false: sum-of-fields equals `size_of` IS a layout claim - it asserts
    zero padding - and narrowing one `f64` to `f32` makes the sum 44 while
    `size_of` rounds to 48, firing on exactly the change the claim exempted.
    When a brittle assertion is converted, the new one's IMMUNITIES are a fresh
    claim owing its own counterexample hunt, not something the conversion
    confers.
  - A MECHANISM NAMED IN A COMMENT MUST BE SHOWN REACHABLE. The drought comment
    explained a sub-unity ratio by "two sites", where the second
    - `low_intensity_gap_ns` - is gated on `arr_mult < 0.01` and the committed
    calendar-free profile bottoms out near 0.584, so it never runs in that test.
    The window derived from the wrong story was nonetheless correct, which is
    the trap: a right number is not evidence for the argument printed above it.
    Grep the gate before naming the branch.
  - A GUARD PLACED AFTER WHAT IT GUARDS IS NOT A GUARD. The Python conformance
    runner's new `spec["version"] != 1` check sat two lines below
    `tol = spec["tolerance"]`, so the v2 fixture it exists to catch raises
    `KeyError` first and the reader gets a traceback instead of the message.
    Version and schema guards belong immediately after the parse, which is also
    where the Rust half had always put its `assert_eq!`.
- A DOCUMENT MUST NOT CLOSE WHAT ITS OWN CHANGE RECORDS AS OPEN. Round 5's
  report said "nothing is left open from this cluster" while the carry-forward
  edit in the same commit recorded two unfixed items, one of them a live hole
  in a binding `AGENTS.md` rule. Residue is named as CARRIED in the report
  itself; a pointer to another file is not a disclosure. The two carried here
  are the manual-only Python conformance lane and, the one that matters, that
  neither shared fixture detects a quietly widened `tolerance` - the version is
  a SCHEMA version and a tolerance edit weakens both implementations at once,
  so the second implementation is structurally blind to it.
- NO `TAPE_PROTOCOL_VERSION` BUMP WAS OWED, checked rather than assumed. The
  round's one production edit is `calendar.rs`'s `settlement_scan` extraction:
  the loop, the pushes and the day step are character-identical, the count is
  the only new binding, and the removed `#[cfg(test)]` update never existed in
  release. No constant, no fingerprint, no artifact moved, and the gate's counts
  are unchanged at 1195 + 440 over 1692 pairs, 0 orphaned - no test added or
  removed, four rewritten. No tape-identity bump was owed by that round.

## The tape document, close pass: what the five commits left

The arc is SOUND. The gate is green at 1195 + 440, 1692 pairs, 1635 run, 57
ignored, 0 orphaned, and the two claims most worth not taking on trust were
re-run rather than read: `shipped_garch_rails_sit_above_the_clean_tail` prints
57.2x, 3.3252e-3 and an RMS of 1.2393e-5 over its 16M updates, exactly the
figures `consts.rs` cites, in 0.39 s; and `dwell_is_bounded_across_run_seeds`
reports PASS / SKIP / PASS over the three sweeps, so the `cfg` really does
produce a reasoned skip rather than a silent zero-match green. No finding in
this pass was in production behaviour.

- THE ETXTBSY RACE IS FIXED, AND THE ADAPTER DOCUMENT'S DIAGNOSIS WAS HALF OF
  IT. Pid-qualifying the silent-venue script closed the cross-process half and
  is still needed. The half it missed is intra-process and is what fired again
  here: `launch.rs` has three sibling tests that spawn children, libtest runs
  them in parallel, and a fork inherits this helper's write descriptor until
  that child's own exec clears it. The test retries on that one errno, bounded,
  with the attempt count in the failure message. It is not a silent-degradation
  shape: `busy` is derived from the error variant, so an exhausted budget still
  fails on the Timeout assertion, and no other error is retried at all.
- TWO ARITHMETIC ERRORS IN `reference/performance.md`, the same one twice:
  "1191 + 436 = 1688 pairs, 1627 run" and "1195 + 440 = 1692 pairs, 1635 run".
  Both sums are the RUN count; the pair count is run plus ignored. The
  decomposition either side of the wrong equals sign was right, which is how it
  survived two corrections of this file - A FALSE EQUATION BETWEEN TWO TRUE
  NUMBERS reads as a typo and gets copied forward as a fact.
- THE CARRY-FORWARD CONTRADICTED ITSELF WITHIN ONE SECTION. The round-5 entry
  said the new `size_of::<PublishedBook>()` assertion is "immune to reordering
  and to a field type changing width", and eleven bullets later the review
  repair states that immunity is false and gives the counterexample. The fix
  pass wrote the first, the repair pass wrote the second, and nothing walked
  back. WHEN A ROUND RETRACTS ITS OWN CLAIM, GREP THE DOCUMENT FOR THE CLAIM;
  appending the correction leaves the falsehood in the live voice above it.
- `AGENTS.md`'s new `segments` entry described `tape` as "writing trades or
  bars out of a generated source". It composes a SEGMENT LIBRARY - the thing
  `cut` produced - which is the whole point of the subcommand, the fitted
  generator being what it is the A/B against. Corrected.
- `target/tmp` IS CLOSED AND WAS NEVER A LEAK; see the adapter close pass's
  lateral list above for the dating that settles it.
- NOT A DEFECT, BUT KNOW IT IS THERE: round 3 added a `#[cfg(test)]`
  `draw_stages` field that `begin_event` pushes to, and round 5 removed a
  `#[cfg(test)]` `thread_local!` that `settlement_scan` pushed to. That is not
  an inconsistency - the objection to the thread-local was that it is
  PROCESS-GLOBAL state whose safety rested on a `.set(0)` one line above the
  read, and a field on `&mut self` cannot be shared - but the two look alike at
  a glance and a later round will notice. The line is shared state, not
  `cfg(test)` in a production body.

## The engine/protocol document, round 1: `launch.rs`

- THE ETXTBSY RACE IS NOW CLOSED STRUCTURALLY, AND THE RETRY IS A BACKSTOP. The
  two halves are separate mechanisms and both are needed. CROSS-PROCESS: the
  script path carries the pid, because the gate's dev and instrumented sweeps
  run this crate's unit tests at once. INTRA-PROCESS: a module-level
  `SPAWN_SERIALIZATION` mutex now makes "a fixture is open for writing" and "a
  sibling test forks" MUTUALLY EXCLUSIVE, rather than retrying the overlap.
  Every `launch` call in the module holds it for the duration of the call -
  `Command::spawn` returns only after the child's exec is confirmed through
  Rust's `CLOEXEC` report pipe, so the whole fork-to-exec span is covered.
  - THE ARGUMENT RESTS ON `launch.rs` BEING THE ONLY `Command::new` IN
    `mogwai-protocol`, verified by grep. A second spawner anywhere in the crate
    reopens the window and owes the same lock. This is stated on the lock.
  - THE BOUNDED RETRY STAYS, moved into `launch_serialized` so every script
    fixture inherits it. It costs nothing while the lock holds, guards a failure
    that has already been misdiagnosed twice, and cannot degrade silently: only
    that one errno is retried and an exhausted budget still fails the caller's
    own assertion, with the attempt count in the message.
- `launch.rs`'s test module now has `write_venue_script` / `scripted_venue`,
  and NOTHING MAY HAND-ROLL A SCRIPT FIXTURE THERE AGAIN - a second hand-rolled
  write is exactly what re-arms the race. `scripted_venue(name, then)` prints
  this module's own `record_json(ReadyRecord::VERSION)` and then runs `then`, so
  a schema bump moves the fixture with the parser. The returned `VenueScript` is
  a guard that unlinks on drop.
- THE SECOND HALF OF `own_venue` HAD NO TEST AT ALL and now has SIX:
  `a_venue_that_ended_during_shutdown_still_records_its_exit`,
  `a_crashed_venue_records_its_nonzero_code`,
  `a_signalled_venue_records_no_exit_code`,
  `a_venue_killed_while_healthy_records_no_exit`,
  `a_recorded_teardown_failure_is_reported_and_not_repeated` and
  `the_teardown_detail_is_read_after_the_owner_joins`. With the boot-path
  `a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure` the
  round added seven. What each one bites, measured as text edits:
  - `a_venue_that_ended_during_shutdown_still_records_its_exit` is THE ONLY test
    of the unconditional reap, and THE COLD REVIEW CAUGHT IT AS A TWO-SIDED
    WALL-CLOCK RACE in its first form, which slept `OWNER_POLL / 4`. SLIP LONG
    and the POLL arm reaps, the assertion passes, AND THE TEST PASSES WITH THE
    FIX REVERTED - reverting moves `if asked_to_stop { break; }` ahead of the
    `try_wait` on the SHUTDOWN arm only. SLIP SHORT and the child has not
    exited, so the test fails for a reason unrelated to the fix. A hand
    bite-check on one machine cannot preserve either property.
    - Both preconditions are now OBSERVABLES. `launch_with_poll` /
      `launch_serialized_with_poll` / `launch_scripted_with_poll` name the
      owner's poll interval, and this test uses an HOUR, so no poll can expire;
      `LaunchedVenue::polls` counts poll-arm entries and the test asserts it is
      ZERO, which is the "still in its first `recv_timeout`" claim the doc
      comment used to merely assert. `await_child_end` then waits for the
      kernel to agree the script is over before tearing down, using the pid the
      fixture reports - `scripted_venue` prints `$$` as the record's `pid`
      rather than the fixture's 42, and `$$` survives the fixtures' `exec`. An
      unreaped child stays a ZOMBIE, so that is a stable state, not a window.
    - Bite-checked by restoring the `if asked_to_stop { break; }` ahead of the
      `try_wait`: 6 of 6 runs fail on the EXIT-RECORD assertion, dev and
      release, with the poll assertion passing - so the evidence that the
      shutdown arm ran held while the fix was absent, which is exactly what the
      observable is for.
    - It uses the private `terminate(&mut self)` because `shutdown` consumes
      the venue and the record must be read AFTER the teardown.
  - `a_venue_killed_while_healthy_records_no_exit` had the same defect shape in
    the other direction and took the same cure: it waits for `polls` to reach
    TWO instead of sleeping one and a half `OWNER_POLL`s. Two, not one, because
    the counter is published at the TOP of the poll arm, so one proves only
    that the arm was entered; a second entry proves the first ran its `try_wait`
    and found the venue alive.
  - `a_crashed_venue_records_its_nonzero_code` and
    `a_signalled_venue_records_no_exit_code` bite the `VenueExit` construction
    (`success: true` / `code.or(Some(0))` fails both, each on its own values).
  - `a_recorded_teardown_failure_is_reported_and_not_repeated` is the vacuity
    bite: making `shutdown` return `Ok(())` unconditionally fails it by name.
    IT FILLS THE `teardown` SLOT DIRECTLY, from inside the module. Provoking a
    genuine failed kill means reaping the child out from under the owner, which
    leaves `Child::kill` signalling a pid the kernel may have recycled; a test
    that can kill an unrelated process is not worth the coverage. WHAT STAYS
    UNPINNED IS THE COUPLING - that the owning thread writes that slot when its
    kill or reap fails. `a_venue_killed_while_healthy_records_no_exit` covers
    the other direction, that a healthy teardown leaves the slot empty.
  - `the_teardown_detail_is_read_after_the_owner_joins` closes the half the
    direct-fill test looked like it bought and did not. `terminate`'s comment
    calls the read's position AFTER the join load-bearing - a read before it
    races the owner's write and reports a clean teardown of a venue that would
    not die - and the direct-fill test builds `owner: None`, so THERE IS NO JOIN
    for the read to be on the wrong side of. Moving the read above the join left
    every other test in the module green. The new test gives the venue a
    stand-in owner thread - a plain thread, since what is under test is
    `terminate`'s order of operations and not the child - which blocks until the
    shutdown sender drops, sleeps 100 ms, and only THEN records the failure,
    the same shape as `own_venue` killing, waiting and then writing. Moving the
    read above the join fails it 4 of 4 runs.
- THE `NoRecord` 50 ms PAUSE IS A REFUSAL, NOT AN OVERSIGHT, and the report's
  premise for changing it was measured false. `NoRecord` is EOF on the stdout
  pipe, which a venue can produce while alive; the child is NOT dead by then.
  `a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure` pins it
  with `exec 1>&-; exec sleep 60`, bounding the report at 5 s - well under both
  the child's own 60 s and the 300 s readiness bound, so neither of those is
  what ended the wait. Draining to EOF there would hang `launch`
  forever right after it decided to report a boot failure. Do not re-open
  without answering that.

## The engine/protocol document, round 2: risk policy and havoc validation

- `risk.rs`'s TEST MODULE HAS A `policed()` FIXTURE HELPER and no test there may
  hand-build a policy again. It names `currency: "USD"` so the currency rule -
  which fires for EVERY policed fixture, whatever the rule under test says -
  cannot fire and take the credit. That shadowing was A1: a fixture leaving
  `currency: None` is refused either way, so a test asserting only `is_err()`
  stayed green over a deleted amount branch. THE GENERAL SHAPE, and it is worth
  carrying to any validator with a cross-cutting precondition: A FIXTURE MUST
  SATISFY EVERY RULE EXCEPT THE ONE UNDER TEST, and the exact message is what
  proves it did.
- `SHIPPED_POLICIES` IS AUTHORITATIVE OVER `shipped_policy`, by a membership
  gate at the top of the function. The unpinnable direction was a match arm
  resolving a name absent from the list; nothing can enumerate a `match`'s arms,
  so the honest close is structural - such an arm is now unreachable rather than
  merely untested. A test can only observe the gate, not the arm, and the
  bite-check has to be run in two steps to show that: add the unlisted arm WITH
  the gate (test passes, which is the point) and then delete the gate (test
  fails). Same lesson as `gate_skip_list.rs`: state the rule the code can
  enforce.
- `EventKind::is_execution` / `is_admission` ARE EXHAUSTIVE MATCHES, NOT
  `matches!`, and that is load-bearing rather than stylistic. The production
  comment claims a new kind "must opt IN to being delayed" - a claim about every
  variant, present and future - and `matches!` cannot carry it. Written as
  exhaustive matches the crate does not build until a new variant is classified,
  at four sites. WHEN A COMMENT MAKES A CLAIM ABOUT VARIANTS THAT DO NOT EXIST
  YET, THE COMPILER IS THE ONLY THING THAT CAN HOLD IT; a test can only pin
  today's variants, so write both and let the test's expectation be an
  exhaustive match too, which makes it fail to compile alongside production.
- WHERE A VALIDATOR'S BRANCHES RETURN DISTINCT MESSAGES, ONE PERTURBATION PER
  BRANCH IS THE ONLY HONEST BITE-CHECK. This round ran thirteen separate text
  edits across `validate()` and `validate_divergence`, and every one had to name
  which assertion fired - four amount branches whose fixtures are otherwise
  identical, three currency directions in one `if`, and six numeric bounds. A
  single perturbation that reddens a multi-assertion test proves only that ONE
  of its assertions is alive.
- A `Result<(), &'static str>` VALIDATOR'S "ALWAYS VALID" LOOP IS A COVERAGE
  CLAIM AND HAS TO LIST EVERY SUCH VARIANT. `validate_divergence`'s loop skipped
  `RejectNextCancel` and `CancelOpenOrderSilently`, so nothing established that a
  WELL-FORMED arm of theirs is accepted - only that a malformed one is refused,
  which a validator refusing everything also satisfies.
- BLANK MEANS TRIMS-TO-EMPTY, ACROSS BOTH VALIDATORS IN THIS CRATE.
  `validate_divergence` refuses a `client_order_id` on `trim().is_empty()`;
  `AccountPolicy::validate` used a bare `is_empty()` on the currency, so
  `Some("   ")` was accepted. Made to agree on the TRIM side, which is a
  production change and needs its reason stated: the currency is a LOOKUP KEY -
  the server sums equity over balances carrying exactly that code - so a
  whitespace code matches no balance and would freeze a policed account's equity
  at zero forever rather than refuse the policy at registration. Refusing at
  registration is the whole point of `validate`. LATERAL, NOT CLOSED: a currency
  with SURROUNDING whitespace, `" USD "`, is still accepted and still matches no
  balance. Neither validator normalizes, only rejects; a later round wanting the
  stronger rule should refuse any code differing from its trimmed form on both
  sides at once.
- THE ROUND'S OWN DEFECT CLASS SURVIVED IN A SIBLING THE ROUND DID NOT LOOK AT.
  `validate()` has SIX branches; round 2 pinned five by name and left
  `a_reset_minute_outside_the_day_is_refused` as a bare `is_err()` with no
  boundary - the exact A1 shape the round existed to eliminate. It escaped
  because it was not one of the four A1 named. WHEN A ROUND FIXES A DEFECT CLASS
  IN A FUNCTION, SWEEP THE WHOLE FUNCTION, not the reported instances: the
  finding names where the class was noticed, never where it lives. Now pinned at
  1439-accepted / 1440-refused with the exact message; bite-checked by flipping
  `>=` to `>`, which fails on the 1440 case by name.

## The engine/protocol document, round 3: the arm classification, and referents

- `Engine::arm`'S MATCH IS EXHAUSTIVE ON BOTH ARMS, and the `queued @ (...)`
  binding is load-bearing rather than stylistic. This is the SECOND instance in
  two rounds of round 2's rule (a comment claiming something about future
  variants is a compiler obligation); the difference worth noting is that A3's
  `matches!` at least classified today's variants, while a `_`/`other`
  catch-all classifies nothing at all - a new server-owned variant fell through
  into the armed queue as an entry `take_armed` can never consume. WHEN A
  FINDING SAYS "A TEST MUST HOLD THIS", CHECK WHETHER THE MATCH HAS A CATCH-ALL
  FIRST; deleting the catch-all is cheaper than any test and strictly stronger.
- A LOOP OVER AN ENUM'S VARIANTS OWES A SECOND, INDEPENDENT CLASSIFICATION.
  `arm_classifies_every_divergence_variant`'s expectation is its own exhaustive
  match, not a call to the production predicate and not `!is_server_owned` over
  one shared list: a single list lets a new variant be classified once and read
  twice, which is the accident the production match is guarding. What NOTHING
  can hold, and it is stated on the fixture rather than left implied, is that
  the hand-built case list stays complete - a forgotten variant is still
  classified deliberately on both sides, it just goes unexercised.
- A CONSTANT WITH NO SECOND DEFINITION HAS NO REFERENT IN ITS OWN CRATE, AND ITS
  REFERENT IS USUALLY THE SUBSTITUTION IT SERVES. `DEFAULT_REQUEST_TIMEOUT_SECS`
  was pinned against its own literal in `mogwai-protocol`; the claim with two
  sides is the adapter's `request_timeout_secs`, where `0` means "keep the
  shipped default", and that branch had NO coverage in the workspace - deleting
  it left everything green, with every unconfigured client silently dropping to
  the 1-second `MIN_WALL_REQUEST_TIMEOUT_SECS` floor. Pinned now in
  `mogwai-adapter/src/client/shared.rs`. GENERAL FORM for the remaining
  documents: when a finding says "delete this, it pins nothing", ask what the
  constant is FOR and look for that in the crate that consumes it - the answer
  was one crate away and was a real hole, not a tidy-up.
- A CROSS-CHECK LIVES WHERE BOTH SIDES EXIST, AND THE VALUE PIN IS NOT
  REDUNDANT. `default_instruments()`'s terms are now read back out of
  `Engine::new`'s order validation
  (`the_default_seed_puts_the_engine_on_a_btcusdt_cent_and_satoshi_grid`), while
  the protocol-side test keeps its literals under an honest name. Both fire on a
  changed increment, which is right: one says the wire defaults moved, the other
  says the venue's behaviour moved with them.
  - AND A SHARED `expect`-STYLE HELPER CAN COST A FINDING ITS ATTRIBUTION. The
    first cut read the refusals through `reject_reason`, whose panic on an
    ACCEPTED order is "expected one order reject" - a message about the helper's
    shape, with the bite-check's actual defect invisible. Destructured locally
    with a message naming the increment instead. Same family as the adapter
    document's shared-wait-helper rule: a helper called from many sites reports
    its own expectation, never the caller's.

COLD REVIEW OF THE ROUND FOUND FOUR, all real, all fixed in the same commit:

- THE ROUND'S OWN RULE WAS NOT APPLIED AT THE SITE THAT MATTERED MOST. Deleting
  `Engine::arm`'s catch-all secured the ENGINE's classification and nothing
  else: `mogwai-server`'s `arm_divergence` - the ROUTING site, where a
  misclassification loses a user-visible control rather than parking a dead
  queue entry - still ended in `engine_div => ...` guarded by a
  `debug_assert!`, and that assert is COMPILED OUT of the release profile the
  socket suites run in. It was also the THIRD hand-maintained copy of the
  server-owned list, kept in sync by nothing. The router now enumerates the
  five engine-armed variants, so a new variant fails this crate's build in both
  profiles. GENERAL FORM: when a round deletes a catch-all, grep the whole
  workspace for the OTHER matches over the same enum before calling it closed -
  the classification is only as strong as its weakest routing site, and a
  `debug_assert!` is not a routing guard.
- A CONTROL ADDED TO PREVENT A VACUOUS TEST WAS ITSELF VACUOUS, the arc's
  seventeenth instance and the most ironic. The zero-band control submitted at
  a stated price of 100 against `last_px: 100` and asserted the fill was 100 -
  but `draw_market_price` IGNORES the stated price, which is the property the
  control exists to distinguish, so an engine returning the stated price and
  never reading the band passed every zero-band assertion. Fixed by one number:
  the control's last print is 99 against the stated 100. A CONTROL MUST VARY
  THE INPUT THE MECHANISM UNDER TEST READS; where two inputs are equal in the
  fixture, the control cannot tell which one the code used.
- A LENGTH ASSERT BELONGS BEFORE THE INDEXING IT LICENSES. Moving
  `assert_eq!(out.len(), 4)` after `out[1]`/`out[2]` turned a short event list
  from a named count mismatch into a bare index panic.
- `field_reassign_with_default` (`let mut x = T::default(); x.f = v;`) appears
  nowhere else in this workspace; use struct-update syntax. The instance also
  hid a no-op - `ConnHavoc::default()` already sets `request_timeout_secs: 0`,
  so that case varies the SPEC's presence, not the field, and the comment says
  so now. `SimClock::identity()` already is the `0, 0, 1.0` literal the test
  hand-built.

## The engine/protocol document, round 4: the wire-size constants

- `sizing.rs`'s TEST MODULE NOW CARRIES A MAXIMAL-FIXTURE KIT - `worst(len)`
  (U+0001 fill), `maximal_balance`, `maximal_position`, `maximal_margin`,
  `maximal_status_row`, `maximal_fill`, `empty_shape` - and nothing there may
  hand-build a row again. The kit's rule is every string at its cap, every
  `Decimal` at `Decimal::MIN`, every OPTIONAL FIELD PRESENT and every enum at
  its longest spelling; a fixture that drops one of those measures a narrower
  struct than the constant bounds, which is the exact defect the round closed.
- THE ACCOUNT ID IS CHARGED AT ITS RAW CAP AND THAT IS A RULING, not an
  oversight to re-tighten. `AccountId` is the only validated-alphabet string in
  the wire types, its sole constructor is `AccountId::parse`, and its alphabet
  (ASCII alphanumerics plus `.`, `_`, `:` and `-`) contains nothing
  `serde_json` escapes, so `JSON_ESCAPE_FACTOR` there bought 320 bytes of dead
  reservation per account snapshot rather than any safety.
  `the_account_id_alphabet_carries_nothing_json_escapes` is the premise's pin
  and its failure message says the factor is owed back. Anyone widening that
  alphabet - or adding a second validated-alphabet type and reaching for the
  same shortcut - owes that test a look first. NO OTHER STRING IN THE MODULE
  QUALIFIES: symbols are `Arc<str>`, ids and currencies are `String`, all
  capped in LENGTH and not in alphabet.
- THE SIZING DERIVATIONS ARE TIGHT, WHICH SETTLES HALF OF SECTION D BEFORE
  ROUND 5 OPENS IT. Measured worst cases against their bounds: balance
  288/234, position 832/785, margin 480/405, order status 1856/1830, fill
  2208/2184, snapshot envelope 512/474, account-state envelope 208/164. So the
  "over-reserves by 1000x also passes" worry has no instance in these
  constants; if round 5 adds a slack ceiling, `2 * analytic` is comfortable
  everywhere and anything tighter than about 1.25x would fail on the loose
  account-state envelope alone.
- A WIRE-SIZE FIXTURE MEASURES THE TAGGED FRAME, NEVER THE BARE STRUCT.
  `ServerMessage` is `#[serde(tag = "type")]`, so a snapshot struct serialized
  on its own is about 21 bytes short of what the server reserves for -
  `"type":"AccountState",`. The round's first account-state fixture measured
  the bare struct and reported 142 where the frame is 164. The general shape,
  and it is NEW FOR THIS ARC: THE ROUND'S OWN PRODUCTION IMPROVEMENT MADE AN
  EXISTING MEASUREMENT ERROR MATTER. Dropping the dead escape factor on the
  account id was right and it also removed the slack that had been hiding a
  22-byte under-measurement in the test guarding the same term. When a change
  TIGHTENS a bound, re-derive what the tests around it measure - a fixture that
  was merely imprecise under generous slack becomes the thing that can no
  longer bite.
- `rust_decimal` SERIALIZES A `Decimal` AS A JSON STRING, not a number, and
  DESERIALIZES from either. Both facts bit during the round: the string form is
  why the row measurements run two bytes over a naive digit count, and the
  lenient parse is why an equity fixture written with `"multiplier":1` decodes
  fine.
- A `rename_all` PERTURBATION IS NOT A BITE-CHECK FOR A SINGLE-WORD WIRE TAG.
  Flipping `InstrumentClass`'s `snake_case` to `camelCase` left every tag
  (`spot`, `future`, `equity`, `perpetual`, `inverse`) byte-identical and the
  round-trip test correctly stayed green. The perturbation that bites is an
  explicit `#[serde(rename = "...")]` on one variant.

## The engine/protocol document, round 5: what carries to `bugs-tests-lab-cli`

The engine mechanics die with that document - the reservation cache, the FOK
refusal texts, the sizing constants. What generalises:

- A ONE-SIDED BOUND ASSERTION IS HALF A TEST, AND THE CEILING BELONGS TO THE
  FIXTURE THAT DEFINES THE CONSTANT. `actual <= bound` is satisfied by a
  derivation that over-reserves by any factor, and over-reservation is rarely
  free. But the ceiling has to go where the constant is measured against its
  MAXIMAL case, not on an end-to-end sample: measured across the engine's
  reservation matrix the same claim ran 2.2x to 249x, because the bound covers
  the widest output a command CLASS can produce while the sample feeds one
  command to one book. MEASURE THE RATIO SPREAD BEFORE ADDING A CEILING - a
  spread of two orders of magnitude means a per-case table of magic numbers that
  reads as a gate and is not one, and the honest move is a reasoned refusal
  written beside the assertion. Where the ceiling does belong, `2 * actual` is
  this workspace's established factor and the loosest real bound sat at 1.27x.
- "THE STATE IS UNTOUCHED" ASSERTED ON AN EMPTY FIXTURE IS A DIFFERENT CLAIM.
  A praised refusal test asserted `balances.is_empty()` after each refusal - on
  an engine constructed with no balances, so it said "no row was CREATED", never
  "no funds moved", and a refusal that debited or locked funds passed it for
  free. Measured: the perturbation that fails the funded form leaves the empty
  form green. Any before/after assertion needs a starting state the defect could
  actually damage, and this is the arc's signature defect wearing the costume of
  a test everyone points at as the good example. VERIFY PRAISE BY PERTURBING,
  not by re-reading.
- A FUNCTION WHOSE EVERY EXISTING USE IS A POSITIVE CASE HAS NO COVERAGE. A
  validator with two production call sites and one test using it as an ORACLE on
  a known-good value passed the whole workspace with a body of `Ok(())`: the
  refusals are what the call sites exist for and nothing exercised one. The
  audit that finds this is grep-the-callers-and-ask-which-way-each-one-points,
  and it is worth doing before writing tests for the whole file the finding
  named - three of the functions on that list turned out to be covered directly
  by another crate's test module.
- A DEBUG-ONLY WITNESS IS A REASON TO FIND AN OBSERVABLE, NOT ONLY A REASON TO
  ADD `#[cfg(debug_assertions)]`. The gate is correct when the panic really is
  the only witness; where the test can read the state the debug check would have
  compared, the assertion works in BOTH profiles and is strictly stronger. In
  the instance found this round the debug reconciliation would not even have
  fired on the perturbation, so the gate would have preserved coverage that was
  not there. Ask what the check compares, and whether the test can compare it.
- TWO SIBLING MESSAGES ONE CLAUSE APART ARE NOT DISCRIMINATED BY `contains`. The
  bite-check is swapping the two production strings: an assertion that survives
  the swap is naming a word both refusals share.
- THE DOC-DRIFT FAMILY RECURS IN THE FILE A ROUND JUST FINISHED. A comment
  naming the "widest" of two derivations had it backwards, in the same module a
  previous round had spent itself correcting, and it was found by MEASURING both
  fixtures rather than by reading the prose. A round that measures should print
  what it measured and compare it against what the file claims.
- A FIX MAY TRADE ONE BLIND SPOT FOR ANOTHER WHILE READING AS STRICTLY
  STRONGER, and this is the sub-shape to check for whenever a weak assertion is
  REPLACED rather than added to. The instance: `balances.is_empty()` on an empty
  fixture ("no row was created") was replaced by a tuple on the one seeded
  currency row ("no funds moved") - each catches a perturbation the other passes,
  measured in both directions, and the replacement silently dropped the first
  claim. THE BITE-CHECK THAT FINDS IT IS TWO-DIRECTIONAL: perturb for the new
  claim AND for the claim being retired, and confirm the retired one still has
  an assertion. Prefer keeping both lines to arguing that one subsumes the other.
- A NEW CONSTRAINT INVALIDATES THE PROSE THAT LICENSED ITS ABSENCE. Adding a
  slack ceiling made a module header advising "round generously upward" a
  description of a test failure, three lines from the tests that now enforce it.
  When a change adds a rule, grep the file it lives in for the sentence stating
  the OLD policy - a durable header is what an implementer reads first, and it
  will point them the wrong way for years.
- AN ASSERTION ON A CONSTANT IS NOT AN ASSERTION ON THE MESSAGE THAT STATES IT.
  `assert_eq!(CAP, 64, "the refusal message states the cap")` holds the constant
  against a literal while the production message carries its own hardcoded "1 to
  64" - the durable-prose-asserting-a-live-fact shape, inside a string literal
  where no prose gate can see it. Assert that the refusal TEXT contains the
  constant.
- A DISPATCH TEST OVER A MATCH IN THE SAME FILE HOLDS ONE THING: THE ARMS ARE
  NOT TRANSPOSED. `f(x) == f(x)` modulo the arm is worth a few lines, but the
  drift a reader will assume it covers - the caller choosing the wrong variant -
  is somewhere else entirely. Write the narrow claim on the assertion, or move
  the broad one to a test that exercises the caller.

## The engine/protocol document, close pass: what the five commits left

The arc is SOUND. The gate is green at 1220 + 440, 1717 pairs, 1660 run, 57
ignored, 0 orphaned. Every one of the six production changes was re-derived
against the code rather than read off the commit messages, and all six are
either behaviour-preserving or intentionally not: `shipped_policy`'s membership
gate is preserving (the list has five names and the match five arms, one for
one); `is_execution` / `is_admission` as exhaustive matches classify today's
variants exactly as the `matches!` did; both catch-all deletions are preserving
because the arms above already intercepted every other variant; `validate()`'s
`trim()` and `account_state_max_bytes` dropping the escape factor are the two
deliberate behaviour changes, each with its premise pinned by a test; and
`launch`'s poll counter and parameterised interval are additive, with the
counter written on every build and read only under `cfg(test)`.

TWO BITE-CHECK CLAIMS WERE RE-RUN RATHER THAN TRUSTED, both the load-bearing
ones, and both hold exactly as recorded. Restoring `if asked_to_stop { break; }`
ahead of the `try_wait` fails `a_venue_that_ended_during_shutdown_still_records
_its_exit` on the EXIT-RECORD assertion (`None` against
`Some(VenueExit { success: true, code: Some(0) })`) in dev and in the timing
sweep alike, with the poll assertion green - which is the whole point of making
the arm an observable. Deleting the zero-hold `filter` in
`order_reservation_entry` fails
`a_zero_initial_margin_policy_cannot_drift_the_reservation_cache` in BOTH
profiles, naming `{"USD": 0}`, so the round-5 claim that the strengthened
assertion needs no `cfg(debug_assertions)` gate is measured, not argued.

THE ONE FINDING IS PROSE, IN THE SECOND HALF NOBODY COLD-REVIEWS, AND IT IS THE
FOURTH DOCUMENT RUNNING THAT THIS IS WHERE THE FIND IS.

- `sizing.rs`'s MODULE HEADER CLAIMED A GATE OVER MORE THAN THE GATE COVERS.
  Round 5's cold review correctly made the header state the new `2 * actual`
  ceiling; the sentence it wrote says "this module's tests bracket EVERY BOUND
  from both sides against the maximal fixture that defines it", and the
  `brackets` doc comment opened with "EVERY BOUND IN THIS MODULE IS ASSERTED
  FROM BOTH SIDES". Neither is true, and the untrue part is precisely what the
  same round REFUSED: `LINKAGE_MAX_BYTES`, `BOUNDARY_REFUSAL_BYTES`,
  `swept_fill_max_bytes` and `swept_batch_max_bytes` have no single maximal
  struct to be measured against and get no `brackets` call, and
  `worst_case_output_bytes` gets one on its two QUERY arms only. So a reader
  adding a composed bound would believe a ceiling was watching it. Both
  statements now scope to the per-struct bounds, enumerate what is covered, and
  carry the refusal's reason (2.2x to 249x over the engine's reservation matrix)
  where the next implementer will hit it. THE GENERAL SHAPE IS THE ARC'S
  SIGNATURE ONE MOVED UP A LEVEL: not a test that cannot fail, but a
  DESCRIPTION of a gate that is wider than the gate - and it was produced by the
  fix for a finding about a header that described the OLD policy. Correcting a
  durable sentence is the moment to check its new scope, because the sentence
  that replaces an under-claim naturally over-claims.

RESIDUALS CARRIED, all three left standing on their stated grounds:

- REFUSED, and it stands: `read_ready`'s `NoRecord` 50 ms pause. `NoRecord` is
  EOF on the stdout pipe, which a LIVE venue can produce, so "drain to EOF" is
  an unbounded wait on a child that is still running. Pinned by
  `a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure`.
- REFUSED, and it stands: a per-case slack ceiling in
  `worst_case_reservation_covers_actual_output`. The header correction above is
  now where that refusal is written down for a reader who never opens the notes.
- CARRIED: `" USD "` - a currency with SURROUNDING whitespace - is accepted by
  both validators and matches no balance. The stronger rule (refuse any code
  differing from its trimmed form) belongs on `AccountPolicy::validate` and
  `validate_divergence` at once, and was deliberately not smuggled in under a
  finding about blanks. It is a candidate for whichever document next touches
  either validator.
- UNPINNED BY RULING, not by omission: that the OWNING THREAD writes the
  teardown slot on a real failed kill. Provoking one means reaping the child out
  from under the owner, which leaves `Child::kill` signalling a pid the kernel
  may have recycled. The ORDERING around that slot is pinned
  (`the_teardown_detail_is_read_after_the_owner_joins`), and the healthy
  direction is pinned too.

## The lab/cli document, round 1: A1, C1 and C3

- `the_control_artifact_carries_no_b8_field` NOW READS THROUGH `repo_root()` and
  PANICS on a missing artifact. It had `std::fs::read(DEFAULT_OUT)` with an
  early-return on error, and `DEFAULT_OUT` is repo-relative while a unit test's
  CWD is its crate, so the read failed on every run since the test was written
  and not one of its assertions had ever executed. BITE-CHECKED IN BOTH
  DIRECTIONS, which is what made it a proof rather than an assertion: with an
  impossible `"B9"` added to the required-gate list the FIXED test fails "B9 is
  missing", and the OLD relative read carrying the same impossible assertion
  PASSES. A test that passes while asserting something false is the vacuity
  demonstrated, not argued.
  - WHAT THEY FOUND ON FIRST EXECUTION: nothing. The committed artifact's
    `gates` map is exactly `B1..B7` with no `B8`, so all four claims are true.
    The defect was entirely in the test.
- `protocol9_tape_oracle` IS NO LONGER `#[ignore]`d, and the reason is worth
  stating as a rule rather than as a fix: ITS ONLY STATED GROUND WAS A COST, AND
  THE COST WAS FALSE. The string said "walks seven 6-hour streams" when the
  matrix has been THREE rows since the ETHUSDT and SOLUSDT presets were retired,
  and the whole test measures comfortably inside the watchdog. It was in no skip
  list and the gate sets `include_ignored`, so the attribute bought only its
  exclusion from `brokkr check`. Checked for a surviving ground and there is
  none - not environment (no socket, no corpus, the fixture is committed), not
  policy - so the attribute was DELETED rather than given a corrected sentence.
  The alternative close, rewriting the cost string, would have kept a claim that
  has to be re-measured forever to stay true.
  - THE PRAISE IS DESERVED, checked rather than taken on trust. The write branch
    is guarded by TWO independent conditions - the fixture must be ABSENT and
    `TAPE_PROTOCOL_VERSION` must equal 9 - and the version assert precedes the
    write, so it cannot re-bless later-protocol output. Note the write targets
    `analysis/`, not `tests/fixtures/`, so the tape document's
    `no_test_binary_writes_a_committed_fixture` enforcer does NOT cover it. That
    is not a hole to close here: the enforcer's own note says an unguarded
    writer elsewhere owes its own rule, and this writer is guarded more tightly
    than the rule would require.
  - AND THE TAPE-LINEAGE FACT IT ESTABLISHES IS NOW DURABLE, in
    `reference/architecture.md` beside the version narrative, because a reader
    who knows bumps are unconditional will assume the opposite: the fixture
    frozen at identity 9 STILL MATCHES at the current identity, so none of 12,
    13, 14, 15 or later moved the bytes it observes. THE SENTENCE NAMES NO
    CURRENT VERSION NUMBER, deliberately: `tape_version_prose.rs` sees only two
    literal phrasings, so a durable claim written any other way survives the
    next bump uncaught, and the round's cold review caught exactly that draft.
    SCOPED DELIBERATELY - six simulated hours,
    BTCUSDT only, offline generation path, three arms - because the sentence
    replacing an under-claim naturally over-claims.
- C3 IS REFUSED, AND THE MEASUREMENT INVERTED THE FINDING.
  `the_control_walk_pair_replays_one_tape`'s 24-hour window was filed as a
  latent gate-breaker to be cut to an hour. It is nowhere near the watchdog in
  any of the three sweeps. More decisively, THE DAY IS THE SHORTEST
  WINDOW THAT MEASURES ANYTHING AT ALL: `window_start_ns` is 2026-06-30T22:00:00Z,
  an MNQ session open, and `per_session` records are emitted per COMPLETED
  session, so 1 h, 6 h AND 12 h each yield ZERO hours and ZERO parents while
  24 h yields 23 hours and 1,455,942 parents. The cliff is the session boundary,
  not a gradient - shortening this pin does not weaken it, it empties it.
  - THE SIBLING'S ARGUMENT DOES NOT TRANSFER, which is what the finding leaned
    on. `arrival_control_refuses_a_tree_that_changed_during_the_run` uses an hour
    and says why in place: it pins STEP ORDERING and needs only to reach its
    gates. This pin's content is two passes agreeing over the tape they walked,
    which is a claim about coverage.
  - THE ROUND ADDED THE COVERAGE ASSERTION rather than only a comment, because a
    comment does not bite. The covered-hour count fails first and by name on any
    shortened window; the pre-existing `parents > 0` could not have caught a cut
    that still landed one busy hour. Bite-checked at 6 h and 12 h.
  - THE EXPECTED COUNT IS DERIVED, not written down - `gate_hours(&profile).len()`,
    the same call the module's other tests make - so an MNQ calendar change fails
    at `gate_hours_excludes_the_unexposed_hour`, which owns that fact, rather
    than here behind a message about window length. Same move as `d0318a3`.

## The lab/cli document, round 2: the tree-state seam

- `mogwai_lab::ledger` NOW READS THE TREE THROUGH A SEAM, and nothing may
  hand-roll a second one. `TreeOracle` serves the two RAW git readings
  (`TreeQuery::Status`, `TreeQuery::Head`, each a `TreeReading { success,
  stdout }`); `GitTreeOracle` is production and is the DEFAULT whenever nothing
  is installed; `install_tree_oracle(Rc<impl TreeOracle>)` returns a guard that
  restores the previous oracle on drop. `require_clean_tree` AND
  `fresh_tree_state` both go through it, so a run's step-1 gate and its
  pre-write re-attestation are scriptable together.
  - THE SEAM DOES NOT SHIP, AND THAT IS A NEW SHAPE FOR THIS ARC: a test seam
    that WEAKENS A PRODUCTION INTEGRITY GUARANTEE. The first cut exported
    `install_tree_oracle`, `TreeOracle` and `ScriptedTree` unconditionally from
    a crate `mogwai-cli` depends on normally, so they landed in
    `target/release/mogwai` - and one `ScriptedTree::clean("deadbeef")` install
    forges BOTH ENDS of the fail-closed contract at once, the step-1 commit
    that becomes `binding.harness_tree_commit` and the pre-write
    re-attestation, with `"clean_tree": true` written beside it. Nothing wrote
    a forged artifact only because each clean-direction test happened to die on
    a missing input first, which is one refactor from being untrue.
    CLOSED WITH BOTH AVAILABLE GUARDS, because neither is sufficient alone:
    - A CARGO FEATURE. The whole seam is `#[cfg(any(test, feature =
      "test-seam"))]`, and `mogwai-cli` enables `test-seam` ONLY in
      `[dev-dependencies]`. Under resolver 3 a dev-dependency's features are
      not unified into a build compiling no test target, so a release build
      gets a `read_tree` that calls `GitTreeOracle` directly with no
      thread-local and no installation point. `test` is in the cfg so
      `mogwai-lab`'s own unit tests need no self-dependency. VERIFIED ON THE
      ARTIFACT rather than argued: `ScriptedTree::dirty`'s porcelain literal is
      absent from the built `target/release/mogwai`.
    - A RUNTIME REFUSAL. `tree_readings_are_production()` exists in BOTH
      configurations - a thread-local probe with the seam, a constant `true`
      without - and every artifact writer bails on it immediately before its
      write. IT WAS TWO WRITERS OF SIX until the close pass; see that section.
      This is what survives an
      `--all-features` release build, which the cfg alone does not. In a
      production build the branch folds away entirely, which is why its message
      is also absent from the binary.
    THE GENERAL RULE, and it is the one to carry: A TEST SEAM ON A GATE WHOSE
    OUTPUT IS A PROVENANCE CLAIM IS AN ATTACK ON THAT CLAIM. Ask of every new
    seam what a caller could ASSERT by installing a double, not just what a
    test can provoke. Where the answer is "a record somebody trusts", the seam
    is feature-gated and the writer refuses on it.
  - THE READINGS ARE RAW ON PURPOSE. A double returning a `clean: bool` verdict
    would move the gate's own decision into the double and leave the production
    branch untested; serving the bytes `git status --porcelain` really prints
    keeps the decision where it is asserted.
  - `ScriptedTree` TAKES BOTH READINGS, and the first cut did not - it
    hardcoded the head reading as a success, so `unreadable()` modelled a
    failing `git status` ONLY. Two consequences, both real: `require_clean_tree`
    has FOUR outcomes and the `rev-parse` failure was unreachable through the
    double and untested, and `fresh_tree_state`'s `&& head.success` term could
    be DELETED with every test still green (measured, not argued). The shape is
    not exotic - a repository with no commits or an unborn HEAD gives a
    succeeding, empty `git status --porcelain` over a failing `rev-parse` - and
    it is the case where the untested branch matters most, because binding the
    trimmed empty stdout would bind an artifact TO NOTHING rather than error.
    `head_unreadable()` models it, and both branches now have a test that bites.
    The same fix repaired a FALSE RED in the double's own audit: it selected the
    scripted constructor on the status bytes alone, so in a repository with no
    commits it compared `clean("")`'s successful bind against the real reading's
    refusal and blamed the double. The selection is on both readings' success.
  - `ScriptedTree` RECORDS EVERY QUERY IN ORDER
    (`queries()`). That log is what makes the ordering claim assertable: a
    dirty or unreadable tree costs `[Status]` alone, a clean one costs
    `[Status, Head]`, and a command that must not consult the tree at all -
    `arrival-screen --cost-probe` and `--demand-census` - must leave it EMPTY.
    Asserting on the log rather than on the refusal text is the whole repair
    for A3: `!error.contains("clean")` is satisfied by a file-not-found error
    unconditionally, so the old assertion could not fail on the clean tree
    every gate run happens on.
  - THE DOUBLE IS AUDITED AGAINST THE REAL ENDPOINT by
    `the_scripted_double_reproduces_the_real_git_readings`, which replays THIS
    tree's actual `git` output through the seam and requires the gate to reach
    the same verdict, then requires the matching `ScriptedTree` constructor to
    reach it too. It is the one test in that module allowed to read the ambient
    tree, because comparing against it is its job, and it passes in all four
    states.
    - IT IS ALSO THE ONLY SUBPROCESS THIS CRATE'S SWEEP SPAWNS, which is a cost
      taken deliberately against that sweep's fast, sandbox-safe framing. `git`
      NOT ON PATH RETURNS RATHER THAN PANICS - there is nothing to audit the
      double against, and reporting that as a defect in this workspace would be
      a wrong answer. Every other outcome, both commands failing outside a
      repository included, is a state it compares in.
    - WHERE ITS SECOND ASSERTION BITES IS NOT WHERE IT RUNS. On a clean tree
      `clean()`'s status bytes are bit-identical to what real `git status`
      printed, so both sides get the same input by construction and the
      comparison is near tautological; the signal is in the other three states,
      where the constructor INVENTS the reading. A clean gate machine therefore
      exercises the weakest of the four. Stated in place rather than
      rearranged, because it is a limit of comparing against whatever tree is
      there.
  - THE GUARD'S RESTORE IS PINNED (`an_installed_oracle_is_removed_when_its
    _guard_drops`), because a leaked oracle would silently re-point whatever
    runs next on that thread. Thread-local is the right scope: libtest gives
    each test its own thread, and these commands read the tree on the thread
    that entered `run`.
- THE FOUR DIRTY-TREE TESTS NOW ASSERT BOTH DIRECTIONS AND NEITHER TOUCHES GIT.
  `fit_refuses_a_dirty_tree_before_the_corpus_and_binds_a_clean_one`,
  `minute_range_envelope_refuses_a_dirty_tree_before_reading_inputs`,
  `arrival_control_refuses_a_dirty_tree_before_reading_inputs` and
  `arrival_screen_refuses_a_dirty_tree_before_reading_inputs` each inject dirty
  then clean. THE EARLY-RETURN GUARDS ARE GONE and must not come back: they
  were added 2026-08-09 against the inverse defect (the tests asserted the
  development tree was dirty and went red at every clean commit) and the cure
  preserved the disease - all four self-disabled in exactly the state the gate
  runs in.
  - THE ORDERING IS ASSERTED POSITIVELY IN BOTH DIRECTIONS, not by the absence
    of a word. Dirty: the refusal is the tree's AND does not name the input.
    Clean: the refusal is the FIRST INPUT'S, named. That needed two production
    messages to carry their path - `minute_range_envelope`'s `verify_input` and
    `arrival_screen`'s measurement read both emitted a bare
    "No such file or directory", indistinguishable from every other read in the
    command. `arrival_control` needed none: its first input is the B5
    transcript and "B5 refused" already names itself.
    - THE NAMING GOES ON THE READ, NOT AROUND THE CALL, and the first cut got
      this wrong in a way that RELOCATED the defect instead of fixing it: it
      wrapped the whole of `ScreenContext::open` as "opening the 12a
      measurement". `open` also parses the JSON, builds the 12a binding,
      resolves the MNQ preset, computes gate hours and hashes
      `analysis/fingerprint.json` - so a corrupt fingerprint or a broken preset
      reported as a measurement-path failure, which is the exact class of
      unattributed error the change existed to remove. It also weakened the
      three tests keying on the string: a fingerprint-hash failure satisfied
      them just as well, so they pinned "we got past the tree gate" rather than
      "we reached the measurement". `open` now names its own `fs::read` and
      `from_slice`, and the callers wrap nothing.
  - `arrival_control`'s clean direction PASSES AN ABSENT `b5_log` EXPLICITLY.
    That is what retires the guard's stated reason - "on a clean tree calling
    `run` would launch the real six-minute artifact run" - because the run now
    dies on the missing transcript before any walk.
    - AND IT RUNS WITH `run_b1: false`. On production `Seams` the test's outcome
      depends on `target/no-such-b5-for-the-ordering-pin.log` NOT EXISTING: if
      it ever did, the run would sail past B5 into `run_b1`, which execs a
      `mogwai gen` subprocess per symbol from inside a unit test. Low
      probability, high blast radius, and the ordering claim is about the B5
      read, which is upstream of B1 either way - so disarming B1 removes the
      hazard without touching the pin. The dirty direction stays on `run` so
      the public entry point's wiring is still pinned.
- `ResourceSampler` GAINED A `Drop`, and the round is what made it bite.
  `arrival_control`'s `run_with` starts the 1 Hz sample thread at its top and
  only `stop`s it at the very end, so every `?` between them leaked the thread
  for the life of the process - latent while those early returns were reachable
  only from an operator run that then exits, and NOT LATENT ANY MORE, because
  the new clean-direction assertion drives one of them IN-PROCESS inside a test
  binary that goes on to run every other test in the crate. The guard-scope
  family with a thread as the resource. Pinned by
  `a_dropped_sampler_stops_its_thread_rather_than_leaking_it`, which asserts on
  the stop flag and SAYS IN PLACE that the join is not observable from there,
  rather than dressing the gap in a predicate that cannot fail.
  - BITE-CHECKED IN THREE DIRECTIONS, all as text edits. Moving each gate to
    AFTER its first input read fails all four on the dirty assertion, each
    naming the input error it got instead. Making `require_clean_tree` never
    bind fails all four on the clean assertion. Making `arrival-screen`
    consult the tree unconditionally fails the cost-probe and census tests on
    the QUERY LOG ("the probe consulted the tree state: [Status]") - the
    assertion that replaced the vacuous one.
- `demand_census_needs_no_clean_tree_and_writes_no_screen_artifact` GOT THE
  SAME REPAIR though the report named only the cost probe: identical shape, one
  line, and leaving it would have left a second instance of a closed finding.
- `arrival_control_refuses_a_tree_that_changed_during_the_run` IS DELIBERATELY
  NOT ON THE SEAM and must stay off it. Its mechanism is planting a real
  untracked file at the repository root and having the run's own re-attestation
  see it; scripting that reading would pin the test against itself. It keeps
  its `#[ignore]`, its clean-tree precondition and its drop guard.

## The lab/cli document, round 3: the six tests that could not fail

Section A is CLOSED. The fix pass's gate was green at 1227 + 447, 1731 pairs,
1674 run, 57 ignored, 0 orphaned - one new test in each sweep,
`a_fifo_attaches_only_for_a_nonempty_openable_path`; the review repair below
took it to 1229 + 449. Two production seams were made PURE rather than
injectable, which is round 2's ruling applied ahead of time: a pure function
taking already-read values is strictly better than an installable double,
because there is nothing to install and therefore nothing to spoof.

- `storage::cache_root_from(Option<&Path>, CacheEnv)` IS THE
  PRECEDENCE RULE and `cache_root` is now a two-line wrapper that reads the
  environment and holds no logic. `CacheEnv` carries the three readings as
  `Option<&str>`, and the EMPTY-IS-UNSET rule lives in the pure function, so all
  four limbs plus every empty-string fall-through are asserted without touching
  the process environment. No call site changed. Bite-checked by transposing the
  XDG and HOME limbs: fails naming `/from/home/.cache/mogwai` against
  `/from/xdg/mogwai`. Reach for this shape before reaching for a seam.
  - BOTH NAMES ARE `pub(crate)`, NOT `pub`, and the reason is round 2's
    shipped-seam incident: a testability helper made public took a cargo feature
    AND a runtime refusal to withdraw. `mod tests` is in the same file, so
    `pub` bought nothing. WHEN A PURE RULE IS EXTRACTED FOR A TEST, EXTRACT IT
    AT THE NARROWEST VISIBILITY THE TEST CAN REACH.
  - AND MAKING THE RULE PURE DID NOT REMOVE THE UNTESTED HOP, IT RELOCATED IT -
    the arc's twenty-second signature instance, in a sub-shape worth naming:
    A REFACTOR THAT MOVES AN UNTESTED HOP RATHER THAN ELIMINATING IT. The first
    cut had `CacheEnv::read()` returning a bare
    `(Option<String>, Option<String>, Option<String>)` and `cache_root` doing
    the positional unpack. THREE SAME-TYPED VALUES IN AN UNNAMED TUPLE, and the
    only wrapper assertion was `cache_root(Some(&cli))`, which returns on the
    first line before any reading is consulted - so transposing the `HOME` and
    `XDG_CACHE_HOME` slots left the whole suite green while a real run resolved
    its cache to `~/mogwai` instead of `~/.cache/mogwai`. MEASURED, not argued:
    the transposition installed as a text edit passes
    `cache_root_prefers_override_then_env_then_xdg` on all three sweeps.
    The read now goes through an owned `AmbientCacheEnv` whose fields carry the
    three names, so both hops - variable to field, field to borrow - are
    name-to-name, and `the_ambient_reading_binds_each_variable_to_its_own_limb`
    pins the variable literals at the read site. Bite-checked with the same
    transposition: fails "the XDG limb is not fed from XDG_CACHE_HOME",
    `Some("/home/folk")` against `None`. The test SAYS SO on stderr when the
    ambient environment cannot discriminate (all three readings equal), rather
    than passing silently as a check that did not happen.
- `mogwai_lab::sidecar::resolve_fifo(Option<OsString>)` IS THE ATTACHMENT RULE,
  same move. Unset, empty and unopenable all degrade to no channel, and that is
  now asserted rather than traversed. `write_line_to(&Channel, &str) -> bool`
  reports whether a line reached a handle, which is what turned a
  four-calls-zero-asserts test into one with an observable; production callers
  bind it to `_inert`, because the module's own header forbids a return value a
  caller can branch on. Bite-checked twice: falling back to `/dev/null` on a
  failed open fails the attachment test on the `"/"` case, and returning `true`
  with no FIFO fails `emission_without_a_fifo_is_inert` on `!write_line(...)`.
  - THE `OnceLock` ORDER-COUPLING IS REAL BUT NOT A DEFECT, measured rather than
    argued. Whichever test reaches `init()` first fixes `CHANNEL` for the whole
    binary - but the resolution is a pure function of one environment variable
    that no test in this workspace mutates, so both orders resolve identically.
    What the coupling DOES bind is the epoch, which is why A6's bound below is
    the sweep's rather than the test's.
  - THE SAME RELOCATED HOP WAS IN `channel()`, smaller blast radius and the
    same residual: it was the ONLY code naming `MARKER_FIFO_ENV`, and no test
    named the variable at all, so re-pointing the channel at a different
    variable left every sidecar test green while the harness's FIFO went unread
    forever. The reading is `configured_fifo_path()` now - one named function,
    the single hop from variable to rule - and
    `the_channel_reads_the_documented_variable` writes the literal
    `BROKKR_MARKER_FIFO` out rather than taking it from the constant, which
    would pin the constant against itself. Bite-checked by re-pointing the
    constant at `BROKKR_MARKER_PIPE`: fails naming both strings. Its second
    assertion, that `configured_fifo_path` really reads that variable, is
    VACUOUS while the variable is unset - the literal-vs-constant half is the
    one that always bites, and it is enough to catch the shape that was open.
  - THE PREMISE ASSERTION IS A SKIP NOW, reversing round 3's ruling on cold
    review. `emission_without_a_fifo_is_inert` used to `assert!` that
    `BROKKR_MARKER_FIFO` was unset - but that is a variable THIS WORKSPACE'S
    OWN BENCHMARK TOOLING SETS, so a `brokkr mogwai` lane turned a correct
    process into a RED SUITE indistinguishable from a regression. The
    guarantee is not lost, because the test's environment-proof half - a
    locally built `Channel` with `resolve_fifo(None)` - establishes inertness
    whatever the environment says, and it now runs FIRST and unconditionally.
    The process-channel half `eprintln!`s and returns when the variable is set.
    Bite-checked in both directions as text edits: forcing
    `configured_fifo_path` to return a path prints the skip line and passes,
    and making `write_line_to` return `true` with no FIFO fails on the
    unconditional `!write_line_to(&inert, ...)`. GENERAL FORM: a precondition
    on an AMBIENT variable may refuse only when the property cannot be
    established any other way; where a second, environment-proof half already
    establishes it, the precondition is a skip.
  - `channel()` OPENS THE FIFO BEFORE IT STAMPS THE EPOCH. The pure-function
    refactor had inverted this into `Channel { epoch: Instant::now(), fifo:
    resolve_fifo(...) }`, which is field order and therefore evaluation order.
    Microseconds and no consumer can see it, but the origin is defined as the
    instant the channel exists, so it is taken where that becomes true.
- A6'S BOUND IS TWO BOUNDS, AND NEITHER CAN ROT. A LOCAL origin (`stamp_from(
  Instant::now()) < 1_000_000`) is exact - the two statements are adjacent, so a
  second is enormous - and it is the one that bites: substituting wall-clock
  micros since 1970 fails THERE. The process origin gets one day of microseconds
  instead, because `CHANNEL`'s epoch belongs to whichever test called `init`
  first and the honest ceiling is the SWEEP's runtime, not this test's. It is
  four orders of magnitude under the ~1.8e15 a UNIX-epoch stamp reports, so it
  discriminates the defect without pinning a number anyone has to re-measure.
  The monotonicity assertion is kept: it costs nothing and it is what the
  protocol says.
- A4 WAS A TAUTOLOGY OVER AN EMPTY RECORD, WHICH IS WORSE THAN THE REPORT SAID,
  and the comparison was the only implementation of NOTHING - `new` and
  `new_with_count_windows` are both exercised in production
  (`measure.rs::run_final_walk` and `count_curve.rs`'s month runs on
  `CURVE_WINDOWS`), and no other test drove either. Its `GeneratedAcc::new(1, 1,
  2, ...)` window is two NANOSECONDS wide, so both sides finished an empty
  artifact and even a genuine seam defect had nothing to move. The rewrite drives
  a crafted July-7 MNQ session through the accumulator three times - default,
  explicit `COUNT_WINDOWS_S`, and `CURVE_WINDOWS` - and asserts the first two
  agree and the third DIFFERS, which is what makes the agreement falsifiable.
  Bite-checked by re-pointing `new` at `&[1, 5, 15, 60, 300]`: fails naming the
  window keys.
  - AND `default == frozen` IS NOT A DEAD ASSERTION, which the first write-up
    here implied by naming only the window-keys failure. Cold review corrected
    it: re-pointing `new` moves the `default` walk ALONE, because `frozen`
    passes `COUNT_WINDOWS_S` explicitly - so that comparison goes red on the
    same edit. It is close to `f(x) == f(x)` in the sense that `new` delegates
    to `new_with_count_windows`, not in the sense that nothing can falsify it.
  - THREE FIXTURE FACTS THE NEXT `GeneratedAcc` TEST WILL OTHERWISE REDISCOVER.
    The measured window must span a WHOLE session (`close_session` emits
    complete sessions only, so a short window yields an empty `per_session`), and
    every parent needs a `VolTrace` (block 5 selects a forensic minute and
    REFUSES one with no traced parent). Both cost a round-trip here.
    The third is a MECHANISM the fixture comment stated wrongly for one round,
    with the right conclusion: the walk needs real TRADES, but not because
    prints are what produce window keys. Block 2 builds its cells from
    `window_schedule` over the session segment, INDEPENDENT of prints, so keys
    appear as soon as any session closes. What needs trades is the session
    existing at all - `push_trade` is the only thing that rotates one in, so a
    walk of quotes alone finishes with an empty `per_session` and a record no
    window list can move.
  - AND THE FAILURE MESSAGE IS DELIBERATELY NOT A BYTE DIFF. `assert_eq!` over
    the two records dumps 20 KB of decimal byte values. The test reports the
    window KEYS the artifact ended up with instead, and uses `assert!` with a
    written message for the byte comparisons.
- A5'S FIXTURE NEEDED THREE THINGS, not just non-zero residuals: at least TWO
  complete dates (one date cannot show a draw taken in the wrong order), an
  INCOMPLETE one (so the return count is something other than 0 and the
  untouched-cells claim exists), and a real shuffle rather than a reverse - a
  reverse keeps each date's cells contiguous, so a draw chained in vector order
  would still see them together. Bite-checked by mixing the cell's POSITION into
  the draw's seed tuple: fails on the order-independence assertion by name, and
  the OLD all-zeros single-date fixture passes that same defect.
  - THE PER-DATE DRAW IS RECOVERED BY SUBTRACTION AND THEREFORE NEEDS A
    TOLERANCE. `excess` adds one `g` to every cell of a date, but the residuals
    span 0 to 122 here, so `residual + g - residual` differs in the last ulp
    across cells. The cells' agreement is `< 1e-12`; the two dates' separation is
    `> 1e-6`. The cross-ORDER comparison stays exact, because both sides add the
    same `g` to the same operand.
- A8 WAS THE PUREST DEGENERATE FIXTURE IN THE DOCUMENT and the repair needed no
  new machinery: `hour_exposure_weights` is ALREADY nonuniform (hour 20 carries
  45 open minutes against everyone else's 60, hour 21 zero), so the weighting is
  observable the moment the RAW curve stops being flat. Putting the outlier at
  hour 20 is what does it. The expected mean is written out as literals
  (`(22*60*1 + 45*100) / (22*60 + 45)`) rather than recomputed from the weight
  table, which would pin the normalization against itself. The test also asserts
  that the UNWEIGHTED mean of the normalized curve is NOT one, which is the
  sensitivity that makes the whole thing discriminating. Bite-checked by
  replacing the weights with 1.0: fails per hour, naming hour 0.
  - `materialization_is_idempotent` GOT THE SAME TREATMENT, unasked but one
    line: a curve already exact at six decimals is a fixed point of any
    rounding, so the flat version tested nothing. It now asserts that the FIRST
    pass moved something, which is the same "the fixture must exercise the
    property" check.

## The lab/cli document, round 4: the envelope gate's tolerances

Section B is CLOSED - B1 and B3 fixed, B2 REFUSED on measurement. Gate counts
did not move (1229 + 449, 1735 pairs), because the round rewrote assertions
inside four existing tests and added none.

- THE DECISION A FIDELITY FAILURE CHANGES, named before anything was touched
  because CLAUDE.md's process reset requires it, and it is a real one rather
  than "re-run and widen". `simulate_month` is a DELIBERATELY IDEALIZED count
  process - the candidate law without the kernel's arrival-budget traversal,
  child dead time, nanosecond rounding and projection boundaries - and Stage A
  screens cells with it instead of with real generator walks because the
  idealization is orders of magnitude cheaper. The fidelity gate is the ONLY
  thing standing behind that substitution. Its verdict decides whether Stage A's
  admissible region, and therefore Stage B's search space, may be read off the
  surrogate at all; the spec calls a miss "a blocking defect in the envelope
  machinery". A failure is not a number to retune, it is a stop on using the
  screen's output.
- B1 WAS REAL AND THE MEASUREMENT IS BRUTAL. `assert_fidelity`'s whole tolerance
  was `5 * sqrt(idealized_var / 32 + candidate_var / 32)`, so the candidate's
  own dispersion bought its own band. Installed as a text edit, a candidate
  sample spread to 100x the reference variance PASSED ALL FORTY comparisons of
  `wall_mmpp_1s` - and every mean slack IMPROVED, from a healthy 0.003-0.34 of
  the band down to 0.0001-0.05. THE GATE REPORTED MORE AGREEMENT THE MORE BROKEN
  THE CANDIDATE WAS. The catch set for the dispersion-inflation class was
  EMPTY at every factor tried; it is not a loose bound, it is a bound pointing
  the wrong way.
- THE FIX IS AN ABSOLUTE ARM PLUS A SELF-PROOF, not a smaller constant.
  `fidelity_verdict` is now a pure predicate carrying both arms, and
  `FIDELITY_DISPERSION_BAND = 6.5` requires the two variances to agree within a
  two-sided ratio. Post-fix catch set: the first bite lands at a 2x spread (4x
  variance) on the hours whose healthy ratio already sat near 1.8, and any
  inflation past 6.5x of the reference is caught everywhere. The licence a
  still-passing candidate can buy is bounded at 1.94x of the honest standard
  error instead of unbounded.
  - 6.5 IS THE FLOOR n = 32 PERMITS, and the next reader wanting it tighter
    should know the arithmetic rather than re-derive it. A FULL GATE RUN MAKES
    400 COMPARISONS, and that number is settled BY COUNTING rather than by
    recollection: `A3_GATED_HOURS` is 20 entries, each hour is compared on two
    statistics, and there are ten macro-generated parts, so 40 per part and 400
    in total. An earlier draft of the constant's own doc comment said 240 over
    "six of the ten gate parts, both grid steps", which is both wrong and
    internally impossible - six parts cannot span both steps of five families -
    while the arithmetic underneath it was stated against 400. Over those 400
    the raw ratio spans 0.3445 to 2.9489, which is F(31, 31) TO THE QUANTILE:
    the two implementations have genuinely equal variances and everything seen
    is sampling noise. F(31, 31) puts P(F > 6.5) at 6.0e-7, two-sided 1.2e-6,
    so 6.5 costs about one false red per TWO thousand runs at 400 comparisons.
    A band of 2 would be red most runs. MORE SENSITIVITY HERE COSTS REPLICATE
    MONTHS, which is wall time, not a smaller literal.
  - AND THE SENSITIVITY IS PROVEN ON EVERY RUN AT ZERO WALK COST, which is the
    report's own recommendation (copy `arch_coefficients_match_the_shipped
    _recursion`). Both probes are arithmetic over the 32 values already
    collected: the sample displaced six combined standard errors must be
    rejected by the mean arm, and the sample spread to twice the band edge must
    be rejected by the dispersion arm. Two details are load-bearing and were
    both wrong in the first cut. The shift is taken AWAY from the idealized mean
    - a blind `+` moves a candidate that was already sitting low back TOWARDS
    agreement, and the probe then fails for being satisfied. And the inflation
    is sized RELATIVE TO THE IDEALIZED VARIANCE (`sqrt(2 * band * iv / cv)`),
    not by a fixed factor: a fixed 10x applied to a sample already at the low
    end of the F band lands inside the band and the probe fires spuriously.
- DEGENERATE COMPARISONS EXIST AND THE BAND MUST NOT DIVIDE BY THEM. This was
  found by the diagnostics, not predicted: `hourly_zero_fraction` at the
  high-rate hours is often CONSTANT across all 32 months. Measured, in a
  healthy run: `shot_noise_1s` reports BOTH variances exactly zero at hours 13
  and 19, and ONE SIDE zero at hours 17 (0 against 2.220e-11) and 18
  (1.279e-11 against 0); `event_markov_1s` reports 0 against 3.235e-11 at hour
  13; `self_exciting_1s` reports 2.988e-10 against 1.029e-10 at hour 13.
- AND THE FIRST CUT OF THE FIX LEFT THE DEFECT LIVE IN EXACTLY THOSE ROWS -
  the twenty-third instance of this arc's signature defect, in its most
  self-referential form: THE GUARD ADDED TO CLOSE A SELF-WIDENING TOLERANCE
  ITSELF WAIVED THE ARM WHERE THE DEFECT LIVES, AND THE SELF-PROOF PROBE WAS
  BLIND THERE FOR THE SAME REASON. The guard read `idealized_variance > 0.0 &&
  candidate_variance > 0.0`, justified for the BOTH-zero case - with a zero
  combined standard error the mean arm already demands exact equality of two
  constants, which is stricter than any band - but that reasoning does not hold
  when only ONE side is zero. With the idealized sample constant the mean band
  is `5 * sqrt(candidate_var / 32)`, bought entirely by the candidate's own
  dispersion: a candidate alternating 10.5 and -9.5 about a constant reference
  of 0.5 has variance 103 and a band of 8.9, its mean difference is zero, and
  it PASSED. The mirror case is waved through too, a candidate frozen at one
  value against a reference that genuinely moves being the loudest possible
  disagreement. And the inflation probe carried the SAME guard, so the one
  configuration where the arm was absent was the one where its bite could never
  be demonstrated; the row printed `var_ratio=inf` and the test stayed green.
- THE REPAIR IS A FLOOR, NOT A BRANCH. Both variances are floored at
  `FIDELITY_VARIANCE_FLOOR = 1e-9` before the ratio is taken, so EVERY
  comparison reaches the arm and no case is exempt. 1e-9 is a standard
  deviation of 3.2e-5, constancy at the resolution of either order-one
  statistic; the degenerate rows above all sit at or below 5.6e-10 while the
  smallest genuinely dispersed pair seen sits at 2.0e-9. Two constants read
  exactly 1.0000 and pass; a constant against real dispersion reads far outside
  the band in whichever direction and fails. FLOORING CANNOT MANUFACTURE A RED:
  it raises whichever side is below it, which moves any ratio monotonically
  towards 1, so no comparison that passed the raw ratio can fail the floored
  one - which is why the whole ten-part suite did not need re-running to prove
  the change safe, only the two parts carrying degenerate rows.
- AND THE DISPERSION PROBE NOW RUNS ON EVERY ROW. It could not simply drop its
  guard, because scaling a constant sample by a spread factor leaves it
  constant; it injects the dispersion ADDITIVELY instead, alternating plus and
  minus a half-width about the candidate's own mean over the even 32 months, so
  the mean is preserved EXACTLY, the mean arm's band only widens, and the
  rejection is necessarily the dispersion arm's. Both probes now assert WHICH
  ARM fired by matching the verdict's prefix, so a probe satisfied by the wrong
  arm is a failure rather than a pass.
- A CHEAP UNIGNORED TEST NOW PINS THE PREDICATE,
  `the_fidelity_predicate_bites_where_one_sample_never_moves`. The ten parts
  that exercise `fidelity_verdict` for real are `#[ignore]`d cost gates, so
  without it the predicate shipped unexecuted by `brokkr check` - and the hole
  above was in the predicate, not in the walks. Bite-checked by restoring the
  `both variances strictly positive` guard as a text edit: the test fails at
  "unbounded dispersion around a constant reference must be refused", which is
  the 103-variance-against-a-constant case, and passes with the floor restored.
- B2 IS A REFUSAL, and the measurement is in `notes/bugs-tests-lab-cli.md`
  rather than repeated here. The one line worth carrying: THE `0.5 * closed` CAP
  NEVER BINDS - the plug-in arm is binding in all 36 comparisons at 0.15x-0.59x
  of the cap - so the report's suggested `0.1 * closed` would have REPLACED a
  healthy adaptive tolerance with a fixed band tighter than the Monte Carlo
  error in most cells. Both gates now print their arms per comparison
  (`conformance_arms ...` and `fidelity_arms ...`), which is what keeps either
  claim from decaying into prose nothing re-checks.
- B3: THE WALL ASSERTION IS GONE, the counter stays. `assert!(elapsed <=
  CONFORMANCE_BUDGET_S)` was a host-property claim inside a correctness gate -
  the `tape_lateness_under_acceleration` shape - and the 12b spec itself asks
  for a REPORT here ("checked by running, not asserted"). It now prints
  `conformance_wall_s=... budget_s=900`. NO NEW BENCHMARK TARGET WAS
  REGISTERED, deliberately: `envelope_evaluation` already measures the
  per-replicate-month unit price the whole budget is derived from, so the cost
  claim is re-runnable without a second instrument for the same quantity.
  - WHAT ACTUALLY BINDS THIS GATE IS NOT THE 900 s BUDGET. Measured 167.6 s and
    168.2 s, so the assertion had 5.3x of headroom and was catching nothing.
    The live ceiling is the RUNNER's - `brokkr test --timeout` refuses anything
    over 280 s - and no assertion inside the test can see it. If this gate ever
    grows, that is the number it collides with.
- COST: ZERO MEASURABLE WALL. Both arms and both probes are arithmetic over 32
  f64s; the walks are untouched. Serial release re-runs of the same parts before
  and after: `wall_mmpp_1s` 54.22 s -> 54.22 s, `log_ou_cox_1s` 58.47 s ->
  58.47 s, `self_exciting_1s` 55.72 s -> 55.97 s, `event_markov_1s` 138.99 s ->
  138.24 s, conformance 167.60 s -> 168.15 s. The other five parts, measured for
  the first time here: `event_markov_250ms` 142.5 s, `shot_noise_250ms` 106.2 s,
  `shot_noise_1s` 78.0 s, `log_ou_cox_250ms` 77.5 s, `self_exciting_250ms`
  75.7 s, `wall_mmpp_250ms` 70.2 s. THE WHOLE ELEVEN-PART ENVELOPE SUITE IS
  ABOUT 16 MINUTES SERIAL, every part inside the 280 s runner ceiling - so the
  `brokkr.toml` skip entry's cost justification for these two patterns is
  CORRECT, unlike the entries the tape round found overstated.
- THE FIDELITY GATE HAS NO ONE-COMMAND FORM, and `brokkr.toml` documented one
  that cannot run - the instruction the next reader would have followed.
  `--timeout` is honored only when the filter matches exactly ONE test and
  `brokkr test` ERRORS BEFORE RUNNING when it matches more, so the frozen
  prefix, which matches all ten parts, fails with a config error rather than
  running anything. Confirmed by running it. The comment now names the ten
  family-and-step suffixes and gives a worked per-part example; the conformance
  gate is a single test, so its own documented command was always fine.
- TWO STALE COST FRAMINGS went with the wall assertion. The
  `#[ignore]` reason said "bounded by 900 seconds" and the `brokkr.toml`
  comment said "spec ceiling 900 s"; nothing bounds either at 900 any more, so
  both now state the measured cost instead.
- DIAGNOSTICS MUST BE FINITE ON THE INTERESTING ROWS, which is where they were
  worst. `var_ratio` printed NaN on the both-zero rows and `inf` on the
  one-sided ones - the exact rows the defect lived in - and `mean_slack`
  printed NaN wherever the band was zero. The ratio is now the FLOORED one, so
  it reads 1.0000 there while the raw variances beside it still identify the
  row, and a zero band prints `mean_slack=exact` rather than NaN. Same shape
  fixed in the conformance gate's `slack_ratio`. A diagnostic presented as what
  keeps a claim from decaying into prose has to be legible on the rows that
  matter.
- THE ZERO-BAND SHIFT PROBE WAS MAGNITUDE-FRAGILE. Its fallback displacement
  was a fixed `1e-12`, which is A NO-OP UNDER FLOAT ADDITION once sample values
  exceed about 1e4 - the ulp at 1e5 is 1.5e-11 - at which point the shifted
  sample equals the original, the verdict is `None`, and A GREEN GATE IS
  REPORTED AS A CORRECTNESS FAILURE. Safe today only because both gated
  statistics are order one, which is a property of today's statistic list and
  not of the probe. It is now `8 * f64::EPSILON` times the sample's own level,
  which cannot be swallowed at any magnitude.
- LATERAL, UNFIXED, AND NOT THIS ROUND'S: `brokkr test`'s PASS/FAIL footer wall
  is not a reading of the assertion phase. A run that panicked on the FIRST of
  40 comparisons still reported 77.97 s, identical to the green run - because
  all 64 month walks happen before the assertion loop, so the footer measures
  the walks and nothing else. Harmless here; do not use that footer to time a
  change to an assertion.

## The lab/cli document, round 5: what carries to `bugs-protocol`

Sections D, E and F are closed and the document has no open findings. The
lab/cli specifics (`unit_test_scratch`, the session-segment collapse) die with
it. What generalises, and the first four are GENERAL RULES the production
documents inherit:

- A SHARED FIXTURE IS NOT THE ONLY CLOSE FOR A TWO-COPY GATE, and reaching for
  it reflexively builds machinery where a deletion was owed. Three shapes, and
  they need different answers. TWO CONVENTIONS FOR ONE QUANTITY - the shape
  `AGENTS.md` describes - takes the fixture. THE SAME ALGORITHM WRITTEN TWICE
  takes a deletion: `mogwai-lab`'s `summary` and `session` each carried the
  session-segment branch structure and its four minute constants, `summary`'s
  is now a field mapping over `session`'s, and the sweep that held them equal
  is gone as `f(x) == f(x)`. TWO PIPELINES PRODUCING A WHOLE RECORD has no
  units-neutral statement to write down at all, so ANCHOR THE INPUTS - which
  is where the drift lives anyway.
- A GATE'S JUSTIFICATION CAN EXPIRE WITHOUT THE GATE NOTICING, and the tell is
  a comment in the future tense. `session_segment_at_agrees_with_mogwai_lab`
  called itself a bridge "until phase 2 rewires this crate onto the lab". The
  rewire had landed, in the opposite direction to the one the comment
  anticipated - `gen.rs` was absorbed into `mogwai_lab::summary`, so the test
  had silently stopped comparing two crates and become a two-copy gate inside
  one. NOTHING DETECTS THIS: the test stays green either way, and its own
  comment is what makes it look accounted for. When a durable comment names a
  future event, check whether the event happened.
- WHEN A REPORT'S TABLE AND ITS PROSE DISAGREE, THE TABLE IS USUALLY RIGHT.
  Two of the four section-D candidates were miscategorised in the prose while
  the table row beside them described the correct shape - a test recomputing
  from a DIFFERENT FIELD of a committed artifact graded as "two production
  copies", and an optimization-preserves-output gate graded as a convention
  drift. Re-derive the shape from the code before accepting a grade.
- THE RECOMMENDED FIX CAN BE THE SECOND INSTANCE OF THE DEFECT. The report
  told this round to make `cadence.rs` follow `storage.rs`'s
  `CARGO_TARGET_TMPDIR` shape. `CARGO_TARGET_TMPDIR` IS SET FOR
  INTEGRATION-TEST TARGETS ONLY, so in a LIB UNIT test that lookup always
  fails and the `env::temp_dir()` fallback beside it is the branch that always
  runs - `storage.rs` was writing outside the tree too, under a comment saying
  it was not. `CARGO_MANIFEST_DIR` is the reading that works in both, which
  `characterize/tests.rs` already knew. Verify a recommended exemplar before
  copying it.
- `mogwai-lab`'s LIB UNIT TESTS NOW HAVE ONE SCRATCH SHAPE and none of them may
  hand-roll another: `storage::unit_test_scratch(name)` returns the existing
  `ScratchDir` guard rooted at `target/lab-unit-scratch/<name>`, inside the
  tree, removed on drop INCLUDING the panic path, keyed on pid plus a
  nanosecond stamp. It replaced FIVE hand-rolled scratch call sites - four in
  `storage.rs`, one in `cadence.rs` - and five `remove_dir_all` calls that ran
  only on the success path. What it does NOT remove is the empty
  `target/lab-unit-scratch/<name>/scratch/` spine above the unique leaf; that
  is a fixed set of empty directories, not an accumulation, and the doc
  comment states it because "removed on drop" reads as more than it is.
  `mogwai-cli`'s test binaries keep `common::scratch`, which
  is `CARGO_TARGET_TMPDIR`-based and correct there because they are
  integration targets - the two are deliberately separate and the reason is the
  bullet above.
- VERIFYING PRAISE MEANS PERTURBING, and this round's five held - the first
  clean praise audit of the arc after four documents found false or vacuous
  claims in praised tests. Two of the five still gained a line, and both gaps
  are of standing families rather than of those tests: three `assert!(x
  .is_err())` calls that could not say WHICH check fired (measurement showed
  three different refusals, so the ambiguity was real), and an
  agreement-only assertion that two zeros or two NaNs satisfy. A GREEN,
  WELL-WRITTEN, GENUINELY GOOD TEST STILL OWES ITS ERROR ASSERTION A NAME AND
  ITS AGREEMENT ASSERTION A VALUE.
- A PREMISE INSIDE AN `#[ignore]`d GATE IS AN UNRUN PREMISE. The exposure pin
  costs ten minutes, and the only cross-check between its two sides' INPUTS
  lived inside it - so a check lane never ran the cheap half, and a window
  mismatch would have surfaced ten minutes later as a diff of two 20 KB
  accumulator records blaming the exposure contract. Split out as
  `the_committed_binding_carries_the_window_run_final_walk_measures`, which is
  the round-4 lesson (`the_fidelity_predicate_bites_where_one_sample_never
  _moves`) applied a second time: WHEN A HEAVY GATE HAS A CHEAP PREMISE, THE
  PREMISE IS ITS OWN UNIGNORED TEST. Bite-checked by moving `SUMMARY_WARMUP`
  to `"4d"`.
- AND THAT PREMISE TEST SHIPPED THE VERY DEFECT ITS SECTION WAS HUNTING, caught
  by the round's cold review. CARRY THE RULE, because it is about CONSTANTS
  rather than about implementations and is therefore easier to miss: TWO
  CONSTANTS ENCODING ONE QUANTITY ARE A TWO-COPY GATE WITH NO GATE.
  `subcontract` carries the final window's length twice - `FINAL_END_NS -
  FINAL_START_NS` in nanoseconds and `FINAL_LENGTH` as a seconds string - read
  by different consumers, and nothing compared them. `run_final_walk` parses
  the STRING; the new premise test asserted against the DIFFERENCE; so the one
  edit the test's own docstring named would have moved the measured window with
  the test green. Two halves close it, and the second is owed regardless of the
  first: make the test read the encoding its subject reads, AND assert the
  identity between the encodings. Bite-checked together at `"2674801s"`.
  - A FROZEN-SNAPSHOT HASH OVER A CONSTANT SET IS NOT THAT IDENTITY, and
    believing otherwise is how the gap reads as covered.
    `hash_matches_the_python_reference` does redden on a bare edit, since
    `FINAL_LENGTH` is in the hashed tree - but the SANCTIONED way to move a
    sub-contract constant re-blesses the hash in the same change, and the
    re-blessed hash says nothing about whether the two encodings still agree. A
    snapshot hash catches an UNINTENDED edit; only the identity catches an
    INTENDED edit to one encoding.
- THE GATE COUNT MOVED BY EXACTLY ONE, and the arithmetic is worth stating
  because r4's was a flat ledger and this one is not: one `mogwai-cli` test
  deleted (the tautological sweep) and one added (the premise) CANCEL, and the
  `mogwai-lab` identity gate is the net +1.
- LATERAL, AND FIXED IN PASSING: `AGENTS.md` said the shared-fixture
  convention had "two instances so far". There are three -
  `segment_library_conformance.json` is loaded by `mogwai-data`'s `segment.rs`
  and `mogwai-lab`'s `segments.rs` - and it is read by PATH rather than
  `include_str!`d, which the sentence also asserted universally. A convention
  that miscounts its own instances is the durable-prose-asserting-a-live-fact
  shape aimed at itself.

## The lab/cli document, close pass: what the five commits left

ONE FINDING, and it is the arc's signature defect for the twenty-fifth time -
A THING THAT READS AS GATED AND IS NOT - in the unreviewed half of the round-2
commit, which is where the previous four close passes found theirs.

- THE RUNTIME BRACE COVERED TWO WRITERS OF SIX. Round 2's cold review found the
  tree-state seam spoofable and the fix-and-commit half closed it with a cargo
  feature plus a runtime refusal - inlined in `arrival_control` and
  `arrival_screen`, the two writers that round's tests exercised, with the
  seam's own module comment then stating the set as "the two artifact writers
  in `mogwai-cli`" as though that were the whole roster. It is not:
  `measure`, `fit`, `minute_range_envelope` and `arrival_envelope_diagnostic`
  all write a tree-attested binding, all four had the braces off, and three of
  them have dirty-tree tests that install a double in the same commit. Under
  the `--all-features` build the runtime guard exists FOR, one install would
  have forged their bindings unchallenged.
  - CLOSED BY A SHARED CALL PLUS A ROSTER TEST, not by four more inline copies.
    `mogwai_cli::attestation::refuse_scripted_tree_attestation` is the one
    guard now; `tree_writers_all_refuse_a_scripted_attestation` reads this
    crate's own sources, finds every file that SERIALIZES `harness_tree_commit`
    or `clean_tree`, and fails naming any that does not call it. Bite-checked
    by deleting the call from `measure.rs`: the test names `measure.rs`.
  - AND THE ROSTER TEST STATES ITS OWN BLIND SPOT. `fit` binds an artifact
    whose JSON is serialized in `mogwai_lab::fit::driver`, so the roster cannot
    see it; the guard is there by judgment and the docstring says which writer
    the file scan misses and why. A source-scanning roster is worth having and
    is not a proof.
  - THE GENERAL RULE, which is the guard-scope family with the guard in the
    right place and the wrong NUMBER of them: WHEN A FIX INSTALLS A CHECK AT A
    CALL SITE, COUNT THE CALL SITES BEFORE WRITING DOWN HOW MANY THERE ARE. The
    comment claiming "the two" is what made the gap read as covered, exactly as
    a green test does.

LATERAL, OBSERVED AND NOT FIXED, AND IT IS A LEAD FOR `bugs-server` RATHER THAN
FOR A TEST DOCUMENT: `run_complete_is_stamped_on_the_receiving_sockets_clock`
failed ONE of the close pass's two full `brokkr check --gate` runs, on
`.expect("second boat receives completion")`, and passed the other. That is not
the family `watch_a_bounded_run` closes: the helper DISCARDS a run whose socket
saw no content frame or whose drain ended on the deadline, so reaching the
`expect` at all means the second socket was a live session, drained to a clean
end, and never received the announcement. The remaining explanations are about
the VENUE - a boat whose completion frame is not written to every attached
socket before teardown closes them - not about the test's premises, which is
why it belongs to a production round. Reproduce it before believing it: one in
two here, and `bugs-tests-lifecycle` is closed, so nothing else is watching.
NOW ALSO FILED IN `notes/bugs-server.md` AS ITS SECTION 9, which is where the
round that works it will actually look; this paragraph dies with this section.

Everything else held. The AGENTS.md edits were verified against the tree (the
shared-fixture count really is three and `segment_library_conformance.json`
really is path-loaded by both sides; `subcontract`'s two encodings really are
read by different consumers); `gen.rs` really does reach the session math
through `mogwai_lab::summary`, so the deleted sweep really was `f(x) == f(x)`;
the architecture note's protocol-9 tape-hash claim is enforced by a now-unignored
test rather than dated; the sampler `Drop`, the `cache_root` purity split, the
`resolve_fifo` split and the two named error messages are behaviour-preserving
or intentionally not, and each says which.

## The protocol document, round 1: the decimal wire and the duration grammars

THE ARC'S FIRST PRODUCTION-CODE ROUND, and the first fix that changes what the
venue does ON THE WIRE. Both findings reproduced, one of them differently and
worse than reported.

- EVERY `Decimal` IN `mogwai-protocol::messages` IS NOW STRING-ONLY ON THE WIRE,
  in both directions, and a JSON NUMBER IS REFUSED. The mechanism was measured
  before it was fixed, and the hunter's account was incomplete in two ways:
  - The rounding is real. `{"price": 12345678901234567890.123}` decoded to
    `12345678901234567000`; `0.1234567890123456789` to `0.12345678901234568`.
    Small values are exact by luck - `0.1` and `100.005` round-trip - so a
    casual probe reports "it is fine" and the loss only appears at width.
  - THE TWO GRAMMARS ARE NOT NESTED, which nobody had noticed: `1e-30` was
    ACCEPTED as a number and decoded to `Decimal::ZERO` at scale 28, while the
    same text as a STRING was refused outright. A quantity that becomes zero
    depending on its spelling is a different and worse defect than a rounded
    one, and it is the fact that settled the "tolerate or refuse" question.
  - WHO ACTUALLY SENDS DECIMALS, established before choosing: nothing in the
    tree sends a number. `mogwai-adapter` builds a real
    `mogwai_protocol::SubmitOrder` and serializes it (strings); every hand-rolled
    frame in `mogwai-cli`'s serving suite and the adapter's stub uses strings;
    `scripts/smoke.py` sends `"0.001"`. broadarrow consumes these same Rust
    types. So the breaking change breaks no known peer.
  - THE LINE IS MONEY, NOT "Decimal everywhere" AND NOT "the module called
    `messages`", and the round's cold review is what forced that phrasing. The
    fix pass wrote the scope as "the execution and market-data wire" and made
    the call on a field list drawn from ONE FILE, which missed two live decode
    paths carrying money and mis-described a third of what it did annotate
    (`AccountState`, `Balance`, `Position` and `PostedMargin` are account
    frames, neither order nor market data). SCOPE STATED BY MODULE IS THE
    DEFECT; the axis is what the number MEANS.
    - STRING-ONLY, and this list is the whole of it: `messages`'s 35 serde
      `Decimal` fields; `risk::RiskState` and its nested `Breach`, published on
      `GET /account` as `AccountSnapshot.risk`; and
      `mogwai-server`'s `OpenAccountRequest.balances`, the `POST /accounts`
      opening balances. The last two were the review's finds. Both are
      OUTPUT-ONLY or string-spelled by every in-tree caller already, so
      annotating them broke nothing and cost two tests -
      `a_published_risk_state_refuses_a_numeric_decimal` and
      `an_opening_balance_must_be_spelled_as_a_string`.
      `RiskState` is the instructive one: nothing in this workspace decodes it,
      which is precisely why the tolerance would never have been observed here
      and would have shipped to a consumer's decoder.
    - TOLERANT, deliberately: `control::Divergence` (POST
      `/control/divergence`), `risk::RiskPolicy` and `risk::AccountPolicy`, and
      `instruments::InstrumentSpec`. The last three are also TOML config, where
      `multiplier = 2` is the natural spelling, and all of them are
      operator-supplied fractions and thresholds rather than booked quantities.
      Refusing numbers there would be a usability regression with no rounding
      hazard behind it. The `an_opening_balance...` test asserts BOTH halves in
      one place, so the exception cannot decay into an oversight.
    - `mogwai-protocol::decimal` IS PUBLIC NOW, for `str_map` - the `POST
      /accounts` balances are a `HashMap<String, Decimal>`, which no
      `rust_decimal` helper covers. A later map-shaped wire decimal uses it
      rather than a second newtype.
  - THE FIX OPENED A HOLE ONE LAYER UP, AND THE GATE CAUGHT IT - the arc's
    signature failure, on schedule. `rust_decimal::serde::str_option` REFUSES an
    explicit `null`, and `"price":null` is what the venue and the adapter emit
    for every priceless order. Two socket tests went red
    (`adapter_submits_a_stop_market_and_sees_triggered_then_filled`,
    `a_trigger_amend_on_a_triggered_stop_limit_keeps_it_triggered`) with the
    unattributable symptom "no execution event reached the sink", because the
    stub's decode is a silent `if let Ok`. SECOND HOLE IN THE SAME FIX: a
    `with = ...` field loses serde's implicit Option-is-optional handling, so an
    ABSENT `price` - what every `Market` fixture in the serving suite sends -
    became a missing-field error. The repair is a local
    `crate::decimal::str_option` (null and absent are `None`, `visit_some`
    delegates to the dependency's `str` deserializer so the one rule lives in
    one place) plus `default` on all five optional decimal fields that lacked
    it. Pinned by `an_absent_or_null_optional_wire_decimal_is_still_none`,
    bite-checked BOTH ways.
  - NOTHING DETECTS A NEW `Decimal` WIRE FIELD THAT FORGETS THE ANNOTATION.
    `every_wire_decimal_refuses_a_numeric_spelling` is a hand-maintained table,
    exhaustive over `messages`'s 35 serde `Decimal` fields across TEN struct
    shapes - `SubmitOrder` 5, `ModifyOrder` 3, `OrderUpdated` 4, `OrderFilled`
    4, `OrderStatusInfo` 4, `Balance` 3, `Position` 4, `PostedMargin` 2,
    `TradeTick` 2, `QuoteTick` 4, with the non-serde `Hit.px` excluded - and a
    new field owes a row. THE COUNT IS LOAD-BEARING, because it IS the argument
    that the hand-maintained table is complete, and the fix pass first wrote it
    as "32 across nine". A count nobody recomputed is the same defect family as
    the prose the whole document is about; recount it whenever a row moves.
    A source-text scanner in the `gate_skip_list.rs` style was
    considered and refused: the `Decimal` occurrences in that file include
    function parameters and a non-serde `Hit` struct, so a parser that fails
    open would be worse than the habit. Same shape as the standing open item on
    durable prose asserting a live fact.
  - Its hot-path block runs FIRST in the test, deliberately.
    `ServerMessage::from_json_str` is a second decode path over the same fields;
    a block placed after the tables could never fire first and so could never be
    bite-checked. It covers EVERY decimal of both market-data frames, not the
    two the first cut sampled: the fields do share one `Deserialize`, but a
    block whose stated claim is "the same fields" has to check all of them, and
    both frames are now consts the table and the block share.
  - THE `SubmitOrder` FIXTURE NEEDS TWO ROWS, and the first cut's single row was
    a frame the venue REFUSES: it spelled all five decimals on a
    `TrailingStopLimit`, which rejects `price` because it derives the limit from
    `limit_offset`. Decode-only tests never call the validator, so it passed
    while pinning an illegal shape for the next reader auditing the table. The
    loop now runs `validate_submit_order` over every decoded `SubmitOrder`, so a
    fixture cannot be illegal silently again. GENERAL FORM: a decode fixture in
    a table read as an exhaustiveness argument owes a legality check, because
    nothing else in a decode test performs one.
- `serve`'s DURATION GRAMMAR IS NOW PINNED ACROSS THE REAL BOUNDARY. G held -
  `serve` takes `humantime::Duration` and `gen`'s in-house `parse_duration` is
  never on that path - but nothing said so, and the two grammars disagree on
  exactly the three things the launcher emits (`0s`, `ms`, `ns`).
  - MAKING `serve_argv` PUBLIC MADE ITS `std::process::id()` A DEFECT, and the
    cold review caught it. The pid it stamps into `--launcher-pid` is the
    liveness inference the venue makes about its owner, and the justification
    for taking it from this process ("`launch` spawns the child from a thread it
    owns") holds only for the IN-CRATE call site. A consumer rendering argv here
    and spawning elsewhere - a systemd unit, a shell, a supervising helper -
    would get a venue watching a stranger: it exits at once if that pid is
    already gone, or never notices its real owner dying. IT IS A PARAMETER NOW,
    `serve_argv(&spec, launcher_pid)`, and the unit tests pass a pid that is
    deliberately not this process's so a regression back to `process::id()`
    fails. THE SHAPE TO CARRY: widening a function's visibility can turn a
    correct ambient assumption into a defect without changing a line of its
    body. Ask what the function reads from its environment before making it
    `pub`.
  - THE TEST CROSSES THE BOUNDARY RATHER THAN ROUND-TRIPPING ONE CRATE'S OWN
    PAIR. `mogwai-protocol`'s `serve_argv` is now `pub` for this, and
    `mogwai-cli`'s `serve_argv_parses_in_the_venues_own_grammar` lives in the BIN
    crate's unit tests - the only place `ServeArgs` is visible - rendering with
    the launcher's function and parsing with the `Cli` that `main` runs. Its
    cases are the ones `durations_render_in_the_coarsest_exact_unit` enumerates,
    so the two cannot drift onto different values.
  - Bite-checked by installing the regression it names, as a text edit: a clap
    `value_parser` routing `--duration` through `gen::parse_duration` fails it at
    `invalid value '0s' ... must be at least 1`. A companion test,
    `the_in_house_gen_grammar_cannot_read_what_the_launcher_renders`, asserts the
    refusal directly so the first test cannot be read as "any parser would do";
    `gen::parse_duration` is `pub(crate)` for it and says why in place.
- FINDING M IS ANNOTATED IN THE REPORT RATHER THAN DELETED, because the smell
  reads as live to anyone who has not seen `bugs-tests-engine-protocol` r1's
  measurement. G and J are deleted outright.

## The protocol document, round 2: the post-only rule and the market-to-limit precedence

BOTH FINDINGS WERE ABOUT PROSE AND ONE OF THEM WAS HIDING A LIVE WIRE/ENGINE
DISAGREEMENT. The brief's instruction to establish the engine's actual behaviour
before deciding is what found it; neither the hunter nor the report knew it was
there.

- THE WIRE ADMITTED A POST-ONLY `TrailingStopLimit` AND THE ENGINE THEN REJECTED
  IT. Measured over all nine order types: the wire gate spelled the rule as
  `Limit || rests_after_trigger()` (four types) while `mogwai-engine`'s
  `validate_submit` carried a hand-rolled `Limit | StopLimit | LimitIfTouched`
  (three), so a client sending the fourth was told its order had passed the front
  door and then that it had not. `on_trigger` HAS ALWAYS enforced post-only on
  that type - the arm at `StopLimit | LimitIfTouched | TrailingStopLimit` does
  it - so the hand-rolled list was the only thing refusing it, and the type that
  came apart is the one added LAST. That is what a hand-rolled list does to the
  next variant anyone adds.
  - CLOSED WITH ONE PREDICATE, `OrderType::may_be_post_only`, read by both
    gates, plus ONE MESSAGE, `mogwai_protocol::POST_ONLY_REFUSAL`, returned by
    both. The engine now accepts what the wire admits.
  - AND THE FIRST CUT UNIFIED THE PREDICATE WHILE LEAVING THE GATE ORDER SPLIT,
    which reinstated the round's own defect one layer up. Cold review caught it:
    the wire checked post-only BEFORE the conditional-IOC arm and the engine
    checked it AFTER, so a post-only `StopMarket` marked `Ioc` - illegal twice
    over - earned `POST_ONLY_REFUSAL` from one gate and the conditional-IOC
    message from the other. Every case the new test built carried the default
    `Gtc`, so it could not see it. CARRY THE SHAPE: deduplicating the PREDICATE
    two gates share does nothing about the ORDER they reach it in, and an input
    that breaks two rules at once is the only input that can tell. The engine's
    check moved ahead of the conditional-IOC arm, both sites say the order is
    part of the contract, and the test carries a both-rules case.
  - AND THE MESSAGE NAMES THE LEGAL SET rather than stating a rule. The old
    text - "post_only is legal only on orders that rest as a limit" - is FALSE
    for `MarketToLimit`, which does rest its remainder as a limit and is refused
    anyway. The refusal is right (its FIRST act is to take what the touch
    offers, which is the thing post-only forbids) and the reason was wrong,
    which is the shape that gets "corrected" wrongly later.
  - THE EQUALITY ASSERTION AGAINST THE SHARED CONSTANT IS VACUOUS BY
    CONSTRUCTION and the test says so in place. Once one string feeds production
    and test, `assert_eq!(err, POST_ONLY_REFUSAL)` cannot fail. What is live is
    a check on the TEXT: it must name every legal type and no illegal one. Carry
    the shape - deduplicating a message into a constant DELETES the assertion
    that the message says anything, and something has to replace it.
    THE CHECK PARSES THE LEGAL LIST RATHER THAN SEARCHING THE MESSAGE, which
    is the second thing cold review corrected. The first cut used anchored
    `contains` needles (`" StopLimit,"` for the `TrailingStopLimit` substring
    hazard) and three of the nine were still unanchored on the left - so a
    STRICTLY BETTER message that also spelled out the illegal types ("... and
    TrailingStopLimit orders; not on Market, StopMarket, ...") would have FAILED
    the test, reporting illegal types as named legal. A GUARD THAT REFUSES AN
    IMPROVEMENT TO THE ARTIFACT IT GUARDS GETS DELETED RATHER THAN UNDERSTOOD,
    and that is a general rule for any test asserting on a message's text: the
    property wanted is a POSITION in the string, not containment. It now splits
    the segment between "legal only on" and the first " orders" and compares
    exact names, so a suffix is ignored and the substring hazard is gone
    outright rather than worked around. THE IMPROVEMENT WAS THEN ADOPTED: the
    shipped message names the refused types too, which is worth having and which
    the guard now tolerates by construction. Bite-checked both ways - the longer
    message passes, dropping `TrailingStopLimit` from the legal half fails
    naming it.
  - `post_only_is_admitted_by_one_rule_at_the_wire_and_in_the_engine` is
    EXHAUSTIVE OVER `OrderType` by a match with no wildcard, so a tenth variant
    fails to compile rather than inheriting a verdict nobody chose. Its legal
    set is written out longhand rather than derived from `may_be_post_only`: a
    test that reuses the production predicate cannot catch the predicate being
    wrong. Bite-checked by restoring the hand-rolled list (fails naming
    `TrailingStopLimit`) and by restoring the old message text (fails naming
    `Limit`).
- FINDING I WAS REFUSED AS A REFUSAL AND CLOSED AS A PRECEDENCE, on the
  measurement. `MarketToLimit` + `Ioc`/`Fok` is admitted at the wire, and it
  should be: THE FILL MODEL HAS NO BOOK TO SWEEP, so a `MarketToLimit` takes its
  whole quantity at its own stated limit price under every time in force and
  leaves no remainder to argue about. The type's doc argues about why the type
  EXISTS, not about what its time in force may be, and refusing the combination
  would have broken something the engine handles.
  - THE PRECEDENCE, now written on the variant, in `docs/oms-types.md` and
    pinned: THE TIME IN FORCE GOVERNS THE REMAINDER. A remainder exists only
    where an armed `PartialFillNext` manufactures one, and there `Fok` rejects
    before acceptance, `Ioc` cancels the remainder, `Gtc`/`Day`/`Gtd` keep it.
  - AND THE MEASUREMENT FOUND A REAL DEFECT THE FINDING HAD NOT SUSPECTED: the
    KEPT remainder rests `Resting::Inert`, offered to no sweep, so it can never
    fill or expire. `orders.rs` gates both the `Resting::Limit` assignment and
    the partial-tranche redraw on `order_type == OrderType::Limit`. The type's
    whole name is unimplemented. FILED IN `notes/todo.md` FOR `bugs-engine`
    rather than fixed here - it is an engine behaviour change with its own
    thinking owed about the band draw - and
    `a_market_to_limit_remainder_is_governed_by_its_time_in_force` pins today's
    truth INERT INCLUDED, saying in place that it must be updated rather than
    deleted when the fix lands.
  - AND THE ROUND BLESSED THE OTHER HALF OF THAT SAME DEFECT IN A MUST-BE-TRUE
    DOCUMENT, which is A NEW SHAPE FOR THIS ARC and the one worth carrying.
    `docs/oms-types.md` gained the sentence "the venue's fill model has no book
    to sweep, so the order takes its whole quantity at its stated limit price
    and leaves no remainder at all" - written as the MODEL, while `notes/todo.md`
    filed the inert remainder as a bug in the same change. Cold review verified
    it against `orders.rs` and it is a defect: the slipped market draw is taken
    only for `OrderType::Market`, every other type fills at `risk_px`, and
    `marketable` is consulted only for `Limit`, so a buy `MarketToLimit` limited
    at 200 against a last print of 100 fills 100% AT 200 - the opposite of
    taking what the touch offers. The two are ONE defect with two symptoms, the
    fill price being why the remainder is unreachable, and they are filed
    together now. THE RULE: when a round explains an absence ("there is no
    remainder to argue about"), ask whether the explanation is the design or a
    bug, BEFORE writing it into `docs/` or `reference/` - those folders certify
    what they describe, and a defect written there stops being findable.
    Describing the gap explicitly, as `docs/oms-types.md` now does, is the fix;
    deleting the sentence would have left the client to discover it.
- NO PEER BREAKS. The refusal string had exactly one in-tree reader, the engine's
  own refusal matrix, updated in the same change; `mogwai-adapter` forwards
  `post_only` for any order type and now gets an end-to-end post-only
  `TrailingStopLimit` that used to die at the engine, which is a widening rather
  than a break; `scripts/smoke.py` sends no post-only order.

## Facts a later round would otherwise re-derive wrong

- LIBTEST SPAWNS A THREAD PER TEST EVEN AT `--test-threads=1`, on any platform
  that supports threads. A round-3 cold review argued the opposite from an OLD
  libtest, whose `run_tests` special-cased `concurrency == 1` and ran each test
  INLINE on the calling thread - which would make every thread-local in a test
  binary process-wide under the invocation `brokkr test` always uses, and would
  have failed the whole socket suite wholesale. MEASURED, not reasoned: with the
  harness's own anchor reset disabled and the wall budget shrunk to six seconds,
  all fourteen tests of a 5.96 s serial run passed and each read an anchor
  140-200 ns old. The named-thread-per-test also shows in every panic line.
  TREAT IT AS TRUE TODAY AND NOT AS A CONTRACT: it is an implementation detail
  that has already changed once, the failure if it changes back is silent and
  wholesale, and nothing in this tree would detect it - which is why the budget
  anchor is reset explicitly rather than trusted to it.
- A BLANKET `#![allow(dead_code)]` ON A SHARED TEST MODULE HIDES DECAY. It is
  there because not every binary uses every helper, so it also silences a field
  whose last reader was just deleted - `Venue::ready_at` survived a round with
  no readers and a doc comment describing a gate that no longer existed. A
  deletion in `common/` carries its own sweep; the compiler will not do it.

- A CONFIGURED `speed = 0.0` DOES NOT MAKE THE CLOCK RACE THE WALL.
  `build_run_clock` and the boatyard both substitute 1.0 for a configured 0.0,
  so the clock IS the wall, at rate 1. What comes apart is DELIVERY: it is
  unpaced, so the tape's `ts_event` runs far ahead of `server_now_ns`, and a
  clock target anchored on a tape stamp is therefore satisfied instantly. The
  round-1 fix pass asserted the opposite mechanism from a correct measurement;
  the measurement stands, the explanation did not. Written down durably in
  `reference/clock.md`.
- `RiskLedger::breach` is LATCHED - `observe` returns `Verdict::Clear`
  immediately once it is set, on the principle that the first breach is the one
  that describes the run. This is why an eventual-consistency poll on `/account`
  does not weaken a test that also asserts `breached`: a transient collapse
  crosses the floor and STICKS, so the poll cannot wait it out.
- AN UNPACED VENUE IS A FIREHOSE, WITH A NUMBER ON IT NOW: a `speed = 0.0`
  socket receives OVER A MILLION FRAMES in 2 s of wall, and a test draining one
  manages about 111,000 a second. That is why so many tests in these files have
  to drain concurrently, why an unread socket is ejected by the bounded fanout
  ring so readily, and why anything the venue writes at the TAIL of a span -
  `RunComplete` is the case that bit - can be unreachable inside a test's wall
  budget. Round 5 measured it while fixing exactly that. Pace the venue when the
  property is not about cadence; drain hard when it is.
- `/trades` PAGES FROM THE OLDEST END. `bounded_trades` walks the window from
  `start` and breaks when the page is full, so a truncated page is the window's
  OLDEST prints - `trades.last()` is then NOT the last print before `end`, and a
  minimum over it is not the window's minimum. Round 2 found a live instance of
  this being read as favourable slippage. A test computing a statistic over a
  `/trades` window PAGES it (`trade_window`) rather than asserting the page did
  not fill: the not-filled assertion was tried first and is a flake vector,
  because a window derived from an acceptance instant widens without bound at
  speed 100 and the test then fails on a run where nothing is wrong.
- A TUNGSTENITE `Error::Http` CARRIES THE REFUSAL BODY. Verified in round 2 on
  both `/ws` 400 paths, so a refusal test can and should assert the REASON
  rather than the status alone.
- THE VENUE'S DEFAULT LOG FILTER IS `mogwai=info` and `EnvFilter` matches
  targets by raw string prefix, so `mogwai_engine::orders` WARNs do reach
  stderr with no `RUST_LOG` set. THAT IS A DEFAULT, NOT A GUARANTEE, and the
  round-2 fix pass mistook it for one: the fallback applies only when `RUST_LOG`
  is ABSENT, the venue inherits the launcher's environment, and nothing in the
  suite controlled it. A test reading the venue's log PINS the variable through
  `LaunchSpec::env` rather than relying on it being unset.
- `ws.rs` GATES MARKET DATA AT SEND TIME, not at publish time
  (`passenger.stall.open_at(sim, now)`). A frame published before a blackout was
  armed and still queued when it lands is dropped too, so NOTHING is legitimately
  in flight past an armed window and a blackout test owes its ceiling no slack on
  that account. Round 2's fix pass asserted the opposite in a comment; the
  assertion it justified was fine, the reason was not.
- The venue serves NO SWEEP-PASS COUNTER. That missing observable is the sole
  reason two fixed sleeps survive round 1 rather than becoming conditions. Filed
  in `notes/todo.md`.

## Couplings and budgets

- THE PER-TEST BUDGET IS 20 SECONDS, on the parallel lane as well as the serial
  one, confirmed from the tool's own reference rather than inferred from
  `brokkr.toml`'s comments. Round 3 swept `lifecycle.rs`, `completion.rs` and
  `serving.rs` through the `common` budget helpers above, so the class is closed
  in those three files - but nothing detects a new `Instant::now() + <cap>`
  landing in them, and the other test binaries in `mogwai-cli` were not swept.
  Round 4 clamped the one deadline round 3 left (finding 17) and found a
  FOURTH FILE holding the same shape - `unconfigured_symbol.rs` had a 30 s
  drain bound, past the watchdog, so its panic could never print. THE SWEEP WAS
  PER-FILE AND THE REMAINING `mogwai-cli` TEST BINARIES ARE STILL UNSWEPT;
  nothing detects the shape, so it is found by looking or not at all. WHERE ONE
  IS FOUND, READ THE DRAIN IT SITS IN TOO: that same file's loop ended on a Ping
  or a Close as well as the deadline, and the round-4 pass clamped the bound
  while standing next to it. The two defects travel together because both are
  written by someone reaching for the shortest loop that compiles.
- THE GATE RUNS THE PARALLEL LANE and says so in its own output, so a green
  gate is NOT evidence about anything serial. Round 3 turned on exactly that
  distinction. Run `brokkr test -p mogwai-cli socket --debug` alongside it after
  touching the harness; it is the invocation AGENTS.md prescribes and the only
  one that exercises `--test-threads=1`.
- `brokkr check` is blind to the socket-backed suites; `brokkr check --gate` is
  the invocation. THE WORKSPACE TEST COUNT ONLY GOES UP, so the series below is
  the arc's ledger - but it spans TWO documents, and an entry that says just
  "round 4" is unreadable. Every entry names its document.
  - lifecycle r4: 1179 + 442, 65 ignored, 19 skips.
  - lifecycle r5: 1181 - two completion gates un-parked, so they RUN rather
    than being skipped (hence 63 ignored, 17 skips from here on).
  - lifecycle close pass: 1183 + 442, in 1m06s, 0 orphaned - two harness pins.
  - adapter r4: 1185 + 442, in 1m05s, 1690 coverage pairs, 0 orphaned - the
    segmented-head pin and the replay pin. Serial adapter sweep 39.71 s.
  - adapter r5: 1187 + 442, in 1m01s, 1692 coverage pairs, 0 orphaned - the
    session-disclosure pin and the undecodable-clock pin. SERIAL ADAPTER SWEEP
    12.14 s, from 39.71 s; the four socket binaries hold 60 tests totalling
    11.38 s one-per-process. See the round-5 section above and
    `reference/performance.md`.
  - adapter close pass: unchanged at 1187 + 442, 1692 pairs, 0 orphaned, 58.3 s;
    serial adapter sweep re-measured at 11.96 s. Prose fixes only, no new test.
  - tape r1: 1187 + 441, 1691 pairs, 0 orphaned, 59.1 s. THE COUNT WENT DOWN
    AND THAT IS CORRECT: an ignored `mogwai-data` test was removed from the
    suite entirely and one `mogwai-cli` test added, so the "only goes up" rule
    above holds for TESTS THAT STAYED TESTS, not for one that became an example
    target. THE TWO BUCKETS ARE THE TWO SWEEPS, workspace and instrumented, and
    that is the whole explanation for the net -1 landing entirely in the second
    one: `mogwai-data` is in BOTH sweeps, `mogwai-cli` is in the workspace
    sweep only. So the removal cost 1 from each bucket and the addition gave 1
    back to the first alone - 1187 stays 1187, 442 becomes 441, and the pair
    count goes 1692 - 2 + 1 = 1691. A cold review read the buckets as
    per-package and called the arithmetic inconsistent; they are per-sweep.
  - tape r1 fix pass: 1191 + 441, 1695 pairs, 0 orphaned, 58.2 s. Four
    `mogwai-cli` parser fixtures for the fixture-write scan, workspace sweep
    only, so +4 in the first bucket and +4 pairs, exactly as the rule above
    predicts.
  - tape r2: 1191 + 436, 1688 pairs, 1627 run, 61 ignored, 0 orphaned, 16
    skips; 41.4 s and 50.4 s on two runs. THE BASELINE
    IS THE LINE DIRECTLY ABOVE, tape r1 fix pass at 1191 + 441 and 1695 pairs -
    stated because a cold reviewer took the baseline from the adapter close
    pass, which used to be appended at the END of this list while belonging
    chronologically before tape r1, and got an arithmetic mismatch out of it.
    The list is in order now; keep it that way.
    THE SECOND BUCKET DROPPED BY 5, 441 to 436, AND THAT IS THE POINT: five
    heavy walks are now absent from the instrumented BUILD SHAPE, so they are
    no pair rather than an orphaned one. The dwell changes move NEITHER bucket:
    `run_seeded_tape_dwell_is_bounded` deleted is -1 in each, and
    `dwell_is_bounded_across_run_seeds` un-skipped so it RUNS is +1 in each.
    Pairs: -2 for the deletion (a `mogwai-data` test, so both shapes), -5 for
    the cfg (absent from one shape), 1695 - 7 = 1688.
    Serial `brokkr test -p mogwai-data "" --debug` runs THREE sweeps, not two,
    and applies no `skip`: 44.62 + 44.61 + 44.43 = 133.66 s -> 37.97 + 9.96 +
    38.22 = 86.15 s. The GATE's saving is a different number and comes entirely
    from the instrumented sweep; see `reference/performance.md`.
  - tape r3: 1191 + 436, 1688 pairs, 61 ignored, 0 orphaned, 51.9 s and 52.3 s
    across the fix pass and its review sweep. UNCHANGED from tape r2 on purpose
    - the round rewrote five vector executors inside one existing test and
    added none.
  - tape r4: 1195 + 440, 1692 pairs, 1635 run, 57 ignored, 0 orphaned, 14
    skips, 49.0 s. FOUR MOVING PARTS AND THEY SEPARATE CLEANLY: two new
    `mogwai-data` tests in `tests/tape_version_prose.rs` are +2 in EACH sweep
    (that crate is in both) and +4 pairs; the two un-ignored GARCH instruments
    are +2 run in each sweep, -4 ignored and -2 skips while adding NO pair,
    because an ignored test was already a pair. The wall is inside the noise
    the r2 entry warns about; read the counts. The review-repair pass re-ran it
    at 52.4 s with every count identical.
  - tape r5: 1195 + 440, 1692 pairs, 1635 run, 57 ignored, 0 orphaned, 14
    skips, 53.4 s. IDENTICAL TO tape r4 IN EVERY COUNT, which is the evidence
    the round added and removed no test: four existing tests were rewritten in
    place and one production function was split without changing what it
    returns. The review-repair pass re-ran it at 41.7 s, every count identical.
  - A GATE FAILURE THAT REPORTS 440 ORPHANS IS ONE FAILURE, NOT 441. The
    repair pass's first gate run died on
    `launch::tests::the_ready_bound_returns_on_time_against_a_silent_venue`
    with `ExecutableFileBusy` - `ETXTBSY`, a real race, unrelated to the round,
    and FIXED BY THE CLOSE PASS rather than left to fire a third time. The
    mechanism first recorded here was wrong: the helper's own write handle IS
    closed before the exec. What is not closed is the copy a CONCURRENTLY
    FORKING sibling test inherits between its fork and its exec, which the
    adapter document's pid qualification cannot reach because it is inside one
    process. See the write-then-exec entry under "Facts a later round would
    otherwise re-derive wrong". The WHOLE
    `instrumented` sweep then never ran, so coverage reported every one of its
    440 tests as orphaned. This looks exactly like the 2026-08-16 brokkr
    coverage bug `AGENTS.md` warns about and is NOT it: the tell is that the
    orphan count equals the missing sweep's pass count. Read the first
    `[error]` line, not the flood under it. A plain re-run was green.
  - engine/protocol r3: 1209 + 440, 1706 pairs, 1649 run, 57 ignored, 0
    orphaned, 14 skips, 55.0 s. THE ROUND'S OWN CONTRIBUTION IS +2 IN THE
    WORKSPACE SWEEP AND NOTHING IN THE INSTRUMENTED ONE: two new `mogwai-engine`
    tests and one new `mogwai-adapter` test, less the deleted `mogwai-protocol`
    timeout pin; `mogwai-engine`, `mogwai-adapter` and `mogwai-protocol` are all
    workspace-sweep only, which is why the second bucket does not move. Three
    further tests were rewritten in place and moved no count. THE CHAIN,
    reconstructed by the orchestrator because rounds 1 and 2 landed as `a3a796d`
    and `db5931b` without an entry here: r1 ended at 1201 + 440 / 1698 pairs,
    r2 at 1207 + 440 / 1704, r3 at 1209 + 440 / 1706. Record the gate counts
    per round as they land; a missing entry makes the next one's arithmetic
    unreadable, which is exactly what the tape r2 entry above warns about, and
    it cost r3 a reconciliation it could not do from inside the round.
  - engine/protocol r4: 1216 + 440, 1713 pairs, 1656 run, 57 ignored, 0
    orphaned, 14 skips, 55.0 s and 54.7 s across two runs. +7 IN THE WORKSPACE
    SWEEP AND NOTHING IN THE INSTRUMENTED ONE, all of it new: four
    `mogwai-protocol` tests in `sizing.rs`, two in `instruments.rs` and one
    `mogwai-engine` funding test. Both crates are workspace-sweep only, which
    is why the second bucket holds. One existing test was RENAMED
    (`account_state_bound_covers_a_max_length_account_id` to
    `..._covers_an_empty_and_a_maximal_snapshot`) and two rewritten in place,
    and none of that moves a count.
  - engine/protocol r5: 1220 + 440, 1717 pairs, 1660 run, 57 ignored, 0
    orphaned, 14 skips, 54.8 s. +4 IN THE WORKSPACE SWEEP AND NOTHING IN THE
    INSTRUMENTED ONE: four new `mogwai-protocol` tests in `messages.rs`
    (session ids, the two echo guards, the two truncations, the three scan
    predicates), and `mogwai-protocol` is workspace-sweep only. Existing tests
    were rewritten in place and move no count: five in `sizing.rs` gained the
    two-sided bracket, the zero-margin cache test gained an assertion that bites
    in both profiles, the refusal matrix moved to a funded fixture, and the two
    `contains("trigger")` sites became exact strings.
  - lab/cli r2: 1226 + 446, 1729 pairs, 1672 run, 57 ignored, 0 orphaned, 14
    skips, 51.7 s. +6 IN EACH SWEEP, +12 PAIRS, and the arithmetic is the rule
    above working exactly: five of the six new tests are `mogwai-lab`'s
    `ledger::tests` and the sixth is its `sampler::tests` drop pin, and that
    crate is in BOTH sweeps, so one test is two pairs. The six rewritten
    `mogwai-cli` tests move no count. The baseline is
    engine/protocol r5 at 1220 + 440 / 1717 - lab/cli r1 landed as `4a1e18e`
    without an entry here, and the arithmetic above shows it moved no count.
    Whoever reconciles r1's own effect on the ignored count owes it a
    measurement rather than a reading of its commit message.
  - lab/cli r3: 1229 + 449, 1735 pairs, 1678 run, 57 ignored, 0 orphaned, 14
    skips, 1m00s. The fix pass itself was 1227 + 447 / 1731 pairs (+1 in each
    sweep over lab/cli r2, the `mogwai-lab` sidecar attachment test); the
    review-repair pass added `the_ambient_reading_binds_each_variable_to_its
    _own_limb` and `the_channel_reads_the_documented_variable`, both
    `mogwai-lab` and therefore +2 IN EACH SWEEP and +4 pairs. Six tests were
    rewritten in place across `count_curve.rs`, `stage_m_tier2.rs` and
    `fit/curves.rs` and move no count.
  - lab/cli r4: 1230 + 450, 1737 pairs, 1680 run, 57 ignored, 0 orphaned, 14
    skips, 54.7 s. +1 per sweep and +2 pairs over lab/cli r3, and the +1 is the
    whole visible footprint of the round: everything else it changed lives
    inside `the_envelope_matches_the_closed_forms_where_they_are_exact` and the
    ten macro-generated fidelity parts, all already `#[ignore]`d and already
    skipped, so those could move no count. THE +1 IS THE POINT rather than an
    accident - `the_fidelity_predicate_bites_where_one_sample_never_moves` is
    the round's only assertion that a general check lane ever executes, and the
    defect the cold review caught was in the predicate it pins. A round whose
    subject is `#[ignore]`d tests otherwise leaves this ledger flat by
    construction; the evidence for the rest is in the eleven named release runs
    recorded in its section above, not here.
  - lab/cli r5: 1231 + 451, 1739 pairs, 1682 run, 57 ignored, 0 orphaned, 14
    skips, 59.0 s. Plus one in EACH sweep against r4's 1230 + 450, and plus TWO
    pairs, which is the one added `mogwai-lab` test counted in both shapes -
    the arithmetic that tells a lab test from a cli one at a glance. The
    `mogwai-cli` half is a
    CANCELLATION - the tautological `session_segment_at_agrees_with_mogwai_lab`
    deleted, `the_committed_binding_carries_the_window_run_final_walk_measures`
    added, both workspace-sweep only - and the net movement is the one
    `mogwai-lab` test the cold review's finding bought,
    `the_final_windows_two_encodings_of_its_length_agree`, which runs in the
    instrumented sweep too. Everything else the round changed was a rewrite in
    place: three refusal assertions in `stage_a_batch.rs`, one value assertion
    in `exact.rs`, the five scratch call sites, and the `summary`/`session`
    collapse.
  - lab/cli close pass: 1233 + 451, 1741 pairs, 1684 run, 57 ignored, 0
    orphaned.
  - protocol r1: 1239 + 451, 1747 pairs, 1690 run, 57 ignored, 0 orphaned, 14
    skips, 54.7 s. +6 IN THE WORKSPACE SWEEP AND NOTHING IN THE INSTRUMENTED
    ONE, which is the expected arithmetic: two `mogwai-protocol` unit tests
    from the fix pass (the numeric refusal and the absent/null companion), two
    `mogwai-cli` bin-crate unit tests (the argv crossing and its companion
    refusal), and two from the round's review repair
    (`a_published_risk_state_refuses_a_numeric_decimal` in `mogwai-protocol`,
    `an_opening_balance_must_be_spelled_as_a_string` in `mogwai-server`). None
    is in a package the instrumented sweep covers. The launcher-pid test was
    RENAMED rather than added, so it moves no count.
    A NOTE ON A FALSE ALARM WORTH NOT RE-DERIVING: the first gate run of this
    round reported 451 orphaned `mogwai-data` pairs, exactly the 2026-08-16
    tool bug `AGENTS.md` describes. It did not recur on any later run in the
    same tree with the same tool. Suspect a concurrent workspace lock before
    the tree.
  - protocol r2: 1241 + 451, 1749 pairs, 1692 run, 57 ignored, 0 orphaned, 14
    skips, 1m03s. +2 IN THE WORKSPACE SWEEP AND NOTHING IN THE INSTRUMENTED
    ONE, which is the whole of the round: two new `mogwai-engine` unit tests
    (the post-only admission table and the market-to-limit remainder pin), and
    `mogwai-engine` is workspace-sweep only. The existing refusal matrix was
    rewritten in place to read the shared constant and moves no count.
  The `mogwai-cli` serial socket suite is green in 6.5 s throughout.
- THE GATE'S `skip` LIST NO LONGER CARRIES A PARKED TEST, and `notes/todo.md`'s
  parked list is empty. What remains in `skip` is cost and environment, which is
  what that list is for - but the COST half was never audited against a
  measurement until the tape round did it, and one entry was flatly wrong while
  two more are off by an order of magnitude. See the tape round-2 section. A
  skip entry states a cost; nothing checks it - so the universal claim that
  headed that list ("every one of them outlives the 20-second watchdog by
  design") is GONE, replaced by a per-entry statement carrying the measured
  number and, where the exclusion is not about cost at all, saying so.
  `test_threads` STAYS AT 8 even so: the cliff at 16 was
  attributed to one of the un-parked tests, and the whole suite now passes at 16
  and at 32, twice each - but one removed cause is not evidence the cliff had
  only one, which is that todo item's own standard for this class of defect.
- `brokkr check --profile timing` is a SEPARATE lane and round 3 changed it. It
  now names one test, `read_market_latency_stays_within_submit_budget`; run it
  after touching either list, because `brokkr.toml`'s `only` and the gate's
  `skip` have to agree. THAT AGREEMENT IS CHECKED NOW, by
  `every_release_only_filter_is_skipped_by_the_gate`, and the config's prose
  saying nothing checks it was corrected in the same pass.

## Decisions already ruled on - do not silently reopen

- `history_is_bounded_by_the_rivers_own_boat_not_the_venue_clock` keeps its
  `sleep(250ms)`. It asserts its premise afterwards, so a short sleep fails the
  premise rather than faking the property. That is already the correct pattern.
- `a_perpetual_position_pays_funding_across_an_interval` keeps its
  `sleep(3_000)`. The clock-poll replacement was implemented, RUN, and found
  vacuous for the delivery reason above. The binding resource is the sweeper's
  wall cadence.
- THE READINESS RECORD DOES NOT CARRY A BOOT SYMBOL, and this is a ruling, not
  a gap. Commit `0f12796` removed it as slice 2 of the grand design - "a venue
  has no symbol under the boatyard model, so the readiness record's symbol field
  could only lie" - and put `common::boot_symbol`'s config-side resolution here
  in its place, with `preset_only_config_resolves_the_boot_river` as its literal
  pin. Round 3 refused finding 16 on that ground: putting the field back is an
  owner decision costing a schema version and every consumer, not a fix a
  test-hygiene finding authorizes. The harness now carries the reasoning in
  place, so the next reader does not rediscover it as an accident. Do not remove
  that pin, and do not let it become the only non-trivial boot river in the tree
  without noticing.
- `unconfigured_symbol.rs` KEEPS `FOOBAR` / `BARFOO`. Round 4 refused the
  rename: `FOOBAR` is the workspace idiom for an unconfigured label, used by
  `config.rs`, `source.rs`, `seeds.rs` and `configs/unmatched-symbol.toml`, and
  a shared literal across SEPARATE BINARIES costs nothing. The hazard round 4
  named instead - that both tests assert an ABSENCE first, which is sound only on
  a venue nothing else has touched - is now moot for this file and live as a
  general rule: round 5 refused the shared-venue split outright, so nothing here
  shares a venue with anything.
- NO TEST BINARY IN THIS WORKSPACE SHARES A VENUE, and round 5 refused the
  proposal to start rather than deferring it. THE MEASUREMENTS THAT DECIDED IT,
  because the next document in the arc covers the adapter's four socket-backed
  binaries and will be offered the same idea: a `fast.toml` venue costs about
  10 ms end to end - launch, bind, 300 s of warmup, one round trip - because
  these tests build under an OPTIMIZED profile and 300 simulated seconds is
  ~15,000 ticks; `serving.rs`'s whole wall at `--test-threads=8` is 9.77 s
  against a SINGLE test at 9.63 s, so its floor is one test's deliberate flake
  margin and not 54 boots; and raising the thread count to 16 leaves that wall
  unchanged, so contention is not the floor either. A venue is not the expensive
  thing. Measure before accepting that it is.
  - AND "READ-ONLY" IS NOT "GET-ONLY". Only six of `serving.rs`'s 54 tests issue
    GETs alone, and two of those still cannot share:
    `history_refuses_an_illegal_symbol_and_serves_an_unconfigured_one`
    MATERIALIZES rivers, which `instrument_defs` then advertises, and
    `a_paged_tape_window_equals_the_same_window_read_in_one_query` asserts its
    window still fits one page, which a longer-lived venue's growing tape
    falsifies. Ask what a test assumes about the venue's HISTORY, not whether it
    writes.
  - THE BUDGET QUESTION A SHARED VENUE RAISES IS THEREFORE UNANSWERED AND DOES
    NOT NEED ANSWERING: `spawn` re-anchors only when no venue is live, so a
    leaked `OnceLock<Venue>` would pin `LIVE_VENUES` above zero forever and every
    test after the first would inherit a spent budget and be refused by the
    clamp. Anyone reviving the idea owes that mechanism a redesign, plus a
    replacement for the panic-path log dump and the guard's kill-and-reap, which
    a leaked venue never runs (`PR_SET_PDEATHSIG` is what would still reap the
    process).
- A SHELL SPAWNED FROM A TEST LEAKS ITS GRANDCHILDREN. `Child::kill` reaches the
  shell and nothing it forked; `/bin/sh -c "... & sleep 3600"` orphans the sleep
  onto init on the SUCCESS path, and nine of them had accumulated on the machine
  before round 4 looked. The fix is `process_group(0)` at spawn plus `killpg` in
  a drop guard, and it composes with a test whose property needs the child alone
  signalled: kill the child explicitly, let the guard take the group afterwards.
- `analysis/asia_jump_probe.py` is untracked, unrelated to this arc, and
  predates it. It is not to be swept into a commit. Nineteen cold reviewers
  have now analysed it unprompted; the standing answer is that it is out of
  scope and stays untracked.
  - ONE THING FROM THAT NOISE IS WORTH KEEPING, because it is the shared-fixture
    trap in a form that would not be caught by a gate: the probe's percentile
    convention does not match `mogwai_lab::kernel::nearest_rank_list`. A number
    out of that script must therefore never land in a durable doc labelled
    "p95" and get compared against a Rust-computed one - two implementations of
    a quantity, no shared fixture, and both sides green because neither is
    checked. This is a note about the CONVENTION, not a request to fix the file.

## The protocol document, round 3: the launcher residuals (C, E, F)

Ledger: round 3 ends at 1244 + 451, 1752 pairs, 1695 run, 57 ignored, 0
orphaned. Round 2 ended at 1241 + 451 / 1749 / 1692, so the delta is exactly the
three tests this round added and nothing was reclassified.

- A `recv_timeout` PARKED ON A CHANNEL IS NOT A POLL, and the whole of finding C
  rested on reading it as one. Dropping the sender DISCONNECTS the channel and
  `recv_timeout` returns at once; the interval bounds only the arm that runs when
  NOBODY signalled. `LaunchedVenue::drop` was measured at 314 us and 258 us
  against an `OWNER_POLL` of 200 ms, with the owner provably parked mid-interval
  (two completed poll windows first). The finding wanted the loop replaced with a
  `SIGCHLD` waiter to buy exactly that. NINETEENTH OVERTURNED ARGUMENT OF THE
  ARC, and the cheapest one yet: one test, four minutes.
  - The residual was real though, and it was INVISIBILITY rather than latency.
    `sleep(owner_poll); try_recv()` is an equivalent-looking refactor that passes
    every other test in the module and costs a full interval on every caller's
    drop. `tearing_down_a_healthy_venue_is_a_wakeup_not_a_poll_interval` pins it.
    GENERAL SHAPE: when measurement refuses a finding, the property that made it
    refusable is usually undocumented and untested, and THAT is what the round
    owes.
  - AND THE TEST THAT PINS IT WAS FIRST WRITTEN AS A LATENCY BUDGET IN THE
    PARALLEL DEV LANE - production `OWNER_POLL` of 200 ms, teardown asserted
    under a quarter of it. THAT IS THE SHAPE THIS WORKSPACE ALREADY DELETED ONCE
    (`tape_lateness_under_acceleration`: 50 ms asserted, 311 ms measured in
    RELEASE at load average 1.46), and `brokkr.toml` carries a standing routing
    for the class - `#[ignore]` at the source, an entry in the gate's `skip`, and
    a name in the `timing` sweep's `only`. NOTHING ENFORCES THAT ROUTING:
    `every_release_only_filter_is_skipped_by_the_gate` checks that the `only`
    filter's tests are gate-skipped, never that a test carrying a budget has a
    filter. So the two ways out are ROUTE IT or STOP IT BEING A BUDGET, and the
    second is better whenever the interval under test is a parameter: the owner
    runs at ten seconds here, the honest teardown is sub-millisecond, and the
    bound is two seconds - the loose-upper-bound class the module's other
    wall-clock assertions are already in (10 s against a 60 s child, 5 s against
    a 300 s bound), not a budget any host has to be fast enough to meet.
    Bite-checked with the `sleep(owner_poll); try_recv()` rewrite: 9.951 s and
    9.950 s against the two-second bound, both sweeps. CARRY THE TEST: before
    writing a wall-clock assertion, ask whether the thing being measured against
    is a constant you can inflate. If it is, inflate it and the assertion stops
    being about the host.
- `mogwai-adapter` NEVER HOLDS A `LaunchedVenue`. It re-exports
  `mogwai_protocol::launch` and nothing else - no client, factory or config in
  that crate constructs or drops one. Any future finding phrased as "the nautilus
  host will drop it on a tokio worker" has to go through broadarrow's
  `ba-worker`, which is where the only async consumer actually lives: it holds
  the venue at worker scope across `async fn run` on a `current_thread` runtime
  and calls `shutdown()` at the end. Check `research/broadarrow` for the consumer
  shape rather than assuming the adapter is it.
- THE LAUNCHER NO LONGER IMPOSES A RULE ON THE VENUE'S STDOUT. The readiness
  reader `io::copy`s into `io::sink()` after sending the record and runs to EOF,
  so a stray `println!` in the serve path is discarded instead of taking `EPIPE`
  mid-run. Chosen over the alternative - a venue-side test forbidding stdout
  writes - because a venue is a process and a launcher that cannot survive one
  writing to its own stdout is the defective party; the prohibition would also
  have been enforceable only over code inside this workspace. Consequences that
  bind: `read_ready` takes `&mut impl Read`, the reader thread is NEVER JOINED on
  any path (the timeout arm already was not, per finding D, so the
  `reader_is_released` flag is gone), and the thread's lifetime is now the
  child's.
  - THE BITE-CHECK IS ALSO THE ONLY DIRECT OBSERVATION OF THE BUG. `/bin/sh` does
    not ignore `SIGPIPE`, so with the drain replaced by `drop(stdout)` the
    fixture dies of the signal and records
    `VenueExit { success: false, code: None }` where it asked for `Some(0)`.
    A shell fixture is the cheap way to make an ignored-signal failure visible,
    because Rust's own `SIGPIPE` handling would have hidden it.
- `LaunchError::Thread { what: &'static str, source }` exists now, and no site may
  put a role name in `Spawn`'s `binary` field again. Adding the variant broke
  NOTHING: no exhaustive `match` on `LaunchError` exists in this tree or in
  `research/` - `mogwai-cli`'s `lifecycle.rs` matches `NoRecord` and `Timeout`
  non-exhaustively, and broadarrow only renders `Display`.
  - AND THAT GREP IS NOT OWED AGAIN. `LaunchError` is `#[non_exhaustive]` as of
    this round, which was the one-line durable fix the first pass discharged by
    hand instead. GENERAL FORM, and it is worth reaching for on sight in this
    pre-1.0 workspace: when the safety of a change rests on a manual survey of
    consumers, half of whom are not in this repository, prefer the attribute that
    makes the survey unnecessary over the survey.
  - THE VARIANT'S MESSAGE IS THE ROUND'S SECOND LESSON ABOUT ITS OWN FIX: it
    ended "nothing was spawned", which is FALSE at the readiness-reader site,
    where a venue process is already running. A change whose entire justification
    is that an operator must not be sent after the wrong remedy shipped a
    sentence telling them no venue had started. Cold review caught it. When a fix
    is about the truth of a message, check the message at EVERY site that raises
    it, not at the one that motivated it.
  - THE COUPLING IS STRUCTURAL, NOT TESTED, and the distinction was mis-recorded
    once (see the ledger-honesty entry below). `spawn_launcher_thread` is the
    module's only thread-creation call and the only constructor of the variant;
    `a_failed_launcher_thread_does_not_blame_the_binary` pins the RENDERING only,
    because reaching the real branch means driving the process to its thread
    limit.
- LEDGER HONESTY BIT ON THIS ROUND'S OWN RECORD. The first pass wrote a
  bite-check it could not have run - "restoring the `Spawn { binary: "<owning
  thread>" }` construction: it fails naming the old rendering" - for a test that
  constructs the error literally and never calls `launch`, so a call-site revert
  leaves it green; only deleting the variant moves it, and that is a compile
  error, not a failure. The test's own doc comment was honest about this and the
  two notes were not. THE RULE THE ARC ALREADY HAD, restated because it was
  broken by a pass that knew it: a bite-check claim names the EDIT that was made
  and the failure that was OBSERVED, and if the edit was never run the claim is
  not written. A test that constructs its subject cannot be bite-checked through
  the production call sites - say what it pins and say what stays unpinned.

## The protocol document, round 4: finding L, refused - and the gap it named

Ledger: 1245 + 451, 1753 pairs, 1696 run, 57 ignored, 0 orphaned. Round 3 ended
at 1244 + 451 / 1752 / 1695, so the delta is exactly the one test this round
added.

- `Symbol` STAYS `Arc<str>`. THE TWENTIETH OVERTURNED ARGUMENT OF THE ARC, and
  the first refused for MAGNITUDE rather than for a false mechanism - the
  mechanism was true and the hunter UNDERSTATED it. Deserializing `Arc<str>`
  costs TWO allocations per frame, not the one reported: serde takes a `String`
  and copies it into a fresh `Arc`. An inline 32-byte `Copy` symbol removes both.
  - MEASURED, `crates/mogwai-protocol/examples/symbol_decode_probe.rs`, 2 M
    iterations per arm, release, run three times: landed `from_json_str`
    239/220/219 ns at 2 allocs; the payload with today's `Arc<str>` 110/115/109 ns
    at 2 allocs; the same payload with an inline symbol 103/111/103 ns at 0
    allocs. So the edit buys 4 to 7 ns and 2 allocations per frame.
  - THE FIRST TABLE SAID 19 ns AND THE PROBE WAS BIASED TWO WAYS AT ONCE, both
    found by cold review, and this is the part to carry rather than the number.
    A PROBE'S ARMS MUST DIFFER IN EXACTLY ONE WAY, and an arm's cost is only
    measured if something OBSERVES the thing that costs. The inline arm
    black-boxed `symbol.as_str().len()` - a read of the `len: u8` field, with
    nothing observing `bytes`, so LLVM could elide the 32-byte `copy_from_slice`
    that is the entire representation cost. And the inline arm alone ran
    `validate_wire_symbol`, the opposite bias, undisclosed. Both arms now
    black-box the whole tuple and neither validates; the omitted alphabet check
    is a cost the proposal would carry, so the figure is an UPPER BOUND on the
    saving. Neither bias changed the verdict, which is exactly why they survived
    a round: A BIASED MEASUREMENT UNDER A CORRECT CONCLUSION IS STILL A DURABLE
    NUMBER SOMEONE WILL QUOTE.
  - WHAT REFUSES IT IS THE ARM NOBODY ASKED FOR, and this is the transferable
    part: the probe also measures WHAT RUNS RIGHT AFTER THE DECODE.
    `mogwai_adapter::convert::trade_id` `format!`s all five trade fields and
    costs 154/155 ns and FIVE allocations, on its own, per trade - before the
    nautilus event construction, `handler().await`, the tungstenite framing and
    the `Message::Text` `String` the frame already arrived in. A 19 ns saving
    inside a 217 ns decode whose immediate consumer spends 155 ns is not
    observable at any rate this venue serves.
    GENERAL FORM, and it is cheap enough to be a habit: WHEN A FINDING PROPOSES
    OPTIMIZING ONE STAGE, MEASURE THE NEXT STAGE IN THE SAME PROBE. A per-frame
    number alone cannot say whether the frame's budget is where the finding
    thinks it is, and the neighbouring arm costs one function to write.
  - THE ONLY DECODER OF `ServerMessage` IS THE ADAPTER'S READ LOOP.
    `from_json_str` and `from_json_slice` have exactly one call site each, both
    in `lifecycle.rs`. Nothing in `mogwai-server`, `mogwai-cli` or `mogwai-lab`
    decodes one. Any future finding phrased as "the hot decode path" is talking
    about those two lines and nothing else. READ THAT AS THE NARROW CLAIM IT IS:
    symbols also reach the adapter through five UNTAGGED HTTP decodes that are
    not `ServerMessage` and validate nothing - `client/shared.rs` (instruments),
    two in `client/data.rs`, `client/exec.rs` and `clock.rs`. Deliberate, same
    posture as the decode bullet below, but "the only decoder" and "the only
    place a symbol arrives unvalidated" are different sentences and the round's
    first draft ran them together.
  - AND THE CORRECTNESS HALF - `MAX_SYMBOL_LEN` as a property of the type - IS
    THE STRONGER ARGUMENT, AND ITS AUDIT WAS WRONG. The audit wrote "three kinds
    of ingress and all three validate"; there are four, they check three
    different things, and the client-inbound one WAS OPEN.
    `validate_submit_order` checked `symbol.len() > MAX_SYMBOL_LEN` and nothing
    else, so `""` and `"MNQ\n\u{7f}<script>"` were admitted at order entry - the
    one ingress a hostile client controls - under a document asserting it closed.
    It calls `validate_wire_symbol` now, which also covers
    `ClientMessage::SubmitOrderGroup`'s up-to-`MAX_GROUP_ORDERS` symbols through
    `validate_submit_group`. Nothing in the tree submits a symbol the alphabet
    refuses, and the engine already refused an unknown instrument, so the change
    tightens without narrowing any live caller. The other three: `http.rs` and
    `source.rs` validate URL-carried symbols; `config.rs` validates an
    instrument's `index_symbol` but NOT its own `symbol` (non-empty and length
    only - filed in `notes/todo.md` as an owner-level policy question); and the
    adapter's decoded symbol is unvalidated ON PURPOSE, with
    `convert::instrument_id` using `NautilusSymbol::new_checked` and a doc
    comment giving the reason - the conversion runs in an unsupervised spawned
    task, so a hostile symbol must cost one frame rather than the task.
    Validating at decode would MOVE that refusal, not close a hole. DO NOT
    REOPEN L WITHOUT DISPLACING ONE OF THOSE.
    THE SHAPE THAT GENERALISES, and it is the arc's own signature defect in a
    new place: A REFUSAL'S AUDIT IS THE SENTENCE THAT TELLS THE NEXT READER NOT
    TO LOOK, so an enumeration inside one is load-bearing in a way a passing
    remark is not. This one enumerated from memory of the call sites rather than
    from the validators' bodies, and the two disagreed at exactly one call.
- THE PROBE IS KEPT AND DELIBERATELY UNREGISTERED, with the table and its
  reading in `reference/performance.md`, a `NOT REGISTERED` note in
  `brokkr.toml` beside `fill_walk_bench`'s, AND a bare `[[example]]` entry in
  `mogwai-protocol/Cargo.toml` beside `tag_decode_probe`'s. The manifest entry
  is not redundant with the `brokkr.toml` note: both probes wrap the global
  allocator, so neither may ever gain `required-features`, and the person who
  would add one is editing the manifest, not the tool config. The arc's rule that a refusal owes
  the property that made it refusable applies to performance findings as
  measurements rather than as tests: there is no assertion to write here, so
  what is owed is a re-runnable number and the reason it decided. A registered
  target would invite re-benching a settled decision.
- NO `TAPE_PROTOCOL_VERSION` BUMP WAS OWED. Nothing in the tape-generation path
  was touched and no artifact moved; the new file is an example target and the
  production change is a wire-boundary validator.

## The protocol document, round 5: what carries to `bugs-data`

Ledger: 1245 + 451, 1753 pairs, 1696 run, 57 ignored, 0 orphaned - unchanged
from round 4, because the round added no test COUNT. It did widen one:
`admission_frames_fit_their_ceiling` went from serializing one subject variant
to serializing all seven.

`bugs-protocol` CLOSED on 2026-08-19 with no open findings - five fixes (A, B,
D, E, F), three recorded refusals (C, L, M) and K closed across two documents.
`bugs-data` is next. The protocol-specific mechanics (the launcher's owner loop,
the wire-size constants, the decimal annotations) die with that document. What
generalises:

- A FINDING CLOSED BY ANOTHER DOCUMENT IS VERIFIED HALF BY HALF, NOT AS A UNIT.
  K had three halves; the other document's two rounds closed two of them and
  never saw the third, because that half named a constant in `messages.rs` while
  those rounds were scoped to `sizing.rs`. THE SCOPE THAT CLOSED A FINDING IS
  NOT THE SCOPE THE FINDING WAS WRITTEN IN, and a multi-part finding routed to a
  narrower document loses exactly the parts that fell outside it. Read each half
  against the code before writing "closed elsewhere".
- RE-CHECK A REFUSAL WHOSE GROUND YOUR OWN DOCUMENT LATER MOVED. M was refused
  on "`NoRecord` is stdout EOF, and a venue can reach that branch alive"; round
  3 of the same document then changed what the stdout reader does on that
  branch. It happened to strengthen the refusal - the reader now parks in
  `io::copy` to EOF instead of finishing, so the branch carries TWO live threads
  rather than one - but that is a fact that had to be established rather than
  assumed. A refusal is a claim about the code, and it expires like any other.
- TWO PIPES, TWO THREADS, AND A SMELL THAT NAMES THE WRONG ONE. The `NoRecord`
  sleep gives the STDERR drain a moment to fill the ring; every reading of it as
  "join the reader, the child is dead" conflates it with the STDOUT thread. When
  a finding proposes replacing a wait with a join, name the thread the wait is
  actually waiting on before judging the wait.
- "THE TEST ALREADY COMPUTES IT" IS A CLAIM ABOUT THE TEST, AND IT WAS HALF
  FALSE. `ADMISSION_FRAME_MAX_BYTES`'s comment carried a `MAX_SYMBOL_LEN` term
  the frame does not have, and the fix pass closed it as a doc edit on the
  ground that `admission_frames_fit_their_ceiling` computes the analytic itself.
  The ANALYTIC half of that is true. The EMPIRICAL half was not: the test
  serialized only the `Submit` subject variant, so it never measured the widest
  one, while the comment it was cited to hold up promised the derivation was run
  rather than trusted. THE ARC'S SIGNATURE DEFECT, CLOSING THE ARC - a comment
  promising a test runs its claim, where the test runs a narrower one. Before
  writing "the test already holds this", read WHICH CASE the test builds, not
  what it asserts.
  - The repair is the general one for a bound over a sum type: enumerate EVERY
    variant at its maximum, bound each, and take the max for the comparison. It
    measured `Query` at 3604 as the real widest against `Submit`'s 3594 and
    `Frame`'s 3188, so the old fixture pinned neither end, and it bites - a
    widened `QueryKind` spelling fails at 4103 over the 4096 ceiling only after
    the four id-only variants have passed.
  - AND A DOC COMMENT STILL HAS NO BITE-CHECK. What it has instead is an
    obligation to be literally true: the first cut said one capped id is the
    whole subject contribution "whichever variant it is", which is false for
    `Query`, and the constant it silently leaned on -
    `ADMISSION_ENVELOPE_BYTES` - scoped itself to scaffolding that does not
    include a serialized enum value. A bound can be SAFE and its stated
    derivation still WRONG; correcting one is not correcting the other.
- A COMMENT NAMING A FUNCTION IS A CLAIM THAT DECAYS SILENTLY. Three comments
  in `launch.rs` said the readiness thread blocks in `read_line`; it calls
  `read_ready`, which uses `read_until`, and has for some time. Nothing checks a
  function name in prose, exactly as nothing checks a count. Fixed here; the
  ONE remaining `read_line` in that file is a HISTORICAL record of the code the
  wall-clock bound replaced and is correct as written.
- COUNT AT THE SITES, NOT BY GREPPING THE FILE. The bullet below was filed as
  "five refusal messages" and there are four: a Rust module carries its tests in
  the same file, so every refusal string appears TWICE, once in production and
  once as a test expectation. A count asserted in prose that no gate checks is
  the very defect this bullet records, committed while recording it - the third
  miscount of the arc.
- LATERAL, RECORDED NOT FIXED: `havoc::validate_divergence` spells the
  divergence ceiling as the literal "3600000" in FOUR refusal messages while
  `control::MAX_DIVERGENCE_MS` is the constant - counted at the production
  sites, not by grepping the file, because the same four strings appear again as
  the test's expected values and a raw match reports eight. Correct today. Same
  family as
  the lab/cli round-5 item about a refusal text hardcoding its cap - a live fact
  asserted inside a string literal, where no prose gate reaches. Worth folding
  into whichever round next touches those messages.

## The protocol document, close pass: what the five commits left

Ran 2026-08-19 over `c5c2209^..HEAD`, five commits. THE ARC'S FIRST
PRODUCTION-CODE DOCUMENT, so the close pass had shipped behaviour to check and
not only gates. ONE FINDING, and it is the arc's signature defect for the
twenty-seventh time - A THING THAT READS AS SCOPED NARROWER THAN IT IS - in the
unreviewed half of the round-4 commit, which is where all five previous close
passes found theirs.

- `validate_wire_symbol`'S DOC STILL SAID IT WAS A URL RULE. Round 4 routed
  ORDER ENTRY through it - `validate_submit_order` calls it, and
  `validate_submit_group` reaches it per member - and left the function's own
  doc comment reading "validate a symbol that is carried directly in a URL query
  string", with the alphabet justified entirely by percent encoding. So the one
  place a reader looks before relaxing the rule described a blast radius of two
  HTTP query params, while the real one now includes the venue's order carrier.
  A caller widening the alphabet for a URL reason would have widened order entry
  without knowing it. The doc now names every caller, says which ingress does
  NOT use it (`config.rs`'s instrument `symbol`, the recorded asymmetry), and
  states that relaxing it relaxes order entry.
  - THE GENERAL SHAPE, and it is worth a habit: WHEN A FIX GIVES AN EXISTING
    FUNCTION A NEW CALLER OF A DIFFERENT KIND, THE FUNCTION'S OWN DOC IS PART OF
    THE FIX. Its old scope sentence does not become false gradually - it becomes
    false at the moment the call lands, and nothing in the toolchain reads it.
    The round wrote the new coupling down in three durable places
    (`reference/performance.md`, `notes/todo.md`, the test's doc comment) and
    not in the one a reader of the validator would reach.

Everything else held, and the production changes were re-derived against source
rather than read:

- THE DECIMAL WIRE. Counted rather than trusted: `messages.rs` carries 35 serde
  `Decimal` annotations - 25 `rust_decimal::serde::str` and 10
  `crate::decimal::str_option` - and the hand-maintained table's rows cover
  exactly those 35 fields across the ten named shapes. The count the round
  wrote down is the count in the file. `str_option`'s `visit_some` really does
  delegate to the dependency's `str` deserializer, so the optional rule cannot
  drift from the required one, and `str_map` builds its map through the same
  deserializer for the same reason.
- THE POST-ONLY ORDER. The claim that both gates reach the rule in the same
  position is TRUE, and it is stronger than the comments state: at the wire the
  post-only arm sits after the whole type/price-shape match and before the
  conditional-IOC arm, and `Engine::validate_submit` now has it in exactly that
  slot - after its own shape match, before its conditional-IOC check. Moving it
  up displaced nothing else.
- THE WIRE/ENGINE DISAGREEMENT THAT LOOKS LIKE THE NEXT ONE IS NOT ONE. The wire
  admits a priceless `Market` order and the engine rejects it with "submit price
  required". That is a documented two-phase split, not drift: `validate_submit_order`
  gates the PRE-stamp wire (a nautilus MARKET order carries no price there) and
  the server stamps a synthetic execution price before the engine sees the
  order. Recorded here because it is the first thing a reader auditing the
  post-only unification will trip over.
- `ADMISSION_FRAME_MAX_BYTES`. The corrected derivation computes:
  `6 * (64 + 512) + 256` is 3712, and the next power of two is 4096.
- THE LAUNCHER. `LaunchError::Thread`'s "no venue is left running" is true at
  both sites - the owner-thread site never reaches the spawn, and the
  reader-thread site falls through to `own_venue`'s unconditional kill-and-reap.
  `read_ready` takes the handle by reference, so the stdout drain resumes on the
  same handle; the bytes its `BufReader` pulled past the newline are lost, which
  is correct for a reader that discards the rest. `docs/cli.md` promises FOUR
  details of the guard and delivers four.

## What is closed and what remains

Six of the eleven `bugs-*` documents are CLOSED as of 2026-08-19, each after
five rounds plus a close pass over its own commit arc:
`bugs-tests-lifecycle`, `bugs-tests-adapter`, `bugs-tests-tape`,
`bugs-tests-engine-protocol`, `bugs-tests-lab-cli` and `bugs-protocol`.

FIVE REMAIN, in this order: `bugs-data`, `bugs-engine`, `bugs-server`,
`bugs-cli`, `bugs-adapter`. All five are production-code documents, so the test
arc's default diagnosis - "the test cannot fail" - is no longer the first thing
to reach for; what carries from it is everything about the tests a production
fix WRITES.

THE DEFERRALS THE PROTOCOL ARC FILED live in `notes/todo.md` and each names the
document that owes it: the `MarketToLimit` defect (one defect, two symptoms,
owner-level, `bugs-engine`), `config.rs`'s instrument `symbol` alphabet
asymmetry (owner-level, `mogwai-server`), the unrouted wall-clock budget
(owner-level if anyone wants it mechanised), the launcher's single-process kill
and its `killpg` remedy, and the refusal texts spelling their own ceilings as
literals. The `run_complete_is_stamped_on_the_receiving_sockets_clock` lead is
in `notes/bugs-server.md` section 9.

## Loop conventions for this arc

- Every stage runs as a foreground Opus subagent, including the fix pass and the
  cold review, which the generic workflow assigns to codex. The cold review must
  stay genuinely cold: an impoverished prompt, no findings, no claims, no
  checklist.
- THE SECOND HALF OF EVERY COMMIT IS UNREVIEWED, and a close pass over the whole
  arc is what catches it. The round shape is fix pass, gate, cold review, then a
  fix-and-commit agent that closes the review's findings AND commits - so that
  agent's own fixes, its new tests and its doc edits reach master with no
  independent eye, in an arc where the first half of every single round contained
  a hole one layer up from what it was closing. Round 6 found two there: the wall
  budget's own reset moving an anchor forward, and a durable performance number
  recorded from a draft of the instrument that the same commit then fixed. BOTH
  WERE IN THE UNREVIEWED HALF. Budget a close pass per document, and point it
  there first. FIVE DOCUMENTS, FIVE CONFIRMATIONS, with no exception yet. Three
  of them were all PROSE - a false equation, a self-contradicting section, a
  subcommand description - which is what the unreviewed half of a round about
  tests produces once the tests themselves are being written carefully. The
  lab/cli one was not: it was a runtime guard installed at two of the six call
  sites that owed it, under a comment stating the set as two. So point the
  close pass at the durable prose the round touched AND at the call sites of
  whatever the round's last fix installed - a fix written after the last cold
  review is the least-examined code in the arc, and it is the code closing a
  finding, which is where the stakes are highest.
- Any ADJUDICATOR launched in this arc reads `notes/todo.md` and `brokkr.toml`
  in addition to its fork framing and the usual contracts. Owner instruction.
  `brokkr.toml` carries the gate profile's parallelism and skip list, which
  several findings in these reports turn on directly.
- `reference/INVENTORY.md` DOES NOT EXIST in this repository. The generic
  orchestration workflow names it as a mandatory read; it is an unsubstituted
  project variable from another repo, and nothing in this tree cites it. Do not
  put it in a brief. The real contracts are `AGENTS.md` and `CLAUDE.md`, then
  `reference/architecture.md`, `clock.md`, `glossary.md`, `performance.md` and
  `technical-implementation-spec.md`.

## The data document, round 1: the phantom parent

Landed as `e2e58ee`. Ledger 1248 + 454, 1759 pairs, 1702 run, 57 ignored, 0
orphaned. Closed findings 1 and 2 of `notes/bugs-data.md`; findings 3 through 7
remain, UNRENUMBERED and uncompressed, and that document's preamble now records
why its sections start at 3.

MACHINERY LATER ROUNDS MAY BUILD ON AND MUST NOT BREAK:

- `GeneratedSource::advance_parent` returns `Result<ParentSummary, TickFault>`.
  It was infallible, and on an `ArrivalRefusal` it fabricated a phantom -
  `ParentSummary { parent_ts_ns: 0, child_count: 0, .. }` on a fresh source -
  because `begin_integrated_event` latches `self.fault` and returns WITHOUT
  touching `burst`. Two guards are needed and both are present: a terminal-fault
  check on entry, matching `next_tick`, AND a re-check after `begin_event`. The
  entry guard alone stops the `seek_to` spin, which is why the spin test only
  goes red with both removed - a half-bite that was caught rather than accepted.
- Five call sites now handle the refusal: `seek_to` adopts the faulted clone and
  returns `None`; `CheckpointIndex::extend_toward` adopts the faulted lead and
  breaks, crediting no `walked` or `since_snapshot` and taking no snapshot;
  `mogwai-lab`'s `arrival_screen.rs` reports it through the new shared
  `screen_refusal_from_arrival`, used by BOTH arms; `arrival_envelope.rs` maps it
  to a `LabError::refusal`; and `mogwai-cli`'s `tick_composition.rs` `measure`
  panics naming preset, seed and mode, because that function returns `Reading`
  rather than a `Result` and its loop exits only on timestamps. THAT PANIC IS
  THE ONE SITE TO REVISIT IF `measure` EVER BECOMES FALLIBLE, and the note
  saying so lives at the site.
- `refusing_source_at(origin_ns)` in `generated/tests.rs` is the refusal fixture
  and it is worth understanding before reusing it. `next_parent` computes
  `limit = from_ns.saturating_add(MAX_SESSION_GAP_NS)`, so any near-max origin
  saturates the limit and the kernel refuses `NoOpenExposure` cheaply. But the
  two origins are NOT interchangeable: at exactly `u64::MAX` the budget walk
  exits before its first iteration and no calendar is consulted, while at
  `u64::MAX - 1_000` it takes one iteration and DOES reach `next_segment_end`.
  Only the near-max origin keeps `extend_toward`'s `lead.clock_ns() < target`
  guard satisfiable, which is the whole reason the checkpoint arm could be
  bitten at all.

FACTS A LATER ROUND WOULD OTHERWISE RE-DERIVE WRONG:

- THE REPORT'S FINDING 2 WAS RIGHT ABOUT THE DEFECT AND WRONG ABOUT THE DAMAGE.
  It claimed the lab consumers produce "an endless run of phantom zero-child
  parents at a frozen timestamp". They do not: both `arrival_screen`'s projection
  and `arrival_envelope`'s `stats_from_parent_walk` carry stall guards on a
  non-advancing timestamp. The real damage was MISATTRIBUTION - a refused cell
  reported as "candidate walk stalled", which aborts the whole run, instead of a
  recorded refusal verdict. Same family, different blast radius. The structural
  fix stands on the corrected ground, not the reported one.
- NO `TAPE_PROTOCOL_VERSION` BUMP WAS OWED and the reasoning is in `e2e58ee`'s
  message: the added expressions only READ `self.fault`, so on any non-refusing
  draw the RNG sequence, clock, burst and emitted values are untouched, and the
  refusal path produced no valid tape to move. Round 2 then took the next
  identity for the composer's price rails, so `TAPE_PROTOCOL_VERSION` next
  takes 22. A later data round touching the generator will almost certainly owe
  one; this round genuinely did not.
- THE 454-ORPHANED-`mogwai-data` WALL HAS A LIKELIER CAUSE THAN THE BROKKR BUG.
  `run_complete_is_stamped_on_the_receiving_sockets_clock` is intermittent and
  real - it failed once more this round at `completion.rs:510` on a tree confined
  to `mogwai-data`, a comment and markdown, and passed on the immediate re-run.
  It aborts the parallel sweep, which produces exactly the ambiguity `AGENTS.md`
  warns reads as a coverage bug. Check for the crashed test FIRST; this cost a
  cycle. The lead is `notes/bugs-server.md` section 9, now recorded as
  intermittent rather than "not seen since".

## The data document, round 2: the composer's rails

Closed findings 3, 4 and 5 of `notes/bugs-data.md` - the `SegmentSource` /
`SegmentCompose` cluster in `crates/mogwai-data/src/segment.rs`. Findings 6 and
7 remain, unrenumbered.

MACHINERY LATER ROUNDS MAY BUILD ON AND MUST NOT BREAK:

- `MID_CEILING` IS NOW ONE CONSTANT FOR TWO TAPE ORIGINS. It moved from
  `pub(super)` in `generated/consts.rs` to `pub(crate)`, re-exported from
  `generated/mod.rs`, and `segment.rs` clamps against it. The generator and the
  composer are the workspace's two price integrations and they now share their
  ceiling by construction rather than by two literals agreeing. Anything that
  gives a third origin a band takes it from here.
- `SegmentSource::integrate` IS THE ONLY PLACE THE RUNNING LEVEL MOVES, and
  `SegmentSource::advance_clock` the only place the clock does. Both were open
  expressions inline in `next_tick` and both were unbounded; keeping them as
  single funnels is what makes the band and the overflow refusal checkable at
  all. `next_tick` copies the segment's row out BEFORE calling either, because
  both need `&mut self`.
- THE SOURCE HAS EXACTLY ONE TERMINAL CONDITION and it is named:
  `SegmentSource::clock_exhausted`. A `None` from this source is never ordinary
  exhaustion. It is an INHERENT method, not a `TickFault`, because the composer
  has no variant and adding one ripples into `mogwai-server`'s fault rendering -
  filed in `notes/todo.md` together with the missing `seek_to` override, which
  is the residual of finding 4 and becomes a hang the moment anything serves a
  composed river.
- `every_rule_the_conformance_fixture_states_is_enforced_somewhere` in
  `segment.rs` PINS THE FIXTURE'S `rules` ARRAY TO ITS ENFORCEMENT SITES, one
  entry each, and fails on a count mismatch. It exists because finding 4 was
  the "nothing detects a missing fixture" hole one level down: the fixture
  STATED `ret[0] == 0` and neither reader checked it. A new rule in
  `analysis/segment_library_conformance.json` now fails this test until it has
  a validator. Both `validate`s refuse a nonzero `ret[0]`; the reader also
  refuses a `side` outside B/A/N. IT MATCHES BY SEARCH, NOT BY POSITION, so a
  reorder of the JSON is not reported as a lost enforcement, and it pins the
  `units` side alphabet SEPARATELY - that sixth enforcement is stated in
  `units` rather than in `rules`, and leaving it uncounted would have been a
  gate whose stated scope was narrower than its real one.

FACTS A LATER ROUND WOULD OTHERWISE RE-DERIVE WRONG:

- THE REPORT HAD FINDING 3'S CEILING MECHANISM WRONG, and only the bite-check
  showed it. It predicted `Decimal::from_f64_retain` returning `None` above
  about 7.9e28 and the old `let else` silently printing one tick. MEASURED: a
  rising walk never gets there, because `emit_price` divides by `tick_size`
  first and that overflows `Decimal` around 1.98e28, so an unbounded rising
  composer PANICS inside rust_decimal several factors of e earlier. The silent
  one-tick print is real code and unreachable by that route. The floor half was
  exactly as reported - below half a tick every print is exactly zero.
- `dt_ns` POSITIVITY IS NOT A FIXTURE RULE AND MUST NOT BECOME ONE. The report
  listed it as a declared-but-unchecked invariant; the fixture's `rules` array
  does not contain it. Two prints at one nanosecond are ordinary in a swept
  book - `mogwai_lab::stream` groups on exactly that equality - and
  `mogwai_lab::segments` records `ts - last_ts` verbatim, so refusing a zero
  would throw away real sessions. The composed tape is NON-DECREASING, not
  strictly increasing, and the `seam_gap_ns` doc comment that claimed otherwise
  now says so and says why. That comment was the actual defect.
- FINDING 5 WAS UNDERSTATED, NOT OVERSTATED - the one that broke the round-1
  pattern. The report priced the frozen clock at roughly 580 years of sim time.
  `mogwai segments tape --start` takes a RAW `u64` from the operator, so a
  near-max value froze `ts` at `u64::MAX` on the first command and every later
  tick shared it, silently. `checked_add` plus the latch, not an argument.
- NO CONSUMER OUTSIDE `mogwai segments tape` TOUCHES THE COMPOSER. No test
  binary, no script, and nothing in `mogwai-server` constructs a
  `SegmentSource`, which is why changing its tape bytes cost only the version
  bump.
- A `TAPE_PROTOCOL_VERSION` BUMP RE-BLESSES THE STAGE-A MANIFEST, and this is
  the mechanism to expect on every future bump.
  `analysis/stage-a-batch-manifest.json` hashes the constant into
  `plan_sha256`, so a bump moves both the `tape_protocol_version` field and the
  hash, plus the literal in
  `stage_a_batch::tests::committed_manifest_is_self_consistent`, whose comment
  names the identity it was blessed at. Three edits, all mechanical, and the
  test tells you the new hash.

WHAT THE ROUND-2 CLOSE PASS FOUND, and it is the seventh close pass out of
seven to find its defect in a NEW TEST rather than in production code:

- `a_clock_that_cannot_advance_ends_the_tape_instead_of_freezing_it` WAS
  HALF-DECORATIVE, and it is instance 28 of this arc's signature defect. Its
  `start_ns` of `u64::MAX - 1_500_000` against the helper's 1 ms `dt_ns` left
  room for exactly ONE tick, so the vector reaching the duplicate-timestamp
  assertion - THE assertion that names the defect - had one element and the
  comparison was vacuously true. The test did go red under the old
  `saturating_add`, but on the loop's `seen.len() < 10` guard, which is a
  different claim. This is precisely the "read WHICH assertion fired" trap
  `AGENTS.md` states, and a bite-check that only observes redness cannot see
  it. Now `u64::MAX - 4_500_000` leaves room for four ticks, the loop BREAKS at
  a cap instead of asserting on the count so the duplicate check is reached,
  and the re-bite fires on that check with a twelve-element vector whose last
  eight are `u64::MAX`. A test whose bite depends on how much room a fixture
  leaves owes a comment saying so; this one has it.
- THE GENERAL RULE, worth more than the instance: WHEN A TEST BOUNDS A LOOP
  WITH AN ASSERTION AND THEN ASSERTS THE REAL PROPERTY AFTER IT, the bound
  fires first under the bug and the real property is never evaluated. Bound
  with a `break` and assert the property; the count then becomes its own
  separate, weaker claim.
- `a_runaway_positive_drift_stays_on_the_trajectory` asserted only
  `price >= 20_000` on a monotonically rising walk, which almost any
  implementation satisfies. It now asserts the price never EXCEEDS
  `MID_CEILING` and that the walk ends PINNED to it, which is the contract the
  clamp actually states; bitten by widening the clamp's ceiling by 1e6, which
  the old assertion could not see.

FILED RATHER THAN FIXED by that pass: `emit_price`'s panic sits inside
`TickSource::next_tick` in a library crate, where the generator's equivalent
failures go through `TickFault`. Unreachable today and correct for an offline
dump, but it becomes a serving-path abort the moment a composed river is
served - folded into the same `notes/todo.md` item as the missing `seek_to`
and `fault` overrides, since one `TickFault` variant closes all of it.

A SECOND INTERMITTENT LIFECYCLE TEST SURFACED on this round's gate and is now
`notes/bugs-server.md` section 10:
`sigterm_stops_the_venue_within_the_shutdown_grace` failed the first of two
full unscoped gate runs on an IDENTICAL tree and passed the second, with
"venue did not exit within 10s". It is distinct from section 9's completion
lead. Both abort the parallel sweep and both therefore produce the wall of
orphaned `mogwai-data` pairs that reads as a brokkr coverage bug - THERE ARE
NOW TWO KNOWN PRODUCERS OF THAT WALL, and the tell is the orphan count equalling
the missing sweep's pass count.
