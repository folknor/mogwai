// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Launching a venue and learning where it listens.
//!
//! One venue serves one run, in the foreground, on a loopback port the kernel
//! assigns. There is no `--addr` and no shared default, deliberately: a shared
//! default is what let two concurrent runs collide or silently connect to each
//! other's venue. So the endpoint is LEARNED rather than assumed - the venue
//! writes exactly one line of JSON to stdout, the [`ReadyRecord`], and that is
//! the only thing it ever writes there. Logs go to stderr, so the two streams
//! never interleave.
//!
//! This module is the launcher side of that handshake, shipped here so consumers
//! do not each re-derive it. Four properties of the contract are load-bearing and
//! none is guessable from the outside; every one of them is handled below rather
//! than documented at the caller.
//!
//! **The child must be spawned from a thread that outlives the run.** The venue
//! arms `PR_SET_PDEATHSIG` so a dead launcher cannot leave an orphan holding
//! positions, and that signal fires on the death of the parent THREAD, not the
//! parent process. In an async application, spawning from a pool task is both
//! the natural thing to write and the wrong thing to write: the task ends, the
//! thread is recycled, and a perfectly healthy launcher loses its venue mid-run.
//! [`launch`] therefore spawns a dedicated OS thread, spawns the child from it,
//! and parks it for the run. Nothing about the caller's runtime can shorten it.
//!
//! **Captured stderr must be drained continuously.** A pipe holds roughly 64 KiB
//! and a full pipe blocks the writer, so a capture nobody reads wedges the venue
//! mid-run - which at the socket is indistinguishable from a hung venue. Every
//! [`StderrSink`] that captures also drains, from the moment of spawn. There is
//! no way to ask this module for an undrained pipe.
//!
//! **The ready read is unbounded unless the launcher bounds it.** It blocks for
//! as long as warmup generation takes, which is proportional to the venue's
//! `warmup_ns` and its tape cadence. [`LaunchSpec::ready_timeout`] bounds it so a
//! venue that will NEVER answer fails as a named timeout instead of hanging the
//! caller forever.
//!
//! **The schema version is checked before any other field is trusted.** The
//! record crosses between separately built binaries, so a venue that has moved
//! ahead of its consumer is refused loudly rather than read field by field with
//! changed meanings. Because this module lives beside [`ReadyRecord::VERSION`], a
//! bump cannot leave the check behind.

use std::{
    collections::VecDeque,
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader},
    net::SocketAddr,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel},
    },
    thread::JoinHandle,
    time::Duration,
};

use crate::ReadyRecord;

/// Binary name assumed when [`LaunchSpec::binary`] is left unset.
pub const DEFAULT_BINARY: &str = "mogwai";

/// Default bound on the readiness read.
///
/// Generous on purpose: the venue synthesizes its warmup tape before it answers,
/// so this bounds a genuinely slow legitimate operation rather than a round
/// trip. Too tight a bound turns a large `warmup_ns` into a spurious boot
/// failure.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(300);

/// How many stderr lines are retained to explain a boot failure.
const STDERR_RING: usize = 64;

/// How often the owning thread wakes to notice the venue exited on its own.
const OWNER_POLL: Duration = Duration::from_millis(200);

/// What to do with the venue's stderr.
///
/// Every capturing variant drains continuously; an undrained pipe is not
/// expressible here, because it wedges the venue in a way that presents as a
/// hang at the socket.
#[derive(Default)]
pub enum StderrSink {
    /// Let the child inherit the caller's stderr. No pipe exists, so nothing can
    /// fill. The venue's log lands wherever the caller's own does.
    #[default]
    Inherit,
    /// Capture and discard. Drained, so the venue never blocks; the most recent
    /// lines are still retained to explain a boot failure.
    Discard,
    /// Capture and hand each line to this callback, on the draining thread.
    /// Keep it cheap - it runs once per log line, and blocking here is the one
    /// way to reintroduce the wedge this type exists to prevent.
    Lines(Box<dyn FnMut(String) + Send>),
}

impl std::fmt::Debug for StderrSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inherit => f.write_str("Inherit"),
            Self::Discard => f.write_str("Discard"),
            Self::Lines(_) => f.write_str("Lines(..)"),
        }
    }
}

