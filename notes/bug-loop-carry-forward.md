# Bug-loop carry-forward

State the bug-hunt orchestration loop's agents cannot see. No agent in the loop
observes any round but its own, so every brief carries the relevant slice of
this forward. Not a history: when an entry stops binding future work, delete it.

Arc in progress: the eleven `notes/bugs-*.md` reports, worked one document at a
time in this order. `bugs-tests-lifecycle` CLOSED on 2026-08-19 after five
rounds plus a close pass over the whole commit arc, with no open findings;
`bugs-tests-adapter` is next. The order is
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
    with `HavocLatency` ZEROED, satisfied entirely by stub time. Deliberately
    independent of the 100 ms pre-push sleep, so whatever a later round does to
    that sleep cannot revive the defect. It is also where a "nothing leaked
    early" window should END: `assert_only_instrument_prologue` with a fixed
    duration starts its window at the return of `connect()` and races the
    harness's own delays, so a slow enough connect fails the test for nothing.
    Poll until the stamp appears instead - it is set strictly before the send.
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
- `notes/bugs-tests-adapter.md`'s "Binary-level timing" section says `havoc` has
  17 tests. It has 19 after this round - ONE ADDED, none deleted; the deletion
  was in `reconciliation`, which went 15 to 14. It also says `havoc`'s floor is
  about 2 s, and the new dead-socket test can spend up to 3 s in
  `wait_for_at_least` before it gives up, so that floor is now the more
  misleading of the two numbers. The section is descriptive prose owned by round
  5; the counts were left as written rather than silently edited under a later
  round's feet.
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
    THE OTHER 37 s IS UNACCOUNTED FOR by anything either round has read, and is
    the thing to measure before another structural-win claim is made about this
    crate. A per-test distribution first; nobody has one.
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
- `notes/bugs-tests-adapter.md`'s "Binary-level timing" section is now WRONG in
  four places and carries an in-place warning saying so, because round 5 owns it
  and rounds 2 and 3 would otherwise be editing under its feet. Stale: the test
  count (19, not 17), the 100 ms pre-push sleep it costs out (gone since round 2,
  replaced by `PushGate`), the trigger test's 1.5-2 s (measured 0.94 s), and the
  2 s floor (the round-1 dead-socket test can spend 3 s). Round 5 re-measures
  rather than editing the numbers in place.
- A LEAD ON THE UNEXPLAINED ~37 s, not chased: the account-snapshot wait above
  puts at least one full inbound-latency window inside EVERY exec client's
  connect - the 30 ms baseline at minimum, the armed exec delay where one is
  armed - and `adapter_smoke`, `reconciliation` and much of `havoc` build one per
  test. Nobody has a per-test distribution yet; libtest's `--report-time` is
  nightly-only, so getting one needs another route - a `Drop` timer on a
  per-test guard, or wrapping each `#[tokio::test]` body, or simply running the
  binaries under `--test-threads=1` and reading the tool's own per-test walls,
  which `brokkr test -p mogwai-adapter <NAME>` already prints one test at a time.
- THE ROUND'S OWN COLD REVIEW FOUND NO DEFECT IN ITS CODE, the first time in
  twelve rounds, and independently re-derived all four mechanisms against
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
  the invocation. Baseline after the close pass: 1183 workspace + 442
  instrumented, in 1m06s, with 63 ignored, 17 skips and 0 orphaned pairs. It was
  1181 at the end of round 5 and 1179 / 65 / 19 at the end of round 4: the
  round-5 difference is the two completion gates it un-parked, which now RUN
  rather than being skipped, and the close pass's is its two new harness pins.
  Serial socket suite green in 6.5s.
- THE GATE'S `skip` LIST NO LONGER CARRIES A PARKED TEST, and `notes/todo.md`'s
  parked list is empty. What remains in `skip` is cost and environment, which is
  what that list is for. `test_threads` STAYS AT 8 even so: the cliff at 16 was
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
  predates it. It is not to be swept into a commit.

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
  WERE IN THE UNREVIEWED HALF. Budget a close pass per arc, and point it there
  first.
- Any ADJUDICATOR launched in this arc reads `notes/todo.md` and `brokkr.toml`
  in addition to its fork framing and the usual contracts. Owner instruction.
  `brokkr.toml` carries the gate profile's parallelism and skip list, which
  several findings in these reports turn on directly.
