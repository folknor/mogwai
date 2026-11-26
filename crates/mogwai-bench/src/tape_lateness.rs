// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! How late a paced tape arrives at a consumer that keeps up, under acceleration.
//!
//! This was a test and should not have been. It asserted a 50 ms p99 and was
//! excluded from every lane that could have run it, because the budget is a
//! statement about the host rather than about this code: a release build failed
//! at 311 ms p99 under a load average of 1.46 across 32 visible CPUs, and no
//! admission test distinguishes a machine that can judge 50 ms from one that
//! cannot. An excluded gate measures nothing; a recorded number measures the
//! machine and the commit it ran on, and a series of them is what makes a
//! regression visible without pretending a threshold is portable.
//!
//! It measures the shipped venue through the shipped launcher, not an in-process
//! venue, because pacing is a property of the serving path as it is deployed -
//! process boundary, socket and all.
//!
//! Argv: `[config] [sample_seconds]`. The default config is the accelerated
//! fixture the retired test used, so the readings remain comparable across the
//! move.
//!
//! Reports on stderr as scraped `key=value` counters: `frames`,
//! `non_trade_text`, `control_frames`, `ending`, `sample_ms`,
//! `p50_lateness_ns`, `p99_lateness_ns`,
//! `max_lateness_ns`. `frames` is the work size, and it is reported for the
//! usual reason - a wall alone cannot tell "faster" from "did less". `ending` is
//! reported for the same reason one step further out: a frame count is only
//! comparable against another if both loops ended the same way, so the loop
//! records whether it ran the whole sample or stopped early and why.

use std::{ffi::OsString, path::PathBuf, time::Duration};

use futures_util::StreamExt;
use mogwai_protocol::{
    VenueMessage,
    launch::{LaunchSpec, StderrSink, launch},
};
use tokio_tungstenite::tungstenite::Message;

/// Names the venue binary to measure, overriding the sibling lookup. A bench
/// runner that executes a copy of this binary from somewhere other than the
/// cargo target directory sets it to the venue built from the measured tree.
const VENUE_BINARY_ENV: &str = "MOGWAI_VENUE_BINARY";

/// The venue binary: `MOGWAI_VENUE_BINARY` when set, else the `mogwai` beside
/// this executable in the cargo target directory. Resolved rather than assumed
/// on `PATH`: a measurement of "what ships" must be of the binary built from
/// this tree, not of whatever a shell happens to find. A relative override is
/// made absolute here, because the launcher would otherwise search `PATH` for
/// a bare name while the check below looked in the working directory.
fn venue_binary() -> OsString {
    let candidate = match std::env::var_os(VENUE_BINARY_ENV) {
        Some(raw) => {
            let path = PathBuf::from(raw);
            if path.is_absolute() {
                path
            } else {
                std::env::current_dir()
                    .expect("the working directory resolves")
                    .join(path)
            }
        }
        None => {
            let exe = std::env::current_exe().expect("this binary has a path");
            exe.parent()
                .expect("<target>/<profile>/mogwai-bench")
                .join(mogwai_protocol::launch::DEFAULT_BINARY)
        }
    };
    assert!(
        candidate.is_file(),
        "the venue binary is not at {}; build the workspace binaries first or set {VENUE_BINARY_ENV}",
        candidate.display()
    );
    candidate.into_os_string()
}

/// The accelerated fixture, in `mogwai-cli`'s test configs. Anchored at compile
/// time on this crate's manifest directory, so the string is the same
/// `crates/mogwai-cli/tests/configs/accelerated.toml` path the harness printed
/// when it lived in `mogwai-cli`.
fn default_config() -> String {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the bench crate sits under crates/")
        .join("mogwai-cli/tests/configs/accelerated.toml")
        .display()
        .to_string()
}

pub(crate) fn run(config: Option<String>, sample_seconds: f64) {
    // The multi-thread runtime with every driver enabled, which is what
    // `#[tokio::main]` expanded to when this was an example's `main`.
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("the tokio runtime builds")
        .block_on(measure(config, sample_seconds));
}

