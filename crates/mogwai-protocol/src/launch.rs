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
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
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

/// Maximum readiness record bytes, including its trailing newline.
const READY_LINE_MAX_BYTES: u64 = 4096;

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
    ///
    /// `Duration::ZERO` is NOT "end immediately" and is not refused: it renders
    /// as `--duration 0s`, which the venue reads as an explicit NO DECLARED
    /// COMPLETION that overrides a finite `run_duration_ns` in the config. A
    /// launcher computing a duration that lands on zero therefore gets a run
    /// bounded only by its own lifetime. Say so rather than assume it: `None`
    /// leaves the config's duration alone, `Some(ZERO)` overrides it to
    /// indefinite, and only a positive value declares a completion.
    pub duration: Option<Duration>,
    /// Bound on the readiness read. Unset means [`DEFAULT_READY_TIMEOUT`].
    pub ready_timeout: Option<Duration>,
    /// What to do with the venue's log stream.
    pub stderr: StderrSink,
    /// Environment variables SET on the venue process, on top of the inherited
    /// environment. Empty by default, which is plain inheritance.
    ///
    /// This exists because inheritance is not a neutral default for one
    /// variable in particular: `RUST_LOG`. The venue's own filter is
    /// `mogwai=info` only when `RUST_LOG` is ABSENT, so a caller that reads the
    /// venue's log to decide something has its conclusion silently rewritten by
    /// whatever the surrounding process happened to export. A test scoring an
    /// ABSENCE of a log line is the dangerous case: under `RUST_LOG=mogwai=error`
    /// the line can never appear, and the absence then means nothing while still
    /// reading as a pass. Pin the variable here rather than inheriting it.
    pub env: Vec<(OsString, OsString)>,
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
    /// The venue would not die: signalling it or reaping it failed at the OS
    /// level. The caller's process keeps running, so a venue still holding its
    /// port is the caller's problem to report rather than ours to swallow.
    Teardown { detail: String },
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
            Self::Teardown { detail } => write!(
                f,
                "the venue could not be brought down: {detail}. It may still be running and \
                 holding its port; check for a stray process before launching another"
            ),
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
    /// What went wrong bringing the venue down, written by the owning thread on
    /// its way out and read by [`LaunchedVenue::shutdown`] after the join.
    teardown: Arc<Mutex<Option<String>>>,
    /// How many owner-poll windows expired WITHOUT a shutdown request, counted
    /// by the owning thread.
    ///
    /// This exists so a test can tell WHICH ARM of the owner loop did the work
    /// instead of inferring it from a fraction of [`OWNER_POLL`]. A test that
    /// means "the reap happened on the shutdown arm" asserts this is zero; one
    /// that means "the owner looked and the venue was still alive" waits for it
    /// to reach one. Both claims used to be encoded as sleeps, which made them
    /// wall-clock races that could stop testing anything and still pass.
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "an observable kept for the tests of the owner loop's two arms; the owning \
                      thread writes it on every build, and only the reader is test-only"
        )
    )]
    polls: Arc<AtomicUsize>,
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

    /// Completed owner-poll windows. See the field.
    #[cfg(test)]
    fn completed_polls(&self) -> usize {
        self.polls.load(Ordering::Relaxed)
    }

    /// Terminate the venue and wait for it.
    ///
    /// `Drop` does the same and ignores the outcome, which is right for an
    /// unwinding path and wrong for an orderly shutdown that wants to report
    /// that the venue would not die.
    ///
    /// # Errors
    ///
    /// [`LaunchError::OwnerDied`] if the owning thread panicked, and
    /// [`LaunchError::Teardown`] if the venue would not die - signalling or
    /// reaping the child failed at the OS level.
    ///
    /// THE SECOND ARM IS WHY THIS RETURNS A RESULT AT ALL. It used to report
    /// only a panicked owner, so the "shut it down and report a failure to do
    /// so" check every caller writes against it was vacuous: a failed kill and a
    /// wedged child both returned `Ok(())`, and a venue still holding its port
    /// was announced as an orderly shutdown. A caller that ignores this may
    /// launch its replacement into an address the old one still owns.
    pub fn shutdown(mut self) -> Result<(), LaunchError> {
        self.terminate()
    }

    fn terminate(&mut self) -> Result<(), LaunchError> {
        drop(self.shutdown.take());
        if let Some(owner) = self.owner.take() {
            owner.join().map_err(|_| LaunchError::OwnerDied)?;
        }
        // AFTER the join, which is what makes the read sound: the owning thread
        // writes this slot on its way out, so reading it before the join could
        // race the write and report a clean teardown of a venue that would not
        // die. Taken rather than copied, so a second call - `Drop` after an
        // explicit `shutdown`, which is the ordinary path - does not report the
        // same failure twice. The ordering is pinned by
        // `the_teardown_detail_is_read_after_the_owner_joins`.
        let detail = self.teardown.lock().ok().and_then(|mut slot| slot.take());
        match detail {
            Some(detail) => Err(LaunchError::Teardown { detail }),
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
    launch_with_poll(spec, OWNER_POLL)
}

/// [`launch`] with the owning thread's poll interval named.
///
/// The interval is a parameter ONLY so a test can pin which arm of the owner
/// loop did the work: a test of the shutdown arm gives an interval no poll can
/// expire within, which is a property of the run rather than a hope about the
/// scheduler. Production has exactly one caller and it passes [`OWNER_POLL`].
fn launch_with_poll(spec: LaunchSpec, owner_poll: Duration) -> Result<LaunchedVenue, LaunchError> {
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
    let LaunchSpec {
        stderr: sink, env, ..
    } = spec;

    let exit = Arc::new(Mutex::new(None));
    let stderr = Arc::new(Mutex::new(VecDeque::new()));
    let teardown: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let polls = Arc::new(AtomicUsize::new(0));
    let (boot_tx, boot_rx) = sync_channel::<Result<ReadyRecord, LaunchError>>(1);
    let (shutdown_tx, shutdown_rx) = sync_channel::<()>(1);

    // The dedicated thread is the whole point: it spawns the child, so it is the
    // parent thread PR_SET_PDEATHSIG watches, and it parks for the run so that
    // signal never fires early.
    let owner = {
        let exit = Arc::clone(&exit);
        let stderr = Arc::clone(&stderr);
        let teardown = Arc::clone(&teardown);
        let polls = Arc::clone(&polls);
        std::thread::Builder::new()
            .name("mogwai-venue".to_owned())
            .spawn(move || {
                own_venue(
                    &binary,
                    &argv,
                    &env,
                    sink,
                    timeout,
                    &boot_tx,
                    &shutdown_rx,
                    &exit,
                    &stderr,
                    &teardown,
                    owner_poll,
                    &polls,
                );
            })
            .map_err(|source| LaunchError::Spawn {
                binary: OsString::from("<owning thread>"),
                source,
            })?
    };

    // Waits WITHOUT a deadline of its own, deliberately. The readiness bound is
    // enforced inside the owner, because the owner holds the `Child` and can
    // therefore end the wait; a deadline here could only stop waiting, and then
    // had to join a thread still blocked in `read_line` on a child that may
    // never write. That inverted the whole point of the bound - a wedged venue
    // made `launch` hang forever, AFTER reporting a timeout. The owner always
    // sends exactly one result, so a disconnect here means it panicked.
    let booted = boot_rx.recv().unwrap_or(Err(LaunchError::OwnerDied));

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
                run_start_ns = record.run_start_ns,
                run_seed = record.run_seed,
                warmup_ns = record.warmup_ns,
                version_string = %record.version_string,
                "mogwai venue up"
            );
            Ok(LaunchedVenue {
                record,
                exit,
                stderr,
                teardown,
                polls,
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
    // A sub-millisecond remainder renders as nanoseconds, which is exact for
    // every duration a run could want - but `Duration` reaches far past what a
    // nanosecond count fits in `u64`, and the venue's parser refuses the
    // oversized integer before startup. Falling back to whole seconds keeps the
    // promise this type exists to make: every value renders into something the
    // venue accepts. The precision lost is nanoseconds off a span of at least
    // half a millennium.
    match u64::try_from(nanos) {
        Ok(nanos) => format!("{nanos}ns"),
        Err(_) => format!("{}s", duration.as_secs()),
    }
}

fn snapshot(ring: &Arc<Mutex<VecDeque<String>>>) -> Vec<String> {
    ring.lock()
        .map(|lines| lines.iter().cloned().collect())
        .unwrap_or_default()
}

/// Body of the dedicated owning thread: spawn, drain, read one line, report,
/// then park until shutdown or until the venue ends itself, and reap.
#[expect(
    clippy::too_many_arguments,
    reason = "every argument is a distinct channel or piece of shared state this thread owns; \
              bundling them into a struct would name the bundle after this function and hide \
              which of them cross thread boundaries"
)]
fn own_venue(
    binary: &OsStr,
    argv: &[OsString],
    env: &[(OsString, OsString)],
    sink: StderrSink,
    ready_timeout: Duration,
    boot_tx: &SyncSender<Result<ReadyRecord, LaunchError>>,
    shutdown_rx: &Receiver<()>,
    exit: &Arc<Mutex<Option<VenueExit>>>,
    stderr_ring: &Arc<Mutex<VecDeque<String>>>,
    teardown: &Arc<Mutex<Option<String>>>,
    owner_poll: Duration,
    polls: &Arc<AtomicUsize>,
) {
    let captures_stderr = !matches!(sink, StderrSink::Inherit);
    let mut command = Command::new(binary);
    command
        .args(argv)
        .envs(env.iter().map(|(key, value)| (key, value)))
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

    // The readiness read happens on ITS OWN thread so this one keeps a deadline.
    // `read_line` on a child that holds stdout open and never writes cannot be
    // interrupted from outside, so the bound has to belong to whoever can end
    // the wait - and that is this thread, which owns the `Child`. On expiry it
    // kills the venue, which closes stdout and releases the reader.
    let stdout = child.stdout.take();
    let (line_tx, line_rx) = sync_channel::<Result<ReadyRecord, LaunchError>>(1);
    let reader = std::thread::Builder::new()
        .name("mogwai-venue-ready".to_owned())
        .spawn(move || {
            let booted = match stdout {
                Some(stdout) => read_ready(stdout),
                None => Err(LaunchError::Read(std::io::Error::other(
                    "the venue was spawned without a stdout pipe",
                ))),
            };
            drop(line_tx.send(booted));
        });

    let booted = match reader {
        Err(source) => Err(LaunchError::Spawn {
            binary: OsString::from("<readiness reader thread>"),
            source,
        }),
        Ok(reader) => {
            let (outcome, reader_is_released) = match line_rx.recv_timeout(ready_timeout) {
                // The reader sent, so it is at or past the send and will end.
                Ok(booted) => (booted, true),
                Err(RecvTimeoutError::Timeout) => {
                    // Kill first, which closes stdout and releases the reader in
                    // every case where the child itself holds the write end.
                    drop(child.kill());
                    (
                        Err(LaunchError::Timeout {
                            waited: ready_timeout,
                            stderr: snapshot(stderr_ring),
                        }),
                        false,
                    )
                }
                // The sender was dropped, so the reader thread is over.
                Err(RecvTimeoutError::Disconnected) => (
                    Err(LaunchError::Read(std::io::Error::other(
                        "the readiness reader died without reporting",
                    ))),
                    true,
                ),
            };
            // THE TIMEOUT PATH DOES NOT JOIN, and that is the difference between
            // reporting a timeout and hanging inside one. Killing the child
            // closes stdout only while the CHILD holds the write end: a
            // `BA_MOGWAI_BIN` naming a wrapper script that launches the venue
            // without `exec` leaves the grandchild holding an inherited copy, so
            // `read_line` never returns, and joining here blocked the launcher
            // forever immediately after it had decided to report a timeout. The
            // bound has to hold against a child that misbehaves, or it is not a
            // bound. A stranded reader is one leaked thread, which is the same
            // trade the stderr drain below makes and for the same reason.
            if reader_is_released {
                drop(reader.join());
            }
            outcome
        }
    };

    // A boot failure's explanation arrives on stderr, which is drained by
    // another thread; give it the moment it needs before the ring is copied
    // into the error, and re-snapshot rather than trusting an earlier copy.
    let booted = match booted {
        Err(LaunchError::NoRecord { .. }) => {
            std::thread::sleep(Duration::from_millis(50));
            Err(LaunchError::NoRecord {
                stderr: snapshot(stderr_ring),
            })
        }
        other => other,
    };

    let serving = booted.is_ok();
    drop(boot_tx.send(booted));

    if serving {
        // Park for the run, waking only often enough to notice the venue ending
        // on its own - which a run with a declared duration does, successfully.
        //
        // THE REAP IS UNCONDITIONAL ON THE WAY OUT, including the arm where the
        // caller asked for shutdown. It used to run only on the poll arm, so a
        // venue that finished its declared run inside the last poll window - the
        // ordinary ending for a bounded run whose caller then tears down - was
        // reaped with no `VenueExit` recorded, reporting `exited() == None` and
        // reading to every caller as a venue killed while healthy. That made the
        // run-complete probe and the teardown disclosure both wrong about a
        // clean ending, which is the worst direction for a signal whose entire
        // job is telling a planned exit from a death.
        loop {
            let asked_to_stop = match shutdown_rx.recv_timeout(owner_poll) {
                Ok(()) | Err(RecvTimeoutError::Disconnected) => true,
                Err(RecvTimeoutError::Timeout) => {
                    // Published BEFORE the `try_wait` below, so an observer that
                    // sees a nonzero count knows the poll arm had already begun
                    // its look. The count is the only evidence a test has of
                    // which arm ran.
                    polls.fetch_add(1, Ordering::Relaxed);
                    false
                }
            };
            if let Ok(Some(status)) = child.try_wait() {
                if let Ok(mut exit) = exit.lock() {
                    *exit = Some(VenueExit {
                        success: status.success(),
                        code: status.code(),
                    });
                }
                break;
            }
            if asked_to_stop {
                break;
            }
        }
    }

    // What the kill and the reap actually did, so `shutdown` can report a venue
    // that would not die instead of returning `Ok(())` for it. A child already
    // reaped above makes both of these fail harmlessly, which is why the record
    // is only kept when nothing was reaped.
    let reaped = exit.lock().is_ok_and(|exit| exit.is_some());
    let killed = child.kill();
    let waited = child.wait();
    if !reaped {
        let failure = match (killed, waited) {
            (Err(e), _) => Some(format!("signalling the venue failed: {e}")),
            (Ok(()), Err(e)) => Some(format!("reaping the venue failed: {e}")),
            (Ok(()), Ok(_)) => None,
        };
        if let (Some(detail), Ok(mut slot)) = (failure, teardown.lock()) {
            *slot = Some(detail);
        }
    }
    // The drain thread is NOT joined. It runs a caller-supplied callback once
    // per log line, and joining it here would put arbitrary user code in the
    // path of `LaunchedVenue::drop` - a callback that blocks on a mutex the
    // dropping thread holds, or on a current-thread runtime that is shutting
    // down, would deadlock a destructor with no cancellation path, including
    // during a panic unwind. The child is dead by this point, so its stderr is
    // closed and the thread ends on its own; a callback that never returns is
    // then one leaked thread rather than a hung process.
    drop(drain);
}

