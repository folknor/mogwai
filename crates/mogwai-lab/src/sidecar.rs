// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The benchmarking output channels: phase markers and counters on the
//! harness marker FIFO, and end-of-run `key=value` scalars on stderr.
//!
//! OBSERVATION ONLY, structurally. Nothing here reads a draw, a tick or an
//! accumulator - it takes names and integers a caller already has, and it has
//! no return value a caller could branch on. That is deliberate: the module is
//! reachable from the hot commands, and a measurement channel that can change
//! what is measured is not a measurement channel.
//!
//! DEGRADES TO NOTHING. `MOGWAI_MARKER_FIFO_ENV` is set only by a benchmark
//! harness that has a FIFO drained on the other end. Absent it, [`marker`] and
//! [`counter`] are a load and a branch. Present it, the FIFO is opened once,
//! non-blocking, and a write that would block is DROPPED rather than paced -
//! the run must not slow down because nobody is reading.
//!
//! The two channels are not interchangeable and the split is not stylistic:
//!
//! - A MARKER is a true phase boundary, and the harness's phase views open a
//!   segment at every one of them. A name emitted in a loop drowns every such
//!   view at once, so markers stay at the handful of boundaries a run has -
//!   `observed`, `walks`, `bootstrap`, `coarse`, `refine`.
//! - A COUNTER is a work-size reading, `<name>=<i64>`. Non-integers are dropped
//!   silently at the far end, so [`counter`] takes `i64` and nothing else.
//! - An STDERR KV pair is an END-OF-RUN scalar scraped into the tracked results
//!   row. It is the durable half: a counter tells the profile timeline what
//!   happened, a kv pair is what a regression query compares across months.
//!
//! Work-size counters exist to answer the first question a moved wall raises:
//! did the code slow down, or did it do more work. So every timing reported
//! here is accompanied by the size of the work it timed - `parents`, `prints`,
//! `cells_evaluated`, `sessions`, `rows`.

use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// The environment variable naming the marker FIFO. Set by the benchmark
/// harness; unset in every other invocation, including a plain interactive
/// run, which is what makes emission free by default.
const MARKER_FIFO_ENV: &str = "BROKKR_MARKER_FIFO";

/// The process-wide channel: the timestamp origin every marker and counter is
/// relative to, plus the FIFO if one was attached.
struct Channel {
    epoch: Instant,
    fifo: Option<Mutex<File>>,
}

static CHANNEL: OnceLock<Channel> = OnceLock::new();

fn channel() -> &'static Channel {
    CHANNEL.get_or_init(|| {
        // O_NONBLOCK on the open as well as the writes: a FIFO opened for
        // writing BLOCKS until a reader attaches, so a blocking open would
        // hang a run whose harness died between setting the variable and
        // draining the pipe. Failure to open is not an error - it is the
        // same no-op as no variable at all.
        let fifo = std::env::var_os(MARKER_FIFO_ENV)
            .filter(|path| !path.is_empty())
            .and_then(|path| {
                OpenOptions::new()
                    .write(true)
                    .custom_flags(nix::libc::O_NONBLOCK)
                    .open(path)
                    .ok()
            })
            .map(Mutex::new);
        Channel {
            epoch: Instant::now(),
            fifo,
        }
    })
}

/// Pin the timestamp origin to process start.
///
/// Every marker is reported as microseconds since [`Channel::epoch`], and the
/// harness aligns those against a `/proc` sample trajectory it starts when it
/// spawns the process. Without this call the origin is instead whenever the
/// first marker happened to be emitted, which shifts the whole timeline by
/// however long startup took - the one interval a phase decomposition most
/// wants to see. Call it as the first statement of `main`.
pub fn init() {
    let _ = channel();
}

fn write_line(line: &str) {
    let Some(fifo) = channel().fifo.as_ref() else {
        return;
    };
    // A poisoned lock means another thread panicked mid-write. The channel is
    // observational, so the recovery is to keep writing rather than to
    // propagate a panic out of an instrument.
    let mut handle = match fifo.lock() {
        Ok(handle) => handle,
        Err(poisoned) => poisoned.into_inner(),
    };
    // Deliberately unchecked: a full pipe returns EAGAIN under O_NONBLOCK and
    // the line is dropped, which is the documented contract.
    let _dropped = handle.write_all(line.as_bytes());
}

fn stamp_us() -> u128 {
    channel().epoch.elapsed().as_micros()
}

/// Emit a phase boundary. See the module header on why these stay few.
pub fn marker(name: &str) {
    write_line(&format!("{} {name}\n", stamp_us()));
}

/// Emit a work-size counter reading on the FIFO. Values must be `i64`; the
/// drain silently discards anything else.
pub fn counter(name: &str, value: i64) {
    write_line(&format!("{} @{name}={value}\n", stamp_us()));
}

/// Emit an end-of-run scalar as a stderr `key=value` line.
///
/// UNITS BELONG IN THE KEY (`_ms`, `_us`, `_bytes`, `_s`): the value column is
/// a bare number in the results row forever after, and a reader months later
/// has only the name to go on.
pub fn kv(key: &str, value: impl Display) {
    // stderr rather than stdout because the harness swallows stdout on every
    // captured path and prints its own result line instead. Unchecked for the
    // same reason as the FIFO write: an instrument does not fail a run.
    let _dropped = writeln!(std::io::stderr().lock(), "{key}={value}");
}

/// Emit a work-size reading on BOTH channels: a counter on the timeline and a
/// kv pair on the results row.
///
/// The common case for a run's final totals, and the reason it is one call is
/// that emitting to only one channel is nearly always an oversight - the
/// timeline wants the number to attribute a phase, the row wants it to answer
/// "did the work change" on the next comparison.
pub fn report(name: &str, value: i64) {
    counter(name, value);
    kv(name, value);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the default path: with no FIFO attached, every
    /// emitter is a no-op that cannot panic, cannot block and cannot fail.
    /// Run without the variable set, which is every non-harness invocation.
    #[test]
    fn emission_without_a_fifo_is_inert() {
        init();
        marker("phase");
        counter("parents", 12);
        report("prints", 34);
    }

    /// `stamp_us` is measured from the pinned origin, so a stamp taken after
    /// `init` is a duration into the run rather than an absolute clock.
    /// Monotonic and small is all the protocol promises.
    #[test]
    fn stamps_run_from_the_pinned_origin() {
        init();
        let first = stamp_us();
        let second = stamp_us();
        assert!(second >= first);
    }
}
