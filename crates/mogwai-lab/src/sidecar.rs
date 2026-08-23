// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The benchmarking output channels: phase markers and counters on the
//! harness marker FIFO, and end-of-run `key=value` scalars on stderr.
//!
//! Observation only, structurally. Nothing here reads a draw, a tick or an
//! accumulator - it takes names and integers a caller already has, and it has
//! no return value a caller could branch on. That is deliberate: the module is
//! reachable from the hot commands, and a measurement channel that can change
//! what is measured is not a measurement channel.
//!
//! Degrades to nothing. `MARKER_FIFO_ENV` (`BROKKR_MARKER_FIFO`) is set only by
//! a benchmark harness that has a FIFO drained on the other end. Absent it,
//! [`marker`] and [`counter`] are a load and a branch. Present it, the FIFO is opened once,
//! non-blocking, and a write that would block is dropped rather than paced -
//! the run must not slow down because nobody is reading.
//!
//! The two channels are not interchangeable and the split is not stylistic:
//!
//! - A marker is a true phase boundary, and the harness's phase views open a
//!   segment at every one of them. A name emitted in a loop drowns every such
//!   view at once, so markers stay at the handful of boundaries a run has -
//!   `observed`, `walks`, `bootstrap`, `coarse`, `refine`.
//! - A counter is a work-size reading, `<name>=<i64>`. Non-integers are dropped
//!   silently at the far end, so [`counter`] takes `i64` and nothing else.
//! - An stderr kv pair is an end-of-run scalar scraped into the tracked results
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

/// Resolve the FIFO half of the channel from an already-read variable value.
///
/// Pure with respect to the process environment on purpose: the attachment
/// rule - unset is nothing, empty is nothing, unopenable is nothing - is the
/// whole of the "degrades to nothing" contract, and a rule that reads the
/// environment itself can only be tested by mutating it, which is unsound in a
/// multi-threaded test binary. This takes the reading as an argument instead,
/// so the rule is exercised directly and no seam ships (`channel` below is
/// still the only caller in a real run).
fn resolve_fifo(configured: Option<std::ffi::OsString>) -> Option<Mutex<File>> {
    // O_NONBLOCK on the open as well as the writes: a FIFO opened for
    // writing blocks until a reader attaches, so a blocking open would
    // hang a run whose harness died between setting the variable and
    // draining the pipe. Failure to open is not an error - it is the
    // same no-op as no variable at all.
    configured
        .filter(|path| !path.is_empty())
        .and_then(|path| {
            OpenOptions::new()
                .write(true)
                .custom_flags(nix::libc::O_NONBLOCK)
                .open(path)
                .ok()
        })
        .map(Mutex::new)
}

/// The ambient reading, taken once at the edge: the one place that names
/// [`MARKER_FIFO_ENV`], kept separate from [`resolve_fifo`] so the hop from
/// variable to rule is a named function a test can point at rather than an
/// argument expression nothing can see. Pinned by
/// `the_channel_reads_the_documented_variable`.
fn configured_fifo_path() -> Option<std::ffi::OsString> {
    std::env::var_os(MARKER_FIFO_ENV)
}

