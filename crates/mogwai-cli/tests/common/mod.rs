// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Launcher-side harness for the venue lifecycle gates.
//!
//! Every test in `tests/` drives the venue through `mogwai_protocol::launch`,
//! the SHIPPED launcher - the same code path a consumer uses, rather than a
//! second hand-rolled implementation of the same contract.
//!
//! That is deliberate and is the reason the launcher lives in `mogwai-protocol`
//! rather than in `mogwai-adapter`: the venue's own gates cannot depend on the
//! adapter (it would pull nautilus into the server's test graph, and the adapter
//! is the client of the thing under test), so a launcher shipped from there
//! would leave mogwai re-deriving the contract by hand forever. Here, a change
//! to the handshake breaks these tests immediately instead of breaking a
//! consumer later.

// A shared test-support module: not every item is used by every test binary,
// and nothing here is reachable outside the crate.
#![allow(dead_code, unreachable_pub)]

use std::{
    ffi::OsString,
    io::{Read, Write},
    net::TcpStream,
    path::PathBuf,
    time::{Duration, Instant},
};

use mogwai_protocol::{
    ReadyRecord,
    launch::{LaunchSpec, LaunchedVenue, StderrSink, VenueExit, launch},
};

/// A launched venue plus the record it reported. The inner guard kills and reaps
/// on drop, so a failing assertion cannot leak a listening process into the rest
/// of the suite.
pub struct Venue {
    inner: LaunchedVenue,
    pub record: ReadyRecord,
    /// Wall instant the readiness line was read. The acceleration gate measures
    /// the served run from here, because that is when the launcher could first
    /// connect.
    pub ready_at: Instant,
}

impl Venue {
    pub fn http_base(&self) -> String {
        self.inner.http_base()
    }

    pub fn ws_url(&self) -> String {
        format!("{}/ws", self.inner.base_url())
    }

    /// Waits for exit, failing the test if the venue outlives `timeout`.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> VenueExit {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(exit) = self.inner.exited() {
                return exit;
            }
            assert!(
                Instant::now() < deadline,
                "venue did not exit within {timeout:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// True once the process has exited, without blocking.
    pub fn has_exited(&mut self) -> bool {
        self.inner.exited().is_some()
    }
}

/// Path to the venue binary this test binary was built alongside.
pub fn venue_binary() -> &'static str {
    env!("CARGO_BIN_EXE_mogwai")
}

/// A spec pointing at the binary this test binary was built alongside, with the
/// log captured so a boot failure can report why.
pub fn spec(extra_args: &[&str]) -> LaunchSpec {
    let mut spec = LaunchSpec {
        binary: Some(OsString::from(venue_binary())),
        stderr: StderrSink::Discard,
        ..LaunchSpec::default()
    };
    // The gates state their arguments the way the CLI does, and this maps them
    // onto the launcher's typed fields - which is also a small check that the
    // two agree about what `serve` accepts.
    let mut args = extra_args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} takes a value"));
        match *flag {
            "--config" => spec.config = Some(PathBuf::from(*value)),
            "--duration" => {
                spec.duration = Some(
                    humantime::parse_duration(value)
                        .unwrap_or_else(|err| panic!("{value} is not a duration: {err}")),
                );
            }
            other => panic!("the harness does not model {other}"),
        }
    }
    spec
}

/// The launcher contract, executed through the shipped launcher. Panics with the
/// boot failure - including the venue's own stderr - if it does not come up.
pub fn spawn(extra_args: &[&str]) -> Venue {
    let inner = launch(spec(extra_args)).expect("the venue launches and reports ready");
    let ready_at = Instant::now();
    Venue {
        record: inner.record().clone(),
        inner,
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
