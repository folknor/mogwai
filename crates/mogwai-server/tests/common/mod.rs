// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Launcher-side harness for the venue lifecycle gates.
//!
//! Every test in `tests/` drives the venue the way the launcher contract in
//! `reference/cli.md` says a launcher must: create a pipe, spawn `mogwai serve
//! --ready-fd 3` as a DIRECT child with the write end at fd 3, read one line,
//! parse the `ReadyRecord`, and use its `addr`. Doing it any other way here
//! would test something other than the contract - in particular, spawning
//! through a wrapper would wire the parent-death watch to the wrapper and
//! quietly stop covering decision 10.

// A shared test-support module: not every item is used by every test binary,
// and nothing here is reachable outside the crate.
#![allow(dead_code, unreachable_pub)]

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::fd::{AsRawFd, OwnedFd},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use mogwai_protocol::ReadyRecord;

/// The fd number the launcher contract nominates for the readiness record.
pub const READY_FD: i32 = 3;

/// A venue process plus the record it reported. Killed on drop, so a failing
/// assertion cannot leak a listening process into the rest of the suite.
pub struct Venue {
    pub child: Child,
    pub record: ReadyRecord,
    /// Wall instant the readiness line was read. The acceleration gate measures
    /// the served run from here, because that is when the launcher could first
    /// connect.
    pub ready_at: Instant,
}

impl Venue {
    pub fn http_base(&self) -> String {
        format!("http://{}", self.record.addr)
    }

    pub fn ws_url(&self) -> String {
        format!("ws://{}/ws", self.record.addr)
    }

    /// Waits for exit, failing the test if the venue outlives `timeout`.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            match self.child.try_wait().expect("poll venue") {
                Some(status) => return status,
                None if Instant::now() >= deadline => {
                    self.child.kill().ok();
                    panic!("venue did not exit within {timeout:?}");
                }
                None => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// True once the process has exited, without blocking.
    pub fn has_exited(&mut self) -> bool {
        self.child.try_wait().expect("poll venue").is_some()
    }
}

impl Drop for Venue {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

/// Path to the venue binary this test binary was built alongside.
pub fn venue_binary() -> &'static str {
    env!("CARGO_BIN_EXE_mogwai")
}

/// Spawns a venue with a readiness pipe wired to [`READY_FD`] and returns the
/// child together with the read end. Split out of [`spawn`] so a test that
/// wants to observe a boot FAILURE can read the closed pipe itself.
pub fn spawn_raw(extra_args: &[&str]) -> (Child, OwnedFd) {
    let (read_fd, write_fd) = nix::unistd::pipe().expect("readiness pipe");
    let raw_write = write_fd.as_raw_fd();
    let mut command = Command::new(venue_binary());
    command
        .arg("serve")
        .arg("--ready-fd")
        .arg(READY_FD.to_string())
        .args(extra_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // dup2 rather than inherit-by-number: the write end lands on fd 3 in the
    // child regardless of which fd the pipe happened to get here, and dup2
    // clears CLOEXEC on the duplicate so it survives exec.
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut command, move || {
            if nix::libc::dup2(raw_write, READY_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn().expect("spawn venue");
    drop(write_fd);
    (child, read_fd)
}

/// The four-step launcher contract, executed. Panics with the boot failure if
/// the pipe closes without a line.
pub fn spawn(extra_args: &[&str]) -> Venue {
    let (child, read_fd) = spawn_raw(extra_args);
    let mut reader = BufReader::new(std::fs::File::from(read_fd));
    let mut line = String::new();
    reader.read_line(&mut line).expect("read readiness pipe");
    let ready_at = Instant::now();
    Venue {
        child,
        record: parse_ready(&line),
        ready_at,
    }
}

/// Step 3 of the contract: check `version` FIRST, refuse a record this
/// launcher does not understand, and only then read the rest.
pub fn parse_ready(line: &str) -> ReadyRecord {
    assert!(
        !line.is_empty(),
        "venue closed the ready pipe without a line"
    );
    let probe: serde_json::Value =
        serde_json::from_str(line).expect("the readiness record is one line of JSON");
    assert_eq!(
        probe.get("version").and_then(serde_json::Value::as_u64),
        Some(u64::from(ReadyRecord::VERSION)),
        "unsupported readiness record version"
    );
    serde_json::from_str(line).expect("the readiness record parses as a ReadyRecord")
}

/// A venue with a small warmup, so a lifecycle gate is not paying for a day of
/// tape it never reads.
pub fn fast_config() -> String {
    format!("{}/tests/configs/fast.toml", env!("CARGO_MANIFEST_DIR"))
}

/// A venue whose tape is paced, so a suppression window is observable as
/// silence rather than drowned in an unpaced backlog.
pub fn paced_config() -> String {
    format!("{}/tests/configs/paced.toml", env!("CARGO_MANIFEST_DIR"))
}

/// A venue whose fanout ring is small enough that the lag policy is reachable
/// without driving hours of tape through a stalled socket.
pub fn tiny_fanout_config() -> String {
    format!(
        "{}/tests/configs/tiny-fanout.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue whose resting limits use the volatility-scaled fill band.
pub fn band_config() -> String {
    format!("{}/tests/configs/band.toml", env!("CARGO_MANIFEST_DIR"))
}

/// A venue with a large warmup and an accelerated clock, for the slow-start gate.
pub fn accelerated_config() -> String {
    format!(
        "{}/tests/configs/accelerated.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// One blocking HTTP GET, returning the status code and the body. Hand-rolled
/// rather than pulling an HTTP client into the dev-dependencies: the venue's
/// request surface is four routes and a status line.
pub fn http_get(base: &str, path: &str) -> (u16, String) {
    let authority = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(authority).expect("connect to the venue");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("send request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}
