// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L1 gates: the foreground process, its ephemeral port, its readiness record
//! and the two ways it dies.
//!
//! Every test here spawns a real venue and binds a real loopback port, so all
//! of them are `#[ignore]`d like their siblings in `mogwai-adapter/tests` - an
//! environment without sockets would fail them for reasons unrelated to the
//! code. The full gate and the focused runner both include them.

mod common;

use std::{
    io::{BufRead, BufReader},
    os::unix::process::CommandExt,
    process::{Child, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use common::{fast_config, http_get, spawn, venue_binary};
use mogwai_protocol::launch::{LaunchError, LaunchSpec, launch};

#[test]
#[ignore = "binds two loopback listeners"]
fn the_ready_record_reports_a_seed_that_differs_between_launches() {
    let config = format!(
        "{}/tests/configs/fast-random.toml",
        env!("CARGO_MANIFEST_DIR")
    );
    let first = spawn(&["--config", &config]);
    let second = spawn(&["--config", &config]);
    // A collision is a 2^-63 event, not a venue defect.
    assert_ne!(first.record.run_seed, second.record.run_seed);
}

#[test]
#[ignore = "binds a loopback listener"]
fn the_ready_record_names_the_tape_protocol_version() {
    let venue = spawn(&["--config", &fast_config()]);
    assert!(
        venue
            .record
            .version_string
            .contains(&format!("tape {}", mogwai_data::TAPE_PROTOCOL_VERSION)),
        "{}",
        venue.record.version_string
    );
}

/// The whole point of the ephemeral port: the venue picks one and REPORTS it,
/// so a launcher never has to pick a port and never collides with another run.
#[test]
#[ignore = "binds a loopback listener"]
fn ready_record_reports_the_bound_ephemeral_port() {
    let venue = spawn(&["--config", &fast_config()]);

    assert_ne!(
        venue.record.addr.port(),
        0,
        "the reported port is the BOUND one, not the requested zero"
    );
    assert_eq!(venue.record.addr.ip().to_string(), "127.0.0.1");
    assert!(venue.record.pid > 0, "the record carries a signalable pid");
    assert!(
        !venue.record.version_string.is_empty(),
        "the record names the binary that produced the run"
    );
    assert_eq!(
        venue.record.data_origin_ns,
        venue.record.run_start_ns - venue.record.warmup_ns,
        "data_origin_ns = run_start_ns - warmup_ns, per decision 7"
    );

    // Reported means reachable: the record is written after the listener is
    // bound, so a connect at this instant must succeed.
    let (status, body) = http_get(&venue.http_base(), "/health");
    assert_eq!(status, 200);
    let health: serde_json::Value = serde_json::from_str(&body).expect("health is JSON");
    assert_eq!(health["status"], "ok");
    assert_eq!(health["oms_type"], "netting");
}

/// The defect this whole lifecycle exists to remove: two runs sharing one
/// venue. With an ephemeral port per run they cannot.
#[test]
#[ignore = "binds two loopback listeners"]
fn two_concurrent_venues_bind_distinct_ports() {
    let first = spawn(&["--config", &fast_config()]);
    let second = spawn(&["--config", &fast_config()]);

    assert_ne!(
        first.record.addr.port(),
        second.record.addr.port(),
        "two concurrent runs must not land on one endpoint"
    );
    assert_ne!(first.record.pid, second.record.pid);

    // Both are independently serving, so neither stole the other's socket.
    assert_eq!(http_get(&first.http_base(), "/health").0, 200);
    assert_eq!(http_get(&second.http_base(), "/health").0, 200);
}

/// Failure to write the readiness record is fatal: a venue nobody can reach is
/// worse than no venue, because the launcher would wait on it.
#[test]
#[ignore = "spawns the venue binary"]
fn serve_exits_nonzero_when_the_readiness_line_cannot_be_written() {
    // Close the read end straight away, so the venue's write to stdout fails
    // with EPIPE. Rust ignores SIGPIPE at startup, so this surfaces as an error
    // on the write rather than killing the process by signal - which is the
    // path that has to stay fatal.
    let mut child = Command::new(venue_binary())
        .arg("serve")
        .arg("--config")
        .arg(fast_config())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn venue");
    drop(child.stdout.take().expect("venue stdout is piped"));

    // Inside the test's budget rather than a flat minute: a venue that keeps
    // serving a run nobody can reach must be reported by the panic below, which
    // names that, and not by the per-test watchdog, which names nothing.
    let deadline = common::wall_deadline(Duration::from_secs(10));
    let status = loop {
        match child.try_wait().expect("poll venue") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                child.kill().ok();
                panic!("the venue kept serving a run nobody could reach");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(
        !status.success(),
        "an unwritable readiness stream must be fatal, got {status:?}"
    );
}

/// A signal must stop the venue promptly - the whole ownership model is that a
/// launcher can end a run - and it must not need the shutdown grace to do it
/// when nothing is holding a connection open.
#[test]
#[ignore = "binds a loopback listener"]
fn sigterm_stops_the_venue_within_the_shutdown_grace() {
    let mut venue = spawn(&["--config", &fast_config()]);
    let pid = nix::unistd::Pid::from_raw(
        i32::try_from(venue.record.pid).expect("a pid fits in the signal type"),
    );

    // THE BOUND IS THE GRACE THIS TEST IS NAMED FOR, not an arbitrary round
    // number twice its size. `serve.rs`'s `SHUTDOWN_GRACE` is five seconds - the
    // window a completed or signalled venue gives its live connections to drain
    // before exiting regardless - and this venue holds no connection at all, so
    // the docstring's property is that it does not need that window. Ten seconds
    // asserted something weaker than the sentence above it. Measured at 0.2 s, so
    // five is still twenty-five times the observed cost; if the shutdown path
    // ever grows a drain that an IDLE venue waits on, this is supposed to fail.
    const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);
    let sent = Instant::now();
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).expect("signal the venue");
    // Looser than the assertion below, deliberately: a venue that never exits at
    // all has to be reported by this wait, naming the venue, rather than by the
    // per-test watchdog naming the whole test. Both are inside the test's budget.
    let status = venue.wait_for_exit(Duration::from_secs(10));
    let elapsed = sent.elapsed();

    assert!(
        elapsed < SHUTDOWN_GRACE,
        "SIGTERM took {elapsed:?}; an idle venue must stop without needing its \
         {SHUTDOWN_GRACE:?} shutdown grace"
    );
    assert!(
        status.success || status.code.is_none(),
        "a signalled venue exits cleanly or by the signal, got {status:?}"
    );
}

/// The harness's own wall budget, pinned where it is weakest: a test that spends
/// budget BEFORE the harness has a venue to count.
///
/// `spawn` opens the budget when no venue is live, and `Venue`'s `Drop` clears
/// the anchor when the last one goes - bookkeeping that only ever sees venues
/// the harness launched. `a_faulted_venue_exits_nonzero_and_an_exhausted_one_
/// does_not` drives two through `launch` DIRECTLY and then calls `spawn`, so the
/// counter has never left zero while up to ten seconds of the test's budget is
/// gone. An `open_budget` that re-anchored on a zero count would restart the
/// budget there and put the ceiling PAST `HANG_WATCHDOG`, which is the one
/// failure the whole mechanism exists to prevent: the deadline that should have
/// reported arrives as an unattributed kill instead.
///
/// THE CEILING IS WHAT IS ASSERTED, not the anchor, because the ceiling is what
/// a bound is clamped to. Both bounds ask for the whole budget, so both are the
/// clamp rather than the cap, and a re-anchor between them moves the second one
/// forward by however long the untracked phase took.
#[test]
#[ignore = "binds a loopback listener"]
fn a_venue_launched_after_untracked_work_inherits_that_works_budget() {
    // Anchors the budget without the harness knowing a thing about it - the
    // blocking HTTP helpers do exactly this against a directly-launched venue.
    let before = common::wall_deadline(common::TEST_WALL_BUDGET);
    std::thread::sleep(Duration::from_millis(250));

    let _venue = spawn(&["--config", &fast_config()]);
    let after = common::wall_deadline(common::TEST_WALL_BUDGET);

    assert_eq!(
        before, after,
        "launching a venue moved this test's wall ceiling forward, so the budget restarted \
         mid-test and every bound below it may now outlive the per-test hang watchdog"
    );
}

/// The intermediate launcher's whole PROCESS GROUP, killed on drop.
///
/// A GROUP RATHER THAN A CHILD, because the child is not what leaks. The venue
/// dies with the shell through `PR_SET_PDEATHSIG` and the shell dies when this
/// test kills it - but `sleep 3600` is a SEPARATE process the shell forked, so
/// killing the shell orphans it onto init and it sits there for an hour. That
/// was happening on the SUCCESS path, every green run, and the machine this was
/// written on was carrying five of them from earlier runs when the guard went
/// in. `Child::kill` cannot reach it: the sleep is a grandchild and the test
/// never learns its pid.
///
/// `process_group(0)` therefore makes the shell a group leader, so the sleep and
/// the venue inherit the group and one `killpg` reaches all three. It runs on
/// DROP rather than at the end of the test because the leak's other half is a
/// panic between the spawn and the kill - and an explicit cleanup on the path
/// that does not fail is not a guard at all.
///
/// It does NOT replace the explicit kill below. That one must signal the SHELL
/// ALONE: the property under test is that the kernel reaps the venue when its
/// launcher dies, and a group kill would have killed the venue directly and
/// proven nothing.
///
/// TWO THINGS IT DEPENDS ON, both load-bearing and neither obvious.
///
/// THE PGID SURVIVES THE LEADER BEING REAPED. The test kills and `wait()`s the
/// shell, so by the time this runs the pid it names has been released - and a
/// pgid is only safe from reuse while the group still has a member. `sleep 3600`
/// is that member: it holds the group open, which is why a `killpg` up to ten
/// seconds later reaches the right processes rather than whatever group
/// inherited a recycled pid. CHANGE THE SCRIPT SO NOTHING OUTLIVES THE SHELL AND
/// THIS GUARD BECOMES A SIGKILL AIMED AT A STRANGER. If the sleep ever goes, the
/// group must be signalled before the shell is reaped, not after.
///
/// WHETHER THE SLEEP IS FORKED IS SHELL-DEPENDENT. A shell may exec the last
/// command of `A & B` in place, in which case there is no grandchild and nothing
/// leaks; `/bin/sh` on the machine this was written on does not, and nine orphans
/// had accumulated. The guard is correct either way - it is the fix's NECESSITY
/// that varies by shell, not its safety.
struct ReapedGroup(Child);

impl ReapedGroup {
    fn pgid(&self) -> nix::unistd::Pid {
        nix::unistd::Pid::from_raw(i32::try_from(self.0.id()).expect("a pid fits in the type"))
    }
}

impl Drop for ReapedGroup {
    fn drop(&mut self) {
        // ESRCH once every member is gone, which is the ordinary outcome.
        nix::sys::signal::killpg(self.pgid(), nix::sys::signal::Signal::SIGKILL).ok();
        self.0.kill().ok();
        self.0.wait().ok();
    }
}

/// Reads one line from a child's stdout, bounded by the test's wall budget.
///
/// `BufRead::read_line` IS UNBOUNDED, and on a pipe a child holds open it blocks
/// forever. A venue that never writes its readiness line would therefore be
/// reported by the per-test hang watchdog - which names the whole test, kills
/// the process group and says nothing about what was being waited for - rather
/// than by this test, which knows. The read runs on its own thread because there
/// is no portable way to bound a blocking pipe read in place; that thread ends
/// when the pipe closes, which the drop guard above guarantees.
///
/// THAT RECLAMATION IS AN ORDERING DEPENDENCY, so it is written down. On the
/// panic path the reader thread is still blocked on the pipe, and what closes
/// the pipe is `ReapedGroup`'s drop killing the shell. The guard must therefore
/// already be CONSTRUCTED when this is called - which it is, the `Child` is
/// moved into it at spawn. Reorder those two and a failing test leaks a thread
/// blocked forever on a live child's stdout.
fn read_line_within(stdout: ChildStdout, cap: Duration) -> String {
    let deadline = common::wall_deadline(cap);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        drop(tx.send(reader.read_line(&mut line).map(|_| line)));
    });
    match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        Ok(Ok(line)) => line,
        Ok(Err(err)) => panic!("reading the readiness line from the intermediate launcher: {err}"),
        Err(_) => panic!(
            "the intermediate launcher produced no readiness line within {cap:?} (or the test's \
             remaining budget); the venue it spawned never reported ready"
        ),
    }
}