fn channel() -> &'static Channel {
    CHANNEL.get_or_init(|| {
        // The FIFO first, then the epoch: the open is a syscall, and stamping
        // before it would date every marker from an instant preceding work the
        // channel had not yet done. Microseconds, and no consumer can see it -
        // but the origin is defined as "the channel exists", so it is taken
        // where that becomes true.
        let fifo = resolve_fifo(configured_fifo_path());
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

fn write_line(line: &str) -> bool {
    write_line_to(channel(), line)
}

/// Returns whether the line reached a FIFO handle at all. The bool is for the
/// inertness test - production callers ignore it, because an instrument that
/// reports failure to its caller is one a caller can branch on.
fn write_line_to(chan: &Channel, line: &str) -> bool {
    let Some(fifo) = chan.fifo.as_ref() else {
        return false;
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
    true
}

fn stamp_from(epoch: Instant) -> u128 {
    epoch.elapsed().as_micros()
}

fn stamp_us() -> u128 {
    stamp_from(channel().epoch)
}

/// Emit a phase boundary. See the module header on why these stay few.
pub fn marker(name: &str) {
    let _inert = write_line(&format!("{} {name}\n", stamp_us()));
}

/// Emit a work-size counter reading on the FIFO. Values must be `i64`; the
/// drain silently discards anything else.
pub fn counter(name: &str, value: i64) {
    let _inert = write_line(&format!("{} @{name}={value}\n", stamp_us()));
}

/// Emit an end-of-run scalar as a stderr `key=value` line.
///
/// Units belong in the key (`_ms`, `_us`, `_bytes`, `_s`): the value column is
/// a bare number in the results row forever after, and a reader months later
/// has only the name to go on.
pub fn kv(key: &str, value: impl Display) {
    // stderr rather than stdout because the harness swallows stdout on every
    // captured path and prints its own result line instead. Unchecked for the
    // same reason as the FIFO write: an instrument does not fail a run.
    let _dropped = writeln!(std::io::stderr().lock(), "{key}={value}");
}

/// Emit a work-size reading on both channels: a counter on the timeline and a
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

    /// The attachment rule itself, asserted rather than merely traversed.
    /// Unset is nothing and empty is nothing, and a path that cannot be
    /// opened as a FIFO is the same nothing - this is what "degrades to
    /// nothing" means, and none of it needs the process environment.
    #[test]
    fn a_fifo_attaches_only_for_a_nonempty_openable_path() {
        assert!(resolve_fifo(None).is_none());
        assert!(resolve_fifo(Some(std::ffi::OsString::from(""))).is_none());
        // A path that exists but is a directory, and a path that does not
        // exist at all: both fail the open, both degrade to no channel.
        assert!(resolve_fifo(Some(std::ffi::OsString::from("/"))).is_none());
        assert!(
            resolve_fifo(Some(std::ffi::OsString::from(
                "/nonexistent/mogwai-sidecar-no-such-fifo"
            )))
            .is_none()
        );
    }

    /// The whole point of the default path: with no FIFO attached, every
    /// emitter is a no-op that cannot panic, cannot block and cannot fail.
    ///
    /// The environment-proof half runs first and unconditionally. The
    /// process channel's half cannot: `BROKKR_MARKER_FIFO` is a real variable
    /// this workspace's own benchmark harness sets, and `CHANNEL` is a
    /// process-wide `OnceLock`, so under a lane that exported it the process
    /// channel legitimately carries a FIFO. That was an `assert!` for one
    /// round and it is a skip now: the guarantee this test exists for is
    /// already established against a locally built inert channel, and a red
    /// suite indistinguishable from a regression is too high a price for
    /// re-establishing it a second way. The skip says so on stderr rather
    /// than passing silently.
    #[test]
    fn emission_without_a_fifo_is_inert() {
        // A channel built here rather than the process one, so the claim holds
        // whatever else in this binary reached `init` first and whatever the
        // ambient environment says.
        let inert = Channel {
            epoch: Instant::now(),
            fifo: resolve_fifo(None),
        };
        assert!(!write_line_to(&inert, "0 phase\n"));

        if let Some(path) = configured_fifo_path() {
            eprintln!(
                "skipping the process-channel half: {MARKER_FIFO_ENV} is set to {path:?}, \
                 so this process legitimately has a FIFO attached"
            );
            return;
        }
        init();
        assert!(
            channel().fifo.is_none(),
            "no variable was set, so the process channel must carry no FIFO"
        );
        // The emitters must reach the drop, not merely not crash. `report`
        // writes a kv pair on stderr unconditionally; only its counter half
        // is inert, which is why the bool is read off `write_line`.
        assert!(!write_line("0 phase\n"));
        marker("phase");
        counter("parents", 12);
        report("prints", 34);
    }

    /// The variable name, which nothing else in this module's tests touches.
    /// `configured_fifo_path` is the only reader, so re-pointing it - or
    /// re-pointing the constant - would otherwise leave every sidecar test
    /// green while the harness's FIFO went unread forever. The literal is
    /// written out here rather than taken from `MARKER_FIFO_ENV`, because
    /// taking it from the constant pins the constant against itself.
    #[test]
    fn the_channel_reads_the_documented_variable() {
        assert_eq!(MARKER_FIFO_ENV, "BROKKR_MARKER_FIFO");
        assert_eq!(
            configured_fifo_path(),
            std::env::var_os("BROKKR_MARKER_FIFO"),
            "the channel's FIFO path does not come from BROKKR_MARKER_FIFO"
        );
    }

    /// `stamp_us` is measured from the pinned origin, so a stamp taken after
    /// `init` is a duration into the run rather than an absolute clock.
    /// Monotonic and small is all the protocol promises, and small is the half
    /// with content: monotonicity is free from `Instant`, while an epoch taken
    /// from the wall clock instead would stamp ~1.8e15 microseconds since 1970
    /// and the harness's phase alignment would be nonsense.
    #[test]
    fn stamps_run_from_the_pinned_origin() {
        // A locally pinned origin, where the bound is exact and cannot rot:
        // the two statements are adjacent, so a second of slack is enormous.
        let local = Instant::now();
        assert!(stamp_from(local) < 1_000_000);

        init();
        let first = stamp_us();
        let second = stamp_us();
        assert!(second >= first);
        // The process-wide origin is pinned at whichever test in this binary
        // reached `init` first, so the bound here is the sweep's, not this
        // test's: one day of microseconds. It cannot rot - no test binary runs
        // for a day, and it is four orders of magnitude below the 1.8e15 a
        // UNIX-epoch stamp would report, which is the defect it discriminates.
        assert!(
            second < 86_400_000_000,
            "stamp {second} is not run-relative"
        );
    }
}