/// How the venue for a run is started.
///
/// Every field has a working default, so `LaunchSpec::default()` launches a
/// venue on the built-in config. There is deliberately no endpoint field: the
/// endpoint is not choosable, it is reported.
#[derive(Debug, Default)]
pub struct LaunchSpec {
    /// The venue binary. Unset means [`DEFAULT_BINARY`], resolved on `PATH`.
    pub binary: Option<OsString>,
    /// `serve --config <path>`. Unset uses the venue's built-in defaults; the
    /// venue never consults the working directory.
    pub config: Option<PathBuf>,
    /// `serve --duration <d>`, overriding the config's run duration. Typed, so a
    /// malformed duration is impossible rather than a boot failure the caller
    /// can only report generically. This is SIM time, which under acceleration
    /// is not wall time.
    pub duration: Option<Duration>,
    /// Bound on the readiness read. Unset means [`DEFAULT_READY_TIMEOUT`].
    pub ready_timeout: Option<Duration>,
    /// What to do with the venue's log stream.
    pub stderr: StderrSink,
}

impl LaunchSpec {
    fn ready_timeout(&self) -> Duration {
        self.ready_timeout.unwrap_or(DEFAULT_READY_TIMEOUT)
    }

    fn binary(&self) -> OsString {
        self.binary
            .clone()
            .unwrap_or_else(|| OsString::from(DEFAULT_BINARY))
    }
}

/// Why a launch did not produce a serving venue.
#[derive(Debug)]
pub enum LaunchError {
    /// The binary could not be spawned at all - usually not on `PATH`.
    Spawn {
        binary: OsString,
        source: std::io::Error,
    },
    /// Reading the readiness line failed at the OS level.
    Read(std::io::Error),
    /// Stdout closed without a line. The venue refused to boot, and its own
    /// stderr says why; retained lines are attached when this module captured
    /// them.
    NoRecord { stderr: Vec<String> },
    /// No line arrived inside [`LaunchSpec::ready_timeout`].
    Timeout {
        waited: Duration,
        stderr: Vec<String>,
    },
    /// A line arrived but was not a `ReadyRecord`.
    Malformed { line: String, source: String },
    /// The record's schema version is not the one this build understands.
    Version { reported: u32, understood: u32 },
    /// The owning thread died before reporting, which means it panicked.
    OwnerDied,
    /// The readiness bound was zero, which no venue can meet.
    ZeroReadyTimeout,
}

impl std::fmt::Display for LaunchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn { binary, source } => write!(
                f,
                "could not spawn the venue binary {}: {source}. If the binary is elsewhere, point \
                 the launcher at it - whatever your launcher calls that setting, it ends up as \
                 LaunchSpec::binary",
                binary.to_string_lossy()
            ),
            Self::Read(source) => write!(f, "reading the venue's readiness line: {source}"),
            Self::NoRecord { stderr } => {
                write!(
                    f,
                    "the venue's stdout closed without a readiness line, which is a boot failure{}",
                    format_stderr(stderr)
                )
            }
            Self::Timeout { waited, stderr } => write!(
                f,
                "the venue did not report ready within {waited:?}. That read blocks for as long as \
                 warmup generation takes, so a large warmup_ns or a slow tape cadence can \
                 legitimately need longer - raise the readiness bound, whatever your launcher \
                 calls it (it ends up as LaunchSpec::ready_timeout){}",
                format_stderr(stderr)
            ),
            Self::Malformed { line, source } => {
                write!(
                    f,
                    "the venue's readiness line did not parse ({source}): {line:?}"
                )
            }
            Self::Version {
                reported,
                understood,
            } => write!(
                f,
                "the venue reports readiness schema version {reported} but this build understands \
                 {understood}. The venue binary and this build are out of step - rebuild whichever \
                 is older"
            ),
            Self::OwnerDied => {
                f.write_str("the thread owning the venue died before reporting readiness")
            }
            Self::ZeroReadyTimeout => f.write_str(
                "the readiness bound is zero, which no venue can meet: readiness comes after \
                 warmup generation, which is never instantaneous. Leave it unset for the \
                 default, or give it a real bound",
            ),
        }
    }
}

impl std::error::Error for LaunchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. } | Self::Read(source) => Some(source),
            _ => None,
        }
    }
}

fn format_stderr(lines: &[String]) -> String {
    if lines.is_empty() {
        String::new()
    } else {
        format!(". Venue stderr:\n{}", lines.join("\n"))
    }
}

/// How a venue that ended on its own terms ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VenueExit {
    /// True when the process exited zero, which is what a completed run does
    /// after announcing `RunComplete`.
    pub success: bool,
    /// Exit code, absent when the process was ended by a signal.
    pub code: Option<i32>,
}

