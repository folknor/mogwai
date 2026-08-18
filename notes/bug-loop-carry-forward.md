# Bug-loop carry-forward

State the bug-hunt orchestration loop's agents cannot see. No agent in the loop
observes any round but its own, so every brief carries the relevant slice of
this forward. Not a history: when an entry stops binding future work, delete it.

Arc in progress: the eleven `notes/bugs-*.md` reports, worked one document at a
time in this order. `bugs-tests-lifecycle` CLOSED on 2026-08-19 after five
rounds, with no open findings; `bugs-tests-adapter` is next. The order is
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
    thread-local it lives in: `spawn` re-anchors when no venue is live,
    `Venue::drop` clears the anchor when the last one goes. See the libtest fact
    below for why that is not redundant. A test in these files must not drop a
    venue and then carry on - a mid-test re-anchor pushes the ceiling PAST the
    watchdog, which is the failure this whole mechanism exists to prevent.
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
  the invocation. Baseline at the end of round 5: 1181 workspace + 442
  instrumented, in 1m02s, with 63 ignored, 17 skips and 0 orphaned pairs. It was
  1179 / 65 / 19 / 1m05s at the end of round 4, and the difference is the two
  completion gates round 5 un-parked, which now RUN rather than being skipped.
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
- Any ADJUDICATOR launched in this arc reads `notes/todo.md` and `brokkr.toml`
  in addition to its fork framing and the usual contracts. Owner instruction.
  `brokkr.toml` carries the gate profile's parallelism and skip list, which
  several findings in these reports turn on directly.
