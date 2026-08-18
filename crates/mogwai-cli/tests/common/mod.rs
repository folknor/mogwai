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
    sync::Mutex,
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
    /// Boot river resolved from the same config this harness launches. A venue
    /// reports no symbol, so tests name one from their own configuration.
    pub symbol: String,
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

    pub fn ws_url_for(&self, symbol: &str) -> String {
        format!("{}/ws?symbol={symbol}", self.inner.base_url())
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
    let spec = spec(extra_args);
    let symbol = boot_symbol(spec.config.as_deref());
    let inner = launch(spec).expect("the venue launches and reports ready");
    let ready_at = Instant::now();
    Venue {
        record: inner.record().clone(),
        symbol,
        inner,
        ready_at,
    }
}

/// As `spawn`, but the venue's stderr is CAPTURED LINE BY LINE into the returned
/// buffer instead of discarded.
///
/// For the one property no wire surface states. A venue decision that is only
/// visible as a log line - `market order has no market reading` is the case this
/// was added for - cannot be observed through `/ws` or any route, and a test
/// that settles for a nearby wire observable ends up asserting something the
/// defect it names would not break. Reading the venue's own log is the honest
/// alternative, so it is offered rather than hidden.
///
/// Prefer a wire observable wherever one exists: a log line is not a contract,
/// and a test keyed on one is refactor-fragile in a way an assertion on a
/// `ServerMessage` is not. The line this is used for is named in the test.
///
/// THE FILTER IS PINNED, not inherited. `init_stderr_logging` falls back to
/// `mogwai=info` only when `RUST_LOG` is ABSENT, so a developer or a CI job
/// exporting `RUST_LOG=mogwai=error` would silence every line a caller here
/// scores on - and a caller scoring an ABSENCE would then pass vacuously, which
/// is the exact defect class this capture was added to close. `RUST_LOG` is
/// therefore SET on the venue rather than left to the ambient environment, and
/// [`CapturedLog::await_positive_control`] proves the plumbing separately.
pub fn spawn_capturing_stderr(extra_args: &[&str]) -> (Venue, CapturedLog) {
    let log = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&log);
    let mut spec = spec(extra_args);
    spec.env
        .push((OsString::from("RUST_LOG"), OsString::from(CAPTURED_FILTER)));
    // The callback runs on the draining thread, so it stays a push onto a
    // `Vec` - anything blocking here reintroduces the wedge `StderrSink` exists
    // to prevent.
    spec.stderr = StderrSink::Lines(Box::new(move |line| {
        if let Ok(mut lines) = sink.lock() {
            lines.push(line);
        }
    }));
    let symbol = boot_symbol(spec.config.as_deref());
    let inner = launch(spec).expect("the venue launches and reports ready");
    let ready_at = Instant::now();
    (
        Venue {
            record: inner.record().clone(),
            symbol,
            inner,
            ready_at,
        },
        CapturedLog { lines: log },
    )
}

/// The filter pinned on a venue whose log is read. Matches
/// `init_stderr_logging`'s own no-`RUST_LOG` fallback, so a captured venue logs
/// exactly what an uncaptured one does - the pin removes the ambient
/// environment's vote, it does not raise the level.
const CAPTURED_FILTER: &str = "mogwai=info";

/// The venue's stderr, line by line, as [`spawn_capturing_stderr`] captured it.
pub struct CapturedLog {
    lines: std::sync::Arc<Mutex<Vec<String>>>,
}

impl CapturedLog {
    /// A line the venue is KNOWN to emit, so that an absence elsewhere in this
    /// buffer means the venue did not say it rather than that nothing was ever
    /// captured. Emitted after the readiness line goes out on stdout, so it is
    /// polled to a deadline rather than read once.
    ///
    /// Every conclusion drawn from an ABSENCE in this buffer owes a call to
    /// this first. Without it a broken filter, a broken pipe or a dead capture
    /// thread all render as "the venue never warned", which is indistinguishable
    /// from the property holding.
    pub fn await_positive_control(&self) {
        const CONTROL: &str = "mogwai listening";
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.contains(CONTROL) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "the venue's log never carried `{CONTROL}` under RUST_LOG={CAPTURED_FILTER}, so this \
             capture is empty for a reason that has nothing to do with the property under test, \
             and no absence in it means anything. Captured: {:?}",
            self.snapshot()
        );
    }

    /// Whether any captured line contains `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        self.lines
            .lock()
            .expect("the log buffer")
            .iter()
            .any(|line| line.contains(needle))
    }

    /// Whether any captured line contains BOTH needles - the shape a per-order
    /// log query takes, where the message names the decision and a field names
    /// which order it was about.
    pub fn contains_both(&self, first: &str, second: &str) -> bool {
        self.lines
            .lock()
            .expect("the log buffer")
            .iter()
            .any(|line| line.contains(first) && line.contains(second))
    }

    /// Everything captured so far, for a panic message.
    pub fn snapshot(&self) -> Vec<String> {
        self.lines.lock().expect("the log buffer").clone()
    }
}

fn boot_symbol(config_path: Option<&std::path::Path>) -> String {
    use mogwai_server::config;

    let cfg = config::Config::load(config_path.map(PathBuf::from))
        .expect("the harness launches only configs the venue accepts");
    config::build_instrument_profiles(&cfg)
        .expect("the harness launches only configs the venue accepts")
        .boot_symbol_def(cfg.boot_symbol())
        .expect("the boot shape resolves for a config the venue accepts")
        .symbol
        .to_string()
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

pub fn two_symbols_config() -> String {
    format!(
        "{}/tests/configs/two-symbols.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue serving a PERPETUAL whose funding interval is one simulated second,
/// so a gate can cross a funding instant without waiting eight sim-hours.
pub fn perpetual_config() -> String {
    format!(
        "{}/tests/configs/perpetual.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

pub fn mnq_preset_config() -> String {
    format!(
        "{}/tests/configs/mnq-preset.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue that materializes NO warmup, so its tape worker starts with the
/// canonical frontier sitting exactly at the tape origin.
pub fn no_warmup_config() -> String {
    format!(
        "{}/tests/configs/no-warmup.toml",
        env!("CARGO_MANIFEST_DIR")
    )
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

/// One blocking HTTP POST of a JSON body, returning the status code and body.
/// Same reasoning as `http_get`, plus a `Content-Type` and a length.
pub fn http_post_json(base: &str, path: &str, body: &str) -> (u16, String) {
    let authority = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(authority).expect("connect to the venue");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
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
