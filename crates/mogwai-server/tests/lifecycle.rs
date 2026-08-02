// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L1 gates: the foreground process, its ephemeral port, its readiness record
//! and the two ways it dies.
//!
//! Every test here spawns a real venue and binds a real loopback port, so all
//! of them are `#[ignore]`d like their siblings in `mogwai-adapter/tests` - an
//! environment without sockets would fail them for reasons unrelated to the
//! code. `brokkr check --gate` and the focused runner both include them.

mod common;

use std::{
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

use common::{fast_config, http_get, spawn, spawn_raw, venue_binary};

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
    assert_eq!(body, "ok");
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
fn serve_exits_nonzero_when_the_ready_fd_is_unwritable() {
    // fd 3 is never opened in the child, so the write fails with EBADF.
    let mut child = Command::new(venue_binary())
        .arg("serve")
        .arg("--config")
        .arg(fast_config())
        .arg("--ready-fd")
        .arg("3")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn venue");

    let deadline = Instant::now() + Duration::from_secs(60);
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
        "an unwritable ready fd must be fatal, got {status:?}"
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

    let sent = Instant::now();
    nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGTERM).expect("signal the venue");
    let status = venue.wait_for_exit(Duration::from_secs(20));
    let elapsed = sent.elapsed();

    assert!(
        elapsed < Duration::from_secs(10),
        "SIGTERM took {elapsed:?}; the venue must stop within its shutdown grace"
    );
    assert!(
        status.success() || status.code().is_none(),
        "a signalled venue exits cleanly or by the signal, got {status:?}"
    );
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
    let script = format!(
        "exec 3>&1; \"{}\" serve --config \"{}\" --ready-fd 3 2>/dev/null & sleep 3600",
        venue_binary(),
        fast_config()
    );
    let mut launcher = Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the intermediate launcher");

    let stdout = launcher.stdout.take().expect("launcher stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("read readiness line");
    let record = common::parse_ready(&line);
    let venue_pid =
        nix::unistd::Pid::from_raw(i32::try_from(record.pid).expect("a pid fits in the type"));

    // The venue is alive and serving right now.
    assert_eq!(
        http_get(&format!("http://{}", record.addr), "/health").0,
        200
    );

    launcher.kill().expect("SIGKILL the intermediate launcher");
    launcher.wait().expect("reap the intermediate launcher");

    // The venue is not our child, so we cannot wait on it; poll for the
    // process to disappear instead.
    let deadline = Instant::now() + Duration::from_secs(20);
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

/// Decision 9's other half, over a real process rather than the unit check: a
/// port reported nowhere serves nobody, so `serve` refuses to start.
#[test]
#[ignore = "spawns the venue binary"]
fn a_bare_serve_refuses_to_start() {
    let output = Command::new(venue_binary())
        .arg("serve")
        .output()
        .expect("run the venue");
    assert!(!output.status.success(), "a bare serve must refuse");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--ready-fd"), "{stderr}");
}

/// The pipe closing with no line is how a launcher learns the venue failed to
/// boot, per step 2 of the contract. A config naming an unfunded quote currency
/// is a boot refusal, so it exercises exactly that path.
#[test]
#[ignore = "spawns the venue binary"]
fn a_boot_failure_closes_the_ready_pipe_without_a_line() {
    let config = format!("{}/tests/configs/unfunded.toml", env!("CARGO_MANIFEST_DIR"));
    let (mut child, read_fd) = spawn_raw(&["--config", &config]);
    let mut reader = BufReader::new(std::fs::File::from(read_fd));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read the ready pipe");
    assert!(
        line.is_empty(),
        "a venue that refused to boot must not report readiness: {line}"
    );
    let status = child.wait().expect("reap the venue");
    assert!(!status.success(), "a refused boot exits nonzero");
}