/// A running venue, owned for the lifetime of the run.
///
/// Holding this value is what keeps the venue alive: dropping it terminates and
/// reaps the child, so cleanup happens on every exit path including a panic
/// unwinding past it, with no teardown call to forget.
#[derive(Debug)]
pub struct LaunchedVenue {
    record: ReadyRecord,
    exit: Arc<Mutex<Option<VenueExit>>>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    /// Dropping the sender tells the owning thread to bring the venue down.
    shutdown: Option<SyncSender<()>>,
    owner: Option<JoinHandle<()>>,
}

impl LaunchedVenue {
    /// The readiness record as reported at boot.
    ///
    /// `run_seed` is the value that, with the config, the fingerprint and
    /// `version_string`, reproduces this run's path, so it is worth recording
    /// wherever the run's outcome is.
    #[must_use]
    pub fn record(&self) -> &ReadyRecord {
        &self.record
    }

    /// Address the venue actually bound, as the kernel assigned it.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.record.addr
    }

    /// The `ws://` base URL a client config takes.
    #[must_use]
    pub fn base_url(&self) -> String {
        format!("ws://{}", self.record.addr)
    }

    /// The `http://` base for the control plane and the REST surface.
    #[must_use]
    pub fn http_base(&self) -> String {
        format!("http://{}", self.record.addr)
    }

    /// How the venue ended, if it already has. `None` means still serving.
    ///
    /// A venue given a duration ends itself: it announces `RunComplete` and
    /// exits zero. That is a SUCCESSFUL run rather than a death, and telling the
    /// two apart is why this is not just "is it alive".
    #[must_use]
    pub fn exited(&self) -> Option<VenueExit> {
        *self.exit.lock().expect("venue exit state is not poisoned")
    }

    /// The most recently captured stderr lines, oldest first. Empty under
    /// [`StderrSink::Inherit`], which keeps no pipe.
    #[must_use]
    pub fn recent_stderr(&self) -> Vec<String> {
        snapshot(&self.stderr)
    }

    /// Terminate the venue and wait for it.
    ///
    /// `Drop` does the same and ignores the outcome, which is right for an
    /// unwinding path and wrong for an orderly shutdown that wants to report
    /// that the venue would not die.
    ///
    /// # Errors
    ///
    /// Returns an error if the owning thread panicked.
    pub fn shutdown(mut self) -> Result<(), LaunchError> {
        self.terminate()
    }

    fn terminate(&mut self) -> Result<(), LaunchError> {
        drop(self.shutdown.take());
        match self.owner.take() {
            Some(owner) => owner.join().map_err(|_| LaunchError::OwnerDied),
            None => Ok(()),
        }
    }
}

impl Drop for LaunchedVenue {
    fn drop(&mut self) {
        drop(self.terminate());
    }
}