/// Read and validate the single readiness line off the child's stdout.
///
/// Takes the handle rather than the `Child`, because this runs on a thread that
/// must not own the process it is reading from - the thread that DOES own it is
/// the one enforcing the readiness deadline, and it can only do that by killing
/// the child out from under this read.
///
/// The stderr for a `NoRecord` is attached by the caller, which is the only
/// party that knows how long the drain has had to deliver it.
fn read_ready(stdout: impl std::io::Read) -> Result<ReadyRecord, LaunchError> {
    let mut bytes = Vec::new();
    // The `take` wraps the CHILD's stream, not the `BufReader` around it. That
    // ordering is the whole bound: wrapping the other way still lets the
    // buffer pull a full chunk past the ceiling from a hostile writer, and
    // leaves the refusal below as the only evidence a cap exists - which a
    // test cannot tell apart from no cap at all.
    let read = BufReader::new(stdout.take(READY_LINE_MAX_BYTES + 1))
        .read_until(b'\n', &mut bytes)
        .map_err(LaunchError::Read)?;
    if read == 0 {
        return Err(LaunchError::NoRecord { stderr: Vec::new() });
    }
    if u64::try_from(read).unwrap_or(u64::MAX) > READY_LINE_MAX_BYTES {
        let prefix = &bytes[..READY_LINE_MAX_BYTES as usize];
        let valid = std::str::from_utf8(prefix).map_or_else(
            |error| &prefix[..error.valid_up_to()],
            |valid| valid.as_bytes(),
        );
        return Err(LaunchError::Malformed {
            line: String::from_utf8_lossy(valid).into_owned(),
            source: format!("readiness line exceeds {READY_LINE_MAX_BYTES} bytes"),
        });
    }

    let line = String::from_utf8(bytes).map_err(|source| {
        LaunchError::Read(std::io::Error::new(std::io::ErrorKind::InvalidData, source))
    })?;

    parse_ready(&line)
}