async fn measure(config: Option<String>, sample_seconds: f64) {
    // An empty first argument means "the default config", so a caller who only
    // wants to change the sample length does not have to restate the fixture
    // path to reach the second positional.
    let config = config
        .filter(|arg| !arg.is_empty())
        .unwrap_or_else(default_config);
    let sample = Duration::from_secs_f64(sample_seconds);

    let venue = launch(LaunchSpec {
        binary: Some(venue_binary()),
        config: Some(config.clone().into()),
        // Discarded rather than inherited: the venue's own log lines would
        // interleave with the counters below, which are scraped off this stream.
        stderr: StderrSink::Discard,
        ..LaunchSpec::default()
    })
    .expect("the venue launches and reports ready");

    let http_base = venue.http_base();
    let clock: mogwai_protocol::VenueClock = {
        let body = http_get(&http_base, "/clock");
        serde_json::from_str(&body).expect("the venue clock parses")
    };

    let (mut socket, _) = tokio_tungstenite::connect_async(format!("{}/ws", venue.base_url()))
        .await
        .expect("attach to the boot river");

    let deadline = tokio::time::Instant::now() + sample;
    let mut lateness = Vec::new();
    // Split, because the two populations mean different things. A text frame
    // that is not a trade was always counted and skipped; a non-text frame - a
    // Ping, a Pong, a Binary - used to end the loop, silently truncating the
    // sample. Reporting them apart is what makes that visible if it recurs.
    let mut non_trade_text = 0u64;
    let mut control_frames = 0u64;
    // How the loop ended is part of the measurement. Only `sample_complete`
    // means the sample covers `sample_ms`; every other ending means `frames` is
    // a prefix, and two runs with different frame counts are not comparable
    // unless the reason is on the record beside them.
    let ending = loop {
        match tokio::time::timeout_at(deadline, socket.next()).await {
            Err(_) => break "sample_complete",
            Ok(None) => break "stream_ended",
            Ok(Some(Err(err))) => {
                eprintln!("transport_error={err}");
                break "transport_error";
            }
            // A Ping, a Pong or a Binary frame is not a tick and is not a reason
            // to stop counting: ending the loop on one would silently truncate
            // the sample and understate the work size.
            Ok(Some(Ok(Message::Text(text)))) => {
                if let Ok(VenueMessage::Trade(trade)) = serde_json::from_str(&text) {
                    // When this tick was due on the wall, per the clock the venue
                    // reported at boot. Lateness is how far past that it actually
                    // arrived here, so it folds in the venue's pacing sleep, the
                    // socket and this reader.
                    let due = clock.sim.wall_ns(trade.ts_event);
                    lateness.push(mogwai_protocol::now_unix_nanos().saturating_sub(due));
                } else {
                    non_trade_text += 1;
                }
            }
            Ok(Some(Ok(_))) => control_frames += 1,
        }
    };
    drop(socket);
    drop(venue);

    assert!(
        !lateness.is_empty(),
        "the venue served no trades in {sample:?}, so there is nothing to report"
    );
    lateness.sort_unstable();
    let quantile = |q: usize| lateness[(q * lateness.len()).div_ceil(100) - 1];
    eprintln!("config={config}");
    eprintln!("frames={}", lateness.len());
    eprintln!("non_trade_text={non_trade_text}");
    eprintln!("control_frames={control_frames}");
    eprintln!("ending={ending}");
    eprintln!("sample_ms={}", sample.as_millis());
    eprintln!("p50_lateness_ns={}", quantile(50));
    eprintln!("p99_lateness_ns={}", quantile(99));
    eprintln!(
        "max_lateness_ns={}",
        lateness.last().copied().unwrap_or_default()
    );
}

/// One blocking HTTP GET, returning the body. Hand-rolled for the same reason
/// the gates do it: the venue's request surface is a handful of routes and a
/// status line, and an HTTP consumer is a dependency this does not need.
fn http_get(base: &str, path: &str) -> String {
    use std::io::{Read, Write};

    let authority = base.trim_start_matches("http://");
    let mut stream = std::net::TcpStream::connect(authority).expect("connect to the venue");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("send request");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).expect("read response");
    let text = String::from_utf8_lossy(&raw).to_string();
    text.split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .expect("a complete response")
}