/// Launch a venue and block until it reports ready.
///
/// # Errors
///
/// See [`LaunchError`]: the binary may not spawn, the readiness line may not
/// arrive in time, stdout may close without one, the line may not parse, or its
/// schema version may not be the one this build understands.
pub fn launch(spec: LaunchSpec) -> Result<LaunchedVenue, LaunchError> {
    let binary = spec.binary();
    let argv = serve_argv(&spec);
    let timeout = spec.ready_timeout();
    // A zero bound cannot be met by any venue: readiness comes after warmup
    // generation, which is never instantaneous. Accepting it produced a launch
    // that failed as a timeout every time, blaming a venue that was booting
    // correctly. A launcher whose own config defaults this to zero should hear
    // about it here rather than at the far end of a boot.
    if timeout.is_zero() {
        return Err(LaunchError::ZeroReadyTimeout);
    }
    let LaunchSpec { stderr: sink, .. } = spec;

    let exit = Arc::new(Mutex::new(None));
    let stderr = Arc::new(Mutex::new(VecDeque::new()));
    let (boot_tx, boot_rx) = sync_channel::<Result<ReadyRecord, LaunchError>>(1);
    let (shutdown_tx, shutdown_rx) = sync_channel::<()>(1);

    // The dedicated thread is the whole point: it spawns the child, so it is the
    // parent thread PR_SET_PDEATHSIG watches, and it parks for the run so that
    // signal never fires early.
    let owner = {
        let exit = Arc::clone(&exit);
        let stderr = Arc::clone(&stderr);
        std::thread::Builder::new()
            .name("mogwai-venue".to_owned())
            .spawn(move || own_venue(&binary, &argv, sink, &boot_tx, &shutdown_rx, &exit, &stderr))
            .map_err(|source| LaunchError::Spawn {
                binary: OsString::from("<owning thread>"),
                source,
            })?
    };

    let booted = match boot_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => Err(LaunchError::Timeout {
            waited: timeout,
            stderr: snapshot(&stderr),
        }),
        Err(RecvTimeoutError::Disconnected) => Err(LaunchError::OwnerDied),
    };

    match booted {
        Ok(record) => {
            // Announce the run HERE rather than leaving it to the caller. A path
            // is reproducible only from `run_seed` with the config and the
            // binary's `version_string`, the seed is drawn at launch and exists
            // nowhere until this record carries it, and a consumer that forgets
            // to log it has silently made every run of an entire session
            // irreproducible. One line from the library removes that from the
            // list of things a consumer has to remember.
            tracing::info!(
                addr = %record.addr,
                pid = record.pid,
                symbol = %record.symbol,
                run_seed = record.run_seed,
                warmup_ns = record.warmup_ns,
                version_string = %record.version_string,
                "mogwai venue up"
            );
            Ok(LaunchedVenue {
                record,
                exit,
                stderr,
                shutdown: Some(shutdown_tx),
                owner: Some(owner),
            })
        }
        Err(error) => {
            // Let the owning thread tear down whatever it spawned before this
            // error propagates, so a failed launch leaves no child behind.
            drop(shutdown_tx);
            drop(owner.join());
            Err(error)
        }
    }
}

/// The argv for `serve`, split out so it is testable without spawning anything.
///
/// `--launcher-pid` is always passed. It lets the venue prove it still has the
/// parent it was started by, rather than inferring a death from a change in
/// `getppid()` - an inference blind to a launcher that was already gone before
/// the venue ran its first instruction, which is exactly what a launcher that
/// spawns and exits produces. This process is that parent: `launch` spawns the
/// child from a thread it owns, so the pid recorded here is the one the venue
/// will see.
fn serve_argv(spec: &LaunchSpec) -> Vec<OsString> {
    let mut argv = vec![
        OsString::from("serve"),
        OsString::from("--launcher-pid"),
        OsString::from(std::process::id().to_string()),
    ];
    if let Some(config) = &spec.config {
        argv.push(OsString::from("--config"));
        argv.push(config.clone().into_os_string());
    }
    if let Some(duration) = spec.duration {
        argv.push(OsString::from("--duration"));
        argv.push(OsString::from(format_duration(duration)));
    }
    argv
}

/// Render a duration in the grammar the venue's `--duration` parses, preferring
/// the coarsest exact unit so the argv stays readable in a process listing.
fn format_duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos == 0 {
        return "0s".to_owned();
    }
    if nanos.is_multiple_of(1_000_000_000) {
        return format!("{}s", nanos / 1_000_000_000);
    }
    if nanos.is_multiple_of(1_000_000) {
        return format!("{}ms", nanos / 1_000_000);
    }
    format!("{nanos}ns")
}

fn snapshot(ring: &Arc<Mutex<VecDeque<String>>>) -> Vec<String> {
    ring.lock()
        .map(|lines| lines.iter().cloned().collect())
        .unwrap_or_default()
}