/// Parse one readiness line and vouch for its schema.
///
/// Split from the read so the guard is testable without spawning anything - the
/// version refusal in particular is the branch a consumer most needs to work and
/// least often exercises.
fn parse_ready(line: &str) -> Result<ReadyRecord, LaunchError> {
    // Version FIRST means reading it off the RAW json, before the record is
    // deserialized. Deserializing first and checking after only works while the
    // rest of the schema still happens to fit this build's struct - the moment a
    // newer venue retypes or removes any other field, the mismatch surfaces as
    // `Malformed` and the operator is told the venue speaks gibberish rather
    // than that it is a version ahead. The version field is the one thing whose
    // shape may never change, so it is the one thing safe to read alone.
    let probe: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|source| LaunchError::Malformed {
            line: line.to_owned(),
            source: source.to_string(),
        })?;
    match probe.get("version").and_then(serde_json::Value::as_u64) {
        Some(version) if version == u64::from(ReadyRecord::VERSION) => {}
        Some(version) => {
            return Err(LaunchError::Version {
                reported: u32::try_from(version).unwrap_or(u32::MAX),
                understood: ReadyRecord::VERSION,
            });
        }
        None => {
            return Err(LaunchError::Malformed {
                line: line.to_owned(),
                source: "no readable `version` field".to_owned(),
            });
        }
    }

    let record: ReadyRecord =
        serde_json::from_str(line.trim()).map_err(|source| LaunchError::Malformed {
            line: line.to_owned(),
            source: source.to_string(),
        })?;
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

    #[test]
    fn zero_duration_is_preserved_as_an_explicit_cli_override() {
        let argv = serve_argv(&LaunchSpec {
            config: Some(PathBuf::from("/venue/finite.toml")),
            duration: Some(Duration::ZERO),
            ..LaunchSpec::default()
        });
        assert_eq!(argv.last(), Some(&OsString::from("0s")));
    }

    /// Linux's `ETXTBSY`. Spelled numerically rather than as
    /// `ErrorKind::ExecutableFileBusy` so the retry does not depend on that
    /// variant's stabilization.
    const ETXTBSY: i32 = 26;

    /// How many times a spawn refused with `ETXTBSY` is retried before the
    /// launch outcome is taken at face value. The window it covers is another
    /// process's fork-to-exec latency, so this budget is orders of magnitude
    /// above it, and exhausting it still fails the caller's real assertion.
    const ETXTBSY_ATTEMPTS: usize = 40;

    /// SERIALIZES EVERY `fork` IN THIS TEST BINARY AGAINST EVERY WRITE OF A
    /// SCRIPTED FIXTURE, which is what closes the intra-process half of the
    /// `ETXTBSY` race STRUCTURALLY rather than by retrying it.
    ///
    /// The race is this. A fixture below writes a shell script and then execs
    /// it. `execve` refuses with `ETXTBSY` while any process holds that inode
    /// open for writing, and while this module's helper holds it open, a
    /// SIBLING TEST THAT FORKS inherits the descriptor - Rust opens files
    /// `O_CLOEXEC`, so the child drops it at ITS exec, but not before. A unique
    /// path cannot touch that window, because the inherited copy is what the
    /// kernel counts; pid-qualifying the name closes only the CROSS-PROCESS
    /// half, where the gate's dev and instrumented sweeps run this crate's
    /// tests at once.
    ///
    /// So the two events are made mutually exclusive instead. Every write of a
    /// fixture takes this lock, and every `launch` in this module takes it for
    /// the duration of the call - `launch` returns only after `Command::spawn`
    /// has confirmed the child's exec through its `CLOEXEC` report pipe, so
    /// holding it across the call covers the whole fork-to-exec span. THE
    /// ARGUMENT DEPENDS ON `launch.rs` BEING THE ONLY `Command::new` IN THE
    /// CRATE, which it is; a second spawner anywhere in `mogwai-protocol` owes
    /// this lock too, or the window reopens.
    ///
    /// The retry below is kept as a backstop rather than as the mechanism. It
    /// costs nothing when the lock holds, and the failure it guards is
    /// intermittent, cross-sweep and expensive to diagnose - it has already
    /// been diagnosed twice. It is not a silent degradation: `busy` is derived
    /// from the error variant, no other error is retried, and an exhausted
    /// budget still fails the caller's own assertion with the attempt count.
    static SPAWN_SERIALIZATION: Mutex<()> = Mutex::new(());

    fn spawn_lock() -> std::sync::MutexGuard<'static, ()> {
        SPAWN_SERIALIZATION
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Launch under [`SPAWN_SERIALIZATION`], retrying only `ETXTBSY`.
    ///
    /// Takes a closure rather than a `LaunchSpec` because the spec owns a boxed
    /// stderr callback and cannot be cloned for a second attempt.
    ///
    /// Returns the attempt count, so a failure message can say whether the
    /// budget was spent, and the LAST attempt's elapsed time, which is the only
    /// one a wall-clock bound may be read off.
    fn launch_serialized(
        spec: impl FnMut() -> LaunchSpec,
    ) -> (Result<LaunchedVenue, LaunchError>, usize, Duration) {
        launch_serialized_with_poll(spec, OWNER_POLL)
    }

    /// [`launch_serialized`] with the owner's poll interval named, for the one
    /// test that must pin WHICH ARM of the owner loop reaped the venue.
    fn launch_serialized_with_poll(
        mut spec: impl FnMut() -> LaunchSpec,
        owner_poll: Duration,
    ) -> (Result<LaunchedVenue, LaunchError>, usize, Duration) {
        let mut attempts = 0;
        loop {
            let guard = spawn_lock();
            let started = std::time::Instant::now();
            let outcome = launch_with_poll(spec(), owner_poll);
            let elapsed = started.elapsed();
            drop(guard);
            attempts += 1;
            let busy = matches!(
                &outcome,
                Err(LaunchError::Spawn { source, .. }) if source.raw_os_error() == Some(ETXTBSY)
            );
            if !busy || attempts >= ETXTBSY_ATTEMPTS {
                return (outcome, attempts, elapsed);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
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
        let (outcome, _, _) = launch_serialized(|| LaunchSpec {
            binary: Some(OsString::from("mogwai-no-such-binary-exists")),
            ready_timeout: Some(Duration::from_secs(5)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        });
        let error = outcome.expect_err("a missing binary cannot launch");
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
        let (outcome, _, _) = launch_serialized(|| LaunchSpec {
            binary: Some(OsString::from("true")),
            ready_timeout: Some(Duration::from_secs(10)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        });
        let error = outcome.expect_err("no readiness line is a boot failure");
        assert!(matches!(error, LaunchError::NoRecord { .. }), "{error:?}");
        assert!(error.to_string().contains("without a readiness line"));
    }

    /// `echo serve` prints its argv, which is not a record.
    #[test]
    fn a_line_that_is_not_a_record_is_refused() {
        let (outcome, _, _) = launch_serialized(|| LaunchSpec {
            binary: Some(OsString::from("echo")),
            ready_timeout: Some(Duration::from_secs(10)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        });
        let error = outcome.expect_err("a non-record line is refused");
        assert!(matches!(error, LaunchError::Malformed { .. }), "{error:?}");
    }

    fn record_value(version: u32) -> serde_json::Value {
        serde_json::json!({
            "version": version,
            "addr": "127.0.0.1:41235",
            "pid": 42,
            "run_seed": 7,
            "data_origin_ns": 1,
            "run_start_ns": 2,
            "run_duration_ns": null,
            "warmup_ns": 1,
            "reset_account_on_reconnect": false,
            "account_ttl_ms": 0,
            "version_string": "test",
        })
    }

    fn record_json(version: u32) -> String {
        record_value(version).to_string()
    }

    /// The same body with the `symbol` key version 5 carried and version 6
    /// dropped, so a test can put a stale field in front of the parser.
    fn record_json_with_symbol(version: u32) -> String {
        let mut value = record_value(version);
        value["symbol"] = serde_json::json!("BTCUSDT");
        value.to_string()
    }

    #[test]
    fn a_current_record_parses() {
        let record = parse_ready(&record_json(ReadyRecord::VERSION)).expect("current schema");
        assert_eq!(record.addr.port(), 41235);
        assert_eq!(record.run_seed, 7);
        assert_eq!(record.data_origin_ns, 1);
        assert_eq!(record.run_start_ns, 2);
        assert_eq!(record.warmup_ns, 1);
        assert_eq!(record.run_duration_ns, None);
    }

    /// `ReadyRecord` does not set `deny_unknown_fields`, so serde happily
    /// ignores the `symbol` key version 5 carried: the raw version check in
    /// `parse_ready` is the ONLY thing that refuses a stale record. Both halves
    /// are asserted, because the refusal alone would hold just as well if serde
    /// were strict, and then the test would be documenting the wrong mechanism.
    #[test]
    fn a_record_carrying_a_symbol_is_refused_by_version_alone() {
        match parse_ready(&record_json_with_symbol(ReadyRecord::VERSION - 1)) {
            Err(LaunchError::Version {
                reported,
                understood,
            }) => {
                assert_eq!(reported, ReadyRecord::VERSION - 1);
                assert_eq!(understood, ReadyRecord::VERSION);
            }
            other => panic!("expected a version refusal, got {other:?}"),
        }

        let record = parse_ready(&record_json_with_symbol(ReadyRecord::VERSION))
            .expect("serde ignores the removed field once the version matches");
        assert_eq!(record.version, ReadyRecord::VERSION);
    }

    /// A reader that reports how many bytes it was actually asked to hand over.
    /// The refusal message alone cannot distinguish a bounded read from an
    /// unbounded one that truncates afterwards, and the finding is about the
    /// unbounded READ, so the byte count is the assertion that matters.
    struct CountingReader<'a> {
        inner: std::io::Cursor<Vec<u8>>,
        delivered: &'a std::cell::Cell<usize>,
    }

    impl std::io::Read for CountingReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = std::io::Read::read(&mut self.inner, buf)?;
            self.delivered.set(self.delivered.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn the_readiness_line_is_size_bounded() {
        for (hostile, expected_prefix_len) in [
            // A hostile venue writing 100 KB with no newline at all.
            ("x".repeat(READY_LINE_MAX_BYTES as usize + 100_000), 4096),
            // The same, in a 3-byte character: the ceiling falls mid-character,
            // so the retained prefix must stop at the last COMPLETE one.
            ("\u{20ac}".repeat(2_000), 4095),
        ] {
            let source_len = hostile.len();
            let delivered = std::cell::Cell::new(0);
            let reader = CountingReader {
                inner: std::io::Cursor::new(hostile.into_bytes()),
                delivered: &delivered,
            };
            let error =
                read_ready(reader).expect_err("a line over the readiness ceiling must be refused");
            let display_len = error.to_string().len();
            let LaunchError::Malformed { line, source } = error else {
                panic!("wrong refusal: {error:?}")
            };
            assert_eq!(line.len(), expected_prefix_len);
            assert!(source.contains("exceeds 4096 bytes"), "{source}");
            assert!(display_len < READY_LINE_MAX_BYTES as usize + 200);
            // The bound itself: one byte past the ceiling is enough to know the
            // record is too long, and nothing beyond it is ever pulled in.
            assert!(
                delivered.get() as u64 <= READY_LINE_MAX_BYTES + 1,
                "read {} of {source_len} bytes; the ceiling is {READY_LINE_MAX_BYTES}",
                delivered.get()
            );
        }
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

    /// An executable fixture venue, unlinked when the test that owns it ends.
    ///
    /// Written at test time rather than committed because no stock binary has
    /// any of these shapes - `sleep` and `cat` both reject the `serve` argv the
    /// launcher prepends and exit, which is a boot failure rather than the
    /// behaviour under test - and because a committed fixture would carry an
    /// executable bit that has to survive a checkout.
    struct VenueScript {
        path: PathBuf,
    }

    impl VenueScript {
        fn binary(&self) -> OsString {
            self.path.clone().into_os_string()
        }
    }

    impl Drop for VenueScript {
        /// The name is per-process AND per-fixture, so nothing collects these
        /// between runs; unlink rather than accumulating one script per fixture
        /// per test-binary pid.
        fn drop(&mut self) {
            drop(std::fs::remove_file(&self.path));
        }
    }

    /// THE PATH IS PID-QUALIFIED, WHICH CLOSES THE CROSS-PROCESS `ETXTBSY` RACE
    /// ONLY. The full gate runs several sweeps, and the dev and instrumented
    /// ones both build and run this crate's unit tests, so two processes run
    /// these tests at once; without the pid one would write a script while the
    /// other execed it. Embedding only the fixture's name would have fixed
    /// nothing, since both processes compute the same name - the pid is what
    /// separates them, and the name is what separates two fixtures inside one
    /// process.
    ///
    /// The INTRA-PROCESS half is closed by [`SPAWN_SERIALIZATION`], which this
    /// takes for the write. See that lock for why a unique path cannot close
    /// it on its own.
    fn write_venue_script(name: &str, body: &str) -> VenueScript {
        use std::os::unix::fs::PermissionsExt;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("mogwai-venue-{name}-{}.sh", std::process::id()));
        std::fs::create_dir_all(path.parent().expect("target dir")).expect("create target dir");
        let guard = spawn_lock();
        std::fs::write(&path, body).expect("write the venue script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make the venue script executable");
        drop(guard);
        VenueScript { path }
    }

    /// A venue that never speaks: holds its inherited stdout open, writes
    /// nothing, and ignores whatever argv it is handed.
    fn silent_venue() -> VenueScript {
        write_venue_script("silent", "#!/bin/sh\nexec sleep 60\n")
    }

    /// A venue that BOOTS: it prints one valid `ReadyRecord` line, flushes it by
    /// virtue of `printf` writing it whole, and then does `then`.
    ///
    /// This is what lets a unit test reach the second half of `own_venue` - the
    /// owner-poll loop, `try_wait`, the `VenueExit` record and the teardown
    /// path. Every other launch fixture in this module fails BEFORE readiness,
    /// so nothing here used to get past it.
    ///
    /// The readiness line is this module's own `record_json`, so a schema bump
    /// moves the fixture with the parser rather than leaving a hand-written
    /// record to rot. The record's `addr` is never connected to; only `launch`
    /// reads it.
    ///
    /// THE `pid` FIELD IS THE SCRIPT'S OWN `$$`, not the fixture's 42, and that
    /// is load-bearing: it is what lets a test WAIT FOR THE CHILD TO ACTUALLY
    /// END rather than sleep and hope. `$$` survives the `exec` the fixtures use,
    /// so it names the process the launcher is holding in every one of them.
    fn scripted_venue(name: &str, then: &str) -> VenueScript {
        let record = record_json(ReadyRecord::VERSION);
        let (prefix, suffix) = record
            .split_once("\"pid\":42")
            .expect("the fixture record carries a literal pid the shell can replace");
        write_venue_script(
            name,
            &format!("#!/bin/sh\nprintf '%s\\n' '{prefix}\"pid\":'$$'{suffix}'\n{then}\n"),
        )
    }

    /// Wait until `pid` is no longer a live process from the kernel's point of
    /// view - it has exited and is either a zombie awaiting its reap or already
    /// gone.
    ///
    /// This is the OBSERVABLE that replaces "sleep a fraction of `OWNER_POLL`
    /// and assume the child got there". A child the launcher has not reaped
    /// stays a zombie, so the state is stable once reached rather than a window
    /// that can be missed.
    fn await_child_end(pid: u32) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            let ended = match &stat {
                // Gone entirely: it exited and something reaped it.
                Err(_) => true,
                // The comm field can contain anything including spaces and
                // parens, so the state letter is the first field after the LAST
                // close paren.
                Ok(stat) => stat
                    .rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .is_none_or(|state| state == "Z" || state == "X"),
            };
            if ended {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the scripted venue never ended"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn scripted_spec(script: &VenueScript) -> LaunchSpec {
        LaunchSpec {
            binary: Some(script.binary()),
            ready_timeout: Some(Duration::from_secs(10)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        }
    }

    /// Launch a scripted venue, or fail naming the attempt count so an
    /// exhausted `ETXTBSY` budget is distinguishable from any other refusal.
    fn launch_scripted(script: &VenueScript) -> LaunchedVenue {
        launch_scripted_with_poll(script, OWNER_POLL)
    }

    fn launch_scripted_with_poll(script: &VenueScript, owner_poll: Duration) -> LaunchedVenue {
        let (outcome, attempts, _) =
            launch_serialized_with_poll(|| scripted_spec(script), owner_poll);
        match outcome {
            Ok(venue) => venue,
            Err(error) => {
                panic!("a scripted venue must boot; after {attempts} attempt(s): {error}")
            }
        }
    }

    /// Poll the venue's own exit record to a deadline. The owner notices an exit
    /// only every [`OWNER_POLL`], so an immediate read would be a race rather
    /// than an observation.
    fn await_exit(venue: &LaunchedVenue) -> VenueExit {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(exit) = venue.exited() {
                return exit;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the venue's exit was never recorded"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The bound is a WALL-CLOCK bound, not merely an eventual error.
    ///
    /// This is the case that used to hang FOREVER after reporting its timeout:
    /// the thread owning the child sat in `read_line` on a line that was never
    /// coming, and `launch` joined it. The assertion that matters is the elapsed
    /// time, not the error variant - the old code produced the right variant and
    /// then never returned.
    #[test]
    fn the_ready_bound_returns_on_time_against_a_silent_venue() {
        let script = silent_venue();
        let (outcome, attempts, elapsed) = launch_serialized(|| LaunchSpec {
            binary: Some(script.binary()),
            ready_timeout: Some(Duration::from_millis(400)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        });
        let error = outcome.expect_err("a venue that never speaks must time out");
        assert!(
            matches!(error, LaunchError::Timeout { .. }),
            "{error:?} after {attempts} attempt(s)"
        );
        assert!(
            elapsed < Duration::from_secs(10),
            "launch must return on its own bound, took {elapsed:?}"
        );
    }

    /// A venue that closes stdout without speaking is a boot failure EVEN
    /// THOUGH IT IS STILL RUNNING, and the launcher reports it promptly rather
    /// than waiting the child out.
    ///
    /// This is the counterexample that keeps the `NoRecord` path's fixed 50 ms
    /// pause from being replaced by "drain stderr to EOF, the child is dead by
    /// then". `NoRecord` means end-of-file on the STDOUT pipe, which is every
    /// write end closed - a venue that closes its own stdout, or hands it to a
    /// grandchild that closes it, reaches this branch while very much alive. An
    /// unbounded drain here would hang `launch` forever immediately after it
    /// had decided to report a boot failure, which is the defect the readiness
    /// reader's bound already exists to prevent.
    #[test]
    fn a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure() {
        let script = write_venue_script("mute", "#!/bin/sh\nexec 1>&-\nexec sleep 60\n");
        let (outcome, attempts, elapsed) = launch_serialized(|| LaunchSpec {
            binary: Some(script.binary()),
            ready_timeout: Some(Duration::from_secs(300)),
            stderr: StderrSink::Discard,
            ..LaunchSpec::default()
        });
        let error = outcome.expect_err("a closed stdout with no line is a boot failure");
        assert!(
            matches!(error, LaunchError::NoRecord { .. }),
            "{error:?} after {attempts} attempt(s)"
        );
        // Well under the child's own 60 s and under the 300 s readiness bound,
        // so neither of those is what ended the wait.
        assert!(
            elapsed < Duration::from_secs(5),
            "the boot failure must be reported without waiting the venue out, took {elapsed:?}"
        );
    }

    /// A venue that ends itself INSIDE THE POLL WINDOW the caller's shutdown
    /// closes must still have its exit recorded.
    ///
    /// THIS IS THE UNCONDITIONAL-REAP FIX, AND IT IS THE ONLY TEST OF IT. The
    /// `try_wait` used to run only on the poll arm, so a bounded run that
    /// finished during the last window - the ORDINARY ending for a run with a
    /// declared duration whose caller then tears down - was reaped with no
    /// `VenueExit`, reporting `exited() == None`, which reads as a venue killed
    /// while healthy. A clean completion and a death are then the same signal.
    ///
    /// NEITHER PRECONDITION IS A SLEEP, because both directions of wall-clock
    /// slip used to be live and one of them made the test PASS WITH THE FIX
    /// REVERTED. Reverting moves `if asked_to_stop { break; }` ahead of the
    /// `try_wait` on the SHUTDOWN arm only; the POLL arm reaps either way. So a
    /// test that slept a fraction of `OWNER_POLL` and let the poll arm win still
    /// saw the exit recorded, and stopped testing the fix without failing. In
    /// the other direction a child that had not yet exited by the sleep failed
    /// the test for a reason unrelated to the fix.
    ///
    /// Both are replaced by observables:
    ///
    /// - The owner's poll interval is set to an hour, so NO POLL CAN EXPIRE
    ///   during the test, and `completed_polls()` is asserted zero afterwards.
    ///   That is the property the doc used to merely claim.
    /// - `await_child_end` waits for the kernel to agree the script is over
    ///   before the teardown, using the pid the fixture reports as its own. An
    ///   unreaped child stays a zombie, so this is a stable state and not a
    ///   window.
    ///
    /// `terminate` rather than `shutdown` because `shutdown` consumes the venue,
    /// and the record has to be READ after the teardown that would have skipped
    /// it.
    #[test]
    fn a_venue_that_ended_during_shutdown_still_records_its_exit() {
        let script = scripted_venue("clean-exit", "exit 0");
        let mut venue = launch_scripted_with_poll(&script, Duration::from_secs(3600));
        await_child_end(venue.record().pid);
        venue
            .terminate()
            .expect("an orderly teardown of a completed run");
        assert_eq!(
            venue.completed_polls(),
            0,
            "no poll window may expire, or the reap under test was the poll arm's"
        );
        assert_eq!(
            venue.exited(),
            Some(VenueExit {
                success: true,
                code: Some(0)
            }),
            "a run that completed inside the shutdown window must be recorded as a success"
        );
    }

    /// A venue that dies on its own is recorded as the failure it was, with the
    /// code it chose - which is what tells a crash from a completed run.
    #[test]
    fn a_crashed_venue_records_its_nonzero_code() {
        let script = scripted_venue("crash", "exit 17");
        let venue = launch_scripted(&script);
        assert_eq!(
            await_exit(&venue),
            VenueExit {
                success: false,
                code: Some(17)
            }
        );
    }

    /// A venue ended by a SIGNAL has no exit code, and `code: None` is the only
    /// thing that says so. Pinned separately because it is the one shape where
    /// `success: false` carries no number, and a record that defaulted the code
    /// to zero would be indistinguishable from a clean run on `code` alone.
    #[test]
    fn a_signalled_venue_records_no_exit_code() {
        let script = scripted_venue("signalled", "kill -9 $$");
        let venue = launch_scripted(&script);
        assert_eq!(
            await_exit(&venue),
            VenueExit {
                success: false,
                code: None
            }
        );
    }

    /// `exited()` is `None` for a venue that was still serving when it was torn
    /// down, and the shutdown of a healthy venue reports no failure.
    ///
    /// The wait is a POSITIVE PRECONDITION rather than padding: without it the
    /// `None` would hold because the owner had not yet looked, which is a
    /// different claim from "it looked and the venue was alive". It waits on the
    /// owner's own poll counter rather than on a multiple of `OWNER_POLL`, so
    /// the precondition is OBSERVED - a scheduler that runs the sleep short
    /// cannot leave the assertion vacuously true.
    #[test]
    fn a_venue_killed_while_healthy_records_no_exit() {
        let script = scripted_venue("healthy", "exec sleep 60");
        let venue = launch_scripted(&script);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        // TWO, not one: the counter is published at the TOP of the poll arm, so
        // a count of one proves only that the arm was entered. A second entry
        // proves the first one ran its `try_wait` to completion and found the
        // venue alive.
        while venue.completed_polls() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the owner never completed a poll window"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            venue.exited(),
            None,
            "a venue still serving has not ended on its own terms"
        );
        venue
            .shutdown()
            .expect("tearing down a healthy venue is not a failure");
    }

    /// A teardown failure recorded by the owning thread REACHES THE CALLER, and
    /// reaches it once.
    ///
    /// This is why `shutdown` returns a `Result` at all. It used to report only
    /// a panicked owner, so the "shut it down and report a failure to do so"
    /// check every caller writes against it was vacuous: a failed kill and a
    /// wedged child both returned `Ok(())`, and a venue still holding its port
    /// was announced as an orderly shutdown. A caller that believes that may
    /// launch its replacement into an address the old one still owns.
    ///
    /// The slot is filled DIRECTLY rather than by provoking a real failed kill.
    /// Provoking one means reaping the child out from under the owner, which
    /// leaves `Child::kill` signalling a pid the kernel may already have
    /// recycled - a test that can kill an unrelated process is not worth the
    /// coverage. What stays unpinned by this test is the COUPLING: that the
    /// owning thread writes this slot when its kill or its reap fails. The
    /// tests above cover the other direction, that a healthy teardown leaves it
    /// empty, so a `shutdown` that always reported would fail there.
    ///
    /// It also cannot pin the ORDERING `terminate` calls load-bearing, because
    /// it builds a venue with no owner at all and so never joins one. That is
    /// `the_teardown_detail_is_read_after_the_owner_joins` below.
    #[test]
    fn a_recorded_teardown_failure_is_reported_and_not_repeated() {
        let teardown = Arc::new(Mutex::new(Some(
            "reaping the venue failed: ECHILD".to_owned(),
        )));
        let venue = LaunchedVenue {
            record: parse_ready(&record_json(ReadyRecord::VERSION)).expect("fixture record"),
            exit: Arc::new(Mutex::new(None)),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            teardown: Arc::clone(&teardown),
            polls: Arc::new(AtomicUsize::new(0)),
            shutdown: None,
            owner: None,
        };
        let error = venue
            .shutdown()
            .expect_err("a venue that would not die must be reported, not swallowed");
        let message = error.to_string();
        assert!(matches!(error, LaunchError::Teardown { .. }), "{error:?}");
        assert!(message.contains("ECHILD"), "{message}");
        assert!(message.contains("holding its port"), "{message}");
        // Taken, not copied: the `Drop` that follows an explicit `shutdown` is
        // the ordinary path, and it must not report the same failure twice.
        assert!(
            teardown.lock().expect("teardown slot").is_none(),
            "the detail must be taken out of the slot by the report that used it"
        );

        let already_reported = LaunchedVenue {
            record: parse_ready(&record_json(ReadyRecord::VERSION)).expect("fixture record"),
            exit: Arc::new(Mutex::new(None)),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            teardown,
            polls: Arc::new(AtomicUsize::new(0)),
            shutdown: None,
            owner: None,
        };
        already_reported
            .shutdown()
            .expect("an emptied slot is an orderly teardown");
    }

    /// The teardown slot is read AFTER the owner is joined, which is the whole
    /// reason the read sits where it does.
    ///
    /// The real owner writes that slot on its way out, so a read placed before
    /// the join races the write and reports a CLEAN teardown for a venue that
    /// would not die - the exact failure the `Result` was added to prevent, put
    /// back by an innocuous-looking reordering. The direct-fill test above
    /// cannot see it: it builds a venue with `owner: None`, so there is no join
    /// for the read to be on the wrong side of.
    ///
    /// The stand-in owner is a plain thread rather than a real venue because
    /// what is under test is `terminate`'s ORDER OF OPERATIONS, not the child.
    /// It reproduces the shape that matters: it blocks until the shutdown sender
    /// drops, then does its work, and only at the END records the failure - just
    /// as `own_venue` kills, waits and then writes.
    #[test]
    fn the_teardown_detail_is_read_after_the_owner_joins() {
        let teardown: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let (shutdown_tx, shutdown_rx) = sync_channel::<()>(1);
        let owner = {
            let slot = Arc::clone(&teardown);
            std::thread::Builder::new()
                .name("stand-in-owner".to_owned())
                .spawn(move || {
                    // Disconnected is the signal, as in `own_venue`.
                    let _asked = shutdown_rx.recv();
                    // Long enough that a read placed before the join loses the
                    // race decisively rather than intermittently.
                    std::thread::sleep(Duration::from_millis(100));
                    *slot.lock().expect("teardown slot") =
                        Some("reaping the venue failed: ECHILD".to_owned());
                })
                .expect("spawn the stand-in owner")
        };
        let venue = LaunchedVenue {
            record: parse_ready(&record_json(ReadyRecord::VERSION)).expect("fixture record"),
            exit: Arc::new(Mutex::new(None)),
            stderr: Arc::new(Mutex::new(VecDeque::new())),
            teardown,
            polls: Arc::new(AtomicUsize::new(0)),
            shutdown: Some(shutdown_tx),
            owner: Some(owner),
        };
        let error = venue
            .shutdown()
            .expect_err("a failure written on the owner's way out must be reported");
        assert!(matches!(error, LaunchError::Teardown { .. }), "{error:?}");
        assert!(error.to_string().contains("ECHILD"), "{error}");
    }

    /// The timeout path needs a process that holds stdout open and stays silent,
    /// and no stock binary does that when handed `serve`. It is covered against
    /// the real venue in `mogwai-cli`'s lifecycle gates, where the binary
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
