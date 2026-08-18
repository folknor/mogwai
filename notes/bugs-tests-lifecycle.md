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

5. **`a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths`**
   (serving.rs:1681) - the PRICELESS ARM IS LIKELY GREEN BY CONSTRUCTION. The
   reading-detector is `fill.last_px < last * 2`, which works for the priced arm
   because the no-reading fallback stamps the absurd `9000000`. In the priceless
   arm there IS no stated price, so a fill decided off no reading at all still
   lands near the tape and satisfies the same test. The arm this test's docstring
   says it exists for ("the price-less one that used to return early with a
   stamped price and no reading at all") is the arm whose discriminator does not
   discriminate. Needs a different signal - the `read_market` WARN on stderr, or
   the reading instant on `OrderFilled`.

6. **`an_armed_divergence_reaches_every_connection`** (serving.rs:1510) -
   `armed_at` is read from the UNNAMED `/clock`, which
   `clock_answers_per_boat_when_a_symbol_is_named` establishes is the venue-clock
   fallback (`boat_clock: false`), and
   `history_is_bounded_by_the_rivers_own_boat...` establishes runs AHEAD of any
   boat. So the ceiling `ts <= armed_at` is systematically generous against the
   boot boat's own frames: post-arm boat data can still be stamped below
   `armed_at` and pass. Compare against `/clock?symbol=<boot>` instead.

7. **`presets_cli::every_listed_preset_is_fetchable_by_name`** - never checks the
   listing command's exit status, and the whole body is
   `for name in listing.lines()`. An empty or failing listing makes this test
   vacuously green, which is precisely the "the listing went stale" defect class
   the file was written for.

8. **`an_account_funded_in_the_wrong_currency_is_refused_at_bind`**
   (serving.rs:1402) - `rendered.contains("400") || rendered.contains("HTTP")`.
   `"HTTP"` matches essentially any tungstenite error including a connection
   refusal or a 500. The test is close to unfalsifiable; the reason the finding
   names ("naming the currency") is never asserted at all despite the docstring.
   Assert `Error::Http(r)` with `r.status() == 400` and the currency in the body.

9. **`a_generator_arm_on_an_unboated_river_is_accepted`** (serving.rs:2202) -
   `body.contains("1000")` to check the armed span. `1000` is a substring of any
   nanosecond timestamp in that ack. Parse the JSON.

10. **`a_second_speed_on_the_same_account_is_refused`** (serving.rs:1310) -
    `contains("400") || contains("already seated")` admits any 400 for any
    reason, including the illegal-symbol path.

## C. Wall-clock budgets

11. **`tape_lateness_under_acceleration`** asserts p99 at most 50 ms. Already
    excluded from the gate lane with a long and honest comment, but the comment
    itself records a RELEASE failure at 311 ms with load average 1.46. This is
    not a code gate, it is a host-capability gate with no admission test - it
    belongs in `brokkr mogwai` as a benchmark row, not in the test suite.
    Structural recommendation: move it.

12. **`lifecycle::sigterm_stops_the_venue_within_the_shutdown_grace`** asserts
    `elapsed < 10s`. The only wall budget in lifecycle.rs; generous, but it is a
    budget and it will be the first thing that goes if the shutdown path ever
    grows a drain.

13. **`a_slow_connection_is_dropped_with_feed_lagged`** (serving.rs:569) - its own
    comment says "keeping the stall for the whole loop made this gate pass alone
    and fail under load", so it has already been patched once for this. It still
    has a FIXED 2 s INNER TIMEOUT INSIDE A 15 s OUTER BUDGET: if the venue stalls
    over 2 s under 8-way load before emitting the lag report, the loop `break`s
    and `lagged.expect(...)` panics with "the venue names the frames it lost
    rather than serving a hole" - again a wrong answer. The inner timeout should
    be the outer deadline.

14. **`websocket_command_work_is_bounded_without_an_act_delay`**
    (serving.rs:2030) - with `pending_command_acts = 1` and no act delay, whether
    50 submits ever overflow the queue depends on send rate beating drain rate.
    It is a race with no controlling condition. It happens to get MORE reliable
    under load, which is the worst kind of reliability.

## D. Harness

15. **`common::spec`'s doc comment lies**: "with the log captured so a boot
    failure can report why" - it sets `StderrSink::Discard`.
    `LaunchError::NoRecord` carries stderr independently (which is why
    `a_boot_failure_reports_no_record_and_says_why` works), but any venue that
    dies AFTER readiness - which is most of the failure modes above - discards
    its diagnostics. Every one of these tests would be easier to triage with
    `StderrSink::Lines` into an `Arc<Mutex<Vec<String>>>` printed on panic.
    Recommend making that the harness default.

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

17. **`lifecycle::venue_dies_when_its_launcher_is_killed_without_cleanup`** -
    `reader.read_line()` is UNBOUNDED; if the venue never writes, this hangs
    until cargo's watchdog. And if anything panics between `spawn` and
    `launcher.kill()`, a `sleep 3600` shell leaks for an hour (the venue itself
    dies via PDEATHSIG, the shell does not). Wrap the `Child` in a drop guard.

18. **`completion::a_faulted_venue_exits_nonzero_and_an_exhausted_one_does_not`**
    - the first `diagnostics`/`diagnostics_for_sink` pair (lines 111-121) is
    built, moved into the sink, then shadowed at line 133 and never read. Dead.
    Its comment ("The faulted configuration is installed after this assertion
    below, avoiding a startup race") describes a structure that is not there -
    the first launch is plain `fast_config`. Delete the first pair and the
    comment.

19. **`characterize_cli::scratch`** does `remove_dir_all` on a
    `CARGO_TARGET_TMPDIR`-relative path keyed by a hand-written name. Currently
    all three names are unique, so it is safe, but nothing enforces it and the
    failure mode under `--test-threads=8` would be one test deleting another's
    output mid-run. A `concat!(module_path!(), line!())` key or a `tempfile`
    would remove the hazard.

20. `unconfigured_symbol.rs`'s two tests hard-code `FOOBAR` / `BARFOO`;
    `serving::a_symbol_no_preset_covers_is_served_under_the_default_bundle` also
    uses `FOOBAR` via `unmatched-symbol.toml`. Different processes, so no
    collision today - noted only because it is the kind of shared literal that
    bites when someone consolidates binaries.

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

- `arrival_control_exposure.rs` writes into
  `<repo>/target/arrival-control-exposure` - a fixed shared path. It is
  `#[ignore]`d and run explicitly, so no collision today, but two concurrent
  invocations would corrupt each other silently.
- `brokkr.toml`'s skip list uses the bare prefix `"parity3b"` and `"parity12a_"`.
  The file's own invariant comment warns that a prefix is "only safe while every
  test that shares it stays ignored, which is a property of files this file
  cannot see" - and `parity3b.rs` currently has tests whose names begin
  `parity3b`, so any future non-ignored `parity3b_*` test silently stops running.
  The invariant is documented but not enforced; nothing detects a violation
  except the coverage audit, and only for the ignored/non-ignored mismatch.