/// Body of the dedicated owning thread: spawn, drain, read one line, report,
/// then park until shutdown or until the venue ends itself, and reap.
fn own_venue(
    binary: &OsStr,
    argv: &[OsString],
    sink: StderrSink,
    boot_tx: &SyncSender<Result<ReadyRecord, LaunchError>>,
    shutdown_rx: &Receiver<()>,
    exit: &Arc<Mutex<Option<VenueExit>>>,
    stderr_ring: &Arc<Mutex<VecDeque<String>>>,
) {
    let captures_stderr = !matches!(sink, StderrSink::Inherit);
    let mut command = Command::new(binary);
    command
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(if captures_stderr {
            Stdio::piped()
        } else {
            Stdio::inherit()
        });

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            drop(boot_tx.send(Err(LaunchError::Spawn {
                binary: binary.to_owned(),
                source,
            })));
            return;
        }
    };

    // Drain from the moment of spawn, forever. An undrained pipe fills at
    // roughly 64 KiB and blocks the venue mid-run.
    let drain = child.stderr.take().map(|stderr| {
        let ring = Arc::clone(stderr_ring);
        let mut sink = sink;
        std::thread::Builder::new()
            .name("mogwai-venue-log".to_owned())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    // A read error means the pipe is gone and the process is
                    // exiting; there is nothing left to drain.
                    let Ok(line) = line else { return };
                    if let Ok(mut ring) = ring.lock() {
                        if ring.len() == STDERR_RING {
                            ring.pop_front();
                        }
                        ring.push_back(line.clone());
                    }
                    if let StderrSink::Lines(callback) = &mut sink {
                        callback(line);
                    }
                }
            })
    });

    let booted = read_ready(&mut child, stderr_ring);
    let serving = booted.is_ok();
    drop(boot_tx.send(booted));

    if serving {
        // Park for the run, waking only often enough to notice the venue ending
        // on its own - which a run with a declared duration does, successfully.
        loop {
            match shutdown_rx.recv_timeout(OWNER_POLL) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if let Ok(Some(status)) = child.try_wait() {
                if let Ok(mut exit) = exit.lock() {
                    *exit = Some(VenueExit {
                        success: status.success(),
                        code: status.code(),
                    });
                }
                break;
            }
        }
    }

    drop(child.kill());
    drop(child.wait());
    if let Some(Ok(drain)) = drain {
        drop(drain.join());
    }
}

/// Read and validate the single readiness line off the child's stdout.
fn read_ready(
    child: &mut Child,
    stderr_ring: &Arc<Mutex<VecDeque<String>>>,
) -> Result<ReadyRecord, LaunchError> {
    let Some(stdout) = child.stdout.take() else {
        return Err(LaunchError::Read(std::io::Error::other(
            "the venue was spawned without a stdout pipe",
        )));
    };
    let mut line = String::new();
    let read = BufReader::new(stdout)
        .read_line(&mut line)
        .map_err(LaunchError::Read)?;
    if read == 0 {
        // Give the drain a moment to deliver the lines that explain why, so the
        // error carries the reason rather than pointing at a log elsewhere.
        std::thread::sleep(Duration::from_millis(50));
        return Err(LaunchError::NoRecord {
            stderr: snapshot(stderr_ring),
        });
    }

    parse_ready(&line)
}