/// Decision 10, tested the only way it can be: an intermediate process spawns
/// the venue and is then SIGKILLed, so it sends nothing on the way out. The
/// kernel's `PR_SET_PDEATHSIG` is what has to reap the venue.
#[test]
#[ignore = "binds a loopback listener"]
fn venue_dies_when_its_launcher_is_killed_without_cleanup() {
    // The intermediate is a shell that spawns the venue and then blocks
    // forever. SIGKILLing it gives it no chance to clean up, which is exactly
    // the orphaned-venue case a SIGTERM handler alone cannot cover.
    // The venue inherits the shell's stdout, which is our pipe, so the readiness
    // line reaches us with no fd plumbing in the script at all.
    let script = format!(
        "\"{}\" serve --config \"{}\" 2>/dev/null & sleep 3600",
        venue_binary(),
        fast_config()
    );
    let mut launcher = ReapedGroup(
        Command::new("/bin/sh")
            .arg("-c")
            .arg(script)
            // Its own process group, so the guard can reach the `sleep` the
            // shell forks. See `ReapedGroup`.
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the intermediate launcher"),
    );

    let stdout = launcher.0.stdout.take().expect("launcher stdout");
    let line = read_line_within(stdout, Duration::from_secs(10));
    let record = common::parse_ready(&line);
    let venue_pid =
        nix::unistd::Pid::from_raw(i32::try_from(record.pid).expect("a pid fits in the type"));

    // The venue is alive and serving right now.
    assert_eq!(
        http_get(&format!("http://{}", record.addr), "/health").0,
        200
    );

    launcher
        .0
        .kill()
        .expect("SIGKILL the intermediate launcher");
    launcher.0.wait().expect("reap the intermediate launcher");

    // The venue is not our child, so we cannot wait on it; poll for the
    // process to disappear instead.
    //
    // On `teardown_deadline` rather than `wall_deadline`: this is the wait that
    // runs LAST and its failure message is a claim about the VENUE, so it is
    // entitled to the reserve the budget holds back for exactly that. Twenty
    // seconds was never a bound at all - it sits past the per-test watchdog, so
    // a venue that outlived its launcher arrived as an unattributed kill.
    let deadline = common::teardown_deadline(Duration::from_secs(10));
    loop {
        let alive = nix::sys::signal::kill(venue_pid, None).is_ok();
        if !alive {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the venue outlived the launcher that owned it: pid {venue_pid} is still alive"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Starting a venue takes NO endpoint flags at all: no port to pick and no fd to
/// nominate. The endpoint is the kernel's choice and it comes back on stdout, so
/// this pins both halves of what a launcher may assume - loopback, and a port
/// nobody chose.
#[test]
#[ignore = "binds a loopback listener"]
fn serve_needs_no_endpoint_flags_and_reports_where_it_landed() {
    let venue = spawn(&["--config", &fast_config()]);
    assert!(
        venue.record.addr.ip().is_loopback(),
        "{}",
        venue.record.addr
    );
    assert_ne!(
        venue.record.addr.port(),
        0,
        "the reported port must be the one the kernel actually allocated"
    );
}

/// Stdout closing with no line is how a launcher learns the venue failed to
/// boot, per step 2 of the contract. A config naming an unfunded quote currency
/// is a boot refusal, so it exercises exactly that path - and the shipped
/// launcher must both classify it and carry the venue's own reason, since a
/// launcher that only says "no record" sends the operator to the wrong repo.
#[test]
#[ignore = "spawns the venue binary"]
fn a_boot_failure_reports_no_record_and_says_why() {
    let config = format!("{}/tests/configs/unfunded.toml", env!("CARGO_MANIFEST_DIR"));
    let error = launch(common::spec(&["--config", &config]))
        .expect_err("a venue that refuses to boot cannot report ready");

    let LaunchError::NoRecord { stderr } = &error else {
        panic!("expected a missing-record boot failure, got {error:?}");
    };
    let log = stderr.join("\n");
    assert!(
        log.contains("balances") || log.to_lowercase().contains("fund"),
        "the boot failure must carry the venue's reason, got: {log}"
    );
}

/// The ready read blocks for as long as warmup generation takes, so a launcher
/// that does not bound it hangs forever on a venue that will never answer. The
/// shipped launcher bounds it and names the knob.
#[test]
#[ignore = "spawns the venue binary"]
fn a_ready_timeout_expires_rather_than_hanging() {
    let error = launch(LaunchSpec {
        // Far below the ~1.5 s a default warmup needs, so the bound trips on a
        // venue that is booting perfectly well.
        ready_timeout: Some(Duration::from_millis(1)),
        ..common::spec(&["--config", &fast_config()])
    })
    .expect_err("a one-millisecond bound cannot be met");
    assert!(matches!(error, LaunchError::Timeout { .. }), "{error:?}");
    assert!(error.to_string().contains("ready_timeout"), "{error}");
}