/// Parse one readiness line and vouch for its schema.
///
/// Split from the read so the guard is testable without spawning anything - the
/// version refusal in particular is the branch a consumer most needs to work and
/// least often exercises.
fn parse_ready(line: &str) -> Result<ReadyRecord, LaunchError> {
    let record: ReadyRecord =
        serde_json::from_str(line.trim()).map_err(|source| LaunchError::Malformed {
            line: line.to_owned(),
            source: source.to_string(),
        })?;
    // Version FIRST, before trusting any other field: the record is a wire
    // schema shared with a separately built binary, and a venue that has moved
    // ahead of this build is exactly the case worth refusing loudly rather than
    // reading a field whose meaning has changed.
    if record.version != ReadyRecord::VERSION {
        return Err(LaunchError::Version {
            reported: record.version,
            understood: ReadyRecord::VERSION,
        });
    }
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Not "no flags": the launcher's own pid always goes, because it is what
    /// lets the venue refuse to serve after its owner is already gone.
    #[test]
    fn a_bare_spec_still_identifies_its_launcher() {
        assert_eq!(
            serve_argv(&LaunchSpec::default()),
            vec![
                OsString::from("serve"),
                OsString::from("--launcher-pid"),
                OsString::from(std::process::id().to_string()),
            ]
        );
    }

    #[test]
    fn config_and_duration_reach_the_argv() {
        let argv = serve_argv(&LaunchSpec {
            config: Some(PathBuf::from("/venue/run.toml")),
            duration: Some(Duration::from_secs(30)),
            ..LaunchSpec::default()
        });
        assert_eq!(
            argv,
            vec![
                OsString::from("serve"),
                OsString::from("--launcher-pid"),
                OsString::from(std::process::id().to_string()),
                OsString::from("--config"),
                OsString::from("/venue/run.toml"),
                OsString::from("--duration"),
                OsString::from("30s"),
            ]
        );
    }

    /// A bound no venue can meet is refused before anything is spawned, rather
    /// than failing as a timeout that blames a venue booting correctly.
    #[test]
    fn a_zero_ready_timeout_is_refused_without_spawning() {
        let error = launch(LaunchSpec {
            binary: Some(OsString::from("mogwai-should-never-be-spawned")),
            ready_timeout: Some(Duration::ZERO),
            ..LaunchSpec::default()
        })
        .expect_err("a zero bound cannot be met");
        assert!(
            matches!(error, LaunchError::ZeroReadyTimeout),
            "the refusal must precede the spawn, got {error:?}"
        );
    }

    /// A typed duration is the whole point of taking `Duration` rather than a
    /// string: every value renders into something the venue parses, so a
    /// malformed duration cannot reach a boot failure the caller can only
    /// report generically.
    #[test]
    fn durations_render_in_the_coarsest_exact_unit() {
        assert_eq!(format_duration(Duration::ZERO), "0s");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_millis(1_500)), "1500ms");
        assert_eq!(format_duration(Duration::from_nanos(1)), "1ns");
        assert_eq!(format_duration(Duration::from_micros(1)), "1000ns");
    }

    #[test]
    fn a_missing_binary_reports_the_binary_it_tried() {
        let error = launch(LaunchSpec {
            binary: Some(OsString::from("mogwai-no-such-binary-exists")),
            ready_timeout: Some(Duration::from_secs(5)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        })
        .expect_err("a missing binary cannot launch");
        let message = error.to_string();
        assert!(
            message.contains("mogwai-no-such-binary-exists"),
            "{message}"
        );
        assert!(matches!(error, LaunchError::Spawn { .. }), "{error:?}");
    }

    /// `true` exits immediately having written nothing, which is the shape of a
    /// venue that dies during startup.
    #[test]
    fn stdout_closing_with_no_line_is_a_boot_failure() {
        let error = launch(LaunchSpec {
            binary: Some(OsString::from("true")),
            ready_timeout: Some(Duration::from_secs(10)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        })
        .expect_err("no readiness line is a boot failure");
        assert!(matches!(error, LaunchError::NoRecord { .. }), "{error:?}");
        assert!(error.to_string().contains("without a readiness line"));
    }

    /// `echo serve` prints its argv, which is not a record.
    #[test]
    fn a_line_that_is_not_a_record_is_refused() {
        let error = launch(LaunchSpec {
            binary: Some(OsString::from("echo")),
            ready_timeout: Some(Duration::from_secs(10)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        })
        .expect_err("a non-record line is refused");
        assert!(matches!(error, LaunchError::Malformed { .. }), "{error:?}");
    }

    fn record_json(version: u32) -> String {
        serde_json::json!({
            "version": version,
            "addr": "127.0.0.1:41235",
            "pid": 42,
            "symbol": "BTCUSDT",
            "run_seed": 7,
            "data_origin_ns": 1,
            "run_start_ns": 2,
            "run_duration_ns": null,
            "warmup_ns": 1,
            "version_string": "test",
        })
        .to_string()
    }

    #[test]
    fn a_current_record_parses() {
        let record = parse_ready(&record_json(ReadyRecord::VERSION)).expect("current schema");
        assert_eq!(record.addr.port(), 41235);
        assert_eq!(record.symbol, "BTCUSDT");
    }

    /// The version guard runs before any other field is trusted, and lives
    /// beside the constant it enforces so a bump cannot leave it behind.
    #[test]
    fn a_record_from_another_schema_is_refused() {
        for version in [ReadyRecord::VERSION + 1, ReadyRecord::VERSION - 1] {
            match parse_ready(&record_json(version)) {
                Err(LaunchError::Version {
                    reported,
                    understood,
                }) => {
                    assert_eq!(reported, version);
                    assert_eq!(understood, ReadyRecord::VERSION);
                }
                other => panic!("expected a version refusal for {version}, got {other:?}"),
            }
        }
    }

    /// The timeout path needs a process that holds stdout open and stays silent,
    /// and no stock binary does that when handed `serve`. It is covered against
    /// the real venue in `mogwai-server`'s lifecycle gates, where the binary
    /// exists; here we only pin that the message tells the caller which knob to
    /// turn, since a bare "timed out" would send them hunting the wrong thing.
    #[test]
    fn the_timeout_message_names_the_knob_that_fixes_it() {
        let message = LaunchError::Timeout {
            waited: Duration::from_secs(300),
            stderr: vec!["a venue log line".to_owned()],
        }
        .to_string();
        assert!(message.contains("ready_timeout"), "{message}");
        assert!(message.contains("warmup_ns"), "{message}");
        assert!(message.contains("a venue log line"), "{message}");
    }
}
